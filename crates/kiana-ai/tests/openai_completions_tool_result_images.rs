//! `openai-completions-tool-result-images.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_completions::{convert_messages, detect_compat};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, ModelInputModality,
    ToolCall, ToolResultMessage, ToolResultRole, Usage, UserContent, UserContentBlock, UserMessage,
    UserRole,
};
use serde_json::json;

fn empty_usage() -> Usage {
    Usage::default()
}

fn model() -> kiana_ai::types::Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-completions".into();
    model.input = vec![ModelInputModality::Text, ModelInputModality::Image];
    model
}

fn compat() -> kiana_ai::api::openai_completions::ResolvedOpenAICompletionsCompat {
    let mut compat = detect_compat(&model());
    compat.supports_strict_mode = true;
    compat.cache_control_format = Some("anthropic".into());
    compat
}

fn tool_result(tool_call_id: &str, timestamp: i64) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.into(),
        tool_name: "read".into(),
        content: vec![
            UserContentBlock::Text(kiana_ai::types::TextContent {
                text: "Read image file [image/png]".into(),
                ..Default::default()
            }),
            UserContentBlock::Image(kiana_ai::types::ImageContent {
                data: "ZmFrZQ==".into(),
                mime_type: "image/png".into(),
                ..Default::default()
            }),
        ],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp,
        duration_ms: None,
    }
}

fn empty_tool_result(tool_call_id: &str, timestamp: i64, text: &str) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.into(),
        tool_name: "bash".into(),
        content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
            text: text.into(),
            ..Default::default()
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp,
        duration_ms: None,
    }
}

fn context(messages: Vec<Message>) -> kiana_ai::types::TranscriptContext {
    kiana_ai::types::TranscriptContext::from_normalized(Context {
        messages,
        ..Context::default()
    })
}

#[test]
fn omits_empty_text_parts_from_user_messages_with_images() {
    let context = context(vec![Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![
            UserContentBlock::Text(kiana_ai::types::TextContent {
                text: String::new(),
                ..Default::default()
            }),
            UserContentBlock::Image(kiana_ai::types::ImageContent {
                data: "ZmFrZQ==".into(),
                mime_type: "image/png".into(),
                ..Default::default()
            }),
        ]),
        timestamp: 1,
    })]);
    let messages = convert_messages(&model(), &context, &compat(), None).expect("convert");
    assert_eq!(
        messages,
        vec![json!({
            "role": "user",
            "content": [{ "type": "image_url", "image_url": { "url": "data:image/png;base64,ZmFrZQ==" } }],
        })]
    );
}

#[test]
fn batches_tool_result_images_after_consecutive_tool_results() {
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall {
                id: "tool-1".into(),
                name: "read".into(),
                arguments: [("path".to_string(), json!("img-1.png"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "tool-2".into(),
                name: "read".into(),
                arguments: [("path".to_string(), json!("img-2.png"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }),
        ],
        api: "openai-completions".into(),
        provider: "openai".into(),
        model: "gpt-4o-mini".into(),
        usage: empty_usage(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 2,
        ..AssistantMessage::default()
    };
    let context = context(vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Read the images".into()),
            timestamp: 1,
        }),
        Message::Assistant(assistant),
        Message::ToolResult(tool_result("tool-1", 3)),
        Message::ToolResult(tool_result("tool-2", 4)),
    ]);
    let messages = convert_messages(&model(), &context, &compat(), None).expect("convert");
    let roles: Vec<&str> = messages
        .iter()
        .map(|message| {
            message
                .get("role")
                .and_then(|role| role.as_str())
                .unwrap_or("")
        })
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool", "user"]);
    let image_message = messages.last().unwrap();
    let parts = image_message["content"].as_array().unwrap();
    let images = parts
        .iter()
        .filter(|part| part.get("type").and_then(|t| t.as_str()) == Some("image_url"))
        .count();
    assert_eq!(images, 2);
}

#[test]
fn uses_no_tool_output_placeholder_for_whitespace_only_results() {
    for text in ["", "\r\n", " \n\t"] {
        let assistant = AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContentBlock::ToolCall(ToolCall {
                id: "tool-1".into(),
                name: "bash".into(),
                arguments: [("command".to_string(), json!("true"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            })],
            api: "openai-completions".into(),
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            usage: empty_usage(),
            stop_reason: kiana_ai::types::StopReason::ToolUse,
            timestamp: 2,
            ..AssistantMessage::default()
        };
        let context = context(vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("Run the command".into()),
                timestamp: 1,
            }),
            Message::Assistant(assistant),
            Message::ToolResult(empty_tool_result("tool-1", 3, text)),
        ]);
        let messages = convert_messages(&model(), &context, &compat(), None).expect("convert");
        let tool_message = messages
            .iter()
            .find(|message| message.get("role").and_then(|r| r.as_str()) == Some("tool"))
            .unwrap();
        assert_eq!(
            tool_message.get("content").and_then(|c| c.as_str()),
            Some("(no tool output)"),
            "{text:?}"
        );
    }
}
