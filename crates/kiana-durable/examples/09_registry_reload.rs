//! 移植自 `packages/durable/test/examples/09-registry-reload.ts`：通过 registry 重载扩展代码。

use std::sync::Arc;

use kiana_ai::models::{Models, create_models};
use kiana_ai::types::{TextContent, TextKind, Tool, UserContentBlock};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::define::{define_extension, define_tool, wrap_tool};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    Conversation as ConversationTrait, Extension, HarnessOptions, ModelsProvider, Registry,
    RegistryReader, RootOptions, ToolExecuteFn, ToolExecutionResult, ToolRegistration, Wrap,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{BoxError, JsonValue, Session};

fn example_tool(name: &str, description: &str) -> ToolRegistration {
    let tool_name = name.to_owned();
    let execute: ToolExecuteFn = Arc::new(move |arguments: JsonValue, _api, _context| {
        let path = arguments
            .get("path")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        let tool_name = tool_name.clone();
        Box::pin(async move {
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("{tool_name} {path}"),
                    text_signature: None,
                })]),
                ..Default::default()
            })
        })
    });
    let mut tool = ToolRegistration::new(Tool {
        name: name.to_owned(),
        description: description.to_owned(),
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

/// 按名字包装 `read` 的 `audit` 扩展。
struct Audit {
    wraps: Vec<Wrap>,
}

impl Extension for Audit {
    fn name(&self) -> &str {
        "audit"
    }

    fn wraps(&self) -> Vec<Wrap> {
        self.wraps.clone()
    }
}

async fn tool_lines(
    conversation: &dyn ConversationTrait,
    context: &dyn Context,
) -> Result<Vec<String>, BoxError> {
    let agent = conversation.agent(context).await?;
    Ok(agent
        .tools
        .iter()
        .map(|tool| format!("{}: {}", tool.name(), tool.tool.description))
        .collect())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;

    let read = example_tool("read", "Read a file");
    let registry = create_registry_impl();
    registry.install(define_extension(Files {
        tools: vec![read.clone(), example_tool("grep", "Search files")],
    }));
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
    let root = harness.root(context, RootOptions::default()).await?;

    // 重载就是安装一个同名的新对象：它在一次发布里原位替换旧的，任何会话都不会看到它缺席。
    // 已经开始的任务阶段保留它取到的快照；下一个阶段用新代码。
    registry.install(define_extension(Files {
        tools: vec![read.clone(), example_tool("grep", "Search files, faster")],
    }));
    println!("after reload: {:?}", tool_lines(&*root, context).await?);

    // 另一个扩展可以按名字包装工具。包装在选中该包装扩展的地方生效，并在被包装工具重载后继续生效。
    let audit = Audit {
        wraps: vec![wrap_tool(
            &read,
            Arc::new(|mut tool: ToolRegistration| {
                tool.tool.description = format!("{} (audited)", tool.tool.description);
                tool
            }),
        )],
    };
    registry.install(define_extension(audit));
    println!("with audit: {:?}", tool_lines(&*root, context).await?);

    // 卸载按名字移除扩展；选中它的会话不再拿到它的代码。
    registry.uninstall("audit");
    println!("after uninstall: {:?}", tool_lines(&*root, context).await?);

    harness.close(context).await;
    Ok(())
}
