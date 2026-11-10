//! runtime/inline.ts：在调用线程上跑 VM（没有 worker 线程的运行时），也是
//! `RemoteCodemodeSandbox` 的远端半边。
//!
//! TS 源：`packages/codemode/src/runtime/inline.ts`（126 行）。
//!
//! 问题：QuickJS 同步运行。脚本计算期间宿主的事件循环不跑，所以 deadline 定时器与 abort 信号
//! 都不会触发；Workers 上连 `Date.now()` 都在同步执行期间冻结。例子：有 worker 线程时
//! `while (true) {}` 会被宿主的 200 ms 定时器停住；在调用线程上那个定时器根本没有机会跑。
//!
//! 解法：中断预算。QuickJS 在脚本运行期间周期性轮询自己的中断处理器；本 launcher 把整个执行
//! 期间的轮询次数累计起来，超过预算就中断脚本，执行以 `kind: "timeout"` 失败。deadline 与
//! abort 信号仍然有效——只要脚本在等工具，那时事件循环是在跑的。
//!
//! 轮询速率取决于代码与机器。紧循环实测：Apple M 系列笔记本约 50,000 次/秒 CPU，Cloudflare
//! Durable Objects 约 10,000 次/秒（2026 年 10 月）。默认 100,000 次允许约 2 到 10 秒的连续
//! 计算；要明显低于平台自己的 CPU 上限——撞上那个上限是重置 Durable Object，而不是让一个
//! 脚本失败。
//!
//! VM 报告 `done` 或 `crash` 之后结果已定，因此下一次轮询就中断 VM，而不是让它一直跑到宿主
//! 来停。例子：脚本在输出上限达到后仍在 `for (;;)` 里打印，否则会跑到预算耗尽、被报成超时。
//!
//! 栈：VM 跑在调用方的栈上，比 worker 线程的栈小（Node 主线程约 1 MB），所以 QuickJS 的栈
//! 护栏设为 [`INLINE_MAX_STACK_SIZE`]。Rust 侧这条约 1 MB 的「调用方栈」映射为
//! `crate::wasm::inline_engine` 的 wasm 栈预算（`INLINE_WASM_STACK_SIZE`）：
//! 解析器与 `JSON.stringify` 这类 C 递归在 600 / 6000 层处以 `RangeError: Maximum call stack
//! size exceeded` 的 crash 结束，与 TS 在 Node 主线程上一致（见 `crate::wasm::CodemodeWasmModule`）。
//!
//! 重入：VM 回调跑在 QuickJS 内部。VM 的消息经**一个专门的转发任务**（`queueMicrotask` 的
//! Rust 等价物）在 VM 返回之后交给宿主，因此宿主代码（工具调用、`finish()`、dispose）永远不
//! 在 VM 回调里运行、也永不重入 VM（spec 附录 A.6 第 6 条）。与 `queueMicrotask` 一样，一轮
//! 同步执行里产生的消息**成批**在 VM 让出之后才交付：脚本计算期间的输出不会中途到宿主。
//!
//! # Rust 映射
//!
//! - `queueMicrotask(() => …)` → 一个无界 [`tokio::sync::mpsc`] 通道 + 一个转发任务：VM 的
//!   `post` 回调只入队（不阻塞、不调用宿主）；每次 VM 让出（`startVm` 返回或 `deliver`
//!   返回）再入队一个 `Yield` 标记，转发任务处理到标记时把这一轮的消息按序交给 [`VmEvents`]。
//!   转发任务在交付终态消息之后、或收到 `stop()` 的 `Shutdown` 之后退出——它因此不长期持有
//!   `events`（见 `ForwardItem::Shutdown`；`events` 与 `Execution`、channel 之间否则成环）。
//! - `startVm` 的 `shouldInterrupt` 闭包 → `Arc<dyn Fn() -> bool>`；两条取得控制权的路径
//!   （QuickJS 的 `interruptHandler` 与 `drain()` 每 50 个 job 一次）共用同一个
//!   [`AtomicU64`] 计数（spec §A.5.2）。
//! - 「脚本计算期间宿主事件循环不跑」→ [`HostEventLoop`]：VM 执行（`startVm` 的初始段与每次
//!   `deliver`）期间占住闸门，转发任务交完这一轮消息后放开；`execution.rs` 的 deadline 与
//!   abort 监听等它放开才 `finish`（spec §A.5.2）。
//! - `session.dispose()` 是同步的（`vm.rs` 的 `VmSession::dispose` 发 Stop 并 join VM 线程），
//!   因此 `stop()` 里同步做完、返回一个立刻完成的 future；TS 的 `stop` 也是同步的
//!   （`async` 只是为了类型）。
//!
//! # 已知偏差
//!
//! - TS 的 `wasm` 是 `CodemodeWasmModule | Promise<CodemodeWasmModule>`，`await` 失败给出
//!   `Failed to load QuickJS: …`。Rust 的 [`CodemodeWasmModule`] 已经是编译好的模块，没有
//!   「被拒的 Promise」这一态：加载失败发生在调用 `load_quickjs_wasm` 的时候，调用方在那里
//!   拿到 `CodemodeWasmError`（文本相同、前缀由 launcher 的同类消费者 `host.rs` 加）。因此
//!   [`inline_launcher`] 没有这条错误路径。
//! - TS 的 `interruptBudget` 是任意 number；`vm.rs` 的冻结 API 是 `Option<u32>`（见
//!   `StartVmOptions::interrupt_budget` 的文档），这里同样用 `u32`。
//!
//! # 一次执行的生命周期
//!
//! 1. [`inline_launcher`] 创建时先把 `wasm` 切到 inline 栈档（`with_inline_stack`，只克隆
//!    已编译的 `Module`）；返回的 launcher 每次被调用时新建 `stopped`/`settled`/`exhausted`/
//!    `polls` 四个标志、转发队列与两个回调（`should_interrupt`、`post`）。
//! 2. 在调用 `start_vm` **之前**占住宿主闸门（`HostEventLoop::set_busy`）：VM 从创建到第一次
//!    让出（或结束）都算「宿主线程被占住」。
//! 3. `start_vm` 返回后，这一轮产生的消息已经全部入队；再入队一个 `Yield(token)`。
//! 4. 启动转发任务消费队列：每遇到 `Yield`，把攒下的消息按序交给 [`VmEvents`]，然后
//!    `set_idle(token)` 放开闸门；交付终态消息后退出。
//! 5. 宿主之后的每条回复走 `InlineChannel::post`：占闸门 → `session.deliver`（同步跑脚本到
//!    下一次让出）→ 入队 `Yield`。正常路径下 `post` 返回时脚本已经再次停下或结束；若 `stop()`
//!    已经置位（或会话已被取走）则直接返回，不跑脚本。
//! 6. `stop()` 置 `stopped`、`dispose` 会话、入队 `Shutdown`；转发任务丢弃排队项并退出，
//!    从而释放它持有的 `events`。
//!
//! # 终态与错误
//!
//! - `start_vm` 失败 → launcher 返回 `Err(StartVmError::message())`（`Execution` 记成
//!   `kind: "sandbox"`），并先放开闸门；这是本模块唯一的 launcher 级错误。
//! - 终态是 `done`（`ok` 为 true 或 false）与 `crash`。`post` 只放行第一条终态消息，之后
//!   的终态消息被丢弃——中断一个已结算 VM 造成的 crash 是噪声。
//! - 预算耗尽：`should_interrupt` 置 `exhausted` 并返回 `true`，QuickJS 随后以 crash 结束
//!   （脚本执行中为 `InternalError: interrupted`，drain 路径为 `Error: Execution interrupted`，
//!   见 `tests/vm.rs` 的 `interrupt_during_script_crashes` / `interrupt_during_drain_crashes`）；
//!   转发任务看到「`exhausted` + 终态」时不再透传该消息，而是用
//!   `Execution exceeded its interrupt budget of N`（`kind: "timeout"`）结算。
//! - `stop()` 之后的排队消息与 VM 消息都不再交付（转发任务在 `Yield` 处按 `stopped` 丢弃）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::runtime::execution::{
    CodemodeSandboxBase, HostEventLoop, StartError, VmChannel, VmEvents, VmLauncher,
};
use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerToHostMessage};
use crate::runtime::vm::{StartVmOptions, VmSession, start_vm};
use crate::types::{
    BoxFuture, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeResult,
    CodemodeRun, CodemodeRunOptions, CodemodeSandboxBaseOptions, CodemodeTool,
};
use crate::wasm::CodemodeWasmModule;

/// 一次执行可以用的中断轮询次数；超过即 `kind: "timeout"`。
///
/// 这是**次数**不是时间：实际墙钟取决于代码与机器（模块文档给出上游的实测区间）。
/// [`InlineCodemodeSandbox::new`] 在 `interrupt_budget: None` 时取它；
/// `CodemodeServeOptions::max_interrupt_budget` 为 `None` 时，`serve_codemode_remote` 用它作为
/// 服务端预算上限与请求缺省值（见 `runtime/remote.rs`）。`RemoteCodemodeSandbox` 自己的
/// `interrupt_budget` 保持 `None`，交给远端默认值。
///
/// TS: `inline.ts#DEFAULT_INTERRUPT_BUDGET`。
pub const DEFAULT_INTERRUPT_BUDGET: u32 = 100_000;

/// 约 1,000 层递归，且远早于 1 MB 宿主栈溢出就触发。
///
/// 它作为 `StartVmOptions::max_stack_size` 交给 `start_vm`，约束的是 QuickJS 自己的 JS 递归
/// 守卫；wasm 帧的预算由 [`inline_launcher`] 通过 `with_inline_stack()` 选择的 Engine 决定
/// （`runtime/vm.rs` 的 `INLINE_WASM_STACK_SIZE`，1_100_000 字节，约 1.1 MB）。两者独立标定：
/// JS 递归先撞可捕获的 `RangeError`，C 递归（解析器、`JSON.stringify`）先撞 wasm 预算并以
/// crash 结束。
///
/// TS: `inline.ts#INLINE_MAX_STACK_SIZE`。
pub const INLINE_MAX_STACK_SIZE: usize = 256 * 1024;

/// [`InlineCodemodeSandbox`] 的选项。
///
/// TS: `inline.ts#InlineCodemodeSandboxOptions`（`extends CodemodeSandboxBaseOptions`；
/// 基类字段在 [`base`](Self::base) 里，与 [`crate::types::CodemodeSandboxOptions`] 的布局一致）。
#[derive(Clone)]
pub struct InlineCodemodeSandboxOptions {
    /// 基类选项（工具、全局函数、默认 deadline、内存上限）。
    pub base: CodemodeSandboxBaseOptions,
    /// 编译好的 `quickjs-wasi/quickjs.wasm`（Rust 侧是同一次编译的 `wasmtime::Module`）。
    ///
    /// TS 允许 `Promise<CodemodeWasmModule>`；Rust 里模块必须先加载完（见模块文档的已知偏差）。
    pub wasm: CodemodeWasmModule,
    /// 一次执行在失败为 `kind: "timeout"` 之前可以用的中断轮询次数。
    ///
    /// 这是「脚本不 await 工具地计算」时唯一的限制。默认 [`DEFAULT_INTERRUPT_BUDGET`]。
    pub interrupt_budget: Option<u32>,
}

impl std::fmt::Debug for InlineCodemodeSandboxOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineCodemodeSandboxOptions")
            .field("base", &self.base)
            .field("wasm", &"<CodemodeWasmModule>")
            .field("interrupt_budget", &self.interrupt_budget)
            .finish()
    }
}

/// 执行是否已到终态（`done` 或 `crash`）。
///
/// `done` 包含成功与失败两种结果（`WorkerDoneOutcome` 的 `ok` 字段），两者都表示脚本已经
/// 结算；`started`/`output`/`call` 都不是终态。
fn is_terminal(message: &WorkerToHostMessage) -> bool {
    matches!(
        message,
        WorkerToHostMessage::Done(_) | WorkerToHostMessage::Crash { .. }
    )
}

/// 转发队列的一项。
enum ForwardItem {
    /// VM 在宿主线程上产生的一条消息。先攒着，等这一轮同步执行结束再批量交付
    /// （`queueMicrotask` 的语义：计算中途不跑宿主代码）。
    Message(WorkerToHostMessage),
    /// VM 让出了宿主（`startVm` 或 `deliver` 返回）。处理到这里时把这一轮的消息按序交给宿主，
    /// 然后放开宿主事件循环闸门——微任务先于定时器与 abort 事件。
    ///
    /// 值是这次占用闸门的令牌：并发的下一段执行可能已经重新占住闸门，只有令牌仍是最新时才
    /// 能放开（[`HostEventLoop::set_idle`]）。`None` 表示 `events.host_event_loop()` 没有
    /// 提供闸门（非 inline 的 `VmEvents`），此时无需放开。
    Yield(Option<u64>),
    /// `stop()` 已 dispose VM：转发任务丢弃排队项并退出。
    ///
    /// 退出即释放转发任务持有的 `events`。这一步必须显式做：`Execution` 反过来持有本 channel
    /// （channel 里有队列的发送端），而 `events` 又持有 `Execution`；只要转发任务还堵在
    /// `recv()` 上，这个环就永不释放（TS 里排队的微任务跑完后整个对象图不可达，V8 可以回收）。
    Shutdown,
}

/// `inlineLauncher(wasm, interruptBudget)`：在调用线程上（`vm.rs` 的专用 VM 线程）跑 VM。
///
/// `interrupt_budget` 是每次执行的轮询次数上限（不是毫秒）：QuickJS 的 interrupt handler 与
/// `drain()` 每 50 个 promise job 一次的轮询共用同一个计数，计数超过预算后下一次轮询中断脚本，
/// 执行以 `kind: "timeout"` 结束。`0` 表示第一次轮询就中断。
///
/// 返回的 launcher 可以多次调用：每次新建一个 VM 与一个转发任务。`wasm` 在创建 launcher 时
/// 就切到 inline 栈档（只克隆已编译的 `Module`，不重新编译）。
///
/// TS: `inline.ts#inlineLauncher`。
pub fn inline_launcher(wasm: CodemodeWasmModule, interrupt_budget: u32) -> VmLauncher {
    // TS 的 inline/remote launcher 把 VM 跑在**调用线程**上（Node 主线程约 1 MB 原生栈），
    // 所以深 C 递归比 worker 后端更早以 crash 结束。wasmtime 的栈预算随 Engine 走，这里切到
    // inline 档的编译结果（见 `crate::wasm::CodemodeWasmModule`）。
    let wasm = wasm.with_inline_stack();
    Arc::new(move |data: VmStartData, events: Arc<dyn VmEvents>| {
        let wasm = wasm.clone();
        Box::pin(async move {
            let stopped = Arc::new(AtomicBool::new(false));
            let settled = Arc::new(AtomicBool::new(false));
            let exhausted = Arc::new(AtomicBool::new(false));
            let polls = Arc::new(AtomicU64::new(0));
            // TS 的 VM 跑在调用线程上，宿主事件循环在计算期间不跑。Rust 的 VM 在自己的线程上，
            // 所以用 `execution.rs` 的闸门显式表达「宿主线程被占住」（spec §A.5.2）。
            let host_event_loop = events.host_event_loop();
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<ForwardItem>();

            // `shouldInterrupt`：两条取得控制权的路径（引擎的 interruptHandler 与 `drain()` 每
            // 50 个 job 一次）都调它，因此计数是共用的。
            let should_interrupt = {
                let stopped = Arc::clone(&stopped);
                let settled = Arc::clone(&settled);
                let exhausted = Arc::clone(&exhausted);
                let polls = Arc::clone(&polls);
                Arc::new(move || {
                    if stopped.load(Ordering::SeqCst)
                        || settled.load(Ordering::SeqCst)
                        || exhausted.load(Ordering::SeqCst)
                    {
                        return true;
                    }
                    // `++polls <= interruptBudget`：JS 的 number 不会溢出；Rust 用 u64 + 显式
                    // wrapping，避免 debug 构建在极端长跑时 panic。
                    let count = polls.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
                    if count <= u64::from(interrupt_budget) {
                        return false;
                    }
                    exhausted.store(true, Ordering::SeqCst);
                    true
                })
            };

            // `post`：VM 回调里**只**入队。宿主的处理在转发任务里，VM 回调返回之后才运行。
            let post = {
                let settled = Arc::clone(&settled);
                let sender = sender.clone();
                Arc::new(move |message: WorkerToHostMessage| {
                    let terminal = is_terminal(&message);
                    // 只有第一条终态消息算数：中断一个已结算 VM 造成的 crash 是噪声。
                    if terminal && settled.load(Ordering::SeqCst) {
                        return;
                    }
                    if terminal {
                        settled.store(true, Ordering::SeqCst);
                    }
                    let _ = sender.send(ForwardItem::Message(message));
                })
            };

            // 初始段：VM 从创建跑到第一次让出（或结束）都在这段时间里同步完成，期间调用线程
            // 被占住——闸门在启动 VM 之前就占上。
            let token = host_event_loop.as_ref().map(|gate| gate.set_busy());
            let session = match start_vm(StartVmOptions {
                data,
                wasm,
                max_stack_size: Some(INLINE_MAX_STACK_SIZE),
                interrupt_budget: Some(interrupt_budget),
                post,
                should_interrupt,
            })
            .await
            {
                Ok(session) => session,
                // TS：`await startVm({…})` 的拒绝直接冒泡，`Execution` 取 `errorMessage`.
                Err(error) => {
                    if let (Some(gate), Some(token)) = (&host_event_loop, token) {
                        gate.set_idle(token);
                    }
                    return Err(error.message().to_string());
                }
            };
            // 初始段结束：这一轮的消息已在队列里，转发任务处理到标记时会先交完它们再放开闸门。
            let _ = sender.send(ForwardItem::Yield(token));

            // 转发任务：`queueMicrotask` 的等价物。VM 线程的消息按轮成批交给宿主；宿主
            // （工具调用、`finish()`、dispose）因此在 VM 之外运行，永不重入 VM。
            {
                let events = Arc::clone(&events);
                let stopped = Arc::clone(&stopped);
                let exhausted = Arc::clone(&exhausted);
                let host_event_loop = host_event_loop.clone();
                let mut receiver = receiver;
                tokio::spawn(async move {
                    let mut batch: Vec<WorkerToHostMessage> = Vec::new();
                    // 已经交付过终态消息（done/crash 或预算耗尽造成的 failure）：这次执行的
                    // 结果已定，之后的消息都会被宿主丢弃，转发任务不再需要 `events`，就此退出。
                    let mut finished = false;
                    while let Some(item) = receiver.recv().await {
                        match item {
                            ForwardItem::Message(message) => batch.push(message),
                            ForwardItem::Yield(token) => {
                                for message in batch.drain(..) {
                                    // TS：`queueMicrotask(() => { if (stopped) return; … })`——
                                    // 停下之后排队的微任务整条丢弃。
                                    if stopped.load(Ordering::SeqCst) {
                                        continue;
                                    }
                                    // 中断最终表现为 crash；报告真正的原因。
                                    if exhausted.load(Ordering::SeqCst) && is_terminal(&message) {
                                        events.failure(CodemodeError::new(
                                            CodemodeErrorKind::Timeout,
                                            format!(
                                                "Execution exceeded its interrupt budget of {interrupt_budget}"
                                            ),
                                        ));
                                        finished = true;
                                        continue;
                                    }
                                    // `post` 只放行第一条终态消息，因此这里最多遇到一次。
                                    if is_terminal(&message) {
                                        finished = true;
                                    }
                                    events.message(
                                        serde_json::to_value(&message)
                                            .expect("WorkerToHostMessage serializes"),
                                    );
                                }
                                // 这一轮的微任务交付完，宿主的定时器与 abort 事件才可以跑。
                                if let (Some(gate), Some(token)) = (&host_event_loop, token) {
                                    gate.set_idle(token);
                                }
                                if finished {
                                    break;
                                }
                            }
                            // `stop()`：执行已经结束（或宿主不再要结果），排队的消息按 `stopped`
                            // 丢弃，然后退出并释放 `events`（见 `ForwardItem::Shutdown`）。
                            ForwardItem::Shutdown => break,
                        }
                    }
                });
            }
            // 转发任务在交付终态消息之后、或收到 `stop()` 的 `Shutdown` 之后退出。它因此不会
            // 长期持有 `events`：`Execution` 持有本 channel（含队列发送端），转发任务若一直等
            // 在 `recv()` 上就与它构成永不释放的环（TS 的排队微任务跑完即可被 GC 回收）。

            Ok(Arc::new(InlineChannel {
                stopped,
                session: Mutex::new(Some(session)),
                host_event_loop,
                sender,
            }) as Arc<dyn VmChannel>)
        })
    })
}

/// 宿主对调用线程 VM 的 channel（TS: `{ post(message) { if (!stopped) session.deliver(message) }, stop() }`）。
struct InlineChannel {
    /// 与 `post` 回调、转发任务共享的停止标志；置位后不再投递、也不再交付排队消息。
    stopped: Arc<AtomicBool>,
    /// VM 会话。`stop()` 取走它并 dispose；`post` 只在 `Some` 时投递（`deliver` 需要 `&mut`）。
    session: Mutex<Option<VmSession>>,
    /// 宿主事件循环闸门；`events.host_event_loop()` 为 `None` 时（非 inline 的 `VmEvents`）
    /// 不做任何占位/放开。
    host_event_loop: Option<Arc<HostEventLoop>>,
    /// 转发队列的发送端：`post` 用它把这一轮的让出标记排进队列，`stop` 用它通知转发任务退出
    /// （转发任务据此释放 `events`，见 `ForwardItem::Shutdown`）。
    sender: tokio::sync::mpsc::UnboundedSender<ForwardItem>,
}

impl VmChannel for InlineChannel {
    fn post(&self, message: HostToWorkerMessage) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        // 会话锁把并发 `post` 串行化：`VmSession::deliver` 需要 `&mut self`（`vm.rs`），
        // 因此同一时刻最多一个 `deliver` 在跑。
        let mut session = self.session.lock().expect("vm session mutex");
        // `stop()` 可能已经把 `stopped` 置位并取走会话；再查一次避免对已 dispose 的 VM 投递。
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if let Some(session) = session.as_mut() {
            // `deliver` 同步跑脚本到下一次让出：这段时间宿主线程被占住。占住闸门与入队标记都在
            // 会话锁内完成，保证下一段的忙标记与这一轮的让出标记顺序一致。
            let token = self.host_event_loop.as_ref().map(|gate| gate.set_busy());
            session.deliver(message);
            let _ = self.sender.send(ForwardItem::Yield(token));
        }
    }

    fn stop(&self) -> BoxFuture<'static, ()> {
        // TS：`if (stopped) return; stopped = true; session.dispose();`——只 dispose 一次。
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Box::pin(async {});
        }
        // 注意：若另一任务正持锁在 `post` 里跑脚本，这里会等它跑到下一次让出才拿到会话；
        // `dispose` 本身也是同步的（`vm.rs` 会停掉 VM 线程）。返回的 future 已经完成，但调用
        // 发生在返回之前。
        if let Some(mut session) = self.session.lock().expect("vm session mutex").take() {
            session.dispose();
        }
        // 通知转发任务退出并释放它持有的 `events`。`Execution` 持有本 channel（含上面的发送端），
        // 而 `events` 又持有 `Execution`；转发任务若一直等在 `recv()` 上就构成永不释放的环。
        let _ = self.sender.send(ForwardItem::Shutdown);
        Box::pin(async {})
    }
}

/// 在调用线程上跑每次执行的 QuickJS VM。
///
/// 任何有 WebAssembly 与微任务的运行时都能用：不需要 worker 线程、文件或 Node 内建。计算
/// 而不 await 工具的脚本会阻塞调用线程，直到它 await 工具或用尽中断预算，所以有 worker 线程
/// 时优先用 `CodemodeSandbox`，需要让脚本离开必须保持响应的线程时用 `RemoteCodemodeSandbox`。
///
/// 每次实际启动的执行（`execute()` 或 `start()`）新建一个 VM；sandbox 已关闭或 signal 已预先
/// 取消时不会启动 VM。`close()` 通过 channel 的 `stop()` 同步 dispose 在途会话（`vm.rs` 会
/// 停掉 VM 线程），并在结果结算后返回。
///
/// TS: `inline.ts#InlineCodemodeSandbox extends CodemodeSandboxBase`。
pub struct InlineCodemodeSandbox {
    base: Arc<CodemodeSandboxBase>,
}

impl InlineCodemodeSandbox {
    /// 构造：用 [`inline_launcher`] 起 VM。
    ///
    /// TS 的构造函数会因非法全局名抛错，因此 Rust 侧返回 `Result`。`interrupt_budget` 缺省取
    /// [`DEFAULT_INTERRUPT_BUDGET`]；构造本身不启动 VM，VM 在每次实际启动的
    /// `execute()`/`start()` 时创建（sandbox 已关闭或 signal 已预先取消时不创建）。
    pub fn new(options: InlineCodemodeSandboxOptions) -> Result<Self, StartError> {
        let launch = inline_launcher(
            options.wasm.clone(),
            options.interrupt_budget.unwrap_or(DEFAULT_INTERRUPT_BUDGET),
        );
        let base = CodemodeSandboxBase::new(options.base, launch)?;
        Ok(InlineCodemodeSandbox {
            base: Arc::new(base),
        })
    }

    /// 基类（工具表、`execute`/`start`/`close`）。
    pub fn base(&self) -> &Arc<CodemodeSandboxBase> {
        &self.base
    }

    /// TS: `CodemodeSandboxBase.execute`。
    ///
    /// 每次实际启动的 `execute()`/`start()` 新建一个 VM；sandbox 已关闭或 signal 已预先取消
    /// 时不会启动 VM（前者以 `SandboxClosed` 失败，后者直接以 `aborted` 结算）。脚本自身的
    /// 失败以 `CodemodeResult::Err` 返回。纯计算的脚本会在 VM 线程上跑到结束或预算耗尽；
    /// 期间宿主事件闸门保持占用，deadline 与 abort 事件被推迟（见模块文档）。
    pub fn execute(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
    ) -> BoxFuture<'static, Result<CodemodeResult, crate::runtime::execution::SandboxClosed>> {
        self.base.execute(code, options)
    }

    /// TS: `CodemodeSandboxBase.start`。
    ///
    /// 参数校验在启动 VM 之前完成（`started_at`、`seed`、工具/脚本名唯一、全局名），失败返回
    /// [`StartError`]；默认 deadline 是无穷，与 `execute()` 的 300000 ms 不同。
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
    /// 可以重复调用。`stop()` 里的 `dispose` 是同步的，因此 `close()` 可能等待 VM 线程处理完
    /// Stop。
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

impl std::fmt::Debug for InlineCodemodeSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InlineCodemodeSandbox")
    }
}
