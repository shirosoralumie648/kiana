//! `codemode/test/worker-lifetime.test.ts` 的移植。
//!
//! TS 用 `vi.spyOn(Worker.prototype, ...)` 断言 terminate/ref/unref 的调用次数与顺序；Rust 没有
//! 这个接缝，[`kiana_codemode::runtime::host::WorkerSpawner`] 就是它的等价物：测试注入一个记录
//! spawn/terminate/unref 的假 spawner，其余记账（admission、release、`onWorkerStarted` 的
//! `exited`、exit 之后的失败）走与生产实现完全相同的代码路径。
//!
//! 进程生命周期的那一半（`fixtures/worker-process-exit.ts` → `examples/worker_process_exit.rs`）
//! 在这里作为子进程跑，断言两行 JSON：结果行与退出行。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kiana_codemode::runtime::execution::CodemodeSandboxBase;
use kiana_codemode::runtime::host::{
    CodemodeSandbox, DEFAULT_WORKER_ENTRY, ThreadSpawner, WorkerHandle, WorkerObserver,
    WorkerSpawner, WorkerWasm, is_watch_notification, worker_launcher_with,
};
use kiana_codemode::runtime::protocol::{HostToWorkerMessage, VmStartData};
use kiana_codemode::types::BoxFuture;
use kiana_codemode::{
    CodemodeExecuteOptions, CodemodeRunOptions, CodemodeSandboxBaseOptions, CodemodeSandboxOptions,
};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// 假 spawner：记录调用并手动驱动 worker 事件
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    spawns: AtomicUsize,
    terminates: AtomicUsize,
    unrefs: AtomicUsize,
    /// unref 相对 terminate 的次序（每个 terminate 记 1，每个 unref 记 2；顺序由调用方检查）。
    order: Mutex<Vec<&'static str>>,
    posts: Mutex<Vec<HostToWorkerMessage>>,
}

struct FakeHandle {
    counters: Arc<Counters>,
}

impl WorkerHandle for FakeHandle {
    fn post(&self, message: HostToWorkerMessage) {
        self.counters.posts.lock().unwrap().push(message);
    }

    fn terminate(&self) {
        self.counters.terminates.fetch_add(1, Ordering::SeqCst);
        self.counters.order.lock().unwrap().push("terminate");
    }

    fn unref(&self) {
        self.counters.unrefs.fetch_add(1, Ordering::SeqCst);
        self.counters.order.lock().unwrap().push("unref");
    }
}

struct FakeSpawner {
    counters: Arc<Counters>,
    result: Mutex<Result<(), String>>,
    observer: Mutex<Option<Arc<dyn WorkerObserver>>>,
    /// 每次 spawn 的 observer（按顺序），用于需要同时驱动多个 worker 的测试。
    observers: Mutex<Vec<Arc<dyn WorkerObserver>>>,
    load_error: Mutex<Option<String>>,
    interrupt: Mutex<Option<Arc<AtomicI32>>>,
    start: Mutex<Option<VmStartData>>,
}

impl FakeSpawner {
    fn new() -> Arc<FakeSpawner> {
        Arc::new(FakeSpawner {
            counters: Arc::new(Counters::default()),
            result: Mutex::new(Ok(())),
            observer: Mutex::new(None),
            observers: Mutex::new(Vec::new()),
            load_error: Mutex::new(None),
            interrupt: Mutex::new(None),
            start: Mutex::new(None),
        })
    }

    fn failing(message: &str) -> Arc<FakeSpawner> {
        let spawner = FakeSpawner::new();
        *spawner.result.lock().unwrap() = Err(message.to_string());
        spawner
    }

    fn fail_loading(message: &str) -> Arc<FakeSpawner> {
        let spawner = FakeSpawner::new();
        *spawner.load_error.lock().unwrap() = Some(message.to_string());
        spawner
    }

    fn observer(&self) -> Arc<dyn WorkerObserver> {
        self.observer
            .lock()
            .unwrap()
            .clone()
            .expect("worker was spawned")
    }

    /// 第 `index` 次 spawn 的 observer（每个 worker 各自的事件通道）。
    fn observer_at(&self, index: usize) -> Arc<dyn WorkerObserver> {
        self.observers
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_else(|| panic!("worker {index} was never spawned"))
    }

    fn interrupt(&self) -> Arc<AtomicI32> {
        self.interrupt
            .lock()
            .unwrap()
            .clone()
            .expect("worker was spawned")
    }
}

impl WorkerSpawner for FakeSpawner {
    fn load_wasm(&self) -> BoxFuture<'static, Result<WorkerWasm, String>> {
        let error = self.load_error.lock().unwrap().clone();
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
        start: VmStartData,
        _wasm: WorkerWasm,
        interrupt: Arc<AtomicI32>,
        observer: Arc<dyn WorkerObserver>,
    ) -> Result<Arc<dyn WorkerHandle>, String> {
        self.counters.spawns.fetch_add(1, Ordering::SeqCst);
        *self.observer.lock().unwrap() = Some(Arc::clone(&observer));
        self.observers.lock().unwrap().push(observer);
        *self.interrupt.lock().unwrap() = Some(interrupt);
        *self.start.lock().unwrap() = Some(start);
        match &*self.result.lock().unwrap() {
            Ok(()) => Ok(Arc::new(FakeHandle {
                counters: Arc::clone(&self.counters),
            }) as Arc<dyn WorkerHandle>),
            Err(message) => Err(message.clone()),
        }
    }
}

/// `acquireWorker` 的记账：TS 的 `vi.fn(() => releaseWorker)`。
#[derive(Default)]
struct Admission {
    acquires: AtomicUsize,
    releases: AtomicUsize,
}

fn acquire_hook(
    admission: &Arc<Admission>,
) -> Arc<dyn Fn() -> kiana_codemode::types::CodemodeWorkerRelease + Send + Sync> {
    let admission = Arc::clone(admission);
    Arc::new(move || {
        admission.acquires.fetch_add(1, Ordering::SeqCst);
        let admission = Arc::clone(&admission);
        Box::new(move || {
            admission.releases.fetch_add(1, Ordering::SeqCst);
        }) as kiana_codemode::types::CodemodeWorkerRelease
    })
}

/// 真实的 [`ThreadSpawner`]，只把 `load_wasm` 换成不编译的占位。
///
/// 默认 feature 下 [`ThreadSpawner::load_wasm`] 立刻失败；`wasm` 下编译 637 KB 的
/// quickjs.wasm 在 debug 构建里要十几秒。这两件事都会盖住本组测试真正验证的 `workerUrl` 校验
/// 与线程记账，因此这里跳过编译（TS 侧模块级的 `loadQuickJSWasm()` 早已缓存）。
struct NoWasmThreadSpawner;

impl WorkerSpawner for NoWasmThreadSpawner {
    fn load_wasm(&self) -> BoxFuture<'static, Result<WorkerWasm, String>> {
        Box::pin(async { Ok(Arc::new(()) as WorkerWasm) })
    }

    fn spawn(
        &self,
        worker_url: Option<&str>,
        start: VmStartData,
        wasm: WorkerWasm,
        interrupt: Arc<AtomicI32>,
        observer: Arc<dyn WorkerObserver>,
    ) -> Result<Arc<dyn WorkerHandle>, String> {
        ThreadSpawner.spawn(worker_url, start, wasm, interrupt, observer)
    }
}

fn sandbox_with(spawner: &Arc<FakeSpawner>, options: CodemodeSandboxOptions) -> CodemodeSandbox {
    CodemodeSandbox::with_spawner(options, Arc::clone(spawner) as Arc<dyn WorkerSpawner>)
        .expect("valid options")
}

fn quiet_options() -> CodemodeSandboxOptions {
    CodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions {
            timeout_ms: Some(f64::INFINITY),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn wait_until(mut check: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("timed out waiting for {what}");
}

// ---------------------------------------------------------------------------
// 记账（#10748）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn does_not_start_a_worker_when_admission_is_denied() {
    let spawner = FakeSpawner::new();
    let started = Arc::new(AtomicUsize::new(0));
    let started_sink = Arc::clone(&started);
    // TS：`acquireWorker` 抛错 → 拒绝准入。冻结签名无法返回 Result，因此用 panic 表达 throw。
    let acquire: kiana_codemode::types::CodemodeAcquireWorker =
        Arc::new(|| -> kiana_codemode::types::CodemodeWorkerRelease {
            panic!("worker capacity exhausted");
        });
    let sandbox = sandbox_with(
        &spawner,
        CodemodeSandboxOptions {
            acquire_worker: Some(acquire),
            on_worker_started: Some(Arc::new(move |_exited| {
                started_sink.fetch_add(1, Ordering::SeqCst);
            })),
            ..quiet_options()
        },
    );
    let result = sandbox
        .execute("while (true) {}", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");
    let kiana_codemode::CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.error.message, "worker capacity exhausted");
    assert_eq!(spawner.counters.spawns.load(Ordering::SeqCst), 0);
    assert_eq!(started.load(Ordering::SeqCst), 0);
    sandbox.close().await;
}

#[tokio::test]
async fn releases_admission_when_worker_construction_fails() {
    let spawner = FakeSpawner::failing("Cannot find module '/nope.js'");
    let admission = Arc::new(Admission::default());
    let sandbox = sandbox_with(
        &spawner,
        CodemodeSandboxOptions {
            acquire_worker: Some(acquire_hook(&admission)),
            ..quiet_options()
        },
    );
    let result = sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");
    let kiana_codemode::CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert!(error.error.message.starts_with("Failed to start worker:"));
    assert_eq!(admission.acquires.load(Ordering::SeqCst), 1);
    assert_eq!(admission.releases.load(Ordering::SeqCst), 1);
    sandbox.close().await;
}

#[tokio::test]
async fn does_not_acquire_admission_when_wasm_loading_fails() {
    let spawner = FakeSpawner::fail_loading("wasm unavailable");
    let admission = Arc::new(Admission::default());
    let sandbox = sandbox_with(
        &spawner,
        CodemodeSandboxOptions {
            acquire_worker: Some(acquire_hook(&admission)),
            ..quiet_options()
        },
    );
    let result = sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");
    let kiana_codemode::CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(
        error.error.message,
        "Failed to load QuickJS: wasm unavailable"
    );
    assert_eq!(admission.acquires.load(Ordering::SeqCst), 0);
    sandbox.close().await;
}

#[tokio::test]
async fn the_worker_url_override_is_passed_to_the_spawner() {
    let spawner = FakeSpawner::new();
    let seen: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    struct RecordingSpawner {
        inner: Arc<FakeSpawner>,
        seen: Arc<Mutex<Option<Option<String>>>>,
    }
    impl WorkerSpawner for RecordingSpawner {
        fn load_wasm(&self) -> BoxFuture<'static, Result<WorkerWasm, String>> {
            self.inner.load_wasm()
        }
        fn spawn(
            &self,
            worker_url: Option<&str>,
            start: VmStartData,
            wasm: WorkerWasm,
            interrupt: Arc<AtomicI32>,
            observer: Arc<dyn WorkerObserver>,
        ) -> Result<Arc<dyn WorkerHandle>, String> {
            *self.seen.lock().unwrap() = Some(worker_url.map(str::to_string));
            self.inner
                .spawn(worker_url, start, wasm, interrupt, observer)
        }
    }
    let recording = Arc::new(RecordingSpawner {
        inner: Arc::clone(&spawner),
        seen: Arc::clone(&seen),
    });
    let sandbox = CodemodeSandbox::with_spawner(
        CodemodeSandboxOptions {
            worker_url: Some("/tmp/custom-worker.js".into()),
            ..quiet_options()
        },
        Arc::clone(&recording) as Arc<dyn WorkerSpawner>,
    )
    .expect("valid");
    let result = sandbox.execute("return 1", CodemodeExecuteOptions::default());
    wait_until(
        || spawner.counters.spawns.load(Ordering::SeqCst) == 1,
        "spawn",
    )
    .await;
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(Some("/tmp/custom-worker.js".to_string()))
    );
    // 正常结束（done 由假 worker 发出）之外的路径：让 close 收尾。
    drop(result);
    sandbox.close().await;
}

// ---------------------------------------------------------------------------
// 结算而不等待 worker 退出（abort / timeout / close）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn settles_without_waiting_for_worker_exit() {
    for how in ["abort", "timeout", "close"] {
        let spawner = FakeSpawner::new();
        let admission = Arc::new(Admission::default());
        let did_exit = Arc::new(AtomicBool::new(false));
        let exit_sink = Arc::clone(&did_exit);
        let exited_seen = Arc::new(AtomicUsize::new(0));
        let exited_ops = Arc::clone(&exited_seen);
        // TS: `expect(toolSignal?.aborted).toBe(true)`——工具的 `context.signal` 必须真的被取消。
        let tool_cancelled = Arc::new(AtomicBool::new(false));
        let tool_cancelled_sink = Arc::clone(&tool_cancelled);
        let sandbox: Arc<CodemodeSandbox> = Arc::new(sandbox_with(
            &spawner,
            CodemodeSandboxOptions {
                base: CodemodeSandboxBaseOptions {
                    timeout_ms: Some(100.0),
                    tools: Some(vec![kiana_codemode::CodemodeTool::new(
                        "hang",
                        Arc::new(
                            move |_args: Value, context: kiana_codemode::CodemodeToolContext| {
                                let tool_cancelled_sink = Arc::clone(&tool_cancelled_sink);
                                Box::pin(async move {
                                    context.signal.cancelled().await;
                                    tool_cancelled_sink.store(true, Ordering::SeqCst);
                                    Err("cancelled".to_string())
                                })
                            },
                        ),
                    )]),
                    ..Default::default()
                },
                acquire_worker: Some(acquire_hook(&admission)),
                on_worker_started: Some(Arc::new(move |exited| {
                    exited_ops.fetch_add(1, Ordering::SeqCst);
                    let exit_sink = Arc::clone(&exit_sink);
                    tokio::spawn(async move {
                        exited.await;
                        exit_sink.store(true, Ordering::SeqCst);
                    });
                })),
                ..Default::default()
            },
        ));
        let token = tokio_util::sync::CancellationToken::new();
        let pending = {
            let sandbox = Arc::clone(&sandbox);
            let token = token.clone();
            async move {
                sandbox
                    .execute(
                        "text(\"partial\"); await tools.hang()",
                        CodemodeExecuteOptions {
                            signal: Some(token),
                            ..Default::default()
                        },
                    )
                    .await
            }
        };
        let results: Arc<Mutex<Option<kiana_codemode::CodemodeResult>>> =
            Arc::new(Mutex::new(None));
        let results_sink = Arc::clone(&results);
        let handle = tokio::spawn(async move {
            let result = pending.await.expect("not closed");
            *results_sink.lock().unwrap() = Some(result);
        });
        // 等工具调用到达宿主（假 worker 先发 output 与 call）。
        wait_until(
            || spawner.counters.spawns.load(Ordering::SeqCst) == 1,
            "spawn",
        )
        .await;
        let observer = spawner.observer();
        observer.message(json!({"type":"output","item":{"type":"text","text":"partial"}}));
        observer.message(json!({"type":"call","id":1,"target":"tool","name":"hang","args":"null"}));

        match how {
            "abort" => token.cancel(),
            "timeout" => tokio::time::sleep(Duration::from_millis(150)).await,
            "close" => sandbox.close().await,
            _ => unreachable!(),
        }
        // 结算不等待退出：结果在 terminate 之后立刻可用。
        wait_until(|| results.lock().unwrap().is_some(), "result").await;
        let result = results.lock().unwrap().clone().expect("result");
        let kiana_codemode::CodemodeResult::Err(error) = &result else {
            panic!("{how}: expected failure, got {result:?}");
        };
        assert_eq!(
            error.error.kind,
            if how == "timeout" {
                kiana_codemode::CodemodeErrorKind::Timeout
            } else {
                kiana_codemode::CodemodeErrorKind::Aborted
            },
            "{how}"
        );
        assert_eq!(
            error.output,
            vec![kiana_codemode::CodemodeOutputItem::text("partial")],
            "{how}"
        );
        assert_eq!(error.calls.len(), 1, "{how}");
        assert_eq!(error.calls[0].name, "hang");
        assert_eq!(
            error.calls[0].status,
            kiana_codemode::CodemodeCallStatus::Cancelled,
            "{how}"
        );
        // 工具信号已 abort（cancel 之后工具任务可能还没跑完，等到它真的看到取消）。
        wait_until(
            || tool_cancelled.load(Ordering::SeqCst),
            "the tool's signal to be cancelled",
        )
        .await;
        // terminate 只一次、unref 只一次，且 unref 在 terminate 之后。
        assert_eq!(
            spawner.counters.terminates.load(Ordering::SeqCst),
            1,
            "{how}"
        );
        assert_eq!(spawner.counters.unrefs.load(Ordering::SeqCst), 1, "{how}");
        let order = spawner.counters.order.lock().unwrap().clone();
        assert_eq!(order, vec!["terminate", "unref"], "{how}");
        // 中断标志已设置。
        assert_eq!(spawner.interrupt().load(Ordering::SeqCst), 1, "{how}");
        // `exited` 尚未 resolve（worker 还没真的退出）。
        assert_eq!(exited_seen.load(Ordering::SeqCst), 1, "{how}");
        assert!(!did_exit.load(Ordering::SeqCst), "{how}");

        // close() 与迟到的 worker 消息都不得替换已决结果，也不得再等待退出。
        sandbox.close().await;
        observer.message(json!({"type":"done","ok":true,"value":"42","writes":"[]"}));
        handle.await.expect("task");
        assert_eq!(results.lock().unwrap().clone(), Some(result), "{how}");
        assert_eq!(
            spawner.counters.terminates.load(Ordering::SeqCst),
            1,
            "{how}"
        );
        // 真实退出之后 `exited` 才 resolve。
        observer.exit(0);
        wait_until(|| did_exit.load(Ordering::SeqCst), "didExit").await;
    }
}

#[tokio::test]
async fn reports_the_actual_exit_of_execute_and_start_workers() {
    let spawner = FakeSpawner::new();
    let admission = Arc::new(Admission::default());
    let exits: Arc<Mutex<Vec<Arc<AtomicBool>>>> = Arc::new(Mutex::new(Vec::new()));
    let exits_sink = Arc::clone(&exits);
    let sandbox = sandbox_with(
        &spawner,
        CodemodeSandboxOptions {
            acquire_worker: Some(acquire_hook(&admission)),
            on_worker_started: Some(Arc::new(move |exited| {
                let flag = Arc::new(AtomicBool::new(false));
                exits_sink.lock().unwrap().push(Arc::clone(&flag));
                tokio::spawn(async move {
                    exited.await;
                    flag.store(true, Ordering::SeqCst);
                });
            })),
            ..quiet_options()
        },
    );

    // execute()
    let pending = sandbox.execute("return 1", CodemodeExecuteOptions::default());
    wait_until(
        || spawner.counters.spawns.load(Ordering::SeqCst) == 1,
        "spawn 1",
    )
    .await;
    spawner
        .observer()
        .message(json!({"type":"done","ok":true,"value":"1","writes":"[]"}));
    let result = pending.await.expect("not closed");
    assert!(result.ok());
    assert_eq!(exits.lock().unwrap().len(), 1);
    // 第一个 worker 还没退出：`exited` 未 resolve。
    assert!(!exits.lock().unwrap()[0].load(Ordering::SeqCst));

    // start()
    let run = sandbox
        .start(
            "return 2",
            CodemodeRunOptions {
                started_at: 0.0,
                seed: 0.0,
                timeout_ms: Some(f64::INFINITY),
                ..Default::default()
            },
        )
        .expect("run");
    wait_until(
        || spawner.counters.spawns.load(Ordering::SeqCst) == 2,
        "spawn 2",
    )
    .await;
    spawner
        .observer()
        .message(json!({"type":"done","ok":true,"value":"2","writes":"[]"}));
    assert!(run.finished());
    let result = run.result.await;
    assert!(result.ok());
    assert_eq!(exits.lock().unwrap().len(), 2);

    // 两个 worker 都还没退出：`exited` 都未 resolve、release 未调用。
    assert_eq!(admission.releases.load(Ordering::SeqCst), 0);
    assert!(!exits.lock().unwrap()[0].load(Ordering::SeqCst));
    assert!(!exits.lock().unwrap()[1].load(Ordering::SeqCst));
    // 每个 worker 真实退出一次 → 各自的 exited resolve、release 各一次（TS 断言
    // `expect(releaseWorker).toHaveBeenCalledTimes(2)` 与 `await Promise.all(exits)`）。
    spawner.observer_at(0).exit(0);
    spawner.observer_at(1).exit(0);
    wait_until(
        || admission.releases.load(Ordering::SeqCst) == 2,
        "both releases",
    )
    .await;
    wait_until(
        || {
            let exits = exits.lock().unwrap();
            exits[0].load(Ordering::SeqCst) && exits[1].load(Ordering::SeqCst)
        },
        "both exits",
    )
    .await;
    sandbox.close().await;
    // `close()` 不等待退出；这里只断言记账与结果一致。
    assert_eq!(admission.acquires.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn requests_termination_when_on_worker_started_throws() {
    let spawner = FakeSpawner::new();
    let did_exit = Arc::new(AtomicBool::new(false));
    let exit_sink = Arc::clone(&did_exit);
    let exited_seen = Arc::new(AtomicUsize::new(0));
    let exited_ops = Arc::clone(&exited_seen);
    let admission = Arc::new(Admission::default());
    let sandbox = sandbox_with(
        &spawner,
        CodemodeSandboxOptions {
            acquire_worker: Some(acquire_hook(&admission)),
            on_worker_started: Some(Arc::new(move |exited| {
                exited_ops.fetch_add(1, Ordering::SeqCst);
                let exit_sink = Arc::clone(&exit_sink);
                tokio::spawn(async move {
                    exited.await;
                    exit_sink.store(true, Ordering::SeqCst);
                });
                // TS 的 `throw new Error("lifetime hook failed")`。
                panic!("lifetime hook failed");
            })),
            ..quiet_options()
        },
    );
    let result = sandbox
        .execute("while (true) {}", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");
    let kiana_codemode::CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.error.message, "lifetime hook failed");
    assert_eq!(exited_seen.load(Ordering::SeqCst), 1);
    // 回调抛错 → 请求终止（`void stop()`）。
    wait_until(
        || spawner.counters.terminates.load(Ordering::SeqCst) == 1,
        "terminate",
    )
    .await;
    // worker 真正退出时 release 恰好一次，且 `exited` resolve。
    spawner.observer().exit(0);
    wait_until(|| did_exit.load(Ordering::SeqCst), "didExit").await;
    assert_eq!(admission.releases.load(Ordering::SeqCst), 1);
    sandbox.close().await;
}

// ---------------------------------------------------------------------------
// Node `--watch` 过滤 与 缺失 worker 文件（sandbox.test.ts 的 worker host 组）
// ---------------------------------------------------------------------------

#[test]
fn watch_notifications_use_key_presence_not_own_properties() {
    assert!(is_watch_notification(
        &json!({"watch:require": ["/worker.js"]})
    ));
    assert!(is_watch_notification(
        &json!({"watch:import": ["file:///worker.js"]})
    ));
    // 只有这两个键名命中；其它键（包括继承自 `Object.prototype` 的名字）都不算。
    assert!(!is_watch_notification(&json!({"toString": 1})));
    assert!(!is_watch_notification(&json!({"watch:imports": 1})));
    // 普通消息不是。
    assert!(!is_watch_notification(&json!({"type": "done", "ok": true})));
    // 非对象（`null`/字符串/数字）在 TS 里被 `typeof message === "object" && message !== null` 挡掉。
    assert!(!is_watch_notification(&Value::Null));
    assert!(!is_watch_notification(&json!("watch:import")));
    assert!(!is_watch_notification(&json!(7)));
    assert!(!is_watch_notification(&json!([])));
}

#[tokio::test]
async fn ignores_node_watch_notifications_from_the_worker() {
    let spawner = FakeSpawner::new();
    let sandbox = sandbox_with(&spawner, quiet_options());
    let pending = sandbox.execute("return 42", CodemodeExecuteOptions::default());
    wait_until(
        || spawner.counters.spawns.load(Ordering::SeqCst) == 1,
        "spawn",
    )
    .await;
    let observer = spawner.observer();
    observer.message(json!({"watch:require": ["/worker.js"]}));
    observer.message(json!({"watch:import": ["file:///worker.js"]}));
    observer.message(json!({"type":"done","ok":true,"value":"42","writes":"[]"}));
    let result = pending.await.expect("not closed");
    assert_eq!(
        result,
        kiana_codemode::CodemodeResult::success(
            Some(json!(42)),
            vec![],
            vec![],
            kiana_codemode::CodemodeStoreWrites::default(),
            None,
        )
    );
    sandbox.close().await;
}

#[tokio::test]
async fn reports_a_missing_worker_file_as_a_sandbox_error() {
    // TS `sandbox.test.ts` 的 `reports a missing worker file as a sandbox error` 只断言
    // `{ ok: false, error: { kind: "sandbox" } }`；报文在 node 26 上是
    // `Cannot find module '<绝对路径>'`（异步 `error` 事件），`name` 是 `"Error"`。
    //
    // 用**不编译 wasm** 的 spawner，否则默认 feature 会先撞上 `Failed to load QuickJS:`。
    let file_sandbox = CodemodeSandbox::with_spawner(
        CodemodeSandboxOptions {
            worker_url: Some("./does-not-exist.js".into()),
            ..quiet_options()
        },
        Arc::new(NoWasmThreadSpawner) as Arc<dyn WorkerSpawner>,
    )
    .expect("valid");
    let result = file_sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");
    let kiana_codemode::CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    let expected_missing = format!(
        "Cannot find module '{}'",
        std::env::current_dir()
            .unwrap()
            .join("does-not-exist.js")
            .display()
    );
    assert_eq!(error.error.message, expected_missing);
    // node 26 的 `error` 事件带普通 `Error`（`name === "Error"`），`host.ts` 因此把它放进结果；
    // 冻结的 `CodemodeError` 有 `name` 字段，缺了它结果 JSON 就与 TS 不同。
    assert_eq!(error.error.name.as_deref(), Some("Error"));
    file_sandbox.close().await;

    // 同步的 `ERR_WORKER_PATH`：node 26 拒绝一切既非绝对、也不以 `./`/`../` 开头的字符串。
    for specifier in [
        "https://invalid.example/worker.js",
        "",
        "nope.js",
        "file:///x.js",
        "data:text/javascript,1",
        "data:text/plain,1",
        "node:fs",
        "C:/x.js",
    ] {
        let sandbox = CodemodeSandbox::with_spawner(
            CodemodeSandboxOptions {
                worker_url: Some(specifier.to_string()),
                ..quiet_options()
            },
            Arc::new(NoWasmThreadSpawner) as Arc<dyn WorkerSpawner>,
        )
        .expect("valid");
        let result = sandbox
            .execute("return 1", CodemodeExecuteOptions::default())
            .await
            .expect("not closed");
        let kiana_codemode::CodemodeResult::Err(error) = &result else {
            panic!("expected failure for {specifier:?}");
        };
        assert_eq!(
            error.error.kind,
            kiana_codemode::CodemodeErrorKind::Sandbox,
            "{specifier:?}"
        );
        // 两个补充子句各自按前缀判定（node 26 的 `internal/errors` 模板）。
        let mut wrap = String::new();
        if specifier.starts_with("file://") {
            wrap.push_str(" Wrap file:// URLs with `new URL`.");
        }
        if specifier.starts_with("data:text/javascript") {
            wrap.push_str(" Wrap data: URLs with `new URL`.");
        }
        let wrap = wrap.as_str();
        assert_eq!(
            error.error.message,
            format!(
                "Failed to start worker: The worker script or module filename must be an absolute \
                 path or a relative path starting with './' or '../'.{wrap} Received \"{specifier}\""
            ),
            "{specifier:?}"
        );
        sandbox.close().await;
    }
}

/// TS 的 worker 是真线程：`startVm` 里未捕获的异常会触发 `error` 事件（`kind: "sandbox"`，
/// `name`/`message` 来自那个错误），**然后**触发 `exit`（进而 release 准入）。Rust 里「未捕获
/// 异常」是 panic，因此线程入口必须 `catch_unwind` 并照常上报——否则线程在 `exit` 之前就结束，
/// 执行永远不结算、admission 泄漏（reviewer 用会 panic 的 `startVm` 复现了挂起）。
/// reviewer 的复现路径：`startVm` panic 时（TS 的 uncaught exception）执行必须以
/// `kind: "sandbox"` 结算，并且 admission 在 worker 线程真正退出时 release 恰好一次
/// （修复前：acquires = 1、releases = 0、结果永远不结算）。
#[tokio::test]
async fn a_panicking_start_vm_settles_the_execution_and_releases_admission() {
    /// 真实线程 + 真实 `spawn_worker_thread_with`，只注入会 panic 的 `startVm` 与占位 wasm。
    struct PanickingSpawner;

    impl WorkerSpawner for PanickingSpawner {
        fn load_wasm(&self) -> BoxFuture<'static, Result<WorkerWasm, String>> {
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
            kiana_codemode::runtime::worker::spawn_worker_thread_with(
                Some(Arc::new(|_start| panic!("the VM backend exploded"))),
                start,
                wasm,
                interrupt,
                observer,
            )
        }
    }

    let admission = Arc::new(Admission::default());
    let sandbox = CodemodeSandbox::with_spawner(
        CodemodeSandboxOptions {
            acquire_worker: Some(acquire_hook(&admission)),
            ..quiet_options()
        },
        Arc::new(PanickingSpawner) as Arc<dyn WorkerSpawner>,
    )
    .expect("valid");
    let result = sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");
    let kiana_codemode::CodemodeResult::Err(error) = &result else {
        panic!("expected failure, got {result:?}");
    };
    assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.error.message, "the VM backend exploded");
    assert_eq!(error.error.name.as_deref(), Some("Error"));
    assert_eq!(admission.acquires.load(Ordering::SeqCst), 1);
    wait_until(
        || admission.releases.load(Ordering::SeqCst) == 1,
        "the admission to be released when the worker exits",
    )
    .await;
    sandbox.close().await;
}

#[tokio::test]
async fn a_panicking_start_vm_reports_an_error_and_exits() {
    /// 只记录事件，不经过真正的宿主（`HostObserver` 是私有的）。
    #[derive(Default)]
    struct Recorder {
        errors: Mutex<Vec<kiana_codemode::CodemodeError>>,
        exits: Mutex<Vec<i32>>,
    }

    impl WorkerObserver for Recorder {
        fn message(&self, _message: Value) {}
        fn error(&self, error: kiana_codemode::CodemodeError) {
            self.errors.lock().unwrap().push(error);
        }
        fn exit(&self, code: i32) {
            self.exits.lock().unwrap().push(code);
        }
    }

    // 直接驱动线程入口：注入一个会 panic 的 `startVm`（不走 `set_start_vm` 的进程级单例，
    // 否则会污染同一进程里跑的其他 worker 测试）。
    let recorder = Arc::new(Recorder::default());
    let worker = kiana_codemode::runtime::worker::spawn_worker_thread_with(
        Some(Arc::new(|_start| panic!("the VM backend exploded"))),
        VmStartData::new("return 1"),
        Arc::new(()),
        Arc::new(AtomicI32::new(0)),
        Arc::clone(&recorder) as Arc<dyn WorkerObserver>,
    )
    .expect("thread");
    wait_until(
        || !recorder.exits.lock().unwrap().is_empty(),
        "the worker thread to exit",
    )
    .await;
    let errors = recorder.errors.lock().unwrap().clone();
    assert_eq!(errors.len(), 1, "one error event: {errors:?}");
    assert_eq!(errors[0].kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(errors[0].message, "the VM backend exploded");
    // `host.ts`：`name: error instanceof Error ? error.name : undefined`——panic 按普通 Error 上报。
    assert_eq!(errors[0].name.as_deref(), Some("Error"));
    assert_eq!(*recorder.exits.lock().unwrap(), vec![0]);
    worker.terminate();
    worker.unref();
}

#[tokio::test]
async fn reports_a_failing_wasm_module_as_a_sandbox_error() {
    let sandbox = CodemodeSandbox::with_spawner(
        CodemodeSandboxOptions {
            base: CodemodeSandboxBaseOptions {
                timeout_ms: Some(f64::INFINITY),
                ..Default::default()
            },
            // 默认 spawner 没有可注入的 wasm，这里用假 spawner 的加载失败路径。
            ..Default::default()
        },
        FakeSpawner::fail_loading("no wasm") as Arc<dyn WorkerSpawner>,
    )
    .expect("valid");
    let result = sandbox
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");
    let kiana_codemode::CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(error.error.message, "Failed to load QuickJS: no wasm");
    sandbox.close().await;
}

#[test]
fn the_default_worker_entry_is_named() {
    assert_eq!(DEFAULT_WORKER_ENTRY, "kiana-codemode/worker");
}

// ---------------------------------------------------------------------------
// 真实线程 spawner：默认后端接线（默认 feature 下 `load_wasm` 先失败；feature `wasm` 下跑真 VM）
// ---------------------------------------------------------------------------

/// 用真实 [`worker_launcher_with`] + [`ThreadSpawner`] 跑一次执行，验证**没有**注入 `startVm`、
/// 也没有注册表条目时线程入口仍能接上后端：
///
/// - 默认 feature（无 wasm）：`load_wasm` 立刻失败，执行以 `kind: "sandbox"` 结算；
/// - feature `wasm`：内置适配器调用 `runtime/vm.rs` 的 `start_vm`，`return 1` 真的跑出 `1`。
///
/// 合并前 `runtime/vm.rs` 在本工作树里是占位，因此 wasm 分支只在合并后（以及合入 vm 单元的
/// scratch 校验）可达。
#[tokio::test]
async fn the_thread_spawner_wires_the_default_worker_backend() {
    let launcher = worker_launcher_with(
        quiet_options(),
        Arc::new(ThreadSpawner) as Arc<dyn WorkerSpawner>,
    );
    let base = Arc::new(
        CodemodeSandboxBase::new(CodemodeSandboxBaseOptions::default(), launcher).expect("valid"),
    );
    let result = base
        .execute("return 1", CodemodeExecuteOptions::default())
        .await
        .expect("not closed");

    #[cfg(not(feature = "wasm"))]
    {
        let kiana_codemode::CodemodeResult::Err(error) = &result else {
            panic!("expected failure, got {result:?}");
        };
        assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
        assert!(
            error.error.message.starts_with("Failed to load QuickJS:"),
            "{}",
            error.error.message
        );
    }
    #[cfg(feature = "wasm")]
    {
        let kiana_codemode::CodemodeResult::Ok(ok) = &result else {
            panic!("expected success, got {result:?}");
        };
        assert_eq!(ok.value, Some(serde_json::json!(1)));
    }
    base.close().await;
}

// ---------------------------------------------------------------------------
// 进程生命周期（TS: `describe.each(runtimes)("$name process lifetime")`）
// ---------------------------------------------------------------------------

/// 跑 `examples/worker_process_exit` 子进程并返回它输出的两行 JSON。
fn run_process_fixture(how: &str) -> Option<(Value, Value, Duration)> {
    // `CARGO_BIN_EXE_*` 只对 `[[bin]]` 生成；示例用 `cargo run` 的路径推算：先按 target 目录找。
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    let fixture = dir.join("examples").join("worker_process_exit");
    if !fixture.exists() {
        return None;
    }
    let started = Instant::now();
    let output = std::process::Command::new(&fixture)
        .arg(how)
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let result: Value = serde_json::from_str(lines.next()?).ok()?;
    let exit: Value = serde_json::from_str(lines.next()?).ok()?;
    Some((result, exit, started.elapsed()))
}

#[test]
fn the_process_fixture_prints_two_json_lines_and_exits_promptly() {
    // TS `worker-lifetime.test.ts`（Node 那一半）：
    // - 两行 JSON（result、exit）；
    // - `result.kind` 是 `abort`/`close` → `"aborted"`、`timeout` → `"timeout"`（TS 从不允许 `sandbox`）；
    // - `result.elapsedMs < 1000`；
    // - `exit.elapsedMs - result.elapsedMs < 1000`。
    //
    // 夹具用一个**卡住**的 `startVm`（见 examples/worker_process_exit.rs 的模块文档），因此它
    // 在所有 feature 组合下都走真实的取消路径，不受 debug 构建的 wasm 编译时间影响。
    for how in ["abort", "timeout", "close"] {
        let Some((result, exit, wall)) = run_process_fixture(how) else {
            // 示例未构建（`cargo test` 默认会构建 examples；直接用 `--test` 时可能没有）。
            eprintln!("skipping: the worker_process_exit example is not built");
            return;
        };
        assert_eq!(result["type"], "result", "{how}");
        let expected = if how == "timeout" {
            "timeout"
        } else {
            "aborted"
        };
        assert_eq!(result["kind"].as_str(), Some(expected), "{how}");
        assert!(
            result["elapsedMs"].as_f64().expect("elapsedMs") < 1000.0,
            "{how}: the result must settle promptly (was {})",
            result["elapsedMs"]
        );
        // worker 不阻塞进程退出：`onWorkerStarted` 的 exited 尚未 resolve（`workerExited: false`）。
        assert_eq!(result["workerExited"], json!(false), "{how}");
        assert_eq!(exit["type"], "exit", "{how}");
        assert!(
            exit["elapsedMs"].as_f64().expect("elapsedMs")
                - result["elapsedMs"].as_f64().expect("elapsedMs")
                < 1000.0,
            "{how}: the process must exit promptly after the result"
        );
        assert!(wall < Duration::from_secs(20), "{how}: fixture wall time");
    }
}

#[test]
fn the_process_fixture_rejects_unknown_stop_modes() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return,
    };
    let Some(dir) = exe.parent().and_then(|path| path.parent()) else {
        return;
    };
    let fixture = dir.join("examples").join("worker_process_exit");
    if !fixture.exists() {
        return;
    }
    let output = std::process::Command::new(&fixture)
        .arg("nonsense")
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown stop mode: nonsense"));
}
