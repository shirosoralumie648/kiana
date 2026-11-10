//! `test/session-deferred-commits.test.ts` 的端口（节选）。

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_chord::context::Context;
use kiana_chord::delta::Op;
use kiana_durable::session::{DocumentCacheSizes, ObservedCommit, SessionImpl, SessionOptions};
use kiana_durable::timers::{SetTimer, SetTimerOptions, TimerCallback};
use kiana_durable::{
    BoxError, CommitOptions, CommitPersistence, ConversationId, DocDefinition, DocTarget, DocToken,
    DocumentCommitChange, DocumentContentKind, DocumentFork, DocumentHistory, DocumentId,
    DocumentPoint, DocumentScope, DocumentSemantics, JsonObject, ReadOnlySession, ScopeKind,
    Session, Storage, StorageWrite, WatchListener,
};
use serde_json::json;
use support::*;
use tokio::sync::mpsc;

/// 一份 latest、initial-fork 的会话文档（延迟提交只能持有这种）。
fn text_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "test.deferred.text",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("text".to_owned(), json!(""));
            value
        }),
    )
}

/// 可回退的会话文档：任何写入都不能被延迟持有。
fn rewindable_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "test.deferred.rewindable",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Rewindable),
                fork: Some(DocumentFork::AsOf),
            },
        )
        .with_initial(|_| JsonObject::new()),
    )
}

/// `current` fork 的会话文档：任何写入都不能被延迟持有。
fn current_fork_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "test.deferred.current-fork",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Current),
            },
        )
        .with_initial(|_| JsonObject::new()),
    )
}

/// Session 作用域文档：任何写入都不能被延迟持有。
fn session_text_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new("test.deferred.session", 1, DocumentSemantics::SESSION)
            .with_initial(|_| JsonObject::new()),
    )
}

fn deferred() -> CommitOptions {
    CommitOptions {
        persistence: Some(CommitPersistence::Deferred { within_ms: None }),
    }
}

fn immediate() -> CommitOptions {
    CommitOptions {
        persistence: Some(CommitPersistence::Immediate),
    }
}

fn within(within_ms: f64) -> CommitOptions {
    CommitOptions {
        persistence: Some(CommitPersistence::Deferred {
            within_ms: Some(within_ms),
        }),
    }
}

/// 一个手动计时器。
struct ArmedTimer {
    callback: TimerCallback,
    delay_ms: f64,
    disarmed: AtomicBool,
    fired: AtomicBool,
}

/// 带手动时钟与手动前台计时器的 Session。
struct Deferred {
    storage: Arc<ControlledStorage>,
    session: SessionImpl,
    stored: Arc<Mutex<Vec<kiana_durable::CommitPublication>>>,
    observed: Arc<Mutex<Vec<ObservedCommit>>>,
    conversation_id: ConversationId,
    record_id: DocumentId,
    timers: Arc<Mutex<Vec<Arc<ArmedTimer>>>>,
    clock: Arc<Mutex<f64>>,
    text: DocToken,
}

impl Deferred {
    fn sizes(&self) -> DocumentCacheSizes {
        self.session.document_cache_sizes()
    }

    fn armed(&self) -> Vec<Arc<ArmedTimer>> {
        let timers = support::lock(&self.timers);
        timers
            .iter()
            .filter(|timer| {
                !timer.disarmed.load(Ordering::SeqCst) && !timer.fired.load(Ordering::SeqCst)
            })
            .cloned()
            .collect()
    }

    fn armed_delays(&self) -> Vec<f64> {
        self.armed().iter().map(|timer| timer.delay_ms).collect()
    }

    fn target(&self) -> DocTarget {
        target(&self.text, self.conversation_id)
    }

    async fn stored_text(&self) -> Option<JsonObject> {
        self.storage
            .document(self.record_id, DocumentPoint::Current, ctx())
            .await
            .unwrap()
            .map(|document| document.value)
    }

    /// 触发所有已装载的计时器，并让变更线跑完它们排入的工作。
    async fn fire(&self) {
        let timers = {
            let timers = support::lock(&self.timers);
            timers.clone()
        };
        for timer in timers {
            if timer.disarmed.load(Ordering::SeqCst) || timer.fired.load(Ordering::SeqCst) {
                continue;
            }
            timer.fired.store(true, Ordering::SeqCst);
            (timer.callback)();
        }
        // 计时器回调把存储任务交给运行时；先让它排进变更线（偏离：TS 的回调直接 enqueue）。
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        self.session
            .read_on_line(async { Ok(()) })
            .await
            .expect("the line drains");
    }

    async fn close(&self) -> Result<(), BoxError> {
        self.session.close_result(ctx()).await
    }

    fn mark(&self) -> usize {
        support::lock(&self.storage.commits).len()
    }

    fn batches_since(&self, from: usize) -> Vec<Vec<StorageWrite>> {
        support::lock(&self.storage.commits)[from..].to_vec()
    }
}

async fn open_deferred(retention_ms: Option<f64>, persist_within_ms: Option<f64>) -> Deferred {
    let clock = Arc::new(Mutex::new(1_000.0_f64));
    let timers: Arc<Mutex<Vec<Arc<ArmedTimer>>>> = Arc::new(Mutex::new(Vec::new()));
    let set_timer: SetTimer = {
        let timers = Arc::clone(&timers);
        Arc::new(
            move |callback: TimerCallback, delay_ms: f64, options: SetTimerOptions| {
                if options.background {
                    return None;
                }
                let timer = Arc::new(ArmedTimer {
                    callback,
                    delay_ms,
                    disarmed: AtomicBool::new(false),
                    fired: AtomicBool::new(false),
                });
                support::lock(&timers).push(Arc::clone(&timer));
                Some(Box::new(move || {
                    timer.disarmed.store(true, Ordering::SeqCst);
                }) as Box<dyn FnOnce() + Send + Sync>)
            },
        )
    };
    let storage = Arc::new(ControlledStorage::new());
    let clock_for_now = Arc::clone(&clock);
    let session = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            now: Some(Arc::new(move || *support::lock(&clock_for_now))),
            document_retention_ms: Some(retention_ms.unwrap_or(600_000.0)),
            set_timer: Some(set_timer),
            persist_within_ms,
            ..Default::default()
        },
    );
    let stored = Arc::new(Mutex::new(Vec::new()));
    let _keep_stored = session
        .subscribe_commits({
            let stored = Arc::clone(&stored);
            Arc::new(move |publication, _context| {
                support::lock(&stored).push(publication.clone());
            })
        })
        .expect("a fresh Session accepts listeners");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let _keep_observed = session
        .observe_commits({
            let observed = Arc::clone(&observed);
            Arc::new(move |publication, _context| {
                support::lock(&observed).push(publication.clone());
            })
        })
        .expect("a fresh Session accepts listeners");

    let conversation_id = create_conversation(&session).await;
    let text = text_definition();
    let initial_target = target(&text, conversation_id);
    let address = initial_target.address();
    commit(&session, move |tx| {
        let initial_target = initial_target.clone();
        Box::pin(async move {
            tx.doc(&initial_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the initial document commits");
    let record_id = storage
        .find_document(&address, DocumentPoint::Current, ctx())
        .await
        .unwrap()
        .expect("the initial incarnation exists")
        .id;

    Deferred {
        storage,
        session,
        stored,
        observed,
        conversation_id,
        record_id,
        timers,
        clock,
        text,
    }
}

async fn append(harness: &Deferred, text: &str, options: CommitOptions) -> Result<(), BoxError> {
    let target = harness.target();
    let text = text.to_owned();
    harness
        .session
        .commit_with(
            move |tx| {
                let target = target.clone();
                let text = text.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&target).await?;
                    let current = draft
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_owned();
                    draft.insert("text".to_owned(), json!(format!("{current}{text}")));
                    Ok(())
                })
            },
            ctx(),
            options,
        )
        .await
}

fn document_changes_of(batches: &[Vec<StorageWrite>]) -> Vec<kiana_durable::DocumentContent> {
    batches
        .iter()
        .flat_map(|batch| batch.iter())
        .filter_map(|write| match write {
            StorageWrite::DocumentChange { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn ctx() -> &'static dyn Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

#[tokio::test]
async fn holds_an_existing_latest_initial_fork_conversation_document_in_memory() {
    let harness = open_deferred(None, None).await;
    let admitted = harness.storage.commits_len();
    let published = support::lock(&harness.stored).len();

    append(&harness, "a", deferred()).await.unwrap();
    append(&harness, "b", deferred()).await.unwrap();

    assert_eq!(harness.storage.commits_len(), admitted);
    assert_eq!(support::lock(&harness.stored).len(), published);
    assert_eq!(
        snapshot(&harness.session, &harness.text, harness.conversation_id)
            .await
            .unwrap(),
        Some(serde_json::from_value(json!({ "text": "ab" })).expect("an object"))
    );
    assert_eq!(
        harness.stored_text().await,
        Some(serde_json::from_value(json!({ "text": "" })).expect("an object"))
    );
    assert_eq!(harness.sizes().held, 1);
    let observed = support::lock(&harness.observed);
    let kinds: Vec<_> = observed[observed.len() - 2..]
        .iter()
        .map(ObservedCommit::kind)
        .collect();
    assert_eq!(kinds, ["memory", "memory"]);
}

#[tokio::test]
async fn stores_a_deferred_commit_at_once_when_it_writes_anything_else() {
    let harness = open_deferred(None, None).await;
    let rewindable = rewindable_definition();
    let session_copy = &harness.session;
    let conversation_id = harness.conversation_id;

    // 与 held 文档变化同批的条目。
    let target = harness.target();
    let entry_before = harness.mark();
    session_copy
        .commit_with(
            move |tx| {
                let target = target.clone();
                Box::pin(async move {
                    tx.doc(&target).await?;
                    tx.append_entry(conversation_id, entry_draft("test.note", None))
                        .await?;
                    Ok(())
                })
            },
            ctx(),
            deferred(),
        )
        .await
        .unwrap();
    assert!(harness.batches_since(entry_before).len() == 1);

    // 创建、其他语义的文档、会话文档与退役都会立即写。
    let cases: Vec<(DocTarget, bool)> = vec![
        (DocTarget::conversation(&rewindable, conversation_id), false),
        (DocTarget::conversation(&rewindable, conversation_id), true),
        (DocTarget::session(&session_text_definition()), false),
        (DocTarget::session(&session_text_definition()), true),
        (
            DocTarget::conversation(&current_fork_definition(), conversation_id),
            false,
        ),
    ];
    for (case, mutate) in cases {
        let before = harness.mark();
        session_copy
            .commit_with(
                move |tx| {
                    let case = case.clone();
                    Box::pin(async move {
                        let mut draft = tx.doc(&case).await?;
                        if mutate {
                            draft.insert("text".to_owned(), json!("x"));
                        }
                        Ok(())
                    })
                },
                ctx(),
                deferred(),
            )
            .await
            .unwrap();
        assert_eq!(
            harness.batches_since(before).len(),
            1,
            "the commit is stored at once"
        );
    }

    // 退役也会立即写（退役的是另一份文档，held 文档不受影响）。
    let before = harness.mark();
    let retire_target = DocTarget::conversation(&current_fork_definition(), conversation_id);
    session_copy
        .commit_with(
            move |tx| {
                let retire_target = retire_target.clone();
                Box::pin(async move {
                    tx.retire_doc(&retire_target).await?;
                    Ok(())
                })
            },
            ctx(),
            deferred(),
        )
        .await
        .unwrap();
    assert_eq!(harness.batches_since(before).len(), 1);

    // 对照：单独的同一种变化仍被持有。
    let before = harness.mark();
    append(&harness, "z", deferred()).await.unwrap();
    assert!(harness.batches_since(before).is_empty());
    assert_eq!(harness.sizes().held, 1);
}

#[tokio::test]
async fn rejects_a_deferred_commit_on_a_read_only_session_like_any_commit_with_writes() {
    let harness = open_deferred(None, None).await;
    let viewer = SessionImpl::new(
        harness.storage.clone(),
        SessionOptions {
            read_only: true,
            ..Default::default()
        },
    );
    let target = target(&harness.text, harness.conversation_id);
    let error = viewer
        .commit_with(
            move |tx| {
                let target = target.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&target).await?;
                    draft.insert("text".to_owned(), json!("a"));
                    Ok(())
                })
            },
            ctx(),
            deferred(),
        )
        .await
        .expect_err("a read-only Session rejects commits with writes");
    assert!(error.downcast_ref::<ReadOnlySession>().is_some());
}

#[tokio::test]
async fn rejects_a_within_ms_that_is_not_a_number_of_milliseconds_or_more() {
    let harness = open_deferred(None, None).await;
    let error = append(&harness, "a", within(-1.0))
        .await
        .expect_err("a negative window is rejected");
    assert_eq!(
        error.to_string(),
        "CommitPersistence.withinMs must be a number of milliseconds, 0 or more"
    );
    append(&harness, "a", immediate())
        .await
        .expect("immediate commits are unaffected");
}

#[tokio::test]
async fn stores_held_changes_first_folded_with_its_own_in_a_commit_that_writes_the_document() {
    let harness = open_deferred(None, None).await;
    append(&harness, "a", deferred()).await.unwrap();
    append(&harness, "b", deferred()).await.unwrap();
    let before = harness.mark();
    append(&harness, "c", immediate()).await.unwrap();

    let batches = harness.batches_since(before);
    assert_eq!(batches.len(), 1);
    let changes = document_changes_of(&batches);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].version, 1);
    assert_eq!(changes[0].kind, kiana_durable::DocumentContentKind::Delta);
    assert_eq!(
        kiana_chord::delta::index::encode(changes[0].ops.as_ref().expect("ops are present")),
        vec![json!(["a", ["text"], "abc"])]
    );
    assert_eq!(
        harness.stored_text().await,
        Some(serde_json::from_value(json!({ "text": "abc" })).expect("an object"))
    );
    assert_eq!(harness.sizes().held, 0);
}

#[tokio::test]
async fn leaves_them_held_through_commits_that_neither_read_nor_write_the_document() {
    let harness = open_deferred(None, None).await;
    append(&harness, "held", deferred()).await.unwrap();
    let before = harness.mark();
    let conversation_id = harness.conversation_id;
    harness
        .session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    tx.append_entry(conversation_id, entry_draft("test.unrelated", None))
                        .await?;
                    Ok(())
                })
            },
            ctx(),
            Default::default(),
        )
        .await
        .unwrap();
    let target = harness.target();
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
        .unwrap();
    assert!(document_changes_of(&harness.batches_since(before)).is_empty());
    assert_eq!(harness.sizes().held, 1);
}

#[tokio::test]
async fn stores_every_held_document_in_one_transaction_when_the_earliest_deadline_comes() {
    let harness = open_deferred(None, None).await;
    let other = create_conversation(&harness.session).await;
    let other_target = target(&harness.text, other);
    harness
        .session
        .commit_with(
            move |tx| {
                let other_target = other_target.clone();
                Box::pin(async move {
                    tx.doc(&other_target).await?;
                    Ok(())
                })
            },
            ctx(),
            Default::default(),
        )
        .await
        .unwrap();

    append(&harness, "one", deferred()).await.unwrap();
    let other_text = target(&harness.text, other);
    harness
        .session
        .commit_with(
            move |tx| {
                let other_text = other_text.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&other_text).await?;
                    draft.insert("text".to_owned(), json!("two"));
                    Ok(())
                })
            },
            ctx(),
            deferred(),
        )
        .await
        .unwrap();
    assert_eq!(harness.armed_delays(), [1_000.0]);

    let published = support::lock(&harness.stored).len();
    let before = harness.mark();
    harness.fire().await;
    let batches = harness.batches_since(before);
    assert_eq!(batches.len(), 1);
    assert_eq!(document_changes_of(&batches).len(), 2);
    assert_eq!(support::lock(&harness.stored).len(), published + 1);
    assert_eq!(harness.sizes().held, 0);
    assert!(harness.armed().is_empty());

    let other_id = harness
        .storage
        .find_document(
            &other_text_address(&harness.text, other),
            DocumentPoint::Current,
            ctx(),
        )
        .await
        .unwrap()
        .unwrap()
        .id;
    assert_eq!(
        harness
            .storage
            .document(other_id, DocumentPoint::Current, ctx())
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::from_value::<JsonObject>(json!({ "text": "two" })).unwrap()
    );
}

fn other_text_address(
    token: &DocToken,
    conversation_id: ConversationId,
) -> kiana_durable::DocumentAddress {
    target(token, conversation_id).address()
}

#[tokio::test]
async fn arms_the_earliest_deadline_of_the_held_commits() {
    let harness = open_deferred(None, Some(2_000.0)).await;
    append(&harness, "a", deferred()).await.unwrap();
    assert_eq!(harness.armed_delays(), [2_000.0]);

    *support::lock(&harness.clock) += 500.0;
    append(&harness, "b", within(100.0)).await.unwrap();
    assert_eq!(harness.armed_delays(), [100.0]);

    append(&harness, "c", within(60_000.0)).await.unwrap();
    assert_eq!(harness.armed_delays(), [100.0]);

    // 无限的窗口不装载计时器。
    let infinite = open_deferred(None, None).await;
    append(&infinite, "a", within(f64::INFINITY)).await.unwrap();
    assert!(infinite.armed().is_empty());
    assert_eq!(infinite.sizes().held, 1);
}

#[tokio::test]
async fn stores_them_at_close_after_the_commits_admitted_before_it() {
    let harness = open_deferred(None, None).await;
    append(&harness, "held", deferred()).await.unwrap();
    harness.close().await.expect("close succeeds");
    harness.storage.reopen();
    assert_eq!(
        harness
            .storage
            .document(harness.record_id, DocumentPoint::Current, ctx())
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::from_value::<JsonObject>(json!({ "text": "held" })).unwrap()
    );

    let failing = open_deferred(None, None).await;
    append(&failing, "lost", deferred()).await.unwrap();
    failing
        .storage
        .fail_next_commit(std::io::Error::other("disk gone").into());
    let error = failing.close().await.expect_err("the closing store fails");
    assert_eq!(error.to_string(), "disk gone");
    failing.storage.reopen();
    assert_eq!(
        failing
            .storage
            .document(failing.record_id, DocumentPoint::Current, ctx())
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::from_value::<JsonObject>(json!({ "text": "" })).unwrap()
    );
}

#[tokio::test]
async fn stores_nothing_once_the_session_failed() {
    let harness = open_deferred(None, None).await;
    append(&harness, "lost", deferred()).await.unwrap();
    let before = harness.mark();
    harness.session.fail(std::io::Error::other("crash").into());
    let _ = harness.session.closed().await;
    assert!(harness.batches_since(before).is_empty());
    harness.storage.reopen();
    assert_eq!(
        harness
            .storage
            .document(harness.record_id, DocumentPoint::Current, ctx())
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::from_value::<JsonObject>(json!({ "text": "" })).unwrap()
    );
}

#[tokio::test]
async fn stores_them_before_unload_documents_drops_the_trackers() {
    let harness = open_deferred(None, None).await;
    append(&harness, "held", deferred()).await.unwrap();
    harness
        .session
        .unload_documents()
        .await
        .expect("unload succeeds");
    assert_eq!(
        harness.stored_text().await,
        Some(serde_json::from_value(json!({ "text": "held" })).expect("an object"))
    );
    let sizes = harness.sizes();
    assert_eq!(sizes.cached, 0);
    assert_eq!(sizes.held, 0);
    assert_eq!(
        snapshot(&harness.session, &harness.text, harness.conversation_id)
            .await
            .unwrap(),
        Some(serde_json::from_value(json!({ "text": "held" })).expect("an object"))
    );
}

#[tokio::test]
async fn stores_them_before_the_documents_retirement_in_the_retiring_commit() {
    let harness = open_deferred(None, None).await;
    append(&harness, "held", deferred()).await.unwrap();
    let before = harness.mark();
    let retire_target = harness.target();
    harness
        .session
        .commit_with(
            move |tx| {
                let retire_target = retire_target.clone();
                Box::pin(async move {
                    tx.retire_doc(&retire_target).await?;
                    Ok(())
                })
            },
            ctx(),
            Default::default(),
        )
        .await
        .unwrap();
    let batches = harness.batches_since(before);
    assert_eq!(batches.len(), 1);
    let kinds: Vec<_> = batches[0]
        .iter()
        .map(|write| match write {
            StorageWrite::DocumentChange { .. } => "document.change",
            StorageWrite::DocumentRetire { .. } => "document.retire",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["document.change", "document.retire"]);
    let changes = document_changes_of(&batches);
    assert_eq!(changes.len(), 1);
    assert_eq!(
        kiana_chord::delta::index::encode(changes[0].ops.as_ref().expect("ops are present")),
        vec![json!(["a", ["text"], "held"])]
    );
    assert_eq!(harness.sizes().held, 0);
    assert!(
        snapshot(&harness.session, &harness.text, harness.conversation_id)
            .await
            .unwrap()
            .is_none()
    );
}

// 让文档作用域类型进入测试，覆盖 `DocumentScope` 的构造路径。
#[allow(dead_code)]
fn _scope_used() -> DocumentScope {
    DocumentScope::Conversation {
        conversation_id: kiana_durable::id_from_number(1),
    }
}

#[allow(dead_code)]
fn _changes_used(change: DocumentCommitChange) -> DocumentCommitChange {
    change
}

// ---------------------------------------------------------------------------
// 更完整的 session-deferred-commits 覆盖
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stores_held_changes_in_a_commit_that_only_reads_the_document() {
    let harness = open_deferred(None, None).await;
    append(&harness, "held", deferred()).await.unwrap();
    let before = harness.mark();
    let doc_target = harness.target();
    let conversation_id = harness.conversation_id;
    commit(&harness.session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            let text = tx
                .doc(&doc_target)
                .await?
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_owned();
            tx.append_entry(
                conversation_id,
                entry_draft("test.derived", Some(json!(text))),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .expect("the read-only commit succeeds");
    let batches = harness.batches_since(before);
    assert_eq!(batches.len(), 1);
    let mut types: Vec<&str> = batches[0]
        .iter()
        .map(|write| match write {
            StorageWrite::DocumentChange { .. } => "document.change",
            StorageWrite::Entry { .. } => "entry",
            _ => "other",
        })
        .collect();
    types.sort_unstable();
    assert_eq!(types, ["document.change", "entry"]);
    assert_eq!(
        harness.stored_text().await,
        Some(serde_json::from_value(json!({ "text": "held" })).expect("an object"))
    );
}

#[tokio::test]
async fn keeps_held_changes_when_a_commit_that_reads_the_document_fails_before_storage() {
    let harness = open_deferred(None, None).await;
    append(&harness, "held", deferred()).await.unwrap();
    let doc_target = harness.target();
    let conversation_id = harness.conversation_id;
    let error = commit::<(), _>(&harness.session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&doc_target).await?;
            draft.insert("text".to_owned(), json!("!"));
            tx.append_entry(conversation_id, entry_draft("test.note", None))
                .await?;
            Err(std::io::Error::other("callback failed").into())
        })
    })
    .await
    .expect_err("the callback failure fails the commit");
    assert_eq!(error.to_string(), "callback failed");
    assert_eq!(harness.sizes().held, 1);
    harness.fire().await;
    assert_eq!(
        harness.stored_text().await,
        Some(serde_json::from_value(json!({ "text": "held" })).expect("an object"))
    );
}

#[tokio::test]
async fn arms_no_timer_for_an_infinite_window_and_moves_it_when_the_document_is_stored() {
    let harness = open_deferred(None, None).await;
    append(&harness, "a", within(f64::INFINITY)).await.unwrap();
    assert!(harness.armed().is_empty());
    assert_eq!(harness.sizes().held, 1);
    append(&harness, "b", deferred()).await.unwrap();
    assert_eq!(harness.armed_delays(), vec![1_000.0]);
    append(&harness, "c", immediate()).await.unwrap();
    assert!(harness.armed().is_empty());
    assert_eq!(harness.sizes().held, 0);
    assert_eq!(
        harness.stored_text().await,
        Some(serde_json::from_value(json!({ "text": "abc" })).expect("an object"))
    );
}

#[tokio::test]
async fn stores_a_deferred_commit_that_migrates_its_document_at_once_as_a_base() {
    let harness = open_deferred(None, None).await;
    let migrated = kiana_durable::define_doc(
        DocDefinition::new(
            "test.deferred.text",
            2,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("text".to_owned(), json!(""));
            value.insert("migrated".to_owned(), json!(true));
            value
        })
        .with_migrate(|value, from_version| {
            assert_eq!(from_version, 1);
            let mut migrated = JsonObject::new();
            migrated.insert(
                "text".to_owned(),
                value.get("text").cloned().unwrap_or(json!("")),
            );
            migrated.insert("migrated".to_owned(), json!(true));
            migrated
        }),
    );
    let before = harness.mark();
    let doc_target = target(&migrated, harness.conversation_id);
    harness
        .session
        .commit_with(
            move |tx| {
                let doc_target = doc_target.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&doc_target).await?;
                    draft.insert("text".to_owned(), json!("v2"));
                    Ok(())
                })
            },
            ctx(),
            deferred(),
        )
        .await
        .expect("the migrating deferred commit succeeds");
    let batches = harness.batches_since(before);
    assert_eq!(batches.len(), 1, "a migrating commit stores at once");
    let changes = document_changes_of(&batches);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, DocumentContentKind::Base);
    assert_eq!(changes[0].version, 2);
    assert_eq!(harness.sizes().held, 0);
}

struct TextWatchListener {
    sender: mpsc::UnboundedSender<String>,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for TextWatchListener {
    async fn call(&self, value: Option<JsonObject>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        let text = value
            .as_ref()
            .and_then(|value| value.get("text").and_then(|value| value.as_str()))
            .unwrap_or("")
            .to_owned();
        let _ = self.sender.send(text);
    }
}

async fn settle_delivery() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
}

#[tokio::test]
async fn shows_every_held_commit_to_watches_and_observers_once() {
    let harness = open_deferred(None, None).await;
    let doc_target = harness.target();
    let watch = harness
        .session
        .watch_doc(&doc_target, ctx())
        .await
        .unwrap()
        .expect("the document exists");
    let (sender, mut receiver) = mpsc::unbounded_channel();
    watch.start(Arc::new(TextWatchListener { sender }));
    let from = support::lock(&harness.observed).len();
    for text in ["a", "b", "c"] {
        append(&harness, text, deferred()).await.unwrap();
        settle_delivery().await;
    }
    append(&harness, "d", immediate()).await.unwrap();
    settle_delivery().await;
    harness.fire().await;

    let mut values = Vec::new();
    while let Ok(value) = receiver.try_recv() {
        values.push(value);
    }
    assert_eq!(values, ["a", "ab", "abc", "abcd"]);

    let seen: Vec<ObservedCommit> = {
        let observed = support::lock(&harness.observed);
        observed[from..].to_vec()
    };
    assert_eq!(
        seen.iter().map(ObservedCommit::kind).collect::<Vec<_>>(),
        ["memory", "memory", "memory", "stored"]
    );
    let ops: Vec<Vec<serde_json::Value>> = seen
        .iter()
        .map(|publication| {
            publication
                .changes()
                .into_iter()
                .filter_map(|change| match change {
                    kiana_durable::CommitChange::Document(DocumentCommitChange::Document {
                        ops,
                        ..
                    }) => Some(kiana_chord::delta::index::encode(&ops.0)),
                    _ => None,
                })
                .flatten()
                .collect()
        })
        .collect();
    assert_eq!(
        ops,
        vec![
            vec![json!(["a", ["text"], "a"])],
            vec![json!(["a", ["text"], "b"])],
            vec![json!(["a", ["text"], "c"])],
            vec![json!(["a", ["text"], "d"])],
        ]
    );
    watch.stop().await;
    harness.close().await.expect("close succeeds");
}

// ---------------------------------------------------------------------------
// 回归：变更线按**调用**顺序接纳（TS 的 `#tail` 链）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admits_commits_in_call_order_even_when_awaited_out_of_order() {
    let (_storage, session, _publications) = open_test_session();
    let context: &dyn Context = &**kiana_chord::context::BACKGROUND_CONTEXT;

    // TS：`const a = session.commit(A); const b = session.commit(B);`
    let first = session.commit_with(
        |tx| {
            Box::pin(async move {
                Ok(tx
                    .create_conversation(&kiana_durable::CreateConversationOptions {
                        ownership: kiana_durable::ConversationOwnership::Ownerless,
                    })
                    .await?
                    .id)
            })
        },
        context,
        Default::default(),
    );
    let second = session.commit_with(
        |tx| {
            Box::pin(async move {
                Ok(tx
                    .create_conversation(&kiana_durable::CreateConversationOptions {
                        ownership: kiana_durable::ConversationOwnership::Ownerless,
                    })
                    .await?
                    .id)
            })
        },
        context,
        Default::default(),
    );
    let b = second.await.expect("B commits");
    let a = first.await.expect("A commits");
    assert!(
        a < b,
        "the commit called first is admitted first (A={a}, B={b})"
    );
    session.close(context).await;
}
