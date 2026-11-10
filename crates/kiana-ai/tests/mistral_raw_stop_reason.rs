//! `test/mistral-raw-stop-reason.test.ts` 的移植。

mod common;

use kiana_ai::api::mistral_conversations::{MistralOptions, stream};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StreamOptions, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::retry::is_retryable_assistant_error;
use kiana_ai::utils::transcript::normalize_context;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn model() -> Model {
    Model {
        id: "devstral-medium-latest".into(),
        name: "Devstral Medium".into(),
        api: "mistral-conversations".into(),
        provider: "mistral".into(),
        base_url: "https://api.mistral.ai".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

async fn run(finish_reason: &str) -> kiana_ai::types::AssistantMessage {
    let server = MockServer::start().await;
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&json!({
            "id": "mistral-response-id",
            "model": "devstral-medium-latest",
            "choices": [{"index": 0, "finish_reason": finish_reason, "delta": {}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 0, "total_tokens": 1},
        }))
        .unwrap()
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let mut model = model();
    model.base_url = server.uri();
    stream(
        &model,
        &context(),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result")
}

#[tokio::test]
async fn preserves_raw_mistral_finish_reasons_for_successful_stops() {
    let message = run("stop").await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
    assert!(message.error_message.is_none());
}

#[tokio::test]
async fn preserves_raw_mistral_finish_reasons_for_provider_error_stops() {
    let message = run("error").await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("error"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: error (server error)")
    );
    assert!(is_retryable_assistant_error(&message));
}

#[tokio::test]
async fn treats_unknown_mistral_finish_reasons_as_provider_error_stops() {
    let message = run("unmapped_error").await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("unmapped_error"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: unmapped_error")
    );
    assert!(!is_retryable_assistant_error(&message));
}
