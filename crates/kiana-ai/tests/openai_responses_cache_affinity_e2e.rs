//! `openai-responses-cache-affinity-e2e.test.ts`（真实 OpenAI API，需要 `OPENAI_API_KEY`）。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::StreamOptions;

#[tokio::test]
#[ignore = "needs live OpenAI API (OPENAI_API_KEY)"]
async fn handles_direct_openai_responses_requests_with_aligned_cache_affinity_identifiers() {
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };
    if api_key.is_empty() {
        return;
    }
    let model = mock_http::catalog_model("openai", "gpt-5.4");
    let session_id = "0195d6e4-4cf9-7f44-a2d8-f8f7f49ee9d3";
    let context = kiana_ai::types::Context {
        system_prompt: Some("You are a helpful assistant. Reply exactly as requested.".into()),
        messages: vec![mock_http::user(
            "Reply with exactly: openai cache affinity e2e success",
        )],
        tools: None,
    };
    let response = mock_http::run_responses(
        &model,
        &mock_http::context_with(&context),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some(api_key),
                session_id: Some(session_id.into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_ne!(
        response.stop_reason,
        kiana_ai::types::StopReason::Error,
        "{:?}",
        response.error_message
    );
    assert!(response.error_message.is_none());
    let text: String = response
        .content
        .iter()
        .filter_map(|block| match block {
            kiana_ai::types::AssistantContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    assert!(text.contains("openai cache affinity e2e success"), "{text}");
}
