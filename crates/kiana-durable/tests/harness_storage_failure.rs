//! `test/harness-storage-failure.test.ts` 的 Rust 端口（可行子集）。
//!
//! 覆盖：一次失败的 Storage 调用如何结束调用、后续调用、等待、watch、事件流与运行中的任务；
//! `StorageRequestError`；失败后 Harness 自身调用被拒；宿主关闭期间的 Storage 失败。
//!
//! 未移植的用例与理由见文件末尾。

#![allow(clippy::await_holding_lock)]

mod support;
#[path = "support/task.rs"]
mod task_support;

use std::any::Any;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::{AbortReason, AbortSignal, Context};
use kiana_durable::harness::events::AgentEvent;
use kiana_durable::harness::live::LIVE_DOC;
use kiana_durable::harness::types::{
    AnyTask, Conversation as ConversationTrait, SubmissionDraft, SubmissionDraftKind,
    TaskRuntimeChange,
};
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocTarget, DocumentAddress, DocumentObserver, DocumentPoint, DocumentQuery, DocumentReader,
    DocumentRecord, EntryDraft, EntryQuery, EntryRecord, IdKind, JsonValue, Page, Seq, Session,
    SessionChange, SessionEnd, Storage, StorageRequestError, StorageWrite, StoredDocument,
    StoredEntry, SubmissionQuery, SubmissionRecord, TaskDefinition, TaskId, TaskOptions,
    TaskOwnership, TaskPhase, TaskQuery, TaskRecord, TaskRuntime, TaskState, Tx, WatchEnd,
    WatchListener,
};
use task_support::{OpenTasksOptions, TasksFixture, open_root, open_tasks, open_tasks_with};

type SignalLog = Arc<Mutex<Vec<String>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn context() -> kiana_chord::context::ContextRef {
    task_support::context()
}

fn io_error(message: &str) -> BoxError {
    Box::new(std::io::Error::other(message.to_owned()))
}

// ---------------------------------------------------------------------------
// 一个等待 abort 信号的任务（TS: `Waiting`）
// ---------------------------------------------------------------------------

struct WaitingPhase {
    signals: SignalLog,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for WaitingPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        let _ = task_support::aborted(&runtime.signal()).await;
        lock(&self.signals).push("aborted".to_owned());
        Ok(())
    }
}

struct AbortedTask;

#[async_trait]
impl TaskRuntimeChange for AbortedTask {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: kiana_durable::types::TaskOutcome::Aborted {
                reason: Some("aborted".to_owned()),
                result: None,
            },
        }))
    }
}

struct AbortPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&AbortedTask, context, CommitOptions::default())
            .await
    }
}

fn waiting_task(signals: SignalLog) -> AnyTask {
    let definition = TaskDefinition {
        name: "test.waiting".to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({ "phase": "work" })),
        phases: IndexMap::from([(
            "work".to_owned(),
            Arc::new(WaitingPhase { signals })
                as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: Arc::new(AbortPhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

struct CreateTask {
    task: AnyTask,
}

#[async_trait]
impl SessionChange for CreateTask {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let id = tx
            .create_task(
                &self.task,
                JsonValue::Null,
                TaskOptions::new(TaskOwnership::Conversation),
            )
            .await?;
        Ok(Box::new(id))
    }
}

async fn start(conversation: &Arc<dyn ConversationTrait>, task: AnyTask) -> TaskId {
    let value = conversation
        .commit(&CreateTask { task }, &*context(), CommitOptions::default())
        .await
        .expect("create the task");
    *value.downcast::<TaskId>().expect("task id")
}

async fn wait_running(harness: &kiana_durable::harness::harness::HarnessImpl, id: TaskId) {
    for _ in 0..2000 {
        let record = harness
            .get_task(id, &*context())
            .await
            .expect("get the task");
        if record.is_some_and(|record| {
            record.state.status() == kiana_durable::types::TaskStatus::Running
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the task never started");
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

struct EmptyListener;

#[async_trait]
impl WatchListener<Vec<AgentEvent>> for EmptyListener {
    async fn call(
        &self,
        _value: Vec<AgentEvent>,
        _ops: Vec<kiana_chord::delta::Op>,
        _context: Arc<dyn Context>,
    ) {
    }
}

fn is_session_failed(error: &BoxError) -> bool {
    error.to_string().contains("Session failed")
}

fn report_messages(fixture: &TasksFixture) -> Vec<String> {
    fixture.report_messages()
}

// ---------------------------------------------------------------------------
// 一次失败的 Storage 调用
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ends_everything_with_the_first_error() {
    let signals: SignalLog = Arc::new(Mutex::new(Vec::new()));
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![waiting_task(Arc::clone(&signals))],
    )
    .await;
    let root = open_root(&fixture.harness).await;
    let id = start(&root, waiting_task(Arc::clone(&signals))).await;
    fixture.harness.resume().expect("resume");
    wait_running(&fixture.harness, id).await;

    let task_wait = {
        let harness = Arc::clone(&fixture.harness);
        tokio::spawn(async move { harness.wait_for_task(id, &*context()).await })
    };
    let idle = {
        let harness = Arc::clone(&fixture.harness);
        tokio::spawn(async move { harness.wait_for_idle(&*context()).await })
    };
    let watch = fixture
        .harness
        .watch_doc(&DocTarget::conversation(&LIVE_DOC, root.id()), &*context())
        .await
        .expect("watch")
        .expect("kiana.live exists");
    let stream = fixture
        .harness
        .watch_events(root.id(), &*context())
        .await
        .expect("events");
    stream.start(Arc::new(EmptyListener));

    storage.fail_next_commit(io_error("disk gone"));
    // 撞上它的调用拿到 Storage 错误本身。
    let error = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert_eq!(error.to_string(), "disk gone");

    let task_error = task_wait
        .await
        .expect("task wait task")
        .expect_err("the task wait fails");
    assert!(is_session_failed(&task_error), "{task_error}");
    let idle_error = idle.await.expect("idle task").expect_err("idle fails");
    assert!(is_session_failed(&idle_error), "{idle_error}");
    assert!(matches!(
        watch.closed().await,
        WatchEnd::Failed(error) if error.to_string() == "disk gone"
    ));
    assert!(matches!(
        stream.closed().await,
        WatchEnd::Failed(error) if error.to_string() == "disk gone"
    ));
    // 之后的每次调用，以及现在才开始的等待。
    for later in [
        fixture.harness.get_task(id, &*context()).await.err(),
        fixture.harness.wait_for_idle(&*context()).await.err(),
        root.submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: kiana_durable::harness::types::UserInput::Text("x".to_owned()),
                    when_busy: None,
                    entry: None,
                },
            },
            &*context(),
        )
        .await
        .err(),
    ] {
        let later = later.expect("the later call fails");
        assert!(is_session_failed(&later), "{later}");
    }
    match fixture.harness.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "disk gone"),
        other => panic!("unexpected end: {other:?}"),
    }
    assert_eq!(lock(&signals).clone(), vec!["aborted".to_owned()]);
    assert_eq!(report_messages(&fixture), vec!["disk gone".to_owned()]);
    fixture.harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_it_for_a_storage_request_error_from_a_commit() {
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    storage.fail_next_commit(Box::new(StorageRequestError::new("bad batch")));
    let error = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert_eq!(error.to_string(), "bad batch");
    match fixture.harness.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "bad batch"),
        other => panic!("unexpected end: {other:?}"),
    }
    fixture.harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_the_harnesss_own_calls_with_session_failed_after_a_failure() {
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    storage.fail_next_commit(io_error("disk gone"));
    let error = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert_eq!(error.to_string(), "disk gone");
    let resume = fixture.harness.resume();
    assert!(resume.as_ref().is_err_and(is_session_failed), "{resume:?}");
    for result in [
        fixture
            .harness
            .root(&*context(), Default::default())
            .await
            .err(),
        fixture
            .harness
            .conversation(root.id(), &*context())
            .await
            .err(),
        fixture
            .harness
            .create_conversation(
                kiana_durable::harness::types::ConversationCreateOptions {
                    ownership: kiana_durable::types::ConversationOwnership::Ownerless,
                    agent: None,
                    init: None,
                },
                &*context(),
            )
            .await
            .err(),
    ] {
        let error = result.expect("the call fails");
        assert!(is_session_failed(&error), "{error}");
    }
    fixture.harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_it_for_a_storage_error_during_a_host_close_before_the_backend_closes() {
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let held = storage.hold_commits();
    storage.fail_next_commit(io_error("disk gone"));
    let harness_for_commit = Arc::clone(&fixture.harness);
    let root_id = root.id();
    let mut committing = Box::pin(async move {
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
        _result = &mut committing => panic!("commit finished before the gate"),
    }
    let closing = Box::pin(fixture.harness.close_result(context()));
    held.release();
    let error = committing.await.expect_err("the commit fails");
    assert_eq!(error.to_string(), "disk gone");
    closing.await.expect("close resolves");
    match fixture.harness.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "disk gone"),
        other => panic!("unexpected end: {other:?}"),
    }
    assert_eq!(report_messages(&fixture), vec!["disk gone".to_owned()]);
}

// ---------------------------------------------------------------------------
// 逐方法失败注入（TS: `FailingCalls extends ControlledStorage`）
// ---------------------------------------------------------------------------

/// 一个可被测试释放的闸门（`support::Gate` 的构造与信号是模块私有的）。
struct Hold {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Notify,
    released: AtomicBool,
}

impl Hold {
    fn new() -> Self {
        Hold {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Notify::new(),
            released: AtomicBool::new(false),
        }
    }

    fn signal_entered(&self) {
        self.entered.add_permits(1);
    }

    /// 等待被扣住的调用到达。
    pub async fn entered(&self) {
        let _ = self.entered.acquire().await;
    }

    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.release.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            if self.released.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.release.notified();
            if self.released.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

/// 让指定 Storage 方法各失败一次（TS 的 `FailingCalls`）：`task`、限长大于 1 的
/// `scanEntries`（上下文读取的范围扫描，在 Session 线之外），以及 `mintId`。
struct FaultyStorage {
    inner: Arc<support::MemoryStorage>,
    error: &'static str,
    failing: Mutex<HashSet<&'static str>>,
    admitted: AtomicUsize,
    held_range: Mutex<Option<Arc<Hold>>>,
    cancel: Mutex<Option<(AbortSignal, &'static str)>>,
    scan_log: Mutex<Vec<String>>,
}

impl FaultyStorage {
    fn new() -> Self {
        FaultyStorage {
            inner: Arc::new(support::MemoryStorage::new()),
            error: "disk gone",
            failing: Mutex::new(HashSet::new()),
            admitted: AtomicUsize::new(0),
            held_range: Mutex::new(None),
            cancel: Mutex::new(None),
            scan_log: Mutex::new(Vec::new()),
        }
    }

    fn fail(&self, method: &'static str) -> Result<(), BoxError> {
        if lock(&self.failing).remove(method) {
            Err(io_error(self.error))
        } else {
            Ok(())
        }
    }

    fn hold_range(&self) -> Arc<Hold> {
        let hold = Arc::new(Hold::new());
        *lock(&self.held_range) = Some(Arc::clone(&hold));
        hold
    }

    fn admitted(&self) -> usize {
        self.admitted.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Storage for FaultyStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: kiana_durable::types::AdoptFn,
        context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        self.admitted.fetch_add(1, Ordering::SeqCst);
        self.inner.commit(writes, adopt, context).await
    }

    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.fail("mintId")?;
        self.inner.mint_id(kind).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.inner
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: kiana_durable::types::EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.entry(id, context).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: kiana_durable::types::EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.visible_entry(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<kiana_durable::types::EntryId>,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        if let Some((signal, message)) = lock(&self.cancel).take() {
            signal.abort(Some(AbortReason::error(std::io::Error::other(
                message.to_owned(),
            ))));
            return Err(io_error(message));
        }
        if limit > 1 {
            lock(&self.scan_log).push(if cursor.is_none() { "first" } else { "next" }.to_owned());
            let held = lock(&self.held_range).clone();
            if let Some(held) = held {
                held.signal_entered();
                held.wait().await;
            }
            self.fail("scanRange")?;
        }
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.fail("task")?;
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: kiana_durable::types::SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.inner
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: kiana_durable::types::DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.inner
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &dyn Context) {
        lock(&self.scan_log).push("close".to_owned());
        self.inner.close(context).await;
    }
}

fn cursor(value: serde_json::Value) -> Cursor {
    match value {
        JsonValue::Object(object) => object.into_iter().collect(),
        _ => panic!("a cursor is an object"),
    }
}

// TS: "fails it from a read off the Session line, as a context read makes".
#[tokio::test(flavor = "multi_thread")]
async fn fails_it_from_a_read_off_the_session_line() {
    let signals: SignalLog = Arc::new(Mutex::new(Vec::new()));
    let storage = Arc::new(FaultyStorage::new());
    let fixture = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![waiting_task(Arc::clone(&signals))],
    )
    .await;
    let root = open_root(&fixture.harness).await;
    root.commit(
        &AppendNote {
            conversation: root.id(),
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("append");
    lock(&storage.failing).insert("scanRange");
    let error = root
        .context(&*context(), Default::default())
        .await
        .expect_err("the context read fails");
    assert_eq!(error.to_string(), "disk gone");
    match fixture.harness.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "disk gone"),
        other => panic!("unexpected end: {other:?}"),
    }
    assert_eq!(report_messages(&fixture), vec!["disk gone".to_owned()]);
    fixture.harness.close(&*context()).await;
}

// TS: "fails it from a failed ID mint".
#[tokio::test(flavor = "multi_thread")]
async fn fails_it_from_a_failed_id_mint() {
    let storage = Arc::new(FaultyStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    lock(&storage.failing).insert("mintId");
    let error = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert_eq!(error.to_string(), "disk gone");
    match fixture.harness.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "disk gone"),
        other => panic!("unexpected end: {other:?}"),
    }
    fixture.harness.close(&*context()).await;
}

struct ReadThenAppend {
    conversation: ConversationId,
}

#[async_trait]
impl SessionChange for ReadThenAppend {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let _ = tx.task(kiana_durable::types::id_from_number(1)).await;
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

// TS: "does not commit what a callback did after it caught a failed read".
#[tokio::test(flavor = "multi_thread")]
async fn does_not_commit_what_a_callback_did_after_it_caught_a_failed_read() {
    let storage = Arc::new(FaultyStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let written = storage.admitted();
    lock(&storage.failing).insert("task");
    let error = root
        .commit(
            &ReadThenAppend {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert!(is_session_failed(&error), "{error}");
    assert_eq!(storage.admitted(), written, "nothing reached Storage");
    fixture.harness.close(&*context()).await;
}

// TS: "fails only the call for an invalid request".
#[tokio::test(flavor = "multi_thread")]
async fn fails_only_the_call_for_an_invalid_request() {
    let storage = Arc::new(support::MemoryStorage::new());
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
    let entry_id = *entry
        .downcast::<kiana_durable::types::EntryId>()
        .expect("entry id");

    let invalid = root
        .entries(
            kiana_durable::harness::types::EntryRangeQuery::default(),
            10,
            Some(cursor(serde_json::json!({"after": "x"}))),
            &*context(),
        )
        .await;
    assert!(
        invalid.as_ref().is_err_and(is_storage_request_error),
        "{invalid:?}"
    );
    let first_page = root
        .entries(
            kiana_durable::harness::types::EntryRangeQuery {
                order: Some(kiana_durable::types::ScanOrder::Ascending),
                ..Default::default()
            },
            1,
            None,
            &*context(),
        )
        .await
        .expect("the first page");
    let next = first_page
        .next
        .clone()
        .unwrap_or_else(|| cursor(serde_json::json!({"after": 1, "order": "ascending"})));
    let mismatched = root
        .entries(
            kiana_durable::harness::types::EntryRangeQuery {
                order: Some(kiana_durable::types::ScanOrder::Descending),
                ..Default::default()
            },
            1,
            Some(next),
            &*context(),
        )
        .await;
    assert!(
        mismatched.as_ref().is_err_and(is_storage_request_error),
        "{mismatched:?}"
    );

    struct InvalidScan {
        conversation: ConversationId,
    }
    #[async_trait]
    impl SessionChange for InvalidScan {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
            let _ = self.conversation;
            let page = tx
                .scan_entries(
                    &EntryQuery {
                        conversation_id: kiana_durable::types::id_from_number(999_999),
                        min_entry_id: None,
                        max_entry_id: None,
                        order: None,
                    },
                    10,
                    None,
                )
                .await?;
            Ok(Box::new(page))
        }
    }
    let invalid_scan = root
        .commit(
            &InvalidScan {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await;
    assert!(
        invalid_scan.as_ref().is_err_and(is_storage_request_error),
        "{invalid_scan:?}"
    );
    // `kiana.live` 不保留历史。
    let as_of = fixture
        .harness
        .snapshot_as_of(
            &DocTarget::conversation(&LIVE_DOC, root.id()),
            entry_id,
            &*context(),
        )
        .await;
    assert!(
        as_of.as_ref().is_err_and(is_storage_request_error),
        "{as_of:?}"
    );

    root.commit(
        &AppendNote {
            conversation: root.id(),
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("the session still works");
    let mut closed = Box::pin(fixture.harness.closed());
    assert!(
        futures::poll!(closed.as_mut()).is_pending(),
        "the session stays open"
    );
    assert_eq!(report_messages(&fixture), Vec::<String>::new());
    fixture.harness.close(&*context()).await;
}

fn is_storage_request_error(error: &BoxError) -> bool {
    error.downcast_ref::<StorageRequestError>().is_some()
}

// TS: "fails it for a throw in its own commit listeners, which would leave memory behind storage".
#[tokio::test(flavor = "multi_thread")]
async fn fails_it_for_a_panicking_internal_commit_listener() {
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let _unsubscribe = fixture
        .harness
        .observe_commits(Arc::new(|_publication, _context| {
            std::panic::panic_any("listener bug");
        }))
        .expect("observe");
    // 提交本身持久并成功；Session 在它之后失败。
    root.commit(
        &AppendNote {
            conversation: root.id(),
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("the commit resolves");
    match fixture.harness.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "listener bug"),
        other => panic!("unexpected end: {other:?}"),
    }
    assert_eq!(report_messages(&fixture), vec!["listener bug".to_owned()]);
    fixture.harness.close(&*context()).await;
}

// TS: "reports a throwing commit or close listener and runs the others; the commit stands".
#[tokio::test(flavor = "multi_thread")]
async fn reports_a_panicking_commit_or_close_listener_and_runs_the_others() {
    let storage = Arc::new(support::MemoryStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let _ = Session::subscribe_commits(
        &*fixture.harness,
        Arc::new(
            |_publication: &kiana_durable::types::CommitPublication, _context: &dyn Context| {
                std::panic::panic_any("commit listener");
            },
        ),
    );
    let seen_commit = Arc::clone(&seen);
    let _ = Session::subscribe_commits(
        &*fixture.harness,
        Arc::new(
            move |_publication: &kiana_durable::types::CommitPublication,
                  _context: &dyn Context| {
                lock(&seen_commit).push("commit".to_owned());
            },
        ),
    );
    let _ = Session::subscribe_close(
        &*fixture.harness,
        Arc::new(|| std::panic::panic_any("close listener")),
    );
    let seen_close = Arc::clone(&seen);
    let _ = Session::subscribe_close(
        &*fixture.harness,
        Arc::new(move || {
            lock(&seen_close).push("close".to_owned());
        }),
    );
    root.commit(
        &AppendNote {
            conversation: root.id(),
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("the commit stands");
    fixture.harness.close(&*context()).await;
    assert_eq!(
        lock(&seen).clone(),
        vec!["commit".to_owned(), "close".to_owned()]
    );
    assert_eq!(
        report_messages(&fixture),
        vec!["commit listener".to_owned(), "close listener".to_owned()]
    );
}

// TS: "ends Storage calls underway with SessionFailed, and closes once they have settled".
#[tokio::test(flavor = "multi_thread")]
async fn ends_storage_calls_underway_with_session_failed_and_closes_once_they_settled() {
    let storage = Arc::new(FaultyStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    root.commit(
        &AppendNote {
            conversation: root.id(),
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("append");
    let hold = storage.hold_range();
    let root_for_read = Arc::clone(&root);
    let reading =
        tokio::spawn(async move { root_for_read.context(&*context(), Default::default()).await });
    hold.entered().await;
    // 另一次调用在这次上下文读取进行中让 Session 失败。
    lock(&storage.failing).insert("mintId");
    let error = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert_eq!(error.to_string(), "disk gone");
    let mut closed = Box::pin(fixture.harness.closed());
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert!(
        futures::poll!(closed.as_mut()).is_pending(),
        "the session waits for the read underway"
    );
    // 读取在后端成功，但调用方拿到失败：它读到的东西不会被使用。
    hold.release();
    let read_error = reading
        .await
        .expect("the read joins")
        .expect_err("the underway read fails");
    assert!(is_session_failed(&read_error), "{read_error}");
    match closed.await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "disk gone"),
        other => panic!("unexpected end: {other:?}"),
    }
    fixture.harness.close(&*context()).await;
}

// TS: "closes the backend only after a paged read underway, which cannot start another page".
#[tokio::test(flavor = "multi_thread")]
async fn closes_the_backend_only_after_a_paged_read_underway() {
    struct AppendMany {
        conversation: ConversationId,
        count: usize,
    }
    #[async_trait]
    impl SessionChange for AppendMany {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
            for _ in 0..self.count {
                tx.append_entry(
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
            }
            Ok(Box::new(()))
        }
    }

    let storage = Arc::new(FaultyStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    // 条目多于一页上下文读取。
    root.commit(
        &AppendMany {
            conversation: root.id(),
            count: 300,
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("append the entries");
    let hold = storage.hold_range();
    let root_for_read = Arc::clone(&root);
    let reading =
        tokio::spawn(async move { root_for_read.context(&*context(), Default::default()).await });
    hold.entered().await;
    let mut closing = Box::pin(fixture.harness.close_result(context()));
    assert!(
        futures::poll!(closing.as_mut()).is_pending(),
        "close waits for the paged read"
    );
    hold.release();
    let read_error = reading
        .await
        .expect("the read joins")
        .expect_err("the read cannot start another page");
    assert!(read_error.to_string().contains("closed"), "{read_error}");
    closing.await.expect("close resolves");
    assert!(matches!(fixture.harness.closed().await, SessionEnd::Closed));
    assert_eq!(
        lock(&storage.scan_log).clone(),
        vec!["first".to_owned(), "close".to_owned()],
        "no page after close began"
    );
}

// TS: "does not fail it for a read its caller cancelled".
#[tokio::test(flavor = "multi_thread")]
async fn does_not_fail_it_for_a_read_its_caller_cancelled() {
    use kiana_chord::context::with_abort_signal;

    let storage = Arc::new(FaultyStorage::new());
    let fixture = open_tasks(Arc::clone(&storage) as Arc<dyn Storage>, Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let signal = AbortSignal::new();
    *lock(&storage.cancel) = Some((signal.clone(), "caller gave up"));
    let cancelling = with_abort_signal(signal, context());
    let error = root
        .context(&*cancelling, Default::default())
        .await
        .expect_err("the cancelled read fails");
    assert_eq!(error.to_string(), "caller gave up");
    root.commit(
        &AppendNote {
            conversation: root.id(),
        },
        &*context(),
        CommitOptions::default(),
    )
    .await
    .expect("the session still works");
    let mut closed = Box::pin(fixture.harness.closed());
    assert!(
        futures::poll!(closed.as_mut()).is_pending(),
        "the session stays open"
    );
    fixture.harness.close(&*context()).await;
}

// TS: "ends what a task holds as failed, not as cancelled by its signal".
#[tokio::test(flavor = "multi_thread")]
async fn ends_what_a_task_holds_as_failed_not_as_cancelled() {
    struct WatcherPhase {
        watching: Arc<task_support::Deferred>,
        ends: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for WatcherPhase {
        async fn run(
            &self,
            _task: &TaskRecord,
            runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
            context: &dyn Context,
        ) -> Result<(), BoxError> {
            let doc = runtime
                .watch_doc(
                    &DocTarget::conversation(&LIVE_DOC, runtime.conversation_id()),
                    context,
                )
                .await?
                .expect("kiana.live exists");
            let handle = runtime
                .conversation(runtime.conversation_id(), context)
                .await?
                .expect("the conversation exists");
            // 先发起 idle 等待，再让测试触发失败；绝不能在这里 await 它（任务自己还占着）。
            let mut idle = Box::pin(handle.wait_for_idle(context));
            self.watching.resolve(());
            let end = doc.closed().await;
            let idle = idle.as_mut().await;
            lock(&self.ends).push(match end {
                WatchEnd::Failed(error) => format!("failed: {error}"),
                WatchEnd::Ended => "ended".to_owned(),
                other => format!("{other:?}"),
            });
            lock(&self.ends).push(if idle.is_err() {
                "SessionFailed".to_owned()
            } else {
                "resolved".to_owned()
            });
            Ok(())
        }
    }

    struct WatcherAbort;

    #[async_trait]
    impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for WatcherAbort {
        async fn run(
            &self,
            _task: &TaskRecord,
            _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
            _context: &dyn Context,
        ) -> Result<(), BoxError> {
            Ok(())
        }
    }

    let ends: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let watching = Arc::new(task_support::Deferred::new());
    let watcher = {
        let definition = TaskDefinition {
            name: "test.watcher".to_owned(),
            version: 1,
            initial: Arc::new(|_| serde_json::json!({"phase": "work"})),
            phases: IndexMap::from([(
                "work".to_owned(),
                Arc::new(WatcherPhase {
                    watching: Arc::clone(&watching),
                    ends: Arc::clone(&ends),
                }) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            )]),
            abort: Arc::new(WatcherAbort)
                as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            migrate: None,
            hooks: None,
        };
        kiana_durable::harness::scheduler::erase_task(&definition)
    };
    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![watcher.clone()],
    )
    .await;
    let root = open_root(&fixture.harness).await;
    start(&root, watcher).await;
    fixture.harness.resume().expect("resume");
    watching.promise().await.expect("the watcher starts");
    storage.fail_next_commit(io_error("disk gone"));
    let error = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert_eq!(error.to_string(), "disk gone");
    fixture.harness.closed().await;
    assert_eq!(
        lock(&ends).clone(),
        vec!["failed: disk gone".to_owned(), "SessionFailed".to_owned()]
    );
    fixture.harness.close(&*context()).await;
}

// TS: "ends an abort that joins a run ignoring its signal, and a reentrant report finds the Session failed".
#[tokio::test(flavor = "multi_thread")]
async fn ends_an_abort_that_joins_a_run_ignoring_its_signal_and_a_reentrant_report_finds_the_failure()
 {
    struct StubbornPhase {
        entered: Arc<task_support::Deferred>,
        release: Arc<task_support::Deferred>,
    }

    #[async_trait]
    impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for StubbornPhase {
        async fn run(
            &self,
            _task: &TaskRecord,
            _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
            _context: &dyn Context,
        ) -> Result<(), BoxError> {
            self.entered.resolve(());
            let _ = self.release.promise().await;
            Ok(())
        }
    }

    struct StubbornAbort;

    #[async_trait]
    impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for StubbornAbort {
        async fn run(
            &self,
            _task: &TaskRecord,
            _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
            _context: &dyn Context,
        ) -> Result<(), BoxError> {
            Ok(())
        }
    }

    let entered = Arc::new(task_support::Deferred::new());
    let release = Arc::new(task_support::Deferred::new());
    let stubborn = {
        let definition = TaskDefinition {
            name: "test.stubborn".to_owned(),
            version: 1,
            initial: Arc::new(|_| serde_json::json!({"phase": "work"})),
            phases: IndexMap::from([(
                "work".to_owned(),
                Arc::new(StubbornPhase {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            )]),
            abort: Arc::new(StubbornAbort)
                as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            migrate: None,
            hooks: None,
        };
        kiana_durable::harness::scheduler::erase_task(&definition)
    };

    type HarnessSlot = Arc<OnceLock<Arc<kiana_durable::harness::harness::HarnessImpl>>>;
    type ReportHandler = Arc<
        dyn Fn(&(dyn std::error::Error + Send + Sync), &kiana_durable::types::ReportInfo)
            + Send
            + Sync,
    >;
    type ReentrantWait = Arc<Mutex<Option<tokio::task::JoinHandle<Result<(), BoxError>>>>>;

    let storage = Arc::new(support::ControlledStorage::new());
    let slot: HarnessSlot = Arc::new(OnceLock::new());
    let reentered: ReentrantWait = Arc::new(Mutex::new(None));
    let on_report: ReportHandler = {
        let slot = Arc::clone(&slot);
        let reentered = Arc::clone(&reentered);
        Arc::new(move |_error, _info| {
            let Some(harness) = slot.get() else {
                return;
            };
            let mut guard = lock(&reentered);
            if guard.is_none() {
                let harness = Arc::clone(harness);
                *guard = Some(tokio::spawn(async move {
                    harness.wait_for_idle(&*context()).await
                }));
            }
        })
    };
    let fixture = open_tasks_with(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![stubborn],
        OpenTasksOptions {
            on_report: Some(on_report),
            ..Default::default()
        },
    )
    .await;
    let _ = slot.set(Arc::clone(&fixture.harness));
    let root = open_root(&fixture.harness).await;
    let id = start(&root, {
        // 与上方的定义同名；`start` 只需要一个携带该名字的任务句柄。
        let definition = TaskDefinition {
            name: "test.stubborn".to_owned(),
            version: 1,
            initial: Arc::new(|_| serde_json::json!({"phase": "work"})),
            phases: IndexMap::from([(
                "work".to_owned(),
                Arc::new(StubbornPhase {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            )]),
            abort: Arc::new(StubbornAbort)
                as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            migrate: None,
            hooks: None,
        };
        kiana_durable::harness::scheduler::erase_task(&definition)
    })
    .await;
    fixture.harness.resume().expect("resume");
    entered.promise().await.expect("the stubborn run starts");
    let held = storage.hold_commits();
    let harness_for_abort = Arc::clone(&fixture.harness);
    let aborting = tokio::spawn(async move { harness_for_abort.abort_task(id, &*context()).await });
    held.entered().await;
    held.release();
    // abort 标记提交后，abort 汇合一个忽略信号的运行；随后 Session 失败。
    tokio::time::sleep(Duration::from_millis(5)).await;
    storage.fail_next_commit(io_error("disk gone"));
    let error = root
        .commit(
            &AppendNote {
                conversation: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect_err("the commit fails");
    assert_eq!(error.to_string(), "disk gone");
    let abort_error = aborting
        .await
        .expect("the abort joins")
        .expect_err("the abort fails");
    assert!(is_session_failed(&abort_error), "{abort_error}");
    let reentered = lock(&reentered)
        .take()
        .expect("the reentrant report started");
    let reentered_error = reentered
        .await
        .expect("the reentrant wait joins")
        .expect_err("the reentrant wait fails");
    assert!(is_session_failed(&reentered_error), "{reentered_error}");
    release.resolve(());
    fixture.harness.close(&*context()).await;
}

// 未移植的 TS 用例与理由：
// - Storage 无法关闭（`Storage::close` 在 Rust 没有返回错误的通道），“关闭失败”的用例
//   在类型上不可表达；打开/失败后关闭的路径已由其他用例覆盖。
// - 监听器/onReport 返回 rejected promise：Rust 的监听器是同步 `Fn`，没有 promise；
//   同步 panic 已端口（提交/关闭监听器、内部提交监听器），onReport 抛错在 harness_reports。
// - “unformattable 抛值”、跨 realm thenable/rejected promise：JS 运行期概念，Rust 无对应物。
// - 视图内部 observer（`conversationViews().attach`）不是 Harness 的公开面；抛错的 watch
//   监听器如何结束该 watch 由 harness_reports 覆盖。
