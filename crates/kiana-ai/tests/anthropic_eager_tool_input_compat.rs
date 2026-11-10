//! `anthropic-eager-tool-input-compat.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use harness::{MockHttpClient, MockResponse, context_with_tools};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{
    CacheRetention, Model, ModelCost, ModelInputModality, Tool, UserMessage, UserRole,
};
use serde_json::{Value, json};

fn model(compat: Option<Value>) -> Model {
    Model {
        id: "claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "anthropic-messages".into(),
        provider: "test-anthropic".into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        compat: Some(match compat {
            Some(Value::Object(mut compat)) => {
                compat.insert("forceAdaptiveThinking".into(), Value::Bool(true));
                Value::Object(compat)
            }
            _ => json!({ "forceAdaptiveThinking": true }),
        }),
        ..Model::default()
    }
}

fn tool() -> Tool {
    Tool {
        name: "lookup".into(),
        description: "Look up a value".into(),
        parameters: json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
        }),
        constrained_sampling: None,
    }
}

fn context(tools: Vec<Tool>) -> kiana_ai::types::TranscriptContext {
    context_with_tools(
        None,
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("Use the tool".into()),
            timestamp: 0,
        })],
        tools,
    )
}

async fn capture(compat: Option<Value>, tools: Vec<Tool>) -> (kiana_ai::http::HttpRequest, Value) {
    let fetch = MockHttpClient::fixed(MockResponse::sse(""));
    let mut base = harness::stream_options(fetch.clone());
    base.cache_retention = Some(CacheRetention::None);
    let options = AnthropicOptions {
        base,
        ..Default::default()
    };
    let stream = stream(&model(compat), &context(tools), options);
    let _ = stream.result().await;
    let request = fetch.requests().remove(0);
    let body = harness::decode_body(request.body.as_deref().expect("body")).expect("json body");
    (request, body)
}

fn header<'a>(request: &'a kiana_ai::http::HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn sends_per_tool_eager_input_streaming_by_default() {
    let (request, body) = capture(None, vec![tool()]).await;
    assert_eq!(body["tools"][0]["eager_input_streaming"], Value::Bool(true));
    assert_eq!(header(&request, "anthropic-beta"), None);
}

#[tokio::test]
async fn uses_the_legacy_fine_grained_beta_when_eager_is_disabled() {
    let (request, body) = capture(
        Some(json!({ "supportsEagerToolInputStreaming": false })),
        vec![tool()],
    )
    .await;
    assert!(body["tools"][0].get("eager_input_streaming").is_none());
    assert_eq!(
        header(&request, "anthropic-beta"),
        Some("fine-grained-tool-streaming-2025-05-14")
    );
}

#[tokio::test]
async fn does_not_send_the_legacy_beta_without_tools() {
    let (request, body) = capture(
        Some(json!({ "supportsEagerToolInputStreaming": false })),
        Vec::new(),
    )
    .await;
    assert!(body.get("tools").is_none());
    assert_eq!(header(&request, "anthropic-beta"), None);
}

#[tokio::test]
async fn adds_the_deferred_tool_placeholder_with_native_tool_changes() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(""));
    let mut model = model(None);
    model.compat = Some(json!({
        "forceAdaptiveThinking": true,
        "supportsMidConvoSystemMessages": true,
        "supportsMidConvoToolChanges": true,
    }));
    let mut base = harness::stream_options(fetch.clone());
    base.cache_retention = Some(kiana_ai::types::CacheRetention::None);
    let options = AnthropicOptions {
        base,
        ..Default::default()
    };
    let stream = stream(&model, &context(vec![tool()]), options);
    let _ = stream.result().await;
    let body = fetch.first_json_body().expect("body");
    let tools = body["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], Value::String("lookup".into()));
    assert_eq!(
        tools[1]["name"],
        Value::String("__kiana_deferred_placeholder__".into())
    );
    assert_eq!(tools[1]["defer_loading"], Value::Bool(true));
    let request = fetch.requests().remove(0);
    assert!(
        request
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("anthropic-beta")
                && value.contains("inline-tools-2026-09-15"))
    );
}
