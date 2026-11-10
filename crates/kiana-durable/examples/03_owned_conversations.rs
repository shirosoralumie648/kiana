//! 移植自 `packages/durable/test/examples/03-owned-conversations.ts`：后台任务拥有子会话。
//!
//! 典型 agent 布局：后台任务监督一个助手会话，主会话维护"名字 → 会话"的登记表。三者在同一个
//! 提交里创建，因此崩溃后要么都存在，要么都不存在。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::types::{AnyTask, AnyTaskDefinition};
use kiana_durable::session::{ErasedTaskInitial, SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::{
    BoxError, ConversationOwnership, CreateConversationOptions, DocDefinition, DocTarget,
    DocumentFork, DocumentHistory, DocumentSemantics, JsonObject, JsonValue, ScopeKind,
    TaskOptions, TaskOwnership, define_doc,
};
use serde_json::json;

fn supervisor_task() -> AnyTask {
    AnyTask {
        definition: Arc::new(AnyTaskDefinition {
            name: "example.supervisor".to_owned(),
            version: 1,
            initial: Arc::new(ErasedTaskInitial(Arc::new(
                |_input| json!({ "phase": "ready" }),
            ))),
            phases: IndexMap::new(),
            abort: Arc::new(()),
            migrate: None,
            hooks: None,
        }),
    }
}

fn agent_registry_definition() -> DocDefinition {
    DocDefinition::new(
        "example.agent-registry",
        1,
        DocumentSemantics {
            scope: ScopeKind::Conversation,
            history: Some(DocumentHistory::Latest),
            fork: Some(DocumentFork::Initial),
        },
    )
    .with_initial(|_| {
        let mut value = JsonObject::new();
        value.insert("agents".to_owned(), json!({}));
        value
    })
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

    let main = session
        .commit_with(
            |tx| Box::pin(async move { tx.create_conversation(&ownerless()).await }),
            context,
            Default::default(),
        )
        .await?;

    let registry = define_doc(agent_registry_definition());
    let registry_target = DocTarget::conversation(&registry, main.id);
    let setup = session
        .commit_with(
            move |tx| {
                let registry_target = registry_target.clone();
                Box::pin(async move {
                    // `background: true`：等待主会话结束不会等待它。
                    let supervisor = supervisor_task();
                    let mut options = TaskOptions::new(TaskOwnership::Conversation);
                    options.conversation_id = Some(main.id);
                    options.background = Some(true);
                    let supervisor_id = tx
                        .create_task(&supervisor, JsonValue::Null, options)
                        .await?;

                    // 子会话记录自己属于监督任务；任务在本提交里刚创建，这是允许的。
                    let child = tx
                        .create_conversation(&CreateConversationOptions {
                            ownership: ConversationOwnership::Task {
                                task_id: supervisor_id,
                            },
                        })
                        .await?;

                    // requestId 是子会话第一条消息的固定名字：崩溃后的重试不会重复投递。
                    let mut draft = tx.doc(&registry_target).await?;
                    if let Some(agents) = draft
                        .get_mut("agents")
                        .and_then(|agents| agents.as_object_mut())
                    {
                        agents.insert(
                            "researcher".to_owned(),
                            json!({
                                "conversationId": child.id,
                                "requestId": format!("researcher:first-message:{supervisor_id}"),
                            }),
                        );
                    }
                    Ok((supervisor_id, child))
                })
            },
            context,
            Default::default(),
        )
        .await?;

    println!("supervisor task: {}", setup.0);
    println!("child conversation: {:?}", setup.1);
    println!(
        "registry: {:?}",
        session
            .snapshot(&DocTarget::conversation(&registry, main.id), context)
            .await?
    );

    session.close(context).await;
    Ok(())
}
