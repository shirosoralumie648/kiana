//! transports/in-memory.ts：`InMemoryTransport` 与 `create_in_memory_transport_pair`。
//!
//! 进程内的一对传输：`client` 与 `server` 互相 `connect_peer`，一侧 `send` 的消息被深克隆后
//! 投递给另一侧的 message 监听器。
//!
//! # 与 TS 的映射要点
//!
//! - **投递在 microtask 上，`await send` 观察得到它**：TS 是 `copy = structuredClone(message)`
//!   之后 `queueMicrotask(() => peer.deliver(copy))`。`send()` 返回的 promise 本身在投递前
//!   落定，但 `await` 的续体排在投递 microtask **之后**（TS 实测：`send-returned` →
//!   `deliver` → `then`），所以调用点的可观察语义是「`await send` 返回时消息已经投递」。
//!   Rust 的 future 没有独立的微任务检查点，[`InMemoryTransport::send`] 因此等待**本条**消息
//!   被对端处理完再返回：返回 `Ok` 就一定表示它已投递（或按 TS 的 `closed` 检查被丢弃）。
//!   并发发送方各自等待自己那条，不会出现「消息还在队列里、`send` 却已返回 Ok」的情况
//!   （评审 finding #1/#2）。
//! - **监听器不在发送方的栈上**：TS 的投递发生在 microtask 里。Rust 侧第一个入队的发送方
//!   `tokio::spawn` 一个排空 task（等价于 `queueMicrotask`），其余发送方等自己那条的完成信号。
//!   监听器因此不会嵌套在发送方（或另一个监听器）的调用栈里（评审 finding #3）。
//!   没有 tokio runtime 时（纯同步调用场景）退化为在当前栈上排空，并在返回前投递完。
//! - **不丢唤醒**：入队与「是否由我排空」在**同一个临界区**决定；排空者的
//!   「取队首 / 队列已空 → 复位 `draining`」也在同一个临界区完成。因此不存在
//!   「入队者看到 `draining == true`、却没有任何排空者会再来看队列」的窗口
//!   （评审 finding #1）。
//! - **FIFO / 不并发**：每个 transport 一个投递队列；同一 transport 的 message 监听器按 TS
//!   单线程事件循环的语义**串行**执行，多个发送方并发 `send` 时按入队顺序投递。监听器调用
//!   期间**不持任何锁**，两个传输互相投递不会 AB/BA 死锁。
//! - **监听器里重入 `send`**：TS 里监听器再 `send` 只是 `queueMicrotask`，消息在本轮 emit
//!   之后才投递（probe：`[1, "send-returned", 2]`）。Rust 的监听器是同步 `Fn`，只能在里面
//!   `block_in_place` + `block_on`；[`InMemoryTransport::send`] 检测到当前线程正在投递时
//!   只入队、不等待（否则自锁），消息仍按 FIFO 排在本轮 emit 之后。
//! - **peer 是强引用**（TS `private peer: InMemoryTransport`）：一侧的句柄被丢弃后，另一侧
//!   仍通过自己的 `peer` 字段持有该对象，`send`/`close` 照常可达（TS 的对象由 GC 保活）。
//!   代价是双向连接形成 `Arc` 环（TS 由 GC 回收）。这里用「外部句柄计数」兜底：最后一个
//!   [`InMemoryTransport`] 句柄被丢弃时切断本侧到 peer 的边，于是整对不可达的 transport
//!   （连同它们持有的监听器闭包）会被释放（评审 finding #4）。
//! - `structuredClone(message)` == 按值传递：[`InMemoryTransport::send`] 取得 [`Value`] 的
//!   所有权，监听器只拿 `&Value`，发送方之后无法再修改它。
//! - `close()` 沿 peer 链**迭代**关闭（TS 递归 `await this.peer?.close()`）：先置 `closed`
//!   再 `emitClose()`，因此环上第二次到达同一个 transport 时立即返回，不会死锁；close
//!   监听器返回错误时对端**不会**被关闭（与 TS 的异常跳过一次 `await this.peer?.close()` 相同）。
//! - message 监听器抛出的异常在 TS 里冒到投递 microtask 上成为 `uncaughtException`：发送方
//!   不受影响（`send` 早已 resolve），同一次 emit 中后续监听器不再调用，之后的消息照常投递。
//!   Rust 侧 [`TransportEvents::emit_message`] 把监听器的异常表现为 `Err`；这里丢弃它
//!   （`send` 仍返回 `Ok`）。监听器 panic 同样被拦在投递 task 里（`catch_unwind`）：panic
//!   hook 会把消息打到 stderr（对应 Node 打印 `uncaughtException`），发送方拿到 `Ok`，
//!   后续消息照常投递（评审 finding #6；`panic = "abort"` 的构建下则整进程终止，与 Node 的
//!   默认未捕获异常一致）。
//! - **已知差异**：Rust 没有全局微任务队列，因此「监听器里入队的消息」与「外层 `send` 的续体」
//!   之间没有 TS 那样确定的先后（TS 的续体先于嵌套 microtask）。嵌套消息保证在本轮 emit 之后
//!   投递，但不保证在 `send` 返回之前。
//!
//! # 在系统中的位置
//!
//! `InMemoryTransport` 是 [`McpTransport`] 的一个实现，只用于**测试**：不打开文件描述符或
//! socket，`send` 只是把消息交给同一进程里的对端。配对入口
//! [`create_in_memory_transport_pair`] 与类型本身由 [`crate::testing`] 再导出
//! （`testing/index.ts` 就是纯再导出，见该模块文档）。仓库里使用它的地方：
//! `crates/kiana-builtin-extensions/tests/mcp_connections.rs` 用一对 in-memory 传输搭假 MCP
//! 服务器，`crates/kiana-mcp/tests/api_surface.rs` 断言公开面形状；`McpClient` 只按
//! `Arc<dyn McpTransport>` 持有传输，不认识这个具体类型。
//!
//! # 一次 send 的执行流程
//!
//! 1. [`InMemoryTransport::send`] 依次检查本侧 `started`/`closed`、对端是否存在、对端的
//!    `started`/`closed`；任一不满足就返回 `McpConnectionClosedError`（消息见该方法文档）。
//! 2. 消息按值推入**对端**的投递队列 `DeliveryState`；入队与「这次是否由我负责排空」在同一个
//!    临界区决定（不丢唤醒的前提）。
//! 3. 此前没有排空者时，调用方 spawn 一个排空 task（等价 TS 的 `queueMicrotask`；没有 tokio
//!    runtime 时退化为当前栈上同步排空）；已有排空者时由它顺带处理。
//! 4. 排空者逐条取队首：`closed` 为真则把本条与整队一起丢弃并结束；否则调用
//!    [`TransportEvents::emit_message`]（同步、串行），随后给该条的发送方发完成信号。
//! 5. 发送方 await 自己那条的完成信号，然后返回 `Ok`。若第 2 步之后发现调用发生在监听器
//!    内部（线程局部标记），则不等待，避免自锁。
//!
//! # 并发与锁
//!
//! - `Inner::peer` 与 `Inner::delivery` 各有一把 `Mutex`；临界区只做队列、`draining`/
//!   `drainer` 标志与 `Arc`/peer 边的短操作（`enqueue`/`drain`/`DrainSession::drop` 改标志，
//!   `connect_peer` 置位 `connected` 后写槽，`close`/`Drop` 取走 peer 边），**监听器调用期间
//!   不持任何锁**。所以监听器里可以再 `send`/`close`，双向同时投递也不会形成 AB/BA 死锁
//!   （测试 `bidirectional_sends_from_two_threads_do_not_deadlock`）。
//! - 同一 transport 最多一个排空者（`DeliveryState::draining`）。排空 task 可能在第一次 poll
//!   之前就被运行时丢弃，角色由 `DrainTaskGuard`/`DrainSession` 在 `Drop` 里归还，队列不会
//!   永远卡在 `draining == true`。
//! - 队列是 `VecDeque`，投递严格按入队顺序；同一 transport 的监听器串行执行，多个发送方并发
//!   时监听器不会重入（测试 `concurrent_senders_never_run_listeners_concurrently`）。
//! - `started`/`closed`/`connected` 是三个独立原子量，`send` 的多次 load 之间不加锁：并发
//!   `close` 落在任意一次检查之后，那条消息仍可能被投递；若排空者在投递前看到 `closed`，则按
//!   TS `deliver` 的检查被丢弃（每条消息投递前都重新检查一次）。
//!
//! # 为什么不直接在 `send` 里调用对端监听器
//!
//! 那会把监听器放到发送方（或另一个监听器）的调用栈上：TS 的 `queueMicrotask` 不会这样，
//! 监听器里再 `send` 还会变成递归投递。这里的「每 transport 一个 FIFO 队列 + 一个排空 task」
//! 复刻了 TS 的事件循环语义，并用每条的完成信号保住「`await send` 返回时已经投递」的可观察
//! 行为（`a_listener_never_runs_on_the_senders_stack` 与
//! `a_listener_that_sends_to_itself_is_delivered_after_the_current_emit` 锁定这两点）。
//!
//! TS: `packages/mcp/src/transports/in-memory.ts`

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::auth_provider::McpTransportError;
use crate::jsonrpc::McpError;
use crate::transports::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, Subscription, TransportEvents,
};

/// `new McpConnectionClosedError("In-memory MCP peer is not connected")`。
fn peer_not_connected() -> McpTransportError {
    McpError::connection_closed_with("In-memory MCP peer is not connected").into()
}

thread_local! {
    /// 当前线程是否正在执行某次 in-memory 投递（含其中的 message 监听器）。
    ///
    /// TS 的监听器运行在 microtask 上：监听器里再 `send` 只会排进队列，不会重入投递。
    /// Rust 的监听器是同步 `Fn`，只能用同样的办法避免自锁：在投递过程中发起的 `send` 只入队、
    /// 不等待投递完成。
    ///
    /// 用线程局部量而不是全局量：只有"与监听器同步地跑在同一线程上"的 `send` 才会自锁。
    /// 监听器另行 spawn 的 task 里发起的 `send` 不在监听器调用栈上，之后执行时读到的是 0
    /// （排空返回时 [`DeliveryMarker`] 已复位），应当正常等待投递完成。
    static DELIVERY_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// [`DELIVERY_DEPTH`] 的守卫（panic 展开时也会复位）。
struct DeliveryMarker;

impl DeliveryMarker {
    fn enter() -> Self {
        DELIVERY_DEPTH.with(|depth| depth.set(depth.get() + 1));
        DeliveryMarker
    }
}

impl Drop for DeliveryMarker {
    fn drop(&mut self) {
        DELIVERY_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// 当前线程是否正在跑某次投递（`DELIVERY_DEPTH > 0`），见 [`DELIVERY_DEPTH`]。
fn inside_delivery() -> bool {
    DELIVERY_DEPTH.with(|depth| depth.get() > 0)
}

/// 排队中的一条消息：`peer.deliver(copy)` 以及「本条已处理完」的信号。
struct Pending {
    message: Value,
    /// 本条处理完（投递过或按 `closed` 检查被丢弃）时触发；`send` 在返回前等它。
    ///
    /// 丢弃发送端（例如关闭后清空队列）会让等待者收到 `Err`，`send` 仍然返回 `Ok`——
    /// TS 的 `send` 的 promise 早已落定。
    done: Option<oneshot::Sender<()>>,
}

/// 进程内传输（`transports/in-memory.ts` 的 `InMemoryTransport`）。
///
/// 句柄本身很轻（一个 [`Arc`]）；[`InMemoryTransport::connect_peer`] 之后对端用[强引用](Arc)
/// 反向持有，与 TS 的对象引用一致（一侧句柄被丢弃后另一侧仍能投递/关闭）。
/// 注意连接是单向的：`A.connect_peer(B)` 只让 A 持有 B，B 不会因此持有 A；两侧各调一次才
/// 形成双向的 `Arc` 环。
/// [`Clone`] 克隆的是**句柄**（共享同一状态），与 JS 里复制对象引用的语义一致，不是新建一个
/// transport。
///
/// 所有方法都取 `&self`：既可以直接用作 `InMemoryTransport`，也可以包成
/// `Arc<dyn McpTransport>` 交给 `McpClient`（`McpClient` 就是按 `Arc<dyn McpTransport>` 使用的）。
pub struct InMemoryTransport {
    inner: Arc<Inner>,
}

/// 一个 [`InMemoryTransport`] 的状态（同一 transport 的所有句柄共享）。
struct Inner {
    /// message/error/close 监听器（`transport.ts` 的 `TransportEvents`）。
    events: TransportEvents,
    /// 对端（TS 的 `private peer: InMemoryTransport`）。
    ///
    /// 强引用：只要一方还有可用句柄，对端的对象就可达（TS 的对象引用语义）。
    /// [`InMemoryTransport::close`] 关闭整条链后清空；本 transport 的最后一个句柄被丢弃时
    /// 也会清空，让不可达的 pair 不再形成 `Arc` 环。
    peer: Mutex<Option<Arc<Inner>>>,
    /// `this.peer` 是否曾经被设置（TS 的 `if (this.peer) throw`）。
    ///
    /// 与 `peer` 槽分开记录：`close()` 会清空强引用，但 TS 里 `this.peer` 仍然存在，
    /// 之后再 `connectPeer` 仍是同一条逐字错误。
    connected: AtomicBool,
    /// `started` 标志（TS 的同名字段）。
    started: AtomicBool,
    /// `closed` 标志（TS 的同名字段）。
    closed: AtomicBool,
    /// 投递队列（保证同一条传输的 message 监听器**不并发**且按入队序执行）。
    delivery: Mutex<DeliveryState>,
    /// 外部 [`InMemoryTransport`] 句柄数（peer 的强引用不计）。
    ///
    /// 归零时切断本侧到 peer 的边，使不可达的 transport（及其监听器闭包）能够被释放。
    handles: AtomicUsize,
}

/// [`Inner::delivery`] 的内容。
#[derive(Default)]
struct DeliveryState {
    /// 等待投递的消息（FIFO）。
    queue: VecDeque<Pending>,
    /// 是否已有排空者在跑。
    draining: bool,
    /// 当前排空者的编号（每次接任时递增）。
    ///
    /// 排空 task 若在运行前就被丢弃（例如 runtime 关停），它的守卫据此判断自己是否仍然是
    /// 指定的排空者，是则归还角色，避免 `draining` 永远为真、队列再没人处理。
    drainer: u64,
}

impl Inner {
    /// 取 `peer` 锁；锁中毒时继续使用内部值。
    ///
    /// `peer` 槽的写入与 `connected` 的原子置位在同一个临界区内依次完成（`connect_peer`
    /// 先 `swap` 再写槽），`peer()` 取对端也持同一把锁，因此看不到"已连接但还没有 peer"的
    /// 中间态。即使持锁线程 panic 导致 `connected == true` 而槽为空，这也与 `close()` 之后
    /// 的合法状态一致（`close()` 只清槽、不复位 `connected`），不需要把锁中毒传染给后续
    /// 调用方（同 `client` 模块的做法）。
    fn lock_peer(&self) -> MutexGuard<'_, Option<Arc<Inner>>> {
        self.peer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 取投递队列锁；锁中毒时继续使用内部值（理由同 [`Inner::lock_peer`]）。
    fn lock_delivery(&self) -> MutexGuard<'_, DeliveryState> {
        self.delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 当前对端（`close()` 清空链接后为 `None`）。
    fn peer(&self) -> Option<Arc<Inner>> {
        self.lock_peer().clone()
    }

    /// 把消息排进本 transport 的投递队列（TS 的 `queueMicrotask(() => deliver(copy))`）。
    ///
    /// 返回等待「本条已处理完」的接收端：
    ///
    /// - `Some(rx)`：调用方不在投递过程中，等它即可得到 TS `await send` 的可观察语义；
    /// - `None`：本条已在当前栈上同步投递完（无 tokio runtime 的退化路径），或调用方正处于
    ///   投递过程中（监听器里重入 `send`），不能等待（否则自锁）。
    ///
    /// 「入队」与「是否由我排空」在同一个临界区决定，因此不会出现消息已经入队、却没有任何
    /// 排空者会再来看它的窗口。
    fn enqueue(self: &Arc<Self>, message: Value) -> Option<oneshot::Receiver<()>> {
        let (done, wait) = oneshot::channel();
        let drainer_id = {
            let mut state = self.lock_delivery();
            state.queue.push_back(Pending {
                message,
                done: Some(done),
            });
            if state.draining {
                None
            } else {
                state.draining = true;
                state.drainer += 1;
                Some(state.drainer)
            }
        };
        if let Some(drainer_id) = drainer_id {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    let inner = Arc::clone(self);
                    // 守卫在投递开始前就构造，随 future 一起被丢弃：即使 task 在第一次 poll
                    // 之前就被运行时丢弃，也会归还「排空者」角色。
                    let guard = DrainTaskGuard {
                        inner: Arc::clone(self),
                        drainer_id,
                    };
                    handle.spawn(async move {
                        let _guard = guard;
                        inner.drain();
                    });
                }
                // 没有 tokio runtime（纯同步调用场景）：退化为当前栈上同步排空。此时监听器会
                // 在调用方栈上运行，但这是唯一能保证消息不丢的办法（文档已注明）。
                Err(_) => {
                    self.drain();
                    return None;
                }
            }
        }
        if inside_delivery() { None } else { Some(wait) }
    }

    /// 排空投递队列，直到为空；监听器运行期间不持任何锁。
    ///
    /// 队列空与 `draining` 复位在同一个临界区完成（不丢唤醒）。排空过程中
    /// [`Inner::closed`] 为真时，本条与队列里剩下的消息都被丢弃（TS 里每条 microtask 的
    /// `deliver` 都会重新检查 `closed`）。
    fn drain(self: &Arc<Self>) {
        let mut session = DrainSession {
            inner: self,
            finished: false,
        };
        // 整个排空过程（含监听器调用）都标记为"投递中"：监听器里同步发起的 `send` 只入队、
        // 不等待，否则可能等待本线程正在执行的排空者（自连接时即自己）。线程局部标记不区分
        // 目标，因此该线程上的 `send` 一律不等待；普通双向 pair 里服务端监听器回复客户端时，
        // 等的是客户端队列上的另一个排空 task，这里只是为自连接/同线程重入做的保守处理。
        let _marker = DeliveryMarker::enter();
        loop {
            let next = {
                let mut state = self.lock_delivery();
                match state.queue.pop_front() {
                    Some(pending) => pending,
                    None => {
                        // 队列空与 `draining` 复位在同一个临界区完成；`finished` 告诉
                        // `DrainSession::drop` 这是正常收尾，不要另起排空者。
                        state.draining = false;
                        session.finished = true;
                        return;
                    }
                }
            };
            if self.closed.load(Ordering::SeqCst) {
                // 本条与之后排队的消息都丢弃；「清空队列」与「复位 draining」同一临界区。
                let dropped: Vec<Pending> = {
                    let mut state = self.lock_delivery();
                    state.draining = false;
                    state.queue.drain(..).collect()
                };
                session.finished = true;
                // 在锁外丢弃 `Pending`：`done` 的发送端被丢弃会让等待者收到 `Err`，而
                // `send` 忽略该错误照常返回 `Ok`（TS 的 promise 早已落定）。
                drop(next);
                drop(dropped);
                return;
            }
            // TS 的监听器异常冒到投递 microtask 上成为 `uncaughtException`：发送方不受影响，
            // 同一次 emit 的后续监听器不再调用，之后的消息照常投递。这里吞掉 panic 并继续。
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = self.events.emit_message(&next.message);
            }));
            // 完成信号在 emit 之后**无条件**发送：监听器返回 `Err` 或 panic 都不改变
            // 发送方的结果（发送方只关心"本条已处理完"）。
            if let Some(done) = next.done {
                let _ = done.send(());
            }
        }
    }

    /// 关闭单个 transport 并触发一次 `emitClose`。
    ///
    /// 返回 `false` 表示它**已经**关闭过（调用方的链式关闭到此为止，与 TS 的
    /// `if (this.closed) return;` 一致）。
    ///
    /// `closed` 用 `swap` 原子地"检查并置位"，并发调用里只有一个会走到 `emit_close`。
    /// 置位发生在 `emit_close` **之前**：close 监听器看到的本 transport 已是关闭状态；
    /// 即使 `emit_close` 返回错误，`closed` 也保持为真（测试
    /// `a_failing_close_listener_propagates_and_leaves_the_peer_open` 里第二次 `close()`
    /// 因此正常返回 `Ok`，但监听器不会再被调用）。
    fn close_once(&self) -> Result<bool, McpTransportError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(false);
        }
        self.events.emit_close()?;
        Ok(true)
    }
}

/// 排空 task 的角色守卫：task 被丢弃时若自己仍是指定的排空者，归还角色。
///
/// 正常跑完时 [`Inner::drain`] 已经把 `draining` 复位（`drainer` 编号未变，再复位一次是
/// no-op）；若其间已有新的排空者接任，编号不同，这里不会误清它的标志。
struct DrainTaskGuard {
    inner: Arc<Inner>,
    drainer_id: u64,
}

impl Drop for DrainTaskGuard {
    fn drop(&mut self) {
        let mut state = self.inner.lock_delivery();
        if state.drainer == self.drainer_id {
            state.draining = false;
        }
    }
}

/// [`Inner::drain`] 的异常路径守卫：正常返回时 `finished` 为真、不做事。
///
/// 若排空过程中出现意料之外的 panic（监听器的 panic 已在 [`Inner::drain`] 里捕获），把
/// 剩余消息交给新的排空者，避免 `draining` 永远为真、队列再也没人处理。
struct DrainSession<'a> {
    inner: &'a Arc<Inner>,
    finished: bool,
}

impl Drop for DrainSession<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let has_leftover = !self.inner.lock_delivery().queue.is_empty();
        if has_leftover && let Ok(handle) = tokio::runtime::Handle::try_current() {
            let inner = Arc::clone(self.inner);
            // 先把编号加一：仍在等待的旧 DrainTaskGuard（例如本次 drain 所在 task 的守卫）
            // 之后会因编号不匹配而不再动 `draining`，排空者角色只归新 task。`draining` 保持为真。
            let drainer_id = {
                let mut state = inner.lock_delivery();
                state.drainer += 1;
                state.drainer
            };
            let guard = DrainTaskGuard {
                inner: Arc::clone(&inner),
                drainer_id,
            };
            handle.spawn(async move {
                let _guard = guard;
                inner.drain();
            });
            return;
        }
        // 没有 runtime 或队列已空：复位标志，剩余消息（如果有）等下一次 `enqueue` 再处理。
        self.inner.lock_delivery().draining = false;
    }
}

impl InMemoryTransport {
    /// `new InMemoryTransport()`：未连接、未启动。
    pub fn new() -> Self {
        InMemoryTransport {
            inner: Arc::new(Inner {
                events: TransportEvents::new(),
                peer: Mutex::new(None),
                connected: AtomicBool::new(false),
                started: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                delivery: Mutex::new(DeliveryState::default()),
                handles: AtomicUsize::new(1),
            }),
        }
    }

    /// `connectPeer(peer)`：把 `peer` 设为本 transport 的对端。
    ///
    /// 只检查**本侧**是否已有 peer（TS 同）；重复连接抛普通 `Error`，消息逐字
    /// `In-memory MCP transport already has a peer`。注意连接是单向的：要双向投递就两侧各调
    /// 一次（[`create_in_memory_transport_pair`] 就是这么做的）。
    ///
    /// - 错误的 `name()` 是 `"Error"`（`McpTransportError::new`），不是
    ///   `McpConnectionClosedError`；测试 `connect_peer_twice_errors_with_the_typescript_message`
    ///   逐字断言这一对名字与消息。
    /// - `connected` 一旦置位就不再复位（`close()` 只清强引用），所以关闭之后再
    ///   `connect_peer` 仍是同一条错误（测试
    ///   `connect_peer_after_close_still_reports_the_duplicate_peer_error`）。
    /// - 目标一侧的状态不被检查：连到已经有 peer 的 transport、甚至连到自己都是允许的
    ///   （自连接见测试 `a_transport_can_connect_to_itself`）。
    /// - 写入 `peer` 槽与置位 `connected` 在同一个临界区（先 `swap` 检查、再写槽），
    ///   `send` 取对端时也要拿这把 `peer` 锁，因此看不到"已连接但还没有 peer"的中间态。
    pub fn connect_peer(&self, peer: &InMemoryTransport) -> Result<(), McpTransportError> {
        let mut slot = self.inner.lock_peer();
        if self.inner.connected.swap(true, Ordering::SeqCst) {
            return Err(McpTransportError::new(
                "In-memory MCP transport already has a peer",
            ));
        }
        *slot = Some(Arc::clone(&peer.inner));
        Ok(())
    }

    /// `start()`：标记已启动（TS 里 `start` 也只置 `started = true`）。
    ///
    /// 已关闭时抛 `McpConnectionClosedError()`（默认消息 `MCP connection closed`）；重复调用
    /// 是 no-op。
    ///
    /// 不检查 `connected`：没有 peer 也能 `start`，随后 `send` 会得到 peer 那条错误（测试
    /// `send_without_a_peer_reports_the_peer_message`）。检查 `closed` 与置位 `started` 是两次
    /// 独立的原子操作，并发 `close()` 可能插在中间；这不会让 `send` 漏掉关闭：`started` 为假
    /// 时 `send` 本来就报错，`started` 为真时它随后仍会检查 `closed`。
    pub async fn start(&self) -> Result<(), McpTransportError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(McpError::connection_closed().into());
        }
        self.inner.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// `send(message)`：深克隆消息并交给 peer 的 message 监听器。
    ///
    /// - 本侧未启动或已关闭 → `McpConnectionClosedError()`（`MCP connection closed`）；
    /// - 没有 peer、peer 未启动或 peer 已关闭 →
    ///   `McpConnectionClosedError("In-memory MCP peer is not connected")`。
    ///
    /// 返回 `Ok` 时本条消息已经处理完（投递过，或按 TS 的 `closed` 检查被丢弃）——与
    /// `await send` 在 TS 调用点的可观察语义一致；并发发送方各自等待自己那条，不会丢失。
    /// 唯一例外是在 message 监听器内部调用（当前线程正在投递）：此时只入队、不等投递完成，
    /// 与 TS 的 `queueMicrotask` 语义一致（见模块文档）。
    /// 监听器返回的错误/panic 都不影响 `send` 的结果（TS 里那是一次投递 microtask 上的
    /// `uncaughtException`）。
    ///
    /// TS 的 `send` 不接受 `options`，这里同样不接受；[`McpTransport::send`] 的
    /// [`McpTransportSendOptions`] 被忽略：in-memory 传输没有任何取消点，取消信号无处生效。
    ///
    /// 检查顺序是"本侧优先于对端"：先看本侧 `started`/`closed`，再看对端。因此一对已经互相
    /// `close()` 的传输报告的是本侧的默认消息（测试
    /// `closing_the_peer_also_closes_the_sender`）；本侧检查通过后，只要 peer 缺失、peer 未
    /// `start` 或 peer 已关闭，都会返回 peer 那条消息（测试
    /// `send_without_a_peer_reports_the_peer_message`、`send_requires_a_started_peer`、
    /// `a_one_way_link_delivers_and_close_propagates_along_it` 与
    /// `a_failing_close_listener_propagates_and_leaves_the_peer_open`）。
    ///
    /// 固有方法不读 `connected` 标志；[`McpTransport`] 的实现会把传入的
    /// [`McpTransportSendOptions`] 直接丢掉（取消信号被忽略），所以已取消的 signal 不会阻止
    /// 投递（测试 `transport_object_sends_with_aborted_options`）。
    pub async fn send(&self, message: Value) -> Result<(), McpTransportError> {
        if !self.inner.started.load(Ordering::SeqCst) || self.inner.closed.load(Ordering::SeqCst) {
            return Err(McpError::connection_closed().into());
        }
        let Some(peer) = self.inner.peer() else {
            return Err(peer_not_connected());
        };
        if !peer.started.load(Ordering::SeqCst) || peer.closed.load(Ordering::SeqCst) {
            return Err(peer_not_connected());
        }
        // `structuredClone(message)` 之后 `queueMicrotask(() => peer.deliver(copy))`：
        // 消息按值进入对端的投递队列。
        if let Some(done) = peer.enqueue(message) {
            // 排空者在 `deliver` 之后发信号；队列在关闭时被清空同样结束等待（TS 里 `send`
            // 的 promise 早已落定）。
            let _ = done.await;
        }
        Ok(())
    }

    /// `close()`：关闭本 transport 以及对端（递归/链式），每个 transport 只 `emitClose` 一次。
    ///
    /// 幂等。close 监听器返回错误时立刻返回该错误，对端**不会**被关闭（TS 的异常同样跳过
    /// `await this.peer?.close()`）。链式关闭完成后清掉强引用，避免双向连接的 `Arc` 环泄漏。
    ///
    /// 实现是**迭代**而不是递归：每轮取当前节点的 peer、关闭当前节点、清掉它的 peer 边，再走
    /// 向下一个节点。环（双向连接或自连接）第二次到达同一个节点时 `close_once` 返回 `false`，
    /// 循环在那里结束（测试 `a_transport_can_connect_to_itself`）。
    ///
    /// 错误路径的清理范围：`close_once()?` 提前返回时，**当前节点**的 peer 边还没有被清掉
    /// （`closed` 已在 `close_once` 里置位）。再次调用 `close()` 会落到"已关闭"分支，先清掉
    /// 这条边再结束，因此不会留下永久的强引用。
    ///
    /// close 监听器运行时不持任何锁（`peer()` 取出 `Arc` 后锁就释放了），所以在监听器里重入
    /// `close()` 不会死锁：`closed` 已置位，重入者立即走到"已关闭"分支返回 `Ok`，也不会重复
    /// `emitClose`。
    pub async fn close(&self) -> Result<(), McpTransportError> {
        let mut current = Some(Arc::clone(&self.inner));
        while let Some(node) = current {
            // 先取 next（`close_once` 之后链接会被清掉）。
            let next = node.peer();
            let was_open = node.close_once()?;
            node.lock_peer().take();
            if !was_open {
                break;
            }
            current = next;
        }
        Ok(())
    }

    /// `emitError(error)`：测试用的公开钩子，模拟传输层错误。
    ///
    /// TS 里是 `override emitError(error: unknown)`（`toError` 之后分发给 error 监听器）；
    /// Rust 侧错误已经归一为 [`McpTransportError`]，因此直接分发。
    ///
    /// 与 message 监听器不同，error 监听器返回的 `Err` **不会**被吞掉：它从这里返回给调用方，
    /// 并中断同一次 emit 里后续的监听器（测试 `emit_error_propagates_listener_errors`；
    /// 对应 `transport.ts` 的 `emitError` 不 try/catch）。
    pub fn emit_error(&self, error: &McpTransportError) -> Result<(), McpTransportError> {
        self.inner.events.emit_error(error)
    }

    /// `onMessage(listener)`。
    ///
    /// 监听器在投递排空 task 里被调用（不在发送方栈上；没有 tokio runtime 的退化路径例外，
    /// 见模块文档），同一 transport 的监听器按注册顺序**串行**执行。返回的 [`Subscription`]
    /// 可随时退订，退订是幂等的，且在投递过程中也立即生效（`TransportEvents` 的实时游标
    /// 语义）。
    /// 监听器返回的 `Err` 或 panic 不会传回发送方：`send` 仍返回 `Ok`，同一次 emit 里后续
    /// 监听器不再被调用，之后的消息照常投递（见模块文档与测试
    /// `a_listener_error_does_not_fail_send_and_later_messages_still_arrive`）。
    pub fn on_message<F>(&self, listener: F) -> Subscription
    where
        F: Fn(&Value) -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        self.inner.events.on_message(listener)
    }

    /// `onError(listener)`。
    ///
    /// 监听器只有在有人调用 [`InMemoryTransport::emit_error`]（测试钩子）时才会被触发；
    /// 返回的 `Err` 会从 `emit_error` 返回给调用方并中断后续监听器（见该方法文档）。
    pub fn on_error<F>(&self, listener: F) -> Subscription
    where
        F: Fn(&McpTransportError) -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        self.inner.events.on_error(listener)
    }

    /// `onClose(listener)`。
    ///
    /// 每个 transport 的 close 监听器最多被触发一次（`close_once` 置位 + `emit_close` 自身的
    /// 一次性守卫）；关闭之后才注册的监听器不会补发。监听器返回的 `Err` 会从
    /// [`InMemoryTransport::close`] 返回给调用方，并让链式关闭跳过对端（见该方法文档）。
    pub fn on_close<F>(&self, listener: F) -> Subscription
    where
        F: Fn() -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        self.inner.events.on_close(listener)
    }
}

impl Default for InMemoryTransport {
    /// 等价于 [`InMemoryTransport::new`]（`new InMemoryTransport()`）。
    fn default() -> Self {
        InMemoryTransport::new()
    }
}

impl Clone for InMemoryTransport {
    /// 克隆的是**句柄**：与 JS 里复制对象引用一致，共享同一份状态。
    fn clone(&self) -> Self {
        self.inner.handles.fetch_add(1, Ordering::SeqCst);
        InMemoryTransport {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for InMemoryTransport {
    /// 最后一个外部句柄消失时切断本侧到 peer 的边。
    ///
    /// TS 里一对互相引用的对象由 GC 回收；Rust 的强引用会形成 `Arc` 环。只要还有任何一方的
    /// 句柄存活，另一方的对象就必须保持可达（TS 的引用语义），因此不能在单个句柄消失时就清
    /// 链接；只在本 transport 彻底不可再被调用时（句柄数为 0）清，随后整对不可达的 transport
    /// 会被依次释放。
    fn drop(&mut self) {
        if self.inner.handles.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        self.inner.lock_peer().take();
    }
}

/// 只打印三个标志位：不展开 `peer`（双向连接会成环，而且读它要拿锁），也不打印监听器
/// （`TransportEvents` 没有 `Debug` 实现）。三个字段都是原子量，打印不会阻塞或触发投递。
impl fmt::Debug for InMemoryTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryTransport")
            .field("started", &self.inner.started.load(Ordering::SeqCst))
            .field("closed", &self.inner.closed.load(Ordering::SeqCst))
            .field("connected", &self.inner.connected.load(Ordering::SeqCst))
            .finish()
    }
}

/// [`McpTransport`] 实现：`start`/`send`/`close` 转发到同名固有方法（`send` 丢弃
/// [`McpTransportSendOptions`]），三个 `on_*` 直接委托给内部的 [`TransportEvents`]。
// 同 `transports.rs`：`async-trait` 生成的 `#[must_use]` 触发 `double_must_use`。
#[allow(clippy::double_must_use)]
#[async_trait]
impl McpTransport for InMemoryTransport {
    async fn start(&self) -> Result<(), McpTransportError> {
        InMemoryTransport::start(self).await
    }

    async fn send(
        &self,
        message: Value,
        _options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        InMemoryTransport::send(self, message).await
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        InMemoryTransport::close(self).await
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.inner
            .events
            .on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.inner.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.inner.events.on_close(move || listener())
    }
}

/// [`create_in_memory_transport_pair`] 的返回值（TS 的 `{ client, server }`）。
///
/// TS 返回匿名对象字面量，字段名就是 `client`/`server`；Rust 用同名字段的结构体，避免位置
/// 元组让调用方弄反方向（spec §16 的形状）。
pub struct InMemoryTransportPair {
    /// 客户端一侧。
    pub client: InMemoryTransport,
    /// 服务端一侧。
    pub server: InMemoryTransport,
}

impl InMemoryTransportPair {
    /// 按位置解构（`const { client, server } = pair` 的位置版）。
    pub fn into_parts(self) -> (InMemoryTransport, InMemoryTransport) {
        (self.client, self.server)
    }
}

/// `createInMemoryTransportPair()`：新建一对互相连接的传输。
///
/// 返回的 `client`/`server` 都已经 `connect_peer` 到对方（但都还没 `start`——在 `start`
/// 之前 `send` 会得到 `McpConnectionClosedError`）。
/// `packages/mcp/src/transports/in-memory.ts` 把它定义在本文件，`testing/index.ts` 只再导出；
/// Rust 侧同（[`crate::testing`] 再导出这个函数）。
///
/// 两侧互相持有强引用，形成 `Arc` 环：显式 [`InMemoryTransport::close`] 会清掉链接；没有
/// 关闭时由 [`InMemoryTransport`] 的 `Drop` 在最后一个外部句柄消失时切断本侧到 peer 的边，
/// 整对随即被释放（测试 `dropping_an_unclosed_pair_releases_both_transports`）。
pub fn create_in_memory_transport_pair() -> InMemoryTransportPair {
    let client = InMemoryTransport::new();
    let server = InMemoryTransport::new();
    // 新建的 transport 一定没有 peer，`connect_peer` 不会失败；两侧都连接后就形成环。
    let _ = client.connect_peer(&server);
    let _ = server.connect_peer(&client);
    InMemoryTransportPair { client, server }
}

#[cfg(test)]
mod tests {
    //! 契约与回归测试。分组：
    //!
    //! - **投递可见性/顺序**：`await send` 之后消息已投递、FIFO、并发发送不丢消息；
    //! - **监听器**：不在发送方栈上、不并发、不嵌套重入、错误/panic 不影响发送方；
    //! - **关闭**：链式关闭、幂等、close 监听器报错的行为、关闭时丢弃排队消息；
    //! - **引用与回收**：peer 强引用、句柄克隆、未关闭的 pair 不泄漏。
    //!
    //! 许多测试的文档注明了对应的 TS 实测 probe（`probe A`/`probe B2`/`probe D`/`probe P1` 等），
    //! 那些顺序是参考实现上跑出来的；改动行为前先读这些注释与模块文档的「已知差异」。
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    /// 等监听器观察到消息（投递是异步 task，测试里给它调度机会）。
    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        for _ in 0..5_000 {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("delivery did not happen within 5s");
    }

    /// 在 `transport` 上注册一个 message 记录器，返回记录表与退订句柄。
    fn record_messages(transport: &InMemoryTransport) -> (Arc<Mutex<Vec<Value>>>, Subscription) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let subscription = transport.on_message(move |message: &Value| {
            sink.lock().unwrap().push(message.clone());
            Ok(())
        });
        (seen, subscription)
    }

    #[tokio::test]
    async fn pair_delivers_messages_in_send_order() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let (seen, _subscription) = record_messages(&server);

        server.start().await.unwrap();
        client.start().await.unwrap();
        for id in 1..=4 {
            client
                .send(json!({"jsonrpc": "2.0", "id": id, "method": "x", "params": {"n": id}}))
                .await
                .unwrap();
        }
        wait_until(|| seen.lock().unwrap().len() == 4).await;

        let delivered = seen.lock().unwrap().clone();
        let ids: Vec<Value> = delivered
            .iter()
            .map(|message| message["id"].clone())
            .collect();
        assert_eq!(ids, vec![json!(1), json!(2), json!(3), json!(4)]);
        assert_eq!(
            delivered[0],
            json!({"jsonrpc": "2.0", "id": 1, "method": "x", "params": {"n": 1}})
        );
    }

    /// TS 实测（probe A）：`await client.send(...)` 之后消息**已经**被投递。
    ///
    /// TS 的 microtask 顺序是 `send-returned` → `deliver` → `then`（probe A2），所以
    /// `await send` 的续体一定看得见投递结果。回归评审 finding #1/#2。
    #[tokio::test]
    async fn awaiting_send_observes_the_message_already_delivered() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let (seen, _subscription) = record_messages(&server);

        server.start().await.unwrap();
        client.start().await.unwrap();
        client
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        // 不需要任何 yield/sleep：TS 的 `await send` 之后同样立刻可见。
        assert_eq!(
            *seen.lock().unwrap(),
            vec![json!({"jsonrpc": "2.0", "id": 1, "method": "x"})]
        );
    }

    /// 多线程 runtime 下，`await send` 之后投递必须**总是**已完成（评审 finding #2 的
    /// 原始复现：另一个线程在投递时，`send` 曾经提前返回）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn delivery_is_deterministic_on_a_multi_thread_runtime() {
        for iteration in 0..500 {
            let pair = create_in_memory_transport_pair();
            let (client, server) = pair.into_parts();
            let (seen, _subscription) = record_messages(&server);
            server.start().await.unwrap();
            client.start().await.unwrap();
            client.send(json!({"id": iteration})).await.unwrap();
            assert_eq!(
                seen.lock().unwrap().len(),
                1,
                "message {iteration} was not delivered before `send` returned"
            );
            // 关闭以清掉双向强引用。
            client.close().await.unwrap();
        }
    }

    /// 回归评审 finding #1（丢消息）与 #2（`send` 提前返回）：多个并发发送方、每轮都检查
    /// 「我的第 n 条已投递」，最后一条不落。旧实现会在 8000 轮里丢掉 2 轮的消息。
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_senders_never_lose_or_strand_a_message() {
        const SENDERS: usize = 8;
        const PER_SENDER: usize = 5;
        for round in 0..200 {
            let pair = create_in_memory_transport_pair();
            let (client, server) = pair.into_parts();
            let received = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&received);
            let _subscription = server.on_message(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            server.start().await.unwrap();
            client.start().await.unwrap();

            let mut senders = Vec::new();
            for sender in 0..SENDERS {
                let transport = client.clone();
                let received = Arc::clone(&received);
                senders.push(tokio::spawn(async move {
                    for n in 1..=PER_SENDER {
                        transport
                            .send(json!({"id": sender * 100 + n}))
                            .await
                            .unwrap();
                        // 本条 `send` 返回时它必须已经投递（可能还夹着别人的消息）。
                        assert!(
                            received.load(Ordering::SeqCst) >= n,
                            "round {round}: sender {sender} message {n} returned before delivery"
                        );
                    }
                }));
            }
            for sender in senders {
                sender.await.unwrap();
            }
            // 所有 `send` 都返回后不应再有消息滞留在队列里。
            assert_eq!(
                received.load(Ordering::SeqCst),
                SENDERS * PER_SENDER,
                "round {round}: messages stranded in the delivery queue"
            );
            assert!(client.inner.lock_delivery().queue.is_empty());
            client.close().await.unwrap();
        }
    }

    /// TS 实测（probe B2）：`await send` 之后 `close()`，消息已经投递出去。
    #[tokio::test]
    async fn delivery_precedes_close_after_an_awaited_send() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let (seen, _subscription) = record_messages(&server);

        server.start().await.unwrap();
        client.start().await.unwrap();
        client
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        server.close().await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn send_requires_a_started_peer() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();

        // 本侧未启动。
        let error = client.send(json!({})).await.unwrap_err();
        assert_eq!(error.name(), "McpConnectionClosedError");
        assert_eq!(error.to_string(), "MCP connection closed");

        client.start().await.unwrap();
        // 对端未启动。
        let error = client.send(json!({})).await.unwrap_err();
        assert_eq!(error.name(), "McpConnectionClosedError");
        assert_eq!(error.to_string(), "In-memory MCP peer is not connected");

        server.start().await.unwrap();
        client.send(json!({})).await.unwrap();
    }

    #[tokio::test]
    async fn send_without_a_peer_reports_the_peer_message() {
        let transport = InMemoryTransport::new();
        transport.start().await.unwrap();
        let error = transport.send(json!({})).await.unwrap_err();
        assert_eq!(error.name(), "McpConnectionClosedError");
        assert_eq!(error.to_string(), "In-memory MCP peer is not connected");
    }

    #[test]
    fn connect_peer_twice_errors_with_the_typescript_message() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let other = InMemoryTransport::new();
        let error = client.connect_peer(&other).unwrap_err();
        assert_eq!(error.name(), "Error");
        assert_eq!(
            error.to_string(),
            "In-memory MCP transport already has a peer"
        );
        // 每一侧各自检查，server 也已经连过。
        assert!(server.connect_peer(&other).is_err());
    }

    /// TS 实测（probe2 C）：`close()` 之后再 `connectPeer` 仍是同一条"已有 peer"错误
    /// （TS 从不清空 `this.peer`）。
    #[tokio::test]
    async fn connect_peer_after_close_still_reports_the_duplicate_peer_error() {
        let pair = create_in_memory_transport_pair();
        let (client, _server) = pair.into_parts();
        client.close().await.unwrap();
        let other = InMemoryTransport::new();
        assert_eq!(
            client.connect_peer(&other).unwrap_err().to_string(),
            "In-memory MCP transport already has a peer"
        );
    }

    #[tokio::test]
    async fn start_twice_is_a_no_op_and_start_after_close_fails() {
        let transport = InMemoryTransport::new();
        transport.start().await.unwrap();
        transport.start().await.unwrap();
        transport.close().await.unwrap();
        let error = transport.start().await.unwrap_err();
        assert_eq!(error.name(), "McpConnectionClosedError");
        assert_eq!(error.to_string(), "MCP connection closed");
        // 关闭之后 send 也是同一条默认消息（即使本 transport 有 peer）。
        let error = transport.send(json!({})).await.unwrap_err();
        assert_eq!(error.to_string(), "MCP connection closed");
    }

    #[tokio::test]
    async fn close_closes_the_peer_recursively_and_emits_close_once() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        client.start().await.unwrap();
        server.start().await.unwrap();

        let client_closes = Arc::new(AtomicUsize::new(0));
        let server_closes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&client_closes);
        let _client_subscription = client.on_close(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let counter = Arc::clone(&server_closes);
        let _server_subscription = server.on_close(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        client.close().await.unwrap();
        assert_eq!(client_closes.load(Ordering::SeqCst), 1);
        assert_eq!(server_closes.load(Ordering::SeqCst), 1);

        // 幂等：两侧再关一次都不会重复 emit。
        client.close().await.unwrap();
        server.close().await.unwrap();
        assert_eq!(client_closes.load(Ordering::SeqCst), 1);
        assert_eq!(server_closes.load(Ordering::SeqCst), 1);

        assert_eq!(
            client.start().await.unwrap_err().to_string(),
            "MCP connection closed"
        );
        assert_eq!(
            server.start().await.unwrap_err().to_string(),
            "MCP connection closed"
        );
    }

    #[tokio::test]
    async fn close_of_a_never_started_transport_emits_close_once() {
        let transport = InMemoryTransport::new();
        let closes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&closes);
        let _subscription = transport.on_close(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        transport.close().await.unwrap();
        transport.close().await.unwrap();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    /// TS 实测（probe P2）：close 监听器抛错 → `close()` reject，对端**不**被关闭；本侧已
    /// 标记 closed，第二次 `close()` 正常 resolve。
    #[tokio::test]
    async fn a_failing_close_listener_propagates_and_leaves_the_peer_open() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        client.start().await.unwrap();
        server.start().await.unwrap();

        let server_closes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&server_closes);
        let _server_subscription = server.on_close(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let _client_subscription =
            client.on_close(|| Err(McpTransportError::new("close listener boom")));

        let error = client.close().await.unwrap_err();
        assert_eq!(error.to_string(), "close listener boom");
        // TS：emitClose 抛错会跳过 `await this.peer?.close()`。
        assert_eq!(server_closes.load(Ordering::SeqCst), 0);

        // 本侧已经被标记为 closed，第二次 close 是 no-op（不再调用监听器）。
        client.close().await.unwrap();
        // 对端仍然开着：它能 start，但发消息时 peer（client）已关闭。
        server.start().await.unwrap();
        assert_eq!(
            server.send(json!({})).await.unwrap_err().to_string(),
            "In-memory MCP peer is not connected"
        );
    }

    /// TS 的 `peer` 是**强引用**，丢掉一侧句柄后另一侧仍能投递。
    #[tokio::test]
    async fn dropping_the_peer_handle_keeps_sending_to_its_listeners() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        server.on_message(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        server.start().await.unwrap();
        client.start().await.unwrap();
        drop(server);

        client
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            client
                .inner
                .peer()
                .map(|peer| peer.started.load(Ordering::SeqCst)),
            Some(true),
            "client must still reach the peer object through its strong reference"
        );
    }

    /// peer 句柄被丢弃后，`close()` 仍能沿强引用关闭对端并触发监听器。
    #[tokio::test]
    async fn close_reaches_a_peer_whose_handle_was_dropped() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let closes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&closes);
        server.on_close(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        client.start().await.unwrap();
        server.start().await.unwrap();
        drop(server);

        client.close().await.unwrap();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    /// TS 里监听器抛错是一次 `uncaughtException`：同一条消息的后续监听器不再调用，`send`
    /// 不受影响，之后的消息照常投递（probe D：第一次抛错后 `seen = [{"n":2}]`）。
    #[tokio::test]
    async fn a_listener_error_does_not_fail_send_and_later_messages_still_arrive() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        // 第一个监听器只在第一条消息上抛错（TS probe 的 `first` 旗标）。
        let should_fail = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&should_fail);
        let _failing = server.on_message(move |_| {
            if flag.swap(false, Ordering::SeqCst) {
                return Err(McpTransportError::new("listener boom"));
            }
            Ok(())
        });
        let (seen, _subscription) = record_messages(&server);

        server.start().await.unwrap();
        client.start().await.unwrap();
        // 第一条：失败监听器抛错 → 记录器（后面注册）不再被调用；`send` 本身不报错。
        client.send(json!({"id": 1})).await.unwrap();
        assert!(seen.lock().unwrap().is_empty());
        // 第二条：新一轮 emit 照常从第一个监听器开始，记录器收到消息。
        client.send(json!({"id": 2})).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![json!({"id": 2})]);
    }

    /// 评审 finding #6：监听器 **panic** 与 TS 的 `uncaughtException` 一样不影响发送方
    /// ——`send` 返回 `Ok`，同一条消息的后续监听器不再调用，之后的消息照常投递。
    #[tokio::test]
    async fn a_panicking_listener_does_not_fail_send_and_later_messages_still_arrive() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let should_panic = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&should_panic);
        let _failing = server.on_message(move |_| {
            if flag.swap(false, Ordering::SeqCst) {
                panic!("listener boom");
            }
            Ok(())
        });
        let (seen, _recorder) = record_messages(&server);

        server.start().await.unwrap();
        client.start().await.unwrap();
        // TS 里这条 microtask 抛出 → 成为 uncaughtException；`await send` 正常 resolve。
        client.send(json!({"id": 1})).await.unwrap();
        assert!(seen.lock().unwrap().is_empty());
        client.send(json!({"id": 2})).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![json!({"id": 2})]);
    }

    /// TS 实测（probe C/E）：监听器里 `send` 的消息排在本轮 emit 之后投递，绝不会嵌套在当前
    /// 监听器的调用栈里（评审 finding #3 的顺序部分）。TS 的完整顺序是
    /// `["server-in:5","server-out","client-send-returned","client-in:5"]`；这里保证前两项与
    /// 「client-in 在后」，但不保证它一定晚于外层的返回（Rust 没有全局微任务队列）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reply_sent_from_a_listener_is_delivered_after_the_current_emit() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let order = Arc::new(Mutex::new(Vec::<String>::new()));

        let log = Arc::clone(&order);
        let responder = server.clone();
        let _server_subscription = server.on_message(move |message: &Value| {
            let id = message["id"].clone();
            log.lock().unwrap().push(format!("server-in:{id}"));
            // TS 的 `void server.send(...)`：只入队，不在本轮 emit 里重入投递。
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": {}});
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(responder.send(reply))
            })
            .unwrap();
            log.lock().unwrap().push("server-out".to_owned());
            Ok(())
        });
        let log = Arc::clone(&order);
        let _client_subscription = client.on_message(move |message: &Value| {
            log.lock()
                .unwrap()
                .push(format!("client-in:{}", message["id"]));
            Ok(())
        });

        server.start().await.unwrap();
        client.start().await.unwrap();
        client
            .send(json!({"jsonrpc": "2.0", "id": 5, "method": "x"}))
            .await
            .unwrap();
        order
            .lock()
            .unwrap()
            .push("client-send-returned".to_owned());
        wait_until(|| order.lock().unwrap().len() == 4).await;

        let order = order.lock().unwrap().clone();
        assert_eq!(
            &order[..2],
            &["server-in:5".to_owned(), "server-out".to_owned()],
            "the reply must not be delivered nested inside the listener: {order:?}"
        );
        // TS 里 `client-in` 一定排在 `client-send-returned` 之后；Rust 没有全局微任务队列，
        // 两者都排在前两项之后，但先后不定（见模块文档）。
        let tail: Vec<&String> = order[2..].iter().collect();
        assert!(
            tail.contains(&&"client-in:5".to_owned())
                && tail.contains(&&"client-send-returned".to_owned()),
            "the reply is delivered once, after the current emit: {order:?}"
        );
        assert_eq!(
            order.iter().filter(|entry| *entry == "client-in:5").count(),
            1,
            "the reply is delivered exactly once: {order:?}"
        );
        client.close().await.unwrap();
    }

    /// TS 实测（probe E）：自连接时监听器里再 `send`，第二条消息在本轮 emit 之后才投递，
    /// 不会在当前监听器的调用栈里重入 emit（旧实现会得到 `[1, 2]` 的嵌套序列）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_listener_that_sends_to_itself_is_delivered_after_the_current_emit() {
        let transport = InMemoryTransport::new();
        transport.connect_peer(&transport).unwrap();
        let log = Arc::new(Mutex::new(Vec::<i64>::new()));
        let inside = Arc::new(AtomicUsize::new(0));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&log);
        let inside_listener = Arc::clone(&inside);
        let overlap_flag = Arc::clone(&overlaps);
        let handle = transport.clone();
        let _subscription = transport.on_message(move |message: &Value| {
            if inside_listener.fetch_add(1, Ordering::SeqCst) != 0 {
                overlap_flag.fetch_add(1, Ordering::SeqCst);
            }
            let id = message["id"].as_i64().unwrap();
            sink.lock().unwrap().push(id);
            if id == 1 {
                let message = json!({"jsonrpc": "2.0", "id": 2, "method": "x"});
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(handle.send(message))
                })
                .unwrap();
            }
            inside_listener.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        });
        transport.start().await.unwrap();
        transport
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        wait_until(|| log.lock().unwrap().len() == 2).await;
        assert_eq!(*log.lock().unwrap(), vec![1, 2]);
        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "listener re-entered itself"
        );
        transport.close().await.unwrap();
    }

    /// 评审 finding #3（栈部分）：监听器绝不在发送方的调用栈上运行。
    ///
    /// `Runtime::block_on` 把测试的 future 驱动在调用线程上，因此"监听器看到的线程"与
    /// "发送方看到的线程"不同就证明监听器在自己的投递 task 里运行（旧实现在发送方栈上同步
    /// 投递，会得到同一个线程）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_listener_never_runs_on_the_senders_stack() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let listener_thread = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&listener_thread);
        let _subscription = server.on_message(move |_| {
            *recorded.lock().unwrap() = Some(std::thread::current().id());
            Ok(())
        });
        server.start().await.unwrap();
        client.start().await.unwrap();

        let sender_thread = std::thread::current().id();
        client
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        let seen = listener_thread.lock().unwrap().expect("listener ran");
        assert_ne!(
            seen, sender_thread,
            "the message listener ran on the sender's stack"
        );
        client.close().await.unwrap();
    }

    /// 监听器把消息排进队列之后又关闭传输：TS 里排队的 microtask 看到 `closed` 就丢弃。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_messages_are_dropped_when_a_listener_closes_the_transport() {
        let transport = InMemoryTransport::new();
        transport.connect_peer(&transport).unwrap();
        let log = Arc::new(Mutex::new(Vec::<i64>::new()));
        let sink = Arc::clone(&log);
        let handle = transport.clone();
        let _subscription = transport.on_message(move |message: &Value| {
            let id = message["id"].as_i64().unwrap();
            sink.lock().unwrap().push(id);
            if id == 1 {
                let message = json!({"jsonrpc": "2.0", "id": 2, "method": "x"});
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(handle.send(message))
                })
                .unwrap();
                handle.inner.closed.store(true, Ordering::SeqCst);
            }
            Ok(())
        });
        transport.start().await.unwrap();
        transport
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        assert_eq!(*log.lock().unwrap(), vec![1]);
        // 模块文档写明的已知差异：监听器里重入 `send` 入队的消息不保证在外层 `send` 返回前
        // 被处理（Rust 没有全局微任务检查点），只保证在本轮 emit 之后处理。`closed` 已置位，
        // 因此它最终会被丢弃而不是投递；这里等排空 task 处理完再断言。
        wait_until(|| transport.inner.lock_delivery().queue.is_empty()).await;
        assert!(transport.inner.lock_delivery().queue.is_empty());
        assert_eq!(*log.lock().unwrap(), vec![1]);
    }

    /// 双向同时投递（多个线程各自 `send` 到对方）不会死锁、不丢消息。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bidirectional_sends_from_two_threads_do_not_deadlock() {
        for _ in 0..50 {
            let pair = create_in_memory_transport_pair();
            let (client, server) = pair.into_parts();
            let received = Arc::new(AtomicUsize::new(0));
            for transport in [&client, &server] {
                let counter = Arc::clone(&received);
                let _subscription = transport.on_message(move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                });
            }
            client.start().await.unwrap();
            server.start().await.unwrap();

            let (client_send, server_send) = tokio::join!(
                tokio::spawn({
                    let client = client.clone();
                    async move {
                        for n in 0..20 {
                            client.send(json!({"id": n})).await.unwrap();
                        }
                    }
                }),
                tokio::spawn({
                    let server = server.clone();
                    async move {
                        for n in 0..20 {
                            server.send(json!({"id": n})).await.unwrap();
                        }
                    }
                })
            );
            client_send.unwrap();
            server_send.unwrap();
            assert_eq!(received.load(Ordering::SeqCst), 40);
            client.close().await.unwrap();
        }
    }

    /// TS 是单线程事件循环：同一条传输的 message 监听器绝不会并发执行。Rust 里多个线程可以
    /// 同时 `send` 到同一个 peer，这里断言监听器之间仍然互斥（否则 `McpClient` 之类在监听器
    /// 里更新状态的下游会出现 TS 里不存在的交错）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_senders_never_run_listeners_concurrently() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let inside = Arc::new(AtomicUsize::new(0));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let count = Arc::new(AtomicUsize::new(0));
        let inside_listener = Arc::clone(&inside);
        let overlap_flag = Arc::clone(&overlaps);
        let counter = Arc::clone(&count);
        let _subscription = server.on_message(move |_| {
            if inside_listener.fetch_add(1, Ordering::SeqCst) != 0 {
                overlap_flag.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(2));
            inside_listener.fetch_sub(1, Ordering::SeqCst);
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        server.start().await.unwrap();
        client.start().await.unwrap();

        let mut senders = Vec::new();
        for id in 0..4 {
            let sender = client.clone();
            senders.push(tokio::spawn(async move {
                for n in 0..5 {
                    sender.send(json!({"id": id * 100 + n})).await.unwrap();
                }
            }));
        }
        for sender in senders {
            sender.await.unwrap();
        }
        assert_eq!(count.load(Ordering::SeqCst), 20);
        assert_eq!(overlaps.load(Ordering::SeqCst), 0);
        client.close().await.unwrap();
    }

    /// §30.3 提到的现实场景：message 监听器里调用 `close()`。投递不持有 close 需要的任何锁，
    /// 因此这不会死锁。
    ///
    /// TS 实测（probe P1）：`close()` 在监听器里同步置 `closed` 并 emit close，但
    /// `emitMessage` 的 `Set` 迭代**不会**因此中断，同一轮 emit 里后面的监听器仍会收到这条
    /// 消息（下一轮才因 `deliver` 的 `closed` 检查而丢弃）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_message_listener_can_close_the_transport() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let closer = server.clone();
        // 先注册关闭者：它关掉 server 之后，同一轮 emit 的后续（记录器）仍会被调用。
        let _close_from_listener = server.on_message(move |_| {
            // 监听器是同步 `Fn`；在 tokio worker 上用 `block_in_place` 驱动一次 `close()`。
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(closer.close())
            })
            .unwrap();
            Ok(())
        });
        let (seen, _subscription) = record_messages(&server);
        server.start().await.unwrap();
        client.start().await.unwrap();
        client.send(json!({"id": 1})).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![json!({"id": 1})]);
        // close() 递归关闭了 peer，所以发送侧也已是 closed。
        assert_eq!(
            client.send(json!({})).await.unwrap_err().to_string(),
            "MCP connection closed"
        );
    }

    #[tokio::test]
    async fn a_one_way_link_delivers_and_close_propagates_along_it() {
        let client = InMemoryTransport::new();
        let server = InMemoryTransport::new();
        client.connect_peer(&server).unwrap();

        let (seen, _subscription) = record_messages(&server);
        client.start().await.unwrap();
        server.start().await.unwrap();

        client
            .send(json!({"jsonrpc": "2.0", "id": 7, "method": "x"}))
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);

        // 反方向没有 peer。
        assert_eq!(
            server.send(json!({})).await.unwrap_err().to_string(),
            "In-memory MCP peer is not connected"
        );

        let server_closes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&server_closes);
        let _server_subscription = server.on_close(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        client.close().await.unwrap();
        assert_eq!(server_closes.load(Ordering::SeqCst), 1);
        assert_eq!(
            server.start().await.unwrap_err().to_string(),
            "MCP connection closed"
        );
    }

    /// TS 实测：`pair.server.close()` 会递归关闭 client，所以之后从 client 发送得到的是
    /// **默认**消息（本侧已关闭），而不是 peer 消息。只有单向连接时才会走 peer 分支
    /// （见 `a_one_way_link_delivers_and_close_propagates_along_it`）。
    #[tokio::test]
    async fn closing_the_peer_also_closes_the_sender() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        client.start().await.unwrap();
        server.start().await.unwrap();
        server.close().await.unwrap();
        let error = client.send(json!({})).await.unwrap_err();
        assert_eq!(error.name(), "McpConnectionClosedError");
        assert_eq!(error.to_string(), "MCP connection closed");
    }

    /// TS 实测（probe P7）：`a.connectPeer(a)` 是合法的（只检查"已有 peer"），自连接能收到
    /// 自己发的消息。
    #[tokio::test]
    async fn a_transport_can_connect_to_itself() {
        let transport = InMemoryTransport::new();
        transport.connect_peer(&transport).unwrap();
        let (seen, _subscription) = record_messages(&transport);
        transport.start().await.unwrap();
        transport
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        // 自连接时 close 的链式循环也必须终止（每侧只有一个节点，第二次到达即 return）。
        transport.close().await.unwrap();
    }

    /// TS 里 peer 是对象引用：Rust 的 `Clone` 共享同一份状态（同一个 transport 的另一个句柄）。
    #[tokio::test]
    async fn clone_shares_the_same_transport_state() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let handle = client.clone();
        let (seen, _subscription) = record_messages(&server);
        client.start().await.unwrap();
        server.start().await.unwrap();
        // 通过克隆的句柄发送，原句柄观察到的关闭状态一致。
        handle
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "x"}))
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        client.close().await.unwrap();
        assert_eq!(
            handle.start().await.unwrap_err().to_string(),
            "MCP connection closed"
        );
    }

    /// TS 由 GC 回收互相引用的对象；Rust 的强引用会形成 `Arc` 环。最后一个句柄消失时切断
    /// 本侧到 peer 的边，于是未 `close()` 的一对也能被释放（评审 finding #4）。
    #[tokio::test]
    async fn dropping_an_unclosed_pair_releases_both_transports() {
        let pair = create_in_memory_transport_pair();
        let weak_client = Arc::downgrade(&pair.client.inner);
        let weak_server = Arc::downgrade(&pair.server.inner);
        let drops = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&drops);
        // 监听器闭包持有这个计数器：它被释放说明整个 transport 状态被回收。
        pair.server.on_message(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        drop(pair);
        assert!(
            weak_client.upgrade().is_none(),
            "client state leaked after the pair became unreachable"
        );
        assert!(
            weak_server.upgrade().is_none(),
            "server state (and its listener closures) leaked after the pair became unreachable"
        );
    }

    /// 关闭后同样释放链接（与上一条互补：显式 `close()` 路径）。
    #[tokio::test]
    async fn closing_the_pair_releases_the_peer_cycle() {
        let pair = create_in_memory_transport_pair();
        let weak_client = Arc::downgrade(&pair.client.inner);
        let weak_server = Arc::downgrade(&pair.server.inner);
        pair.client.close().await.unwrap();
        drop(pair);
        assert!(weak_client.upgrade().is_none(), "client state leaked");
        assert!(weak_server.upgrade().is_none(), "server state leaked");
    }

    #[tokio::test]
    async fn emit_error_dispatches_to_error_listeners() {
        let transport = InMemoryTransport::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let _subscription = transport.on_error(move |error| {
            sink.lock()
                .unwrap()
                .push((error.name().to_owned(), error.to_string()));
            Ok(())
        });

        transport
            .emit_error(&McpTransportError::new("transport boom"))
            .unwrap();
        transport
            .emit_error(&McpTransportError::network("socket hang up"))
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("Error".to_owned(), "transport boom".to_owned()),
                ("TypeError".to_owned(), "socket hang up".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn emit_error_propagates_listener_errors() {
        // §30.4：error 监听器的异常不会被吞掉，而是冒给 emit 的调用方并中断后续监听器。
        let transport = InMemoryTransport::new();
        let _first = transport.on_error(|_| Err(McpTransportError::new("listener boom")));
        let reached = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reached);
        let _second = transport.on_error(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let error = transport
            .emit_error(&McpTransportError::new("transport boom"))
            .unwrap_err();
        assert_eq!(error.to_string(), "listener boom");
        assert_eq!(reached.load(Ordering::SeqCst), 0);
    }

    /// 通过 trait object 使用（`McpClient` 就是这么持有传输的），并确认 in-memory 传输忽略
    /// `McpTransportSendOptions`（TS 的 `send(message)` 不接收 options）。
    #[tokio::test]
    async fn transport_object_sends_with_aborted_options() {
        let pair = create_in_memory_transport_pair();
        let (client, server) = pair.into_parts();
        let client: Arc<dyn McpTransport> = Arc::new(client);
        let server: Arc<dyn McpTransport> = Arc::new(server);

        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let _subscription = server.on_message(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));

        server.start().await.unwrap();
        client.start().await.unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        client
            .send(
                json!({"jsonrpc": "2.0", "id": 1, "method": "x"}),
                McpTransportSendOptions::with_signal(Some(token)),
            )
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);

        client.close().await.unwrap();
        // 关闭后通过 trait 发送同样报错。
        assert_eq!(
            client
                .send(json!({}), McpTransportSendOptions::none())
                .await
                .unwrap_err()
                .to_string(),
            "MCP connection closed"
        );
    }
}
