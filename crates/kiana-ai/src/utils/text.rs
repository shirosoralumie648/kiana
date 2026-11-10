//! `utils/text.ts`——消息内容取文本与 system prompt 渲染。
//!
//! 三个函数（`contentText` / `getSystemMessageText` / `renderSystemMessageUpdate`）都被
//! provider 适配层与 transcript 复用；`ownedSlice` 在 Rust 里只是「复制一段 UTF-16 切片」，
//! 因为 `String` 天然拥有所有权（`spec/ai-core.md` §2.2.4）。
//!
//! TS: `utils/text.ts`
//!
//! # 为什么需要它
//! provider 适配层、transcript、token 估算与摘要代码都要做同一件事：从一条消息的
//! content 里取出纯文本。TS 的 `contentText` 用 `string | Content[]` 联合类型一次覆盖
//! 所有消息类型；Rust 没有重载，于是按内容类型拆成 `content_text_*` 四个入口，共用
//! [`content_text_from_blocks`] 的「过滤 + 分隔符拼接」逻辑。
//!
//! # 调用方
//! - `kiana-session::agent_session`：比较 steering 队列里的用户消息、给会话树节点取文本；
//! - `kiana-compaction` 与 `kiana-resources`：摘要与 bug report 的上下文拼接；
//! - `kiana-agent-core`、`crate::utils::transcript`、`crate::utils::estimate`：system
//!   prompt 渲染与 token 估算；
//! - `crate::api::{anthropic_messages, openai_responses_shared, openai_codex_responses}`：
//!   把 system 消息写进请求体。
//!
//! # 关键约束
//! - 所有 `content_text_*` 只收集 `Text` 块；thinking、toolCall、image 一律忽略。
//! - `sections` 的遍历顺序必须按 **JS 对象枚举序**（数组索引键优先，其余按插入序），
//!   否则渲染文本与 TS 不一致；实现见 `js_object_ordered_keys`。
//! - [`owned_slice`] 的索引是 **UTF-16 码元**，且只对 `end` 边界做代理对保护。

use crate::types::{
    AssistantContentBlock, SystemContent, SystemMessage, UserContent, UserContentBlock,
};

/// 内容块中参与文本提取的两种来源。
///
/// 这是一个借用视图：调用方把不同内容类型的块映射成 `Text(&str)` 或 `Other`，再由
/// [`content_text_from_blocks`] 统一拼接。「哪些变体算文本」因此在类型层面显式可查，
/// 不必为每种内容类型各写一份循环（`utils::estimate` 也复用它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextBlock<'a> {
    /// `TextContent`。
    Text(&'a str),
    /// 其它块（image / thinking / toolCall）：`contentText` 忽略它们。
    Other,
}

/// `contentText(content, separator = "\n")`：只取 `type === "text"` 的块并以 `separator` 连接。
///
/// 字符串内容原样返回（不 trim、不拼接）。
///
/// 上面这句是 TS `contentText` 重载的字符串分支；本函数只接收块序列，字符串分支在
/// [`content_text_system`] / [`content_text_user_content`] 里实现。
///
/// # 语义细节
/// - `separator` 只插在**被保留的块之间**；被忽略的块既不占位，也不会贡献分隔符。
/// - 空字符串的 `Text` 块仍算一个块：`[Text(""), Text("a")]` 的结果以 `separator` 开头
///   （与 TS 的 `join` 一致，因为 `join` 不跳过空元素）。
/// - 不做 trim、不做 Unicode 规范化；结果就是各文本与分隔符按顺序拼接。
///
/// 复杂度与输出长度成线性关系；除结果字符串外不需要额外分配。
///
/// TS: `utils/text.ts#contentText`
pub fn content_text_from_blocks(blocks: &[TextBlock<'_>], separator: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for block in blocks {
        if let TextBlock::Text(text) = block {
            if !first {
                out.push_str(separator);
            }
            out.push_str(text);
            first = false;
        }
    }
    out
}

/// `contentText` 的 assistant 内容版本（`TextContent | ThinkingContent | ToolCall`）。
///
/// 只有 `AssistantContentBlock::Text` 参与拼接；`Thinking` 与 `ToolCall` 被忽略。
/// `separator` 由调用方给出（TS 的缺省 `"\n"` 在 Rust 里必须显式传）。
pub fn content_text_assistant(content: &[AssistantContentBlock], separator: &str) -> String {
    let blocks: Vec<TextBlock<'_>> = content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Text(text) => TextBlock::Text(&text.text),
            _ => TextBlock::Other,
        })
        .collect();
    content_text_from_blocks(&blocks, separator)
}

/// `contentText` 的 user / toolResult 内容版本（`TextContent | ImageContent`）。
///
/// 只有 `UserContentBlock::Text` 参与拼接；`ImageContent` 没有文本字段，因此被忽略。
pub fn content_text_user(content: &[UserContentBlock], separator: &str) -> String {
    let blocks: Vec<TextBlock<'_>> = content
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => TextBlock::Text(&text.text),
            UserContentBlock::Image(_) => TextBlock::Other,
        })
        .collect();
    content_text_from_blocks(&blocks, separator)
}

/// `contentText` 的 `string | TextContent[]` 版本（system 消息）。
///
/// `SystemContent::Text` 分支直接克隆字符串（不 trim）；`Blocks` 分支里该枚举只含
/// `TextContent`，因此每个块都会参与拼接，不需要过滤。
pub fn content_text_system(content: &SystemContent, separator: &str) -> String {
    match content {
        SystemContent::Text(text) => text.clone(),
        SystemContent::Blocks(blocks) => {
            let blocks: Vec<TextBlock<'_>> = blocks
                .iter()
                .map(|block| TextBlock::Text(&block.text))
                .collect();
            content_text_from_blocks(&blocks, separator)
        }
    }
}

/// `contentText` 的 `string | (TextContent | ImageContent)[]` 版本（user 消息 / `UserContent`）。
///
/// 字符串分支克隆；块分支转调 [`content_text_user`]（忽略图片块）。
pub fn content_text_user_content(content: &UserContent, separator: &str) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => content_text_user(blocks, separator),
    }
}

/// JS 普通对象的属性枚举顺序：数组索引键按数值升序在前，其余键按插入序在后。
///
/// `sections` 在 TS 里是 `Record<string, string>`；`Object.values` / `Object.entries` 会先
/// 访问 "0"、"1"、"42" 这类数组索引键（规范十进制、0..=2^32-2）。Rust 的 `IndexMap` 只保留
/// 插入序，因此渲染前要按 JS 顺序重排。
///
/// 非索引键保持传入迭代器的顺序（对 `IndexMap` 就是插入序）；索引键之间按数值升序。
/// 返回的是新分配的 `Vec<String>`（每个键都是独立分配的 `String`），因此调用方可以在
/// 遍历 `sections` 的同时安全使用结果。
pub(crate) fn js_object_ordered_keys<'a, I>(keys: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut index_keys: Vec<(u64, String)> = Vec::new();
    let mut other_keys: Vec<String> = Vec::new();
    for key in keys {
        match js_array_index(key) {
            Some(index) => index_keys.push((index, key.to_string())),
            None => other_keys.push(key.to_string()),
        }
    }
    index_keys.sort_by_key(|(index, _)| *index);
    index_keys
        .into_iter()
        .map(|(_, key)| key)
        .chain(other_keys)
        .collect()
}

/// 是否为 JS 的数组索引键（规范十进制、值在 0..=2^32-2）。
///
/// 拒绝的情况：空串、超过 10 位、除 `"0"` 以外以 `0` 开头、含非 ASCII 数字。通过初筛后
/// 用 `u64` 解析再与 `u32::MAX` 比较，因此 `"4294967295"`（2^32-1）本身不算数组索引键。
fn js_array_index(key: &str) -> Option<u64> {
    if key.is_empty() || key.len() > 10 || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    (value < u64::from(u32::MAX)).then_some(value)
}

/// `getSystemMessageText(message)`：system 消息的完整提示——内容后接各个 section。
///
/// section 的值按 JS 对象枚举序追加，`null` 跳过；空串项被过滤掉，最后以 `"\n\n"` 连接。
///
/// # 渲染规则
/// 1. 先取 `content` 的文本（字符串原样；块数组按 `"\n"` 连接）。
/// 2. 按 JS 对象枚举序追加每个 section 的值；`null`（`Some(None)`）整个跳过。
/// 3. 空串在 join **之前**被过滤，因此不会出现开头的 `"\n\n"` 或连续空段。
///
/// `sections` 的键本身不进入输出，只用于排序与取值；值原样保留（不 trim，也不要求非空）。
///
/// TS: `utils/text.ts#getSystemMessageText`
pub fn get_system_message_text(message: &SystemMessage) -> String {
    let mut parts = vec![content_text_system(&message.content, "\n")];
    if let Some(sections) = &message.sections {
        for name in js_object_ordered_keys(sections.keys().map(String::as_str)) {
            if let Some(Some(text)) = sections.get(&name) {
                parts.push(text.clone());
            }
        }
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `renderSystemMessageUpdate(message)`：把后续 system 消息渲染成对话中的一条文本。
///
/// 正文非空时先放入；每个 section 生成
/// `Removed system prompt section "<name>".`（值为 `null`）或
/// `Updated system prompt section "<name>":\n\n<value>`；以 `"\n\n"` 连接。
///
/// 注意：这是**请求期**渲染，可跨版本变化（源码注释明示）。
///
/// 与 [`get_system_message_text`] 的差别：这里**不**过滤空值，而是把每个 section 都渲染成
/// 一条变更说明；正文为空时直接省略正文段。空字符串的值仍会生成
/// `Updated system prompt section "<name>":\n\n`（value 为空），只有 `null` 才会生成
/// `Removed system prompt section "<name>".`。
///
/// TS: `utils/text.ts#renderSystemMessageUpdate`
pub fn render_system_message_update(message: &SystemMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    let text = content_text_system(&message.content, "\n");
    if !text.is_empty() {
        parts.push(text);
    }
    if let Some(sections) = &message.sections {
        for name in js_object_ordered_keys(sections.keys().map(String::as_str)) {
            parts.push(match sections.get(&name) {
                Some(Some(value)) => {
                    format!("Updated system prompt section \"{name}\":\n\n{value}")
                }
                _ => format!("Removed system prompt section \"{name}\"."),
            });
        }
    }
    parts.join("\n\n")
}

/// `text.slice(start, end)` 的语义等价物（索引按 **UTF-16 码元**，边界不拆代理对）。
///
/// TS 的实现 `\`${text.slice(start, end)} \`.slice(0, -1)` 只为让 V8 复制切片；Rust 的
/// [`String`] 本就拥有数据，这里直接返回新 `String`，语义与 `slice` 一致
/// （`ARCHITECTURE.md` §4.6）。
///
/// # 边界行为
/// - `start` / `end` 按 UTF-16 码元计数，越界会被夹到 `[0, len]`；`end < start` 会被抬到
///   `start`，结果是空串（不会出现负长度）。
/// - `end = None` 表示一直到文本末尾（等价于 `text.length`）。
/// - 只调整 `end`：若 `end` 落在代理对中间，会把后面的低代理项一起纳入，从而不产生孤立
///   高代理项。
/// - `start` 不调整：若调用方把 `start` 指在代理对中间，切片会以孤立低代理项开头，
///   `String::from_utf16_lossy` 会把它替换成 U+FFFD。现有生产调用方都从 0 开始截断。
///
/// 实现先把整个输入编码成 `Vec<u16>` 再解码切片，时间与额外内存都与输入长度成正比。
///
/// TS: `utils/text.ts#ownedSlice`
pub fn owned_slice(text: &str, start: usize, end: Option<usize>) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let len = units.len();
    let start = start.min(len);
    let mut end = end.unwrap_or(len).min(len).max(start);
    // `ARCHITECTURE.md` §4.6 第 1 条：截断不得拆开代理对。TS 的 `slice` 会切出孤立代理项，
    // Rust 侧改为把配对的那个码元一起包含进来。
    let is_high = |unit: u16| (0xd800..=0xdbff).contains(&unit);
    let is_low = |unit: u16| (0xdc00..=0xdfff).contains(&unit);
    if end > start && end < len && is_high(units[end - 1]) && is_low(units[end]) {
        end += 1;
    }
    String::from_utf16_lossy(&units[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TextContent, TextKind};

    fn text_block(text: &str) -> TextContent {
        TextContent {
            kind_text: TextKind::Text,
            text: text.into(),
            text_signature: None,
        }
    }

    #[test]
    fn extracts_assistant_text_blocks() {
        let content = vec![
            AssistantContentBlock::Thinking(crate::types::ThinkingContent {
                kind_thinking: crate::types::ThinkingKind::Thinking,
                thinking: "reasoning".into(),
                thinking_signature: None,
                redacted: None,
            }),
            AssistantContentBlock::Text(text_block("first")),
            AssistantContentBlock::ToolCall(crate::types::ToolCall {
                kind_tool_call: crate::types::ToolCallKind::ToolCall,
                id: "1".into(),
                name: "read".into(),
                arguments: Default::default(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }),
            AssistantContentBlock::Text(text_block("second")),
        ];
        assert_eq!(content_text_assistant(&content, "\n"), "first\nsecond");
        assert_eq!(content_text_assistant(&content, ""), "firstsecond");
    }

    #[test]
    fn extracts_text_from_tool_result_content() {
        let content = vec![
            UserContentBlock::Text(text_block("first")),
            UserContentBlock::Image(crate::types::ImageContent {
                kind_image: crate::types::ImageKind::Image,
                data: "...".into(),
                mime_type: "image/png".into(),
            }),
            UserContentBlock::Text(text_block("second")),
        ];
        assert_eq!(content_text_user(&content, ""), "firstsecond");
    }

    #[test]
    fn passes_string_content_through() {
        assert_eq!(
            content_text_system(&SystemContent::Text("hello".into()), "\n"),
            "hello"
        );
    }

    #[test]
    fn renders_system_message_text_and_updates() {
        let mut sections = indexmap::IndexMap::new();
        sections.insert("a".to_string(), Some("A".to_string()));
        sections.insert("b".to_string(), None);
        let message = SystemMessage {
            role: crate::types::SystemRole::System,
            content: SystemContent::Text("base".into()),
            sections: Some(sections),
            tools_added: None,
            tools_removed: None,
            timestamp: 0,
        };
        assert_eq!(get_system_message_text(&message), "base\n\nA");
        assert_eq!(
            render_system_message_update(&message),
            "base\n\nUpdated system prompt section \"a\":\n\nA\n\nRemoved system prompt section \"b\"."
        );
    }

    #[test]
    fn section_rendering_follows_js_object_key_order() {
        let mut sections = indexmap::IndexMap::new();
        sections.insert("2".to_string(), Some("two".to_string()));
        sections.insert("1".to_string(), Some("one".to_string()));
        sections.insert("b".to_string(), Some("bee".to_string()));
        sections.insert("a".to_string(), Some("ay".to_string()));
        let message = SystemMessage {
            role: crate::types::SystemRole::System,
            content: SystemContent::Text("base".into()),
            sections: Some(sections),
            tools_added: None,
            tools_removed: None,
            timestamp: 0,
        };
        // TS: Object.values -> [1, 2, b, a]。
        assert_eq!(
            get_system_message_text(&message),
            "base\n\none\n\ntwo\n\nbee\n\nay"
        );
        assert_eq!(
            render_system_message_update(&message),
            "base\n\nUpdated system prompt section \"1\":\n\none\n\nUpdated system prompt section \"2\":\n\ntwo\n\nUpdated system prompt section \"b\":\n\nbee\n\nUpdated system prompt section \"a\":\n\nay"
        );
    }

    #[test]
    fn owned_slice_uses_utf16_units() {
        assert_eq!(owned_slice("abcdef", 1, Some(3)), "bc");
        assert_eq!(owned_slice("abc", 1, None), "bc");
        // 代理对不被拆开：😀 占两个码元，切在中间时把配对码元一起纳入（§4.6）。
        assert_eq!(owned_slice("a😀b", 0, Some(2)), "a😀");
        assert_eq!(owned_slice("a😀b", 0, Some(1)), "a");
        assert_eq!(owned_slice("a😀b", 3, None), "b");
    }
}
