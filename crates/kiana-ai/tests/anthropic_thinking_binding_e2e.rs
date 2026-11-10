//! `anthropic-thinking-binding-e2e.test.ts` 的 Rust 移植（真实 API，`#[ignore]`）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Model, UserMessage, UserRole};

fn catalog_model() -> Model {
    let value =
        kiana_ai::providers::catalog::get_model("anthropic", "claude-fable-5-1").expect("model");
    serde_json::from_value(value.clone()).expect("model parses")
}

#[tokio::test]
#[ignore = "live Anthropic API (managed effort models)"]
async fn replays_managed_effort_markers_required_by_signed_fable_thinking() {
    let model = catalog_model();
    let mut options = AnthropicOptions::default();
    options.base.api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    options.thinking_enabled = Some(true);
    options.effort = Some("high".into());
    let first_context = harness::context(
        Some("You are a helpful assistant. Use the tool when asked."),
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("Solve 17 * 23 step by step.".into()),
            timestamp: 0,
        })],
    );
    let first = harness::run_to_result(stream(&model, &first_context, options)).await;
    assert_ne!(first.stop_reason, kiana_ai::types::StopReason::Error);
}
