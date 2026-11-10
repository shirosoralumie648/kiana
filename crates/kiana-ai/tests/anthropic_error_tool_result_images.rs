//! `anthropic-error-tool-result-images.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::stream_simple;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, CacheRetention, ImageContent, Message, Model,
    ModelCost, ModelInputModality, SimpleStreamOptions, TextContent, ToolCall, ToolResultMessage,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use serde_json::{Value, json};

fn model() -> Model {
    Model {
        id: "claude-test".into(),
        name: "Claude Test".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(1024),
        ..Model::default()
    }
}

fn assistant() -> Message {
    Message::Assistant(AssistantMessage {
        role: Default::default(),
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "codemode".into(),
                ..Default::default()
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call-2".into(),
                name: "read".into(),
                ..Default::default()
            }),
        ],
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: "claude-test".into(),
        usage: Default::default(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 1,
        ..Default::default()
    })
}

async fn capture_payload(messages: Vec<Message>) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(MockHttpClient::fixed(MockResponse::json(400, "{}"))),
        ..Default::default()
    };
    options.base.api_key = Some("fake-key".into());
    options.base.cache_retention = Some(CacheRetention::None);
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let context = context(None, messages);
    if let Ok(stream) = stream_simple(&model(), &context, options) {
        let _ = stream.result().await;
    }
    captured.lock().unwrap().clone().expect("payload")
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: 0,
    })
}

fn tool_result(
    tool_call_id: &str,
    tool_name: &str,
    content: Vec<UserContentBlock>,
    is_error: bool,
) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: tool_call_id.into(),
        tool_name: tool_name.into(),
        content,
        details: None,
        usage: None,
        nested_calls: None,
        is_error,
        timestamp: 2,
        duration_ms: None,
    })
}

fn text(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        text: text.into(),
        ..Default::default()
    })
}

fn image(data: &str) -> UserContentBlock {
    UserContentBlock::Image(ImageContent {
        data: data.into(),
        mime_type: "image/png".into(),
        ..Default::default()
    })
}

#[tokio::test]
async fn moves_the_images_after_the_tool_results_of_the_turn() {
    let payload = capture_payload(vec![
        user("Show the screenshots"),
        assistant(),
        tool_result(
            "call-1",
            "codemode",
            vec![
                text("Script failed"),
                image("AAAA"),
                image("CCCC"),
                text("Script error"),
            ],
            true,
        ),
        tool_result("call-2", "read", vec![image("BBBB")], false),
    ])
    .await;
    assert_eq!(
        payload["messages"][2],
        json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "content": "Script failed\n(image attached after the tool results)\nScript error",
                    "is_error": true,
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "call-2",
                    "content": [
                        { "type": "text", "text": "(see attached image)" },
                        {
                            "type": "image",
                            "source": { "type": "base64", "media_type": "image/png", "data": "BBBB" },
                        },
                    ],
                    "is_error": false,
                },
                { "type": "text", "text": "Images from the error result of tool call call-1 (codemode):" },
                {
                    "type": "image",
                    "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" },
                },
                {
                    "type": "image",
                    "source": { "type": "base64", "media_type": "image/png", "data": "CCCC" },
                },
            ],
        })
    );
}

#[tokio::test]
async fn keeps_error_tool_results_without_images_unchanged() {
    let mut assistant = assistant();
    if let Message::Assistant(assistant) = &mut assistant {
        assistant.content.truncate(1);
    }
    let payload = capture_payload(vec![
        user("Run it"),
        assistant,
        tool_result("call-1", "codemode", vec![text("Script failed")], true),
    ])
    .await;
    assert_eq!(
        payload["messages"][2],
        json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-1",
                "content": "Script failed",
                "is_error": true,
            }],
        })
    );
}
