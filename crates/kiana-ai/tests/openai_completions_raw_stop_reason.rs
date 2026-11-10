//! `openai-completions-raw-stop-reason.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::types::StopReason;
use serde_json::json;

fn model() -> kiana_ai::types::Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-completions".into();
    model.compat = None;
    model
}

#[tokio::test]
async fn preserves_raw_finish_reasons_for_successful_stops() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-1",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
    })]));
    let message = mock_http::run_completions_simple(
        &model(),
        &mock_http::text_context("hello"),
        mock_http::simple_options(&mock, "test"),
    )
    .await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(message.error_message, None);
}

#[tokio::test]
async fn preserves_raw_finish_reasons_for_provider_error_stops() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-2",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "content_filter" }],
    })]));
    let message = mock_http::run_completions_simple(
        &model(),
        &mock_http::text_context("hello"),
        mock_http::simple_options(&mock, "test"),
    )
    .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("content_filter"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider finish_reason: content_filter")
    );
}
