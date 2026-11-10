//! `openai-completions-cache-control-format.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::types::{CacheRetention, Context, Model, ModelInputModality, StreamOptions, Tool};
use serde_json::{Value, json};

fn custom_model() -> Model {
    Model {
        id: "custom-qwen".into(),
        name: "Custom Qwen".into(),
        api: "openai-completions".into(),
        provider: "openrouter".into(),
        base_url: "https://example.com/v1".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(32_000),
        compat: Some(json!({ "cacheControlFormat": "anthropic" })),
        ..Model::default()
    }
}

fn read_tool() -> Tool {
    Tool {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
        constrained_sampling: None,
    }
}

async fn capture(
    model: &Model,
    messages: Vec<kiana_ai::types::Message>,
    cache_retention: Option<CacheRetention>,
) -> (Value, mock_http::MockHttp) {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-test",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 },
        },
    })]));
    let context = mock_http::context_with(&Context {
        system_prompt: Some("System prompt".into()),
        messages,
        tools: Some(vec![read_tool()]),
    });
    let options = OpenAICompletionsOptions {
        base: StreamOptions {
            api_key: Some("test-key".into()),
            fetch: Some(Arc::new(mock.clone())),
            cache_retention,
            ..StreamOptions::default()
        },
        ..Default::default()
    };
    let _ = mock_http::run_completions(model, &context, options).await;
    (mock.last_body(), mock)
}

fn expect_anthropic_cache_markers(params: &Value) {
    let messages = params["messages"].as_array().unwrap();
    let instruction = messages
        .iter()
        .find(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("system") | Some("developer")
            )
        })
        .expect("instruction message");
    let content = instruction["content"].as_array().expect("content array");
    assert_eq!(content[0]["cache_control"], json!({ "type": "ephemeral" }));

    let tools = params["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["cache_control"], json!({ "type": "ephemeral" }));

    let last = messages.last().unwrap();
    assert_eq!(last["role"], json!("user"));
    let content = last["content"].as_array().expect("content array");
    assert_eq!(content[0]["cache_control"], json!({ "type": "ephemeral" }));
}

#[tokio::test]
async fn applies_anthropic_style_cache_markers_when_model_compat_enables_them() {
    let (payload, _) = capture(&custom_model(), vec![mock_http::user("Hello")], None).await;
    expect_anthropic_cache_markers(&payload);
}

#[tokio::test]
async fn preserves_anthropic_style_cache_markers_for_openrouter_anthropic_batch_aliases() {
    let model = mock_http::catalog_model("openrouter", "anthropic/claude-fable-5.1:batch");
    let (payload, _) = capture(&model, vec![mock_http::user("Hello")], None).await;
    expect_anthropic_cache_markers(&payload);
}

#[tokio::test]
async fn moves_the_conversation_cache_marker_to_a_tool_result() {
    let model = mock_http::catalog_model("openrouter", "anthropic/claude-fable-5.1:batch");
    let assistant = kiana_ai::types::AssistantMessage {
        role: Default::default(),
        content: vec![kiana_ai::types::AssistantContentBlock::ToolCall(
            kiana_ai::types::ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: [("path".to_string(), json!("README.md"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )],
        api: "openai-completions".into(),
        provider: "openrouter".into(),
        model: model.id.clone(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    };
    let tool_result = kiana_ai::types::ToolResultMessage {
        role: Default::default(),
        tool_call_id: "call_1".into(),
        tool_name: "read".into(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                text: "file contents".into(),
                ..Default::default()
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    };
    let (payload, _) = capture(
        &model,
        vec![
            mock_http::user("Read the file"),
            kiana_ai::types::Message::Assistant(assistant),
            kiana_ai::types::Message::ToolResult(tool_result),
        ],
        None,
    )
    .await;

    let messages = payload["messages"].as_array().unwrap();
    let user_message = messages
        .iter()
        .find(|m| m["role"] == json!("user"))
        .unwrap();
    assert_eq!(user_message["content"], json!("Read the file"));
    let tool_message = messages.last().unwrap();
    assert_eq!(tool_message["role"], json!("tool"));
    let content = tool_message["content"].as_array().expect("content array");
    assert_eq!(content[0]["cache_control"], json!({ "type": "ephemeral" }));
}

#[tokio::test]
async fn omits_anthropic_style_cache_markers_when_cache_retention_is_none() {
    let (payload, _) = capture(
        &custom_model(),
        vec![mock_http::user("Hello")],
        Some(CacheRetention::None),
    )
    .await;
    let messages = payload["messages"].as_array().unwrap();
    let instruction = messages
        .iter()
        .find(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("system") | Some("developer")
            )
        })
        .unwrap();
    assert!(!instruction["content"].is_array());
    assert!(payload["tools"][0].get("cache_control").is_none());
    assert!(messages.last().unwrap()["content"].is_string());
}
