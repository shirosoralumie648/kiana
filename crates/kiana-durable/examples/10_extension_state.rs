//! 移植自 `packages/durable/test/examples/10-extension-state.ts`：一个持有自己逐会话文档的扩展。

use std::any::Any;
use std::sync::Arc;

use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_provider,
    faux_tool_call,
};
use kiana_ai::types::{
    AssistantContentBlock, Message, StopReason, TextContent, TextKind, Tool, UserContentBlock,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::define::{SectionRender, define_extension, define_tool, section};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, ContextViewOptions, Extension, HarnessOptions, ModelRef, ModelsProvider, Registry,
    RegistryReader, RootOptions, SubmissionDraft, SubmissionDraftKind, ToolExecuteFn,
    ToolExecutionResult, ToolRegistration, UserInput,
};
use kiana_durable::session::context_handle;
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, DocDefinition, DocTarget, DocumentFork,
    DocumentHistory, DocumentReader, DocumentSemantics, JsonObject, JsonValue, ScopeKind, Session,
    SessionChange, Tx, define_doc,
};

/// 每个会话一份 todo 列表。`asOf` rewindable：fork 从父会话在 fork 条目处的 todo 开始。
fn todos_token() -> kiana_durable::types::DocToken {
    define_doc(
        DocDefinition::new(
            "example.todos",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Rewindable),
                fork: Some(DocumentFork::AsOf),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("items".to_owned(), JsonValue::Array(Vec::new()));
            value
        }),
    )
}

fn items_of(draft: &JsonObject) -> Vec<String> {
    draft
        .get("items")
        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
        .unwrap_or_default()
}

/// 往会话的 todo 文档追加一项。
struct AddTodo {
    conversation: ConversationId,
    item: String,
}

#[async_trait::async_trait]
impl SessionChange for AddTodo {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut draft = tx
            .doc(&DocTarget::conversation(&todos_token(), self.conversation))
            .await?;
        let mut items = items_of(draft.value());
        items.push(self.item.clone());
        draft
            .value_mut()
            .insert("items".to_owned(), serde_json::to_value(items)?);
        Ok(Box::new(()))
    }
}

fn todo_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(|arguments: JsonValue, api, context| {
        let item = arguments
            .get("item")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        Box::pin(async move {
            api.commit(
                &AddTodo {
                    conversation: api.conversation_id(),
                    item: item.clone(),
                },
                &*context,
                CommitOptions::default(),
            )
            .await?;
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("added {item}"),
                    text_signature: None,
                })]),
                ..Default::default()
            })
        })
    });
    let mut tool = ToolRegistration::new(Tool {
        name: "todo".to_owned(),
        description: "Add an item to your todo list".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "item": { "type": "string" } },
            "required": ["item"]
        }),
        constrained_sampling: None,
    });
    tool.execute = Some(execute);
    define_tool(tool)
}

/// 请求前把 todo 列表展示给模型的分段；缺失文档当作空列表。
fn todos_section() -> Arc<dyn kiana_durable::harness::types::PromptSection> {
    let token = todos_token();
    let render: SectionRender = Arc::new(move |input, context| {
        let token = token.clone();
        let read = Arc::clone(&input.read);
        let conversation = input.conversation_id;
        let context = context_handle(context);
        Box::pin(async move {
            let value = read
                .snapshot(&DocTarget::conversation(&token, conversation), &*context)
                .await?;
            let Some(value) = value else {
                return Ok(None);
            };
            let items = items_of(&value);
            if items.is_empty() {
                Ok(None)
            } else {
                Ok(Some(items.join("\n")))
            }
        })
    });
    section("todos", render, None)
}

struct Todo {
    tools: Vec<ToolRegistration>,
    sections: Vec<Arc<dyn kiana_durable::harness::types::PromptSection>>,
}

impl Extension for Todo {
    fn name(&self) -> &str {
        "todo"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }

    fn sections(&self) -> Vec<Arc<dyn kiana_durable::harness::types::PromptSection>> {
        self.sections.clone()
    }
}

fn faux_model() -> ModelRef {
    ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    }
}

fn todo_call(item: &str) -> FauxResponseStep {
    let mut arguments = JsonObject::new();
    arguments.insert("item".to_owned(), JsonValue::from(item));
    FauxResponseStep::Message(faux_assistant_message(
        AssistantContentBlock::ToolCall(faux_tool_call("todo", arguments)),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn text(text: &str) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(text, Default::default()))
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;

    let faux = faux_provider();
    let models: Arc<dyn Models> = {
        let models = create_models(Default::default());
        models.set_provider(Arc::clone(&faux.provider));
        models
    };
    let registry = create_registry_impl();
    registry.install(define_extension(Todo {
        tools: vec![todo_tool()],
        sections: vec![todos_section()],
    }));
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
    let root = harness
        .root(
            context,
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(faux_model())),
                    thinking_level: Some(Some(ModelThinkingLevel::Off)),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await?;

    faux.set_responses(vec![
        todo_call("fix the build"),
        text("Noted."),
        text("Working on it."),
    ]);
    let submission = root
        .submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: UserInput::Text("Remember to fix the build.".to_owned()),
                    when_busy: None,
                    entry: None,
                },
            },
            context,
        )
        .await?;
    submission.wait(context).await?;
    let todos = harness
        .snapshot(&DocTarget::conversation(&todos_token(), root.id()), context)
        .await?;
    println!("todos: {}", serde_json::to_string(&todos)?);

    // 下一次请求的系统提示带着这个列表。第一条系统消息只宣告了 todo 工具；当时还没有分段文本。
    let submission = root
        .submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: UserInput::Text("What is next?".to_owned()),
                    when_busy: None,
                    entry: None,
                },
            },
            context,
        )
        .await?;
    submission.wait(context).await?;
    let view = root.context(context, ContextViewOptions::default()).await?;
    let system_messages: Vec<JsonValue> = view
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::System(system) => Some(serde_json::json!({
                "sections": system.sections,
                "toolsAdded": system.tools_added.as_ref().map(|tools| {
                    tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>()
                }),
            })),
            _ => None,
        })
        .collect();
    println!(
        "system messages: {}",
        serde_json::to_string(&system_messages)?
    );

    harness.close(context).await;
    Ok(())
}
