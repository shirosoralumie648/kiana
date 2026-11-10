//! runtime/host.ts：worker 线程后端（`CodemodeSandbox`）。
//!
//! TS 源：`packages/codemode/src/runtime/host.ts`（88 行）。
//!
//! 每次执行都有自己的 worker 线程与 QuickJS VM。每次都用新 worker 让终止保持简单：失控脚本
//! （包括只在微任务队列里打转的）被终止后不会污染下一次执行。终止之前，宿主先设置 VM 轮询的
//! 中断标志；停止只请求终止、不等待退出：有些内建不轮询该标志，因此 worker 可以比结果活得更久。
//!
//! # Rust 映射（ARCHITECTURE.md §8 第 8 条 / spec §A.6 第 9 条）
//!
//! `std::thread` 无法强杀，所以「终止」= 设置中断标志 + 请求本线程退出 + **detach 且不 join**：
//! - `terminate()` 与 `unref()` 分开保留（测试要断言调用次数与先后顺序，见 TS 的 spy）；
//! - `onWorkerStarted(exited)` 的 `exited` 用一个 oneshot 在**真实退出**时 resolve，而不是在结果
//!   结算或 `close()` 时；
//! - `acquireWorker` 在 wasm 加载之后、worker 构造之前**同步**调用；构造失败或真实退出时
//!   release 恰好一次。
//!
//! [`WorkerSpawner`] 是这段逻辑的接缝：默认实现（[`ThreadSpawner`]，feature `wasm`）在独立
//! `std::thread` 上跑 [`crate::runtime::worker::run_worker`]；测试注入假 spawner 来断言上述
//! 记账（admission、release、terminate/unref），不需要真的启动 VM。
//!
//! 冻结的 `CodemodeSandboxOptions` 以 `Option<String>` 表示 `workerUrl`（TS 是 `string | URL`）。
//! Rust 没有「从 URL 载入 worker 入口」的机制：`worker_url` 被当作**入口是否存在**的检查，
//! 字符串形态逐字复刻 Node 26 的 `new Worker(specifier)`（见 `check_worker_url`）：既非绝对
//! 路径、也不以 `./`/`../` 开头 → 同步 `Failed to start worker: The worker script or module
//! filename must be an absolute path or a relative path starting with './' or '../'. … Received
//! "<specifier>"`；路径不存在 → 异步 `Failed to start worker: Cannot find module '<path>'`
//! （`name: "Error"`），像 Node 一样先 `error` 再 `exit` 1。真正执行的一律是编译进二进制的
//! worker 入口——这是 `string | URL` 收窄成 `Option<String>` 后剩下的最小偏差：URL 对象形态
//! （`new URL(...)`）在 Rust 侧没有对应类型。
//!
//! # 一次执行的生命周期（每次实际启动的执行一次）
//!
//! 1. [`CodemodeSandbox::new`]（或测试用 [`CodemodeSandbox::with_spawner`]）把
//!    [`worker_launcher_with`] 交给 [`CodemodeSandboxBase`]；之后**每次实际启动的执行**
//!    （sandbox 未关闭，且 signal 没有在启动前就取消）调用 launcher 一次，因此每个真正启动
//!    的执行都有独立的 worker 线程与 VM。sandbox 已关闭时 `execute()`/`start()` 在启动前
//!    返回；signal 已预先取消时 `Execution::start` 先以 `aborted` 结算，launcher 不会被
//!    调用，也就没有 worker/VM（`tests/execution.rs` 的
//!    `an_already_cancelled_signal_finishes_before_launching` 锁定后者）。
//! 2. `launch_worker` 先准备 wasm：`options.wasm` 优先，否则 `spawner.load_wasm()`；失败直接
//!    返回 `Failed to load QuickJS: …`，此时还没有占用准入配额（见测试
//!    `does_not_acquire_admission_when_wasm_loading_fails`）。
//! 3. 新建中断标志（`Arc<AtomicI32>`，VM 侧轮询它），然后同步调用 `options.acquire_worker`
//!    （panic = 拒绝准入，见 `CodemodeAcquireWorker` 的文档）。
//! 4. `spawner.spawn(...)` 构造 worker；失败时调用一次 release，并把文本包成
//!    `Failed to start worker: …`。
//! 5. 构造成功且给了 `on_worker_started` 时，回调收到一个只在 worker **真实退出**时 resolve
//!    的 future；回调 panic 会请求终止该 worker，并让执行以 panic 文本失败。
//!
//! `launch_worker` 返回的 `Err(String)` 由 [`crate::runtime::execution`] 的 launcher future
//! 路径转成 `kind: "sandbox"` 的结果；本模块不构造结果，负责产生错误文本（wasm 加载与
//! worker 构造失败带前缀，准入或 `on_worker_started` 回调 panic 的原文本不带前缀）。
//!
//! # 生命周期约束
//!
//! - 结果结算**不等待** worker 退出：`WorkerChannel::stop` 只设置中断标志、`terminate`、`unref`，
//!   从不 join。worker 线程可能比执行结果活得更久（`runtime/worker.rs` 的线程入口）。
//! - release 至多一次：构造失败时立即调用；构造成功时挂在 `exit` 事件上（`ReleaseOnce` 用
//!   `Arc<Mutex<Option<…>>>` 的 `take` 保证并发/迟到事件也只生效一次）。
//! - `exited` 通知也只在 `exit` 事件里 resolve，不在结果结算或 `close()` 时——测试
//!   `settles_without_waiting_for_worker_exit` 断言 terminate 之后 `exited` 仍未完成。
//! - `exit` 事件总会再报一次 `Worker exited with code {code} before the script settled`
//!   （即使 code 为 0）；执行业已结算时由 `execution.rs` 的 `finished` 丢弃。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::oneshot;

use crate::runtime::execution::{CodemodeSandboxBase, StartError, VmChannel, VmEvents, VmLauncher};
use crate::runtime::protocol::{HostToWorkerMessage, VmStartData};
use crate::types::{
    BoxFuture, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeResult,
    CodemodeRun, CodemodeRunOptions, CodemodeSandboxOptions, CodemodeTool,
};

/// 启动 worker 后可投递/终止的句柄（TS 的 `Worker` 对象在本单元用到的那一面）。
///
/// # 与 TS 的对应
///
/// - `post` ↔ `worker.postMessage`；
/// - `terminate` ↔ `worker.terminate()`（请求终止；**可能不返回**，因此调用方不等它）；
/// - `unref` ↔ `worker.unref()`（`terminate()` 内部会 ref，所以随后必须 unref）。
pub trait WorkerHandle: Send + Sync {
    /// 宿主 → worker 的消息。
    ///
    /// 实现必须容忍终止之后仍被调用（`runtime/worker.rs` 的线程句柄会静默丢弃）。
    fn post(&self, message: HostToWorkerMessage);
    /// 请求终止。只请求，不等待。
    fn terminate(&self);
    /// 释放宿主对 worker 的引用（Rust 里是 detach，不 join）。
    fn unref(&self);
}

/// worker 向宿主报告的回调（TS 的 `worker.on("message" | "error" | "exit")`）。
///
/// 这些方法由 spawner 在 worker 侧调用：正常启动的 worker 路径在 worker 线程上调用；
/// 入口文件缺失时由 `MissingWorker::report` 在 Tokio 任务（能取到运行时时）或 `spawn` 的
/// 调用线程上报告，没有 worker 线程。实现必须满足 `Send + Sync`，且不能假设自己跑在 Tokio
/// 任务里。`exit` 应报告线程的真实结束——宿主靠它释放准入配额并 resolve
/// `on_worker_started` 的 `exited`。重复调用 `exit` 不会重复 release（`ReleaseOnce`），但会
/// 让 [`VmEvents::failure`] 再收到一次「Worker exited …」。
pub trait WorkerObserver: Send + Sync {
    /// 一条 worker → 宿主消息（已过滤 Node `--watch` 的依赖报告）。
    fn message(&self, message: Value);
    /// worker 报告了一个错误。
    fn error(&self, error: CodemodeError);
    /// worker 退出，`code` 是退出码。
    fn exit(&self, code: i32);
}

/// 编译好的 wasm 模块在 spawner 接缝上的不透明句柄。
///
/// 默认 spawner 里它是 `CodemodeWasmModule`（feature `wasm` 时即 `Arc<wasmtime::Module>`）；
/// 测试里的假 spawner 可以忽略它。这样启动与生命周期逻辑在 feature `wasm` 关闭时也能被测试覆盖。
pub type WorkerWasm = Arc<dyn std::any::Any + Send + Sync>;

/// 启动一个 worker 的工厂。
///
/// TS 里这些步骤分散在 `workerLauncher` 中（`loadQuickJSWasm()` 与 `new Worker(...)`）；拆成
/// trait 是为了让测试替换掉真正起线程/编译 wasm 的部分，从而逐条断言记账行为。
///
/// 调用契约（由 `launch_worker` 依赖）：
/// - `options.wasm` 为 `Some` 时**不会**调用 `load_wasm`；
/// - 调用 `spawn` 之前，宿主已经取得准入配额；`spawn` 返回 `Err` 时由宿主调用 release
///   （重复调用会被 `ReleaseOnce` 吞掉，实现不需要自己调用）；
/// - `spawn` 返回 `Ok` 后，实现必须在 worker 真实退出时调用 `observer.exit` 恰好一次，
///   否则 `on_worker_started` 的 `exited` 永远不会 resolve、release 也不会发生。
pub trait WorkerSpawner: Send + Sync {
    /// 加载（并编译）QuickJS 模块。失败文本进入 `Failed to load QuickJS: ${message}`。
    ///
    /// TS: `options.wasm ?? loadQuickJSWasm()`。
    fn load_wasm(&self) -> BoxFuture<'static, Result<WorkerWasm, String>>;
    /// 构造 worker。失败文本进入 `Failed to start worker: ${message}`。
    ///
    /// `interrupt` 是宿主在终止前设为非零、VM 轮询的中断标志（TS: `SharedArrayBuffer(4)` +
    /// `Atomics`）；`observer` 是 worker 向宿主报告消息/错误/退出的回调。
    fn spawn(
        &self,
        worker_url: Option<&str>,
        start: VmStartData,
        wasm: WorkerWasm,
        interrupt: Arc<AtomicI32>,
        observer: Arc<dyn WorkerObserver>,
    ) -> Result<Arc<dyn WorkerHandle>, String>;
}

/// Node 24/26 的 `--watch` 会把依赖报告泄露到 worker 通道上（nodejs/node#65044）。
///
/// TS: `typeof message === "object" && message !== null && ("watch:import" in message || "watch:require" in message)`。
pub fn is_watch_notification(message: &Value) -> bool {
    // `typeof message === "object" && message !== null`：数组也通过，但只有对象能有这些键。
    let Value::Object(object) = message else {
        return false;
    };
    // `"watch:import" in message`：键存在即命中（`in` 沿原型链查找；JSON 对象没有原型链，
    // 继承来的键不可能叫这两个名字，因此 `contains_key` 与 TS 的 `in` 在这里等价）。
    object.contains_key("watch:import") || object.contains_key("watch:require")
}

/// 默认 worker 入口（编译进本 crate 的线程入口；`worker_url` 见模块文档）。
///
/// TS: `defaultWorkerUrl()`（`.ts` 源码跑 `./worker.ts`，发布版跑 `./worker.js`）。Rust 侧没有
/// 「从 URL 载入模块」的等价物，入口恒为 [`crate::runtime::worker::run_worker`]。本常量当前
/// 只作为公开标识与测试断言存在，不参与 spawn 路径。
pub const DEFAULT_WORKER_ENTRY: &str = "kiana-codemode/worker";

/// `workerLauncher(options)`：一个执行一个 worker 线程。
///
/// 返回的 [`VmLauncher`] 可以被多次调用（每次实际启动的执行调用一次：sandbox 未关闭且
/// signal 未预先取消），每次克隆 `options` 并新建一个 worker。启动失败不会 panic，而是从
/// launcher future 返回 `Err(String)`，随后由 `execution.rs` 记成 `kind: "sandbox"`。文本有
/// 三种来源：wasm 加载失败带 `Failed to load QuickJS:` 前缀；worker 构造失败带
/// `Failed to start worker:` 前缀；`acquire_worker` 准入回调或 `on_worker_started` 回调
/// panic 时是 panic 的原始文本，没有前缀（TS 侧同样是裸 throw 文本）。
///
/// TS: `host.ts#workerLauncher`。
pub fn worker_launcher(options: CodemodeSandboxOptions) -> VmLauncher {
    worker_launcher_with(options, default_spawner())
}

/// [`worker_launcher`] 的可注入版本：测试用它断言 admission / release / terminate / unref。
pub fn worker_launcher_with(
    options: CodemodeSandboxOptions,
    spawner: Arc<dyn WorkerSpawner>,
) -> VmLauncher {
    Arc::new(move |data: VmStartData, events: Arc<dyn VmEvents>| {
        let options = options.clone();
        let spawner = Arc::clone(&spawner);
        Box::pin(async move { launch_worker(options, spawner, data, events).await })
    })
}

/// 让 release 只发生一次（`worker.once("exit", release)` 或构造失败时的一次调用）。
///
/// 内部的 `Arc` 会在 `launch_worker` 与 [`HostObserver`] 之间共享：worker 的 `exit` 与构造
/// 失败这两条路径即使都尝试调用，`take()` 也只会让其中一次拿到回调。
#[derive(Clone, Default)]
struct ReleaseOnce {
    release: Arc<Mutex<Option<crate::types::CodemodeWorkerRelease>>>,
}

impl ReleaseOnce {
    fn new(release: Option<crate::types::CodemodeWorkerRelease>) -> Self {
        ReleaseOnce {
            release: Arc::new(Mutex::new(release)),
        }
    }

    /// 调用并拿走 release（`release` 回调不得抛错；抛错被吞掉，与 TS 的 `?.()` 一致地不传播）。
    fn call(&self) {
        let release = self.release.lock().expect("release mutex").take();
        if let Some(release) = release {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(release));
        }
    }
}

/// worker 事件 → 宿主 [`VmEvents`]，并负责 release-on-exit 与 `exited` 通知。
///
/// 事件映射：`message` 先过滤 Node `--watch` 报告再交给 [`VmEvents::message`]；`error` 直接
/// 转给 [`VmEvents::failure`]；`exit` 的顺序是「release → resolve `exited` → `failure`」，
/// 因此即使执行早已结算，release 与 `exited` 仍会按 worker 的真实退出发生。
struct HostObserver {
    events: Arc<dyn VmEvents>,
    release: ReleaseOnce,
    exited: Mutex<Option<oneshot::Sender<()>>>,
    /// worker 真正退出后置位。
    ///
    /// 本 crate 内没有其它读取点：已决结果的保护由 `execution.rs` 的 `finished` 标志负责，
    /// 本字段只记录状态。
    exited_flag: AtomicBool,
}

impl WorkerObserver for HostObserver {
    fn message(&self, message: Value) {
        // Node `--watch` 的依赖报告（nodejs/node#65044）不交给宿主。
        if is_watch_notification(&message) {
            return;
        }
        self.events.message(message);
    }

    fn error(&self, error: CodemodeError) {
        self.events.failure(error);
    }

    fn exit(&self, code: i32) {
        self.release.call();
        if let Some(sender) = self.exited.lock().expect("exited mutex").take() {
            let _ = sender.send(());
        }
        self.exited_flag.store(true, Ordering::SeqCst);
        self.events.failure(CodemodeError::new(
            CodemodeErrorKind::Sandbox,
            format!("Worker exited with code {code} before the script settled"),
        ));
    }
}

/// 宿主对 worker 的 channel（TS: `{ post, stop }`）。
struct WorkerChannel {
    /// spawner 返回的底层句柄；`post`/`terminate`/`unref` 都转发给它。
    worker: Arc<dyn WorkerHandle>,
    /// 与 worker 共享的中断标志：VM 侧的 `should_interrupt` 在脚本运行时轮询它
    /// （`runtime/worker.rs` 的 `interrupted`）。
    interrupt: Arc<AtomicI32>,
    /// `stop()` 只调用一次 terminate/unref 的幂等开关（`Execution` 只调一次，这里是保险）。
    stopped: AtomicBool,
}

impl VmChannel for WorkerChannel {
    fn post(&self, message: HostToWorkerMessage) {
        self.worker.post(message);
    }

    fn stop(&self) -> BoxFuture<'static, ()> {
        // `stop()` 只会被 `Execution` 调一次；幂等只是保险（测试断言 terminate/unref 各一次）。
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Box::pin(async {});
        }
        // 1. 设置中断标志：Bun 的 `terminate()` 停不住正在 wasm 里自旋的线程。
        self.interrupt.store(1, Ordering::SeqCst);
        // 2. 请求终止；不等待退出（`terminate()` 可能一直挂着）。
        self.worker.terminate();
        // 3. `terminate()` 内部会 ref，这里 unref 掉；不 join。
        self.worker.unref();
        Box::pin(async {})
    }
}

/// 一次执行的启动序列：wasm → 中断标志 → 准入 → 构造 worker → `on_worker_started`。
///
/// 成功返回 VM channel；`Err` 文本由 `Execution` 转成 `kind: "sandbox"`，来源有三种：
/// wasm 加载失败带 `Failed to load QuickJS:` 前缀，worker 构造失败带
/// `Failed to start worker:` 前缀，`acquire_worker` 准入回调或 `on_worker_started` 回调
/// panic 时则是该 panic 的原始文本（无前缀，见 `panic_message`）。
/// 准入配额（`acquire_worker` 返回的 release）只会在两条路径上被调用：worker 构造失败，
/// 或 worker 真实退出。`start_vm` 式的失败不会在这里发生——VM 的创建在 worker 线程内，
/// 以 `crash` 消息回报。
async fn launch_worker(
    options: CodemodeSandboxOptions,
    spawner: Arc<dyn WorkerSpawner>,
    data: VmStartData,
    events: Arc<dyn VmEvents>,
) -> Result<Arc<dyn VmChannel>, String> {
    // 1. wasm：`options.wasm` 优先，否则加载并编译。失败不占配额。
    let module = match options.wasm.clone() {
        Some(module) => Ok(Arc::new(module) as WorkerWasm),
        None => spawner.load_wasm().await,
    };
    let module = match module {
        Ok(module) => module,
        Err(message) => return Err(format!("Failed to load QuickJS: {message}")),
    };

    // 2. 中断标志（TS: `new SharedArrayBuffer(4)`）。
    let interrupt = Arc::new(AtomicI32::new(0));

    // 3. `acquireWorker?.()`：在 worker 构造之前、wasm 加载之后同步调用；抛错 = 拒绝准入。
    let release = match &options.acquire_worker {
        Some(acquire) => {
            let acquire = Arc::clone(acquire);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| acquire())) {
                Ok(release) => Some(release),
                Err(payload) => return Err(panic_message(payload)),
            }
        }
        None => None,
    };
    let release = ReleaseOnce::new(release);

    // 4. 构造 worker：失败 → release 一次，并报 `Failed to start worker:`。
    let (exited_tx, exited_rx) = oneshot::channel();
    let observer: Arc<dyn WorkerObserver> = Arc::new(HostObserver {
        events: Arc::clone(&events),
        release: release.clone(),
        exited: Mutex::new(Some(exited_tx)),
        exited_flag: AtomicBool::new(false),
    });
    let worker = match spawner.spawn(
        options.worker_url.as_deref(),
        data,
        module,
        Arc::clone(&interrupt),
        observer,
    ) {
        Ok(worker) => worker,
        Err(error) => {
            release.call();
            return Err(format!("Failed to start worker: {error}"));
        }
    };

    // 5. `onWorkerStarted(exited)`：`exited` 在 worker 真正退出时 resolve；回调抛错 →
    //    请求终止并把错误抛给执行（`kind: "sandbox"`）。
    if let Some(on_worker_started) = &options.on_worker_started {
        let on_worker_started = Arc::clone(on_worker_started);
        let exited: BoxFuture<'static, ()> = Box::pin(async move {
            let _ = exited_rx.await;
        });
        let channel = Arc::new(WorkerChannel {
            worker: Arc::clone(&worker),
            interrupt: Arc::clone(&interrupt),
            stopped: AtomicBool::new(false),
        });
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_worker_started(exited)));
        if let Err(payload) = outcome {
            // `void stop()`：请求终止，不等退出。
            let stop = channel.stop();
            tokio::spawn(stop);
            return Err(panic_message(payload));
        }
        return Ok(channel);
    }
    // 没有 `on_worker_started` 时丢弃接收端：没人等待 `exited`，但 `HostObserver::exit` 仍会
    // 释放准入配额（release 不依赖这个通道）。
    drop(exited_rx);

    Ok(Arc::new(WorkerChannel {
        worker,
        interrupt,
        stopped: AtomicBool::new(false),
    }))
}

/// panic 载荷 → 文本：TS 的 `throw` 在 Rust 侧用 panic 表达，回调文本要原样进入错误结果。
///
/// `&str` 与 `String` 载荷可以还原成文本；其它载荷（例如 `panic_any` 进来的自定义类型）没有
/// JS 对应物，统一记为 `an unknown error`（`worker.rs` 有同形的辅助函数）。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "an unknown error".to_string()
    }
}

// ---------------------------------------------------------------------------
// 默认 spawner：std::thread + worker 入口
// ---------------------------------------------------------------------------

/// 默认 spawner：每个 worker 一个 `std::thread`，线程入口是
/// [`crate::runtime::worker::run_worker`]。
///
/// `spawn` 先做 `worker_url` 的同步校验（非法形态直接 `Err`）；文件缺失时返回 `MissingWorker`
/// 句柄（异步 `error` + `exit`）；文件存在时仍启动编译进二进制的入口，不读取该文件的内容。
///
/// feature `wasm` 关闭时它也**存在**（测试仍可构造与注入），只是 `load_wasm` 立刻返回
/// [`crate::wasm::load_quickjs_wasm`] 的「后端已禁用」错误，thus 每次执行以
/// `Failed to load QuickJS: …` 的 sandbox 错误结束——与 TS 在加载失败时的可观察行为一致。
pub struct ThreadSpawner;

impl WorkerSpawner for ThreadSpawner {
    fn load_wasm(&self) -> BoxFuture<'static, Result<WorkerWasm, String>> {
        Box::pin(async move {
            match crate::wasm::load_quickjs_wasm(None).await {
                Ok(module) => Ok(Arc::new(module) as WorkerWasm),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn spawn(
        &self,
        worker_url: Option<&str>,
        start: VmStartData,
        wasm: WorkerWasm,
        interrupt: Arc<AtomicI32>,
        observer: Arc<dyn WorkerObserver>,
    ) -> Result<Arc<dyn WorkerHandle>, String> {
        let path = check_worker_url(worker_url)?;
        if let Some(path) = path
            && !path.exists()
        {
            // Node 的 MODULE_NOT_FOUND 是异步错误（见 `check_worker_url` 的文档）。
            let worker = Arc::new(MissingWorker {
                observer: Arc::clone(&observer),
                message: format!("Cannot find module '{}'", resolved_path(&path).display()),
                reported: AtomicBool::new(false),
            });
            let reported = Arc::clone(&worker);
            // 借用调用方的 Tokio 运行时投递；没有运行时就同步报告（测试路径）。
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move { reported.report() });
                }
                Err(_) => worker.report(),
            }
            return Ok(worker);
        }
        crate::runtime::worker::spawn_worker_thread(start, wasm, interrupt, observer)
    }
}

/// 默认 spawner（见 [`ThreadSpawner`]）：[`CodemodeSandbox::new`] 不带注入时用它。
pub fn default_spawner() -> Arc<dyn WorkerSpawner> {
    Arc::new(ThreadSpawner)
}

/// `workerUrl` 的同步校验：逐字复刻 Node 26 `new Worker(specifier)` 的 `ERR_WORKER_PATH` 规则。
///
/// node 26 的 `internal/worker` 源码（`process.binding("natives")["internal/worker"]`）：
///
/// ```text
/// if (isURL(filename)) { url = filename; filename = fileURLToPath(filename); }
/// else if (typeof filename !== 'string') { throw ERR_INVALID_ARG_TYPE(...) }
/// else if (path.isAbsolute(filename) || /^\.\.?[\\/]/.test(filename)) {
///   filename = path.resolve(filename); url = pathToFileURL(filename);
/// } else { throw new ERR_WORKER_PATH(filename); }
/// ```
///
/// 也就是说**字符串**形态只接受绝对路径或 `./`、`../` 开头；其余（空串、裸名字
/// `nope.js`、`file:///x.js`、`https://…`、`data:…`、`node:fs`）都同步抛
/// `ERR_WORKER_PATH`，文本是
/// `The worker script or module filename must be an absolute path or a relative path starting with
/// './' or '../'.` + （`file://` 前缀 → `` Wrap file:// URLs with `new URL`. ``；`data:text/javascript`
/// 前缀 → `` Wrap data: URLs with `new URL`. ``）+ ` Received "<specifier>"`。
/// 冻结的 `CodemodeSandboxOptions::worker_url` 是 `Option<String>`（TS 是 `string | URL`），
/// 因此只有字符串这条路径；`Some("")` 不再静默回落到内置 worker，而是与 Node 一样报错。
///
/// 文件缺失**不是**同步错误：worker 稍后发 `error`（`Cannot find module '<绝对路径>'`，
/// `MODULE_NOT_FOUND`）再 `exit` 1，由 [`ThreadSpawner::spawn`] 用一条异步失败句柄复刻。
///
/// 返回值 `Some(path)` 表示调用方给了 `worker_url`（`None` 表示用内置入口）。**是否合法**的
/// 判定按字节前缀做（`/` 开头，或 `./`、`../` 开头；复刻 Node 的 `/^\.\.?[\\/]/`，因此
/// Windows 分隔符变体 `.\`、`..\` 也被接受），不走 `Path::is_absolute`；测试
/// `reports_a_missing_worker_file_as_a_sandbox_error` 锁定了这组输入（含被拒的 `C:/x.js`）。
fn check_worker_url(worker_url: Option<&str>) -> Result<Option<std::path::PathBuf>, String> {
    let Some(url) = worker_url else {
        return Ok(None);
    };
    if url.starts_with('/') {
        return Ok(Some(absolute(std::path::Path::new(url))));
    }
    // `RegExpPrototypeExec(/^\.\.?[\\/]/, filename)`。
    let bytes = url.as_bytes();
    let relative = match bytes.first() {
        Some(b'.') => match bytes.get(1) {
            Some(b'/') | Some(b'\\') => true,
            // `./` 与 `../`
            Some(b'.') => matches!(bytes.get(2), Some(b'/') | Some(b'\\')),
            _ => false,
        },
        _ => false,
    };
    if relative {
        return Ok(Some(absolute(std::path::Path::new(url))));
    }
    Err(node_worker_path_error(url))
}

/// `ERR_WORKER_PATH(specifier)` 的逐字文本（node 26 的 `internal/errors`）。
fn node_worker_path_error(specifier: &str) -> String {
    let mut message = String::from(
        "The worker script or module filename must be an absolute path or a relative path starting with './' or '../'.",
    );
    if specifier.starts_with("file://") {
        message.push_str(" Wrap file:// URLs with `new URL`.");
    }
    if specifier.starts_with("data:text/javascript") {
        message.push_str(" Wrap data: URLs with `new URL`.");
    }
    message.push_str(&format!(" Received \"{specifier}\""));
    message
}

/// `path.resolve(path)` 的第一步：相对路径按 cwd 展开。
///
/// 这里只做 `cwd.join(path)`，不消解 `.`/`..`（消解在 `resolved_path` 里做）。Node 的
/// `Cannot find module` 报文使用解析后的绝对路径，`./x.js` 因此在这里按 cwd 展开成绝对路径；
/// 裸名字 `x.js` 既非绝对也不以 `./`/`../` 开头，在 `check_worker_url` 就被拒绝，不会走到
/// 这里。`current_dir()` 失败时退回未展开的路径（测试环境正常时不会走这条分支）。
fn absolute(path: &std::path::Path) -> std::path::PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// Node 的 `require.resolve` 报文用的路径：绝对化后再消掉 `.`/`..`（不解析符号链接）。
fn resolved_path(path: &std::path::Path) -> std::path::PathBuf {
    let absolute = absolute(path);
    let mut out = std::path::PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 入口文件缺失时的句柄：安静地 post 失败、terminate/unref 都是 no-op。
///
/// `ThreadSpawner::spawn` 在存在性检查失败时返回它而不是 `Err`，因为 Node 的
/// `new Worker('./missing.js')` 是**先成功构造、再异步发 `error`/`exit`**（同步抛出的只有
/// 非法路径形态）。没有真实 VM，所以宿主后续的 `post` 只能丢弃；执行会在
/// [`MissingWorker::report`] 的失败事件里结算。
struct MissingWorker {
    observer: Arc<dyn WorkerObserver>,
    message: String,
    reported: std::sync::atomic::AtomicBool,
}

impl MissingWorker {
    /// 复刻 Node 的事件顺序：`error` 之后 `exit` 1（两者都在 worker 线程启动后异步发生）。
    ///
    /// Node 的 `error` 事件携带一个普通 `Error`（`name === "Error"`），`host.ts` 的
    /// `worker.on("error", …)` 因此报出 `{ kind: "sandbox", name: "Error", message }`；
    /// 冻结的 [`CodemodeError`] 有 `name` 字段，缺了它就与 TS 的结果 JSON 不同。
    fn report(&self) {
        if self.reported.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut error = CodemodeError::new(CodemodeErrorKind::Sandbox, self.message.clone());
        error.name = Some("Error".to_string());
        self.observer.error(error);
        self.observer.exit(1);
    }
}

impl WorkerHandle for MissingWorker {
    fn post(&self, _message: HostToWorkerMessage) {}
    fn terminate(&self) {}
    fn unref(&self) {}
}

// ---------------------------------------------------------------------------
// CodemodeSandbox
// ---------------------------------------------------------------------------

/// 在 worker 线程里的 QuickJS VM 中运行 JavaScript（独立的 wasm 实例）。
///
/// worker 让脚本执行离开宿主线程：QuickJS 同步运行，自旋脚本在宿主线程上会卡住事件循环。
/// 每次实际启动的 `execute()`/`start()` 新建一个 worker（sandbox 已关闭或 signal 已预先取消
/// 时不启动；见模块文档的生命周期）；worker 线程不 join，所以 `close()` 返回时线程可能仍在
/// 收尾。
///
/// TS: `host.ts#CodemodeSandbox extends CodemodeSandboxBase`。
pub struct CodemodeSandbox {
    base: Arc<CodemodeSandboxBase>,
}

impl CodemodeSandbox {
    /// 构造：用 [`worker_launcher`] 起 VM。
    ///
    /// TS: `new CodemodeSandbox(options = {})`。Rust 侧构造可能因非法全局名失败（TS 里是
    /// 构造函数抛错），因此返回 `Result`。构造本身不启动任何 worker；worker 在每次实际启动的
    /// `execute()`/`start()` 时创建（sandbox 已关闭或 signal 已预先取消时不创建）。
    pub fn new(options: CodemodeSandboxOptions) -> Result<Self, StartError> {
        Self::with_spawner(options, default_spawner())
    }

    /// 用注入的 spawner 构造（测试用；与 [`Self::new`] 走同一条启动逻辑）。
    pub fn with_spawner(
        options: CodemodeSandboxOptions,
        spawner: Arc<dyn WorkerSpawner>,
    ) -> Result<Self, StartError> {
        let base_options = options.base.clone();
        let launch = worker_launcher_with(options, spawner);
        let base = CodemodeSandboxBase::new(base_options, launch)?;
        Ok(CodemodeSandbox {
            base: Arc::new(base),
        })
    }

    /// 基类（工具表、`execute`/`start`/`close`）。
    pub fn base(&self) -> &Arc<CodemodeSandboxBase> {
        &self.base
    }

    /// TS: `CodemodeSandboxBase.execute`。
    ///
    /// 每次实际启动的 `execute()`/`start()` 新建一个 worker；sandbox 已关闭或 signal 已预先
    /// 取消时不会启动 worker（前者以 `SandboxClosed` 失败，后者直接以 `aborted` 结算）。
    /// 脚本自身的失败以 `CodemodeResult::Err` 返回，不通过 `Result` 传播。
    pub fn execute(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
    ) -> BoxFuture<'static, Result<CodemodeResult, crate::runtime::execution::SandboxClosed>> {
        self.base.execute(code, options)
    }

    /// TS: `CodemodeSandboxBase.start`。
    ///
    /// 参数校验在启动 worker 之前完成（`started_at`、`seed`、工具/脚本名唯一、全局名），失败
    /// 返回 [`StartError`]；默认 deadline 是无穷，与 `execute()` 的 300000 ms 不同。
    pub fn start(
        &self,
        code: &str,
        options: CodemodeRunOptions,
    ) -> Result<CodemodeRun, StartError> {
        self.base.start(code, options)
    }

    /// TS: `CodemodeSandboxBase.close`。
    ///
    /// 取消在途执行（它们以 `kind: "aborted"` 结算）并拒绝之后的新执行；等在途结果结算后返回。
    /// 可以重复调用（第二次没有在途执行可取消）。
    pub async fn close(&self) {
        self.base.close().await;
    }

    /// TS: `CodemodeSandboxBase.registerTool`。
    ///
    /// 重名返回 `Tool "<name>" is already registered`；只影响之后调用的 `execute()`（每次
    /// `execute()` 取工具表快照）。`start()` 使用自己声明的 `tools`，不读 sandbox 的工具表，
    /// 因此不受这里影响。
    pub fn register_tool(&self, tool: CodemodeTool) -> Result<(), String> {
        self.base.register_tool(tool)
    }

    /// TS: `CodemodeSandboxBase.unregisterTool`。
    ///
    /// 返回该工具此前是否存在；不影响已在途的执行。
    pub fn unregister_tool(&self, name: &str) -> bool {
        self.base.unregister_tool(name)
    }

    /// TS: `get tools()`：注册顺序的快照。
    pub fn tools(&self) -> Vec<CodemodeTool> {
        self.base.tools()
    }

    /// TS: `get globals()`：注册顺序的快照。
    pub fn globals(&self) -> Vec<CodemodeTool> {
        self.base.globals()
    }
}

impl std::fmt::Debug for CodemodeSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodemodeSandbox")
    }
}
