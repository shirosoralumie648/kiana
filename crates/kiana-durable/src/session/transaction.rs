//! `session/transaction.ts`（1211 行）：一次 Session 提交的事务。
//!
//! 与 TS 的对应关系见 `spec/durable-rest.md` §2.17。几个 Rust 侧的结构差异：
//!
//! - TS 的 `Draft<T>` 是记录赋值的可变代理，`prepare()` 产出 op 批次；这里 `DocDraft` 是普通
//!   可变 JSON 对象（`crate::types::DocDraft`），结算时用 chord 的 `diff_revisions()` 把它与
//!   载入值做 diff。因此纯结构性的 no-op（先 shift 再 unshift）不会产生 op（见交付说明的
//!   残留差异）。
//! - TS 的 `Tracker` 用指针交换采纳新值；这里 [`Tracker`] 是 `Mutex<JsonObject>`，`adopt()`
//!   整体替换。`LoadedDocument` 的 `storedVersion`/`deltasSinceBase` 是可变状态，放在
//!   `Arc<Mutex<LoadedState>>` 里，由缓存与所有克隆共享。
//! - 事务的可变状态全部在 `Mutex<TxInner>` 里，adopt 回调只短暂加锁（`Storage::commit` 的
//!   `adopt` 是同步 `Fn`），因此可以安全地跨 `await`。
//!
//! # 这个文件负责什么
//!
//! 它是 `Tx` trait（定义在 `crate::types`）在本 crate 里的生产实现：Session 的一次提交
//! 回调拿到的就是这里的 [`Transaction`]。回调期间的所有意图先暂存在内存里（会话、条目、
//! 任务、提交记录、文档化身），回调成功返回后才由 `settle_success()` 统一校验并装配成一个
//! 原子写入批次，再交给 `Storage::commit`。因此回调中途失败不会留下持久副作用。
//!
//! # 一次提交的执行流程
//!
//! 1. `Session` 构造 [`Transaction`]，以 `&Transaction` 调用用户回调；
//! 2. 表写入（`create_conversation` / `append_entry` / `create_task` / ...）只进入事务暂存；
//!    文档写入先通过 `DocDraft` 改共享活值，结算时才 diff 成 ops；
//! 3. 回调返回错误时调用 `settle_failure()`：封闭事务、放弃所有草稿、等排空在途操作；
//! 4. 回调成功时调用 `settle_success()`：封闭事务 → 所有文档 `prepare()` → `assemble()` 做
//!    跨记录校验并生成写入批次；
//! 5. 无写入直接 `discard()`；延迟提交路径用 `holdable()` / `adopt_held()` 只更新内存；
//!    其余路径先 `take_held()` 折叠已持有变化，再用 `current_writes()` 取最终批次；
//! 6. `Storage::commit` 持久化成功后同步调用 `adopt(seq)`，由它更新 tracker、安装/驱逐缓存
//!    化身并生成发布列表。
//!
//! # 并发与锁
//!
//! - 事务状态在一个 `std::sync::Mutex<TxInner>` 里；所有需要 `await` 的路径都先在一个独立
//!   代码块里取锁、取完数据立即释放，不存在跨 `await` 持锁。
//! - 在途表操作由 `watch::Sender<usize>` 计数，`settle_*` 通过 `wait_idle()` 等待归零。
//! - 同一地址的文档获取由每个 `DocumentEntry` 上的 `tokio::sync::Mutex` 串行化，避免一次
//!   事务里并发的 `doc()` 触发重复冷加载。
//!
//! # 注意事项
//!
//! - 事务封闭（`sealed`）后，读写方法返回 `Transaction has settled`；`Tx` trait 里没有错误
//!   通道的 `settle_submission` / `place_submission` 以同样的文案 panic。
//! - 表写之后再发起的表读会得到 `ReadAfterWrite` 错误；文档草稿不设置这个标志。
//! - 文档草稿的结算状态与本文件的 [`Change`] 共享：`prepare()` 之后只读，
//!   `abort()`/`consume()` 之后读取为空对象、写入 panic（细节见 `DocDraft` 的类型文档）。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use kiana_chord::context::{BACKGROUND_CONTEXT, ContextRef};
use kiana_chord::delta::Op;
use kiana_chord::delta::diff::diff_revisions;
use kiana_chord::delta::pending::{PendingOps, PendingTake};
use kiana_telemetry::TelemetryCarrier;
use serde_json::Value;

use crate::harness::types::{AnyTask, AnyTaskDefinition};
use crate::session::forks::prepare_fork_document_copies;
use crate::types::documents::{
    DocDefinition, address_id, check_record_scope, check_record_version, document_create,
    materialize_document_value,
};
use crate::types::{
    BoxError, CheckpointInfo, ConversationId, ConversationOwnership, ConversationParent,
    ConversationQuery, ConversationRecord, CreateConversationOptions, Cursor, DRAFT_ABORTED,
    DRAFT_CONSUMED, DRAFT_OPEN, DRAFT_PREPARED, DocDraft, DocTarget, DocumentAddress,
    DocumentBaseContent, DocumentCommitChange, DocumentContent, DocumentContentKind,
    DocumentCopySource, DocumentCreate, DocumentFork, DocumentHistory, DocumentId, DocumentPoint,
    DocumentQuery, DocumentRecord, DocumentScope, EntryDraft, EntryHead, EntryId, EntryQuery,
    EntryRecord, IdKind, JsonObject, JsonValue, Ops, Page, ReadAfterWrite, Seq, Storage,
    StorageWrite, SubmissionCreate, SubmissionId, SubmissionKind, SubmissionRecord,
    SubmissionSettlement, SubmissionStatus, TaskId, TaskOptions, TaskOwnership, TaskQuery,
    TaskRecord, TaskState, TaskStatus, TaskTrace, Tx, id_from_number,
};

/// 内部扫描分页大小。
const INTERNAL_SCAN_PAGE_SIZE: usize = 256;

/// 事务结算后的错误（TS：`#assertOpen()` 的消息）。
///
/// 回调结束后 Session 会封闭事务；此后仍持有 `&Transaction` 的迟到读写调用都会得到这个
/// 错误（`Display` 为 `Transaction has settled`）。它实现 `Error`，通常以 `BoxError` 形式
/// 返回；`Tx` trait 中没有错误通道的方法则直接 panic 同一文案。
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("Transaction has settled")]
pub struct TransactionSettled;

// 取锁时忽略 poison：即使某个持锁路径 panic 过，也继续使用内部值，否则后续每个调用者都会
// 在 poison 错误上 panic。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn object_value(value: &JsonObject) -> Value {
    Value::Object(value.clone().into_iter().collect())
}

fn io_error(message: impl Into<String>) -> BoxError {
    std::io::Error::other(message.into()).into()
}

// ---------------------------------------------------------------------------
// Tracker / Change / Prepared / LoadedDocument
// ---------------------------------------------------------------------------

/// 已提交文档值的可变持有者（TS: `Tracker<JsonObject>`）。
///
/// 缓存里的 [`LoadedDocument`] 与所有 [`Change`] 都以它保存的值为“已提交基线”：
/// `begin_change()` 克隆当前值作为 diff 基线，一次变化只有被采纳后才会写回这里，因此
/// 未结算的草稿不会污染缓存。`value()` 返回深拷贝，调用方修改副本不影响 tracker。
#[derive(Debug)]
pub struct Tracker {
    value: Mutex<JsonObject>,
}

impl Tracker {
    /// 从值创建。
    pub fn new(value: JsonObject) -> Self {
        Tracker {
            value: Mutex::new(value),
        }
    }

    /// 当前已提交值（拷贝）。
    pub fn value(&self) -> JsonObject {
        lock(&self.value).clone()
    }

    fn adopt(&self, value: JsonObject) {
        *lock(&self.value) = value;
    }

    /// 开始一次可变事务（TS: `tracker.beginChange()`）。
    pub fn begin_change(&self) -> Change {
        Change::new(self.value())
    }
}

/// 一次进行中的文档变化（TS: `Change<JsonObject>`）。
///
/// TS 的 `Change.state` 返回指向同一个 overlay 的可变代理：赋值立即记录，`prepare()` 读出最终值
/// 与操作日志。Rust 没有属性代理，因此这里让变化的最终值活在一个共享单元里：[`DocDraft`] 通过
/// 它读写，`prepare()` 也从它读取。同一变化的每次获取因此看到同一份活值（与 TS 的同一代理
/// 等价），但**不会**获得 TS 的操作日志——`prepare()` 仍是值与载入值的快照 diff。
///
/// 结算状态与 `DocDraft` 共享，转换是单向的：
/// - `open` → `prepared`：`prepare()` 读取最终值、计算 ops，并把草稿标成只读；
/// - `open`/`prepared` → `aborted`：`abort()`（提交失败、放弃或装配出错）；
/// - `open`/`prepared` → `consumed`：`consume()`，或结算采纳路径（`adopt()` /
///   `adopt_held()`）直接写状态（已持久化/已在内存采纳）。
///
/// 一旦进入 `aborted`/`consumed`，草稿的读取返回空对象、写入 panic；因此草稿不能逃逸出
/// 它所属的事务结算周期后继续使用。
#[derive(Debug)]
pub struct Change {
    base: JsonObject,
    shared: Arc<Mutex<JsonObject>>,
    status: Arc<AtomicU8>,
}

impl Change {
    fn new(base: JsonObject) -> Self {
        Change {
            shared: Arc::new(Mutex::new(base.clone())),
            status: Arc::new(AtomicU8::new(DRAFT_OPEN)),
            base,
        }
    }

    /// 与事务共享最终值的草稿；每次获取返回同一活值上的句柄（TS 返回同一 `Draft`）。
    pub fn state(&self) -> DocDraft {
        DocDraft::transaction(Arc::clone(&self.shared), Arc::clone(&self.status))
    }

    /// 结算时的准备结果。
    ///
    /// 与 TS 一样，准备时草稿变为只读（仍可读）；采纳或放弃后草稿被吊销。
    ///
    /// `ops` 是 `base` 与当前活值用 `diff_revisions` 计算出的批次：结构上先改回原值的变化
    /// （例如先 `shift` 再 `unshift`）会得到空批次。`value` 是最终值快照。两者都可能为空；
    /// 整个计算只读共享值、不修改状态以外的任何东西。
    pub fn prepare(&self) -> Prepared {
        let value = lock(&self.shared).clone();
        let ops = diff_revisions(&object_value(&self.base), &object_value(&value));
        self.status.store(DRAFT_PREPARED, Ordering::SeqCst);
        Prepared { ops, value }
    }

    /// 放弃这次变化（TS: `change.abort()`）。
    pub fn abort(&self) {
        self.status.store(DRAFT_ABORTED, Ordering::SeqCst);
    }

    /// 采纳这次变化（TS: `tracker.adopt(prepared)` 吊销草稿）。
    pub fn consume(&self) {
        self.status.store(DRAFT_CONSUMED, Ordering::SeqCst);
    }

    /// 变化草稿的结算状态句柄（供计划在采纳时吊销）。
    pub fn status_handle(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.status)
    }
}

/// 已准备的文档变化（TS: `Prepared<JsonObject>`）。
///
/// 它是 `base` 快照与最终值之间的差异。写 Storage 时 `ops` 用于 delta、`value` 用于 base
/// （迁移、新建化身与 `checkpoint_when` 选中的 checkpoint）；发布时已加载文档的
/// `DocumentCommitChange::Document::ops` 取 `ops`，新建化身发布空 ops。结算阶段每个有变化
/// 的文档各产生一份。
#[derive(Debug, Clone)]
pub struct Prepared {
    /// 相对载入值的操作批次。
    pub ops: Vec<Op>,
    /// 最终值。
    pub value: JsonObject,
}

/// 缓存化身的可变状态（`storedVersion` 与 `deltasSinceBase`）。
///
/// 这两个值决定下一次写入是 base 还是 delta，以及 `checkpoint_when` 的输入；它们随
/// [`LoadedDocument`] 的所有克隆共享，Storage 采纳后由 `adopt()` 更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedState {
    /// 已持久化的定义版本。
    pub stored_version: i64,
    /// 最新 base 之后已存的 delta 数。
    pub deltas_since_base: u64,
}

/// Session tracker 缓存里的一个已提交文档化身（TS: `LoadedDocument`）。
///
/// 不变量：`tracker` 里的值符合 `value_version` 所表示的定义版本；`record` 是该化身的持久
/// 身份，地址退役后重新获取会得到新的 `DocumentId`。`state` 记录持久层进度，由缓存与事务
/// 计划共享，所以采纳后的版本/计数对所有克隆立即可见。
#[derive(Clone, Debug)]
pub struct LoadedDocument {
    /// 稳定地址身份。
    pub address_id: String,
    /// 化身记录。
    pub record: DocumentRecord,
    /// tracker 值形状对应的定义版本。
    pub value_version: i64,
    /// 当前值的 tracker。
    pub tracker: Arc<Tracker>,
    /// 持久化版本与 delta 计数（共享可变）。
    pub state: Arc<Mutex<LoadedState>>,
}

impl LoadedDocument {
    /// 构造。
    ///
    /// `stored_version` 是持久记录的定义版本（低于 `value_version` 时下一次写入会以 base
    /// 重写），`deltas_since_base` 是当前 base 之后累计的 delta 数。
    pub fn new(
        address_id: impl Into<String>,
        record: DocumentRecord,
        stored_version: i64,
        value_version: i64,
        deltas_since_base: u64,
        tracker: Arc<Tracker>,
    ) -> Self {
        LoadedDocument {
            address_id: address_id.into(),
            record,
            value_version,
            tracker,
            state: Arc::new(Mutex::new(LoadedState {
                stored_version,
                deltas_since_base,
            })),
        }
    }

    /// 已持久化的定义版本。
    pub fn stored_version(&self) -> i64 {
        lock(&self.state).stored_version
    }

    /// 最新 base 之后已存的 delta 数。
    pub fn deltas_since_base(&self) -> u64 {
        lock(&self.state).deltas_since_base
    }

    fn raise_stored_version(&self, version: i64) {
        let mut state = lock(&self.state);
        if state.stored_version < version {
            state.stored_version = version;
        }
    }

    fn note_content(&self, content: &DocumentContent) {
        let mut state = lock(&self.state);
        if content.kind == DocumentContentKind::Base {
            state.deltas_since_base = 0;
        } else {
            state.deltas_since_base += 1;
        }
    }
}

/// 一次延迟提交在内存里持有的变化（TS: `HeldDocument`）。
///
/// `pending` 是自上次存储以来由 `store_held()` 折叠的操作；任何一次持有提交选择 base 后
/// `base` 置位且不会清除，下一次存储会写完整值。持有期间 Session 用缓存 pin 保证 `loaded`
/// 不被驱逐（pin 的释放句柄放在 [`Hold`] 上）。
#[derive(Debug)]
pub struct HeldDocument {
    /// 被持有的化身。
    pub loaded: LoadedDocument,
    /// 折叠中的待处理操作。
    pub pending: Mutex<PendingOps>,
    /// 某个被持有的提交选择了 base：存储时写完整值。
    pub base: AtomicBool,
}

impl HeldDocument {
    /// 构造。
    pub fn new(loaded: LoadedDocument) -> Self {
        HeldDocument {
            loaded,
            pending: Mutex::new(PendingOps::new()),
            base: AtomicBool::new(false),
        }
    }
}

/// Session 持有的一个延迟提交化身：文档本身、最早 deadline 与缓存 pin。
///
/// `deadline` 是所有并入该 hold 的延迟提交里最早的截止时间（Session 时钟，与
/// `within_ms` 同单位）；`unpin` 在 Session 释放这个 hold 时取出并执行一次，置 `None`
/// 表示缓存 pin 已解除。Session 以 `Arc<Mutex<Hold>>` 持有它，`held.pending`/`held.base`
/// 由同一把锁保护，见 [`HeldDocument`]。
pub struct Hold {
    /// 被持有的文档变化。
    pub held: HeldDocument,
    /// 最早 deadline（Session 时钟）。
    pub deadline: f64,
    /// 解除缓存 pin；`None` 表示已解除。
    pub unpin: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl std::fmt::Debug for Hold {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Hold")
            .field("loaded", &self.held.loaded.address_id)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// `storeHeld()` 的结果。
///
/// `content` 是要提交给 Storage 的 base/delta；`ops` 是给读者重放用的操作——pending 溢出时
/// 是一条根替换 `Op::Replace`，否则是折叠后的批次。两者由同一份 pending 生成，调用后
/// [`HeldDocument::pending`] 为空。
pub struct StoredHeld {
    /// 要写入的内容。
    pub content: DocumentContent,
    /// 读者从已存状态到达当前值所需应用的操作。
    pub ops: Vec<Op>,
}

/// 取出一份 hold 的变化，与本提交自己的 ops 折叠成一次存储（TS: `storeHeld`）。
///
/// 步骤：
/// 1. 先把本次提交的 `ops` 并入 `hold.pending`；
/// 2. 再整体 `take()`：`PendingTake::Replace`（折叠溢出）表示无法继续合并，用 `value` 写
///    完整 base，并发布一条根替换；
/// 3. 否则若 `hold.base` 已被任一持有提交置位，也写 base（ops 用取出的批次）；
/// 4. 其余情况写 delta，`version` 由调用方给出（`take_held` 传 `loaded.value_version`）。
///
/// 调用后 pending 回到空状态，队列可继续使用。`value` 必须是折叠后 pending 对应的最终值，
/// 否则 Replace 分支写出的 base 会与 ops 不一致。
pub fn store_held(hold: &HeldDocument, ops: &[Op], value: &JsonObject, version: i64) -> StoredHeld {
    let mut pending = lock(&hold.pending);
    pending.push(ops);
    match pending.take() {
        PendingTake::Replace => StoredHeld {
            content: DocumentContent::base(version, value.clone()),
            ops: vec![Op::Replace(object_value(value))],
        },
        PendingTake::Ops(taken) => {
            if hold.base.load(Ordering::SeqCst) {
                StoredHeld {
                    content: DocumentContent::base(version, value.clone()),
                    ops: taken,
                }
            } else {
                StoredHeld {
                    content: DocumentContent::delta(version, taken.clone()),
                    ops: taken,
                }
            }
        }
    }
}

/// 采纳发布的文档变化（TS: `AdoptedDocuments`）。
///
/// `observed` 是本次提交自己观察到、并发布给内部监听器的变化；`stored` 是 Storage 实际接受
/// 的变化。没有并入手持变化时两者相等；`take_held()` 折叠了别的提交留下的待存 ops 时，
/// `stored` 会包含本提交没改过、只由折叠产生的发布项，且这些项的 `ops` 是折叠后的批次。
/// Session 用 `stored` 决定缓存 touch，用 `observed` 通知内部监听器。
#[derive(Debug, Clone, Default)]
pub struct AdoptedDocuments {
    /// 本提交自己的变化，按内存采纳的顺序。
    pub observed: Vec<DocumentCommitChange>,
    /// Storage 收到并发布的变化（无 held 时与 `observed` 相同）。
    pub stored: Vec<DocumentCommitChange>,
}

// ---------------------------------------------------------------------------
// Task creation
// ---------------------------------------------------------------------------

/// 擦除后的任务 `initial` 回调（registry 用它填充 `AnyTaskDefinition.initial`）。
///
/// `AnyTaskDefinition` 用 `Arc<dyn Any + Send + Sync>` 存放各处理器；这里存的仍是强类型的
/// `initial` 闭包，[`call_task_initial`] 负责 downcast。放入其他类型不会 panic，而是让任务
/// 创建返回错误。
pub struct ErasedTaskInitial(pub Arc<dyn Fn(JsonValue) -> JsonValue + Send + Sync>);

impl std::fmt::Debug for ErasedTaskInitial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ErasedTaskInitial")
    }
}

/// 调用一个擦除任务定义的 `initial`。
///
/// 返回的 JSON 会成为新任务的 `TaskState::Pending { checkpoint }`。调用方（`create_task_inner`）
/// 通常传入 `input.clone()`，因为同一个输入还要原样存进任务记录。
///
/// 契约：`AnyTaskDefinition.initial` 必须存放 [`ErasedTaskInitial`]；否则返回错误。
pub fn call_task_initial(
    definition: &AnyTaskDefinition,
    input: JsonValue,
) -> Result<JsonValue, BoxError> {
    match definition.initial.downcast_ref::<ErasedTaskInitial>() {
        Some(initial) => Ok((initial.0)(input)),
        None => Err(io_error(
            "task definition initial is not a kiana-durable ErasedTaskInitial",
        )),
    }
}

// ---------------------------------------------------------------------------
// Transaction host / scope / options
// ---------------------------------------------------------------------------

/// 事务在持有变更线时使用的 Session 服务（TS: `TransactionHost`）。
///
/// 由 Session 实现（`SessionHost`）并交给 [`Transaction`]，把事务与 Session 的缓存、失败
/// 保护和创建钩子隔开。除 `load`/`conversation_created` 外的方法都是同步的，可能在回调或
/// 结算路径（包括 Storage 的同步 `adopt` 回调）里被调用，因此不能阻塞或等待。
#[async_trait]
pub trait TransactionHost: Send + Sync {
    /// 受失败保护的 Storage。
    ///
    /// Storage 的错误通常是最终错误，会让 Session 失败（只影响单次读的例外见 `Storage`
    /// trait 的文档）；事务只负责把错误向上传播。
    fn storage(&self) -> &dyn Storage;
    /// 任务生命周期的墙钟（TS: `#host.now`；提供者抛错时让提交失败）。
    fn now(&self) -> Result<f64, BoxError>;
    /// 取缓存里的当前化身，不加载。
    ///
    /// `retire_doc` 用它查本事务没获取过的文档；未命中时调用方再查 Storage。
    fn cached(&self, address_id: &str) -> Option<LoadedDocument>;
    /// 取缓存化身，必要时冷加载并迁移。
    ///
    /// `None` 表示该地址还没有持久记录，调用方会按“创建”处理。缓存命中但
    /// `value_version` 与 `definition.version` 不同时，实现负责先存储持有的变化、再重新
    /// 加载并迁移。会话关闭后实现可能返回错误。
    async fn load(
        &self,
        definition: Arc<DocDefinition>,
        address_id: &str,
        address: DocumentAddress,
        context: ContextRef,
    ) -> Result<Option<LoadedDocument>, BoxError>;
    /// 安装新提交的化身（本事务创建、且已被 Storage 接受）。
    fn install(&self, document: LoadedDocument);
    /// 移除已退役的化身（仍是该地址占用者时）。
    ///
    /// 缓存里该地址若已被更新的 `DocumentId` 占用，则保持不变。
    fn evict(&self, address_id: &str, record_id: DocumentId);
    /// 新建或 fork 会话时追加属于每个会话的写入。
    ///
    /// 在会话记录暂存之后、提交结算之前调用；harness 的钩子会用同一个事务创建内建文档。
    /// 钩子返回错误时 `stage_conversation` 会失败，harness 的创建路径据此放弃整次创建。
    async fn conversation_created(
        &self,
        tx: &Transaction,
        record: &ConversationRecord,
    ) -> Result<(), BoxError>;
}

/// 一次提交绑定的默认值（TS: `TransactionScope`）。
///
/// 由 Session 在 `commit` 时给出；它只提供默认值，不限制方法参数里显式指定的其他会话/任务。
/// `conversation_id` 是 `createTask()` 在既没有 owner 也没有显式选项时使用的会话；
/// `task_id` 会写进 `appendEntry()` 的 `byTaskId`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransactionScope {
    /// `tx.createTask()` 的默认会话。
    pub conversation_id: Option<ConversationId>,
    /// 追加条目时写入 `byTaskId` 的任务。
    pub task_id: Option<TaskId>,
}

/// 内部 run 创建的选项：`TaskOptions` 加上要解析的提交载体（TS: `TaskCreationOptions`）。
///
/// `submission_ids` 只由内部 run 创建任务时设置：`create_task_inner` 会把第一个提交的 trace
/// 作为任务 trace（缺失则为 root），其余提交的非空 trace 追加为 links，并覆盖这里已有的
/// `trace`/`links`。公开构造 [`TaskCreationOptions::new`] 总是把它设为 `None`。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskCreationOptions {
    /// `trace`。
    pub trace: Option<TaskTrace>,
    /// 不可变创建期链接。
    pub links: Option<Vec<TelemetryCarrier>>,
    /// 拥有权（必填）。
    pub ownership: TaskOwnership,
    /// 默认会话。
    pub conversation_id: Option<ConversationId>,
    /// background。
    pub background: Option<bool>,
    /// abandonOnRestart。
    pub abandon_on_restart: Option<bool>,
    /// 内部 run 创建时解析的提交载体。
    pub submission_ids: Option<Vec<SubmissionId>>,
}

impl TaskCreationOptions {
    /// 由公开的 [`TaskOptions`] 构造。
    pub fn new(options: TaskOptions) -> Self {
        TaskCreationOptions {
            trace: options.trace,
            links: options.links,
            ownership: options.ownership,
            conversation_id: options.conversation_id,
            background: options.background,
            abandon_on_restart: options.abandon_on_restart,
            submission_ids: None,
        }
    }

    /// 去掉内部字段后的公开形状。
    pub fn task_options(&self) -> TaskOptions {
        TaskOptions {
            trace: self.trace.clone(),
            links: self.links.clone(),
            ownership: self.ownership.clone(),
            conversation_id: self.conversation_id,
            background: self.background,
            abandon_on_restart: self.abandon_on_restart,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// 一次提交里落定的提交变化（TS: `SubmissionChange`）。
///
/// 只是排队记录，真正的状态转换在 `assemble()` 调用 `apply_submission_change` 时发生。
#[derive(Debug, Clone, PartialEq)]
enum SubmissionChange {
    Settlement(SubmissionSettlement),
    Placed { entry: EntryId },
}

/// `applySubmissionChange()`。
///
/// 状态机（`done`/`unanswered` 是吸收态，对它们的任何变化都直接返回原记录）：
/// - `queued` --Placed--> `input` 变 `placed`，其他类型（被动写入）变 `done`；非 `queued`
///   记录收到 Placed 会报错；
/// - `placed` --Settlement::Done--> `done`；其他状态收到 Done 会报错；
/// - 任意非终态 --Settlement::Unanswered--> `unanswered`。
///
/// 每次转换都用 `merged_key_order` 保持 TS 展开赋值的键序（新键追加到末尾）。
fn apply_submission_change(
    current: &SubmissionRecord,
    change: &SubmissionChange,
) -> Result<SubmissionRecord, BoxError> {
    if matches!(
        current.status,
        SubmissionStatus::Done | SubmissionStatus::Unanswered
    ) {
        return Ok(current.clone());
    }
    if let SubmissionChange::Placed { entry } = change {
        if current.status != SubmissionStatus::Queued {
            return Err(io_error(format!("Submission {} is not queued", current.id)));
        }
        let mut next = current.clone();
        next.status = if current.kind == SubmissionKind::Input {
            SubmissionStatus::Placed
        } else {
            SubmissionStatus::Done
        };
        next.entry = Some(*entry);
        // TS: `{ ...current, status, entry }`——`entry` 追加到既有键之后（queued 记录的 `id` 之后）。
        next.key_order = current.merged_key_order(&["status", "entry"]);
        return Ok(next);
    }
    let SubmissionChange::Settlement(settlement) = change else {
        unreachable!("submission change is a settlement");
    };
    if matches!(settlement, SubmissionSettlement::Done { .. })
        && current.status != SubmissionStatus::Placed
    {
        return Err(io_error(format!(
            "Submission {} is not a placed input",
            current.id
        )));
    }
    let mut next = current.clone();
    match settlement {
        SubmissionSettlement::Done { answer } => {
            next.status = SubmissionStatus::Done;
            next.answer = Some(*answer);
            next.key_order = current.merged_key_order(&["status", "answer"]);
        }
        SubmissionSettlement::Unanswered { reason, detail } => {
            next.status = SubmissionStatus::Unanswered;
            next.reason = Some(reason.clone());
            next.detail = detail.clone();
            next.key_order = current.merged_key_order(&["status", "reason", "detail"]);
        }
    }
    Ok(next)
}

/// 本事务触碰的一个任务的已提交读取与候选写入。
#[derive(Default)]
struct TransactionTask {
    /// TS: `committedRead`——缓存的是**在途读取**（promise），不是结果：并发读共享同一次后端读。
    committed_read: Option<Arc<tokio::sync::OnceCell<Option<TaskRecord>>>>,
    write: Option<TaskWrite>,
    publication_conversation_id: Option<ConversationId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskWriteKind {
    Create,
    Replace,
}

struct TaskWrite {
    kind: TaskWriteKind,
    record: TaskRecord,
}

/// 暂存文档化身的来源。
enum DocumentTarget {
    // 从 Storage/缓存载入的已提交化身。
    Loaded(LoadedDocument),
    // 本事务新建的化身（记录 + 定义版本 + 值的 tracker）。
    Created {
        record: DocumentCreate,
        version: i64,
        tracker: Arc<Tracker>,
    },
    // fork 时登记的惰性 copy；提交时走 `document.copy`。若本事务 `doc()` 读过它，
    // 会被替换成 `Created` 并写完整值。
    ForkCopy {
        record: DocumentCreate,
        source: DocumentCopySource,
    },
    // 本事务没获取过、只按地址找到持久记录并退休的化身。
    RetireOnly {
        record: DocumentRecord,
    },
}

/// 获取模式：`SkipLoad` 用于同一事务里重建已退休的化身。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AcquireMode {
    Load,
    SkipLoad,
}

/// 本事务获取、创建或退休的一个文档化身。
// 一次事务里按地址维护的文档条目。`documents` 只增不改索引，`latest_document_by_address`
// 指向当前有效条目（退休后重建会压入新条目并更新映射）。`acquire` 串行化冷加载；
// `retire_on_commit` 只在本事务结算时生效。
struct DocumentEntry {
    address_id: String,
    address: DocumentAddress,
    definition: Option<Arc<DocDefinition>>,
    acquire: Arc<tokio::sync::Mutex<()>>,
    target: Option<DocumentTarget>,
    change: Option<Change>,
    prepared: Option<Prepared>,
    retire_on_commit: bool,
}

/// 一次提交为某个化身写入并发布的内容。
// 结算阶段由 DocumentEntry 生成的写入计划：持久记录、是否退休、要写的文档内容，以及
// adopt() 之后发布所需的信息（`conversation_id`、held 折叠结果）。
struct DocumentPlan {
    address_id: String,
    record: PlanRecord,
    retire: bool,
    content: Option<StorageWrite>,
    change: Option<PlanChange>,
    conversation_id: Option<ConversationId>,
    held: Option<PlanHeld>,
}

enum PlanRecord {
    Create(DocumentCreate),
    Existing(DocumentRecord),
}

impl PlanRecord {
    fn id(&self) -> DocumentId {
        match self {
            PlanRecord::Create(record) => record.id,
            PlanRecord::Existing(record) => record.id,
        }
    }

    fn scope(&self) -> DocumentScope {
        match self {
            PlanRecord::Create(record) => record.scope,
            PlanRecord::Existing(record) => record.scope,
        }
    }

    fn record_fork(&self) -> Option<DocumentFork> {
        match self {
            PlanRecord::Create(record) => record.fork,
            PlanRecord::Existing(record) => record.fork,
        }
    }

    fn is_committed(&self) -> bool {
        matches!(self, PlanRecord::Existing(_))
    }
}

struct PlanChange {
    tracker: Arc<Tracker>,
    prepared: Prepared,
    version: i64,
    loaded: Option<LoadedDocument>,
    definition: Option<Arc<DocDefinition>>,
    /// 草稿的结算状态；采纳时吊销（TS: `tracker.adopt()` 让代理失效）。
    status: Option<Arc<AtomicU8>>,
}

struct PlanHeld {
    ops: Vec<Op>,
    observed: bool,
}

// 事务的全部可变状态。锁只保护内存暂存，不保护 Storage；任何需要 await 的代码都必须在
// 独立代码块里取完快照后立即释放锁。`documents` 只追加不删除，`latest_document_by_address`
// 指向该地址当前有效的条目；`plans`/`writes` 在 assemble() 时由暂存状态重建。
struct TxInner {
    context: ContextRef,
    scope: TransactionScope,
    sealed: bool,
    has_table_write: bool,
    writes: Vec<StorageWrite>,
    created_conversation_ids: HashSet<ConversationId>,
    fork_source_conversation_ids: HashSet<ConversationId>,
    fork_source_document_ids: HashSet<DocumentId>,
    tasks_by_id: indexmap::IndexMap<TaskId, TransactionTask>,
    submissions: indexmap::IndexMap<SubmissionId, SubmissionRecord>,
    submission_changes: Vec<(SubmissionId, SubmissionChange)>,
    plans: Vec<DocumentPlan>,
    documents: Vec<DocumentEntry>,
    latest_document_by_address: HashMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Transaction
// ---------------------------------------------------------------------------

/// 一次 Session 提交回调的事务（TS: `Transaction`）。
///
/// 生命周期只有两段：回调运行期间可读写（`open`），之后 `settle_failure()` 或
/// `settle_success()` 把它封闭（`sealed`）。此后表/文档读写返回 [`TransactionSettled`]；
/// 没有错误通道的 `settle_submission` / `place_submission` 则以同一文案 panic（见文件顶部
/// 注意事项）。
/// 所有方法都以 `&self` 调用，内部状态由 `Mutex<TxInner>` 保护；但文档草稿本身按 TS 的
/// 单任务 overlay 假设使用，不应跨任务并发读写。
pub struct Transaction {
    host: Arc<dyn TransactionHost>,
    inner: Mutex<TxInner>,
    /// 在途操作数。用 `watch` 而不是 `AtomicUsize + Notify`：`Notify::notify_waiters()`
    /// 不保存许可，可能在"检查为 0"与"第一个 poll"之间丢失唤醒；`watch` 的版本比较不会。
    pending_operations: tokio::sync::watch::Sender<usize>,
}

impl Transaction {
    /// 新建事务。
    ///
    /// `context` 会用于本事务发起的所有 Storage 读；`settle_*` 结束后被替换为
    /// `BACKGROUND_CONTEXT`。`pending_operations` 的初值为 0。
    pub fn new(
        host: Arc<dyn TransactionHost>,
        context: ContextRef,
        scope: TransactionScope,
    ) -> Self {
        Transaction {
            host,
            inner: Mutex::new(TxInner {
                context,
                scope,
                sealed: false,
                has_table_write: false,
                writes: Vec::new(),
                created_conversation_ids: HashSet::new(),
                fork_source_conversation_ids: HashSet::new(),
                fork_source_document_ids: HashSet::new(),
                tasks_by_id: indexmap::IndexMap::new(),
                submissions: indexmap::IndexMap::new(),
                submission_changes: Vec::new(),
                plans: Vec::new(),
                documents: Vec::new(),
                latest_document_by_address: HashMap::new(),
            }),
            pending_operations: tokio::sync::watch::channel(0).0,
        }
    }

    fn pending_count(&self) -> usize {
        *self.pending_operations.borrow()
    }

    /// 当前提交上下文（结算后是 `BACKGROUND_CONTEXT`）。
    pub fn context(&self) -> ContextRef {
        Arc::clone(&lock(&self.inner).context)
    }

    /// 本事务绑定的作用域。
    pub fn scope(&self) -> TransactionScope {
        lock(&self.inner).scope
    }

    fn assert_open(inner: &TxInner) -> Result<(), BoxError> {
        if inner.sealed {
            return Err(Box::new(TransactionSettled));
        }
        Ok(())
    }

    fn assert_open_now(&self) -> Result<(), BoxError> {
        Self::assert_open(&lock(&self.inner))
    }

    /// 第一次表写之后的表读（TS: `ReadAfterWrite`）。
    ///
    /// `method` 只用于错误的 `method` 字段，取值与 TS 的读取方法名一致（如
    /// `scanConversations`）。
    fn assert_readable(inner: &TxInner, method: &str) -> Result<(), BoxError> {
        Self::assert_open(inner)?;
        if inner.has_table_write {
            return Err(Box::new(ReadAfterWrite::new(method)));
        }
        Ok(())
    }

    /// 把一个在途操作计入计数；guard 在 future 完成或被丢弃时递减。
    fn begin_operation(&self) -> PendingOperation<'_> {
        self.pending_operations.send_modify(|count| *count += 1);
        PendingOperation { transaction: self }
    }

    /// 表读的统一入口：先在锁内检查可读性（事务开放且尚无表写），再计入在途操作并等待后端。
    ///
    /// 检查发生在计数之前，因此被拒绝的读不会留下计数；真正的后端读取会一直计数到 future
    /// 结束（成功、失败或 drop 都会递减）。
    async fn read<T, F>(&self, method: &str, run: F) -> Result<T, BoxError>
    where
        F: Future<Output = Result<T, BoxError>>,
    {
        {
            let inner = lock(&self.inner);
            Self::assert_readable(&inner, method)?;
        }
        let _operation = self.begin_operation();
        run.await
    }

    /// 表写的统一入口：断言事务开放、置位 `has_table_write`，再计入在途操作并执行 `run`。
    ///
    /// 置位发生在暂存写入之前，因此同一事务里“先写后读”总是被拒绝；写失败也不会清除该标志。
    async fn write<T, F>(&self, run: F) -> Result<T, BoxError>
    where
        F: Future<Output = Result<T, BoxError>>,
    {
        {
            let mut inner = lock(&self.inner);
            Self::assert_open(&inner)?;
            inner.has_table_write = true;
        }
        let _operation = self.begin_operation();
        run.await
    }

    /// 等待在途表操作归零。
    ///
    /// 每次循环先读取当前值；`watch` 的版本比较保证“检查为 0”之后到下一次 `changed()` 之间
    /// 的唤醒不会丢失（这正是用 `watch` 而不是 `Notify` 的原因）。计数降到 0 或发送端消失
    /// 时返回。
    async fn wait_idle(&self) {
        let mut receiver = self.pending_operations.subscribe();
        loop {
            if *receiver.borrow_and_update() == 0 {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    // ─── Table reads ────────────────────────────────────────────────────────

    async fn conversation_impl(
        &self,
        id: ConversationId,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        let context = self.context();
        self.read("conversation", async {
            self.host.storage().conversation(id, &*context).await
        })
        .await
    }

    async fn entry_impl(&self, id: EntryId) -> Result<Option<EntryRecord>, BoxError> {
        let context = self.context();
        self.read("entry", async {
            Ok(self
                .host
                .storage()
                .entry(id, &*context)
                .await?
                .map(|stored| stored.entry))
        })
        .await
    }

    async fn task_impl(&self, id: TaskId) -> Result<Option<TaskRecord>, BoxError> {
        self.read("task", async { self.committed_task(id).await })
            .await
    }

    async fn scan_conversations_impl(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        let context = self.context();
        self.read("scanConversations", async {
            self.host
                .storage()
                .scan_conversations(query, limit, cursor, &*context)
                .await
        })
        .await
    }

    async fn scan_entries_impl(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<EntryRecord>, BoxError> {
        let context = self.context();
        self.read("scanEntries", async {
            self.host
                .storage()
                .scan_entries(query, limit, cursor, &*context)
                .await
        })
        .await
    }

    async fn latest_head_marker_impl(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<EntryRecord>, BoxError> {
        let context = self.context();
        self.read("latestHeadMarker", async {
            self.host
                .storage()
                .find_latest_head_marker(conversation_id, None, &*context)
                .await
        })
        .await
    }

    async fn scan_tasks_impl(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<TaskRecord>, BoxError> {
        let context = self.context();
        self.read("scanTasks", async {
            self.host
                .storage()
                .scan_tasks(query, limit, cursor, &*context)
                .await
        })
        .await
    }

    async fn submission_by_request_impl(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        let context = self.context();
        self.read("submissionByRequest", async {
            self.host
                .storage()
                .submission_by_request(conversation_id, request_id, &*context)
                .await
        })
        .await
    }

    /// 内部：已提交的提交记录。
    ///
    /// 直接读 Storage，不使用本事务暂存（`create_submission` 刚创建的记录也查不到）。
    pub async fn submission(&self, id: SubmissionId) -> Result<Option<SubmissionRecord>, BoxError> {
        let context = self.context();
        self.read("submission", async {
            self.host.storage().submission(id, &*context).await
        })
        .await
    }

    // ─── Table writes ───────────────────────────────────────────────────────

    /// 创建会话。
    ///
    /// 记录只暂存到事务里；`id` 由 Storage 铸号，`ownership == Task` 时要求 owner 任务在
    /// 事务候选或 Storage 中存在（否则返回错误）。
    pub async fn create_conversation(
        &self,
        options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError> {
        let ownership = options.ownership.clone();
        self.write(self.stage_conversation(None, ownership, None))
            .await
    }

    /// 内部：以保留的根身份引导。
    ///
    /// 使用 `ROOT_CONVERSATION_ID` 且 `Ownerless`，不铸号。
    pub async fn create_root_conversation(&self) -> Result<ConversationRecord, BoxError> {
        self.write(self.stage_conversation(
            None,
            ConversationOwnership::Ownerless,
            Some(crate::types::ROOT_CONVERSATION_ID),
        ))
        .await
    }

    /// 在某个可见条目处 fork 一个会话。
    ///
    /// 除父链接外，还会把 `prepare_fork_document_copies` 选出的文档登记成惰性 copy 目标：
    /// 它先取 `at` 条目所在会话在 `entry.commitSeq` 的 `asOf` 文档，再取父会话当前的
    /// `current` 文档。提交时没被读取的 copy 走 `document.copy`，被读取的会在本事务里物化
    /// 成完整 create。
    pub async fn fork_conversation(
        &self,
        parent_conversation_id: ConversationId,
        at: EntryId,
        options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError> {
        let ownership = options.ownership.clone();
        self.write(self.stage_conversation(
            Some(ConversationParent {
                conversation_id: parent_conversation_id,
                at,
            }),
            ownership,
            None,
        ))
        .await
    }

    async fn stage_conversation(
        &self,
        parent: Option<ConversationParent>,
        ownership: ConversationOwnership,
        reserved_id: Option<ConversationId>,
    ) -> Result<ConversationRecord, BoxError> {
        // 阶段 1：确定 ID（显式保留或铸号）与 owner。owner 任务必须已存在；记录里保存
        // 该任务所在的会话 ID 与任务 ID，供结算时校验 owner 仍活跃。
        let owner_task_id = match &ownership {
            ConversationOwnership::Task { task_id } => Some(*task_id),
            ConversationOwnership::Ownerless => None,
        };
        let id = match reserved_id {
            Some(id) => id,
            None => {
                let minted = self.host.storage().mint_id(IdKind::Conversation).await?;
                id_from_number::<crate::types::ConversationBrand>(minted)
            }
        };
        self.assert_open_now()?;
        let mut owner = None;
        if let Some(owner_task_id) = owner_task_id {
            let task = self.current_task(owner_task_id).await?;
            self.assert_open_now()?;
            let task = task.ok_or_else(|| {
                io_error(format!(
                    "Conversation owner task {owner_task_id} does not exist"
                ))
            })?;
            owner = Some(crate::types::ConversationOwner {
                conversation_id: task.conversation_id,
                task_id: owner_task_id,
            });
        }
        let record = ConversationRecord { id, parent, owner };
        // 阶段 2：fork 时选出要复制的文档；这里只登记来源与新记录，不读取源值。
        let copies = match &parent {
            None => Vec::new(),
            Some(parent) => {
                let context = self.context();
                prepare_fork_document_copies(
                    self.host.storage(),
                    parent.conversation_id,
                    parent.at,
                    id,
                    &*context,
                )
                .await?
            }
        };
        self.assert_open_now()?;
        // 阶段 3：把 copy 目标与 fork 来源登记到暂存区，写入会话记录，然后调用创建钩子。
        // 钩子用同一个事务追加内建文档，能看到已暂存的会话记录；它返回错误时错误会沿
        // `stage_conversation` 传给调用方（harness 的调用链因此放弃本次创建）。
        {
            let mut inner = lock(&self.inner);
            for copy in copies {
                inner.fork_source_document_ids.insert(copy.source.id);
                let address = DocumentAddress {
                    kind: copy.record.kind.clone(),
                    scope: copy.record.scope,
                    key: copy.record.key.clone(),
                };
                let address_id = address_id(&address);
                let entry = DocumentEntry {
                    address_id: address_id.clone(),
                    address,
                    definition: None,
                    acquire: Arc::new(tokio::sync::Mutex::new(())),
                    target: Some(DocumentTarget::ForkCopy {
                        record: copy.record,
                        source: copy.source,
                    }),
                    change: None,
                    prepared: None,
                    retire_on_commit: false,
                };
                let index = inner.documents.len();
                inner.documents.push(entry);
                inner.latest_document_by_address.insert(address_id, index);
            }
            if let Some(parent) = &record.parent {
                inner
                    .fork_source_conversation_ids
                    .insert(parent.conversation_id);
            }
            inner.created_conversation_ids.insert(id);
            inner.writes.push(StorageWrite::Conversation {
                value: record.clone(),
            });
        }
        self.host.conversation_created(self, &record).await?;
        self.assert_open_now()?;
        Ok(record)
    }

    /// 追加一个条目。
    ///
    /// 要求会话存在（本事务刚创建的也算）；`byTaskId` 取事务作用域里的 `task_id`。条目 ID
    /// 由 Storage 铸号；`head == EntryHead::SelfHead` 时写入新 ID 自引用。记录只暂存，
    /// 结算时才写入 Storage。
    pub async fn append_entry(
        &self,
        conversation_id: ConversationId,
        value: EntryDraft,
    ) -> Result<EntryRecord, BoxError> {
        let task_id = lock(&self.inner).scope.task_id;
        self.write(async {
            self.require_conversation(conversation_id).await?;
            self.assert_open_now()?;
            let minted = self.host.storage().mint_id(IdKind::Entry).await?;
            self.assert_open_now()?;
            let id: EntryId = id_from_number(minted);
            let head = match value.head {
                None => None,
                Some(EntryHead::SelfHead) => Some(id),
                Some(EntryHead::Id(head)) => Some(head),
            };
            // TS: `copyJson({ ...rest, id, conversationId, head, byTaskId })`；`rest` 是草稿去掉
            // `head` 的键（草稿字面量序），`id` 等追加在后。
            let mut order = value.key_sequence();
            order.retain(|key| key != "head");
            if !order.iter().any(|key| key == "kind") {
                // 带类型 token 的路径：`{ ...third, kind }`。
                order.push("kind".to_owned());
            }
            order.push("id".to_owned());
            order.push("conversationId".to_owned());
            order.push("head".to_owned());
            order.push("byTaskId".to_owned());
            let record = EntryRecord {
                model: value.model,
                data: value.data,
                edits: value.edits,
                kind: value.kind,
                id,
                conversation_id,
                head,
                by_task_id: task_id,
                key_order: Some(order),
            };
            lock(&self.inner).writes.push(StorageWrite::Entry {
                value: record.clone(),
            });
            Ok(record)
        })
        .await
    }

    /// 创建一个任务（公开 `TaskOptions` 形态）。
    ///
    /// 这是 `Tx` trait 同名方法的实现：会话按“owner 任务 > `options.conversation_id` >
    /// 事务作用域”的顺序解析，任务以 `Pending { checkpoint }` 暂存，checkpoint 由定义里的
    /// `initial` 计算。
    pub async fn create_task(
        &self,
        task: &AnyTask,
        input: JsonValue,
        options: TaskOptions,
    ) -> Result<TaskId, BoxError> {
        self.create_task_with(task, input, TaskCreationOptions::new(options))
            .await
    }

    /// 创建一个任务（内部形态，可解析提交载体）。
    ///
    /// `options.submission_ids` 存在时，先按 ID 取提交记录（先查本事务暂存，再查 Storage），
    /// 第一个提交的 trace 成为任务 trace（无 trace 则 root），其余提交的非空 trace 追加为
    /// links；这些字段会覆盖调用方传入的 `trace`/`links`。
    pub async fn create_task_with(
        &self,
        task: &AnyTask,
        input: JsonValue,
        options: TaskCreationOptions,
    ) -> Result<TaskId, BoxError> {
        self.write(self.create_task_inner(task, input, options))
            .await
    }

    async fn create_task_inner(
        &self,
        task: &AnyTask,
        input: JsonValue,
        mut options: TaskCreationOptions,
    ) -> Result<TaskId, BoxError> {
        // 阶段 1：内部 run 的提交载体解析。第一个提交决定 trace（缺失即 root），后续提交
        // 各贡献一条 link；本事务暂存的提交优先于 Storage。
        if let Some(submission_ids) = options.submission_ids.clone() {
            let mut trace: Option<TaskTrace> = None;
            let mut links: Option<Vec<TelemetryCarrier>> = None;
            for (index, id) in submission_ids.iter().enumerate() {
                let submission = {
                    let inner = lock(&self.inner);
                    inner.submissions.get(id).cloned()
                };
                let submission = match submission {
                    Some(submission) => Some(submission),
                    None => {
                        let context = self.context();
                        self.host.storage().submission(*id, &*context).await?
                    }
                };
                if index == 0 {
                    trace = Some(match submission.and_then(|record| record.trace) {
                        Some(carrier) => TaskTrace::Carrier(carrier),
                        None => TaskTrace::Root,
                    });
                } else if let Some(carrier) = submission.and_then(|record| record.trace) {
                    links.get_or_insert_with(Vec::new).push(carrier);
                }
            }
            options.trace = trace;
            options.links = links;
        }
        let ownership = options.ownership.clone();
        let mut owner: Option<TaskRecord> = None;
        // 阶段 2：owner 任务必须存在；子任务不能是 background，且必须住在 owner 的会话里
        // （显式传入不同会话是错误，而不是被忽略）。
        if let TaskOwnership::Task { task_id } = &ownership {
            let current = self.current_task(*task_id).await?;
            self.assert_open_now()?;
            let current =
                current.ok_or_else(|| io_error(format!("Task owner {task_id} does not exist")))?;
            if options.background == Some(true) {
                return Err(Box::new(TransactionTypeError::new(
                    "A child task cannot be background",
                )));
            }
            if let Some(conversation_id) = options.conversation_id
                && conversation_id != current.conversation_id
            {
                return Err(io_error(format!(
                    "A child task lives in its owner's conversation {}",
                    current.conversation_id
                )));
            }
            owner = Some(current);
        }
        let scope_conversation_id = lock(&self.inner).scope.conversation_id;
        // 阶段 3：解析会话（owner 的任务会话 > 显式选项 > 事务作用域），没有会话就无法创建。
        let conversation_id = owner
            .as_ref()
            .map(|record| record.conversation_id)
            .or(options.conversation_id)
            .or(scope_conversation_id);
        let Some(conversation_id) = conversation_id else {
            return Err(Box::new(TransactionTypeError::new(
                "Tx.createTask() requires options.conversationId",
            )));
        };
        self.require_conversation(conversation_id).await?;
        self.assert_open_now()?;
        let definition = Arc::clone(&task.definition);
        // `initial` 是本项目代码（可能 panic 或返回任意 JSON）；它在铸号之前执行，失败不会
        // 消耗 ID。
        let checkpoint = call_task_initial(&definition, input.clone())?;
        let minted = self.host.storage().mint_id(IdKind::Task).await?;
        self.assert_open_now()?;
        let id: TaskId = id_from_number(minted);
        // trace 解析：显式 `Root` 表示不继承；显式 `Carrier` 原样使用；缺省时从当前上下文的
        // telemetry 传播信息注入（没有传播信息则保持 `None`）。
        let trace = match options.trace.clone() {
            Some(TaskTrace::Root) => None,
            Some(TaskTrace::Carrier(carrier)) => Some(carrier),
            None => {
                let context = self.context();
                let telemetry = context.telemetry();
                match telemetry.propagation() {
                    Some(propagation) => {
                        let telemetry: &dyn kiana_telemetry::TelemetryContext = &*telemetry;
                        propagation.inject(telemetry)
                    }
                    None => None,
                }
            }
        };
        let record = TaskRecord {
            id,
            conversation_id,
            kind: definition.name.clone(),
            version: definition.version,
            input,
            trace,
            links: options.links.clone(),
            owner: owner.as_ref().map(|record| record.id),
            background: options.background.unwrap_or(false),
            abort_requested: false,
            abandon_on_restart: match options.abandon_on_restart {
                Some(true) => Some(true),
                _ => None,
            },
            state: TaskState::Pending { checkpoint },
            started_at: None,
            ended_at: None,
            restarts: None,
            abort_reason: None,
            memos: None,
        };
        lock(&self.inner).tasks_by_id.insert(
            id,
            TransactionTask {
                committed_read: None,
                write: Some(TaskWrite {
                    kind: TaskWriteKind::Create,
                    record,
                }),
                publication_conversation_id: None,
            },
        );
        Ok(id)
    }

    /// 创建一个原始提交记录。
    ///
    /// 只暂存记录，不校验状态转换；`status` 由调用方决定（通常是 `queued`）。键序按 TS 的
    /// `copyJson({ ...create, id })` 重建，`id` 总在最后，结算时再按变化追加 `answer` 等键。
    pub async fn create_submission(
        &self,
        create: SubmissionCreate,
    ) -> Result<SubmissionRecord, BoxError> {
        self.write(async {
            self.require_conversation(create.conversation_id).await?;
            self.assert_open_now()?;
            let minted = self.host.storage().mint_id(IdKind::Submission).await?;
            self.assert_open_now()?;
            let id: SubmissionId = id_from_number(minted);
            // TS: `copyJson({ ...create, id })`——`id` 总在创建字段之后（创建字段由
            // `{ conversationId, requestId, trace, type, status, entry/answer/reason/detail }` 组成）。
            let mut order: Vec<String> = Vec::with_capacity(10);
            order.push("conversationId".to_owned());
            if create.request_id.is_some() {
                order.push("requestId".to_owned());
            }
            if create.trace.is_some() {
                order.push("trace".to_owned());
            }
            order.push("type".to_owned());
            order.push("status".to_owned());
            if create.entry.is_some() {
                order.push("entry".to_owned());
            }
            if create.answer.is_some() {
                order.push("answer".to_owned());
            }
            if create.reason.is_some() {
                order.push("reason".to_owned());
            }
            if create.detail.is_some() {
                order.push("detail".to_owned());
            }
            order.push("id".to_owned());
            let record = SubmissionRecord {
                conversation_id: create.conversation_id,
                request_id: create.request_id,
                trace: create.trace,
                kind: create.kind,
                status: create.status,
                entry: create.entry,
                id,
                answer: create.answer,
                reason: create.reason,
                detail: create.detail,
                key_order: Some(order),
            };
            lock(&self.inner).submissions.insert(id, record.clone());
            Ok(record)
        })
        .await
    }

    /// 落定一个提交（装配期解析）。
    ///
    /// TS 的 `settleSubmission()` 在事务结算后抛出 `Transaction has settled`；`Tx` trait 的方法
    /// 不能返回错误，因此这里以同样的消息 panic（需要可处理错误的调用方用
    /// [`Transaction::try_settle_submission`]）。
    ///
    /// 本调用只把变化排进队列并置位表写标志；目标提交是否存在、状态是否允许转换，要等到
    /// `assemble()` 里 `apply_submission_change` 才校验，失败会让整个提交失败。
    pub fn settle_submission(&self, id: SubmissionId, settlement: SubmissionSettlement) {
        self.try_settle_submission(id, settlement)
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// `settleSubmission()` 的可观察错误版本。
    ///
    /// 只检查事务是否开放；它和记账版本都会把变化排队，真正的状态校验延迟到 `assemble()`。
    pub fn try_settle_submission(
        &self,
        id: SubmissionId,
        settlement: SubmissionSettlement,
    ) -> Result<(), BoxError> {
        let mut inner = lock(&self.inner);
        Self::assert_open(&inner)?;
        inner.has_table_write = true;
        inner
            .submission_changes
            .push((id, SubmissionChange::Settlement(settlement)));
        Ok(())
    }

    /// 把一个 queued 提交放到 `entry`。
    ///
    /// 与 [`Transaction::settle_submission`] 一样，结算后以 TS 的消息 panic。
    ///
    /// 状态转换延迟到装配期：`Input` 变成 `Placed`，其他类型（被动写入）直接变 `Done`。
    pub fn place_submission(&self, id: SubmissionId, entry: EntryId) {
        self.try_place_submission(id, entry)
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// `placeSubmission()` 的可观察错误版本。
    ///
    /// 只检查事务是否开放；目标提交的状态检查同样在 `assemble()`。
    pub fn try_place_submission(&self, id: SubmissionId, entry: EntryId) -> Result<(), BoxError> {
        let mut inner = lock(&self.inner);
        Self::assert_open(&inner)?;
        inner.has_table_write = true;
        inner
            .submission_changes
            .push((id, SubmissionChange::Placed { entry }));
        Ok(())
    }

    /// 内部：完整替换一个任务记录。
    ///
    /// 若本事务已有候选：候选终态时拒绝再次替换；候选会话不可改变。时间戳只在该状态首次
    /// 出现且候选/新值都没有值时向宿主时钟取（`running` 记 `started_at`，`terminal` 记
    /// `ended_at`）。本事务里创建的任务再更新仍保留 `Create` 种类——存储语义是插入而非替换。
    pub fn set_task(&self, value: TaskRecord) -> Result<(), BoxError> {
        let mut inner = lock(&self.inner);
        Self::assert_open(&inner)?;
        inner.has_table_write = true;
        let candidate = inner
            .tasks_by_id
            .get(&value.id)
            .and_then(|task| task.write.as_ref())
            .map(|write| write.record.clone());
        if let Some(candidate) = &candidate {
            if candidate.state.status() == TaskStatus::Terminal {
                return Err(io_error(format!(
                    "Task {} already has a terminal candidate",
                    value.id
                )));
            }
            if candidate.conversation_id != value.conversation_id {
                return Err(io_error(format!(
                    "Task {} cannot change conversations",
                    value.id
                )));
            }
        }
        let stamped = self.stamp_times(value, candidate.as_ref())?;
        let kind = match inner
            .tasks_by_id
            .get(&stamped.id)
            .and_then(|task| task.write.as_ref())
            .map(|write| write.kind)
        {
            Some(TaskWriteKind::Create) => TaskWriteKind::Create,
            _ => TaskWriteKind::Replace,
        };
        let task = inner.tasks_by_id.entry(stamped.id).or_default();
        task.write = Some(TaskWrite {
            kind,
            record: stamped,
        });
        Ok(())
    }

    fn stamp_times(
        &self,
        value: TaskRecord,
        candidate: Option<&TaskRecord>,
    ) -> Result<TaskRecord, BoxError> {
        let status = value.state.status();
        // TS 只在真正需要盖时间戳时才调用宿主时钟。
        // 取值的优先级：候选记录已有的时间戳 > 新值自带的时间戳 > 当前状态要求的时钟。
        let stamp = |wanted: bool| -> Result<Option<i64>, BoxError> {
            if wanted {
                Ok(Some(self.host.now()? as i64))
            } else {
                Ok(None)
            }
        };
        let started_at = match candidate
            .and_then(|record| record.started_at)
            .or(value.started_at)
        {
            Some(value) => Some(value),
            None => stamp(status == TaskStatus::Running)?,
        };
        let ended_at = match candidate
            .and_then(|record| record.ended_at)
            .or(value.ended_at)
        {
            Some(value) => Some(value),
            None => stamp(status == TaskStatus::Terminal)?,
        };
        if started_at == value.started_at && ended_at == value.ended_at {
            return Ok(value);
        }
        Ok(TaskRecord {
            started_at,
            ended_at,
            ..value
        })
    }

    /// 内部：本事务已创建或替换的任务候选。
    ///
    /// 按任务首次进入事务的顺序返回（`IndexMap` 的插入顺序）。
    pub fn staged_tasks(&self) -> Vec<TaskRecord> {
        lock(&self.inner)
            .tasks_by_id
            .values()
            .filter_map(|task| task.write.as_ref().map(|write| write.record.clone()))
            .collect()
    }

    /// 内部：本事务已创建或 fork 的会话。
    ///
    /// 按暂存顺序返回；只包含 `StorageWrite::Conversation`，不包含文档/条目。
    pub fn staged_conversations(&self) -> Vec<ConversationRecord> {
        lock(&self.inner)
            .writes
            .iter()
            .filter_map(|write| match write {
                StorageWrite::Conversation { value } => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    // ─── Documents ──────────────────────────────────────────────────────────

    /// 获取文档的可变草稿（不存在时创建）。
    ///
    /// 流程：
    /// 1. 用地址身份（kind + scope + key）查本事务的最新条目；该条目已有未退休的变化时，
    ///    直接返回同一 `Change` 的新草稿，所以同一事务里多次 `doc()` 共享一份活值；
    /// 2. 该地址在本事务里已被标记退休：新推一个条目并跳过冷加载，提交时以新的
    ///    `DocumentId` 重新创建（退休的是旧化身）；
    /// 3. 否则在条目的 `acquire` 门上串行化冷加载：已存在则校验记录的作用域/版本后开始新
    ///    变化；不存在则校验 scope（会话存在、任务存在且非终态），用 `initial(seed)` 生成
    ///    值、铸文档 ID 并登记为创建。
    ///
    /// 失败路径：事务已结算、定义语义与持久记录不符、持久版本高于定义或缺少迁移、目标
    /// 会话/任务不存在、任务已终态。文档草稿不设置表写标志，也不计入表读。
    ///
    /// 地址身份由 kind/scope/key 组成，不包含定义版本：同一地址再次获取会沿用条目上首次
    /// 登记的定义，不会用新传入的 `target.definition.version` 重新校验。
    pub async fn doc(&self, target: &DocTarget) -> Result<DocDraft, BoxError> {
        self.assert_open_now()?;
        let address = target.address();
        let address_id = target.address_id();
        self.assert_task_documents_open(&address)?;
        let mut mode = AcquireMode::Load;
        let index = {
            let mut inner = lock(&self.inner);
            let latest = inner.latest_document_by_address.get(&address_id).copied();
            match latest {
                Some(index) if !inner.documents[index].retire_on_commit => {
                    if let Some(change) = &inner.documents[index].change {
                        return Ok(change.state());
                    }
                    index
                }
                Some(_) => {
                    // 同一事务里重建已退休的化身：跳过加载。
                    mode = AcquireMode::SkipLoad;
                    self.push_document_entry(&mut inner, target, &address, &address_id)
                }
                None => self.push_document_entry(&mut inner, target, &address, &address_id),
            }
        };
        let acquire = Arc::clone(&lock(&self.inner).documents[index].acquire);
        // 门内复查：并发的 `doc()` 可能已经完成了这次获取；直接复用它的 Change，避免同一
        // 事务里同一地址出现两份草稿/两次冷加载。
        let gate = acquire.lock().await;
        if let Some(change) = lock(&self.inner).documents[index].change.as_ref() {
            let draft = change.state();
            drop(gate);
            return Ok(draft);
        }
        let result = self.acquire_document(index, target, mode).await;
        drop(gate);
        result
    }

    fn push_document_entry(
        &self,
        inner: &mut TxInner,
        target: &DocTarget,
        address: &DocumentAddress,
        address_id: &str,
    ) -> usize {
        let entry = DocumentEntry {
            address_id: address_id.to_owned(),
            address: address.clone(),
            definition: Some(Arc::clone(&target.definition)),
            acquire: Arc::new(tokio::sync::Mutex::new(())),
            target: None,
            change: None,
            prepared: None,
            retire_on_commit: false,
        };
        let index = inner.documents.len();
        inner.documents.push(entry);
        inner
            .latest_document_by_address
            .insert(address_id.to_owned(), index);
        index
    }

    async fn acquire_document(
        &self,
        index: usize,
        target: &DocTarget,
        mode: AcquireMode,
    ) -> Result<DocDraft, BoxError> {
        // 阶段 1：取出条目上的定义（fork copy 条目没有定义）与 fork 来源。
        let (definition, address, address_id, fork) = {
            let inner = lock(&self.inner);
            let entry = &inner.documents[index];
            let definition = entry
                .definition
                .clone()
                .unwrap_or_else(|| Arc::clone(&target.definition));
            let fork = match &entry.target {
                Some(DocumentTarget::ForkCopy { record, source }) => {
                    Some((record.clone(), *source))
                }
                _ => None,
            };
            (
                definition,
                entry.address.clone(),
                entry.address_id.clone(),
                fork,
            )
        };
        let context = self.context();
        // 阶段 2（fork copy）：读取源化身在 `source.at` 的值，校验它的记录形状与 fork 时
        // 登记的一致（作用域/kind/key/history/fork），再按目标定义迁移。成功后这个条目从
        // ForkCopy 变成 Created：提交时写完整 base，而不是 `document.copy`。
        if let Some((record, source)) = fork {
            let stored = self
                .host
                .storage()
                .document(source.id, source.at, &*context)
                .await?;
            self.assert_open_now()?;
            let stored = stored.ok_or_else(|| {
                io_error(format!("Fork source document {} cannot be read", source.id))
            })?;
            let matches = matches!(stored.record.scope, DocumentScope::Conversation { .. })
                && stored.record.kind == record.kind
                && stored.record.key == record.key
                && stored.record.history == record.history
                && stored.record.fork == record.fork;
            if !matches {
                return Err(io_error(format!(
                    "Fork source document {} does not match the copied record",
                    source.id
                )));
            }
            let value =
                materialize_document_value(&definition, &record, stored.version, stored.value)?;
            let tracker = Arc::new(Tracker::new(value));
            let change = tracker.begin_change();
            let draft = change.state();
            let mut inner = lock(&self.inner);
            let entry = &mut inner.documents[index];
            entry.definition = Some(definition.clone());
            entry.target = Some(DocumentTarget::Created {
                record,
                version: definition.version,
                tracker,
            });
            entry.change = Some(change);
            return Ok(draft);
        }
        let loaded = if mode == AcquireMode::SkipLoad {
            // 同一事务里重建已退休化身：绝不能把旧化身的持久记录再加载回来。
            None
        } else {
            self.host
                .load(
                    Arc::clone(&definition),
                    &address_id,
                    address.clone(),
                    Arc::clone(&context),
                )
                .await?
        };
        self.assert_open_now()?;
        // 阶段 3（已存在）：定义语义与持久记录必须一致；持久版本高于定义或需要迁移却没有
        // `migrate` 时，check_record_version 会在这里拒绝。通过后以当前值为基线开始变化。
        if let Some(loaded) = loaded {
            check_record_scope(&definition, &loaded.record)?;
            check_record_version(&definition, &loaded.record, loaded.stored_version())?;
            let change = loaded.tracker.begin_change();
            let draft = change.state();
            let mut inner = lock(&self.inner);
            let entry = &mut inner.documents[index];
            entry.target = Some(DocumentTarget::Loaded(loaded));
            entry.change = Some(change);
            return Ok(draft);
        }
        match &address.scope {
            DocumentScope::Conversation { conversation_id } => {
                self.require_conversation(*conversation_id).await?;
            }
            DocumentScope::Task { task_id } => {
                // 新建 task 作用域文档前再次读取任务：本事务里的候选优先，其次 Storage。
                let task = self.current_task(*task_id).await?;
                let Some(task) = task else {
                    return Err(io_error(format!("Task {task_id} does not exist")));
                };
                if task.state.status() == TaskStatus::Terminal {
                    return Err(io_error(format!("Task {task_id} is terminal")));
                }
            }
            DocumentScope::Session => {}
        }
        self.assert_open_now()?;
        // 阶段 4（不存在）：只有 family 定义接收 seed；值在铸号前生成，`initial` panic 会
        // 直接沿提交回调传播。创建记录不带 history/fork（由 document_create 按作用域补）。
        let seed = if definition.family {
            target.seed.as_ref()
        } else {
            None
        };
        let value = (definition.initial)(seed);
        let minted = self.host.storage().mint_id(IdKind::Document).await?;
        self.assert_open_now()?;
        let id: DocumentId = id_from_number(minted);
        let tracker = Arc::new(Tracker::new(value));
        let change = tracker.begin_change();
        let draft = change.state();
        let version = definition.version;
        let record = document_create(&definition, &address, id);
        let mut inner = lock(&self.inner);
        let entry = &mut inner.documents[index];
        entry.target = Some(DocumentTarget::Created {
            record,
            version,
            tracker,
        });
        entry.change = Some(change);
        Ok(draft)
    }

    /// 退役一个文档化身。
    ///
    /// 退役是“提交时”动作：这里只标记，`assemble()` 才生成 `DocumentRetire`。三种入口：
    /// - 本事务已标记退休：幂等返回；
    /// - 本事务有该地址的条目：标记 `retire_on_commit`。若已有变化（`change`）直接结束；
    ///   否则等 `acquire` 门放行，确保并发的 `doc()` 已完成加载/创建，计划里才有可退休的记录；
    /// - 本事务没碰过该地址：从缓存或 Storage 找到当前记录，登记 `RetireOnly`。
    ///
    /// 找不到持久记录时是空操作（返回 `Ok`）。只在从缓存/Storage 读出记录、或登记尚未物化
    /// 的 fork copy 时，才按 `target.definition` 校验记录语义（不符合则报错）；本事务已有该
    /// 地址的条目时沿用获取时的定义，retire 路径不再复核。尚未物化的 fork copy 直接按登记的
    /// copy 记录校验，不需要读取源文档。
    pub async fn retire_doc(&self, target: &DocTarget) -> Result<(), BoxError> {
        self.assert_open_now()?;
        let address = target.address();
        let address_id = target.address_id();
        // 锁内只做决定，不在持锁期间 await：Done 直接返回，WaitAcquire 需要等门，
        // FindRetirement 需要读缓存/Storage。
        enum Decision {
            Done,
            WaitAcquire(usize),
            FindRetirement(usize),
        }
        let decision = {
            let mut inner = lock(&self.inner);
            let latest = inner.latest_document_by_address.get(&address_id).copied();
            match latest {
                Some(index) if inner.documents[index].retire_on_commit => Decision::Done,
                Some(index) => {
                    let fork = match &inner.documents[index].target {
                        Some(DocumentTarget::ForkCopy { record, .. }) => Some(record.clone()),
                        _ => None,
                    };
                    if let Some(record) = fork {
                        check_record_scope(&target.definition, &record)?;
                        inner.documents[index].retire_on_commit = true;
                        Decision::Done
                    } else {
                        let has_change = inner.documents[index].change.is_some();
                        inner.documents[index].retire_on_commit = true;
                        if has_change {
                            Decision::Done
                        } else {
                            Decision::WaitAcquire(index)
                        }
                    }
                }
                None => {
                    let index = self.push_document_entry(&mut inner, target, &address, &address_id);
                    inner.documents[index].retire_on_commit = true;
                    Decision::FindRetirement(index)
                }
            }
        };
        match decision {
            Decision::Done => Ok(()),
            Decision::WaitAcquire(index) => {
                // 等并发的获取结束再返回。若那次获取失败，条目没有 target，结算时会跳过它
                // （退役成为空操作），不会误退一个没加载成功的化身。
                let acquire = Arc::clone(&lock(&self.inner).documents[index].acquire);
                let _gate = acquire.lock().await;
                Ok(())
            }
            Decision::FindRetirement(index) => {
                // 缓存优先；未命中再按当前点查 Storage。两者都没有记录时无需生成 Retirement
                // 写入。
                let record = match self.host.cached(&address_id) {
                    Some(cached) => Some(cached.record),
                    None => {
                        let context = self.context();
                        self.host
                            .storage()
                            .find_document(&address, DocumentPoint::Current, &*context)
                            .await?
                    }
                };
                self.assert_open_now()?;
                let Some(record) = record else {
                    return Ok(());
                };
                check_record_scope(&target.definition, &record)?;
                lock(&self.inner).documents[index].target =
                    Some(DocumentTarget::RetireOnly { record });
                Ok(())
            }
        }
    }

    /// 拒绝为“本事务已暂存为终态”的任务新建/打开文档。
    ///
    /// 只看 `tasks_by_id` 里的候选写，不查 Storage；任务作用域的持久化检查由
    /// `acquire_document` 在“文档尚不存在”的分支完成。
    fn assert_task_documents_open(&self, address: &DocumentAddress) -> Result<(), BoxError> {
        let DocumentScope::Task { task_id } = &address.scope else {
            return Ok(());
        };
        let inner = lock(&self.inner);
        let terminal = inner
            .tasks_by_id
            .get(task_id)
            .and_then(|task| task.write.as_ref())
            .map(|write| write.record.state.status() == TaskStatus::Terminal)
            .unwrap_or(false);
        if terminal {
            return Err(io_error(format!("Task {task_id} is terminal")));
        }
        Ok(())
    }

    /// 要求会话存在：本事务刚创建的直接通过，否则读 Storage（不缓存结果）。
    async fn require_conversation(&self, id: ConversationId) -> Result<(), BoxError> {
        if lock(&self.inner).created_conversation_ids.contains(&id) {
            return Ok(());
        }
        let context = self.context();
        if self
            .host
            .storage()
            .conversation(id, &*context)
            .await?
            .is_none()
        {
            return Err(io_error(format!("Conversation {id} does not exist")));
        }
        Ok(())
    }

    // ─── Settlement ─────────────────────────────────────────────────────────

    /// 回调失败后封闭事务并排空在途操作。
    ///
    /// 步骤：置 `sealed` → `abort()` 所有变化草稿（之后读写它们会 panic 或返回空对象）→
    /// `wait_idle()` 等在途表操作结束 → 把上下文换成 `BACKGROUND_CONTEXT`。没有返回值：
    /// 调用方（Session）保留原始错误并返回给提交者。
    pub async fn settle_failure(&self) {
        {
            let mut inner = lock(&self.inner);
            inner.sealed = true;
            self.abort_changes(&mut inner);
        }
        self.wait_idle().await;
        lock(&self.inner).context = Arc::clone(&BACKGROUND_CONTEXT);
    }

    /// 回调成功后封闭事务、准备全部变化并装配原子批次。
    ///
    /// 先置 `sealed`。若仍有在途操作，放弃全部草稿、等排空、换回后台上下文并返回错误。
    /// 否则 `settle_success_inner()` 准备所有文档并调用 `assemble()`；装配失败也会放弃草稿。
    ///
    /// 成功时返回的写入批次还不是最终版：`take_held()` 可能就地改写它，调用方应重新用
    /// `current_writes()` 取一次。无论成功失败，返回前上下文都已换成 `BACKGROUND_CONTEXT`。
    pub async fn settle_success(&self) -> Result<Vec<StorageWrite>, BoxError> {
        {
            let mut inner = lock(&self.inner);
            inner.sealed = true;
        }
        if self.pending_count() > 0 {
            {
                let mut inner = lock(&self.inner);
                self.abort_changes(&mut inner);
            }
            self.wait_idle().await;
            lock(&self.inner).context = Arc::clone(&BACKGROUND_CONTEXT);
            return Err(io_error(
                "Session commit callback settled before its pending Tx operations",
            ));
        }
        let result = self.settle_success_inner().await;
        lock(&self.inner).context = Arc::clone(&BACKGROUND_CONTEXT);
        result
    }

    async fn settle_success_inner(&self) -> Result<Vec<StorageWrite>, BoxError> {
        // 阶段 1：为每个有变化的文档计算 Prepared；这一刻起草稿只读（仍可读）。
        {
            let mut inner = lock(&self.inner);
            for document in &mut inner.documents {
                if let Some(change) = &document.change {
                    document.prepared = Some(change.prepare());
                }
            }
        }
        // 阶段 2：跨记录校验 + 装配。任何错误都会把草稿全部 abort，避免调用方复用半结算
        // 的草稿。
        let result = self.assemble().await;
        if result.is_err() {
            let mut inner = lock(&self.inner);
            self.abort_changes(&mut inner);
        }
        result
    }

    /// 把所有草稿置为 `aborted`；可重复调用（只是再次写状态码）。
    fn abort_changes(&self, inner: &mut TxInner) {
        for document in &inner.documents {
            if let Some(change) = &document.change {
                change.abort();
            }
        }
    }

    /// 当前已组装的全部写入（`take_held()` 会改写这个列表）。
    ///
    /// Session 在 `take_held()` 之后重新取一次，确保折叠进来的文档变化进入最终批次。
    pub fn current_writes(&self) -> Vec<StorageWrite> {
        lock(&self.inner).writes.clone()
    }

    /// 放弃全部已准备的变化（Storage 失败或无需写入时）。
    ///
    /// 只把草稿置为 `aborted`，不删除已暂存的表写入/任务候选，也不封闭事务；调用方
    /// （Session）只在结算之后使用它。
    pub fn discard(&self) {
        let mut inner = lock(&self.inner);
        self.abort_changes(&mut inner);
    }

    /// 延迟提交可以在内存里持有的变化；不能持有时返回 `None`。
    ///
    /// 任一条件不满足就返回 `None`：
    /// - 批次里除 `DocumentChange` 外还有其他写入（会话、条目、任务、提交、文档创建/复制/
    ///   退休）；
    /// - 某个有内容或退休的计划缺少 `Change`、内容不是 `DocumentChange`，或该计划正在退休；
    /// - 记录不是会话作用域，或 `history != latest`，或 `fork != initial`；
    /// - `stored_version` 与当前定义版本不同（需要迁移或以 base 重写）。
    ///
    /// 这些限制把“可延迟”限定在能由后来的 `store_held()` 仅靠 delta 继续累积的会话文档上；
    /// 不满足时调用方应走立即提交路径。返回顺序与 `inner.plans` 一致。
    pub fn holdable(&self) -> Option<Vec<HoldableChange>> {
        let inner = lock(&self.inner);
        if inner
            .writes
            .iter()
            .any(|write| !matches!(write, StorageWrite::DocumentChange { .. }))
        {
            return None;
        }
        let mut held = Vec::new();
        for plan in &inner.plans {
            if plan.content.is_none() && !plan.retire {
                continue;
            }
            let change = plan.change.as_ref()?;
            let loaded = change.loaded.as_ref()?;
            let content = match plan.content.as_ref() {
                Some(StorageWrite::DocumentChange { content, .. }) => content,
                _ => return None,
            };
            if plan.retire {
                return None;
            }
            let record = &loaded.record;
            if !matches!(record.scope, DocumentScope::Conversation { .. })
                || record.history != Some(DocumentHistory::Latest)
                || record.fork != Some(DocumentFork::Initial)
            {
                return None;
            }
            if loaded.stored_version() != change.version {
                return None;
            }
            held.push(HoldableChange {
                loaded: loaded.clone(),
                ops: change.prepared.ops.clone(),
                base: content.kind == DocumentContentKind::Base,
            });
        }
        Some(held)
    }

    /// 仅内存采纳一次延迟提交。
    ///
    /// 对每个有非空 ops 的计划：把 `Prepared.value` 写回 tracker、把草稿置为 `consumed`；
    /// 已加载的化身生成 `Document` 发布项（创建中的化身没有 `loaded`，因此不发布）。返回的
    /// 列表由 Session 作为 `ObservedCommit::Memory` 通知内部监听器；此时 Storage 没有任何
    /// 写入，持久化要等 hold 的 deadline 或下一次提交。
    pub fn adopt_held(&self) -> Vec<DocumentCommitChange> {
        let mut publications = Vec::new();
        let mut inner = lock(&self.inner);
        for plan in &mut inner.plans {
            let Some(change) = &plan.change else {
                continue;
            };
            if change.prepared.ops.is_empty() {
                continue;
            }
            change.tracker.adopt(change.prepared.value.clone());
            if let Some(status) = &change.status {
                status.store(DRAFT_CONSUMED, Ordering::SeqCst);
            }
            let Some(loaded) = &change.loaded else {
                continue;
            };
            publications.push(DocumentCommitChange::Document {
                record: loaded.record.clone(),
                conversation_id: plan.conversation_id,
                version: Some(change.version),
                value: Some(change.prepared.value.clone()),
                ops: Ops(change.prepared.ops.clone()),
            });
        }
        publications
    }

    /// 在 Storage 接纳前把本提交读/写到的化身的 held 变化并进自己的写入。
    ///
    /// `held` 回调按 `DocumentId` 取 Session 持有的 [`Hold`]；只考虑已存在的化身（本事务
    /// 新建的文档不可能有 hold）。对每个命中：
    /// 1. 本提交自己的值优先（有变化取 `Prepared.value`，否则读 hold 的 tracker）；
    /// 2. `store_held()` 把本提交的 ops 与 hold 的 pending 折叠成一次写入；
    /// 3. 若本提交自己写了 base，则用本提交的内容（base 覆盖旧的 delta 累积）；
    /// 4. 写回 `inner.writes`：本提交本来就有该文档的写入时原位替换，否则插到同一 ID 的
    ///    `DocumentRetire` 之前（没有退休则追加）；
    /// 5. 不退休时记录 `PlanHeld { ops, observed }`，供 [`Transaction::adopt`] 区分“自己
    ///    观察到的”与“Storage 实际发布的”列表；同时更新计划的发布会话。
    ///
    /// 返回被取用的 holds；调用方必须在 Storage 成功后释放它们（解除缓存 pin）。
    pub fn take_held(
        &self,
        held: &(dyn Fn(DocumentId) -> Option<Arc<Mutex<Hold>>> + Send + Sync),
    ) -> Vec<Arc<Mutex<Hold>>> {
        // 只有已提交化身可能有 hold；`PlanRecord::Create` 一律跳过。
        let candidates: Vec<(usize, DocumentId)> = {
            let inner = lock(&self.inner);
            inner
                .plans
                .iter()
                .enumerate()
                .filter_map(|(index, plan)| match &plan.record {
                    PlanRecord::Existing(record) => Some((index, record.id)),
                    PlanRecord::Create(_) => None,
                })
                .collect()
        };
        let mut found: Vec<(usize, Arc<Mutex<Hold>>)> = Vec::new();
        for (index, id) in candidates {
            if let Some(hold) = held(id) {
                found.push((index, hold));
            }
        }
        if found.is_empty() {
            return Vec::new();
        }
        let mut taken = Vec::with_capacity(found.len());
        let mut inner = lock(&self.inner);
        for (index, hold) in found {
            let own = match &inner.plans[index].content {
                Some(StorageWrite::DocumentChange { id, content }) => Some((*id, content.clone())),
                _ => None,
            };
            let value = match inner.plans[index].change.as_ref() {
                Some(change) => change.prepared.value.clone(),
                None => lock(&hold).held.loaded.tracker.value(),
            };
            let own_ops = inner.plans[index]
                .change
                .as_ref()
                .map(|change| change.prepared.ops.clone())
                .unwrap_or_default();
            // 版本取 hold 化身的 value_version：待存 ops 是针对这个版本累积的。
            let version = lock(&hold).held.loaded.value_version;
            // 折叠：把本提交的 ops 并入 hold 的 pending 后一次性取出。`stored_content` 可能
            // 是 base（pending 溢出或 hold.base 置位），`stored_ops` 是给监听器重放的批次。
            let (stored_content, stored_ops) = {
                let guard = lock(&hold);
                let stored = store_held(&guard.held, &own_ops, &value, version);
                (stored.content, stored.ops)
            };
            let content = match &own {
                Some((_, own_content)) if own_content.kind == DocumentContentKind::Base => {
                    own_content.clone()
                }
                _ => stored_content,
            };
            let id = inner.plans[index].record.id();
            let observed = own.is_some();
            let write = StorageWrite::DocumentChange {
                id,
                content: content.clone(),
            };
            match own {
                Some(_) => {
                    if let Some(position) = inner.writes.iter().position(|candidate| {
                        matches!(candidate, StorageWrite::DocumentChange { id: candidate_id, .. } if *candidate_id == id)
                    }) {
                        inner.writes[position] = write;
                    } else {
                        inner.writes.push(write);
                    }
                }
                None => insert_before_retirement(&mut inner.writes, write),
            }
            let plan = &mut inner.plans[index];
            if !plan.retire {
                plan.held = Some(PlanHeld {
                    ops: stored_ops,
                    observed,
                });
            }
            plan.content = Some(StorageWrite::DocumentChange { id, content });
            let scope = plan.record.scope();
            if let DocumentScope::Conversation { conversation_id } = scope {
                plan.conversation_id = Some(conversation_id);
            }
            taken.push(hold);
        }
        taken
    }

    /// Storage 成功后采纳每个已准备的变化，并描述发布。
    ///
    /// 对每个计划：
    /// - 用 `seq` 生成创建记录的 `created_at`，退休记录写 `retired_at`；
    /// - 已有变化时把 `Prepared.value` 采纳进 tracker、把草稿置为 `consumed`，并更新持久
    ///   版本与 delta 计数；已加载文档的 `ops` 为空时不采纳（没有实际变化）；
    /// - 本事务创建且未退休的化身安装进 Session 缓存；已提交化身被退休时请求驱逐（宿主会
    ///   校验该地址仍由同一 `DocumentId` 占用）；
    /// - `describe()` 生成 `observed` 发布项；有 held 折叠时 `stored` 额外包含只由 Storage
    ///   发布的变化（`observed` 只保留本提交自己改过的文档）。
    ///
    /// 返回 [`AdoptedDocuments`] 供 Session 通知内部/宿主监听器。调用方只在 Storage 真正
    /// 持久化批次后，通过 `Storage::commit` 的同步 `adopt` 回调调用一次（见 `Storage` trait
    /// 的契约）；它会修改 tracker 与缓存状态，不能提前或重复调用。
    pub fn adopt(&self, seq: Seq) -> AdoptedDocuments {
        let mut publications: Vec<DocumentCommitChange> = Vec::new();
        let mut stored: Option<Vec<DocumentCommitChange>> = None;
        let inner = lock(&self.inner);
        let plan_count = inner.plans.len();
        for index in 0..plan_count {
            let committed = inner.plans[index].record.is_committed();
            // 创建计划没有完整 DocumentRecord，用 `seq` 补齐 `created_at`；退休在这里打上
            // `retired_at`，发布与缓存都以这份记录为准。
            let mut record: DocumentRecord = match &inner.plans[index].record {
                PlanRecord::Existing(record) => record.clone(),
                PlanRecord::Create(create) => DocumentRecord {
                    id: create.id,
                    kind: create.kind.clone(),
                    key: create.key.clone(),
                    scope: create.scope,
                    history: create.history,
                    fork: create.fork,
                    created_at: seq,
                    retired_at: None,
                },
            };
            if inner.plans[index].retire {
                record.retired_at = Some(seq);
            }
            let plan = &inner.plans[index];
            let mut installed: Option<LoadedDocument> = None;
            let mut evicted: Option<(String, DocumentId)> = None;
            if let Some(change) = &plan.change {
                // 已加载文档只有真的产生 op 才采纳；新建化身只要没被退休就采纳（初值本身
                // 就是一次创建）。
                let adopt_change = match &change.loaded {
                    Some(_) => !change.prepared.ops.is_empty(),
                    None => !plan.retire,
                };
                if adopt_change {
                    change.tracker.adopt(change.prepared.value.clone());
                    if let Some(status) = &change.status {
                        status.store(DRAFT_CONSUMED, Ordering::SeqCst);
                    }
                }
                if let Some(loaded) = &change.loaded {
                    // 持久版本单调提升；delta/base 的实际写入决定 delta 计数（base 归零，
                    // delta 加一）。内容为 `None` 表示这次没有写内容，计数保持。
                    loaded.raise_stored_version(change.version);
                    if let Some(StorageWrite::DocumentChange { content, .. }) = &plan.content {
                        loaded.note_content(content);
                    }
                } else if !plan.retire {
                    installed = Some(LoadedDocument::new(
                        plan.address_id.clone(),
                        record.clone(),
                        change.version,
                        change.version,
                        0,
                        Arc::clone(&change.tracker),
                    ));
                }
            }
            if plan.retire && committed {
                // 本事务新建又退休的化身从未进过缓存，不需要驱逐。
                evicted = Some((plan.address_id.clone(), record.id));
            }
            let publication = describe(plan, &record);
            if let Some(publication) = publication {
                match &plan.held {
                    None => {
                        // 没有折叠别人的 held 变化：observed 与 stored 共用同一条。
                        publications.push(publication.clone());
                        if let Some(list) = &mut stored {
                            list.push(publication);
                        }
                    }
                    Some(held) => {
                        // 一旦出现 held，stored 必须先补齐此前已产生的 observed 项；之后
                        // 本提交自己改过的文档（observed）两边都收，纯 held 文档只进 stored，
                        // 且它的 ops 用折叠后的批次而不是 plan 的 prepared.ops。
                        let list = stored.get_or_insert_with(|| publications.clone());
                        if held.observed {
                            publications.push(publication.clone());
                        }
                        list.push(match publication {
                            DocumentCommitChange::Document {
                                record,
                                conversation_id,
                                version,
                                value,
                                ..
                            } => DocumentCommitChange::Document {
                                record,
                                conversation_id,
                                version,
                                value,
                                ops: Ops(held.ops.clone()),
                            },
                            other => other,
                        });
                    }
                }
            }
            if let Some(document) = installed {
                self.host.install(document);
            }
            if let Some((address_id, record_id)) = evicted {
                self.host.evict(&address_id, record_id);
            }
        }
        let stored = stored.unwrap_or_else(|| publications.clone());
        AdoptedDocuments {
            observed: publications,
            stored,
        }
    }

    /// 结算主装配：校验跨记录不变量，并把暂存状态变成一个原子写入批次。
    ///
    /// 阶段：
    /// 1. 由 `documents` 条目生成计划（`plan_document`），拒绝本事务改动 fork 源；
    /// 2. 校验会话/任务 owner 存在且活跃；
    /// 3. 校验被替换的任务：已提交、非终态、会话不变；
    /// 4. 终态任务让所有 task 作用域文档退休——本事务计划里的直接标记，Storage 里已有但
    ///    本事务没碰过的分页扫描后补进计划，按 `DocumentId` 去重；
    /// 5. 为发布项解析所属会话；再应用排队的提交变化（未知提交/非法转换报错）；
    /// 6. 汇总写入：暂存表写、提交记录、任务记录，最后文档内容与退休。delta 内容会询问
    ///    定义的 `checkpoint_when`（用户代码，panic 被转成错误）是否改写成 base。
    ///
    /// 结果同时写回 `inner.plans` 与 `inner.writes`；`take_held()` 之后还会就地改写后者。
    async fn assemble(&self) -> Result<Vec<StorageWrite>, BoxError> {
        let storage = self.host.storage();
        let context = self.context();
        let mut plans: Vec<DocumentPlan> = {
            let inner = lock(&self.inner);
            inner.documents.iter().filter_map(plan_document).collect()
        };
        {
            let inner = lock(&self.inner);
            reject_fork_source_writes(&plans, &inner)?;
        }
        self.validate_owners().await?;
        // 替换任务必须是已提交、非终态、同一会话的记录。
        let replaces: Vec<TaskId> = {
            let inner = lock(&self.inner);
            inner
                .tasks_by_id
                .iter()
                .filter(|(_, task)| {
                    task.write
                        .as_ref()
                        .map(|write| write.kind == TaskWriteKind::Replace)
                        .unwrap_or(false)
                })
                .map(|(id, _)| *id)
                .collect()
        };
        for id in replaces {
            let committed = self.committed_task(id).await?;
            let Some(committed) = committed else {
                return Err(io_error(format!("Task {id} does not exist")));
            };
            let candidate_conversation = {
                let inner = lock(&self.inner);
                inner
                    .tasks_by_id
                    .get(&id)
                    .and_then(|task| task.write.as_ref())
                    .map(|write| write.record.conversation_id)
            };
            if committed.state.status() == TaskStatus::Terminal {
                return Err(io_error(format!("Task {id} is already terminal")));
            }
            if Some(committed.conversation_id) != candidate_conversation {
                return Err(io_error(format!("Task {id} cannot change conversations")));
            }
        }
        // 终态任务退休其全部 task 作用域文档（含本事务创建的）。
        let terminal_task_ids: Vec<TaskId> = {
            let inner = lock(&self.inner);
            inner
                .tasks_by_id
                .values()
                .filter_map(|task| task.write.as_ref())
                .filter(|write| write.record.state.status() == TaskStatus::Terminal)
                .map(|write| write.record.id)
                .collect()
        };
        if !terminal_task_ids.is_empty() {
            let mut retiring: HashSet<DocumentId> = HashSet::new();
            for plan in &mut plans {
                let DocumentScope::Task { task_id } = plan.record.scope() else {
                    continue;
                };
                if !terminal_task_ids.contains(&task_id) {
                    continue;
                }
                plan.retire = true;
                retiring.insert(plan.record.id());
            }
            for task_id in &terminal_task_ids {
                let created_here = {
                    let inner = lock(&self.inner);
                    inner
                        .tasks_by_id
                        .get(task_id)
                        .and_then(|task| task.write.as_ref())
                        .map(|write| write.kind == TaskWriteKind::Create)
                        .unwrap_or(false)
                };
                if created_here {
                    // 本事务刚创建的任务在 Storage 里还没有文档，扫描只会白跑。
                    continue;
                }
                let mut cursor: Option<Cursor> = None;
                loop {
                    let page = storage
                        .scan_documents(
                            &DocumentQuery {
                                scope: DocumentScope::Task { task_id: *task_id },
                                at: DocumentPoint::Current,
                                kind: None,
                            },
                            INTERNAL_SCAN_PAGE_SIZE,
                            cursor.as_ref(),
                            &*context,
                        )
                        .await?;
                    for record in page.items {
                        if retiring.contains(&record.id) {
                            continue;
                        }
                        plans.push(DocumentPlan {
                            address_id: address_id(&DocumentAddress {
                                kind: record.kind.clone(),
                                scope: record.scope,
                                key: record.key.clone(),
                            }),
                            record: PlanRecord::Existing(record.clone()),
                            retire: true,
                            content: None,
                            change: None,
                            conversation_id: None,
                            held: None,
                        });
                        retiring.insert(record.id);
                    }
                    cursor = page.next;
                    if cursor.is_none() {
                        break;
                    }
                }
            }
        }
        // 在 Storage 接纳前解析发布归属，采纳保持同步。
        for plan in &mut plans {
            if !publishes(plan) {
                continue;
            }
            let scope = plan.record.scope();
            if let DocumentScope::Conversation { conversation_id } = scope {
                plan.conversation_id = Some(conversation_id);
            }
            let DocumentScope::Task { task_id } = scope else {
                continue;
            };
            let known = {
                let inner = lock(&self.inner);
                inner
                    .tasks_by_id
                    .get(&task_id)
                    .and_then(|task| task.publication_conversation_id)
            };
            let conversation_id = match known {
                Some(conversation_id) => Some(conversation_id),
                None => {
                    let current = self.current_task(task_id).await?;
                    let conversation_id = current.map(|record| record.conversation_id);
                    if let Some(conversation_id) = conversation_id {
                        let mut inner = lock(&self.inner);
                        inner
                            .tasks_by_id
                            .entry(task_id)
                            .or_default()
                            .publication_conversation_id = Some(conversation_id);
                    }
                    conversation_id
                }
            };
            plan.conversation_id = conversation_id;
        }
        // 提交变化。
        let changes: Vec<(SubmissionId, SubmissionChange)> = {
            let inner = lock(&self.inner);
            inner.submission_changes.clone()
        };
        for (id, change) in changes {
            let current = {
                let inner = lock(&self.inner);
                inner.submissions.get(&id).cloned()
            };
            let current = match current {
                Some(current) => Some(current),
                None => storage.submission(id, &*context).await?,
            };
            let Some(current) = current else {
                return Err(io_error(format!("Submission {id} does not exist")));
            };
            let next = apply_submission_change(&current, &change)?;
            if next != current {
                lock(&self.inner).submissions.insert(id, next);
            }
        }
        let mut writes = {
            let inner = lock(&self.inner);
            inner.writes.clone()
        };
        // 写入顺序：先暂存的会话/条目写，再提交记录、任务记录，最后按计划追加文档内容与
        // 退休。Storage 必须原子地应用整批（见 `Storage::commit`）。
        {
            let inner = lock(&self.inner);
            for value in inner.submissions.values() {
                writes.push(StorageWrite::Submission {
                    value: value.clone(),
                });
            }
            for task in inner.tasks_by_id.values() {
                if let Some(write) = &task.write {
                    writes.push(StorageWrite::Task {
                        value: write.record.clone(),
                    });
                }
            }
        }
        for plan in &mut plans {
            if let Some(StorageWrite::DocumentChange { content, .. }) = &plan.content
                && content.kind == DocumentContentKind::Delta
                && let Some(change) = &plan.change
                && let Some(loaded) = &change.loaded
            {
                // checkpoint 判据是用户提供的同步闭包，可能 panic；catch_unwind 把 panic
                // 转成提交错误，避免 unwind 穿过 Storage 提交路径。返回 true 时把这次 delta
                // 改写成完整 base（`value` 已包含本次变化）。
                let definition = change
                    .definition
                    .as_ref()
                    .expect("loaded plans carry their definition");
                let info = CheckpointInfo {
                    deltas_since_base: loaded.deltas_since_base(),
                };
                let decision = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (definition.checkpoint_when)(&change.prepared.value, &change.prepared.ops, info)
                }));
                match decision {
                    Ok(true) => {
                        plan.content = Some(StorageWrite::DocumentChange {
                            id: plan.record.id(),
                            content: DocumentContent::base(
                                change.version,
                                change.prepared.value.clone(),
                            ),
                        });
                    }
                    Ok(false) => {}
                    Err(payload) => return Err(panic_message(payload)),
                }
            }
            if let Some(content) = &plan.content {
                writes.push(content.clone());
            }
            if plan.retire {
                writes.push(StorageWrite::DocumentRetire {
                    id: plan.record.id(),
                });
            }
        }
        {
            let mut inner = lock(&self.inner);
            inner.plans = plans;
            // 与 TS 相同：`this.#writes` 保持同一个列表，`takeHeld()` 之后还要改写它。
            inner.writes = writes.clone();
        }
        Ok(writes)
    }

    /// 校验本事务即将写入的 owner 引用。
    ///
    /// 检查对象：会话记录里的 `owner.task_id` 与本事务新建任务的 `owner`。owner 任务必须
    /// 存在，且不能处于 `completing`/`terminal` 或已请求 abort；违规会让整个提交失败。
    async fn validate_owners(&self) -> Result<(), BoxError> {
        let mut owners: Vec<(String, TaskId)> = Vec::new();
        {
            let inner = lock(&self.inner);
            for write in &inner.writes {
                if let StorageWrite::Conversation { value } = write
                    && let Some(owner) = &value.owner
                {
                    owners.push(("Conversation owner task".to_owned(), owner.task_id));
                }
            }
            for task in inner.tasks_by_id.values() {
                let record = match &task.write {
                    Some(write) if write.kind == TaskWriteKind::Create => Some(&write.record),
                    _ => None,
                };
                if let Some(record) = record
                    && let Some(owner) = record.owner
                {
                    owners.push(("Task owner".to_owned(), owner));
                }
            }
        }
        for (what, task_id) in owners {
            let task = self.current_task(task_id).await?;
            let Some(task) = task else {
                return Err(io_error(format!("{what} {task_id} does not exist")));
            };
            let status = task.state.status();
            if matches!(status, TaskStatus::Terminal | TaskStatus::Completing) {
                return Err(io_error(format!(
                    "{what} {task_id} is {}",
                    status_str(status)
                )));
            }
            if task.abort_requested {
                return Err(io_error(format!("{what} {task_id} is abort-marked")));
            }
        }
        Ok(())
    }

    fn task_entry(
        tasks: &mut indexmap::IndexMap<TaskId, TransactionTask>,
        id: TaskId,
    ) -> &mut TransactionTask {
        tasks.entry(id).or_default()
    }

    /// 先在事务候选里找任务，找不到再读已提交记录。
    ///
    /// 候选由 `create_task` 或 `set_task` 暂存；候选缺失时读取会穿透到 Storage，因此调用方
    /// 在 `await` 之后通常会再 `assert_open_now`。
    async fn current_task(&self, id: TaskId) -> Result<Option<TaskRecord>, BoxError> {
        let candidate = {
            let inner = lock(&self.inner);
            inner
                .tasks_by_id
                .get(&id)
                .and_then(|task| task.write.as_ref())
                .map(|write| write.record.clone())
        };
        if candidate.is_some() {
            return Ok(candidate);
        }
        self.committed_task(id).await
    }

    /// 已提交任务读取；成功时每个 `TaskId` 每事务只发起一次后端读。
    async fn committed_task(&self, id: TaskId) -> Result<Option<TaskRecord>, BoxError> {
        // TS: `task.committedRead ??= storage.task(id, context)`——缓存 promise 本身，因此同一
        // 事务里并发（join!/select!）的重复读取只做一次后端读，所有调用者等待同一个结果。
        let cell = {
            let mut inner = lock(&self.inner);
            let task = Self::task_entry(&mut inner.tasks_by_id, id);
            Arc::clone(
                task.committed_read
                    .get_or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };
        let context = self.context();
        let value = cell
            .get_or_try_init(|| async { self.host.storage().task(id, &*context).await })
            .await?;
        Ok(value.clone())
    }
}

/// `holdable()` 的一项。
///
/// `base` 表示这次写入计划的内容是完整 base（`DocumentContentKind::Base`）；`ops` 是相对
/// 本次载入 tracker 值的批次：该地址若已有未落盘的 hold，tracker 已包含这些内存变化（见
/// [`Transaction::adopt_held`]），只有与 hold 的 pending 一起经 `store_held()` 折叠后才相对
/// 上次持久值；`loaded` 是共享的缓存化身，hold 成立后会继续使用它的 tracker 与 `state`。
#[derive(Debug, Clone)]
pub struct HoldableChange {
    /// 被持有的化身。
    pub loaded: LoadedDocument,
    /// 本提交自己的操作。
    pub ops: Vec<Op>,
    /// 本提交是否选择了 base。
    pub base: bool,
}

// 在途操作计数 guard：创建时加一，Drop 时减一（饱和到 0）；`settle_*` 用 `wait_idle()`
// 等它归零。
struct PendingOperation<'a> {
    transaction: &'a Transaction,
}

impl Drop for PendingOperation<'_> {
    fn drop(&mut self) {
        self.transaction
            .pending_operations
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

/// 参数类型错误的等价物（消息是当前稳定契约，改动需同步测试）。
///
/// 用于“调用参数在事务语义上非法”的情况，例如 `createTask` 解析不出会话、子任务被要求
/// 设为 background。与普通 `io::Error` 一样以 `BoxError` 返回。
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct TransactionTypeError {
    /// 完整消息。
    pub message: String,
}

impl TransactionTypeError {
    /// 新建。
    pub fn new(message: impl Into<String>) -> Self {
        TransactionTypeError {
            message: message.into(),
        }
    }
}

// 把 panic payload 转成提交错误：优先取 `String`，其次 `&'static str`，都不是就用固定文案。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> BoxError {
    match payload.downcast::<String>() {
        Ok(message) => io_error(*message),
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => io_error((*message).to_owned()),
            Err(_) => io_error("checkpoint predicate panicked"),
        },
    }
}

fn status_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Waiting => "waiting",
        TaskStatus::Sleeping => "sleeping",
        TaskStatus::Completing => "completing",
        TaskStatus::Terminal => "terminal",
    }
}

// 把内容写入插到同一个 DocumentId 的退休写入之前；没有对应退休时追加到末尾。
// 只调整 `writes` 里的相对位置，不改变写入内容。
fn insert_before_retirement(writes: &mut Vec<StorageWrite>, write: StorageWrite) {
    let StorageWrite::DocumentChange { id, .. } = &write else {
        writes.push(write);
        return;
    };
    match writes.iter().position(|candidate| {
        matches!(candidate, StorageWrite::DocumentRetire { id: candidate_id } if candidate_id == id)
    }) {
        Some(position) => writes.insert(position, write),
        None => writes.push(write),
    }
}

// fork 事务内的保护：本事务不能写/退休任何被选作 copy 来源的文档；同一事务里 fork 父会话
// 并改动其 `current` 策略文档也会被拒绝。没有内容且不退休的计划直接跳过。
fn reject_fork_source_writes(plans: &[DocumentPlan], inner: &TxInner) -> Result<(), BoxError> {
    for plan in plans {
        if plan.content.is_none() && !plan.retire {
            continue;
        }
        let id = plan.record.id();
        if inner.fork_source_document_ids.contains(&id) {
            return Err(io_error(format!(
                "Cannot change fork source document {id} in the fork transaction"
            )));
        }
        if let DocumentScope::Conversation { conversation_id } = plan.record.scope()
            && inner
                .fork_source_conversation_ids
                .contains(&conversation_id)
            && plan.record.record_fork() == Some(DocumentFork::Current)
        {
            return Err(io_error(format!(
                "Cannot fork conversation {conversation_id} while changing its current-policy documents"
            )));
        }
    }
    Ok(())
}

/// 一个暂存文档的写入计划（TS: `planDocument`）。
///
/// `target` 为空时返回 `None`，该地址不产生任何写入。已加载文档的内容选择：持久版本低于
/// 定义版本时写 base（迁移），否则 ops 非空写 delta，ops 为空则没有内容写入。
fn plan_document(document: &DocumentEntry) -> Option<DocumentPlan> {
    let target = document.target.as_ref()?;
    let retire = document.retire_on_commit;
    match target {
        DocumentTarget::Created {
            record,
            version,
            tracker,
        } => {
            let prepared = document
                .prepared
                .as_ref()
                .expect("created documents are prepared")
                .clone();
            Some(DocumentPlan {
                address_id: document.address_id.clone(),
                record: PlanRecord::Create(record.clone()),
                retire,
                content: Some(StorageWrite::DocumentCreate {
                    record: record.clone(),
                    content: DocumentBaseContent::new(*version, prepared.value.clone()),
                }),
                change: Some(PlanChange {
                    tracker: Arc::clone(tracker),
                    prepared,
                    version: *version,
                    loaded: None,
                    definition: document.definition.clone(),
                    status: document.change.as_ref().map(Change::status_handle),
                }),
                conversation_id: None,
                held: None,
            })
        }
        DocumentTarget::ForkCopy { record, source } => Some(DocumentPlan {
            address_id: document.address_id.clone(),
            record: PlanRecord::Create(record.clone()),
            retire,
            content: Some(StorageWrite::DocumentCopy {
                record: record.clone(),
                source: *source,
            }),
            change: None,
            conversation_id: None,
            held: None,
        }),
        DocumentTarget::RetireOnly { record } => Some(DocumentPlan {
            address_id: document.address_id.clone(),
            record: PlanRecord::Existing(record.clone()),
            retire,
            content: None,
            change: None,
            conversation_id: None,
            held: None,
        }),
        DocumentTarget::Loaded(loaded) => {
            let definition = document
                .definition
                .as_ref()
                .expect("loaded documents carry their definition")
                .clone();
            let prepared = document
                .prepared
                .as_ref()
                .expect("loaded documents are prepared")
                .clone();
            let version = definition.version;
            let id = loaded.record.id;
            let content = if loaded.stored_version() < version {
                Some(StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::base(version, prepared.value.clone()),
                })
            } else if !prepared.ops.is_empty() {
                Some(StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::delta(version, prepared.ops.clone()),
                })
            } else {
                None
            };
            Some(DocumentPlan {
                address_id: document.address_id.clone(),
                record: PlanRecord::Existing(loaded.record.clone()),
                retire,
                content,
                change: Some(PlanChange {
                    tracker: Arc::clone(&loaded.tracker),
                    prepared,
                    version,
                    loaded: Some(loaded.clone()),
                    definition: Some(definition),
                    status: document.change.as_ref().map(Change::status_handle),
                }),
                conversation_id: None,
                held: None,
            })
        }
    }
}

/// 是否发布这个计划（TS: `publishes`）。
///
/// 退休、新建化身（变化的 `loaded` 为 `None`）或有内容写入的计划都发布；已加载且没有内容
/// 写入的计划不发布（没有可观察变化）。没有 `change` 的计划（如仅退休）按发布处理。
/// `assemble()` 也用它决定是否解析发布会话。
fn publishes(plan: &DocumentPlan) -> bool {
    plan.retire
        || plan
            .change
            .as_ref()
            .map(|change| change.loaded.is_none())
            .unwrap_or(true)
        || plan.content.is_some()
}

/// 一个已采纳计划的发布项（TS: `describe`）。
///
/// 退休优先（即使同一计划里还写了内容，也只发布退休）；copy 计划发布 `DocumentCopy`；
/// 普通计划发布 `Document`，已加载文档带 `prepared.ops`，新建化身带空 ops。没有可发布内容
/// 时返回 `None`。
fn describe(plan: &DocumentPlan, record: &DocumentRecord) -> Option<DocumentCommitChange> {
    if plan.retire {
        return Some(DocumentCommitChange::Document {
            record: record.clone(),
            conversation_id: plan.conversation_id,
            version: None,
            value: None,
            ops: Ops::new(),
        });
    }
    if let Some(StorageWrite::DocumentCopy { source, .. }) = &plan.content {
        return Some(DocumentCommitChange::DocumentCopy {
            record: record.clone(),
            conversation_id: plan
                .conversation_id
                .expect("fork copies publish a conversation"),
            source: *source,
        });
    }
    let change = plan.change.as_ref()?;
    if !publishes(plan) {
        return None;
    }
    let ops = match &change.loaded {
        None => Ops::new(),
        Some(_) => Ops(change.prepared.ops.clone()),
    };
    Some(DocumentCommitChange::Document {
        record: record.clone(),
        conversation_id: plan.conversation_id,
        version: Some(change.version),
        value: Some(change.prepared.value.clone()),
        ops,
    })
}

// ---------------------------------------------------------------------------
// Tx trait implementation
// ---------------------------------------------------------------------------

// `Tx` trait 的逐方法转发。Transaction 同时保留同名固有方法：内部调用方（如 harness）
// 直接调用固有方法，外部以 `&dyn Tx` 使用时走这里。
#[async_trait]
impl Tx for Transaction {
    async fn conversation(
        &self,
        id: ConversationId,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.conversation_impl(id).await
    }

    async fn entry(&self, id: EntryId) -> Result<Option<EntryRecord>, BoxError> {
        self.entry_impl(id).await
    }

    async fn task(&self, id: TaskId) -> Result<Option<TaskRecord>, BoxError> {
        self.task_impl(id).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.scan_conversations_impl(query, limit, cursor).await
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<EntryRecord>, BoxError> {
        self.scan_entries_impl(query, limit, cursor).await
    }

    async fn latest_head_marker(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.latest_head_marker_impl(conversation_id).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.scan_tasks_impl(query, limit, cursor).await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.submission_by_request_impl(conversation_id, request_id)
            .await
    }

    async fn create_conversation(
        &self,
        options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError> {
        Transaction::create_conversation(self, options).await
    }

    async fn fork_conversation(
        &self,
        parent_conversation_id: ConversationId,
        at: EntryId,
        options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError> {
        Transaction::fork_conversation(self, parent_conversation_id, at, options).await
    }

    async fn append_entry(
        &self,
        conversation_id: ConversationId,
        value: EntryDraft,
    ) -> Result<EntryRecord, BoxError> {
        Transaction::append_entry(self, conversation_id, value).await
    }

    async fn create_task(
        &self,
        task: &AnyTask,
        input: JsonValue,
        options: TaskOptions,
    ) -> Result<TaskId, BoxError> {
        Transaction::create_task(self, task, input, options).await
    }

    async fn create_task_with(
        &self,
        task: &AnyTask,
        input: JsonValue,
        options: TaskCreationOptions,
    ) -> Result<TaskId, BoxError> {
        Transaction::create_task_with(self, task, input, options).await
    }

    async fn create_submission(
        &self,
        create: SubmissionCreate,
    ) -> Result<SubmissionRecord, BoxError> {
        Transaction::create_submission(self, create).await
    }

    fn settle_submission(&self, id: SubmissionId, settlement: SubmissionSettlement) {
        Transaction::settle_submission(self, id, settlement);
    }

    fn place_submission(&self, id: SubmissionId, entry: EntryId) {
        Transaction::place_submission(self, id, entry);
    }

    async fn doc(&self, target: &DocTarget) -> Result<DocDraft, BoxError> {
        Transaction::doc(self, target).await
    }

    async fn retire_doc(&self, target: &DocTarget) -> Result<(), BoxError> {
        Transaction::retire_doc(self, target).await
    }
}
