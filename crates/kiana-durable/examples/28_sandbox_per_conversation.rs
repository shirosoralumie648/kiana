//! 移植自 `packages/durable/test/examples/28-sandbox-per-conversation.ts`：每个会话一个沙箱。
//! 应用把每个会话的沙箱记录在它自己的文档里，Harness 环境函数每次工具调用都去查它。这里一个
//! 沙箱就是一个目录；托管产品会返回一个在该会话容器里运行的 ExecutionEnv。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_provider,
    faux_tool_call,
};
use kiana_ai::types::{AssistantContentBlock, JsonObject, StopReason};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::env::ExecutionEnv;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, Conversation as ConversationTrait, ConversationCreateOptions, ConversationInit,
    EnvBuilder, HarnessOptions, ModelRef, ModelsProvider, Registry, RegistryReader,
    SubmissionDraft, SubmissionDraftKind, UserInput,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::tools::CodingTools;
use kiana_durable::types::{
    BoxError, ConversationOwnership, DocDefinition, DocTarget, DocumentFork, DocumentHistory,
    DocumentSemantics, JsonObject as DocObject, JsonValue, ScopeKind, Session, Tx, define_doc,
};

/// 一个会话在哪个沙箱里运行。`fork: "initial"`：fork 直到应用分配之前没有沙箱。
fn sandbox_token() -> kiana_durable::types::DocToken {
    define_doc(
        DocDefinition::new(
            "app.sandbox",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| DocObject::new()),
    )
}

fn faux_model() -> ModelRef {
    ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    }
}

fn note(text: &str) -> FauxResponseStep {
    let mut arguments = JsonObject::new();
    arguments.insert("path".to_owned(), JsonValue::from("note.txt"));
    arguments.insert("content".to_owned(), JsonValue::from(text));
    FauxResponseStep::Message(faux_assistant_message(
        AssistantContentBlock::ToolCall(faux_tool_call("write", arguments)),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn saved() -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message("Saved.", Default::default()))
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// 为一个用户建一个带新沙箱目录的会话：沙箱路径在创建提交里写进会话文档。
async fn conversation_for(
    harness: &Arc<kiana_durable::harness::harness::HarnessImpl>,
    user: &str,
    context: &dyn Context,
) -> Result<(Arc<dyn ConversationTrait>, String), BoxError> {
    let path = format!(
        "{}/kiana-durable-sandbox-{user}-{}",
        std::env::temp_dir().to_string_lossy(),
        unique_suffix()
    );
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|error| Box::new(error) as BoxError)?;
    let sandbox = sandbox_token();
    let init_path = path.clone();
    let init: ConversationInit = Arc::new(move |tx: &dyn Tx, id| {
        let sandbox = sandbox.clone();
        let path = init_path.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&DocTarget::conversation(&sandbox, id)).await?;
            draft
                .value_mut()
                .insert("path".to_owned(), JsonValue::from(path));
            Ok(())
        })
    });
    let conversation = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    model: Some(Some(faux_model())),
                    ..Default::default()
                }),
                init: Some(init),
            },
            context,
        )
        .await?;
    Ok((conversation, path))
}

async fn submit_note(
    conversation: &Arc<dyn ConversationTrait>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let submission = conversation
        .submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: UserInput::Text("Leave a note.".to_owned()),
                    when_busy: None,
                    entry: None,
                },
            },
            context,
        )
        .await?;
    submission.wait(context).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;

    let faux = faux_provider();
    faux.set_responses(vec![note("from alice"), saved(), note("from bob"), saved()]);
    let models: Arc<dyn Models> = {
        let models = create_models(Default::default());
        models.set_provider(Arc::clone(&faux.provider));
        models
    };

    let registry = create_registry_impl();
    registry.install(Arc::clone(&*CodingTools));
    let env: EnvBuilder = Arc::new(|target, context| {
        Box::pin(async move {
            // 只读已提交的文档；没有沙箱的会话拿不到环境，工具会干净地失败。
            let sandbox = target
                .read
                .snapshot(
                    &DocTarget::conversation(&sandbox_token(), target.conversation_id),
                    &*context,
                )
                .await?;
            let path = sandbox
                .as_ref()
                .and_then(|sandbox| sandbox.get("path"))
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            match path {
                None => Ok(None),
                Some(path) => {
                    let env = NodeExecutionEnv::new(NodeEnvOptions::new(path));
                    Ok(Some(Arc::new(env) as Arc<dyn ExecutionEnv>))
                }
            }
        })
    });
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(models),
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: Some(env),
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

    // 每个用户的会话在创建提交里拿到一个全新的沙箱。
    let (alice, alice_path) = conversation_for(&harness, "alice", context).await?;
    let (bob, bob_path) = conversation_for(&harness, "bob", context).await?;
    submit_note(&alice, context).await?;
    submit_note(&bob, context).await?;
    println!(
        "alice's sandbox: {}",
        tokio::fs::read_to_string(format!("{alice_path}/note.txt"))
            .await
            .map_err(|error| Box::new(error) as BoxError)?
    );
    println!(
        "bob's sandbox: {}",
        tokio::fs::read_to_string(format!("{bob_path}/note.txt"))
            .await
            .map_err(|error| Box::new(error) as BoxError)?
    );

    harness.close(context).await;
    let _ = tokio::fs::remove_dir_all(&alice_path).await;
    let _ = tokio::fs::remove_dir_all(&bob_path).await;
    Ok(())
}
