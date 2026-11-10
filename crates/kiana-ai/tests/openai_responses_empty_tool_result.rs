//! `openai-responses-empty-tool-result.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses_shared::convert_responses_messages;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, ToolCall, ToolResultMessage,
};
use serde_json::json;

fn empty_tool_result(tool_call_id: &str, timestamp: i64, text: &str) -> ToolResultMessage {
    ToolResultMessage {
        role: Default::default(),
        tool_call_id: tool_call_id.into(),
        tool_name: "bash".into(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                text: text.into(),
                ..Default::default()
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp,
        duration_ms: None,
    }
}

#[test]
fn uses_no_tool_output_placeholder_for_whitespace_only_tool_results() {
    for text in ["", "\r\n", " \n\t"] {
        let model = mock_http::catalog_model("openai", "gpt-4o-mini");
        let assistant = AssistantMessage {
            role: Default::default(),
            content: vec![AssistantContentBlock::ToolCall(ToolCall {
                id: "tool-1".into(),
                name: "bash".into(),
                arguments: [("command".to_string(), json!("true"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            })],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            stop_reason: kiana_ai::types::StopReason::ToolUse,
            timestamp: 1,
            ..Default::default()
        };
        let context = mock_http::context(vec![
            mock_http::user("Run the command"),
            Message::Assistant(assistant),
            Message::ToolResult(empty_tool_result("tool-1", 2, text)),
        ]);
        let input = convert_responses_messages(
            &model,
            &context,
            &["openai", "openai-codex", "opencode"],
            None,
        )
        .expect("convert");
        let output = input
            .iter()
            .find(|item| item["type"] == json!("function_call_output"))
            .and_then(|item| item["output"].as_str())
            .expect("function_call_output");
        assert_eq!(output, "(no tool output)", "{text:?}");
    }
}
