//! stdio 分帧、stderr 尾部与关闭阶梯的单元/集成测试（§15.4、§15.5、§15.3）。
//!
//! 载荷的黄金文本来自 TS 参考实现（`tests/fixtures/stdio_payload_golden.json`，由
//! `scratch/kiana-mcp__transports-stdio/gen_payload_golden.mjs` 跑 `StdioTransport` 写到一个真实子进程
//! 的 stdin 得到）：服务器可能比较原始字节，因此断言逐字文本。

use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_mcp::transports::stdio::{StdioStderrMode, StdioTransport, StdioTransportOptions};
use kiana_mcp::transports::{
    McpTransport, McpTransportMessageListener, McpTransportSendOptions, Subscription,
};
use serde_json::{Value, json};

/// PATH 上可执行 `node` 的绝对路径（`inherit_env = false` 时子进程没有 PATH，必须用绝对路径）。
fn node_path() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("node"))
        .find(|candidate| candidate.is_file())
}

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

/// 一个往 stdout 写固定字节、然后保持 stdin 打开的 `node -e` 服务器。
fn node_script(script: &str) -> StdioTransportOptions {
    let mut options = StdioTransportOptions::new("node");
    options.args = Some(vec!["-e".to_owned(), script.to_owned()]);
    options
}

struct Collector {
    messages: Arc<Mutex<Vec<Value>>>,
    errors: Arc<Mutex<Vec<(String, String)>>>,
    closes: Arc<Mutex<usize>>,
    _subscriptions: Vec<Subscription>,
}

fn collect(transport: &StdioTransport) -> Collector {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let closes = Arc::new(Mutex::new(0usize));

    let sink = Arc::clone(&messages);
    let listener: McpTransportMessageListener = Arc::new(move |message: &Value| {
        sink.lock().unwrap().push(message.clone());
        Ok(())
    });
    let message_subscription = transport.on_message(listener);

    let sink = Arc::clone(&errors);
    let error_subscription = transport.on_error(Arc::new(move |error| {
        sink.lock()
            .unwrap()
            .push((error.name().to_owned(), error.to_string()));
        Ok(())
    }));

    let counter = Arc::clone(&closes);
    let close_subscription = transport.on_close(Arc::new(move || {
        *counter.lock().unwrap() += 1;
        Ok(())
    }));

    Collector {
        messages,
        errors,
        closes,
        _subscriptions: vec![message_subscription, error_subscription, close_subscription],
    }
}

async fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    for _ in 0..500 {
        if let Some(value) = check() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

// ---------------------------------------------------------------------------
// 载荷（§15.2 / §15.6）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn send_payload_matches_the_typescript_bytes() {
    require_node();
    let golden: Value = serde_json::from_str(include_str!("fixtures/stdio_payload_golden.json"))
        .expect("golden fixture parses");
    let messages = golden["messages"].as_array().expect("messages");
    let wire = golden["wire"].as_array().expect("wire");

    let capture = std::env::temp_dir().join(format!(
        "kiana-mcp-stdio-payload-{}.bin",
        std::process::id()
    ));
    let script = format!(
        r#"const fs = require("node:fs");
const chunks = [];
process.stdin.on("data", (c) => chunks.push(c));
process.stdin.on("end", () => {{ fs.writeFileSync({:?}, Buffer.concat(chunks)); process.exit(0); }});"#,
        capture.to_string_lossy()
    );
    let transport = StdioTransport::new(node_script(&script));
    transport.start().await.expect("start");
    for message in messages {
        transport
            .send(message.clone(), McpTransportSendOptions::none())
            .await
            .expect("send");
    }
    transport.close().await.expect("close");

    let written = wait_for("child stdin capture", || {
        std::fs::read_to_string(&capture).ok()
    })
    .await;
    let _ = std::fs::remove_file(&capture);
    let expected: Vec<&str> = wire.iter().filter_map(Value::as_str).collect();
    assert_eq!(written, format!("{}\n", expected.join("\n")));
}

// ---------------------------------------------------------------------------
// 分帧（§15.4）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn splits_lines_strips_one_cr_and_skips_blank_lines() {
    require_node();
    let transport = StdioTransport::new(node_script(
        r#"process.stdout.write('{"jsonrpc":"2.0","method":"a"}\r\n\r\n   \n\t\n{"jsonrpc":"2.0","method":"b"}\n{"jsonrpc":"2.0","method":"c"}\r\n');"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("three messages", || {
        let messages = collector.messages.lock().unwrap();
        (messages.len() >= 3).then(|| messages.clone())
    })
    .await;
    let methods: Vec<String> = collector
        .messages
        .lock()
        .unwrap()
        .iter()
        .map(|message| message["method"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(methods, vec!["a", "b", "c"]);
    assert!(collector.errors.lock().unwrap().is_empty());
    transport.close().await.expect("close");
}

#[tokio::test]
async fn overlong_line_reports_the_byte_limit_and_keeps_streaming() {
    require_node();
    // 第一行 40 字节 > 上限 35：报错并继续；第二行（31 字节）正常派发。
    let transport = {
        let mut options = node_script(
            r#"process.stdout.write('{"jsonrpc":"2.0","parammm":"0123456789"}\n{"jsonrpc":"2.0","method":"ok"}\n');"#,
        );
        options.max_message_bytes = Some(35);
        StdioTransport::new(options)
    };
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("one message", || {
        let messages = collector.messages.lock().unwrap();
        (!messages.is_empty()).then(|| messages.clone())
    })
    .await;
    assert_eq!(collector.messages.lock().unwrap()[0]["method"], "ok");
    let errors = collector.errors.lock().unwrap().clone();
    assert_eq!(
        errors,
        vec![(
            "Error".to_owned(),
            "MCP stdio message exceeds 35 bytes".to_owned()
        )]
    );
    transport.close().await.expect("close");
}

#[tokio::test]
async fn unterminated_oversized_buffer_is_dropped_and_reported() {
    require_node();
    let transport = {
        let mut options = node_script(
            r#"process.stdout.write('{"jsonrpc":"2.0","method":"' + "x".repeat(200));"#,
        );
        options.max_message_bytes = Some(20);
        StdioTransport::new(options)
    };
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("overrun error", || {
        let errors = collector.errors.lock().unwrap();
        (!errors.is_empty()).then(|| errors.clone())
    })
    .await;
    assert_eq!(
        *collector.errors.lock().unwrap(),
        vec![(
            "Error".to_owned(),
            "MCP stdio message exceeds 20 bytes".to_owned()
        )]
    );
    transport.close().await.expect("close");
}

#[tokio::test]
async fn incomplete_message_at_close_is_reported_before_close() {
    require_node();
    let transport = StdioTransport::new(node_script(
        r#"process.stdout.write('{"jsonrpc":"2.0","method":"trunc');"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("close", || {
        (*collector.closes.lock().unwrap() > 0).then_some(())
    })
    .await;
    assert_eq!(
        *collector.errors.lock().unwrap(),
        vec![(
            "Error".to_owned(),
            "MCP stdio server closed with an incomplete JSON-RPC message".to_owned()
        )]
    );
    transport.close().await.expect("close");
}

#[tokio::test]
async fn invalid_json_reports_an_error_and_the_transport_stays_open() {
    require_node();
    let transport = StdioTransport::new(node_script(
        r#"process.stdout.write('not json\n{"jsonrpc":"2.0","method":"still-here"}\n'); setTimeout(() => {}, 60_000);"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("error and message", || {
        let errors = collector.errors.lock().unwrap();
        let messages = collector.messages.lock().unwrap();
        (!errors.is_empty() && !messages.is_empty()).then_some(())
    })
    .await;
    let errors = collector.errors.lock().unwrap().clone();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "SyntaxError");
    assert!(errors[0].1.contains("JSON"), "message: {}", errors[0].1);
    assert_eq!(*collector.closes.lock().unwrap(), 0);
    // 传输仍然可用：再发一条。
    transport
        .send(
            json!({"jsonrpc":"2.0","method":"ping"}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect("send after parse error");
    transport.close().await.expect("close");
}

#[tokio::test]
async fn a_valid_json_value_that_is_not_jsonrpc_reports_mcperror() {
    require_node();
    let transport = StdioTransport::new(node_script(
        r#"process.stdout.write('{"hello":"world"}\n'); setTimeout(() => {}, 60_000);"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("error", || {
        let errors = collector.errors.lock().unwrap();
        (!errors.is_empty()).then(|| errors.clone())
    })
    .await;
    assert_eq!(
        *collector.errors.lock().unwrap(),
        vec![("McpError".to_owned(), "Invalid JSON-RPC message".to_owned())]
    );
    transport.close().await.expect("close");
}

#[tokio::test]
async fn framing_event_order_matches_the_typescript_golden() {
    require_node();
    // 黄金值：`tests/fixtures/stdio_framing_golden.json`，由 TS 参考实现对同一个
    // `framing-server.mjs` 运行 `StdioTransport` 记录的事件序列（message / error name+message / close）。
    let golden: Value = serde_json::from_str(include_str!("fixtures/stdio_framing_golden.json"))
        .expect("golden fixture parses");
    for case in golden["cases"].as_array().expect("cases") {
        let mut options = StdioTransportOptions::new("node");
        options.args = Some(vec![fixture_path("framing-server.mjs")]);
        if let Some(max) = case["options"]["maxMessageBytes"].as_u64() {
            options.max_message_bytes = Some(max as usize);
        }
        let transport = StdioTransport::new(options);

        // 单一有序事件表（TS 的 onMessage/onError/onClose 都汇到这里，避免按类型分组丢失交错顺序）。
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let _message = transport.on_message(Arc::new(move |message: &Value| {
            sink.lock().unwrap().push(format!(
                "msg:{}",
                message["method"].as_str().unwrap_or_default()
            ));
            Ok(())
        }));
        let sink = Arc::clone(&events);
        let _error = transport.on_error(Arc::new(move |error| {
            sink.lock()
                .unwrap()
                .push(format!("err:{}:{}", error.name(), error));
            Ok(())
        }));
        let sink = Arc::clone(&events);
        let _close = transport.on_close(Arc::new(move || {
            sink.lock().unwrap().push("close".to_owned());
            Ok(())
        }));

        transport.start().await.expect("start");
        wait_for("close", || {
            events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "close")
                .then_some(())
        })
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut expected: Vec<String> = Vec::new();
        for event in case["events"].as_array().expect("events") {
            match event["kind"].as_str().expect("kind") {
                "message" => expected.push(format!(
                    "msg:{}",
                    event["message"]["method"].as_str().unwrap_or_default()
                )),
                "error" => expected.push(format!(
                    "err:{}:{}",
                    event["name"].as_str().unwrap_or_default(),
                    event["message"].as_str().unwrap_or_default()
                )),
                _ => expected.push("close".to_owned()),
            }
        }
        let actual = events.lock().unwrap().clone();
        assert_eq!(actual.len(), expected.len(), "event count, case {case:?}");
        // 全部事件逐字相等：`kiana_json::js_parse` 的错误文本就是 V8 的模板，`SyntaxError` 也一致。
        assert_eq!(actual, expected, "events, case {case:?}");
        transport.close().await.expect("close");
    }
}

/// `tests/fixtures/<name>` 的绝对路径。
fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test]
async fn max_message_bytes_zero_reports_every_non_empty_line() {
    require_node();
    // TS 实测：`maxMessageBytes: 0` 时每条非空行都报 `exceeds 0 bytes`（空白行仍被跳过）。
    let transport = {
        let mut options = node_script(
            r#"process.stdout.write('\n\n{"jsonrpc":"2.0","method":"a"}\n'); setTimeout(() => {}, 60_000);"#,
        );
        options.max_message_bytes = Some(0);
        StdioTransport::new(options)
    };
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("error", || {
        let errors = collector.errors.lock().unwrap();
        (!errors.is_empty()).then(|| errors.clone())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *collector.errors.lock().unwrap(),
        vec![(
            "Error".to_owned(),
            "MCP stdio message exceeds 0 bytes".to_owned()
        )]
    );
    assert!(collector.messages.lock().unwrap().is_empty());
    transport.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// stderr（§15.5）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stderr_tail_keeps_the_last_bytes_and_on_stderr_gets_raw_chunks() {
    require_node();
    let chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&chunks);
    let mut options = node_script(
        r#"process.stderr.write("0123456789"); process.stderr.write("abcdefghij"); setTimeout(() => {}, 60_000);"#,
    );
    options.max_stderr_bytes = Some(4);
    options.on_stderr = Some(Arc::new(move |chunk: &str| {
        sink.lock().unwrap().push(chunk.to_owned())
    }));
    let transport = StdioTransport::new(options);
    transport.start().await.expect("start");
    wait_for("stderr tail", || {
        (transport.stderr() == "ghij").then_some(())
    })
    .await;
    let joined = chunks.lock().unwrap().join("");
    assert_eq!(joined, "0123456789abcdefghij");
    transport.close().await.expect("close");
}

#[tokio::test]
async fn stderr_tail_decodes_lossily_when_truncated_mid_character() {
    require_node();
    // "é" 是 0xC3 0xA9；上限 1 字节 → 尾部只剩 0xA9 → U+FFFD。
    let mut options = node_script(r#"process.stderr.write("é"); setTimeout(() => {}, 60_000);"#);
    options.max_stderr_bytes = Some(1);
    let transport = StdioTransport::new(options);
    transport.start().await.expect("start");
    wait_for("stderr tail", || {
        (!transport.stderr().is_empty()).then(|| transport.stderr())
    })
    .await;
    assert_eq!(transport.stderr(), "\u{fffd}");
    transport.close().await.expect("close");
}

#[tokio::test]
async fn stderr_inherit_mode_keeps_no_tail_and_calls_no_callback() {
    require_node();
    let calls = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&calls);
    let mut options =
        node_script(r#"process.stderr.write("inherited\n"); setTimeout(() => {}, 60_000);"#);
    options.stderr = Some(StdioStderrMode::Inherit);
    options.on_stderr = Some(Arc::new(move |_chunk: &str| {
        *counter.lock().unwrap() += 1;
    }));
    let transport = StdioTransport::new(options);
    transport.start().await.expect("start");
    // stderr 仍被重定向到终端，但没有管道读取 ⇒ 缓冲永远为空、回调不触发。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(transport.stderr(), "");
    assert_eq!(*calls.lock().unwrap(), 0);
    transport.close().await.expect("close");
}

#[tokio::test]
async fn max_stderr_bytes_zero_keeps_the_whole_buffer() {
    require_node();
    // TS 的 `subarray(-0)` 是 `subarray(0)`：上限 0 不截断（缓冲无限增长）。
    let mut options = node_script(
        r#"process.stderr.write("abc"); setTimeout(() => process.stderr.write("def"), 50); setTimeout(() => {}, 60_000);"#,
    );
    options.max_stderr_bytes = Some(0);
    let transport = StdioTransport::new(options);
    transport.start().await.expect("start");
    wait_for("both stderr chunks", || {
        (transport.stderr().len() >= 6).then(|| transport.stderr())
    })
    .await;
    assert_eq!(transport.stderr(), "abcdef");
    transport.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// 生命周期（§15.1 / §15.2 / §15.3）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn start_twice_reports_already_started() {
    require_node();
    let transport = StdioTransport::new(node_script("setTimeout(() => {}, 60_000);"));
    transport.start().await.expect("start");
    let error = transport.start().await.expect_err("second start");
    assert_eq!(error.to_string(), "MCP stdio transport already started");
    assert_eq!(error.name(), "Error");
    transport.close().await.expect("close");
    // TS 的次序：`started` 检查在 `closed` 之前，所以 start→close 之后再 start 报 already started。
    let error = transport.start().await.expect_err("start after close");
    assert_eq!(error.to_string(), "MCP stdio transport already started");

    // 只 close、从未 start 的传输：start 报 McpConnectionClosedError（§15.1）。
    let fresh = StdioTransport::new(node_script("setTimeout(() => {}, 60_000);"));
    fresh.close().await.expect("close before start");
    let error = fresh.start().await.expect_err("start after close");
    assert_eq!(error.name(), "McpConnectionClosedError");
    assert_eq!(error.to_string(), "MCP connection closed");
}

#[tokio::test]
async fn spawn_failure_rejects_start_and_closes_once() {
    let mut options = StdioTransportOptions::new("/definitely/not/here-xyz");
    options.args = Some(Vec::new());
    let transport = StdioTransport::new(options);
    let closed = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&closed);
    let _close = transport.on_close(Arc::new(move || {
        *counter.lock().unwrap() += 1;
        Ok(())
    }));
    let error = transport.start().await.expect_err("spawn fails");
    assert_eq!(error.name(), "Error");
    // Node 的异步 spawn 错误文本带文件名：`spawn <file> ENOENT`（并带 `code`）。
    assert_eq!(error.to_string(), "spawn /definitely/not/here-xyz ENOENT");
    assert_eq!(error.code(), Some("ENOENT"));
    assert_eq!(transport.pid(), None);
    wait_for("close after spawn failure", || {
        (*closed.lock().unwrap() > 0).then_some(())
    })
    .await;
    // spawn 失败后 `started` 仍是 true。
    let error = transport.start().await.expect_err("already started");
    assert_eq!(error.to_string(), "MCP stdio transport already started");
    // 没有可写的 stdin ⇒ send 报 McpConnectionClosedError（TS 同样如此，§15.2）。
    let error = transport
        .send(
            json!({"jsonrpc":"2.0","method":"x"}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("send without a child");
    assert_eq!(error.name(), "McpConnectionClosedError");
    transport.close().await.expect("close");
    assert_eq!(*closed.lock().unwrap(), 1, "emitClose fires at most once");
}

#[tokio::test]
async fn send_before_start_and_after_close_report_connection_closed() {
    let transport = StdioTransport::new(StdioTransportOptions::new("node"));
    let error = transport
        .send(
            json!({"jsonrpc":"2.0","method":"x"}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("send before start");
    assert_eq!(error.name(), "McpConnectionClosedError");
    assert_eq!(error.to_string(), "MCP connection closed");
    transport.close().await.expect("close");
    let error = transport
        .send(
            json!({"jsonrpc":"2.0","method":"x"}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("send after close");
    assert_eq!(error.to_string(), "MCP connection closed");
}

#[tokio::test]
async fn close_before_start_emits_close_once() {
    let transport = StdioTransport::new(StdioTransportOptions::new("node"));
    let collector = collect(&transport);
    transport.close().await.expect("close");
    assert_eq!(*collector.closes.lock().unwrap(), 1);
    transport.close().await.expect("close again");
    assert_eq!(*collector.closes.lock().unwrap(), 1);
}

#[tokio::test]
async fn close_after_the_child_exited_only_ends_stdin() {
    require_node();
    let transport = StdioTransport::new(node_script("process.exit(0);"));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    // 子进程 close 事件 ⇒ emitClose（1 次）。之后再 close() 只 end stdin。
    wait_for("child close", || {
        (*collector.closes.lock().unwrap() > 0).then_some(())
    })
    .await;
    assert_eq!(*collector.closes.lock().unwrap(), 1);
    transport.close().await.expect("close");
    assert_eq!(transport.pid(), None);
}

#[tokio::test]
async fn signal_is_ignored_by_send() {
    require_node();
    let transport = StdioTransport::new(node_script("setTimeout(() => {}, 60_000);"));
    transport.start().await.expect("start");
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    transport
        .send(
            json!({"jsonrpc":"2.0","method":"x"}),
            McpTransportSendOptions::with_signal(Some(token)),
        )
        .await
        .expect("cancelled signal does not stop a stdio send");
    transport.close().await.expect("close");
}

#[tokio::test]
async fn env_is_merged_unless_inherit_env_is_false() {
    require_node();
    // 写入 DEVNULL 的探针：子进程把选定的环境变量写进 stdout，再作为一条 JSON-RPC 消息发出来。
    let script = r#"process.stdout.write(JSON.stringify({jsonrpc:"2.0",method:"env",params:{only:process.env.KIANA_ONLY ?? null, inherited:process.env.PATH === undefined ? null : "present"}})+"\n"); setTimeout(() => {}, 60_000);"#;

    let mut env = indexmap::IndexMap::new();
    env.insert("KIANA_ONLY".to_owned(), "yes".to_owned());
    let mut options = node_script(script);
    options.env = Some(env.clone());
    let transport = StdioTransport::new(options);
    let collector = collect(&transport);
    transport.start().await.expect("start");
    let message = wait_for("env message", || {
        collector.messages.lock().unwrap().first().cloned()
    })
    .await;
    assert_eq!(message["params"]["only"], "yes");
    assert_eq!(message["params"]["inherited"], "present");
    transport.close().await.expect("close");

    let node = node_path().expect("node on PATH (required by these tests)");
    let mut options = StdioTransportOptions::new(node.to_string_lossy().into_owned());
    options.args = Some(vec!["-e".to_owned(), script.to_owned()]);
    options.env = Some(env);
    options.inherit_env = Some(false);
    let transport = StdioTransport::new(options);
    let collector = collect(&transport);
    transport.start().await.expect("start");
    let message = wait_for("env message", || {
        collector.messages.lock().unwrap().first().cloned()
    })
    .await;
    assert_eq!(message["params"]["only"], "yes");
    assert_eq!(message["params"]["inherited"], Value::Null);
    transport.close().await.expect("close");
}

#[tokio::test]
async fn message_listener_error_is_reported_as_an_error() {
    require_node();
    // TS：`try { emitMessage } catch (e) { emitError(e) }` —— 监听器的异常转成 error 事件。
    let transport = StdioTransport::new(node_script(
        r#"process.stdout.write('{"jsonrpc":"2.0","method":"a"}\n{"jsonrpc":"2.0","method":"b"}\n'); setTimeout(() => {}, 60_000);"#,
    ));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _message = transport.on_message(Arc::new(move |message: &Value| {
        let method = message["method"].as_str().unwrap_or_default().to_owned();
        sink.lock().unwrap().push(method.clone());
        if method == "a" {
            return Err(kiana_mcp::auth_provider::McpTransportError::new(
                "listener boom",
            ));
        }
        Ok(())
    }));
    let sink = Arc::clone(&errors);
    let _error = transport.on_error(Arc::new(move |error| {
        sink.lock().unwrap().push(error.to_string());
        Ok(())
    }));
    transport.start().await.expect("start");
    wait_for("both messages", || {
        (seen.lock().unwrap().len() >= 2).then_some(())
    })
    .await;
    assert_eq!(*seen.lock().unwrap(), vec!["a", "b"]);
    assert_eq!(*errors.lock().unwrap(), vec!["listener boom"]);
    transport.close().await.expect("close");
}

#[tokio::test]
async fn cwd_selects_the_child_working_directory() {
    require_node();
    let directory = std::env::temp_dir();
    let script = r#"process.stdout.write(JSON.stringify({jsonrpc:"2.0",method:"cwd",params:{dir:process.cwd()}})+"\n"); setTimeout(() => {}, 60_000);"#;
    let mut options = node_script(script);
    options.cwd = Some(directory.clone());
    let transport = StdioTransport::new(options);
    let collector = collect(&transport);
    transport.start().await.expect("start");
    let message = wait_for("cwd message", || {
        collector.messages.lock().unwrap().first().cloned()
    })
    .await;
    let actual = std::fs::canonicalize(message["params"]["dir"].as_str().unwrap_or_default())
        .expect("child cwd resolves");
    let expected = std::fs::canonicalize(&directory).expect("temp dir resolves");
    assert_eq!(actual, expected);
    transport.close().await.expect("close");
}

#[tokio::test]
async fn close_right_after_a_failed_spawn_does_not_emit_a_second_close() {
    // TS 实测：spawn 失败 → close() 只 end stdin（exitCode !== null 分支），close 事件随后由子进程的
    // close 处理器发出，且只发一次。
    let transport = StdioTransport::new(StdioTransportOptions::new("/definitely/not/here-xyz"));
    let collector = collect(&transport);
    assert!(transport.start().await.is_err());
    transport.close().await.expect("close");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(*collector.closes.lock().unwrap(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn close_after_exit_but_before_close_only_ends_stdin() {
    require_node();
    // 子进程 exit 后，孙进程仍持有它的 stdout 管道 ⇒ Node 的 `close` 事件迟迟不来（只有 `exit` 到了）。
    // §15.3：此时 `close()` 走 `exitCode !== null` 分支——只 end stdin，**不** emitClose。
    let transport = StdioTransport::new(node_script(
        r#"const { spawn } = require("node:child_process");
const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: ["ignore", "inherit", "ignore"] });
process.stderr.write("holder " + child.pid + "\n");
process.exit(0);"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    let holder = wait_for("holder pid", || {
        let stderr = transport.stderr();
        let index = stderr.find("holder ")?;
        stderr[index + "holder ".len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<i32>()
            .ok()
    })
    .await;
    // 等子进程 exit（pid 变 None 前的窗口由 close_pending/alive 状态保护）。
    tokio::time::sleep(Duration::from_millis(300)).await;
    transport.close().await.expect("close");
    assert_eq!(
        *collector.closes.lock().unwrap(),
        0,
        "close() after exit must not emitClose"
    );
    // 收尾：杀掉仍然持有管道的孙进程，close 事件随后到达。
    unsafe {
        libc::kill(holder, libc::SIGKILL);
    }
    wait_for("close after the pipe holder died", || {
        (*collector.closes.lock().unwrap() > 0).then_some(())
    })
    .await;
}

// ---------------------------------------------------------------------------
// 评审回归（finding #2/#3/#5/#6/#7/#8/#9）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawn_errors_carry_the_command_and_node_error_codes() {
    // 评审 finding #2：Node 的文本是 `spawn <file> <CODE>`，且 9 个映射之外的 errno 也带 code。
    require_node();
    // ENOENT（异步类）：带文件名。
    let transport = StdioTransport::new(StdioTransportOptions::new("/definitely/not/here-xyz"));
    let error = transport.start().await.expect_err("missing binary");
    assert_eq!(error.name(), "Error");
    assert_eq!(error.to_string(), "spawn /definitely/not/here-xyz ENOENT");
    assert_eq!(error.code(), Some("ENOENT"));
    transport.close().await.expect("close");

    // E2BIG（同步类）：**不带**文件名，且没有 close 事件。
    let mut options = StdioTransportOptions::new("node");
    options.args = Some(vec![
        "-e".to_owned(),
        "1".to_owned(),
        "x".repeat(3 * 1024 * 1024),
    ]);
    let transport = StdioTransport::new(options);
    let collector = collect(&transport);
    let error = transport.start().await.expect_err("argv too long");
    assert_eq!(error.name(), "Error");
    assert_eq!(error.to_string(), "spawn E2BIG");
    assert_eq!(error.code(), Some("E2BIG"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *collector.closes.lock().unwrap(),
        0,
        "sync spawn failures have no close event"
    );
    // close() 走 `no child` 分支：emitClose 一次。
    transport.close().await.expect("close");
    assert_eq!(*collector.closes.lock().unwrap(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn spawn_errors_for_an_unexecutable_file_include_the_path() {
    // 评审 finding #2：EACCES 同样是异步类（带文件名 + errno -13 → close 事件）。
    require_node();
    let directory = std::env::temp_dir();
    let path = directory.join(format!("kiana-mcp-noexec-{}", std::process::id()));
    std::fs::write(&path, "#!/bin/sh\necho hi\n").expect("write fixture");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o644);
    }
    std::fs::set_permissions(&path, permissions).expect("chmod");
    let transport = StdioTransport::new(StdioTransportOptions::new(
        path.to_string_lossy().into_owned(),
    ));
    let collector = collect(&transport);
    let error = transport.start().await.expect_err("not executable");
    let _ = std::fs::remove_file(&path);
    assert_eq!(error.name(), "Error");
    assert_eq!(
        error.to_string(),
        format!("spawn {} EACCES", path.to_string_lossy())
    );
    assert_eq!(error.code(), Some("EACCES"));
    wait_for("close after EACCES", || {
        (*collector.closes.lock().unwrap() > 0).then_some(())
    })
    .await;
    transport.close().await.expect("close");
}

#[tokio::test]
async fn invalid_spawn_arguments_reject_with_a_type_error_and_no_events() {
    // 评审 finding #3：cross-spawn/child_process 在 JS 层同步抛 TypeError
    // （code = ERR_INVALID_ARG_VALUE）；没有 error/close 事件，this.child 保持 undefined。
    let cases: Vec<(StdioTransportOptions, &str)> = vec![
        (
            StdioTransportOptions::new(""),
            "The argument 'file' cannot be empty. Received ''",
        ),
        (
            {
                let mut options = StdioTransportOptions::new("node");
                options.args = Some(vec!["-e".to_owned(), "1".to_owned(), "a\0b".to_owned()]);
                options
            },
            "The argument 'args[2]' must be a string without null bytes. Received 'a\\x00b'",
        ),
        (
            {
                let mut options = StdioTransportOptions::new("node");
                options.args = Some(vec!["-e".to_owned(), "1".to_owned()]);
                let mut env = indexmap::IndexMap::new();
                env.insert("A".to_owned(), "x\0y".to_owned());
                options.env = Some(env);
                options
            },
            "The property 'options.env['A']' must be a string without null bytes. Received 'x\\x00y'",
        ),
    ];
    for (options, message) in cases {
        let transport = StdioTransport::new(options);
        let collector = collect(&transport);
        let error = transport.start().await.expect_err("invalid arguments");
        assert_eq!(error.name(), "TypeError", "case {message}");
        assert_eq!(
            error.code(),
            Some("ERR_INVALID_ARG_VALUE"),
            "case {message}"
        );
        assert_eq!(error.to_string(), message);
        assert_eq!(transport.pid(), None);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            collector.errors.lock().unwrap().is_empty(),
            "no error event"
        );
        assert_eq!(*collector.closes.lock().unwrap(), 0, "no close event");
        // close() 走"没有 child"分支：只 emitClose 一次。
        transport.close().await.expect("close");
        assert_eq!(*collector.closes.lock().unwrap(), 1);
    }
}

#[tokio::test]
async fn close_timeout_above_the_timer_maximum_clamps_like_set_timeout() {
    // 评审 finding #5：Node 把 > 2^31-1 的 setTimeout 延迟钳到 1 ms；旧实现在 debug 下
    // 加法溢出 panic，release 下等约 49 天。
    //
    // 本轮评审 finding #4：钳位后 SIGKILL 定时器（1 ms）**早于** SIGTERM 定时器（500 ms）
    // 到期，Node 的定时器按到期时间触发，因此 close() 应在毫秒级完成（TS 实测 5–9 ms）；
    // 旧实现按注册顺序先等 500 ms 的 SIGTERM，要用 505 ms 左右。
    require_node();
    for close_timeout_ms in [u64::MAX, 4_294_967_295, 2_147_483_648] {
        let mut options =
            node_script("process.on('SIGTERM', () => {}); setInterval(() => {}, 1000);");
        options.close_timeout_ms = Some(close_timeout_ms);
        let transport = StdioTransport::new(options);
        transport.start().await.expect("start");
        let started = std::time::Instant::now();
        transport.close().await.expect("close");
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "closeTimeoutMs {close_timeout_ms} should SIGKILL at the clamped 1 ms deadline, took {:?}",
            started.elapsed()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sends_after_a_write_failure_report_connection_closed() {
    // 评审 finding #6：第一次写失败（EPIPE）后 Node 会 destroy stdin，之后的 send 在
    // `stdin?.writable` 检查处抛 McpConnectionClosedError，不再发 error 事件。
    require_node();
    let transport = StdioTransport::new(node_script(
        r#"require("node:fs").closeSync(0); process.stderr.write("stdin closed\n"); setTimeout(() => {}, 60_000);"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("stdin closed", || {
        transport.stderr().contains("stdin closed").then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let first = transport
        .send(
            json!({"jsonrpc":"2.0","method":"first"}),
            McpTransportSendOptions::none(),
        )
        .await
        .expect_err("first write fails");
    assert_eq!(first.name(), "Error");
    assert_eq!(first.to_string(), "write EPIPE");
    assert_eq!(first.code(), Some("EPIPE"));
    for index in 0..3 {
        let error = transport
            .send(
                json!({"jsonrpc":"2.0","method":format!("later-{index}")}),
                McpTransportSendOptions::none(),
            )
            .await
            .expect_err("stdin is destroyed");
        assert_eq!(error.name(), "McpConnectionClosedError");
    }
    assert_eq!(
        *collector.errors.lock().unwrap(),
        vec![("Error".to_owned(), "write EPIPE".to_owned())],
        "only the first failure emits an error event"
    );
    transport.close().await.expect("close");
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_sends_after_a_broken_pipe_emit_one_error_and_close_the_rest() {
    // 本轮评审 finding #1（major）：Node 的 `stdin.write` 在**空闲**流上同步 `write(2)`，
    // EPIPE 会当场 `destroy()` 流；同一批并发 `send()` 里只有第一个拿到 `write EPIPE`，其余在
    // `stdin?.writable` 检查处抛 `McpConnectionClosedError`，流恰好发一个 error 事件
    // （TS 实测：n=2 → 1+1，n=3 → 1+2，n=5 → 1+4）。旧实现把每笔写入排进同一个通道，所有
    // 写入都以 EPIPE 失败并各发一个事件。
    require_node();
    let transport = StdioTransport::new(node_script(
        r#"require("node:fs").closeSync(0); process.stderr.write("stdin closed\n"); setTimeout(() => {}, 60_000);"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("stdin closed", || {
        transport.stderr().contains("stdin closed").then_some(())
    })
    .await;
    // 等管道的可写就绪事件到达（第一个 send 就发生在就绪之前时，Node 也会先写、拿到 EPIPE）。
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut tasks = Vec::new();
    for index in 0..5 {
        let transport = transport.clone();
        tasks.push(tokio::spawn(async move {
            transport
                .send(
                    json!({"jsonrpc":"2.0","method":format!("concurrent-{index}")}),
                    McpTransportSendOptions::none(),
                )
                .await
                .expect_err("stdin is closed")
        }));
    }
    let mut broken_pipe = 0;
    let mut closed = 0;
    for task in tasks {
        let error = task.await.expect("send task");
        if error.name() == "Error" && error.code() == Some("EPIPE") {
            assert_eq!(error.to_string(), "write EPIPE");
            broken_pipe += 1;
        } else if error.name() == "McpConnectionClosedError" {
            closed += 1;
        } else {
            panic!("unexpected send error: {} ({error})", error.name());
        }
    }
    assert_eq!(broken_pipe, 1, "exactly one send sees the broken pipe");
    assert_eq!(closed, 4, "the rest see stdin as non-writable");
    assert_eq!(
        *collector.errors.lock().unwrap(),
        vec![("Error".to_owned(), "write EPIPE".to_owned())],
        "one stdin error event, not one per send"
    );
    transport.close().await.expect("close");
}

#[cfg(unix)]
#[tokio::test]
async fn buffered_writes_all_fail_with_one_error_event() {
    // 本轮评审 finding #1（major）的另一半：管道写满时 Node 把数据放进流的内部缓冲；管道断开
    // 后**所有**缓冲中的写入都回调同一个 EPIPE，而流只发一个 error 事件（TS 实测：大块载荷
    // 并发写入 → 全部 `write EPIPE` + 一个事件）。旧实现每笔写入各自失败、各发一个事件。
    require_node();
    // 服务器不读 stdin，就绪 500 ms 后关掉它：期间父进程 1 MiB 的载荷写满 64 KiB 管道，其余
    // 进入内部缓冲；关掉之后写任务拿到 EPIPE。`ready` 保证 500 ms 的窗口从子进程启动后算起。
    let transport = StdioTransport::new(node_script(
        r#"process.stderr.write("ready\n"); setTimeout(() => { require("node:fs").closeSync(0); }, 500); setTimeout(() => {}, 60_000);"#,
    ));
    let collector = collect(&transport);
    transport.start().await.expect("start");
    wait_for("ready", || {
        transport.stderr().contains("ready").then_some(())
    })
    .await;

    let big = "x".repeat(1024 * 1024);
    let mut tasks = Vec::new();
    for index in 0..3 {
        let transport = transport.clone();
        let big = big.clone();
        tasks.push(tokio::spawn(async move {
            transport
                .send(
                    json!({"jsonrpc":"2.0","method":format!("buffered-{index}"),"params":{"data":big}}),
                    McpTransportSendOptions::none(),
                )
                .await
                .expect_err("stdin is broken")
        }));
    }
    for task in tasks {
        let error = task.await.expect("send task");
        assert_eq!(error.name(), "Error", "{error}");
        assert_eq!(error.to_string(), "write EPIPE", "{error}");
        assert_eq!(error.code(), Some("EPIPE"));
    }
    assert_eq!(
        *collector.errors.lock().unwrap(),
        vec![("Error".to_owned(), "write EPIPE".to_owned())],
        "one stream error event for every buffered write"
    );
    transport.close().await.expect("close");
}

#[tokio::test]
async fn invalid_argument_messages_truncate_at_128_utf16_code_units() {
    // 本轮评审 finding #2（major）：Node 用 `inspected.length > 128` 判断、`slice(0, 128)`
    // 截断，单位是 **UTF-16 码元**（emoji 占 2 个）；旧实现按 Unicode 标量计数，截断位置偏后。
    // 期望值来自 TS 参考实现的实测输出（同一条 `args[2]` 的 NUL 报错）。
    let emoji = "😀";
    let cases: Vec<(String, String)> = vec![
        // 61 个：inspect 长度恰好 128 个码元，不截断。
        (
            emoji.repeat(61) + "\0",
            format!("'{}\\x00'", emoji.repeat(61)),
        ),
        // 62 个：130 个码元 → 切在第 128 个（`\x00` 的第 3 个字符）之后。
        (
            emoji.repeat(62) + "\0",
            format!("'{}\\x0...", emoji.repeat(62)),
        ),
        // 63 个：132 个码元 → 恰好切在转义的反斜杠之后。
        (
            emoji.repeat(63) + "\0",
            format!("'{}\\...", emoji.repeat(63)),
        ),
        // 64/65 个：切点落在代理对中间；Node 留下孤立代理项 `\ud83d`，Rust 用 U+FFFD 代替
        // （模块文档"与 TS 的实现差异"第 6 条）。
        (
            emoji.repeat(64) + "\0",
            format!("'{}\u{FFFD}...", emoji.repeat(63)),
        ),
        (
            emoji.repeat(65) + "\0",
            format!("'{}\u{FFFD}...", emoji.repeat(63)),
        ),
    ];
    for (value, expected_inspect) in cases {
        let mut options = StdioTransportOptions::new("node");
        options.args = Some(vec!["-e".to_owned(), "1".to_owned(), value]);
        let transport = StdioTransport::new(options);
        let error = transport.start().await.expect_err("NUL in args");
        assert_eq!(error.code(), Some("ERR_INVALID_ARG_VALUE"));
        assert_eq!(
            error.to_string(),
            format!(
                "The argument 'args[2]' must be a string without null bytes. Received {expected_inspect}"
            )
        );
    }
}

#[tokio::test]
async fn an_empty_cwd_runs_in_the_current_directory() {
    // 评审 finding #8：Node 的 `cwd: ""` 等同于"不传"。
    require_node();
    let script = r#"process.stdout.write(JSON.stringify({jsonrpc:"2.0",method:"cwd",params:{dir:process.cwd()}})+"\n"); setTimeout(() => {}, 60_000);"#;
    let mut options = node_script(script);
    options.cwd = Some(std::path::PathBuf::new());
    let transport = StdioTransport::new(options);
    let collector = collect(&transport);
    transport.start().await.expect("start");
    let message = wait_for("cwd message", || {
        collector.messages.lock().unwrap().first().cloned()
    })
    .await;
    let actual = std::fs::canonicalize(message["params"]["dir"].as_str().unwrap_or_default())
        .expect("child cwd resolves");
    let expected =
        std::fs::canonicalize(std::env::current_dir().expect("cwd")).expect("parent cwd resolves");
    assert_eq!(actual, expected);
    transport.close().await.expect("close");
}

#[cfg(unix)]
#[tokio::test]
async fn stderr_chunks_are_split_like_node_pipe_reads() {
    // 评审 finding #4：Node 的 `data` chunk 上限是 64 KiB；旧实现用 16 KiB 读缓冲，跨边界的
    // 多字节字符会被切成两个 U+FFFD。这里断言分块与 TS 参考实测一致。
    require_node();
    let chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&chunks);
    let mut options =
        node_script(r#"process.stderr.write("x".repeat(20000)); setTimeout(() => {}, 60_000);"#);
    options.on_stderr = Some(Arc::new(move |chunk: &str| {
        sink.lock().unwrap().push(chunk.to_owned());
    }));
    let transport = StdioTransport::new(options);
    transport.start().await.expect("start");
    wait_for("stderr chunk", || {
        (!chunks.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    assert_eq!(
        chunks.lock().unwrap()[0].len(),
        20000,
        "one chunk, as in Node"
    );
    transport.close().await.expect("close");

    // 多字节字符跨 64 KiB 边界：TS 同样把它切成两个 U+FFFD（整块 lossy 解码），
    // 所以两边的 joined 文本都是 65547 个字符（含两个替换符）。
    let payload: String = format!("{}{}{}", "x".repeat(65535), 'é', "y".repeat(10));
    let encoded = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(payload.as_bytes())
    };
    let chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&chunks);
    let mut options = node_script(&format!(
        "process.stderr.write(Buffer.from('{encoded}', 'base64')); setTimeout(() => {{}}, 60_000);"
    ));
    options.on_stderr = Some(Arc::new(move |chunk: &str| {
        sink.lock().unwrap().push(chunk.to_owned());
    }));
    let transport = StdioTransport::new(options);
    transport.start().await.expect("start");
    wait_for("stderr chunks", || {
        (chunks.lock().unwrap().len() >= 2).then_some(())
    })
    .await;
    let chunks = chunks.lock().unwrap().clone();
    // 一次 64 KiB 管道读取 = 65536 字节；其中第 65536 字节是 `é` 的首字节，被整块 lossy 解码
    // 成 U+FFFD，所以首块的**字符**数正好是 65536（TS 实测同样是 65536 个字符）。
    assert_eq!(
        chunks[0].chars().count(),
        65536,
        "first chunk is one pipe read"
    );
    let joined = chunks.concat();
    assert_eq!(joined.chars().count(), 65547);
    assert_eq!(joined.matches('\u{FFFD}').count(), 2);
    transport.close().await.expect("close");
}

#[tokio::test]
async fn start_publishes_the_child_before_yielding() {
    // 评审 finding #1：并发 close() 不能在 start() 的 spawn 与 child 发布之间插队。
    // 这里串行验证可观察的部分：start() 返回的瞬间 close() 一定能看到 pid 并走关闭阶梯。
    require_node();
    for _ in 0..10 {
        let transport = StdioTransport::new(node_script(
            r#"process.on("SIGTERM", () => {}); process.stderr.write("pid " + process.pid + "\n"); setInterval(() => {}, 1000);"#,
        ));
        transport.start().await.expect("start");
        assert!(
            transport.pid().is_some(),
            "pid published when start returns"
        );
        let started = std::time::Instant::now();
        transport.close().await.expect("close");
        // 关闭阶梯跑过（SIGTERM + SIGKILL）而不是立即返回。
        assert!(started.elapsed() >= Duration::from_millis(400));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_shebang_less_executable_script_runs_through_the_shell() {
    // Node（经 libuv）在 ENOEXEC 后改用 `/bin/sh <file> <args...>` 重试，因此没有 shebang 的
    // 可执行脚本能运行（已对 TS 实测：未修复的 Rust 会直接 `spawn ENOEXEC`）。
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir();
    let path = directory.join(format!("kiana-mcp-noshebang-{}", std::process::id()));
    std::fs::write(
        &path,
        "printf '{\"jsonrpc\":\"2.0\",\"method\":\"sh\",\"params\":{\"arg\":\"%s\"}}\\n' \"$1\"\n",
    )
    .expect("write fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let mut options = StdioTransportOptions::new(path.to_string_lossy().into_owned());
    options.args = Some(vec!["hello-from-sh".to_owned()]);
    // 测试二进制里的其它用例会并发 fork；若 fork 发生在本文件写入 fd 尚未关闭时，子进程会继承
    // 该 fd，exec 就可能得到 ETXTBSY。这是测试环境的瞬时竞态，与 ENOEXEC → `/bin/sh` 的待测
    // 行为无关，短暂重试即可。`start()` 在 spawn **之前**置
    // `started`（spawn 失败也会留下），所以同一个实例不能重试——每次尝试都新建 transport。
    let mut attempts = 0;
    let (transport, collector) = loop {
        let transport = StdioTransport::new(options.clone());
        let collector = collect(&transport);
        match transport.start().await {
            Ok(()) => break (transport, collector),
            Err(error) if attempts < 40 && format!("{error:?}").contains("ETXTBSY") => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => panic!("start: {error:?}"),
        }
    };
    let message = wait_for("shell message", || {
        collector.messages.lock().unwrap().first().cloned()
    })
    .await;
    assert_eq!(message["method"], "sh");
    assert_eq!(message["params"]["arg"], "hello-from-sh");
    transport.close().await.expect("close");
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_close_cannot_leak_the_child() {
    // 评审 finding #1 的并发回归：40 组并发 start()/close()，close() 返回后子进程必须已被
    // 关闭阶梯杀掉。修复前 39/40 会留下存活的子进程（close 在 `started = true` 之后、
    // `pid` 记录之前插队，发出 close 并返回；之后的 close() 全部在 `if state.closed` 早退）。
    //
    // pid 从子进程自己写到 stderr 的 `pid N` 里取：即使 close 先跑完（此时 `pid()` 是 None），
    // 旧实现里 spawn 仍会发生，stderr 里就有那个再没人杀的 pid。
    require_node();
    let script = r#"process.on("SIGTERM", () => {}); process.stderr.write("pid " + process.pid + "\n"); setInterval(() => {}, 1000);"#;
    let mut leaked = Vec::new();
    for _ in 0..40 {
        let mut options = node_script(script);
        options.close_timeout_ms = Some(100);
        let transport = Arc::new(StdioTransport::new(options));
        let starter = Arc::clone(&transport);
        let closer = Arc::clone(&transport);
        let start = tokio::spawn(async move { starter.start().await });
        let close = tokio::spawn(async move { closer.close().await });
        let _ = start.await.expect("start task");
        let _ = close.await.expect("close task");
        // 等子进程写出 pid（若它真的被 spawn 了）。
        let mut pid = None;
        for _ in 0..100 {
            let stderr = transport.stderr();
            if let Some(index) = stderr.find("pid ") {
                pid = stderr[index + 4..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<i32>()
                    .ok();
                if pid.is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let _ = transport.close().await;
        if let Some(pid) = pid {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if unsafe { libc::kill(pid, 0) } == 0 {
                leaked.push(pid);
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
    assert!(
        leaked.is_empty(),
        "children still alive after close: {leaked:?}"
    );
}
