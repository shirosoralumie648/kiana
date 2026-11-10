//! `github-copilot-request-size-overflow.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::{Model, StreamOptions};
use kiana_ai::utils::overflow::is_context_overflow;
use serde_json::json;

fn model() -> Model {
    Model {
        id: "copilot-test-model".into(),
        name: "Copilot test model".into(),
        api: "openai-responses".into(),
        provider: "github-copilot".into(),
        base_url: "https://copilot.invalid".into(),
        reasoning: Some(true),
        input: vec![
            kiana_ai::types::ModelInputModality::Text,
            kiana_ai::types::ModelInputModality::Image,
        ],
        context_window: Some(1_000_000),
        max_tokens: Some(128_000),
        ..Model::default()
    }
}

#[tokio::test]
async fn classifies_http_request_size_errors() {
    for (status, nested, overflow) in [
        (413u16, true, true),
        (413u16, false, true),
        (400u16, true, false),
    ] {
        let error_body = json!({ "message": "failed to parse request", "code": "" });
        let body = if nested {
            json!({ "error": error_body })
        } else {
            error_body
        };
        let mock = mock_http::MockHttp::single(mock_http::MockResponse::json(status, &body));
        let result = mock_http::run_responses(
            &model(),
            &mock_http::text_context("Continue"),
            OpenAIResponsesOptions {
                base: StreamOptions {
                    api_key: Some("test-key".into()),
                    fetch: Some(std::sync::Arc::new(mock.clone())),
                    max_retries: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
        assert_eq!(mock.request_count(), 1, "{status} {nested}");
        assert_eq!(result.provider, "github-copilot");
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
        assert_eq!(result.usage.total_tokens, 0);
        let message = result.error_message.clone().unwrap_or_default();
        assert!(message.contains(&format!("({status})")), "{message}");
        assert!(message.contains("failed to parse request"), "{message}");
        assert_eq!(
            is_context_overflow(&result, model().context_window),
            overflow,
            "{status} {nested}"
        );
    }
}
