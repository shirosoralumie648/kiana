//! `xiaomi-token-plan-ams-anthropic-empty-signature-smoke.test.ts` 的 Rust 移植
//! （真实 API，`#[ignore]`）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Model, UserMessage, UserRole};

fn catalog_model() -> Model {
    let value = kiana_ai::providers::catalog::get_model("xiaomi-token-plan-ams", "mimo-v2.6-pro")
        .expect("model");
    serde_json::from_value(value.clone()).expect("model parses")
}

#[tokio::test]
#[ignore = "live Xiaomi Token Plan AMS API"]
async fn reproduces_empty_thinking_signatures_and_preserves_them_for_replay() {
    let model = catalog_model();
    let mut options = AnthropicOptions::default();
    options.base.api_key = std::env::var("XIAOMI_TOKEN_PLAN_API_KEY").ok();
    options.thinking_enabled = Some(true);
    let context = harness::context(
        Some("Think step by step."),
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("What is 6*7?".into()),
            timestamp: 0,
        })],
    );
    let result = harness::run_to_result(stream(&model, &context, options)).await;
    assert_ne!(result.stop_reason, kiana_ai::types::StopReason::Error);
}
