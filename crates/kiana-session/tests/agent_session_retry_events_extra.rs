//! `test/suite/agent-session-retry-events.test.ts` 中剩余用例的 Rust 移植。
//!
//! 已由 `agent_session_retry.rs` 覆盖：延迟 message_end 时 prompt 等待、重试后工具循环等待。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{AgentMessage, AgentTool, AgentToolError, AgentToolExecute};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxResponseStep, faux_assistant_message,
    faux_thinking, faux_tool_call,
};
use kiana_ai::types::{
    AssistantContentBlock, Message, StopReason, TextContent, TextKind, UserContentBlock,
};
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
    base_delay_ms: i64,
    enabled: bool,
) -> kiana_config::settings::schema::Settings {
    settings_from_json(json!({
        "retry": {
            "enabled": enabled,
            "maxRetries": max_retries,
            "baseDelayMs": base_delay_ms
        }
    }))
}

fn normalize_event_order(events: &[AgentSessionEvent]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for event in events {
        let label = match event {
            AgentSessionEvent::MessageStart { message } => {
                format!("message_start:{}", message.role())
            }
            AgentSessionEvent::MessageEnd { message } => {
                format!("message_end:{}", message.role())
            }
            AgentSessionEvent::ToolExecutionStart { tool_name, .. } => {
                format!("tool_execution_start:{tool_name}")
            }
            AgentSessionEvent::ToolExecutionEnd { tool_name, .. } => {
                format!("tool_execution_end:{tool_name}")
            }
            other => event_type_name(other).to_string(),
        };
        if label == "message_update"
            && normalized.last().map(String::as_str) == Some("message_update")
        {
            continue;
        }
        normalized.push(label);
    }
    normalized
}

struct EchoTool {
    runs: Arc<Mutex<Vec<String>>>,
}

impl AgentToolExecute for EchoTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        let text = params
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.runs.lock().unwrap().push(text.clone());
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("echo:{text}"),
                    text_signature: None,
                })],
                details: json!({"text": text}),
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn echo_tool(runs: Arc<Mutex<Vec<String>>>) -> AgentTool {
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
        execute: Arc::new(EchoTool { runs }),
    }
}

fn echo_call(text: &str) -> kiana_ai::types::ToolCall {
    let mut arguments = kiana_ai::types::JsonObject::new();
    arguments.insert("text".to_string(), json!(text));
    faux_tool_call("echo", arguments)
}

/// TS: "does not retry when retry is disabled"
#[tokio::test]
async fn does_not_retry_when_retry_is_disabled() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, 1, false)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![error_step("overloaded_error")]);

        harness
            .session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 1);
        assert!(harness.events_of_type("auto_retry_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "does not retry non-retryable errors"
#[tokio::test]
async fn does_not_retry_non_retryable_errors() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, 1, true)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![error_step("invalid_api_key")]);

        harness
            .session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 1);
        assert!(harness.events_of_type("auto_retry_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "cancels retry sleep when abortRetry is called"
#[tokio::test]
async fn cancels_retry_sleep_when_abort_retry_is_called() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, 100, true)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![error_step("overloaded_error")]);

        let saw_retry_start = Arc::new(tokio::sync::Notify::new());
        let start_sink = Arc::clone(&saw_retry_start);
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::AutoRetryStart { .. } = event {
                start_sink.notify_one();
            }
        }));

        let session = harness.session.clone();
        let prompt = tokio::task::spawn_local(async move {
            session
                .prompt("test".to_string(), PromptOptions::default())
                .await
        });
        saw_retry_start.notified().await;
        harness.session.abort_retry();
        prompt.await.unwrap().unwrap();

        assert!(!harness.session.is_retrying());
        let final_errors: Vec<Option<String>> = harness
            .events_of_type("auto_retry_end")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::AutoRetryEnd { final_error, .. } => Some(final_error),
                _ => None,
            })
            .collect();
        assert!(
            final_errors
                .iter()
                .any(|error| error.as_deref() == Some("Retry cancelled"))
        );
        assert_eq!(harness.faux.state().call_count(), 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "emits extension events before public event subscribers"
#[tokio::test]
async fn emits_extension_events_before_public_event_subscribers() {
    run(async {
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ext_sink = Arc::clone(&order);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let start_sink = Arc::clone(&ext_sink);
                let _ = kiana.on(
                    ExtensionEventKind::MessageStart,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&start_sink);
                        Box::pin(async move {
                            if let ExtensionEvent::MessageStart(start) = &event {
                                sink.lock().unwrap().push(format!(
                                    "extension:message_start:{}",
                                    start.message.role()
                                ));
                            }
                            Ok(None)
                        })
                    }),
                );
                let end_sink = Arc::clone(&ext_sink);
                let _ = kiana.on(
                    ExtensionEventKind::MessageEnd,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&end_sink);
                        Box::pin(async move {
                            if let ExtensionEvent::MessageEnd(end) = &event {
                                sink.lock()
                                    .unwrap()
                                    .push(format!("extension:message_end:{}", end.message.role()));
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let public_sink = Arc::clone(&order);
        let _unsubscribe = harness
            .session
            .subscribe(Arc::new(move |event| match event {
                AgentSessionEvent::MessageStart { message } => public_sink
                    .lock()
                    .unwrap()
                    .push(format!("public:message_start:{}", message.role())),
                AgentSessionEvent::MessageEnd { message } => public_sink
                    .lock()
                    .unwrap()
                    .push(format!("public:message_end:{}", message.role())),
                _ => {}
            }));
        harness.set_responses(vec![faux_text_response("done")]);

        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(
            *order.lock().unwrap(),
            vec![
                "extension:message_start:system",
                "public:message_start:system",
                "extension:message_end:system",
                "public:message_end:system",
                "extension:message_start:user",
                "public:message_start:user",
                "extension:message_end:user",
                "public:message_end:user",
                "extension:message_start:assistant",
                "public:message_start:assistant",
                "extension:message_end:assistant",
                "public:message_end:assistant",
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "emits the expected event order for a single prompt"
#[tokio::test]
async fn emits_expected_event_order_for_a_single_prompt() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello")]);

        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let events = harness.events.lock().unwrap().clone();
        assert_eq!(
            normalize_event_order(&events),
            vec![
                "agent_start",
                "turn_start",
                "message_start:system",
                "message_end:system",
                "message_start:user",
                "message_end:user",
                "message_start:assistant",
                "message_update",
                "message_end:assistant",
                "turn_end",
                "agent_end",
                "agent_settled",
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "emits the expected event order for a tool call turn"
#[tokio::test]
async fn emits_expected_event_order_for_a_tool_call_turn() {
    run(async {
        let runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![echo_tool(Arc::clone(&runs))]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                echo_call("hello"),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);

        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(*runs.lock().unwrap(), vec!["hello".to_string()]);
        let events = harness.events.lock().unwrap().clone();
        assert_eq!(
            normalize_event_order(&events),
            vec![
                "agent_start",
                "turn_start",
                "message_start:system",
                "message_end:system",
                "message_start:user",
                "message_end:user",
                "message_start:assistant",
                "message_update",
                "message_end:assistant",
                "tool_execution_start:echo",
                "tool_execution_end:echo",
                "message_start:toolResult",
                "message_end:toolResult",
                "turn_end",
                "turn_start",
                "message_start:assistant",
                "message_update",
                "message_end:assistant",
                "turn_end",
                "agent_end",
                "agent_settled",
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "emits streaming deltas for text, thinking, and tool calls in message_update events"
#[tokio::test]
async fn emits_streaming_deltas_for_text_thinking_and_tool_calls() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxContent::Blocks(vec![
                AssistantContentBlock::Thinking(faux_thinking("plan")),
                AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "answer".to_string(),
                    text_signature: None,
                }),
                AssistantContentBlock::ToolCall(echo_call("hello")),
            ]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        ))]);

        let _ = harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await;

        let update_types: Vec<&'static str> = harness
            .events_of_type("message_update")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } => Some(assistant_message_event.kind()),
                _ => None,
            })
            .collect();
        assert!(update_types.contains(&"thinking_delta"));
        assert!(update_types.contains(&"text_delta"));
        assert!(update_types.contains(&"toolcall_delta"));
        harness.cleanup();
    })
    .await;
}

/// TS: "emits agent_end for error responses"
#[tokio::test]
async fn emits_agent_end_for_error_responses() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![error_step("broken")]);

        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.events_of_type("agent_end").len(), 1);
        let events = harness.events.lock().unwrap();
        assert_eq!(events.last().map(event_type_name), Some("agent_settled"));
        harness.cleanup();
    })
    .await;
}

/// TS: "emits agent_end for aborted runs and persists the aborted assistant message"
#[tokio::test]
async fn emits_agent_end_for_aborted_runs_and_persists_aborted_message() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("x".repeat(20_000))]);

        let saw_update = Arc::new(tokio::sync::Notify::new());
        let sink = Arc::clone(&saw_update);
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::MessageUpdate { .. } = event {
                sink.notify_one();
            }
        }));

        let session = harness.session.clone();
        let prompt = tokio::task::spawn_local(async move {
            session
                .prompt("hi".to_string(), PromptOptions::default())
                .await
        });
        saw_update.notified().await;
        harness.session.abort().await;
        prompt.await.unwrap().unwrap();

        assert_eq!(harness.events_of_type("agent_end").len(), 1);
        let events = harness.events.lock().unwrap();
        assert_eq!(events.last().map(event_type_name), Some("agent_settled"));
        drop(events);
        let last = harness
            .session
            .messages()
            .into_iter()
            .next_back()
            .expect("last message");
        assert_eq!(last.role(), "assistant");
        match last {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                assert_eq!(assistant.stop_reason, StopReason::Aborted);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}
