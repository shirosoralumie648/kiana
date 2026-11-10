//! `test/harness-sleep.test.ts` 的 Rust 端口（调度器可直测部分）。
//!
//! TS 测试用假 Harness 时钟驱动 `sleepUntil`；这里用调度器的 `now` 注入与 `resume()` 唤醒同一套 sleeping/notDue 语义，并覆盖 abort 结束睡眠、abort handler 不能睡眠与非法时间点的逐字错误。依赖 Harness 计时器宿主回调的用例见报告的 remainingGaps；`NaN`/`Infinity` 无法用冻结的 `until: i64` 表达。
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
    Agent, AnyTask, ConversationHandle, Extension, RegistryReader, RegistrySection,
    RegistrySnapshot, RegistryTool, TaskRuntime, TaskRuntimeChange,
};
use kiana_durable::session::{SessionImpl, SessionOptions, Transaction};
use kiana_durable::timers::host_timers;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, JsonValue, ReportInfo, Storage, TaskId, TaskOptions,
    TaskOutcome, TaskOwnership, TaskRecord, TaskState, TaskStatus, Tx,
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

struct SleepStart;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for SleepStart {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let until = runtime.now() + 1_000;
        runtime
            .commit(&SleepUntil { until }, context, CommitOptions::default())
            .await
    }
}

struct SleepUntil {
    until: i64,
}
#[async_trait]
impl TaskRuntimeChange for SleepUntil {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Sleeping {
            checkpoint: json!({ "phase": "later" }),
            until: self.until,
        }))
    }
}

struct SleepDone;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for SleepDone {
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
                        result: json!({ "woke": true }),
                    },
                }),
                context,
                CommitOptions::default(),
            )
            .await
    }
}

struct AbortedSleep;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortedSleep {
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
                        reason: Some("cancelled".to_owned()),
                        result: None,
                    },
                }),
                context,
                CommitOptions::default(),
            )
            .await
    }
}

/// 在 abort handler 里试图睡眠，把错误留给测试。
struct SleepingAbort {
    error: Arc<Mutex<Option<String>>>,
}
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for SleepingAbort {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let result = runtime
            .sleep_until(runtime.now() + 1_000, json!({}), context)
            .await;
        *self.error.lock().unwrap() = result.err().map(|error| error.to_string());
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

fn sleeper(name: &str) -> AnyTask {
    define_with(
        name,
        1,
        vec![
            ("start", Arc::new(SleepStart)),
            ("later", Arc::new(SleepDone)),
        ],
        Arc::new(AbortedSleep),
        None,
    )
}

async fn status(fixture: &Fixture, id: TaskId) -> TaskStatus {
    fixture
        .storage
        .task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists")
        .state
        .status()
}

#[tokio::test]
async fn sleep_until_waits_for_the_harness_clock() {
    let fixture = Fixture::open(vec![sleeper("test.sleep.wait")]).await;
    let conversation = conversation(&fixture.session).await;
    let task = sleeper("test.sleep.wait");
    let id = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    for _ in 0..600 {
        if status(&fixture, id).await == TaskStatus::Sleeping {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(status(&fixture, id).await, TaskStatus::Sleeping);
    let record = fixture
        .storage
        .task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("read")
        .expect("exists");
    assert_eq!(
        record.state,
        TaskState::Sleeping {
            checkpoint: json!({ "phase": "later" }),
            until: 1_001_000,
        }
    );
    // 时钟还没到：resume 不会把它弄醒。
    fixture.scheduler.resume().expect("resume succeeds");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(status(&fixture, id).await, TaskStatus::Sleeping);
    fixture.clock.fetch_add(2_000, Ordering::SeqCst);
    fixture.scheduler.resume().expect("resume succeeds");
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Completed { result },
        } => assert_eq!(result, json!({ "woke": true })),
        other => panic!("unexpected state {other:?}"),
    }
}

#[tokio::test]
async fn abort_ends_a_sleeping_task_through_its_handler() {
    let fixture = Fixture::open(vec![sleeper("test.sleep.abort")]).await;
    let conversation = conversation(&fixture.session).await;
    let task = sleeper("test.sleep.abort");
    let id = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    for _ in 0..600 {
        if status(&fixture, id).await == TaskStatus::Sleeping {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    fixture
        .scheduler
        .abort(id, &**BACKGROUND_CONTEXT, false)
        .await
        .expect("abort succeeds");
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    assert!(matches!(
        record.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Aborted { .. }
        }
    ));
}

#[tokio::test]
async fn an_abort_handler_cannot_sleep() {
    let error = Arc::new(Mutex::new(None));
    let definition = define_with(
        "test.sleep.abort_error",
        1,
        vec![
            ("start", Arc::new(SleepStart)),
            ("later", Arc::new(SleepDone)),
        ],
        Arc::new(SleepingAbort {
            error: Arc::clone(&error),
        }),
        None,
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
    for _ in 0..600 {
        if status(&fixture, id).await == TaskStatus::Sleeping {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    fixture
        .scheduler
        .abort(id, &**BACKGROUND_CONTEXT, false)
        .await
        .expect("abort succeeds");
    fixture.wait_status(id, TaskStatus::Terminal).await;
    assert_eq!(
        error.lock().unwrap().clone(),
        Some(format!("Abort handler of task {id} cannot sleep"))
    );
}

/// `wake()` 报告最早的睡眠 deadline 且不算作 active（TS: `harness-sleep.test.ts`
/// "counts neither a wait on a sleeping task ... as active" 的睡眠侧）。
#[tokio::test]
async fn wake_reports_the_earliest_sleep_deadline_without_being_active() {
    use kiana_durable::harness::types::Wake;

    let fixture = Fixture::open(vec![sleeper("test.sleep.wake")]).await;
    let conversation = conversation(&fixture.session).await;
    let task = sleeper("test.sleep.wake");
    let id = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    for _ in 0..600 {
        if status(&fixture, id).await == TaskStatus::Sleeping {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(status(&fixture, id).await, TaskStatus::Sleeping);
    assert_eq!(
        fixture.scheduler.wake(),
        Wake {
            active: false,
            at: Some(1_001_000),
        }
    );
}

/// 没有定义的任务不能被运行，也不算 active（TS: "does not count a task without a definition as
/// active"）。
#[tokio::test]
async fn wake_ignores_a_task_without_a_definition() {
    use kiana_durable::harness::types::Wake;

    let fixture = Fixture::open(Vec::new()).await;
    let conversation = conversation(&fixture.session).await;
    let task = sleeper("test.sleep.missing");
    let _id = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    for _ in 0..100 {
        if fixture.scheduler.wake()
            == (Wake {
                active: false,
                at: None,
            })
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        fixture.scheduler.wake(),
        Wake {
            active: false,
            at: None,
        }
    );
    fixture.scheduler.resume().expect("resume succeeds");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        fixture.scheduler.wake(),
        Wake {
            active: false,
            at: None,
        }
    );
}

/// 在 phase 里尝试一个非法时间点，把错误留给测试（TS: “rejects a sleep until something
/// that is not a time, and a sleep in an abort handler” 的非法时间点部分）。
struct InvalidSleep {
    until: i64,
    error: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for InvalidSleep {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let result = runtime
            .sleep_until(self.until, json!({ "phase": "later" }), context)
            .await;
        *self.error.lock().unwrap() = result.err().map(|error| error.to_string());
        runtime
            .commit(
                &TerminalChange(TaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: JsonValue::Null,
                    },
                }),
                context,
                CommitOptions::default(),
            )
            .await
    }
}

/// TS 还覆盖 `NaN` 与 `Infinity`：Rust 的 `sleep_until(until: i64)`（§4.9 冻结的签名）
/// 无法表示非有限值，这里只覆盖负数（同样的逐字消息）。
#[tokio::test]
async fn rejects_a_sleep_until_something_that_is_not_a_time() {
    let error = Arc::new(Mutex::new(None));
    let definition = define_with(
        "test.sleep.invalid",
        1,
        vec![(
            "start",
            Arc::new(InvalidSleep {
                until: -1,
                error: Arc::clone(&error),
            }),
        )],
        Arc::new(AbortedSleep),
        None,
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
    assert_eq!(
        error.lock().unwrap().clone(),
        Some(format!(
            "Task {id} cannot sleep until -1: not a time in milliseconds"
        ))
    );
}
