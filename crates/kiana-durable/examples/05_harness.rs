//! 移植自 `packages/durable/test/examples/05-harness.ts`：用一份扩展 registry 打开 Harness。
//!
//! Harness 是 Session 加上会话句柄与持久任务。扩展代码（工具、提示分段、hook、任务）来自应用
//! 拥有的 registry 里安装的具名扩展；registry 本身不保存，它是本进程的代码。

use std::sync::Arc;

use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{Models, create_models};
use kiana_ai::types::{TextContent, TextKind, Tool, UserContentBlock};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::AGENT_DOC;
use kiana_durable::harness::define::{define_extension, define_tool};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, ConversationInit, Extension, HarnessOptions, ModelsProvider, Registry,
    RegistryReader, RootOptions, ToolExecuteFn, ToolExecutionResult, ToolRegistration,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    BoxError, DocDefinition, DocTarget, DocumentFork, DocumentHistory, DocumentReader,
    DocumentSemantics, JsonObject, JsonValue, ScopeKind, define_doc,
};
use kiana_durable::{Session, Tx};

/// `read` 工具：返回 `contents of <path>`。
fn read_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(|arguments: JsonValue, _api, _context| {
        let path = arguments
            .get("path")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        Box::pin(async move {
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("contents of {path}"),
                    text_signature: None,
                })]),
                ..Default::default()
            })
        })
    });
    let mut tool = ToolRegistration::new(Tool {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
        constrained_sampling: None,
    });
    tool.execute = Some(execute);
    define_tool(tool)
}

struct Files {
    tools: Vec<ToolRegistration>,
}

impl Extension for Files {
    fn name(&self) -> &str {
        "files"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }
}

fn notes_definition() -> kiana_durable::types::DocToken {
    define_doc(
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
        }),
    )
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;

    // registry 随着进程里的代码变化；Harness 打开后仍可以继续安装/替换。
    let registry = create_registry_impl();
    registry.install(define_extension(Files {
        tools: vec![read_tool()],
    }));

    // `models` 是 kiana-ai 的模型访问；generation 通过它调用模型。
    let models: Arc<dyn Models> = create_models(Default::default());
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

    let notes = notes_definition();

    // 根会话永远是 ID 1。第一次 `root()` 创建它、它的内建文档、`agent` 选择，以及 `init`
    // 写入的一切，全在一个提交里。之后的调用（包括重启之后）返回它并忽略两个选项。
    let init_notes = notes.clone();
    let init: ConversationInit = Arc::new(move |tx: &dyn Tx, root_id| {
        let notes = init_notes.clone();
        Box::pin(async move {
            let mut draft = tx.doc(&DocTarget::conversation(&notes, root_id)).await?;
            draft
                .value_mut()
                .insert("text".to_owned(), JsonValue::from("root notes"));
            Ok(())
        })
    });
    let root = harness
        .root(
            context,
            RootOptions {
                agent: Some(AgentChange {
                    thinking_level: Some(Some(ModelThinkingLevel::Low)),
                    ..Default::default()
                }),
                init: Some(init),
            },
        )
        .await?;

    let root_notes = harness
        .snapshot(&DocTarget::conversation(&notes, root.id()), context)
        .await?;
    println!(
        "root: {} {}",
        root.id(),
        serde_json::to_string(&root_notes)?
    );

    // 保存的是名字；`agent()` 把它们对着 registry 解析。
    let stored_agent = harness
        .snapshot(&DocTarget::conversation(&AGENT_DOC, root.id()), context)
        .await?;
    println!("stored agent: {}", serde_json::to_string(&stored_agent)?);
    let agent = root.agent(context).await?;
    println!(
        "resolved: {:?} {:?} {:?}",
        agent.thinking_level,
        agent
            .extensions
            .iter()
            .map(|extension| extension.name().to_owned())
            .collect::<Vec<_>>(),
        agent
            .tools
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect::<Vec<_>>()
    );

    harness.close(context).await;
    Ok(())
}
