//! `test/codemode-checkpoints.test.ts` 的移植：内建文档的 checkpoint 策略。
//!
//! 任务作用域的文档永远用 delta（任务退役时一起消失）；`kiana.codemode.store` 在
//! `deltasSinceBase >= 64` 时把下一次变化存成完整 base，但历史读取与 fork 看到的仍是
//! 各自时间点的值。

mod support;

use kiana_chord::context::Context;
use kiana_durable::codemode::codemode::{
    CODEMODE_JOURNAL_DOC, CODEMODE_RUN_DOC, CODEMODE_STORE_DOC,
};
use kiana_durable::harness::tool::NESTED_CALL_DOC;
use kiana_durable::types::{
    CheckpointInfo, ConversationOwnership, CreateConversationOptions, DocTarget, DocumentAddress,
    DocumentPoint, DocumentScope, EntryDraft, JsonObject, Storage, StorageWrite,
};
use serde_json::json;

use support::{commit, create_conversation, lock, open_test_session};

fn context() -> &'static dyn Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

#[test]
fn keeps_task_scoped_documents_as_deltas_until_their_task_retires() {
    let info = CheckpointInfo {
        deltas_since_base: 1000,
    };
    let empty = JsonObject::new();
    assert!(!(NESTED_CALL_DOC.definition.checkpoint_when)(
        &empty,
        &[],
        info
    ));
    assert!(!(CODEMODE_RUN_DOC.definition.checkpoint_when)(
        &empty,
        &[],
        info
    ));
    let mut journal = JsonObject::new();
    journal.insert("entries".to_owned(), json!([]));
    assert!(!(CODEMODE_JOURNAL_DOC.definition.checkpoint_when)(
        &journal,
        &[],
        info
    ));
}

fn counter_target(conversation_id: kiana_durable::types::ConversationId) -> DocTarget {
    DocTarget::conversation(&CODEMODE_STORE_DOC, conversation_id)
}

fn counter_entry() -> EntryDraft {
    EntryDraft {
        key_order: None,
        kind: "test.counter".to_owned(),
        model: None,
        data: None,
        head: None,
        edits: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn checkpoints_the_codemode_store_after_64_deltas_without_changing_historical_reads_or_forks()
{
    let (storage, session, _publications) = open_test_session();
    let context = context();
    let conversation_id = create_conversation(&session).await;
    let target = counter_target(conversation_id);
    let first_target = target.clone();

    let mut cutoff = commit(&session, move |tx| {
        let target = first_target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&target).await?;
            draft
                .value_mut()
                .insert("values".to_owned(), json!({ "counter": 0 }));
            Ok(tx.append_entry(conversation_id, counter_entry()).await?.id)
        })
    })
    .await
    .expect("the first commit");
    let record = storage
        .inner()
        .find_document(
            &DocumentAddress {
                kind: CODEMODE_STORE_DOC.definition.kind.clone(),
                scope: DocumentScope::Conversation { conversation_id },
                key: None,
            },
            DocumentPoint::Current,
            context,
        )
        .await
        .expect("find")
        .expect("Expected codemode store");
    for counter in 1..=64i64 {
        let target = target.clone();
        let entry = commit(&session, move |tx| {
            let target = target.clone();
            Box::pin(async move {
                let mut draft = tx.doc(&target).await?;
                draft
                    .value_mut()
                    .insert("values".to_owned(), json!({ "counter": counter }));
                Ok(tx.append_entry(conversation_id, counter_entry()).await?.id)
            })
        })
        .await
        .expect("a counter commit");
        if counter == 32 {
            cutoff = entry;
        }
    }
    let stored = storage
        .inner()
        .document(record.id, DocumentPoint::Current, context)
        .await
        .expect("document")
        .expect("the store");
    assert_eq!(stored.deltas_since_base, 64);

    let target = counter_target(conversation_id);
    commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&target).await?;
            draft
                .value_mut()
                .insert("values".to_owned(), json!({ "counter": 65 }));
            Ok(())
        })
    })
    .await
    .expect("the 65th commit");
    let stored = storage
        .inner()
        .document(record.id, DocumentPoint::Current, context)
        .await
        .expect("document")
        .expect("the store");
    assert_eq!(stored.deltas_since_base, 0);
    {
        let commits = lock(&storage.commits);
        let last = commits.last().expect("a commit");
        let expected = json!({
            "type": "document.change",
            "id": record.id.get(),
            "content": {
                "version": 1,
                "kind": "base",
                "value": { "values": { "counter": 65 } },
            },
        });
        assert!(
            last.iter().any(|write| {
                matches!(write, StorageWrite::DocumentChange { .. })
                    && serde_json::to_value(write).expect("wire") == expected
            }),
            "{last:?}"
        );
    }

    let as_of = session
        .snapshot_as_of(&counter_target(conversation_id), cutoff, context)
        .await
        .expect("as of")
        .expect("the store");
    assert_eq!(
        serde_json::to_value(&as_of).expect("json"),
        json!({ "values": { "counter": 32 } })
    );

    let fork = commit(&session, move |tx| {
        Box::pin(async move {
            Ok(tx
                .fork_conversation(
                    conversation_id,
                    cutoff,
                    &CreateConversationOptions {
                        ownership: ConversationOwnership::Ownerless,
                    },
                )
                .await?
                .id)
        })
    })
    .await
    .expect("fork");
    let forked = session
        .snapshot(&counter_target(fork), context)
        .await
        .expect("snapshot")
        .expect("the forked store");
    assert_eq!(
        serde_json::to_value(&forked).expect("json"),
        json!({ "values": { "counter": 32 } })
    );

    let target = counter_target(conversation_id);
    commit(&session, move |tx| {
        let target = target.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&target).await?;
            draft
                .value_mut()
                .insert("values".to_owned(), json!({ "counter": 66 }));
            Ok(())
        })
    })
    .await
    .expect("the 66th commit");
    let stored = storage
        .inner()
        .document(record.id, DocumentPoint::Current, context)
        .await
        .expect("document")
        .expect("the store");
    assert_eq!(stored.deltas_since_base, 1);
    session.close(context).await;
}
