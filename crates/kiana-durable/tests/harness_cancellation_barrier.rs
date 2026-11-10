//! `test/harness-cancellation-barrier.test.ts` 的 Rust 端口。
//!
//! 已逐用例端口（在 Session 上造记录后打开调度器，不需要完整 `Harness`）：
//! - "lets a failFast request in the same pass override a restart mark"
//! - "upgrades restart marks down the tree when an abort is requested above them"
//! - "marks a flagged task that holds its outcome, aborting its owned work, and keeps the outcome"
//!
//! 未端口："does not start its next phase while the cascade that marks it has not committed"
//! 需要扣住一次 abort 标记提交的存储闸门（TS 自定义 `HoldMarks extends MemoryStorage`），
//! 见报告的 remainingGaps。
//!
//! 其余测试是调度器级场景（abort 信号与 join、会话作用域空闲、failFast 依赖、会话失败赛跑）。
#![allow(dead_code)]

mod support;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{CreateModelsOptions, Models, ModelsImpl};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::scheduler::{
    AgentResolver, InvocationBinding, TaskScheduler, TaskSchedulerOptions, erase_task,
};
use kiana_durable::harness::types::{
    Agent, AnyTask, BlockedReason, ConversationHandle, Extension, RegistryReader, RegistrySection,
    RegistrySnapshot, RegistryTool, TaskInspectionState, TaskRuntime, TaskRuntimeChange,
};
use kiana_durable::session::{SessionImpl, SessionOptions, Transaction};
use kiana_durable::timers::host_timers;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, JoinPolicy, JsonValue, ReportInfo, Storage,
    StorageWrite, TaskAbortReason, TaskId, TaskOptions, TaskOutcome, TaskOutcomeError,
    TaskOwnership, TaskRecord, TaskState, TaskStatus, Tx,
};
use kiana_durable::{TaskDefinition, TaskMigrate, TaskPhase};
use serde_json::json;
use support::{commit, create_conversation, open_test_session};

// ---------------------------------------------------------------------------
// 假 registry / 假依赖
// ---------------------------------------------------------------------------

struct FakeSnapshot {
    tasks: Vec<AnyTask>,
}

impl RegistrySnapshot for FakeSnapshot {
    fn installed(&self) -> Vec<Arc<dyn Extension>> {
        Vec::new()
    }
    fn extension(&self, _name: &str) -> Option<Arc<dyn Extension>> {
        None
    }
    fn tools(&self) -> Vec<RegistryTool> {
        Vec::new()
    }
    fn sections(&self) -> Vec<RegistrySection> {
        Vec::new()
    }
    fn tasks(&self) -> Vec<AnyTask> {
        self.tasks.clone()
    }
    fn task(&self, name: &str) -> Option<AnyTask> {
        self.tasks
            .iter()
            .find(|task| task.definition.name == name)
            .cloned()
    }
}

struct FakeRegistry {
    tasks: Vec<AnyTask>,
    listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl RegistryReader for FakeRegistry {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::new(FakeSnapshot {
            tasks: self.tasks.clone(),
        })
    }
    fn subscribe(
        &self,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> kiana_durable::types::Unsubscribe {
        self.listeners.lock().unwrap().push(listener);
        Box::new(|| {})
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    storage: Arc<support::ControlledStorage>,
    session: SessionImpl,
    scheduler: TaskScheduler,
    reports: Arc<Mutex<Vec<ReportInfo>>>,
    registry: Arc<FakeRegistry>,
    /// 假 Harness 时钟（毫秒）；睡眠用例推进它。
    #[allow(dead_code)]
    clock: Arc<AtomicI64>,
}

impl Fixture {
    async fn open(tasks: Vec<AnyTask>) -> Fixture {
        let (fixture, ()) = Fixture::open_with(tasks, |_session| async {}).await;
        fixture
    }

    /// 在调度器打开前用 `setup` 创建任务（此时没有任务会被预留）。
    async fn open_with<T, F, Fut>(tasks: Vec<AnyTask>, setup: F) -> (Fixture, T)
    where
        F: FnOnce(SessionImpl) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let (storage, session, _publications) = open_test_session();
        let extra = setup(session.clone()).await;
        let fixture =
            Fixture::start(storage, session, tasks, Arc::new(AtomicI64::new(1_000_000))).await;
        (fixture, extra)
    }

    async fn wait(&self, id: TaskId) -> TaskRecord {
        self.scheduler
            .wait_for_task(id, &**BACKGROUND_CONTEXT)
            .await
            .expect("task settles")
            .into_record()
    }

    async fn wait_status(&self, id: TaskId, status: TaskStatus) -> TaskRecord {
        let record = self.wait(id).await;
        assert_eq!(record.state.status(), status, "record: {record:?}");
        record
    }

    fn report_types(&self) -> Vec<&'static str> {
        self.reports
            .lock()
            .unwrap()
            .iter()
            .map(report_type)
            .collect()
    }

    /// 在已有存储上重新打开一个 Session 与调度器（TS: 重开 Harness）。
    async fn attach(storage: Arc<support::ControlledStorage>, tasks: Vec<AnyTask>) -> Fixture {
        let session = SessionImpl::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            SessionOptions::default(),
        );
        Fixture::start(storage, session, tasks, Arc::new(AtomicI64::new(1_000_000))).await
    }

    async fn start(
        storage: Arc<support::ControlledStorage>,
        session: SessionImpl,
        tasks: Vec<AnyTask>,
        clock: Arc<AtomicI64>,
    ) -> Fixture {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let registry = Arc::new(FakeRegistry {
            tasks,
            listeners: Mutex::new(Vec::new()),
        });
        let report_sink = Arc::clone(&reports);
        let model: Arc<dyn Models> = Arc::new(ModelsImpl::new(CreateModelsOptions::new()));
        let now = Arc::clone(&clock);
        let scheduler = TaskScheduler::new(TaskSchedulerOptions {
            telemetry: None,
            session: session.clone(),
            storage: storage.clone(),
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            models: Arc::new(move || Arc::clone(&model)),
            agent: minimal_agent(),
            settings: Arc::new(|| Ok(Arc::new(resolve_settings(None)))),
            env: Arc::new(|_, _| Box::pin(async { Ok(None) })),
            now: Arc::new(move || now.load(Ordering::SeqCst)),
            on_wake: None,
            timers: Arc::new(host_timers(None, Arc::new(|_| {}))),
            write_at_close: Arc::new(|_writes| Box::pin(async { Ok(()) })),
            report: Arc::new(move |_error, info, _context| {
                report_sink.lock().unwrap().push(info.clone());
            }),
            settle_outcome: Arc::new(
                |_tx: &Transaction,
                 _record: &TaskRecord,
                 _outcome: &kiana_durable::harness::types::SchedulerOutcome| {
                    Box::pin(async { Ok(()) })
                },
            ),
            withdraw_inputs: Arc::new(|_tx: &Transaction, _conversation: ConversationId| {
                Box::pin(async { Ok(()) })
            }),
            conversation:
                Arc::new(
                    |_id: ConversationId,
                     _binding: InvocationBinding,
                     _context: kiana_chord::context::ContextRef|
                     -> BoxFuture<
                        'static,
                        Result<Option<Arc<dyn ConversationHandle>>, BoxError>,
                    > { Box::pin(async { Ok(None) }) },
                ),
            context: Arc::clone(&BACKGROUND_CONTEXT),
            read_only: None,
        });
        let context = &**BACKGROUND_CONTEXT;
        scheduler.open(context).await.expect("open succeeds");
        scheduler.resume().expect("resume succeeds");
        Fixture {
            storage,
            session,
            scheduler,
            reports,
            registry,
            clock,
        }
    }
}

fn report_type(info: &ReportInfo) -> &'static str {
    match info {
        ReportInfo::TaskFaulted { .. } => "task_faulted",
        ReportInfo::TaskParked { .. } => "task_parked",
        ReportInfo::MigrationFailed { .. } => "migration_failed",
        ReportInfo::TaskOutdated { .. } => "task_outdated",
        ReportInfo::TaskError { .. } => "task_error",
        ReportInfo::ExtensionError { .. } => "extension_error",
        ReportInfo::HostError { .. } => "host_error",
        ReportInfo::ListenerError => "listener_error",
        ReportInfo::SessionFailed => "session_failed",
    }
}

fn minimal_agent() -> AgentResolver {
    Arc::new(|_conversation, _snapshot, _context| {
        Box::pin(async {
            Ok(Arc::new(Agent {
                model: None,
                thinking_level: ModelThinkingLevel::Off,
                extensions: Vec::new(),
                tools: Vec::new(),
                callable: Vec::new(),
                deferred: Vec::new(),
                sections: Vec::new(),
                instructions: None,
                cwd: None,
            }))
        })
    })
}

// ---------------------------------------------------------------------------
// 任务定义
// ---------------------------------------------------------------------------

type Def = TaskDefinition<JsonValue, JsonValue, JsonValue, ()>;
type Phase = Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>;

fn noop() -> Phase {
    Arc::new(Noop)
}
fn chain(next: &str) -> Phase {
    Arc::new(Chain {
        next: next.to_owned(),
    })
}
fn complete() -> Phase {
    Arc::new(Complete)
}
fn idle() -> Phase {
    Arc::new(Idle)
}

fn define(name: &str, version: i64, phases: Vec<(&str, Phase)>) -> AnyTask {
    define_with(name, version, phases, noop(), None)
}

fn define_with(
    name: &str,
    version: i64,
    phases: Vec<(&str, Phase)>,
    abort: Phase,
    migrate: Option<TaskMigrate<JsonValue, JsonValue>>,
) -> AnyTask {
    let phases: IndexMap<String, Phase> = phases
        .into_iter()
        .map(|(name, phase)| (name.to_owned(), phase))
        .collect();
    let definition: Def = TaskDefinition {
        name: name.to_owned(),
        version,
        initial: Arc::new(|_| json!({ "phase": "start" })),
        phases,
        abort,
        migrate,
        hooks: None,
    };
    erase_task(&definition)
}

struct Noop;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Noop {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

/// 提交一个 `running` 状态，checkpoint 的 phase 指到下一段。
struct Chain {
    next: String,
}
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Chain {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(
                &NextPhase {
                    phase: self.next.clone(),
                },
                context,
                CommitOptions::default(),
            )
            .await
    }
}

struct NextPhase {
    phase: String,
}
#[async_trait]
impl TaskRuntimeChange for NextPhase {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Running {
            checkpoint: json!({ "phase": self.phase }),
        }))
    }
}

struct Complete;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Complete {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(
                &TerminalChange(TaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: json!({ "ok": true }),
                    },
                }),
                context,
                CommitOptions::default(),
            )
            .await
    }
}

struct TerminalChange(TaskState);
#[async_trait]
impl TaskRuntimeChange for TerminalChange {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(self.0.clone()))
    }
}

/// 不做持久进展：调度器必须让它 fault。
struct Idle;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Idle {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 帮助函数：创建任务
// ---------------------------------------------------------------------------

async fn conversation(session: &SessionImpl) -> ConversationId {
    create_conversation(session).await
}

async fn create(
    session: &SessionImpl,
    task: &AnyTask,
    conversation: ConversationId,
    ownership: TaskOwnership,
    input: JsonValue,
) -> TaskId {
    let task = task.clone();
    commit(session, move |tx| {
        let task = task.clone();
        Box::pin(async move {
            let owned_by_conversation = matches!(ownership, TaskOwnership::Conversation);
            let mut options = TaskOptions::new(ownership);
            if owned_by_conversation {
                options.conversation_id = Some(conversation);
            }
            tx.create_task(&task, input, options).await
        })
    })
    .await
    .expect("create task")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
struct Hold;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Hold {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime.signal().token().cancelled().await;
        Ok(())
    }
}

struct Aborted;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Aborted {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(
                &TerminalChange(TaskState::Terminal {
                    outcome: TaskOutcome::Aborted {
                        reason: None,
                        result: None,
                    },
                }),
                context,
                CommitOptions::default(),
            )
            .await
    }
}

fn holding(name: &str) -> AnyTask {
    define_with(
        name,
        1,
        vec![("start", Arc::new(Hold))],
        Arc::new(Aborted),
        None,
    )
}

#[tokio::test]
async fn abort_signals_the_run_and_joins_it() {
    let fixture = Fixture::open(vec![holding("test.cancel.hold")]).await;
    let conversation = conversation(&fixture.session).await;
    let task = holding("test.cancel.hold");
    let id = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    for _ in 0..600 {
        let current = fixture
            .storage
            .task(id, &**BACKGROUND_CONTEXT)
            .await
            .expect("read")
            .expect("exists");
        if current.state.status() == TaskStatus::Running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let result = fixture
        .scheduler
        .abort(id, &**BACKGROUND_CONTEXT, false)
        .await
        .expect("abort succeeds");
    assert!(matches!(
        result,
        kiana_durable::harness::scheduler::TaskAbortResult::Marked
    ));
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    assert!(matches!(
        record.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Aborted { .. }
        }
    ));
}

#[tokio::test]
async fn abort_conversation_waits_for_the_scope_to_become_idle() {
    let fixture = Fixture::open(vec![holding("test.cancel.scope")]).await;
    let conversation = conversation(&fixture.session).await;
    let task = holding("test.cancel.scope");
    let first = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let second = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    fixture
        .scheduler
        .abort_conversation(conversation, false, &**BACKGROUND_CONTEXT)
        .await
        .expect("abort conversation succeeds");
    for id in [first, second] {
        let record = fixture.wait_status(id, TaskStatus::Terminal).await;
        assert!(matches!(
            record.state,
            TaskState::Terminal {
                outcome: TaskOutcome::Aborted { .. }
            }
        ));
    }
    fixture
        .scheduler
        .wait_for_idle(Some(conversation), &**BACKGROUND_CONTEXT)
        .await
        .expect("scope is idle");
    assert_eq!(fixture.scheduler.index_sizes().live, 0);
}

/// TS `#observe` 只把**自身**处于 `waiting` 且策略为 `failFast` 的 waiter 加入
/// `#failFastChecks`；同一失败任务上的 `allSettled` waiter 不得连带标记其余依赖。
#[tokio::test]
async fn a_fail_fast_waiter_does_not_cancel_another_waiters_dependencies() {
    let fixture = Fixture::open(Vec::new()).await;
    let conversation = conversation(&fixture.session).await;
    // 未注册的定义让这些任务保持可等待而不运行。
    let definition = define("test.cancel.waiter", 1, vec![("start", noop())]);
    let failed = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let held = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let marked = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let fail_fast = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let all_settled = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;

    // 同一个提交：失败任务 + 两个不同策略的 waiter。所有读都在第一次写之前。
    commit(&fixture.session, move |tx| {
        Box::pin(async move {
            let mut failed_record = tx.task(failed).await?.expect("failed exists");
            let mut fail_fast_record = tx.task(fail_fast).await?.expect("failFast exists");
            let mut all_settled_record = tx.task(all_settled).await?.expect("allSettled exists");
            failed_record.state = TaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: TaskOutcomeError {
                        message: "declined".to_owned(),
                        detail: None,
                    },
                    result: None,
                },
            };
            fail_fast_record.state = TaskState::Waiting {
                checkpoint: json!({ "phase": "start" }),
                on: vec![failed, marked],
                policy: JoinPolicy::FailFast,
            };
            all_settled_record.state = TaskState::Waiting {
                checkpoint: json!({ "phase": "start" }),
                on: vec![failed, held],
                policy: JoinPolicy::AllSettled,
            };
            tx.set_task(failed_record)?;
            tx.set_task(fail_fast_record)?;
            tx.set_task(all_settled_record)?;
            Ok(())
        })
    })
    .await
    .expect("write waiting records");

    // `marked` 是 failFast waiter 的另一依赖：检查必然跑过并标记它。
    let mut marked_record = None;
    for _ in 0..400 {
        let record = fixture
            .storage
            .task(marked, &**BACKGROUND_CONTEXT)
            .await
            .expect("read succeeds")
            .expect("task exists");
        if record.abort_requested {
            marked_record = Some(record);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        marked_record.is_some(),
        "the failFast waiter marks its other dependency"
    );
    let held_record = fixture
        .storage
        .task(held, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists");
    assert!(
        !held_record.abort_requested,
        "an allSettled waiter's other dependency is never abort-marked"
    );
    let all_settled_record = fixture
        .storage
        .task(all_settled, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists");
    assert_eq!(all_settled_record.state.status(), TaskStatus::Waiting);
    assert!(!all_settled_record.abort_requested);
}

/// 忽略取消信号、永不返回的 phase：模拟一个活过失败 Session 的 run 调用。
struct Forever;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Forever {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        std::future::pending::<()>().await;
        Ok(())
    }
}

fn forever(name: &str) -> AnyTask {
    define_with(
        name,
        1,
        vec![("start", Arc::new(Forever))],
        Arc::new(Aborted),
        None,
    )
}

/// TS 的 `abort()` 与 `#session.failed` 赛跑：一个忽略信号、活过失败 Session 的 run 调用让
/// abort() 以 `SessionFailed` 拒绝（而不是一直等 run，或成功返回）。
#[tokio::test]
async fn abort_rejects_with_the_session_failure_when_the_run_outlives_it() {
    let fixture = Fixture::open(vec![forever("test.cancel.forever")]).await;
    let conversation = conversation(&fixture.session).await;
    let task = forever("test.cancel.forever");
    let id = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    // 等 run 调用真正跑起来。
    let mut running = false;
    for _ in 0..600 {
        let current = fixture
            .storage
            .task(id, &**BACKGROUND_CONTEXT)
            .await
            .expect("read")
            .expect("exists");
        if current.state.status() == TaskStatus::Running {
            running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(running, "the task starts running");
    let scheduler = fixture.scheduler.clone();
    let abort =
        tokio::spawn(async move { scheduler.abort(id, &**BACKGROUND_CONTEXT, false).await });
    // 等 abort 标记落盘：提交成功后 abort() 才进入与 Session 失败的赛跑。
    let mut marked = false;
    for _ in 0..600 {
        let current = fixture
            .storage
            .task(id, &**BACKGROUND_CONTEXT)
            .await
            .expect("read")
            .expect("exists");
        if current.abort_requested {
            marked = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(marked, "the abort mark commits");
    // run 调用忽略信号、永远不结束；Session 失败结束等待，并以它的错误拒绝。
    fixture
        .session
        .fail(Box::new(std::io::Error::other("boom")));
    // 回归时 abort() 会一直等 run（旧行为）：用超时把它变成失败而不是挂起。
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), abort)
        .await
        .expect("abort must not wait for a run that outlives a failed Session")
        .expect("the abort task joins");
    let error = result.expect_err("a run that outlives a failed Session fails the abort");
    assert!(
        error.to_string().contains("Session failed"),
        "error: {error}"
    );
}

// ---------------------------------------------------------------------------
// `harness-cancellation-barrier.test.ts` 逐用例端口（调度器可直测部分）
// ---------------------------------------------------------------------------

/// 轮询直到 `check` 成立（TS: `eventually()`）。
async fn eventually<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..600 {
        if check().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("condition was not reached");
}

async fn read_record(storage: &support::ControlledStorage, id: TaskId) -> TaskRecord {
    storage
        .task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists")
}

/// 直接写 Storage（TS 测试里的 `storage.commit([...])`，绕过 Session/事务）。
fn write_records(storage: &support::ControlledStorage, values: Vec<TaskRecord>) {
    let writes: Vec<StorageWrite> = values
        .into_iter()
        .map(|value| StorageWrite::Task { value })
        .collect();
    storage
        .inner()
        .commit_inner(&writes, &|_| {})
        .expect("write records");
}

/// TS: “lets a failFast request in the same pass override a restart mark”。
///
/// 一个 restart 放弃的 owner 以 failFast 等待一个失败的孩子和一个活孩子；同一 pass 里
/// failFast 产生的请求必须赢过 restart 级联的标记，于是没有定义的 child 被 orphaned。
#[tokio::test]
async fn lets_a_fail_fast_request_in_the_same_pass_override_a_restart_mark() {
    let (storage, session, _publications) = open_test_session();
    let dormant = holding("test.dormant");
    let conversation = conversation(&session).await;
    let parent = create(
        &session,
        &dormant,
        conversation,
        TaskOwnership::Conversation,
        json!(null),
    )
    .await;
    let failed_task = create(
        &session,
        &dormant,
        conversation,
        TaskOwnership::Task { task_id: parent },
        json!(null),
    )
    .await;
    let child = create(
        &session,
        &dormant,
        conversation,
        TaskOwnership::Task { task_id: parent },
        json!(null),
    )
    .await;
    // restart 放弃的 owner 以 failFast 等待一个失败的孩子和一个活孩子。
    let mut owner = read_record(&storage, parent).await;
    owner.abort_requested = true;
    owner.abort_reason = Some(TaskAbortReason::Restart);
    owner.state = TaskState::Waiting {
        checkpoint: json!({ "phase": "work" }),
        on: vec![failed_task, child],
        policy: JoinPolicy::FailFast,
    };
    let mut failed = read_record(&storage, failed_task).await;
    failed.state = TaskState::Terminal {
        outcome: TaskOutcome::Failed {
            error: TaskOutcomeError {
                message: "failed".to_owned(),
                detail: None,
            },
            result: None,
        },
    };
    write_records(&storage, vec![owner, failed]);
    // child 的定义缺失：restart 标记会让它继续等定义，请求则把它 orphaned。
    let fixture = Fixture::attach(Arc::clone(&storage), Vec::new()).await;
    let record = fixture.wait(child).await;
    assert_eq!(record.state.status(), TaskStatus::Terminal);
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Orphaned { reason },
        } => assert_eq!(reason, "missing_task"),
        other => panic!("child outcome: {other:?}"),
    }
}

/// TS: “upgrades restart marks down the tree when an abort is requested above them”。
#[tokio::test]
async fn upgrades_restart_marks_down_the_tree_when_an_abort_is_requested_above_them() {
    let (storage, session, _publications) = open_test_session();
    let dormant = holding("test.dormant");
    let missing = holding("test.missing");
    let conversation = conversation(&session).await;
    let top = create(
        &session,
        &dormant,
        conversation,
        TaskOwnership::Conversation,
        json!(null),
    )
    .await;
    let middle = {
        let task = dormant.clone();
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut options = TaskOptions::new(TaskOwnership::Task { task_id: top });
                options.abandon_on_restart = Some(true);
                tx.create_task(&task, json!(null), options).await
            })
        })
        .await
        .expect("create middle")
    };
    let leaf = create(
        &session,
        &missing,
        conversation,
        TaskOwnership::Task { task_id: middle },
        json!(null),
    )
    .await;
    // 只注册 Dormant：leaf 在 restart 级联下等它的定义。
    let fixture = Fixture::attach(Arc::clone(&storage), vec![dormant]).await;
    eventually(|| async {
        read_record(&fixture.storage, leaf).await.abort_reason == Some(TaskAbortReason::Restart)
    })
    .await;
    eventually(|| async {
        let inspection = fixture
            .scheduler
            .inspect(fixture.registry.snapshot())
            .await
            .expect("inspect succeeds");
        inspection.tasks.iter().any(|task| {
            task.record.id == leaf
                && matches!(
                    task.state,
                    TaskInspectionState::Blocked {
                        reason: BlockedReason::MissingTask,
                        ..
                    }
                )
        })
    })
    .await;
    assert_ne!(
        read_record(&fixture.storage, leaf).await.state.status(),
        TaskStatus::Terminal
    );
    // 对 top 的请求向下两层到达 leaf，它随后被 orphaned。
    fixture
        .scheduler
        .abort(top, &**BACKGROUND_CONTEXT, false)
        .await
        .expect("abort top");
    let leaf_record = fixture.wait(leaf).await;
    match leaf_record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Orphaned { reason },
        } => assert_eq!(reason, "missing_task"),
        other => panic!("leaf outcome: {other:?}"),
    }
    for id in [middle, top] {
        let record = fixture.wait(id).await;
        assert!(
            matches!(
                record.state,
                TaskState::Terminal {
                    outcome: TaskOutcome::Aborted { .. }
                }
            ),
            "record: {record:?}"
        );
    }
}

/// TS: “marks a flagged task that holds its outcome, aborting its owned work, and keeps the outcome”。
#[tokio::test]
async fn marks_a_flagged_task_that_holds_its_outcome_aborting_owned_work_and_keeps_the_outcome() {
    let (storage, session, _publications) = open_test_session();
    let dormant = holding("test.dormant");
    let conversation = conversation(&session).await;
    let holder = {
        let task = dormant.clone();
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut options = TaskOptions::new(TaskOwnership::Conversation);
                options.conversation_id = Some(conversation);
                options.abandon_on_restart = Some(true);
                tx.create_task(&task, json!(null), options).await
            })
        })
        .await
        .expect("create holder")
    };
    let child = create(
        &session,
        &dormant,
        conversation,
        TaskOwnership::Task { task_id: holder },
        json!(null),
    )
    .await;
    let mut completing = read_record(&storage, holder).await;
    completing.state = TaskState::Completing {
        outcome: TaskOutcome::Completed {
            result: JsonValue::Null,
        },
    };
    write_records(&storage, vec![completing]);
    let fixture = Fixture::attach(Arc::clone(&storage), vec![dormant]).await;
    let child_record = fixture.wait(child).await;
    assert!(
        matches!(
            child_record.state,
            TaskState::Terminal {
                outcome: TaskOutcome::Aborted { .. }
            }
        ),
        "record: {child_record:?}"
    );
    let settled = fixture.wait(holder).await;
    assert_eq!(settled.abort_reason, Some(TaskAbortReason::Restart));
    assert!(
        matches!(
            settled.state,
            TaskState::Terminal {
                outcome: TaskOutcome::Completed { .. }
            }
        ),
        "record: {settled:?}"
    );
}
