//! `storage/sqlite/storage.ts`：可移植 SQLite 上的 `Storage` 实现。
//!
//! 一个 SQL 事务就是一次 Session 提交：`commit` 事务内的全部检查、表写入、文档动作与
//! `durable_metadata` 的 `next_id`/`next_seq` 更新都在 `db.transaction` 的同一个事务中，
//! 提交返回（`COMMIT` 已落盘）之后才调用 `adopt`。（事务外只剩批内文档动作聚合与
//! `nextId` 候选值的计算，见下文“一次提交的执行顺序”。）
//!
//! # 与 TS 的对应
//!
//! - TS 的 `SqliteDatabase`/`SqliteExecutor` 是异步门面；Rust 复用
//!   [`kiana_chord::sqlite::SqliteDatabase`]，调用方以 `Arc<dyn SqliteDatabase>` 注入具体驱动
//!   （`open_node_sqlite_storage` 用 `rusqlite`）。
//! - 事务回调的错误类型是 [`SqliteError`]（只含消息、类别与嵌套错误，装不下原始 `BoxError`
//!   类型）。TS 把回调里抛出的**同一个** `Error`
//!   原样重抛给调用方；为了保留 `StorageRequestError` 的可判别性（Session 用它区分“只让这次
//!   读失败”的请求错误），回调用 `ErrorSlot` 记住原始错误，事务失败后再换回来；只有回滚也
//!   失败（`AggregateError`）时保留聚合错误。
//! - TS 的 `nextId` 是可变字段；Rust 用 `Mutex<f64>` 保存。用 `f64` 而不是 `i64` 是为了复刻
//!   TS 的 `Number(metadata.next_id)`：损坏的元数据在 TS 里会变成 `NaN` 并在之后被
//!   `String(Math.max(...))` 写回，Rust 保持同一行为。
//! - `admittedReads` 用 `tokio::sync::watch` 的计数复刻：`close` 等已接纳的读读完再关库，
//!   关库之后的新读报 `SqliteStorage is closed`。
//!
//! # 职责边界
//!
//! 本文件只实现 `Storage` 的读写语义；连接、事务与错误类型来自 `kiana_chord::sqlite`，
//! 表名/索引名一律取自 [`SqliteSchema`]（由 `prefix` 渲染），列名则是 SQL 文本里的固定
//! 契约。主要调用方是 `session`（经 `GuardedStorage`）与 `harness`/`scheduler`，它们都只
//! 持有 `Arc<dyn Storage>`；具体驱动通过 `Arc<dyn SqliteDatabase>` 注入。
//!
//! # 一次提交的执行顺序
//!
//! [`Storage::commit`] 分三段，只要回滚成功，任何一段失败都不会留下本批的部分写入：
//! 1. 事务外：`assert_open` → `prepare_document_actions` 按文档 ID 聚合动作并在批内查重
//!    → `candidate_next_id` 从内存 `nextId` 与写入的显式 ID 算出候选值；
//! 2. 事务内（`commit_transaction`）：读 `durable_metadata` 拿 `next_seq` 作为本批序号
//!    → `check_global_ids` → `check_document_actions` → 逐条 `apply_table_write` →
//!    `apply_document_actions` → 写回 `next_id`/`next_seq`；
//! 3. `COMMIT` 返回后：刷新内存 `nextId`，再同步调用 `adopt(seq)`。
//!
//! 检查都在表写入之前，但并非所有失败都能提前发现（例如 revision 的 `content` 缺失要靠
//! 驱动的 `NOT NULL` 约束报错）；只要回滚成功，两种失败都不留下半批数据，序号也不会被
//! 消耗。若回滚本身也失败（`AggregateError`，数据库状态未知），本文件不提供该保证。
//!
//! # 数据的存放约定
//!
//! - 记录以 `record` 列保存 `JSON.stringify` 的结果，读取时反序列化。文档的
//!   `createdAt`/`retiredAt` 同时冗余在 SQL 列里（用于过滤/排序），返回给调用方的记录仍
//!   来自 JSON 列；条目的提交序号不在 JSON 里，由 `commit_seq` 列补回。
//! - 被索引的字符串列（文档 `kind`/`key_value`、任务 `kind`、提交 `request_id`）存的是
//!   JSON 编码形式（字符串值带引号），不是裸字符串。读写两侧必须使用同一编码，手写 SQL
//!   用裸字符串做等值比较匹配不到任何行。
//! - 文档地址展开为 `(kind, scope_kind, owner_id, family, key_value)` 五列，`family` 区分
//!   有无 `key`；`address_key` 把这五列拼成字符串，只用于同一提交内的地址去重。
//! - 文档内容按 revision 追加：`base` 是完整值，`delta` 是**已解码**的 `Op[]` JSON 数组。
//!   读取时从 `seq <= 读取点` 的最后一条 base 开始，按 `seq` 增序回放其后的 delta。
//!
//! # 读路径与关闭
//!
//! `entry`/`visible_entry`（可见性重载要沿祖先链多次查询）、`find_latest_head_marker`
//! 与 `scan_entries` 经 `admit_read` 计数；`close` 先置位 `closed`，再等计数归零才关库。
//! 只要读的计数递增先于 `close_database` 的零值检查，`close` 就会等它读完；由于递增与
//! `assert_open` 不是原子操作，存在 `admit_read` 注释描述的交错窗口。关库后的新读得到
//! `SqliteStorage is closed`。
//!
//! 计数范围是精确清单：经 `admit_read` 计数的是 `entry`/`visible_entry`/
//! `find_latest_head_marker`/`scan_entries`；不计数的是读事务 `document`、其余单查询读
//! （`conversation`/`task`/`submission`/`submission_by_request`/`find_document`/
//! `scan_conversations`/`scan_tasks`/`scan_submissions`/`scan_documents`）以及 `commit`。
//! 它们与 `close` 的相对顺序依赖数据库适配器的契约（与 TS 的 `admitRead` 注释同一假设），
//! 不是本文件提供的保证。
//!
//! # 注意事项
//!
//! - `nextId` 有两份状态：内存缓存（可用 [`SqliteStorage::next_id`] 观察）与
//!   `durable_metadata.next_id` 文本列。`mint_id` 只改内存，提交时把
//!   `max(内存值, 本批最大写入 ID + 1)` 写回数据库；重开时以数据库为准重建缓存。
//! - 所有扫描的续页游标都由 [`next_cursor`] 生成，`after` 是上一页最后一项的 ID；带游标
//!   时以游标里的 `order` 为准，查询指定了不同顺序会由 `scan_start` 报错。
//!   `scan_documents` 是例外：只读游标的 `after`，忽略 `order`，顺序固定升序。
//! - `DocumentPoint::Current` 在 SQL 里是 `retired_at IS NULL`；`DocumentPoint::At(seq)` 是
//!   半开区间 `[created_at, retired_at)`，退休点所在提交里文档已经不可见。

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_chord::context::Context;
use kiana_chord::sqlite::database::{
    SqliteDatabase, SqliteDatabaseExt, SqliteError, SqliteExecutor, SqliteRow, SqliteValue,
};
use serde_json::Value;
use tokio::sync::watch;

use crate::storage::memory::DocumentCopyRejected;
use crate::storage::scan::{ScanStart, next_cursor, scan_start};
use crate::storage::sqlite::migrations::apply_sqlite_migrations_for_schema;
use crate::storage::sqlite::schema::{SqliteSchema, create_sqlite_schema};
use crate::types::{
    AdoptFn, BoxError, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentContent, DocumentContentKind, DocumentCreate, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, EntryId, EntryQuery, EntryRecord,
    IdKind, Page, ScanOrder, Seq, Storage, StorageRequestError, StorageWrite, StoredDocument,
    StoredEntry, SubmissionId, SubmissionQuery, SubmissionRecord, SubmissionStatus, TaskId,
    TaskQuery, TaskRecord, TaskStatus, seq_from_number,
};

/// JS 的 `Number.MAX_SAFE_INTEGER`（2^53-1）。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
/// 同一个常量作为降序扫描游标的缺省参数。
const MAX_SAFE_INTEGER_I64: i64 = 9_007_199_254_740_991;

/// `SqliteStorageOptions`（TS: `storage/sqlite/storage.ts`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SqliteStorageOptions {
    /// 整个 durable schema 的表/索引前缀。必须是 SQLite 标识符且不得使用保留的
    /// `sqlite_` 前缀；修改它不会迁移既有数据。
    ///
    /// 前缀由 `create_sqlite_schema` 渲染到所有表名/索引名，打开时先校验、再应用迁移；
    /// 不支持空串——无前缀要用 `None`。同一数据库文件可以用不同前缀共存
    /// （`tests/sqlite_storage.rs` 的
    /// `namespaces_tables_and_indexes_with_a_prefix_across_reopen` 覆盖了这一点）。
    pub prefix: Option<String>,
}

/// TS 里 `new Error(message)` 的等价物（`SqliteStorage` 的内部/不变量错误）。
///
/// 存储自己的不变量失败（未知文档、重复 ID、元数据缺失等）用它；只有
/// [`StorageRequestError`] 是「只让这次读失败」的请求错误（另一类非致命情形是调用方取消）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SqliteStorageError {
    /// 完整消息。
    pub message: String,
}

impl SqliteStorageError {
    /// `new Error(message)`。
    pub fn new(message: impl Into<String>) -> Self {
        SqliteStorageError {
            message: message.into(),
        }
    }
}

/// 内建/不变量错误：与 TS 的 `new Error(message)` 一样不可恢复，`Storage` 的调用方会把它
/// 当成致命错误。
fn error(message: impl Into<String>) -> BoxError {
    Box::new(SqliteStorageError::new(message))
}

/// 请求错误：非法游标、未知会话等。Session 只让这一次读失败，不会拉闸整个存储。
fn request_error(message: impl Into<String>) -> BoxError {
    Box::new(StorageRequestError::new(message))
}

fn int(value: i64) -> SqliteValue {
    SqliteValue::Integer(value)
}

fn text(value: impl Into<String>) -> SqliteValue {
    SqliteValue::Text(value.into())
}

/// TS `parseJson`：`JSON.parse`。
fn parse_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, BoxError> {
    serde_json::from_str(value).map_err(|failure| Box::new(failure) as BoxError)
}

/// TS `encodeJson`：`JSON.stringify(value)`（JS 的键序与数字格式）。
fn encode_json<T: ?Sized + serde::Serialize>(value: &T) -> Result<String, BoxError> {
    crate::types::stringify_json(value).map_err(|failure| Box::new(failure) as BoxError)
}

/// TS `JSON.stringify(ops)` 的 `Op[]`：已解码 op 的 JSON 元组数组（含内联路径，无 wire 压缩）。
fn ops_to_json(ops: &[kiana_chord::delta::Op]) -> Vec<Value> {
    ops.iter()
        .map(kiana_chord::delta::index::op_to_json)
        .collect()
}

/// TS `encodeIndexedString`：`JSON.stringify(value)`，用于以 JSON 字面量入库的被索引字符串。
fn encode_indexed_string(value: &str) -> String {
    kiana_json::js_string::js_quote(value)
}

/// `encodeIndexedString(address.key ?? "")`：`??` 把 `undefined` 与 `null` 都变成空串。
fn encode_indexed_key(key: Option<&Value>) -> String {
    match key {
        None | Some(Value::Null) => encode_indexed_string(""),
        Some(value) => kiana_json::js_object::stringify(value),
    }
}

/// JS 的 `Math.max`：任一参数是 `NaN` 结果就是 `NaN`（Rust 的 `f64::max` 会忽略 `NaN`）。
fn js_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left >= right {
        left
    } else {
        right
    }
}

/// JS 的 `Number.isSafeInteger`。
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER
}

/// JS 的 `String(number)`（`Math.max(...)` 写回 `next_id` 时使用）。
fn js_number_to_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        kiana_json::js_num::format_js_number(value)
    }
}

/// 事务回调里记录原始错误的位置（见模块文档）。
type ErrorSlot = Arc<Mutex<Option<BoxError>>>;

/// 把 `error` 记进槽并转成 [`SqliteError`]（消息逐字保留）。
///
/// `SqliteError` 只保存消息、类别与嵌套错误，无法携带 `StorageRequestError` 等原始
/// `BoxError` 类型；槽保留原始错误，事务结束后由 `unwrap_transaction_error` 换回。
fn fail_transaction(slot: &ErrorSlot, error: BoxError) -> SqliteError {
    let message = error.to_string();
    *slot.lock().expect("sqlite error slot") = Some(error);
    SqliteError::new(message)
}

/// 事务失败后取回原始错误；回滚也失败时保留聚合错误。
///
/// 聚合错误表示回滚本身也没能完成，数据库状态未知——此时原始错误不再能代表这次提交的
/// 后果，保留聚合错误才符合 `SqliteDatabase` 的失败契约（回调错误与回滚失败必须可区分）。
fn unwrap_transaction_error(slot: &ErrorSlot, error: SqliteError) -> BoxError {
    if error.is_aggregate() {
        return Box::new(error);
    }
    match slot.lock().expect("sqlite error slot").take() {
        Some(original) => original,
        None => Box::new(error),
    }
}

fn metadata_missing() -> BoxError {
    error("Durable SQLite metadata is missing")
}

/// TS `cursorId`：游标里的 `after` 必须是安全整数；缺失返回 `None`。
fn cursor_id(cursor: Option<&Cursor>) -> Result<Option<i64>, BoxError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let Some(after) = cursor.get("after") else {
        return Ok(None);
    };
    match crate::types::js_safe_integer(after) {
        Some(value) => Ok(Some(value)),
        None => Err(request_error("Invalid storage cursor")),
    }
}

/// 扫描页的条目形状：需要能给出 ID 以生成续页游标。
trait PageItem {
    fn id_number(&self) -> i64;
}

impl PageItem for ConversationRecord {
    fn id_number(&self) -> i64 {
        self.id.get()
    }
}
impl PageItem for EntryRecord {
    fn id_number(&self) -> i64 {
        self.id.get()
    }
}
impl PageItem for TaskRecord {
    fn id_number(&self) -> i64 {
        self.id.get()
    }
}
impl PageItem for SubmissionRecord {
    fn id_number(&self) -> i64 {
        self.id.get()
    }
}
impl PageItem for DocumentRecord {
    fn id_number(&self) -> i64 {
        self.id.get()
    }
}

/// TS `page(values, limit, order)`。
///
/// `limit == 0` 且仍有数据时返回错误
/// `TypeError: Cannot read properties of undefined (reading 'id')`（当前稳定契约，改动需
/// 同步测试；`usize` 的 0 是合法入参，不能静默返回空页）。
fn page<T: PageItem>(
    mut values: Vec<T>,
    limit: usize,
    order: ScanOrder,
) -> Result<Page<T>, BoxError> {
    let has_more = values.len() > limit;
    if has_more {
        values.truncate(limit);
    }
    if !has_more {
        return Ok(Page {
            items: values,
            next: None,
        });
    }
    let Some(last_id) = values.last().map(PageItem::id_number) else {
        return Err(error("Cannot read properties of undefined (reading 'id')"));
    };
    Ok(Page {
        items: values,
        next: Some(next_cursor(last_id, order)),
    })
}

/// 一次表扫描的 `WHERE` 子句、参数与排序方向（TS `scanSql`）。
struct ScanSql {
    clause: &'static str,
    param: i64,
    direction: &'static str,
}

/// TS `scanSql(start)`。
///
/// 游标是排他的（`id > after` / `id < after`），缺省值取两个方向的全覆盖边界：升序 `-1`、
/// 降序 JS 的 `Number.MAX_SAFE_INTEGER`。`clause`、`param`、`direction` 必须成对使用，
/// 否则会跳过或重复页边界。本文件不校验 ID 的数值范围，显式写入超出这两个边界的 ID 时
/// 首轮扫描看不到它们。
fn scan_sql(start: ScanStart) -> ScanSql {
    match start.order {
        ScanOrder::Ascending => ScanSql {
            clause: "id > ?",
            param: start.after.unwrap_or(-1),
            direction: "ASC",
        },
        ScanOrder::Descending => ScanSql {
            clause: "id < ?",
            param: start.after.unwrap_or(MAX_SAFE_INTEGER_I64),
            direction: "DESC",
        },
    }
}

/// TS `scopeColumns(scope)`：`(scope_kind, owner_id)`。
///
/// Session 作用域没有所有者，固定写 `0`；`owner_id` 只与 `scope_kind` 一起解释。
fn scope_columns(scope: &DocumentScope) -> (&'static str, i64) {
    match scope {
        DocumentScope::Session => ("session", 0),
        DocumentScope::Conversation { conversation_id } => ("conversation", conversation_id.get()),
        DocumentScope::Task { task_id } => ("task", task_id.get()),
    }
}

/// 文档地址的入库列（TS `addressParts` 的返回值）。
struct AddressParts {
    kind: String,
    scope_kind: &'static str,
    owner_id: i64,
    family: i64,
    key_value: String,
}

/// TS `addressParts(address)`。
///
/// `family` 是「有没有 key」的判别列：无 key 记 0，有 key 记 1；`kind` 与 `key_value`
/// 都存 JSON 编码（字符串值带引号），保持与既有库文件的逐字节兼容。`family = 0` 时
/// `key_value` 是空字符串的编码 `""`，不是 SQL NULL。
fn address_parts(kind: &str, scope: &DocumentScope, key: Option<&Value>) -> AddressParts {
    let (scope_kind, owner_id) = scope_columns(scope);
    AddressParts {
        kind: encode_indexed_string(kind),
        scope_kind,
        owner_id,
        family: if key.is_none() { 0 } else { 1 },
        key_value: encode_indexed_key(key),
    }
}

/// TS `addressKey(address)`：`JSON.stringify([...])`。
///
/// 只用于同一提交内的地址去重（`check_document_actions` 的 `live_counts`），不落库，
/// 所以不需要是稳定的列格式；但它的分量顺序必须与 `address_parts` 一致。
fn address_key(kind: &str, scope: &DocumentScope, key: Option<&Value>) -> String {
    let parts = address_parts(kind, scope, key);
    let array = Value::Array(vec![
        Value::String(parts.kind),
        Value::String(parts.scope_kind.to_owned()),
        Value::from(parts.owner_id),
        Value::from(parts.family),
        Value::String(parts.key_value),
    ]);
    kiana_json::js_object::stringify(&array)
}

/// 记录在时间点 `at` 是否存活：`Current` 等价于未退休；`At(seq)` 是半开区间
/// `created_at <= seq < retired_at`（没有退休则上界无限）。退休点所在的提交里文档已不可见。
fn is_alive_at(record: &DocumentRecord, at: DocumentPoint) -> bool {
    match at {
        DocumentPoint::Current => record.retired_at.is_none(),
        DocumentPoint::At(seq) => {
            record.created_at.get() <= seq.get()
                && record
                    .retired_at
                    .is_none_or(|retired| seq.get() < retired.get())
        }
    }
}

/// Session/Task 文档一律是 current-only；conversation 文档仅当
/// `history == Some(Latest)` 时是 current-only，未声明 `history` 或 `Rewindable` 的
/// conversation 文档才保留历史。current-only 的 revision 可以被回收，对它们做历史读会
/// 失败，而不是读到旧值。
fn is_current_only(record: &DocumentRecord) -> bool {
    !matches!(record.scope, DocumentScope::Conversation { .. })
        || record.history == Some(crate::types::DocumentHistory::Latest)
}

/// TS `writeId(write)`：会占用全局 ID 的写入返回其 ID，`document.change`/`retire` 不占新 ID。
///
/// 返回值用于把 `nextId` 候选值推进到「本批最大 ID + 1」，避免后续 `mint_id` 与已写入记录
/// 撞号。
fn write_id(write: &StorageWrite) -> Option<i64> {
    match write {
        StorageWrite::Conversation { value } => Some(value.id.get()),
        StorageWrite::Entry { value } => Some(value.id.get()),
        StorageWrite::Task { value } => Some(value.id.get()),
        StorageWrite::Submission { value } => Some(value.id.get()),
        StorageWrite::DocumentCreate { record, .. } | StorageWrite::DocumentCopy { record, .. } => {
            Some(record.id.get())
        }
        StorageWrite::DocumentChange { .. } | StorageWrite::DocumentRetire { .. } => None,
    }
}

/// TS `TableName`。
///
/// [`TableName::as_str`] 的取值写入 `record_ids.record_type`，迁移里对该列有
/// `CHECK (record_type IN (...))`；新增记录类型时必须同步修改迁移（本文件之外）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableName {
    Conversation,
    Entry,
    Task,
    Submission,
    Document,
}

impl TableName {
    fn as_str(self) -> &'static str {
        match self {
            TableName::Conversation => "conversation",
            TableName::Entry => "entry",
            TableName::Task => "task",
            TableName::Submission => "submission",
            TableName::Document => "document",
        }
    }
}

/// 一个文档在一个提交里的聚合动作（TS `DocumentAction`）。
#[derive(Debug, Clone, Default, PartialEq)]
struct DocumentAction {
    create: Option<DocumentCreate>,
    copy: Option<crate::types::DocumentCopySource>,
    content: Option<DocumentContent>,
    retire: bool,
}

/// TS `prepareDocumentActions(writes)`；`IndexMap` 保持首次出现顺序（与 JS `Map` 一致）。
///
/// 同一文档的多个写入合并成一个 `DocumentAction`：至多一个内容命令（create/copy/change
/// 三者互斥：create 会同时占用 `create` 与 `content` 槽位，copy 占用 `create` 与 `copy`，
/// change 占用 `content`）与至多一次 retire，违反时按 TS 文案报错。`create`/`copy` 与
/// `retire` 可以共存（生成一条创建即退休的墓碑记录），但同一批里「先 create 再 change」
/// 也会被拒绝。顺序本身仍重要：`apply_document_actions` 按首次出现顺序执行动作。
fn prepare_document_actions(
    writes: &[StorageWrite],
) -> Result<indexmap::IndexMap<DocumentId, DocumentAction>, BoxError> {
    let mut actions: indexmap::IndexMap<DocumentId, DocumentAction> = indexmap::IndexMap::new();
    for write in writes {
        let id = match write {
            StorageWrite::DocumentCreate { record, .. }
            | StorageWrite::DocumentCopy { record, .. } => record.id,
            StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => *id,
            _ => continue,
        };
        let action = actions.entry(id).or_default();
        match write {
            StorageWrite::DocumentCreate { record, content } => {
                if action.create.is_some() || action.content.is_some() || action.copy.is_some() {
                    return Err(error(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                action.create = Some(record.clone());
                action.content = Some(DocumentContent::base(
                    content.version,
                    content.value.clone(),
                ));
            }
            StorageWrite::DocumentCopy { record, source } => {
                if action.create.is_some() || action.content.is_some() || action.copy.is_some() {
                    return Err(error(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                action.create = Some(record.clone());
                action.copy = Some(*source);
            }
            StorageWrite::DocumentChange { content, .. } => {
                if action.content.is_some() || action.copy.is_some() {
                    return Err(error(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                action.content = Some(content.clone());
            }
            StorageWrite::DocumentRetire { .. } => {
                if action.retire {
                    return Err(error(format!("Document {id} is retired more than once")));
                }
                action.retire = true;
            }
            _ => unreachable!("only document writes reach the action table"),
        }
    }
    Ok(actions)
}

/// 一行的 `record` 列（TS `JsonRow`）。
///
/// 列缺失或 JSON 无法反序列化成目标类型都是内建错误：库被外部改坏时，读请求会以错误
/// 失败，而不是返回部分字段。
fn parse_record<T: serde::de::DeserializeOwned>(row: &SqliteRow) -> Result<T, BoxError> {
    let Some(record) = row.get_text("record") else {
        return Err(error("record column is missing"));
    };
    parse_json(record)
}

/// 一次查询的全部行 → 记录。
fn parse_records<T: serde::de::DeserializeOwned>(rows: &[SqliteRow]) -> Result<Vec<T>, BoxError> {
    rows.iter().map(parse_record).collect()
}

/// TS 里 `limit + 1 - values.length` 的 `LIMIT` 参数。
///
/// 多取一条用于让 `page` 判断还有没有下一页。`saturating_add`/`saturating_sub` 保证
/// `limit == usize::MAX` 或已取条数超过 limit 时不会溢出/下溢成负的 `LIMIT`。
fn fetch_limit(limit: usize, taken: usize) -> i64 {
    let remaining = limit.saturating_add(1).saturating_sub(taken);
    i64::try_from(remaining).unwrap_or(i64::MAX)
}

/// 任务状态写入 `tasks.status`；取值必须与迁移里的 `CHECK (status IN (...))` 列表一致。
fn task_status_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Waiting => "waiting",
        TaskStatus::Sleeping => "sleeping",
        TaskStatus::Completing => "completing",
        TaskStatus::Terminal => "terminal",
    }
}

/// 提交状态写入 `submissions.status`；取值必须与迁移里的 `CHECK (status IN (...))` 一致。
fn submission_status_str(status: SubmissionStatus) -> &'static str {
    match status {
        SubmissionStatus::Queued => "queued",
        SubmissionStatus::Placed => "placed",
        SubmissionStatus::Done => "done",
        SubmissionStatus::Unanswered => "unanswered",
    }
}

/// `Value::Object` → `JsonObject`；根不是对象时返回 `None`。
fn value_into_json_object(value: Value) -> Option<crate::types::JsonObject> {
    match value {
        Value::Object(map) => Some(map.into_iter().collect()),
        _ => None,
    }
}

/// 可移植 SQLite 之上的 `Storage` 实现（TS: `SqliteStorage`）。
///
/// 不变量：
/// - `schema` 在 `open` 时固定，之后所有 SQL 都用它渲染表名；换前缀必须重新打开存储。
/// - `next_id` 是缓存，重开时从 `durable_metadata.next_id` 重建；提交成功后才把推进后的
///   值写回数据库。
/// - `closed` 单调：`close_result` 置位后，除 `close` 外的所有 `Storage` 方法都返回
///   `SqliteStorage is closed`；关闭前已接纳的读仍会跑完。
/// - `closing` 缓存首次关闭的 `Result`：正常完成时（含 `Err`）会缓存；若初始化 future
///   在 `close_database().await` 期间被取消，则不会缓存，下一次调用会重新执行
///   `close_database`，因此不能保证 `db.close()` 只被调用一次。
///
/// 并发：数据库操作本身由注入的 `SqliteDatabase` 排序（事务由适配器串行化）；本类型自己的
/// 可变状态只有 `next_id` 缓存、`closed` 标志、`closing` 结果与 `admitted` 计数，
/// `std::sync::Mutex` 不跨 `await` 持有。
pub struct SqliteStorage {
    db: Arc<dyn SqliteDatabase>,
    schema: Arc<SqliteSchema>,
    /// TS 的 `nextId`（`Number`）。
    next_id: Mutex<f64>,
    closed: AtomicBool,
    /// TS 的 `closing`：首次 `close()` 启动的关闭任务；初始化正常完成时后续调用返回同一个
    /// 结果（初始化被取消时下一次调用会重新执行，见 [`SqliteStorage::close_result`]）。
    /// TS 的 promise 会以驱动的错误 rejection，这里缓存 `Result` 以便 [`SqliteStorage::close_result`]
    /// 复现同一行为（冻结的 `Storage::close` 没有返回错误的位置）。
    closing: tokio::sync::OnceCell<Result<(), SqliteError>>,
    /// TS 的 `admittedReads`（读的接纳计数）。
    admitted: watch::Sender<usize>,
}

impl std::fmt::Debug for SqliteStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteStorage")
            .finish_non_exhaustive()
    }
}

impl SqliteStorage {
    /// 初始化一个拥有 SQLite 门面的存储（TS: `SqliteStorage.open(db, options)`）。
    ///
    /// 任何初始化失败都会先尝试关闭数据库（错误被吞掉），再抛出最初的失败。
    ///
    /// 失败来源：前缀非法（`create_sqlite_schema` 校验）、迁移失败、`durable_metadata`
    /// 的单行缺失（`Durable SQLite metadata is missing`），或读取该行时的驱动错误。
    pub async fn open(
        db: Arc<dyn SqliteDatabase>,
        options: SqliteStorageOptions,
    ) -> Result<SqliteStorage, BoxError> {
        match SqliteStorage::initialize(Arc::clone(&db), options).await {
            Ok(storage) => Ok(storage),
            Err(failure) => {
                let _ = db.close().await;
                Err(failure)
            }
        }
    }

    // 迁移与元数据读取都在 `open` 的失败清理范围内：这里出错不负责关库，由 `open` 统一收尾。
    async fn initialize(
        db: Arc<dyn SqliteDatabase>,
        options: SqliteStorageOptions,
    ) -> Result<SqliteStorage, BoxError> {
        let schema = create_sqlite_schema(options.prefix.as_deref())
            .map_err(|failure| Box::new(failure) as BoxError)?;
        let schema = Arc::new(schema);
        apply_sqlite_migrations_for_schema(db.as_ref(), &schema)
            .await
            .map_err(|failure| Box::new(failure) as BoxError)?;
        let row = db
            .get(
                &format!(
                    "SELECT next_id, next_seq FROM {} WHERE singleton = 1",
                    schema.tables.durable_metadata
                ),
                &[],
            )
            .await
            .map_err(|failure| Box::new(failure) as BoxError)?;
        let Some(row) = row else {
            return Err(metadata_missing());
        };
        // TS: `Number(metadata.next_id)`。非数字文本在 JS 里是 `NaN`，这里保持一致。
        let next_id = row
            .get_text("next_id")
            .map(kiana_json::js_num::parse_js_number)
            .unwrap_or(f64::NAN);
        Ok(SqliteStorage {
            db,
            schema,
            next_id: Mutex::new(next_id),
            closed: AtomicBool::new(false),
            closing: tokio::sync::OnceCell::new(),
            admitted: watch::channel(0).0,
        })
    }

    /// 当前 `nextId`（测试与诊断用；TS 里是私有字段）。
    ///
    /// 持久化文本不是合法数字时（`Number(...)` → `NaN`）这里就是 `NaN`；此时 `mint_id`
    /// 会报 `ID space is exhausted`。
    pub fn next_id(&self) -> f64 {
        *self.next_id.lock().expect("sqlite next id")
    }

    fn assert_open(&self) -> Result<(), BoxError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(error("SqliteStorage is closed"))
        } else {
            Ok(())
        }
    }

    /// TS `candidateNextId(writes)`：从内存 `nextId` 出发，取「每条占用 ID 的写入 + 1」的
    /// 最大值。
    ///
    /// 候选值在事务内用于写回 `durable_metadata.next_id`，并在提交成功后用于刷新内存
    /// `nextId`；计算本身不修改缓存，提交失败时缓存不推进。
    fn candidate_next_id(&self, writes: &[StorageWrite]) -> f64 {
        let mut next_id = *self.next_id.lock().expect("sqlite next id");
        for write in writes {
            if let Some(id) = write_id(write) {
                next_id = js_max(next_id, id as f64 + 1.0);
            }
        }
        next_id
    }

    /// TS `admitRead(read)`：关闭存储要等已接纳的读读完。
    ///
    /// `close` 等的是「计数归零」，不是「任务结束」：计数器在第一次 `.await` 之前递增、
    /// 由 `AdmittedRead` 的 `Drop` 递减（等价 TS 的 `finally`）。计数范围是精确清单：
    /// `entry`/`visible_entry`/`find_latest_head_marker`/`scan_entries` 经这里计数（`entry`
    /// 的全局重载只发一条查询也会计数，与 TS 对两个重载统一 `admitRead` 一致）；不在清单里
    /// 的读与 `commit` 不计数，它们与 `close` 的相对顺序依赖适配器契约。对沿祖先链多次查询
    /// 的读，接纳保证关闭不会插在查询之间。
    ///
    /// 注意：`assert_open` 与随后的计数递增是两条独立语句，中间没有互斥。若
    /// `close_result` 在这两步之间置位 `closed` 并观察到计数为 0，它就会开始关库，随后
    /// 递增的读可能打到已关闭的连接。单线程执行（TS）不会出现该交错，多线程运行时则由
    /// 调度时序决定。
    async fn admit_read<T>(
        &self,
        read: impl Future<Output = Result<T, BoxError>>,
    ) -> Result<T, BoxError> {
        self.assert_open()?;
        self.admitted.send_modify(|count| *count += 1);
        let guard = AdmittedRead {
            admitted: &self.admitted,
        };
        let result = read.await;
        drop(guard);
        result
    }

    async fn read_conversation(
        &self,
        id: ConversationId,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        let row = self
            .db
            .get(
                &format!(
                    "SELECT record FROM {} WHERE id = ?",
                    self.schema.tables.conversations
                ),
                &[int(id.get())],
            )
            .await?;
        match row {
            Some(row) => Ok(Some(parse_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn read_entry_global(&self, id: EntryId) -> Result<Option<StoredEntry>, BoxError> {
        let row = self
            .db
            .get(
                &format!(
                    "SELECT record, commit_seq FROM {} WHERE id = ?",
                    self.schema.tables.entries
                ),
                &[int(id.get())],
            )
            .await?;
        stored_entry(row.as_ref())
    }

    /// TS `readEntry` 的“会话 + 条目”重载（`Storage.visible_entry`）。
    ///
    /// 可见性规则：从请求会话沿 `parent` 链向上走，每跨一代把上界收紧为
    /// `min(已收紧的上界, parent.at)`；只有条目所属会话处在这条链上、且条目 ID 不超过
    /// 所有 fork 点的最小上界时才可见。请求会话不存在是 `StorageRequestError`
    /// （只让这次读失败）；链上某个祖先会话缺失时按 TS 的非空断言失败，错误文案
    /// `Cannot read properties of undefined (reading 'id')` 属于内建错误。
    ///
    /// 这里的多次查询正是它必须经 `admit_read` 接纳的原因：中途关库会让后续查询打到
    /// 已关闭的连接。
    async fn read_visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
    ) -> Result<Option<StoredEntry>, BoxError> {
        let mut conversation = self.read_conversation(conversation_id).await?;
        let Some(mut current) = conversation.take() else {
            return Err(request_error(format!(
                "Unknown conversation: {conversation_id}"
            )));
        };
        let row = self
            .db
            .get(
                &format!(
                    "SELECT record, commit_seq FROM {} WHERE id = ?",
                    self.schema.tables.entries
                ),
                &[int(id.get())],
            )
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let entry: EntryRecord = parse_record(&row)?;
        let mut upper: Option<i64> = None;
        while current.id != entry.conversation_id {
            let Some(parent) = current.parent else {
                return Ok(None);
            };
            upper = Some(match upper {
                Some(value) => value.min(parent.at.get()),
                None => parent.at.get(),
            });
            current = self
                .read_conversation(parent.conversation_id)
                .await?
                .ok_or_else(|| error("Cannot read properties of undefined (reading 'id')"))?;
        }
        if let Some(upper) = upper
            && entry.id.get() > upper
        {
            return Ok(None);
        }
        let Some(commit_seq) = row.get_i64("commit_seq") else {
            return Err(error("commit_seq column is missing"));
        };
        Ok(Some(StoredEntry {
            entry,
            commit_seq: seq_from_number(commit_seq),
        }))
    }

    /// TS `readLatestHeadMarker`。
    ///
    /// 先在请求会话里找 `head IS NOT NULL` 且 `id <= 上界` 的最新一条；找不到就带上
    /// `min(上界, parent.at)` 去父会话继续，直到链根。每段都用 `id DESC LIMIT 1`，所以
    /// 返回的是「离上界最近的祖先 head」，而不是根会话里的第一条 head。
    async fn read_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
    ) -> Result<Option<EntryRecord>, BoxError> {
        let mut conversation = self
            .read_conversation(conversation_id)
            .await?
            .ok_or_else(|| request_error(format!("Unknown conversation: {conversation_id}")))?;
        let mut upper: Option<i64> = at_or_before_entry_id.map(|id| id.get());
        loop {
            let row = match upper {
                None => {
                    self.db
                        .get(
                            &format!(
                                "SELECT record FROM {} WHERE conversation_id = ? AND head IS NOT NULL ORDER BY id DESC LIMIT 1",
                                self.schema.tables.entries
                            ),
                            &[int(conversation.id.get())],
                        )
                        .await?
                }
                Some(upper) => {
                    self.db
                        .get(
                            &format!(
                                "SELECT record FROM {} WHERE conversation_id = ? AND head IS NOT NULL AND id <= ? ORDER BY id DESC LIMIT 1",
                                self.schema.tables.entries
                            ),
                            &[int(conversation.id.get()), int(upper)],
                        )
                        .await?
                }
            };
            if let Some(row) = row {
                return Ok(Some(parse_record(&row)?));
            }
            let Some(parent) = conversation.parent else {
                return Ok(None);
            };
            upper = Some(match upper {
                Some(value) => value.min(parent.at.get()),
                None => parent.at.get(),
            });
            conversation = self
                .read_conversation(parent.conversation_id)
                .await?
                .ok_or_else(|| error("Cannot read properties of undefined (reading 'id')"))?;
        }
    }

    /// TS `readEntries`（降序）。
    ///
    /// `scan_start` 先解析顺序与游标：带游标时以游标里的 `order` 为准，查询给出不同顺序会
    /// 报错（见 `scan_start`）。降序路径：
    /// 1. 从请求会话开始沿 `parent` 链把可见历史切成若干段：第一段的上界是
    ///    `min(max_entry_id, 游标 after - 1)`；每跨到父会话，上界再与 `parent.at` 取 min
    ///    （父会话在 fork 点之后的条目不属于这条链）；
    /// 2. 每段执行一次 `ORDER BY id DESC LIMIT (limit + 1 - 已取条数)`，取满 `limit + 1`
    ///    条就停止继续往父会话走；
    /// 3. 各段结果按「子 → 父」直接拼接，不再做全局排序（与 TS 相同）。
    ///
    /// 因此跨段顺序取决于写入时分配的 ID；本层不校验 ID 的全局单调性。段内用闭区间
    /// `id >= min_entry_id` 与 `id <= 上界` 过滤。
    async fn read_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<EntryRecord>, BoxError> {
        let start = scan_start(query.order, cursor, ScanOrder::Descending)?;
        if start.order == ScanOrder::Ascending {
            return self.read_entries_ascending(query, limit, start.after).await;
        }
        let mut conversation = self
            .read_conversation(query.conversation_id)
            .await?
            .ok_or_else(|| {
                request_error(format!("Unknown conversation: {}", query.conversation_id))
            })?;
        let mut upper: Option<i64> = query.max_entry_id.map(|id| id.get());
        if let Some(after) = start.after {
            upper = Some(match upper {
                Some(value) => value.min(after - 1),
                None => after - 1,
            });
        }
        let mut values: Vec<EntryRecord> = Vec::new();
        loop {
            let mut clauses = vec!["conversation_id = ?".to_owned()];
            let mut params = vec![int(conversation.id.get())];
            if let Some(min) = query.min_entry_id {
                clauses.push("id >= ?".to_owned());
                params.push(int(min.get()));
            }
            if let Some(upper) = upper {
                clauses.push("id <= ?".to_owned());
                params.push(int(upper));
            }
            params.push(int(fetch_limit(limit, values.len())));
            let rows = self
                .db
                .all(
                    &format!(
                        "SELECT record FROM {} WHERE {} ORDER BY id DESC LIMIT ?",
                        self.schema.tables.entries,
                        clauses.join(" AND ")
                    ),
                    &params,
                )
                .await?;
            values.extend(parse_records::<EntryRecord>(&rows)?);
            let Some(parent) = conversation.parent else {
                break;
            };
            if values.len() > limit {
                break;
            }
            upper = Some(match upper {
                Some(value) => value.min(parent.at.get()),
                None => parent.at.get(),
            });
            if let Some(min) = query.min_entry_id
                && upper.expect("upper is set above") < min.get()
            {
                break;
            }
            conversation = self
                .read_conversation(parent.conversation_id)
                .await?
                .ok_or_else(|| error("Cannot read properties of undefined (reading 'id')"))?;
        }
        page(values, limit, ScanOrder::Descending)
    }

    /// TS `readEntriesAscending`：最旧的在前——fork 链的段从根会话开始，每段到它的 fork 点。
    ///
    /// 与降序相反，这里先从请求会话向上收集相关祖先段（请求会话先入、根最后入；
    /// `min_entry_id` 命中时提前停止），再 `rev()` 从根到叶扫描。这些段在第一次表扫描前
    /// 建好，因此被收集范围内的缺失祖先会在任何页返回前报错；TS 的 `readEntriesAscending`
    /// 同样在首次查询前建好这些段。`after` 转成闭区间下界
    /// `after + 1` 并与 `min_entry_id` 取 max；每段都用同一对 `(lower, segment_upper)`
    /// 过滤。段结果直接拼接，与降序路径一样不做跨段归并排序。
    async fn read_entries_ascending(
        &self,
        query: &EntryQuery,
        limit: usize,
        after: Option<i64>,
    ) -> Result<Page<EntryRecord>, BoxError> {
        let mut segments: Vec<(ConversationId, Option<i64>)> = Vec::new();
        let mut conversation = self
            .read_conversation(query.conversation_id)
            .await?
            .ok_or_else(|| {
                request_error(format!("Unknown conversation: {}", query.conversation_id))
            })?;
        let mut upper: Option<i64> = query.max_entry_id.map(|id| id.get());
        loop {
            segments.push((conversation.id, upper));
            let Some(parent) = conversation.parent else {
                break;
            };
            upper = Some(match upper {
                Some(value) => value.min(parent.at.get()),
                None => parent.at.get(),
            });
            if let Some(min) = query.min_entry_id
                && upper.expect("upper is set above") < min.get()
            {
                break;
            }
            conversation = self
                .read_conversation(parent.conversation_id)
                .await?
                .ok_or_else(|| error("Cannot read properties of undefined (reading 'id')"))?;
        }
        let mut lower: Option<i64> = query.min_entry_id.map(|id| id.get());
        if let Some(after) = after {
            lower = Some(match lower {
                Some(value) => value.max(after + 1),
                None => after + 1,
            });
        }
        let mut values: Vec<EntryRecord> = Vec::new();
        for (conversation_id, segment_upper) in segments.into_iter().rev() {
            let mut clauses = vec!["conversation_id = ?".to_owned()];
            let mut params = vec![int(conversation_id.get())];
            if let Some(lower) = lower {
                clauses.push("id >= ?".to_owned());
                params.push(int(lower));
            }
            if let Some(segment_upper) = segment_upper {
                clauses.push("id <= ?".to_owned());
                params.push(int(segment_upper));
            }
            params.push(int(fetch_limit(limit, values.len())));
            let rows = self
                .db
                .all(
                    &format!(
                        "SELECT record FROM {} WHERE {} ORDER BY id ASC LIMIT ?",
                        self.schema.tables.entries,
                        clauses.join(" AND ")
                    ),
                    &params,
                )
                .await?;
            values.extend(parse_records::<EntryRecord>(&rows)?);
            if values.len() > limit {
                break;
            }
        }
        page(values, limit, ScanOrder::Ascending)
    }
}

/// `admitRead` 的计数器守卫：`Drop` 时递减（保证所有退出路径都会归还计数）。
///
/// 读的 future 被取消时守卫也会 `Drop`，因此取消一个已接纳的读不会让 `close` 永远等下去；
/// `saturating_sub` 只是防御重复递减，正常路径上计数不会被减到 0 以下。
struct AdmittedRead<'a> {
    admitted: &'a watch::Sender<usize>,
}

impl Drop for AdmittedRead<'_> {
    fn drop(&mut self) {
        self.admitted
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

/// 查询 `documents` 里某个地址的当前化身 ID（TS `currentDocumentId`）。
///
/// 地址五列都用 `address_parts` 编码，条件固定 `retired_at IS NULL LIMIT 1`。返回值只用来
/// 给 `check_document_actions` 的存活计数定初值：这里只取一条并把存活计数初始化为 1（不是
/// 行数）。本批若在同一地址再 `create` 且不 `retire`，计数会超过 1 并在计数阶段报
/// `Document address already has a current incarnation`；仅 `change`/`retire` 不会发现
/// 既有的重复行。
async fn current_document_id(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    kind: &str,
    scope: &DocumentScope,
    key: Option<&Value>,
) -> Result<Option<DocumentId>, BoxError> {
    let parts = address_parts(kind, scope, key);
    let row = executor
        .get(
            &format!(
                "SELECT id FROM {} \
                 WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ? AND retired_at IS NULL \
                 LIMIT 1",
                schema.tables.documents
            ),
            &[
                text(parts.kind),
                text(parts.scope_kind),
                int(parts.owner_id),
                int(parts.family),
                text(parts.key_value),
            ],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(row
        .get_i64("id")
        .map(crate::types::id_from_number::<crate::types::DocumentBrand>))
}

/// TS `checkGlobalIds(executor, writes)`。
///
/// `record_ids` 是全局 ID 命名空间，同一数字 ID 只能属于一种记录类型。规则分两类：
/// - conversation/entry/document 只能由本批**第一次**声明：库里已有或批内重复都报错；
/// - task/submission 允许覆盖写：库里已存在时必须同类型，批内重复也要求同类型
///   （`apply_table_write` 的 `ON CONFLICT(id) DO UPDATE` 依赖这一点）。
///
/// 检查发生在任何表写入之前，因此这一阶段失败时不会留下半批记录。
async fn check_global_ids(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    writes: &[StorageWrite],
) -> Result<(), BoxError> {
    let mut claimed: indexmap::IndexMap<i64, TableName> = indexmap::IndexMap::new();
    for write in writes {
        let (table, id) = match write {
            StorageWrite::DocumentChange { .. } | StorageWrite::DocumentRetire { .. } => continue,
            StorageWrite::DocumentCreate { record, .. }
            | StorageWrite::DocumentCopy { record, .. } => (TableName::Document, record.id.get()),
            StorageWrite::Conversation { value } => (TableName::Conversation, value.id.get()),
            StorageWrite::Entry { value } => (TableName::Entry, value.id.get()),
            StorageWrite::Task { value } => (TableName::Task, value.id.get()),
            StorageWrite::Submission { value } => (TableName::Submission, value.id.get()),
        };
        let row = executor
            .get(
                &format!(
                    "SELECT record_type FROM {} WHERE id = ?",
                    schema.tables.record_ids
                ),
                &[int(id)],
            )
            .await?;
        let existing = row
            .as_ref()
            .and_then(|row| row.get_text("record_type"))
            .map(str::to_owned);
        let earlier = claimed.get(&id).copied();
        if matches!(
            table,
            TableName::Conversation | TableName::Entry | TableName::Document
        ) {
            if let Some(existing) = &existing {
                return Err(error(format!("ID {id} already belongs to {existing}")));
            }
            if earlier.is_some() {
                return Err(error(format!("ID {id} is written more than once")));
            }
        } else {
            if let Some(existing) = &existing
                && existing != table.as_str()
            {
                return Err(error(format!("ID {id} already belongs to {existing}")));
            }
            if let Some(earlier) = earlier
                && earlier != table
            {
                return Err(error(format!("ID {id} is written as two record types")));
            }
        }
        claimed.insert(id, table);
    }
    Ok(())
}

/// TS `checkDocumentActions(executor, actions)`。
///
/// 在写入前验证一批文档动作能保持以下不变量：
/// - 复制源不能同时出现在本批动作里（源会被同一批改写，复制结果因此取决于动作顺序）；
/// - 新建要求库里不存在，修改/退休要求已存在；已存在的记录不能已退休；
/// - delta 必须接在最后一条 revision 之后：没有 revision 报 `delta has no base`，
///   最后一条 revision 的 `version` 不一致报 `version transition requires a base`
///   （换版本必须先写 base）；
/// - 同一逻辑地址（kind + scope + key）提交后最多一个未退休化身：先按需查询库里现值，
///   再按「退休 -1 / 新建 +1」累计；所有动作处理完后任何地址计数 > 1 就失败。
///
/// 新建与退休可以出现在同一个动作里：此时只会插入一条 `created_at == retired_at == seq`
/// 的墓碑记录（执行阶段负责），存活计数不会因为这次「创建」而增加。
async fn check_document_actions(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    actions: &indexmap::IndexMap<DocumentId, DocumentAction>,
) -> Result<(), BoxError> {
    let mut live_counts: indexmap::IndexMap<String, i64> = indexmap::IndexMap::new();
    for (id, action) in actions {
        if let Some(copy) = &action.copy
            && actions.contains_key(&copy.id)
        {
            return Err(error(format!(
                "Document copy {id} source is changed in the copy batch"
            )));
        }
        let row = executor
            .get(
                &format!(
                    "SELECT record FROM {} WHERE id = ?",
                    schema.tables.documents
                ),
                &[int(id.get())],
            )
            .await?;
        let existing: Option<DocumentRecord> = match &row {
            Some(row) => Some(parse_record(row)?),
            None => None,
        };
        if action.create.is_none() && existing.is_none() {
            return Err(error(format!("Unknown document: {id}")));
        }
        if action.create.is_some() && existing.is_some() {
            return Err(error(format!("Document {id} already exists")));
        }
        if existing
            .as_ref()
            .is_some_and(|record| record.retired_at.is_some())
        {
            return Err(error(format!("Document {id} is retired")));
        }
        if let Some(content) = &action.content
            && content.kind == DocumentContentKind::Delta
        {
            let previous = executor
                .get(
                    &format!(
                        "SELECT version FROM {} WHERE document_id = ? ORDER BY seq DESC LIMIT 1",
                        schema.tables.document_revisions
                    ),
                    &[int(id.get())],
                )
                .await?;
            let Some(previous) = previous else {
                return Err(error(format!("Document {id} delta has no base")));
            };
            if previous.get_i64("version") != Some(content.version) {
                return Err(error(format!(
                    "Document {id} version transition requires a base"
                )));
            }
        }
        let (kind, scope, key) = match (&action.create, &existing) {
            (Some(create), _) => (create.kind.as_str(), &create.scope, create.key.as_ref()),
            (None, Some(record)) => (record.kind.as_str(), &record.scope, record.key.as_ref()),
            (None, None) => unreachable!("checked above"),
        };
        let key = address_key(kind, scope, key);
        let mut live = match live_counts.get(&key).copied() {
            Some(live) => live,
            None => {
                if current_document_id(
                    executor,
                    schema,
                    kind,
                    scope,
                    key_of(&action.create, &existing),
                )
                .await?
                .is_some()
                {
                    1
                } else {
                    0
                }
            }
        };
        if action.retire && existing.is_some() {
            live -= 1;
        }
        if action.create.is_some() && !action.retire {
            live += 1;
        }
        live_counts.insert(key, live);
    }
    for live in live_counts.values() {
        if *live > 1 {
            return Err(error("Document address already has a current incarnation"));
        }
    }
    Ok(())
}

/// 当前动作地址的 `key`（与 [`address_key`] 使用同一个值）。
///
/// `create` 优先于 `existing`：create 动作的目标记录就是本次要写入的记录；两者在
/// `check_document_actions` 里最多只有一个存在。
fn key_of<'a>(
    create: &'a Option<DocumentCreate>,
    existing: &'a Option<DocumentRecord>,
) -> Option<&'a Value> {
    match (create, existing) {
        (Some(create), _) => create.key.as_ref(),
        (None, Some(record)) => record.key.as_ref(),
        (None, None) => None,
    }
}

/// TS `applyTableWrite(executor, write, seq)`。
///
/// 只处理四种「记录」写入；四种文档写入由 `apply_document_actions` 单独处理（这里显式
/// no-op）。conversation/entry 是纯 INSERT（`check_global_ids` 已保证 ID 未被占用）；
/// task/submission 用 `ON CONFLICT(id) DO UPDATE` 覆盖同一 ID 的旧行，因此同一任务/提交
/// 可以在多批提交之间原地更新状态。索引列（`task.kind`、`submission.request_id`）写入的是
/// JSON 编码后的字符串，过滤查询必须用同一个 `encode_indexed_string` 才能命中。
async fn apply_table_write(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    write: &StorageWrite,
    seq: Seq,
) -> Result<(), BoxError> {
    match write {
        StorageWrite::Conversation { value } => {
            claim_id(executor, schema, value.id.get(), TableName::Conversation).await?;
            executor
                .run(
                    &format!(
                        "INSERT INTO {} (id, owner_conversation_id, owner_task_id, record) VALUES (?, ?, ?, ?)",
                        schema.tables.conversations
                    ),
                    &[
                        int(value.id.get()),
                        value
                            .owner
                            .map_or(SqliteValue::Null, |owner| int(owner.conversation_id.get())),
                        value
                            .owner
                            .map_or(SqliteValue::Null, |owner| int(owner.task_id.get())),
                        text(encode_json(value)?),
                    ],
                )
                .await?;
        }
        StorageWrite::Entry { value } => {
            claim_id(executor, schema, value.id.get(), TableName::Entry).await?;
            executor
                .run(
                    &format!(
                        "INSERT INTO {} (id, conversation_id, head, commit_seq, record) VALUES (?, ?, ?, ?, ?)",
                        schema.tables.entries
                    ),
                    &[
                        int(value.id.get()),
                        int(value.conversation_id.get()),
                        value.head.map_or(SqliteValue::Null, |head| int(head.get())),
                        int(seq.get()),
                        text(encode_json(value)?),
                    ],
                )
                .await?;
        }
        StorageWrite::Task { value } => {
            claim_id(executor, schema, value.id.get(), TableName::Task).await?;
            executor
                .run(
                    &format!(
                        "INSERT INTO {} (id, conversation_id, kind, status, abort_requested, background, record)\n\
                         \t\t\t\t\t\tVALUES (?, ?, ?, ?, ?, ?, ?)\n\
                         \t\t\t\t\t\tON CONFLICT(id) DO UPDATE SET conversation_id = excluded.conversation_id, kind = excluded.kind,\n\
                         \t\t\t\t\t\tstatus = excluded.status, abort_requested = excluded.abort_requested,\n\
                         \t\t\t\t\t\tbackground = excluded.background, record = excluded.record",
                        schema.tables.tasks
                    ),
                    &[
                        int(value.id.get()),
                        int(value.conversation_id.get()),
                        text(encode_indexed_string(&value.kind)),
                        text(task_status_str(value.state.status())),
                        int(if value.abort_requested { 1 } else { 0 }),
                        int(if value.background { 1 } else { 0 }),
                        text(encode_json(value)?),
                    ],
                )
                .await?;
        }
        StorageWrite::Submission { value } => {
            claim_id(executor, schema, value.id.get(), TableName::Submission).await?;
            executor
                .run(
                    &format!(
                        "INSERT INTO {} (id, conversation_id, request_id, status, record) VALUES (?, ?, ?, ?, ?)\n\
                         \t\t\t\t\t\tON CONFLICT(id) DO UPDATE SET conversation_id = excluded.conversation_id,\n\
                         \t\t\t\t\t\trequest_id = excluded.request_id, status = excluded.status, record = excluded.record",
                        schema.tables.submissions
                    ),
                    &[
                        int(value.id.get()),
                        int(value.conversation_id.get()),
                        match &value.request_id {
                            Some(request_id) => text(encode_indexed_string(request_id)),
                            None => SqliteValue::Null,
                        },
                        text(submission_status_str(value.status)),
                        text(encode_json(value)?),
                    ],
                )
                .await?;
        }
        StorageWrite::DocumentCreate { .. }
        | StorageWrite::DocumentCopy { .. }
        | StorageWrite::DocumentChange { .. }
        | StorageWrite::DocumentRetire { .. } => {}
    }
    Ok(())
}

/// TS `claimId(executor, id, table)`。
///
/// `INSERT OR IGNORE`：已存在的行保持原类型。类型冲突在本函数之前已由
/// `check_global_ids` 拒绝；这里只负责登记 id → 记录类型，供后续批次做碰撞检查。
async fn claim_id(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    id: i64,
    table: TableName,
) -> Result<(), BoxError> {
    executor
        .run(
            &format!(
                "INSERT OR IGNORE INTO {} (id, record_type) VALUES (?, ?)",
                schema.tables.record_ids
            ),
            &[int(id), text(table.as_str())],
        )
        .await?;
    Ok(())
}

/// TS `applyDocumentActions(executor, actions, seq)`。
///
/// 按 `prepare_document_actions` 保留的首次出现顺序逐个文档执行，单个文档的顺序是：
/// 1. copy：物化源文档，把它的 `(version, value)` 当作本次 base；源必须仍是 conversation
///    作用域，且 kind/key/history/fork 与目标记录完全一致，否则把失败包成
///    `DocumentCopyRejected`（消息 `Document copy {id} was rejected`，原始错误在 `cause`）；
/// 2. create：登记 ID 并插入 `documents` 行，`created_at = seq`；若同一动作要求退休，
///    则 `retired_at` 也是 `seq`（墓碑）；
/// 3. 否则读回已有记录（`check_document_actions` 已确认它存在）。
/// 4. 有内容时插入一条 revision：current-only 文档的 base 会先删掉全部旧 revision，
///    只保留本次 base，避免库随 delta 数量增长；delta 存已解码 `Op[]` 的 JSON，缺内容时
///    绑定 NULL，让 `content TEXT NOT NULL` 以驱动错误失败（与 TS 绑定 `undefined` 一致）；
/// 5. retire：非新建记录 UPDATE `retired_at` 与 `record` JSON；current-only 文档删除全部
///    revision（墓碑不再需要内容）。新建+退休时第 2 步已写好 `retired_at`，不再 UPDATE。
///
/// 这些步骤仍在 `commit` 的同一个事务里，任何一步失败都会回滚整批。
async fn apply_document_actions(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    actions: &indexmap::IndexMap<DocumentId, DocumentAction>,
    seq: Seq,
) -> Result<(), BoxError> {
    for (id, action) in actions {
        let mut content = action.content.clone();
        if let Some(copy) = &action.copy {
            let stored = match materialize_document(executor, schema, copy.id, copy.at).await {
                Ok(stored) => stored,
                Err(failure) => {
                    return Err(Box::new(DocumentCopyRejected {
                        id: *id,
                        cause: failure,
                    }));
                }
            };
            let Some(stored) = stored else {
                return Err(Box::new(DocumentCopyRejected {
                    id: *id,
                    cause: error(format!("Fork source document {} cannot be read", copy.id)),
                }));
            };
            let create = action
                .create
                .as_ref()
                .expect("copy actions always carry their record");
            if !matches!(stored.record.scope, DocumentScope::Conversation { .. })
                || !matches!(create.scope, DocumentScope::Conversation { .. })
                || stored.record.kind != create.kind
                || stored.record.key != create.key
                || stored.record.history != create.history
                || stored.record.fork != create.fork
            {
                return Err(Box::new(DocumentCopyRejected {
                    id: *id,
                    cause: error(format!(
                        "Fork source document {} does not match the copied record",
                        copy.id
                    )),
                }));
            }
            content = Some(DocumentContent::base(stored.version, stored.value));
        }

        let mut record = if let Some(create) = &action.create {
            let record = DocumentRecord {
                id: create.id,
                kind: create.kind.clone(),
                key: create.key.clone(),
                scope: create.scope,
                history: create.history,
                fork: create.fork,
                created_at: seq,
                retired_at: action.retire.then_some(seq),
            };
            let parts = address_parts(&record.kind, &record.scope, record.key.as_ref());
            claim_id(executor, schema, id.get(), TableName::Document).await?;
            executor
                .run(
                    &format!(
                        "INSERT INTO {} \
                         (id, kind, family, key_value, scope_kind, owner_id, created_at, retired_at, record)\n\
                         \t\t\t\t\t\tVALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        schema.tables.documents
                    ),
                    &[
                        int(id.get()),
                        text(parts.kind),
                        int(parts.family),
                        text(parts.key_value),
                        text(parts.scope_kind),
                        int(parts.owner_id),
                        int(seq.get()),
                        if action.retire {
                            int(seq.get())
                        } else {
                            SqliteValue::Null
                        },
                        text(encode_json(&record)?),
                    ],
                )
                .await?;
            record
        } else {
            let row = executor
                .get(
                    &format!(
                        "SELECT record FROM {} WHERE id = ?",
                        schema.tables.documents
                    ),
                    &[int(id.get())],
                )
                .await?
                .ok_or_else(|| error(format!("Unknown document: {id}")))?;
            parse_record(&row)?
        };

        if let Some(content) = &content {
            if content.kind == DocumentContentKind::Base && is_current_only(&record) {
                executor
                    .run(
                        &format!(
                            "DELETE FROM {} WHERE document_id = ?",
                            schema.tables.document_revisions
                        ),
                        &[int(id.get())],
                    )
                    .await?;
            }
            // TS: `content.kind === "base" ? encodeJson(content.value) : encodeJson(content.ops)`。
            // 缺失时 `JSON.stringify(undefined)` 是 `undefined`，node:sqlite 把它绑成 NULL，
            // 随后 `content TEXT NOT NULL` 约束以驱动错误失败；这里绑定 NULL 复现同一失败。
            let encoded = match content.kind {
                DocumentContentKind::Base => match &content.value {
                    None => SqliteValue::Null,
                    Some(value) => text(encode_json(value)?),
                },
                DocumentContentKind::Delta => match &content.ops {
                    None => SqliteValue::Null,
                    // `JSON.stringify(ops)` 存的是**已解码**的 `Op[]`（不是 wire 压缩形式），
                    // 这样 sqlite 与 TS 写出的 content 字节一致，读取端也按已解码 op 校验。
                    Some(ops) => text(encode_json(&ops_to_json(ops))?),
                },
            };
            executor
                .run(
                    &format!(
                        "INSERT INTO {} (document_id, seq, kind, version, content) VALUES (?, ?, ?, ?, ?)",
                        schema.tables.document_revisions
                    ),
                    &[
                        int(id.get()),
                        int(seq.get()),
                        text(match content.kind {
                            DocumentContentKind::Base => "base",
                            DocumentContentKind::Delta => "delta",
                        }),
                        int(content.version),
                        encoded,
                    ],
                )
                .await?;
        }

        if action.retire {
            if action.create.is_none() {
                record.retired_at = Some(seq);
                executor
                    .run(
                        &format!(
                            "UPDATE {} SET retired_at = ?, record = ? WHERE id = ?",
                            schema.tables.documents
                        ),
                        &[int(seq.get()), text(encode_json(&record)?), int(id.get())],
                    )
                    .await?;
            }
            if is_current_only(&record) {
                executor
                    .run(
                        &format!(
                            "DELETE FROM {} WHERE document_id = ?",
                            schema.tables.document_revisions
                        ),
                        &[int(id.get())],
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

/// TS `apply(value, parseJson<readonly Op[]>(revision.content))`：逐条校验并应用 revision 内容。
///
/// `JSON.parse` 之后 `for...of` 的真实语义决定错误形状：
/// - JSON 数组：逐条形如 `assertValidOp`（[`kiana_chord::delta::index::parse_op`]），失败时报
///   `TypeError` 文案（`op is not a tuple`、`s arity`、`a shape` …），并**边校验边应用**以保持
///   错误优先级；
/// - JSON 字符串：V8 逐码点迭代，每个字符都不是元组 → `op is not a tuple`（空串没有 op）；
/// - 其余（数字、布尔、`null`、对象）：V8 抛 `ops is not iterable`。
fn apply_revision_ops(value: Value, content: &str) -> Result<Value, BoxError> {
    let parsed: Value = parse_json(content)?;
    match parsed {
        Value::Array(items) => {
            let mut value = value;
            for item in &items {
                let op = kiana_chord::delta::index::parse_op(item)
                    .map_err(|failure| Box::new(failure) as BoxError)?;
                value =
                    kiana_chord::delta::index::apply_immutable(value, std::slice::from_ref(&op))
                        .map_err(|failure| Box::new(failure) as BoxError)?;
            }
            Ok(value)
        }
        Value::String(text) if text.is_empty() => Ok(value),
        Value::String(_) => Err(Box::new(kiana_chord::delta::index::OpError::Invalid(
            "op is not a tuple",
        ))),
        _ => Err(error("ops is not iterable")),
    }
}

/// TS `materializeDocument(executor, id, at)`。
///
/// 读取一个化身在 `at` 的值：
/// - 未知 ID → `None`；记录在该时间点不存活（尚未创建或已退休）→ `None`；
/// - 对 current-only 文档做历史读（`at` 不是 `Current`）→ `StorageRequestError`，因为对应
///   的 revision 已被回收；
/// - 取 `seq <= at`（`Current` 取 `Number.MAX_SAFE_INTEGER`）的最后一条 base，再按 `seq`
///   增序回放区间
///   `(base.seq, at]` 的 revision；回放中遇到 `kind != "delta"` 或 `version != base.version`
///   说明跨过了版本边界而缺少新 base，报错；
/// - 物化结果必须是 JSON 对象：TS 只做 `as JsonObject` 断言，Rust 的 `JsonObject` 无法表示
///   非对象根，因此显式报错（类型契约的已知偏差）。
///
/// 回放用 `apply_revision_ops`（按已解码 `Op[]` 校验），不能用 wire 解码：库里的 content
/// 是 `JSON.stringify(Op[])`，wire 的短形式与 `#` 引用在这里非法。
async fn materialize_document(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    id: DocumentId,
    at: DocumentPoint,
) -> Result<Option<StoredDocument>, BoxError> {
    let row = executor
        .get(
            &format!(
                "SELECT record FROM {} WHERE id = ?",
                schema.tables.documents
            ),
            &[int(id.get())],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let record: DocumentRecord = parse_record(&row)?;
    if !matches!(at, DocumentPoint::Current) && is_current_only(&record) {
        return Err(request_error(format!(
            "Document {id} does not retain historical content"
        )));
    }
    if !is_alive_at(&record, at) {
        return Ok(None);
    }
    let upper = match at {
        DocumentPoint::Current => MAX_SAFE_INTEGER_I64,
        DocumentPoint::At(seq) => seq.get(),
    };
    let base = executor
        .get(
            &format!(
                "SELECT seq, kind, version, content FROM {} \
                 WHERE document_id = ? AND kind = 'base' AND seq <= ? ORDER BY seq DESC LIMIT 1",
                schema.tables.document_revisions
            ),
            &[int(id.get()), int(upper)],
        )
        .await?;
    let Some(base) = base else {
        return Err(error(format!("Document {id} is missing a required base")));
    };
    let base_seq = base
        .get_i64("seq")
        .ok_or_else(|| error("seq column is missing"))?;
    let base_version = base
        .get_i64("version")
        .ok_or_else(|| error("version column is missing"))?;
    let Some(base_content) = base.get_text("content") else {
        return Err(error("content column is missing"));
    };
    let mut value: Value = parse_json(base_content)?;
    let tail = executor
        .all(
            &format!(
                "SELECT seq, kind, version, content FROM {} \
                 WHERE document_id = ? AND seq > ? AND seq <= ? ORDER BY seq",
                schema.tables.document_revisions
            ),
            &[int(id.get()), int(base_seq), int(upper)],
        )
        .await?;
    for revision in &tail {
        let kind = revision.get_text("kind").unwrap_or("");
        let version = revision.get_i64("version");
        if kind != "delta" || version != Some(base_version) {
            return Err(error(format!(
                "Document {id} crosses a stored version boundary without a base"
            )));
        }
        let Some(content) = revision.get_text("content") else {
            return Err(error("content column is missing"));
        };
        // TS: `apply(value, parseJson<readonly Op[]>(revision.content))`。JSON.parse 之后
        // `for...of ops` 与 `assertValidOp` 的真实语义决定错误（见 `apply_revision_ops`），
        // 不能用 wire 解码：wire 允许短形式与 `#` 引用，而这里存的是已解码的 `Op[]`。
        value = apply_revision_ops(value, content)?;
    }
    let Some(value) = value_into_json_object(value) else {
        // TS 在这里只做 `as JsonObject` 断言；Rust 的 `JsonObject` 无法表示根被替换成
        // 非对象的值。文档定义恒为对象，因此这是类型违约，显式报错而不是静默丢弃。
        return Err(error(format!(
            "Document {id} materialized a non-object value"
        )));
    };
    Ok(Some(StoredDocument {
        record,
        version: base_version,
        value,
        deltas_since_base: tail.len() as u64,
    }))
}

/// 从一行 `record, commit_seq` 构造 `StoredEntry`。
///
/// 条目的 JSON 里没有提交序号，必须从同名的 SQL 列补回；列缺失是内建错误（表由本 crate
/// 的迁移创建，出现缺失说明库被外部改坏）。
fn stored_entry(row: Option<&SqliteRow>) -> Result<Option<StoredEntry>, BoxError> {
    let Some(row) = row else {
        return Ok(None);
    };
    let entry: EntryRecord = parse_record(row)?;
    let Some(commit_seq) = row.get_i64("commit_seq") else {
        return Err(error("commit_seq column is missing"));
    };
    Ok(Some(StoredEntry {
        entry,
        commit_seq: seq_from_number(commit_seq),
    }))
}

/// TS `commit` 事务里的全部工作。
///
/// 顺序固定：读元数据 → 全局 ID 检查 → 文档动作检查 → 记录写入 → 文档写入 → 写回元数据。
/// `next_seq` 在事务内读取并原样作为本批所有记录的 `commit_seq`，并发提交因此在数据库上
/// 串行化、各自拿到不同序号；`next_id` 用 `max(库里的值, 事务外的候选值)` 推进，候选值
/// 已经包含内存 `nextId` 与本批的显式 ID。
///
/// `next_seq` 不是整数时也报 `Durable SQLite metadata is missing`（与元数据行缺失同一条
/// 消息），因为迁移保证该列是 `INTEGER NOT NULL`，走到这里说明库已损坏。
async fn commit_transaction(
    executor: &dyn SqliteExecutor,
    schema: &SqliteSchema,
    writes: &[StorageWrite],
    document_actions: &indexmap::IndexMap<DocumentId, DocumentAction>,
    candidate_next_id: f64,
) -> Result<Seq, BoxError> {
    let metadata = executor
        .get(
            &format!(
                "SELECT next_id, next_seq FROM {} WHERE singleton = 1",
                schema.tables.durable_metadata
            ),
            &[],
        )
        .await?;
    let Some(metadata) = metadata else {
        return Err(metadata_missing());
    };
    let Some(next_seq) = metadata.get_i64("next_seq") else {
        return Err(metadata_missing());
    };
    let committed_seq = seq_from_number(next_seq);
    let current_next_id = metadata
        .get_text("next_id")
        .map(kiana_json::js_num::parse_js_number)
        .unwrap_or(f64::NAN);

    check_global_ids(executor, schema, writes).await?;
    check_document_actions(executor, schema, document_actions).await?;
    for write in writes {
        apply_table_write(executor, schema, write, committed_seq).await?;
    }
    apply_document_actions(executor, schema, document_actions, committed_seq).await?;
    executor
        .run(
            &format!(
                "UPDATE {} SET next_id = ?, next_seq = ? WHERE singleton = 1",
                schema.tables.durable_metadata
            ),
            &[
                text(js_number_to_string(js_max(
                    current_next_id,
                    candidate_next_id,
                ))),
                int(committed_seq.get() + 1),
            ],
        )
        .await?;
    Ok(committed_seq)
}

#[async_trait]
impl Storage for SqliteStorage {
    /// TS `commit(writes, adopt, _context)`。
    ///
    /// 批内文档动作聚合、重复命令检查与 `nextId` 候选值在事务外完成；其余校验与表写入
    /// 都在 `commit_transaction` 内。`COMMIT` 返回后才刷新内存 `nextId` 并同步调用
    /// `adopt`，因此 `adopt` 看到的一定是已持久化的序号。
    ///
    /// 失败时原始 `BoxError` 经错误槽还原（见模块文档）；若回滚本身也失败，返回驱动的
    /// 聚合错误，因为此时数据库状态未知。
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        _context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        self.assert_open()?;
        // 事务外准备：文档动作按 ID 聚合；候选 nextId 取内存值与写入 ID 的较大者。
        let document_actions = prepare_document_actions(writes)?;
        let candidate_next_id = self.candidate_next_id(writes);
        let schema = Arc::clone(&self.schema);
        // 写入切片在进入事务前克隆：闭包按值捕获，事务内的检查与写入都基于这份快照。
        let writes = writes.to_vec();
        // 错误槽：`SqliteError` 只保存消息、类别与嵌套错误，装不下原始 `BoxError` 类型；
        // 回调先把原始错误存进槽再返回消息错误；
        // 事务失败后取回原始类型（`StorageRequestError` 等仍可 downcast），只有回滚也失败
        // 时才保留聚合错误。
        let slot: ErrorSlot = Arc::new(Mutex::new(None));
        let transaction_slot = Arc::clone(&slot);
        let result = self
            .db
            .transaction_typed(move |transaction| {
                let schema = Arc::clone(&schema);
                let slot = Arc::clone(&transaction_slot);
                Box::pin(async move {
                    match commit_transaction(
                        transaction.as_ref(),
                        &schema,
                        &writes,
                        &document_actions,
                        candidate_next_id,
                    )
                    .await
                    {
                        Ok(seq) => Ok(seq.get()),
                        Err(failure) => Err(fail_transaction(&slot, failure)),
                    }
                })
            })
            .await;
        let committed_seq = match result {
            Ok(seq) => seq,
            Err(failure) => return Err(unwrap_transaction_error(&slot, failure)),
        };
        let seq = seq_from_number(committed_seq);
        {
            let mut next_id = self.next_id.lock().expect("sqlite next id");
            *next_id = js_max(*next_id, candidate_next_id);
        }
        // 与 TS 一样先刷新内存缓存再 adopt：adopt 回调读到的是本批推进后的 nextId。
        // `COMMIT` 已返回：这一批已经持久。
        adopt(seq);
        Ok(seq)
    }

    /// TS `mintId`：只从内存计数器分配，不访问数据库，也不区分记录类型。
    ///
    /// 分配到的值会在下一次成功提交时随候选值写进 `durable_metadata.next_id`；在提交前
    /// 崩溃或关闭会丢掉这次递增（重开从库里的值继续），但该 ID 也从未写入任何记录。
    /// `nextId` 不是安全整数时（持久化文本损坏为 `NaN`，或已超过 2^53-1）报
    /// `ID space is exhausted`。
    async fn mint_id(&self, _kind: IdKind) -> Result<i64, BoxError> {
        self.assert_open()?;
        let mut next_id = self.next_id.lock().expect("sqlite next id");
        if !is_safe_integer(*next_id) {
            return Err(error("ID space is exhausted"));
        }
        let value = *next_id;
        *next_id = value + 1.0;
        Ok(value as i64)
    }

    async fn conversation(
        &self,
        id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.assert_open()?;
        self.read_conversation(id).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.assert_open()?;
        let start = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        let scan = scan_sql(start);
        let mut clauses = vec![scan.clause.to_owned()];
        let mut params = vec![int(scan.param)];
        if let Some(owner) = query.owner_conversation_id {
            clauses.push("owner_conversation_id = ?".to_owned());
            params.push(int(owner.get()));
        }
        if let Some(owner) = query.owner_task_id {
            clauses.push("owner_task_id = ?".to_owned());
            params.push(int(owner.get()));
        }
        params.push(int(fetch_limit(limit, 0)));
        let rows = self
            .db
            .all(
                &format!(
                    "SELECT record FROM {} WHERE {} ORDER BY id {} LIMIT ?",
                    self.schema.tables.conversations,
                    clauses.join(" AND "),
                    scan.direction
                ),
                &params,
            )
            .await?;
        page(
            parse_records::<ConversationRecord>(&rows)?,
            limit,
            start.order,
        )
    }

    async fn entry(
        &self,
        id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.admit_read(self.read_entry_global(id)).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.admit_read(self.read_visible_entry(conversation_id, id))
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        _context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.admit_read(self.read_latest_head_marker(conversation_id, at_or_before_entry_id))
            .await
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        self.admit_read(self.read_entries(query, limit, cursor))
            .await
    }

    async fn task(
        &self,
        id: TaskId,
        _context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.assert_open()?;
        let row = self
            .db
            .get(
                &format!(
                    "SELECT record FROM {} WHERE id = ?",
                    self.schema.tables.tasks
                ),
                &[int(id.get())],
            )
            .await?;
        match row {
            Some(row) => Ok(Some(parse_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.assert_open()?;
        let start = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        let scan = scan_sql(start);
        let mut clauses = vec![scan.clause.to_owned()];
        let mut params = vec![int(scan.param)];
        if let Some(conversation_id) = query.conversation_id {
            clauses.push("conversation_id = ?".to_owned());
            params.push(int(conversation_id.get()));
        }
        if let Some(kind) = &query.kind {
            clauses.push("kind = ?".to_owned());
            params.push(text(encode_indexed_string(kind)));
        }
        if let Some(status) = query.status {
            clauses.push("status = ?".to_owned());
            params.push(text(task_status_str(status)));
        }
        if let Some(abort_requested) = query.abort_requested {
            clauses.push("abort_requested = ?".to_owned());
            params.push(int(if abort_requested { 1 } else { 0 }));
        }
        if let Some(background) = query.background {
            clauses.push("background = ?".to_owned());
            params.push(int(if background { 1 } else { 0 }));
        }
        params.push(int(fetch_limit(limit, 0)));
        let rows = self
            .db
            .all(
                &format!(
                    "SELECT record FROM {} WHERE {} ORDER BY id {} LIMIT ?",
                    self.schema.tables.tasks,
                    clauses.join(" AND "),
                    scan.direction
                ),
                &params,
            )
            .await?;
        page(parse_records::<TaskRecord>(&rows)?, limit, start.order)
    }

    async fn submission(
        &self,
        id: SubmissionId,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.assert_open()?;
        let row = self
            .db
            .get(
                &format!(
                    "SELECT record FROM {} WHERE id = ?",
                    self.schema.tables.submissions
                ),
                &[int(id.get())],
            )
            .await?;
        match row {
            Some(row) => Ok(Some(parse_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.assert_open()?;
        let start = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        let scan = scan_sql(start);
        let mut clauses = vec![scan.clause.to_owned()];
        let mut params = vec![int(scan.param)];
        if let Some(conversation_id) = query.conversation_id {
            clauses.push("conversation_id = ?".to_owned());
            params.push(int(conversation_id.get()));
        }
        if let Some(status) = query.status {
            clauses.push("status = ?".to_owned());
            params.push(text(submission_status_str(status)));
        }
        params.push(int(fetch_limit(limit, 0)));
        let rows = self
            .db
            .all(
                &format!(
                    "SELECT record FROM {} WHERE {} ORDER BY id {} LIMIT ?",
                    self.schema.tables.submissions,
                    clauses.join(" AND "),
                    scan.direction
                ),
                &params,
            )
            .await?;
        page(
            parse_records::<SubmissionRecord>(&rows)?,
            limit,
            start.order,
        )
    }

    /// `request_id` 与入库时一样做 `JSON.stringify` 编码后比较；索引
    /// `submissions_by_request` 不是 UNIQUE，本函数也只取驱动返回的第一行。
    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.assert_open()?;
        let row = self
            .db
            .get(
                &format!(
                    "SELECT record FROM {} WHERE conversation_id = ? AND request_id = ?",
                    self.schema.tables.submissions
                ),
                &[
                    int(conversation_id.get()),
                    text(encode_indexed_string(request_id)),
                ],
            )
            .await?;
        match row {
            Some(row) => Ok(Some(parse_record(&row)?)),
            None => Ok(None),
        }
    }

    /// 精确地址查找：`Current` 取 `retired_at IS NULL` 中 `created_at` 最新的一行；
    /// `At(seq)` 取 `created_at <= seq AND (retired_at IS NULL OR retired_at > seq)` 的
    /// 最新一行（半开区间，退休边界点已不可见）。地址五列都用 `address_parts` 的编码，
    /// 不存在的地址返回 `None`。
    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.assert_open()?;
        let parts = address_parts(&address.kind, &address.scope, address.key.as_ref());
        let mut params = vec![
            text(parts.kind),
            text(parts.scope_kind),
            int(parts.owner_id),
            int(parts.family),
            text(parts.key_value),
        ];
        let sql = match at {
            DocumentPoint::Current => format!(
                "SELECT record FROM {}\n\
                 \t\t\t\t\tWHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?\n\
                 \t\t\t\t\tAND retired_at IS NULL ORDER BY created_at DESC LIMIT 1",
                self.schema.tables.documents
            ),
            DocumentPoint::At(seq) => {
                params.push(int(seq.get()));
                params.push(int(seq.get()));
                format!(
                    "SELECT record FROM {}\n\
                     \t\t\t\t\tWHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?\n\
                     \t\t\t\t\tAND created_at <= ? AND (retired_at IS NULL OR retired_at > ?)\n\
                     \t\t\t\t\tORDER BY created_at DESC LIMIT 1",
                    self.schema.tables.documents
                )
            }
        };
        let row = self.db.get(&sql, &params).await?;
        match row {
            Some(row) => Ok(Some(parse_record(&row)?)),
            None => Ok(None),
        }
    }

    /// 在一个事务里物化记录与 revision（本函数不写表），保证两者观察到同一个已提交状态；
    /// 否则两次查询之间的提交可能替换 base，回放会读到不一致的版本。物化错误经错误槽还原
    /// 为原始 `BoxError`，事务失败后存储仍可继续使用。
    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.assert_open()?;
        // 记录与修订查询必须观察同一已提交状态；它们之间的提交可能替换 base。
        let schema = Arc::clone(&self.schema);
        let slot: ErrorSlot = Arc::new(Mutex::new(None));
        let transaction_slot = Arc::clone(&slot);
        let result = self
            .db
            .transaction_typed(move |transaction| {
                let schema = Arc::clone(&schema);
                let slot = Arc::clone(&transaction_slot);
                Box::pin(async move {
                    match materialize_document(transaction.as_ref(), &schema, id, at).await {
                        Ok(stored) => Ok(stored),
                        Err(failure) => Err(fail_transaction(&slot, failure)),
                    }
                })
            })
            .await;
        match result {
            Ok(stored) => Ok(stored),
            Err(failure) => Err(unwrap_transaction_error(&slot, failure)),
        }
    }

    /// 固定按 `id` 升序扫描，不接受顺序参数；游标只读 `after`（`cursor_id` 校验它必须是
    /// 安全整数，否则报 `StorageRequestError("Invalid storage cursor")`），游标里即使带
    /// `order` 也不会被读取——`DocumentQuery` 本身没有顺序字段。
    ///
    /// `DocumentPoint::At(seq)` 用半开区间 `[created_at, retired_at)` 过滤；`kind` 过滤值
    /// 与入库时一样是 JSON 编码后的字符串。
    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.assert_open()?;
        let (scope_kind, owner_id) = scope_columns(&query.scope);
        let mut clauses = vec![
            "scope_kind = ?".to_owned(),
            "owner_id = ?".to_owned(),
            "id > ?".to_owned(),
        ];
        let mut params = vec![
            text(scope_kind),
            int(owner_id),
            int(cursor_id(cursor)?.unwrap_or(-1)),
        ];
        if let Some(kind) = &query.kind {
            clauses.push("kind = ?".to_owned());
            params.push(text(encode_indexed_string(kind)));
        }
        match query.at {
            DocumentPoint::Current => clauses.push("retired_at IS NULL".to_owned()),
            DocumentPoint::At(seq) => {
                clauses.push("created_at <= ?".to_owned());
                clauses.push("(retired_at IS NULL OR retired_at > ?)".to_owned());
                params.push(int(seq.get()));
                params.push(int(seq.get()));
            }
        }
        params.push(int(fetch_limit(limit, 0)));
        let rows = self
            .db
            .all(
                &format!(
                    "SELECT record FROM {} WHERE {} ORDER BY id LIMIT ?",
                    self.schema.tables.documents,
                    clauses.join(" AND ")
                ),
                &params,
            )
            .await?;
        page(
            parse_records::<DocumentRecord>(&rows)?,
            limit,
            ScanOrder::Ascending,
        )
    }

    async fn close(&self, context: &dyn Context) {
        // 冻结的 `Storage::close` 返回 `()`（ARCHITECTURE §4.9），没有 rejection 的位置；
        // 驱动错误缓存在 `closing` 里，可通过 [`SqliteStorage::close_result`] 观察。
        let _ = self.close_result(context).await;
    }
}

impl SqliteStorage {
    /// TS `close(context)`：幂等；首次调用把 `closeDatabase()` 的结果缓存进 `closing`，
    /// 初始化正常完成时后续调用返回同一结果。驱动关闭失败时首个调用即失败，之后的调用
    /// 同样失败（TS 缓存的是同一个已 rejection 的 promise）。
    ///
    /// 这是 [`Storage::close`] 之外的可失败入口：冻结的 trait 方法没有返回错误的位置。
    ///
    /// `closed` 在等待之前置位：关闭期间新到的读立即失败，已经通过 `admit_read` 的读继续
    /// 完成。并发调用共享同一个 `OnceCell`：初始化正常完成时只有一个任务真正执行
    /// `close_database`，后续调用拿到缓存的 `Result`（错误通过 `SqliteError: Clone` 复制）；
    /// 若初始化 future 被取消（例如调用方丢弃 `close_result` 的 future），tokio 会放弃这次
    /// 初始化，下一次调用会重新执行 `close_database`。
    pub async fn close_result(&self, _context: &dyn Context) -> Result<(), BoxError> {
        self.closed.store(true, Ordering::SeqCst);
        match self
            .closing
            .get_or_init(|| async { self.close_database().await })
            .await
        {
            Ok(()) => Ok(()),
            Err(failure) => Err(Box::new(failure.clone())),
        }
    }

    /// TS `closeDatabase`：等已接纳的读排空，再关闭数据库。
    ///
    /// 先 `subscribe` 拿到当前计数，再在循环里用 `borrow_and_update` 检查；订阅之后发生的
    /// 递减不会被漏掉。发送端由 `SqliteStorage` 自己持有，正常路径上 `changed()` 不会返回
    /// `Err`，那个分支只是防御性退出。
    async fn close_database(&self) -> Result<(), SqliteError> {
        let mut admitted = self.admitted.subscribe();
        loop {
            if *admitted.borrow_and_update() == 0 {
                break;
            }
            if admitted.changed().await.is_err() {
                break;
            }
        }
        self.db.close().await
    }
}
