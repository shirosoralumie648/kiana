//! `test/suite/regressions/1717-2113-agent-session-event-settlement.test.ts` 的 Rust 移植。

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use kiana_agent_core::types::{
    AgentTool, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxResponseStep, faux_assistant_message,
    faux_tool_call_with,
};
use kiana_ai::types::{AssistantContentBlock, StopReason, UserContentBlock};
use kiana_ext_api::events::{ExtensionEvent, ExtensionEventKind};
use kiana_session::agent_session::PromptOptions;
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

struct EchoTool;

impl AgentToolExecute for EchoTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<AgentToolResult<serde_json::Value>, kiana_agent_core::types::AgentToolError>,
    > {
        Box::pin(async move {
            let text = params
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: text.clone(),
                    text_signature: None,
                })],
                details: json!({ "text": text }),
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn echo_tool() -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        }),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(EchoTool),
    }
}

fn echo_call(text: &str) -> kiana_ai::types::ToolCall {
    let mut arguments = kiana_ai::types::JsonObject::new();
    arguments.insert("text".to_string(), json!(text));
    faux_tool_call_with("echo", arguments, Default::default())
}

fn branch_roles(harness: &Harness) -> Vec<String> {
    harness
        .session_manager()
        .get_branch(None)
        .into_iter()
        .filter_map(|entry| match entry {
            SessionEntry::Message(message) => Some(message.message.role().to_string()),
            _ => None,
        })
        .collect()
}

/// TS: "keeps persisted assistant/toolResult message order when extension message_end handlers yield"
#[tokio::test]
async fn keeps_persisted_assistant_tool_result_message_order_when_message_end_handlers_yield() {
    run(async {
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![echo_tool()]),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::MessageEnd,
                    standard(move |event, _ctx| {
                        Box::pin(async move {
                            if let ExtensionEvent::MessageEnd(event) = event
                                && event.message.role() == "assistant"
                            {
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                FauxContent::Blocks(vec![
                    AssistantContentBlock::ToolCall(echo_call("one")),
                    AssistantContentBlock::ToolCall(echo_call("two")),
                ]),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("run tools".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let roles = branch_roles(&harness);
        assert_eq!(
            roles,
            vec![
                "system",
                "user",
                "assistant",
                "toolResult",
                "toolResult",
                "assistant"
            ]
        );
        let first_tool_result = roles
            .iter()
            .position(|role| role == "toolResult")
            .expect("tool result");
        assert!(first_tool_result > 0);
        assert_eq!(roles[first_tool_result - 1], "assistant");
        harness.cleanup();
    })
    .await;
}

/// TS: "runs tool_call handlers after the assistant tool-use message is settled in the session"
#[tokio::test]
async fn runs_tool_call_handlers_after_the_assistant_tool_use_message_is_settled() {
    run(async {
        let branch_roles_at_tool_call: Arc<Mutex<Vec<Vec<String>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![echo_tool()]),
            extension_factories: vec![factory({
                let branch_roles_at_tool_call = Arc::clone(&branch_roles_at_tool_call);
                move |kiana| {
                    let branch_roles_at_tool_call = Arc::clone(&branch_roles_at_tool_call);
                    let _ = kiana.on(
                        ExtensionEventKind::ToolCall,
                        standard(move |_event, ctx| {
                            let branch_roles_at_tool_call = Arc::clone(&branch_roles_at_tool_call);
                            Box::pin(async move {
                                let roles: Vec<String> = ctx
                                    .session_manager()
                                    .get_branch(None)
                                    .into_iter()
                                    .filter_map(|entry| match entry {
                                        SessionEntry::Message(message) => {
                                            Some(message.message.role().to_string())
                                        }
                                        _ => None,
                                    })
                                    .collect();
                                branch_roles_at_tool_call.lock().unwrap().push(roles);
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                FauxContent::Blocks(vec![AssistantContentBlock::ToolCall(echo_call("hello"))]),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("run tool".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(
            *branch_roles_at_tool_call.lock().unwrap(),
            vec![vec!["system", "user", "assistant"]]
        );
        harness.cleanup();
    })
    .await;
}
