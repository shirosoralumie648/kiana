//! `test/codemode.test.ts` 的移植：durable code mode 的脚本、重放与输出格式化。
//!
//! 纯格式化的用例直接驱动 [`format_result`]（无需 Harness）；脚本用例通过完整 Harness +
//! 真实 `CodemodeSandbox`（wasm 后端）跑一遍模型发起的 `codemode` 调用。
//!
//! 需要跨进程重启的用例（关掉 Harness 再打开同一份持久存储）用 `support::MemoryStorage`
//! 的 `reopen()` 模拟。同一进程里并行跑多个 QuickJS/wasmtime sandbox 会挂起甚至 SIGSEGV
//! （kiana-codemode 的跨单元问题），所以所有用例先取一把进程级锁 [`test_gate`]——TS 侧同一个
//! vitest 文件里的用例本来也是串行执行的。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_ai::providers::faux::FauxResponseStep;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, TextContent, TextKind, UserContentBlock,
};
use kiana_chord::context::Context;
use kiana_codemode::runtime::execution::StartError;
use kiana_codemode::types::{
    CodemodeOutputItem, CodemodeResult, CodemodeResultErr, CodemodeResultOk, CodemodeRun,
    CodemodeRunOptions, CodemodeStoreWrites,
};
use kiana_codemode::{CodemodeError, CodemodeErrorKind};
use kiana_durable::Session as _;
use kiana_durable::codemode::codemode::{
    CODEMODE_JOURNAL_DOC, CODEMODE_RUN_DOC, CodemodeGlobal, CodemodeSandbox, CodemodeToolOptions,
};
use kiana_durable::codemode::output::{
    CallSummary, DEFAULT_MAX_OUTPUT_TOKENS, FormatResultOptions, Limits, OutputData, OutputFile,
    OutputFileResult, format_result,
};
use kiana_durable::harness::tool::TOOL_TASK;
use kiana_durable::harness::types::{
    Conversation as ConversationTrait, Registry as _, ToolExecuteFn, ToolExecutionApi,
    ToolExecutionResult, ToolRegistration,
};
use kiana_durable::types::{
    BoxError, CommitOptions, DocTarget, DocumentReader as _, JsonObject, JsonValue, SessionChange,
    Storage, SubmissionStatus, TaskId, TaskQuery, TaskRecord, Tx,
};
use serde_json::json;

use chat_support::{ChatSetup, context, open_chat};
use harness_support::{add_tool, text_input, tool as base_tool};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 同一进程里并行跑多个 QuickJS/wasmtime sandbox 会偶发挂起甚至 SIGSEGV（kiana-codemode 的
/// 跨单元问题，见交付说明）；TS 侧同一个 vitest 文件里的测试默认串行，这里用一把进程级锁
/// 复刻同样的串行性，测试覆盖的内容不变。
static TEST_GATE: Mutex<()> = Mutex::new(());

fn test_gate() -> MutexGuard<'static, ()> {
    lock(&TEST_GATE)
}

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

fn text_item(text: &str) -> CodemodeOutputItem {
    CodemodeOutputItem::text(text)
}

fn done() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "done".to_owned(),
            text_signature: None,
        })],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: kiana_ai::types::StopReason::Stop,
        timestamp: 3,
        ..Default::default()
    }
}

fn script(code: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(
            kiana_ai::providers::faux::faux_tool_call_with(
                "codemode",
                {
                    let mut arguments = kiana_ai::types::JsonObject::new();
                    arguments.insert("code".to_owned(), JsonValue::String(code.to_owned()));
                    arguments
                },
                kiana_ai::providers::faux::FauxToolCallOptions {
                    id: Some("s1".to_owned()),
                },
            ),
        )],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    }
}

/// 记录下来的写入：路径与字节。
type Writes = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// 一个记录写入的 `OutputFile`，写入 `dir` 下的真实文件。
fn writer(dir: &str) -> (OutputFile, Writes) {
    let writes: Writes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&writes);
    let directory = dir.to_owned();
    let file: OutputFile = Arc::new(move |name: String, data: OutputData| {
        let sink = Arc::clone(&sink);
        let path = format!("{directory}/{name}");
        let bytes = data.into_bytes();
        Box::pin(async move {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                std::fs::create_dir_all(parent).expect("the directory");
            }
            std::fs::write(&path, &bytes).expect("the file");
            lock(&sink).push((name, bytes));
            OutputFileResult::Path(path)
        }) as kiana_durable::codemode::output::OutputFuture
    });
    (file, writes)
}

fn temp_dir(name: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "kiana-codemode-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&path).expect("the temp dir");
    path.to_string_lossy().into_owned()
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn ok_result(value: Option<JsonValue>, output: Vec<CodemodeOutputItem>) -> CodemodeResult {
    CodemodeResult::Ok(CodemodeResultOk::new(
        value,
        output,
        Vec::new(),
        CodemodeStoreWrites::default(),
        None,
    ))
}

fn options<'a>(write: &'a OutputFile, calls: &'a [CallSummary<'a>]) -> FormatResultOptions<'a> {
    FormatResultOptions {
        generated_images: 0,
        wall_ms: 1234.0,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        calls,
        limits: Limits {
            max_bytes: 4 * 1024 * 1024,
            max_lines: 100_000,
        },
        write: Arc::clone(write),
    }
}

fn content_text(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .map(|item| match item {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(_) => "<image>".to_owned(),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn lays_out_text_items_console_lines_and_the_returned_value_under_a_header() {
    let _guard = test_gate();
    let directory = temp_dir("layout");
    let (write, _) = writer(&directory);
    let calls: Vec<CallSummary<'_>> = Vec::new();
    let result = ok_result(
        Some(JsonValue::String("three".to_owned())),
        vec![
            text_item("one"),
            CodemodeOutputItem::console("log 1"),
            text_item("{\"two\":2}"),
        ],
    );
    let formatted = format_result(&result, options(&write, &calls))
        .await
        .expect("the result formats");
    let text = content_text(&formatted.content);
    assert!(
        text.starts_with("Script completed\nWall time 1.2 seconds\nOutput:\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "==> text 1/3 <==\none\n==> text 2/3 <==\n{\"two\":2}\n==> text 3/3 <==\nthree"
        )
    );
    assert!(text.contains("<console_output>\nlog 1\n</console_output>"));
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn cuts_text_past_the_token_budget_to_its_start_and_end_with_the_full_text_in_a_file() {
    let _guard = test_gate();
    let directory = temp_dir("truncate");
    let (write, _) = writer(&directory);
    let calls: Vec<CallSummary<'_>> = Vec::new();
    let big = format!("BEGIN{}END", "x".repeat(1000));
    let result = ok_result(None, vec![text_item(&big)]);
    let mut format_options = options(&write, &calls);
    format_options.max_output_tokens = 20.0;
    let formatted = format_result(&result, format_options)
        .await
        .expect("the result formats");
    let text = content_text(&formatted.content);
    assert!(text.contains("Warning: truncated output"));
    assert!(text.contains("BEGIN"));
    assert!(text.contains("END"));
    let path = formatted.full_output_path.clone().expect("the spill file");
    assert!(text.contains(&format!("[Full output: {path}")));
    assert_eq!(std::fs::read_to_string(&path).expect("the file"), big);
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_a_result_inside_the_tool_output_limits_whatever_max_output_tokens_says() {
    let _guard = test_gate();
    let directory = temp_dir("limits");
    let (write, _) = writer(&directory);
    let calls: Vec<CallSummary<'_>> = Vec::new();
    // 6 MiB 文本：超过 Harness 的 4 MiB 上限，但溢出文件仍然写全。
    let big = "x".repeat(6 * 1024 * 1024);
    let result = ok_result(None, vec![text_item(&big)]);
    let mut format_options = options(&write, &calls);
    format_options.max_output_tokens = 2_000_000.0;
    let formatted = format_result(&result, format_options)
        .await
        .expect("the result formats");
    let text = content_text(&formatted.content);
    assert!(text.contains("Warning: truncated output"));
    assert!(text.len() <= 1024 * 1024 + 1000, "{}", text.len());
    let path = formatted.full_output_path.clone().expect("the spill file");
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file").len(),
        6 * 1024 * 1024
    );
    // 200,000 条短行：字节数在上限内，超过 100,000 行的上限。
    let lines = std::iter::repeat_n("a", 200_000)
        .collect::<Vec<_>>()
        .join("\n");
    let result = ok_result(None, vec![text_item(&lines)]);
    let mut format_options = options(&write, &calls);
    format_options.max_output_tokens = 2_000_000.0;
    let formatted = format_result(&result, format_options)
        .await
        .expect("the result formats");
    let text = content_text(&formatted.content);
    assert!(text.contains("Warning: truncated output"));
    assert!(text.split('\n').count() < 100_000);
    assert!(formatted.full_output_path.is_some());
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_images_to_files_and_names_them_next_to_the_image() {
    let _guard = test_gate();
    let directory = temp_dir("images");
    let (write, _) = writer(&directory);
    let calls: Vec<CallSummary<'_>> = Vec::new();
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let result = ok_result(None, vec![CodemodeOutputItem::image(png, "image/png")]);
    let formatted = format_result(&result, options(&write, &calls))
        .await
        .expect("the result formats");
    let text = content_text(&formatted.content);
    assert!(
        text.contains("(image/png, 70 bytes)"),
        "the label names the type and size: {text}"
    );
    assert!(
        formatted
            .content
            .iter()
            .any(|item| matches!(item, UserContentBlock::Image(_)))
    );
    let saved = formatted
        .content
        .iter()
        .find_map(|item| match item {
            UserContentBlock::Image(image) => Some(image.data.clone()),
            _ => None,
        })
        .expect("the image survives");
    assert_eq!(saved, png);
    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_failed_script_with_its_error_and_the_calls_already_made() {
    let _guard = test_gate();
    let directory = temp_dir("failed");
    let (write, _) = writer(&directory);
    let calls = [CallSummary {
        name: "echo",
        status: "ok",
    }];
    let result = CodemodeResult::Err(CodemodeResultErr::new(
        CodemodeError {
            kind: CodemodeErrorKind::Script,
            name: Some("Error".to_owned()),
            message: "boom".to_owned(),
            stack: Some("Error: boom\n    at <anonymous>:1:1".to_owned()),
        },
        vec![text_item("a")],
        Vec::new(),
        None,
    ));
    let formatted = format_result(&result, options(&write, &calls))
        .await
        .expect("the result formats");
    let text = content_text(&formatted.content);
    assert!(text.starts_with("Script failed\n"), "{text}");
    assert!(text.contains("Error: boom"));
    assert!(text.contains("Tool calls made before the failure (they are not undone): echo (ok)"));
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// 通过完整 Harness 跑脚本
// ---------------------------------------------------------------------------

fn sandbox() -> Arc<dyn kiana_durable::codemode::codemode::CodemodeSandbox> {
    let sandbox =
        kiana_codemode::CodemodeSandbox::new(kiana_codemode::types::CodemodeSandboxOptions {
            base: kiana_codemode::types::CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some(64.0 * 1024.0 * 1024.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("the sandbox");
    Arc::new(sandbox) as Arc<dyn kiana_durable::codemode::codemode::CodemodeSandbox>
}

fn text_output(text: &str) -> Vec<UserContentBlock> {
    vec![UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_owned(),
        text_signature: None,
    })]
}

/// 一个计数运行次数、返回 `echo <text>` 的工具。
fn echo_tool(calls: Arc<Mutex<Vec<String>>>) -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |arguments, _api, _context| {
        let text = arguments
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            lock(&calls).push(text.clone());
            Ok(ToolExecutionResult {
                output: Some(text_output(&format!("echo {text}"))),
                ..Default::default()
            })
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = base_tool("echo");
    registration.execute = Some(execute);
    registration
}

/// 一个一直等到调用被中止的工具（TS 的 `hold`）。
fn hold_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |_arguments, _api, context| {
        Box::pin(async move {
            match context.abort_signal() {
                Some(signal) => signal.token().cancelled().await,
                None => std::future::pending::<()>().await,
            }
            Ok(ToolExecutionResult {
                output: Some(text_output("held")),
                ..Default::default()
            })
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = base_tool("hold");
    registration.execute = Some(execute);
    registration
}

fn codemode_chat() -> (ChatSetup, Arc<Mutex<Vec<String>>>) {
    let setup = ChatSetup::new();
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let _ = add_tool(&setup.registry, echo_tool(Arc::clone(&calls)));
    let _ = add_tool(&setup.registry, hold_tool());
    setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions::new(
            sandbox(),
        )),
    );
    (setup, calls)
}

async fn result_text(root: &Arc<dyn ConversationTrait>) -> (String, bool) {
    let entries = chat_support::all_entries(root, &*context()).await;
    let entry = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("a tool result");
    let message = entry
        .model
        .as_ref()
        .and_then(|model| model.first())
        .expect("the message");
    match message {
        kiana_ai::types::Message::ToolResult(result) => {
            let text = result
                .content
                .iter()
                .map(|item| match item {
                    UserContentBlock::Text(text) => text.text.clone(),
                    UserContentBlock::Image(_) => String::new(),
                })
                .collect::<String>();
            let trimmed = strip_header(&text);
            (trimmed, result.is_error)
        }
        _ => (String::new(), false),
    }
}

fn strip_header(text: &str) -> String {
    let rest = text
        .strip_prefix("Script completed\n")
        .or_else(|| text.strip_prefix("Script failed\n"));
    let Some(rest) = rest else {
        return text.to_owned();
    };
    let Some(rest) = rest.strip_prefix("Wall time ") else {
        return text.to_owned();
    };
    let Some((_, rest)) = rest.split_once(" seconds\nOutput:\n") else {
        return text.to_owned();
    };
    rest.to_owned()
}

async fn run_once(root: &Arc<dyn ConversationTrait>) {
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_a_script_that_calls_tools_as_nested_calls() {
    let _guard = test_gate();
    let (setup, calls) = codemode_chat();
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            "const a = await tools.echo({ text: \"a\" }); const b = await tools.echo({ text: \"b\" }); return [a, b];",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    run_once(&root).await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "[\"echo a\",\"echo b\"]");
    assert!(!is_error);
    assert_eq!(*lock(&calls), vec!["a".to_owned(), "b".to_owned()]);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_the_scripts_tool_calls_in_its_results_details() {
    let _guard = test_gate();
    let (setup, _) = codemode_chat();
    let fails: ToolExecuteFn = Arc::new(move |_arguments, _api, _context| {
        Box::pin(async move {
            Ok(ToolExecutionResult {
                output: Some(text_output("permission denied")),
                is_error: Some(true),
                ..Default::default()
            })
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut fails_tool = base_tool("fails");
    fails_tool.execute = Some(fails);
    let _ = add_tool(&setup.registry, fails_tool);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            "await tools.echo({ text: \"a\" }); try { await tools.fails({}); } catch {} tools.hold({ id: 9 }); return 1;",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    run_once(&root).await;
    let entries = chat_support::all_entries(&root, &*context()).await;
    let entry = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("a tool result");
    let message = match entry
        .model
        .as_ref()
        .and_then(|model| model.first())
        .expect("the message")
    {
        kiana_ai::types::Message::ToolResult(result) => result,
        _ => panic!("a tool result message"),
    };
    let details = message.details.clone().expect("details");
    // TS 的 `toMatchObject`：额外键（`durationMs`）允许存在。
    assert_eq!(details["calls"][0]["id"], json!(1));
    assert_eq!(details["calls"][0]["name"], json!("echo"));
    assert_eq!(details["calls"][0]["args"], json!("{\"text\":\"a\"}"));
    assert_eq!(details["calls"][0]["status"], json!("ok"));
    assert!(details["calls"][0]["durationMs"].is_number());
    assert_eq!(details["calls"][1]["name"], json!("fails"));
    assert_eq!(details["calls"][1]["status"], json!("error"));
    assert_eq!(details["calls"][1]["error"], json!("permission denied"));
    assert_eq!(details["calls"][2]["name"], json!("hold"));
    assert_eq!(details["calls"][2]["status"], json!("cancelled"));
    harness.close(&*context()).await;
}

/// 一个返回固定文本、可选带 isError/structuredOutput 的工具。
fn fixed_tool(
    name: &str,
    text: &str,
    is_error: bool,
    structured: Option<JsonValue>,
    structured_schema: Option<JsonValue>,
    output: Option<Vec<UserContentBlock>>,
) -> ToolRegistration {
    let text = text.to_owned();
    let execute: ToolExecuteFn = Arc::new(move |_arguments, _api, _context| {
        let text = text.clone();
        let structured = structured.clone();
        let output = output.clone();
        Box::pin(async move {
            Ok(ToolExecutionResult {
                output: output.or_else(|| Some(text_output(&text))),
                structured_output: structured,
                is_error: if is_error { Some(true) } else { None },
                ..Default::default()
            })
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = base_tool(name);
    registration.execute = Some(execute);
    registration.structured_output_schema = structured_schema;
    registration
}

async fn run_script_setup(
    setup: &ChatSetup,
    code: &str,
) -> (
    Arc<dyn ConversationTrait>,
    Arc<kiana_durable::harness::harness::HarnessImpl>,
) {
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(code)),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), setup).await;
    run_once(&root).await;
    (root, harness)
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_failed_tool_call_with_its_error_text_and_structured_output() {
    let _guard = test_gate();
    let (setup, _) = codemode_chat();
    let _ = add_tool(
        &setup.registry,
        fixed_tool("fails", "permission denied", true, None, None, None),
    );
    let (root, harness) = run_script_setup(
        &setup,
        "try { await tools.fails({}); } catch (e) { return [e.message, e.structuredOutput]; }",
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "[\"permission denied\",\"permission denied\"]");
    assert!(!is_error);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resolves_a_schema_tools_error_result_that_carries_its_value_and_rejects_one_without_its_text()
 {
    let _guard = test_gate();
    let (setup, _) = codemode_chat();
    let schema = json!({
        "type": "object",
        "required": ["output", "exitCode"],
        "properties": {
            "output": { "type": "string" },
            "exitCode": { "type": "number" }
        }
    });
    let _ = add_tool(
        &setup.registry,
        fixed_tool(
            "run",
            "no matches",
            true,
            Some(json!({ "output": "no matches", "exitCode": 1 })),
            Some(schema.clone()),
            None,
        ),
    );
    let _ = add_tool(
        &setup.registry,
        fixed_tool(
            "denied",
            "permission denied",
            true,
            None,
            Some(schema),
            None,
        ),
    );
    let (root, harness) = run_script_setup(
        &setup,
        "const r = await tools.run({});\nlet e; try { await tools.denied({}); } catch (error) { e = [error.message, error.structuredOutput ?? null]; }\nreturn [r.exitCode, r.output, e];",
    )
    .await;
    let (text, _) = result_text(&root).await;
    assert_eq!(text, "[1,\"no matches\",[\"permission denied\",null]]");
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resolves_a_schemaless_tool_with_its_text_also_when_it_returned_images() {
    let _guard = test_gate();
    let (setup, _) = codemode_chat();
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let image = UserContentBlock::Image(kiana_ai::types::ImageContent {
        kind_image: kiana_ai::types::ImageKind::Image,
        data: png.to_owned(),
        mime_type: "image/png".to_owned(),
    });
    let with_caption: Vec<UserContentBlock> = vec![
        UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "a cat".to_owned(),
            text_signature: None,
        }),
        image.clone(),
    ];
    let only_image = vec![image];
    // 按 caption 参数选择输出：两个工具调用结果不同，所以分成两个脚本。
    let _ = add_tool(
        &setup.registry,
        fixed_tool("picture1", "", false, None, None, Some(with_caption)),
    );
    let _ = add_tool(
        &setup.registry,
        fixed_tool("picture2", "", false, None, None, Some(only_image)),
    );
    let (root, harness) = run_script_setup(
        &setup,
        "return [await tools.picture1({}), await tools.picture2({})];",
    )
    .await;
    let (text, _) = result_text(&root).await;
    assert_eq!(text, "[\"a cat\",\"\"]");
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_a_deferred_tool_the_model_is_not_offered_and_finds_it_in_all_tools() {
    let _guard = test_gate();
    let (setup, _) = codemode_chat();
    let mut later = fixed_tool("later", "later ran", false, None, None, None);
    later.deferred = true;
    let _ = add_tool(&setup.registry, later);
    let (root, harness) = run_script_setup(
        &setup,
        "return [await tools.later({}), ALL_TOOLS.some((tool) => tool.name === \"later\")];",
    )
    .await;
    let (text, _) = result_text(&root).await;
    assert_eq!(text, "[\"later ran\",true]");
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn finds_tools_with_discovery_globals_and_lists_them_in_all_tools() {
    let _guard = test_gate();
    let setup = ChatSetup::new();
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let _ = add_tool(&setup.registry, echo_tool(Arc::clone(&calls)));
    let mut list_issues = fixed_tool(
        "mcp__github__list_issues",
        "",
        false,
        None,
        None,
        Some(text_output("issues")),
    );
    list_issues.tool.description = "List issues.".to_owned();
    let _ = add_tool(&setup.registry, list_issues);
    setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions {
            sandbox: sandbox(),
            namespaces: Some(vec![kiana_durable::codemode::describe::CodemodeNamespace {
                name: "github".to_owned(),
                description: Some("GitHub MCP server".to_owned()),
                instructions: None,
                prefix: "mcp__github__".to_owned(),
            }]),
            inline_budget: None,
            models: None,
            globals: None,
        }),
    );
    let (root, harness) = run_script_setup(
        &setup,
        "const found = await searchTools(\"issues\");\nconst inNamespace = await searchTools(\"echo\", { namespace: \"github\" });\nreturn {\n  found: found.map((tool) => tool.name),\n  inNamespace,\n  described: (await describeTool(\"echo\")).includes(\"echo(args:\"),\n  missing: (await describeTool(\"nope\")) ?? null,\n  namespace: await describeNamespace(\"github\"),\n  all: ALL_TOOLS.map((tool) => tool.name).includes(\"mcp__github__list_issues\"),\n};",
    )
    .await;
    let (text, _) = result_text(&root).await;
    let value: JsonValue = serde_json::from_str(&text).expect("the returned object");
    assert_eq!(
        value,
        json!({
            "found": ["mcp__github__list_issues"],
            "inNamespace": [],
            "described": true,
            "missing": null,
            "namespace": { "name": "github", "description": "GitHub MCP server", "tools": ["mcp__github__list_issues"] },
            "all": true
        })
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_store_values_per_conversation() {
    let _guard = test_gate();
    let (setup, _) = codemode_chat();
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script("store(\"k\", 1); return load(\"k\")")),
        FauxResponseStep::Message(done()),
        FauxResponseStep::Message(script("return [load(\"k\") ?? null]")),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    run_once(&root).await;
    run_once(&root).await;
    let entries = chat_support::all_entries(&root, &*context()).await;
    let texts: Vec<String> = entries
        .iter()
        .filter(|entry| entry.kind == "kiana.tool-result")
        .map(|entry| {
            let message = entry
                .model
                .as_ref()
                .and_then(|model| model.first())
                .expect("the message");
            match message {
                kiana_ai::types::Message::ToolResult(result) => strip_header(
                    &result
                        .content
                        .iter()
                        .map(|item| match item {
                            UserContentBlock::Text(text) => text.text.clone(),
                            UserContentBlock::Image(_) => String::new(),
                        })
                        .collect::<String>(),
                ),
                _ => String::new(),
            }
        })
        .collect();
    assert_eq!(texts, vec!["1".to_owned(), "[1]".to_owned()]);
    harness.close(&*context()).await;
}

/// 与 TS 的 `wait` 全局函数相同：第 50 个调用放出全部。
struct WaitGlobal {
    release: tokio::sync::watch::Sender<bool>,
    waiting: Arc<AtomicUsize>,
}

impl kiana_durable::codemode::codemode::CodemodeGlobal for WaitGlobal {
    fn name(&self) -> &str {
        "wait"
    }

    fn spread(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        _args: Option<JsonValue>,
        _api: Arc<dyn kiana_durable::harness::types::ToolExecutionApi>,
        _context: Arc<dyn Context>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsonValue>, BoxError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut receiver = self.release.subscribe();
            if self.waiting.fetch_add(1, Ordering::SeqCst) + 1 == 50 {
                let _ = self.release.send(true);
            }
            while !*receiver.borrow() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
            Ok(None)
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn journals_results_that_finish_together_in_one_commit() {
    let _guard = test_gate();
    let setup = ChatSetup::new();
    let (release, _receiver) = tokio::sync::watch::channel(false);
    let wait = Arc::new(WaitGlobal {
        release,
        waiting: Arc::new(AtomicUsize::new(0)),
    });
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            "return (await Promise.all(Array.from({ length: 50 }, () => wait()))).length",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let journal_commits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&journal_commits);
    setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions {
            sandbox: sandbox(),
            namespaces: None,
            inline_budget: None,
            models: None,
            globals: Some(vec![wait]),
        }),
    );
    let (harness, root) = open_chat(memory(), &setup).await;
    let _subscription = harness.subscribe_commits(Arc::new(
        move |publication: &kiana_durable::types::CommitPublication, _context: &dyn Context| {
            for change in &publication.changes {
                if let kiana_durable::types::CommitChange::Document(
                    kiana_durable::types::DocumentCommitChange::Document { record, .. },
                ) = change
                    && record.kind == "kiana.codemode.journal"
                {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            }
        },
    ));
    run_once(&root).await;
    let (text, _) = result_text(&root).await;
    assert_eq!(text, "50");
    // journal 的创建与分组提交，而不是每个结果一次（51）。
    assert!(
        journal_commits.load(Ordering::SeqCst) < 10,
        "{}",
        journal_commits.load(Ordering::SeqCst)
    );
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// 重启：关掉 Harness 再用同一份存储打开，调度器恢复被中断的 codemode 任务
// ---------------------------------------------------------------------------

/// 计数启动次数的沙箱包装（TS 里替换 `sandbox.start` 的 `vm.starts`）。
struct CountingSandbox {
    inner: Arc<kiana_codemode::CodemodeSandbox>,
    starts: Arc<AtomicUsize>,
}

impl CodemodeSandbox for CountingSandbox {
    fn start(&self, code: &str, options: CodemodeRunOptions) -> Result<CodemodeRun, StartError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.inner.start(code, options)
    }
}

/// 重启夹具里的 `hold`：`block` 时等到被中止，并记下第一次到达与运行次数。
#[derive(Clone)]
struct RestartHolds {
    block: Arc<AtomicBool>,
    /// 当前这一次重开用的闭锁（TS 的 `chat.holds.reached = deferred()` 可以整个换掉）。
    reached: Arc<Mutex<Arc<tokio::sync::Semaphore>>>,
    runs: Arc<AtomicUsize>,
}

impl RestartHolds {
    /// 换一个新的闭锁：重启后 `hold` 再到达一次。
    fn reset_reached(&self) {
        *lock(&self.reached) = Arc::new(tokio::sync::Semaphore::new(0));
    }

    /// 等 `hold` 到达（或重跑时再次到达）。
    async fn wait_reached(&self) {
        let semaphore = lock(&self.reached).clone();
        semaphore.acquire().await.expect("hold is reached").forget();
    }
}

fn restart_hold_tool(holds: &RestartHolds) -> ToolRegistration {
    let block = Arc::clone(&holds.block);
    let reached = Arc::clone(&holds.reached);
    let runs = Arc::clone(&holds.runs);
    let execute: ToolExecuteFn = Arc::new(move |arguments, _api, context| {
        let block = Arc::clone(&block);
        let reached = Arc::clone(&reached);
        let runs = Arc::clone(&runs);
        let id = arguments.get("id").and_then(JsonValue::as_i64);
        Box::pin(async move {
            runs.fetch_add(1, Ordering::SeqCst);
            if block.load(Ordering::SeqCst) {
                lock(&reached).add_permits(1);
                match context.abort_signal() {
                    Some(signal) => signal.token().cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            }
            let text = match id {
                Some(id) => format!("held {id}"),
                None => "held ".to_owned(),
            };
            Ok(ToolExecutionResult {
                output: Some(text_output(&text)),
                ..Default::default()
            })
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = base_tool("hold");
    registration.execute = Some(execute);
    registration.replay = Some(kiana_durable::harness::types::ReplayPolicy::Safe);
    registration
}

struct RestartChat {
    setup: ChatSetup,
    storage: Arc<support::MemoryStorage>,
    holds: RestartHolds,
    starts: Arc<AtomicUsize>,
    echo: Arc<Mutex<Vec<String>>>,
    slows: Arc<Slows>,
    /// 具体沙箱：`close()` 只在这个类型上（宿主关掉沙箱的用例用）。
    sandbox: Arc<kiana_codemode::CodemodeSandbox>,
}

fn restart_chat() -> RestartChat {
    restart_chat_with(Vec::new())
}

fn restart_chat_with(
    globals: Vec<Arc<dyn kiana_durable::codemode::codemode::CodemodeGlobal>>,
) -> RestartChat {
    let setup = ChatSetup::new();
    let echo: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let _ = add_tool(&setup.registry, echo_tool(Arc::clone(&echo)));
    let holds = RestartHolds {
        block: Arc::new(AtomicBool::new(true)),
        reached: Arc::new(Mutex::new(Arc::new(tokio::sync::Semaphore::new(0)))),
        runs: Arc::new(AtomicUsize::new(0)),
    };
    let _ = add_tool(&setup.registry, restart_hold_tool(&holds));
    let slows = Arc::new(Slows::default());
    let _ = add_tool(&setup.registry, slow_tool(Arc::clone(&slows)));
    let starts = Arc::new(AtomicUsize::new(0));
    let inner = Arc::new(
        kiana_codemode::CodemodeSandbox::new(kiana_codemode::types::CodemodeSandboxOptions {
            base: kiana_codemode::types::CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some(64.0 * 1024.0 * 1024.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("the sandbox"),
    );
    let sandbox: Arc<dyn CodemodeSandbox> = Arc::new(CountingSandbox {
        inner: Arc::clone(&inner),
        starts: Arc::clone(&starts),
    });
    setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions {
            sandbox,
            namespaces: None,
            inline_budget: None,
            models: None,
            globals: if globals.is_empty() {
                None
            } else {
                Some(globals)
            },
        }),
    );
    RestartChat {
        setup,
        storage: Arc::new(support::MemoryStorage::new()),
        holds,
        starts,
        echo,
        slows,
        sandbox: inner,
    }
}

/// 跑 `code` 直到 `hold` 到达，关掉 Harness（重启），放开 `hold`，用同一份存储重开并恢复。
async fn restart(
    chat: &RestartChat,
    code: &str,
) -> (
    Arc<kiana_durable::harness::harness::HarnessImpl>,
    Arc<dyn ConversationTrait>,
) {
    restart_hooked(chat, code, RestartHooks::default()).await
}

async fn restart_hooked(
    chat: &RestartChat,
    code: &str,
    hooks: RestartHooks,
) -> (
    Arc<kiana_durable::harness::harness::HarnessImpl>,
    Arc<dyn ConversationTrait>,
) {
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(code)),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    let id = submission.id();
    chat.holds.wait_reached().await;
    if let Some(before_close) = hooks.before_close {
        let task = script_task(&harness).await;
        before_close(Arc::clone(&harness), task).await;
    }
    harness.close(&*context()).await;
    chat.storage.reopen();
    chat.holds.block.store(false, Ordering::SeqCst);
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    if let Some(between) = hooks.between {
        let task = script_task(&harness).await;
        between(Arc::clone(&harness), task).await;
    }
    harness.resume().expect("resume");
    let settled = harness
        .submission(id, &*context())
        .await
        .expect("submission")
        .expect("the submission")
        .wait(&*context())
        .await
        .expect("the submission settles");
    assert_eq!(settled.record().status, SubmissionStatus::Done);
    (harness, root)
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_a_script_after_a_restart_without_running_its_finished_calls_again() {
    let _guard = test_gate();
    let chat = restart_chat();
    let (harness, root) = restart(
        &chat,
        "const a = await tools.echo({ text: \"a\" });\nconst held = await tools.hold({ id: 1 });\nconst b = await tools.echo({ text: \"b\" });\nreturn [a, held, b, Date.now() > 0];",
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "[\"echo a\",\"held 1\",\"echo b\",true]");
    assert!(!is_error);
    let calls = lock(&chat.echo).clone();
    assert_eq!(calls.iter().filter(|text| *text == "a").count(), 1);
    assert_eq!(calls.iter().filter(|text| *text == "b").count(), 1);
    // VM 跑了两次；replay-safe 的 hold 在重启后重跑了一次。
    assert_eq!(chat.starts.load(Ordering::SeqCst), 2);
    assert_eq!(chat.holds.runs.load(Ordering::SeqCst), 2);
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// 重启用例的夹具：慢工具、全局函数、提交回调用的事务适配器与文档修改
// ---------------------------------------------------------------------------

/// r#"slow"#：等到 r#"release(id)"#，按完成顺序记录，返回 r#"slow <id>"#。
#[derive(Default)]
struct Slows {
    open: Mutex<std::collections::HashMap<i64, tokio::sync::watch::Sender<bool>>>,
    done: Mutex<std::collections::HashMap<i64, tokio::sync::watch::Sender<bool>>>,
    finished: Mutex<Vec<i64>>,
}

impl Slows {
    fn sender(
        map: &Mutex<std::collections::HashMap<i64, tokio::sync::watch::Sender<bool>>>,
        id: i64,
    ) -> tokio::sync::watch::Sender<bool> {
        let mut map = lock(map);
        map.entry(id)
            .or_insert_with(|| tokio::sync::watch::channel(false).0)
            .clone()
    }

    fn release(&self, id: i64) {
        Self::sender(&self.open, id).send_replace(true);
    }

    fn gate(&self, id: i64) -> tokio::sync::watch::Receiver<bool> {
        Self::sender(&self.open, id).subscribe()
    }

    fn finished_call(&self, id: i64) -> tokio::sync::watch::Receiver<bool> {
        Self::sender(&self.done, id).subscribe()
    }

    fn finish(&self, id: i64) {
        lock(&self.finished).push(id);
        Self::sender(&self.done, id).send_replace(true);
    }
}

fn slow_tool(slows: Arc<Slows>) -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |arguments, _api, _context| {
        let slows = Arc::clone(&slows);
        let id = arguments.get("id").and_then(JsonValue::as_i64).unwrap_or(0);
        Box::pin(async move {
            let mut gate = slows.gate(id);
            while !*gate.borrow() {
                if gate.changed().await.is_err() {
                    break;
                }
            }
            slows.finish(id);
            Ok(ToolExecutionResult {
                output: Some(text_output(&format!("slow {id}"))),
                ..Default::default()
            })
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = base_tool("slow");
    registration.execute = Some(execute);
    registration
}

type RestartHook = Box<
    dyn FnOnce(
            Arc<kiana_durable::harness::harness::HarnessImpl>,
            TaskId,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send,
>;

/// 重启前后可插入的步骤：r#"before_close"# 在关掉 Harness 之前，r#"between"# 在重开之后、恢复之前。
#[derive(Default)]
struct RestartHooks {
    before_close: Option<RestartHook>,
    between: Option<RestartHook>,
}

/// 把 r#"async (tx) => value"# 的提交回调包成 [r#"SessionChange"#]。
struct TestChange<F> {
    run: F,
}

#[async_trait::async_trait]
impl<F, T> SessionChange for TestChange<F>
where
    F: for<'t> Fn(
            &'t dyn Tx,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, BoxError>> + Send + 't>,
        > + Send
        + Sync,
    T: Send + 'static,
{
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        Ok(Box::new((self.run)(tx).await?))
    }
}

async fn harness_commit<T, F>(
    harness: &kiana_durable::harness::harness::HarnessImpl,
    run: F,
) -> Result<T, BoxError>
where
    F: for<'t> Fn(
            &'t dyn Tx,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, BoxError>> + Send + 't>,
        > + Send
        + Sync,
    T: Send + 'static,
{
    let boxed = harness
        .commit(&TestChange { run }, &*context(), CommitOptions::default())
        .await?;
    match boxed.downcast::<T>() {
        Ok(value) => Ok(*value),
        Err(_) => Err("commit() did not return its value".into()),
    }
}

fn tool_task_query() -> TaskQuery {
    TaskQuery {
        conversation_id: None,
        kind: Some(TOOL_TASK.definition.name.clone()),
        status: None,
        abort_requested: None,
        background: None,
        order: None,
    }
}

async fn all_tasks(harness: &kiana_durable::harness::harness::HarnessImpl) -> Vec<TaskRecord> {
    harness_commit(harness, |tx| {
        Box::pin(async move { Ok(tx.scan_tasks(&tool_task_query(), 100, None).await?.items) })
    })
    .await
    .expect("the tasks")
}

async fn script_task(harness: &kiana_durable::harness::harness::HarnessImpl) -> TaskId {
    all_tasks(harness)
        .await
        .iter()
        .find(|task| task.input.get("kind") == Some(&json!("model")))
        .expect("the model task")
        .id
}

async fn nested_count(harness: &kiana_durable::harness::harness::HarnessImpl) -> usize {
    all_tasks(harness)
        .await
        .iter()
        .filter(|task| task.input.get("kind") == Some(&json!("nested")))
        .count()
}

/// 执行一次的文档修改回调。
type EditFn = Box<dyn FnOnce(&mut JsonObject) + Send>;

/// 一次性的文档修改，包成 [`SessionChange`]。
struct EditChange {
    token: kiana_durable::DocToken,
    task: TaskId,
    edit: Mutex<Option<EditFn>>,
}

#[async_trait::async_trait]
impl SessionChange for EditChange {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let mut doc = tx.doc(&DocTarget::task(&self.token, self.task)).await?;
        let edit = lock(&self.edit).take().expect("the edit runs once");
        edit(doc.value_mut());
        Ok(Box::new(()))
    }
}

async fn edit_doc(
    harness: &kiana_durable::harness::harness::HarnessImpl,
    token: &kiana_durable::DocToken,
    task: TaskId,
    edit: impl FnOnce(&mut JsonObject) + Send + 'static,
) {
    let change = EditChange {
        token: token.clone(),
        task,
        edit: Mutex::new(Some(Box::new(edit))),
    };
    harness
        .commit(&change, &*context(), CommitOptions::default())
        .await
        .expect("the document edit commits");
}

async fn edit_journal(
    harness: &kiana_durable::harness::harness::HarnessImpl,
    task: TaskId,
    edit: impl FnOnce(&mut JsonObject) + Send + Sync + 'static,
) {
    edit_doc(harness, &CODEMODE_JOURNAL_DOC, task, edit).await;
}

async fn edit_run(
    harness: &kiana_durable::harness::harness::HarnessImpl,
    task: TaskId,
    edit: impl FnOnce(&mut JsonObject) + Send + Sync + 'static,
) {
    edit_doc(harness, &CODEMODE_RUN_DOC, task, edit).await;
}

fn journal_entries(journal: &mut JsonObject) -> &mut Vec<JsonValue> {
    journal
        .get_mut("entries")
        .and_then(JsonValue::as_array_mut)
        .expect("the journal's entries")
}

fn set_store(run: &mut JsonObject, store: JsonValue) {
    run.get_mut("start")
        .and_then(JsonValue::as_object_mut)
        .expect("the run's start")
        .insert("store".to_owned(), store);
}

async fn journal_issued_after(
    harness: &kiana_durable::harness::harness::HarnessImpl,
    task: TaskId,
) -> Option<JsonValue> {
    let snapshot = harness
        .snapshot(&DocTarget::task(&CODEMODE_JOURNAL_DOC, task), &*context())
        .await
        .expect("the journal snapshot")?;
    snapshot
        .get("entries")?
        .as_array()?
        .first()?
        .get("issuedAfter")
        .cloned()
}

async fn journal_first_issued(
    harness: &kiana_durable::harness::harness::HarnessImpl,
    task: TaskId,
) -> Option<JsonValue> {
    harness
        .snapshot(&DocTarget::task(&CODEMODE_JOURNAL_DOC, task), &*context())
        .await
        .expect("the journal snapshot")?
        .get("firstIssued")
        .cloned()
}

/// 闭包实现的一个脚本全局函数。
type GlobalFn = dyn Fn(
        Option<JsonValue>,
        Arc<dyn ToolExecutionApi>,
        Arc<dyn Context>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsonValue>, BoxError>> + Send>,
    > + Send
    + Sync;

struct FnGlobal {
    name: &'static str,
    run: Arc<GlobalFn>,
}

impl CodemodeGlobal for FnGlobal {
    fn name(&self) -> &str {
        self.name
    }

    fn execute<'a>(
        &'a self,
        args: Option<JsonValue>,
        api: Arc<dyn ToolExecutionApi>,
        context: Arc<dyn Context>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsonValue>, BoxError>> + Send + 'a>,
    > {
        (self.run)(args, api, context)
    }
}

fn global(
    name: &'static str,
    run: impl Fn(
        Option<JsonValue>,
        Arc<dyn ToolExecutionApi>,
        Arc<dyn Context>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsonValue>, BoxError>> + Send>,
    > + Send
    + Sync
    + 'static,
) -> Arc<dyn CodemodeGlobal> {
    Arc::new(FnGlobal {
        name,
        run: Arc::new(run),
    })
}

fn value_global(name: &'static str, value: JsonValue) -> Arc<dyn CodemodeGlobal> {
    global(name, move |_args, _api, _context| {
        let value = value.clone();
        Box::pin(async move { Ok(Some(value)) })
    })
}

fn undefined_global(name: &'static str) -> Arc<dyn CodemodeGlobal> {
    global(name, |_args, _api, _context| {
        Box::pin(async { Ok::<Option<JsonValue>, BoxError>(None) })
    })
}

/// 每次调用加一并返回的 r#"next"#。
fn counter_global(name: &'static str, counter: Arc<AtomicI64>) -> Arc<dyn CodemodeGlobal> {
    global(name, move |_args, _api, _context| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            let next = counter.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(Some(json!(next)))
        })
    })
}

// ---------------------------------------------------------------------------
// 重放顺序、全局函数、时钟与随机
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn replays_results_in_the_order_they_arrived_not_the_order_the_calls_were_made() {
    let _guard = test_gate();
    let chat = restart_chat();
    let code = r#"const order = [];
        await Promise.all([tools.slow({ id: 1 }).then(() => order.push(1)), tools.slow({ id: 2 }).then(() => order.push(2))]);
        await tools.echo({ text: order.join(",") });
        await tools.hold({});
        return order;"#;
    // 2 先完成，然后 1。
    chat.slows.release(2);
    let slows = Arc::clone(&chat.slows);
    tokio::spawn(async move {
        let mut finished = slows.finished_call(2);
        while !*finished.borrow() {
            if finished.changed().await.is_err() {
                break;
            }
        }
        slows.release(1);
    });
    let (harness, root) = restart(&chat, code).await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "[2,1]");
    assert!(!is_error);
    assert_eq!(*lock(&chat.echo), vec!["2,1".to_owned()]);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn journals_globals_with_their_results_and_hands_a_replay_the_recorded_ones() {
    let _guard = test_gate();
    let counter = Arc::new(AtomicI64::new(0));
    let chat = restart_chat_with(vec![counter_global("next", Arc::clone(&counter))]);
    let (harness, root) = restart(
        &chat,
        r#"const n = await next(); await tools.hold({}); return [n, await next()];"#,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    // 第一个 next() 只跑了一次，重放从 journal 取；第二个在重启之后跑。
    assert_eq!(text, "[1,2]");
    assert!(!is_error);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn journals_a_globals_null_argument_and_replays_it_as_null() {
    let _guard = test_gate();
    let counter = Arc::new(AtomicI64::new(0));
    let chat = restart_chat_with(vec![counter_global("next", Arc::clone(&counter))]);
    let (harness, root) = restart(
        &chat,
        r#"const first = await next(null); await tools.hold({}); return [first, await next(null)];"#,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    // journal 里的 `args: null` 读回仍是 `null`（不是缺失），重放认出是同一次调用，不判分叉。
    assert_eq!(text, "[1,2]");
    assert!(!is_error);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_scripts_clock_and_randomness_across_a_restart() {
    let _guard = test_gate();
    let chat = restart_chat();
    let (harness, root) = restart(
        &chat,
        r#"const before = [Date.now(), Math.random()];
        await tools.hold({});
        await tools.echo({ text: String(before[0]) + "/" + String(before[1]) });
        return "ok";"#,
    )
    .await;
    let (text, _) = result_text(&root).await;
    assert_eq!(text, "ok");
    // 一次 echo：参数相同，所以重跑没有再接纳第二个调用。
    assert_eq!(lock(&chat.echo).len(), 1);
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// 分叉检测
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_resumed_script_made_a_different_number_of_calls_than_before() {
    let _guard = test_gate();
    let chat = restart_chat();
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_journal(&harness, task, |journal| {
                    journal_entries(journal)[0]
                        .as_object_mut()
                        .expect("the entry")
                        .insert("issuedAfter".to_owned(), json!(7));
                })
                .await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"await tools.echo({ text: "a" }); await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("after result 1 it had made 2 calls, where it had made 7"),
        "{text}"
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_at_once_when_a_resumed_script_waits_without_making_a_call_it_made_before() {
    let _guard = test_gate();
    let chat = restart_chat();
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_journal(&harness, task, |journal| {
                    journal_entries(journal)[0]
                        .as_object_mut()
                        .expect("the entry")
                        .insert("id".to_owned(), json!(99));
                })
                .await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"await tools.echo({ text: "a" }); await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(text.contains("it waits without making call 99"), "{text}");
    // hold 在重启前已被接纳，自己恢复；重放没有再接纳任何调用。
    assert_eq!(nested_count(&harness).await, 2);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_resumed_script_calls_a_global_with_other_arguments() {
    let _guard = test_gate();
    let chat = restart_chat_with(vec![counter_global("next", Arc::new(AtomicI64::new(0)))]);
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_journal(&harness, task, |journal| {
                    journal_entries(journal)[0]["global"]
                        .as_object_mut()
                        .expect("the global call")
                        .insert("args".to_owned(), json!("other"));
                })
                .await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) =
        restart_hooked(&chat, r#"await next(); await tools.hold({});"#, hooks).await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(text.contains("call 1 is next"), "{text}");
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admits_no_call_of_a_diverging_replay_even_one_the_script_never_made_before() {
    let _guard = test_gate();
    let chat = restart_chat();
    // 设了 k 时，重放在结果 1 之后多发三个调用，实时运行没发过；计数在结果 1 的确认里才显出分叉。
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": 3 }))).await;
            })
        })),
        before_close: Some(Box::new(|harness, task| {
            Box::pin(async move {
                chat_support::eventually(|| async {
                    journal_issued_after(&harness, task).await == Some(json!(2))
                })
                .await
                .expect("the live run acknowledged result 1");
            })
        })),
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"await tools.echo({ text: "a" });
        const extra = load("k") ?? 0;
        for (let i = 0; i < extra; i++) tools.echo({ text: "extra" + i });
        await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("after result 1 it had made 5 calls, where it had made 2"),
        "{text}"
    );
    assert_eq!(*lock(&chat.echo), vec!["a".to_owned()]);
    assert_eq!(nested_count(&harness).await, 2);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_resumed_script_ends_where_it_went_on_before() {
    let _guard = test_gate();
    let chat = restart_chat();
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": true }))).await;
            })
        })),
        before_close: Some(Box::new(|harness, task| {
            Box::pin(async move {
                chat_support::eventually(|| async {
                    journal_issued_after(&harness, task).await == Some(json!(2))
                })
                .await
                .expect("the live run acknowledged result 1");
            })
        })),
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"await tools.echo({ text: "a" }); if (load("k")) return "early"; await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("it ended after result 1, where it went on before"),
        "{text}"
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_resumed_script_ends_before_calls_the_live_run_made_even_without_a_recorded_count()
 {
    let _guard = test_gate();
    let chat = restart_chat();
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": true }))).await;
                // 就像实时运行在记录结果 1 的计数之前崩溃了。
                edit_journal(&harness, task, |journal| {
                    journal_entries(journal)[0]
                        .as_object_mut()
                        .expect("the entry")
                        .remove("issuedAfter");
                })
                .await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"await tools.echo({ text: "a" }); if (load("k")) return "early"; await tools.echo({ text: "b" }); await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("it ended after result 1, where it went on before"),
        "{text}"
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_resumed_script_makes_a_call_an_earlier_attempt_admitted_with_other_arguments()
 {
    let _guard = test_gate();
    let chat = restart_chat();
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| {
                    set_store(run, json!({ "k": "other" }))
                })
                .await;
                // 就像重启发生在第一次 journal 提交之前：重跑从头实时运行。
                edit_journal(&harness, task, |journal| {
                    journal_entries(journal).clear();
                })
                .await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"await tools.echo({ text: load("k") ?? "a" }); await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(text.contains("different path"), "{text}");
    assert!(text.contains("call 1 is tools.echo"), "{text}");
    assert_eq!(*lock(&chat.echo), vec!["a".to_owned()]);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admits_a_reruns_tool_call_with_a_null_argument_against_the_same_earlier_call() {
    let _guard = test_gate();
    let chat = restart_chat();
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                // 就像重启发生在第一次 journal 提交之前：重跑从头实时运行。
                edit_journal(&harness, task, |journal| {
                    journal_entries(journal).clear();
                })
                .await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"await tools.echo(null); await tools.hold({}); return "ok";"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    // `null` 按 `{}` 比较（TS 的 `call.args ?? {}`）：早前尝试的嵌套调用参数已是 `{}`，
    // 这次是同一个调用，复用它的结果而不是判分叉。
    assert_eq!(text, "ok");
    assert!(!is_error);
    assert_eq!(*lock(&chat.echo), vec![String::new()]);
    assert_eq!(nested_count(&harness).await, 2);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_a_reruns_calls_against_an_earlier_attempts_before_admitting_any_later_call() {
    let _guard = test_gate();
    let chat = restart_chat();
    // 设了 k 时，调用 1 的参数不同、调用 2 是另一个工具；调用 3 是新的。
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": "x" }))).await;
                edit_journal(&harness, task, |journal| {
                    journal_entries(journal).clear();
                })
                .await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"const k = load("k");
        tools.echo({ text: k ?? "a" });
        if (k) tools.echo({ text: "new" });
        await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    // 第一次运行已经比之前多发了调用。
    assert!(
        text.contains("its first run made 3 calls, where it had made 2"),
        "{text}"
    );
    assert_eq!(nested_count(&harness).await, 2);
    assert_eq!(*lock(&chat.echo), vec!["a".to_owned()]);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_rerun_makes_a_global_call_where_an_earlier_attempt_made_a_tool_call()
{
    let _guard = test_gate();
    let chat = restart_chat_with(vec![value_global("pick", json!(1))]);
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": true }))).await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"if (load("k")) { await pick(); await tools.echo({ text: "new" }); } else { await tools.hold({}); }"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("call 1 is pick(), but was tools.hold({})"),
        "{text}"
    );
    assert!(lock(&chat.echo).is_empty());
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_reruns_first_run_makes_fewer_calls_than_an_earlier_attempt_admitted()
{
    let _guard = test_gate();
    // 实时时第一次运行接纳 hold 1 与 hold 2。设了 k 后只发 hold 1，在它的结果之后发一个全局函数和一个
    // 新工具调用：全局函数的 ID 是 hold 2 的 key，释放之后的检查看不到。
    let chat = restart_chat_with(vec![value_global("pick", json!(1))]);
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": true }))).await;
            })
        })),
        before_close: Some(Box::new(|harness, task| {
            Box::pin(async move {
                chat_support::eventually(|| async {
                    journal_first_issued(&harness, task).await == Some(json!(2))
                })
                .await
                .expect("the live run recorded its first call count");
            })
        })),
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"const k = load("k");
        const first = tools.hold({ id: 1 });
        if (!k) tools.hold({ id: 2 });
        await first;
        await pick();
        await tools.echo({ text: "new" });"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("its first run made 1 calls, where it had made 2"),
        "{text}"
    );
    assert!(lock(&chat.echo).is_empty());
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_call_when_a_rerun_without_journal_entries_ends_in_its_first_run_where_it_went_on_before()
 {
    let _guard = test_gate();
    let chat = restart_chat();
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": true }))).await;
            })
        })),
        before_close: Some(Box::new(|harness, task| {
            Box::pin(async move {
                chat_support::eventually(|| async {
                    journal_first_issued(&harness, task).await == Some(json!(1))
                })
                .await
                .expect("the live run recorded its first call count");
            })
        })),
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"if (load("k")) { store("x", 1); return "early"; } await tools.hold({});"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("it ended after result 0, where it went on before"),
        "{text}"
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_every_call_a_rerun_holds_before_launching_any_of_them() {
    let _guard = test_gate();
    // 实时时调用 1 是永不完成的全局函数，调用 2 是 hold；设了 k 后调用 1 是新工具调用，调用 2 带别的
    // 参数等待。只检查调用 1 会在调用 2 的冲突暴露之前就启动它。
    let chat = restart_chat_with(vec![global("forever", |_args, _api, context| {
        Box::pin(async move {
            match context.abort_signal() {
                Some(signal) => signal.token().cancelled().await,
                None => std::future::pending::<()>().await,
            }
            Ok(Some(JsonValue::Null))
        })
    })]);
    let hooks = RestartHooks {
        between: Some(Box::new(|harness, task| {
            Box::pin(async move {
                edit_run(&harness, task, |run| set_store(run, json!({ "k": true }))).await;
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"const k = load("k");
        if (k) tools.echo({ text: "new" }); else forever();
        await tools.hold({ id: k ? 2 : 1 });"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(
        text.contains("call 2 is tools.hold({\"id\":2}), but was tools.hold({\"id\":1})"),
        "{text}"
    );
    assert!(lock(&chat.echo).is_empty());
    assert_eq!(nested_count(&harness).await, 1);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_a_globals_undefined_result_undefined_live_and_replayed() {
    let _guard = test_gate();
    let chat = restart_chat_with(vec![undefined_global("nothing")]);
    let (harness, root) = restart(
        &chat,
        r#"const before = await nothing(); await tools.hold({}); return [before === undefined, (await nothing()) === undefined];"#,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "[true,true]");
    assert!(!is_error);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn drops_a_globals_result_that_arrives_after_the_script_finished() {
    let _guard = test_gate();
    let (late, late_receiver) = tokio::sync::watch::channel::<Option<JsonValue>>(None);
    let chat = restart_chat_with(vec![
        global("slowly", move |_args, _api, _context| {
            let mut receiver = late_receiver.clone();
            Box::pin(async move {
                while receiver.borrow().is_none() {
                    if receiver.changed().await.is_err() {
                        break;
                    }
                }
                Ok(Some(json!("late")))
            })
        }),
        value_global("quickly", json!("quick")),
    ]);
    chat.holds.block.store(false, Ordering::SeqCst);
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            r#"const pending = slowly(); await quickly(); return 1;"#,
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    run_once(&root).await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "1");
    assert!(!is_error);
    // 脚本的工具调用已经结束；挂着的全局函数现在才解析。
    late.send_replace(Some(json!("late")));
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// 实时运行里的取消、提交与文档
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn aborts_a_call_still_running_when_the_script_ends() {
    let _guard = test_gate();
    let chat = restart_chat();
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            r#"tools.hold({}); await tools.echo({ text: "a" }); return 1;"#,
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    tokio::time::timeout(std::time::Duration::from_secs(60), run_once(&root))
        .await
        .expect("the submission settles");
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "1");
    assert!(!is_error);
    let tasks = all_tasks(&harness).await;
    let hold = tasks
        .iter()
        .find(|task| {
            task.input.get("kind") == Some(&json!("nested"))
                && task.input.get("call").and_then(|call| call.get("name")) == Some(&json!("hold"))
        })
        .expect("the hold task");
    assert_eq!(chat.holds.runs.load(Ordering::SeqCst), 1);
    match &hold.state {
        kiana_durable::types::TaskState::Terminal { outcome, .. } => {
            assert!(matches!(
                outcome,
                kiana_durable::types::TaskOutcome::Aborted { .. }
            ));
        }
        other => panic!("a terminal task, got {other:?}"),
    }
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delivers_a_result_only_after_its_journal_commit() {
    let _guard = test_gate();
    let storage = Arc::new(support::ControlledStorage::new());
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let first_seen = Arc::clone(&seen);
    let first_storage = Arc::clone(&storage);
    let second_seen = Arc::clone(&seen);
    let chat = restart_chat_with(vec![
        global("first", move |_args, _api, _context| {
            let seen = Arc::clone(&first_seen);
            let storage = Arc::clone(&first_storage);
            Box::pin(async move {
                lock(&seen).push("first".to_owned());
                // 下一次提交就是本结果的 journal 提交：它永远不到达。
                storage.fail_next_commit(std::io::Error::other("lost").into());
                Ok(Some(json!(1)))
            })
        }),
        global("second", move |_args, _api, _context| {
            let seen = Arc::clone(&second_seen);
            Box::pin(async move {
                lock(&seen).push("second".to_owned());
                Ok(Some(json!(2)))
            })
        }),
    ]);
    chat.holds.block.store(false, Ordering::SeqCst);
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(r#"await first(); await second();"#)),
        FauxResponseStep::Message(done()),
    ]);
    let storage_ref: Arc<dyn Storage> = storage.clone();
    let (harness, root) = open_chat(storage_ref, &chat.setup).await;
    let _submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    // TS 的 `eventually()` 只等约 200 个宏任务；Rust 侧第一次 `sandbox.start()` 还要在
    // worker 线程里编译 quickjs.wasm（调试构建约 10 秒，套件并发时更久），固定轮数会在
    // 冷启动完成前就耗尽。这里等真实时间，语义不变：等到第一次调用真的发生。
    chat_support::wait_for(
        || async { lock(&seen).iter().any(|item| item == "first") },
        std::time::Duration::from_secs(180),
    )
    .await
    .expect("the first call ran");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(*lock(&seen), vec!["first".to_owned()]);
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        harness.close(&*context()),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_to_resume_when_the_sandboxs_fingerprint_or_the_code_mode_version_changed() {
    let _guard = test_gate();
    for field in ["fingerprint", "version"] {
        let chat = restart_chat();
        let hooks = RestartHooks {
            between: Some(Box::new(move |harness, task| {
                Box::pin(async move {
                    edit_run(&harness, task, move |run| {
                        let start = run
                            .get_mut("start")
                            .and_then(JsonValue::as_object_mut)
                            .expect("the run's start");
                        if field == "fingerprint" {
                            start.insert("fingerprint".to_owned(), json!("another sandbox"));
                        } else {
                            start.insert("version".to_owned(), json!(0));
                        }
                    })
                    .await;
                })
            })),
            ..Default::default()
        };
        let (harness, root) = restart_hooked(
            &chat,
            r#"await tools.hold({}); await tools.echo({ text: "late" });"#,
            hooks,
        )
        .await;
        let (text, is_error) = result_text(&root).await;
        assert!(is_error, "{field}");
        assert!(text.contains("cannot resume"), "{field}: {text}");
        assert!(lock(&chat.echo).is_empty(), "{field}");
        harness.close(&*context()).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn times_a_script_out_across_a_restart_without_admitting_new_calls() {
    let _guard = test_gate();
    let chat = restart_chat();
    // 用可控时钟把「重启期间 deadline 过期」变成确定性事件：真实时间只走几百毫秒，
    // sandbox 的看门狗（真实计时器）不会在 `hold` 到达前触发；`between` 钩子把时钟
    // 拨过 deadline，恢复时必然判超时。依赖墙钟会让满负载下的 VM 启动吃掉 deadline。
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(1_700_000_000_000));
    let base = Arc::clone(&clock);
    chat.setup
        .set_now(Arc::new(move || base.load(Ordering::SeqCst)));
    let deadline_clock = Arc::clone(&clock);
    let hooks = RestartHooks {
        between: Some(Box::new(move |_harness, _task| {
            let clock = Arc::clone(&deadline_clock);
            Box::pin(async move {
                // timeout_ms = 60_000：拨快 120_000 确保恢复时 deadline 已过。
                clock.fetch_add(120_000, Ordering::SeqCst);
            })
        })),
        ..Default::default()
    };
    let (harness, root) = restart_hooked(
        &chat,
        r#"// @options: {"timeout_ms": 60000}
        await tools.hold({}); await tools.echo({ text: "late" });"#,
        hooks,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert!(is_error);
    assert!(text.contains("deadline passed"), "{text}");
    assert!(lock(&chat.echo).is_empty());
    assert_eq!(chat.starts.load(Ordering::SeqCst), 1);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_store_values_per_conversation_as_of_a_forks_entry() {
    let _guard = test_gate();
    let (setup, _) = codemode_chat();
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(r#"store("k", 1); return load("k")"#)),
        FauxResponseStep::Message(done()),
        FauxResponseStep::Message(script(r#"return [load("k") ?? null]"#)),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    run_once(&root).await;
    let entries = chat_support::all_entries(&root, &*context()).await;
    let first_user = entries
        .iter()
        .find(|entry| entry.kind == "kiana.user")
        .expect("the first input");
    run_once(&root).await;
    let texts: Vec<String> = entries_texts(&root).await;
    assert_eq!(texts, vec!["1".to_owned(), "[1]".to_owned()]);
    // 在第一次输入处的 fork 来自第一次脚本存任何东西之前。
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(r#"return [load("k") ?? null]"#)),
        FauxResponseStep::Message(done()),
    ]);
    let fork = root
        .fork(
            first_user.id,
            kiana_durable::harness::types::ConversationCreateOptions {
                ownership: kiana_durable::types::ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context(),
        )
        .await
        .expect("the fork");
    run_once(&fork).await;
    let fork_texts = entries_texts(&fork).await;
    assert_eq!(fork_texts.last().map(String::as_str), Some("[null]"));
    harness.close(&*context()).await;
}

/// 每个 r#"kiana.tool-result"# 去头之后的第一段文本。
async fn entries_texts(root: &Arc<dyn ConversationTrait>) -> Vec<String> {
    let entries = chat_support::all_entries(root, &*context()).await;
    entries
        .iter()
        .filter(|entry| entry.kind == "kiana.tool-result")
        .map(
            |entry| match entry.model.as_ref().and_then(|model| model.first()) {
                Some(kiana_ai::types::Message::ToolResult(result)) => strip_header(
                    &result
                        .content
                        .iter()
                        .map(|item| match item {
                            UserContentBlock::Text(text) => text.text.clone(),
                            UserContentBlock::Image(_) => String::new(),
                        })
                        .collect::<String>(),
                ),
                _ => String::new(),
            },
        )
        .collect()
}

// ---------------------------------------------------------------------------
// 审查修复的回归用例与 TS 用例补齐
// ---------------------------------------------------------------------------

use kiana_durable::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirReader, ExecutionEnv, ExecutionError,
    ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileListing, FileWatcher,
    ListFilesOptions, OpenBinaryReaderOptions, ReadTextLinesOptions, RemoveOptions, ShellCommand,
    ShellExecOptions, ShellExecResult, TextLineReader, WatchCallback, WatchTarget,
};
use kiana_durable::harness::types::{EnvBuilder, EnvTarget};

use chat_support::{OpenChatOptions, open_chat_with};

/// 最近一条 `kiana.tool-result` 的 details。
async fn tool_result_details(root: &Arc<dyn ConversationTrait>) -> JsonValue {
    let entries = chat_support::all_entries(root, &*context()).await;
    let entry = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("a tool result");
    let message = match entry
        .model
        .as_ref()
        .and_then(|model| model.first())
        .expect("the message")
    {
        kiana_ai::types::Message::ToolResult(result) => result,
        _ => panic!("a tool result message"),
    };
    message.details.clone().expect("details")
}

/// TS: `tools.echo(5)`——脚本给的工具参数不是对象时，嵌套任务的参数校验以
/// `invalid_arguments` 结束；脚本的 promise 以那条校验消息拒绝，调用本身不分叉。
///
/// 冻结的 `execute_tool(name, args: JsonObject, …)` 收不下非对象参数，所以这里构造该嵌套校验
/// 本会存下的结果；TS 还会创建嵌套任务、NestedCallDoc 与 live 槽位（见交付说明的偏差）。
#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_non_object_argument_through_the_nested_tools_validation() {
    let _guard = test_gate();
    let (setup, echo) = codemode_chat();
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            r#"try { await tools.echo(5); return "no error"; } catch (error) { return error.message; }"#,
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    run_once(&root).await;
    let (text, is_error) = result_text(&root).await;
    assert!(!is_error);
    assert_eq!(
        text,
        "Validation failed for tool \"echo\":\n  - root: must be object\n\nReceived arguments:\n5"
    );
    assert!(lock(&echo).is_empty());
    let details = tool_result_details(&root).await;
    assert_eq!(details["calls"][0]["name"], json!("echo"));
    assert_eq!(details["calls"][0]["status"], json!("error"));
    assert_eq!(
        details["calls"][0]["error"],
        json!(
            "Validation failed for tool \"echo\":\n  - root: must be object\n\nReceived arguments:\n5"
        )
    );
    harness.close(&*context()).await;
}

/// TS 的 `options?.limit ?? DEFAULT_SEARCH_LIMIT`：显式 `null` 与缺省一样回退到 8。
#[tokio::test(flavor = "multi_thread")]
async fn search_tools_treats_a_null_limit_as_the_default() {
    let _guard = test_gate();
    let setup = ChatSetup::new();
    for name in [
        "tool_aa", "tool_bb", "tool_cc", "tool_dd", "tool_ee", "tool_ff", "tool_gg", "tool_hh",
        "tool_ii", "tool_jj",
    ] {
        let _ = add_tool(&setup.registry, base_tool(name));
    }
    setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions::new(
            sandbox(),
        )),
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            r#"return (await searchTools("tool", { limit: null })).length"#,
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    run_once(&root).await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "8");
    assert!(!is_error);
    harness.close(&*context()).await;
}

/// 重放的多个结果一起就绪时，每个 journal 条目只投递一次，并且按 journal 顺序投递。
#[tokio::test(flavor = "multi_thread")]
async fn delivers_a_burst_of_replayed_results_once_each() {
    let _guard = test_gate();
    let chat = restart_chat();
    let (harness, root) = restart(
        &chat,
        r#"const values = await Promise.all(["a", "b", "c", "d", "e", "f"].map((text) => tools.echo({ text })));
        await tools.hold({});
        return values.join(",");"#,
    )
    .await;
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "echo a,echo b,echo c,echo d,echo e,echo f");
    assert!(!is_error);
    // 每个 echo 只跑过一次：重放的结果来自 journal，没有再执行（完成顺序不定）。
    let mut calls = lock(&chat.echo).clone();
    calls.sort();
    assert_eq!(calls, vec!["a", "b", "c", "d", "e", "f"]);
    harness.close(&*context()).await;
}

/// TS: "records a count the live run never recorded as soon as a replay acknowledges it"。
#[tokio::test(flavor = "multi_thread")]
async fn records_a_count_the_live_run_never_recorded_as_soon_as_a_replay_acknowledges_it() {
    let _guard = test_gate();
    let chat = restart_chat();
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            r#"await tools.echo({ text: "a" }); await tools.hold({});"#,
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    let id = submission.id();
    chat.holds.wait_reached().await;
    harness.close(&*context()).await;

    // 结果 1 的计数从没被记录；重启后 hold 再次阻塞。
    chat.holds.reset_reached();
    chat.storage.reopen();
    let (harness, _root) = open_chat(chat.storage.clone(), &chat.setup).await;
    let task = script_task(&harness).await;
    edit_journal(&harness, task, |journal| {
        let entries = journal_entries(journal);
        entries[0]
            .as_object_mut()
            .expect("an entry")
            .remove("issuedAfter");
    })
    .await;
    harness.resume().expect("resume");
    // 重放确认了结果 1 并转入实时；脚本停在 hold 上，此时计数已经记下。
    chat.holds.wait_reached().await;
    chat_support::eventually(|| async {
        let journal = harness
            .snapshot(&DocTarget::task(&CODEMODE_JOURNAL_DOC, task), &*context())
            .await
            .expect("the journal");
        journal
            .as_ref()
            .and_then(|journal| journal.get("entries"))
            .and_then(|entries| entries.get(0))
            .and_then(|entry| entry.get("issuedAfter"))
            .and_then(JsonValue::as_u64)
            == Some(2)
    })
    .await
    .expect("the replay recorded the count");
    harness.close(&*context()).await;

    chat.holds.block.store(false, Ordering::SeqCst);
    chat.storage.reopen();
    let (harness, _root) = open_chat(chat.storage.clone(), &chat.setup).await;
    harness.resume().expect("resume");
    let settled = harness
        .submission(id, &*context())
        .await
        .expect("submission")
        .expect("the submission")
        .wait(&*context())
        .await
        .expect("the submission settles");
    assert_eq!(settled.record().status, SubmissionStatus::Done);
    harness.close(&*context()).await;
}

/// TS: "stores no outcome when the host closes the sandbox, and replays the script after a restart"。
#[tokio::test(flavor = "multi_thread")]
async fn stores_no_outcome_when_the_host_closes_the_sandbox_and_replays_after_a_restart() {
    let _guard = test_gate();
    let chat = restart_chat();
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            r#"await tools.echo({ text: "a" }); await tools.hold({}); return "done";"#,
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    let id = submission.id();
    chat.holds.wait_reached().await;
    let task = script_task(&harness).await;
    // 宿主关掉沙箱先于关掉 Harness。
    chat.sandbox.close().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let run = harness
        .snapshot(&DocTarget::task(&CODEMODE_RUN_DOC, task), &*context())
        .await
        .expect("the run doc");
    assert!(run.as_ref().and_then(|run| run.get("result")).is_none());
    harness.close(&*context()).await;

    // 下一个进程有一个新沙箱。
    chat.holds.block.store(false, Ordering::SeqCst);
    chat.setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions::new(
            sandbox(),
        )),
    );
    chat.storage.reopen();
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    harness.resume().expect("resume");
    let settled = harness
        .submission(id, &*context())
        .await
        .expect("submission")
        .expect("the submission")
        .wait(&*context())
        .await
        .expect("the submission settles");
    assert_eq!(settled.record().status, SubmissionStatus::Done);
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "done");
    assert!(!is_error);
    assert_eq!(*lock(&chat.echo), vec!["a".to_owned()]);
    harness.close(&*context()).await;
}

/// 一个在沙箱启动时扣住提交的包装：TS 用例在 `vm.onStart` 里 `storage.holdCommits()`。
struct HoldingSandbox {
    inner: Arc<dyn CodemodeSandbox>,
    storage: Arc<support::ControlledStorage>,
    gate: Arc<Mutex<Option<support::Gate>>>,
    started: Arc<tokio::sync::Semaphore>,
}

impl CodemodeSandbox for HoldingSandbox {
    fn start(&self, code: &str, options: CodemodeRunOptions) -> Result<CodemodeRun, StartError> {
        let gate = self.storage.hold_commits();
        *lock(&self.gate) = Some(gate);
        self.started.add_permits(1);
        self.inner.start(code, options)
    }
}

/// TS: "admits no call once the script has ended, even one it made before ending"。
#[tokio::test(flavor = "multi_thread")]
async fn admits_no_call_once_the_script_has_ended_even_one_it_made_before_ending() {
    let _guard = test_gate();
    let chat = restart_chat();
    chat.holds.block.store(false, Ordering::SeqCst);
    let storage = Arc::new(support::ControlledStorage::new());
    let gate = Arc::new(Mutex::new(None));
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    chat.setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions {
            sandbox: Arc::new(HoldingSandbox {
                inner: sandbox(),
                storage: Arc::clone(&storage),
                gate: Arc::clone(&gate),
                started: Arc::clone(&started),
            }),
            namespaces: None,
            inline_budget: None,
            models: None,
            globals: None,
        }),
    );
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(r#"tools.echo({ text: "late" }); return 1;"#)),
        FauxResponseStep::Message(done()),
    ]);
    let storage_ref: Arc<dyn Storage> = storage.clone();
    let (harness, root) = open_chat(storage_ref, &chat.setup).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    // 指纹提交（以及之后接纳调用）被扣到脚本结束之后。
    started
        .acquire()
        .await
        .expect("the sandbox starts")
        .forget();
    let held = lock(&gate).clone().expect("a held commit");
    held.entered().await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    held.release();
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "1");
    assert!(!is_error);
    assert!(lock(&chat.echo).is_empty());
    assert_eq!(nested_count(&harness).await, 0);
    harness.close(&*context()).await;
}

/// `afterTool` 在结果提交前停住（TS 用例的 `stopped` 钩子），直到调用被中止。
struct BlockingAfterTool {
    block: Arc<AtomicBool>,
    reached: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl kiana_durable::harness::types::ToolHooks for BlockingAfterTool {
    async fn before_tool(
        &self,
        call: kiana_durable::harness::types::ToolHookCall,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::BeforeToolOutput>, BoxError> {
        Ok(Some(kiana_durable::harness::types::BeforeToolOutput {
            arguments: Some(call.call.arguments),
            block: None,
        }))
    }

    async fn after_tool(
        &self,
        call: kiana_durable::harness::types::ToolHookCall,
        _result: ToolExecutionResult,
        _api: Arc<dyn kiana_durable::harness::types::HookApi>,
        context: &dyn Context,
    ) -> Result<Option<ToolExecutionResult>, BoxError> {
        if call.call.name == "codemode" && self.block.load(Ordering::SeqCst) {
            self.reached.add_permits(1);
            if let Some(signal) = context.abort_signal() {
                signal.token().cancelled().await;
            }
        }
        Ok(None)
    }
}

/// TS: "returns a decided outcome after a restart without running the script again"。
#[tokio::test(flavor = "multi_thread")]
async fn returns_a_decided_outcome_after_a_restart_without_running_the_script_again() {
    let _guard = test_gate();
    let chat = restart_chat();
    chat.holds.block.store(false, Ordering::SeqCst);
    let blocker = Arc::new(BlockingAfterTool {
        block: Arc::new(AtomicBool::new(true)),
        reached: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    harness_support::add_hooks(
        &chat.setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::tool::tool_task(),
            Arc::clone(&blocker) as Arc<dyn kiana_durable::harness::types::ToolHooks>,
        )],
        "hooks",
    );
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(r#"return "first""#)),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    let id = submission.id();
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        blocker.reached.acquire(),
    )
    .await
    .expect("the hook runs")
    .expect("the hook permit")
    .forget();
    // execute() 返回之后（结局已存下），结果提交之前。
    let task = script_task(&harness).await;
    let run = harness
        .snapshot(&DocTarget::task(&CODEMODE_RUN_DOC, task), &*context())
        .await
        .expect("the run doc");
    assert!(run.as_ref().and_then(|run| run.get("result")).is_some());
    harness.close(&*context()).await;

    blocker.block.store(false, Ordering::SeqCst);
    chat.storage.reopen();
    let (harness, root) = open_chat(chat.storage.clone(), &chat.setup).await;
    harness.resume().expect("resume");
    let settled = harness
        .submission(id, &*context())
        .await
        .expect("submission")
        .expect("the submission")
        .wait(&*context())
        .await
        .expect("the submission settles");
    assert_eq!(settled.record().status, SubmissionStatus::Done);
    let (text, is_error) = result_text(&root).await;
    assert_eq!(text, "first");
    assert!(!is_error);
    assert_eq!(chat.starts.load(Ordering::SeqCst), 1);
    harness.close(&*context()).await;
}

/// code mode 结果落盘用的内存文件系统（TS 的 `NodeExecutionEnv` 替身）。
#[derive(Default)]
struct MemFs {
    dirs: Mutex<Vec<String>>,
    files: Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
    counter: AtomicUsize,
}

impl MemFs {
    fn new_dir(&self) -> String {
        let number = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let path = format!("/kiana-codemode-test-{number}");
        lock(&self.dirs).push(path.clone());
        path
    }

    fn write(&self, path: &str, content: FileContent) {
        let bytes = match content {
            FileContent::Text(text) => text.into_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        lock(&self.files).insert(path.to_owned(), bytes);
    }

    /// `readdir(dir)`：目录下的文件名。
    fn files_in(&self, dir: &str) -> Vec<String> {
        let prefix = format!("{dir}/");
        lock(&self.files)
            .keys()
            .filter(|path| path.starts_with(&prefix))
            .map(|path| path[prefix.len()..].to_owned())
            .collect()
    }
}

/// 写文件之后的钩子：拿到路径与调用的中止信号（模拟在提交前崩溃）。
type WriteHook = Arc<
    dyn Fn(
            String,
            Option<kiana_chord::context::AbortSignal>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

struct MemEnv {
    fs: Arc<MemFs>,
    after_write: Option<WriteHook>,
    /// `Some` 时，`createTempDir` 的前几次调用失败（TS 的 `createTempDir` 拒绝）。
    temp_dir_fault: Option<Arc<TempDirFault>>,
}

/// 注入的 `createTempDir` 失败：失败次数与调用次数。
#[derive(Default)]
struct TempDirFault {
    /// 还要失败几次。
    failures: AtomicUsize,
    /// 被调用了多少次。
    calls: AtomicUsize,
}

fn unsupported() -> FileError {
    FileError::new(FileErrorCode::NotSupported, "not supported")
}

#[async_trait::async_trait]
impl kiana_durable::env::FileSystem for MemEnv {
    fn id(&self) -> &str {
        "test-mem"
    }

    fn cwd(&self) -> &str {
        "/"
    }

    fn set_cwd(&self, _cwd: String) {}

    async fn absolute_path(&self, path: &str, _context: &dyn Context) -> Result<String, FileError> {
        Ok(path.to_owned())
    }

    async fn join_path(
        &self,
        parts: &[String],
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Ok(parts.join("/"))
    }

    async fn read_text_file(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        lock(&self.fs.files)
            .get(path)
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .ok_or_else(|| FileError::new(FileErrorCode::NotFound, format!("no file {path}")))
    }

    async fn open_text_line_reader(
        &self,
        _path: &str,
        _context: &dyn Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError> {
        Err(unsupported())
    }

    async fn read_text_lines(
        &self,
        _path: &str,
        _options: Option<&ReadTextLinesOptions>,
        _context: &dyn Context,
    ) -> Result<Vec<String>, FileError> {
        Err(unsupported())
    }

    async fn read_binary_file(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        lock(&self.fs.files)
            .get(path)
            .cloned()
            .ok_or_else(|| FileError::new(FileErrorCode::NotFound, format!("no file {path}")))
    }

    async fn open_binary_reader(
        &self,
        _path: &str,
        _options: Option<&OpenBinaryReaderOptions>,
        _context: &dyn Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError> {
        Err(unsupported())
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        self.fs.write(path, content);
        if let Some(hook) = &self.after_write {
            (hook)(path.to_owned(), context.abort_signal()).await;
        }
        Ok(())
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        let bytes = match content {
            FileContent::Text(text) => text.into_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        lock(&self.fs.files)
            .entry(path.to_owned())
            .or_default()
            .extend(bytes);
        Ok(())
    }

    async fn truncate_file(
        &self,
        _path: &str,
        _size: u64,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        Err(unsupported())
    }

    async fn flush_file(&self, _path: &str, _context: &dyn Context) -> Result<(), FileError> {
        Ok(())
    }

    async fn rename_file(
        &self,
        _source_path: &str,
        _destination_path: &str,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        Err(unsupported())
    }

    async fn file_info(&self, _path: &str, _context: &dyn Context) -> Result<FileInfo, FileError> {
        Err(unsupported())
    }

    async fn list_dir(
        &self,
        _path: &str,
        _context: &dyn Context,
    ) -> Result<Vec<FileInfo>, FileError> {
        Err(unsupported())
    }

    async fn list_files(
        &self,
        _path: &str,
        _options: Option<&ListFilesOptions>,
        _context: &dyn Context,
    ) -> Result<FileListing, FileError> {
        Err(unsupported())
    }

    async fn open_dir_reader(
        &self,
        _path: &str,
        _context: &dyn Context,
    ) -> Result<Arc<dyn DirReader>, FileError> {
        Err(unsupported())
    }

    async fn watch(
        &self,
        _targets: &[WatchTarget],
        _on_change: WatchCallback,
        _context: &dyn Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError> {
        Err(unsupported())
    }

    async fn canonical_path(
        &self,
        path: &str,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Ok(path.to_owned())
    }

    async fn exists(&self, path: &str, _context: &dyn Context) -> Result<bool, FileError> {
        Ok(lock(&self.fs.files).contains_key(path))
    }

    async fn create_dir(
        &self,
        _path: &str,
        _options: Option<&CreateDirOptions>,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        Ok(())
    }

    async fn remove(
        &self,
        path: &str,
        _options: Option<&RemoveOptions>,
        _context: &dyn Context,
    ) -> Result<(), FileError> {
        lock(&self.fs.files).remove(path);
        Ok(())
    }

    async fn create_temp_dir(
        &self,
        _prefix: Option<&str>,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        if let Some(fault) = &self.temp_dir_fault {
            fault.calls.fetch_add(1, Ordering::SeqCst);
            if fault.failures.load(Ordering::SeqCst) > 0 {
                fault.failures.fetch_sub(1, Ordering::SeqCst);
                return Err(FileError::new(
                    FileErrorCode::Unknown,
                    "could not create a temporary directory",
                ));
            }
        }
        Ok(self.fs.new_dir())
    }

    async fn create_temp_file(
        &self,
        _options: Option<&CreateTempFileOptions>,
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        Err(unsupported())
    }

    async fn cleanup(&self, _context: &dyn Context) {}
}

#[async_trait::async_trait]
impl kiana_durable::env::Shell for MemEnv {
    async fn exec(
        &self,
        _command: ShellCommand,
        _options: Option<&ShellExecOptions>,
        _context: &dyn Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            "no shell",
        ))
    }

    async fn cleanup(&self, _context: &dyn Context) {}
}

fn mem_env_builder(fs: Arc<MemFs>, after_write: Option<WriteHook>) -> EnvBuilder {
    mem_env_builder_with_fault(fs, after_write, None)
}

fn mem_env_builder_with_fault(
    fs: Arc<MemFs>,
    after_write: Option<WriteHook>,
    temp_dir_fault: Option<Arc<TempDirFault>>,
) -> EnvBuilder {
    Arc::new(move |_target: EnvTarget, _context| {
        let fs = Arc::clone(&fs);
        let after_write = after_write.clone();
        let temp_dir_fault = temp_dir_fault.clone();
        Box::pin(async move {
            let env: Arc<dyn ExecutionEnv> = Arc::new(MemEnv {
                fs,
                after_write,
                temp_dir_fault,
            });
            Ok(Some(env))
        })
    })
}

/// TS: "writes the result's files to the same paths when it reruns after a crash"。
#[tokio::test(flavor = "multi_thread")]
async fn writes_the_results_files_to_the_same_paths_when_it_reruns_after_a_crash() {
    let _guard = test_gate();
    let chat = restart_chat();
    chat.holds.block.store(false, Ordering::SeqCst);
    let fs = Arc::new(MemFs::default());
    let (written_tx, mut written_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let after_write: WriteHook = Arc::new(
        move |path: String, signal: Option<kiana_chord::context::AbortSignal>| {
            let written_tx = written_tx.clone();
            Box::pin(async move {
                let _ = written_tx.send(path);
                // 进程在完整输出写完之后、结果提交之前死掉。
                if let Some(signal) = signal {
                    signal.token().cancelled().await;
                }
            })
        },
    );
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            "// @options: {\"max_output_tokens\": 20}\nreturn \"x\".repeat(1000);",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let first_builder = mem_env_builder(Arc::clone(&fs), Some(after_write));
    let (harness, root) = open_chat_with(
        chat.storage.clone(),
        &chat.setup,
        OpenChatOptions {
            env: Some(first_builder),
        },
    )
    .await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    let id = submission.id();
    let first = tokio::time::timeout(std::time::Duration::from_secs(60), written_rx.recv())
        .await
        .expect("the full output is written")
        .expect("the written path");
    harness.close(&*context()).await;

    chat.storage.reopen();
    let (harness, root) = open_chat_with(
        chat.storage.clone(),
        &chat.setup,
        OpenChatOptions {
            env: Some(mem_env_builder(Arc::clone(&fs), None)),
        },
    )
    .await;
    harness.resume().expect("resume");
    let settled = harness
        .submission(id, &*context())
        .await
        .expect("submission")
        .expect("the submission")
        .wait(&*context())
        .await
        .expect("the submission settles");
    assert_eq!(settled.record().status, SubmissionStatus::Done);
    let details = tool_result_details(&root).await;
    assert_eq!(details["fullOutputPath"], json!(first));
    let directory = first.rsplit_once('/').expect("a directory").0;
    assert_eq!(fs.files_in(directory), vec!["output.txt".to_owned()]);
    assert_eq!(
        lock(&fs.files).get(&first).map(Vec::len),
        Some(1000),
        "the full output is still there"
    );
    harness.close(&*context()).await;
}

/// TS 把 `outputDir()` 的 promise 缓存在闭包里：它拒绝后，同一结果的每个文件都报同一条错误，
/// 环境也不会再被问一次。
#[tokio::test(flavor = "multi_thread")]
async fn reports_the_first_output_directory_error_for_every_file_without_asking_again() {
    let _guard = test_gate();
    let chat = restart_chat();
    chat.holds.block.store(false, Ordering::SeqCst);
    let fs = Arc::new(MemFs::default());
    let fault = Arc::new(TempDirFault {
        failures: AtomicUsize::new(1),
        calls: AtomicUsize::new(0),
    });
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let png2 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==";
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(&format!(
            r#"image("data:image/png;base64,{png}"); image("data:image/png;base64,{png2}"); return 1;"#
        ))),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat_with(
        chat.storage.clone(),
        &chat.setup,
        OpenChatOptions {
            env: Some(mem_env_builder_with_fault(
                Arc::clone(&fs),
                None,
                Some(Arc::clone(&fault)),
            )),
        },
    )
    .await;
    run_once(&root).await;
    let (text, is_error) = result_text(&root).await;
    assert!(!is_error);
    let failed =
        "[Image (image/png, 70 bytes) could not be saved: could not create a temporary directory]";
    assert_eq!(text.matches(failed).count(), 2, "{text}");
    assert_eq!(fault.calls.load(Ordering::SeqCst), 1);
    harness.close(&*context()).await;
}

/// 存进文档的数字是 JS 的写法：整数值不带 serde_json 的 `.0` 后缀。
#[tokio::test(flavor = "multi_thread")]
async fn stores_document_numbers_the_way_the_ts_writes_them() {
    let _guard = test_gate();
    let (setup, _echo) = codemode_chat();
    let blocker = Arc::new(BlockingAfterTool {
        block: Arc::new(AtomicBool::new(true)),
        reached: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    harness_support::add_hooks(
        &setup.registry,
        vec![kiana_durable::harness::define::hook(
            &kiana_durable::harness::tool::tool_task(),
            Arc::clone(&blocker) as Arc<dyn kiana_durable::harness::types::ToolHooks>,
        )],
        "hooks",
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(r#"await tools.echo({ text: "a" }); return 1;"#)),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let _submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        blocker.reached.acquire(),
    )
    .await
    .expect("the hook runs")
    .expect("the hook permit")
    .forget();
    // 结局已存下、结果提交之前：跑完的脚本任务连同它的文档都还在。
    let task = script_task(&harness).await;
    let run = harness
        .snapshot(&DocTarget::task(&CODEMODE_RUN_DOC, task), &*context())
        .await
        .expect("the run doc")
        .expect("the run doc exists");
    let start = run.get("start").expect("start data");
    assert!(start["startedAt"].as_i64().is_some(), "{start:?}");
    assert!(start["seed"].as_i64().is_some(), "{start:?}");
    assert!(start["maxOutputTokens"].as_i64().is_some(), "{start:?}");
    let journal = harness
        .snapshot(&DocTarget::task(&CODEMODE_JOURNAL_DOC, task), &*context())
        .await
        .expect("the journal")
        .expect("the journal exists");
    let entries = journal
        .get("entries")
        .and_then(JsonValue::as_array)
        .expect("the entries");
    assert!(!entries.is_empty());
    for entry in entries {
        assert!(entry["at"].as_i64().is_some(), "{entry:?}");
    }
    // details 里的 `durationMs` 也是整数（TS 的 `Math.round`）。
    let details = run
        .get("result")
        .and_then(|result| result.get("details"))
        .expect("the result details");
    assert!(
        details["calls"][0]["durationMs"].as_i64().is_some(),
        "{details:?}"
    );
    harness.close(&*context()).await;
}
