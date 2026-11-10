//! 会话数据层：JSONL 存储、树结构、分支、投影与发现。
//!
//! TS: `packages/coding-agent/src/core/session-manager.ts`（2,018 行）。
//!
//! 会话文件是 append-only 树：每个条目有 `id` / `parentId` 形成树，`leaf` 指针标记当前
//! 位置。写入必须与 `JSON.stringify(entry)` **逐字节**一致（字段顺序 == TS 对象字面量顺序，
//! 数字按 JS `Number::toString`，`undefined` 键省略）。
//!
//! - 通过 append API 新建的条目：typed 视图（固定字段顺序的 serde 结构体）经
//!   [`entry_json_string`]（`serde_json::to_value` + `kiana_json` 的 JS 语义 `stringify`）写出。
//! - 从文件加载的条目：保留 `JSON.parse` 得到的原始对象（`StoredFileEntry::raw`），
//!   `_rewriteFile` / fork / 分支 / 导出都逐字回放——未知字段、未知条目类型与键序不会丢失。
//!
//! 解析侧刻意宽松：`JSON.parse` 不校验字段，未知条目类型保留原始对象
//! （[`SessionEntry::Unknown`]），读取时 `content: null` 在投影阶段归一化。
//!
//! # 设计原因：为什么同时保留 typed 视图与原始对象
//! 会话文件是跨语言（TS 版 Kiana ↔ 本 Rust 实现）的持久化格式，读写都必须逐字节兼容：
//! - 读：`JSON.parse` 不校验字段。本文件用 `kiana_json::js_parse` 复刻 V8 的接受范围，
//!   并用一组 `deserialize_lenient_*` 把类型错误降级为字段级默认值；只有条目根本无法建成
//!   typed 视图（如 `message` 不是对象）时才退化为
//!   [`crate::session_manager::SessionEntry::Unknown`]。
//! - 写：从文件加载的条目把原始对象保存在 `StoredFileEntry::raw`，任何“读入再写出”的路径
//!   （迁移、`fork_from`、`create_branched_session`、导出）都优先逐字回放它；没有 `raw`
//!   的条目（append API 新建、`new_session` 生成的 header）才走 serde，再经
//!   `kiana_agent_core::wire_keys` 还原 TS 对象字面量的键序。
//! 若丢掉 `raw`，未知字段与键序会在第一次重写时消失，破坏与 TS 的字节级契约。
//!
//! # 一次典型的读路径
//! 1. `SessionManager::open` / `fork_from` 调 `load_stored_entries_from_file`：流式读取、
//!    增量 UTF-8 解码、按 `\n` 切行、逐行解析；
//! 2. 首个成功解析且为真值的条目必须是合法 header（`type == "session"` 且字段能反序列化），
//!    否则该文件不是会话文件（`set_session_file` 对非空文件返回 `InvalidSessionFile`）；
//! 3. `load_entries` 按 header 的 `version` 依次跑 v1→v2、v2→v3 迁移；发生迁移时立即
//!    `rewrite_file` 覆盖原文件；
//! 4. `build_index` 重建 `by_id`、标签表与 leaf 指针，之后的读取全部走内存。
//!
//! # 一次典型的写路径
//! 1. `new_session` 只生成 id / 时间戳 / 文件名并重置内存状态，**不创建文件**；
//! 2. `append_*` → `append_entry`：把条目 push 进 `file_entries`、更新 `by_id` 与 leaf，
//!    再调 `persist_last_entry`；
//! 3. 首次落盘要求已有 user/assistant 消息（`has_conversation`），用 `create_new` 一次写出
//!    全部条目；之后每次只以追加模式写最后一条。这样只有 setup 条目（模型、thinking
//!    level）的会话不会留下文件，而首个 user 消息即使没有得到回复也会被保存。
//!
//! # 边界与注意事项
//! - `in_memory` 实例的 `session_file` 为 `None`，所有写盘函数都是 no-op；`create` 在拿到
//!   第一个 user/assistant 消息前同样不产生文件。
//! - `by_id` 只收录字符串 id；非字符串 id 仍留在 `file_entries` 并影响 `get_entry_count()`
//!   与 leaf 指针（见 `LeafPointer` 的几种异常状态）。
//! - 字节敏感消费方（HTML 导出等）必须用原始 JSON 视图
//!   [`crate::session_manager::SessionManager::get_entries_json`] /
//!   `get_header_json` / `get_branch_json` / `get_leaf_id_json`；typed 视图会丢弃未建模字段。
//! - 路径参数经 `kiana_core::paths` 的 `normalize_path` 或 `resolve_path` 归一化；需要以
//!   cwd 为基准绝对化时使用 `resolve_path`（本文件封装为 `resolve_path_default`）。
//! - 带显式 `map_err` 的 fs 调用点（追加/排他创建文件的 `open`、目录创建的 `mkdir`、
//!   `File::create` 重写等）用本文件的 `node_fs_error` 重建为 Node 风格
//!   （如 `ENOENT: no such file or directory, open '...'`）；没有显式映射、经 `?` 传播的
//!   读取/写入/stat 错误则原样包装为 `SessionManagerError::Io`，保留 Rust 原始
//!   `io::Error` 文案。CLI 会把错误的 `Display` 原样打印给用户。
//!
//! # 阅读顺序
//! 常量与错误 → 条目结构体（`SessionEntry` / `FileEntry`）→ 解析与迁移 →
//! `SessionManager` 的构造/追加/投影/树遍历 → 会话发现（`list*`）。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use indexmap::IndexMap;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{JsonObject, JsonValue, Message, SystemMessage, Usage};
use kiana_ai::utils::abort::AbortScope;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::messages::{
    convert_to_llm, create_branch_summary_message, create_compaction_summary_message,
    create_custom_message, js_date_time,
};

/// 当前会话文件版本。
///
/// 版本史：v1 没有 id/parentId 树结构；v2 补齐 id/parentId 并把 `firstKeptEntryIndex`
/// 换成 id；v3 把 `hookMessage` role 改名为 `custom`。`header.version >= 3` 的文件不再
/// 迁移（见 `migrate_stored_to_current_version`）。
///
/// TS: `session-manager.ts#CURRENT_SESSION_VERSION`
pub const CURRENT_SESSION_VERSION: f64 = 3.0;

/// `SessionEntryBase.type` 的取值集合（TS 是 string；Rust 侧保留开放字符串语义）。
///
/// 这里只是已知类型的清单，不代表校验：解析与比较都以原始字符串为准，未知值仍可往返。
/// TS: session-manager.ts#SESSIONENTRYTYPES
pub const SESSION_ENTRY_TYPES: &[&str] = &[
    "message",
    "thinking_level_change",
    "model_change",
    "usage",
    "compaction",
    "branch_summary",
    "custom",
    "custom_message",
    "context_edit",
    "label",
    "session_info",
];

const SESSION_READ_BUFFER_SIZE: usize = 1024 * 1024;
const SESSION_HEADER_READ_BUFFER_SIZE: usize = 4096;
/// 同步头部扫描的上限（允许 cwd 与自定义元数据很大）。
///
/// TS: `session-manager.ts#MAX_SESSION_HEADER_SCAN_BYTES`
pub const MAX_SESSION_HEADER_SCAN_BYTES: usize = 1024 * 1024;

const MAX_CONCURRENT_SESSION_INFO_LOADS: usize = 10;
const MAX_CONCURRENT_SESSION_DISCOVERY_LOADS: usize = 64;
const CURRENT_SESSION_LIST_PUBLISH_INTERVAL: u64 = 10;
const ALL_SESSION_LIST_PUBLISH_INTERVAL: u64 = 100;

/// 会话数据层错误。`Display` 文本是当前稳定契约（改动需同步测试）。
///
/// 变体与 TS 抛出的 `Error` 文案一一对应（由 `Display` 输出）。注意 `Io` 同时承载真实的
/// fs 错误和“TS 会抛 TypeError、但 Rust 侧没有错误通道”时的替代错误（例如会话列表读到
/// 非字符串 `name` 时构造的 `io::Error::other(...)`），因此调用方不能只按 `ErrorKind`
/// 判断错误的真实来源。
#[derive(Debug)]
/// TS: session-manager.ts#SessionManagerError
pub enum SessionManagerError {
    /// 文件系统错误（TS 直接把 fs 的异常抛给调用方）。
    Io(std::io::Error),
    /// `Entry ${id} not found`
    EntryNotFound { id: String },
    /// `Entry ${id} is not on the active branch`
    NotOnActiveBranch { id: String },
    /// `Entry ${id} does not contribute editable model content`
    NotEditable { id: String },
    /// `Context edit replacement must be null or contain string/array content`
    InvalidContextEditReplacement,
    /// `Session id must be non-empty, ...`
    InvalidSessionId,
    /// `Session file is not a valid ${APP_NAME} session: ${path}`
    InvalidSessionFile { path: String },
    /// `Cannot fork: source session file is empty or invalid: ${path}`
    ForkEmptyOrInvalid { path: String },
    /// `Cannot fork: source session has no header: ${path}`
    ForkNoHeader { path: String },
    /// `Session header exceeds ${MAX_SESSION_HEADER_SCAN_BYTES}-byte scan limit: ${path}`
    HeaderScanLimit { path: String },
}

impl std::fmt::Display for SessionManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionManagerError::Io(error) => write!(f, "{error}"),
            SessionManagerError::EntryNotFound { id } => write!(f, "Entry {id} not found"),
            SessionManagerError::NotOnActiveBranch { id } => {
                write!(f, "Entry {id} is not on the active branch")
            }
            SessionManagerError::NotEditable { id } => {
                write!(f, "Entry {id} does not contribute editable model content")
            }
            SessionManagerError::InvalidContextEditReplacement => write!(
                f,
                "Context edit replacement must be null or contain string/array content"
            ),
            SessionManagerError::InvalidSessionId => write!(
                f,
                "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character"
            ),
            SessionManagerError::InvalidSessionFile { path } => write!(
                f,
                "Session file is not a valid {} session: {path}",
                kiana_core::config::APP_NAME
            ),
            SessionManagerError::ForkEmptyOrInvalid { path } => {
                write!(
                    f,
                    "Cannot fork: source session file is empty or invalid: {path}"
                )
            }
            SessionManagerError::ForkNoHeader { path } => {
                write!(f, "Cannot fork: source session has no header: {path}")
            }
            SessionManagerError::HeaderScanLimit { path } => write!(
                f,
                "Session header exceeds {MAX_SESSION_HEADER_SCAN_BYTES}-byte scan limit: {path}"
            ),
        }
    }
}

impl std::error::Error for SessionManagerError {}

impl From<std::io::Error> for SessionManagerError {
    fn from(error: std::io::Error) -> Self {
        SessionManagerError::Io(error)
    }
}

/// `io::Error` → Node/libuv 的 `(CODE, description)`。只覆盖 Node fs 异常在 CLI 里会出现的
/// errno；不认识的错误退回 Rust 的错误种类。
fn node_error_parts(error: &std::io::Error) -> (&'static str, &'static str) {
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error() {
        let known = match raw {
            1 => Some(("EPERM", "operation not permitted")),
            2 => Some(("ENOENT", "no such file or directory")),
            13 => Some(("EACCES", "permission denied")),
            17 => Some(("EEXIST", "file already exists")),
            20 => Some(("ENOTDIR", "not a directory")),
            21 => Some(("EISDIR", "illegal operation on a directory")),
            28 => Some(("ENOSPC", "no space left on device")),
            30 => Some(("EROFS", "read-only file system")),
            36 => Some(("ENAMETOOLONG", "name too long")),
            40 => Some(("ELOOP", "too many symbolic links encountered")),
            _ => None,
        };
        if let Some(parts) = known {
            return parts;
        }
    }
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        ErrorKind::StorageFull => ("ENOSPC", "no space left on device"),
        ErrorKind::ReadOnlyFilesystem => ("EROFS", "read-only file system"),
        _ => ("UNKNOWN", "unknown error"),
    }
}

/// 把 fs 异常转成 Node 的 `error.message`
/// （`EACCES: permission denied, mkdir '<dir>'` / `ENOENT: no such file or directory, open '<file>'`）。
///
/// TS 的 `SessionManager` 直接把 fs 异常抛给 CLI，CLI 打印 `Error: <message>`；Rust 的
/// `io::Error` Display 只有 `Permission denied (os error 13)`，因此在这里补齐
/// `<code>: <description>, <syscall> '<path>'`。
fn node_fs_error(error: std::io::Error, syscall: &str, path: &str) -> SessionManagerError {
    let (code, description) = node_error_parts(&error);
    SessionManagerError::Io(std::io::Error::new(
        error.kind(),
        format!("{code}: {description}, {syscall} '{path}'"),
    ))
}

impl SessionManagerError {
    /// 是否是头部扫描上限错误（`SessionManager::open` 据此回退到全量加载）。
    /// TS: session-manager.ts#isHeaderScanLimit
    pub fn is_header_scan_limit(&self) -> bool {
        matches!(self, SessionManagerError::HeaderScanLimit { .. })
    }
}

/// 会话列表调用的测试探针（**test hook, not public API**）。
///
/// `session-id-readonly.test.ts` 用 `vi.spyOn(SessionManager, "list")` 断言“按精确 ID
/// 查找”不会构建完整会话列表；Rust 的静态方法没有注入点，`kiana-coding-agent` 的回归测试
/// 改为在调用前后比较计数，确保 `list`/`list_all` 没有被调用。
#[doc(hidden)]
pub mod listing_call_probe {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// `SessionManager::list` 的调用次数。
    pub static LIST: AtomicUsize = AtomicUsize::new(0);
    /// `SessionManager::list_all` / `list_all_in` 的调用次数。
    pub static LIST_ALL: AtomicUsize = AtomicUsize::new(0);

    /// 当前计数（`list`, `list_all`）。
    pub fn counts() -> (usize, usize) {
        (LIST.load(Ordering::SeqCst), LIST_ALL.load(Ordering::SeqCst))
    }
}

/// 会话列表取消错误（TS 的 `AbortError`）。
///
/// 由本文件的 `throw_if_aborted` 在检测到 signal 中止时构造：通常
/// `name == "AbortError"`、`message == "This operation was aborted"`；若 abort reason
/// 自带 name/message 则原样采用。`Display` 只输出 `message`（与 CLI 打印
/// `error.message` 一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
/// TS: session-manager.ts#SessionListError
pub struct SessionListError {
    /// `error.name`（通常为 `"AbortError"`；abort reason 自带 name 时沿用该值）。
    pub name: String,
    /// `error.message`。
    pub message: String,
}

impl std::fmt::Display for SessionListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionListError {}

/// `signal.throwIfAborted()`。
fn throw_if_aborted(signal: Option<&AbortScope>) -> Result<(), SessionListError> {
    match signal {
        Some(signal) if signal.is_aborted() => {
            let reason = signal.reason();
            Err(SessionListError {
                name: reason
                    .as_ref()
                    .map(|reason| reason.name.clone())
                    .unwrap_or_else(|| "AbortError".to_string()),
                message: reason
                    .map(|reason| reason.message)
                    .unwrap_or_else(|| "This operation was aborted".to_string()),
            })
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// 通用 JS 助手
// ---------------------------------------------------------------------------

/// ECMAScript `String.prototype.trim()`（`WhiteSpace` + `LineTerminator`）。
pub(crate) fn js_trim(text: &str) -> &str {
    fn is_js_whitespace(c: char) -> bool {
        matches!(
            c,
            '\u{0009}'..='\u{000D}'
                | '\u{0020}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
        )
    }
    text.trim_matches(is_js_whitespace)
}

/// `Date.now()`。
fn now_epoch_millis() -> i64 {
    kiana_ai::model_catalog::now_epoch_millis()
}

/// JS 真值语义（`if (value)`）：`null`/`false`/`0`/`""` 为假，其余（含空数组/空对象）为真。
pub(crate) fn js_truthy(value: &JsonValue) -> bool {
    match value {
        JsonValue::Null => false,
        JsonValue::Bool(value) => *value,
        JsonValue::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        JsonValue::String(text) => !text.is_empty(),
        JsonValue::Array(_) | JsonValue::Object(_) => true,
    }
}

/// JS `Map` 键的 SameValueZero 比较（用于 `id` 不是字符串时的原始键）。
///
/// - 数字按 f64 比较（`JSON.parse` 与 JS 一样把数字读成 f64；`-0 === +0`）；
/// - `null` / 布尔 / 字符串按值比较；
/// - 对象与数组按引用比较：每次 `JSON.parse` 都产生新引用，因此内容相同也不相等。
fn js_map_key_eq(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(left), JsonValue::Bool(right)) => left == right,
        (JsonValue::Number(left), JsonValue::Number(right)) => {
            match (left.as_f64(), right.as_f64()) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            }
        }
        (JsonValue::String(left), JsonValue::String(right)) => left == right,
        _ => false,
    }
}

/// JS 模板字符串 / `String(value)` 语义；`undefined` 用 `None` 表示。
pub(crate) fn js_to_string(value: Option<&JsonValue>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(JsonValue::Null) => "null".to_string(),
        Some(JsonValue::Bool(value)) => value.to_string(),
        Some(JsonValue::Number(number)) => {
            kiana_ai::utils::error_body::safe_json_stringify(&JsonValue::Number(number.clone()))
        }
        Some(JsonValue::String(text)) => text.clone(),
        Some(JsonValue::Array(items)) => items
            .iter()
            .map(|item| match item {
                // `Array.prototype.toString`：null/undefined 元素写成空串。
                JsonValue::Null => String::new(),
                other => js_to_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(JsonValue::Object(_)) => "[object Object]".to_string(),
    }
}

/// `new Date().toISOString()`。
fn now_iso_string() -> String {
    kiana_ai::model_catalog::js_date_to_iso_string(now_epoch_millis())
}

/// V8 `kMaxTimeInMs`（±8.64e15）；`TimeClip` 之外的日期为 Invalid Date。
const MAX_TIME_IN_MS: f64 = 864_000_000.0 * 10_000_000.0;

/// JS `TimeClip(value)`：非有限或超出 ±8.64e15 → `NaN`，否则截断为整数毫秒。
fn time_clip(value: f64) -> f64 {
    if !value.is_finite() || value.abs() > MAX_TIME_IN_MS {
        f64::NAN
    } else {
        value.trunc()
    }
}

/// `new Date(value).getTime()`：按 `Date` 构造器的强制转换规则处理任意 JSON 值。
///
/// - 缺省（`undefined`）→ `NaN`；`null` → 0；
/// - 布尔 → 0/1；数字 → 毫秒（TimeClip）；
/// - 字符串 → `Date.parse`；
/// - 数组 → `Array.prototype.toString` 后再 `Date.parse`；对象 → `"[object Object]"`（`NaN`）。
pub(crate) fn js_date_constructor_time(value: Option<&JsonValue>) -> f64 {
    match value {
        None => f64::NAN,
        Some(JsonValue::Null) => 0.0,
        Some(JsonValue::Bool(value)) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Some(JsonValue::Number(number)) => time_clip(number.as_f64().unwrap_or(f64::NAN)),
        Some(JsonValue::String(text)) => kiana_ai::model_catalog::js_date_parse(text)
            .map(time_clip)
            .unwrap_or(f64::NAN),
        Some(JsonValue::Array(items)) => {
            let text = items
                .iter()
                .map(|item| match item {
                    // `Array.prototype.join`：null/undefined 元素写成空串。
                    JsonValue::Null => String::new(),
                    other => js_to_string(Some(other)),
                })
                .collect::<Vec<_>>()
                .join(",");
            kiana_ai::model_catalog::js_date_parse(&text)
                .map(time_clip)
                .unwrap_or(f64::NAN)
        }
        Some(JsonValue::Object(_)) => f64::NAN,
    }
}

/// 当前工作目录（`process.cwd()`）。
fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `process.cwd()` 替换点（测试注入用；保持与 Node 一致）。
fn resolve_path_default(input: &str) -> String {
    kiana_core::paths::resolve_path(input, &current_dir_string(), &Default::default())
}

/// `path.join(...)` 的等价物（`node:path` 的平台语义）。
///
/// 注意不能用 `PathBuf::push`：它保留 `.` 段、且遇到绝对路径段会重置结果，而 Node 的
/// `join` 做词法归一（`join("./rel", "x")` → `"rel/x"`，`join("/a", "/b")` → `"/a/b"`）。
fn path_join(parts: &[&str]) -> String {
    kiana_core::paths::join_path(parts)
}

/// `path.basename(...)`。
fn path_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `dirname(path)`（`path.resolve(path, "..")`）。
fn path_dirname(path: &str) -> String {
    kiana_core::paths::resolve_path("..", path, &Default::default())
}

/// `JSON.stringify(value)`（JS 键序与数字格式）。
pub(crate) fn json_stringify_js<T: Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).unwrap_or(JsonValue::Null);
    kiana_ai::utils::error_body::safe_json_stringify(&value)
}

/// [`JsonValue`] → [`JsonObject`]（非对象时为 `None`）。
pub(crate) fn json_object_from_value(value: JsonValue) -> Option<JsonObject> {
    match value {
        JsonValue::Object(map) => Some(kiana_ai::utils::json_parse::json_object_from_map(map)),
        _ => None,
    }
}

/// `IndexMap` → `serde_json::Map`（保持插入序）。
pub(crate) fn to_serde_map(object: &JsonObject) -> serde_json::Map<String, JsonValue> {
    let mut map = serde_json::Map::with_capacity(object.len());
    for (key, value) in object {
        map.insert(key.clone(), value.clone());
    }
    map
}

/// JS `Number(value)`：JSON 值 → `f64`（不可转换时为 `NaN`）。
///
/// 用于迁移里的 `version` 比较（TS 的 `header?.version ?? 1` 后直接做 `<`/`>=`）。
fn js_number_value(value: &JsonValue) -> f64 {
    match value {
        JsonValue::Null => 0.0,
        JsonValue::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        JsonValue::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        JsonValue::String(text) => js_parse_number_string(text),
        JsonValue::Array(items) => js_parse_number_string(&js_array_to_string(items)),
        JsonValue::Object(_) => f64::NAN,
    }
}

/// `header?.version ?? 1`：`null`/缺省为 `None`（调用方回退到 1），其余按 JS `Number()`。
fn js_version_number(value: Option<&JsonValue>) -> Option<f64> {
    match value {
        None | Some(JsonValue::Null) => None,
        Some(value) => Some(js_number_value(value)),
    }
}

/// JS `Number(string)`（`StringNumericLiteral`）：空串/空白 → 0，`Infinity`、`0x`/`0b`/`0o`
/// 前缀与十进制支持，其余 → `NaN`。Rust 的 `f64::from_str` 接受 `inf`/`NaN` 等 JS 不接受的
/// 拼写，因此先按 JS 语法校验。
fn js_parse_number_string(text: &str) -> f64 {
    let text = js_trim(text);
    if text.is_empty() {
        return 0.0;
    }
    let (sign, body) = match text.as_bytes()[0] {
        b'+' => (1.0, &text[1..]),
        b'-' => (-1.0, &text[1..]),
        _ => (1.0, text),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    // 进制前缀不允许带符号（`Number("-0x10")` 是 NaN）。
    if !text.starts_with('+') && !text.starts_with('-') {
        if let Some(digits) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            return parse_radix_digits(digits, 16);
        }
        if let Some(digits) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            return parse_radix_digits(digits, 2);
        }
        if let Some(digits) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            return parse_radix_digits(digits, 8);
        }
    }
    if !is_decimal_number_literal(body) {
        return f64::NAN;
    }
    match body.parse::<f64>() {
        Ok(value) => sign * value,
        Err(_) => f64::NAN,
    }
}

/// `0x`/`0b`/`0o` 之后的数字串（至少一位，全部合法）。
fn parse_radix_digits(digits: &str, radix: u32) -> f64 {
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return f64::NAN;
    }
    match u128::from_str_radix(digits, radix) {
        Ok(value) => value as f64,
        // 超出 u128 时按 JS 的浮点舍入值：逐位累加。
        Err(_) => digits.chars().fold(0.0_f64, |acc, c| {
            let digit = c.to_digit(radix).unwrap_or(0) as f64;
            acc * radix as f64 + digit
        }),
    }
}

/// JS `StrDecimalLiteral`（不含 `Infinity`）：`digits[.digits][e[+-]digits]` 或
/// `.digits[e...]`，整体必须消费完。
fn is_decimal_number_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut digits = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
        digits += 1;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        index += 1;
        if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
            index += 1;
        }
        let mut exponent_digits = 0;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            exponent_digits += 1;
        }
        if exponent_digits == 0 {
            return false;
        }
    }
    index == bytes.len()
}

/// `Array.prototype.toString()`：元素以 `,` 连接，`null`/`undefined` 元素为空串。
fn js_array_to_string(items: &[JsonValue]) -> String {
    items
        .iter()
        .map(|item| match item {
            JsonValue::Null => String::new(),
            other => js_to_string(Some(other)),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 任意 JSON 值 → `String` 的宽松反序列化（按 JS `String(value)`；缺省由 `default` 提供空串）。
///
/// TS 的 `thinking_level_change.thinkingLevel` / `model_change.provider` / `modelId` 只做属性
/// 读取，不做类型校验：数字/布尔等原始值会被下游直接使用。Rust 的字段是 `String`，用 JS
/// `String(value)` 承载（例如 `5` → `"5"`）。
fn deserialize_lenient_js_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = JsonValue::deserialize(deserializer)?;
    Ok(js_to_string(Some(&value)))
}

/// 任意 JSON 值 → `f64` 的宽松反序列化（按 JS `Number(value)`；缺省由 `default` 提供 `0`）。
///
/// TS 不校验 `compaction.tokensBefore`：数字字符串/数组等原始值参与后续算术。
fn deserialize_lenient_js_number<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = JsonValue::deserialize(deserializer)?;
    Ok(js_number_value(&value))
}

/// `Option<T>` 的宽松反序列化：字段存在且能解析为 `T` 时保留，否则按 `undefined`。
///
/// TS 的 `compaction.usage` / `systemMessage` 等字段只做属性读取或真值判断，类型错误不会
/// 让整个条目失效（`getLatestCompactionEntry` 只看 `type`）。
fn deserialize_lenient_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Option::<JsonValue>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value::<T>(value).ok()))
}

/// `Option<f64>` 的宽松反序列化：**只有** JSON 数字保留（TS 的 `typeof value === "number"`），
/// 其余类型按 `undefined`（`string` 数字不参与 v1 迁移）。
fn deserialize_lenient_number_option<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<JsonValue>::deserialize(deserializer)?;
    Ok(match value {
        Some(JsonValue::Number(number)) => number.as_f64(),
        _ => None,
    })
}

/// `Option<bool>` 的宽松反序列化：非布尔值按 `undefined`。
fn deserialize_lenient_bool_option<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<JsonValue>::deserialize(deserializer)?;
    Ok(match value {
        Some(JsonValue::Bool(value)) => Some(value),
        _ => None,
    })
}

/// 保留“键存在且为 JSON `null`”与“键缺省”的区别。
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<JsonValue>, D::Error>
where
    D: Deserializer<'de>,
{
    JsonValue::deserialize(deserializer).map(Some)
}

/// `version?: number` 的宽松反序列化（v1 无该键）。
///
/// TS 的 `header?.version ?? 1` 保留任意原始值，随后用 JS 关系比较（`Number` 强制转换）：
/// 数字字符串按数值参与比较，无法转换的值（`"abc"`、数组、对象）得到 `NaN`，所有比较均为
/// false（不触发任何字段迁移）。
fn deserialize_lenient_version<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<JsonValue>::deserialize(deserializer)?;
    Ok(js_version_number(value.as_ref()))
}

/// `string | undefined` 字段的宽松反序列化：非字符串按 `undefined` 处理。
///
/// TS 的 header 只校验 `type` 与 `id`；`cwd`/`timestamp`/`parentSession` 即使类型错误，
/// header 仍然有效（读取方按 `typeof === "string"` 判断）。
fn deserialize_lenient_option_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<JsonValue>::deserialize(deserializer)?;
    Ok(match value {
        Some(JsonValue::String(text)) => Some(text),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// 条目类型
// ---------------------------------------------------------------------------

macro_rules! entry_type_marker {
    ($name:ident, $lit:literal) => {
        /// 条目 `type` 字面量判别式。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
        /// TS: session-manager.ts#SessionEntry.type
        pub enum $name {
            /// 固定字面量。
            #[default]
            #[serde(rename = $lit)]
            Marker,
        }
    };
}

entry_type_marker!(SessionTypeMarker, "session");
entry_type_marker!(SessionMessageType, "message");
entry_type_marker!(ThinkingLevelChangeType, "thinking_level_change");
entry_type_marker!(ModelChangeType, "model_change");
entry_type_marker!(UsageEntryType, "usage");
entry_type_marker!(CompactionType, "compaction");
entry_type_marker!(BranchSummaryType, "branch_summary");
entry_type_marker!(CustomEntryType, "custom");
entry_type_marker!(CustomMessageType, "custom_message");
entry_type_marker!(ContextEditType, "context_edit");
entry_type_marker!(LabelType, "label");
entry_type_marker!(SessionInfoType, "session_info");

/// 会话文件头（第一行）。
///
/// 键序 `type, version?, id, timestamp, cwd, parentSession?`。
///
/// 有效性判定只要求 `type == "session"` 且 `id` 能反序列化为字符串；`version` 与其它
/// 可选字段都是宽松解析：`version` 缺失或为 JSON `null` 时是 `None`，其余值按 JS
/// `Number()` 转成 `f64`，字符串/数组/对象等无法转换的值成为 `NaN`（`NaN` 与任何版本
/// 比较都为 false，因此不会执行迁移步骤；但 `migrate_to_current_version` 仍返回“有迁移、
/// 需写回”）。`timestamp`/`cwd`/`parentSession` 等非字符串值才降级为 `None`。因此旧文件/
/// 手工编辑的文件仍能识别为会话。`type` 或 `id` 不合法时整个文件不会被视为会话文件。
///
/// TS: `session-manager.ts#SessionHeader`
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    /// 恒为 `"session"`。
    #[serde(rename = "type")]
    pub kind: SessionTypeMarker,
    /// v1 会话没有该字段。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_version"
    )]
    pub version: Option<f64>,
    /// 会话 id（UUIDv7 或自定义 id）。
    pub id: String,
    /// ISO 8601 时间戳。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_option_string"
    )]
    pub timestamp: Option<String>,
    /// 会话启动时的 cwd。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_option_string"
    )]
    pub cwd: Option<String>,
    /// fork 来源会话文件。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_option_string"
    )]
    pub parent_session: Option<String>,
}

/// `NewSessionOptions`。
///
/// TS: `session-manager.ts#NewSessionOptions`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewSessionOptions {
    /// 指定会话 id（必须通过 [`assert_valid_session_id`]）。
    pub id: Option<String>,
    /// 父会话文件路径。
    pub parent_session: Option<String>,
}

impl SessionHeader {
    /// header 的 `id`。
    /// TS: session-manager.ts#idString
    pub fn id_string(&self) -> &str {
        &self.id
    }

    /// `getSessionHeaderCwd`：`typeof cwd === "string" ? cwd : undefined`。
    /// TS: session-manager.ts#getSessionHeaderCwd
    pub fn cwd_string(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
}

/// `message` 条目。
///
/// TS: `session-manager.ts#SessionMessageEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    /// 恒为 `"message"`。
    #[serde(rename = "type")]
    pub kind: SessionMessageType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id（root 为 `null`）。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// 消息本体。
    pub message: AgentMessage,
    /// `JSON.parse` 得到的原始消息对象（仅从文件加载时保留）。
    ///
    /// TS 的 `sessionEntryToContextMessages` 原样返回消息对象；当原始对象带有 typed
    /// [`AgentMessage`] 之外的额外键时，投影会回放这个原始对象（见
    /// [`session_entry_to_context_messages`]）。通过 append API 新建的条目没有原始对象。
    #[serde(skip)]
    pub raw_message: Option<JsonObject>,
}

/// `thinking_level_change` 条目。
///
/// `thinkingLevel` 在 TS 里是未校验的任意值；Rust 的 `String` 字段用 JS `String(value)`
/// 承载非字符串值（数字 `5` → `"5"`）。
///
/// TS: `session-manager.ts#ThinkingLevelChangeEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    /// 恒为 `"thinking_level_change"`。
    #[serde(rename = "type")]
    pub kind: ThinkingLevelChangeType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// thinking level。
    #[serde(default, deserialize_with = "deserialize_lenient_js_string")]
    pub thinking_level: String,
}

/// `model_change` 条目。
///
/// `provider`/`modelId` 在 TS 里是未校验的任意值；Rust 的 `String` 字段用 JS
/// `String(value)` 承载非字符串值。
///
/// TS: `session-manager.ts#ModelChangeEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    /// 恒为 `"model_change"`。
    #[serde(rename = "type")]
    pub kind: ModelChangeType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// provider id。
    #[serde(default, deserialize_with = "deserialize_lenient_js_string")]
    pub provider: String,
    /// model id。
    #[serde(default, deserialize_with = "deserialize_lenient_js_string")]
    pub model_id: String,
}

/// `usage` 条目（模型归属的用量，不进入 LLM 上下文）。
///
/// TS: `session-manager.ts#UsageEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    /// 恒为 `"usage"`。
    #[serde(rename = "type")]
    pub kind: UsageEntryType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// 用量类别（如 `"cache_warm"`）。
    #[serde(default, rename = "kind")]
    pub kind_name: String,
    /// provider id。
    #[serde(default)]
    pub provider: String,
    /// model id。
    #[serde(default)]
    pub model: String,
    /// 用量。
    #[serde(default)]
    pub usage: Usage,
    /// 人类可读的限定说明（空串不写键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `JSON.parse` 得到的原始条目对象（仅从文件加载时保留）。
    ///
    /// TS 直接读取原始 `provider` / `model` / `usage`：缺省字段在模板插值里是
    /// `"undefined"`（Rust 的 typed `String` 默认值会变成空串），因此按模型分组的
    /// key 必须从原始对象读取。
    #[serde(skip)]
    pub raw_entry: Option<JsonObject>,
}

/// `compaction` 条目。
///
/// 键序 `type,id,parentId,timestamp,summary,firstKeptEntryId,tokensBefore,details?,
/// usage?,fromHook?,systemMessage?`（v1 遗留的 `firstKeptEntryIndex` 在迁移时删除）。
///
/// TS: `session-manager.ts#CompactionEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    /// 恒为 `"compaction"`。
    #[serde(rename = "type")]
    pub kind: CompactionType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// 摘要文本。
    ///
    /// TS 不校验字段类型；投影读取原始对象（[`CompactionEntry::raw_entry`]），这里用
    /// JS `String(value)` 承载，保证畸形条目仍能识别为 `compaction`。
    #[serde(default, deserialize_with = "deserialize_lenient_js_string")]
    pub summary: String,
    /// 保留区起点（retain-none 时为自身 id）。
    #[serde(default, deserialize_with = "deserialize_lenient_js_string")]
    pub first_kept_entry_id: String,
    /// 压缩前 token 数。
    #[serde(default, deserialize_with = "deserialize_lenient_js_number")]
    pub tokens_before: f64,
    /// 扩展私有数据。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub details: Option<JsonValue>,
    /// 生成摘要的 LLM 调用用量。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_option"
    )]
    pub usage: Option<Usage>,
    /// 是否由扩展生成。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_bool_option"
    )]
    pub from_hook: Option<bool>,
    /// 压缩边界处的完整 system 消息（timestamp 被替换为条目时间）。
    ///
    /// 字段类型不合法时按 `undefined`（TS 的投影仍会原样返回原始对象，见
    /// [`session_entry_to_context_messages`]）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_lenient_option"
    )]
    pub system_message: Option<SystemMessage>,
    /// v1 遗留字段：`firstKeptEntryIndex`（迁移时转换为 id 并删除）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "firstKeptEntryIndex",
        deserialize_with = "deserialize_lenient_number_option"
    )]
    pub first_kept_entry_index: Option<f64>,
    /// `JSON.parse` 得到的原始条目对象（仅从文件加载时保留）。
    ///
    /// TS 的 `sessionEntryToContextMessages` 直接读取原始字段：`summary` / `tokensBefore`
    /// 缺省时生成的 `compactionSummary` 消息省略对应键（而不是空串 / 0）。
    #[serde(skip)]
    pub raw_entry: Option<JsonObject>,
}

/// `branch_summary` 条目。
///
/// TS: `session-manager.ts#BranchSummaryEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    /// 恒为 `"branch_summary"`。
    #[serde(rename = "type")]
    pub kind: BranchSummaryType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// 被放弃分支的 leaf id（或字面量 `"root"`）。
    #[serde(default)]
    pub from_id: String,
    /// 摘要文本。
    #[serde(default)]
    pub summary: String,
    /// 扩展私有数据（不进入 LLM）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub details: Option<JsonValue>,
    /// 生成摘要的 LLM 用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 是否由扩展生成。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
    /// `JSON.parse` 得到的原始条目对象（仅从文件加载时保留）。
    ///
    /// TS 的 `sessionEntryToContextMessages` 直接读取原始 `summary` / `fromId` / `timestamp`。
    #[serde(skip)]
    pub raw_entry: Option<JsonObject>,
}

/// 扩展私有 `custom` 条目（不进入 LLM 上下文）。
///
/// 键序 `type,customType,data?,id,parentId,timestamp`（**不是** base 前置；与
/// `appendCustomEntry` 的对象字面量一致）。
///
/// TS: `session-manager.ts#CustomEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    /// 恒为 `"custom"`。
    #[serde(rename = "type")]
    pub kind: CustomEntryType,
    /// 扩展标识。
    #[serde(default)]
    pub custom_type: String,
    /// 扩展数据。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub data: Option<JsonValue>,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
}

/// 参与 LLM 上下文的自定义消息条目。
///
/// 键序 `type,customType,content,display,details,id,parentId,timestamp`（**不是** base 前置）。
///
/// TS: `session-manager.ts#CustomMessageEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    /// 恒为 `"custom_message"`。
    #[serde(rename = "type")]
    pub kind: CustomMessageType,
    /// 扩展标识。
    #[serde(default)]
    pub custom_type: String,
    /// 字符串或内容块数组（宽松保存任意 JSON；`null` 在投影时归一化为 `[]`）。
    #[serde(default)]
    pub content: JsonValue,
    /// TUI 是否显示。
    #[serde(default)]
    pub display: bool,
    /// 扩展私有元数据。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub details: Option<JsonValue>,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// `JSON.parse` 得到的原始条目对象（仅从文件加载时保留）。
    ///
    /// TS 的 `sessionEntryToContextMessages` 用 `entry.customType` / `entry.content ?? []` /
    /// `entry.display` / `entry.details` / `entry.timestamp` 重新构造消息：未校验的类型原样
    /// 保留，缺省字段在 `JSON.stringify` 时被省略。typed 视图无法表达这些（`String`/`bool`
    /// 默认值会物化空串与 `false`），因此从文件加载时回放原始字段。
    #[serde(skip)]
    pub raw_entry: Option<JsonObject>,
}

/// `context_edit` 的替换内容：`{ content }`。
///
/// TS: `session-manager.ts#ContextEditEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEditReplacement {
    /// 替换后的内容（字符串或内容块数组）。
    #[serde(default)]
    pub content: JsonValue,
}

/// 追加式的上下文编辑条目（只影响模型投影）。
///
/// TS: `session-manager.ts#ContextEditEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditEntry {
    /// 恒为 `"context_edit"`。
    #[serde(rename = "type")]
    pub kind: ContextEditType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// 被编辑的条目 id。
    #[serde(default)]
    pub target_id: String,
    /// `null` 表示从模型上下文省略目标。
    #[serde(default)]
    pub replacement: Option<ContextEditReplacement>,
    /// 原始 JSON 条目（仅从文件加载时保留）。
    ///
    /// TS 不校验字段类型；`entry.replacement === null` 与「缺少 `replacement` 键」
    /// （`undefined`）语义不同。typed 视图里两者都是 `None`，所以回放原始字段。
    #[serde(skip)]
    pub raw_entry: Option<JsonObject>,
}

/// 用户书签 / 标记。
///
/// TS: `session-manager.ts#LabelEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    /// 恒为 `"label"`。
    #[serde(rename = "type")]
    pub kind: LabelType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// 目标条目 id。
    #[serde(default)]
    pub target_id: String,
    /// 标签值（`None` 时省略键；空串会写 `"label": ""`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// 会话元数据条目（如显示名）。
///
/// TS: `session-manager.ts#SessionInfoEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    /// 恒为 `"session_info"`。
    #[serde(rename = "type")]
    pub kind: SessionInfoType,
    /// 条目 id。
    #[serde(default)]
    pub id: String,
    /// 父条目 id。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ISO 8601 时间戳。
    #[serde(default)]
    pub timestamp: String,
    /// 显示名（空串显式清除标题）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 会话条目联合。
///
/// 已知类型用带固定字段顺序的结构体（序列化字节与 TS 对象字面量一致）；未知类型保留
/// 原始 JSON 对象，保证 `_rewriteFile` / fork / copy 能原样回写。
///
/// TS: `session-manager.ts#SessionEntry`
///
/// 变体大小差异来自 TS 条目形状本身（`CompactionEntry` 字段最多）；装箱会改变所有下游
/// match，因此保留直接嵌套并关闭该 lint。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEntry {
    /// `type: "message"`。
    Message(SessionMessageEntry),
    /// `type: "thinking_level_change"`。
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    /// `type: "model_change"`。
    ModelChange(ModelChangeEntry),
    /// `type: "usage"`。
    Usage(UsageEntry),
    /// `type: "compaction"`。
    Compaction(CompactionEntry),
    /// `type: "branch_summary"`。
    BranchSummary(BranchSummaryEntry),
    /// `type: "custom"`。
    Custom(CustomEntry),
    /// `type: "custom_message"`。
    CustomMessage(CustomMessageEntry),
    /// `type: "context_edit"`。
    ContextEdit(ContextEditEntry),
    /// `type: "label"`。
    Label(LabelEntry),
    /// `type: "session_info"`。
    SessionInfo(SessionInfoEntry),
    /// 未知 / 无法解析的条目（保留原始对象）。
    Unknown(JsonObject),
}

impl Serialize for SessionEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            SessionEntry::Message(entry) => entry.serialize(serializer),
            SessionEntry::ThinkingLevelChange(entry) => entry.serialize(serializer),
            SessionEntry::ModelChange(entry) => entry.serialize(serializer),
            SessionEntry::Usage(entry) => entry.serialize(serializer),
            SessionEntry::Compaction(entry) => entry.serialize(serializer),
            SessionEntry::BranchSummary(entry) => entry.serialize(serializer),
            SessionEntry::Custom(entry) => entry.serialize(serializer),
            SessionEntry::CustomMessage(entry) => entry.serialize(serializer),
            SessionEntry::ContextEdit(entry) => entry.serialize(serializer),
            SessionEntry::Label(entry) => entry.serialize(serializer),
            SessionEntry::SessionInfo(entry) => entry.serialize(serializer),
            SessionEntry::Unknown(object) => object.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SessionEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = JsonValue::deserialize(deserializer)?;
        let Some(object) = json_object_from_value(value) else {
            return Err(serde::de::Error::custom("session entry must be an object"));
        };
        Ok(parse_entry_object(object))
    }
}

/// 按 `type` 分派解析条目；无法解析的已知识别值退化为 [`SessionEntry::Unknown`]。
fn parse_entry_object(object: JsonObject) -> SessionEntry {
    let kind = object
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let parsed = match kind.as_str() {
        "message" => from_object::<SessionMessageEntry>(&object).map(|mut entry| {
            // 保留原始消息对象；投影时若它带有 typed 视图之外的键，按 TS 原样回放。
            entry.raw_message = object
                .get("message")
                .and_then(|value| json_object_from_value(value.clone()));
            SessionEntry::Message(entry)
        }),
        "thinking_level_change" => {
            from_object::<ThinkingLevelChangeEntry>(&object).map(SessionEntry::ThinkingLevelChange)
        }
        "model_change" => from_object::<ModelChangeEntry>(&object).map(SessionEntry::ModelChange),
        "usage" => from_object::<UsageEntry>(&object).map(|mut entry| {
            entry.raw_entry = Some(object.clone());
            SessionEntry::Usage(entry)
        }),
        "compaction" => from_object::<CompactionEntry>(&object).map(|mut entry| {
            entry.raw_entry = Some(object.clone());
            SessionEntry::Compaction(entry)
        }),
        "branch_summary" => from_object::<BranchSummaryEntry>(&object).map(|mut entry| {
            entry.raw_entry = Some(object.clone());
            SessionEntry::BranchSummary(entry)
        }),
        "custom" => from_object::<CustomEntry>(&object).map(SessionEntry::Custom),
        "custom_message" => from_object::<CustomMessageEntry>(&object).map(|mut entry| {
            entry.raw_entry = Some(object.clone());
            SessionEntry::CustomMessage(entry)
        }),
        "context_edit" => from_object::<ContextEditEntry>(&object).map(|mut entry| {
            entry.raw_entry = Some(object.clone());
            SessionEntry::ContextEdit(entry)
        }),
        "label" => from_object::<LabelEntry>(&object).map(SessionEntry::Label),
        "session_info" => from_object::<SessionInfoEntry>(&object).map(SessionEntry::SessionInfo),
        _ => None,
    };
    parsed.unwrap_or(SessionEntry::Unknown(object))
}

/// 从保留插入序的对象反序列化。
fn from_object<T: DeserializeOwned>(object: &JsonObject) -> Option<T> {
    serde_json::from_value(JsonValue::Object(to_serde_map(object))).ok()
}

impl SessionEntry {
    /// 条目 `type`。
    /// TS: session-manager.ts#SessionEntry.type
    pub fn kind(&self) -> &str {
        match self {
            SessionEntry::Message(_) => "message",
            SessionEntry::ThinkingLevelChange(_) => "thinking_level_change",
            SessionEntry::ModelChange(_) => "model_change",
            SessionEntry::Usage(_) => "usage",
            SessionEntry::Compaction(_) => "compaction",
            SessionEntry::BranchSummary(_) => "branch_summary",
            SessionEntry::Custom(_) => "custom",
            SessionEntry::CustomMessage(_) => "custom_message",
            SessionEntry::ContextEdit(_) => "context_edit",
            SessionEntry::Label(_) => "label",
            SessionEntry::SessionInfo(_) => "session_info",
            SessionEntry::Unknown(object) => object
                .get("type")
                .and_then(JsonValue::as_str)
                .unwrap_or_default(),
        }
    }

    /// 条目 `id`。
    /// TS: session-manager.ts#SessionEntry.id
    pub fn id(&self) -> &str {
        match self {
            SessionEntry::Message(entry) => &entry.id,
            SessionEntry::ThinkingLevelChange(entry) => &entry.id,
            SessionEntry::ModelChange(entry) => &entry.id,
            SessionEntry::Usage(entry) => &entry.id,
            SessionEntry::Compaction(entry) => &entry.id,
            SessionEntry::BranchSummary(entry) => &entry.id,
            SessionEntry::Custom(entry) => &entry.id,
            SessionEntry::CustomMessage(entry) => &entry.id,
            SessionEntry::ContextEdit(entry) => &entry.id,
            SessionEntry::Label(entry) => &entry.id,
            SessionEntry::SessionInfo(entry) => &entry.id,
            SessionEntry::Unknown(object) => object
                .get("id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default(),
        }
    }

    /// 条目 `parentId`（`null`/缺失为 `None`）。
    /// TS: session-manager.ts#SessionEntry.parentId
    pub fn parent_id(&self) -> Option<&str> {
        match self {
            SessionEntry::Message(entry) => entry.parent_id.as_deref(),
            SessionEntry::ThinkingLevelChange(entry) => entry.parent_id.as_deref(),
            SessionEntry::ModelChange(entry) => entry.parent_id.as_deref(),
            SessionEntry::Usage(entry) => entry.parent_id.as_deref(),
            SessionEntry::Compaction(entry) => entry.parent_id.as_deref(),
            SessionEntry::BranchSummary(entry) => entry.parent_id.as_deref(),
            SessionEntry::Custom(entry) => entry.parent_id.as_deref(),
            SessionEntry::CustomMessage(entry) => entry.parent_id.as_deref(),
            SessionEntry::ContextEdit(entry) => entry.parent_id.as_deref(),
            SessionEntry::Label(entry) => entry.parent_id.as_deref(),
            SessionEntry::SessionInfo(entry) => entry.parent_id.as_deref(),
            SessionEntry::Unknown(object) => object.get("parentId").and_then(JsonValue::as_str),
        }
    }

    /// 条目 `timestamp`（ISO 字符串）。
    /// TS: session-manager.ts#SessionEntry.timestamp
    pub fn timestamp(&self) -> Option<&str> {
        match self {
            SessionEntry::Message(entry) => Some(&entry.timestamp),
            SessionEntry::ThinkingLevelChange(entry) => Some(&entry.timestamp),
            SessionEntry::ModelChange(entry) => Some(&entry.timestamp),
            SessionEntry::Usage(entry) => Some(&entry.timestamp),
            SessionEntry::Compaction(entry) => Some(&entry.timestamp),
            SessionEntry::BranchSummary(entry) => Some(&entry.timestamp),
            SessionEntry::Custom(entry) => Some(&entry.timestamp),
            SessionEntry::CustomMessage(entry) => Some(&entry.timestamp),
            SessionEntry::ContextEdit(entry) => Some(&entry.timestamp),
            SessionEntry::Label(entry) => Some(&entry.timestamp),
            SessionEntry::SessionInfo(entry) => Some(&entry.timestamp),
            SessionEntry::Unknown(object) => object.get("timestamp").and_then(JsonValue::as_str),
        }
    }

    /// `message` 条目里的消息。
    /// TS: session-manager.ts#SessionEntry.message
    pub fn message(&self) -> Option<&AgentMessage> {
        match self {
            SessionEntry::Message(entry) => Some(&entry.message),
            _ => None,
        }
    }

    /// 修改条目 id（迁移用）。
    fn set_id(&mut self, id: String) {
        match self {
            SessionEntry::Message(entry) => entry.id = id,
            SessionEntry::ThinkingLevelChange(entry) => entry.id = id,
            SessionEntry::ModelChange(entry) => entry.id = id,
            SessionEntry::Usage(entry) => entry.id = id,
            SessionEntry::Compaction(entry) => entry.id = id,
            SessionEntry::BranchSummary(entry) => entry.id = id,
            SessionEntry::Custom(entry) => entry.id = id,
            SessionEntry::CustomMessage(entry) => entry.id = id,
            SessionEntry::ContextEdit(entry) => entry.id = id,
            SessionEntry::Label(entry) => entry.id = id,
            SessionEntry::SessionInfo(entry) => entry.id = id,
            SessionEntry::Unknown(object) => {
                object.insert("id".to_string(), JsonValue::String(id));
            }
        }
    }

    /// 修改条目 parentId（迁移用）。
    fn set_parent_id(&mut self, parent_id: Option<String>) {
        match self {
            SessionEntry::Message(entry) => entry.parent_id = parent_id,
            SessionEntry::ThinkingLevelChange(entry) => entry.parent_id = parent_id,
            SessionEntry::ModelChange(entry) => entry.parent_id = parent_id,
            SessionEntry::Usage(entry) => entry.parent_id = parent_id,
            SessionEntry::Compaction(entry) => entry.parent_id = parent_id,
            SessionEntry::BranchSummary(entry) => entry.parent_id = parent_id,
            SessionEntry::Custom(entry) => entry.parent_id = parent_id,
            SessionEntry::CustomMessage(entry) => entry.parent_id = parent_id,
            SessionEntry::ContextEdit(entry) => entry.parent_id = parent_id,
            SessionEntry::Label(entry) => entry.parent_id = parent_id,
            SessionEntry::SessionInfo(entry) => entry.parent_id = parent_id,
            SessionEntry::Unknown(object) => {
                let value = match parent_id {
                    Some(id) => JsonValue::String(id),
                    None => JsonValue::Null,
                };
                object.insert("parentId".to_string(), value);
            }
        }
    }

    /// 迁移 v1→v2 时转换 `firstKeptEntryIndex`。
    fn convert_first_kept_entry_index(&mut self, target_id: Option<String>) {
        if let SessionEntry::Compaction(entry) = self
            && entry.first_kept_entry_index.is_some()
        {
            if let Some(target_id) = target_id {
                entry.first_kept_entry_id = target_id.clone();
                // 保留的原始对象同样要更新：投影按 TS 读原始 `firstKeptEntryId`。
                if let Some(raw) = &mut entry.raw_entry {
                    raw.insert("firstKeptEntryId".to_string(), JsonValue::String(target_id));
                }
            }
            entry.first_kept_entry_index = None;
            if let Some(raw) = &mut entry.raw_entry {
                raw.shift_remove("firstKeptEntryIndex");
            }
        }
    }
}

/// 会话文件条目：header 或普通条目。
///
/// TS: `session-manager.ts#FileEntry`
///
/// 大小差异来自 `SessionEntry` 的 TS 形状；装箱会让所有 `matches!`/`if let` 下游解构多一层，
/// 因此保留直接嵌套并关闭该 lint。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum FileEntry {
    /// 第一行会话头。
    Header(SessionHeader),
    /// 普通条目。
    Entry(SessionEntry),
    /// `JSON.parse` 得到的非对象真值（数组/非空字符串/非零数字/`true`）。
    ///
    /// （[`parse_session_entries`] 不丢弃假值，因此它也可能产出 `Raw(null)` / `Raw(false)`
    /// 等；文件加载路径会先过滤假值。）
    ///
    /// TS 不校验行内容，`loadEntriesFromFile` 把解析结果直接放进数组，因此首个条目不是
    /// 对象时整个文件都不是 kiana 会话；这些值在重写/导出时也原样回放。
    ///
    /// 注意：`SessionManager` 的 `byId` / `getEntries()` 仍只处理对象条目（TS 会把非对象值
    /// 以 `id: undefined` 放进 `byId`）；公开的 [`SessionEntry`] 类型无法表示 `undefined` id。
    Raw(JsonValue),
}

impl Serialize for FileEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            FileEntry::Header(header) => header.serialize(serializer),
            FileEntry::Entry(entry) => entry.serialize(serializer),
            FileEntry::Raw(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for FileEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = JsonValue::deserialize(deserializer)?;
        Ok(file_entry_from_value(value))
    }
}

impl FileEntry {
    /// 条目 `type`。
    /// TS: session-manager.ts#FileEntry.type
    pub fn kind(&self) -> &str {
        match self {
            FileEntry::Header(_) => "session",
            FileEntry::Entry(entry) => entry.kind(),
            // 非对象值没有 `type` 属性（`entry.type === undefined`）。
            FileEntry::Raw(_) => "",
        }
    }

    /// 条目 id（header 为会话 id）。
    /// TS: session-manager.ts#FileEntry.id
    pub fn id(&self) -> &str {
        match self {
            FileEntry::Header(header) => &header.id,
            FileEntry::Entry(entry) => entry.id(),
            FileEntry::Raw(_) => "",
        }
    }

    /// 是否是会话头。
    /// TS: session-manager.ts#FileEntry.asHeader
    pub fn as_header(&self) -> Option<&SessionHeader> {
        match self {
            FileEntry::Header(header) => Some(header),
            FileEntry::Entry(_) | FileEntry::Raw(_) => None,
        }
    }

    /// 是否是普通条目。
    /// TS: session-manager.ts#FileEntry.asEntry
    pub fn as_entry(&self) -> Option<&SessionEntry> {
        match self {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        }
    }

    /// header（或无法解析成 typed 视图、但 `type` 仍是 `"session"` 的条目）上的
    /// `version`；其它条目返回 `None`。迁移用。
    fn version(&self) -> Option<f64> {
        match self {
            FileEntry::Header(header) => header.version,
            FileEntry::Entry(entry) => match entry {
                SessionEntry::Unknown(object) if entry.kind() == "session" => {
                    js_version_number(object.get("version"))
                }
                _ => None,
            },
            FileEntry::Raw(_) => None,
        }
    }

    fn set_version(&mut self, version: f64) {
        match self {
            FileEntry::Header(header) => header.version = Some(version),
            FileEntry::Entry(entry) => {
                let SessionEntry::Unknown(object) = entry else {
                    return;
                };
                object.insert(
                    "version".to_string(),
                    serde_json::Number::from_f64(version)
                        .map(JsonValue::Number)
                        .unwrap_or(JsonValue::Null),
                );
            }
            // 对数组/字符串/数字赋值自定义属性不会出现在 JSON.stringify 里。
            FileEntry::Raw(_) => {}
        }
    }

    /// 设置条目 id（迁移用；header 不受影响）。
    fn set_id(&mut self, id: String) {
        if let FileEntry::Entry(entry) = self {
            entry.set_id(id);
        }
    }

    /// 设置条目 parentId（迁移用；header 不受影响）。
    fn set_parent_id(&mut self, parent_id: Option<String>) {
        if let FileEntry::Entry(entry) = self {
            entry.set_parent_id(parent_id);
        }
    }

    /// 转换 v1 的 `firstKeptEntryIndex`（迁移用）。
    fn convert_first_kept_entry_index(&mut self, target_id: Option<String>) {
        if let FileEntry::Entry(entry) = self {
            entry.convert_first_kept_entry_index(target_id);
        }
    }
}

/// 条目序列化：`JSON.stringify(entry)`。
///
/// 内部先 `serde_json::to_value` 再做 JS 语义的 stringify：数字格式、`Option::None`
/// 省略键、键序都按 JS 处理；序列化本身失败时退化为字符串 `"null"`。测试 fixture 与
/// `fork_from` 写 header 都用它构造期望字节。
/// TS: session-manager.ts#FileEntry.entryJsonString
pub fn entry_json_string<T: Serialize>(value: &T) -> String {
    json_stringify_js(value)
}

/// TS 的 `entry.parentId` 原值分类：`undefined`（属性缺省）与 `null` 在 `getTree` / 追加时
/// 行为不同，因此必须区分。
#[derive(Debug, Clone, Copy, PartialEq)]
enum ParentIdRef<'a> {
    /// 属性缺省（`undefined`）。
    Undefined,
    /// 显式 `null`。
    Null,
    /// 字符串 id。
    Str(&'a str),
    /// 其它原始值（数字/布尔/对象/数组）。
    Other,
}

/// 从文件加载的条目：`entry` 是供逻辑使用的带类型视图，`raw` 是 `JSON.parse` 得到的原始
/// 对象。
///
/// TS 在内存里保存解析后的普通对象，`_rewriteFile` / `forkFrom` / `createBranchedSession` /
/// 导出都对该对象做 `JSON.stringify`，因此未知字段与键序会原样保留；`raw` 复刻这一点。
/// 通过 append API 新建的条目没有 `raw`（序列化 typed 视图即可，键序与 TS 字面量一致）。
///
/// 不变量：`raw` 存在时必须与 `entry` 表示同一条记录，所有迁移/修改函数都要同时更新两边。
/// 查询/投影走 typed 视图，落盘与导出走 `raw`；只改一边会让内存状态与再次读盘的结果分叉。
#[derive(Debug, Clone, PartialEq)]
struct StoredFileEntry {
    entry: FileEntry,
    raw: Option<JsonObject>,
}

impl StoredFileEntry {
    /// 新建条目（无原始对象）。
    fn new(entry: FileEntry) -> Self {
        StoredFileEntry { entry, raw: None }
    }

    /// 落盘用的 JSON 值：有原始对象时逐字回放，否则序列化 typed 视图。
    ///
    /// 新建条目的 `message` 由 TS 的运行时对象构造，`JSON.stringify` 按插入顺序输出
    /// （provider 字面量 + 后续赋值），与 Rust 结构体声明序不同。这里对整条记录做一次
    /// 递归键序还原；原始对象分支已带有 TS 键序，保持逐字回放。
    fn to_json_value(&self) -> JsonValue {
        match &self.raw {
            Some(raw) => JsonValue::Object(to_serde_map(raw)),
            None => {
                let mut value = kiana_agent_core::wire_keys::to_wire_value(&self.entry)
                    .unwrap_or(JsonValue::Null);
                kiana_agent_core::wire_keys::reorder_wire_keys(&mut value);
                value
            }
        }
    }

    /// `JSON.stringify(entry)`。
    fn json_string(&self) -> String {
        kiana_ai::utils::error_body::safe_json_stringify(&self.to_json_value())
    }

    fn kind(&self) -> &str {
        self.entry.kind()
    }

    fn id(&self) -> &str {
        self.entry.id()
    }

    /// TS 的 `entry.id` 原值：对象条目取 `JSON.parse` 结果的 `id` 属性（可能是任意 JSON
    /// 值或缺省），非对象值没有该属性（`undefined`）。
    fn raw_id_value(&self) -> Option<&JsonValue> {
        match (&self.entry, &self.raw) {
            (FileEntry::Raw(_), _) => None,
            (_, Some(raw)) => raw.get("id"),
            (_, None) => None,
        }
    }

    /// `byId` 的字符串键：`entry.id` 是字符串时返回它。
    ///
    /// 没有原始对象的新建条目（`inMemory(entries)` 的调用方构造、append API）的 `id`
    /// 在 TS 里总是字符串，因此回退到 typed id。
    fn string_id(&self) -> Option<&str> {
        match (&self.entry, &self.raw) {
            (FileEntry::Raw(_), _) => None,
            (_, Some(raw)) => raw.get("id").and_then(JsonValue::as_str),
            (_, None) => Some(self.entry.id()),
        }
    }

    /// TS 的 `entry.parentId` 原值分类（`None` 表示属性缺省 / `undefined`）。
    fn parent_id_ref(&self) -> ParentIdRef<'_> {
        match (&self.entry, &self.raw) {
            (FileEntry::Raw(_), _) => ParentIdRef::Undefined,
            (_, Some(raw)) => match raw.get("parentId") {
                None => ParentIdRef::Undefined,
                Some(JsonValue::Null) => ParentIdRef::Null,
                Some(JsonValue::String(id)) => ParentIdRef::Str(id),
                Some(_) => ParentIdRef::Other,
            },
            // 没有原始对象的新建条目：typed `None` 对应 TS 对象字面量里的 `null`
            // （append API 总是写入 `parentId` 键）。
            (FileEntry::Entry(entry), None) => match entry.parent_id() {
                Some(id) => ParentIdRef::Str(id),
                None => ParentIdRef::Null,
            },
            (FileEntry::Header(_), None) => ParentIdRef::Undefined,
        }
    }

    fn as_header(&self) -> Option<&SessionHeader> {
        self.entry.as_header()
    }

    /// 条目的 `timestamp` 字符串（非对象值/缺省为 `None`）。
    fn timestamp(&self) -> Option<&str> {
        match &self.entry {
            FileEntry::Entry(entry) => entry.timestamp(),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        }
    }

    /// 迁移用：设置 header 版本（原始对象保持键位置；新键追加到末尾）。
    fn set_version(&mut self, version: f64) {
        if let Some(raw) = &mut self.raw {
            raw.insert(
                "version".to_string(),
                serde_json::Number::from_f64(version)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null),
            );
        }
        self.entry.set_version(version);
    }

    /// 迁移用：设置条目 id（原始对象保持键位置；新键追加到末尾）。
    fn set_id(&mut self, id: String) {
        if let Some(raw) = &mut self.raw {
            raw.insert("id".to_string(), JsonValue::String(id.clone()));
        }
        self.entry.set_id(id);
    }

    /// 迁移用：设置 parentId。
    fn set_parent_id(&mut self, parent_id: Option<String>) {
        if let Some(raw) = &mut self.raw {
            raw.insert(
                "parentId".to_string(),
                match &parent_id {
                    Some(id) => JsonValue::String(id.clone()),
                    None => JsonValue::Null,
                },
            );
        }
        self.entry.set_parent_id(parent_id);
    }

    /// 迁移用：`firstKeptEntryIndex` → `firstKeptEntryId` 并删除旧键。
    ///
    /// TS 只在 `typeof firstKeptEntryIndex === "number"` 时转换并删除；非数字值原样保留。
    fn convert_first_kept_entry_index(&mut self, target_id: Option<String>) {
        if self.first_kept_entry_index().is_none() {
            return;
        }
        if let Some(raw) = &mut self.raw {
            if let Some(target_id) = &target_id {
                raw.insert(
                    "firstKeptEntryId".to_string(),
                    JsonValue::String(target_id.clone()),
                );
            }
            raw.shift_remove("firstKeptEntryIndex");
        }
        self.entry.convert_first_kept_entry_index(target_id);
    }

    /// 迁移用：把原始消息对象的 `role: "hookMessage"` 改为 `"custom"`。
    fn migrate_hook_message_role(&mut self) {
        if self.kind() != "message" {
            return;
        }
        if let Some(raw) = &mut self.raw
            && let Some(JsonValue::Object(message)) = raw.get_mut("message")
            && message.get("role").and_then(JsonValue::as_str) == Some("hookMessage")
        {
            message.insert("role".to_string(), JsonValue::String("custom".to_string()));
        }
        if let FileEntry::Entry(SessionEntry::Message(message_entry)) = &mut self.entry
            && message_entry.message.role() == "hookMessage"
        {
            normalize_role_to_custom(&mut message_entry.message);
        }
    }

    /// v1 迁移用：原始对象上的 `firstKeptEntryIndex`（数字）。
    fn first_kept_entry_index(&self) -> Option<f64> {
        match (&self.raw, &self.entry) {
            (Some(raw), _) => raw.get("firstKeptEntryIndex").and_then(JsonValue::as_f64),
            (None, FileEntry::Entry(SessionEntry::Compaction(compaction))) => {
                compaction.first_kept_entry_index
            }
            _ => None,
        }
    }

    /// 条目的原始 `id`（非对象值没有 `id` 属性）。
    fn id_option(&self) -> Option<&str> {
        match &self.entry {
            FileEntry::Raw(_) => None,
            _ => Some(self.entry.id()),
        }
    }
}

impl From<FileEntry> for StoredFileEntry {
    fn from(entry: FileEntry) -> Self {
        StoredFileEntry::new(entry)
    }
}

// ---------------------------------------------------------------------------
// ID 生成
// ---------------------------------------------------------------------------

/// `createSessionId()`：UUIDv7。
///
/// UUIDv7 带时间前缀，便于按创建时间粗略排序；生成失败（例如系统时间异常）时退回
/// `uuid` crate 的 v4 字符串，保证本函数不会失败。
///
/// TS: `session-manager.ts#createSessionId`
pub fn create_session_id() -> String {
    kiana_ai::utils::uuid::uuidv7(None).unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
}

/// `assertValidSessionId(id)`。
///
/// 校验规则等价于正则 `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`：非空、首尾必须是
/// ASCII 字母/数字、中间只允许字母/数字与 `-` `_` `.`。只允许 ASCII，所以非 ASCII 字符
/// （如 `é`）一律拒绝；失败返回 [`SessionManagerError::InvalidSessionId`]（文案是当前稳定
/// 契约，改动需同步测试）。
///
/// TS: `session-manager.ts#assertValidSessionId`
pub fn assert_valid_session_id(id: &str) -> Result<(), SessionManagerError> {
    fn is_alnum(c: char) -> bool {
        c.is_ascii_alphanumeric()
    }
    let chars: Vec<char> = id.chars().collect();
    let valid = !chars.is_empty()
        && is_alnum(chars[0])
        && is_alnum(chars[chars.len() - 1])
        && chars
            .iter()
            .all(|c| is_alnum(*c) || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(SessionManagerError::InvalidSessionId)
    }
}

/// `_buildIndex` 的 label 分支：从原始对象读取 `targetId` / `label` / `timestamp`；
/// label 为真值时写入/覆盖，假值（含缺失）时删除该目标的标签。
///
/// （原注释块开头的 “`generateId(byId)` …” 与本函数无关，已原样移到 `generate_id` 的
/// 定义处——那段文字描述的正是 `generate_id`。）
///
/// TS 直接读取 `JSON.parse` 得到的原始对象：`if (entry.label)` 为真值 → 原样存入
/// `labelsById` / `labelTimestampsById`，否则删除该目标的标签。label/timestamp 的原始值
/// 可以是任意类型；Rust 的映射值是 `String`，非字符串按 JS `String(value)` 承载
/// （`targetId` 非字符串时 TS 以非字符串为 Map 键，字符串查询永远取不到，这里直接跳过）。
fn apply_label_entry(
    stored: &StoredFileEntry,
    entry: &SessionEntry,
    labels_by_id: &mut IndexMap<String, String>,
    label_timestamps_by_id: &mut IndexMap<String, Option<JsonValue>>,
) {
    let fallback = match entry {
        SessionEntry::Label(label_entry) => Some(label_entry),
        _ => None,
    };
    let raw = stored.raw.as_ref();
    let target_id = raw
        .and_then(|object| object.get("targetId"))
        .and_then(JsonValue::as_str)
        .or_else(|| fallback.map(|label_entry| label_entry.target_id.as_str()));
    let Some(target_id) = target_id else {
        return;
    };
    let label_value = match raw {
        Some(object) => object.get("label").cloned(),
        None => fallback
            .and_then(|label_entry| label_entry.label.clone())
            .map(JsonValue::String),
    };
    if label_value.as_ref().is_some_and(js_truthy) {
        let label = js_to_string(label_value.as_ref());
        labels_by_id.insert(target_id.to_string(), label);
        // `labelTimestampsById.set(targetId, entry.timestamp)`：属性缺省时值为 `undefined`
        // （键存在）；其余原始值原样保留（`null`、数字等）。
        match raw {
            Some(object) => {
                label_timestamps_by_id
                    .insert(target_id.to_string(), object.get("timestamp").cloned());
            }
            None => {
                if let Some(label_entry) = fallback {
                    label_timestamps_by_id.insert(
                        target_id.to_string(),
                        Some(JsonValue::String(label_entry.timestamp.clone())),
                    );
                }
            }
        }
    } else {
        labels_by_id.shift_remove(target_id);
        label_timestamps_by_id.shift_remove(target_id);
    }
}

/// `generateId(byId)`：8 位小写十六进制，冲突时最多重试 100 次，最后回退完整 UUID。
///
/// TS: `session-manager.ts#generateId`
///
/// 实现：前 100 次取 v4 UUID 的前 8 个十六进制字符（`simple()` 为无连字符小写形式），
/// 被 `has` 判定冲突就重试；仍冲突时返回完整 UUID 字符串。
///
/// TS 的 `randomUUID().slice(0, 8)` 与本实现都不保证具体取值，只保证“8 位十六进制 +
/// 冲突重试 100 次 + 完整 UUID 兜底”的形状。
fn generate_id(mut has: impl FnMut(&str) -> bool) -> String {
    for _ in 0..100 {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let short = id[..8].to_string();
        if !has(&short) {
            return short;
        }
    }
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// 迁移
// ---------------------------------------------------------------------------

/// v1 → v2：添加 id/parentId 树结构（原地修改）。
///
/// 对每个 `FileEntry::Entry` 的非 header 条目：生成新 id、把上一条目的 id 作为 parentId
/// （第一条为 `None`，序列化为 `"parentId":null`）、把 header 的 `version` 改成 2。
/// `FileEntry::Raw`（`JSON.parse` 得到的非对象真值）无法附加字段：它的 `set_id` /
/// `set_parent_id` 是 no-op，不会写入 id/parentId；但 `prev_id` 仍会前进，后续条目可能
/// 指向一个根本没有落盘的 id（TS 直接对原始值赋 `entry.id`/`entry.parentId`，Rust 无法
/// 在非对象值上附加字段，这是一处能力差异）。compaction 条目额外把
/// v1 的 `firstKeptEntryIndex` 换成目标条目的 id 并删除旧键；索引必须是有限的非负整数
/// 且目标不是 header，否则只删除旧键、不设置 `firstKeptEntryId`。
///
/// 注意源码的行为：`ids` 集合从不插入新 id，因此冲突检测退化为「永不冲突」；这里照抄
/// （TS 的 `generateId(ids)` 同样传入空集合）。
fn migrate_v1_to_v2(entries: &mut [FileEntry]) {
    let mut prev_id: Option<String> = None;
    let len = entries.len();
    for index in 0..len {
        if entries[index].kind() == "session" {
            entries[index].set_version(2.0);
            continue;
        }
        // `generateId(ids)`：ids 恒为空集合。
        let id = generate_id(|_| false);
        entries[index].set_id(id.clone());
        entries[index].set_parent_id(prev_id.clone());

        if entries[index].kind() == "compaction" {
            // TS 只把 number 类型的索引当作可转换值；`entries[index]` 可能是 header，
            // 所以要再过滤目标类型。
            let target_id = match &entries[index] {
                FileEntry::Entry(SessionEntry::Compaction(compaction)) => compaction
                    .first_kept_entry_index
                    .filter(|value| value.is_finite() && value.fract() == 0.0 && *value >= 0.0)
                    .map(|value| value as usize)
                    .and_then(|target| entries.get(target))
                    .filter(|target| target.kind() != "session")
                    .map(|target| target.id().to_string()),
                _ => None,
            };
            entries[index].convert_first_kept_entry_index(target_id);
        }
        prev_id = Some(id);
    }
}

/// v2 → v3：把 `hookMessage` role 重命名为 `custom`（原地修改）。
///
/// 只处理 message 条目且消息 `role == "hookMessage"`；header 的 `version` 改为 3。其它
/// 条目原样保留（包括未知类型与 keep 原始对象的条目——`StoredFileEntry` 版本会同步改
/// `raw`，见 `migrate_stored_v2_to_v3`）。
fn migrate_v2_to_v3(entries: &mut [FileEntry]) {
    for entry in entries.iter_mut() {
        if entry.kind() == "session" {
            entry.set_version(3.0);
            continue;
        }
        if let FileEntry::Entry(SessionEntry::Message(message_entry)) = entry
            && message_entry.message.role() == "hookMessage"
        {
            normalize_role_to_custom(&mut message_entry.message);
        }
    }
}

/// 把 `AgentMessage::Custom` 的 `role` 改为 `"custom"`（键位置保持不变）。
fn normalize_role_to_custom(message: &mut AgentMessage) {
    if let AgentMessage::Custom(object) = message {
        object.insert("role".to_string(), JsonValue::String("custom".to_string()));
    }
}

/// 运行所有必要迁移；有迁移时返回 true。
///
/// 版本取“第一个 `type == "session"` 的条目”的 `version`，缺省按 1 处理（v1 文件没有
/// 该字段）。`NaN`（无法转换的值）与任何数比较都为 false，因此不会触发任何迁移，但仍
/// 返回 true——“有迁移，条目需要写回”。本函数唯一调用方 `migrate_session_entries` 是只读
/// 内存操作；真正据此重写文件的是 `load_entries` 里的 `migrate_stored_to_current_version`。
fn migrate_to_current_version(entries: &mut [FileEntry]) -> bool {
    let version = entries
        .iter()
        .find(|entry| entry.kind() == "session")
        .and_then(FileEntry::version)
        .unwrap_or(1.0);
    if version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if version < 2.0 {
        migrate_v1_to_v2(entries);
    }
    if version < 3.0 {
        migrate_v2_to_v3(entries);
    }
    true
}

/// 供测试导出：就地迁移条目。
///
/// 只修改传入切片，不读写文件；版本已 >= 3 时不做任何事。header 缺失时按 v1 处理，
/// 会为 `FileEntry::Entry` 的非 header 条目补 id/parentId；`FileEntry::Raw` 无法附加字段
/// （`set_id`/`set_parent_id` 是 no-op），不会被补上——调用方应保证输入确实是按文件顺序
/// 排列的条目。
///
/// TS: `session-manager.ts#migrateSessionEntries`
pub fn migrate_session_entries(entries: &mut [FileEntry]) {
    migrate_to_current_version(entries);
}

/// v1 → v2（保留原始对象版本）：修改同时作用于原始对象与 typed 视图。
///
/// 逻辑与 [`migrate_v1_to_v2`] 相同；差别是 compaction 的 `firstKeptEntryIndex` 优先从
/// `raw` 读取（TS 直接看 `JSON.parse` 结果），并且改 id/parentId/version 时同步更新 `raw`，
/// 保证之后 `rewrite_file` 逐字回放的是迁移后的对象而不是旧内容。
fn migrate_stored_v1_to_v2(entries: &mut [StoredFileEntry]) {
    let mut prev_id: Option<String> = None;
    let len = entries.len();
    for index in 0..len {
        if entries[index].kind() == "session" {
            entries[index].set_version(2.0);
            continue;
        }
        // `generateId(ids)`：ids 恒为空集合。
        let id = generate_id(|_| false);
        entries[index].set_id(id.clone());
        entries[index].set_parent_id(prev_id.clone());

        if entries[index].kind() == "compaction" {
            let target_id = entries[index]
                .first_kept_entry_index()
                .filter(|value| value.is_finite() && value.fract() == 0.0 && *value >= 0.0)
                .map(|value| value as usize)
                .and_then(|target| entries.get(target))
                .filter(|target| target.kind() != "session")
                .and_then(StoredFileEntry::id_option)
                .map(str::to_string);
            entries[index].convert_first_kept_entry_index(target_id);
        }
        prev_id = Some(id);
    }
}

/// v2 → v3（保留原始对象版本）。
///
/// `migrate_hook_message_role` 同时改 `raw.message.role` 与 typed 消息，二者必须一致，
/// 否则投影（走 typed）与重写（走 raw）会看到不同的 role。
fn migrate_stored_v2_to_v3(entries: &mut [StoredFileEntry]) {
    for entry in entries.iter_mut() {
        if entry.kind() == "session" {
            entry.set_version(3.0);
            continue;
        }
        entry.migrate_hook_message_role();
    }
}

/// 运行所有必要迁移（保留原始对象版本）；有迁移时返回 true。
fn migrate_stored_to_current_version(entries: &mut [StoredFileEntry]) -> bool {
    // TS 直接读取 `JSON.parse` 得到的 `version` 并做 JS 关系比较；有原始对象时优先用它，
    // 避免类型化视图把 `"abc"` 之类无法比较的值误当成 v1。
    let version = entries
        .iter()
        .find(|entry| entry.kind() == "session")
        .and_then(|entry| match &entry.raw {
            Some(raw) => js_version_number(raw.get("version")),
            None => entry.entry.version(),
        })
        .unwrap_or(1.0);
    if version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if version < 2.0 {
        migrate_stored_v1_to_v2(entries);
    }
    if version < 3.0 {
        migrate_stored_v2_to_v3(entries);
    }
    true
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// `content.trim().split("\n")`，逐行跳过空行与 JSON 解析失败行。
///
/// TS 不校验行内容：解析成功的任何值（含 `null`/数组/字符串）都会进入结果。
///
/// 与 [`load_entries_from_file`] 的差别：这里不读文件、不要求首条是 header、不修复末尾
/// 换行，也不做任何迁移；适合处理内存中的 JSONL 文本与测试 fixture。
///
/// TS: `session-manager.ts#parseSessionEntries`
pub fn parse_session_entries(content: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in js_trim(content).split('\n') {
        if js_trim(line).is_empty() {
            continue;
        }
        if let Some(value) = parse_session_entry_value(line) {
            entries.push(file_entry_from_value(value));
        }
    }
    entries
}

/// `parseSessionEntryLine(line)`：空行/解析失败返回 `None`；成功时返回解析值（TS 不校验
/// 它是否是对象）。
///
/// 必须用 [`kiana_json::js_parse::parse`]（`JSON.parse` 的等价实现）而不是
/// `serde_json::from_str`：后者会拒绝 V8 接受的孤立代理项转义与超出 f64 范围的数字，
/// 从而把整行当作畸形行丢弃（ARCHITECTURE §8 决定 31/32）。
fn parse_session_entry_value(line: &str) -> Option<JsonValue> {
    if js_trim(line).is_empty() {
        return None;
    }
    kiana_json::js_parse::parse(line).ok()
}

/// 解析值 → [`FileEntry`]：对象走 header/条目分派，其它值保留为 [`FileEntry::Raw`]。
fn file_entry_from_value(value: JsonValue) -> FileEntry {
    match value {
        JsonValue::Object(map) => {
            let object = kiana_ai::utils::json_parse::json_object_from_map(map);
            file_entry_from_object(object)
        }
        other => FileEntry::Raw(other),
    }
}

fn file_entry_from_object(object: JsonObject) -> FileEntry {
    if object.get("type").and_then(JsonValue::as_str) == Some("session")
        && let Some(header) = from_object::<SessionHeader>(&object)
    {
        return FileEntry::Header(header);
    }
    FileEntry::Entry(parse_entry_object(object))
}

/// 增量 UTF-8 解码器（`StringDecoder("utf8")` 的等价物）。
///
/// 保留末尾不完整的多字节序列；其余部分按 WHATWG 规则解码（无效序列替换为 U+FFFD）。
#[derive(Default)]
struct Utf8StreamDecoder {
    carry: Vec<u8>,
}

impl Utf8StreamDecoder {
    /// `decoder.write(chunk)`。
    fn write(&mut self, chunk: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(chunk);
        let split = incomplete_utf8_suffix_len(&bytes);
        let (complete, tail) = bytes.split_at(bytes.len() - split);
        let text = String::from_utf8_lossy(complete).into_owned();
        self.carry = tail.to_vec();
        text
    }

    /// `decoder.end()`。
    fn end(&mut self) -> String {
        let bytes = std::mem::take(&mut self.carry);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// 末尾不完整 UTF-8 序列的字节数（0..=3）。
fn incomplete_utf8_suffix_len(bytes: &[u8]) -> usize {
    let len = bytes.len();
    let max = len.min(3);
    for back in 1..=max {
        let byte = bytes[len - back];
        if byte & 0b1100_0000 == 0b1000_0000 {
            // 续字节：继续向前找起始字节。
            continue;
        }
        let needed = if byte & 0b1000_0000 == 0 {
            1
        } else if byte & 0b1110_0000 == 0b1100_0000 {
            2
        } else if byte & 0b1111_0000 == 0b1110_0000 {
            3
        } else if byte & 0b1111_1000 == 0b1111_0000 {
            4
        } else {
            // 无效起始字节：不是「不完整」，交给 lossy 解码。
            return 0;
        };
        if needed > back {
            return back;
        }
        return 0;
    }
    0
}

/// 读取整个文件并按行解析（流式；1 MiB 块 + 增量解码）。
///
/// 与 [`parse_session_entries`] 的差别：补写末尾换行、校验首个条目是合法 header。
///
/// 返回值：文件不存在返回空 `Vec`（不报错）；首个**成功解析且为真值**的条目不是合法
/// header 时同样返回空 `Vec`，并把该文件视为“不是会话文件”。前导的空行/畸形行会被跳过，
/// 因此 header 不必出现在第一行（测试 `reads cwd from a session with leading malformed
/// lines` 锁定该行为）。真实 IO 失败（打开/读取/补写换行）返回
/// [`SessionManagerError::Io`]。
///
/// 副作用：文件以合法 header 开头且最后一行没有换行符时，会以追加模式向文件补一个
/// `\n`（即使最后一行是畸形片段）。这是 TS 的 `appendFileSync(...,"\n")` 行为，目的是
/// 让后续 append 不会与最后一条记录粘在同一行。header 校验失败时不会触碰文件。
///
/// TS: `session-manager.ts#loadEntriesFromFile`
pub fn load_entries_from_file(file_path: &str) -> Result<Vec<FileEntry>, SessionManagerError> {
    Ok(load_stored_entries_from_file(file_path)?
        .into_iter()
        .map(|stored| stored.entry)
        .collect())
}

/// [`load_entries_from_file`] 的实现，额外保留原始 JSON 对象。
fn load_stored_entries_from_file(
    file_path: &str,
) -> Result<Vec<StoredFileEntry>, SessionManagerError> {
    let resolved = kiana_core::paths::normalize_path(file_path, &Default::default());
    // 文件不存在不是错误：会话发现与 `open` 都依赖“空结果=不可用”的约定。
    if !Path::new(&resolved).exists() {
        return Ok(Vec::new());
    }

    // 分块读取 + 增量 UTF-8 解码：`decoder` 会暂存跨块的不完整多字节序列。
    // `pending` 只保存“尚未遇到换行”的当前行片段（不是整个文件），因此不会构造一个与
    // 文件同大的字符串（测试 `opens_session_files_larger_than_nodes_max_string_length`
    // 依赖这一点；解析后的条目仍会累积在 `entries` 里）。
    let mut entries: Vec<StoredFileEntry> = Vec::new();
    let mut pending = String::new();
    let file = std::fs::File::open(&resolved)?;
    let mut reader = BufReader::with_capacity(SESSION_READ_BUFFER_SIZE, file);
    let mut decoder = Utf8StreamDecoder::default();
    let mut buffer = vec![0u8; SESSION_READ_BUFFER_SIZE];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        pending.push_str(&decoder.write(&buffer[..bytes_read]));
        let mut line_start = 0usize;
        while let Some(offset) = pending[line_start..].find('\n') {
            let newline_index = line_start + offset;
            if let Some(value) = parse_session_entry_value(&pending[line_start..newline_index]) {
                // TS: `if (entry) entries.push(entry)`（假值行被跳过）。
                // 注意这里与 `parse_session_entries` 不同：文件加载路径会丢弃
                // `null`/`false`/`0`/`""` 这类解析成功但为假值的行。
                if js_truthy(&value) {
                    entries.push(stored_entry_from_value(value));
                }
            }
            line_start = newline_index + 1;
        }
        // 保留最后一个换行之后的内容，作为下一块的起始片段。
        pending = pending[line_start..].to_string();
    }

    // 文件末尾没有换行时，`pending` 是最后一条记录；有换行时它是空串。
    pending.push_str(&decoder.end());
    if let Some(value) = parse_session_entry_value(&pending)
        && js_truthy(&value)
    {
        entries.push(stored_entry_from_value(value));
    }

    // 先校验 header，再修复文件（首个条目必须是 `{type:"session", id: string}`）。
    // 非空但无 header 的文件原样返回空，避免把用户文件截断成空会话。
    if entries.is_empty() || !matches!(&entries[0].entry, FileEntry::Header(_)) {
        return Ok(Vec::new());
    }

    // 末尾无换行：追加一个 `\n`，保证之后 append 的记录不会粘在最后一行上。
    // `pending` 非空恰表示“最后一条记录没有以换行结束”（即使它解析失败）。
    if !pending.is_empty() {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&resolved)
            .map_err(|error| node_fs_error(error, "open", &resolved))?;
        file.write_all(b"\n")?;
    }
    Ok(entries)
}

/// 解析值 → 带原始对象的条目。
fn stored_entry_from_value(value: JsonValue) -> StoredFileEntry {
    match value {
        JsonValue::Object(map) => {
            let object = kiana_ai::utils::json_parse::json_object_from_map(map);
            let entry = file_entry_from_object(object.clone());
            StoredFileEntry {
                entry,
                raw: Some(object),
            }
        }
        other => StoredFileEntry {
            entry: FileEntry::Raw(other),
            raw: None,
        },
    }
}

/// 读取会话头（最多扫描 1 MiB）。
///
/// 返回值：`Ok(Some(header))` 命中；`Ok(None)` 表示首条真值行不是 session header（或者
/// 扫描到 EOF 仍未命中）；`Err(HeaderScanLimit)` 表示读满 1 MiB 后 EOF 探测仍读到字节
/// （文件还有未读内容）——可能由超大 header 造成，也可能由大量空行/畸形行铺满 1 MiB 前缀
/// 造成（扫描过程中确实会遇到并处理换行，只是总字节数先到上限）。`open` 用
/// `is_header_scan_limit()` 识别这种情况并回退到全量流式加载，其余错误原样上抛。
///
/// 空白行与畸形行会被跳过并继续向前找；一旦读到“真值且不是 header”的行就立即停止，
/// 不会越过它继续向后找。
///
/// TS: `session-manager.ts#readSessionHeader`
fn read_session_header(file_path: &str) -> Result<Option<SessionHeader>, SessionManagerError> {
    let file = std::fs::File::open(file_path)?;
    let mut reader = BufReader::with_capacity(SESSION_HEADER_READ_BUFFER_SIZE, file);
    let mut decoder = Utf8StreamDecoder::default();
    let mut line_chunks = String::new();
    let mut scanned_bytes = 0usize;
    let mut buffer = vec![0u8; SESSION_HEADER_READ_BUFFER_SIZE];

    // 按块读到扫描上限；只有遇到换行才把累计的 `line_chunks` 当作完整一行解析，
    // 一命中（header 或“确定不是 header”）就返回，不再读后续内容。
    while scanned_bytes < MAX_SESSION_HEADER_SCAN_BYTES {
        let read_length = buffer
            .len()
            .min(MAX_SESSION_HEADER_SCAN_BYTES - scanned_bytes);
        let bytes_read = reader.read(&mut buffer[..read_length])?;
        if bytes_read == 0 {
            line_chunks.push_str(&decoder.end());
            return Ok(parse_session_header_candidate(&line_chunks).unwrap_or(None));
        }
        scanned_bytes += bytes_read;
        let chunk = decoder.write(&buffer[..bytes_read]);
        let mut line_start = 0usize;
        while let Some(offset) = chunk[line_start..].find('\n') {
            let newline_index = line_start + offset;
            line_chunks.push_str(&chunk[line_start..newline_index]);
            if let Some(candidate) = parse_session_header_candidate(&line_chunks) {
                return Ok(candidate);
            }
            line_chunks.clear();
            line_start = newline_index + 1;
        }
        line_chunks.push_str(&chunk[line_start..]);
    }

    // 探测 EOF：恰好结束在扫描上限的、无换行的 header 仍然允许。
    let mut probe = [0u8; 1];
    if reader.read(&mut probe)? == 0 {
        line_chunks.push_str(&decoder.end());
        return Ok(parse_session_header_candidate(&line_chunks).unwrap_or(None));
    }
    Err(SessionManagerError::HeaderScanLimit {
        path: file_path.to_string(),
    })
}

/// `Some(Some(header))` 命中；`Some(None)` 停止（解析出非 header）；`None` 继续扫描。
fn parse_session_header_candidate(line: &str) -> Option<Option<SessionHeader>> {
    if js_trim(line).is_empty() {
        return None;
    }
    let value = parse_session_entry_value(line)?;
    // TS: `if (!entry) return undefined`（解析出的假值按畸形行跳过）。
    if !js_truthy(&value) {
        return None;
    }
    match value {
        JsonValue::Object(map) => {
            let object = kiana_ai::utils::json_parse::json_object_from_map(map);
            if object.get("type").and_then(JsonValue::as_str) == Some("session")
                && let Some(header) = from_object::<SessionHeader>(&object)
            {
                return Some(Some(header));
            }
            Some(None)
        }
        // 非对象没有 `.type` → 停止扫描。
        _ => Some(None),
    }
}

/// 发现用头部读取：吞掉一切异常返回 `None`。
fn read_session_header_for_discovery(file_path: &str) -> Option<SessionHeader> {
    read_session_header(file_path).ok().flatten()
}

/// `sessionCwdMatches(cwd, resolvedCwd)`。
fn session_cwd_matches(cwd: Option<&str>, resolved_cwd: &str) -> bool {
    match cwd {
        Some(cwd) if !cwd.is_empty() => resolve_path_default(cwd) == resolved_cwd,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 会话目录
// ---------------------------------------------------------------------------

/// 默认 agent 目录（`getAgentDir()`）。
fn default_agent_dir() -> String {
    kiana_core::config::get_agent_dir()
        .to_string_lossy()
        .into_owned()
}

/// `getDefaultSessionDirPath(cwd, agentDir?)`。
///
/// 目录名形如 `<agentDir>/sessions/--<cwd>--`：先解析为绝对路径，去掉开头的 `/` 或 `\`，
/// 再把 `/`、`\`、`:` 替换成 `-`。它只是**路径计算**，不创建目录，也不检查是否存在。
///
/// TS: `session-manager.ts#getDefaultSessionDirPath`
pub fn get_default_session_dir_path(cwd: &str, agent_dir: &str) -> String {
    let resolved_cwd = resolve_path_default(cwd);
    let resolved_agent_dir = resolve_path_default(agent_dir);
    let stripped = resolved_cwd
        .strip_prefix('/')
        .or_else(|| resolved_cwd.strip_prefix('\\'))
        .unwrap_or(&resolved_cwd);
    let safe_path = format!(
        "--{}--",
        stripped
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' => '-',
                other => other,
            })
            .collect::<String>()
    );
    path_join(&[&resolved_agent_dir, "sessions", &safe_path])
}

/// `getDefaultSessionDir(cwd, agentDir?)`：目录不存在时递归创建。
///
/// `agent_dir` 缺省时用 `kiana_core::config::get_agent_dir()`。创建失败会把 `io::Error`
/// 包装成 Node 风格文案（`EACCES: permission denied, mkdir '<dir>'`），因为 CLI 直接打印
/// 错误信息。已存在时不验证它是不是目录。
///
/// TS: `session-manager.ts#getDefaultSessionDir`
pub fn get_default_session_dir(
    cwd: &str,
    agent_dir: Option<&str>,
) -> Result<String, SessionManagerError> {
    let agent_dir = agent_dir
        .map(str::to_string)
        .unwrap_or_else(default_agent_dir);
    let session_dir = get_default_session_dir_path(cwd, &agent_dir);
    if !Path::new(&session_dir).exists() {
        std::fs::create_dir_all(&session_dir)
            .map_err(|error| node_fs_error(error, "mkdir", &session_dir))?;
    }
    Ok(session_dir)
}

/// `sessionDir ? normalizePath(sessionDir) : getDefaultSessionDir(cwd)`。
fn resolve_session_dir(
    cwd: &str,
    session_dir: Option<&str>,
) -> Result<String, SessionManagerError> {
    match session_dir {
        Some(dir) if !dir.is_empty() => {
            Ok(kiana_core::paths::normalize_path(dir, &Default::default()))
        }
        _ => get_default_session_dir(cwd, None),
    }
}

/// `findMostRecentSession(sessionDir, cwd?)`。
///
/// 按文件 mtime 降序（`NaN`/相同值视为相等，稳定排序）扫描 `.jsonl`，返回第一条 header
/// 有效且（可选的）cwd 匹配的文件路径；header 读取失败或超过扫描上限的文件被跳过。
/// 目录不可读、任一条目读取失败或 stat 失败都会直接返回 `None`（TS 把整段逻辑包在
/// try/catch 里，一次失败就放弃整个查找），不会跳过该文件继续找。
///
/// TS: `session-manager.ts#findMostRecentSession`
pub fn find_most_recent_session(session_dir: &str, cwd: Option<&str>) -> Option<String> {
    let resolved_session_dir = kiana_core::paths::normalize_path(session_dir, &Default::default());
    // TS: `const resolvedCwd = cwd ? resolvePath(cwd) : undefined`（空串是假值，不过滤）。
    let resolved_cwd = cwd.filter(|cwd| !cwd.is_empty()).map(resolve_path_default);
    let entries = std::fs::read_dir(&resolved_session_dir).ok()?;
    let mut files: Vec<(String, f64)> = Vec::new();
    for entry in entries {
        // 与 TS 的 try/catch 一致：任意条目/stat 失败都会让整个查找返回 None。
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") {
            continue;
        }
        let path = path_join(&[&resolved_session_dir, &name]);
        let metadata = std::fs::metadata(&path).ok()?;
        files.push((path, mtime_ms(&metadata)));
    }
    // 降序；mtime 相同或为 NaN 时返回 Equal，保留 readdir 的原始相对顺序（稳定排序）。
    files.sort_by(|a, b| {
        let delta = b.1 - a.1;
        if delta.is_nan() || delta == 0.0 {
            std::cmp::Ordering::Equal
        } else if delta < 0.0 {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    for (path, _) in files {
        let Some(header) = read_session_header_for_discovery(&path) else {
            continue;
        };
        if let Some(resolved_cwd) = &resolved_cwd
            && !session_cwd_matches(header.cwd_string(), resolved_cwd)
        {
            continue;
        }
        return Some(path);
    }
    None
}

/// `Stats.mtimeMs`（浮点毫秒）。
fn mtime_ms(metadata: &std::fs::Metadata) -> f64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|delta| delta.as_secs_f64() * 1000.0)
        .unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------
// 投影
// ---------------------------------------------------------------------------

/// `SessionTreeNode`。
///
/// TS: `session-manager.ts#SessionTreeNode`
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeNode {
    /// 条目。
    pub entry: SessionEntry,
    /// 子节点。
    pub children: Vec<SessionTreeNode>,
    /// 解析后的标签。
    pub label: Option<String>,
    /// 最近一次标签变更的时间戳。
    ///
    /// 来自 label 条目的原始 `timestamp` 值并按 JS `String(value)` 转换（`null` → `"null"`、
    /// 数字按 JS 数字格式）；来源条目没有 `timestamp` 时为 `None`。
    pub label_timestamp: Option<String>,
}

impl Drop for SessionTreeNode {
    fn drop(&mut self) {
        // 深树不能用递归 Drop（显式栈，与 getTree 的构建方式对应）。
        let mut stack: Vec<SessionTreeNode> = std::mem::take(&mut self.children);
        while let Some(mut node) = stack.pop() {
            stack.extend(std::mem::take(&mut node.children));
        }
    }
}

/// 投影来源条目与它贡献的模型消息。
///
/// TS: `session-manager.ts#ProjectedSessionEntry`
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedSessionEntry {
    /// 拥有该投影贡献的原始条目。
    pub source_entry: SessionEntry,
    /// 编辑后的模型可见消息。
    pub messages: Vec<AgentMessage>,
}

/// `{ provider, modelId }`。
///
/// TS: `session-manager.ts#SessionContext.model`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// provider id。
    pub provider: String,
    /// model id。
    #[serde(rename = "modelId")]
    pub model_id: String,
}

/// 会话投影。
///
/// TS: `session-manager.ts#SessionProjection`
#[derive(Debug, Clone, PartialEq)]
pub struct SessionProjection {
    /// 逐来源条目的投影。
    pub entries: Vec<ProjectedSessionEntry>,
    /// 展平后的模型消息。
    pub messages: Vec<AgentMessage>,
    /// 生效的 thinking level。
    pub thinking_level: String,
    /// 生效的模型。
    pub model: Option<ModelRef>,
}

/// 会话上下文（投影的消息 + 设置）。
///
/// TS: `session-manager.ts#SessionContext`
#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    /// 模型消息。
    pub messages: Vec<AgentMessage>,
    /// 生效的 thinking level。
    pub thinking_level: String,
    /// 生效的模型。
    pub model: Option<ModelRef>,
}

/// `leafId` 的三种状态：省略（undefined，回退到最后一个条目）、`null`（空路径）、具体 id。
///
/// TS: `session-manager.ts#buildSessionPath` 的 `leafId?: string | null`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LeafId<'a> {
    /// `leafId === undefined`：使用最后一个条目。
    #[default]
    Unspecified,
    /// `leafId === null`：路径为空。
    Null,
    /// 具体条目 id（找不到时回退到最后一个条目）。
    Id(&'a str),
}

impl<'a> From<&'a str> for LeafId<'a> {
    fn from(id: &'a str) -> Self {
        LeafId::Id(id)
    }
}

/// `buildEntryIndex(entries, byId?)`。
///
/// 本函数实际对应 TS 的 `buildSessionPath`；开头引用的名字是它复用 `by_id` 的那一步。
/// 从 leaf 回溯到 root，返回 root→leaf 顺序的路径。`leaf_id` 语义：`Null`
/// 直接返回空路径；具体 id 找不到、或 id 为空串时回退到 `entries` 的最后一条；
/// `Unspecified` 同样回退到最后一条。`by_id` 传入时直接复用（同 id 后者覆盖），否则现场
/// 建索引。父链遇到缺失/空串即停止，所以孤立条目会得到一段短路径而不是错误。
fn build_session_path<'a>(
    entries: &'a [SessionEntry],
    leaf_id: LeafId<'_>,
    by_id: Option<&'a IndexMap<String, SessionEntry>>,
) -> Vec<SessionEntry> {
    let owned_index;
    let index: &IndexMap<String, SessionEntry> = match by_id {
        Some(index) => index,
        None => {
            owned_index = {
                let mut map = IndexMap::with_capacity(entries.len());
                for entry in entries {
                    map.insert(entry.id().to_string(), entry.clone());
                }
                map
            };
            &owned_index
        }
    };

    let leaf: Option<&SessionEntry> = match leaf_id {
        LeafId::Null => return Vec::new(),
        // TS 的 `if (leafId)`：空串也是假值 → 回退到最后一个条目。
        LeafId::Id("") => entries.last(),
        LeafId::Id(id) => index.get(id).or_else(|| entries.last()),
        LeafId::Unspecified => entries.last(),
    };
    let Some(mut current) = leaf else {
        return Vec::new();
    };

    let mut path: Vec<SessionEntry> = Vec::new();
    loop {
        path.push(current.clone());
        current = match current.parent_id() {
            Some(parent_id) if !parent_id.is_empty() => match index.get(parent_id) {
                Some(parent) => parent,
                None => break,
            },
            _ => break,
        };
    }
    path.reverse();
    path
}

/// `getSessionContextSettings(path)`。
///
/// 顺序扫描整条路径，后出现的设置覆盖先前的：`thinking_level_change` 改 thinking level，
/// `model_change` 与 assistant 消息（含宽松解析得到的 `Custom`）改 model。路径为空时返回
/// `("off", None)`。
///
/// TS 直接读取 `entry.message.role` 与 `.provider`/`.model`；宽松解析成
/// `AgentMessage::Custom` 的 assistant 消息同样参与模型推导。
fn get_session_context_settings(path: &[SessionEntry]) -> (String, Option<ModelRef>) {
    let mut thinking_level = "off".to_string();
    let mut model: Option<ModelRef> = None;
    for entry in path {
        match entry {
            SessionEntry::ThinkingLevelChange(entry) => {
                thinking_level = entry.thinking_level.clone();
            }
            SessionEntry::ModelChange(entry) => {
                model = Some(ModelRef {
                    provider: entry.provider.clone(),
                    model_id: entry.model_id.clone(),
                });
            }
            SessionEntry::Message(entry) => {
                let Some((provider, model_id)) = assistant_model_ref(&entry.message) else {
                    continue;
                };
                model = Some(ModelRef { provider, model_id });
            }
            _ => {}
        }
    }
    (thinking_level, model)
}

/// 消息是 assistant 时返回 `{provider, model}`（TS 的 `entry.message.role === "assistant"`
/// 分支）；否则 `None`。`Custom` 消息缺 `provider`/`model` 时得到空串（对应 TS 读取
/// `undefined` 属性后参与模板/对象构造），不会返回 `None`。
fn assistant_model_ref(message: &AgentMessage) -> Option<(String, String)> {
    if message.role() != "assistant" {
        return None;
    }
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            Some((assistant.provider.clone(), assistant.model.clone()))
        }
        AgentMessage::Custom(object) => Some((
            json_field_string(object, "provider"),
            json_field_string(object, "model"),
        )),
        _ => None,
    }
}

/// 从原始对象读取字段并按 JS 语义转成字符串（缺省 → 空串）。
pub(crate) fn json_field_string(object: &JsonObject, key: &str) -> String {
    match object.get(key) {
        Some(JsonValue::String(text)) => text.clone(),
        Some(value) => js_to_string(Some(value)),
        None => String::new(),
    }
}

/// 把宽松解析得到的 `Custom` 消息的 `content: null` 归一化。
///
/// system → `""`，user/assistant/toolResult → `[]`；其它角色或 content 非 null 时原样
/// 克隆。对应 TS `sessionEntryToContextMessages` 的 `{ ...message, content }` 分支。
fn normalize_message_content(message: &AgentMessage) -> AgentMessage {
    let AgentMessage::Custom(object) = message else {
        return message.clone();
    };
    let role = object
        .get("role")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let content_is_null = match object.get("content") {
        None => true,
        Some(value) => value.is_null(),
    };
    if !content_is_null {
        return message.clone();
    }
    let replacement = match role {
        "system" => JsonValue::String(String::new()),
        "user" | "assistant" | "toolResult" => JsonValue::Array(Vec::new()),
        _ => return message.clone(),
    };
    let mut object = object.clone();
    object.insert("content".to_string(), replacement);
    AgentMessage::Custom(object)
}

/// 标准消息对象的已知键（kiana-ai 的 typed 形状，camelCase）。
fn known_message_keys(role: &str) -> &'static [&'static str] {
    match role {
        "system" => &[
            "role",
            "content",
            "sections",
            "toolsAdded",
            "toolsRemoved",
            "timestamp",
        ],
        "user" => &["role", "content", "timestamp"],
        "assistant" => &[
            "role",
            "content",
            "api",
            "provider",
            "model",
            "responseModel",
            "responseId",
            "providerThinkingLevel",
            "thinkingLevel",
            "diagnostics",
            "usage",
            "stopReason",
            "deferred",
            "errorMessage",
            "rawStopReason",
            "endTurn",
            "timestamp",
            "durationMs",
        ],
        "toolResult" => &[
            "role",
            "content",
            "toolCallId",
            "toolName",
            "details",
            "usage",
            "nestedCalls",
            "isError",
            "timestamp",
            "durationMs",
        ],
        _ => &[],
    }
}

/// 原始消息对象是否带有 typed 视图之外的键（TS 会原样保留它们）。
fn message_has_extra_keys(raw: &JsonObject, role: &str) -> bool {
    let known = known_message_keys(role);
    !known.is_empty() && raw.keys().any(|key| !known.contains(&key.as_str()))
}

/// 回放原始消息对象（`{...message, content}` 的 `content: null` 归一化）。
fn normalized_raw_message(raw: &JsonObject) -> AgentMessage {
    let role = raw
        .get("role")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let content_is_null = raw.get("content").is_none_or(JsonValue::is_null);
    let mut object = raw.clone();
    if content_is_null {
        let replacement = match role {
            "system" => JsonValue::String(String::new()),
            "user" | "assistant" | "toolResult" => JsonValue::Array(Vec::new()),
            _ => return AgentMessage::Custom(Box::new(object)),
        };
        object.insert("content".to_string(), replacement);
    }
    AgentMessage::Custom(Box::new(object))
}

/// 投影一个条目贡献的模型消息（不含 context_edit）。
///
/// 与 TS 一样，`content: null` 的 system/user/assistant/toolResult 消息会被归一化
/// （system → `""`，其余 → `[]`），`custom_message` 的 `content ?? []`、`branch_summary`
/// 的空摘要、`compaction.systemMessage` 都按 TS 处理。
///
/// 从文件加载的 `custom_message` / `compaction` 按原始对象重建消息（`customType` /
/// `display` / `details` / `systemMessage` 的未校验类型原样保留，缺省键被省略，额外键与
/// 键序保留）；通过 append API 新建的条目没有原始对象，使用 typed 视图。
///
/// 表示能力边界（消息条目由 `AgentMessage` 承载，§4.9 冻结为
/// `Standard(Message) | Custom(JsonObject)`）：
/// - `message` 为 `null`/缺失时 TS 读取 `.role` 会抛 TypeError；Rust 在解析阶段把该条目
///   归入 [`SessionEntry::Unknown`]（不进入上下文），因为 `AgentMessage` 无法表示这两个值，
///   且本函数没有错误通道；
/// - `message` 为字符串/数字/数组等非对象时 TS 原样返回它，`compaction.systemMessage`
///   为字符串等真值非对象时同样原样返回；Rust 无法表示非对象 JSON 值，这类值不进入上下文
///   （TS 的 `convertToLlm` 同样会丢弃它们，模型可见上下文一致）。
///
/// 从文件加载的标准消息若带有 typed 视图之外的额外键，会以
/// [`AgentMessage::Custom`] 回放原始对象（键序与额外键原样保留）；通过 append API 新建的
/// 消息没有原始对象，序列化后的 typed 视图即其完整形状。
///
/// TS: `session-manager.ts#sessionEntryToContextMessages`
pub fn session_entry_to_context_messages(entry: &SessionEntry) -> Vec<AgentMessage> {
    match entry {
        SessionEntry::Message(entry) => {
            // TS 原样返回 `entry.message`：从文件加载且原始对象带有 typed 视图之外的键时，
            // 回放原始对象（含额外键与键序）；否则用 typed 视图（标准消息）或已存在的
            // `AgentMessage::Custom`（宽松消息）。
            match &entry.raw_message {
                Some(raw)
                    if matches!(&entry.message, AgentMessage::Standard(_))
                        && message_has_extra_keys(raw, entry.message.role()) =>
                {
                    vec![normalized_raw_message(raw)]
                }
                _ => vec![normalize_message_content(&entry.message)],
            }
        }
        SessionEntry::CustomMessage(entry) => match &entry.raw_entry {
            // TS 用原始字段重新构造消息（未校验的类型原样保留、缺省键被省略）。
            Some(raw) => vec![custom_message_from_raw(raw)],
            None => {
                let content = if entry.content.is_null() {
                    JsonValue::Array(Vec::new())
                } else {
                    entry.content.clone()
                };
                let message = create_custom_message(
                    &entry.custom_type,
                    content,
                    entry.display,
                    entry.details.clone(),
                    &entry.timestamp,
                );
                vec![message.to_agent_message()]
            }
        },
        SessionEntry::BranchSummary(entry) => match &entry.raw_entry {
            Some(raw) => branch_summary_message_from_raw(raw).into_iter().collect(),
            None => {
                if entry.summary.is_empty() {
                    vec![]
                } else {
                    let message = create_branch_summary_message(
                        &entry.summary,
                        &entry.from_id,
                        &entry.timestamp,
                    );
                    vec![message.to_agent_message()]
                }
            }
        },
        SessionEntry::Compaction(entry) => {
            let summary = match &entry.raw_entry {
                Some(raw) => compaction_summary_message_from_raw(raw),
                None => create_compaction_summary_message(
                    &entry.summary,
                    entry.tokens_before,
                    &entry.timestamp,
                )
                .to_agent_message(),
            };
            // TS 返回 `entry.systemMessage` 原始对象（额外键与键序保留）；从文件加载时
            // 一律走原始对象，append API 新建的条目才用 typed `SystemMessage`。
            match &entry.raw_entry {
                Some(raw) => match compaction_system_message_from_raw(raw) {
                    Some(system_message) => vec![system_message, summary],
                    None => vec![summary],
                },
                None => match &entry.system_message {
                    Some(system_message) => vec![
                        AgentMessage::Standard(Message::System(system_message.clone())),
                        summary,
                    ],
                    None => vec![summary],
                },
            }
        }
        // 无法解析成 typed 视图的已知类型条目：TS 直接读原始对象。
        SessionEntry::Unknown(object) => match entry.kind() {
            "branch_summary" => branch_summary_message_from_raw(object)
                .into_iter()
                .collect(),
            "custom_message" => vec![custom_message_from_raw(object)],
            "compaction" => {
                let summary = compaction_summary_message_from_raw(object);
                match compaction_system_message_from_raw(object) {
                    Some(system_message) => vec![system_message, summary],
                    None => vec![summary],
                }
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// `createBranchSummaryMessage(summary, fromId, timestamp)` 的原始值版本。
///
/// TS 直接使用 `entry.summary` 等原始字段：`summary` 为假值时不生成消息；`fromId` 缺省
/// （`undefined`）时 `JSON.stringify` 省略该键；`timestamp` 按 `new Date(value).getTime()`
/// 计算（无效为 `NaN`，序列化为 `null`）。返回 `None` 表示 `entry.summary` 为假值。
fn branch_summary_message_from_raw(object: &JsonObject) -> Option<AgentMessage> {
    let summary = object.get("summary")?;
    if !js_truthy(summary) {
        return None;
    }
    let mut message = JsonObject::new();
    message.insert(
        "role".to_string(),
        JsonValue::String("branchSummary".to_string()),
    );
    message.insert("summary".to_string(), summary.clone());
    if let Some(from_id) = object.get("fromId") {
        message.insert("fromId".to_string(), from_id.clone());
    }
    message.insert(
        "timestamp".to_string(),
        json_number_or_null(js_date_constructor_time(object.get("timestamp"))),
    );
    Some(AgentMessage::Custom(Box::new(message)))
}

/// `createCompactionSummaryMessage(summary, tokensBefore, timestamp)` 的原始值版本。
///
/// `summary` / `tokensBefore` 缺省时键被 `JSON.stringify` 省略（TS 里值是 `undefined`）。
fn compaction_summary_message_from_raw(object: &JsonObject) -> AgentMessage {
    let mut message = JsonObject::new();
    message.insert(
        "role".to_string(),
        JsonValue::String("compactionSummary".to_string()),
    );
    if let Some(summary) = object.get("summary") {
        message.insert("summary".to_string(), summary.clone());
    }
    if let Some(tokens_before) = object.get("tokensBefore") {
        message.insert("tokensBefore".to_string(), tokens_before.clone());
    }
    message.insert(
        "timestamp".to_string(),
        json_number_or_null(js_date_constructor_time(object.get("timestamp"))),
    );
    AgentMessage::Custom(Box::new(message))
}

/// `createCustomMessage(customType, content ?? [], display, details, timestamp)` 的原始值版本。
///
/// TS 用条目上的原始字段重新构造对象：`customType` / `display` / `details` 不校验类型，
/// 缺省（`undefined`）时 `JSON.stringify` 省略对应键；`content` 为 `null`/缺省时变成 `[]`；
/// `timestamp` 按 `new Date(value).getTime()` 计算。
fn custom_message_from_raw(object: &JsonObject) -> AgentMessage {
    let mut message = JsonObject::new();
    message.insert("role".to_string(), JsonValue::String("custom".to_string()));
    if let Some(custom_type) = object.get("customType") {
        message.insert("customType".to_string(), custom_type.clone());
    }
    let content = match object.get("content") {
        None | Some(JsonValue::Null) => JsonValue::Array(Vec::new()),
        Some(content) => content.clone(),
    };
    message.insert("content".to_string(), content);
    if let Some(display) = object.get("display") {
        message.insert("display".to_string(), display.clone());
    }
    if let Some(details) = object.get("details") {
        message.insert("details".to_string(), details.clone());
    }
    message.insert(
        "timestamp".to_string(),
        json_number_or_null(js_date_constructor_time(object.get("timestamp"))),
    );
    AgentMessage::Custom(Box::new(message))
}

/// `entry.systemMessage ? [entry.systemMessage, summary] : [summary]` 的原始值版本。
///
/// 真值判断沿用 JSON/JS 语义（`null`/`false`/`0`/`""` 为假，其余为真）；对象原样返回（额外键与键序保留）。
/// 非对象真值（字符串/数字/数组）TS 会原样返回，但 [`AgentMessage`] 的冻结契约只能承载
/// 对象，因此这类值返回 `None`（表示能力边界，见 [`session_entry_to_context_messages`]）。
fn compaction_system_message_from_raw(object: &JsonObject) -> Option<AgentMessage> {
    match object.get("systemMessage") {
        Some(JsonValue::Object(system_message)) => {
            let value = JsonValue::Object(system_message.clone());
            if !js_truthy(&value) {
                return None;
            }
            json_object_from_value(value)
                .map(|system_message| AgentMessage::Custom(Box::new(system_message)))
        }
        _ => None,
    }
}

/// `f64` → JSON 数字；`NaN`/`±Infinity` → `null`（与 `JSON.stringify` 一致）。
fn json_number_or_null(value: f64) -> JsonValue {
    serde_json::Number::from_f64(value)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

/// 应用一次上下文编辑。
///
/// 无 `edit` 时返回条目的常规投影；`edit.replacement == None` 表示把目标从模型上下文
/// 移除，返回空 `Vec`；否则对每条消息替换 `content`（见 `apply_context_replacement`）。
/// 注意 typed 视图无法区分 `replacement: null` 与缺少该键，两者都按“移除”处理。
fn project_context_entry(
    entry: &SessionEntry,
    edit: Option<&ContextEditEntry>,
) -> Vec<AgentMessage> {
    let messages = session_entry_to_context_messages(entry);
    let Some(edit) = edit else {
        return messages;
    };
    let Some(replacement) = &edit.replacement else {
        return Vec::new();
    };
    messages
        .into_iter()
        .map(|message| apply_context_replacement(&message, &replacement.content))
        .collect()
}

/// `{ ...message, content }`。
///
/// 只对 user/assistant/toolResult/custom 角色生效，system 及其它角色原样返回。assistant/
/// toolResult 的字符串替换先包成单个 `{type:"text",text:...}` 块；其余内容原样写入。
/// typed 消息（`Standard`）若无法把替换内容反序列化成对应 content 类型，会退化为
/// `Custom` 消息（保留其余字段 + 原始 content）。
fn apply_context_replacement(message: &AgentMessage, replacement: &JsonValue) -> AgentMessage {
    match message {
        AgentMessage::Custom(object) => {
            let role = object
                .get("role")
                .and_then(JsonValue::as_str)
                .unwrap_or_default();
            if !matches!(role, "user" | "assistant" | "toolResult" | "custom") {
                return message.clone();
            }
            // TS：assistant/toolResult 的字符串替换先转成单个 text 块；其余原样。
            let content =
                if (role == "assistant" || role == "toolResult") && replacement.is_string() {
                    text_block_array(replacement.as_str().unwrap_or_default())
                } else {
                    replacement.clone()
                };
            let mut object = object.clone();
            object.insert("content".to_string(), content);
            AgentMessage::Custom(object)
        }
        AgentMessage::Standard(message) => match message {
            Message::User(user) => {
                let Ok(content) =
                    serde_json::from_value::<kiana_ai::types::UserContent>(replacement.clone())
                else {
                    return standard_with_raw_content(message, replacement.clone());
                };
                AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
                    role: user.role,
                    content,
                    timestamp: user.timestamp,
                }))
            }
            Message::Assistant(assistant) => {
                let value = assistant_content_blocks(replacement);
                let Ok(content) = serde_json::from_value::<
                    Vec<kiana_ai::types::AssistantContentBlock>,
                >(value.clone()) else {
                    return standard_with_raw_content(message, value);
                };
                let mut assistant = assistant.clone();
                assistant.content = content;
                AgentMessage::Standard(Message::Assistant(assistant))
            }
            Message::ToolResult(tool_result) => {
                let value = tool_result_content_blocks(replacement);
                let Ok(content) =
                    serde_json::from_value::<Vec<kiana_ai::types::UserContentBlock>>(value.clone())
                else {
                    return standard_with_raw_content(message, value);
                };
                let mut tool_result = tool_result.clone();
                tool_result.content = content;
                AgentMessage::Standard(Message::ToolResult(tool_result))
            }
            // system 不在可编辑集合内（TS 的 map 原样返回）。
            Message::System(_) => AgentMessage::Standard(message.clone()),
        },
    }
}

/// 无法用 typed 内容表示的替换：保留消息的其余字段，把 `content` 换成原始值
/// （对应 TS 的 `{ ...message, content }`；Rust 用 `AgentMessage::Custom` 承载）。
fn standard_with_raw_content(message: &Message, content: JsonValue) -> AgentMessage {
    let mut value = serde_json::to_value(message).unwrap_or(JsonValue::Null);
    if let JsonValue::Object(object) = &mut value {
        object.insert("content".to_string(), content);
    }
    match json_object_from_value(value) {
        Some(object) => AgentMessage::Custom(Box::new(object)),
        None => AgentMessage::Custom(Box::default()),
    }
}

/// assistant/toolResult 的字符串替换先转成单个 text 块。
fn assistant_content_blocks(replacement: &JsonValue) -> JsonValue {
    match replacement {
        JsonValue::String(text) => text_block_array(text),
        other => other.clone(),
    }
}

fn tool_result_content_blocks(replacement: &JsonValue) -> JsonValue {
    match replacement {
        JsonValue::String(text) => text_block_array(text),
        other => other.clone(),
    }
}

fn text_block_array(text: &str) -> JsonValue {
    JsonValue::Array(vec![serde_json::json!({
        "type": "text",
        "text": text,
    })])
}

/// `entry.type === "compaction"` 的 `firstKeptEntryId`。
///
/// TS 直接读原始对象；typed 视图无法解析的 compaction（[`SessionEntry::Unknown`]）同样按
/// 原始字段参与上下文裁剪。非字符串值不会等于任何字符串条目 id，返回 `None`。
fn entry_first_kept_entry_id(entry: &SessionEntry) -> Option<String> {
    match entry {
        // TS 用严格相等比较原始值：非字符串的 `firstKeptEntryId` 不可能等于字符串条目 id。
        SessionEntry::Compaction(compaction) => match &compaction.raw_entry {
            Some(raw) => raw
                .get("firstKeptEntryId")
                .and_then(JsonValue::as_str)
                .map(str::to_string),
            None => Some(compaction.first_kept_entry_id.clone()),
        },
        SessionEntry::Unknown(object) if entry.kind() == "compaction" => object
            .get("firstKeptEntryId")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// `buildContextEntries(entries, leafId?, byId?)`。
///
/// 无 compaction 时返回完整路径。有 compaction 时：
/// 1. 取路径上**最后一条** compaction 作为结果第一项（它是唯一被当作压缩检查点的一项；
///    更早的 compaction 若落在保留区内仍会作为普通条目保留）；
/// 2. 从它的 `firstKeptEntryId` 起、到该 compaction 之前，按原顺序保留条目，并跳过
///    system 消息（含宽松解析成 `AgentMessage::Custom` 的 system 消息）；
/// 3. 追加该 compaction 之后的全部条目。
///
/// 若 `firstKeptEntryId` 缺失/非字符串、找不到对应条目，`found_first_kept` 永远为 false，
/// 压缩前的条目会全部丢弃。`by_id` 用于判定“这条 compaction 在路径中的位置”。
///
/// TS: `session-manager.ts#buildContextEntries`
pub fn build_context_entries(
    entries: &[SessionEntry],
    leaf_id: LeafId<'_>,
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> Vec<SessionEntry> {
    let path = build_session_path(entries, leaf_id, by_id);
    let mut compaction: Option<&SessionEntry> = None;
    // 顺序扫描覆盖前值，循环结束时留下路径上最后一条 compaction（最新压缩）。
    for entry in &path {
        // TS 判断 `entry.type === "compaction"`：无法 typed 解析的 compaction 条目同样参与。
        if entry.kind() == "compaction" {
            compaction = Some(entry);
        }
    }
    let Some(compaction) = compaction else {
        return path;
    };
    let Some(compaction_index) = path.iter().position(|entry| entry.id() == compaction.id()) else {
        return path;
    };
    let first_kept_entry_id = entry_first_kept_entry_id(compaction);

    // 结果固定以 compaction 自身开头；随后是保留区与压缩后的全部条目。
    let mut context_entries: Vec<SessionEntry> = vec![compaction.clone()];
    let mut found_first_kept = false;
    for entry in &path[..compaction_index] {
        if Some(entry.id()) == first_kept_entry_id.as_deref() {
            found_first_kept = true;
        }
        // TS 在原始对象上判断 `entry.message.role === "system"`：宽松解析成
        // `AgentMessage::Custom` 的 system 消息（如 `content: null`）同样要跳过。
        let is_system_message = matches!(
            entry,
            SessionEntry::Message(message_entry) if message_entry.message.role() == "system"
        );
        if found_first_kept && !is_system_message {
            context_entries.push(entry.clone());
        }
    }
    context_entries.extend(path[compaction_index + 1..].iter().cloned());
    context_entries
}

/// `buildSessionProjection(entries, leafId?, byId?)`。
///
/// 执行顺序：
/// 1. 完整路径（不裁剪）推导 thinking level 与 model，压缩不应改变当前设置；
/// 2. 压缩感知的 context entries 按出现顺序收集 `context_edit`，同一 `targetId` 后者
///    覆盖前者；
/// 3. 有 compaction 时第 0 项是最新 compaction；无 compaction 时 `build_context_entries`
///    直接返回完整路径，第 0 项就是路径的首个普通条目。`index > 0` 的 compaction 不贡献
///    消息——较早的压缩条目可能因 `firstKeptEntryId` 落在更早区间而残留在保留区，其
///    summary 已被更新的压缩吸收（TS 源码在该分支有同样的说明）；
/// 4. 每个条目经 `project_context_entry` 生成消息，最后按顺序展平。
///
/// `entries` 保留来源条目（投影/调试/渲染用），`messages` 是展平后的最终 LLM 输入。
///
/// TS: `session-manager.ts#buildSessionProjection`
pub fn build_session_projection(
    entries: &[SessionEntry],
    leaf_id: LeafId<'_>,
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> SessionProjection {
    let path = build_session_path(entries, leaf_id, by_id);
    // 设置从完整路径推导：压缩裁剪只影响模型消息，不应改变当前生效的模型/thinking。
    let (thinking_level, model) = get_session_context_settings(&path);
    let context_entries = build_context_entries(entries, leaf_id, by_id);
    let mut edits: IndexMap<String, ContextEditEntry> = IndexMap::new();
    // 同 targetId 后出现的编辑覆盖先前的（对应 TS 的 `Map.set`）。
    for entry in &context_entries {
        if let SessionEntry::ContextEdit(edit) = entry {
            edits.insert(edit.target_id.clone(), edit.clone());
        }
    }
    let projected_entries: Vec<ProjectedSessionEntry> = context_entries
        .iter()
        .enumerate()
        .map(|(index, source_entry)| {
            let messages = if source_entry.kind() == "compaction" && index > 0 {
                Vec::new()
            } else {
                project_context_entry(source_entry, edits.get(source_entry.id()))
            };
            ProjectedSessionEntry {
                source_entry: source_entry.clone(),
                messages,
            }
        })
        .collect();
    let messages = projected_entries
        .iter()
        .flat_map(|entry| entry.messages.iter().cloned())
        .collect();
    SessionProjection {
        entries: projected_entries,
        messages,
        thinking_level,
        model,
    }
}

/// `buildSessionContext(entries, leafId?, byId?)`。
///
/// 等价于调用 [`build_session_projection`] 后只取 `messages`/`thinking_level`/`model`；
/// 需要每条消息的来源条目时用投影结果。
///
/// TS: `session-manager.ts#buildSessionContext`
pub fn build_session_context(
    entries: &[SessionEntry],
    leaf_id: LeafId<'_>,
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> SessionContext {
    let projection = build_session_projection(entries, leaf_id, by_id);
    SessionContext {
        messages: projection.messages,
        thinking_level: projection.thinking_level,
        model: projection.model,
    }
}

/// `getLatestCompactionEntry(entries)`。
///
/// TS 只判断 `entry.type === "compaction"`，不校验字段；[`CompactionEntry`] 的字段反序列化
/// 因此是宽松的（不合法的 `summary`/`tokensBefore`/`usage`/`systemMessage` 等不会让条目
/// 退化为 [`SessionEntry::Unknown`]）。仅 `id`/`parentId`/`timestamp` 这些树结构字段仍是
/// 严格类型，畸形值仍会落到 `Unknown`（公开返回类型无法表示它们）。
///
/// 注意与 [`build_context_entries`] 的差异：后者按 `kind()` 判定，`Unknown` 的 compaction
/// 也参与裁剪；本函数只认能解析成 [`CompactionEntry`] 的条目——遇到畸形 compaction 会跳过
/// 并继续向前找，只有全部 compaction 都无法解析时才返回 `None`（TS 直接返回最近的
/// `type === "compaction"` 原始条目，这是 Rust 的能力边界差异）。
///
/// TS: `session-manager.ts#getLatestCompactionEntry`
pub fn get_latest_compaction_entry(entries: &[SessionEntry]) -> Option<&CompactionEntry> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Compaction(compaction) = entry {
            return Some(compaction);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 会话信息与发现
// ---------------------------------------------------------------------------

/// 会话列表项。
///
/// `created` / `modified` 是 JS `Date` 的 `getTime()` 语义（Unix 毫秒 `f64`；无效日期为
/// `NaN`）——TS 的 `SessionInfo` 里是 `Date` 对象，Rust 没有跨 crate 冻结的形状。
///
/// `modified` 的优先级：最新一条 user/assistant 消息的活动时间（必须 > 0）→ header 的
/// `timestamp`（必须能解析）→ 文件 mtime。`message_count` 统计所有 `type == "message"`
/// 的条目（含 system/toolResult，以及 `message` 为其它非对象值——字符串/数字/布尔/数组——
/// 的畸形条目）；`message` 缺失或为 `null` 会让整个会话不可读，`build_session_info` 随之
/// 返回 `Ok(None)`，因此不会产生带 `message_count` 的 `SessionInfo`。`first_message` /
/// `all_messages_text` 只统计能提取出非空文本的 user/assistant 消息。
///
/// TS: `session-manager.ts#SessionInfo`
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    /// 会话文件路径。
    pub path: String,
    /// 会话 id。
    pub id: String,
    /// 会话启动时的 cwd（旧会话为空串）。
    pub cwd: String,
    /// 用户设置的显示名。
    pub name: Option<String>,
    /// fork 来源路径。
    pub parent_session_path: Option<String>,
    /// `new Date(header.timestamp)` 的毫秒值（可能 `NaN`）。
    pub created: f64,
    /// 最后活动时间（毫秒；可能 `NaN`）。
    pub modified: f64,
    /// 消息条目数（含 system/toolResult）。
    pub message_count: usize,
    /// 第一条非空 user 文本（无则 `"(no messages)"`）。
    pub first_message: String,
    /// 所有 user+assistant 文本以单个空格连接。
    pub all_messages_text: String,
}

/// 进度回调：`(loaded, total, partialSessions?)`。
///
/// 类型要求 `Send + Sync`：多个并发 worker 会同时调用它，实现必须自行处理并发（例如
/// 内部加锁）。回调在 worker 任务中**同步**执行（执行期间阻塞该 worker），因此实现应保持
/// 轻量。`partialSessions` 是已加载会话的快照（按 `modified` 降序），只在发布点给出
/// `Some`。
///
/// 取消不是即时的：已开始的文件解析会在下一个检查点返回错误，中止后仍可能有回调发生。
///
/// TS: `session-manager.ts#SessionListProgress`
pub type SessionListProgress = Arc<dyn Fn(u64, u64, Option<Vec<SessionInfo>>) + Send + Sync>;

/// 会话文件候选（可选 stat 结果）。
#[derive(Debug, Clone)]
struct SessionFileCandidate {
    path: String,
    mtime: Option<f64>,
}

/// `sortSessionInfos(sessions)`：`modified` 降序（NaN 视为相等，稳定排序）。
///
/// 使用稳定排序：`NaN` 或相同的 `modified` 保留输入顺序（通常来自文件名的 locale 排序），
/// 因此同一目录的列表输出是可复现的。
fn sort_session_infos(mut sessions: Vec<SessionInfo>) -> Vec<SessionInfo> {
    sessions.sort_by(|a, b| {
        let delta = b.modified - a.modified;
        if delta.is_nan() || delta == 0.0 {
            std::cmp::Ordering::Equal
        } else if delta < 0.0 {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    sessions
}

/// `getMessageActivityTime(entry)`（原始 JSON 版本）。
///
/// 只在 `message` 是对象、含 `content` 键且 `role` 是 user/assistant 时返回时间：
/// `message.timestamp` 是数字时直接用它（结束时间），否则把 `entry.timestamp` 按
/// `new Date(...).getTime()` 解析；都无效时返回 `None`。
fn message_activity_time(entry: &serde_json::Map<String, JsonValue>) -> Option<f64> {
    let message = entry.get("message")?.as_object()?;
    // TS `isMessageWithContent`：`typeof message.role === "string" && "content" in message`。
    if !message.contains_key("content") {
        return None;
    }
    let role = message.get("role")?.as_str()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    if let Some(timestamp) = message.get("timestamp").and_then(JsonValue::as_f64) {
        return Some(timestamp);
    }
    // `new Date(entry.timestamp).getTime()`：非字符串也按 Date 构造器强制转换。
    let parsed = js_date_constructor_time(entry.get("timestamp"));
    if parsed.is_nan() { None } else { Some(parsed) }
}

/// `extractTextContent(message)`（原始 JSON 版本）。
///
/// 与 TS 一样不做类型校验：
/// - `content` 是字符串 → 原样；
/// - 是数组 → 过滤 `block.type === "text"`（访问 `null` 块的 `.type` 会抛 TypeError），
///   再按 `.map(block => block.text).join(" ")` 拼接（`undefined`/`null` 元素渲染为空串，
///   其它值按 JS `ToString`）；
/// - 其它（`null`/对象/数字/布尔…）→ `content.filter is not a function`，同样抛错。
///
/// `Err(())` 表示 TS 会抛错（整个会话不可读）；`Ok(None)` 表示没有 `content` 键
/// （调用方在 `isMessageWithContent` 处已跳过）。
fn extract_text_content(
    message: &serde_json::Map<String, JsonValue>,
) -> Result<Option<String>, ()> {
    let Some(content) = message.get("content") else {
        return Ok(None);
    };
    if let Some(text) = content.as_str() {
        return Ok(Some(text.to_string()));
    }
    let Some(blocks) = content.as_array() else {
        return Err(());
    };
    let mut parts: Vec<String> = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            // `block.type` on null/undefined 抛 TypeError（undefined 不会出现在 JSON 数组里）。
            JsonValue::Null => return Err(()),
            JsonValue::Object(object)
                if object.get("type").and_then(JsonValue::as_str) == Some("text") =>
            {
                // `Array.prototype.join`：undefined/null 元素写成空串，其余按 `String(value)`。
                parts.push(match object.get("text") {
                    None | Some(JsonValue::Null) => String::new(),
                    Some(value) => js_to_string(Some(value)),
                });
            }
            // 数字/布尔/字符串/数组/非 text 对象的 `.type` 不是 "text"，不过滤也不抛错。
            _ => {}
        }
    }
    Ok(Some(parts.join(" ")))
}

/// `buildSessionInfo(filePath, signal?, fileStats?)`。
///
/// IO/解析错误返回 `Ok(None)`（对应 TS 的 catch：一个损坏文件不影响整个列表）；取消返回
/// `Err`（对应 TS 的 `throwIfAborted`），错误名恒为 `AbortError`（除非 abort reason 自带
/// name/message）。`mtime` 由调用方传入时可省掉一次 `metadata`。
async fn build_session_info(
    file_path: &str,
    signal: Option<&AbortScope>,
    mtime: Option<f64>,
) -> Result<Option<SessionInfo>, SessionListError> {
    match build_session_info_inner(file_path, signal, mtime) {
        Ok(info) => Ok(info),
        Err(_) => {
            throw_if_aborted(signal)?;
            Ok(None)
        }
    }
}

/// `buildSessionInfo` 的实现（不吞错误，供列表与 `build_session_info` 共用）。
///
/// 扫描顺序固定：首个真值行必须是 `{type:"session"}` 对象，否则整个文件返回
/// `Ok(None)`；之后逐行处理：
/// - `session_info` 的 `name` 每次经过 JS trim，后出现的覆盖先前的（空串表示清空标题）；
/// - 每条 `message` 都计入 `message_count`（即使 `message` 不是对象），但只有
///   user/assistant 且能提取出非空文本的才进入 `all_messages_text`；
/// - `last_activity_time` 取各条消息活动时间的最大值（只有 > 0 才用于 `modified`）；
/// - 非法 `name` 类型、`message` 为 null/缺失、`content` 为 null/对象等会让 TS 抛
///   TypeError 的输入返回 `Err`，最终由 `build_session_info` 变成 `Ok(None)`。
fn build_session_info_inner(
    file_path: &str,
    signal: Option<&AbortScope>,
    mtime: Option<f64>,
) -> Result<Option<SessionInfo>, SessionManagerError> {
    let stats_mtime = match mtime {
        Some(mtime) => mtime,
        None => mtime_ms(&std::fs::metadata(file_path)?),
    };
    let file = std::fs::File::open(file_path)?;
    let mut reader = LineReader::new(BufReader::with_capacity(SESSION_READ_BUFFER_SIZE, file));

    let mut header: Option<serde_json::Map<String, JsonValue>> = None;
    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity_time: Option<f64> = None;

    loop {
        if signal.is_some_and(AbortScope::is_aborted) {
            return Err(SessionManagerError::Io(std::io::Error::other("aborted")));
        }
        let Some(line) = reader.next_line()? else {
            break;
        };
        let text = String::from_utf8_lossy(&line);
        // 注意：这里用的是 serde_json（而不是文件加载路径的 `kiana_json::js_parse`）。
        // V8 能接受但 serde_json 拒绝的行（例如孤立代理项转义、超出 f64 范围的数字）会被
        // 当作畸形行跳过，只影响本函数的统计字段，不影响 `load_stored_entries_from_file`。
        let Ok(value) = serde_json::from_str::<JsonValue>(&text) else {
            continue;
        };
        // TS: `if (!entry) continue;`（null/false/0/"" 视为畸形行）。
        if !js_truthy(&value) {
            continue;
        }
        let entry = value.as_object();
        if header.is_none() {
            // TS 对数组/字符串等取 `.type`（undefined）→ 不是 header → 整个文件无效；
            // 非对象行不能被跳过继续找 header。
            let is_header = entry.is_some_and(|entry| {
                entry.get("type").and_then(JsonValue::as_str) == Some("session")
            });
            if !is_header {
                return Ok(None);
            }
            header = entry.cloned();
            continue;
        }
        let Some(entry) = entry else {
            continue;
        };
        if entry.get("type").and_then(JsonValue::as_str) == Some("session_info") {
            // TS: `entry.name?.trim() || undefined`；非字符串 name 会抛 TypeError，
            // 外层 catch 把整个会话视为不可读（返回 None）。
            name = match entry.get("name") {
                None | Some(JsonValue::Null) => None,
                Some(JsonValue::String(text)) => {
                    let trimmed = js_trim(text);
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                }
                Some(_) => {
                    return Err(SessionManagerError::Io(std::io::Error::other(
                        "entry.name.trim is not a function",
                    )));
                }
            };
        }
        if entry.get("type").and_then(JsonValue::as_str) != Some("message") {
            continue;
        }
        // TS 也在此处先 +1：message 字段畸形不影响计数，后续才决定能否提取文本。
        message_count += 1;
        // TS `isMessageWithContent(entry.message)` 在 message 缺失/null 时抛 TypeError
        // （整个会话视为不可读）；其它非对象值按「没有 content」处理。
        if entry.get("message").is_none_or(JsonValue::is_null) {
            return Err(SessionManagerError::Io(std::io::Error::other(
                "Cannot read properties of undefined (reading 'role')",
            )));
        }
        if let Some(activity_time) = message_activity_time(entry) {
            last_activity_time = Some(last_activity_time.unwrap_or(0.0).max(activity_time));
        }
        let Some(message) = entry.get("message").and_then(JsonValue::as_object) else {
            continue;
        };
        let Some(role) = message.get("role").and_then(JsonValue::as_str) else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        // TS 对 `content: null`/对象、数组里的 null 块都会抛 TypeError，整个会话不可读。
        let text_content = match extract_text_content(message) {
            Ok(Some(text_content)) => text_content,
            Ok(None) => continue,
            Err(()) => {
                return Err(SessionManagerError::Io(std::io::Error::other(
                    "Cannot read properties of null (reading 'type')",
                )));
            }
        };
        if text_content.is_empty() {
            continue;
        }
        all_messages.push(text_content.clone());
        if first_message.is_empty() && role == "user" {
            first_message = text_content;
        }
    }

    let Some(header) = header else {
        return Ok(None);
    };
    // 派生字段：cwd/父会话按原始类型读取；created 用 Date 构造器的强制转换；
    // modified 三级回退（最新消息时间 > header 时间 > 文件 mtime）。
    let cwd = header
        .get("cwd")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let parent_session_path = match header.get("parentSession") {
        None | Some(JsonValue::Null) => None,
        Some(JsonValue::String(text)) => Some(text.clone()),
        // TS 原样返回；Rust 的 `Option<String>` 用 JS `String(value)` 承载非字符串值。
        Some(value) => Some(js_to_string(Some(value))),
    };
    let header_time = header
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .map(js_date_time)
        .unwrap_or(f64::NAN);
    // TS `created: new Date(header.timestamp)`：`null` → 0、布尔 → 0/1、数字 → 毫秒、
    // 字符串 → Date.parse、缺省/对象 → Invalid Date（`NaN`）。
    let created = js_date_constructor_time(header.get("timestamp"));
    let modified = match last_activity_time {
        Some(last_activity_time) if last_activity_time > 0.0 => last_activity_time,
        _ if !header_time.is_nan() => header_time,
        _ => stats_mtime,
    };
    // TS 原样返回 `header.id`；Rust 的 `String` 用 JS `String(value)` 承载非字符串值
    // （缺省仍为空串）。
    let id = match header.get("id") {
        None => String::new(),
        Some(JsonValue::String(text)) => text.clone(),
        Some(value) => js_to_string(Some(value)),
    };
    Ok(Some(SessionInfo {
        path: file_path.to_string(),
        id,
        cwd,
        name,
        parent_session_path,
        created,
        modified,
        message_count,
        first_message: if first_message.is_empty() {
            "(no messages)".to_string()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    }))
}

/// 按行读取（`readline` + `crlfDelay: Infinity` 语义：`\r\n`、`\n`、`\r` 都分行）。
///
/// 面向字节而不是 `str`：调用方随后自行 `from_utf8_lossy`，这样不必假定输入是合法 UTF-8。
/// 每次 `fill` 最多读 8 KiB；末尾没有行终止符的最后一行会在 EOF 时作为一行返回一次。
/// `\r` 恰好落在缓冲区末尾时会先补读，以判断它是否属于 `\r\n`。
struct LineReader<R: BufRead> {
    reader: R,
    pending: Vec<u8>,
    eof: bool,
}

impl<R: BufRead> LineReader<R> {
    fn new(reader: R) -> Self {
        LineReader {
            reader,
            pending: Vec::new(),
            eof: false,
        }
    }

    fn next_line(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(position) = self
                .pending
                .iter()
                .position(|byte| *byte == b'\n' || *byte == b'\r')
            {
                if self.pending[position] == b'\r'
                    && position + 1 == self.pending.len()
                    && !self.eof
                {
                    self.fill()?;
                    if !self.eof {
                        continue;
                    }
                }
                let line = self.pending[..position].to_vec();
                let mut skip = 1;
                if self.pending[position] == b'\r' && self.pending.get(position + 1) == Some(&b'\n')
                {
                    skip = 2;
                }
                self.pending.drain(..position + skip);
                return Ok(Some(line));
            }
            if self.eof {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(std::mem::take(&mut self.pending)));
            }
            self.fill()?;
        }
    }

    fn fill(&mut self) -> std::io::Result<()> {
        let mut chunk = [0u8; 8192];
        let read = self.reader.read(&mut chunk)?;
        if read == 0 {
            self.eof = true;
        } else {
            self.pending.extend_from_slice(&chunk[..read]);
        }
        Ok(())
    }
}

/// 一个映射任务（`Pin<Box<dyn Future>>`，避免给本 crate 引入 `futures`）。
type MapFuture<T> = Pin<Box<dyn std::future::Future<Output = Result<T, SessionListError>> + Send>>;

/// `mapWithConcurrency(items, limit, map, signal?)`。
///
/// TS 的语义：固定 `min(limit, n)` 个 worker，每个 worker 取任务前
/// `signal.throwIfAborted()`；已启动的任务不会被打断（完成后再检查）。
///
/// 实现要点（并发约束见 COMMENT_STYLE 的并发要求）：
/// - 任务索引由 `AtomicUsize` 领取，worker 数固定为 `min(limit, total)`；结果写入按索引
///   预分配的 `Vec<Option<R>>`，最后按原输入顺序收集——**输出顺序与完成顺序无关**；
/// - `signal` 只在每个任务开始前检查，因此取消不是即时的；已启动任务会跑完或在下一次
///   内部检查点失败；
/// - 任一任务返回错误或 worker panic，都会让本次调用返回第一个观察到的错误（panic 被
///   转换成 `SessionListError { name: "Error" }`）；其余 worker 不会被主动中止，但结果
///   会被丢弃。Mutex 中毒通过 `into_inner` 恢复，错误不会因此丢失。
async fn map_with_concurrency<T, R>(
    items: Vec<T>,
    limit: usize,
    map: Arc<dyn Fn(T, usize) -> MapFuture<R> + Send + Sync>,
    signal: Option<AbortScope>,
) -> Result<Vec<R>, SessionListError>
where
    T: Clone + Send + Sync + 'static,
    R: Send + 'static,
{
    let total = items.len();
    if total == 0 {
        return Ok(Vec::new());
    }
    let items = Arc::new(items);
    let next_index = Arc::new(AtomicUsize::new(0));
    let results: Arc<Mutex<Vec<Option<R>>>> =
        Arc::new(Mutex::new((0..total).map(|_| None).collect()));
    let mut join_set = tokio::task::JoinSet::new();

    for _ in 0..limit.min(total) {
        let items = Arc::clone(&items);
        let next_index = Arc::clone(&next_index);
        let results = Arc::clone(&results);
        let map = Arc::clone(&map);
        let signal = signal.clone();
        join_set.spawn(async move {
            loop {
                throw_if_aborted(signal.as_ref())?;
                let index = next_index.fetch_add(1, Ordering::SeqCst);
                if index >= total {
                    return Ok(());
                }
                let item = items[index].clone();
                let value = map(item, index).await?;
                let mut guard = results.lock().unwrap_or_else(|e| e.into_inner());
                guard[index] = Some(value);
            }
        });
    }

    let mut first_error: Option<SessionListError> = None;
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Err(_join_error) => {
                if first_error.is_none() {
                    first_error = Some(SessionListError {
                        name: "Error".to_string(),
                        message: "session listing task failed".to_string(),
                    });
                }
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    let mut results = results.lock().unwrap_or_else(|e| e.into_inner());
    Ok(results.drain(..).flatten().collect())
}

/// `listSessionsFromDir(dir, onProgress?, signal?)`。
///
/// 流程：目录不存在 → 空列表；`read_dir` 失败或任一条目读取失败 → 空列表（先检查是否
/// 因取消导致，若是则返回 `AbortError`）；只收 `.jsonl`，按文件名 locale 倒序后以
/// `MAX_CONCURRENT_SESSION_INFO_LOADS`（10）并发解析。
///
/// 进度发布点：第 1 个完成、每完成 10 个、以及全部完成时，`onProgress` 收到
/// `(loaded, total, Some(按 modified 降序的快照))`；其余时刻 `partialSessions` 为 `None`。
/// `total` 是候选文件数，包含随后会被 cwd 过滤掉的会话（过滤在 `SessionManager::list`
/// 的包装层完成）。
async fn list_sessions_from_dir(
    dir: &str,
    on_progress: Option<SessionListProgress>,
    signal: Option<&AbortScope>,
) -> Result<Vec<SessionInfo>, SessionListError> {
    throw_if_aborted(signal)?;
    if !Path::new(dir).exists() {
        return Ok(Vec::new());
    }

    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(_) => {
            throw_if_aborted(signal)?;
            return Ok(Vec::new());
        }
    };
    let mut files: Vec<String> = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else {
            throw_if_aborted(signal)?;
            return Ok(Vec::new());
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".jsonl") {
            files.push(path_join(&[dir, &name]));
        }
    }
    files.sort_by(|a, b| kiana_ai::model_catalog::locale_compare(b, a));

    let total = files.len() as u64;
    let loaded = Arc::new(AtomicUsize::new(0));
    let partial_sessions: Arc<Mutex<Vec<SessionInfo>>> = Arc::new(Mutex::new(Vec::new()));
    let progress_loaded = Arc::clone(&loaded);
    let progress_partial = Arc::clone(&partial_sessions);
    let progress = on_progress.clone();
    let signal_owned = signal.cloned();

    let map: Arc<dyn Fn(String, usize) -> MapFuture<Option<SessionInfo>> + Send + Sync> =
        Arc::new(move |path: String, _index: usize| {
            let progress_loaded = Arc::clone(&progress_loaded);
            let progress_partial = Arc::clone(&progress_partial);
            let progress = progress.clone();
            let signal = signal_owned.clone();
            Box::pin(async move {
                let info = build_session_info(&path, signal.as_ref(), None).await?;
                let loaded_now = progress_loaded.fetch_add(1, Ordering::SeqCst) as u64 + 1;
                let partial_snapshot = {
                    let mut partial = progress_partial.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(info) = &info {
                        partial.push(info.clone());
                    }
                    let publish = loaded_now == 1
                        || loaded_now.is_multiple_of(CURRENT_SESSION_LIST_PUBLISH_INTERVAL)
                        || loaded_now == total;
                    publish.then(|| sort_session_infos(partial.clone()))
                };
                if let Some(progress) = &progress {
                    progress(loaded_now, total, partial_snapshot);
                }
                Ok(info)
            })
        });

    let results = map_with_concurrency(
        files,
        MAX_CONCURRENT_SESSION_INFO_LOADS,
        map,
        signal.cloned(),
    )
    .await?;
    Ok(results.into_iter().flatten().collect())
}

/// 统计会话信息（并发 10），通过 `on_loaded` 上报进度。
///
/// `on_loaded(info, index)` 在对应项解析完成后调用；`info` 为 `None` 表示该文件不可读
/// （坏文件不终止整个列表）。返回值与输入候选同序，`None` 位保留。
async fn build_session_infos_with_concurrency(
    files: Vec<SessionFileCandidate>,
    on_loaded: Arc<dyn Fn(Option<SessionInfo>, usize) + Send + Sync>,
    signal: Option<&AbortScope>,
) -> Result<Vec<Option<SessionInfo>>, SessionListError> {
    let signal_owned = signal.cloned();
    let map: Arc<
        dyn Fn(SessionFileCandidate, usize) -> MapFuture<Option<SessionInfo>> + Send + Sync,
    > = Arc::new(move |file: SessionFileCandidate, index: usize| {
        let on_loaded = Arc::clone(&on_loaded);
        let signal = signal_owned.clone();
        Box::pin(async move {
            let info = build_session_info(&file.path, signal.as_ref(), file.mtime).await?;
            on_loaded(info.clone(), index);
            Ok(info)
        })
    });
    map_with_concurrency(
        files,
        MAX_CONCURRENT_SESSION_INFO_LOADS,
        map,
        signal.cloned(),
    )
    .await
}

/// `stat` 文件候选（失败只保留 path）。
///
/// `metadata` 失败时 `mtime = None`（排序时视为 `-inf`，排在最后）；每个任务开始前检查
/// 取消，因此中止后不再发起新的 stat。
async fn stat_candidates(
    paths: Vec<String>,
    signal: Option<&AbortScope>,
) -> Result<Vec<SessionFileCandidate>, SessionListError> {
    let signal_owned = signal.cloned();
    let map: Arc<dyn Fn(String, usize) -> MapFuture<SessionFileCandidate> + Send + Sync> =
        Arc::new(move |path: String, _index: usize| {
            let signal = signal_owned.clone();
            Box::pin(async move {
                throw_if_aborted(signal.as_ref())?;
                let mtime = std::fs::metadata(&path)
                    .ok()
                    .map(|metadata| mtime_ms(&metadata));
                Ok(SessionFileCandidate { path, mtime })
            })
        });
    map_with_concurrency(
        paths,
        MAX_CONCURRENT_SESSION_DISCOVERY_LOADS,
        map,
        signal.cloned(),
    )
    .await
}

/// `readdir` 一个目录并返回 `.jsonl` 文件（失败为空）。
///
/// 目录读取失败视为空列表（与 TS 的 `catch { return [] }` 一致），单个目录的结果在
/// 展平后合并；这里借用 `map_with_concurrency` 得到任务级的取消检查点。
async fn read_jsonl_files(
    dir: String,
    signal: Option<&AbortScope>,
) -> Result<Vec<String>, SessionListError> {
    let signal_owned = signal.cloned();
    let map: Arc<dyn Fn(String, usize) -> MapFuture<Vec<String>> + Send + Sync> =
        Arc::new(move |dir: String, _index: usize| {
            let signal = signal_owned.clone();
            Box::pin(async move {
                throw_if_aborted(signal.as_ref())?;
                let Ok(read_dir) = std::fs::read_dir(&dir) else {
                    return Ok(Vec::new());
                };
                let mut files = Vec::new();
                for entry in read_dir.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".jsonl") {
                        files.push(path_join(&[&dir, &name]));
                    }
                }
                Ok(files)
            })
        });
    map_with_concurrency(
        vec![dir],
        MAX_CONCURRENT_SESSION_DISCOVERY_LOADS,
        map,
        signal.cloned(),
    )
    .await
    .map(|lists| lists.into_iter().flatten().collect())
}

// ---------------------------------------------------------------------------
// SessionManager
// ---------------------------------------------------------------------------

/// TS `leafId: string | null` 的运行时状态。
///
/// `_buildIndex` 直接执行 `leafId = entry.id`：畸形条目（非对象行、缺 `id` 的对象）
/// 会把 leaf 变成 `undefined`，与初始的 `null` 在追加/遍历时行为不同
/// （`parentId: undefined` 会被 `JSON.stringify` 省略，而 `null` 写成 `"parentId":null`）。
///
/// TS: `session-manager.ts#SessionManager.leafId`
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum LeafPointer {
    /// 初始状态 / `resetLeaf()` / `branch(null)`：`parentId` 写为 `null`。
    #[default]
    Null,
    /// 最后一条非 header 条目的 `id` 是 `undefined`（缺省属性 / 非对象行）。
    Undefined,
    /// 最后一条非 header 条目的 `id` 是其它非字符串原始值（数字/布尔/对象/数组）。
    ///
    /// TS 的 `leafId` 原样保存该值（`getLeafId()` 返回它，导出时 `JSON.stringify`
    /// 按原类型写出）；typed 视图无法表达，除原始 JSON 视图（[`SessionManager::get_leaf_id_json`]）
    /// 外其余逻辑按 `undefined` 处理。
    Other(JsonValue),
    /// 具体条目 id。
    Id(String),
}

/// 会话管理器。
///
/// 职责：持有会话文件的全部条目（`file_entries`）与派生索引（`by_id`、标签表、leaf
/// 指针），提供追加、投影、树遍历与分支操作。除显式调用 `set_session_file` 重新加载外，
/// 读取都基于内存副本；写操作只有两种——`persist_last_entry` 追加一条，或
/// `rewrite_file` 全量覆盖（迁移 / 分支切分）。
///
/// 不变量（正常加载的持久化会话）：
/// - `file_entries` 的顺序与文件行序一致，header 是首个 `type:"session"` 的条目；
/// - `by_id` 只含 id 为字符串的条目，同 id 后者覆盖前者；非字符串 id 由
///   `other_index_keys` / `undefined_index_key` 单独计数（供 `get_entry_count`）；
/// - `leaf_id` 指向最后一条非 header 条目的 id（`build_index` 时重建），每次追加后前进到
///   新条目；
/// - `persist == false` 时 `session_file` 为 `None`，所有写盘函数都是 no-op。
///
/// 并发：本类型没有内部锁，`append_*` / `branch*` 需要 `&mut self`；跨线程共享由持有它的
/// 上层加锁（例如会话运行时用 `Mutex<SessionManager>`），只读者可用
/// [`ReadonlySessionManager`] 的 `&self` 视图。
///
/// TS: `session-manager.ts#SessionManager`
#[derive(Debug, Clone)]
pub struct SessionManager {
    session_id: String,
    session_file: Option<String>,
    session_dir: String,
    cwd: String,
    persist: bool,
    flushed: bool,
    file_entries: Vec<StoredFileEntry>,
    by_id: IndexMap<String, SessionEntry>,
    /// TS 的 `byId` 里 id 不是字符串的条目（Map 以原始值为键）。字符串查询取不到，
    /// 但 `getEntryCount()` 要计入这些不同键的数量。
    other_index_keys: Vec<JsonValue>,
    /// `byId` 是否含有以 `undefined` 为键的条目（非对象行 / 缺 `id` 的对象）。
    undefined_index_key: bool,
    labels_by_id: IndexMap<String, String>,
    /// TS `labelTimestampsById: Map<string, unknown>`：`None` 表示值为 `undefined`
    /// （键存在但来源条目没有 `timestamp`），其余保留原始 JSON 值。
    label_timestamps_by_id: IndexMap<String, Option<JsonValue>>,
    leaf_id: LeafPointer,
}

/// 只读会话管理器视图（扩展上下文与压缩单元使用）。
///
/// 所有方法只借用 `&self`：返回引用时生命周期绑定到实现者，返回 `Vec`/投影时是克隆
/// 快照。实现者必须 `Send + Sync`（扩展 runner 会跨线程/跨任务持有它）。仓库内的实现
/// 包括 [`SessionManager`] 与扩展侧的包装/测试 fake；本 trait 是二者之间的最小读接口。
///
/// TS: `session-manager.ts#ReadonlySessionManager`
pub trait ReadonlySessionManager: Send + Sync {
    /// `getCwd()`。
    fn get_cwd(&self) -> &str;
    /// `getSessionDir()`。
    fn get_session_dir(&self) -> &str;
    /// `getSessionId()`。
    fn get_session_id(&self) -> &str;
    /// `getSessionFile()`。
    fn get_session_file(&self) -> Option<&str>;
    /// `getLeafId()`。
    fn get_leaf_id(&self) -> Option<&str>;
    /// `getLeafEntry()`。
    fn get_leaf_entry(&self) -> Option<&SessionEntry>;
    /// `getEntry(id)`。
    fn get_entry(&self, id: &str) -> Option<&SessionEntry>;
    /// `getLabel(id)`。
    fn get_label(&self, id: &str) -> Option<&str>;
    /// `getBranch(fromId?)`。
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry>;
    /// `buildContextEntries()`。
    fn build_context_entries(&self) -> Vec<SessionEntry>;
    /// `buildSessionProjection()`。
    fn build_session_projection(&self) -> SessionProjection;
    /// `getHeader()`。
    fn get_header(&self) -> Option<&SessionHeader>;
    /// `getEntries()`。
    fn get_entries(&self) -> Vec<SessionEntry>;
    /// `getTree()`。
    fn get_tree(&self) -> Vec<SessionTreeNode>;
    /// `getSessionName()`。
    fn get_session_name(&self) -> Option<String>;
}

impl SessionManager {
    /// 私有构造函数（通过静态工厂使用）。
    ///
    /// 三条互斥的初始化分支：
    /// 1. 显式 `session_file`：`set_session_file_internal`（加载/恢复已有文件，或按该路径
    ///    新建会话）；
    /// 2. 有非空 `preloaded_file_entries`：直接 `load_entries`（`in_memory` 用）；
    /// 3. 其它：`new_session`。
    /// 持久化时先递归创建 `session_dir`；`cwd`、`session_dir` 在这里统一归一化。
    fn new(
        cwd: &str,
        session_dir: &str,
        session_file: Option<&str>,
        persist: bool,
        new_session_options: Option<NewSessionOptions>,
        preloaded_file_entries: Option<Vec<StoredFileEntry>>,
    ) -> Result<Self, SessionManagerError> {
        let mut manager = SessionManager {
            session_id: String::new(),
            session_file: None,
            session_dir: kiana_core::paths::normalize_path(session_dir, &Default::default()),
            cwd: resolve_path_default(cwd),
            persist,
            flushed: false,
            file_entries: Vec::new(),
            by_id: IndexMap::new(),
            other_index_keys: Vec::new(),
            undefined_index_key: false,
            labels_by_id: IndexMap::new(),
            label_timestamps_by_id: IndexMap::new(),
            leaf_id: LeafPointer::Null,
        };
        if manager.persist
            && !manager.session_dir.is_empty()
            && !Path::new(&manager.session_dir).exists()
        {
            std::fs::create_dir_all(&manager.session_dir)
                .map_err(|error| node_fs_error(error, "mkdir", &manager.session_dir))?;
        }
        if let Some(session_file) = session_file {
            manager.set_session_file_internal(session_file, preloaded_file_entries)?;
        } else if preloaded_file_entries
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
        {
            let entries = preloaded_file_entries.unwrap_or_default();
            manager.load_entries(entries, new_session_options)?;
        } else {
            manager.new_session(new_session_options)?;
        }
        Ok(manager)
    }

    /// `SessionManager.create(cwd, sessionDir?, options?)`。
    ///
    /// 创建新的持久化会话。`session_dir` 为空/缺省时用 `get_default_session_dir(cwd, None)`
    /// （会递归创建目录）；显式目录只做归一化，目录创建由构造函数完成。`options.id` 先经
    /// `assert_valid_session_id` 校验，非法时返回 `InvalidSessionId`。
    ///
    /// 返回时**尚未写盘**：只有出现 user/assistant 消息后才会创建文件（见
    /// `persist_last_entry`）。目录创建失败按 Node fs 文案返回 `Io`。
    ///
    /// TS: session-manager.ts#SessionManager.create
    pub fn create(
        cwd: &str,
        session_dir: Option<&str>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self, SessionManagerError> {
        let dir = resolve_session_dir(cwd, session_dir)?;
        Self::new(cwd, &dir, None, true, options, None)
    }

    /// `SessionManager.open(path, sessionDir?, cwdOverride?)`。
    ///
    /// 打开指定会话文件。要点：
    /// - `cwd_override` 为 `None` 且文件存在时，先有界扫描 header（最多 1 MiB）；超过上限
    ///   时回退为全量流式加载，并用首条记录作为 header（超大 header/前缀的旧文件仍可打开）；
    /// - cwd 优先级：`cwd_override` > header 的 `cwd`（仅当是字符串）> 进程当前目录；
    /// - `session_dir` 为空/缺省时取文件所在目录（`dirname`）；
    /// - 文件不存在时按该路径新建会话，同样延迟到首条对话消息才落盘；存在但不是合法会话
    ///   （非空且无合法 header）返回 `InvalidSessionFile`，原文件不被修改；
    /// - 加载中若发生 v1/v2 迁移，会立即覆盖重写该文件。
    ///
    /// 错误：header 读取的 IO 错误、回退加载错误、迁移重写错误、`InvalidSessionFile`。
    ///
    /// TS: session-manager.ts#SessionManager.open
    pub fn open(
        path: &str,
        session_dir: Option<&str>,
        cwd_override: Option<&str>,
    ) -> Result<Self, SessionManagerError> {
        let resolved_path = resolve_path_default(path);
        let mut header: Option<SessionHeader> = None;
        let mut preloaded_file_entries: Option<Vec<StoredFileEntry>> = None;
        if cwd_override.is_none() && Path::new(&resolved_path).exists() {
            match read_session_header(&resolved_path) {
                Ok(found) => header = found,
                Err(error) if error.is_header_scan_limit() => {
                    let entries = load_stored_entries_from_file(&resolved_path)?;
                    header = entries
                        .first()
                        .and_then(StoredFileEntry::as_header)
                        .cloned();
                    preloaded_file_entries = Some(entries);
                }
                Err(error) => return Err(error),
            }
        }
        let cwd = cwd_override
            .map(str::to_string)
            .or_else(|| {
                header
                    .as_ref()
                    .and_then(|header| header.cwd_string().map(str::to_string))
            })
            .unwrap_or_else(current_dir_string);
        let dir = match session_dir {
            Some(dir) if !dir.is_empty() => {
                kiana_core::paths::normalize_path(dir, &Default::default())
            }
            _ => path_dirname(&resolved_path),
        };
        Self::new(
            &cwd,
            &dir,
            Some(&resolved_path),
            true,
            None,
            preloaded_file_entries,
        )
    }

    /// `SessionManager.continueRecent(cwd, sessionDir?)`。
    ///
    /// 打开 `session_dir` 中最近的会话；没有可用会话时新建。`session_dir` 缺省时使用默认
    /// 目录并按默认规则检索（目录名本身已编码 cwd，不再过滤）。显式指定且该目录不等于
    /// cwd 的默认目录时，额外要求 header 的 cwd 解析后与本 cwd 相等，避免共享目录里混入
    /// 其它项目的会话。错误：目录解析/创建失败。
    ///
    /// TS: session-manager.ts#SessionManager.continueRecent
    pub fn continue_recent(
        cwd: &str,
        session_dir: Option<&str>,
    ) -> Result<Self, SessionManagerError> {
        let dir = resolve_session_dir(cwd, session_dir)?;
        let filter_cwd =
            session_dir.is_some() && dir != get_default_session_dir_path(cwd, &default_agent_dir());
        let most_recent = find_most_recent_session(&dir, filter_cwd.then_some(cwd));
        Self::new(cwd, &dir, most_recent.as_deref(), true, None, None)
    }

    /// `SessionManager.inMemory(cwd?, options?, entries?)`。
    ///
    /// 内存态永不落盘（`persist = false`，`session_dir` 为空串）。`entries` 被复制为内存
    /// 条目后走 `load_entries`：若其中没有 header，会先用 `options` 新建 header，再把传入
    /// 条目接在其后；`None` 与空 `Vec` 等价，都直接新建空会话。`get_session_file()` 恒为
    /// `None`，所有写盘函数都是 no-op。
    ///
    /// TS: session-manager.ts#SessionManager.inMemory
    pub fn in_memory(
        cwd: &str,
        options: Option<NewSessionOptions>,
        entries: Option<Vec<FileEntry>>,
    ) -> Result<Self, SessionManagerError> {
        let entries =
            entries.map(|entries| entries.into_iter().map(StoredFileEntry::from).collect());
        Self::new(cwd, "", None, false, options, entries)
    }

    /// `SessionManager.forkFrom(sourcePath, targetCwd, sessionDir?, options?)`。
    ///
    /// 从另一个会话文件 fork 出新会话（例如把会话迁到另一个项目目录）。
    ///
    /// 源文件必须非空（否则 `ForkEmptyOrInvalid`）；没有合法 header 的源文件同样返回
    /// `ForkEmptyOrInvalid`——`load_stored_entries_from_file` 只在首个成功解析的真值条目
    /// 是合法 header 时才返回非空 `Vec`，所以下面的 `ForkNoHeader` 分支在当前加载器下
    /// 不可达，只是防御性映射（加载器语义改变时才可能触发）。新文件写在目标目录下，文件名
    /// `{ISO 时间戳，:/. 替换为 -}_{id}.jsonl`，以 `create_new` 打开，绝不覆盖已有文件。
    /// 新 header 的 `cwd` 为 `target_cwd`、`parentSession` 为解析后的源路径；源文件的
    /// 所有非 header 条目经原始对象逐字回放（未知字段与键序保留）。
    ///
    /// `options.id` 非法时返回 `InvalidSessionId`。源文件读失败、目标目录创建失败或写新文件
    /// 失败都会以 `Io` 上抛：目录创建与 `create_new` 打开用 `node_fs_error` 重建为 Node
    /// 风格（`create_new` 的冲突是 `EEXIST`）；源文件的 `File::open`/读取与写新文件时的
    /// `BufWriter` 写入经 `?` 原样包装，保留 Rust 原始 `io::Error` 文案（源文件末尾补换行
    /// 的追加打开仍用 Node 文案）。
    /// 返回的 manager 指向已写好的新文件；后续 `load_entries` 会重建索引（并把 `flushed`
    /// 置为 true）。
    ///
    /// TS: session-manager.ts#SessionManager.forkFrom
    pub fn fork_from(
        source_path: &str,
        target_cwd: &str,
        session_dir: Option<&str>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self, SessionManagerError> {
        let resolved_source_path = resolve_path_default(source_path);
        let resolved_target_cwd = resolve_path_default(target_cwd);
        let source_entries = load_stored_entries_from_file(&resolved_source_path)?;
        if source_entries.is_empty() {
            return Err(SessionManagerError::ForkEmptyOrInvalid {
                path: resolved_source_path,
            });
        }
        if !source_entries.iter().any(|entry| entry.kind() == "session") {
            return Err(SessionManagerError::ForkNoHeader {
                path: resolved_source_path,
            });
        }
        let dir = resolve_session_dir(&resolved_target_cwd, session_dir)?;
        if !Path::new(&dir).exists() {
            std::fs::create_dir_all(&dir).map_err(|error| node_fs_error(error, "mkdir", &dir))?;
        }
        let options = options.unwrap_or_default();
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        let new_session_id = options.id.clone().unwrap_or_else(create_session_id);
        let timestamp = now_iso_string();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file =
            path_join(&[&dir, &format!("{file_timestamp}_{new_session_id}.jsonl")]);
        let new_header = SessionHeader {
            kind: SessionTypeMarker::Marker,
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id,
            timestamp: Some(timestamp),
            cwd: Some(resolved_target_cwd.clone()),
            parent_session: Some(resolved_source_path),
        };
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&new_session_file)
            .map_err(|error| node_fs_error(error, "open", &new_session_file))?;
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(entry_json_string(&FileEntry::Header(new_header)).as_bytes())?;
        writer.write_all(b"\n")?;
        for entry in &source_entries {
            if entry.kind() != "session" {
                writer.write_all(entry.json_string().as_bytes())?;
                writer.write_all(b"\n")?;
            }
        }
        writer.flush()?;
        drop(writer);
        Self::new(
            &resolved_target_cwd,
            &dir,
            Some(&new_session_file),
            true,
            None,
            None,
        )
    }

    /// `SessionManager.findById(cwd, id, sessionDir?)`。
    ///
    /// 按精确 id 查找会话文件，不构建完整会话列表、也不解析消息体。只扫描
    /// `session_dir`（缺省为 cwd 的默认目录）下的直接 `.jsonl` 文件并读取 header；
    /// 显式目录且不等于 cwd 默认目录时同样启用 cwd 过滤。
    ///
    /// 目录不可读或没有匹配返回 `Ok(None)`；只有目录解析/创建失败才返回 `Err`。返回顺序
    /// 取决于底层 `read_dir`，同 id 的多个文件会返回先遇到的那个。
    ///
    /// TS: session-manager.ts#SessionManager.findById
    pub fn find_by_id(
        cwd: &str,
        id: &str,
        session_dir: Option<&str>,
    ) -> Result<Option<String>, SessionManagerError> {
        let dir = resolve_session_dir(cwd, session_dir)?;
        let filter_cwd =
            session_dir.is_some() && dir != get_default_session_dir_path(cwd, &default_agent_dir());
        let resolved_cwd = resolve_path_default(cwd);
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            return Ok(None);
        };
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".jsonl") {
                continue;
            }
            let path = path_join(&[&dir, &name]);
            let Some(header) = read_session_header_for_discovery(&path) else {
                continue;
            };
            if header.id != id {
                continue;
            }
            if filter_cwd && !session_cwd_matches(header.cwd_string(), &resolved_cwd) {
                continue;
            }
            return Ok(Some(path));
        }
        Ok(None)
    }

    /// `SessionManager.list(cwd, sessionDir?, onProgress?, signal?)`。
    ///
    /// 列出 cwd 对应的会话，按 `modified` 降序返回。`filter_cwd` 仅在“显式 session_dir
    /// 且它不是 cwd 的默认目录”时启用：header 的 cwd 必须解析后等于 `cwd`，进度快照也会
    /// 被同样过滤；但 `loaded`/`total` 仍统计全部候选文件，不扣除被过滤项。
    ///
    /// 取消：`signal` 中止时返回 `SessionListError`，`name` 通常为 `"AbortError"`；若 abort
    /// reason 自带 `name`/`message` 则原样沿用（见 `throw_if_aborted` 与 `SessionListError`）。
    /// 目录解析/创建失败包装成 `name: "Error"`（TS 在 try/catch 之外计算目录，失败会让
    /// promise reject）。单个文件的读取/解析错误被吞掉（该会话只是不出现在结果里）。
    ///
    /// 本方法会触发 `listing_call_probe::LIST` 计数（test hook，见模块内说明）。
    ///
    /// TS: session-manager.ts#SessionManager.list
    pub async fn list(
        cwd: &str,
        session_dir: Option<&str>,
        on_progress: Option<SessionListProgress>,
        signal: Option<&AbortScope>,
    ) -> Result<Vec<SessionInfo>, SessionListError> {
        listing_call_probe::LIST.fetch_add(1, Ordering::SeqCst);
        // TS 在 try/catch 之外计算 `dir`：目录解析/创建失败会让 list() 的 promise 拒绝，
        // 而不是返回空列表。
        let dir = resolve_session_dir(cwd, session_dir).map_err(|error| SessionListError {
            name: "Error".to_string(),
            message: error.to_string(),
        })?;
        let filter_cwd =
            session_dir.is_some() && dir != get_default_session_dir_path(cwd, &default_agent_dir());
        let resolved_cwd = resolve_path_default(cwd);
        let include_cwd = resolved_cwd.clone();
        let include = move |session: &SessionInfo| -> bool {
            !filter_cwd || session_cwd_matches(Some(session.cwd.as_str()), &include_cwd)
        };
        let progress: Option<SessionListProgress> = on_progress.map(|callback| {
            let resolved_cwd = resolved_cwd.clone();
            Arc::new(
                move |loaded: u64, total: u64, partial: Option<Vec<SessionInfo>>| {
                    let partial = partial.map(|sessions| {
                        sessions
                            .into_iter()
                            .filter(|session| {
                                !filter_cwd
                                    || session_cwd_matches(
                                        Some(session.cwd.as_str()),
                                        &resolved_cwd,
                                    )
                            })
                            .collect::<Vec<_>>()
                    });
                    callback(loaded, total, partial);
                },
            ) as SessionListProgress
        });
        let sessions = list_sessions_from_dir(&dir, progress, signal).await?;
        let sessions = sessions
            .into_iter()
            .filter(|session| include(session))
            .collect();
        Ok(sort_session_infos(sessions))
    }

    /// `SessionManager.listAll(onProgress?, signal?)`。
    ///
    /// 列出默认 sessions 根目录下所有项目子目录中的会话，按 `modified` 降序返回。只扫描
    /// 根目录的直接子项：目录与符号链接都算（不解析链接目标，是否可读由后续 `read_dir`
    /// 决定）。收集完所有子目录的 `.jsonl` 后，全部候选按 mtime 降序（mtime 缺失排最后，
    /// 同值按文件名 locale 倒序）排序，再以 10 并发解析。
    ///
    /// 进度：只有在索引 0 的候选完成后才开始发布；发布点为“首个候选”“每 100 个”“最后
    /// 一个”。取消与错误语义同 [`SessionManager::list`]；根目录读取失败返回空列表，单个
    /// 子目录读取失败按空目录处理（不终止其它子目录的扫描），因此最终结果仍可能包含其它
    /// 子目录的会话。
    ///
    /// 本方法会触发 `listing_call_probe::LIST_ALL` 计数（test hook）。
    ///
    /// TS: session-manager.ts#SessionManager.listAll
    pub async fn list_all(
        on_progress: Option<SessionListProgress>,
        signal: Option<&AbortScope>,
    ) -> Result<Vec<SessionInfo>, SessionListError> {
        listing_call_probe::LIST_ALL.fetch_add(1, Ordering::SeqCst);
        throw_if_aborted(signal)?;
        let sessions_dir = kiana_core::config::get_sessions_dir()
            .to_string_lossy()
            .into_owned();
        if !Path::new(&sessions_dir).exists() {
            return Ok(Vec::new());
        }
        let Ok(read_dir) = std::fs::read_dir(&sessions_dir) else {
            return Ok(Vec::new());
        };
        let mut dirs: Vec<String> = Vec::new();
        for entry in read_dir.flatten() {
            let file_type = entry.file_type();
            let is_dir = file_type
                .as_ref()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false);
            let is_symlink = file_type
                .as_ref()
                .map(|file_type| file_type.is_symlink())
                .unwrap_or(false);
            if is_dir || is_symlink {
                dirs.push(path_join(&[
                    &sessions_dir,
                    &entry.file_name().to_string_lossy(),
                ]));
            }
        }

        let mut all_files: Vec<String> = Vec::new();
        let signal_owned = signal.cloned();
        let read_dirs: Arc<dyn Fn(String, usize) -> MapFuture<Vec<String>> + Send + Sync> =
            Arc::new(move |dir: String, _index: usize| {
                let signal = signal_owned.clone();
                Box::pin(async move {
                    throw_if_aborted(signal.as_ref())?;
                    read_jsonl_files(dir, signal.as_ref()).await
                })
            });
        let dir_files = map_with_concurrency(
            dirs,
            MAX_CONCURRENT_SESSION_DISCOVERY_LOADS,
            read_dirs,
            signal.cloned(),
        )
        .await?;
        for files in dir_files {
            all_files.extend(files);
        }

        let mut candidates = stat_candidates(all_files, signal).await?;
        // mtime 降序；无 stat 的（None）当作 -inf 排最后；mtime 相同则按文件名 locale 倒序。
        candidates.sort_by(|a, b| {
            let a_mtime = a.mtime.unwrap_or(f64::NEG_INFINITY);
            let b_mtime = b.mtime.unwrap_or(f64::NEG_INFINITY);
            let delta = b_mtime - a_mtime;
            if delta.is_nan() || delta == 0.0 {
                kiana_ai::model_catalog::locale_compare(
                    &path_basename(&b.path),
                    &path_basename(&a.path),
                )
            } else if delta < 0.0 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        });

        let total_files = candidates.len() as u64;
        let loaded = Arc::new(AtomicUsize::new(0));
        let first_candidate_loaded = Arc::new(AtomicBool::new(false));
        let partial_sessions: Arc<Mutex<Vec<SessionInfo>>> = Arc::new(Mutex::new(Vec::new()));
        let progress = on_progress;
        let loaded_for_callback = Arc::clone(&loaded);
        let first_for_callback = Arc::clone(&first_candidate_loaded);
        let partial_for_callback = Arc::clone(&partial_sessions);
        // 进度发布策略（与 TS 相同）：先等索引 0 的候选完成（`first_candidate_loaded`），
        // 之后在“首个候选”“每 100 个”“最后一个”发布排序后的部分快照。
        let on_loaded: Arc<dyn Fn(Option<SessionInfo>, usize) + Send + Sync> =
            Arc::new(move |info: Option<SessionInfo>, index: usize| {
                let loaded_now = loaded_for_callback.fetch_add(1, Ordering::SeqCst) as u64 + 1;
                if index == 0 {
                    first_for_callback.store(true, Ordering::SeqCst);
                }
                let publish_partial = first_for_callback.load(Ordering::SeqCst)
                    && (index == 0
                        || loaded_now.is_multiple_of(ALL_SESSION_LIST_PUBLISH_INTERVAL)
                        || loaded_now == total_files);
                let partial_snapshot = {
                    let mut partial = partial_for_callback
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Some(info) = &info {
                        partial.push(info.clone());
                    }
                    publish_partial.then(|| sort_session_infos(partial.clone()))
                };
                if let Some(progress) = &progress {
                    progress(loaded_now, total_files, partial_snapshot);
                }
            });

        let results = build_session_infos_with_concurrency(candidates, on_loaded, signal).await?;
        Ok(sort_session_infos(results.into_iter().flatten().collect()))
    }

    /// `SessionManager.listAll(sessionDir?, onProgress?, signal?)`。
    ///
    /// 列出指定目录（非递归）中的会话，按 `modified` 降序返回；对应 TS `listAll` 的显式
    /// 目录分支。进度发布规则与 `list_sessions_from_dir` 相同（第 1 个、每 10 个、最后
    /// 一个）。同样触发 `listing_call_probe::LIST_ALL` 计数。
    ///
    /// TS: session-manager.ts#SessionManager.listAll
    pub async fn list_all_in(
        session_dir: &str,
        on_progress: Option<SessionListProgress>,
        signal: Option<&AbortScope>,
    ) -> Result<Vec<SessionInfo>, SessionListError> {
        listing_call_probe::LIST_ALL.fetch_add(1, Ordering::SeqCst);
        throw_if_aborted(signal)?;
        let dir = kiana_core::paths::normalize_path(session_dir, &Default::default());
        let sessions = list_sessions_from_dir(&dir, on_progress, signal).await?;
        Ok(sort_session_infos(sessions))
    }

    /// 切换到另一个会话文件（resume / branch）。
    ///
    /// 文件存在时：加载并在需要时迁移；空文件会用**新会话 id** 初始化、立即写入 header 并
    /// 置 `flushed = true`；非空但解析不出 header 的文件返回 `InvalidSessionFile`，原文件
    /// 不被修改。文件不存在时：按该路径新建会话（`flushed` 保持 false），同样延迟到首条
    /// 对话消息才写盘。
    ///
    /// TS: session-manager.ts#SessionManager.setSessionFile
    pub fn set_session_file(&mut self, session_file: &str) -> Result<(), SessionManagerError> {
        self.set_session_file_internal(session_file, None)
    }

    /// `_setSessionFile` 的实现：加载已有文件，或按该路径新建会话。
    ///
    /// 分支：
    /// - 文件存在、解析结果为空：若文件大小为 0，用新会话 header 覆盖重写并置
    ///   `flushed = true`（`session_file` 仍是调用方给出的显式路径，而不是 `new_session`
    ///   按 session_dir 生成的新文件名）；否则返回 `InvalidSessionFile`（不修改文件）；
    /// - 文件存在、解析结果非空：`load_entries`（可能触发迁移重写），置 `flushed = true`；
    /// - 文件不存在：新建会话并保留显式路径，`flushed` 保持 false（首条对话消息才落盘）。
    ///
    /// `preloaded_file_entries` 由 `open` 在 header 扫描超限时传入，避免二次全量读取。
    fn set_session_file_internal(
        &mut self,
        session_file: &str,
        preloaded_file_entries: Option<Vec<StoredFileEntry>>,
    ) -> Result<(), SessionManagerError> {
        self.session_file = Some(resolve_path_default(session_file));
        let explicit_path = self.session_file.clone().unwrap_or_default();
        if Path::new(&explicit_path).exists() {
            let entries = match preloaded_file_entries {
                Some(entries) => entries,
                None => load_stored_entries_from_file(&explicit_path)?,
            };
            if entries.is_empty() {
                if std::fs::metadata(&explicit_path)?.len() > 0 {
                    return Err(SessionManagerError::InvalidSessionFile {
                        path: explicit_path,
                    });
                }
                self.new_session(None)?;
                self.session_file = Some(explicit_path);
                self.rewrite_file()?;
                self.flushed = true;
                return Ok(());
            }
            self.load_entries(entries, None)?;
            self.flushed = true;
        } else {
            self.new_session(None)?;
            self.session_file = Some(explicit_path);
        }
        Ok(())
    }

    /// `newSession(options?)`。
    ///
    /// 生成新会话并重置全部内存状态（条目、索引、标签、leaf）。只写内存：header 存在
    /// `file_entries` 中但不落盘；持久化时立即计算
    /// `session_file = <session_dir>/{ISO 时间戳，:/. → -}_{id}.jsonl` 并返回该路径，
    /// 内存态返回 `None`。自定义 id 非法时返回 `InvalidSessionId`。
    ///
    /// TS: session-manager.ts#SessionManager.newSession
    pub fn new_session(
        &mut self,
        options: Option<NewSessionOptions>,
    ) -> Result<Option<String>, SessionManagerError> {
        let options = options.unwrap_or_default();
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        self.session_id = options.id.clone().unwrap_or_else(create_session_id);
        let timestamp = now_iso_string();
        let header = SessionHeader {
            kind: SessionTypeMarker::Marker,
            version: Some(CURRENT_SESSION_VERSION),
            id: self.session_id.clone(),
            timestamp: Some(timestamp.clone()),
            cwd: Some(self.cwd.clone()),
            parent_session: options.parent_session.clone(),
        };
        self.file_entries = vec![StoredFileEntry::new(FileEntry::Header(header))];
        self.by_id.clear();
        self.other_index_keys.clear();
        self.undefined_index_key = false;
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = LeafPointer::Null;
        self.flushed = false;
        if self.persist {
            let file_timestamp = timestamp.replace([':', '.'], "-");
            self.session_file = Some(path_join(&[
                &self.session_dir,
                &format!("{file_timestamp}_{}.jsonl", self.session_id),
            ]));
        }
        Ok(self.session_file.clone())
    }

    /// 装载已有条目并重建索引（除迁移触发重写外不写盘）。
    ///
    /// header 取第一条 `kind() == "session"` 的条目（不要求位于第 0 位；正常文件由
    /// `load_stored_entries_from_file` 保证）。没有 header 时先用 `options` 新建会话，再把
    /// 传入条目整体接在 header 之后。随后按版本迁移，发生迁移时立即 `rewrite_file` 覆盖
    /// 原文件；最后 `build_index`。
    fn load_entries(
        &mut self,
        entries: Vec<StoredFileEntry>,
        options: Option<NewSessionOptions>,
    ) -> Result<(), SessionManagerError> {
        let header_position = entries.iter().position(|entry| entry.kind() == "session");
        match header_position {
            Some(position) => {
                self.session_id = entries[position].id().to_string();
                self.file_entries = entries;
                if migrate_stored_to_current_version(&mut self.file_entries) {
                    self.rewrite_file()?;
                }
            }
            None => {
                self.new_session(options)?;
                self.file_entries.extend(entries);
            }
        }
        self.build_index();
        Ok(())
    }

    /// `_buildIndex`：从 `file_entries` 重建 `by_id`、标签表与 leaf 指针。
    ///
    /// 跳过 header；字符串 id 进 `by_id` 并推进 leaf；非字符串 id 记录到
    /// `other_index_keys` / `undefined_index_key`，并按 TS 的 `leafId = entry.id` 语义更新
    /// `LeafPointer`。非对象的原始值没有 `id` 属性，同样按 `undefined` 处理。label 条目在
    /// 扫描过程中即时应用（后出现者覆盖先前的），因此无需第二遍。
    fn build_index(&mut self) {
        let SessionManager {
            file_entries,
            by_id,
            other_index_keys,
            undefined_index_key,
            leaf_id,
            labels_by_id,
            label_timestamps_by_id,
            ..
        } = self;
        by_id.clear();
        other_index_keys.clear();
        *undefined_index_key = false;
        labels_by_id.clear();
        label_timestamps_by_id.clear();
        *leaf_id = LeafPointer::Null;
        for stored in file_entries.iter() {
            if stored.kind() == "session" {
                continue;
            }
            if let FileEntry::Entry(entry) = &stored.entry {
                // TS 的 `byId.set(entry.id, entry)` 以原始值为键；只有字符串 id 能被
                // 字符串查询命中，其余键单独计数（`getEntryCount` == `byId.size`）。
                match stored.string_id() {
                    Some(id) => {
                        by_id.insert(id.to_string(), entry.clone());
                        *leaf_id = LeafPointer::Id(id.to_string());
                    }
                    None => {
                        // TS `leafId = entry.id`：`id: null` 时 leaf 就是 `null`（追加写
                        // `"parentId":null`）；其它非字符串值保留原值（导出时按原类型
                        // 写出），除原始 JSON 视图外按 `undefined` 处理。
                        *leaf_id = match stored.raw_id_value() {
                            Some(JsonValue::Null) => LeafPointer::Null,
                            Some(value) => LeafPointer::Other(value.clone()),
                            None => LeafPointer::Undefined,
                        };
                        match stored.raw_id_value() {
                            // `id` 缺省（对象没有该键）：Map 键是 `undefined`。
                            None => *undefined_index_key = true,
                            Some(value) => {
                                if !other_index_keys
                                    .iter()
                                    .any(|existing| js_map_key_eq(existing, value))
                                {
                                    other_index_keys.push(value.clone());
                                }
                            }
                        }
                    }
                }
            } else {
                // 非对象值没有 `id` 属性（`entry.id === undefined`）。
                *leaf_id = LeafPointer::Undefined;
                *undefined_index_key = true;
            }
            if stored.kind() == "label"
                && let FileEntry::Entry(entry) = &stored.entry
            {
                apply_label_entry(stored, entry, labels_by_id, label_timestamps_by_id);
            }
        }
    }

    /// `_rewriteFile`：清空并重写整个文件（迁移与分支切分使用）。
    ///
    /// `File::create` 会截断已有文件；非持久化或没有 `session_file` 时是 no-op。每条记录
    /// 一行、行尾 `\n`。写入过程中失败可能留下截断文件——TS 也没有临时文件 + rename 的
    /// 原子替换步骤。
    fn rewrite_file(&self) -> Result<(), SessionManagerError> {
        if !self.persist {
            return Ok(());
        }
        let Some(session_file) = &self.session_file else {
            return Ok(());
        };
        let file = std::fs::File::create(session_file)
            .map_err(|error| node_fs_error(error, "open", session_file))?;
        let mut writer = std::io::BufWriter::new(file);
        for entry in &self.file_entries {
            writer.write_all(entry.json_string().as_bytes())?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        Ok(())
    }

    /// `isPersisted()`。
    /// TS: session-manager.ts#SessionManager.isPersisted
    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    /// `getCwd()`。
    /// TS: session-manager.ts#SessionManager.getCwd
    pub fn get_cwd(&self) -> &str {
        &self.cwd
    }

    /// `getSessionDir()`。
    /// TS: session-manager.ts#SessionManager.getSessionDir
    pub fn get_session_dir(&self) -> &str {
        &self.session_dir
    }

    /// `usesDefaultSessionDir()`。
    /// TS: session-manager.ts#SessionManager.usesDefaultSessionDir
    pub fn uses_default_session_dir(&self) -> bool {
        self.session_dir == get_default_session_dir_path(&self.cwd, &default_agent_dir())
    }

    /// `getSessionId()`。
    /// TS: session-manager.ts#SessionManager.getSessionId
    pub fn get_session_id(&self) -> &str {
        &self.session_id
    }

    /// `getSessionFile()`。
    /// TS: session-manager.ts#SessionManager.getSessionFile
    pub fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }

    /// `_hasConversation()`：是否已有 user/assistant 消息。
    ///
    /// 只统计能解析成 typed message 的条目。用于决定是否允许首次落盘：只有 setup 条目
    /// （模型、thinking level、system 提示）的会话留在内存，避免“打开又关闭”留下空文件；
    /// 从 user 消息（而不是首条 assistant 回复）开始保存，保证第一轮即使没有完成，prompt
    /// 也已经落盘。
    fn has_conversation(&self) -> bool {
        self.file_entries.iter().any(|stored| match &stored.entry {
            FileEntry::Entry(SessionEntry::Message(message_entry)) => {
                matches!(message_entry.message.role(), "user" | "assistant")
            }
            _ => false,
        })
    }

    /// `_persist(entry)`：写入最近追加的条目（含 `parentId` 省略覆盖）。
    ///
    /// 两种模式：
    /// - `flushed == false`：还没有成功落盘过。若尚未出现 user/assistant 消息则什么都不做；
    ///   否则用 `create_new`（`wx`）一次写出全部条目并置 `flushed = true`。`create_new`
    ///   在文件已存在时失败（`EEXIST`），不会覆盖别人的文件；
    /// - `flushed == true`：以 append 模式只写最后一条。注意这里用了 `create(true)`：
    ///   文件被外部删除后会被重建为“只含最后一条记录”的文件（没有 header）。
    fn persist_last_entry(&mut self) -> Result<(), SessionManagerError> {
        if !self.persist {
            return Ok(());
        }
        let Some(session_file) = self.session_file.clone() else {
            return Ok(());
        };
        if !self.flushed {
            if !self.has_conversation() {
                return Ok(());
            }
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&session_file)
                .map_err(|error| node_fs_error(error, "open", &session_file))?;
            let mut writer = std::io::BufWriter::new(file);
            for file_entry in &self.file_entries {
                writer
                    .write_all(file_entry.json_string().as_bytes())
                    .map_err(|error| node_fs_error(error, "write", &session_file))?;
                writer
                    .write_all(b"\n")
                    .map_err(|error| node_fs_error(error, "write", &session_file))?;
            }
            writer
                .flush()
                .map_err(|error| node_fs_error(error, "write", &session_file))?;
            self.flushed = true;
        } else {
            let Some(entry) = self.file_entries.last() else {
                return Ok(());
            };
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&session_file)
                .map_err(|error| node_fs_error(error, "open", &session_file))?;
            file.write_all(entry.json_string().as_bytes())
                .map_err(|error| node_fs_error(error, "write", &session_file))?;
            file.write_all(b"\n")
                .map_err(|error| node_fs_error(error, "write", &session_file))?;
        }
        Ok(())
    }

    /// 追加条目：更新内存结构（`file_entries` / `by_id` / leaf），然后落盘。
    ///
    /// 条目的 `parentId` 已由调用方按**追加前**的 leaf 填好。当 leaf 是 `Undefined` 或
    /// 非字符串原始值（`Other`）时，typed 视图只能给出 `None`，直接序列化会写成
    /// `"parentId":null`；为复刻 TS 的 `undefined`（键被 `JSON.stringify` 省略），这里额外
    /// 保存一份去掉 `parentId` 键的原始对象用于落盘。`Other` 的原始值无法写入 typed 字段，
    /// 也只能按 `undefined` 处理（能力边界，见 `LeafPointer`）。
    fn append_entry(&mut self, entry: SessionEntry) -> Result<String, SessionManagerError> {
        let id = entry.id().to_string();
        // TS `parentId: this.leafId`：leaf 为 `undefined` 时该键被 `JSON.stringify` 省略，
        // `null` 时写 `"parentId":null`。typed 视图两者都是 `None`，因此 undefined 情形
        // 保存一份去掉 `parentId` 的原始对象用于落盘。
        let stored = match (
            matches!(self.leaf_id, LeafPointer::Undefined | LeafPointer::Other(_)),
            serde_json::to_value(&entry),
        ) {
            (true, Ok(JsonValue::Object(mut map))) => {
                map.shift_remove("parentId");
                StoredFileEntry {
                    entry: FileEntry::Entry(entry.clone()),
                    raw: Some(kiana_ai::utils::json_parse::json_object_from_map(map)),
                }
            }
            _ => StoredFileEntry::new(FileEntry::Entry(entry.clone())),
        };
        self.file_entries.push(stored);
        self.by_id.insert(id.clone(), entry);
        self.leaf_id = LeafPointer::Id(id.clone());
        self.persist_last_entry()?;
        Ok(id)
    }

    /// 追加消息（`appendMessage`）。
    ///
    /// id 由 `generate_id` 生成（8 位十六进制，对 `by_id` 去重），`parentId` 为当前 leaf，
    /// 时间戳为当前 ISO 时间。返回新条目 id；落盘策略见 `append_entry`。本函数不校验
    /// 消息类型。
    ///
    /// TS: session-manager.ts#SessionManager.appendMessage
    pub fn append_message(&mut self, message: AgentMessage) -> Result<String, SessionManagerError> {
        let entry = SessionEntry::Message(SessionMessageEntry {
            kind: SessionMessageType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            message,
            raw_message: None,
        });
        self.append_entry(entry)
    }

    /// 追加 thinking level 变更（`appendThinkingLevelChange`，不校验取值）。
    /// TS: session-manager.ts#SessionManager.appendThinkingLevelChange
    pub fn append_thinking_level_change(
        &mut self,
        thinking_level: &str,
    ) -> Result<String, SessionManagerError> {
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            kind: ThinkingLevelChangeType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            thinking_level: thinking_level.to_string(),
        });
        self.append_entry(entry)
    }

    /// 追加模型变更（`appendModelChange`）。
    ///
    /// `provider`/`modelId` 原样写入，不校验是否存在于模型目录。返回条目 id。
    ///
    /// TS: session-manager.ts#SessionManager.appendModelChange
    pub fn append_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, SessionManagerError> {
        let entry = SessionEntry::ModelChange(ModelChangeEntry {
            kind: ModelChangeType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        });
        self.append_entry(entry)
    }

    /// 追加用量（`appendUsage`）：返回追加的条目（不是 id）。
    ///
    /// `note` 为空串时按 `None` 处理，序列化时省略该键（TS 的 `...(note ? {note} : {})`）。
    ///
    /// TS: session-manager.ts#SessionManager.appendUsage
    pub fn append_usage(
        &mut self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: Usage,
        note: Option<&str>,
    ) -> Result<UsageEntry, SessionManagerError> {
        let entry = UsageEntry {
            kind: UsageEntryType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            kind_name: kind.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            usage,
            note: note.filter(|note| !note.is_empty()).map(str::to_string),
            raw_entry: None,
        };
        self.append_entry(SessionEntry::Usage(entry.clone()))?;
        Ok(entry)
    }

    /// 追加压缩摘要（`appendCompaction`）。
    ///
    /// 先从当前投影的 `convert_to_llm` 结果里取当前 system 消息（若存在），把它的
    /// `timestamp` 改成与条目时间相同的毫秒值（TS: `new Date(timestamp).getTime()`），
    /// 作为压缩边界处的完整 system 消息。
    ///
    /// `first_kept_entry_id` 为 `None` 时写成自身 id（retain-none：保留区从压缩条目之后
    /// 开始）。`details`/`usage`/`from_hook` 原样写入。返回条目 id。
    ///
    /// TS: session-manager.ts#SessionManager.appendCompaction
    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: Option<&str>,
        tokens_before: f64,
        details: Option<JsonValue>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String, SessionManagerError> {
        let epoch_millis = now_epoch_millis();
        let timestamp = kiana_ai::model_catalog::js_date_to_iso_string(epoch_millis);
        let projection = self.build_session_projection();
        let llm_messages = convert_to_llm(&projection.messages);
        let system_message = kiana_ai::utils::transcript::get_current_system_message(&llm_messages)
            .map(|mut system_message| {
                system_message.timestamp = epoch_millis;
                system_message
            });
        let id = generate_id(|id| self.by_id.contains_key(id));
        let entry = CompactionEntry {
            kind: CompactionType::Marker,
            id: id.clone(),
            parent_id: self.leaf_parent_id(),
            timestamp,
            summary: summary.to_string(),
            first_kept_entry_id: first_kept_entry_id
                .map(str::to_string)
                .unwrap_or_else(|| id.clone()),
            tokens_before,
            details,
            usage,
            from_hook,
            system_message,
            first_kept_entry_index: None,
            raw_entry: None,
        };
        self.append_entry(SessionEntry::Compaction(entry))
    }

    // ------------------------------------------------------------------
    // 第 6 周：压缩提交事务
    // ------------------------------------------------------------------

    /// 记录一次 append 事务的内存回滚点（条目数 + leaf 指针）。
    ///
    /// 只供 [`crate::compaction_support`] 使用：`append_compaction` 先更新内存再落盘，
    /// 落盘失败时用 [`Self::rollback_append_transaction`] 恢复内存状态，保证调用方看到
    /// 的历史与落盘前一致。
    pub(crate) fn append_transaction_marker(&self) -> (usize, LeafPointer) {
        (self.file_entries.len(), self.leaf_id.clone())
    }

    /// 当前内存条目数（含 header 与未知条目）。
    pub(crate) fn entry_count(&self) -> usize {
        self.file_entries.len()
    }

    /// 回滚一次失败的 append：截断 `file_entries`、移除新条目 id、恢复 leaf。
    ///
    /// `marker_len` 由 [`Self::append_transaction_marker`] 提供；只有条目数确实增长时
    /// 才执行回滚，避免把失败前的并发 append 一起撤销。
    pub(crate) fn rollback_append_transaction(
        &mut self,
        marker: (usize, LeafPointer),
        appended_id: Option<&str>,
    ) {
        let (marker_len, previous_leaf) = marker;
        if self.file_entries.len() > marker_len {
            // 先移除 marker 之后所有条目的 id 索引（包括因落盘失败而未返回 id 的条目），
            // 再截断 file_entries；两者必须成对恢复，否则 by_id 会留下悬空条目。
            let appended_ids: Vec<String> = self.file_entries[marker_len..]
                .iter()
                .map(|entry| entry.id().to_string())
                .collect();
            for id in &appended_ids {
                self.by_id.shift_remove(id);
            }
            if let Some(id) = appended_id {
                self.by_id.shift_remove(id);
            }
            self.file_entries.truncate(marker_len);
        }
        self.leaf_id = previous_leaf;
    }

    /// 从 `from_index` 开始最近一次 append 的条目 id。
    pub(crate) fn appended_entry_id(&self, from_index: usize) -> Option<String> {
        self.file_entries
            .get(from_index)
            .map(|entry| entry.id().to_string())
    }

    /// 事务回滚：只从索引移除一个条目；`file_entries` 由
    /// [`Self::rollback_append_transaction`] 统一截断。
    pub(crate) fn remove_appended_entry(&mut self, id: &str) {
        self.by_id.shift_remove(id);
    }

    /// 追加扩展自定义条目（`appendCustomEntry`）。
    ///
    /// 不进入 LLM 上下文；键序是 `type,customType,data?,id,parentId,timestamp`（见
    /// [`CustomEntry`]，与其它条目的 base 前置不同）。返回条目 id。
    ///
    /// TS: session-manager.ts#SessionManager.appendCustomEntry
    pub fn append_custom_entry(
        &mut self,
        custom_type: &str,
        data: Option<JsonValue>,
    ) -> Result<String, SessionManagerError> {
        let entry = CustomEntry {
            kind: CustomEntryType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            custom_type: custom_type.to_string(),
            data,
        };
        self.append_entry(SessionEntry::Custom(entry))
    }

    /// 追加会话元数据（显示名）；换行折叠为空格并 trim。
    ///
    /// 连续的 `\r`/`\n` 折叠成一个空格，再用 JS `trim` 去掉首尾空白；空串会作为
    /// `"name":""` 写入（`get_session_name` 随后把它视为“无名字”，用于清空标题）。
    /// 返回条目 id。
    ///
    /// TS: session-manager.ts#SessionManager.appendSessionInfo
    pub fn append_session_info(&mut self, name: &str) -> Result<String, SessionManagerError> {
        let mut sanitized = String::with_capacity(name.len());
        let mut last_was_break = false;
        for character in name.chars() {
            if character == '\r' || character == '\n' {
                if !last_was_break {
                    sanitized.push(' ');
                    last_was_break = true;
                }
            } else {
                sanitized.push(character);
                last_was_break = false;
            }
        }
        let sanitized = js_trim(&sanitized).to_string();
        let entry = SessionInfoEntry {
            kind: SessionInfoType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            name: Some(sanitized),
        };
        self.append_entry(SessionEntry::SessionInfo(entry))
    }

    /// 当前会话名（反向扫描，遇到第一条 `session_info` 即返回）。
    ///
    /// 反向扫描意味着**最后一条** `session_info` 决定结果；即使它没有 `name` 或值为空，
    /// 也返回 `None`，不会继续向前找更早的名字（空名字=显式清空标题）。
    ///
    /// 表示能力边界：TS 的 `entry.name?.trim()` 在 `name` 是非字符串（数字/布尔/对象）时抛
    /// TypeError；Rust 的返回类型没有错误通道，这类条目按「没有名字」返回 `None`
    /// （`null`/缺失与 TS 一样得到 `undefined`）。
    ///
    /// TS: session-manager.ts#SessionManager.getSessionName
    pub fn get_session_name(&self) -> Option<String> {
        for stored in self.file_entries.iter().rev() {
            match &stored.entry {
                FileEntry::Entry(SessionEntry::SessionInfo(entry)) => {
                    return entry
                        .name
                        .as_deref()
                        .map(js_trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string);
                }
                FileEntry::Entry(SessionEntry::Unknown(object))
                    if stored.kind() == "session_info" =>
                {
                    return object
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .map(js_trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string);
                }
                _ => {}
            }
        }
        None
    }

    /// 追加参与 LLM 上下文的自定义消息条目（`appendCustomMessageEntry`）。
    ///
    /// `content` 原样保存（字符串或内容块数组；`null` 在投影时变成 `[]`），`display`
    /// 控制 TUI 是否显示，`details` 只供扩展读取（不进入 LLM）。返回条目 id。
    ///
    /// TS: session-manager.ts#SessionManager.appendCustomMessageEntry
    pub fn append_custom_message_entry(
        &mut self,
        custom_type: &str,
        content: JsonValue,
        display: bool,
        details: Option<JsonValue>,
    ) -> Result<String, SessionManagerError> {
        let entry = CustomMessageEntry {
            kind: CustomMessageType::Marker,
            custom_type: custom_type.to_string(),
            content,
            display,
            details,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            raw_entry: None,
        };
        self.append_entry(SessionEntry::CustomMessage(entry))
    }

    /// 追加 context edit（`appendContextEdit`）。
    ///
    /// 校验顺序与错误：
    /// 1. `replacement` 为 `Some` 时其 `content` 必须是字符串或数组，否则
    ///    `InvalidContextEditReplacement`（先于目标查询）；
    /// 2. 目标必须存在于 `by_id`，否则 `EntryNotFound`；
    /// 3. 目标必须在当前分支上，否则 `NotOnActiveBranch`；
    /// 4. 目标必须是 `custom_message` 或 user/assistant/toolResult 的 message，否则
    ///    `NotEditable`。
    /// 目标角色是 assistant/toolResult 且替换内容是字符串时，追加前就转成单个 text 块；
    /// 投影侧对导入的旧数据做同样的转换。
    ///
    /// 表示能力边界：目标条目是 `type: "message"` 但 `message` 为 `null`/非对象时，TS 读取
    /// `target.message.role` 会抛 TypeError；Rust 的该条目是 [`SessionEntry::Unknown`]，
    /// 返回 `Entry {id} does not contribute editable model content`。
    ///
    /// TS: session-manager.ts#SessionManager.appendContextEdit
    pub fn append_context_edit(
        &mut self,
        target_id: &str,
        replacement: Option<ContextEditReplacement>,
    ) -> Result<String, SessionManagerError> {
        if let Some(replacement) = &replacement
            && !matches!(
                replacement.content,
                JsonValue::String(_) | JsonValue::Array(_)
            )
        {
            return Err(SessionManagerError::InvalidContextEditReplacement);
        }
        let Some(target) = self.by_id.get(target_id).cloned() else {
            return Err(SessionManagerError::EntryNotFound {
                id: target_id.to_string(),
            });
        };
        if !self
            .get_branch(None)
            .iter()
            .any(|entry| entry.id() == target_id)
        {
            return Err(SessionManagerError::NotOnActiveBranch {
                id: target_id.to_string(),
            });
        }
        let editable = match &target {
            SessionEntry::CustomMessage(_) => true,
            SessionEntry::Message(entry) => {
                matches!(entry.message.role(), "user" | "assistant" | "toolResult")
            }
            _ => false,
        };
        if !editable {
            return Err(SessionManagerError::NotEditable {
                id: target_id.to_string(),
            });
        }
        let target_role = match &target {
            SessionEntry::Message(entry) => entry.message.role().to_string(),
            _ => "custom".to_string(),
        };
        let replacement = match replacement {
            Some(mut replacement)
                if (target_role == "assistant" || target_role == "toolResult")
                    && replacement.content.is_string() =>
            {
                let text = replacement.content.as_str().unwrap_or_default().to_string();
                replacement.content = text_block_array(&text);
                Some(replacement)
            }
            other => other,
        };
        let entry = ContextEditEntry {
            kind: ContextEditType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            target_id: target_id.to_string(),
            replacement,
            raw_entry: None,
        };
        self.append_entry(SessionEntry::ContextEdit(entry))
    }

    /// 设置 / 清除标签（`appendLabelChange`）。
    ///
    /// 目标必须存在于 `by_id`（否则 `EntryNotFound`）。`label` 为 `None` 或空串都表示清除；
    /// 非空时更新 `labels_by_id` 与 `label_timestamps_by_id`。标签本身也是一条树条目，会
    /// 被写入文件并参与路径遍历，但投影不产生消息。返回条目 id。
    ///
    /// TS: session-manager.ts#SessionManager.appendLabelChange
    pub fn append_label_change(
        &mut self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, SessionManagerError> {
        if !self.by_id.contains_key(target_id) {
            return Err(SessionManagerError::EntryNotFound {
                id: target_id.to_string(),
            });
        }
        let entry = LabelEntry {
            kind: LabelType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: self.leaf_parent_id(),
            timestamp: now_iso_string(),
            target_id: target_id.to_string(),
            label: label.map(str::to_string),
        };
        let entry_timestamp = entry.timestamp.clone();
        let id = self.append_entry(SessionEntry::Label(entry))?;
        match label {
            Some(label) if !label.is_empty() => {
                self.labels_by_id
                    .insert(target_id.to_string(), label.to_string());
                self.label_timestamps_by_id.insert(
                    target_id.to_string(),
                    Some(JsonValue::String(entry_timestamp)),
                );
            }
            _ => {
                self.labels_by_id.shift_remove(target_id);
                self.label_timestamps_by_id.shift_remove(target_id);
            }
        }
        Ok(id)
    }

    /// `getLeafId()`。
    ///
    /// 只有 leaf 是字符串 id 时返回 `Some`；`null`、`undefined` 与非字符串原始值都返回
    /// `None`。需要区分布局细节或原样导出时用 [`SessionManager::get_leaf_id_json`]。
    /// TS: session-manager.ts#SessionManager.getLeafId
    pub fn get_leaf_id(&self) -> Option<&str> {
        self.leaf_id_string()
    }

    /// `getLeafEntry()`。
    ///
    /// TS 的 `this.leafId ? ... : undefined`：空串也是假值，因此空 id 返回 `None`；
    /// `null`/`undefined`/非字符串 leaf 同样取不到（`by_id` 只有字符串键）。
    /// TS: session-manager.ts#SessionManager.getLeafEntry
    pub fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        // TS: `this.leafId ? this.byId.get(this.leafId) : undefined`（空串也是假值）。
        self.leaf_id_string()
            .filter(|leaf_id| !leaf_id.is_empty())
            .and_then(|leaf_id| self.by_id.get(leaf_id))
    }

    /// `getEntry(id)`。
    ///
    /// 按字符串 id 精确查找；非字符串 id 的条目（`LeafPointer::Other`）查不到。
    /// TS: session-manager.ts#SessionManager.getEntry
    pub fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.by_id.get(id)
    }

    /// `getChildren(parentId)`（`byId` 插入顺序）。
    ///
    /// `by_id` 只收录 id 为字符串的对象条目，因此非字符串 id / 非对象原始值不会出现在
    /// 结果里。线性扫描 `by_id`，复杂度 O(条目数)。
    /// TS: session-manager.ts#SessionManager.getChildren
    pub fn get_children(&self, parent_id: &str) -> Vec<SessionEntry> {
        self.by_id
            .values()
            .filter(|entry| entry.parent_id() == Some(parent_id))
            .cloned()
            .collect()
    }

    /// `getLabel(id)`。
    ///
    /// 值来自最后一条真值 `label` 条目；索引阶段会把非字符串 label 按 JS `String(value)`
    /// 转成字符串，因此这里总是 `&str`。
    /// TS: session-manager.ts#SessionManager.getLabel
    pub fn get_label(&self, id: &str) -> Option<&str> {
        self.labels_by_id.get(id).map(String::as_str)
    }

    /// `getBranch(fromId?)`。
    ///
    /// 从 `from_id`（缺省为当前 leaf）沿 `parentId` 回溯到根，返回 root→leaf 顺序的
    /// **克隆**；起点不存在或为空串时返回空 `Vec`。包含 label/custom 等所有条目类型
    /// （header 不在 `by_id` 中），父链断开处停止且不报错。
    /// TS: session-manager.ts#SessionManager.getBranch
    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        let start_id = from_id.or_else(|| self.leaf_id_string());
        // `startId` 为 undefined / 空串（`this.leafId` 可能是 `""`）时返回空路径。
        let Some(start_id) = start_id.filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        let Some(mut current) = self.by_id.get(start_id) else {
            return Vec::new();
        };
        let mut path: Vec<SessionEntry> = Vec::new();
        loop {
            path.push(current.clone());
            current = match current.parent_id() {
                Some(parent_id) if !parent_id.is_empty() => match self.by_id.get(parent_id) {
                    Some(parent) => parent,
                    None => break,
                },
                _ => break,
            };
        }
        path.reverse();
        path
    }

    /// 分支上最近一条 assistant 消息的 prompt token 总数。
    ///
    /// 与 `get_branch(None)` 后倒序查找结果相同，但只沿 `by_id` 的 parent 链回溯、
    /// 不克隆任何条目；cache-warmer 的 `status()` 每次查询都会调用它。
    pub fn last_prompt_tokens(&self) -> u64 {
        let Some(start_id) = self.leaf_id_string().filter(|id| !id.is_empty()) else {
            return 0;
        };
        let mut current = self.by_id.get(start_id);
        while let Some(entry) = current {
            if let SessionEntry::Message(message_entry) = entry
                && let Some(message) = message_entry.message.as_assistant()
            {
                return message.usage.input + message.usage.cache_read + message.usage.cache_write;
            }
            current = match entry.parent_id() {
                Some(parent_id) if !parent_id.is_empty() => self.by_id.get(parent_id),
                _ => None,
            };
        }
        0
    }

    /// 分支条目的原始 JSON（`serializeSessionBranch` 展开 `{ ...entry }` 用）。
    ///
    /// TS 的 `getBranch()` 返回的是 `JSON.parse` 得到的对象引用；导出时未知字段与键序
    /// 逐字保留。这里返回与 [`SessionManager::get_branch`] 相同路径的原始值。
    /// TS: session-manager.ts#SessionManager.getBranch
    pub fn get_branch_json(&self, from_id: Option<&str>) -> Vec<JsonValue> {
        let mut raw_by_id: IndexMap<&str, &StoredFileEntry> = IndexMap::new();
        for stored in &self.file_entries {
            if stored.kind() == "session" {
                continue;
            }
            // 同 id 后者覆盖（与 `byId.set` 一致）。
            raw_by_id.insert(stored.id(), stored);
        }
        let start_id = from_id.or_else(|| self.leaf_id_string());
        let Some(start_id) = start_id.filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        let Some(mut current) = self.by_id.get(start_id) else {
            return Vec::new();
        };
        let mut path: Vec<&SessionEntry> = Vec::new();
        loop {
            path.push(current);
            current = match current.parent_id() {
                Some(parent_id) if !parent_id.is_empty() => match self.by_id.get(parent_id) {
                    Some(parent) => parent,
                    None => break,
                },
                _ => break,
            };
        }
        path.reverse();
        path.into_iter()
            .map(|entry| match raw_by_id.get(entry.id()) {
                Some(stored) => stored.to_json_value(),
                None => serde_json::to_value(entry).unwrap_or(JsonValue::Null),
            })
            .collect()
    }

    /// `buildContextEntries()`。
    ///
    /// 使用当前 leaf 与 `by_id` 调用自由函数
    /// [`crate::session_manager::build_context_entries`]。leaf 的映射：`null` → 空路径；
    /// `undefined`/非字符串/空串 → 回退到最后一个条目。
    /// TS: session-manager.ts#SessionManager.buildContextEntries
    pub fn build_context_entries(&self) -> Vec<SessionEntry> {
        build_context_entries(
            &self.get_entries_ref(),
            self.leaf_selection(),
            Some(&self.by_id),
        )
    }

    /// `buildSessionProjection()`。
    ///
    /// 使用当前 leaf 与 `by_id` 调用自由函数
    /// [`crate::session_manager::build_session_projection`]；这是会话运行时构造 LLM
    /// 输入的入口。
    /// TS: session-manager.ts#SessionManager.buildSessionProjection
    pub fn build_session_projection(&self) -> SessionProjection {
        build_session_projection(
            &self.get_entries_ref(),
            self.leaf_selection(),
            Some(&self.by_id),
        )
    }

    /// `buildSessionContext()`。
    ///
    /// 只保留投影的 `messages` / `thinking_level` / `model` 三个字段；需要每条消息的来源
    /// 条目时用 [`SessionManager::build_session_projection`]。
    /// TS: session-manager.ts#SessionManager.buildSessionContext
    pub fn build_session_context(&self) -> SessionContext {
        let projection = self.build_session_projection();
        SessionContext {
            messages: projection.messages,
            thinking_level: projection.thinking_level,
            model: projection.model,
        }
    }

    /// `getLeafId()` 的**原始 JSON 视图**（HTML 导出用）。
    ///
    /// TS 的 `leafId` 可能是 `null`（新会话 / `branch(null)` / 显式 `id: null`）、
    /// 任意非字符串原始值（数字/布尔/对象/数组，原样写出）或 `undefined`（最后一条
    /// 条目的 `id` 缺省）；`JSON.stringify` 会把 `undefined` 的键**整个省略**。
    /// `None` 即 TS 的 `undefined`。
    /// TS: session-manager.ts#SessionManager.getLeafId
    pub fn get_leaf_id_json(&self) -> Option<JsonValue> {
        match &self.leaf_id {
            LeafPointer::Null => Some(JsonValue::Null),
            LeafPointer::Undefined => None,
            LeafPointer::Other(value) => Some(value.clone()),
            LeafPointer::Id(id) => Some(JsonValue::String(id.clone())),
        }
    }

    /// TS `this.leafId` 是具体字符串 id 时返回它（`null`/`undefined`/其它原始值 → `None`）。
    fn leaf_id_string(&self) -> Option<&str> {
        match &self.leaf_id {
            LeafPointer::Id(id) => Some(id),
            LeafPointer::Null | LeafPointer::Undefined | LeafPointer::Other(_) => None,
        }
    }

    /// TS `parentId: this.leafId` 的 typed 视图（`null`/`undefined` → `None`）。
    fn leaf_parent_id(&self) -> Option<String> {
        self.leaf_id_string().map(str::to_string)
    }

    fn leaf_selection(&self) -> LeafId<'_> {
        // TS `buildSessionPath(entries, this.leafId)`：`null` → 空路径；`undefined`（或空串）
        // → 回退到最后一个条目。
        match &self.leaf_id {
            LeafPointer::Null => LeafId::Null,
            LeafPointer::Undefined | LeafPointer::Other(_) => LeafId::Unspecified,
            LeafPointer::Id(id) if id.is_empty() => LeafId::Unspecified,
            LeafPointer::Id(id) => LeafId::Id(id),
        }
    }

    /// `getEntries()` 的内部实现：跳过 header 与非对象原始值，克隆其余对象条目。
    ///
    /// 名字里的“借用版本”指它避免把 header/原始值也克隆进 `Vec<SessionEntry>`；对象条目
    /// 本身仍是克隆（调用方拿到的不是只读引用）。
    fn get_entries_ref(&self) -> Vec<SessionEntry> {
        self.file_entries
            .iter()
            .filter_map(|stored| match &stored.entry {
                FileEntry::Entry(entry) if entry.kind() != "session" => Some(entry.clone()),
                _ => None,
            })
            .collect()
    }

    /// `getHeader()`。
    ///
    /// 只返回能解析成 [`SessionHeader`] 的条目。header 行字段畸形（例如 `id` 是数字）时
    /// 该条目会落成 [`SessionEntry::Unknown`]（`kind()` 仍为 `"session"`），本方法返回
    /// `None`，而 [`SessionManager::get_header_json`] 仍能返回原始对象。
    /// TS: session-manager.ts#SessionManager.getHeader
    pub fn get_header(&self) -> Option<&SessionHeader> {
        self.file_entries
            .iter()
            .find_map(StoredFileEntry::as_header)
    }

    /// `getEntryCount()`。
    ///
    /// 等于 TS 的 `byId.size`：字符串 id 去重数 + `undefined` 键（只算一次）+ 其它非字符串
    /// 键的 SameValueZero 去重数。非对象行与缺 id 的条目只贡献 `undefined` 键，非字符串 id
    /// 的条目只贡献 `other_index_keys`，而 [`SessionManager::get_entries`] 会包含这些对象
    /// 条目、却不包含非对象值，因此两个数字没有固定的大小关系。
    /// TS: session-manager.ts#SessionManager.getEntryCount
    pub fn get_entry_count(&self) -> usize {
        // `byId.size`：字符串 id 的不同键 + 非字符串 id 的不同键（`undefined` 只算一次）。
        self.by_id.len() + usize::from(self.undefined_index_key) + self.other_index_keys.len()
    }

    /// `getEntries()`（浅拷贝；不含 header）。
    ///
    /// 只包含能表示成 [`SessionEntry`] 的对象条目：文件里的非对象原始值（数组/字符串/
    /// 数字/`true`）不会出现（TS 的 `getEntries()` 会包含它们，这是 typed 视图的能力
    /// 边界）。需要逐字保真时用 [`SessionManager::get_entries_json`]。
    /// TS: session-manager.ts#SessionManager.getEntries
    pub fn get_entries(&self) -> Vec<SessionEntry> {
        self.get_entries_ref()
    }

    /// `getEntries()` 的**原始 JSON 视图**（HTML 导出用）。
    ///
    /// TS 的 `getEntries()` 返回 `JSON.parse` 得到的普通对象数组，导出时未知字段与键序
    /// 逐字保留（见模块文档）。typed [`SessionEntry`] 无法承载未建模字段，因此导出这类
    /// 需要逐字节一致的消费方必须用本方法。
    ///
    /// 与 TS 的过滤规则一致：只排除 `type === "session"` 的条目（header）；文件里的非对象
    /// 真值（数组/字符串/数字/`true`）保留在结果中。
    /// TS: session-manager.ts#SessionManager.getEntries
    pub fn get_entries_json(&self) -> Vec<JsonValue> {
        self.file_entries
            .iter()
            .filter(|stored| stored.kind() != "session")
            .map(StoredFileEntry::to_json_value)
            .collect()
    }

    /// `getHeader()` 的**原始 JSON 视图**（无 header 时为 `null`）。
    ///
    /// 与 [`SessionManager::get_header`] 的 typed 视图不同，这里保留未知字段与键序；
    /// `find` 语义与 TS 相同（第一个 `type === "session"` 的条目）。即使该条目无法解析成
    /// typed header，也会返回它的原始对象。
    /// TS: session-manager.ts#SessionManager.getHeader
    pub fn get_header_json(&self) -> JsonValue {
        self.file_entries
            .iter()
            .find(|stored| stored.kind() == "session")
            .map_or(JsonValue::Null, StoredFileEntry::to_json_value)
    }

    /// `getTree()`。
    ///
    /// 返回森林（正常会话只有一个根）：`parentId === null` 或 `parentId === entry.id` 的
    /// 条目是根，父链断开的孤立条目也作为根返回。children 按解析后的 `timestamp` 升序
    /// （`NaN` 相等，稳定排序），根保持文件顺序。
    ///
    /// 只用能表示成 [`SessionEntry`] 的对象条目建节点：文件里的非对象原始值被丢弃
    /// （TS 会为它们建 `id: undefined` 的节点）。父节点查找按 id 后写覆盖先写（与
    /// `Map.set` 一致），但每个条目仍各自成为一个节点、按自己的 `parentId` 挂载。
    /// 构建使用显式栈（避免深树递归爆栈），并用 `visited` 防止环形 `parentId` 死循环。
    /// TS: session-manager.ts#SessionManager.getTree
    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        // TS 遍历 `getEntries()`（含非对象值）；Rust 的 [`SessionEntry`] 无法表示非对象值，
        // 因此只为对象条目建节点（非对象值生成的 `undefined` 键节点会丢失，见模块文档）。
        let entries: Vec<&StoredFileEntry> = self
            .file_entries
            .iter()
            .filter(|stored| matches!(stored.entry, FileEntry::Entry(_)))
            // `getEntries()` 过滤 `e.type !== "session"`（包括解析失败的 session 条目）。
            .filter(|stored| stored.kind() != "session")
            .collect();
        let mut nodes: Vec<SessionTreeNode> = entries
            .iter()
            .map(|stored| {
                let entry = match &stored.entry {
                    FileEntry::Entry(entry) => entry,
                    FileEntry::Header(_) | FileEntry::Raw(_) => unreachable!("filtered to entries"),
                };
                let id = stored.string_id();
                SessionTreeNode {
                    entry: entry.clone(),
                    children: Vec::new(),
                    // TS `labelsById.get(entry.id)`：非字符串 id 不会命中字符串键。
                    label: id.and_then(|id| self.labels_by_id.get(id)).cloned(),
                    label_timestamp: id
                        .and_then(|id| self.label_timestamps_by_id.get(id))
                        .and_then(|value| value.as_ref())
                        .map(|value| js_to_string(Some(value))),
                }
            })
            .collect();

        // TS 的 `nodeMap` 以 `entry.id` 原值为键。Rust 只保留字符串键与 `undefined` 键
        // （`None`）；同键后者覆盖前者，与 `Map.set` 一致。
        let mut index_by_id: IndexMap<Option<String>, usize> = IndexMap::new();
        for (index, stored) in entries.iter().enumerate() {
            index_by_id.insert(stored.string_id().map(str::to_string), index);
        }

        let mut roots: Vec<usize> = Vec::new();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); entries.len()];
        for (index, stored) in entries.iter().enumerate() {
            let entry_key = stored.string_id();
            // TS：`parentId === null || parentId === entry.id` → root。
            let is_root = match stored.parent_id_ref() {
                ParentIdRef::Null => true,
                // 字符串 parentId 只能与字符串 id 相等（`undefined === undefined` 在下方处理）。
                ParentIdRef::Str(parent_id) => Some(parent_id) == entry_key,
                // 非字符串 parentId 无法命中字符串键；索引也无法表示原始非字符串键。
                ParentIdRef::Other => false,
                // 缺省 parentId（`undefined`）：`undefined === undefined` 时是根。
                ParentIdRef::Undefined => entry_key.is_none(),
            };
            if is_root {
                roots.push(index);
                continue;
            }
            // `nodeMap.get(entry.parentId)`：缺省时查 `undefined` 键节点，非字符串键无法表示。
            let parent_index = match stored.parent_id_ref() {
                ParentIdRef::Undefined => index_by_id.get(&None).copied(),
                ParentIdRef::Str(parent_id) => {
                    index_by_id.get(&Some(parent_id.to_string())).copied()
                }
                ParentIdRef::Null | ParentIdRef::Other => None,
            };
            match parent_index {
                Some(parent_index) => children[parent_index].push(index),
                None => roots.push(index),
            }
        }

        // children 按解析后的 timestamp 升序（NaN 视为相等，稳定排序）。
        for child_indices in children.iter_mut() {
            child_indices.sort_by(|a, b| {
                let a_time = entries[*a]
                    .timestamp()
                    .map(js_date_time)
                    .unwrap_or(f64::NAN);
                let b_time = entries[*b]
                    .timestamp()
                    .map(js_date_time)
                    .unwrap_or(f64::NAN);
                if a_time.is_nan() || b_time.is_nan() {
                    std::cmp::Ordering::Equal
                } else {
                    a_time
                        .partial_cmp(&b_time)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            });
        }

        // 显式栈的后序构建（避免深树递归）；环形 parentId 通过 visited 保护。
        let mut arena: Vec<Option<SessionTreeNode>> = nodes.drain(..).map(Some).collect();
        let mut visited = vec![false; entries.len()];
        let mut stack: Vec<(usize, bool)> = roots.iter().map(|index| (*index, false)).collect();
        while let Some((index, expanded)) = stack.pop() {
            if expanded {
                let child_nodes: Vec<SessionTreeNode> = children[index]
                    .iter()
                    .filter_map(|child| arena[*child].take())
                    .collect();
                if let Some(node) = arena[index].as_mut() {
                    node.children = child_nodes;
                }
                continue;
            }
            if visited[index] {
                continue;
            }
            visited[index] = true;
            stack.push((index, true));
            for child in children[index].iter().rev() {
                if !visited[*child] {
                    stack.push((*child, false));
                }
            }
        }

        let mut tree: Vec<SessionTreeNode> = Vec::with_capacity(roots.len());
        for root in roots {
            if let Some(node) = arena[root].take() {
                tree.push(node);
            }
        }
        tree
    }

    /// `branch(branchFromId)`。
    ///
    /// 只移动 leaf 指针，不修改/删除任何条目，也不立即写盘；下一次 append 才会产生新
    /// 分支。目标必须在 `by_id` 中（非字符串 id 查不到），否则返回 `EntryNotFound`。
    /// TS: session-manager.ts#SessionManager.branch
    pub fn branch(&mut self, branch_from_id: &str) -> Result<(), SessionManagerError> {
        if !self.by_id.contains_key(branch_from_id) {
            return Err(SessionManagerError::EntryNotFound {
                id: branch_from_id.to_string(),
            });
        }
        self.leaf_id = LeafPointer::Id(branch_from_id.to_string());
        Ok(())
    }

    /// `resetLeaf()`。
    ///
    /// 把 leaf 置为 `null`：下一次 append 产生 `parentId: null` 的新根条目（用于重新编辑
    /// 第一条消息）。不写盘。
    /// TS: session-manager.ts#SessionManager.resetLeaf
    pub fn reset_leaf(&mut self) {
        self.leaf_id = LeafPointer::Null;
    }

    /// `branchWithSummary(branchFromId, summary, details?, fromHook?, usage?)`。
    ///
    /// 先把 leaf 移到 `branch_from_id`（`None` 表示根之前），再追加一条 `branch_summary`
    /// 条目；其 `from_id` 记录**移动前**的 leaf：`null`/`undefined`/非字符串时写
    /// `"root"`，字符串（含空串）原样保留。目标不存在时返回 `EntryNotFound`，且不会改动
    /// leaf。返回新条目 id。
    /// TS: session-manager.ts#SessionManager.branchWithSummary
    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<JsonValue>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String, SessionManagerError> {
        if let Some(branch_from_id) = branch_from_id
            && !self.by_id.contains_key(branch_from_id)
        {
            return Err(SessionManagerError::EntryNotFound {
                id: branch_from_id.to_string(),
            });
        }
        // TS: `const fromId = this.leafId ?? "root"`（空串保留，`null`/`undefined` 用 "root"）。
        let from_id = match &self.leaf_id {
            LeafPointer::Id(id) => id.clone(),
            LeafPointer::Null | LeafPointer::Undefined | LeafPointer::Other(_) => {
                "root".to_string()
            }
        };
        self.leaf_id = match branch_from_id {
            Some(id) => LeafPointer::Id(id.to_string()),
            None => LeafPointer::Null,
        };
        let entry = BranchSummaryEntry {
            kind: BranchSummaryType::Marker,
            id: generate_id(|id| self.by_id.contains_key(id)),
            parent_id: branch_from_id.map(str::to_string),
            timestamp: now_iso_string(),
            from_id,
            summary: summary.to_string(),
            details,
            usage,
            from_hook,
            raw_entry: None,
        };
        self.append_entry(SessionEntry::BranchSummary(entry))
    }

    /// `createBranchedSession(leafId)`。
    ///
    /// 把从根到 `leafId` 的路径（`get_branch(Some(leafId))`）抽成一个新会话；当前 leaf
    /// 不参与路径选择：
    /// 1. `get_branch(leafId)` 为空 → `EntryNotFound`（路径为空意味着 `leafId` 不在
    ///    `by_id` 中）；
    /// 2. 移除路径上的 label 条目，并把它们之后的条目重新串成 parent 链（否则被删 label
    ///    的子节点会变孤儿）；`firstKeptEntryId` 若指向被删 label，改指 label 之后第一条
    ///    保留条目，retain-none 的自引用保持不变；
    /// 3. 每个条目用原始对象回放（保留未知字段/键序），只覆盖 `parentId`（以及上述
    ///    `firstKeptEntryId`）；
    /// 4. 新 header 的 `parentSession` 在持久化时指向旧 `session_file`；
    /// 5. 为仍在新路径上的目标重建 label 条目，按 `labels_by_id` 的插入顺序串在路径末尾。
    ///
    /// 返回：持久化时返回新文件路径——若路径上还没有 user/assistant 消息，文件暂不创建；
    /// 要等下一次追加产生 user/assistant 消息（触发 `has_conversation`）时才写盘，仅追加
    /// setup 条目（model/thinking level/label 等）不会创建文件。内存态返回 `None`，并直接
    /// 替换当前内存会话。
    /// 新会话 id 由 [`create_session_id`] 生成（通常 UUIDv7，生成失败回退 v4 字符串）。
    /// TS: session-manager.ts#SessionManager.createBranchedSession
    pub fn create_branched_session(
        &mut self,
        leaf_id: &str,
    ) -> Result<Option<String>, SessionManagerError> {
        let previous_session_file = self.session_file.clone();
        let path = self.get_branch(Some(leaf_id));
        if path.is_empty() {
            return Err(SessionManagerError::EntryNotFound {
                id: leaf_id.to_string(),
            });
        }

        // 原始对象按 id 索引：TS 的 `{ ...entry, parentId }` 在解析对象上展开，
        // 未知字段与键序保留；新键追加到末尾。
        let raw_by_id: IndexMap<String, JsonObject> = self
            .file_entries
            .iter()
            .filter(|stored| stored.kind() != "session")
            .filter_map(|stored| match stored.to_json_value() {
                JsonValue::Object(map) => Some((
                    stored.id().to_string(),
                    kiana_ai::utils::json_parse::json_object_from_map(map),
                )),
                _ => None,
            })
            .collect();

        // 过滤 label 条目并重新串链；被移除 label 之后的第一个非 label 条目成为替换目标。
        let mut path_without_labels: Vec<StoredFileEntry> = Vec::new();
        let mut replacement_by_label_id: IndexMap<String, String> = IndexMap::new();
        let mut pending_label_ids: Vec<String> = Vec::new();
        let mut path_parent_id: Option<String> = None;
        for entry in &path {
            if let SessionEntry::Label(_) = entry {
                pending_label_ids.push(entry.id().to_string());
                continue;
            }
            for label_id in pending_label_ids.drain(..) {
                replacement_by_label_id.insert(label_id, entry.id().to_string());
            }
            let mut clone = entry.clone();
            let mut raw = raw_by_id.get(entry.id()).cloned().unwrap_or_else(|| {
                match serde_json::to_value(entry) {
                    Ok(JsonValue::Object(map)) => {
                        kiana_ai::utils::json_parse::json_object_from_map(map)
                    }
                    _ => JsonObject::new(),
                }
            });
            let parent_id_value = match &path_parent_id {
                Some(id) => JsonValue::String(id.clone()),
                None => JsonValue::Null,
            };
            if let SessionEntry::Compaction(compaction) = &mut clone {
                compaction.parent_id = path_parent_id.clone();
                raw.insert("parentId".to_string(), parent_id_value);
                if compaction.first_kept_entry_id == compaction.id {
                    // retain-none：保持自引用。
                } else if let Some(replacement) =
                    replacement_by_label_id.get(&compaction.first_kept_entry_id)
                {
                    compaction.first_kept_entry_id = replacement.clone();
                    raw.insert(
                        "firstKeptEntryId".to_string(),
                        JsonValue::String(replacement.clone()),
                    );
                }
            } else {
                clone.set_parent_id(path_parent_id.clone());
                raw.insert("parentId".to_string(), parent_id_value);
            }
            path_without_labels.push(StoredFileEntry {
                entry: FileEntry::Entry(clone),
                raw: Some(raw),
            });
            path_parent_id = Some(entry.id().to_string());
        }

        // 新会话 id/时间戳总由本函数生成；`parentSession` 只在持久化时记录旧文件，
        // 内存态保持 undefined（序列化时省略键）。
        let new_session_id = create_session_id();
        let timestamp = now_iso_string();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file = path_join(&[
            self.get_session_dir(),
            &format!("{file_timestamp}_{new_session_id}.jsonl"),
        ]);
        let header = SessionHeader {
            kind: SessionTypeMarker::Marker,
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id.clone(),
            timestamp: Some(timestamp),
            cwd: Some(self.cwd.clone()),
            parent_session: if self.persist {
                previous_session_file.clone()
            } else {
                None
            },
        };

        // 收集路径上条目的标签（保留最新值与时间戳）。
        let path_entry_ids: std::collections::HashSet<String> = path_without_labels
            .iter()
            .map(|stored| stored.entry.id().to_string())
            .collect();
        let mut labels_to_write: Vec<(String, String, Option<JsonValue>)> = Vec::new();
        for (target_id, label) in &self.labels_by_id {
            if path_entry_ids.contains(target_id) {
                labels_to_write.push((
                    target_id.clone(),
                    label.clone(),
                    self.label_timestamps_by_id
                        .get(target_id)
                        .cloned()
                        .flatten(),
                ));
            }
        }

        // 重建的 label 条目也要占用 id：`id_set` 同时包含路径条目 id 与已生成的 label id，
        // 避免 `generate_id` 与它们冲突。
        let mut id_set: std::collections::HashSet<String> = path_entry_ids.clone();
        if self.persist {
            let mut parent_id = path_without_labels
                .last()
                .map(|stored| stored.entry.id().to_string());
            let mut label_entries: Vec<StoredFileEntry> = Vec::new();
            for (target_id, label, label_timestamp) in labels_to_write {
                let id = generate_id(|id| id_set.contains(id));
                id_set.insert(id.clone());
                let stored = branched_label_entry(
                    id.clone(),
                    parent_id.clone(),
                    label_timestamp,
                    target_id,
                    label,
                );
                parent_id = Some(id);
                label_entries.push(stored);
            }

            let mut file_entries: Vec<StoredFileEntry> =
                Vec::with_capacity(2 + path_without_labels.len());
            file_entries.push(StoredFileEntry::new(FileEntry::Header(header)));
            file_entries.extend(path_without_labels);
            file_entries.extend(label_entries);
            self.file_entries = file_entries;
            self.session_id = new_session_id;
            self.session_file = Some(new_session_file.clone());
            self.build_index();

            // 与 `_persist()` 相同规则：路径上已有对话才立即写盘。
            if self.has_conversation() {
                self.rewrite_file()?;
                self.flushed = true;
            } else {
                self.flushed = false;
            }
            return Ok(Some(new_session_file));
        }

        // 内存态：替换当前会话为路径 + 标签。
        let mut parent_id = path_without_labels
            .last()
            .map(|stored| stored.entry.id().to_string());
        let mut label_entries: Vec<StoredFileEntry> = Vec::new();
        for (target_id, label, label_timestamp) in labels_to_write {
            let id = generate_id(|id| id_set.contains(id));
            id_set.insert(id.clone());
            let stored = branched_label_entry(
                id.clone(),
                parent_id.clone(),
                label_timestamp,
                target_id,
                label,
            );
            parent_id = Some(id);
            label_entries.push(stored);
        }
        let mut file_entries: Vec<StoredFileEntry> =
            Vec::with_capacity(2 + path_without_labels.len());
        file_entries.push(StoredFileEntry::new(FileEntry::Header(header)));
        file_entries.extend(path_without_labels);
        file_entries.extend(label_entries);
        self.file_entries = file_entries;
        self.session_id = new_session_id;
        self.build_index();
        Ok(None)
    }
}

/// `createBranchedSession` 重建的 label 条目。
///
/// TS 的对象字面量是 `{ type, id, parentId, timestamp, targetId, label }`；来源 label 没有
/// `timestamp` 时 `this.labelTimestampsById.get(targetId)!` 是 `undefined`，
/// `JSON.stringify` 会省略该键（其余原始值原样保留）。typed 视图只能承载字符串，
/// 因此逐字回放由 `raw` 负责。
fn branched_label_entry(
    id: String,
    parent_id: Option<String>,
    timestamp: Option<JsonValue>,
    target_id: String,
    label: String,
) -> StoredFileEntry {
    let mut raw = JsonObject::new();
    raw.insert("type".to_string(), JsonValue::String("label".to_string()));
    raw.insert("id".to_string(), JsonValue::String(id.clone()));
    raw.insert(
        "parentId".to_string(),
        match &parent_id {
            Some(parent_id) => JsonValue::String(parent_id.clone()),
            None => JsonValue::Null,
        },
    );
    if let Some(timestamp) = &timestamp {
        raw.insert("timestamp".to_string(), timestamp.clone());
    }
    raw.insert("targetId".to_string(), JsonValue::String(target_id.clone()));
    raw.insert("label".to_string(), JsonValue::String(label.clone()));
    let entry = LabelEntry {
        kind: LabelType::Marker,
        id,
        parent_id,
        timestamp: timestamp
            .as_ref()
            .map(|value| js_to_string(Some(value)))
            .unwrap_or_default(),
        target_id,
        label: Some(label),
    };
    StoredFileEntry {
        entry: FileEntry::Entry(SessionEntry::Label(entry)),
        raw: Some(raw),
    }
}

impl ReadonlySessionManager for SessionManager {
    fn get_cwd(&self) -> &str {
        SessionManager::get_cwd(self)
    }
    fn get_session_dir(&self) -> &str {
        SessionManager::get_session_dir(self)
    }
    fn get_session_id(&self) -> &str {
        SessionManager::get_session_id(self)
    }
    fn get_session_file(&self) -> Option<&str> {
        SessionManager::get_session_file(self)
    }
    fn get_leaf_id(&self) -> Option<&str> {
        SessionManager::get_leaf_id(self)
    }
    fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        SessionManager::get_leaf_entry(self)
    }
    fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        SessionManager::get_entry(self, id)
    }
    fn get_label(&self, id: &str) -> Option<&str> {
        SessionManager::get_label(self, id)
    }
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        SessionManager::get_branch(self, from_id)
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        SessionManager::build_context_entries(self)
    }
    fn build_session_projection(&self) -> SessionProjection {
        SessionManager::build_session_projection(self)
    }
    fn get_header(&self) -> Option<&SessionHeader> {
        SessionManager::get_header(self)
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        SessionManager::get_entries(self)
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        SessionManager::get_tree(self)
    }
    fn get_session_name(&self) -> Option<String> {
        SessionManager::get_session_name(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_utf8_suffix_detection() {
        assert_eq!(incomplete_utf8_suffix_len(b"abc"), 0);
        assert_eq!(incomplete_utf8_suffix_len("é".as_bytes()), 0);
        assert_eq!(incomplete_utf8_suffix_len(&[0xC3]), 1);
        assert_eq!(incomplete_utf8_suffix_len(&[0x61, 0xC3]), 1);
        assert_eq!(incomplete_utf8_suffix_len(&[0xE2, 0x82]), 2);
        assert_eq!(incomplete_utf8_suffix_len(&[0xF0, 0x9F, 0x98]), 3);
        assert_eq!(incomplete_utf8_suffix_len(&[0xFF]), 0);
    }

    #[test]
    fn validates_session_ids_like_the_ts_regex() {
        for valid in ["a", "abc-123_def.456", "A1", "x.y", "0"] {
            assert!(assert_valid_session_id(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "", "-abc", "abc-", "_abc", "abc_", ".abc", "abc.", "abc/def", "abc\\def", "abc def",
            "é",
        ] {
            assert!(assert_valid_session_id(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn trims_like_javascript() {
        assert_eq!(js_trim("\u{00a0} hi \u{3000}"), "hi");
        assert_eq!(js_trim("\u{feff}x"), "x");
    }
}
