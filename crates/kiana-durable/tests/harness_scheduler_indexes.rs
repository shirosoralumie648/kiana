//! `test/harness-scheduler-indexes.test.ts` 的 Rust 端口。
//!
//! 已逐用例端口（驱动 `TaskScheduler` + `SessionImpl`，不需要完整 `Harness`）：
//! - "takes a cascade's reason from the nearest cancelling owner"
//! - "finalizes a chain of held owners in the commit that frees the last one"
//! - "lists the live tasks a task owns directly, in ID order, also after a reopen"
//!
//! 其余两个用例依赖本单元无法提供的 Harness/存储闸门，见报告的 remainingGaps：
//! - "keeps a scope busy after reopen for work below an ended task whose chain is not loaded yet"
//!   （需要扣住 `storage.conversation()` 的读，让链保持未载入）
//! - "rejects once its invocation has ended"（需要把 `&dyn TaskRuntime` 带出 phase 存活期）
//!
//! 另有两条调度器级用例不对应 TS 用例：索引归零、blocked 订阅的 Set 语义。
#![allow(dead_code)]

mod support;

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
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
    BoxError, CommitOptions, ConversationId, ConversationOwnership, CreateConversationOptions,
    JsonValue, ReportInfo, Storage, StorageWrite, TaskAbortReason, TaskId, TaskOptions,
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

use kiana_durable::harness::types::{BlockedReason, SchedulingState, TaskInspectionState};

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
async fn releases_every_ownership_index_once_the_last_live_work_ends() {
    let complete_definition = define("test.indexes.complete", 1, vec![("start", complete())]);
    let missing_definition = define("test.indexes.missing", 1, vec![("start", complete())]);
    let registry_tasks = vec![complete_definition.clone()];
    let setup_complete = complete_definition.clone();
    let setup_missing = missing_definition.clone();
    let (fixture, (conversation, first, second, blocked)) =
        Fixture::open_with(registry_tasks, move |session| async move {
            let conversation = conversation(&session).await;
            let first = create(
                &session,
                &setup_complete,
                conversation,
                TaskOwnership::Conversation,
                json!({}),
            )
            .await;
            let second = create(
                &session,
                &setup_complete,
                conversation,
                TaskOwnership::Conversation,
                json!({}),
            )
            .await;
            let blocked = create(
                &session,
                &setup_missing,
                conversation,
                TaskOwnership::Conversation,
                json!({}),
            )
            .await;
            (conversation, first, second, blocked)
        })
        .await;
    fixture.wait_status(first, TaskStatus::Terminal).await;
    fixture.wait_status(second, TaskStatus::Terminal).await;
    // 阻塞的任务仍活着：只有它保持在索引里。
    let mut sizes = fixture.scheduler.index_sizes();
    for _ in 0..600 {
        sizes = fixture.scheduler.index_sizes();
        if sizes.live == 1 && sizes.settled <= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(sizes.live, 1, "sizes: {sizes:?}");
    assert!(sizes.settled <= 2, "sizes: {sizes:?}");
    assert_eq!(sizes.below, 1, "sizes: {sizes:?}");
    assert_eq!(sizes.roots, 1, "sizes: {sizes:?}");
    assert_eq!(sizes.unloaded, 0, "sizes: {sizes:?}");
    assert_eq!(sizes.drop_queue, 0, "sizes: {sizes:?}");
    let inspection = fixture
        .scheduler
        .inspect(fixture.registry.snapshot())
        .await
        .expect("inspect succeeds");
    assert_eq!(inspection.scheduling, SchedulingState::Running);
    assert_eq!(inspection.tasks.len(), 1);
    assert_eq!(inspection.tasks[0].record.id, blocked);
    assert_eq!(
        inspection.tasks[0].state,
        TaskInspectionState::Blocked {
            reason: BlockedReason::MissingTask,
            error: None,
        }
    );
    // 没有可接管定义的 abort 直接把它落定为 orphaned。
    fixture
        .scheduler
        .abort(blocked, &**BACKGROUND_CONTEXT, false)
        .await
        .expect("abort succeeds");
    fixture.wait_status(blocked, TaskStatus::Terminal).await;
    let mut empty = fixture.scheduler.index_sizes();
    for _ in 0..600 {
        empty = fixture.scheduler.index_sizes();
        if empty.live == 0 && empty.below == 0 && empty.roots == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(empty.live, 0, "sizes: {empty:?}");
    assert_eq!(empty.below, 0, "sizes: {empty:?}");
    assert_eq!(empty.roots, 0, "sizes: {empty:?}");
    assert_eq!(empty.edges, 0, "sizes: {empty:?}");
    assert_eq!(empty.unloaded, 0, "sizes: {empty:?}");
    assert_eq!(empty.drop_queue, 0, "sizes: {empty:?}");
    assert!(empty.settled <= 3, "sizes: {empty:?}");
    let _ = conversation;
}

/// TS 用 `Set` 保存 blocked 监听器：同一监听器重复注册是空操作，任一退订都移除它。
#[tokio::test]
async fn blocked_subscriptions_behave_like_a_set() {
    let fixture = Fixture::open(Vec::new()).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let listener: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let notify = |fixture: &Fixture| {
        let listeners = fixture.registry.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener();
        }
    };

    let first = fixture.scheduler.subscribe_blocked(Arc::clone(&listener));
    let second = fixture.scheduler.subscribe_blocked(Arc::clone(&listener));
    notify(&fixture);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a duplicate subscription is a no-op"
    );
    first();
    notify(&fixture);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one unsubscribe removes the listener"
    );
    second();
    notify(&fixture);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// `harness-scheduler-indexes.test.ts` 逐用例端口：门控 phase、所有权树与索引可见行为
// ---------------------------------------------------------------------------

/// 一个可打开一次的门（TS: `deferred<void>`；打开后一直保持打开）。
#[derive(Default)]
struct Gate {
    open: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl Gate {
    fn new() -> Arc<Gate> {
        Arc::new(Gate::default())
    }

    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

/// 按名字取门（TS 测试里的 `gates` Map）。
type Gates = Arc<Mutex<IndexMap<String, Arc<Gate>>>>;

fn gates() -> Gates {
    Arc::new(Mutex::new(IndexMap::new()))
}

fn gate(gates: &Gates, name: &str) -> Arc<Gate> {
    let mut gates = gates.lock().unwrap();
    Arc::clone(gates.entry(name.to_owned()).or_default())
}

fn task_name(task: &TaskRecord) -> String {
    task.input
        .get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned()
}

/// 等到 `name` 门打开或调用被中止（TS: `Promise.race([gate.promise, aborted(signal)])`）。
async fn wait_gate_or_abort(gates: &Gates, name: &str, signal: &kiana_chord::context::AbortSignal) {
    let gate = gate(gates, name);
    tokio::select! {
        _ = gate.wait() => {}
        _ = signal.token().cancelled() => {}
    }
}

/// 提交一个 `completed` 终态。
struct CompletedChange;
#[async_trait]
impl TaskRuntimeChange for CompletedChange {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: JsonValue::Null,
            },
        }))
    }
}

/// 提交一个 `aborted` 终态。
struct AbortedChange;
#[async_trait]
impl TaskRuntimeChange for AbortedChange {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Aborted {
                reason: None,
                result: None,
            },
        }))
    }
}

/// TS 的 `Hold`：等到自己的门打开再完成；abort 处理器把它结束为 aborted。
struct Holding {
    gates: Gates,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Holding {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let name = task_name(task);
        wait_gate_or_abort(&self.gates, &name, &runtime.signal()).await;
        if runtime.signal().is_aborted() {
            return Ok(());
        }
        runtime
            .commit(&CompletedChange, context, CommitOptions::default())
            .await
    }
}

struct HoldingAbort;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for HoldingAbort {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&AbortedChange, context, CommitOptions::default())
            .await
    }
}

fn hold_definition(name: &str, gates: Gates) -> AnyTask {
    let phases: IndexMap<String, Phase> =
        [("hold".to_owned(), Arc::new(Holding { gates }) as Phase)]
            .into_iter()
            .collect();
    let definition: Def = TaskDefinition {
        name: name.to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": "hold" })),
        phases,
        abort: Arc::new(HoldingAbort),
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
}

/// TS 的 `Probe`：`look` 门打开后记下 `ownedTasks()`，然后等自己的门再完成。
struct Probe {
    gates: Gates,
    owned: Arc<Mutex<IndexMap<String, Vec<TaskId>>>>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Probe {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let name = task_name(task);
        wait_gate_or_abort(&self.gates, &format!("look.{name}"), &runtime.signal()).await;
        if runtime.signal().is_aborted() {
            return Ok(());
        }
        let owned = runtime.owned_tasks(context).await?;
        self.owned
            .lock()
            .unwrap()
            .insert(name.clone(), owned.iter().map(|record| record.id).collect());
        wait_gate_or_abort(&self.gates, &name, &runtime.signal()).await;
        if runtime.signal().is_aborted() {
            return Ok(());
        }
        runtime
            .commit(&CompletedChange, context, CommitOptions::default())
            .await
    }
}

fn probe_definition(
    name: &str,
    gates: Gates,
    owned: Arc<Mutex<IndexMap<String, Vec<TaskId>>>>,
) -> AnyTask {
    let phases: IndexMap<String, Phase> =
        [("look".to_owned(), Arc::new(Probe { gates, owned }) as Phase)]
            .into_iter()
            .collect();
    let definition: Def = TaskDefinition {
        name: name.to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": "look" })),
        phases,
        abort: Arc::new(HoldingAbort),
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
}

async fn poll_status(fixture: &Fixture, id: TaskId) -> TaskStatus {
    status(fixture, id).await
}

async fn wait_for_status(fixture: &Fixture, id: TaskId, wanted: TaskStatus) {
    for _ in 0..600 {
        if poll_status(fixture, id).await == wanted {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "task {id} never reached {wanted:?} (last: {:?})",
        poll_status(fixture, id).await
    );
}

async fn wait_for_owned(
    owned: &Arc<Mutex<IndexMap<String, Vec<TaskId>>>>,
    name: &str,
) -> Vec<TaskId> {
    for _ in 0..600 {
        if let Some(ids) = owned.lock().unwrap().get(name).cloned() {
            return ids;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("ownedTasks() was never recorded for {name}");
}

/// TS: “takes a cascade's reason from the nearest cancelling owner”。
///
/// top（请求）→ middle（restart 标记）→ leaf：leaf 先拿到的是最近的取消 owner
/// middle 的 `restart` 原因。
#[tokio::test]
async fn takes_a_cascades_reason_from_the_nearest_cancelling_owner() {
    let gates = gates();
    let hold = hold_definition("test.indexes.hold", Arc::clone(&gates));
    let (storage, session, _publications) = open_test_session();
    let ids = {
        let hold = hold.clone();
        commit(&session, move |tx| {
            Box::pin(async move {
                let conversation = tx
                    .create_conversation(&CreateConversationOptions {
                        ownership: ConversationOwnership::Ownerless,
                    })
                    .await?;
                let mut top_options = TaskOptions::new(TaskOwnership::Conversation);
                top_options.conversation_id = Some(conversation.id);
                let top = tx
                    .create_task(&hold, json!({ "name": "top" }), top_options)
                    .await?;
                let middle = tx
                    .create_task(
                        &hold,
                        json!({ "name": "middle" }),
                        TaskOptions::new(TaskOwnership::Task { task_id: top }),
                    )
                    .await?;
                let leaf = tx
                    .create_task(
                        &hold,
                        json!({ "name": "leaf" }),
                        TaskOptions::new(TaskOwnership::Task { task_id: middle }),
                    )
                    .await?;
                Ok((top, middle, leaf))
            })
        })
        .await
        .expect("create the ownership tree")
    };
    let (top, middle, leaf) = ids;
    // top 是请求（无原因）；middle 是 `restart` 标记。
    commit(&session, move |tx| {
        Box::pin(async move {
            // 同一提交里先读完所有记录再写（事务禁止 read-after-write）。
            let mut top = tx.task(top).await?.expect("top exists");
            let mut middle = tx.task(middle).await?.expect("middle exists");
            top.abort_requested = true;
            top.abort_reason = None;
            middle.abort_requested = true;
            middle.abort_reason = Some(TaskAbortReason::Restart);
            tx.set_task(top)?;
            tx.set_task(middle)?;
            Ok(())
        })
    })
    .await
    .expect("mark the owners");
    session.close(&**BACKGROUND_CONTEXT).await;
    storage.reopen();
    let fixture = Fixture::attach(Arc::clone(&storage), vec![hold]).await;
    for id in [leaf, middle, top] {
        fixture.wait(id).await;
    }
    // 按提交顺序看写给 leaf 的每个 abort 标记。
    let reasons: Vec<Option<TaskAbortReason>> = {
        let commits = fixture.storage.commits.lock().unwrap();
        let mut reasons = Vec::new();
        for writes in commits.iter() {
            for write in writes {
                if let StorageWrite::Task { value } = write
                    && value.id == leaf
                    && value.abort_requested
                {
                    reasons.push(value.abort_reason);
                }
            }
        }
        reasons
    };
    assert!(
        !reasons.is_empty(),
        "the leaf received an abort mark at all"
    );
    assert_eq!(
        reasons[0],
        Some(TaskAbortReason::Restart),
        "reasons: {reasons:?}"
    );
}

/// TS: “finalizes a chain of held owners in the commit that frees the last one”。
#[tokio::test]
async fn finalizes_a_chain_of_held_owners_in_the_commit_that_frees_the_last_one() {
    let gates = gates();
    let hold = hold_definition("test.indexes.finalize", Arc::clone(&gates));
    let fixture = Fixture::open(vec![hold.clone()]).await;
    let conversation = conversation(&fixture.session).await;
    let (top, middle, leaf) = {
        let hold = hold.clone();
        let conversation_id = conversation;
        commit(&fixture.session, move |tx| {
            Box::pin(async move {
                let mut top_options = TaskOptions::new(TaskOwnership::Conversation);
                top_options.conversation_id = Some(conversation_id);
                let top = tx
                    .create_task(&hold, json!({ "name": "top" }), top_options)
                    .await?;
                let middle = tx
                    .create_task(
                        &hold,
                        json!({ "name": "middle" }),
                        TaskOptions::new(TaskOwnership::Task { task_id: top }),
                    )
                    .await?;
                let leaf = tx
                    .create_task(
                        &hold,
                        json!({ "name": "leaf" }),
                        TaskOptions::new(TaskOwnership::Task { task_id: middle }),
                    )
                    .await?;
                Ok((top, middle, leaf))
            })
        })
        .await
        .expect("create the ownership tree")
    };
    gate(&gates, "top").open();
    gate(&gates, "middle").open();
    wait_for_status(&fixture, top, TaskStatus::Completing).await;
    wait_for_status(&fixture, middle, TaskStatus::Completing).await;
    gate(&gates, "leaf").open();
    let record = fixture.wait(top).await;
    assert_eq!(record.state.status(), TaskStatus::Terminal);
    // 每个任务的终态写进第几个提交（1 起）。
    let ended_in = |id: TaskId| -> Option<usize> {
        let commits = fixture.storage.commits.lock().unwrap();
        for (index, writes) in commits.iter().enumerate() {
            for write in writes {
                if let StorageWrite::Task { value } = write
                    && value.id == id
                    && value.state.status() == TaskStatus::Terminal
                {
                    return Some(index + 1);
                }
            }
        }
        None
    };
    let middle_at = ended_in(middle).expect("middle ends");
    let top_at = ended_in(top).expect("top ends");
    let leaf_at = ended_in(leaf).expect("leaf ends");
    assert_eq!(middle_at, top_at, "both owners end in one finalize commit");
    assert!(
        middle_at > leaf_at,
        "the owners end after the leaf (middle {middle_at}, leaf {leaf_at})"
    );
}

/// TS: “lists the live tasks a task owns directly, in ID order, also after a reopen”。
#[tokio::test]
async fn lists_the_live_tasks_a_task_owns_directly_in_id_order_also_after_a_reopen() {
    let gates = gates();
    let owned: Arc<Mutex<IndexMap<String, Vec<TaskId>>>> = Arc::new(Mutex::new(IndexMap::new()));
    let hold_task = hold_definition("test.indexes.hold", Arc::clone(&gates));
    let probe_task = probe_definition("test.indexes.probe", Arc::clone(&gates), Arc::clone(&owned));
    let fixture = Fixture::open(vec![hold_task.clone(), probe_task.clone()]).await;
    let conversation = conversation(&fixture.session).await;
    let ids = {
        let hold = hold_task.clone();
        let probe = probe_task.clone();
        let conversation_id = conversation;
        commit(&fixture.session, move |tx| {
            Box::pin(async move {
                let mut probe_options = TaskOptions::new(TaskOwnership::Conversation);
                probe_options.conversation_id = Some(conversation_id);
                let probe = tx
                    .create_task(&probe, json!({ "name": "probe" }), probe_options)
                    .await?;
                let child_options =
                    |owner: TaskId| TaskOptions::new(TaskOwnership::Task { task_id: owner });
                let first = tx
                    .create_task(
                        &hold,
                        json!({ "name": "first" }),
                        TaskOptions::new(TaskOwnership::Task { task_id: probe }),
                    )
                    .await?;
                let ends = tx
                    .create_task(
                        &hold,
                        json!({ "name": "ends" }),
                        TaskOptions::new(TaskOwnership::Task { task_id: probe }),
                    )
                    .await?;
                let second = tx
                    .create_task(
                        &hold,
                        json!({ "name": "second" }),
                        TaskOptions::new(TaskOwnership::Task { task_id: probe }),
                    )
                    .await?;
                let _grandchild = tx
                    .create_task(&hold, json!({ "name": "grandchild" }), child_options(first))
                    .await?;
                let child = tx
                    .create_conversation(&CreateConversationOptions {
                        ownership: ConversationOwnership::Task { task_id: probe },
                    })
                    .await?;
                let mut inside_options = TaskOptions::new(TaskOwnership::Conversation);
                inside_options.conversation_id = Some(child.id);
                let _inside = tx
                    .create_task(&hold, json!({ "name": "inside" }), inside_options)
                    .await?;
                Ok((probe, first, ends, second))
            })
        })
        .await
        .expect("create the probe tree")
    };
    let (probe, first, ends, second) = ids;
    gate(&gates, "ends").open();
    fixture.wait(ends).await;
    // first 也结束，但它的结果作为 `completing` 持有（grandchild 还活着）：仍是活工作，会被列出。
    gate(&gates, "first").open();
    wait_for_status(&fixture, first, TaskStatus::Completing).await;
    gate(&gates, "look.probe").open();
    let listed = wait_for_owned(&owned, "probe").await;
    assert_eq!(listed, vec![first, second]);
    // 关闭后重开：重跑的调用看到的还是同一批任务。先关闭 Session（`#seal` 置 closing），
    // 再 join：TS 的 `Harness.close()` 也是先跑关闭监听器、再 `beforeClose()` 的 join。
    fixture.session.close(&**BACKGROUND_CONTEXT).await;
    fixture.scheduler.join().await;
    fixture.storage.reopen();
    owned.lock().unwrap().clear();
    let reopened = Fixture::attach(Arc::clone(&fixture.storage), vec![hold_task, probe_task]).await;
    let listed = wait_for_owned(&owned, "probe").await;
    assert_eq!(listed, vec![first, second]);
    let _ = (probe, reopened);
}
