//! `openai-completions-vllm-priority.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use serde_json::json;

fn create_model(compat: Option<serde_json::Value>) -> kiana_ai::types::Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-completions".into();
    model.compat = compat;
    model
}

async fn capture_request(model: &kiana_ai::types::Model) -> serde_json::Value {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 },
        },
    })]));
    let mut context = mock_http::text_context("hi");
    context = kiana_ai::types::TranscriptContext::from_normalized(kiana_ai::types::Context {
        system_prompt: Some("sys".into()),
        messages: context.as_context().messages.clone(),
        tools: None,
    });
    let _ = mock_http::run_completions_simple(
        model,
        &context,
        mock_http::simple_options(&mock, "test-key"),
    )
    .await;
    mock.last_body()
}

#[tokio::test]
async fn sends_compat_vllm_priority_as_the_top_level_priority_request_field() {
    let body = capture_request(&create_model(Some(json!({ "vllmPriority": 10 })))).await;
    assert_eq!(body.get("priority"), Some(&json!(10)));
}

#[tokio::test]
async fn omits_priority_when_vllm_priority_is_not_set() {
    let body = capture_request(&create_model(None)).await;
    assert!(body.get("priority").is_none());
}
