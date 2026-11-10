//! `packages/agent/test/proxy.test.ts`（4 个 `it`）的移植，外加 spec/agent-codemode.md §4.1
//! 线协议（请求形状、非 2xx 文案、UTF-8 流式解码、取消）的补充用例。
//!
//! TS 用 `vi.stubGlobal("fetch", ...)` 顶替网络；Rust 侧 `stream_proxy` 直接走 reqwest
//! （`ARCHITECTURE.md` §8 第 18 条），所以这里用真实的本地 HTTP 服务器：
//! `wiremock` 覆盖普通响应与请求断言，原始 TCP 服务器覆盖“分片到达/中途取消”。

use std::time::{Duration, Instant};

use kiana_agent_core::proxy::{
    ProxyAssistantMessageEvent, ProxyEventError, ProxyEventState, ProxyStreamOptions,
    process_proxy_event, stream_proxy,
};
use kiana_ai::AssistantMessageEventStream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, JsonObject, Model,
    ModelCost, ModelInputModality, StopReason, ToolCall, TranscriptContext, Usage,
};
use kiana_ai::utils::transcript::normalize_context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// 夹具（对应 proxy.test.ts 顶部的 `model` / `usage` 取值）
// ---------------------------------------------------------------------------

fn test_model() -> Model {
    Model {
        id: "gpt-5.4".into(),
        name: "GPT-5.4".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(400_000),
        max_tokens: Some(128_000),
        ..Model::default()
    }
}

/// `normalizeContext({ systemPrompt: "", messages: [] })` → `{messages: []}`。
fn empty_context() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some(String::new()),
        messages: Vec::new(),
        tools: None,
    })
}

fn options(proxy_url: impl Into<String>) -> ProxyStreamOptions {
    ProxyStreamOptions {
        auth_token: "test-token".into(),
        proxy_url: proxy_url.into(),
        ..ProxyStreamOptions::default()
    }
}

/// 全 0 的 `Usage`（与 TS 的 `usage` 常量同形）。
fn zero_usage_json() -> serde_json::Value {
    serde_json::to_value(Usage::default()).expect("usage serializes")
}

fn sse_body(events: &[serde_json::Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {}\n\n", serde_json::to_string(event).unwrap()))
        .collect()
}

fn event_types(events: &[AssistantMessageEvent]) -> Vec<&'static str> {
    events.iter().map(AssistantMessageEvent::kind).collect()
}

async fn collect(
    stream: &AssistantMessageEventStream,
) -> (Vec<AssistantMessageEvent>, AssistantMessage) {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let result = stream.result().await.expect("stream result");
    (events, result)
}

async fn mount_stream(server: &MockServer, body: impl Into<String>) {
    Mock::given(method("POST"))
        .and(path("/api/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body.into()),
        )
        .mount(server)
        .await;
}

// ---------------------------------------------------------------------------
// 原始 TCP 服务器（分片可控）
// ---------------------------------------------------------------------------

/// 起一个只处理一次连接的 HTTP/1.1 服务器：`parts[0]` 是响应头，其余是 body 分片。
///
/// `delay` 是分片之间的间隔（给消费者留出让出/排空事件队列的机会）；`hold_open` 为真时
/// 写完所有分片后保持连接（用于“读 body 途中取消”的用例）。
async fn serve_parts(parts: Vec<Vec<u8>>, delay: Duration, hold_open: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind raw server");
    let address = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        socket.set_nodelay(true).ok();
        // 读掉请求头（请求体很小，通常与头一起到达）。
        let mut scratch = vec![0u8; 64 * 1024];
        let _ = socket.read(&mut scratch).await;
        for part in parts {
            if socket.write_all(&part).await.is_err() {
                return;
            }
            if socket.flush().await.is_err() {
                return;
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        if hold_open {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
    format!("http://{address}")
}

const RAW_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

/// 把 body 按 `chunk` 字节切成分片，与 [`RAW_HEAD`] 一起交给 [`serve_parts`]。
async fn serve_body(body: &[u8], chunk: usize, delay: Duration) -> String {
    let mut parts = vec![RAW_HEAD.to_vec()];
    parts.extend(body.chunks(chunk.max(1)).map(<[u8]>::to_vec));
    serve_parts(parts, delay, false).await
}

// ---------------------------------------------------------------------------
// 1. toolcall_end 上才出现的元数据必须保留
// ---------------------------------------------------------------------------

/// TS: "preserves tool-call metadata received only on toolcall_end"
#[tokio::test]
async fn preserves_tool_call_metadata_received_only_on_toolcall_end() {
    let body = sse_body(&[
        serde_json::json!({ "type": "start" }),
        serde_json::json!({
            "type": "toolcall_start",
            "contentIndex": 0,
            "id": "call_test|fc_test",
            "toolName": "lookup",
        }),
        serde_json::json!({
            "type": "toolcall_delta",
            "contentIndex": 0,
            "delta": "{\"value\":\"hello\"}",
        }),
        serde_json::json!({
            "type": "toolcall_end",
            "contentIndex": 0,
            "toolCall": {
                "type": "toolCall",
                "id": "call_test|fc_test",
                "name": "lookup",
                "arguments": { "value": "hello" },
                "namespace": "dynamic_tools",
            },
        }),
        serde_json::json!({ "type": "done", "reason": "toolUse", "usage": zero_usage_json() }),
    ]);
    let server = MockServer::start().await;
    mount_stream(&server, body).await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    let end_event = events
        .iter()
        .find(|event| matches!(event, AssistantMessageEvent::ToolCallEnd { .. }))
        .expect("toolcall_end event");
    match end_event {
        AssistantMessageEvent::ToolCallEnd { tool_call, .. } => {
            assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));
            assert_eq!(
                tool_call.arguments.get("value"),
                Some(&serde_json::json!("hello"))
            );
        }
        _ => unreachable!(),
    }

    assert_eq!(result.stop_reason, StopReason::ToolUse);
    match &result.content[0] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "call_test|fc_test");
            assert_eq!(call.name, "lookup");
            assert_eq!(
                call.arguments.get("value"),
                Some(&serde_json::json!("hello"))
            );
            assert_eq!(call.namespace.as_deref(), Some("dynamic_tools"));
        }
        other => panic!("expected toolCall content, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 2. 回归 #8996：最后一帧不以换行结尾时仍要处理终态事件
// ---------------------------------------------------------------------------

/// TS: "processes terminal metadata when the event is not newline-terminated"
#[tokio::test]
async fn processes_terminal_metadata_when_the_event_is_not_newline_terminated() {
    let start = format!("data: {}\n\n", serde_json::json!({ "type": "start" }));
    let done = format!(
        "data: {}",
        serde_json::json!({
            "type": "done",
            "reason": "stop",
            "usage": zero_usage_json(),
            "providerThinkingLevel": "high",
        })
    );
    let server = MockServer::start().await;
    mount_stream(&server, start + &done).await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    assert_eq!(event_types(&events), ["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.provider_thinking_level.as_deref(), Some("high"));
}

// ---------------------------------------------------------------------------
// 3. 没有终态事件就 EOF：合成 error，而不是让消费者永远等下去
// ---------------------------------------------------------------------------

/// TS: "emits an error instead of hanging when the stream ends without a terminal event"
#[tokio::test]
async fn emits_an_error_instead_of_hanging_when_the_stream_ends_without_a_terminal_event() {
    let body = format!("data: {}\n\n", serde_json::json!({ "type": "start" }));
    let server = MockServer::start().await;
    mount_stream(&server, body).await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    assert_eq!(event_types(&events), ["start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Connection closed by proxy server before the response completed")
    );
    // durationMs 由 AssistantMessageEventStream 在 error 事件上写入。
    assert!(result.duration_ms.is_some());
}

// ---------------------------------------------------------------------------
// 4. 回归 #9265：partial 参数线性增长，块上不保留原始文本
// ---------------------------------------------------------------------------

/// TS: "shows growing partial arguments without keeping the raw text on the block, in linear time"
#[tokio::test]
async fn shows_growing_partial_arguments_without_keeping_the_raw_text_on_the_block() {
    let content = "line \"quoted\" \\ end\n".repeat(20_000);
    let text = serde_json::to_string(&serde_json::json!({
        "path": "a.ts",
        "content": content,
    }))
    .unwrap();

    let mut events = vec![
        serde_json::json!({ "type": "start" }),
        serde_json::json!({
            "type": "toolcall_start",
            "contentIndex": 0,
            "id": "call_1",
            "toolName": "write",
        }),
    ];
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let end = (index + 32).min(bytes.len());
        events.push(serde_json::json!({
            "type": "toolcall_delta",
            "contentIndex": 0,
            "delta": std::str::from_utf8(&bytes[index..end]).unwrap(),
        }));
        index = end;
    }
    events.push(serde_json::json!({
        "type": "toolcall_end",
        "contentIndex": 0,
        "toolCall": {
            "type": "toolCall",
            "id": "call_1",
            "name": "write",
            "arguments": { "path": "a.ts", "content": content },
        },
    }));
    events.push(
        serde_json::json!({ "type": "done", "reason": "toolUse", "usage": zero_usage_json() }),
    );
    let body = sse_body(&events);
    drop(events);

    // 分片送达：既覆盖跨 chunk 的行拼接，也让消费者有机会排空事件队列
    // （每个事件都携带一份 partial 快照，TS 里是共享引用，Rust 里是所有权副本）。
    let proxy_url = serve_body(body.as_bytes(), 4096, Duration::from_millis(1)).await;

    let started = Instant::now();
    let stream = stream_proxy(test_model(), empty_context(), options(proxy_url));
    let mut shown = 0usize;
    let mut deltas = 0usize;
    while let Some(event) = stream.recv().await {
        let AssistantMessageEvent::ToolCallDelta { partial, .. } = &event else {
            continue;
        };
        let AssistantContentBlock::ToolCall(block) = &partial.content[0] else {
            panic!("content[0] must stay a toolCall during deltas");
        };
        // `Object.keys(block).sort()` 的等价断言：delta 期间块上只有 type/id/name/arguments。
        assert!(block.thought_signature.is_none());
        assert!(block.namespace.is_none());
        assert!(block.arguments_error.is_none());
        let Some(serde_json::Value::String(partial_text)) = block.arguments.get("content") else {
            continue;
        };
        // 每一步都比较整段文本会让测试本身变成二次方；每 500 个 delta 抽查一次前缀。
        if deltas.is_multiple_of(500) {
            assert!(
                content.starts_with(partial_text),
                "partial arguments must be a prefix of the full text"
            );
        }
        assert!(partial_text.len() >= shown);
        shown = partial_text.len();
        deltas += 1;
    }
    let result = stream.result().await.expect("stream result");
    let elapsed = started.elapsed();

    assert!(shown > content.len() - 64, "shown = {shown}");
    let expected_arguments: JsonObject = serde_json::from_value(serde_json::json!({
        "path": "a.ts",
        "content": content,
    }))
    .unwrap();
    assert_eq!(
        result.content,
        vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "write".into(),
            arguments: expected_arguments,
            ..ToolCall::default()
        })]
    );
    // TS 的断言是 < 10_000 ms（16,000 个 delta / 520 KB）；Rust 侧同样要求线性时间。
    assert!(
        elapsed < Duration::from_secs(10),
        "toolcall_delta handling took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 请求形状 / Authorization / body 键序（spec §4.1）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sends_the_ts_request_body_shape() {
    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[serde_json::json!({
            "type": "done",
            "reason": "stop",
            "usage": zero_usage_json(),
        })]),
    )
    .await;

    let mut stream_options = options(server.uri());
    stream_options.temperature = Some(0.25);
    stream_options.max_tokens = Some(1234);
    let stream = stream_proxy(test_model(), empty_context(), stream_options);
    let _ = collect(&stream).await;

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/api/stream");
    assert_eq!(
        request
            .headers
            .get("authorization")
            .expect("authorization header")
            .to_str()
            .unwrap(),
        "Bearer test-token"
    );
    assert_eq!(
        request
            .headers
            .get("content-type")
            .expect("content-type header")
            .to_str()
            .unwrap(),
        "application/json"
    );

    let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
    let keys: Vec<&String> = body.as_object().expect("object").keys().collect();
    assert_eq!(keys, ["model", "context", "options"]);
    assert_eq!(body["model"]["id"], "gpt-5.4");
    assert_eq!(body["model"]["baseUrl"], "https://api.openai.com/v1");
    assert_eq!(body["context"], serde_json::json!({ "messages": [] }));
    let option_keys: Vec<&String> = body["options"]
        .as_object()
        .expect("options object")
        .keys()
        .collect();
    assert_eq!(option_keys, ["temperature", "maxTokens"]);
    // JSON.stringify(0.25) 与 JSON.stringify(1234)。
    assert_eq!(body["options"]["temperature"], serde_json::json!(0.25));
    assert_eq!(body["options"]["maxTokens"], serde_json::json!(1234));
}

// ---------------------------------------------------------------------------
// 非 2xx 的逐字文案
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reports_status_line_and_json_error_for_non_2xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/stream"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string(serde_json::json!({ "error": "upstream boom" }).to_string()),
        )
        .mount(&server)
        .await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;
    assert_eq!(event_types(&events), ["error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Proxy error: upstream boom")
    );

    // 没有 JSON error 字段时退回状态行文案。
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/stream"))
        .respond_with(ResponseTemplate::new(503).set_body_string("not json"))
        .mount(&server)
        .await;
    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (_, result) = collect(&stream).await;
    assert_eq!(
        result.error_message.as_deref(),
        Some("Proxy error: 503 Service Unavailable")
    );
}

/// null body 状态：`fetch` 对 204/205 返回 `response.ok === true` 但 `body === null`，
/// TS 的 `response.body!.getReader()` 于是抛 TypeError；Rust 保留同一条文案。
///
/// 304 不同：`response.ok` 为 false，先走非 2xx 分支（`response.json()` 失败后保留
/// 状态行文案），永远到不了 `getReader()`。
#[tokio::test]
async fn reports_the_null_body_type_error_for_no_content_responses() {
    for status in [204, 205] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/stream"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;

        let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
        let (events, result) = collect(&stream).await;
        assert_eq!(event_types(&events), ["error"], "status {status}");
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(
            result.error_message.as_deref(),
            Some("Cannot read properties of null (reading 'getReader')")
        );
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/stream"))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;
    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (_, result) = collect(&stream).await;
    assert_eq!(
        result.error_message.as_deref(),
        Some("Proxy error: 304 Not Modified")
    );
}

// ---------------------------------------------------------------------------
// 未知事件类型：`console.warn` 之后继续（不中断流）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ignores_unknown_event_types_and_keeps_streaming() {
    let body = sse_body(&[
        serde_json::json!({ "type": "start" }),
        serde_json::json!({ "type": "future_event", "whatever": 1 }),
        serde_json::json!({ "type": "done", "reason": "stop", "usage": zero_usage_json() }),
    ]);
    let server = MockServer::start().await;
    mount_stream(&server, body).await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;
    assert_eq!(event_types(&events), ["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
}

// ---------------------------------------------------------------------------
// UTF-8 流式解码：多字节字符跨 TCP 分片
// ---------------------------------------------------------------------------

#[tokio::test]
async fn decodes_multibyte_characters_split_across_chunks() {
    let mut body = Vec::new();
    let head = format!(
        "data: {}\n\n",
        serde_json::json!({ "type": "text_start", "contentIndex": 0 })
    );
    body.extend_from_slice(head.as_bytes());

    // 一帧被切在 UTF-8 序列中间：`你`(3 字节) 与 `好`(3 字节) 分别跨片。
    let frame = format!(
        "data: {}\n\n",
        serde_json::json!({ "type": "text_delta", "contentIndex": 0, "delta": "你好" })
    );
    let frame_bytes = frame.as_bytes();
    let split_at = frame.find("你").unwrap() + 1; // `你` 的第一个字节之后
    let split_at2 = frame.find("好").unwrap() + 2; // `好` 的第二个字节之后
    body.extend_from_slice(&frame_bytes[..split_at]);
    let middle = frame_bytes[split_at..split_at2].to_vec();
    let tail = frame_bytes[split_at2..].to_vec();

    let end = format!(
        "data: {}\n\ndata: {}\n\n",
        serde_json::json!({ "type": "text_end", "contentIndex": 0 }),
        serde_json::json!({ "type": "done", "reason": "stop", "usage": zero_usage_json() }),
    );
    let mut parts = vec![RAW_HEAD.to_vec(), body];
    parts.push(middle);
    parts.push(tail);
    parts.push(end.into_bytes());
    let proxy_url = serve_parts(parts, Duration::from_millis(5), false).await;

    let stream = stream_proxy(test_model(), empty_context(), options(proxy_url));
    let (events, result) = collect(&stream).await;

    assert_eq!(
        event_types(&events),
        ["text_start", "text_delta", "text_end", "done"]
    );
    match &events[1] {
        AssistantMessageEvent::TextDelta { delta, .. } => assert_eq!(delta, "你好"),
        other => panic!("expected text_delta, got {other:?}"),
    }
    match &result.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "你好"),
        other => panic!("expected text content, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 取消（AbortSignal → CancellationToken）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn abort_while_waiting_for_headers_uses_the_fetch_abort_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_string("data: {}\n\n"),
        )
        .mount(&server)
        .await;

    let signal = tokio_util::sync::CancellationToken::new();
    let mut stream_options = options(server.uri());
    stream_options.signal = Some(signal.clone());
    let stream = stream_proxy(test_model(), empty_context(), stream_options);
    tokio::time::sleep(Duration::from_millis(50)).await;
    signal.cancel();

    let (events, result) = tokio::time::timeout(Duration::from_secs(5), collect(&stream))
        .await
        .expect("abort must not hang");
    assert_eq!(event_types(&events), ["error"]);
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(
        result.error_message.as_deref(),
        Some("This operation was aborted")
    );
}

#[tokio::test]
async fn abort_while_reading_the_body_uses_the_reader_cancel_message() {
    let head = RAW_HEAD.to_vec();
    let first = format!("data: {}\n\n", serde_json::json!({ "type": "start" })).into_bytes();
    let proxy_url = serve_parts(vec![head, first], Duration::from_millis(5), true).await;

    let signal = tokio_util::sync::CancellationToken::new();
    let mut stream_options = options(proxy_url);
    stream_options.signal = Some(signal.clone());
    let stream = stream_proxy(test_model(), empty_context(), stream_options);

    // 响应头与第一帧已经送达、连接保持打开时取消：body reader 被 cancel，
    // 循环出口抛 `Request aborted by user`。
    let cancel_signal = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_signal.cancel();
    });

    let (events, result) = tokio::time::timeout(Duration::from_secs(5), collect(&stream))
        .await
        .expect("abort must not hang");
    assert_eq!(event_types(&events), ["start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Request aborted by user")
    );
}

// ---------------------------------------------------------------------------
// processProxyEvent 的 13 条映射（进程内直调）
// ---------------------------------------------------------------------------

fn empty_partial() -> AssistantMessage {
    AssistantMessage {
        stop_reason: StopReason::Pending,
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "gpt-5.4".into(),
        timestamp: 1_700_000_000_000,
        ..AssistantMessage::default()
    }
}

#[test]
fn text_events_build_and_mutate_the_blocks_in_place() {
    let mut partial = empty_partial();
    let mut state = ProxyEventState::new();

    let event = process_proxy_event(
        ProxyAssistantMessageEvent::TextStart { content_index: 0 },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        event,
        AssistantMessageEvent::TextStart {
            content_index: 0,
            ..
        }
    ));

    let event = process_proxy_event(
        ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "hello".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    match event {
        AssistantMessageEvent::TextDelta { delta, .. } => assert_eq!(delta, "hello"),
        other => panic!("expected text_delta, got {other:?}"),
    }

    let event = process_proxy_event(
        ProxyAssistantMessageEvent::TextEnd {
            content_index: 0,
            content_signature: Some("sig".into()),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    match event {
        AssistantMessageEvent::TextEnd { content, .. } => assert_eq!(content, "hello"),
        other => panic!("expected text_end, got {other:?}"),
    }
    let AssistantContentBlock::Text(block) = &partial.content[0] else {
        panic!("content[0] must be text");
    };
    assert_eq!(block.text, "hello");
    assert_eq!(block.text_signature.as_deref(), Some("sig"));
}

#[test]
fn thinking_events_build_and_mutate_the_blocks_in_place() {
    let mut partial = empty_partial();
    let mut state = ProxyEventState::new();

    process_proxy_event(
        ProxyAssistantMessageEvent::ThinkingStart { content_index: 1 },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    process_proxy_event(
        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index: 1,
            delta: "hmm".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    let event = process_proxy_event(
        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index: 1,
            content_signature: None,
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();

    match event {
        AssistantMessageEvent::ThinkingEnd { content, .. } => assert_eq!(content, "hmm"),
        other => panic!("expected thinking_end, got {other:?}"),
    }
    // TS 用下标赋值 → 中间留下空洞；Rust 用空文本块占位（见 proxy.rs 的偏差说明）。
    assert_eq!(partial.content.len(), 2);
    let AssistantContentBlock::Thinking(block) = &partial.content[1] else {
        panic!("content[1] must be thinking");
    };
    assert_eq!(block.thinking, "hmm");
    assert_eq!(block.thinking_signature, None);
}

#[test]
fn delta_and_end_events_reject_mismatched_content_types() {
    let mut partial = empty_partial();
    let mut state = ProxyEventState::new();

    let error = process_proxy_event(
        ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap_err();
    assert_eq!(error, ProxyEventError::TextDeltaForNonText);
    assert_eq!(
        error.to_string(),
        "Received text_delta for non-text content"
    );

    let error = process_proxy_event(
        ProxyAssistantMessageEvent::TextEnd {
            content_index: 0,
            content_signature: None,
        },
        &mut partial,
        &mut state,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "Received text_end for non-text content");

    let error = process_proxy_event(
        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index: 0,
            delta: "x".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Received thinking_delta for non-thinking content"
    );

    let error = process_proxy_event(
        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index: 0,
            content_signature: None,
        },
        &mut partial,
        &mut state,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Received thinking_end for non-thinking content"
    );

    let error = process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "x".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Received toolcall_delta for non-toolCall content"
    );
}

#[test]
fn toolcall_events_keep_the_argument_stream_off_the_block() {
    let mut partial = empty_partial();
    let mut state = ProxyEventState::new();

    process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallStart {
            content_index: 0,
            id: "call_1".into(),
            tool_name: "write".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    assert!(state.tool_arguments.contains_key(&0));
    assert_eq!(
        partial.content[0],
        AssistantContentBlock::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "write".into(),
            ..ToolCall::default()
        })
    );

    process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "{\"path\":\"a.ts\"}".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    let AssistantContentBlock::ToolCall(block) = &partial.content[0] else {
        panic!("content[0] must be a toolCall");
    };
    assert_eq!(
        block.arguments.get("path"),
        Some(&serde_json::json!("a.ts"))
    );

    // 非 toolCall 下标上的 toolcall_end 不推事件（TS 返回 undefined）。
    let none = process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index: 3,
            tool_call: ToolCall::default(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap();
    assert!(none.is_none());

    let event = process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index: 0,
            tool_call: ToolCall {
                id: "call_1".into(),
                name: "write".into(),
                arguments: serde_json::from_value(serde_json::json!({ "path": "a.ts" })).unwrap(),
                namespace: Some("dynamic_tools".into()),
                ..ToolCall::default()
            },
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    assert!(
        !state.tool_arguments.contains_key(&0),
        "argument stream is dropped"
    );
    match event {
        AssistantMessageEvent::ToolCallEnd {
            tool_call, partial, ..
        } => {
            assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));
            match &partial.content[0] {
                AssistantContentBlock::ToolCall(block) => {
                    assert_eq!(block.namespace.as_deref(), Some("dynamic_tools"))
                }
                other => panic!("expected toolCall, got {other:?}"),
            }
        }
        other => panic!("expected toolcall_end, got {other:?}"),
    }
}

#[test]
fn done_and_error_events_replace_usage_and_provider_thinking_level() {
    let mut partial = empty_partial();
    let mut state = ProxyEventState::new();

    let usage = Usage {
        input: 7,
        output: 3,
        total_tokens: 10,
        ..Usage::default()
    };

    let event = process_proxy_event(
        ProxyAssistantMessageEvent::Done {
            reason: kiana_agent_core::proxy::ProxyDoneReason::Length,
            usage: usage.clone(),
            provider_thinking_level: Some("high".into()),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    match event {
        AssistantMessageEvent::Done { reason, message } => {
            assert_eq!(reason, kiana_ai::types::DoneReason::Length);
            assert_eq!(message.usage, usage);
            assert_eq!(message.stop_reason, StopReason::Length);
            assert_eq!(message.provider_thinking_level.as_deref(), Some("high"));
        }
        other => panic!("expected done, got {other:?}"),
    }

    let event = process_proxy_event(
        ProxyAssistantMessageEvent::Error {
            reason: kiana_agent_core::proxy::ProxyErrorReason::Aborted,
            error_message: Some("Request aborted by user".into()),
            usage: Usage::default(),
            provider_thinking_level: None,
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    match event {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(reason, kiana_ai::types::ErrorReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
            assert_eq!(
                error.error_message.as_deref(),
                Some("Request aborted by user")
            );
        }
        other => panic!("expected error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 审查回归：非 2xx 错误 body 的取消（TS 的 `response.json()` 被 AbortSignal 打断）
// ---------------------------------------------------------------------------

const RAW_500_HEAD: &[u8] = b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";

#[tokio::test]
async fn abort_while_reading_a_non_2xx_error_body_ends_promptly() {
    // 服务端发完 500 响应头后挂住 body：TS 里 `response.json()` 被 abort 打断，内层 catch 吞掉
    // AbortError，外层仍以状态行文案 + reason=aborted 结束。Rust 侧必须在取消时立刻返回，
    // 不能等错误 body 读完（可能永远等不到）。
    let proxy_url = serve_parts(vec![RAW_500_HEAD.to_vec()], Duration::ZERO, true).await;

    let signal = tokio_util::sync::CancellationToken::new();
    let mut stream_options = options(proxy_url);
    stream_options.signal = Some(signal.clone());
    let stream = stream_proxy(test_model(), empty_context(), stream_options);

    let started = Instant::now();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        signal.cancel();
    });

    let (events, result) = tokio::time::timeout(Duration::from_secs(5), collect(&stream))
        .await
        .expect("abort must not hang on the error body");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "error body read must observe the abort, took {:?}",
        started.elapsed()
    );
    assert_eq!(event_types(&events), ["error"]);
    assert_eq!(result.stop_reason, StopReason::Aborted);
    // TS 实测（node 26）：`Proxy error: 500 Internal Server Error`。
    assert_eq!(
        result.error_message.as_deref(),
        Some("Proxy error: 500 Internal Server Error")
    );
}

// ---------------------------------------------------------------------------
// 审查回归：`new TextDecoder()` 默认丢弃流开头的 BOM
// ---------------------------------------------------------------------------

#[tokio::test]
async fn strips_a_leading_utf8_bom_before_splitting_lines() {
    // 首帧以 BOM 开头，末帧不以换行结尾（同时覆盖 EOF 刷写路径）。
    let body = format!(
        "\u{FEFF}data: {}\n\ndata: {}",
        serde_json::json!({ "type": "start" }),
        serde_json::json!({ "type": "done", "reason": "stop", "usage": zero_usage_json() }),
    );
    let server = MockServer::start().await;
    mount_stream(&server, body).await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    assert_eq!(event_types(&events), ["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
}

// ---------------------------------------------------------------------------
// 审查回归：TS 无校验转换会容忍的事件形态（Rust 侧按 JS 语义补齐）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tolerates_terminal_events_without_usage() {
    // TS：`partial.usage = undefined`，流仍以 done/error resolve。Rust 的 Usage 是必填结构体，
    // 缺失时退化为全 0（见 proxy.rs 模块文档的偏差）。
    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[
            serde_json::json!({ "type": "start" }),
            serde_json::json!({ "type": "done", "reason": "stop" }),
        ]),
    )
    .await;
    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;
    assert_eq!(event_types(&events), ["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.usage, Usage::default());

    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[
            serde_json::json!({ "type": "start" }),
            serde_json::json!({ "type": "error", "reason": "error", "errorMessage": "boom" }),
        ]),
    )
    .await;
    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;
    assert_eq!(event_types(&events), ["start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("boom"));
    assert_eq!(result.usage, Usage::default());
}

#[tokio::test]
async fn applies_js_coercion_to_event_fields() {
    // `contentIndex` 是数字字符串（`content["0"]` === `content[0]`）；`delta` 是数字
    // （`text += 5` → "5"）；缺失的 `delta` 是 `String(undefined)` → "undefined"。
    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[
            serde_json::json!({ "type": "start" }),
            serde_json::json!({ "type": "text_start", "contentIndex": "0" }),
            serde_json::json!({ "type": "text_delta", "contentIndex": "0", "delta": 5 }),
            serde_json::json!({ "type": "text_end", "contentIndex": "0" }),
            serde_json::json!({ "type": "text_start", "contentIndex": "1" }),
            serde_json::json!({ "type": "text_delta", "contentIndex": "1" }),
            serde_json::json!({ "type": "done", "reason": "stop", "usage": zero_usage_json() }),
        ]),
    )
    .await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    assert_eq!(
        event_types(&events),
        [
            "start",
            "text_start",
            "text_delta",
            "text_end",
            "text_start",
            "text_delta",
            "done"
        ]
    );
    match &result.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "5"),
        other => panic!("expected text content, got {other:?}"),
    }
    match &result.content[1] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "undefined"),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[tokio::test]
async fn toolcall_end_preserves_client_fields_when_the_server_omits_them() {
    // TS：`Object.assign(content, toolCall)` 只复制源对象存在的字段。服务端只发 namespace 时，
    // 客户端流式累积的 id/name/arguments 保留（TS 实测结果同形）。
    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[
            serde_json::json!({ "type": "start" }),
            serde_json::json!({
                "type": "toolcall_start",
                "contentIndex": 0,
                "id": "call_1",
                "toolName": "write",
            }),
            serde_json::json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": "{\"path\":\"a.ts\"}" }),
            serde_json::json!({
                "type": "toolcall_end",
                "contentIndex": 0,
                "toolCall": { "namespace": "ns" },
            }),
            serde_json::json!({ "type": "done", "reason": "toolUse", "usage": zero_usage_json() }),
        ]),
    )
    .await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    assert_eq!(
        event_types(&events),
        [
            "start",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done"
        ]
    );
    match &result.content[0] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "call_1");
            assert_eq!(call.name, "write");
            assert_eq!(call.arguments.get("path"), Some(&serde_json::json!("a.ts")));
            assert_eq!(call.namespace.as_deref(), Some("ns"));
        }
        other => panic!("expected toolCall content, got {other:?}"),
    }
}

#[tokio::test]
async fn toolcall_end_on_a_non_toolcall_index_is_ignored_without_inspecting_the_payload() {
    // TS 先看 `partial.content[contentIndex]?.type`，不是 toolCall 就返回 undefined——payload
    // 根本不会被读。因此畸形的 toolCall（这里是数字）不应让整条流报错。
    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[
            serde_json::json!({ "type": "start" }),
            serde_json::json!({ "type": "toolcall_end", "contentIndex": 0, "toolCall": 123 }),
            serde_json::json!({ "type": "done", "reason": "stop", "usage": zero_usage_json() }),
        ]),
    )
    .await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    assert_eq!(event_types(&events), ["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
}

// ---------------------------------------------------------------------------
// 审查回归：非 2xx 的 `${status} ${statusText}` 使用状态行的 reason phrase 原文
// ---------------------------------------------------------------------------

/// 自定义 reason phrase：hyper 把与标准短语不同的原文存进响应扩展。
const RAW_500_CUSTOM_HEAD: &[u8] = b"HTTP/1.1 500 All Broke\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";

#[tokio::test]
async fn uses_the_status_line_reason_phrase_for_non_2xx() {
    // Node 26 实测：`HTTP/1.1 500 All Broke` → statusText "All Broke"。
    let proxy_url = serve_parts(vec![RAW_500_CUSTOM_HEAD.to_vec()], Duration::ZERO, false).await;
    let stream = stream_proxy(test_model(), empty_context(), options(proxy_url));
    let (events, result) = collect(&stream).await;
    assert_eq!(event_types(&events), ["error"]);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Proxy error: 500 All Broke")
    );

    // 空短语：Node 的 statusText 是空串（`Proxy error: 500 `，带尾随空格），不是标准短语。
    let proxy_url = serve_parts(
        vec![b"HTTP/1.1 500 \r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()],
        Duration::ZERO,
        false,
    )
    .await;
    let stream = stream_proxy(test_model(), empty_context(), options(proxy_url));
    let (_, result) = collect(&stream).await;
    assert_eq!(result.error_message.as_deref(), Some("Proxy error: 500 "));

    // 标准短语：hyper 不写扩展，退回 canonical（与状态行原文相同）。
    let proxy_url = serve_parts(
        vec![
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        ],
        Duration::ZERO,
        false,
    )
    .await;
    let stream = stream_proxy(test_model(), empty_context(), options(proxy_url));
    let (_, result) = collect(&stream).await;
    assert_eq!(
        result.error_message.as_deref(),
        Some("Proxy error: 503 Service Unavailable")
    );
}

// ---------------------------------------------------------------------------
// 审查回归：超大 contentIndex 不做物化（TS 的稀疏数组赋值是 O(1)、不占内存）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn accepts_large_content_indexes_without_materializing_padding() {
    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[
            serde_json::json!({ "type": "start" }),
            serde_json::json!({ "type": "text_start", "contentIndex": 1_000_000u64 }),
            serde_json::json!({ "type": "text_delta", "contentIndex": 1_000_000u64, "delta": "x" }),
            serde_json::json!({ "type": "text_end", "contentIndex": 1_000_000u64 }),
            serde_json::json!({ "type": "done", "reason": "stop", "usage": zero_usage_json() }),
        ]),
    )
    .await;

    let started = Instant::now();
    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    // 事件照常推送（TS 的行为），但块存在字典里，不填充 `partial.content`。
    assert_eq!(
        event_types(&events),
        ["start", "text_start", "text_delta", "text_end", "done"]
    );
    match &events[1] {
        AssistantMessageEvent::TextStart { content_index, .. } => {
            assert_eq!(*content_index, 1_000_000);
        }
        other => panic!("expected text_start, got {other:?}"),
    }
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert!(
        result.content.is_empty(),
        "content must not be padded, len = {}",
        result.content.len()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "large index handling took {:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// 审查回归：数组空洞按 `undefined` 处理（delta/end 报错，而不是命中占位块）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_deltas_that_land_on_sparse_content_holes() {
    let server = MockServer::start().await;
    mount_stream(
        &server,
        sse_body(&[
            serde_json::json!({ "type": "start" }),
            serde_json::json!({ "type": "text_start", "contentIndex": 2 }),
            serde_json::json!({ "type": "text_delta", "contentIndex": 0, "delta": "x" }),
            serde_json::json!({ "type": "done", "reason": "stop", "usage": zero_usage_json() }),
        ]),
    )
    .await;

    let stream = stream_proxy(test_model(), empty_context(), options(server.uri()));
    let (events, result) = collect(&stream).await;

    // TS 实测：start, text_start, error（`Received text_delta for non-text content`）。
    assert_eq!(event_types(&events), ["start", "text_start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Received text_delta for non-text content")
    );
    // 空洞用空文本块占位（模块文档记录的偏差），长度与 TS 的稀疏数组一致。
    assert_eq!(result.content.len(), 3);
}

#[test]
fn content_holes_behave_like_undefined_for_every_delta_and_end() {
    let mut partial = empty_partial();
    let mut state = ProxyEventState::new();
    process_proxy_event(
        ProxyAssistantMessageEvent::TextStart { content_index: 2 },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    assert_eq!(partial.content.len(), 3);
    assert!(state.holes.contains(&0) && state.holes.contains(&1));
    assert!(!state.holes.contains(&2));

    let cases = [
        (
            ProxyAssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "x".into(),
            },
            "Received text_delta for non-text content",
        ),
        (
            ProxyAssistantMessageEvent::TextEnd {
                content_index: 1,
                content_signature: None,
            },
            "Received text_end for non-text content",
        ),
        (
            ProxyAssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "x".into(),
            },
            "Received thinking_delta for non-thinking content",
        ),
        (
            ProxyAssistantMessageEvent::ThinkingEnd {
                content_index: 1,
                content_signature: None,
            },
            "Received thinking_end for non-thinking content",
        ),
        (
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: "x".into(),
            },
            "Received toolcall_delta for non-toolCall content",
        ),
    ];
    for (event, message) in cases {
        let error = process_proxy_event(event, &mut partial, &mut state).unwrap_err();
        assert_eq!(error.to_string(), message);
    }

    // `toolcall_end` 落到空洞上：TS 的 `content?.type` 不成立 → 返回 `undefined`。
    let none = process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index: 0,
            tool_call: ToolCall::default(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap();
    assert!(none.is_none());

    // 空洞被后续的 `*_start` 填充后，delta 恢复正常。
    process_proxy_event(
        ProxyAssistantMessageEvent::TextStart { content_index: 0 },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    assert!(!state.holes.contains(&0));
    let event = process_proxy_event(
        ProxyAssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(event, AssistantMessageEvent::TextDelta { .. }));
}

#[test]
fn toolcalls_beyond_the_padding_limit_live_in_the_dictionary() {
    // 超过 MAX_PADDED_CONTENT_INDEX 的 toolCall 也走字典：start/delta/end 全流程可用，
    // `partial.content` 不被填充（见 proxy.rs 模块文档）。
    let mut partial = empty_partial();
    let mut state = ProxyEventState::new();
    let index = 1_000_000;

    process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallStart {
            content_index: index,
            id: "call_1".into(),
            tool_name: "write".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    assert!(partial.content.is_empty());
    assert!(state.far_content.contains_key(&index));

    process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: index,
            delta: "{\"path\":\"a.ts\"}".into(),
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();

    let event = process_proxy_event(
        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index: index,
            tool_call: ToolCall {
                id: "call_1".into(),
                name: "write".into(),
                arguments: serde_json::from_value(serde_json::json!({ "path": "a.ts" })).unwrap(),
                namespace: Some("ns".into()),
                ..ToolCall::default()
            },
        },
        &mut partial,
        &mut state,
    )
    .unwrap()
    .unwrap();
    match event {
        AssistantMessageEvent::ToolCallEnd { tool_call, .. } => {
            assert_eq!(tool_call.id, "call_1");
            assert_eq!(
                tool_call.arguments.get("path"),
                Some(&serde_json::json!("a.ts"))
            );
            assert_eq!(tool_call.namespace.as_deref(), Some("ns"));
        }
        other => panic!("expected toolcall_end, got {other:?}"),
    }
    // 参数流被删除；块仍留在字典里（TS 的数组元素在 toolcall_end 之后仍然存在）。
    assert!(!state.tool_arguments.contains_key(&index));
    assert!(state.far_content.contains_key(&index));
}
