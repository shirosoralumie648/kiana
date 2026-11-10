//! `packages/codemode/test/fixtures/worker-process-exit.ts` 的移植：进程生命周期夹具。
//!
//! 用法：`worker_process_exit abort|timeout|close`（其它值打印
//! `Unknown stop mode: ${how}` 并以非零码退出，与 TS 的 `throw` 对应）。
//!
//! 输出两行 JSON，与 TS 夹具相同：
//! 1. `{"type":"result","kind":…,"elapsedMs":…,"workerExited":…}`——执行结果；
//! 2. `{"type":"exit","elapsedMs":…}`——进程退出前的时刻（TS 用 `process.on("exit")`；Rust 在
//!    `main` 返回前打印，语义等价：它证明进程没有等待 worker 线程）。
//!
//! # 与 TS 夹具的差异（测试夹具本身，不是产品行为）
//!
//! TS 夹具跑的是**真的** QuickJS VM：脚本先 `await tools.ready()`，随后在
//! `'a'.repeat(300000).indexOf('a'.repeat(10000) + 'b')` 里自旋（Bun 上不可中断），因此
//! `terminate()` 可能挂起。这里保留**真实**的 worker 线程、真实的消息通道、真实的
//! `terminate`/`unref`/detach 与 `onWorkerStarted(exited)`，只把 VM 换成一条永不返回的会话
//! （[`spawn_worker_thread_with`] 注入，不碰进程级注册表）：
//!
//! - 纯 Rust 的 wasmtime 在 debug 构建里编译 637 KB 的 quickjs.wasm 约需 15 s，而 TS 的断言是
//!   `result.elapsedMs < 1000`（V8 的 `WebAssembly.compile` 只需几十毫秒）。用真 VM 会让这条
//!   断言测到编译时间而不是取消延迟，与 #10748 想验证的东西无关；
//! - worker 后端的取消路径（中断标志 → 请求终止 → unref → 不等待退出）与 VM 内部无关。
//!
//! 因此本夹具在**所有 feature 组合**下都断言 `kind` 是 `timeout`/`aborted`（TS 从不让
//! `sandbox` 出现在这里）。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use kiana_codemode::runtime::host::{
    CodemodeSandbox, WorkerHandle, WorkerObserver, WorkerSpawner, WorkerWasm,
};
use kiana_codemode::runtime::protocol::{CallTarget, VmStartData, WorkerToHostMessage};
use kiana_codemode::runtime::worker::{
    StartVmError, VmSession, WorkerStart, spawn_worker_thread_with,
};
use kiana_codemode::{
    CodemodeExecuteOptions, CodemodeResult, CodemodeSandboxBaseOptions, CodemodeSandboxOptions,
    CodemodeTool, CodemodeToolContext,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const SCRIPT: &str =
    "await tools.ready(); return 'a'.repeat(300000).indexOf('a'.repeat(10000) + 'b')";

/// 取代 `runtime/vm.rs` 的 `startVm`：VM 一开口就调用 `tools.ready()`，然后卡住直到宿主终止
/// worker（TS 的脚本卡在 `'a'.repeat(300000).indexOf(...)` 里）。
fn spinning_start_vm(
    start: WorkerStart,
) -> kiana_codemode::types::BoxFuture<'static, Result<Arc<dyn VmSession>, StartVmError>> {
    Box::pin(async move {
        // 与真实 prelude 一样，先把工具调用交给宿主；之后脚本不再前进（消息队列里排着的回答
        // 永远没人消费，宿主最终 terminate 整个线程）。
        (start.post)(WorkerToHostMessage::Call {
            id: 1,
            target: CallTarget::Tool,
            name: "ready".to_string(),
            args: Some("null".to_string()),
        });
        std::future::pending::<()>().await;
        unreachable!("the fixture's VM never returns")
    })
}

/// 不起 wasm 编译的 spawner：仍是真实线程（`spawn_worker_thread_with`），只是把线程入口的
/// `startVm` 换成 [`spinning_start_vm`]，且加载阶段不编译 `quickjs.wasm`。
///
/// `WorkerWasm` 是类型擦除的句柄，假 `startVm` 不解引用它，因此加载阶段返回一个占位。
struct SpinningSpawner;

impl WorkerSpawner for SpinningSpawner {
    fn load_wasm(&self) -> kiana_codemode::types::BoxFuture<'static, Result<WorkerWasm, String>> {
        Box::pin(async { Ok(Arc::new(()) as WorkerWasm) })
    }

    fn spawn(
        &self,
        _worker_url: Option<&str>,
        start: VmStartData,
        wasm: WorkerWasm,
        interrupt: Arc<AtomicI32>,
        observer: Arc<dyn WorkerObserver>,
    ) -> Result<Arc<dyn WorkerHandle>, String> {
        // 夹具不设 `workerUrl`（TS 夹具也没有），因此不需要入口校验。
        spawn_worker_thread_with(
            Some(Arc::new(|start: WorkerStart| spinning_start_vm(start))),
            start,
            wasm,
            interrupt,
            observer,
        )
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let how = std::env::args().nth(1).unwrap_or_default();
    if how != "abort" && how != "timeout" && how != "close" {
        eprintln!("Unknown stop mode: {how}");
        std::process::exit(1);
    }
    let started = Instant::now();

    let controller = CancellationToken::new();
    let worker_exited = Arc::new(AtomicBool::new(false));
    let exited_flag = Arc::clone(&worker_exited);
    // `sandbox.close()` 需要 `Arc<CodemodeSandbox>`，而工具闭包在 sandbox 内部构造：用弱引用
    // 在上一次执行之后回填（工具只在执行期间被调用，那时已经填好）。
    let sandbox_slot: Arc<Mutex<Option<Weak<CodemodeSandbox>>>> = Arc::new(Mutex::new(None));
    let tools = {
        let controller = controller.clone();
        let how = how.clone();
        let sandbox_slot = Arc::clone(&sandbox_slot);
        vec![CodemodeTool::new(
            "ready",
            Arc::new(
                move |_args: serde_json::Value, _context: CodemodeToolContext| {
                    if how != "timeout"
                        && let Ok(runtime) = tokio::runtime::Handle::try_current()
                    {
                        let controller = controller.clone();
                        let how = how.clone();
                        let sandbox_slot = Arc::clone(&sandbox_slot);
                        runtime.spawn(async move {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            if how == "abort" {
                                controller.cancel();
                            } else {
                                let sandbox = sandbox_slot
                                    .lock()
                                    .expect("sandbox slot")
                                    .as_ref()
                                    .and_then(Weak::upgrade);
                                if let Some(sandbox) = sandbox {
                                    sandbox.close().await;
                                }
                            }
                        });
                    }
                    Box::pin(async { Ok(serde_json::Value::Null) })
                },
            ),
        )]
    };
    let spawner = Arc::new(SpinningSpawner) as Arc<dyn WorkerSpawner>;
    let sandbox = Arc::new(
        CodemodeSandbox::with_spawner(
            CodemodeSandboxOptions {
                base: CodemodeSandboxBaseOptions {
                    memory_limit_bytes: Some((256 * 1024 * 1024) as f64),
                    tools: Some(tools),
                    ..Default::default()
                },
                on_worker_started: Some(Arc::new(move |exited| {
                    let exited_flag = Arc::clone(&exited_flag);
                    tokio::spawn(async move {
                        exited.await;
                        exited_flag.store(true, Ordering::SeqCst);
                    });
                })),
                ..Default::default()
            },
            spawner,
        )
        .expect("sandbox"),
    );
    *sandbox_slot.lock().expect("sandbox slot") = Some(Arc::downgrade(&sandbox));

    let timeout_ms = if how == "timeout" {
        300.0
    } else {
        f64::INFINITY
    };
    // TS 夹具的 abort 走 `controller.abort()`（无参）：`signal.reason` 是 `message` 为
    // `This operation was aborted` 的 DOMException（`instanceof Error` 为真）。裸令牌取消就是
    // 这条路径，不需要显式 reason。
    let result: CodemodeResult = sandbox
        .base()
        .execute(
            SCRIPT,
            CodemodeExecuteOptions {
                signal: Some(controller.clone()),
                timeout_ms: Some(timeout_ms),
                store: None,
            },
        )
        .await
        .expect("not closed");

    if result.ok() {
        eprintln!("The script completed instead of being cancelled");
        std::process::exit(1);
    }
    sandbox.close().await;
    let kind = match &result {
        CodemodeResult::Err(error) => match error.error.kind {
            kiana_codemode::CodemodeErrorKind::Timeout => "timeout",
            kiana_codemode::CodemodeErrorKind::Aborted => "aborted",
            kiana_codemode::CodemodeErrorKind::Sandbox => "sandbox",
            kiana_codemode::CodemodeErrorKind::Script => "script",
        },
        CodemodeResult::Ok(_) => unreachable!("checked above"),
    };
    println!(
        "{}",
        json!({
            "type": "result",
            "kind": kind,
            "elapsedMs": started.elapsed().as_secs_f64() * 1000.0,
            "workerExited": worker_exited.load(Ordering::SeqCst),
        })
    );
    // TS 的 `process.on("exit")`：进程退出前的时刻。
    println!(
        "{}",
        json!({ "type": "exit", "elapsedMs": started.elapsed().as_secs_f64() * 1000.0 })
    );
}
