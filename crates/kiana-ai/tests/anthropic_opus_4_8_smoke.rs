//! `anthropic-opus-4-8-smoke.test.ts` 的 Rust 移植（真实 API，`#[ignore]`）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Model, UserMessage, UserRole};

fn catalog_model() -> Model {
    let value =
        kiana_ai::providers::catalog::get_model("anthropic", "claude-opus-4-8").expect("model");
    serde_json::from_value(value.clone()).expect("model parses")
}

fn context() -> kiana_ai::types::TranscriptContext {
    harness::context(
        Some("You are a concise assistant."),
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("Reply with exactly: opus 4.8 ok".into()),
            timestamp: 0,
        })],
    )
}

#[tokio::test]
#[ignore = "live Anthropic API (ANTHROPIC_API_KEY)"]
async fn streams_claude_opus_4_8_with_reasoning_enabled() {
    let model = catalog_model();
    let mut options = AnthropicOptions::default();
    options.base.api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    options.base.max_tokens = Some(160);
    options.thinking_enabled = Some(true);
    options.effort = Some("high".into());
    let result = harness::run_to_result(stream(&model, &context(), options)).await;
    assert_eq!(
        result.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        result.error_message
    );
}
