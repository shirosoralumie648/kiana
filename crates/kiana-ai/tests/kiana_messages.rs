//! `test/kiana-messages.test.ts` 的移植。
//!
//! TS 用 `node:http` 起本地服务器；Rust 侧用 wiremock（错误体/响应头/流式事件都走真实 HTTP）。

mod common;

use std::sync::{Arc, Mutex};

use kiana_ai::api::kiana_messages::{KianaMessagesOptions, stream, stream_simple};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessageEvent, Context, DoneReason, Message, Model,
    ModelInputModality, StopReason, StreamOptions, TextContent, TextKind, ToolChoice, Usage,
    UsageCost, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn create_model(base_url: &str) -> Model {
    Model {
        id: "auto".into(),
        name: "Radius Auto".into(),
        api: "kiana-messages".into(),
        provider: "radius".into(),
        base_url: base_url.into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: kiana_ai::types::ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 0.2,
            ..Default::default()
        },
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    let now = now_ms();
    normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("Hello".into()),
            timestamp: now,
        })],
        ..Context::default()
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn usage_json() -> Value {
    json!({
        "input": 10,
        "output": 5,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 15,
        "cost": {"input": 0.1, "output": 0.2, "cacheRead": 0, "cacheWrite": 0, "total": 0.3},
    })
}

async fn mount_sse(server: &MockServer, events: Vec<Value>) {
    let body: String = events
        .iter()
        .map(|event| format!("data: {}\n\n", serde_json::to_string(event).unwrap()))
        .collect();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn streams_text_and_tool_calls_and_resolves_the_terminal_message() {
    let server = common::wiremock_util::start().await;
    mount_sse(
        &server,
        vec![
            json!({"type": "start"}),
            json!({"type": "text_start", "contentIndex": 0}),
            json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}),
            json!({"type": "text_delta", "contentIndex": 0, "delta": "lo"}),
            json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}),
            json!({"type": "toolcall_start", "contentIndex": 1, "id": "call_1", "toolName": "read"}),
            json!({"type": "toolcall_delta", "contentIndex": 1, "delta": "{\"path\":"}),
            json!({"type": "toolcall_delta", "contentIndex": 1, "delta": "\"a.txt\"}"}),
            json!({"type": "toolcall_end", "contentIndex": 1,
                   "toolCall": {"type": "toolCall", "id": "call_1", "name": "read", "arguments": {"path": "a.txt"}}}),
            json!({"type": "done", "reason": "toolUse", "usage": usage_json(),
                   "responseId": "resp_1", "responseModel": "claude-opus-5-5",
                   "providerThinkingLevel": "high"}),
        ],
    )
    .await;

    let model = create_model(&format!("{}/v1", server.uri()));
    let stream = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                session_id: Some("session-1".into()),
                max_tokens: Some(100),
                headers: Some(
                    [("x-custom".to_string(), Some("1".to_string()))]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            tool_choice: Some(json!("auto")),
            ..Default::default()
        },
    );

    let mut events = Vec::new();
    let mut partial_stop_reasons = Vec::new();
    while let Some(event) = stream.recv().await {
        if let Some(partial) = event.partial() {
            partial_stop_reasons.push(partial.stop_reason);
        }
        events.push(event);
    }
    let message = stream.result().await.expect("result");

    assert_eq!(partial_stop_reasons[0], StopReason::Pending);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.usage.input, 10);
    assert_eq!(message.usage.output, 5);
    assert_eq!(message.usage.total_tokens, 15);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(message.provider_thinking_level.as_deref(), Some("high"));
    assert_eq!(message.model, "auto");
    assert_eq!(message.response_model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(message.provider, "radius");
    assert_eq!(message.content.len(), 2);
    match &message.content[0] {
        AssistantContentBlock::Text(text) => {
            assert_eq!(text.text, "Hello");
            assert_eq!(text.text_signature, None);
        }
        other => panic!("expected text, got {other:?}"),
    }
    match &message.content[1] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "call_1");
            assert_eq!(call.name, "read");
            assert_eq!(
                call.arguments.get("path").and_then(Value::as_str),
                Some("a.txt")
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::TextDelta { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AssistantMessageEvent::ToolCallEnd { .. }))
            .count(),
        1
    );

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url.path(), "/v1/messages");
    let headers: std::collections::HashMap<_, _> = request
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_lowercase(),
                value.to_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Bearer test-key")
    );
    assert_eq!(headers.get("x-custom").map(String::as_str), Some("1"));

    let body: Value = serde_json::from_slice(&request.body).expect("json body");
    assert_eq!(
        body,
        json!({
            "model": "auto",
            "context": {"messages": [{"role": "user", "content": "Hello", "timestamp": body["context"]["messages"][0]["timestamp"]}]},
            "options": {"maxTokens": 100, "sessionId": "session-1", "toolChoice": "auto"},
        })
    );
}

#[tokio::test]
async fn accepts_lone_surrogate_escapes_like_json_parse() {
    // serde_json 会拒绝 `\ud800`；JSON.parse 接受（Rust 侧按 ARCHITECTURE §8 第 31 条落成 U+FFFD）。
    let server = common::wiremock_util::start().await;
    let body = concat!(
        "data: {\"type\":\"start\"}\n\n",
        "data: {\"type\":\"text_start\",\"contentIndex\":0}\n\n",
        "data: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"\\ud800\"}\n\n",
        "data: {\"type\":\"text_end\",\"contentIndex\":0,\"content\":\"\\ud800\"}\n\n",
        "data: {\"type\":\"done\",\"reason\":\"stop\",\"usage\":{\"input\":1,\"output\":1,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":2},\"responseId\":\"resp_surrogate\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let model = create_model(&format!("{}/v1", server.uri()));
    let stream = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let message = stream.result().await.expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("resp_surrogate"));
    match &message.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "\u{fffd}"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn forwards_parsed_wire_events_in_order_before_converting_them() {
    let server = common::wiremock_util::start().await;
    let wire_events = vec![
        json!({"type": "start"}),
        json!({"type": "text_start", "contentIndex": 0}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hello", "gatewayField": "upstream-value"}),
        json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}),
        json!({"type": "done", "reason": "stop", "usage": usage_json(), "responseId": "resp_1"}),
    ];
    mount_sse(&server, wire_events.clone()).await;
    let model = create_model(&format!("{}/v1", server.uri()));

    let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let received_in_callback = Arc::clone(&received);
    let event_models: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let models_in_callback = Arc::clone(&event_models);
    let message = stream_simple(
        &model,
        &context(),
        kiana_ai::types::SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                on_provider_stream_event: Some(Arc::new(move |event, event_model| {
                    received_in_callback.lock().unwrap().push(event);
                    models_in_callback
                        .lock()
                        .unwrap()
                        .push(event_model.provider.clone());
                })),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("stream")
    .result()
    .await
    .expect("result");

    assert_eq!(*received.lock().unwrap(), wire_events);
    assert_eq!(event_models.lock().unwrap().len(), wire_events.len());
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert!(message.response_model.is_none());
    assert_eq!(message.content.len(), 1);
}

#[tokio::test]
async fn unknown_event_types_do_not_drop_or_break_the_stream() {
    // TS 的 convertEvent 不校验形状：未知类型原样透传（`{...event, partial}`），
    // 不影响后续事件。旧实现用封闭 serde 枚举，未知类型整条丢弃。
    let server = common::wiremock_util::start().await;
    mount_sse(
        &server,
        vec![
            json!({"type": "start"}),
            json!({"type": "text_start", "contentIndex": 0}),
            json!({"type": "text_delta", "contentIndex": 0, "delta": "hi"}),
            json!({"type": "future_event", "foo": {"bar": 1}}),
            json!({"type": "text_end", "contentIndex": 0, "content": "hi"}),
            json!({"type": "done", "reason": "stop", "usage": usage_json(), "responseId": "resp_1"}),
        ],
    )
    .await;
    let model = create_model(&format!("{}/v1", server.uri()));
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_cb = Arc::clone(&seen);
    let stream = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                on_provider_stream_event: Some(Arc::new(move |event, _model| {
                    seen_cb.lock().unwrap().push(event);
                })),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let message = stream.result().await.expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    match &message.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "hi"),
        other => panic!("expected text, got {other:?}"),
    }
    // 未知事件仍然交给 onProviderStreamEvent（TS 的透传语义的可观察部分）。
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|event| event.get("type").and_then(Value::as_str) == Some("future_event"))
    );
}

#[tokio::test]
async fn a_done_event_with_a_partial_rewrite_object_is_still_honored() {
    // `appendRewriteDiagnostic` 只要求 rewrite 为真值并展开字段；缺字段不应丢掉 done。
    let server = common::wiremock_util::start().await;
    mount_sse(
        &server,
        vec![
            json!({"type": "start"}),
            json!({"type": "done", "reason": "stop", "usage": usage_json(),
                   "responseId": "resp_rewrite", "rewrite": {"reason": "policy"}}),
        ],
    )
    .await;
    let model = create_model(&format!("{}/v1", server.uri()));
    let message = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    let diagnostic = message
        .diagnostics
        .as_ref()
        .and_then(|diagnostics| {
            diagnostics
                .iter()
                .find(|diagnostic| diagnostic.kind == "kiana_messages_rewrite")
        })
        .expect("rewrite diagnostic");
    let details = serde_json::to_value(diagnostic.details.clone().unwrap_or_default()).unwrap();
    assert_eq!(details, json!({"reason": "policy"}));
}

#[tokio::test]
async fn a_toolcall_end_event_without_a_tool_call_payload_is_still_honored() {
    // TS: `Object.assign(block, event.toolCall)` 对 undefined 是空操作，不抛错。
    let server = common::wiremock_util::start().await;
    mount_sse(
        &server,
        vec![
            json!({"type": "start"}),
            json!({"type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "read"}),
            json!({"type": "toolcall_delta", "contentIndex": 0, "delta": "{\"path\":"}),
            json!({"type": "toolcall_end", "contentIndex": 0}),
            json!({"type": "done", "reason": "toolUse", "usage": usage_json(), "responseId": "resp_1"}),
        ],
    )
    .await;
    let model = create_model(&format!("{}/v1", server.uri()));
    let message = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    match &message.content[0] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "call_1");
            assert_eq!(call.name, "read");
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn appends_debug_one_and_reports_response_headers_via_on_response() {
    let server = common::wiremock_util::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-kiana-gateway-upstream-provider", "anthropic")
                .set_body_string("data: {\"type\":\"done\",\"reason\":\"stop\",\"usage\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":0,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}}}\n\n"),
        )
        .mount(&server)
        .await;
    let model = create_model(&format!("{}/v1", server.uri()));

    let observed: Arc<Mutex<Option<kiana_ai::types::ProviderResponse>>> =
        Arc::new(Mutex::new(None));
    let observed_cb = Arc::clone(&observed);
    let message = stream_simple(
        &model,
        &context(),
        kiana_ai::types::SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                on_response: Some(Arc::new(move |response, _model| {
                    *observed_cb.lock().unwrap() = Some(response);
                })),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("stream")
    .result()
    .await
    .expect("result");
    // SimpleStreamOptions 没有 debug 字段；debug 由 full options 的 extra 传入。
    assert_eq!(message.stop_reason, StopReason::Stop);
    let headers = observed.lock().unwrap().clone().expect("headers");
    assert_eq!(
        headers
            .headers
            .get("x-kiana-gateway-upstream-provider")
            .map(String::as_str),
        Some("anthropic")
    );

    // 完整入口：debug 通过 extra 打开。
    let stream = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                ..Default::default()
            },
            debug: true,
            ..Default::default()
        },
    );
    let _ = stream.result().await;
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests[1].url.query(), Some("debug=1"));
}

#[tokio::test]
async fn surfaces_backend_error_responses_with_diagnostics() {
    let server = common::wiremock_util::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("content-type", "application/json")
                .set_body_string(
                    serde_json::to_string(
                        &json!({"error": {"message": "Token expired", "code": "unauthorized"}}),
                    )
                    .unwrap(),
                ),
        )
        .mount(&server)
        .await;
    let model = create_model(&format!("{}/v1", server.uri()));

    let message = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("stale".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.clone().unwrap();
    assert!(error.contains("401"));
    assert!(error.contains("Token expired"));
    assert!(error.contains("unauthorized"));
    let diagnostics = message.diagnostics.as_ref().expect("diagnostics");
    assert_eq!(diagnostics[0].kind, "kiana_messages_response_failure");
    assert_eq!(
        diagnostics[0]
            .details
            .as_ref()
            .and_then(|details| details.get("status"))
            .and_then(Value::as_i64),
        Some(401)
    );
}

#[tokio::test]
async fn propagates_server_sent_error_events() {
    let server = common::wiremock_util::start().await;
    mount_sse(
        &server,
        vec![
            json!({"type": "start"}),
            json!({"type": "error", "reason": "error", "usage": usage_json(),
                   "errorMessage": "Upstream failed", "responseModel": "claude-opus-5-5"}),
        ],
    )
    .await;
    let model = create_model(&format!("{}/v1", server.uri()));
    let message = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Upstream failed"));
    assert_eq!(message.usage.input, 10);
    assert_eq!(message.response_model.as_deref(), Some("claude-opus-5-5"));
}

#[tokio::test]
async fn errors_when_no_api_key_is_provided() {
    let model = create_model("http://127.0.0.1:1/v1");
    let message = stream(&model, &context(), KianaMessagesOptions::default())
        .result()
        .await
        .expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("No API key provided")
    );
}

#[tokio::test]
async fn errors_when_the_stream_ends_without_a_terminal_event() {
    let server = common::wiremock_util::start().await;
    mount_sse(
        &server,
        vec![
            json!({"type": "start"}),
            json!({"type": "text_start", "contentIndex": 0}),
            json!({"type": "text_delta", "contentIndex": 0, "delta": "partial"}),
        ],
    )
    .await;
    let model = create_model(&format!("{}/v1", server.uri()));
    let message = stream(
        &model,
        &context(),
        KianaMessagesOptions {
            base: StreamOptions {
                api_key: Some("test-key".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("stream ended without a terminal event")
    );
}

#[tokio::test]
async fn api_factory_is_registered() {
    let provider = kiana_ai::api::kiana_messages::kiana_messages_api();
    let model = create_model("http://127.0.0.1:1/v1");
    let stream = kiana_ai::models::ProviderStreams::stream(
        provider.as_ref(),
        &model,
        &context(),
        StreamOptions {
            api_key: Some("k".into()),
            ..Default::default()
        },
    );
    let message = stream.result().await.expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
}

#[test]
fn usage_defaults_match_ts() {
    let usage = Usage {
        cost: UsageCost::default(),
        ..Default::default()
    };
    assert_eq!(usage.total_tokens, 0);
    let _ = DoneReason::Stop;
    let _ = ToolChoice::Auto;
    let _ = TextContent {
        kind_text: TextKind::Text,
        text: String::new(),
        text_signature: None,
    };
}
