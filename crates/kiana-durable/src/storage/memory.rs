//! `storage/memory.ts`：`MemoryStorage`——`Storage` 的参考语义实现。
//!
//! 它是**脱离式**（detached）的内存后端：`prepare_commit` 克隆保留的写入值，所有读方法克隆
//! 返回的记录。这是后端一致性（模拟 SQLite 编解码 / JSONL 序列化的所有权边界），不是防御性
//! 校验。TS 用 `Object.freeze` 冻结 `PreparedMemoryCommit.writes`；Rust 由所有权承担同一契约：
//! [`PreparedMemoryCommit::writes`] 只给出 `&[StorageWrite]`，状态里保存的是独立拷贝。
//!
//! 状态用 `Mutex` 保护（[`Storage`] 的方法接收 `&self`）。锁只在单次方法调用内持有，绝不在
//! `await` 或 `adopt` 回调期间持有——`adopt` 里同步读取存储（TS conformance 用例）不会死锁。
//!
//! # 职责与边界
//!
//! 本模块只做两件事：把一次已经校验过的提交原子地变成可查询的内存状态，以及从这些索引里
//! 读出记录的快照。它不解释业务语义：任务状态只是记录里的字段，文档内容只是按修订顺序
//! 回放的 base/delta。错误文案、序号检查与地址键格式严格对齐 TS 的 `storage/memory.ts`，
//! 因此其它后端的 conformance 用例可以拿它当语义基准（`storage/mod.rs` 的模块文档也这样
//! 定位它）。
//!
//! 直接调用方：
//!
//! - `storage/jsonl/storage.rs`：把本类型当作内存镜像。写入走 [`MemoryStorage::prepare_commit`]，
//!   用 [`PreparedMemoryCommit::writes`] 编码 JSONL 并落盘后再 [`PreparedMemoryCommit::apply`]；
//!   所有读取、`mint_id` 与 `close` 直接转发到同一个实例。
//! - `tests/memory_storage.rs` 逐条覆盖共享 conformance 用例（TS 的
//!   `memory-storage.test.ts`）；`testing/storage_conformance.rs` 是同一套用例的注册器，
//!   JSONL/SQLite 测试用它跑各自后端。
//! - `index.rs` 把 [`MemoryStorage`] 作为公开入口再导出。
//!
//! # 数据结构与不变量
//!
//! 全部可变状态在 `Inner` 里：`State` 保存各表记录与索引（`HashMap` 主表 + 按 ID 升序的
//! `Vec` 索引 + 文档地址/作用域索引），`Inner` 另外持有 `next_id`/`next_seq`/`closed`。
//! 所有 `*_ids` 向量都按 ID 升序（由 `insert_sorted`/`remove_sorted` 维护），
//! 索引里的每个 ID 都必须能反查到记录——索引查询处的 `expect`（如 `entryIds`/`headEntryIds`/
//! `taskIds`/`submissionIds`/`documentAddresses`/`documentIdsByScope` 指向主表）以这条不变量
//! 为前提；其它 `expect` 依赖各自的局部前提（如 `upper_entry_id.expect("set above")` 依赖紧邻
//! 的赋值、`action.content.expect(...)` 依赖聚合函数保证 create 必带 base）。记录一旦
//! 写入就不再借用调用方的值（写入前克隆），读取一律返回克隆，所以提交后的值对外不可变。
//!
//! `record_types` 是跨表共享的数字命名空间：会话、条目、文档创建要求 ID 全局唯一；任务与
//! 提交按 ID 替换，同表重复写入是合法更新（细节见 `check_global_ids` 的注释）。
//!
//! # 提交流程
//!
//! 1. [`MemoryStorage::prepare_commit`]（锁内，只读）：关闭检查 → 序号检查 → 展开
//!    `document.copy`（物化源文档）→ 全局 ID 检查 → 文档动作聚合 → 文档不变量检查。
//! 2. [`PreparedMemoryCommit::apply`]（再次取锁）：按写入顺序更新表与索引，再按文档动作的
//!    首次出现顺序写入文档修订与地址索引，最后把 `next_seq` 推进到 `seq + 1`。
//! 3. [`Storage::commit`] 的实现是 `prepare_commit(writes, None)?.apply()?`，成功后才在锁外
//!    调用 `adopt(seq)`；失败时 `adopt` 不会被调用。
//!
//! 校验全部集中在 `prepare_commit`；`apply` 不做任何可失败的准备工作，唯一可能返回错误的
//! 路径是遇到“未解析的 `document.copy`”（`prepare_commit` 本应消除它）。
//!
//! # 读取与分页
//!
//! 读取方法都在单次调用内取锁、查询、克隆后返回，调用方拿到的记录与后续提交互不影响。
//! 扫描分两套：会话/条目/任务/提交统一用 `storage/scan.rs` 的 `scan_start` 解释游标（给出
//! 游标时 `after` 必须是安全整数，游标里的 `order` 决定扫描方向）；`scan_documents` 例外，
//! 使用本文件的 `cursor_id`（`after` 缺省表示从头，存在时必须是安全整数；忽略 `order`，
//! 固定升序）。每页多取一条仅用于判断是否还有下一页。条目扫描是 fork-aware 的：沿
//! [`ConversationRecord::parent`] 链合并祖先条目，并用游标收紧链上各段的 ID 边界。
//!
//! # 并发与失败模式
//!
//! - 状态由 `Mutex` 串行化；`PreparedMemoryCommit::apply` 在锁内检查 `applied`，并发重复
//!   apply 只生效一次。
//! - 本类型不提供跨 `prepare`/`apply` 的提交排序：`prepare_commit` 只读取 `next_seq`，不预留
//!   序号，`apply` 直接把 `next_seq` 设为 `seq + 1`。TS 的 `commit` 在 `prepareCommit` 与
//!   `apply` 之间没有 `await`；Rust 侧若允许多个任务并发提交同一实例，调用方需要自行
//!   串行化，否则默认序号可能重复，`next_seq` 也可能因乱序 apply 而回退。
//! - 互斥锁中毒时 `lock_inner` 直接取回内部值继续使用：持锁期间 panic 之后，后续调用看到的
//!   `State` 可能停在半更新状态，本类型不会因此永久拒绝服务。
//! - `close` 只置位 `closed`；之后除 `close` 外的公开入口（含 `prepare_commit`）返回
//!   `MemoryStorage is closed`，已经准备好的 [`PreparedMemoryCommit`] 仍可 `apply`（`apply`
//!   不检查 `closed`）。
//! - 请求类错误（未知会话、非法游标、对 current-only 文档做历史读取）用
//!   [`StorageRequestError`]，只让本次读取失败；其余错误（`MemoryStorageError`，以及复制
//!   失败时的 [`DocumentCopyRejected`]）按 [`Storage`] 契约会让 Session 失败。
//!
//! # 修改注意
//!
//! - 读取返回值与保留的写入值都必须保持克隆（脱离式语义）；把引用或共享结构暴露出去会破坏
//!   与持久化后端一致的“提交后不可变”边界。
//! - 新增 [`StorageWrite`] 变体时，`resolve_document_copies`、`check_global_ids`、
//!   `prepare_document_actions`、`apply_prepared_commit` 都与它相关。其中
//!   `resolve_document_copies` 与 `prepare_document_actions` 带通配分支，新变体不会编译
//!   报错但会被静默忽略——必须手动补上，并让错误文案与 TS 对齐。
//! - 地址键经 `kiana_json::js_object::stringify` 生成，必须与 `JSON.stringify` 逐字节一致
//!   （本文件的单元测试锁定）；改动键的形状会破坏与持久化后端/TS 的兼容性。
//!
//! # 与 TS 的偏差
//!
//! [`PreparedMemoryCommit::apply`] 在 TS 里直接返回 `Seq`，Rust 版返回 `Result<Seq, BoxError>`。
//! 唯一的错误分支是“未解析的 document.copy”：`prepare_commit` 会展开本批所有 copy，因此该
//! 分支只可能由内部不变量被破坏触发。成功路径的返回值与 TS 相同。
//!
//! # 阅读顺序
//!
//! 先看 `State`/`Inner` 的字段，再读 [`MemoryStorage::prepare_commit`] 与
//! [`PreparedMemoryCommit::apply`] 两个入口，随后看 `apply_prepared_commit` 和
//! `apply_document_actions`；键函数与分页辅助集中在文件前半部分，按需查阅。

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;
use kiana_chord::context::Context;
use serde_json::Value;

use crate::storage::scan::{ScanStart, next_cursor, scan_start};
use crate::types::{
    AdoptFn, BoxError, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentBaseContent, DocumentContent, DocumentContentKind, DocumentCreate,
    DocumentHistory, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope,
    EntryId, EntryQuery, EntryRecord, Id, IdBrand, IdKind, Page, ScanOrder, ScopeKind, Seq,
    Storage, StorageRequestError, StorageWrite, StoredDocument, StoredEntry, SubmissionId,
    SubmissionQuery, SubmissionRecord, SubmissionStatus, TaskId, TaskQuery, TaskRecord, TaskStatus,
};

/// TS `TableName`。
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

/// TS 里 `new Error(message)` 的通用载体：`MemoryStorage` 的提交校验、关闭与内部不变量错误。
///
/// 与 [`StorageRequestError`] 的分工沿用 TS：调用方请求非法（未知会话、坏游标、对
/// current-only 文档做历史读取）抛请求错误，只让那次读取失败。本类型既承载提交校验失败
/// （`Commit sequence ... does not strictly increase`、`ID ... already belongs to ...`、
/// `Document ... already exists` / `Document ... is retired`、
/// `Document ... has more than one content command`、copy 的 fork 原因（作为
/// `DocumentCopyRejected` 的 `cause`）、`ID space is exhausted` 等），也承载存储关闭与
/// 内部不变量错误；除读取路径的 [`StorageRequestError`] 外，按 [`Storage`] 的契约都属于
/// 致命错误，会让 Session 失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct MemoryStorageError {
    /// 完整消息。
    pub message: String,
}

impl MemoryStorageError {
    /// `new Error(message)`。
    pub fn new(message: impl Into<String>) -> Self {
        MemoryStorageError {
            message: message.into(),
        }
    }
}

/// `Document copy ${id} was rejected`（TS 的 `new Error(message, { cause })`）。
///
/// `Display` 只输出被拒绝的复制目标 ID；真正的原因保存在 `#[source]` 的 `cause` 里，调用方
/// 可以沿 `source()` 链读取，也可以 `downcast_ref::<DocumentCopyRejected>()` 后直接读公开的
/// `cause` 字段（`error_message` 只做 `to_string`，不会包含 cause）。
#[derive(Debug, thiserror::Error)]
#[error("Document copy {id} was rejected")]
pub struct DocumentCopyRejected {
    /// 被拒绝的复制目标。
    pub id: DocumentId,
    /// 原始失败。
    #[source]
    pub cause: BoxError,
}

fn error(message: impl Into<String>) -> BoxError {
    Box::new(MemoryStorageError::new(message))
}

fn request_error(message: impl Into<String>) -> BoxError {
    Box::new(StorageRequestError::new(message))
}

/// JS 的 `Number.MAX_SAFE_INTEGER`（2^53-1）。
///
/// `prepareCommit` 用 `Number.isSafeInteger(seq)` 校验序号；`Seq` 是 `i64`，非整数 / `NaN` /
/// `Infinity` 都不可表示，负数会被 `seq < nextSeq` 覆盖，只有上界需要显式比较。
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// 一条已提交的内容修订（TS `DocumentRevision = DocumentContent & { seq }`）。
///
/// `seq` 是写入这条修订的提交序号，不是文档的创建序号；按时间点读取时用
/// `revision.seq <= at` 过滤（见 `materialize_document`），所以同一文档的修订必须按 seq
/// 升序追加。
#[derive(Debug, Clone, PartialEq)]
struct DocumentRevision {
    content: DocumentContent,
    seq: Seq,
}

/// 一个文档化身的记录与全部已提交修订（TS `StoredDocumentState`）。
///
/// `revisions[0]` 通常是 base；对 current-only 文档（非 conversation 作用域或
/// `history === "latest"`），新 base 会替换整个列表，退役时列表被清空（见
/// `apply_document_actions`）。因此“列表里一定还有可物化的 base”不是本结构的类型不变量：
/// 提交路径（create 必带 base、delta 必须有前一条修订）共同维持它，`materialize_document`
/// 仍会显式检查并在不满足时报 `missing a required base`。
#[derive(Debug, Clone, PartialEq)]
struct StoredDocumentState {
    record: DocumentRecord,
    revisions: Vec<DocumentRevision>,
}

/// `addressKey` → 该逻辑地址上的化身（升序）与当前化身（TS `DocumentAddressIndex`）。
///
/// `ids` 含该地址下所有化身（含已退役的），`current_id` 只在存在存活化身时是 `Some`，且
/// 必须是 `ids` 之一。两个字段都由 `apply_document_actions` 维护：create 把新 ID 插入 `ids`
/// 并设为 current（除非同一提交里立即退役）；retire 的若是 current 化身则清除 current。
/// 按时间点查找走 `ids`
/// 顺序扫描（`find_document`），所以顺序必须是升序。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DocumentAddressIndex {
    ids: Vec<DocumentId>,
    current_id: Option<DocumentId>,
}

/// 一个文档在一个提交里的聚合动作（TS `DocumentAction`）。
///
/// `prepare_document_actions` 把同一 ID 的多条文档写入合并到这里：`content` 至多一条
/// （create 的初始 base 或一次 change），`retire` 至多一次；create 与 retire 可以同时出现，
/// 表示“创建后在同一提交内立刻退役”。`create` 与 `content` 的关系由聚合函数保证：create
/// 一定伴随它的 base，change 不会与 create 同时出现。
#[derive(Debug, Clone, Default, PartialEq)]
struct DocumentAction {
    create: Option<DocumentCreate>,
    content: Option<DocumentContent>,
    retire: bool,
}

/// `Record<TaskStatus, TaskId[]>`（TS 用对象字面量，键顺序固定）。
///
/// 这是 `scan_tasks(status: Some(_))` 的二级索引：每个桶按 ID 升序持有该状态的所有任务。
/// `apply_prepared_commit` 在任务状态变化时从旧桶 `remove_sorted`、向新桶 `insert_sorted`；
/// 同状态重复写入不移动位置。桶里的 ID 与 `State::tasks` 必须同步，否则扫描会取到
/// 不存在的任务（代码用 `expect` 断言这一点）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TaskStatusIndex {
    pending: Vec<TaskId>,
    running: Vec<TaskId>,
    waiting: Vec<TaskId>,
    sleeping: Vec<TaskId>,
    completing: Vec<TaskId>,
    terminal: Vec<TaskId>,
}

impl TaskStatusIndex {
    fn get(&self, status: TaskStatus) -> &Vec<TaskId> {
        match status {
            TaskStatus::Pending => &self.pending,
            TaskStatus::Running => &self.running,
            TaskStatus::Waiting => &self.waiting,
            TaskStatus::Sleeping => &self.sleeping,
            TaskStatus::Completing => &self.completing,
            TaskStatus::Terminal => &self.terminal,
        }
    }

    fn get_mut(&mut self, status: TaskStatus) -> &mut Vec<TaskId> {
        match status {
            TaskStatus::Pending => &mut self.pending,
            TaskStatus::Running => &mut self.running,
            TaskStatus::Waiting => &mut self.waiting,
            TaskStatus::Sleeping => &mut self.sleeping,
            TaskStatus::Completing => &mut self.completing,
            TaskStatus::Terminal => &mut self.terminal,
        }
    }
}

/// `Record<SubmissionStatus, SubmissionId[]>`。
///
/// 与 `TaskStatusIndex` 同构：`scan_submissions(status: Some(_))` 直接遍历对应桶，状态变化时
/// 在旧桶删除、新桶插入，桶内按 ID 升序。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SubmissionStatusIndex {
    queued: Vec<SubmissionId>,
    placed: Vec<SubmissionId>,
    done: Vec<SubmissionId>,
    unanswered: Vec<SubmissionId>,
}

impl SubmissionStatusIndex {
    fn get(&self, status: SubmissionStatus) -> &Vec<SubmissionId> {
        match status {
            SubmissionStatus::Queued => &self.queued,
            SubmissionStatus::Placed => &self.placed,
            SubmissionStatus::Done => &self.done,
            SubmissionStatus::Unanswered => &self.unanswered,
        }
    }

    fn get_mut(&mut self, status: SubmissionStatus) -> &mut Vec<SubmissionId> {
        match status {
            SubmissionStatus::Queued => &mut self.queued,
            SubmissionStatus::Placed => &mut self.placed,
            SubmissionStatus::Done => &mut self.done,
            SubmissionStatus::Unanswered => &mut self.unanswered,
        }
    }
}

/// TS `State`。`Map` 的顺序语义只在 `document_addresses` 的遍历里出现过（无副作用），
/// 其余索引按 ID 有序的 `Vec` 保存，使遍历顺序确定。
///
/// 读取方可以依赖的不变量（全部由提交路径维护）：
///
/// - 所有 `*_ids` 向量按 ID 升序且不含重复；`entry_ids` / `head_entry_ids` 按会话分组，
///   后者只含 `head.is_some()` 的条目。
/// - `record_types`、各表 `HashMap` 与上述索引描述同一批记录；`entry_commit_seqs` 为每个
///   已存条目记录写入它的提交序号。
/// - `submission_ids_by_request` 只保留每个 `(conversation_id, request_id)` 的最新写入；
///   覆盖旧提交时先删除旧映射，会话的 Map 变空后整个条目被移除。
/// - `document_addresses[*].current_id` 指向的化身必未退役；当前化身被退役时该字段先清空，
///   同一批的 create 再把它指向新化身。
///
/// ID 永不复用，因此 `record_types` 只增不删。
#[derive(Debug, Default)]
struct State {
    /// 全局 ID → 表（跨表唯一性）；所有表共享一个数字命名空间。
    record_types: HashMap<i64, TableName>,
    conversations: HashMap<ConversationId, ConversationRecord>,
    /// 升序。
    conversation_ids: Vec<ConversationId>,
    conversation_ids_by_owner_conversation: HashMap<ConversationId, Vec<ConversationId>>,
    conversation_ids_by_owner_task: HashMap<TaskId, Vec<ConversationId>>,
    entries: HashMap<EntryId, EntryRecord>,
    /// 每会话升序。
    entry_ids: HashMap<ConversationId, Vec<EntryId>>,
    /// 每会话升序；只有带 `head` 的条目。
    head_entry_ids: HashMap<ConversationId, Vec<EntryId>>,
    entry_commit_seqs: HashMap<EntryId, Seq>,
    tasks: HashMap<TaskId, TaskRecord>,
    task_ids: Vec<TaskId>,
    task_ids_by_status: TaskStatusIndex,
    submissions: HashMap<SubmissionId, SubmissionRecord>,
    submission_ids: Vec<SubmissionId>,
    submission_ids_by_status: SubmissionStatusIndex,
    submission_ids_by_request: HashMap<ConversationId, HashMap<String, SubmissionId>>,
    documents: HashMap<DocumentId, StoredDocumentState>,
    /// `addressKey` → 索引。
    document_addresses: HashMap<String, DocumentAddressIndex>,
    /// `scopeKey` → 升序化身 ID。
    document_ids_by_scope: HashMap<String, Vec<DocumentId>>,
}

/// 状态与序号分配器；`Mutex` 的受保护内容。
///
/// 所有可变状态都放在这里，保证一次公开调用内的多次索引更新处于同一个临界区。
#[derive(Debug)]
struct Inner {
    /// 全部表、索引与文档状态。
    state: State,
    /// 下一个候选全局 ID。`mint_id` 返回它并自增；`apply` 会把本批写入的最大 ID + 1 纳入
    /// 下界，避免显式写入大 ID 后继续发放冲突的 ID。
    next_id: i64,
    /// 下一个默认提交序号，也是 `prepare_commit` 校验显式序号的下界；`apply` 后变成
    /// `seq + 1`。
    next_seq: i64,
    /// `close` 置位；只由 `assert_open` 检查（`PreparedMemoryCommit::apply` 不检查）。
    closed: bool,
}

impl Inner {
    fn new() -> Self {
        Inner {
            state: State::default(),
            next_id: 2,
            next_seq: 1,
            closed: false,
        }
    }
}

// ---------------------------------------------------------------------------
// 键函数（逐字节对应 TS；JSONL/SQLite/IndexedDB 依赖同类语义）
// ---------------------------------------------------------------------------

/// `scopeKey(scope)`。
///
/// 作用域被编码成 `JSON.stringify` 的数组字符串（`["session"]`、
/// `["conversation",7]`、`["task",9]`），用作 `State::document_ids_by_scope` 的键。数字必须
/// 经 `Value::from(i64)` 序列化而不是字符串拼接，否则字节表示会与 TS 不同。
fn scope_key(scope: &DocumentScope) -> String {
    let value = match scope {
        DocumentScope::Session => Value::Array(vec![Value::String("session".to_owned())]),
        DocumentScope::Conversation { conversation_id } => Value::Array(vec![
            Value::String("conversation".to_owned()),
            Value::from(conversation_id.get()),
        ]),
        DocumentScope::Task { task_id } => Value::Array(vec![
            Value::String("task".to_owned()),
            Value::from(task_id.get()),
        ]),
    };
    kiana_json::js_object::stringify(&value)
}

/// `addressKey(address)`：`["kind", "[\"scope\"]", ["singleton"] | ["family", key]]`。
///
/// 中间元素是 [`scope_key`] 返回的**字符串**（内层引号因此被转义）；这是 TS 直接
/// `JSON.stringify([kind, scopeKey(scope), ...])` 的结果。`key: None` 编码为
/// `["singleton"]`，显式 JSON `null` 则属于 `["family", null]`，两者不能混同。
fn address_key(address: &DocumentAddress) -> String {
    let tail = match &address.key {
        None => Value::Array(vec![Value::String("singleton".to_owned())]),
        Some(key) => Value::Array(vec![Value::String("family".to_owned()), key.clone()]),
    };
    let value = Value::Array(vec![
        Value::String(address.kind.clone()),
        Value::String(scope_key(&address.scope)),
        tail,
    ]);
    kiana_json::js_object::stringify(&value)
}

/// 从待创建的 `DocumentCreate` 生成地址键。
fn create_address_key(record: &DocumentCreate) -> String {
    address_key(&DocumentAddress {
        kind: record.kind.clone(),
        scope: record.scope,
        key: record.key.clone(),
    })
}

/// 从已存 `DocumentRecord` 生成地址键；必须与 [`create_address_key`] 对同一逻辑地址给出
/// 相同结果，地址索引才能被创建路径与后续 change/retire 路径共同命中。
fn record_address_key(record: &DocumentRecord) -> String {
    address_key(&DocumentAddress {
        kind: record.kind.clone(),
        scope: record.scope,
        key: record.key.clone(),
    })
}

// ---------------------------------------------------------------------------
// 存活判定
// ---------------------------------------------------------------------------

/// `JsonObject`（`IndexMap`）→ `Value::Object`（chord 的 op 应用面）。
fn json_object_value(object: crate::types::JsonObject) -> Value {
    Value::Object(object.into_iter().collect())
}

/// `Value::Object` → `JsonObject`；根不是对象时返回 `None`。
fn value_into_json_object(value: Value) -> Option<crate::types::JsonObject> {
    match value {
        Value::Object(map) => Some(map.into_iter().collect()),
        _ => None,
    }
}

/// `isAliveAt(record, at)`。
///
/// `Current` 等价于“未退役”；`At(seq)` 把存活区间看作半开区间
/// `[created_at, retired_at)`——退役的那次提交里文档已经不再存活。这个边界同时被
/// `find_document`、`scan_documents` 与 `materialize_document` 使用。
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

/// `isCurrentOnly(record)`：非 conversation 作用域，或 `history === "latest"`。
///
/// current-only 文档不保留历史：`materialize_document` 对非 `Current` 的读取直接报
/// `does not retain historical content`，`apply_document_actions` 收到新 base 时会丢掉旧修订、
/// 退役时清空修订列表。注意 `history` 为 `None` 的 conversation 文档不算 current-only。
fn is_current_only(record: &DocumentRecord) -> bool {
    record.scope.kind() != ScopeKind::Conversation
        || record.history == Some(DocumentHistory::Latest)
}

// ---------------------------------------------------------------------------
// 有序数组工具
// ---------------------------------------------------------------------------

// 下面四个函数（以及 `insert_map_id`）是所有有序 ID 索引的唯一维护方式。前提是 `ids`
// 始终按 ID 严格升序；二分查找本身是 O(log n)，`insert_sorted`/`remove_sorted` 需要
// `Vec` 位移，代价 O(n)。索引一旦失序，扫描的游标边界会静默出错。

/// 第一个 `>= target` 的下标。
fn lower_bound<K: IdBrand>(ids: &[Id<K>], target: i64) -> usize {
    let mut low = 0usize;
    let mut high = ids.len();
    while low < high {
        let middle = (low + high) / 2;
        if ids[middle].get() < target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// 第一个 `> target` 的下标。
///
/// 与 [`lower_bound`] 的唯一区别是相等元素落在边界右侧；游标续页要求“严格大于上一页最后
/// 一个 ID”，因此升序扫描用本函数，降序扫描用 `lower_bound`（见 [`scan_indexes`]）。
fn upper_bound<K: IdBrand>(ids: &[Id<K>], target: i64) -> usize {
    let mut low = 0usize;
    let mut high = ids.len();
    while low < high {
        let middle = (low + high) / 2;
        if ids[middle].get() <= target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// `insertSorted(ids, id)`。
///
/// 常见情况（新 ID 大于末尾）是一次 `push`；否则二分定位后插入。调用方必须保证不重复插入
/// 同一 ID（提交路径只对新建记录调用）。
fn insert_sorted<K: IdBrand>(ids: &mut Vec<Id<K>>, id: Id<K>) {
    if ids.last().is_none_or(|last| last.get() < id.get()) {
        ids.push(id);
    } else {
        let index = lower_bound(ids, id.get());
        ids.insert(index, id);
    }
}

/// `removeSorted(ids, id)`。
///
/// 定位后只在精确命中时删除：ID 不存在是合法的无操作，不会误删相邻元素。
fn remove_sorted<K: IdBrand>(ids: &mut Vec<Id<K>>, id: Id<K>) {
    let index = lower_bound(ids, id.get());
    if ids.get(index) == Some(&id) {
        ids.remove(index);
    }
}

/// `insertMapId(index, key, id)`。
///
/// 首次见到 `key` 时创建空向量；每个 key 下的 ID 仍按升序维护。目前只用于会话的两个
/// owner 索引（`conversation_ids_by_owner_conversation` / `..._by_owner_task`）。
fn insert_map_id<Key: Eq + Hash, K: IdBrand>(
    index: &mut HashMap<Key, Vec<Id<K>>>,
    key: Key,
    id: Id<K>,
) {
    insert_sorted(index.entry(key).or_default(), id);
}

// ---------------------------------------------------------------------------
// 分页
// ---------------------------------------------------------------------------

/// `Page` 条目里取 ID 的私有视图（TS 的 `{ readonly id: Id }` 约束）。
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

/// `page(values, limit, order)`：所有扫描都多取一个元素来判断是否还有下一页。
///
/// `next` 的 `after` 是截断后最后一项（第 `limit` 项）的 ID；调用方传入的 `values` 已经是
/// 克隆后的自有记录，这里只负责截断并生成游标。
///
/// `limit == 0` 且仍有数据时返回错误
/// `TypeError: Cannot read properties of undefined (reading 'id')`（当前稳定契约，改动需同步
/// 测试；`usize` 的 0 是合法入参，不能静默返回空页）。
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

/// `scanIndexes(ids, start)`。
///
/// 返回的是索引下标（按扫描顺序），不是 ID。升序从 `upper_bound(after)` 开始（严格大于
/// 游标），降序从 `lower_bound(after)` 的前一个开始（严格小于游标）并反向遍历；`after`
/// 为 `None` 时覆盖整个数组。调用方用这些下标再查主表。
fn scan_indexes<K: IdBrand>(ids: &[Id<K>], start: ScanStart) -> Vec<usize> {
    match start.order {
        ScanOrder::Ascending => {
            let first = start.after.map_or(0, |after| upper_bound(ids, after));
            (first..ids.len()).collect()
        }
        ScanOrder::Descending => {
            let end = start
                .after
                .map_or(ids.len(), |after| lower_bound(ids, after));
            (0..end).rev().collect()
        }
    }
}

/// `cursorId(cursor)`：`after` 必须是安全整数。
///
/// 只有 `scan_documents` 使用它（其它扫描走 `storage/scan.rs` 的 `scan_start`）。语义与 TS
/// 一致：游标存在但**缺少** `after` 键时返回 `None`（从头开始），`after` 存在但不是安全
/// 整数时返回 [`StorageRequestError`]；游标里的 `order` 字段在这里被忽略，文档扫描固定升序。
fn cursor_id(cursor: Option<&Cursor>) -> Result<Option<i64>, BoxError> {
    let Some(after) = cursor.and_then(|cursor| cursor.get("after")) else {
        return Ok(None);
    };
    crate::types::js_safe_integer(after)
        .map(Some)
        .ok_or_else(|| request_error("Invalid storage cursor"))
}

// ---------------------------------------------------------------------------
// PreparedMemoryCommit
// ---------------------------------------------------------------------------

/// 一个已完整校验、脱离的提交；[`PreparedMemoryCommit::apply`] 不执行任何可失败的准备工作。
///
/// TS: `storage/memory.ts#PreparedMemoryCommit`。`apply()` 在 TS 里不可失败；Rust 里保留
/// `Result` 以承载“未解析的 document.copy”这一不可达分支（见模块文档的偏差说明）。
///
/// 生命周期：由 [`MemoryStorage::prepare_commit`] 创建，持有与存储共享的 `Inner`，可以在
/// 另一个任务里调用 `apply`。`apply` 幂等，`seq()`/`writes()` 在 apply 前后都能读——
/// `storage/jsonl/storage.rs` 正是先取 `writes()` 落盘、再 `apply()` 更新内存。
pub struct PreparedMemoryCommit {
    seq: Seq,
    writes: Vec<StorageWrite>,
    actions: IndexMap<DocumentId, DocumentAction>,
    inner: Arc<Mutex<Inner>>,
    applied: AtomicBool,
}

impl PreparedMemoryCommit {
    /// 这次提交的序号。
    ///
    /// 等于 `prepare_commit` 的显式入参，或未给出时创建时刻的 `next_seq`；[`Self::apply`]
    /// 返回同一个值，不会重新分配。
    pub fn seq(&self) -> Seq {
        self.seq
    }

    /// 深脱离、已冻结语义的写入（TS 里是 `Object.freeze` 后的克隆）。
    ///
    /// 顺序与调用方传入的 `writes` 一致；其中所有 `document.copy` 都已在 prepare 阶段展开成
    /// `document.create`（携带物化后的 base）。持久化后端直接消费这个切片来编码磁盘格式。
    pub fn writes(&self) -> &[StorageWrite] {
        &self.writes
    }

    /// 应用这次提交；幂等（只生效一次）。
    ///
    /// 锁内检查并更新 `applied`，重复或并发调用只会真正写入一次，之后直接返回同一个
    /// [`Seq`]。`apply` **不检查** `closed`：在 `close` 之前 prepare 的提交仍可生效。
    ///
    /// # Errors
    /// 只可能在内部不变量被破坏时失败（TS 的 `Prepared document copy was not resolved`）。
    /// 该错误发生时 `applied` 仍为 `false`，但排在失败写入之前的表记录可能已经落地——正常
    /// API 路径不会产生这种提交（`prepare_commit` 已展开全部 copy）。
    pub fn apply(&self) -> Result<Seq, BoxError> {
        // 在状态锁内读写 `applied`：并发调用只会真正应用一次，且后到者看到的是已完成的状态。
        // 两个操作都在同一把 Mutex 的临界区内，互斥锁的 acquire/release 已经提供 happens-before，
        // 因此 `Relaxed` 足够；这个模式不能搬到锁外使用。
        let mut inner = lock_inner(&self.inner);
        if self.applied.load(Ordering::Relaxed) {
            return Ok(self.seq);
        }
        apply_prepared_commit(&mut inner, &self.writes, &self.actions, self.seq)?;
        self.applied.store(true, Ordering::Relaxed);
        Ok(self.seq)
    }
}

impl std::fmt::Debug for PreparedMemoryCommit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedMemoryCommit")
            .field("seq", &self.seq)
            .field("writes", &self.writes)
            .finish_non_exhaustive()
    }
}

/// 取状态锁；互斥锁中毒时直接取回内部值继续使用。
///
/// 中毒意味着上一次持锁期间发生了 panic，`State` 可能停在半更新状态；这里选择不把这一
/// 次 panic 升级为存储的永久失败（否则 `unwrap()` 会让之后每次调用都 panic）。读取方因此
/// 可能观察到部分写入，调用方不应在持锁代码里 panic。
fn lock_inner(inner: &Arc<Mutex<Inner>>) -> MutexGuard<'_, Inner> {
    inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// MemoryStorage
// ---------------------------------------------------------------------------

/// 脱离式内存参考实现（TS `class MemoryStorage implements Storage`）。
///
/// 克隆是**共享**的：每个克隆只是同一个 `Arc<Mutex<Inner>>` 的句柄，能看到彼此的提交与
/// `close`（`storage/jsonl/storage.rs` 依赖这一点，把同一个实例同时用作持久化与读取视图）。
/// 所有方法接收 `&self`，每次调用自行取锁；没有任何后台任务或定时器，最后一个句柄释放时
/// 状态随之丢弃。
#[derive(Debug, Clone)]
pub struct MemoryStorage {
    inner: Arc<Mutex<Inner>>,
}

impl Default for MemoryStorage {
    fn default() -> Self {
        MemoryStorage::new()
    }
}

impl MemoryStorage {
    /// 新建空的存储（`nextId = 2`、`nextSeq = 1`）。
    ///
    /// ID 1 保留给不可变的根会话（`ROOT_CONVERSATION_ID`；conformance 用例
    /// `reserves ID 1 for the immutable root conversation` 锁定该行为），因此候选 ID 从 2
    /// 开始发放；默认提交序号从 1 开始。`Default` 等价于调用本函数。
    pub fn new() -> Self {
        MemoryStorage {
            inner: Arc::new(Mutex::new(Inner::new())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock_inner(&self.inner)
    }

    /// 关闭检查：`prepare_commit`、`mint_id` 与全部读取入口共用；已 prepare 的提交不受影响。
    fn assert_open(inner: &Inner) -> Result<(), BoxError> {
        if inner.closed {
            return Err(error("MemoryStorage is closed"));
        }
        Ok(())
    }

    /// 校验并脱离一次提交，不改变可观察状态（TS `prepareCommit`）。
    ///
    /// `seq` 为 `None` 时默认取 `seqFromNumber(nextSeq)`。校验顺序固定，因此多种错误
    /// 同时存在时抛出的那一个也是确定的：关闭 → 序号 → copy 展开 → 全局 ID → 文档动作聚合 →
    /// 文档动作检查。所有检查只读取当前状态，不预留 ID 或序号；成功返回的提交在 apply 之前
    /// 不会影响任何读取结果。
    ///
    /// 传入的 `writes` 会被逐条克隆（`document.copy` 展开时还会重写成 `document.create`），
    /// 返回的提交与存储状态完全脱离。
    ///
    /// # Errors
    /// 关闭、序号不严格递增（含超过 `Number.MAX_SAFE_INTEGER`）、ID 冲突、非法文档命令等；
    /// 文案是当前稳定契约，改动需同步测试。
    pub fn prepare_commit(
        &self,
        writes: &[StorageWrite],
        seq: Option<Seq>,
    ) -> Result<PreparedMemoryCommit, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let seq = seq.unwrap_or_else(|| crate::types::seq_from_number(inner.next_seq));
        // TS: `!Number.isSafeInteger(seq) || seq < this.nextSeq`（spec §2.12 第 2 步）。
        if seq.get() > MAX_SAFE_INTEGER || seq.get() < inner.next_seq {
            return Err(error(format!(
                "Commit sequence {} does not strictly increase",
                seq
            )));
        }
        // 第一遍：把 document.copy 物化成携带 base 的 document.create，同时克隆所有写入。
        let detached = resolve_document_copies(&inner.state, writes.to_vec())?;
        // 第二遍：跨表 ID 命名空间检查（必须在 copy 展开后做，展开出的 create 也占 ID）。
        check_global_ids(&inner.state, &detached)?;
        // 第三遍：按文档 ID 聚合 create/change/retire；随后检查地址上是否会有两个当前化身。
        let actions = prepare_document_actions(&detached)?;
        check_document_actions(&inner.state, &actions)?;
        Ok(PreparedMemoryCommit {
            seq,
            writes: detached,
            actions,
            inner: Arc::clone(&self.inner),
            applied: AtomicBool::new(false),
        })
    }

    /// TS `materializeDocument(id, at)`。
    ///
    /// 物化步骤：
    ///
    /// 1. 未知 ID → `Ok(None)`；对 current-only 文档做非 `Current` 读取 → 请求错误。
    /// 2. 记录在该时间点不存活 → `Ok(None)`。
    /// 3. 取 `seq <= at` 的修订（`Current` 取全部），从后往前找**最后一个** base。
    /// 4. 从该 base 的值开始，顺序回放其后的每条 delta。
    /// 5. 结果必须是 JSON 对象。
    ///
    /// 返回值的 `version` 是选中 base 的版本（回放要求后续 delta 与它同版本），
    /// `deltas_since_base` 是实际回放的 delta 数。更深的历史 base 被跳过：同一时间点只以
    /// 最近一次 checkpoint 为起点，之前的修订不再参与物化。
    ///
    /// # Errors
    ///
    /// chord 回放的失败文本也会被转成存储错误。具体文案（除 `ops is not iterable` 外均带
    /// `Document {id} ` 前缀）：
    /// `does not retain historical content`（请求错误）、`missing a required base`、
    /// `has a base without a value`、`crosses a stored version boundary without a base`、
    /// `ops is not iterable`、`materialized a non-object value`。
    fn materialize_document(
        state: &State,
        id: DocumentId,
        at: DocumentPoint,
    ) -> Result<Option<StoredDocument>, BoxError> {
        let Some(stored) = state.documents.get(&id) else {
            return Ok(None);
        };
        if !matches!(at, DocumentPoint::Current) && is_current_only(&stored.record) {
            return Err(request_error(format!(
                "Document {id} does not retain historical content"
            )));
        }
        if !is_alive_at(&stored.record, at) {
            return Ok(None);
        }
        // 修订按 seq 升序追加，过滤后仍保持升序；`Current` 不做时间截断。
        let revisions: Vec<&DocumentRevision> = match at {
            DocumentPoint::Current => stored.revisions.iter().collect(),
            DocumentPoint::At(seq) => stored
                .revisions
                .iter()
                .filter(|revision| revision.seq.get() <= seq.get())
                .collect(),
        };
        // 从最新修订往回找最后一个 base：更早的 base 只是旧 checkpoint，选最近的能少回放
        // 一段历史；若整个列表没有 base（非法状态），显式报错而不是 panic。
        let mut base_index = revisions.len() as i64 - 1;
        while base_index >= 0
            && revisions[base_index as usize].content.kind != DocumentContentKind::Base
        {
            base_index -= 1;
        }
        if base_index < 0 {
            return Err(error(format!("Document {id} is missing a required base")));
        }
        let base = revisions[base_index as usize];
        // TS 的 `DocumentContent` 判别联合保证 base 带 `value`；Rust 的宽松结构体允许缺失，
        // 这种类型违约在 TS 里会把 `undefined` 交给 chord（运行期 TypeError），这里显式报错。
        let Some(base_value) = base.content.value.clone() else {
            return Err(error(format!("Document {id} has a base without a value")));
        };
        // TS 的 `applyImmutableBatches` 会逐批回放生成器产出的 `ops`：每拉到一批就应用，
        // 因此后一批的校验（版本边界 / `ops` 是否存在）发生在前一批成功应用之后。这里同样
        // 边校验边应用，保持错误优先级稳定（Rust 的 `apply_immutable_batches` 就是
        // 顺序 `apply_immutable` 的折叠，逐批调用与一次性传入等价）。
        let mut value = json_object_value(base_value);
        for revision in &revisions[base_index as usize + 1..] {
            if revision.content.kind != DocumentContentKind::Delta
                || revision.content.version != base.content.version
            {
                return Err(error(format!(
                    "Document {id} crosses a stored version boundary without a base"
                )));
            }
            // TS 的 `documentDeltaBatches` yield `revision.ops`，chord 的
            // `applyImmutableBatches` 再对 `ops` 做 `for...of`；`ops` 缺失或为 null 时 V8 抛
            // `TypeError: ops is not iterable`。这里同样显式报错，不能当成空批次静默应用。
            let Some(ops) = revision.content.ops.clone().map(|ops| ops.0) else {
                return Err(error("ops is not iterable"));
            };
            value = kiana_chord::delta::index::apply_immutable(value, &ops)
                .map_err(|failure| error(failure.to_string()))?;
        }
        let Some(value) = value_into_json_object(value) else {
            // TS 在这里只做 `as JsonObject` 断言；Rust 的 `JsonObject` 无法表示根被替换成
            // 非对象的值。文档定义恒为对象，因此这是类型违约，显式报错而不是静默丢弃。
            return Err(error(format!(
                "Document {id} materialized a non-object value"
            )));
        };
        Ok(Some(StoredDocument {
            record: stored.record.clone(),
            version: base.content.version,
            value,
            deltas_since_base: (revisions.len() - base_index as usize - 1) as u64,
        }))
    }

    /// `visibleEntries`：沿 fork 祖先链由新到旧。
    ///
    /// TS 的实现是生成器：调用方（`entry()` 的 `.next()`、`scanEntries` 的 `limit + 1` 循环）
    /// 按需拉取，所以一旦已经拿到足够的条目就不会继续走到父会话。`take` 复现这一惰性：
    /// 收集到 `take` 条后立即返回，不再读取父会话；链上真正缺失的父会话只在遍历走到它时
    /// 才成为错误（TS 在 `conversations.get(currentId)!` 处抛
    /// `TypeError: Cannot read properties of undefined (reading 'parent')`，这里返回同文案的
    /// 可捕获错误而不是 panic）。
    ///
    /// 输入边界 `[min_entry_id, max_entry_id]` 是闭区间；返回值借用 `state` 内的记录，函数
    /// 本身不取锁，调用方必须已持有状态锁。`take` 是“最多收集多少条”，不是分页 `limit`。
    fn visible_entries(
        state: &State,
        conversation_id: ConversationId,
        min_entry_id: Option<i64>,
        max_entry_id: Option<i64>,
        take: usize,
    ) -> Result<Vec<&EntryRecord>, BoxError> {
        if !state.conversations.contains_key(&conversation_id) {
            return Err(request_error(format!(
                "Unknown conversation: {conversation_id}"
            )));
        }
        let mut out = Vec::new();
        if take == 0 {
            return Ok(out);
        }
        let mut current_id = conversation_id;
        let mut upper_entry_id = max_entry_id;
        loop {
            let empty: Vec<EntryId> = Vec::new();
            let ids = state.entry_ids.get(&current_id).unwrap_or(&empty);
            let end = upper_entry_id.map_or(ids.len(), |upper| upper_bound(ids, upper));
            for index in (0..end).rev() {
                let id = ids[index].get();
                if min_entry_id.is_some_and(|min| id < min) {
                    break;
                }
                out.push(
                    state
                        .entries
                        .get(&ids[index])
                        .expect("entryIds only holds stored entries"),
                );
                if out.len() >= take {
                    return Ok(out);
                }
            }
            // 父会话缺失时 TS 的 `.get(currentId)!` 抛 TypeError；这里返回可捕获的错误，
            // 状态锁随后照常释放（绝不 panic，也不会毒化互斥锁）。
            let Some(conversation) = state.conversations.get(&current_id) else {
                return Err(error(
                    "Cannot read properties of undefined (reading 'parent')",
                ));
            };
            let Some(parent) = conversation.parent else {
                break;
            };
            upper_entry_id =
                Some(upper_entry_id.map_or(parent.at.get(), |upper| upper.min(parent.at.get())));
            if min_entry_id.is_some_and(|min| upper_entry_id.expect("set above") < min) {
                break;
            }
            current_id = parent.conversation_id;
        }
        Ok(out)
    }

    /// `visibleEntriesAscending`：同一祖先链，但由旧到新。
    ///
    /// TS 的生成器在第一次 `yield` 之前就建好整条祖先链（`segments`），所以父会话缺失
    /// 总是先报错，与 `take` 无关；这里保持同样的求值顺序。
    ///
    /// 边界同样是闭区间；输出顺序是“祖先段在前、当前会话在后”，每段内按 ID 升序。返回值
    /// 借用 `state`，调用方必须已持有状态锁。
    fn visible_entries_ascending(
        state: &State,
        conversation_id: ConversationId,
        min_entry_id: Option<i64>,
        max_entry_id: Option<i64>,
        take: usize,
    ) -> Result<Vec<&EntryRecord>, BoxError> {
        if !state.conversations.contains_key(&conversation_id) {
            return Err(request_error(format!(
                "Unknown conversation: {conversation_id}"
            )));
        }
        let mut segments: Vec<(ConversationId, Option<i64>)> = Vec::new();
        let mut current_id = conversation_id;
        let mut upper_entry_id = max_entry_id;
        loop {
            segments.push((current_id, upper_entry_id));
            // 与 `visible_entries` 相同：缺失的父会话在 TS 里是 TypeError，这里不 panic。
            let Some(conversation) = state.conversations.get(&current_id) else {
                return Err(error(
                    "Cannot read properties of undefined (reading 'parent')",
                ));
            };
            let Some(parent) = conversation.parent else {
                break;
            };
            upper_entry_id =
                Some(upper_entry_id.map_or(parent.at.get(), |upper| upper.min(parent.at.get())));
            if min_entry_id.is_some_and(|min| upper_entry_id.expect("set above") < min) {
                break;
            }
            current_id = parent.conversation_id;
        }
        let mut out = Vec::new();
        for (segment_conversation, segment_upper) in segments.into_iter().rev() {
            let empty: Vec<EntryId> = Vec::new();
            let ids = state.entry_ids.get(&segment_conversation).unwrap_or(&empty);
            let start = lower_bound(ids, min_entry_id.unwrap_or(i64::MIN));
            for id in &ids[start..] {
                if segment_upper.is_some_and(|upper| id.get() > upper) {
                    break;
                }
                out.push(
                    state
                        .entries
                        .get(id)
                        .expect("entryIds only holds stored entries"),
                );
                if out.len() >= take {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// 提交准备
// ---------------------------------------------------------------------------

/// TS `resolveDocumentCopies(writes)`。
///
/// 快速路径：本批没有 `document.copy` 时原样返回。否则先收集本批涉及的所有文档 ID
/// （create/copy 的目标、change/retire 的对象），再逐条把 copy 换成 create：
///
/// - 源文档不能同时出现在本批的文档写入里，否则报
///   `Fork source document {id} is changed in the copy batch`；
/// - 用 `materialize_document(source.id, source.at)` 物化源；缺失或在该时间点不存活即
///   `cannot be read`；
/// - 源记录与目标 `DocumentCreate` 都必须是 conversation 作用域，且
///   `kind`/`key`/`history`/`fork` 完全一致；
/// - 新 create 的初始 base 取物化结果的 `version` 和 `value`。
///
/// 任一失败都被包成 `DocumentCopyRejected { id: 目标 ID, cause }`。展开必须在 prepare
/// 阶段完成，apply 才能保持“只写入、不做可失败准备”的形状。
fn resolve_document_copies(
    state: &State,
    writes: Vec<StorageWrite>,
) -> Result<Vec<StorageWrite>, BoxError> {
    // 绝大多数提交没有 copy：直接返回，避免任何额外扫描或克隆。
    if !writes
        .iter()
        .any(|write| matches!(write, StorageWrite::DocumentCopy { .. }))
    {
        return Ok(writes);
    }
    // 先收集本批改动到的文档 ID（含 copy 目标自身）；源 ID 若在其中，下面会命中
    // `Fork source document ... is changed in the copy batch`。
    let mut changed_document_ids: Vec<DocumentId> = Vec::new();
    for write in &writes {
        let id = match write {
            StorageWrite::DocumentCreate { record, .. }
            | StorageWrite::DocumentCopy { record, .. } => Some(record.id),
            StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => {
                Some(*id)
            }
            _ => None,
        };
        if let Some(id) = id
            && !changed_document_ids.contains(&id)
        {
            changed_document_ids.push(id);
        }
    }
    // 按原始顺序重写：非 copy 写入原样保留，copy 变成等价的 create。
    let mut resolved = Vec::with_capacity(writes.len());
    for write in writes {
        let StorageWrite::DocumentCopy { record, source } = &write else {
            resolved.push(write);
            continue;
        };
        let copy_id = record.id;
        // 用闭包收集失败原因，统一在下面包成带目标 ID 的 DocumentCopyRejected。
        let outcome = (|| -> Result<StorageWrite, BoxError> {
            if changed_document_ids.contains(&source.id) {
                return Err(error(format!(
                    "Fork source document {} is changed in the copy batch",
                    source.id
                )));
            }
            let stored = MemoryStorage::materialize_document(state, source.id, source.at)?;
            let Some(stored) = stored else {
                return Err(error(format!(
                    "Fork source document {} cannot be read",
                    source.id
                )));
            };
            // fork 复制只允许 conversation 作用域的文档；源与目标的 scope 可以不同（目标通常
            // 在新会话上），但两者都必须是 Conversation 作用域，且 kind/key/history/fork 必须
            // 一致，否则报 `Fork source document <源 ID> does not match the copied record`。
            if stored.record.scope.kind() != ScopeKind::Conversation
                || record.scope.kind() != ScopeKind::Conversation
                || stored.record.kind != record.kind
                || stored.record.key != record.key
                || stored.record.history != record.history
                || stored.record.fork != record.fork
            {
                return Err(error(format!(
                    "Fork source document {} does not match the copied record",
                    source.id
                )));
            }
            Ok(StorageWrite::DocumentCreate {
                record: record.clone(),
                content: DocumentBaseContent::new(stored.version, stored.value),
            })
        })();
        match outcome {
            Ok(write) => resolved.push(write),
            Err(cause) => {
                return Err(Box::new(DocumentCopyRejected { id: copy_id, cause }));
            }
        }
    }
    Ok(resolved)
}

/// TS `checkGlobalIds(writes)`。
///
/// 全局数字命名空间按表分类：
///
/// - conversation / entry / document：ID 必须从未出现过，且同一批里只能写一次（copy 已在
///   前一步展开成 create）。
/// - task / submission：允许同表重复写（按 ID 替换是正常的状态更新），但跨表复用 ID 仍报错。
///
/// 检查同时看已提交状态（`record_types`）与本批已声明的 ID（`claimed`）。三类错误文案不同：
/// `ID {id} already belongs to {table}`、`ID {id} is written more than once`、
/// `ID {id} is written as two record types`。
fn check_global_ids(state: &State, writes: &[StorageWrite]) -> Result<(), BoxError> {
    // 与 TS 的 `Map` 对应；本函数只按 key 查询，不依赖迭代顺序。
    let mut claimed: IndexMap<i64, TableName> = IndexMap::new();
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
        let existing = state.record_types.get(&id).copied();
        let earlier = claimed.get(&id).copied();
        // 创建型表：ID 即使指向同表也视为冲突（记录不可重复创建）。
        // 可更新表：只有跨表复用才冲突，同表重复是本批内的多次更新。
        if matches!(
            table,
            TableName::Conversation | TableName::Entry | TableName::Document
        ) {
            if let Some(existing) = existing {
                return Err(error(format!(
                    "ID {id} already belongs to {}",
                    existing.as_str()
                )));
            }
            if earlier.is_some() {
                return Err(error(format!("ID {id} is written more than once")));
            }
        } else {
            if let Some(existing) = existing
                && existing != table
            {
                return Err(error(format!(
                    "ID {id} already belongs to {}",
                    existing.as_str()
                )));
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

/// TS `prepareDocumentActions(writes)`；`IndexMap` 保持首次出现顺序（与 JS `Map` 一致）。
///
/// 只为文档类写入（create/change/retire）建条目，并按文档 ID 聚合；`apply_document_actions`
/// 按同一顺序落地。约束：一个文档在一个提交里至多一条内容命令（create 携带的 base 或一次
/// change）与至多一次 retire；create + retire 合法，表示创建后立即退役。
///
/// # Errors
///
/// `Document {id} has more than one content command`、
/// `Document {id} is retired more than once`。
fn prepare_document_actions(
    writes: &[StorageWrite],
) -> Result<IndexMap<DocumentId, DocumentAction>, BoxError> {
    let mut actions: IndexMap<DocumentId, DocumentAction> = IndexMap::new();
    for write in writes {
        let (id, kind) = match write {
            StorageWrite::DocumentCreate { record, .. } => (record.id, 0u8),
            StorageWrite::DocumentChange { id, .. } => (*id, 1u8),
            StorageWrite::DocumentRetire { id } => (*id, 2u8),
            _ => continue,
        };
        // 0 = create、1 = change、2 = retire：本函数内的临时标签，用来复用同一条聚合路径。
        let action = actions.entry(id).or_default();
        match kind {
            0 => {
                if action.create.is_some() || action.content.is_some() {
                    return Err(error(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                let StorageWrite::DocumentCreate { record, content } = write else {
                    unreachable!("kind 0 is a create");
                };
                action.create = Some(record.clone());
                action.content = Some(DocumentContent::base(
                    content.version,
                    content.value.clone(),
                ));
            }
            1 => {
                if action.content.is_some() {
                    return Err(error(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                let StorageWrite::DocumentChange { content, .. } = write else {
                    unreachable!("kind 1 is a change");
                };
                action.content = Some(content.clone());
            }
            _ => {
                if action.retire {
                    return Err(error(format!("Document {id} is retired more than once")));
                }
                action.retire = true;
            }
        }
    }
    Ok(actions)
}

/// TS `checkDocumentActions(actions)`。
///
/// 对聚合后的每个动作检查（全部针对提交前状态）：
///
/// 1. create 与既有记录互斥：无 create 且无记录 → `Unknown document`；create 但已存在 →
///    `already exists`；既有记录已退役 → `is retired`。
/// 2. delta 必须接在一条已有修订之后，且版本与上一条相同；否则报 `delta has no base` 或
///    `version transition requires a base`。
/// 3. 地址存活计数：每个地址从“当前是否有 current 化身”起算，retire 当前化身 -1，create
///    且不立即退役 +1；任何地址最终大于 1 就报
///    `Document address already has a current incarnation`。
///
/// 因为 create+change 已在聚合阶段被拒绝，这里不需要把“本批新建的文档”当作 change 的目标。
fn check_document_actions(
    state: &State,
    actions: &IndexMap<DocumentId, DocumentAction>,
) -> Result<(), BoxError> {
    // 每个地址的存活化身数；初值取提交前状态（有 current 化身即 1），同批多个动作共享。
    let mut live_counts: IndexMap<String, i64> = IndexMap::new();
    for (id, action) in actions {
        let existing = state.documents.get(id);
        if action.create.is_none() && existing.is_none() {
            return Err(error(format!("Unknown document: {id}")));
        }
        if action.create.is_some() && existing.is_some() {
            return Err(error(format!("Document {id} already exists")));
        }
        if let Some(existing) = existing
            && existing.record.retired_at.is_some()
        {
            return Err(error(format!("Document {id} is retired")));
        }
        let previous = existing.and_then(|existing| existing.revisions.last());
        if let Some(content) = &action.content
            && content.kind == DocumentContentKind::Delta
        {
            let Some(previous) = previous else {
                return Err(error(format!("Document {id} delta has no base")));
            };
            if previous.content.version != content.version {
                return Err(error(format!(
                    "Document {id} version transition requires a base"
                )));
            }
        }

        // change/retire 沿用既有记录的地址键；create 用待创建记录的地址键。
        let key = match &action.create {
            None => record_address_key(&existing.expect("checked above").record),
            Some(create) => create_address_key(create),
        };
        let current_id = state
            .document_addresses
            .get(&key)
            .and_then(|address| address.current_id);
        let mut live = live_counts
            .get(&key)
            .copied()
            .unwrap_or(if current_id.is_some() { 1 } else { 0 });
        // 只有退役的恰好是当前化身才减少；退役历史化身不影响 current 计数。
        if action.retire && current_id == Some(*id) {
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

// ---------------------------------------------------------------------------
// 提交应用
// ---------------------------------------------------------------------------

/// TS `applyPreparedCommit(prepared, documentActions, seq)`。
///
/// 应用顺序：先按写入顺序应用表记录（文档类写入在这里跳过），再统一应用聚合后的文档动作
/// （按动作在 `prepare_document_actions` 中的首次出现顺序），最后推进 `next_seq`。
/// `next_id` 对每条表记录取 `max(next_id, id + 1)`；文档 ID 在 `apply_document_actions` 里
/// 做同样处理。
///
/// # Errors
///
/// 只有遇到未展开的 `document.copy` 才会返回错误；此时排在它前面的写入可能已经应用
/// （正常调用路径不可达，见 [`PreparedMemoryCommit::apply`]）。
fn apply_prepared_commit(
    inner: &mut Inner,
    prepared: &[StorageWrite],
    document_actions: &IndexMap<DocumentId, DocumentAction>,
    seq: Seq,
) -> Result<Seq, BoxError> {
    // 表记录按提交内的原始顺序落地；主表与它的所有二级索引在同一个分支里更新。
    for write in prepared {
        match write {
            StorageWrite::Conversation { value } => {
                let state = &mut inner.state;
                state
                    .record_types
                    .insert(value.id.get(), TableName::Conversation);
                state.conversations.insert(value.id, value.clone());
                insert_sorted(&mut state.conversation_ids, value.id);
                if let Some(owner) = value.owner {
                    insert_map_id(
                        &mut state.conversation_ids_by_owner_conversation,
                        owner.conversation_id,
                        value.id,
                    );
                    insert_map_id(
                        &mut state.conversation_ids_by_owner_task,
                        owner.task_id,
                        value.id,
                    );
                }
                inner.next_id = inner.next_id.max(value.id.get().saturating_add(1));
            }
            // 条目：主表 + 会话内有序索引 + head 索引 + 提交序号；后三者是
            // `visible_entry` / `find_latest_head_marker` / 祖先链扫描的依据。
            StorageWrite::Entry { value } => {
                let state = &mut inner.state;
                state.record_types.insert(value.id.get(), TableName::Entry);
                state.entries.insert(value.id, value.clone());
                state.entry_commit_seqs.insert(value.id, seq);
                insert_sorted(
                    state.entry_ids.entry(value.conversation_id).or_default(),
                    value.id,
                );
                if value.head.is_some() {
                    insert_sorted(
                        state
                            .head_entry_ids
                            .entry(value.conversation_id)
                            .or_default(),
                        value.id,
                    );
                }
                inner.next_id = inner.next_id.max(value.id.get().saturating_add(1));
            }
            // 任务更新：主表按 ID 替换；状态桶只在状态变化时搬移，同状态重复写入不移动位置。
            StorageWrite::Task { value } => {
                let state = &mut inner.state;
                state.record_types.insert(value.id.get(), TableName::Task);
                let previous_status = state
                    .tasks
                    .get(&value.id)
                    .map(|previous| previous.state.status());
                match previous_status {
                    None => {
                        insert_sorted(&mut state.task_ids, value.id);
                        insert_sorted(
                            state.task_ids_by_status.get_mut(value.state.status()),
                            value.id,
                        );
                    }
                    Some(previous) if previous != value.state.status() => {
                        remove_sorted(state.task_ids_by_status.get_mut(previous), value.id);
                        insert_sorted(
                            state.task_ids_by_status.get_mut(value.state.status()),
                            value.id,
                        );
                    }
                    Some(_) => {}
                }
                state.tasks.insert(value.id, value.clone());
                inner.next_id = inner.next_id.max(value.id.get().saturating_add(1));
            }
            // 提交更新：状态桶与 `(conversation, requestId)` 去重索引必须同步。旧记录带
            // requestId 且仍指向本提交时先删除旧映射；会话的映射表空了就移除整个条目，
            // 与 TS 的 `Map.size === 0` 清理一致。
            StorageWrite::Submission { value } => {
                let state = &mut inner.state;
                state
                    .record_types
                    .insert(value.id.get(), TableName::Submission);
                let previous = state.submissions.get(&value.id).cloned();
                match &previous {
                    None => {
                        insert_sorted(&mut state.submission_ids, value.id);
                        insert_sorted(
                            state.submission_ids_by_status.get_mut(value.status),
                            value.id,
                        );
                    }
                    Some(previous) if previous.status != value.status => {
                        remove_sorted(
                            state.submission_ids_by_status.get_mut(previous.status),
                            value.id,
                        );
                        insert_sorted(
                            state.submission_ids_by_status.get_mut(value.status),
                            value.id,
                        );
                    }
                    Some(_) => {}
                }
                if let Some(previous) = &previous
                    && let Some(request_id) = &previous.request_id
                {
                    let mut emptied = false;
                    if let Some(requests) = state
                        .submission_ids_by_request
                        .get_mut(&previous.conversation_id)
                        && requests.get(request_id) == Some(&value.id)
                    {
                        requests.remove(request_id);
                        emptied = requests.is_empty();
                    }
                    if emptied {
                        state
                            .submission_ids_by_request
                            .remove(&previous.conversation_id);
                    }
                }
                state.submissions.insert(value.id, value.clone());
                if let Some(request_id) = &value.request_id {
                    state
                        .submission_ids_by_request
                        .entry(value.conversation_id)
                        .or_default()
                        .insert(request_id.clone(), value.id);
                }
                inner.next_id = inner.next_id.max(value.id.get().saturating_add(1));
            }
            // prepare 阶段应已消除所有 copy；能走到这里说明内部不变量被破坏（apply 不做读取，
            // 因此不能在这里补做展开）。
            StorageWrite::DocumentCopy { .. } => {
                return Err(error("Prepared document copy was not resolved"));
            }
            StorageWrite::DocumentCreate { .. }
            | StorageWrite::DocumentChange { .. }
            | StorageWrite::DocumentRetire { .. } => {}
        }
    }

    // 文档类写入在上一轮被全部跳过；这里按聚合顺序统一落地修订、地址索引与 current 标记。
    apply_document_actions(&mut inner.state, &mut inner.next_id, document_actions, seq);
    // 与 TS 相同：直接设为 seq + 1，不重新比较当前 next_seq（见模块文档的并发注意事项）。
    inner.next_seq = seq.get().saturating_add(1);
    Ok(seq)
}

/// TS `applyDocumentActions(actions, seq)`。
///
/// 对每个动作：
///
/// - **create**：用 `seq` 打 `created_at`（同一提交里 retire 则同时打 `retired_at`），初始
///   修订就是该动作的 content；登记 `record_types`、地址索引、作用域索引并推进 `next_id`。
/// - **既有文档的 content**：追加修订；current-only 文档遇到新 base 时用新 base 替换整个
///   修订列表。
/// - **既有文档 retire**：打 `retired_at`；current-only 文档清空修订列表。
/// - **地址 current_id**：retire 当前化身则清空；create 且未立即退役则指向新化身。
///
/// 本函数不做校验，内部 `expect` 依赖 `check_document_actions` 已经通过，以及
/// `prepare_document_actions` 保证 “create 一定带 content”。
fn apply_document_actions(
    state: &mut State,
    next_id: &mut i64,
    actions: &IndexMap<DocumentId, DocumentAction>,
    seq: Seq,
) {
    for (id, action) in actions {
        if let Some(create) = &action.create {
            // create：地址字段来自 DocumentCreate，时间戳只能由本次提交打上。
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
            let content = action
                .content
                .clone()
                .expect("a created document always carries its initial base");
            state.record_types.insert(id.get(), TableName::Document);
            state.documents.insert(
                *id,
                StoredDocumentState {
                    record: record.clone(),
                    revisions: vec![DocumentRevision { content, seq }],
                },
            );

            // 地址索引保存该地址下的全部化身（升序）；scope 索引供 scan_documents 使用。
            let key = record_address_key(&record);
            insert_sorted(
                &mut state.document_addresses.entry(key).or_default().ids,
                *id,
            );

            let scope_key = scope_key(&record.scope);
            insert_sorted(
                state.document_ids_by_scope.entry(scope_key).or_default(),
                *id,
            );
            *next_id = (*next_id).max(id.get().saturating_add(1));
        } else if let Some(content) = &action.content {
            // change：修订追加到历史；current-only 文档的新 base 是 checkpoint，替换旧历史。
            let stored = state
                .documents
                .get_mut(id)
                .expect("checked document actions only touch existing documents");
            let revision = DocumentRevision {
                content: content.clone(),
                seq,
            };
            if content.kind == DocumentContentKind::Base && is_current_only(&stored.record) {
                stored.revisions = vec![revision];
            } else {
                stored.revisions.push(revision);
            }
        }

        if action.retire && action.create.is_none() {
            // 对既有文档的 retire：时间戳在这里补；create+retire 的时间戳已在创建记录时设置。
            let stored = state
                .documents
                .get_mut(id)
                .expect("checked document actions only touch existing documents");
            stored.record.retired_at = Some(seq);
        }
        if action.retire {
            // current-only 文档退役后历史不可读，直接释放修订列表；可回溯文档保留历史。
            let stored = state
                .documents
                .get_mut(id)
                .expect("checked document actions only touch existing documents");
            if is_current_only(&stored.record) {
                stored.revisions.clear();
            }
        }
        if action.create.is_some() || action.retire {
            // 最后同步地址的当前化身：retire 清空、create 设置；create+retire 两者都不做。
            let stored = state
                .documents
                .get(id)
                .expect("checked document actions only touch existing documents");
            let key = record_address_key(&stored.record);
            let address = state
                .document_addresses
                .get_mut(&key)
                .expect("created documents always register their address");
            if action.retire && address.current_id == Some(*id) {
                address.current_id = None;
            }
            if action.create.is_some() && !action.retire {
                address.current_id = Some(*id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// impl Storage
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl Storage for MemoryStorage {
    /// 原子提交：`prepare_commit`（校验并脱离）→ `apply`（写入）→ `adopt(seq)`。
    ///
    /// `adopt` 只在成功提交后调用一次，且在状态锁释放之后同步执行（`apply` 返回时锁已释放）。
    /// `context` 被忽略：本后端没有可取消的 IO。
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        _context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        let seq = self.prepare_commit(writes, None)?.apply()?;
        adopt(seq);
        Ok(seq)
    }

    /// 从共享数字命名空间取下一个 ID；`kind` 不参与分配（TS 的 `mintId` 也没有 kind 参数）。
    ///
    /// 只递增计数器，不写入记录；用返回的 ID 提交时仍要经过 `check_global_ids`。
    /// `next_id` 超过 2^53-1 后返回 `ID space is exhausted`。
    async fn mint_id(&self, _kind: IdKind) -> Result<i64, BoxError> {
        let mut inner = self.lock();
        Self::assert_open(&inner)?;
        if inner.next_id > 9_007_199_254_740_991 {
            return Err(error("ID space is exhausted"));
        }
        let id = inner.next_id;
        inner.next_id += 1;
        Ok(id)
    }

    async fn conversation(
        &self,
        id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        Ok(inner.state.conversations.get(&id).cloned())
    }

    /// 按 ID 顺序扫描会话。`owner_task_id` 优先于 `owner_conversation_id` 选择候选索引；两个
    /// 条件同时给出时，先在 owner-task 索引上扫描，再在循环里按 owner conversation 过滤。
    /// 默认升序（有游标时以游标里的顺序为准）。
    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let state = &inner.state;
        let empty: Vec<ConversationId> = Vec::new();
        let ids: &[ConversationId] = if let Some(owner_task_id) = query.owner_task_id {
            state
                .conversation_ids_by_owner_task
                .get(&owner_task_id)
                .map(Vec::as_slice)
                .unwrap_or(&empty)
        } else if let Some(owner_conversation_id) = query.owner_conversation_id {
            state
                .conversation_ids_by_owner_conversation
                .get(&owner_conversation_id)
                .map(Vec::as_slice)
                .unwrap_or(&empty)
        } else {
            &state.conversation_ids
        };
        let start = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        // 收集到 limit + 1 条后退出（多出的一条由 `page` 用来判断是否还有下一页）；过滤不
        // 命中时继续扫描索引，直到耗尽。
        let mut values: Vec<ConversationRecord> = Vec::new();
        for index in scan_indexes(ids, start) {
            if values.len() > limit {
                break;
            }
            let value = state
                .conversations
                .get(&ids[index])
                .expect("conversationIds only holds stored conversations");
            // owner-task 索引已经保证任务维度；这里再过滤会话维度（两个条件可以同时给出）。
            if let Some(owner_conversation_id) = query.owner_conversation_id
                && value.owner.map(|owner| owner.conversation_id) != Some(owner_conversation_id)
            {
                continue;
            }
            values.push(value.clone());
        }
        page(values, limit, start.order)
    }

    /// 按全局 ID 查条目，不做 fork 可见性过滤；`commit_seq` 来自 `entry_commit_seqs`。
    async fn entry(
        &self,
        id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let Some(entry) = inner.state.entries.get(&id) else {
            return Ok(None);
        };
        Ok(Some(StoredEntry {
            entry: entry.clone(),
            commit_seq: *inner
                .state
                .entry_commit_seqs
                .get(&id)
                .expect("every stored entry records its commit"),
        }))
    }

    /// TS 单方法 `entry` 的三参重载：只在请求会话的 fork 祖先链内查精确 ID。
    ///
    /// 用闭区间 `[id, id]` 调 `visible_entries` 且 `take = 1`，找到即停，不会继续访问父会话；
    /// 找不到返回 `None`，未知会话返回请求错误。返回值里的 `commit_seq` 按请求的 `id` 查。
    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let visible = MemoryStorage::visible_entries(
            &inner.state,
            conversation_id,
            Some(id.get()),
            Some(id.get()),
            // TS 只对生成器调用一次 `.next()`：拿到第一条就停，绝不继续走祖先链。
            1,
        )?;
        let Some(entry) = visible.first().copied() else {
            return Ok(None);
        };
        Ok(Some(StoredEntry {
            entry: entry.clone(),
            commit_seq: *inner
                .state
                .entry_commit_seqs
                .get(&id)
                .expect("every stored entry records its commit"),
        }))
    }

    /// 沿 fork 祖先链找 `<= at_or_before_entry_id` 的最新带 `head` 的条目。
    ///
    /// 当前会话没有符合条件的 head 时上溯父会话，并用 `parent.at` 收紧上界（与已有上界取
    /// 较小值），因此看不到 fork 点之后的父条目。`at_or_before_entry_id` 为 `None` 表示不设
    /// 上界。未知会话报请求错误；链上父会话缺失时报与 TS `TypeError` 同文案的存储错误。
    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        _context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let state = &inner.state;
        if !state.conversations.contains_key(&conversation_id) {
            return Err(request_error(format!(
                "Unknown conversation: {conversation_id}"
            )));
        }
        let mut current_id = conversation_id;
        let mut upper_entry_id = at_or_before_entry_id.map(|id| id.get());
        loop {
            let empty: Vec<EntryId> = Vec::new();
            let ids = state.head_entry_ids.get(&current_id).unwrap_or(&empty);
            // head 索引按 ID 升序；`end` 是第一个 > upper 的下标，`end - 1` 即最新候选。
            let end = upper_entry_id.map_or(ids.len(), |upper| upper_bound(ids, upper));
            if end > 0 {
                let entry = state
                    .entries
                    .get(&ids[end - 1])
                    .expect("headEntryIds only holds stored entries");
                // 索引里的条目本来就带 head；显式重设既复现 TS `{ ...entry, head }` 的返回
                // 形状，也把“head 索引只含带 head 的条目”这一不变量写进代码。
                let mut marker = entry.clone();
                marker.head = Some(
                    entry
                        .head
                        .expect("headEntryIds only holds entries with a head"),
                );
                return Ok(Some(marker));
            }
            let Some(conversation) = state.conversations.get(&current_id) else {
                // TS 的 `.get(currentId)!` 对未提交的父会话抛 TypeError；返回可捕获错误。
                return Err(error(
                    "Cannot read properties of undefined (reading 'parent')",
                ));
            };
            let Some(parent) = conversation.parent else {
                return Ok(None);
            };
            upper_entry_id =
                Some(upper_entry_id.map_or(parent.at.get(), |upper| upper.min(parent.at.get())));
            current_id = parent.conversation_id;
        }
    }

    /// fork-aware 的条目扫描：按 ID 顺序合并本会话与祖先链的可见条目。
    ///
    /// 默认降序（有游标时以游标顺序为准）。游标收紧扫描“远离”的那一侧边界：降序把
    /// `max_entry_id` 压到严格小于 `after`，升序把 `min_entry_id` 抬到严格大于 `after`，
    /// 续页从上一页最后一条之后继续（边界算法与 TS 相同）。内部最多收集 `limit + 1` 条
    /// （多出的一条只用于判断 `next`），与 TS 生成器的惰性求值一致。
    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let state = &inner.state;
        let start = scan_start(query.order, cursor, ScanOrder::Descending)?;
        // 游标收紧扫描远离的那一侧的边界。
        let mut min_entry_id = query.min_entry_id.map(|id| id.get());
        let mut max_entry_id = query.max_entry_id.map(|id| id.get());
        if let Some(after) = start.after {
            match start.order {
                ScanOrder::Descending => {
                    max_entry_id = Some(
                        max_entry_id
                            .map_or(after, |max| max.min(after))
                            .min(after.saturating_sub(1)),
                    );
                }
                ScanOrder::Ascending => {
                    min_entry_id = Some(
                        min_entry_id
                            .map_or(after, |min| min.max(after))
                            .max(after.saturating_add(1)),
                    );
                }
            }
        }
        // TS 的 `for (const entry of entries) { ...; if (visible.length > limit) break; }` 至多
        // 拉取 `limit + 1` 条——生成器是惰性的，达到这个数量后不再读取父会话。
        let take = limit.saturating_add(1);
        let entries = match start.order {
            ScanOrder::Descending => MemoryStorage::visible_entries(
                state,
                query.conversation_id,
                min_entry_id,
                max_entry_id,
                take,
            )?,
            ScanOrder::Ascending => MemoryStorage::visible_entries_ascending(
                state,
                query.conversation_id,
                min_entry_id,
                max_entry_id,
                take,
            )?,
        };
        let mut visible: Vec<EntryRecord> = Vec::new();
        for entry in entries {
            visible.push(entry.clone());
            if visible.len() > limit {
                break;
            }
        }
        page(visible, limit, start.order)
    }

    async fn task(
        &self,
        id: TaskId,
        _context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        Ok(inner.state.tasks.get(&id).cloned())
    }

    /// 扫描任务：`status` 选候选桶（`None` 用全局 `task_ids`），其余过滤条件
    /// （`conversation_id`/`kind`/`abort_requested`/`background`）在循环内逐条过滤。
    /// 默认升序；收集到 `limit + 1` 条或索引耗尽时停止。
    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let state = &inner.state;
        let start = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        // status 过滤走二级索引；没有 status 时用全局有序 ID 列表。
        let ids: &[TaskId] = match query.status {
            None => &state.task_ids,
            Some(status) => state.task_ids_by_status.get(status),
        };
        let mut values: Vec<TaskRecord> = Vec::new();
        for index in scan_indexes(ids, start) {
            if values.len() > limit {
                break;
            }
            let value = state
                .tasks
                .get(&ids[index])
                .expect("taskIds only holds stored tasks");
            if let Some(conversation_id) = query.conversation_id
                && value.conversation_id != conversation_id
            {
                continue;
            }
            if let Some(kind) = &query.kind
                && &value.kind != kind
            {
                continue;
            }
            if let Some(abort_requested) = query.abort_requested
                && value.abort_requested != abort_requested
            {
                continue;
            }
            if let Some(background) = query.background
                && value.background != background
            {
                continue;
            }
            values.push(value.clone());
        }
        page(values, limit, start.order)
    }

    async fn submission(
        &self,
        id: SubmissionId,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        Ok(inner.state.submissions.get(&id).cloned())
    }

    /// 扫描提交：`status` 选候选桶（`None` 用全局 `submission_ids`），`conversation_id` 在
    /// 循环内过滤；默认升序，收集到 `limit + 1` 条或索引耗尽时停止。
    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let state = &inner.state;
        let start = scan_start(query.order, cursor, ScanOrder::Ascending)?;
        let ids: &[SubmissionId] = match query.status {
            None => &state.submission_ids,
            Some(status) => state.submission_ids_by_status.get(status),
        };
        let mut values: Vec<SubmissionRecord> = Vec::new();
        for index in scan_indexes(ids, start) {
            if values.len() > limit {
                break;
            }
            let value = state
                .submissions
                .get(&ids[index])
                .expect("submissionIds only holds stored submissions");
            if let Some(conversation_id) = query.conversation_id
                && value.conversation_id != conversation_id
            {
                continue;
            }
            values.push(value.clone());
        }
        page(values, limit, start.order)
    }

    /// 用 `(conversation_id, request_id)` 去重索引查提交；索引只保留最新写入，未命中返回
    /// `None`（不扫描提交表）。索引指向的提交若不在主表里，同样返回 `None`。
    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        _context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let Some(id) = inner
            .state
            .submission_ids_by_request
            .get(&conversation_id)
            .and_then(|requests| requests.get(request_id))
        else {
            return Ok(None);
        };
        Ok(inner.state.submissions.get(id).cloned())
    }

    /// 解析逻辑地址：`Current` 直接读地址索引的 `current_id`；历史时间点按 `ids` 升序找
    /// 第一个在该时间点存活的化身。只返回化身记录，不物化内容。
    ///
    /// 地址不存在或没有存活化身时返回 `None`。本方法不检查 current-only 文档的历史读取——
    /// 该检查只在物化内容时发生（见 [`Storage::document`] 的实现）。
    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let state = &inner.state;
        let index = state.document_addresses.get(&address_key(address));
        if matches!(at, DocumentPoint::Current) {
            return Ok(index.and_then(|index| index.current_id).map(|id| {
                state
                    .documents
                    .get(&id)
                    .expect("documentAddresses only holds stored documents")
                    .record
                    .clone()
            }));
        }
        let empty: Vec<DocumentId> = Vec::new();
        for id in index.map(|index| &index.ids).unwrap_or(&empty) {
            let record = &state
                .documents
                .get(id)
                .expect("documentAddresses only holds stored documents")
                .record;
            if is_alive_at(record, at) {
                return Ok(Some(record.clone()));
            }
        }
        Ok(None)
    }

    /// 按 ID 物化一个化身；步骤与错误见 `materialize_document`。未知 ID 或在该时间点不存活
    /// 返回 `None`；对 current-only 文档做历史读取返回请求错误。
    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        _context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        MemoryStorage::materialize_document(&inner.state, id, at)
    }

    /// 扫描某个精确作用域内的文档记录（不物化内容）。
    ///
    /// 作用域经 `scope_key` 定位索引；游标只读 `after`（安全整数）且扫描固定升序，游标里的
    /// `order` 被忽略。`kind` 过滤与 `is_alive_at(query.at)` 都在循环内进行，直到收集
    /// `limit + 1` 条或索引耗尽。
    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        _context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        let inner = self.lock();
        Self::assert_open(&inner)?;
        let state = &inner.state;
        let empty: Vec<DocumentId> = Vec::new();
        let ids = state
            .document_ids_by_scope
            .get(&scope_key(&query.scope))
            .unwrap_or(&empty);
        let after = cursor_id(cursor)?;
        let start = after.map_or(0, |after| upper_bound(ids, after));
        let mut values: Vec<DocumentRecord> = Vec::new();
        let mut index = start;
        while index < ids.len() && values.len() <= limit {
            let record = &state
                .documents
                .get(&ids[index])
                .expect("documentIdsByScope only holds stored documents")
                .record;
            index += 1;
            if let Some(kind) = &query.kind
                && &record.kind != kind
            {
                continue;
            }
            if is_alive_at(record, query.at) {
                values.push(record.clone());
            }
        }
        page(values, limit, ScanOrder::Ascending)
    }

    /// 置位关闭标志；幂等。不释放状态，也不影响已经 prepare 的提交；之后所有读取入口与
    /// `prepare_commit`/`mint_id` 都以 `MemoryStorage is closed` 拒绝。
    async fn close(&self, _context: &dyn Context) {
        let mut inner = self.lock();
        inner.closed = true;
    }
}

// 本文件的单元测试锁定三类不变量：
// 1. `scope_key` / `address_key` 生成的文本保持既有 `JSON.stringify` 逐字节格式（稳定契约）；
// 2. `prepare_commit` 的序号规则（显式序号、安全整数上界、空提交推进 `next_seq`）；
// 3. `MemoryStorage` / `PreparedMemoryCommit` 的 `Send + Sync` 编译期断言（`assert_send_sync`）。
// 更完整的 Storage 契约在 tests/memory_storage.rs 的 conformance 用例里。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ConversationRecord, DocumentScope, StorageWrite, id_from_number, seq_from_number,
    };

    /// `scopeKey` / `addressKey` 生成的文本保持既有 `JSON.stringify` 逐字节格式（spec §2.12 的稳定契约）。
    #[test]
    fn address_keys_match_typescript_json_stringify() {
        let conversation = DocumentScope::Conversation {
            conversation_id: id_from_number(7),
        };
        assert_eq!(scope_key(&DocumentScope::Session), r#"["session"]"#);
        assert_eq!(scope_key(&conversation), r#"["conversation",7]"#);
        assert_eq!(
            scope_key(&DocumentScope::Task {
                task_id: id_from_number(9)
            }),
            r#"["task",9]"#
        );
        let singleton = DocumentAddress {
            kind: "kiana.agent".to_owned(),
            scope: conversation,
            key: None,
        };
        assert_eq!(
            address_key(&singleton),
            r#"["kiana.agent","[\"conversation\",7]",["singleton"]]"#
        );
        let family = DocumentAddress {
            kind: "kiana.agent".to_owned(),
            scope: conversation,
            key: Some(serde_json::json!("member")),
        };
        assert_eq!(
            address_key(&family),
            r#"["kiana.agent","[\"conversation\",7]",["family","member"]]"#
        );
    }

    fn assert_send_sync<T: Send + Sync>() {}

    /// `JSONL` 后端复用同一个 `prepare_commit(writes, seq)` 入口（TS 的 `jsonl/storage.ts`）。
    #[tokio::test]
    async fn prepare_commit_applies_an_explicit_sequence() {
        assert_send_sync::<MemoryStorage>();
        assert_send_sync::<PreparedMemoryCommit>();
        let storage = MemoryStorage::new();
        let writes = vec![StorageWrite::Conversation {
            value: ConversationRecord {
                id: id_from_number(1),
                parent: None,
                owner: None,
            },
        }];
        let prepared = storage
            .prepare_commit(&writes, Some(seq_from_number(7)))
            .expect("prepare");
        assert_eq!(prepared.seq(), seq_from_number(7));
        assert_eq!(prepared.apply().expect("apply"), seq_from_number(7));
        // 下一次提交从 8 开始（`nextSeq = seq + 1`）。
        let next = storage.prepare_commit(&[], None).expect("prepare empty");
        assert_eq!(next.seq(), seq_from_number(8));
        // 低于 `nextSeq` 的显式序号被拒绝；不消耗状态。
        let stale = storage
            .prepare_commit(&[], Some(seq_from_number(7)))
            .expect_err("stale sequence");
        assert_eq!(
            stale.to_string(),
            "Commit sequence 7 does not strictly increase"
        );
        // 空提交也会推进 `nextSeq`。
        assert_eq!(next.apply().expect("apply empty"), seq_from_number(8));
        assert_eq!(
            storage.prepare_commit(&[], None).expect("prepare").seq(),
            seq_from_number(9)
        );
    }

    /// 回归（spec §2.12 第 2 步）：`prepareCommit` 的 `!Number.isSafeInteger(seq)` 半边。
    ///
    /// `Seq` 是 `i64`，非整数 / `NaN` / `Infinity` 不可表示、负数由 `seq < nextSeq` 覆盖；
    /// 只有 2^53-1 的上界需要显式检查。TS 对 2^53 的显式序号与非安全的默认序号都报同一句话。
    #[tokio::test]
    async fn prepare_commit_rejects_sequences_beyond_the_safe_integer_range() {
        let storage = MemoryStorage::new();
        let unsafe_seq = storage
            .prepare_commit(&[], Some(seq_from_number(9_007_199_254_740_992)))
            .expect_err("2^53 is not a safe integer");
        assert_eq!(
            unsafe_seq.to_string(),
            "Commit sequence 9007199254740992 does not strictly increase"
        );
        // 拒绝的提交不消耗状态。
        assert_eq!(
            storage.prepare_commit(&[], None).expect("prepare").seq(),
            seq_from_number(1)
        );

        // 2^53-1 本身是 safe integer，可以提交；随后 `nextSeq = 2^53` 不再是安全整数，
        // 默认序号与显式非安全序号都会像 TS 一样被拒绝。
        let boundary = storage
            .prepare_commit(&[], Some(seq_from_number(MAX_SAFE_INTEGER)))
            .expect("the largest safe integer is accepted");
        assert_eq!(
            boundary.apply().expect("apply"),
            seq_from_number(MAX_SAFE_INTEGER)
        );
        let next = storage
            .prepare_commit(&[], None)
            .expect_err("the default sequence is no longer a safe integer");
        assert_eq!(
            next.to_string(),
            "Commit sequence 9007199254740992 does not strictly increase"
        );
    }
}
