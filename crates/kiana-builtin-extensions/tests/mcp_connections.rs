//! `packages/coding-agent/test/mcp-extension.test.ts` 的 `MCP connections` 部分。
//!
//! TS 用 `createInMemoryTransportPair` 造假的 MCP 服务器，并按需替换 `transport.send`；
//! Rust 侧用 [`TestTransport`] 包装 in-memory 传输，`send` 钩子可以模拟 HTTP 错误、会话过期与
//! 认证失败。`~` 展开那条用真实 `node` 子进程（与 TS 相同）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kiana_builtin_extensions::mcp::config::McpServerEntry;
use kiana_builtin_extensions::mcp::log::McpServerLog;
use kiana_builtin_extensions::mcp::resources::McpResourceServer;
use kiana_builtin_extensions::mcp::runtime::{
    McpOAuthCredentialStore, McpServerConnection, McpServerConnectionOptions, McpServerStatusState,
    McpTransportFactory, ServerState, create_default_transport,
};
use kiana_builtin_extensions::mcp::tools::McpToolCaller;
use kiana_config::auth::storage::InMemoryAuthStorageBackend;
use kiana_core::mcp_servers::validate_mcp_server_config;
use kiana_mcp::auth_provider::{AuthProvider, McpTransportError};
use kiana_mcp::oauth::OAuthChallenge;
use kiana_mcp::testing::{InMemoryTransport, create_in_memory_transport_pair};
use kiana_mcp::transports::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, Subscription,
};
use kiana_mcp::{CallToolResult, LATEST_PROTOCOL_VERSION, McpRequestOptions};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// 建一个通过校验的条目（不启动任何东西）。
fn entry(name: &str, config: Value) -> McpServerEntry {
    McpServerEntry {
        name: name.to_string(),
        config: validate_mcp_server_config(name, &config).expect("config is valid"),
        source: "test".to_string(),
        scope: None,
        override_: None,
    }
}

fn credentials() -> Arc<McpOAuthCredentialStore> {
    Arc::new(McpOAuthCredentialStore::new(
        Some(InMemoryAuthStorageBackend::new().into()),
        None,
    ))
}

type SendHook = Arc<dyn Fn(&Value) -> Option<McpTransportError> + Send + Sync>;

/// 包装 in-memory 传输，并按需让 `send` 失败（TS 里直接改写 `transport.send`）。
#[derive(Clone)]
struct TestTransport {
    inner: InMemoryTransport,
    hook: Option<SendHook>,
}

#[async_trait]
impl McpTransport for TestTransport {
    async fn start(&self) -> Result<(), McpTransportError> {
        self.inner.start().await
    }

    async fn send(
        &self,
        message: Value,
        _options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        if let Some(hook) = &self.hook
            && let Some(error) = hook(&message)
        {
            return Err(error);
        }
        self.inner.send(message).await
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        self.inner.close().await
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.inner.on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.inner.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.inner.on_close(move || listener())
    }
}

fn test_transport(inner: InMemoryTransport, hook: Option<SendHook>) -> Arc<dyn McpTransport> {
    Arc::new(TestTransport { inner, hook })
}

#[derive(Default)]
struct FakeOptions {
    methods: Arc<Mutex<Vec<String>>>,
    no_tools: bool,
    /// 作为 `capabilities.resources` 发送的原始值；`None` 表示不发送该键
    /// （TS 测试里是 `...(options.resources ? { resources: {} } : {})`）。
    resources: Option<Value>,
    /// 作为 `capabilities.prompts` 发送的原始值；`Some` 时同时应答 prompts/list/get。
    prompts: Option<Value>,
}

/// 回应 initialize / tools/list / tools/call 的假服务器；返回客户端一侧的传输。
async fn fake_server(options: FakeOptions) -> (Arc<dyn McpTransport>, InMemoryTransport) {
    let pair = create_in_memory_transport_pair();
    let server = pair.server.clone();
    let methods = Arc::clone(&options.methods);
    let no_tools = options.no_tools;
    let resources = options.resources.clone();
    let prompts = options.prompts.clone();
    pair.server.on_message(move |message: &Value| {
        if message.get("id").is_none() || message.get("method").is_none() {
            return Ok(());
        }
        let method = message["method"].as_str().unwrap_or("").to_string();
        methods.lock().unwrap().push(method.clone());
        let id = message["id"].clone();
        let response = if method == "initialize" {
            let mut capabilities = serde_json::Map::new();
            capabilities.insert(
                "prompts".to_string(),
                prompts.clone().unwrap_or(Value::Null),
            );
            capabilities.insert(
                "tools".to_string(),
                if no_tools { json!(null) } else { json!({}) },
            );
            if let Some(resources) = &resources {
                capabilities.insert("resources".to_string(), resources.clone());
            }
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": Value::Object(capabilities),
                    "serverInfo": { "name": "fake", "version": "1.0.0" },
                },
            })
        } else if method == "tools/list" {
            if no_tools {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}})
            }
        } else if method == "resources/read" {
            json!({"jsonrpc": "2.0", "id": id, "result": {"contents": [{"uri": "docs://a", "text": "ok"}]}})
        } else if method == "prompts/list" {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "prompts": [
                        {
                            "name": "summarize",
                            "description": "Summarize a topic",
                            "arguments": [
                                { "name": "topic", "required": true },
                                { "name": "style", "required": false }
                            ]
                        },
                        { "name": "review" }
                    ]
                }
            })
        } else if method == "prompts/get" {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "messages": [
                        { "role": "user", "content": { "type": "text", "text": "summary body" } }
                    ]
                }
            })
        } else {
            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "ok"}]}})
        };
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.send(response).await;
        });
        Ok(())
    });
    pair.server.start().await.unwrap();
    (
        test_transport(pair.client.clone(), None),
        pair.server.clone(),
    )
}

#[derive(Default)]
struct ConnectOptions {
    log: Option<McpServerLog>,
    provider_token: Option<kiana_builtin_extensions::mcp::runtime::ProviderTokenFn>,
}

fn connect(
    entry: McpServerEntry,
    factories: Vec<Arc<dyn Fn() -> Arc<dyn McpTransport> + Send + Sync>>,
    options: ConnectOptions,
) -> (McpServerConnection, Arc<AtomicUsize>) {
    let opened = Arc::new(AtomicUsize::new(0));
    let opened_for_factory = Arc::clone(&opened);
    let factories = Arc::new(Mutex::new(VecDeque::from(factories)));
    let factory: McpTransportFactory = Arc::new(move |_, _, _| {
        let factory = factories
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .expect("a transport factory was configured");
        opened_for_factory.fetch_add(1, Ordering::SeqCst);
        factory()
    });
    let connection = McpServerConnection::new(McpServerConnectionOptions {
        entry,
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        create_transport: factory,
        credentials: credentials(),
        provider_token: options.provider_token,
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: options.log,
    });
    (connection, opened)
}

fn simple_factory(
    transport: Arc<dyn McpTransport>,
) -> Arc<dyn Fn() -> Arc<dyn McpTransport> + Send + Sync> {
    Arc::new(move || Arc::clone(&transport))
}

fn result_value(result: &CallToolResult) -> Value {
    serde_json::to_value(result).expect("CallToolResult serializes")
}

#[tokio::test]
async fn starts_a_new_session_and_retries_once_when_the_session_expired() {
    let first_pair = create_in_memory_transport_pair();
    let server = first_pair.server.clone();
    let server_for_handler = server.clone();
    let _ = server.on_message(move |message: &Value| {
        if message.get("id").is_none() || message.get("method").is_none() {
            return Ok(());
        }
        let method = message["method"].as_str().unwrap_or("");
        let id = message["id"].clone();
        let response = if method == "initialize" {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1.0.0"},
                },
            })
        } else if method == "tools/list" {
            json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "ok"}]}})
        };
        let server = server_for_handler.clone();
        tokio::spawn(async move {
            let _ = server.send(response).await;
        });
        Ok(())
    });
    first_pair.server.start().await.unwrap();
    // 第一个传输上的每次 tools/call 都模拟 HTTP 传输对未知会话的 404（TS 永久替换 `send`），
    // 两个并发调用都必须走会话过期重试。
    let hook_transport = TestTransport {
        inner: first_pair.client.clone(),
        hook: Some(Arc::new(|message: &Value| {
            (message.get("method").and_then(Value::as_str) == Some("tools/call"))
                .then(|| McpTransportError::session_expired("gone"))
        })),
    };
    let normal = fake_server(FakeOptions::default()).await;
    let (connection, opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![
            Arc::new(move || Arc::new(hook_transport.clone()) as Arc<dyn McpTransport>),
            simple_factory(normal.0),
        ],
        ConnectOptions::default(),
    );
    let request_options = McpRequestOptions::new();
    let (first, second) = futures::join!(
        connection.call_tool("echo", json!({}), &request_options),
        connection.call_tool("echo", json!({}), &request_options)
    );
    assert_eq!(
        result_value(&first.unwrap()),
        json!({"content": [{"type": "text", "text": "ok"}]})
    );
    assert_eq!(
        result_value(&second.unwrap()),
        json!({"content": [{"type": "text", "text": "ok"}]})
    );
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    connection.close().await;
}

#[tokio::test]
async fn connects_to_servers_without_the_tools_capability_without_listing_tools() {
    let methods = Arc::new(Mutex::new(Vec::new()));
    let (transport, _server) = fake_server(FakeOptions {
        no_tools: true,
        methods: Arc::clone(&methods),
        ..FakeOptions::default()
    })
    .await;
    let (connection, _opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![simple_factory(transport)],
        ConnectOptions::default(),
    );
    connection.get_client().await.unwrap();
    assert_eq!(connection.state(), ServerState::Connected);
    assert!(connection.tools().is_empty());
    assert_eq!(*methods.lock().unwrap(), vec!["initialize"]);
    connection.close().await;
}

#[tokio::test]
async fn marks_a_dropped_connection_and_reconnects_on_the_next_call() {
    let (first, server) = fake_server(FakeOptions::default()).await;
    let (second, _server2) = fake_server(FakeOptions::default()).await;
    let (connection, opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![simple_factory(first), simple_factory(second)],
        ConnectOptions::default(),
    );
    connection.get_client().await.unwrap();
    let _ = server.close().await;
    tokio::task::yield_now().await;
    assert_eq!(connection.state(), ServerState::Disconnected);
    assert_eq!(connection.error().as_deref(), Some("Connection closed"));
    let result = connection
        .call_tool("echo", json!({}), &McpRequestOptions::new())
        .await
        .unwrap();
    assert_eq!(
        result_value(&result),
        json!({"content": [{"type": "text", "text": "ok"}]})
    );
    assert_eq!(connection.state(), ServerState::Connected);
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    connection.close().await;
}

#[tokio::test]
async fn retries_http_connections_that_fail_with_a_transient_error() {
    let failing_transport = || {
        let pair = create_in_memory_transport_pair();
        tokio::spawn({
            let server = pair.server.clone();
            async move {
                let _ = server.start().await;
            }
        });
        test_transport(
            pair.client.clone(),
            Some(Arc::new(|_: &Value| {
                Some(McpTransportError::http(
                    503,
                    "MCP HTTP request failed with status 503",
                    "",
                ))
            })),
        )
    };
    let successful = fake_server(FakeOptions::default()).await;
    let (connection, opened) = connect(
        entry(
            "fake",
            json!({"url": "http://unused.invalid", "headers": {"Authorization": "x"}}),
        ),
        vec![
            {
                let transport = failing_transport();
                simple_factory(transport)
            },
            simple_factory(successful.0),
        ],
        ConnectOptions::default(),
    );
    connection.get_client().await.unwrap();
    assert_eq!(connection.state(), ServerState::Connected);
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    connection.close().await;

    let (failing, failing_opened) = connect(
        entry(
            "fake",
            json!({"url": "http://unused.invalid", "headers": {"Authorization": "x"}}),
        ),
        vec![Arc::new(|| {
            let pair = create_in_memory_transport_pair();
            tokio::spawn({
                let server = pair.server.clone();
                async move {
                    let _ = server.start().await;
                }
            });
            test_transport(
                pair.client.clone(),
                Some(Arc::new(|_: &Value| {
                    Some(McpTransportError::http(
                        400,
                        "MCP HTTP request failed with status 400: bad",
                        "",
                    ))
                })),
            )
        })
            as Arc<dyn Fn() -> Arc<dyn McpTransport> + Send + Sync>],
        ConnectOptions::default(),
    );
    let error = failing.get_client().await.unwrap_err();
    assert!(error.to_string().contains("status 400: bad"), "{error}");
    assert_eq!(failing.state(), ServerState::Failed);
    assert_eq!(failing_opened.load(Ordering::SeqCst), 1);
    failing.close().await;
}

/// 评审回归：TS `open()` 用 `"url" in config ? CONNECT_RETRY_DELAYS_MS : []`——stdio 变体
/// 带 `url` 键时同样重试。
#[tokio::test]
async fn retries_connections_when_a_stdio_config_still_has_a_url_key() {
    let failing_transport = || {
        let pair = create_in_memory_transport_pair();
        tokio::spawn({
            let server = pair.server.clone();
            async move {
                let _ = server.start().await;
            }
        });
        test_transport(
            pair.client.clone(),
            Some(Arc::new(|_: &Value| {
                Some(McpTransportError::network("boom"))
            })),
        )
    };
    let successful = fake_server(FakeOptions::default()).await;
    let (connection, opened) = connect(
        entry(
            "stray",
            json!({"type": "stdio", "command": "unused", "url": "https://unused.invalid"}),
        ),
        vec![
            simple_factory(failing_transport()),
            simple_factory(successful.0),
        ],
        ConnectOptions::default(),
    );
    connection.get_client().await.unwrap();
    assert_eq!(connection.state(), ServerState::Connected);
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    connection.close().await;

    // 没有 `url` 键的 stdio 条目只尝试一次。
    let successful = fake_server(FakeOptions::default()).await;
    let (connection, opened) = connect(
        entry("plain", json!({"command": "unused"})),
        vec![
            simple_factory(failing_transport()),
            simple_factory(successful.0),
        ],
        ConnectOptions::default(),
    );
    assert!(connection.get_client().await.is_err());
    assert_eq!(connection.state(), ServerState::Failed);
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    connection.close().await;
}

/// 评审回归：`{command, url: null}` 在 TS 里建 HTTP transport（`new URL(null)` →
/// `TypeError("Invalid URL")`），command 从不启动；服务器最终 `failed`。
#[tokio::test]
async fn null_url_fails_with_invalid_url_without_spawning_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let entry = entry(
        "nullurl",
        json!({
            "command": "sh",
            "args": ["-c", format!("touch '{}'", marker.display())],
            "url": null,
        }),
    );
    let connection = McpServerConnection::new(McpServerConnectionOptions {
        entry,
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        create_transport: Arc::new(create_default_transport),
        credentials: credentials(),
        provider_token: None,
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: None,
    });
    assert_eq!(connection.oauth_url(), None, "url null is falsy");
    let error = connection.get_client().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "MCP server \"nullurl\" failed to connect: Invalid URL"
    );
    assert_eq!(connection.state(), ServerState::Failed);
    assert!(
        !marker.exists(),
        "the stdio command must never be spawned when config has a url key"
    );
    connection.close().await;
}

#[tokio::test]
async fn closes_a_connection_that_is_still_initializing_before_close_returns() {
    let pair = create_in_memory_transport_pair();
    let (initialized_tx, initializing) = tokio::sync::oneshot::channel();
    let initialized = Arc::new(Mutex::new(Some(initialized_tx)));
    pair.server.on_message({
        let initialized = Arc::clone(&initialized);
        move |_message: &Value| {
            if let Some(sender) = initialized.lock().unwrap().take() {
                let _ = sender.send(());
            }
            Ok(())
        }
    });
    pair.server.start().await.unwrap();
    let transport_closed = Arc::new(AtomicBool::new(false));
    let closed_flag = Arc::clone(&transport_closed);
    let _subscription = pair.client.on_close(move || {
        closed_flag.store(true, Ordering::SeqCst);
        Ok(())
    });
    let transport = test_transport(pair.client.clone(), None);
    let (connection, _opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![simple_factory(transport)],
        ConnectOptions::default(),
    );
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_slot = Arc::clone(&failure);
    let handle = tokio::spawn({
        let connection = connection.clone();
        async move {
            if let Err(error) = connection.get_client().await {
                *failure_slot.lock().unwrap() = Some(error.to_string());
            }
        }
    });
    let _ = tokio::time::timeout(Duration::from_secs(5), initializing)
        .await
        .expect("initialize arrives");
    connection.close().await;
    assert!(transport_closed.load(Ordering::SeqCst));
    handle.await.unwrap();
    assert!(
        failure
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|failure| failure.contains("failed to connect")),
        "{:?}",
        failure.lock().unwrap()
    );
}

#[tokio::test]
async fn stops_waiting_to_retry_a_connection_when_closed() {
    let pair = create_in_memory_transport_pair();
    tokio::spawn({
        let server = pair.server.clone();
        async move {
            let _ = server.start().await;
        }
    });
    let transport = test_transport(
        pair.client.clone(),
        Some(Arc::new(|_: &Value| {
            Some(McpTransportError::http(
                503,
                "MCP HTTP request failed with status 503",
                "",
            ))
        })),
    );
    let (connection, _opened) = connect(
        entry(
            "fake",
            json!({"url": "http://unused.invalid", "headers": {"Authorization": "x"}}),
        ),
        vec![simple_factory(transport)],
        ConnectOptions::default(),
    );
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_slot = Arc::clone(&failure);
    let handle = tokio::spawn({
        let connection = connection.clone();
        async move {
            if let Err(error) = connection.get_client().await {
                *failure_slot.lock().unwrap() = Some(error.to_string());
            }
        }
    });
    // 第一次尝试已经失败；重试在 250ms 之后。
    tokio::time::sleep(Duration::from_millis(10)).await;
    connection.close().await;
    handle.await.unwrap();
    assert!(
        failure
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|failure| failure.contains("status 503")),
        "{:?}",
        failure.lock().unwrap()
    );
}

#[tokio::test]
async fn retries_resource_reads_but_not_tool_calls_after_a_transient_http_error() {
    let pair = create_in_memory_transport_pair();
    let server = pair.server.clone();
    pair.server.start().await.unwrap();
    let failed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let failed_hook = Arc::clone(&failed);
    pair.server.on_message(move |message: &Value| {
        if message.get("id").is_none() || message.get("method").is_none() {
            return Ok(());
        }
        let method = message["method"].as_str().unwrap_or("").to_string();
        let id = message["id"].clone();
        let response = if method == "initialize" {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1.0.0"},
                },
            })
        } else if method == "tools/list" {
            json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}})
        } else if method == "resources/read" {
            json!({"jsonrpc": "2.0", "id": id, "result": {"contents": [{"uri": "docs://a", "text": "ok"}]}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "ok"}]}})
        };
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.send(response).await;
        });
        Ok(())
    });
    let failed_transport = Arc::clone(&failed_hook);
    let transport = TestTransport {
        inner: pair.client.clone(),
        // 第一次 read 与第一次 call 都以 502 失败。
        hook: Some(Arc::new(move |message: &Value| {
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if method == "resources/read" || method == "tools/call" {
                let mut failed = failed_transport.lock().unwrap();
                if !failed.contains(&method) {
                    failed.push(method);
                    return Some(McpTransportError::http(
                        502,
                        "MCP HTTP request failed with status 502",
                        "",
                    ));
                }
            }
            None
        })),
    };
    let (connection, _opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![Arc::new(move || {
            Arc::new(transport.clone()) as Arc<dyn McpTransport>
        })],
        ConnectOptions::default(),
    );
    let result = connection
        .read_resource("docs://a", &McpRequestOptions::new())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"contents": [{"uri": "docs://a", "text": "ok"}]})
    );
    let error = connection
        .call_tool("echo", json!({}), &McpRequestOptions::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("status 502"), "{error}");
    connection.close().await;
}

/// 一个永远以 401 拒绝的传输。
fn auth_required_transport() -> Arc<dyn McpTransport> {
    let pair = create_in_memory_transport_pair();
    tokio::spawn({
        let server = pair.server.clone();
        async move {
            let _ = server.start().await;
        }
    });
    test_transport(
        pair.client.clone(),
        Some(Arc::new(|_: &Value| {
            Some(McpTransportError::auth_required(None, ""))
        })),
    )
}

#[tokio::test]
async fn asks_oauth_servers_that_keep_rejecting_requests_for_a_new_sign_in() {
    let (connection, _opened) = connect(
        entry("fake", json!({"url": "http://unused.invalid"})),
        vec![simple_factory(auth_required_transport())],
        ConnectOptions::default(),
    );
    let error = connection.get_client().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "MCP server \"fake\" requires sign-in. Run /mcp to sign in."
    );
    assert_eq!(connection.state(), ServerState::NeedsAuth);
    connection.close().await;
}

#[tokio::test]
async fn sends_the_provider_token_and_asks_for_the_provider_login_when_the_server_rejects_it() {
    let mock_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(401))
        .mount(&mock_server)
        .await;
    let entry = entry(
        "radius",
        json!({
            "url": format!("{}/mcp", mock_server.uri()),
            "auth": {"provider": "radius"},
        }),
    );
    let provider_token: kiana_builtin_extensions::mcp::runtime::ProviderTokenFn =
        Arc::new(|provider: String| {
            Box::pin(async move {
                if provider == "radius" {
                    Some("tok".to_string())
                } else {
                    None
                }
            })
        });
    let opened = Arc::new(AtomicUsize::new(0));
    let factory_opened = Arc::clone(&opened);
    let factory: McpTransportFactory = Arc::new(move |entry, cwd, auth_provider| {
        factory_opened.fetch_add(1, Ordering::SeqCst);
        create_default_transport(entry, cwd, auth_provider)
    });
    let connection = McpServerConnection::new(McpServerConnectionOptions {
        entry,
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        create_transport: factory,
        credentials: credentials(),
        provider_token: Some(provider_token),
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: None,
    });
    assert_eq!(connection.oauth_url(), None);
    let error = connection.get_client().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "MCP server \"radius\" requires sign-in. Run /login radius to sign in."
    );
    assert_eq!(connection.state(), ServerState::NeedsAuth);
    let requests = mock_server.received_requests().await.unwrap_or_default();
    let authorization = requests.first().and_then(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    });
    assert_eq!(authorization.as_deref(), Some("Bearer tok"));
    connection.close().await;
}

#[tokio::test]
async fn appends_server_log_messages_to_the_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.log");
    let (transport, server) = fake_server(FakeOptions::default()).await;
    let (connection, _opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![simple_factory(transport)],
        ConnectOptions {
            log: Some(McpServerLog::new(&path)),
            ..ConnectOptions::default()
        },
    );
    connection.get_client().await.unwrap();
    server
        .send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {"level": "warning", "logger": "db", "data": "slow\nquery"},
        }))
        .await
        .unwrap();
    server
        .send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {"level": "error", "data": {"code": 7}},
        }))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    let contents = std::fs::read_to_string(&path).unwrap();
    let timestamp = regex::Regex::new(r"(?m)^\S+ ").unwrap();
    let lines = timestamp.replace_all(&contents, "");
    assert_eq!(
        lines,
        "[fake] warning db: slow\n    query\n[fake] error {\"code\":7}\n"
    );
    connection.close().await;
}

#[tokio::test]
async fn resolves_the_oauth_client_secret_lazily() {
    let (transport, _server) = fake_server(FakeOptions::default()).await;
    let (connection, _opened) = connect(
        entry(
            "fake",
            json!({"url": "http://unused.invalid", "oauth": {"clientSecret": "!exit 1"}}),
        ),
        vec![simple_factory(transport)],
        ConnectOptions::default(),
    );
    let result = connection
        .call_tool("echo", json!({}), &McpRequestOptions::new())
        .await
        .unwrap();
    assert_eq!(
        result_value(&result),
        json!({"content": [{"type": "text", "text": "ok"}]})
    );
    let error = connection.oauth_settings().unwrap_err();
    assert!(error.contains("oauth.clientSecret"), "{error}");
    connection.close().await;
}

#[tokio::test]
async fn connects_to_servers_with_resources_without_listing_them() {
    let methods = Arc::new(Mutex::new(Vec::new()));
    let (transport, _server) = fake_server(FakeOptions {
        resources: Some(json!({})),
        methods: Arc::clone(&methods),
        ..FakeOptions::default()
    })
    .await;
    let (connection, _opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![simple_factory(transport)],
        ConnectOptions::default(),
    );
    connection.get_client().await.unwrap();
    assert_eq!(connection.state(), ServerState::Connected);
    assert!(connection.has_resources());
    assert_eq!(*methods.lock().unwrap(), vec!["initialize", "tools/list"]);
    connection.close().await;
}

/// TS: `this.hasResources = client.serverCapabilities?.resources !== undefined` —— 只看键
/// 是否存在，`null`/`false`/非对象值也算（宽松解析把它们留在 `extra` 里）。
#[tokio::test]
async fn treats_a_present_resources_capability_as_resources_even_when_not_an_object() {
    for value in [json!(null), json!(false), json!([]), json!("yes")] {
        let (transport, _server) = fake_server(FakeOptions {
            resources: Some(value.clone()),
            ..FakeOptions::default()
        })
        .await;
        let (connection, _opened) = connect(
            entry("fake", json!({"command": "unused"})),
            vec![simple_factory(transport)],
            ConnectOptions::default(),
        );
        connection.get_client().await.unwrap();
        assert!(
            connection.has_resources(),
            "capabilities.resources = {value} must count as resources"
        );
        connection.close().await;
    }

    let (transport, _server) = fake_server(FakeOptions::default()).await;
    let (connection, _opened) = connect(
        entry("fake", json!({"command": "unused"})),
        vec![simple_factory(transport)],
        ConnectOptions::default(),
    );
    connection.get_client().await.unwrap();
    assert!(!connection.has_resources());
    connection.close().await;
}

/// `roots` 来自构造函数收到的原始 cwd 字符串：`pathToFileURL(cwd).href` 与 `basename(cwd)`，
/// 不做词法规范化（`"."` 的 name 仍是 `"."`，尾部斜杠保留在 URI 里）。
#[tokio::test]
async fn sends_the_raw_working_directory_as_the_roots_payload() {
    async fn roots_for(cwd: PathBuf) -> Value {
        let pair = create_in_memory_transport_pair();
        let server = pair.server.clone();
        let seen: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let seen_for_handler = Arc::clone(&seen);
        pair.server.on_message(move |message: &Value| {
            let id = message.get("id").cloned();
            let method = message.get("method").and_then(Value::as_str);
            match (id, method) {
                (Some(id), Some("initialize")) => {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": LATEST_PROTOCOL_VERSION,
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "fake", "version": "1.0.0"},
                        },
                    });
                    let server = server.clone();
                    tokio::spawn(async move {
                        let _ = server.send(response).await;
                        // 客户端把 `roots` 作为 `roots/list` 的处理结果暴露出来。
                        let _ = server
                            .send(json!({
                                "jsonrpc": "2.0",
                                "id": "roots-request",
                                "method": "roots/list",
                            }))
                            .await;
                    });
                }
                (Some(id), Some("tools/list")) => {
                    let response = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}});
                    let server = server.clone();
                    tokio::spawn(async move {
                        let _ = server.send(response).await;
                    });
                }
                (Some(Value::String(id)), None) if id == "roots-request" => {
                    *seen_for_handler.lock().unwrap() = Some(message["result"]["roots"].clone());
                }
                _ => {}
            }
            Ok(())
        });
        pair.server.start().await.unwrap();
        let client = pair.client.clone();
        let factory: McpTransportFactory =
            Arc::new(move |_, _, _| Arc::new(client.clone()) as Arc<dyn McpTransport>);
        let connection = McpServerConnection::new(McpServerConnectionOptions {
            entry: entry("fake", json!({"command": "unused"})),
            cwd,
            create_transport: factory,
            credentials: credentials(),
            provider_token: None,
            on_tools: Arc::new(|_| {}),
            on_change: None,
            log: None,
        });
        connection.get_client().await.unwrap();
        for _ in 0..200 {
            if seen.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        connection.close().await;
        seen.lock()
            .unwrap()
            .clone()
            .expect("roots/list was answered")
    }

    let cwd = std::env::current_dir().unwrap();
    let expected_uri = url::Url::from_file_path(&cwd).unwrap().to_string();
    assert_eq!(
        roots_for(PathBuf::from(".")).await,
        json!([{"uri": expected_uri, "name": "."}])
    );

    #[cfg(not(windows))]
    {
        let roots = roots_for(PathBuf::from("/tmp/roots-probe/")).await;
        assert_eq!(
            roots,
            json!([{"uri": "file:///tmp/roots-probe/", "name": "roots-probe"}])
        );

        // Node `pathToFileURL("/tmp/roots[b]~probe/")` 也编码 `[ ] ~`（url crate 不编码）。
        let roots = roots_for(PathBuf::from("/tmp/roots[b]~probe/")).await;
        assert_eq!(
            roots,
            json!([{"uri": "file:///tmp/roots%5Bb%5D%7Eprobe/", "name": "roots[b]~probe"}])
        );
    }
}

/// `node` 是这些用例的硬性前提（与 TS 的 vitest 相同：缺少 node 时失败）。
fn require_node() {
    let available = std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    assert!(
        available,
        "this test spawns `node`; install node or run it in an environment that has it"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn expands_tilde_in_the_command_arguments_and_cwd_of_stdio_servers() {
    require_node();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("work")).unwrap();
    std::fs::write(
        home.path().join("server.mjs"),
        r#"import { createInterface } from "node:readline";
for await (const line of createInterface({ input: process.stdin })) {
	const message = JSON.parse(line);
	if (!("id" in message)) continue;
	const result = message.method === "initialize"
		? { protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "cwd", version: "1" } }
		: message.method === "tools/list"
			? { tools: [{ name: "cwd", inputSchema: { type: "object" } }] }
			: { content: [{ type: "text", text: process.cwd() }] };
	process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }) + "\n");
}
"#,
    )
    .unwrap();
    let previous_home = std::env::var_os("HOME");
    // SAFETY: 这是本测试二进制里唯一修改 HOME 的地方；其余用例不依赖它。
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    let entry = entry(
        "home",
        json!({"command": "node", "args": ["~/server.mjs"], "cwd": "~/work"}),
    );
    let opened = Arc::new(AtomicUsize::new(0));
    let _ = &opened;
    let factory: McpTransportFactory = Arc::new(create_default_transport);
    let connection = McpServerConnection::new(McpServerConnectionOptions {
        entry,
        cwd: std::env::temp_dir(),
        create_transport: factory,
        credentials: credentials(),
        provider_token: None,
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: None,
    });
    let result = connection
        .call_tool("cwd", json!({}), &McpRequestOptions::new())
        .await;
    let expected = std::fs::canonicalize(home.path().join("work")).unwrap();
    match result {
        Ok(result) => {
            let value = result_value(&result);
            let text = value["content"][0]["text"].as_str().unwrap_or("");
            assert_eq!(std::fs::canonicalize(text).unwrap(), expected);
        }
        Err(error) => panic!("cwd tool failed: {error}"),
    }
    connection.close().await;
    match previous_home {
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        None => unsafe { std::env::remove_var("HOME") },
    }
}

/// 服务器声明 `prompts` 后握手阶段会拉取 prompts/list，并出现在状态快照里；
/// `prompts/get` 的参数原样下发。
#[tokio::test]
async fn loads_prompts_and_reports_status() {
    let (transport, _server) = fake_server(FakeOptions {
        prompts: Some(json!({ "listChanged": true })),
        ..FakeOptions::default()
    })
    .await;
    let (connection, _opened) = connect(
        entry("docs", json!({"command": "unused"})),
        vec![simple_factory(transport)],
        ConnectOptions::default(),
    );
    connection.get_client().await.unwrap();
    assert_eq!(connection.state(), ServerState::Connected);

    let prompts = connection.prompts();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0].name, "summarize");
    assert_eq!(prompts[0].description.as_deref(), Some("Summarize a topic"));
    let arguments = prompts[0].arguments.as_ref().expect("arguments listed");
    assert_eq!(arguments.len(), 2);
    assert_eq!(arguments[0].name, "topic");
    assert_eq!(arguments[0].required, Some(true));
    assert_eq!(arguments[1].name, "style");
    assert_eq!(arguments[1].required, Some(false));
    assert_eq!(prompts[1].name, "review");
    assert!(prompts[1].arguments.is_none());

    let args = json!({ "topic": "MCP" })
        .as_object()
        .expect("object")
        .clone();
    let result = connection
        .get_prompt("summarize", Some(args))
        .await
        .expect("prompts/get succeeds");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].role.as_str(), "user");

    let status = connection.status(true);
    assert_eq!(status.state, McpServerStatusState::Connected);
    assert_eq!(status.tool_count, 0);
    assert_eq!(status.prompt_count, 2);
    assert!(status.last_error.is_none());

    // 禁用配置时状态是 disabled（即使连接仍然存在）。
    let disabled = connection.status(false);
    assert_eq!(disabled.state, McpServerStatusState::Disabled);
    assert_eq!(disabled.prompt_count, 2);
    connection.close().await;
}

/// 一个服务器握手失败时状态是 `failed`，`last_error` 带上原因；关闭后是 `disabled`。
#[tokio::test]
async fn reports_failed_status_with_last_error() {
    let factory: Arc<dyn Fn() -> Arc<dyn McpTransport> + Send + Sync> = Arc::new(|| {
        let pair = create_in_memory_transport_pair();
        tokio::spawn({
            let server = pair.server.clone();
            async move {
                let _ = server.start().await;
            }
        });
        test_transport(
            pair.client.clone(),
            Some(Arc::new(|_: &Value| {
                Some(McpTransportError::new("boom: handshake rejected"))
            })),
        )
    });
    let (connection, _opened) = connect(
        entry("broken", json!({"command": "unused"})),
        vec![factory],
        ConnectOptions::default(),
    );
    let error = connection.get_client().await.expect_err("handshake fails");
    assert!(
        error.to_string().contains("boom: handshake rejected"),
        "{error}"
    );

    let status = connection.status(true);
    assert_eq!(status.state, McpServerStatusState::Failed);
    assert_eq!(
        status.last_error.as_deref(),
        Some("boom: handshake rejected")
    );
    assert_eq!(status.tool_count, 0);
    assert_eq!(status.prompt_count, 0);

    connection.close().await;
    assert_eq!(
        connection.status(true).state,
        McpServerStatusState::Disabled
    );
}

/// 一个服务器启动失败不影响另一个服务器：连接对象彼此独立，健康服务器的工具照常可用。
#[tokio::test]
async fn one_failed_server_does_not_affect_another() {
    let (healthy_transport, _server) = fake_server(FakeOptions::default()).await;
    let (healthy, _) = connect(
        entry("healthy", json!({"command": "unused"})),
        vec![simple_factory(healthy_transport)],
        ConnectOptions::default(),
    );
    let failing_factory: Arc<dyn Fn() -> Arc<dyn McpTransport> + Send + Sync> = Arc::new(|| {
        let pair = create_in_memory_transport_pair();
        tokio::spawn({
            let server = pair.server.clone();
            async move {
                let _ = server.start().await;
            }
        });
        test_transport(
            pair.client.clone(),
            Some(Arc::new(|_: &Value| {
                Some(McpTransportError::new("broken server cannot initialize"))
            })),
        )
    });
    let (broken, _) = connect(
        entry("broken", json!({"command": "unused"})),
        vec![failing_factory],
        ConnectOptions::default(),
    );

    let error = broken.get_client().await.expect_err("broken server fails");
    assert!(
        error
            .to_string()
            .contains("broken server cannot initialize")
    );
    assert_eq!(broken.status(true).state, McpServerStatusState::Failed);

    healthy.get_client().await.expect("healthy server connects");
    assert_eq!(healthy.status(true).state, McpServerStatusState::Connected);
    let result = healthy
        .call_tool("echo", json!({}), &McpRequestOptions::new())
        .await
        .expect("healthy tool call succeeds");
    assert_eq!(
        result_value(&result),
        json!({"content": [{"type": "text", "text": "ok"}]})
    );

    healthy.close().await;
    broken.close().await;
}

/// 用不到的信道类型（保持 `CancellationToken`/`AuthProvider` 导入被使用）。
#[allow(dead_code)]
fn _typecheck(token: CancellationToken, _: Arc<dyn AuthProvider>, _: Option<OAuthChallenge>) {
    let _ = token;
}
