//! `openai-responses-message-id.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses_shared::convert_responses_messages;
use kiana_ai::types::{AssistantContentBlock, AssistantMessage, Message, TextContent, Usage};

#[test]
fn generates_unique_fallback_message_ids_for_multiple_text_blocks_in_one_assistant_turn() {
    let model = mock_http::catalog_model("openai-codex", "gpt-6.1-sol");
    let assistant = AssistantMessage {
        role: Default::default(),
        content: vec![
            AssistantContentBlock::Text(TextContent {
                text: "first answer".into(),
                ..Default::default()
            }),
            AssistantContentBlock::Text(TextContent {
                text: "visible answer".into(),
                ..Default::default()
            }),
        ],
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: "claude-opus-4-8".into(),
        usage: Usage::default(),
        stop_reason: kiana_ai::types::StopReason::Stop,
        timestamp: 1,
        ..Default::default()
    };
    let context = mock_http::context_with(&kiana_ai::types::Context {
        system_prompt: Some("You are concise.".into()),
        messages: vec![mock_http::user("hello"), Message::Assistant(assistant)],
        tools: None,
    });
    let input = convert_responses_messages(
        &model,
        &context,
        &["openai", "openai-codex", "opencode"],
        None,
    )
    .expect("convert");
    let message_ids: Vec<&str> = input
        .iter()
        .filter(|item| item["type"] == serde_json::json!("message"))
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(message_ids, ["msg_kiana_1", "msg_kiana_1_1"]);
}
