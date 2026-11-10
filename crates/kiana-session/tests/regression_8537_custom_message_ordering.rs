//! `test/suite/regressions/8537-custom-message-tool-result-ordering.test.ts` 的 Rust 移植。

mod common;

use std::sync::Arc;

use common::*;
use kiana_agent_core::types::{AgentMessage, AgentTool, AgentToolError, AgentToolExecute};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{Message, StopReason, UserContentBlock};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use kiana_session_core::messages::CustomMessage;
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

/// 执行时先通知 started，再等待 release（模拟后台任务在工具执行期间回消息）。
struct SlowTool {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl AgentToolExecute for SlowTool {
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
        self.started.notify_one();
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            release.notified().await;
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "tool done".to_string(),
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

struct SlowToolHandle {
    tool: AgentTool,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

fn slow_tool() -> SlowToolHandle {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait for a background task".to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(SlowTool {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
    };
    SlowToolHandle {
        tool,
        started,
        release,
    }
}

fn custom_message() -> CustomMessage {
    CustomMessage {
        role: kiana_session_core::messages::CustomRole::Custom,
        custom_type: "subagent-reply".to_string(),
        content: json!("subagent replied"),
        display: true,
        details: None,
        timestamp: 0.0,
    }
}

async fn run_custom_message_scenario() -> Harness {
    let handle = slow_tool();
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![handle.tool]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            kiana_ai::providers::faux::faux_tool_call("wait", indexmap::IndexMap::new()),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )),
        faux_text_response("done"),
        faux_text_response("second turn"),
    ]);
    let session = harness.session.clone();
    let prompt = tokio::task::spawn_local(async move {
        session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
    });
    handle.started.notified().await;
    harness
        .session
        .send_custom_message(custom_message(), Some(false), None)
        .await
        .unwrap();
    handle.release.notify_one();
    prompt.await.unwrap().unwrap();
    harness
}

fn roles(messages: &[AgentMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.role().to_string())
        .collect()
}

/// TS: "appends the message after the turn's tool results instead of between call and result"
#[tokio::test]
async fn appends_custom_message_after_tool_results() {
    run(async {
        let harness = run_custom_message_scenario().await;
        assert_eq!(
            roles(&harness.session.messages()),
            vec![
                "system",
                "user",
                "assistant",
                "toolResult",
                "custom",
                "assistant"
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps session entries and message events in the same order as agent state"
#[tokio::test]
async fn keeps_session_entries_and_message_events_in_agent_order() {
    run(async {
        let harness = run_custom_message_scenario().await;
        let entry_kinds: Vec<String> = harness
            .session_manager()
            .get_branch(None)
            .into_iter()
            .flat_map(|entry| match entry {
                SessionEntry::Message(message) => vec![message.message.role().to_string()],
                SessionEntry::CustomMessage(_) => vec!["custom".to_string()],
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(
            entry_kinds,
            vec![
                "system",
                "user",
                "assistant",
                "toolResult",
                "custom",
                "assistant"
            ]
        );
        let message_starts: Vec<String> = harness
            .events_of_type("message_start")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::MessageStart { message } => Some(message.role().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            message_starts,
            vec![
                "system",
                "user",
                "assistant",
                "toolResult",
                "custom",
                "assistant"
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "produces an llm history where every tool result follows its tool call"
#[tokio::test]
async fn produces_llm_history_with_tool_result_after_its_call() {
    run(async {
        let harness = run_custom_message_scenario().await;
        harness
            .session
            .prompt("and now?".to_string(), PromptOptions::default())
            .await
            .unwrap();
        let llm_messages =
            kiana_session_core::messages::convert_to_llm(&harness.session.messages());
        let mut open_tool_call_ids: Vec<String> = Vec::new();
        for message in llm_messages {
            match message {
                Message::Assistant(assistant) => {
                    open_tool_call_ids.clear();
                    for block in assistant.content {
                        if let kiana_ai::types::AssistantContentBlock::ToolCall(call) = block {
                            open_tool_call_ids.push(call.id);
                        }
                    }
                }
                Message::ToolResult(result) => {
                    assert!(
                        open_tool_call_ids.contains(&result.tool_call_id),
                        "tool result {} without open tool call",
                        result.tool_call_id
                    );
                    open_tool_call_ids.retain(|id| id != &result.tool_call_id);
                }
                _ => open_tool_call_ids.clear(),
            }
        }
        harness.cleanup();
    })
    .await;
}
