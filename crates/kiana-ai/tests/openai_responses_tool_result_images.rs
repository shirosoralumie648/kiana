//! `openai-responses-tool-result-images.test.ts`（真实 provider；全部 live）。
//!
//! 断言 tool result 的图片必须留在 `function_call_output` 的 content 数组中，而不是被改写
//! 成后续 user 消息；请求体通过 `on_payload` 捕获后检查。

mod common;

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use kiana_ai::compat::complete;
use kiana_ai::types::{
    AssistantContentBlock, Context, Message, Model, StreamOptions, Tool, ToolResultMessage,
};
use serde_json::{Value, json};

/// 1x1 透明 PNG。
const RED_CIRCLE_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn get_image_tool() -> Tool {
    Tool {
        name: "get_circle_with_description".into(),
        description: "Returns a red circle image with a short text description.".into(),
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
    }
}

fn context() -> Context {
    Context {
        system_prompt: Some(
            "You are a helpful assistant that always uses the provided tool when asked.".into(),
        ),
        messages: vec![mock_http::user(
            "Call get_circle_with_description, then describe both the tool text and the image. Mention the color and shape.",
        )],
        tools: Some(vec![get_image_tool()]),
    }
}

/// TS 的 `verifyToolResultImagesStayInFunctionCallOutput`（live）：第一次请求拿到 tool call，
/// 第二次带文本+图片 tool result，并捕获请求体验证 `function_call_output` 的形状。
async fn verify_tool_result_images_stay_in_function_call_output(
    model: &Model,
    api_key: String,
    extras: &[(&str, Value)],
) {
    if !model
        .input
        .contains(&kiana_ai::types::ModelInputModality::Image)
    {
        return;
    }
    let tool_text = "A red circle with a diameter of 100 pixels.";
    let options = |capture: Option<Arc<Mutex<Option<Value>>>>| {
        let mut options = StreamOptions {
            api_key: Some(api_key.clone()),
            ..Default::default()
        };
        for (key, value) in extras {
            options.extra.insert((*key).to_string(), value.clone());
        }
        if let Some(capture) = capture {
            options.on_payload = Some(Arc::new(move |payload: Value, _model: &Model| {
                *capture.lock().unwrap_or_else(|error| error.into_inner()) = Some(payload);
                None
            }));
        }
        options
    };

    let first = complete(model, &context(), options(None)).await;
    assert_eq!(
        first.stop_reason,
        kiana_ai::types::StopReason::ToolUse,
        "Error: {:?}",
        first.error_message
    );
    let tool_call = first
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .expect("tool call");

    let mut messages = context().messages;
    messages.push(Message::Assistant(first));
    messages.push(Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        content: vec![
            kiana_ai::types::UserContentBlock::Text(kiana_ai::types::TextContent {
                text: tool_text.into(),
                ..Default::default()
            }),
            kiana_ai::types::UserContentBlock::Image(kiana_ai::types::ImageContent {
                data: RED_CIRCLE_PNG_BASE64.into(),
                mime_type: "image/png".into(),
                ..Default::default()
            }),
        ],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 2,
        duration_ms: None,
    }));
    let second_context = Context {
        system_prompt: context().system_prompt,
        messages,
        tools: Some(vec![get_image_tool()]),
    };
    let capture = Arc::new(Mutex::new(None));
    let second = complete(model, &second_context, options(Some(Arc::clone(&capture)))).await;
    assert_eq!(
        second.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "Error: {:?}",
        second.error_message
    );
    assert!(second.error_message.is_none());

    let payload = capture
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("captured payload");
    let input = payload["input"].as_array().expect("input array");
    let index = input
        .iter()
        .position(|item| item["type"] == json!("function_call_output"))
        .expect("function_call_output item");
    let output = input[index]["output"]
        .as_array()
        .expect("output content array");
    let text_item = output
        .iter()
        .find(|item| item["type"] == json!("input_text"))
        .expect("input_text item");
    let image_item = output
        .iter()
        .find(|item| item["type"] == json!("input_image"))
        .expect("input_image item");
    assert!(
        text_item["text"]
            .as_str()
            .unwrap_or_default()
            .contains(tool_text)
    );
    assert!(
        image_item["image_url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("data:image/png;base64,")
    );
    let later_user_messages = input[index + 1..]
        .iter()
        .filter(|item| item["role"] == json!("user"))
        .count();
    assert_eq!(later_user_messages, 0);

    let response_text = second
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(response_text.contains("red"));
    assert!(response_text.contains("circle"));
}

#[tokio::test]
#[ignore = "needs live OPENAI_API_KEY"]
async fn openai_responses_provider_gpt_5_mini_sends_tool_result_images_in_function_call_output() {
    if std::env::var("OPENAI_API_KEY").map_or(true, |key| key.is_empty()) {
        return;
    }
    let model = mock_http::catalog_model("openai", "gpt-5-mini");
    verify_tool_result_images_stay_in_function_call_output(
        &model,
        std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        &[("reasoningEffort", json!("low"))],
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live Azure OpenAI credentials"]
async fn azure_openai_responses_provider_gpt_4o_mini_sends_tool_result_images_in_function_call_output()
 {
    if !common::credentials::has_azure_openai_credentials() {
        return;
    }
    let model = mock_http::catalog_model("azure", "gpt-4o-mini");
    let extras: Vec<(&str, Value)> = common::credentials::resolve_azure_deployment_name(&model.id)
        .map(|name| vec![("azureDeploymentName", json!(name))])
        .unwrap_or_default();
    verify_tool_result_images_stay_in_function_call_output(
        &model,
        std::env::var("AZURE_OPENAI_API_KEY").unwrap_or_default(),
        &extras,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live github-copilot credentials"]
async fn github_copilot_responses_provider_gpt_6_luna_sends_tool_result_images_in_function_call_output()
 {
    let Some(token) = common::oauth::resolve_api_key("github-copilot") else {
        return;
    };
    let model = mock_http::catalog_model("github-copilot", "gpt-6-luna");
    verify_tool_result_images_stay_in_function_call_output(
        &model,
        token,
        &[("reasoningEffort", json!("low"))],
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live openai-codex credentials"]
async fn openai_codex_responses_provider_gpt_6_1_sol_sends_tool_result_images_in_function_call_output()
 {
    let Some(token) = common::oauth::resolve_api_key("openai-codex") else {
        return;
    };
    let model = mock_http::catalog_model("openai-codex", "gpt-6.1-sol");
    verify_tool_result_images_stay_in_function_call_output(
        &model,
        token,
        &[("reasoningEffort", json!("low"))],
    )
    .await;
}
