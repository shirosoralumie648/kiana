//! `openai-responses-chatgpt-sign-in.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::{CacheRetention, Model, StreamOptions};
use serde_json::{Value, json};

fn model() -> Model {
    Model {
        id: "gpt-5-mini".into(),
        name: "GPT-5 Mini".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: Some(true),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(400_000),
        max_tokens: Some(128_000),
        ..Model::default()
    }
}

async fn capture_payload(api_key: &str, request_model: Model) -> Value {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::json(
        500,
        &json!({ "error": { "message": "boom" } }),
    ));
    let _ = mock_http::run_responses(
        &request_model,
        &mock_http::context(vec![]),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some(api_key.into()),
                fetch: Some(std::sync::Arc::new(mock.clone())),
                max_tokens: Some(1000),
                temperature: Some(0.5),
                cache_retention: Some(CacheRetention::Long),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    mock.last_body()
}

#[tokio::test]
async fn omits_request_fields_that_token_sharing_rejects() {
    let payload = capture_payload("chatgpt-access-token", model()).await;
    assert!(payload.get("max_output_tokens").is_none());
    assert!(payload.get("temperature").is_none());
    assert!(payload.get("prompt_cache_retention").is_none());
    assert!(payload.get("prompt_cache_options").is_none());
}

#[tokio::test]
async fn omits_prompt_cache_options_on_models_with_explicit_prompt_cache_mode() {
    let mut explicit_cache_model = model();
    explicit_cache_model.compat = Some(json!({ "supportsExplicitPromptCacheMode": true }));

    let sign_in = capture_payload("chatgpt-access-token", explicit_cache_model.clone()).await;
    let api_key = capture_payload("sk-proj-test", explicit_cache_model).await;

    assert!(sign_in.get("prompt_cache_options").is_none());
    assert_eq!(
        api_key.get("prompt_cache_options"),
        Some(&json!({ "ttl": "30m" }))
    );
}

#[tokio::test]
async fn keeps_those_fields_for_api_keys_and_other_compatible_endpoints() {
    let payload = capture_payload("sk-proj-test", model()).await;
    assert_eq!(payload.get("max_output_tokens"), Some(&json!(1000)));
    assert_eq!(payload.get("temperature"), Some(&json!(0.5)));
    assert_eq!(payload.get("prompt_cache_retention"), Some(&json!("24h")));

    let mut gateway = model();
    gateway.base_url = "https://gateway.example.com/v1".into();
    let payload = capture_payload("gateway-key", gateway).await;
    assert_eq!(payload.get("max_output_tokens"), Some(&json!(1000)));
    assert_eq!(payload.get("temperature"), Some(&json!(0.5)));
    assert_eq!(payload.get("prompt_cache_retention"), Some(&json!("24h")));
}
