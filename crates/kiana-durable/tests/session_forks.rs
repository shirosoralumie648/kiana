//! `test/session-forks.test.ts` 的端口（节选）。

mod support;

use std::sync::{Arc, Mutex};

use kiana_chord::context::Context;
use kiana_durable::{
    BoxError, ConversationId, ConversationOwnership, CreateConversationOptions, DocDefinition,
    DocFamilyToken, DocTarget, DocToken, DocumentCommitChange, DocumentFork, DocumentHistory,
    DocumentSemantics, EntryId, JsonObject, ScopeKind, Storage, StorageWrite, Tx,
};
use serde_json::json;
use support::*;

fn ctx() -> &'static dyn Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

fn policy_doc(kind: &str, history: DocumentHistory, fork: DocumentFork, initial: &str) -> DocToken {
    let initial = initial.to_owned();
    kiana_durable::define_doc(
        DocDefinition::new(
            kind,
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(history),
                fork: Some(fork),
            },
        )
        .with_initial(move |_| text_object(&initial)),
    )
}

fn policy_family(kind: &str, history: DocumentHistory, fork: DocumentFork) -> DocFamilyToken {
    kiana_durable::define_doc_family(
        DocDefinition::family(
            kind,
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(history),
                fork: Some(fork),
            },
        )
        .with_initial(|seed| {
            let value = seed
                .and_then(|seed| seed.as_str())
                .unwrap_or("unused")
                .to_owned();
            text_object(&value)
        }),
    )
}

fn text_object(text: &str) -> JsonObject {
    let mut value = JsonObject::new();
    value.insert("value".to_owned(), json!(text));
    value
}

fn family_target(token: &DocFamilyToken, conversation_id: ConversationId, key: &str) -> DocTarget {
    DocTarget::conversation_family(token, conversation_id, key, json!("unused"))
}

fn text_value(document: Option<JsonObject>) -> Option<String> {
    document.and_then(|document| {
        document
            .get("value")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    })
}

fn document_copies(writes: &[StorageWrite]) -> Vec<StorageWrite> {
    writes
        .iter()
        .filter(|write| matches!(write, StorageWrite::DocumentCopy { .. }))
        .cloned()
        .collect()
}

fn copy_changes(publication: &kiana_durable::CommitPublication) -> Vec<DocumentCommitChange> {
    document_copy_changes(publication)
}

fn fork_options() -> CreateConversationOptions {
    CreateConversationOptions {
        ownership: ConversationOwnership::Ownerless,
    }
}

#[tokio::test]
async fn copies_as_of_and_current_singleton_and_family_bases_leaving_initial_documents_absent() {
    let as_of = policy_doc(
        "fork.policies.as-of",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
        "as-initial",
    );
    let current = policy_doc(
        "fork.policies.current",
        DocumentHistory::Latest,
        DocumentFork::Current,
        "current-initial",
    );
    let initial = policy_doc(
        "fork.policies.initial",
        DocumentHistory::Latest,
        DocumentFork::Initial,
        "fresh",
    );
    let as_of_family = policy_family(
        "fork.policies.as-of-family",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
    );
    let current_family = policy_family(
        "fork.policies.current-family",
        DocumentHistory::Latest,
        DocumentFork::Current,
    );
    let (storage, session, publications) = open_test_session();
    let parent_id = create_conversation(&session).await;

    let as_of_target = target(&as_of, parent_id);
    let current_target = target(&current, parent_id);
    let initial_target = target(&initial, parent_id);
    let as_of_a = family_target(&as_of_family, parent_id, "a");
    let as_of_b = family_target(&as_of_family, parent_id, "b");
    let current_a = family_target(&current_family, parent_id, "a");
    let fork_at = commit(&session, move |tx| {
        let as_of_target = as_of_target.clone();
        let current_target = current_target.clone();
        let initial_target = initial_target.clone();
        let as_of_a = as_of_a.clone();
        let as_of_b = as_of_b.clone();
        let current_a = current_a.clone();
        Box::pin(async move {
            let fork_at = tx
                .append_entry(parent_id, entry_draft("fork-point", None))
                .await?
                .id;
            tx.doc(&as_of_target)
                .await?
                .insert("value".to_owned(), json!("as-at-fork"));
            tx.doc(&current_target)
                .await?
                .insert("value".to_owned(), json!("current-at-fork"));
            tx.doc(&initial_target)
                .await?
                .insert("value".to_owned(), json!("parent-only"));
            tx.doc(&as_of_a)
                .await?
                .insert("value".to_owned(), json!("family-as-a"));
            tx.doc(&as_of_b)
                .await?
                .insert("value".to_owned(), json!("family-as-b"));
            tx.doc(&current_a)
                .await?
                .insert("value".to_owned(), json!("family-current-a"));
            Ok(fork_at)
        })
    })
    .await
    .expect("the fork point commits");

    let as_of_target = target(&as_of, parent_id);
    let current_target = target(&current, parent_id);
    let as_of_a = family_target(&as_of_family, parent_id, "a");
    let current_a = family_target(&current_family, parent_id, "a");
    commit(&session, move |tx| {
        let as_of_target = as_of_target.clone();
        let current_target = current_target.clone();
        let as_of_a = as_of_a.clone();
        let current_a = current_a.clone();
        Box::pin(async move {
            tx.doc(&as_of_target)
                .await?
                .insert("value".to_owned(), json!("as-after-fork"));
            tx.doc(&current_target)
                .await?
                .insert("value".to_owned(), json!("current-when-copied"));
            tx.doc(&as_of_a)
                .await?
                .insert("value".to_owned(), json!("family-as-after"));
            tx.doc(&current_a)
                .await?
                .insert("value".to_owned(), json!("family-current-when-copied"));
            Ok(())
        })
    })
    .await
    .expect("the later changes commit");

    let document_reads = storage
        .document_read_count
        .load(std::sync::atomic::Ordering::SeqCst);
    let child = commit(&session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(parent_id, fork_at, &fork_options())
                .await
        })
    })
    .await
    .expect("the fork commits");
    assert_eq!(
        storage
            .document_read_count
            .load(std::sync::atomic::Ordering::SeqCst),
        document_reads
    );

    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&as_of, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("as-at-fork")
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&current, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("current-when-copied")
    );
    assert!(
        snapshot_target(&session, &target(&initial, child.id))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &family_target(&as_of_family, child.id, "a"))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("family-as-a")
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &family_target(&as_of_family, child.id, "b"))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("family-as-b")
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &family_target(&current_family, child.id, "a"))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("family-current-when-copied")
    );

    let admitted = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the fork reached storage");
    let copies = document_copies(&admitted);
    assert_eq!(copies.len(), 5);
    for copy in &copies {
        let StorageWrite::DocumentCopy { record, .. } = copy else {
            unreachable!()
        };
        assert!(
            matches!(record.scope, kiana_durable::DocumentScope::Conversation { conversation_id } if conversation_id == child.id)
        );
    }

    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the fork published");
    let copied = copy_changes(&publication);
    assert_eq!(copied.len(), 5);
    for change in &copied {
        let DocumentCommitChange::DocumentCopy {
            record,
            conversation_id,
            source,
        } = change
        else {
            panic!("expected a document copy");
        };
        assert_eq!(record.created_at, publication.seq);
        assert_eq!(*conversation_id, child.id);
        let write = copies
            .iter()
            .find_map(|candidate| match candidate {
                StorageWrite::DocumentCopy {
                    record: written,
                    source: written_source,
                } if written.id == record.id => Some(written_source),
                _ => None,
            })
            .expect("the copy has its write");
        assert_eq!(write, source);
    }

    let parent_record = storage
        .find_document(
            &target(&as_of, parent_id).address(),
            kiana_durable::DocumentPoint::Current,
            ctx(),
        )
        .await
        .unwrap()
        .unwrap();
    let child_record = storage
        .find_document(
            &target(&as_of, child.id).address(),
            kiana_durable::DocumentPoint::Current,
            ctx(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_ne!(child_record.id, parent_record.id);

    let child_as_of = target(&as_of, child.id);
    commit(&session, move |tx| {
        let child_as_of = child_as_of.clone();
        Box::pin(async move {
            tx.doc(&child_as_of)
                .await?
                .insert("value".to_owned(), json!("child-independent"));
            Ok(())
        })
    })
    .await
    .expect("the child document changes independently");
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&as_of, parent_id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("as-after-fork")
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&as_of, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("child-independent")
    );

    let child_initial = target(&initial, child.id);
    commit(&session, move |tx| {
        let child_initial = child_initial.clone();
        Box::pin(async move {
            tx.doc(&child_initial)
                .await?
                .insert("value".to_owned(), json!("child-created"));
            Ok(())
        })
    })
    .await
    .expect("the initial-policy document is created in the child");
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&initial, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("child-created")
    );
}

#[tokio::test]
async fn uses_final_document_state_from_the_fork_entry_commit_excluding_later_same_commit_entries()
{
    let doc = policy_doc(
        "fork.same-commit",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
        "initial",
    );
    let (_storage, session, _publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let doc_target = target(&doc, parent_id);
    let (fork_at, excluded) = commit(&session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            let fork_at = tx
                .append_entry(parent_id, entry_draft("included", None))
                .await?
                .id;
            tx.doc(&doc_target)
                .await?
                .insert("value".to_owned(), json!("final-state-of-commit"));
            let excluded = tx
                .append_entry(parent_id, entry_draft("excluded", None))
                .await?
                .id;
            Ok((fork_at, excluded))
        })
    })
    .await
    .expect("the parent commits");
    let child = commit(&session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(parent_id, fork_at, &fork_options())
                .await
        })
    })
    .await
    .expect("the fork commits");
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&doc, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("final-state-of-commit")
    );
    let visible = commit(&session, move |tx| {
        Box::pin(async move {
            tx.scan_entries(
                &kiana_durable::EntryQuery {
                    conversation_id: child.id,
                    min_entry_id: None,
                    max_entry_id: None,
                    order: None,
                },
                10,
                None,
            )
            .await
        })
    })
    .await
    .expect("the child entries scan");
    let ids: Vec<EntryId> = visible.items.iter().map(|entry| entry.id).collect();
    assert!(ids.contains(&fork_at));
    assert!(!ids.contains(&excluded));
}

#[tokio::test]
async fn rejects_invisible_fork_points_before_admission_and_remains_usable() {
    let (storage, session, publications) = open_test_session();
    let root_id = create_conversation(&session).await;
    let (visible, hidden) = commit(&session, move |tx| {
        Box::pin(async move {
            let visible = tx
                .append_entry(root_id, entry_draft("visible", None))
                .await?
                .id;
            let hidden = tx
                .append_entry(root_id, entry_draft("hidden", None))
                .await?
                .id;
            Ok((visible, hidden))
        })
    })
    .await
    .expect("the root entries commit");
    let parent = commit(&session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(root_id, visible, &fork_options())
                .await
        })
    })
    .await
    .expect("the first fork commits");

    let commits = support::lock(&storage.commits).len();
    let published = support::lock(&publications).len();
    let error = commit(&session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(parent.id, hidden, &fork_options())
                .await
        })
    })
    .await
    .expect_err("an invisible fork point is rejected");
    assert!(
        error
            .to_string()
            .contains(&format!("Entry {hidden} is not visible"))
    );
    assert_eq!(support::lock(&storage.commits).len(), commits);
    assert_eq!(support::lock(&publications).len(), published);

    let independent = commit(&session, |tx| {
        Box::pin(async move {
            tx.create_conversation(&CreateConversationOptions {
                ownership: ConversationOwnership::Ownerless,
            })
            .await
        })
    })
    .await
    .expect("the Session remains usable");
    assert_eq!(
        storage.conversation(independent.id, ctx()).await.unwrap(),
        Some(independent)
    );
}

#[tokio::test]
async fn rejects_duplicate_as_of_and_current_selections_for_one_child_address_before_admission() {
    let as_of = policy_doc(
        "fork.duplicate-policy",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
        "as-of",
    );
    let current = policy_doc(
        "fork.duplicate-policy",
        DocumentHistory::Latest,
        DocumentFork::Current,
        "current",
    );
    let (storage, session, _publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let as_of_target = target(&as_of, parent_id);
    let old_at = commit(&session, move |tx| {
        let as_of_target = as_of_target.clone();
        Box::pin(async move {
            let old_at = tx
                .append_entry(parent_id, entry_draft("old", None))
                .await?
                .id;
            tx.doc(&as_of_target).await?;
            Ok(old_at)
        })
    })
    .await
    .expect("the old incarnation commits");
    let as_of_target = target(&as_of, parent_id);
    commit(&session, move |tx| {
        let as_of_target = as_of_target.clone();
        Box::pin(async move {
            tx.retire_doc(&as_of_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the old incarnation retires");
    let current_target = target(&current, parent_id);
    commit(&session, move |tx| {
        let current_target = current_target.clone();
        Box::pin(async move {
            tx.doc(&current_target).await?;
            Ok(())
        })
    })
    .await
    .expect("the current incarnation commits");

    let commits = support::lock(&storage.commits).len();
    let error = commit(&session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(parent_id, old_at, &fork_options())
                .await
        })
    })
    .await
    .expect_err("two source selections for one child address are rejected");
    assert!(
        error
            .to_string()
            .contains("Fork selects multiple source documents")
    );
    assert_eq!(support::lock(&storage.commits).len(), commits);

    let independent = commit(&session, |tx| {
        Box::pin(async move {
            tx.create_conversation(&CreateConversationOptions {
                ownership: ConversationOwnership::Ownerless,
            })
            .await
        })
    })
    .await
    .expect("the Session remains usable");
    assert!(
        storage
            .conversation(independent.id, ctx())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn rejects_current_and_as_of_source_writes_in_the_fork_transaction() {
    let current = policy_doc(
        "fork.same-transaction-current",
        DocumentHistory::Latest,
        DocumentFork::Current,
        "committed",
    );
    let as_of = policy_doc(
        "fork.same-transaction-as-of",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
        "committed",
    );
    let (storage, session, _publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let current_target = target(&current, parent_id);
    let as_of_target = target(&as_of, parent_id);
    let fork_at = commit(&session, move |tx| {
        let current_target = current_target.clone();
        let as_of_target = as_of_target.clone();
        Box::pin(async move {
            let fork_at = tx
                .append_entry(parent_id, entry_draft("point", None))
                .await?
                .id;
            tx.doc(&current_target).await?;
            tx.doc(&as_of_target).await?;
            Ok(fork_at)
        })
    })
    .await
    .expect("the parent commits");
    let commits = support::lock(&storage.commits).len();

    let current_first = target(&current, parent_id);
    let error = commit(&session, move |tx| {
        let current_first = current_first.clone();
        Box::pin(async move {
            tx.doc(&current_first)
                .await?
                .insert("value".to_owned(), json!("before-fork"));
            tx.fork_conversation(parent_id, fork_at, &fork_options())
                .await?;
            Ok(())
        })
    })
    .await
    .expect_err("a source write before the fork is rejected");
    assert!(
        error
            .to_string()
            .contains("Cannot change fork source document")
    );

    let current_after = target(&current, parent_id);
    let error = commit(&session, move |tx| {
        let current_after = current_after.clone();
        Box::pin(async move {
            tx.fork_conversation(parent_id, fork_at, &fork_options())
                .await?;
            tx.doc(&current_after)
                .await?
                .insert("value".to_owned(), json!("after-fork"));
            Ok(())
        })
    })
    .await
    .expect_err("a source write after the fork is rejected");
    assert!(
        error
            .to_string()
            .contains("Cannot change fork source document")
    );

    let as_of_first = target(&as_of, parent_id);
    let error = commit(&session, move |tx| {
        let as_of_first = as_of_first.clone();
        Box::pin(async move {
            tx.doc(&as_of_first)
                .await?
                .insert("value".to_owned(), json!("as-of-write"));
            tx.fork_conversation(parent_id, fork_at, &fork_options())
                .await?;
            Ok(())
        })
    })
    .await
    .expect_err("an as-of source write is rejected");
    assert!(
        error
            .to_string()
            .contains("Cannot change fork source document")
    );
    assert_eq!(support::lock(&storage.commits).len(), commits);
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&current, parent_id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("committed")
    );

    let child = commit(&session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(parent_id, fork_at, &fork_options())
                .await
        })
    })
    .await
    .expect("the fork commits");
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&current, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("committed")
    );
}

#[tokio::test]
async fn copies_only_conversation_documents_leaving_session_and_task_documents_in_their_scopes() {
    let copied = policy_doc(
        "fork.scope.conversation",
        DocumentHistory::Latest,
        DocumentFork::Current,
        "conversation",
    );
    let session_only = kiana_durable::define_doc(
        DocDefinition::new("fork.scope.session", 1, DocumentSemantics::SESSION)
            .with_initial(|_| text_object("session")),
    );
    let task_only = kiana_durable::define_doc(
        DocDefinition::new("fork.scope.task", 1, DocumentSemantics::TASK)
            .with_initial(|_| text_object("task")),
    );
    let work = work_task("fork.scope.work", 1);
    let (storage, session, publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let copied_target = target(&copied, parent_id);
    let work_for_commit = work.clone();
    let session_only_for_commit = session_only.clone();
    let task_only_for_commit = task_only.clone();
    let (fork_at, task_id) = commit(&session, move |tx| {
        let copied_target = copied_target.clone();
        let work = work_for_commit.clone();
        let session_only = session_only_for_commit.clone();
        let task_only = task_only_for_commit.clone();
        Box::pin(async move {
            let fork_at = tx
                .append_entry(parent_id, entry_draft("point", None))
                .await?
                .id;
            let task_id = tx
                .create_task(
                    &work,
                    json!(null),
                    conversation_task_options(parent_id, false),
                )
                .await?;
            tx.doc(&copied_target).await?;
            tx.doc(&DocTarget::session(&session_only)).await?;
            tx.doc(&DocTarget::task(&task_only, task_id)).await?;
            Ok((fork_at, task_id))
        })
    })
    .await
    .expect("the parent commits");

    let child = commit(&session, move |tx| {
        Box::pin(async move {
            tx.fork_conversation(parent_id, fork_at, &fork_options())
                .await
        })
    })
    .await
    .expect("the fork commits");

    let admitted = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the fork reached storage");
    let copies = document_copies(&admitted);
    assert_eq!(copies.len(), 1);
    match &copies[0] {
        StorageWrite::DocumentCopy { record, .. } => {
            assert_eq!(record.kind, "fork.scope.conversation")
        }
        _ => unreachable!(),
    }
    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the fork published");
    let copied_changes = copy_changes(&publication);
    assert_eq!(copied_changes.len(), 1);
    match &copied_changes[0] {
        DocumentCommitChange::DocumentCopy { record, .. } => {
            assert_eq!(record.kind, "fork.scope.conversation")
        }
        _ => panic!("expected a document copy"),
    }
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&copied, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("conversation")
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &DocTarget::session(&session_only))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("session")
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &DocTarget::task(&task_only, task_id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("task")
    );
}

#[allow(dead_code)]
fn _box_error_used(error: BoxError) -> BoxError {
    error
}

#[tokio::test]
async fn fails_the_session_on_a_rejected_fork_commit_which_leaves_no_effect() {
    use kiana_durable::{Session, SessionEnd};

    let doc = policy_doc(
        "fork.storage-rejected",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
        "source",
    );
    let (storage, session, _publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let doc_target = target(&doc, parent_id);
    let fork_at = commit(&session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            let fork_at = tx
                .append_entry(parent_id, entry_draft("point", None))
                .await?
                .id;
            tx.doc(&doc_target).await?;
            Ok(fork_at)
        })
    })
    .await
    .expect("the parent commit succeeds");

    let captured: Arc<Mutex<Option<ConversationId>>> = Arc::new(Mutex::new(None));
    let captured_for_commit = Arc::clone(&captured);
    storage.fail_next_commit(std::io::Error::other("copy rejected").into());
    let error = commit::<(), _>(&session, move |tx| {
        let captured = Arc::clone(&captured_for_commit);
        Box::pin(async move {
            let child = tx
                .fork_conversation(parent_id, fork_at, &fork_options())
                .await?;
            *support::lock(&captured) = Some(child.id);
            Ok(())
        })
    })
    .await
    .expect_err("the fork commit is rejected");
    assert_eq!(error.to_string(), "copy rejected");
    let error = commit::<(), _>(&session, |_tx| Box::pin(async { Ok(()) }))
        .await
        .expect_err("the failed Session rejects later commits");
    let failed = error
        .downcast_ref::<kiana_durable::SessionFailed>()
        .expect("later commits fail with SessionFailed");
    assert_eq!(failed.cause.to_string(), "copy rejected");
    match session.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "copy rejected"),
        SessionEnd::Closed => panic!("a rejected commit must fail the Session"),
    }
    storage.reopen();
    let child_id = support::lock(&captured).expect("the child id was staged");
    assert!(
        storage
            .conversation(child_id, ctx())
            .await
            .unwrap()
            .is_none(),
        "nothing of the rejected batch reached Storage"
    );
}

#[tokio::test]
async fn retires_a_copied_document_and_can_recreate_the_address_in_the_fork_transaction() {
    let doc = policy_doc(
        "fork.retire-recreate",
        DocumentHistory::Rewindable,
        DocumentFork::AsOf,
        "fresh",
    );
    let (storage, session, _publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let doc_target = target(&doc, parent_id);
    let fork_at = commit(&session, move |tx| {
        let doc_target = doc_target.clone();
        Box::pin(async move {
            let fork_at = tx
                .append_entry(parent_id, entry_draft("point", None))
                .await?
                .id;
            if let Some(value) = tx.doc(&doc_target).await?.get_mut("value") {
                *value = json!("copied");
            }
            Ok(fork_at)
        })
    })
    .await
    .expect("the parent commit succeeds");

    let doc_for_fork = doc.clone();
    let child = commit(&session, move |tx| {
        let doc_for_fork = doc_for_fork.clone();
        Box::pin(async move {
            let created = tx
                .fork_conversation(parent_id, fork_at, &fork_options())
                .await?;
            let child_target = target(&doc_for_fork, created.id);
            tx.retire_doc(&child_target).await?;
            let mut replacement = tx.doc(&child_target).await?;
            if let Some(value) = replacement.get_mut("value") {
                *value = json!("replacement");
            }
            Ok(created)
        })
    })
    .await
    .expect("the fork transaction commits");

    let writes = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the fork reached storage");
    assert_eq!(
        writes
            .iter()
            .filter(|write| matches!(write, StorageWrite::DocumentCopy { .. }))
            .count(),
        1
    );
    assert_eq!(
        writes
            .iter()
            .filter(|write| matches!(write, StorageWrite::DocumentCreate { .. }))
            .count(),
        1
    );
    assert_eq!(
        writes
            .iter()
            .filter(|write| matches!(write, StorageWrite::DocumentRetire { .. }))
            .count(),
        1
    );
    assert_eq!(
        text_value(
            snapshot_target(&session, &target(&doc, child.id))
                .await
                .unwrap()
        )
        .as_deref(),
        Some("replacement")
    );
}
