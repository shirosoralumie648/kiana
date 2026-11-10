//! `anthropic-temperature-compat.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::stream_simple;
use kiana_ai::types::{
    Model, ModelCost, ModelInputModality, SimpleStreamOptions, UserMessage, UserRole,
};
use serde_json::Value;

fn catalog_model(provider: &str, id: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id).expect("catalog model");
    serde_json::from_value(value.clone()).expect("model parses")
}

fn custom_model(compat: Option<Value>) -> Model {
    Model {
        id: "vendor--claude-opus-4-7".into(),
        name: "Vendor Proxy Opus 4.7".into(),
        api: "anthropic-messages".into(),
        provider: "vendor-proxy".into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        compat,
        ..Model::default()
    }
}

fn user() -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text("Hello".into()),
        timestamp: 0,
    })
}

async fn capture_temperature(model: &Model, temperature: f64) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(MockHttpClient::fixed(MockResponse::json(400, "{}"))),
        ..Default::default()
    };
    options.base.temperature = Some(temperature);
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
async fn omits_temperature_for_claude_opus_4_7_and_4_8() {
    for id in ["claude-opus-4-7", "claude-opus-4-8"] {
        let payload = capture_temperature(&catalog_model("anthropic", id), 0.0).await;
        assert!(payload.get("temperature").is_none(), "{id}");
        let payload = capture_temperature(&catalog_model("anthropic", id), 1.0).await;
        assert!(payload.get("temperature").is_none(), "{id} default");
    }
}

#[tokio::test]
async fn keeps_temperature_for_claude_opus_4_6_and_sonnet_4_6() {
    let payload = capture_temperature(&catalog_model("anthropic", "claude-opus-4-6"), 0.0).await;
    assert_eq!(payload["temperature"], Value::from(0.0));
    let payload = capture_temperature(&catalog_model("anthropic", "claude-sonnet-4-6"), 0.0).await;
    assert_eq!(payload["temperature"], Value::from(0.0));
}

#[tokio::test]
async fn omits_temperature_for_custom_models_with_supports_temperature_disabled() {
    let model = custom_model(Some(serde_json::json!({ "supportsTemperature": false })));
    let payload = capture_temperature(&model, 0.0).await;
    assert!(payload.get("temperature").is_none());
}
