//! `test/harness-tasks.test.ts` 的 Rust 端口（调度器可直测部分）。
//!
//! TS 测试围绕完整 `Harness`/`Conversation` 打开任务；本单元的上层 `harness.rs` 尚未实现，
//! 因此这里直接用 `SessionImpl` 提交任务、用 `TaskScheduler` 驱动同一条调度路径（open → resume
//! → reserve → start），断言 phase 推进、fault、waiting/resume、终态与时间戳。依赖 Harness 便利
//! 方法（root 句柄、resume/close 循环、事件流）的用例见报告的 remainingGaps。
//!
//! 另含针对修复的回归用例：`wait_for_task` 不校验终态、数字 `checkpoint.phase` 的属性键强制。

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
    AgentResolver, InvocationBinding, SchedulerInspection, TaskScheduler, TaskSchedulerOptions,
    erase_task,
};
use kiana_durable::harness::types::{
    Agent, AnyTask, ConversationHandle, Extension, RegistryReader, RegistrySection,
    RegistrySnapshot, RegistryTool, SchedulingState, TaskRuntime, TaskRuntimeChange, Wake,
};
use kiana_durable::session::{SessionImpl, SessionOptions, Transaction};
use kiana_durable::timers::host_timers;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, JoinPolicy, JsonValue, ReportFrom, ReportInfo,
    Storage, TaskId, TaskOptions, TaskOutcome, TaskOwnership, TaskRecord, TaskState, TaskStatus,
    Tx,
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
    #[allow(dead_code)]
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
        Fixture::start_configured(storage, session, tasks, clock, None, true, None).await
    }

    /// 在调度器打开前用 `setup` 创建任务，但不 `resume()`（TS: 未 resume 的 Harness）。
    async fn open_paused_with<T, F, Fut>(tasks: Vec<AnyTask>, setup: F) -> (Fixture, T)
    where
        F: FnOnce(SessionImpl) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let (storage, session, _publications) = open_test_session();
        let extra = setup(session.clone()).await;
        let fixture = Fixture::start_configured(
            storage,
            session,
            tasks,
            Arc::new(AtomicI64::new(1_000_000)),
            None,
            false,
            None,
        )
        .await;
        (fixture, extra)
    }

    /// 只读打开（TS: `HarnessOptions.readOnly`）：不 resume，只观察。
    async fn open_read_only_with<T, F, Fut>(tasks: Vec<AnyTask>, setup: F) -> (Fixture, T)
    where
        F: FnOnce(SessionImpl) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let (storage, session, _publications) = open_test_session();
        let extra = setup(session.clone()).await;
        let fixture = Fixture::start_configured(
            storage,
            session,
            tasks,
            Arc::new(AtomicI64::new(1_000_000)),
            None,
            false,
            Some(true),
        )
        .await;
        (fixture, extra)
    }

    /// 带遥测适配器打开（TS: `HarnessOptions.telemetry`）。
    async fn open_with_telemetry(
        tasks: Vec<AnyTask>,
        telemetry: Arc<dyn kiana_telemetry::TelemetryContext>,
    ) -> Fixture {
        let (storage, session, _publications) = open_test_session();
        Fixture::start_configured(
            storage,
            session,
            tasks,
            Arc::new(AtomicI64::new(1_000_000)),
            Some(telemetry),
            true,
            None,
        )
        .await
    }

    async fn start_configured(
        storage: Arc<support::ControlledStorage>,
        session: SessionImpl,
        tasks: Vec<AnyTask>,
        clock: Arc<AtomicI64>,
        telemetry: Option<Arc<dyn kiana_telemetry::TelemetryContext>>,
        resume: bool,
        read_only: Option<bool>,
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
            telemetry,
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
            read_only,
        });
        let context = &**BACKGROUND_CONTEXT;
        scheduler.open(context).await.expect("open succeeds");
        if resume {
            scheduler.resume().expect("resume succeeds");
        }
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

#[tokio::test]
async fn continues_one_invocation_through_checkpoint_progress_and_completes() {
    let fixture = Fixture::open(vec![define(
        "test.tasks.chain",
        1,
        vec![("start", chain("second")), ("second", complete())],
    )])
    .await;
    let conversation = conversation(&fixture.session).await;
    let id = create(
        &fixture.session,
        &define(
            "test.tasks.chain",
            1,
            vec![("start", chain("second")), ("second", complete())],
        ),
        conversation,
        TaskOwnership::Conversation,
        json!({ "path": "a" }),
    )
    .await;
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Completed { result },
        } => assert_eq!(result, json!({ "ok": true })),
        other => panic!("unexpected state {other:?}"),
    }
    assert!(record.started_at.is_some(), "startedAt is stamped");
    assert!(record.ended_at.is_some(), "endedAt is stamped");
    assert_eq!(record.restarts, None, "a clean run has no restart count");
    let _ = fixture.storage;
}

#[tokio::test]
async fn faults_a_phase_without_durable_progress() {
    let fixture = Fixture::open(vec![define("test.tasks.idle", 1, vec![("start", idle())])]).await;
    let conversation = conversation(&fixture.session).await;
    let definition = define("test.tasks.idle", 1, vec![("start", idle())]);
    let id = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Faulted { error },
        } => assert_eq!(
            error.message,
            "Task test.tasks.idle phase start returned without durable progress"
        ),
        other => panic!("unexpected state {other:?}"),
    }
    assert_eq!(fixture.report_types(), vec!["task_faulted"]);
}

#[tokio::test]
async fn keeps_a_committed_terminal_outcome_when_the_handler_throws_afterwards() {
    struct CommitThenThrow;
    #[async_trait]
    impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for CommitThenThrow {
        async fn run(
            &self,
            _task: &TaskRecord,
            runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
            context: &dyn Context,
        ) -> Result<(), BoxError> {
            runtime
                .commit(
                    &TerminalChange(TaskState::Terminal {
                        outcome: TaskOutcome::Completed { result: json!(7) },
                    }),
                    context,
                    CommitOptions::default(),
                )
                .await?;
            Err(Box::new(std::io::Error::other("after commit")))
        }
    }
    let definition = define(
        "test.tasks.throw_after",
        1,
        vec![("start", Arc::new(CommitThenThrow))],
    );
    let fixture = Fixture::open(vec![definition.clone()]).await;
    let conversation = conversation(&fixture.session).await;
    let id = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Completed { result },
        } => assert_eq!(result, json!(7)),
        other => panic!("unexpected state {other:?}"),
    }
}

#[tokio::test]
async fn inspect_reports_missing_and_old_definitions_without_running_them() {
    use kiana_durable::harness::types::{BlockedReason, TaskInspectionState};

    let fixture = Fixture::open(vec![define(
        "test.tasks.old",
        1,
        vec![("start", complete())],
    )])
    .await;
    let conversation = conversation(&fixture.session).await;
    let known = define("test.tasks.known", 2, vec![("start", complete())]);
    let missing = create(
        &fixture.session,
        &known,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    // 存了 v2、registry 里是 v1：过旧。
    let old = define("test.tasks.old", 2, vec![("start", complete())]);
    let old_id = create(
        &fixture.session,
        &old,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let inspection = fixture
        .scheduler
        .inspect(fixture.registry.snapshot())
        .await
        .expect("inspect succeeds");
    assert_eq!(
        inspection.scheduling,
        kiana_durable::harness::types::SchedulingState::Running
    );
    let state_of = |id: TaskId| {
        inspection
            .tasks
            .iter()
            .find(|task| task.record.id == id)
            .map(|task| task.state.clone())
            .expect("inspection lists live tasks")
    };
    assert_eq!(
        state_of(missing),
        TaskInspectionState::Blocked {
            reason: BlockedReason::MissingTask,
            error: None,
        }
    );
    assert_eq!(
        state_of(old_id),
        TaskInspectionState::Blocked {
            reason: BlockedReason::TaskTooOld,
            error: None,
        }
    );
}

/// 已提交 checkpoint 指向定义里不存在的 phase 时，TS 的 `erased(...).phases[...]` 调用抛出
/// TypeError，其 message 成为阶段失败；任务照常 fault。
#[tokio::test]
async fn a_checkpoint_naming_a_missing_phase_faults_with_the_call_type_error() {
    let definition = define(
        "test.tasks.missing_phase",
        1,
        vec![("start", chain("gone"))],
    );
    let fixture = Fixture::open(vec![definition.clone()]).await;
    let conversation = conversation(&fixture.session).await;
    let id = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Faulted { error },
        } => assert_eq!(
            error.message,
            "erased(...).phases[checkpoint.phase] is not a function"
        ),
        other => panic!("unexpected state {other:?}"),
    }
    assert_eq!(fixture.report_types(), vec!["task_faulted"]);
}

/// `runtime.report(..., { type: "extension" })` 只带 extension 与 task，不带 conversationId
/// （TS: `{ type: "extension_error", task, ...optionalExtension(from.extension) }`）。
struct ReportExtension;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for ReportExtension {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime.report(
            &std::io::Error::other("hook failed"),
            Some(ReportFrom::Extension {
                extension: Some("test.ext".to_owned()),
            }),
        );
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

#[tokio::test]
async fn extension_reports_from_a_task_omit_the_conversation_id() {
    let definition = define(
        "test.tasks.report_extension",
        1,
        vec![("start", Arc::new(ReportExtension))],
    );
    let fixture = Fixture::open(vec![definition.clone()]).await;
    let conversation = conversation(&fixture.session).await;
    let id = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    fixture.wait_status(id, TaskStatus::Terminal).await;
    let reports = fixture.reports.lock().unwrap().clone();
    let extension: Vec<&ReportInfo> = reports
        .iter()
        .filter(|info| matches!(info, ReportInfo::ExtensionError { .. }))
        .collect();
    assert_eq!(extension.len(), 1, "reports: {reports:?}");
    match extension[0] {
        ReportInfo::ExtensionError {
            extension,
            task,
            conversation_id,
        } => {
            assert_eq!(extension.as_deref(), Some("test.ext"));
            assert_eq!(task.as_ref().map(|task| task.id), Some(id));
            assert_eq!(*conversation_id, None);
        }
        other => panic!("unexpected report {other:?}"),
    }
}

/// checkpoint 遥测只在 running 提交改变 phase 时记录；同一 phase 的其它字段变化不算。
struct StepPhase;

struct NextCheckpoint {
    attempt: i64,
}

#[async_trait]
impl TaskRuntimeChange for NextCheckpoint {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Running {
            checkpoint: json!({ "phase": "next", "attempt": self.attempt }),
        }))
    }
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for StepPhase {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let checkpoint = task.state.checkpoint();
        let phase = checkpoint
            .and_then(|checkpoint| checkpoint.get("phase"))
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let attempt = checkpoint
            .and_then(|checkpoint| checkpoint.get("attempt"))
            .and_then(JsonValue::as_i64)
            .unwrap_or(0);
        if phase == "next" && attempt >= 1 {
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
        } else {
            let attempt = if phase == "next" { attempt + 1 } else { 0 };
            runtime
                .commit(
                    &NextCheckpoint { attempt },
                    context,
                    CommitOptions::default(),
                )
                .await
        }
    }
}

#[tokio::test]
async fn checkpoint_events_record_phase_changes_only() {
    use kiana_telemetry::{AttributeValue, InMemoryTelemetryContext, TelemetryContext};

    let telemetry = Arc::new(InMemoryTelemetryContext::new());
    let adapter: Arc<dyn TelemetryContext> = Arc::clone(&telemetry) as Arc<dyn TelemetryContext>;
    let definition = define(
        "test.tasks.checkpoint_event",
        1,
        vec![
            ("start", Arc::new(StepPhase)),
            ("next", Arc::new(StepPhase)),
        ],
    );
    let fixture = Fixture::open_with_telemetry(vec![definition.clone()], adapter).await;
    let conversation = conversation(&fixture.session).await;
    let id = create(
        &fixture.session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    fixture.wait_status(id, TaskStatus::Terminal).await;

    let mut phases = Vec::new();
    for span in telemetry.get_spans() {
        for event in span.events {
            if event.name == "kiana.durable.checkpoint" {
                phases.push(event.attributes.get("phase").cloned());
            }
        }
    }
    assert_eq!(
        phases,
        vec![Some(AttributeValue::String("next".to_owned()))],
        "only the start -> next commit changes the phase"
    );
}

/// TS 的 `#reserve` 用 try/catch 包住整个 `commitWith`：回调返回后提交本身失败，也要回滚已创建的
/// 调用（从 `#invocations` 移除、放回 `#runnable`、`finish()`）。否则 `join()` 会等一个从未启动的
/// 调用。
#[tokio::test]
async fn a_failed_reservation_commit_rolls_back_created_invocations() {
    use std::time::Duration;

    let definition = define("test.tasks.reserve_failure", 1, vec![("start", complete())]);
    let (storage, session, _publications) = open_test_session();
    let conversation = conversation(&session).await;
    let id = create(
        &session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let fixture = Fixture::start_configured(
        Arc::clone(&storage),
        session,
        vec![definition],
        Arc::new(AtomicI64::new(1_000_000)),
        None,
        false,
        None,
    )
    .await;
    // 等 open 里那次（无写的）reconcile 提交静默。
    let mut last = fixture.storage.commits_len();
    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let current = fixture.storage.commits_len();
        if current == last {
            break;
        }
        last = current;
    }
    fixture
        .storage
        .fail_next_commit(Box::new(std::io::Error::other("storage down")));
    fixture.scheduler.resume().expect("resume succeeds");
    // 失败的提交必须确实是预留提交（回调创建了调用、把任务写成 running）。
    let mut attempted = false;
    for _ in 0..400 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let admitted = fixture
            .storage
            .admitted_commits
            .lock()
            .unwrap()
            .last()
            .cloned();
        if let Some(writes) = admitted
            && writes.iter().any(|write| {
                matches!(
                    write,
                    kiana_durable::StorageWrite::Task { value }
                        if value.id == id && value.state.status() == TaskStatus::Running
                )
            })
        {
            attempted = true;
            break;
        }
    }
    assert!(
        attempted,
        "the failed commit is the reservation commit that created the invocation"
    );
    // 回滚之后没有幽灵调用：join 立刻返回，而不是等一个从未启动的调用。
    let joined = tokio::time::timeout(Duration::from_millis(750), fixture.scheduler.join()).await;
    assert!(
        joined.is_ok(),
        "join returns after the failed reservation commit"
    );
    // 失败的预留让 Session 关闭。
    let _ = support::wait_closed(&fixture.session).await;
}

/// 只读 Harness：另一个进程驱动会话时，它只观察——不把 running 任务改回 pending、不增加
/// restart 计数、不调度、不写存储（TS: `harness-read-only.test.ts`）。
#[tokio::test]
async fn a_read_only_scheduler_observes_running_work_without_writing() {
    let definition = define("test.tasks.read_only", 1, vec![("start", noop())]);
    let definition_for_setup = definition.clone();
    let (fixture, id) = Fixture::open_read_only_with(vec![definition], move |session| async move {
        let conversation = conversation(&session).await;
        let id = create(
            &session,
            &definition_for_setup,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        // 另一个进程正跑到一半：running，已有两次 restart。
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut record = tx.task(id).await?.expect("task exists");
                record.state = TaskState::Running {
                    checkpoint: json!({ "phase": "start" }),
                };
                record.restarts = Some(2);
                tx.set_task(record)?;
                Ok(())
            })
        })
        .await
        .expect("mark running");
        id
    })
    .await;
    // 打开没有写存储。
    let record = fixture
        .storage
        .task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists");
    assert_eq!(record.state.status(), TaskStatus::Running);
    assert_eq!(record.restarts, Some(2));
    // 不调度。
    assert_eq!(
        fixture.scheduler.wake(),
        Wake {
            active: false,
            at: None
        }
    );
    let error = fixture
        .scheduler
        .resume()
        .expect_err("a read-only scheduler refuses to run tasks");
    assert_eq!(
        error.to_string(),
        "Read-only Harness: running tasks refused"
    );
    let inspection = fixture
        .scheduler
        .inspect(fixture.registry.snapshot())
        .await
        .expect("inspect succeeds");
    assert_eq!(inspection.scheduling, SchedulingState::Paused);
    assert_eq!(inspection.tasks.len(), 1);
    assert_eq!(inspection.tasks[0].record.id, id);
}

/// `inspect()` 派生活任务的调度状态而不运行任务代码（TS: `harness-inspect.test.ts` 第一例）。
struct GatePhase {
    gate: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for GatePhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.gate.notified().await;
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

/// 先等待它的 owner 列表（`on`），下一个 phase 完成。
struct WaitOncePhase {
    on: Arc<Mutex<Option<TaskId>>>,
}

struct WaitOn {
    on: TaskId,
}

#[async_trait]
impl TaskRuntimeChange for WaitOn {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Waiting {
            checkpoint: json!({ "phase": "done" }),
            on: vec![self.on],
            policy: JoinPolicy::AllSettled,
        }))
    }
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for WaitOncePhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let on = self.on.lock().unwrap().expect("gate id set during setup");
        runtime
            .commit(&WaitOn { on }, context, CommitOptions::default())
            .await
    }
}

#[tokio::test]
async fn inspect_derives_every_live_tasks_state_without_running_task_code() {
    use kiana_durable::harness::types::{BlockedReason, TaskInspectionState};

    let gate = Arc::new(tokio::sync::Notify::new());
    let gate_id: Arc<Mutex<Option<TaskId>>> = Arc::new(Mutex::new(None));
    let migrations = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let gate_task = define_with(
        "test.inspect.gate",
        1,
        vec![(
            "start",
            Arc::new(GatePhase {
                gate: Arc::clone(&gate),
            }),
        )],
        noop(),
        None,
    );
    let dependent = define_with(
        "test.inspect.dependent",
        1,
        vec![
            (
                "start",
                Arc::new(WaitOncePhase {
                    on: Arc::clone(&gate_id),
                }),
            ),
            ("done", complete()),
        ],
        noop(),
        None,
    );
    let migrations_for_run = Arc::clone(&migrations);
    let migrating = define_with(
        "test.inspect.migrating",
        2,
        vec![("start", complete())],
        noop(),
        Some(Arc::new(move |input, _checkpoint, _version| {
            migrations_for_run.fetch_add(1, Ordering::SeqCst);
            Ok((input, json!({ "phase": "start" })))
        })),
    );
    let failing = define_with(
        "test.inspect.failing",
        2,
        vec![("start", complete())],
        noop(),
        Some(Arc::new(|_input, _checkpoint, _version| {
            Err(std::io::Error::other("cannot migrate").into())
        })),
    );
    let no_migration = define_with(
        "test.inspect.no_migration",
        2,
        vec![("start", complete())],
        noop(),
        None,
    );
    let too_old = define_with(
        "test.inspect.too_old",
        1,
        vec![("start", complete())],
        noop(),
        None,
    );
    let registry_tasks = vec![
        gate_task.clone(),
        dependent.clone(),
        migrating.clone(),
        failing.clone(),
        no_migration.clone(),
        too_old.clone(),
    ];
    let stored = |name: &str| define(name, 1, vec![("start", complete())]);
    let gate_task_for_setup = gate_task.clone();
    let dependent_for_setup = dependent.clone();
    let too_old_for_setup = too_old.clone();
    let (fixture, ids) = Fixture::open_paused_with(registry_tasks, move |session| async move {
        let conversation = conversation(&session).await;
        let gate = create(
            &session,
            &gate_task_for_setup,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        *gate_id.lock().unwrap() = Some(gate);
        let dependent_id = create(
            &session,
            &dependent_for_setup,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        let migrating_id = create(
            &session,
            &stored("test.inspect.migrating"),
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        let no_migration_id = create(
            &session,
            &stored("test.inspect.no_migration"),
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        let failing_id = create(
            &session,
            &stored("test.inspect.failing"),
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        let too_old_id = create(
            &session,
            &define("test.inspect.too_old", 2, vec![("start", complete())]),
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        let missing = create(
            &session,
            &stored("test.inspect.missing"),
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        let _ = too_old_for_setup;
        (
            gate,
            dependent_id,
            migrating_id,
            no_migration_id,
            failing_id,
            too_old_id,
            missing,
        )
    })
    .await;
    let (
        gate_task_id,
        dependent_id,
        migrating_id,
        no_migration_id,
        failing_id,
        too_old_id,
        missing,
    ) = ids;

    let state_of = |inspection: &SchedulerInspection, id: TaskId| -> Option<TaskInspectionState> {
        inspection
            .tasks
            .iter()
            .find(|task| task.record.id == id)
            .map(|task| task.state.clone())
    };

    let paused = fixture
        .scheduler
        .inspect(fixture.registry.snapshot())
        .await
        .expect("inspect succeeds");
    assert_eq!(paused.scheduling, SchedulingState::Paused);
    assert_eq!(
        state_of(&paused, gate_task_id),
        Some(TaskInspectionState::Ready { migrates: false })
    );
    assert_eq!(
        state_of(&paused, dependent_id),
        Some(TaskInspectionState::Ready { migrates: false })
    );
    assert_eq!(
        state_of(&paused, migrating_id),
        Some(TaskInspectionState::Ready { migrates: true })
    );
    // 从未尝试的迁移不会为了解结果而运行。
    assert_eq!(
        state_of(&paused, failing_id),
        Some(TaskInspectionState::Ready { migrates: true })
    );
    assert_eq!(
        state_of(&paused, no_migration_id),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::MigrationFailed,
            error: Some(json!(
                "Task test.inspect.no_migration version 2 has no migration from 1"
            )),
        })
    );
    assert_eq!(
        state_of(&paused, too_old_id),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::TaskTooOld,
            error: None,
        })
    );
    assert_eq!(
        state_of(&paused, missing),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::MissingTask,
            error: None,
        })
    );
    assert_eq!(migrations.load(Ordering::SeqCst), 0);

    fixture.scheduler.resume().expect("resume succeeds");
    fixture
        .wait_status(migrating_id, TaskStatus::Terminal)
        .await;
    assert_eq!(migrations.load(Ordering::SeqCst), 1);

    // 等到 gate 在跑、dependent 在等它。
    let mut running = None;
    for _ in 0..400 {
        let inspection = fixture
            .scheduler
            .inspect(fixture.registry.snapshot())
            .await
            .expect("inspect succeeds");
        let gate_state = state_of(&inspection, gate_task_id);
        let dependent_state = state_of(&inspection, dependent_id);
        if gate_state == Some(TaskInspectionState::Running)
            && dependent_state
                == Some(TaskInspectionState::Waiting {
                    on: vec![gate_task_id],
                })
        {
            running = Some(inspection);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let running = running.expect("gate runs while the dependent waits");
    assert_eq!(running.scheduling, SchedulingState::Running);
    assert_eq!(
        state_of(&running, failing_id),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::MigrationFailed,
            error: Some(json!("cannot migrate")),
        })
    );
    assert_eq!(state_of(&running, migrating_id), None);

    gate.notify_one();
    fixture
        .wait_status(gate_task_id, TaskStatus::Terminal)
        .await;
    fixture
        .wait_status(dependent_id, TaskStatus::Terminal)
        .await;
    let settled = fixture
        .scheduler
        .inspect(fixture.registry.snapshot())
        .await
        .expect("inspect succeeds");
    let remaining: Vec<TaskId> = settled.tasks.iter().map(|task| task.record.id).collect();
    let mut expected = vec![no_migration_id, failing_id, too_old_id, missing];
    expected.sort();
    let mut remaining_sorted = remaining.clone();
    remaining_sorted.sort();
    assert_eq!(remaining_sorted, expected);
}

/// TS 的 `waitForTask()` 把 Storage 读到的记录直接 `as SettledTask` 返回，不校验状态：
/// 另一个 Session（另一个进程）写下的非终态记录也原样解析，而不是 panic。
#[tokio::test]
async fn wait_for_task_resolves_a_record_another_session_wrote_unchecked() {
    let fixture = Fixture::open(Vec::new()).await;
    // 第二条 Session 共用同一份 Storage：它写下的记录不会被本调度器观察到（跨进程场景）。
    let other = SessionImpl::new(
        Arc::clone(&fixture.storage) as Arc<dyn Storage>,
        SessionOptions::default(),
    );
    let task = define("test.tasks.foreign", 1, vec![("start", complete())]);
    let conversation = conversation(&other).await;
    let id = create(
        &other,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let settled = fixture
        .scheduler
        .wait_for_task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("a stored record resolves as-is");
    assert_eq!(settled.record().id, id);
    assert_eq!(settled.record().state.status(), TaskStatus::Pending);
}

/// TS 的 `Checkpoint` 类型写着 `phase: string`，但任务提交的 checkpoint 不做校验：数字 `1`
/// 在 `phases[checkpoint.phase]` 的属性键强制下命中键 `"1"` 的处理器（`String(1)` 语义）。
#[tokio::test]
async fn a_numeric_checkpoint_phase_selects_the_coerced_handler() {
    let phases: IndexMap<String, Phase> = [("1".to_owned(), complete())].into_iter().collect();
    let definition: Def = TaskDefinition {
        name: "test.tasks.numeric_phase".to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": 1 })),
        phases,
        abort: noop(),
        migrate: None,
        hooks: None,
    };
    let fixture = Fixture::open(vec![erase_task(&definition)]).await;
    let conversation = conversation(&fixture.session).await;
    let id = create(
        &fixture.session,
        &erase_task(&definition),
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    assert!(matches!(
        record.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed { .. }
        }
    ));
}
