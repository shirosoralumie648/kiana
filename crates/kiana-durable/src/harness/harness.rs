//! `harness/harness.ts`：`Harness.open`、`HarnessImpl`、`ConversationImpl`、`safeReport`、
//! `safeNow`、`boundConversation`。
//!
//! TS: `packages/durable/src/harness/harness.ts`。Session 内核（`SessionImpl`）以字段持有而非
//! 继承：`HarnessImpl` 实现 `Session`/`DocumentReader`/`DocumentObserver` 并全部转发；Session 的
//! 宿主钩子（`report`、`conversationCreated`、`beforeClose`、`writeAtClose`）由一个
//! [`crate::session::SessionHooks`] 实现承载。
//!
//! # 这个文件解决什么问题
//! 普通 Session 只负责“文档 + 条目 + 提交”的持久化；本文件把它组装成一个能运行 agent 任务的
//! Harness：构造时创建 `TaskScheduler`（任务调度）、`Submissions`（提交接纳）、
//! `TaskGraphView`（任务图）与 `ConversationViews`（会话视图），并把 Session 的宿主钩子接到
//! 调度器上。对外它提供两个门面：`Harness::open`（打开）与 `HarnessImpl`（`Harness` trait、
//! `Conversation` 句柄与 Session 转发）。
//!
//! # 在系统中的位置
//! - 上游：`crate::index` 重导出 `Harness` 与 `DEFAULT_HARNESS_IDENTITY`；examples/tests 通过
//!   `Harness::open` 打开，经 `root()` / `conversation()` 拿到 `Conversation` 句柄。
//! - 下游：`harness::scheduler` / `submissions` / `task_graph` / `view` / `events` 都通过本文件
//!   注入的回调访问 Session 与 Storage；`ConversationImpl` 把 `Conversation` trait 的每个操作
//!   转发给其中之一。
//!
//! # 打开与关闭的执行顺序
//! 1. `Harness::open` 先检查调用方上下文是否已取消，再用**一次** registry 快照校验三个内建
//!    任务（`kiana.generation` / `kiana.tool` / `kiana.compaction`）都在。
//! 2. `build_harness` 构造 `SessionImpl`（注入每次求值的 settings 来源与 `HarnessHooks`），再
//!    依次构造调度器、提交管理器、任务图与会话视图；构造期间只注册监听，不读写 Storage。
//! 3. `open_tasks` 在一条提交里调和幸存任务：`running` 改回 `pending` 并累加 restart 计数，
//!    达到上限（restarts >= restart limit，且没有 abort 标记）的停泊；它不派发任务，
//!    `resume()` 之后调度器才真正开始推进。
//! 4. 关闭由 `SessionImpl` 驱动：先 `HarnessHooks::before_close`（中止并等待调度器的调用），
//!    再 `HarnessHooks::write_at_close`（把调度器排出的任务状态批次交给 Session 直写），最后
//!    关 Storage。
//!
//! # 对象图与生命周期
//! 调度器的三个 resolver（agent/env/conversation）需要反向指回 Harness，直接强引用会成环。
//! `build_harness` 用 `weak_slot`（`Arc<Mutex<Option<Weak<HarnessImpl>>>>`）+
//! `Arc::new_cyclic` 打破：回调升级失败时返回 `closed_error(&session)`——Session 有失败闩锁时
//! 是 `SessionFailed`，否则 `HarnessClosed`。`TaskGraphView` 与 `ConversationViews` 不在这个
//! 环里：它们构造时只接收 `SessionImpl`、`Storage`、report 闭包（任务图另有调度器提供的
//! blocked 判定与通知闭包），不持有 `HarnessImpl`。
//! 反方向的 `ConversationImpl` 用 `_keepalive: Arc<HarnessImpl>` 强引用 Harness（复刻 TS 句柄
//! 持有 Harness 的语义）；Harness 不持有 `ConversationImpl`，因此不成环。
//!
//! # 重要约束
//! - 需要一致读的操作（`inspect`、`get_task`、`conversation` 的查找等）都经
//!   `SessionImpl::read_on_line` 排队，与提交在同一条变更线上串行；`snapshot` 走 Session 的
//!   文档缓存与加载路径。
//! - 宿主回调（`settings`、`now`、`on_report`、`setTimer` 等）是外部代码，可能 panic。本文件
//!   在 safe 包装器以及需要对齐 TS “throw” 的位置用 `catch_unwind` 转换；未包装的宿主回调
//!   （如 `conversation_created`、`env` 构建器）返回的错误按调用点原样传播。
//! - 关闭或失败后的调用统一被拒绝：Session 有失败闩锁时返回带 cause 的 `SessionFailed`；
//!   否则按入口区分——经 `SessionImpl::assert_usable` 的路径返回 `SessionClosed`，经
//!   `harness::util::closed_error` 的路径（调度器的 resolver、`wait_for_idle` 等）返回
//!   `HarnessClosed`。

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, Weak};

use async_trait::async_trait;
use kiana_ai::models::Models;
use kiana_ai::types::ClientIdentity;
use kiana_ai::utils::identity::{KIANA_ATTRIBUTION, merge_client_identity, with_client_identity};
use kiana_chord::context::{Context, ContextRef, with_abort_signal, without_abort_signal};
use kiana_telemetry::metrics::create_typed_metrics;
use kiana_telemetry::noop::{noop_telemetry_context_as_context, noop_telemetry_metrics};
use kiana_telemetry::{AttributeValue, SpanAttributes, TelemetryContext};

use crate::env::ExecutionEnv;
use crate::harness::agent::{AGENT_DOC, configure, create_agent, resolve_agent, resolve_settings};
use crate::harness::compaction::{CompactionInput, create_compaction};
use crate::harness::context::read_context;
use crate::harness::events::AgentEventStream;
use crate::harness::inbox::{INBOX_DOC, QueueModes, withdraw_queued_inputs};
use crate::harness::live::{LIVE_DOC, settle_scheduler_outcome};
use crate::harness::provider::PROVIDER_DOC;
use crate::harness::scheduler::{
    AgentResolver, ConversationResolver, EnvResolver, InvocationBinding, ReportFn,
    SettingsResolver, SettleOutcome, TaskScheduler, TaskSchedulerOptions, WithdrawInputs,
    WriteAtClose,
};
use crate::harness::submissions::{AbortResult, Submissions};
use crate::harness::task_graph::{TaskGraphBlocking, TaskGraphView};
use crate::harness::telemetry::DURABLE_TELEMETRY_SCHEMA;
use crate::harness::types::{
    AbortSubmissionOutcome, AbortTaskOutcome, Agent, AgentChange, AgentState, CompactionReason,
    CompactionResult, ContextView, ContextViewOptions, Conversation as ConversationTrait,
    ConversationAbortOptions, ConversationCreateOptions, ConversationHandle, ConversationInit,
    ConversationWatch, EntryRangeQuery, Harness as HarnessTrait, HarnessInspection, HarnessOptions,
    HarnessSettings, ModelsProvider, RootOptions, SettingsProvider, SettledSubmissionRecord,
    SettledTask, Submission, SubmissionAbortOutcome, SubmissionDraft, SubmissionDraftKind,
    TaskGraphWatch, Wake,
};
use crate::harness::usage::{USAGE_DOC, UsageState, add_usage_state};
use crate::harness::util::{HarnessClosed, closed_error, scan_all};
use crate::harness::view::{ConversationViews, register_views};
use crate::session::session::{ContextSource, SessionHooks, TransactionChange};
use crate::session::transaction::{Transaction, TransactionScope};
use crate::session::{ReportedError, SessionClosed, SessionImpl, SessionOptions};
use crate::types::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, ConversationRecord,
    CreateConversationOptions, Cursor, DocTarget, DocumentObserver, DocumentReader, DocumentWatch,
    EntryDraft, EntryHead, EntryId, EntryRecord, JsonObject, JsonValue, Page, ROOT_CONVERSATION_ID,
    ReportInfo, ReportTask, SessionChange, SessionEnd, SessionFailed, Storage, StorageWrite,
    SubmissionId, SubmissionQuery, SubmissionRecord, SubmissionStatus, TaskId, TaskRecord, Tx,
};

/// 一轮扫描的页大小（TS: `SCAN_PAGE_SIZE`）。
const SCAN_PAGE_SIZE: usize = 256;

/// Harness 的模型请求身份：kiana-ai 的默认值加上指向 kiana 的 provider 归属。
///
/// TS: `DEFAULT_HARNESS_IDENTITY: ClientIdentity = { attribution: KIANA_ATTRIBUTION }`
pub static DEFAULT_HARNESS_IDENTITY: LazyLock<ClientIdentity> = LazyLock::new(|| ClientIdentity {
    attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
    user_agent: None,
    version: None,
});

/// 非致命失败报告回调（TS: `#report`）。
pub type HarnessReport = Arc<
    dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo, Option<&dyn Context>) + Send + Sync,
>;

/// `Harness` 的值面（TS 的 `export const Harness = { open }`）。
pub struct Harness;

impl Harness {
    /// 在一个 Storage 上打开 Harness。registry 可以在 Harness 运行期间继续变化。
    ///
    /// # 参数
    /// - `storage`：底层持久化后端，交给 `SessionImpl` 与调度器/视图共享。
    /// - `options`：模型、registry、设置、时钟、计时器与各类宿主回调。只取一次的（如 `now`
    ///   回调本身）与每次访问重新求值的（`models`、`settings`）在 `build_harness` 里分别处理。
    /// - `context`：打开期间的取消与 Storage 读取上下文；已取消时不做任何工作，直接返回
    ///   `abort_error` 对应的错误。
    ///
    /// # 执行顺序
    /// 1. 用 registry 的快照检查 `BUILTIN_TASKS` 里的三个内建任务；缺失时返回带缺失名单的
    ///    [`ReportedError`]（名单顺序即 `BUILTIN_TASKS` 的顺序，registry.rs 有说明）。
    /// 2. `build_harness`：构造 Session、调度器、提交管理器与视图（只接线，没有 Storage IO）。
    /// 3. `open_tasks`：载入活任务并把幸存的 `running` 任务调和成 `pending`。
    ///
    /// # 失败模式
    /// 第 3 步失败时，调用方的 `context` 本身可能就是失败原因，因此用去掉 abort signal 的
    /// 上下文关闭已建好的 Session，再把原错误返回。第 2 步的构造错误（订阅/视图初始化）直接
    /// 返回。打开本身不启用调度：要运行任务需调用 `resume()`（或由 `submit()` 隐式触发）。
    pub async fn open(
        storage: Arc<dyn Storage>,
        options: HarnessOptions,
        context: &dyn Context,
    ) -> Result<Arc<HarnessImpl>, BoxError> {
        if let Some(signal) = context.abort_signal()
            && signal.is_aborted()
        {
            return Err(Box::new(kiana_chord::context::abort_error(&signal)));
        }
        let snapshot = options.registry.snapshot();
        let missing: Vec<String> = crate::harness::registry::BUILTIN_TASKS
            .iter()
            .filter(|task| snapshot.task(&task.definition.name).is_none())
            .map(|task| task.definition.name.clone())
            .collect();
        if !missing.is_empty() {
            return Err(Box::new(ReportedError(format!(
                "Registry lacks built-in tasks {}; create it with createRegistry()",
                missing.join(", ")
            ))));
        }
        let harness = build_harness(storage, options, context)?;
        if let Err(error) = harness.open_tasks(context).await {
            // 调用方的 context 可能就是 open 失败的原因：不带它关闭，然后重抛 open 的错误。
            harness
                .session
                .close(without_abort_signal(context.context_ref()))
                .await;
            return Err(error);
        }
        Ok(harness)
    }
}

// ---------------------------------------------------------------------------
// HarnessImpl
// ---------------------------------------------------------------------------

/// Harness 私有服务：Conversation 句柄通过它回到 Harness（TS: `ConversationHost`）。
struct ConversationHost {
    harness: Arc<Mutex<Option<Weak<HarnessImpl>>>>,
}

impl ConversationHost {
    fn upgrade(&self) -> Result<Arc<HarnessImpl>, BoxError> {
        let weak = self
            .harness
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        match weak.and_then(|weak| weak.upgrade()) {
            Some(harness) => Ok(harness),
            None => Err(Box::new(HarnessClosed)),
        }
    }
}

/// Session 的宿主钩子状态：`beforeClose()` 与 `writeAtClose()` 在 Session 构造之后才用到
/// 调度器，因此用 `OnceLock` 打破构造顺序。
#[derive(Default)]
struct HookState {
    tasks: OnceLock<TaskScheduler>,
    writes: Mutex<Vec<StorageWrite>>,
}

/// Session 侧钩子的 Harness 实现：把错误报告、会话创建、关闭前 join 与关闭时直写接到
/// 调度器或宿主回调上。`state` 与 `build_harness` 共享，因为钩子必须先于调度器构造。
struct HarnessHooks {
    state: Arc<HookState>,
    report: HarnessReport,
    conversation_created: Option<crate::harness::types::ConversationCreatedHandler>,
}

#[async_trait]
impl SessionHooks for HarnessHooks {
    // Session 只在钩子上传错误文本，因此这里把消息重新包成 `ReportedError`；原始错误类型在
    // 会话边界已经丢失，宿主 `on_report` 只能依赖文本与 `ReportInfo`。
    fn report(&self, message: &str, info: ReportInfo, context: Option<&dyn Context>) {
        (self.report)(&ReportedError(message.to_owned()), &info, context);
    }

    async fn conversation_created(
        &self,
        tx: &Transaction,
        record: &ConversationRecord,
    ) -> Result<(), BoxError> {
        // 四个内建文档，然后 `kiana.agent`，最后宿主钩子。整个顺序在同一个创建事务里执行，
        // 任一步失败都会让这次创建失败（会话记录不会落盘）；宿主钩子放在最后，因此它能看到
        // 完整的初始文档，也可以覆盖 `create_agent` 复制/物化出来的 agent 值。
        for token in [&LIVE_DOC, &INBOX_DOC, &USAGE_DOC, &PROVIDER_DOC] {
            tx.doc(&DocTarget::conversation(token, record.id)).await?;
        }
        create_agent(tx, record).await?;
        if let Some(handler) = &self.conversation_created {
            handler(tx, record).await?;
        }
        Ok(())
    }

    async fn before_close(&self) -> Result<(), BoxError> {
        // 关闭监听器之后、Storage 关闭之前；不写 task outcome。
        // `join()` 会中止所有在跑的调用并等它们退出，再把仍为 running 的任务以 pending 状态
        // 经 `write_at_close` 回调交给 `HookState.writes`（见调度器的 `TaskScheduler::join`）。
        // 本钩子只等待；Session 会忽略这里的返回值，因此没有可上报错误的位置。
        if let Some(tasks) = self.state.tasks.get() {
            tasks.join().await;
        }
        Ok(())
    }

    fn write_at_close(&self) -> Vec<StorageWrite> {
        // 调度器在 `join()` 里通过 `WriteAtClose` 回调把批次追加到 `state.writes`；Session 在
        // `run_close_job` 的第一步调用本钩子。`take` 保证同一批次只会被写一次。
        std::mem::take(&mut *lock(&self.state.writes))
    }
}

impl std::fmt::Debug for HarnessHooks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HarnessHooks")
    }
}

/// 一个 Session 之上的持久 agent Harness（TS: `HarnessImpl`）。
///
/// 组合而非继承：`SessionImpl`、任务调度器、提交管理器、任务图与会话视图都是字段。
/// `HarnessImpl` 自身实现 `Session`/`DocumentReader`/`DocumentObserver`（见文件末尾的转发
/// impl）以及 `Harness` trait，因此既能当普通 Session 用，也能作为 `dyn Harness` 交给
/// events/view 的辅助函数。
///
/// 生命周期：由 [`Harness::open`] 以 `Arc<HarnessImpl>` 返回，调用方按 `Arc` 共享。关闭开始
/// 或 Session 失败后，新的操作会被拒绝：Session 有失败闩锁时两种入口都是 `SessionFailed`；
/// 仅关闭时按入口区分——经 Session 入口（`SessionImpl::assert_usable`，如 `resume`）是
/// `SessionClosed`，经调度器/`harness::util::closed_error` 入口（如 `wait_for_idle`）是
/// `HarnessClosed`。已在变更线上排队的读会先完成。
pub struct HarnessImpl {
    session: SessionImpl,
    options: HarnessOptions,
    host_settings: Arc<dyn Fn() -> Option<HarnessSettings> + Send + Sync>,
    report: HarnessReport,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    tasks: TaskScheduler,
    submissions: Submissions,
    task_graph: TaskGraphView,
    views: Arc<ConversationViews>,
    host: Arc<ConversationHost>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// 已存文档读出来是 IndexMap 形式的 `JsonObject`，而类型化反序列化走 `serde_json::from_value`，
// 需要先转成 `serde_json::Value::Object`（深拷贝一次）。
fn json_object(value: &JsonObject) -> JsonValue {
    JsonValue::Object(value.clone().into_iter().collect())
}

// 把 `catch_unwind` 捕获的 panic 负载翻译成错误：宿主以 BoxError / String / &'static str
// 抛出的内容原样保留，其它负载归为一条通用文本。
fn panic_to_error(payload: Box<dyn Any + Send>) -> BoxError {
    match payload.downcast::<BoxError>() {
        Ok(error) => *error,
        Err(payload) => match payload.downcast::<String>() {
            Ok(message) => Box::new(ReportedError(*message)),
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => Box::new(ReportedError((*message).to_owned())),
                Err(_) => Box::new(ReportedError("host callback panicked".to_owned())),
            },
        },
    }
}

// 系统墙钟的 epoch 毫秒；系统时钟早于 UNIX_EPOCH（配置错误或极端环境）时返回 0。
fn date_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `HarnessOptions.models` 作为函数并应用 `HarnessOptions.identity`：普通 `Models` 只包裹一次，
/// 函数的结果每次调用都包裹。
///
/// 身份合并顺序是 `DEFAULT_HARNESS_IDENTITY` 在前、`options.identity` 在后，后者逐字段覆盖
/// （`merge_client_identity` 的语义；`attribution = Some(None)` 表示显式清空）。包装器注入的
/// 只是默认身份，单次请求的 `StreamOptions.identity` 仍然优先级最高（见 kiana-ai 的
/// `utils/identity.rs`）。`Dynamic` 每次调用才求值，因此宿主可以在运行期换模型。
fn current_models(
    models: ModelsProvider,
    identity: Option<ClientIdentity>,
) -> Arc<dyn Fn() -> Arc<dyn Models> + Send + Sync> {
    let merged = merge_client_identity([Some(DEFAULT_HARNESS_IDENTITY.clone()), identity])
        .unwrap_or_else(|| DEFAULT_HARNESS_IDENTITY.clone());
    match models {
        ModelsProvider::Direct(models) => {
            let wrapped = with_client_identity(models, merged);
            Arc::new(move || Arc::clone(&wrapped))
        }
        ModelsProvider::Dynamic(provider) => {
            Arc::new(move || with_client_identity(provider(), merged.clone()))
        }
    }
}

/// `HarnessOptions.settings` 作为函数：普通设置或没有设置都在每次调用时返回。
///
/// 三种来源都保留“每次访问重新求值”的语义：`None` 恒为 `None`，`Static` 每次克隆一份快照，
/// `Dynamic` 每次调用宿主函数。Session 的保留期与调度器的 restart 上限等都依赖这一点。
fn current_settings(
    settings: Option<SettingsProvider>,
) -> Arc<dyn Fn() -> Option<HarnessSettings> + Send + Sync> {
    match settings {
        None => Arc::new(|| None),
        Some(SettingsProvider::Static(settings)) => Arc::new(move || Some(settings.clone())),
        Some(SettingsProvider::Dynamic(settings)) => Arc::new(move || settings()),
    }
}

// 报告类型到遥测字符串的映射；词汇表由 `DURABLE_TELEMETRY_SCHEMA` 的 schema 测试锁定
// （counter 的 `type` 属性有 9 个允许值）。
fn report_kind(info: &ReportInfo) -> &'static str {
    match info {
        ReportInfo::SessionFailed => "session_failed",
        ReportInfo::TaskFaulted { .. } => "task_faulted",
        ReportInfo::TaskParked { .. } => "task_parked",
        ReportInfo::MigrationFailed { .. } => "migration_failed",
        ReportInfo::TaskOutdated { .. } => "task_outdated",
        ReportInfo::TaskError { .. } => "task_error",
        ReportInfo::ExtensionError { .. } => "extension_error",
        ReportInfo::HostError { .. } => "host_error",
        ReportInfo::ListenerError => "listener_error",
    }
}

fn faulted_reason(reason: crate::types::TaskFaultedReason) -> &'static str {
    match reason {
        crate::types::TaskFaultedReason::Threw => "threw",
        crate::types::TaskFaultedReason::RestartLimit => "restart_limit",
    }
}

fn outdated_reason(reason: crate::types::TaskOutdatedReason) -> &'static str {
    match reason {
        crate::types::TaskOutdatedReason::MissingTask => "missing_task",
        crate::types::TaskOutdatedReason::IncompatibleTask => "incompatible_task",
    }
}

fn put_extension(attributes: &mut SpanAttributes, extension: &Option<String>) {
    if let Some(extension) = extension {
        attributes.insert(
            "extension".to_owned(),
            AttributeValue::String(extension.clone()),
        );
    }
}

fn put_task(attributes: &mut SpanAttributes, task: &ReportTask) {
    attributes.insert(
        "task.id".to_owned(),
        AttributeValue::Number(task.id.get() as f64),
    );
}

fn put_optional_task(attributes: &mut SpanAttributes, task: &Option<ReportTask>) {
    if let Some(task) = task {
        put_task(attributes, task);
    }
}

/// 保留宿主回调的错误容纳；遥测适配器负责自己的失败（TS: `safeReport`）。
///
/// 返回的闭包每次被调用时依次：
/// 1. 用 `catch_unwind` 调用宿主的 `on_report`（它不接收上下文；panic 被吞掉，不影响调用方；
///    tests/harness_reports.rs 的 `safe_report_contains_a_throwing_on_report_and_counts_each_type`
///    锁定了这一点）；
/// 2. 向 `kiana.durable.reports` 计数器加 1，带 `type` 属性；
/// 3. 有调用方上下文时，再向该上下文的 telemetry 发 `kiana.durable.report` 事件。事件属性按
///    `ReportInfo` 变体填充：`task_faulted`/`task_outdated` 额外写 `reason`，带任务的变体写
///    `task.id`，带扩展名的变体写 `extension`。
///
/// `telemetry` 为 `None` 时使用 noop 适配器。计数器由 `DURABLE_TELEMETRY_SCHEMA` 静态定义，
/// 因此 `expect` 是内部不变量，不是宿主可触发的错误。
pub fn safe_report(
    on_report: Option<
        Arc<dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo) + Send + Sync>,
    >,
    telemetry: Option<Arc<dyn TelemetryContext>>,
) -> HarnessReport {
    let telemetry = telemetry.unwrap_or_else(noop_telemetry_context_as_context);
    let backend = telemetry.metrics().unwrap_or_else(noop_telemetry_metrics);
    let definitions = DURABLE_TELEMETRY_SCHEMA.metrics.clone().unwrap_or_default();
    let metrics = create_typed_metrics(definitions, backend);
    let reports = metrics
        .counter("kiana.durable.reports")
        .expect("the durable telemetry schema defines kiana.durable.reports");
    Arc::new(move |error, info, context| {
        // `onReport` 抛错被吞掉。
        if let Some(on_report) = &on_report {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_report(error, info)));
        }
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "type".to_owned(),
            AttributeValue::String(report_kind(info).to_owned()),
        );
        match info {
            ReportInfo::TaskFaulted {
                task,
                extension,
                reason,
            } => {
                attributes.insert(
                    "reason".to_owned(),
                    AttributeValue::String(faulted_reason(*reason).to_owned()),
                );
                put_extension(&mut attributes, extension);
                put_task(&mut attributes, task);
            }
            ReportInfo::TaskOutdated {
                task,
                extension,
                reason,
            } => {
                attributes.insert(
                    "reason".to_owned(),
                    AttributeValue::String(outdated_reason(*reason).to_owned()),
                );
                put_extension(&mut attributes, extension);
                put_task(&mut attributes, task);
            }
            ReportInfo::TaskParked { task, extension }
            | ReportInfo::MigrationFailed { task, extension }
            | ReportInfo::TaskError { task, extension } => {
                put_extension(&mut attributes, extension);
                put_task(&mut attributes, task);
            }
            ReportInfo::ExtensionError {
                extension,
                task,
                conversation_id: _,
            } => {
                put_extension(&mut attributes, extension);
                put_optional_task(&mut attributes, task);
            }
            ReportInfo::HostError { task, .. } => put_optional_task(&mut attributes, task),
            ReportInfo::SessionFailed | ReportInfo::ListenerError => {}
        }
        let mut metric_attributes = SpanAttributes::new();
        metric_attributes.insert(
            "type".to_owned(),
            AttributeValue::String(report_kind(info).to_owned()),
        );
        reports.add(1.0, Some(metric_attributes));
        if let Some(context) = context {
            context
                .telemetry()
                .add_event("kiana.durable.report", Some(attributes));
        }
    })
}

/// 宿主时钟：`now` 抛错时回落到 `Date.now`。第一次抛错被报告；时钟在每次任务变化时都会被读，
/// 之后的抛错不再报告（TS: `safeNow`）。
///
/// panic 负载经 `panic_to_error` 转换后以 `host_error`/`now` 报告。`reported` 在调用报告之前
/// 就置位，因此报告处理器内部再次读时钟不会递归报告。没有宿主时钟时直接返回系统墙钟。
pub fn safe_now(
    now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    report: Arc<dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo) + Send + Sync>,
) -> Arc<dyn Fn() -> i64 + Send + Sync> {
    let Some(now) = now else {
        return Arc::new(date_now);
    };
    let reported = Arc::new(AtomicBool::new(false));
    Arc::new(move || {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| now())) {
            Ok(value) => value,
            Err(payload) => {
                // 先置位：报告处理器里再读时钟也不会二次报告。
                if !reported.swap(true, Ordering::SeqCst) {
                    let error = panic_to_error(payload);
                    report(
                        &*error,
                        &ReportInfo::HostError {
                            callback: crate::types::HostCallback::Now,
                            task: None,
                        },
                    );
                }
                date_now()
            }
        }
    })
}

/// 一次创建的目标（TS: `CreateTarget`）。
enum CreateTarget {
    Root,
    Independent {
        ownership: ConversationOwnership,
    },
    Fork {
        parent_id: ConversationId,
        at: EntryId,
        ownership: ConversationOwnership,
    },
}

/// 创建提交里应用的便利项（TS: `CreateOptions`）。
#[derive(Default)]
struct CreateOptions {
    agent: Option<AgentChange>,
    init: Option<ConversationInit>,
}

fn build_harness(
    storage: Arc<dyn Storage>,
    options: HarnessOptions,
    context: &dyn Context,
) -> Result<Arc<HarnessImpl>, BoxError> {
    // 先把宿主回调包成不会 panic 的形式：报告始终可用，时钟失败时回落到墙钟。注意 `now`
    // 会同时供 Session 的提交时钟使用，所以宿主时钟的 panic 不会让提交失败，只会被报告一次
    // 并退化为 `Date.now()`（TS 的 Harness 构造也是这样包装的）。
    let report = safe_report(options.on_report.clone(), options.telemetry.clone());
    let now = safe_now(options.now.clone(), {
        let report = Arc::clone(&report);
        Arc::new(move |error, info| report(error, info, None))
    });
    let host_settings = current_settings(options.settings.clone());

    let hook_state = Arc::new(HookState::default());
    let hooks = Arc::new(HarnessHooks {
        state: Arc::clone(&hook_state),
        report: Arc::clone(&report),
        conversation_created: options.conversation_created.clone(),
    });

    // Session 的两个“活设置”：文档保留期与延迟提交窗口都在每次使用时重新读宿主设置（字段名见
    // `HarnessSettings`），因此规则可以随运行期设置变化；`read_only` 同时传给 Session 与调度器。
    let settings_clock = Arc::clone(&host_settings);
    let retention_clock = Arc::clone(&host_settings);
    let now_clock = Arc::clone(&now);
    let session = SessionImpl::new(
        storage,
        SessionOptions {
            now: Some(Arc::new(move || now_clock() as f64)),
            document_retention_ms: None,
            document_retention_ms_source: Some(Arc::new(move || {
                settings_clock()
                    .as_ref()
                    .and_then(|settings| settings.document_retention_ms)
            })),
            set_timer: options.set_timer.clone(),
            persist_within_ms: None,
            persist_within_ms_source: Some(Arc::new(move || {
                retention_clock().as_ref().and_then(|settings| {
                    settings
                        .progress
                        .as_ref()
                        .and_then(|progress| progress.persist_within_ms)
                })
            })),
            read_only: options.read_only.unwrap_or(false),
            hooks: Some(hooks),
        },
    );
    // 调度器与视图共享 Session 的 `GuardedStorage`：它在 Session 关闭后拒绝新调用，并把写入
    // 错误与未被豁免的读错误记到 Session 的失败闩锁上；读调用里“调用方已取消”与
    // `StorageRequestError` 例外，只让本次读取失败。直接拿原始 `storage` 会绕过这些约束。
    let guarded: Arc<dyn Storage> = session.storage().clone();

    // 回调反向引用 Harness 的中转槽。此时 Harness 还不存在，`Arc::new_cyclic` 会在对象地址
    // 确定后填入 Weak；在那之前槽里是 `None`。
    let weak_slot: Arc<Mutex<Option<Weak<HarnessImpl>>>> = Arc::new(Mutex::new(None));

    // 每 phase 至多一次的 agent 解析。三个 resolver 都先从 `weak_slot` 升级：Harness 已释放
    // 时用 `closed_error(&session)` 区分“Session 失败”与“只是关闭”。它们各自持有 Session 的
    // 强引用，因此即使 Harness 不在了，失败原因仍然可读。
    let agent_resolver: AgentResolver = {
        let slot = Arc::clone(&weak_slot);
        let session = session.clone();
        Arc::new(move |id, snapshot, call_context| {
            let slot = Arc::clone(&slot);
            let session = session.clone();
            Box::pin(async move {
                let weak = slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .ok_or_else(|| -> BoxError { closed_error(&session) })?;
                let harness = weak
                    .upgrade()
                    .ok_or_else(|| -> BoxError { closed_error(&session) })?;
                harness
                    .resolve_agent(id, Some(snapshot), &*call_context)
                    .await
            })
        })
    };
    // 环境解析：每次调用都重新构建（不缓存），cwd 来自会话当前的 `kiana.agent`；没有配置
    // `HarnessOptions.env` 时返回 `Ok(None)`，任务按无环境运行。
    let env_resolver: EnvResolver = {
        let slot = Arc::clone(&weak_slot);
        let session = session.clone();
        Arc::new(move |id, call_context| {
            let slot = Arc::clone(&slot);
            let session = session.clone();
            Box::pin(async move {
                let weak = slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .ok_or_else(|| -> BoxError { closed_error(&session) })?;
                let harness = weak
                    .upgrade()
                    .ok_or_else(|| -> BoxError { closed_error(&session) })?;
                harness.build_env(id, &*call_context).await
            })
        })
    };
    // 会话解析：先按 id 在变更线上读会话记录，存在才返回 `ConversationHandle`。句柄携带本次
    // 调用的 `InvocationBinding`（abort signal + check），任务结束后对它的操作会被拒绝。
    let conversation_resolver: ConversationResolver = {
        let slot = Arc::clone(&weak_slot);
        let session = session.clone();
        Arc::new(move |id, binding, call_context| {
            let slot = Arc::clone(&slot);
            let session = session.clone();
            Box::pin(async move {
                let weak = slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .ok_or_else(|| -> BoxError { closed_error(&session) })?;
                let harness = weak
                    .upgrade()
                    .ok_or_else(|| -> BoxError { closed_error(&session) })?;
                let storage = harness.session.storage().clone();
                let record = harness
                    .session
                    .read_on_line(async move { storage.conversation(id, &*call_context).await })
                    .await?;
                Ok(record.map(|_| {
                    Arc::new(bound_conversation(
                        id,
                        binding,
                        harness.submissions.clone(),
                        harness.tasks.clone(),
                    )) as Arc<dyn ConversationHandle>
                }))
            })
        })
    };

    // 模型每次访问都取当前值（`current_models`），设置也是：调度器在需要时重新解析 agent 与
    // 设置，宿主因此可以在运行期切换模型或策略。
    let models = current_models(options.models.clone(), options.identity.clone());
    // settings 回调每次求值；panic 转成错误返回。哪些调用点回落默认值（调度器的
    // restartLimit/contextRetentionMs）、哪些让操作失败（`Submissions` 的队列模式）由使用方决定。
    let settings_resolver: SettingsResolver = {
        let host_settings = Arc::clone(&host_settings);
        Arc::new(move || {
            let settings =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host_settings()))
                    .map_err(panic_to_error)?;
            Ok(Arc::new(resolve_settings(settings.as_ref())))
        })
    };
    let report_for_scheduler: ReportFn = Arc::new({
        let report = Arc::clone(&report);
        move |error, info, context| report(error, info, context)
    });
    // 调度器关闭时（`TaskScheduler::join` 末尾）调用这个回调，把仍在 running 的任务应写回的
    // `pending` 记录交出来。此时 Session 还没到 `HarnessHooks::write_at_close`，所以先攒在
    // `HookState.writes`，等 Session 的关闭流程来取。
    let write_at_close: WriteAtClose = {
        let state = Arc::clone(&hook_state);
        Arc::new(move |writes| {
            let batch = writes();
            lock(&state.writes).extend(batch);
            Box::pin(async { Ok(()) })
        })
    };
    // 调度器自己判定 faulted/orphaned 时，由 live 模块清理 `kiana.live`（工具槽、压缩状态、
    // 运行归属）；中止会话时把排队输入落定为 `unanswered`（reason 固定 `aborted`）。
    let settle_outcome: SettleOutcome =
        Arc::new(|tx, record, outcome| Box::pin(settle_scheduler_outcome(tx, record, outcome)));
    let withdraw_inputs: WithdrawInputs = Arc::new(|tx, conversation_id| {
        Box::pin(withdraw_queued_inputs(tx, conversation_id, Some("aborted")))
    });
    // 前台计时器：宿主 `setTimer` 被拒绝或 panic 时，`host_timers` 回落到原生计时器，并只
    // 报告一次 `host_error`/`setTimer`。后台计时器（`background()`）则不同：宿主 panic 时
    // 每次都报告且不回落，宿主拒绝（返回 `None`）时直接返回 `None` 而不报告。Session 与
    // 调度器都会使用 `background()`。
    let timers = Arc::new(crate::timers::host_timers(options.set_timer.clone(), {
        let report = Arc::clone(&report);
        Arc::new(move |error: BoxError| {
            report(
                &*error,
                &ReportInfo::HostError {
                    callback: crate::types::HostCallback::SetTimer,
                    task: None,
                },
                None,
            );
        })
    }));
    // 调度器在这里只完成构造与依赖注入，尚未启用；`open_tasks` 负责调和，之后由调用方的
    // `resume()` 打开调度。调度器保存的是去掉 abort signal 的上下文（TS 同）：打开之后调用方
    // 上下文的取消不会传播到后台任务的提交与调用。
    let tasks = TaskScheduler::new(TaskSchedulerOptions {
        telemetry: options.telemetry.clone(),
        session: session.clone(),
        storage: Arc::clone(&guarded),
        registry: Arc::clone(&options.registry),
        models,
        agent: agent_resolver,
        settings: settings_resolver,
        env: env_resolver,
        now: Arc::clone(&now),
        on_wake: options.on_wake.clone(),
        timers,
        write_at_close,
        report: report_for_scheduler,
        settle_outcome,
        withdraw_inputs,
        conversation: conversation_resolver,
        context: without_abort_signal(context.context_ref()),
        read_only: options.read_only,
    });
    // `beforeClose()`/`writeAtClose()` 从这里开始可以拿到调度器。
    let _ = hook_state.tasks.set(tasks.clone());

    // TS 的 `Submissions` 直接在接纳提交的回调里调用宿主 settings：抛错会让这次 submit 以同一
    // 错误拒绝，且什么都不入队。这里保留同一语义（回调返回 Result）。
    let queue_modes: Arc<dyn Fn() -> Result<QueueModes, BoxError> + Send + Sync> = {
        let host_settings = Arc::clone(&host_settings);
        Arc::new(move || {
            let settings =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host_settings()))
                    .map_err(panic_to_error)?;
            Ok(QueueModes::from_settings(&resolve_settings(
                settings.as_ref(),
            )))
        })
    };
    // 提交接纳与等待都会调用这个 `resume`，所以 `submit()`/`wait()` 会隐式启用调度；错误被
    // 忽略（只读打开时 `resume` 必失败，带写入的操作随后由提交路径自己拒绝）。
    let resume: Arc<dyn Fn() + Send + Sync> = {
        let tasks = tasks.clone();
        Arc::new(move || {
            let _ = tasks.resume();
        })
    };
    let submissions = Submissions::new_fallible(
        session.clone(),
        Arc::clone(&now),
        queue_modes,
        resume,
        options.telemetry.clone(),
    )?;

    // 任务图与会话视图都靠 Session 的内部提交监听器推进；图上的 blocked 原因实时问调度器。
    // `report_for_graph` 只用于把这些 watch 观察者的错误按 `listener_error` 转给宿主。
    let report_for_graph: Arc<dyn Fn(BoxError) + Send + Sync> = {
        let report = Arc::clone(&report);
        Arc::new(move |error| report(&*error, &ReportInfo::ListenerError, None))
    };
    // 两个视图构造时都会在 Session 上注册提交/关闭观察，失败（Session 已不可用）就传播为
    // open 的错误；它们共享上面那个受保护的 Storage 句柄。
    let task_graph = TaskGraphView::new(
        session.clone(),
        Arc::clone(&guarded),
        TaskGraphBlocking {
            blocked: {
                let tasks = tasks.clone();
                Arc::new(move |record| tasks.blocked_reason(record))
            },
            subscribe_blocked: {
                let tasks = tasks.clone();
                Arc::new(move |listener| tasks.subscribe_blocked(listener))
            },
        },
        Arc::clone(&report_for_graph),
    )?;
    let views = Arc::new(ConversationViews::new(
        session.clone(),
        Arc::clone(&guarded),
        report_for_graph,
    )?);

    let host = Arc::new(ConversationHost {
        harness: Arc::clone(&weak_slot),
    });

    // `Arc::new_cyclic` 在对象地址确定后给出 Weak：先把它填进 `weak_slot`（回调从此可以升级
    // 回 Harness），再用同一地址把 `ConversationViews` 登记到 `view.rs` 的全局 VIEWS，
    // `events::watch_events` 正是以 `&dyn Harness` 的数据指针查回这份视图。
    let harness = Arc::new_cyclic(|weak: &Weak<HarnessImpl>| {
        *weak_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(weak.clone());
        register_views(weak.as_ptr() as usize, &views);
        HarnessImpl {
            session,
            options,
            host_settings,
            report,
            now,
            tasks,
            submissions,
            task_graph,
            views,
            host,
        }
    });
    Ok(harness)
}

impl HarnessImpl {
    /// 按当前 registry 快照（或 `snapshot`）与当前设置解析一个会话的已提交 `kiana.agent`。
    ///
    /// # 参数与错误
    /// - `snapshot`：调度器传入它本 phase 使用的快照，保证 agent 与任务定义来自同一份 registry；
    ///   为 `None` 时现取当前快照。
    /// - `kiana.agent` 的快照无法反序列化为 `AgentState` 时按“无已存状态”处理（走默认值），
    ///   不报错；读取文档本身的错误照常返回。
    /// - 宿主 settings 回调 panic 会让本次解析以错误返回（对应 TS 的 throw）。
    ///
    /// 扩展包装器失败（panic/改名）被跳过，并以 `extension_error` 报告，附带本会话 id。返回值是
    /// 快照：之后的 registry 或文档变化需要重新调用才会体现。
    pub async fn resolve_agent(
        &self,
        id: ConversationId,
        snapshot: Option<Arc<dyn crate::harness::types::RegistrySnapshot>>,
        context: &dyn Context,
    ) -> Result<Arc<Agent>, BoxError> {
        let registry = snapshot.unwrap_or_else(|| self.options.registry.snapshot());
        let state = self
            .session
            .snapshot(&DocTarget::conversation(&AGENT_DOC, id), context)
            .await?;
        let state: Option<AgentState> =
            state.and_then(|state| serde_json::from_value(json_object(&state)).ok());
        let settings =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.host_settings)()))
            {
                Ok(settings) => resolve_settings(settings.as_ref()),
                // TS 里宿主 settings 抛错会让 `resolveAgent` 拒绝；这里把 panic 变成错误。
                Err(payload) => return Err(panic_to_error(payload)),
            };
        let report = Arc::clone(&self.report);
        let agent = resolve_agent(
            state.as_ref(),
            &*registry,
            &settings,
            &mut |error, extension| {
                report(
                    &*error,
                    &ReportInfo::ExtensionError {
                        extension: Some(extension.to_owned()),
                        task: None,
                        conversation_id: Some(id),
                    },
                    Some(context),
                );
            },
        );
        Ok(Arc::new(agent))
    }

    /// 从会话当前的 `cwd` 构建它的环境；没有 `env` 选项时返回 `None`。
    ///
    /// `cwd` 取自 `kiana.agent` 快照的 `cwd` 字段（缺失或不是字符串时为 `None`）。`EnvTarget`
    /// 里的 `read` 是 Session 的 `DocumentReader`，构建器可以借此读文档。文档读取或构建器返回的
    /// 错误原样传播，本方法不缓存结果。
    pub async fn build_env(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn ExecutionEnv>>, BoxError> {
        let Some(build) = &self.options.env else {
            return Ok(None);
        };
        let cwd = self
            .session
            .snapshot(&DocTarget::conversation(&AGENT_DOC, id), context)
            .await?
            .and_then(|state| {
                state
                    .get("cwd")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
            });
        let target = crate::harness::types::EnvTarget {
            conversation_id: id,
            cwd,
            read: Arc::new(self.session.clone()) as Arc<dyn DocumentReader>,
        };
        build(target, context.context_ref()).await
    }

    /// 把幸存的 `running` 任务调和成 `pending`；open 的一部分。
    ///
    /// 一条提交内完成：载入活任务，`running` 转 `pending` 并递增 restart 计数，达到上限的
    /// 停泊并报告 `task_parked`；只读打开只载入、不改写。它不派发任务，`resume()` 后才开始。
    pub async fn open_tasks(&self, context: &dyn Context) -> Result<(), BoxError> {
        self.tasks.open(context).await
    }

    // 失败闩锁优先于关闭状态：Session 既失败又已关闭时返回第一个失败原因（与 `closed_error`
    // 的选择一致，只是这里不经过 util）。
    fn assert_usable(&self) -> Result<(), BoxError> {
        if let Some(failure) = self.session.failure() {
            return Err(Box::new(SessionFailed::new(Box::new(failure.error()))));
        }
        if self.session.is_closing() {
            return Err(Box::new(SessionClosed));
        }
        Ok(())
    }

    /// 启用调度。
    ///
    /// 只读 Harness 以 `ReadOnlySession` 拒绝（错误文本描述的是“running tasks”）；关闭/失败后
    /// 以 `assert_usable` 的错误拒绝。启用后调度器会唤醒已到期的睡眠任务并尝试推进。
    pub fn resume(&self) -> Result<(), BoxError> {
        self.assert_usable()?;
        self.tasks.resume()
    }

    /// 下一次需要活进程的时点。TS 的 `wake()` 先 `assertUsable()` 再取值；失败时与 TS 一样抛
    /// 出错误（以 `panic_any` 携带同一个错误对象，调用方可 `catch_unwind` 取回）。
    ///
    /// `panic_any` 的负载就是 `BoxError`，可用本文件 `panic_to_error` 的同一套 downcast 还原。
    /// 只读 Harness 不会报错：调度器的 `wake()` 直接返回 `Wake { active: false, at: None }`。
    /// 需要 `Result` 的调用方用 [`try_wake`](Self::try_wake)。
    pub fn wake(&self) -> Wake {
        match self.try_wake() {
            Ok(wake) => wake,
            Err(error) => std::panic::panic_any(error),
        }
    }

    /// [`wake`](Self::wake) 的可观察错误版本。
    pub fn try_wake(&self) -> Result<Wake, BoxError> {
        self.assert_usable()?;
        Ok(self.tasks.wake())
    }

    /// 取一个任务。
    ///
    /// 经 Session 的变更线读 Storage，任务不存在时返回 `None`。`TaskRecord` 是那一刻的持久
    /// 状态，不包含调度器内存里的派生状态；后者用 `inspect()`。
    pub async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        let storage = self.session.storage().clone();
        let context_ref = context.context_ref();
        self.session
            .read_on_line(async move { storage.task(id, &*context_ref).await })
            .await
    }

    /// 内部（测试用）：调度器所有权索引的尺寸。
    pub fn scheduler_index_sizes(&self) -> crate::harness::scheduler::SchedulerIndexSizes {
        self.tasks.index_sizes()
    }

    /// 活动任务与未落定的提交。
    ///
    /// 整个读取在 Session 的变更线上执行，因此两次分页扫描之间不会插入提交：
    /// 1. 让调度器给出 `scheduling` 与每个活任务的派生状态（registry 快照由本方法在进入变更
    ///    线前取好并作为参数传入，调度器不会重新读取 registry）；
    /// 2. 用 `SCAN_PAGE_SIZE`（256）分页扫出 `queued` 与 `placed` 的提交；
    /// 3. 合并后按提交 id 升序排序再返回（`HarnessInspection` 的字段文档承诺“按 ID 顺序”）。
    pub async fn inspect(&self, context: &dyn Context) -> Result<HarnessInspection, BoxError> {
        let storage = self.session.storage().clone();
        let tasks = self.tasks.clone();
        let registry = self.options.registry.snapshot();
        let context_ref = context.context_ref();
        self.session
            .read_on_line(async move {
                let crate::harness::scheduler::SchedulerInspection {
                    scheduling,
                    tasks: inspected,
                } = tasks.inspect(registry).await?;
                let scan = |status: SubmissionStatus| {
                    let storage = Arc::clone(&storage);
                    let context_ref = Arc::clone(&context_ref);
                    async move {
                        scan_all(|cursor: Option<Cursor>| {
                            let query = SubmissionQuery {
                                status: Some(status),
                                ..SubmissionQuery::default()
                            };
                            let storage = Arc::clone(&storage);
                            let context_ref = Arc::clone(&context_ref);
                            async move {
                                storage
                                    .scan_submissions(
                                        &query,
                                        SCAN_PAGE_SIZE,
                                        cursor.as_ref(),
                                        &*context_ref,
                                    )
                                    .await
                            }
                        })
                        .await
                    }
                };
                let mut submissions = scan(SubmissionStatus::Queued).await?;
                submissions.extend(scan(SubmissionStatus::Placed).await?);
                submissions.sort_by_key(|record| record.id);
                Ok(HarnessInspection {
                    scheduling,
                    tasks: inspected,
                    submissions,
                })
            })
            .await
    }

    /// 重新获取一个提交。
    ///
    /// 按 id 读 Storage，不存在时返回 `None`；返回的句柄只是 `(id, Submissions)`，之后
    /// `status()`/`wait()`/`abort()` 都会重新读存储。
    pub async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn Submission>>, BoxError> {
        self.submissions.get(id, context).await
    }

    /// 中止一个提交。
    ///
    /// 只有 `queued` 的提交会被真正撤回（落定 `unanswered` 并移出 inbox）；已经进入转录或已
    /// 落定的分别返回 `AlreadyPlaced`/`Settled`。给出 `conversation_id` 时，属于别的会话的
    /// 提交按 `NotFound` 处理。整个过程是一条提交，检查与撤回是原子的。
    pub async fn abort_submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
        conversation_id: Option<ConversationId>,
    ) -> Result<AbortSubmissionOutcome, BoxError> {
        let outcome = self.submissions.abort(id, context, conversation_id).await?;
        Ok(match outcome {
            AbortResult::Aborted => AbortSubmissionOutcome::Aborted,
            AbortResult::AlreadyPlaced => AbortSubmissionOutcome::AlreadyPlaced,
            AbortResult::Settled => AbortSubmissionOutcome::Settled,
            AbortResult::NotFound => AbortSubmissionOutcome::NotFound,
        })
    }

    /// 中止一个任务。
    ///
    /// 提交 abort 标记；没有可接管 live 归属（owned live）的孤儿任务若解析为 `Blocked`，会在
    /// 同一提交里直接以 `orphaned` 落定。若该任务有 run 调用，本方法会等该调用结束（与
    /// Session 失败竞速），但不等待 abort handler 完成或任务进入终态；要等终态用
    /// `wait_for_task`。任务不存在时调度器返回错误。
    pub async fn abort_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<AbortTaskOutcome, BoxError> {
        let result = self.tasks.abort(id, context, false).await?;
        Ok(match result {
            crate::harness::scheduler::TaskAbortResult::Marked => AbortTaskOutcome::Marked,
            crate::harness::scheduler::TaskAbortResult::Terminal => AbortTaskOutcome::Terminal,
        })
    }

    /// 让被 restart 上限停泊的任务再跑一次。
    ///
    /// 只对停泊的任务有效：其它状态（终态、completing、未停泊）是无操作。任务不存在或提交
    /// 失败时返回错误；成功后调度器会顺带 `resume()`。
    pub async fn retry_task(&self, id: TaskId, context: &dyn Context) -> Result<(), BoxError> {
        self.assert_usable()?;
        self.tasks.retry(id, context).await
    }

    /// 等一个任务终态。
    ///
    /// 先 `resume()`（只读/已关闭会在这里失败），再登记等待；等待本身受 `context` 的取消影响。
    pub async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError> {
        self.tasks.resume()?;
        self.tasks.wait_for_task(id, context).await
    }

    /// 等所有 ownerless 会话空闲。
    ///
    /// 先 `resume()`，再等待普通遍历从所有 ownerless 会话都到不了活的非 background 任务；
    /// background 任务不阻止空闲。关闭时立即以 `closed_error` 的规则失败，已挂起的等待也会被
    /// 拒绝。
    pub async fn wait_for_idle(&self, context: &dyn Context) -> Result<(), BoxError> {
        self.tasks.resume()?;
        self.tasks.wait_for_idle(None, context).await
    }

    /// 汇总每个会话已提交的 `kiana.usage`。每份文档在各自时点读取；总量只会增长。
    ///
    /// 会话列表在变更线上一次扫完；随后逐个读 `kiana.usage` 快照，因此结果不是同一时点的原子
    /// 切片：扫描之后新建的会话不进入本次结果，单份文档在读取之间的增量也只体现在下一次调用。
    /// 单份文档缺失或无法反序列化时跳过它；总量用 `USAGE_DOC` 的初始值起算，初始值不可解析时
    /// 退回 `UsageState::default()`。
    pub async fn usage(&self, context: &dyn Context) -> Result<UsageState, BoxError> {
        let storage = self.session.storage().clone();
        let context_ref = context.context_ref();
        let conversations = self
            .session
            .read_on_line(async move {
                scan_all(|cursor: Option<Cursor>| {
                    let storage = Arc::clone(&storage);
                    let context_ref = Arc::clone(&context_ref);
                    async move {
                        storage
                            .scan_conversations(
                                &Default::default(),
                                SCAN_PAGE_SIZE,
                                cursor.as_ref(),
                                &*context_ref,
                            )
                            .await
                    }
                })
                .await
            })
            .await?;
        let initial = (USAGE_DOC.definition.initial)(None);
        let mut total: UsageState =
            serde_json::from_value(json_object(&initial)).unwrap_or_default();
        for conversation in conversations {
            let state = self
                .session
                .snapshot(
                    &DocTarget::conversation(&USAGE_DOC, conversation.id),
                    context,
                )
                .await?;
            if let Some(state) = state
                && let Ok(state) = serde_json::from_value::<UsageState>(json_object(&state))
            {
                add_usage_state(&mut total, &state);
            }
        }
        Ok(total)
    }

    /// 任务图 watch。
    ///
    /// 图在第一次观察时构建，之后随已存储的提交推进；取消 `context` 会停止 watch。
    pub async fn watch_task_graph(
        &self,
        context: &dyn Context,
    ) -> Result<TaskGraphWatch, BoxError> {
        self.task_graph.watch(context).await
    }

    /// 根会话。
    ///
    /// 根不存在时创建；已存在时在事务里直接复用 `ROOT_CONVERSATION_ID`，**不会**应用本次传入的
    /// `agent`/`init`（tests/harness_conversations.rs 的“第二次 root() 复用；不运行 init”锁定
    /// 了这一行为，并断言没有第二次存储提交）。创建失败会让整条提交回滚。
    pub async fn root(
        &self,
        context: &dyn Context,
        options: RootOptions,
    ) -> Result<Arc<dyn ConversationTrait>, BoxError> {
        self.create(
            CreateTarget::Root,
            CreateOptions {
                agent: options.agent,
                init: options.init,
            },
            context,
        )
        .await
    }

    /// 按 ID 取会话。
    ///
    /// 记录不存在时返回 `None`；存在时返回一个持有 Harness 强引用（`_keepalive`）的句柄，
    /// 因此只要句柄还在，Harness 就不会被释放。查找与关闭竞争时由 `assert_usable` 拒绝。
    pub async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationTrait>>, BoxError> {
        self.assert_usable()?;
        let storage = self.session.storage().clone();
        let context_ref = context.context_ref();
        let record = self
            .session
            .read_on_line(async move { storage.conversation(id, &*context_ref).await })
            .await?;
        match record {
            None => Ok(None),
            Some(record) => {
                let keepalive = self.host.upgrade()?;
                Ok(Some(Arc::new(ConversationImpl {
                    id: record.id,
                    host: Arc::clone(&self.host),
                    _keepalive: keepalive,
                }) as Arc<dyn ConversationTrait>))
            }
        }
    }

    /// 创建会话。
    ///
    /// 独立会话（不是根、不是 fork）；`ownership` 必填。`agent` 变更与 `init` 与创建记录在同
    /// 一条提交里执行，任一步失败都不留下会话。
    pub async fn create_conversation(
        &self,
        options: ConversationCreateOptions,
        context: &dyn Context,
    ) -> Result<Arc<dyn ConversationTrait>, BoxError> {
        self.create(
            CreateTarget::Independent {
                ownership: options.ownership,
            },
            CreateOptions {
                agent: options.agent,
                init: options.init,
            },
            context,
        )
        .await
    }

    /// 报告一个错误（TS: `report(error, info = { type: "listener_error" }, context?)`）。
    ///
    /// Rust 没有默认参数：要对应 TS 的默认值，调用方显式传 `ReportInfo::ListenerError`。
    /// 宿主 `on_report` 的 panic 被 `safe_report` 吞掉；遥测适配器自身的失败由适配器负责。
    pub fn report(
        &self,
        error: &(dyn std::error::Error + Send + Sync),
        info: ReportInfo,
        context: Option<&dyn Context>,
    ) {
        (self.report)(error, &info, context);
    }

    /// TS `#create`：创建（或复用根）会话，然后在同一提交里应用 agent 变更与 `init`。
    ///
    /// 三种目标共用一条提交：`Root` 先在事务里查根、存在则提前返回；`Independent` 与 `Fork`
    /// 分别调用事务的创建/fork。记录创建成功后按固定顺序应用 `agent`（`configure`）再 `init`，
    /// 因此 `init` 能读到 configure 写入的 `kiana.agent`。任一步失败都会回滚整条提交。
    async fn create(
        &self,
        target: CreateTarget,
        options: CreateOptions,
        context: &dyn Context,
    ) -> Result<Arc<dyn ConversationTrait>, BoxError> {
        self.assert_usable()?;
        let id = self
            .session
            .commit_with(
                move |tx| {
                    Box::pin(async move {
                        // 在事务里查：并发的两个 root() 由变更线串行化，后一个复用同一个根。
                        // 复用分支直接返回，因此本次的 agent/init 都不会执行（见 root() 文档）。
                        if matches!(target, CreateTarget::Root)
                            && tx.conversation(ROOT_CONVERSATION_ID).await?.is_some()
                        {
                            return Ok(ROOT_CONVERSATION_ID);
                        }
                        let record = match target {
                            CreateTarget::Root => tx.create_root_conversation().await?,
                            CreateTarget::Independent { ownership } => {
                                tx.create_conversation(&CreateConversationOptions { ownership })
                                    .await?
                            }
                            CreateTarget::Fork {
                                parent_id,
                                at,
                                ownership,
                            } => {
                                tx.fork_conversation(
                                    parent_id,
                                    at,
                                    &CreateConversationOptions { ownership },
                                )
                                .await?
                            }
                        };
                        if let Some(agent) = &options.agent {
                            configure(tx, record.id, agent).await?;
                        }
                        if let Some(init) = &options.init {
                            init(tx, record.id).await?;
                        }
                        Ok(record.id)
                    })
                },
                context.context_ref(),
                CommitOptions::default(),
            )
            .await?;
        Ok(Arc::new(ConversationImpl {
            id,
            host: Arc::clone(&self.host),
            _keepalive: self.host.upgrade()?,
        }))
    }

    /// 观察已存储的提交（TS 的 `HarnessImpl` 继承 `SessionImpl.observeCommits`）。
    ///
    /// 监听器在 Session 线上运行：与 `watch` 不同，每个提交恰好调用一次，不折叠。
    ///
    /// 与宿主 commit 监听器不同，内部监听器 panic 会让 Session 直接失败（SessionImpl 的
    /// `contained` 回调走 `fail`），实现必须自己吞掉预期内的错误。返回的注销闭包需要显式调用
    /// （`Unsubscribe` 是 `Box<dyn FnOnce()>`，丢弃不会注销）。
    pub fn observe_commits(
        &self,
        listener: crate::session::session::InternalListener,
    ) -> Result<crate::types::Unsubscribe, BoxError> {
        self.session.observe_commits(listener)
    }

    /// 视图 watch（内部便利）。
    ///
    /// 等价于对着同一个会话调用 `Conversation::watch`；视图随观察到的提交推进（包括只在内存里
    /// 采纳的延迟提交），取消 `context` 会停止 watch。
    pub async fn watch_view(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<ConversationWatch, BoxError> {
        self.views.watch(id, context).await
    }

    /// agent 事件流（TS: `watchEvents(this, id, context)`）。
    ///
    /// 转调 `harness::events::watch_events`。后者按 Harness 的对象地址从 `VIEWS` 取回本对象在
    /// `build_harness` 里登记的 `ConversationViews`，因此 `self` 必须就是那个 HarnessImpl；
    /// 拿别的 `dyn Harness` 实现来调用会得到逐字错误 `Not a Harness`。
    pub async fn watch_events(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<AgentEventStream, BoxError> {
        crate::harness::events::watch_events(self as &dyn HarnessTrait, id, context).await
    }

    /// `close()` 的可观察错误版本（TS 的 `close()` 返回可拒绝的关闭任务）。
    ///
    /// [`Session::close`](crate::types::Session::close) 为了对象安全丢弃错误；这里保留关闭任务
    /// 自身的错误，以及等待期间调用方上下文被取消的错误（关闭继续）。
    ///
    /// TS 的 `close()` 是普通方法：调用时就封闭接纳并同步运行关闭监听器（`#notifyClose()`），
    /// 返回的 promise 只负责等待。Rust 的 `async fn` 在第一次 poll 之前什么都不做，因此这里
    /// **同步启动一次**关闭任务（返回的 future 仍是 Joiner），让"调用 = 开始关闭"的时序与 TS
    /// 一致——包括在等待期间的提交不再投递给 watch。
    ///
    /// 返回值是 Joiner：重复调用加入同一次关闭，等待期间的上下文取消只让该次等待失败、关闭
    /// 任务继续（再次调用可以接着等）。因为本方法不是 `async fn`，构造 future 时就已经启动
    /// 关闭，即使调用方从不 poll 它。
    pub fn close_result<'a, C: crate::session::session::ContextSource + 'a>(
        &'a self,
        context: C,
    ) -> impl std::future::Future<Output = Result<(), BoxError>> + 'a {
        use futures::FutureExt as _;
        if !self.session.is_closing() {
            // 启动用的 future 只 poll 一次：begin_close 已运行，关闭任务在后台继续。
            let _ = self
                .session
                .close_result(context.context_ref())
                .now_or_never();
        }
        self.session.close_result(context)
    }
}

// ---------------------------------------------------------------------------
// ConversationImpl
// ---------------------------------------------------------------------------

/// 一个会话的无状态句柄（TS: `ConversationImpl`）。
///
/// TS 的 `#host.harness` 强引用 Harness，因此只要句柄在手，Harness 就仍然可用（即使调用方
/// 丢掉了自己的引用）。Rust 侧用 `keepalive` 复刻这一点；它不成环，因为 Harness 不持有
/// `ConversationImpl`。
struct ConversationImpl {
    id: ConversationId,
    host: Arc<ConversationHost>,
    _keepalive: Arc<HarnessImpl>,
}

// 除 `id()` 外，每个方法都先 `host.upgrade()`：句柄自身持有 `_keepalive`，正常使用中升级必然
// 成功，错误分支只有将来去掉 keepalive 时才可达。
#[async_trait]
impl ConversationTrait for ConversationImpl {
    fn id(&self) -> ConversationId {
        self.id
    }

    async fn agent(&self, context: &dyn Context) -> Result<Arc<Agent>, BoxError> {
        let harness = self.host.upgrade()?;
        harness.resolve_agent(self.id, None, context).await
    }

    // 独立提交：`configure` 只修改 `kiana.agent` 文档草稿，失败随提交回滚。
    async fn configure(&self, change: AgentChange, context: &dyn Context) -> Result<(), BoxError> {
        let harness = self.host.upgrade()?;
        let id = self.id;
        harness
            .session
            .commit_with(
                move |tx| Box::pin(async move { configure(tx, id, &change).await }),
                context.context_ref(),
                CommitOptions::default(),
            )
            .await
    }

    // 交给 `Submissions::submit`：它先请求 resume，再按 spec §6 的接纳规则放置/排队，并返回
    // 用于 status/wait/abort 的句柄。
    async fn submit(
        &self,
        submission: SubmissionDraft,
        context: &dyn Context,
    ) -> Result<Arc<dyn Submission>, BoxError> {
        let harness = self.host.upgrade()?;
        harness
            .submissions
            .submit(self.id, submission, context)
            .await
    }

    // `reset()` 是 Write 提交，不是一次运行。`kiana.reset` 条目总是 `head: "self"`；有 handoff
    // 时把文本包成一条 user 消息作为 `model` 一起写入，时间戳取自 Harness 的安全时钟。
    // `key_order` 复刻 TS 对象字面量的键序 `kind, head, model`，保证落盘键序一致。
    async fn reset(&self, handoff: Option<String>, context: &dyn Context) -> Result<(), BoxError> {
        let harness = self.host.upgrade()?;
        let model = handoff.map(|text| {
            vec![kiana_ai::Message::User(kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text(text),
                timestamp: (harness.now)(),
            })]
        });
        let entry = EntryDraft {
            kind: crate::types::ResetEntry.kind.to_string(),
            model,
            data: None,
            head: Some(EntryHead::SelfHead),
            edits: None,
            // TS: `{ kind: ResetEntry.kind, head: "self", ...model }`。
            key_order: Some(
                ["kind", "head", "model"]
                    .iter()
                    .map(|key| (*key).to_owned())
                    .collect(),
            ),
        };
        harness
            .submissions
            .submit(
                self.id,
                SubmissionDraft {
                    request_id: None,
                    kind: SubmissionDraftKind::Write { entry },
                },
                context,
            )
            .await?;
        Ok(())
    }

    // 手动压缩：`reason = Manual` 且没有 owner，因此 `create_compaction` 建的是会话拥有、
    // 非 background 的任务。它直接提交而不经过 `Submissions`，所以这里先 `resume()` 确保调度
    // 已启用；返回值是要等的任务 id，压缩结果由该任务的终态给出。
    async fn compact(
        &self,
        instructions: Option<String>,
        context: &dyn Context,
    ) -> Result<TaskId<CompactionResult>, BoxError> {
        let harness = self.host.upgrade()?;
        harness.tasks.resume()?;
        let id = self.id;
        let input = CompactionInput {
            reason: CompactionReason::Manual,
            instructions,
        };
        harness
            .session
            .commit_with(
                move |tx| Box::pin(async move { create_compaction(tx, id, input, None).await }),
                context.context_ref(),
                CommitOptions::default(),
            )
            .await
    }

    // 与 Harness 级 `commit` 的区别：这里带 `TransactionScope`，事务里的 `tx.createTask()`
    // 默认落到本会话（`task_id` 不设置，追加条目不会写 `byTaskId`）。
    async fn commit(
        &self,
        change: &dyn SessionChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        let harness = self.host.upgrade()?;
        harness
            .session
            .commit_transaction(
                &SessionChangeRef(change),
                context,
                Some(TransactionScope {
                    conversation_id: Some(self.id),
                    task_id: None,
                }),
                options,
            )
            .await
    }

    // `read_context` 每次全量扫描，不保留状态；`options.at` 为 Some 时只派生到该可见条目。
    async fn context(
        &self,
        context: &dyn Context,
        options: ContextViewOptions,
    ) -> Result<ContextView, BoxError> {
        let harness = self.host.upgrade()?;
        read_context(
            &harness.session,
            &**harness.session.storage(),
            self.id,
            context,
            options.at,
        )
        .await
    }

    // `into_query` 把 fork-aware 的范围绑定到本会话；扫描在变更线上执行，`limit`/`cursor`
    // 原样交给 Storage 分页。
    async fn entries(
        &self,
        query: EntryRangeQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        let harness = self.host.upgrade()?;
        let bounded = query.into_query(self.id);
        let storage = harness.session.storage().clone();
        let context_ref = context.context_ref();
        harness
            .session
            .read_on_line(async move {
                storage
                    .scan_entries(&bounded, limit, cursor.as_ref(), &*context_ref)
                    .await
            })
            .await
    }

    // fork 与普通创建共用 `create` 的提交：文档副本由文档定义的 as-of 语义生成，`agent`/`init`
    // 仍会在同一事务里应用（`create_agent` 对带 parent 的会话直接返回，不重复复制）。
    async fn fork(
        &self,
        at: EntryId,
        options: ConversationCreateOptions,
        context: &dyn Context,
    ) -> Result<Arc<dyn ConversationTrait>, BoxError> {
        let harness = self.host.upgrade()?;
        harness
            .create(
                CreateTarget::Fork {
                    parent_id: self.id,
                    at,
                    ownership: options.ownership,
                },
                CreateOptions {
                    agent: options.agent,
                    init: options.init,
                },
                context,
            )
            .await
    }

    // 与 TS 一样，中止路径会先 `resume()` 启用调度；`background` 决定是否越过 background 边界
    // 去中止下层任务。
    async fn abort(
        &self,
        context: &dyn Context,
        options: ConversationAbortOptions,
    ) -> Result<(), BoxError> {
        let harness = self.host.upgrade()?;
        harness.tasks.resume()?;
        harness
            .tasks
            .abort_conversation(self.id, options.background.unwrap_or(false), context)
            .await
    }

    async fn wait_for_idle(&self, context: &dyn Context) -> Result<(), BoxError> {
        let harness = self.host.upgrade()?;
        harness.tasks.resume()?;
        harness.tasks.wait_for_idle(Some(self.id), context).await
    }

    async fn watch(&self, context: &dyn Context) -> Result<ConversationWatch, BoxError> {
        let harness = self.host.upgrade()?;
        harness.views.watch(self.id, context).await
    }
}

// ---------------------------------------------------------------------------
// boundConversation
// ---------------------------------------------------------------------------

/// 一次调用的绑定句柄（TS: `boundConversation`）。
///
/// 每个操作先 `binding.check()`（调用已结束就拒绝），再把调用方的上下文与 binding 的 abort
/// signal 合并：任一方取消都会取消本次操作。`submit()` 返回的 `BoundSubmission` 沿用同一个
/// binding，因此任务结束后它也不能再被使用。
struct BoundConversation {
    id: ConversationId,
    binding: InvocationBinding,
    submissions: Submissions,
    tasks: TaskScheduler,
}

impl BoundConversation {
    fn bind(&self, context: &dyn Context) -> ContextRef {
        with_abort_signal(self.binding.signal.clone(), context.context_ref())
    }

    fn check(&self) -> Result<(), BoxError> {
        (self.binding.check)()
    }
}

#[async_trait]
impl ConversationHandle for BoundConversation {
    fn id(&self) -> ConversationId {
        self.id
    }

    async fn submit(
        &self,
        submission: crate::harness::types::InputSubmissionDraft,
        context: &dyn Context,
    ) -> Result<Arc<dyn Submission>, BoxError> {
        self.check()?;
        let bound = self.bind(context);
        let record = self
            .submissions
            .submit(self.id, submission.into_draft(), &*bound)
            .await?;
        Ok(Arc::new(BoundSubmission {
            id: record.id(),
            binding: self.binding.clone(),
            inner: record,
        }))
    }

    async fn abort(
        &self,
        context: &dyn Context,
        options: ConversationAbortOptions,
    ) -> Result<(), BoxError> {
        self.check()?;
        let bound = self.bind(context);
        self.tasks
            .abort_conversation(self.id, options.background.unwrap_or(false), &*bound)
            .await
    }

    async fn wait_for_idle(&self, context: &dyn Context) -> Result<(), BoxError> {
        self.check()?;
        let bound = self.bind(context);
        self.tasks.wait_for_idle(Some(self.id), &*bound).await
    }
}

/// 一个调用绑定提交的句柄：每个操作先检查调用是否结束，并在 binding 信号下运行。
struct BoundSubmission {
    id: SubmissionId,
    binding: InvocationBinding,
    inner: Arc<dyn Submission>,
}

impl BoundSubmission {
    fn bind(&self, context: &dyn Context) -> ContextRef {
        with_abort_signal(self.binding.signal.clone(), context.context_ref())
    }

    fn check(&self) -> Result<(), BoxError> {
        (self.binding.check)()
    }
}

#[async_trait]
impl Submission for BoundSubmission {
    fn id(&self) -> SubmissionId {
        self.id
    }

    async fn status(&self, context: &dyn Context) -> Result<SubmissionRecord, BoxError> {
        self.check()?;
        let bound = self.bind(context);
        self.inner.status(&*bound).await
    }

    async fn wait(&self, context: &dyn Context) -> Result<SettledSubmissionRecord, BoxError> {
        self.check()?;
        let bound = self.bind(context);
        self.inner.wait(&*bound).await
    }

    async fn abort(&self, context: &dyn Context) -> Result<SubmissionAbortOutcome, BoxError> {
        self.check()?;
        let bound = self.bind(context);
        self.inner.abort(&*bound).await
    }
}

fn bound_conversation(
    id: ConversationId,
    binding: InvocationBinding,
    submissions: Submissions,
    tasks: TaskScheduler,
) -> BoundConversation {
    BoundConversation {
        id,
        binding,
        submissions,
        tasks,
    }
}

// ---------------------------------------------------------------------------
// Session 转发
// ---------------------------------------------------------------------------

/// `&dyn SessionChange` 的事务适配器（TS 的 `commit(change)`）。
struct SessionChangeRef<'a>(&'a dyn SessionChange);

#[async_trait]
impl TransactionChange for SessionChangeRef<'_> {
    async fn run(&self, tx: &Transaction) -> Result<Box<dyn Any + Send>, BoxError> {
        self.0.run(tx).await
    }
}

// TS 的 Harness 继承 Session；Rust 用 trait 实现 + 转发。这里 `commit` 不设默认
// `TransactionScope`（Harness 级提交，事务不默认绑定任何会话）；需要“默认落到本会话”时用
// `Conversation::commit`。`subscribe_*` 在 SessionImpl 里返回 `Result`（关闭后拒绝），而 trait
// 签名没有错误通道，因此失败以 `panic_any(BoxError)` 抛出。
#[async_trait]
impl crate::types::Session for HarnessImpl {
    async fn commit(
        &self,
        change: &dyn SessionChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        self.session
            .commit_transaction(&SessionChangeRef(change), context, None, options)
            .await
    }

    // 对象安全的 `close` 丢弃错误；要观察关闭任务本身的错误或等待取消，用 `close_result`。
    async fn close(&self, context: &dyn Context) {
        self.session.close(context).await;
    }

    async fn closed(&self) -> SessionEnd {
        self.session.closed().await
    }

    fn subscribe_commits(
        &self,
        listener: crate::types::CommitListener,
    ) -> crate::types::Unsubscribe {
        match self.session.subscribe_commits(listener) {
            Ok(unsubscribe) => unsubscribe,
            Err(error) => std::panic::panic_any(error),
        }
    }

    fn subscribe_close(&self, listener: crate::types::CloseListener) -> crate::types::Unsubscribe {
        match self.session.subscribe_close(listener) {
            Ok(unsubscribe) => unsubscribe,
            Err(error) => std::panic::panic_any(error),
        }
    }
}

// TS 的继承面：Harness 本身也是 DocumentReader，外部可以只拿 `&dyn DocumentReader` 使用它；
// 实现全部转发给 Session。紧接着的 DocumentObserver 同理。
#[async_trait]
impl DocumentReader for HarnessImpl {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.session.snapshot(target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.session.snapshot_as_of(target, at, context).await
    }
}

#[async_trait]
impl DocumentObserver for HarnessImpl {
    async fn watch_doc(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<DocumentWatch>, BoxError> {
        self.session.watch_doc(target, context).await
    }
}

// 纯转发到同名 inherent 方法（显式写 `HarnessImpl::` 以避免递归）。trait 面比 inherent API 窄：
// `try_wake`、`resolve_agent`、`build_env`、`open_tasks`、`watch_view`/`watch_events`、
// `close_result` 等只在具体类型上提供。
#[async_trait]
impl HarnessTrait for HarnessImpl {
    fn resume(&self) -> Result<(), BoxError> {
        HarnessImpl::resume(self)
    }

    fn wake(&self) -> Wake {
        HarnessImpl::wake(self)
    }

    async fn root(
        &self,
        context: &dyn Context,
        options: RootOptions,
    ) -> Result<Arc<dyn ConversationTrait>, BoxError> {
        HarnessImpl::root(self, context, options).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationTrait>>, BoxError> {
        HarnessImpl::conversation(self, id, context).await
    }

    async fn create_conversation(
        &self,
        options: ConversationCreateOptions,
        context: &dyn Context,
    ) -> Result<Arc<dyn ConversationTrait>, BoxError> {
        HarnessImpl::create_conversation(self, options, context).await
    }

    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        HarnessImpl::get_task(self, id, context).await
    }

    async fn inspect(&self, context: &dyn Context) -> Result<HarnessInspection, BoxError> {
        HarnessImpl::inspect(self, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn Submission>>, BoxError> {
        HarnessImpl::submission(self, id, context).await
    }

    async fn abort_submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
        conversation_id: Option<ConversationId>,
    ) -> Result<AbortSubmissionOutcome, BoxError> {
        HarnessImpl::abort_submission(self, id, context, conversation_id).await
    }

    async fn abort_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<AbortTaskOutcome, BoxError> {
        HarnessImpl::abort_task(self, id, context).await
    }

    async fn retry_task(&self, id: TaskId, context: &dyn Context) -> Result<(), BoxError> {
        HarnessImpl::retry_task(self, id, context).await
    }

    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError> {
        HarnessImpl::wait_for_task(self, id, context).await
    }

    async fn wait_for_idle(&self, context: &dyn Context) -> Result<(), BoxError> {
        HarnessImpl::wait_for_idle(self, context).await
    }

    async fn usage(&self, context: &dyn Context) -> Result<UsageState, BoxError> {
        HarnessImpl::usage(self, context).await
    }

    async fn watch_task_graph(&self, context: &dyn Context) -> Result<TaskGraphWatch, BoxError> {
        HarnessImpl::watch_task_graph(self, context).await
    }
}
