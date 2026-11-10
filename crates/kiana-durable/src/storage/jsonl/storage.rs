//! storage/jsonl/storage.ts。
//!
//! 可移植的 JSONL 存储后端：一个目录、一份 append-only 的 `main.jsonl` 提交标记，加上每个
//! 文档化身/活动任务一份 sidecar 文件。
//!
//! # 与 TS 的对应
//!
//! - **磁盘格式是线协议**：键序、`ordinal` 的跨文件全局递增、`\n` 行尾、原子 rename、目录
//!   布局都必须保持稳定，以便既有磁盘数据与跨语言实现互操作（§4.1）。JSON 一律经
//!   [`kiana_json::js_object::stringify`] 写出，保持 `JSON.stringify` 的字节格式（数字格式
//!   与整数键序），否则读取方无法复现同一份文本。
//! - **解析**用 [`kiana_json::js_parse::parse`]（V8 的 `JSON.parse` 语义），校验分支与错误文案
//!   逐字来自 TS 的 `validateMainOperation` / `parseMainMarker` / `parseSidecarRecord`。
//! - **UTF-8 解码**是 fatal 的 `TextDecoder("utf-8")`：非法字节 → `Invalid UTF-8 in complete
//!   … line …`；默认 `ignoreBOM: false` 会丢掉每次 `decode()` 开头的 BOM，这里同样按行丢弃。
//! - **提交语义**：先 `prepareCommit`（同步校验与脱钩），再写 sidecar、可选 flush、写主标记，
//!   然后 `apply` / `adopt` / 尽力而为的回收。append/flush 失败让存储进入 poison 状态。
//! - **恢复**：按 §2.14 的 16 步重建内存状态，截断未确认尾部并回收失效 sidecar。
//!
//! # 这个文件负责什么
//!
//! 它把 [`MemoryStorage`] 的语义镜像到磁盘：写入路径是「内存校验 → 编码 → 追加 sidecar →
//! 追加 `main.jsonl` 标记 → 应用到内存」；读取路径完全不碰磁盘，直接转发给由恢复过程重建出来
//! 的内存镜像。文档物化、扫描、游标、ID 分配等逻辑都在 [`MemoryStorage`] 中，本文件只负责
//! **持久化边界**：哪些行已经随 marker 提交、哪些 sidecar 记录已确认、哪些 sidecar 可以回收。
//!
//! 主要调用方是 `open_node_jsonl_storage`（`storage/jsonl/node.rs`，绑定本地 Node 文件系统
//! 语义）、示例与存储一致性测试。本类型只依赖 [`FileSystem`] 抽象，因此不绑定具体平台。
//!
//! # 磁盘布局
//!
//! ```text
//! <directory>/
//!   main.jsonl                     提交标记，append-only，每行 {format,type:"commit",seq,writes}
//!   doc-<DocumentId>.jsonl         文档化身的内容记录（base / delta）
//!   task-<TaskId>.jsonl            非终态任务记录
//!   doc-<id>.jsonl.reclaim         回收临时文件；open 时尝试删除普通文件（失败静默，下次 open 再试）
//! ```
//!
//! 提交点是 `main.jsonl` 里的一行 marker：sidecar 记录先追加，marker 后追加。没有 marker
//! 确认的 sidecar 行视为"未确认"；未确认行只允许出现在文件尾部，open 会从第一条未确认行处
//! 截断，若其后仍出现已确认记录则按损坏失败（`Confirmed record follows an unconfirmed tail
//! in <file>`）。
//!
//! # 内存结构
//!
//! - `memory`：提交校验与读取语义的参考实现，也是全部读取的数据源；
//! - `state`（`StorageState`，由 `Mutex` 保护）：`current_only_documents`（只保留当前状态的
//!   文档化身）、`live_task_sidecars`（当前存在 sidecar 文件的活动任务）、`closed`、`poison`。
//!   锁内只做内存集合操作，不跨文件 I/O 持锁。
//!
//! # 一次 commit 的执行顺序
//!
//! 1. `assert_usable`：拒绝已关闭或 poisoned 的实例。
//! 2. `memory.prepare_commit(writes, None)`：同步完成全部校验、脱钩与 `document.copy` 展开，
//!    并分配 seq；此步失败时磁盘完全未动，序号也不消耗。
//! 3. `encode_commit`：按写入顺序生成 marker 与各 sidecar 的行文本（`ordinal` 在一次提交内
//!    跨文件全局递增）。
//! 4. `plan_reclamations`：算出本提交落盘后要删除/改写的 sidecar。
//! 5. 逐个追加 sidecar；`fsync` 时再逐个 flush。
//! 6. 追加 `main.jsonl` 的 marker —— **这一步成功即视为提交已持久**。
//! 7. `memory.apply()`、更新 `StorageState`、调用 `adopt(seq)`，最后尽力回收 sidecar。
//!
//! sidecar 的 append/flush 或 main 的 append 失败会让实例进入 poison：磁盘上可能留下部分行，
//! 内存镜像已不可信，必须重新 `open`。
//!
//! # 一次 open / recover 的执行顺序
//!
//! `open` 只做三步准备（解析绝对路径、递归建目录、拼接 `main.jsonl` 路径）后调用 `recover`：
//! 读主日志 → 校验 seq 严格递增 → 清理 `.reclaim` → 按文件名排序列出 sidecar → 逐文件解析并
//! 校验 `(seq, ordinal)` 顺序 → 计算 current-only/退役/终态任务/最新 base → 逐 marker 重新
//! 通过 `MemoryStorage::prepare_commit(seq)` + `apply()` 重放 → 截断未确认尾部 → 回收失效
//! sidecar → 记录 sidecar 状态。整个过程复用内存实现的校验，因此"磁盘上结构合法但语义非法"
//! 的状态会以 `Invalid committed state at sequence N` 失败。
//!
//! # 注意事项
//!
//! - **磁盘格式是线协议**：键序、`ordinal` 规则、`\n` 行尾、错误文案都不能随手改；改动会同时
//!   破坏重启恢复与 TS 对照测试。
//! - **读取不碰磁盘**：运行期读到的都是最近一次成功提交的内存状态；外部进程对目录的修改只有
//!   在重新 `open` 时才会被看到。
//! - **回收是尽力而为**：主标记已落盘，sidecar 的删除/改写失败只留下垃圾文件，下次 open 会
//!   重新计算并回收；这些失败不会向调用方报错。
//! - **close 不写盘**：它只把实例标记为 closed 并关闭内存存储，持久性完全由每次 commit 的
//!   append 决定。
//! - **并发边界**：`state` 锁只保护四个内存字段；`commit` 的 prepare、文件 I/O 与 apply 都
//!   不在该锁内，本文件没有实现跨 I/O 的提交串行化。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use kiana_chord::context::Context;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::env::{
    CreateDirOptions, FileContent, FileError, FileErrorCode, FileKind, FileSystem, RemoveOptions,
};
use crate::storage::memory::MemoryStorage;
use crate::types::{
    AdoptFn, BoxError, ConversationId, ConversationQuery, Cursor, DocumentAddress,
    DocumentContentKind, DocumentCreate, DocumentHistory, DocumentId, DocumentPoint, DocumentQuery,
    DocumentRecord, EntryId, EntryQuery, EntryRecord, IdKind, Page, ScopeKind, Seq, Storage,
    StorageWrite, StoredDocument, StoredEntry, SubmissionId, SubmissionQuery, SubmissionRecord,
    TaskId, TaskQuery, TaskRecord, TaskStatus, id_from_number, js_safe_integer, seq_from_number,
};

/// `const FORMAT_VERSION = 1`。
const FORMAT_VERSION: i64 = 1;
/// `const MAIN_FILE = "main.jsonl"`。
const MAIN_FILE: &str = "main.jsonl";
/// `const RECLAIM_SUFFIX = ".reclaim"`。
const RECLAIM_SUFFIX: &str = ".reclaim";

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 公开的普通错误（TS 里是 `new Error(message)`）：只用 `Display` 对外。
#[derive(Debug, Clone)]
struct JsonlError {
    message: String,
}

impl fmt::Display for JsonlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JsonlError {}

/// `errorFromFile(action, error)`：`JSONL ${action} failed: ${error.message}`。
///
/// `cause` 通过 `Error::source()` 暴露，调用方可下钻到 [`FileError`] 读取错误码。
#[derive(Debug)]
struct JsonlFileError {
    message: String,
    cause: FileError,
}

impl fmt::Display for JsonlFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JsonlFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

fn error(message: impl Into<String>) -> BoxError {
    Box::new(JsonlError {
        message: message.into(),
    })
}

fn error_from_file(action: &str, error: FileError) -> BoxError {
    Box::new(JsonlFileError {
        message: format!("JSONL {action} failed: {}", error.message),
        cause: error,
    })
}

/// `JsonlCorruptionError`：磁盘上出现了结构非法或无法解释的内容。
///
/// `Display` 是 `message`（与 TS 的 `Error.message` 一致），`source` 是 `cause`。
///
/// 由恢复路径产生：解析失败、字段形状非法、行顺序错误、UTF-8 非法、已确认的 sidecar 记录
/// 缺失，以及重放时被 `MemoryStorage` 拒绝的"语义非法"状态。错误消息是当前稳定契约，调用方
/// 可以据此区分"磁盘损坏"与普通 I/O 失败；出现本错误时 `open` 失败。
#[derive(Debug)]
pub struct JsonlCorruptionError {
    message: String,
    cause: Option<BoxError>,
}

impl JsonlCorruptionError {
    /// `new JsonlCorruptionError(message)`。
    ///
    /// 没有底层原因（`source()` 返回 `None`），用于纯校验类失败。
    pub fn new(message: impl Into<String>) -> Self {
        JsonlCorruptionError {
            message: message.into(),
            cause: None,
        }
    }

    /// `new JsonlCorruptionError(message, cause)`。
    ///
    /// `cause` 通过 `Error::source()` 暴露，通常是 JSON 解析错误或 `MemoryStorage` 的校验错误。
    pub fn with_cause(message: impl Into<String>, cause: BoxError) -> Self {
        JsonlCorruptionError {
            message: message.into(),
            cause: Some(cause),
        }
    }

    /// 错误消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for JsonlCorruptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JsonlCorruptionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| &**cause as &(dyn std::error::Error + 'static))
    }
}

fn corruption(message: impl Into<String>) -> BoxError {
    Box::new(JsonlCorruptionError::new(message))
}

fn corruption_with_cause(message: impl Into<String>, cause: BoxError) -> BoxError {
    Box::new(JsonlCorruptionError::with_cause(message, cause))
}

/// `JsonlStoragePoisonedError`：一次写入失败后，磁盘状态不可信，必须重新打开。
///
/// 同一个存储实例只会保留第一个 cause；之后每次抛出都复用这条错误（`Display` 固定）。
///
/// 触发条件是 sidecar 追加、sidecar flush（`fsync`）或 `main.jsonl` 追加失败：磁盘上可能留下
/// 未确认的半行，内存镜像不再可信。poisoned 实例上除 `close` 外的所有操作（包括读取）都返回
/// 本错误；重新 `open` 同一目录即可恢复到最后一个已提交状态。
#[derive(Debug, Clone)]
pub struct JsonlStoragePoisonedError {
    cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

impl JsonlStoragePoisonedError {
    /// `new JsonlStoragePoisonedError(cause)`。
    ///
    /// `None` 表示没有底层原因；`cause` 通过 `Error::source()` 暴露。
    /// 本文件内部的 `poison` 总是带 cause。
    pub fn new(cause: Option<Arc<dyn std::error::Error + Send + Sync>>) -> Self {
        JsonlStoragePoisonedError { cause }
    }

    /// 第一次失败的原因。
    pub fn cause(&self) -> Option<&(dyn std::error::Error + Send + Sync)> {
        self.cause.as_deref()
    }
}

impl fmt::Display for JsonlStoragePoisonedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSONL storage is poisoned and must be reopened")
    }
}

impl std::error::Error for JsonlStoragePoisonedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
}

// ---------------------------------------------------------------------------
// 选项
// ---------------------------------------------------------------------------

/// `JsonlStorageOptions`：`fsync` 为 true 时在追加主标记之前 flush 每个受影响的 sidecar。
///
/// 这个开关同时约束回收路径：有回收动作时，删除/改写 sidecar 之前先 flush `main.jsonl`，
/// flush 失败则跳过本次回收（留给下次 open 重试）。为 false 时提交与回收都不调用 flush。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonlStorageOptions {
    /// 每个受影响的 sidecar 在追加主标记之前刷盘；默认 false。
    ///
    /// 覆盖本提交追加过的 sidecar、回收前对 `main.jsonl` 的 flush，以及重写 sidecar 时对
    /// `<file>.reclaim` 临时文件的 flush；删除路径与 `fsync = false` 不调用 flush。
    pub fsync: bool,
}

// ---------------------------------------------------------------------------
// 文件名与键
// ---------------------------------------------------------------------------

/// sidecar 文件名里的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidecarKind {
    Doc,
    Task,
}

impl SidecarKind {
    fn prefix(self) -> &'static str {
        match self {
            SidecarKind::Doc => "doc",
            SidecarKind::Task => "task",
        }
    }
}

/// `sidecarFileName(kind, id)`：`${kind}-${id}.jsonl`。
fn sidecar_file_name(kind: SidecarKind, id: impl fmt::Display) -> String {
    format!("{}-{id}.jsonl", kind.prefix())
}

/// `^(?:doc|task)-(?:0|[1-9]\d*)\.jsonl$`。
fn is_sidecar_file_name(name: &str) -> bool {
    sidecar_numeric_part(name, ".jsonl").is_some()
}

/// `^(?:doc|task)-(?:0|[1-9]\d*)\.jsonl\.reclaim$`。
fn is_reclaim_file_name(name: &str) -> bool {
    sidecar_numeric_part(name, ".jsonl.reclaim").is_some()
}

/// 解析文件名 `<kind>-<digits><suffix>` 中的数字部分；不匹配或违反数字规则时返回 `None`。
///
/// 数字只接受 `0` 或非零开头的纯 ASCII 数字，与 TS 正则 `(?:0|[1-9]\d*)` 一致。恢复阶段还会
/// 用同一规则从文件名还原文档/任务 ID。
fn sidecar_numeric_part<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let rest = name.strip_suffix(suffix)?;
    let digits = rest
        .strip_prefix("doc-")
        .or_else(|| rest.strip_prefix("task-"))?;
    let valid = digits == "0"
        || (!digits.is_empty()
            && !digits.starts_with('0')
            && digits.bytes().all(|b| b.is_ascii_digit()));
    valid.then_some(digits)
}

/// `isCurrentOnly(record)`：非会话作用域，或只保留最新状态的会话文档。
///
/// 返回 true 表示该文档化身不需要 as-of 历史：session/task 作用域的文档天生如此，conversation
/// 作用域则只有 `history === "latest"` 时成立。回收时因此只需要保留最新 base 及其后的记录，
/// 退役时整个 sidecar 都可以删除。
fn is_current_only(record: &DocumentCreate) -> bool {
    record.scope.kind() != ScopeKind::Conversation
        || record.history == Some(DocumentHistory::Latest)
}

/// 恢复期的 `isCurrentOnly(operation.record)`：记录还是未类型化的 JSON。
///
/// TS 在这里直接读 `record.scope.kind`（scope 缺失会抛 TypeError）。Rust 侧对同样的畸形输入
/// 取“当前态”分支，见模块交付说明的偏差。
fn is_current_only_record_json(record: &Value) -> bool {
    let scope_kind = record.get("scope").and_then(|scope| scope.get("kind"));
    match scope_kind {
        Some(Value::String(kind)) if kind == "conversation" => {
            record.get("history").and_then(Value::as_str) == Some("latest")
        }
        _ => true,
    }
}

/// `sidecarKey(file, seq, ordinal)` 的等价键。
///
/// `(文件, seq, ordinal)` 唯一标识一条 sidecar 记录：恢复阶段用它把 marker 里的引用映射到
/// 已解析的行，并保证同一条记录不会被两个 marker 重复确认。
type SidecarKey = (String, i64, i64);

fn sidecar_key(file: &str, seq: i64, ordinal: i64) -> SidecarKey {
    (file.to_owned(), seq, ordinal)
}

/// `jsonLine(value)`：`JSON.stringify(value)` + `\n`。
fn json_line(value: &Value) -> String {
    let mut line = kiana_json::js_object::stringify(value);
    line.push('\n');
    line
}

/// `JSON.parse(text)`：失败 → `Malformed complete ${description}`。
fn parse_json(text: &str, description: &str) -> Result<Value, BoxError> {
    kiana_json::js_parse::parse(text).map_err(|failure| {
        corruption_with_cause(
            format!("Malformed complete {description}"),
            Box::new(failure),
        )
    })
}

/// 把 Rust 记录序列化成中间 JSON 值。
///
/// 这里只决定值的内容；键序由各记录类型的字段声明顺序决定，最终写盘统一经
/// [`kiana_json::js_object::stringify`] 输出 ECMAScript 兼容的字节。
fn to_json<T>(value: &T) -> Result<Value, BoxError>
where
    T: ?Sized + Serialize,
{
    serde_json::to_value(value).map_err(|failure| -> BoxError { Box::new(failure) })
}

// ---------------------------------------------------------------------------
// 磁盘行（解析后的表示）
// ---------------------------------------------------------------------------

/// `MainOperation`：`main.jsonl` 一行里的一个写操作。
///
/// `Direct` / `Task` 保留原始 JSON 对象：TS 把 `JSON.parse` 得到的对象原样交给
/// `memory.prepareCommit`，结构问题（如果有）在重放时才暴露成 `Invalid committed state`。
#[derive(Debug, Clone)]
enum MainOperation {
    /// `conversation` / `entry` / `submission`（原始对象）。
    Direct(Value),
    /// `task`：原始对象 + 已校验的任务 ID。
    Task { value: Value, id: TaskId },
    /// `document.retire`。
    DocumentRetire { id: DocumentId },
    /// `task.sidecar`。
    TaskSidecar { id: TaskId, ordinal: i64 },
    /// `document.create`：原始 record + 已校验的 ID 与 ordinal。
    DocumentCreate {
        record: Value,
        id: DocumentId,
        ordinal: i64,
    },
    /// `document.change`。
    DocumentChange { id: DocumentId, ordinal: i64 },
}

/// `MainMarker`：`main.jsonl` 的一行。
#[derive(Debug, Clone)]
struct MainMarker {
    seq: i64,
    writes: Vec<MainOperation>,
}

/// `SidecarPayload`。
#[derive(Debug, Clone)]
enum SidecarPayload {
    /// `{ type: "task", value }`。
    Task { value: Value },
    /// `{ type: "document", id, content }`。
    Document { id: DocumentId, content: Value },
}

/// `SidecarRecord`：sidecar 文件的一行。
#[derive(Debug, Clone)]
struct SidecarRecord {
    /// `JSON.parse` 得到的原始对象；回收时按它重新写行（键序保留）。
    value: Value,
    seq: i64,
    ordinal: i64,
    payload: SidecarPayload,
}

/// `ParsedLine<T>`：`start` 是该行在文件里的字节偏移。
#[derive(Debug, Clone)]
struct ParsedLine<T> {
    value: T,
    start: usize,
}

/// `ParsedFile<T>`。
#[derive(Debug, Clone)]
struct ParsedFile<T> {
    path: String,
    lines: Vec<ParsedLine<T>>,
}

/// `EncodedCommit`。
struct EncodedCommit {
    marker: String,
    sidecars: IndexMap<String, String>,
}

// ---------------------------------------------------------------------------
// 校验器（错误文案逐字来自 TS）
// ---------------------------------------------------------------------------

fn is_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object()?.get(key)
}

/// `value.format !== FORMAT_VERSION` 的反面（JS 的 `===` 对数字是数值比较）。
fn is_format_version(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Number(number)) if number.as_f64() == Some(FORMAT_VERSION as f64))
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.as_object()?.get(key)?.as_str()
}

fn array_field<'a>(value: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    value.as_object()?.get(key)?.as_array()
}

/// `validateMainOperation(value, description)`。
///
/// 只做结构校验：对象形状、ID/ordinal 是 JS 安全整数、ordinal 非负、任务必须终态、
/// `document.create` 的 `record` 是对象、`type` 是已知写类型。本函数不读取文档内容：主日志的
/// `document.create` 项只带 `type`/`record`/`ordinal`，base/delta 判别值由 sidecar 路径上的
/// `validate_document_content` 校验。`Direct`/`Task` 分支保留原始 JSON，记录本身的完整
/// 反序列化推迟到 `recover` 重放时（失败会包装成 `Invalid committed state at sequence N`），
/// 因此这里能早发现的是"行写法不对"，不是"记录语义不对"。
///
/// `description` 形如 `main.jsonl line N`，会拼进每条错误消息；文案是当前稳定契约，改动需
/// 同步测试。
fn validate_main_operation(value: &Value, description: &str) -> Result<MainOperation, BoxError> {
    let Some(kind) = string_field(value, "type") else {
        return Err(corruption(format!("Invalid write in {description}")));
    };
    match kind {
        "conversation" | "entry" | "submission" => {
            let record = field(value, "value");
            let valid = record.is_some_and(|record| {
                is_object(record) && record.get("id").and_then(js_safe_integer).is_some()
            });
            if !valid {
                return Err(corruption(format!("Invalid {kind} write in {description}")));
            }
            Ok(MainOperation::Direct(value.clone()))
        }
        "task" => {
            let record = field(value, "value");
            let valid = record.is_some_and(|record| {
                is_object(record)
                    && record.get("id").and_then(js_safe_integer).is_some()
                    && record.get("state").is_some_and(is_object)
                    && record
                        .get("state")
                        .and_then(|state| state.get("status"))
                        .and_then(Value::as_str)
                        == Some("terminal")
            });
            if !valid {
                return Err(corruption(format!(
                    "Invalid terminal task write in {description}"
                )));
            }
            let record = field(value, "value").expect("validated task record");
            let id = record
                .get("id")
                .and_then(js_safe_integer)
                .expect("validated task id");
            Ok(MainOperation::Task {
                value: value.clone(),
                id: id_from_number::<crate::types::TaskBrand>(id),
            })
        }
        "document.retire" => {
            let Some(id) = field(value, "id").and_then(js_safe_integer) else {
                return Err(corruption(format!(
                    "Invalid document retirement in {description}"
                )));
            };
            Ok(MainOperation::DocumentRetire {
                id: id_from_number::<crate::types::DocumentBrand>(id),
            })
        }
        "task.sidecar" => {
            let id = field(value, "id").and_then(js_safe_integer);
            let ordinal = field(value, "ordinal").and_then(js_safe_integer);
            let (Some(id), Some(ordinal)) = (id, ordinal) else {
                return Err(corruption(format!(
                    "Invalid task sidecar write in {description}"
                )));
            };
            if ordinal < 0 {
                return Err(corruption(format!(
                    "Invalid task sidecar write in {description}"
                )));
            }
            Ok(MainOperation::TaskSidecar {
                id: id_from_number::<crate::types::TaskBrand>(id),
                ordinal,
            })
        }
        "document.create" => {
            let record = field(value, "record");
            let id = record
                .and_then(|record| record.get("id"))
                .and_then(js_safe_integer);
            let ordinal = field(value, "ordinal").and_then(js_safe_integer);
            let (Some(id), Some(ordinal)) = (id, ordinal) else {
                return Err(corruption(format!(
                    "Invalid document creation in {description}"
                )));
            };
            if ordinal < 0 || !record.is_some_and(is_object) {
                return Err(corruption(format!(
                    "Invalid document creation in {description}"
                )));
            }
            Ok(MainOperation::DocumentCreate {
                record: record.expect("validated document record").clone(),
                id: id_from_number::<crate::types::DocumentBrand>(id),
                ordinal,
            })
        }
        "document.change" => {
            let id = field(value, "id").and_then(js_safe_integer);
            let ordinal = field(value, "ordinal").and_then(js_safe_integer);
            let (Some(id), Some(ordinal)) = (id, ordinal) else {
                return Err(corruption(format!(
                    "Invalid document change in {description}"
                )));
            };
            if ordinal < 0 {
                return Err(corruption(format!(
                    "Invalid document change in {description}"
                )));
            }
            Ok(MainOperation::DocumentChange {
                id: id_from_number::<crate::types::DocumentBrand>(id),
                ordinal,
            })
        }
        _ => Err(corruption(format!("Unknown write type in {description}"))),
    }
}

/// `parseMainMarker(text, line)`。
///
/// 一行即一次提交。校验外层对象（`format === 1`、`type === "commit"`、`seq` 是安全整数且
/// `>= 1`、`writes` 是数组）并逐条校验 write；任何不满足都返回 `Invalid commit marker ...`
/// 或更具体的 write 错误。`line` 从 1 开始，仅用于错误文案。
fn parse_main_marker(text: &str, line: i64) -> Result<MainMarker, BoxError> {
    let description = format!("{MAIN_FILE} line {line}");
    let value = parse_json(text, &description)?;
    let invalid = || corruption(format!("Invalid commit marker in {description}"));
    if !is_object(&value)
        || !is_format_version(field(&value, "format"))
        || string_field(&value, "type") != Some("commit")
    {
        return Err(invalid());
    }
    let Some(seq) = field(&value, "seq").and_then(js_safe_integer) else {
        return Err(invalid());
    };
    if seq < 1 {
        return Err(invalid());
    }
    let Some(writes) = array_field(&value, "writes") else {
        return Err(invalid());
    };
    let writes = writes
        .iter()
        .map(|write| validate_main_operation(write, &description))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MainMarker { seq, writes })
}

/// `validateDocumentContent(value, description)`。
///
/// `version` 必须是安全整数且 `>= 1`；`kind === "base"` 要求 `value` 是对象，
/// `kind === "delta"` 要求 `ops` 是数组。其他字段不参与校验（与 TS 一样被忽略）。
///
/// 这是 sidecar 文档内容判别值（`kind`/`version`）的校验点，由 `parse_sidecar_record` 在
/// `payload.type === "document"` 分支调用；主日志的 `document.*` 写项不携带内容，不经过这里。
fn validate_document_content(value: Option<&Value>, description: &str) -> Result<(), BoxError> {
    let invalid = || corruption(format!("Invalid document content in {description}"));
    let Some(value) = value else {
        return Err(invalid());
    };
    if !is_object(value) {
        return Err(invalid());
    }
    let Some(version) = value.get("version").and_then(js_safe_integer) else {
        return Err(invalid());
    };
    if version < 1 {
        return Err(invalid());
    }
    match value.get("kind").and_then(Value::as_str) {
        Some("base") if value.get("value").is_some_and(is_object) => Ok(()),
        Some("delta") if value.get("ops").is_some_and(Value::is_array) => Ok(()),
        _ => Err(invalid()),
    }
}

/// `parseSidecarRecord(text, file, line)`。
///
/// sidecar 只允许**非终态**任务（终态任务写主日志），document 内容（含 `kind`/`version`
/// 判别值）转交 `validate_document_content` 校验。返回值保留解析出的原始对象 `value`，回收
/// 阶段按它的键序重新写行，因此这里的解析不能改用会丢键序的表示。
fn parse_sidecar_record(text: &str, file: &str, line: i64) -> Result<SidecarRecord, BoxError> {
    let description = format!("{file} line {line}");
    let value = parse_json(text, &description)?;
    let invalid = || corruption(format!("Invalid sidecar record in {description}"));
    if !is_object(&value) || !is_format_version(field(&value, "format")) {
        return Err(invalid());
    }
    if string_field(&value, "type") != Some("record") {
        return Err(invalid());
    }
    let Some(seq) = field(&value, "seq").and_then(js_safe_integer) else {
        return Err(invalid());
    };
    let Some(ordinal) = field(&value, "ordinal").and_then(js_safe_integer) else {
        return Err(invalid());
    };
    let payload = field(&value, "payload");
    let Some(payload) = payload.filter(|payload| is_object(payload)) else {
        return Err(invalid());
    };
    let Some(payload_type) = payload.get("type").and_then(Value::as_str) else {
        return Err(invalid());
    };
    if seq < 1 || ordinal < 0 {
        return Err(invalid());
    }
    let payload = match payload_type {
        "task" => {
            let task = payload.get("value");
            let valid = task.is_some_and(|task| {
                is_object(task)
                    && task.get("id").and_then(js_safe_integer).is_some()
                    && task.get("state").is_some_and(is_object)
                    && task
                        .get("state")
                        .and_then(|state| state.get("status"))
                        .and_then(Value::as_str)
                        != Some("terminal")
            });
            if !valid {
                return Err(corruption(format!(
                    "Invalid live task record in {description}"
                )));
            }
            SidecarPayload::Task {
                value: payload.get("value").expect("validated task value").clone(),
            }
        }
        "document" => {
            let Some(id) = payload.get("id").and_then(js_safe_integer) else {
                return Err(corruption(format!(
                    "Invalid document record in {description}"
                )));
            };
            validate_document_content(payload.get("content"), &description)?;
            SidecarPayload::Document {
                id: id_from_number::<crate::types::DocumentBrand>(id),
                content: payload.get("content").expect("validated content").clone(),
            }
        }
        _ => {
            return Err(corruption(format!(
                "Unknown sidecar record type in {description}"
            )));
        }
    };
    Ok(SidecarRecord {
        value,
        seq,
        ordinal,
        payload,
    })
}

// ---------------------------------------------------------------------------
// readLines
// ---------------------------------------------------------------------------

/// `readLines(fs, path, name, context, parse)`。
///
/// 只解析以 `\n` 结尾的完整行；末行不完整时先按 `lastIndexOf(0x0a) + 1` 截断。
///
/// 副作用：文件不存在（`NotFound`）视为空文件而不是错误；末行撕裂时本函数会**截断文件**到
/// 最后一个完整行之后，这是 open 期间的写操作之一。每行独立做 fatal UTF-8 解码（非法字节
/// 立即失败，不做 U+FFFD 替换）并丢弃行首 BOM；`start` 记录行首字节偏移，供恢复阶段按偏移
/// 截断未确认尾部。
async fn read_lines<T, F>(
    fs: &dyn FileSystem,
    path: &str,
    name: &str,
    context: &dyn Context,
    parse: F,
) -> Result<ParsedFile<T>, BoxError>
where
    F: Fn(&str, i64) -> Result<T, BoxError>,
{
    let bytes = match fs.read_binary_file(path, context).await {
        Ok(bytes) => bytes,
        Err(error) if error.code == FileErrorCode::NotFound => {
            return Ok(ParsedFile {
                path: path.to_owned(),
                lines: Vec::new(),
            });
        }
        Err(error) => return Err(error_from_file(&format!("read of {name}"), error)),
    };
    let mut complete_size = bytes.len();
    if complete_size > 0 && bytes[complete_size - 1] != 0x0a {
        // 末字节不是 '\n'：最后一次 append 可能只写了半行（崩溃或短写）。只保留最后一个
        // '\n' 之前的完整前缀；一个 '\n' 都没有时截到 0（整个文件都是未完成行）。
        complete_size = bytes
            .iter()
            .rposition(|byte| *byte == 0x0a)
            .map_or(0, |index| index + 1);
        fs.truncate_file(path, complete_size as u64, context)
            .await
            .map_err(|error| error_from_file(&format!("torn-line truncation of {name}"), error))?;
    }
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_number = 1i64;
    // 按 0x0a 切分；每个 [start, end) 段是一行，`line_number` 从 1 开始（只用于错误文案）。
    for end in 0..complete_size {
        if bytes[end] != 0x0a {
            continue;
        }
        // fatal UTF-8：与 `TextDecoder("utf-8", { fatal: true })` 一样把非法字节当作损坏。
        let text = std::str::from_utf8(&bytes[start..end]).map_err(|failure| {
            corruption_with_cause(
                format!("Invalid UTF-8 in complete {name} line {line_number}"),
                Box::new(failure),
            )
        })?;
        // `TextDecoder("utf-8", { fatal: true })` 默认 `ignoreBOM: false`：每次 `decode()`
        // 都会丢掉开头的 U+FEFF。
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        lines.push(ParsedLine {
            value: parse(text, line_number)?,
            start,
        });
        start = end + 1;
        line_number += 1;
    }
    Ok(ParsedFile {
        path: path.to_owned(),
        lines,
    })
}

// ---------------------------------------------------------------------------
// JsonlStorage
// ---------------------------------------------------------------------------

/// 内存中的 sidecar 状态。
///
/// 全部字段由 [`JsonlStorage`] 的 `Mutex` 保护；锁内只做集合读写，不持锁做文件 I/O。
#[derive(Debug, Default)]
struct StorageState {
    /// 以 current-only 语义创建过的文档化身（session/task 作用域，或 conversation + latest）。
    /// 回收时据此判断某文档的历史能否丢弃。
    current_only_documents: IndexSet<DocumentId>,
    /// 已知存在 sidecar 文件的活动任务；任务转终态后要删除对应文件。
    live_task_sidecars: IndexSet<TaskId>,
    /// 已调用 `close`。
    closed: bool,
    /// 首次写入失败的原因；一旦置位，实例不再可用。
    poison: Option<JsonlStoragePoisonedError>,
}

impl StorageState {
    /// `assertUsable()`。
    ///
    /// 先判 `closed` 再判 `poison`（与 TS 的检查顺序一致）。两种错误都不可重试：实例只能
    /// `close` 后重新 `open`。
    fn assert_usable(&self) -> Result<(), BoxError> {
        if self.closed {
            return Err(error("JsonlStorage is closed"));
        }
        if let Some(poison) = &self.poison {
            return Err(Box::new(poison.clone()));
        }
        Ok(())
    }
}

/// `JsonlStorage`：`Storage` 的可移植 JSONL 实现。
///
/// 组合一个 [`MemoryStorage`] 作为提交校验器与唯一读源：运行期读取不会访问磁盘。`state` 只
/// 保存回收所需的 sidecar 集合与实例生命周期（closed/poison）。没有析构逻辑，关闭请调用
/// [`Storage::close`]。
pub struct JsonlStorage {
    fs: Arc<dyn FileSystem>,
    directory: String,
    main_path: String,
    fsync: bool,
    memory: MemoryStorage,
    state: Mutex<StorageState>,
}

impl fmt::Debug for JsonlStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonlStorage")
            .field("directory", &self.directory)
            .field("fsync", &self.fsync)
            .finish_non_exhaustive()
    }
}

impl JsonlStorage {
    /// 用给定的文件系统打开或创建一个 JSONL 存储目录（TS: `JsonlStorage.open`）。
    ///
    /// 步骤：`absolute_path` 解析目录 → 递归 `create_dir` → `join_path` 拼接 `main.jsonl`
    /// → 调用 `recover` 重放磁盘状态。目录不存在时会被创建。
    ///
    /// 打开过程本身可能写盘：截断撕裂的末行、删除 `.reclaim` 残留、截断未确认尾部、回收失效
    /// sidecar。
    ///
    /// # Errors
    /// 路径解析/建目录/加入 `main.jsonl` 失败，或恢复时发现磁盘状态损坏。
    ///
    /// - 文件系统操作失败返回 `JSONL <action> failed: <message>`，`source()` 保留 [`FileError`]；
    /// - 磁盘结构非法、行顺序错误、UTF-8 非法、已确认记录缺失，或重放被内存实现拒绝时返回
    ///   [`JsonlCorruptionError`]（重放类错误的消息为 `Invalid committed state at sequence N`）。
    pub async fn open<F>(
        directory: &str,
        fs: Arc<F>,
        context: &dyn Context,
        options: JsonlStorageOptions,
    ) -> Result<JsonlStorage, BoxError>
    where
        F: FileSystem + 'static,
    {
        let fs: Arc<dyn FileSystem> = fs;
        let absolute = fs
            .absolute_path(directory, context)
            .await
            .map_err(|error| error_from_file("path resolution", error))?;
        fs.create_dir(
            &absolute,
            Some(&CreateDirOptions {
                recursive: Some(true),
            }),
            context,
        )
        .await
        .map_err(|error| error_from_file("directory creation", error))?;
        let main_path = fs
            .join_path(&[absolute.clone(), MAIN_FILE.to_owned()], context)
            .await
            .map_err(|error| error_from_file("path join", error))?;
        let storage = JsonlStorage {
            fs,
            directory: absolute,
            main_path,
            fsync: options.fsync,
            memory: MemoryStorage::new(),
            state: Mutex::new(StorageState::default()),
        };
        storage.recover(context).await?;
        Ok(storage)
    }

    fn lock_state(&self) -> MutexGuard<'_, StorageState> {
        // 锁中毒时取回内部值继续使用，而不是把 panic 传播给调用方。
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `assertUsable()`。
    ///
    /// 每个公共操作入口（读取经 `store`、写入经 `commit`）都会先调用它。
    fn assert_usable(&self) -> Result<(), BoxError> {
        self.lock_state().assert_usable()
    }

    /// `get store()`：先 `assertUsable`，再返回内存存储。
    ///
    /// [`MemoryStorage`] 内部是 `Arc` 上的锁，克隆只是共享同一份状态；所有读取方法都经由这里，
    /// 因此 closed/poisoned 实例连读取都会失败。
    fn store(&self) -> Result<MemoryStorage, BoxError> {
        self.assert_usable()?;
        Ok(self.memory.clone())
    }

    /// `poison(cause)`：第一次失败的原因被保留，之后复用同一条错误。
    ///
    /// 只记录第一个原因；之后的失败都返回这条已有错误的克隆，因此同一实例的所有 poison 错误
    /// 文案一致。返回值的 `source()` 指向第一次的 cause。
    fn poison(&self, cause: BoxError) -> BoxError {
        let mut state = self.lock_state();
        if state.poison.is_none() {
            state.poison = Some(JsonlStoragePoisonedError::new(Some(Arc::from(cause))));
        }
        Box::new(state.poison.as_ref().expect("poison set above").clone())
    }

    /// `encodeCommit(seq, writes)`。
    ///
    /// `copied_documents` 是原始写入里 `document.copy` 的目标 ID：TS 的
    /// `memory.resolveDocumentCopies` 用 `{ kind: "base", version, value }` 字面量构造复制内容，
    /// `JSON.stringify` 保留该字面量键序；而 `DocumentBaseContent` 只能给出声明序
    /// （`version, kind, value`，也是生产路径 `session/transaction.ts` 的键序）。为了对齐两种
    /// 来源的磁盘字节，复制产生的 `document.create` 在这里按 TS 的 kind-first 键序重建内容。
    ///
    /// 输出：`marker` 是一行提交标记；`sidecars` 是「文件名 → 本次要追加的行文本」，按写入中
    /// 文件首次出现的顺序排列（同一文件的多条记录合并到同一段文本）。函数是纯计算，不做 I/O；
    /// `document.copy` 已被 `prepare_commit` 展开，不会落盘（TS 的 switch 同样没有该分支）。
    fn encode_commit(
        seq: Seq,
        writes: &[StorageWrite],
        copied_documents: &IndexSet<DocumentId>,
    ) -> Result<EncodedCommit, BoxError> {
        // 第一遍：按写入顺序生成主日志条目，并为需要 sidecar 的写入分配全局 ordinal。
        // `records` 用 IndexMap 保留文件首次出现的顺序；`next_ordinal` 在整次提交内跨文件递增。
        let mut main_writes: Vec<Value> = Vec::new();
        let mut records: IndexMap<String, Vec<Value>> = IndexMap::new();
        let mut next_ordinal = 0i64;
        let add_sidecar = |records: &mut IndexMap<String, Vec<Value>>,
                           next_ordinal: &mut i64,
                           file: String,
                           payload: Value|
         -> i64 {
            let ordinal = *next_ordinal;
            *next_ordinal += 1;
            let record = json!({
                "format": FORMAT_VERSION,
                "type": "record",
                "seq": seq.get(),
                "ordinal": ordinal,
                "payload": payload,
            });
            records.entry(file).or_default().push(record);
            ordinal
        };

        for write in writes {
            match write {
                StorageWrite::Conversation { value } => {
                    main_writes.push(json!({
                        "type": "conversation",
                        "value": to_json(value)?,
                    }));
                }
                StorageWrite::Entry { value } => {
                    main_writes.push(json!({
                        "type": "entry",
                        "value": to_json(value)?,
                    }));
                }
                StorageWrite::Submission { value } => {
                    main_writes.push(json!({
                        "type": "submission",
                        "value": to_json(value)?,
                    }));
                }
                StorageWrite::DocumentRetire { id } => {
                    main_writes.push(json!({
                        "type": "document.retire",
                        "id": id,
                    }));
                }
                StorageWrite::Task { value } => {
                    if value.state.status() == TaskStatus::Terminal {
                        main_writes.push(json!({
                            "type": "task",
                            "value": to_json(value)?,
                        }));
                    } else {
                        let ordinal = add_sidecar(
                            &mut records,
                            &mut next_ordinal,
                            sidecar_file_name(SidecarKind::Task, value.id),
                            json!({ "type": "task", "value": to_json(value)? }),
                        );
                        main_writes.push(json!({
                            "type": "task.sidecar",
                            "id": value.id,
                            "ordinal": ordinal,
                        }));
                    }
                }
                StorageWrite::DocumentCreate { record, content } => {
                    let content_json = if copied_documents.contains(&record.id) {
                        // TS `resolveDocumentCopies` 的字面量键序：kind, version, value。
                        let mut object = Map::new();
                        object.insert("kind".to_owned(), Value::String("base".to_owned()));
                        object.insert("version".to_owned(), json!(content.version));
                        object.insert("value".to_owned(), to_json(&content.value)?);
                        Value::Object(object)
                    } else {
                        to_json(content)?
                    };
                    let ordinal = add_sidecar(
                        &mut records,
                        &mut next_ordinal,
                        sidecar_file_name(SidecarKind::Doc, record.id),
                        json!({
                            "type": "document",
                            "id": record.id,
                            "content": content_json,
                        }),
                    );
                    main_writes.push(json!({
                        "type": "document.create",
                        "record": to_json(record)?,
                        "ordinal": ordinal,
                    }));
                }
                StorageWrite::DocumentChange { id, content } => {
                    let ordinal = add_sidecar(
                        &mut records,
                        &mut next_ordinal,
                        sidecar_file_name(SidecarKind::Doc, id),
                        json!({
                            "type": "document",
                            "id": id,
                            "content": to_json(content)?,
                        }),
                    );
                    main_writes.push(json!({
                        "type": "document.change",
                        "id": id,
                        "ordinal": ordinal,
                    }));
                }
                // `prepareCommit` 已把 `document.copy` 展开成 `document.create`；TS 的 switch 没有
                // 这个分支，遇到时静默跳过，这里保持同样行为。
                StorageWrite::DocumentCopy { .. } => {}
            }
        }

        // 第二遍：把每个文件的行序列化成追加文本。拼接顺序就是后续 append 的顺序。
        let mut sidecars: IndexMap<String, String> = IndexMap::new();
        for (file, file_records) in records {
            let mut content = String::new();
            for record in &file_records {
                content.push_str(&json_line(record));
            }
            sidecars.insert(file, content);
        }
        // marker 最后生成，键序固定为 format,type,seq,writes；`writes` 与输入写入同序。
        let marker = json!({
            "format": FORMAT_VERSION,
            "type": "commit",
            "seq": seq.get(),
            "writes": main_writes,
        });
        Ok(EncodedCommit {
            marker: json_line(&marker),
            sidecars,
        })
    }

    /// `planReclamations(writes, encoded)`：本提交落盘后可以回收/改写的 sidecar。
    ///
    /// 三类规则（只作用于 current-only 文档与终态任务；rewindable 文档永远保留历史）：
    /// 1. 退役的 current-only 文档 → 删除 sidecar（替换内容空字符串）；
    /// 2. 写入 base 的 current-only 文档 → 用本提交为它编码出的文本覆盖 sidecar，丢弃最新
    ///    base 之前的记录（同批退役的跳过，由规则 1 删除）；
    /// 3. 转成终态的活动任务 → 删除 sidecar（终态记录只留在主日志）。
    ///
    /// 判断一个文档是否 current-only 要同时看已提交集合与本批写入，所以先读 `StorageState`
    /// 再与 `writes` 求并集。返回值是「文件名 → 新内容」，空内容表示删除；这里只做规划，
    /// 真正的删除/改写发生在 marker 落盘之后（见 `commit`），失败不影响提交结果。
    fn plan_reclamations(
        &self,
        writes: &[StorageWrite],
        encoded: &EncodedCommit,
    ) -> IndexMap<String, String> {
        // 按本批写入收集四类候选：新建的 current-only 文档、写 base 的文档、退役的文档、
        // 本批最终出现的任务记录（同一任务后写覆盖前写）。
        let mut created_current_only_documents: IndexSet<DocumentId> = IndexSet::new();
        let mut retired_documents: IndexSet<DocumentId> = IndexSet::new();
        let mut base_documents: IndexSet<DocumentId> = IndexSet::new();
        let mut final_tasks: IndexMap<TaskId, &TaskRecord> = IndexMap::new();
        for write in writes {
            match write {
                StorageWrite::DocumentCreate { record, .. } => {
                    if is_current_only(record) {
                        created_current_only_documents.insert(record.id);
                    }
                }
                StorageWrite::DocumentChange { id, content } => {
                    if content.kind == DocumentContentKind::Base {
                        base_documents.insert(*id);
                    }
                }
                StorageWrite::DocumentRetire { id } => {
                    retired_documents.insert(*id);
                }
                StorageWrite::Task { value } => {
                    final_tasks.insert(value.id, value);
                }
                _ => {}
            }
        }

        let state = self.lock_state();
        let mut replacements: IndexMap<String, String> = IndexMap::new();
        let is_current_only_document = |id: &DocumentId| {
            state.current_only_documents.contains(id) || created_current_only_documents.contains(id)
        };
        // 规则 1：退役的 current-only 文档不再需要任何 sidecar 历史。
        for id in &retired_documents {
            if is_current_only_document(id) {
                replacements.insert(sidecar_file_name(SidecarKind::Doc, *id), String::new());
            }
        }
        // 规则 2：新 base 之后的历史才有意义；用本提交追加的那段文本覆盖旧内容。
        for id in &base_documents {
            if !is_current_only_document(id) || retired_documents.contains(id) {
                continue;
            }
            let file = sidecar_file_name(SidecarKind::Doc, *id);
            if let Some(content) = encoded.sidecars.get(&file) {
                replacements.insert(file, content.clone());
            }
        }
        // 规则 3：只有"已知有 sidecar 文件"的任务才需要删除动作——要么内存集合里记着，
        // 要么本提交刚为它编码了 sidecar。
        for (id, task) in &final_tasks {
            let file = sidecar_file_name(SidecarKind::Task, *id);
            if task.state.status() == TaskStatus::Terminal
                && (state.live_task_sidecars.contains(id) || encoded.sidecars.contains_key(&file))
            {
                replacements.insert(file, String::new());
            }
        }
        replacements
    }

    /// `adoptSidecarState(writes)`。
    ///
    /// 主标记已落盘后，把本批写入对 sidecar 状态的影响记到内存。
    ///
    /// 只跟踪 current-only 文档与任务。必须在 `commit` 中位于 marker 追加成功之后调用：否则
    /// 一次失败提交会让后续回收基于错误的前提。
    fn adopt_sidecar_state(&self, writes: &[StorageWrite]) {
        let mut state = self.lock_state();
        for write in writes {
            match write {
                StorageWrite::DocumentCreate { record, .. } => {
                    if is_current_only(record) {
                        state.current_only_documents.insert(record.id);
                    }
                }
                StorageWrite::Task { value } => {
                    if value.state.status() == TaskStatus::Terminal {
                        state.live_task_sidecars.shift_remove(&value.id);
                    } else {
                        state.live_task_sidecars.insert(value.id);
                    }
                }
                _ => {}
            }
        }
    }

    /// `reclaimSidecars`：主标记已发布，回收只是可重试的尽力维护，失败静默。
    ///
    /// `fsync` 时先 flush `main.jsonl`，确认 marker 已落盘后才允许删除/改写 sidecar 中的历史
    /// 内容；flush 失败就跳过本次回收，等下次 open 重算。`replacements` 为空时直接返回，
    /// 不会产生任何 I/O。各文件按规划顺序逐个处理。
    async fn reclaim_sidecars(
        &self,
        replacements: &IndexMap<String, String>,
        context: &dyn Context,
    ) {
        if replacements.is_empty() {
            return;
        }
        if self.fsync && self.fs.flush_file(&self.main_path, context).await.is_err() {
            return;
        }
        for (file, content) in replacements {
            self.replace_sidecar(file, content, context).await;
        }
    }

    /// `replaceSidecar(file, content)`：临时文件 + rename；任何失败都静默返回。
    ///
    /// 空内容表示删除文件（`force: true` 忽略 not-found）。非空时先写 `<file>.reclaim`，
    /// `fsync` 时 flush 它，再 rename 覆盖目标——rename 是这里唯一的原子替换点。中途失败会
    /// 留下 `.reclaim` 临时文件或旧文件，下次 open 清理/重算。
    async fn replace_sidecar(&self, file: &str, content: &str, context: &dyn Context) {
        let Ok(path) = self
            .fs
            .join_path(&[self.directory.clone(), file.to_owned()], context)
            .await
        else {
            return;
        };
        if content.is_empty() {
            let _ = self
                .fs
                .remove(
                    &path,
                    Some(&RemoveOptions {
                        recursive: None,
                        force: Some(true),
                    }),
                    context,
                )
                .await;
            return;
        }
        let Ok(temporary_path) = self
            .fs
            .join_path(
                &[self.directory.clone(), format!("{file}{RECLAIM_SUFFIX}")],
                context,
            )
            .await
        else {
            return;
        };
        if self
            .fs
            .write_file(
                &temporary_path,
                FileContent::Text(content.to_owned()),
                context,
            )
            .await
            .is_err()
        {
            return;
        }
        if self.fsync && self.fs.flush_file(&temporary_path, context).await.is_err() {
            return;
        }
        let _ = self.fs.rename_file(&temporary_path, &path, context).await;
    }

    /// `recover(context)`：重建内存状态、截断未确认尾部、回收失效 sidecar。
    ///
    /// 只在 `open` 中调用一次，此时 `memory` 是全新的空实例。步骤编号对应 spec §2.14：
    /// 1-6 读取并校验磁盘；7-11 计算文档/任务集合与最新 base；12 逐 marker 重放回内存；
    /// 13-15 截断未确认尾部并回收；16 记录 sidecar 状态。
    ///
    /// # Errors
    /// 主日志/sidecar 结构非法、UTF-8 非法、行顺序错误、已确认记录缺失，或重放时被内存实现
    /// 拒绝（`Invalid committed state at sequence N`）。失败时磁盘可能已被部分修改（撕裂行
    /// 截断、`.reclaim` 清理、未确认尾部截断），但这些修改都不触及已提交状态。
    async fn recover(&self, context: &dyn Context) -> Result<(), BoxError> {
        // 1. 读主日志；文件不存在视为空，撕裂的末行会先被截断。
        let main = read_lines(
            self.fs.as_ref(),
            &self.main_path,
            MAIN_FILE,
            context,
            parse_main_marker,
        )
        .await?;
        // 2. marker 的 seq 必须严格递增；解析器已保证每行 seq >= 1，这里从 0 起比较。
        let mut previous_seq = 0i64;
        for line in &main.lines {
            if line.value.seq <= previous_seq {
                return Err(corruption(format!(
                    "Commit sequence does not strictly increase in {MAIN_FILE}"
                )));
            }
            previous_seq = line.value.seq;
        }

        // 3. 列出目录；后续只关注普通文件（子目录、符号链接被忽略）。
        let listed = self
            .fs
            .list_dir(&self.directory, context)
            .await
            .map_err(|error| error_from_file("directory listing", error))?;
        // 4. 清理上次回收失败留下的临时文件；失败静默，下次 open 再试。
        for info in &listed {
            if info.kind == FileKind::File && is_reclaim_file_name(&info.name) {
                let _ = self
                    .fs
                    .remove(
                        &info.path,
                        Some(&RemoveOptions {
                            recursive: None,
                            force: Some(true),
                        }),
                        context,
                    )
                    .await;
            }
        }
        // 5. 按文件名排序，让解析与后续回收顺序不依赖目录返回顺序（文件名均为 ASCII）。
        let mut sidecar_files: Vec<String> = listed
            .iter()
            .filter(|info| info.kind == FileKind::File && is_sidecar_file_name(&info.name))
            .map(|info| info.name.clone())
            .collect();
        sidecar_files.sort();

        // 6. 逐文件解析，并校验文件内 (seq, ordinal) 严格递增；同时按三元组建索引。
        let mut parsed_files: IndexMap<String, ParsedFile<SidecarRecord>> = IndexMap::new();
        let mut record_by_key: HashMap<SidecarKey, SidecarRecord> = HashMap::new();
        for file in &sidecar_files {
            let path = self
                .fs
                .join_path(&[self.directory.clone(), file.clone()], context)
                .await
                .map_err(|error| error_from_file("path join", error))?;
            let parsed = read_lines(self.fs.as_ref(), &path, file, context, |text, line| {
                parse_sidecar_record(text, file, line)
            })
            .await?;
            let mut previous: Option<&SidecarRecord> = None;
            for line in &parsed.lines {
                if let Some(previous) = previous
                    && (line.value.seq < previous.seq
                        || (line.value.seq == previous.seq
                            && line.value.ordinal <= previous.ordinal))
                {
                    return Err(corruption(format!(
                        "Sidecar records are out of order in {file}"
                    )));
                }
                previous = Some(&line.value);
                record_by_key.insert(
                    sidecar_key(file, line.value.seq, line.value.ordinal),
                    line.value.clone(),
                );
            }
            parsed_files.insert(file.clone(), parsed);
        }

        // 7. 主日志里的文档/任务状态。
        // `final_task_is_live` 记录每个任务在主日志中最后一次出现的形态：终态 task 写 → false，
        // task.sidecar → true（同 ID 后写覆盖前写）。
        let mut current_only_documents: IndexSet<DocumentId> = IndexSet::new();
        let mut retired_documents: IndexSet<DocumentId> = IndexSet::new();
        let mut final_task_is_live: IndexMap<TaskId, bool> = IndexMap::new();
        for line in &main.lines {
            for operation in &line.value.writes {
                match operation {
                    MainOperation::DocumentCreate { record, id, .. } => {
                        if is_current_only_record_json(record) {
                            current_only_documents.insert(*id);
                        }
                    }
                    MainOperation::DocumentRetire { id } => {
                        retired_documents.insert(*id);
                    }
                    MainOperation::Task { id, .. } => {
                        final_task_is_live.insert(*id, false);
                    }
                    MainOperation::TaskSidecar { id, .. } => {
                        final_task_is_live.insert(*id, true);
                    }
                    MainOperation::Direct(_) | MainOperation::DocumentChange { .. } => {}
                }
            }
        }
        // 8. 已退役的当前态文档。
        let retired_current_only_documents: IndexSet<DocumentId> = retired_documents
            .iter()
            .filter(|id| current_only_documents.contains(*id))
            .copied()
            .collect();

        // 9. 每个当前态文档最新的已确认 base。
        // 只认 payload 是 document、id 匹配且 content.kind == "base" 的记录；按 (seq, ordinal)
        // 取最大。找不到说明内容已被回收，或者该位置不是 base（delta 不参与）。
        let mut latest_bases: IndexMap<DocumentId, SidecarRecord> = IndexMap::new();
        for line in &main.lines {
            let marker = &line.value;
            for operation in &marker.writes {
                let (id, ordinal) = match operation {
                    MainOperation::DocumentCreate { id, ordinal, .. } => (*id, *ordinal),
                    MainOperation::DocumentChange { id, ordinal } => (*id, *ordinal),
                    _ => continue,
                };
                if !current_only_documents.contains(&id) {
                    continue;
                }
                let Some(record) = record_by_key.get(&sidecar_key(
                    &sidecar_file_name(SidecarKind::Doc, id),
                    marker.seq,
                    ordinal,
                )) else {
                    continue;
                };
                match &record.payload {
                    SidecarPayload::Task { .. } => continue,
                    SidecarPayload::Document {
                        id: record_id,
                        content,
                    } => {
                        if *record_id != id
                            || content.get("kind").and_then(Value::as_str) != Some("base")
                        {
                            continue;
                        }
                    }
                }
                let replace = match latest_bases.get(&id) {
                    None => true,
                    Some(previous) => {
                        record.seq > previous.seq
                            || (record.seq == previous.seq && record.ordinal > previous.ordinal)
                    }
                };
                if replace {
                    latest_bases.insert(id, record.clone());
                }
            }
        }

        // 10. "是否早于最新 base" 的判断由文件末尾的 is_before_latest_base 完成，步骤 12 使用。

        // 11. 终态任务集合。
        let terminal_tasks: IndexSet<TaskId> = final_task_is_live
            .iter()
            .filter(|(_, live)| !**live)
            .map(|(id, _)| *id)
            .collect();

        // 12. 逐 marker 确认 sidecar 记录并重放进内存存储。
        let mut confirmed: HashSet<SidecarKey> = HashSet::new();
        for line in &main.lines {
            let marker = &line.value;
            let mut writes: Vec<StorageWrite> = Vec::new();
            for operation in &marker.writes {
                match operation {
                    MainOperation::Direct(value) | MainOperation::Task { value, .. } => {
                        // 主日志里存的是原始 JSON，这里才做 StorageWrite 的完整反序列化。
                        let write = serde_json::from_value::<StorageWrite>(value.clone())
                            .map_err(|failure| invalid_committed_state(marker.seq, failure))?;
                        writes.push(write);
                    }
                    MainOperation::DocumentRetire { id } => {
                        writes.push(StorageWrite::DocumentRetire { id: *id });
                    }
                    MainOperation::TaskSidecar { id, ordinal } => {
                        let file = sidecar_file_name(SidecarKind::Task, *id);
                        // 终态任务的 sidecar 可能已被回收：optional 为 true 时缺失不算损坏；
                        // 记录存在时仍会校验 payload/id，但不 push 写——终态记录由主日志里的
                        // task 写重放。
                        let optional = terminal_tasks.contains(id);
                        let record = confirm_record(
                            marker.seq,
                            *ordinal,
                            &file,
                            &record_by_key,
                            &mut confirmed,
                            optional,
                        )?;
                        if let Some(record) = record {
                            let SidecarPayload::Task { value } = &record.payload else {
                                return Err(corruption(format!(
                                    "Confirmed task sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            };
                            let task_id = value
                                .get("id")
                                .and_then(js_safe_integer)
                                .map(id_from_number::<crate::types::TaskBrand>);
                            if task_id != Some(*id) {
                                return Err(corruption(format!(
                                    "Confirmed task sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            }
                            if !optional {
                                let write = serde_json::from_value::<StorageWrite>(json!({
                                    "type": "task",
                                    "value": value,
                                }))
                                .map_err(|failure| invalid_committed_state(marker.seq, failure))?;
                                writes.push(write);
                            }
                        }
                    }
                    MainOperation::DocumentCreate {
                        record: record_json,
                        id,
                        ordinal,
                    } => {
                        // 回收判定：文档已退役，或这条记录早于该文档的最新 base，说明内容已被
                        // 丢弃/覆盖；此时重放必须用空 base 占位，让化身在内存中仍然存在。
                        let reclaimed = retired_current_only_documents.contains(id)
                            || is_before_latest_base(&latest_bases, id, marker.seq, *ordinal);
                        let file = sidecar_file_name(SidecarKind::Doc, *id);
                        let record = confirm_record(
                            marker.seq,
                            *ordinal,
                            &file,
                            &record_by_key,
                            &mut confirmed,
                            reclaimed,
                        )?;
                        let mut content: Option<Value> = None;
                        if let Some(record) = record {
                            let SidecarPayload::Document {
                                id: record_id,
                                content: record_content,
                            } = &record.payload
                            else {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            };
                            if record_id != id {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            }
                            content = Some(record_content.clone());
                        }
                        if let Some(content) = &content
                            && content.get("kind").and_then(Value::as_str) != Some("base")
                        {
                            return Err(corruption(format!(
                                "Document creation lacks a confirmed base in commit {}",
                                marker.seq
                            )));
                        }
                        // 已回收则即使记录还在也以空 base 代替；否则必须使用确认到的 base。
                        let effective = match content {
                            Some(content) if !reclaimed => content,
                            _ => default_base_content(),
                        };
                        let write = document_create_write(record_json, effective)
                            .map_err(|failure| invalid_committed_state(marker.seq, failure))?;
                        writes.push(write);
                    }
                    MainOperation::DocumentChange { id, ordinal } => {
                        // 与 create 相同的回收判定：被最新 base 覆盖的旧 delta 不再重放。
                        let reclaimed = retired_current_only_documents.contains(id)
                            || is_before_latest_base(&latest_bases, id, marker.seq, *ordinal);
                        let file = sidecar_file_name(SidecarKind::Doc, *id);
                        let record = confirm_record(
                            marker.seq,
                            *ordinal,
                            &file,
                            &record_by_key,
                            &mut confirmed,
                            reclaimed,
                        )?;
                        let mut content: Option<Value> = None;
                        if let Some(record) = record {
                            let SidecarPayload::Document {
                                id: record_id,
                                content: record_content,
                            } = &record.payload
                            else {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            };
                            if record_id != id {
                                return Err(corruption(format!(
                                    "Confirmed document sidecar data does not match commit {}",
                                    marker.seq
                                )));
                            }
                            content = Some(record_content.clone());
                        }
                        if !reclaimed && let Some(content) = content {
                            let content = serde_json::from_value(content)
                                .map_err(|failure| invalid_committed_state(marker.seq, failure))?;
                            writes.push(StorageWrite::DocumentChange { id: *id, content });
                        }
                    }
                }
            }
            // 每个 marker 作为一个带确定 seq 的提交重放进内存；任何校验失败都说明磁盘状态
            // 无法用 Memory 语义解释，按损坏上报。
            let prepared = self
                .memory
                .prepare_commit(&writes, Some(seq_from_number(marker.seq)))
                .map_err(|failure| invalid_committed_state(marker.seq, failure))?;
            prepared
                .apply()
                .map_err(|failure| invalid_committed_state(marker.seq, failure))?;
        }

        // 13/14. 截断未确认尾部并计算保留内容。
        // 13 对应下面的 `unconfirmed_at`：合法文件里的未确认行只可能出现在尾部，一旦它后面
        // 又出现已确认记录，说明确认集合与文件顺序矛盾，按损坏上报。
        let mut reclamations: IndexMap<String, String> = IndexMap::new();
        for (file, parsed) in &parsed_files {
            let mut unconfirmed_at: Option<usize> = None;
            for line in &parsed.lines {
                let key = sidecar_key(file, line.value.seq, line.value.ordinal);
                if confirmed.contains(&key) {
                    if unconfirmed_at.is_some() {
                        return Err(corruption(format!(
                            "Confirmed record follows an unconfirmed tail in {file}"
                        )));
                    }
                } else if unconfirmed_at.is_none() {
                    unconfirmed_at = Some(line.start);
                }
            }
            if let Some(offset) = unconfirmed_at {
                self.fs
                    .truncate_file(&parsed.path, offset as u64, context)
                    .await
                    .map_err(|error| {
                        error_from_file(&format!("tail truncation of {file}"), error)
                    })?;
            }

            let confirmed_lines: Vec<&ParsedLine<SidecarRecord>> = parsed
                .lines
                .iter()
                .filter(|line| {
                    confirmed.contains(&sidecar_key(file, line.value.seq, line.value.ordinal))
                })
                .collect();
            // 文件名已由 is_sidecar_file_name 校验为纯数字；超出 i64 的超长数字串回落到
            // i64::MIN，这个哨兵不会命中任何任务/文档集合。
            let numeric_id = sidecar_numeric_part(file, ".jsonl")
                .and_then(|digits| digits.parse::<i64>().ok())
                .unwrap_or(i64::MIN);
            // 14. 计算要保留的已确认行：终态任务文件清空；退役的 current-only 文档清空；
            // 有最新 base 的 current-only 文档只保留不早于该 base 的行。其余情况（rewindable
            // 文档、没有 base 的文档）保留全部。
            let mut retained_lines: Option<Vec<&ParsedLine<SidecarRecord>>> = None;
            if file.starts_with("task-")
                && terminal_tasks.contains(&id_from_number::<crate::types::TaskBrand>(numeric_id))
            {
                retained_lines = Some(Vec::new());
            } else if file.starts_with("doc-") {
                let document_id = id_from_number::<crate::types::DocumentBrand>(numeric_id);
                if retired_current_only_documents.contains(&document_id) {
                    retained_lines = Some(Vec::new());
                } else if latest_bases.contains_key(&document_id) {
                    retained_lines = Some(
                        confirmed_lines
                            .iter()
                            .copied()
                            .filter(|line| {
                                !is_before_latest_base(
                                    &latest_bases,
                                    &document_id,
                                    line.value.seq,
                                    line.value.ordinal,
                                )
                            })
                            .collect(),
                    );
                }
            }
            // 只有在"确实丢弃了某些行"或"文件应清空"时才安排回收；否则不动这个文件。
            if let Some(retained) = retained_lines
                && (retained.len() < confirmed_lines.len() || retained.is_empty())
            {
                let mut content = String::new();
                for line in &retained {
                    content.push_str(&json_line(&line.value.value));
                }
                reclamations.insert(file.clone(), content);
            }
        }

        // 15. 回收。
        self.reclaim_sidecars(&reclamations, context).await;

        // 16. 记住 sidecar 状态。
        // 记录两类集合：所有 current-only 文档（含已退役，回收判断需要），以及最终仍活动的
        // 任务；终态任务的 sidecar 会被安排回收（删除失败静默，残留由下次 open 清理），因此
        // 不需要记录。
        let mut state = self.lock_state();
        for id in current_only_documents {
            state.current_only_documents.insert(id);
        }
        for (id, live) in final_task_is_live {
            if live {
                state.live_task_sidecars.insert(id);
            }
        }
        Ok(())
    }

    /// `resolveFile(file)`。
    async fn resolve_file(&self, file: &str, context: &dyn Context) -> Result<String, BoxError> {
        self.fs
            .join_path(&[self.directory.clone(), file.to_owned()], context)
            .await
            .map_err(|error| error_from_file("path join", error))
    }
}

/// `isBeforeLatestBase(id, seq, ordinal)`。
///
/// 严格早于该文档的最新 base 才返回 true；与 base 相等（或更晚）的记录要保留，因为 base
/// 本身之后的内容仍然需要。文档没有 base 记录时返回 false。
fn is_before_latest_base(
    latest_bases: &IndexMap<DocumentId, SidecarRecord>,
    id: &DocumentId,
    seq: i64,
    ordinal: i64,
) -> bool {
    match latest_bases.get(id) {
        Some(base) => seq < base.seq || (seq == base.seq && ordinal < base.ordinal),
        None => false,
    }
}

/// `confirmRecord(marker, ordinal, file, recordByKey, confirmed, optional)`。
///
/// 每个 `(file, seq, ordinal)` 至多确认一次：两个 marker 引用同一条 sidecar 记录说明磁盘
/// 状态矛盾。`optional` 表示该记录可能已被合法回收（退役的 current-only 文档、终态任务），
/// 此时缺失返回 `None`；非 optional 缺失则是损坏（`Missing confirmed sidecar record ...`）。
fn confirm_record<'a>(
    marker_seq: i64,
    ordinal: i64,
    file: &str,
    record_by_key: &'a HashMap<SidecarKey, SidecarRecord>,
    confirmed: &mut HashSet<SidecarKey>,
    optional: bool,
) -> Result<Option<&'a SidecarRecord>, BoxError> {
    let key = sidecar_key(file, marker_seq, ordinal);
    if confirmed.contains(&key) {
        return Err(corruption("Sidecar record is confirmed more than once"));
    }
    let Some(record) = record_by_key.get(&key) else {
        if optional {
            return Ok(None);
        }
        return Err(corruption(format!(
            "Missing confirmed sidecar record {file} at sequence {marker_seq}"
        )));
    };
    confirmed.insert(key);
    Ok(Some(record))
}

/// `Invalid committed state at sequence ${seq}`：磁盘上的状态无法通过 Memory 的校验。
///
/// `cause` 保留为 `source()`，通常是 `StorageWrite` 反序列化或 `MemoryStorage` 的校验错误。
fn invalid_committed_state(seq: i64, cause: impl Into<BoxError>) -> BoxError {
    corruption_with_cause(
        format!("Invalid committed state at sequence {seq}"),
        cause.into(),
    )
}

/// 恢复时缺失/已回收的 `document.create` 内容（TS 的字面量）。
///
/// 内容被回收后仍然必须重放这次 create：否则后续 marker 对同一化身的 change/retire 会让
/// 内存实现报 `Unknown document`（见 `check_document_actions` 的校验）。空 base 是 TS 选定的
/// 占位值。
fn default_base_content() -> Value {
    json!({ "kind": "base", "version": 1, "value": {} })
}

/// 把 `{type:"document.create", record, content}` 反序列化成 [`StorageWrite`]。
///
/// 键序按 TS 字面量 `type`、`record`、`content` 插入；`serde` 反序列化本身不依赖键序。
fn document_create_write(
    record: &Value,
    content: Value,
) -> Result<StorageWrite, serde_json::Error> {
    let mut map = Map::new();
    map.insert(
        "type".to_owned(),
        Value::String("document.create".to_owned()),
    );
    map.insert("record".to_owned(), record.clone());
    map.insert("content".to_owned(), content);
    serde_json::from_value(Value::Object(map))
}

// ---------------------------------------------------------------------------
// impl Storage
// ---------------------------------------------------------------------------

// 读方法全部转发给 `store()` 返回的内存镜像（`MemoryStorage` 内部是 Arc，克隆只是共享状态），
// 每个入口先检查 closed/poison，因此 poisoned 实例连读都会失败。真正有逻辑的只有 `commit`
// 与 `close`。
#[async_trait]
impl Storage for JsonlStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        // 1. 先做纯准备：prepare_commit 校验并脱钩全部写入（含 document.copy 展开），
        //    encode_commit 只做序列化。两步都在任何 I/O 之前完成，失败不会留下磁盘痕迹，
        //    也不会消耗序号。
        self.assert_usable()?;
        let prepared = self.memory.prepare_commit(writes, None)?;
        // `prepareCommit` 已把 `document.copy` 展开成 `document.create`；记下目标 ID 以便
        // `encode_commit` 复现 TS 复制内容的键序（见该方法文档）。
        let copied_documents: IndexSet<DocumentId> = writes
            .iter()
            .filter_map(|write| match write {
                StorageWrite::DocumentCopy { record, .. } => Some(record.id),
                _ => None,
            })
            .collect();
        let encoded =
            JsonlStorage::encode_commit(prepared.seq(), prepared.writes(), &copied_documents)?;
        let reclamations = self.plan_reclamations(prepared.writes(), &encoded);

        // 2. 先解析全部 sidecar 路径：任何路径失败都在写盘之前返回。
        let mut sidecars: Vec<(String, String, String)> =
            Vec::with_capacity(encoded.sidecars.len());
        for (file, content) in &encoded.sidecars {
            let path = self.resolve_file(file, context).await?;
            sidecars.push((file.clone(), content.clone(), path));
        }

        // 3. sidecar 记录先落盘。失败（包括短写）时写入结果不确定：可能留下没有 marker 确认
        //    的半行，也可能什么都没写；内存镜像不能再用来判断回收，因此一律 poison。
        for (file, content, path) in &sidecars {
            if let Err(failure) = self
                .fs
                .append_file(path, FileContent::Text(content.clone()), context)
                .await
            {
                return Err(self.poison(error_from_file(&format!("append to {file}"), failure)));
            }
        }
        // 4. fsync：追加 marker 之前把本提交涉及的 sidecar 刷盘。
        if self.fsync {
            for (file, _, path) in &sidecars {
                if let Err(failure) = self.fs.flush_file(path, context).await {
                    return Err(self.poison(error_from_file(&format!("flush of {file}"), failure)));
                }
            }
        }
        // 5. 追加主标记：它是唯一的提交点，成功之后磁盘状态才包含本批写入。
        if let Err(failure) = self
            .fs
            .append_file(
                &self.main_path,
                FileContent::Text(encoded.marker.clone()),
                context,
            )
            .await
        {
            return Err(self.poison(error_from_file(&format!("append to {MAIN_FILE}"), failure)));
        }
        // 6. 标记已持久：先更新内存与 sidecar 集合，再回调 adopt，最后才回收。
        //    `apply` 的失败只可能来自内部不变量（业务校验已在 prepare 完成），按普通错误传播，
        //    不再 poison。
        let seq = prepared.apply()?;
        self.adopt_sidecar_state(prepared.writes());
        // 主标记已落盘：这一批已持久。回收 sidecar 在 adopt 之后。
        adopt(seq);
        self.reclaim_sidecars(&reclamations, context).await;
        Ok(seq)
    }

    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.store()?.mint_id(kind).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<crate::types::ConversationRecord>, BoxError> {
        self.store()?.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<crate::types::ConversationRecord>, BoxError> {
        self.store()?
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.store()?.entry(id, context).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.store()?
            .visible_entry(conversation_id, id, context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.store()?
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        self.store()?
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.store()?.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.store()?
            .scan_tasks(query, limit, cursor, context)
            .await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.store()?.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.store()?
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.store()?
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.store()?.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.store()?.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.store()?
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &dyn Context) {
        // 幂等：锁内先置 closed 并立即释放锁，保证内存存储只被关闭一次。本方法不做任何
        // 持久化 I/O，也不 flush；已提交内容完全由每次 commit 的 append 决定。
        {
            let mut state = self.lock_state();
            if state.closed {
                return;
            }
            state.closed = true;
        }
        self.memory.close(context).await;
    }
}
