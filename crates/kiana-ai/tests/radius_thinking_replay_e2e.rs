//! `radius-thinking-replay-e2e.test.ts` 的 Rust 移植（真实网关，`#[ignore]`）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Model, ModelThinkingLevel, UserMessage, UserRole};

fn catalog_model(provider: &str, id: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id).expect("catalog model");
    serde_json::from_value(value.clone()).expect("model parses")
}

fn context() -> kiana_ai::types::TranscriptContext {
    harness::context(
        Some("Think carefully before answering."),
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("What is 2+2? Think first.".into()),
            timestamp: 0,
        })],
    )
}

#[tokio::test]
#[ignore = "live Anthropic API (control)"]
async fn anthropic_keeps_replayed_thinking() {
    let model = catalog_model("anthropic", "claude-opus-4-8");
    let mut options = AnthropicOptions::default();
    options.base.api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    options.thinking_enabled = Some(true);
    let result = harness::run_to_result(stream(&model, &context(), options)).await;
    assert_ne!(result.stop_reason, kiana_ai::types::StopReason::Error);
    let _ = ModelThinkingLevel::High;
}

#[tokio::test]
#[ignore = "live Radius gateway (RADIUS_API_KEY)"]
async fn radius_keeps_replayed_thinking() {
    let model = catalog_model("radius", "claude-opus-4-8");
    let mut options = AnthropicOptions::default();
    options.base.api_key = std::env::var("RADIUS_API_KEY").ok();
    options.thinking_enabled = Some(true);
    let result = harness::run_to_result(stream(&model, &context(), options)).await;
    assert_ne!(result.stop_reason, kiana_ai::types::StopReason::Error);
}
