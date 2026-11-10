//! `test/harness-inspect.test.ts` 的 Rust 端口：`Harness.inspect()` 不运行任务代码地
//! 导出每个活动任务的调度状态，并列出未落定的提交。

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;
#[path = "support/task.rs"]
mod task_support;

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::Context;
use kiana_durable::Session;
use kiana_durable::harness::live::TaskCommit;
use kiana_durable::harness::scheduler::erase_task;
use kiana_durable::harness::types::{
    AnyTask, BlockedReason, SubmissionDraft, SubmissionDraftKind, TaskInspection,
    TaskInspectionState, TaskRuntime,
};
use kiana_durable::types::{
    BoxError, CommitOptions, EntryDraft, JoinPolicy, JsonValue, RunningTask, SessionChange,
    Storage, TaskDefinition, TaskId, TaskMigrate, TaskOptions, TaskOutcome, TaskOwnership,
    TaskPhase, TaskState, Tx,
};

use chat_support::{ChatSetup, context, open_chat, ownerless, unanswered};
use harness_support::text_input;
use task_support::{Deferred, OpenTasksOptions, open_root, open_tasks_with};

type Phase = Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn completed_change() -> TaskCommit<'static> {
    TaskCommit::new(|_tx, _current| {
        Box::pin(async {
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Completed {
                    result: JsonValue::Null,
                },
            }))
        })
    })
}

struct TerminalPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for TerminalPhase {
    async fn run(
        &self,
        _task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&completed_change(), context, CommitOptions::default())
            .await
    }
}

struct NoopPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for NoopPhase {
    async fn run(
        &self,
        _task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

struct GatePhase {
    gate: Arc<Deferred<()>>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for GatePhase {
    async fn run(
        &self,
        _task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.gate.promise().await.expect("the gate");
        runtime
            .commit(&completed_change(), context, CommitOptions::default())
            .await
    }
}

struct WaitPhase {
    gate: Arc<Mutex<Option<TaskId>>>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for WaitPhase {
    async fn run(
        &self,
        _task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let gate = lock(&self.gate).expect("the gate task");
        let change = TaskCommit::new(move |_tx, _current| {
            Box::pin(async move {
                Ok(Some(TaskState::Waiting {
                    checkpoint: serde_json::json!({ "phase": "done" }),
                    on: vec![gate],
                    policy: JoinPolicy::AllSettled,
                }))
            })
        });
        runtime
            .commit(&change, context, CommitOptions::default())
            .await
    }
}

fn definition(
    name: &str,
    version: i64,
    initial_phase: &str,
    phases: Vec<(&str, Phase)>,
    migrate: Option<TaskMigrate<JsonValue, JsonValue>>,
) -> AnyTask {
    let initial_phase = initial_phase.to_owned();
    let definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()> = TaskDefinition {
        name: name.to_owned(),
        version,
        initial: Arc::new(move |_input| serde_json::json!({ "phase": initial_phase })),
        phases: phases
            .into_iter()
            .map(|(key, phase)| (key.to_owned(), phase))
            .collect::<IndexMap<_, _>>(),
        abort: Arc::new(NoopPhase) as Phase,
        migrate,
        hooks: None,
    };
    erase_task(&definition)
}

fn gate_task(gate: Arc<Deferred<()>>) -> AnyTask {
    definition(
        "test.gate",
        1,
        "run",
        vec![("run", Arc::new(GatePhase { gate }) as Phase)],
        None,
    )
}

fn dependent_task(gate_id: Arc<Mutex<Option<TaskId>>>) -> AnyTask {
    definition(
        "test.dependent",
        1,
        "wait",
        vec![
            ("wait", Arc::new(WaitPhase { gate: gate_id }) as Phase),
            ("done", Arc::new(TerminalPhase) as Phase),
        ],
        None,
    )
}

fn simple_task(name: &str, version: i64) -> AnyTask {
    definition(
        name,
        version,
        "run",
        vec![("run", Arc::new(TerminalPhase) as Phase)],
        None,
    )
}

fn migrating_task(migrations: Arc<AtomicUsize>) -> AnyTask {
    let migrate: TaskMigrate<JsonValue, JsonValue> = Arc::new(move |_input, _checkpoint, _from| {
        migrations.fetch_add(1, Ordering::SeqCst);
        Ok((JsonValue::Null, serde_json::json!({ "phase": "run" })))
    });
    definition(
        "test.migrating",
        2,
        "run",
        vec![("run", Arc::new(TerminalPhase) as Phase)],
        Some(migrate),
    )
}

fn failing_migration_task() -> AnyTask {
    let migrate: TaskMigrate<JsonValue, JsonValue> = Arc::new(|_input, _checkpoint, _from| {
        Err(Box::new(std::io::Error::other("cannot migrate")))
    });
    definition(
        "test.failing",
        2,
        "run",
        vec![("run", Arc::new(TerminalPhase) as Phase)],
        Some(migrate),
    )
}

#[derive(Debug)]
struct Ids {
    gate: TaskId,
    dependent: TaskId,
    migrating: TaskId,
    no_migration: TaskId,
    failing: TaskId,
    too_old: TaskId,
    missing: TaskId,
}

struct CreateInspectionTasks {
    gate: AnyTask,
    dependent: AnyTask,
    migrating: AnyTask,
    no_migration: AnyTask,
    failing: AnyTask,
    too_old: AnyTask,
    missing: AnyTask,
}

#[async_trait]
impl SessionChange for CreateInspectionTasks {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let options = || TaskOptions::new(TaskOwnership::Conversation);
        let gate = tx
            .create_task(&self.gate, JsonValue::Null, options())
            .await?;
        let dependent = tx
            .create_task(&self.dependent, JsonValue::Null, options())
            .await?;
        let migrating = tx
            .create_task(&self.migrating, JsonValue::Null, options())
            .await?;
        let no_migration = tx
            .create_task(&self.no_migration, JsonValue::Null, options())
            .await?;
        let failing = tx
            .create_task(&self.failing, JsonValue::Null, options())
            .await?;
        let too_old = tx
            .create_task(&self.too_old, JsonValue::Null, options())
            .await?;
        let missing = tx
            .create_task(&self.missing, JsonValue::Null, options())
            .await?;
        Ok(Box::new(Ids {
            gate,
            dependent,
            migrating,
            no_migration,
            failing,
            too_old,
            missing,
        }))
    }
}

fn state_of(tasks: &[TaskInspection], id: TaskId) -> Option<TaskInspectionState> {
    tasks
        .iter()
        .find(|entry| entry.record.id == id)
        .map(|entry| entry.state.clone())
}

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

#[tokio::test(flavor = "multi_thread")]
async fn derives_every_live_tasks_state_without_running_task_code() {
    let gate = Arc::new(Deferred::<()>::new());
    let gate_id: Arc<Mutex<Option<TaskId>>> = Arc::new(Mutex::new(None));
    let migrations = Arc::new(AtomicUsize::new(0));
    let registered = vec![
        gate_task(Arc::clone(&gate)),
        dependent_task(Arc::clone(&gate_id)),
        migrating_task(Arc::clone(&migrations)),
        simple_task("test.no-migration", 2),
        failing_migration_task(),
        simple_task("test.too-old", 1),
    ];
    let fixture = open_tasks_with(memory(), registered, OpenTasksOptions::default()).await;
    let root = open_root(&fixture.harness).await;
    let ids = {
        let change = CreateInspectionTasks {
            gate: gate_task(Arc::clone(&gate)),
            dependent: dependent_task(Arc::clone(&gate_id)),
            migrating: simple_task("test.migrating", 1),
            no_migration: simple_task("test.no-migration", 1),
            failing: simple_task("test.failing", 1),
            too_old: simple_task("test.too-old", 2),
            missing: simple_task("test.missing", 1),
        };
        let value = root
            .commit(&change, &*context(), CommitOptions::default())
            .await
            .expect("the tasks commit");
        *value.downcast::<Ids>().expect("the ids")
    };
    // 注册时的 dependent 读到同一个 gate 单元。
    *lock(&gate_id) = Some(ids.gate);

    let paused = fixture.harness.inspect(&*context()).await.expect("inspect");
    assert_eq!(
        paused.scheduling,
        kiana_durable::harness::types::SchedulingState::Paused
    );
    assert_eq!(
        paused
            .tasks
            .iter()
            .map(|entry| entry.record.id)
            .collect::<Vec<_>>(),
        vec![
            ids.gate,
            ids.dependent,
            ids.migrating,
            ids.no_migration,
            ids.failing,
            ids.too_old,
            ids.missing
        ]
    );
    assert_eq!(
        state_of(&paused.tasks, ids.gate),
        Some(TaskInspectionState::Ready { migrates: false })
    );
    assert_eq!(
        state_of(&paused.tasks, ids.dependent),
        Some(TaskInspectionState::Ready { migrates: false })
    );
    assert_eq!(
        state_of(&paused.tasks, ids.migrating),
        Some(TaskInspectionState::Ready { migrates: true })
    );
    // 从未尝试的迁移不会为了探明结果而运行。
    assert_eq!(
        state_of(&paused.tasks, ids.failing),
        Some(TaskInspectionState::Ready { migrates: true })
    );
    assert_eq!(
        state_of(&paused.tasks, ids.no_migration),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::MigrationFailed,
            error: Some(JsonValue::String(
                "Task test.no-migration version 2 has no migration from 1".to_owned()
            )),
        })
    );
    assert_eq!(
        state_of(&paused.tasks, ids.too_old),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::TaskTooOld,
            error: None,
        })
    );
    assert_eq!(
        state_of(&paused.tasks, ids.missing),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::MissingTask,
            error: None,
        })
    );
    assert_eq!(migrations.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .harness
            .inspect(&*context())
            .await
            .expect("inspect")
            .scheduling,
        kiana_durable::harness::types::SchedulingState::Paused
    );

    let _ = fixture.harness.resume();
    task_support::eventually(|| {
        let migrations = Arc::clone(&migrations);
        async move { migrations.load(Ordering::SeqCst) == 1 }
    })
    .await
    .expect("the migration runs once");
    fixture
        .harness
        .wait_for_task(ids.migrating, &*context())
        .await
        .expect("the migrated task settles");
    let mut running = fixture.harness.inspect(&*context()).await.expect("inspect");
    for _ in 0..200 {
        if matches!(
            state_of(&running.tasks, ids.gate),
            Some(TaskInspectionState::Running)
        ) && matches!(
            state_of(&running.tasks, ids.dependent),
            Some(TaskInspectionState::Waiting { .. })
        ) {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        running = fixture.harness.inspect(&*context()).await.expect("inspect");
    }
    assert_eq!(
        state_of(&running.tasks, ids.dependent),
        Some(TaskInspectionState::Waiting { on: vec![ids.gate] })
    );
    assert_eq!(
        running.scheduling,
        kiana_durable::harness::types::SchedulingState::Running
    );
    assert_eq!(
        state_of(&running.tasks, ids.gate),
        Some(TaskInspectionState::Running)
    );
    assert!(state_of(&running.tasks, ids.migrating).is_none());
    assert_eq!(
        state_of(&running.tasks, ids.failing),
        Some(TaskInspectionState::Blocked {
            reason: BlockedReason::MigrationFailed,
            error: Some(JsonValue::String("cannot migrate".to_owned())),
        })
    );

    gate.resolve(());
    fixture
        .harness
        .wait_for_task(ids.dependent, &*context())
        .await
        .expect("the dependent task settles");
    let settled = fixture.harness.inspect(&*context()).await.expect("inspect");
    assert_eq!(
        settled
            .tasks
            .iter()
            .map(|entry| entry.record.id)
            .collect::<Vec<_>>(),
        vec![ids.no_migration, ids.failing, ids.too_old, ids.missing]
    );
    fixture.harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_unsettled_submissions() {
    let setup = ChatSetup::new();
    let busy = unanswered();
    setup.faux.set_responses(vec![busy.step.clone()]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let other = harness
        .create_conversation(ownerless(None), &*context())
        .await
        .expect("the conversation");
    let write = SubmissionDraft {
        request_id: None,
        kind: SubmissionDraftKind::Write {
            entry: EntryDraft {
                key_order: None,
                kind: "note".to_owned(),
                model: None,
                data: None,
                head: None,
                edits: None,
            },
        },
    };
    let _ = other
        .submit(write, &*context())
        .await
        .expect("the write submission");
    let input = root
        .submit(text_input("hi").into_draft(), &*context())
        .await
        .expect("the input submission");
    busy.reached().await;

    let inspection = harness.inspect(&*context()).await.expect("inspect");
    let status = input.status(&*context()).await.expect("status");
    assert_eq!(inspection.submissions, vec![status]);
    let tasks: Vec<(String, &'static str)> = inspection
        .tasks
        .iter()
        .map(|entry| {
            (
                entry.record.kind.clone(),
                match &entry.state {
                    TaskInspectionState::Running => "running",
                    TaskInspectionState::Ready { .. } => "ready",
                    TaskInspectionState::Waiting { .. } => "waiting",
                    TaskInspectionState::Sleeping { .. } => "sleeping",
                    TaskInspectionState::Completing => "completing",
                    TaskInspectionState::Blocked { .. } => "blocked",
                },
            )
        })
        .collect();
    assert_eq!(tasks, vec![("kiana.generation".to_owned(), "running")]);
    harness.close(&*context()).await;
}
