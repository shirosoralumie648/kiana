//! `openai-completions-retry.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use indexmap::IndexMap;
use kiana_ai::types::{SimpleStreamOptions, StopReason, StreamOptions};
use serde_json::json;

fn model() -> kiana_ai::types::Model {
    let mut model = mock_http::catalog_model("openrouter", "openrouter/auto");
    model.id = "test-model".into();
    model.name = "Test Model".into();
    model.api = "openai-completions".into();
    model.provider = "opencode-go".into();
    model.base_url = "https://opencode.ai/zen/go/v1".into();
    model.reasoning = Some(false);
    model.context_window = Some(1000);
    model.max_tokens = Some(100);
    model.compat = None;
    model
}

fn success_body() -> Vec<u8> {
    mock_http::sse_body(&[
        json!({
            "id": "chatcmpl-test",
            "choices": [{ "index": 0, "delta": { "content": "ok" } }],
        }),
        json!({
            "id": "chatcmpl-test",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        }),
    ])
}

fn options(
    mock: &mock_http::MockHttp,
    max_retries: Option<u32>,
    max_retry_delay_ms: Option<u64>,
) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".into()),
            fetch: Some(std::sync::Arc::new(mock.clone())),
            max_retries,
            max_retry_delay_ms,
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    }
}

fn rate_limited(status: u16, message: &str, headers: &[(&str, &str)]) -> mock_http::MockResponse {
    let mut header_map = IndexMap::new();
    for (name, value) in headers {
        header_map.insert((*name).to_string(), (*value).to_string());
    }
    mock_http::MockResponse {
        status,
        headers: header_map,
        body: json!({ "error": { "message": message } })
            .to_string()
            .into_bytes(),
    }
}

#[tokio::test]
async fn disables_sdk_retries_by_default() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse_raw(success_body()));
    let message = mock_http::run_completions_simple(
        &model(),
        &mock_http::text_context("hi"),
        options(&mock, None, None),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn honors_provider_retries_while_keeping_sdk_retries_disabled() {
    let mock = mock_http::MockHttp::new(vec![
        rate_limited(429, "rate limited", &[("retry-after-ms", "100")]),
        rate_limited(500, "server error", &[("retry-after-ms", "100")]),
        mock_http::MockResponse::sse_raw(success_body()),
    ]);
    let message = mock_http::run_completions_simple(
        &model(),
        &mock_http::text_context("hi"),
        options(&mock, Some(2), Some(100)),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(mock.request_count(), 3);
}

#[tokio::test]
async fn fails_immediately_when_a_provider_requested_retry_delay_exceeds_the_limit() {
    let mock = mock_http::MockHttp::single(rate_limited(
        429,
        "rate limited",
        &[("retry-after", "277403")],
    ));
    let message = mock_http::run_completions_simple(
        &model(),
        &mock_http::text_context("hi"),
        options(&mock, Some(2), Some(1000)),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.unwrap();
    assert!(
        error.contains("Server requested 277403s retry delay (max: 1s)"),
        "{error}"
    );
    assert!(error.contains("rate limited"), "{error}");
    assert_eq!(mock.request_count(), 1);
}
