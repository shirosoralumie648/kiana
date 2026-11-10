//! `interleaved-thinking.test.ts` 的 Rust 移植。
//!
//! TS 的断言是真实 API 上的多轮工具调用（需要 Anthropic/Bedrock 凭据），因此标为
//! `#[ignore]`；离线部分断言 `interleaved-thinking-2025-05-14` beta 的发送条件。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Model, Tool, UserMessage, UserRole};
use serde_json::json;

fn catalog_model(provider: &str, id: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id).expect("catalog model");
    serde_json::from_value(value.clone()).expect("model parses")
}

fn user() -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text("Use calculator to calculate 328 * 29.".into()),
        timestamp: 0,
    })
}

fn sse_body() -> String {
    [
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_test", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]
    .iter()
    .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
    .collect()
}

fn beta_header(request: &kiana_ai::http::HttpRequest) -> String {
    request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn sends_interleaved_thinking_beta_for_non_adaptive_reasoning_models() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.thinking_enabled = Some(true);
    let model = catalog_model("anthropic", "claude-sonnet-4-5");
    let context = harness::context_with_tools(
        None,
        vec![user()],
        vec![Tool {
            name: "calculator".into(),
            description: "Perform basic arithmetic operations".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" },
                    "operation": { "type": "string" },
                },
                "required": ["a", "b", "operation"],
            }),
            constrained_sampling: None,
        }],
    );
    let _ = harness::run_to_result(stream(&model, &context, options)).await;
    let request = fetch.requests().remove(0);
    assert!(beta_header(&request).contains("interleaved-thinking-2025-05-14"));
}

#[tokio::test]
async fn omits_interleaved_thinking_beta_when_the_caller_disables_it() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.thinking_enabled = Some(true);
    options.interleaved_thinking = Some(false);
    let model = catalog_model("anthropic", "claude-sonnet-4-5");
    let _ = harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;
    let request = fetch.requests().remove(0);
    assert!(!beta_header(&request).contains("interleaved-thinking-2025-05-14"));
}

#[tokio::test]
#[ignore = "live Anthropic API"]
async fn should_do_interleaved_thinking_on_claude_opus_4_5() {
    let model = catalog_model("anthropic", "claude-opus-4-5");
    let mut options = AnthropicOptions::default();
    options.base.api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    options.thinking_enabled = Some(true);
    options.effort = Some("high".into());
    let result =
        harness::run_to_result(stream(&model, &context(None, vec![user()]), options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::ToolUse);
    assert!(
        result
            .content
            .iter()
            .any(|block| matches!(block, kiana_ai::types::AssistantContentBlock::Thinking(_)))
    );
}
