//! 传输层 HTTP 测试套件（16 个 `it` 用例，移植自 `packages/mcp/test/streamable-http.test.ts`）。
//!
//! TS 的测试用 `node:http` 起真实回环服务器（`test/helpers.ts`），并用 `McpClient` 驱动传输。
//! `McpClient` 属于另一个单元（`src/client.rs` 是占位模块），因此这里的 **传输层** 测试直接驱动
//! [`StreamableHttpTransport`]，把 TS 测试里 `client.*` 的那一层换成测试内的最小握手辅助
//! （`connect` = `start` + `initialize` + `notifications/initialized`；`call_tool`/`list_tools` =
//! `send` + 等 `emitMessage` 答复）。网络轨迹（方法、头、体、顺序）与断言逐条对应 TS 版本；
//! 差异见各测试内的注释与文件末尾的 `deviations` 说明。
//!
//! 服务器与请求记录来自 `tests/support/mod.rs`（`test/helpers.ts` 的移植：绑定 127.0.0.1:0、
//! `readBody`、`closeServers`）。

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_mcp::auth_provider::{
    AuthProvider, FetchByteStream, FetchResponse, McpFetch, McpTransportError, ReqwestFetch,
    UnauthorizedContext,
};
use kiana_mcp::protocol_types::LATEST_PROTOCOL_VERSION;
use kiana_mcp::transports::streamable_http::{
    ConsumeSseOptions, SseEvent, StreamableHttpReconnectOptions, StreamableHttpTransport,
    StreamableHttpTransportOptions, consume_sse_stream,
};
use kiana_mcp::transports::{McpTransport, McpTransportSendOptions};
use serde_json::{Value, json};
use support::{HttpRequest, RecordedRequest, ResponseWriter, TestServer, listen};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_util::sync::CancellationToken;
use url::Url;

// ---------------------------------------------------------------------------
// 测试内的“客户端”辅助（TS 里由 McpClient 提供的那一层）
// ---------------------------------------------------------------------------

/// `message.id === id`（数字按 f64 比较：解析出来的 `3` 是 JSON 整数，`3.0` 是浮点，
/// 而 TS 里两者是同一个 Number）。
pub fn id_matches(message: &Value, id: f64) -> bool {
    message.get("id").and_then(Value::as_f64) == Some(id)
}

/// 一个通道化的监听器集合：把 `emitMessage`/`emitError` 转成可 await 的事件流。
pub struct Harness {
    messages: Mutex<UnboundedReceiver<Value>>,
    errors: Mutex<UnboundedReceiver<McpTransportError>>,
    closes: UnboundedReceiver<()>,
    transport: Arc<StreamableHttpTransport>,
}

impl Harness {
    pub fn new(transport: StreamableHttpTransport) -> Arc<Self> {
        let (message_tx, message_rx) = unbounded_channel();
        let (error_tx, error_rx) = unbounded_channel();
        let (close_tx, close_rx) = unbounded_channel();
        transport.on_message(Arc::new(move |message: &Value| {
            let _ = message_tx.send(message.clone());
            Ok(())
        }));
        transport.on_error(Arc::new(move |error: &McpTransportError| {
            let _ = error_tx.send(error.clone());
            Ok(())
        }));
        transport.on_close(Arc::new(move || {
            let _ = close_tx.send(());
            Ok(())
        }));
        Arc::new(Harness {
            messages: Mutex::new(message_rx),
            errors: Mutex::new(error_rx),
            closes: close_rx,
            transport: Arc::new(transport),
        })
    }

    pub fn transport(&self) -> &Arc<StreamableHttpTransport> {
        &self.transport
    }

    /// 等一条满足谓词的消息（超时后 panic）。
    pub async fn next_message<F>(&self, predicate: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let received = {
                let mut receiver = self.messages.lock().unwrap();
                receiver.try_recv().ok()
            };
            if let Some(message) = received {
                if predicate(&message) {
                    return message;
                }
                continue;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for a matching message");
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// 收集错误（非阻塞）。
    pub fn drain_errors(&self) -> Vec<McpTransportError> {
        let mut receiver = self.errors.lock().unwrap();
        let mut errors = Vec::new();
        while let Ok(error) = receiver.try_recv() {
            errors.push(error);
        }
        errors
    }

    /// 收集已发出的消息（非阻塞）。
    pub fn drain_messages(&self) -> Vec<Value> {
        let mut receiver = self.messages.lock().unwrap();
        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            messages.push(message);
        }
        messages
    }

    /// 等一个错误。
    pub async fn next_error(&self) -> McpTransportError {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(error) = self.errors.lock().unwrap().try_recv() {
                return error;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for an error");
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// 等 close 事件。
    pub async fn next_close(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), self.closes.recv())
            .await
            .expect("close event")
            .expect("close channel");
    }
}

/// `protocolHandler`：TS 测试里的默认服务器行为。
async fn protocol_handler(request: HttpRequest, response: ResponseWriter, body: Option<Value>) {
    if request.method == "GET" {
        response.send(405, &[], "").await;
        return;
    }
    if request.method == "DELETE" {
        response.send(200, &[], "").await;
        return;
    }
    let message = match body.or(request.message.clone()) {
        Some(message) => message,
        None => {
            response.send(400, &[], "no body").await;
            return;
        }
    };
    if message.get("id").is_none() {
        response.send(202, &[], "").await;
        return;
    }
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            response
                .send(
                    200,
                    &[
                        ("content-type", "application/json"),
                        ("mcp-session-id", "session-1"),
                    ],
                    &json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {
                            "protocolVersion": LATEST_PROTOCOL_VERSION,
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "http-fixture", "version": "1.0.0" },
                        },
                    })
                    .to_string(),
                )
                .await;
        }
        Some("tools/list") => {
            response
                .send(
                    200,
                    &[("content-type", "application/json")],
                    &json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": { "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] },
                    })
                    .to_string(),
                )
                .await;
        }
        _ => {
            response
                .stream_head(200, &[("content-type", "text/event-stream")])
                .await;
            response.stream_chunk("id: tool-result\n").await;
            response
                .stream_chunk(&format!(
                    "data: {}\n\n",
                    json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": { "content": [{ "type": "text", "text": "hello" }] },
                    })
                ))
                .await;
            response.stream_end().await;
        }
    }
}

/// `startServer(handler)`：记录请求并把它们交给 handler。
async fn start_server<F, Fut>(handler: F) -> TestServer
where
    F: Fn(HttpRequest, ResponseWriter, Arc<Mutex<Vec<RecordedRequest>>>) -> Fut
        + Send
        + Sync
        + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let records: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&records);
    listen(move |request, response| {
        let records = Arc::clone(&sink);
        handler(request, response, records)
    })
    .await
}

/// 默认的 `startServer(protocolHandler)`。
async fn start_protocol_server() -> TestServer {
    start_server(|request, response, _records| async move {
        protocol_handler(request, response, None).await;
    })
    .await
}

/// 构造传输（`new StreamableHttpTransport({ url, ... })`）。
fn options(url: &str) -> StreamableHttpTransportOptions {
    StreamableHttpTransportOptions {
        url: url.to_owned(),
        ..StreamableHttpTransportOptions::default()
    }
}

/// `client.connect(transport)`：`start` + `initialize` + `notifications/initialized`。
async fn connect(harness: &Arc<Harness>) {
    harness.transport().start().await.expect("start");
    send_request(
        harness,
        1.0,
        "initialize",
        Some(json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "http-test", "version": "1.0.0" },
        })),
    )
    .await;
    let result = harness
        .next_message(|message| id_matches(message, 1.0))
        .await;
    let version = result["result"]["protocolVersion"]
        .as_str()
        .expect("protocolVersion")
        .to_owned();
    harness.transport().set_protocol_version(&version);
    send_notification(harness, "notifications/initialized", None).await;
}

/// `client.request(...)`：`send` 一个请求（答复经 `emitMessage` 到达）。
async fn send_request(harness: &Arc<Harness>, id: f64, method: &str, params: Option<Value>) {
    let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
    if let Some(params) = params {
        message["params"] = params;
    }
    harness
        .transport()
        .send(message, McpTransportSendOptions::none())
        .await
        .expect("send request");
}

/// `client.callTool(name, args)`：等带同一 id 的答复，返回 `result`。
async fn call_tool(harness: &Arc<Harness>, id: f64, name: &str) -> Value {
    send_request(
        harness,
        id,
        "tools/call",
        Some(json!({ "name": name, "arguments": {} })),
    )
    .await;
    let message = harness
        .next_message(|message| id_matches(message, id))
        .await;
    message
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("no result in {message}"))
}

/// `client.notify(...)`。
async fn send_notification(harness: &Arc<Harness>, method: &str, params: Option<Value>) {
    let mut message = json!({ "jsonrpc": "2.0", "method": method });
    if let Some(params) = params {
        message["params"] = params;
    }
    harness
        .transport()
        .send(message, McpTransportSendOptions::none())
        .await
        .expect("send notification");
}

// ---------------------------------------------------------------------------
// consumeSseStream（2 个 it）
// ---------------------------------------------------------------------------

/// 按顺序产出若干块的字节流（TS 的 `new ReadableStream({start: enqueue..., close})`）。
struct ChunkStream {
    chunks: std::vec::IntoIter<Vec<u8>>,
}

impl futures_core::Stream for ChunkStream {
    type Item = Result<Vec<u8>, McpTransportError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.chunks.next().map(Ok))
    }
}

/// 把一个 `Vec<&str>` 变成字节流。
fn chunk_stream(chunks: Vec<&'static str>) -> FetchByteStream {
    Box::pin(ChunkStream {
        chunks: chunks
            .into_iter()
            .map(|chunk| chunk.as_bytes().to_vec())
            .collect::<Vec<_>>()
            .into_iter(),
    })
}

#[tokio::test]
async fn parses_chunked_crlf_events_comments_ids_and_multiline_data() {
    // `it("parses chunked CRLF events, comments, IDs, and multiline data")`
    let stream = chunk_stream(vec![
        ": keepalive\r\nid: 7\r\ndata: {\"one\":\r\n",
        "data: 1}\r\n\r\n",
    ]);
    let events = Arc::new(Mutex::new(Vec::<SseEvent>::new()));
    let sink = Arc::clone(&events);
    consume_sse_stream(
        stream,
        ConsumeSseOptions::new(move |event| {
            sink.lock().unwrap().push(event);
            Ok(())
        }),
    )
    .await
    .expect("parse");
    assert_eq!(
        *events.lock().unwrap(),
        vec![SseEvent {
            id: Some("7".to_owned()),
            data: "{\"one\":\n1}".to_owned(),
            event: None,
        }]
    );
}

/// 一个只会不断产出 `data: xxxxxxxxxxxxxxxx\n`、从不发空行的流（TS 的 `pull`）。
///
/// `sent` 与测试共享，对应 TS 测试里的闭包变量 `sent`（`expect(sent).toBeLessThan(100)` 断言
/// 超限是**逐次拉取**时被发现的，而不是把整条流读完之后才发现）。
struct EndlessDataLines {
    sent: Arc<AtomicUsize>,
}

impl futures_core::Stream for EndlessDataLines {
    type Item = Result<Vec<u8>, McpTransportError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        // `if (sent++ > 1000) controller.close(); else controller.enqueue(...)`
        let sent = self.sent.fetch_add(1, Ordering::SeqCst);
        if sent > 1_000 {
            return std::task::Poll::Ready(None);
        }
        std::task::Poll::Ready(Some(Ok(b"data: xxxxxxxxxxxxxxxx\n".to_vec())))
    }
}

#[tokio::test]
async fn rejects_events_whose_data_lines_exceed_the_limit_without_a_blank_line() {
    // `it("rejects events whose data lines exceed the limit without a blank line")`
    let sent = Arc::new(AtomicUsize::new(0));
    let stream: FetchByteStream = Box::pin(EndlessDataLines {
        sent: Arc::clone(&sent),
    });
    let error = consume_sse_stream(
        stream,
        ConsumeSseOptions {
            max_event_bytes: Some(256),
            ..ConsumeSseOptions::new(|_| Ok(()))
        },
    )
    .await
    .expect_err("must exceed");
    assert_eq!(error.to_string(), "MCP SSE event exceeds 256 bytes");
    // `expect(sent).toBeLessThan(100)`：256 字节的预算在 ~16 行内就用完，绝不会读完整条流。
    assert!(
        sent.load(Ordering::SeqCst) < 100,
        "the limit must be detected per pull, saw {} pulls",
        sent.load(Ordering::SeqCst)
    );
}

// ---------------------------------------------------------------------------
// StreamableHttpTransport（14 个 it）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handles_json_and_sse_responses_with_session_and_protocol_headers() {
    // `it("handles JSON and SSE responses with session and protocol headers")`
    let server = start_protocol_server().await;
    let harness =
        Harness::new(StreamableHttpTransport::new(options(&server.url())).expect("transport"));
    connect(&harness).await;
    assert_eq!(
        harness.transport().session_id().as_deref(),
        Some("session-1")
    );

    send_request(&harness, 2.0, "tools/list", None).await;
    let tools = harness
        .next_message(|message| id_matches(message, 2.0))
        .await;
    assert_eq!(
        tools["result"],
        json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] })
    );

    let result = call_tool(&harness, 3.0, "echo").await;
    assert_eq!(
        result,
        json!({ "content": [{ "type": "text", "text": "hello" }] })
    );
    harness.transport().close().await.expect("close");

    let requests = server.requests();
    let list_request = requests
        .iter()
        .find(|entry| {
            entry
                .message
                .as_ref()
                .and_then(|m| m.get("method"))
                .and_then(Value::as_str)
                == Some("tools/list")
        })
        .expect("tools/list request");
    assert_eq!(
        list_request.header("mcp-session-id").as_deref(),
        Some("session-1")
    );
    assert_eq!(
        list_request.header("mcp-protocol-version").as_deref(),
        Some(LATEST_PROTOCOL_VERSION)
    );
    // `expect(requests.some(entry => entry.method === "GET")).toBe(true)`
    server.wait_for_request(|entry| entry.method == "GET").await;
    // `expect(requests.some(entry => entry.method === "DELETE")).toBe(true)`
    server
        .wait_for_request(|entry| entry.method == "DELETE")
        .await;
}

#[tokio::test]
async fn classifies_authentication_failures() {
    // `it("classifies authentication failures")`
    let server = start_server(|request, response, _records| async move {
        let _ = request.body;
        response
            .send(
                401,
                &[(
                    "www-authenticate",
                    "Bearer resource_metadata=\"https://example.com/meta\"",
                )],
                "login required",
            )
            .await;
    })
    .await;
    let harness =
        Harness::new(StreamableHttpTransport::new(options(&server.url())).expect("transport"));
    harness.transport().start().await.expect("start");
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("401 must fail");
    assert_eq!(error.name(), "McpAuthRequiredError");
    assert_eq!(error.status(), Some(401));
    assert_eq!(error.body(), Some("login required"));
    assert_eq!(
        error.www_authenticate(),
        Some("Bearer resource_metadata=\"https://example.com/meta\"")
    );
    assert_eq!(error.to_string(), "MCP server requires authentication");
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn fails_only_the_request_whose_sse_stream_breaks() {
    // `it("fails only the request whose SSE stream breaks")`
    // `Semaphore` 而不是 `Notify`：信号可以在慢请求真正开始等待之前就发放（许可会
    // 保留），并行负载下不会出现「notify 先于 notified、慢请求永久挂起」的丢唤醒。
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let slow_gate = Arc::clone(&gate);
    let server = start_server(move |request, response, _records| {
        let slow_gate = Arc::clone(&slow_gate);
        async move {
            if request.method != "POST" {
                return protocol_handler(request, response, None).await;
            }
            let message = request.message.clone().expect("json body");
            let name = message["params"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if name == "broken" {
                response
                    .stream_head(200, &[("content-type", "text/event-stream")])
                    .await;
                response.stream_chunk("data: not json\n\n").await;
                response.stream_end().await;
                return;
            }
            if name == "slow" {
                let _ = slow_gate.acquire().await;
            }
            protocol_handler(request, response, Some(message)).await;
        }
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;

    let slow = tokio::spawn({
        let harness = Arc::clone(&harness);
        async move { call_tool(&harness, 2.0, "slow").await }
    });
    // 语义等价于 `await expect(client.callTool("broken")).rejects.toThrow("MCP response stream failed")`：
    // 传输层把合成的错误响应经 emitMessage 送出，客户端据此 reject 请求。
    send_request(
        &harness,
        3.0,
        "tools/call",
        Some(json!({ "name": "broken", "arguments": {} })),
    )
    .await;
    let broken = harness
        .next_message(|message| id_matches(message, 3.0))
        .await;
    let message = broken["error"]["message"].as_str().expect("error message");
    assert!(
        message.starts_with("MCP response stream failed"),
        "{message}"
    );
    assert_eq!(broken["error"]["code"], json!(-32603));

    gate.add_permits(1);
    assert_eq!(
        slow.await.expect("join"),
        json!({ "content": [{ "type": "text", "text": "hello" }] })
    );
    // `expect(errors).toHaveLength(1)`：只有 `data: not json` 那条解析错误。
    let errors = harness.drain_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].name(), "SyntaxError");
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn opens_the_get_stream_after_initialization_and_sends_last_event_id_only_when_resuming() {
    // `it("opens the GET stream after initialization and sends Last-Event-ID only when resuming")`
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_order = Arc::clone(&order);
    let server = start_server(move |request, response, _records| {
        let order = Arc::clone(&recorded_order);
        async move {
            if request.method == "POST" {
                let message = request.message.clone().expect("json body");
                order
                    .lock()
                    .unwrap()
                    .push(message["method"].as_str().unwrap_or_default().to_owned());
                return protocol_handler(request, response, Some(message)).await;
            }
            order.lock().unwrap().push(request.method.clone());
            protocol_handler(request, response, None).await;
        }
    })
    .await;
    let harness =
        Harness::new(StreamableHttpTransport::new(options(&server.url())).expect("transport"));
    connect(&harness).await;
    call_tool(&harness, 2.0, "echo").await;
    send_request(&harness, 3.0, "tools/list", None).await;
    let _ = harness
        .next_message(|message| id_matches(message, 3.0))
        .await;
    harness.transport().close().await.expect("close");

    let recorded = order.lock().unwrap().clone();
    let get_index = recorded
        .iter()
        .position(|method| method == "GET")
        .expect("GET was requested");
    let initialized_index = recorded
        .iter()
        .position(|method| method == "notifications/initialized")
        .expect("initialized was sent");
    assert!(
        get_index > initialized_index,
        "GET must come after notifications/initialized: {recorded:?}"
    );
    // `expect(requests.every(entry => entry.headers["last-event-id"] === undefined)).toBe(true)`
    assert!(
        server
            .requests()
            .iter()
            .all(|entry| entry.header("last-event-id").is_none())
    );
}

#[tokio::test]
async fn resumes_a_response_stream_the_server_closed_before_answering() {
    // `it("resumes a response stream the server closed before answering")`
    let resume_headers: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&resume_headers);
    let server = start_server(move |request, response, _records| {
        let resume_headers = Arc::clone(&recorded);
        async move {
            if request.method == "GET" {
                if let Some(last_event_id) = request.header("last-event-id") {
                    resume_headers.lock().unwrap().push(last_event_id);
                    response
                        .stream_head(200, &[("content-type", "text/event-stream")])
                        .await;
                    response
                        .stream_chunk(&format!(
                            "id: 2\ndata: {}\n\n",
                            json!({
                                "jsonrpc": "2.0",
                                "id": 2,
                                "result": { "content": [{ "type": "text", "text": "resumed" }] },
                            })
                        ))
                        .await;
                    response.stream_end().await;
                    return;
                }
                response.send(405, &[], "").await;
                return;
            }
            if request.method != "POST" {
                return protocol_handler(request, response, None).await;
            }
            let message = request.message.clone().expect("json body");
            if message["method"].as_str() != Some("tools/call") {
                return protocol_handler(request, response, Some(message)).await;
            }
            // 预备事件（只有 id、没有 data）与 retry 提示，然后服务器断开流。
            response
                .stream_head(200, &[("content-type", "text/event-stream")])
                .await;
            response.stream_chunk("id: 1\nretry: 5\ndata:\n\n").await;
            response.stream_end().await;
        }
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    assert_eq!(
        call_tool(&harness, 2.0, "echo").await,
        json!({ "content": [{ "type": "text", "text": "resumed" }] })
    );
    assert_eq!(*resume_headers.lock().unwrap(), vec!["1".to_owned()]);
    assert!(harness.drain_errors().is_empty());
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn fails_a_request_whose_response_stream_ends_without_an_answer() {
    // `it("fails a request whose response stream ends without an answer")`
    let server = start_server(|request, response, _records| async move {
        if request.method != "POST" {
            return protocol_handler(request, response, None).await;
        }
        let message = request.message.clone().expect("json body");
        if message["method"].as_str() != Some("tools/call") {
            return protocol_handler(request, response, Some(message)).await;
        }
        response
            .stream_head(200, &[("content-type", "text/event-stream")])
            .await;
        response.stream_chunk(": nothing here\n\n").await;
        response.stream_end().await;
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    send_request(
        &harness,
        2.0,
        "tools/call",
        Some(json!({ "name": "echo", "arguments": {} })),
    )
    .await;
    // TS 是 `client.callTool(...)` 以 `MCP response stream failed: stream ended without a response`
    // reject；传输层可观察的等价物是合成的错误响应（client 的 reject 来自它）。
    let message = harness
        .next_message(|message| id_matches(message, 2.0))
        .await;
    assert_eq!(
        message["error"]["message"].as_str(),
        Some("MCP response stream failed: stream ended without a response")
    );
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn reconnects_the_get_stream_after_it_drops() {
    // `it("reconnects the GET stream after it drops")`
    let gets = Arc::new(AtomicUsize::new(0));
    let last_event_ids: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let counter = Arc::clone(&gets);
    let recorded_ids = Arc::clone(&last_event_ids);
    let server = start_server(move |request, response, _records| {
        let counter = Arc::clone(&counter);
        let recorded_ids = Arc::clone(&recorded_ids);
        async move {
            if request.method != "GET" {
                return protocol_handler(request, response, None).await;
            }
            let count = counter.fetch_add(1, Ordering::SeqCst) + 1;
            recorded_ids
                .lock()
                .unwrap()
                .push(request.header("last-event-id"));
            response
                .stream_head(200, &[("content-type", "text/event-stream")])
                .await;
            let notification =
                json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" });
            if count == 1 {
                response
                    .stream_chunk(&format!("id: g1\ndata: {notification}\n\n"))
                    .await;
                response.stream_end().await;
                return;
            }
            response
                .stream_chunk(&format!("id: g2\ndata: {notification}\n\n"))
                .await;
            // 不结束：第二次 GET 保持打开（TS 的 `response.write`）。
            futures_std_pending().await;
        }
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            reconnect: Some(StreamableHttpReconnectOptions {
                initial_delay_ms: Some(1),
                ..StreamableHttpReconnectOptions::default()
            }),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    // 两条通知（第二条来自重连后的流）。
    for _ in 0..2 {
        let message = harness
            .next_message(|message| {
                message.get("method").and_then(Value::as_str)
                    == Some("notifications/tools/list_changed")
            })
            .await;
        assert_eq!(message["jsonrpc"], json!("2.0"));
    }
    assert_eq!(
        *last_event_ids.lock().unwrap(),
        vec![None, Some("g1".to_owned())]
    );
    harness.transport().close().await.expect("close");
}

/// 永远 pending 的 future（模拟保持打开、不再写数据的响应）。
async fn futures_std_pending() {
    std::future::pending::<()>().await
}

#[tokio::test]
async fn rejects_a_request_the_server_accepts_without_a_response() {
    // `it("rejects a request the server accepts without a response")`
    let server = start_server(|request, response, _records| async move {
        if request.method != "POST" {
            return protocol_handler(request, response, None).await;
        }
        let message = request.message.clone().expect("json body");
        if message["method"].as_str() != Some("tools/call") {
            return protocol_handler(request, response, Some(message)).await;
        }
        response.send(202, &[], "").await;
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo" } }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("202 must fail the request");
    assert!(
        error.to_string().contains("without a response"),
        "{}",
        error
    );
    assert_eq!(
        error.to_string(),
        "MCP server accepted request tools/call without a response"
    );
    assert_eq!(error.status(), Some(202));
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn includes_the_response_body_in_http_errors() {
    // `it("includes the response body in HTTP errors")`
    let server = start_server(|request, response, _records| async move {
        let _ = request.body;
        response.send(400, &[], "Invalid Accept header").await;
    })
    .await;
    let harness =
        Harness::new(StreamableHttpTransport::new(options(&server.url())).expect("transport"));
    harness.transport().start().await.expect("start");
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("400 must fail");
    assert_eq!(
        error.to_string(),
        "MCP HTTP request failed with status 400: Invalid Accept header"
    );
    assert_eq!(error.body(), Some("Invalid Accept header"));
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn closing_an_open_get_stream_emits_no_dropped_error() {
    // 回归：评审 finding #1（major）。TS 的 `for (let attempt = 0; !this.closed; )` 在每一轮体
    // **之前**重新检查 `closed`；Rust 只在读循环里检查，于是 `maxRetries: 0` 时 `close()`
    // （取消令牌 → `consumeSse` 正常返回）会继续落到 `attempt >= maxRetries()` 分支，冒出一条
    // 假的 `could not be reopened` 错误。TS 参考实现实测：`TS errors after close: []`。
    let server = start_server(|request, response, _records| async move {
        if request.method == "GET" {
            response
                .stream_head(200, &[("content-type", "text/event-stream")])
                .await;
            response
                .stream_chunk("id: g1\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n")
                .await;
            // 保持打开（TS 的 `res.write(...)` 之后不 end）。
            futures_std_pending().await;
            return;
        }
        protocol_handler(request, response, None).await;
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            reconnect: Some(StreamableHttpReconnectOptions {
                max_retries: Some(0),
                initial_delay_ms: Some(1),
                ..StreamableHttpReconnectOptions::default()
            }),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    // 等 GET 流真的打开并投递了那条通知（TS 的 `setTimeout(300)`）。
    server.wait_for_request(|entry| entry.method == "GET").await;
    let notification = harness
        .next_message(|message| {
            message.get("method").and_then(Value::as_str)
                == Some("notifications/tools/list_changed")
        })
        .await;
    assert_eq!(notification["jsonrpc"], json!("2.0"));
    harness.transport().close().await.expect("close");
    // `close()` 之后给后台任务一点时间把流收尾（TS 的 `setTimeout(200)`）。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        harness.drain_errors().len(),
        0,
        "a closed transport must not report a dropped GET stream"
    );
    // 流没有被重试：`maxRetries: 0` 且已关闭。
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|entry| entry.method == "GET")
            .count(),
        1
    );
}

#[tokio::test]
async fn does_not_retry_a_401_when_the_provider_has_no_on_unauthorized_handler() {
    // 回归：评审 finding #2（major）。TS 的 `onUnauthorized` 是可选的：
    // `const onUnauthorized = this.options.authProvider?.onUnauthorized?.bind(...)` 对「只有 token()」
    // 的 provider 是 `undefined`，于是 `if (attempt > 0 || !onUnauthorized || ...) return response`
    // 在**第一次**就返回 401。Rust 的 `on_unauthorized` 缺省是 no-op，旧实现因此多发一次请求
    // （服务器能看到重复的 POST）。TS 参考实测：`POST requests: 1`。
    let posts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&posts);
    let server = start_server(move |_request, response, _records| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            response
                .send(401, &[("www-authenticate", "Bearer")], "nope")
                .await;
        }
    })
    .await;
    /// 只有 `token()`：不覆盖 `has_on_unauthorized`（缺省 `false`），也没有 `on_unauthorized`。
    struct TokenOnly;
    #[async_trait::async_trait]
    impl AuthProvider for TokenOnly {
        async fn token(&self) -> Option<String> {
            Some("tok".to_owned())
        }
    }
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            auth_provider: Some(Arc::new(TokenOnly) as Arc<dyn AuthProvider>),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    harness.transport().start().await.expect("start");
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("401 must fail");
    assert_eq!(error.name(), "McpAuthRequiredError");
    assert_eq!(error.status(), Some(401));
    assert_eq!(error.to_string(), "MCP server requires authentication");
    assert_eq!(error.body(), Some("nope"));
    assert_eq!(
        posts.load(Ordering::SeqCst),
        1,
        "a provider without onUnauthorized must not trigger a retry"
    );
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn a_get_stream_without_a_body_is_a_non_retryable_content_type_error() {
    // 回归：评审 finding #3（major）。TS：`if (type !== "text/event-stream" || !response.body)`，
    // 而 fetch 对 204/205/304 的 `Response.body` 总是 `null`。于是 204 + `text/event-stream` 的 GET
    // 抛 `McpHttpError: Unsupported MCP GET response content type: text/event-stream`（**不可**重试，
    // `runGetStream` 报一次就停），而不是把空流当作正常流消费后反复重连。
    // TS 参考实测：`GETs: 1`、`errors: ["McpHttpError: Unsupported MCP GET response content type: text/event-stream"]`。
    let gets = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&gets);
    let server = start_server(move |request, response, _records| {
        let counter = Arc::clone(&counter);
        async move {
            if request.method == "GET" {
                counter.fetch_add(1, Ordering::SeqCst);
                response
                    .send(204, &[("content-type", "text/event-stream")], "")
                    .await;
                return;
            }
            protocol_handler(request, response, None).await;
        }
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            reconnect: Some(StreamableHttpReconnectOptions {
                initial_delay_ms: Some(1),
                max_retries: Some(2),
                ..StreamableHttpReconnectOptions::default()
            }),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let error = harness.next_error().await;
    assert_eq!(error.name(), "McpHttpError");
    assert_eq!(error.status(), Some(204));
    assert_eq!(
        error.to_string(),
        "Unsupported MCP GET response content type: text/event-stream"
    );
    // 不可重试：不再有第二次 GET，也不会有 `could not be reopened`。
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(gets.load(Ordering::SeqCst), 1, "must not retry");
    assert_eq!(harness.drain_errors().len(), 0);
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn a_response_stream_without_a_body_is_an_unsupported_content_type_error() {
    // 同上但走 `send` 的分支：TS 的条件是 `type === "text/event-stream" && response.body`，
    // 205 + SSE 时 `body` 是 `null` → 落到 `Unsupported MCP response content type: text/event-stream`
    // （**不是** "missing"）。TS 参考实测：`McpHttpError | "Unsupported MCP response content type:
    // text/event-stream" status 205`。
    let server = start_server(|request, response, _records| async move {
        if request.method != "POST" {
            return protocol_handler(request, response, None).await;
        }
        let message = request.message.clone().expect("json body");
        if message["method"].as_str() == Some("tools/call") {
            response
                .send(205, &[("content-type", "text/event-stream")], "")
                .await;
            return;
        }
        protocol_handler(request, response, Some(message)).await;
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo" } }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("205 + SSE must fail");
    assert_eq!(error.name(), "McpHttpError");
    assert_eq!(error.status(), Some(205));
    assert_eq!(
        error.to_string(),
        "Unsupported MCP response content type: text/event-stream"
    );
    harness.transport().close().await.expect("close");
}

/// `onUnauthorized` 看到的 `(status, token)` 序列（TS 的 `seen: { status; token? }[]`）。
type SeenChallenges = Arc<Mutex<Vec<(u16, Option<String>)>>>;

/// 一次 POST 上观察到的 `(accept, content-type, x-custom, authorization, mcp-session-id)`。
type SeenHeaders = Arc<Mutex<Option<(String, String, String, String, String)>>>;

/// 测试用的认证提供方：token 来自闭包，`on_unauthorized` 记录并改状态。
struct TestProvider<F, G> {
    token: F,
    on_unauthorized: G,
}

#[async_trait::async_trait]
impl<F, G, Fut> AuthProvider for TestProvider<F, G>
where
    F: Fn() -> Option<String> + Send + Sync,
    G: Fn(UnauthorizedContext) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    async fn token(&self) -> Option<String> {
        (self.token)()
    }

    /// TS 的 `onUnauthorized` 存在：传输收到 401/403 时交给它并重试一次（§17.6）。
    fn has_on_unauthorized(&self) -> bool {
        true
    }

    async fn on_unauthorized(&self, context: UnauthorizedContext) -> Result<(), McpTransportError> {
        (self.on_unauthorized)(context).await;
        Ok(())
    }
}

#[tokio::test]
async fn hands_401_and_insufficient_scope_403_responses_to_the_auth_provider_with_the_rejected_token()
 {
    // `it("hands 401 and insufficient-scope 403 responses to the auth provider with the rejected token")`
    let seen: SeenChallenges = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let token = Arc::new(Mutex::new(Some("old".to_owned())));
    let server = start_server(|request, response, _records| async move {
        if request.method != "POST" {
            return protocol_handler(request, response, None).await;
        }
        let message = request.message.clone().expect("json body");
        if request.header("authorization").as_deref() == Some("Bearer old") {
            response
                .send(401, &[("www-authenticate", "Bearer")], "")
                .await;
            return;
        }
        if message["method"].as_str() == Some("tools/call")
            && request.header("authorization").as_deref() == Some("Bearer new")
        {
            response
                .send(
                    403,
                    &[(
                        "www-authenticate",
                        "Bearer error=\"insufficient_scope\", scope=\"admin\"",
                    )],
                    "",
                )
                .await;
            return;
        }
        protocol_handler(request, response, Some(message)).await;
    })
    .await;
    let token_state = Arc::clone(&token);
    let token_for_read = Arc::clone(&token);
    let seen_state = Arc::clone(&recorded);
    let provider: Arc<dyn AuthProvider> = Arc::new(TestProvider {
        token: move || token_for_read.lock().unwrap().clone(),
        on_unauthorized: move |context: UnauthorizedContext| {
            let seen = Arc::clone(&seen_state);
            let token = Arc::clone(&token_state);
            async move {
                seen.lock()
                    .unwrap()
                    .push((context.status, context.token.clone()));
                let next = if context.status == 401 {
                    "new"
                } else {
                    "admin"
                };
                *token.lock().unwrap() = Some(next.to_owned());
            }
        },
    });
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            auth_provider: Some(provider),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    harness.transport().start().await.expect("start");
    // initialize（用 old → 401 → onUnauthorized → new）。
    send_request(&harness, 1.0, "initialize", Some(json!({ "protocolVersion": LATEST_PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "http-test", "version": "1.0.0" } }))).await;
    let init = harness
        .next_message(|message| id_matches(message, 1.0))
        .await;
    harness
        .transport()
        .set_protocol_version(init["result"]["protocolVersion"].as_str().unwrap());
    send_notification(&harness, "notifications/initialized", None).await;
    assert_eq!(
        call_tool(&harness, 2.0, "echo").await,
        json!({ "content": [{ "type": "text", "text": "hello" }] })
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(401, Some("old".to_owned())), (403, Some("new".to_owned())),]
    );
    harness.transport().close().await.expect("close");
}

/// 401 响应的体在 drain 时读取失败。
///
/// TS 把活的 `Response` 交给 `onUnauthorized`：provider 不读体时没有可观察差异（`discard`
/// 只 `cancel()` 体，不读），读体才会看到流错误。Rust 的 [`UnauthorizedContext::body`] 是
/// §12 冻结的 `Vec<u8>`（已 drain，没有错误通道），因此读取失败退化成空体——模块文档的
/// 「被迫偏差」一节登记了这条。这里钉住：provider 仍被调用、body 为空、请求仍重试一次。
#[tokio::test]
async fn a_failed_401_body_read_becomes_an_empty_unauthorized_context_body() {
    /// 第一次读取就报错的体流（模拟传输中连接中断）。
    struct FailingBodyStream;

    impl futures_core::Stream for FailingBodyStream {
        type Item = Result<Vec<u8>, McpTransportError>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(Some(Err(McpTransportError::network("body stream failed"))))
        }
    }

    /// 第一次返回 401 + 读取失败的体；第二次返回合法的 initialize 结果。
    struct FailingBodyFetch {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl McpFetch for FailingBodyFetch {
        async fn fetch(
            &self,
            _url: &Url,
            _init: kiana_mcp::auth_provider::FetchInit,
        ) -> Result<FetchResponse, McpTransportError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(FetchResponse::new(
                    401,
                    http::HeaderMap::new(),
                    Box::pin(FailingBodyStream),
                ));
            }
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1.0,
                "result": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "serverInfo": { "name": "http-fixture", "version": "1.0.0" },
                },
            })
            .to_string();
            let mut headers = http::HeaderMap::new();
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
            Ok(FetchResponse::from_bytes(200, headers, body.into_bytes()))
        }
    }

    let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&bodies);
    let provider: Arc<dyn AuthProvider> = Arc::new(TestProvider {
        token: || Some("old".to_owned()),
        on_unauthorized: move |context: UnauthorizedContext| {
            let recorded = Arc::clone(&recorded);
            async move {
                recorded.lock().unwrap().push(context.body.clone());
            }
        },
    });
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            fetch: Some(Arc::new(FailingBodyFetch {
                calls: AtomicUsize::new(0),
            })),
            auth_provider: Some(provider),
            ..options("http://127.0.0.1:1/mcp")
        })
        .expect("transport"),
    );
    harness.transport().start().await.expect("start");
    send_request(
        &harness,
        1.0,
        "initialize",
        Some(json!({ "protocolVersion": LATEST_PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "http-test", "version": "1.0.0" } })),
    )
    .await;
    let init = harness
        .next_message(|message| id_matches(message, 1.0))
        .await;
    assert_eq!(init["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
    assert_eq!(*bodies.lock().unwrap(), vec![Vec::<u8>::new()]);
}

#[tokio::test]
async fn does_not_retry_requests_that_were_cancelled_while_authorization_was_recovering() {
    // `it("does not retry requests that were cancelled while authorization was recovering")`
    // #10598：在共享刷新期间被取消/超时的请求不得重试。
    let token = Arc::new(Mutex::new(Some("old".to_owned())));
    let server = start_server(|request, response, _records| async move {
        if request.method != "POST" {
            return protocol_handler(request, response, None).await;
        }
        let message = request.message.clone().expect("json body");
        if message["method"].as_str() == Some("tools/call")
            && request.header("authorization").as_deref() == Some("Bearer old")
        {
            response
                .send(401, &[("www-authenticate", "Bearer")], "")
                .await;
            return;
        }
        protocol_handler(request, response, Some(message)).await;
    })
    .await;
    // 共享刷新门闸：初始 0 个许可，测试放行时 `add_permits(3)`（`Notify` 的 `notify_waiters`
    // 会漏掉尚未注册的等待者，这里用信号量避免该竞态）。
    let refresh_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let waiting = Arc::new(AtomicUsize::new(0));
    let all_waiting = Arc::new(tokio::sync::Notify::new());
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refresh_started = Arc::new(AtomicBool::new(false));
    let token_state = Arc::clone(&token);
    let token_for_read = Arc::clone(&token);
    let provider: Arc<dyn AuthProvider> = Arc::new(TestProvider {
        token: move || token_for_read.lock().unwrap().clone(),
        on_unauthorized: {
            let gate = Arc::clone(&refresh_gate);
            let waiting = Arc::clone(&waiting);
            let all_waiting = Arc::clone(&all_waiting);
            let refreshes = Arc::clone(&refreshes);
            let refresh_started = Arc::clone(&refresh_started);
            let token_state = Arc::clone(&token_state);
            move |_context: UnauthorizedContext| {
                let gate = Arc::clone(&gate);
                let waiting = Arc::clone(&waiting);
                let all_waiting = Arc::clone(&all_waiting);
                let refreshes = Arc::clone(&refreshes);
                let refresh_started = Arc::clone(&refresh_started);
                let token_state = Arc::clone(&token_state);
                async move {
                    // TS 的 `if (!refresh) { refreshes++; ... }`：只有第一个调用者发起刷新，
                    // 其余等待同一个 promise。
                    if !refresh_started.swap(true, Ordering::SeqCst) {
                        refreshes.fetch_add(1, Ordering::SeqCst);
                    }
                    if waiting.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
                        all_waiting.notify_waiters();
                    }
                    let _permit = gate.acquire().await.expect("gate");
                    *token_state.lock().unwrap() = Some("new".to_owned());
                }
            }
        },
    });
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            auth_provider: Some(provider),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    harness.transport().start().await.expect("start");
    send_request(&harness, 1.0, "initialize", Some(json!({ "protocolVersion": LATEST_PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "http-test", "version": "1.0.0" } }))).await;
    let init = harness
        .next_message(|message| id_matches(message, 1.0))
        .await;
    harness
        .transport()
        .set_protocol_version(init["result"]["protocolVersion"].as_str().unwrap());
    send_notification(&harness, "notifications/initialized", None).await;

    // 三个请求同时等共享刷新：前两个被取消（abort / timeout），第三个继续。
    let aborted_signal = CancellationToken::new();
    let timed_out_signal = CancellationToken::new();
    let aborted = tokio::spawn({
        let harness = Arc::clone(&harness);
        let signal = aborted_signal.clone();
        async move {
            harness
                .transport()
                .send(
                    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "aborted" } }),
                    McpTransportSendOptions::with_signal(Some(signal)),
                )
                .await
        }
    });
    let timed_out = tokio::spawn({
        let harness = Arc::clone(&harness);
        let signal = timed_out_signal.clone();
        async move {
            harness
                .transport()
                .send(
                    json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "timed-out" } }),
                    McpTransportSendOptions::with_signal(Some(signal)),
                )
                .await
        }
    });
    let kept = tokio::spawn({
        let harness = Arc::clone(&harness);
        async move {
            harness
                .transport()
                .send(
                    json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "kept" } }),
                    McpTransportSendOptions::none(),
                )
                .await
        }
    });
    // 等三个请求都卡在共享刷新上。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while waiting.load(Ordering::SeqCst) < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "requests never reached the shared refresh"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    aborted_signal.cancel();
    timed_out_signal.cancel();
    // TS 的 client 立刻用它自己的 abort/timeout 结算这两个请求（不等共享刷新）；传输层只在
    // 刷新结束后、派发之前检查 signal。因此这里先放行刷新，再断言两个请求**都没有派发**。
    refresh_gate.add_permits(3);
    let aborted_error = aborted.await.expect("join").expect_err("aborted must fail");
    assert_eq!(aborted_error.name(), "AbortError");
    let timed_out_error = timed_out
        .await
        .expect("join")
        .expect_err("timed out must fail");
    assert_eq!(timed_out_error.name(), "AbortError");
    kept.await.expect("join").expect("kept must succeed");
    // `expect(executed).toEqual(["kept"])`：服务器只**执行**了 kept 的 tools/call。
    // TS 的 `executed` 记录发生在 401 分支之后，因此这里同样排除携带旧 token 的首轮请求。
    let calls: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|entry| {
            entry.header("authorization").as_deref() == Some("Bearer new")
                && entry
                    .message
                    .as_ref()
                    .and_then(|m| m.get("method"))
                    .and_then(Value::as_str)
                    == Some("tools/call")
        })
        .filter_map(|entry| {
            entry
                .message
                .as_ref()
                .and_then(|m| m["params"]["name"].as_str().map(str::to_owned))
        })
        .collect();
    assert_eq!(calls, vec!["kept".to_owned()]);
    // `expect(refreshes).toBe(1)`
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn closes_the_session_with_the_last_request_s_token_without_asking_the_auth_provider() {
    // `it("closes the session with the last request's token, without asking the auth provider")`
    // #10565：认证提供方可能走网络刷新，关闭不得等它。
    let server = start_protocol_server().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let provider: Arc<dyn AuthProvider> = Arc::new(TestProvider {
        token: move || {
            let next = counter.fetch_add(1, Ordering::SeqCst) + 1;
            Some(format!("token-{next}"))
        },
        on_unauthorized: |_context: UnauthorizedContext| async {},
    });
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            auth_provider: Some(provider),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let before = calls.load(Ordering::SeqCst);
    harness.transport().close().await.expect("close");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "close must not call token()"
    );

    // DELETE 是 `close()` 里的一次真实网络交换：等服务器记录到它（TS 的断言发生在
    // `await client.close()` 之后，那时 DELETE 已经完成）。
    server
        .wait_for_request(|entry| entry.method == "DELETE")
        .await;
    let deletes: Vec<RecordedRequest> = server
        .requests()
        .into_iter()
        .filter(|entry| entry.method == "DELETE")
        .collect();
    assert_eq!(
        deletes
            .iter()
            .map(|entry| entry.header("authorization"))
            .collect::<Vec<_>>(),
        vec![Some(format!("Bearer token-{before}"))]
    );
    assert_eq!(
        deletes[0].header("mcp-session-id").as_deref(),
        Some("session-1")
    );
}

/// 严格 fetch：TS 里它检查 `this` 不是 `undefined`/`globalThis` 时抛 `TypeError("Illegal invocation")`。
///
/// Rust 的 `Arc<dyn McpFetch>` 没有接收者，因此 `Illegal invocation` 不可达；这个包装只用来断言
/// **注入的 fetch 确实被用于所有请求**（包括 `onUnauthorized` 的 `context.fetch` 与 close 的
/// DELETE），见测试内的断言。
struct StrictFetch {
    inner: ReqwestFetch,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl McpFetch for StrictFetch {
    async fn fetch(
        &self,
        url: &Url,
        init: kiana_mcp::auth_provider::FetchInit,
    ) -> Result<FetchResponse, McpTransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch(url, init).await
    }
}

#[tokio::test]
async fn calls_fetch_without_a_receiver() {
    // `it("calls fetch without a receiver")`
    // #10188：Cloudflare Workers 拒绝带接收者的平台 fetch。Rust 侧用 `Arc<dyn McpFetch>` 建模，
    // 方法调用没有 `this`，因此这里断言注入的 fetch 覆盖了传输的所有出站请求（含 401 恢复路径里
    // 认证提供方用 `context.fetch` 重新发起的那个）。
    let calls = Arc::new(AtomicUsize::new(0));
    let server = start_server(|request, response, _records| async move {
        if request.method == "POST" && request.header("authorization").is_none() {
            response
                .send(401, &[("www-authenticate", "Bearer")], "")
                .await;
            return;
        }
        protocol_handler(request, response, None).await;
    })
    .await;
    let token = Arc::new(Mutex::new(None::<String>));
    let url = Url::parse(&server.url()).expect("url");
    let token_state = Arc::clone(&token);
    let token_for_read = Arc::clone(&token);
    let fetch_calls = Arc::clone(&calls);
    let provider: Arc<dyn AuthProvider> = Arc::new(TestProvider {
        token: move || token_for_read.lock().unwrap().clone(),
        on_unauthorized: move |context: UnauthorizedContext| {
            let url = url.clone();
            let token = Arc::clone(&token_state);
            async move {
                let _ = context
                    .fetch
                    .fetch(&url, kiana_mcp::auth_provider::FetchInit::get())
                    .await;
                *token.lock().unwrap() = Some("token".to_owned());
            }
        },
    });
    let strict: Arc<dyn McpFetch> = Arc::new(StrictFetch {
        inner: ReqwestFetch::new(),
        calls: Arc::clone(&fetch_calls),
    });
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            fetch: Some(strict),
            auth_provider: Some(provider),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    harness.transport().start().await.expect("start");
    send_request(
        &harness,
        1.0,
        "initialize",
        Some(json!({ "protocolVersion": LATEST_PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "http-test", "version": "1.0.0" } })),
    )
    .await;
    let _ = harness
        .next_message(|message| id_matches(message, 1.0))
        .await;
    send_request(&harness, 2.0, "tools/list", None).await;
    let tools = harness
        .next_message(|message| id_matches(message, 2.0))
        .await;
    assert_eq!(
        tools["result"],
        json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] })
    );
    harness.transport().close().await.expect("close");
    // 精确计数：initialize 的 401、initialize 的 200、provider 的 context.fetch、tools/list、
    // close 的 DELETE —— 5 次 fetch、服务器也恰好看到 5 个请求。两个计数相等说明**所有**出站
    // 交换都走了注入的句柄（没有任何请求绕过它，例如落到缺省的全局 fetch 上）。
    let injected = fetch_calls.load(Ordering::SeqCst);
    assert_eq!(injected, 5, "the injected fetch must serve every request");
    assert_eq!(
        server.request_count(),
        injected,
        "a request bypassed the injected fetch: {:?}",
        server
            .requests()
            .iter()
            .map(|entry| entry.method.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn classifies_an_expired_established_session() {
    // `it("classifies an expired established session")`
    let posts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&posts);
    let server = start_server(move |request, response, _records| {
        let counter = Arc::clone(&counter);
        async move {
            if request.method == "POST" && counter.fetch_add(1, Ordering::SeqCst) >= 2 {
                response.send(404, &[], "gone").await;
                return;
            }
            protocol_handler(request, response, None).await;
        }
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("404 must fail");
    assert!(error.is_session_expired(), "{}", error.name());
    assert_eq!(error.name(), "McpSessionExpiredError");
    assert_eq!(error.status(), Some(404));
    assert_eq!(error.to_string(), "MCP session expired");
    assert_eq!(error.body(), Some("gone"));
    harness.transport().close().await.expect("close");
}

// ---------------------------------------------------------------------------
// 黄金向量：由 TS 参考实现生成（`tests/fixtures/sse_vectors.json`）
// ---------------------------------------------------------------------------

/// `crates/kiana-mcp/tests/fixtures/sse_vectors.json` 由
/// `/home/ctyun/kiana-work/scratch/kiana-mcp__transports-http/sse_vectors.mjs` 在 node 26 上跑
/// **未修改的** TS 实现生成（§17.1 的逐字测试与 §30.28 的实测向量）。
///
/// 覆盖：CRLF/注释/多行 data、只有 id 的事件、空 id、含 NUL 的 id、`retry` 的
/// `+5`/`5.0`/` 5`/`-5`/`007`/`Infinity`/空串、无冒号字段、自定义/空 event、EOF 冲刷、
/// 纯注释、dispatch 后 id 清空、冒号后的空格/tab、裸 CR、未知字段、跨 chunk 拆分的多字节字符、
/// 流首 BOM、非法 UTF-8。
#[tokio::test]
async fn matches_the_typescript_reference_on_sse_vectors() {
    let raw = include_str!("fixtures/sse_vectors.json");
    let cases: Vec<Value> = serde_json::from_str(raw).expect("fixture");
    assert!(cases.len() >= 30);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let chunks: Vec<Vec<u8>> = case["chunks"]
            .as_array()
            .expect("chunks")
            .iter()
            .map(|chunk| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(chunk.as_str().expect("chunk"))
                    .expect("base64")
            })
            .collect();
        let stream: FetchByteStream = Box::pin(ChunkStream {
            chunks: chunks.into_iter(),
        });
        let events = Arc::new(Mutex::new(Vec::<Value>::new()));
        let ids = Arc::new(Mutex::new(Vec::<String>::new()));
        let retries = Arc::new(Mutex::new(Vec::<Value>::new()));
        let event_sink = Arc::clone(&events);
        let id_sink = Arc::clone(&ids);
        let retry_sink = Arc::clone(&retries);
        let result = consume_sse_stream(
            stream,
            ConsumeSseOptions {
                max_event_bytes: case["maxEventBytes"].as_u64().map(|max| max as usize),
                on_event: Some(Arc::new(move |event: SseEvent| {
                    // 键序：event、data、id（TS 的对象字面量序），缺省的键不写。
                    let mut value = serde_json::Map::new();
                    if let Some(event_name) = &event.event {
                        value.insert("event".to_owned(), json!(event_name));
                    }
                    value.insert("data".to_owned(), json!(event.data));
                    if let Some(id) = &event.id {
                        value.insert("id".to_owned(), json!(id));
                    }
                    event_sink.lock().unwrap().push(Value::Object(value));
                    Ok(())
                })),
                on_id: Some(Arc::new(move |id: &str| {
                    id_sink.lock().unwrap().push(id.to_owned());
                })),
                on_retry: Some(Arc::new(move |delay: f64| {
                    // 按 JS 的数字文本记录（`7` 而不是 `7.0`，`Infinity` 而不是 `null`），
                    // 这样与 fixture 里的 JSON 数字逐字可比。
                    // `format_js_number` 把非有限值写成 JSON 的 `null`；JS 的 `String(Infinity)`
                    // 是 `"Infinity"`，fixture 里按后者记录。
                    let text = if delay.is_nan() {
                        "NaN".to_owned()
                    } else if delay.is_finite() {
                        kiana_json::js_num::format_js_number(delay)
                    } else if delay > 0.0 {
                        "Infinity".to_owned()
                    } else {
                        "-Infinity".to_owned()
                    };
                    retry_sink.lock().unwrap().push(json!(text));
                })),
                cancel: None,
            },
        )
        .await;
        match case["error"].as_str() {
            Some(expected) => {
                let error = result
                    .err()
                    .unwrap_or_else(|| panic!("{name}: expected error {expected}, got success"));
                assert_eq!(error.to_string(), expected, "{name}");
            }
            None => {
                result.unwrap_or_else(|error| panic!("{name}: unexpected error {error}"));
            }
        }
        let strip = |value: &Value| {
            let mut value = value.clone();
            if let Some(object) = value.as_object_mut() {
                object.retain(|key, _| {
                    key != "chunks" && key != "name" && key != "maxEventBytes" && key != "error"
                });
                // retry 的延迟按 JS 数字文本比较（fixture 里是 JSON 数字，`7` 与 `7.0`
                // 在 JS 里是同一个 Number）。
                if let Some(retries) = object.get_mut("retries").and_then(Value::as_array_mut) {
                    for retry in retries.iter_mut() {
                        if let Some(delay) = retry.as_f64() {
                            *retry = json!(kiana_json::js_num::format_js_number(delay));
                        }
                    }
                }
            }
            value
        };
        let actual = json!({
            "events": *events.lock().unwrap(),
            "ids": *ids.lock().unwrap(),
            "retries": *retries.lock().unwrap(),
        });
        assert_eq!(actual, strip(&case), "{name}");
    }
}

// ---------------------------------------------------------------------------
// 头装配（§17.4 / §30.19）与结束条件
// ---------------------------------------------------------------------------

#[tokio::test]
async fn header_order_and_replacement_match_the_whatwg_headers_layer() {
    // 覆盖顺序：options.headers → 每次请求的额外头 → Mcp-Session-Id → MCP-Protocol-Version →
    // Authorization；大小写不敏感覆盖（§30.19）。值只裁剪首尾、内部空白原样保留（§17.4）。
    let seen: SeenHeaders = Arc::new(Mutex::new(None));
    let recorded = Arc::clone(&seen);
    let server = start_server(move |request, response, _records| {
        let recorded = Arc::clone(&recorded);
        async move {
            if request.method == "POST" {
                let message = request.message.clone().expect("json body");
                if message["method"].as_str() == Some("tools/list") {
                    *recorded.lock().unwrap() = Some((
                        request.header("accept").unwrap_or_default(),
                        request.header("content-type").unwrap_or_default(),
                        request.header("x-custom").unwrap_or_default(),
                        request.header("authorization").unwrap_or_default(),
                        request.header("mcp-session-id").unwrap_or_default(),
                    ));
                }
                return protocol_handler(request, response, Some(message)).await;
            }
            protocol_handler(request, response, None).await;
        }
    })
    .await;
    let provider: Arc<dyn AuthProvider> = Arc::new(TestProvider {
        token: || Some("tok".to_owned()),
        on_unauthorized: |_context: UnauthorizedContext| async {},
    });
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            auth_provider: Some(provider),
            // 调用方在 options.headers 里写小写 authorization → 被 `Authorization: Bearer` 覆盖；
            // x-custom 只裁剪首尾（内部的两个空格保留）。
            headers: Some(vec![
                ("authorization".to_owned(), "Basic nope".to_owned()),
                ("x-custom".to_owned(), "  a  b  ".to_owned()),
            ]),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    send_request(&harness, 2.0, "tools/list", None).await;
    let _ = harness
        .next_message(|message| id_matches(message, 2.0))
        .await;
    let (accept, content_type, custom, authorization, session) =
        seen.lock().unwrap().clone().expect("tools/list recorded");
    assert_eq!(accept, "application/json, text/event-stream");
    assert_eq!(content_type, "application/json");
    assert_eq!(custom, "a  b");
    assert_eq!(authorization, "Bearer tok");
    assert_eq!(session, "session-1");
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn start_and_send_failures_use_the_typescript_messages() {
    // `start()` 两次 → `MCP Streamable HTTP transport already started`；
    // 未 start / 已 close 的 `send` → `McpConnectionClosedError`（§17.3、§27.3）。
    let server = start_protocol_server().await;
    let harness =
        Harness::new(StreamableHttpTransport::new(options(&server.url())).expect("transport"));
    let not_started = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("send before start");
    assert_eq!(not_started.name(), "McpConnectionClosedError");
    assert_eq!(not_started.to_string(), "MCP connection closed");

    harness.transport().start().await.expect("start");
    let again = harness.transport().start().await.expect_err("double start");
    assert_eq!(
        again.to_string(),
        "MCP Streamable HTTP transport already started"
    );
    harness.transport().close().await.expect("close");
    let after_close = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("send after close");
    assert_eq!(after_close.name(), "McpConnectionClosedError");
    // `start()` 在 close 之后：TS 先查 `started`（已在第一次 start 置位），因此报的是
    // `MCP Streamable HTTP transport already started`，而不是 `McpConnectionClosedError`。
    assert_eq!(
        harness
            .transport()
            .start()
            .await
            .expect_err("start after close")
            .to_string(),
        "MCP Streamable HTTP transport already started"
    );
}

#[tokio::test]
async fn unsupported_content_types_and_get_content_types_use_the_typescript_messages() {
    // `Unsupported MCP response content type: {type ?? "missing"}` /
    // `Unsupported MCP GET response content type: {type ?? "missing"}`（§27.3）。
    let mode = Arc::new(Mutex::new("response"));
    let selected = Arc::clone(&mode);
    let server = start_server(move |request, response, _records| {
        let selected = Arc::clone(&selected);
        async move {
            if request.method == "GET" {
                response
                    .send(200, &[("content-type", "application/json")], "{}")
                    .await;
                return;
            }
            if request.method != "POST" {
                return protocol_handler(request, response, None).await;
            }
            let message = request.message.clone().expect("json body");
            if message["method"].as_str() == Some("tools/call") {
                let missing = *selected.lock().unwrap() == "missing";
                if missing {
                    response.send(200, &[], "plain").await;
                } else {
                    response
                        .send(200, &[("content-type", "text/plain")], "plain")
                        .await;
                }
                return;
            }
            protocol_handler(request, response, Some(message)).await;
        }
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            // GET 流开启：收到的 200 + application/json 必须是 `Unsupported MCP GET ...`。
            open_get_stream: Some(true),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    // GET 流的错误经 emitError 到达（`runGetStream` 对不可重试的错误 emitError 后返回）。
    let get_error = harness.next_error().await;
    assert_eq!(
        get_error.to_string(),
        "Unsupported MCP GET response content type: application/json"
    );
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo" } }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("text/plain must fail");
    assert_eq!(
        error.to_string(),
        "Unsupported MCP response content type: text/plain"
    );
    *mode.lock().unwrap() = "missing";
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "echo" } }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("missing content type must fail");
    assert_eq!(
        error.to_string(),
        "Unsupported MCP response content type: missing"
    );
    harness.transport().close().await.expect("close");
}

// ---------------------------------------------------------------------------
// 回归：独立评审报告的偏差（TS 参考逐条实测）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_null_body_json_response_is_a_syntax_error_like_response_json() {
    // 回归：评审 finding #1（major）。
    //
    // TS 的 JSON 分支只写 `const body = await response.json()`，**不**看 `response.body`。
    // fetch 把 101/103/204/205/304 定为无体状态，`Response.text()` 一律是 `""`，于是
    // `JSON.parse("")` 抛 `SyntaxError: Unexpected end of JSON input`——服务器在 205 上发的
    // 字节被丢弃。TS 参考实测（真实 socket、205 + `{"jsonrpc":"2.0","id":2,…}`）：
    // `SyntaxError | "Unexpected end of JSON input"`，`messages: []`。
    //
    // 204 与 202 先被 202/204 分支拦截，205 是唯一走得到的无体 JSON 状态。
    let server = start_server(|request, response, _records| async move {
        if request.method != "POST" {
            return protocol_handler(request, response, None).await;
        }
        let message = request.message.clone().expect("json body");
        if message["method"].as_str() == Some("tools/call") {
            response
                .send(
                    205,
                    &[("content-type", "application/json")],
                    &json!({ "jsonrpc": "2.0", "id": 2, "result": { "ok": true } }).to_string(),
                )
                .await;
            return;
        }
        protocol_handler(request, response, Some(message)).await;
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo" } }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("205 + JSON must fail like Response.json() on a null body");
    assert_eq!(error.name(), "SyntaxError");
    assert_eq!(error.to_string(), "Unexpected end of JSON input");
    // 什么都不能发出去（TS 在 `emitMessage` 之前就抛了）。
    assert_eq!(harness.drain_messages().len(), 0);
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn a_content_type_ending_in_nel_is_not_trimmed_like_javascript() {
    // 回归：评审 finding #2（major）。
    //
    // undici 把头字节按 latin1 解码；服务器发 `content-type: text/event-stream` 后跟一个裸
    // 0x85 字节时，应用看到的是 `"text/event-stream\u{85}"`。JS 的 `String.prototype.trim()`
    // **不**含 U+0085（NEL），所以 type 不等于 `"text/event-stream"`，`send` 抛
    // `Unsupported MCP response content type: text/event-stream`（消息里是 trim 后的实际值）。
    // Rust 的 `str::trim()` 含 NEL，会误判成 SSE 并把体当流消费。
    //
    // TS 参考实测（真实 socket、同样的字节）：`McpHttpError | "Unsupported MCP response
    // content type: text/event-stream"`，`messages: []`。
    let payload = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
    let server = start_server(move |request, response, _records| async move {
        if request.method != "POST" {
            return protocol_handler(request, response, None).await;
        }
        let message = request.message.clone().expect("json body");
        if message["method"].as_str() == Some("tools/call") {
            response
                .send_raw_header(
                    200,
                    &[("content-type", b"text/event-stream\x85")],
                    payload.as_bytes(),
                )
                .await;
            return;
        }
        protocol_handler(request, response, Some(message)).await;
    })
    .await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo" } }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("NEL must survive trim, so this is not text/event-stream");
    assert_eq!(error.name(), "McpHttpError");
    assert_eq!(error.status(), Some(200));
    assert_eq!(
        error.to_string(),
        "Unsupported MCP response content type: text/event-stream\u{85}"
    );
    assert_eq!(harness.drain_messages().len(), 0);
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn a_203_challenge_with_nel_before_the_error_parameter_is_not_a_retryable_403() {
    // 回归：评审 finding #5（minor），端到端。
    //
    // `needsAuthorization` 的正则用 JS 的 `\s`（不含 U+0085）。服务器发
    // `www-authenticate: Bearer\x85error="insufficient_scope"` 时 TS **不**交给认证提供方、
    // 不重试，直接走 `checkResponse` 抛 403 的 `McpHttpError`；Rust 的 `regex` `\s` 含 NEL，
    // 原来会多一次授权往返。
    let seen: SeenChallenges = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let posts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&posts);
    let server = start_server(move |request, response, _records| {
        let counter = Arc::clone(&counter);
        async move {
            if request.method != "POST" {
                return protocol_handler(request, response, None).await;
            }
            let message = request.message.clone().expect("json body");
            if message["method"].as_str() == Some("tools/call") {
                counter.fetch_add(1, Ordering::SeqCst);
                response
                    .send_raw_header(
                        403,
                        &[(
                            "www-authenticate",
                            b"Bearer\x85error=\"insufficient_scope\"",
                        )],
                        b"",
                    )
                    .await;
                return;
            }
            protocol_handler(request, response, Some(message)).await;
        }
    })
    .await;
    let token = Arc::new(Mutex::new(Some("tok".to_owned())));
    let token_for_read = Arc::clone(&token);
    let seen_state = Arc::clone(&recorded);
    let provider: Arc<dyn AuthProvider> = Arc::new(TestProvider {
        token: move || token_for_read.lock().unwrap().clone(),
        on_unauthorized: move |context: UnauthorizedContext| {
            let seen = Arc::clone(&seen_state);
            async move {
                seen.lock()
                    .unwrap()
                    .push((context.status, context.token.clone()));
            }
        },
    });
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            auth_provider: Some(provider),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    connect(&harness).await;
    let error = harness
        .transport()
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo" } }),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("NEL is not whitespace in JS, so the challenge does not match");
    assert_eq!(error.name(), "McpHttpError");
    assert_eq!(error.status(), Some(403));
    assert_eq!(error.to_string(), "MCP HTTP request failed with status 403");
    assert_eq!(
        *seen.lock().unwrap(),
        Vec::new(),
        "onUnauthorized must not run (JS: no match)"
    );
    assert_eq!(
        posts.load(Ordering::SeqCst),
        1,
        "the request must not be retried"
    );
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn an_empty_protocol_version_omits_the_header_like_a_falsy_javascript_field() {
    // 回归：评审 finding #4（minor）。
    //
    // `buildHeaders` 写的是 `if (this.protocolVersion) headers.set(...)`——空串在 JS 里是假值，
    // 因此 `setProtocolVersion("")` 后请求上**没有** `mcp-protocol-version` 头。
    // TS 参考实测（真实 socket）：头列表里没有 `mcp-protocol-version`。
    let server = start_protocol_server().await;
    let harness = Harness::new(
        StreamableHttpTransport::new(StreamableHttpTransportOptions {
            open_get_stream: Some(false),
            ..options(&server.url())
        })
        .expect("transport"),
    );
    harness.transport().start().await.expect("start");
    harness.transport().set_protocol_version("");
    send_request(
        &harness,
        1.0,
        "initialize",
        Some(json!({ "protocolVersion": LATEST_PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "http-test", "version": "1.0.0" } })),
    )
    .await;
    let _ = harness
        .next_message(|message| id_matches(message, 1.0))
        .await;
    let request = server
        .wait_for_request(|entry| entry.method == "POST")
        .await;
    assert_eq!(
        request.header("mcp-protocol-version"),
        None,
        "an empty protocol version is falsy in JS and must omit the header"
    );
    // 非空值仍然会发（对照）。
    harness
        .transport()
        .set_protocol_version(LATEST_PROTOCOL_VERSION);
    send_request(&harness, 2.0, "tools/list", None).await;
    let _ = harness
        .next_message(|message| id_matches(message, 2.0))
        .await;
    let requests = server.requests();
    let with_version = requests
        .iter()
        .rev()
        .find(|entry| entry.method == "POST")
        .expect("second POST");
    assert_eq!(
        with_version.header("mcp-protocol-version").as_deref(),
        Some(LATEST_PROTOCOL_VERSION)
    );
    harness.transport().close().await.expect("close");
}

#[tokio::test]
async fn consume_sse_stream_matches_the_node_text_decoder_on_bom_vectors() {
    // 回归：评审 finding #3（minor），SSE 层。
    //
    // 向量来自 `tests/fixtures/decoder_vectors.json` 的 `sseVectors`（node 26 上跑未修改的
    // TS `consumeSseStream` 生成）。关键一条：`[[0xC3], [0xA9, BOM, "x"×6]]` 在 Node 里
    // BOM 被丢掉，`buffered` 短 3 字节，`maxEventBytes: 8` 下**不**超限；朴素实现会保留 BOM
    // 并抛 `MCP SSE event exceeds 8 bytes`。
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/decoder_vectors.json")).expect("fixture");
    let cases = fixture["sseVectors"].as_array().expect("sseVectors");
    assert!(cases.len() >= 7);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let chunks: Vec<Vec<u8>> = case["chunks"]
            .as_array()
            .expect("chunks")
            .iter()
            .map(|chunk| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(chunk.as_str().expect("chunk"))
                    .expect("base64")
            })
            .collect();
        let stream: FetchByteStream = Box::pin(ChunkStream {
            chunks: chunks.into_iter(),
        });
        let events = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = Arc::clone(&events);
        let result = consume_sse_stream(
            stream,
            ConsumeSseOptions {
                max_event_bytes: case["maxEventBytes"].as_u64().map(|max| max as usize),
                on_event: Some(Arc::new(move |event: SseEvent| {
                    let mut value = serde_json::Map::new();
                    if let Some(event_name) = &event.event {
                        value.insert("event".to_owned(), json!(event_name));
                    }
                    value.insert("data".to_owned(), json!(event.data));
                    if let Some(id) = &event.id {
                        value.insert("id".to_owned(), json!(id));
                    }
                    sink.lock().unwrap().push(Value::Object(value));
                    Ok(())
                })),
                ..ConsumeSseOptions::default()
            },
        )
        .await;
        match case["error"].as_str() {
            Some(expected) => assert_eq!(
                result
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| panic!("{name}: expected error {expected}")),
                expected,
                "{name}"
            ),
            None => result.unwrap_or_else(|error| panic!("{name}: unexpected error {error}")),
        }
        assert_eq!(
            json!(*events.lock().unwrap()),
            case["events"],
            "{name}: emitted events"
        );
    }
}
