//! `anthropic-auth-token.test.ts` 的 Rust 移植：Bearer / x-api-key / 头自有鉴权与 UA。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use harness::{MockHttpClient, MockResponse, context, model};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream, stream_simple};
use kiana_ai::types::{SimpleStreamOptions, UserMessage, UserRole};
use serde_json::Value;

fn sse_body() -> String {
    let events = [
        (
            "message_start",
            serde_json::json!({
                "type": "message_start",
                "message": { "id": "msg_test", "usage": { "input_tokens": 1, "output_tokens": 0 } },
            }),
        ),
        (
            "message_delta",
            serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 1 },
            }),
        ),
        (
            "message_stop",
            serde_json::json!({ "type": "message_stop" }),
        ),
    ];
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn user() -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text("Hello".into()),
        timestamp: 0,
    })
}

fn header<'a>(request: &'a kiana_ai::http::HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn uses_authorization_headers_without_oauth_mode_request_shaping() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = None;
    options.base.headers = Some(indexmap::IndexMap::from([(
        "Authorization".to_string(),
        Some("Bearer gateway-token".to_string()),
    )]));
    let model = model("claude-test", "anthropic", "https://api.anthropic.com");
    let result = harness::run_to_result(stream(
        &model,
        &context(Some("System prompt."), vec![user()]),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);

    let request = fetch.requests().remove(0);
    assert_eq!(
        header(&request, "Authorization"),
        Some("Bearer gateway-token")
    );
    assert_eq!(header(&request, "x-api-key"), None);
    assert_eq!(header(&request, "anthropic-beta"), None);
    let body = harness::decode_body(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(
        body["system"][0]["text"],
        Value::String("System prompt.".into())
    );
}

#[tokio::test]
async fn preserves_oauth_request_shaping_for_sk_ant_oat_tokens() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = Some("sk-ant-oat-test".into());
    let model = model("claude-test", "anthropic", "https://api.anthropic.com");
    harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;

    let request = fetch.requests().remove(0);
    assert_eq!(
        header(&request, "authorization"),
        Some("Bearer sk-ant-oat-test")
    );
    assert_eq!(header(&request, "x-api-key"), None);
    assert_eq!(header(&request, "user-agent"), Some("claude-cli/2.1.280"));
    assert_eq!(header(&request, "x-app"), Some("cli"));
    assert!(
        header(&request, "anthropic-beta")
            .unwrap_or_default()
            .contains("oauth-2025-04-20")
    );
}

#[tokio::test]
async fn sends_x_api_key_for_api_key_auth_and_kiana_user_agent() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    let model = model("claude-test", "anthropic", "https://api.anthropic.com");
    harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "x-api-key"), Some("test-key"));
    assert_eq!(header(&request, "authorization"), None);
    assert!(header(&request, "user-agent").is_some_and(|ua| ua.contains("kiana")));
}

#[tokio::test]
async fn explicit_headers_override_the_default_user_agent() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.headers = Some(indexmap::IndexMap::from([(
        "User-Agent".to_string(),
        Some("custom-client".to_string()),
    )]));
    let model = model(
        "kimi-for-coding",
        "kimi-coding",
        "https://api.kimi.com/coding",
    );
    harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "User-Agent"), Some("custom-client"));
}

#[tokio::test]
async fn preserves_explicit_beta_replacement_and_suppression() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.headers = Some(indexmap::IndexMap::from([(
        "anthropic-beta".to_string(),
        Some("custom-beta".to_string()),
    )]));
    let model = model("claude-test", "anthropic", "https://api.anthropic.com");
    harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "anthropic-beta"), Some("custom-beta"));

    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.headers = Some(indexmap::IndexMap::from([(
        "anthropic-beta".to_string(),
        None,
    )]));
    harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "anthropic-beta"), None);
}

#[tokio::test]
async fn stream_simple_returns_an_error_when_no_auth_is_available() {
    let model = model("claude-test", "anthropic", "https://api.anthropic.com");
    match stream_simple(
        &model,
        &context(None, vec![user()]),
        SimpleStreamOptions::default(),
    ) {
        Ok(_) => panic!("expected a synchronous auth error"),
        Err(error) => assert_eq!(error.message, "No API key for provider: anthropic"),
    }
}

#[tokio::test]
async fn stream_emits_an_error_event_when_no_auth_is_available() {
    let model = model("claude-test", "anthropic", "https://api.anthropic.com");
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user()]),
        AnthropicOptions::default(),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("No API key for provider: anthropic")
    );
}
