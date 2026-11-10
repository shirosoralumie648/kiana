//! `anthropic-cache-write-1h-cost.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Message, UserMessage, UserRole};
use serde_json::{Value, json};

fn catalog_model(provider: &str, id: &str) -> kiana_ai::types::Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id).expect("catalog model");
    serde_json::from_value(value.clone()).expect("model parses")
}

fn user() -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text("hi".into()),
        timestamp: 0,
    })
}

fn sse(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn events_with_cache_creation(cache_creation: Option<Value>) -> Vec<(&'static str, Value)> {
    let mut start_usage = json!({
        "input_tokens": 100,
        "output_tokens": 0,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 1_000_000,
    });
    if let Some(cache_creation) = cache_creation {
        start_usage["cache_creation"] = cache_creation;
    }
    vec![
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_test", "usage": start_usage } }),
        ),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hi" } }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 1_000_000,
                },
            }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]
}

fn options(fetch: std::sync::Arc<MockHttpClient>) -> AnthropicOptions {
    AnthropicOptions {
        base: harness::stream_options(fetch),
        ..Default::default()
    }
}

#[tokio::test]
async fn prices_the_1h_portion_at_2x_input_and_the_rest_at_the_5m_rate() {
    let model = catalog_model("anthropic", "claude-opus-4-8");
    let events = events_with_cache_creation(Some(json!({
        "ephemeral_5m_input_tokens": 600_000,
        "ephemeral_1h_input_tokens": 400_000,
    })));
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user()]),
        options(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.usage.cache_write, 1_000_000);
    assert_eq!(result.usage.cache_write1h, Some(400_000));
    // 600k * 6.25/Mtok + 400k * 10/Mtok = 3.75 + 4.0 = 7.75
    assert!((result.usage.cost.cache_write - 7.75).abs() < 1e-10);
}

#[tokio::test]
async fn prices_1h_cache_writes_reported_only_in_message_delta() {
    let model = catalog_model("vercel-ai-gateway", "anthropic/claude-haiku-4.5");
    let events = vec![
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_test", "usage": { "input_tokens": 0, "output_tokens": 0 } } }),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 4,
                    "cache_creation_input_tokens": 6535,
                    "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 6535 },
                },
            }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ];
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user()]),
        options(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.usage.cache_write, 6535);
    assert_eq!(result.usage.cache_write1h, Some(6535));
    assert!(
        (result.usage.cost.cache_write - (6535.0 * model.cost.input * 2.0) / 1_000_000.0).abs()
            < 1e-10
    );
}

#[tokio::test]
async fn falls_back_to_the_5m_rate_when_no_breakdown_is_reported() {
    let model = catalog_model("anthropic", "claude-opus-4-8");
    let events = events_with_cache_creation(None);
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user()]),
        options(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.usage.cache_write, 1_000_000);
    assert_eq!(result.usage.cache_write1h, Some(0));
    assert!((result.usage.cost.cache_write - 6.25).abs() < 1e-10);
}
