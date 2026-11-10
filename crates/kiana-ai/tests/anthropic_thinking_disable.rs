//! `anthropic-thinking-disable.test.ts` 的 Rust 移植（E2E 部分为 `#[ignore]`）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::stream_simple;
use kiana_ai::types::{Model, SimpleStreamOptions, ThinkingLevel, UserMessage, UserRole};
use serde_json::Value;

fn catalog_model(provider: &str, id: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id).expect("catalog model");
    serde_json::from_value(value.clone()).expect("model parses")
}

fn user() -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text("Hello".into()),
        timestamp: 0,
    })
}

async fn capture_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(MockHttpClient::fixed(MockResponse::json(400, "{}"))),
        reasoning,
        ..Default::default()
    };
    options.base.api_key = Some("test-key".into());
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let context = context(None, vec![user()]);
    if let Ok(stream) = stream_simple(model, &context, options) {
        let _ = stream.result().await;
    }
    captured.lock().unwrap().clone().expect("payload")
}

#[tokio::test]
async fn sends_thinking_disabled_for_budget_and_adaptive_models_when_off() {
    for (provider, id) in [
        ("anthropic", "claude-sonnet-4-5"),
        ("anthropic", "claude-opus-4-6"),
        ("anthropic", "claude-opus-4-8"),
    ] {
        let payload = capture_payload(&catalog_model(provider, id), None).await;
        assert_eq!(
            payload["thinking"],
            serde_json::json!({ "type": "disabled" }),
            "{id}"
        );
        assert!(payload.get("output_config").is_none(), "{id}");
    }
}

#[tokio::test]
async fn omits_thinking_disabled_for_claude_fable_5_when_off() {
    let payload = capture_payload(&catalog_model("anthropic", "claude-fable-5"), None).await;
    assert!(payload.get("thinking").is_none());
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn uses_adaptive_thinking_for_opus_4_8_sonnet_5_and_xhigh_mapping() {
    let payload = capture_payload(
        &catalog_model("anthropic", "claude-opus-4-8"),
        Some(ThinkingLevel::High),
    )
    .await;
    assert_eq!(
        payload["thinking"],
        serde_json::json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(
        payload["output_config"],
        serde_json::json!({ "effort": "high" })
    );

    let payload = capture_payload(
        &catalog_model("anthropic", "claude-sonnet-5"),
        Some(ThinkingLevel::High),
    )
    .await;
    assert_eq!(
        payload["output_config"],
        serde_json::json!({ "effort": "high" })
    );

    let payload = capture_payload(
        &catalog_model("anthropic", "claude-opus-4-8"),
        Some(ThinkingLevel::Xhigh),
    )
    .await;
    assert_eq!(
        payload["output_config"],
        serde_json::json!({ "effort": "xhigh" })
    );
}

#[tokio::test]
#[ignore = "live Anthropic API"]
async fn disables_thinking_for_claude_reasoning_models_e2e() {
    let model = catalog_model("anthropic", "claude-sonnet-4-5");
    let stream = stream_simple(
        &model,
        &context(None, vec![user()]),
        SimpleStreamOptions::default(),
    )
    .expect("auth");
    let result = stream.result().await.expect("result");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert!(
        !result
            .content
            .iter()
            .any(|block| matches!(block, kiana_ai::types::AssistantContentBlock::Thinking(_)))
    );
}
