//! `test/scheduler-fanout-bench.ts` 的 Rust 端口（调度器层）。
//!
//! TS 基准用一个工具扇出 `calls` 个并行嵌套调用；这里用调度器自己的所有权模型复现同一形状：一个父任务扇出 64 个 owned 任务并 `allSettled` 等待。断言全部落定且所有权索引归零；耗时数字不在测试里断言（基准脚本语义）。
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

use kiana_durable::types::JoinPolicy;

struct FanOut {
    children: Arc<Mutex<Vec<TaskId>>>,
}
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for FanOut {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let on = self.children.lock().unwrap().clone();
        runtime
            .commit(&WaitAll { on }, context, CommitOptions::default())
            .await
    }
}

struct WaitAll {
    on: Vec<TaskId>,
}
#[async_trait]
impl TaskRuntimeChange for WaitAll {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Waiting {
            checkpoint: json!({ "phase": "done" }),
            on: self.on.clone(),
            policy: JoinPolicy::AllSettled,
        }))
    }
}

const CALLS: usize = 64;

#[tokio::test]
async fn fans_out_to_many_owned_tasks_and_settles_them_all() {
    let leaf = define("bench.fanout.leaf", 1, vec![("start", complete())]);
    let children = Arc::new(Mutex::new(Vec::new()));
    let parent = define_with(
        "bench.fanout.parent",
        1,
        vec![
            (
                "start",
                Arc::new(FanOut {
                    children: Arc::clone(&children),
                }),
            ),
            ("done", complete()),
        ],
        noop(),
        None,
    );
    let registry_tasks = vec![leaf.clone(), parent.clone()];
    let setup_leaf = leaf.clone();
    let setup_parent = parent.clone();
    let setup_children = Arc::clone(&children);
    let (fixture, parent_id) = Fixture::open_with(registry_tasks, move |session| async move {
        let conversation = conversation(&session).await;
        let parent = create(
            &session,
            &setup_parent,
            conversation,
            TaskOwnership::Conversation,
            json!({}),
        )
        .await;
        let mut ids = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            ids.push(
                create(
                    &session,
                    &setup_leaf,
                    conversation,
                    TaskOwnership::Task { task_id: parent },
                    json!({}),
                )
                .await,
            );
        }
        *setup_children.lock().unwrap() = ids;
        parent
    })
    .await;
    let record = fixture.wait_status(parent_id, TaskStatus::Terminal).await;
    assert!(matches!(
        record.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed { .. }
        }
    ));
    let child_ids = children.lock().unwrap().clone();
    for id in child_ids {
        let record = fixture.wait(id).await;
        assert!(matches!(
            record.state,
            TaskState::Terminal {
                outcome: TaskOutcome::Completed { .. }
            }
        ));
    }
    let sizes = fixture.scheduler.index_sizes();
    assert_eq!(sizes.live, 0, "sizes: {sizes:?}");
    assert_eq!(sizes.below, 0, "sizes: {sizes:?}");
    assert_eq!(sizes.roots, 0, "sizes: {sizes:?}");
}
