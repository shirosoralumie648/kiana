//! `packages/codemode/test/portable.test.ts` 的移植（spec §7.9，7 个 `it`）。
//!
//! `InlineCodemodeSandbox`（调用线程 + 中断预算）与 `RemoteCodemodeSandbox`（工具在调用方、
//! 脚本在远端）的端到端行为。远端用进程内的 [`RecordingRemote`]，每次交换的请求、消息与回答
//! 都经 `serde_json` 往返一次，模拟 Workers RPC 的 `structuredClone`。
//!
//! 需要嵌入的 `quickjs.wasm`，因此整个文件在 feature `wasm` 关闭时为空。

#![cfg(feature = "wasm")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use kiana_codemode::runtime::inline::{InlineCodemodeSandbox, InlineCodemodeSandboxOptions};
use kiana_codemode::runtime::protocol::VmStartData;
use kiana_codemode::runtime::remote::{
    CodemodeRemote, CodemodeRemoteExchange, CodemodeRemoteMessage, CodemodeRemoteReply,
    CodemodeRemoteRequest, CodemodeServeOptions, RemoteCodemodeSandbox,
    RemoteCodemodeSandboxOptions, serve_codemode_remote,
};
use kiana_codemode::types::{
    BoxFuture, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeResult, CodemodeResultErr,
    CodemodeResultOk, CodemodeSandboxBaseOptions, CodemodeTool, CodemodeToolContext,
};
use kiana_codemode::wasm::{CodemodeWasmModule, load_quickjs_wasm};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 脚手架
// ---------------------------------------------------------------------------

/// `wasm`：模块级只加载一次（TS: `const wasm = loadQuickJSWasm()`）。
async fn wasm() -> CodemodeWasmModule {
    load_quickjs_wasm(None)
        .await
        .expect("the embedded quickjs.wasm must load")
}

fn expect_failure(result: &CodemodeResult) -> &CodemodeResultErr {
    match result {
        CodemodeResult::Err(error) => error,
        CodemodeResult::Ok(ok) => panic!("expected a failure result, got {ok:?}"),
    }
}

fn expect_success(result: &CodemodeResult) -> &CodemodeResultOk {
    match result {
        CodemodeResult::Ok(ok) => ok,
        CodemodeResult::Err(error) => panic!("expected a success result, got {error:?}"),
    }
}

fn tool(
    name: &str,
    execute: impl Fn(Value, CodemodeToolContext) -> BoxFuture<'static, Result<Value, String>>
    + Send
    + Sync
    + 'static,
) -> CodemodeTool {
    CodemodeTool::new(name, Arc::new(execute))
}

fn inline_sandbox(
    wasm: &CodemodeWasmModule,
    base: CodemodeSandboxBaseOptions,
    interrupt_budget: Option<u32>,
) -> InlineCodemodeSandbox {
    InlineCodemodeSandbox::new(InlineCodemodeSandboxOptions {
        base,
        wasm: wasm.clone(),
        interrupt_budget,
    })
    .expect("valid inline sandbox options")
}

/// `structuredClone(request)`：Workers RPC 的深拷贝。
fn clone_request(request: CodemodeRemoteRequest) -> CodemodeRemoteRequest {
    serde_json::from_value(serde_json::to_value(request).expect("request serializes"))
        .expect("request clones")
}

/// `structuredClone(messages)`。
fn clone_messages(messages: Vec<CodemodeRemoteMessage>) -> Vec<CodemodeRemoteMessage> {
    serde_json::from_value(serde_json::to_value(messages).expect("messages serialize"))
        .expect("messages clone")
}

/// `structuredClone(reply)`。
fn clone_reply(reply: CodemodeRemoteReply) -> CodemodeRemoteReply {
    serde_json::from_value(serde_json::to_value(reply).expect("reply serializes"))
        .expect("reply clones")
}

/// 进程内远端，记录跨界的东西（TS: `recordingRemote`）。
///
/// `exchanges` 记录每次交换的消息数；`served` 是每个 serve 完成时触发的一次性信号（TS 的
/// `served: Promise<void>[]`，测试用 `await Promise.all(served)` 等远端收尾）。
struct RecordingRemote {
    wasm: CodemodeWasmModule,
    max_interrupt_budget: Option<u32>,
    max_memory_limit_bytes: Option<f64>,
    exchanges: Arc<Mutex<Vec<usize>>>,
    served: Mutex<Vec<oneshot::Receiver<()>>>,
}

impl RecordingRemote {
    fn new(
        wasm: CodemodeWasmModule,
        max_interrupt_budget: Option<u32>,
        max_memory_limit_bytes: Option<f64>,
    ) -> Arc<Self> {
        Arc::new(RecordingRemote {
            wasm,
            max_interrupt_budget,
            max_memory_limit_bytes,
            exchanges: Arc::new(Mutex::new(Vec::new())),
            served: Mutex::new(Vec::new()),
        })
    }

    fn exchange_count(&self) -> usize {
        self.exchanges.lock().expect("exchanges").len()
    }

    /// 等每个 serve 完成（TS: `await Promise.all(served)`）。
    async fn wait_served(&self) {
        let receivers: Vec<oneshot::Receiver<()>> =
            std::mem::take(&mut *self.served.lock().expect("served"));
        for receiver in receivers {
            tokio::time::timeout(Duration::from_secs(20), receiver)
                .await
                .expect("the remote must finish serving")
                .expect("the serve signal must fire");
        }
    }
}

impl CodemodeRemote for RecordingRemote {
    fn execute(
        &self,
        request: CodemodeRemoteRequest,
        exchange: CodemodeRemoteExchange,
    ) -> BoxFuture<'static, Result<(), String>> {
        let wasm = self.wasm.clone();
        let max_interrupt_budget = self.max_interrupt_budget;
        let max_memory_limit_bytes = self.max_memory_limit_bytes;
        let exchanges = Arc::clone(&self.exchanges);
        let (served_tx, served_rx) = oneshot::channel();
        self.served.lock().expect("served").push(served_rx);
        Box::pin(async move {
            // `structuredClone(request)`，并让每条消息与每个回答都过一次 Workers RPC 的深拷贝。
            let request = clone_request(request);
            let wrapped: CodemodeRemoteExchange = {
                let exchange = Arc::clone(&exchange);
                let exchanges = Arc::clone(&exchanges);
                Arc::new(move |messages: Vec<CodemodeRemoteMessage>| {
                    let exchange = Arc::clone(&exchange);
                    let exchanges = Arc::clone(&exchanges);
                    Box::pin(async move {
                        exchanges.lock().expect("exchanges").push(messages.len());
                        let reply = (exchange)(clone_messages(messages)).await;
                        clone_reply(reply)
                    })
                })
            };
            serve_codemode_remote(
                request,
                wrapped,
                CodemodeServeOptions {
                    wasm,
                    max_interrupt_budget,
                    max_memory_limit_bytes,
                },
            )
            .await;
            let _ = served_tx.send(());
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// InlineCodemodeSandbox
// ---------------------------------------------------------------------------

/// TS: `fails a script that computes past its interrupt budget as a timeout`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_a_script_that_computes_past_its_interrupt_budget_as_a_timeout() {
    let wasm = wasm().await;
    let sandbox = inline_sandbox(&wasm, CodemodeSandboxBaseOptions::default(), Some(1_000));
    for code in ["while (true) {}", "while (true) await null"] {
        let result = sandbox
            .execute(
                code,
                CodemodeExecuteOptions {
                    timeout_ms: Some(f64::INFINITY),
                    ..Default::default()
                },
            )
            .await
            .expect("sandbox is open");
        let error = expect_failure(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Timeout, "{code}");
        assert_eq!(
            error.error.message, "Execution exceeded its interrupt budget of 1000",
            "{code}"
        );
    }
}

/// TS: `counts the budget across waits on tools`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn counts_the_budget_across_waits_on_tools() {
    let wasm = wasm().await;
    let sandbox = inline_sandbox(
        &wasm,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![tool("tick", |_args, _context| {
                Box::pin(async { Ok(Value::from(1)) })
            })]),
            ..Default::default()
        },
        Some(1_000),
    );
    let result = sandbox
        .execute(
            "for (;;) { for (let i = 0; i < 1e5; i++); await tools.tick(); }",
            CodemodeExecuteOptions::default(),
        )
        .await
        .expect("sandbox is open");
    let error = expect_failure(&result);
    assert_eq!(error.error.kind, CodemodeErrorKind::Timeout);
    assert!(
        error.calls.len() > 1,
        "the budget is shared across tool waits, got {:?}",
        error.calls
    );
}

// ---------------------------------------------------------------------------
// RemoteCodemodeSandbox
// ---------------------------------------------------------------------------

fn remote_sandbox(
    recording: &Arc<RecordingRemote>,
    base: CodemodeSandboxBaseOptions,
    interrupt_budget: Option<u32>,
) -> RemoteCodemodeSandbox {
    let remote = Arc::clone(recording);
    RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        base,
        remote: Arc::new(move || Arc::clone(&remote) as Arc<dyn CodemodeRemote>),
        interrupt_budget,
    })
    .expect("valid remote sandbox options")
}

/// TS: `runs tools in the caller and the script on the remote`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_tools_in_the_caller_and_the_script_on_the_remote() {
    let wasm = wasm().await;
    let recording = RecordingRemote::new(wasm, None, None);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_tool = Arc::clone(&seen);
    let sandbox = remote_sandbox(
        &recording,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![tool("add", move |args, _context| {
                seen_by_tool.lock().expect("seen").push(args.clone());
                let a = args
                    .get("a")
                    .and_then(Value::as_f64)
                    .expect("a is a number");
                Box::pin(async move { Ok(Value::from(a + 1.0)) })
            })]),
            ..Default::default()
        },
        None,
    );
    let result = sandbox
        .execute(
            "text('hi'); return await tools.add({ a: 1 })",
            CodemodeExecuteOptions {
                store: Some(indexmap::IndexMap::from([(
                    "k".to_string(),
                    Value::from(1),
                )])),
                ..Default::default()
            },
        )
        .await
        .expect("sandbox is open");
    let ok = expect_success(&result);
    assert_eq!(ok.value, Some(Value::from(2)));
    assert_eq!(ok.output.len(), 1);
    assert_eq!(
        ok.output[0],
        kiana_codemode::types::CodemodeOutputItem::text("hi")
    );
    assert_eq!(
        *seen.lock().expect("seen"),
        vec![serde_json::json!({ "a": 1 })]
    );
    assert_eq!(ok.calls.len(), 1);
    assert_eq!(ok.calls[0].name, "add");
    assert_eq!(
        ok.calls[0].status,
        kiana_codemode::types::CodemodeCallStatus::Ok
    );
    recording.wait_served().await;
}

/// TS: `batches output instead of making one exchange per line`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batches_output_instead_of_making_one_exchange_per_line() {
    let wasm = wasm().await;
    let recording = RecordingRemote::new(wasm, None, None);
    let sandbox = remote_sandbox(&recording, CodemodeSandboxBaseOptions::default(), None);
    let result = sandbox
        .execute(
            "for (let i = 0; i < 5000; i++) console.log(i); return 'ok'",
            CodemodeExecuteOptions::default(),
        )
        .await
        .expect("sandbox is open");
    let ok = expect_success(&result);
    assert_eq!(ok.value, Some(Value::from("ok")));
    assert_eq!(ok.output.len(), 5000);
    assert_eq!(
        ok.output[4999],
        kiana_codemode::types::CodemodeOutputItem::console("4999")
    );
    assert!(
        recording.exchange_count() <= 6,
        "output must be batched, got {} exchanges",
        recording.exchange_count()
    );
}

/// TS: `caps the interrupt budget and memory at the remote's limits`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caps_the_interrupt_budget_and_memory_at_the_remote_limits() {
    let wasm = wasm().await;
    let recording = RecordingRemote::new(wasm, Some(1_000), Some((8 * 1024 * 1024) as f64));
    let sandbox = remote_sandbox(
        &recording,
        CodemodeSandboxBaseOptions::default(),
        Some(1_000_000_000),
    );
    let crashed = sandbox
        .execute("while (true) {}", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    let error = expect_failure(&crashed);
    assert_eq!(error.error.kind, CodemodeErrorKind::Timeout);
    assert_eq!(
        error.error.message,
        "Execution exceeded its interrupt budget of 1000"
    );

    let memory = sandbox
        .execute(
            "const parts = []; for (;;) parts.push('x'.repeat(1 << 20) + parts.length);",
            CodemodeExecuteOptions::default(),
        )
        .await
        .expect("sandbox is open");
    assert!(matches!(memory, CodemodeResult::Err(_)));
    assert!(
        serde_json::to_string(&memory)
            .expect("the result serializes")
            .contains("out of memory"),
        "the failure must surface the VM's out-of-memory error: {memory:?}"
    );
}

/// TS: `tells the remote to stop when the caller aborts`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tells_the_remote_to_stop_when_the_caller_aborts() {
    let wasm = wasm().await;
    let recording = RecordingRemote::new(wasm, None, None);
    let started = Arc::new(tokio::sync::Notify::new());
    let notified = Arc::clone(&started);
    let sandbox = Arc::new(remote_sandbox(
        &recording,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![tool("hang", move |_args, _context| {
                notified.notify_one();
                Box::pin(std::future::pending::<Result<Value, String>>())
            })]),
            ..Default::default()
        },
        None,
    ));
    let token = CancellationToken::new();
    let pending = {
        let sandbox = Arc::clone(&sandbox);
        let token = token.clone();
        tokio::spawn(async move {
            sandbox
                .execute(
                    "await tools.hang(); return 'never'",
                    CodemodeExecuteOptions {
                        signal: Some(token),
                        ..Default::default()
                    },
                )
                .await
        })
    };
    started.notified().await;
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(20), pending)
        .await
        .expect("the aborted execution must settle")
        .expect("the execution task must not panic")
        .expect("sandbox is open");
    assert_eq!(
        expect_failure(&result).error.kind,
        CodemodeErrorKind::Aborted
    );
    // 远端未决的 exchange 被 `{ type: "stop" }` 回答，因此它的 VM 被 dispose、execute() 返回。
    recording.wait_served().await;
}

/// TS: `reports a failing or vanishing remote as a sandbox error`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_failing_or_vanishing_remote_as_a_sandbox_error() {
    struct Failing;
    impl CodemodeRemote for Failing {
        fn execute(
            &self,
            _request: CodemodeRemoteRequest,
            _exchange: CodemodeRemoteExchange,
        ) -> BoxFuture<'static, Result<(), String>> {
            Box::pin(async { Err("object reset".to_string()) })
        }
    }
    struct Silent;
    impl CodemodeRemote for Silent {
        fn execute(
            &self,
            _request: CodemodeRemoteRequest,
            _exchange: CodemodeRemoteExchange,
        ) -> BoxFuture<'static, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    let failing = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions::default(),
        remote: Arc::new(|| Arc::new(Failing) as Arc<dyn CodemodeRemote>),
        interrupt_budget: None,
    })
    .expect("valid options");
    let result = failing
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    let error = expect_failure(&result);
    assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox);
    assert_eq!(error.error.message, "Remote sandbox failed: object reset");

    let silent = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions::default(),
        remote: Arc::new(|| Arc::new(Silent) as Arc<dyn CodemodeRemote>),
        interrupt_budget: None,
    })
    .expect("valid options");
    let result = silent
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    let error = expect_failure(&result);
    assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox);
    assert_eq!(
        error.error.message,
        "Remote sandbox returned before the script settled"
    );

    let unreachable = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions::default(),
        remote: Arc::new(|| -> Arc<dyn CodemodeRemote> { panic!("no binding") }),
        interrupt_budget: None,
    })
    .expect("valid options");
    let result = unreachable
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    let error = expect_failure(&result);
    assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox);
    assert_eq!(
        error.error.message,
        "Failed to reach the remote sandbox: no binding"
    );
}

/// spec §A.5.1（Rust 回归）：远端 VM 在同步计算期间不产生 `exchange`。
///
/// TS 的 serve 把 VM 跑在调用线程上，`onMessage` 由微任务驱动：脚本计算期间宿主事件循环不跑，
/// 输出缓冲的冲刷（1000 项）也要等 VM 让出之后才发生。Rust 的转发任务若在 VM 计算时就把消息
/// 交给宿主，客户端会在脚本还在计算时收到 `exchange`，并在它已经结束后回答 `stop`——与
/// 「远端会继续计算到下一次工具调用或预算耗尽」不符（评审发现的次要偏差）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defers_remote_exchanges_until_the_script_yields() {
    let wasm = wasm().await;
    let exchanges = Arc::new(Mutex::new(Vec::<usize>::new()));
    let exchange: CodemodeRemoteExchange = {
        let exchanges = Arc::clone(&exchanges);
        Arc::new(move |messages: Vec<CodemodeRemoteMessage>| {
            let exchanges = Arc::clone(&exchanges);
            Box::pin(async move {
                exchanges.lock().expect("exchanges").push(messages.len());
                CodemodeRemoteReply::Null
            })
        })
    };
    let serve = tokio::spawn(serve_codemode_remote(
        CodemodeRemoteRequest {
            data: VmStartData {
                code: "for (let i = 0; i < 2000; i++) console.log(i); \
                       const t = performance.now(); while (performance.now() - t < 2000) {} return 'done'"
                    .to_string(),
                tools: Vec::new(),
                globals: Vec::new(),
                memory_limit_bytes: None,
                store: IndexMap::new(),
                deterministic: None,
            },
            interrupt_budget: Some(100_000),
        },
        exchange,
        CodemodeServeOptions {
            wasm,
            max_interrupt_budget: None,
            max_memory_limit_bytes: None,
        },
    ));

    // 脚本在这段时间里只做计算：一个 exchange 都不该发出（TS 的微任务/定时器都跑不了）。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        exchanges.lock().expect("exchanges").is_empty(),
        "the remote must not exchange while the script computes: {:?}",
        exchanges.lock().expect("exchanges")
    );

    tokio::time::timeout(Duration::from_secs(30), serve)
        .await
        .expect("the remote must finish serving")
        .expect("serve must not panic");
    assert!(
        !exchanges.lock().expect("exchanges").is_empty(),
        "the buffered output and the terminal message must be exchanged once the VM yields"
    );
}
