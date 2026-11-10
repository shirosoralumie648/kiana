//! `packages/ai/test/anthropic-sse-parsing.test.ts` 的 Rust 移植。
//!
//! TS 用 `vi.mock` 注入假的 Anthropic SDK 客户端；Rust 侧注入 [`MockHttpClient`]，
//! 断言最终消息、事件顺序与转发给 `onProviderStreamEvent` 的原始事件。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream, stream_simple};
use kiana_ai::api::transform_messages::transform_messages;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Tool, ToolCall, UserMessage,
    UserRole,
};
use serde_json::{Value, json};

fn sse(events: &[(String, String)]) -> String {
    let mut body = String::new();
    for (event, data) in events {
        body.push_str(&format!("event: {event}\ndata: {data}\n\n"));
    }
    body
}

fn catalog_model(provider: &str, id: &str) -> kiana_ai::types::Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id)
        .unwrap_or_else(|| panic!("missing catalog model {provider}/{id}"));
    serde_json::from_value(value.clone()).expect("catalog model parses")
}

fn user_message(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.into()),
        timestamp: 1,
    })
}

fn minimal_events() -> Vec<(String, String)> {
    vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                    },
                },
            })
            .to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" },
            })
            .to_string(),
        ),
        (
            "content_block_delta".into(),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "Hello" },
            })
            .to_string(),
        ),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta".into(),
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
            })
            .to_string(),
        ),
        (
            "message_stop".into(),
            json!({ "type": "message_stop" }).to_string(),
        ),
    ]
}

fn options_with(fetch: Arc<MockHttpClient>) -> AnthropicOptions {
    AnthropicOptions {
        base: harness::stream_options(fetch),
        ..Default::default()
    }
}

#[tokio::test]
async fn forwards_parsed_provider_stream_events_in_order() {
    let provider_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let events_for_callback = provider_events.clone();
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let mut options = options_with(fetch);
    options.base.on_provider_stream_event = Some(Arc::new(move |event: Value, _model| {
        if let Some(kind) = event.get("type").and_then(Value::as_str) {
            events_for_callback.lock().unwrap().push(kind.to_string());
        }
    }));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(
        *provider_events.lock().unwrap(),
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
}

#[tokio::test]
async fn uses_openrouter_reported_cost() {
    let mut events = minimal_events();
    let mut delta: Value = serde_json::from_str(&events[4].1).unwrap();
    delta["usage"] = json!({
        "input_tokens": 12,
        "output_tokens": 5,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cost": 0.004,
        "is_byok": false,
        "cost_details": { "upstream_inference_cost": 0.003 },
    });
    events[4].1 = delta.to_string();
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&events)));
    let mut model = catalog_model("openrouter", "anthropic/claude-haiku-4.5");
    model.provider = "openrouter-work".into();
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(fetch),
    ))
    .await;
    assert_eq!(result.usage.cost.total, 0.004);
    assert_eq!(result.usage.cost.total_from_provider, Some(true));
    assert_ne!(
        result.usage.cost.input
            + result.usage.cost.output
            + result.usage.cost.cache_read
            + result.usage.cost.cache_write,
        result.usage.cost.total
    );
}

/// TS 的 `applyOpenRouterReportedCost` 只在 `is_byok === false` / `=== true` 时改写 usage；
/// 流式 usage 缺少 `is_byok`（undefined）时必须保留 `calculateCost` 的目录价。
#[tokio::test]
async fn ignores_openrouter_accounting_without_a_boolean_is_byok() {
    let mut events = minimal_events();
    let mut delta: Value = serde_json::from_str(&events[4].1).unwrap();
    delta["usage"] = json!({
        "input_tokens": 12,
        "output_tokens": 5,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cost": 0.004,
        "cost_details": { "upstream_inference_cost": 0.003 },
    });
    events[4].1 = delta.to_string();
    let mut model = catalog_model("openrouter", "anthropic/claude-haiku-4.5");
    model.provider = "openrouter-work".into();

    let reported = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    let catalog = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(
            &minimal_events(),
        )))),
    ))
    .await;

    assert_eq!(reported.usage.cost.total, catalog.usage.cost.total);
    assert_eq!(reported.usage.cost.total_from_provider, Some(false));
    assert_ne!(reported.usage.cost.total, 0.004);
}

#[tokio::test]
async fn keeps_signed_thinking_replayable_when_a_proxy_relabels_the_model() {
    let model = catalog_model("anthropic", "claude-opus-5");
    let response_model = "kimi-for-coding";
    let events = vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_response_model",
                    "model": response_model,
                    "usage": { "input_tokens": 100, "output_tokens": 0 },
                },
            })
            .to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "thinking", "thinking": "reasoning", "signature": "signature" },
            })
            .to_string(),
        ),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta".into(),
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "input_tokens": 100, "output_tokens": 20 },
            })
            .to_string(),
        ),
        (
            "message_stop".into(),
            json!({ "type": "message_stop" }).to_string(),
        ),
    ];
    let initial_context = context(None, vec![user_message("Hello")]);
    let first = harness::run_to_result(stream(
        &model,
        &initial_context,
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(first.model, model.id);
    assert_eq!(first.response_model.as_deref(), Some(response_model));

    let mut messages = initial_context.0.messages.clone();
    messages.push(Message::Assistant(first));
    let transformed = transform_messages(&messages, &model);
    let assistant = transformed
        .iter()
        .find_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .expect("assistant message");
    assert_eq!(
        assistant.content,
        vec![AssistantContentBlock::Thinking(
            kiana_ai::types::ThinkingContent {
                thinking: "reasoning".into(),
                thinking_signature: Some("signature".into()),
                ..Default::default()
            }
        )]
    );
}

#[tokio::test]
async fn uses_a_returned_fallback_model_for_cost_attribution() {
    let fallback_model = "fallback-model";
    let mut model = catalog_model("anthropic", "claude-opus-5");
    model.compat = Some(json!({
        "allowedFallbackModels": [{
            "provider": "anthropic",
            "model": fallback_model,
            "cost": { "input": 3, "output": 5, "cacheRead": 0, "cacheWrite": 0 },
        }],
    }));
    let events = vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_response_model",
                    "model": fallback_model,
                    "usage": { "input_tokens": 100, "output_tokens": 0 },
                },
            })
            .to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "done" },
            })
            .to_string(),
        ),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta".into(),
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "input_tokens": 100, "output_tokens": 20 },
            })
            .to_string(),
        ),
        (
            "message_stop".into(),
            json!({ "type": "message_stop" }).to_string(),
        ),
    ];
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.model, model.id);
    assert_eq!(result.response_model.as_deref(), Some(fallback_model));
    assert!((result.usage.cost.input - 0.0003).abs() < 1e-10);
    assert!((result.usage.cost.output - 0.0001).abs() < 1e-10);
}

#[tokio::test]
async fn fails_safely_when_anthropic_falls_back_after_output_begins() {
    let model = catalog_model("anthropic", "claude-opus-5");
    let events = vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": { "id": "msg_fallback", "usage": { "input_tokens": 1, "output_tokens": 0 } },
            })
            .to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "partial" },
            })
            .to_string(),
        ),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": { "type": "fallback", "from": { "model": "claude-opus-5" }, "to": { "model": "claude-opus-4-8" } },
            })
            .to_string(),
        ),
    ];
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("unsupported mid-output model fallback"))
    );
}

#[tokio::test]
async fn forces_streaming_after_an_on_payload_replacement() {
    let seen_stream = Arc::new(Mutex::new(None::<Value>));
    let seen = seen_stream.clone();
    let fetch = MockHttpClient::with_handler(move |request| {
        if let Some(body) = request.body.as_deref().and_then(harness::decode_body) {
            *seen.lock().unwrap() = body.get("stream").cloned();
        }
        Ok(MockResponse::sse(sse(&minimal_events())))
    });
    let mut options = options_with(fetch);
    options.base.on_payload = Some(Arc::new(|payload: Value, _model| {
        let mut payload = payload;
        payload["stream"] = Value::Bool(false);
        Some(payload)
    }));
    let model = catalog_model("anthropic", "claude-fable-5-1");
    harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;
    assert_eq!(*seen_stream.lock().unwrap(), Some(Value::Bool(true)));
}

#[tokio::test]
async fn omits_interleaved_thinking_beta_when_thinking_is_disabled() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let mut options = options_with(fetch.clone());
    options.thinking_enabled = Some(false);
    let model = catalog_model("openrouter", "anthropic/claude-haiku-4.5");
    harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;
    let request = fetch.requests().remove(0);
    let beta_header = request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    assert!(!beta_header.contains("interleaved-thinking-2025-05-14"));
    // `betas` 字段不进入 wire body（SDK 把它拼进头）。
    let body = fetch.first_json_body().expect("body");
    assert!(body.get("betas").is_none());
}

#[tokio::test]
async fn passes_managed_beta_features_to_requests() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let model = catalog_model("anthropic", "claude-fable-5-1");
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(fetch.clone()),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Stop);
    let request = fetch.requests().remove(0);
    let beta_header = request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
        .map(|(_, value)| value.clone())
        .expect("anthropic-beta header");
    assert!(beta_header.contains("mid-conversation-output-config-2026-07-01"));
    assert!(beta_header.contains("thinking-binding-controls-2026-08-01"));
}

#[tokio::test]
async fn uses_serving_model_input_transformations_from_the_final_stream_event() {
    let mut events = minimal_events();
    let mut start: Value = serde_json::from_str(&events[0].1).unwrap();
    start["message"]["model"] = Value::String("claude-fable-5-1".into());
    start["message"]["input_transformations"] = json!([
        { "type": "thinking_dropped", "path": "messages.1.content.0", "reason": "prefix_binding_mismatch" }
    ]);
    events[0].1 = start.to_string();
    let mut delta: Value = serde_json::from_str(&events[4].1).unwrap();
    delta["input_transformations"] = json!([
        { "type": "thinking_dropped", "path": "messages.3.content.0", "reason": "model_binding_mismatch" }
    ]);
    events[4].1 = delta.to_string();
    let model = catalog_model("anthropic", "claude-fable-5-1");
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    let diagnostics = result.diagnostics.expect("diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, "anthropic_input_transformations");
    assert_eq!(
        diagnostics[0].details.as_ref().unwrap()["transformations"],
        json!([{
            "type": "thinking_dropped",
            "path": "messages.3.content.0",
            "reason": "model_binding_mismatch",
        }])
    );
}

#[tokio::test]
async fn repairs_malformed_sse_json_and_malformed_streamed_tool_json() {
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let context = harness::context_with_tools(
        None,
        vec![user_message("Use the edit tool.")],
        vec![Tool {
            name: "edit".into(),
            description: "Edit a file.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "text": { "type": "string" },
                },
                "required": ["path", "text"],
            }),
            constrained_sampling: None,
        }],
    );
    let malformed_tool_json_delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"A\\H\",\"text\":\"col1\tcol2\"}"}}"#;
    let events = vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": { "id": "msg_test", "usage": { "input_tokens": 12, "output_tokens": 0 } },
            })
            .to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "tool_use", "id": "toolu_test", "name": "edit", "input": {} },
            })
            .to_string(),
        ),
        ("content_block_delta".into(), malformed_tool_json_delta.into()),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta".into(),
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "tool_use" },
                "usage": { "input_tokens": 12, "output_tokens": 5 },
            })
            .to_string(),
        ),
        (
            "message_stop".into(),
            json!({ "type": "message_stop" }).to_string(),
        ),
    ];
    let result = harness::run_to_result(stream(
        &model,
        &context,
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::ToolUse);
    assert_eq!(result.error_message, None);
    let tool_call = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(
        tool_call.arguments.get("path"),
        Some(&Value::String("A\\H".into()))
    );
    assert_eq!(
        tool_call.arguments.get("text"),
        Some(&Value::String("col1\tcol2".into()))
    );
}

#[tokio::test]
async fn preserves_content_from_content_block_start_events() {
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let events = vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": { "id": "msg_initial_content", "usage": { "input_tokens": 12, "output_tokens": 0 } },
            })
            .to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "Initial text" },
            })
            .to_string(),
        ),
        (
            "content_block_delta".into(),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": " plus delta" },
            })
            .to_string(),
        ),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": { "type": "thinking", "thinking": "Initial thinking", "signature": "initial signature" },
            })
            .to_string(),
        ),
        (
            "content_block_delta".into(),
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": { "type": "thinking_delta", "thinking": " plus delta" },
            })
            .to_string(),
        ),
        (
            "content_block_delta".into(),
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": { "type": "signature_delta", "signature": " plus delta" },
            })
            .to_string(),
        ),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 1 }).to_string(),
        ),
        (
            "message_delta".into(),
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "input_tokens": 12, "output_tokens": 5 },
            })
            .to_string(),
        ),
        (
            "message_stop".into(),
            json!({ "type": "message_stop" }).to_string(),
        ),
    ];
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Say hello.")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(
        result.content,
        vec![
            AssistantContentBlock::Text(kiana_ai::types::TextContent {
                text: "Initial text plus delta".into(),
                ..Default::default()
            }),
            AssistantContentBlock::Thinking(kiana_ai::types::ThinkingContent {
                thinking: "Initial thinking plus delta".into(),
                thinking_signature: Some("initial signature plus delta".into()),
                ..Default::default()
            }),
        ]
    );
}

fn stop_reason_events(stop_reason: &str, stop_details: Option<Value>) -> Vec<(String, String)> {
    let mut delta = json!({ "type": "message_delta", "delta": { "stop_reason": stop_reason } });
    if let Some(stop_details) = stop_details {
        delta["delta"]["stop_details"] = stop_details;
    }
    vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": { "id": "msg_01", "usage": { "input_tokens": 412, "output_tokens": 0 } },
            })
            .to_string(),
        ),
        ("message_delta".into(), delta.to_string()),
        (
            "message_stop".into(),
            json!({ "type": "message_stop" }).to_string(),
        ),
    ]
}

#[tokio::test]
async fn preserves_refusal_stop_details() {
    let explanation = "This request triggered restrictions on violative cyber content and was blocked under Anthropic's Usage Policy. To learn more, provide feedback, or request an exemption based on how you use Claude, visit our help center: https://support.claude.com/en/articles/14604842-real-time-cyber-safeguards-on-claude.";
    let model = catalog_model("anthropic", "claude-fable-5");
    let events = stop_reason_events(
        "refusal",
        Some(json!({ "type": "refusal", "category": "cyber", "explanation": explanation })),
    );
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("blocked request")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.raw_stop_reason.as_deref(), Some("refusal"));
    assert_eq!(result.error_message.as_deref(), Some(explanation));
}

#[tokio::test]
async fn preserves_sensitive_stop_reasons_with_a_descriptive_error_message() {
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("blocked request")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(
            &stop_reason_events("sensitive", None),
        )))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.raw_stop_reason.as_deref(), Some("sensitive"));
    assert_eq!(
        result.error_message.as_deref(),
        Some("Provider stopped with: sensitive")
    );
}

#[tokio::test]
async fn treats_message_delta_without_usage_as_a_noop_for_usage_accumulation() {
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut events = minimal_events();
    events[4] = (
        "message_delta".into(),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } }).to_string(),
    );
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Say hello.")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.error_message, None);
    assert_eq!(result.usage.input, 12);
    assert_eq!(result.usage.total_tokens, 12);
}

#[tokio::test]
async fn ignores_unknown_sse_events_after_message_stop() {
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut events = minimal_events();
    events.push(("done".into(), "[DONE]".into()));
    events.push(("proxy.stats".into(), "not json".into()));
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Say hello.")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.error_message, None);
    assert_eq!(
        result.content,
        vec![AssistantContentBlock::Text(kiana_ai::types::TextContent {
            text: "Hello".into(),
            ..Default::default()
        })]
    );
}

#[tokio::test]
async fn aborts_when_the_signal_is_cancelled() {
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let mut options = options_with(fetch);
    options.base.signal = Some(token);
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(result.error_message.as_deref(), Some("Request was aborted"));
}

#[tokio::test]
async fn message_start_usage_is_visible_even_when_the_stream_ends_early() {
    // message_start 之后立刻 EOF：错误路径里仍保留输入 token（TS 注释的同一目的）。
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events()[..1])));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(fetch),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Anthropic stream ended before message_stop")
    );
    assert_eq!(result.usage.input, 12);
}

#[tokio::test]
async fn forwards_tool_call_events_and_final_arguments() {
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let events = vec![
        (
            "message_start".into(),
            json!({
                "type": "message_start",
                "message": { "id": "msg_tool", "usage": { "input_tokens": 1, "output_tokens": 0 } },
            })
            .to_string(),
        ),
        (
            "content_block_start".into(),
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": "shell", "input": {} },
            })
            .to_string(),
        ),
        (
            "content_block_delta".into(),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": "{\"command\":" },
            })
            .to_string(),
        ),
        (
            "content_block_delta".into(),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": "\"ls\"}" },
            })
            .to_string(),
        ),
        (
            "content_block_stop".into(),
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta".into(),
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "tool_use" },
                "usage": { "input_tokens": 1, "output_tokens": 2 },
            })
            .to_string(),
        ),
        (
            "message_stop".into(),
            json!({ "type": "message_stop" }).to_string(),
        ),
    ];
    let (result, events) = harness::collect(stream(
        &model,
        &context(None, vec![user_message("run")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::ToolUse);
    let tool_call: &ToolCall = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(tool_call.id, "toolu_1");
    assert_eq!(tool_call.name, "shell");
    assert_eq!(tool_call.arguments.get("command"), Some(&json!("ls")));
    assert!(events.iter().any(|event| event == "toolcall_start"));
    assert_eq!(
        events
            .iter()
            .filter(|event| *event == "toolcall_delta")
            .count(),
        2
    );
    assert!(events.iter().any(|event| event == "toolcall_end"));
}

#[tokio::test]
#[allow(clippy::field_reassign_with_default)]
async fn truncates_and_normalizes_tool_call_ids_in_the_request() {
    // Anthropic 只接受 [a-zA-Z0-9_-] 且 ≤64 的 id：请求里的历史 tool_use id 会被归一化。
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut assistant = AssistantMessage::default();
    assistant.api = "anthropic-messages".into();
    assistant.provider = "anthropic".into();
    assistant.model = "claude-other-model".into();
    assistant.content = vec![AssistantContentBlock::ToolCall(ToolCall {
        id: "bad.id".into(),
        name: "shell".into(),
        arguments: json!({}).as_object().unwrap().clone().into_iter().collect(),
        ..Default::default()
    })];
    let mut messages = vec![
        user_message("hi"),
        Message::Assistant(assistant),
        Message::ToolResult(kiana_ai::types::ToolResultMessage {
            role: Default::default(),
            tool_call_id: "bad.id".into(),
            tool_name: "shell".into(),
            content: vec![kiana_ai::types::UserContentBlock::Text(
                kiana_ai::types::TextContent {
                    text: "ok".into(),
                    ..Default::default()
                },
            )],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 2,
            duration_ms: None,
        }),
    ];
    messages.push(user_message("again"));
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    harness::run_to_result(stream(
        &model,
        &context(None, messages),
        options_with(fetch.clone()),
    ))
    .await;
    let body = fetch.first_json_body().expect("body");
    let serialized = body.to_string();
    assert!(serialized.contains("bad_id"));
    assert!(!serialized.contains("bad.id"));
}

#[tokio::test]
async fn emits_server_side_fallbacks_and_beta_for_models_with_allowed_fallbacks() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let model = catalog_model("anthropic", "claude-fable-5");
    let context = harness::context(Some("System prompt."), vec![user_message("Hello")]);
    let _ = harness::run_to_result(stream(&model, &context, options_with(fetch.clone()))).await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(
        body["fallbacks"],
        json!([{ "model": "claude-opus-4-8" }, { "model": "claude-opus-5" }])
    );
    let request = fetch.requests().remove(0);
    assert!(
        request
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("anthropic-beta")
                && value.contains("server-side-fallback-2026-07-01"))
    );
}

#[tokio::test]
async fn honours_kiana_cache_retention_env_for_long_cache() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let mut options = options_with(fetch.clone());
    options.base.env = Some(indexmap::IndexMap::from([(
        "KIANA_CACHE_RETENTION".to_string(),
        "long".to_string(),
    )]));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let context = harness::context(Some("System prompt."), vec![user_message("Hello")]);
    let _ = harness::run_to_result(stream(&model, &context, options)).await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({ "type": "ephemeral", "ttl": "1h" })
    );
}

// ---------------------------------------------------------------------------
// review 回归：默认超时 / timeoutMs=0 / null 头删除 / validateHeaders / 显式空 beta
// ---------------------------------------------------------------------------

fn header_of<'a>(request: &'a kiana_ai::http::HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn leaves_the_body_unbounded_when_timeout_ms_is_unset() {
    // 默认的 10 分钟上限只在等响应头时生效（SDK `fetchWithTimeout` 在头到达后 clearTimeout），
    // 因此交给 `HttpClient` 的整请求超时必须关闭。
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let _ = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(fetch.clone()),
    ))
    .await;
    assert_eq!(fetch.requests()[0].timeout_ms, None);
}

#[tokio::test]
async fn times_out_while_waiting_for_response_headers() {
    let fetch = harness::MockStreamingClient::delayed(
        MockResponse::sse(sse(&minimal_events())),
        std::time::Duration::from_secs(5),
    );
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let base = kiana_ai::types::StreamOptions {
        api_key: Some("test-key".into()),
        fetch: Some(fetch.clone()),
        timeout_ms: Some(10),
        ..Default::default()
    };
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        AnthropicOptions {
            base,
            ..Default::default()
        },
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    // SDK `APIConnectionTimeoutError` 的文案。
    assert_eq!(result.error_message.as_deref(), Some("Request timed out."));
}

#[tokio::test]
async fn does_not_time_out_while_reading_the_body() {
    let body = sse(&minimal_events()).into_bytes();
    let fetch = harness::MockStreamingClient::streaming_delayed(
        vec![body],
        false,
        std::time::Duration::from_millis(50),
    );
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let base = kiana_ai::types::StreamOptions {
        api_key: Some("test-key".into()),
        fetch: Some(fetch),
        timeout_ms: Some(10),
        ..Default::default()
    };
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        AnthropicOptions {
            base,
            ..Default::default()
        },
    ))
    .await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
}

#[tokio::test]
async fn rejects_a_non_positive_timeout_ms_like_the_sdk() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut options = options_with(fetch.clone());
    options.base.timeout_ms = Some(0);
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("timeout must be a positive integer")
    );
    assert!(fetch.requests().is_empty());
}

#[tokio::test]
async fn null_default_headers_delete_client_auth_headers() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut options = options_with(fetch.clone());
    options.base.api_key = Some("api-key".into());
    options.base.headers = Some(indexmap::IndexMap::from([
        ("x-api-key".to_string(), None),
        (
            "anthropic-dangerous-direct-browser-access".to_string(),
            None,
        ),
    ]));
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Stop);
    let request = fetch.requests().remove(0);
    // SDK `buildHeaders` 的 null 语义：从最终请求（含 SDK 自己加的头）中删除。
    assert_eq!(header_of(&request, "x-api-key"), None);
    assert_eq!(
        header_of(&request, "anthropic-dangerous-direct-browser-access"),
        None
    );
    assert_eq!(header_of(&request, "accept"), Some("application/json"));
    assert_eq!(header_of(&request, "anthropic-version"), Some("2023-06-01"));
}

#[tokio::test]
async fn requires_a_resolvable_auth_method_like_the_sdk() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut options = options_with(fetch.clone());
    options.base.api_key = None;
    options.base.headers = Some(indexmap::IndexMap::from([(
        "cf-aig-authorization".to_string(),
        Some("Bearer x".to_string()),
    )]));
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some(
            "Could not resolve authentication method. Expected one of apiKey, authToken, credentials, config, or profile to be set. Or for one of the \"X-Api-Key\" or \"Authorization\" headers to be explicitly omitted"
        )
    );
    assert!(fetch.requests().is_empty());
}

#[tokio::test]
async fn keeps_an_explicit_empty_anthropic_beta_header() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut options = options_with(fetch.clone());
    options.base.headers = Some(indexmap::IndexMap::from([(
        "anthropic-beta".to_string(),
        Some(String::new()),
    )]));
    let _ = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options,
    ))
    .await;
    let request = fetch.requests().remove(0);
    assert_eq!(header_of(&request, "anthropic-beta"), Some(""));
    let body = fetch.first_json_body().expect("body");
    assert!(body.get("betas").is_none());
}

#[tokio::test]
async fn reports_a_json_null_sse_payload_as_a_wrapped_parse_error() {
    // TS 的 `iterateAnthropicEvents` 在 try 里访问 `event.type`：JSON null 抛 TypeError，
    // 被包装成 “Could not parse Anthropic SSE event …” 并结束流。
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let mut events = minimal_events();
    events[2] = ("content_block_delta".into(), "null".into());
    let result = harness::run_to_result(stream(
        &model,
        &context(None, vec![user_message("Hello")]),
        options_with(MockHttpClient::fixed(MockResponse::sse(sse(&events)))),
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some(
            "Could not parse Anthropic SSE event content_block_delta: Cannot read properties of null (reading 'type'); data=null; raw=event: content_block_delta\\ndata: null"
        )
    );
}

#[tokio::test]
async fn formats_http_error_messages_like_the_sdk_make_message() {
    // SDK `APIError.makeMessage`：`error?.message` 为真值时用（字符串原样 / 其它 JSON），
    // 否则用整个 body 的 JSON；body 不是真值 JSON 时用原始文本。
    let model = catalog_model("anthropic", "claude-haiku-4-5");
    let cases = [
        ("{\"message\":\"\"}", "400 {\"message\":\"\"}"),
        ("{\"message\":123}", "400 123"),
        ("{\"message\":[\"a\"]}", "400 [\"a\"]"),
        (
            "{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"boom\"}}",
            "400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"boom\"}}",
        ),
        ("plain text body", "400 plain text body"),
        ("", "400 status code (no body)"),
    ];
    for (body, expected) in cases {
        let fetch = MockHttpClient::fixed(MockResponse::json(400, body));
        let result = harness::run_to_result(stream(
            &model,
            &context(None, vec![user_message("Hello")]),
            options_with(fetch),
        ))
        .await;
        assert_eq!(result.stop_reason, StopReason::Error, "{body}");
        assert_eq!(result.error_message.as_deref(), Some(expected), "{body}");
    }
}

#[tokio::test]
async fn stream_simple_sends_a_null_max_tokens_when_the_model_has_none() {
    // TS: `clampMaxTokensToContext(..., undefined)` 得到 NaN → `max_tokens: null`；
    // thinking 预算是 NaN，`NaN || 1024` → 1024。
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&minimal_events())));
    let mut model = catalog_model("anthropic", "claude-haiku-4-5");
    model.max_tokens = None;
    model.compat = None;
    let base = kiana_ai::types::StreamOptions {
        api_key: Some("test-key".into()),
        fetch: Some(fetch.clone()),
        ..Default::default()
    };
    let result = harness::run_to_result(
        stream_simple(
            &model,
            &context(None, vec![user_message("Hello")]),
            kiana_ai::types::SimpleStreamOptions {
                base,
                reasoning: Some(kiana_ai::types::ThinkingLevel::Medium),
                ..Default::default()
            },
        )
        .expect("stream simple"),
    )
    .await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body.get("max_tokens"), Some(&Value::Null));
    assert_eq!(body["thinking"]["type"], json!("enabled"));
    assert_eq!(body["thinking"]["budget_tokens"], json!(1024));
}
