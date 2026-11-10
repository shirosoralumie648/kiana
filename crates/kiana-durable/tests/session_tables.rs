//! `test/session-tables.test.ts` 的端口（节选）。

mod support;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use kiana_durable::{
    CommitChange, ConversationId, ConversationOwnership, ConversationQuery, DocTarget, EntryQuery,
    ReadAfterWrite, Storage, TableCommitChange, TaskId, TaskOwnership, TaskQuery, TaskState, Tx,
};
use serde_json::json;
use support::*;

fn table_change_is(change: &CommitChange, kind: &str) -> bool {
    matches!(
        (change, kind),
        (
            CommitChange::Table(TableCommitChange::Conversation { .. }),
            "conversation"
        ) | (
            CommitChange::Table(TableCommitChange::Entry { .. }),
            "entry"
        ) | (CommitChange::Table(TableCommitChange::Task { .. }), "task")
            | (
                CommitChange::Table(TableCommitChange::Submission { .. }),
                "submission"
            )
    )
}

fn ctx() -> &'static dyn kiana_chord::context::Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

fn is_read_after_write(error: &kiana_durable::BoxError) -> bool {
    error.downcast_ref::<ReadAfterWrite>().is_some()
}

fn terminal_state() -> TaskState {
    TaskState::Terminal {
        outcome: kiana_durable::TaskOutcome::Completed {
            result: json!({ "ok": true }),
        },
    }
}

#[tokio::test]
async fn allows_table_reads_only_before_the_first_table_write() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let notes = notes_definition("test.notes");

    let notes_target = target(&notes, conversation_id);
    commit(&session, move |tx| {
        let notes_target = notes_target.clone();
        Box::pin(async move {
            let conversation = tx
                .conversation(conversation_id)
                .await?
                .expect("the conversation exists");
            assert_eq!(conversation.id, conversation_id);
            let page = tx
                .scan_conversations(
                    &ConversationQuery {
                        owner_conversation_id: None,
                        owner_task_id: None,
                        order: None,
                    },
                    1,
                    None,
                )
                .await?;
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].id, conversation_id);
            let tasks = tx
                .scan_tasks(
                    &TaskQuery {
                        conversation_id: Some(conversation_id),
                        ..TaskQuery::default()
                    },
                    10,
                    None,
                )
                .await?;
            assert!(tasks.items.is_empty());
            let entries = tx
                .scan_entries(
                    &EntryQuery {
                        conversation_id,
                        min_entry_id: None,
                        max_entry_id: None,
                        order: None,
                    },
                    10,
                    None,
                )
                .await?;
            assert!(entries.items.is_empty());

            tx.append_entry(conversation_id, entry_draft("note", None))
                .await?;
            let error = tx
                .conversation(conversation_id)
                .await
                .expect_err("conversation reads are rejected after a write");
            assert!(is_read_after_write(&error));
            let error = tx
                .task(kiana_durable::id_from_number::<kiana_durable::TaskBrand>(1))
                .await
                .expect_err("task reads are rejected after a write");
            assert_eq!(
                error.to_string(),
                "Tx.task() cannot read tables after the first table write"
            );
            let error = tx
                .entry(kiana_durable::id_from_number::<kiana_durable::EntryBrand>(
                    1,
                ))
                .await
                .expect_err("entry reads are rejected after a write");
            assert!(is_read_after_write(&error));
            let error = tx
                .scan_conversations(
                    &ConversationQuery {
                        owner_conversation_id: None,
                        owner_task_id: None,
                        order: None,
                    },
                    10,
                    None,
                )
                .await
                .expect_err("conversation scans are rejected after a write");
            assert!(is_read_after_write(&error));
            let error = tx
                .scan_entries(
                    &EntryQuery {
                        conversation_id,
                        min_entry_id: None,
                        max_entry_id: None,
                        order: None,
                    },
                    10,
                    None,
                )
                .await
                .expect_err("entry scans are rejected after a write");
            assert!(is_read_after_write(&error));

            let mut draft = tx.doc(&notes_target).await?;
            draft.insert("text".to_owned(), json!("after write"));
            Ok(())
        })
    })
    .await
    .expect("the commit succeeds");

    assert_eq!(
        snapshot(&session, &notes, conversation_id)
            .await
            .unwrap()
            .unwrap()
            .get("text"),
        Some(&json!("after write"))
    );
}

#[tokio::test]
async fn passes_caller_selected_limits_and_cursors_through_table_scans() {
    let (_storage, session, _publications) = open_test_session();
    let ids = [
        create_conversation(&session).await,
        create_conversation(&session).await,
        create_conversation(&session).await,
    ];
    commit(&session, move |tx| {
        Box::pin(async move {
            let query = ConversationQuery {
                owner_conversation_id: None,
                owner_task_id: None,
                order: None,
            };
            let first = tx.scan_conversations(&query, 2, None).await?;
            assert_eq!(first.items.len(), 2);
            assert_eq!(first.items[0].id, ids[0]);
            assert_eq!(first.items[1].id, ids[1]);
            let next = first.next.expect("the first page has a cursor");
            let second = tx.scan_conversations(&query, 2, Some(&next)).await?;
            assert_eq!(second.items.len(), 1);
            assert_eq!(second.items[0].id, ids[2]);
            assert!(second.next.is_none());
            Ok(())
        })
    })
    .await
    .expect("scans page through the caller limits and cursors");
}

#[tokio::test]
async fn treats_synchronous_set_task_as_the_first_table_write() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let task = work_task("test.work", 1);
    let task_id = create_task(&session, &task, conversation_id, None)
        .await
        .expect("task creation succeeds");

    commit(&session, move |tx| {
        Box::pin(async move {
            let record = tx.task(task_id).await?.expect("the task exists");
            tx.set_task(record)?;
            let error = tx
                .task(task_id)
                .await
                .expect_err("setTask counts as the first table write");
            assert!(is_read_after_write(&error));
            Ok(())
        })
    })
    .await
    .expect("setTask marks the table as written");
}

#[tokio::test]
async fn creates_conversations_entries_and_tasks_with_minted_ids() {
    let (storage, session, publications) = open_test_session();
    let work = work_task("test.work", 1);

    let work_for_commit = work.clone();
    let created = commit(&session, move |tx| {
        let work = work_for_commit.clone();
        Box::pin(async move {
            let conversation = tx
                .create_conversation(&kiana_durable::CreateConversationOptions {
                    ownership: ConversationOwnership::Ownerless,
                })
                .await?;
            let first = tx
                .append_entry(conversation.id, entry_draft("note", Some(json!("one"))))
                .await?;
            let headed = tx
                .append_entry(
                    conversation.id,
                    kiana_durable::EntryDraft {
                        key_order: None,
                        kind: "summary".to_owned(),
                        model: None,
                        data: None,
                        head: Some(kiana_durable::EntryHead::SelfHead),
                        edits: None,
                    },
                )
                .await?;
            let task = tx
                .create_task(
                    &work,
                    json!({ "path": "x" }),
                    conversation_task_options(conversation.id, true),
                )
                .await?;
            Ok((conversation, first, headed, task))
        })
    })
    .await
    .expect("the commit succeeds");

    let (conversation, first, headed, task_id) = created;
    let ids = [
        conversation.id.get(),
        first.id.get(),
        headed.id.get(),
        task_id.get(),
    ];
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 4);
    assert_eq!(headed.head, Some(headed.id));
    assert_eq!(first.kind, "note");
    assert_eq!(first.data, Some(json!("one")));
    assert_eq!(first.conversation_id, conversation.id);

    let stored_entry = storage
        .entry(headed.id, ctx())
        .await
        .unwrap()
        .expect("the headed entry is stored");
    assert_eq!(stored_entry.entry, headed);

    let stored_task = storage
        .task(task_id, ctx())
        .await
        .unwrap()
        .expect("the task is stored");
    assert_eq!(
        serde_json::to_value(&stored_task).unwrap(),
        json!({
            "id": task_id.get(),
            "conversationId": conversation.id.get(),
            "kind": "test.work",
            "version": 1,
            "input": { "path": "x" },
            "background": true,
            "abortRequested": false,
            "state": { "status": "pending", "checkpoint": { "phase": "start" } },
        })
    );

    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the commit published");
    assert_eq!(publication.changes.len(), 4);
    let admitted = support::lock(&storage.admitted_commits)
        .last()
        .cloned()
        .expect("the commit reached storage");
    let mut kinds = Vec::new();
    for change in &publication.changes {
        for name in ["conversation", "entry", "task"] {
            if table_change_is(change, name) {
                kinds.push(name.to_owned());
            }
        }
        assert!(match change {
            CommitChange::Table(TableCommitChange::Conversation { value }) => {
                admitted.iter().any(|write| {
                    matches!(write, kiana_durable::StorageWrite::Conversation { value: written } if written == value)
                })
            }
            CommitChange::Table(TableCommitChange::Entry { value }) => admitted.iter().any(|write| {
                matches!(write, kiana_durable::StorageWrite::Entry { value: written } if written == value)
            }),
            CommitChange::Table(TableCommitChange::Task { value }) => admitted.iter().any(|write| {
                matches!(write, kiana_durable::StorageWrite::Task { value: written } if written == value)
            }),
            _ => false,
        });
    }
    kinds.sort();
    assert_eq!(kinds, ["conversation", "entry", "entry", "task"]);

    let work_for_error = work.clone();
    let error = commit(&session, move |tx| {
        let work = work_for_error.clone();
        Box::pin(async move {
            tx.create_task(
                &work,
                json!({ "path": "x" }),
                kiana_durable::TaskOptions::new(TaskOwnership::Conversation),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .expect_err("conversation-owned tasks require a conversation");
    assert!(
        error
            .to_string()
            .contains("requires options.conversationId")
    );

    let error = commit(&session, move |tx| {
        Box::pin(async move {
            tx.append_entry(
                kiana_durable::id_from_number::<kiana_durable::ConversationBrand>(12345),
                entry_draft("note", None),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .expect_err("appending to a missing conversation fails");
    assert_eq!(error.to_string(), "Conversation 12345 does not exist");
}

#[tokio::test]
async fn creates_a_conversation_with_an_explicitly_staged_task_owner() {
    let (storage, session, _publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let work = work_task("test.work", 1);

    let work_for_commit = work.clone();
    let created = commit(&session, move |tx| {
        let work = work_for_commit.clone();
        Box::pin(async move {
            let supervisor_id = tx
                .create_task(
                    &work,
                    json!({ "path": "background" }),
                    conversation_task_options(parent_id, true),
                )
                .await?;
            let child = tx
                .create_conversation(&kiana_durable::CreateConversationOptions {
                    ownership: ConversationOwnership::Task {
                        task_id: supervisor_id,
                    },
                })
                .await?;
            Ok((supervisor_id, child))
        })
    })
    .await
    .expect("the commit succeeds");

    let (supervisor_id, child) = created;
    assert_eq!(
        child.owner,
        Some(kiana_durable::ConversationOwner {
            conversation_id: parent_id,
            task_id: supervisor_id,
        })
    );
    assert_eq!(
        storage.conversation(child.id, ctx()).await.unwrap(),
        Some(child)
    );

    let error = commit(&session, move |tx| {
        Box::pin(async move {
            let supervisor = tx.task(supervisor_id).await?.expect("the task exists");
            tx.set_task(kiana_durable::TaskRecord {
                conversation_id: kiana_durable::id_from_number::<kiana_durable::ConversationBrand>(
                    child_id_for(&supervisor.conversation_id),
                ),
                ..supervisor
            })?;
            Ok(())
        })
    })
    .await
    .expect_err("a task cannot change conversations");
    assert!(error.to_string().contains("cannot change conversations"));
    assert_eq!(
        storage
            .task(supervisor_id, ctx())
            .await
            .unwrap()
            .unwrap()
            .conversation_id,
        parent_id
    );
}

// 读取一个与父会话不同的会话 ID，用于构造“改会话”的非法记录。
fn child_id_for(parent: &ConversationId) -> i64 {
    parent.get() + 1
}

#[tokio::test]
async fn rejects_missing_terminal_and_abort_marked_conversation_owners_atomically() {
    let (storage, session, _publications) = open_test_session();
    let parent_id = create_conversation(&session).await;
    let work = work_task("test.work", 1);

    let work_for_move = work.clone();
    let moved_task = std::sync::Arc::new(std::sync::Mutex::new(None::<TaskId>));
    let moved_slot = std::sync::Arc::clone(&moved_task);
    let error = commit(&session, move |tx| {
        let work = work_for_move.clone();
        let moved_slot = std::sync::Arc::clone(&moved_slot);
        Box::pin(async move {
            let task_id = tx
                .create_task(
                    &work,
                    json!({ "path": "move" }),
                    conversation_task_options(parent_id, false),
                )
                .await?;
            *support::lock(&moved_slot) = Some(task_id);
            tx.set_task(kiana_durable::TaskRecord {
                id: task_id,
                conversation_id: kiana_durable::id_from_number(998),
                kind: "test.work".to_owned(),
                version: 1,
                input: json!({ "path": "move" }),
                trace: None,
                links: None,
                owner: None,
                background: false,
                abort_requested: false,
                abandon_on_restart: None,
                state: TaskState::Pending {
                    checkpoint: json!({ "phase": "start" }),
                },
                started_at: None,
                ended_at: None,
                restarts: None,
                abort_reason: None,
                memos: None,
            })?;
            Ok(())
        })
    })
    .await
    .expect_err("moving a staged task is rejected");
    assert!(error.to_string().contains("cannot change conversations"));
    let moved_id = support::lock(&moved_task).expect("the staged task ID was recorded");
    assert!(storage.task(moved_id, ctx()).await.unwrap().is_none());

    let error = commit(&session, move |tx| {
        Box::pin(async move {
            tx.create_conversation(&kiana_durable::CreateConversationOptions {
                ownership: ConversationOwnership::Task {
                    task_id: kiana_durable::id_from_number(999),
                },
            })
            .await?;
            Ok(())
        })
    })
    .await
    .expect_err("a missing owner task is rejected");
    assert_eq!(
        error.to_string(),
        "Conversation owner task 999 does not exist"
    );

    let work_for_abort = work.clone();
    let rejected_child = std::sync::Arc::new(std::sync::Mutex::new(None::<ConversationId>));
    let rejected_slot = std::sync::Arc::clone(&rejected_child);
    let error = commit(&session, move |tx| {
        let work = work_for_abort.clone();
        let rejected_slot = std::sync::Arc::clone(&rejected_slot);
        Box::pin(async move {
            let supervisor_id = tx
                .create_task(
                    &work,
                    json!({ "path": "aborting" }),
                    conversation_task_options(parent_id, false),
                )
                .await?;
            let child = tx
                .create_conversation(&kiana_durable::CreateConversationOptions {
                    ownership: ConversationOwnership::Task {
                        task_id: supervisor_id,
                    },
                })
                .await?;
            *support::lock(&rejected_slot) = Some(child.id);
            tx.set_task(kiana_durable::TaskRecord {
                id: supervisor_id,
                conversation_id: parent_id,
                kind: "test.work".to_owned(),
                version: 1,
                input: json!({ "path": "aborting" }),
                trace: None,
                links: None,
                owner: None,
                background: false,
                abort_requested: true,
                abandon_on_restart: None,
                state: TaskState::Pending {
                    checkpoint: json!({ "phase": "start" }),
                },
                started_at: None,
                ended_at: None,
                restarts: None,
                abort_reason: None,
                memos: None,
            })?;
            Ok(())
        })
    })
    .await
    .expect_err("an abort-marked owner is rejected");
    assert!(error.to_string().contains("is abort-marked"));
    let rejected_child_id = support::lock(&rejected_child).expect("the child was recorded");
    assert!(
        storage
            .conversation(rejected_child_id, ctx())
            .await
            .unwrap()
            .is_none()
    );

    let work_for_terminal = work.clone();
    let error = commit(&session, move |tx| {
        let work = work_for_terminal.clone();
        Box::pin(async move {
            let supervisor_id = tx
                .create_task(
                    &work,
                    json!({ "path": "terminal" }),
                    conversation_task_options(parent_id, false),
                )
                .await?;
            tx.create_conversation(&kiana_durable::CreateConversationOptions {
                ownership: ConversationOwnership::Task {
                    task_id: supervisor_id,
                },
            })
            .await?;
            tx.set_task(kiana_durable::TaskRecord {
                id: supervisor_id,
                conversation_id: parent_id,
                kind: "test.work".to_owned(),
                version: 1,
                input: json!({ "path": "terminal" }),
                trace: None,
                links: None,
                owner: None,
                background: false,
                abort_requested: false,
                abandon_on_restart: None,
                state: terminal_state(),
                started_at: None,
                ended_at: None,
                restarts: None,
                abort_reason: None,
                memos: None,
            })?;
            Ok(())
        })
    })
    .await
    .expect_err("a terminal owner is rejected");
    assert!(error.to_string().contains("is terminal"));

    let owner_id = create_task(&session, &work, parent_id, None)
        .await
        .expect("task creation succeeds");
    let owner = storage.task(owner_id, ctx()).await.unwrap().unwrap();
    commit(&session, move |tx| {
        let mut record = owner.clone();
        Box::pin(async move {
            record.state = terminal_state();
            tx.set_task(record)?;
            Ok(())
        })
    })
    .await
    .expect("the owner settles terminally");

    let error = commit(&session, move |tx| {
        Box::pin(async move {
            tx.create_conversation(&kiana_durable::CreateConversationOptions {
                ownership: ConversationOwnership::Task { task_id: owner_id },
            })
            .await?;
            Ok(())
        })
    })
    .await
    .expect_err("a committed terminal owner is rejected");
    assert!(error.to_string().contains("is terminal"));
}

#[tokio::test]
async fn takes_ownership_of_table_json() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let payload = json!({ "nested": { "value": 1 } });

    let entry = commit(&session, move |tx| {
        Box::pin(async move {
            let created = tx
                .append_entry(conversation_id, entry_draft("data", Some(payload.clone())))
                .await?;
            let mut payload = payload;
            payload["nested"]["value"] = json!(2);
            Ok(created)
        })
    })
    .await
    .expect("the entry commits");
    assert_eq!(
        storage
            .entry(entry.id, ctx())
            .await
            .unwrap()
            .unwrap()
            .entry
            .data,
        Some(json!({ "nested": { "value": 1 } }))
    );

    let omitted = commit(&session, move |tx| {
        Box::pin(async move {
            tx.append_entry(conversation_id, entry_draft("omitted", None))
                .await
        })
    })
    .await
    .expect("the entry commits");
    assert!(
        storage
            .entry(omitted.id, ctx())
            .await
            .unwrap()
            .unwrap()
            .entry
            .data
            .is_none()
    );
}

#[tokio::test]
async fn replaces_task_records_completely() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let task = work_task("test.work", 1);
    let task_id = create_task(&session, &task, conversation_id, None)
        .await
        .expect("task creation succeeds");
    let record = storage.task(task_id, ctx()).await.unwrap().unwrap();

    commit(&session, move |tx| {
        let record = record.clone();
        Box::pin(async move {
            let mut replacement = record.clone();
            replacement.state = TaskState::Running {
                checkpoint: json!({ "phase": "next", "step": 2 }),
            };
            replacement.memos = Some([("choice".to_owned(), json!("b"))].into_iter().collect());
            tx.set_task(replacement)?;
            Ok(())
        })
    })
    .await
    .expect("the replacement commits");

    let stored = storage.task(task_id, ctx()).await.unwrap().unwrap();
    assert_eq!(
        stored.state,
        TaskState::Running {
            checkpoint: json!({ "phase": "next", "step": 2 }),
        }
    );
    assert_eq!(stored.memos.unwrap().get("choice"), Some(&json!("b")));
}

#[tokio::test]
async fn creates_a_task_and_then_its_document_in_one_transaction_without_read_after_write() {
    let (_storage, session, publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let work = work_task("test.work", 1);
    let progress = progress_definition("test.progress");
    let step = step_definition("test.step");

    let work_for_commit = work.clone();
    let progress_for_commit = progress.clone();
    let step_for_commit = step.clone();
    let task_id = commit(&session, move |tx| {
        let work = work_for_commit.clone();
        let progress = progress_for_commit.clone();
        let step = step_for_commit.clone();
        Box::pin(async move {
            let task_id = tx
                .create_task(
                    &work,
                    json!({ "path": "a" }),
                    conversation_task_options(conversation_id, false),
                )
                .await?;
            let mut progress_draft = tx.doc(&DocTarget::task(&progress, task_id)).await?;
            push_line(&mut progress_draft, "created");
            let mut step_draft = tx
                .doc(&DocTarget::task_family(&step, task_id, "one", json!(null)))
                .await?;
            push_line(&mut step_draft, "step");
            Ok(task_id)
        })
    })
    .await
    .expect("the commit succeeds");

    assert_eq!(
        snapshot_target(&session, &DocTarget::task(&progress, task_id))
            .await
            .unwrap(),
        Some(
            serde_json::from_value(json!({ "lines": ["created"] }))
                .expect("the snapshot is an object")
        )
    );

    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the commit published");
    assert!(publication.changes.iter().any(|change| matches!(
        change,
        CommitChange::Table(TableCommitChange::Task { value }) if value.id == task_id
    )));
    let documents = document_changes(&publication);
    assert_eq!(documents.len(), 2);
    for document in &documents {
        match document {
            kiana_durable::DocumentCommitChange::Document {
                conversation_id: owner,
                ..
            } => {
                assert_eq!(*owner, Some(conversation_id));
            }
            kiana_durable::DocumentCommitChange::DocumentCopy {
                conversation_id: owner,
                ..
            } => {
                assert_eq!(*owner, conversation_id);
            }
        }
    }

    let progress_for_later = progress.clone();
    commit(&session, move |tx| {
        let progress = progress_for_later.clone();
        Box::pin(async move {
            tx.create_conversation(&kiana_durable::CreateConversationOptions {
                ownership: ConversationOwnership::Ownerless,
            })
            .await?;
            let mut draft = tx.doc(&DocTarget::task(&progress, task_id)).await?;
            push_line(&mut draft, "committed task");
            Ok(())
        })
    })
    .await
    .expect("the follow-up commit succeeds");
    let documents = document_changes(
        &support::lock(&publications)
            .last()
            .cloned()
            .expect("the commit published"),
    );
    assert_eq!(documents.len(), 1);
    match &documents[0] {
        kiana_durable::DocumentCommitChange::Document {
            conversation_id: owner,
            ..
        } => {
            assert_eq!(*owner, Some(conversation_id));
        }
        kiana_durable::DocumentCommitChange::DocumentCopy {
            conversation_id: owner,
            ..
        } => {
            assert_eq!(*owner, conversation_id);
        }
    }
}

fn push_line(draft: &mut kiana_durable::JsonObject, line: &str) {
    let lines = draft
        .entry("lines".to_owned())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("lines is an array");
    lines.push(json!(line));
}

#[tokio::test]
async fn rejects_task_documents_after_a_terminal_candidate() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let work = work_task("test.work", 1);
    let progress = progress_definition("test.progress");
    let step = step_definition("test.step");
    let task_id = create_task(
        &session,
        &work,
        conversation_id,
        Some((&progress, "started")),
    )
    .await
    .expect("task creation succeeds");

    let progress_for_commit = progress.clone();
    let step_for_commit = step.clone();
    commit(&session, move |tx| {
        let progress = progress_for_commit.clone();
        let step = step_for_commit.clone();
        Box::pin(async move {
            let task = tx.task(task_id).await?.expect("the task exists");
            let mut progress_draft = tx.doc(&DocTarget::task(&progress, task_id)).await?;
            let mut record = task.clone();
            record.state = terminal_state();
            tx.set_task(record)?;
            let error = tx
                .doc(&DocTarget::task(&progress, task_id))
                .await
                .expect_err("a terminal candidate closes task documents");
            assert_eq!(error.to_string(), format!("Task {task_id} is terminal"));
            let error = tx
                .doc(&DocTarget::task_family(&step, task_id, "late", json!(null)))
                .await
                .expect_err("a terminal candidate closes task document families");
            assert_eq!(error.to_string(), format!("Task {task_id} is terminal"));
            let error = tx
                .set_task(task.clone())
                .expect_err("an earlier record cannot replace a terminal candidate");
            assert!(error.to_string().contains("terminal candidate"));
            push_line(&mut progress_draft, "final");
            Ok(())
        })
    })
    .await
    .expect("the commit succeeds");

    let error = commit(&session, move |tx| {
        let progress = progress.clone();
        Box::pin(async move {
            tx.doc(&DocTarget::task(&progress, task_id)).await?;
            Ok(())
        })
    })
    .await
    .expect_err("a committed terminal task rejects document access");
    assert_eq!(error.to_string(), format!("Task {task_id} is terminal"));
}

#[tokio::test]
async fn retires_task_documents_at_terminal_settlement() {
    let (storage, session, publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let work = work_task("test.work", 1);
    let progress = progress_definition("test.progress");
    let step = step_definition("test.step");
    let task_id = create_task(
        &session,
        &work,
        conversation_id,
        Some((&progress, "started")),
    )
    .await
    .expect("task creation succeeds");

    let step_for_commit = step.clone();
    commit(&session, move |tx| {
        let step = step_for_commit.clone();
        Box::pin(async move {
            let mut draft = tx
                .doc(&DocTarget::task_family(
                    &step,
                    task_id,
                    "committed",
                    json!(null),
                ))
                .await?;
            push_line(&mut draft, "x");
            Ok(())
        })
    })
    .await
    .expect("the step commits");

    let published = support::lock(&publications).len();
    let step_for_settle = step.clone();
    commit(&session, move |tx| {
        let step = step_for_settle.clone();
        Box::pin(async move {
            let task = tx.task(task_id).await?.expect("the task exists");
            let mut draft = tx
                .doc(&DocTarget::task_family(&step, task_id, "new", json!(null)))
                .await?;
            push_line(&mut draft, "created then retired");
            let mut record = task;
            record.state = terminal_state();
            tx.set_task(record)?;
            Ok(())
        })
    })
    .await
    .expect("the terminal settlement commits");

    let writes = support::lock(&storage.commits)
        .last()
        .cloned()
        .expect("the settlement reached storage");
    assert_eq!(writes.len(), 5);
    assert_eq!(
        writes
            .iter()
            .filter(|write| matches!(write, kiana_durable::StorageWrite::Task { .. }))
            .count(),
        1
    );
    let creations: Vec<_> = writes
        .iter()
        .filter_map(|write| match write {
            kiana_durable::StorageWrite::DocumentCreate { record, .. } => Some(record.id),
            _ => None,
        })
        .collect();
    let retirements: Vec<_> = writes
        .iter()
        .filter_map(|write| match write {
            kiana_durable::StorageWrite::DocumentRetire { id } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(creations.len(), 1);
    assert_eq!(retirements.len(), 3);
    assert!(retirements.contains(&creations[0]));

    assert_eq!(support::lock(&publications).len(), published + 1);
    let publication = support::lock(&publications)
        .last()
        .cloned()
        .expect("the settlement published");
    assert_eq!(
        publication
            .changes
            .iter()
            .filter(|change| matches!(change, CommitChange::Table(TableCommitChange::Task { .. })))
            .count(),
        1
    );
    let documents = document_changes(&publication);
    assert_eq!(documents.len(), 3);
    for document in &documents {
        match document {
            kiana_durable::DocumentCommitChange::Document {
                value,
                ops,
                conversation_id: owner,
                ..
            } => {
                assert!(value.is_none());
                assert!(ops.is_empty());
                assert_eq!(*owner, Some(conversation_id));
            }
            kiana_durable::DocumentCommitChange::DocumentCopy { .. } => {
                panic!("expected a document retirement")
            }
        }
    }
    assert!(
        snapshot_target(&session, &DocTarget::task(&progress, task_id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        snapshot_target(
            &session,
            &DocTarget::task_family(&step, task_id, "committed", json!(null))
        )
        .await
        .unwrap()
        .is_none()
    );
    let alive = storage
        .scan_documents(
            &kiana_durable::DocumentQuery {
                scope: kiana_durable::DocumentScope::Task { task_id },
                at: kiana_durable::DocumentPoint::Current,
                kind: None,
            },
            10,
            None,
            ctx(),
        )
        .await
        .unwrap();
    assert!(alive.items.is_empty());

    let error = commit(&session, move |tx| {
        Box::pin(async move {
            let record = tx.task(task_id).await?.expect("the task exists");
            let mut record = record;
            record.state = terminal_state();
            tx.set_task(record)?;
            Ok(())
        })
    })
    .await
    .expect_err("a settled task cannot settle again");
    assert!(
        error
            .to_string()
            .contains(&format!("Task {task_id} is already terminal"))
    );
}

#[tokio::test]
async fn validates_document_owners() {
    let (_storage, session, _publications) = open_test_session();
    let progress = progress_definition("test.progress");
    let notes = notes_definition("test.notes");

    let target = DocTarget::task(
        &progress,
        kiana_durable::id_from_number::<kiana_durable::TaskBrand>(4242),
    );
    let error = commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            tx.doc(&target).await?;
            Ok(())
        })
    })
    .await
    .expect_err("a missing task owner is rejected");
    assert_eq!(error.to_string(), "Task 4242 does not exist");

    let target = DocTarget::conversation(
        &notes,
        kiana_durable::id_from_number::<kiana_durable::ConversationBrand>(4242),
    );
    let error = commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            tx.doc(&target).await?;
            Ok(())
        })
    })
    .await
    .expect_err("a missing conversation owner is rejected");
    assert_eq!(error.to_string(), "Conversation 4242 does not exist");
}

// ---------------------------------------------------------------------------
// 回归：一个事务里的重复任务读共享同一次后端读（TS: `committedRead ??= storage.task()`）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memoizes_a_duplicate_task_read_within_one_transaction() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let task = work_task("test.work", 1);
    let task_id = create_task(&session, &task, conversation_id, None)
        .await
        .expect("task creation succeeds");
    let before = storage.task_read_count.load(Ordering::SeqCst);

    commit(&session, move |tx| {
        Box::pin(async move {
            let (first, second) = futures::join!(tx.task(task_id), tx.task(task_id));
            assert_eq!(first?.map(|record| record.id), Some(task_id));
            assert_eq!(second?.map(|record| record.id), Some(task_id));
            Ok(())
        })
    })
    .await
    .expect("the reads succeed");

    assert_eq!(
        storage.task_read_count.load(Ordering::SeqCst) - before,
        1,
        "duplicate task reads share one backend read"
    );
}

// ---------------------------------------------------------------------------
// 回归：`#host.now` 是原始提供者，它抛出即让提交失败（不被兜底墙钟替换）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn propagates_a_failing_clock_to_the_commit() {
    let storage: Arc<dyn Storage> = Arc::new(ControlledStorage::new());
    let session = kiana_durable::session::SessionImpl::new(
        Arc::clone(&storage),
        kiana_durable::session::SessionOptions {
            document_retention_ms: Some(600_000.0),
            // `now` 直接 panic 以模拟失败时钟；提交路径必须把该失败传播出去。
            now: Some(Arc::new(|| panic!("clock unavailable"))),
            ..Default::default()
        },
    );

    let conversation_id = create_conversation(&session).await;
    let task = work_task("test.work", 1);
    // Pending 任务不需要时间戳：提交成功，证明失败只来自之后的时钟调用。
    let task_id = create_task(&session, &task, conversation_id, None)
        .await
        .expect("a Pending task creation does not read the clock");
    let record = storage.task(task_id, ctx()).await.unwrap().unwrap();

    let error = commit(&session, move |tx| {
        Box::pin(async move {
            let mut running = record;
            running.state = TaskState::Running {
                checkpoint: json!({ "phase": "run" }),
            };
            tx.set_task(running)?;
            Ok(())
        })
    })
    .await
    .expect_err("a throwing clock fails the commit");

    assert_eq!(error.to_string(), "clock unavailable");
    assert!(
        error
            .downcast_ref::<kiana_durable::session::ReportedError>()
            .is_some(),
        "the thrown clock error is what the commit reports"
    );
}
