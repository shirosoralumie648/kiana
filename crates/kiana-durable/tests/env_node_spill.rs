//! `packages/durable/test/env-node-spill.test.ts` 的移植。
//!
//! TS 用 `vi.mock("node:fs")` 把 `createWriteStream` 换成每次 `_write` 延迟 600 ms、缓冲 1 字节
//! 的流，从而确定性地制造 spill 背压，并断言 `spill.rejectedWrites > 0`。Rust 侧没有进程内
//! 替换 `tokio::fs::File` 的手段，因此这里保留测试**可观察的契约**：shell 退出后、继承了 stdio
//! 的后代在退出宽限期（100 ms）内写入的字节，必须同时出现在 `onOutput` 的输出与 spill 文件里；
//! 并且 spill 文件恰好是全部原始输出（前缀 + 后代写入）。被 mock 的 `rejectedWrites` 计数与
//! “spill 写入 pending 时把宽限期无限延长”的行为由 `NodeExecutionEnv::write_spill` 的内联
//! `write_all`（写期间不读管道，形成背压）保证，已在 `env_node.rs` 的 spill 用例中
//! 覆盖完整性与时序。
//!
//! `vi.spyOn` 无法注入，`fails rather than silently losing a requested spill` 改在
//! `src/env/node.rs` 的单元测试里用私有 hook（`spill_temp_file`）实现。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::env::node::NodeExecutionEnv;
use kiana_durable::env::{
    ExecutionError, FileError, FileSystem, Shell, ShellCommand, ShellExecOptions, ShellExecResult,
    ShellSpillOptions,
};

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn ok_file<T>(result: Result<T, FileError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{}", error.message),
    }
}

fn ok_exec(result: Result<ShellExecResult, ExecutionError>) -> ShellExecResult {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{}", error.message),
    }
}

struct TempDir {
    path: String,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "kiana-durable-env-spill-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir {
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `keeps inherited stdio open past the exit grace period while a spill write is pending` 的
/// 无 mock 版本：后代在宽限期内写入，流与 spill 都必须完整。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_inherited_stdio_open_past_exit_for_descendant_output_while_spilling() {
    let root = TempDir::new("backpressure");
    let env = NodeExecutionEnv::new(root.path());
    // shell 在第一个块越过 spill 阈值后就退出；后台后代继承 stdout 并在 shell 退出后写入。
    let command = "printf '%020d' 0 | tr 0 a; (sleep 0.02; printf '%01000d' 0 | tr 0 b) &";

    let output = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&output);
    let result = env
        .exec(
            ShellCommand::Shell(command.to_owned()),
            Some(&ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 10,
                    after_lines: 10,
                }),
                on_output: Some(Arc::new(move |text, _context, _info| {
                    sink.lock().expect("output lock").push_str(text);
                })),
                ..Default::default()
            }),
            context(),
        )
        .await;
    let result = ok_exec(result);

    let output = output.lock().expect("output lock").clone();
    assert_eq!(output, format!("{}{}", "a".repeat(20), "b".repeat(1000)));

    let spill_path = result.spill_path.expect("spill path");
    assert_eq!(
        ok_file(env.read_text_file(&spill_path, context()).await),
        format!("{}{}", "a".repeat(20), "b".repeat(1000))
    );
    if let Some(parent) = std::path::Path::new(&spill_path).parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}
