//! 移植自 `packages/durable/test/examples/07-harness-conversations.ts`：会话句柄、带类型的条目、
//! 创建的会话与 fork。

use std::any::Any;
use std::sync::Arc;

use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{Models, create_models};
use kiana_ai::types::{Message, UserContent, UserMessage, UserRole};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, ConversationCreateOptions, HarnessOptions, ModelsProvider, RegistryReader,
    RootOptions,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, EntryRecord, JsonValue,
    Session, SessionChange, Tx, TypedEntryDraft, TypedEntryTx, define_entry,
};

/// 应用条目 kind：`message` 的 `data` 是 `{ from }`。
fn message_token() -> kiana_durable::types::entries::Entry<JsonValue> {
    define_entry::<JsonValue>("message")
}

/// 在会话里追加一条带类型的条目，并把记录返回给调用方。
struct AppendMessage {
    conversation: ConversationId,
    from: String,
}

#[async_trait::async_trait]
impl SessionChange for AppendMessage {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let message = message_token();
        let typed = tx
            .append_entry_typed(
                &message,
                self.conversation,
                TypedEntryDraft {
                    model: Some(vec![Message::User(UserMessage {
                        role: UserRole::User,
                        content: UserContent::Text("hello".to_owned()),
                        timestamp: 1,
                    })]),
                    data: serde_json::json!({ "from": self.from }),
                    head: None,
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(typed.record))
    }
}

fn ownerless(agent: Option<AgentChange>) -> ConversationCreateOptions {
    ConversationCreateOptions {
        ownership: ConversationOwnership::Ownerless,
        agent,
        init: None,
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let models: Arc<dyn Models> = create_models(Default::default());
    let registry = create_registry_impl();
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(models),
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: None,
            read_only: None,
        },
        context,
    )
    .await?;
    let root = harness.root(context, RootOptions::default()).await?;
    root.configure(
        AgentChange {
            thinking_level: Some(Some(ModelThinkingLevel::High)),
            ..Default::default()
        },
        context,
    )
    .await?;

    // 会话句柄是无状态的；按 ID 比较。它们把提交绑定到自己的会话。条目 token 给一个
    // kind 的 `data` 提供类型。
    let message = message_token();
    let hello = root
        .commit(
            &AppendMessage {
                conversation: root.id(),
                from: "example".to_owned(),
            },
            context,
            CommitOptions::default(),
        )
        .await?;
    let hello = *hello
        .downcast::<EntryRecord>()
        .expect("the commit returns the entry record");
    println!(
        "typed entry: {} {}",
        message.is(&hello),
        hello
            .data
            .as_ref()
            .and_then(|data| data.get("from"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
    );

    // `create_conversation()` 与 `fork()` 在创建提交里应用 `agent` 并运行 `init`。fork 从
    // 父会话在 fork 条目处的 agent 开始。
    let helper = harness
        .create_conversation(
            ownerless(Some(AgentChange {
                thinking_level: Some(Some(ModelThinkingLevel::Minimal)),
                ..Default::default()
            })),
            context,
        )
        .await?;
    let retry = root.fork(hello.id, ownerless(None), context).await?;
    println!(
        "helper thinking: {:?}",
        helper.agent(context).await?.thinking_level
    );
    println!(
        "fork thinking: {:?}",
        retry.agent(context).await?.thinking_level
    );
    println!(
        "lookup: {}",
        harness
            .conversation(retry.id(), context)
            .await?
            .is_some_and(|found| found.id() == retry.id())
    );

    harness.close(context).await;
    Ok(())
}
