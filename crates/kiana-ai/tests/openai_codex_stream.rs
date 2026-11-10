//! `openai-codex-stream.test.ts` 的 Rust 移植。
//!
//! TS 用 `vi.stubGlobal("fetch", ...)` 与真实 `WebSocket`；Rust 侧用
//! [`harness::MockHttpClient`]（SSE）与本地 TCP 服务器（同一端口上按算法处理 HTTP 与
//! WebSocket 升级），因此测试不依赖外网。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use harness::{MockHttpClient, MockResponse, MockStreamingClient};
use kiana_ai::api::openai_codex_responses::{
    OpenAICodexResponsesOptions, close_openai_codex_websocket_sessions,
    get_openai_codex_websocket_debug_stats, reset_openai_codex_websocket_debug_stats, stream,
    stream_simple,
};
use kiana_ai::types::{
    AssistantContentBlock, Model, ModelCost, ModelInputModality, ModelThinkingLevel,
    SimpleStreamOptions, ThinkingLevel, ThinkingLevelMap, Tool, ToolChoice, Transport, UserMessage,
    UserRole,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsHandshakeRequest, Response as WsHandshakeResponse,
};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

fn mock_token() -> String {
    mock_token_for("acc_test")
}

fn mock_token_for(account_id: &str) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(
        json!({ "https://api.openai.com/auth": { "chatgpt_account_id": account_id } }).to_string(),
    );
    format!("aaa.{payload}.bbb")
}

fn model(base_url: &str) -> Model {
    Model {
        id: "gpt-5.1-codex".into(),
        name: "GPT-5.1 Codex".into(),
        api: "openai-codex-responses".into(),
        provider: "openai-codex".into(),
        base_url: base_url.into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(400_000),
        max_tokens: Some(128_000),
        ..Model::default()
    }
}

fn user(text: &str) -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.into()),
        timestamp: 1,
    })
}

fn context() -> kiana_ai::types::TranscriptContext {
    harness::context(
        Some("You are a helpful assistant."),
        vec![user("Say hello")],
    )
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn build_sse_payload(status: &str, include_done: bool, end_turn: Option<bool>) -> String {
    let terminal_type = if status == "incomplete" {
        "response.incomplete"
    } else {
        "response.completed"
    };
    let events = vec![
        json!({
            "type": "response.output_item.added",
            "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] },
        }),
        json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
        json!({ "type": "response.output_text.delta", "delta": "Hello" }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "Hello" }],
            },
        }),
        json!({
            "type": terminal_type,
            "response": {
                "status": status,
                "end_turn": end_turn,
                "incomplete_details": if status == "incomplete" { json!({ "reason": "max_output_tokens" }) } else { Value::Null },
                "usage": {
                    "input_tokens": 5,
                    "output_tokens": 3,
                    "total_tokens": 8,
                    "input_tokens_details": { "cached_tokens": 0 },
                },
            },
        }),
    ];
    let mut body = sse(&events);
    if include_done {
        body.push_str("data: [DONE]\n\n");
    }
    body
}

fn sse_options(fetch: Arc<MockHttpClient>) -> OpenAICodexResponsesOptions {
    let mut base = harness::stream_options(fetch);
    base.api_key = Some(mock_token());
    base.transport = Some(Transport::Sse);
    OpenAICodexResponsesOptions::from_stream_options(base)
}

fn header<'a>(request: &'a kiana_ai::http::HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn text_of(result: &kiana_ai::types::AssistantMessage) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn streams_sse_responses_and_forwards_raw_provider_events() {
    let provider_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let events_for_callback = provider_events.clone();
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&[
        json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] } }),
        json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
        json!({ "type": "response.output_text.delta", "delta": "Hello" }),
        json!({ "type": "response.output_item.done", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": "Hello" }] } }),
        json!({ "type": "response.completed", "response": { "status": "completed", "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8, "input_tokens_details": { "cached_tokens": 0 } } } }),
    ])));
    let mut options = sse_options(fetch.clone());
    options.base.on_provider_stream_event = Some(Arc::new(move |event: Value, _model| {
        if let Some(kind) = event.get("type").and_then(Value::as_str) {
            events_for_callback.lock().unwrap().push(kind.to_string());
        }
    }));
    let (result, events) = harness::collect(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    assert!(events.iter().any(|event| event == "text_delta"));
    assert!(events.iter().any(|event| event == "done"));
    assert_eq!(
        *provider_events.lock().unwrap(),
        vec![
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_item.done",
            "response.completed",
        ]
    );

    let request = fetch.requests().remove(0);
    let expected_authorization = format!("Bearer {}", mock_token());
    assert_eq!(
        header(&request, "Authorization"),
        Some(expected_authorization.as_str())
    );
    assert_eq!(header(&request, "chatgpt-account-id"), Some("acc_test"));
    assert_eq!(
        header(&request, "OpenAI-Beta"),
        Some("responses=experimental")
    );
    assert_eq!(header(&request, "originator"), Some("kiana"));
    assert!(header(&request, "User-Agent").is_some());
    assert_eq!(header(&request, "accept"), Some("text/event-stream"));
    assert_eq!(header(&request, "x-api-key"), None);
    assert_eq!(
        request.url,
        "https://chatgpt.com/backend-api/codex/responses"
    );
}

#[tokio::test]
async fn processes_a_terminal_sse_event_without_a_trailing_blank_line() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(
        build_sse_payload("completed", false, None).trim_end(),
    ));
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        sse_options(fetch),
    ))
    .await;
    assert_eq!(
        result.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        result.error_message
    );
    assert_eq!(text_of(&result), "Hello");
}

#[tokio::test]
async fn completes_after_response_completed_even_when_the_sse_body_stays_open() {
    let body = build_sse_payload("completed", true, Some(false)).into_bytes();
    let fetch = MockStreamingClient::streaming(vec![body], true);
    let mut options = sse_options_dyn(fetch);
    options.base.transport = Some(Transport::Sse);
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        harness::run_to_result(stream(
            &model("https://chatgpt.com/backend-api"),
            &context(),
            options,
        )),
    )
    .await
    .expect("completed stream should finish");
    assert_eq!(text_of(&result), "Hello");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(result.end_turn, Some(false));
}

fn sse_options_dyn(fetch: Arc<MockStreamingClient>) -> OpenAICodexResponsesOptions {
    let base = kiana_ai::types::StreamOptions {
        api_key: Some(mock_token()),
        fetch: Some(fetch),
        transport: Some(Transport::Sse),
        ..Default::default()
    };
    OpenAICodexResponsesOptions::from_stream_options(base)
}

#[tokio::test]
async fn maps_response_incomplete_to_length_with_an_open_body() {
    let body = build_sse_payload("incomplete", false, None).into_bytes();
    let fetch = MockStreamingClient::streaming(vec![body], true);
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        harness::run_to_result(stream(
            &model("https://chatgpt.com/backend-api"),
            &context(),
            sse_options_dyn(fetch),
        )),
    )
    .await
    .expect("incomplete stream should finish");
    assert_eq!(text_of(&result), "Hello");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Length);
}

#[tokio::test]
async fn aborts_sse_fetch_after_the_configured_http_timeout_when_headers_do_not_arrive() {
    let fetch = MockStreamingClient::delayed(
        MockResponse::sse(build_sse_payload("completed", false, None)),
        Duration::from_secs(5),
    );
    let mut options = sse_options_dyn(fetch);
    options.base.timeout_ms = Some(10);
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Codex SSE response headers timed out after 10ms")
    );
}

#[tokio::test]
async fn aborts_sse_body_reads_after_response_headers_arrive() {
    let first = sse(&[
        json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] } }),
        json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
        json!({ "type": "response.output_text.delta", "delta": "one" }),
    ])
    .into_bytes();
    let second = sse(&[
        json!({ "type": "response.output_text.delta", "delta": "two" }),
        json!({ "type": "response.output_item.done", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": "onetwo" }] } }),
        json!({ "type": "response.completed", "response": { "status": "completed", "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8, "input_tokens_details": { "cached_tokens": 0 } } } }),
    ])
    .into_bytes();
    // 第二块延迟 50ms 才到达；客户端在 "one" 之后立刻取消。
    let fetch = MockStreamingClient::new(move |_request| harness::MockOutcome::Streaming {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![first.clone(), second.clone()],
        keep_open: true,
        chunk_delay: Some(Duration::from_millis(50)),
    });
    let token = CancellationToken::new();
    let stream_options = kiana_ai::types::StreamOptions {
        api_key: Some(mock_token()),
        fetch: Some(fetch),
        transport: Some(Transport::Sse),
        signal: Some(token.clone()),
        ..Default::default()
    };
    let options = OpenAICodexResponsesOptions::from_stream_options(stream_options);
    let result_stream = stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    );
    let mut seen = Vec::new();
    loop {
        let Some(event) = result_stream.recv().await else {
            break;
        };
        if let kiana_ai::types::AssistantMessageEvent::TextDelta { delta, .. } = &event {
            seen.push(delta.clone());
            if delta == "one" {
                token.cancel();
            }
        }
        if matches!(
            event,
            kiana_ai::types::AssistantMessageEvent::Done { .. }
                | kiana_ai::types::AssistantMessageEvent::Error { .. }
        ) {
            break;
        }
    }
    let result = result_stream.result().await.expect("result");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Aborted);
    assert_eq!(result.error_message.as_deref(), Some("Request was aborted"));
    assert!(seen.contains(&"one".to_string()));
    assert!(!seen.contains(&"two".to_string()));
}

#[tokio::test]
async fn sets_session_headers_and_prompt_cache_key_when_session_id_is_provided() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = sse_options(fetch.clone());
    options.base.session_id = Some("test-session-123".into());
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "session-id"), Some("test-session-123"));
    assert_eq!(header(&request, "session_id"), None);
    assert_eq!(
        header(&request, "x-client-request-id"),
        Some("test-session-123")
    );
    let body = fetch.first_json_body().expect("body");
    assert_eq!(
        body["prompt_cache_key"],
        Value::String("test-session-123".into())
    );
}

#[tokio::test]
async fn omits_sse_cache_affinity_when_cache_retention_is_none() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = sse_options(fetch.clone());
    options.base.session_id = Some("one-off-summary".into());
    options.base.cache_retention = Some(kiana_ai::types::CacheRetention::None);
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "session-id"), None);
    assert_eq!(header(&request, "x-client-request-id"), None);
    let body = fetch.first_json_body().expect("body");
    assert!(body.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn clamps_prompt_cache_key_and_session_header_to_64_characters() {
    let session_id = "x".repeat(67);
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = sse_options(fetch.clone());
    options.base.session_id = Some(session_id.clone());
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["prompt_cache_key"], Value::String("x".repeat(64)));
    let request = fetch.requests().remove(0);
    let expected = "x".repeat(64);
    assert_eq!(header(&request, "session-id"), Some(expected.as_str()));
    assert_eq!(
        header(&request, "x-client-request-id"),
        Some(expected.as_str())
    );
}

#[tokio::test]
async fn lets_model_and_caller_headers_override_originator_and_user_agent() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut model = model("https://chatgpt.com/backend-api");
    model.headers = Some(indexmap::IndexMap::from([(
        "originator".to_string(),
        "my-app".to_string(),
    )]));
    let mut options = sse_options(fetch.clone());
    options.base.headers = Some(indexmap::IndexMap::from([
        ("user-agent".to_string(), Some("my-app/1.0".to_string())),
        (
            "Authorization".to_string(),
            Some("Bearer ignored".to_string()),
        ),
    ]));
    let _ = harness::run_to_result(stream(&model, &context(), options)).await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "originator"), Some("my-app"));
    assert_eq!(header(&request, "User-Agent"), Some("my-app/1.0"));
    let expected_authorization = format!("Bearer {}", mock_token());
    assert_eq!(
        header(&request, "Authorization"),
        Some(expected_authorization.as_str())
    );
}

#[tokio::test]
async fn preserves_gpt_5_5_xhigh_reasoning_effort_from_simple_options() {
    let mut model = model("https://chatgpt.com/backend-api");
    model.id = "gpt-5.5".into();
    model.thinking_level_map = Some(ThinkingLevelMap::from([(
        ModelThinkingLevel::Xhigh,
        Some("xhigh".into()),
    )]));
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let mut options =
        SimpleStreamOptions {
            base: harness::stream_options(MockHttpClient::fixed(MockResponse::sse(
                build_sse_payload("completed", false, None),
            ))),
            reasoning: Some(ThinkingLevel::Xhigh),
            ..Default::default()
        };
    options.base.api_key = Some(mock_token());
    options.base.transport = Some(Transport::Sse);
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let context = harness::context(None, vec![user("Say hello")]);
    let stream = stream_simple(&model, &context, options).expect("stream");
    let _ = stream.result().await;
    assert_eq!(
        captured.lock().unwrap().clone().expect("payload")["reasoning"],
        json!({ "effort": "xhigh", "summary": "auto" })
    );
}

#[tokio::test]
async fn forwards_required_tool_choice() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = sse_options(fetch.clone());
    options.tool_choice = Some(Value::String("required".into()));
    let context = harness::context_with_tools(
        None,
        vec![user("Do not call ping. Respond with text instead.")],
        vec![Tool {
            name: "ping".into(),
            description: "Ping".into(),
            parameters: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
            }),
            constrained_sampling: None,
        }],
    );
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context,
        options,
    ))
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["tool_choice"], Value::String("required".into()));
}

#[tokio::test]
async fn sets_codex_strict_mode_explicitly_and_honors_constrained_sampling() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let context = harness::context_with_tools(
        None,
        vec![user("Use a tool")],
        vec![
            Tool {
                name: "optional".into(),
                description: "Optional constrained sampling".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                }),
                constrained_sampling: Some(kiana_ai::types::ConstrainedSampling::Disabled(false)),
            },
            Tool {
                name: "strict".into(),
                description: "Strict constrained sampling".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                    "additionalProperties": false,
                }),
                constrained_sampling: Some(kiana_ai::types::ConstrainedSampling::Config(
                    kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                        strict: kiana_ai::types::StrictMode::Prefer,
                    },
                )),
            },
        ],
    );
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context,
        sse_options(fetch.clone()),
    ))
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["tools"][0]["type"], Value::String("function".into()));
    assert_eq!(body["tools"][0]["name"], Value::String("optional".into()));
    assert_eq!(body["tools"][0]["strict"], Value::Null);
    assert_eq!(body["tools"][1]["strict"], Value::Bool(true));
}

#[tokio::test]
async fn clamps_minimal_reasoning_effort_to_low_via_the_thinking_level_map() {
    // TS 的 it.each 矩阵：三个模型 id 共用同一份 `thinkingLevelMap`。
    for model_id in ["gpt-5.3-codex", "gpt-5.4", "gpt-5.5"] {
        let mut candidate = model("https://chatgpt.com/backend-api");
        candidate.id = model_id.into();
        candidate.thinking_level_map = Some(ThinkingLevelMap::from([(
            ModelThinkingLevel::Minimal,
            Some("low".into()),
        )]));
        let captured = Arc::new(Mutex::new(None::<Value>));
        let captured_for_callback = captured.clone();
        let mut options = SimpleStreamOptions {
            base: harness::stream_options(MockHttpClient::fixed(MockResponse::sse(
                build_sse_payload("completed", false, None),
            ))),
            reasoning: Some(ThinkingLevel::Minimal),
            ..Default::default()
        };
        options.base.api_key = Some(mock_token());
        options.base.transport = Some(Transport::Sse);
        options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
            *captured_for_callback.lock().unwrap() = Some(payload);
            None
        }));
        let stream = stream_simple(&candidate, &context(), options).expect("stream");
        let _ = stream.result().await;
        assert_eq!(
            captured.lock().unwrap().clone().expect("payload")["reasoning"]["effort"],
            Value::String("low".into()),
            "{model_id}"
        );
    }
}

#[tokio::test]
async fn uses_simple_tool_choice_objects_in_the_wire_body() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(fetch.clone()),
        tool_choice: Some(ToolChoice::Auto),
        ..Default::default()
    };
    options.base.api_key = Some(mock_token());
    options.base.transport = Some(Transport::Sse);
    let stream = stream_simple(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    )
    .expect("stream");
    let _ = stream.result().await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["tool_choice"], json!({ "type": "auto" }));
}

// ---------------------------------------------------------------------------
// WebSocket 测试：同一端口上同时处理 HTTP POST 与 WebSocket 升级。
// ---------------------------------------------------------------------------

#[derive(Clone)]
#[allow(dead_code)]
struct HttpRecord {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// 一次 WebSocket 握手的请求头。
type HandshakeHeaders = Vec<(String, String)>;

#[derive(Clone)]
struct ServerState {
    http_requests: Arc<Mutex<Vec<HttpRecord>>>,
    ws_requests: Arc<Mutex<Vec<Value>>>,
    ws_connections: Arc<AtomicUsize>,
    /// 每次 WebSocket 握手的请求头（按连接顺序）。
    ws_handshake_headers: Arc<Mutex<Vec<HandshakeHeaders>>>,
    /// 服务端看到连接结束（客户端关闭）的次数。
    ws_closed: Arc<AtomicUsize>,
}

fn server_state() -> ServerState {
    ServerState {
        http_requests: Arc::new(Mutex::new(Vec::new())),
        ws_requests: Arc::new(Mutex::new(Vec::new())),
        ws_connections: Arc::new(AtomicUsize::new(0)),
        ws_handshake_headers: Arc::new(Mutex::new(Vec::new())),
        ws_closed: Arc::new(AtomicUsize::new(0)),
    }
}

#[allow(dead_code)]
type WsHandler = Arc<dyn Fn(&Value, usize) -> WsAction + Send + Sync>;

#[allow(dead_code)]
enum WsAction {
    Send(Vec<Value>),
    /// 逐字发送原始文本帧（空帧等非 JSON 负载无法用 [`Value`] 表达）。
    SendRaw(Vec<String>),
    Close(u16, &'static str),
}

struct TestServer {
    base_url: String,
    #[allow(dead_code)]
    state: ServerState,
    token: CancellationToken,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

async fn start_test_server(
    http_sse: String,
    ws_handler: WsHandler,
    state: ServerState,
) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let token_for_task = token.clone();
    let state_for_task = state.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                _ = token_for_task.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else {
                break;
            };
            let http_sse = http_sse.clone();
            let ws_handler = ws_handler.clone();
            let state = state_for_task.clone();
            tokio::spawn(async move {
                handle_connection(stream, http_sse, ws_handler, state).await;
            });
        }
    });
    TestServer {
        base_url: format!("http://127.0.0.1:{}", address.port()),
        state,
        token,
    }
}

#[allow(clippy::result_large_err)]
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    http_sse: String,
    ws_handler: WsHandler,
    state: ServerState,
) {
    let mut peek = [0u8; 64];
    let peeked = match stream.peek(&mut peek).await {
        Ok(count) => count,
        Err(_) => return,
    };
    let is_websocket = peek[..peeked].starts_with(b"GET ");
    if is_websocket {
        state.ws_connections.fetch_add(1, Ordering::SeqCst);
        let headers_slot = state.ws_handshake_headers.clone();
        let callback = move |request: &WsHandshakeRequest, response: WsHandshakeResponse| {
            let recorded: Vec<(String, String)> = request
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            headers_slot.lock().unwrap().push(recorded);
            Ok(response)
        };
        let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
            return;
        };
        let mut message_index = 0usize;
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                        continue;
                    };
                    state.ws_requests.lock().unwrap().push(value.clone());
                    match ws_handler(&value, message_index) {
                        WsAction::Send(events) => {
                            for event in events {
                                if socket
                                    .send(WsMessage::Text(event.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                        WsAction::SendRaw(messages) => {
                            for message in messages {
                                if socket.send(WsMessage::Text(message.into())).await.is_err() {
                                    return;
                                }
                            }
                        }
                        WsAction::Close(code, reason) => {
                            let _ = socket
                                .close(Some(CloseFrame {
                                    code: CloseCode::from(code),
                                    reason: reason.into(),
                                }))
                                .await;
                            state.ws_closed.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                    }
                    message_index += 1;
                }
                Some(Ok(WsMessage::Close(_))) | None => {
                    state.ws_closed.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) => {
                    state.ws_closed.fetch_add(1, Ordering::SeqCst);
                    return;
                }
            }
        }
    }

    // 普通 HTTP：读到完整请求体。
    let mut data = Vec::new();
    let mut buffer = [0u8; 8192];
    let header_end;
    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) => return,
            Ok(read) => read,
            Err(_) => return,
        };
        data.extend_from_slice(&buffer[..read]);
        if let Some(index) = find_header_end(&data) {
            header_end = index;
            break;
        }
    }
    let head = String::from_utf8_lossy(&data[..header_end]).to_string();
    let mut content_length = 0usize;
    let mut headers = Vec::new();
    let mut request_line = String::new();
    for (index, line) in head.split("\r\n").enumerate() {
        if index == 0 {
            request_line = line.to_string();
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }
    let mut body = data[header_end + 4..].to_vec();
    while body.len() < content_length {
        let read = match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        body.extend_from_slice(&buffer[..read]);
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    state.http_requests.lock().unwrap().push(HttpRecord {
        url: format!("http://127.0.0.1{path}"),
        headers,
        body,
    });
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        http_sse.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(http_sse.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn ws_completed_events(text: &str) -> Vec<Value> {
    vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] } }),
        json!({ "type": "response.output_text.delta", "output_index": 0, "delta": text }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": text }] } }),
        json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed", "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8, "input_tokens_details": { "cached_tokens": 0 } } } }),
    ]
}

fn ws_options(fetch: Arc<MockHttpClient>, transport: Transport) -> OpenAICodexResponsesOptions {
    let mut base = harness::stream_options(fetch);
    base.api_key = Some(mock_token());
    base.transport = Some(transport);
    base.websocket_connect_timeout_ms = Some(200);
    OpenAICodexResponsesOptions::from_stream_options(base)
}

fn header_in<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn wait_for_counter(counter: &AtomicUsize, expected: usize) {
    for _ in 0..200 {
        if counter.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(counter.load(Ordering::SeqCst), expected, "counter timeout");
}

struct SilentServer {
    base_url: String,
    token: CancellationToken,
    connections: Arc<AtomicUsize>,
}

impl Drop for SilentServer {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

/// 接受 TCP 连接但**永不完成** WebSocket 握手（也不关闭）：模拟 Codex 后端卡在握手阶段。
async fn start_silent_server() -> SilentServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let token = CancellationToken::new();
    let token_for_task = token.clone();
    let connections = Arc::new(AtomicUsize::new(0));
    let connections_for_task = connections.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                _ = token_for_task.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else {
                break;
            };
            connections_for_task.fetch_add(1, Ordering::SeqCst);
            let token = token_for_task.clone();
            tokio::spawn(async move {
                let _stream = stream;
                token.cancelled().await;
            });
        }
    });
    SilentServer {
        base_url: format!("http://127.0.0.1:{}", address.port()),
        token,
        connections,
    }
}

#[tokio::test]
async fn streams_websocket_responses() {
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, _index| WsAction::Send(ws_completed_events("Hello"))),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::json(500, "{}"));
    let result = harness::run_to_result(stream(
        &model(&server.base_url),
        &context(),
        ws_options(fetch, Transport::Websocket),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    let requests = state.ws_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["type"], Value::String("response.create".into()));
    assert_eq!(requests[0]["model"], Value::String("gpt-5.1-codex".into()));
}

#[tokio::test]
async fn ignores_empty_websocket_frames() {
    // TS `parseWebSocket`: `if (!text) return;` —— 空帧必须被静默忽略。
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, _index| {
            let mut messages = vec![String::new()];
            messages.extend(ws_completed_events("Hello").iter().map(Value::to_string));
            WsAction::SendRaw(messages)
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::json(500, "{}"));
    let result = harness::run_to_result(stream(
        &model(&server.base_url),
        &context(),
        ws_options(fetch.clone(), Transport::Websocket),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    assert!(fetch.requests().is_empty(), "must not fall back to SSE");
}

#[tokio::test]
async fn reports_json_null_websocket_frames_as_protocol_errors() {
    // TS 在 try 里访问 `parsed.type`：JSON null 抛 TypeError → CodexProtocolError。
    // CodexProtocolError 属于「非传输错误」，不触发 SSE 回退，直接以流错误结束。
    let session = "ws-null-frame";
    close_openai_codex_websocket_sessions(Some(session));
    reset_openai_codex_websocket_debug_stats(Some(session));
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, _index| {
            let mut messages = vec!["null".to_string()];
            messages.extend(ws_completed_events("Hello").iter().map(Value::to_string));
            WsAction::SendRaw(messages)
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = ws_options(fetch, Transport::Websocket);
    options.base.session_id = Some(session.into());
    let result =
        harness::run_to_result(stream(&model(&server.base_url), &context(), options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Invalid Codex WebSocket JSON: Cannot read properties of null (reading 'type')")
    );
    reset_openai_codex_websocket_debug_stats(Some(session));
    close_openai_codex_websocket_sessions(Some(session));
}

#[tokio::test]
async fn releases_the_websocket_cache_entry_when_sending_the_request_fails() {
    let session = "ws-send-failure-release";
    close_openai_codex_websocket_sessions(Some(session));
    reset_openai_codex_websocket_debug_stats(Some(session));

    // 服务端完成握手后先读到客户端写出的第一个字节（证明 send 已经开始），随后立刻以
    // RST 关闭。请求体远大于 socket 缓冲区之和（tcp_wmem 4MB + tcp_rmem 6MB），
    // 因此客户端的 send 必然在写入过程中以错误结束（而不是悄悄成功）。
    let send_started = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let server_token = CancellationToken::new();
    let token = server_token.clone();
    let send_started_for_task = send_started.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                _ = token.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else {
                break;
            };
            let send_started = send_started_for_task.clone();
            tokio::spawn(async move {
                let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let mut first = [0u8; 1];
                let read = tokio::time::timeout(
                    Duration::from_secs(5),
                    AsyncReadExt::read(socket.get_mut(), &mut first),
                )
                .await;
                if matches!(read, Ok(Ok(1))) {
                    send_started.fetch_add(1, Ordering::SeqCst);
                }
                // SO_LINGER(0)：关闭时发 RST（而不是 FIN），客户端阻塞中的写入立即失败。
                // `std::net::TcpStream::set_linger` 在本工具链仍是 unstable，直接 setsockopt。
                let linger = libc::linger {
                    l_onoff: 1,
                    l_linger: 0,
                };
                // SAFETY: fd 在 socket 存活期间有效；linger 是合法的 SO_LINGER 载荷。
                unsafe {
                    libc::setsockopt(
                        AsRawFd::as_raw_fd(socket.get_ref()),
                        libc::SOL_SOCKET,
                        libc::SO_LINGER,
                        std::ptr::addr_of!(linger).cast(),
                        std::mem::size_of::<libc::linger>() as libc::socklen_t,
                    );
                }
                drop(socket);
            });
        }
    });

    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = ws_options(fetch.clone(), Transport::WebsocketCached);
    options.base.session_id = Some(session.into());
    let big = "x".repeat(16 * 1024 * 1024);
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        harness::run_to_result(stream(
            &model(&format!("http://127.0.0.1:{}", address.port())),
            &harness::context(Some("You are a helpful assistant."), vec![user(&big)]),
            options,
        )),
    )
    .await
    .expect("send failure must not hang");
    // 失败发生在任何事件之前 → 回退到 SSE。
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    assert_eq!(
        send_started.load(Ordering::SeqCst),
        1,
        "the client must have started sending the response.create frame"
    );
    {
        let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
        assert_eq!(stats.connections_created, 1);
        assert_eq!(stats.websocket_failures, 1);
    }
    // 清掉「粘滞回退」标记（reset 不动连接缓存），让后续请求再次尝试 WebSocket。
    reset_openai_codex_websocket_debug_stats(Some(session));

    // 缓存条目必须已被删除：下一次请求建立并缓存新连接，第三次请求复用它。
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, _index| WsAction::Send(ws_completed_events("Hello"))),
        state.clone(),
    )
    .await;
    for _ in 0..2 {
        let fetch = MockHttpClient::fixed(MockResponse::json(500, "{}"));
        let mut options = ws_options(fetch, Transport::WebsocketCached);
        options.base.session_id = Some(session.into());
        let result =
            harness::run_to_result(stream(&model(&server.base_url), &context(), options)).await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    }
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(
        stats.connections_created, 1,
        "the first follow-up request must cache its new connection"
    );
    assert_eq!(
        stats.connections_reused, 1,
        "the second follow-up request must reuse the cached connection"
    );
    reset_openai_codex_websocket_debug_stats(Some(session));
    close_openai_codex_websocket_sessions(Some(session));
    server_token.cancel();
}

#[tokio::test]
async fn falls_back_to_sse_when_websocket_fails_before_starting() {
    // 未监听端口：WS 连接立即失败 → 回退到 SSE（注入的 mock）。
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = ws_options(fetch.clone(), Transport::Websocket);
    options.base.session_id = Some("fallback-session".into());
    reset_openai_codex_websocket_debug_stats(Some("fallback-session"));
    let result =
        harness::run_to_result(stream(&model("http://127.0.0.1:1"), &context(), options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    let stats = get_openai_codex_websocket_debug_stats("fallback-session").expect("stats");
    assert_eq!(stats.websocket_failures, 1);
    assert_eq!(stats.sse_fallbacks, 1);
    assert_eq!(stats.websocket_fallback_active, Some(true));
    // 粘滞语义：下一次请求直接走 SSE。
    let fetch2 = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options2 = ws_options(fetch2.clone(), Transport::Websocket);
    options2.base.session_id = Some("fallback-session".into());
    let _ =
        harness::run_to_result(stream(&model("http://127.0.0.1:1"), &context(), options2)).await;
    assert_eq!(fetch2.requests().len(), 1);
    let stats = get_openai_codex_websocket_debug_stats("fallback-session").expect("stats");
    assert_eq!(stats.sse_fallbacks, 2);
    reset_openai_codex_websocket_debug_stats(Some("fallback-session"));
}

#[tokio::test]
async fn falls_back_to_sse_when_a_websocket_is_idle_before_the_first_event() {
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, _index| WsAction::Send(Vec::new())),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = ws_options(fetch.clone(), Transport::Websocket);
    options.base.timeout_ms = Some(50);
    options.base.session_id = Some("idle-session".into());
    reset_openai_codex_websocket_debug_stats(Some("idle-session"));
    let result =
        harness::run_to_result(stream(&model(&server.base_url), &context(), options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    reset_openai_codex_websocket_debug_stats(Some("idle-session"));
}

#[tokio::test]
async fn errors_when_a_websocket_is_idle_after_the_stream_started() {
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, index| {
            if index == 0 {
                WsAction::Send(vec![json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] },
                }), json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "one" })])
            } else {
                WsAction::Send(Vec::new())
            }
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = ws_options(fetch.clone(), Transport::Websocket);
    options.base.timeout_ms = Some(50);
    let result =
        harness::run_to_result(stream(&model(&server.base_url), &context(), options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("WebSocket idle timeout after 50ms"))
    );
    // 已经开始输出后不回退到 SSE。
    assert!(fetch.requests().is_empty());
}

#[tokio::test]
async fn reconnects_once_when_the_websocket_connection_limit_is_reached_before_output() {
    let state = server_state();
    let connections = Arc::new(AtomicUsize::new(0));
    let connections_for_handler = connections.clone();
    let server = start_test_server(
        String::new(),
        Arc::new(move |_request, _index| {
            let connection = connections_for_handler.fetch_add(1, Ordering::SeqCst);
            if connection == 0 {
                WsAction::Send(vec![json!({
                    "type": "error",
                    "code": "websocket_connection_limit_reached",
                    "message": "connection limit reached",
                })])
            } else {
                WsAction::Send(ws_completed_events("Hello"))
            }
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::json(500, "{}"));
    let result = harness::run_to_result(stream(
        &model(&server.base_url),
        &context(),
        ws_options(fetch, Transport::Websocket),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(state.ws_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn sends_only_response_input_deltas_in_websocket_cached_mode() {
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, _index| WsAction::Send(ws_completed_events("Hello"))),
        state.clone(),
    )
    .await;
    let session_id = "cached-session";
    close_openai_codex_websocket_sessions(Some(session_id));
    reset_openai_codex_websocket_debug_stats(Some(session_id));
    let fetch = MockHttpClient::fixed(MockResponse::json(500, "{}"));
    let mut options = ws_options(fetch.clone(), Transport::WebsocketCached);
    options.base.session_id = Some(session_id.into());
    let first = harness::run_to_result(stream(&model(&server.base_url), &context(), options)).await;
    assert_eq!(first.stop_reason, kiana_ai::types::StopReason::Stop);

    // 第二轮：同一 session + 追加真正的第一条回复（带 textSignature）。
    let mut messages = context().0.messages.clone();
    messages.push(kiana_ai::types::Message::Assistant(first.clone()));
    messages.push(user("Again"));
    let second_context = kiana_ai::types::TranscriptContext(kiana_ai::types::Context {
        system_prompt: None,
        messages,
        tools: None,
    });
    let mut options2 = ws_options(fetch, Transport::WebsocketCached);
    options2.base.session_id = Some(session_id.into());
    let second =
        harness::run_to_result(stream(&model(&server.base_url), &second_context, options2)).await;
    assert_eq!(second.stop_reason, kiana_ai::types::StopReason::Stop);

    let requests = state.ws_requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].get("previous_response_id").is_none());
    assert_eq!(
        requests[1]["previous_response_id"],
        Value::String("resp_1".into())
    );
    let delta = requests[1]["input"].as_array().expect("delta input");
    assert_eq!(delta.len(), 1);
    // 缓存键在两次请求间不能变化：prompt_cache_key 相同。
    assert_eq!(
        requests[0]["prompt_cache_key"],
        requests[1]["prompt_cache_key"]
    );
    let stats = get_openai_codex_websocket_debug_stats(session_id).expect("stats");
    assert_eq!(stats.requests, 2);
    assert_eq!(stats.delta_requests, 1);
    assert_eq!(stats.full_context_requests, 1);
    assert_eq!(stats.connections_created, 1);
    assert_eq!(stats.connections_reused, 1);
    close_openai_codex_websocket_sessions(Some(session_id));
    reset_openai_codex_websocket_debug_stats(Some(session_id));
}

#[tokio::test]
async fn streams_function_calls_and_finalizes_arguments() {
    let events = vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "lookup", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "delta": "{\"value\":" }),
        json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "delta": "\"x\"}" }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "lookup", "arguments": "{\"value\":\"x\"}" } }),
        json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed", "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8, "input_tokens_details": { "cached_tokens": 2 }, "output_tokens_details": { "reasoning_tokens": 2 } } } }),
    ];
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&events)));
    let (result, event_kinds) = harness::collect(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        sse_options(fetch),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::ToolUse);
    let tool_call = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(tool_call.id, "call_1|fc_1");
    assert_eq!(tool_call.name, "lookup");
    assert_eq!(tool_call.arguments.get("value"), Some(&json!("x")));
    assert_eq!(result.usage.input, 3);
    assert_eq!(result.usage.cache_read, 2);
    assert_eq!(result.usage.output, 3);
    assert_eq!(result.usage.reasoning, Some(2));
    assert_eq!(result.usage.total_tokens, 8);
    assert!(event_kinds.iter().any(|kind| kind == "toolcall_start"));
    assert_eq!(
        event_kinds
            .iter()
            .filter(|kind| *kind == "toolcall_delta")
            .count(),
        2
    );
    assert!(event_kinds.iter().any(|kind| kind == "toolcall_end"));
}

#[tokio::test]
async fn streams_reasoning_and_persists_the_item_signature() {
    let reasoning_item = json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [{ "type": "summary_text", "text": "thought" }],
        "encrypted_content": "enc",
    });
    let events = vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "reasoning", "id": "rs_1", "summary": [] } }),
        json!({ "type": "response.reasoning_summary_text.delta", "output_index": 0, "delta": "thought" }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": reasoning_item.clone() }),
        json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed", "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 } } }),
    ];
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&events)));
    let (result, event_kinds) = harness::collect(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        sse_options(fetch),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    let thinking = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("thinking");
    assert_eq!(thinking.thinking, "thought");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(kiana_json::js_object::stringify(&reasoning_item).as_str())
    );
    assert!(event_kinds.iter().any(|kind| kind == "thinking_start"));
    assert!(event_kinds.iter().any(|kind| kind == "thinking_delta"));
    assert!(event_kinds.iter().any(|kind| kind == "thinking_end"));
}

#[tokio::test]
async fn surfaces_codex_error_events_as_failures() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse(&[json!({
        "type": "error",
        "code": "invalid_request",
        "message": "bad request",
    })])));
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        sse_options(fetch),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Codex error: bad request")
    );
}

// ---------------------------------------------------------------------------
// review 回归：connect timeout / statusText / reasoning null / verbosity /
// SSE 重试（header timeout、retry-after、指数退避）/ service tier /
// 自动传输 + provider 事件 / 账号隔离 / one-shot 连接 / continuation 恢复
// ---------------------------------------------------------------------------

#[tokio::test]
async fn falls_back_to_sse_when_websocket_connect_does_not_open_before_the_connect_timeout() {
    let server = start_silent_server().await;
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let session = "ws-connect-timeout";
    close_openai_codex_websocket_sessions(Some(session));
    reset_openai_codex_websocket_debug_stats(Some(session));
    let mut options = ws_options(fetch.clone(), Transport::Auto);
    options.base.session_id = Some(session.into());
    options.base.timeout_ms = Some(300_000);
    options.base.websocket_connect_timeout_ms = Some(50);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        harness::run_to_result(stream(&model(&server.base_url), &context(), options)),
    )
    .await
    .expect("connect timeout must fire");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
    assert_eq!(fetch.requests().len(), 1, "falls back to SSE");
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.websocket_failures, 1);
    assert_eq!(stats.sse_fallbacks, 1);
    assert_eq!(stats.websocket_fallback_active, Some(true));
    assert_eq!(
        stats.last_websocket_error.as_deref(),
        Some("WebSocket connect timeout after 50ms")
    );
    reset_openai_codex_websocket_debug_stats(Some(session));
    close_openai_codex_websocket_sessions(Some(session));
}

#[tokio::test]
async fn uses_the_http_status_text_for_empty_error_bodies() {
    let fetch = MockHttpClient::fixed(MockResponse {
        status: 502,
        headers: vec![("content-type".into(), "application/json".into())],
        body: Vec::new(),
    });
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        sse_options(fetch),
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    // TS: `raw || response.statusText || "Request failed"`。
    assert_eq!(result.error_message.as_deref(), Some("Bad Gateway"));
}

#[tokio::test]
async fn keeps_the_requested_reasoning_effort_when_the_level_map_entry_is_null() {
    let mut model = model("https://chatgpt.com/backend-api");
    model.thinking_level_map = Some(ThinkingLevelMap::from([(ModelThinkingLevel::High, None)]));
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = sse_options(fetch);
    options.reasoning_effort = Some("high".into());
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let _ = harness::run_to_result(stream(&model, &context(), options)).await;
    // TS: `model.thinkingLevelMap?.[effort] ?? effort` —— 显式 null 回退到请求值。
    assert_eq!(
        captured.lock().unwrap().clone().expect("payload")["reasoning"],
        json!({ "effort": "high", "summary": "auto" })
    );
}

#[tokio::test]
async fn treats_an_empty_text_verbosity_as_falsy() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let mut options = sse_options(fetch.clone());
    options.text_verbosity = Some(String::new());
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["text"]["verbosity"], Value::String("low".into()));
}

#[tokio::test]
async fn retries_codex_sse_header_timeouts() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_handler = attempts.clone();
    let fetch = MockStreamingClient::new(move |_request| {
        let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
        let response = MockResponse::sse(build_sse_payload("completed", false, None));
        if attempt == 0 {
            harness::MockOutcome::Delayed(response, Duration::from_secs(5))
        } else {
            harness::MockOutcome::Immediate(response)
        }
    });
    let mut options = sse_options_dyn(fetch);
    options.base.timeout_ms = Some(10);
    options.base.max_retries = Some(1);
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn uses_retry_after_ms_for_sse_retries() {
    let fetch = MockHttpClient::sequential(vec![
        MockResponse::json(
            429,
            "{\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"rate limited\"}}",
        )
        .with_headers(vec![("retry-after-ms", "1500")]),
        MockResponse::sse(build_sse_payload("completed", false, None)),
    ]);
    let mut options = sse_options(fetch);
    options.base.max_retries = Some(1);
    let start = tokio::time::Instant::now();
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(start.elapsed(), Duration::from_millis(1500));
}

#[tokio::test(start_paused = true)]
async fn uses_retry_after_seconds_for_sse_retries() {
    let fetch = MockHttpClient::sequential(vec![
        MockResponse::json(
            429,
            "{\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"rate limited\"}}",
        )
        .with_headers(vec![("retry-after", "60")]),
        MockResponse::sse(build_sse_payload("completed", false, None)),
    ]);
    let mut options = sse_options(fetch);
    options.base.max_retries = Some(1);
    let start = tokio::time::Instant::now();
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(start.elapsed(), Duration::from_secs(60));
}

#[tokio::test(start_paused = true)]
async fn uses_retry_after_http_date_for_sse_retries() {
    let retry_at = chrono::Utc::now() + chrono::Duration::seconds(45);
    let header = retry_at.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    let fetch = MockHttpClient::sequential(vec![
        MockResponse::json(
            429,
            "{\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"rate limited\"}}",
        )
        .with_headers(vec![("retry-after", header.as_str())]),
        MockResponse::sse(build_sse_payload("completed", false, None)),
    ]);
    let mut options = sse_options(fetch);
    options.base.max_retries = Some(1);
    let start = tokio::time::Instant::now();
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(
        (44.0..=45.0).contains(&elapsed),
        "expected ~45s retry delay, got {elapsed}"
    );
}

#[tokio::test]
async fn fails_immediately_when_a_retry_delay_exceeds_the_limit() {
    for status in [429u16, 503] {
        let fetch = MockHttpClient::fixed(
            MockResponse::json(
                status,
                "{\"error\":{\"code\":\"temporarily_unavailable\",\"message\":\"retry later\"}}",
            )
            .with_headers(vec![("retry-after", "2")]),
        );
        let mut options = sse_options(fetch.clone());
        options.base.max_retries = Some(3);
        options.base.max_retry_delay_ms = Some(1000);
        let result = harness::run_to_result(stream(
            &model("https://chatgpt.com/backend-api"),
            &context(),
            options,
        ))
        .await;
        assert_eq!(
            result.stop_reason,
            kiana_ai::types::StopReason::Error,
            "{status}"
        );
        assert_eq!(
            result.error_message.as_deref(),
            Some("Server requested 2s retry delay (max: 1s)")
        );
        assert_eq!(fetch.requests().len(), 1, "{status}");
    }
}

#[tokio::test(start_paused = true)]
async fn uses_exponential_backoff_across_repeated_sse_retries_without_retry_headers() {
    let rate_limited = || {
        MockResponse::json(
            429,
            "{\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"rate limited\"}}",
        )
    };
    let fetch = MockHttpClient::sequential(vec![
        rate_limited(),
        rate_limited(),
        rate_limited(),
        MockResponse::sse(build_sse_payload("completed", false, None)),
    ]);
    let mut options = sse_options(fetch.clone());
    options.base.max_retries = Some(3);
    let start = tokio::time::Instant::now();
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(text_of(&result), "Hello");
    assert_eq!(start.elapsed(), Duration::from_millis(1000 + 2000 + 4000));
    assert_eq!(fetch.requests().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn retries_non_retryable_sse_status_errors_with_exponential_backoff() {
    // TS 的内层 throw 会被外层 catch 捕获：400 这类 `isRetryableError()` 为 false 的
    // 状态码在 `attempt < maxRetries` 时同样按 1s、2s… 重试，最后抛出同一条消息。
    let bad_request = || {
        MockResponse::json(
            400,
            "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"bad request\"}}",
        )
    };
    let fetch = MockHttpClient::sequential(vec![bad_request(), bad_request(), bad_request()]);
    let mut options = sse_options(fetch.clone());
    options.base.max_retries = Some(2);
    let start = tokio::time::Instant::now();
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("bad request"));
    assert_eq!(start.elapsed(), Duration::from_millis(1000 + 2000));
    assert_eq!(fetch.requests().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn never_retries_sse_errors_that_mention_a_usage_limit() {
    let usage_limit = || {
        MockResponse::json(
            400,
            "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"You have hit your usage limit\"}}",
        )
    };
    let fetch = MockHttpClient::sequential(vec![usage_limit(), usage_limit()]);
    let mut options = sse_options(fetch.clone());
    options.base.max_retries = Some(2);
    let result = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("You have hit your usage limit")
    );
    assert_eq!(fetch.requests().len(), 1);
}

fn service_tier_sse() -> String {
    sse(&[
        json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] } }),
        json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
        json!({ "type": "response.output_text.delta", "delta": "Hello" }),
        json!({ "type": "response.output_item.done", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": "Hello" }] } }),
        json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "service_tier": "default",
                "usage": {
                    "input_tokens": 1_000_000,
                    "output_tokens": 1_000_000,
                    "total_tokens": 2_000_000,
                    "input_tokens_details": { "cached_tokens": 0 },
                },
            },
        }),
    ])
}

#[tokio::test]
async fn uses_the_client_sent_service_tier_when_codex_echoes_default() {
    for (model_id, tier, multiplier) in [
        ("gpt-5.1-codex", "flex", 0.5),
        ("gpt-5.1-codex", "priority", 2.0),
        ("gpt-5.5", "flex", 0.5),
        ("gpt-5.5", "priority", 2.5),
    ] {
        let mut model = model("https://chatgpt.com/backend-api");
        model.id = model_id.into();
        model.cost = ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.0,
            cache_write: 0.0,
            ..ModelCost::default()
        };
        let fetch = MockHttpClient::fixed(MockResponse::sse(service_tier_sse()));
        let mut options = sse_options(fetch);
        options.service_tier = Some(tier.into());
        let result = harness::run_to_result(stream(&model, &context(), options)).await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
        assert!(
            (result.usage.cost.input - multiplier).abs() < 1e-9,
            "{model_id} {tier}"
        );
        assert!((result.usage.cost.output - 2.0 * multiplier).abs() < 1e-9);
        assert!((result.usage.cost.total - 3.0 * multiplier).abs() < 1e-9);
    }
}

#[tokio::test]
async fn forwards_auto_transport_and_raw_provider_events_from_stream_simple() {
    let state = server_state();
    let server = start_test_server(
        String::new(),
        Arc::new(|_request, _index| {
            WsAction::Send(vec![
                json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] } }),
                json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
                json!({ "type": "response.output_text.delta", "delta": "Hello" }),
                json!({ "type": "response.output_item.done", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": "Hello" }] } }),
                json!({
                    "type": "response.done",
                    "response": {
                        "status": "completed",
                        "end_turn": false,
                        "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8, "input_tokens_details": { "cached_tokens": 0 } },
                    },
                }),
            ])
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::json(500, "unexpected fetch"));
    let session = "session-auto";
    close_openai_codex_websocket_sessions(Some(session));
    reset_openai_codex_websocket_debug_stats(Some(session));
    let event_types = Arc::new(Mutex::new(Vec::<String>::new()));
    let event_models = Arc::new(Mutex::new(Vec::<String>::new()));
    let event_types_for_cb = event_types.clone();
    let event_models_for_cb = event_models.clone();
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = Some(mock_token());
    options.base.session_id = Some(session.into());
    options.base.transport = Some(Transport::Auto);
    options.base.on_provider_stream_event = Some(Arc::new(move |event, model| {
        event_types_for_cb
            .lock()
            .unwrap()
            .push(event["type"].as_str().unwrap_or_default().to_string());
        event_models_for_cb.lock().unwrap().push(model.id.clone());
    }));
    let stream = stream_simple(&model(&server.base_url), &context(), options).expect("stream");
    let result = stream.result().await.expect("result");
    assert_eq!(result.end_turn, Some(false));
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(
        *event_types.lock().unwrap(),
        vec![
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_item.done",
            "response.done",
        ]
    );
    assert_eq!(*event_models.lock().unwrap(), vec!["gpt-5.1-codex"; 5]);
    assert!(fetch.requests().is_empty(), "auto transport uses websocket");
    let headers = state.ws_handshake_headers.lock().unwrap();
    let handshake = headers.last().expect("handshake headers");
    assert_eq!(header_in(handshake, "session-id"), Some(session));
    assert_eq!(header_in(handshake, "session_id"), None);
    assert_eq!(header_in(handshake, "x-client-request-id"), Some(session));
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.cached_context_requests, 1);
    assert_eq!(stats.full_context_requests, 1);
    reset_openai_codex_websocket_debug_stats(Some(session));
    close_openai_codex_websocket_sessions(Some(session));
}

#[tokio::test]
async fn scopes_cached_websockets_to_the_authenticated_account() {
    let state = server_state();
    let response_id = Arc::new(AtomicUsize::new(0));
    let response_id_for_handler = response_id.clone();
    let server = start_test_server(
        String::new(),
        Arc::new(move |_request, _index| {
            let id = response_id_for_handler.fetch_add(1, Ordering::SeqCst) + 1;
            WsAction::Send(vec![json!({
                "type": "response.completed",
                "response": {
                    "id": format!("resp_{id}"),
                    "status": "completed",
                    "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 },
                },
            })])
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::json(500, "unexpected fetch"));
    let session = "shared-session";
    close_openai_codex_websocket_sessions(Some(session));
    reset_openai_codex_websocket_debug_stats(Some(session));
    let empty_context = harness::context(None, Vec::new());
    for account in ["account-a", "account-b", "account-a"] {
        let mut options = ws_options(fetch.clone(), Transport::WebsocketCached);
        options.base.session_id = Some(session.into());
        options.base.api_key = Some(mock_token_for(account));
        let result =
            harness::run_to_result(stream(&model(&server.base_url), &empty_context, options)).await;
        assert_eq!(
            result.stop_reason,
            kiana_ai::types::StopReason::Stop,
            "{:?}",
            result.error_message
        );
    }
    let headers = state.ws_handshake_headers.lock().unwrap();
    let accounts: Vec<&str> = headers
        .iter()
        .map(|headers| header_in(headers, "chatgpt-account-id").unwrap_or_default())
        .collect();
    assert_eq!(accounts, vec!["account-a", "account-b"]);
    let authorizations: Vec<&str> = headers
        .iter()
        .map(|headers| header_in(headers, "authorization").unwrap_or_default())
        .collect();
    assert_eq!(
        authorizations,
        vec![
            format!("Bearer {}", mock_token_for("account-a")),
            format!("Bearer {}", mock_token_for("account-b")),
        ]
    );
    assert!(fetch.requests().is_empty());
    let stats = get_openai_codex_websocket_debug_stats(session).expect("stats");
    assert_eq!(stats.connections_created, 2);
    assert_eq!(stats.connections_reused, 1);
    reset_openai_codex_websocket_debug_stats(Some(session));
    close_openai_codex_websocket_sessions(Some(session));
}

#[tokio::test]
async fn closes_one_shot_websockets_when_cache_retention_is_none() {
    let state = server_state();
    let connections = Arc::new(AtomicUsize::new(0));
    let connections_for_handler = connections.clone();
    let server = start_test_server(
        String::new(),
        Arc::new(move |_request, _index| {
            let id = connections_for_handler.fetch_add(1, Ordering::SeqCst) + 1;
            WsAction::Send(vec![json!({
                "type": "response.completed",
                "response": {
                    "id": format!("resp_{id}"),
                    "status": "completed",
                    "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8 },
                },
            })])
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::json(500, "unexpected fetch"));
    let session = "one-off-summary";
    close_openai_codex_websocket_sessions(Some(session));
    reset_openai_codex_websocket_debug_stats(Some(session));
    for _ in 0..2 {
        let mut options = ws_options(fetch.clone(), Transport::Auto);
        options.base.session_id = Some(session.into());
        options.base.cache_retention = Some(kiana_ai::types::CacheRetention::None);
        let result =
            harness::run_to_result(stream(&model(&server.base_url), &context(), options)).await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    }
    assert_eq!(connections.load(Ordering::SeqCst), 2);
    wait_for_counter(&state.ws_closed, 2).await;
    let requests = state.ws_requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|body| body.get("prompt_cache_key").is_none())
    );
    assert!(get_openai_codex_websocket_debug_stats(session).is_none());
    assert!(fetch.requests().is_empty());
    close_openai_codex_websocket_sessions(Some(session));
}

fn ws_events_for_response(response_id: &str, message_id: &str, text: &str) -> Vec<Value> {
    vec![
        json!({ "type": "response.created", "response": { "id": response_id } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "message", "id": message_id, "role": "assistant", "status": "in_progress", "content": [] } }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "message", "id": message_id, "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": text }] } }),
        json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "status": "completed",
                "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8 },
            },
        }),
    ]
}

async fn assert_recovers_a_missing_cached_websocket_continuation(fallback_to_sse: bool) {
    let state = server_state();
    let total_messages = Arc::new(AtomicUsize::new(0));
    let total_for_handler = total_messages.clone();
    let server = start_test_server(
        String::new(),
        Arc::new(move |_request, _index| {
            let index = total_for_handler.fetch_add(1, Ordering::SeqCst);
            match index {
                0 => WsAction::Send(ws_events_for_response("resp_1", "msg_1", "Hello")),
                1 => WsAction::Send(vec![
                    json!({
                        "type": "codex.rate_limits",
                        "plan_type": "plus",
                        "rate_limits": {
                            "allowed": true,
                            "limit_reached": false,
                            "primary": {
                                "used_percent": 7,
                                "window_minutes": 10080,
                                "reset_after_seconds": 556112,
                                "reset_at": 1785269351,
                            },
                            "secondary": null,
                        },
                        "code_review_rate_limits": null,
                        "additional_rate_limits": null,
                        "credits": { "has_credits": false, "unlimited": false, "balance": "0" },
                        "promo": null,
                    }),
                    json!({
                        "type": "error",
                        "status": 400,
                        "error": {
                            "code": "previous_response_not_found",
                            "message": "Previous response with id 'resp_1' not found.",
                            "param": "previous_response_id",
                        },
                    }),
                ]),
                _ => {
                    if fallback_to_sse {
                        // 第 3 次发送落在新连接上；让它失败以触发 SSE 回退。
                        WsAction::Close(1000, "retry websocket failed")
                    } else {
                        WsAction::Send(ws_events_for_response("resp_2", "msg_2", "Recovered"))
                    }
                }
            }
        }),
        state.clone(),
    )
    .await;
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let session = if fallback_to_sse {
        "missing-continuation-sse".to_string()
    } else {
        "missing-continuation-websocket".to_string()
    };
    close_openai_codex_websocket_sessions(Some(&session));
    reset_openai_codex_websocket_debug_stats(Some(&session));

    let first_context = context();
    let mut options = ws_options(fetch.clone(), Transport::WebsocketCached);
    options.base.session_id = Some(session.clone());
    let first =
        harness::run_to_result(stream(&model(&server.base_url), &first_context, options)).await;
    assert_eq!(first.stop_reason, kiana_ai::types::StopReason::Stop);

    let mut messages = first_context.0.messages.clone();
    messages.push(kiana_ai::types::Message::Assistant(first.clone()));
    messages.push(user("Now finish"));
    let second_context = kiana_ai::types::TranscriptContext(kiana_ai::types::Context {
        system_prompt: None,
        messages,
        tools: None,
    });
    let mut options2 = ws_options(fetch.clone(), Transport::WebsocketCached);
    options2.base.session_id = Some(session.clone());
    let (second, event_kinds) =
        harness::collect(stream(&model(&server.base_url), &second_context, options2)).await;

    assert_eq!(second.stop_reason, kiana_ai::types::StopReason::Stop);
    let expected = if fallback_to_sse {
        "Hello"
    } else {
        "Recovered"
    };
    assert_eq!(text_of(&second), expected);
    assert_eq!(
        event_kinds.iter().filter(|kind| *kind == "start").count(),
        1
    );
    assert!(!event_kinds.iter().any(|kind| kind == "error"));
    assert_eq!(state.ws_connections.load(Ordering::SeqCst), 2);
    let requests = state.ws_requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1]["previous_response_id"],
        Value::String("resp_1".into())
    );
    let delta = requests[1]["input"].as_array().expect("delta input");
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0]["role"], Value::String("user".into()));
    assert_eq!(
        delta[0]["content"][0]["text"],
        Value::String("Now finish".into())
    );
    assert!(requests[2].get("previous_response_id").is_none());
    let full = requests[2]["input"].as_array().expect("full input");
    assert_eq!(full.len(), 3);
    assert_eq!(
        full.last().unwrap()["content"][0]["text"],
        Value::String("Now finish".into())
    );
    let fetch_requests = fetch.requests().len();
    assert_eq!(fetch_requests, if fallback_to_sse { 1 } else { 0 });
    let stats = get_openai_codex_websocket_debug_stats(&session).expect("stats");
    assert_eq!(stats.requests, 3);
    assert_eq!(stats.connections_created, 2);
    assert_eq!(stats.connections_reused, 1);
    assert_eq!(stats.full_context_requests, 2);
    assert_eq!(stats.delta_requests, 1);
    let failures = if fallback_to_sse { 1 } else { 0 };
    assert_eq!(stats.websocket_failures, failures);
    assert_eq!(stats.sse_fallbacks, failures);
    reset_openai_codex_websocket_debug_stats(Some(&session));
    close_openai_codex_websocket_sessions(Some(&session));
}

#[tokio::test]
async fn recovers_a_missing_cached_websocket_continuation_via_websocket() {
    assert_recovers_a_missing_cached_websocket_continuation(false).await;
}

#[tokio::test]
async fn recovers_a_missing_cached_websocket_continuation_via_sse() {
    assert_recovers_a_missing_cached_websocket_continuation(true).await;
}

#[tokio::test]
async fn zstd_compresses_sse_request_bodies() {
    let large_text = "compress me ".repeat(400);
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let large_context = harness::context(
        Some("You are a helpful assistant."),
        vec![user(&large_text)],
    );
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &large_context,
        sse_options(fetch.clone()),
    ))
    .await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "content-encoding"), Some("zstd"));
    let decoded = harness::decode_body(request.body.as_deref().expect("body")).expect("zstd body");
    assert_eq!(
        decoded["input"][0]["content"][0]["text"],
        Value::String(large_text)
    );

    // 小请求体同样压缩（TS 无条件调用 compressRequestBodyZstd）。
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let small_context = harness::context(Some("You are a helpful assistant."), vec![user("hi")]);
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &small_context,
        sse_options(fetch.clone()),
    ))
    .await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "content-encoding"), Some("zstd"));
    let decoded = harness::decode_body(request.body.as_deref().expect("body")).expect("zstd body");
    assert_eq!(decoded["input"][0]["content"][0]["text"], json!("hi"));
}

#[tokio::test]
async fn does_not_set_session_headers_when_session_id_is_not_provided() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(build_sse_payload(
        "completed",
        false,
        None,
    )));
    let _ = harness::run_to_result(stream(
        &model("https://chatgpt.com/backend-api"),
        &context(),
        sse_options(fetch.clone()),
    ))
    .await;
    let request = fetch.requests().remove(0);
    assert_eq!(header(&request, "session-id"), None);
    assert_eq!(header(&request, "session_id"), None);
    assert_eq!(header(&request, "x-client-request-id"), None);
    let body = fetch.first_json_body().expect("body");
    assert!(body.get("prompt_cache_key").is_none());
}
