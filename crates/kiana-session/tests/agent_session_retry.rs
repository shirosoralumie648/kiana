//! `test/agent-session-retry.test.ts` 的 Rust 移植补充（验收门补齐）。
//!
//! 瞬态错误重试/耗尽两个用例已由 `agent_session_retry_events.rs` 覆盖；本文件补齐：
//! 延迟上限、message_end 延迟时 prompt 等待、network_error 分类、重试后工具循环等待。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use kiana_agent_core::types::{AgentTool, AgentToolError, AgentToolExecute};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{StopReason, UserContentBlock};
use kiana_ext_api::events::{ExtensionEvent, ExtensionEventKind};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

fn error_step(message: &str) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(message.to_string()),
            ..Default::default()
        },
    ))
}

fn retry_settings(
    max_retries: i64,
    max_agent_delay_ms: Option<i64>,
) -> kiana_config::settings::schema::Settings {
    settings_from_json(json!({
        "retry": {
            "enabled": true,
            "maxRetries": max_retries,
            "baseDelayMs": 1,
            "maxAgentDelayMs": max_agent_delay_ms.unwrap_or(60000)
        }
    }))
}

struct EchoTool {
    executed: Arc<AtomicUsize>,
}

impl AgentToolExecute for EchoTool {
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
        self.executed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "echoed".to_string(),
                    text_signature: None,
                })],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn echo_tool(executed: Arc<AtomicUsize>) -> AgentTool {
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
        execute: Arc::new(EchoTool { executed }),
    }
}

fn retry_events(harness: &Harness) -> Vec<String> {
    harness
        .events_of_type("auto_retry_start")
        .into_iter()
        .chain(harness.events_of_type("auto_retry_end"))
        .filter_map(|event| match event {
            AgentSessionEvent::AutoRetryStart { attempt, .. } => Some(format!("start:{attempt}")),
            AgentSessionEvent::AutoRetryEnd { success, .. } => {
                Some(format!("end:success={success}"))
            }
            _ => None,
        })
        .collect()
}

/// TS: "caps agent retry delay"（#8826）
#[tokio::test]
async fn caps_agent_retry_delay() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(5, Some(5))),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            error_step("overloaded_error"),
            error_step("overloaded_error"),
            error_step("overloaded_error"),
            error_step("overloaded_error"),
            faux_text_response("Success"),
        ]);
        let delays: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&delays);
        let _ = harness.session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::AutoRetryStart { delay_ms, .. } = event {
                sink.lock().unwrap().push(*delay_ms);
            }
        }));

        harness
            .session
            .prompt("Test".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(*delays.lock().unwrap(), vec![1, 2, 4, 5]);
        assert!(!harness.session.is_retrying());
        harness.cleanup();
    })
    .await;
}

/// TS: "prompt waits for retry completion even when assistant message_end handling is delayed"
#[tokio::test]
async fn prompt_waits_for_retry_completion_when_message_end_is_delayed() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, None)),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::MessageEnd,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            if let ExtensionEvent::MessageEnd(end) = &event
                                && end.message.role() == "assistant"
                            {
                                tokio::time::sleep(Duration::from_millis(40)).await;
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
            error_step("overloaded_error"),
            faux_text_response("Success"),
        ]);

        harness
            .session
            .prompt("Test".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 2);
        assert!(!harness.session.is_retrying());
        harness.cleanup();
    })
    .await;
}

/// TS: "retries provider network_error failures"
#[tokio::test]
async fn retries_provider_network_error_failures() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, None)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            error_step("Provider finish_reason: network_error"),
            faux_text_response("Recovered after retry"),
        ]);

        harness
            .session
            .prompt("Test".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 2);
        assert_eq!(
            retry_events(&harness),
            vec!["start:1".to_string(), "end:success=true".to_string()]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "prompt waits for full agent loop when retry produces tool calls"
#[tokio::test]
async fn prompt_waits_for_full_agent_loop_when_retry_produces_tool_calls() {
    run(async {
        let executed = Arc::new(AtomicUsize::new(0));
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, None)),
            tools: Some(vec![echo_tool(Arc::clone(&executed))]),
            ..Default::default()
        })
        .await;
        let mut tool_call_arguments = kiana_ai::types::JsonObject::new();
        tool_call_arguments.insert("text".to_string(), json!("hello"));
        let tool_call = kiana_ai::providers::faux::faux_tool_call("echo", tool_call_arguments);
        harness.set_responses(vec![
            error_step("overloaded_error"),
            FauxResponseStep::Message(faux_assistant_message(
                kiana_ai::providers::faux::FauxContent::Blocks(vec![
                    kiana_ai::types::AssistantContentBlock::Text(kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "Looking that up now.".to_string(),
                        text_signature: None,
                    }),
                    kiana_ai::types::AssistantContentBlock::ToolCall(tool_call),
                ]),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("Final answer."),
            faux_text_response("Follow-up answer."),
        ]);

        harness
            .session
            .prompt("Test".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 3);
        assert_eq!(executed.load(Ordering::SeqCst), 1);
        assert!(!harness.session.is_streaming());
        harness
            .session
            .prompt("Follow-up".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(harness.faux.state().call_count(), 4);
        harness.cleanup();
    })
    .await;
}
