//! `anthropic-force-adaptive-thinking.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream_simple};
use kiana_ai::types::{
    Model, ModelCost, ModelInputModality, SimpleStreamOptions, ThinkingLevel, UserMessage, UserRole,
};
use serde_json::Value;

fn capture_model(id: &str, provider: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "anthropic-messages".into(),
        provider: provider.into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        ..Model::default()
    }
}

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

async fn capture_payload(model: &Model, options: SimpleStreamOptions) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::json(
        400,
        "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"x\"}}",
    ));
    let mut options = options;
    options.base.fetch = Some(fetch);
    options.base.api_key = Some("test-key".into());
    options.base.api_key = Some("test-key".into());
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let context = context(None, vec![user()]);
    if let Ok(stream) = stream_simple(model, &context, options) {
        let _ = stream.result().await;
    }
    let payload = captured.lock().unwrap().clone();
    payload.expect("payload captured")
}

#[tokio::test]
async fn sends_legacy_thinking_payload_for_custom_model_ids_by_default() {
    let model = capture_model("vendor--claude-opus-latest", "vendor-proxy");
    let payload = capture_payload(
        &model,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(payload["thinking"]["type"], Value::String("enabled".into()));
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn sends_adaptive_thinking_payload_when_compat_force_adaptive_thinking_is_true() {
    let mut model = capture_model("vendor--claude-opus-latest", "vendor-proxy");
    model.compat = Some(serde_json::json!({ "forceAdaptiveThinking": true }));
    let payload = capture_payload(
        &model,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        payload["thinking"],
        serde_json::json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(
        payload["output_config"],
        serde_json::json!({ "effort": "medium" })
    );
}

#[tokio::test]
async fn uses_adaptive_thinking_with_native_xhigh_effort_for_claude_fable_5() {
    let model = catalog_model("anthropic", "claude-fable-5");
    let payload = capture_payload(
        &model,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Xhigh),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        payload["thinking"],
        serde_json::json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(
        payload["output_config"],
        serde_json::json!({ "effort": "xhigh" })
    );
}

#[tokio::test]
async fn allows_built_in_adaptive_models_to_opt_out_with_compat_false() {
    let mut model = catalog_model("anthropic", "claude-opus-4-8");
    model.compat = Some(serde_json::json!({ "forceAdaptiveThinking": false }));
    let payload = capture_payload(
        &model,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(payload["thinking"]["type"], Value::String("enabled".into()));
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn preserves_thinking_type_disabled_when_reasoning_is_off_regardless_of_override() {
    let mut model = capture_model("vendor--claude-opus-latest", "vendor-proxy");
    model.compat = Some(serde_json::json!({ "forceAdaptiveThinking": true }));
    let payload = capture_payload(&model, SimpleStreamOptions::default()).await;
    assert_eq!(
        payload["thinking"],
        serde_json::json!({ "type": "disabled" })
    );
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn direct_anthropic_options_default_effort_still_uses_adaptive_thinking() {
    // `stream()` 直接给 thinkingEnabled + effort 时，forceAdaptiveThinking 决定 payload 形状。
    let model = catalog_model("anthropic", "claude-opus-4-8");
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch),
        ..Default::default()
    };
    options.thinking_enabled = Some(true);
    options.effort = Some("high".into());
    options.base.api_key = Some("test-key".into());
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let stream =
        kiana_ai::api::anthropic_messages::stream(&model, &context(None, vec![user()]), options);
    let _ = stream.result().await;
    let payload = captured.lock().unwrap().clone().expect("payload");
    assert_eq!(
        payload["output_config"],
        serde_json::json!({ "effort": "high" })
    );
}
