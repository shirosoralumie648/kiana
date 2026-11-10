//! core/compaction/utils.ts（FileOperations/序列化）。
//!
//! 文件操作跟踪、`computeFileLists` / `formatFileOperations`，以及把 LLM 消息序列化成
//! 摘要 prompt 文本的 `serializeConversation`。所有 JS 字符串长度语义按 UTF-16 code unit
//! 处理（`ARCHITECTURE.md` §4.6）。
//!
//! # 这个文件解决什么问题
//!
//! 摘要 prompt 是纯文本：模型看到的是“谁说了什么、调用了什么工具、改了哪些文件”的平铺
//! 记录，而不是 JSON 消息。本模块负责两类工作：
//!
//! 1. 把消息渲染成这段文本（[`serialize_conversation`]）；
//! 2. 跨消息与历史摘要累积文件操作，并渲染成 `<read-files>` / `<modified-files>` 区段
//!    （[`extract_file_ops_from_message`] → [`compute_file_lists`] →
//!    [`format_file_operations`]）。
//!
//! # 与谁交互
//!
//! - [`crate::compaction`]：压缩 prompt 的序列化与结果摘要的文件列表；
//! - [`crate::branch_summarization`]：分支摘要的 prompt 与文件列表；
//! - `kiana-coding-agent/src/sdk_exports.rs` 按 TS 原名再导出本模块的
//!   `serialize_conversation`（别名 `serializeConversation`）。
//!
//! # 数据流
//!
//! 1. [`extract_file_ops_from_message`] 从 assistant 的 toolCall（以及 toolResult 上记录的
//!    codemode 嵌套调用）里按工具名 `read`/`write`/`edit` 与参数 `path` 归类；
//! 2. 结果存进 [`FileOperations`] 的三个 `IndexSet`（插入序、去重）；元素是原始 JSON 值
//!    [`FileValue`]，因为会话文件里的 `details.readFiles` 不保证是字符串；
//! 3. [`compute_file_lists`] 计算 `modified = edited ∪ written`、`read - modified`，按 JS
//!    默认 `sort()` 排序；[`format_file_operations`] 负责渲染。
//!
//! # 重要约束
//!
//! - **UTF-16 语义**：JS 的 `string.length` / `sort()` / `slice()` 都按 UTF-16 code unit
//!   工作，而 Rust 的 `str` 是 UTF-8。本模块用 `utf16_len` / `utf16_cmp` /
//!   `utf16_truncate` 逐个复现；唯一的已知偏差是截断不会产生孤立代理项（见
//!   `utf16_truncate` 的说明）。
//! - **JS 真值/ToString 规则**：`details` 数组元素不做类型校验，`null`、数字、对象都在
//!   排序或渲染时按 JS 规则转换；[`FileValue`] 保留原始值以复现这些差异。
//! - **输出必须确定**：集合使用插入序 `IndexSet`，最终列表再排序，相同输入总得到相同
//!   文本；摘要文本会写入会话文件，逐字节稳定是与 TS 输出比对的前提。
//!
//! 逐条语义见 `spec/ca-tools-compaction-export.md` §2.19。

use std::hash::{Hash, Hasher};

use indexmap::{Equivalent, IndexSet};
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{AssistantContentBlock, JsonObject, JsonValue, Message};
use kiana_ai::utils::error_body::safe_json_stringify;
use kiana_ai::utils::text::{content_text_assistant, content_text_user, content_text_user_content};

// ============================================================================
// UTF-16 helpers
// ============================================================================

/// JS `string.length`：UTF-16 code unit 数。
///
/// TS: `utils.ts#truncateForSummary`（`text.length`）
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `String.prototype.sort()` 的默认比较：逐 UTF-16 code unit 比较。
///
/// Rust 的 `String: Ord` 按 UTF-8 字节 / code point 比较；对 BMP 以上的字符两者不同
/// （例如 U+FF5E 与 U+1F600）。TS: `utils.ts#computeFileLists`（`.sort()`）。
fn utf16_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// JS `String.prototype.slice(0, maxUnits)` 的 UTF-16 语义。
///
/// 偏差（`spec/ca-tools-compaction-export.md` §8.2 第 2 条）：切点落在代理对中间时保留
/// 整个字符，而不是像 JS 那样留下孤立高代理。
fn utf16_truncate(text: &str, max_units: usize) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    for ch in text.chars() {
        let char_units = ch.len_utf16();
        if units + char_units > max_units {
            break;
        }
        units += char_units;
        out.push(ch);
    }
    out
}

/// JS 普通对象的属性枚举顺序：数组索引键按数值升序在前，其余键保持插入序。
///
/// `Object.entries(arguments)` 会先访问 `"0"`、`"42"` 这类规范数组索引键（0..=2^32-2，
/// 无前导零），Rust 的 `IndexMap` 只保留插入序，因此序列化前要按 JS 顺序重排。
///
/// TS: `utils.ts#serializeConversation`（`Object.entries(args)`）
fn js_object_ordered_keys<'a, I>(keys: I) -> Vec<String>
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

/// `key` 是否是 JS 的规范数组索引字符串（`"0"`、`"1"`…；无前导零；`< 2^32 - 1`）。
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

/// JS `String(value)`（ToString）。
///
/// 会话文件的 `details.readFiles` / `details.modifiedFiles` 不做元素类型校验：TS 的
/// `Set<string>` 运行期可以装进任意 JSON 值，`sort` / `join` / `JSON.stringify` 再按各自的
/// JS 规则转换（`sort` 用 `String()`；`join` 用 ToString 但把 `null` 渲染成空串；对象 →
/// `"[object Object]"`；数组 → `join(",")`）。[`FileOperations`] 用 [`FileValue`] 保留原始
/// 值，只有 `sort()` / `join()` 走到这里。
///
/// TS: `compaction.ts#extractFileOperations` / `branch-summarization.ts#prepareBranchEntries`
pub(crate) fn js_value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(value) => value.to_string(),
        // 有限数字的 `String(n)` 与 `JSON.stringify(n)` 一致（kiana-json 的 JS 数字格式）。
        JsonValue::Number(_) => safe_json_stringify(value),
        // `Array.prototype.join(",")`：null（及 JSON 不存在的 undefined）渲染为空串。
        JsonValue::Array(items) => items
            .iter()
            .map(|item| match item {
                JsonValue::Null => String::new(),
                other => js_value_to_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        JsonValue::Object(_) => "[object Object]".to_string(),
    }
}

// ============================================================================
// File Operation Tracking
// ============================================================================

/// `FileOperations` 集合中的元素：原始 JSON 值。
///
/// TS 声明 `Set<string>`，但运行期不做元素类型校验：从会话文件读到的
/// `details.readFiles` / `details.modifiedFiles` 元素原样进入 Set，只在 `sort()` / `join()` /
/// `JSON.stringify` 时按 JS 规则转换。为了逐字节复现这些差异（`null` 的排序键是 `"null"`
/// 但 `join` 渲染成空串；数字 `123` 与字符串 `"123"` 是两个不同元素），Rust 侧也保留原始值。
///
/// 表示能力边界：JS 的 `Set` 用引用比较对象/数组，这里用 JSON 结构相等去重；JSON 没有
/// `undefined`（TS 的 Set 可以装 `undefined`），`NaN` 不是合法 JSON。`-0` 可以出现
/// （`serde_json` 会把 `"-0"` 解析为负零，见 `crates/kiana-json/src/lib.rs` 的说明），
/// 但 `PartialEq`/`Hash` 按 JS 的 `SameValueZero` 把它与 `0` 视为同一元素（见下方实现）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct FileValue(JsonValue);

impl FileValue {
    /// 原始 JSON 值。
    pub fn as_json(&self) -> &JsonValue {
        &self.0
    }

    /// 取出原始 JSON 值。
    pub fn into_json(self) -> JsonValue {
        self.0
    }
}

impl From<JsonValue> for FileValue {
    fn from(value: JsonValue) -> Self {
        FileValue(value)
    }
}

impl From<String> for FileValue {
    fn from(value: String) -> Self {
        FileValue(JsonValue::String(value))
    }
}

impl From<&str> for FileValue {
    fn from(value: &str) -> Self {
        FileValue(JsonValue::String(value.to_string()))
    }
}

/// JS 的 `SameValueZero`/`==` 对数字没有整数/浮点之分（JSON.parse 一律得到 f64）：
/// `123` 与 `123.0` 是同一个元素。其余类型按 JSON 结构比较。
fn js_values_equal(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::Number(left), JsonValue::Number(right)) => {
            left.as_f64().unwrap_or(f64::NAN) == right.as_f64().unwrap_or(f64::NAN)
        }
        (left, right) => left == right,
    }
}

impl PartialEq for FileValue {
    fn eq(&self, other: &Self) -> bool {
        js_values_equal(&self.0, &other.0)
    }
}

impl Eq for FileValue {}

/// 字符串按 `str` 的哈希参与查找，使 `IndexSet<FileValue>::contains("path")` 能找到元素。
/// 数字按 `f64`（JS 唯一的数字类型）；其余按 JSON 值的哈希，与 `PartialEq` 保持一致。
impl Hash for FileValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            JsonValue::String(text) => text.hash(state),
            JsonValue::Number(number) => {
                let number = number.as_f64().unwrap_or(f64::NAN);
                // `+0` 与 `-0` 在 JS 里相等，用同一个哈希。
                let number = if number == 0.0 { 0.0 } else { number };
                number.to_bits().hash(state);
            }
            value => value.hash(state),
        }
    }
}

impl PartialEq<str> for FileValue {
    fn eq(&self, other: &str) -> bool {
        matches!(&self.0, JsonValue::String(text) if text == other)
    }
}

impl PartialEq<&str> for FileValue {
    fn eq(&self, other: &&str) -> bool {
        matches!(&self.0, JsonValue::String(text) if text == *other)
    }
}

impl PartialEq<String> for FileValue {
    fn eq(&self, other: &String) -> bool {
        matches!(&self.0, JsonValue::String(text) if text == other)
    }
}

impl PartialEq<JsonValue> for FileValue {
    fn eq(&self, other: &JsonValue) -> bool {
        js_values_equal(&self.0, other)
    }
}

impl Equivalent<FileValue> for str {
    fn equivalent(&self, key: &FileValue) -> bool {
        key == self
    }
}

/// 跟踪到的文件操作集合（插入序，去重）。
///
/// TS: `utils.ts#FileOperations` —— `{ read: Set<string>; written: Set<string>; edited: Set<string> }`
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperations {
    /// 发生过 `read` 的路径。
    pub read: IndexSet<FileValue>,
    /// 发生过 `write` 的路径。
    pub written: IndexSet<FileValue>,
    /// 发生过 `edit` 的路径。
    pub edited: IndexSet<FileValue>,
}

/// `createFileOps()`：三个空集合。
///
/// TS: `utils.ts#createFileOps`
pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// 从 assistant 消息的 toolCall，或 toolResult 上记录的嵌套调用中提取文件操作。
///
/// # 扫描规则
///
/// - `toolResult`：只看 `nestedCalls.calls`（codemode 脚本发起的调用记录在脚本结果上），
///   不扫描普通 `content`；
/// - `assistant`：只看内容里的 `toolCall` 块，`text`/`thinking` 块被忽略；
/// - 其它角色不产生文件操作；
/// - 每个调用只认工具名 `read`/`write`/`edit` 与参数 `path`（必须是字符串且非空），
///   其它工具，以及缺失、非字符串或空串的 `path`，都静默跳过。
///
/// TS: `utils.ts#extractFileOpsFromMessage`
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    match message.role() {
        "toolResult" => {
            // codemode 脚本发起的调用记录在脚本结果上。
            match message {
                AgentMessage::Standard(Message::ToolResult(result)) => {
                    if let Some(nested) = &result.nested_calls {
                        for call in &nested.calls {
                            let name = call
                                .get("name")
                                .and_then(JsonValue::as_str)
                                .unwrap_or_default();
                            add_file_op(name, call.get("arguments"), file_ops);
                        }
                    }
                }
                AgentMessage::Custom(object) => {
                    if let Some(calls) = object
                        .get("nestedCalls")
                        .and_then(|nested| nested.get("calls"))
                        .and_then(JsonValue::as_array)
                    {
                        for call in calls {
                            let name = call
                                .get("name")
                                .and_then(JsonValue::as_str)
                                .unwrap_or_default();
                            add_file_op(name, call.get("arguments"), file_ops);
                        }
                    }
                }
                _ => {}
            }
        }
        "assistant" => match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(call) = block {
                        let arguments = json_object_to_value(&call.arguments);
                        add_file_op(&call.name, Some(&arguments), file_ops);
                    }
                }
            }
            AgentMessage::Custom(object) => {
                if let Some(content) = object.get("content").and_then(JsonValue::as_array) {
                    for block in content {
                        if block.get("type").and_then(JsonValue::as_str) != Some("toolCall") {
                            continue;
                        }
                        let Some(name) = block.get("name").and_then(JsonValue::as_str) else {
                            continue;
                        };
                        if block.get("arguments").is_none() {
                            continue;
                        }
                        add_file_op(name, block.get("arguments"), file_ops);
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
}

/// `addFileOp(toolName, args, fileOps)`：只认 `path` 参数名，路径按原始字符串记录。
fn add_file_op(tool_name: &str, args: Option<&JsonValue>, file_ops: &mut FileOperations) {
    let path = args
        .and_then(|args| args.get("path"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    if path.is_empty() {
        return;
    }
    match tool_name {
        "read" => {
            file_ops.read.insert(FileValue::from(path));
        }
        "write" => {
            file_ops.written.insert(FileValue::from(path));
        }
        "edit" => {
            file_ops.edited.insert(FileValue::from(path));
        }
        _ => {}
    }
}

/// `computeFileLists` 的结果。
///
/// TS: `utils.ts#computeFileLists` 的返回对象 `{ readFiles, modifiedFiles }`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileLists {
    /// 只被读过、未被修改的文件（按 JS 默认 `sort()` 排序）。
    pub read_files: Vec<FileValue>,
    /// 被修改过的文件（edited ∪ written，按 JS 默认 `sort()` 排序）。
    pub modified_files: Vec<FileValue>,
}

/// JS `Array.prototype.sort()` 的默认比较键：`String(element)`。
fn js_sort_key(value: &FileValue) -> String {
    js_value_to_string(value.as_json())
}

/// `computeFileLists(fileOps)`：`modified = edited ∪ written`；只读文件 = read - modified；
/// 两者都按 JS 默认 `sort()`（`String(element)` 后逐 UTF-16 码元比较，稳定排序）排序。
///
/// `modified` 的插入序是 `edited` 后接 `written`（对应 TS 的
/// `new Set([...edited, ...written])`）；排序键相等时保留这个插入序（稳定排序）。
///
/// TS: `utils.ts#computeFileLists`
pub fn compute_file_lists(file_ops: &FileOperations) -> FileLists {
    let mut modified: IndexSet<FileValue> = IndexSet::new();
    for path in &file_ops.edited {
        modified.insert(path.clone());
    }
    for path in &file_ops.written {
        modified.insert(path.clone());
    }

    let mut read_files: Vec<FileValue> = file_ops
        .read
        .iter()
        .filter(|path| !modified.contains(*path))
        .cloned()
        .collect();
    read_files.sort_by(|left, right| utf16_cmp(&js_sort_key(left), &js_sort_key(right)));

    let mut modified_files: Vec<FileValue> = modified.into_iter().collect();
    modified_files.sort_by(|left, right| utf16_cmp(&js_sort_key(left), &js_sort_key(right)));

    FileLists {
        read_files,
        modified_files,
    }
}

/// `Array.prototype.join` 的元素渲染：`null`/`undefined` 为空串，其余同 `String()`。
fn js_join_element(value: &FileValue) -> String {
    match value.as_json() {
        JsonValue::Null => String::new(),
        other => js_value_to_string(other),
    }
}

/// `formatFileOperations(readFiles, modifiedFiles)`：空列表 -> `""`；否则用
/// `<read-files>` / `<modified-files>` 包裹并以 `"\n\n"` 连接，整体带**前导** `"\n\n"`。
///
/// 调用方（[`crate::compaction::compact`] 与
/// [`crate::branch_summarization::generate_branch_summary`]）把返回值直接 `push_str` 到
/// 摘要正文末尾，前导两个换行因此紧跟在正文之后。
///
/// TS: `utils.ts#formatFileOperations`
pub fn format_file_operations(read_files: &[FileValue], modified_files: &[FileValue]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files
                .iter()
                .map(js_join_element)
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files
                .iter()
                .map(js_join_element)
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

// ============================================================================
// Message Serialization
// ============================================================================

/// 序列化摘要中工具结果的最大字符数（UTF-16 code unit）。
///
/// TS: `utils.ts#TOOL_RESULT_MAX_CHARS`
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// `truncateForSummary(text, maxChars)`：超出时截断并追加省略标记。
///
/// TS: `utils.ts#truncateForSummary`
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let length = utf16_len(text);
    if length <= max_chars {
        return text.to_string();
    }
    let truncated_chars = length - max_chars;
    format!(
        "{}\n\n[... {truncated_chars} more characters truncated]",
        utf16_truncate(text, max_chars)
    )
}

/// 把 `Object.entries(arguments)` 渲染成 `k=<JSON.stringify(v)>` 并以 `", "` 连接。
///
/// TS: `utils.ts#serializeConversation`
fn render_tool_arguments(arguments: &JsonObject) -> String {
    let mut pairs: Vec<String> = Vec::new();
    for key in js_object_ordered_keys(arguments.keys().map(String::as_str)) {
        let Some(value) = arguments.get(&key) else {
            continue;
        };
        pairs.push(format!("{key}={}", safe_json_stringify(value)));
    }
    pairs.join(", ")
}

/// 把 LLM 消息序列化为摘要用的文本。
///
/// 这样模型不会把它当成要继续的对话。调用方需要先用 `convertToLlm()` 处理自定义消息。
/// 工具结果会被截断以控制摘要请求的大小；thinking 被省略。
///
/// # 输出格式
///
/// 每条消息产出 0..n 段，段之间用 `"\n\n"` 连接：
/// - `[User]: ...`：内容为空时整条省略；
/// - `[Assistant]: ...`：仅当内容里至少有一个 text 块；多个文本块用 `"\n"` 拼接；
/// - `[Assistant tool calls]: ...`：`name(k=v, ...)` 形式，多个调用用 `"; "` 连接，
///   参数按 JS 对象键序与 `JSON.stringify` 规则渲染；
/// - `[Tool result]: ...`：超过 2000 个 UTF-16 码元时截断并追加省略标记；
/// - `system` 消息不渲染；只有 thinking、既无 text 也无 toolCall 的 assistant 消息不产段。
///
/// TS: `utils.ts#serializeConversation`
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for message in messages {
        match message {
            Message::User(user) => {
                let content = content_text_user_content(&user.content, "");
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut tool_calls: Vec<String> = Vec::new();
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(call) = block {
                        let args = render_tool_arguments(&call.arguments);
                        tool_calls.push(format!("{}({args})", call.name));
                    }
                }

                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, AssistantContentBlock::Text(_)))
                {
                    parts.push(format!(
                        "[Assistant]: {}",
                        content_text_assistant(&assistant.content, "\n")
                    ));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let content = content_text_user(&result.content, "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }

    parts.join("\n\n")
}

// ============================================================================
// Summarization System Prompt
// ============================================================================

/// 摘要请求的 system prompt（逐字）。
///
/// 压缩、turn prefix 与分支摘要三类请求都经 `crate::compaction` 的
/// `build_summarization_context` 把它转成 system 消息。
///
/// TS: `utils.ts#SUMMARIZATION_SYSTEM_PROMPT`（UTF-16 长度 310）
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

// ============================================================================
// JSON object helpers
// ============================================================================

/// 保序 [`JsonObject`] -> `serde_json::Map`。
pub(crate) fn json_object_to_value(object: &JsonObject) -> JsonValue {
    JsonValue::Object(object.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarization_system_prompt_is_byte_exact() {
        assert_eq!(SUMMARIZATION_SYSTEM_PROMPT.len(), 310);
        assert_eq!(utf16_len(SUMMARIZATION_SYSTEM_PROMPT), 310);
        assert!(SUMMARIZATION_SYSTEM_PROMPT.ends_with("ONLY output the structured summary."));
    }

    #[test]
    fn truncates_tool_results_by_utf16_units() {
        let text = "x".repeat(5000);
        let truncated = truncate_for_summary(&text, 2000);
        assert!(truncated.starts_with(&"x".repeat(2000)));
        assert!(truncated.ends_with("[... 3000 more characters truncated]"));
        assert_eq!(truncate_for_summary("short", 2000), "short");
    }

    #[test]
    fn truncation_keeps_whole_surrogate_pairs() {
        // UTF-16 长度 2001：第 2000 个 code unit 会落在代理对中间。JS 的 slice(0, 2000)
        // 留下孤立高代理；Rust 侧丢弃整个字符（文档化的偏差，spec §8.2 第 2 条）。
        let text = format!("{}😀", "x".repeat(1999));
        assert_eq!(text.encode_utf16().count(), 2001);
        let truncated = truncate_for_summary(&text, 2000);
        assert_eq!(
            truncated,
            format!("{}\n\n[... 1 more characters truncated]", "x".repeat(1999))
        );
        assert!(!truncated.contains('😀'));
    }

    #[test]
    fn sorts_paths_in_utf16_code_unit_order() {
        let mut ops = create_file_ops();
        ops.read.insert("b.ts".into());
        ops.read.insert("a.ts".into());
        ops.read.insert("B.ts".into());
        let lists = compute_file_lists(&ops);
        assert_eq!(lists.read_files, vec!["B.ts", "a.ts", "b.ts"]);
    }

    #[test]
    fn formats_file_operations_with_leading_blank_line() {
        let formatted = format_file_operations(&["a.ts".into()], &["b.ts".into()]);
        assert_eq!(
            formatted,
            "\n\n<read-files>\na.ts\n</read-files>\n\n<modified-files>\nb.ts\n</modified-files>"
        );
        assert_eq!(format_file_operations(&[], &[]), "");
    }

    /// TS 参考（`computeFileLists` + `formatFileOperations`，元素为
    /// `[null, 123, "123", true, ["a", null], {x:1}, "a.ts"]`、modified 为
    /// `[null, 123, "123"]`）：
    /// `readFiles=[{x:1},["a",null],"a.ts",true]`，
    /// `modifiedFiles=[123,"123",null]`，
    /// 渲染为 `"\n\n<read-files>\n[object Object]\na,\na.ts\ntrue\n</read-files>\n\n"
    /// <modified-files>\n123\n123\n\n</modified-files>"`。
    #[test]
    fn file_values_keep_raw_json_and_render_like_js() {
        let mut ops = create_file_ops();
        for value in [
            JsonValue::Null,
            serde_json::json!(123),
            serde_json::json!("123"),
            serde_json::json!(true),
            serde_json::json!(["a", null]),
            serde_json::json!({ "x": 1 }),
            serde_json::json!("a.ts"),
        ] {
            ops.read.insert(FileValue::from(value));
        }
        for value in [
            JsonValue::Null,
            serde_json::json!(123),
            serde_json::json!("123"),
        ] {
            ops.edited.insert(FileValue::from(value));
        }

        // 123 与 "123" 在 JS Set 里是两个元素。
        assert_eq!(ops.read.len(), 7);
        assert_eq!(ops.edited.len(), 3);

        // JS 的数字没有整数/浮点之分：123 与 123.0 去重成一个。
        let mut numeric = create_file_ops();
        assert!(numeric.read.insert(FileValue::from(serde_json::json!(123))));
        assert!(
            !numeric
                .read
                .insert(FileValue::from(serde_json::json!(123.0)))
        );
        assert_eq!(numeric.read.len(), 1);

        let lists = compute_file_lists(&ops);
        assert_eq!(
            lists.read_files,
            vec![
                serde_json::json!({ "x": 1 }),
                serde_json::json!(["a", null]),
                serde_json::json!("a.ts"),
                serde_json::json!(true),
            ]
        );
        assert_eq!(
            lists.modified_files,
            vec![
                serde_json::json!(123),
                serde_json::json!("123"),
                JsonValue::Null,
            ]
        );
        assert_eq!(
            format_file_operations(&lists.read_files, &lists.modified_files),
            "\n\n<read-files>\n[object Object]\na,\na.ts\ntrue\n</read-files>\n\n<modified-files>\n123\n123\n\n</modified-files>"
        );
        // `details` 的 JSON 类型保持原始值（TS 的 `JSON.stringify` 同样不转字符串）。
        let details = serde_json::to_value(CompactionDetailsForTest {
            read_files: lists.read_files,
            modified_files: lists.modified_files,
        })
        .expect("serialize details");
        assert_eq!(
            details,
            serde_json::json!({
                "readFiles": [{ "x": 1 }, ["a", null], "a.ts", true],
                "modifiedFiles": [123, "123", null]
            })
        );
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CompactionDetailsForTest {
        read_files: Vec<FileValue>,
        modified_files: Vec<FileValue>,
    }

    #[test]
    fn js_value_to_string_matches_js_tostring() {
        assert_eq!(js_value_to_string(&serde_json::json!("a.ts")), "a.ts");
        assert_eq!(js_value_to_string(&serde_json::json!(123)), "123");
        assert_eq!(js_value_to_string(&serde_json::json!(1.5)), "1.5");
        assert_eq!(js_value_to_string(&serde_json::json!(true)), "true");
        assert_eq!(js_value_to_string(&JsonValue::Null), "null");
        // `Array.prototype.join(",")`：null 元素渲染为空串。
        assert_eq!(
            js_value_to_string(&serde_json::json!([1, null, "x"])),
            "1,,x"
        );
        assert_eq!(
            js_value_to_string(&serde_json::json!({ "a": 1 })),
            "[object Object]"
        );
    }
}
