//! 客户端测试套件（13 个 `it` 用例，移植自 `packages/mcp/test/client.test.ts`）。
//!
//! TS 用 vitest 的 fake timers（`vi.useFakeTimers` / `vi.advanceTimersByTimeAsync`）；Rust 用
//! `tokio::time::pause` + `tokio::time::advance`（`tokio` 的 `test-util` 特性，仅 dev-dependency）。
//!
//! 测试服务器等价于 `test/client.test.ts` 的 `createServer()`：它记录每条入站消息，并在
//! 独立的 task 里回答请求（TS 的 `queueMicrotask`）。应答异步到达，因此 `send` 先 resolve。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kiana_mcp::auth_provider::McpTransportError;
// 走 crate 根（`index.ts` 的 67 个名字之一）：确认 `pub use` 面可用。
use kiana_mcp::client::ProgressCallback;
use kiana_mcp::jsonrpc::McpError;
use kiana_mcp::protocol_types::LATEST_PROTOCOL_VERSION;
use kiana_mcp::transports::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, Subscription, TransportEvents,
};
use kiana_mcp::{McpClient, McpClientOptions, McpRequestOptions};
use serde_json::{Map, Value, json};

type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send>>;
type Handler = Arc<dyn Fn(Value) -> HandlerFuture + Send + Sync>;

/// 服务端：记录入站消息、按 method 分派 handler，并把应答异步送回客户端。
#[derive(Clone)]
struct TestServer {
    transport: Arc<TestTransport>,
    messages: Arc<Mutex<Vec<Value>>>,
    handlers: Arc<Mutex<HashMap<String, Handler>>>,
}

struct TestTransport {
    events: TransportEvents,
    outbox: tokio::sync::mpsc::UnboundedSender<Value>,
    started: Mutex<bool>,
    closed: Mutex<bool>,
}

#[async_trait]
impl McpTransport for TestTransport {
    async fn start(&self) -> Result<(), McpTransportError> {
        if *self.closed.lock().unwrap() {
            return Err(McpTransportError::from(McpError::connection_closed()));
        }
        *self.started.lock().unwrap() = true;
        Ok(())
    }

    async fn send(
        &self,
        message: Value,
        _options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        if !*self.started.lock().unwrap() || *self.closed.lock().unwrap() {
            return Err(McpTransportError::from(McpError::connection_closed()));
        }
        let _ = self.outbox.send(message);
        Ok(())
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        {
            let mut closed = self.closed.lock().unwrap();
            if *closed {
                return Ok(());
            }
            *closed = true;
        }
        let _ = self.events.emit_close();
        Ok(())
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.events.on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.events.on_close(move || listener())
    }
}

impl TestServer {
    fn new() -> Self {
        let (outbox, mut inbox) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let transport = Arc::new(TestTransport {
            events: TransportEvents::new(),
            outbox,
            started: Mutex::new(false),
            closed: Mutex::new(false),
        });
        let messages: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let handlers: Arc<Mutex<HashMap<String, Handler>>> = Arc::new(Mutex::new(HashMap::new()));

        let server = TestServer {
            transport: Arc::clone(&transport),
            messages: Arc::clone(&messages),
            handlers: Arc::clone(&handlers),
        };

        // 服务端主循环：记录每条消息，并在新的 task 里应答请求。
        let loop_transport = Arc::clone(&transport);
        tokio::spawn(async move {
            while let Some(message) = inbox.recv().await {
                messages.lock().unwrap().push(message.clone());
                if message.get("id").is_none() || message.get("method").is_none() {
                    continue;
                }
                let handler = {
                    let handlers = handlers.lock().unwrap();
                    handlers
                        .get(message["method"].as_str().unwrap_or_default())
                        .cloned()
                };
                let method = message["method"].as_str().unwrap_or_default().to_owned();
                let reply_transport = Arc::clone(&loop_transport);
                tokio::spawn(async move {
                    let id = message["id"].clone();
                    let outcome = match handler {
                        Some(handler) => handler(message.clone()).await,
                        None => Err(McpError::new(-32601, format!("Method not found: {method}"))),
                    };
                    let mut reply = Map::new();
                    reply.insert("jsonrpc".to_owned(), json!("2.0"));
                    reply.insert("id".to_owned(), id);
                    match outcome {
                        Ok(result) => {
                            reply.insert("result".to_owned(), result);
                        }
                        Err(error) => {
                            let mut object = Map::new();
                            object.insert("code".to_owned(), json!(error.code().unwrap_or(-32603)));
                            object.insert("message".to_owned(), json!(error.to_string()));
                            if let Some(data) = error.data() {
                                object.insert("data".to_owned(), data.clone());
                            }
                            reply.insert("error".to_owned(), Value::Object(object));
                        }
                    }
                    let _ = reply_transport.events.emit_message(&Value::Object(reply));
                });
            }
        });

        server
    }

    fn set_handler<F, Fut>(&self, method: &str, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, McpError>> + Send + 'static,
    {
        self.handlers.lock().unwrap().insert(
            method.to_owned(),
            Arc::new(move |request| Box::pin(handler(request))),
        );
    }

    fn messages(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }

    /// 等到至少 `count` 条入站消息（`yield` 直到服务端主循环收到它们）。
    async fn wait_messages(&self, count: usize) -> Vec<Value> {
        for _ in 0..10_000 {
            let messages = self.messages();
            if messages.len() >= count {
                return messages;
            }
            tokio::task::yield_now().await;
        }
        self.messages()
    }

    /// 让服务端把客户端所有已排队的异步工作跑完（等价于 `await new Promise(r => setTimeout(r, 0))`）。
    async fn settle(&self) {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }
}

async fn create_server() -> (Arc<TestTransport>, TestServer) {
    let server = TestServer::new();
    server.set_handler("initialize", |_request| async {
        Ok(json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "test-server", "version": "1.0.0"},
            "instructions": "Use test tools.",
        }))
    });
    server.transport.start().await.expect("server starts");
    (Arc::clone(&server.transport), server)
}

async fn connect() -> (McpClient, TestServer) {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("test-client", "2.0.0"));
    client.connect(transport).await.expect("connect succeeds");
    (client, server)
}

// ---------------------------------------------------------------------------
// 13 个 `it`（标题逐字）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initializes_the_connection_before_exposing_server_information() {
    let (client, server) = connect().await;
    assert_eq!(
        client.connection_state(),
        kiana_mcp::client::ConnectionState::Connected
    );
    assert_eq!(
        client.protocol_version().as_deref(),
        Some(LATEST_PROTOCOL_VERSION)
    );
    assert_eq!(
        serde_json::to_value(client.server_info().expect("server info")).unwrap(),
        json!({"name": "test-server", "version": "1.0.0"})
    );
    assert_eq!(
        serde_json::to_value(client.server_capabilities().expect("capabilities")).unwrap(),
        json!({"tools": {"listChanged": true}})
    );
    assert_eq!(client.instructions().as_deref(), Some("Use test tools."));
    assert_eq!(
        server.wait_messages(2).await,
        vec![
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "2.0.0"},
                },
            }),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        ]
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn paginates_tools_and_preserves_protocol_tool_definitions() {
    let (client, server) = connect().await;
    server.set_handler("tools/list", |request| async move {
        let cursor = request
            .get("params")
            .and_then(|params| params.get("cursor"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(match cursor {
            None => json!({
                "tools": [{"name": "search", "description": "Search", "inputSchema": {"type": "object"}}],
                "nextCursor": "page-2",
            }),
            Some(_) => json!({
                "tools": [{
                    "name": "read",
                    "inputSchema": {"type": "object"},
                    "outputSchema": {"type": "object"},
                    "annotations": {"readOnlyHint": true},
                }],
                // 有些服务器用空 cursor 结束分页。
                "nextCursor": "",
            }),
        })
    });
    let tools = client
        .list_tools(&McpRequestOptions::new())
        .await
        .expect("list tools");
    assert_eq!(
        serde_json::to_value(&tools).unwrap(),
        json!([
            {"name": "search", "description": "Search", "inputSchema": {"type": "object"}},
            {
                "name": "read",
                "inputSchema": {"type": "object"},
                "outputSchema": {"type": "object"},
                "annotations": {"readOnlyHint": true},
            },
        ])
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn lists_and_reads_resources() {
    let (client, server) = connect().await;
    server.set_handler("resources/list", |request| async move {
        let cursor = request
            .get("params")
            .and_then(|params| params.get("cursor"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(match cursor {
            None => json!({"resources": [{"uri": "file:///a", "name": "a", "mimeType": "text/plain"}], "nextCursor": "2"}),
            Some(_) => json!({"resources": [{"uri": "file:///b"}]}),
        })
    });
    server.set_handler("resources/templates/list", |_request| async {
        Ok(json!({"resourceTemplates": [{"uriTemplate": "repo://{owner}/{repo}", "name": "repo"}]}))
    });
    server.set_handler("resources/read", |request| async move {
        let uri = request["params"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        Ok(json!({"contents": [{"uri": uri, "text": "hello"}]}))
    });
    // 缺失的 name 回退为 URI。
    let resources = client
        .list_resources(&McpRequestOptions::new())
        .await
        .expect("list resources");
    assert_eq!(
        serde_json::to_value(&resources).unwrap(),
        json!([
            {"uri": "file:///a", "name": "a", "mimeType": "text/plain"},
            {"uri": "file:///b", "name": "file:///b"},
        ])
    );
    let templates = client
        .list_resource_templates(&McpRequestOptions::new())
        .await
        .expect("list templates");
    assert_eq!(
        serde_json::to_value(&templates).unwrap(),
        json!([{"uriTemplate": "repo://{owner}/{repo}", "name": "repo"}])
    );
    // 单页把 cursor 原样透传。
    let page = client
        .list_resources_page(None, &McpRequestOptions::new())
        .await
        .expect("page 1");
    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        json!({
            "resources": [{"uri": "file:///a", "name": "a", "mimeType": "text/plain"}],
            "nextCursor": "2",
        })
    );
    let page = client
        .list_resources_page(Some("2"), &McpRequestOptions::new())
        .await
        .expect("page 2");
    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        json!({"resources": [{"uri": "file:///b", "name": "file:///b"}]})
    );
    let read = client
        .read_resource("file:///a", &McpRequestOptions::new())
        .await
        .expect("read resource");
    assert_eq!(
        serde_json::to_value(&read).unwrap(),
        json!({"contents": [{"uri": "file:///a", "text": "hello"}]})
    );

    server.set_handler("resources/read", |_request| async {
        Ok(json!({"contents": [{"uri": "file:///a"}]}))
    });
    let error = client
        .read_resource("file:///a", &McpRequestOptions::new())
        .await
        .expect_err("missing text/blob");
    assert_eq!(
        error.to_string(),
        "Invalid contents in MCP resources/read result"
    );
    server.set_handler("resources/list", |_request| async {
        Ok(json!({"resources": [{"name": "no uri"}]}))
    });
    let error = client
        .list_resources(&McpRequestOptions::new())
        .await
        .expect_err("missing uri");
    assert_eq!(
        error.to_string(),
        "Invalid entry in MCP resources/list result"
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn returns_structured_tool_content_and_surfaces_json_rpc_errors() {
    let (client, server) = connect().await;
    server.set_handler("tools/call", |request| async move {
        let name = request["params"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if name == "fail" {
            return Err(McpError::with_data(
                1234,
                "tool failed",
                json!({"retryable": false}),
            ));
        }
        let count = request["params"]["arguments"]["count"].clone();
        Ok(json!({
            "content": [{"type": "text", "text": "ok"}],
            "structuredContent": {"count": count},
        }))
    });
    let mut args = Map::new();
    args.insert("count".to_owned(), json!(3));
    let result = client
        .call_tool("count", Some(args), &McpRequestOptions::new())
        .await
        .expect("call tool");
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"content": [{"type": "text", "text": "ok"}], "structuredContent": {"count": 3}})
    );
    let error = client
        .call_tool("fail", None, &McpRequestOptions::new())
        .await
        .expect_err("tool failed");
    assert_eq!(error.name(), "McpError");
    let mcp = error.as_mcp().expect("wrapped McpError");
    assert_eq!(mcp.code(), Some(1234));
    assert_eq!(error.to_string(), "tool failed");
    assert_eq!(mcp.data(), Some(&json!({"retryable": false})));
    client.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn renews_the_timeout_on_progress() {
    let (client, server) = connect().await;
    let progress_transport = Arc::clone(&server.transport);
    server.set_handler("tools/call", move |request| {
        let transport = Arc::clone(&progress_transport);
        async move {
            let token = request["params"]["_meta"]["progressToken"].clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let _ = transport.events.emit_message(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {"progressToken": token, "progress": 1, "total": 2},
                }));
            });
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok(json!({"content": [{"type": "text", "text": "done"}]}))
        }
    });
    let progress: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&progress);
    let callback: ProgressCallback = Arc::new(move |params| {
        sink.lock().unwrap().push(params.clone());
        Ok(())
    });
    let options = McpRequestOptions::new()
        .with_timeout_ms(50.0)
        .with_on_progress(callback);
    let call = tokio::spawn({
        let client = client.clone();
        async move { client.call_tool("slow", None, &options).await }
    });
    // 让 `call_tool` 注册 pending 条目与 50ms 定时器。
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(40)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(40)).await;
    let result = call.await.expect("task").expect("call tool");
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"content": [{"type": "text", "text": "done"}]})
    );
    assert_eq!(
        *progress.lock().unwrap(),
        vec![json!({"progressToken": 2, "progress": 1, "total": 2})]
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn cancels_aborted_and_timed_out_requests() {
    let (client, server) = connect().await;
    server.set_handler("tools/call", |_request| async {
        std::future::pending::<Result<Value, McpError>>().await
    });
    let signal = tokio_util::sync::CancellationToken::new();
    let aborted = {
        let client = client.clone();
        let options = McpRequestOptions::new().with_signal_reason(signal.clone(), json!("stop"));
        tokio::spawn(async move { client.call_tool("wait", None, &options).await })
    };
    tokio::task::yield_now().await;
    signal.cancel();
    let error = aborted.await.expect("task").expect_err("aborted");
    assert!(error.is_abort(), "expected AbortError, got {error:?}");
    let messages = server.wait_messages(3).await;
    assert!(
        messages.contains(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 2, "reason": "stop"},
        })),
        "messages: {messages:#?}"
    );

    let error = client
        .call_tool("wait", None, &McpRequestOptions::new().with_timeout_ms(5.0))
        .await
        .expect_err("timed out");
    assert_eq!(error.name(), "McpTimeoutError");
    client.close().await.expect("close");
}

#[tokio::test]
async fn reports_transport_errors_without_failing_pending_requests() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("test-client", "1.0.0"));
    client.connect(transport.clone()).await.expect("connect");
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let _subscription = client.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.to_string());
        Ok(())
    }));
    let respond: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&respond);
    server.set_handler("tools/call", move |_request| {
        let slot = Arc::clone(&slot);
        async move {
            let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
            *slot.lock().unwrap() = Some(sender);
            let _ = receiver.await;
            Ok(json!({"content": []}))
        }
    });
    let call = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .call_tool("wait", None, &McpRequestOptions::new())
                .await
        })
    };
    tokio::task::yield_now().await;
    transport
        .events
        .emit_error(&McpTransportError::new("stray log line"))
        .expect("emit error");
    let sender = respond.lock().unwrap().take();
    if let Some(sender) = sender {
        let _ = sender.send(());
    }
    let result = call.await.expect("task").expect("call resolves");
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"content": []})
    );
    assert_eq!(*errors.lock().unwrap(), vec!["stray log line".to_owned()]);
    client.close().await.expect("close");
}

#[tokio::test]
async fn accepts_servers_that_answer_with_an_older_protocol_version() {
    let (transport, server) = create_server().await;
    server.set_handler("initialize", |_request| async {
        Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {"name": "old-server", "version": "0.1.0"},
        }))
    });
    let client = McpClient::new(McpClientOptions::new("test-client", "1.0.0"));
    client.connect(transport).await.expect("connect");
    assert_eq!(client.protocol_version().as_deref(), Some("2024-11-05"));
    client.close().await.expect("close");

    let (unsupported_transport, unsupported) = create_server().await;
    unsupported.set_handler("initialize", |_request| async {
        Ok(json!({
            "protocolVersion": "1999-01-01",
            "capabilities": {},
            "serverInfo": {"name": "ancient-server", "version": "0.1.0"},
        }))
    });
    let rejected = McpClient::new(McpClientOptions::new("test-client", "1.0.0"));
    let error = rejected
        .connect(unsupported_transport)
        .await
        .expect_err("unsupported version");
    assert!(
        error.to_string().contains("unsupported protocol version"),
        "message: {error}"
    );
    assert_eq!(
        rejected.connection_state(),
        kiana_mcp::client::ConnectionState::Closed
    );
}

#[tokio::test]
async fn defaults_missing_tool_result_content_to_an_empty_list() {
    let (client, server) = connect().await;
    server.set_handler("tools/call", |_request| async {
        Ok(json!({"structuredContent": {"ok": true}}))
    });
    let result = client
        .call_tool("structured", None, &McpRequestOptions::new())
        .await
        .expect("call");
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"content": [], "structuredContent": {"ok": true}})
    );
    server.set_handler("tools/call", |_request| async {
        Ok(json!({"content": "not a list"}))
    });
    let error = client
        .call_tool("broken", None, &McpRequestOptions::new())
        .await
        .expect_err("invalid content");
    assert_eq!(error.to_string(), "Invalid MCP tools/call result");
    client.close().await.expect("close");
}

// #10416: MCP 2026-07-28 允许 structuredContent 是任意 JSON 值。
#[tokio::test]
async fn accepts_structured_content_that_is_not_an_object() {
    let (client, server) = connect().await;
    for structured in [
        json!([{"id": 1}, {"id": 2}]),
        json!(null),
        json!("text"),
        json!(3),
        json!(false),
    ] {
        let value = structured.clone();
        server.set_handler("tools/call", move |_request| {
            let value = value.clone();
            async move { Ok(json!({"content": [], "structuredContent": value})) }
        });
        let result = client
            .call_tool("structured", None, &McpRequestOptions::new())
            .await
            .expect("call");
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"content": [], "structuredContent": structured}),
        );
    }
    client.close().await.expect("close");
}

#[tokio::test]
async fn does_not_send_notifications_cancelled_for_a_timed_out_initialize() {
    let (transport, server) = create_server().await;
    server.set_handler("initialize", |_request| async {
        std::future::pending::<Result<Value, McpError>>().await
    });
    let client =
        McpClient::new(McpClientOptions::new("test-client", "1.0.0").with_request_timeout_ms(5.0));
    let error = client.connect(transport).await.expect_err("timed out");
    assert_eq!(error.name(), "McpTimeoutError");
    server.settle().await;
    assert!(
        !server
            .messages()
            .iter()
            .any(|message| message.get("method").and_then(Value::as_str)
                == Some("notifications/cancelled")),
        "messages: {:#?}",
        server.messages()
    );
}

#[tokio::test]
async fn notifies_close_listeners_once_when_the_transport_drops() {
    let (client, server) = connect().await;
    let closed = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&closed);
    let _subscription = client.on_close(Arc::new(move || {
        *sink.lock().unwrap() += 1;
        Ok(())
    }));
    server.set_handler("tools/call", |_request| async {
        std::future::pending::<Result<Value, McpError>>().await
    });
    let pending = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .call_tool("wait", None, &McpRequestOptions::new())
                .await
        })
    };
    tokio::task::yield_now().await;
    server.transport.close().await.expect("server closes");
    let error = pending.await.expect("task").expect_err("connection closed");
    assert_eq!(error.to_string(), "MCP connection closed");
    assert_eq!(
        client.connection_state(),
        kiana_mcp::client::ConnectionState::Closed
    );
    client.close().await.expect("close");
    assert_eq!(*closed.lock().unwrap(), 1);
}

#[tokio::test]
async fn answers_roots_list_and_dispatches_notifications() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("test-client", "1.0.0").with_roots(
        kiana_mcp::client::Roots::list(vec![kiana_mcp::protocol_types::Root {
            uri: "file:///workspace".to_owned(),
            name: Some("workspace".to_owned()),
            extra: Map::new(),
        }]),
    ));
    client.connect(transport).await.expect("connect");
    let changed = Arc::new(Mutex::new(Vec::<Option<Value>>::new()));
    let sink = Arc::clone(&changed);
    let _subscription = client.on_notification(
        "notifications/tools/list_changed",
        Arc::new(move |params| {
            sink.lock().unwrap().push(params.cloned());
            Ok(())
        }),
    );
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "roots", "method": "roots/list"}))
        .expect("roots request");
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}))
        .expect("notification");
    server.settle().await;
    assert!(
        server.messages().contains(&json!({
            "jsonrpc": "2.0",
            "id": "roots",
            "result": {"roots": [{"uri": "file:///workspace", "name": "workspace"}]},
        })),
        "messages: {:#?}",
        server.messages()
    );
    assert_eq!(*changed.lock().unwrap(), vec![None]);
    client.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// 黄金向量：TS 参考实现（`scratch/kiana-mcp-client/probe.mjs`）的实测输出
// ---------------------------------------------------------------------------

/// `requireTransport` 的错误消息（§30.27）。
#[tokio::test]
async fn require_transport_messages_match_the_typescript_vectors() {
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    let error = client
        .ping(&McpRequestOptions::new())
        .await
        .expect_err("idle");
    assert_eq!(error.name(), "McpConnectionClosedError");
    assert_eq!(error.to_string(), "MCP client is idle");
    let error = client.notify("x", None).await.expect_err("idle notify");
    assert_eq!(error.to_string(), "MCP client is idle");
    client.close().await.expect("close");
    let error = client
        .request("x", None, &McpRequestOptions::new())
        .await
        .expect_err("closed");
    assert_eq!(error.to_string(), "MCP client is closed");

    let (transport, _server) = create_server().await;
    let error = client
        .connect(transport)
        .await
        .expect_err("cannot reconnect");
    assert_eq!(
        error.to_string(),
        "Cannot connect MCP client in closed state"
    );
}

/// 早期抛出不消耗 id：idle 下的 `ping` 失败后 initialize 仍是 1；预先 abort 的 `ping` 不占号。
#[tokio::test]
async fn request_ids_are_not_consumed_by_early_failures() {
    let (transport, server) = create_server().await;
    server.set_handler("ping", |_request| async { Ok(json!({})) });
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    // idle 下的失败（`requireTransport`）不消耗 id。
    let _ = client.ping(&McpRequestOptions::new()).await;
    client.connect(transport).await.expect("connect");
    // 预先 abort 的 signal（`options.signal?.aborted` 在 `nextRequestId++` 之前）。
    let signal = tokio_util::sync::CancellationToken::new();
    signal.cancel();
    let error = client
        .ping(&McpRequestOptions::new().with_signal(signal))
        .await
        .expect_err("pre-aborted");
    assert_eq!(error.name(), "AbortError");
    assert_eq!(error.to_string(), "MCP request aborted");
    client.ping(&McpRequestOptions::new()).await.expect("ping");
    let ids: Vec<Value> = server
        .messages()
        .iter()
        .filter_map(|message| message.get("id").cloned())
        .collect();
    assert_eq!(ids, vec![json!(1), json!(2)]);
    client.close().await.expect("close");
}

/// 超时消耗 id（序列里出现空洞）：`tools/call` 占 3，之后 `ping` 是 4。
#[tokio::test]
async fn timed_out_requests_consume_request_ids() {
    let (transport, server) = create_server().await;
    server.set_handler("tools/call", |_request| async {
        std::future::pending::<Result<Value, McpError>>().await
    });
    server.set_handler("ping", |_request| async { Ok(json!({})) });
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client.connect(transport).await.expect("connect");
    let error = client
        .call_tool("slow", None, &McpRequestOptions::new().with_timeout_ms(1.0))
        .await
        .expect_err("timed out");
    assert_eq!(error.to_string(), "MCP request timed out after 1ms");
    client.ping(&McpRequestOptions::new()).await.expect("ping");
    let ids: Vec<Value> = server
        .messages()
        .iter()
        .filter(|message| message.get("id").is_some())
        .map(|message| message["id"].clone())
        .collect();
    assert_eq!(ids, vec![json!(1), json!(2), json!(3)]);
    client.close().await.expect("close");
}

/// 取消通知的 `reason` 字符串：§13.5 的整张表（本机 Node 26 实测）。
#[tokio::test]
async fn cancelled_notifications_match_every_typescript_reason_vector() {
    struct Case {
        label: &'static str,
        reason: Option<Value>,
        expected: Option<&'static str>,
    }
    let cases = [
        Case {
            label: "bare",
            reason: None,
            expected: Some("AbortError: This operation was aborted"),
        },
        Case {
            label: "null",
            reason: Some(Value::Null),
            expected: Some("Aborted"),
        },
        Case {
            label: "empty",
            reason: Some(json!("")),
            expected: None,
        },
        Case {
            label: "zero",
            reason: Some(json!(0)),
            expected: Some("0"),
        },
        Case {
            label: "false",
            reason: Some(json!(false)),
            expected: Some("false"),
        },
        Case {
            label: "stop",
            reason: Some(json!("stop")),
            expected: Some("stop"),
        },
        Case {
            label: "object",
            reason: Some(json!({})),
            expected: Some("[object Object]"),
        },
    ];
    for case in cases {
        let (transport, server) = create_server().await;
        server.set_handler("tools/call", |_request| async {
            std::future::pending::<Result<Value, McpError>>().await
        });
        let client = McpClient::new(McpClientOptions::new("c", "1"));
        client.connect(transport).await.expect("connect");
        let signal = tokio_util::sync::CancellationToken::new();
        let mut options = McpRequestOptions::new().with_signal(signal.clone());
        if let Some(reason) = &case.reason {
            options.signal_reason = Some(reason.clone());
        }
        let call = {
            let client = client.clone();
            tokio::spawn(async move { client.call_tool("wait", None, &options).await })
        };
        tokio::task::yield_now().await;
        signal.cancel();
        call.await.expect("task").expect_err("aborted");
        let messages = server.wait_messages(3).await;
        let cancelled = messages
            .iter()
            .find(|message| {
                message.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
            })
            .unwrap_or_else(|| {
                panic!("{}: no cancelled notification in {messages:#?}", case.label)
            });
        let expected = match case.expected {
            Some(reason) => json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 2, "reason": reason},
            }),
            // `...(reason ? {reason} : {})`：空字符串连键都不写。
            None => json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 2},
            }),
        };
        assert_eq!(*cancelled, expected, "case {}", case.label);
        client.close().await.expect("close");
    }
}

/// `handleMessage` 的归类顺序：response 优先（重叠消息报 unknown request）。
#[tokio::test]
async fn overlapping_messages_are_classified_as_responses_first() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let _subscription = client.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.to_string());
        Ok(())
    }));
    client.connect(transport).await.expect("connect");
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "m", "result": {}}))
        .expect("overlap 1");
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "1", "result": {}}))
        .expect("overlap 2");
    assert_eq!(
        *errors.lock().unwrap(),
        vec![
            "Received response for unknown MCP request 1".to_owned(),
            "Received response for unknown MCP request 1".to_owned(),
        ]
    );
    // 非法消息产生 `-32600 "Received invalid JSON-RPC message"`，通知不产生错误。
    errors.lock().unwrap().clear();
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "nope": true}))
        .expect("invalid");
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "method": "notifications/x", "params": 1}))
        .expect("notification");
    assert_eq!(
        *errors.lock().unwrap(),
        vec!["Received invalid JSON-RPC message".to_owned()]
    );
    client.close().await.expect("close");
}

/// 分页错误消息：重复 cursor 与 1000 页上限（§30.25 第 8、9 条）。
#[tokio::test]
async fn pagination_errors_are_verbatim() {
    let (transport, server) = create_server().await;
    server.set_handler("tools/list", |_request| async {
        Ok(json!({"tools": [], "nextCursor": "same"}))
    });
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client.connect(transport).await.expect("connect");
    let error = client
        .list_tools(&McpRequestOptions::new())
        .await
        .expect_err("duplicate");
    assert_eq!(error.name(), "Error");
    assert_eq!(
        error.to_string(),
        "MCP tools/list returned duplicate cursor: same"
    );
    client.close().await.expect("close");

    // `MAX_LIST_PAGES`：每页给一个新 cursor，第 1000 页之后抛出。
    let (transport, server) = create_server().await;
    server.set_handler("tools/list", |request| async move {
        let page = request
            .get("params")
            .and_then(|params| params.get("cursor"))
            .and_then(Value::as_str)
            .and_then(|cursor| cursor.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(json!({"tools": [], "nextCursor": (page + 1).to_string()}))
    });
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client.connect(transport).await.expect("connect");
    let error = client
        .list_tools(&McpRequestOptions::new())
        .await
        .expect_err("too many pages");
    assert_eq!(error.to_string(), "MCP tools/list exceeded 1000 pages");
    // 评审 finding 1：请求在每轮**开头**发出，第 1000 页之后直接抛错——恰好 1000 个
    // `tools/list` 请求（修复前在轮末预取，多发一个第 1001 个请求并多消耗一个 id）。
    server.settle().await;
    let list_requests = server
        .messages()
        .into_iter()
        .filter(|message| message.get("method") == Some(&json!("tools/list")))
        .count();
    assert_eq!(
        list_requests, 1000,
        "exactly MAX_LIST_PAGES requests, never a 1001st"
    );
    // id 分配是**同步**的（在 `start_list_page` 里）：第 1001 个页请求若被发出会占掉
    // 下一个 id。`initialize` = 1、1000 页 = 2..=1001，因此下一个请求必须是 1002。
    let _ = client.ping(&McpRequestOptions::new()).await;
    let ping_id = server
        .messages()
        .into_iter()
        .find(|message| message.get("method") == Some(&json!("ping")))
        .map(|message| message["id"].clone())
        .expect("ping recorded");
    assert_eq!(
        ping_id,
        json!(1002),
        "no id was consumed by a 1001st list page"
    );
    let last_cursor = server
        .messages()
        .into_iter()
        .filter(|message| message.get("method") == Some(&json!("tools/list")))
        .filter_map(|message| message["params"]["cursor"].as_str().map(str::to_owned))
        .next_back();
    assert_eq!(last_cursor.as_deref(), Some("999"));
    client.close().await.expect("close");
}

/// `roots` 提供者：每次调用都重新求值，且返回一份新数组。
#[tokio::test]
async fn roots_provider_is_awaited_on_every_call() {
    let (transport, server) = create_server().await;
    let calls = Arc::new(Mutex::new(0u32));
    let counter = Arc::clone(&calls);
    let client = McpClient::new(McpClientOptions::new("c", "1").with_roots(
        kiana_mcp::client::Roots::provider(move || {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                let n = {
                    let mut calls = counter.lock().unwrap();
                    *calls += 1;
                    *calls
                };
                vec![kiana_mcp::protocol_types::Root {
                    uri: format!("file:///run-{n}"),
                    name: None,
                    extra: Map::new(),
                }]
            })
        }),
    ));
    client.connect(transport).await.expect("connect");
    for _ in 0..2 {
        server
            .transport
            .events
            .emit_message(&json!({"jsonrpc": "2.0", "id": "roots", "method": "roots/list"}))
            .expect("roots request");
        server.settle().await;
    }
    let values: Vec<Value> = server
        .messages()
        .into_iter()
        .filter(|message| {
            message.get("id").and_then(Value::as_str) == Some("roots")
                && message.get("result").is_some()
        })
        .map(|message| message["result"].clone())
        .collect();
    assert_eq!(
        values,
        vec![
            json!({"roots": [{"uri": "file:///run-1"}]}),
            json!({"roots": [{"uri": "file:///run-2"}]}),
        ]
    );
    assert_eq!(*calls.lock().unwrap(), 2);
    client.close().await.expect("close");
}

/// `connect` 的 `params.capabilities`：配置了 roots 且未显式给出时补 `{}`；
/// 显式 capabilities 原样透传（`{...options.capabilities}`）。
#[tokio::test]
async fn roots_enable_the_roots_capability_only_when_unset() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(
        McpClientOptions::new("c", "1").with_roots(kiana_mcp::client::Roots::list(Vec::new())),
    );
    client.connect(transport).await.expect("connect");
    let initialize = server
        .messages()
        .into_iter()
        .find(|m| m.get("method") == Some(&json!("initialize")))
        .unwrap();
    assert_eq!(initialize["params"]["capabilities"], json!({"roots": {}}));
    client.close().await.expect("close");

    let capabilities = kiana_mcp::protocol_types::ClientCapabilities {
        roots: Some(kiana_mcp::protocol_types::ListChangedCapability {
            list_changed: Some(true),
            extra: Map::new(),
        }),
        ..kiana_mcp::protocol_types::ClientCapabilities::default()
    };
    let (transport, server) = create_server().await;
    let client = McpClient::new(
        McpClientOptions::new("c", "1")
            .with_roots(kiana_mcp::client::Roots::list(Vec::new()))
            .with_capabilities(capabilities),
    );
    client.connect(transport).await.expect("connect");
    let initialize = server
        .messages()
        .into_iter()
        .find(|m| m.get("method") == Some(&json!("initialize")))
        .unwrap();
    assert_eq!(
        initialize["params"]["capabilities"],
        json!({"roots": {"listChanged": true}})
    );
    client.close().await.expect("close");
}

/// `clientInfo.title`：给出时写出，缺省时省略该键。
#[tokio::test]
async fn client_info_title_is_optional() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("c", "1").with_title("Kiana"));
    client.connect(transport).await.expect("connect");
    let initialize = server
        .messages()
        .into_iter()
        .find(|m| m.get("method") == Some(&json!("initialize")))
        .unwrap();
    assert_eq!(
        initialize["params"]["clientInfo"],
        json!({"name": "c", "version": "1", "title": "Kiana"})
    );
    client.close().await.expect("close");
}

/// 传输关闭后，新请求在 `requireTransport` 处失败（§13.8 / §30.27：`MCP client is closed`）。
#[tokio::test]
async fn requests_after_the_transport_dropped_fail_at_require_transport() {
    let (client, server) = connect().await;
    server.set_handler("tools/call", |_request| async {
        Ok(json!({"content": []}))
    });
    server
        .transport
        .close()
        .await
        .expect("client closes server");
    let error = client
        .call_tool("x", None, &McpRequestOptions::new())
        .await
        .expect_err("transport dropped");
    assert_eq!(error.name(), "McpConnectionClosedError");
    assert_eq!(error.to_string(), "MCP client is closed");
    client.close().await.expect("close");
}

/// `notify` 在 `connecting` 状态不可用（§30.25 第 10 条）：只有 `connect` 内部的调用允许。
#[tokio::test]
async fn notifications_require_a_connected_client() {
    let (transport, _server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    // connect 之前是 idle。
    let error = client.notify("x", None).await.expect_err("idle notify");
    assert_eq!(error.to_string(), "MCP client is idle");
    client.connect(transport).await.expect("connect");
    client
        .notify("notifications/x", Some(json!({"a": 1})))
        .await
        .expect("notify");
    client.close().await.expect("close");
}

/// `close()` 幂等：重复调用不报错，close 监听器只触发一次（含传输也关闭的场景）。
#[tokio::test]
async fn close_is_idempotent() {
    let (client, server) = connect().await;
    let closed = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&closed);
    let _subscription = client.on_close(Arc::new(move || {
        *sink.lock().unwrap() += 1;
        Ok(())
    }));
    client.close().await.expect("close 1");
    client.close().await.expect("close 2");
    server.transport.close().await.expect("server close");
    assert_eq!(*closed.lock().unwrap(), 1);
    assert_eq!(
        client.connection_state(),
        kiana_mcp::client::ConnectionState::Closed
    );
}

/// 服务端请求无 handler 时回 `-32601 Method not found: <method>`。
#[tokio::test]
async fn unknown_server_requests_get_method_not_found() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client.connect(transport).await.expect("connect");
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": 9, "method": "unknown/method"}))
        .expect("request");
    server.settle().await;
    assert!(
        server.messages().contains(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": {"code": -32601, "message": "Method not found: unknown/method"},
        })),
        "messages: {:#?}",
        server.messages()
    );
    client.close().await.expect("close");
}

/// `onProgress` 给 `params` 追加 `_meta.progressToken`（等于请求 id），并保留原有键序。
#[tokio::test]
async fn progress_requests_carry_the_request_id_as_progress_token() {
    let (transport, server) = create_server().await;
    server.set_handler("tools/call", |_request| async {
        Ok(json!({"content": []}))
    });
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client.connect(transport).await.expect("connect");
    let mut args = Map::new();
    args.insert("count".to_owned(), json!(1));
    client
        .call_tool(
            "x",
            Some(args),
            &McpRequestOptions::new().with_on_progress(Arc::new(|_| Ok(()))),
        )
        .await
        .expect("call");
    let messages = server.messages();
    let call = messages
        .iter()
        .find(|message| message.get("method").and_then(Value::as_str) == Some("tools/call"))
        .expect("tools/call");
    assert_eq!(
        call,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "x", "arguments": {"count": 1}, "_meta": {"progressToken": 2}},
        })
    );
    assert_eq!(
        kiana_json::js_object::stringify(call),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"x","arguments":{"count":1},"_meta":{"progressToken":2}}}"#
    );
    client.close().await.expect("close");
}

/// 服务端请求被 `notifications/cancelled` 取消：`context.signal` 被取消并带上 reason。
#[tokio::test]
async fn server_requests_can_be_cancelled_by_the_server() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client.connect(transport).await.expect("connect");
    let _handler = client.set_request_handler(
        "slow",
        Arc::new(|_params, context: kiana_mcp::client::RequestContext| {
            Box::pin(async move {
                context.signal().cancelled().await;
                Err(McpTransportError::new(format!("{:?}", context.reason())))
            })
        }),
    );
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "srv-1", "method": "slow"}))
        .expect("server request");
    tokio::task::yield_now().await;
    server
        .transport
        .events
        .emit_message(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": "srv-1", "reason": "user"},
        }))
        .expect("cancel");
    server.settle().await;
    let response = server
        .messages()
        .into_iter()
        .find(|message| {
            message.get("id") == Some(&json!("srv-1")) && message.get("error").is_some()
        })
        .expect("error response");
    assert_eq!(response["error"]["code"], json!(-32603));
    assert_eq!(
        response["error"]["message"],
        json!("Some(String(\"user\"))")
    );
    client.close().await.expect("close");
}

/// handler 抛 `McpError` → 原样 `{code,message,data}`；抛其他错误 → `-32603` + 消息。
#[tokio::test]
async fn handler_errors_become_json_rpc_error_responses() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client.connect(transport).await.expect("connect");
    let _boom = client.set_request_handler(
        "boom",
        Arc::new(|_params, _context| {
            Box::pin(async {
                Err(McpTransportError::from(McpError::with_data(
                    42,
                    "kaboom",
                    json!({"d": 1}),
                )))
            })
        }),
    );
    let _plain = client.set_request_handler(
        "plain",
        Arc::new(|_params, _context| {
            Box::pin(async { Err(McpTransportError::new("plain failure")) })
        }),
    );
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "boom"}))
        .expect("boom");
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": 2, "method": "plain"}))
        .expect("plain");
    server.settle().await;
    let messages = server.messages();
    assert!(
        messages.contains(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": 42, "message": "kaboom", "data": {"d": 1}},
        })),
        "messages: {messages:#?}"
    );
    assert!(
        messages.contains(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {"code": -32603, "message": "plain failure"},
        })),
        "messages: {messages:#?}"
    );
    client.close().await.expect("close");
}

/// 预置的 `ping` handler 答 `{}`；通知 listener 抛出的错误交给 `onError`。
#[tokio::test]
async fn ping_handler_and_listener_errors_match_typescript() {
    let (transport, server) = create_server().await;
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let _error_subscription = client.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.to_string());
        Ok(())
    }));
    client.connect(transport).await.expect("connect");
    let _notification = client.on_notification(
        "notifications/x",
        Arc::new(|_params| Err(McpTransportError::new("listener boom"))),
    );
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "ping-id", "method": "ping"}))
        .expect("ping");
    server.settle().await;
    server
        .transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "method": "notifications/x"}))
        .expect("notification");
    assert!(
        server.messages().contains(&json!({
            "jsonrpc": "2.0", "id": "ping-id", "result": {},
        })),
        "messages: {:#?}",
        server.messages()
    );
    assert_eq!(*errors.lock().unwrap(), vec!["listener boom".to_owned()]);
    client.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// 评审修复的回归测试（每条对应一个在 TS 参考实现上实测过的行为）
// ---------------------------------------------------------------------------

type ReplyHook = Arc<dyn Fn(&Value) -> Option<Result<Value, Value>> + Send + Sync>;
type SendFilter = Arc<dyn Fn(&Value) -> Result<(), McpTransportError> + Send + Sync>;

/// 可编程传输：记录出站消息，`reply` 为请求生成应答（异步送回），`send_filter` 可拒绝特定出站
/// 消息；`closed` 记录 `close()` 是否被执行（finding 5 需要断言它**没**被执行）。
struct ProgrammableTransport {
    events: Arc<TransportEvents>,
    messages: Arc<Mutex<Vec<Value>>>,
    reply: Mutex<Option<ReplyHook>>,
    send_filter: Mutex<Option<SendFilter>>,
    closed: Arc<Mutex<bool>>,
}

impl ProgrammableTransport {
    fn new() -> Arc<Self> {
        Arc::new(ProgrammableTransport {
            events: Arc::new(TransportEvents::new()),
            messages: Arc::new(Mutex::new(Vec::new())),
            reply: Mutex::new(None),
            send_filter: Mutex::new(None),
            closed: Arc::new(Mutex::new(false)),
        })
    }

    fn set_reply<F>(&self, reply: F)
    where
        F: Fn(&Value) -> Option<Result<Value, Value>> + Send + Sync + 'static,
    {
        *self.reply.lock().unwrap() = Some(Arc::new(reply));
    }

    fn set_send_filter<F>(&self, filter: F)
    where
        F: Fn(&Value) -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        *self.send_filter.lock().unwrap() = Some(Arc::new(filter));
    }

    fn messages(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }

    fn is_closed(&self) -> bool {
        *self.closed.lock().unwrap()
    }
}

#[async_trait]
impl McpTransport for ProgrammableTransport {
    async fn start(&self) -> Result<(), McpTransportError> {
        Ok(())
    }

    async fn send(
        &self,
        message: Value,
        _options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        if let Some(filter) = self.send_filter.lock().unwrap().clone() {
            filter(&message)?;
        }
        self.messages.lock().unwrap().push(message.clone());
        let reply = self.reply.lock().unwrap().clone();
        if let (Some(reply), Some(id)) = (reply, message.get("id").cloned()) {
            let method = message["method"].as_str().unwrap_or_default().to_owned();
            if let Some(outcome) = reply(&message) {
                let events = Arc::clone(&self.events);
                tokio::spawn(async move {
                    let mut response = Map::new();
                    response.insert("jsonrpc".to_owned(), json!("2.0"));
                    response.insert("id".to_owned(), id);
                    match outcome {
                        Ok(result) => {
                            response.insert("result".to_owned(), result);
                        }
                        Err(error) => {
                            response.insert("error".to_owned(), error);
                        }
                    }
                    let _ = method;
                    let _ = events.emit_message(&Value::Object(response));
                });
            }
        }
        Ok(())
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        *self.closed.lock().unwrap() = true;
        let _ = self.events.emit_close();
        Ok(())
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.events.on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.events.on_close(move || listener())
    }
}

/// 已连接的客户端 + 会应答 `initialize` 的可编程传输。
async fn connect_programmable() -> (McpClient, Arc<ProgrammableTransport>) {
    let transport = ProgrammableTransport::new();
    transport.set_reply(|message| {
        (message.get("method").and_then(Value::as_str) == Some("initialize")).then(|| {
            Ok(json!({
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "serverInfo": {"name": "s", "version": "1"},
            }))
        })
    });
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    client
        .connect(Arc::clone(&transport) as Arc<dyn McpTransport>)
        .await
        .expect("connect");
    (client, transport)
}

async fn settle() {
    for _ in 0..256 {
        tokio::task::yield_now().await;
    }
}

/// finding 1：成功响应**发送失败**时，TS 落入同一个 catch，再发一条 `-32603` 错误响应，
/// 且**不**调用 `emitError`（参考实现实测：服务端收到
/// `{"id":"srv","error":{"code":-32603,"message":"response send failed"}}`，onError 为空）。
#[tokio::test]
async fn success_response_send_failure_sends_an_internal_error_without_emitting() {
    let (client, transport) = connect_programmable().await;
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let _error_subscription = client.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.to_string());
        Ok(())
    }));
    transport.set_send_filter(|message| {
        if message.get("id") == Some(&json!("srv")) && message.get("result").is_some() {
            return Err(McpTransportError::new("response send failed"));
        }
        Ok(())
    });
    let _handler = client.set_request_handler(
        "echo",
        Arc::new(|_params, _context| Box::pin(async { Ok(json!({"ok": true})) })),
    );
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "srv", "method": "echo"}))
        .expect("server request");
    settle().await;
    let sent: Vec<Value> = transport
        .messages()
        .into_iter()
        .filter(|message| message.get("id") == Some(&json!("srv")))
        .collect();
    assert_eq!(
        sent,
        vec![json!({
            "jsonrpc": "2.0",
            "id": "srv",
            "error": {"code": -32603, "message": "response send failed"},
        })],
        "messages: {sent:#?}"
    );
    assert_eq!(*errors.lock().unwrap(), Vec::<String>::new());
    let _ = client.close().await;
}

/// finding 2：`result ?? {}` 是 nullish 合并，`null` 也变成 `{}`（`0` 原样保留）。
#[tokio::test]
async fn handler_returning_null_is_serialized_as_an_empty_object() {
    let (client, transport) = connect_programmable().await;
    let _null = client.set_request_handler(
        "nul",
        Arc::new(|_params, _context| Box::pin(async { Ok(Value::Null) })),
    );
    let _zero = client.set_request_handler(
        "zero",
        Arc::new(|_params, _context| Box::pin(async { Ok(json!(0)) })),
    );
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "a", "method": "nul"}))
        .expect("server request");
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "b", "method": "zero"}))
        .expect("server request");
    settle().await;
    let sent: Vec<Value> = transport
        .messages()
        .into_iter()
        .filter(|message| {
            message.get("id") == Some(&json!("a")) || message.get("id") == Some(&json!("b"))
        })
        .collect();
    assert_eq!(
        sent,
        vec![
            // 固定键序：jsonrpc、id、result（线上契约，改动需同步测试）。
            json!({"jsonrpc": "2.0", "id": "a", "result": {}}),
            json!({"jsonrpc": "2.0", "id": "b", "result": 0}),
        ]
    );
    let _ = client.close().await;
}

/// finding 3：`notifications/cancelled` 发送失败 → `emitError`（§13.7；参考实现实测
/// onError 收到 `"send blew up"`）。
#[tokio::test]
async fn cancelled_notification_send_failures_are_reported_to_error_listeners() {
    let (client, transport) = connect_programmable().await;
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let _error_subscription = client.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.to_string());
        Ok(())
    }));
    transport.set_send_filter(|message| {
        if message.get("method") == Some(&json!("notifications/cancelled")) {
            return Err(McpTransportError::new("send blew up"));
        }
        Ok(())
    });
    let signal = tokio_util::sync::CancellationToken::new();
    let call = {
        let client = client.clone();
        let options = McpRequestOptions::new().with_signal(signal.clone());
        tokio::spawn(async move { client.request("slow", None, &options).await })
    };
    tokio::task::yield_now().await;
    signal.cancel();
    let error = call.await.expect("task").expect_err("aborted");
    assert!(error.is_abort(), "expected AbortError, got {error:?}");
    settle().await;
    assert_eq!(*errors.lock().unwrap(), vec!["send blew up".to_owned()]);
    let _ = client.close().await;
}

/// finding 4：调用方 drop 掉请求的 future 之后，超时仍然触发并发送
/// `notifications/cancelled`（TS 的 Promise 独立于 await 方；参考实现实测
/// `{"requestId":2,"reason":"Request timed out"}`）。
#[tokio::test(start_paused = true)]
async fn dropping_the_request_future_still_times_out_and_notifies_the_server() {
    let (client, transport) = connect_programmable().await;
    let options = McpRequestOptions::new().with_timeout_ms(50.0);
    let pending = client.request("slow", None, &options);
    // 让 future 至少被 poll 一次（注册 pending / 定时器 / send），随后丢弃它。
    let abandoned = tokio::time::timeout(Duration::ZERO, pending).await;
    assert!(
        abandoned.is_err(),
        "the request must not settle immediately"
    );
    tokio::time::advance(Duration::from_millis(120)).await;
    settle().await;
    let cancelled: Vec<Value> = transport
        .messages()
        .into_iter()
        .filter(|message| message.get("method") == Some(&json!("notifications/cancelled")))
        .collect();
    assert_eq!(
        cancelled,
        vec![json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 2, "reason": "Request timed out"},
        })],
        "messages: {:#?}",
        transport.messages()
    );
    let _ = client.close().await;
}

/// finding 5：close 期间抛异常的 error listener 中断 close listener 循环，并让 `close()`
/// 在 `transport.close()` 之前返回错误（参考实现实测：closeErr = "error listener boom"，
/// transportClosed = false）。
#[tokio::test]
async fn throwing_error_listeners_during_close_abort_the_close_and_skip_the_transport() {
    let (client, transport) = connect_programmable().await;
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _error = client.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(format!("err:{error}"));
        Err(McpTransportError::new("error listener boom"))
    }));
    let sink = Arc::clone(&events);
    let _close_one = client.on_close(Arc::new(move || {
        sink.lock().unwrap().push("close1".to_owned());
        Err(McpTransportError::new("close listener boom"))
    }));
    let sink = Arc::clone(&events);
    let _close_two = client.on_close(Arc::new(move || {
        sink.lock().unwrap().push("close2".to_owned());
        Ok(())
    }));
    let error = client.close().await.expect_err("close must reject");
    assert_eq!(error.to_string(), "error listener boom");
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            "close1".to_owned(),
            "err:close listener boom".to_owned(),
            // close2 不会被调用：emitError 的异常中断了 close 循环。
        ]
    );
    assert!(
        !transport.is_closed(),
        "transport.close() must not run when markClosed throws"
    );
}

/// finding 6：派发期间新增的 notification / error listener 也会被调用（V8 Set 迭代；
/// 参考实现实测 `['first','late']`）。
#[tokio::test]
async fn listeners_added_during_dispatch_are_invoked() {
    let (client, transport) = connect_programmable().await;
    let calls: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    let inner_client = client.clone();
    let _note = client.on_notification(
        "note",
        Arc::new(move |_params| {
            sink.lock().unwrap().push("first");
            let sink = Arc::clone(&sink);
            inner_client.on_notification(
                "note",
                Arc::new(move |_params| {
                    sink.lock().unwrap().push("late");
                    Ok(())
                }),
            );
            Ok(())
        }),
    );
    let sink = Arc::clone(&calls);
    let inner_client = client.clone();
    let _error = client.on_error(Arc::new(move |_error| {
        sink.lock().unwrap().push("err-first");
        let sink = Arc::clone(&sink);
        inner_client.on_error(Arc::new(move |_error| {
            sink.lock().unwrap().push("err-late");
            Ok(())
        }));
        Ok(())
    }));
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "method": "note", "params": {}}))
        .expect("notification");
    // 未知响应 → `emitError`（"Received response for unknown MCP request 999"）。
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": 999, "result": {}}))
        .expect("unknown response");
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["first", "late", "err-first", "err-late"]
    );
    let _ = client.close().await;
}

/// finding 7：`onProgress` 收到的是**原始参数**（未知键、类型不符的 total、原始键序）。
#[tokio::test]
async fn progress_callbacks_receive_the_raw_params_object() {
    let (client, transport) = connect_programmable().await;
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let callback: ProgressCallback = Arc::new(move |params| {
        sink.lock().unwrap().push(params.clone());
        Ok(())
    });
    let call = {
        let client = client.clone();
        let options = McpRequestOptions::new().with_on_progress(callback);
        tokio::spawn(async move { client.request("slow", None, &options).await })
    };
    tokio::task::yield_now().await;
    transport
        .events
        .emit_message(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {
                "progressToken": 2,
                "progress": 1,
                "total": "many",
                "message": 5,
                "extra": [1],
            },
        }))
        .expect("progress");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![json!({
            "progressToken": 2,
            "progress": 1,
            "total": "many",
            "message": 5,
            "extra": [1],
        })]
    );
    drop(call);
    let _ = client.close().await;
}

/// finding 8：陈旧 disposer 把已重建的 method 集合清空（TS 的闭包行为；参考实现实测 []）。
#[tokio::test]
async fn stale_notification_disposers_clear_rebuilt_listener_sets() {
    let (client, transport) = connect_programmable().await;
    let calls: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    let first = client.on_notification(
        "x",
        Arc::new(move |_| {
            sink.lock().unwrap().push("a");
            Ok(())
        }),
    );
    first.unsubscribe();
    // 集合被删空后重建 → 新的 Set。
    let sink = Arc::clone(&calls);
    let _second = client.on_notification(
        "x",
        Arc::new(move |_| {
            sink.lock().unwrap().push("b");
            Ok(())
        }),
    );
    // 陈旧 disposer 仍然会删除 method 键 → 新监听器被一并清掉。
    first.unsubscribe();
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "method": "x"}))
        .expect("notification");
    assert_eq!(*calls.lock().unwrap(), Vec::<&'static str>::new());
    let _ = client.close().await;
}

/// finding 9：`notifications/cancelled` 的 reason：缺省是默认 AbortError，且只有第一次生效
/// （参考实现实测：首次取消后 `signal.reason` 的 `name`/`message` 是 AbortError /
/// "This operation was aborted"，第二次取消不再改动）。
#[tokio::test]
async fn cancelled_reasons_default_to_abort_error_and_first_cancel_wins() {
    use kiana_mcp::client::RequestContext;
    let (client, transport) = connect_programmable().await;
    let reasons: Arc<Mutex<Vec<Option<Value>>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(tokio::sync::Notify::new());
    let sink = Arc::clone(&reasons);
    let handler_gate = Arc::clone(&gate);
    let _handler = client.set_request_handler(
        "wait",
        Arc::new(move |_params, context: RequestContext| {
            let sink = Arc::clone(&sink);
            let gate = Arc::clone(&handler_gate);
            Box::pin(async move {
                sink.lock().unwrap().push(context.reason());
                gate.notified().await;
                sink.lock().unwrap().push(context.reason());
                Ok(json!({}))
            })
        }),
    );
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": 7, "method": "wait"}))
        .expect("server request");
    tokio::task::yield_now().await;
    // 没有 reason 字段 → `abort(undefined)` → 默认 DOMException。
    transport
        .events
        .emit_message(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 7},
        }))
        .expect("cancel 1");
    // 第二次取消是 no-op：reason 保持第一次的值。
    transport
        .events
        .emit_message(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 7, "reason": "two"},
        }))
        .expect("cancel 2");
    gate.notify_one();
    settle().await;
    let observed = reasons.lock().unwrap().clone();
    assert_eq!(observed.len(), 2, "reasons: {observed:#?}");
    assert_eq!(observed[0], None);
    assert_eq!(
        observed[1],
        Some(json!({"name": "AbortError", "message": "This operation was aborted"}))
    );
    let _ = client.close().await;
}

/// finding 12：失败的 roots 提供者 → `-32603` 错误响应（消息取自错误本身；参考实现实测
/// `{"code":-32603,"message":"roots failed"}`）。
#[tokio::test]
async fn failing_roots_providers_produce_internal_error_responses() {
    let transport = ProgrammableTransport::new();
    transport.set_reply(|message| {
        (message.get("method").and_then(Value::as_str) == Some("initialize")).then(|| {
            Ok(json!({
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "serverInfo": {"name": "s", "version": "1"},
            }))
        })
    });
    let client = McpClient::new(McpClientOptions::new("c", "1").with_roots(
        kiana_mcp::client::Roots::fallible_provider(|| {
            Box::pin(async { Err(McpTransportError::new("roots failed")) })
        }),
    ));
    client
        .connect(Arc::clone(&transport) as Arc<dyn McpTransport>)
        .await
        .expect("connect");
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "roots", "method": "roots/list"}))
        .expect("roots request");
    settle().await;
    let reply = transport
        .messages()
        .into_iter()
        .find(|message| message.get("id") == Some(&json!("roots")))
        .expect("roots reply");
    assert_eq!(
        reply,
        json!({
            "jsonrpc": "2.0",
            "id": "roots",
            "error": {"code": -32603, "message": "roots failed"},
        })
    );
    let _ = client.close().await;
}

/// finding 10（部分修复）：`Resource`/`CallToolResult` 的显式 `null` 可选字段不再丢失
/// （TS 的 `{...item}` 会保留；键序仍是类型化结构体的声明序，见模块文档的已知偏差）。
#[tokio::test]
async fn explicit_null_optional_fields_survive_typed_results() {
    let (client, transport) = connect_programmable().await;
    transport.set_reply(|message| match message["method"].as_str() {
        Some("initialize") => Some(Ok(json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {},
            "serverInfo": {"name": "s", "version": "1"},
        }))),
        Some("resources/list") => Some(Ok(json!({
            "resources": [{"uri": "file:///b", "mimeType": "text/plain", "title": null, "zzz": 1}],
        }))),
        Some("tools/call") => Some(Ok(json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": null,
            "_meta": null,
            "custom": null,
        }))),
        _ => None,
    });
    let resources = client
        .list_resources(&McpRequestOptions::new())
        .await
        .expect("list resources");
    let resource = serde_json::to_value(&resources[0]).expect("json");
    // `name` 回退成 `uri`；`title: null` 与未知键 `zzz` 都保留。
    assert_eq!(resource["name"], json!("file:///b"));
    assert_eq!(resource["title"], Value::Null);
    assert_eq!(resource["mimeType"], json!("text/plain"));
    assert_eq!(resource["zzz"], json!(1));

    let result = client
        .call_tool("t", None, &McpRequestOptions::new())
        .await
        .expect("call tool");
    let value = serde_json::to_value(&result).expect("json");
    assert!(value.get("isError").is_some(), "isError: {value}");
    assert_eq!(value["isError"], Value::Null);
    assert_eq!(value["_meta"], Value::Null);
    assert_eq!(value["custom"], Value::Null);
    assert_eq!(value["content"], json!([{"type": "text", "text": "ok"}]));
    let _ = client.close().await;
}

/// finding 13（部分修复）：`capabilities.extra` 里的 `"roots"`（显式值，含 `null`）不被 `{}` 覆盖。
#[tokio::test]
async fn explicit_capability_roots_values_are_not_overridden() {
    let mut capabilities = kiana_mcp::protocol_types::ClientCapabilities::default();
    capabilities.extra.insert("roots".to_owned(), Value::Null);
    let transport = ProgrammableTransport::new();
    transport.set_reply(|message| {
        (message["method"].as_str() == Some("initialize")).then(|| {
            Ok(json!({
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "serverInfo": {"name": "s", "version": "1"},
            }))
        })
    });
    let client = McpClient::new(
        McpClientOptions::new("c", "1")
            .with_roots(kiana_mcp::client::Roots::list(Vec::new()))
            .with_capabilities(capabilities),
    );
    client
        .connect(Arc::clone(&transport) as Arc<dyn McpTransport>)
        .await
        .expect("connect");
    let initialize = transport
        .messages()
        .into_iter()
        .find(|message| message.get("method") == Some(&json!("initialize")))
        .expect("initialize");
    assert_eq!(
        initialize["params"]["capabilities"],
        json!({"roots": null}),
        "initialize: {initialize}"
    );
    let _ = client.close().await;
}

/// finding 5（传输掉落路径）：`handleTransportClose` 没有 try/catch，error listener 的异常
/// 会从 `emitClose()` 抛给调用 `transport.close()` 的一方（TS 实测：`server.close()` reject）。
#[tokio::test]
async fn throwing_error_listeners_escape_the_transport_close_dispatch() {
    let (client, transport) = connect_programmable().await;
    let _error = client.on_error(Arc::new(|_error| {
        Err(McpTransportError::new("error listener boom"))
    }));
    // `markClosed` 只有在 close listener 抛错时才会走 `emitError`，所以两层都要。
    let _close = client.on_close(Arc::new(|| {
        Err(McpTransportError::new("close listener boom"))
    }));
    // `ProgrammableTransport::close` 里 `let _ = self.events.emit_close();` 会把异常吞掉，
    // 因此这里直接调用 `emit_close` 观察返回值。
    let error = transport
        .events
        .emit_close()
        .expect_err("emit_close must report the listener error");
    assert_eq!(error.to_string(), "error listener boom");
    let _ = client.close().await;
}

// ---------------------------------------------------------------------------
// 评审 finding 的回归测试
// ---------------------------------------------------------------------------

/// finding 1：服务端请求**成功**完成后，`incoming` 条目必须删除（TS 的 `finally`）。
///
/// 否则之后到达的 `notifications/cancelled` 会取消一个已经结束的 controller，handler 保留的
/// `context.signal` 会变成已取消。参考实现实测（`probe3.mts`）：
/// `after late cancel, aborted: false` / `reason: undefined`。
#[tokio::test]
async fn completed_server_requests_are_removed_from_incoming() {
    let (client, transport) = connect_programmable().await;
    let saved: Arc<Mutex<Option<kiana_mcp::client::RequestContext>>> = Arc::new(Mutex::new(None));
    let store = Arc::clone(&saved);
    let _handler = client.set_request_handler(
        "keep",
        Arc::new(move |_params, context: kiana_mcp::client::RequestContext| {
            *store.lock().unwrap() = Some(context);
            Box::pin(async { Ok(json!({"ok": true})) })
        }),
    );
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": "r1", "method": "keep"}))
        .expect("server request");
    settle().await;
    assert!(
        transport
            .messages()
            .iter()
            .any(|message| message.get("id") == Some(&json!("r1"))
                && message.get("result") == Some(&json!({"ok": true}))),
        "messages: {:#?}",
        transport.messages()
    );
    // 迟到的取消：TS 此时 `incoming` 已空，signal 不会被取消。
    transport
        .events
        .emit_message(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": "r1", "reason": "late"},
        }))
        .expect("late cancel");
    settle().await;
    let context = saved.lock().unwrap().clone().expect("handler ran");
    assert!(
        !context.signal().is_cancelled(),
        "a completed server request must not be cancelled afterwards"
    );
    assert_eq!(context.reason(), None);
    let _ = client.close().await;
}

/// finding 2：请求在**调用时**发出。调用方在同一 tick 里取消信号，TS 仍然发送请求与
/// `notifications/cancelled`（`probe6.mts`：`tools/call` id 2 + cancelled reason "stop"）。
#[tokio::test]
async fn requests_are_sent_before_the_first_poll() {
    let (client, transport) = connect_programmable().await;
    let signal = tokio_util::sync::CancellationToken::new();
    let options = McpRequestOptions::new().with_signal_reason(signal.clone(), json!("stop"));
    // 不 spawn、不 await：与 TS 的 `const pending = client.callTool(...)` 相同。
    // id 分配、pending 注册与定时器在**调用时**同步完成（TS 的同步前缀）；`transport.send`
    // 本体是 async fn，要到 driver task 的第一次 poll 才真正入队。
    let pending = client.call_tool("wait", None, &options);
    // 同步证据：下一个请求拿到 id 3，说明 `tools/call` 已经占用了 id 2（修复前这里是 2，
    // 因为整个请求体在首次 poll 之前都没运行）。
    let follow_up = client.ping(&McpRequestOptions::new());
    drop(follow_up);
    assert!(
        transport
            .messages()
            .iter()
            .all(|message| { message.get("method") != Some(&json!("ping")) }),
        "ping must not be sent before the runtime polls: {:#?}",
        transport.messages()
    );
    signal.cancel();
    let error = pending.await.expect_err("aborted");
    assert!(error.is_abort(), "expected AbortError, got {error:?}");
    settle().await;
    assert!(
        transport.messages().iter().any(|message| message
            == &json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 2, "reason": "stop"},
            })),
        "messages: {:#?}",
        transport.messages()
    );
    // 请求本身也被发出（TS 在同一 tick 里先 `transport.send`，abort 只是后续通知）。
    assert!(
        transport
            .messages()
            .iter()
            .any(|message| message.get("id") == Some(&json!(2))
                && message.get("method") == Some(&json!("tools/call"))),
        "messages: {:#?}",
        transport.messages()
    );
    let _ = client.close().await;
}

/// finding 4：`params` 不是对象时，JS 的对象展开仍然有定义（字符串/数组按索引展开）。
/// 参考实现实测（`probe7.mts`）：
/// `"ab"` → `{"0":"a","1":"b","_meta":{"progressToken":2}}`、
/// `[7,8]` → `{"0":7,"1":8,…}`、`5` → `{"_meta":…}`。
#[tokio::test]
async fn non_object_params_are_spread_like_javascript() {
    let (client, transport) = connect_programmable().await;
    transport.set_reply(|message| {
        message
            .get("method")
            .and_then(Value::as_str)
            .filter(|method| *method != "initialize")
            .map(|_| Ok(json!({})))
    });
    let progress: ProgressCallback = Arc::new(|_params| Ok(()));
    let options = McpRequestOptions::new().with_on_progress(progress);
    let _ = client
        .request("probe/string", Some(json!("ab")), &options)
        .await;
    let _ = client
        .request("probe/array", Some(json!([7, 8])), &options)
        .await;
    let _ = client
        .request("probe/number", Some(json!(5)), &options)
        .await;
    let params: Vec<Value> = transport
        .messages()
        .into_iter()
        .filter(|message| message.get("method") != Some(&json!("initialize")))
        .filter_map(|message| message.get("params").cloned())
        .collect();
    assert_eq!(
        params,
        vec![
            json!({"0": "a", "1": "b", "_meta": {"progressToken": 2}}),
            json!({"0": 7, "1": 8, "_meta": {"progressToken": 3}}),
            json!({"_meta": {"progressToken": 4}}),
        ]
    );
    let _ = client.close().await;
}

/// finding 3：显式 `null` 在嵌套位置也必须保留（TS 原样返回服务器对象）。
/// 参考实现实测（`probe5.mts`）：`serverInfo.title: null`、`capabilities.tools: null`、
/// `Tool.title/_meta/...: null`、`contents[].mimeType/_meta: null`、`content[]._meta: null`。
#[tokio::test]
async fn nested_explicit_nulls_survive_typed_results() {
    let (client, transport) = connect_programmable().await;
    transport.set_reply(|message| match message["method"].as_str() {
        Some("initialize") => Some(Ok(json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {"tools": null, "logging": null, "zzz": null},
            "serverInfo": {"name": "s", "version": "1", "title": null, "qqq": null},
        }))),
        Some("tools/list") => Some(Ok(json!({
            "tools": [{
                "name": "t",
                "title": null,
                "description": null,
                "inputSchema": {"type": "object"},
                "outputSchema": null,
                "annotations": null,
                "execution": null,
                "_meta": null,
                "zzz": 1,
            }],
        }))),
        Some("resources/read") => Some(Ok(json!({
            "contents": [{"uri": "u", "text": "x", "mimeType": null, "_meta": null}],
        }))),
        Some("tools/call") => Some(Ok(json!({
            "content": [{"type": "text", "text": "ok", "annotations": null, "_meta": null, "zzz": 3}],
        }))),
        _ => None,
    });
    let info = serde_json::to_value(client.server_info().expect("serverInfo")).expect("json");
    assert_eq!(info["title"], Value::Null);
    assert_eq!(info["qqq"], Value::Null);
    let caps = serde_json::to_value(client.server_capabilities().expect("caps")).expect("json");
    assert_eq!(caps["tools"], Value::Null);
    assert_eq!(caps["logging"], Value::Null);
    assert_eq!(caps["zzz"], Value::Null);

    let tools = client
        .list_tools(&McpRequestOptions::new())
        .await
        .expect("tools");
    let tool = serde_json::to_value(&tools[0]).expect("json");
    for key in [
        "title",
        "description",
        "outputSchema",
        "annotations",
        "execution",
        "_meta",
    ] {
        assert_eq!(tool[key], Value::Null, "Tool.{key} must survive: {tool}");
    }
    assert_eq!(tool["zzz"], json!(1));

    let read = client
        .read_resource("u", &McpRequestOptions::new())
        .await
        .expect("read");
    let read = serde_json::to_value(&read).expect("json");
    assert_eq!(read["contents"][0]["mimeType"], Value::Null);
    assert_eq!(read["contents"][0]["_meta"], Value::Null);

    let call = client
        .call_tool("t", None, &McpRequestOptions::new())
        .await
        .expect("call");
    let call = serde_json::to_value(&call).expect("json");
    assert_eq!(call["content"][0]["annotations"], Value::Null);
    assert_eq!(call["content"][0]["_meta"], Value::Null);
    assert_eq!(call["content"][0]["zzz"], json!(3));
    let _ = client.close().await;
}

/// finding 3（续）：单页结果的 `nextCursor: null` 被丢弃是**正确**的（`validateListPage`
/// 把 `null`/`""` 当作分页结束），但页面元素里的显式 `null` 仍要保留；`annotations` 内层的
/// `null` 同样保留。参考实现实测（`probe_more.mts`）。
#[tokio::test]
async fn page_results_preserve_nested_nulls_and_drop_null_cursors() {
    let (client, transport) = connect_programmable().await;
    transport.set_reply(|message| match message["method"].as_str() {
        Some("initialize") => Some(Ok(json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {},
            "serverInfo": {"name": "s", "version": "1"},
        }))),
        Some("resources/templates/list") => Some(Ok(json!({
            "resourceTemplates": [{
                "uriTemplate": "t://x",
                "title": null,
                "description": null,
                "mimeType": null,
                "annotations": {"audience": null, "priority": null, "lastModified": null},
                "_meta": null,
            }],
            "nextCursor": null,
        }))),
        Some("resources/list") => Some(Ok(json!({
            "resources": [{
                "uri": "u",
                "annotations": {"audience": null, "priority": null},
                "title": null,
            }],
            "nextCursor": "",
        }))),
        _ => None,
    });
    let page = client
        .list_resource_templates_page(None, &McpRequestOptions::new())
        .await
        .expect("template page");
    assert_eq!(page.next_cursor, None, "null cursor ends the pagination");
    let template = serde_json::to_value(&page.resource_templates[0]).expect("json");
    for key in ["title", "description", "mimeType", "_meta"] {
        assert_eq!(template[key], Value::Null, "template.{key}: {template}");
    }
    assert_eq!(template["name"], json!("t://x"));
    assert_eq!(template["annotations"]["audience"], Value::Null);
    assert_eq!(template["annotations"]["priority"], Value::Null);
    assert_eq!(template["annotations"]["lastModified"], Value::Null);

    let page = client
        .list_resources_page(None, &McpRequestOptions::new())
        .await
        .expect("resource page");
    assert_eq!(page.next_cursor, None, "empty cursor ends the pagination");
    let resource = serde_json::to_value(&page.resources[0]).expect("json");
    assert_eq!(resource["title"], Value::Null);
    assert_eq!(resource["annotations"]["audience"], Value::Null);
    assert_eq!(resource["annotations"]["priority"], Value::Null);
    assert_eq!(resource["name"], json!("u"));
    let _ = client.close().await;
}

// ---------------------------------------------------------------------------
// 评审 finding 回归（第二轮）
// ---------------------------------------------------------------------------

/// 发送会一直挂起到测试放行的传输：`tools/call` 的 send 完成时才发出应答（迟到应答）。
struct SlowSendTransport {
    events: Arc<TransportEvents>,
    sent: Arc<Mutex<Vec<Value>>>,
    release: Arc<tokio::sync::Notify>,
    finished: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl McpTransport for SlowSendTransport {
    async fn start(&self) -> Result<(), McpTransportError> {
        Ok(())
    }

    async fn send(
        &self,
        message: Value,
        _options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.sent.lock().unwrap().push(message.clone());
        match method.as_deref() {
            Some("initialize") => {
                let _ = self.events.emit_message(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"].clone(),
                    "result": {
                        "protocolVersion": LATEST_PROTOCOL_VERSION,
                        "capabilities": {},
                        "serverInfo": {"name": "s", "version": "1"},
                    },
                }));
            }
            Some("tools/call") => {
                // 请求方早已超时（pending 已删除）：send 仍要跑完并发出答复。
                self.release.notified().await;
                self.finished.notify_one();
                let _ = self.events.emit_message(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"].clone(),
                    "result": {"content": []},
                }));
            }
            _ => {}
        }
        Ok(())
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        let _ = self.events.emit_close();
        Ok(())
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.events.on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.events.on_close(move || listener())
    }
}

/// 评审 finding 2（major）：请求结算（超时/取消/已答复/关闭）**不**撤销已经派发的
/// `transport.send`。TS 的 `transport.send(...).catch(...)` 让 promise 继续跑完；迟到的
/// application/json 应答仍被解析，并经 `handleMessage` 报成
/// `Received response for unknown MCP request <id>`（Node 26 实测，见
/// `scratch/gate__kiana-mcp/probe_client.mts`）。
///
/// 修复前 Rust 在结算时 drop 掉 send future（`PendingDriver` 的局部变量），
/// StreamableHttpTransport 里 reqwest 的交换随之中止：迟到的答复永远看不到。
#[tokio::test]
async fn a_settled_request_does_not_cancel_its_dispatched_send() {
    let transport = Arc::new(SlowSendTransport {
        events: Arc::new(TransportEvents::new()),
        sent: Arc::new(Mutex::new(Vec::new())),
        release: Arc::new(tokio::sync::Notify::new()),
        finished: Arc::new(tokio::sync::Notify::new()),
    });
    let client = McpClient::new(McpClientOptions::new("c", "1"));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let _subscription = client.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.to_string());
        Ok(())
    }));
    client
        .connect(Arc::clone(&transport) as Arc<dyn McpTransport>)
        .await
        .expect("connect");

    let error = client
        .call_tool(
            "slow",
            None,
            &McpRequestOptions::new().with_timeout_ms(10.0),
        )
        .await
        .expect_err("timed out");
    assert_eq!(error.name(), "McpTimeoutError");

    // 放行 send。修复前 send future 已随超时被 drop，这个通知永远不会到。
    transport.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), transport.finished.notified())
        .await
        .expect("the dispatched send must still be running after the timeout");

    for _ in 0..256 {
        if errors
            .lock()
            .unwrap()
            .iter()
            .any(|error| error == "Received response for unknown MCP request 2")
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        *errors.lock().unwrap(),
        vec!["Received response for unknown MCP request 2".to_owned()]
    );
    let _ = client.close().await;
}

/// 评审 finding 6（minor）：`close()`/传输关闭**不**覆盖服务端已经给出的取消原因。
/// TS 对每个 incoming 控制器调用 `controller.abort(error)`，而 `AbortController.abort` 在
/// 已取消的控制器上是 no-op，`signal.reason` 保持第一次的值（Node 26 实测）。
#[tokio::test]
async fn close_does_not_overwrite_the_first_cancel_reason() {
    use kiana_mcp::client::RequestContext;
    let (client, transport) = connect_programmable().await;
    let reasons: Arc<Mutex<Vec<Option<Value>>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(tokio::sync::Notify::new());
    let sink = Arc::clone(&reasons);
    let handler_gate = Arc::clone(&gate);
    let _handler = client.set_request_handler(
        "wait",
        Arc::new(move |_params, context: RequestContext| {
            let sink = Arc::clone(&sink);
            let gate = Arc::clone(&handler_gate);
            Box::pin(async move {
                gate.notified().await;
                sink.lock().unwrap().push(context.reason());
                Ok(json!({}))
            })
        }),
    );
    transport
        .events
        .emit_message(&json!({"jsonrpc": "2.0", "id": 9, "method": "wait"}))
        .expect("server request");
    // `incoming` 在第一个 await 之前就登记好了（同步前奏），可直接取消。
    transport
        .events
        .emit_message(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 9, "reason": "stop"},
        }))
        .expect("cancel");
    client.close().await.expect("close");
    gate.notify_one();
    settle().await;
    assert_eq!(
        *reasons.lock().unwrap(),
        vec![Some(json!("stop"))],
        "close() must not overwrite the server-supplied reason"
    );
}

/// 评审 finding 3（minor）：自动补的 `capabilities.roots` 按 TS 的赋值语义
/// **追加在末尾**（`capabilities = {...options.capabilities}` 之后 `capabilities.roots = {}`）。
/// 调用方在 TS 对象字面量里的自定义键序无法在类型化结构体里表达（模块文档登记的偏差），
/// 但追加位置可以逐字复刻。
#[tokio::test]
async fn auto_added_roots_capability_is_appended_last() {
    let (transport, server) = create_server().await;
    let capabilities = kiana_mcp::protocol_types::ClientCapabilities {
        sampling: Some(Map::from_iter([("x".to_owned(), json!(1))])),
        ..kiana_mcp::protocol_types::ClientCapabilities::default()
    };
    let client = McpClient::new(
        McpClientOptions::new("c", "1")
            .with_roots(kiana_mcp::client::Roots::list(Vec::new()))
            .with_capabilities(capabilities),
    );
    client.connect(transport).await.expect("connect");
    let initialize = server
        .messages()
        .into_iter()
        .find(|message| message.get("method") == Some(&json!("initialize")))
        .expect("initialize");
    let keys: Vec<&str> = initialize["params"]["capabilities"]
        .as_object()
        .expect("capabilities object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["sampling", "roots"]);
    assert_eq!(
        initialize["params"]["capabilities"]["roots"],
        json!({}),
        "auto-added roots is still `{{}}`"
    );
    let _ = client.close().await;
}
