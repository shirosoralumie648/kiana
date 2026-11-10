//! `openai-codex-cache-affinity-e2e.test.ts` 的 Rust 移植（真实 Codex 后端，`#[ignore]`）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use kiana_ai::api::openai_codex_responses::{OpenAICodexResponsesOptions, stream};
use kiana_ai::types::{Model, Transport, UserMessage, UserRole};

fn catalog_model() -> Model {
    let value =
        kiana_ai::providers::catalog::get_model("openai-codex", "gpt-6.1-sol").expect("model");
    serde_json::from_value(value.clone()).expect("model parses")
}

#[tokio::test]
#[ignore = "live OpenAI Codex backend (requires OAuth credentials)"]
async fn handles_sse_requests_with_aligned_cache_affinity_identifiers() {
    let model = catalog_model();
    let mut options = OpenAICodexResponsesOptions::default();
    options.base.api_key = std::env::var("OPENAI_CODEX_ACCESS_TOKEN").ok();
    options.base.session_id = Some("0195d6e4-4cf9-7f44-a2d8-f8f7f49ee9d3".into());
    options.base.transport = Some(Transport::Sse);
    let context = harness::context(
        Some("You are a helpful assistant. Reply exactly as requested."),
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text(
                "Reply with exactly: cache affinity e2e success".into(),
            ),
            timestamp: 0,
        })],
    );
    let result = harness::run_to_result(stream(&model, &context, options)).await;
    assert_ne!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(result.error_message.is_none());
}
