//! `test/harness-lifecycle.test.ts` 的 Rust 端口：打开失败、关闭顺序、关闭时的提交、
//! 关死后拒绝的操作、封印时排队的读写，以及 resume 时的 registry。
//!
//! 与 TS 的差异见文件末尾的说明（hook 变体与 watch 获取取消的订阅计数）。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;
#[path = "support/task.rs"]
mod task_support;

use std::any::Any;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::{AbortReason, AbortSignal, Context, ContextRef, with_abort_signal};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::types::{
    AnyTask, BlockedReason, Conversation as ConversationTrait, ConversationCreateOptions,
    EntryRangeQuery, RootOptions, SchedulingState, SubmissionDraft, SubmissionDraftKind,
    TaskInspectionState, TaskRuntime, TaskRuntimeChange,
};
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, DocDefinition, DocTarget,
    DocumentHistory, DocumentObserver, DocumentReader, DocumentSemantics, EntryDraft, EntryId,
    IdKind, JsonObject, JsonValue, ScopeKind, Session, SessionChange, Storage, StorageWrite,
    TaskDefinition, TaskId, TaskOptions, TaskOwnership, TaskPhase, TaskRecord, TaskState, Tx,
    WatchEnd, define_doc, id_from_number,
};
use task_support::{Deferred, TasksFixture, open_root, open_tasks};

type RunFuture = Pin<Box<dyn std::future::Future<Output = Result<(), BoxError>> + Send>>;
type RunFn = Arc<dyn Fn() -> RunFuture + Send + Sync>;

fn context() -> ContextRef {
    task_support::context()
}

fn io_error(message: &str) -> BoxError {
    Box::new(std::io::Error::other(message.to_owned()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// 一个只跑 `run` 阶段并以 null 完成的任务
// ---------------------------------------------------------------------------

struct CompleteTask;

#[async_trait]
impl TaskRuntimeChange for CompleteTask {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: kiana_durable::types::TaskOutcome::Completed {
                result: JsonValue::Null,
            },
        }))
    }
}

struct RunFnPhase {
    run: RunFn,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for RunFnPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        (self.run)().await?;
        runtime
            .commit(&CompleteTask, context, CommitOptions::default())
            .await
    }
}

struct NoopPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for NoopPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

fn one_step(name: &str, run: RunFn) -> AnyTask {
    let definition = TaskDefinition {
        name: name.to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({ "phase": "run" })),
        phases: IndexMap::from([(
            "run".to_owned(),
            Arc::new(RunFnPhase { run }) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: Arc::new(NoopPhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

fn noop_step(name: &str) -> AnyTask {
    one_step(name, Arc::new(|| Box::pin(async { Ok(()) })))
}

struct CreateTask {
    task: AnyTask,
    background: bool,
}

#[async_trait]
impl SessionChange for CreateTask {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut options = TaskOptions::new(TaskOwnership::Conversation);
        options.background = self.background.then_some(true);
        let id = tx.create_task(&self.task, JsonValue::Null, options).await?;
        Ok(Box::new(id))
    }
}

async fn start(
    conversation: &Arc<dyn ConversationTrait>,
    task: AnyTask,
    background: bool,
) -> TaskId {
    let value = conversation
        .commit(
            &CreateTask { task, background },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("create the task");
    *value.downcast::<TaskId>().expect("a task ID")
}

/// 一次原始 Storage 读取是否仍然成功（被 join 的调用依赖这一点）。
async fn storage_open(storage: &support::MemoryStorage) -> bool {
    Storage::task(storage, id_from_number(1), &*context())
        .await
        .is_ok()
}

/// 一个统计 `subscribe` 次数的 registry 读端。
struct CountingRegistry {
    inner: Arc<kiana_durable::harness::registry::RegistryImpl>,
    subscriptions: Arc<std::sync::atomic::AtomicUsize>,
}

impl kiana_durable::harness::types::RegistryReader for CountingRegistry {
    fn snapshot(&self) -> Arc<dyn kiana_durable::harness::types::RegistrySnapshot> {
        self.inner.snapshot()
    }

    fn subscribe(
        &self,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> kiana_durable::types::Unsubscribe {
        let count = Arc::clone(&self.subscriptions);
        count.fetch_add(1, Ordering::SeqCst);
        let unsubscribe = self.inner.subscribe(listener);
        Box::new(move || {
            count.fetch_sub(1, Ordering::SeqCst);
            unsubscribe();
        })
    }
}

fn open_options(
    registry: Arc<dyn kiana_durable::harness::types::RegistryReader>,
) -> kiana_durable::harness::types::HarnessOptions {
    kiana_durable::harness::types::HarnessOptions {
        telemetry: None,
        models: kiana_durable::harness::types::ModelsProvider::Direct(
            kiana_ai::models::create_models(Default::default()),
        ),
        identity: None,
        registry,
        settings: None,
        env: None,
        conversation_created: None,
        now: None,
        set_timer: None,
        on_wake: None,
        on_report: None,
        read_only: None,
    }
}

/// 留下一份 `running` 任务（一次崩溃后的样子），让 open 的 reconcile 提交有事可做。
async fn seed_running_task(storage: &support::ControlledStorage) {
    use kiana_durable::types::Storage as _;
    let conversation_id: ConversationId =
        id_from_number(storage.mint_id(IdKind::Conversation).await.expect("mint"));
    let id: TaskId = id_from_number(storage.mint_id(IdKind::Task).await.expect("mint"));
    let task = TaskRecord {
        id,
        conversation_id,
        kind: "test.seeded".to_owned(),
        version: 1,
        input: JsonValue::Null,
        trace: None,
        links: None,
        owner: None,
        background: false,
        abort_requested: false,
        abandon_on_restart: None,
        state: TaskState::Running {
            checkpoint: serde_json::json!({ "phase": "run" }),
        },
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    };
    storage
        .commit(
            &[
                StorageWrite::Conversation {
                    value: kiana_durable::types::ConversationRecord {
                        id: conversation_id,
                        parent: None,
                        owner: None,
                    },
                },
                StorageWrite::Task { value: task },
            ],
            Box::new(|_| {}),
            &*context(),
        )
        .await
        .expect("seed the running task");
}

// ---------------------------------------------------------------------------
// Harness open
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn open_closes_without_a_cancelled_caller_context_and_rethrows_the_original_error() {
    let storage = Arc::new(support::ControlledStorage::new());
    seed_running_task(&storage).await;
    let subscriptions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader: Arc<dyn kiana_durable::harness::types::RegistryReader> =
        Arc::new(CountingRegistry {
            inner: kiana_durable::harness::registry::create_registry_impl(),
            subscriptions: Arc::clone(&subscriptions),
        });
    let held = storage.hold_commits();
    storage.fail_next_commit(io_error("disk full"));

    let signal = AbortSignal::new();
    let caller = with_abort_signal(signal.clone(), context());
    let storage_handle: Arc<dyn Storage> = Arc::clone(&storage) as Arc<dyn Storage>;
    let mut opening = Box::pin(Harness::open(
        storage_handle,
        open_options(reader),
        &*caller,
    ));
    tokio::select! {
        _ = held.entered() => {}
        _result = &mut opening => panic!("open finished before the commit was held"),
    }
    signal.abort(Some(AbortReason::error(std::io::Error::other(
        "caller gave up",
    ))));
    held.release();
    let error = match opening.await {
        Ok(_) => panic!("open must fail with the storage error"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("disk full"), "{error}");
    assert_eq!(subscriptions.load(Ordering::SeqCst), 0);
    assert!(!storage_open(storage.inner()).await);
}

// ---------------------------------------------------------------------------
// Harness close
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn joins_a_task_handler_that_ignores_its_signal_before_closing_storage() {
    let storage = Arc::new(support::MemoryStorage::new());
    let reached = Arc::new(Deferred::new());
    let gate = Arc::new(Deferred::new());
    let read_after_release = Arc::new(AtomicBool::new(false));
    let stubborn = {
        let reached = Arc::clone(&reached);
        let gate = Arc::clone(&gate);
        let read_after_release = Arc::clone(&read_after_release);
        let storage = Arc::clone(&storage);
        one_step(
            "test.close-stubborn",
            Arc::new(move || {
                let reached = Arc::clone(&reached);
                let gate = Arc::clone(&gate);
                let read_after_release = Arc::clone(&read_after_release);
                let storage = Arc::clone(&storage);
                Box::pin(async move {
                    reached.resolve(());
                    let _ = gate.promise().await;
                    if storage_open(&storage).await {
                        read_after_release.store(true, Ordering::SeqCst);
                    }
                    Ok(())
                })
            }),
        )
    };
    let task_for_start = stubborn.clone();
    let fixture: TasksFixture =
        open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, vec![stubborn]).await;
    let root = open_root(&fixture.harness).await;
    let _id = start(&root, task_for_start, false).await;
    fixture.harness.resume().expect("resume");
    reached.promise().await.expect("the handler reached");

    let mut closing = Box::pin(fixture.harness.close_result(context()));
    assert!(
        !task_support::settled(closing.as_mut()).await,
        "close must wait for the stubborn handler"
    );
    assert!(storage_open(&storage).await);
    gate.resolve(());
    closing.await.expect("close resolves");
    assert!(read_after_release.load(Ordering::SeqCst));
    assert!(!storage_open(&storage).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_shutting_down_after_a_cancelled_close_and_a_second_close_awaits_the_same_shutdown() {
    let storage = Arc::new(support::MemoryStorage::new());
    let reached = Arc::new(Deferred::new());
    let gate = Arc::new(Deferred::new());
    let reached_run = Arc::clone(&reached);
    let gate_run = Arc::clone(&gate);
    let stubborn = one_step(
        "test.close-cancelled",
        Arc::new(move || {
            let reached = Arc::clone(&reached_run);
            let gate = Arc::clone(&gate_run);
            Box::pin(async move {
                reached.resolve(());
                let _ = gate.promise().await;
                Ok(())
            })
        }),
    );
    let task_for_start = stubborn.clone();
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, vec![stubborn]).await;
    let root = open_root(&fixture.harness).await;
    let _ = start(&root, task_for_start, false).await;
    fixture.harness.resume().expect("resume");
    reached.promise().await.expect("the handler reached");

    let signal = AbortSignal::new();
    let cancelled_context = with_abort_signal(signal.clone(), context());
    let cancelled = Box::pin(fixture.harness.close_result(cancelled_context));
    tokio::time::sleep(Duration::from_millis(1)).await;
    signal.abort(Some(AbortReason::error(std::io::Error::other(
        "stop waiting",
    ))));
    let error = cancelled
        .await
        .expect_err("the cancelled waiter gets the abort reason");
    assert!(error.to_string().contains("stop waiting"), "{error}");

    // 接纳保持封闭，调用仍然占着 Storage。
    let refused = fixture
        .harness
        .commit(&NoChange, &*context(), CommitOptions::default())
        .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("closed")),
        "{refused:?}"
    );
    assert!(storage_open(&storage).await);
    let mut second = Box::pin(fixture.harness.close_result(context()));
    assert!(!task_support::settled(second.as_mut()).await);
    gate.resolve(());
    second.await.expect("the second close resolves");
    assert!(!storage_open(&storage).await);
}

struct NoChange;

#[async_trait]
impl SessionChange for NoChange {
    async fn run(&self, _tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        Ok(Box::new(()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn settles_durably_a_commit_whose_committer_was_cancelled_while_it_was_in_storage() {
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let held = storage.hold_commits();
    let signal = AbortSignal::new();
    let caller = with_abort_signal(signal.clone(), context());
    let root_clone = Arc::clone(&root);
    let mut committing = Box::pin(async move {
        root_clone
            .commit(
                &AppendNote {
                    conversation: root_clone.id(),
                },
                &*caller,
                CommitOptions::default(),
            )
            .await
    });
    tokio::select! {
        _ = held.entered() => {}
        _result = &mut committing => panic!("commit finished before the gate"),
    }
    signal.abort(Some(AbortReason::error(std::io::Error::other(
        "committer gave up",
    ))));
    held.release();
    let id = *committing
        .await
        .expect("the commit settles")
        .downcast::<EntryId>()
        .expect("entry id");
    let page = root
        .entries(EntryRangeQuery::default(), 10, None, &*context())
        .await
        .expect("entries");
    assert_eq!(
        page.items.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![id]
    );
    fixture.harness.close(&*context()).await;
}

struct AppendNote {
    conversation: ConversationId,
}

#[async_trait]
impl SessionChange for AppendNote {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let entry = tx
            .append_entry(
                self.conversation,
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
        Ok(Box::new(entry.id))
    }
}

fn notes_definition() -> kiana_durable::types::DocToken {
    define_doc(
        DocDefinition::new(
            "test.close-frames",
            1,
            DocumentSemantics {
                scope: ScopeKind::Session,
                history: Some(DocumentHistory::Latest),
                fork: None,
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("text".to_owned(), JsonValue::String(String::new()));
            value
        }),
    )
}

struct SetNotes {
    text: String,
}

#[async_trait]
impl SessionChange for SetNotes {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut draft = tx.doc(&DocTarget::session(&notes_definition())).await?;
        draft
            .value_mut()
            .insert("text".to_owned(), JsonValue::from(self.text.clone()));
        Ok(Box::new(()))
    }
}

struct CloseDuring {
    root: ConversationId,
    task: AnyTask,
}

#[async_trait]
impl SessionChange for CloseDuring {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut draft = tx.doc(&DocTarget::session(&notes_definition())).await?;
        draft
            .value_mut()
            .insert("text".to_owned(), JsonValue::from("during close"));
        tx.append_entry(
            self.root,
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
        let mut options = TaskOptions::new(TaskOwnership::Conversation);
        options.conversation_id = Some(self.root);
        tx.create_task(&self.task, JsonValue::Null, options).await?;
        Ok(Box::new(()))
    }
}

struct FrameRecorder {
    frames: Arc<Mutex<Vec<String>>>,
    name: &'static str,
}

#[async_trait]
impl<T: Send + Sync + 'static> kiana_durable::types::WatchListener<T> for FrameRecorder {
    async fn call(&self, _value: T, _ops: Vec<kiana_chord::delta::Op>, _context: Arc<dyn Context>) {
        lock(&self.frames).push(self.name.to_owned());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn publishes_no_frame_to_watches_from_a_commit_that_settles_during_close() {
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let notes = notes_definition();
    fixture
        .harness
        .commit(
            &SetNotes {
                text: "before".to_owned(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("seed the notes");
    let doc_watch = fixture
        .harness
        .watch_doc(&DocTarget::session(&notes), &*context())
        .await
        .expect("watch")
        .expect("the notes exist");
    let view_watch = root.watch(&*context()).await.expect("watch");
    let graph_watch = fixture
        .harness
        .watch_task_graph(&*context())
        .await
        .expect("watch");
    let frames: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    graph_watch.start(Arc::new(FrameRecorder {
        frames: Arc::clone(&frames),
        name: "graphWatch",
    }));
    doc_watch.start(Arc::new(FrameRecorder {
        frames: Arc::clone(&frames),
        name: "docWatch",
    }));
    view_watch.start(Arc::new(FrameRecorder {
        frames: Arc::clone(&frames),
        name: "viewWatch",
    }));
    let doc_value = doc_watch.value().clone();
    let view_entries: Vec<EntryId> = view_watch
        .value()
        .entries
        .iter()
        .map(|entry| entry.id)
        .collect();

    let held = storage.hold_commits();
    let harness_for_commit = Arc::clone(&fixture.harness);
    let root_id = root.id();
    let task = noop_step("test.close-graph");
    let mut committing = Box::pin(async move {
        harness_for_commit
            .commit(
                &CloseDuring {
                    root: root_id,
                    task,
                },
                &*context(),
                CommitOptions::default(),
            )
            .await
    });
    tokio::select! {
        _ = held.entered() => {}
        _result = &mut committing => panic!("commit finished before the gate"),
    }
    let closing = fixture.harness.close_result(context());
    held.release();
    committing.await.expect("the commit settles");
    closing.await.expect("close resolves");
    task_support::drained_for_tests().await;

    let frame_names = lock(&frames).clone();
    assert!(
        frame_names.is_empty(),
        "no watch frame during close: {frame_names:?}"
    );
    assert_eq!(doc_watch.value(), &doc_value);
    assert_eq!(
        view_watch
            .value()
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        view_entries
    );
    assert!(graph_watch.value().tasks.is_empty());
    assert!(matches!(graph_watch.closed().await, WatchEnd::Ended));
    assert!(matches!(doc_watch.closed().await, WatchEnd::Ended));
    assert!(matches!(view_watch.closed().await, WatchEnd::Ended));
    assert!(
        lock(&storage.commits).last().is_some_and(|batch| batch
            .iter()
            .any(|write| matches!(write, StorageWrite::Entry { .. }))),
        "the commit itself settled"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_conversation_and_harness_operations_once_close_begins() {
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let entry = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("append");
    let entry_id = *entry.downcast::<EntryId>().expect("entry id");
    let submission = root
        .submit(
            SubmissionDraft {
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
            },
            &*context(),
        )
        .await
        .expect("submit");

    let held = storage.hold_commits();
    let harness_for_commit = Arc::clone(&fixture.harness);
    let root_for_block = Arc::clone(&root);
    let mut blocking = Box::pin(async move {
        harness_for_commit
            .commit(
                &AppendNote {
                    conversation: root_for_block.id(),
                },
                &*context(),
                CommitOptions::default(),
            )
            .await
    });
    tokio::select! {
        _ = held.entered() => {}
        _result = &mut blocking => panic!("commit finished before the gate"),
    }
    // 关闭之前排队的 inspect 报告 closing。
    //
    // 先 poll 一次让两个 future 按固定顺序入队；用 spawn + sleep(1ms) 在并行负载下
    // 无法保证 inspect 比 close 先入队（曾导致全量测试偶发失败）。
    let inspect_context = context();
    let mut inspector = Box::pin(fixture.harness.inspect(&*inspect_context));
    assert!(
        futures::poll!(inspector.as_mut()).is_pending(),
        "inspect is queued behind the held commit"
    );
    let mut closer = Box::pin(fixture.harness.close_result(context()));
    assert!(
        futures::poll!(closer.as_mut()).is_pending(),
        "close waits for the queued inspect"
    );

    async fn classify<T>(result: Result<T, BoxError>) -> String {
        match result {
            Ok(_) => "resolved".to_owned(),
            Err(error) => {
                let message = error.to_string();
                if message.contains("closed") {
                    "closed".to_owned()
                } else {
                    message
                }
            }
        }
    }

    let ctx = context();
    let operations: Vec<(&str, String)> = vec![
        (
            "submit",
            classify(
                root.submit(
                    SubmissionDraft {
                        request_id: None,
                        kind: SubmissionDraftKind::Input {
                            content: kiana_durable::harness::types::UserInput::Text("x".to_owned()),
                            when_busy: None,
                            entry: None,
                        },
                    },
                    &*ctx,
                )
                .await,
            )
            .await,
        ),
        ("agent", classify(root.agent(&*ctx).await).await),
        (
            "configure",
            classify(root.configure(Default::default(), &*ctx).await).await,
        ),
        (
            "commit",
            classify(
                fixture
                    .harness
                    .commit(&NoChange, &*ctx, CommitOptions::default())
                    .await,
            )
            .await,
        ),
        (
            "context",
            classify(root.context(&*ctx, Default::default()).await).await,
        ),
        (
            "entries",
            classify(
                root.entries(EntryRangeQuery::default(), 10, None, &*ctx)
                    .await,
            )
            .await,
        ),
        (
            "fork",
            classify(
                root.fork(
                    entry_id,
                    ConversationCreateOptions {
                        ownership: ConversationOwnership::Ownerless,
                        agent: None,
                        init: None,
                    },
                    &*ctx,
                )
                .await,
            )
            .await,
        ),
        ("compact", classify(root.compact(None, &*ctx).await).await),
        ("reset", classify(root.reset(None, &*ctx).await).await),
        (
            "abort",
            classify(root.abort(&*ctx, Default::default()).await).await,
        ),
        (
            "conversationIdle",
            classify(root.wait_for_idle(&*ctx).await).await,
        ),
        ("watch", classify(root.watch(&*ctx).await).await),
        (
            "watchTaskGraph",
            classify(fixture.harness.watch_task_graph(&*ctx).await).await,
        ),
        ("status", classify(submission.status(&*ctx).await).await),
        ("wait", classify(submission.wait(&*ctx).await).await),
        (
            "abortSubmission",
            classify(submission.abort(&*ctx).await).await,
        ),
        (
            "root",
            classify(fixture.harness.root(&*ctx, RootOptions::default()).await).await,
        ),
        (
            "conversation",
            classify(fixture.harness.conversation(root.id(), &*ctx).await).await,
        ),
        (
            "createConversation",
            classify(
                fixture
                    .harness
                    .create_conversation(
                        ConversationCreateOptions {
                            ownership: ConversationOwnership::Ownerless,
                            agent: None,
                            init: None,
                        },
                        &*ctx,
                    )
                    .await,
            )
            .await,
        ),
        (
            "getTask",
            classify(fixture.harness.get_task(id_from_number(1), &*ctx).await).await,
        ),
        (
            "inspect",
            classify(fixture.harness.inspect(&*ctx).await).await,
        ),
        (
            "submission",
            classify(fixture.harness.submission(submission.id(), &*ctx).await).await,
        ),
        (
            "abortTask",
            classify(fixture.harness.abort_task(id_from_number(1), &*ctx).await).await,
        ),
        (
            "waitForTask",
            classify(
                fixture
                    .harness
                    .wait_for_task(id_from_number(1), &*ctx)
                    .await,
            )
            .await,
        ),
        (
            "harnessIdle",
            classify(fixture.harness.wait_for_idle(&*ctx).await).await,
        ),
        ("usage", classify(fixture.harness.usage(&*ctx).await).await),
    ];
    for (name, outcome) in &operations {
        assert_eq!(
            outcome, "closed",
            "{name} must be refused after close begins"
        );
    }
    let resume = fixture.harness.resume();
    assert!(
        resume
            .as_ref()
            .is_err_and(|error| error.to_string().contains("closed")),
        "{resume:?}"
    );
    held.release();
    blocking.await.expect("the blocking commit settles");
    let queued = inspector.await.expect("inspect");
    assert_eq!(queued.scheduling, SchedulingState::Closing);
    closer.await.expect("close resolves");
}

#[tokio::test(flavor = "multi_thread")]
async fn completes_reads_queued_at_the_seal_and_rejects_queued_waits_and_acquisitions() {
    let notes = notes_definition();
    let absent = define_doc(
        DocDefinition::new(
            "test.queued-absent",
            1,
            DocumentSemantics {
                scope: ScopeKind::Session,
                history: Some(DocumentHistory::Latest),
                fork: None,
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("text".to_owned(), JsonValue::String(String::new()));
            value
        }),
    );
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    fixture
        .harness
        .commit(
            &SetNotes {
                text: "x".to_owned(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("seed notes");
    let task_id = start(&root, noop_step("test.queued-pending"), false).await;
    let write = root
        .submit(
            SubmissionDraft {
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
            },
            &*context(),
        )
        .await
        .expect("submit");
    write.wait(&*context()).await.expect("the write settles");

    let held = storage.hold_commits();
    let harness_for_commit = Arc::clone(&fixture.harness);
    let root_id = root.id();
    let mut blocking = Box::pin(async move {
        harness_for_commit
            .commit(
                &AppendNote {
                    conversation: root_id,
                },
                &*context(),
                CommitOptions::default(),
            )
            .await
    });
    tokio::select! {
        _ = held.entered() => {}
        _result = &mut blocking => panic!("commit finished before the gate"),
    }

    async fn outcome<T>(result: Result<T, BoxError>) -> String {
        match result {
            Ok(_) => "resolved".to_owned(),
            Err(error) => {
                let message = error.to_string();
                if message.contains("closed") {
                    "closed".to_owned()
                } else {
                    message
                }
            }
        }
    }

    // 每个操作各自入队：Rust 的 future 是惰性的，同一个任务里顺序 await 的第二个读取
    // 在封印之后才会开始。
    let ctx = context();
    let context_read = {
        let root = Arc::clone(&root);
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move { outcome(root.context(&*ctx, Default::default()).await).await })
    };
    let snapshot = {
        let harness = Arc::clone(&fixture.harness);
        let ctx = Arc::clone(&ctx);
        let notes = notes.clone();
        tokio::spawn(async move {
            outcome(harness.snapshot(&DocTarget::session(&notes), &*ctx).await).await
        })
    };
    let settled_wait = {
        let write = Arc::clone(&write);
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move { outcome(write.wait(&*ctx).await).await })
    };
    let absent_watch = {
        let harness = Arc::clone(&fixture.harness);
        let ctx = Arc::clone(&ctx);
        let absent = absent.clone();
        tokio::spawn(async move {
            match harness.watch_doc(&DocTarget::session(&absent), &*ctx).await {
                Ok(None) => "undefined".to_owned(),
                Ok(Some(_)) => "resolved".to_owned(),
                Err(error) => outcome::<()>(Err(error)).await,
            }
        })
    };
    let wait_for_task = {
        let harness = Arc::clone(&fixture.harness);
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move { outcome(harness.wait_for_task(task_id, &*ctx).await).await })
    };
    let watch_doc = {
        let harness = Arc::clone(&fixture.harness);
        let ctx = Arc::clone(&ctx);
        let notes = notes.clone();
        tokio::spawn(async move {
            outcome(harness.watch_doc(&DocTarget::session(&notes), &*ctx).await).await
        })
    };
    let watch = {
        let root = Arc::clone(&root);
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move { outcome(root.watch(&*ctx).await).await })
    };
    let watch_task_graph = {
        let harness = Arc::clone(&fixture.harness);
        tokio::spawn(async move { outcome(harness.watch_task_graph(&*context()).await).await })
    };
    tokio::time::sleep(Duration::from_millis(5)).await;
    let closer = {
        let harness = Arc::clone(&fixture.harness);
        tokio::spawn(async move { harness.close_result(context()).await })
    };
    held.release();
    blocking.await.expect("the blocking commit settles");
    let outcomes = (
        context_read.await.expect("context read"),
        snapshot.await.expect("snapshot"),
        settled_wait.await.expect("settled wait"),
        absent_watch.await.expect("absent watch"),
        wait_for_task.await.expect("waitForTask"),
        watch_doc.await.expect("watchDoc"),
        watch.await.expect("watch"),
        watch_task_graph.await.expect("watchTaskGraph"),
    );
    assert_eq!(
        outcomes,
        (
            "resolved".to_owned(),
            "resolved".to_owned(),
            "resolved".to_owned(),
            "undefined".to_owned(),
            "closed".to_owned(),
            "closed".to_owned(),
            "closed".to_owned(),
            "closed".to_owned(),
        )
    );
    closer.await.expect("close task").expect("close resolves");
}

// ---------------------------------------------------------------------------
// 暂停的 Harness 上的调度
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn never_schedules_from_a_read_only_viewer() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_task = Arc::clone(&ran);
    let marker = one_step(
        "test.marker",
        Arc::new(move || {
            let ran = Arc::clone(&ran_task);
            Box::pin(async move {
                ran.store(true, Ordering::SeqCst);
                Ok(())
            })
        }),
    );
    let storage = Arc::new(support::MemoryStorage::new());
    let task_for_start = marker.clone();
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, vec![marker]).await;
    let root = open_root(&fixture.harness).await;
    // 后台任务：idle 等待与会话 abort 都不碰它。
    let id = start(&root, task_for_start, true).await;
    let submission_id = fixture
        .harness
        .commit(
            &QueueWrite {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("queue the write");
    let submission_id = *submission_id
        .downcast::<kiana_durable::types::SubmissionId>()
        .expect("submission id");

    let notes = notes_definition();
    fixture
        .harness
        .commit(
            &SetNotes {
                text: String::new(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("seed notes");
    let _ = fixture.harness.inspect(&*context()).await.expect("inspect");
    let _ = fixture
        .harness
        .get_task(id, &*context())
        .await
        .expect("get");
    let _ = fixture.harness.usage(&*context()).await.expect("usage");
    let _ = fixture
        .harness
        .conversation(root.id(), &*context())
        .await
        .expect("conversation");
    let _ = fixture
        .harness
        .snapshot(&DocTarget::session(&notes), &*context())
        .await
        .expect("snapshot");
    fixture
        .harness
        .watch_doc(&DocTarget::session(&notes), &*context())
        .await
        .expect("watch")
        .expect("the notes exist")
        .stop()
        .await;
    let submission = fixture
        .harness
        .submission(submission_id, &*context())
        .await
        .expect("submission")
        .expect("the submission exists");
    let _ = submission.status(&*context()).await.expect("status");
    let _ = root.agent(&*context()).await.expect("agent");
    let _ = root
        .context(&*context(), Default::default())
        .await
        .expect("context");
    let _ = root
        .entries(EntryRangeQuery::default(), 10, None, &*context())
        .await
        .expect("entries");
    root.watch(&*context()).await.expect("watch").stop().await;
    fixture
        .harness
        .watch_task_graph(&*context())
        .await
        .expect("watch")
        .stop()
        .await;
    task_support::drained_for_tests().await;
    task_support::drained_for_tests().await;
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(
        fixture
            .harness
            .inspect(&*context())
            .await
            .expect("inspect")
            .scheduling,
        SchedulingState::Paused
    );
    fixture.harness.close(&*context()).await;
}

struct QueueWrite {
    conversation: ConversationId,
}

#[async_trait]
impl SessionChange for QueueWrite {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let record = tx
            .create_submission(kiana_durable::types::SubmissionCreate {
                trace: None,
                conversation_id: self.conversation,
                request_id: None,
                kind: kiana_durable::types::SubmissionKind::Write,
                status: kiana_durable::types::SubmissionStatus::Queued,
                entry: None,
                answer: None,
                reason: None,
                detail: None,
            })
            .await?;
        Ok(Box::new(record.id))
    }
}

// ---------------------------------------------------------------------------
// resume 之前的 registry 变化
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn runs_what_the_registry_holds_at_resume_after_open() {
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let define = |label: &'static str| {
        let log = Arc::clone(&log);
        one_step(
            "test.late-definition",
            Arc::new(move || {
                let log = Arc::clone(&log);
                Box::pin(async move {
                    lock(&log).push(label.to_owned());
                    Ok(())
                })
            }),
        )
    };
    let storage = Arc::new(support::MemoryStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let missing = start(&root, define("unused"), false).await;
    let inspections = fixture.harness.inspect(&*context()).await.expect("inspect");
    assert!(matches!(
        inspections.tasks[0].state,
        TaskInspectionState::Blocked {
            reason: BlockedReason::MissingTask,
            ..
        }
    ));
    let installed = harness_support::add_task(&fixture.registry, define("v1"));
    let inspections = fixture.harness.inspect(&*context()).await.expect("inspect");
    assert!(matches!(
        inspections.tasks[0].state,
        TaskInspectionState::Ready { .. }
    ));
    // 同一个扩展名原位替换定义，仍然在 resume 之前。
    installed.dispose();
    harness_support::add_task(&fixture.registry, define("v2"));
    fixture.harness.resume().expect("resume");
    fixture
        .harness
        .wait_for_task(missing, &*context())
        .await
        .expect("the task settles");
    assert_eq!(lock(&log).clone(), vec!["v2".to_owned()]);
    fixture.harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_the_open_failure_once_and_rethrows_it() {
    // TS 的同一个用例还在 close 失败时断言只报告最初的打开错误；Rust 的
    // `Storage::close` 没有失败通道，因此只端口可观察的“报告一次 + 重新抛出”。
    let storage = Arc::new(support::ControlledStorage::new());
    seed_running_task(&storage).await;
    storage.fail_next_commit(io_error("disk full"));
    let reports: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    let mut options = open_options(kiana_durable::harness::registry::create_registry_impl());
    options.on_report = Some(Arc::new(
        move |error: &(dyn std::error::Error + Send + Sync),
              _info: &kiana_durable::types::ReportInfo| {
            lock(&sink).push(error.to_string());
        },
    ));
    let error = match Harness::open(
        Arc::clone(&storage) as Arc<dyn Storage>,
        options,
        &*context(),
    )
    .await
    {
        Ok(_) => panic!("the open must fail with the storage error"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("disk full"), "{error}");
    assert_eq!(lock(&reports).clone(), vec!["disk full".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn joins_a_tool_execute_that_ignores_its_signal_before_closing_storage() {
    use kiana_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxToolCallOptions, faux_assistant_message, faux_text,
        faux_tool_call_with,
    };
    use kiana_ai::types::{StopReason, Tool};
    use kiana_durable::harness::types::{ToolExecuteFn, ToolExecutionResult, ToolRegistration};

    let storage = Arc::new(support::MemoryStorage::new());
    let reached = Arc::new(Deferred::new());
    let gate = Arc::new(Deferred::new());
    let read_after_release = Arc::new(AtomicBool::new(false));
    let setup = chat_support::ChatSetup::new();
    let execute: ToolExecuteFn = {
        let reached = Arc::clone(&reached);
        let gate = Arc::clone(&gate);
        let read_after_release = Arc::clone(&read_after_release);
        let storage = Arc::clone(&storage);
        Arc::new(move |_arguments: JsonValue, _api, _context| {
            let reached = Arc::clone(&reached);
            let gate = Arc::clone(&gate);
            let read_after_release = Arc::clone(&read_after_release);
            let storage = Arc::clone(&storage);
            Box::pin(async move {
                reached.resolve(());
                let _ = gate.promise().await;
                if storage_open(&storage).await {
                    read_after_release.store(true, Ordering::SeqCst);
                }
                Ok(ToolExecutionResult::default())
            })
                as Pin<
                    Box<
                        dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>>
                            + Send,
                    >,
                >
        })
    };
    let mut registration = ToolRegistration::new(Tool {
        name: "wait".to_owned(),
        description: "wait".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    });
    registration.execute = Some(execute);
    harness_support::add_tool(&setup.registry, registration);
    setup.faux.set_responses(vec![
        faux_assistant_message(
            faux_tool_call_with(
                "wait",
                JsonObject::new(),
                FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                },
            ),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(faux_text("done"), Default::default()).into(),
    ]);
    let (harness, root) =
        chat_support::open_chat(Arc::clone(&storage) as Arc<dyn Storage>, &setup).await;
    root.submit(harness_support::text_input("go").into_draft(), &*context())
        .await
        .expect("submit");
    reached.promise().await.expect("the tool executes");
    let mut closing = Box::pin(harness.close_result(context()));
    assert!(
        !task_support::settled(closing.as_mut()).await,
        "close must wait for the stubborn tool execute"
    );
    assert!(storage_open(&storage).await);
    gate.resolve(());
    closing.await.expect("close resolves");
    assert!(read_after_release.load(Ordering::SeqCst));
    assert!(!storage_open(&storage).await);
}

/// TS 的同一个用例在 `where === "hook"` 时让 `beforeRequest` 忽略信号。
#[tokio::test(flavor = "multi_thread")]
async fn joins_a_hook_that_ignores_its_signal_before_closing_storage() {
    use kiana_durable::harness::define::hook;
    use kiana_durable::harness::generation::generation_task;
    use kiana_durable::harness::types::{
        BeforeRequestInput, GenerationHooks, HookApi, HookRegistration,
    };

    struct StubbornBeforeRequest {
        reached: Arc<Deferred>,
        gate: Arc<Deferred>,
    }

    #[async_trait]
    impl GenerationHooks for StubbornBeforeRequest {
        async fn before_request(
            &self,
            request: BeforeRequestInput,
            _api: Arc<dyn HookApi>,
            _context: &dyn Context,
        ) -> Result<Option<BeforeRequestInput>, BoxError> {
            self.reached.resolve(());
            let _ = self.gate.promise().await;
            Ok(Some(request))
        }

        async fn after_response(
            &self,
            _message: kiana_ai::types::AssistantMessage,
            _api: Arc<dyn HookApi>,
            _context: &dyn Context,
        ) -> Result<(), BoxError> {
            Ok(())
        }

        async fn on_yield(
            &self,
            _answer: kiana_ai::types::AssistantMessage,
            _api: Arc<dyn HookApi>,
            _context: &dyn Context,
        ) -> Result<Option<kiana_durable::harness::types::OnYieldOutput>, BoxError> {
            Ok(None)
        }

        async fn after_tools(
            &self,
            _assistant: EntryId,
            _results: Vec<EntryId>,
            _api: Arc<dyn HookApi>,
            _context: &dyn Context,
        ) -> Result<(), BoxError> {
            Ok(())
        }
    }

    let storage = Arc::new(support::MemoryStorage::new());
    let reached = Arc::new(Deferred::new());
    let gate = Arc::new(Deferred::new());
    let read_after_release = Arc::new(AtomicBool::new(false));
    let setup = chat_support::ChatSetup::new();
    let hooks: Vec<HookRegistration> = vec![hook(
        &generation_task(),
        Arc::new(StubbornBeforeRequest {
            reached: Arc::clone(&reached),
            gate: Arc::clone(&gate),
        }) as Arc<dyn GenerationHooks>,
    )];
    harness_support::add_hooks(&setup.registry, hooks, "hooks");
    setup.faux.set_responses(vec![chat_support::answer("done")]);
    let (harness, root) =
        chat_support::open_chat(Arc::clone(&storage) as Arc<dyn Storage>, &setup).await;
    root.submit(harness_support::text_input("go").into_draft(), &*context())
        .await
        .expect("submit");
    reached.promise().await.expect("the hook runs");
    let read_storage = Arc::clone(&storage);
    let read_flag = Arc::clone(&read_after_release);
    let reader = tokio::spawn(async move {
        if storage_open(&read_storage).await {
            read_flag.store(true, Ordering::SeqCst);
        }
    });
    let mut closing = Box::pin(harness.close_result(context()));
    assert!(
        !task_support::settled(closing.as_mut()).await,
        "close must wait for the stubborn hook"
    );
    gate.resolve(());
    closing.await.expect("close resolves");
    reader.await.expect("the reader joins");
    assert!(read_after_release.load(Ordering::SeqCst));
    assert!(!storage_open(&storage).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn lets_a_new_harness_open_the_same_storage_once_close_resolved() {
    let storage = Arc::new(support::MemoryStorage::new());
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let generation = Arc::new(AtomicUsize::new(1));
    let gate = Arc::new(Deferred::new());
    let stubborn = {
        let log = Arc::clone(&log);
        let generation = Arc::clone(&generation);
        let gate = Arc::clone(&gate);
        one_step(
            "test.generations",
            Arc::new(move || {
                let log = Arc::clone(&log);
                let generation = Arc::clone(&generation);
                let gate = Arc::clone(&gate);
                Box::pin(async move {
                    let mine = generation.load(Ordering::SeqCst);
                    lock(&log).push(format!("start {mine}"));
                    if mine == 1 {
                        let _ = gate.promise().await;
                    }
                    lock(&log).push(format!("end {mine}"));
                    Ok(())
                })
            }),
        )
    };
    let first = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![stubborn.clone()],
    )
    .await;
    let root = open_root(&first.harness).await;
    let id = start(&root, stubborn.clone(), false).await;
    first.harness.resume().expect("resume");
    task_support::eventually(|| {
        let log = Arc::clone(&log);
        async move { lock(&log).len() == 1 }
    })
    .await
    .expect("the first generation starts");
    let mut closing = {
        let harness = Arc::clone(&first.harness);
        let log = Arc::clone(&log);
        Box::pin(async move {
            harness
                .close_result(context())
                .await
                .expect("close resolves");
            lock(&log).push("closed".to_owned());
        })
    };
    assert!(
        !task_support::settled(closing.as_mut()).await,
        "close must wait for the first invocation"
    );
    gate.resolve(());
    closing.await;
    assert_eq!(
        lock(&log).clone(),
        vec![
            "start 1".to_owned(),
            "end 1".to_owned(),
            "closed".to_owned()
        ]
    );
    generation.store(2, Ordering::SeqCst);
    storage.reopen();
    let second = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, vec![stubborn]).await;
    second.harness.resume().expect("resume");
    second
        .harness
        .wait_for_task(id, &*context())
        .await
        .expect("the surviving task runs again");
    assert_eq!(
        lock(&log).clone(),
        vec![
            "start 1".to_owned(),
            "end 1".to_owned(),
            "closed".to_owned(),
            "start 2".to_owned(),
            "end 2".to_owned(),
        ],
        "no old invocation code runs"
    );
    second.harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn schedules_from_every_progress_call() {
    use kiana_durable::types::SubmissionId;

    async fn paused() -> (
        TasksFixture,
        Arc<dyn ConversationTrait>,
        TaskId,
        SubmissionId,
        Arc<AtomicBool>,
    ) {
        let ran = Arc::new(AtomicBool::new(false));
        let marker = {
            let ran = Arc::clone(&ran);
            one_step(
                "test.progress-marker",
                Arc::new(move || {
                    let ran = Arc::clone(&ran);
                    Box::pin(async move {
                        ran.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                }),
            )
        };
        let storage = Arc::new(support::MemoryStorage::new());
        let task_for_start = marker.clone();
        let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, vec![marker]).await;
        let root = open_root(&fixture.harness).await;
        // 后台任务：idle 等待与会话 abort 都不碰它。
        let id = start(&root, task_for_start, true).await;
        let submission_id = fixture
            .harness
            .commit(
                &QueueWrite {
                    conversation: root.id(),
                },
                &*context(),
                CommitOptions::default(),
            )
            .await
            .expect("queue the write");
        let submission_id = *submission_id
            .downcast::<SubmissionId>()
            .expect("submission id");
        (fixture, root, id, submission_id, ran)
    }

    for name in [
        "submit",
        "compact",
        "abort",
        "conversationIdle",
        "submissionWait",
        "waitForTask",
        "harnessIdle",
    ] {
        let (fixture, root, id, submission_id, ran) = paused().await;
        let pending: tokio::task::JoinHandle<Result<(), BoxError>> = match name {
            "submit" => {
                let root = Arc::clone(&root);
                tokio::spawn(async move {
                    root.submit(
                        SubmissionDraft {
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
                        },
                        &*context(),
                    )
                    .await
                    .map(|_| ())
                })
            }
            "compact" => {
                let root = Arc::clone(&root);
                tokio::spawn(async move { root.compact(None, &*context()).await.map(|_| ()) })
            }
            "abort" => {
                let root = Arc::clone(&root);
                tokio::spawn(async move {
                    root.abort(&*context(), Default::default())
                        .await
                        .map(|_| ())
                })
            }
            "conversationIdle" => {
                let root = Arc::clone(&root);
                tokio::spawn(async move { root.wait_for_idle(&*context()).await })
            }
            "submissionWait" => {
                let harness = Arc::clone(&fixture.harness);
                tokio::spawn(async move {
                    let submission = harness
                        .submission(submission_id, &*context())
                        .await?
                        .expect("the queued submission exists");
                    submission.wait(&*context()).await.map(|_| ())
                })
            }
            "waitForTask" => {
                let harness = Arc::clone(&fixture.harness);
                tokio::spawn(
                    async move { harness.wait_for_task(id, &*context()).await.map(|_| ()) },
                )
            }
            "harnessIdle" => {
                let harness = Arc::clone(&fixture.harness);
                tokio::spawn(async move { harness.wait_for_idle(&*context()).await })
            }
            other => unreachable!("unknown call {other}"),
        };
        let ran_wait = Arc::clone(&ran);
        task_support::eventually(|| {
            let ran = Arc::clone(&ran_wait);
            async move { ran.load(Ordering::SeqCst) }
        })
        .await
        .unwrap_or_else(|_| panic!("{name} did not enable scheduling"));
        fixture.harness.close(&*context()).await;
        let _ = pending.await;
    }
}

fn cancelled_watch_definition() -> kiana_durable::types::DocToken {
    define_doc(
        DocDefinition::new(
            "test.cancelled-watch",
            1,
            DocumentSemantics {
                scope: ScopeKind::Session,
                history: Some(DocumentHistory::Latest),
                fork: None,
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("text".to_owned(), JsonValue::String(String::new()));
            value
        }),
    )
}

/// TS 用例还统计 `subscribeCommits` 的订阅数（Rust 的 Harness 不暴露可替换的订阅入口，
/// 内部观察者直接挂在 `SessionImpl` 上），并断言两个watch 的挂载对象身份不同（Rust 的
/// watch 按值交付，无法比较对象身份）。这里端口可观察的取消语义：在挂载加载期间取消的
/// 获取以取消原因失败，之后新的观察者正常工作。
#[tokio::test(flavor = "multi_thread")]
async fn leaves_nothing_behind_when_a_watch_acquisition_is_cancelled_on_the_line() {
    let notes = cancelled_watch_definition();
    let storage = Arc::new(support::ControlledStorage::new());
    let first = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let _root = open_root(&first.harness).await;
    first
        .harness
        .commit(
            &SetNotes {
                text: "x".to_owned(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("seed the notes");
    first.harness.close(&*context()).await;
    storage.reopen();

    let second = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let reopened = open_root(&second.harness).await;

    // 在一个 watcher 的获取加载文档时取消它。
    let find = storage.hold_find_document();
    let signal = AbortSignal::new();
    let caller = with_abort_signal(signal.clone(), context());
    let harness = Arc::clone(&second.harness);
    let target = DocTarget::session(&notes);
    let mut acquiring = Box::pin(async move { harness.watch_doc(&target, &*caller).await });
    tokio::select! {
        _ = find.entered() => {}
        _ = &mut acquiring => panic!("the acquisition finished before the load"),
    }
    signal.abort(Some(AbortReason::error(std::io::Error::other("cancelled"))));
    find.release();
    let error = match acquiring.await {
        Ok(_) => panic!("the cancelled acquisition fails"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cancelled"), "{error}");

    // 在视图 watch 在线构建挂载时取消它。
    let find = storage.hold_find_document();
    let signal = AbortSignal::new();
    let caller = with_abort_signal(signal.clone(), context());
    let conversation = Arc::clone(&reopened);
    let mut acquiring = Box::pin(async move { conversation.watch(&*caller).await });
    tokio::select! {
        _ = find.entered() => {}
        _ = &mut acquiring => panic!("the acquisition finished before the load"),
    }
    signal.abort(Some(AbortReason::error(std::io::Error::other("cancelled"))));
    find.release();
    let error = match acquiring.await {
        Ok(_) => panic!("the cancelled view acquisition fails"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cancelled"), "{error}");

    // 没有观察者留下挂载：后续观察者都能正常获取。
    let first_watch = reopened
        .watch(&*context())
        .await
        .expect("the first observer");
    let first_value = first_watch.value().clone();
    first_watch.stop().await;
    let second_watch = reopened
        .watch(&*context())
        .await
        .expect("the second observer");
    assert_eq!(second_watch.value(), &first_value);
    second_watch.stop().await;
    second.harness.close(&*context()).await;
}

// TS 用例未移植的部分（见交付说明）：
// - （已启用）hook 变体（`beforeRequest` 忽略关闭信号）：
//   `joins_a_hook_that_ignores_its_signal_before_closing_storage`。
// - 在线上取消 watch 获取后的 `subscribeCommits` 订阅计数，以及两个 watch 的挂载对象身份
//   比较：Rust 的 Harness 不暴露可替换的订阅入口，watch 按值交付；可观察的取消语义已端口。
