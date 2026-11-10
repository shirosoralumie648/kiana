//! `packages/codemode/test/sandbox.test.ts` 的移植：crate 级验收套件。
//!
//! 每个行为测试都在 VM 能跑的三处各跑一次（TS 的 `describe.each(Object.keys(BACKENDS))`）：
//! worker（Node）、inline（调用线程）与 remote（进程内 serve，每条消息 `structuredClone`，
//! 与 Workers RPC 一致）。两个在调用线程上的后端用小中断预算，让失控脚本很快结束。
//!
//! `worker host` 与 `embedded sources` 两组只在 worker 上跑一次（与 TS 相同）。
//!
//! 需要嵌入的 `quickjs.wasm`，因此整个文件在 feature `wasm` 关闭时为空。

#![cfg(feature = "wasm")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use kiana_codemode::runtime::execution::{
    CodemodeSandboxBase, StartError, VmChannel, VmEvents, VmLauncher,
};
use kiana_codemode::runtime::host::{
    CodemodeSandbox, WorkerHandle, WorkerObserver, WorkerSpawner, WorkerWasm,
};
use kiana_codemode::runtime::inline::{
    InlineCodemodeSandbox, InlineCodemodeSandboxOptions, inline_launcher,
};
use kiana_codemode::runtime::protocol::{HostToWorkerMessage, VmStartData};
use kiana_codemode::runtime::remote::{
    CodemodeRemote, CodemodeRemoteExchange, CodemodeRemoteMessage, CodemodeRemoteReply,
    CodemodeRemoteRequest, CodemodeServeOptions, RemoteCodemodeSandbox,
    RemoteCodemodeSandboxOptions, serve_codemode_remote,
};
use kiana_codemode::types::{
    BoxFuture, CodemodeCallStatus, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem,
    CodemodeResult, CodemodeResultErr, CodemodeResultOk, CodemodeSandboxBaseOptions,
    CodemodeSandboxOptions, CodemodeTool, CodemodeToolContext,
};
use kiana_codemode::wasm::{CodemodeWasmModule, load_quickjs_wasm};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// TS: `const INTERRUPT_BUDGET = 20_000`（inline 与 remote 的预算）。
const INTERRUPT_BUDGET: u32 = 20_000;

/// `image()` 只检查签名，所以测试只需要每种格式的开头几个字节的 base64。
const PNG: &str = "iVBORw0KGgo=";
const JPEG: &str = "/9j/4A==";
const GIF: &str = "R0lGODlh";
const WEBP: &str = "UklGRgAAAABXRUJQ";

// ---------------------------------------------------------------------------
// 后端
// ---------------------------------------------------------------------------

/// TS: `BACKENDS`（`worker`、`inline`、`remote`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    Worker,
    Inline,
    Remote,
}

const BACKENDS: [Backend; 3] = [Backend::Worker, Backend::Inline, Backend::Remote];

/// 一个 sandbox 句柄；三个后端都经由 [`CodemodeSandboxBase`]。
enum Sandbox {
    Worker(CodemodeSandbox),
    Inline(InlineCodemodeSandbox),
    Remote(RemoteCodemodeSandbox),
}

impl Sandbox {
    fn base(&self) -> &Arc<CodemodeSandboxBase> {
        match self {
            Sandbox::Worker(sandbox) => sandbox.base(),
            Sandbox::Inline(sandbox) => sandbox.base(),
            Sandbox::Remote(sandbox) => sandbox.base(),
        }
    }

    async fn close(&self) {
        match self {
            Sandbox::Worker(sandbox) => sandbox.close().await,
            Sandbox::Inline(sandbox) => sandbox.close().await,
            Sandbox::Remote(sandbox) => sandbox.close().await,
        }
    }
}

async fn wasm() -> CodemodeWasmModule {
    load_quickjs_wasm(None)
        .await
        .expect("the embedded quickjs.wasm must load")
}

/// `structuredClone` 的等价物（Workers RPC 的深拷贝）。
fn clone_request(request: CodemodeRemoteRequest) -> CodemodeRemoteRequest {
    serde_json::from_value(serde_json::to_value(request).expect("request serializes"))
        .expect("request clones")
}

fn clone_messages(messages: Vec<CodemodeRemoteMessage>) -> Vec<CodemodeRemoteMessage> {
    serde_json::from_value(serde_json::to_value(messages).expect("messages serialize"))
        .expect("messages clone")
}

fn clone_reply(reply: CodemodeRemoteReply) -> CodemodeRemoteReply {
    serde_json::from_value(serde_json::to_value(reply).expect("reply serializes"))
        .expect("reply clones")
}

/// 远端后端：进程内 serve；每条消息经 `structuredClone`。
struct ServeRemote {
    wasm: CodemodeWasmModule,
}

impl CodemodeRemote for ServeRemote {
    fn execute(
        &self,
        request: CodemodeRemoteRequest,
        exchange: CodemodeRemoteExchange,
    ) -> BoxFuture<'static, Result<(), String>> {
        let wasm = self.wasm.clone();
        Box::pin(async move {
            let wrapped: CodemodeRemoteExchange = Arc::new(move |messages| {
                let exchange = Arc::clone(&exchange);
                Box::pin(async move { clone_reply((exchange)(clone_messages(messages)).await) })
            });
            serve_codemode_remote(
                clone_request(request),
                wrapped,
                CodemodeServeOptions {
                    wasm,
                    max_interrupt_budget: Some(INTERRUPT_BUDGET),
                    max_memory_limit_bytes: None,
                },
            )
            .await;
            Ok(())
        })
    }
}

/// 建一个后端 sandbox；构造选项非法时返回错误（TS 的构造函数抛错）。
async fn try_new_sandbox(
    backend: Backend,
    base: CodemodeSandboxBaseOptions,
) -> Result<Sandbox, StartError> {
    let wasm = wasm().await;
    match backend {
        Backend::Worker => {
            // 与 deterministic 套件相同：先取到缓存的模块再交给 worker launcher，避免第一个
            // 执行在 launcher 里等一次十几秒的编译（TS 的 `loadQuickJSWasm()` 缓存与这里等价）。
            CodemodeSandbox::new(CodemodeSandboxOptions {
                base,
                wasm: Some(wasm),
                ..Default::default()
            })
            .map(Sandbox::Worker)
        }
        Backend::Inline => InlineCodemodeSandbox::new(InlineCodemodeSandboxOptions {
            base,
            wasm,
            interrupt_budget: Some(INTERRUPT_BUDGET),
        })
        .map(Sandbox::Inline),
        Backend::Remote => RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
            base,
            remote: Arc::new(move || {
                Arc::new(ServeRemote { wasm: wasm.clone() }) as Arc<dyn CodemodeRemote>
            }),
            interrupt_budget: Some(INTERRUPT_BUDGET),
        })
        .map(Sandbox::Remote),
    }
}

async fn new_sandbox(backend: Backend, base: CodemodeSandboxBaseOptions) -> Sandbox {
    try_new_sandbox(backend, base)
        .await
        .expect("valid sandbox options")
}

/// TS: `createSandbox(tools = [], timeoutMs = 10_000)`。
fn create_options(tools: Vec<CodemodeTool>, timeout_ms: f64) -> CodemodeSandboxBaseOptions {
    CodemodeSandboxBaseOptions {
        tools: Some(tools),
        timeout_ms: Some(timeout_ms),
        ..Default::default()
    }
}

async fn create_sandbox(backend: Backend, tools: Vec<CodemodeTool>) -> Sandbox {
    new_sandbox(backend, create_options(tools, 10_000.0)).await
}

// ---------------------------------------------------------------------------
// 工具与结果辅助
// ---------------------------------------------------------------------------

fn tool<F>(name: &str, execute: F) -> CodemodeTool
where
    F: Fn(Value, CodemodeToolContext) -> BoxFuture<'static, Result<Value, String>>
        + Send
        + Sync
        + 'static,
{
    CodemodeTool::new(name, Arc::new(execute))
}

/// TS: `const echo: CodemodeTool = { name: "echo", execute: (args) => args }`。
fn echo() -> CodemodeTool {
    tool("echo", |args, _context| Box::pin(async move { Ok(args) }))
}

fn described(name: &str, description: &str, value: &'static str) -> CodemodeTool {
    CodemodeTool {
        description: Some(description.to_string()),
        ..tool(name, move |_args, _context| {
            Box::pin(async move { Ok(Value::from(value)) })
        })
    }
}

async fn run(sandbox: &Sandbox, code: &str) -> CodemodeResult {
    sandbox
        .base()
        .execute(code, CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open")
}

async fn run_with(
    sandbox: &Sandbox,
    code: &str,
    options: CodemodeExecuteOptions,
) -> CodemodeResult {
    sandbox
        .base()
        .execute(code, options)
        .await
        .expect("sandbox is open")
}

/// 执行 `return <expression>;`：成功给值，失败给错误消息。
async fn attempt(sandbox: &Sandbox, expression: &str) -> Value {
    let result = run(sandbox, &format!("return {expression};")).await;
    match result {
        CodemodeResult::Ok(ok) => ok.value.unwrap_or(Value::Null),
        CodemodeResult::Err(error) => Value::from(error.error.message),
    }
}

fn ok_of(result: &CodemodeResult) -> &CodemodeResultOk {
    match result {
        CodemodeResult::Ok(ok) => ok,
        CodemodeResult::Err(error) => panic!("expected a success result, got {error:?}"),
    }
}

fn err_of(result: &CodemodeResult) -> &CodemodeResultErr {
    match result {
        CodemodeResult::Err(error) => error,
        CodemodeResult::Ok(ok) => panic!("expected a failure, got {ok:?}"),
    }
}

fn value_of(result: &CodemodeResult) -> Value {
    ok_of(result)
        .value
        .clone()
        .expect("the script returned a value")
}

fn call_names(result: &CodemodeResult) -> Vec<String> {
    match result {
        CodemodeResult::Ok(ok) => ok.calls.iter().map(|call| call.name.clone()).collect(),
        CodemodeResult::Err(error) => error.calls.iter().map(|call| call.name.clone()).collect(),
    }
}

fn call_statuses(result: &CodemodeResult) -> Vec<(String, CodemodeCallStatus)> {
    let calls = match result {
        CodemodeResult::Ok(ok) => &ok.calls,
        CodemodeResult::Err(error) => &error.calls,
    };
    calls
        .iter()
        .map(|call| (call.name.clone(), call.status))
        .collect()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(|item| item.as_str().expect("a string").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// embedded sources
// ---------------------------------------------------------------------------

/// `parse as JavaScript`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_sources_parse_as_javascript() {
    // TS: `new vm.Script(PRELUDE_SOURCE)`。Rust 侧让 VM 自己编译同一份源码：`new Function`
    // 只编译不执行，语法错误会抛出——与 `vm.Script` 的解析语义一致。
    let source = kiana_codemode::runtime::prelude_source::PRELUDE_SOURCE;
    let literal = serde_json::to_string(source).expect("the prelude serializes");
    let sandbox = create_sandbox(Backend::Inline, vec![]).await;
    let result = run(
        &sandbox,
        &format!("new Function({literal}); return 'parsed'"),
    )
    .await;
    assert_eq!(value_of(&result), json!("parsed"));
    sandbox.close().await;
}

// ---------------------------------------------------------------------------
// script execution
// ---------------------------------------------------------------------------

/// `returns the script's return value after a JSON round trip`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_the_scripts_return_value_after_a_json_round_trip() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "return { a: 1, b: [true, 'x'] }").await;
        let ok = ok_of(&result);
        assert_eq!(
            ok.value,
            Some(json!({ "a": 1, "b": [true, "x"] })),
            "{backend:?}"
        );
        assert!(ok.output.is_empty(), "{backend:?}");
        assert!(ok.calls.is_empty(), "{backend:?}");
        let plain = run(&sandbox, "return 'plain'").await;
        assert_eq!(value_of(&plain), json!("plain"), "{backend:?}");
        let empty = run(&sandbox, "").await;
        // TS: `value: undefined`。
        assert!(ok_of(&empty).value.is_none(), "{backend:?}");
        sandbox.close().await;
    }
}

/// `supports top-level await`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supports_top_level_await() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            "const x = await Promise.resolve(41); return x + 1",
        )
        .await;
        assert_eq!(value_of(&result), json!(42), "{backend:?}");
        sandbox.close().await;
    }
}

/// `collects text(), image(), and console output in order`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collects_text_image_and_console_output_in_order() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            &format!(
                r#"
				console.log("hello", 1, {{ a: 1 }});
				text({{ json: true }});
				text(undefined);
				text(7);
				image("data:image/png;base64,{PNG}");
				image({{ image_url: "data:image/jpeg;base64,{JPEG}" }});
				image({{ type: "image", data: "{GIF}", mimeType: "image/gif" }});
				image("data:image/png;base64,{WEBP}");
				image({{ type: "image", data: "{PNG}" }});
				console.error(new Error("bad"));
				return null;
			"#
            ),
        )
        .await;
        let ok = ok_of(&result);
        assert_eq!(ok.output.len(), 10, "{backend:?}");
        let expected = vec![
            CodemodeOutputItem::console("hello 1 {\"a\":1}"),
            CodemodeOutputItem::text("{\"json\":true}"),
            CodemodeOutputItem::text("undefined"),
            CodemodeOutputItem::text("7"),
            CodemodeOutputItem::image(PNG, "image/png"),
            CodemodeOutputItem::image(JPEG, "image/jpeg"),
            CodemodeOutputItem::image(GIF, "image/gif"),
            // MIME 类型来自数据签名，而不是声明的类型。
            CodemodeOutputItem::image(WEBP, "image/webp"),
            CodemodeOutputItem::image(PNG, "image/png"),
        ];
        assert_eq!(&ok.output[..9], &expected[..], "{backend:?}");
        match &ok.output[9] {
            CodemodeOutputItem::Text { text, console } => {
                assert_eq!(console, &Some(true), "{backend:?}");
                assert!(text.starts_with("Error: bad"), "{backend:?}: {text}");
            }
            other => panic!("{backend:?}: expected a console text item, got {other:?}"),
        }
        sandbox.close().await;
    }
}

/// `rejects invalid text() and image() arguments`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_text_and_image_arguments() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            r#"
				const errors = [];
				const circular = {};
				circular.self = circular;
				for (const run of [
					() => text(circular),
					() => image(""),
					() => image("https://example.com/a.png"),
					() => image("data:image/png,raw"),
					() => image({ type: "text", text: "x" }),
					() => image({ type: "image", data: "" }),
					() => image(42),
					() => image("data:image/png;base64,AAAA!"),
					() => image("data:image/png;base64,AAAAA"),
					() => image("data:image/png;base64,AA=A"),
					() => image("data:image/png;base64,"),
					() => image("data:image/png;base64,AAAA\n[Output truncated]"),
					() => image({ type: "image", data: "AAAA!", mimeType: "image/png" }),
					() => image("data:image/png;base64,AAAA"),
					() => image("data:image/png;base64,QUJD"),
					() => image("data:image/jpeg;base64,/9j/9w=="),
				]) {
					try { run(); errors.push("no error"); } catch (error) { errors.push(error.name + ": " + error.message); }
				}
				return errors;
			"#,
        )
        .await;
        let ok = ok_of(&result);
        assert!(ok.output.is_empty(), "{backend:?}");
        let errors = strings(&value_of(&result));
        assert_eq!(errors.len(), 16, "{backend:?}");
        assert!(
            errors[0].starts_with("TypeError: ") && errors[0].contains("circular"),
            "{backend:?}: {}",
            errors[0]
        );
        let expected = vec![
            "TypeError: image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }",
            "TypeError: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead",
            "TypeError: invalid image output. Pass a base64 data URI instead",
            "TypeError: image only accepts image blocks, got \"text\"",
            "TypeError: image expected image block data",
            "TypeError: image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }",
        ];
        let mut expected: Vec<String> = expected.into_iter().map(str::to_string).collect();
        for _ in 0..6 {
            expected.push(
                "TypeError: invalid image output. The image data is not valid base64 (truncated or corrupted?)"
                    .to_string(),
            );
        }
        for _ in 0..3 {
            expected.push(
                "TypeError: invalid image output. The image data is not a PNG, JPEG, GIF, or WebP image"
                    .to_string(),
            );
        }
        assert_eq!(&errors[1..], &expected[..], "{backend:?}");
        sandbox.close().await;
    }
}

/// `accepts wrapped and large base64 image data`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepts_wrapped_and_large_base64_image_data() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let large = format!("iVBORw0KGgoA{}", "QUJD".repeat(256 * 1024));
        let result = run(
            &sandbox,
            &format!(
                r#"
				image("data:image/png;base64,iVBORw0K\r\nGgo=\n");
				image("data:image/png;base64,{large}");
			"#
            ),
        )
        .await;
        let ok = ok_of(&result);
        assert_eq!(
            ok.output,
            vec![
                CodemodeOutputItem::image(PNG, "image/png"),
                CodemodeOutputItem::image(large, "image/png"),
            ],
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `ends the script successfully on exit(), keeping output and store writes`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ends_the_script_successfully_on_exit_keeping_output_and_store_writes() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![echo()]).await;
        let result = run(
            &sandbox,
            r#"
				text("before");
				store("k", 1);
				await tools.echo(1);
				try { exit(); } catch {}
				text("after");
				return "unreachable";
			"#,
        )
        .await;
        let ok = ok_of(&result);
        assert_eq!(ok.value, None, "{backend:?}");
        assert_eq!(
            ok.output,
            vec![CodemodeOutputItem::text("before")],
            "{backend:?}"
        );
        let mut set = IndexMap::new();
        set.insert("k".to_string(), json!(1));
        assert_eq!(ok.store_writes.set, set, "{backend:?}");
        assert!(ok.store_writes.delete.is_empty(), "{backend:?}");
        sandbox.close().await;
    }
}

/// `keeps output produced before a failure`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_output_produced_before_a_failure() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "text(\"partial\");\nthrow new Error(\"boom\")").await;
        let error = err_of(&result);
        assert_eq!(
            error.output,
            vec![CodemodeOutputItem::text("partial")],
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `reports syntax errors with the script's line number`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_syntax_errors_with_the_scripts_line_number() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "const a = 1;\nconst b = ;\nreturn a").await;
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
        assert_eq!(
            error.error.name.as_deref(),
            Some("SyntaxError"),
            "{backend:?}"
        );
        let stack = error.error.stack.as_deref().expect("a stack");
        assert!(stack.contains("codemode.js:2"), "{backend:?}: {stack}");
        sandbox.close().await;
    }
}

/// `reports thrown errors with the script's line number`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_thrown_errors_with_the_scripts_line_number() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "const a = 1;\nthrow new TypeError('boom ' + a)").await;
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
        assert_eq!(
            error.error.name.as_deref(),
            Some("TypeError"),
            "{backend:?}"
        );
        assert_eq!(error.error.message, "boom 1", "{backend:?}");
        let stack = error.error.stack.as_deref().expect("a stack");
        assert!(stack.contains("codemode.js:2"), "{backend:?}: {stack}");
        sandbox.close().await;
    }
}

/// `formats stacks like V8 without prelude frames`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn formats_stacks_like_v8_without_prelude_frames() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            "console.log(new Error('inner'));\nthrow new RangeError('outer')",
        )
        .await;
        let error = err_of(&result);
        let stack = error.error.stack.as_deref().expect("a stack");
        assert!(
            stack.starts_with("RangeError: outer\n    at "),
            "{backend:?}: {stack}"
        );
        assert!(
            stack
                .lines()
                .nth(1)
                .is_some_and(|line| line.contains("codemode.js:2")),
            "{backend:?}: {stack}"
        );
        assert!(
            !stack.contains("codemode-prelude.js"),
            "{backend:?}: {stack}"
        );
        match &error.output[0] {
            CodemodeOutputItem::Text { text, console } => {
                assert_eq!(console, &Some(true), "{backend:?}");
                assert!(
                    text.starts_with("Error: inner\n    at "),
                    "{backend:?}: {text}"
                );
                assert!(
                    text.lines()
                        .nth(1)
                        .is_some_and(|line| line.contains("codemode.js:1")),
                    "{backend:?}: {text}"
                );
            }
            other => panic!("{backend:?}: expected a text item, got {other:?}"),
        }
        let printed = serde_json::to_string(&error.output).expect("output serializes");
        assert!(!printed.contains("codemode-prelude.js"), "{backend:?}");
        sandbox.close().await;
    }
}

/// `reports non-Error throws`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_non_error_throws() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "throw { code: 7 }").await;
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
        assert_eq!(error.error.message, "{\"code\":7}", "{backend:?}");
        sandbox.close().await;
    }
}

/// `reports a non-serializable return value as a script error`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_non_serializable_return_value_as_a_script_error() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "return 10n").await;
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
        assert_eq!(
            error.error.name.as_deref(),
            Some("TypeError"),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

// ---------------------------------------------------------------------------
// tools
// ---------------------------------------------------------------------------

/// `exposes tools as async functions and records calls`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exposes_tools_as_async_functions_and_records_calls() {
    for backend in BACKENDS {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let add = {
            let seen = Arc::clone(&seen);
            tool("add", move |args, _context| {
                seen.lock().expect("seen mutex").push(args.clone());
                Box::pin(async move {
                    let a = args["a"].as_f64().expect("a");
                    let b = args["b"].as_f64().expect("b");
                    Ok(json!({ "sum": a + b }))
                })
            })
        };
        let sandbox = create_sandbox(backend, vec![add]).await;
        let result = run(
            &sandbox,
            r#"
				const first = await tools.add({ a: 1, b: 2 });
				const second = await tools.add({ a: first.sum, b: 10 });
				return second.sum;
			"#,
        )
        .await;
        assert_eq!(value_of(&result), json!(13), "{backend:?}");
        assert_eq!(
            *seen.lock().expect("seen mutex"),
            vec![json!({ "a": 1, "b": 2 }), json!({ "a": 3, "b": 10 })],
            "{backend:?}"
        );
        assert_eq!(
            call_statuses(&result),
            vec![
                ("add".to_string(), CodemodeCallStatus::Ok),
                ("add".to_string(), CodemodeCallStatus::Ok),
            ],
            "{backend:?}"
        );
        let calls = match &result {
            CodemodeResult::Ok(ok) => &ok.calls,
            CodemodeResult::Err(error) => &error.calls,
        };
        assert!(
            calls.iter().all(|call| call.duration_ms >= 0.0),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `runs concurrent calls and lists tool names`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_concurrent_calls_and_lists_tool_names() {
    for backend in BACKENDS {
        let delay = tool("delay", |args, _context| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(args)
            })
        });
        let sandbox = create_sandbox(backend, vec![echo(), delay]).await;
        let result = run(
            &sandbox,
            r#"
				const [a, b, c] = await Promise.all([tools.delay(1), tools.delay(2), tools.echo(3)]);
				return { values: [a, b, c], names: Object.keys(tools) };
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!({ "values": [1, 2, 3], "names": ["echo", "delay"] }),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `exposes tools under normalized identifiers and lists them in ALL_TOOLS`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exposes_tools_under_normalized_identifiers_and_lists_them_in_all_tools() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(
            backend,
            vec![
                described("my-tool", "Dashes", "dash"),
                described("my_tool", "Shadowed", "underscore"),
                described("mcp__docs__search", "", "mcp"),
            ],
        )
        .await;
        let result = run(
            &sandbox,
            r#"
				try { ALL_TOOLS.push({}); } catch {}
				return {
					all: ALL_TOOLS,
					calls: [await tools.my_tool(), await tools["my-tool"](), await tools.mcp__docs__search()],
				};
			"#,
        )
        .await;
        let value = value_of(&result);
        assert_eq!(
            value["calls"],
            json!(["dash", "dash", "mcp"]),
            "{backend:?}"
        );
        let all = value["all"].as_array().expect("an array");
        assert_eq!(all.len(), 2, "{backend:?}: {all:?}");
        assert_eq!(all[0]["name"], json!("my_tool"), "{backend:?}");
        assert_eq!(all[0]["description"], json!("Dashes"), "{backend:?}");
        assert_eq!(all[1]["name"], json!("mcp__docs__search"), "{backend:?}");
        assert_eq!(all[1]["description"], json!(""), "{backend:?}");
        sandbox.close().await;
    }
}

/// `passes undefined arguments and results through`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passes_undefined_arguments_and_results_through() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(
            backend,
            vec![tool("noop", |args, _| Box::pin(async move { Ok(args) }))],
        )
        .await;
        let result = run(
            &sandbox,
            "return [await tools.noop(), await tools.noop(null)]",
        )
        .await;
        assert_eq!(value_of(&result), json!([null, null]), "{backend:?}");
        sandbox.close().await;
    }
}

/// `turns tool errors into catchable Errors in the script`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turns_tool_errors_into_catchable_errors_in_the_script() {
    for backend in BACKENDS {
        let fail = tool("fail", |_args, _context| {
            Box::pin(async move { Err("tool exploded".to_string()) })
        });
        let sandbox = create_sandbox(backend, vec![fail]).await;
        let result = run(
            &sandbox,
            r#"
				try {
					await tools.fail();
					return "no error";
				} catch (error) {
					return { isError: error instanceof Error, message: error.message };
				}
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!({ "isError": true, "message": "tool exploded" }),
            "{backend:?}"
        );
        assert_eq!(
            call_statuses(&result),
            vec![("fail".to_string(), CodemodeCallStatus::Error)],
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `rejects calls to unknown tools`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_calls_to_unknown_tools() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "return await tools.missing()").await;
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
        assert_eq!(
            error.error.name.as_deref(),
            Some("TypeError"),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `aborts unawaited calls when the script returns`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborts_unawaited_calls_when_the_script_returns() {
    for backend in BACKENDS {
        let aborted = Arc::new(AtomicBool::new(false));
        let slow = {
            let aborted = Arc::clone(&aborted);
            tool("slow", move |_args, context| {
                let aborted = Arc::clone(&aborted);
                Box::pin(async move {
                    context.signal.cancelled().await;
                    aborted.store(true, Ordering::SeqCst);
                    Err("aborted".to_string())
                })
            })
        };
        let sandbox = create_sandbox(backend, vec![slow]).await;
        let result = run(&sandbox, "tools.slow(); return 'early'").await;
        assert_eq!(value_of(&result), json!("early"), "{backend:?}");
        assert_eq!(
            call_statuses(&result),
            vec![("slow".to_string(), CodemodeCallStatus::Cancelled)],
            "{backend:?}"
        );
        assert!(aborted.load(Ordering::SeqCst), "{backend:?}");
        sandbox.close().await;
    }
}

/// `names close matches when a script reads a tool that does not exist`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn names_close_matches_when_a_script_reads_a_tool_that_does_not_exist() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![echo(), described("web-search", "", "")]).await;
        assert_eq!(
            attempt(&sandbox, "tools.Echo").await,
            json!(
                "tools.Echo does not exist. Did you mean tools.echo? ALL_TOOLS lists every tool; searchTools(query) finds tools by topic. Check for a member with \"Echo\" in tools."
            ),
            "{backend:?}"
        );
        let websearch = attempt(&sandbox, "tools.websearch").await;
        assert!(
            websearch
                .as_str()
                .expect("a message")
                .contains("Did you mean tools.web_search?"),
            "{backend:?}: {websearch}"
        );
        let nothing = attempt(&sandbox, "tools.nothing").await;
        assert!(
            nothing
                .as_str()
                .expect("a message")
                .contains("Available: echo, web_search."),
            "{backend:?}: {nothing}"
        );
        assert_eq!(
            attempt(&sandbox, "['echo' in tools, 'nothing' in tools, String(tools.toString), JSON.stringify(tools)]").await,
            json!([true, false, "undefined", "{}"]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `supports register and unregister between executions`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supports_register_and_unregister_between_executions() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        sandbox.base().register_tool(echo()).expect("registers");
        let error = sandbox
            .base()
            .register_tool(echo())
            .expect_err("a second registration fails");
        assert!(error.contains("already registered"), "{backend:?}: {error}");
        assert_eq!(
            sandbox
                .base()
                .tools()
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            vec!["echo".to_string()],
            "{backend:?}"
        );
        let result = run(&sandbox, "return await tools.echo('a')").await;
        assert_eq!(value_of(&result), json!("a"), "{backend:?}");
        assert!(sandbox.base().unregister_tool("echo"), "{backend:?}");
        let after = run(&sandbox, "return 'echo' in tools").await;
        assert_eq!(value_of(&after), json!(false), "{backend:?}");
        sandbox.close().await;
    }
}

// ---------------------------------------------------------------------------
// store and load
// ---------------------------------------------------------------------------

fn store_of(entries: &[(&str, Value)]) -> IndexMap<String, Value> {
    let mut store = IndexMap::new();
    for (key, value) in entries {
        store.insert((*key).to_string(), value.clone());
    }
    store
}

/// `reads the snapshot and reports writes`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_the_snapshot_and_reports_writes() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run_with(
            &sandbox,
            r#"
				const seen = load("counter");
				store("counter", seen + 1);
				store("list", [1, { a: null }]);
				store("old", undefined);
				return [seen, load("counter"), load("missing"), load("old")];
			"#,
            CodemodeExecuteOptions {
                store: Some(store_of(&[("counter", json!(41)), ("old", json!("x"))])),
                ..Default::default()
            },
        )
        .await;
        let ok = ok_of(&result);
        // 返回值的 JSON 往返把 undefined 数组元素变成 null。
        assert_eq!(ok.value, Some(json!([41, 42, null, null])), "{backend:?}");
        assert_eq!(
            ok.store_writes.set,
            store_of(&[("counter", json!(42)), ("list", json!([1, { "a": null }]))]),
            "{backend:?}"
        );
        assert_eq!(
            ok.store_writes.delete,
            vec!["old".to_string()],
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `returns copies, so mutating a loaded value does not change the store`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_copies_so_mutating_a_loaded_value_does_not_change_the_store() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run_with(
            &sandbox,
            r#"const value = load("obj"); value.a = 2; const kept = { b: 1 }; store("kept", kept); kept.b = 2;
				return [load("obj").a, load("kept").b];"#,
            CodemodeExecuteOptions {
                store: Some(store_of(&[("obj", json!({ "a": 1 }))])),
                ..Default::default()
            },
        )
        .await;
        let ok = ok_of(&result);
        assert_eq!(ok.value, Some(json!([1, 1])), "{backend:?}");
        assert_eq!(
            ok.store_writes.set,
            store_of(&[("kept", json!({ "b": 1 }))]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `rejects invalid keys, values, and oversized writes inside the script`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_keys_values_and_oversized_writes_inside_the_script() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            r#"
				const attempt = (fn) => { try { fn(); return "ok"; } catch (error) { return error.name; } };
				return [
					attempt(() => store(1, "x")),
					attempt(() => load({})),
					attempt(() => store("fn", () => 1)),
					attempt(() => store("big", "x".repeat(300 * 1024))),
					attempt(() => { for (let i = 0; i < 8; i++) store("k" + i, "x".repeat(200 * 1024)); }),
				];
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!([
                "TypeError",
                "TypeError",
                "TypeError",
                "RangeError",
                "RangeError"
            ]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `explains oversized writes`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explains_oversized_writes() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(&sandbox, "store(\"img\", \"x\".repeat(300 * 1024));").await;
        let error = err_of(&result);
        assert!(
            error
                .error
                .message
                .contains("store(\"img\") value has 307202 characters of JSON"),
            "{backend:?}: {}",
            error.error.message
        );
        assert!(
            error.error.message.contains("Show images with image()"),
            "{backend:?}: {}",
            error.error.message
        );
        sandbox.close().await;
    }
}

/// `reserves the store and load names`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserves_the_store_and_load_names() {
    for backend in BACKENDS {
        for reserved in ["store", "load"] {
            let base = CodemodeSandboxBaseOptions {
                globals: Some(vec![tool(reserved, |_args, _context| {
                    Box::pin(async { Ok(Value::Null) })
                })]),
                timeout_ms: Some(10_000.0),
                ..Default::default()
            };
            let error = match try_new_sandbox(backend, base).await {
                Ok(_) => panic!("{backend:?}: {reserved} must be rejected"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("Invalid global"),
                "{backend:?}: {error}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// globals
// ---------------------------------------------------------------------------

/// `exposes globals as top-level functions without recording them as calls`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exposes_globals_as_top_level_functions_without_recording_them_as_calls() {
    for backend in BACKENDS {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let attach = {
            let seen = Arc::clone(&seen);
            tool("attach", move |args, _context| {
                seen.lock().expect("seen mutex").push(args);
                Box::pin(async { Ok(Value::Null) })
            })
        };
        let sandbox = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                tools: Some(vec![echo()]),
                globals: Some(vec![attach]),
                timeout_ms: Some(10_000.0),
                ..Default::default()
            },
        )
        .await;
        let result = run(
            &sandbox,
            r#"
				await attach({ ref: 1 });
				attach("not awaited");
				return [typeof attach, typeof globalThis.attach, await tools.echo(2)];
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!(["function", "function", 2]),
            "{backend:?}"
        );
        assert_eq!(call_names(&result), vec!["echo".to_string()], "{backend:?}");
        // 消息按序处理，所以未 await 的全局函数也在脚本结算前运行。
        assert_eq!(
            *seen.lock().expect("seen mutex"),
            vec![json!({ "ref": 1 }), json!("not awaited")],
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `groups namespaced globals and spreads arguments on request`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groups_namespaced_globals_and_spreads_arguments_on_request() {
    for backend in BACKENDS {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let list = {
            let seen = Arc::clone(&seen);
            CodemodeTool {
                spread: Some(true),
                ..tool("models.list", move |args, _context| {
                    seen.lock().expect("seen mutex").push(args);
                    Box::pin(async { Ok(Value::Null) })
                })
            }
        };
        let first = tool("models.first", |args, _context| {
            Box::pin(async move { Ok(args) })
        });
        let sandbox = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                globals: Some(vec![list, first]),
                timeout_ms: Some(10_000.0),
                ..Default::default()
            },
        )
        .await;
        let result = run(
            &sandbox,
            r#"
				await models.list("classifier", undefined, 3);
				await models.list();
				try { models.extra = 1; } catch {}
				return [Object.keys(models), await models.first("a", "ignored"), "extra" in models];
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!([["list", "first"], "a", false]),
            "{backend:?}"
        );
        // 返回值的 JSON 往返把 undefined 数组元素变成 null。
        assert_eq!(
            *seen.lock().expect("seen mutex"),
            vec![json!(["classifier", null, 3]), json!([])],
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `names the members of a namespace when a script reads one that does not exist`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn names_the_members_of_a_namespace_when_a_script_reads_one_that_does_not_exist() {
    for backend in BACKENDS {
        let sandbox = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                globals: Some(vec![
                    tool("models.classify", |_args, _context| {
                        Box::pin(async { Ok(Value::Null) })
                    }),
                    tool("models.generateImages", |_args, _context| {
                        Box::pin(async { Ok(Value::Null) })
                    }),
                ]),
                timeout_ms: Some(10_000.0),
                ..Default::default()
            },
        )
        .await;
        let result = run(&sandbox, "await models.generateImage();").await;
        let error = err_of(&result);
        assert_eq!(
            error.error.message,
            "models.generateImage does not exist. Did you mean models.generateImages? Check for a member with \"generateImage\" in models.",
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `rejects invalid and reserved global names`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_and_reserved_global_names() {
    for backend in BACKENDS {
        for name in ["a.b.c", "a.", ".a", "tools.x", "store.x", "a.not-valid"] {
            let base = CodemodeSandboxBaseOptions {
                globals: Some(vec![tool(name, |_args, _context| {
                    Box::pin(async { Ok(Value::Null) })
                })]),
                timeout_ms: Some(10_000.0),
                ..Default::default()
            };
            match try_new_sandbox(backend, base).await {
                Ok(_) => panic!("{backend:?}: {name} must be rejected"),
                Err(error) => assert!(
                    error.to_string().contains("Invalid global"),
                    "{backend:?}: {name}: {error}"
                ),
            }
        }
        let base = CodemodeSandboxBaseOptions {
            globals: Some(vec![
                tool("models", |_args, _context| {
                    Box::pin(async { Ok(Value::Null) })
                }),
                tool("models.list", |_args, _context| {
                    Box::pin(async { Ok(Value::Null) })
                }),
            ]),
            timeout_ms: Some(10_000.0),
            ..Default::default()
        };
        match try_new_sandbox(backend, base).await {
            Ok(_) => panic!("{backend:?}: a namespace and its member must conflict"),
            Err(error) => assert!(
                error.to_string().contains("conflicts with the namespace"),
                "{backend:?}: {error}"
            ),
        }
        for name in ["not-valid", "tools", "console"] {
            let base = CodemodeSandboxBaseOptions {
                globals: Some(vec![tool(name, |_args, _context| {
                    Box::pin(async { Ok(Value::Null) })
                })]),
                timeout_ms: Some(10_000.0),
                ..Default::default()
            };
            match try_new_sandbox(backend, base).await {
                Ok(_) => panic!("{backend:?}: {name} must be rejected"),
                Err(error) => assert!(
                    error.to_string().contains("Invalid global"),
                    "{backend:?}: {name}: {error}"
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// limits and lifetime
// ---------------------------------------------------------------------------

/// `terminates a synchronous infinite loop on timeout`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminates_a_synchronous_infinite_loop_on_timeout() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let started = Instant::now();
        let result = run_with(
            &sandbox,
            "while (true) {}",
            CodemodeExecuteOptions {
                timeout_ms: Some(200.0),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            err_of(&result).error.kind,
            CodemodeErrorKind::Timeout,
            "{backend:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{backend:?}");
        sandbox.close().await;
    }
}

/// `runs without a deadline when timeoutMs is Infinity`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_without_a_deadline_when_timeout_ms_is_infinity() {
    for backend in BACKENDS {
        let wait = tool("wait", |_args, _context| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(json!("late"))
            })
        });
        let sandbox = create_sandbox(backend, vec![wait]).await;
        let result = run_with(
            &sandbox,
            "return await tools.wait()",
            CodemodeExecuteOptions {
                timeout_ms: Some(f64::INFINITY),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(value_of(&result), json!("late"), "{backend:?}");
        sandbox.close().await;
    }
}

/// `fails a script that waits on a promise nothing can settle`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_a_script_that_waits_on_a_promise_nothing_can_settle() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![echo()]).await;
        let result = run_with(
            &sandbox,
            "await tools.echo(1); await new Promise(() => {}); return 'never'",
            CodemodeExecuteOptions {
                timeout_ms: Some(f64::INFINITY),
                ..Default::default()
            },
        )
        .await;
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
        assert!(
            error.error.message.contains("can never settle"),
            "{backend:?}: {}",
            error.error.message
        );
        // 有调用未结算时返回不算卡住。
        let returned = run_with(
            &sandbox,
            "tools.echo(2); return 'early'",
            CodemodeExecuteOptions {
                timeout_ms: Some(f64::INFINITY),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(value_of(&returned), json!("early"), "{backend:?}");
        sandbox.close().await;
    }
}

/// `terminates a microtask-spinning loop on timeout`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminates_a_microtask_spinning_loop_on_timeout() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run_with(
            &sandbox,
            "while (true) await null",
            CodemodeExecuteOptions {
                timeout_ms: Some(200.0),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            err_of(&result).error.kind,
            CodemodeErrorKind::Timeout,
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// spec §A.5.2（Rust 回归）：inline 的 VM 跑在调用线程上，脚本纯计算期间宿主事件循环不跑，
/// 所以 `timeoutMs` 与 abort 信号都不起作用——纯计算只能被中断预算停下。
///
/// TS 的 `sandbox.test.ts` 在三个后端上只断言 `kind: "timeout"`（worker 靠 deadline，inline
/// 靠预算，两者都是 timeout），因此矩阵看不出这条语义。Rust 的 VM 在专用线程上，如果不显式
/// 占住宿主事件循环，deadline 与 abort 会在计算中途触发（评审发现的偏差）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inline_ignores_the_deadline_and_abort_signal_while_the_script_computes() {
    const COMPUTE: &str =
        "const t = performance.now(); while (performance.now() - t < 1500) {} return 'done'";
    let wasm = wasm().await;
    let sandbox = InlineCodemodeSandbox::new(InlineCodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions::default(),
        wasm,
        interrupt_budget: Some(1_000_000_000),
    })
    .expect("valid inline options");

    // deadline 在计算期间到点：不起作用，脚本返回自己的结果。
    let result = sandbox
        .execute(
            COMPUTE,
            CodemodeExecuteOptions {
                timeout_ms: Some(200.0),
                ..Default::default()
            },
        )
        .await
        .expect("sandbox is open");
    assert_eq!(value_of(&result), json!("done"));

    // abort 在计算期间到达：同样不起作用。
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let result = sandbox
        .execute(
            COMPUTE,
            CodemodeExecuteOptions {
                timeout_ms: Some(f64::INFINITY),
                signal: Some(token),
                ..Default::default()
            },
        )
        .await
        .expect("sandbox is open");
    assert_eq!(value_of(&result), json!("done"));
    sandbox.close().await;
}

/// spec §A.5.2（Rust 回归）：deadline 在计算期间到点时不能取代中断预算的报告。
///
/// TS 里 `setTimeout` 回调要等 VM 让出宿主才运行，那时预算耗尽的 crash 已经把执行结算成
/// `Execution exceeded its interrupt budget of N`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_reports_the_interrupt_budget_when_the_deadline_passed_during_compute() {
    let wasm = wasm().await;
    let sandbox = InlineCodemodeSandbox::new(InlineCodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions::default(),
        wasm,
        interrupt_budget: Some(INTERRUPT_BUDGET),
    })
    .expect("valid inline options");
    let started = Instant::now();
    let result = sandbox
        .execute(
            "while (true) {}",
            CodemodeExecuteOptions {
                timeout_ms: Some(200.0),
                ..Default::default()
            },
        )
        .await
        .expect("sandbox is open");
    let error = err_of(&result);
    assert_eq!(error.error.kind, CodemodeErrorKind::Timeout);
    assert_eq!(
        error.error.message,
        format!("Execution exceeded its interrupt budget of {INTERRUPT_BUDGET}")
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    sandbox.close().await;
}

/// 最近一次执行拿到的 `events` 弱引用（见 [`track_events`]）。
type TrackedEventsRef = Arc<Mutex<Option<Weak<dyn VmEvents>>>>;

/// 回归测试用：包一层 launcher，记录最近一次执行拿到的 `Arc<dyn VmEvents>` 的弱引用。
///
/// `Weak<dyn VmEvents>` 只在 inline 的转发任务退出后失效（`Execution` 本身不持有 launcher 的
/// `events`，`ExecutionEvents` 持有的是 `Execution`），因此可以用它观察转发任务是否已释放
/// `events`——也就是「`events` 与 `Execution` 持有的 channel（含发送端）之间的环」是否被打破。
fn track_events(inner: VmLauncher) -> (VmLauncher, TrackedEventsRef) {
    struct TrackedEvents {
        inner: Arc<dyn VmEvents>,
    }

    impl VmEvents for TrackedEvents {
        fn message(&self, message: Value) {
            self.inner.message(message);
        }

        fn failure(&self, error: kiana_codemode::types::CodemodeError) {
            self.inner.failure(error);
        }

        fn host_event_loop(
            &self,
        ) -> Option<Arc<kiana_codemode::runtime::execution::HostEventLoop>> {
            self.inner.host_event_loop()
        }
    }

    let last_events: TrackedEventsRef = Arc::new(Mutex::new(None));
    let launcher: VmLauncher = {
        let last_events = Arc::clone(&last_events);
        Arc::new(move |data, events| {
            let tracked: Arc<dyn VmEvents> = Arc::new(TrackedEvents { inner: events });
            *last_events.lock().expect("events mutex") = Some(Arc::downgrade(&tracked));
            inner(data, tracked)
        })
    };
    (launcher, last_events)
}

/// 等 [`track_events`] 记录的 `events` 被释放（转发任务退出），最多 10 秒。
async fn wait_for_events_drop(last_events: TrackedEventsRef, message: &str) {
    let events = last_events
        .lock()
        .expect("events mutex")
        .take()
        .expect("the launcher ran");
    let deadline = Instant::now() + Duration::from_secs(10);
    while events.upgrade().is_some() {
        assert!(Instant::now() < deadline, "{message}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 回归（评审发现的 inline 泄漏）：转发任务在交付终态消息或收到 `stop()` 之后必须释放它持有
/// 的 `VmEvents`。否则 `events`（ExecutionEvents → `Arc<Execution>`）与 `Execution` 持有的
/// channel（含转发队列的发送端）构成永不释放的环，每次执行的结果与输出都留到进程结束。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_drops_its_events_after_an_execution_finishes() {
    let wasm = wasm().await;
    let (launcher, last_events) = track_events(inline_launcher(wasm, INTERRUPT_BUDGET));
    let base = CodemodeSandboxBase::new(CodemodeSandboxBaseOptions::default(), launcher)
        .expect("valid sandbox options");
    let result = base
        .execute(
            "text('x'.repeat(65536)); return 'ok'",
            CodemodeExecuteOptions::default(),
        )
        .await
        .expect("sandbox is open");
    assert_eq!(value_of(&result), json!("ok"));
    base.close().await;

    wait_for_events_drop(
        last_events,
        "the inline launcher still holds its VmEvents after the execution finished",
    )
    .await;
}

/// 回归（评审发现的 inline 泄漏，`stop()` 路径）：`close()`（deadline 与 abort 同）走
/// `InlineChannel::stop()`；转发任务必须收到 `ForwardItem::Shutdown` 并释放它持有的
/// `VmEvents`。否则环仍在，`close()` 之后执行的结果与输出仍留到进程结束（评审的 RSS 探针
/// 观察到 close() 之后内存不回落）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_drops_its_events_after_close_stops_a_script() {
    let (called, mut calls) = tokio::sync::mpsc::unbounded_channel::<()>();
    let hang = {
        let called = called.clone();
        tool("hang", move |_args, context| {
            let signal = context.signal.clone();
            let called = called.clone();
            Box::pin(async move {
                let _ = called.send(());
                signal.cancelled().await;
                Err::<Value, String>("cancelled".to_string())
            })
        })
    };
    let wasm = wasm().await;
    let (launcher, last_events) = track_events(inline_launcher(wasm, INTERRUPT_BUDGET));
    let base = CodemodeSandboxBase::new(
        CodemodeSandboxBaseOptions {
            tools: Some(vec![hang]),
            ..Default::default()
        },
        launcher,
    )
    .expect("valid sandbox options");

    let pending = base.execute(
        "await tools.hang(); return 'never'",
        CodemodeExecuteOptions::default(),
    );
    calls.recv().await.expect("the tool is called");
    // 执行停在工具上：close() 以 abort → finish → channel.stop() 结束它（不发终态消息）。
    base.close().await;
    let result = pending.await.expect("sandbox is open");
    assert_eq!(err_of(&result).error.kind, CodemodeErrorKind::Aborted);

    wait_for_events_drop(
        last_events,
        "the inline launcher still holds its VmEvents after close() stopped the execution",
    )
    .await;
}

/// 回归（评审发现的 serve 端泄漏）：`serveCodemodeRemote` 复用同一个 inline launcher，所以
/// `ServeEvents → ServeState → channel（发送端）` 也曾与转发任务成环，ServeState 连同
/// exchange 闭包一起留到进程结束。远端结束时它必须被释放。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_drops_its_state_after_the_script_ends() {
    struct TrackingRemote {
        wasm: CodemodeWasmModule,
        exchange_ref: Arc<Mutex<Option<Weak<()>>>>,
    }

    impl CodemodeRemote for TrackingRemote {
        fn execute(
            &self,
            request: CodemodeRemoteRequest,
            exchange: CodemodeRemoteExchange,
        ) -> BoxFuture<'static, Result<(), String>> {
            let wasm = self.wasm.clone();
            let marker = Arc::new(());
            *self.exchange_ref.lock().expect("exchange mutex") = Some(Arc::downgrade(&marker));
            Box::pin(async move {
                let wrapped: CodemodeRemoteExchange = Arc::new(move |messages| {
                    // 闭包持有 marker：只有保存 exchange 的 ServeState 被释放，marker 才消失。
                    let _held = &marker;
                    exchange(messages)
                });
                serve_codemode_remote(
                    request,
                    wrapped,
                    CodemodeServeOptions {
                        wasm,
                        max_interrupt_budget: Some(INTERRUPT_BUDGET),
                        max_memory_limit_bytes: None,
                    },
                )
                .await;
                Ok(())
            })
        }
    }

    let wasm = wasm().await;
    let exchange_ref: Arc<Mutex<Option<Weak<()>>>> = Arc::new(Mutex::new(None));
    let sandbox = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions::default(),
        remote: {
            let wasm = wasm.clone();
            let exchange_ref = Arc::clone(&exchange_ref);
            Arc::new(move || {
                Arc::new(TrackingRemote {
                    wasm: wasm.clone(),
                    exchange_ref: Arc::clone(&exchange_ref),
                }) as Arc<dyn CodemodeRemote>
            })
        },
        interrupt_budget: Some(INTERRUPT_BUDGET),
    })
    .expect("valid remote options");
    let result = sandbox
        .execute("text('hello'); return 1", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    assert_eq!(value_of(&result), json!(1));
    sandbox.close().await;

    let marker = exchange_ref
        .lock()
        .expect("exchange mutex")
        .take()
        .expect("the remote ran");
    let deadline = Instant::now() + Duration::from_secs(10);
    while marker.upgrade().is_some() {
        assert!(
            Instant::now() < deadline,
            "serveCodemodeRemote still holds its exchange after the script ended"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// `aborts via signal and cancels in-flight calls`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborts_via_signal_and_cancels_in_flight_calls() {
    for backend in BACKENDS {
        let (called, mut calls) = tokio::sync::mpsc::unbounded_channel::<()>();
        let tool_signal: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
        let hang = {
            let tool_signal = Arc::clone(&tool_signal);
            let called = called.clone();
            tool("hang", move |_args, context| {
                *tool_signal.lock().expect("signal mutex") = Some(context.signal.clone());
                let _ = called.send(());
                Box::pin(std::future::pending::<Result<Value, String>>())
            })
        };
        let sandbox = create_sandbox(backend, vec![hang]).await;

        // 无 signal 的执行：控制器的 abort 与它无关，close() 时才结束。
        let promise = sandbox.base().execute(
            "await tools.hang(); return 'never'",
            CodemodeExecuteOptions::default(),
        );
        calls.recv().await.expect("the tool is called");

        let controller = CancellationToken::new();
        controller.cancel();
        assert!(
            !tool_signal
                .lock()
                .expect("signal mutex")
                .as_ref()
                .expect("a tool signal")
                .is_cancelled(),
            "{backend:?}"
        );
        let result = sandbox
            .base()
            .execute_with_abort_reason(
                "await tools.hang()",
                CodemodeExecuteOptions {
                    signal: Some(controller.clone()),
                    ..Default::default()
                },
                Some("user cancelled".to_string()),
            )
            .await
            .expect("sandbox is open");
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Aborted, "{backend:?}");
        assert_eq!(error.error.message, "user cancelled", "{backend:?}");

        let second = CancellationToken::new();
        let pending = sandbox.base().execute(
            "await tools.hang(); return 'never'",
            CodemodeExecuteOptions {
                signal: Some(second.clone()),
                ..Default::default()
            },
        );
        calls.recv().await.expect("the second call is made");
        second.cancel();
        let aborted = pending.await.expect("sandbox is open");
        assert_eq!(
            err_of(&aborted).error.kind,
            CodemodeErrorKind::Aborted,
            "{backend:?}"
        );
        assert_eq!(
            call_statuses(&aborted),
            vec![("hang".to_string(), CodemodeCallStatus::Cancelled)],
            "{backend:?}"
        );
        assert!(
            tool_signal
                .lock()
                .expect("signal mutex")
                .as_ref()
                .expect("a tool signal")
                .is_cancelled(),
            "{backend:?}"
        );

        sandbox.close().await;
        let closed = promise.await.expect("sandbox is open");
        let error = err_of(&closed);
        assert_eq!(error.error.kind, CodemodeErrorKind::Aborted, "{backend:?}");
        assert_eq!(error.error.message, "Sandbox closed", "{backend:?}");
    }
}

/// `fails a script whose output passes the limits, even if it catches the error`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_a_script_whose_output_passes_the_limits_even_if_it_catches_the_error() {
    for backend in BACKENDS {
        // TS 的 `createSandbox()` 用默认的 10 s deadline：脚本在一个 deadline 内就会撞上输出
        // 上限（100 000 项 / 16 MiB 文本）。Rust 侧曾因输出桥（每条消息 12 次 wasm 调用）在
        // debug 构建里慢 10 倍以上而放宽成 120 s；`[profile.dev.package.wasmtime]` 打开优化后
        // 恢复 TS 的 deadline，不能再用放宽期限掩盖行为差异（hosts 设 10 s 时 TS 报的是输出
        // 上限的 script RangeError，不是 timeout）。
        let sandbox = new_sandbox(backend, create_options(vec![], 10_000.0)).await;
        for print in [
            "text(s)",
            "console.log(s)",
            "image(\"data:image/png;base64,\" + p)",
        ] {
            let result = run(
                &sandbox,
                &format!(
                    r#"
					const s = "x".repeat(1 << 20);
					const p = "iVBORw0KGgoA" + "A".repeat(1 << 20);
					for (;;) {{ try {{ {print}; }} catch {{}} }}
				"#
                ),
            )
            .await;
            let error = err_of(&result);
            assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
            assert_eq!(
                error.error.name.as_deref(),
                Some("RangeError"),
                "{backend:?}"
            );
            assert!(
                error.error.message.contains("script output exceeded"),
                "{backend:?}: {}",
                error.error.message
            );
            let chars: usize = error
                .output
                .iter()
                .map(|item| match item {
                    CodemodeOutputItem::Text { text, .. } => text.len(),
                    CodemodeOutputItem::Image { data, .. } => data.len(),
                })
                .sum();
            assert!(
                chars <= kiana_codemode::MAX_OUTPUT_CHARS,
                "{backend:?}: {chars}"
            );
            assert!(
                chars > kiana_codemode::MAX_OUTPUT_CHARS - (2 << 20),
                "{backend:?}: {chars}"
            );
        }

        let empty = run(&sandbox, "for (;;) text(\"\");").await;
        let error = err_of(&empty);
        assert_eq!(
            error.error.name.as_deref(),
            Some("RangeError"),
            "{backend:?}: kind={:?} message={:?} output={}",
            error.error.kind,
            error.error.message,
            error.output.len()
        );
        assert_eq!(
            error.output.len(),
            kiana_codemode::MAX_OUTPUT_ITEMS,
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `rejects execute after close`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_execute_after_close() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        sandbox.close().await;
        let result = sandbox
            .base()
            .execute("return 1", CodemodeExecuteOptions::default())
            .await;
        let error = result.expect_err("the sandbox is closed");
        assert!(error.to_string().contains("closed"), "{backend:?}: {error}");
    }
}

/// `runs executions in parallel without sharing state`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_executions_in_parallel_without_sharing_state() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let (a, b, c) = tokio::join!(
            sandbox.base().execute(
                "globalThis.shared = 'a'; await null; return globalThis.shared",
                CodemodeExecuteOptions::default()
            ),
            sandbox.base().execute(
                "globalThis.shared = 'b'; await null; return globalThis.shared",
                CodemodeExecuteOptions::default()
            ),
            sandbox.base().execute(
                "return typeof globalThis.shared",
                CodemodeExecuteOptions::default()
            ),
        );
        let values = vec![
            value_of(&a.expect("open")),
            value_of(&b.expect("open")),
            value_of(&c.expect("open")),
        ];
        assert_eq!(
            values,
            vec![json!("a"), json!("b"), json!("undefined")],
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `turns deep recursion into a catchable RangeError`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turns_deep_recursion_into_a_catchable_range_error() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            r#"
				let depth = 0;
				function dive() { depth++; dive(); }
				try { dive(); } catch (error) { return [error.name, depth > 1000]; }
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!(["RangeError", true]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// 深 **C** 递归（解析器、`JSON.stringify`）的崩溃边界按后端区分，与 TS 逐点一致。
///
/// TS 把 inline/remote 的 VM 跑在**调用线程**上（Node 主线程约 1 MB 原生栈），worker 的 VM
/// 跑在 4 MB 的 worker 线程上：同一段 600 层嵌套字面量在 inline/remote 上是宿主
/// `RangeError`（`startVm` 把它当 crash 上报 → `kind:"sandbox"`），在 worker 上正常结束。
/// 审查回归：此前所有后端共用 5 MiB 的 wasm 栈预算，inline/remote 在 600 层仍成功（直到
/// 2400 层才变成可 catch 的 script `RangeError`），worker 的边界也比 TS 早约 7%。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_c_recursion_uses_the_ts_stack_boundary_per_backend() {
    let nested = |depth: usize| format!("return {}1{}", "[".repeat(depth), "]".repeat(depth));
    let stringify = |depth: usize| {
        format!("let v=1;for(let i=0;i<{depth};i++)v=[v];return JSON.stringify(v).length")
    };
    for backend in BACKENDS {
        // 深 C 递归可能跑几秒，别让默认 10 s 的 deadline 干扰（测的是栈，不是时间）。
        let sandbox = new_sandbox(backend, create_options(vec![], 60_000.0)).await;
        let on_calling_thread = matches!(backend, Backend::Inline | Backend::Remote);

        // TS 在这两处都成功（worker 与 inline 的差别还看不出来）。
        assert!(
            value_of(&run(&sandbox, &nested(400)).await).is_array(),
            "{backend:?}"
        );
        assert_eq!(
            value_of(&run(&sandbox, &stringify(4000)).await),
            json!(8001),
            "{backend:?}"
        );

        // 600 层：TS inline/remote 的宿主栈已经耗尽。
        let result = run(&sandbox, &nested(600)).await;
        if on_calling_thread {
            let error = err_of(&result);
            assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox, "{backend:?}");
            assert_eq!(
                error.error.message, "RangeError: Maximum call stack size exceeded",
                "{backend:?}"
            );
        } else {
            assert!(value_of(&result).is_array(), "{backend:?}");
        }

        // 6000 层 `JSON.stringify`：同一处分界（TS inline/remote 失败，worker 成功）。
        let result = run(&sandbox, &stringify(6000)).await;
        if on_calling_thread {
            let error = err_of(&result);
            assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox, "{backend:?}");
            assert_eq!(
                error.error.message, "RangeError: Maximum call stack size exceeded",
                "{backend:?}"
            );
        } else {
            assert_eq!(value_of(&result), json!(12001), "{backend:?}");
        }
        sandbox.close().await;
    }
}

// ---------------------------------------------------------------------------
// escape hatches
// ---------------------------------------------------------------------------

/// `has no host globals`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn has_no_host_globals() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            r#"
				return [
					typeof process, typeof require, typeof module, typeof setTimeout, typeof fetch,
					typeof WebAssembly, typeof std, typeof os, typeof globalThis.constructor,
				]
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!([
                "undefined",
                "undefined",
                "undefined",
                "undefined",
                "undefined",
                "undefined",
                "undefined",
                "undefined",
                "function",
            ]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `keeps eval and Function inside the VM`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_eval_and_function_inside_the_vm() {
    for backend in BACKENDS {
        // 允许代码生成：它只能产生同一个 wasm 实例里的更多代码。
        let sandbox = create_sandbox(backend, vec![echo()]).await;
        let result = run(
            &sandbox,
            r#"
				return [
					eval("typeof process"),
					new Function("return typeof process")(),
					tools.echo.constructor("return typeof require")(),
					(async () => {}).constructor("return typeof setTimeout")() instanceof Promise,
				];
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!(["undefined", "undefined", "undefined", true]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `rejects dynamic import`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_dynamic_import() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            r#"
				try { await import("node:fs"); return "imported"; } catch (error) { return error.constructor.name; }
			"#,
        )
        .await;
        assert_ne!(value_of(&result), json!("imported"), "{backend:?}");
        sandbox.close().await;
    }
}

/// `ignores patches to built-ins and built-in globals`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_patches_to_built_ins_and_built_in_globals() {
    for backend in BACKENDS {
        // prelude 与脚本共享内建：补丁不能污染 prelude 发给宿主的东西（#10444）。
        let sandbox = create_sandbox(backend, vec![echo()]).await;
        let result = run(
            &sandbox,
            r#"
				Array.prototype.toJSON = () => null;
				Object.prototype.toJSON = () => 5;
				Promise.prototype.then = () => {};
				Map.prototype.get = () => undefined;
				globalThis.JSON = { stringify: () => "x", parse: () => "x" };
				store("k", [1]);
				return [await tools.echo([2]), JSON.stringify({ a: 1 })];
			"#,
        )
        .await;
        let ok = ok_of(&result);
        assert_eq!(ok.value, Some(json!([[2], "{\"a\":1}"])), "{backend:?}");
        assert_eq!(
            ok.store_writes.set,
            store_of(&[("k", json!([1]))]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `freezes intrinsics that are only reachable from instances`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freezes_intrinsics_that_are_only_reachable_from_instances() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            r#"
				return [
					Object.getPrototypeOf(function* () {}).prototype,
					Object.getPrototypeOf(async function () {}),
					Object.getPrototypeOf(Int8Array).prototype,
					Object.getPrototypeOf([][Symbol.iterator]()),
					Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())),
					Object.getPrototypeOf(new Map()[Symbol.iterator]()),
					Object.getPrototypeOf(/a/[Symbol.matchAll]("")),
				].every((object) => Object.isFrozen(object));
			"#,
        )
        .await;
        assert_eq!(value_of(&result), json!(true), "{backend:?}");
        sandbox.close().await;
    }
}

/// `still lets instances override properties of frozen prototypes`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn still_lets_instances_override_properties_of_frozen_prototypes() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            r#"
				const object = {};
				object.toString = () => "custom";
				function Legacy() {}
				Legacy.prototype = Object.create(Error.prototype);
				Legacy.prototype.constructor = Legacy;
				const bare = new Error();
				bare.message = "set later";
				class MyError extends Error {
					constructor(message) {
						super(message);
						this.name = "MyError";
					}
				}
				let patched = "silent";
				try { Error.prototype.name = "Patched"; } catch (error) { patched = error.constructor.name; }
				return [String(object), new Legacy().constructor === Legacy, bare.message, new MyError("x").name, Error.prototype.name, patched];
			"#,
        )
        .await;
        assert_eq!(
            value_of(&result),
            json!(["custom", true, "set later", "MyError", "Error", "TypeError"]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `reports errors whose name or message is not a string`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_errors_whose_name_or_message_is_not_a_string() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![]).await;
        let result = run(
            &sandbox,
            "const error = new Error('x'); error.message = 42; throw error;",
        )
        .await;
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Script, "{backend:?}");
        assert_eq!(error.error.name.as_deref(), Some("Error"), "{backend:?}");
        assert_eq!(error.error.message, "42", "{backend:?}");
        sandbox.close().await;
    }
}

/// `keeps tools and console frozen`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_tools_and_console_frozen() {
    for backend in BACKENDS {
        let sandbox = create_sandbox(backend, vec![echo()]).await;
        let result = run(
            &sandbox,
            r#"
				try { tools.echo = () => 'nope'; } catch {}
				try { tools.extra = () => 'nope'; } catch {}
				try { globalThis.tools = null; } catch {}
				return ["extra" in tools, await tools.echo('still')];
			"#,
        )
        .await;
        assert_eq!(value_of(&result), json!([false, "still"]), "{backend:?}");
        sandbox.close().await;
    }
}

// ---------------------------------------------------------------------------
// worker host
// ---------------------------------------------------------------------------

/// `CodemodeSandbox::with_spawner` 用的假 worker：`load_wasm` 可控，`spawn` 直接报告脚本化的消息。
struct FakeSpawner {
    load_wasm_error: Option<String>,
    messages: Vec<Value>,
}

impl WorkerSpawner for FakeSpawner {
    fn load_wasm(&self) -> BoxFuture<'static, Result<WorkerWasm, String>> {
        let error = self.load_wasm_error.clone();
        Box::pin(async move {
            match error {
                Some(message) => Err(message),
                None => Ok(Arc::new(()) as WorkerWasm),
            }
        })
    }

    fn spawn(
        &self,
        _worker_url: Option<&str>,
        _start: VmStartData,
        _wasm: WorkerWasm,
        _interrupt: Arc<std::sync::atomic::AtomicI32>,
        observer: Arc<dyn WorkerObserver>,
    ) -> Result<Arc<dyn WorkerHandle>, String> {
        for message in &self.messages {
            observer.message(message.clone());
        }
        Ok(Arc::new(SilentWorker) as Arc<dyn WorkerHandle>)
    }
}

/// 假 worker 的句柄：既收不到也发不出任何消息。
struct SilentWorker;

impl WorkerHandle for SilentWorker {
    fn post(&self, _message: HostToWorkerMessage) {}
    fn terminate(&self) {}
    fn unref(&self) {}
}

/// 一个可直接注入 JSON 消息的假 launcher（TS 的 `fixtures/raw-worker.ts` 在 Rust 侧的等价物：
/// `VmEvents::message` 收的就是 `Value`）。
fn scripted_launcher(message: Value) -> VmLauncher {
    Arc::new(move |_data: VmStartData, events: Arc<dyn VmEvents>| {
        let message = message.clone();
        Box::pin(async move {
            events.message(message);
            Ok(Arc::new(ScriptedChannel) as Arc<dyn VmChannel>)
        })
    })
}

/// 假 launcher 的 channel：脚本不会回答任何东西。
struct ScriptedChannel;

impl VmChannel for ScriptedChannel {
    fn post(&self, _message: HostToWorkerMessage) {}
    fn stop(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

fn unique_path(prefix: &str, extension: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{counter}.{extension}",
        std::process::id()
    ))
}

/// `accepts a worker path string`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepts_a_worker_path_string() {
    let path = unique_path("kiana-codemode-worker", "mjs");
    std::fs::write(&path, "// worker entry\n").expect("the worker file is writable");
    let wasm = wasm().await;
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
        worker_url: Some(path.to_string_lossy().into_owned()),
        wasm: Some(wasm),
        ..Default::default()
    })
    .expect("valid worker options");
    let result = sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    assert_eq!(value_of(&result), json!(1));
    sandbox.close().await;
    let _ = std::fs::remove_file(&path);
}

/// `reports a missing worker file as a sandbox error`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_missing_worker_file_as_a_sandbox_error() {
    let path = unique_path("kiana-codemode-does-not-exist", "js");
    let _ = std::fs::remove_file(&path);
    let wasm = wasm().await;
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
        worker_url: Some(path.to_string_lossy().into_owned()),
        wasm: Some(wasm),
        ..Default::default()
    })
    .expect("valid worker options");
    let result = sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    let error = err_of(&result);
    assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox);
    assert!(
        error.error.message.contains("Cannot find module"),
        "{}",
        error.error.message
    );
    sandbox.close().await;
}

/// `reports a failing wasm module as a sandbox error`。
///
/// TS 的 `wasm: Promise.reject(new Error("no wasm"))` 在冻结的 Rust 选项里没有等价物
/// （`wasm` 是已编译的模块或 `None`）；这里让 spawner 的 `load_wasm` 失败，走 worker host
/// 的同一条 `Failed to load QuickJS: …` 路径。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_failing_wasm_module_as_a_sandbox_error() {
    let spawner = Arc::new(FakeSpawner {
        load_wasm_error: Some("no wasm".to_string()),
        messages: vec![],
    });
    let sandbox = CodemodeSandbox::with_spawner(CodemodeSandboxOptions::default(), spawner)
        .expect("valid worker options");
    let result = sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("sandbox is open");
    let error = err_of(&result);
    assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox);
    assert_eq!(error.error.message, "Failed to load QuickJS: no wasm");
    sandbox.close().await;
}

/// `ignores Node --watch notifications from the worker`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_node_watch_notifications_from_the_worker() {
    // Node 24/26 `--watch` 在 worker 通道上投递依赖报告（#10725）；host 的 observer 把它们
    // 丢掉，只把 `done` 交给执行。
    let spawner = Arc::new(FakeSpawner {
        load_wasm_error: None,
        messages: vec![
            json!({ "watch:require": ["/worker.js"] }),
            json!({ "watch:import": ["file:///worker.js"] }),
            json!({ "type": "done", "ok": true, "value": "42", "writes": "[]" }),
        ],
    });
    let sandbox = CodemodeSandbox::with_spawner(CodemodeSandboxOptions::default(), spawner)
        .expect("valid worker options");
    let result = sandbox
        .execute(
            "[{\"watch:require\":[\"/worker.js\"]},{\"watch:import\":[\"file:///worker.js\"]},{\"type\":\"done\",\"ok\":true,\"value\":\"42\",\"writes\":\"[]\"}]",
            CodemodeExecuteOptions::default(),
        )
        .await
        .expect("sandbox is open");
    assert_eq!(value_of(&result), json!(42));
    sandbox.close().await;
}

/// `reports a broken bridge as a sandbox error: %j`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_broken_bridge_as_a_sandbox_error() {
    // #10444：worker 发来的畸形载荷必须以 sandbox 错误结束执行，而不是在宿主的消息监听器里
    // 抛错并让执行永远不结算。
    let cases = [
        (
            json!({ "type": "done", "ok": true, "value": "1", "writes": "null" }),
            "store writes are not an array",
        ),
        (
            json!({ "type": "done", "ok": true, "value": "1", "writes": "[1]" }),
            "store writes contain a malformed entry",
        ),
        (
            json!({ "type": "done", "ok": true, "value": "1", "writes": "[[\"k\",\"{\"]]" }),
            "store value for \"k\" is not valid JSON",
        ),
        (
            json!({ "type": "done", "ok": true, "value": "{", "writes": "[]" }),
            "return value is not valid JSON",
        ),
        (
            json!({ "type": "done", "ok": false, "error": "5" }),
            "script error is not an object",
        ),
        (
            json!({ "type": "done", "ok": false, "error": "{}" }),
            "script error is malformed",
        ),
        (
            json!({ "type": "nonsense" }),
            "unknown message from the worker",
        ),
    ];
    for (message, reason) in cases {
        let base = CodemodeSandboxBase::new(
            CodemodeSandboxBaseOptions {
                timeout_ms: Some(10_000.0),
                ..Default::default()
            },
            scripted_launcher(message.clone()),
        )
        .expect("valid sandbox options");
        let result = base
            .execute("", CodemodeExecuteOptions::default())
            .await
            .expect("sandbox is open");
        let error = err_of(&result);
        assert_eq!(error.error.kind, CodemodeErrorKind::Sandbox, "{message}");
        assert!(
            error
                .error
                .message
                .contains(&format!("Sandbox bridge broken: {reason}")),
            "{message}: {}",
            error.error.message
        );
    }
}
