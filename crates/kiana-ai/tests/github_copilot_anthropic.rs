//! `github-copilot-anthropic.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Model, ModelThinkingLevel, UserMessage, UserRole};
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

fn sse_body() -> String {
    [
        (
            "message_start",
            serde_json::json!({ "type": "message_start", "message": { "id": "msg_test", "usage": { "input_tokens": 10, "output_tokens": 0 } } }),
        ),
        (
            "message_delta",
            serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 5 } }),
        ),
        (
            "message_stop",
            serde_json::json!({ "type": "message_stop" }),
        ),
    ]
    .iter()
    .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
    .collect()
}

fn header<'a>(request: &'a kiana_ai::http::HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[test]
fn applies_copilot_specific_adaptive_thinking_effort_overrides() {
    let opus5 = catalog_model("github-copilot", "claude-opus-5");
    assert_eq!(opus5.api, "anthropic-messages");
    assert_eq!(opus5.context_window, Some(1_000_000));
    let map = opus5.thinking_level_map.as_ref().expect("map");
    assert_eq!(
        map.get(&ModelThinkingLevel::Minimal),
        Some(&Some("low".to_string()))
    );
    assert_eq!(
        map.get(&ModelThinkingLevel::Xhigh),
        Some(&Some("xhigh".to_string()))
    );
    assert_eq!(
        map.get(&ModelThinkingLevel::Max),
        Some(&Some("max".to_string()))
    );
    let levels = kiana_ai::models::get_supported_thinking_levels(&opus5);
    assert!(levels.contains(&ModelThinkingLevel::Xhigh));
    assert!(levels.contains(&ModelThinkingLevel::Max));

    let opus55 = catalog_model("github-copilot", "claude-opus-5.5");
    assert_eq!(opus55.api, "anthropic-messages");
    assert_eq!(opus55.context_window, Some(1_000_000));
    assert_eq!(
        kiana_ai::models::get_supported_thinking_levels(&opus55),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
            ModelThinkingLevel::Max,
        ]
    );

    let sonnet46 = catalog_model("github-copilot", "claude-sonnet-4.6");
    let map = sonnet46.thinking_level_map.as_ref().expect("map");
    assert_eq!(
        map.get(&ModelThinkingLevel::Minimal),
        Some(&Some("low".to_string()))
    );
    assert_eq!(
        map.get(&ModelThinkingLevel::Max),
        Some(&Some("max".to_string()))
    );
    let levels = kiana_ai::models::get_supported_thinking_levels(&sonnet46);
    assert!(levels.contains(&ModelThinkingLevel::Max));
    assert!(!levels.contains(&ModelThinkingLevel::Xhigh));
}

#[tokio::test]
async fn uses_bearer_auth_copilot_headers_and_valid_anthropic_payload() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = Some("tid_copilot_session_test_token".into());
    let model = catalog_model("github-copilot", "claude-sonnet-4.6");
    let result = harness::run_to_result(stream(
        &model,
        &context(Some("You are a helpful assistant."), vec![user()]),
        options,
    ))
    .await;
    assert_eq!(
        result.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        result.error_message
    );

    let request = fetch.requests().remove(0);
    assert_eq!(
        header(&request, "authorization"),
        Some("Bearer tid_copilot_session_test_token")
    );
    assert_eq!(header(&request, "x-api-key"), None);
    assert!(
        header(&request, "user-agent").is_some_and(|value| value.contains("GitHubCopilotChat"))
    );
    assert_eq!(
        header(&request, "Copilot-Integration-Id"),
        Some("vscode-chat")
    );
    assert_eq!(header(&request, "X-Initiator"), Some("user"));
    assert_eq!(
        header(&request, "Openai-Intent"),
        Some("conversation-edits")
    );

    let body = fetch.first_json_body().expect("body");
    let betas = body
        .get("betas")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !betas
            .iter()
            .any(|beta| beta.as_str() == Some("fine-grained-tool-streaming-2025-05-14"))
    );
    assert_eq!(body["model"], Value::String("claude-sonnet-4.6".into()));
    assert_eq!(body["stream"], Value::Bool(true));
    assert_eq!(body["max_tokens"], Value::from(model.max_tokens.unwrap()));
    assert!(body["messages"].is_array());
}

#[tokio::test]
async fn omits_interleaved_thinking_beta_for_adaptive_thinking_models() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = Some("tid_copilot_session_test_token".into());
    options.interleaved_thinking = Some(true);
    let model = catalog_model("github-copilot", "claude-sonnet-4.6");
    let _ = harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;
    let request = fetch.requests().remove(0);
    assert!(
        !header(&request, "anthropic-beta")
            .unwrap_or_default()
            .contains("interleaved-thinking-2025-05-14")
    );
}
