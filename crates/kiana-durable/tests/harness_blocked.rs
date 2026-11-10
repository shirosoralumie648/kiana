//! `test/harness-blocked.test.ts` 的 Rust 端口（调度器可直测部分）。
//!
//! 已逐用例端口（用同一存储上的新 `SessionImpl` + `TaskScheduler` 模拟崩溃重开）：
//! - "does not count the same checkpoint committed again as progress"
//! - "starts the count over when the task makes progress"
//!
//! 调度器级场景：blocked 判定（缺失定义 / 停泊）、restart 上限停泊与 `retry`、abort handler
//! 被切断的报告、`inspect`，以及宿主 settings 回调可重入调度器（TS 单线程无锁）。
//! 依赖 Harness 便利句柄、blocked 图订阅与 close/writeAtClose 的用例见报告的 remainingGaps。
#![allow(dead_code)]

mod support;

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{CreateModelsOptions, Models, ModelsImpl};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::scheduler::{
    AgentResolver, InvocationBinding, SettingsResolver, TaskScheduler, TaskSchedulerOptions,
    erase_task,
};
use kiana_durable::harness::types::{
    Agent, AnyTask, ConversationHandle, Extension, RegistryReader, RegistrySection,
    RegistrySnapshot, RegistryTool, TaskInspectionState, TaskRuntime, TaskRuntimeChange,
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

/// 调度器构造完成、`open()` 之前的观察钩子（测试在调用前把它放进共享槽）。
type PreOpenHook = Arc<dyn Fn(&TaskScheduler)>;

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
        Fixture::start_with(storage, session, tasks, clock, None, None, true).await
    }

    /// 带 `settings.restartLimit` 的打开（TS: `HarnessOptions.settings`）。
    async fn open_with_settings<T, F, Fut>(
        tasks: Vec<AnyTask>,
        restart_limit: f64,
        setup: F,
    ) -> (Fixture, T)
    where
        F: FnOnce(SessionImpl) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let (storage, session, _publications) = open_test_session();
        let extra = setup(session.clone()).await;
        let settings: SettingsResolver = Arc::new(move || {
            let mut settings = resolve_settings(None);
            settings.restart_limit = restart_limit;
            Ok(Arc::new(settings))
        });
        let fixture = Fixture::start_with(
            storage,
            session,
            tasks,
            Arc::new(AtomicI64::new(1_000_000)),
            Some(settings),
            None,
            true,
        )
        .await;
        (fixture, extra)
    }

    /// `resume` 为 false 时只 `open()`：TS 的 `Harness.open()` 不启用调度，测试自己调 `resume()`。
    async fn start_with(
        storage: Arc<support::ControlledStorage>,
        session: SessionImpl,
        tasks: Vec<AnyTask>,
        clock: Arc<AtomicI64>,
        settings: Option<SettingsResolver>,
        before_open: Option<PreOpenHook>,
        resume: bool,
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
            settings: settings.unwrap_or_else(|| Arc::new(|| Ok(Arc::new(resolve_settings(None))))),
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
        if let Some(before_open) = before_open {
            before_open(&scheduler);
        }
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
use kiana_durable::harness::types::{BlockedReason, BlockedSubject};

#[tokio::test]
async fn blocks_work_whose_definition_is_missing() {
    let fixture = Fixture::open(Vec::new()).await;
    let conversation = conversation(&fixture.session).await;
    let task = define("test.blocked.missing", 1, vec![("start", complete())]);
    let id = create(
        &fixture.session,
        &task,
        conversation,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let record = fixture
        .storage
        .task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists");
    assert_eq!(record.state.status(), TaskStatus::Pending);
    let subject = BlockedSubject {
        id: record.id,
        kind: record.kind.clone(),
        version: record.version,
        abort_requested: record.abort_requested,
        abort_reason: record.abort_reason,
        restarts: record.restarts,
        status: record.state.status(),
    };
    assert_eq!(
        fixture.scheduler.blocked_reason(&subject),
        Some(BlockedReason::MissingTask)
    );
}

#[tokio::test]
async fn parks_a_task_a_crash_interrupted_at_the_restart_limit() {
    let definition = define("test.blocked.restart", 1, vec![("start", complete())]);
    let setup_task = definition.clone();
    let (fixture, id) = Fixture::open_with(vec![definition], move |session| async move {
        let conversation = conversation(&session).await;
        let id = create(
            &session,
            &setup_task,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut record = tx.task(id).await?.expect("task exists");
                record.state = TaskState::Running {
                    checkpoint: json!({ "phase": "start" }),
                };
                record.restarts = Some(4);
                record.started_at = Some(1);
                tx.set_task(record)?;
                Ok(())
            })
        })
        .await
        .expect("write the interrupted run");
        id
    })
    .await;
    let record = fixture
        .storage
        .task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists");
    assert_eq!(record.state.status(), TaskStatus::Pending);
    assert_eq!(record.restarts, Some(5));
    let subject = BlockedSubject {
        id: record.id,
        kind: record.kind.clone(),
        version: record.version,
        abort_requested: record.abort_requested,
        abort_reason: record.abort_reason,
        restarts: record.restarts,
        status: record.state.status(),
    };
    assert_eq!(
        fixture.scheduler.blocked_reason(&subject),
        Some(BlockedReason::RestartLimit)
    );
    let reports = fixture.report_types();
    assert!(reports.contains(&"task_parked"), "reports: {reports:?}");
}

#[tokio::test]
async fn retry_unparks_a_parked_task_for_one_more_run() {
    let definition = define("test.blocked.retry", 1, vec![("start", complete())]);
    let setup_task = definition.clone();
    let (fixture, id) = Fixture::open_with(vec![definition], move |session| async move {
        let conversation = conversation(&session).await;
        let id = create(
            &session,
            &setup_task,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut record = tx.task(id).await?.expect("task exists");
                record.state = TaskState::Running {
                    checkpoint: json!({ "phase": "start" }),
                };
                record.restarts = Some(9);
                tx.set_task(record)?;
                Ok(())
            })
        })
        .await
        .expect("write the interrupted run");
        id
    })
    .await;
    fixture
        .scheduler
        .retry(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("retry succeeds");
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    assert!(matches!(
        record.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed { .. }
        }
    ));
}

/// abort handler 的 restart 上限文案逐字内插 `record.restarts`：没有计数时 TS 输出 `undefined`。
#[tokio::test]
async fn an_abort_handler_cut_off_without_a_restart_count_says_undefined() {
    let definition = define("test.blocked.abort_limit", 1, vec![("start", complete())]);
    let definition_for_setup = definition.clone();
    let (fixture, id) = Fixture::open_with_settings(Vec::new(), 0.0, move |session| async move {
        let conversation = conversation(&session).await;
        let id = create(
            &session,
            &definition_for_setup,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        // 只加 abort 标记，没有 restart 计数。
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut record = tx.task(id).await?.expect("task exists");
                record.abort_requested = true;
                record.abort_reason = None;
                record.restarts = None;
                tx.set_task(record)?;
                Ok(())
            })
        })
        .await
        .expect("mark task");
        id
    })
    .await;
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Faulted { error },
        } => assert_eq!(
            error.message,
            format!(
                "Abort handler of task {id} (test.blocked.abort_limit) was cut off undefined times"
            )
        ),
        other => panic!("unexpected state {other:?}"),
    }
    assert_eq!(fixture.report_types(), vec!["task_faulted"]);
}

/// TS 是单线程无锁的：宿主 `settings` 回调里重入调度器（`wake()`、`indexSizes()`）是安全的。
/// Rust 用 `std::sync::Mutex` 保护索引，任何在持锁时调用宿主回调的路径都会自死锁；这里让
/// settings 回调每次读取都重入，并用看门狗线程把回归的死锁变成一次明确的失败。
#[tokio::test]
async fn a_settings_callback_may_reenter_the_scheduler() {
    let definition = define("test.blocked.reenter", 1, vec![("start", complete())]);
    let setup_task = definition.clone();
    let (storage, session, _publications) = open_test_session();
    let id = {
        let conversation = conversation(&session).await;
        let id = create(
            &session,
            &setup_task,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        commit(&session, move |tx| {
            Box::pin(async move {
                let mut record = tx.task(id).await?.expect("task exists");
                record.state = TaskState::Running {
                    checkpoint: json!({ "phase": "start" }),
                };
                record.restarts = Some(4);
                record.started_at = Some(1);
                tx.set_task(record)?;
                Ok(())
            })
        })
        .await
        .expect("write the interrupted run");
        id
    };
    let slot: Arc<Mutex<Option<TaskScheduler>>> = Arc::new(Mutex::new(None));
    let slot_in = Arc::clone(&slot);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in = Arc::clone(&calls);
    let settings: SettingsResolver = Arc::new(move || {
        calls_in.fetch_add(1, Ordering::SeqCst);
        if let Some(scheduler) = slot_in.lock().unwrap().clone() {
            // 只读索引：与 `#unparkRaised` 里的 settings 读取重入同一把状态锁。
            let _ = scheduler.index_sizes();
        }
        Ok(Arc::new(resolve_settings(None)))
    });
    let slot_hook = Arc::clone(&slot);
    let hook: PreOpenHook = Arc::new(move |scheduler| {
        *slot_hook.lock().unwrap() = Some(scheduler.clone());
    });
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if done_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .is_err()
        {
            eprintln!("deadlock: a host settings callback blocked inside the scheduler");
            std::process::abort();
        }
    });
    let fixture = Fixture::start_with(
        storage,
        session,
        vec![definition],
        Arc::new(AtomicI64::new(1_000_000)),
        Some(settings),
        Some(hook),
        true,
    )
    .await;
    // 打开时它已被停泊；再 kick 几次 reserve，让 `#unparkRaised` 在已停泊任务上读 settings。
    for _ in 0..5 {
        fixture.scheduler.resume().expect("resume succeeds");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        calls.load(Ordering::SeqCst) > 0,
        "the settings callback ran"
    );
    // 调度器仍然有响应：宿主回调没有在持锁时运行。
    let sizes = fixture.scheduler.index_sizes();
    assert!(sizes.live >= 1, "sizes: {sizes:?}");
    let _ = done_tx.send(());
    let _ = id;
}

// ---------------------------------------------------------------------------
// `harness-blocked.test.ts`：restart 计数语义（逐用例端口）
// ---------------------------------------------------------------------------

/// 初始 checkpoint 指向 `initial_phase` 的定义（本文件的 `define_with` 固定从 `start` 开始）。
fn define_starting_at(
    name: &str,
    version: i64,
    initial_phase: &str,
    phases: Vec<(&str, Phase)>,
    abort: Phase,
) -> AnyTask {
    let phases: IndexMap<String, Phase> = phases
        .into_iter()
        .map(|(phase, handler)| (phase.to_owned(), handler))
        .collect();
    let initial = initial_phase.to_owned();
    let definition: Def = TaskDefinition {
        name: name.to_owned(),
        version,
        initial: Arc::new(move |_| json!({ "phase": initial })),
        phases,
        abort,
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
}

fn settings_with_restart_limit(limit: f64) -> SettingsResolver {
    Arc::new(move || {
        let mut settings = resolve_settings(None);
        settings.restart_limit = limit;
        Ok(Arc::new(settings))
    })
}

async fn read_record(storage: &support::ControlledStorage, id: TaskId) -> TaskRecord {
    storage
        .task(id, &**BACKGROUND_CONTEXT)
        .await
        .expect("read succeeds")
        .expect("task exists")
}

async fn wait_started(started: &Arc<AtomicUsize>, wanted: usize) {
    for _ in 0..600 {
        if started.load(Ordering::SeqCst) == wanted {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "started {} invocations, wanted {wanted}",
        started.load(Ordering::SeqCst)
    );
}

/// 效果前的意图提交：把当前 checkpoint 原样再提交一次。
struct SameCheckpoint {
    checkpoint: JsonValue,
}

#[async_trait]
impl TaskRuntimeChange for SameCheckpoint {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Running {
            checkpoint: self.checkpoint.clone(),
        }))
    }
}

struct IntentPhase {
    started: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for IntentPhase {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        // 效果前的意图，然后效果永不返回：checkpoint 没变。
        let checkpoint = task.state.checkpoint().cloned().unwrap_or(JsonValue::Null);
        runtime
            .commit(
                &SameCheckpoint { checkpoint },
                context,
                CommitOptions::default(),
            )
            .await?;
        runtime.signal().token().cancelled().await;
        Ok(())
    }
}

/// TS: “does not count the same checkpoint committed again as progress”。
#[tokio::test]
async fn does_not_count_the_same_checkpoint_committed_again_as_progress() {
    let started = Arc::new(AtomicUsize::new(0));
    let definition = define_starting_at(
        "test.blocked.intent",
        1,
        "run",
        vec![(
            "run",
            Arc::new(IntentPhase {
                started: Arc::clone(&started),
            }) as Phase,
        )],
        noop(),
    );
    let (storage, session, _publications) = open_test_session();
    let conversation = conversation(&session).await;
    let id = create(
        &session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!(null),
    )
    .await;
    let settings = settings_with_restart_limit(2.0);
    let clock = || Arc::new(AtomicI64::new(1_000_000));
    let mut opened = Fixture::start_with(
        Arc::clone(&storage),
        session,
        vec![definition.clone()],
        clock(),
        Some(Arc::clone(&settings)),
        None,
        true,
    )
    .await;
    for run in 1..=2 {
        opened.scheduler.resume().expect("resume succeeds");
        wait_started(&started, run).await;
        // 等同一 checkpoint 的 running 提交写下。
        for _ in 0..600 {
            if read_record(&opened.storage, id).await.state.status() == TaskStatus::Running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // 崩溃：不 close，直接在同一条存储上开新的 Session 与调度器。
        let next = SessionImpl::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            SessionOptions::default(),
        );
        opened = Fixture::start_with(
            Arc::clone(&storage),
            next,
            vec![definition.clone()],
            clock(),
            Some(Arc::clone(&settings)),
            None,
            true,
        )
        .await;
    }
    let record = read_record(&opened.storage, id).await;
    assert_eq!(record.restarts, Some(2), "record: {record:?}");
    let inspection = opened
        .scheduler
        .inspect(opened.registry.snapshot())
        .await
        .expect("inspect succeeds");
    let state = inspection
        .tasks
        .iter()
        .find(|task| task.record.id == id)
        .map(|task| task.state.clone());
    assert!(
        matches!(
            state,
            Some(TaskInspectionState::Blocked {
                reason: BlockedReason::RestartLimit,
                ..
            })
        ),
        "state: {state:?}"
    );
}

struct StepOne {
    started: Arc<AtomicUsize>,
    crashing: Arc<AtomicBool>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for StepOne {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        if self.crashing.load(Ordering::SeqCst) {
            runtime.signal().token().cancelled().await;
        } else {
            runtime
                .commit(&NextStep, context, CommitOptions::default())
                .await?;
        }
        Ok(())
    }
}

struct NextStep;

#[async_trait]
impl TaskRuntimeChange for NextStep {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Running {
            checkpoint: json!({ "phase": "two" }),
        }))
    }
}

struct StepTwo {
    started: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for StepTwo {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        runtime.signal().token().cancelled().await;
        Ok(())
    }
}

/// TS: “starts the count over when the task makes progress”。
#[tokio::test]
async fn starts_the_count_over_when_the_task_makes_progress() {
    let started = Arc::new(AtomicUsize::new(0));
    let crashing = Arc::new(AtomicBool::new(true));
    let definition = define_starting_at(
        "test.blocked.steps",
        1,
        "one",
        vec![
            (
                "one",
                Arc::new(StepOne {
                    started: Arc::clone(&started),
                    crashing: Arc::clone(&crashing),
                }) as Phase,
            ),
            (
                "two",
                Arc::new(StepTwo {
                    started: Arc::clone(&started),
                }) as Phase,
            ),
        ],
        noop(),
    );
    let (storage, session, _publications) = open_test_session();
    let conversation = conversation(&session).await;
    let id = create(
        &session,
        &definition,
        conversation,
        TaskOwnership::Conversation,
        json!(null),
    )
    .await;
    let settings = settings_with_restart_limit(2.0);
    let clock = || Arc::new(AtomicI64::new(1_000_000));
    let mut opened = Fixture::start_with(
        Arc::clone(&storage),
        session,
        vec![definition.clone()],
        clock(),
        Some(Arc::clone(&settings)),
        None,
        false,
    )
    .await;
    opened.scheduler.resume().expect("resume succeeds");
    wait_started(&started, 1).await;
    // 崩溃：不 close，重开（open 计一次 restart），但不 resume。
    let next = SessionImpl::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        SessionOptions::default(),
    );
    opened = Fixture::start_with(
        Arc::clone(&storage),
        next,
        vec![definition.clone()],
        clock(),
        Some(Arc::clone(&settings)),
        None,
        false,
    )
    .await;
    let record = read_record(&opened.storage, id).await;
    assert_eq!(record.restarts, Some(1), "record: {record:?}");
    // 进展：phase one 提交 phase two，计数清零。
    crashing.store(false, Ordering::SeqCst);
    opened.scheduler.resume().expect("resume succeeds");
    wait_started(&started, 3).await;
    let mut record = read_record(&opened.storage, id).await;
    for _ in 0..600 {
        if record.state.status() == TaskStatus::Running
            && record.state.checkpoint() == Some(&json!({ "phase": "two" }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        record = read_record(&opened.storage, id).await;
    }
    assert_eq!(
        record.state.checkpoint(),
        Some(&json!({ "phase": "two" })),
        "record: {record:?}"
    );
    assert_eq!(record.restarts, None, "record: {record:?}");
}

// ---------------------------------------------------------------------------
// 回归：宿主 settings getter 抛错必须传进 phase（TS: `get settings() { return settings(); }`）
// ---------------------------------------------------------------------------

/// 读取一次 `runtime.settings()` 并让错误像 TS 的 throw 一样从 phase 逸出。
struct ReadSettings;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for ReadSettings {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        let _ = runtime.settings()?;
        Ok(())
    }
}

#[tokio::test]
async fn a_failing_settings_provider_faults_the_task() {
    let definition = define(
        "test.settings.failure",
        1,
        vec![("start", Arc::new(ReadSettings))],
    );
    let setup = definition.clone();
    let (storage, session, _publications) = open_test_session();
    let conversation_id = conversation(&session).await;
    let id = create(
        &session,
        &setup,
        conversation_id,
        TaskOwnership::Conversation,
        json!({}),
    )
    .await;
    let settings: SettingsResolver =
        Arc::new(|| Err(Box::new(std::io::Error::other("settings unavailable")) as BoxError));
    let fixture = Fixture::start_with(
        storage,
        session,
        vec![definition],
        Arc::new(AtomicI64::new(1_000_000)),
        Some(settings),
        None,
        true,
    )
    .await;
    let record = fixture.wait_status(id, TaskStatus::Terminal).await;
    match record.state {
        TaskState::Terminal {
            outcome: TaskOutcome::Faulted { error },
        } => assert_eq!(error.message, "settings unavailable"),
        other => panic!("unexpected state {other:?}"),
    }
}
