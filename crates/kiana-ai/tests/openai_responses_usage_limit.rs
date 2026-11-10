//! `openai-responses-usage-limit.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::{Model, StreamOptions};
use serde_json::json;

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

async fn error_message(response: mock_http::MockResponse) -> Option<String> {
    let mock = mock_http::MockHttp::single(response);
    let message = mock_http::run_responses(
        &model(),
        &mock_http::context(vec![]),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(std::sync::Arc::new(mock)),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    message.error_message
}

#[tokio::test]
async fn links_to_chatgpt_usage_when_the_request_is_rejected() {
    let usage_limit_error = json!({
        "code": "subscription_sharing_usage_limit_exceeded",
        "message": "Usage limit reached.",
        "type": "rate_limit_error",
    });
    let error = error_message(mock_http::MockResponse::json(
        429,
        &json!({ "error": usage_limit_error }),
    ))
    .await
    .unwrap();
    assert!(
        error.contains("subscription_sharing_usage_limit_exceeded"),
        "{error}"
    );
    assert!(
        error.contains("Check your ChatGPT usage: https://chatgpt.com/settings/usage"),
        "{error}"
    );
}

#[tokio::test]
async fn links_to_chatgpt_usage_when_the_stream_fails() {
    let event = json!({
        "type": "response.failed",
        "sequence_number": 0,
        "response": {
            "id": "resp_failed",
            "status": "failed",
            "error": { "code": "subscription_sharing_usage_limit_exceeded", "message": "Usage limit reached." },
        },
    });
    let body = format!(
        "event: response.failed\ndata: {}\n\n",
        kiana_json::js_object::stringify(&event)
    );
    let error = error_message(mock_http::MockResponse::text(
        200,
        "text/event-stream",
        body,
    ))
    .await
    .unwrap();
    assert!(
        error.contains("subscription_sharing_usage_limit_exceeded: Usage limit reached."),
        "{error}"
    );
    assert!(
        error.contains("Check your ChatGPT usage: https://chatgpt.com/settings/usage"),
        "{error}"
    );
}
