//! runtime/vm.ts：驱动一个 QuickJS VM 跑完一个脚本，与把它接到宿主的传输方式无关。
//!
//! TS 源：`packages/codemode/src/runtime/vm.ts`（341 行）。
//!
//! worker 线程、调用线程内的 launcher、远端服务器都通过这一份实现跑 VM，prelude 与宿主之间的
//! 桥因此只有一份。
//!
//! # 与调用方的契约（当前稳定契约，改动需同步调用方与测试）
//!
//! - [`StartVmOptions::post`] 在 VM 回调内**同步**收到发往宿主的每条消息。调用方**不得**从
//!   `post` 重入 VM（不 `deliver`、不 `dispose`）：worker 把消息投给另一个线程，in-thread
//!   launcher 则把投递推迟到微任务。
//! - [`VmSession::deliver`] 把一条工具结果交给 VM 并跑到它再次等待。只能在 VM 空闲时调用——
//!   任何不从 `post` 重入的调用方都满足这一点。
//! - [`StartVmOptions::should_interrupt`] 由 QuickJS 在脚本运行时轮询，这里也在每批 promise
//!   job 之间轮询（`JOBS_PER_POLL = 50`）。返回 `true` 会在脚本里抛出一个不可 catch 的错误，
//!   VM 随后报 `crash` 或失败的 `done`。
//! - VM **不会**自行决定何时结束：`done`/`crash` 之后它仍活着，直到调用方 dispose 或终止
//!   宿主它的线程。
//!
//! VM 存在之后的失败都以 `crash` 消息报告；[`start_vm`] 只在 VM 根本创建不出来时返回 `Err`。
//!
//! # 确定性模式（`data.deterministic`，`CodemodeSandboxBase::start()` 使用）
//!
//! 同一脚本在同一份结果下必须每次做出同样的调用、得到同样的输出，宿主才能在重启后重放它。
//! VM 本身已经是确定的（只在宿主投递东西时运行，没有 timer）；剩下在这里固定：
//! - **时间**：每个 WASI 时钟读虚拟时间——`startedAt`，以及正在处理的那条结果的 `now`，绝不
//!   倒退。创建上下文期间 realtime 时钟读一个由种子派生的值，因为 QuickJS 用第一次时钟读
//!   来播种 `Math.random()`，而哪次读先发生是引擎的事。
//! - **随机**：`random_get` 从种子上的 PRNG 取数。
//! - **时区**：UTC，不是宿主的。
//! - **GC**：没有 `WeakRef` 与 `FinalizationRegistry`（`Intrinsics::ALL & !WEAK_REF`），
//!   回收时机不可观察。
//! - **投递顺序**：结果带 `seq`，严格按序应用，早到的先缓冲——传输（RPC 回复）可能以不同于
//!   宿主发送的顺序送达。
//!
//! VM 在脚本运行前报 `started`（带指纹），每次 drain 之后报 `idle`（带已发出的调用数），
//! 好让宿主逐步核对重放。
//!
//! # Rust 侧实现（wasmtime 嵌入同一份 `quickjs-wasi` 3.6.2 的 wasm）
//!
//! `quickjs-wasi` 的 JS 层只是这份 core module 的一层薄绑定（`QuickJSOptions` →
//! `_initialize` + `qjs_init2` + `qjs_set_*`，`JSValueHandle` → `qjs_*_value` 的堆盒）。
//! 这里按 ABI 重新实现 vm.ts 用到的那一小撮绑定（`wasm_impl` 模块），不依赖 npm 包：
//! - 12 个 import：`env.{host_call, host_interrupt, host_promise_rejection,
//!   host_module_normalize, host_module_load, host_get_timezone_offset}` 与
//!   `wasi_snapshot_preview1.{clock_time_get, fd_close, fd_fdstat_get, fd_seek, fd_write,
//!   random_get}`；
//! - 引擎版本从 `qjs_get_quickjs_version()` 读（`0.17.0`），包版本固定为常量 `3.6.2`；
//! - `Intrinsics::ALL & !WEAK_REF` = `0xFFFF_FDFF` 传给 `qjs_init2`；
//! - `timezoneOffset: 0` → `host_get_timezone_offset` 恒返回 0。
//!
//! prelude 的**文本本身**逐字节进入 VM（hash `8c6c9242`），不注入 `Math.random`——引擎的
//! 播种链（时钟 → xorshift64* → `(out >> 12) * 2^-52`）在虚拟时钟下就是确定性值，见
//! spec §2.2.8 与 `tests/vm.rs` 的 pinned 断言。
//!
//! WASI 函数的字节序（spec 附录 A.6 第 11 条）：`clock_time_get` 写**小端 u64**；
//! `random_get` 按 4 字节一组小端写（最后一组按需截断）；`fd_write` 的 iovec 是每 8 字节的
//! `(ptr: u32, len: u32)`，`nwritten` 为小端 u32。
//!
//! # 已知偏差：孤立 UTF-16 代理项 → U+FFFD
//!
//! quickjs-wasi 的 `JSValueHandle.toString()` 用 WTF-8 解码，孤立代理项（`"\uD800"`）作为
//! 单个 UTF-16 码元原样保留在一个 JS 字符串里；Rust 的 `String` 装不下代理项。桥的**每个**
//! 字符串出口因此都换成一个 U+FFFD（`wasm_impl` 的 `decode_wtf8` / `host_to_string`）：
//! `text()`/`console.log()` 的输出文本、脚本返回值/工具参数等经 `JSON.stringify` 的桥文本
//! （再由 `kiana_json::js_parse::parse` 按 ARCHITECTURE §8 第 31 条换成 U+FFFD）、以及脚本错误的
//! `name`/`message`/`stack`。TS 侧这些位置都保留代理项（例如 `["\uD800"]` 的返回值仍是
//! `["\ud800"]`，输出文本长度为 1）。这与 §8 第 31 条对 `kiana-json` 的既定决定同一族、同样
//! 是 Rust `String` 的被迫偏差；要消除它需要在公开类型里引入 WTF-8/UTF-16 字符串，超出本
//! crate 的冻结面。
//!
//! # 一次运行的生命周期
//!
//! [`VmSession`] 背后是一个专用 OS 线程（创建见 [`start_vm`]，线程主体见 `wasm_impl` 的
//! `Handle`/`run_vm_thread`）。时序是：
//!
//! 1. **创建**：`start_vm` 校验 `max_stack_size`，建三条阻塞通道（命令、就绪、回复），起线程，
//!    然后等「就绪」。它虽然是 `async fn`，函数体里没有任何 `await`；返回时 VM 已经停在第一次
//!    等待上，所以宿主看到的时序与 TS 的同步 `startVm` 相同。
//! 2. **初始化**（VM 线程，`Session::create`）：实例化 wasm → `_initialize` → `qjs_init`
//!    （确定性模式用 `qjs_init2` 去掉 `WeakRef`/`FinalizationRegistry`）→ 读导出表 → 缓存
//!    `undefined` 单例 → 应用内存/栈/中断限制 → 结束虚拟时钟的 `creating` 阶段。这一段的失败
//!    全部是 [`StartVmError`]，此时还没有任何 `post`。
//! 3. **启动段**（VM 线程，`Session::preamble`，即 TS `startVm` 的 try 块）：注册 `bridge` →
//!    求值并调用 prelude 得到 `{ settle, run, stalled }` → 确定性模式先报 `started` 指纹 →
//!    求值用户脚本 → 调 `run` → `drain` 到空 → 确定性模式报 `idle{seq:0}`。这一段的普通错误
//!    变成消息（`done` 或 `crash`），**不是** [`StartVmError`]；例外是宿主回调的 panic，见
//!    下节「失败的出口」第 4 条（`catch_unwind` 把它转成 `StartVmError` 经就绪通道返回）。
//! 4. **投递**（每次 [`VmSession::deliver`]）：把一条 `HostResult` 送进 VM，跑到它再次等待或
//!    结束才返回。
//! 5. **销毁**：[`VmSession::dispose`] 发 `Stop` 并 `join` VM 线程；之后再 `deliver` 是 no-op。
//!
//! 宿主能看到的消息：`call`/`output` 由 `bridge` 在脚本运行时发出，`done`/`crash` 由 prelude
//! 或本文件的失败路径发出，`started`/`idle` 只在确定性模式出现。
//!
//! # 线程与并发
//!
//! - VM 线程独占 `wasmtime::Store`；[`VmSession`] 只保证 `Send`（`tests/vm.rs` 的
//!   `session_is_send` 钉住这一点），要跨线程共享由调用方加锁（`runtime::worker` 与
//!   `runtime::inline` 都用 `Mutex`）。同一会话上的 `deliver` 因此不会并发进入 VM。
//! - `post` 在 VM 线程上被同步调用（多数来自 VM 回调内部，`started`/`idle` 来自启动段与
//!   `deliver` 之后）：回调返回前 VM 不会前进。**不要**从 `post` 里再进同一会话的
//!   `deliver`/`dispose`（见本文件开头的契约）。
//! - `start_vm` 的 `ready_rx.recv()` 与 `Handle::deliver` 的 `replies.recv()` 都是阻塞调用：
//!   在 async 任务里调用会占住当前线程，直到 VM 停下来。这是「同步语义」的另一面。
//!
//! # 失败的出口
//!
//! 1. `Session::create` 阶段（实例化、`qjs_init`、读导出、线程起不来）→ `Err(StartVmError)`，
//!    没有任何消息被 `post` 过。
//! 2. 脚本求值抛出 guest 异常 → 一条失败的 `done`（TS 也把它当脚本错误，不是 crash），
//!    `start_vm` 仍返回 `Ok`。
//! 3. 其它运行期失败（prelude、`run`、trap、`should_interrupt` 命中、确定性模式下 `seq`
//!    缺失/重复/回退）→ `crash`。启动段的这类失败会先 `post` 一条 `crash`、再让 `start_vm`
//!    返回 `Ok`（会话对象仍然存在）；`deliver` 段的失败只 `post`，函数本身没有返回值。
//! 4. 宿主回调里的 panic：启动段（`Session::preamble`）被 `catch_unwind` 转成 `StartVmError`
//!    （`QuickJS panicked while starting: …`）；`deliver` 段把 panic 载荷交回调用线程重新
//!    抛出，不吞掉。`Session::create` 没有包 `catch_unwind`，那里的 panic 会让 VM 线程直接
//!    退出，调用方只看到 `Failed to start QuickJS`（通道关闭）。
//!
//! # 阅读顺序
//!
//! [`StartVmOptions`] → [`start_vm`] → `wasm_impl` 的模块说明（句柄所有权）→
//! `Session::create` → `Session::preamble` → `Session::drain` / `Session::apply` /
//! `Session::deliver_result`。WASI 与 `env` 导入函数只在排查时钟、随机数、崩溃文本时需要。

use std::fmt;
use std::sync::Arc;

use crate::runtime::protocol::{HostToWorkerMessage, WorkerToHostMessage};
use crate::wasm::CodemodeWasmModule;

/// `MAX_STACK_SIZE`：QuickJS 自己栈守卫的默认上限（512 KiB，quickjs-wasi 的默认值）。
///
/// TS: `quickjs-wasi` 的 `MAX_STACK_SIZE`。它不只是 QuickJS 的数字：wasm 帧也用宿主线程的
/// 原生栈，守卫必须在原生栈耗尽之前触发。
///
/// # 它与 wasmtime 的栈上限是两个数
///
/// 这个值经 `qjs_set_max_stack_size` 交给 QuickJS，决定 **JS 递归**在哪一层抛出可 catch 的
/// `RangeError`（worker 约 2180 层、inline 的 256 KiB 约 1090 层）；`wasmtime::Config::
/// max_wasm_stack` 决定 wasm **帧**能占多少原生栈，超了就是 trap（crash）。两者独立标定：
/// 每档 wasm 预算都远大于该档守卫换算出的 wasm 帧用量，所以 JS 递归先撞 `RangeError`；
/// 而 C 递归（解析器、`JSON.stringify`）几乎不耗 QuickJS 的守卫，先撞 wasm 预算。见
/// `WASM_STACK_SIZE` 与 `INLINE_WASM_STACK_SIZE`。
pub const MAX_STACK_SIZE: usize = 512 * 1024;

/// wasmtime 的 wasm 栈预算（`Config::max_wasm_stack`），worker 后端。
///
/// 与 [`MAX_STACK_SIZE`]（QuickJS 自己的守卫）是两个数：后者决定深递归在哪一层抛可 catch 的
/// `RangeError`，前者决定 wasm **帧**能用多少原生栈，超了是 trap（`crash`）。
///
/// # 取值依据（与 TS worker 的逐点对齐）
///
/// TS 的 worker 线程默认 4 MB 原生栈；V8 在那上面跑到 `JSON.stringify` 22000 层成功、
/// 23000 层 trap，嵌套数组字面量 2600 层成功、2800 层 trap。quickjs.wasm 在 wasmtime 里
/// 每层 C 递归占的 wasm 帧略大于 V8，实测 5 650 000 字节让 Rust worker 后端落在同一组
/// 阈值上（`crates/kiana-codemode/tests/vm.rs` 的深递归测试钉住这些点）。
///
/// 这个预算必须小于运行 VM 的线程真实可用的栈：见 [`VM_THREAD_STACK_SIZE`]。
#[cfg_attr(not(feature = "wasm"), allow(dead_code))]
pub(crate) const WASM_STACK_SIZE: usize = 5_650_000;

/// wasmtime 的 wasm 栈预算，inline/remote 后端（[`crate::wasm::inline_engine`]）。
///
/// TS 的 inline/remote launcher 把 VM 跑在**调用线程**上，Node 主线程只有约 1 MB 原生栈：
/// 深 C 递归（解析器、`JSON.stringify`）在 600 层嵌套字面量 / 6000 层 `JSON.stringify` 处
/// 耗尽它，V8 抛宿主 `RangeError`，`startVm` 把它当 crash 上报（`kind: "sandbox"`）。
/// wasmtime 的 wasm 帧比 V8 略大，实测 1 100 000 字节落在同一组阈值上：500 层嵌套字面量与
/// 4000 层 `JSON.stringify` 成功，600 层与 6000 层变成
/// `RangeError: Maximum call stack size exceeded` 的 crash。
///
/// 这一档不能复现 TS 的**全部**边界：wasmtime 的「解析器帧 / JSON.stringify 帧」比值（约
/// 8）比 V8（约 10）小，所以同一个预算下 `JSON.stringify` 的边界比 TS 早约 15%（TS 在
/// 5500 层仍成功，这里在 5000 层就 trap）。两档之间没有能同时对齐两条曲线的取值；这里按
/// 审查给出的可观察点（600 / 6000 层）取值。
#[cfg_attr(not(feature = "wasm"), allow(dead_code))]
pub(crate) const INLINE_WASM_STACK_SIZE: usize = 1_100_000;

/// VM 专用线程的原生栈大小。
///
/// wasm 栈预算（[`WASM_STACK_SIZE`] / [`INLINE_WASM_STACK_SIZE`]）之外还要留给宿主函数、
/// wasmtime 的 trampoline 和 `post` 回调；16 MiB 对两档预算都有充足余量（真正决定深递归
/// 行为的是 wasmtime 的 `max_wasm_stack`，不是这个数：它只是一道「绝不让原生栈先于 wasm
/// 预算耗尽」的保险）。
#[cfg_attr(not(feature = "wasm"), allow(dead_code))]
const VM_THREAD_STACK_SIZE: usize = 16 * 1024 * 1024;

/// 本模块或 prelude 改变确定性脚本可观察行为时递增（指纹的一部分）。
///
/// TS: `DETERMINISTIC_VERSION`。
pub const DETERMINISTIC_VERSION: u32 = 1;

/// drain 时每两个 `should_interrupt` 轮询之间跑多少个 promise job。
///
/// TS: `JOBS_PER_POLL`。
pub const JOBS_PER_POLL: u32 = 50;

/// `quickjs-wasi` npm 包的版本（指纹的第一段）。
///
/// TS: `vm.versions["quickjs-wasi"]`（quickjs-wasi 的 `VERSION` 常量，随包固定为 3.6.2）。
pub const QUICKJS_WASI_VERSION: &str = "3.6.2";

/// 嵌入的 `quickjs.wasm` 里 `qjs_get_quickjs_version()` 报告的引擎版本。
///
/// 指纹里的这一段仍**从 wasm 读**（`wasm_impl::Session::fingerprint` 调
/// `qjs_get_quickjs_version`），这个常量只用于文档与测试。
pub const QUICKJS_VERSION: &str = "0.17.0";

/// `Intrinsics.WEAK_REF`（`1 << 9`）：确定性模式把它从全集中去掉。
#[cfg(feature = "wasm")]
const INTRINSICS_WEAK_REF: u32 = 1 << 9;

/// `Intrinsics.ALL`：quickjs-wasi 的「全部 intrinsics」掩码。
#[cfg(feature = "wasm")]
const INTRINSICS_ALL: u32 = 0xFFFF_FFFF;

/// 确定性模式传给 `qjs_init2` 的掩码：`Intrinsics.ALL & ~Intrinsics.WEAK_REF`（`0xFFFF_FDFF`）。
///
/// 效果是 `typeof WeakRef === "undefined"` **且** `typeof FinalizationRegistry ===
/// "undefined"`（spec 附录 A.6 第 2 条）；必须在**创建上下文时**去掉，而不是在 prelude 里删除
/// （prelude 的 `lockdown` 只冻结，不删除）。
#[cfg(feature = "wasm")]
pub(crate) const DETERMINISTIC_INTRINSICS: u32 = INTRINSICS_ALL & !INTRINSICS_WEAK_REF;

/// `startVm` 的选项。
///
/// TS: `StartVmOptions`。整体移交给 [`start_vm`]，之后随 VM 线程存活到 `dispose`；两个回调
/// 都在 VM 线程上被调用，所以要求 `Send + Sync`。
pub struct StartVmOptions {
    /// 运行一个脚本所需的全部数据。
    pub data: crate::runtime::protocol::VmStartData,
    /// 编译好的 `quickjs.wasm`（与 TS 的 `WebAssembly.Module` 等价）。
    pub wasm: CodemodeWasmModule,
    /// QuickJS 对自身栈的守卫上限；缺省为 [`MAX_STACK_SIZE`]。
    ///
    /// wasm 帧也用宿主线程的原生栈，所以守卫必须在原生栈耗尽之前触发：宿主栈溢出会让整个
    /// 执行失败，而不是抛出一个可 catch 的 `RangeError`。
    ///
    /// TS 的 `maxStackSize` 是任意 number，quickjs-wasi 的 `normalizeOptions` 用
    /// `RangeError` 拒绝非整数、负数与超过 [`MAX_STACK_SIZE`] 的值；`Option<usize>` 只能
    /// 表示其中合法的整数，所以前两类在 Rust 里是类型层拒绝（编译错误），只有超限会在
    /// 运行期得到一个 name 为 `RangeError` 的 [`StartVmError`]。
    pub max_stack_size: Option<usize>,
    /// 这次 VM 可用的中断轮询次数（如果 launcher 计数）；只进入指纹。
    ///
    /// TS 的 `interruptBudget` 是任意 number，指纹用 JS 的 `String(number)` 插值；
    /// `Option<u32>` 是骨架冻结的 API（launcher 传的是整数计数），因此 `50000.5` 这类
    /// 非整数预算不可表示——已发布的后端都不传非整数。
    pub interrupt_budget: Option<u32>,
    /// 每条发往宿主的消息的回调，**同步**地由 VM 回调内调用。
    pub post: Arc<dyn Fn(WorkerToHostMessage) + Send + Sync>,
    /// QuickJS 的 interrupt handler；`true` 表示中断脚本。
    pub should_interrupt: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl fmt::Debug for StartVmOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartVmOptions")
            .field("data", &self.data)
            .field("max_stack_size", &self.max_stack_size)
            .field("interrupt_budget", &self.interrupt_budget)
            .finish_non_exhaustive()
    }
}

/// 一个绑定到某次脚本运行的 VM 会话。
///
/// TS: `VmSession`。`dispose` 之后不得再使用。
pub struct VmSession {
    #[cfg(feature = "wasm")]
    inner: Option<wasm_impl::Handle>,
    #[cfg(not(feature = "wasm"))]
    _private: (),
}

impl fmt::Debug for VmSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VmSession")
    }
}

impl VmSession {
    /// 把一条宿主回答交给 VM，并跑到它再次等待（或结束）。
    ///
    /// TS: `session.deliver(message)`。失败以 `crash` 消息报告，不返回 `Err`。
    ///
    /// 返回时脚本已经停住（或已 `done`/`crash`）。`dispose` 之后调用是 no-op。确定性模式下
    /// 结果必须带 `seq` 且不能回退或重复：早到的会先缓冲，缺 `seq`/重复会变成 `crash`。
    pub fn deliver(&mut self, message: HostToWorkerMessage) {
        #[cfg(feature = "wasm")]
        if let Some(session) = self.inner.as_mut() {
            session.deliver(message);
        }
        #[cfg(not(feature = "wasm"))]
        let _ = message;
    }

    /// 释放 VM 的 wasm 实例。之后不得再使用本会话。
    ///
    /// TS: `session.dispose()`。只执行一次。
    ///
    /// wasm 后端下阻塞到 VM 线程退出（`join`）后才返回；重复调用是 no-op。
    pub fn dispose(&mut self) {
        #[cfg(feature = "wasm")]
        if let Some(session) = self.inner.as_mut() {
            session.dispose();
            self.inner = None;
        }
    }
}

/// [`start_vm`] 失败：VM 根本创建不出来。
///
/// TS 的 `startVm` 在这种情况下 reject 一个 `Error`；调用方一般取 `errorMessage`
/// （`message`），而 `worker.ts:37` 那样把启动失败转成 `crash` 的调用方拼的是
/// `` `${error.name}: ${error.message}` ``，所以错误名也是契约的一部分（例如
/// `maxStackSize` 校验失败是 `RangeError`）。
///
/// # 文本
///
/// `Display` 只写 `message`；需要 `${name}: ${message}` 的调用方（如 worker 的 `crash` 文案）
/// 自己用 [`StartVmError::name`] 拼。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartVmError {
    name: &'static str,
    message: String,
}

impl StartVmError {
    /// 用原因文本构造一个普通 `Error`（TS 的 `error.name === "Error"`）。
    pub fn new(message: impl Into<String>) -> Self {
        StartVmError {
            name: "Error",
            message: message.into(),
        }
    }

    /// 用错误名与原因文本构造（TS 的 `error.name` / `error.message`）。
    ///
    /// `name` 是 `'static`：这几种错误来自固定的内建类型名（`"RangeError"` 等），不是数据。
    pub fn with_name(name: &'static str, message: impl Into<String>) -> Self {
        StartVmError {
            name,
            message: message.into(),
        }
    }

    /// TS 的 `error.name`（`"Error"`、`"RangeError"` …）。
    pub fn name(&self) -> &str {
        self.name
    }

    /// 原因文本（TS 的 `error.message`）。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for StartVmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StartVmError {}

/// `startVm(options)`：创建 VM，跑完 prelude 与脚本，直到它第一次等待（或结束）。
///
/// 返回 future 只是为了便于调用方 await；实际工作在专用的 VM 线程上同步完成（见
/// `wasm_impl::Handle`），本函数等它跑到第一次等待后返回，因此宿主看到的时序固定为：返回时脚本已停在第一次等待。
///
/// # 失败
///
/// 返回 `Err` 的有两类路径（见 [`StartVmError`]）：
/// - VM 创建阶段：线程起不来、wasm 实例化/初始化失败、模块缺少要求的导出，或
///   `max_stack_size` 超过 [`MAX_STACK_SIZE`]（错误名 `RangeError`）；
/// - 启动段（preamble）里宿主回调 panic：`run_vm_thread` 用 `catch_unwind` 把它转成
///   `StartVmError`（`QuickJS panicked while starting: …`）经就绪通道发回。
///
/// 脚本自己的求值错误不在这里：那条路径会先 `post` 一条失败的 `done`，然后返回 `Ok`。
/// feature `wasm` 关闭时本函数总是返回后端不可用的 `Err`。
pub async fn start_vm(options: StartVmOptions) -> Result<VmSession, StartVmError> {
    #[cfg(feature = "wasm")]
    {
        wasm_impl::start(options).map(|session| VmSession {
            inner: Some(session),
        })
    }
    #[cfg(not(feature = "wasm"))]
    {
        let _ = options;
        Err(StartVmError::new(
            "the wasm backend is disabled (enable the \"wasm\" feature)",
        ))
    }
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// wasm 后端：直接在 wasmtime 里跑 quickjs-wasi 3.6.2 的 core module
// ---------------------------------------------------------------------------

#[cfg(feature = "wasm")]
mod wasm_impl {
    //! wasm 后端实现：把 `quickjs.wasm` 当成一个普通的 wasm core module 驱动。
    //!
    //! 分层（自下而上）：
    //!
    //! - WASI / `env` 导入函数：替代 TS 的 `createWasiShim`、`deterministicWasi` 与
    //!   `QuickJS.handleHostCall` 三个 shim；
    //! - `Exports`：按 ABI 从实例取导出函数并做类型检查，`HostApi` 是它给宿主回调用的一小撮
    //!   可克隆句柄；
    //! - `VmState`：`Store` 的附带数据（`post`、中断闭包、虚拟时钟/PRNG、`undefined` 单例）；
    //! - `Session`：一个 VM 的全部可变状态（store、导出句柄、prelude 的 `api`/`settle`/
    //!   `stalled` 句柄、确定性模式的 seq 队列）；
    //! - `Handle`：`VmSession` 的内部表示——两条通道（命令发送端、回复接收端）+ 专用线程，提供
    //!   同步的 `deliver`/`dispose`。第三条「就绪」通道是 `start` 与 `run_vm_thread` 之间的
    //!   局部握手：线程只拿到 `ready_tx`，`ready_rx` 在 `start` 里收到结果后即丢弃，`Handle`
    //!   不持有它。
    //!
    //! # guest 句柄的所有权（改这里最容易出错的地方）
    //!
    //! wasm 边界上的 `i32` 是 QuickJS 堆里的**引用**，不是 Rust 指针：
    //!
    //! - 每个从导出读出的 guest **值句柄**（属性值、函数调用结果、`qjs_new_*` 的产物）都要
    //!   `qjs_free_value` 恰好一次（主要出口是 `Exports::free` 与 `host_free`；
    //!   `host_promise_rejection_import` 也直接释放它收到的两个值）；例外是与 VM 同寿的那几个
    //!   句柄（`undefined` 单例、`bridge`、prelude 的 `api`/`settle`/`run`/`stalled`），它们
    //!   从不单独释放。传入导出函数的实参句柄仍由调用方持有：被调者只借用、不接管所有权，
    //!   宿主在 `qjs_call`/`qjs_eval` 返回后必须释放自己创建的实参（见 `Session::apply` 与
    //!   `eval_prelude` 在调用返回后逐个 `free`）。
    //! - `qjs_get_string_len` 返回的 C 字符串由引擎分配，用 `qjs_free_cstring` 释放；它写入的
    //!   长度缓冲是宿主用 `wasm_malloc` 分配的，用 `wasm_free` 释放。二者成对出现，漏一个就是
    //!   wasm 线性内存泄漏——不是 Rust 堆泄漏，Rust 的所有权检查看不见它。
    //! - `host_call` 的返回值归调用它的 C 跳板所有，宿主必须交出一份**新的**引用；
    //!   `host_return_undefined` 因此 `qjs_dup_value` 缓存的单例，而单例本身永不释放。
    //! - 每个 `wasm_malloc` 出来的缓冲（字符串、argv、属性名）都由调用方 `wasm_free`。
    //!
    //! # 宿主函数如何报告失败
    //!
    //! 宿主函数无法像 JS 那样直接 `throw`，只有两条路：
    //!
    //! - 返回哨兵值，同时用 `qjs_throw` 把异常挂进 VM（`host_throw`，返回 `0`）；
    //! - 返回 `Err` 中止整个 wasm 调用（`HostThrown`，WASI 时钟的 `RangeError` 走这条）。
    //!
    //! 两条路径的可观察行为不同：
    //!
    //! - `qjs_throw` 挂起的异常是普通的 JS 异常，脚本可以 `try/catch`；没被接住时它经
    //!   `qjs_call`/`qjs_eval` 返回给宿主，变成 `done` 的 `error` 或 `crash` 文本。
    //! - 返回 `Err` 会直接中止这次 wasm 调用，guest 接不住，由 `wasm_failure` 变成 `crash`。
    //!
    //! WASI 时钟的 `RangeError` 走第二条，`bridge` 的错误走第一条。

    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use wasmtime::{Caller, Extern, Func, Instance, Linker, Memory, Store, TypedFunc};

    use super::{
        DETERMINISTIC_INTRINSICS, DETERMINISTIC_VERSION, JOBS_PER_POLL, MAX_STACK_SIZE,
        QUICKJS_WASI_VERSION, StartVmError, StartVmOptions,
    };
    use crate::runtime::prelude_source::{PRELUDE_HASH, PRELUDE_SOURCE};
    use crate::runtime::protocol::{
        CallTarget, HostResult, HostToWorkerMessage, WorkerToHostMessage,
    };
    use crate::types::{CodemodeDuring, CodemodeOutputItem};

    /// 脚本源码的文件名：前缀与脚本共处第 1 行，报错行号与脚本原文一致。
    const SCRIPT_FILENAME: &str = "codemode.js";

    /// prelude 的文件名（prelude 的栈帧据此被过滤）。
    const PRELUDE_FILENAME: &str = "codemode-prelude.js";

    /// `bridge` 宿主函数的注册名。
    const BRIDGE_NAME: &str = "bridge";

    /// WASI 的 `ERRNO_SUCCESS`。
    const ERRNO_SUCCESS: i32 = 0;
    /// WASI 的 `ERRNO_BADF`。
    const ERRNO_BADF: i32 = 8;
    /// WASI 的 `ERRNO_NOSYS`。
    const ERRNO_NOSYS: i32 = 52;

    /// WASI 的 `CLOCK_REALTIME`。
    const CLOCK_REALTIME: i32 = 0;
    /// WASI 的 `CLOCK_MONOTONIC`。
    const CLOCK_MONOTONIC: i32 = 1;

    /// `fs_filetype = CHARACTER_DEVICE`（`fd_fdstat_get` 对 fd 1/2 的回答）。
    const FILETYPE_CHARACTER_DEVICE: u8 = 2;

    // ---- 确定性时钟与 PRNG ----

    /// 确定性 VM 的时钟：上下文创建期间由种子派生，之后是虚拟时间。
    ///
    /// TS: `VirtualClock`。
    #[derive(Debug, Clone, Copy)]
    pub(super) struct VirtualClock {
        /// 上下文是否仍在创建（QuickJS 用第一次时钟读播种 `Math.random()`）。
        pub(super) creating: bool,
        /// 起点（ms since epoch）。
        pub(super) started_at: f64,
        /// 当前虚拟时间（ms since epoch），只增不减。
        pub(super) now: f64,
        /// 32 位种子。
        pub(super) seed: u32,
    }

    /// mulberry32：一个小而快的 PRNG，每次调用一个 32 位输出。
    ///
    /// TS: `prng(seed)`（`Math.imul` 就是 32 位回绕乘法）。
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Prng {
        state: u32,
    }

    impl Prng {
        /// `prng(seed)`：`state = seed >>> 0`。
        pub(super) fn new(seed: u32) -> Self {
            Prng { state: seed }
        }

        /// 一次抽取：`state = (state + 0x6d2b79f5) >>> 0`，两次 `Math.imul` 混淆，末了
        /// `(value ^ (value >>> 14)) >>> 0`。
        fn next_u32(&mut self) -> u32 {
            self.state = self.state.wrapping_add(0x6d2b_79f5);
            let mut value = (self.state ^ (self.state >> 15)).wrapping_mul(1 | self.state);
            value = value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value)) ^ value;
            value ^ (value >> 14)
        }
    }

    /// `Math.round`：最近的整数，`.5` 向 +∞（`-0.5` 得 `-0`，`-0.5 < x < 0` 也得 `-0`）。
    ///
    /// 不能写成 `(x + 0.5).floor()`：`0.49999999999999994 + 0.5` 在 f64 里进位成 `1.0`，
    /// 而规范（以及 QuickJS 的 `js_round`）取「与 `floor(x)` 的距离是否 ≥ 0.5」，
    /// `Math.round(0.49999999999999994)` 因此是 `0` 而不是 `1`。时钟把毫秒喂给
    /// `BigInt(Math.round(ms))`，这个差值会直接变成 1 毫秒的偏差。
    fn js_math_round(x: f64) -> f64 {
        if !x.is_finite() || x == 0.0 || x.fract() == 0.0 {
            return x;
        }
        // 规范：`-0.5 <= x < 0` 的结果是 `-0`（`floor(x) + 1` 会丢掉符号）。
        if x > -0.5 && x < 0.0 {
            return -0.0;
        }
        if x == -0.5 {
            return -0.0;
        }
        let floor = x.floor();
        if x - floor >= 0.5 { floor + 1.0 } else { floor }
    }

    /// ECMAScript 的 `Math.max(a, b)`（两个参数）。
    ///
    /// 不能直接用 `f64::max`：Rust 的 `f64::max(NaN, x)` 返回 `x`（IEEE 754 的 maxNum），
    /// 而 JS 的 `Math.max(NaN, x)` 是 `NaN`。`startedAt` 为 `NaN` 时虚拟时钟必须一直保持
    /// `NaN`（下一次时钟读仍抛 `RangeError`），不能因为一条 `now` 有限的投递变回有限值。
    /// 两个 `±0` 按规范的 `+0 > -0` 取 `+0`。
    ///
    /// TS: `vm.ts:218` 的 `Math.max(clock.now, next.now)`。
    fn js_math_max(a: f64, b: f64) -> f64 {
        if a.is_nan() || b.is_nan() {
            return f64::NAN;
        }
        if a == 0.0 && b == 0.0 {
            return if a.is_sign_positive() || b.is_sign_positive() {
                0.0
            } else {
                -0.0
            };
        }
        if a > b { a } else { b }
    }

    /// ECMAScript 的 `ToInt32`（`wasm` 的 i32 参数就是 `ToInt32`）。
    fn js_to_int32(value: f64) -> i32 {
        if !value.is_finite() {
            return 0;
        }
        let modulo = value.trunc().rem_euclid(4_294_967_296.0);
        (modulo as u64 as u32) as i32
    }

    /// ECMAScript 的 `ToUint32`（`seed >>> 0`）。
    fn js_to_uint32(value: f64) -> u32 {
        js_to_int32(value) as u32
    }

    /// ECMAScript 的 `ToUint64`（`a.toNumber()` 进来的调用编号）。
    fn js_to_uint64(value: f64) -> u64 {
        if !value.is_finite() {
            return 0;
        }
        value.trunc() as i64 as u64
    }

    /// `BigInt(Math.round(ms)) * 1_000_000n` 交给 `setBigUint64` 的结果（按 2^64 回绕）。
    ///
    /// TS 的表达式 `BigInt(Math.round(clock.now)) * 1_000_000n` 有两个语义：
    /// - `BigInt(x)` 要求 `x` 是整数（`Math.round` 的输出总是整数或 `NaN`/`±Infinity`）；
    ///   `NaN`/`±Infinity` 抛 `RangeError`，`clock_time_get` 于是抛进 VM，最终成为 `crash`。
    /// - 乘法**精确**并回绕到 2^64（`DataView.setBigUint64` 截断）。`f64` 只保留 53 位有效位，
    ///   所以先 `Math.round` 再乘 1e6 与 `(x * 1e6) as u64` 不同：`1e19` 的 `Math.round` 是
    ///   `1e19`，乘积精确值 `10^25` mod 2^64 = `1590897978359414784`；而直接乘 `1e6` 再转
    ///   `u64` 会先做浮点乘法（丢低位）再饱和/回绕，结果不同。实现因此走「位移 + 加法」的
    ///   精确 64×20 位乘法，而不是 `i128` 乘法（后者在 |ms| ≥ 2^103 附近会溢出，debug 构建
    ///   直接 panic）。
    ///
    /// 返回值：`Err(())` 即 TS 的 `BigInt` 抛 `RangeError`（调用方转成同样的 `crash` 文案）。
    fn nanos_from_millis(millis: f64) -> Result<u64, ClockError> {
        let rounded = js_math_round(millis);
        // `BigInt(NaN)` / `BigInt(±Infinity)`：`RangeError: The number X cannot be converted to
        // a BigInt because it is not an integer`。
        if !rounded.is_finite() {
            return Err(ClockError { value: rounded });
        }
        // 精确的 `round(ms) * 1_000_000 mod 2^64`。
        //
        // `BigInt(rounded)` 对 `f64` 是精确的（`rounded` 本身是整数），所以第一步先得到一个
        // 64 位补码表示：|rounded| < 2^63 时走 `i64`，更大的值（`f64` 时最大 2^1024）拆成
        // 指数 + 尾数移位。溢出位直接丢弃——这正是 `setBigUint64` 的截断。
        //
        // 拆法：`f64` 整数可以写成 `m * 2^e`（`m` 是 53 位尾数，`e >= 0`）；
        // `x * 10^6 = m * 10^6 << e`，其中 `m * 10^6` 至多 73 位，用 u128 精确算完再左移。
        // 左移 `e` 位后只保留低 64 位；`e >= 64 + 73` 时结果必然是 0。
        let magnitude = rounded.abs();
        if magnitude < 9_007_199_254_740_992.0 {
            // 2^53 以内：直接用 i64 乘法（`1e6 * 2^53` 远小于 2^63）。
            let value = rounded as i64;
            return Ok((value.wrapping_mul(1_000_000)) as u64);
        }
        // 2^53 以上：`rounded` 的尾数是 `mantissa`（53 位），值为 `mantissa * 2^shift`，
        // 其中 `shift = exponent - 1075`（`exponent` 是 f64 的偏置指数，`-1023 - 52` 落在尾数上）。
        let bits = magnitude.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as i32;
        debug_assert!(exponent > 0 && exponent < 0x7ff, "finite and >= 2^53");
        // 有效位：`1.fraction` 的 53 位整数。
        let mantissa = (bits & 0x000f_ffff_ffff_ffff) | 0x0010_0000_0000_0000;
        let shift = exponent - 1075;
        debug_assert!(shift >= 1, "magnitude >= 2^53");
        // `mantissa * 10^6` 至多 73 位，u128 足够；`10^6 = 2^6 * 5^6`，乘法与左移可交换。
        let scaled = u128::from(mantissa) * 1_000_000u128;
        // 与 `setBigUint64` 一致：只留低 64 位。`scaled << shift` 的低 64 位只取决于
        // `scaled` 的低 `64 - shift` 位（`shift < 64`）；`shift >= 64` 时整个乘积被 2^64 整除。
        let low = scaled as u64;
        let value = if shift >= 64 {
            0
        } else {
            let mask = (1u64 << (64 - shift)) - 1;
            (low & mask) << shift
        };
        // 负数的 `* 1_000_000n` 是负数，`asUintN(64, …)` 按 2^64 取模。
        Ok(if rounded < 0.0 {
            value.wrapping_neg()
        } else {
            value
        })
    }

    /// `BigInt(Math.round(value))` 拒绝的值（TS 的 `RangeError`）。
    struct ClockError {
        /// `Math.round` 的结果（`NaN` 或 `±Infinity`）。
        value: f64,
    }

    impl ClockError {
        /// TS 的 `RangeError` 文案：`The number ${String(value)} cannot be converted to a BigInt
        /// because it is not an integer`。
        fn message(&self) -> String {
            format!(
                "The number {} cannot be converted to a BigInt because it is not an integer",
                js_number_to_string(self.value)
            )
        }
    }

    /// 宿主函数「抛进」wasm 的错误（TS 里是 WASI shim 抛出的 JS 异常）。
    ///
    /// wasmtime 的宿主函数只能通过 `Err` 中止 wasm 调用（等价于 trap）；这里把 JS 异常的名字
    /// 与文案带在错误对象上，由 [`wasm_failure`] 还原成 `crash` 的 `Name: message`，即同一个
    /// 异常抛到 `startVm`/`deliver` 时应呈现的形态。
    #[derive(Debug)]
    struct HostThrown {
        /// `RangeError` 等。
        name: &'static str,
        /// `error.message`。
        message: String,
    }

    impl fmt::Display for HostThrown {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}: {}", self.name, self.message)
        }
    }

    impl std::error::Error for HostThrown {}

    /// `String.prototype.trimEnd()` 的空白集合（ECMAScript 的 `WhiteSpace` + `LineTerminator`）。
    ///
    /// 不能直接用 `str::trim_end`：Rust 的 `char::is_whitespace` 不是这个集合——它把 U+0085
    /// 当空白（ECMAScript 不算），却不含 U+FEFF（ECMAScript 的 `WhiteSpace` 含）。`stack`
    /// 文本是稳定的用户可见输出（由锁定测试比对），所以这里逐项列出。
    pub(super) fn js_trim_end(text: &str) -> &str {
        text.trim_end_matches(|ch: char| {
            matches!(
                ch,
                '\u{0009}'
                    | '\u{000A}'
                    | '\u{000B}'
                    | '\u{000C}'
                    | '\u{000D}'
                    | '\u{0020}'
                    | '\u{00A0}'
                    | '\u{1680}'
                    | '\u{2000}'
                    ..='\u{200A}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{202F}'
                        | '\u{205F}'
                        | '\u{3000}'
                        | '\u{FEFF}'
            )
        })
    }

    /// `Date.now()`（ms since epoch）。
    ///
    /// `SystemTime::now()` 早于 UNIX_EPOCH 时 `duration_since` 返回 `Err`，这里取负值——
    /// `Date.now()` 对 1970 年之前同样给负数。
    fn wall_clock_millis() -> f64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_millis() as f64,
            Err(error) => -(error.duration().as_millis() as f64),
        }
    }

    // ---- 宿主回调需要克隆的 wasm 导出句柄 ----

    /// 从宿主回调内调用 wasm 导出所需的一小撮函数（`TypedFunc` 是 `Send + Sync` 的轻量句柄）。
    #[derive(Clone)]
    struct HostApi {
        qjs_free_value: TypedFunc<i32, ()>,
        qjs_dup_value: TypedFunc<i32, i32>,
        qjs_new_string: TypedFunc<(i32, i32), i32>,
        qjs_new_error: TypedFunc<(), i32>,
        qjs_throw: TypedFunc<i32, i32>,
        qjs_set_prop_string: TypedFunc<(i32, i32, i32), i32>,
        qjs_get_string_len: TypedFunc<(i32, i32), i32>,
        qjs_free_cstring: TypedFunc<i32, ()>,
        qjs_is_undefined: TypedFunc<i32, i32>,
        qjs_get_float64: TypedFunc<i32, f64>,
        qjs_get_bool: TypedFunc<i32, i32>,
        wasm_malloc: TypedFunc<i32, i32>,
        wasm_free: TypedFunc<i32, ()>,
    }

    /// VM 的 `Store` 数据：宿主回调需要的全部状态。
    pub(super) struct VmState {
        /// 发往宿主的消息（同步调用，不得重入 VM）。
        post: Arc<dyn Fn(WorkerToHostMessage) + Send + Sync>,
        /// QuickJS 的 interrupt handler。
        should_interrupt: Arc<dyn Fn() -> bool + Send + Sync>,
        /// 确定性模式的时钟；`None` 即非确定性（宿主时钟、宿主随机、宿主时区）。
        clock: Option<VirtualClock>,
        /// 确定性模式的随机源（种子 `seed ^ 0x9e3779b9`）。
        random: Option<Prng>,
        /// 已发出的调用总数（tools + globals）。
        issued: u64,
        /// 正在处理的结果号（0 = 脚本的首次运行）。
        current: u64,
        /// 实例化之后填入；宿主回调据此调用 wasm 导出。
        host: Option<HostApi>,
        /// 缓存的一次性 `undefined` 句柄（npm 的 `vm.undefined` 单例）。
        ///
        /// `host_call` 的返回值归 C 跳板所有，所以每次都 `qjs_dup_value` 这个句柄；它本身
        /// 与 VM 同寿，绝不 `qjs_free_value`（free 了会让 `dup` 出来的引用悬空）。
        undefined: i32,
    }

    impl fmt::Debug for VmState {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("VmState")
                .field("clock", &self.clock)
                .field("issued", &self.issued)
                .field("current", &self.current)
                .finish_non_exhaustive()
        }
    }

    impl VmState {
        /// 从启动选项派生初始状态。
        ///
        /// 确定性模式同时建虚拟时钟与 PRNG：时钟保留原始 `seed`（创建期读数时 +1），PRNG 用
        /// `seed ^ 0x9e3779b9` 另起一个种子。非确定性时两者都是 `None`，对应的导入函数改读
        /// 宿主时钟与宿主熵。
        fn new(options: &StartVmOptions) -> Self {
            let clock = options
                .data
                .deterministic
                .map(|deterministic| VirtualClock {
                    creating: true,
                    started_at: deterministic.started_at,
                    now: deterministic.started_at,
                    seed: js_to_uint32(deterministic.seed),
                });
            let random = clock.map(|clock| Prng::new(clock.seed ^ 0x9e37_79b9));
            VmState {
                post: Arc::clone(&options.post),
                should_interrupt: Arc::clone(&options.should_interrupt),
                clock,
                random,
                issued: 0,
                current: 0,
                host: None,
                undefined: 0,
            }
        }
    }

    // ---- 宿主回调内的内存读写 ----

    /// 栈上的宿主回调一律用 `i32` 表示 wasm 指针（wasm 的 i32 在 Rust 侧就是 `i32`）。
    ///
    /// 取不到实例的 `memory` 导出时返回 `None`。返回 `i32` 的宿主回调（`host_call`/`bridge`、
    /// WASI 的文件与随机数导入）没有 `Result` 通道，调用方只能走保守分支（返回哨兵值/空串/
    /// `ERRNO_BADF`），不能 panic。`clock_time_get_import` 是例外：它返回
    /// `Result<i32, wasmtime::Error>`（非有限时钟的 `RangeError` 走 `Err`），但取不到 `memory`
    /// 时同样只能 `Ok(ERRNO_BADF)`。
    fn host_memory(caller: &mut Caller<'_, VmState>) -> Option<Memory> {
        match caller.get_export("memory") {
            Some(Extern::Memory(memory)) => Some(memory),
            _ => None,
        }
    }

    /// 读 4 字节小端 u32；越界或读失败时返回 0（`memory.read` 的错误被丢弃）。
    ///
    /// `offset` 由 guest 传入，负数一律截到 0。这里不做完整性校验：回调没有可回报错误的通道，
    /// 需要区分「读到 0」与「读失败」的调用方请用 [`read_bytes_at`]。
    fn read_u32_at(caller: &mut Caller<'_, VmState>, memory: &Memory, offset: i32) -> u32 {
        let mut bytes = [0u8; 4];
        let _ = memory.read(&mut *caller, offset.max(0) as usize, &mut bytes);
        u32::from_le_bytes(bytes)
    }

    /// 写 4 字节小端；越界或写失败被静默丢弃（原因同上）。
    fn write_u32_at(caller: &mut Caller<'_, VmState>, memory: &Memory, offset: i32, value: u32) {
        let _ = memory.write(&mut *caller, offset.max(0) as usize, &value.to_le_bytes());
    }

    /// 写 8 字节小端（WASI 时钟结果的布局，见模块文档）。
    fn write_u64_at(caller: &mut Caller<'_, VmState>, memory: &Memory, offset: i32, value: u64) {
        let _ = memory.write(&mut *caller, offset.max(0) as usize, &value.to_le_bytes());
    }

    /// 读一段定长字节；越界或读失败返回 `None`。
    ///
    /// 与上面的定长读写不同，这里把失败暴露给调用方：字符串内容是否完整会影响后续语义
    /// （`host_call_import` 因此把 `None` 当空串，而不是补零当成真实文本）。
    fn read_bytes_at(
        caller: &mut Caller<'_, VmState>,
        memory: &Memory,
        offset: i32,
        len: u32,
    ) -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; len as usize];
        memory
            .read(&mut *caller, offset.max(0) as usize, &mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// WTF-8 → UTF-8：把 QuickJS 字符串的字节解码成 Rust `String`。
    ///
    /// quickjs-wasi 的 `decodeWtf8` 把孤立代理项（`0xED 0xA0..0xBF 0x80..0xBF`）解回 JS 字符串
    /// 里的**单个**代理项码元；Rust 的 `String` 不是 WTF-8，装不下代理项，所以按
    /// `kiana_json::js_parse` 同一条既定策略（ARCHITECTURE.md §8 第 31 条：孤立代理项 → U+FFFD）
    /// 把每个代理项换成**一个** U+FFFD。
    ///
    /// 不能直接用 `String::from_utf8_lossy`：它按非法**字节**替换，一个 3 字节的代理项会变成
    /// 3 个 U+FFFD，文本长度进一步偏离既定策略。合法 UTF-8（含 4 字节的星平面字符）逐字节一致。
    fn decode_wtf8(bytes: &[u8]) -> String {
        // 快路径：没有 `ED A0..BF` 开头的代理项序列时，标准 UTF-8 解码已经正确。
        if !bytes
            .windows(2)
            .any(|pair| pair[0] == 0xED && (0xA0..=0xBF).contains(&pair[1]))
        {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut text = String::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            let first = bytes[index];
            let width = match first {
                0x00..=0x7F => 1,
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => 0,
            };
            let mut code = 0u32;
            let valid = width > 0
                && index + width <= bytes.len()
                && bytes[index + 1..index + width]
                    .iter()
                    .all(|byte| (0x80..=0xBF).contains(byte));
            if valid {
                code = match width {
                    1 => u32::from(first),
                    2 => (u32::from(first & 0x1F) << 6) | u32::from(bytes[index + 1] & 0x3F),
                    3 => {
                        (u32::from(first & 0x0F) << 12)
                            | (u32::from(bytes[index + 1] & 0x3F) << 6)
                            | u32::from(bytes[index + 2] & 0x3F)
                    }
                    _ => {
                        (u32::from(first & 0x07) << 18)
                            | (u32::from(bytes[index + 1] & 0x3F) << 12)
                            | (u32::from(bytes[index + 2] & 0x3F) << 6)
                            | u32::from(bytes[index + 3] & 0x3F)
                    }
                };
            }
            // 代理项与超范围码点（`char::from_u32` 拒绝）各换一个 U+FFFD。
            match char::from_u32(code).filter(|_| valid) {
                Some(ch) => text.push(ch),
                None => text.push(char::REPLACEMENT_CHARACTER),
            }
            // 非法序列只前进一个字节（与 `TextDecoder` 一样不吞掉后续的 ASCII 字节）。
            index += if valid { width } else { 1 };
        }
        text
    }

    /// 读 `(offset, len)` 指定的一段 wasm 内存，并按 WTF-8 解码成 `String`
    /// （`host_call` 传入的函数名走这里）。
    ///
    /// `len < 0` 按 0 处理；越界/读失败返回 `None`，不从这段内存里猜内容。
    fn read_mem_string(
        caller: &mut Caller<'_, VmState>,
        memory: &Memory,
        offset: i32,
        len: i32,
    ) -> Option<String> {
        let bytes = read_bytes_at(caller, memory, offset, len.max(0) as u32)?;
        Some(decode_wtf8(&bytes))
    }

    /// `JSValueHandle::toString()`：长度感知的 WTF-8 读取。
    ///
    /// 孤立代理项在 WTF-8 里是 `0xED 0xA0..0xBF` 开头的 3 字节序列；Rust 的 `String` 装不下
    /// 代理项，按 [`decode_wtf8`] 换成一个 U+FFFD（spec 附录 A.6 同族偏差）。
    pub(super) fn host_to_string(caller: &mut Caller<'_, VmState>, ptr: i32) -> String {
        let Some(api) = caller.data().host.clone() else {
            return String::new();
        };
        let Some(memory) = host_memory(caller) else {
            return String::new();
        };
        let Ok(len_ptr) = api.wasm_malloc.call(&mut *caller, 4) else {
            return String::new();
        };
        if len_ptr == 0 {
            return String::new();
        }
        let cstr = match api.qjs_get_string_len.call(&mut *caller, (ptr, len_ptr)) {
            Ok(value) => value,
            Err(_) => {
                let _ = api.wasm_free.call(&mut *caller, len_ptr);
                return String::new();
            }
        };
        if cstr == 0 {
            let _ = api.wasm_free.call(&mut *caller, len_ptr);
            return "<null>".to_string();
        }
        let len = read_u32_at(caller, &memory, len_ptr);
        let bytes = read_bytes_at(caller, &memory, cstr, len).unwrap_or_default();
        let _ = api.qjs_free_cstring.call(&mut *caller, cstr);
        let _ = api.wasm_free.call(&mut *caller, len_ptr);
        decode_wtf8(&bytes)
    }

    /// 以下四个是 `HostApi` 的薄包装：wasm 调用失败时一律给保守值（`undefined` 判定为 false、
    /// 数字为 `NaN`、布尔为 false、释放静默跳过），因为宿主回调里没有别的失败通道。
    ///
    /// 实参缺失（`argc` 不够）在 [`bridge_call`] 上游已经按 JS 的 `undefined` 处理，不依赖这里。
    fn host_is_undefined(caller: &mut Caller<'_, VmState>, ptr: i32) -> bool {
        match caller.data().host.clone() {
            Some(api) => api
                .qjs_is_undefined
                .call(&mut *caller, ptr)
                .map(|value| value != 0)
                .unwrap_or(false),
            None => false,
        }
    }

    fn host_to_number(caller: &mut Caller<'_, VmState>, ptr: i32) -> f64 {
        match caller.data().host.clone() {
            Some(api) => api
                .qjs_get_float64
                .call(&mut *caller, ptr)
                .unwrap_or(f64::NAN),
            None => f64::NAN,
        }
    }

    fn host_to_bool(caller: &mut Caller<'_, VmState>, ptr: i32) -> bool {
        match caller.data().host.clone() {
            Some(api) => api
                .qjs_get_bool
                .call(&mut *caller, ptr)
                .map(|value| value != 0)
                .unwrap_or(false),
            None => false,
        }
    }

    fn host_free(caller: &mut Caller<'_, VmState>, ptr: i32) {
        if let Some(api) = caller.data().host.clone() {
            let _ = api.qjs_free_value.call(&mut *caller, ptr);
        }
    }

    /// 把一段文本写进 wasm 内存并生成 guest 字符串值（`writeString` + `qjs_new_string`）。
    fn host_new_string(caller: &mut Caller<'_, VmState>, text: &str) -> Option<i32> {
        let api = caller.data().host.clone()?;
        let memory = host_memory(caller)?;
        let bytes = text.as_bytes();
        let ptr = api
            .wasm_malloc
            .call(&mut *caller, bytes.len() as i32 + 1)
            .ok()?;
        if ptr == 0 {
            return None;
        }
        let mut buffer = bytes.to_vec();
        buffer.push(0);
        let _ = memory.write(&mut *caller, ptr as usize, &buffer);
        let value = api
            .qjs_new_string
            .call(&mut *caller, (ptr, bytes.len() as i32))
            .ok();
        let _ = api.wasm_free.call(&mut *caller, ptr);
        value
    }

    /// `newError(message)`：造一个 `Error`，把 `message` 设成自有属性。
    fn host_new_error(caller: &mut Caller<'_, VmState>, message: &str) -> Option<i32> {
        let api = caller.data().host.clone()?;
        let Ok(error) = api.qjs_new_error.call(&mut *caller, ()) else {
            return None;
        };
        let Some(message_value) = host_new_string(caller, message) else {
            return Some(error);
        };
        let Some(memory) = host_memory(caller) else {
            host_free(caller, message_value);
            return Some(error);
        };
        let Ok(key_ptr) = api.wasm_malloc.call(&mut *caller, 8) else {
            host_free(caller, message_value);
            return Some(error);
        };
        if key_ptr != 0 {
            let _ = memory.write(&mut *caller, key_ptr as usize, b"message\0");
            let _ = api
                .qjs_set_prop_string
                .call(&mut *caller, (error, key_ptr, message_value));
            let _ = api.wasm_free.call(&mut *caller, key_ptr);
        }
        host_free(caller, message_value);
        Some(error)
    }

    /// `handleHostCall` 的错误路径：把消息作为 guest 的 `Error` 抛出，返回 `0`。
    fn host_throw(caller: &mut Caller<'_, VmState>, message: &str) -> i32 {
        let Some(api) = caller.data().host.clone() else {
            return 0;
        };
        let Some(error) = host_new_error(caller, message) else {
            return 0;
        };
        let _ = api.qjs_throw.call(&mut *caller, error);
        host_free(caller, error);
        0
    }

    /// 取 undefined 的 **dup**。
    ///
    /// `host_call` 的返回值归 C 跳板所有（它负责释放），所以这里给出一份新的引用；缓存的
    /// 单例本身绝不释放（见 [`VmState::undefined`]）。
    fn host_return_undefined(caller: &mut Caller<'_, VmState>) -> i32 {
        let Some(api) = caller.data().host.clone() else {
            return 0;
        };
        let undefined = caller.data().undefined;
        if undefined == 0 {
            return 0;
        }
        api.qjs_dup_value.call(&mut *caller, undefined).unwrap_or(0)
    }

    /// 第 `index` 个实参的指针；缺省（`argc` 不够）时是 `None`（JS 侧的 `undefined`）。
    fn call_argument(
        caller: &mut Caller<'_, VmState>,
        memory: &Memory,
        argv_ptr: i32,
        argc: i32,
        index: i32,
    ) -> Option<i32> {
        if index >= argc || argv_ptr == 0 {
            return None;
        }
        Some(read_u32_at(caller, memory, argv_ptr + index * 4) as i32)
    }

    // ---- guest 值（`JSValueHandle` 的堆盒） ----

    /// `JSException`：guest 抛出的错误，读出的三个字段。
    ///
    /// TS: `JSException`（`name` 缺省为 `"Error"`，`message` 缺省退回 `handle.toString()`，
    /// `stack` 未定义时为 `undefined`）。
    pub(super) struct JsException {
        pub(super) name: String,
        pub(super) message: String,
        pub(super) stack: Option<String>,
    }

    /// 宿主侧的 `Error`（VM 回调之外的失败），[`crash_message`] 拼成 `Name: message`。
    pub(super) struct HostFailure {
        pub(super) name: &'static str,
        pub(super) message: String,
    }

    impl HostFailure {
        fn new(message: impl Into<String>) -> Self {
            HostFailure {
                name: "Error",
                message: message.into(),
            }
        }
    }

    /// VM 里发生的一次失败：guest 异常或宿主侧错误。
    pub(super) enum Failure {
        /// guest 抛出的错误（可能带栈）。
        Js(JsException),
        /// 宿主侧的错误（`new Error("…")`）。
        Host(HostFailure),
    }

    impl Failure {
        /// TS 的 `crashMessage(error)`：`error instanceof Error ? name + ": " + message : String(error)`。
        ///
        /// 这里两支都是 `Error`，所以总是 `Name: message`。
        pub(super) fn crash_text(&self) -> String {
            match self {
                Failure::Js(error) => format!("{}: {}", error.name, error.message),
                Failure::Host(error) => format!("{}: {}", error.name, error.message),
            }
        }
    }

    /// `String(number)`（模板字面量里的 `${x}`）：`NaN`/`Infinity` 有各自的文本，
    /// 其余与 `JSON.stringify` 的数字格式一致。
    fn js_number_to_string(value: f64) -> String {
        if value.is_nan() {
            return "NaN".to_string();
        }
        if value.is_infinite() {
            return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
        }
        kiana_json::js_num::format_js_number(value)
    }

    // ---- `bridge` 宿主函数 ----

    /// `bridge(kind, a, b, c)`：prelude 与宿主之间**唯一**的通道。
    ///
    /// TS: `startVm` 里的 `vm.newFunction("bridge", …)`。
    fn bridge_call(caller: &mut Caller<'_, VmState>, argc: i32, argv_ptr: i32) -> i32 {
        let Some(memory) = host_memory(caller) else {
            return host_throw(caller, "Host callback \"bridge\" is not registered");
        };
        let Some(kind_handle) = call_argument(caller, &memory, argv_ptr, argc, 0) else {
            return host_throw(caller, "Host callback \"bridge\" is not registered");
        };
        // `kind.toString()`；后面的分支还会再读一次（TS 的 `kind.toString() === "call"`）。
        let kind = host_to_string(caller, kind_handle);
        let a = call_argument(caller, &memory, argv_ptr, argc, 1);
        let b = call_argument(caller, &memory, argv_ptr, argc, 2);
        let c = call_argument(caller, &memory, argv_ptr, argc, 3);
        let is_undefined = |caller: &mut Caller<'_, VmState>, value: Option<i32>| match value {
            None => true,
            Some(ptr) => host_is_undefined(caller, ptr),
        };
        // 实参约定（prelude 只发 `call`/`global`/`output`/`done` 四种 kind）：
        // - 缺失实参按 JS 的 `undefined` 处理（`call_argument` 返回 `None`）；
        // - 需要文本而值缺失时退化成空串，而不是抛错；
        // - 未识别的 kind 静默忽略，仍返回 `undefined`。
        match kind.as_str() {
            "call" | "global" => {
                let id = a.map(|ptr| host_to_number(caller, ptr)).unwrap_or(f64::NAN);
                let name = b.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default();
                let args = if is_undefined(caller, c) {
                    None
                } else {
                    Some(c.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default())
                };
                let target = if kind == "call" {
                    CallTarget::Tool
                } else {
                    CallTarget::Global
                };
                let state = caller.data_mut();
                state.issued = state.issued.wrapping_add(1);
                let post = Arc::clone(&state.post);
                post(WorkerToHostMessage::Call {
                    id: js_to_uint64(id),
                    target,
                    name,
                    args,
                });
            }
            "output" => {
                let output_kind = a.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default();
                let data = b.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default();
                let item = if output_kind == "image" {
                    let mime = c.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default();
                    CodemodeOutputItem::image(data, mime)
                } else if output_kind == "console" {
                    CodemodeOutputItem::console(data)
                } else {
                    CodemodeOutputItem::text(data)
                };
                let post = Arc::clone(&caller.data().post);
                post(WorkerToHostMessage::Output { item });
            }
            "done" => {
                let during = caller.data().clock.map(|_| CodemodeDuring {
                    seq: caller.data().current,
                    issued: caller.data().issued,
                });
                let ok = a.map(|ptr| host_to_bool(caller, ptr)).unwrap_or(false);
                let message = if ok {
                    let value = if is_undefined(caller, b) {
                        None
                    } else {
                        Some(b.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default())
                    };
                    let writes = c.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default();
                    WorkerToHostMessage::done_ok(value, writes, during)
                } else {
                    let error = b.map(|ptr| host_to_string(caller, ptr)).unwrap_or_default();
                    WorkerToHostMessage::done_error(error, during)
                };
                let post = Arc::clone(&caller.data().post);
                post(message);
            }
            _ => {}
        }
        host_return_undefined(caller)
    }

    /// `env.host_call`：QuickJS 调用宿主函数时的跳板。
    ///
    /// TS: `QuickJS.handleHostCall`。唯一注册的回调名是 [`BRIDGE_NAME`]；未注册的名字按 TS
    /// 的文案抛进 guest。
    fn host_call_import(
        mut caller: Caller<'_, VmState>,
        name_ptr: i32,
        name_len: i32,
        _this_ptr: i32,
        argc: i32,
        argv_ptr: i32,
    ) -> i32 {
        let Some(memory) = host_memory(&mut caller) else {
            return 0;
        };
        let name = read_mem_string(&mut caller, &memory, name_ptr, name_len).unwrap_or_default();
        if name != BRIDGE_NAME {
            let message = format!(
                "Host callback \"{name}\" is not registered: it was unregistered, its ephemeral function handle was disposed, or it was never re-registered after a snapshot restore."
            );
            return host_throw(&mut caller, &message);
        }
        bridge_call(&mut caller, argc, argv_ptr)
    }

    /// `env.host_interrupt`：QuickJS 在脚本运行时轮询。
    fn host_interrupt_import(caller: Caller<'_, VmState>) -> i32 {
        let should_interrupt = Arc::clone(&caller.data().should_interrupt);
        i32::from(should_interrupt())
    }

    /// `env.host_promise_rejection`：没有注册 handler 时由宿主释放两个值（TS 的同名分支）。
    fn host_promise_rejection_import(
        mut caller: Caller<'_, VmState>,
        promise_ptr: i32,
        reason_ptr: i32,
        _is_handled: i32,
    ) {
        let Some(api) = caller.data().host.clone() else {
            return;
        };
        let _ = api.qjs_free_value.call(&mut caller, promise_ptr);
        let _ = api.qjs_free_value.call(&mut caller, reason_ptr);
    }

    /// `env.host_module_normalize`：没有 normalize handler 时返回 specifier 的副本（malloc 的结果，
    /// 调用方负责 free）。
    fn host_module_normalize_import(
        mut caller: Caller<'_, VmState>,
        _base_name_ptr: i32,
        name_ptr: i32,
    ) -> i32 {
        let Some(api) = caller.data().host.clone() else {
            return 0;
        };
        let Some(memory) = host_memory(&mut caller) else {
            return 0;
        };
        let mut bytes = Vec::new();
        let mut offset = name_ptr.max(0) as usize;
        loop {
            let mut byte = [0u8; 1];
            if memory.read(&mut caller, offset, &mut byte).is_err() {
                break;
            }
            if byte[0] == 0 {
                break;
            }
            bytes.push(byte[0]);
            offset += 1;
        }
        let Ok(ptr) = api.wasm_malloc.call(&mut caller, bytes.len() as i32 + 1) else {
            return 0;
        };
        if ptr == 0 {
            return 0;
        }
        bytes.push(0);
        let _ = memory.write(&mut caller, ptr as usize, &bytes);
        ptr
    }

    /// `env.host_module_load`：没有 loader 时返回 `0`（TS 的同名分支）。
    fn host_module_load_import(
        _caller: Caller<'_, VmState>,
        _name_ptr: i32,
        _out_len_ptr: i32,
    ) -> i32 {
        0
    }

    /// `env.host_get_timezone_offset(hi, lo)`：UTC 偏移的**秒**数（正数在东）。
    ///
    /// 确定性模式固定 `timezoneOffset: 0` → 恒 `0`（`-0 * 60`）。
    fn host_get_timezone_offset_import(caller: Caller<'_, VmState>, hi: i32, lo: i32) -> i32 {
        if caller.data().clock.is_some() {
            return 0;
        }
        let seconds = ((i64::from(hi)) << 32) | i64::from(lo as u32);
        host_timezone_offset_seconds(seconds)
    }

    /// 宿主时区在 `unix_seconds` 时刻相对 UTC 的偏移（秒，正数在东）。
    ///
    /// `getTimezoneOffset()` 是「UTC − 本地」的分钟数，quickjs-wasi 取反并乘 60，等价于 libc 的
    /// `tm_gmtoff`。没有本地时区信息时（非 unix）返回 0，与 `TZ=UTC` 相同。
    ///
    /// # unsafe
    ///
    /// `localtime_r` 要求 `tm` 可写；`zeroed()` 得到全零的 `libc::tm`（纯数据，无析构），
    /// 调用成功后由 libc 填充（本函数只读 `tm_gmtoff`）。`time` 只在调用期间被借用，返回后
    /// 不再使用。返回空指针表示时区数据不可用，此时不读 `tm` 的任何字段。
    #[cfg(unix)]
    fn host_timezone_offset_seconds(unix_seconds: i64) -> i32 {
        let time: libc::time_t = unix_seconds as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&time, &mut tm) };
        if result.is_null() {
            return 0;
        }
        tm.tm_gmtoff as i32
    }

    /// 非 unix 宿主：没有本地时区数据库可用，按 UTC 处理。
    #[cfg(not(unix))]
    fn host_timezone_offset_seconds(_unix_seconds: i64) -> i32 {
        0
    }

    // ---- WASI preview1 子集 ----

    /// `wasi_snapshot_preview1.clock_time_get`：虚拟时钟或宿主时钟，写**小端 u64** ns。
    ///
    /// TS: `deterministicWasi(clock).clock_time_get` 与 `createWasiShim` 的同名函数。
    ///
    /// 确定性模式下 `BigInt(Math.round(clock.now))` 在 `now` 是 `NaN`/`±Infinity` 时抛
    /// `RangeError`（非有限值只能来自非有限的 `startedAt`）。这个异常从 WASI 导入函数抛出，
    /// QuickJS 的 C 代码接不住，于是整个 wasm 调用中止、异常冒到宿主——这里用
    /// [`HostThrown`] + wasmtime 的宿主错误复刻同样的路径：guest 的 `try/catch` 接不住，
    /// `startVm`/`deliver` 把它变成 `crash`。
    fn clock_time_get_import(
        mut caller: Caller<'_, VmState>,
        clock_id: i32,
        _precision: i64,
        result_ptr: i32,
    ) -> Result<i32, wasmtime::Error> {
        let Some(memory) = host_memory(&mut caller) else {
            return Ok(ERRNO_BADF);
        };
        let nanoseconds = match caller.data().clock {
            Some(clock) => {
                if clock.creating {
                    // QuickJS 用这次读数（微秒）播种 Math.random()，并把 0 变成 1，所以 seed s 读 s + 1。
                    if clock_id == CLOCK_REALTIME {
                        Ok((u64::from(clock.seed) + 1).wrapping_mul(1_000))
                    } else {
                        Ok(0)
                    }
                } else if clock_id == CLOCK_REALTIME {
                    nanos_from_millis(clock.now)
                } else {
                    // TS 对 `clockId !== 0` 一律按 monotonic 处理（只看 `clock.startedAt`）。
                    nanos_from_millis(clock.now - clock.started_at)
                }
            }
            None => {
                // 非确定性模式两个时钟都读宿主墙钟（这里没有单调时钟源）；其余 clock id 按
                // WASI 的约定返回 NOSYS。
                if clock_id == CLOCK_REALTIME || clock_id == CLOCK_MONOTONIC {
                    nanos_from_millis(wall_clock_millis())
                } else {
                    return Ok(ERRNO_NOSYS);
                }
            }
        };
        let nanoseconds = match nanoseconds {
            Ok(value) => value,
            Err(error) => {
                return Err(wasmtime::Error::new(HostThrown {
                    name: "RangeError",
                    message: error.message(),
                }));
            }
        };
        write_u64_at(&mut caller, &memory, result_ptr, nanoseconds);
        Ok(ERRNO_SUCCESS)
    }

    /// `wasi_snapshot_preview1.random_get`：确定性 PRNG 或宿主熵，按 4 字节一组小端写。
    ///
    /// TS: `deterministicWasi(clock).random_get` 与 `createWasiShim` 的同名函数。
    fn random_get_import(mut caller: Caller<'_, VmState>, buf_ptr: i32, buf_len: i32) -> i32 {
        let Some(memory) = host_memory(&mut caller) else {
            return ERRNO_BADF;
        };
        let len = buf_len.max(0) as u32;
        let mut bytes = vec![0u8; len as usize];
        if let Some(random) = caller.data_mut().random.as_mut() {
            // 每 4 字节取一个 u32，按小端铺开；最后一组按剩余长度截断。字节序是确定性的
            // 一部分：同样的种子必须给 QuickJS 同样的字节流。
            let mut index = 0usize;
            while index < bytes.len() {
                let value = random.next_u32();
                for byte in 0..4 {
                    if index + byte >= bytes.len() {
                        break;
                    }
                    bytes[index + byte] = (value >> (byte * 8)) as u8;
                }
                index += 4;
            }
        } else {
            os_random(&mut bytes);
        }
        let _ = memory.write(&mut caller, buf_ptr.max(0) as usize, &bytes);
        ERRNO_SUCCESS
    }

    /// 宿主随机源：unix 读 `/dev/urandom`，其余平台用时钟 + 计数器的 PRNG。
    ///
    /// TS 用 `crypto.getRandomValues`（见 `createWasiShim` 的 `random_get`）。这里没有
    /// 密码学要求：`random_get` 只服务于 WASI libc 的初始化与 QuickJS 的内部种子（
    /// `Math.random()` 真正的种子来自时钟，见 `clock_time_get`）。
    #[cfg(unix)]
    fn os_random(bytes: &mut [u8]) {
        use std::io::Read;
        if let Ok(mut file) = std::fs::File::open("/dev/urandom")
            && file.read_exact(bytes).is_ok()
        {
            return;
        }
        fallback_random(bytes);
    }

    /// 没有 `/dev/urandom` 时的退化实现（时钟 + 进程内计数）。
    #[cfg(not(unix))]
    fn os_random(bytes: &mut [u8]) {
        fallback_random(bytes);
    }

    /// 时钟与计数器混出的伪随机字节（**不是**密码学安全的，只用来让 VM 拿到变化的字节）。
    fn fallback_random(bytes: &mut [u8]) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        let tick = COUNTER.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
        let mut random = Prng::new((nanos ^ tick) as u32);
        for slot in bytes.chunks_mut(4) {
            let value = random.next_u32();
            for (byte, out) in slot.iter_mut().enumerate() {
                *out = (value >> (byte * 8)) as u8;
            }
        }
    }

    /// `wasi_snapshot_preview1.fd_write`：丢弃引擎诊断输出，但如实回报写入字节数。
    ///
    /// TS: `discardOutput(memory).fd_write`（**不检查 fd**：它盖住了 WASI 内建的 BADF 分支）。
    fn fd_write_import(
        mut caller: Caller<'_, VmState>,
        _fd: i32,
        iovs_ptr: i32,
        iovs_len: i32,
        nwritten_ptr: i32,
    ) -> i32 {
        let Some(memory) = host_memory(&mut caller) else {
            return ERRNO_BADF;
        };
        let mut written = 0u32;
        // iovec 是每 8 字节的 `(ptr: u32, len: u32)`；输出直接丢弃，所以只累加 len、不读 ptr，
        // 也不检查 fd。
        for index in 0..iovs_len.max(0) {
            let len = read_u32_at(&mut caller, &memory, iovs_ptr + index * 8 + 4);
            written = written.wrapping_add(len);
        }
        write_u32_at(&mut caller, &memory, nwritten_ptr, written);
        ERRNO_SUCCESS
    }

    /// `wasi_snapshot_preview1.fd_close`：TS 的桩返回 `ERRNO_NOSYS`。
    fn fd_close_import(_caller: Caller<'_, VmState>, _fd: i32) -> i32 {
        ERRNO_NOSYS
    }

    /// `wasi_snapshot_preview1.fd_fdstat_get`：fd 1/2 是字符设备，其余 `ERRNO_BADF`。
    fn fd_fdstat_get_import(mut caller: Caller<'_, VmState>, fd: i32, stat_ptr: i32) -> i32 {
        let Some(memory) = host_memory(&mut caller) else {
            return ERRNO_BADF;
        };
        if fd != 1 && fd != 2 {
            return ERRNO_BADF;
        }
        let base = stat_ptr as usize;
        // wasi_snapshot_preview1 的 fdstat 布局：0 是 filetype(u8)，2 是 fs_flags(u16)，
        // 8 / 16 是 rights_base / rights_inheriting(u64)；其余字节保持 wasm 内存原样。
        let _ = memory.write(&mut caller, base, &[FILETYPE_CHARACTER_DEVICE]);
        let _ = memory.write(&mut caller, base + 2, &0u16.to_le_bytes());
        let _ = memory.write(&mut caller, base + 8, &0u64.to_le_bytes());
        let _ = memory.write(&mut caller, base + 16, &0u64.to_le_bytes());
        ERRNO_SUCCESS
    }

    /// `wasi_snapshot_preview1.fd_seek`：TS 的桩返回 `ERRNO_NOSYS`。
    fn fd_seek_import(
        _caller: Caller<'_, VmState>,
        _fd: i32,
        _offset: i64,
        _whence: i32,
        _result_ptr: i32,
    ) -> i32 {
        ERRNO_NOSYS
    }

    // ---- 导出表 ----

    /// `QuickJSExports` 里本模块用到的导出（wasm 的 i32 参数在 Rust 侧就是 `i32`）。
    pub(super) struct Exports {
        pub(super) memory: Memory,
        qjs_get_quickjs_version: TypedFunc<(), i32>,
        qjs_eval: TypedFunc<(i32, i32, i32, i32), i32>,
        qjs_new_string: TypedFunc<(i32, i32), i32>,
        qjs_new_number: TypedFunc<f64, i32>,
        qjs_get_undefined: TypedFunc<(), i32>,
        qjs_get_true: TypedFunc<(), i32>,
        qjs_get_false: TypedFunc<(), i32>,
        qjs_get_float64: TypedFunc<i32, f64>,
        qjs_get_string_len: TypedFunc<(i32, i32), i32>,
        qjs_free_cstring: TypedFunc<i32, ()>,
        qjs_is_exception: TypedFunc<i32, i32>,
        qjs_is_undefined: TypedFunc<i32, i32>,
        qjs_get_bool: TypedFunc<i32, i32>,
        qjs_get_prop_string: TypedFunc<(i32, i32), i32>,
        qjs_set_prop_string: TypedFunc<(i32, i32, i32), i32>,
        qjs_call: TypedFunc<(i32, i32, i32, i32), i32>,
        qjs_new_host_function: TypedFunc<(i32, i32, i32), i32>,
        qjs_is_job_pending: TypedFunc<(), i32>,
        qjs_execute_pending_job: TypedFunc<(), i32>,
        qjs_get_exception: TypedFunc<(), i32>,
        qjs_new_error: TypedFunc<(), i32>,
        qjs_throw: TypedFunc<i32, i32>,
        qjs_set_memory_limit: TypedFunc<i32, ()>,
        qjs_set_max_stack_size: TypedFunc<i32, ()>,
        qjs_set_interrupt_handler: TypedFunc<i32, ()>,
        qjs_dup_value: TypedFunc<i32, i32>,
        qjs_free_value: TypedFunc<i32, ()>,
        wasm_malloc: TypedFunc<i32, i32>,
        wasm_free: TypedFunc<i32, ()>,
    }

    impl Exports {
        /// 取并类型检查全部导出；缺一个或签名不符都转成 `StartVmError`。
        ///
        /// 这是「传进来的模块是不是这份 quickjs.wasm」的唯一防线：除了本模块调用的函数，还检查
        /// `memory` 导出与 `__stack_pointer` 全局（后者只用于快照/恢复，这里只确认它存在，
        /// 没有它就不是这份 wasm）。
        fn load(store: &mut Store<VmState>, instance: &Instance) -> Result<Self, StartVmError> {
            fn missing(name: &str) -> StartVmError {
                StartVmError::new(format!("the QuickJS module does not export `{name}`"))
            }
            fn take(
                store: &mut Store<VmState>,
                instance: &Instance,
                name: &str,
            ) -> Result<Func, StartVmError> {
                match instance.get_export(&mut *store, name) {
                    Some(Extern::Func(func)) => Ok(func),
                    _ => Err(missing(name)),
                }
            }
            macro_rules! typed {
                ($name:ident, $params:ty, $results:ty) => {
                    take(store, instance, stringify!($name))?
                        .typed::<$params, $results>(&*store)
                        .map_err(|error| {
                            StartVmError::new(format!(
                                "the QuickJS module export `{}`: {error}",
                                stringify!($name)
                            ))
                        })?
                };
            }
            let memory = match instance.get_export(&mut *store, "memory") {
                Some(Extern::Memory(memory)) => memory,
                _ => return Err(missing("memory")),
            };
            // `__stack_pointer` 只在快照/恢复时用；这里确认它存在（无它就不是这份 wasm）。
            match instance.get_export(&mut *store, "__stack_pointer") {
                Some(Extern::Global(_)) => {}
                _ => return Err(missing("__stack_pointer")),
            }
            Ok(Exports {
                memory,
                qjs_get_quickjs_version: typed!(qjs_get_quickjs_version, (), i32),
                qjs_eval: typed!(qjs_eval, (i32, i32, i32, i32), i32),
                qjs_new_string: typed!(qjs_new_string, (i32, i32), i32),
                qjs_new_number: typed!(qjs_new_number, f64, i32),
                qjs_get_undefined: typed!(qjs_get_undefined, (), i32),
                qjs_get_true: typed!(qjs_get_true, (), i32),
                qjs_get_false: typed!(qjs_get_false, (), i32),
                qjs_get_float64: typed!(qjs_get_float64, i32, f64),
                qjs_get_string_len: typed!(qjs_get_string_len, (i32, i32), i32),
                qjs_free_cstring: typed!(qjs_free_cstring, i32, ()),
                qjs_is_exception: typed!(qjs_is_exception, i32, i32),
                qjs_is_undefined: typed!(qjs_is_undefined, i32, i32),
                qjs_get_bool: typed!(qjs_get_bool, i32, i32),
                qjs_get_prop_string: typed!(qjs_get_prop_string, (i32, i32), i32),
                qjs_set_prop_string: typed!(qjs_set_prop_string, (i32, i32, i32), i32),
                qjs_call: typed!(qjs_call, (i32, i32, i32, i32), i32),
                qjs_new_host_function: typed!(qjs_new_host_function, (i32, i32, i32), i32),
                qjs_is_job_pending: typed!(qjs_is_job_pending, (), i32),
                qjs_execute_pending_job: typed!(qjs_execute_pending_job, (), i32),
                qjs_get_exception: typed!(qjs_get_exception, (), i32),
                qjs_new_error: typed!(qjs_new_error, (), i32),
                qjs_throw: typed!(qjs_throw, i32, i32),
                qjs_set_memory_limit: typed!(qjs_set_memory_limit, i32, ()),
                qjs_set_max_stack_size: typed!(qjs_set_max_stack_size, i32, ()),
                qjs_set_interrupt_handler: typed!(qjs_set_interrupt_handler, i32, ()),
                qjs_dup_value: typed!(qjs_dup_value, i32, i32),
                qjs_free_value: typed!(qjs_free_value, i32, ()),
                wasm_malloc: typed!(wasm_malloc, i32, i32),
                wasm_free: typed!(wasm_free, i32, ()),
            })
        }

        /// 宿主回调里可用的克隆（`TypedFunc` 是轻量句柄）。
        fn host_api(&self) -> HostApi {
            HostApi {
                qjs_free_value: self.qjs_free_value.clone(),
                qjs_dup_value: self.qjs_dup_value.clone(),
                qjs_new_string: self.qjs_new_string.clone(),
                qjs_new_error: self.qjs_new_error.clone(),
                qjs_throw: self.qjs_throw.clone(),
                qjs_set_prop_string: self.qjs_set_prop_string.clone(),
                qjs_get_string_len: self.qjs_get_string_len.clone(),
                qjs_free_cstring: self.qjs_free_cstring.clone(),
                qjs_is_undefined: self.qjs_is_undefined.clone(),
                qjs_get_float64: self.qjs_get_float64.clone(),
                qjs_get_bool: self.qjs_get_bool.clone(),
                wasm_malloc: self.wasm_malloc.clone(),
                wasm_free: self.wasm_free.clone(),
            }
        }

        /// 读一段 NUL 结尾的 C 字符串（版本号、模块名）。
        fn read_c_string(&self, store: &mut Store<VmState>, ptr: i32) -> String {
            let mut bytes = Vec::new();
            let mut offset = ptr.max(0) as usize;
            loop {
                let mut byte = [0u8; 1];
                if self.memory.read(&mut *store, offset, &mut byte).is_err() {
                    break;
                }
                if byte[0] == 0 {
                    break;
                }
                bytes.push(byte[0]);
                offset += 1;
            }
            decode_wtf8(&bytes)
        }

        /// `JSValueHandle::toString()`（会话内版本，直接拿 `&mut Store`）。
        fn to_string(&self, store: &mut Store<VmState>, ptr: i32) -> String {
            let Ok(len_ptr) = self.wasm_malloc.call(&mut *store, 4) else {
                return String::new();
            };
            if len_ptr == 0 {
                return String::new();
            }
            let cstr = match self.qjs_get_string_len.call(&mut *store, (ptr, len_ptr)) {
                Ok(value) => value,
                Err(_) => {
                    let _ = self.wasm_free.call(&mut *store, len_ptr);
                    return String::new();
                }
            };
            if cstr == 0 {
                let _ = self.wasm_free.call(&mut *store, len_ptr);
                return "<null>".to_string();
            }
            let mut len_bytes = [0u8; 4];
            let _ = self
                .memory
                .read(&mut *store, len_ptr as usize, &mut len_bytes);
            let len = u32::from_le_bytes(len_bytes);
            let mut bytes = vec![0u8; len as usize];
            let _ = self.memory.read(&mut *store, cstr as usize, &mut bytes);
            let _ = self.qjs_free_cstring.call(&mut *store, cstr);
            let _ = self.wasm_free.call(&mut *store, len_ptr);
            decode_wtf8(&bytes)
        }

        fn is_undefined(&self, store: &mut Store<VmState>, ptr: i32) -> bool {
            self.qjs_is_undefined
                .call(&mut *store, ptr)
                .map(|value| value != 0)
                .unwrap_or(false)
        }

        fn free(&self, store: &mut Store<VmState>, ptr: i32) {
            if ptr != 0 {
                let _ = self.qjs_free_value.call(&mut *store, ptr);
            }
        }

        /// `vm.newString(text)`；失败返回 `0`（`wasm_malloc` 失败时 TS 抛 `Error`，宿主侧
        /// 会变成 `crash`）。
        fn new_string(&self, store: &mut Store<VmState>, text: &str) -> i32 {
            let bytes = text.as_bytes();
            let Ok(ptr) = self.wasm_malloc.call(&mut *store, bytes.len() as i32 + 1) else {
                return 0;
            };
            if ptr == 0 {
                return 0;
            }
            let mut buffer = bytes.to_vec();
            buffer.push(0);
            let _ = self.memory.write(&mut *store, ptr as usize, &buffer);
            let value = self
                .qjs_new_string
                .call(&mut *store, (ptr, bytes.len() as i32))
                .unwrap_or(0);
            let _ = self.wasm_free.call(&mut *store, ptr);
            value
        }

        /// 把一段文本的 NUL 结尾副本写进 wasm 内存，返回指针（调用方负责 `wasm_free`）。
        fn write_c_string(&self, store: &mut Store<VmState>, text: &str) -> i32 {
            let bytes = text.as_bytes();
            let Ok(ptr) = self.wasm_malloc.call(&mut *store, bytes.len() as i32 + 1) else {
                return 0;
            };
            if ptr == 0 {
                return 0;
            }
            let mut buffer = bytes.to_vec();
            buffer.push(0);
            let _ = self.memory.write(&mut *store, ptr as usize, &buffer);
            ptr
        }

        /// `vm.evalCode(code, filename)`：异常转 `Err(Failure::Js)`。
        fn eval(
            &self,
            store: &mut Store<VmState>,
            code: &str,
            filename: &str,
        ) -> Result<i32, Failure> {
            let bytes = code.as_bytes();
            let code_ptr = self.write_c_string(store, code);
            if code_ptr == 0 {
                return Err(Failure::Host(HostFailure::new("wasm_malloc failed")));
            }
            let name_ptr = self.write_c_string(store, filename);
            if name_ptr == 0 {
                let _ = self.wasm_free.call(&mut *store, code_ptr);
                return Err(Failure::Host(HostFailure::new("wasm_malloc failed")));
            }
            let result = self
                .qjs_eval
                .call(&mut *store, (code_ptr, bytes.len() as i32, name_ptr, 0))
                .map_err(wasm_failure);
            let _ = self.wasm_free.call(&mut *store, code_ptr);
            let _ = self.wasm_free.call(&mut *store, name_ptr);
            self.check(store, result?)
        }

        /// `vm.callFunction(func, this, …args)`：异常转 `Err(Failure::Js)`。
        fn call(
            &self,
            store: &mut Store<VmState>,
            func: i32,
            this: i32,
            args: &[i32],
        ) -> Result<i32, Failure> {
            let result = self.raw_call(store, func, this, args)?;
            self.check(store, result)
        }

        /// `callFunctionRaw`：不检查异常。
        ///
        /// 失败（trap、宿主抛错、`wasm_malloc` 失败）一律上抛——TS 里 `callFunctionRaw`
        /// 抛出的异常同样不会变成「无异常、返回 null 句柄」。
        fn raw_call(
            &self,
            store: &mut Store<VmState>,
            func: i32,
            this: i32,
            args: &[i32],
        ) -> Result<i32, Failure> {
            let argc = args.len() as i32;
            let mut argv_ptr = 0i32;
            if argc > 0 {
                let ptr = self
                    .wasm_malloc
                    .call(&mut *store, argc * 4)
                    .map_err(wasm_failure)?;
                if ptr == 0 {
                    return Err(Failure::Host(HostFailure::new("wasm_malloc failed")));
                }
                argv_ptr = ptr;
                let mut bytes = Vec::with_capacity(args.len() * 4);
                for arg in args {
                    bytes.extend_from_slice(&arg.to_le_bytes());
                }
                let _ = self.memory.write(&mut *store, ptr as usize, &bytes);
            }
            let result = self
                .qjs_call
                .call(&mut *store, (func, this, argc, argv_ptr))
                .map_err(wasm_failure);
            if argv_ptr != 0 {
                let _ = self.wasm_free.call(&mut *store, argv_ptr);
            }
            result
        }

        /// `throwIfException`：异常时取出 `qjs_get_exception()` 并读成 [`JsException`]。
        fn check(&self, store: &mut Store<VmState>, ptr: i32) -> Result<i32, Failure> {
            let is_exception = self
                .qjs_is_exception
                .call(&mut *store, ptr)
                .map_err(wasm_failure)?;
            if is_exception == 0 {
                return Ok(ptr);
            }
            self.free(store, ptr);
            let handle = self
                .qjs_get_exception
                .call(&mut *store, ())
                .map_err(wasm_failure)?;
            let parsed = self.read_exception(store, handle);
            self.free(store, handle);
            Err(Failure::Js(parsed))
        }

        /// `new JSException(handle)`：读 `name`/`message`/`stack`。
        ///
        /// 这里已经在错误路径上（QuickJS 已经返回一个异常句柄）：读属性时引擎若再失败，
        /// 按空值降级——`String(undefined)` 一类的兜底与 TS 的 `toString()` 结果一致。
        fn read_exception(&self, store: &mut Store<VmState>, handle: i32) -> JsException {
            let name_handle = self.get_prop(store, handle, "name").unwrap_or(0);
            let name = if name_handle == 0 || self.is_undefined(store, name_handle) {
                "Error".to_string()
            } else {
                self.to_string(store, name_handle)
            };
            self.free(store, name_handle);
            let message_handle = self.get_prop(store, handle, "message").unwrap_or(0);
            let message = if message_handle == 0 || self.is_undefined(store, message_handle) {
                self.to_string(store, handle)
            } else {
                self.to_string(store, message_handle)
            };
            self.free(store, message_handle);
            let stack_handle = self.get_prop(store, handle, "stack").unwrap_or(0);
            let stack = if stack_handle == 0 || self.is_undefined(store, stack_handle) {
                None
            } else {
                Some(self.to_string(store, stack_handle))
            };
            self.free(store, stack_handle);
            JsException {
                name,
                message,
                stack,
            }
        }

        /// `handle.getProp(name)`。
        ///
        /// `wasm_malloc` 失败返回 `Ok(0)`（TS 会抛 `Error: wasm_malloc failed`，调用方按空句柄
        /// 处理即可）；wasm 调用失败则上抛。
        fn get_prop(
            &self,
            store: &mut Store<VmState>,
            object: i32,
            name: &str,
        ) -> Result<i32, wasmtime::Error> {
            let ptr = self.write_c_string(store, name);
            if ptr == 0 {
                return Ok(0);
            }
            let value = self.qjs_get_prop_string.call(&mut *store, (object, ptr));
            let _ = self.wasm_free.call(&mut *store, ptr);
            value
        }
    }

    // ---- 错误文本 ----

    /// `describeException(error)`：`done` 的失败载荷。
    ///
    /// TS: `describeException`（`JSON.stringify({ name, message, stack })`，`stack` 在
    /// `trimEnd` 后非空时是 `head + "\n" + stack`，否则是 `head`；`head` 在 `message` 为空时
    /// 只有 `name`）。
    ///
    /// 返回值是 JSON **文本**，直接放进 `done` 的 `error` 字段（协议里是 `String`）。没有可用
    /// 的 `stack` 时，JSON 里的 `stack` 键就是 `head`（`name`，或 `name: message`）；
    /// `done.error` 始终是整段 JSON 文本（含 `name`/`message`/`stack` 三个键），不是其中
    /// 某一个字段。
    pub(super) fn describe_exception(error: &JsException) -> String {
        let head = if error.message.is_empty() {
            error.name.clone()
        } else {
            format!("{}: {}", error.name, error.message)
        };
        let trimmed = error.stack.as_deref().map(js_trim_end);
        let stack = match trimmed {
            Some(text) if !text.is_empty() => format!("{head}\n{text}"),
            _ => head,
        };
        let mut map = serde_json::Map::new();
        map.insert(
            "name".to_string(),
            serde_json::Value::String(error.name.clone()),
        );
        map.insert(
            "message".to_string(),
            serde_json::Value::String(error.message.clone()),
        );
        map.insert("stack".to_string(), serde_json::Value::String(stack));
        kiana_json::js_object::stringify(&serde_json::Value::Object(map))
    }

    /// 一次失败对应的 `crash` 载荷文本：`Name: message`。
    pub(super) fn crash_message(failure: &Failure) -> String {
        failure.crash_text()
    }

    /// wasm 调用失败（trap / 宿主抛错 / 类型不匹配）的 `crash` 文本。
    ///
    /// 三种来源：
    /// - [`HostThrown`]：WASI 导入「抛」出的异常（如非有限时钟的 `RangeError`），guest 接不住，
    ///   与 TS 的宿主异常同路径；
    /// - [`wasmtime::Trap`]：wasm 帧自己失败。V8（TS 的宿主）把 wasm 栈耗尽报成
    ///   `RangeError: Maximum call stack size exceeded`、把越界内存访问报成
    ///   `RuntimeError: memory access out of bounds`；这里按同样的名字与文案翻译，避免
    ///   wasmtime 的 `wasm trap: …` 前缀泄漏给脚本层；
    /// - 其余（实例化/类型不匹配等）：`RuntimeError: <压平后的错误链>`。
    fn wasm_failure(error: wasmtime::Error) -> Failure {
        if let Some(thrown) = error.downcast_ref::<HostThrown>() {
            return Failure::Host(HostFailure {
                name: thrown.name,
                message: thrown.message.clone(),
            });
        }
        if let Some(trap) = error.downcast_ref::<wasmtime::Trap>() {
            let name = match trap {
                wasmtime::Trap::StackOverflow => "RangeError",
                _ => "RuntimeError",
            };
            return Failure::Host(HostFailure {
                name,
                message: trap_message(trap),
            });
        }
        Failure::Host(HostFailure {
            name: "RuntimeError",
            message: collapse_whitespace(&format!("{error:#}")),
        })
    }

    /// trap 的 V8 文案（`WebAssembly.RuntimeError: …` 的 message 部分）。
    ///
    /// 只翻译能出现的几种；其余保留 wasmtime 的描述（脚本层本来也不该看到引擎内部文案，
    /// 这里宁可如实报告引擎的限制）。
    fn trap_message(trap: &wasmtime::Trap) -> String {
        match trap {
            wasmtime::Trap::StackOverflow => "Maximum call stack size exceeded".to_string(),
            wasmtime::Trap::MemoryOutOfBounds => "memory access out of bounds".to_string(),
            wasmtime::Trap::TableOutOfBounds => "table index is out of bounds".to_string(),
            wasmtime::Trap::IndirectCallToNull | wasmtime::Trap::BadSignature => {
                "null function or function signature mismatch".to_string()
            }
            wasmtime::Trap::UnreachableCodeReached => "unreachable".to_string(),
            wasmtime::Trap::IntegerDivisionByZero => "divide by zero".to_string(),
            wasmtime::Trap::IntegerOverflow => "divide result unrepresentable".to_string(),
            wasmtime::Trap::BadConversionToInteger => "invalid conversion to integer".to_string(),
            wasmtime::Trap::HeapMisaligned => "unaligned atomic".to_string(),
            other => collapse_whitespace(&other.to_string()),
        }
    }

    /// 把多行错误压成一行（`Display` 的 `{:#}` 会带上下文链）。
    fn collapse_whitespace(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut pending_space = false;
        for ch in text.chars() {
            if ch.is_whitespace() {
                pending_space = true;
                continue;
            }
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        }
        out
    }

    /// 把一次 wasm 调用失败转成 [`StartVmError`]（`QuickJS.create` 期间的失败都走这里）。
    fn start_error(error: wasmtime::Error) -> StartVmError {
        StartVmError::new(collapse_whitespace(&format!("{error:#}")))
    }

    /// `JSON.stringify(value)`：走 JS 的数字格式与整数键序（ARCHITECTURE §4.3）。
    ///
    /// tools / globals / store 三份数据都经它变成 prelude 的字符串实参。`to_value` 失败时退回
    /// `null` 文本，而不是让启动段 panic。
    fn js_stringify<T: serde::Serialize>(value: &T) -> String {
        match serde_json::to_value(value) {
            Ok(value) => kiana_json::js_object::stringify(&value),
            Err(_) => "null".to_string(),
        }
    }

    // ---- 会话 ----

    /// 一个绑定到某次脚本运行的 VM。
    ///
    /// TS: `QuickJS` 实例 + `startVm` 闭包里的状态（`issued`、`nextSeq`、`current`、`early`）。
    pub(super) struct Session {
        store: Store<VmState>,
        exports: Exports,
        /// `bridge` 宿主函数（prelude 闭包引用它，活得和 VM 一样长）。
        bridge: i32,
        /// prelude 返回的对象 `{ settle, run, stalled }`。
        api: i32,
        settle: i32,
        stalled: i32,
        /// `api`/`settle`/`stalled` 是否都已拿到。
        ///
        /// TS 里它们是 `let` 变量：`prelude` 求值失败时仍是 `undefined`，此后 `deliver` 调用
        /// `vm.callFunction(settle, api, …)` 会先读 `settle.ptr`，抛
        /// `TypeError: Cannot read properties of undefined (reading 'ptr')`。
        ready: bool,
        disposed: bool,
        /// 确定性模式（投递按 seq 排队，drain 后报 `idle`）。
        deterministic: bool,
        /// 下一个要应用的结果号（确定性模式）。
        next_seq: u64,
        /// 早到、还在等前序结果的消息（确定性模式）。
        early: BTreeMap<u64, HostResult>,
    }

    impl fmt::Debug for Session {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Session")
                .field("disposed", &self.disposed)
                .field("deterministic", &self.deterministic)
                .field("next_seq", &self.next_seq)
                .finish_non_exhaustive()
        }
    }

    impl Session {
        /// `QuickJS.create({…})`：实例化、初始化、应用限制，然后 `clock.creating = false`。
        fn create(options: &StartVmOptions, max_stack_size: usize) -> Result<Self, StartVmError> {
            // 实例必须在模块自己的 Engine 上创建（`max_wasm_stack` 随句柄的栈档走，见
            // `crate::wasm::CodemodeWasmModule`）。
            let engine = options.wasm.engine();
            let module = options.wasm.module();
            let mut store = Store::new(engine, VmState::new(options));
            let mut linker: Linker<VmState> = Linker::new(engine);
            install_imports(&mut linker)?;
            // 阶段 1：实例化。12 个导入必须先全部注册，否则 instantiate 直接失败（错误经
            // `start_error` 压成一行文本）。
            let instance = linker
                .instantiate(&mut store, &module)
                .map_err(start_error)?;
            // 阶段 2：模块启动导出。顺序固定为 `_initialize` → `qjs_init*`；本文件没有绕过
            // `_initialize` 的路径。
            let initialize: TypedFunc<(), ()> = instance
                .get_typed_func(&mut store, "_initialize")
                .map_err(start_error)?;
            initialize.call(&mut store, ()).map_err(start_error)?;
            let deterministic = options.data.deterministic.is_some();
            // 阶段 3：QuickJS 引擎初始化。确定性模式走带 intrinsics 掩码的 `qjs_init2`，
            // 其余走无参 `qjs_init`；返回非 0 是引擎自己的初始化失败（消息由下面统一给出）。
            let init_result = if deterministic {
                let init2: TypedFunc<i32, i32> = instance
                    .get_typed_func(&mut store, "qjs_init2")
                    .map_err(start_error)?;
                init2.call(&mut store, DETERMINISTIC_INTRINSICS as i32)
            } else {
                let init: TypedFunc<(), i32> = instance
                    .get_typed_func(&mut store, "qjs_init")
                    .map_err(start_error)?;
                init.call(&mut store, ())
            }
            .map_err(start_error)?;
            if init_result != 0 {
                return Err(StartVmError::new("Failed to initialize QuickJS runtime"));
            }
            // 阶段 4：读导出表，并把其中一小撮可克隆句柄挂到 `VmState::host`。之后的运行期
            // 回调（bridge、WASI 字符串/值转换）全部从这里取导出。
            let exports = Exports::load(&mut store, &instance)?;
            store.data_mut().host = Some(exports.host_api());
            // npm 的 `vm.undefined` 是缓存单例：`host_call` 每次 dup 它，绝不 free 它。
            let undefined = exports
                .qjs_get_undefined
                .call(&mut store, ())
                .map_err(start_error)?;
            store.data_mut().undefined = undefined;
            // `applyLimits`：内存上限、栈上限、interrupt handler。
            //
            // 必须在任何脚本代码之前生效：prelude 与用户脚本都在返回之后才求值。
            if let Some(limit) = options.data.memory_limit_bytes {
                exports
                    .qjs_set_memory_limit
                    .call(&mut store, js_to_int32(limit))
                    .map_err(start_error)?;
            }
            exports
                .qjs_set_max_stack_size
                .call(&mut store, js_to_int32(max_stack_size as f64))
                .map_err(start_error)?;
            exports
                .qjs_set_interrupt_handler
                .call(&mut store, 1)
                .map_err(start_error)?;
            // 上下文创建结束。在此之前时钟读的是由种子派生的值（QuickJS 用第一次时钟读播种
            // `Math.random()`），此后才是虚拟时间；两者不能对调。
            if let Some(clock) = store.data_mut().clock.as_mut() {
                clock.creating = false;
            }
            Ok(Session {
                store,
                exports,
                bridge: 0,
                api: 0,
                settle: 0,
                stalled: 0,
                ready: false,
                disposed: false,
                deterministic,
                next_seq: 1,
                early: BTreeMap::new(),
            })
        }

        /// `startVm` 的 try 块：bridge、prelude、指纹、脚本、`run`、drain、`idle`。
        ///
        /// 返回 `Err(None)` 表示失败已作为 `done` 报过（脚本编译失败）；`Err(Some(..))` 要由
        /// 调用方报 `crash`。
        fn preamble(
            &mut self,
            options: &StartVmOptions,
            max_stack_size: usize,
        ) -> Result<(), Option<Failure>> {
            // 阶段 1：注册 `bridge`。它是 prelude 与宿主之间唯一的通道；prelude 闭包会持有它，
            // 所以这个句柄活得和 VM 一样长，之后不再释放。
            self.bridge = self.make_bridge().map_err(Some)?;
            // 阶段 2：求值并调用 prelude，得到它的返回值 `{ settle, run, stalled }`。
            let api = self.eval_prelude(&options.data).map_err(Some)?;
            self.api = api;
            // 阶段 3：取三个函数句柄。属性读取失败属于宿主侧的 wasm 错误（不是 guest 异常），
            // 由调用方转成 `crash`。
            let settle = self
                .exports
                .get_prop(&mut self.store, api, "settle")
                .map_err(|error| Some(wasm_failure(error)))?;
            let run = self
                .exports
                .get_prop(&mut self.store, api, "run")
                .map_err(|error| Some(wasm_failure(error)))?;
            let stalled = self
                .exports
                .get_prop(&mut self.store, api, "stalled")
                .map_err(|error| Some(wasm_failure(error)))?;
            self.settle = settle;
            self.stalled = stalled;
            // `settle`/`stalled` 都拿到了才算「startVm 的 try 块成功走到这里」；之后 `deliver`
            // 才能像 TS 一样 `vm.callFunction(settle, api, …)`（否则是 `settle.ptr` 的
            // TypeError，见 `apply`）。
            self.ready = settle != 0 && stalled != 0 && run != 0;
            // 阶段 4：确定性模式在脚本运行前报指纹，宿主从这里开始就能核对重放。
            if self.deterministic {
                let fingerprint = self.fingerprint(options, max_stack_size);
                self.post(WorkerToHostMessage::Started { fingerprint });
            }
            // 阶段 5：求值用户脚本（包成一个 async 箭头函数）。
            //
            // guest 抛出的异常就是脚本错误 → 一条失败的 `done`；只有宿主侧失败（wasm_malloc、
            // trap）才交给调用方报 `crash`。这条区分沿用 TS：编译/求值错误不算 VM 崩溃。
            // 前缀与脚本共处第 1 行：报错行号与脚本原文一致。
            let source = format!("(async (tools, console) => {{{}\n}})", options.data.code);
            let function = match self.exports.eval(&mut self.store, &source, SCRIPT_FILENAME) {
                Ok(function) => function,
                Err(Failure::Js(error)) => {
                    self.post(WorkerToHostMessage::done_error(
                        describe_exception(&error),
                        None,
                    ));
                    return Err(None);
                }
                Err(failure) => return Err(Some(failure)),
            };
            // 阶段 6：`run(api, function)` 启动脚本，跑到它第一次 await 或结束；返回值只是
            // 一个句柄，没有任何语义，读出来释放即可。
            let run_result = self.exports.call(&mut self.store, run, api, &[function]);
            self.exports.free(&mut self.store, function);
            match run_result {
                Ok(value) => self.exports.free(&mut self.store, value),
                Err(failure) => return Err(Some(failure)),
            }
            // 阶段 7：跑空 promise 队列，然后报首次的 `idle`（此时还没有任何结果被应用，
            // 所以 seq 是 0）。
            self.drain().map_err(Some)?;
            if self.deterministic {
                let issued = self.store.data().issued;
                self.post(WorkerToHostMessage::Idle { seq: 0, issued });
            }
            Ok(())
        }

        /// `vm.newFunction("bridge", …)`。
        fn make_bridge(&mut self) -> Result<i32, Failure> {
            let name = BRIDGE_NAME;
            let name_ptr = self.exports.write_c_string(&mut self.store, name);
            if name_ptr == 0 {
                return Err(Failure::Host(HostFailure::new("wasm_malloc failed")));
            }
            let result = self
                .exports
                .qjs_new_host_function
                .call(&mut self.store, (name_ptr, name.len() as i32, 0));
            let _ = self.exports.wasm_free.call(&mut self.store, name_ptr);
            result.map_err(wasm_failure)
        }

        /// 求值 prelude 并调用它，返回 `{ settle, run, stalled }` 对象。
        ///
        /// 调用形状是 `prelude(bridge, toolsJson, globalsJson, storeJson)`：三个数据参数都是
        /// JSON **文本**（prelude 自己 `JSON.parse`），`bridge` 是宿主函数句柄，`this` 用
        /// `undefined`。函数句柄、`this` 与三个数据实参都由这里释放；`bridge` 交给 prelude
        /// 持有，不在本函数释放。
        fn eval_prelude(
            &mut self,
            data: &crate::runtime::protocol::VmStartData,
        ) -> Result<i32, Failure> {
            let function = self
                .exports
                .eval(&mut self.store, PRELUDE_SOURCE, PRELUDE_FILENAME)?;
            let undefined = self
                .exports
                .qjs_get_undefined
                .call(&mut self.store, ())
                .map_err(wasm_failure)?;
            let tools = self
                .exports
                .new_string(&mut self.store, &js_stringify(&data.tools));
            let globals = self
                .exports
                .new_string(&mut self.store, &js_stringify(&data.globals));
            let store_text = self
                .exports
                .new_string(&mut self.store, &js_stringify(&data.store));
            let args = [self.bridge, tools, globals, store_text];
            let result = self
                .exports
                .call(&mut self.store, function, undefined, &args);
            self.exports.free(&mut self.store, function);
            self.exports.free(&mut self.store, undefined);
            for handle in [tools, globals, store_text] {
                self.exports.free(&mut self.store, handle);
            }
            result
        }

        /// 确定性指纹（`started` 载荷）。
        ///
        /// 文本形状是重放契约的一部分：`tests/vm.rs` 用逐字字符串断言它（包括 `heap none` /
        /// `budget none` 这两个字面量）。`qjs_get_quickjs_version` 失败（返回 0）时退回编译期
        /// 常量 `QUICKJS_VERSION`，不让它变成启动失败。
        fn fingerprint(&mut self, options: &StartVmOptions, max_stack_size: usize) -> String {
            let version_ptr = self
                .exports
                .qjs_get_quickjs_version
                .call(&mut self.store, ())
                .unwrap_or(0);
            let quickjs = if version_ptr == 0 {
                super::QUICKJS_VERSION.to_string()
            } else {
                self.exports.read_c_string(&mut self.store, version_ptr)
            };
            let heap = match options.data.memory_limit_bytes {
                Some(bytes) => js_number_to_string(bytes),
                None => "none".to_string(),
            };
            let budget = match options.interrupt_budget {
                Some(budget) => budget.to_string(),
                None => "none".to_string(),
            };
            format!(
                "quickjs-wasi {QUICKJS_WASI_VERSION}, quickjs {quickjs}, prelude {PRELUDE_HASH}, deterministic {DETERMINISTIC_VERSION}, stack {max_stack_size}, heap {heap}, budget {budget}"
            )
        }

        /// 把一条消息交给宿主（`StartVmOptions::post` 的转发）。
        ///
        /// 在 VM 线程上同步调用：回调返回之前 VM 不会继续执行。调用方必须遵守
        /// `StartVmOptions::post` 的「不得重入」契约。
        fn post(&self, message: WorkerToHostMessage) {
            let post = Arc::clone(&self.store.data().post);
            post(message);
        }

        /// 跑排队中的 promise job，然后调用 `stalled()` 检测永远等不到的等待。
        ///
        /// TS: `drain`。`executePendingJobs()` 会一口气跑空队列、没有中断的机会，所以逐个
        /// job 走内部导出，并每 [`JOBS_PER_POLL`] 个 job 轮询一次 `should_interrupt`。
        fn drain(&mut self) -> Result<(), Failure> {
            let mut jobs: u32 = 0;
            loop {
                let pending = self
                    .exports
                    .qjs_is_job_pending
                    .call(&mut self.store, ())
                    .map_err(wasm_failure)?;
                if pending == 0 {
                    break;
                }
                // job 只在不可 catch 的错误（如 interrupt）下失败。
                let result = self
                    .exports
                    .qjs_execute_pending_job
                    .call(&mut self.store, ())
                    .map_err(wasm_failure)?;
                if result < 0 {
                    return Err(Failure::Host(HostFailure::new("Promise job failed")));
                }
                jobs += 1;
                if jobs.is_multiple_of(JOBS_PER_POLL) {
                    let should_interrupt = Arc::clone(&self.store.data().should_interrupt);
                    if should_interrupt() {
                        return Err(Failure::Host(HostFailure::new("Execution interrupted")));
                    }
                }
            }
            // 队列空了不代表脚本健康：还要问一次 prelude 的 `stalled()`，让「等待一个永远
            // 不会来的结果」的脚本在那里失败；返回值是一个句柄，释放即可。
            let stalled = self.stalled;
            let api = self.api;
            let value = self.exports.call(&mut self.store, stalled, api, &[])?;
            self.exports.free(&mut self.store, value);
            Ok(())
        }

        /// `apply(message)`：把一条结果交给 prelude 的 `settle`，然后 drain。
        ///
        /// 调用形状是 `settle.call(api, id, ok, payload, properties)`：`payload` 是结果的
        /// JSON 文本（失败时是错误消息），`properties` 是失败时 `Error` 要带的自有属性
        /// （见 `HostResult` 的字段文档）。两个可选值缺失时以 `undefined` 传入。
        fn apply(&mut self, message: &HostResult) -> Result<(), Failure> {
            // TS：`vm.callFunction(settle, api, …)` 里 `settle` 是 `undefined`（prelude 没跑成），
            // 读 `settle.ptr` 抛 TypeError。
            if !self.ready {
                return Err(Failure::Host(HostFailure {
                    name: "TypeError",
                    message: "Cannot read properties of undefined (reading 'ptr')".to_string(),
                }));
            }
            let id = self
                .exports
                .qjs_new_number
                .call(&mut self.store, message.id as f64)
                .map_err(wasm_failure)?;
            let ok = if message.ok {
                self.exports.qjs_get_true.call(&mut self.store, ())
            } else {
                self.exports.qjs_get_false.call(&mut self.store, ())
            }
            .map_err(wasm_failure)?;
            let undefined = self
                .exports
                .qjs_get_undefined
                .call(&mut self.store, ())
                .map_err(wasm_failure)?;
            let payload = match message.payload.as_deref() {
                Some(text) => self.exports.new_string(&mut self.store, text),
                None => 0,
            };
            let properties = match message.properties.as_deref() {
                Some(text) => self.exports.new_string(&mut self.store, text),
                None => 0,
            };
            // `None` 用 `undefined` 占位，而不是传句柄 0：0 在 `Exports::free` 里是「没有句柄」
            // 的哨兵，不是合法的 guest 值。
            let payload_arg = if payload == 0 { undefined } else { payload };
            let properties_arg = if properties == 0 {
                undefined
            } else {
                properties
            };
            let args = [id, ok, payload_arg, properties_arg];
            let settle = self.settle;
            let api = self.api;
            let result = self.exports.call(&mut self.store, settle, api, &args);
            // 实参句柄与 `settle` 的返回句柄都在这里释放；这几行先于 `result?`，所以
            // `settle` 抛异常时同样不会漏放。
            self.exports.free(&mut self.store, id);
            self.exports.free(&mut self.store, ok);
            self.exports.free(&mut self.store, undefined);
            if payload != 0 {
                self.exports.free(&mut self.store, payload);
            }
            if properties != 0 {
                self.exports.free(&mut self.store, properties);
            }
            let value = result?;
            self.exports.free(&mut self.store, value);
            self.drain()
        }

        /// `session.deliver(message)`：宿主回答一个调用。
        ///
        /// TS: `deliver`。非确定性直接 `apply`；确定性按 `seq` 排队、只按序应用，每应用一条就
        /// 报一次 `idle`。任何失败（包括坏 `seq`）都转成 `crash`，不返回错误。
        pub(super) fn deliver(&mut self, message: HostToWorkerMessage) {
            if self.disposed {
                return;
            }
            let HostToWorkerMessage::Result(result) = message;
            let outcome = self.deliver_result(result);
            if let Err(failure) = outcome {
                self.post(WorkerToHostMessage::Crash {
                    message: crash_message(&failure),
                });
            }
        }

        /// `deliver` 的实现：非确定性直接 `apply`；确定性按 `seq` 排队、只按序应用。
        ///
        /// 返回 `Err` 由调用方（`Session::deliver`）转成 `crash` 消息。坏 `seq` 的文本在这里
        /// 生成，因为只有这里知道「期望的下一个号」。
        fn deliver_result(&mut self, message: HostResult) -> Result<(), Failure> {
            if !self.deterministic {
                return self.apply(&message);
            }
            // 确定性模式的两条不变量：
            // 1. `seq` 必须 ≥ `next_seq` 且同一号只能出现一次；回退、重复、缺失都是宿主侧的
            //    协议错误 → crash（文案与 TS 相同）。
            // 2. 早到的结果先存进 `early`，只按 `next_seq` 连续应用；每应用一条报一次
            //    `idle{seq, issued}`，宿主据此确认重放进度。
            let seq = message.seq;
            let valid = match seq {
                Some(seq) => seq >= self.next_seq && !self.early.contains_key(&seq),
                None => false,
            };
            if !valid {
                let seq_text = match seq {
                    Some(seq) => js_number_to_string(seq as f64),
                    None => "undefined".to_string(),
                };
                let id_text = js_number_to_string(message.id as f64);
                return Err(Failure::Host(HostFailure::new(format!(
                    "Result {id_text} has sequence number {seq_text}, expected {} or later, once",
                    self.next_seq
                ))));
            }
            self.early.insert(seq.unwrap_or(0), message);
            while let Some(next) = self.early.remove(&self.next_seq) {
                // 绝不倒退：恢复的宿主的时钟可能落后于启动这次运行的时钟。
                // 用 JS 的 `Math.max`（NaN 传染），不是 `f64::max`（会吞掉 NaN）。
                if let Some(now) = next.now.filter(|now| now.is_finite())
                    && let Some(clock) = self.store.data_mut().clock.as_mut()
                {
                    clock.now = js_math_max(clock.now, now);
                }
                let seq = self.next_seq;
                self.store.data_mut().current = seq;
                self.apply(&next)?;
                let issued = self.store.data().issued;
                self.post(WorkerToHostMessage::Idle { seq, issued });
                self.next_seq += 1;
            }
            Ok(())
        }

        /// `session.dispose()`：只执行一次；之后的 `deliver` 静默忽略。
        ///
        /// 真正的释放由 [`super::VmSession::dispose`] 丢弃整个会话完成（清空对 wasm 实例的引用）。
        pub(super) fn dispose(&mut self) {
            self.disposed = true;
        }
    }

    /// 发给 VM 线程的命令。
    enum Command {
        /// 应用一条宿主结果。
        Deliver(HostResult),
        /// 释放 VM 并退出线程。
        Stop,
    }

    /// VM 线程处理完一条命令的回复。
    enum Reply {
        /// 处理完成（`post` 的消息已在返回前全部发出）。
        Done,
        /// 处理过程中 panic 了；调用方线程原样重新 panic（保持 `start_vm`/`deliver` 的
        /// 原有语义：宿主回调里的 panic 不会被吞掉）。
        Panicked(Box<dyn std::any::Any + Send>),
    }

    /// VM 线程的栈帧里取出 panic 文案（与 `std::panic::PanicHookInfo` 的载荷一致）。
    fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
        if let Some(text) = payload.downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = payload.downcast_ref::<String>() {
            text.clone()
        } else {
            "unknown panic".to_string()
        }
    }

    /// 与 VM 线程的句柄（[`VmSession`] 的内部表示）。
    ///
    /// # 为什么要单独一个线程
    ///
    /// wasm 帧用的是**调用线程**的原生栈，而 wasmtime 的 `max_wasm_stack` 绝不能超过线程
    /// 真实可用的栈：超了不是 trap，而是进程 abort。宿主线程的栈大小（worker 线程、tokio
    /// worker、测试线程）我们无法假设——默认都只有 2 MiB，比两档 wasm 预算
    /// （[`WASM_STACK_SIZE`] / [`INLINE_WASM_STACK_SIZE`]）都小。VM 因此跑在自己创建的线程上
    /// （[`VM_THREAD_STACK_SIZE`]），`deliver`/`dispose` 通过通道把命令交给它并等到处理完成：
    /// - `post` 依旧**同步**地在 VM 回调里被调用（只是换了个线程），顺序不变；
    /// - `deliver` 返回时脚本已经跑到下一次等待，宿主观测到的时序与 TS 的同步 `deliver` 相同。
    pub(super) struct Handle {
        commands: std::sync::mpsc::Sender<Command>,
        replies: std::sync::mpsc::Receiver<Reply>,
        thread: Option<std::thread::JoinHandle<()>>,
        disposed: bool,
    }

    impl Handle {
        /// 把一条宿主回答交给 VM 线程并等它处理完。
        ///
        /// 已 dispose、或命令通道断开时静默返回。VM 线程处理这条命令时 panic，会在**本**
        /// 线程上重新抛出（`resume_unwind`），调用方看到原始 panic 而不是被吞掉。
        pub(super) fn deliver(&mut self, message: HostToWorkerMessage) {
            if self.disposed {
                return;
            }
            let HostToWorkerMessage::Result(result) = message;
            if self.commands.send(Command::Deliver(result)).is_err() {
                return;
            }
            match self.replies.recv() {
                Ok(Reply::Done) | Err(_) => {}
                Ok(Reply::Panicked(payload)) => std::panic::resume_unwind(payload),
            }
        }

        /// 通知 VM 线程退出并等它释放 wasm 实例。
        ///
        /// 只执行一次；重复调用是 no-op。`join` 等 VM 线程在命令循环里看到 `Stop` 后退出。
        pub(super) fn dispose(&mut self) {
            if self.disposed {
                return;
            }
            self.disposed = true;
            let _ = self.commands.send(Command::Stop);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            if self.disposed {
                return;
            }
            self.disposed = true;
            // 只发 Stop、不 join：调用方可能是在 worker 被 terminate 后丢弃会话的，脚本
            // 可能还没跑到下一个中断轮询，join 会挂住调用方（TS 的 `terminate()` 也不等待）。
            let _ = self.commands.send(Command::Stop);
        }
    }

    /// VM 线程主体：创建 VM、跑 `startVm` 的 try 块，然后处理 `deliver`/`dispose`。
    fn run_vm_thread(
        options: StartVmOptions,
        max_stack_size: usize,
        runtime: Option<tokio::runtime::Handle>,
        ready: std::sync::mpsc::Sender<Result<(), StartVmError>>,
        commands: std::sync::mpsc::Receiver<Command>,
        replies: std::sync::mpsc::Sender<Reply>,
    ) {
        // 让 `post` 里的 `tokio::spawn`（若有）仍然可用：worker 后端的 post 运行在
        // `runtime.block_on` 里，换成 VM 线程后补上同样的上下文。
        let _entered = runtime.as_ref().map(|handle| handle.enter());
        // 就绪握手：`ready` 只发一条——要么是创建失败，要么是「启动段已经跑完」。调用方
        // `start` 阻塞在 `ready_rx.recv()` 上；不 send 就 return 的话，它只能看到通道关闭
        // （`Failed to start QuickJS`）而拿不到真正的原因，所以下面每条路径都显式 send。
        let mut session = match Session::create(&options, max_stack_size) {
            Ok(session) => session,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            session.preamble(&options, max_stack_size)
        }));
        match started {
            Ok(Ok(())) => {}
            Ok(Err(Some(failure))) => {
                let message = crash_message(&failure);
                session.post(WorkerToHostMessage::Crash { message });
            }
            Ok(Err(None)) => {}
            Err(payload) => {
                let _ = ready.send(Err(StartVmError::new(format!(
                    "QuickJS panicked while starting: {}",
                    panic_text(&*payload)
                ))));
                return;
            }
        }
        // 启动段结束（成功，或已按 done/crash 收尾）：这时才放行调用方。之后的 `deliver`
        // 由下面的命令循环逐条处理。
        if ready.send(Ok(())).is_err() {
            return;
        }
        while let Ok(command) = commands.recv() {
            match command {
                Command::Deliver(message) => {
                    // 每条命令单独 `catch_unwind`：一次 `deliver` 里的宿主 panic 只回传给它的
                    // 调用方，VM 线程继续服务下一条命令。
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        session.deliver(HostToWorkerMessage::Result(message));
                    }));
                    let reply = match outcome {
                        Ok(()) => Reply::Done,
                        Err(payload) => Reply::Panicked(payload),
                    };
                    if replies.send(reply).is_err() {
                        break;
                    }
                }
                Command::Stop => break,
            }
        }
        // `Stop`，或命令通道断开（调用方丢掉了 `Handle`）：释放会话与 wasm 实例后退出。
        session.dispose();
        drop(session);
    }

    /// 把 12 个 import 装进 linker。
    ///
    /// TS: `QuickJS.instantiate` 的 `WebAssembly.instantiate(module, { env, wasi_snapshot_preview1 })`
    /// 与 `createWasiShim`。
    fn install_imports(linker: &mut Linker<VmState>) -> Result<(), StartVmError> {
        let wrap_error = start_error;
        linker
            .func_wrap("env", "host_call", host_call_import)
            .map_err(wrap_error)?;
        linker
            .func_wrap("env", "host_interrupt", host_interrupt_import)
            .map_err(wrap_error)?;
        linker
            .func_wrap(
                "env",
                "host_promise_rejection",
                host_promise_rejection_import,
            )
            .map_err(wrap_error)?;
        linker
            .func_wrap("env", "host_module_normalize", host_module_normalize_import)
            .map_err(wrap_error)?;
        linker
            .func_wrap("env", "host_module_load", host_module_load_import)
            .map_err(wrap_error)?;
        linker
            .func_wrap(
                "env",
                "host_get_timezone_offset",
                host_get_timezone_offset_import,
            )
            .map_err(wrap_error)?;
        linker
            .func_wrap(
                "wasi_snapshot_preview1",
                "clock_time_get",
                clock_time_get_import,
            )
            .map_err(wrap_error)?;
        linker
            .func_wrap("wasi_snapshot_preview1", "fd_close", fd_close_import)
            .map_err(wrap_error)?;
        linker
            .func_wrap(
                "wasi_snapshot_preview1",
                "fd_fdstat_get",
                fd_fdstat_get_import,
            )
            .map_err(wrap_error)?;
        linker
            .func_wrap("wasi_snapshot_preview1", "fd_seek", fd_seek_import)
            .map_err(wrap_error)?;
        linker
            .func_wrap("wasi_snapshot_preview1", "fd_write", fd_write_import)
            .map_err(wrap_error)?;
        linker
            .func_wrap("wasi_snapshot_preview1", "random_get", random_get_import)
            .map_err(wrap_error)?;
        Ok(())
    }

    /// `startVm(options)`：创建 VM 并跑到第一次等待（或结束）。
    ///
    /// VM 创建、`startVm` 的 try 块和之后的每次 `deliver` 都跑在专用的 VM 线程上（见
    /// [`Handle`]）；本函数等它跑到第一次等待后返回，因此宿主看到的时序与 TS 的同步
    /// `startVm` 相同。
    pub(super) fn start(options: StartVmOptions) -> Result<Handle, StartVmError> {
        let max_stack_size = options.max_stack_size.unwrap_or(MAX_STACK_SIZE);
        if max_stack_size > MAX_STACK_SIZE {
            // quickjs-wasi 的 `normalizeOptions` 对超限的 `maxStackSize` 抛 `RangeError`。
            return Err(StartVmError::with_name(
                "RangeError",
                format!("maxStackSize must be an integer between 0 and {MAX_STACK_SIZE}"),
            ));
        }
        // 三条阻塞通道：命令（调用方 → VM 线程）、就绪（VM 线程 → 调用方，只发一条）、
        // 回复（每条 `Deliver` 一条）。`Handle::deliver` 用阻塞的 `recv` 等 VM 处理完，所以
        // 在 async 任务里调用 `deliver` 会占住当前线程，直到脚本再次停下。
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        // `post` 里可能用 `tokio::spawn`：worker 后端的 post 原本跑在 runtime 上下文里，
        // 挪到 VM 线程后要补上同样的上下文，否则 `tokio::spawn` 会 panic。
        let runtime = tokio::runtime::Handle::try_current().ok();
        let thread = std::thread::Builder::new()
            .name("kiana-codemode-vm".to_string())
            .stack_size(super::VM_THREAD_STACK_SIZE)
            .spawn(move || {
                run_vm_thread(
                    options,
                    max_stack_size,
                    runtime,
                    ready_tx,
                    command_rx,
                    reply_tx,
                );
            })
            .map_err(|error| {
                StartVmError::new(format!("Failed to start the QuickJS thread: {error}"))
            })?;
        // 等 VM 线程报「已跑到第一次等待」。这也是阻塞调用；创建失败时原因已经在 `ready`
        // 里，join 掉线程后原样返回。
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Handle {
                commands: command_tx,
                replies: reply_rx,
                thread: Some(thread),
                disposed: false,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(StartVmError::new("Failed to start QuickJS"))
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{js_math_max, js_math_round, nanos_from_millis};

        /// `BigInt(Math.round(ms)) * 1_000_000n` 交给 `setBigUint64` 的结果（低 64 位）。
        ///
        /// 期望值是**精确**算出来的：把 `Math.round(ms)` 的 f64 值取成整数（`BigInt` 正是
        /// 这个语义），乘 `10^6`，再按 2^64 取模。不是 `(ms * 1e6) as u64`（那条路径先做
        /// 浮点乘法、再饱和/回绕，`1e19` 就会错）。这些值同时由 TS `startVm` 的
        /// `Date.now()` 输出核对过（`tests/vm.rs` 的时钟回归测试）。
        #[test]
        fn nanos_from_millis_matches_the_bigint_expression() {
            let cases: [(f64, u64); 17] = [
                (0.0, 0),
                (0.5, 1_000_000),
                (1.5, 2_000_000),
                (-1.5, 18_446_744_073_708_551_616), // -1_000_000 mod 2^64
                (-0.5, 0),
                // `Math.round(0.49999999999999994)` 是 0，不是 1（×1e6 会差 1 毫秒）。
                (0.499_999_999_999_999_94, 0),
                (9_007_199_254_740_992.0, 5_188_146_770_730_811_392), // 2^53
                (9_007_199_254_740_994.0, 5_188_146_770_732_811_392), // 2^53 + 2
                (9_223_372_036_854_775_808.0, 0),                     // 2^63
                (1e16, 1_864_712_049_423_024_128),
                (1e18, 2_003_764_205_206_896_640),
                (1e19, 1_590_897_978_359_414_784),
                (-1e19, 16_855_846_095_350_136_832),
                (1e33, 0),
                (1.7e32, 6_917_529_027_641_081_856),
                (1e300, 0),
                (-1e300, 0),
            ];
            for (millis, expected) in cases {
                assert_eq!(
                    nanos_from_millis(millis).map_err(|error| error.message()),
                    Ok(expected),
                    "nanos_from_millis({millis:?})"
                );
            }
        }

        /// `BigInt` 拒绝 `NaN`/`±Infinity`：TS 的 `RangeError` 文案逐字复刻。
        #[test]
        fn nanos_from_millis_rejects_non_finite_values() {
            let cases = [
                (f64::NAN, "NaN"),
                (f64::INFINITY, "Infinity"),
                (f64::NEG_INFINITY, "-Infinity"),
            ];
            for (millis, text) in cases {
                let error = nanos_from_millis(millis).expect_err("non-finite must fail");
                assert_eq!(
                    error.message(),
                    format!(
                        "The number {text} cannot be converted to a BigInt because it is not an integer"
                    )
                );
            }
        }

        /// `Math.round` 的边角语义（与 QuickJS 的 `js_round` 实测一致）。
        #[test]
        fn js_math_round_matches_quickjs() {
            // (输入, 期望值, 期望值是否 -0)
            let cases = [
                (0.499_999_999_999_999_94, 0.0, false),
                (1.499_999_999_999_999_8, 1.0, false),
                (4_503_599_627_370_495.5, 4_503_599_627_370_496.0, false),
                (-4_503_599_627_370_495.5, -4_503_599_627_370_495.0, false),
                (2.500_000_000_000_000_4, 3.0, false),
                (-2.500_000_000_000_000_4, -3.0, false),
                (1_000_000_000_000_000.5, 1_000_000_000_000_001.0, false),
                (2.5, 3.0, false),
                (-2.5, -2.0, false),
                (-1.5, -1.0, false),
                (-0.5, 0.0, true),
                (-0.4, 0.0, true),
                (-0.499_999_999_999_999_94, 0.0, true),
            ];
            for (input, expected, negative_zero) in cases {
                let rounded = js_math_round(input);
                assert_eq!(rounded, expected, "Math.round({input})");
                assert_eq!(
                    rounded == 0.0 && rounded.is_sign_negative(),
                    negative_zero,
                    "Math.round({input}) 的零符号"
                );
            }
        }

        /// `Math.max` 的 NaN 传染与 `±0`：`f64::max` 这两条都与 JS 不同。
        ///
        /// TS: `vm.ts:218`。`Math.max(NaN, 1)` 是 `NaN`（`f64::max` 会给 `1`），
        /// `Math.max(-0, +0)` 是 `+0`（`f64::max` 的零符号未指定）。
        #[test]
        fn js_math_max_matches_math_max() {
            assert!(js_math_max(f64::NAN, 1.0).is_nan(), "NaN 传染（左）");
            assert!(js_math_max(1.0, f64::NAN).is_nan(), "NaN 传染（右）");
            assert_eq!(js_math_max(1.0, 2.0), 2.0);
            assert_eq!(js_math_max(2.0, 1.0), 2.0);
            assert_eq!(js_math_max(f64::INFINITY, 1.0), f64::INFINITY);
            assert_eq!(js_math_max(f64::NEG_INFINITY, 1.0), 1.0);
            for (a, b, negative_zero) in
                [(-0.0, 0.0, false), (0.0, -0.0, false), (-0.0, -0.0, true)]
            {
                let max = js_math_max(a, b);
                assert_eq!(max, 0.0, "Math.max({a:?}, {b:?})");
                assert_eq!(max.is_sign_negative(), negative_zero, "零符号 {a:?} {b:?}");
            }
        }
    }
}
