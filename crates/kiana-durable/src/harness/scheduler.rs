//! `harness/scheduler.ts`：`TaskScheduler` —— 持久任务的状态机调度器。
//!
//! 一个 `TaskScheduler` 是单个 `Harness` 的调度核心：它在 Session 变更线上串行地决定并写入每一次
//! 任务状态转移（预留、abort 标记、runtime 提交、finalize、每个 phase 之前的同步 step），
//! handler 与 join 在线外运行。
//!
//! 结构对应（逐段对应 TS 源）：
//! - 索引：`#live`/`#below`/`#edges`/`#settled`/`#unloaded`/`#runnable`/`#intent` 等
//!   `IndexMap`/`IndexSet`（插入序与 TS 的 `Map`/`Set` 一致，`harness-scheduler-indexes` /
//!   `harness-scheduler-retention` 断言具体数值）。
//! - 所有权树：任务的父是其 owner 任务或所属会话；会话的父是其 owner 任务。`#relist` 向上传播，
//!   `#walkDown` 向下 DFS，background 任务被到达但不进入（除非 `crossBackground`）。
//! - `#decide` 的优先级规则、`withAbortMark`/`withState`/`progressed` 的丢弃语义、
//!   "相同 checkpoint 不算进展"、`completing` 持有，全部由 `harness-scheduler-*` 测试锁定。
//! - `queueMicrotask` 在 Rust 侧映射为 `tokio::spawn`：调度器自己的提交绝不在提交/注册监听里
//!   同步发起，与 TS 的"绝不在提交或注册监听里同步提交"一致。
//!
//! 擦除后的任务定义的存放契约由本模块定义：`ErasedTaskPhase`（阶段与 abort 处理器）、
//! `ErasedTaskMigrate`（迁移函数），与 `ErasedTaskInitial` 同一模式；[`erase_task`] 把
//! JsonValue 形态的 `TaskDefinition` 转成 `AnyTask`。
//!
//! # 执行流程：一次任务调用经过哪些阶段
//!
//! 1. [`TaskScheduler::open`] 先在 Session 上注册三个监听（已提交变化、关闭、registry 变化），
//!    再在**一个**提交里扫描五种非终态记录（`LIVE_STATUSES`）：`running` 记录改回 `pending`
//!    并把 `restarts` 加一，`abandon_on_restart` 的任务进入 `abandoned`。`read_only` 时只载入
//!    索引、不写任何记录，随后直接返回。
//! 2. 含任务记录变化的已提交变化、registry 变化或宿主调用 [`TaskScheduler::resume`] 都会
//!    走 `kick()`：置 `dirty` 并 spawn 一次 `drain()`。`drain()` 循环调用 `reserve()`，
//!    在**一个**提交里挑出可运行任务、写 `running`、为每个任务创建 `Invocation`。
//! 3. `start()` 在提交之外 spawn 调用体：`run()` 反复执行「`step()` 决策 → phase handler →
//!    `step()` 决策」，直到任务不再是 `running` 或 Harness 开始关闭；带 abort 标记的任务走
//!    `run_abort()`。
//! 4. 每次决策都由 `step()` 在 Session 线上用 `commit_transaction` 完成：读已提交记录、按
//!    优先级决定继续/结束/故障，必要时写终态。handler 本身永远在线外运行。
//! 5. 一个任务写终态时若仍持有活的普通 owned 工作，就先写 `completing`；`reconcile()` 的
//!    `finalize()` 在这些工作消失后的提交里把它落成 `terminal`，并对 `faulted`/`orphaned`
//!    调用宿主 `settle_outcome`。
//! 6. [`TaskScheduler::join`] 中止所有调用并等它们结束，然后通过 `write_at_close` 把仍为
//!    `running` 的记录写回 `pending`。close 被视作正常停止而不是 crash，因此不增加 restart 计数。
//!
//! # 并发与锁
//!
//! - 全部可变索引放在一个 `Mutex<State>`（`std::sync::Mutex`）里；`Flags` 用独立原子量，
//!   避免为了检查 `enabled`/`closing`/`dirty` 去抢状态锁。`lock()` 在锁被 panic 毒化时
//!   取回内部值，一次 panic 不会让调度器之后每次取锁都再 panic。
//! - TS 的 `queueMicrotask` 在这里对应 `tokio::spawn`：`kick()`、`schedule_wake()`、
//!   `schedule_reconcile()`、`schedule_sweep()` 都不会在提交或监听回调里同步执行下一段调度。
//!   这也意味着“提交完成后多久开始下一次预留”没有顺序保证，只保证最终会跑。
//! - 宿主回调可以重入调度器（例如 settings 回调里调用 `wake()`），因此调用它们时不能持有
//!   `State` 锁。本文件的做法是先把需要的值拷出来、释放守卫，再调用宿主。
//! - `State` 守卫只用于同步的索引读写，跨线的读写一律通过 Session/Transaction 串行化。
//!
//! # 索引与不变量
//!
//! - `State::live` 是已提交的非终态记录镜像；`settled` 只保存已知终态任务的归属字段；
//!   `unloaded` 表示该任务的 owner 链可能尚未从 Storage 载入，`load_chain()` 会补齐。
//! - `below`/`roots` 只围绕活工作维护。节点下方没有活工作后会被放进 `drop_queue`，
//!   由 `sweep()` 丢弃 `edges`/`settled` 条目，所以索引大小跟随活工作而不是 Session 的全部历史。
//! - 取消意图（`intent`）= 活任务有 abort 标记，或持有非 `completed` 的结果。`reconcile()`
//!   沿所有权树向下传播 abort 标记；遍历会产出 `background` 任务，默认不进入其子树，
//!   级联标记也会跳过它们。
//!
//! # 失败模式
//!
//! - registry / settings / onWake 是宿主回调：本文件用 `catch_unwind` 把 panic 当作 TS 的
//!   throw，多数位置通过 `report()` 变成 `ReportInfo::HostError` 报告，不中断调度。
//! - `reserve()` 与 `reconcile()` 的提交出错不会重试：错误被视为存储失败或 bug，
//!   `fail_session()` 让 Session 失败；`step()` 的提交出错同样先结束调用再失败 Session。
//! - `read_only` 调度器不写、不预留、不请求唤醒；[`TaskScheduler::resume`] 直接返回
//!   [`crate::types::ReadOnlySession`] 错误。
//!
//! # 阅读顺序
//!
//! 先看 `State` 与 [`TaskScheduler`] 的公共方法（`open`/`wake`/`join`/`abort`），再看
//! `observe`/`track`/`relist` 这套索引维护，最后看 `reserve`/`run`/`step`/`decide` 的调用链。
//! 每个方法文档里的 `TS:` 标注给出对应的 TS 源码位置。

use std::any::Any;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use indexmap::{IndexMap, IndexSet};
use kiana_ai::models::Models;
use kiana_chord::context::{
    AbortReason, AbortSignal, Context, ContextRef, abort_error, await_with_context,
    with_abort_signal, with_telemetry, without_abort_signal,
};
use kiana_chord::delta::index::owned_slice;
use kiana_chord::json::copy_json;
use kiana_json::js_object::deep_equal;
use kiana_telemetry::metrics::{TelemetryHistogram, TelemetryUpDownCounter, create_typed_metrics};
use kiana_telemetry::noop::{noop_telemetry_context_as_context, noop_telemetry_metrics};
use kiana_telemetry::{
    AttributeValue, SpanAttributes, SpanLink, SpanStatus, StatusError, TelemetryCarrier,
    TelemetryContext, create_typed_span_starter,
};
use tokio::sync::Notify;

use crate::env::ExecutionEnv;
use crate::harness::agent::{DEFAULT_RESTART_LIMIT, agent_hooks};
use crate::harness::context::{ContextRange, read_context_from};
use crate::harness::telemetry::DURABLE_TELEMETRY_SCHEMA;
use crate::harness::types::{
    Agent, AnyTask, AnyTaskDefinition, ContextView, ContextViewOptions, ConversationHandle,
    HookRunner, RegistryReader, RegistrySnapshot, SchedulingState, Settings, SettledTask,
    TaskInspection, TaskInspectionState, TaskRuntime, TaskRuntimeChange, Wake,
};
use crate::harness::util::{WaitResult, Waiters, closed_error, scan_all};
use crate::session::session::{ObservedCommit, TransactionChange, context_handle};
use crate::session::transaction::{Transaction, TransactionScope};
use crate::session::{SessionClosed, SessionImpl};
use crate::timers::{Disarm, Timers};
use crate::types::{
    BoxError, CommitChange, CommitOptions, ConversationId, DocTarget, DocumentObserver,
    DocumentReader, DocumentWatch, EntryId, EntryRecord, HostCallback, JoinPolicy, JsonObject,
    JsonValue, ReportFrom, ReportInfo, ReportTask, SessionFailed, Storage, StorageWrite,
    SubmissionKind, SubmissionQuery, SubmissionStatus, TableCommitChange, TaskAbortReason,
    TaskDefinition, TaskFaultedReason, TaskId, TaskMigrate, TaskOutcome, TaskOutcomeError,
    TaskOutcomeStatus, TaskOutdatedReason, TaskPhase, TaskQuery, TaskRecord, TaskState, TaskStatus,
    Tx, error_message,
};

pub use crate::harness::types::{BlockedReason, BlockedSubject, SchedulerOutcome};

// ---------------------------------------------------------------------------
// 别名与常量
// ---------------------------------------------------------------------------

/// `TaskRecord<JsonValue, JsonValue, JsonValue>`（TS: `AnyTaskRecord`）。
type AnyTaskRecord = TaskRecord;

/// 仍是活工作、还能跑代码的检查点（TS: `Checkpoint` 在 Rust 侧就是 JSON）。
type Checkpoint = JsonValue;

/// 一轮扫描的页大小。
const SCAN_PAGE_SIZE: usize = 256;

/// 非终态、需要调度器照看的任务状态。
const LIVE_STATUSES: [TaskStatus; 5] = [
    TaskStatus::Pending,
    TaskStatus::Running,
    TaskStatus::Waiting,
    TaskStatus::Sleeping,
    TaskStatus::Completing,
];

/// 宿主 registry 读取失败时报告的回调名（TS: `REGISTRY`）。
const REGISTRY: ReportInfo = ReportInfo::HostError {
    callback: HostCallback::Registry,
    task: None,
};
/// 宿主 settings 读取失败时报告的回调名（TS: `SETTINGS`）。
const SETTINGS: ReportInfo = ReportInfo::HostError {
    callback: HostCallback::Settings,
    task: None,
};
/// 宿主 onWake 抛错时报告的回调名（TS: `ON_WAKE`）。
const ON_WAKE: ReportInfo = ReportInfo::HostError {
    callback: HostCallback::OnWake,
    task: None,
};

/// 取 `State` 锁；锁被 panic 毒化时取回内部值而不是继续 panic。
///
/// 持锁段只做索引读写，不运行扩展代码、不调用宿主回调。恢复内部值可避免一次 panic 之后
/// 每个调度动作都因 `PoisonError` 再次失败；代价是 panic 当时可能只写了一半的索引，
/// 这里没有撤销机制，调用方不能假设恢复出的状态是一致的。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn io_error(message: impl Into<String>) -> BoxError {
    Box::new(io::Error::other(message.into()))
}

/// 一个共享错误的新装箱副本（`BoxError` 不可克隆）。
///
/// `Waiters::reject_all`、`blocked_reason` 的迁移失败等位置需要给每个等待者/每次读取一个
/// 独立的 `BoxError`；`Box<dyn Error>` 不能克隆，于是用 `Arc` 共享同一个源错误，
/// 这一层包装只转发 `Display`/`source`。
#[derive(Debug)]
struct SharedError(Arc<dyn std::error::Error + Send + Sync>);

impl std::fmt::Display for SharedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

fn clone_error(error: &Arc<dyn std::error::Error + Send + Sync>) -> BoxError {
    Box::new(SharedError(Arc::clone(error)))
}

/// 「调用已结束」错误（TS: `endedError(invocation)`）。
///
/// 出现在两个通道：runtime 在被访问时返回它（`ensure_live()`），以及 `end()` 把它作为
/// `AbortReason::Error` 交给 invocation 的 `AbortSignal`，让绑定该调用的等待（例如
/// 工具的 `waitForTask()`）随调用结束一起失败。
#[derive(Debug)]
struct EndedError {
    task: TaskId,
}

impl std::fmt::Display for EndedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Task {} invocation has ended", self.task)
    }
}

impl std::error::Error for EndedError {}

fn ended_error(invocation: &Invocation) -> BoxError {
    Box::new(EndedError {
        task: invocation.task_id,
    })
}

// ---------------------------------------------------------------------------
// 状态判别值的字符串形式（错误消息与文档一致）
// ---------------------------------------------------------------------------

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

fn outcome_status(outcome: &TaskOutcome) -> TaskOutcomeStatus {
    match outcome {
        TaskOutcome::Completed { .. } => TaskOutcomeStatus::Completed,
        TaskOutcome::Failed { .. } => TaskOutcomeStatus::Failed,
        TaskOutcome::Aborted { .. } => TaskOutcomeStatus::Aborted,
        TaskOutcome::Orphaned { .. } => TaskOutcomeStatus::Orphaned,
        TaskOutcome::Faulted { .. } => TaskOutcomeStatus::Faulted,
    }
}

// ---------------------------------------------------------------------------
// 记录、节点与所有权
// ---------------------------------------------------------------------------

/// 所有权树的一个节点：任务，或记作 `c<id>` 的会话（TS: `Node`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    /// 任务。
    Task(TaskId),
    /// 会话。
    Conversation(ConversationId),
}

fn conversation_node(id: ConversationId) -> Node {
    Node::Conversation(id)
}

/// 任务的不可变拥有权字段（TS: `TaskNode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskNode {
    conversation_id: ConversationId,
    owner: Option<TaskId>,
    background: bool,
}

/// 向上走的一步：owner 任务或会话（TS: `Up`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Up {
    Task(TaskId),
    Conversation(ConversationId),
}

/// `#above()` 产出的一步（TS: `Step`）。
///
/// `Unknown` 表示走到一个还没载入 owner 链的任务/会话：向上遍历到此为止。`in_scope()`
/// 因此返回 `None`（“无法判断”），而不是把它当成“不在作用域”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Task { task: TaskId, node: TaskNode },
    Conversation(ConversationId),
    Unknown,
}

/// 遍历范围：一个会话，或所有 ownerless 会话（TS: `Scope`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Conversation(ConversationId),
    Roots,
}

fn parent_node(node: &TaskNode) -> Node {
    match node.owner {
        Some(owner) => Node::Task(owner),
        None => conversation_node(node.conversation_id),
    }
}

fn parent_of(node: &TaskNode) -> Up {
    match node.owner {
        Some(task) => Up::Task(task),
        None => Up::Conversation(node.conversation_id),
    }
}

fn node_of(record: &AnyTaskRecord) -> TaskNode {
    TaskNode {
        conversation_id: record.conversation_id,
        owner: record.owner,
        background: record.background,
    }
}

/// `blockedSubject(record)`：`blockedReason()` 读取的记录片段。
///
/// 只是一份拷贝，不借用调度器状态：任务图（`task_graph.rs`）用它从已提交记录构造查询输入，
/// 再交给 `TaskScheduler::blocked_reason()`。字段缺失（`abort_reason`/`restarts`）保持 `None`。
pub fn blocked_subject(record: &AnyTaskRecord) -> BlockedSubject {
    BlockedSubject {
        id: record.id,
        kind: record.kind.clone(),
        version: record.version,
        abort_requested: record.abort_requested,
        abort_reason: record.abort_reason,
        restarts: record.restarts,
        status: record.state.status(),
    }
}

// ---------------------------------------------------------------------------
// 调用绑定
// ---------------------------------------------------------------------------

/// 一次调用的绑定：它的信号，以及"调用已结束即抛错"的检查（TS: `InvocationBinding`）。
///
/// 任务通过 runtime 的 `conversation()` 取得这个绑定，Harness 用它包出会话句柄
/// （`BoundConversation`）：每次跨线操作前调用 `check()`，并把 `signal` 叠加到调用方上下文。
/// 因此任务结束后，它持有的会话句柄既会被 `check()` 拒绝，也会被信号取消。
#[derive(Clone)]
pub struct InvocationBinding {
    /// 调用被中止时触发。调用结束（`end()`）也会以 `EndedError` 中止它。
    pub signal: AbortSignal,
    /// 调用已结束时返回错误。
    pub check: Arc<dyn Fn() -> Result<(), BoxError> + Send + Sync>,
}

// ---------------------------------------------------------------------------
// TaskSchedulerOptions
// ---------------------------------------------------------------------------

/// 解析一个会话的 agent（TS `TaskSchedulerOptions.agent`）。
pub type AgentResolver = Arc<
    dyn Fn(
            ConversationId,
            Arc<dyn RegistrySnapshot>,
            ContextRef,
        ) -> BoxFuture<'static, Result<Arc<Agent>, BoxError>>
        + Send
        + Sync,
>;

/// 读取设置（TS `TaskSchedulerOptions.settings`；宿主回调可能失败）。
pub type SettingsResolver = Arc<dyn Fn() -> Result<Arc<Settings>, BoxError> + Send + Sync>;

/// 构建会话环境（TS `TaskSchedulerOptions.env`）。
pub type EnvResolver = Arc<
    dyn Fn(
            ConversationId,
            ContextRef,
        ) -> BoxFuture<'static, Result<Option<Arc<dyn ExecutionEnv>>, BoxError>>
        + Send
        + Sync,
>;

/// 关闭时直写 Storage 的回调（TS `TaskSchedulerOptions.writeAtClose`）。
pub type WriteAtClose = Arc<
    dyn Fn(
            Arc<dyn Fn() -> Vec<StorageWrite> + Send + Sync>,
        ) -> BoxFuture<'static, Result<(), BoxError>>
        + Send
        + Sync,
>;

/// 非致命失败报告（TS `TaskSchedulerOptions.report`）。
pub type ReportFn = Arc<
    dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo, Option<&dyn Context>) + Send + Sync,
>;

/// 调度器自己写入终态时的 Harness 清理（TS `TaskSchedulerOptions.settleOutcome`）。
pub type SettleOutcome = Arc<
    dyn for<'a> Fn(
            &'a Transaction,
            &'a AnyTaskRecord,
            &'a SchedulerOutcome,
        ) -> BoxFuture<'a, Result<(), BoxError>>
        + Send
        + Sync,
>;

/// 撤回一个会话排队的输入（TS `TaskSchedulerOptions.withdrawInputs`）。
pub type WithdrawInputs = Arc<
    dyn for<'a> Fn(&'a Transaction, ConversationId) -> BoxFuture<'a, Result<(), BoxError>>
        + Send
        + Sync,
>;

/// 调用绑定的会话句柄（TS `TaskSchedulerOptions.conversation`）。
pub type ConversationResolver = Arc<
    dyn Fn(
            ConversationId,
            InvocationBinding,
            ContextRef,
        ) -> BoxFuture<'static, Result<Option<Arc<dyn ConversationHandle>>, BoxError>>
        + Send
        + Sync,
>;

/// `TaskScheduler` 的构造参数（TS: `TaskSchedulerOptions`）。
///
/// 回调分两类：同步回调（`settings`/`models`/`now`/`on_wake`/`report` 与 registry 读取）
/// 由调度器按需直接调用；返回 `BoxFuture` 的异步回调（`agent`/`env`/`write_at_close`/
/// `settle_outcome`/`withdraw_inputs`/`conversation`）在调用点 `await`。两类都要求
/// `Send + Sync`。本文件的调用点都不在持有内部 `State` 锁时调用宿主回调，因此回调里
/// 重入 `wake()`/`blocked_reason()` 等方法不会自死锁。回调运行在调用它的线程上（可能是
/// Session 线或某个调度任务），应避免长时间阻塞。
pub struct TaskSchedulerOptions {
    /// 遥测适配器。
    pub telemetry: Option<Arc<dyn TelemetryContext>>,
    /// Session。
    pub session: SessionImpl,
    /// Storage。
    pub storage: Arc<dyn Storage>,
    /// registry 读端。
    pub registry: Arc<dyn RegistryReader>,
    /// 每次访问读取当前模型。
    pub models: Arc<dyn Fn() -> Arc<dyn Models> + Send + Sync>,
    /// 每个 phase 至多解析一次 agent。
    pub agent: AgentResolver,
    /// 每次访问读取设置。
    pub settings: SettingsResolver,
    /// 构建会话环境。
    pub env: EnvResolver,
    /// Harness 时钟（epoch 毫秒）。
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// 睡眠任务的唤醒回调。
    pub on_wake: Option<Arc<dyn Fn(&Wake) + Send + Sync>>,
    /// 宿主计时器。
    pub timers: Arc<Timers>,
    /// 关闭时直写。
    pub write_at_close: WriteAtClose,
    /// 错误报告。
    pub report: ReportFn,
    /// 调度器终态的清理。
    pub settle_outcome: SettleOutcome,
    /// 撤回排队输入。
    pub withdraw_inputs: WithdrawInputs,
    /// 调用绑定的会话句柄。
    pub conversation: ConversationResolver,
    /// 调度器提交与调用用的上下文（不携带调用者取消）。
    pub context: ContextRef,
    /// 只读打开。
    pub read_only: Option<bool>,
}

// ---------------------------------------------------------------------------
// 索引尺寸 / 检查结果
// ---------------------------------------------------------------------------

/// `indexSizes`：所有权索引的尺寸（测试用，TS 同名字段）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerIndexSizes {
    /// `#live` 的条目数。
    pub live: usize,
    /// `#settled` 的条目数。
    pub settled: usize,
    /// `#edges` 的条目数。
    pub edges: usize,
    /// `#below` 的全部子节点数。
    pub below: usize,
    /// `#roots` 的条目数。
    pub roots: usize,
    /// `#unloaded` 的条目数。
    pub unloaded: usize,
    /// `#dropQueue` 的条目数。
    pub drop_queue: usize,
}

/// `inspect()` 的结果：调度状态与每个活任务的派生状态（TS `{ scheduling, tasks }`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerInspection {
    /// 调度状态。
    pub scheduling: SchedulingState,
    /// 任务。
    pub tasks: Vec<TaskInspection>,
}

/// `abort()` 的结果（TS: `"marked" | "terminal"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAbortResult {
    /// 调用前任务不是终态：本次写入了 abort 标记，或（没有可接管的定义时）直接把它落定为
    /// `orphaned`；若任务本来就有 abort 请求，也可能只是复用了已有标记。
    Marked,
    /// 读取时已经是终态。
    Terminal,
}

// ---------------------------------------------------------------------------
// 擦除后的任务定义契约
// ---------------------------------------------------------------------------

/// 擦除后的阶段/abort 处理器（`AnyTaskDefinition.phases`/`.abort` 的存放契约）。
///
/// 与 [`crate::session::ErasedTaskInitial`] 同一模式：`Arc<dyn Any>` 里存放本结构，调度器
/// downcast 后调用。用 [`ErasedTaskPhase::new`] 从带类型的处理器构造。
pub(crate) struct ErasedTaskPhase(pub(crate) Arc<dyn ErasedPhaseHandler>);

impl std::fmt::Debug for ErasedTaskPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ErasedTaskPhase")
    }
}

impl ErasedTaskPhase {
    /// 擦除一个 `TaskDefinition<JsonValue, JsonValue, JsonValue, H>` 的阶段处理器。
    pub(crate) fn new<H>(phase: Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, H>>) -> Self
    where
        H: Send + Sync + 'static,
    {
        ErasedTaskPhase(Arc::new(PhaseAdapter { phase }))
    }
}

struct PhaseAdapter<H> {
    phase: Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, H>>,
}

#[async_trait]
impl<H> ErasedPhaseHandler for PhaseAdapter<H>
where
    H: Send + Sync + 'static,
{
    async fn run(
        &self,
        task: &AnyTaskRecord,
        runtime: &SchedulerRuntime,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, H> = runtime;
        self.phase.run(task, runtime, context).await
    }
}

/// 擦除后的阶段处理器：接收调度器的 runtime（TS 的 `ErasedDefinition.phases[phase]`）。
#[async_trait]
pub(crate) trait ErasedPhaseHandler: Send + Sync {
    /// 运行一个阶段（或 abort）。
    async fn run(
        &self,
        task: &AnyTaskRecord,
        runtime: &SchedulerRuntime,
        context: &dyn Context,
    ) -> Result<(), BoxError>;
}

/// 擦除后的迁移函数（`AnyTaskDefinition.migrate` 的存放契约）。
pub(crate) struct ErasedTaskMigrate(pub(crate) TaskMigrate<JsonValue, JsonValue>);

impl std::fmt::Debug for ErasedTaskMigrate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ErasedTaskMigrate")
    }
}

/// 把 `TaskDefinition<JsonValue, JsonValue, JsonValue, H>` 擦除成可注册的 [`AnyTask`]。
///
/// registry 与内置任务用它填充 `AnyTaskDefinition`（`initial` 用
/// `ErasedTaskInitial` 的契约）。`phases` 的键是 checkpoint 里的 phase 名；调度器运行时
/// 按名字取出并 downcast 回 `ErasedTaskPhase`。
///
/// 擦除后定义以 `Arc<dyn Any>` 存放，`same_task()` 用 `Arc::ptr_eq` 判断“同一个定义对象”，
/// 与 TS 的 `===` 对象身份一致：克隆出来的 `AnyTask` 仍共享同一份 `definition`。
pub fn erase_task<H>(task: &TaskDefinition<JsonValue, JsonValue, JsonValue, H>) -> AnyTask
where
    H: Send + Sync + 'static,
{
    let mut phases = IndexMap::new();
    for (name, phase) in &task.phases {
        phases.insert(
            name.clone(),
            Arc::new(ErasedTaskPhase::new(Arc::clone(phase))) as Arc<dyn Any + Send + Sync>,
        );
    }
    let initial = Arc::clone(&task.initial);
    let definition = AnyTaskDefinition {
        name: task.name.clone(),
        version: task.version,
        initial: Arc::new(crate::session::ErasedTaskInitial(Arc::new(move |input| {
            initial(input)
        }))) as Arc<dyn Any + Send + Sync>,
        phases,
        abort: Arc::new(ErasedTaskPhase::new(Arc::clone(&task.abort)))
            as Arc<dyn Any + Send + Sync>,
        migrate: task
            .migrate
            .clone()
            .map(|migrate| Arc::new(ErasedTaskMigrate(migrate)) as Arc<dyn Any + Send + Sync>),
        hooks: task
            .hooks
            .clone()
            .map(|hooks| hooks as Arc<dyn Any + Send + Sync>),
    };
    AnyTask {
        definition: Arc::new(definition),
    }
}

/// 取一个已擦除定义的阶段处理器（TS: `erased(task).phases[phase]`）。
fn erased_phase(
    definition: &AnyTaskDefinition,
    phase: &str,
) -> Result<Arc<dyn ErasedPhaseHandler>, BoxError> {
    // TS 在 phase 的 try 里调用 `erased(state.task).phases[checkpoint.phase]`；缺失时抛出
    // TypeError，其 message（逐字）成为阶段失败。
    let handler = definition
        .phases
        .get(phase)
        .ok_or_else(|| io_error("erased(...).phases[checkpoint.phase] is not a function"))?;
    handler
        .downcast_ref::<ErasedTaskPhase>()
        .map(|erased| Arc::clone(&erased.0))
        .ok_or_else(|| io_error("task definition phase is not a kiana-durable ErasedTaskPhase"))
}

/// 取一个已擦除定义的 abort 处理器。
fn erased_abort(definition: &AnyTaskDefinition) -> Result<Arc<dyn ErasedPhaseHandler>, BoxError> {
    definition
        .abort
        .downcast_ref::<ErasedTaskPhase>()
        .map(|erased| Arc::clone(&erased.0))
        // TS: `erased(reservation.task).abort(...)` 缺失时 TypeError。
        .ok_or_else(|| io_error("erased(...).abort is not a function"))
}

/// 调用一个已擦除定义的 migrate（TS: `definition.migrate(input, checkpoint, version)`）。
fn erased_migrate(
    definition: &AnyTaskDefinition,
    input: &JsonValue,
    checkpoint: &JsonValue,
    version: i64,
) -> Result<(JsonValue, JsonValue), BoxError> {
    let migrate = definition.migrate.as_ref().ok_or_else(|| {
        io_error(format!(
            "Task definition {} has no migration",
            definition.name
        ))
    })?;
    if let Some(erased) = migrate.downcast_ref::<ErasedTaskMigrate>() {
        return (erased.0)(input.clone(), checkpoint.clone(), version);
    }
    if let Some(plain) = migrate.downcast_ref::<TaskMigrate<JsonValue, JsonValue>>() {
        return plain(input.clone(), checkpoint.clone(), version);
    }
    Err(io_error(
        "task definition migrate is not a kiana-durable ErasedTaskMigrate",
    ))
}

fn missing_migration(record: &AnyTaskRecord, definition: &AnyTaskDefinition) -> BoxError {
    io_error(format!(
        "Task {} version {} has no migration from {}",
        record.kind, definition.version, record.version
    ))
}

/// TS 里 `AnyTask` 的身份比较（`===`）：同一个定义对象。共享的定义 Arc 是身份。
fn same_task(left: &AnyTask, right: &AnyTask) -> bool {
    Arc::ptr_eq(&left.definition, &right.definition)
}

// ---------------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------------

/// 调用的运行模式（TS: `Invocation.mode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvocationMode {
    Run,
    Abort,
}

impl InvocationMode {
    fn as_str(self) -> &'static str {
        match self {
            InvocationMode::Run => "run",
            InvocationMode::Abort => "abort",
        }
    }
}

/// 一个已结束调用留下的可分享撤销函数。
#[derive(Clone)]
struct SharedDisarm(Arc<Mutex<Option<Disarm>>>);

impl SharedDisarm {
    fn new(disarm: Disarm) -> Self {
        SharedDisarm(Arc::new(Mutex::new(Some(disarm))))
    }

    fn disarm(&self) {
        if let Some(disarm) = lock(&self.0).take() {
            disarm();
        }
    }
}

/// 任务在内存里的一次执行（run 或 abort 模式，TS: `Invocation`）。
///
/// 生命周期有两个独立标志：
/// - `ended`：调用已经做出“不再继续”的决定（`end()` 置位）。置位后 runtime 的提交、
///   等待、watch、计时器都拒绝或停止；它是幂等的。
/// - `finished`：`start()` 里调用体完全退出、清理完 record/links/fault 后才置位；
///   `join()`/`abort()` 通过 `wait_finished()` 等它。
///
/// `record` 是 `#live` 之外的一份副本：任务写终态离开 `#live` 后，phase 收尾仍需要读到
/// 结果来生成遥测；调用结束后由 `start()` 清空。
struct Invocation {
    task_id: TaskId,
    kind: String,
    /// 预留时安装该定义的扩展；内置任务没有。
    extension: Option<String>,
    conversation_id: ConversationId,
    mode: InvocationMode,
    controller: AbortSignal,
    /// 只在任务第一次启动时从记录继承：`create_invocation` 看到 `started_at` 已存在就丢弃。
    links: Mutex<Option<Vec<TelemetryCarrier>>>,
    /// 传给 handler 的上下文；由 `controller` 取消，phase 执行期间会临时叠加 span。
    context: Mutex<ContextRef>,
    /// 离开 `#live` 后仍保留终态记录，直到调用结束。
    record: Mutex<Option<AnyTaskRecord>>,
    /// `step()` 写入故障终态时留下的结果，phase 收尾用它生成 span 状态。
    fault: Mutex<Option<SchedulerOutcome>>,
    /// 通过 runtime 取得的 watch；调用结束时停止。
    watches: Mutex<Vec<DocumentWatch>>,
    /// 通过 runtime 装载的计时器；调用结束时撤销。标记用于运行时计时器自撤销。
    timers: Mutex<Vec<(Arc<()>, SharedDisarm)>>,
    ended: AtomicBool,
    finished: AtomicBool,
    finished_notify: Notify,
}

impl Invocation {
    fn is_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    fn set_record(&self, record: AnyTaskRecord) {
        *lock(&self.record) = Some(record);
    }

    fn record(&self) -> Option<AnyTaskRecord> {
        lock(&self.record).clone()
    }

    fn context(&self) -> ContextRef {
        Arc::clone(&lock(&self.context))
    }

    fn set_context(&self, context: ContextRef) {
        *lock(&self.context) = context;
    }

    fn links(&self) -> Option<Vec<TelemetryCarrier>> {
        lock(&self.links).clone()
    }

    fn take_fault(&self) -> Option<SchedulerOutcome> {
        lock(&self.fault).clone()
    }

    fn finish(&self) {
        if !self.finished.swap(true, Ordering::SeqCst) {
            self.finished_notify.notify_waiters();
        }
    }

    async fn wait_finished(&self) {
        loop {
            if self.finished.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.finished_notify.notified();
            if self.finished.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

// ---------------------------------------------------------------------------
// 内部索引状态
// ---------------------------------------------------------------------------

/// 迁移失败的定义与错误（TS: `#failedMigrations` 的值）。
struct FailedMigration {
    task: AnyTask,
    error: Arc<dyn std::error::Error + Send + Sync>,
}

/// 每个会话最近一次通过任务 runtime 读取的上下文范围（TS: `#contexts` 的值）。
struct KeptContext {
    range: ContextRange,
    idle_since: Option<i64>,
}

/// 一个前台/后台计时器（TS: `{ at, disarm }`）。
struct TimerHandle {
    /// 到期时刻（JS 的毫秒数可以是小数；TS 用 `number` 比较与 `at - now()` 计算延迟）。
    at: f64,
    disarm: SharedDisarm,
}

/// `subscribeBlocked` 的监听项。
struct BlockedListener {
    id: u64,
    listener: Arc<dyn Fn() + Send + Sync>,
}

/// 调度器的全部可变索引（TS 类字段）。
struct State {
    /// 已提交的非终态记录镜像（与 `LIVE_STATUSES` 对应）。提交监听在 Session 线上更新它，
    /// 因此线上代码读到的就是已提交状态。
    live: IndexMap<TaskId, AnyTaskRecord>,
    /// 每个会话最近一次通过任务 runtime 读取的上下文范围；不持久化，`seal()` 清空。
    contexts: IndexMap<ConversationId, KeptContext>,
    /// 最早的空闲 context 到期计时器（后台计时器）。
    expiry: Option<TimerHandle>,
    /// 当前在跑的调用，每个任务至多一个；`reserve_inner` 见到它就不再预留。
    invocations: IndexMap<TaskId, Arc<Invocation>>,
    /// `wait_for_task` 的等待者，按任务 ID 分组。
    task_waiters: Waiters<TaskId, SettledTask>,
    /// `wait_for_idle` 的等待者；键 `None` 表示所有 ownerless 根。
    idle_waiters: Waiters<Option<ConversationId>, ()>,
    /// 迁移失败的定义与错误；同一任务再次 `resolve` 时据此保持 `MigrationFailed`。
    failed_migrations: IndexMap<TaskId, FailedMigration>,
    /// 每个已知会话的 owner 任务；`None` 表示 ownerless。键缺席表示会话链尚未载入。
    edges: IndexMap<ConversationId, Option<TaskId>>,
    /// 已知终态任务的拥有权字段。
    settled: IndexMap<TaskId, TaskNode>,
    /// 等待 `sweep()` 处理的节点；下方重新出现活工作时会被 `sweep` 跳过。
    drop_queue: IndexSet<Node>,
    /// 活工作周围的向下树（父节点 → 子节点），节点可以是任务或会话。
    below: IndexMap<Node, IndexSet<Node>>,
    /// 没有任何 owner 的会话，是普通遍历的起点。
    roots: IndexSet<ConversationId>,
    /// owner 链可能还没从 Storage 载入的已知任务；`load_chain()` 负责补齐。
    unloaded: IndexSet<TaskId>,
    /// 有取消意图的活任务：有 abort 标记，或持有非 `completed` 结果。
    intent: IndexSet<TaskId>,
    /// 非 `completing` 的活任务候选；调用结束、记录变化或依赖结束时重新加入。
    runnable: IndexSet<TaskId>,
    /// 需要检查能否落成 `terminal` 的 `completing` 任务。
    finalize_checks: IndexSet<TaskId>,
    /// 反向等待边：被等待任务 → 等待它的任务。用于失败时唤醒 `failFast` 检查。
    waiters: IndexMap<TaskId, IndexSet<TaskId>>,
    /// 需要检查其 `failFast` 依赖是否失败的等待者。
    fail_fast_checks: IndexSet<TaskId>,
    /// `open()` 时带 `abandonOnRestart` 的任务；预留的第一趟先给它们打 `restart` 标记。
    abandoned: IndexSet<TaskId>,
    /// 已报告停泊、等 `retry()` 或上限升高的任务（TS: `#parkedTasks`）。
    parked_tasks: IndexSet<TaskId>,
    /// 处于 `sleeping` 的任务，用于 `wake_due()`。
    sleeping: IndexSet<TaskId>,
    /// `open()` 载入、尚未被任何 phase 消费的任务；span 的 `resumed` 属性由它决定。
    resumed: IndexSet<TaskId>,
    /// 最早睡眠的前台计时器（到点后 `wake_due()` + `kick()`）。
    timer: Option<TimerHandle>,
    /// 上一次成功送达宿主的 `Wake`；相同值不重复上报。
    last_wake: Option<Wake>,
    /// 有级联 abort 标记待 `reconcile()` 派生。
    cascade_pending: bool,
    /// `registry.snapshot()` 失败是否已报告；只报告第一次，避免刷屏。
    snapshot_reported: bool,
    /// 进行中的 reconcile 工作数；`wake()` 用它判断“还有事没做完”。
    reconciling_work: usize,
    /// `subscribe_blocked` 的监听器（TS 用 `Set` 语义，重复注册是空操作）。
    blocked_listeners: Vec<BlockedListener>,
    blocked_listener_index: u64,
    /// registry 订阅的注销函数（`#seal` 调用）。
    unsubscribe_registry: Option<crate::types::Unsubscribe>,
}

impl Default for State {
    fn default() -> Self {
        State {
            live: IndexMap::new(),
            contexts: IndexMap::new(),
            expiry: None,
            invocations: IndexMap::new(),
            task_waiters: Waiters::new(),
            idle_waiters: Waiters::new(),
            failed_migrations: IndexMap::new(),
            edges: IndexMap::new(),
            settled: IndexMap::new(),
            drop_queue: IndexSet::new(),
            below: IndexMap::new(),
            roots: IndexSet::new(),
            unloaded: IndexSet::new(),
            intent: IndexSet::new(),
            runnable: IndexSet::new(),
            finalize_checks: IndexSet::new(),
            waiters: IndexMap::new(),
            fail_fast_checks: IndexSet::new(),
            abandoned: IndexSet::new(),
            parked_tasks: IndexSet::new(),
            sleeping: IndexSet::new(),
            resumed: IndexSet::new(),
            timer: None,
            last_wake: None,
            cascade_pending: false,
            snapshot_reported: false,
            reconciling_work: 0,
            blocked_listeners: Vec::new(),
            blocked_listener_index: 0,
            unsubscribe_registry: None,
        }
    }
}

/// 调度器的调度标志（不放在 `State` 里，避免重入锁）。
///
/// `dirty`/`draining` 是 `kick()` → `drain()` 的单飞闩：同一时刻至多一个 drain 循环，
/// 循环结束前再查一次 `dirty`，避免漏掉最后一次变化。
///
/// `wake_scheduled`/`reconcile_scheduled`/`sweep_scheduled` 同样是单飞闩，对应 TS 用
/// `queueMicrotask` 去重的三段后台工作：置位者 spawn，执行者先清位再干活。
#[derive(Default)]
struct Flags {
    enabled: AtomicBool,
    closing: AtomicBool,
    dirty: AtomicBool,
    draining: AtomicBool,
    wake_scheduled: AtomicBool,
    reconcile_scheduled: AtomicBool,
    sweep_scheduled: AtomicBool,
}

fn flag(flag: &AtomicBool) -> bool {
    flag.load(Ordering::SeqCst)
}

/// 调度器的不可变依赖。
struct Deps {
    telemetry: Arc<dyn TelemetryContext>,
    active_metric: Arc<dyn TelemetryUpDownCounter>,
    duration_metric: Arc<dyn TelemetryHistogram>,
    session: SessionImpl,
    storage: Arc<dyn Storage>,
    registry: Arc<dyn RegistryReader>,
    models: Arc<dyn Fn() -> Arc<dyn Models> + Send + Sync>,
    agent: AgentResolver,
    settings: SettingsResolver,
    env: EnvResolver,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    on_wake: Option<Arc<dyn Fn(&Wake) + Send + Sync>>,
    timers: Arc<Timers>,
    write_at_close: WriteAtClose,
    report: ReportFn,
    settle_outcome: SettleOutcome,
    withdraw_inputs: WithdrawInputs,
    conversation: ConversationResolver,
    context: ContextRef,
    read_only: bool,
}

/// 一个 Harness 的持久任务调度器（TS: `TaskScheduler`）。
///
/// `Clone` 是廉价的：所有克隆共享同一份 `Deps`、`State` 与 `Flags`，因此可以在提交回调、
/// spawn 出去的调用体和宿主回调之间自由传递，不需要额外同步。
#[derive(Clone)]
pub struct TaskScheduler {
    deps: Arc<Deps>,
    state: Arc<Mutex<State>>,
    flags: Arc<Flags>,
}

impl TaskScheduler {
    /// 构造调度器（TS: `new TaskScheduler(options)`）。
    ///
    /// # Panics
    /// 本 crate 的 `DURABLE_TELEMETRY_SCHEMA` 缺少 `kiana.durable.tasks.active` 或
    /// `kiana.durable.phase.duration` 定义时 panic；这两个指标由 schema 常量固定提供。
    pub fn new(options: TaskSchedulerOptions) -> Self {
        let telemetry = options
            .telemetry
            .unwrap_or_else(noop_telemetry_context_as_context);
        let backend = telemetry.metrics().unwrap_or_else(noop_telemetry_metrics);
        let definitions = DURABLE_TELEMETRY_SCHEMA.metrics.clone().unwrap_or_default();
        let metrics = create_typed_metrics(definitions, backend);
        let active_metric = metrics
            .up_down_counter("kiana.durable.tasks.active")
            .expect("the durable telemetry schema defines kiana.durable.tasks.active");
        let duration_metric = metrics
            .histogram("kiana.durable.phase.duration")
            .expect("the durable telemetry schema defines kiana.durable.phase.duration");
        TaskScheduler {
            deps: Arc::new(Deps {
                telemetry,
                active_metric,
                duration_metric,
                session: options.session,
                storage: options.storage,
                registry: options.registry,
                models: options.models,
                agent: options.agent,
                settings: options.settings,
                env: options.env,
                now: options.now,
                on_wake: options.on_wake,
                timers: options.timers,
                write_at_close: options.write_at_close,
                report: options.report,
                settle_outcome: options.settle_outcome,
                withdraw_inputs: options.withdraw_inputs,
                conversation: options.conversation,
                context: options.context,
                read_only: options.read_only.unwrap_or(false),
            }),
            state: Arc::new(Mutex::new(State::default())),
            flags: Arc::new(Flags::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// `#closing`。
    fn closing(&self) -> bool {
        flag(&self.flags.closing)
    }

    /// `#enabled`。
    fn enabled(&self) -> bool {
        flag(&self.flags.enabled)
    }

    fn report(
        &self,
        error: &(dyn std::error::Error + Send + Sync),
        info: ReportInfo,
        context: Option<&dyn Context>,
    ) {
        (self.deps.report)(error, &info, context);
    }

    fn now(&self) -> i64 {
        (self.deps.now)()
    }

    /// `settings.restartLimit`，宿主抛错时用默认值（TS: `#restartLimit`）。
    fn restart_limit(&self) -> f64 {
        match (self.deps.settings)() {
            Ok(settings) => settings.restart_limit,
            Err(error) => {
                self.report(&*error, SETTINGS, None);
                DEFAULT_RESTART_LIMIT
            }
        }
    }

    /// `settings.contextRetentionMs`，宿主抛错时视为 0（TS: `#contextRetentionMs`）。
    fn context_retention_ms(&self) -> f64 {
        match (self.deps.settings)() {
            Ok(settings) => settings.context_retention_ms,
            Err(error) => {
                self.report(&*error, SETTINGS, None);
                0.0
            }
        }
    }

    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        self.deps.registry.snapshot()
    }

    /// 读取快照并把 panic 当成 TS 的 throw（只有 TS 里 try/catch 的位置用）。
    fn try_snapshot(&self) -> Result<Arc<dyn RegistrySnapshot>, BoxError> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.deps.registry.snapshot()
        }))
        .map_err(panic_to_error)
    }

    // -----------------------------------------------------------------------
    // open / resume / retry
    // -----------------------------------------------------------------------

    /// 载入活任务，把幸存 `running` 任务改回 `pending` 并计一次重启；不派发任何东西
    /// （TS: `open(context)`）。
    ///
    /// 三个监听先于扫描注册，因此 open 期间到达的提交也会进入 `observe()`。恢复写回与扫描
    /// 在同一个 `commit_with` 里完成，其余实例看不到“一半 running 一半 pending”的中间态。
    ///
    /// `read_only` 时仍然载入索引与 `resumed` 集合，但不提交、不标记 `abandoned`、不排队
    /// reconcile/wake；返回前不会触发任何任务代码。
    ///
    /// # 错误
    /// 注册监听、扫描、`set_task` 或提交本身失败都会原样返回；此前的监听已经生效，
    /// 调用方通常会让整个 Harness 打开失败。
    pub async fn open(&self, context: &dyn Context) -> Result<(), BoxError> {
        let this = self.clone();
        // 与 TS 一样不保留这两个取消订阅句柄：监听器随 Session 一起结束。
        let _unsubscribe_commits =
            self.deps
                .session
                .observe_commits(Arc::new(move |publication, _context| {
                    this.observe(publication);
                }))?;
        let this = self.clone();
        let _unsubscribe_close = self.deps.session.subscribe_close(Arc::new(move || {
            this.seal();
        }))?;
        let this = self.clone();
        let unsubscribe_registry = self.deps.registry.subscribe(Arc::new(move || {
            this.notify_blocked();
            this.kick();
        }));
        self.state().unsubscribe_registry = Some(unsubscribe_registry);

        let limit = self.restart_limit();
        let read_only = self.deps.read_only;
        let parked_slot = Arc::new(Mutex::new(Vec::<AnyTaskRecord>::new()));
        let this = self.clone();
        let parked_in = Arc::clone(&parked_slot);
        self.deps
            .session
            .commit_with(
                move |tx| {
                    let this = this.clone();
                    let parked_in = Arc::clone(&parked_in);
                    Box::pin(async move {
                        // 所有表都在第一次写之前读完：先收齐五种状态，再统一 track 并写回。
                        // 事务一旦发生表写，后续表读会被 `ReadAfterWrite` 拒绝，所以必须在
                        // 任何 `set_task` 之前收齐五种状态的扫描结果。
                        let mut scans: Vec<Vec<AnyTaskRecord>> = Vec::new();
                        for status in LIVE_STATUSES {
                            let query = TaskQuery {
                                status: Some(status),
                                ..TaskQuery::default()
                            };
                            let records = scan_all(|cursor| {
                                let query = query.clone();
                                async move {
                                    tx.scan_tasks(&query, SCAN_PAGE_SIZE, cursor.as_ref()).await
                                }
                            })
                            .await?;
                            scans.push(records);
                        }
                        for records in &scans {
                            for record in records {
                                // `track` 建立所有权索引；`resumed` 只标记 open 载入的记录，
                                // phase span 的 `resumed` 属性在第一次消费后消失。
                                this.track(None, record);
                                this.state().resumed.insert(record.id);
                                if read_only {
                                    continue;
                                }
                                if record.abandon_on_restart == Some(true) {
                                    this.state().abandoned.insert(record.id);
                                }
                                // `running` 说明上次进程在任务中途退出：改回 `pending` 并计一次
                                // restart（checkpoint 保留，阶段从原处继续）。达到上限的记录
                                // 只收集到 `parked_slot`，等提交成功后再报告。
                                if record.state.status() == TaskStatus::Running {
                                    let restarts = record.restarts.unwrap_or(0) + 1;
                                    let checkpoint = checkpoint_of(&record.state);
                                    let mut next =
                                        with_state(record, TaskState::Pending { checkpoint });
                                    next.restarts = Some(restarts);
                                    tx.set_task(next)?;
                                    if (restarts as f64) >= limit && !record.abort_requested {
                                        lock(&parked_in).push(record.clone());
                                    }
                                }
                                if let TaskState::Waiting {
                                    policy: JoinPolicy::FailFast,
                                    ..
                                } = &record.state
                                {
                                    this.state().fail_fast_checks.insert(record.id);
                                }
                            }
                        }
                        Ok(())
                    })
                },
                context,
                CommitOptions::default(),
            )
            .await?;
        let parked = parked_slot
            .lock()
            .map(|mut parked| std::mem::take(&mut *parked))
            .unwrap_or_default();
        for record in parked {
            let restarts = record.restarts.unwrap_or(0) + 1;
            let error = io_error(format!(
                "Task {} ({}) was cut off {} times without progress; it is parked until retryTask()",
                record.id, record.kind, restarts
            ));
            let (task, extension) = self.about_record(&record);
            self.report(&*error, ReportInfo::TaskParked { task, extension }, None);
        }
        if read_only {
            return Ok(());
        }
        // 派生崩溃遗留在被取消 owner 之下的 abort 标记，并落定持有的结果。
        self.state().cascade_pending = true;
        self.schedule_reconcile();
        self.schedule_wake();
        Ok(())
    }

    /// 启用调度，并把到期睡眠的任务变为可运行（TS: `resume()`）。
    ///
    /// 幂等：重复调用只重复 `wake_due()`/`kick()`。`read_only` 时返回
    /// [`crate::types::ReadOnlySession`] 错误（`what` 为 `"running tasks"`）。
    pub fn resume(&self) -> Result<(), BoxError> {
        if self.deps.read_only {
            return Err(Box::new(crate::types::ReadOnlySession::new(
                "running tasks",
            )));
        }
        self.flags.enabled.store(true, Ordering::SeqCst);
        self.wake_due();
        self.kick();
        Ok(())
    }

    /// 让被 restart 上限停泊的任务再跑一次（TS: `retry(id, context)`）。
    ///
    /// 只清除 `restarts`（`progressed`），不直接启动：提交成功后调用 `resume()`，由下一次
    /// `reserve()` 重新选中它。任务不存在时报错；已是 `terminal`/`completing`、或并未停泊时
    /// 是空操作（不报错）。
    pub async fn retry(&self, id: TaskId, context: &dyn Context) -> Result<(), BoxError> {
        let this = self.clone();
        self.deps
            .session
            .commit_with(
                move |tx| {
                    let this = this.clone();
                    Box::pin(async move {
                        let current = tx.task(id).await?;
                        let Some(current) = current else {
                            return Err(io_error(format!("Task {id} does not exist")));
                        };
                        let status = current.state.status();
                        if status == TaskStatus::Terminal
                            || status == TaskStatus::Completing
                            || !this.is_parked(&current)
                        {
                            return Ok(());
                        }
                        tx.set_task(progressed(&current))?;
                        Ok(())
                    })
                },
                context,
                CommitOptions::default(),
            )
            .await?;
        self.resume()
    }

    // -----------------------------------------------------------------------
    // blockedReason / subscribeBlocked
    // -----------------------------------------------------------------------

    /// 一个活任务在这里跑不了的原因（TS: `blockedReason(record)`）。
    ///
    /// 这是纯查询：只读索引与 registry 快照，不改变调度状态，也不预留任务。返回 `None`
    /// 表示“当前没有阻塞原因”，在 `running`/`completing`/`terminal` 以及只有 abort 请求
    /// （标记本身会让它走 abort handler 或 orphan）时提前返回。
    pub fn blocked_reason(&self, record: &BlockedSubject) -> Option<BlockedReason> {
        let status = record.status;
        // 前三种状态不参与“能否在这里跑”的判断：running 已在跑，completing/terminal 不会
        // 再被预留。
        if status == TaskStatus::Running
            || status == TaskStatus::Completing
            || status == TaskStatus::Terminal
        {
            return None;
        }
        // 只有 abort 请求、没有 abort 原因：即将启动 abort handler（或直接被 orphan），
        // 不算被定义阻塞。
        if record.abort_requested && record.abort_reason.is_none() {
            return None;
        }
        if self.is_parked_subject(record) {
            return Some(BlockedReason::RestartLimit);
        }
        let snapshot = match self.try_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let first = {
                    let mut state = self.state();
                    let first = !state.snapshot_reported;
                    state.snapshot_reported = true;
                    first
                };
                if first {
                    self.report(&*error, REGISTRY, None);
                }
                // 快照读不到时无法判断，返回“无阻塞原因”而不是猜测；错误只报告第一次，
                // 避免每个任务每次查询都刷一条。
                return None;
            }
        };
        let failed = {
            let state = self.state();
            state
                .failed_migrations
                .get(&record.id)
                .map(|failed| (failed.task.clone(), clone_error(&failed.error)))
        };
        match fit(
            record.id,
            record.version,
            snapshot.task(&record.kind).as_ref(),
            failed,
        ) {
            Fit::Ready { migrates, task } => {
                if migrates && task.definition.migrate.is_none() {
                    Some(BlockedReason::MigrationFailed)
                } else {
                    None
                }
            }
            Fit::Blocked { reason, .. } => Some(reason),
        }
    }

    /// 注册一个 blocked 原因可能变化的监听器（TS: `subscribeBlocked`）。
    ///
    /// 重复注册同一个 `Arc` 是空操作（TS 用 `Set` 按引用相等判定）。返回的退订函数按内部
    /// 注册 id 移除，因此重复调用、或监听器被重新注册后调用旧函数，都不会误删新注册。
    pub fn subscribe_blocked(
        &self,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> crate::types::Unsubscribe {
        let id = {
            let mut state = self.state();
            // TS 用 `Set`：同一监听器重复注册是空操作（引用相等按 `Arc::ptr_eq` 判定）。
            if let Some(existing) = state
                .blocked_listeners
                .iter()
                .find(|candidate| Arc::ptr_eq(&candidate.listener, &listener))
            {
                existing.id
            } else {
                state.blocked_listener_index += 1;
                let id = state.blocked_listener_index;
                state
                    .blocked_listeners
                    .push(BlockedListener { id, listener });
                id
            }
        };
        let state = Arc::clone(&self.state);
        Box::new(move || {
            // 按注册 id 移除（TS: `Set.delete(listener)`）。
            let mut state = lock(&state);
            if let Some(position) = state
                .blocked_listeners
                .iter()
                .position(|candidate| candidate.id == id)
            {
                state.blocked_listeners.remove(position);
            }
        })
    }

    // -----------------------------------------------------------------------
    // wake / join
    // -----------------------------------------------------------------------

    /// Session 何时再需要一个活进程（TS: `wake()`）。
    ///
    /// 纯查询：不改变索引。`active = true` 表示现在就有调用/ reconcile 工作、或至少一个活
    /// 任务没有被判为不可运行；`at` 是最早一个未到期且未被 abort 的睡眠的 epoch 毫秒。
    /// `completing`、等活任务的 `waiting`、停泊任务、以及没有定义可接手的任务都不算 active；
    /// `read_only` 永远返回 `{ active: false, at: None }`。
    pub fn wake(&self) -> Wake {
        if self.deps.read_only {
            return Wake {
                active: false,
                at: None,
            };
        }
        let now = self.now();
        let mut active = {
            let state = self.state();
            !state.invocations.is_empty() || state.reconciling_work > 0
        } || {
            let state = self.state();
            self.reconcile_work(&state)
        };
        let mut at: Option<i64> = None;
        let mut snapshot: Option<Arc<dyn RegistrySnapshot>> = None;
        let mut snapshot_failed = false;
        let records: Vec<AnyTaskRecord> = {
            let state = self.state();
            state.live.values().cloned().collect()
        };
        for record in records {
            let state = &record.state;
            if state.status() == TaskStatus::Completing {
                continue;
            }
            if let TaskState::Waiting { on, .. } = state
                && !record.abort_requested
                && {
                    let state = self.state();
                    on.iter().any(|id| state.live.contains_key(id))
                }
            {
                continue;
            }
            if let TaskState::Sleeping { until, .. } = state
                && !record.abort_requested
                && *until > now
            {
                at = Some(match at {
                    None => *until,
                    Some(at) => at.min(*until),
                });
                continue;
            }
            if active {
                continue;
            }
            if self.is_parked(&record) {
                continue;
            }
            // 一个 abort 调用在等它下面的活工作，那些工作自己会算。
            if record.abort_requested && !self.waiting_on(&record, true).is_empty() {
                continue;
            }
            if !record.abort_requested || record.abort_reason.is_some() {
                if snapshot.is_none() && !snapshot_failed {
                    match self.try_snapshot() {
                        Ok(found) => snapshot = Some(found),
                        Err(error) => {
                            snapshot_failed = true;
                            self.report(&*error, REGISTRY, None);
                        }
                    }
                }
                if let Some(snapshot) = &snapshot
                    && let Fit::Blocked { .. } =
                        self.fit_record(&record, snapshot.task(&record.kind))
                {
                    continue;
                }
            }
            active = true;
        }
        Wake { active, at }
    }

    /// 通知每个调用并等待它们，然后把它们跑的任务写回 `pending`（TS: `join()`）。
    ///
    /// 顺序：先对所有 invocation 发 abort 信号，再逐个等 `finished`；然后调用
    /// `write_at_close` 并传入一个收集函数——宿主调用它取得“仍为 `running` 的记录改写成
    /// `pending`（checkpoint 保留）”的 StorageWrite 批次，使下一次 open 不把正常 close
    /// 计成 crash。`write_at_close` 的错误被忽略（TS 对应 `.catch(() => {})`），关闭流程
    /// 不应因写失败而卡住。Harness 在 Session 关闭监听（`seal()`）之后、Storage 关闭之前
    /// 调用 `join()`（见 `HarnessHooks::before_close`）。
    pub async fn join(&self) {
        let stopped: Vec<Arc<Invocation>> = {
            let state = self.state();
            state.invocations.values().cloned().collect()
        };
        for invocation in &stopped {
            invocation.controller.abort(None);
        }
        for invocation in &stopped {
            invocation.wait_finished().await;
        }
        if stopped.is_empty() {
            return;
        }
        let state = Arc::clone(&self.state);
        let write = Arc::clone(&self.deps.write_at_close);
        let callback: Arc<dyn Fn() -> Vec<StorageWrite> + Send + Sync> = Arc::new(move || {
            let state = lock(&state);
            let mut writes = Vec::new();
            for invocation in &stopped {
                let Some(record) = state.live.get(&invocation.task_id) else {
                    continue;
                };
                if record.state.status() != TaskStatus::Running {
                    continue;
                }
                let checkpoint = checkpoint_of(&record.state);
                let value = with_state(record, TaskState::Pending { checkpoint });
                writes.push(StorageWrite::Task { value });
            }
            writes
        });
        let _ = write(callback).await;
    }

    // -----------------------------------------------------------------------
    // abort / waitForTask / waitForIdle / abortConversation
    // -----------------------------------------------------------------------

    /// 提交 abort 标记；没有可接管定义的孤儿任务直接落定（TS: `abort(id, context, keepRestart)`）。
    ///
    /// `keep_restart` 为真时，已经带 `restart` 标记的任务直接返回 `Marked`，不再覆盖原因
    /// （`runtime.abort_owned` 以 `keep_restart = true` 调用它）。提交只负责标记或 orphan；
    /// 真正的 abort handler 由后续 `reserve()` 启动。
    ///
    /// 如果该任务有一个 run 调用，本方法会等它结束；等待过程与 Session 失败竞速，
    /// Session 先失败时返回 `SessionFailed`。
    ///
    /// # 错误
    /// 任务不存在、提交失败、Session 已失败或调用上下文被取消时返回对应错误。
    pub async fn abort(
        &self,
        id: TaskId,
        context: &dyn Context,
        keep_restart: bool,
    ) -> Result<TaskAbortResult, BoxError> {
        let this = self.clone();
        let marked = self
            .deps
            .session
            .commit_with(
                move |tx| {
                    let this = this.clone();
                    Box::pin(async move {
                        let current = tx.task(id).await?;
                        let Some(current) = current else {
                            return Err(io_error(format!("Task {id} does not exist")));
                        };
                        if current.state.status() == TaskStatus::Terminal {
                            return Ok(Marked {
                                result: TaskAbortResult::Terminal,
                                run: None,
                            });
                        }
                        let invocation = { this.state().invocations.get(&id).cloned() };
                        // 被 restart 标记的任务没有 run 调用；它只等自己的 abort。
                        if keep_restart && current.abort_reason == Some(TaskAbortReason::Restart) {
                            return Ok(Marked {
                                result: TaskAbortResult::Marked,
                                run: None,
                            });
                        }
                        if invocation.is_none() && current.state.status() != TaskStatus::Completing
                        {
                            this.load_scopes(false).await?;
                            if !this.has_owned_live(id, None, None) {
                                let snapshot = this.try_snapshot()?;
                                if let Resolution::Blocked { reason } =
                                    this.resolve(&current, &snapshot)
                                {
                                    this.terminate(
                                        tx,
                                        &current,
                                        SchedulerOutcome::Orphaned {
                                            reason: reason_str(reason).to_owned(),
                                        },
                                    )
                                    .await?;
                                    return Ok(Marked {
                                        result: TaskAbortResult::Marked,
                                        run: None,
                                    });
                                }
                            }
                        }
                        if !current.abort_requested || current.abort_reason.is_some() {
                            tx.set_task(with_abort_mark(&current, None))?;
                        }
                        let run =
                            invocation.filter(|invocation| invocation.mode == InvocationMode::Run);
                        Ok(Marked {
                            result: TaskAbortResult::Marked,
                            run,
                        })
                    })
                },
                context,
                CommitOptions::default(),
            )
            .await?;
        if let Some(run) = marked.run {
            let session = self.deps.session.clone();
            let race = futures::future::select(
                Box::pin(run.wait_finished()),
                Box::pin(async move { session.failed().await }),
            );
            // TS: `Promise.race([run.done, this.#session.failed])`；`failed` 以 `SessionFailed`
            // 拒绝，所以一个忽略信号、活过失败 Session 的调用让 abort() 失败。`select` 与
            // `Promise.race` 一样在两侧都已就绪时选左边（run 先结束即成功）。
            match await_with_context(race, context).await {
                Ok(futures::future::Either::Left(_)) => {}
                Ok(futures::future::Either::Right(_)) => {
                    let failure = self.deps.session.failure();
                    return Err(match failure {
                        Some(failure) => Box::new(SessionFailed::new(failure.error())),
                        None => io_error("Session failed"),
                    });
                }
                Err(abort) => return Err(Box::new(abort)),
            }
        }
        Ok(marked.result)
    }

    /// 等一个任务落定（TS: `waitForTask(id, context)`）。
    ///
    /// 任务若在 `#live` 中，就在 Session 线上注册 waiter，由 `observe()` 在终态提交时结算；
    /// 否则直接读一次 Storage。Storage 记录用 `from_record_unchecked` 包装：TS 在这里同样
    /// 把读到的记录直接当作 `SettledTask` 返回，读到非终态不会报错。
    ///
    /// # 错误
    /// 按路径区分：
    /// - 读取时已关闭：`closed_error` 选出具体错误——Session 未失败时是 `HarnessClosed`，
    ///   已失败时是 `SessionFailed`。
    /// - 已登记的 waiter 随后被 `seal()` 拒绝：文本与上面相同，但由 `Waiters::reject_all`
    ///   包了一层共享错误（`SharedError`），不能 `downcast` 回 `HarnessClosed`。
    /// - 等待端在结算前消失（发送端被丢弃）：`io::Error`，文本为 `Harness is closed`。
    ///
    /// 任务不存在时报错。
    pub async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError> {
        let this = self.clone();
        let storage = Arc::clone(&self.deps.storage);
        let found = self
            .deps
            .session
            .read_on_line(async move {
                if this.closing() {
                    return Err(closed_error(&this.deps.session));
                }
                if this.state().live.contains_key(&id) {
                    return this
                        .state()
                        .task_waiters
                        .add(id, context)
                        .map(Found::Waiting);
                }
                let record = storage.task(id, context).await?;
                let Some(record) = record else {
                    return Err(io_error(format!("Task {id} does not exist")));
                };
                Ok(Found::Settled(Box::new(
                    SettledTask::from_record_unchecked(record),
                )))
            })
            .await?;
        match found {
            Found::Settled(record) => Ok(*record),
            Found::Waiting(receiver) => match receiver.await {
                Ok(result) => result,
                Err(_) => Err(io_error("Harness is closed")),
            },
        }
    }

    /// 等普通遍历从一个会话（或所有 ownerless 会话）达不到任何活非 background 任务
    /// （TS: `waitForIdle`）。
    ///
    /// 先做一次同步检查，未 idle 时登记 waiter 并 `schedule_reconcile()`；之后由
    /// `reconcile()` 末尾、含任务记录变化的提交（`observe()`）、以及 context 到期计时器
    /// 调用 `settle_idle()` 重新判定并结算。`background` 任务不计入 idle（它们不阻止空闲）。
    ///
    /// # 错误
    /// 关闭时立即返回 `HarnessClosed`（Session 已失败则 `SessionFailed`）；已经挂起的
    /// waiter 在 `seal()` 时被以关闭错误拒绝。
    pub async fn wait_for_idle(
        &self,
        conversation_id: Option<ConversationId>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        if self.closing() {
            return Err(closed_error(&self.deps.session));
        }
        if self.idle(conversation_id) {
            return Ok(());
        }
        self.schedule_reconcile();
        let receiver = {
            let state = self.state();
            state.idle_waiters.add(conversation_id, context)?
        };
        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(io_error("Harness is closed")),
        }
    }

    /// `Conversation.abort()`：一个提交里撤回排队输入并标记遍历可达的每个活非 background
    /// 任务；等作用域空闲（TS: `abortConversation`）。
    ///
    /// `background` 决定遍历是否穿过 background 任务，也决定是否逐个 `wait_for_task`：
    /// 为假时只用 `wait_for_idle` 等前台工作结束，background 任务保持运行。排队输入的撤回
    /// 只针对普通遍历能到达同一会话的提交。
    ///
    /// # 错误
    /// 提交失败或等待失败时返回；任务不存在等错误会从等待步骤冒泡。
    pub async fn abort_conversation(
        &self,
        conversation_id: ConversationId,
        background: bool,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let this = self.clone();
        let reached = self
            .deps
            .session
            .commit_with(
                move |tx| {
                    let this = this.clone();
                    Box::pin(async move {
                        let queued = this.load_scopes(true).await?;
                        let scope = Scope::Conversation(conversation_id);
                        let mut reached: Vec<TaskId> = Vec::new();
                        let walk = {
                            let state = this.state();
                            walk_down(
                                &state,
                                conversation_node(conversation_id),
                                None,
                                &WalkOptions {
                                    cross_background: background,
                                    enter: None,
                                },
                            )
                        };
                        for item in walk {
                            let Some(live) = item.live else {
                                continue;
                            };
                            if item.node.background && !background {
                                continue;
                            }
                            reached.push(live.id);
                            if !live.abort_requested || live.abort_reason.is_some() {
                                tx.set_task(with_abort_mark(&live, None))?;
                            }
                        }
                        for id in queued {
                            if this.in_scope(Up::Conversation(id), scope, background) == Some(true)
                            {
                                (this.deps.withdraw_inputs)(tx, id).await?;
                            }
                        }
                        Ok(reached)
                    })
                },
                context,
                CommitOptions::default(),
            )
            .await?;
        if background {
            for id in reached {
                self.wait_for_task(id, context).await?;
            }
        }
        self.wait_for_idle(Some(conversation_id), context).await
    }
}

/// `waitForTask` 线上读取的结果。
enum Found {
    Waiting(tokio::sync::oneshot::Receiver<WaitResult<SettledTask>>),
    Settled(Box<SettledTask>),
}

/// `abort()` 的提交结果。
struct Marked {
    result: TaskAbortResult,
    run: Option<Arc<Invocation>>,
}

/// `#resolve()` 的结果（TS: `Resolution`）。
#[allow(clippy::large_enum_variant)]
enum Resolution {
    Ready {
        task: AnyTask,
        record: AnyTaskRecord,
        changed: bool,
    },
    Blocked {
        reason: BlockedReason,
    },
}

/// `#fit()` 的结果（TS: `Fit`）。
enum Fit {
    Ready {
        task: AnyTask,
        migrates: bool,
    },
    Blocked {
        reason: BlockedReason,
        error: Option<BoxError>,
    },
}

/// `#fit(record, task)`：一个定义能否接管记录，或为什么不能。
fn fit(
    id: TaskId,
    version: i64,
    task: Option<&AnyTask>,
    failed: Option<(AnyTask, BoxError)>,
) -> Fit {
    let Some(task) = task else {
        return Fit::Blocked {
            reason: BlockedReason::MissingTask,
            error: None,
        };
    };
    let definition = &task.definition;
    if definition.version == version {
        return Fit::Ready {
            task: task.clone(),
            migrates: false,
        };
    }
    if definition.version < version {
        return Fit::Blocked {
            reason: BlockedReason::TaskTooOld,
            error: None,
        };
    }
    if let Some((failed_task, error)) = failed
        && same_task(&failed_task, task)
    {
        return Fit::Blocked {
            reason: BlockedReason::MigrationFailed,
            error: Some(error),
        };
    }
    let _ = id;
    Fit::Ready {
        task: task.clone(),
        migrates: true,
    }
}

fn panic_to_error(payload: Box<dyn Any + Send>) -> BoxError {
    let message = match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "host callback threw".to_owned(),
        },
    };
    io_error(message)
}

fn checkpoint_of(state: &TaskState) -> Checkpoint {
    state.checkpoint().cloned().unwrap_or(JsonValue::Null)
}

/// 在一个已知的 `settings.restartLimit` 下判定停泊（TS: `#parked`）。宿主 settings 回调
/// 必须在锁外读取，所以把上限作为参数传入。
fn is_parked_at(record: &AnyTaskRecord, limit: f64) -> bool {
    !record.abort_requested
        && record
            .restarts
            .is_some_and(|restarts| restarts as f64 >= limit)
}

/// `checkpoint.phase` 的 JS 字符串语义：缺失渲染为 `"undefined"`，非字符串按 `String(v)` 强制
/// （数字 `1` → `"1"`，与 TS 里 `phases[checkpoint.phase]` 的属性键强制、模板字符串一致）。
/// TS 的类型写着 `phase: string`，运行期却接受任意 JSON 值（任务提交的 checkpoint 无校验）。
fn checkpoint_phase(checkpoint: &Checkpoint) -> String {
    kiana_json::js_string::js_string(checkpoint.get("phase"))
}

fn checkpoint_attempt(checkpoint: &Checkpoint) -> Option<f64> {
    checkpoint.get("attempt").and_then(JsonValue::as_f64)
}

fn reason_str(reason: BlockedReason) -> &'static str {
    match reason {
        BlockedReason::MissingTask => "missing_task",
        BlockedReason::TaskTooOld => "task_too_old",
        BlockedReason::MigrationFailed => "migration_failed",
        BlockedReason::RestartLimit => "restart_limit",
    }
}

fn with_abort_mark(record: &AnyTaskRecord, reason: Option<TaskAbortReason>) -> AnyTaskRecord {
    let mut next = record.clone();
    next.abort_requested = true;
    next.abort_reason = reason;
    next.restarts = None;
    next
}

/// 一个活 owner 的持久取消意图：abort 标记，或非 `completed` 的持有结果（TS:
/// `cancellationIntent`）。
fn cancellation_intent(record: &AnyTaskRecord) -> bool {
    record.state.status() != TaskStatus::Terminal
        && (record.abort_requested || failed_outcome(record))
}

/// 记录持有或落定于非 `completed` 的结果（TS: `failedOutcome`）。
fn failed_outcome(record: &AnyTaskRecord) -> bool {
    match record.state.outcome() {
        Some(outcome) => outcome_status(outcome) != TaskOutcomeStatus::Completed,
        None => false,
    }
}

/// 去掉 restart 计数（TS: `progressed`）。
fn progressed(record: &AnyTaskRecord) -> AnyTaskRecord {
    let mut next = record.clone();
    next.restarts = None;
    next
}

/// 替换活记录的状态；结果一旦决定，memos 与 restart 计数消失（TS: `withState`）。
fn with_state(record: &AnyTaskRecord, state: TaskState) -> AnyTaskRecord {
    let mut next = record.clone();
    if matches!(
        state,
        TaskState::Terminal { .. } | TaskState::Completing { .. }
    ) {
        next.memos = None;
        next.restarts = None;
    }
    next.state = state;
    next
}

/// 定义能否在预留时接管记录：同版本，或带迁移的更新版本（TS: `canReserve`）。
fn can_reserve(task: &AnyTask, record: &AnyTaskRecord) -> bool {
    let definition = &task.definition;
    definition.version == record.version
        || (definition.version > record.version && definition.migrate.is_some())
}

/// 睡眠的 `until` 必须是 Harness 时钟上的时间点；abort handler 不能睡（TS: `validateSleep`）。
fn validate_sleep(invocation: &Invocation, task: TaskId, until: i64) -> Result<(), BoxError> {
    if invocation.mode == InvocationMode::Abort {
        return Err(io_error(format!(
            "Abort handler of task {task} cannot sleep"
        )));
    }
    if until < 0 {
        return Err(io_error(format!(
            "Task {task} cannot sleep until {until}: not a time in milliseconds"
        )));
    }
    Ok(())
}

/// 存下来的 memos 里的自有条目（TS: `memoOf`）。
fn memo_of(record: Option<&AnyTaskRecord>, name: &str) -> Option<JsonValue> {
    record
        .and_then(|record| record.memos.as_ref())
        .and_then(|memos| memos.get(name).cloned())
}

fn add_to(map: &mut IndexMap<Node, IndexSet<Node>>, key: Node, value: Node) {
    match map.get_mut(&key) {
        Some(values) => {
            values.insert(value);
        }
        None => {
            let mut values = IndexSet::new();
            values.insert(value);
            map.insert(key, values);
        }
    }
}

fn delete_from(map: &mut IndexMap<Node, IndexSet<Node>>, key: &Node, value: &Node) {
    let Some(values) = map.get_mut(key) else {
        return;
    };
    values.shift_remove(value);
    if values.is_empty() {
        map.shift_remove(key);
    }
}

fn add_waiter(map: &mut IndexMap<TaskId, IndexSet<TaskId>>, key: TaskId, value: TaskId) {
    match map.get_mut(&key) {
        Some(values) => {
            values.insert(value);
        }
        None => {
            let mut values = IndexSet::new();
            values.insert(value);
            map.insert(key, values);
        }
    }
}

fn delete_waiter(map: &mut IndexMap<TaskId, IndexSet<TaskId>>, key: TaskId, value: TaskId) {
    let Some(values) = map.get_mut(&key) else {
        return;
    };
    values.shift_remove(&value);
    if values.is_empty() {
        map.shift_remove(&key);
    }
}

/// 两个 JSON 值的结构相等（忽略对象键序，TS: `jsonEqual`）。
fn json_equal(left: &JsonValue, right: &JsonValue) -> bool {
    deep_equal(left, right)
}

/// 活记录的状态序列化成 JSON（TS: `current.state as unknown as JsonValue`）。
fn state_json(state: &TaskState) -> JsonValue {
    serde_json::to_value(state).unwrap_or(JsonValue::Null)
}

// ---------------------------------------------------------------------------
// 提交观察
// ---------------------------------------------------------------------------

impl TaskScheduler {
    /// `#observe(publication)`：把一次已存储提交的变化并入索引。
    ///
    /// 这是调度器唯一的“提交 → 内存索引”入口，在 Session 线上同步执行。处理顺序本身是
    /// 不变量：先会话边（同一提交里新建会话必须先可查，后面的新任务才能挂到 owner 下），
    /// 再任务记录，再由失败任务反推 `failFast` 等待者，再排队输入，最后处理首次出现的任务。
    /// `publication.kind() == "memory"` 的提交只改文档，直接跳过。
    fn observe(&self, publication: &ObservedCommit) {
        // 只在内存里采纳的提交只改文档。
        if publication.kind() == "memory" {
            return;
        }
        let changes = publication.changes();
        // 先处理 edges，好让提交在新会话里创建的任务挂上去。
        for change in &changes {
            if let CommitChange::Table(TableCommitChange::Conversation { value }) = change {
                let known = self.state().edges.contains_key(&value.id);
                if !known {
                    let owner = value.owner.map(|owner| owner.task_id);
                    self.set_edge(value.id, owner);
                }
            }
        }
        let mut updated: Vec<AnyTaskRecord> = Vec::new();
        let mut failed: Vec<TaskId> = Vec::new();
        let mut changed = false;
        for change in &changes {
            let CommitChange::Table(TableCommitChange::Task { value: record }) = change else {
                continue;
            };
            changed = true;
            let id = record.id;
            let previous = { self.state().live.get(&id).cloned() };
            if failed_outcome(record)
                && previous
                    .as_ref()
                    .is_none_or(|previous| !failed_outcome(previous))
            {
                failed.push(id);
            }
            self.track(previous.as_ref(), record);
            if record.state.status() == TaskStatus::Terminal {
                {
                    let mut state = self.state();
                    state.failed_migrations.shift_remove(&id);
                    state.fail_fast_checks.shift_remove(&id);
                }
                {
                    let state = self.state();
                    state
                        .task_waiters
                        .resolve(&id, SettledTask::new(record.clone()));
                }
                // 它的 owner 现在可能可以落定。
                self.schedule_reconcile();
                continue;
            }
            if record.abort_requested
                && previous
                    .as_ref()
                    .is_none_or(|previous| !previous.abort_requested)
            {
                self.state().cascade_pending = true;
                // 给新标记任务的 run 调用发信号；它的下一个 step 结束它。
                let invocation = { self.state().invocations.get(&id).cloned() };
                if let Some(invocation) = invocation
                    && invocation.mode == InvocationMode::Run
                {
                    invocation.controller.abort(None);
                }
            }
            // 请求替换 restart 标记时，升级下面工作的标记。
            if previous
                .as_ref()
                .is_some_and(|previous| previous.abort_reason.is_some())
                && record.abort_reason.is_none()
            {
                self.state().cascade_pending = true;
            }
            let status = record.state.status();
            if status == TaskStatus::Completing
                && previous
                    .as_ref()
                    .is_none_or(|previous| previous.state.status() != TaskStatus::Completing)
            {
                if cancellation_intent(record) {
                    self.state().cascade_pending = true;
                }
                self.schedule_reconcile();
            }
            if status == TaskStatus::Waiting
                && matches!(
                    &record.state,
                    TaskState::Waiting {
                        policy: JoinPolicy::FailFast,
                        ..
                    }
                )
                && previous
                    .as_ref()
                    .is_none_or(|previous| previous.state.status() != TaskStatus::Waiting)
            {
                self.state().fail_fast_checks.insert(id);
                self.schedule_reconcile();
            }
            // 它的 owner 与位置永不变：它变为活工作时，上面的链只查一次。
            if previous.is_none() {
                updated.push(record.clone());
            }
        }
        for id in failed {
            let waiters = {
                let state = self.state();
                state.waiters.get(&id).cloned().unwrap_or_default()
            };
            // 只有本身以 `failFast` 等待的 waiter 进入检查；同一失败任务上的 `allSettled`
            // waiter 保留其余依赖（TS: 逐个 waiter 判定 `state.policy === "failFast"`）。
            let checks: Vec<TaskId> = {
                let state = self.state();
                waiters
                    .into_iter()
                    .filter(|waiter| {
                        matches!(
                            state.live.get(waiter).map(|record| &record.state),
                            Some(TaskState::Waiting {
                                policy: JoinPolicy::FailFast,
                                ..
                            })
                        )
                    })
                    .collect()
            };
            if !checks.is_empty() {
                let mut state = self.state();
                for waiter in checks {
                    state.fail_fast_checks.insert(waiter);
                }
                drop(state);
                self.schedule_reconcile();
            }
        }
        for change in &changes {
            // 被取消 owner 下面的排队输入会被撤回，即使在其级联之后。
            let CommitChange::Table(TableCommitChange::Submission { value: submission }) = change
            else {
                continue;
            };
            if submission.status != SubmissionStatus::Queued
                || submission.kind != SubmissionKind::Input
            {
                continue;
            }
            // 只有带取消意图的 owner 才撤回它；之后出现的意图会在那时跑级联。
            if self.state().intent.is_empty() {
                continue;
            }
            let conversation = submission.conversation_id;
            let known = {
                let state = self.state();
                self.known(&state, conversation_node(conversation))
            };
            if !known || self.below_cancelled(Up::Conversation(conversation)) {
                self.state().cascade_pending = true;
            }
        }
        for record in updated {
            // 被取消 owner 之下新建的工作（即使在其级联之后）也会被 abort。
            if self.state().unloaded.contains(&record.id) {
                self.schedule_reconcile();
            } else if !record.background
                && !record.abort_requested
                && self.below_cancelled(parent_of(&node_of(&record)))
            {
                self.state().cascade_pending = true;
            }
        }
        // 预留或 step 发现待处理级联时，下一次任意提交都会跑它。
        if self.state().cascade_pending {
            self.schedule_reconcile();
        }
        // sweep 每次提交都跑（会话边变化也可能让节点失效）；只有确实出现了任务记录变化
        // 才结算 idle 并 kick 预留，避免纯会话/文档提交反复触发调度。
        self.sweep();
        if !changed {
            return;
        }
        self.settle_idle();
        self.kick();
    }

    // -----------------------------------------------------------------------
    // idle 结算与上下文过期
    // -----------------------------------------------------------------------

    /// 落定 idle 等待者，并丢弃空闲超过保留期的 kept context（TS: `#settleIdle`）。
    ///
    /// 两件事：先对每个 `idle_waiters` 键重新判定并结算；再维护 `contexts` 的
    /// `idle_since`——会话不再 idle 时清空，idle 且 `contextRetentionMs > 0` 时从当前时刻起算，
    /// 保留期为 0 时立即丢弃。`context` 只是读缓存，丢弃它只会让下一次读取从头扫描，
    /// 因此这里不报告、不回退。
    fn settle_idle(&self) {
        let keys = { self.state().idle_waiters.keys() };
        for conversation_id in keys {
            if self.idle(conversation_id) {
                let state = self.state();
                state.idle_waiters.resolve(&conversation_id, ());
            }
        }
        let now = self.now();
        let retention = self.context_retention_ms();
        let mut state = self.state();
        let ids: Vec<ConversationId> = state.contexts.keys().copied().collect();
        for conversation_id in ids {
            let idle = self.idle_state(&state, Some(conversation_id));
            let Some(kept) = state.contexts.get(&conversation_id) else {
                continue;
            };
            let idle_since = kept.idle_since;
            if let Some(idle_since) = idle_since
                && (now - idle_since) as f64 >= retention
            {
                state.contexts.shift_remove(&conversation_id);
                continue;
            }
            if !idle {
                if let Some(kept) = state.contexts.get_mut(&conversation_id) {
                    kept.idle_since = None;
                }
            } else if retention > 0.0 {
                if let Some(kept) = state.contexts.get_mut(&conversation_id)
                    && kept.idle_since.is_none()
                {
                    kept.idle_since = Some(now);
                }
            } else {
                state.contexts.shift_remove(&conversation_id);
            }
        }
        drop(state);
        self.schedule_expiry();
    }

    /// 最早的空闲 context 到期时跑 `#settleIdle()`（后台计时器，TS: `#scheduleExpiry`）。
    fn schedule_expiry(&self) {
        let retention = self.context_retention_ms();
        // TS 的 `idleSince + retention` 是 JS number 相加：`contextRetentionMs` 可以是小数，
        // 结果不截断（`#expiry.at` 逐值比较，计时器延迟也可以是小数）。
        let at: Option<f64> = {
            let state = self.state();
            let mut at: Option<f64> = None;
            for kept in state.contexts.values() {
                if let Some(idle_since) = kept.idle_since {
                    let candidate = idle_since as f64 + retention;
                    at = Some(match at {
                        None => candidate,
                        Some(at) => at.min(candidate),
                    });
                }
            }
            at
        };
        let existing = {
            let state = self.state();
            state.expiry.as_ref().map(|expiry| expiry.at)
        };
        if existing.is_some() && existing == at {
            return;
        }
        // 宿主的 disarm 回调在锁外运行（TS 无锁，回调可重入调度器）。
        let previous = {
            let mut state = self.state();
            state.expiry.take()
        };
        if let Some(previous) = previous {
            previous.disarm.disarm();
        }
        let Some(at) = at else {
            return;
        };
        if self.closing() {
            return;
        }
        let this = self.clone();
        // 后台计时器只用于释放内存：宿主可以拒绝（`Timers::background` 返回 `None`），
        // 此时不记录 `#expiry`，保留的 context 要等下一次 `settle_idle()` 才被重新检查。
        let disarm = self.deps.timers.background(
            Arc::new(move || {
                {
                    let mut state = this.state();
                    state.expiry = None;
                }
                this.settle_idle();
            }),
            at - self.now() as f64,
        );
        if let Some(disarm) = disarm {
            self.state().expiry = Some(TimerHandle {
                at,
                disarm: SharedDisarm::new(disarm),
            });
        }
    }

    // -----------------------------------------------------------------------
    // 所有权
    // -----------------------------------------------------------------------

    /// 排队一次 reconcile（TS: `#scheduleReconcile`）。
    ///
    /// 只去重“已排队但尚未开始”的调用：`reconcile()` 一进入就把闩清零，因此在它执行期间
    /// 产生的新请求会再 spawn 一次（多次 reconcile 的提交仍在 Session 线上串行）。
    /// `read_only` 与 closing 时是空操作。
    fn schedule_reconcile(&self) {
        if self.closing() || self.deps.read_only {
            return;
        }
        if self.flags.reconcile_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            this.reconcile().await;
        });
    }

    /// 一次提交，应用已提交记录蕴含的东西：级联 abort 标记、failFast 标记、撤回排队输入，
    /// 以及每个 `completing` 任务的最终终态记录（TS: `#reconcile`）。
    ///
    /// 阶段：先清闩并取走 `cascade_pending`/`fail_fast_checks` 快照；在一个提交里依次
    /// 载入未加载的链、沿所有权树派生 abort 标记、处理 `failFast` 失败、撤回排队输入、
    /// `finalize()` 落定 `completing`；提交失败按存储失败处理（`fail_session`，不重试）。
    async fn reconcile(&self) {
        self.flags
            .reconcile_scheduled
            .store(false, Ordering::SeqCst);
        let work = {
            let state = self.state();
            self.reconcile_work(&state)
        };
        if work {
            // 计数非零期间 `wake()` 报告 active：reconcile 也被视为未完成的工作。
            self.state().reconciling_work += 1;
        }
        let cascade = {
            let mut state = self.state();
            let cascade = state.cascade_pending;
            state.cascade_pending = false;
            cascade
        };
        let checks: Vec<TaskId> = {
            let mut state = self.state();
            let checks: Vec<TaskId> = state.fail_fast_checks.iter().copied().collect();
            state.fail_fast_checks.clear();
            checks
        };
        let this = self.clone();
        let context = Arc::clone(&self.deps.context);
        let result = self
            .deps
            .session
            .commit_with(
                move |tx| {
                    let this = this.clone();
                    let checks = checks.clone();
                    Box::pin(async move {
                        // 排队后到真正提交前可能已经开始关闭：这时不写任何东西。
                        if this.closing() {
                            return Ok(());
                        }
                        // cascade 为真时还会扫描 queued input，返回其会话 id。
                        let queued = this.load_scopes(cascade).await?;
                        // 级联来自被放弃的 owner 时传递它的 `restart` 原因；其它意图标记或升级为请求。
                        // 先收集，让同一个 pass 里请求赢过 `restart` 标记。
                        let mut marks: IndexMap<TaskId, (AnyTaskRecord, Option<TaskAbortReason>)> =
                            IndexMap::new();
                        let intents: Vec<TaskId> = {
                            let state = this.state();
                            state.intent.iter().copied().collect()
                        };
                        // 每个有取消意图的 owner：向下遍历普通 owned 工作（不进 background，
                        // 也不进入另一个有意图的子树，避免同一轮重复标记）。
                        for id in intents {
                            let owner = { this.state().live.get(&id).cloned() };
                            let Some(owner) = owner else {
                                continue;
                            };
                            // 只有 restart 后被放弃、且没有其它结果的 owner 才传递原因。
                            let restart = owner.abort_reason == Some(TaskAbortReason::Restart)
                                && !failed_outcome(&owner);
                            let intent_set: IndexSet<TaskId> = {
                                let state = this.state();
                                state.intent.clone()
                            };
                            let walk = {
                                let state = this.state();
                                walk_down(
                                    &state,
                                    Node::Task(id),
                                    None,
                                    &WalkOptions {
                                        cross_background: false,
                                        enter: Some(&|below| !intent_set.contains(&below)),
                                    },
                                )
                            };
                            for item in walk {
                                if let Some(live) = item.live
                                    && !item.node.background
                                {
                                    mark_task(
                                        &mut marks,
                                        &live,
                                        if restart {
                                            Some(TaskAbortReason::Restart)
                                        } else {
                                            None
                                        },
                                    );
                                }
                            }
                        }
                        // failFast：`on` 里只要有一个失败，就把其余尚未失败的成员标记掉；
                        // 失败者保留自己的结果。
                        for id in checks {
                            let waiter = { this.state().live.get(&id).cloned() };
                            let Some(waiter) = waiter else {
                                continue;
                            };
                            let TaskState::Waiting { on, .. } = &waiter.state else {
                                continue;
                            };
                            let on = on.clone();
                            if !this.any_failed(&on).await? {
                                continue;
                            }
                            // 其它所有活任务；失败的那个保留自己的结果。
                            for member in on {
                                let record = { this.state().live.get(&member).cloned() };
                                if let Some(record) = record
                                    && !failed_outcome(&record)
                                {
                                    mark_task(&mut marks, &record, None);
                                }
                            }
                        }
                        for (_, (record, reason)) in marks {
                            tx.set_task(with_abort_mark(&record, reason))?;
                        }
                        for id in queued {
                            if this.below_cancelled(Up::Conversation(id)) {
                                (this.deps.withdraw_inputs)(tx, id).await?;
                            }
                        }
                        this.finalize(tx).await?;
                        Ok(())
                    })
                },
                context,
                CommitOptions::default(),
            )
            .await;
        if let Err(error) = result
            && !self.closing()
        {
            // 这个提交里不跑扩展代码：抛错是存储失败、宿主回调或 bug。重跑也修不好，让 Session 失败。
            self.fail_session(error, None);
        }
        if work {
            let mut state = self.state();
            state.reconciling_work = state.reconciling_work.saturating_sub(1);
        }
        self.schedule_wake();
        self.settle_idle();
    }

    /// 一次 reconcile 是否有工作可做（TS: `#reconcileWork`）。
    fn reconcile_work(&self, state: &State) -> bool {
        state.cascade_pending
            || !state.finalize_checks.is_empty()
            || !state.fail_fast_checks.is_empty()
    }

    /// `ids` 里是否有任何一个持有或落定于非 `completed` 的结果（TS: `#anyFailed`）。
    async fn any_failed(&self, ids: &[TaskId]) -> Result<bool, BoxError> {
        for id in ids {
            let record = {
                let state = self.state();
                state.live.get(id).cloned()
            };
            let record = match record {
                Some(record) => Some(record),
                None => self.deps.storage.task(*id, &*self.deps.context).await?,
            };
            if let Some(record) = record
                && failed_outcome(&record)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 写入每个没有活普通 owned 工作的 `completing` 任务的终态记录（TS: `#finalize`）。
    ///
    /// 分轮处理：一轮里刚落定的持有者可能让它的 owner 在下一轮也可落定，因此用 `next`
    /// 集合迭代到没有新候选；`done` 防止同一轮重复处理，`overlay` 让本提交里刚写入的
    /// 终态对后续判断可见。只有调度器负责的 `faulted`/`orphaned` 才调用 `settle_outcome`。
    async fn finalize(&self, tx: &Transaction) -> Result<(), BoxError> {
        let mut round: Vec<TaskId> = {
            let mut state = self.state();
            let round: Vec<TaskId> = state.finalize_checks.iter().copied().collect();
            state.finalize_checks.clear();
            round
        };
        let mut done: IndexSet<TaskId> = IndexSet::new();
        let mut overlay = overlay_of(tx);
        while !round.is_empty() {
            let mut next: IndexSet<TaskId> = IndexSet::new();
            for id in round {
                let record = overlay
                    .tasks
                    .get(&id)
                    .cloned()
                    .or_else(|| self.state().live.get(&id).cloned());
                let Some(record) = record else {
                    continue;
                };
                if record.state.status() != TaskStatus::Completing || done.contains(&id) {
                    continue;
                }
                if self.has_owned_live(id, Some(&overlay), Some(&done)) {
                    continue;
                }
                let Some(outcome) = record.state.outcome().cloned() else {
                    continue;
                };
                let terminal = with_state(
                    &record,
                    TaskState::Terminal {
                        outcome: outcome.clone(),
                    },
                );
                tx.set_task(terminal.clone())?;
                overlay.tasks.insert(id, terminal);
                done.insert(id);
                // 只有调度器写 `faulted` 与 `orphaned`；它们的清理等这次提交。
                if matches!(
                    outcome,
                    TaskOutcome::Faulted { .. } | TaskOutcome::Orphaned { .. }
                ) {
                    let scheduler_outcome = scheduler_outcome_of(&outcome);
                    if let Some(scheduler_outcome) = scheduler_outcome {
                        (self.deps.settle_outcome)(tx, &record, &scheduler_outcome).await?;
                    }
                }
                if let Some(above) = self.nearest_live_above(&node_of(&record), Some(&overlay))
                    && above.state.status() == TaskStatus::Completing
                {
                    next.insert(above.id);
                }
            }
            round = next.into_iter().collect();
        }
        Ok(())
    }

    /// 载入 `#unloaded` 里任务的 owner 链；`queued` 时也载入有排队输入的每个会话
    /// （TS: `#loadScopes`）。
    ///
    /// 可能在 `await` 期间读到 Storage；返回 `queued` 扫描到的去重会话 id（调用方据此
    /// 判断哪些会话的排队输入可能要被撤回）。没有排队输入时返回空 `Vec`。
    async fn load_scopes(&self, queued: bool) -> Result<Vec<ConversationId>, BoxError> {
        let unloaded: Vec<TaskId> = {
            let state = self.state();
            state.unloaded.iter().copied().collect()
        };
        for id in unloaded {
            // 前一次 `load_chain` 可能已经把后面某个 id 的链补齐了，逐个复查再加载。
            let still = { self.state().unloaded.contains(&id) };
            if still {
                self.load_chain(Up::Task(id), None).await?;
            }
        }
        if !queued {
            return Ok(Vec::new());
        }
        let storage = Arc::clone(&self.deps.storage);
        let context = Arc::clone(&self.deps.context);
        let query = SubmissionQuery {
            status: Some(SubmissionStatus::Queued),
            ..SubmissionQuery::default()
        };
        let submissions = scan_all(|cursor| {
            let storage = Arc::clone(&storage);
            let context = Arc::clone(&context);
            let query = query.clone();
            async move {
                storage
                    .scan_submissions(&query, SCAN_PAGE_SIZE, cursor.as_ref(), &*context)
                    .await
            }
        })
        .await?;
        let mut conversations: IndexSet<ConversationId> = IndexSet::new();
        for submission in submissions {
            if submission.kind == SubmissionKind::Input {
                conversations.insert(submission.conversation_id);
            }
        }
        for id in conversations.clone() {
            self.load_chain(Up::Conversation(id), None).await?;
        }
        Ok(conversations.into_iter().collect())
    }

    /// 从 `start` 向上载入 owner 边与任务节点，直到 ownerless 根（TS: `#loadChain`）。
    ///
    /// 三种停止条件：到达 ownerless 根；遇到已载入且不在 `#unloaded` 的节点；走到本提交里
    /// 新建、自己的记录会覆盖上方链的任务（`overlay`）。`Storage` 里查不到记录时直接返回，
    /// 链断在那里。只有完整走完（或被上述停止条件截断）才把经过的任务移出 `#unloaded`。
    async fn load_chain(&self, start: Up, overlay: Option<&Overlay>) -> Result<(), BoxError> {
        let mut passed: Vec<TaskId> = Vec::new();
        let mut at: Option<Up> = Some(start);
        while let Some(current) = at {
            match current {
                Up::Task(id) => {
                    let mut node: Option<TaskNode> = {
                        let state = self.state();
                        state
                            .live
                            .get(&id)
                            .map(node_of)
                            .or_else(|| state.settled.get(&id).copied())
                    };
                    // 本次提交里创建的：从它自己的记录载入覆盖了上面的链。
                    if node.is_none()
                        && overlay.is_some_and(|overlay| overlay.tasks.contains_key(&id))
                    {
                        break;
                    }
                    if node.is_some() && !{ self.state().unloaded.contains(&id) } {
                        break;
                    }
                    if node.is_none() {
                        let record = self.deps.storage.task(id, &*self.deps.context).await?;
                        let Some(record) = record else {
                            return Ok(());
                        };
                        let fields = node_of(&record);
                        node = Some(fields);
                        if record.state.status() == TaskStatus::Terminal {
                            self.settle(id, fields);
                        }
                        // 载入的节点可能揭示一个正在取消的 owner：下一次 reconcile 重新派生标记。
                        self.schedule_reconcile();
                    }
                    passed.push(id);
                    at = node.map(|node| parent_of(&node));
                }
                Up::Conversation(conversation_id) => {
                    let edge = self.edge(conversation_id, overlay);
                    let owner = match edge {
                        Some(owner) => owner,
                        None => {
                            let record = self
                                .deps
                                .storage
                                .conversation(conversation_id, &*self.deps.context)
                                .await?;
                            let owner = record.and_then(|record| record.owner.map(|o| o.task_id));
                            self.set_edge(conversation_id, owner);
                            self.schedule_reconcile();
                            owner
                        }
                    };
                    at = owner.map(Up::Task);
                }
            }
        }
        {
            let mut state = self.state();
            for id in passed {
                state.unloaded.shift_remove(&id);
            }
        }
        Ok(())
    }

    /// 记录/更新一个会话的 owner 边（TS: `#setEdge`）。
    fn set_edge(&self, conversation_id: ConversationId, owner: Option<TaskId>) {
        let mut state = self.state();
        state.edges.insert(conversation_id, owner);
        // 会话现在挂在它的 owner 下，或是根；下面没有东西时等 sweep。
        self.relist(&mut state, conversation_node(conversation_id));
        self.drop_if_unneeded(&mut state, conversation_node(conversation_id));
    }

    /// 知道一个链载入读到的终态任务，让遍历穿过它（TS: `#settle`）。
    fn settle(&self, id: TaskId, node: TaskNode) {
        let mut state = self.state();
        state.settled.insert(id, node);
        state.unloaded.insert(id);
        self.relist(&mut state, Node::Task(id));
        self.drop_if_unneeded(&mut state, Node::Task(id));
    }

    /// 没有东西列在它下面时，把已结束任务或会话排进 sweep（TS: `#dropIfUnneeded`）。
    fn drop_if_unneeded(&self, state: &mut State, node: Node) {
        if let Node::Task(id) = node
            && state.live.contains_key(&id)
        {
            return;
        }
        if state.below.contains_key(&node) {
            return;
        }
        state.drop_queue.insert(node);
        self.schedule_sweep();
    }

    /// 在变更线上作为独立 job 排队一次 sweep（TS: `#scheduleSweep`）。
    ///
    /// 单飞；`sweep()` 在 `read_on_line` 里执行，与提交监听串行。执行或读取失败被忽略
    /// （只可能在关闭/失败时发生）。
    fn schedule_sweep(&self) {
        if self.closing() {
            return;
        }
        if self.flags.sweep_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _ = this
                .deps
                .session
                .read_on_line(async {
                    this.flags.sweep_scheduled.store(false, Ordering::SeqCst);
                    this.sweep();
                    Ok(())
                })
                .await;
        });
    }

    /// 丢弃仍不需要的排队节点（TS: `#sweep`）。
    ///
    /// `drop_queue` 里的节点若在 `#below` 中重新出现（又有活工作挂上来）就跳过；否则会话
    /// 丢弃 `edges`/`roots`，任务丢弃 `settled`/`unloaded`。这是索引有界的实现点：
    /// 已结束任务与空闲会话不会被永久保留。
    fn sweep(&self) {
        let queued: Vec<Node> = {
            let state = self.state();
            state.drop_queue.iter().copied().collect()
        };
        {
            let mut state = self.state();
            for node in queued {
                if state.below.contains_key(&node) {
                    continue;
                }
                match node {
                    Node::Conversation(conversation_id) => {
                        state.edges.shift_remove(&conversation_id);
                        state.roots.shift_remove(&conversation_id);
                    }
                    Node::Task(id) => {
                        if !state.live.contains_key(&id) {
                            state.settled.shift_remove(&id);
                            state.unloaded.shift_remove(&id);
                        }
                    }
                }
            }
            state.drop_queue.clear();
        }
    }

    /// 把调度器的集合对齐到任务的已提交记录（TS: `#track`）。
    ///
    /// 这是索引维护的中心：同步 invocation 的 `record` 副本；维护活动指标
    /// `kiana.durable.tasks.active`；删掉旧状态留下的等待边、按新状态重建；终态记录离开
    /// `live` 进入 `settled`，并触发 owner 的落定检查。调用必须发生在 Session 线上
    /// （`observe`/`open` 都满足），否则 `previous` 与 `live` 可能不一致。
    fn track(&self, previous: Option<&AnyTaskRecord>, record: &AnyTaskRecord) {
        let id = record.id;
        {
            let state = self.state();
            if let Some(invocation) = state.invocations.get(&id) {
                invocation.set_record(record.clone());
            }
        }
        // 指标只在非 closing 时增减：`seal()` 会为残留的 live 记录统一减一，closing 之后
        // 这里不再改动指标。
        if record.state.status() == TaskStatus::Terminal {
            if previous.is_some() && !self.closing() {
                self.deps
                    .active_metric
                    .add(-1.0, Some(active_attributes(record)));
            }
            self.state().resumed.shift_remove(&id);
        } else if previous.is_none() && !self.closing() {
            self.deps
                .active_metric
                .add(1.0, Some(active_attributes(record)));
        }
        if let Some(previous) = previous
            && let TaskState::Waiting { on, .. } = &previous.state
        {
            let mut state = self.state();
            for on_id in on {
                delete_waiter(&mut state.waiters, *on_id, id);
            }
        }
        if record.state.status() == TaskStatus::Terminal {
            let node = node_of(record);
            {
                let mut state = self.state();
                state.live.shift_remove(&id);
                state.sleeping.shift_remove(&id);
                state.intent.shift_remove(&id);
                state.runnable.shift_remove(&id);
                state.finalize_checks.shift_remove(&id);
                state.settled.insert(id, node);
            }
            // 在忘掉任何东西之前：走到它持有的任务的路径会穿过中间的已结束任务。
            self.release(node, None);
            let waiters = {
                let state = self.state();
                state.waiters.get(&id).cloned().unwrap_or_default()
            };
            {
                let mut state = self.state();
                for waiter in waiters {
                    state.runnable.insert(waiter);
                }
            }
            self.relist_id(Node::Task(id));
            self.drop_if_unneeded_locked(Node::Task(id));
            return;
        }
        {
            let mut state = self.state();
            state.live.insert(id, record.clone());
        }
        if previous.is_none() {
            // 首次出现的任务：父链未知时只标 `unloaded`，真正的 Storage 读取留给下一次
            // `load_scopes()`（track 在线上，不在这里做 IO）。
            let parent_known = {
                let state = self.state();
                self.known(&state, parent_node(&node_of(record)))
            };
            if !parent_known {
                self.state().unloaded.insert(id);
            }
            self.relist_id(Node::Task(id));
        }
        {
            let mut state = self.state();
            if cancellation_intent(record) {
                state.intent.insert(id);
            } else {
                state.intent.shift_remove(&id);
            }
            if record.state.status() == TaskStatus::Completing {
                state.runnable.shift_remove(&id);
                state.finalize_checks.insert(id);
            } else {
                state.runnable.insert(id);
            }
            if let TaskState::Waiting { on, .. } = &record.state {
                for on_id in on {
                    add_waiter(&mut state.waiters, *on_id, id);
                }
            }
            if record.state.status() == TaskStatus::Sleeping {
                state.sleeping.insert(id);
            } else {
                state.sleeping.shift_remove(&id);
            }
        }
    }

    fn relist_id(&self, node: Node) {
        let mut state = self.state();
        self.relist(&mut state, node);
    }

    fn drop_if_unneeded_locked(&self, node: Node) {
        let mut state = self.state();
        self.drop_if_unneeded(&mut state, node);
    }

    /// 已结束普通工作上方最近的活任务现在可能空闲了：持有结果的、或正在 abort 的
    /// （TS: `#release`）。
    fn release(&self, node: TaskNode, overlay: Option<&Overlay>) -> Option<AnyTaskRecord> {
        let above = self.nearest_live_above(&node, overlay);
        if let Some(above) = &above {
            if above.state.status() == TaskStatus::Completing {
                self.state().finalize_checks.insert(above.id);
            } else if above.abort_requested {
                self.state().runnable.insert(above.id);
            }
        }
        above
    }

    /// 把 `start` 列到 `#below` 里它的父节点下（它是活任务或下面有节点），否则取消列出；
    /// 然后向上直到不再变化（TS: `#relist`）。
    ///
    /// “需要列出”的条件只有两条：节点是仍在 `live` 的任务，或节点自己已经列出了子节点。
    /// 向上传播在这几种情况下停止：父节点对该子节点的列出状态已经一致（无变化）；加入时
    /// 父节点原本已有子节点（`below` 已有该父键，说明父节点本来就被列出，更上层无变化）；
    /// 或删除后父节点仍非空（更上层看不到变化）。会话没有 owner 时改为维护 `roots`。
    fn relist(&self, state: &mut State, start: Node) {
        let mut node = start;
        loop {
            let needed = match node {
                Node::Task(id) => state.live.contains_key(&id),
                Node::Conversation(_) => false,
            } || state.below.contains_key(&node);
            let parent = match node {
                Node::Conversation(conversation_id) => match state.edges.get(&conversation_id) {
                    None => return,
                    Some(None) => {
                        if needed {
                            state.roots.insert(conversation_id);
                        } else {
                            state.roots.shift_remove(&conversation_id);
                        }
                        return;
                    }
                    Some(Some(owner)) => Node::Task(*owner),
                },
                Node::Task(id) => {
                    let known = state
                        .live
                        .get(&id)
                        .map(node_of)
                        .or_else(|| state.settled.get(&id).copied());
                    let Some(known) = known else {
                        return;
                    };
                    parent_node(&known)
                }
            };
            let listed = state
                .below
                .get(&parent)
                .is_some_and(|children| children.contains(&node));
            if listed == needed {
                return;
            }
            if needed {
                let first = !state.below.contains_key(&parent);
                add_to(&mut state.below, parent, node);
                if !first {
                    return;
                }
            } else {
                delete_from(&mut state.below, &parent, &node);
                if state.below.contains_key(&parent) {
                    return;
                }
                self.drop_if_unneeded(state, parent);
            }
            node = parent;
        }
    }

    /// 已结束普通工作 `node` 上方最近的活任务（TS: `#nearestLiveAbove`）。
    ///
    /// 沿 `owner`/会话边向上；`overlay` 里本提交刚写入的记录优先于 `live`。走到 `background`
    /// 节点就停止（background 工作不参与普通 owned 链的持有）。中间经过的已结束任务用
    /// `settled` 里的归属字段继续向上。
    fn nearest_live_above(
        &self,
        node: &TaskNode,
        overlay: Option<&Overlay>,
    ) -> Option<AnyTaskRecord> {
        let state = self.state();
        let mut current = *node;
        while !current.background {
            let owner = match current.owner {
                Some(owner) => Some(owner),
                None => self.edge_state(&state, current.conversation_id, overlay)?,
            };
            let owner = owner?;
            let record = overlay
                .and_then(|overlay| overlay.tasks.get(&owner).cloned())
                .or_else(|| state.live.get(&owner).cloned());
            if let Some(record) = &record
                && record.state.status() != TaskStatus::Terminal
            {
                return Some(record.clone());
            }
            let above: Option<TaskNode> = match record {
                Some(record) => Some(node_of(&record)),
                None => state.settled.get(&owner).copied(),
            };
            current = above?;
        }
        None
    }

    /// `node` 之上的链是否已载入（TS: `#known`）。
    fn known(&self, state: &State, node: Node) -> bool {
        match node {
            Node::Task(id) => {
                (state.live.contains_key(&id) || state.settled.contains_key(&id))
                    && !state.unloaded.contains(&id)
            }
            Node::Conversation(conversation_id) => match state.edges.get(&conversation_id) {
                None => false,
                Some(None) => true,
                Some(Some(owner)) => self.known(state, Node::Task(*owner)),
            },
        }
    }

    /// 一个会话的 owner 任务（TS: `#edge`）。
    fn edge(
        &self,
        conversation_id: ConversationId,
        overlay: Option<&Overlay>,
    ) -> Option<Option<TaskId>> {
        if let Some(overlay) = overlay
            && let Some(edge) = overlay.edges.get(&conversation_id)
        {
            return Some(*edge);
        }
        let state = self.state();
        state.edges.get(&conversation_id).copied()
    }

    fn edge_state(
        &self,
        state: &State,
        conversation_id: ConversationId,
        overlay: Option<&Overlay>,
    ) -> Option<Option<TaskId>> {
        if let Some(overlay) = overlay
            && let Some(edge) = overlay.edges.get(&conversation_id)
        {
            return Some(*edge);
        }
        state.edges.get(&conversation_id).copied()
    }

    /// `#node(id, overlay)`。
    fn node_lookup(
        &self,
        state: &State,
        id: TaskId,
        overlay: Option<&Overlay>,
    ) -> Option<TaskNode> {
        overlay
            .and_then(|overlay| overlay.tasks.get(&id))
            .map(node_of)
            .or_else(|| state.live.get(&id).map(node_of))
            .or_else(|| state.settled.get(&id).copied())
    }

    /// 从 `start` 向上：owner 任务与会话，终于 ownerless 根或未载入的边（TS: `#above`）。
    fn above(&self, state: &State, start: Up, overlay: Option<&Overlay>) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut at: Option<Up> = Some(start);
        while let Some(current) = at {
            match current {
                Up::Task(task) => {
                    let Some(node) = self.node_lookup(state, task, overlay) else {
                        steps.push(Step::Unknown);
                        return steps;
                    };
                    steps.push(Step::Task { task, node });
                    at = Some(parent_of(&node));
                }
                Up::Conversation(conversation) => {
                    steps.push(Step::Conversation(conversation));
                    let edge = self.edge_state(state, conversation, overlay);
                    let Some(owner) = edge else {
                        steps.push(Step::Unknown);
                        return steps;
                    };
                    at = owner.map(Up::Task);
                }
            }
        }
        steps
    }

    /// `owner` 的活普通 owned 工作（TS: `#ownedLive`）。
    ///
    /// 向下遍历 `owner` 的子树，只收集 `live` 中非终态、非 `background` 的任务。`first` 为真
    /// 时找到第一个就返回（`has_owned_live` 用）；`done` 里已落定的任务不再进入其子树，
    /// 避免 `finalize()` 的多轮循环重复访问。
    fn owned_live(
        &self,
        owner: TaskId,
        overlay: Option<&Overlay>,
        first: bool,
        done: Option<&IndexSet<TaskId>>,
    ) -> Vec<TaskId> {
        let state = self.state();
        let enter = done.map(|done| |id: TaskId| !done.contains(&id));
        let walk = walk_down(
            &state,
            Node::Task(owner),
            overlay,
            &WalkOptions {
                cross_background: false,
                enter: enter.as_ref().map(|f| f as &dyn Fn(TaskId) -> bool),
            },
        );
        let mut found = Vec::new();
        for item in walk {
            if item.live.is_none() || item.node.background {
                continue;
            }
            found.push(item.id);
            if first {
                break;
            }
        }
        found
    }

    fn has_owned_live(
        &self,
        owner: TaskId,
        overlay: Option<&Overlay>,
        done: Option<&IndexSet<TaskId>>,
    ) -> bool {
        !self.owned_live(owner, overlay, true, done).is_empty()
    }

    /// 从 `scope` 的普通遍历能否到达 `start`（TS: `#inScope`）。
    ///
    /// `Some(true)`：向上遇到目标会话（`Scope::Conversation`），或 `Scope::Roots` 且一路
    /// 到达链顶；`Some(false)`：路径上有 `background` 任务且未要求穿过，或遍历结束仍未命中；
    /// `None`：中途遇到 `Unknown`（owner 链未载入），无法判断——调用方不能把它当成“不在”。
    fn in_scope(&self, start: Up, scope: Scope, cross_background: bool) -> Option<bool> {
        let state = self.state();
        self.in_scope_state(&state, start, scope, cross_background)
    }

    fn in_scope_state(
        &self,
        state: &State,
        start: Up,
        scope: Scope,
        cross_background: bool,
    ) -> Option<bool> {
        for step in self.above(state, start, None) {
            match step {
                Step::Unknown => return None,
                Step::Conversation(conversation) => {
                    if let Scope::Conversation(scoped) = scope
                        && conversation == scoped
                    {
                        return Some(true);
                    }
                }
                Step::Task { node, .. } => {
                    if node.background && !cross_background {
                        return Some(false);
                    }
                }
            }
        }
        Some(matches!(scope, Scope::Roots))
    }

    /// 一个活 owner 的取消意图能否到达 `start`（TS: `#belowCancelled`）。
    fn below_cancelled(&self, start: Up) -> bool {
        self.cancelling_owner(start).is_some()
    }

    /// `start` 之上最近的、取消意图能到达它的活 owner（TS: `#cancellingOwner`）。
    ///
    /// 每步先检查该任务自身的取消意图，命中就返回（`background` 节点自身带意图时也会被
    /// 返回）；只有 `background` 节点自身没有意图时才作为边界返回 `None`；遇到 `Unknown`
    /// 也返回 `None`（链未载入时不能假设上方有取消者）。`intent` 为空时快速返回，这是
    /// 没有取消在进行的常见路径。
    fn cancelling_owner(&self, start: Up) -> Option<AnyTaskRecord> {
        let state = self.state();
        // 没有东西在取消，常见情况：不走。
        if state.intent.is_empty() {
            return None;
        }
        for step in self.above(&state, start, None) {
            match step {
                Step::Unknown => return None,
                Step::Conversation(_) => {}
                Step::Task { task, node } => {
                    let live = state.live.get(&task).cloned();
                    if let Some(live) = &live
                        && cancellation_intent(live)
                    {
                        return live.clone().into();
                    }
                    if node.background {
                        return None;
                    }
                }
            }
        }
        None
    }

    /// 让 Session 失败（TS: `#failSession`）。
    ///
    /// 不重试：调用方已经判定这是存储失败、宿主回调失败或内部 bug。已经 `closing`、或错误
    /// 本身是 `SessionClosed` 时直接忽略——那是正常的关闭竞态，不是失败。
    fn fail_session(&self, error: BoxError, context: Option<&dyn Context>) {
        if self.closing() {
            return;
        }
        // 另一个线程可能已经开始关闭 Session：这时 `commitWith()` 之类会以 `SessionClosed` 拒绝，
        // 而关闭监听的 `seal()` 还没把 `closing` 置位。这不是存储失败，不能把 Session 标记为失败
        // （TS 单线程里 `#notifyClose()` 先于任何其它代码运行，看不到这个窗口）。
        if error.downcast_ref::<SessionClosed>().is_some() {
            return;
        }
        match context {
            Some(context) => self.deps.session.fail_with_context(error, context),
            None => self.deps.session.fail(error),
        }
    }

    /// 关闭监听器：接纳被封住后、`join()` 之前同步运行，或 Session 失败时运行（TS: `#seal`）。
    ///
    /// 依次：为残留的 live 记录减活动指标 → 置 `closing` → 退订 registry（panic 转成报告）
    /// → 用关闭错误拒绝所有 task/idle waiter → 清空 kept context 并撤销两个计时器。
    /// 指标扣减必须在 `closing` 置位前完成（它受 `if !closing` 守卫）；此后所有 `track()`
    /// 都不再改指标。宿主回调一律在锁外调用。
    fn seal(&self) {
        // 宿主回调（metric、registry unsubscribe、report、计时器 disarm）一律在锁外运行：
        // TS 是单线程无锁的，回调里重入任何调度器方法都安全；这里持锁调用会自死锁。
        if !self.closing() {
            let records: Vec<AnyTaskRecord> = {
                let state = self.state();
                state.live.values().cloned().collect()
            };
            for record in &records {
                self.deps
                    .active_metric
                    .add(-1.0, Some(active_attributes(record)));
            }
        }
        self.flags.closing.store(true, Ordering::SeqCst);
        self.state().resumed.clear();
        let unsubscribe = {
            let mut state = self.state();
            state.unsubscribe_registry.take()
        };
        if let Some(unsubscribe) = unsubscribe
            && let Err(payload) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(unsubscribe))
        {
            self.report(&*panic_to_error(payload), REGISTRY, None);
        }
        let error = closed_error(&self.deps.session);
        let shared: Arc<dyn std::error::Error + Send + Sync> = Arc::from(error);
        let (task_waiters, idle_waiters) = {
            let state = self.state();
            (state.task_waiters.clone(), state.idle_waiters.clone())
        };
        task_waiters.reject_all(clone_error(&shared));
        idle_waiters.reject_all(Box::new(SharedError(shared)));
        let (expiry, timer) = {
            let mut state = self.state();
            state.contexts.clear();
            (state.expiry.take(), state.timer.take())
        };
        if let Some(expiry) = expiry {
            expiry.disarm.disarm();
        }
        if let Some(timer) = timer {
            timer.disarm.disarm();
        }
    }

    /// 在微任务里重算 wake（TS: `#scheduleWake`）。
    ///
    /// 单飞闩只去重尚未开始的调用。没有 `on_wake` 时改为装 sleep 计时器（`enabled` 为假
    /// 则不装，即撤销已有的）；有 `on_wake` 时值未变化不重复上报，只有成功送达才记录
    /// `last_wake`，panic 通过 `ON_WAKE` 报告，下一次变化/kick 会再投递。
    fn schedule_wake(&self) {
        if self.closing() || self.deps.read_only {
            return;
        }
        if self.flags.wake_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            this.flags.wake_scheduled.store(false, Ordering::SeqCst);
            if this.closing() {
                return;
            }
            let wake = this.wake();
            let Some(on_wake) = this.deps.on_wake.clone() else {
                let at = if this.enabled() {
                    wake.at.map(|at| at as f64)
                } else {
                    None
                };
                this.arm_timer(at);
                return;
            };
            let last = { this.state().last_wake };
            if last == Some(wake) {
                return;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_wake(&wake)));
            match result {
                Ok(()) => {
                    // 只有成功送达的 wake 才算数：抛错之后，下一次变化或 kick 会再提供它。
                    this.state().last_wake = Some(wake);
                }
                Err(payload) => this.report(&*panic_to_error(payload), ON_WAKE, None),
            }
        });
    }

    /// 保留最早睡眠的计时器（TS: `#armTimer`）。
    ///
    /// `at` 与现有计时器相同则不重装；变化时先撤销旧的，再按 `at - now()` 装新的。
    /// `at` 为空或正在关闭则不装（等于撤销）。这里用前台计时器：即使宿主拒绝，
    /// `Timers::foreground` 也会退回 `setTimeout`，所以总能拿到 disarm。
    fn arm_timer(&self, at: Option<f64>) {
        let existing = {
            let state = self.state();
            state.timer.as_ref().map(|timer| timer.at)
        };
        if existing.is_some() && existing == at {
            return;
        }
        // 宿主的 disarm 回调在锁外运行（TS 无锁，回调可重入调度器）。
        let previous = {
            let mut state = self.state();
            state.timer.take()
        };
        if let Some(previous) = previous {
            previous.disarm.disarm();
        }
        let Some(at) = at else {
            return;
        };
        if self.closing() {
            return;
        }
        let this = self.clone();
        let disarm = self.deps.timers.foreground(
            Arc::new(move || {
                {
                    let mut state = this.state();
                    state.timer = None;
                }
                this.wake_due();
                this.kick();
            }),
            at - self.now() as f64,
        );
        self.state().timer = Some(TimerHandle {
            at,
            disarm: SharedDisarm::new(disarm),
        });
    }

    /// 把每个时间已到的睡眠任务变为预留守选（TS: `#wakeDue`）。
    fn wake_due(&self) {
        let ids: Vec<TaskId> = {
            let state = self.state();
            state.sleeping.iter().copied().collect()
        };
        for id in ids {
            let record = { self.state().live.get(&id).cloned() };
            if let Some(record) = record
                && !self.not_due(&record)
            {
                self.state().runnable.insert(id);
            }
        }
    }

    /// 连续被切断 `restartLimit` 次的任务；abort 标记仍会跑它的 abort handler（TS: `#parked`）。
    fn is_parked(&self, record: &AnyTaskRecord) -> bool {
        is_parked_at(record, self.restart_limit())
    }

    fn is_parked_subject(&self, record: &BlockedSubject) -> bool {
        !record.abort_requested
            && record
                .restarts
                .is_some_and(|restarts| restarts as f64 >= self.restart_limit())
    }

    /// 报告 `task` 自身代码时的信息（TS: `#aboutTask`）。
    fn about_invocation(&self, invocation: &Invocation) -> (ReportTask, Option<String>) {
        (
            report_task_of(
                invocation.task_id,
                &invocation.kind,
                invocation.conversation_id,
            ),
            invocation.extension.clone(),
        )
    }

    fn about_record(&self, record: &AnyTaskRecord) -> (ReportTask, Option<String>) {
        let extension = match self.try_snapshot() {
            Ok(snapshot) => installer_of(&snapshot, snapshot.task(&record.kind).as_ref()),
            // registry 抛错在别处读取时已报告；报告不带名字发出。
            Err(_) => None,
        };
        (
            report_task_of(record.id, &record.kind, record.conversation_id),
            extension,
        )
    }

    fn about_record_with(
        &self,
        record: &AnyTaskRecord,
        extension: Option<String>,
    ) -> (ReportTask, Option<String>) {
        (
            report_task_of(record.id, &record.kind, record.conversation_id),
            extension,
        )
    }

    /// 把升高的 `settings.restartLimit` 不再停泊的任务重新变为候选（TS: `#unparkRaised`）。
    fn unpark_raised(&self) {
        // 宿主 settings 回调在锁外读取：TS 无锁，回调里重入调度器（如 `wake()`）是安全的。
        let limit = self.restart_limit();
        let mut state = self.state();
        let ids: Vec<TaskId> = state.parked_tasks.iter().copied().collect();
        let mut unparked = false;
        for id in ids {
            let record = state.live.get(&id).cloned();
            if let Some(record) = &record
                && is_parked_at(record, limit)
            {
                continue;
            }
            state.parked_tasks.shift_remove(&id);
            let Some(_record) = record else {
                continue;
            };
            state.runnable.insert(id);
            unparked = true;
        }
        drop(state);
        if unparked {
            let this = self.clone();
            tokio::spawn(async move {
                this.notify_blocked();
            });
        }
    }

    fn notify_blocked(&self) {
        let listeners: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let state = self.state();
            state
                .blocked_listeners
                .iter()
                .map(|listener| Arc::clone(&listener.listener))
                .collect()
        };
        for listener in listeners {
            if let Err(payload) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener()))
            {
                self.report(&*panic_to_error(payload), ReportInfo::ListenerError, None);
            }
        }
    }

    /// 一个时钟尚未到达的睡眠；abort 标记会结束睡眠（TS: `#notDue`）。
    fn not_due(&self, record: &AnyTaskRecord) -> bool {
        match &record.state {
            TaskState::Sleeping { until, .. } => !record.abort_requested && *until > self.now(),
            _ => false,
        }
    }

    /// 请求一次调度（TS: `#kick`）：重算 wake、置 `dirty`，需要时 spawn `drain()`。
    ///
    /// `drain` 已在跑、调度未启用（`resume()` 之前）或正在关闭时，只留下 `dirty` 标志，
    /// 由现有循环或下一次 kick 处理。提交与监听回调里绝不能同步提交，因此这里只 spawn。
    fn kick(&self) {
        self.schedule_wake();
        self.flags.dirty.store(true, Ordering::SeqCst);
        if flag(&self.flags.draining) || !self.enabled() || self.closing() {
            return;
        }
        if self.flags.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        // 绝不在提交或 registry 监听里同步提交。
        let this = self.clone();
        tokio::spawn(async move {
            this.drain().await;
        });
    }

    /// 反复预留，直到没有新变化（TS: `#drain`）。
    ///
    /// 每轮先清 `dirty` 再 `reserve()`；`reserve()` 出错按存储失败处理并退出循环。结束后
    /// 清 `draining`，若期间又置了 `dirty` 就再 kick 一次，避免最后一次变化被漏掉。
    async fn drain(&self) {
        loop {
            if !(flag(&self.flags.dirty) && self.enabled() && !self.closing()) {
                break;
            }
            self.flags.dirty.store(false, Ordering::SeqCst);
            match self.reserve().await {
                Ok(reservations) => {
                    for reservation in reservations {
                        self.start(reservation);
                    }
                }
                Err(error) => {
                    // 与 `#reconcile()` 一样：预留提交不跑扩展代码。
                    self.fail_session(error, None);
                    break;
                }
            }
        }
        self.flags.draining.store(false, Ordering::SeqCst);
        // 循环最后一次检查之后到达的 wakeup 仍需要一次 pass。
        if flag(&self.flags.dirty) {
            self.kick();
        }
    }

    /// 一个提交里预留每个合格任务（TS: `#reserve`）。
    ///
    /// 一个 pass 只做一件事：若 `abandoned` 非空，先给这些任务打 `restart` 标记并结束本次
    /// 提交（下一轮才真正预留）；否则加载未载入链、接上到期睡眠、解除因上限升高而停泊的
    /// 任务，再逐个挑选可运行任务。所有 `Invocation` 先记进日志：提交失败时逐个移除、
    /// 放回 `runnable` 并 `finish()`，否则 `join()` 会永远等一个不会启动的调用。
    async fn reserve(&self) -> Result<Vec<Reservation>, BoxError> {
        // TS 的 try/catch 覆盖整个 `commitWith`：回调返回后提交本身失败，也要回滚已创建的调用。
        let created: Arc<Mutex<Vec<Arc<Invocation>>>> = Arc::new(Mutex::new(Vec::new()));
        let this = self.clone();
        let created_in = Arc::clone(&created);
        let output = self
            .deps
            .session
            .commit_with(
                move |tx| {
                    let this = this.clone();
                    let created_in = Arc::clone(&created_in);
                    Box::pin(async move {
                        if !this.enabled() || this.closing() {
                            return Ok(ReserveOutput::default());
                        }
                        let abandoned_ids: Vec<TaskId> = {
                            let state = this.state();
                            state.abandoned.iter().copied().collect()
                        };
                        if !abandoned_ids.is_empty() {
                            for id in &abandoned_ids {
                                let record = { this.state().live.get(id).cloned() };
                                if let Some(record) = record
                                    && !record.abort_requested
                                {
                                    tx.set_task(with_abort_mark(
                                        &record,
                                        Some(TaskAbortReason::Restart),
                                    ))?;
                                }
                            }
                            // 已标记，或本来就标记/不在了：完成。
                            return Ok(ReserveOutput {
                                reservations: Vec::new(),
                                abandoned: abandoned_ids,
                                faulted: Vec::new(),
                            });
                        }
                        this.load_scopes(false).await?;
                        // 每个 pass 都接上到期的睡眠：任意变化启动的 pass 都可能是 deadline 之后的第一次。
                        this.wake_due();
                        this.unpark_raised();
                        let (reservations, faulted) = this.reserve_pass(tx).await?;
                        // 记下已创建的调用，供提交失败时回滚。
                        {
                            let mut log = lock(&created_in);
                            for reservation in &reservations {
                                log.push(Arc::clone(&reservation.invocation));
                            }
                        }
                        Ok(ReserveOutput {
                            reservations,
                            abandoned: Vec::new(),
                            faulted,
                        })
                    })
                },
                &self.deps.context,
                CommitOptions::default(),
            )
            .await;
        // 提交失败：本 pass 已经塞进 `invocations` 的调用必须撤回，任务重新变回候选，
        // 并 `finish()` 让 `join()` 的等待结束；随后把错误交给 `drain()` 失败整个 Session。
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let invocations = {
                    let mut log = lock(&created);
                    std::mem::take(&mut *log)
                };
                for invocation in invocations {
                    let id = invocation.task_id;
                    {
                        let mut state = self.state();
                        state.invocations.shift_remove(&id);
                        state.runnable.insert(id);
                    }
                    invocation.finish();
                }
                return Err(error);
            }
        };
        // 故障报告放在提交成功之后：只有已经持久化的 faulted 终态才通知宿主。
        for (record, message) in &output.faulted {
            let (task, extension) = self.about_record(record);
            self.report(
                &*io_error(message.clone()),
                ReportInfo::TaskFaulted {
                    task,
                    extension,
                    reason: TaskFaultedReason::RestartLimit,
                },
                None,
            );
        }
        if !output.abandoned.is_empty() {
            let mut state = self.state();
            for id in &output.abandoned {
                state.abandoned.shift_remove(id);
            }
            drop(state);
            // 重新预留，现在能看到标记。
            self.flags.dirty.store(true, Ordering::SeqCst);
        }
        Ok(output.reservations)
    }

    /// `#reserve` 的主体；出错时回滚已创建的调用。
    async fn reserve_pass(
        &self,
        tx: &Transaction,
    ) -> Result<(Vec<Reservation>, Vec<(AnyTaskRecord, String)>), BoxError> {
        let mut reservations: Vec<Reservation> = Vec::new();
        let mut faulted: Vec<(AnyTaskRecord, String)> = Vec::new();
        let result = self
            .reserve_inner(tx, &mut reservations, &mut faulted)
            .await;
        match result {
            Ok(()) => Ok((reservations, faulted)),
            Err(error) => {
                for reservation in &reservations {
                    let id = reservation.invocation.task_id;
                    {
                        let mut state = self.state();
                        state.invocations.shift_remove(&id);
                        state.runnable.insert(id);
                    }
                    reservation.invocation.finish();
                }
                Err(error)
            }
        }
    }

    /// `reserve_pass` 的主体，按 `runnable` 快照逐个挑选。
    ///
    /// 检查顺序：记录不存在、`completing`、已有调用、仍在等活任务、睡眠未到期、停泊——
    /// 命中任何一条就从候选集移除并跳过。之后判定 run/abort 模式，处理“被取消 owner 之下”
    /// 与“abort handler 超过 restart 上限”，最后 `resolve()` 定义（含迁移），必要时写
    /// `running` 并创建调用。`snapshot` 在本 pass 内惰性读取并被所有预留共享。
    ///
    /// 任一 `set_task` 失败都会冒泡给 `reserve_pass`，由它回滚本 pass 已创建的调用。
    async fn reserve_inner(
        &self,
        tx: &Transaction,
        out: &mut Vec<Reservation>,
        faulted: &mut Vec<(AnyTaskRecord, String)>,
    ) -> Result<(), BoxError> {
        // 惰性读取：只有真的遇到候选任务才调用 registry，避免每个 pass 都为无工作付出一次。
        let mut snapshot: Option<Arc<dyn RegistrySnapshot>> = None;
        let ids: Vec<TaskId> = {
            let state = self.state();
            state.runnable.iter().copied().collect()
        };
        for id in ids {
            let record = { self.state().live.get(&id).cloned() };
            let Some(record) = record else {
                self.state().runnable.shift_remove(&id);
                continue;
            };
            if record.state.status() == TaskStatus::Completing || {
                self.state().invocations.contains_key(&id)
            } {
                self.state().runnable.shift_remove(&id);
                continue;
            }
            // 它等待的东西结束、它的记录变化或下面的工作结束时它会回来。
            if !self.waiting_on(&record, true).is_empty() {
                self.state().runnable.shift_remove(&id);
                continue;
            }
            // 时间到了之后第一个 pass 它会回来，由计时器、`resume()` 或任意变化启动。
            if self.not_due(&record) {
                self.state().runnable.shift_remove(&id);
                continue;
            }
            // `retry()` 清掉 restart 计数、或 `settings.restartLimit` 升高后的某个 pass 它才会回来。
            if self.is_parked(&record) {
                self.state().runnable.shift_remove(&id);
                // 没有提交就被重新停泊（例如上限降低）：图需要听到。
                let newly = { self.state().parked_tasks.insert(id) };
                if newly {
                    let this = self.clone();
                    tokio::spawn(async move {
                        this.notify_blocked();
                    });
                }
                continue;
            }
            let mode = if record.abort_requested {
                InvocationMode::Abort
            } else {
                InvocationMode::Run
            };
            // 带取消意图的 owner 下面的工作等它的级联标记，不跑 phase；排上级联（它可能还没跑）。
            if mode == InvocationMode::Run
                && !record.background
                && self.below_cancelled(parent_of(&node_of(&record)))
            {
                self.state().cascade_pending = true;
                self.schedule_reconcile();
                continue;
            }
            // 连续被切断 `restartLimit` 次的 abort handler 会永远跑下去；没有东西可停泊它。
            if mode == InvocationMode::Abort
                && (record.restarts.unwrap_or(0) as f64) >= self.restart_limit()
            {
                // TS 直接内插 `record.restarts`：没有计数时文本就是 `undefined`。
                let restarts = match record.restarts {
                    Some(restarts) => restarts.to_string(),
                    None => "undefined".to_owned(),
                };
                let message = format!(
                    "Abort handler of task {id} ({}) was cut off {restarts} times",
                    record.kind,
                );
                self.terminate(
                    tx,
                    &record,
                    SchedulerOutcome::Faulted {
                        error: TaskOutcomeError {
                            message: message.clone(),
                            detail: Some(serde_json::json!({ "reason": "restart_limit" })),
                        },
                    },
                )
                .await?;
                faulted.push((record.clone(), message));
                continue;
            }
            if snapshot.is_none() {
                snapshot = Some(self.try_snapshot()?);
            }
            let snapshot_ref = snapshot.as_ref().expect("snapshot was just initialized");
            let resolution = self.resolve(&record, snapshot_ref);
            match resolution {
                Resolution::Blocked { reason } => {
                    // 被放弃的任务等它的定义，好让 abort handler 清理。
                    if mode == InvocationMode::Abort && record.abort_reason.is_none() {
                        self.terminate(
                            tx,
                            &record,
                            SchedulerOutcome::Orphaned {
                                reason: reason_str(reason).to_owned(),
                            },
                        )
                        .await?;
                    }
                    continue;
                }
                Resolution::Ready {
                    task,
                    record: next_record,
                    changed,
                } => {
                    // 只有迁移改变了记录、或它本来不是 running 时才写；同一 checkpoint 的
                    // 重复预留不产生写入，也不清 restart 计数。
                    if changed || record.state.status() != TaskStatus::Running {
                        let checkpoint = checkpoint_of(&next_record.state);
                        tx.set_task(with_state(&next_record, TaskState::Running { checkpoint }))?;
                    }
                    // 在线上注册，好让标记与之后的预留看到它、close 能 join 它；`#end` 让它重新成为候选。
                    // 提交失败时 `reserve_pass`/`reserve` 会把这条调用撤销。
                    self.state().runnable.shift_remove(&id);
                    let extension = installer_of(snapshot_ref, Some(&task));
                    let invocation = self.create_invocation(&record, mode, extension);
                    out.push(Reservation {
                        invocation,
                        task,
                        snapshot: Arc::clone(snapshot_ref),
                    });
                }
            }
        }
        Ok(())
    }

    /// 活任务在下次调用前等待的东西（TS: `#waitingOn`）。
    fn waiting_on(&self, record: &AnyTaskRecord, first: bool) -> Vec<TaskId> {
        if record.abort_requested {
            return self.owned_live(record.id, None, first, None);
        }
        let TaskState::Waiting { on, .. } = &record.state else {
            return Vec::new();
        };
        let state = self.state();
        if !first {
            return on
                .iter()
                .copied()
                .filter(|id| state.live.contains_key(id))
                .collect();
        }
        on.iter()
            .copied()
            .find(|id| state.live.contains_key(id))
            .into_iter()
            .collect()
    }

    /// `#fit(record, task)`：带上已记录的迁移失败。
    fn fit_record(&self, record: &AnyTaskRecord, task: Option<AnyTask>) -> Fit {
        let failed = {
            let state = self.state();
            state
                .failed_migrations
                .get(&record.id)
                .map(|failed| (failed.task.clone(), clone_error(&failed.error)))
        };
        fit(record.id, record.version, task.as_ref(), failed)
    }

    /// 按 kind 解析记录的定义，迁移更旧的已存版本（TS: `#resolve`）。
    ///
    /// 同版本直接返回 `Ready{changed:false}`；更新版本且带 `migrate` 时执行迁移并返回改写后
    /// 的记录（`changed=true`），迁移结果会做深拷贝。迁移失败会写入 `failed_migrations`、
    /// 异步通知 blocked 监听、安排 wake 并报告；之后该任务按 `MigrationFailed` 处理，直到
    /// registry 里的定义换成另一个对象，`fit()` 才会重新尝试迁移。
    fn resolve(&self, record: &AnyTaskRecord, snapshot: &Arc<dyn RegistrySnapshot>) -> Resolution {
        let fit = self.fit_record(record, snapshot.task(&record.kind));
        let Fit::Ready { task, migrates } = fit else {
            let Fit::Blocked { reason, .. } = fit else {
                unreachable!()
            };
            return Resolution::Blocked { reason };
        };
        if !migrates {
            return Resolution::Ready {
                task,
                record: record.clone(),
                changed: false,
            };
        }
        let definition = Arc::clone(&task.definition);
        // 到这里只可能是“定义版本更新”：`fit()` 已排除同版本与更旧版本。若定义没有提供
        // migrate 函数，就无法把旧记录交给新定义，按迁移失败处理。
        let result = (|| -> Result<(JsonValue, JsonValue), BoxError> {
            if definition.migrate.is_none() {
                return Err(missing_migration(record, &definition));
            }
            let (input, checkpoint) = erased_migrate(
                &definition,
                &record.input,
                &checkpoint_of(&record.state),
                record.version,
            )?;
            // 迁移结果交给任务前深拷贝（TS: `copyJson`）。
            Ok((copy_json(&input)?, copy_json(&checkpoint)?))
        })();
        match result {
            Ok((input, checkpoint)) => {
                let mut next = record.clone();
                next.version = definition.version;
                next.input = input;
                next.state = match &record.state {
                    TaskState::Pending { .. } => TaskState::Pending {
                        checkpoint: checkpoint.clone(),
                    },
                    TaskState::Running { .. } => TaskState::Running {
                        checkpoint: checkpoint.clone(),
                    },
                    TaskState::Waiting { on, policy, .. } => TaskState::Waiting {
                        checkpoint: checkpoint.clone(),
                        on: on.clone(),
                        policy: *policy,
                    },
                    TaskState::Sleeping { until, .. } => TaskState::Sleeping {
                        checkpoint: checkpoint.clone(),
                        until: *until,
                    },
                    TaskState::Completing { outcome } => TaskState::Completing {
                        outcome: outcome.clone(),
                    },
                    TaskState::Terminal { outcome } => TaskState::Terminal {
                        outcome: outcome.clone(),
                    },
                };
                Resolution::Ready {
                    task,
                    record: next,
                    changed: true,
                }
            }
            Err(error) => {
                let shared: Arc<dyn std::error::Error + Send + Sync> = Arc::from(error);
                {
                    let mut state = self.state();
                    state.failed_migrations.insert(
                        record.id,
                        FailedMigration {
                            task: task.clone(),
                            error: Arc::clone(&shared),
                        },
                    );
                }
                // 现在就被 block，没有提交：图与宿主 wake 必须听到。
                let this = self.clone();
                tokio::spawn(async move {
                    this.notify_blocked();
                });
                self.schedule_wake();
                let extension = installer_of(snapshot, Some(&task));
                let (task_report, extension) = self.about_record_with(record, extension);
                self.report(
                    &*shared,
                    ReportInfo::MigrationFailed {
                        task: task_report,
                        extension,
                    },
                    None,
                );
                Resolution::Blocked {
                    reason: BlockedReason::MigrationFailed,
                }
            }
        }
    }

    /// 创建一个调用（TS: `#createInvocation`）。
    ///
    /// 创建时就放进 `state.invocations`（在同一事务内），因此本 pass 的后续检查与提交监听
    /// 都能看到它。context 叠加 `controller` 的信号；`links` 只在任务第一次启动
    /// （`started_at` 为空）时继承，重试不会重复挂载旧的 trace links。
    fn create_invocation(
        &self,
        record: &AnyTaskRecord,
        mode: InvocationMode,
        extension: Option<String>,
    ) -> Arc<Invocation> {
        let controller = AbortSignal::new();
        let links = if record.started_at.is_none() {
            record.links.clone()
        } else {
            None
        };
        let context = with_abort_signal(controller.clone(), Arc::clone(&self.deps.context));
        let invocation = Arc::new(Invocation {
            task_id: record.id,
            kind: record.kind.clone(),
            extension,
            conversation_id: record.conversation_id,
            mode,
            controller,
            links: Mutex::new(links),
            context: Mutex::new(context),
            record: Mutex::new(Some(record.clone())),
            fault: Mutex::new(None),
            watches: Mutex::new(Vec::new()),
            timers: Mutex::new(Vec::new()),
            ended: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            finished_notify: Notify::new(),
        });
        self.state()
            .invocations
            .insert(record.id, Arc::clone(&invocation));
        invocation
    }

    /// 启动一个预留的调用（TS: `#start`）。
    ///
    /// 调用体在独立 task 里跑；无论 `run`/`run_abort` 成功还是失败，收尾都固定为：报告
    /// 错误（`task_error`，不改变任务状态，状态由 `step()` 的决策负责）→ `end()` → 清空
    /// record/links/fault → `finish()`（唤醒 `join()`/`abort()`）→ `kick()`。
    fn start(&self, reservation: Reservation) {
        let this = self.clone();
        tokio::spawn(async move {
            let invocation = Arc::clone(&reservation.invocation);
            let result = match invocation.mode {
                InvocationMode::Run => this.run(&reservation).await,
                InvocationMode::Abort => this.run_abort(&reservation).await,
            };
            if let Err(error) = result {
                let (task, extension) = this.about_invocation(&invocation);
                let context = invocation.context();
                this.report(
                    &*error,
                    ReportInfo::TaskError { task, extension },
                    Some(&*context),
                );
            }
            this.end(&invocation);
            *lock(&invocation.record) = None;
            *lock(&invocation.links) = None;
            *lock(&invocation.fault) = None;
            invocation.finish();
            this.kick();
        });
    }

    /// 结束一个调用（TS: `#end`）。
    ///
    /// 幂等：`ended` 的交换保证只有第一次生效。只有当前登记的仍是这条 invocation 时才从
    /// `invocations` 移除、并把仍活着的任务放回 `runnable`。随后异步停止 document watch
    /// （不等待）、撤销 runtime 计时器，最后用 `EndedError` 中止 controller，让绑定该调用的
    /// 等待（工具里的 `waitForTask()` 等）一起失败。
    fn end(&self, invocation: &Arc<Invocation>) {
        if invocation.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        {
            let mut state = self.state();
            let same = state
                .invocations
                .get(&invocation.task_id)
                .is_some_and(|current| Arc::ptr_eq(current, invocation));
            if same {
                state.invocations.shift_remove(&invocation.task_id);
                if state.live.contains_key(&invocation.task_id) {
                    state.runnable.insert(invocation.task_id);
                }
            }
        }
        let watches: Vec<DocumentWatch> = std::mem::take(&mut *lock(&invocation.watches));
        for watch in watches {
            tokio::spawn(async move {
                let _ = watch.stop().await;
            });
        }
        let timers: Vec<(Arc<()>, SharedDisarm)> = std::mem::take(&mut *lock(&invocation.timers));
        for (_, timer) in timers {
            timer.disarm();
        }
        // 绑定在调用上的挂起等待（例如工具的 `waitForTask()`）随它一起失败。
        invocation
            .controller
            .abort(Some(AbortReason::Error(Arc::new(EndedError {
                task: invocation.task_id,
            }))));
    }

    /// 作用域里没有活非 background 任务（TS: `#idle`）。
    fn idle(&self, conversation_id: Option<ConversationId>) -> bool {
        let state = self.state();
        self.idle_state(&state, conversation_id)
    }

    fn idle_state(&self, state: &State, conversation_id: Option<ConversationId>) -> bool {
        let scope = match conversation_id {
            None => Scope::Roots,
            Some(conversation_id) => Scope::Conversation(conversation_id),
        };
        // 链未载入的任务还没列到作用域下面；除非走上链判定为否，否则算在里面。
        for id in &state.unloaded {
            let record = state.live.get(id);
            if let Some(record) = record
                && !record.background
                && self.in_scope_state(state, parent_of(&node_of(record)), scope, false)
                    != Some(false)
            {
                return false;
            }
        }
        let starts: Vec<ConversationId> = match conversation_id {
            None => state.roots.iter().copied().collect(),
            Some(conversation_id) => vec![conversation_id],
        };
        for start in starts {
            for item in walk_down(
                state,
                conversation_node(start),
                None,
                &WalkOptions::default(),
            ) {
                if let Some(_live) = item.live
                    && !item.node.background
                {
                    return false;
                }
            }
        }
        true
    }
}

/// `#reserve` 的返回值。
#[derive(Default)]
struct ReserveOutput {
    reservations: Vec<Reservation>,
    abandoned: Vec<TaskId>,
    faulted: Vec<(AnyTaskRecord, String)>,
}

/// 一次预留（TS: `Reservation`）。
struct Reservation {
    invocation: Arc<Invocation>,
    task: AnyTask,
    snapshot: Arc<dyn RegistrySnapshot>,
}

/// 提交暂存（overlay）：本提交写下的记录覆盖已提交记录（TS: `Overlay`）。
///
/// `tasks`/`edges` 来自 `tx.staged_tasks()`/`tx.staged_conversations()`；`links` 是
/// `walk_down()` 需要的、本提交新任务的父→子链接缓存。用 `Mutex<Option<...>>` 是因为
/// `walk_down()` 只持有 `&Overlay`，而构建链接要写缓存。
struct Overlay {
    tasks: IndexMap<TaskId, AnyTaskRecord>,
    edges: IndexMap<ConversationId, Option<TaskId>>,
    /// 为提交中的新任务构建的"父节点 → 子节点"链接。
    links: Mutex<Option<Arc<IndexMap<Node, IndexSet<Node>>>>>,
}

impl Overlay {
    fn new(
        tasks: IndexMap<TaskId, AnyTaskRecord>,
        edges: IndexMap<ConversationId, Option<TaskId>>,
    ) -> Self {
        Overlay {
            tasks,
            edges,
            links: Mutex::new(None),
        }
    }

    fn edge(&self, id: ConversationId) -> Option<Option<TaskId>> {
        self.edges.get(&id).copied()
    }
}

/// `overlayOf(tx)`。
fn overlay_of(tx: &Transaction) -> Overlay {
    let mut tasks = IndexMap::new();
    for record in tx.staged_tasks() {
        tasks.insert(record.id, record);
    }
    let mut edges = IndexMap::new();
    for record in tx.staged_conversations() {
        edges.insert(record.id, record.owner.map(|owner| owner.task_id));
    }
    Overlay::new(tasks, edges)
}

/// `#above()` 的产出一项。
#[derive(Default)]
struct WalkOptions<'a> {
    cross_background: bool,
    enter: Option<&'a dyn Fn(TaskId) -> bool>,
}

/// `#walkDown()` 的产出一项。
struct WalkItem {
    id: TaskId,
    node: TaskNode,
    live: Option<AnyTaskRecord>,
}

/// 向下遍历（TS: `#walkDown`）：`#below` 列出的与 overlay 里本次提交的新任务。
///
/// 迭代式深度优先（`pending` 当栈），每个节点先产出再决定是否进入：任务节点总是被产出
/// （`live` 只在该记录非终态时给出），但只有 `cross_background` 为真或它不是 background、
/// 且 `enter` 谓词（默认全部允许）通过时才压栈。不在 `live`/`settled`/overlay 中的任务
/// 直接跳过；会话节点无条件压栈。
fn walk_down(
    state: &State,
    start: Node,
    overlay: Option<&Overlay>,
    options: &WalkOptions<'_>,
) -> Vec<WalkItem> {
    let links = overlay.map(|overlay| overlay_links(state, overlay));
    let mut out = Vec::new();
    let mut pending: Vec<Node> = vec![start];
    while let Some(at) = pending.pop() {
        let first = state.below.get(&at);
        let second = links.as_ref().and_then(|links| links.get(&at));
        for children in [first, second].into_iter().flatten() {
            for child in children {
                match child {
                    Node::Conversation(_) => pending.push(*child),
                    Node::Task(id) => {
                        let record = overlay
                            .and_then(|overlay| overlay.tasks.get(id))
                            .or_else(|| state.live.get(id));
                        let node = record
                            .map(node_of)
                            .or_else(|| state.settled.get(id).copied());
                        let Some(node) = node else {
                            continue;
                        };
                        let live = record
                            .filter(|record| record.state.status() != TaskStatus::Terminal)
                            .cloned();
                        out.push(WalkItem {
                            id: *id,
                            node,
                            live,
                        });
                        let enter = options.enter.is_none_or(|enter| enter(*id));
                        if (options.cross_background || !node.background) && enter {
                            pending.push(*child);
                        }
                    }
                }
            }
        }
    }
    out
}

/// `#links(overlay)`：向下遍历需要的、本次提交新任务的链接。
///
/// 只处理“还没进入 `live`/`settled`”的 overlay 任务：从它的父节点向上逐层补边；当前节点
/// 是活任务、或本提交里的任务、或上层已经能通过 `#below` 列出它时就停止；否则用 `settled`
/// 里的归属字段继续向上，直到链端。结果缓存在 `overlay.links`，同一次提交里的多次
/// `walk_down` 复用。
fn overlay_links(state: &State, overlay: &Overlay) -> Arc<IndexMap<Node, IndexSet<Node>>> {
    if let Some(links) = lock(&overlay.links).clone() {
        return links;
    }
    let mut links: IndexMap<Node, IndexSet<Node>> = IndexMap::new();
    for record in overlay.tasks.values() {
        if state.live.contains_key(&record.id) || state.settled.contains_key(&record.id) {
            continue;
        }
        let mut child = Node::Task(record.id);
        let mut parent: Option<Node> = Some(parent_node(&node_of(record)));
        while let Some(current) = parent {
            if links
                .get(&current)
                .is_some_and(|children| children.contains(&child))
            {
                break;
            }
            add_to(&mut links, current, child);
            if let Node::Task(id) = current
                && (state.live.contains_key(&id) || overlay.tasks.contains_key(&id))
            {
                break;
            }
            let above: Option<Node> = match current {
                Node::Conversation(conversation_id) => {
                    match overlay
                        .edge(conversation_id)
                        .or_else(|| state.edges.get(&conversation_id).copied())
                    {
                        None => None,
                        Some(None) => None,
                        Some(Some(owner)) => Some(Node::Task(owner)),
                    }
                }
                Node::Task(id) => state.settled.get(&id).map(parent_node),
            };
            if let Some(above) = above
                && state
                    .below
                    .get(&above)
                    .is_some_and(|children| children.contains(&current))
            {
                break;
            }
            child = current;
            parent = above;
        }
    }
    let links = Arc::new(links);
    *lock(&overlay.links) = Some(Arc::clone(&links));
    links
}

/// 暂存一条 abort 标记，同一任务只保留一个。
///
/// 优先级：已暂存“请求”（`reason == None`）时，后面的 `restart` 标记不覆盖；记录已带
/// `restart` 原因时，本次不带 reason 的标记会把它升级为请求。其余情况写入/覆盖本条。
fn mark_task(
    marks: &mut IndexMap<TaskId, (AnyTaskRecord, Option<TaskAbortReason>)>,
    record: &AnyTaskRecord,
    reason: Option<TaskAbortReason>,
) {
    if let Some(staged) = marks.get(&record.id)
        && staged.1.is_none()
    {
        return;
    }
    if record.abort_requested && (record.abort_reason.is_none() || reason.is_some()) {
        return;
    }
    marks.insert(record.id, (record.clone(), reason));
}

/// 从终态结果里取出调度器会写的那两个变体（TS: `SchedulerOutcome`）。
fn scheduler_outcome_of(outcome: &TaskOutcome) -> Option<SchedulerOutcome> {
    match outcome {
        TaskOutcome::Orphaned { reason } => Some(SchedulerOutcome::Orphaned {
            reason: reason.clone(),
        }),
        TaskOutcome::Faulted { error } => Some(SchedulerOutcome::Faulted {
            error: error.clone(),
        }),
        _ => None,
    }
}

fn report_task_of(id: TaskId, kind: &str, conversation_id: ConversationId) -> ReportTask {
    ReportTask {
        id,
        kind: kind.to_owned(),
        conversation_id,
    }
}

/// `installerOf(snapshot, task)`：安装该定义（最后列出的）扩展。
fn installer_of(snapshot: &Arc<dyn RegistrySnapshot>, task: Option<&AnyTask>) -> Option<String> {
    let task = task?;
    snapshot
        .installed()
        .iter()
        .rev()
        .find(|extension| extension.tasks().iter().any(|owned| same_task(owned, task)))
        .map(|extension| extension.name().to_owned())
}

/// `kiana.durable.tasks.active` 指标的属性：只有任务 kind。
fn active_attributes(record: &AnyTaskRecord) -> SpanAttributes {
    let mut attributes = SpanAttributes::new();
    attributes.insert(
        "task".to_owned(),
        AttributeValue::String(record.kind.clone()),
    );
    attributes
}

// ---------------------------------------------------------------------------
// 阶段执行
// ---------------------------------------------------------------------------

/// `#phase` 每次调用期间共享的解析结果（TS: `#run` 里的 `Phase`）。
///
/// `snapshot` 在 `decide()` 判断有进展时被刷新（`set_snapshot`）；`task` 是本次预留取得的
/// 定义，整个调用期间不变（新定义只有在 decide 写回 `pending` 后才在下一轮接管）。
/// `agent`/`resolved` 在每个 handler 之前由 `reset_phase()` 清空，实现“每个 phase 至多
/// 解析一次 agent”；`resolved` 供同步的 `HookRunner` 使用。
struct PhaseState {
    snapshot: Mutex<Arc<dyn RegistrySnapshot>>,
    task: Mutex<AnyTask>,
    /// 每个 phase handler 最多解析一次 agent；进入下一个 handler 时清空。
    agent: Mutex<Option<SharedAgent>>,
    /// 最近一次成功解析的 agent（`HookRunner` 的同步近似）。
    resolved: Mutex<Option<Arc<Agent>>>,
    /// 上一次 `task_outdated` 报告针对的定义（TS: `state.reported`）。
    reported: Mutex<Option<Option<AnyTask>>>,
}

/// 一个 phase handler 的结果（TS: `PhaseResult`）。
struct PhaseResult {
    checkpoint: Checkpoint,
    failure: Option<BoxError>,
}

/// `#decide` 的返回值（TS: `true | false | { fault }`）。
enum Decision {
    /// 继续下一个 phase。
    Continue,
    /// 结束调用。
    End,
    /// 以 `faulted` 结束。
    Fault(BoxError),
    /// 决策本身抛错：提交被拒绝，`#step` 的 catch 处理它。
    Throw(BoxError),
}

/// 阶段之间共享的 agent 解析（TS: `phase.agent` 的 promise）。
type SharedAgent =
    Shared<BoxFuture<'static, Result<Arc<Agent>, Arc<dyn std::error::Error + Send + Sync>>>>;

impl PhaseState {
    fn new(task: AnyTask, snapshot: Arc<dyn RegistrySnapshot>) -> Self {
        PhaseState {
            snapshot: Mutex::new(snapshot),
            task: Mutex::new(task),
            agent: Mutex::new(None),
            resolved: Mutex::new(None),
            reported: Mutex::new(None),
        }
    }

    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::clone(&lock(&self.snapshot))
    }

    fn set_snapshot(&self, snapshot: Arc<dyn RegistrySnapshot>) {
        *lock(&self.snapshot) = snapshot;
    }

    fn task(&self) -> AnyTask {
        lock(&self.task).clone()
    }

    fn reset_phase(&self) {
        *lock(&self.agent) = None;
        *lock(&self.resolved) = None;
    }
}

impl TaskScheduler {
    /// 运行 phase 处理器，每次之前有一个在线上决定调用是否继续的 step（TS: `#run`）。
    ///
    /// 循环结构：先用 `previous=None` 的 `step()` 做进入第一个 handler 前的决策（应用
    /// 规则 1/2/3，可能直接结束）；之后每个 phase handler 前后各一次 `step()`，handler
    /// 的结果作为 `previous` 交给 `decide()`，由它决定继续、故障还是交回 `pending`。
    /// `closing` 后不再进入下一轮。
    async fn run(&self, reservation: &Reservation) -> Result<(), BoxError> {
        let invocation = Arc::clone(&reservation.invocation);
        let phase = Arc::new(PhaseState::new(
            reservation.task.clone(),
            Arc::clone(&reservation.snapshot),
        ));
        let runtime =
            SchedulerRuntime::new(self.clone(), Arc::clone(&invocation), Arc::clone(&phase));
        let this = self.clone();
        let invocation_for_decide = Arc::clone(&invocation);
        let phase_for_decide = Arc::clone(&phase);
        let mut current = self
            .step(&invocation, move |tx, next| {
                this.decide(tx, next, None, &phase_for_decide, &invocation_for_decide)
            })
            .await;
        while current.is_some() && !self.closing() {
            let task = current.expect("checked above");
            let checkpoint = checkpoint_of(&task.state);
            let task_for_phase = task.clone();
            let this = self.clone();
            let invocation_for_phase = Arc::clone(&invocation);
            let phase_for_phase = Arc::clone(&phase);
            let runtime_for_phase = runtime.clone();
            let run = Box::pin(async move {
                // 每个 phase handler 重新解析自己的 agent（`PhaseState::reset_phase`）。
                phase_for_phase.reset_phase();
                let definition = phase_for_phase.task();
                let phase_name = checkpoint_phase(&checkpoint);
                let handler = match erased_phase(&definition.definition, &phase_name) {
                    Ok(handler) => handler,
                    // phase 名不在定义里：把错误当成一次 handler 失败，仍走同一个 decide
                    // （TS 在 try 内取 handler，TypeError 由 PhaseResult 收下）。
                    Err(error) => {
                        let previous = PhaseResult {
                            checkpoint: checkpoint.clone(),
                            failure: Some(error),
                        };
                        let this_for_step = this.clone();
                        let this_for_decide = this.clone();
                        let phase_for_step = Arc::clone(&phase_for_phase);
                        let invocation_for_step = Arc::clone(&invocation_for_phase);
                        let invocation_for_decide = Arc::clone(&invocation_for_phase);
                        return Ok(this_for_step
                            .step(&invocation_for_step, move |tx, next| {
                                this_for_decide.decide(
                                    tx,
                                    next,
                                    Some(previous),
                                    &phase_for_step,
                                    &invocation_for_decide,
                                )
                            })
                            .await);
                    }
                };
                let previous = match handler
                    .run(
                        &task_for_phase,
                        &runtime_for_phase,
                        &*invocation_for_phase.context(),
                    )
                    .await
                {
                    Ok(()) => PhaseResult {
                        checkpoint: checkpoint.clone(),
                        failure: None,
                    },
                    Err(error) => PhaseResult {
                        checkpoint: checkpoint.clone(),
                        failure: Some(error),
                    },
                };
                let this_for_step = this.clone();
                let this_for_decide = this.clone();
                let phase_for_step = Arc::clone(&phase_for_phase);
                let invocation_for_step = Arc::clone(&invocation_for_phase);
                let invocation_for_decide = Arc::clone(&invocation_for_phase);
                Ok(this_for_step
                    .step(&invocation_for_step, move |tx, next| {
                        this_for_decide.decide(
                            tx,
                            next,
                            Some(previous),
                            &phase_for_step,
                            &invocation_for_decide,
                        )
                    })
                    .await)
            });
            current = self.phase(&invocation, &task, run).await?;
        }
        Ok(())
    }

    /// 一个 phase 拥有自己的决定提交，不拥有下一个 handler、持久等待或恢复（TS: `#phase`）。
    ///
    /// 职责：构造父 span（`task.trace` 提取，否则用 Harness telemetry）、写 start 属性、
    /// 把 span 叠加进 invocation context、执行 `run`（handler + step），最后按终态/状态写
    /// `outcome` 属性、span status 与 phase 时长指标。`attempt` 与 `resumed` 也在这里读取。
    /// `run` 的结果原样返回；span 启动失败转成 `io_error(error.message)`。
    async fn phase<T>(
        &self,
        invocation: &Arc<Invocation>,
        task: &AnyTaskRecord,
        run: BoxFuture<'static, Result<T, BoxError>>,
    ) -> Result<T, BoxError>
    where
        T: Send + 'static,
    {
        let parent: Arc<dyn TelemetryContext> = match &task.trace {
            None => Arc::clone(&self.deps.telemetry),
            Some(trace) => match self.deps.telemetry.propagation() {
                Some(propagation) => propagation.extract(Some(trace)),
                None => Arc::clone(&self.deps.telemetry),
            },
        };
        let checkpoint = checkpoint_of(&task.state);
        let attempt = checkpoint_attempt(&checkpoint);
        let resumed = { self.state().resumed.shift_remove(&task.id) };
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "task.id".to_owned(),
            AttributeValue::Number(task.id.get() as f64),
        );
        attributes.insert(
            "task.name".to_owned(),
            AttributeValue::String(task.kind.clone()),
        );
        attributes.insert(
            "phase".to_owned(),
            AttributeValue::String(checkpoint_phase(&checkpoint)),
        );
        attributes.insert(
            "kind".to_owned(),
            AttributeValue::String(invocation.mode.as_str().to_owned()),
        );
        attributes.insert("resumed".to_owned(), AttributeValue::Boolean(resumed));
        if let Some(attempt) = attempt {
            attributes.insert("attempt".to_owned(), AttributeValue::Number(attempt));
        }
        let starter =
            create_typed_span_starter(parent, std::slice::from_ref(&*DURABLE_TELEMETRY_SCHEMA));
        let invocation = Arc::clone(invocation);
        let task = task.clone();
        let this = self.clone();
        let result = starter
            .start("kiana.durable.phase", attributes, move |span, _child| {
                let invocation = Arc::clone(&invocation);
                let task = task.clone();
                let this = this.clone();
                let checkpoint = checkpoint.clone();
                Box::pin(async move {
                    if let Some(propagation) = span.propagation()
                        && let Some(links) = invocation.links()
                    {
                        for carrier in links {
                            let context = propagation.extract(Some(&carrier));
                            span.add_link(SpanLink {
                                context: context.span_context_id(),
                                attributes: None,
                            });
                        }
                    }
                    let previous = invocation.context();
                    invocation
                        .set_context(with_telemetry(Arc::clone(&span), Arc::clone(&previous)));
                    let started = Instant::now();
                    let value = run.await;
                    invocation.set_context(previous);
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    // 终态优先取 `step()` 写入的 fault（此时记录可能已离开 live），其次取
                    // 记录里的 terminal/completing outcome。
                    let record = invocation.record().unwrap_or_else(|| task.clone());
                    let terminal: Option<TaskOutcome> = invocation
                        .take_fault()
                        .map(|outcome| outcome.into_task_outcome())
                        .or_else(|| match record.state.status() {
                            TaskStatus::Terminal | TaskStatus::Completing => {
                                record.state.outcome().cloned()
                            }
                            _ => None,
                        });
                    let outcome = match &terminal {
                        Some(outcome) => match outcome_status(outcome) {
                            TaskOutcomeStatus::Completed => "done".to_owned(),
                            TaskOutcomeStatus::Aborted => "aborted".to_owned(),
                            _ => "faulted".to_owned(),
                        },
                        None => match record.state.status() {
                            TaskStatus::Waiting => "waiting".to_owned(),
                            TaskStatus::Sleeping => "sleeping".to_owned(),
                            _ if invocation.is_ended() || this.closing() => "aborted".to_owned(),
                            _ => {
                                format!("next={}", checkpoint_phase(&checkpoint_of(&record.state)))
                            }
                        },
                    };
                    let mut end_attributes = SpanAttributes::new();
                    end_attributes.insert(
                        "outcome".to_owned(),
                        AttributeValue::String(outcome.clone()),
                    );
                    span.set_attributes(end_attributes);
                    if let Some(terminal) = &terminal {
                        let (name, message) = match terminal {
                            TaskOutcome::Faulted { error } => {
                                ("task_faulted", Some(error.message.clone()))
                            }
                            TaskOutcome::Failed { error, .. } => {
                                ("task_failed", Some(error.message.clone()))
                            }
                            TaskOutcome::Orphaned { reason } => {
                                ("task_orphaned", Some(reason.clone()))
                            }
                            _ => ("", None),
                        };
                        if let Some(message) = message {
                            span.set_status(SpanStatus::Error {
                                error: Some(StatusError {
                                    name: name.to_owned(),
                                    message: owned_slice(&message, 0, Some(500)),
                                }),
                            });
                        } else if outcome == "aborted" {
                            span.set_status(SpanStatus::Ok);
                        }
                    } else if outcome == "aborted" {
                        span.set_status(SpanStatus::Ok);
                    }
                    let mut metric_attributes = SpanAttributes::new();
                    metric_attributes
                        .insert("task".to_owned(), AttributeValue::String(task.kind.clone()));
                    metric_attributes.insert(
                        "phase".to_owned(),
                        AttributeValue::String(checkpoint_phase(&checkpoint)),
                    );
                    metric_attributes.insert("outcome".to_owned(), AttributeValue::String(outcome));
                    this.deps.duration_metric.record(
                        elapsed,
                        Some(metric_attributes),
                        Some(span.as_ref() as &dyn TelemetryContext),
                    );
                    value
                })
            })
            .await;
        match result {
            Ok(value) => value,
            Err(error) => Err(io_error(error.message)),
        }
    }

    /// run 调用的优先级规则，在线上（TS: `#decide`）。规则 1（终态、`completing`、`waiting`、
    /// `sleeping`）与 2（closing）由 `#step` 应用。
    ///
    /// 本函数依次应用：3 abort 标记 / 取消中的 owner → 4 未捕获错误 → 6 没有持久进展
    /// （checkpoint 结构相等）→ 5 有进展：刷新 registry 快照，若新定义能接管就写回
    /// `pending` 交给下一轮，否则在定义对象变化时报告一次 `task_outdated`，再继续下一个
    /// phase。`Throw` 表示决策本身失败（例如 `set_task` 出错），由 `step()` 的提交失败路径
    /// 处理。
    fn decide(
        &self,
        tx: &Transaction,
        current: &AnyTaskRecord,
        previous: Option<PhaseResult>,
        state: &PhaseState,
        invocation: &Arc<Invocation>,
    ) -> Decision {
        // 3. abort 标记：结束；一旦它下面的普通 owned 工作没了，会启动一个全新的 abort 调用。
        if current.abort_requested {
            return Decision::End;
        }
        // owner 的取消意图也会结束它，在它的级联标记之前；预留随后会挡住它。
        if !current.background && self.below_cancelled(parent_of(&node_of(current))) {
            self.state().cascade_pending = true;
            self.schedule_reconcile();
            return Decision::End;
        }
        let Some(previous) = previous else {
            return Decision::Continue;
        };
        // 4. 未捕获错误。
        if let Some(failure) = previous.failure {
            return Decision::Fault(failure);
        }
        // 6. 没有持久进展。
        if json_equal(&checkpoint_of(&current.state), &previous.checkpoint) {
            let message = format!(
                "Task {} phase {} returned without durable progress",
                current.kind,
                checkpoint_phase(&previous.checkpoint)
            );
            return Decision::Fault(io_error(message));
        }
        // 5. 进展：刷新快照；若能接管，交给替换定义。
        let snapshot =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.snapshot())) {
                Ok(snapshot) => snapshot,
                Err(payload) => return Decision::Throw(panic_to_error(payload)),
            };
        state.set_snapshot(Arc::clone(&snapshot));
        let next = snapshot.task(&current.kind);
        if !same_optional_task(next.as_ref(), Some(&state.task())) {
            if let Some(next) = &next
                && can_reserve(next, current)
            {
                let checkpoint = checkpoint_of(&current.state);
                if let Err(error) =
                    tx.set_task(with_state(current, TaskState::Pending { checkpoint }))
                {
                    return Decision::Throw(error);
                }
                return Decision::End;
            }
            let reported = lock(&state.reported).clone();
            let differs = match &reported {
                None => true,
                Some(reported) => !same_optional_task(reported.as_ref(), next.as_ref()),
            };
            if differs {
                *lock(&state.reported) = Some(next.clone());
                let (task, extension) =
                    self.about_record_with(current, invocation.extension.clone());
                let reason = if next.is_none() {
                    TaskOutdatedReason::MissingTask
                } else {
                    TaskOutdatedReason::IncompatibleTask
                };
                let error = io_error(format!(
                    "Task {} keeps running under its old {} definition",
                    current.id, current.kind
                ));
                self.report(
                    &*error,
                    ReportInfo::TaskOutdated {
                        task,
                        extension,
                        reason,
                    },
                    Some(&*invocation.context()),
                );
            }
        }
        Decision::Continue
    }

    /// 运行一次 abort 处理器；规则 1、2、4 适用，返回时没有结果即 fault（TS: `#runAbort`）。
    ///
    /// abort handler 只跑一次，不循环 phase。handler 的错误优先作为 fault 原因；handler
    /// 正常返回但没写终态时，用“returned without a terminal outcome”作为 fault。fault 以
    /// `Faulted` outcome 落盘；若任务仍持有活的普通 owned 工作，`step()` 里的 `terminate()`
    /// 先写成 `completing`（outcome 仍是 `Faulted`），之后由 `finalize()` 落成 `terminal`。
    async fn run_abort(&self, reservation: &Reservation) -> Result<(), BoxError> {
        let invocation = Arc::clone(&reservation.invocation);
        let current = { self.state().live.get(&invocation.task_id).cloned() };
        let Some(current) = current else {
            return Ok(());
        };
        if self.closing() {
            return Ok(());
        }
        let this = self.clone();
        let invocation_for_phase = Arc::clone(&invocation);
        let task = reservation.task.clone();
        let snapshot = Arc::clone(&reservation.snapshot);
        let record = current.clone();
        let run = Box::pin(async move {
            let mut failure: Option<BoxError> = None;
            let phase = Arc::new(PhaseState::new(task.clone(), snapshot));
            let runtime = SchedulerRuntime::new(
                this.clone(),
                Arc::clone(&invocation_for_phase),
                Arc::clone(&phase),
            );
            match erased_abort(&task.definition) {
                Ok(handler) => {
                    if let Err(error) = handler
                        .run(&current, &runtime, &*invocation_for_phase.context())
                        .await
                    {
                        failure = Some(error);
                    }
                }
                Err(error) => failure = Some(error),
            }
            let message = format!(
                "Abort handler of task {} returned without a terminal outcome",
                invocation_for_phase.task_id
            );
            let error = failure.unwrap_or_else(|| io_error(message));
            this.step(&invocation_for_phase, move |_tx, _current| {
                Decision::Fault(error)
            })
            .await;
            Ok::<(), BoxError>(())
        });
        self.phase(&invocation, &record, run).await?;
        Ok(())
    }

    /// 变更线上的一个同步决定（TS: `#step`）。不再 running 的任务（规则 1）或 closing 的
    /// Harness（规则 2）不写入就结束调用；否则 `decide` 可以暂存写入并返回是否继续。
    ///
    /// 决策在 `StepChange` 的提交回调里执行；`Decision::Fault` 先 `terminate()` 写终态，
    /// 再把错误交给 `step()` 在提交成功后报告（`task_faulted`/`threw`），保证宿主看到报告时
    /// 故障已经持久化。提交本身失败时先 `end()` 再 `fail_session()`，并且不返回任务。
    async fn step<F>(&self, invocation: &Arc<Invocation>, decide: F) -> Option<AnyTaskRecord>
    where
        F: for<'a> FnOnce(&'a Transaction, &'a AnyTaskRecord) -> Decision + Send,
    {
        let fault = Arc::new(Mutex::new(None::<BoxError>));
        let adapter = StepChange {
            scheduler: self,
            invocation,
            decide: Mutex::new(Some(decide)),
            fault: Arc::clone(&fault),
        };
        let context = without_abort_signal(invocation.context());
        let result = self
            .deps
            .session
            .commit_transaction(&adapter, &*context, None, CommitOptions::default())
            .await;
        match result {
            Ok(value) => {
                let continued = value
                    .downcast::<Option<AnyTaskRecord>>()
                    .map(|value| *value)
                    .unwrap_or(None);
                // fault 持久化之后才报告：宿主可以据此计数，例如计入熔断器。
                if let Some(error) = lock(&fault).take() {
                    let (task, extension) = self.about_invocation(invocation);
                    self.report(
                        &*error,
                        ReportInfo::TaskFaulted {
                            task,
                            extension,
                            reason: TaskFaultedReason::Threw,
                        },
                        Some(&*invocation.context()),
                    );
                }
                continued
            }
            Err(error) => {
                // step 写的是调度器自己的决定、故障或终态记录；这里的抛错否则会让任务保持
                // running，被重新预留并再次运行。
                self.end(invocation);
                self.fail_session(error, Some(&*invocation.context()));
                None
            }
        }
    }

    /// 写入调度器决定的终态。任务的普通 owned 工作还活时保持为 `completing`，它的 Harness
    /// 清理等最终提交（TS: `#terminate`）。
    ///
    /// 先 `load_scopes(false)` 载入标记为未加载的链，再结合本次提交的 overlay 判断是否还
    /// 持有活的普通工作。写 `terminal` 时立即调用 `settle_outcome`；写 `completing` 时把
    /// 清理推迟到 `finalize()` 落定的那次提交。
    async fn terminate(
        &self,
        tx: &Transaction,
        record: &AnyTaskRecord,
        outcome: SchedulerOutcome,
    ) -> Result<(), BoxError> {
        self.load_scopes(false).await?;
        let overlay = overlay_of(tx);
        if self.has_owned_live(record.id, Some(&overlay), None) {
            tx.set_task(with_state(
                record,
                TaskState::Completing {
                    outcome: outcome.clone().into_task_outcome(),
                },
            ))?;
            return Ok(());
        }
        tx.set_task(with_state(
            record,
            TaskState::Terminal {
                outcome: outcome.clone().into_task_outcome(),
            },
        ))?;
        (self.deps.settle_outcome)(tx, record, &outcome).await
    }

    /// 用任务提交的状态替换 running 任务的状态。终态在普通 owned 工作还活时保持为
    /// `completing`，按提交里的候选判断，所以同一提交在任务下面创建的工作也算（TS: `#commitState`）。
    ///
    /// 校验先行：`waiting` 走 `validate_wait()`，`sleeping` 走 `validate_sleep()`（abort
    /// handler 不能睡）。`terminal` 先载入链并检查 owned 工作，仍有时改写成 `completing`
    /// 且清 restart 计数。最后比较新旧状态的结构等价：不同才 `progressed()` 清 restart
    /// 计数——相同 checkpoint 的重复提交（例如一次副作用前的意图）不算进展。
    async fn commit_state(
        &self,
        tx: &Transaction,
        invocation: &Arc<Invocation>,
        current: &AnyTaskRecord,
        next: TaskState,
    ) -> Result<(), BoxError> {
        if let TaskState::Waiting { on, policy, .. } = &next {
            self.validate_wait(tx, invocation, current, on, *policy)
                .await?;
        }
        if let TaskState::Sleeping { until, .. } = &next {
            validate_sleep(invocation, current.id, *until)?;
        }
        let mut outcome = None;
        if let TaskState::Terminal { outcome: terminal } = &next {
            outcome = Some(terminal.clone());
            let overlay = overlay_of(tx);
            self.load_scopes(false).await?;
            for record in overlay.tasks.values() {
                self.load_chain(parent_of(&node_of(record)), Some(&overlay))
                    .await?;
            }
            if self.has_owned_live(current.id, Some(&overlay), None) {
                tx.set_task(with_state(
                    &progressed(current),
                    TaskState::Completing {
                        outcome: terminal.clone(),
                    },
                ))?;
                return Ok(());
            }
        }
        let _ = outcome;
        // 任务提交的、与其当前状态不同的状态是进展：restart 计数重新开始。相同的 checkpoint
        // 再次提交（例如作为一次副作用之前的意图）不是。
        let same = json_equal(&state_json(&current.state), &state_json(&next));
        let progressed_current;
        let base = if same {
            current
        } else {
            progressed_current = progressed(current);
            &progressed_current
        };
        tx.set_task(with_state(base, next))?;
        Ok(())
    }

    /// 等待只能点名已存在的任务，`failFast` 只能等它拥有的任务；等待图不得成环（TS: `#validateWait`）。
    ///
    /// 目标记录按 overlay → live → Storage 的顺序解析。只有“活的、且不是自己拥有”的目标
    /// 进入环检测队列：活着的子任务已经持有等待者，终态任务没有边。所有错误文本与 TS 逐字
    /// 一致，会作为一次提交拒绝传回任务代码。
    async fn validate_wait(
        &self,
        tx: &Transaction,
        invocation: &Arc<Invocation>,
        current: &AnyTaskRecord,
        on: &[TaskId],
        policy: JoinPolicy,
    ) -> Result<(), BoxError> {
        if invocation.mode == InvocationMode::Abort {
            return Err(io_error(format!(
                "Abort handler of task {} cannot wait",
                current.id
            )));
        }
        let overlay = overlay_of(tx);
        let mut targets: Vec<TaskId> = Vec::new();
        for id in on {
            let member = overlay
                .tasks
                .get(id)
                .cloned()
                .or_else(|| self.state().live.get(id).cloned());
            let member = match member {
                Some(member) => Some(member),
                None => self.deps.storage.task(*id, &*self.deps.context).await?,
            };
            let Some(member) = member else {
                return Err(io_error(format!("Task {id} does not exist")));
            };
            if policy == JoinPolicy::FailFast && member.owner != Some(current.id) {
                return Err(io_error(format!(
                    "Task {} can wait failFast only on tasks it owns; {id} is not one",
                    current.id
                )));
            }
            // 活着的子任务已经持有等待者；终态任务没有边。
            if member.state.status() != TaskStatus::Terminal && member.owner != Some(current.id) {
                targets.push(*id);
            }
        }
        if targets.is_empty() {
            return Ok(());
        }
        if let Some(cycle) = self.wait_cycle(current.id, &targets, &overlay).await? {
            let chain = std::iter::once(current.id)
                .chain(cycle.iter().copied())
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(io_error(format!(
                "Task {} cannot wait on {}: {chain} is a wait cycle",
                current.id, cycle[0]
            )));
        }
        Ok(())
    }

    /// 等待图里从 `on` 的某个成员回到 `task` 的路径（TS: `#waitCycle`）。边 X -> Y 表示 X
    /// 在 Y 之前不能落定。每个任务只访问一次，每棵子树只走一次。
    ///
    /// `from` 是首次到达时的父指针，`pending` 是深度优先的待展开栈，`walked` 记住已经向下
    /// 展开过的任务。边有两类：等待中（非 abort）任务的 `on` 成员，以及活任务的普通 owned
    /// 工作（它们让持有者保持 `completing`）。命中 `task` 时沿 `from` 回溯出完整环路径。
    async fn wait_cycle(
        &self,
        task: TaskId,
        on: &[TaskId],
        overlay: &Overlay,
    ) -> Result<Option<Vec<TaskId>>, BoxError> {
        // owned 工作由向下遍历找到，需要每个活任务的链已载入。
        self.load_scopes(false).await?;
        let mut from: IndexMap<TaskId, Option<TaskId>> = IndexMap::new();
        let mut walked: IndexSet<TaskId> = IndexSet::new();
        let mut pending: Vec<TaskId> = Vec::new();
        for id in on {
            reach(&mut from, &mut pending, *id, None);
        }
        while let Some(id) = pending.pop() {
            if id == task {
                let mut steps: Vec<TaskId> = Vec::new();
                let mut at = Some(id);
                while let Some(current) = at {
                    steps.push(current);
                    at = from.get(&current).copied().flatten();
                }
                steps.reverse();
                return Ok(Some(steps));
            }
            let record = overlay
                .tasks
                .get(&id)
                .cloned()
                .or_else(|| self.state().live.get(&id).cloned());
            let Some(record) = record else {
                continue;
            };
            if record.state.status() == TaskStatus::Terminal {
                continue;
            }
            // abort 标记的任务只等它的 owned 工作（abort 自下而上，见 `#waitingOn`）。
            if record.state.status() == TaskStatus::Waiting
                && !record.abort_requested
                && let TaskState::Waiting { on: members, .. } = &record.state
            {
                for member in members {
                    reach(&mut from, &mut pending, *member, Some(id));
                }
            }
            if walked.contains(&id) {
                continue;
            }
            walked.insert(id);
            // 已走过的任务下面不再走：它自己的遍历已经覆盖了下面。
            let walk = {
                let state = self.state();
                walk_down(
                    &state,
                    Node::Task(id),
                    Some(overlay),
                    &WalkOptions {
                        cross_background: false,
                        enter: Some(&|below| !walked.contains(&below)),
                    },
                )
            };
            let mut found: Vec<TaskId> = Vec::new();
            for item in walk {
                if item.live.is_none() || item.node.background {
                    continue;
                }
                found.push(item.id);
                reach(&mut from, &mut pending, item.id, Some(id));
            }
            for below in found {
                walked.insert(below);
            }
        }
        Ok(None)
    }
}

/// `#waitCycle` 的 `reach` 闭包。
fn reach(
    from: &mut IndexMap<TaskId, Option<TaskId>>,
    pending: &mut Vec<TaskId>,
    id: TaskId,
    via: Option<TaskId>,
) {
    if from.contains_key(&id) {
        return;
    }
    from.insert(id, via);
    pending.push(id);
}

/// 两个可选定义的身份比较（TS: `state.task !== next` 的包装）。
fn same_optional_task(left: Option<&AnyTask>, right: Option<&AnyTask>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => same_task(left, right),
        _ => false,
    }
}

/// `#step` 的提交回调。
///
/// `decide` 只能取用一次（`Mutex<Option<F>>`）；`run()` 先重读 live 记录并应用规则 1
/// （不是 running 就结束）与规则 2（closing 就结束），再调用决策。`Continue` 返回当前记录，
/// 其余决策返回 `None` 结束调用；`Fault` 还会先 `terminate()` 写入终态。
struct StepChange<'a, F> {
    scheduler: &'a TaskScheduler,
    invocation: &'a Arc<Invocation>,
    decide: Mutex<Option<F>>,
    fault: Arc<Mutex<Option<BoxError>>>,
}

#[async_trait]
impl<F> TransactionChange for StepChange<'_, F>
where
    F: for<'a> FnOnce(&'a Transaction, &'a AnyTaskRecord) -> Decision + Send,
{
    async fn run(&self, tx: &Transaction) -> Result<Box<dyn Any + Send>, BoxError> {
        let id = self.invocation.task_id;
        let found = { self.scheduler.state().live.get(&id).cloned() };
        let current = found.filter(|record| record.state.status() == TaskStatus::Running);
        let decision = match &current {
            Some(current) if !self.scheduler.closing() => {
                let decide = lock(&self.decide)
                    .take()
                    .ok_or_else(|| io_error("the step decision already ran"))?;
                decide(tx, current)
            }
            _ => Decision::End,
        };
        match decision {
            Decision::Continue => Ok(Box::new(current)),
            Decision::End => {
                self.scheduler.end(self.invocation);
                Ok(Box::new(None::<AnyTaskRecord>))
            }
            Decision::Fault(error) => {
                self.scheduler.end(self.invocation);
                let current = current.expect("a fault decision needs a running task");
                let message = error_message(&*error);
                let outcome = SchedulerOutcome::Faulted {
                    error: TaskOutcomeError {
                        message,
                        detail: None,
                    },
                };
                *lock(&self.invocation.fault) = Some(outcome.clone());
                self.scheduler.terminate(tx, &current, outcome).await?;
                *lock(&self.fault) = Some(error);
                Ok(Box::new(None::<AnyTaskRecord>))
            }
            Decision::Throw(error) => Err(error),
        }
    }
}

// ---------------------------------------------------------------------------
// 调度视角
// ---------------------------------------------------------------------------

impl TaskScheduler {
    /// `indexSizes`：所有权索引的尺寸（测试用）。
    ///
    /// 锁住状态读一次快照；数值是瞬时的，后台 `sweep()`/`load_chain()` 可能随后改变它们。
    pub fn index_sizes(&self) -> SchedulerIndexSizes {
        let state = self.state();
        let mut below = 0;
        for children in state.below.values() {
            below += children.len();
        }
        SchedulerIndexSizes {
            live: state.live.len(),
            settled: state.settled.len(),
            edges: state.edges.len(),
            below,
            roots: state.roots.len(),
            unloaded: state.unloaded.len(),
            drop_queue: state.drop_queue.len(),
        }
    }

    /// 调度状态与每个活任务的派生状态，在 Session 线上读取（TS: `inspect`）。不运行任务代码。
    ///
    /// 先 `load_scopes(false)`（可能读 Storage 补链），再对 `live` 中每个任务按 running →
    /// completing → waiting → sleeping → parked → fit 的优先级给出状态。传入的 `snapshot`
    /// 只用于定义解析，本方法不会重新读取 registry。
    pub async fn inspect(
        &self,
        snapshot: Arc<dyn RegistrySnapshot>,
    ) -> Result<SchedulerInspection, BoxError> {
        self.load_scopes(false).await?;
        let records: Vec<AnyTaskRecord> = {
            let state = self.state();
            state.live.values().cloned().collect()
        };
        let mut tasks = Vec::with_capacity(records.len());
        for record in records {
            let inspection = self.inspect_task(&record, &snapshot);
            tasks.push(TaskInspection {
                record,
                state: inspection,
            });
        }
        let scheduling = if self.closing() {
            SchedulingState::Closing
        } else if self.enabled() {
            SchedulingState::Running
        } else {
            SchedulingState::Paused
        };
        Ok(SchedulerInspection { scheduling, tasks })
    }

    fn inspect_task(
        &self,
        record: &AnyTaskRecord,
        snapshot: &Arc<dyn RegistrySnapshot>,
    ) -> TaskInspectionState {
        if self.state().invocations.contains_key(&record.id) {
            return TaskInspectionState::Running;
        }
        if record.state.status() == TaskStatus::Completing {
            return TaskInspectionState::Completing;
        }
        let on = self.waiting_on(record, false);
        if !on.is_empty() {
            return TaskInspectionState::Waiting { on };
        }
        if self.not_due(record)
            && let TaskState::Sleeping { until, .. } = &record.state
        {
            return TaskInspectionState::Sleeping { until: *until };
        }
        if self.is_parked(record) {
            return TaskInspectionState::Blocked {
                reason: BlockedReason::RestartLimit,
                error: None,
            };
        }
        match self.fit_record(record, snapshot.task(&record.kind)) {
            Fit::Blocked { reason, error } => TaskInspectionState::Blocked {
                reason,
                error: error.map(|error| JsonValue::String(error.to_string())),
            },
            Fit::Ready { task, migrates } => {
                if migrates && task.definition.migrate.is_none() {
                    let error = missing_migration(record, &task.definition);
                    TaskInspectionState::Blocked {
                        reason: BlockedReason::MigrationFailed,
                        error: Some(JsonValue::String(error.to_string())),
                    }
                } else {
                    TaskInspectionState::Ready { migrates }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 调用 runtime
// ---------------------------------------------------------------------------

/// 一次任务调用的 runtime（TS: `#runtime` 返回的 `ErasedRuntime`）。
///
/// 每次调用（`run`/`run_abort`）创建一个，持有 scheduler、invocation 与 phase 状态；
/// 同一次调用的各个 handler 拿到的是它的克隆（共享同样的 `Arc` 字段）。任务状态的提交类
/// 写操作（`commit`/`memo_put`/`sleep_until`）走 `gated()`（Session 线 + `TransactionScope`）；
/// `abort_owned` 是例外，它委托 `TaskScheduler::abort`（Session 线但没有
/// `TransactionScope`）。读操作多数也走 `read_on_line`，`context` 则会直接写
/// `State::contexts`；`ensure_live()` 实现“调用结束后拒绝服务”的统一规则。
#[derive(Clone)]
pub(crate) struct SchedulerRuntime {
    scheduler: TaskScheduler,
    invocation: Arc<Invocation>,
    phase: Arc<PhaseState>,
}

impl SchedulerRuntime {
    fn new(scheduler: TaskScheduler, invocation: Arc<Invocation>, phase: Arc<PhaseState>) -> Self {
        SchedulerRuntime {
            scheduler,
            invocation,
            phase,
        }
    }

    fn ensure_live(&self) -> Result<(), BoxError> {
        if self.invocation.is_ended() {
            return Err(ended_error(&self.invocation));
        }
        Ok(())
    }

    /// 本阶段最多解析一次的 agent；共享解析失败也不留下未观察的拒绝。
    fn shared_agent(&self) -> SharedAgent {
        let mut slot = lock(&self.phase.agent);
        if let Some(existing) = slot.as_ref() {
            return existing.clone();
        }
        let scheduler = self.scheduler.clone();
        let invocation = Arc::clone(&self.invocation);
        let snapshot = self.phase.snapshot();
        let context = invocation.context();
        let phase = Arc::clone(&self.phase);
        let future: BoxFuture<
            'static,
            Result<Arc<Agent>, Arc<dyn std::error::Error + Send + Sync>>,
        > = Box::pin(async move {
            match (scheduler.deps.agent)(invocation.conversation_id, snapshot, context).await {
                Ok(agent) => {
                    *lock(&phase.resolved) = Some(Arc::clone(&agent));
                    Ok(agent)
                }
                Err(error) => Err(Arc::from(error)),
            }
        });
        let shared = future.shared();
        *slot = Some(shared.clone());
        shared
    }

    /// 本阶段候选的 hook 处理器表（TS: `agentHooks(await agent(), taskName)`）。
    ///
    /// 每项带提供它的扩展名：`extension_error` 报告依赖这个归属（TS 的 `hooks.each` 从
    /// `agentHooks()` 的 `extension` 取）。只使用“最近一次成功解析的 agent”（`phase.resolved`），
    /// 因此在 handler 自己调用 `runtime.agent()` 之前，这里返回空表。
    fn hook_tables(&self) -> Vec<(Option<String>, Arc<dyn Any + Send + Sync>)> {
        let Some(agent) = lock(&self.phase.resolved).clone() else {
            return Vec::new();
        };
        let task_name = self.phase.task().definition.name.clone();
        agent_hooks(&agent, &task_name)
            .into_iter()
            .map(|hook| (Some(hook.extension), hook.handlers))
            .collect()
    }

    /// 提交回调用的 gated 变更（TS: `#gated`）。
    ///
    /// 用 `TransactionScope { conversation_id, task_id }` 限定提交可见范围。`is_ended`
    /// 在提交前与回调内各查一次：排队期间调用可能已经结束。`GatedChange::run` 再应用其余
    /// 门控（closing、记录存在、状态、abort 标记）后才真正执行变更。
    async fn gated(
        &self,
        op: GatedOp<'_>,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        if self.invocation.is_ended() {
            return Err(ended_error(&self.invocation));
        }
        let adapter = GatedChange {
            scheduler: &self.scheduler,
            invocation: &self.invocation,
            op: Mutex::new(Some(op)),
        };
        let scope = TransactionScope {
            conversation_id: Some(self.invocation.conversation_id),
            task_id: Some(self.invocation.task_id),
        };
        self.scheduler
            .deps
            .session
            .commit_transaction(&adapter, context, Some(scope), options)
            .await
    }

    fn report_task(&self) -> ReportTask {
        report_task_of(
            self.invocation.task_id,
            &self.invocation.kind,
            self.invocation.conversation_id,
        )
    }

    fn report_task_with_extension(&self) -> (ReportTask, Option<String>) {
        (self.report_task(), self.invocation.extension.clone())
    }
}

/// `#gated` 的变更种类。
enum GatedOp<'a> {
    /// `runtime.commit(change, ...)`。
    Commit(&'a dyn TaskRuntimeChange),
    /// `runtime.memo(name, candidate, ...)` 的写入。
    MemoPut { name: String, candidate: JsonValue },
    /// `runtime.sleepUntil(until, checkpoint, ...)`。
    Sleep { until: i64, checkpoint: JsonValue },
}

/// `#gated` 的提交回调：重读任务、门控调用，然后应用变更。
///
/// 门控顺序：invocation 已结束 → `EndedError`；Harness 关闭 → 关闭错误；live 里找不到 →
/// “is terminal”；记录不是 running → 状态名；run 模式遇到 abort 标记 → “durable abort
/// mark”。全部通过后才应用变更。`MemoPut` 是首次写入者赢：同名 memo 已存在时直接返回旧值。
struct GatedChange<'a> {
    scheduler: &'a TaskScheduler,
    invocation: &'a Arc<Invocation>,
    op: Mutex<Option<GatedOp<'a>>>,
}

#[async_trait]
impl TransactionChange for GatedChange<'_> {
    async fn run(&self, tx: &Transaction) -> Result<Box<dyn Any + Send>, BoxError> {
        let id = self.invocation.task_id;
        if self.invocation.is_ended() {
            return Err(ended_error(self.invocation));
        }
        if self.scheduler.closing() {
            return Err(closed_error(&self.scheduler.deps.session));
        }
        let found = { self.scheduler.state().live.get(&id).cloned() };
        let Some(current) = found else {
            return Err(io_error(format!("Task {id} is terminal")));
        };
        if current.state.status() != TaskStatus::Running {
            return Err(io_error(format!(
                "Task {id} is {}",
                status_str(current.state.status())
            )));
        }
        if self.invocation.mode == InvocationMode::Run && current.abort_requested {
            return Err(io_error(format!("Task {id} has a durable abort mark")));
        }
        let op = lock(&self.op)
            .take()
            .ok_or_else(|| io_error("the runtime change already ran"))?;
        match op {
            GatedOp::Commit(change) => {
                let tx_dyn: &dyn Tx = tx;
                let next = change.run(tx_dyn, &current).await?;
                let mut checkpoint: Option<String> = None;
                if let Some(next) = next {
                    // 只有 running 提交改变了 checkpoint 的 phase 才记事件；同一 phase 的其它字段变化不算
                    // （TS: `next?.status === "running" && current.state.checkpoint.phase !== next.checkpoint.phase`）。
                    if let TaskState::Running {
                        checkpoint: next_checkpoint,
                    } = &next
                    {
                        let current_phase = current
                            .state
                            .checkpoint()
                            .map(checkpoint_phase)
                            .unwrap_or_else(|| "undefined".to_owned());
                        let next_phase = checkpoint_phase(next_checkpoint);
                        if current_phase != next_phase {
                            checkpoint = Some(next_phase.to_owned());
                        }
                    }
                    self.scheduler
                        .commit_state(tx, self.invocation, &current, next)
                        .await?;
                }
                Ok(Box::new(checkpoint))
            }
            GatedOp::MemoPut { name, candidate } => {
                if let Some(winner) = memo_of(Some(&current), &name) {
                    return Ok(Box::new(winner));
                }
                let mut next = current.clone();
                let mut memos = current.memos.clone().unwrap_or_default();
                memos.insert(name, candidate.clone());
                next.memos = Some(memos);
                tx.set_task(next)?;
                Ok(Box::new(candidate))
            }
            GatedOp::Sleep { until, checkpoint } => {
                self.scheduler
                    .commit_state(
                        tx,
                        self.invocation,
                        &current,
                        TaskState::Sleeping { checkpoint, until },
                    )
                    .await?;
                Ok(Box::new(()))
            }
        }
    }
}

#[async_trait]
impl<H> TaskRuntime<JsonValue, JsonValue, JsonValue, H> for SchedulerRuntime
where
    H: Send + Sync + 'static,
{
    fn task_id(&self) -> TaskId {
        self.invocation.task_id
    }

    fn conversation_id(&self) -> ConversationId {
        self.invocation.conversation_id
    }

    fn signal(&self) -> AbortSignal {
        self.invocation.controller.clone()
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        self.phase.snapshot()
    }

    async fn agent(&self, context: &dyn Context) -> Result<Arc<Agent>, BoxError> {
        self.ensure_live()?;
        let shared = self.shared_agent();
        match await_with_context(shared, context).await {
            Ok(result) => result.map_err(|error| clone_error(&error)),
            Err(abort) => Err(Box::new(abort)),
        }
    }

    fn settings(&self) -> Result<Arc<Settings>, BoxError> {
        // TS: `get settings() { return settings(); }`——宿主抛错直接传进 phase。
        (self.scheduler.deps.settings)()
    }

    fn models(&self) -> Arc<dyn Models> {
        (self.scheduler.deps.models)()
    }

    async fn env(&self, context: &dyn Context) -> Result<Option<Arc<dyn ExecutionEnv>>, BoxError> {
        self.ensure_live()?;
        (self.scheduler.deps.env)(self.invocation.conversation_id, context_handle(context)).await
    }

    fn hooks(&self) -> Arc<dyn HookRunner> {
        Arc::new(SchedulerHooks {
            runtime: self.clone(),
        })
    }

    async fn commit(
        &self,
        change: &dyn TaskRuntimeChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<(), BoxError> {
        let value = self
            .gated(GatedOp::Commit(change), context, options)
            .await?;
        let checkpoint = value
            .downcast::<Option<String>>()
            .map(|value| *value)
            .unwrap_or(None);
        if let Some(phase) = checkpoint {
            let mut attributes = SpanAttributes::new();
            attributes.insert("phase".to_owned(), AttributeValue::String(phase));
            self.invocation
                .context()
                .telemetry()
                .add_event("kiana.durable.checkpoint", Some(attributes));
        }
        Ok(())
    }

    async fn memo(
        &self,
        name: &str,
        _context: &dyn Context,
    ) -> Result<Option<JsonValue>, BoxError> {
        self.ensure_live()?;
        let record = {
            self.scheduler
                .state()
                .live
                .get(&self.invocation.task_id)
                .cloned()
        };
        Ok(memo_of(record.as_ref(), name))
    }

    async fn memo_put(
        &self,
        name: &str,
        candidate: JsonValue,
        context: &dyn Context,
    ) -> Result<JsonValue, BoxError> {
        let value = self
            .gated(
                GatedOp::MemoPut {
                    name: name.to_owned(),
                    candidate,
                },
                context,
                CommitOptions::default(),
            )
            .await?;
        value
            .downcast::<JsonValue>()
            .map(|value| *value)
            .map_err(|_| io_error("the runtime memo commit returned an unexpected value"))
    }

    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.ensure_live()?;
        let storage = Arc::clone(&self.scheduler.deps.storage);
        let context = context_handle(context);
        self.scheduler
            .deps
            .session
            .read_on_line(async move { storage.task(id, &*context).await })
            .await
    }

    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError> {
        self.ensure_live()?;
        let bound = with_abort_signal(self.invocation.controller.clone(), context_handle(context));
        self.scheduler.wait_for_task(id, &*bound).await
    }

    async fn abort_owned(&self, id: TaskId, context: &dyn Context) -> Result<(), BoxError> {
        self.ensure_live()?;
        let bound = with_abort_signal(self.invocation.controller.clone(), context_handle(context));
        let storage = Arc::clone(&self.scheduler.deps.storage);
        let read_bound = Arc::clone(&bound);
        let record = self
            .scheduler
            .deps
            .session
            .read_on_line(async move { storage.task(id, &*read_bound).await })
            .await?;
        if record.as_ref().map(|record| record.owner) != Some(Some(self.invocation.task_id)) {
            return Err(io_error(format!(
                "Task {id} is not owned by task {}",
                self.invocation.task_id
            )));
        }
        if record.is_some_and(|record| record.state.status() == TaskStatus::Terminal) {
            return Ok(());
        }
        self.scheduler.abort(id, &*bound, true).await?;
        self.scheduler.wait_for_task(id, &*bound).await?;
        Ok(())
    }

    async fn owned_tasks(&self, context: &dyn Context) -> Result<Vec<TaskRecord>, BoxError> {
        self.ensure_live()?;
        let scheduler = self.scheduler.clone();
        let invocation = Arc::clone(&self.invocation);
        let context = context_handle(context);
        self.scheduler
            .deps
            .session
            .read_on_line(async move {
                // 结束调用的 step 可能已经先跑在线上。
                if invocation.is_ended() {
                    return Err(ended_error(&invocation));
                }
                if let Some(signal) = context.abort_signal()
                    && signal.is_aborted()
                {
                    return Err(Box::new(abort_error(&signal)));
                }
                // 每个活任务都列在它的父节点下，有 owner 任务时为那个任务。
                let mut owned: Vec<AnyTaskRecord> = Vec::new();
                {
                    let state = scheduler.state();
                    if let Some(children) = state.below.get(&Node::Task(invocation.task_id)) {
                        for node in children {
                            if let Node::Task(id) = node
                                && let Some(record) = state.live.get(id)
                                && record.owner == Some(invocation.task_id)
                            {
                                owned.push(record.clone());
                            }
                        }
                    }
                }
                // TS 测试锁定的顺序：按 ID 升序。`#below` 是插入序，不保证与 ID 一致。
                owned.sort_by_key(|record| record.id);
                // 与 storage 读出的记录一样是副本：调度器读自己那份（TS: `copyJson`）。
                let mut copied: Vec<AnyTaskRecord> = Vec::with_capacity(owned.len());
                for record in owned {
                    let value = serde_json::to_value(&record)
                        .map_err(|error| io_error(error.to_string()))?;
                    let value = copy_json(&value)?;
                    copied.push(
                        serde_json::from_value(value)
                            .map_err(|error| io_error(error.to_string()))?,
                    );
                }
                Ok(copied)
            })
            .await
    }

    async fn outcomes(
        &self,
        ids: &[TaskId],
        context: &dyn Context,
    ) -> Result<Vec<TaskOutcome>, BoxError> {
        self.ensure_live()?;
        let storage = Arc::clone(&self.scheduler.deps.storage);
        let context = context_handle(context);
        let ids = ids.to_vec();
        self.scheduler
            .deps
            .session
            .read_on_line(async move {
                // 按传入顺序返回；任何一个不存在或不是终态，整个调用失败（不做部分返回）。
                let mut outcomes: Vec<TaskOutcome> = Vec::new();
                for id in ids {
                    let state = storage
                        .task(id, &*context)
                        .await?
                        .map(|record| record.state);
                    match state {
                        Some(TaskState::Terminal { outcome }) => outcomes.push(outcome),
                        _ => return Err(io_error(format!("Task {id} is not terminal"))),
                    }
                }
                Ok(outcomes)
            })
            .await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, BoxError> {
        self.ensure_live()?;
        let invocation = Arc::clone(&self.invocation);
        let check_invocation = Arc::clone(&invocation);
        let check: Arc<dyn Fn() -> Result<(), BoxError> + Send + Sync> = Arc::new(move || {
            if check_invocation.is_ended() {
                Err(ended_error(&check_invocation))
            } else {
                Ok(())
            }
        });
        (self.scheduler.deps.conversation)(
            id,
            InvocationBinding {
                signal: invocation.controller.clone(),
                check,
            },
            context_handle(context),
        )
        .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.ensure_live()?;
        let storage = Arc::clone(&self.scheduler.deps.storage);
        let conversation_id = self.invocation.conversation_id;
        let context = context_handle(context);
        // 只读本会话祖先范围内可见的条目（TS: `this.#storage.entry(invocation.conversationId, id, context)`）。
        let found = self
            .scheduler
            .deps
            .session
            .read_on_line(
                async move { storage.visible_entry(conversation_id, id, &*context).await },
            )
            .await?;
        Ok(found.map(|stored| stored.entry))
    }

    async fn context(
        &self,
        conversation_id: ConversationId,
        context: &dyn Context,
        options: ContextViewOptions,
    ) -> Result<ContextView, BoxError> {
        self.ensure_live()?;
        let previous = {
            let state = self.scheduler.state();
            state
                .contexts
                .get(&conversation_id)
                .map(|kept| kept.range.clone())
        };
        let (view, range) = read_context_from(
            &self.scheduler.deps.session,
            &*self.scheduler.deps.storage,
            conversation_id,
            context,
            options.at,
            previous.as_ref(),
        )
        .await?;
        if let Some(range) = range {
            // 调用结束或并发读取已经保留了更新的范围时丢弃。
            let kept = {
                let state = self.scheduler.state();
                state
                    .contexts
                    .get(&conversation_id)
                    .map(|kept| (kept.range.bounds.tail, kept.idle_since))
            };
            let accept = !self.scheduler.closing()
                && !self.invocation.is_ended()
                && kept
                    .as_ref()
                    .is_none_or(|(tail, _)| *tail <= range.bounds.tail);
            if accept {
                // 读取另一个空闲会话会开始或继续它的保留期。
                if !self.scheduler.idle(Some(conversation_id)) {
                    let mut state = self.scheduler.state();
                    state.contexts.insert(
                        conversation_id,
                        KeptContext {
                            range,
                            idle_since: None,
                        },
                    );
                } else if self.scheduler.context_retention_ms() > 0.0 {
                    let idle_since = kept
                        .and_then(|(_, idle_since)| idle_since)
                        .unwrap_or_else(|| self.scheduler.now());
                    {
                        let mut state = self.scheduler.state();
                        state.contexts.insert(
                            conversation_id,
                            KeptContext {
                                range,
                                idle_since: Some(idle_since),
                            },
                        );
                    }
                    self.scheduler.schedule_expiry();
                }
            }
        }
        Ok(view)
    }

    fn now(&self) -> i64 {
        // TS 的 `now: () => { if (invocation.ended) throw endedError(invocation); ... }`。
        // 该 trait 方法没有错误通道，因此用 panic 表达 throw（调用方以 `catch_unwind` 还原，
        // 与 TS 的 try/catch 位置一致）。
        if self.invocation.is_ended() {
            panic!(
                "{}",
                EndedError {
                    task: self.invocation.task_id
                }
            );
        }
        self.scheduler.now()
    }

    fn set_timer(
        &self,
        callback: crate::timers::TimerCallback,
        delay_ms: f64,
    ) -> crate::timers::Disarm {
        // 已结束调用的计时器永不触发。
        if self.invocation.is_ended() {
            return Box::new(|| {});
        }
        let marker = Arc::new(());
        let invocation = Arc::clone(&self.invocation);
        let callback_marker = Arc::clone(&marker);
        let callback_invocation = Arc::clone(&invocation);
        let disarm = self.scheduler.deps.timers.foreground(
            Arc::new(move || {
                {
                    let mut timers = lock(&callback_invocation.timers);
                    timers.retain(|(candidate, _)| !Arc::ptr_eq(candidate, &callback_marker));
                }
                if !callback_invocation.is_ended() {
                    callback();
                }
            }),
            delay_ms,
        );
        let shared = SharedDisarm::new(disarm);
        lock(&invocation.timers).push((Arc::clone(&marker), shared));
        let return_marker = Arc::clone(&marker);
        Box::new(move || {
            let shared = {
                let mut timers = lock(&invocation.timers);
                timers
                    .iter()
                    .position(|(candidate, _)| Arc::ptr_eq(candidate, &return_marker))
                    .map(|position| timers.remove(position).1)
            };
            if let Some(shared) = shared {
                shared.disarm();
            }
        })
    }

    fn report(&self, error: &(dyn std::error::Error + Send + Sync), from: Option<ReportFrom>) {
        if self.invocation.is_ended() {
            // TS 在此抛 `Task N invocation has ended`；没有错误通道的方法用 panic 表达 throw。
            panic!(
                "{}",
                EndedError {
                    task: self.invocation.task_id
                }
            );
        }
        let (task, extension) = self.report_task_with_extension();
        let info = match from {
            None => ReportInfo::TaskError { task, extension },
            Some(ReportFrom::Extension { extension }) => ReportInfo::ExtensionError {
                extension,
                task: Some(task),
                // 任务内的扩展错误不带 conversationId（TS: `{ type: "extension_error", task, ...optionalExtension }`）。
                conversation_id: None,
            },
            Some(ReportFrom::Host { callback }) => ReportInfo::HostError {
                callback,
                task: Some(task),
            },
        };
        self.scheduler
            .report(error, info, Some(&*self.invocation.context()));
    }

    async fn sleep_until(
        &self,
        until: i64,
        checkpoint: JsonValue,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.gated(
            GatedOp::Sleep { until, checkpoint },
            context,
            CommitOptions::default(),
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl DocumentReader for SchedulerRuntime {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.ensure_live()?;
        self.scheduler.deps.session.snapshot(target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.ensure_live()?;
        self.scheduler
            .deps
            .session
            .snapshot_as_of(target, at, context)
            .await
    }
}

#[async_trait]
impl DocumentObserver for SchedulerRuntime {
    async fn watch_doc(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<DocumentWatch>, BoxError> {
        self.ensure_live()?;
        let watch = self
            .scheduler
            .deps
            .session
            .watch_doc(target, context)
            .await?;
        let Some(watch) = watch else {
            return Ok(None);
        };
        if self.invocation.is_ended() {
            tokio::spawn(async move {
                let _ = watch.stop().await;
            });
            return Err(ended_error(&self.invocation));
        }
        lock(&self.invocation.watches).push(Arc::clone(&watch));
        // watch 终止时移除（TS: `watch.closed.then(() => watches.delete(watch))`）。
        let invocation = Arc::clone(&self.invocation);
        let closing = Arc::clone(&watch);
        tokio::spawn(async move {
            let _ = closing.closed().await;
            let mut watches = lock(&invocation.watches);
            watches.retain(|candidate| !Arc::ptr_eq(candidate, &closing));
        });
        Ok(Some(watch))
    }
}

/// `runtime.hooks()`：本阶段所选扩展的 hook 处理器表。
///
/// 每次 `hooks()` 新建一个；处理器表按 `agent_hooks()` 给出的扩展顺序返回。
/// `handlers_with_extension()` 保留扩展名，调用方在处理器抛错时用它生成 `extension_error`。
struct SchedulerHooks {
    runtime: SchedulerRuntime,
}

impl HookRunner for SchedulerHooks {
    fn names(&self) -> Vec<String> {
        let mut names: IndexSet<String> = IndexSet::new();
        for (_extension, table) in self.runtime.hook_tables() {
            collect_handler_names(&*table, &mut names);
        }
        names.into_iter().collect()
    }

    fn handlers(&self, name: &str) -> Vec<Arc<dyn Any + Send + Sync>> {
        self.handlers_with_extension(name)
            .into_iter()
            .map(|(_extension, handler)| handler)
            .collect()
    }

    fn handlers_with_extension(
        &self,
        name: &str,
    ) -> Vec<(Option<String>, Arc<dyn Any + Send + Sync>)> {
        let mut handlers = Vec::new();
        for (extension, table) in self.runtime.hook_tables() {
            match handler_from_table(&*table, name) {
                Some(handler) => handlers.push((extension, handler)),
                // 无法解释为 map 的处理器表原样交给调用方（任务代码知道自己的 `H`）；
                // TS 里这是 `handlers[name]`，Rust 的类型擦除无法通用地做字段索引。
                None if !is_handler_map(&*table) => handlers.push((extension, table)),
                None => {}
            }
        }
        handlers
    }
}

fn collect_handler_names(table: &(dyn Any + Send + Sync), names: &mut IndexSet<String>) {
    if let Some(map) = table.downcast_ref::<IndexMap<String, Arc<dyn Any + Send + Sync>>>() {
        for key in map.keys() {
            names.insert(key.clone());
        }
    } else if let Some(map) =
        table.downcast_ref::<std::collections::HashMap<String, Arc<dyn Any + Send + Sync>>>()
    {
        for key in map.keys() {
            names.insert(key.clone());
        }
    }
}

fn is_handler_map(table: &(dyn Any + Send + Sync)) -> bool {
    table
        .downcast_ref::<IndexMap<String, Arc<dyn Any + Send + Sync>>>()
        .is_some()
        || table
            .downcast_ref::<std::collections::HashMap<String, Arc<dyn Any + Send + Sync>>>()
            .is_some()
}

fn handler_from_table(
    table: &(dyn Any + Send + Sync),
    name: &str,
) -> Option<Arc<dyn Any + Send + Sync>> {
    if let Some(map) = table.downcast_ref::<IndexMap<String, Arc<dyn Any + Send + Sync>>>() {
        return map.get(name).cloned();
    }
    if let Some(map) =
        table.downcast_ref::<std::collections::HashMap<String, Arc<dyn Any + Send + Sync>>>()
    {
        return map.get(name).cloned();
    }
    None
}
