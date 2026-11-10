//! `test/session-watches.test.ts` 的端口（节选）。
//!
//! watch 的投递是异步的（TS 的 queueMicrotask，这里是 tokio 任务），测试用通道与短等待收帧。

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kiana_chord::context::Context;
use kiana_chord::delta::Op;
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::{
    BoxError, DocDefinition, DocTarget, DocToken, DocumentSemantics, DocumentWatch, JsonObject,
    WatchEnd, WatchListener,
};
use serde_json::json;
use support::*;
use tokio::sync::{Notify, mpsc};

fn ctx() -> &'static dyn Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

fn state_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new("watch.state", 1, DocumentSemantics::SESSION).with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("value".to_owned(), json!(0));
            value.insert("items".to_owned(), json!(["a", "b"]));
            value.insert("retained".to_owned(), json!({ "label": "stable" }));
            value
        }),
    )
}

/// 一帧投递。
#[derive(Debug, Clone)]
struct Frame {
    value: Option<JsonObject>,
    ops: Vec<serde_json::Value>,
}

struct ChannelListener {
    sender: mpsc::UnboundedSender<Frame>,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for ChannelListener {
    async fn call(&self, value: Option<JsonObject>, ops: Vec<Op>, _context: Arc<dyn Context>) {
        let _ = self.sender.send(Frame {
            value,
            ops: kiana_chord::delta::index::encode(&ops),
        });
    }
}

fn open_channel() -> (mpsc::UnboundedSender<Frame>, mpsc::UnboundedReceiver<Frame>) {
    mpsc::unbounded_channel()
}

async fn next_frame(receiver: &mut mpsc::UnboundedReceiver<Frame>) -> Frame {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("a frame arrives")
        .expect("the channel stays open")
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
}

async fn create_state(session: &SessionImpl) {
    commit(session, |tx| {
        let target = DocTarget::session(&state_definition());
        Box::pin(async move {
            tx.doc(&target).await?;
            Ok(())
        })
    })
    .await
    .expect("the state document is created");
}

fn set_value(
    session: &SessionImpl,
    value: i64,
) -> impl std::future::Future<Output = Result<(), BoxError>> {
    let target = DocTarget::session(&state_definition());
    session.commit_with(
        move |tx| {
            let target = target.clone();
            Box::pin(async move {
                let mut draft = tx.doc(&target).await?;
                draft.insert("value".to_owned(), json!(value));
                Ok(())
            })
        },
        ctx(),
        Default::default(),
    )
}

#[tokio::test]
async fn never_creates_an_absent_document() {
    let (storage, session, _publications) = open_test_session();
    let commits = support::lock(&storage.commits).len();
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .expect("the watch call succeeds");
    assert!(watch.is_none());
    assert_eq!(support::lock(&storage.commits).len(), commits);
    assert_eq!(storage.mint_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn keeps_the_acquisition_revision_until_start_then_delivers_the_commits_since_folded() {
    let (_storage, session, publications) = open_test_session();
    create_state(&session).await;
    let watch: DocumentWatch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let initial = watch.value().clone();
    set_value(&session, 1).await.unwrap();
    set_value(&session, 2).await.unwrap();
    assert_eq!(watch.value(), &initial);

    let (sender, mut receiver) = open_channel();
    watch.start(Arc::new(ChannelListener { sender }));
    // 同步安装，绝不同步调用。
    assert!(receiver.try_recv().is_err());
    let frame = next_frame(&mut receiver).await;
    assert_eq!(
        frame
            .value
            .as_ref()
            .and_then(|value| value.get("value").and_then(|value| value.as_i64())),
        Some(2)
    );
    assert_eq!(frame.ops, vec![json!(["s", ["value"], 2])]);
    assert_eq!(
        initial
            .as_ref()
            .and_then(|value| value.get("value").and_then(|value| value.as_i64())),
        Some(0)
    );

    // 跟得上的监听器逐次收到每个提交。
    set_value(&session, 3).await.unwrap();
    let third = next_frame(&mut receiver).await;
    assert_eq!(
        third
            .value
            .as_ref()
            .and_then(|value| value.get("value").and_then(|value| value.as_i64())),
        Some(3)
    );
    let published = support::lock(&publications)
        .last()
        .cloned()
        .expect("the commit published");
    let document = document_changes(&published)
        .into_iter()
        .next()
        .expect("the commit published a document change");
    match document {
        kiana_durable::DocumentCommitChange::Document { ops, .. } => {
            assert_eq!(third.ops, kiana_chord::delta::index::encode(&ops.0));
        }
        _ => panic!("expected a document change"),
    }
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
}

struct BlockingListener {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    values: Arc<Mutex<Vec<i64>>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for BlockingListener {
    async fn call(&self, value: Option<JsonObject>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let number = value
            .as_ref()
            .and_then(|value| value.get("value").and_then(|value| value.as_i64()))
            .unwrap_or(-1);
        let first = {
            let mut values = support::lock(&self.values);
            values.push(number);
            values.len() == 1
        };
        if first {
            self.entered.notify_waiters();
            self.release.notified().await;
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn serializes_callbacks_and_folds_commits_made_while_one_is_in_flight() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let values = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    watch.start(Arc::new(BlockingListener {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        values: Arc::clone(&values),
        active: Arc::clone(&active),
        max_active: Arc::clone(&max_active),
    }));
    let entered_wait = entered.notified();
    set_value(&session, 1).await.unwrap();
    entered_wait.await;
    for value in 2..=20 {
        set_value(&session, value).await.unwrap();
    }
    assert_eq!(*support::lock(&values), [1]);
    release.notify_waiters();
    settle().await;
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    assert_eq!(*support::lock(&values), [1, 20]);
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
}

#[tokio::test]
async fn delivers_retirement_and_does_not_follow_recreation() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let (sender, mut receiver) = open_channel();
    watch.start(Arc::new(ChannelListener { sender }));
    let target = DocTarget::session(&state_definition());
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.retire_doc(&target).await?;
            Ok(())
        })
    })
    .await
    .expect("the retirement commits");
    let frame = next_frame(&mut receiver).await;
    assert!(frame.value.is_none());
    assert_eq!(frame.ops, vec![json!(["r", null])]);
    assert!(matches!(watch.closed().await, WatchEnd::Retired));

    // 重建同一地址后旧 watch 不再跟随。
    create_state(&session).await;
    settle().await;
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn supports_idempotent_stop_and_stops_future_delivery() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let (sender, mut receiver) = open_channel();
    let listener = Arc::new(ChannelListener { sender });
    watch.start(Arc::clone(&listener) as Arc<dyn WatchListener<Option<JsonObject>>>);
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
    // stop 是幂等的。
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
    set_value(&session, 1).await.unwrap();
    settle().await;
    assert!(receiver.try_recv().is_err());

    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    session.close(ctx()).await;
    assert!(matches!(watch.closed().await, WatchEnd::Ended));
}

#[tokio::test]
async fn rejects_repeated_or_late_start() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let (sender, _receiver) = open_channel();
    let listener = Arc::new(ChannelListener { sender });
    watch.start(Arc::clone(&listener) as Arc<dyn WatchListener<Option<JsonObject>>>);
    let repeated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        watch.start(Arc::clone(&listener) as Arc<dyn WatchListener<Option<JsonObject>>>);
    }));
    let panic = repeated.expect_err("repeated start panics");
    assert_eq!(
        panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str)),
        Some("Watch is already started")
    );
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
    // `started` 一旦置位就不再复位：watch 是一次性的，stop 只终止监听而不重置生命周期状态，
    // 因此停止后再 start 仍是 "already started"。
    let after_stop = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        watch.start(Arc::clone(&listener) as Arc<dyn WatchListener<Option<JsonObject>>>);
    }));
    let panic = after_stop.expect_err("a start after stop panics");
    assert_eq!(
        panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str)),
        Some("Watch is already started")
    );

    // 从未 start 就被结束的 watch：end 先于 started，报 "Watch is stopped"。
    let late = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    session.close(ctx()).await;
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        late.start(Arc::clone(&listener) as Arc<dyn WatchListener<Option<JsonObject>>>);
    }));
    let panic = refused.expect_err("a start after the end panics");
    assert_eq!(
        panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str)),
        Some("Watch is stopped")
    );
}

#[allow(dead_code)]
fn _session_options_used() -> SessionOptions {
    SessionOptions::default()
}

// ---------------------------------------------------------------------------
// 更完整的 session-watches 覆盖
// ---------------------------------------------------------------------------

fn value_number(value: &Option<JsonObject>) -> i64 {
    value
        .as_ref()
        .and_then(|value| value.get("value").and_then(|value| value.as_i64()))
        .unwrap_or(-1)
}

fn set_value_commit(
    session: &SessionImpl,
    value: i64,
) -> impl std::future::Future<Output = Result<(), BoxError>> {
    set_value(session, value)
}

struct FollowUpListener {
    session: SessionImpl,
    values: Arc<Mutex<Vec<i64>>>,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for FollowUpListener {
    async fn call(&self, value: Option<JsonObject>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        let current = value_number(&value);
        support::lock(&self.values).push(current);
        if current == 1 {
            let target = DocTarget::session(&state_definition());
            self.session
                .commit_with(
                    move |tx| {
                        let target = target.clone();
                        Box::pin(async move {
                            let mut draft = tx.doc(&target).await?;
                            draft.insert("value".to_owned(), json!(2));
                            Ok(())
                        })
                    },
                    ctx(),
                    Default::default(),
                )
                .await
                .expect("a listener may commit");
        }
    }
}

#[tokio::test]
async fn allows_a_listener_to_initiate_a_later_session_commit() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let values = Arc::new(Mutex::new(Vec::new()));
    watch.start(Arc::new(FollowUpListener {
        session: session.clone(),
        values: Arc::clone(&values),
    }));
    set_value_commit(&session, 1).await.unwrap();
    for _ in 0..200 {
        if support::lock(&values).len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(*support::lock(&values), [1, 2]);
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
}

#[tokio::test]
async fn folds_101_pending_commits_of_one_field_into_one_set() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    for value in 1..=101 {
        set_value_commit(&session, value).await.unwrap();
    }
    let (sender, mut receiver) = open_channel();
    watch.start(Arc::new(ChannelListener { sender }));
    let frame = next_frame(&mut receiver).await;
    assert_eq!(value_number(&frame.value), 101);
    assert_eq!(frame.ops, vec![json!(["s", ["value"], 101])]);
    assert!(receiver.try_recv().is_err());
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
}

type Deliveries = Arc<Mutex<Vec<(i64, Vec<serde_json::Value>)>>>;

struct BlockingDeliveryListener {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    deliveries: Deliveries,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for BlockingDeliveryListener {
    async fn call(&self, value: Option<JsonObject>, ops: Vec<Op>, _context: Arc<dyn Context>) {
        let delivery = (
            value_number(&value),
            kiana_chord::delta::index::encode(&ops),
        );
        let first = {
            let mut deliveries = support::lock(&self.deliveries);
            deliveries.push(delivery);
            deliveries.len() == 1
        };
        if first {
            self.entered.notify_waiters();
            self.release.notified().await;
        }
    }
}

#[tokio::test]
async fn never_folds_the_in_flight_frame_into_the_next_call() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let deliveries = Arc::new(Mutex::new(Vec::new()));
    let entered_wait = entered.notified();
    watch.start(Arc::new(BlockingDeliveryListener {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        deliveries: Arc::clone(&deliveries),
    }));
    set_value_commit(&session, 1).await.unwrap();
    entered_wait.await;
    for value in 2..=102 {
        set_value_commit(&session, value).await.unwrap();
    }
    release.notify_waiters();
    settle().await;
    {
        let deliveries = support::lock(&deliveries);
        assert_eq!(deliveries[0].0, 1);
        assert_eq!(deliveries[1].0, 102);
        assert_eq!(deliveries[1].1, vec![json!(["s", ["value"], 102])]);
        assert_eq!(deliveries.len(), 2);
    }
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
}

#[tokio::test]
async fn folds_retirement_over_pending_commits_and_then_closes() {
    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    for value in 1..=100 {
        set_value_commit(&session, value).await.unwrap();
    }
    let target = DocTarget::session(&state_definition());
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.retire_doc(&target).await?;
            Ok(())
        })
    })
    .await
    .expect("the retirement commits");
    let (sender, mut receiver) = open_channel();
    watch.start(Arc::new(ChannelListener { sender }));
    let frame = next_frame(&mut receiver).await;
    assert!(frame.value.is_none());
    assert_eq!(frame.ops, vec![json!(["r", null])]);
    assert!(matches!(watch.closed().await, WatchEnd::Retired));
    assert!(receiver.try_recv().is_err());
}

struct PanickingListener;

#[async_trait]
impl WatchListener<Option<JsonObject>> for PanickingListener {
    async fn call(&self, _value: Option<JsonObject>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        panic!("listener failed");
    }
}

struct CountingListener {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for CountingListener {
    async fn call(&self, _value: Option<JsonObject>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct RecordingHooks {
    reports: Mutex<Vec<(String, kiana_durable::ReportInfo)>>,
}

impl kiana_durable::session::SessionHooks for RecordingHooks {
    fn report(
        &self,
        message: &str,
        info: kiana_durable::ReportInfo,
        _context: Option<&dyn Context>,
    ) {
        support::lock(&self.reports).push((message.to_owned(), info));
    }
}

#[tokio::test]
async fn settles_listener_failure_on_only_the_affected_watch() {
    let storage = Arc::new(ControlledStorage::new());
    let hooks = Arc::new(RecordingHooks::default());
    let session = SessionImpl::new(
        storage,
        SessionOptions {
            document_retention_ms: Some(600_000.0),
            hooks: Some(hooks.clone()),
            ..Default::default()
        },
    );
    create_state(&session).await;
    let target = DocTarget::session(&state_definition());
    let failed = session
        .watch_doc(&target, ctx())
        .await
        .unwrap()
        .expect("exists");
    let healthy = session
        .watch_doc(&target, ctx())
        .await
        .unwrap()
        .expect("exists");
    failed.start(Arc::new(PanickingListener));
    let calls = Arc::new(AtomicUsize::new(0));
    healthy.start(Arc::new(CountingListener {
        calls: Arc::clone(&calls),
    }));
    set_value_commit(&session, 1).await.unwrap();
    match failed.closed().await {
        WatchEnd::ListenerError(message) => assert_eq!(message, "listener failed"),
        other => panic!("expected a listener error, got {other:?}"),
    }
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    {
        let reports = support::lock(&hooks.reports);
        assert!(
            reports
                .iter()
                .any(|(message, info)| message == "listener failed"
                    && *info == kiana_durable::ReportInfo::ListenerError),
            "the failure is reported: {reports:?}"
        );
    }
    // 失败的 watch 不影响 Session 或另一个 watch。
    assert_eq!(
        session
            .snapshot(&target, ctx())
            .await
            .unwrap()
            .expect("the document exists")
            .get("value"),
        Some(&json!(1))
    );
    assert!(matches!(healthy.stop().await, WatchEnd::Stopped));
}

type CapturedContext = Arc<Mutex<Option<(Option<String>, bool)>>>;

struct ContextProbeListener {
    key: kiana_chord::context::ContextKey<String>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    captured: CapturedContext,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for ContextProbeListener {
    async fn call(&self, _value: Option<JsonObject>, _ops: Vec<Op>, context: Arc<dyn Context>) {
        *support::lock(&self.captured) = Some((
            self.key.get(context.as_ref()),
            context.abort_signal().is_some(),
        ));
        self.entered.notify_waiters();
        self.release.notified().await;
    }
}

#[tokio::test]
async fn preserves_commit_context_values_without_inheriting_producer_cancellation() {
    use kiana_chord::context::{
        BACKGROUND_CONTEXT, ContextRef, create_context_key, with_abort_signal, with_context_value,
    };

    let (_storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let watch = session
        .watch_doc(&DocTarget::session(&state_definition()), ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let key = create_context_key::<String>("watch-test");
    let producer = kiana_chord::context::AbortSignal::new();
    let commit_context: ContextRef = with_context_value(
        key.clone(),
        "newest-commit".to_owned(),
        with_abort_signal(producer.clone(), ContextRef::clone(&BACKGROUND_CONTEXT)),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let captured = Arc::new(Mutex::new(None));
    watch.start(Arc::new(ContextProbeListener {
        key: key.clone(),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        captured: Arc::clone(&captured),
    }));

    let target = DocTarget::session(&state_definition());
    session
        .commit_with(
            move |tx| {
                let target = target.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&target).await?;
                    draft.insert("value".to_owned(), json!(1));
                    Ok(())
                })
            },
            Arc::clone(&commit_context),
            Default::default(),
        )
        .await
        .expect("the commit succeeds");
    for _ in 0..200 {
        if support::lock(&captured).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        *support::lock(&captured),
        Some((Some("newest-commit".to_owned()), false)),
        "values survive and the producer signal is masked"
    );
    producer.abort(None);
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
    assert_eq!(
        support::lock(&captured)
            .as_ref()
            .map(|(_, aborted)| *aborted),
        Some(false),
        "an abort after delivery does not reach the delivered context"
    );
    release.notify_waiters();
}

#[tokio::test]
async fn continues_after_the_tracker_cache_unloads() {
    let (storage, session, _publications) = open_test_session();
    create_state(&session).await;
    let target = DocTarget::session(&state_definition());
    let watch = session
        .watch_doc(&target, ctx())
        .await
        .unwrap()
        .expect("exists");
    let baseline = watch.value().clone();
    let reads = storage
        .document_read_count
        .load(std::sync::atomic::Ordering::SeqCst);
    session.unload_documents().await.expect("unload succeeds");
    let reloaded = session.snapshot(&target, ctx()).await.unwrap();
    assert_eq!(reloaded, baseline);
    assert!(
        storage
            .document_read_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > reads
    );
    let (sender, mut receiver) = open_channel();
    watch.start(Arc::new(ChannelListener { sender }));
    set_value_commit(&session, 7).await.unwrap();
    let frame = next_frame(&mut receiver).await;
    assert_eq!(value_number(&frame.value), 7);
    assert_eq!(value_number(watch.value()), 7);
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
}

fn migration_old() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new("watch.migration", 1, DocumentSemantics::SESSION).with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("value".to_owned(), json!(3));
            value
        }),
    )
}

fn migration_current() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new("watch.migration", 2, DocumentSemantics::SESSION)
            .with_initial(|_| {
                let mut value = JsonObject::new();
                value.insert("value".to_owned(), json!(0));
                value.insert("migrated".to_owned(), json!(false));
                value
            })
            .with_migrate(|value, from_version| {
                assert_eq!(from_version, 1);
                let mut migrated = JsonObject::new();
                migrated.insert(
                    "value".to_owned(),
                    value.get("value").cloned().unwrap_or(json!(0)),
                );
                migrated.insert("migrated".to_owned(), json!(true));
                migrated
            }),
    )
}

#[tokio::test]
async fn hydrates_migration_without_writing_and_observes_the_later_exact_edit() {
    let (storage, session, _publications) = open_test_session();
    let old_target = DocTarget::session(&migration_old());
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&old_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the old document is created");
    session.unload_documents().await.expect("unload succeeds");
    let commits = support::lock(&storage.commits).len();

    let current_target = DocTarget::session(&migration_current());
    let watch = session
        .watch_doc(&current_target, ctx())
        .await
        .unwrap()
        .expect("the migrated document exists");
    assert_eq!(
        watch.value().as_ref().and_then(|value| {
            if value.get("migrated") == Some(&json!(true)) {
                value.get("value").cloned()
            } else {
                None
            }
        }),
        Some(json!(3))
    );
    assert_eq!(support::lock(&storage.commits).len(), commits);

    let (sender, mut receiver) = open_channel();
    watch.start(Arc::new(ChannelListener { sender }));
    let quiet_target = DocTarget::session(&migration_current());
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.doc(&quiet_target).await?;
            Ok(())
        })
    })
    .await
    .expect("a migration-only transaction commits");
    settle().await;
    assert!(receiver.try_recv().is_err());

    let edit_target = DocTarget::session(&migration_current());
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&edit_target).await?;
            draft.insert("value".to_owned(), json!(4));
            Ok(())
        })
    })
    .await
    .expect("the exact edit commits");
    let frame = next_frame(&mut receiver).await;
    assert_eq!(
        frame.value.as_ref().and_then(|value| value.get("value")),
        Some(&json!(4))
    );
    assert_eq!(frame.ops, vec![json!(["s", ["value"], 4])]);
    assert!(matches!(watch.stop().await, WatchEnd::Stopped));
}
