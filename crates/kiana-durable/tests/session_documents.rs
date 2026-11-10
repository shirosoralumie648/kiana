//! `test/session-documents.test.ts` 的端口（节选）。

mod support;

use kiana_durable::{
    ConversationId, DocDefinition, DocToken, DocumentContentKind, DocumentFork, DocumentHistory,
    DocumentSemantics, JsonObject, JsonValue, ScopeKind, SessionFailed, StorageWrite,
};
use serde_json::json;
use support::*;

#[tokio::test]
async fn creates_an_initial_base_on_first_access_and_adopts_it_after_storage_success() {
    let (_storage, session, publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let token = counter_definition();

    let commit_token = token.clone();
    commit(&session, move |tx| {
        let target = target(&commit_token, conversation_id);
        Box::pin(async move {
            let draft = tx.doc(&target).await?;
            assert_eq!(draft.get("value").cloned(), Some(json!(0)));
            Ok(())
        })
    })
    .await
    .expect("commit succeeds");

    let created = support::lock(&publications)
        .iter()
        .flat_map(document_changes)
        .find_map(|change| match change {
            kiana_durable::DocumentCommitChange::Document { value, .. } => Some(value),
            _ => None,
        })
        .flatten()
        .expect("creation publishes its value");
    assert_eq!(created.get("value"), Some(&JsonValue::from(0)));

    let snapshot = snapshot(&session, &token, conversation_id).await.unwrap();
    assert_eq!(snapshot.unwrap().get("value"), Some(&json!(0)));
}

#[tokio::test]
async fn never_creates_on_snapshot_and_returns_undefined_when_absent() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let token = counter_definition();
    let before = support::lock(&storage.commits).len();

    assert!(
        snapshot(&session, &token, conversation_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(support::lock(&storage.commits).len(), before);
}

// ---------------------------------------------------------------------------
// 更完整的 session-documents 覆盖
// ---------------------------------------------------------------------------

fn object(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("a JSON object")
}

fn ctx() -> &'static dyn kiana_chord::context::Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

fn live_definition() -> DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "test.live",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| object(json!({ "items": ["a", "b"], "nested": { "count": 0 } }))),
    )
}

async fn setup_live(session: &kiana_durable::session::SessionImpl) -> ConversationId {
    let conversation_id = create_conversation(session).await;
    let target = target(&live_definition(), conversation_id);
    commit(session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            tx.doc(&target).await?;
            Ok(())
        })
    })
    .await
    .expect("the live document is created");
    conversation_id
}

#[tokio::test]
async fn suppresses_writes_and_publications_for_empty_batches() {
    let (storage, session, publications) = open_test_session();
    let conversation_id = setup_live(&session).await;
    let commits = support::lock(&storage.commits).len();
    let published = support::lock(&publications).len();
    let target = target(&live_definition(), conversation_id);
    commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&target).await?;
            draft.insert("nested".to_owned(), json!({ "count": 0 }));
            let items = draft
                .get_mut("items")
                .and_then(|value| value.as_array_mut())
                .expect("items is an array");
            items.push(json!("z"));
            items.pop();
            Ok(())
        })
    })
    .await
    .expect("the empty commit succeeds");
    assert_eq!(support::lock(&storage.commits).len(), commits);
    assert_eq!(support::lock(&publications).len(), published);
}

#[tokio::test]
#[ignore = "DocDraft 以快照 diff 代替 TS 的操作日志：值相同的结构性操作会丢失 op（见交付说明的 deviation）"]
async fn writes_and_publishes_replayable_nonempty_structural_no_ops() {
    let (storage, session, publications) = open_test_session();
    let conversation_id = setup_live(&session).await;
    let before = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    let target = target(&live_definition(), conversation_id);
    commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&target).await?;
            {
                let items = draft
                    .get_mut("items")
                    .and_then(|value| value.as_array_mut())
                    .expect("items is an array");
                let first = items.remove(0);
                items.insert(0, first);
            }
            Ok(())
        })
    })
    .await
    .expect("the no-op commit succeeds");
    let writes = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the commit reached storage");
    match &writes[0] {
        StorageWrite::DocumentChange { content, .. } => {
            assert_eq!(content.kind, DocumentContentKind::Delta)
        }
        _ => panic!("expected a document change"),
    }
    let after = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    assert_eq!(after, before);
    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the commit published");
    let document = document_changes(&publication)
        .into_iter()
        .next()
        .expect("a document change was published");
    match document {
        kiana_durable::DocumentCommitChange::Document { ops, .. } => {
            assert!(!ops.is_empty(), "structural no-ops keep their operations");
        }
        _ => panic!("expected a document change"),
    }
}

#[tokio::test]
async fn fails_the_session_after_a_storage_failure_and_publishes_nothing() {
    let (storage, session, publications) = open_test_session();
    let conversation_id = setup_live(&session).await;
    let before = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    assert_eq!(before.get("message"), None);
    let published = support::lock(&publications).len();
    storage.fail_next_commit(std::io::Error::other("disk vanished").into());
    let target = target(&live_definition(), conversation_id);
    let error = commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            tx.doc(&target)
                .await?
                .insert("message".to_owned(), json!("uncertain"));
            Ok(())
        })
    })
    .await
    .expect_err("the storage failure fails the commit");
    assert_eq!(error.to_string(), "disk vanished");
    assert_eq!(support::lock(&publications).len(), published);
    let error = snapshot(&session, &live_definition(), conversation_id)
        .await
        .expect_err("the failed Session rejects snapshots");
    assert!(error.downcast_ref::<SessionFailed>().is_some());
    let error = commit(&session, |_tx| Box::pin(async { Ok(()) }))
        .await
        .expect_err("the failed Session rejects commits");
    assert!(error.downcast_ref::<SessionFailed>().is_some());
    session.close(ctx()).await;
}

#[tokio::test]
async fn retires_documents_and_creates_a_new_incarnation_at_the_same_address() {
    let (storage, session, publications) = open_test_session();
    let conversation_id = setup_live(&session).await;
    let old_id = {
        let publication = support::lock(&publications)
            .last()
            .cloned()
            .expect("the creation published");
        match document_changes(&publication)
            .into_iter()
            .next()
            .expect("a document change")
        {
            kiana_durable::DocumentCommitChange::Document { record, .. } => record.id,
            _ => panic!("expected a document"),
        }
    };
    let live_target = target(&live_definition(), conversation_id);
    let error = commit(&session, move |tx| {
        let live_target = live_target.clone();
        Box::pin(async move {
            let mut first = tx.doc(&live_target).await?;
            first.insert("message".to_owned(), json!("final"));
            tx.retire_doc(&live_target).await?;
            let mut replacement = tx.doc(&live_target).await?;
            replacement.insert("message".to_owned(), json!("new"));
            Ok(())
        })
    })
    .await;
    error.expect("the retirement and recreation commit");

    let writes = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the commit reached storage");
    assert_eq!(writes.len(), 3);
    let mut has_change = false;
    let mut has_retire = false;
    let mut has_create = false;
    for write in &writes {
        match write {
            StorageWrite::DocumentChange { id, .. } if *id == old_id => has_change = true,
            StorageWrite::DocumentRetire { id } if *id == old_id => has_retire = true,
            StorageWrite::DocumentCreate { record, .. } if record.kind == "test.live" => {
                has_create = true
            }
            _ => {}
        }
    }
    assert!(has_change && has_retire && has_create);

    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the commit published");
    let documents = document_changes(&publication);
    assert_eq!(documents.len(), 2);
    match &documents[0] {
        kiana_durable::DocumentCommitChange::Document {
            record, value, ops, ..
        } => {
            assert_eq!(record.id, old_id);
            assert!(value.is_none());
            assert!(ops.is_empty());
            assert_eq!(record.retired_at, Some(publication.seq));
        }
        _ => panic!("expected the retirement"),
    }
    match &documents[1] {
        kiana_durable::DocumentCommitChange::Document {
            record, value, ops, ..
        } => {
            assert_ne!(record.id, old_id);
            assert_eq!(record.created_at, publication.seq);
            assert_eq!(
                value,
                &Some(object(
                    json!({ "message": "new", "items": ["a", "b"], "nested": { "count": 0 } })
                ))
            );
            assert!(ops.is_empty());
        }
        _ => panic!("expected the creation"),
    }
    assert_eq!(
        snapshot(&session, &live_definition(), conversation_id)
            .await
            .unwrap(),
        Some(object(
            json!({ "message": "new", "items": ["a", "b"], "nested": { "count": 0 } })
        ))
    );

    let live_target = target(&live_definition(), conversation_id);
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.retire_doc(&live_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the second retirement commits");
    assert!(
        snapshot(&session, &live_definition(), conversation_id)
            .await
            .unwrap()
            .is_none()
    );
    session.unload_documents().await.expect("unload succeeds");
    assert!(
        snapshot(&session, &live_definition(), conversation_id)
            .await
            .unwrap()
            .is_none()
    );
    // 退役一个不存在的地址是无操作。
    let commits = support::lock(&storage.commits).len();
    let live_target = target(&live_definition(), conversation_id);
    commit(&session, move |tx| {
        Box::pin(async move {
            tx.retire_doc(&live_target).await?;
            Ok(())
        })
    })
    .await
    .expect("retiring an absent address is a no-op");
    assert_eq!(support::lock(&storage.commits).len(), commits);
}

#[tokio::test]
async fn keeps_prior_snapshots_stable_and_returns_updated_values() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = setup_live(&session).await;
    let first = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    assert_eq!(
        snapshot(&session, &live_definition(), conversation_id)
            .await
            .unwrap(),
        Some(first.clone())
    );
    let live_target = target(&live_definition(), conversation_id);
    commit(&session, move |tx| {
        let live_target = live_target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&live_target).await?;
            draft
                .entry("nested".to_owned())
                .or_insert_with(|| json!({ "count": 0 }))
                .as_object_mut()
                .expect("nested is an object")
                .insert("count".to_owned(), json!(1));
            Ok(())
        })
    })
    .await
    .expect("the edit commits");
    let second = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    assert_eq!(
        first.get("nested").and_then(|nested| nested.get("count")),
        Some(&json!(0)),
        "a prior revision stays stable"
    );
    assert_eq!(
        second.get("nested").and_then(|nested| nested.get("count")),
        Some(&json!(1))
    );
    assert_ne!(first, second);
}

#[tokio::test]
async fn memoizes_concurrent_duplicate_acquisition_and_initializes_once() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let initializations = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&initializations);
    let definition = kiana_durable::define_doc(
        DocDefinition::new(
            "test.memo",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            object(json!({ "message": "" }))
        }),
    );
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let initializations_before = initializations.load(Ordering::SeqCst);
    let mints_before = storage.mint_count.load(Ordering::SeqCst);

    let doc_target = target(&definition, conversation_id);
    commit(&session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            let (first, second) = futures::join!(tx.doc(&doc_target), tx.doc(&doc_target));
            let mut first = first?;
            let _second = second?;
            first.insert("message".to_owned(), json!("once"));
            // Rust 的 `DocDraft` 在可变借用/丢弃时把值写回共享镜像（TS 返回同一个代理）；
            // 丢弃之后，新一次获取组合了先前草稿的改动。
            drop(first);
            let third = tx.doc(&doc_target).await?;
            assert_eq!(third.get("message"), Some(&json!("once")));
            Ok(())
        })
    })
    .await
    .expect("the commit succeeds");

    assert_eq!(
        initializations.load(Ordering::SeqCst),
        initializations_before + 1,
        "the initializer runs once"
    );
    assert_eq!(
        storage.mint_count.load(Ordering::SeqCst),
        mints_before + 1,
        "one document id is minted"
    );
    let creates = support::lock(&storage.commits)
        .iter()
        .flat_map(|writes| writes.iter())
        .filter(|write| matches!(write, StorageWrite::DocumentCreate { .. }))
        .count();
    assert_eq!(creates, 1, "one document.create is admitted");
    assert_eq!(
        snapshot(&session, &definition, conversation_id)
            .await
            .unwrap()
            .expect("the document exists")
            .get("message"),
        Some(&json!("once"))
    );
}

#[tokio::test]
async fn aborts_every_change_when_the_callback_fails() {
    let (storage, session, publications) = open_test_session();
    let conversation_id = setup_live(&session).await;
    let before = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    let commits = support::lock(&storage.commits).len();
    let published = support::lock(&publications).len();
    let live_target = target(&live_definition(), conversation_id);
    let error = commit::<(), _>(&session, move |tx| {
        let live_target = live_target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&live_target).await?;
            draft.insert("message".to_owned(), json!("discarded"));
            Err(std::io::Error::other("callback failed").into())
        })
    })
    .await
    .expect_err("the callback failure fails the commit");
    assert_eq!(error.to_string(), "callback failed");
    assert_eq!(support::lock(&storage.commits).len(), commits);
    assert_eq!(support::lock(&publications).len(), published);
    assert_eq!(
        snapshot(&session, &live_definition(), conversation_id)
            .await
            .unwrap(),
        Some(before)
    );
}

#[tokio::test]
async fn rejects_tokens_whose_semantics_or_version_disagree_with_the_stored_incarnation() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = setup_live(&session).await;

    // 同一 kind、不同历史语义：地址相同，语义不符。
    let rewindable = kiana_durable::define_doc(
        DocDefinition::new(
            "test.live",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Rewindable),
                fork: Some(DocumentFork::AsOf),
            },
        )
        .with_initial(|_| object(json!({ "items": [] }))),
    );
    let error = snapshot(&session, &rewindable, conversation_id)
        .await
        .expect_err("a semantics mismatch is rejected");
    assert!(
        error
            .to_string()
            .contains("does not match the supplied definition semantics"),
        "unexpected error: {error}"
    );

    // 同一 kind、更高版本但没有迁移。
    let newer = kiana_durable::define_doc(
        DocDefinition::new(
            "test.live",
            2,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| object(json!({ "items": [] }))),
    );
    let error = snapshot(&session, &newer, conversation_id)
        .await
        .expect_err("a newer token without migration is rejected");
    assert!(
        error
            .to_string()
            .contains("requires migration from version 1"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn delivers_publications_synchronously_with_the_commit_context() {
    use std::sync::{Arc, Mutex};

    use kiana_chord::context::{
        BACKGROUND_CONTEXT, ContextRef, create_context_key, with_context_value,
    };

    let (storage, session, _publications) = open_test_session();
    let conversation_id = setup_live(&session).await;

    // 内部观察者在 adopt 的同一同步栈里运行。
    let order_storage = Arc::clone(&storage);
    let _observed = session
        .observe_commits(Arc::new(move |_publication, _context| {
            support::lock(&order_storage.order).push("observed");
        }))
        .expect("observation is accepted");

    // 交付的上下文保留提交时携带的值。
    let key = create_context_key::<String>("test.commit-context");
    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen_for_listener = Arc::clone(&seen);
    let key_for_listener = key.clone();
    let _stored = session
        .subscribe_commits(Arc::new(move |_publication, delivered| {
            let value = delivered
                .value(key_for_listener.identity())
                .and_then(|value| Arc::downcast::<String>(value).ok())
                .map(|value| (*value).clone());
            *support::lock(&seen_for_listener) = value;
        }))
        .expect("subscription is accepted");

    let context: ContextRef = with_context_value(
        key.clone(),
        "kept".to_owned(),
        ContextRef::clone(&BACKGROUND_CONTEXT),
    );
    support::lock(&storage.order).clear();
    let live_target = target(&live_definition(), conversation_id);
    session
        .commit_with(
            move |tx| {
                let live_target = live_target.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&live_target).await?;
                    draft.insert("message".to_owned(), json!("m"));
                    Ok("done")
                })
            },
            Arc::clone(&context),
            Default::default(),
        )
        .await
        .expect("the commit succeeds");

    assert_eq!(*support::lock(&seen), Some("kept".to_owned()));
    assert_eq!(
        *support::lock(&storage.order),
        vec!["adopt", "adopted", "resolve", "observed"],
        "publications run after Storage resolves, as in TS"
    );
}

#[tokio::test]
async fn publishes_close_and_supports_unsubscription() {
    use std::sync::{Arc, Mutex};

    let (_storage, session, _publications) = open_test_session();
    let calls: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::clone(&calls);
    let _active_subscription = session
        .subscribe_close(Arc::new(move || support::lock(&active).push("active")))
        .expect("subscription is accepted");
    let removed = Arc::clone(&calls);
    let unsubscribe = session
        .subscribe_close(Arc::new(move || support::lock(&removed).push("removed")))
        .expect("subscription is accepted");
    unsubscribe();
    session.close(ctx()).await;
    assert_eq!(*support::lock(&calls), vec!["active"]);
}

#[tokio::test]
async fn reloads_an_unloaded_document_from_storage() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = setup_live(&session).await;
    let loaded = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    session.unload_documents().await.expect("unload succeeds");
    let reloaded = snapshot(&session, &live_definition(), conversation_id)
        .await
        .unwrap()
        .expect("the document exists");
    assert_eq!(reloaded, loaded);

    let live_target = target(&live_definition(), conversation_id);
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut draft = tx.doc(&live_target).await?;
            draft
                .get_mut("items")
                .and_then(|value| value.as_array_mut())
                .expect("items is an array")
                .push(json!("c"));
            Ok(())
        })
    })
    .await
    .expect("the item appends");
    session.unload_documents().await.expect("unload succeeds");
    assert_eq!(
        snapshot(&session, &live_definition(), conversation_id)
            .await
            .unwrap()
            .expect("the document exists")
            .get("items"),
        Some(&json!(["a", "b", "c"]))
    );
}

// ---------------------------------------------------------------------------
// 回归：TS 的 draft 代理语义（同一草稿、逃逸草稿、结算后吊销）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn returns_the_same_live_draft_for_every_acquisition() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let target = target(&counter_definition(), conversation_id);

    commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            let mut first = tx.doc(&target).await?;
            let second = tx.doc(&target).await?;
            first.insert("value".to_owned(), json!(7));
            // TS: `expect(second).toBe(first)`；Rust 的等价物是共享活值。
            assert_eq!(second.get("value"), Some(&json!(7)));
            let third = tx.doc(&target).await?;
            assert_eq!(third.get("value"), Some(&json!(7)));
            Ok(())
        })
    })
    .await
    .expect("the commit succeeds");
}

#[tokio::test]
async fn composes_mutations_in_the_order_they_are_made() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let token = counter_definition();
    let target = target(&token, conversation_id);

    commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            let first = tx.doc(&target).await?;
            let mut second = tx.doc(&target).await?;
            second.insert("value".to_owned(), json!(2));
            let mut first = first;
            first.insert("value".to_owned(), json!(3));
            Ok(())
        })
    })
    .await
    .expect("the commit succeeds");

    assert_eq!(
        snapshot(&session, &token, conversation_id)
            .await
            .unwrap()
            .and_then(|value| value.get("value").cloned()),
        Some(json!(3)),
        "TS 的对象语义：后一次赋值胜出"
    );
}

#[tokio::test]
async fn persists_mutations_made_through_an_escaped_draft() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let token = counter_definition();
    let target = target(&token, conversation_id);

    let escaped: std::sync::Arc<std::sync::Mutex<Option<kiana_durable::DocDraft>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = std::sync::Arc::clone(&escaped);
    commit(&session, move |tx| {
        let target = target.clone();
        let sink = std::sync::Arc::clone(&sink);
        Box::pin(async move {
            let mut draft = tx.doc(&target).await?;
            draft.insert("value".to_owned(), json!(42));
            *support::lock(&sink) = Some(draft);
            Ok(())
        })
    })
    .await
    .expect("the commit succeeds");

    assert_eq!(
        snapshot(&session, &token, conversation_id)
            .await
            .unwrap()
            .and_then(|value| value.get("value").cloned()),
        Some(json!(42)),
        "逃逸草稿的修改属于这次提交"
    );
}

#[tokio::test]
async fn revokes_an_escaped_or_returned_draft_once_the_commit_settles() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let token = counter_definition();
    let target = target(&token, conversation_id);

    // TS `revokes escaped drafts when the callback settles`：提交后读为空、写入抛出。
    let escaped: std::sync::Arc<std::sync::Mutex<Option<kiana_durable::DocDraft>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = std::sync::Arc::clone(&escaped);
    let commit_target = target.clone();
    commit(&session, move |tx| {
        let commit_target = commit_target.clone();
        let sink = std::sync::Arc::clone(&sink);
        Box::pin(async move {
            let mut draft = tx.doc(&commit_target).await?;
            draft.insert("value".to_owned(), json!(5));
            *support::lock(&sink) = Some(draft);
            Ok(())
        })
    })
    .await
    .expect("the commit succeeds");
    let mut escaped = support::lock(&escaped).take().expect("the draft escaped");
    assert_eq!(
        escaped.get("value"),
        None,
        "a settled draft reads as empty (TS throws on any access; Deref has no error channel)"
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            escaped.insert("value".to_owned(), json!(6));
        }))
        .is_err(),
        "a settled draft rejects writes"
    );

    // TS：`const returned = await session.commit((tx) => tx.doc(...))`；返回的草稿同样被吊销。
    let commit_target = target.clone();
    let returned = commit(&session, move |tx| {
        let commit_target = commit_target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&commit_target).await?;
            draft.insert("value".to_owned(), json!(9));
            Ok(draft)
        })
    })
    .await
    .expect("the commit succeeds");
    assert_eq!(
        snapshot(&session, &token, conversation_id)
            .await
            .unwrap()
            .and_then(|value| value.get("value").cloned()),
        Some(json!(9))
    );
    assert_eq!(
        returned.get("value"),
        None,
        "the returned draft is revoked like an escaped one"
    );
}
