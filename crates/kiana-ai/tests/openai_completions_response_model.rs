//! `openai-completions-response-model.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::types::StopReason;
use serde_json::json;

fn openrouter_auto() -> kiana_ai::types::Model {
    let mut model = mock_http::catalog_model("openrouter", "openrouter/auto");
    model.api = "openai-completions".into();
    model.reasoning = Some(false);
    model
}

#[tokio::test]
async fn surfaces_routed_chunk_model_on_response_model_without_changing_model() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({
            "id": "chatcmpl-1",
            "model": "anthropic/claude-opus-4.8",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-1",
            "model": "anthropic/claude-opus-4.8",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ]));
    let message = mock_http::run_completions_simple(
        &openrouter_auto(),
        &mock_http::text_context("hi"),
        mock_http::simple_options(&mock, "test"),
    )
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(
        message.response_model.as_deref(),
        Some("anthropic/claude-opus-4.8")
    );
    assert_eq!(message.provider, "openrouter");
    assert_eq!(message.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn leaves_response_model_unset_when_chunks_echo_the_requested_id() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({
            "id": "chatcmpl-2",
            "model": "openrouter/auto",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-2",
            "model": "openrouter/auto",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ]));
    let message = mock_http::run_completions_simple(
        &openrouter_auto(),
        &mock_http::text_context("hi"),
        mock_http::simple_options(&mock, "test"),
    )
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model, None);
}

#[tokio::test]
async fn ignores_empty_or_missing_chunk_model() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({
            "id": "chatcmpl-3",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-3",
            "model": "",
            "choices": [{ "index": 0, "delta": { "content": "!" } }],
        }),
        json!({
            "id": "chatcmpl-3",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ]));
    let message = mock_http::run_completions_simple(
        &openrouter_auto(),
        &mock_http::text_context("hi"),
        mock_http::simple_options(&mock, "test"),
    )
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model, None);
}
