//! `openai-responses-foreign-toolcall-id.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses_shared::convert_responses_messages;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, ToolCall, ToolResultMessage, Usage,
};
use kiana_ai::utils::hash::short_hash;

const COPILOT_RAW_TOOL_CALL_ID: &str = "call_4VnzVawQXPB9MgYib7CiQFEY|I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";

#[test]
fn hashes_foreign_copilot_tool_item_ids_into_a_bounded_codex_safe_shape() {
    let model = mock_http::catalog_model("openai-codex", "gpt-6.1-sol");
    let assistant = AssistantMessage {
        role: Default::default(),
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
            id: COPILOT_RAW_TOOL_CALL_ID.into(),
            name: "edit".into(),
            arguments: [("path".to_string(), serde_json::json!("src/styles/app.css"))]
                .into_iter()
                .collect(),
            ..Default::default()
        })],
        api: "openai-responses".into(),
        provider: "github-copilot".into(),
        model: "gpt-5.5".into(),
        usage: Usage::default(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 1,
        ..Default::default()
    };
    let tool_result = ToolResultMessage {
        role: Default::default(),
        tool_call_id: COPILOT_RAW_TOOL_CALL_ID.into(),
        tool_name: "edit".into(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                text: "ok".into(),
                ..Default::default()
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 2,
        duration_ms: None,
    };
    let context = mock_http::context_with(&kiana_ai::types::Context {
        system_prompt: Some("You are concise.".into()),
        messages: vec![
            mock_http::user("Use the tool."),
            Message::Assistant(assistant),
            Message::ToolResult(tool_result),
        ],
        tools: None,
    });
    let input = convert_responses_messages(
        &model,
        &context,
        &["openai", "openai-codex", "opencode"],
        None,
    )
    .expect("convert");
    let function_call = input
        .iter()
        .find(|item| item["type"] == serde_json::json!("function_call"))
        .expect("function_call");
    let item_id = COPILOT_RAW_TOOL_CALL_ID.split('|').nth(1).unwrap();
    let expected = format!("fc_{}", short_hash(item_id));
    assert_eq!(function_call["id"], serde_json::json!(expected));
    assert!(expected.len() <= 64);
    let id = function_call["id"].as_str().unwrap();
    assert!(id.starts_with("fc_"));
    assert!(id[3..].chars().all(|c| c.is_ascii_alphanumeric()));
}
