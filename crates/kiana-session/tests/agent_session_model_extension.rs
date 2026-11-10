//! `test/suite/agent-session-model-extension.test.ts` 的 Rust 移植（验收门补齐）。
//!
//! `getSystemPromptOptions` 的 TS 用例断言「两次调用返回同一个对象且就地修改可见」；Rust 的
//! `ExtensionCommandContextApi` 返回按值 clone 的 `BuildSystemPromptOptions`（跨 crate 契约），
//! 无法表达对象身份，因此该用例只断言内容（cwd/selectedTools），不复制身份语义。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{AgentTool, AgentToolError, AgentToolExecute};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{
    Message, StopReason, TextContent, TextKind, Usage, UsageCost, UserContent, UserContentBlock,
};
use kiana_ext_api::events::{
    BeforeAgentStartEventResult, ContextEventResult, ExtensionEvent, ExtensionEventKind,
    ExtensionEventResult, InputEventResult, OptionalPatch, ToolCallEventResult,
    ToolResultEventResult,
};
use kiana_ext_api::types::{CustomMessagePayload, RegisterCommandOptions};
use kiana_session::agent_session::{ModelMutationOptions, PromptOptions, ScopedModel};
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

/// 本文件的用例必须串行：`AgentSession::reload()` 会调用进程级
/// `resetApiProviders()`（TS 语义），并行执行会清掉其它用例刚注册的 faux api provider。
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn run<F: std::future::Future>(future: F) -> F::Output {
    let _guard = TEST_LOCK.lock().await;
    common::run(future).await
}

fn usage(input: f64, output: f64, total: f64) -> Usage {
    Usage {
        input: input as u64,
        output: output as u64,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: total as u64,
        cost: UsageCost {
            input,
            output,
            cache_read: 0.0,
            cache_write: 0.0,
            total,
            total_from_provider: None,
        },
    }
}

use kiana_agent_core::types::AgentMessage;

/// 自定义工具：可指定 result usage。
struct UsageTool {
    text: String,
    usage: Option<Usage>,
}

impl AgentToolExecute for UsageTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        let text = self.text.clone();
        let usage = self.usage.clone();
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text,
                    text_signature: None,
                })],
                details: json!({"text": "hello"}),
                structured_content: None,
                usage,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn echo_tool(usage: Option<Usage>) -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(UsageTool {
            text: "hello".to_string(),
            usage,
        }),
    }
}

fn tool_result_text(context: &kiana_ai::types::Context) -> String {
    context
        .messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(
                result
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn provider_user_text(context: &kiana_ai::types::Context) -> String {
    context
        .messages
        .iter()
        .find_map(|message| match message {
            Message::User(user) => Some(match &user.content {
                UserContent::Text(text) => text.clone(),
                UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            }),
            _ => None,
        })
        .unwrap_or_default()
}

fn model_event_text(event: &ExtensionEvent) -> Option<String> {
    let ExtensionEvent::ModelSelect(select) = event else {
        return None;
    };
    Some(format!(
        "{}->{}:{:?}",
        select
            .previous_model
            .as_ref()
            .map(|model| model.id.as_str())
            .unwrap_or("none"),
        select.model.id,
        select.source
    ))
}

// ============================================================================
// 用例
// ============================================================================

/// TS: "setModel saves the model to the session and emits model_select"
#[tokio::test]
async fn set_model_saves_the_model_and_emits_model_select() {
    run(async {
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    name: Some("One".to_string()),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    name: Some("Two".to_string()),
                    reasoning: Some(true),
                    ..Default::default()
                },
            ]),
            extension_factories: vec![factory(move |kiana| {
                let sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::ModelSelect,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        Box::pin(async move {
                            if let Some(text) = model_event_text(&event) {
                                sink.lock().unwrap().push(text);
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let next_model = harness.get_model(Some("faux-2")).expect("faux-2");

        harness
            .session
            .set_model(next_model.clone(), ModelMutationOptions::default())
            .await
            .unwrap();

        assert_eq!(
            harness.session.model().map(|m| m.id),
            Some("faux-2".to_string())
        );
        assert_eq!(
            *events.lock().unwrap(),
            vec!["faux-1->faux-2:Set".to_string()]
        );
        let model_changes: Vec<String> = harness
            .session_entries()
            .into_iter()
            .filter_map(|entry| match entry {
                SessionEntry::ModelChange(entry) => {
                    Some(format!("{}/{}", entry.provider, entry.model_id))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            model_changes,
            vec![format!("{}/{}", next_model.provider, next_model.id)]
        );
        assert_eq!(harness.settings_manager.get_default_provider(), None);
        assert_eq!(harness.settings_manager.get_default_model(), None);
        harness.cleanup();
    })
    .await;
}

/// TS: "only persists model and thinking defaults when requested"
#[tokio::test]
async fn only_persists_model_and_thinking_defaults_when_requested() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        })
        .await;
        let next_model = harness.get_model(Some("faux-2")).expect("faux-2");

        harness
            .session
            .set_model(next_model.clone(), ModelMutationOptions::default())
            .await
            .unwrap();
        assert_eq!(harness.settings_manager.get_default_provider(), None);
        assert_eq!(harness.settings_manager.get_default_model(), None);

        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::Low,
            ModelMutationOptions::default(),
        );
        assert_eq!(harness.settings_manager.get_default_thinking_level(), None);

        harness
            .session
            .set_model(
                next_model.clone(),
                ModelMutationOptions {
                    persist: Some(true),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            harness.settings_manager.get_default_provider(),
            Some(next_model.provider.clone())
        );
        assert_eq!(
            harness.settings_manager.get_default_model(),
            Some(next_model.id.clone())
        );

        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::High,
            ModelMutationOptions {
                persist: Some(true),
            },
        );
        assert_eq!(
            harness.settings_manager.get_default_thinking_level(),
            Some(kiana_ai::types::ModelThinkingLevel::High)
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "persists the requested default thinking level even when the current model clamps it"
#[tokio::test]
async fn persists_requested_default_thinking_level_even_when_clamped() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                reasoning: Some(true),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await;

        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::Max,
            ModelMutationOptions {
                persist: Some(true),
            },
        );

        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::High
        );
        assert_eq!(
            harness.settings_manager.get_default_thinking_level(),
            Some(kiana_ai::types::ModelThinkingLevel::Max)
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "cycleModel and cycleThinkingLevel are session-only by default"
#[tokio::test]
async fn cycle_model_and_cycle_thinking_level_are_session_only_by_default() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
            ]),
            settings: Some(settings_from_json(json!({
                "defaultProvider": "faux",
                "defaultModel": "faux-1",
                "defaultThinkingLevel": "low"
            }))),
            ..Default::default()
        })
        .await;

        harness
            .session
            .cycle_model("forward", ModelMutationOptions::default())
            .await
            .unwrap();
        assert_eq!(
            harness.session.model().map(|m| m.id),
            Some("faux-2".to_string())
        );
        assert_eq!(
            harness.settings_manager.get_default_model(),
            Some("faux-1".to_string())
        );

        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::Off,
            ModelMutationOptions::default(),
        );
        assert_eq!(
            harness
                .session
                .cycle_thinking_level(ModelMutationOptions::default()),
            Some(kiana_ai::types::ModelThinkingLevel::Minimal)
        );
        assert_eq!(
            harness.settings_manager.get_default_thinking_level(),
            Some(kiana_ai::types::ModelThinkingLevel::Low)
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "applies per-model thinking level override on model switch"
#[tokio::test]
async fn applies_per_model_thinking_level_override_on_model_switch() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
            ]),
            settings: Some(settings_from_json(json!({
                "defaultThinkingLevel": "medium"
            }))),
            ..Default::default()
        })
        .await;
        let _ = harness.settings_manager.set_model_thinking_level(
            "faux",
            "faux-2",
            kiana_ai::types::ModelThinkingLevel::Low,
        );

        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::High,
            ModelMutationOptions::default(),
        );
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::High
        );

        let model2 = harness.get_model(Some("faux-2")).expect("faux-2");
        harness
            .session
            .set_model(model2, ModelMutationOptions::default())
            .await
            .unwrap();
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::Low
        );

        let model1 = harness.get_model(Some("faux-1")).expect("faux-1");
        harness
            .session
            .set_model(model1, ModelMutationOptions::default())
            .await
            .unwrap();
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::Medium
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "falls back to current session thinking level when no per-model or global default is configured"
#[tokio::test]
async fn falls_back_to_current_session_thinking_level() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        })
        .await;
        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::High,
            ModelMutationOptions::default(),
        );
        harness
            .session
            .set_model(
                harness.get_model(Some("faux-2")).expect("faux-2"),
                ModelMutationOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::High
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "per-model override takes priority over global default during model switch"
#[tokio::test]
async fn per_model_override_takes_priority_over_global_default() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
            ]),
            settings: Some(settings_from_json(json!({
                "defaultThinkingLevel": "high",
                "modelThinkingLevels": { "faux/faux-2": "minimal" }
            }))),
            ..Default::default()
        })
        .await;

        harness
            .session
            .set_model(
                harness.get_model(Some("faux-2")).expect("faux-2"),
                ModelMutationOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::Minimal
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "cycles through scoped models and preserves the scoped thinking preference"
#[tokio::test]
async fn cycles_through_scoped_models_and_preserves_thinking_preference() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    reasoning: Some(false),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        })
        .await;
        let model_one = harness.get_model(Some("faux-1")).expect("faux-1");
        let model_two = harness.get_model(Some("faux-2")).expect("faux-2");
        harness.session.set_scoped_models(vec![
            ScopedModel {
                model: model_one,
                thinking_level: Some(kiana_ai::types::ModelThinkingLevel::High),
            },
            ScopedModel {
                model: model_two,
                thinking_level: None,
            },
        ]);
        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::High,
            ModelMutationOptions::default(),
        );

        harness
            .session
            .cycle_model("forward", ModelMutationOptions::default())
            .await
            .unwrap();
        assert_eq!(
            harness.session.model().map(|m| m.id),
            Some("faux-2".to_string())
        );
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::Off
        );

        harness
            .session
            .cycle_model("forward", ModelMutationOptions::default())
            .await
            .unwrap();
        assert_eq!(
            harness.session.model().map(|m| m.id),
            Some("faux-1".to_string())
        );
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::High
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "clamps thinking levels to model capabilities and cycles available levels"
#[tokio::test]
async fn clamps_thinking_levels_to_model_capabilities() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                reasoning: Some(false),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await;
        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::High,
            ModelMutationOptions::default(),
        );
        assert_eq!(
            harness.session.thinking_level(),
            kiana_ai::types::ModelThinkingLevel::Off
        );
        assert_eq!(
            harness
                .session
                .cycle_thinking_level(ModelMutationOptions::default()),
            None
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "cycles xhigh before max when both are supported"
#[tokio::test]
async fn cycles_xhigh_before_max_when_both_are_supported() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                reasoning: Some(true),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await;
        let mut model = harness.get_model(None).expect("model");
        let mut map = indexmap::IndexMap::new();
        map.insert(
            kiana_ai::types::ModelThinkingLevel::Xhigh,
            Some("xhigh".to_string()),
        );
        map.insert(
            kiana_ai::types::ModelThinkingLevel::Max,
            Some("max".to_string()),
        );
        model.thinking_level_map = Some(map);
        harness.agent.set_model(model);

        assert_eq!(
            harness.session.get_available_thinking_levels(),
            vec![
                kiana_ai::types::ModelThinkingLevel::Off,
                kiana_ai::types::ModelThinkingLevel::Minimal,
                kiana_ai::types::ModelThinkingLevel::Low,
                kiana_ai::types::ModelThinkingLevel::Medium,
                kiana_ai::types::ModelThinkingLevel::High,
                kiana_ai::types::ModelThinkingLevel::Xhigh,
                kiana_ai::types::ModelThinkingLevel::Max,
            ]
        );
        harness.session.set_thinking_level(
            kiana_ai::types::ModelThinkingLevel::High,
            ModelMutationOptions::default(),
        );
        assert_eq!(
            harness
                .session
                .cycle_thinking_level(ModelMutationOptions::default()),
            Some(kiana_ai::types::ModelThinkingLevel::Xhigh)
        );
        assert_eq!(
            harness
                .session
                .cycle_thinking_level(ModelMutationOptions::default()),
            Some(kiana_ai::types::ModelThinkingLevel::Max)
        );
        assert_eq!(
            harness
                .session
                .cycle_thinking_level(ModelMutationOptions::default()),
            Some(kiana_ai::types::ModelThinkingLevel::Off)
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "throws when setModel is called without configured auth"
#[tokio::test]
async fn throws_when_set_model_is_called_without_configured_auth() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-1".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
                kiana_ai::providers::faux::FauxModelDefinition {
                    id: "faux-2".to_string(),
                    reasoning: Some(true),
                    ..Default::default()
                },
            ]),
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        let provider = harness.get_model(None).expect("model").provider;
        let error = harness
            .session
            .set_model(
                harness.get_model(Some("faux-2")).expect("faux-2"),
                ModelMutationOptions::default(),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with(&format!("No API key for {provider}/faux-2")),
            "unexpected error: {error}"
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "allows extension tool_call handlers to block tool execution"
#[tokio::test]
async fn allows_extension_tool_call_handlers_to_block_tool_execution() {
    run(async {
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![echo_tool(None)]),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::ToolCall,
                    standard(|_event, _ctx| {
                        Box::pin(async move {
                            Ok(Some(ExtensionEventResult::ToolCall(ToolCallEventResult {
                                block: Some(true),
                                reason: Some("Blocked by test".to_string()),
                                terminate: None,
                            })))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("echo", indexmap::IndexMap::new()),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            FauxResponseStep::message_factory(|input| async move {
                faux_assistant_message(tool_result_text(input.context.as_context()), Default::default())
            }),
        ]);

        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert!(get_assistant_texts(&harness).contains(&"Blocked by test".to_string()));
        assert!(harness.session.messages().iter().any(|message| {
            matches!(message, AgentMessage::Standard(Message::ToolResult(result)) if result.is_error)
        }));
        harness.cleanup();
    })
    .await;
}

/// TS: "allows extension tool_result handlers to modify tool results"
#[tokio::test]
async fn allows_extension_tool_result_handlers_to_modify_tool_results() {
    run(async {
        let tool_usage = usage(1.0, 2.0, 10.0);
        let patched_usage = usage(5.0, 6.0, 26.0);
        let observed: Arc<Mutex<Option<Usage>>> = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&observed);
        let patched = patched_usage.clone();
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![echo_tool(Some(tool_usage.clone()))]),
            extension_factories: vec![factory(move |kiana| {
                let sink = Arc::clone(&sink);
                let patched = patched.clone();
                let _ = kiana.on(
                    ExtensionEventKind::ToolResult,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        let patched = patched.clone();
                        Box::pin(async move {
                            if let ExtensionEvent::ToolResult(
                                kiana_ext_api::events::ToolResultEvent::Custom(result),
                            ) = &event
                            {
                                *sink.lock().unwrap() = result.usage.clone();
                            }
                            Ok(Some(ExtensionEventResult::ToolResult(
                                ToolResultEventResult {
                                    content: OptionalPatch::Value(vec![UserContentBlock::Text(
                                        TextContent {
                                            kind_text: TextKind::Text,
                                            text: "patched result".to_string(),
                                            text_signature: None,
                                        },
                                    )]),
                                    details: OptionalPatch::Value(json!({"patched": true})),
                                    structured_content: OptionalPatch::Absent,
                                    is_error: OptionalPatch::Absent,
                                    usage: OptionalPatch::Value(patched),
                                },
                            )))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("echo", indexmap::IndexMap::new()),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            FauxResponseStep::message_factory(|input| async move {
                faux_assistant_message(
                    tool_result_text(input.context.as_context()),
                    Default::default(),
                )
            }),
        ]);

        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert!(get_assistant_texts(&harness).contains(&"patched result".to_string()));
        let tool_result = harness
            .session
            .messages()
            .into_iter()
            .find_map(|message| match message {
                AgentMessage::Standard(Message::ToolResult(result))
                    if result
                        .details
                        .as_ref()
                        .and_then(|details| details.get("patched"))
                        .and_then(serde_json::Value::as_bool)
                        == Some(true) =>
                {
                    Some(result)
                }
                _ => None,
            })
            .expect("patched tool result");
        assert_eq!(*observed.lock().unwrap(), Some(tool_usage));
        assert_eq!(tool_result.usage, Some(patched_usage));
        harness.cleanup();
    })
    .await;
}

/// TS: "allows extension context handlers to modify messages before the LLM call"
#[tokio::test]
async fn allows_extension_context_handlers_to_modify_messages() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::Context,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Context(context) = event else {
                                return Ok(None);
                            };
                            let messages = context
                                .messages
                                .into_iter()
                                .map(|message| match message {
                                    AgentMessage::Standard(Message::User(mut user)) => {
                                        user.content =
                                            UserContent::Blocks(vec![UserContentBlock::Text(
                                                TextContent {
                                                    kind_text: TextKind::Text,
                                                    text: "rewritten".to_string(),
                                                    text_signature: None,
                                                },
                                            )]);
                                        AgentMessage::Standard(Message::User(user))
                                    }
                                    other => other,
                                })
                                .collect();
                            Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                                messages: Some(messages),
                            })))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let seen: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&seen);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let text = provider_user_text(input.context.as_context());
            async move {
                *sink.lock().unwrap() = text;
                faux_assistant_message("done", Default::default())
            }
        })]);

        harness
            .session
            .prompt("original".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), "rewritten");
        let stored_user = harness
            .session
            .messages()
            .into_iter()
            .find(|message| message.role() == "user")
            .expect("stored user message");
        assert_eq!(
            get_message_text(&stored_user),
            "original",
            "session must keep the original message"
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "allows extension input handlers to transform or handle input"
#[tokio::test]
async fn allows_extension_input_handlers_to_transform_or_handle_input() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::Input,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Input(input) = event else {
                                return Ok(None);
                            };
                            if input.text == "ping" {
                                return Ok(Some(ExtensionEventResult::Input(
                                    InputEventResult::Handled,
                                )));
                            }
                            Ok(Some(ExtensionEventResult::Input(
                                InputEventResult::Transform {
                                    text: format!("transformed:{}", input.text),
                                    images: None,
                                },
                            )))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let seen: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&seen);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let text = provider_user_text(input.context.as_context());
            async move {
                *sink.lock().unwrap() = text;
                faux_assistant_message("done", Default::default())
            }
        })]);

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("ping".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), "transformed:hello");
        let user_messages = harness
            .session
            .messages()
            .into_iter()
            .filter(|message| message.role() == "user")
            .count();
        assert_eq!(user_messages, 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "allows extension commands to inspect live system prompt options"
///
/// 偏差：TS 断言两次调用返回同一对象且就地修改在第二次可见；Rust 的
/// `ExtensionCommandContextApi::get_system_prompt_options` 按值返回，无法表达对象身份，
/// 这里只断言内容。
#[tokio::test]
async fn allows_extension_commands_to_inspect_system_prompt_options() {
    run(async {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let sink = Arc::clone(&sink);
                kiana.register_command(
                    "inspect-options",
                    RegisterCommandOptions {
                        description: Some("Inspect system prompt options".to_string()),
                        get_argument_completions: None,
                        handler: std::sync::Arc::new(move |_args, ctx| {
                            let sink = Arc::clone(&sink);
                            Box::pin(async move {
                                let options = ctx.get_system_prompt_options();
                                sink.lock().unwrap().push(format!(
                                    "{}|{}",
                                    options.cwd,
                                    options.selected_tools.clone().unwrap_or_default().join(",")
                                ));
                            })
                        }),
                    },
                );
            })],
            ..Default::default()
        })
        .await;

        harness
            .session
            .prompt("/inspect-options".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("/inspect-options".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let cwd = harness.temp_dir.path().to_string_lossy().to_string();
        assert!(seen[0].starts_with(&format!("{cwd}|")));
        assert!(seen[0].contains("read"));
        assert!(seen[1].contains("read"));
        harness.cleanup();
    })
    .await;
}

/// TS: "allows before_agent_start handlers to inject custom messages and modify the system prompt"
#[tokio::test]
async fn allows_before_agent_start_handlers_to_inject_and_modify_prompt() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::BeforeAgentStart,
                    standard(|event, _ctx| {
                        let system_prompt = match &event {
                            ExtensionEvent::BeforeAgentStart(
                                kiana_ext_api::events::BeforeAgentStartEvent::User(user),
                            ) => user.system_prompt.clone(),
                            ExtensionEvent::BeforeAgentStart(
                                kiana_ext_api::events::BeforeAgentStartEvent::Custom(custom),
                            ) => custom.system_prompt.clone(),
                            _ => String::new(),
                        };
                        Box::pin(async move {
                            Ok(Some(ExtensionEventResult::BeforeAgentStart(
                                BeforeAgentStartEventResult {
                                    message: Some(CustomMessagePayload {
                                        custom_type: "before-start".to_string(),
                                        content: UserContent::Text("injected".to_string()),
                                        display: true,
                                        details: Some(json!({"injected": true})),
                                    }),
                                    system_prompt: Some(format!(
                                        "{system_prompt}\n\nextra instructions"
                                    )),
                                },
                            )))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let captured: Arc<Mutex<(String, bool)>> = Arc::new(Mutex::new((String::new(), false)));
        let sink = Arc::clone(&captured);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let context = input.context.as_context().clone();
            async move {
                let prompt =
                    kiana_ai::utils::transcript::get_current_system_prompt(&context.messages);
                let saw_injected = context.messages.iter().any(|message| {
                    message.role() == "user"
                        && match message {
                            Message::User(user) => match &user.content {
                                UserContent::Text(text) => text.contains("injected"),
                                UserContent::Blocks(blocks) => {
                                    blocks.iter().any(|block| match block {
                                        UserContentBlock::Text(text) => text.text == "injected",
                                        _ => false,
                                    })
                                }
                            },
                            _ => false,
                        }
                });
                *sink.lock().unwrap() = (prompt, saw_injected);
                faux_assistant_message("done", Default::default())
            }
        })]);

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let (prompt, saw_injected) = captured.lock().unwrap().clone();
        assert!(prompt.contains("extra instructions"), "prompt: {prompt}");
        assert!(saw_injected);
        assert!(harness.session.messages().iter().any(|message| {
            message.role() == "custom" && {
                match message {
                    AgentMessage::Custom(object) => {
                        object.get("customType").and_then(serde_json::Value::as_str)
                            == Some("before-start")
                    }
                    _ => false,
                }
            }
        }));
        harness.cleanup();
    })
    .await;
}

/// TS: "bindExtensions emits session_start and reload emits session_shutdown then session_start"
#[tokio::test]
async fn bind_extensions_and_reload_emit_lifecycle_events() {
    run(async {
        let lifecycle: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lifecycle);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let start_sink = Arc::clone(&sink);
                let shutdown_sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::SessionStart,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&start_sink);
                        Box::pin(async move {
                            if let ExtensionEvent::Session(
                                kiana_ext_api::events::SessionEvent::Start(start),
                            ) = event
                            {
                                sink.lock()
                                    .unwrap()
                                    .push(format!("start:{:?}", start.reason));
                            }
                            Ok(None)
                        })
                    }),
                );
                let _ = kiana.on(
                    ExtensionEventKind::SessionShutdown,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&shutdown_sink);
                        Box::pin(async move {
                            if let ExtensionEvent::Session(
                                kiana_ext_api::events::SessionEvent::Shutdown(shutdown),
                            ) = event
                            {
                                sink.lock()
                                    .unwrap()
                                    .push(format!("shutdown:{:?}", shutdown.reason));
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;

        harness
            .session
            .bind_extensions(kiana_session::agent_session::ExtensionBindings {
                shutdown_handler: Some(Arc::new(|| {})),
                ..Default::default()
            })
            .await;
        harness.session.reload(None).await.unwrap();

        let lifecycle = lifecycle.lock().unwrap().clone();
        assert_eq!(
            lifecycle,
            vec![
                "start:Startup".to_string(),
                "shutdown:Reload".to_string(),
                "start:Reload".to_string(),
            ]
        );
        harness.cleanup();
    })
    .await;
}
