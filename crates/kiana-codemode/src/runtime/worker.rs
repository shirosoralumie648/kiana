//! runtime/worker.ts：worker 线程入口。
//!
//! TS 源：`packages/codemode/src/runtime/worker.ts`（39 行）。
//!
//! 一个 worker 在一个全新的 QuickJS VM（独立的 wasm 实例）里跑一个脚本，把工具调用与输出转给
//! 宿主，并报告结果。脚本结算、超时或被取消后由宿主终止 worker；worker 存在的意义是让自旋
//! 脚本永远不阻塞宿主线程。
//!
//! TS 的模块顶层就是入口（`if (parentPort) main(workerData).catch(…)`）：导入即启动。Rust 没有
//! 可执行模块顶层的等价物，因此入口是显式的 [`run_worker`]；[`spawn_worker_thread`] 在一个
//! `std::thread` 上跑它：`startVm` 的 rejection 变成 `crash`（对应 TS 顶层 `.catch`），
//! 而 panic 由入口的 `catch_unwind` 变成 `error` 事件后照常 `exit`。
//!
//! # 顺序（固定契约，逐条）
//!
//! 1. `interrupt = new Int32Array(data.interrupt)`：中断标志与宿主共享（[`interrupted`]）；
//! 2. `session = await startVm({data, wasm: data.wasm, post, shouldInterrupt})`——VM 活到宿主
//!    terminate，因此**永不 dispose**；
//! 3. **之后**才安装宿主消息监听（`parentPort.on("message", …)`）：启动期间到达的 result 会在
//!    端口上排队（[`tokio::sync::mpsc`] 的缓冲区），等监听器装上后再交付；
//! 4. `startVm` 失败（VM 建不起来）→ `crash` 消息，文本是 `${error.name}: ${error.message}`
//!    （非 `Error` 时是 `String(error)`，对应 [`StartVmError`] 的 `Display`）。
//!
//! # 后端接线
//!
//! `startVm` 属于 `runtime/vm.rs`（另一个单元）。worker 线程按以下顺序取实现：
//!
//! 1. 显式参数（[`spawn_worker_thread_with`] 注入，测试用）；
//! 2. 进程级注册表（[`set_start_vm`]）；
//! 3. feature `wasm` 打开时，内置适配器直接调用 `runtime::vm::start_vm`：把 [`WorkerStart`]
//!    映射成 `StartVmOptions`（`WorkerWasm` downcast 回 `CodemodeWasmModule`，`post` /
//!    `should_interrupt` 原样传递），并用 `Mutex` 把 vm 单元 `&mut self` 的会话包成这里的
//!    `&self` 接缝。
//!
//! 都没有时 post 一条明确的 `crash`（内部接线错误）。默认构建（`default = ["wasm"]`）里
//! `default_start_vm` 恒返回 `Some`，所以正常宿主路径走不到；关掉 `wasm` 时宿主会更早地在
//! `load_quickjs_wasm` 失败。只有绕过宿主的 `load_wasm`、直接调用 [`run_worker`] 或
//! [`spawn_worker_thread`] / [`spawn_worker_thread_with`] 且未注入、未注册后端时才可能看到它。
//!
//! # 执行模型：worker 线程与 VM 线程
//!
//! 本文件只负责 worker 这一层。默认后端（`runtime/vm.rs` 的 `start_vm`）还会为每个 VM 再起
//! 一条专用线程，[`VmSession::deliver`] 通过通道把命令交给它并同步等待；因此一次默认后端的
//! worker 执行至少涉及两条线程：
//!
//! - **worker 线程**（[`spawn_worker_thread`] 创建）：建一个 current-thread Tokio 运行时，
//!   在 [`run_worker`] 里按到达顺序把宿主消息交给会话。默认后端下它不执行 JavaScript。
//! - **VM 线程**（`runtime/vm.rs` 内部）：真正跑 wasm/QuickJS；`WorkerStart::post` 回调也在
//!   这里同步触发。
//!
//! 由此有两个约束：
//!
//! - [`VmSession::deliver`] 是同步的，脚本可以运行任意久才交还控制权，所以 worker 线程一次
//!   只处理一条宿主消息，后续消息在 [`tokio::sync::mpsc`] 的无界队列里排队；
//! - [`spawn_worker_thread_with`] 返回时 VM 可能还没建好，但消息队列已经存在，宿主可以立刻
//!   `post`，启动期间到达的结果不会丢。
//!
//! # 生命周期与失败出口
//!
//! 1. 三个后端来源都没有 → 一条 `crash`，[`run_worker`] 返回；
//! 2. `start_vm` 返回 `Err(StartVmError)` → 一条 `crash`（文本为 `name: message`），
//!    [`run_worker`] 返回；
//! 3. 正常路径：`inbound` 的所有发送端被丢弃（[`WorkerHandle::terminate`] 取走唯一的生产者）
//!    → `recv()` 得到 `None` → [`run_worker`] 返回，`dispose()` 从不显式调用；
//! 4. 消息循环内 panic（注入的 `start_vm` 或 `deliver`）→ 线程入口的 `catch_unwind` 先上报
//!    `WorkerObserver::error`，随后照常上报 `exit`，这样 `runtime/host.rs` 的记账
//!    （release、`exited`）不会丢；
//! 5. 线程入口最后**总是**上报 `exit`：`terminate()` 被调用过是 `1`，否则 `0`。它只表示这条
//!    `std::thread` 结束：会话 Drop 只向 VM 线程发 `Stop`、不 join，VM 线程可能尚未处理它；
//!    但 [`VmSession::deliver`] 是同步的，`run_worker` 只有在 `deliver` 返回之后才会走到
//!    `recv()` 的 `None` 出口，所以上报 `exit` 时脚本已经停下（不再执行 JavaScript）。
//!
//! # 协议边界
//!
//! 宿主 → worker：[`WorkerHandle::post`] 把 [`HostToWorkerMessage`] 序列化成
//! [`serde_json::Value`]（模拟 Node `postMessage` 的结构化克隆），[`run_worker`] 先用
//! [`is_host_message`] 只按 `type` 过滤，再完整反序列化成 [`HostToWorkerMessage`]。守卫放行但
//! 字段不合协议的值会让反序列化失败：这时发一条 `crash` 并继续循环（TS 不做第二步解析；真实
//! 宿主不会发出这种值）。
//!
//! worker → 宿主：不经过 [`WorkerHandle`]；`WorkerStart::post` 回调直接调用
//! `WorkerObserver::message`，由 `runtime/host.rs` 的观察者过滤 Node `--watch` 通知后交给执行层。
//!
//! # 改动影响
//!
//! `runtime/host.rs` 依赖 `exit` 事件做 release 与 `exited` 结算；`tests/worker_lifetime.rs`
//! 钉住了 panic → error + exit、`terminate`/`unref` 顺序以及默认后端的端到端结果。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use tokio::sync::mpsc;

use crate::runtime::host::{WorkerHandle, WorkerObserver, WorkerWasm};
use crate::runtime::protocol::{
    HostToWorkerMessage, VmStartData, WorkerToHostMessage, is_host_to_worker_message,
};
use crate::types::{BoxFuture, CodemodeError, CodemodeErrorKind};

/// worker 向宿主发出的消息回调（TS: `post(message)` = `parentPort?.postMessage(message)`）。
///
/// 与 `vm.rs` 的 `StartVmOptions::post` 同型（`WorkerToHostMessage` 即 TS 的
/// `WorkerToHostMessage` 联合），因此默认后端可以直接把它交给 `start_vm`；线程出口处再序列化
/// 成 JSON 值交给宿主（`--watch` 过滤要看任意形状的端口消息，见 [`crate::runtime::host`]）。
///
/// 回调可能在任意线程被调用：默认后端在 VM 线程的 VM 回调里**同步**调用它，回调返回前 QuickJS
/// 不会继续前进。实现因此必须快速返回。本文件里，序列化发生在
/// [`spawn_worker_thread_with`] 构造的 `post` 闭包中，不在 VM 内部。
pub type WorkerPost = Arc<dyn Fn(WorkerToHostMessage) + Send + Sync>;

/// `startVm` 的失败（TS 的 reject 值在 worker 入口里被写成 `${error.name}: ${error.message}`）。
///
/// 它与 `runtime/vm.rs` 的 [`crate::runtime::vm::StartVmError`] 是**两个不同类型**：worker 接缝
/// （含测试注入的实现）只需要 `name` + `message` 就能拼出 `crash` 文本。默认适配器从 vm 错误
/// 构造本类型时只取 `message()`，名字因此固定为 `"Error"`（见 `default_start_vm` 的注释）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartVmError {
    /// `error.name`（`JSException` 之类；普通错误是 `"Error"`）。
    pub name: String,
    /// `error.message`。
    pub message: String,
}

impl StartVmError {
    /// 普通 `Error`：`name` 固定为 `"Error"`，`message` 原样保留。
    pub fn new(message: impl Into<String>) -> Self {
        StartVmError {
            name: "Error".to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StartVmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

impl std::error::Error for StartVmError {}

/// VM 会话（TS: `vm.ts#VmSession`）。
///
/// `deliver(message)` 交一条工具结果给 VM 并跑到它再次等待；`dispose()` 释放 wasm 实例。
/// worker 这里**永不** dispose（VM 活到宿主 terminate）。
///
/// # 调用约束
///
/// - 两个方法都是同步调用：`deliver` 要等 VM 跑完当前这一轮（脚本可能继续执行任意久，直到
///   它再次等待或结束）才返回，所以 [`run_worker`] 的消息循环天然是串行的；
/// - `Send + Sync` 只表示可以跨线程共享，不表示可以并发进入同一个 VM。默认后端用 `Mutex`
///   包住 `runtime/vm.rs` 的会话（它需要 `&mut self`）来适配这里的 `&self` 接缝，锁的范围
///   覆盖整个 `deliver`；
/// - `dispose` 之后不得再使用会话；默认后端的 `dispose` 只执行一次。
pub trait VmSession: Send + Sync {
    /// TS: `session.deliver(message)`。
    fn deliver(&self, message: HostToWorkerMessage);
    /// TS: `session.dispose()`。
    fn dispose(&self);
}

/// `startVm(options)`（TS: `vm.ts#startVm`），由 vm 单元实现并注册。
///
/// 参数是 [`WorkerStart`]，成功时返回可共享的 [`VmSession`]；失败时返回的 [`StartVmError`]
/// 会被 [`run_worker`] 用其 `Display` 拼成 `crash` 的 `message`。三种来源的优先级见模块文档的
/// “后端接线”。
pub type StartVm = Arc<
    dyn Fn(WorkerStart) -> BoxFuture<'static, Result<Arc<dyn VmSession>, StartVmError>>
        + Send
        + Sync,
>;

/// `startVm` 的参数（TS: `vm.ts#StartVmOptions`）。
///
/// # 所有权与默认值
///
/// 整个结构按值交给 `start_vm`；默认适配器把它逐字段搬进 `runtime/vm.rs` 的
/// `StartVmOptions`。[`spawn_worker_thread_with`] 固定把 `max_stack_size` 与
/// `interrupt_budget` 留空：TS 的 worker 入口本来也不传这两个选项（spec §2.2.13）；`None`
/// 时 `runtime/vm.rs` 用默认栈上限，中断预算字段则只影响确定性指纹（见字段注释）。
pub struct WorkerStart {
    /// 启动数据。
    pub data: VmStartData,
    /// 编译好的 QuickJS 模块。
    pub wasm: WorkerWasm,
    /// QuickJS 自己的栈护栏（深度递归变成可 catch 的 `RangeError`）；`None` 用引擎默认。
    pub max_stack_size: Option<usize>,
    /// 中断预算（只进入确定性指纹；worker 后端不设预算）。
    pub interrupt_budget: Option<u32>,
    /// VM → 宿主。
    pub post: WorkerPost,
    /// 中断轮询（TS: `() => Atomics.load(interrupt, 0) !== 0`）。
    pub should_interrupt: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl std::fmt::Debug for WorkerStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerStart")
            .field("data", &self.data)
            .field("max_stack_size", &self.max_stack_size)
            .field("interrupt_budget", &self.interrupt_budget)
            .finish_non_exhaustive()
    }
}

static START_VM: OnceLock<StartVm> = OnceLock::new();

/// 可选地覆盖 VM 后端的 `startVm`（优先于 feature `wasm` 的内置适配器；进程级、只生效一次）。
///
/// 生产路径不需要调用它：默认适配器已经接上 `runtime/vm.rs` 的 `start_vm`。保留注册表是为
/// 了替换实现/测试注入。
///
/// 只保留第一次注册；后续调用返回 `false`。
pub fn set_start_vm(start_vm: StartVm) -> bool {
    START_VM.set(start_vm).is_ok()
}

/// 当前注册的 `startVm`（未注册时返回 `None`）。
pub fn registered_start_vm() -> Option<StartVm> {
    START_VM.get().cloned()
}

impl WorkerStart {
    /// 取回具体类型的 wasm 模块（`WorkerWasm` 是 [`crate::runtime::host::WorkerSpawner`] 接缝上的
    /// 类型擦除句柄；生产路径上它恒为 `CodemodeWasmModule`）。
    ///
    /// # 合并说明
    ///
    /// `vm.rs` 的 `start_vm` 需要 `CodemodeWasmModule`；这里做一次 downcast，失败只可能发生在
    /// 注入假 spawner 的测试里。
    pub fn wasm_module(&self) -> Result<crate::wasm::CodemodeWasmModule, StartVmError> {
        self.wasm
            .clone()
            .downcast::<crate::wasm::CodemodeWasmModule>()
            .map(|module| (*module).clone())
            .map_err(|_| {
                StartVmError::new("the worker was started with an incompatible wasm handle")
            })
    }
}

/// 中断标志的读取（TS: `Atomics.load(interrupt, 0) !== 0`）。
///
/// 宿主在终止前把标志写成 1（`runtime/host.rs` 的 `WorkerChannel::stop`，同样用 `SeqCst`），
/// VM 通过 [`WorkerStart::should_interrupt`] 反复调用这里；任何非零值都算中断，但宿主只会写 1。
pub fn interrupted(interrupt: &AtomicI32) -> bool {
    interrupt.load(Ordering::SeqCst) != 0
}

/// `isHostToWorkerMessage(message)`：只认 `type === "result"`。
///
/// TS: `portable.ts` 导出的守卫（worker 只用它过滤宿主消息）。与 [`is_host_to_worker_message`]
/// 是同一个判定：`type` 是唯一校验，其余字段原样交给 [`run_worker`] 的 serde 反序列化。
pub fn is_host_message(message: &Value) -> bool {
    is_host_to_worker_message(message)
}

/// `main(data)`：起 VM，然后安装宿主消息监听。
///
/// TS: `worker.ts#main`。`inbound` 是 `parentPort` 的消息端口；它在 VM 建好之前就在缓冲，
/// 因此启动期间到达的结果不会丢。
///
/// # 参数与返回
///
/// - `start`：本次 worker 的启动数据与回调，按值交给解析出来的 `start_vm`；
/// - `inbound`：宿主 → worker 的消息队列，由 [`spawn_worker_thread_with`] 创建，
///   `terminate()` 时发送端被取走并丢弃；
/// - `start_vm`：显式注入的后端（测试用）；`None` 时依次回退到进程级注册表与内置适配器。
///
/// 函数没有返回值：启动失败与协议解析失败都变成一条 `crash`，循环内的 panic 由线程入口的
/// `catch_unwind` 接住。正常返回只发生在 `inbound` 关闭之后；返回时会话随 `Arc` 释放，默认
/// 后端的会话 Drop 只通知 VM 线程 Stop、不 join（`runtime/vm.rs` 的 `Handle::drop`）。
pub async fn run_worker(
    start: WorkerStart,
    mut inbound: mpsc::UnboundedReceiver<Value>,
    start_vm: Option<StartVm>,
) {
    // 先克隆 post：`start` 之后按值交给 start_vm，但启动失败时还要用它发 `crash`。
    let post = Arc::clone(&start.post);
    let crash = |message: String| {
        post(WorkerToHostMessage::Crash { message });
    };
    // 后端只在入口解析一次：显式注入 > 进程级注册表 > feature 内置适配器。
    let Some(start_vm) = start_vm
        .or_else(registered_start_vm)
        .or_else(default_start_vm)
    else {
        // 连 VM 都不存在，只能把接线错误当 `crash` 发出去；线程入口随后仍会发 `exit`。
        crash(
            "Error: the QuickJS VM backend is not registered (call kiana_codemode::runtime::worker::set_start_vm)"
                .to_string(),
        );
        return;
    };
    // 1–2. 起 VM（`interrupt` 由 `WorkerStart::should_interrupt` 闭包持有）。
    let session = match start_vm(start).await {
        Ok(session) => session,
        Err(error) => {
            crash(error.to_string());
            return;
        }
    };
    // 3. VM 存在之后才安装监听；`session` 永不 dispose。
    while let Some(message) = inbound.recv().await {
        if !is_host_message(&message) {
            // 与 TS 的 `isHostToWorkerMessage` 一致：只认 `type: "result"`，其余端口消息
            // 静默丢弃（守卫不校验其它字段，坏字段留给下一步的反序列化报错）。
            continue;
        }
        match serde_json::from_value::<HostToWorkerMessage>(message) {
            // `deliver` 同步跑到 VM 再次等待才返回，因此这里天然一次只投递一条。
            Ok(message) => session.deliver(message),
            // 解析失败说明宿主发来了不符合冻结协议的 JSON：上报 `crash` 后循环继续接收
            // （执行层收到 crash 后已结算，后续消息通常不会再被处理）。
            Err(error) => crash(format!("Error: {error}")),
        }
    }
    // 发送端已全部丢弃（`terminate()` 取走了唯一的生产者）：默认后端的会话在这里随 `Arc`
    // 释放，其 Drop 只发 Stop、不 join，所以不会等 VM 线程处理完 Stop。循环能走到这里，
    // 说明 worker 线程当前不在 `deliver` 里，脚本此刻不会继续执行。
}

/// 未注入、未注册时的默认后端：feature `wasm` 打开时直接接 `runtime/vm.rs` 的 `start_vm`。
///
/// `worker.ts` 的调用形态是 `startVm({ data, wasm: data.wasm, post, shouldInterrupt })`
/// （spec §2.2.13）：TS 的 worker 入口不传 `maxStackSize` / `interruptBudget`，Rust 侧对应的
/// [`spawn_worker_thread_with`] 也把这两个字段恒设为 `None`；本函数只是把 [`WorkerStart`] 里
/// 的值原样搬进 `StartVmOptions`。`WorkerWasm` 是宿主接缝上的类型擦除句柄，这里 downcast 回
/// `CodemodeWasmModule`。
///
/// 只在 feature `wasm` 打开时编译（`default = ["wasm"]`，所以默认构建走这里）。`runtime/vm.rs`
/// 的 `start_vm` 是真实实现；`tests/worker_lifetime.rs` 的
/// `the_thread_spawner_wires_the_default_worker_backend` 在 feature 打开时用真 VM 跑
/// `return 1` 并断言结果就是 `1`。
#[cfg(feature = "wasm")]
fn default_start_vm() -> Option<StartVm> {
    let start_vm: StartVm = Arc::new(|start: WorkerStart| {
        Box::pin(async move {
            // 注意：这里只搬运 vm 错误的 `message()`。`runtime/vm.rs` 的 `StartVmError` 带名字
            // （`maxStackSize` 超限是 `RangeError`），重建成本文件的 `StartVmError` 后名字固定为
            // `"Error"`，crash 文本因此是 `Error: …`。生产路径 `spawn_worker_thread` 固定传
            // `max_stack_size: None`，不会触发那条校验；只有直接构造 `WorkerStart` 调
            // `run_worker` 时才可能看到这个差异。
            let wasm = start.wasm_module()?;
            let session = crate::runtime::vm::start_vm(crate::runtime::vm::StartVmOptions {
                data: start.data,
                wasm,
                max_stack_size: start.max_stack_size,
                interrupt_budget: start.interrupt_budget,
                post: start.post,
                should_interrupt: start.should_interrupt,
            })
            .await
            .map_err(|error| StartVmError::new(error.message()))?;
            Ok(Arc::new(Mutex::new(session)) as Arc<dyn VmSession>)
        })
    });
    Some(start_vm)
}

/// `runtime/vm.rs` 的 `VmSession` 用 `&mut self`（内部要可变借用 wasm store），而这里的接缝是
/// `&self`（会话随 `Arc` 共享、永不 dispose）；用 `Mutex` 桥接。
///
/// 锁的持有范围覆盖整个 `deliver`（脚本可以在里面跑任意久），所以同一个会话上的并发投递会被
/// 串行化。`expect("vm session mutex")` 在锁中毒时 panic；默认后端里这把锁只被 [`run_worker`]
/// 的消息循环使用，此前的持锁 panic 已经终止了那个循环。
#[cfg(feature = "wasm")]
impl VmSession for Mutex<crate::runtime::vm::VmSession> {
    fn deliver(&self, message: HostToWorkerMessage) {
        self.lock().expect("vm session mutex").deliver(message);
    }

    fn dispose(&self) {
        self.lock().expect("vm session mutex").dispose();
    }
}

/// feature `wasm` 关闭时没有可用的后端；worker 仍能启动并报出明确的 `crash`。
#[cfg(not(feature = "wasm"))]
fn default_start_vm() -> Option<StartVm> {
    None
}

// ---------------------------------------------------------------------------
// 真实 worker：std::thread + 当前线程 Tokio 运行时
// ---------------------------------------------------------------------------

/// `std::thread` 上的 worker 句柄。
///
/// 它只做三件事：把宿主的 [`HostToWorkerMessage`] 序列化后塞进消息队列
/// （[`WorkerHandle::post`]）、在终止时关掉队列并记录标志（[`WorkerHandle::terminate`]）、
/// 记录 `unref()` 是否被调用。线程真正的退出通知由线程入口通过 `WorkerObserver` 发出，不经过
/// 这个结构。
struct ThreadWorker {
    /// 宿主 → worker 的消息端口；`terminate()` 时丢弃（线程的 `recv()` 随即结束）。
    ///
    /// 唯一的生产者；`post` 发送前检查 `terminated`（在锁外先读）。
    inbound: Mutex<Option<mpsc::UnboundedSender<Value>>>,
    /// `terminate()` 是否被调用过：`post` 用它拒写，线程出口用它决定 `exit` 的退出码（1）。
    terminated: AtomicBool,
    /// `unref()` 是否被调用过。本文件的其它逻辑不读它；真正的「不等待线程」来自
    /// `spawn_worker_thread_with` 丢弃 `JoinHandle`（`std::thread` 的 detach）。
    unreferenced: AtomicBool,
}

impl WorkerHandle for ThreadWorker {
    fn post(&self, message: HostToWorkerMessage) {
        if self.terminated.load(Ordering::SeqCst) {
            // TS: `worker.postMessage` 在终止之后仍会被调用（`Execution` 已决时不会再发）；
            // 这里静默丢弃，避免向已关闭的端口写入。
            return;
        }
        // 端口消息在 TS 里经过结构化克隆；这里用 JSON 值模拟。协议消息的字段只有字符串、
        // 整数/浮点数（`HostResult::now` 是 `Option<f64>`，NaN/Infinity 写成 null）、布尔、
        // `Option` 与枚举；当前 `HostToWorkerMessage` 的所有字段都能被 `to_value` 表示，
        // 失败分支不可达，`expect` 只是防御性断言。
        let value = serde_json::to_value(message).expect("HostToWorkerMessage serializes");
        if let Some(sender) = self.inbound.lock().expect("inbound mutex").as_ref() {
            // 锁守卫覆盖整个 `if let`：`as_ref()` 与 `send()` 在同一持锁区间内，并发的
            // `terminate()` 不可能插在两者之间。`terminated` 是在锁外先读的；若随后
            // `terminate()` 抢先取走 sender，这里直接得到 `None`，消息被静默丢弃。
            // `send` 返回 `Err` 只说明接收端（worker 线程的 receiver）已消失；`let _ =`
            // 只吞掉这一处发送错误——`post` 里还有序列化与 `inbound` 锁两处 `expect`
            // （按上面的论证当前不可达）。
            let _ = sender.send(value);
        }
    }

    fn terminate(&self) {
        // Node 的 `terminate()` 停止线程；`std::thread` 无法强杀，所以：丢弃端口（线程的循环
        // 在下一次 `recv()` 时结束）并记为已终止。真正让 VM 停下来的，是 channel 先设置的中断
        // 标志（`WorkerChannel::stop` 的第 1 步）。重复调用是幂等的：`take()` 第二次得到
        // `None`，只是再写一次标志。
        //
        // 如果 worker 线程正卡在 `session.deliver` 里，取走发送端不会打断它：`deliver` 是同步
        // 的，要等它返回，循环才会走到下一次 `recv()`；VM 不轮询中断标志时线程会一直活着。
        self.terminated.store(true, Ordering::SeqCst);
        let _ = self.inbound.lock().expect("inbound mutex").take();
    }

    fn unref(&self) {
        // `unref()`：宿主进程不再等待该线程；Rust 的线程本来就是 detach 的（不 join）。
        // 这个标志在本文件里没有读者，保留它是为了对应 TS 的 `worker.unref()` 语义。
        self.unreferenced.store(true, Ordering::SeqCst);
    }
}

/// 在独立线程上跑 [`run_worker`]，返回一个 [`WorkerHandle`]。
///
/// TS: `new Worker(worker_url, { workerData })`。`std::thread` 无法强杀，因此 `terminate()` 只
/// 请求退出；线程可能比结果活得更久（spec §A.6 第 9 条）。
///
/// 返回时线程已经创建，但 VM 可能尚未就绪：宿主可以立刻 [`WorkerHandle::post`]，消息在无界
/// 队列里等到 [`run_worker`] 进入接收循环。线程创建失败（例如 OS 资源不足）时返回 `Err`，
/// 错误文本由 `runtime/host.rs` 包装成 `Failed to start worker: …`。生产调用方是
/// `runtime/host.rs` 的 `ThreadSpawner::spawn`。
pub fn spawn_worker_thread(
    data: VmStartData,
    wasm: WorkerWasm,
    interrupt: Arc<AtomicI32>,
    observer: Arc<dyn WorkerObserver>,
) -> Result<Arc<dyn WorkerHandle>, String> {
    spawn_worker_thread_with(None, data, wasm, interrupt, observer)
}

/// [`spawn_worker_thread`] 的可注入 `startVm` 版本。
///
/// `start_vm` 为 [`None`] 时线程入口取进程级注册表（[`registered_start_vm`]），与生产路径
/// 完全一致。测试用它注入会 panic / 会卡住的实现，而不污染 [`set_start_vm`] 的全局单例
/// （那里只保留第一次注册，且 `cargo test` 的多个测试共享一个进程）。
///
/// # 线程与退出码
///
/// 线程名固定为 `kiana-codemode-worker`，线程内建一个 current-thread Tokio 运行时再
/// `block_on(run_worker)`。整个入口包在 `catch_unwind` 里：panic 先按 `kind: "sandbox"`、
/// `name: "Error"`（panic 载荷没有名字）上报 `WorkerObserver::error`，随后照常上报 `exit`——
/// `terminate()` 被调用过时退出码是 `1`，否则 `0`。`exit` 只代表这条 `std::thread` 结束了：
/// 会话 Drop 只向 VM 线程发 `Stop`、不 join，VM 线程可能尚未处理完它，但脚本此时已经停下。
pub fn spawn_worker_thread_with(
    start_vm: Option<StartVm>,
    data: VmStartData,
    wasm: WorkerWasm,
    interrupt: Arc<AtomicI32>,
    observer: Arc<dyn WorkerObserver>,
) -> Result<Arc<dyn WorkerHandle>, String> {
    // 无界通道扮演 Node 的 `parentPort`：`post` 可能在任意线程调用，`send` 不阻塞；
    // worker 线程在 `run_worker` 里逐条消费。
    let (sender, receiver) = mpsc::unbounded_channel();
    let worker = Arc::new(ThreadWorker {
        inbound: Mutex::new(Some(sender)),
        terminated: AtomicBool::new(false),
        unreferenced: AtomicBool::new(false),
    });

    let post_observer = Arc::clone(&observer);
    let interrupt_for_vm = Arc::clone(&interrupt);
    // 线程出口：`postMessage` 的结构化克隆在 Rust 侧就是序列化成 JSON 值。
    let post: WorkerPost = Arc::new(move |message: WorkerToHostMessage| {
        post_observer
            .message(serde_json::to_value(message).expect("WorkerToHostMessage serializes"))
    });
    let should_interrupt = Arc::new(move || interrupted(&interrupt_for_vm));
    // 与 TS 的 `startVm({data, wasm, post, shouldInterrupt})` 对齐：worker 后端不传
    // `maxStackSize` / `interruptBudget`，两者留空。
    let start = WorkerStart {
        data,
        wasm,
        max_stack_size: None,
        interrupt_budget: None,
        post,
        should_interrupt,
    };
    let thread_observer = Arc::clone(&observer);
    let worker_for_thread = Arc::clone(&worker);
    // spawn 只保证线程已创建，不等 VM 就绪；`worker` 句柄可以立即 post（消息先排队）。
    let thread = std::thread::Builder::new()
        .name("kiana-codemode-worker".to_string())
        .spawn(move || {
            // TS 的 worker 是一个真线程：`run_worker` / `startVm` 里未捕获的异常会触发
            // `worker.on("error")`（→ `events.failure({kind:"sandbox", name, message})`），
            // **然后**触发 `exit`。Rust 里「未捕获异常」就是 panic：如果不接住，线程在到达下面
            // 的 `exit` 之前就结束了，执行永远不结算、admission 也泄漏。因此整个入口包在
            // `catch_unwind` 里，panic 按 `error` 上报。
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(run_worker(start, receiver, start_vm)),
                    Err(error) => thread_observer.error(CodemodeError::new(
                        CodemodeErrorKind::Sandbox,
                        format!("Failed to start worker: {error}"),
                    )),
                }
            }));
            if let Err(payload) = outcome {
                // TS 的 `error` 事件的 `name` 来自 `Error.name`；`panic_payload` 没有名字，
                // 用普通 `Error`（`host.ts` 的 `error instanceof Error ? error.name : undefined`）。
                let mut error =
                    CodemodeError::new(CodemodeErrorKind::Sandbox, panic_message(payload));
                error.name = Some("Error".to_string());
                thread_observer.error(error);
            }
            // `exit` 事件：线程真正结束（`terminate()` 之后可能很久才到）。panic 之后也必须发。
            thread_observer.exit(if worker_for_thread.terminated.load(Ordering::SeqCst) {
                1
            } else {
                0
            });
        })
        .map_err(|error| error.to_string())?;
    // 不 join：`unref()` 的语义就是宿主不等待线程（`std::thread` 由系统回收）。
    drop(thread);
    Ok(worker)
}

/// panic 载荷 →文本（与 `execution.rs` 的同名辅助一致；worker 线程的 panic 文本要进
/// `SandboxError.message`）。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "an unknown error".to_string()
    }
}
