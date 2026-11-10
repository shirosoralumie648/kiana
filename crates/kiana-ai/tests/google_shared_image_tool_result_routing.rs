//! `test/google-shared-image-tool-result-routing.test.ts` 的移植。

use kiana_ai::api::google_shared::convert_messages;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, ImageContent, ImageKind,
    Message, Model, ModelInputModality, TextContent, TextKind, ToolCall, ToolCallKind,
    ToolResultMessage, ToolResultRole, Usage, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

fn make_model(id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "google-generative-ai".into(),
        provider: "google".into(),
        base_url: "https://example.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        context_window: Some(128_000),
        max_tokens: Some(8192),
        ..Model::default()
    }
}

fn text_block(text: &str) -> kiana_ai::types::UserContentBlock {
    kiana_ai::types::UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

fn image_block() -> kiana_ai::types::UserContentBlock {
    kiana_ai::types::UserContentBlock::Image(ImageContent {
        kind_image: ImageKind::Image,
        data: "abc".into(),
        mime_type: "image/png".into(),
    })
}

fn make_context(model: &Model) -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("read the files".into()),
                timestamp: 1,
            }),
            Message::Assistant(AssistantMessage {
                role: AssistantRole::Assistant,
                content: vec![
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "call_a".into(),
                        name: "read".into(),
                        arguments: [("path".to_string(), json!("a.txt"))].into_iter().collect(),
                        ..ToolCall::default()
                    }),
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "call_img".into(),
                        name: "read".into(),
                        arguments: [("path".to_string(), json!("image.png"))]
                            .into_iter()
                            .collect(),
                        ..ToolCall::default()
                    }),
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "call_b".into(),
                        name: "read".into(),
                        arguments: [("path".to_string(), json!("b.txt"))].into_iter().collect(),
                        ..ToolCall::default()
                    }),
                ],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                usage: Usage::default(),
                stop_reason: kiana_ai::types::StopReason::ToolUse,
                timestamp: 1,
                ..Default::default()
            }),
            Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
                tool_call_id: "call_a".into(),
                tool_name: "read".into(),
                content: vec![text_block("alpha text")],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
            Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
                tool_call_id: "call_img".into(),
                tool_name: "read".into(),
                content: vec![image_block()],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
            Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
                tool_call_id: "call_b".into(),
                tool_name: "read".into(),
                content: vec![text_block("beta text")],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
        ],
        ..Context::default()
    })
}

#[test]
fn keeps_separate_synthetic_image_turn_for_gemini_2x_models() {
    let model = make_model("gemini-2.5-flash");
    let contents = convert_messages(&model, &make_context(&model));
    let contents = contents.as_array().expect("contents");
    assert_eq!(contents.len(), 5);
    assert!(
        contents[2]["parts"]
            .as_array()
            .expect("parts")
            .iter()
            .all(|part| part.get("functionResponse").is_some())
    );
    assert_eq!(contents[3]["parts"][0]["text"], json!("Tool result image:"));
    assert!(contents[3]["parts"][1].get("inlineData").is_some());
    assert!(contents[4]["parts"][0].get("functionResponse").is_some());
}

#[test]
fn nests_image_tool_results_for_gemini_3_models() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(&model, &make_context(&model));
    let contents = contents.as_array().expect("contents");
    assert_eq!(contents.len(), 3);
    let parts = contents[2]["parts"].as_array().expect("parts");
    assert_eq!(parts.len(), 3);
    let image_response = &parts[1]["functionResponse"];
    assert!(image_response.is_object());
    assert_eq!(image_response["parts"].as_array().expect("parts").len(), 1);
    assert!(image_response["parts"][0].get("inlineData").is_some());
}

#[test]
fn treats_whitespace_only_tool_result_text_as_no_text() {
    let model = make_model("gemini-3-pro-preview");
    let mut context = make_context(&model);
    for message in &mut context.0.messages {
        if let Message::ToolResult(result) = message {
            if result.tool_call_id == "call_a" {
                result.content = vec![text_block("\r\n")];
            }
            if result.tool_call_id == "call_img" {
                result.content.insert(0, text_block(" \n"));
            }
        }
    }
    let contents = convert_messages(&model, &context);
    let contents = contents.as_array().expect("contents");
    let responses: Vec<Value> = contents[2]["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .map(|part| part["functionResponse"]["response"].clone())
        .collect();
    assert_eq!(responses[0], json!({"output": ""}));
    assert_eq!(responses[1], json!({"output": "(see attached image)"}));
    assert_eq!(responses[2], json!({"output": "beta text"}));
}
