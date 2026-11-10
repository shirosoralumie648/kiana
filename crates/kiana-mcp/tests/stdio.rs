//! `packages/mcp/test/stdio.test.ts` 的移植：真实子进程 + `tests/fixtures/*.mjs`。
//!
//! TS 测试通过 `McpClient` 驱动传输；`McpClient` 属于本 crate 的另一个单元（`src/client.rs`
//! 在本单元里仍是占位模块），因此这里用一个最小的请求/响应助手直接驱动传输，断言与 TS 测试
//! **相同**（`listTools` 形状、`callTool` 回显、数字 pid、stderr 含 `stdio fixture ready`、
//! `close()` 之后 close 事件已派发——对应 TS 的 `client.connectionState === "closed"`）。
//! `McpClient` 那条路径（initialize 校验、`connectionState`）由该单元的测试覆盖；本文件的
//! 断言刻意只碰传输层，因此不与它重复。
//!
//! fixture 通过 `node <fixture>` 启动（与 TS 的 `process.execPath` 等价；本机有 node 26）。
//! `node` 是硬性前提：缺失时用例**失败**（`require_node`），与 TS 的 vitest 行为一致。

use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kiana_mcp::jsonrpc::McpError;
use kiana_mcp::transports::stdio::{StdioStderrMode, StdioTransport, StdioTransportOptions};
use kiana_mcp::transports::{
    McpTransport, McpTransportMessageListener, McpTransportSendOptions, Subscription,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// 本机是否有可用的 `node`（没有时进程级用例 `#[ignore]`）。
fn node_available() -> bool {
    StdCommand::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// `node` 是这些用例的**硬性前提**（TS 的 vitest 在缺少 node 时同样会失败：`crossSpawn` reject）。
/// 早期版本在这里 `return` 就算通过，等于把"缺少前提"变成静默成功（评审 finding #9）。
fn require_node() {
    assert!(
        node_available(),
        "these stdio tests spawn `node`; install node or run them in an environment that has it"
    );
}

/// 收集消息的监听器 + 一个 `recv` 接口（与 `McpClient` 的 pending map 等价的小助手）。
struct Inbox {
    receiver: mpsc::UnboundedReceiver<Value>,
    _subscription: Subscription,
}

fn inbox(transport: &StdioTransport) -> Inbox {
    let (sender, receiver) = mpsc::unbounded_channel();
    let listener: McpTransportMessageListener = Arc::new(move |message: &Value| {
        let _ = sender.send(message.clone());
        Ok(())
    });
    Inbox {
        receiver,
        _subscription: transport.on_message(listener),
    }
}

impl Inbox {
    async fn response(&mut self, id: i64) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let message = tokio::time::timeout_at(deadline, self.receiver.recv())
                .await
                .expect("response before timeout")
                .expect("transport stayed open");
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                return message;
            }
        }
    }
}

#[tokio::test]
async fn connects_to_a_newline_delimited_mcp_server_and_captures_stderr() {
    // TS: "connects to a newline-delimited MCP server and captures stderr".
    require_node();
    let stderr_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let collected = Arc::clone(&stderr_chunks);
    let mut options = StdioTransportOptions::new("node");
    options.args = Some(vec![fixture("stdio-server.mjs")]);
    options.on_stderr = Some(Arc::new(move |chunk: &str| {
        collected.lock().unwrap().push(chunk.to_owned());
    }));
    let transport = StdioTransport::new(options);

    let mut inbox = inbox(&transport);
    transport.start().await.expect("start");

    transport
        .send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"stdio-test","version":"1.0.0"}
            }}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect("initialize");
    let initialize = inbox.response(1).await;
    assert_eq!(initialize["result"]["serverInfo"]["name"], "stdio-fixture");

    transport
        .send(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect("initialized");

    transport
        .send(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect("tools/list");
    let tools = inbox.response(2).await;
    assert_eq!(
        tools["result"],
        json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
    );

    transport
        .send(
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hello"}}}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect("tools/call");
    let call = inbox.response(3).await;
    assert_eq!(
        call["result"],
        json!({"content":[{"type":"text","text":"hello"}]})
    );

    assert!(transport.pid().is_some(), "pid is a number");
    tokio::time::sleep(Duration::from_millis(10)).await;
    let joined = stderr_chunks.lock().unwrap().join("");
    assert!(joined.contains("stdio fixture ready"), "onStderr: {joined}");
    assert!(transport.stderr().contains("stdio fixture ready"));

    let closed = Arc::new(Mutex::new(false));
    let seen = Arc::clone(&closed);
    let _close = transport.on_close(Arc::new(move || {
        *seen.lock().unwrap() = true;
        Ok(())
    }));
    transport.close().await.expect("close");
    assert!(*closed.lock().unwrap(), "emitClose on close");
}

#[cfg(unix)]
#[tokio::test]
async fn kills_a_server_that_ignores_shutdown_including_its_children() {
    // TS: "kills a server that ignores shutdown, including its children"（skipIf(win32), 10 s）。
    require_node();
    let mut options = StdioTransportOptions::new("node");
    options.args = Some(vec![fixture("stubborn-server.mjs")]);
    options.close_timeout_ms = Some(100);
    let transport = StdioTransport::new(options);
    transport.start().await.expect("start");

    let mut grandchild = None;
    for _ in 0..100 {
        if grandchild.is_some() {
            break;
        }
        grandchild = parse_grandchild(&transport.stderr());
        if grandchild.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    let grandchild = grandchild.expect("grandchild pid on stderr");

    let started_at = Instant::now();
    transport.close().await.expect("close");
    let elapsed = started_at.elapsed();
    assert!(elapsed < Duration::from_secs(5), "close took {elapsed:?}");
    // TS 参考实测：grace(100ms) + closeTimeoutMs(100ms) ≈ 205 ms。给足调度余量。
    assert!(
        elapsed >= Duration::from_millis(90),
        "close returned before the SIGTERM grace: {elapsed:?}"
    );

    let mut alive = true;
    for _ in 0..100 {
        if !alive {
            break;
        }
        // `process.kill(pid, 0)` 的等价：`kill(pid, 0)` 返回 ESRCH 表示已消失。
        let running = unsafe { libc::kill(grandchild, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if running {
            tokio::time::sleep(Duration::from_millis(20)).await;
        } else {
            alive = false;
        }
    }
    assert!(!alive, "grandchild {grandchild} survived");
}

/// `/grandchild (\d+)/`。
#[cfg(unix)]
fn parse_grandchild(stderr: &str) -> Option<i32> {
    let start = stderr.find("grandchild ")? + "grandchild ".len();
    let rest = &stderr[start..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

// 让 `McpError` 的名字断言在进程级测试里也有一处（与 §27.3 的 stdio 错误串并列）。
#[test]
fn connection_closed_error_name_matches_ts() {
    assert_eq!(
        McpError::connection_closed().name(),
        "McpConnectionClosedError"
    );
    assert_eq!(StdioStderrMode::default(), StdioStderrMode::Pipe);
}
