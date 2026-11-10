//! `test/harness-task-graph.test.ts` 的 Rust 端口。
//!
//! 任务图从提交状态构建：覆盖 owner 边、拥有的会话排序、abort 标记、无节点变化的提交
//! 不发布修订、关闭结束 watch，以及重新打开后的存活任务、精确帧重放与运行期 abort。

#![allow(clippy::await_holding_lock)]

#[path = "support/harness.rs"]
mod harness_support;
mod support;
#[path = "support/task.rs"]
mod task_support;

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::Context;
use kiana_durable::harness::types::{Conversation as ConversationTrait, TaskGraph, TaskGraphState};
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, CreateConversationOptions,
    EntryDraft, JsonValue, SessionChange, Storage, TaskDefinition, TaskId, TaskOptions,
    TaskOwnership, TaskPhase, TaskRecord, TaskRuntime, Tx, WatchEnd, WatchListener,
};

use kiana_durable::Session;
use task_support::{OpenTasksOptions, TasksFixture, open_tasks_with};

// ---------------------------------------------------------------------------
// 夹具：一个只声明 `work` 阶段的简单任务
// ---------------------------------------------------------------------------

struct WorkPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for WorkPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

fn work_task() -> kiana_durable::harness::types::AnyTask {
    let definition: TaskDefinition = TaskDefinition {
        name: "test.graph-work".to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({"phase": "work"})),
        phases: IndexMap::from([(
            "work".to_owned(),
            Arc::new(WorkPhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: Arc::new(WorkPhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

async fn open_fixture() -> (TasksFixture, Arc<dyn ConversationTrait>) {
    let fixture = open_tasks_with(memory(), vec![work_task()], OpenTasksOptions::default()).await;
    let root = task_support::open_root(&fixture.harness).await;
    (fixture, root)
}

fn statuses(graph: &TaskGraph) -> std::collections::BTreeMap<String, String> {
    graph
        .tasks
        .values()
        .map(|node| {
            (
                format!("{}#{}", node.kind, node.id),
                match &node.state {
                    TaskGraphState::Pending { .. } => "pending",
                    TaskGraphState::Running { .. } => "running",
                    TaskGraphState::Waiting { .. } => "waiting",
                    TaskGraphState::Sleeping { .. } => "sleeping",
                    TaskGraphState::Completing { .. } => "completing",
                }
                .to_owned(),
            )
        })
        .collect()
}

struct Recorder {
    frames: Mutex<Vec<TaskGraph>>,
}

#[async_trait]
impl WatchListener<TaskGraph> for Recorder {
    async fn call(
        &self,
        value: TaskGraph,
        _ops: Vec<kiana_chord::delta::Op>,
        _context: Arc<dyn Context>,
    ) {
        self.frames
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(value);
    }
}

// ---------------------------------------------------------------------------
// 提交里的任务创建
// ---------------------------------------------------------------------------

struct CreateTask {
    task: kiana_durable::harness::types::AnyTask,
    background: bool,
    owned_conversations: usize,
}

#[async_trait]
impl SessionChange for CreateTask {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut options = TaskOptions::new(TaskOwnership::Conversation);
        options.background = self.background.then_some(true);
        let id = tx
            .create_task(&self.task, JsonValue::Object(Default::default()), options)
            .await?;
        let mut conversations = Vec::new();
        for _ in 0..self.owned_conversations {
            let record = tx
                .create_conversation(&CreateConversationOptions {
                    ownership: ConversationOwnership::Task { task_id: id },
                })
                .await?;
            conversations.push(record.id);
        }
        Ok(Box::new((id, conversations)))
    }
}

struct AppendNote {
    conversation_id: ConversationId,
}

#[async_trait]
impl SessionChange for AppendNote {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let record = tx
            .append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "note".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(record))
    }
}

async fn create_task(
    conversation: &Arc<dyn ConversationTrait>,
    background: bool,
    owned: usize,
) -> (TaskId, Vec<ConversationId>) {
    let value = conversation
        .commit(
            &CreateTask {
                task: work_task(),
                background,
                owned_conversations: owned,
            },
            &*task_support::context(),
            CommitOptions::default(),
        )
        .await
        .expect("the task commits");
    *value
        .downcast::<(TaskId, Vec<ConversationId>)>()
        .expect("the ids")
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn follows_committed_nodes_owner_edges_and_owned_conversations() {
    let (fixture, root) = open_fixture().await;
    let watch = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    assert_eq!(watch.value(), &TaskGraph::default());
    let recorder = Arc::new(Recorder {
        frames: Mutex::new(Vec::new()),
    });
    watch.start(Arc::clone(&recorder) as Arc<dyn WatchListener<TaskGraph>>);

    let (task, owned) = create_task(&root, false, 2).await;
    task_support::eventually(|| {
        let graph = watch.value().clone();
        async move {
            graph
                .tasks
                .get(&task.to_string())
                .is_some_and(|node| node.conversations.len() == 2)
        }
    })
    .await
    .expect("the node reaches the graph");
    let graph = watch.value().clone();
    let node = graph.tasks.get(&task.to_string()).expect("the node");
    assert_eq!(node.id, task);
    assert_eq!(node.kind, "test.graph-work");
    assert_eq!(node.conversation_id, root.id());
    assert!(node.owner.is_none());
    assert!(!node.background);
    assert!(!node.abort_requested);
    assert_eq!(
        node.state,
        TaskGraphState::Pending {
            phase: "work".to_owned()
        }
    );
    let mut sorted = owned.clone();
    sorted.sort();
    assert_eq!(node.conversations, sorted);
    assert_eq!(
        statuses(&graph).get(&format!("test.graph-work#{task}")),
        Some(&"pending".to_owned())
    );
    for id in &owned {
        assert!(
            fixture
                .harness
                .conversation(*id, &*task_support::context())
                .await
                .expect("conversation")
                .is_some()
        );
    }

    // 最后一个观察者离开后重建的值结构与推进值相同（Rust 的值按值持有，身份按结构断言）。
    let advanced = watch.value().clone();
    let _ = watch.stop().await;
    let rebuilt = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    assert_eq!(rebuilt.value(), &advanced);
    let _ = rebuilt.stop().await;
    fixture.harness.close(&*task_support::context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn marks_aborts_on_pending_tasks() {
    let (fixture, root) = open_fixture().await;
    let (task, _owned) = create_task(&root, false, 0).await;
    let watch = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    let recorder = Arc::new(Recorder {
        frames: Mutex::new(Vec::new()),
    });
    watch.start(Arc::clone(&recorder) as Arc<dyn WatchListener<TaskGraph>>);
    let outcome = fixture
        .harness
        .abort_task(task, &*task_support::context())
        .await
        .expect("abort");
    assert_eq!(
        outcome,
        kiana_durable::harness::types::AbortTaskOutcome::Marked
    );
    task_support::eventually(|| {
        let watch_value = watch.value().clone();
        async move {
            watch_value
                .tasks
                .get(&task.to_string())
                .is_some_and(|node| node.abort_requested)
        }
    })
    .await
    .expect("the abort mark reaches the graph");
    task_support::drained_for_tests().await;
    let frames = recorder.frames.lock().unwrap();
    let marked = frames.iter().any(|frame| {
        frame
            .tasks
            .get(&task.to_string())
            .is_some_and(|node| node.abort_requested)
    });
    drop(frames);
    assert!(marked);
    let _ = watch.stop().await;
    fixture.harness.close(&*task_support::context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn publishes_no_revision_for_a_commit_that_changes_no_node_in_it() {
    let (fixture, root) = open_fixture().await;
    let (task, _owned) = create_task(&root, false, 0).await;
    let first = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    let second = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    // 同一个挂载：两份观察者的值结构相等（Rust 按值持有，不能断言对象身份）。
    assert_eq!(first.value(), second.value());
    let recorder = Arc::new(Recorder {
        frames: Mutex::new(Vec::new()),
    });
    first.start(Arc::clone(&recorder) as Arc<dyn WatchListener<TaskGraph>>);
    let _ = root
        .commit(
            &AppendNote {
                conversation_id: root.id(),
            },
            &*task_support::context(),
            CommitOptions::default(),
        )
        .await
        .expect("append");
    task_support::drained_for_tests().await;
    assert!(recorder.frames.lock().unwrap().is_empty());
    assert!(first.value().tasks.contains_key(&task.to_string()));
    let _ = first.stop().await;
    let _ = second.stop().await;
    fixture.harness.close(&*task_support::context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_watches_at_close() {
    let (fixture, root) = open_fixture().await;
    let _ = create_task(&root, true, 0).await;
    let watch = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    let background = watch
        .value()
        .tasks
        .values()
        .next()
        .map(|node| node.background);
    assert_eq!(background, Some(true));
    fixture.harness.close(&*task_support::context()).await;
    assert!(matches!(watch.closed().await, WatchEnd::Ended));
}

// ---------------------------------------------------------------------------
// 回归：拥有的会话按 ID 排序；取消的获取不留下观察者
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn lists_owned_conversations_in_id_order_whatever_order_one_commit_creates_them_in() {
    let (fixture, root) = open_fixture().await;
    let watch = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("graph");
    let recorder = Arc::new(Recorder {
        frames: Mutex::new(Vec::new()),
    });
    watch.start(Arc::clone(&recorder) as Arc<dyn WatchListener<TaskGraph>>);
    let (id, created) = create_task(&root, false, 4).await;
    task_support::eventually(|| {
        let recorder = Arc::clone(&recorder);
        let key = id.get().to_string();
        async move {
            recorder
                .frames
                .lock()
                .unwrap()
                .last()
                .and_then(|graph| graph.tasks.get(&key).map(|node| node.conversations.len()))
                == Some(4)
        }
    })
    .await
    .expect("the node lists its conversations");
    let advanced = recorder
        .frames
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("a revision");
    let mut expected = created.clone();
    expected.sort();
    assert_eq!(
        advanced.tasks[&id.get().to_string()].conversations,
        expected
    );
    assert_eq!(advanced.tasks[&id.get().to_string()].conversations, created);
    let _ = watch.stop().await;
    // 重新获取的图与推进后的值一致。
    let rebuilt = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("rebuilt");
    assert_eq!(rebuilt.value(), &advanced);
    let _ = rebuilt.stop().await;
    fixture.harness.close(&*task_support::context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn registers_nothing_for_an_acquisition_cancelled_while_it_waits_for_the_line() {
    use kiana_chord::context::{AbortReason, AbortSignal, with_abort_signal};
    use support::ControlledStorage;

    let storage = Arc::new(ControlledStorage::new());
    let fixture = open_tasks_with(
        storage.clone() as Arc<dyn Storage>,
        vec![work_task()],
        OpenTasksOptions::default(),
    )
    .await;
    let root = task_support::open_root(&fixture.harness).await;
    let _ = create_task(&root, false, 0).await;
    // 让下一次提交停在 Storage 里，占住 Session 线。
    let gate = storage.hold_commits();
    let conversation_id = root.id();
    let committer = Arc::clone(&fixture.harness);
    let commit_context = task_support::context();
    let blocking = tokio::spawn(async move {
        committer
            .commit(
                &AppendNote { conversation_id },
                &*commit_context,
                CommitOptions::default(),
            )
            .await
            .expect("the blocking commit")
    });
    gate.entered().await;
    let signal = AbortSignal::new();
    let cancelled_context = with_abort_signal(signal.clone(), Arc::clone(&task_support::context()));
    let harness = Arc::clone(&fixture.harness);
    let acquiring =
        tokio::spawn(async move { harness.watch_task_graph(&*cancelled_context).await });
    tokio::task::yield_now().await;
    signal.abort(Some(AbortReason::error(std::io::Error::other("cancelled"))));
    gate.release();
    blocking.await.expect("the blocking commit joins");
    let error = match acquiring.await.expect("the acquisition joins") {
        Ok(_) => panic!("the acquisition is cancelled"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cancelled"), "{error}");
    // 没有观察者留下挂载：重新获取得到同样的值。
    let first = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("first");
    let value = first.value().clone();
    let _ = first.stop().await;
    let second = fixture
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("second");
    assert_eq!(second.value(), &value);
    let _ = second.stop().await;
    fixture.harness.close(&*task_support::context()).await;
}

// ---------------------------------------------------------------------------
// 重新打开后的存活任务、abort 标记与精确帧
// ---------------------------------------------------------------------------

/// 可被任意多个调用同时等待的一次性闸门（TS 的 `gate.promise` 可被多个调用 await）。
struct BroadcastGate {
    sender: tokio::sync::watch::Sender<bool>,
}

impl BroadcastGate {
    fn new() -> Self {
        BroadcastGate {
            sender: tokio::sync::watch::channel(false).0,
        }
    }

    fn receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.sender.subscribe()
    }

    fn resolve(&self) {
        let _ = self.sender.send(true);
    }
}

struct GateWorkPhase {
    gate: Arc<BroadcastGate>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for GateWorkPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let mut open = self.gate.receiver();
        let opened = async move {
            loop {
                if *open.borrow() {
                    return;
                }
                if open.changed().await.is_err() {
                    return;
                }
            }
        };
        let signal = runtime.signal();
        let cancelled = signal.token().cancelled();
        tokio::select! {
            _ = opened => {}
            _ = cancelled => {}
        }
        // 关闭/中止时这次提交可能被拒绝；调用本身仍然结束。
        let _ = runtime
            .commit(&CompleteRun, context, CommitOptions::default())
            .await;
        Ok(())
    }
}

struct CompleteRun;

#[async_trait]
impl kiana_durable::harness::types::TaskRuntimeChange for CompleteRun {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<kiana_durable::types::TaskState>, BoxError> {
        Ok(Some(kiana_durable::types::TaskState::Terminal {
            outcome: kiana_durable::types::TaskOutcome::Completed {
                result: JsonValue::Null,
            },
        }))
    }
}

struct AbortRunPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortRunPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&AbortedRun, context, CommitOptions::default())
            .await
    }
}

struct AbortedRun;

#[async_trait]
impl kiana_durable::harness::types::TaskRuntimeChange for AbortedRun {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<kiana_durable::types::TaskState>, BoxError> {
        Ok(Some(kiana_durable::types::TaskState::Terminal {
            outcome: kiana_durable::types::TaskOutcome::Aborted {
                reason: Some("test".to_owned()),
                result: None,
            },
        }))
    }
}

fn gate_work_task(gate: Arc<BroadcastGate>) -> kiana_durable::harness::types::AnyTask {
    let definition: TaskDefinition = TaskDefinition {
        name: "test.graph-work".to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({"phase": "work"})),
        phases: IndexMap::from([(
            "work".to_owned(),
            Arc::new(GateWorkPhase { gate })
                as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: Arc::new(AbortRunPhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

struct CreateGraph {
    task: kiana_durable::harness::types::AnyTask,
}

#[async_trait]
impl SessionChange for CreateGraph {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let foreground = tx
            .create_task(
                &self.task,
                JsonValue::Null,
                TaskOptions::new(TaskOwnership::Conversation),
            )
            .await?;
        let mut background_options = TaskOptions::new(TaskOwnership::Conversation);
        background_options.background = Some(true);
        let background = tx
            .create_task(&self.task, JsonValue::Null, background_options)
            .await?;
        let owned = tx
            .create_conversation(&CreateConversationOptions {
                ownership: ConversationOwnership::Task {
                    task_id: foreground,
                },
            })
            .await?;
        Ok(Box::new((foreground, background, owned.id)))
    }
}

struct OpsRecorder {
    initial: serde_json::Value,
    frames: Mutex<Vec<(TaskGraph, Vec<kiana_chord::delta::Op>)>>,
}

#[async_trait]
impl WatchListener<TaskGraph> for OpsRecorder {
    async fn call(
        &self,
        value: TaskGraph,
        ops: Vec<kiana_chord::delta::Op>,
        _context: Arc<dyn Context>,
    ) {
        // 帧必须能从获取时的值经操作重放出来。
        let mut replica = self.initial.clone();
        for (previous, previous_ops) in self.frames.lock().unwrap().iter() {
            let _ = previous;
            replica = kiana_chord::delta::index::apply_immutable(replica, previous_ops)
                .expect("previous ops apply");
        }
        replica = kiana_chord::delta::index::apply_immutable(replica, &ops).expect("ops apply");
        assert_eq!(
            replica,
            serde_json::to_value(&value).expect("serialize the graph"),
            "the operations rebuild the delivered value"
        );
        self.frames.lock().unwrap().push((value, ops));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn builds_from_committed_tasks_shows_surviving_tasks_as_pending_after_reopen_and_marks_aborts()
 {
    use kiana_chord::delta::{Op, Seg};
    use support::MemoryStorage;

    let gate = Arc::new(BroadcastGate::new());
    let storage = Arc::new(MemoryStorage::new());
    let first = open_tasks_with(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![gate_work_task(Arc::clone(&gate))],
        OpenTasksOptions::default(),
    )
    .await;
    let first_root = task_support::open_root(&first.harness).await;
    let value = first_root
        .commit(
            &CreateGraph {
                task: gate_work_task(Arc::clone(&gate)),
            },
            &*task_support::context(),
            CommitOptions::default(),
        )
        .await
        .expect("create the tasks");
    let (foreground, background, owned) = *value
        .downcast::<(TaskId, TaskId, ConversationId)>()
        .expect("the ids");
    first.harness.resume().expect("resume");
    task_support::eventually(|| {
        let harness = Arc::clone(&first.harness);
        async move {
            harness
                .inspect(&*task_support::context())
                .await
                .is_ok_and(|inspection| {
                    inspection.tasks.len() == 2
                        && inspection.tasks.iter().all(|task| {
                            matches!(
                                task.state,
                                kiana_durable::harness::types::TaskInspectionState::Running
                            )
                        })
                })
        }
    })
    .await
    .expect("both tasks run");
    let running = first
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    assert_eq!(
        statuses(running.value()),
        std::collections::BTreeMap::from([
            (
                format!("test.graph-work#{foreground}"),
                "running".to_owned()
            ),
            (
                format!("test.graph-work#{background}"),
                "running".to_owned()
            ),
        ])
    );
    let _ = running.stop().await;
    first.harness.close(&*task_support::context()).await;

    // 重新打开后获取：从提交的记录与 owner 边构建；open 把 running 归为 pending。
    storage.reopen();
    let second = open_tasks_with(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![gate_work_task(Arc::clone(&gate))],
        OpenTasksOptions::default(),
    )
    .await;
    let watch = second
        .harness
        .watch_task_graph(&*task_support::context())
        .await
        .expect("watch");
    assert_eq!(
        statuses(watch.value()),
        std::collections::BTreeMap::from([
            (
                format!("test.graph-work#{foreground}"),
                "pending".to_owned()
            ),
            (
                format!("test.graph-work#{background}"),
                "pending".to_owned()
            ),
        ])
    );
    let foreground_node = watch
        .value()
        .tasks
        .get(&foreground.to_string())
        .expect("the foreground node");
    assert_eq!(foreground_node.conversations, vec![owned]);
    assert!(watch.value().tasks[&background.to_string()].background);

    let recorder = Arc::new(OpsRecorder {
        initial: serde_json::to_value(watch.value()).expect("serialize"),
        frames: Mutex::new(Vec::new()),
    });
    watch.start(Arc::clone(&recorder) as Arc<dyn WatchListener<TaskGraph>>);
    second.harness.resume().expect("resume");
    task_support::eventually(|| {
        let watch = Arc::clone(&watch);
        let key = background.to_string();
        async move {
            watch
                .value()
                .tasks
                .get(&key)
                .is_some_and(|node| matches!(node.state, TaskGraphState::Running { .. }))
        }
    })
    .await
    .expect("the surviving task runs");
    assert_eq!(
        second
            .harness
            .abort_task(background, &*task_support::context())
            .await
            .expect("abort"),
        kiana_durable::harness::types::AbortTaskOutcome::Marked
    );
    second
        .harness
        .wait_for_task(background, &*task_support::context())
        .await
        .expect("the aborted task settles");
    task_support::drained_for_tests().await;
    let frames = recorder.frames.lock().unwrap();
    let key = background.to_string();
    assert!(
        frames.iter().any(|(_, ops)| ops.iter().any(|op| matches!(
            op,
            Op::Set(path, node)
                if path == &vec![Seg::Key("tasks".to_owned()), Seg::Key(key.clone())]
                    && node
                        .as_object()
                        .is_some_and(|node| node.get("abortRequested") == Some(&serde_json::json!(true)))
        ))),
        "the abort mark reaches a frame"
    );
    let (last_value, last_ops) = frames.last().expect("frames");
    assert_eq!(
        last_ops,
        &vec![Op::Delete(vec![
            Seg::Key("tasks".to_owned()),
            Seg::Key(key.clone()),
        ])],
        "the terminal task is deleted in the last frame"
    );
    let mut replica = recorder.initial.clone();
    for (_, ops) in frames.iter() {
        replica = kiana_chord::delta::index::apply_immutable(replica, ops).expect("ops apply");
    }
    assert_eq!(
        replica["tasks"]
            .as_object()
            .expect("tasks")
            .keys()
            .collect::<Vec<_>>(),
        vec![&foreground.to_string()],
        "only the surviving task remains"
    );
    assert_eq!(
        serde_json::to_value(last_value).expect("serialize"),
        replica,
        "the last frame is the replayed value"
    );
    drop(frames);
    let _ = watch.stop().await;
    gate.resolve();
    second
        .harness
        .wait_for_task(foreground, &*task_support::context())
        .await
        .expect("the foreground task completes");
    second.harness.close(&*task_support::context()).await;
}
