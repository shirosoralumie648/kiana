//! `test/codemode-bench.ts` 的移植：durable code mode 的开销。
//!
//! 一次模型发起的调用让一个平凡的 `echo` 工具跑 N 次，顺序或并发，两种方式：code mode
//! 脚本（`tools.echo()`，入 journal、可重放）与直接调用 `api.executeTool()` 的普通工具
//! （嵌套调用、无 journal）。报告墙钟时间与每次调用的提交数。
//!
//! 这是基准而不是回归测试：用 `#[ignore]` 标注，按需运行
//! `cargo test -p kiana-durable --test codemode_bench -- --ignored --nocapture`。
//! 尺寸取自 `CODEMODE_BENCH_SIZES`（默认 `100,1000`）。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::providers::faux::{FauxResponseStep, FauxToolCallOptions, faux_tool_call_with};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, JsonObject, StopReason, TextContent,
    TextKind, UserContentBlock,
};
use kiana_chord::context::Context;
use kiana_durable::Session as _;
use kiana_durable::codemode::codemode::{CodemodeToolOptions, create_codemode_tool};
use kiana_durable::harness::types::Registry as _;
use kiana_durable::harness::types::{
    NestedToolOptions, ToolExecuteFn, ToolExecutionResult, ToolRegistration,
};
use kiana_durable::types::{BoxError, JsonValue, Storage};
use serde_json::json;

use chat_support::{ChatSetup, context, open_chat};
use harness_support::add_tool;

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

fn text_output(text: &str) -> Vec<UserContentBlock> {
    vec![UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_owned(),
        text_signature: None,
    })]
}

fn done() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "ok".to_owned(),
            text_signature: None,
        })],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        timestamp: 3,
        ..Default::default()
    }
}

fn call(name: &str, arguments: JsonObject) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(faux_tool_call_with(
            name,
            arguments,
            FauxToolCallOptions {
                id: Some("c1".to_owned()),
            },
        ))],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    }
}

fn echo_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |arguments, _api, _context| {
        let text = arguments
            .get("i")
            .map(|value| match value {
                JsonValue::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        Box::pin(async move {
            Ok(ToolExecutionResult {
                output: Some(text_output(&text)),
                ..Default::default()
            })
        })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = harness_support::tool("echo");
    registration.execute = Some(execute);
    registration.tool.parameters = json!({
        "type": "object",
        "required": ["i"],
        "properties": { "i": { "type": "number" } }
    });
    registration
}

fn plain_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |arguments, api, context| {
        let count = arguments.get("n").and_then(JsonValue::as_i64).unwrap_or(0);
        let sequential = arguments
            .get("shape")
            .and_then(JsonValue::as_str)
            .unwrap_or("sequential")
            == "sequential";
        Box::pin(async move {
            if sequential {
                for index in 0..count {
                    let mut args = JsonObject::new();
                    args.insert("i".to_owned(), json!(index));
                    Arc::clone(&api)
                        .execute_tool("echo", args, &*context, NestedToolOptions::default())
                        .await?;
                }
            } else {
                let mut pending = Vec::new();
                for index in 0..count {
                    let mut args = JsonObject::new();
                    args.insert("i".to_owned(), json!(index));
                    let api = Arc::clone(&api);
                    let context = Arc::clone(&context);
                    pending.push(tokio::spawn(async move {
                        api.execute_tool("echo", args, &*context, NestedToolOptions::default())
                            .await
                    }));
                }
                for task in pending {
                    task.await.expect("the nested call joins")?;
                }
            }
            Ok(ToolExecutionResult {
                output: Some(text_output("done")),
                ..Default::default()
            })
        })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let mut registration = harness_support::tool("plain");
    registration.execute = Some(execute);
    registration.replay = Some(kiana_durable::harness::types::ReplayPolicy::Safe);
    registration.tool.parameters = json!({
        "type": "object",
        "required": ["n", "shape"],
        "properties": { "n": { "type": "number" }, "shape": { "type": "string" } }
    });
    registration
}

fn sandbox() -> Arc<dyn kiana_durable::codemode::codemode::CodemodeSandbox> {
    let sandbox =
        kiana_codemode::CodemodeSandbox::new(kiana_codemode::types::CodemodeSandboxOptions {
            base: kiana_codemode::types::CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some(256.0 * 1024.0 * 1024.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("the sandbox");
    Arc::new(sandbox) as Arc<dyn kiana_durable::codemode::codemode::CodemodeSandbox>
}

async fn run(mode: &str, shape: &str, n: u64) -> (f64, usize) {
    let setup = ChatSetup::new();
    let _ = add_tool(&setup.registry, echo_tool());
    let _ = add_tool(&setup.registry, plain_tool());
    let codemode = create_codemode_tool(CodemodeToolOptions::new(sandbox()));
    setup.registry.install(Arc::new(
        harness_support::TestExtension::new("bench").with_tools(vec![codemode]),
    ) as Arc<dyn kiana_durable::harness::types::Extension>);
    let code = if shape == "sequential" {
        format!("for (let i = 0; i < {n}; i++) await tools.echo({{ i }}); return \"done\";")
    } else {
        format!(
            "await Promise.all(Array.from({{ length: {n} }}, (_, i) => tools.echo({{ i }}))); return \"done\";"
        )
    };
    let message = if mode == "codemode" {
        let mut arguments = JsonObject::new();
        arguments.insert("code".to_owned(), JsonValue::String(code));
        call("codemode", arguments)
    } else {
        let mut arguments = JsonObject::new();
        arguments.insert("n".to_owned(), json!(n));
        arguments.insert("shape".to_owned(), JsonValue::String(shape.to_owned()));
        call("plain", arguments)
    };
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(message),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let commits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&commits);
    let _subscription = harness.subscribe_commits(Arc::new(
        move |_publication: &kiana_durable::types::CommitPublication, _context: &dyn Context| {
            counter.fetch_add(1, Ordering::SeqCst);
        },
    ));
    let started = std::time::Instant::now();
    let submission = root
        .submit(harness_support::text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    harness.close(&*context()).await;
    (ms, commits.load(Ordering::SeqCst))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: prints code mode overhead for sequential and parallel echo calls"]
async fn reports_code_mode_overhead() {
    let sizes: Vec<u64> = std::env::var("CODEMODE_BENCH_SIZES")
        .unwrap_or_else(|_| "100,1000".to_owned())
        .split(',')
        .filter_map(|size| size.trim().parse().ok())
        .collect();
    for shape in ["sequential", "parallel"] {
        for n in &sizes {
            let (plain_ms, plain_commits) = run("plain", shape, *n).await;
            let (codemode_ms, codemode_commits) = run("codemode", shape, *n).await;
            let per = |value: f64| format!("{:.2}", value / *n as f64);
            println!(
                "{shape} n={n}: plain {:.0} ms ({} ms, {} commits/call), codemode {:.0} ms ({} ms, {} commits/call)",
                plain_ms,
                per(plain_ms),
                per(plain_commits as f64),
                codemode_ms,
                per(codemode_ms),
                per(codemode_commits as f64),
            );
        }
    }
}
