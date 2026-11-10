//! 移植自 `packages/durable/test/examples/01-documents.ts`：把文档状态与转录条目存在一起。
//!
//! 该示例依赖 `session`（`create_session`）与 `storage::MemoryStorage` 两个单元的完整实现，
//! 因此由 `examples` feature 门控；在单元合并前 `cargo check --all-targets` 会跳过它。

use std::sync::Arc;

use async_trait::async_trait;

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::session::create_session;
use kiana_durable::storage::MemoryStorage;
use kiana_durable::{
    BoxError, ConversationOwnership, CreateConversationOptions, DocDefinition, DocTarget,
    DocumentFork, DocumentHistory, DocumentSemantics, ScopeKind, Session, SessionChange, Tx,
    define_doc,
};

struct CreateConversation {
    options: CreateConversationOptions,
}

#[async_trait]
impl SessionChange for CreateConversation {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let conversation = tx.create_conversation(&self.options).await?;
        Ok(Box::new(conversation))
    }
}

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
        let mut value = kiana_durable::JsonObject::new();
        value.insert(
            "text".to_owned(),
            kiana_durable::JsonValue::String(String::new()),
        );
        value
    })
}

#[tokio::main]
async fn main() -> Result<(), kiana_durable::BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let session = create_session(Arc::new(MemoryStorage::new()));

    #[allow(non_snake_case)]
    let Notes = define_doc(notes_definition());

    let conversation = session
        .commit(
            &CreateConversation {
                options: CreateConversationOptions {
                    ownership: ConversationOwnership::Ownerless,
                },
            },
            context,
            Default::default(),
        )
        .await?
        .downcast::<kiana_durable::ConversationRecord>()
        .expect("commit returns the conversation record");

    // 每个提交里写入条目与文档。
    let first_entry = session
        .commit(
            &AppendAndSetNotes {
                conversation_id: conversation.id,
                data: "hello".to_owned(),
                text: "after hello".to_owned(),
            },
            context,
            Default::default(),
        )
        .await?;
    let second_entry = session
        .commit(
            &AppendAndSetNotes {
                conversation_id: conversation.id,
                data: "goodbye".to_owned(),
                text: "after goodbye".to_owned(),
            },
            context,
            Default::default(),
        )
        .await?;

    let notes = DocTarget::conversation(&Notes, conversation.id);
    println!(
        "latest notes: {:?}",
        session.snapshot(&notes, context).await?
    );
    let first_id = first_entry
        .downcast::<kiana_durable::EntryRecord>()
        .expect("entry")
        .id;
    let second_id = second_entry
        .downcast::<kiana_durable::EntryRecord>()
        .expect("entry")
        .id;
    println!(
        "notes at first entry: {:?}",
        session.snapshot_as_of(&notes, first_id, context).await?
    );
    println!(
        "notes at second entry: {:?}",
        session.snapshot_as_of(&notes, second_id, context).await?
    );

    session.close(context).await;
    Ok(())
}

struct AppendAndSetNotes {
    conversation_id: kiana_durable::ConversationId,
    data: String,
    text: String,
}

#[async_trait]
impl SessionChange for AppendAndSetNotes {
    async fn run(
        &self,
        tx: &dyn Tx,
    ) -> Result<Box<dyn std::any::Any + Send>, kiana_durable::BoxError> {
        let entry = tx
            .append_entry(
                self.conversation_id,
                kiana_durable::EntryDraft {
                    kind: "note".to_owned(),
                    model: None,
                    data: Some(kiana_durable::JsonValue::String(self.data.clone())),
                    head: None,
                    edits: None,
                },
            )
            .await?;
        // 示例定义在真实代码里由 `define_doc` 持有；这里重建同一个定义以取地址。
        let notes = DocTarget::conversation(&define_doc(notes_definition()), self.conversation_id);
        let mut draft = tx.doc(&notes).await?;
        draft.value_mut().insert(
            "text".to_owned(),
            kiana_durable::JsonValue::String(self.text.clone()),
        );
        Ok(Box::new(entry))
    }
}
