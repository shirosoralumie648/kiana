//! `session/session.ts`（1307 行）：Session 内核——一条变更线、文档 tracker 缓存与已提交发布。
//!
//! 结构对应：
//! - [`SessionImpl`] 是公开的 Session（`Session`/`DocumentReader`/`DocumentObserver`），内部状态在
//!   [`SessionInner`]（`Arc`），因此 watcher/计时器只持 `Weak`，不会有引用环。
//! - 变更线是显式的 FIFO 票据队列（[`Line`]）：票据在**调用时**（而不是首次 poll 时）分配，
//!   按调用顺序串行；`commit_with()` 还会在调用时立即把任务交给运行时（排入执行队列），
//!   因此并发创建的提交按调用顺序被接纳。
//! - 需要区分两类入口：`commit_with`/`commit_with_scope` 在**调用时**取票；
//!   `read_on_line`、`snapshot` 的加载与 `unload_documents` 走 `enqueue`，票据在**首次
//!   poll** 时分配。混用时仍按票据分配顺序执行。
//! - TS 的 `queueMicrotask` 在异步监听器（watch 投递）与清扫任务上映射为 `tokio::spawn`；
//!   Session 自己的观察者（`observeCommits`）仍在采纳的同一个同步栈里运行（§8.2 的关键
//!   不变量）。宿主监听器在发布时同步运行。
//! - 失败闩锁只保存错误文本（[`Failure`]）；`SessionEnd`/`WatchEnd` 不是 `Clone`，每次读取用
//!   同样的文本构造新的错误对象（见交付说明）。
//!
//! 文档：TS 的 `Guard<T>`/`track()` 代理在这里是普通 `JsonObject` 值 + [`Tracker`]（见
//! `transaction.rs` 的说明）。
//!
//! # 这个文件负责什么
//!
//! [`crate::session::session::SessionImpl`] 是 durable 包的 Session 内核：它把一次提交拆成
//! “回调在事务上改文档 → Storage 原子持久化 → 同步发布变化”，并保证所有提交与关闭在同一
//! 条 FIFO 变更线上串行。Harness 与任务调度器通过 `read_on_line` / `commit_with` /
//! `commit_transaction` 借用这条串行性；会话视图的内部监听器也在发布时同步运行。
//!
//! # 一次提交的执行顺序
//!
//! 1. `commitWith` 在调用方的同步栈里先做接纳检查：`assert_usable()`（失败闩锁 / 已开始
//!    关闭）与 `deferral()`（`withinMs` 校验）。这些错误以立即失败的 `Future` 返回，不进入
//!    变更线。
//! 2. 取票据并等待成为活动票据；`run_commit()` 再查一次健康状态与上下文中止。
//! 3. 在 `Transaction` 上运行回调。回调通过 `Transaction` 读写草稿，也可能经受保护的
//!    Storage 读取/铸号；写入在 settle 后一次性提交（延迟提交除外，见第 5 步）。
//! 4. `settle_success()` 产出写入列表；空写入不落盘；`read_only` 在 Storage 之前拒绝。
//! 5. 有 `withinMs` 且事务可延迟时进入内存 hold：变化在内存里可见，稍后由计时器或关闭存储。
//! 6. 否则把 holds 里的旧变化并入本批次，调用 `Storage::commit`；`adopt` 回调把新化身装回
//!    缓存，最后 `publish()` 同步通知内部观察者与宿主监听器。
//!
//! # 失败、关闭与缓存
//!
//! - 失败闩锁只有一个错误：第一个胜出，之后所有经 `assert_usable()` 的提交/读取/观察入口
//!   返回 `SessionFailed`；关闭观察接口例外（`close_result()` 在关闭任务无错时返回 `Ok`，
//!   `closed()` 返回 `SessionEnd::Failed`，`failure()`/`document_cache_sizes()` 等只读观察
//!   照常返回）。失败会封闭接纳并立即开始关闭（`fail()` → `begin_close()`）。
//! - `GuardedStorage` 给每次后端调用计数；关闭先拒绝新调用、再等在途调用归零，最后关闭后端。
//! - `documents` 按稳定地址身份缓存 `LoadedDocument`；`attached_documents` 是按化身
//!   `record.id` 的 pin 计数（watch 与延迟提交的 Hold 持有），`idle_documents` 按最后一次
//!   触碰的先后排列，清扫从最旧的一端开始。pin 计数不为零的化身不会被清扫。
//!
//! # 阅读顺序
//!
//! `Line` / `LineTicket`（串行化原语）→ `commit_closure` / `run_commit`（提交主线）→
//! `hold` / `store_holds`（延迟持久化）→ `begin_close` / `finish_close`（关闭）→
//! `Watch`（文档推送状态机）。
//!
//! # 注意事项
//!
//! - 本文件所有 `std::sync::Mutex` 的临界区都不跨 `await`：需要等待时先拷贝出所需数据、
//!   释放锁再 await（见 `load_document`、`publish`、`Watch::drain`）。
//! - 宿主回调（提交监听器、watch 监听器、关闭监听器）用 `contained()`/`catch_unwind` 隔离
//!   panic：内部观察者 panic 让 Session 失败，宿主监听器 panic 只报告 `listener_error`。
//! - 内部观察者（`observe_commits`）在采纳的同步栈里运行，能看到只在内存里采纳的延迟提交；
//!   宿主提交监听器只在 Storage 已持久化后运行（`publish()` 的 `observed`/`stored` 区分）。

use std::any::Any;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use kiana_chord::context::{
    BACKGROUND_CONTEXT, Context, ContextRef, TODO_CONTEXT, abort_error, await_with_context,
    without_abort_signal,
};
use kiana_chord::delta::Op;
use kiana_chord::delta::pending::{PendingOps, PendingTake};
use kiana_telemetry::{SpanAttributes, TelemetrySpan};
use serde_json::Value;

use crate::timers::{Disarm, SetTimer, TimerCallback, Timers, host_timers};
use crate::types::documents::{
    DocDefinition, address_id, check_record_scope, check_record_version, materialize_document,
};
use crate::types::{
    BoxError, CloseListener, CommitChange, CommitListener, CommitOptions, CommitPersistence,
    CommitPublication, ConversationId, ConversationRecord, DocumentAddress, DocumentCommitChange,
    DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, DocumentWatch, EntryId,
    HostCallback, IdKind, JsonObject, ReadOnlySession, ReportInfo, Seq, Session, SessionChange,
    SessionEnd, Storage, StorageWrite, StoredDocument, SubmissionId, TaskRecord, Unsubscribe,
    WatchEnd, WatchHandle, WatchListener,
};
use crate::types::{DocTarget, DocumentObserver, DocumentReader, SessionFailed, Tx};

use crate::session::transaction::{
    AdoptedDocuments, Hold, HoldableChange, LoadedDocument, Tracker, Transaction, TransactionHost,
    TransactionScope, TransactionSettled,
};

// ---------------------------------------------------------------------------
// Retention defaults
// ---------------------------------------------------------------------------

/// 默认文档保留期：十分钟（与保留上下文一致）。
pub const DEFAULT_DOCUMENT_RETENTION_MS: f64 = 600_000.0;

/// 延迟提交的默认 `withinMs`（`settings.progress.persistWithinMs`）。
pub const DEFAULT_PERSIST_WITHIN_MS: f64 = 1_000.0;

/// 默认保留期的全局覆盖（测试用）；`f64` 以位模式存进 `AtomicU64`。
static DEFAULT_RETENTION: LazyLock<AtomicU64> =
    LazyLock::new(|| AtomicU64::new(DEFAULT_DOCUMENT_RETENTION_MS.to_bits()));

/// `setDefaultDocumentRetention()`：改变所有使用默认保留期的 Session（测试用）。
pub fn set_default_document_retention(ms: i64) {
    DEFAULT_RETENTION.store((ms as f64).to_bits(), Ordering::SeqCst);
}

fn default_document_retention() -> f64 {
    f64::from_bits(DEFAULT_RETENTION.load(Ordering::SeqCst))
}

/// `documentRetention()`：给定的毫秒数（0 或更大；`Infinity` 表示永不驱逐），否则默认。
pub fn document_retention(ms: Option<f64>) -> f64 {
    match ms {
        Some(value) if value >= 0.0 => value,
        _ => default_document_retention(),
    }
}

/// `persistWithin()`：给定的毫秒数（0 或更大；`Infinity` 表示等到读写提交或关闭），否则默认。
pub fn persist_within(ms: Option<f64>) -> f64 {
    match ms {
        Some(value) if value >= 0.0 => value,
        _ => DEFAULT_PERSIST_WITHIN_MS,
    }
}

// ---------------------------------------------------------------------------
// Observed commits
// ---------------------------------------------------------------------------

/// Session 自己的组件观察到的一次提交（TS: `ObservedCommit`）。
#[derive(Debug, Clone, PartialEq)]
pub enum ObservedCommit {
    /// 一次已存储的提交。
    Stored {
        /// 提交序号。
        seq: Seq,
        /// 变化。
        changes: Vec<CommitChange>,
    },
    /// 一次只在内存里采纳的延迟提交（只改文档）。
    Memory {
        /// 文档变化。
        changes: Vec<DocumentCommitChange>,
    },
}

impl ObservedCommit {
    /// 判别值：`"stored"` 或 `"memory"`。
    pub fn kind(&self) -> &'static str {
        match self {
            ObservedCommit::Stored { .. } => "stored",
            ObservedCommit::Memory { .. } => "memory",
        }
    }

    /// 已存储提交的序号；内存提交（`Memory`）为 `None`。
    pub fn seq(&self) -> Option<Seq> {
        match self {
            ObservedCommit::Stored { seq, .. } => Some(*seq),
            ObservedCommit::Memory { .. } => None,
        }
    }

    /// 变化列表（统一投影）：`Memory` 的文档变化被包装成 `CommitChange::Document`。
    pub fn changes(&self) -> Vec<CommitChange> {
        match self {
            ObservedCommit::Stored { changes, .. } => changes.clone(),
            ObservedCommit::Memory { changes } => changes
                .iter()
                .cloned()
                .map(CommitChange::Document)
                .collect(),
        }
    }
}

/// Session 内部观察者（`observeCommits()`）。
pub type InternalListener = Arc<dyn Fn(&ObservedCommit, &ContextRef) + Send + Sync>;

// ---------------------------------------------------------------------------
// Reporting hooks
// ---------------------------------------------------------------------------

/// Session 的宿主钩子（Harness 继承 TS `SessionImpl` 的那些点）。
///
/// 普通 Session 用 [`NoopHooks`]；`report()` 丢弃错误，`conversationCreated()` 什么也不做。
#[async_trait]
pub trait SessionHooks: Send + Sync {
    /// 报告一个错误（TS: `report(error, info, context?)`）。
    fn report(&self, _message: &str, _info: ReportInfo, _context: Option<&dyn Context>) {}

    /// 每个创建或 fork 会话的事务里，会话记录暂存之后调用。
    ///
    /// 在事务内部运行，错误让该提交失败（与事务回调的错误相同）。
    async fn conversation_created(
        &self,
        _tx: &Transaction,
        _record: &ConversationRecord,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    /// 关闭封闭接纳之后、变更线关闭 Storage 之前调用；必须不失败。
    ///
    /// TS 契约要求它不得 reject；Rust 侧返回的错误会被忽略（也不会报告）。`close_result()`
    /// 在关闭任务其余步骤出错时失败；调用方等待被取消时也会返回取消错误，即使关闭任务本身
    /// 全部成功。
    async fn before_close(&self) -> Result<(), BoxError> {
        Ok(())
    }

    /// 关闭时、清空缓存之前要直接写入 Storage 的批次（TS: `writeAtClose()`）。
    ///
    /// 在变更线上、held 变化存储之前执行，使用背景上下文；写入不发布（Session 正在关闭），
    /// 已失败的 Session 不会执行。
    fn write_at_close(&self) -> Vec<StorageWrite> {
        Vec::new()
    }
}

/// 普通 Session 的钩子。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopHooks;

impl SessionHooks for NoopHooks {}

// ---------------------------------------------------------------------------
// Options & creation
// ---------------------------------------------------------------------------

/// `createSession()` 的选项（Rust 侧把 TS 的可选参数收进结构体）。
///
/// TS 的构造器接收 `documentRetentionMs: () => number | undefined` 与
/// `persistWithinMs: () => number | undefined`，并在**每次使用**时重新求值（Harness 的
/// `settings.documentRetentionMs` 因此可以随时变化）。Rust 侧用
/// [`SessionOptions::document_retention_ms_source`] 与
/// [`SessionOptions::persist_within_ms_source`] 表达同样的活设置；固定值字段
/// `document_retention_ms` / `persist_within_ms` 保留为便捷写法，活设置优先。
#[derive(Default, Clone)]
pub struct SessionOptions {
    /// 任务生命周期的墙钟；默认 `Date.now()`（`SystemTime`）。
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    /// 固定的文档保留期；缺省用 [`document_retention`] 的默认值。
    pub document_retention_ms: Option<f64>,
    /// 文档保留期的活来源（TS: `documentRetentionMs: () => number | undefined`）；每次使用求值。
    pub document_retention_ms_source: Option<Arc<dyn Fn() -> Option<f64> + Send + Sync>>,
    /// 宿主计时器；缺省 `setTimeout`。
    pub set_timer: Option<SetTimer>,
    /// 固定的延迟提交默认 `withinMs`；缺省 1,000。
    pub persist_within_ms: Option<f64>,
    /// 延迟提交默认窗口的活来源（TS: `persistWithinMs: () => number | undefined`）。
    pub persist_within_ms_source: Option<Arc<dyn Fn() -> Option<f64> + Send + Sync>>,
    /// 拒绝带写入的提交（`HarnessOptions.readOnly`）。
    pub read_only: bool,
    /// 宿主钩子。
    pub hooks: Option<Arc<dyn SessionHooks>>,
}

impl std::fmt::Debug for SessionOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionOptions")
            .field("document_retention_ms", &self.document_retention_ms)
            .field(
                "document_retention_ms_source",
                &self.document_retention_ms_source.is_some(),
            )
            .field("persist_within_ms", &self.persist_within_ms)
            .field(
                "persist_within_ms_source",
                &self.persist_within_ms_source.is_some(),
            )
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl SessionOptions {
    /// 固定的文档保留期（便捷 builder）。
    pub fn with_document_retention(mut self, ms: f64) -> Self {
        self.document_retention_ms = Some(ms);
        self
    }

    /// 活文档保留期（TS: `documentRetentionMs: () => number | undefined`）。
    pub fn with_document_retention_source(
        mut self,
        source: Arc<dyn Fn() -> Option<f64> + Send + Sync>,
    ) -> Self {
        self.document_retention_ms_source = Some(source);
        self
    }

    /// 固定的延迟提交默认窗口（便捷 builder）。
    pub fn with_persist_within(mut self, ms: f64) -> Self {
        self.persist_within_ms = Some(ms);
        self
    }

    /// 活延迟提交默认窗口（TS: `persistWithinMs: () => number | undefined`）。
    pub fn with_persist_within_source(
        mut self,
        source: Arc<dyn Fn() -> Option<f64> + Send + Sync>,
    ) -> Self {
        self.persist_within_ms_source = Some(source);
        self
    }
}

/// `createSession(storage)`：在一个 Storage 后端上打开 Session 内核。
pub fn create_session<S: Storage + 'static>(storage: Arc<S>) -> SessionImpl {
    create_session_with(storage, SessionOptions::default())
}

/// `createSession(storage, options)`。
pub fn create_session_with<S: Storage + 'static>(
    storage: Arc<S>,
    options: SessionOptions,
) -> SessionImpl {
    let storage: Arc<dyn Storage> = storage;
    SessionImpl::new(storage, options)
}

// ---------------------------------------------------------------------------
// Failure latch
// ---------------------------------------------------------------------------

/// Session 的失败记录（TS: `{ error }`）。
#[derive(Debug, Clone)]
pub struct Failure {
    message: String,
}

impl Failure {
    /// 错误文本。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 以同一文本构造一个错误对象。
    pub fn error(&self) -> ReportedError {
        ReportedError(self.message.clone())
    }
}

/// 失败文本的 `Error` 载体（`SessionFailed` 的 cause、`SessionEnd::Failed`）。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ReportedError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
enum EndKind {
    Closed,
    Failed(String),
}

impl EndKind {
    fn to_session_end(&self) -> SessionEnd {
        match self {
            EndKind::Closed => SessionEnd::Closed,
            EndKind::Failed(message) => SessionEnd::Failed {
                error: Box::new(ReportedError(message.clone())),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Session types
// ---------------------------------------------------------------------------

/// 内部缓存条目：已提交文档化身。
type DocumentCache = IndexMap<String, LoadedDocument>;

/// 一次清扫的登记（`#documentSweep`）：`at` 是计划时间（Session 时钟），`disarm` 用于
/// 撤销已装载的计时器。
struct DocumentSweep {
    at: f64,
    disarm: Mutex<Option<Disarm>>,
}

/// 已装载的 hold 计时器：`at` 是目标 deadline，`fired` 表示回调已排入变更线。
struct HoldTimer {
    at: f64,
    fired: AtomicBool,
    disarm: Mutex<Option<Disarm>>,
}

/// 关闭的共享结果：`sender`/`receiver` 广播终态，`job_error` 保存关闭任务自身的错误。
///
/// 两者刻意分开：`SessionEnd` 只由失败闩锁决定，`close_result()` 返回的是关闭任务的错误。
struct SharedClose {
    sender: tokio::sync::watch::Sender<Option<EndKind>>,
    receiver: tokio::sync::watch::Receiver<Option<EndKind>>,
    /// 关闭任务自身的错误（TS: `#closing` 的 rejection）；与失败闩锁是两件事：已经失败的
    /// Session 只要关闭本身不抛错，`close()` 仍然 resolve。
    job_error: Mutex<Option<String>>,
}

impl SharedClose {
    fn new() -> Self {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        SharedClose {
            sender,
            receiver,
            job_error: Mutex::new(None),
        }
    }

    fn finish(&self, outcome: EndKind, job_error: Option<String>) {
        *lock(&self.job_error) = job_error;
        self.sender.send_replace(Some(outcome));
    }

    fn job_error(&self) -> Option<String> {
        lock(&self.job_error).clone()
    }

    async fn wait(&self) -> Option<EndKind> {
        let mut receiver = self.receiver.clone();
        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return Some(outcome);
            }
            if receiver.changed().await.is_err() {
                return None;
            }
        }
    }
}

/// `begin_close()` 的结果：调用者是关闭任务的启动者（`Owner`）还是加入者（`Joiner`）。
enum CloseStart {
    Owner(Arc<SharedClose>),
    Joiner(Arc<SharedClose>),
}

// ---------------------------------------------------------------------------
// Mutation line (TS: the `#tail` promise chain)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct LineState {
    next_ticket: u64,
    /// 当前允许运行的票据；它完成（或被丢弃）后前进。
    serving: u64,
    waiters: BTreeMap<u64, tokio::sync::oneshot::Sender<()>>,
    canceled: HashSet<u64>,
}

fn line_lock(state: &Mutex<LineState>) -> MutexGuard<'_, LineState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn line_advance(state: &mut LineState) {
    while state.serving < state.next_ticket {
        if state.canceled.remove(&state.serving) {
            state.serving += 1;
            continue;
        }
        if let Some(sender) = state.waiters.remove(&state.serving) {
            // 该票据成为活动票据；`serving` 在它完成时前进。
            let _ = sender.send(());
        }
        break;
    }
}

/// Session 的变更线（TS: `#tail`）。
///
/// 票据在创建时分配，因此**调用顺序就是入队顺序**。等待中的票据被丢弃时标记取消，活动票据被
/// 丢弃时立刻让出线——否则一个被取消的提交会让整个 Session 卡死。
///
/// 线上同一时刻至多一张活动票据：`acquire()` 等到自己成为活动票据，`finish()` 把 `serving`
/// 推进到下一张。票据在 `Drop` 时自动完成，因此 future 被丢弃（select/timeout/取消）不会把
/// 线留在活动状态。
pub struct Line {
    state: Mutex<LineState>,
}

impl Line {
    fn new() -> Arc<Line> {
        Arc::new(Line {
            state: Mutex::new(LineState::default()),
        })
    }

    /// 同步分配一张票据（在调用方的那一轮里）。
    fn ticket(self: &Arc<Line>) -> LineTicket {
        let mut state = line_lock(&self.state);
        let id = state.next_ticket;
        state.next_ticket += 1;
        if id == state.serving {
            LineTicket {
                line: Arc::clone(self),
                id,
                active: true,
                receiver: None,
                finished: false,
            }
        } else {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            state.waiters.insert(id, sender);
            LineTicket {
                line: Arc::clone(self),
                id,
                active: false,
                receiver: Some(receiver),
                finished: false,
            }
        }
    }
}

/// 变更线上的一张票据；Drop 时让出线（被取消的提交不会阻塞后续任务）。
struct LineTicket {
    line: Arc<Line>,
    id: u64,
    active: bool,
    receiver: Option<tokio::sync::oneshot::Receiver<()>>,
    finished: bool,
}

impl LineTicket {
    /// 等到这张票据成为活动票据。
    async fn acquire(&mut self) {
        if self.active {
            return;
        }
        if let Some(receiver) = self.receiver.take() {
            // 发送端只会随 `Line` 一起消失；那时线已不可用，按已获得处理。
            let _ = receiver.await;
            self.active = true;
        }
    }

    /// 完成（或放弃）这张票据，把线交给下一张。
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut state = line_lock(&self.line.state);
        if self.id == state.serving {
            state.serving += 1;
            line_advance(&mut state);
        } else {
            // 还排在后面：标记取消，轮到它时跳过。
            state.canceled.insert(self.id);
            state.waiters.remove(&self.id);
        }
    }
}

impl Drop for LineTicket {
    fn drop(&mut self) {
        self.finish();
    }
}

/// 内部状态。`SessionImpl` 只是它的 `Arc` 包装。
///
/// 这是实现细节：字段全部私有，外部不能构造，只能通过 [`SessionImpl`] 的方法观察它。用
/// `Arc` 而不是借用，是因为 watch 的内部监听器与清扫/hold 计时器只持 `Weak`（见
/// [`SessionImpl::new`] 的 `Arc::new_cyclic`）；spawn 出的关闭任务在完成前持有 `Arc`
/// （`close_result` 与 `fail` 启动时都 `Arc::clone` 了本类型），但 `SessionInner` 不持有该
/// 任务，因此仍不会形成引用环。
///
/// 不变量：
/// - `documents` 以稳定地址身份为键；缓存条目存在时，它的 `record.id` 是当前化身。
/// - `idle_documents` 只包含未被 pin 的缓存条目，按“最后一次触碰”的先后排列（最旧在前）。
/// - `attached_documents` 按化身 `record.id` 计数；`holds` 里的每个 `Hold` 都对应一次
///   `pin_document`，`unpin` 只执行一次。
/// - `failure` 只会从 `None` 变为 `Some`，此后不再清除（关闭也不会清）。
/// - 变更线上至多一张活动票据；票据按分配顺序执行（`commit_closure` 在调用时分配，
///   `enqueue` 在首次 poll 时分配）。
///
/// 状态锁用 `lock()`（中毒视为未中毒）；`DocumentSweep`/`HoldTimer` 的 `disarm` 槽直接
/// 加锁并同样忽略中毒。本类型的方法不跨 `await` 持锁。
pub struct SessionInner {
    storage: Arc<GuardedStorage>,
    documents: Mutex<DocumentCache>,
    idle_documents: Mutex<IndexMap<String, f64>>,
    attached_documents: Mutex<IndexMap<crate::types::DocumentId, usize>>,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
    /// TS 的 `#host.now`：原始时钟提供者（错误向提交传播；只有缓存记账用 [`SessionInner::clock`]）。
    raw_now: Box<dyn Fn() -> f64 + Send + Sync>,
    document_retention_ms: Box<dyn Fn() -> f64 + Send + Sync>,
    persist_within_ms: Box<dyn Fn() -> f64 + Send + Sync>,
    holds: Mutex<IndexMap<crate::types::DocumentId, Arc<Mutex<Hold>>>>,
    hold_timer: Mutex<Option<Arc<HoldTimer>>>,
    read_only: bool,
    timers: Timers,
    document_sweep: Mutex<Option<Arc<DocumentSweep>>>,
    sweep_jobs: AtomicUsize,
    document_timers_unavailable: AtomicBool,
    commit_listeners: Mutex<Vec<(u64, CommitListener)>>,
    internal_listeners: Mutex<Vec<(u64, InternalListener)>>,
    close_listeners: Mutex<Vec<(u64, CloseListener)>>,
    next_listener_id: AtomicU64,
    /// 变更线（FIFO 票据队列）。
    line: Arc<Line>,
    /// 关闭已开始（TS: `#closing !== undefined`）。
    closing: std::sync::Mutex<Option<Arc<SharedClose>>>,
    failure: std::sync::Mutex<Option<Failure>>,
    /// 失败时立即落定、永不 resolve 的等待（TS: `failed`）。
    failed: tokio::sync::watch::Sender<Option<()>>,
    /// 关闭终态广播（`closed()` 每次调用订阅）；`None` 表示尚未关闭。
    closed: tokio::sync::watch::Sender<Option<EndKind>>,
    hooks: Arc<dyn SessionHooks>,
}

/// Session 内核（TS: `SessionImpl`）。
#[derive(Clone)]
pub struct SessionImpl {
    inner: Arc<SessionInner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

fn panic_error(payload: Box<dyn Any + Send>) -> BoxError {
    match payload.downcast::<String>() {
        Ok(message) => Box::new(ReportedError(*message)),
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => Box::new(ReportedError((*message).to_owned())),
            Err(_) => Box::new(ReportedError("callback panicked".to_owned())),
        },
    }
}

/// 运行一个不能失败其调用方（也不让 panic 逃逸）的宿主回调。
pub fn contained<F, E>(callback: F, on_error: E)
where
    F: FnOnce(),
    E: FnOnce(BoxError),
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)) {
        Ok(()) => {}
        Err(payload) => on_error(panic_error(payload)),
    }
}

/// Session 各入口接受的上下文形态。
///
/// TS 的 `Context` 是引用语义，Session 保存的是同一个对象，因此 `context.value(key)` 在
/// 提交、Storage 调用与 watch 投递里始终可见。Rust 的 `&dyn Context` 无法复制，也**无法**
/// 枚举其不透明值（`Context` trait 只按 key 取值），所以：
/// - 传入 [`ContextRef`]（或它的引用）时保留同一对象（引用语义，调用方看到同一上下文的值）；
/// - 传入 `&dyn Context` 时退化为 [`SnapshotContext`]（保留中止信号与 telemetry，不透明值
///   不可见）。需要值保持的调用方（Harness、测试）应传 `ContextRef`。
pub trait ContextSource {
    /// 升级为可跨 `await` 的共享句柄。
    fn context_ref(&self) -> ContextRef;
}

impl ContextSource for Arc<dyn Context> {
    fn context_ref(&self) -> ContextRef {
        Arc::clone(self)
    }
}

impl ContextSource for &Arc<dyn Context> {
    fn context_ref(&self) -> ContextRef {
        Arc::clone(self)
    }
}

impl ContextSource for &dyn Context {
    fn context_ref(&self) -> ContextRef {
        context_handle(*self)
    }
}

/// 把借用上下文提升为可跨 `await` 的共享句柄。
///
/// 静态背景上下文直接复制 `Arc`；其它上下文由 [`SnapshotContext`] 代理（保留中止信号与
/// telemetry，见 [`ContextSource`] 的说明）。
pub fn context_handle(context: &dyn Context) -> ContextRef {
    let pointer = context as *const dyn Context;
    if std::ptr::addr_eq(pointer, BACKGROUND_CONTEXT.as_ref() as *const dyn Context) {
        return Arc::clone(&BACKGROUND_CONTEXT);
    }
    if std::ptr::addr_eq(pointer, TODO_CONTEXT.as_ref() as *const dyn Context) {
        return Arc::clone(&TODO_CONTEXT);
    }
    Arc::new(SnapshotContext {
        abort_signal: context.abort_signal(),
        telemetry: context.telemetry(),
        name: context.to_string_value(),
    })
}

/// 借用上下文的异步快照（不携带不透明值）。
pub struct SnapshotContext {
    abort_signal: Option<kiana_chord::context::AbortSignal>,
    telemetry: Arc<dyn TelemetrySpan>,
    name: String,
}

impl std::fmt::Debug for SnapshotContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotContext")
            .field("name", &self.name)
            .field(
                "aborted",
                &self.abort_signal.as_ref().is_some_and(|s| s.is_aborted()),
            )
            .finish()
    }
}

impl Context for SnapshotContext {
    fn abort_signal(&self) -> Option<kiana_chord::context::AbortSignal> {
        self.abort_signal.clone()
    }

    fn telemetry(&self) -> Arc<dyn TelemetrySpan> {
        Arc::clone(&self.telemetry)
    }

    fn value(
        &self,
        _key: &kiana_chord::context::KeyIdentity,
    ) -> Option<kiana_chord::context::ContextValue> {
        None
    }

    fn to_string_value(&self) -> String {
        self.name.clone()
    }
}

/// `Session.documentCacheSizes`（Internal, for tests）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentCacheSizes {
    /// 缓存里的文档数。
    pub cached: usize,
    /// 空闲文档数。
    pub idle: usize,
    /// 被 pin 的化身数。
    pub attached: usize,
    /// 已排队的清扫任务数（自创建以来的累计值：`run_sweep_job` 只递增，从不清零）。
    pub sweeps: usize,
    /// 有 held 变化的文档数。
    pub held: usize,
}

impl SessionImpl {
    /// 在一个 Storage 后端上打开 Session。
    ///
    /// 三个宿主闭包在这里被固定下来：`now` 在缓存记账路径上 panic 会退回系统墙钟，而事务的
    /// `now()` 会把 panic 转成提交错误（见 `SessionHost::now`）；两个 `*_source` 每次使用时
    /// 重新求值，panic 视为返回 `None`，从而退回默认值。`Arc::new_cyclic` 让计时器错误报告与
    /// `GuardedStorage` 在 `SessionInner` 建好之前就能持有 `Weak`。
    pub fn new(storage: Arc<dyn Storage>, options: SessionOptions) -> Self {
        let (failed, _) = tokio::sync::watch::channel(None);
        let (closed, _) = tokio::sync::watch::channel(None);
        let options_clock = options.now.clone();
        let clock: Box<dyn Fn() -> f64 + Send + Sync> = Box::new(move || match &options_clock {
            Some(now) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| now())) {
                Ok(value) => value,
                Err(_) => now_millis(),
            },
            None => now_millis(),
        });
        let raw_clock = options.now.clone();
        // TS: `#host.now` 是原始提供者——它抛出的错误让提交失败，不会被替换成墙钟。
        let raw_now: Box<dyn Fn() -> f64 + Send + Sync> = Box::new(move || match &raw_clock {
            Some(now) => now(),
            None => now_millis(),
        });
        // 与 TS 相同：保留期在**每次使用**时重新求值（Harness 的活设置），抛错按下限处理。
        let retention = options.document_retention_ms;
        let retention_source = options.document_retention_ms_source.clone();
        let document_retention_ms: Box<dyn Fn() -> f64 + Send + Sync> = Box::new(move || {
            let ms = match &retention_source {
                Some(source) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source()))
                    .unwrap_or(None),
                None => retention,
            };
            document_retention(ms)
        });
        let within = options.persist_within_ms;
        let within_source = options.persist_within_ms_source.clone();
        let persist_within_ms: Box<dyn Fn() -> f64 + Send + Sync> = Box::new(move || {
            let ms = match &within_source {
                Some(source) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source()))
                    .unwrap_or(None),
                None => within,
            };
            persist_within(ms)
        });
        let hooks = options.hooks.clone().unwrap_or_else(|| Arc::new(NoopHooks));
        let inner = Arc::new_cyclic(|weak: &Weak<SessionInner>| {
            // 宿主 `setTimer` 的失败按 TS 报告为 `host_error`/`setTimer`。
            let timer_weak = weak.clone();
            let timers = host_timers(
                options.set_timer.clone(),
                Arc::new(move |error: BoxError| {
                    if let Some(inner) = timer_weak.upgrade() {
                        inner.report_with(
                            &error.to_string(),
                            ReportInfo::HostError {
                                callback: HostCallback::SetTimer,
                                task: None,
                            },
                            None,
                        );
                    }
                }),
            );
            let guarded = Arc::new(GuardedStorage::new(Arc::clone(&storage), weak.clone()));
            SessionInner {
                storage: guarded,
                documents: Mutex::new(IndexMap::new()),
                idle_documents: Mutex::new(IndexMap::new()),
                attached_documents: Mutex::new(IndexMap::new()),
                clock,
                raw_now,
                document_retention_ms,
                persist_within_ms,
                holds: Mutex::new(IndexMap::new()),
                hold_timer: Mutex::new(None),
                read_only: options.read_only,
                timers,
                document_sweep: Mutex::new(None),
                sweep_jobs: AtomicUsize::new(0),
                document_timers_unavailable: AtomicBool::new(false),
                commit_listeners: Mutex::new(Vec::new()),
                internal_listeners: Mutex::new(Vec::new()),
                close_listeners: Mutex::new(Vec::new()),
                next_listener_id: AtomicU64::new(0),
                line: Line::new(),
                closing: std::sync::Mutex::new(None),
                failure: std::sync::Mutex::new(None),
                failed,
                closed,
                hooks,
            }
        });
        SessionImpl { inner }
    }

    /// 内部：错误闩锁的记录。
    pub fn failure(&self) -> Option<Failure> {
        lock(&self.inner.failure).clone()
    }

    /// 内部：缓存规模。
    pub fn document_cache_sizes(&self) -> DocumentCacheSizes {
        DocumentCacheSizes {
            cached: lock(&self.inner.documents).len(),
            idle: lock(&self.inner.idle_documents).len(),
            attached: lock(&self.inner.attached_documents).len(),
            sweeps: self.inner.sweep_jobs.load(Ordering::SeqCst),
            held: lock(&self.inner.holds).len(),
        }
    }

    /// 内部：Storage 背后的受保护句柄。
    pub fn storage(&self) -> &Arc<GuardedStorage> {
        &self.inner.storage
    }

    /// 内部：失败等待：Session 失败后立即落定（包括失败之后才开始等待的调用方）。
    pub async fn failed(&self) {
        let mut receiver = self.inner.failed.subscribe();
        if receiver.borrow_and_update().is_some() {
            return;
        }
        let _ = receiver.changed().await;
    }

    /// 内部：让 Session 失败（第一个错误胜出），并立即开始关闭。
    pub fn fail(&self, error: BoxError) {
        SessionInner::fail(&self.inner, error, None);
    }

    /// 内部：带调用方上下文的 [`SessionImpl::fail`]（TS: `fail(error, context)`）。
    pub fn fail_with_context(&self, error: BoxError, context: &dyn Context) {
        SessionInner::fail(&self.inner, error, Some(context));
    }

    /// 在变更线上运行一个只读任务。
    ///
    /// 首次 poll 时先做可接纳性检查，再把 `assert_healthy` 与任务一起排进变更线，因此任务
    /// 看到的是此前已接纳提交完成后的状态。任务自身返回的错误原样传出，不会因此让 Session
    /// 失败；任务若经过 [`GuardedStorage`] 调用后端，守卫会按 Storage 的规则处理错误。
    pub async fn read_on_line<T, F>(&self, job: F) -> Result<T, BoxError>
    where
        F: Future<Output = Result<T, BoxError>>,
    {
        self.assert_usable()?;
        self.inner
            .enqueue(async {
                self.inner.assert_healthy()?;
                job.await
            })
            .await
    }

    /// 一个会话文档的当前化身与值，供已经在变更线上的任务使用。
    ///
    /// 返回 `(记录, valueVersion, tracker 当前值)`；文档不存在时为 `None`。调用方必须已经
    /// 在变更线上——本方法直接加载，不再取票据。记录的作用域或版本与定义不符时返回错误，
    /// 检查与 `snapshot()` 相同。
    pub async fn conversation_document_on_line<C: ContextSource>(
        &self,
        target: &DocTarget,
        context: C,
    ) -> Result<Option<(DocumentRecord, i64, JsonObject)>, BoxError> {
        let ctx = context.context_ref();
        let definition = Arc::clone(&target.definition);
        let address = target.address();
        let id = target.address_id();
        let Some(loaded) = self
            .inner
            .load_document(definition.clone(), &id, address, ctx)
            .await?
        else {
            return Ok(None);
        };
        check_record_scope(&definition, &loaded.record)?;
        check_record_version(&definition, &loaded.record, loaded.stored_version())?;
        Ok(Some((
            loaded.record.clone(),
            loaded.value_version,
            loaded.tracker.value(),
        )))
    }

    /// `commitWith()`：在变更线上运行一个使用具体 [`Transaction`] 的回调。
    ///
    /// 可接纳性检查与 `withinMs` 参数在**调用时**完成（错误以已失败的 `Future` 返回），
    /// 任务随即取票据并交给运行时，因此并发发起的提交按调用顺序被接纳。返回值被擦除为
    /// `Any` 后再 downcast 回 `T`；类型不匹配时返回 `TransactionSettled`。
    pub fn commit_with<T, F, C: ContextSource>(
        &self,
        change: F,
        context: C,
        options: CommitOptions,
    ) -> BoxFuture<'static, Result<T, BoxError>>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a Transaction) -> BoxFuture<'a, Result<T, BoxError>> + Send + 'static,
    {
        self.commit_closure(change, context.context_ref(), None, options)
    }

    /// 内部：带 [`TransactionScope`] 的提交（Harness 用）。
    ///
    /// `scope` 是事务的默认作用域：`Tx.create_task()` 的默认会话与追加条目的归属，见
    /// [`TransactionScope`] 的说明。
    pub fn commit_with_scope<T, F, C: ContextSource>(
        &self,
        change: F,
        context: C,
        scope: TransactionScope,
        options: CommitOptions,
    ) -> BoxFuture<'static, Result<T, BoxError>>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a Transaction) -> BoxFuture<'a, Result<T, BoxError>> + Send + 'static,
    {
        self.commit_closure(change, context.context_ref(), Some(scope), options)
    }

    /// `commitWith()` 的实现：与 TS 一样，可接纳性检查与延迟参数在**调用方的同步栈**里完成，然后
    /// 立刻把任务排到变更线上（而不是等首次 poll），因此并发创建的提交按调用顺序被接纳。
    fn commit_closure<T, F>(
        &self,
        change: F,
        context: ContextRef,
        scope: Option<TransactionScope>,
        options: CommitOptions,
    ) -> BoxFuture<'static, Result<T, BoxError>>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a Transaction) -> BoxFuture<'a, Result<T, BoxError>> + Send + 'static,
    {
        // TS 的 `commitWith()` 把这两个失败变成 rejected promise，而不是同步抛出。
        let checked = self.assert_usable().and_then(|()| self.deferral(&options));
        let within = match checked {
            Ok(within) => within,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let adapter = Arc::new(ClosureChange {
            change: Mutex::new(Some(change)),
            marker: std::marker::PhantomData,
        });
        let inner = Arc::clone(&self.inner);
        let scope = scope.unwrap_or_default();
        let job = async move {
            let value = inner.run_commit(&*adapter, context, scope, within).await?;
            value
                .downcast::<T>()
                .map(|value| *value)
                .map_err(|_| Box::new(TransactionSettled) as BoxError)
        };
        self.inner.enqueue_eager(job)
    }

    /// 内部：用 [`TransactionChange`] 运行一次提交。
    ///
    /// 与 [`SessionImpl::commit_with`] 不同，它不 spawn，票据在首次 poll 时分配。回调结果以
    /// `Box<dyn Any + Send>` 交给调用方，由调用方 downcast。
    pub async fn commit_transaction<C: ContextSource>(
        &self,
        change: &dyn TransactionChange,
        context: C,
        scope: Option<TransactionScope>,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        self.assert_usable()?;
        let within = self.deferral(&options)?;
        let ctx = context.context_ref();
        self.inner
            .enqueue(
                self.inner
                    .run_commit(change, ctx, scope.unwrap_or_default(), within),
            )
            .await
    }

    /// 解释 `CommitOptions.persistence`：立即提交返回 `None`；延迟提交返回 `withinMs`，
    /// 缺省时求值活的 `persist_within_ms` 来源。
    ///
    /// 负数在这里被拒绝（TS 的 `TypeError`），发生在取票据之前。`0` 合法：可 hold 的事务把
    /// 变化留在内存，并由已到期的 hold 计时器尽快落盘；不可 hold 的提交仍立即存储。
    fn deferral(&self, options: &CommitOptions) -> Result<Option<f64>, BoxError> {
        let Some(CommitPersistence::Deferred { within_ms }) = options.persistence else {
            return Ok(None);
        };
        match within_ms {
            None => Ok(Some(persist_within(Some((self.inner.persist_within_ms)())))),
            Some(ms) if ms >= 0.0 => Ok(Some(ms)),
            Some(_) => Err(Box::new(SessionTypeError::new(
                "CommitPersistence.withinMs must be a number of milliseconds, 0 or more",
            ))),
        }
    }

    /// 失败返回 `SessionFailed`（含首个错误），否则关闭已开始返回 `SessionClosed`。
    fn assert_usable(&self) -> Result<(), BoxError> {
        self.inner.assert_healthy()?;
        if lock(&self.inner.closing).is_some() {
            return Err(Box::new(SessionClosed));
        }
        Ok(())
    }

    /// `snapshot()`：当前值。
    ///
    /// 缓存命中且 `valueVersion` 与定义版本一致时同步读取并触碰缓存；否则把加载任务排进
    /// 变更线。返回 `None` 表示文档不存在；记录作用域/版本与定义不符时返回错误。
    pub async fn snapshot<C: ContextSource>(
        &self,
        target: &DocTarget,
        context: C,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.assert_usable()?;
        let definition = Arc::clone(&target.definition);
        let address = target.address();
        let id = target.address_id();
        let cached = {
            let docs = lock(&self.inner.documents);
            docs.get(&id)
                .filter(|document| document.value_version == definition.version)
                .cloned()
        };
        let loaded = match cached {
            Some(cached) => {
                self.inner.touch_document(&id);
                Some(cached)
            }
            None => {
                let ctx = context.context_ref();
                self.inner
                    .enqueue(
                        self.inner
                            .load_document(definition.clone(), &id, address, ctx),
                    )
                    .await?
            }
        };
        let Some(loaded) = loaded else {
            return Ok(None);
        };
        check_record_scope(&definition, &loaded.record)?;
        check_record_version(&definition, &loaded.record, loaded.stored_version())?;
        Ok(Some(loaded.tracker.value()))
    }

    /// `watchDoc()`：观察一份已提交文档化身。
    ///
    /// 返回的 watch 在 `start()` 之前只累积最新一帧。加载期间上下文被取消时返回错误而不是
    /// watch；加载完成但信号已中止时先取消 watch 再报错（与 TS 的 `watchDoc` 一致）。终止
    /// （停止/取消/退役/关闭）会通过 detach 解绑内部监听器并解除缓存 pin。
    pub async fn watch_doc<C: ContextSource>(
        &self,
        target: &DocTarget,
        context: C,
    ) -> Result<Option<DocumentWatch>, BoxError> {
        self.assert_usable()?;
        let definition = Arc::clone(&target.definition);
        let address = target.address();
        let id = target.address_id();
        let ctx = context.context_ref();
        let signal = ctx.abort_signal();
        // TS 在入队**之前**注册 abort 监听（`signal.addEventListener`），取消落在加载窗口里也
        // 不会丢。`on_abort` 与 DOM 一样不会对已中止的信号补发，因此先检查一次。
        let cancelled = Arc::new(AtomicBool::new(
            signal.as_ref().is_some_and(|signal| signal.is_aborted()),
        ));
        let subscription = signal.as_ref().map(|signal| {
            let cancelled = Arc::clone(&cancelled);
            signal.on_abort(move |_reason| cancelled.store(true, Ordering::SeqCst))
        });
        let is_cancelled = || cancelled.load(Ordering::SeqCst);
        let watch = self
            .inner
            .enqueue(async {
                self.inner.assert_healthy()?;
                if is_cancelled() {
                    return Err(cancellation_error(signal.as_ref()) as BoxError);
                }
                let loaded = self
                    .inner
                    .load_document(definition.clone(), &id, address, ctx.clone())
                    .await?;
                if is_cancelled() {
                    return Err(cancellation_error(signal.as_ref()) as BoxError);
                }
                let Some(loaded) = loaded else {
                    return Ok(None);
                };
                self.inner
                    .attach_document(definition.clone(), loaded)
                    .map(Some)
            })
            .await?;
        let Some(watch) = watch else {
            return Ok(None);
        };
        if is_cancelled() {
            watch.cancel();
            drop(subscription);
            return Err(cancellation_error(signal.as_ref()) as BoxError);
        }
        if let Some(signal) = &signal {
            watch.observe_cancellation(signal);
        }
        drop(subscription);
        Ok(Some(Arc::new(watch) as DocumentWatch))
    }

    /// `snapshotAsOf()`：某个可见条目处的值（只支持会话文档）。
    ///
    /// 非会话作用域返回 `SessionTypeError`。条目从该会话不可见、或历史文档存在但内容读不到
    /// 时返回错误；该历史位置没有这份文档时返回 `None`。读取在变更线上进行。
    pub async fn snapshot_as_of<C: ContextSource>(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: C,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.assert_usable()?;
        let definition = Arc::clone(&target.definition);
        let address = target.address();
        let DocumentScope::Conversation { conversation_id } = address.scope else {
            return Err(Box::new(SessionTypeError::new(
                "Session.snapshotAsOf() requires a conversation document",
            )));
        };
        let ctx = context.context_ref();
        self.inner
            .enqueue(async {
                self.inner.assert_healthy()?;
                let storage = self.inner.storage.as_ref();
                let Some(stored_entry) = storage.visible_entry(conversation_id, at, &*ctx).await?
                else {
                    return Err(io::Error::other(format!(
                        "Entry {at} is not visible from conversation {conversation_id}"
                    ))
                    .into());
                };
                let historical_address = DocumentAddress {
                    kind: address.kind.clone(),
                    scope: DocumentScope::Conversation {
                        conversation_id: stored_entry.entry.conversation_id,
                    },
                    key: address.key.clone(),
                };
                let recorded_at = DocumentPoint::At(stored_entry.commit_seq);
                let Some(record) = storage
                    .find_document(&historical_address, recorded_at, &*ctx)
                    .await?
                else {
                    return Ok(None);
                };
                let Some(stored) = storage.document(record.id, recorded_at, &*ctx).await? else {
                    return Err(io::Error::other(format!(
                        "Historical document {} ({}) cannot be read",
                        record.id, record.kind
                    ))
                    .into());
                };
                materialize_document(&definition, &stored)
                    .map(Some)
                    .map_err(Into::into)
            })
            .await
    }

    /// `unloadDocuments()`：丢掉全部已载入 tracker（先存 held 变化）。
    ///
    /// 只清空 `documents` 与 `idle_documents`；`holds` 与 pin 计数不动（被持有的化身由
    /// `Hold` 自己保活）。失败状态下跳过存储，但仍清空缓存。
    pub async fn unload_documents(&self) -> Result<(), BoxError> {
        self.inner
            .enqueue(async {
                if self.inner.failure().is_none() {
                    let holds = self.inner.holds_snapshot();
                    self.inner
                        .store_holds(holds, Arc::clone(&BACKGROUND_CONTEXT))
                        .await?;
                }
                lock(&self.inner.documents).clear();
                lock(&self.inner.idle_documents).clear();
                Ok(())
            })
            .await
    }

    /// `subscribeCommits()`。
    ///
    /// 监听器在 Storage 提交成功后同步运行（`publish()` 或 `store_holds()`），panic 被
    /// `contained()` 捕获并作为 `listener_error` 报告。返回的注销函数用 `Weak` 回指
    /// Session，重复调用无副作用；Session 已失败或关闭时注册返回错误。
    pub fn subscribe_commits(&self, listener: CommitListener) -> Result<Unsubscribe, BoxError> {
        self.assert_usable()?;
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.commit_listeners).push((id, listener));
        let weak = Arc::downgrade(&self.inner);
        Ok(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                lock(&inner.commit_listeners).retain(|(candidate, _)| *candidate != id);
            }
        }))
    }

    /// `observeCommits()`（内部）。
    ///
    /// 与 [`SessionImpl::subscribe_commits`] 不同，内部监听器还会看到只在内存里采纳的延迟
    /// 提交（`ObservedCommit::Memory`），并在采纳的同一个同步栈里运行。它的 panic 会让
    /// Session 失败（内部状态不能与内存分叉），而不是只报告。
    pub fn observe_commits(&self, listener: InternalListener) -> Result<Unsubscribe, BoxError> {
        self.assert_usable()?;
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.internal_listeners).push((id, listener));
        let weak = Arc::downgrade(&self.inner);
        Ok(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                lock(&inner.internal_listeners).retain(|(candidate, _)| *candidate != id);
            }
        }))
    }

    /// `subscribeClose()`。
    ///
    /// 关闭开始时 `begin_close()` 会同步取走并运行全部关闭监听器（失败关闭也运行，此时
    /// `failure()` 已可见）；panic 作为 `listener_error` 报告，其余监听器继续运行。之后再
    /// 注销是空操作。
    pub fn subscribe_close(&self, listener: CloseListener) -> Result<Unsubscribe, BoxError> {
        self.assert_usable()?;
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.close_listeners).push((id, listener));
        let weak = Arc::downgrade(&self.inner);
        Ok(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                lock(&inner.close_listeners).retain(|(candidate, _)| *candidate != id);
            }
        }))
    }

    /// `close()`：封闭接纳，结清已接纳的提交，然后关闭 Storage。
    pub async fn close<C: ContextSource>(&self, context: C) {
        let _ = self.close_result(context).await;
    }

    /// `close()` 的可观察错误版本。
    ///
    /// TS 的 `close()` 返回 `#closing`：关闭任务自身的错误（held 存储失败、`storage.close()`
    /// 抛错、`beforeClose()`）让每个等待者都 reject；已经失败的 Session 只要关闭本身不抛错，
    /// `close()` 仍然 resolve（失败由 `closed()`/`failure()` 观察）。等待期间调用方上下文被
    /// 取消只让这次等待失败，关闭继续。
    ///
    /// 只有第一个调用者（或 `fail()`）负责启动关闭任务；其余调用者只是加入等待。没有 Tokio
    /// 运行时时，关闭任务在第一个调用者的栈上直接运行，等待者随后立即拿到结果。
    pub async fn close_result<C: ContextSource>(&self, context: C) -> Result<(), BoxError> {
        let context = context.context_ref();
        let share = match self.inner.begin_close() {
            CloseStart::Joiner(share) => share,
            CloseStart::Owner(share) => {
                // `cleanup`：TS 从**第一个**调用 `close()` 的上下文派生（只去掉 abort signal）。
                let cleanup = without_abort_signal(Arc::clone(&context));
                // 关闭任务与等待分离：等待者被取消时关闭仍要完成。
                let inner = Arc::clone(&self.inner);
                let job_share = Arc::clone(&share);
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        handle.spawn(async move {
                            inner.finish_close(job_share, cleanup).await;
                        });
                    }
                    Err(_) => self.inner.finish_close(share.clone(), cleanup).await,
                }
                share
            }
        };
        match await_with_context(share.wait(), &*context).await {
            Ok(_) => match share.job_error() {
                Some(message) => Err(Box::new(ReportedError(message))),
                None => Ok(()),
            },
            Err(abort) => Err(Box::new(abort)),
        }
    }

    /// 内部：关闭是否已开始。
    pub fn is_closing(&self) -> bool {
        lock(&self.inner.closing).is_some()
    }
}

/// 一次提交回调（具体 `Transaction`）。
///
/// 与 [`SessionChange`] 的区别是回调直接拿到具体类型 [`Transaction`]（Harness 需要事务的
/// 内部操作）。`Session::commit` 会把 `&dyn Tx` 适配成这个 trait；返回的值被擦除成
/// `Box<dyn Any + Send>`，由调用方 downcast。
#[async_trait]
pub trait TransactionChange: Send + Sync {
    /// 在事务上运行。
    ///
    /// 返回错误只让本次提交失败并回滚草稿；它本身不会把 Session 置为失败（回调若吞掉了
    /// 失败的 Storage 读取，`run_commit` 的复查会发现）。返回值经 downcast 交给调用方。
    async fn run(&self, tx: &Transaction) -> Result<Box<dyn Any + Send>, BoxError>;
}

struct ClosureChange<F, T> {
    change: Mutex<Option<F>>,
    marker: std::marker::PhantomData<fn() -> T>,
}

#[async_trait]
impl<F, T> TransactionChange for ClosureChange<F, T>
where
    F: for<'a> FnOnce(&'a Transaction) -> BoxFuture<'a, Result<T, BoxError>> + Send,
    T: Send + 'static,
{
    async fn run(&self, tx: &Transaction) -> Result<Box<dyn Any + Send>, BoxError> {
        let change = lock(&self.change)
            .take()
            .ok_or_else(|| Box::new(TransactionSettled) as BoxError)?;
        Ok(Box::new(change(tx).await?))
    }
}

/// Session 已封闭接纳（TS: `Error("Session is closed")`）。
#[derive(Debug, Clone, thiserror::Error)]
#[error("Session is closed")]
pub struct SessionClosed;

/// 参数类型错误的等价物（消息是当前稳定契约，改动需同步测试）。
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct SessionTypeError {
    /// 完整消息。
    pub message: String,
}

impl SessionTypeError {
    /// 新建。
    pub fn new(message: impl Into<String>) -> Self {
        SessionTypeError {
            message: message.into(),
        }
    }
}

fn cancellation_error(signal: Option<&kiana_chord::context::AbortSignal>) -> BoxError {
    match signal {
        Some(signal) => Box::new(abort_error(signal)),
        None => Box::new(ReportedError("The operation was aborted".to_owned())),
    }
}

// ---------------------------------------------------------------------------
// SessionInner
// ---------------------------------------------------------------------------

impl SessionInner {
    fn clock(&self) -> f64 {
        (self.clock)()
    }

    fn retention(&self) -> f64 {
        (self.document_retention_ms)()
    }

    fn failure(&self) -> Option<Failure> {
        lock(&self.failure).clone()
    }

    /// 失败时返回携带首个错误的 `SessionFailed`，否则 `Ok(())`。
    fn assert_healthy(&self) -> Result<(), BoxError> {
        match lock(&self.failure).clone() {
            Some(failure) => Err(Box::new(SessionFailed::new(failure.error()))),
            None => Ok(()),
        }
    }

    /// 记录首个错误、唤醒 `failed()` 等待者、封闭接纳并报告。
    ///
    /// 第一个错误胜出：之后调用直接返回，不覆盖原因也不重复报告。关闭监听器在
    /// `begin_close()` 里同步运行；关闭任务在 Tokio 运行时的后台任务中继续。没有 Tokio
    /// 运行时时不会 spawn 关闭任务（`fail()` 只完成到 `begin_close()`）。
    fn fail(self: &Arc<Self>, error: BoxError, context: Option<&dyn Context>) {
        let message = error.to_string();
        {
            let mut slot = lock(&self.failure);
            if slot.is_some() {
                return;
            }
            *slot = Some(Failure {
                message: message.clone(),
            });
        }
        let _ = self.failed.send(Some(()));
        // 先封闭，让报告处理器回调时看到失败状态；关闭监听器同步运行。
        let owner = match self.begin_close() {
            CloseStart::Owner(share) => Some(share),
            CloseStart::Joiner(_) => None,
        };
        self.report_with(&message, ReportInfo::SessionFailed, context);
        if let Some(share) = owner {
            let inner = Arc::clone(self);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    // TS `fail()` 用 `this.close(BACKGROUND_CONTEXT)`，cleanup 也是背景上下文。
                    inner
                        .finish_close(share, Arc::clone(&BACKGROUND_CONTEXT))
                        .await;
                });
            }
        }
    }

    /// 内部：宿主报告（钩子；普通 Session 丢弃）。`message` 是 TS `report(error)` 的 error 文本。
    fn report_with(&self, message: &str, info: ReportInfo, context: Option<&dyn Context>) {
        self.hooks.report(message, info, context);
    }

    /// 报告一个监听器错误：TS `this.report(error[, LISTENER, context])`。
    fn report_listener(&self, error: &BoxError, context: Option<&dyn Context>) {
        self.report_with(&error.to_string(), ReportInfo::ListenerError, context);
    }

    // ─── Close ──────────────────────────────────────────────────────────────

    /// 封闭接纳并返回关闭共享句柄：第一个调用者成为 Owner，其余成为 Joiner。
    ///
    /// Owner 在这里同步撤销文档清扫与 hold 计时器，取走并逐个运行关闭监听器（panic 作为
    /// `listener_error` 报告，不阻止其它监听器）；这些动作只发生一次。
    fn begin_close(self: &Arc<Self>) -> CloseStart {
        let mut slot = lock(&self.closing);
        if let Some(share) = slot.as_ref() {
            return CloseStart::Joiner(Arc::clone(share));
        }
        let share = Arc::new(SharedClose::new());
        *slot = Some(Arc::clone(&share));
        drop(slot);
        // `#notifyClose()`：先撤销计时器，再同步运行一次关闭监听器。
        if let Some(sweep) = lock(&self.document_sweep).take()
            && let Some(disarm) = sweep
                .disarm
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
        {
            disarm();
        }
        if let Some(timer) = lock(&self.hold_timer).take()
            && let Some(disarm) = timer
                .disarm
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
        {
            disarm();
        }
        let listeners: Vec<CloseListener> = {
            let mut registered = lock(&self.close_listeners);
            registered
                .drain(..)
                .map(|(_id, listener)| listener)
                .collect()
        };
        for listener in listeners {
            let weak = Arc::downgrade(self);
            contained(
                || listener(),
                move |error| {
                    if let Some(inner) = weak.upgrade() {
                        inner.report_listener(&error, None);
                    }
                },
            );
        }
        CloseStart::Owner(share)
    }

    /// 运行 `before_close()` 与关闭任务，最后把结果写入 `SharedClose` 与 `closed`。
    ///
    /// `before_close()` 的错误被忽略（TS 契约要求它不得 reject）。关闭任务自身的错误进入
    /// `job_error`，由 `close_result()` 返回；`SessionEnd` 只由失败闩锁决定，两者互不替代。
    async fn finish_close(self: &Arc<Self>, share: Arc<SharedClose>, cleanup: ContextRef) {
        if let Err(_error) = self.hooks.before_close().await {
            // TS 的 `beforeClose()` 必须不 reject；普通 Session 没有这一步。
        }
        let result = self.run_close_job(cleanup).await;
        let outcome = match self.failure() {
            Some(failure) => EndKind::Failed(failure.message),
            None => EndKind::Closed,
        };
        // `close()` 的 rejection 是关闭任务自身的错误（TS: `#closing`），与失败闩锁分开。
        let job_error = result.err().map(|error| error.to_string());
        share.finish(outcome.clone(), job_error);
        self.closed.send_replace(Some(outcome));
    }

    /// 在变更线上执行关闭：先 `write_at_close()` 批次与 held 变化，再清空监听器/缓存，
    /// 最后关闭 Storage。
    ///
    /// 取票据保证此前已接纳的提交全部结清。失败的 Session 什么都不写；`write_at_close()`
    /// 或 held 存储失败会让 Session 失败，但后端仍会关闭。返回未存储错误（若有）。
    async fn run_close_job(self: &Arc<Self>, cleanup: ContextRef) -> Result<(), BoxError> {
        // TS：`writeAtClose()` 用 `BACKGROUND_CONTEXT`，held 存储与 `storage.close()` 用
        // 调用方 `close()` 上下文去掉 abort signal 后的 `cleanup`。
        let background = Arc::clone(&BACKGROUND_CONTEXT);
        let mut ticket = self.line.ticket();
        ticket.acquire().await;
        let mut unstored: Option<BoxError> = None;
        // 先写 `writeAtClose()` 的批次，再在 tracker 还在时存储 held 变化；失败的 Session 什么也不写。
        if self.failure().is_none() {
            let batch = self.hooks.write_at_close();
            if !batch.is_empty() {
                // 不发布，因为 Session 正在关闭。
                if let Err(error) = self
                    .storage
                    .commit(&batch, Box::new(|_seq| {}), &*background)
                    .await
                {
                    if self.failure().is_none() {
                        let _ = self.fail_with(&error);
                    }
                    unstored = Some(error);
                }
            }
            let holds = self.holds_snapshot();
            if !holds.is_empty()
                && let Err(error) = self.store_holds(holds, Arc::clone(&cleanup)).await
            {
                unstored = Some(error);
            }
        }
        lock(&self.commit_listeners).clear();
        lock(&self.internal_listeners).clear();
        lock(&self.documents).clear();
        lock(&self.idle_documents).clear();
        lock(&self.attached_documents).clear();
        lock(&self.holds).clear();
        if let Err(error) = self.storage.close_storage(&*cleanup).await {
            // 无法关闭的 Storage 是失败的；更早的失败保持原因。
            if self.failure().is_none() {
                let _ = self.fail_with(&error);
            }
            return Err(error);
        }
        match unstored {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// 记录错误并返回等价的 `ReportedError`；只有首次失败会以 `SessionFailed` 报告。
    ///
    /// 与 `fail()` 的区别：不唤醒 `failed()`、不启动关闭，只更新闩锁并报告。只用于关闭任务
    /// 内部（`write_at_close()` 批次提交失败、`storage.close_storage()` 失败），此时关闭已经
    /// 进行中。held 存储失败不经过这里：`run_close_job` 只把它记入 `unstored`，失败闩锁由
    /// `GuardedStorage::call`（写调用失败 → `fail()`）或 `store_holds` 的 adopt 检查负责。
    fn fail_with(&self, error: &BoxError) -> BoxError {
        let message = error.to_string();
        let first = {
            let mut slot = lock(&self.failure);
            if slot.is_none() {
                *slot = Some(Failure {
                    message: message.clone(),
                });
                true
            } else {
                false
            }
        };
        if first {
            // TS: 关闭时的 Storage 失败以 `session_failed` 报告（更早的失败不重复报告）。
            self.report_with(&message, ReportInfo::SessionFailed, None);
        }
        Box::new(ReportedError(message))
    }

    // ─── Cache ──────────────────────────────────────────────────────────────

    /// 把加载好的化身放进缓存并刷新空闲时间。
    fn install_document(self: &Arc<Self>, document: LoadedDocument) {
        let id = document.address_id.clone();
        lock(&self.documents).insert(id.clone(), document);
        self.touch_document(&id);
    }

    /// 仅当缓存里仍是同一个化身（`record.id` 相同）时丢弃；避免迟到的退役移除新化身。
    fn evict_document(&self, address_id: &str, record_id: crate::types::DocumentId) {
        let matches = lock(&self.documents)
            .get(address_id)
            .map(|document| document.record.id == record_id)
            .unwrap_or(false);
        if matches {
            self.drop_document(address_id);
        }
    }

    /// 记录一次使用：未 pin 的文档移动到空闲表队尾，并在需要时提前安排清扫。
    ///
    /// 为了不让高频访问的文档在每次提交里都重排，移动最多每“粒”一次——1 秒与保留期十分
    /// 之一的较小值。代价是它可能提前至多一粒被驱逐并重新加载。若当前清扫时间晚于
    /// `now + retention`（保留期被调短），则重新安排。已 pin 的文档不进空闲表。
    fn touch_document(self: &Arc<Self>, address_id: &str) {
        let record_id = match lock(&self.documents).get(address_id) {
            Some(document) => document.record.id,
            None => return,
        };
        if lock(&self.attached_documents).contains_key(&record_id) {
            return;
        }
        let now = self.clock();
        {
            let idle = lock(&self.idle_documents);
            if let Some(since) = idle.get(address_id)
                && now - since < 1_000.0_f64.min(self.retention() / 10.0)
            {
                return;
            }
        }
        {
            let mut idle = lock(&self.idle_documents);
            idle.shift_remove(address_id);
            idle.insert(address_id.to_owned(), now);
        }
        let sweep_too_late = match lock(&self.document_sweep).as_ref() {
            Some(sweep) => sweep.at > now + self.retention(),
            None => true,
        };
        if sweep_too_late {
            self.schedule_document_sweep();
        }
    }

    /// 同时从缓存与空闲表移除；pin 计数不动（由 `unpin` 单独归还）。
    fn drop_document(&self, address_id: &str) {
        lock(&self.documents).shift_remove(address_id);
        lock(&self.idle_documents).shift_remove(address_id);
    }

    /// 为最旧空闲文档安排一次清扫：计算出的计划时间已到（`at - clock() <= 0`，例如保留期为
    /// 0，或最旧空闲文档已经过期）时立即排到变更线，否则装载后台计时器。
    ///
    /// 已有一个不晚于新时间的清扫时直接复用；被替换的旧计时器会先撤销。宿主拒绝后台计时器
    /// 时记住 `document_timers_unavailable`，之后不再安排计时器——过期驱逐改由每次提交
    /// 结束时的 `sweep_documents()` 完成。
    fn schedule_document_sweep(self: &Arc<Self>) {
        if lock(&self.closing).is_some() {
            return;
        }
        let oldest = match lock(&self.idle_documents).values().next().copied() {
            Some(oldest) => oldest,
            None => return,
        };
        let at = oldest + self.retention();
        if let Some(sweep) = lock(&self.document_sweep).as_ref()
            && sweep.at <= at
        {
            return;
        }
        if let Some(previous) = lock(&self.document_sweep).take()
            && let Some(disarm) = previous
                .disarm
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
        {
            disarm();
        }
        let sweep = Arc::new(DocumentSweep {
            at,
            disarm: Mutex::new(None),
        });
        let delay = at - self.clock();
        *lock(&self.document_sweep) = Some(Arc::clone(&sweep));
        if delay <= 0.0 {
            self.run_sweep_job(Arc::clone(&sweep));
            return;
        }
        if self.document_timers_unavailable.load(Ordering::SeqCst) {
            *lock(&self.document_sweep) = None;
            return;
        }
        let weak = Arc::downgrade(self);
        let sweep_for_callback = Arc::clone(&sweep);
        let callback: TimerCallback = Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.run_sweep_job(Arc::clone(&sweep_for_callback));
            }
        });
        let disarm = self.timers.background(callback, delay);
        match disarm {
            Some(disarm) => {
                *sweep.disarm.lock().unwrap_or_else(|p| p.into_inner()) = Some(disarm);
            }
            None => {
                self.document_timers_unavailable
                    .store(true, Ordering::SeqCst);
                *lock(&self.document_sweep) = None;
            }
        }
    }

    /// 把一次清扫排到变更线上；无 Tokio 运行时时只递增计数，什么也不做。
    ///
    /// 线上先用 `Arc::ptr_eq` 判断自己是否仍是当前登记的清扫：只有是时才清空
    /// `document_sweep` 槽，避免旧任务清掉新登记的清扫；旧任务即使已被替换/撤销，只要关闭
    /// 尚未开始仍会执行一次 `sweep_documents()`。关闭已开始时跳过清扫。
    fn run_sweep_job(self: &Arc<Self>, sweep: Arc<DocumentSweep>) {
        self.sweep_jobs.fetch_add(1, Ordering::SeqCst);
        let inner = Arc::clone(self);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = inner
                    .enqueue(async {
                        {
                            let mut slot = lock(&inner.document_sweep);
                            if let Some(current) = slot.as_ref()
                                && Arc::ptr_eq(current, &sweep)
                            {
                                *slot = None;
                            }
                        }
                        if lock(&inner.closing).is_none() {
                            inner.sweep_documents();
                        }
                        Ok(())
                    })
                    .await;
            });
        }
    }

    /// 从空闲表最旧一端驱逐已过保留期的文档，然后安排下一次清扫。
    ///
    /// 依赖空闲表按触碰先后排列：遇到第一个未过期条目就停止。时钟若倒退，后面的过期项会
    /// 留到下一次清扫。
    fn sweep_documents(self: &Arc<Self>) {
        let expired = self.clock() - self.retention();
        let expired_addresses: Vec<String> = {
            let idle = lock(&self.idle_documents);
            let mut expired_addresses = Vec::new();
            for (address_id, since) in idle.iter() {
                if *since > expired {
                    break;
                }
                expired_addresses.push(address_id.clone());
            }
            expired_addresses
        };
        for address_id in expired_addresses {
            self.drop_document(&address_id);
        }
        self.schedule_document_sweep();
    }

    /// 给一个化身加一次 pin，并从空闲表移除；返回的解除函数是幂等的。
    ///
    /// 每个 watch/hold 持有一个 pin；计数归零时若缓存仍指向同一化身，重新触碰它以从
    /// “现在”开始计算空闲期。
    fn pin_document(
        self: &Arc<Self>,
        address_id: &str,
        record_id: crate::types::DocumentId,
    ) -> Arc<dyn Fn() + Send + Sync> {
        {
            let mut attached = lock(&self.attached_documents);
            *attached.entry(record_id).or_insert(0) += 1;
        }
        lock(&self.idle_documents).shift_remove(address_id);
        let weak = Arc::downgrade(self);
        let address_id = address_id.to_owned();
        let pinned = AtomicBool::new(true);
        Arc::new(move || {
            if !pinned.swap(false, Ordering::SeqCst) {
                return;
            }
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let remaining = {
                let mut attached = lock(&inner.attached_documents);
                let count = attached.get(&record_id).copied().unwrap_or(1) - 1;
                if count > 0 {
                    attached.insert(record_id, count);
                    return;
                }
                attached.shift_remove(&record_id);
                count
            };
            let _ = remaining;
            let still_occupant = lock(&inner.documents)
                .get(&address_id)
                .map(|document| document.record.id == record_id)
                .unwrap_or(false);
            if still_occupant {
                inner.touch_document(&address_id);
            }
        })
    }

    // ─── Loading ────────────────────────────────────────────────────────────

    /// 取当前化身：版本匹配的缓存直接复用；否则先存储它的 held 变化（若有）、丢弃旧
    /// tracker，再从 Storage 冷加载并物化。
    ///
    /// 重新加载前必须把 held 变化写掉，否则它们会随旧 tracker 离开缓存而丢失。返回 `None`
    /// 表示 Storage 没有该地址的记录；记录存在但内容读不到是错误（不是 `None`）。
    async fn load_document(
        self: &Arc<Self>,
        definition: Arc<DocDefinition>,
        address_id: &str,
        address: DocumentAddress,
        context: ContextRef,
    ) -> Result<Option<LoadedDocument>, BoxError> {
        let cached = lock(&self.documents).get(address_id).cloned();
        if let Some(cached) = &cached
            && cached.value_version == definition.version
        {
            self.touch_document(address_id);
            return Ok(Some(cached.clone()));
        }
        if let Some(cached) = cached {
            // 重新载入必须找到内存里的状态：held 变化在 tracker 离开前先存储。
            let hold = lock(&self.holds).get(&cached.record.id).cloned();
            if let Some(hold) = hold {
                self.store_holds(vec![hold], context.clone()).await?;
            }
            self.drop_document(address_id);
        }
        let storage = self.storage.clone();
        let record = storage
            .find_document(&address, DocumentPoint::Current, &*context)
            .await?;
        let Some(record) = record else {
            return Ok(None);
        };
        let stored = storage
            .document(record.id, DocumentPoint::Current, &*context)
            .await?;
        let Some(stored) = stored else {
            return Err(io::Error::other(format!(
                "Current document {} ({}) cannot be read",
                record.id, record.kind
            ))
            .into());
        };
        let value = materialize_document(&definition, &stored)?;
        let loaded = LoadedDocument::new(
            address_id.to_owned(),
            stored.record.clone(),
            stored.version,
            definition.version,
            stored.deltas_since_base,
            Arc::new(Tracker::new(value)),
        );
        lock(&self.documents).insert(address_id.to_owned(), loaded.clone());
        self.touch_document(address_id);
        Ok(Some(loaded))
    }

    // ─── Line ───────────────────────────────────────────────────────────────

    /// 在变更线上运行一个任务；调用（首次 poll）时取票据。
    async fn enqueue<T, F>(self: &Arc<Self>, job: F) -> Result<T, BoxError>
    where
        F: Future<Output = Result<T, BoxError>>,
    {
        let mut ticket = self.line.ticket();
        ticket.acquire().await;
        let result = job.await;
        ticket.finish();
        result
    }

    /// 在变更线上运行一个 `'static` 任务，并**现在**（调用方的同步栈里）取票据。
    ///
    /// 有运行时就直接 spawn：任务像 TS 的 `#tail.then(job)` 一样无论调用方是否继续等待都会运行；
    /// 没有运行时时退化为惰性 future（票据已按调用顺序分配）。
    ///
    /// 等待 spawn 结果时：任务 panic 会 `resume_unwind` 回调用方；任务被取消（非 panic 的
    /// `JoinError`，例如运行时关闭）映射为 `SessionClosed`。
    fn enqueue_eager<T, F>(self: &Arc<Self>, job: F) -> BoxFuture<'static, Result<T, BoxError>>
    where
        F: Future<Output = Result<T, BoxError>> + Send + 'static,
        T: Send + 'static,
    {
        let mut ticket = self.line.ticket();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let handle = handle.spawn(async move {
                    ticket.acquire().await;
                    let result = job.await;
                    ticket.finish();
                    result
                });
                Box::pin(async move {
                    match handle.await {
                        Ok(result) => result,
                        Err(error) => {
                            if error.is_panic() {
                                std::panic::resume_unwind(error.into_panic());
                            }
                            Err(Box::new(SessionClosed) as BoxError)
                        }
                    }
                })
            }
            Err(_) => Box::pin(async move {
                ticket.acquire().await;
                let result = job.await;
                ticket.finish();
                result
            }),
        }
    }

    // ─── Commits ────────────────────────────────────────────────────────────

    /// 一次提交的主体，必须在活动票据上运行。
    ///
    /// 阶段：
    /// 1. 健康检查与上下文中止检查；建 `Transaction` 并运行回调。
    /// 2. 回调失败或期间 Session 失败：`settle_failure()` 后返回。
    /// 3. `settle_success()` 结算草稿并产出写入；空写入直接丢弃，不落盘。
    /// 4. `read_only` 在 Storage 之前拒绝带写入的提交，Session 保持健康。
    /// 5. 延迟提交：把可 hold 的文档变化放进内存并发布 `ObservedCommit::Memory`。
    /// 6. 立即提交：取走相关 holds，调用 `Storage::commit`（`adopt` 在持久化后同步执行），
    ///    然后释放被并入的 holds、触碰缓存并 `publish()`。
    ///
    /// 成功路径与回调/结算失败路径会调用 `sweep_documents()`，让提交也触发过期驱逐；开头的
    /// 健康检查与上下文中止检查失败、`Storage::commit` 失败、adopt 缺失或失败这几类路径不会
    /// sweep。Storage 已提交但 adopt 缺失或失败会让 Session 失败，因为内存已经落后于持久
    /// 状态。
    async fn run_commit(
        self: &Arc<Self>,
        change: &dyn TransactionChange,
        context: ContextRef,
        scope: TransactionScope,
        within_ms: Option<f64>,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        self.assert_healthy()?;
        if let Some(signal) = context.abort_signal()
            && signal.is_aborted()
        {
            return Err(Box::new(abort_error(&signal)));
        }
        let host: Arc<dyn TransactionHost> = Arc::new(SessionHost {
            inner: Arc::downgrade(self),
            storage: self.storage.clone(),
        });
        let tx = Arc::new(Transaction::new(host, context.clone(), scope));
        let result = match change.run(&tx).await {
            Ok(result) => result,
            Err(error) => {
                tx.settle_failure().await;
                self.sweep_documents();
                return Err(error);
            }
        };
        // 捕获了失败读取的回调既不能提交，也不能假装成功。
        if let Err(error) = self.assert_healthy() {
            tx.settle_failure().await;
            self.sweep_documents();
            return Err(error);
        }
        let writes = match tx.settle_success().await {
            Ok(writes) => writes,
            Err(error) => {
                self.sweep_documents();
                return Err(error);
            }
        };
        if writes.is_empty() {
            tx.discard();
            self.sweep_documents();
            return Ok(result);
        }
        if self.read_only {
            // 在 Storage 之前拒绝：Session 保持健康，调用方能看到原因。
            tx.discard();
            self.sweep_documents();
            return Err(Box::new(ReadOnlySession::new("a commit with writes")));
        }
        if let Some(within_ms) = within_ms
            && let Some(held) = tx.holdable()
        {
            self.hold(&tx, held, within_ms, context);
            self.sweep_documents();
            return Ok(result);
        }
        // Storage 接纳前的最后一步：held 变化只在除了 Storage 之外不会再失败时离开队列。
        let taken = {
            let holds = lock(&self.holds);
            if holds.is_empty() {
                Vec::new()
            } else {
                let holds = holds.clone();
                tx.take_held(&move |id| holds.get(&id).cloned())
            }
        };
        // `take_held()` 与 TS 一样就地改写事务的写入列表。
        let writes = tx.current_writes();
        let adoption: Arc<Mutex<Option<Result<AdoptedDocuments, BoxError>>>> =
            Arc::new(Mutex::new(None));
        let adopt = {
            let tx = Arc::clone(&tx);
            let adoption = Arc::clone(&adoption);
            Box::new(move |seq: Seq| {
                let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tx.adopt(seq)
                })) {
                    Ok(documents) => Ok(documents),
                    Err(payload) => Err(panic_error(payload)),
                };
                *lock(&adoption) = Some(result);
            }) as crate::types::AdoptFn
        };
        let storage = self.storage.clone();
        let commit_context = without_abort_signal(context.clone());
        let seq = match storage.commit(&writes, adopt, &*commit_context).await {
            Ok(seq) => seq,
            Err(error) => {
                // 接纳之后的拒绝没有可丢弃的暂存；之前的丢弃它。
                if lock(&adoption).is_none() {
                    tx.discard();
                }
                return Err(error);
            }
        };
        let adopted = lock(&adoption).take();
        let Some(adopted) = adopted else {
            let error: BoxError = Box::new(ReportedError(
                "Storage committed without adopting the batch".to_owned(),
            ));
            self.fail(Box::new(ReportedError(error.to_string())), None);
            return Err(error);
        };
        let documents = match adopted {
            Ok(documents) => documents,
            Err(error) => {
                // Storage 已经提交；失败的采纳让内存落后于持久状态。
                self.fail(Box::new(ReportedError(error.to_string())), None);
                return Err(error);
            }
        };
        if !taken.is_empty() {
            self.release_holds(taken);
        }
        for document in &documents.stored {
            let record = match document {
                DocumentCommitChange::Document { record, .. }
                | DocumentCommitChange::DocumentCopy { record, .. } => record,
            };
            self.touch_document(&address_id(&DocumentAddress {
                kind: record.kind.clone(),
                scope: record.scope,
                key: record.key.clone(),
            }));
        }
        self.publish(seq, &writes, &documents, &context);
        self.sweep_documents();
        Ok(result)
    }

    /// 延迟提交：把事务的文档变化按化身并入 `holds`，并发布 `ObservedCommit::Memory`。
    ///
    /// - `tx.adopt_held()` 先把变化采纳到内存 tracker；
    /// - 每个文档只保留一个 `Hold`：后续变化追加进它的 `PendingOps`，deadline 取最早值；
    ///   首次创建时 pin 住化身；
    /// - 最后同步运行内部监听器。宿主提交监听器要等到真正存储后才看到这次变化。
    fn hold(
        self: &Arc<Self>,
        tx: &Transaction,
        held: Vec<HoldableChange>,
        within_ms: f64,
        context: ContextRef,
    ) {
        let documents = tx.adopt_held();
        let deadline = self.clock() + within_ms;
        for change in held {
            let id = change.loaded.record.id;
            let existing = lock(&self.holds).get(&id).cloned();
            let hold = match existing {
                Some(hold) => hold,
                None => {
                    let unpin = self.pin_document(&change.loaded.address_id, id);
                    let hold = Arc::new(Mutex::new(Hold {
                        held: crate::session::transaction::HeldDocument::new(change.loaded.clone()),
                        deadline,
                        unpin: Some(Box::new(move || unpin())),
                    }));
                    lock(&self.holds).insert(id, Arc::clone(&hold));
                    hold
                }
            };
            {
                let mut hold = lock(&hold);
                lock(&hold.held.pending).push(&change.ops);
                if change.base {
                    hold.held.base.store(true, Ordering::SeqCst);
                }
                if deadline < hold.deadline {
                    hold.deadline = deadline;
                }
            }
        }
        self.arm_hold_timer();
        let publication = ObservedCommit::Memory { changes: documents };
        let listeners: Vec<InternalListener> = lock(&self.internal_listeners)
            .iter()
            .map(|(_id, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            let inner = Arc::downgrade(self);
            let error_context = Arc::clone(&context);
            contained(
                || listener(&publication, &context),
                move |error| {
                    if let Some(inner) = inner.upgrade() {
                        inner.fail(error, Some(&*error_context));
                    }
                },
            );
        }
    }

    /// 把一组 hold 的变化合并成一次 Storage 提交，成功后释放 pin，并只通知宿主提交监听器。
    ///
    /// 变化已经在内存里（延迟提交时发布过 `ObservedCommit::Memory`），所以这里不再通知内部
    /// 观察者；`adopt` 回调按内容种类更新每个化身的 `deltas_since_base`。`AdoptFn` 是
    /// `Box<dyn Fn(Seq) + Send + Sync>`，没有错误通道；代码只检查 `adopted` 布尔值是否被
    /// 置位——Storage 已提交但 adopt 未被调用会让 Session 失败（内存与持久状态分叉）。
    async fn store_holds(
        self: &Arc<Self>,
        holds: Vec<Arc<Mutex<Hold>>>,
        context: ContextRef,
    ) -> Result<(), BoxError> {
        if holds.is_empty() {
            return Ok(());
        }
        let mut writes: Vec<StorageWrite> = Vec::new();
        let mut changes: Vec<DocumentCommitChange> = Vec::new();
        for hold in &holds {
            let hold = lock(hold);
            let loaded = &hold.held.loaded;
            let record = &loaded.record;
            let value = loaded.tracker.value();
            let stored = crate::session::transaction::store_held(
                &hold.held,
                &[],
                &value,
                loaded.value_version,
            );
            writes.push(StorageWrite::DocumentChange {
                id: record.id,
                content: stored.content.clone(),
            });
            // 被持有的文档都是会话文档。
            let conversation_id = match record.scope {
                DocumentScope::Conversation { conversation_id } => Some(conversation_id),
                _ => None,
            };
            changes.push(DocumentCommitChange::Document {
                record: record.clone(),
                conversation_id,
                version: Some(loaded.value_version),
                value: Some(value),
                ops: crate::types::Ops(stored.ops.clone()),
            });
        }
        let kinds: Vec<crate::types::DocumentContentKind> = writes
            .iter()
            .map(|write| match write {
                StorageWrite::DocumentChange { content, .. } => content.kind,
                _ => crate::types::DocumentContentKind::Base,
            })
            .collect();
        let adopted = Arc::new(AtomicBool::new(false));
        let adopt: crate::types::AdoptFn = {
            let holds = holds.clone();
            let adopted = Arc::clone(&adopted);
            Box::new(move |_seq: Seq| {
                adopted.store(true, Ordering::SeqCst);
                for (hold, kind) in holds.iter().zip(kinds.iter()) {
                    let hold = lock(hold);
                    let mut state = lock(&hold.held.loaded.state);
                    if *kind == crate::types::DocumentContentKind::Base {
                        state.deltas_since_base = 0;
                    } else {
                        state.deltas_since_base += 1;
                    }
                }
            })
        };
        let commit_context = without_abort_signal(context.clone());
        let seq = self
            .storage
            .commit(&writes, adopt, &*commit_context)
            .await?;
        if !adopted.load(Ordering::SeqCst) {
            let error: BoxError = Box::new(ReportedError(
                "Storage committed without adopting the batch".to_owned(),
            ));
            self.fail(Box::new(ReportedError(error.to_string())), None);
            return Err(error);
        }
        self.release_holds(holds);
        let publication = CommitPublication {
            seq,
            changes: changes
                .iter()
                .cloned()
                .map(CommitChange::Document)
                .collect(),
        };
        let listeners: Vec<CommitListener> = lock(&self.commit_listeners)
            .iter()
            .map(|(_id, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            let inner = Arc::downgrade(self);
            let listener_context = Arc::clone(&context);
            let error_context = Arc::clone(&context);
            contained(
                || listener(&publication, &*listener_context),
                move |error| {
                    if let Some(inner) = inner.upgrade() {
                        inner.report_listener(&error, Some(&*error_context));
                    }
                },
            );
        }
        Ok(())
    }

    /// 复制当前 holds 的 `Arc` 列表；调用方随后在锁外逐个加锁读取。
    fn holds_snapshot(&self) -> Vec<Arc<Mutex<Hold>>> {
        lock(&self.holds).values().cloned().collect()
    }

    /// 从 `holds` 移除并执行每个 hold 的 unpin（幂等），然后重新安排计时器。
    fn release_holds(self: &Arc<Self>, holds: Vec<Arc<Mutex<Hold>>>) {
        for hold in holds {
            let id = { lock(&hold).held.loaded.record.id };
            lock(&self.holds).shift_remove(&id);
            let unpin = { lock(&hold).unpin.take() };
            if let Some(unpin) = unpin {
                unpin();
            }
        }
        self.arm_hold_timer();
    }

    /// 为最早的 hold deadline 安排前台计时器。
    ///
    /// 已有计时器且已触发、或 deadline 未变时复用。关闭已开始时不再安排。到点后由
    /// `run_hold_store` 把所有 holds 一次写入：deadline 是上限，最早 deadline 时一次事务
    /// 存储全部 held 文档（`spec/durable-rest.md` 的 deferred commits 说明）。
    fn arm_hold_timer(self: &Arc<Self>) {
        if lock(&self.closing).is_some() {
            return;
        }
        let mut at = f64::INFINITY;
        for hold in lock(&self.holds).values() {
            let deadline = lock(hold).deadline;
            if deadline < at {
                at = deadline;
            }
        }
        {
            let armed = lock(&self.hold_timer);
            if let Some(armed) = armed.as_ref()
                && (armed.fired.load(Ordering::SeqCst) || armed.at == at)
            {
                return;
            }
        }
        let previous = lock(&self.hold_timer).take();
        if let Some(previous) = previous
            && let Some(disarm) = previous
                .disarm
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
        {
            disarm();
        }
        if !at.is_finite() {
            return;
        }
        let timer = Arc::new(HoldTimer {
            at,
            fired: AtomicBool::new(false),
            disarm: Mutex::new(None),
        });
        *lock(&self.hold_timer) = Some(Arc::clone(&timer));
        let weak = Arc::downgrade(self);
        let timer_for_callback = Arc::clone(&timer);
        let callback: TimerCallback = Arc::new(move || {
            timer_for_callback.fired.store(true, Ordering::SeqCst);
            if let Some(inner) = weak.upgrade() {
                inner.run_hold_store(Arc::clone(&timer_for_callback));
            }
        });
        let delay = (at - self.clock()).max(0.0);
        let disarm = self.timers.foreground(callback, delay);
        *timer.disarm.lock().unwrap_or_else(|p| p.into_inner()) = Some(disarm);
    }

    /// 计时器回调：在变更线上清掉自己，检查关闭/失败，然后把当前全部 holds 一次存储。
    ///
    /// 关闭会自己存储 held 变化，因此这里看到 `closing` 或失败时直接返回。无 Tokio 运行时
    /// 时不 spawn，什么也不做。
    fn run_hold_store(self: &Arc<Self>, timer: Arc<HoldTimer>) {
        let inner = Arc::clone(self);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = inner
                    .enqueue(async {
                        {
                            let mut slot = lock(&inner.hold_timer);
                            if slot
                                .as_ref()
                                .is_some_and(|current| Arc::ptr_eq(current, &timer))
                            {
                                *slot = None;
                            }
                        }
                        // 关闭会自己存储被持有的变化。
                        if lock(&inner.closing).is_some() || inner.failure().is_some() {
                            return Ok(());
                        }
                        let holds = inner.holds_snapshot();
                        inner
                            .store_holds(holds, Arc::clone(&BACKGROUND_CONTEXT))
                            .await
                    })
                    .await;
            });
        }
    }

    /// 同步发布一次已持久化提交。
    ///
    /// 先按写入类型收集 `CommitChange` 并发送 telemetry（`entry` 事件总是发送，`submission`
    /// 只在 done/unanswered 时发送），没有监听器时到此为止。内部监听器看到
    /// `ObservedCommit::Stored`（含本次提交自己的文档变化）；宿主监听器看到
    /// `CommitPublication`——只有本次提交还存储了 held 变化时两者才不同（此时宿主只看到实际
    /// 存储的文档变化）。任一监听器 panic 都被隔离，不影响其它监听器。
    fn publish(
        self: &Arc<Self>,
        seq: Seq,
        writes: &[StorageWrite],
        documents: &AdoptedDocuments,
        context: &ContextRef,
    ) {
        let has_listeners =
            !lock(&self.commit_listeners).is_empty() || !lock(&self.internal_listeners).is_empty();
        let mut changes: Vec<CommitChange> = Vec::new();
        for write in writes {
            match write {
                StorageWrite::Entry { value } => {
                    let mut attributes = SpanAttributes::new();
                    attributes.insert(
                        "entry.id".to_owned(),
                        kiana_telemetry::AttributeValue::Number(value.id.get() as f64),
                    );
                    attributes.insert(
                        "entry.kind".to_owned(),
                        kiana_telemetry::AttributeValue::String(value.kind.clone()),
                    );
                    context
                        .telemetry()
                        .add_event("kiana.durable.entry", Some(attributes));
                    if has_listeners {
                        changes.push(CommitChange::Table(
                            crate::types::TableCommitChange::Entry {
                                value: value.clone(),
                            },
                        ));
                    }
                }
                StorageWrite::Submission { value } => {
                    if matches!(
                        value.status,
                        crate::types::SubmissionStatus::Done
                            | crate::types::SubmissionStatus::Unanswered
                    ) {
                        let mut attributes = SpanAttributes::new();
                        attributes.insert(
                            "submission.id".to_owned(),
                            kiana_telemetry::AttributeValue::Number(value.id.get() as f64),
                        );
                        attributes.insert(
                            "status".to_owned(),
                            kiana_telemetry::AttributeValue::String(
                                match value.status {
                                    crate::types::SubmissionStatus::Done => "done",
                                    crate::types::SubmissionStatus::Unanswered => "unanswered",
                                    _ => "queued",
                                }
                                .to_owned(),
                            ),
                        );
                        if let Some(answer) = value.answer {
                            attributes.insert(
                                "answer.entry_id".to_owned(),
                                kiana_telemetry::AttributeValue::Number(answer.get() as f64),
                            );
                        }
                        context
                            .telemetry()
                            .add_event("kiana.durable.submission", Some(attributes));
                    }
                    if has_listeners {
                        changes.push(CommitChange::Table(
                            crate::types::TableCommitChange::Submission {
                                value: value.clone(),
                            },
                        ));
                    }
                }
                StorageWrite::Conversation { value } => {
                    if has_listeners {
                        changes.push(CommitChange::Table(
                            crate::types::TableCommitChange::Conversation {
                                value: value.clone(),
                            },
                        ));
                    }
                }
                StorageWrite::Task { value } => {
                    if has_listeners {
                        changes.push(CommitChange::Table(crate::types::TableCommitChange::Task {
                            value: value.clone(),
                        }));
                    }
                }
                StorageWrite::DocumentCreate { .. }
                | StorageWrite::DocumentCopy { .. }
                | StorageWrite::DocumentChange { .. }
                | StorageWrite::DocumentRetire { .. } => {}
            }
        }
        if !has_listeners {
            return;
        }
        let tables = changes.len();
        for document in &documents.observed {
            changes.push(CommitChange::Document(document.clone()));
        }
        let observed = ObservedCommit::Stored {
            seq,
            changes: changes.clone(),
        };
        // 两者共用一份发布，除非这次提交还存储了 held 变化，那只有持久监听器能看到。
        let stored = if documents.stored == documents.observed {
            CommitPublication {
                seq,
                changes: changes.clone(),
            }
        } else {
            let mut stored_changes: Vec<CommitChange> = changes[..tables].to_vec();
            for document in &documents.stored {
                stored_changes.push(CommitChange::Document(document.clone()));
            }
            CommitPublication {
                seq,
                changes: stored_changes,
            }
        };
        let internal: Vec<InternalListener> = lock(&self.internal_listeners)
            .iter()
            .map(|(_id, listener)| Arc::clone(listener))
            .collect();
        for listener in internal {
            let inner = Arc::downgrade(self);
            contained(
                || listener(&observed, context),
                move |error| {
                    if let Some(inner) = inner.upgrade() {
                        inner.fail(error, Some(&**context));
                    }
                },
            );
        }
        let host: Vec<CommitListener> = lock(&self.commit_listeners)
            .iter()
            .map(|(_id, listener)| Arc::clone(listener))
            .collect();
        for listener in host {
            let inner = Arc::downgrade(self);
            contained(
                || listener(&stored, &**context),
                move |error| {
                    if let Some(inner) = inner.upgrade() {
                        inner.report_listener(&error, Some(&**context));
                    }
                },
            );
        }
    }

    /// 把一个已加载化身包装成 watch，并挂上提交、关闭两条监听。
    ///
    /// 先校验记录作用域与版本，再 pin 化身。提交监听器只转发同一 `record.id` 的变化：退役
    /// （`value == None`）先 unpin 再让 watch 以 `Retired` 结束；观察版本变化时用完整根替换
    /// 代替形状不兼容的 ops；空 ops（纯迁移）跳过。关闭监听器以失败文本结束 watch
    /// （`Failed`）或正常结束（`Ended`）。detach 同时注销两条监听并 unpin，保证 watch 终止
    /// 后缓存可被清扫。
    fn attach_document(
        self: &Arc<Self>,
        definition: Arc<DocDefinition>,
        loaded: LoadedDocument,
    ) -> Result<Watch, BoxError> {
        check_record_scope(&definition, &loaded.record)?;
        check_record_version(&definition, &loaded.record, loaded.stored_version())?;
        let unpin = self.pin_document(&loaded.address_id, loaded.record.id);
        let record_id = loaded.record.id;
        // TS: `new Watch(value, detach, (error) => this.report(error))`：监听器错误以 `listener_error`
        // 报告给宿主，并只结束这一个 watch。
        let reporter = {
            let weak = Arc::downgrade(self);
            Arc::new(move |error: BoxError| {
                if let Some(inner) = weak.upgrade() {
                    inner.report_listener(&error, None);
                }
            }) as WatchErrorReport
        };
        let watch = Watch::new(loaded.tracker.value(), Some(reporter));
        let watch_for_commit = watch.clone();
        let watch_for_close = watch.clone();
        let unpin_for_commit = Arc::clone(&unpin);
        let observed_version = Arc::new(Mutex::new(loaded.value_version));
        let observer: InternalListener =
            Arc::new(move |publication: &ObservedCommit, ctx: &ContextRef| {
                let changes: Vec<DocumentCommitChange> = match publication {
                    ObservedCommit::Stored { changes, .. } => changes
                        .iter()
                        .filter_map(|change| match change {
                            CommitChange::Document(document) => Some(document.clone()),
                            CommitChange::Table(_) => None,
                        })
                        .collect(),
                    ObservedCommit::Memory { changes } => changes.clone(),
                };
                for change in changes {
                    let DocumentCommitChange::Document {
                        record,
                        version,
                        value,
                        ops,
                        ..
                    } = &change
                    else {
                        continue;
                    };
                    if record.id != record_id {
                        continue;
                    }
                    match value {
                        None => {
                            // 退役的化身再也不会变化：它的 watch 送达退役，但不再 pin 任何东西。
                            unpin_for_commit();
                            watch_for_commit.retire(ctx.clone());
                        }
                        Some(value) => {
                            let ops = {
                                let mut observed = lock(&observed_version);
                                if *version != Some(*observed) {
                                    *observed = version.unwrap_or(*observed);
                                    vec![Op::Replace(object_value(value))]
                                } else {
                                    ops.0.clone()
                                }
                            };
                            // 只有迁移的 base 对新版本的观察者什么也没变。
                            if ops.is_empty() {
                                continue;
                            }
                            watch_for_commit.advance(value.clone(), ops, ctx.clone());
                        }
                    }
                }
            });
        if lock(&self.closing).is_some() {
            return Err(Box::new(SessionClosed));
        }
        let commit_listener_id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.internal_listeners).push((commit_listener_id, observer));
        let weak = Arc::downgrade(self);
        let unsubscribe_commit: Unsubscribe = Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                lock(&inner.internal_listeners)
                    .retain(|(candidate, _)| *candidate != commit_listener_id);
            }
        });
        let weak = Arc::downgrade(self);
        let close_listener: CloseListener = Arc::new(move || {
            let failure = weak.upgrade().and_then(|inner| inner.failure());
            watch_for_close.end_with_failure(failure.map(|failure| failure.message));
        });
        let close_listener_id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.close_listeners).push((close_listener_id, close_listener));
        let weak = Arc::downgrade(self);
        let unsubscribe_close: Unsubscribe = Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                lock(&inner.close_listeners)
                    .retain(|(candidate, _)| *candidate != close_listener_id);
            }
        });
        let detach: Box<dyn FnOnce() + Send + Sync> = Box::new(move || {
            unsubscribe_commit();
            unsubscribe_close();
            unpin();
        });
        watch.set_detach(detach);
        Ok(watch)
    }
}

fn object_value(value: &JsonObject) -> Value {
    Value::Object(value.clone().into_iter().collect())
}

// ---------------------------------------------------------------------------
// GuardedStorage
// ---------------------------------------------------------------------------

/// 在途调用计数的 Drop guard：调用被丢弃时也归还计数。
struct UnderwayGuard<'a> {
    storage: &'a GuardedStorage,
}

impl Drop for UnderwayGuard<'_> {
    fn drop(&mut self) {
        self.storage.leave();
    }
}

/// Session 的 Storage（TS: `GuardedStorage`）：失败闩锁 + 在途调用跟踪。
///
/// Session 的所有持久化调用都应经过它，而不是直接使用构造时的后端：
/// - 调用前检查 Session 是否已失败或正在关闭；
/// - 调用失败时（`commit`/`mint_id` 总是，读调用除 `StorageRequestError` 与调用方取消外）
///   把错误写入失败闩锁并开始关闭；
/// - 计数在途调用，`close_storage()` 等它们全部结束后才关闭后端。
///
/// 注意：这里只在**返回错误**时标记失败；`Storage::commit` 的成功路径由调用方检查
/// `adopt` 是否被调用（见 `SessionInner::run_commit`）。
pub struct GuardedStorage {
    storage: Arc<dyn Storage>,
    latch: Mutex<Weak<SessionInner>>,
    /// 在途后端调用数。`watch` 的版本比较避免 `Notify::notify_waiters()` 丢失唤醒。
    underway: tokio::sync::watch::Sender<usize>,
    closing: AtomicBool,
}

impl GuardedStorage {
    fn new(storage: Arc<dyn Storage>, latch: Weak<SessionInner>) -> Self {
        GuardedStorage {
            storage,
            latch: Mutex::new(latch),
            underway: tokio::sync::watch::channel(0).0,
            closing: AtomicBool::new(false),
        }
    }

    fn inner(&self) -> Option<Arc<SessionInner>> {
        lock(&self.latch).upgrade()
    }

    fn assert_healthy(&self) -> Result<(), BoxError> {
        match self.inner().and_then(|inner| inner.failure()) {
            Some(failure) => Err(Box::new(SessionFailed::new(failure.error()))),
            None => Ok(()),
        }
    }

    fn fail(&self, error: &(dyn std::error::Error + 'static)) {
        if let Some(inner) = self.inner() {
            inner.fail(Box::new(ReportedError(error.to_string())), None);
        }
    }

    fn enter(&self) -> UnderwayGuard<'_> {
        self.underway.send_modify(|count| *count += 1);
        UnderwayGuard { storage: self }
    }

    fn leave(&self) {
        self.underway
            .send_modify(|count| *count = count.saturating_sub(1));
    }

    /// 等到在途计数归零；用 `watch` 的版本比较而不是一次性的通知，避免丢唤醒。
    async fn wait_underway(&self) {
        let mut receiver = self.underway.subscribe();
        loop {
            if *receiver.borrow_and_update() == 0 {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    /// 执行一次后端调用，并把结果映射到 Session 的健康状态。
    ///
    /// - 调用前：已失败或正在关闭时直接返回 `SessionFailed` / `SessionClosed`，不触达后端。
    /// - 调用中：`UnderwayGuard` 计数；future 被丢弃时计数也会归还。
    /// - 成功后：再查一次健康（并发失败可能发生在此期间），已失败则本次结果作废。
    /// - 失败后：已失败的 Session 返回**第一个**失败原因；`read` 为 `Some` 时，
    ///   `StorageRequestError` 或被取消上下文的错误只让本次调用失败，否则写闩锁并开始关闭。
    ///
    /// `read = None` 的调用（`commit`、`mint_id`）因此总是把错误升级为 Session 失败：
    /// 批次一旦被接纳，是否已提交不可知，不能当作可重试的读错误。
    ///
    /// 注意：`closing` 检查与 `enter()` 的计数递增之间没有互斥；跨线程并发
    /// `close_storage()` 时，关闭可能在这两步之间读到旧计数并开始关闭后端。仓库内是否有
    /// 依赖这一窗口的调用方未核实。
    async fn call<T, F>(&self, read: Option<&dyn Context>, run: F) -> Result<T, BoxError>
    where
        F: Future<Output = Result<T, BoxError>>,
    {
        self.assert_healthy()?;
        if self.closing.load(Ordering::SeqCst) {
            return Err(Box::new(SessionClosed));
        }
        // 在途计数必须在**将来被丢弃**时也归还：TS 把后端 promise 放进 `#underway` 并在
        // `finally` 删除；Rust 的调用方可以 drop 正在等待的后端调用（timeout/select/取消的
        // task），所以用 Drop guard，否则 `close()` 会永远等一个不会归零的计数。
        let result = {
            let _underway = self.enter();
            run.await
        };
        match result {
            Ok(value) => {
                self.assert_healthy()?;
                Ok(value)
            }
            Err(error) => {
                // TS `#assertHealthy()`：已经失败时抛出**第一个**错误作为原因，而不是本次错误。
                if let Some(inner) = self.inner()
                    && let Some(failure) = inner.failure()
                {
                    return Err(Box::new(SessionFailed::new(failure.error())));
                }
                // 被调用方取消的读、或非法请求：只让这次调用失败。
                let exempt = match read {
                    Some(context) => {
                        error
                            .downcast_ref::<crate::types::StorageRequestError>()
                            .is_some()
                            || context
                                .abort_signal()
                                .is_some_and(|signal| signal.is_aborted())
                    }
                    None => false,
                };
                if !exempt {
                    self.fail(&*error);
                }
                Err(error)
            }
        }
    }

    /// 拒绝新调用、等在途调用结束，然后关闭后端。
    ///
    /// `Storage::close` 没有错误通道，因此这里总是 `Ok(())`；需要关闭错误的调用方应查询
    /// 后端自己的接口（例如 SQLite 后端的 `close_result`）。
    async fn close_storage(&self, context: &dyn Context) -> Result<(), BoxError> {
        self.closing.store(true, Ordering::SeqCst);
        self.wait_underway().await;
        self.storage.close(context).await;
        Ok(())
    }
}

#[async_trait]
impl Storage for GuardedStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: crate::types::AdoptFn,
        context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        self.call(None, self.storage.commit(writes, adopt, context))
            .await
    }

    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.call(None, self.storage.mint_id(kind)).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.call(Some(context), self.storage.conversation(id, context))
            .await
    }

    async fn scan_conversations(
        &self,
        query: &crate::types::ConversationQuery,
        limit: usize,
        cursor: Option<&crate::types::Cursor>,
        context: &dyn Context,
    ) -> Result<crate::types::Page<ConversationRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage
                .scan_conversations(query, limit, cursor, context),
        )
        .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<crate::types::StoredEntry>, BoxError> {
        self.call(Some(context), self.storage.entry(id, context))
            .await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<crate::types::StoredEntry>, BoxError> {
        self.call(
            Some(context),
            self.storage.visible_entry(conversation_id, id, context),
        )
        .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &dyn Context,
    ) -> Result<Option<crate::types::EntryRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage
                .find_latest_head_marker(conversation_id, at_or_before_entry_id, context),
        )
        .await
    }

    async fn scan_entries(
        &self,
        query: &crate::types::EntryQuery,
        limit: usize,
        cursor: Option<&crate::types::Cursor>,
        context: &dyn Context,
    ) -> Result<crate::types::Page<crate::types::EntryRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage.scan_entries(query, limit, cursor, context),
        )
        .await
    }

    async fn task(
        &self,
        id: crate::types::TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.call(Some(context), self.storage.task(id, context))
            .await
    }

    async fn scan_tasks(
        &self,
        query: &crate::types::TaskQuery,
        limit: usize,
        cursor: Option<&crate::types::Cursor>,
        context: &dyn Context,
    ) -> Result<crate::types::Page<TaskRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage.scan_tasks(query, limit, cursor, context),
        )
        .await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<crate::types::SubmissionRecord>, BoxError> {
        self.call(Some(context), self.storage.submission(id, context))
            .await
    }

    async fn scan_submissions(
        &self,
        query: &crate::types::SubmissionQuery,
        limit: usize,
        cursor: Option<&crate::types::Cursor>,
        context: &dyn Context,
    ) -> Result<crate::types::Page<crate::types::SubmissionRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage.scan_submissions(query, limit, cursor, context),
        )
        .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<crate::types::SubmissionRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage
                .submission_by_request(conversation_id, request_id, context),
        )
        .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage.find_document(address, at, context),
        )
        .await
    }

    async fn document(
        &self,
        id: crate::types::DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.call(Some(context), self.storage.document(id, at, context))
            .await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&crate::types::Cursor>,
        context: &dyn Context,
    ) -> Result<crate::types::Page<DocumentRecord>, BoxError> {
        self.call(
            Some(context),
            self.storage.scan_documents(query, limit, cursor, context),
        )
        .await
    }

    async fn close(&self, context: &dyn Context) {
        let _ = self.close_storage(context).await;
    }
}

// ---------------------------------------------------------------------------
// SessionHost
// ---------------------------------------------------------------------------

/// 事务看到的 Session 服务：受保护的 Storage、原始 `now` 与缓存访问。
///
/// `inner` 是 `Weak`，事务不延长 Session 的生命周期；升级失败时 `load` /
/// `conversation_created` 返回 `SessionClosed`，`now` 退回系统墙钟，`cached` 视为未命中，
/// `install`/`evict` 什么也不做。
struct SessionHost {
    inner: Weak<SessionInner>,
    storage: Arc<GuardedStorage>,
}

#[async_trait]
impl TransactionHost for SessionHost {
    fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    fn now(&self) -> Result<f64, BoxError> {
        match self.inner.upgrade() {
            Some(inner) => {
                // TS 的 `now()` 抛出即让提交失败；Rust 里提供者以 panic 表示抛出，转成错误。
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (inner.raw_now)())) {
                    Ok(value) => Ok(value),
                    Err(payload) => Err(panic_error(payload)),
                }
            }
            None => Ok(now_millis()),
        }
    }

    fn cached(&self, address_id: &str) -> Option<LoadedDocument> {
        let inner = self.inner.upgrade()?;
        let cached = lock(&inner.documents).get(address_id).cloned();
        if cached.is_some() {
            inner.touch_document(address_id);
        }
        cached
    }

    async fn load(
        &self,
        definition: Arc<DocDefinition>,
        address_id: &str,
        address: DocumentAddress,
        context: ContextRef,
    ) -> Result<Option<LoadedDocument>, BoxError> {
        let Some(inner) = self.inner.upgrade() else {
            return Err(Box::new(SessionClosed));
        };
        inner
            .load_document(definition, address_id, address, context)
            .await
    }

    fn install(&self, document: LoadedDocument) {
        if let Some(inner) = self.inner.upgrade() {
            inner.install_document(document);
        }
    }

    fn evict(&self, address_id: &str, record_id: crate::types::DocumentId) {
        if let Some(inner) = self.inner.upgrade() {
            inner.evict_document(address_id, record_id);
        }
    }

    async fn conversation_created(
        &self,
        tx: &Transaction,
        record: &ConversationRecord,
    ) -> Result<(), BoxError> {
        let Some(inner) = self.inner.upgrade() else {
            return Err(Box::new(SessionClosed));
        };
        inner.hooks.conversation_created(tx, record).await
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

struct SessionChangeAdapter<'a>(&'a dyn SessionChange);

#[async_trait]
impl TransactionChange for SessionChangeAdapter<'_> {
    async fn run(&self, tx: &Transaction) -> Result<Box<dyn Any + Send>, BoxError> {
        self.0.run(tx as &dyn Tx).await
    }
}

#[async_trait]
impl Session for SessionImpl {
    /// `Session::commit`：把 `&dyn Tx` 适配成 `TransactionChange`，在变更线上运行。
    async fn commit(
        &self,
        change: &dyn SessionChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        let adapter = SessionChangeAdapter(change);
        self.commit_transaction(&adapter, context, None, options)
            .await
    }

    /// 转发到 [`SessionImpl::close`]；关闭任务自身的错误在这里被丢弃。
    async fn close(&self, context: &dyn Context) {
        SessionImpl::close(self, context).await;
    }

    /// 等待关闭完成，可重复调用；关闭后立即返回同一终态。
    async fn closed(&self) -> SessionEnd {
        let mut receiver = self.inner.closed.subscribe();
        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return outcome.to_session_end();
            }
            if receiver.changed().await.is_err() {
                return SessionEnd::Closed;
            }
        }
    }

    /// # Panics
    ///
    /// Session 已失败或已开始关闭时 panic；panic payload 是 `BoxError`（trait 返回类型没有
    /// 错误通道），调用方可 `catch_unwind` 后 `downcast_ref::<BoxError>()` 取回。
    fn subscribe_commits(&self, listener: CommitListener) -> Unsubscribe {
        // TS 的 `subscribeCommits()` 调 `assertUsable()` 并抛出：失败时 `SessionFailed`，关闭中
        // `Error("Session is closed")`。trait 的返回类型没有错误通道，用 `panic_any` 携带同一个
        // 错误对象（调用方可 `catch_unwind` + `downcast_ref::<BoxError>()` 取回）。
        match SessionImpl::subscribe_commits(self, listener) {
            Ok(unsubscribe) => unsubscribe,
            Err(error) => std::panic::panic_any(error),
        }
    }

    /// # Panics
    ///
    /// Session 已失败或已开始关闭时 panic；payload 同样是 `BoxError`（见
    /// [`Session::subscribe_commits`]）。
    fn subscribe_close(&self, listener: CloseListener) -> Unsubscribe {
        // 同上：TS 的 `subscribeClose()` 抛出 `SessionFailed` / `Error("Session is closed")`。
        match SessionImpl::subscribe_close(self, listener) {
            Ok(unsubscribe) => unsubscribe,
            Err(error) => std::panic::panic_any(error),
        }
    }
}

#[async_trait]
impl DocumentReader for SessionImpl {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        SessionImpl::snapshot(self, target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        SessionImpl::snapshot_as_of(self, target, at, context).await
    }
}

#[async_trait]
impl DocumentObserver for SessionImpl {
    async fn watch_doc(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<DocumentWatch>, BoxError> {
        SessionImpl::watch_doc(self, target, context).await
    }
}

// ---------------------------------------------------------------------------
// Watch
// ---------------------------------------------------------------------------

/// `Watch<T>` 的终态（内部，可重复读取）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum WatchEndKind {
    Stopped,
    Cancelled,
    Retired,
    Ended,
    ListenerError(String),
    Failed(String),
}

impl WatchEndKind {
    fn to_end(&self) -> WatchEnd {
        match self {
            WatchEndKind::Stopped => WatchEnd::Stopped,
            WatchEndKind::Cancelled => WatchEnd::Cancelled,
            WatchEndKind::Retired => WatchEnd::Retired,
            WatchEndKind::Ended => WatchEnd::Ended,
            WatchEndKind::ListenerError(message) => WatchEnd::ListenerError(message.clone()),
            WatchEndKind::Failed(message) => {
                WatchEnd::Failed(Box::new(ReportedError(message.clone())))
            }
        }
    }
}

/// watch 监听器错误的报告通道（TS: `new Watch(..., report)` 的 `report`）。
pub type WatchErrorReport = Arc<dyn Fn(BoxError) + Send + Sync>;

/// watch 的共享可变状态（受 `WatchInner::state` 保护）。
///
/// 生命周期由 `end` 表示：`None`（活动）→ `Some(Stopped/Cancelled/Retired/Ended/
/// ListenerError/Failed)`，转换只发生在 `terminate_inner()` 且是单向的；终止时清空
/// `listener`、`pending`、`newest`、`cancellation` 并取走 `detach`。
///
/// 投递侧另有三个标记，它们不是生命周期状态：
/// - `started`：`start()` 已安装监听器；之前 `advance`/`retire` 只累积最新帧；
/// - `scheduled`：已有投递任务在排队（防止重复 spawn）；
/// - `running`：`drain()` 正在循环投递；期间到达的帧只更新 `newest`，循环结束再补排程。
///
/// `newest` 只保留最近一帧（旧帧被覆盖），`pending` 保存与之配套、尚未折叠的 op；
/// `drain()` 取走一帧时调用 `PendingOps::take()`，若返回 `Replace` 就用这一帧的值构造
/// 根替换。`retiring` 表示退役帧已排队，`value` 变成 `None` 的那一帧送达后以 `Retired`
/// 结束。
struct WatchState {
    value: Arc<Option<JsonObject>>,
    pending: PendingOps,
    newest: Option<(Arc<Option<JsonObject>>, ContextRef)>,
    retiring: bool,
    started: bool,
    scheduled: bool,
    running: bool,
    end: Option<WatchEndKind>,
    listener: Option<Arc<dyn WatchListener<Option<JsonObject>>>>,
    detach: Option<Box<dyn FnOnce() + Send + Sync>>,
    cancellation: Option<kiana_chord::context::AbortSubscription>,
}

/// 内部状态，watch 句柄与投递任务共享（`Arc`）。
///
/// TS 的 `Watch` 只保留最近的值（终止时 `release()` 清空 pending/newest 与所有源引用）。这里
/// `state.value` 也只保留最近的值；被替换掉的旧值随最后一个 `Arc` 释放。冻结契约
/// `WatchHandle::value(&self) -> &T` 要求调用方能持有引用直到 `&self` 借用结束，因此**每个句柄**
/// 在自己被借用时保留它交出过的值（见 `Watch::retained`），句柄 drop 时释放——不会像旧的
/// 竞技场那样在终止后仍保留整段历史。
pub struct WatchInner {
    state: Mutex<WatchState>,
    closed: tokio::sync::watch::Sender<Option<WatchEndKind>>,
    report: Option<WatchErrorReport>,
}

/// 一份已提交文档化身的 watch（TS: chord 的 `Watch<JsonObject | null>`）。
///
/// 生命周期：`start()` 之前只累积最新帧；`start()` 后由后台任务按序投递；`stop()`、上下文
/// 取消、源关闭、退役与监听器错误都经 `terminate_inner()` 进入终态，之后 `advance()` 被
/// 忽略，`closed()` 可重复读取。状态细节见 `WatchState` 的说明。
pub struct Watch {
    inner: Arc<WatchInner>,
    /// 这个句柄通过 `value()` 交出过的值：让返回的引用在 `&self` 借用期内保持有效。
    /// 句柄 drop（或被克隆时）释放；克隆出的句柄从空列表开始。
    retained: Mutex<Vec<Arc<Option<JsonObject>>>>,
}

impl Clone for Watch {
    fn clone(&self) -> Self {
        Watch {
            inner: Arc::clone(&self.inner),
            retained: Mutex::new(Vec::new()),
        }
    }
}

impl Watch {
    fn new(initial: JsonObject, report: Option<WatchErrorReport>) -> Self {
        let value = Arc::new(Some(initial));
        let (closed, _) = tokio::sync::watch::channel(None);
        Watch {
            inner: Arc::new(WatchInner {
                state: Mutex::new(WatchState {
                    value,
                    pending: PendingOps::new(),
                    newest: None,
                    retiring: false,
                    started: false,
                    scheduled: false,
                    running: false,
                    end: None,
                    listener: None,
                    detach: None,
                    cancellation: None,
                }),
                closed,
                report,
            }),
            retained: Mutex::new(Vec::new()),
        }
    }

    /// 把一帧设为 `newest`。
    fn enqueue_frame(state: &mut WatchState, value: Arc<Option<JsonObject>>, context: ContextRef) {
        state.newest = Some((value, context));
    }

    /// `advance()`：排入一帧。
    ///
    /// `ops` 追加进 `pending`，值替换 `newest`；未 `start()` 时只保留最新一帧，已终止或正在
    /// 退役时忽略。帧在监听器消费时才会被应用/交付。
    pub fn advance(&self, value: JsonObject, ops: Vec<Op>, context: ContextRef) {
        let mut state = lock(&self.inner.state);
        if state.end.is_some() || state.retiring {
            return;
        }
        state.pending.push(&ops);
        let arc = Arc::new(Some(value));
        Self::enqueue_frame(&mut state, arc, context);
        if state.started {
            Self::schedule(&self.inner, &mut state);
        }
    }

    /// `retire()`：排入 `[["r", null]]`，随后以 `retired` 结束。
    ///
    /// `retiring` 置位后 `advance()` 被忽略；该帧送达（值为 `null`）后 watch 终止。
    pub fn retire(&self, context: ContextRef) {
        let mut state = lock(&self.inner.state);
        if state.end.is_some() || state.retiring {
            return;
        }
        state.retiring = true;
        state.pending.push(&[Op::Replace(Value::Null)]);
        let arc: Arc<Option<JsonObject>> = Arc::new(None);
        Self::enqueue_frame(&mut state, arc, context);
        if state.started {
            Self::schedule(&self.inner, &mut state);
        }
    }

    /// 源关闭：立即以 `Ended` 终止，不再投递额外帧。
    pub fn end(&self) {
        self.terminate(WatchEndKind::Ended);
    }

    /// Session 关闭时调用：有失败文本则以 `Failed` 终止，否则以 `Ended` 终止。
    fn end_with_failure(&self, failure: Option<String>) {
        match failure {
            Some(message) => self.terminate(WatchEndKind::Failed(message)),
            None => self.terminate(WatchEndKind::Ended),
        }
    }

    /// 获取它的上下文被取消：立即以 `Cancelled` 终止。
    pub fn cancel(&self) {
        self.terminate(WatchEndKind::Cancelled);
    }

    /// `observeCancellation()`：结束安装一次；重复安装是调用方错误（TS 抛
    /// `Watch cancellation is already installed`）。
    ///
    /// # Panics
    ///
    /// 第二次安装时。
    pub fn observe_cancellation(&self, signal: &kiana_chord::context::AbortSignal) {
        {
            let state = lock(&self.inner.state);
            if state.cancellation.is_some() {
                panic!("Watch cancellation is already installed");
            }
            if state.end.is_some() {
                return;
            }
        }
        let inner = Arc::downgrade(&self.inner);
        let subscription = signal.on_abort(move |_reason| {
            if let Some(inner) = inner.upgrade() {
                Watch::terminate_inner(&inner, WatchEndKind::Cancelled);
            }
        });
        {
            let mut state = lock(&self.inner.state);
            if state.end.is_some() {
                return;
            }
            state.cancellation = Some(subscription);
        }
        // `on_abort` 与 DOM 一样不会对已中止的信号补发（TS 在此之后再查一次 `aborted`）。
        if signal.is_aborted() {
            self.cancel();
        }
    }

    /// 安装终止时的 detach；若 watch 已经终止，立即在锁外运行它。
    fn set_detach(&self, detach: Box<dyn FnOnce() + Send + Sync>) {
        let mut state = lock(&self.inner.state);
        if state.end.is_some() {
            drop(state);
            detach();
            return;
        }
        state.detach = Some(detach);
    }

    /// 确保有一个投递任务在排队；`scheduled`/`running` 或已终止时不再 spawn。
    ///
    /// 投递任务需要 Tokio 运行时；没有运行时时不会 spawn（`scheduled` 已置位）。
    fn schedule(inner: &Arc<WatchInner>, state: &mut WatchState) {
        if state.scheduled || state.running || state.end.is_some() {
            return;
        }
        state.scheduled = true;
        let weak = Arc::downgrade(inner);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Some(inner) = weak.upgrade() {
                    Watch::drain(inner).await;
                }
            });
        }
    }

    /// 把 `newest` 与折叠后的 pending 依次交给监听器，直到没有帧或 watch 终止。
    ///
    /// 每轮在锁内取走 `(listener, value, ops, context, retires)`，锁外 await 监听器；监听器
    /// 运行期间到达的帧只更新 `newest`，循环结束后补一次排程。监听器 panic 按抛错处理：
    /// 报告并以 `ListenerError` 结束这一个 watch，不影响其他 watch。`retires` 帧送达
    /// 后以 `Retired` 结束。
    async fn drain(inner: Arc<WatchInner>) {
        {
            let mut state = lock(&inner.state);
            state.scheduled = false;
            if state.running || state.end.is_some() || !state.started {
                return;
            }
            state.running = true;
        }
        loop {
            let next = {
                let mut state = lock(&inner.state);
                if state.end.is_some() {
                    None
                } else {
                    match state.listener.clone() {
                        None => None,
                        Some(listener) => match state.newest.take() {
                            None => None,
                            Some((value, context)) => {
                                let batch = state.pending.take();
                                let ops = match batch {
                                    PendingTake::Ops(ops) => ops,
                                    PendingTake::Replace => vec![Op::Replace(match &*value {
                                        Some(object) => {
                                            Value::Object(object.clone().into_iter().collect())
                                        }
                                        None => Value::Null,
                                    })],
                                };
                                // 这一帧成为 `value`：被替换掉的旧值随它的最后一个 `Arc` 释放。
                                state.value = Arc::clone(&value);
                                let retires = state.retiring;
                                Some((listener, value, ops, context, retires))
                            }
                        },
                    }
                }
            };
            let Some((listener, value, ops, context, retires)) = next else {
                break;
            };
            let delivery_context: Arc<dyn Context> = without_abort_signal(context);
            // TS 在 `#drain` 里 try/catch 监听器：抛错报告并以 `listener_error` 结束**这一个**
            // watch，其他 watch 不受影响。Rust 的监听器没有错误通道，panic 是等价的抛出。
            let outcome = std::panic::AssertUnwindSafe(listener.call(
                (*value).clone(),
                ops,
                delivery_context,
            ))
            .catch_unwind()
            .await;
            if let Err(payload) = outcome {
                let error = panic_error(payload);
                if lock(&inner.state).end.is_none() {
                    if let Some(report) = &inner.report {
                        report(Box::new(ReportedError(error.to_string())));
                    }
                    Watch::terminate_inner(&inner, WatchEndKind::ListenerError(error.to_string()));
                }
                break;
            }
            if retires {
                Watch::terminate_inner(&inner, WatchEndKind::Retired);
                break;
            }
        }
        let mut state = lock(&inner.state);
        state.running = false;
        // 监听器运行期间到达的帧没有另外的排程：这里补上。
        if state.end.is_none() && state.newest.is_some() && state.started {
            Watch::schedule(&inner, &mut state);
        }
    }

    /// 进入终态：首个终态胜出，之后调用是无操作。
    fn terminate(&self, end: WatchEndKind) {
        Self::terminate_inner(&self.inner, end);
    }

    /// 写入终态、清空待投递帧与取消订阅、唤醒 `closed()` 等待者，并在锁外运行 detach。
    fn terminate_inner(inner: &Arc<WatchInner>, end: WatchEndKind) {
        let detach;
        {
            let mut state = lock(&inner.state);
            if state.end.is_some() {
                return;
            }
            state.end = Some(end.clone());
            state.listener = None;
            state.pending.take();
            state.newest = None;
            state.cancellation = None;
            detach = state.detach.take();
            inner.closed.send_replace(Some(end));
        }
        if let Some(detach) = detach {
            detach();
        }
    }
}

#[async_trait]
impl WatchHandle<Option<JsonObject>> for Watch {
    /// 当前值（初始值，或 `drain()` 最近取出的一帧；退役后为 `None`）。
    ///
    /// 返回的引用绑定到 `&self`：实现把该 `Arc` 记入句柄的 `retained` 列表，保证本次借用
    /// 期间不会被释放。克隆出的句柄不共享 `retained`，各自从空列表开始。
    fn value(&self) -> &Option<JsonObject> {
        let value = {
            let state = lock(&self.inner.state);
            Arc::clone(&state.value)
        };
        // 把值保留在这个句柄上，直到句柄 drop：返回的引用绑定到 `&self`，届时一定仍然有效。
        {
            let mut retained = lock(&self.retained);
            if !retained
                .last()
                .is_some_and(|held| Arc::ptr_eq(held, &value))
            {
                retained.push(Arc::clone(&value));
            }
        }
        let pointer = Arc::as_ptr(&value);
        // SAFETY: `Arc` 的堆分配地址稳定，且同一个 `Arc` 已保存在 `self.retained` 里；返回的引用
        // 生命周期绑定到 `&self`，因此不会比保留更强。
        unsafe { &*pointer }
    }

    /// 安装唯一的监听器。重复 start 或已停止时以 TS 的消息 panic（TS 抛
    /// `Watch is already started` / `Watch is stopped`）。
    ///
    /// `start` 之后才开始投递；未启动时 `advance`/`retire` 只累积最新帧。
    fn start(&self, listener: Arc<dyn WatchListener<Option<JsonObject>>>) {
        let mut state = lock(&self.inner.state);
        if state.started {
            panic!("Watch is already started");
        }
        if state.end.is_some() {
            panic!("Watch is stopped");
        }
        state.started = true;
        state.listener = Some(listener);
        Self::schedule(&self.inner, &mut state);
    }

    /// 幂等停止后续回调，返回终态（已终止时返回已有终态）。
    async fn stop(&self) -> WatchEnd {
        self.terminate(WatchEndKind::Stopped);
        self.closed().await
    }

    /// 等待终态，可重复调用；内部终态通道意外关闭时按 `Ended` 返回。
    async fn closed(&self) -> WatchEnd {
        let mut receiver = self.inner.closed.subscribe();
        loop {
            if let Some(end) = receiver.borrow().clone() {
                return end.to_end();
            }
            if receiver.changed().await.is_err() {
                return WatchEnd::Ended;
            }
        }
    }

    fn is_idle(&self) -> bool {
        let state = lock(&self.inner.state);
        state.pending.size() == 0 && state.newest.is_none() && !state.scheduled && !state.running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Discard;

    #[async_trait]
    impl WatchListener<Option<JsonObject>> for Discard {
        async fn call(
            &self,
            _value: Option<JsonObject>,
            _ops: Vec<Op>,
            _context: Arc<dyn Context>,
        ) {
        }
    }

    /// 回归（review finding 6）：watch 只保留最近送达的值。被替换掉的旧值必须立刻释放，
    /// 而不是像旧的 `WatchInner.values` 竞技场那样留到句柄 drop（TS 的 `release()` 也清空
    /// 除最后一帧外的一切）。
    #[tokio::test]
    async fn a_replaced_watch_value_is_released() {
        let watch = Watch::new(JsonObject::new(), None);
        let first = Arc::new(Some(JsonObject::new()));
        let first_weak = Arc::downgrade(&first);
        {
            let mut state = lock(&watch.inner.state);
            state.started = true;
            state.listener = Some(Arc::new(Discard));
            state.pending.push(&[]);
            state.newest = Some((Arc::clone(&first), Arc::clone(&BACKGROUND_CONTEXT)));
        }
        drop(first);
        Watch::drain(Arc::clone(&watch.inner)).await;
        assert!(
            first_weak.upgrade().is_some(),
            "the delivered frame becomes the current value"
        );

        let second = Arc::new(Some(JsonObject::new()));
        let second_weak = Arc::downgrade(&second);
        {
            let mut state = lock(&watch.inner.state);
            state.pending.push(&[]);
            state.newest = Some((Arc::clone(&second), Arc::clone(&BACKGROUND_CONTEXT)));
        }
        drop(second);
        Watch::drain(Arc::clone(&watch.inner)).await;
        assert!(
            second_weak.upgrade().is_some(),
            "the newest frame becomes the current value"
        );
        assert!(
            first_weak.upgrade().is_none(),
            "the replaced value is released instead of being kept in a history arena"
        );
    }
}
