//! 移植自 `packages/durable/test/examples/14-system-prompt.ts`：系统提示分段与逐会话说明。
//!
//! TS 示例通过 `Harness`/扩展注册表渲染分段；本单元的上层 `harness.rs` 与 registry 尚未实现，
//! 因此这里直接构造 `PromptInput`，调用同一套 [`render_sections`] 与 [`plan_system_entries`]，
//! 再像 agent 那样把计划出的 `kiana.system` 条目写入转录。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::{ModelThinkingLevel, Tool};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::context::read_context;
use kiana_durable::harness::define::section;
use kiana_durable::harness::prompt::{plan_system_entries, render_sections, replay_sections};
use kiana_durable::harness::types::{Agent, ModelRef, PromptInput, PromptSection};
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    BoxError, ConversationId, ConversationOwnership, ConversationRecord, CreateConversationOptions,
    EntryQuery, ScanOrder, Storage, SystemEntry, TypedEntryTx,
};

/// `tag: false` 的静态分段：文本原样发送。
fn static_section(key: &str, text: &str, tag: Option<bool>) -> Arc<dyn PromptSection> {
    let text = text.to_owned();
    section(
        key.to_owned(),
        Arc::new(move |_, _| {
            let text = text.clone();
            Box::pin(async move { Ok(Some(text)) })
        }),
        tag.map(Some),
    )
}

/// 读取本次请求环境工作目录的分段（in this port, the agent's cwd）。
fn cwd_section() -> Arc<dyn PromptSection> {
    section(
        "cwd",
        Arc::new(|input: &PromptInput, _| {
            let cwd = input.agent.cwd.clone();
            Box::pin(async move { Ok(cwd) })
        }),
        None,
    )
}

/// 逐会话的 `instructions`：永远最后渲染。
fn instructions_section() -> Arc<dyn PromptSection> {
    section(
        "instructions",
        Arc::new(|input: &PromptInput, _| {
            let instructions = input.agent.instructions.clone();
            Box::pin(async move { Ok(instructions) })
        }),
        None,
    )
}

/// 一个装饰已有分段的包装器：在原文后追加 " Be terse."。
struct Terse {
    inner: Arc<dyn PromptSection>,
}

#[async_trait]
impl PromptSection for Terse {
    fn key(&self) -> &str {
        self.inner.key()
    }

    async fn render(
        &self,
        input: &PromptInput,
        context: &dyn Context,
    ) -> Result<Option<String>, BoxError> {
        Ok(self
            .inner
            .render(input, context)
            .await?
            .map(|text| format!("{text} Be terse.")))
    }

    fn tag(&self) -> Option<bool> {
        self.inner.tag()
    }
}

fn prompt_agent(cwd: &str, with_agents_md: bool, instructions: Option<&str>) -> Agent {
    let mut sections: Vec<Arc<dyn PromptSection>> = vec![
        // 另一个扩展按 key 装饰分段，而不替换它。
        Arc::new(Terse {
            inner: static_section("preamble", "You are a coding agent.", Some(false)),
        }),
        cwd_section(),
    ];
    if with_agents_md {
        sections.push(static_section(
            "agents_md",
            "Run npm run check after changes.",
            None,
        ));
    }
    if instructions.is_some() {
        sections.push(instructions_section());
    }
    Agent {
        model: Some(ModelRef {
            provider: "faux".to_owned(),
            model_id: "faux-1".to_owned(),
        }),
        thinking_level: ModelThinkingLevel::Off,
        extensions: Vec::new(),
        tools: Vec::new(),
        callable: Vec::new(),
        deferred: Vec::new(),
        sections,
        instructions: instructions.map(str::to_owned),
        cwd: Some(cwd.to_owned()),
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

fn prompt_input(
    conversation_id: ConversationId,
    agent: Agent,
    shown: IndexMap<String, String>,
    session: &SessionImpl,
) -> PromptInput {
    PromptInput {
        conversation_id,
        agent: Arc::new(agent),
        env: None,
        shown,
        read: Arc::new(session.clone()),
    }
}

async fn append_planned(
    session: &SessionImpl,
    conversation_id: ConversationId,
    planned: Vec<kiana_durable::harness::prompt::SystemDraft>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    for draft in planned {
        session
            .commit_with(
                move |tx| {
                    let draft = draft.clone();
                    Box::pin(async move {
                        tx.append_entry_typed(&SystemEntry, conversation_id, draft)
                            .await
                            .map(|_| ())
                    })
                },
                context,
                Default::default(),
            )
            .await?;
    }
    Ok(())
}

fn system_prompt(view: &kiana_durable::harness::types::ContextView) -> Vec<String> {
    view.messages
        .iter()
        .filter_map(|message| match message {
            kiana_ai::Message::System(system) => Some(format!(
                "system: {}",
                serde_json::to_string(&system.sections).unwrap_or_else(|_| "null".to_owned())
            )),
            _ => None,
        })
        .collect()
}

fn read_tool() -> Tool {
    Tool {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let session = SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default());
    let storage: &dyn Storage = session.storage().as_ref();
    let tools = vec![read_tool()];

    let root = create_ownerless(&session, context).await?;
    let subagent = create_ownerless(&session, context).await?;

    // 根会话：所有分段；子 agent 去掉 agents_md，并把自己的 instructions 最后渲染。
    for (conversation, agent) in [
        (root.id, prompt_agent("/repo", true, None)),
        (
            subagent.id,
            prompt_agent("/repo", false, Some("Only read; never edit files.")),
        ),
    ] {
        let input = prompt_input(conversation, agent, IndexMap::new(), &session);
        let mut report = |error: BoxError, section: &Arc<dyn PromptSection>| {
            eprintln!("section {} failed: {error}", section.key());
        };
        let desired = render_sections(
            &input.agent.sections,
            &input,
            &input.shown,
            &mut report,
            context,
        )
        .await?;
        let view = read_context(&session, storage, conversation, context, None).await?;
        let planned = plan_system_entries(&view, &desired, &tools, 1);
        append_planned(&session, conversation, planned, context).await?;
        let view = read_context(&session, storage, conversation, context, None).await?;
        println!("system prompt: {:?}", system_prompt(&view));
    }

    // 分段输出变化时，下一次请求只追加变化。
    let view = read_context(&session, storage, root.id, context, None).await?;
    let shown = replay_sections(&view.messages);
    let changed = prompt_agent("/repo/packages", true, None);
    let input = prompt_input(root.id, changed, shown, &session);
    let mut report = |error: BoxError, section: &Arc<dyn PromptSection>| {
        eprintln!("section {} failed: {error}", section.key());
    };
    let desired = render_sections(
        &input.agent.sections,
        &input,
        &input.shown,
        &mut report,
        context,
    )
    .await?;
    let planned = plan_system_entries(&view, &desired, &tools, 2);
    println!(
        "planned after cwd change: {:?}",
        planned
            .iter()
            .map(|draft| draft
                .model
                .as_ref()
                .and_then(|messages| messages.first())
                .map(|message| match message {
                    kiana_ai::Message::System(system) => serde_json::to_string(&system.sections)
                        .unwrap_or_else(|_| "null".to_owned()),
                    _ => "null".to_owned(),
                })
                .unwrap_or_else(|| "null".to_owned()))
            .collect::<Vec<_>>()
    );
    append_planned(&session, root.id, planned, context).await?;
    let view = read_context(&session, storage, root.id, context, None).await?;
    println!(
        "root system entries after cwd change: {:?}",
        system_prompt(&view)
    );

    // 存储的条目仍然是逐条补丁：head/edit 之前的内容都还在转录里。
    let stored = storage
        .scan_entries(
            &EntryQuery {
                conversation_id: root.id,
                min_entry_id: None,
                max_entry_id: None,
                order: Some(ScanOrder::Descending),
            },
            100,
            None,
            context,
        )
        .await?;
    let kinds: Vec<&str> = stored
        .items
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    println!("stored kinds: {kinds:?}");

    session.close(context).await;
    Ok(())
}
