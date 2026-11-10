//! `max-thinking.test.ts` 的 Rust 移植（模型目录 + Codex payload）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::openai_codex_responses::stream_simple;
use kiana_ai::models::{clamp_thinking_level, get_supported_thinking_levels};
use kiana_ai::types::{
    Model, ModelCost, ModelInputModality, ModelThinkingLevel, SimpleStreamOptions, ThinkingLevel,
    ThinkingLevelMap, UserMessage, UserRole,
};
use serde_json::Value;

fn mock_token() -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(
        serde_json::json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acc_test" } })
            .to_string(),
    );
    format!("aaa.{payload}.bbb")
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

#[test]
fn max_is_opt_in_for_ordinary_reasoning_models() {
    let model = Model {
        id: "ordinary-reasoning".into(),
        name: "Ordinary Reasoning".into(),
        api: "openai-completions".into(),
        provider: "test".into(),
        base_url: "https://example.com/v1".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(128_000),
        max_tokens: Some(4096),
        ..Model::default()
    };
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );
    assert_eq!(
        clamp_thinking_level(&model, ModelThinkingLevel::Max),
        ModelThinkingLevel::High
    );
}

#[test]
fn exposes_xhigh_and_max_for_openai_codex_models() {
    for id in [
        "gpt-5.6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-6-luna",
        "gpt-6-sol",
    ] {
        let model = catalog_model("openai-codex", id);
        let map = model.thinking_level_map.as_ref().expect("map");
        assert_eq!(
            map.get(&ModelThinkingLevel::Xhigh),
            Some(&Some("xhigh".to_string())),
            "{id}"
        );
        assert_eq!(
            map.get(&ModelThinkingLevel::Max),
            Some(&Some("max".to_string())),
            "{id}"
        );
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
                ModelThinkingLevel::Max,
            ],
            "{id}"
        );
    }
}

#[test]
fn supports_a_hole_between_high_and_max() {
    let model = Model {
        id: "high-and-max".into(),
        name: "High and Max".into(),
        api: "openai-completions".into(),
        provider: "test".into(),
        base_url: "https://example.com/v1".into(),
        reasoning: Some(true),
        thinking_level_map: Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Xhigh, None),
            (ModelThinkingLevel::Max, Some("max".into())),
        ])),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(128_000),
        max_tokens: Some(4096),
        ..Model::default()
    };
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max,
        ]
    );
    assert_eq!(
        clamp_thinking_level(&model, ModelThinkingLevel::Xhigh),
        ModelThinkingLevel::Max
    );
}

#[tokio::test]
async fn sends_max_to_the_codex_responses_api() {
    for id in [
        "gpt-5.6-sol",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
    ] {
        let model = catalog_model("openai-codex", id);
        let captured = Arc::new(Mutex::new(None::<Value>));
        let captured_for_callback = captured.clone();
        let mut options = SimpleStreamOptions {
            base: harness::stream_options(MockHttpClient::fixed(MockResponse::json(400, "{}"))),
            reasoning: Some(ThinkingLevel::Max),
            ..Default::default()
        };
        options.base.api_key = Some(mock_token());
        options.base.transport = Some(kiana_ai::types::Transport::Sse);
        options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
            *captured_for_callback.lock().unwrap() = Some(payload);
            None
        }));
        let context = context(Some("You are a helpful assistant."), vec![user()]);
        if let Ok(stream) = stream_simple(&model, &context, options) {
            let _ = stream.result().await;
        }
        let payload = captured.lock().unwrap().clone().expect("payload");
        assert_eq!(
            payload["reasoning"],
            serde_json::json!({ "effort": "max", "summary": "auto" }),
            "{id}"
        );
    }
}
