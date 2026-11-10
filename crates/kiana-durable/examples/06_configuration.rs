//! 移植自 `packages/durable/test/examples/06-configuration.ts`：每个会话的 agent 选择与 Harness 级设置。
//!
//! TS 示例通过 `Harness`/`configure()` 演示；本单元的上层 `harness.rs` 尚未实现，因此这里按现有
//! 公开面直接使用 `SessionImpl` + [`kiana_durable::harness::agent`] 的同一套 `kiana.agent` 文档与设置解析。
//! 注册表（extension 选择的应用）也是上层单元；这里展示选择被持久化的形状与逐字段的设置合并。

use std::sync::Arc;

use kiana_ai::{ModelThinkingLevel, Tool};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::{AGENT_DOC, configure, resolve_settings};
use kiana_durable::harness::define::define_extension;
use kiana_durable::harness::types::{
    AgentChange, ConversationStreamOptions, Extension, ExtensionChange, HarnessSettings, ModelRef,
    ProgressSettings, RetrySettings, ToolChange, ToolExecutionMode, ToolRegistration,
};
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    BoxError, ConversationOwnership, ConversationRecord, CreateConversationOptions, DocTarget,
};

/// 一个只带 kiana-ai 字段的示例工具。
fn example_tool(name: &str, description: &str) -> ToolRegistration {
    ToolRegistration::new(Tool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
        constrained_sampling: None,
    })
}

/// `files` 扩展：读与写两个工具。
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

/// `search` 扩展：grep。
struct Search {
    tools: Vec<ToolRegistration>,
}

impl Extension for Search {
    fn name(&self) -> &str {
        "search"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }
}

async fn create_ownerless(
    session: &SessionImpl,
    context: &dyn Context,
) -> Result<ConversationRecord, BoxError> {
    session
        .commit_with(
            |tx| {
                Box::pin(async move {
                    tx.create_conversation(&CreateConversationOptions {
                        ownership: ConversationOwnership::Ownerless,
                    })
                    .await
                })
            },
            context,
            Default::default(),
        )
        .await
}

/// 把一次 `configure()` 提交到 `kiana.agent`。
async fn configure_in_commit(
    session: &SessionImpl,
    conversation_id: kiana_durable::ConversationId,
    change: AgentChange,
    context: &dyn Context,
) -> Result<(), BoxError> {
    session
        .commit_with(
            move |tx| Box::pin(async move { configure(tx, conversation_id, &change).await }),
            context,
            Default::default(),
        )
        .await
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let session = SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default());

    // 每一轮请求都把设置读一遍，所以取值是“活的”，和 TS 里带 getter 的对象一样。
    let settings_for = |timeout_ms: f64| HarnessSettings {
        // 只写部分字段：其余沿用 DEFAULT_* 的对应值（TS: { ...DEFAULT, ...settings }）。
        stream: Some(ConversationStreamOptions {
            timeout_ms: Some(timeout_ms),
            ..Default::default()
        }),
        retry: Some(RetrySettings {
            max_retries: Some(5),
            ..Default::default()
        }),
        progress: Some(ProgressSettings {
            partial_interval_ms: Some(250.0),
            ..Default::default()
        }),
        tool_execution: Some(ToolExecutionMode::Sequential),
        ..Default::default()
    };

    let chat = create_ownerless(&session, context).await?;

    // configure() 把选择存进会话的 kiana.agent 文档；名字按存储值保存，代码消失后仍保留。
    let read = example_tool("read", "Read a file");
    let write = example_tool("write", "Write a file");
    let grep = example_tool("grep", "Search files");
    let files = define_extension(Files {
        tools: vec![read.clone(), write.clone()],
    });
    let search = define_extension(Search {
        tools: vec![grep.clone()],
    });

    configure_in_commit(
        &session,
        chat.id,
        AgentChange {
            model: Some(Some(ModelRef {
                provider: "anthropic".to_owned(),
                model_id: "claude-sonnet-4-5".to_owned(),
            })),
            thinking_level: Some(Some(ModelThinkingLevel::High)),
            tools: Some(ToolChange::Set(vec![write.clone(), read.clone()])),
            extensions: Some(ExtensionChange::Edit {
                add: Some(vec![Arc::clone(&files), Arc::clone(&search)]),
                remove: None,
            }),
            ..Default::default()
        },
        context,
    )
    .await?;
    let stored = session
        .snapshot(&DocTarget::conversation(&AGENT_DOC, chat.id), context)
        .await?;
    println!("stored: {}", serde_json::to_string(&stored)?);

    // 取消选择一个扩展；`null` 把已存字段清回宿主默认（tools / modelTools 清空）。
    configure_in_commit(
        &session,
        chat.id,
        AgentChange {
            extensions: Some(ExtensionChange::Edit {
                add: None,
                remove: Some(vec![Arc::clone(&search)]),
            }),
            tools: Some(ToolChange::Clear),
            ..Default::default()
        },
        context,
    )
    .await?;
    let stored = session
        .snapshot(&DocTarget::conversation(&AGENT_DOC, chat.id), context)
        .await?;
    println!("without search: {}", serde_json::to_string(&stored)?);

    // 选择按名字存储，卸载扩展不会改写存储值。
    configure_in_commit(
        &session,
        chat.id,
        AgentChange {
            extensions: Some(ExtensionChange::Set(vec![
                Arc::clone(&files),
                Arc::clone(&search),
            ])),
            ..Default::default()
        },
        context,
    )
    .await?;
    let stored = session
        .snapshot(&DocTarget::conversation(&AGENT_DOC, chat.id), context)
        .await?;
    println!(
        "exactly files and search: {}",
        serde_json::to_string(&stored)?
    );

    // 设置变化不需要提交；下一次解析就是新值。
    let timeout_ms = 60_000.0;
    let resolved = resolve_settings(Some(&settings_for(timeout_ms)));
    println!(
        "timeout: {:?} retry: enabled={} maxRetries={} baseDelayMs={} toolExecution={:?}",
        resolved.stream.timeout_ms,
        resolved.retry.enabled,
        resolved.retry.max_retries,
        resolved.retry.base_delay_ms,
        resolved.tool_execution
    );
    let timeout_ms = 120_000.0;
    let resolved = resolve_settings(Some(&settings_for(timeout_ms)));
    println!("timeout after change: {:?}", resolved.stream.timeout_ms);

    session.close(context).await;
    Ok(())
}
