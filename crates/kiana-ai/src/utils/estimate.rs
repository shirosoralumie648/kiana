//! `utils/estimate.ts`——上下文 token 估算。
//!
//! TS: `packages/ai/src/utils/estimate.ts`。
//!
//! 全部是纯函数：字符数按 **UTF-16 code unit** 计（`ARCHITECTURE.md` §4.6，TS 的 `string.length`
//! 语义），token 数按 `ceil(chars / 3.5)`；图片按 4800 字符估算。
//!
//! 使用方见 `spec/ai-core.md` §2.2 的上下文窗口管理：`estimateContextTokens()` 优先用最近一次
//! 适用 assistant 消息的 `usage`，再叠加其后的消息。
//!
//! # 为什么需要估算
//!
//! 真实 token 数只有 provider 返回的 `usage` 才知道；但在**发请求之前**就要决定还能留多少输出
//! 预算（`api::simple_options::clamp_max_tokens_to_context` 用
//! `contextWindow - estimate.tokens - 4096`），只能按字符数估算。3.5 字符/token 与图片 4800
//! 字符都是 TS 侧固定的启发式常数，**不是分词器**：结果只用于预算夹取与压缩阈值，
//! 不应作为面向用户的精确计量。
//!
//! # 两种数据来源
//!
//! - 存在「适用」的 assistant `usage` 时：直接采用 provider 上报的总量，只对**其后**的消息
//!   做字符估算（`trailing`）。
//! - 没有时（首轮、或上下文刚被压缩/改写）：逐条全量估算。
//!
//! # 调用方
//!
//! - `api::simple_options::clamp_max_tokens_to_context`：用 `tokens` 夹取输出预算。
//! - `kiana-durable` 的压缩流程：用 `estimate_message_tokens` 计算保留区 token 数。
//! - `tests/context_estimate.rs` 逐值复刻 TS 测试 `packages/ai/test/context-estimate.test.ts`
//!   的期望值（含 3.5 字符/token 与 4096 安全余量）。
//!
//! 「适用」的判定见 [`estimate_context_tokens_for_messages`] 与内部
//! `get_last_assistant_usage_info`：timestamp 不早于它之前的任何消息、`stop_reason` 不是
//! aborted/error、且 usage 总量 > 0。压缩摘要插入到旧 assistant 消息之后会让旧 usage 失效，
//! 正是靠 timestamp 比较实现的。
//!
//! # 重要约束
//!
//! - 所有长度都是 **UTF-16 code unit**（`encode_utf16().count()`），与 TS 的 `string.length`
//!   一致：一个 emoji 计 2，不能改用 `chars().count()` 或字节数。
//! - 只有**已归一化**的 [`TranscriptContext`] 才保证 system prompt 与 tools 已并入首条
//!   system 消息。按 [`Context`] 重载的 [`estimate_context_tokens_for_context`] 只读
//!   `messages`，未归一化的 `system_prompt`/`tools` 会被漏计；调用前先走
//!   `utils::transcript::normalize_context`。
//! - [`ContextUsageEstimate`] 的不变量：`tokens == usage_tokens + trailing_tokens`
//!   （无 usage 时 `usage_tokens = 0`，有 usage 时 `trailing_tokens` 是其后消息之和）。
//! - 全部是纯函数，无 I/O、无锁，可并发调用。

use crate::types::{
    AssistantContentBlock, Context, Message, SystemMessage, TranscriptContext, Usage, UserContent,
    UserContentBlock,
};
use crate::utils::text::{TextBlock, get_system_message_text};

/// `ContextUsageEstimate`。
///
/// 不变量：`tokens == usage_tokens + trailing_tokens`。`last_usage_index` 是相对于传入的
/// 消息切片的下标（不是消息在完整会话中的绝对位置），调用方若对切片做过裁剪需要自行换算。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContextUsageEstimate {
    /// 估算的上下文总 token 数。
    pub tokens: u64,
    /// 最近一次适用的 assistant `usage` 报告的 token 数。
    pub usage_tokens: u64,
    /// 该 `usage` 之后的消息估算出的 token 数。
    pub trailing_tokens: u64,
    /// 提供 `usage` 的那条消息在 `messages` 里的下标；没有时为 `None`。
    pub last_usage_index: Option<usize>,
}

/// 每个 token 的字符数（TS: `CHARS_PER_TOKEN = 3.5`）。
///
/// 这是启发式平均值，不是任何模型的真实分词比例；改动会同时影响
/// `clamp_max_tokens_to_context` 的预算与压缩阈值，并改变多个测试的期望值。
pub const CHARS_PER_TOKEN: f64 = 3.5;
/// 图片的等效字符数（TS: `ESTIMATED_IMAGE_CHARS = 4800`）。
///
/// 图片块不读取实际内容（尺寸、格式、编码都不看），统一按 4800 字符折算。
pub const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// `calculateContextTokens(usage)`：`totalTokens` 为 0 时回退到各分项之和。
///
/// 先取 provider/适配器保证的 `total_tokens`（口径见 [`Usage`]：有的直接用上报值，有的由
/// 适配器重算），为 0 时才累加 `input + output + cache_read + cache_write`。`cache_write1h`
/// 是 `cache_write` 的子集、`reasoning` 是 `output` 的子集（见 [`Usage`] 的字段文档），
/// 因此都不能再加，否则重复计数。
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens != 0 {
        return usage.total_tokens;
    }
    usage.input + usage.output + usage.cache_read + usage.cache_write
}

/// TS 的 `string.length`：UTF-16 code unit 数。
///
/// `encode_utf16().count()` 每次都会重新遍历并编码字符串（线性成本）；本模块的调用点都是
/// 一次性估算，没有做缓存。
fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `safeJsonStringify(value)`：`JSON.stringify`，异常时为 `"[unserializable]"`。
///
/// `serde_json::Value` 不能表示 `undefined`，也不会因循环引用抛错，因此只需走
/// [`kiana_json::js_object::stringify`]（与 JS 的键序/数字格式一致）。TS 的
/// `JSON.stringify(value) ?? "undefined"` 与 `catch -> "[unserializable]"` 两条回退分支在
/// Rust 侧不可达。
fn safe_json_stringify(value: &serde_json::Value) -> String {
    kiana_json::js_object::stringify(value)
}

/// `estimateTextAndImageContentChars(content)`。
///
/// 字符串内容直接取长度；块数组里 text 块取文本长度、image 块固定按
/// [`ESTIMATED_IMAGE_CHARS`] 计。块数组**不**序列化成 JSON，因此 `type` 等字段不计入。
fn estimate_text_and_image_content_chars(content: &UserContent) -> usize {
    match content {
        UserContent::Text(text) => js_length(text),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                UserContentBlock::Text(text) => js_length(&text.text),
                UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
    }
}

/// `estimateTextTokens(text)`：`Math.ceil(text.length / 3.5)`。
///
/// 空串得到 0；结果按 UTF-16 长度向上取整，不使用任何模型分词器。
pub fn estimate_text_tokens(text: &str) -> u64 {
    (js_length(text) as f64 / CHARS_PER_TOKEN).ceil() as u64
}

/// `estimateTextAndImageContentTokens(content)`。
///
/// 先把所有块的字符数**求和再取整**（与 `ceil(total / 3.5)` 一致），不是逐块 `ceil` 后相加，
/// 否则每块都会向上取整、总量被放大。
pub fn estimate_text_and_image_content_tokens(content: &UserContent) -> u64 {
    (estimate_text_and_image_content_chars(content) as f64 / CHARS_PER_TOKEN).ceil() as u64
}

/// `estimateToolsTokens(tools)`。
///
/// 空数组与 `null` 为 0；其它值按其 JSON 文本估算。参数是 [`serde_json::Value`] 是因为
/// `tools_added`（`Vec<Tool>`）与 `tools_removed`（`Vec<ToolReference>`）的静态类型不同，
/// 调用点统一转成 `Value`（见 `estimate_system_tools`）。
fn estimate_tools_tokens(tools: &serde_json::Value) -> u64 {
    match tools {
        serde_json::Value::Array(items) if items.is_empty() => 0,
        serde_json::Value::Null => 0,
        other => estimate_text_tokens(&safe_json_stringify(other)),
    }
}

/// `system` 消息的 `toolsAdded` / `toolsRemoved` 序列化成 JSON 后的 token 数。
///
/// 两次 `serde_json::to_value` 失败时 `unwrap_or_default()` 得到 `Value::Null`，该项按 0
/// 计——误差方向是**低估**。正常工具定义是可序列化的，失败只在字段含不可序列化值时发生。
fn estimate_system_tools(message: &SystemMessage) -> u64 {
    let mut total = 0;
    if let Some(tools) = &message.tools_added {
        total += estimate_tools_tokens(&serde_json::to_value(tools).unwrap_or_default());
    }
    if let Some(tools) = &message.tools_removed {
        total += estimate_tools_tokens(&serde_json::to_value(tools).unwrap_or_default());
    }
    total
}

/// `estimateMessageTokens(message)`。
///
/// 按消息角色分派：
///
/// - `system`：`estimateTextTokens(getSystemMessageText(m))` 加上 `toolsAdded`/`toolsRemoved`
///   的 JSON 估算；system prompt 的 sections 已由 `get_system_message_text` 拼接在内。
/// - `user` / `toolResult`：文本 + 图片的字符估算；`toolResult` 把内容块克隆成
///   `UserContent::Blocks` 后复用同一路径（这是每次调用的一次深拷贝）。
/// - `assistant`：text 计 `text.length`、thinking 计 `thinking.length`、toolCall 计
///   `name.length + JSON.stringify(arguments).length`，最后**整体**取一次
///   `ceil(chars / 3.5)`（不是每块各自取整）。
///
/// `assistant.usage` 不参与本函数：usage 的处理在
/// [`estimate_context_tokens_for_messages`] 里，避免同一份用量被重复计入。
pub fn estimate_message_tokens(message: &Message) -> u64 {
    match message {
        Message::System(system) => {
            estimate_text_tokens(&get_system_message_text(system)) + estimate_system_tools(system)
        }
        Message::User(user) => estimate_text_and_image_content_tokens(&user.content),
        Message::ToolResult(result) => {
            estimate_text_and_image_content_tokens(&UserContent::Blocks(result.content.clone()))
        }
        Message::Assistant(assistant) => {
            let mut chars = 0usize;
            // 每个块只累加字符数，最后统一向上取整（与 TS 的 Math.ceil 位置一致）。
            for block in &assistant.content {
                match block {
                    AssistantContentBlock::Text(text) => chars += js_length(&text.text),
                    AssistantContentBlock::Thinking(thinking) => {
                        chars += js_length(&thinking.thinking)
                    }
                    AssistantContentBlock::ToolCall(call) => {
                        chars += js_length(&call.name);
                        chars += js_length(&safe_json_stringify(&serde_json::Value::Object(
                            call.arguments.clone().into_iter().collect(),
                        )));
                    }
                }
            }
            (chars as f64 / CHARS_PER_TOKEN).ceil() as u64
        }
    }
}

/// `getLastAssistantUsageInfo(messages)`。
///
/// 找出「能描述当前前缀」的最后一条 assistant usage，返回 `(usage, index)`。
///
/// # 算法
///
/// 维护 `latest_prefix_timestamp`：到目前为止所有消息 timestamp 的最大值（每轮末尾更新，
/// 因此比较当前消息时它只含**之前**的消息）。对每条 assistant：
///
/// 1. `timestamp >= latest_prefix_timestamp` 才算「没有被更新的前缀消息插到它后面」。
///    压缩摘要插入到旧响应之后时，摘要的 timestamp 更大，旧 usage 因此被排除
///    （测试 `ignores_stale_assistant_usage_after_a_newer_message_is_inserted_before_it`）。
/// 2. `stop_reason` 不能是 [`crate::types::StopReason::Aborted`] 或
///    [`crate::types::StopReason::Error`]：失败调用的 usage 不能代表前缀。
/// 3. [`calculate_context_tokens`] 必须 > 0：空用量的响应等于没测到。
///
/// 满足条件的候选会**覆盖**前一个（不 break），所以最终取最后一条可用 assistant。
/// `>=` 允许相同 timestamp 的消息（并列时取后者）。
fn get_last_assistant_usage_info(messages: &[Message]) -> Option<(&Usage, usize)> {
    let mut latest_prefix_timestamp = f64::NEG_INFINITY;
    let mut usage_info: Option<(&Usage, usize)> = None;
    for (index, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            // 该响应之后插入了更新的「前缀」消息（例如压缩摘要）时，它的 usage 不能描述当前前缀。
            let usage_applies_to_prefix = assistant.timestamp as f64 >= latest_prefix_timestamp;
            if usage_applies_to_prefix
                && assistant.stop_reason != crate::types::StopReason::Aborted
                && assistant.stop_reason != crate::types::StopReason::Error
                && calculate_context_tokens(&assistant.usage) > 0
            {
                usage_info = Some((&assistant.usage, index));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message) as f64);
    }
    usage_info
}

/// 消息的 `timestamp` 字段（所有变体都有）。
///
/// 单位是 Unix 毫秒；仅用于比较消息先后（见 `get_last_assistant_usage_info`），不做时区换算。
fn message_timestamp(message: &Message) -> i64 {
    match message {
        Message::System(message) => message.timestamp,
        Message::User(message) => message.timestamp,
        Message::Assistant(message) => message.timestamp,
        Message::ToolResult(message) => message.timestamp,
    }
}

/// `estimateContextTokens(context)`。
///
/// 只读取归一化上下文的消息列表（system prompt 与 tools 此时已在首条 system 消息里），
/// 不统计顶层 `system_prompt`/`tools` 字段——对 [`TranscriptContext`] 而言它们应为 `None`。
pub fn estimate_context_tokens(context: &TranscriptContext) -> ContextUsageEstimate {
    estimate_context_tokens_for_messages(&context.as_context().messages)
}

/// `estimateContextTokens(messages)` 的按消息切片版本（TS 的联合入参的另一半）。
///
/// # 计算
///
/// - 找到可用的 assistant usage 时：`usage_tokens = calculate_context_tokens(usage)`，
///   `trailing_tokens = 其后所有消息的 estimate_message_tokens 之和`，
///   `tokens = usage_tokens + trailing_tokens`，`last_usage_index = Some(index)`。
///   「其后」严格从 `index + 1` 开始，usage 所在消息本身不再按字符估算。
/// - 找不到时：全量逐条累加，`usage_tokens = 0`、`trailing_tokens = tokens`、
///   `last_usage_index = None`。
///
/// `last_usage_index` 是相对本切片的下标；同一份消息用不同切片调用会得到不同下标。
pub fn estimate_context_tokens_for_messages(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = get_last_assistant_usage_info(messages) {
        // usage 已覆盖该 assistant 消息及其之前的前缀，只需补算其后的增量。
        let usage_tokens = calculate_context_tokens(usage);
        let mut trailing_tokens = 0;
        for message in &messages[index + 1..] {
            trailing_tokens += estimate_message_tokens(message);
        }
        return ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        };
    }
    // 没有可用 usage：全量估算，tokens 同时作为 trailing_tokens 返回。
    let mut tokens = 0;
    for message in messages {
        tokens += estimate_message_tokens(message);
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// `estimateContextTokens(context)`：`Context`（未归一化）版本。
///
/// 只使用 `context.messages`，**忽略**顶层 `system_prompt` 与 `tools`。对未归一化的输入这
/// 会低估上下文（system prompt 与工具定义都没计），因此只应在确认已归一化、或明确接受低估
/// 的内部路径使用；常规路径请用 [`estimate_context_tokens`]。
pub fn estimate_context_tokens_for_context(context: &Context) -> ContextUsageEstimate {
    estimate_context_tokens_for_messages(&context.messages)
}

/// 供其他模块复用的 `TextBlock` 适配（`text.rs` 已有的联合视图）。
///
/// 只累加 [`TextBlock::Text`](crate::utils::text::TextBlock) 的文本长度，`Other`
/// （image/thinking/toolCall）一律忽略；结果同样按 `ceil(chars / 3.5)` 取整。
pub fn estimate_text_blocks(blocks: &[TextBlock<'_>]) -> u64 {
    let mut chars = 0usize;
    for block in blocks {
        if let TextBlock::Text(text) = block {
            chars += js_length(text);
        }
    }
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_utf16_code_units_like_js_string_length() {
        // "😀" 在 JS 里 length 是 2。
        assert_eq!(estimate_text_tokens("😀"), 1);
        assert_eq!(js_length("😀"), 2);
        assert_eq!(js_length("abc"), 3);
        assert_eq!(estimate_text_tokens(&"a".repeat(7)), 2);
    }
}
