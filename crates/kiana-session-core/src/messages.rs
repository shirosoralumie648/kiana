//! coding-agent 自定义消息类型与 LLM 转换。
//!
//! TS: `packages/coding-agent/src/core/messages.ts`。本模块定义 `bashExecution` /
//! `custom` / `branchSummary` / `compactionSummary` 四种自定义消息（TS 通过
//! declaration merging 扩展 `CustomAgentMessages`），以及 `bashExecutionToText`、
//! 三个构造函数与 [`crate::messages::convert_to_llm`]。
//!
//! Rust 的 `AgentMessage` 是 `Standard(kiana_ai::types::Message) | Custom(JsonObject)`
//! 的开放联合；本模块的四个结构体是 `Custom` 变体的**带类型视图**：读取时从
//! `JsonObject` 宽松反序列化（TS 的 `JSON.parse` 不做校验），写出时按 TS 对象字面量的
//! 键序生成 `JsonObject`（数字格式由 `AgentMessage` 的 `JSON.stringify` 语义负责）。
//!
//! # 这个模块解决什么问题
//!
//! 模型只能理解 `kiana_ai::types::Message` 这套封闭的标准消息（system / user /
//! assistant / toolResult），而会话里还保存着 bash 执行记录、扩展注入的内容与压缩/分支
//! 摘要。这些自定义消息必须同时满足两件事：
//!
//! 1. 以 TS 的 JSON 形状落盘并回读（键序、缺省键、数字格式都是会话文件的兼容契约）；
//! 2. 在每次调用模型前被翻译成标准消息，且不能因为字段畸形丢消息或错位。
//!
//! 本模块把这两件事分开：四个结构体负责 (1)，`convert_to_llm` 负责 (2)。
//!
//! # 数据流
//!
//! 1. 构造：`create_branch_summary_message` / `create_compaction_summary_message` /
//!    `create_custom_message` / `BashExecutionMessage::new` 得到带类型结构体；
//! 2. 写会话：`to_agent_message()` 序列化成 `AgentMessage::Custom`，键序 = 结构体字段
//!    声明顺序；带 `skip_serializing_if = "Option::is_none"` 的字段在 `None` 时省略
//!    （`BranchSummaryMessage::from_id` 是例外：`None` 会写成 `null`）；
//! 3. 读会话：`from_agent_message()` 做**严格**回读（role 不匹配或字段类型不符返回
//!    `None`），或由 `convert_to_llm` 直接读原始 JSON 对象做**宽松**投影；
//! 4. 调用模型前：`convert_to_llm(&[AgentMessage]) -> Vec<Message>`，把自定义 role 变成
//!    user 文本，标准 role 原样或宽松投影。
//!
//! 调用方：`kiana-session`（事件落盘、请求上下文）、`kiana-compaction`（压缩/分支摘要
//! prompt）、`kiana-resources`（bug report）等；`convert_to_llm` 的输出会直接进入发给
//! 模型的消息列表，结构体的序列化结果则会进入会话文件。
//!
//! # 重要约束
//!
//! - 结构体字段顺序就是 JSON 键序（`serde_json` 开启了 `preserve_order`），重排字段会
//!   改变会话文件字节，属于兼容性破坏。
//! - 四个 role 判别式的 serde 重命名是当前稳定契约，字符串值不可更改：`AgentMessage::role()`
//!   与 `convert_to_llm` 都按这些字符串分派，改动会导致分派失效。
//! - `convert_to_llm` 的契约是**保持消息条数与顺序**：除 `excludeFromContext` 的
//!   bashExecution 和未知 role 外，任何字段畸形都不得丢消息（`tests/messages.rs` 的
//!   `convert_to_llm_keeps_messages_with_malformed_fields` 与
//!   `convert_to_llm_retains_lenient_standard_role_messages` 锁定）。
//! - 两条读取路径的严格性不同：视图构造函数会拒绝畸形字段，转换函数不会。
//! - `f64::NAN` 经 serde 序列化为 JSON `null`（与 JS `JSON.stringify(NaN)` 一致），
//!   所以无效日期得到的 `timestamp` 在 JSON 里是 `null`。
//! - 本模块不持久化（由 `session_manager` 负责），也不定义标准消息类型（在 `kiana-ai`）。
//!
//! # 阅读顺序
//!
//! 四个前缀/后缀常量 → 四个 role 判别式与结构体 → `bash_execution_to_text` →
//! 构造函数 → `convert_to_llm` 及其宽松投影助手。

use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, JsonObject, JsonValue, Message,
    StopReason, SystemContent, SystemMessage, SystemRole, TextContent, TextKind, ToolResultMessage,
    ToolResultRole, Usage, UsageCost, UserContent, UserContentBlock, UserMessage, UserRole,
};
use serde::{Deserialize, Serialize};

use crate::session_manager::{js_to_string, js_truthy, json_object_from_value};

/// 压缩摘要前缀（结尾带换行）。
///
/// TS: `messages.ts#COMPACTION_SUMMARY_PREFIX`
pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

/// 压缩摘要后缀（开头带换行）。
///
/// TS: `messages.ts#COMPACTION_SUMMARY_SUFFIX`
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

/// 分支摘要前缀（结尾带换行）。
///
/// TS: `messages.ts#BRANCH_SUMMARY_PREFIX`
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";

/// 分支摘要后缀（**没有**前导换行）。
///
/// TS: `messages.ts#BRANCH_SUMMARY_SUFFIX`
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// 按 JS `JSON.stringify` 语义把任意可序列化值转成 [`JsonValue`]。
fn to_json_value<T: Serialize>(value: &T) -> JsonValue {
    // 本模块的所有结构体只含字符串/布尔/数字/JSON 值；`to_value` 不会失败。时间戳字段
    // 可能是 `NaN`（无效日期），serde_json 会把它写成 JSON `null`，同样不报错。
    serde_json::to_value(value).unwrap_or(JsonValue::Null)
}

/// 把结构体转成 `AgentMessage::Custom` 所需的 `JsonObject`。
///
/// 键序 = 结构体字段声明顺序（`serde_json` 的 `preserve_order` 保留序列化顺序），
/// `skip_serializing_if` 的可选字段不写键。
fn to_agent_message<T: Serialize>(value: &T) -> AgentMessage {
    match to_json_value(value) {
        JsonValue::Object(map) => AgentMessage::Custom(Box::new(
            json_object_from_value(JsonValue::Object(map)).unwrap_or_default(),
        )),
        // 结构体序列化必然是对象；此分支不可达。
        _ => AgentMessage::Custom(Box::default()),
    }
}

// ---------------------------------------------------------------------------
// 角色判别式
// ---------------------------------------------------------------------------

/// `BashExecutionMessage["role"]`（字面量 `"bashExecution"`）。
///
/// TS: `messages.ts#BashExecutionMessage`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BashExecutionRole {
    /// `"bashExecution"`。
    #[default]
    #[serde(rename = "bashExecution")]
    BashExecution,
}

/// `CustomMessage["role"]`（字面量 `"custom"`）。
///
/// TS: `messages.ts#CustomMessage`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CustomRole {
    /// `"custom"`。
    #[default]
    #[serde(rename = "custom")]
    Custom,
}

/// `BranchSummaryMessage["role"]`（字面量 `"branchSummary"`）。
///
/// TS: `messages.ts#BranchSummaryMessage`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BranchSummaryRole {
    /// `"branchSummary"`。
    #[default]
    #[serde(rename = "branchSummary")]
    BranchSummary,
}

/// `CompactionSummaryMessage["role"]`（字面量 `"compactionSummary"`）。
///
/// TS: `messages.ts#CompactionSummaryMessage`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompactionSummaryRole {
    /// `"compactionSummary"`。
    #[default]
    #[serde(rename = "compactionSummary")]
    CompactionSummary,
}

// ---------------------------------------------------------------------------
// 四个自定义消息类型
// ---------------------------------------------------------------------------

/// `!` / `!!` 命令的 bash 执行消息。
///
/// 键序：`role, command, output, exitCode, cancelled, truncated, fullOutputPath,
/// timestamp, excludeFromContext`。`exitCode`（`number | undefined`）、`fullOutputPath`
/// 与 `excludeFromContext` 缺省时由 `JSON.stringify` 省略。
///
/// 这是**严格视图**：`from_agent_message` 通过 serde 反序列化，已知字段类型不符
/// （例如 `exitCode` 是字符串）会返回 `None`。需要像 TS 那样容忍畸形字段时使用
/// [`crate::messages::convert_to_llm`]，它直接读取原始 JSON 对象。
///
/// TS: `messages.ts#BashExecutionMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    /// 恒为 `"bashExecution"`。
    pub role: BashExecutionRole,
    /// 原始命令（不含 shell 前缀）。
    #[serde(default)]
    pub command: String,
    /// 捕获的输出。
    #[serde(default)]
    pub output: String,
    /// 退出码；未取得（被取消）时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<f64>,
    /// 是否被取消。
    #[serde(default)]
    pub cancelled: bool,
    /// 输出是否被截断。
    #[serde(default)]
    pub truncated: bool,
    /// 完整输出文件路径（截断时）。
    ///
    /// 只有 `truncated` 为真时 `bash_execution_to_text` 才会附加这一行；路径为空串时
    /// 仍会附加（类型化路径用 `Option` 判空，宽松路径用 JS 真值判断）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// Unix 毫秒。
    #[serde(default)]
    pub timestamp: f64,
    /// `!!` 前缀时为 true（不进入 LLM 上下文）。
    ///
    /// `convert_to_llm` 按 JS 真值判断：只有真值（`true`、非空字符串、非零数字等）才丢弃，
    /// `false`/`0`/空串/`null`/缺省都保留。经本结构体序列化时只可能是 `true`/`false`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

impl Default for BashExecutionMessage {
    fn default() -> Self {
        BashExecutionMessage {
            role: BashExecutionRole::BashExecution,
            command: String::new(),
            output: String::new(),
            exit_code: None,
            cancelled: false,
            truncated: false,
            full_output_path: None,
            timestamp: 0.0,
            exclude_from_context: None,
        }
    }
}

impl BashExecutionMessage {
    /// 构造一条 bash 执行消息：未取消、未截断、没有退出码与完整输出路径。
    ///
    /// `timestamp` 原样写入（Unix 毫秒，不做校验）。
    /// TS: messages.ts#BashExecutionMessage.new
    pub fn new(command: impl Into<String>, output: impl Into<String>, timestamp: f64) -> Self {
        BashExecutionMessage {
            command: command.into(),
            output: output.into(),
            timestamp,
            ..Default::default()
        }
    }

    /// 转成 `AgentMessage::Custom`。
    /// TS: messages.ts#BashExecutionMessage.toAgentMessage
    pub fn to_agent_message(&self) -> AgentMessage {
        to_agent_message(self)
    }

    /// 从任意 `AgentMessage` 读取（role 不匹配或字段不可解析时返回 `None`）。
    ///
    /// `AgentMessage::Standard` 恒返回 `None`；只有 `Custom` 对象的 `role` 恰好是
    /// `"bashExecution"` 且已知字段类型都能反序列化时才成功。
    /// TS: messages.ts#BashExecutionMessage.fromAgentMessage
    pub fn from_agent_message(message: &AgentMessage) -> Option<Self> {
        from_agent_message_for_role(message, "bashExecution")
    }
}

/// 扩展通过 `sendMessage()` 注入的自定义消息。
///
/// 键序：`role, customType, content, display, details, timestamp`。
///
/// `content` 与 `details` 保存任意 JSON，不做校验：`content` 的“字符串还是内容块数组”由
/// `convert_to_llm` 决定如何投影；`details` 只供扩展/UI 使用，不进入 LLM。
///
/// TS: `messages.ts#CustomMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    /// 恒为 `"custom"`。
    pub role: CustomRole,
    /// 扩展标识（重载时按它过滤）。
    #[serde(default)]
    pub custom_type: String,
    /// 字符串或 `TextContent | ImageContent` 数组（宽松保存任意 JSON）。
    ///
    /// 键缺失时 serde 取默认值 `null`，且没有 `skip_serializing_if`，因此重新序列化会
    /// 补出 `"content": null` 键（TS 对畸形对象的“原样往返”会保留缺键）。
    #[serde(default)]
    pub content: JsonValue,
    /// TUI 是否显示。
    #[serde(default)]
    pub display: bool,
    /// 扩展私有元数据（不进入 LLM）。
    ///
    /// 区分“键缺省”（`None`，序列化时省略）与“键存在且为 JSON `null`”
    /// （`Some(JsonValue::Null)`，保留 `null`）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub details: Option<JsonValue>,
    /// Unix 毫秒。
    #[serde(default)]
    pub timestamp: f64,
}

impl CustomMessage {
    /// 转成 `AgentMessage::Custom`。
    /// TS: messages.ts#CustomMessage.toAgentMessage
    pub fn to_agent_message(&self) -> AgentMessage {
        to_agent_message(self)
    }

    /// 从任意 `AgentMessage` 读取（role 不匹配或字段不可解析时返回 `None`）。
    /// TS: messages.ts#CustomMessage.fromAgentMessage
    pub fn from_agent_message(message: &AgentMessage) -> Option<Self> {
        from_agent_message_for_role(message, "custom")
    }
}

/// 分支摘要消息（进入 LLM 上下文）。
///
/// 键序：`role, summary, fromId, timestamp`。`fromId` 为 `string | null`，键始终存在。
///
/// TS: `messages.ts#BranchSummaryMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    /// 恒为 `"branchSummary"`。
    pub role: BranchSummaryRole,
    /// 摘要文本。
    #[serde(default)]
    pub summary: String,
    /// 被放弃分支的 leaf id（或 `null`）。
    #[serde(default)]
    pub from_id: Option<String>,
    /// Unix 毫秒。
    #[serde(default)]
    pub timestamp: f64,
}

impl BranchSummaryMessage {
    /// 转成 `AgentMessage::Custom`。
    /// TS: messages.ts#BranchSummaryMessage.toAgentMessage
    pub fn to_agent_message(&self) -> AgentMessage {
        to_agent_message(self)
    }

    /// 从任意 `AgentMessage` 读取（role 不匹配或字段不可解析时返回 `None`）。
    /// TS: messages.ts#BranchSummaryMessage.fromAgentMessage
    pub fn from_agent_message(message: &AgentMessage) -> Option<Self> {
        from_agent_message_for_role(message, "branchSummary")
    }
}

/// 压缩摘要消息（进入 LLM 上下文）。
///
/// 键序：`role, summary, tokensBefore, timestamp`。
///
/// TS: `messages.ts#CompactionSummaryMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    /// 恒为 `"compactionSummary"`。
    pub role: CompactionSummaryRole,
    /// 摘要文本。
    #[serde(default)]
    pub summary: String,
    /// 压缩前的 token 数。
    #[serde(default)]
    pub tokens_before: f64,
    /// Unix 毫秒。
    #[serde(default)]
    pub timestamp: f64,
}

impl CompactionSummaryMessage {
    /// 转成 `AgentMessage::Custom`。
    /// TS: messages.ts#CompactionSummaryMessage.toAgentMessage
    pub fn to_agent_message(&self) -> AgentMessage {
        to_agent_message(self)
    }

    /// 从任意 `AgentMessage` 读取（role 不匹配或字段不可解析时返回 `None`）。
    /// TS: messages.ts#CompactionSummaryMessage.fromAgentMessage
    pub fn from_agent_message(message: &AgentMessage) -> Option<Self> {
        from_agent_message_for_role(message, "compactionSummary")
    }
}

/// 保留“键存在且为 JSON `null`”与“键缺省”的区别（与 kiana-agent-core 的同名技巧一致）。
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<JsonValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    JsonValue::deserialize(deserializer).map(Some)
}

/// 从 `AgentMessage::Custom` 的整对象反序列化一个带类型视图（role 必须匹配）。
///
/// 使用 serde 的严格反序列化：未知键被忽略；已知字段类型不符、或在没有 `default` 的情况下
/// 缺失，都会让整个反序列化失败并返回 `None`（因此这是“严格视图”，与 `convert_to_llm`
/// 的宽松读取不同）。
fn from_agent_message_for_role<T>(message: &AgentMessage, role: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    match message {
        AgentMessage::Standard(_) => None,
        AgentMessage::Custom(object) => {
            if object.get("role").and_then(JsonValue::as_str) != Some(role) {
                return None;
            }
            serde_json::from_value(JsonValue::Object(to_serde_map(object.as_ref()))).ok()
        }
    }
}

/// `IndexMap<String, Value>` → `serde_json::Map<String, Value>`（两者都保持插入序）。
///
/// 用于把本 crate 的 `JsonObject` 交给 `serde_json::from_value`；`serde_json` 开启了
/// `preserve_order`，转换后键序不变。
fn to_serde_map(object: &JsonObject) -> serde_json::Map<String, JsonValue> {
    let mut map = serde_json::Map::with_capacity(object.len());
    for (key, value) in object {
        map.insert(key.clone(), value.clone());
    }
    map
}

// ---------------------------------------------------------------------------
// bashExecutionToText
// ---------------------------------------------------------------------------

/// 把 bash 执行消息转成 user 消息文本。
///
/// 输入是**带类型**的结构体，字段类型已由 Rust 保证。`convert_to_llm` 处理会话里宽松
/// 解析出的原始对象时走另一条实现（`bash_execution_to_text_from_object`），按 JS 真值和
/// 字符串插值读取畸形字段；对正常字段两者输出同一段文本。
///
/// 输出格式：``Ran `<command>` `` 后接围栏代码块或 `(no output)`，再按需附加取消提示、
/// 非零退出码、截断后的完整输出路径。
///
/// TS: `messages.ts#bashExecutionToText`
pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if !msg.output.is_empty() {
        text.push_str("```\n");
        text.push_str(&msg.output);
        text.push_str("\n```");
    } else {
        text.push_str("(no output)");
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(exit_code) = msg.exit_code {
        // `exitCode !== null && exitCode !== undefined && exitCode !== 0`
        if exit_code != 0.0 {
            text.push_str("\n\nCommand exited with code ");
            text.push_str(&js_number_to_string(exit_code));
        }
    }
    if msg.truncated
        && let Some(path) = &msg.full_output_path
    {
        text.push_str("\n\n[Output truncated. Full output: ");
        text.push_str(path);
        text.push(']');
    }
    text
}

/// JS 的 `String(number)`（模板字符串内插）。
///
/// `bashExecutionToText` 只在这里用到数字格式化（`exitCode` 是整数退出码）；整数走 `i64`，
/// 其余退回 Rust 的最短往返表示。`-0.0` 走 `0` 分支（JS `String(-0)` 也是 `"0"`）。
fn js_number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e21 {
        return format!("{}", value as i64);
    }
    value.to_string()
}

// ---------------------------------------------------------------------------
// 构造函数
// ---------------------------------------------------------------------------

/// `createBranchSummaryMessage(summary, fromId, timestamp)`。
///
/// `timestamp` 是 ISO 字符串，按 `new Date(timestamp).getTime()` 解析（无效为 `NaN`）。
///
/// TS: `messages.ts#createBranchSummaryMessage`
pub fn create_branch_summary_message(
    summary: &str,
    from_id: &str,
    timestamp: &str,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        role: BranchSummaryRole::BranchSummary,
        summary: summary.to_string(),
        from_id: Some(from_id.to_string()),
        timestamp: js_date_time(timestamp),
    }
}

/// `createCompactionSummaryMessage(summary, tokensBefore, timestamp)`。
///
/// TS: `messages.ts#createCompactionSummaryMessage`
pub fn create_compaction_summary_message(
    summary: &str,
    tokens_before: f64,
    timestamp: &str,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        role: CompactionSummaryRole::CompactionSummary,
        summary: summary.to_string(),
        tokens_before,
        timestamp: js_date_time(timestamp),
    }
}

/// `createCustomMessage(customType, content, display, details, timestamp)`。
///
/// `content` 原样保存（不校验是字符串还是内容块数组）；`details` 为 `None` 时序列化省略
/// `details` 键。
///
/// TS: `messages.ts#createCustomMessage`
pub fn create_custom_message(
    custom_type: &str,
    content: JsonValue,
    display: bool,
    details: Option<JsonValue>,
    timestamp: &str,
) -> CustomMessage {
    CustomMessage {
        role: CustomRole::Custom,
        custom_type: custom_type.to_string(),
        content,
        display,
        details,
        timestamp: js_date_time(timestamp),
    }
}

/// `new Date(timestamp).getTime()`：无效日期返回 `NaN`（`JSON.stringify` 写成 `null`）。
///
/// 这是三个 `create*Message` 构造函数解析 ISO 时间戳的唯一入口；`to_agent_message` 之后
/// `NaN` 会由 serde 序列化成 JSON `null`（`tests/messages.rs` 的无效日期用例锁定）。
/// TS: messages.ts#CompactionSummaryMessage.jsDateTime
pub fn js_date_time(timestamp: &str) -> f64 {
    kiana_ai::model_catalog::js_date_parse(timestamp).unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------
// convertToLlm
// ---------------------------------------------------------------------------

/// 把 `AgentMessage`（含自定义类型）转换成 LLM 可用的 `Message`。
///
/// TS: `messages.ts#convertToLlm`。`bashExecution`（`excludeFromContext` 为真时整条丢弃）、
/// `custom`、`branchSummary`、`compactionSummary` 转成 user 消息；四种标准角色原样返回；
/// 其它 role 丢弃。
///
/// TS 直接在 `JSON.parse` 得到的对象上读取字段，不做类型校验；这里对自定义角色同样从原始
/// `JsonObject` 读取（见 `bash_execution_to_text_from_object`），因此字段类型错误（如
/// 字符串 `exitCode`、`output: null`）不会导致整条消息被丢弃。
///
/// 标准角色（`system` / `user` / `assistant` / `toolResult`）的 `AgentMessage::Custom`
/// 视图（字段/形状超出 kiana-ai 类型化联合的消息）同样**保留**：按字段逐项宽松投影（缺省值补
/// 空串/0/空内容块，`content: null` 按 `sessionEntryToContextMessages` 的规则归一化），
/// 与 TS「原样返回」的对象保持相同的消息条数与角色。
///
/// 表示能力边界：kiana-ai 的 `Message` 是带类型的联合，无法承载 `content: null`、未知内容块
/// 或消息对象上的额外键；这些形态会被归一化（空内容块/过滤未知块/丢弃额外键），而不是像
/// TS 那样原样透传。
///
/// # 映射规则
///
/// | 输入 role | 输出 |
/// |---|---|
/// | `bashExecution` | `excludeFromContext` 为真 → 丢弃；否则单条 user 文本（`bashExecutionToText`） |
/// | `custom` | `content` 是字符串 → 单个 text 块；是数组 → 逐个解析已知块；其它 → 空内容 |
/// | `branchSummary` | user 文本：前缀 + `summary` + 后缀 |
/// | `compactionSummary` | user 文本：前缀 + `summary` + 后缀 |
/// | `system` / `user` / `assistant` / `toolResult` | 标准消息克隆；`Custom` 视图逐字段宽松投影 |
/// | 其它 | 丢弃 |
///
/// `summary` 缺失时按 JS 模板插值写成 `undefined`（`tests/messages.rs` 锁定该形态）。
///
/// # 时间戳
///
/// 由自定义 role 生成的消息与四种标准 role 的 `Custom` 视图，`timestamp` 都取原始对象
/// `timestamp` 字段的数值；缺失或非数字时写 0（kiana-ai 的 `Message` 用 `i64`，无法表示
/// TS 的字符串或 `undefined`）。这是与 TS “原样透传消息对象”的一处已知表示差异。
///
/// # 失败模式
///
/// 没有错误返回值：任何输入都会得到 `Vec<Message>`。字段畸形只影响该条消息的内容
/// （空串/空内容块/0），不会丢消息；只有 unknown role 与 `excludeFromContext` 的 bash
/// 消息会被跳过。输出顺序与输入顺序一致。
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    let mut out = Vec::new();
    // 单次线性遍历，保持输入顺序：每个分支要么 push 恰好一条消息，要么 continue。
    for message in messages {
        // role 是唯一派发依据：Standard 消息的 role 来自类型，Custom 消息取对象字段；
        // 缺失或未知的 role 落到最后的 `_` 分支丢弃。
        match message.role() {
            "bashExecution" => {
                // role 字符串只可能来自 Custom（Standard 的 role 固定为四类标准值之一），
                // 这里的 else 是防御性分支。
                let AgentMessage::Custom(object) = message else {
                    continue;
                };
                // TS: `if (m.excludeFromContext) return undefined;`（真值判断）。
                if js_truthy_value(object.get("excludeFromContext")) {
                    continue;
                }
                let text = bash_execution_to_text_from_object(object);
                out.push(Message::User(text_user_message(
                    text,
                    epoch_millis_from_json(object.get("timestamp")),
                )));
            }
            "custom" => {
                let AgentMessage::Custom(object) = message else {
                    continue;
                };
                // TS: `typeof m.content === "string" ? [{type:"text",text}] : m.content`。
                let content =
                    match object.get("content") {
                        Some(JsonValue::String(text)) => kiana_ai::types::UserContent::Blocks(
                            vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                                kind_text: kiana_ai::types::TextKind::Text,
                                text: text.clone(),
                                text_signature: None,
                            })],
                        ),
                        Some(JsonValue::Array(blocks)) => {
                            kiana_ai::types::UserContent::Blocks(lenient_user_blocks(blocks))
                        }
                        // 非字符串/数组（含缺省与 null）：TS 原样透传，Rust 无法表示 → 空内容块。
                        _ => kiana_ai::types::UserContent::Blocks(Vec::new()),
                    };
                out.push(Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content,
                    timestamp: epoch_millis_from_json(object.get("timestamp")),
                }));
            }
            "branchSummary" => {
                let AgentMessage::Custom(object) = message else {
                    continue;
                };
                // 摘要文本直接拼接；`js_to_string` 复刻 JS 模板插值（缺失 → "undefined"，
                // 对象 → "[object Object]"），不做类型校验。
                let text = format!(
                    "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                    js_to_string(object.get("summary"))
                );
                out.push(Message::User(text_user_message(
                    text,
                    epoch_millis_from_json(object.get("timestamp")),
                )));
            }
            "compactionSummary" => {
                let AgentMessage::Custom(object) = message else {
                    continue;
                };
                let text = format!(
                    "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                    js_to_string(object.get("summary"))
                );
                out.push(Message::User(text_user_message(
                    text,
                    epoch_millis_from_json(object.get("timestamp")),
                )));
            }
            // 标准 role 的 Custom 视图：TS 原样返回消息对象；Rust 必须投影到封闭的
            // `Message` 联合，因此逐字段宽松解析（见 `lenient_standard_message`），但消息
            // 本身始终保留，保证条数不因字段畸形而减少。
            role @ ("system" | "user" | "assistant" | "toolResult") => match message {
                AgentMessage::Standard(message) => out.push(message.clone()),
                AgentMessage::Custom(object) => {
                    out.push(lenient_standard_message(object, role));
                }
            },
            // TS 的穷尽检查：未知 role 丢弃。
            _ => {}
        }
    }
    out
}

/// `if (value)`（缺省为 `undefined` → false）。
fn js_truthy_value(value: Option<&JsonValue>) -> bool {
    value.is_some_and(js_truthy)
}

/// 原始 `bashExecution` 对象上的 `bashExecutionToText`（TS 直接读取字段，不做类型校验）。
///
/// 每个字段都按 JS 语义使用：`output` 等按真值判断，模板插值走 `js_to_string`。
///
/// 与类型化版本 `bash_execution_to_text` 的差异：这里所有字段都可能缺失或类型错误，
/// `truncated`/`fullOutputPath` 用真值判断（空串不附加），而类型化版本只判断
/// `Option::is_some`（`Some("")` 会附加空路径）。合法输入下两者输出一致。
fn bash_execution_to_text_from_object(object: &JsonObject) -> String {
    let mut text = format!("Ran `{}`\n", js_to_string(object.get("command")));
    if js_truthy_value(object.get("output")) {
        text.push_str("```\n");
        text.push_str(&js_to_string(object.get("output")));
        text.push_str("\n```");
    } else {
        text.push_str("(no output)");
    }
    if js_truthy_value(object.get("cancelled")) {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(exit_code) = object.get("exitCode") {
        // `exitCode !== null && exitCode !== undefined && exitCode !== 0`
        if !exit_code.is_null() && exit_code.as_f64() != Some(0.0) {
            text.push_str("\n\nCommand exited with code ");
            text.push_str(&js_to_string(Some(exit_code)));
        }
    }
    if js_truthy_value(object.get("truncated")) && js_truthy_value(object.get("fullOutputPath")) {
        text.push_str("\n\n[Output truncated. Full output: ");
        text.push_str(&js_to_string(object.get("fullOutputPath")));
        text.push(']');
    }
    text
}

/// 宽松解析 user 内容块数组：未知/畸形块被跳过（TS 原样透传，Rust 的联合类型无法表示）。
///
/// `UserContentBlock` 只覆盖 text 与 image；其它块类型会在 `from_value` 时失败并被丢弃，
/// 同一数组里的合法块仍然保留。
fn lenient_user_blocks(blocks: &[JsonValue]) -> Vec<UserContentBlock> {
    blocks
        .iter()
        .filter_map(|block| serde_json::from_value::<UserContentBlock>(block.clone()).ok())
        .collect()
}

/// 标准 role 的 `AgentMessage::Custom` 视图 → `Message`。
///
/// TS 原样返回消息对象；Rust 需要投影到封闭的 `Message` 联合。`content: null` 按
/// `sessionEntryToContextMessages` 的规则归一化（system → `""`，其余 → `[]`），无法表示的
/// 字段/内容块被丢弃，但消息本身始终保留（不丢条数）。
///
/// 调用方已经确认 `role` 是四个标准值之一；这里把 toolResult 作为兜底分支。
fn lenient_standard_message(object: &JsonObject, role: &str) -> Message {
    match role {
        "system" => Message::System(lenient_system_message(object)),
        "user" => Message::User(lenient_user_message(object)),
        "assistant" => Message::Assistant(assistant_message_from_object(object)),
        _ => Message::ToolResult(lenient_tool_result_message(object)),
    }
}

/// 宽松 user 消息：`content` 为字符串/已知内容块数组时保留，其余（含 `null`）→ 空数组。
fn lenient_user_message(object: &JsonObject) -> UserMessage {
    UserMessage {
        role: UserRole::User,
        content: match object.get("content") {
            Some(JsonValue::String(text)) => UserContent::Text(text.clone()),
            Some(JsonValue::Array(blocks)) => UserContent::Blocks(lenient_user_blocks(blocks)),
            // TS 原样透传 `null`/其它值；Rust 用空内容块表示（消息保留）。
            _ => UserContent::Blocks(Vec::new()),
        },
        timestamp: epoch_millis_from_json(object.get("timestamp")),
    }
}

/// 宽松 system 消息：保留字符串内容/已知 text 块、`sections`/`toolsAdded`/`toolsRemoved`，
/// 无法解析的可选字段丢弃。
///
/// `content` 既不是字符串也不是数组时（含 `null`）归一化为空文本；块数组里无法解析成
/// `TextContent` 的块被跳过（system 消息不支持图片）。
fn lenient_system_message(object: &JsonObject) -> SystemMessage {
    let content = match object.get("content") {
        Some(JsonValue::String(text)) => SystemContent::Text(text.clone()),
        Some(JsonValue::Array(blocks)) => SystemContent::Blocks(
            blocks
                .iter()
                .filter_map(|block| serde_json::from_value::<TextContent>(block.clone()).ok())
                .collect(),
        ),
        _ => SystemContent::Text(String::new()),
    };
    SystemMessage {
        role: SystemRole::System,
        content,
        sections: object
            .get("sections")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        tools_added: object
            .get("toolsAdded")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        tools_removed: object
            .get("toolsRemoved")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        timestamp: epoch_millis_from_json(object.get("timestamp")),
    }
}

/// 宽松 toolResult 消息：已知内容块保留，`content` 为字符串时视为单个 text 块，
/// `details`/`usage`/`nestedCalls`/`durationMs` 能解析时保留。
///
/// `details` 原样克隆（含显式 `null`）；`usage` 只在 JS 真值时解析（空对象 `{}` 也是真值，
/// 会得到全 0 的 `Usage`）；`isError` 用真值判断。
fn lenient_tool_result_message(object: &JsonObject) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: string_or_empty(object, "toolCallId"),
        tool_name: string_or_empty(object, "toolName"),
        content: match object.get("content") {
            Some(JsonValue::String(text)) => {
                vec![UserContentBlock::Text(text_content(text))]
            }
            Some(JsonValue::Array(blocks)) => lenient_user_blocks(blocks),
            _ => Vec::new(),
        },
        details: object.get("details").cloned(),
        usage: match object.get("usage") {
            Some(value) if js_truthy(value) => Some(lenient_usage(Some(value))),
            _ => None,
        },
        nested_calls: object
            .get("nestedCalls")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        is_error: object.get("isError").is_some_and(js_truthy),
        timestamp: epoch_millis_from_json(object.get("timestamp")),
        duration_ms: object.get("durationMs").and_then(JsonValue::as_f64),
    }
}

/// 宽松 assistant 消息：已知内容块保留，`content` 为字符串时视为单个 text 块；缺失的
/// 必填字段补空串/0/`Pending`，可解析的可选字段保留。
///
/// 也被 `virtual-models.ts#findLatestResponse` 用于宽松解析 assistant 消息。
///
/// Rust 侧另由 `crate::virtual_models::find_latest_response` 复用：它需要把宽松
/// assistant 消息投影成带类型的 `AssistantMessage` 再检查 `stop_reason`。内容块数组里
/// 无法解析的块被跳过，消息本身始终保留。
pub(crate) fn assistant_message_from_object(object: &JsonObject) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: match object.get("content") {
            Some(JsonValue::String(text)) => {
                vec![AssistantContentBlock::Text(text_content(text))]
            }
            Some(JsonValue::Array(blocks)) => blocks
                .iter()
                .filter_map(|block| {
                    serde_json::from_value::<AssistantContentBlock>(block.clone()).ok()
                })
                .collect(),
            _ => Vec::new(),
        },
        api: string_or_empty(object, "api"),
        provider: string_or_empty(object, "provider"),
        model: string_or_empty(object, "model"),
        response_model: optional_string(object, "responseModel"),
        response_id: optional_string(object, "responseId"),
        provider_thinking_level: optional_string(object, "providerThinkingLevel"),
        thinking_level: object
            .get("thinkingLevel")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        diagnostics: object
            .get("diagnostics")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        usage: lenient_usage(object.get("usage")),
        stop_reason: stop_reason_from_json(object.get("stopReason")),
        deferred: object
            .get("deferred")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        error_message: optional_string(object, "errorMessage"),
        raw_stop_reason: optional_string(object, "rawStopReason"),
        end_turn: object.get("endTurn").and_then(JsonValue::as_bool),
        timestamp: epoch_millis_from_json(object.get("timestamp")),
        duration_ms: object.get("durationMs").and_then(JsonValue::as_f64),
    }
}

/// `stopReason` → [`StopReason`]；未知/缺省按 `Pending`（TS 里是 `undefined`，既不是
/// `error` 也不是 `aborted`）。
fn stop_reason_from_json(value: Option<&JsonValue>) -> StopReason {
    match value.and_then(JsonValue::as_str) {
        Some("stop") => StopReason::Stop,
        Some("length") => StopReason::Length,
        Some("toolUse") => StopReason::ToolUse,
        Some("error") => StopReason::Error,
        Some("aborted") => StopReason::Aborted,
        Some("deferred") => StopReason::Deferred,
        _ => StopReason::Pending,
    }
}

/// 宽松 `Usage`：缺失/非数字字段按 0 处理（Rust 的 `Usage` 是 `u64`，无法表示 TS 的 `NaN`）。
///
/// 例外是 `cacheWrite1h` 与 `reasoning` 两个可选 token 字段：键缺失时保留 `None`，键存在
/// 但不可解析时才是 0。
fn lenient_usage(value: Option<&JsonValue>) -> Usage {
    let object = value.and_then(JsonValue::as_object);
    let cost = object
        .and_then(|object| object.get("cost"))
        .and_then(JsonValue::as_object);
    Usage {
        input: usage_token(object, "input"),
        output: usage_token(object, "output"),
        cache_read: usage_token(object, "cacheRead"),
        cache_write: usage_token(object, "cacheWrite"),
        cache_write1h: usage_optional_token(object, "cacheWrite1h"),
        reasoning: usage_optional_token(object, "reasoning"),
        total_tokens: usage_token(object, "totalTokens"),
        cost: UsageCost {
            input: usage_cost(cost, "input"),
            output: usage_cost(cost, "output"),
            cache_read: usage_cost(cost, "cacheRead"),
            cache_write: usage_cost(cost, "cacheWrite"),
            total: usage_cost(cost, "total"),
            total_from_provider: cost
                .and_then(|cost| cost.get("totalFromProvider"))
                .and_then(JsonValue::as_bool),
        },
    }
}

/// 宽松 token 字段：非负有限数字取整，其余（缺省/`null`/字符串…）为 0。
fn usage_token(object: Option<&serde_json::Map<String, JsonValue>>, key: &str) -> u64 {
    usage_optional_token(object, key).unwrap_or(0)
}

/// 宽松可选 token 字段：缺失视为 `None`；存在但不可解析视为 0（保留键）。
fn usage_optional_token(
    object: Option<&serde_json::Map<String, JsonValue>>,
    key: &str,
) -> Option<u64> {
    let value = object?.get(key)?;
    if value.is_null() {
        return Some(0);
    }
    let number = value.as_f64().unwrap_or(0.0);
    Some(if number.is_finite() && number > 0.0 {
        number.trunc() as u64
    } else {
        0
    })
}

/// 宽松 cost 字段：缺失/不可解析为 0（TS 的 `undefined` 会得到 `NaN`，Rust 无法表示）。
fn usage_cost(cost: Option<&serde_json::Map<String, JsonValue>>, key: &str) -> f64 {
    cost.and_then(|cost| cost.get(key))
        .and_then(JsonValue::as_f64)
        .unwrap_or(0.0)
}

/// 字符串字段（缺失 → `""`；非字符串按 JS `String(value)`）。
fn string_or_empty(object: &JsonObject, key: &str) -> String {
    match object.get(key) {
        None => String::new(),
        Some(JsonValue::String(text)) => text.clone(),
        Some(value) => js_to_string(Some(value)),
    }
}

/// 可选字符串字段（缺失/`null` → `None`；非字符串按 JS `String(value)`）。
fn optional_string(object: &JsonObject, key: &str) -> Option<String> {
    match object.get(key) {
        None | Some(JsonValue::Null) => None,
        Some(JsonValue::String(text)) => Some(text.clone()),
        Some(value) => Some(js_to_string(Some(value))),
    }
}

/// 单个 text 内容块。
fn text_content(text: &str) -> TextContent {
    TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    }
}

/// JS 消息时间戳（毫秒）：非数字/缺省按 `0` 处理。
///
/// 只接受 JSON 数字（`as_f64`）；字符串形式的时间戳会被当成 0（TS 会把原值原样放进消息
/// 对象）。这是封闭的 `Message` 联合带来的表示差异。
fn epoch_millis_from_json(value: Option<&JsonValue>) -> i64 {
    value
        .and_then(JsonValue::as_f64)
        .map(epoch_millis_from_f64)
        .unwrap_or(0)
}

/// 构造带单个 text 块的 user 消息。
fn text_user_message(text: String, timestamp: i64) -> kiana_ai::types::UserMessage {
    kiana_ai::types::UserMessage {
        role: kiana_ai::types::UserRole::User,
        content: kiana_ai::types::UserContent::Blocks(vec![UserContentBlock::Text(
            kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text,
                text_signature: None,
            },
        )]),
        timestamp,
    }
}

/// JS 数字 → `i64` 毫秒（非有限/超界时取 0）。
fn epoch_millis_from_f64(value: f64) -> i64 {
    if value.is_finite() && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        value.trunc() as i64
    } else {
        0
    }
}

/// 把 `kiana_ai::types::ToolResultMessage` 的 `content` 作为 JSON 数组
/// （测试与扩展代码的便捷助手）。
///
/// 序列化走与 `AgentMessage` 相同的 serde 形状：每个块带 `type` 判别式（如
/// `{"type":"text","text":"x"}`），`textSignature` 为 `None` 时不写键。
/// TS: messages.ts#CompactionSummaryMessage.toolResultContentJson
pub fn tool_result_content_json(content: &[UserContentBlock]) -> JsonValue {
    to_json_value(&content)
}
