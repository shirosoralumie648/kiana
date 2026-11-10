//! 移植自 `packages/durable/test/examples/02-forks.ts`：从一个条目分叉会话。
//!
//! fork 是另一个会话的延续：它看见父会话截至该条目的转录，文档则各自按 `fork` 语义复制。
//! Notes 用 "asOf"，因此分支从 fork 条目处的值开始。

use std::sync::Arc;

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::{
    BoxError, ConversationOwnership, CreateConversationOptions, DocDefinition, DocTarget,
    DocumentFork, DocumentHistory, DocumentSemantics, EntryDraft, EntryQuery, JsonObject,
    JsonValue, ScopeKind, Tx, define_doc,
};

fn notes_definition() -> DocDefinition {
    DocDefinition::new(
        "example.notes",
        1,
        DocumentSemantics {
            scope: ScopeKind::Conversation,
            history: Some(DocumentHistory::Rewindable),
            fork: Some(DocumentFork::AsOf),
        },
    )
    .with_initial(|_| {
        let mut value = JsonObject::new();
        value.insert("text".to_owned(), JsonValue::String(String::new()));
        value
    })
}

fn note(kind: &str, data: Option<JsonValue>) -> EntryDraft {
    EntryDraft {
        kind: kind.to_owned(),
        model: None,
        data,
        head: None,
        edits: None,
    }
}

fn ownerless() -> CreateConversationOptions {
    CreateConversationOptions {
        ownership: ConversationOwnership::Ownerless,
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let session = SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default());
    let notes = define_doc(notes_definition());

    let chat = session
        .commit_with(
            |tx| Box::pin(async move { tx.create_conversation(&ownerless()).await }),
            context,
            Default::default(),
        )
        .await?;

    let notes_target = DocTarget::conversation(&notes, chat.id);
    let first_entry = session
        .commit_with(
            {
                let notes_target = notes_target.clone();
                move |tx| {
                    let notes_target = notes_target.clone();
                    Box::pin(async move {
                        let entry = tx
                            .append_entry(chat.id, note("note", Some(JsonValue::from("hello"))))
                            .await?;
                        let mut draft = tx.doc(&notes_target).await?;
                        draft
                            .value_mut()
                            .insert("text".to_owned(), JsonValue::from("after hello"));
                        Ok(entry)
                    })
                }
            },
            context,
            Default::default(),
        )
        .await?;

    let notes_target = DocTarget::conversation(&notes, chat.id);
    session
        .commit_with(
            move |tx| {
                let notes_target = notes_target.clone();
                Box::pin(async move {
                    tx.append_entry(chat.id, note("note", Some(JsonValue::from("goodbye"))))
                        .await?;
                    let mut draft = tx.doc(&notes_target).await?;
                    draft
                        .value_mut()
                        .insert("text".to_owned(), JsonValue::from("after goodbye"));
                    Ok(())
                })
            },
            context,
            Default::default(),
        )
        .await?;

    // 分支从 first_entry 处继续；它继承 "hello"，但看不到后来的 "goodbye"。
    let branch = session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    tx.fork_conversation(chat.id, first_entry.id, &ownerless())
                        .await
                })
            },
            context,
            Default::default(),
        )
        .await?;

    let entries = session
        .commit_with(
            move |tx| {
                Box::pin(async move {
                    tx.scan_entries(
                        &EntryQuery {
                            conversation_id: branch.id,
                            min_entry_id: None,
                            max_entry_id: None,
                            order: None,
                        },
                        10,
                        None,
                    )
                    .await
                })
            },
            context,
            Default::default(),
        )
        .await?;
    println!(
        "fork transcript: {:?}",
        entries
            .items
            .iter()
            .map(|entry| entry.data.clone())
            .collect::<Vec<_>>()
    );
    println!(
        "fork notes: {:?}",
        session
            .snapshot(&DocTarget::conversation(&notes, branch.id), context)
            .await?
    );

    // 副本互相独立：编辑分支不影响父会话。
    let branch_target = DocTarget::conversation(&notes, branch.id);
    session
        .commit_with(
            move |tx| {
                let branch_target = branch_target.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&branch_target).await?;
                    draft.value_mut().insert(
                        "text".to_owned(),
                        JsonValue::from("changed only in the fork"),
                    );
                    Ok(())
                })
            },
            context,
            Default::default(),
        )
        .await?;
    println!(
        "fork notes after edit: {:?}",
        session
            .snapshot(&DocTarget::conversation(&notes, branch.id), context)
            .await?
    );
    println!(
        "parent notes after edit: {:?}",
        session
            .snapshot(&DocTarget::conversation(&notes, chat.id), context)
            .await?
    );

    session.close(context).await;
    Ok(())
}
