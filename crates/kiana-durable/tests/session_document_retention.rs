//! `test/session-document-retention.test.ts` 的端口（节选）。
//!
//! 缓存保留依赖台钟与计时器；这里用可控时钟 + 拒绝后台计时器的宿主，让提交结束时同步清扫。

mod support;

use std::sync::{Arc, Mutex};

use kiana_chord::context::Context;
use kiana_durable::session::{DocumentCacheSizes, SessionImpl, SessionOptions};
use kiana_durable::timers::SetTimer;
use kiana_durable::{
    BoxError, ConversationId, DocDefinition, DocToken, DocumentContentKind, DocumentFork,
    DocumentHistory, DocumentSemantics, ScopeKind, StorageWrite,
};
use serde_json::json;
use support::*;

fn ctx() -> &'static dyn Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

fn counter_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "test.retention.counter",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| {
            let mut value = kiana_durable::JsonObject::new();
            value.insert("count".to_owned(), json!(0));
            value
        }),
    )
}

fn rewindable_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "test.retention.rewindable",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Rewindable),
                fork: Some(DocumentFork::AsOf),
            },
        )
        .with_initial(|_| {
            let mut value = kiana_durable::JsonObject::new();
            value.insert("count".to_owned(), json!(0));
            value
        }),
    )
}

fn session_counter_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "test.retention.session-counter",
            1,
            DocumentSemantics::SESSION,
        )
        .with_initial(|_| {
            let mut value = kiana_durable::JsonObject::new();
            value.insert("count".to_owned(), json!(0));
            value
        }),
    )
}

struct Retained {
    storage: Arc<ControlledStorage>,
    session: SessionImpl,
    clock: Arc<Mutex<f64>>,
    counter: DocToken,
    conversation_id: ConversationId,
    timers: Arc<Mutex<Vec<RecordedTimer>>>,
}

/// 一个被宿主记录下来的计时器。
struct RecordedTimer {
    callback: kiana_durable::timers::TimerCallback,
    delay_ms: f64,
    disarmed: Arc<std::sync::atomic::AtomicBool>,
}

impl Retained {
    fn sizes(&self) -> DocumentCacheSizes {
        self.session.document_cache_sizes()
    }

    fn advance(&self, ms: f64) {
        *support::lock(&self.clock) += ms;
    }

    /// 已武装（未撤销）的计时器延迟。
    fn armed_delays(&self) -> Vec<f64> {
        support::lock(&self.timers)
            .iter()
            .filter(|timer| !timer.disarmed.load(std::sync::atomic::Ordering::SeqCst))
            .map(|timer| timer.delay_ms)
            .collect()
    }

    /// 触发每个已武装的计时器（可重复触发），并让变更线跑完排队的工作。
    async fn fire_timers(&self) {
        let callbacks: Vec<kiana_durable::timers::TimerCallback> = support::lock(&self.timers)
            .iter()
            .filter(|timer| !timer.disarmed.load(std::sync::atomic::Ordering::SeqCst))
            .map(|timer| Arc::clone(&timer.callback))
            .collect();
        for callback in callbacks {
            callback();
        }
        self.settle_line().await;
    }

    async fn settle_line(&self) {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        self.session
            .read_on_line(async { Ok(()) })
            .await
            .expect("the line drains");
    }

    async fn bump(&self, by: i64) -> Result<(), BoxError> {
        let target = target(&self.counter, self.conversation_id);
        self.session
            .commit_with(
                move |tx| {
                    let target = target.clone();
                    Box::pin(async move {
                        let mut draft = tx.doc(&target).await?;
                        let count = draft
                            .get("count")
                            .and_then(|value| value.as_i64())
                            .unwrap_or(0);
                        draft.insert("count".to_owned(), json!(count + by));
                        Ok(())
                    })
                },
                ctx(),
                Default::default(),
            )
            .await
    }

    async fn read(&self) -> Option<kiana_durable::JsonObject> {
        snapshot(&self.session, &self.counter, self.conversation_id)
            .await
            .unwrap()
    }
}

async fn open(retention_ms: f64) -> Retained {
    open_with(
        Some(retention_ms),
        None,
        Arc::new(|_callback, _delay, _options| None),
    )
    .await
}

/// 接受并记录宿主计时器（文档清扫用的是 background 计时器）。
async fn open_timed(retention_ms: f64) -> Retained {
    let timers: Arc<Mutex<Vec<RecordedTimer>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&timers);
    let set_timer: SetTimer = Arc::new(move |callback, delay_ms, options| {
        if options.background {
            let disarmed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = Arc::clone(&disarmed);
            support::lock(&recorded).push(RecordedTimer {
                callback,
                delay_ms,
                disarmed,
            });
            return Some(Box::new(move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        None
    });
    let mut harness = open_with(Some(retention_ms), None, set_timer).await;
    harness.timers = timers;
    harness
}

/// 活保留期来源（TS 的 `documentRetentionMs: () => number | undefined`）。
async fn open_with_source(source: Arc<dyn Fn() -> Option<f64> + Send + Sync>) -> Retained {
    open_with(
        None,
        Some(source),
        Arc::new(|_callback, _delay, _options| None),
    )
    .await
}

async fn open_with(
    retention_ms: Option<f64>,
    retention_source: Option<Arc<dyn Fn() -> Option<f64> + Send + Sync>>,
    set_timer: SetTimer,
) -> Retained {
    let clock = Arc::new(Mutex::new(1_000.0_f64));
    let storage = Arc::new(ControlledStorage::new());
    let clock_for_now = Arc::clone(&clock);
    let session = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            now: Some(Arc::new(move || *support::lock(&clock_for_now))),
            document_retention_ms: retention_ms,
            document_retention_ms_source: retention_source,
            set_timer: Some(set_timer),
            ..Default::default()
        },
    );
    let conversation_id = create_conversation(&session).await;
    Retained {
        storage,
        session,
        clock,
        counter: counter_definition(),
        conversation_id,
        timers: Arc::new(Mutex::new(Vec::new())),
    }
}

#[tokio::test]
async fn evicts_an_unattached_document_once_its_retention_passed_and_loads_it_again() {
    let harness = open(60_000.0).await;
    harness.bump(3).await.unwrap();
    assert!(harness.sizes().cached > 0);

    // 还没到：在保留期内。
    harness.advance(59_000.0);
    harness.bump(0).await.unwrap();
    harness.settle_line().await;
    assert!(harness.sizes().cached > 0);

    // 超过保留期且之后没被使用：下一次提交清掉它。
    harness.advance(61_000.0);
    harness
        .session
        .read_on_line(async { Ok(()) })
        .await
        .expect("the line drains");
    let empty = harness.session.document_cache_sizes();
    let _ = empty;
    harness
        .session
        .commit_with(|_tx| Box::pin(async { Ok(()) }), ctx(), Default::default())
        .await
        .expect("the empty commit succeeds");
    let target = kiana_durable::DocTarget::session(&session_counter_definition());
    harness
        .session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    tx.doc(&target).await?;
                    Ok(())
                })
            },
            ctx(),
            Default::default(),
        )
        .await
        .expect("the session document commits");
    harness.settle_line().await;

    let reads = harness
        .storage
        .document_read_count
        .load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        harness.read().await,
        Some(serde_json::from_value(json!({ "count": 3 })).expect("an object"))
    );
    assert!(
        harness
            .storage
            .document_read_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > reads
    );
    harness.session.close(ctx()).await;
}

#[tokio::test]
async fn evicts_with_retention_zero_right_after_use_on_the_line() {
    let harness = open(0.0).await;
    harness.bump(1).await.unwrap();
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 0);
    assert_eq!(
        harness.read().await,
        Some(serde_json::from_value(json!({ "count": 1 })).expect("an object"))
    );
    harness.session.close(ctx()).await;
}

#[tokio::test]
async fn never_evicts_a_document_a_watch_observes_and_evicts_once_the_last_watch_detaches() {
    let harness = open(0.0).await;
    harness.bump(4).await.unwrap();
    let doc_target = target(&harness.counter, harness.conversation_id);
    let watch = harness
        .session
        .watch_doc(&doc_target, ctx())
        .await
        .unwrap()
        .expect("the document exists");
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 1);
    assert_eq!(harness.sizes().attached, 1);

    // 提交后仍不驱逐。
    harness.bump(1).await.unwrap();
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 1);

    watch.stop().await;
    harness.settle_line().await;
    // 解除 pin 之后，下一次提交的清扫让它离开。
    harness
        .session
        .commit_with(|_tx| Box::pin(async { Ok(()) }), ctx(), Default::default())
        .await
        .expect("the empty commit succeeds");
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 0);
    harness.session.close(ctx()).await;
}

#[tokio::test]
async fn writes_after_a_reload_continue_the_stored_deltas_and_bases() {
    let harness = open(0.0).await;
    harness.bump(2).await.unwrap();
    harness.settle_line().await;
    assert_eq!(
        harness.read().await,
        Some(serde_json::from_value(json!({ "count": 2 })).expect("an object"))
    );
    harness.bump(3).await.unwrap();
    let writes = support::lock(&harness.storage.commits)
        .last()
        .cloned()
        .expect("the commit reached storage");
    match &writes[0] {
        StorageWrite::DocumentChange { content, .. } => {
            assert_eq!(content.kind, DocumentContentKind::Delta);
            assert_eq!(content.version, 1);
        }
        _ => panic!("expected a document change"),
    }
    assert_eq!(
        harness.read().await,
        Some(serde_json::from_value(json!({ "count": 5 })).expect("an object"))
    );
    harness.session.close(ctx()).await;
}

#[tokio::test]
async fn keeps_history_and_forks_of_rewindable_documents_after_eviction() {
    let harness = open(0.0).await;
    let rewindable = rewindable_definition();
    let doc_target = target(&rewindable, harness.conversation_id);
    let entry = commit(&harness.session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            let entry = tx
                .append_entry(harness.conversation_id, entry_draft("point", None))
                .await?
                .id;
            let mut draft = tx.doc(&doc_target).await?;
            draft.insert("count".to_owned(), json!(1));
            Ok(entry)
        })
    })
    .await
    .expect("the rewindable document commits");
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 0);

    let child = commit(&harness.session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(
                harness.conversation_id,
                entry,
                &kiana_durable::CreateConversationOptions {
                    ownership: kiana_durable::ConversationOwnership::Ownerless,
                },
            )
            .await
        })
    })
    .await
    .expect("the fork commits");
    assert_eq!(
        snapshot_target(&harness.session, &target(&rewindable, child.id))
            .await
            .unwrap(),
        Some(serde_json::from_value(json!({ "count": 1 })).expect("an object"))
    );
    harness.session.close(ctx()).await;
}

#[tokio::test]
async fn document_retention_and_persist_within_validate_their_inputs() {
    use kiana_durable::session::{document_retention, persist_within};
    // 无效输入回落到**当前**默认值：`setup-document-retention` 夹具可以在第二遍把它改成 0
    // （`defaultRetention` 是进程级设置，TS 里同样可被 setDefaultDocumentRetention 改写）。
    let default_retention = document_retention(None);
    assert_eq!(document_retention(Some(0.0)), 0.0);
    assert_eq!(document_retention(Some(60_000.0)), 60_000.0);
    assert_eq!(document_retention(Some(f64::INFINITY)), f64::INFINITY);
    assert_eq!(document_retention(Some(-1.0)), default_retention);
    assert_eq!(document_retention(Some(f64::NAN)), default_retention);
    assert_eq!(document_retention(None), default_retention);
    assert_eq!(persist_within(Some(0.0)), 0.0);
    assert_eq!(persist_within(Some(f64::INFINITY)), f64::INFINITY);
    assert_eq!(persist_within(Some(-1.0)), 1_000.0);
    assert_eq!(persist_within(Some(f64::NAN)), 1_000.0);
    assert_eq!(persist_within(None), 1_000.0);
}

#[tokio::test]
async fn evicts_on_its_timer_without_a_commit() {
    let harness = open_timed(60_000.0).await;
    harness.bump(1).await.unwrap();
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 1);
    assert_eq!(harness.armed_delays(), vec![60_000.0]);

    harness.advance(61_000.0);
    harness.fire_timers().await;
    assert_eq!(harness.sizes().cached, 0);
    // 同一回调重复触发不改变结果。
    harness.fire_timers().await;
    assert_eq!(harness.sizes().cached, 0);
    harness.session.close(ctx()).await;
}

#[tokio::test]
async fn evicts_after_a_commit_whose_settlement_fails() {
    let exploding = kiana_durable::define_doc(
        DocDefinition::new(
            "test.retention.exploding",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| {
            let mut value = kiana_durable::JsonObject::new();
            value.insert("count".to_owned(), json!(0));
            value
        })
        .with_checkpoint_when(|_value, _ops, _info| panic!("checkpoint failed")),
    );
    let harness = open(0.0).await;
    let doc_target = target(&exploding, harness.conversation_id);
    commit(&harness.session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            tx.doc(&doc_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the creation commits");
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 0);

    let doc_target = target(&exploding, harness.conversation_id);
    let error = commit(&harness.session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&doc_target).await?;
            draft.insert("count".to_owned(), json!(1));
            Ok(())
        })
    })
    .await
    .expect_err("the settlement fails");
    assert!(error.to_string().contains("checkpoint failed"));
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 0);
    assert_eq!(
        snapshot_target(
            &harness.session,
            &target(&exploding, harness.conversation_id)
        )
        .await
        .unwrap()
        .and_then(|value| value.get("count").cloned()),
        Some(json!(0)),
        "the failed commit changed nothing"
    );
    harness.session.close(ctx()).await;
}

#[tokio::test]
async fn follows_a_live_retention_setting_at_the_next_use() {
    let setting = Arc::new(Mutex::new(Some(60_000.0)));
    let source_setting = Arc::clone(&setting);
    let source: Arc<dyn Fn() -> Option<f64> + Send + Sync> =
        Arc::new(move || *support::lock(&source_setting));
    let harness = open_with_source(source).await;
    harness.bump(1).await.unwrap();
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 1);

    // 缩短保留期后，下一次使用就清扫。
    *support::lock(&setting) = Some(0.0);
    harness.bump(0).await.unwrap();
    harness.settle_line().await;
    assert_eq!(harness.sizes().cached, 0);
    harness.session.close(ctx()).await;
}

#[derive(Default)]
struct ReportHooks {
    reports: Mutex<Vec<(String, kiana_durable::ReportInfo)>>,
}

impl kiana_durable::session::SessionHooks for ReportHooks {
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
async fn reports_a_host_timer_failure_without_changing_the_commit() {
    let storage = Arc::new(ControlledStorage::new());
    let hooks = Arc::new(ReportHooks::default());
    let set_timer: SetTimer = Arc::new(|_callback, _delay, _options| panic!("timer host failed"));
    let session = SessionImpl::new(
        storage,
        SessionOptions {
            document_retention_ms: Some(60_000.0),
            set_timer: Some(set_timer),
            hooks: Some(hooks.clone()),
            ..Default::default()
        },
    );
    let conversation_id = create_conversation(&session).await;
    let counter = counter_definition();
    let doc_target = target(&counter, conversation_id);
    session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    let mut draft = tx.doc(&doc_target).await?;
                    draft.insert("count".to_owned(), json!(1));
                    Ok(())
                })
            },
            ctx(),
            Default::default(),
        )
        .await
        .expect("a failing host timer does not change the commit outcome");
    for _ in 0..16 {
        tokio::task::yield_now().await;
        if !support::lock(&hooks.reports).is_empty() {
            break;
        }
    }
    {
        let reports = support::lock(&hooks.reports);
        assert!(
            reports
                .iter()
                .any(|(message, info)| message.contains("timer host failed")
                    && *info
                        == kiana_durable::ReportInfo::HostError {
                            callback: kiana_durable::HostCallback::SetTimer,
                            task: None,
                        }),
            "the timer failure is reported: {reports:?}"
        );
    }
    assert_eq!(
        snapshot(&session, &counter, conversation_id)
            .await
            .unwrap()
            .and_then(|value| value.get("count").cloned()),
        Some(json!(1))
    );
    session.close(ctx()).await;
}

#[tokio::test]
async fn never_changes_the_outcome_of_a_commit_when_the_clock_throws() {
    let storage = Arc::new(ControlledStorage::new());
    let decline: SetTimer = Arc::new(|_callback, _delay, _options| None);
    let session = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            now: Some(Arc::new(|| -> f64 { panic!("clock failed") })),
            document_retention_ms_source: Some(Arc::new(|| -> Option<f64> {
                panic!("settings failed")
            })),
            set_timer: Some(decline),
            ..Default::default()
        },
    );
    let conversation_id = create_conversation(&session).await;
    let counter = counter_definition();
    let doc_target = target(&counter, conversation_id);
    session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    let mut draft = tx.doc(&doc_target).await?;
                    draft.insert("count".to_owned(), json!(1));
                    Ok(())
                })
            },
            ctx(),
            Default::default(),
        )
        .await
        .expect("a throwing clock does not change the commit outcome");
    assert_eq!(
        snapshot(&session, &counter, conversation_id)
            .await
            .unwrap()
            .and_then(|value| value.get("count").cloned()),
        Some(json!(1))
    );
    session.close(ctx()).await;
}
