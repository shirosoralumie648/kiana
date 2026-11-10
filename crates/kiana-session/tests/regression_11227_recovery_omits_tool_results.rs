//! `test/suite/regressions/11227-recovery-omits-tool-results.test.ts` 的 Rust 移植。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxResponseStep, FauxToolCallOptions,
    faux_assistant_message, faux_tool_call_with,
};
use kiana_ai::types::{
    AssistantContentBlock, StopReason, TextContent, TextKind, ToolResultMessage, ToolResultRole,
    Usage, UsageCost, UserContentBlock,
};
use kiana_compaction::compaction::CompactionResult;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, SessionBeforeCompactResult,
    SessionEvent,
};
use kiana_session::agent_session::PromptOptions;

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: UsageCost::default(),
    }
}

fn summary_extension() -> kiana_ext_api::types::ExtensionFactory {
    factory(|kiana| {
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(move |event, _ctx| {
                Box::pin(async move {
                    let ExtensionEvent::Session(SessionEvent::BeforeCompact(event)) = event else {
                        return Ok(None);
                    };
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: "summary".to_string(),
                                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                                tokens_before: event.preparation.tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: Some(serde_json::json!({})),
                            }),
                        },
                    )))
                })
            }),
        );
    })
}

fn text_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

/// TS: "pre-prompt recovery of a resumed length stop omits the truncated call and its result"
#[tokio::test]
async fn pre_prompt_recovery_of_a_resumed_length_stop_omits_the_truncated_call_and_its_result() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(100_000),
                max_tokens: Some(1_000),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(serde_json::json!({
                "compaction": { "enabled": true }
            }))),
            extension_factories: vec![summary_extension()],
            ..Default::default()
        })
        .await;
        let model = harness.get_model(None).expect("model");
        let now = now_ms();

        harness.append_session_message(AgentMessage::Standard(kiana_ai::types::Message::User(
            kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text("check status".to_string()),
                timestamp: now - 1000,
            },
        )));
        let mut arguments = kiana_ai::types::JsonObject::new();
        arguments.insert(
            "command".to_string(),
            serde_json::Value::String("echo".to_string()),
        );
        let mut truncated = faux_assistant_message(
            FauxContent::Blocks(vec![text_block("")]),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                timestamp: Some(now - 500),
                ..Default::default()
            },
        );
        truncated.content = vec![AssistantContentBlock::ToolCall(faux_tool_call_with(
            "bash",
            arguments,
            FauxToolCallOptions {
                id: Some("call_truncated".to_string()),
            },
        ))];
        truncated.api = model.api.clone();
        truncated.provider = model.provider.clone();
        truncated.model = model.id.clone();
        truncated.usage = usage(100, 16);
        harness.append_session_message(AgentMessage::Standard(
            kiana_ai::types::Message::Assistant(truncated),
        ));
        harness.append_session_message(AgentMessage::Standard(
            kiana_ai::types::Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
                tool_call_id: "call_truncated".to_string(),
                tool_name: "bash".to_string(),
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "Not executed: output token limit reached.".to_string(),
                    text_signature: None,
                })],
                details: Some(serde_json::json!({})),
                usage: None,
                nested_calls: None,
                is_error: true,
                duration_ms: None,
                timestamp: now - 400,
            }),
        ));

        let request_messages: Arc<Mutex<Vec<kiana_ai::types::Message>>> = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&request_messages);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let capture = Arc::clone(&capture);
            async move {
                *capture.lock().unwrap() = input.context.as_context().messages.clone();
                faux_assistant_message("done", Default::default())
            }
        })]);

        harness
            .session
            .prompt("continue".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let messages = request_messages.lock().unwrap().clone();
        assert!(!messages.iter().any(|message| message.role() == "toolResult"));
        assert!(!messages.iter().any(|message| {
            matches!(message, kiana_ai::types::Message::Assistant(assistant) if assistant.stop_reason == StopReason::Length)
        }));
        harness.cleanup();
    })
    .await;
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
