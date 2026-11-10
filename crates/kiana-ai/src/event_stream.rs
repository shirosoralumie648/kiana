//! `AssistantMessageEventStream`：可推送、可等待结果的事件流。
//!
//! TS 实现（`utils/event-stream.ts`）是 `EventStream<T, R>`：内部 `FifoQueue` 事件队列 +
//! `FifoQueue` 等待者队列，`push()` / `end()` 同步写入，`result()` 返回一个在流结束时 resolve
//! 的 Promise，`[Symbol.asyncIterator]` 逐个消费。
//!
//! 使用方：各 provider 适配器（`api/*`）用它把后台请求（HTTP/SSE/WebSocket）转成统一事件流；
//! 上层（agent / session / extension 运行时）通过 `recv()` 或 `take_receiver()` 消费，
//! 只关心最终消息的调用方直接 `await result()`。
//!
//! Rust 侧逐条复刻这套队列语义（`ARCHITECTURE.md` §4.2/§4.9 冻结的表面：`push`/`end`/
//! `result`/`recv`）：
//!
//! - [`AssistantMessageEventStream::new`] 创建（对应 `new AssistantMessageEventStream()`）。
//! - [`push`](AssistantMessageEventStream::push) 推入事件（同步，不阻塞）：若有等待中的消费者，
//!   事件交给**最早注册**的那个；否则入队（与 TS 的 `waiting.dequeue()` / `queue.enqueue()` 相同）。
//! - [`end`](AssistantMessageEventStream::end) 结束并设定最终消息；[`end_without_result`](AssistantMessageEventStream::end_without_result)
//!   (对应 TS 的无参 `end()`) 结束但不 resolve `result()`。
//! - [`result`](AssistantMessageEventStream::result) 等待最终消息（对应 `result()` Promise；
//!   与 TS 一样**可重复调用**，每次都得到同一结果）。无参 `end()`（[`end_without_result`](AssistantMessageEventStream::end_without_result)）
//!   之后没有结果可提交时，它与 TS 的 `finalResultPromise` 一样**永远 pending**（既不 resolve
//!   也不 reject）；冻结签名里的 `Result` 仍然保留，`StreamEndedWithoutResult` 只在结果通道的
//!   发送端被丢弃时出现——持有句柄时不会发生。
//! - [`recv`](AssistantMessageEventStream::recv) 消费事件（对应异步迭代器的一次 `next()`）。
//!   每个 `recv()` future 都 `Send`，多个并发 `recv()` 各注册一个等待者、按注册序各得一个事件
//!   （TS 的 “delivers events to waiting consumers in registration order”）。
//!
//! 事件队列与 TS 的 `FifoQueue` 一样**无上限**（受内存约束）：终态事件因此不会因为缓冲满而丢失。
//! [`EVENT_CHANNEL_CAPACITY`](crate::event_stream::EVENT_CHANNEL_CAPACITY) 是骨架期 mpsc 实现的容量常量，保留给按名字引用它的代码。
//!
//! 计时语义：TS 在构造时记录 `Date.now()` 与 `performance.now()`，在 `done`/`error` 事件上
//! 写 `durationMs = now_monotonic - startedAtMonotonic`（`AssistantMessageEventStream` 子类
//! 的 `#time()`）。Rust 用 [`std::time::Instant`] 做同样的事：`done`/`error`/`end(message)` 三条
//! 路径都会调用 [`AssistantMessageEventStream::time_message`]，规则逐字对照 TS（流已完成 /
//! 已有 `durationMs` / `timestamp` 早于流的创建时刻 → 不写）。
//!
//! 与 TS 的**有意差异**（签名由 §4.9 冻结）：
//!
//! - TS 的 `end()` 允许不带结果；冻结的 `end(message)` 必须给消息，另有
//!   [`end_without_result`](AssistantMessageEventStream::end_without_result) 表达无参形式。
//! - TS 的子类 `#time()` 就地修改调用方传入的对象；Rust 按值接收，写回只发生在流内部持有的
//!   副本上（`result()` 与事件里可见，调用方的原值不变）。
//!
//! # 并发模型（实现层面）
//!
//! - `queue` 与 `waiting` 放在**同一个** `Mutex` 里：入队与注册等待者不会交错，避免“事件
//!   已入队、但消费者尚未注册”的丢失窗口。`recv()` 等待期间不持锁，因此 future 是 `Send`。
//! - `done` 是 `AtomicBool`；最终结果放在独立的 `Mutex<Option<AssistantMessage>>`，并用
//!   `watch` 通道广播唤醒。多个 `result()` 等待者共享同一份结果，重复调用直接命中槽位。
//! - 锁顺序：`push` 在持有 `state` 锁时调用 `resolve_result`（再取 `result` 锁）；没有
//!   反向路径在持有 `result` 锁时去取 `state` 锁，因此不存在环形等待。锁中毒一律取回
//!   内部值继续，流不会因别的线程 panic 而失效。
//! - 所有锁内只做内存操作，不 `await`、不调用外部代码。
//!
//! # 取消与背压
//!
//! - `recv()` 的 future 在等待期间被 drop 时，`WaiterGuard` 会把它的等待者条目从
//!   `waiting` 移除；若 `push` 已经把事件写进它的槽位（即使 `send` 已成功）但事件还未被
//!   取走，guard 会把事件放回队首。因此取消 `recv()` 不会丢事件（详见 `recv` 的文档）。
//! - `take_receiver()` 的 mpsc 通道容量是 `EVENT_CHANNEL_CAPACITY`；通道满时转发任务在
//!   `send().await` 上等待（背压），不会丢事件。内部事件队列仍然无上限，因此生产者不会
//!   被慢消费者阻塞。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use kiana_telemetry::TelemetrySpan;
use tokio::sync::oneshot;

use crate::types::{AssistantMessage, AssistantMessageEvent};

/// 骨架期 mpsc 实现的事件缓冲上限（`ARCHITECTURE.md` §4.2）。
///
/// 现在的实现与 TS 的 `FifoQueue` 一样无上限，因此本常量不再限制事件队列；保留它是为了让按名字
/// 引用它的代码继续编译（§4.9 记录了该常量）。
///
/// 它目前唯一的实际用途是 [`AssistantMessageEventStream::take_receiver`] 里 mpsc 通道的容量：
/// 通道满时转发任务在 `send().await` 上等待（背压），不会丢事件，也不会阻塞 `push()`。
pub const EVENT_CHANNEL_CAPACITY: usize = 4096;

/// `push` 失败的原因。
///
/// TS 的 `push()` 没有失败路径（`done` 之后静默丢弃、队列无上限）。冻结的返回类型
/// `Result<(), StreamPushError>` 仍然保留，但当前实现**不会**产生这些错误；变体保留给按名字
/// 匹配的调用方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StreamPushError {
    /// 接收端已丢弃（历史 mpsc 实现；现在事件总是入队）。
    #[error("event stream receiver was dropped")]
    Closed,
    /// 消费者落后超过 [`EVENT_CHANNEL_CAPACITY`]（历史 mpsc 实现；队列现在无上限）。
    #[error("event stream buffer is full")]
    Full,
    /// 已经调用过 `end()`（TS 里 `end()` 之后 `push()` 被静默忽略，本实现同样返回 `Ok(())`）。
    #[error("event stream already ended")]
    Ended,
}

/// 流结束时没有结果（实现 bug 或结果通道被丢弃）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("assistant message event stream ended without a result")]
pub struct StreamEndedWithoutResult;

/// 一个等待中的消费者（`waiting.enqueue(resolve)`）。
///
/// 事件先写进每个等待者自己的 `slot`，再用 `tx` 唤醒它。不用 `oneshot<Event>` 直接投递：
/// `send` 成功后接收端仍可能在 poll 之前被 drop（`select!` 输掉），此时 oneshot 会把事件
/// 一起丢掉且无法取回。`slot` 让 [`WaiterGuard`] 在取消时把未消费的事件放回队列。
struct Waiter {
    id: u64,
    slot: Arc<Mutex<Option<AssistantMessageEvent>>>,
    tx: oneshot::Sender<()>,
}

/// `recv()` 注册等待者后的清理守卫。
///
/// `recv()` future 在等待期间被 drop（`select!` 输掉、超时、任务取消）时：
/// - 先从 `waiting` 里移除自己的等待者条目（避免后续 `push` 选中死等待者）；
/// - 如果 `push` 已经把事件写进 `slot` 但还没来得及被消费，把事件放回队首——
///   否则「`send` 成功 + 接收端未 poll 就被 drop」会永久丢掉该事件。
struct WaiterGuard {
    shared: Arc<Shared>,
    id: u64,
    slot: Arc<Mutex<Option<AssistantMessageEvent>>>,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        // 加锁顺序与 `push` 一致（state → slot），避免与投递路径形成环。
        let mut state = lock(&self.shared.state);
        if let Some(index) = state.waiting.iter().position(|waiter| waiter.id == self.id) {
            state.waiting.remove(index);
        }
        if let Some(event) = lock(&self.slot).take() {
            // 事件比队列里任何条目都早被 push，放回队首才能保持投递顺序；随后立即尝试
            // 交给下一个仍然存活的等待者，否则它会一直睡到下一次 `push`（即使队列里
            // 已经有事件），消费者会看到乱序。
            state.queue.push_front(event);
            deliver_queued_events(&mut state);
        }
    }
}

/// 把队列里的事件按 FIFO 逐个交给等待者（push 与取消回收共用）。
///
/// 调用方必须持有 `state` 锁；函数内部不会 `await`。
fn deliver_queued_events(state: &mut StreamState) {
    while !state.queue.is_empty() {
        let Some(waiter) = state.waiting.pop_front() else {
            break;
        };
        let event = state
            .queue
            .pop_front()
            .expect("queue is non-empty (checked above)");
        *lock(&waiter.slot) = Some(event);
        if waiter.tx.send(()).is_err() {
            // 接收端已 drop。`WaiterGuard` 的 Drop 也拿这把 state 锁，所以此刻它无法
            // 抢先取走槽位；事件一定还在，取回来放回队首交给下一个等待者。
            if let Some(event) = lock(&waiter.slot).take() {
                state.queue.push_front(event);
            }
        }
    }
}

/// 事件流句柄（可克隆；每个句柄都可以 `recv()`，与 TS 的多个迭代器一致）。
#[derive(Clone)]
pub struct AssistantMessageEventStream {
    shared: Arc<Shared>,
}

// 所有 clone 出的句柄共享这一份状态；Arc 的最后一个强引用释放时状态才销毁（drop 句柄
// 不会自动调用 `end()`，生产者应显式结束流）。
struct Shared {
    /// TS `EventStream.queue` + `waiting`（两者总在同一个临界区里操作，避免“入队与注册等待者”
    /// 之间的竞态）。
    state: Mutex<StreamState>,
    /// TS 的 `EventStream.done`：`push` 在 `done` 之后被静默忽略，且 `#time()` 以它判断流是否已结束。
    done: AtomicBool,
    /// TS `finalResultPromise` + `resolveFinalResult`：保存已提交的最终消息，`result()` 可重复读取。
    result: Mutex<Option<AssistantMessage>>,
    /// `result()` 的唤醒通道（`watch` 让任意多个等待者同时醒来并读到同一份结果）。
    result_tx: tokio::sync::watch::Sender<Option<AssistantMessage>>,
    /// [`AssistantMessageEventStream::take_receiver`] 是否已经交出过接收端（至多一次）。
    ///
    /// 这里**不**保存发送端：转发任务自己持有唯一的 `Sender`，`recv()` 在流结束后返回
    /// `None`，任务随之结束并丢弃它，接收端因此看到通道关闭（与 TS 迭代器结束后
    /// `for await` 退出等价）。
    forward_taken: AtomicBool,
    started_at: Instant,
    started_at_unix_ms: AtomicI64,
    parent_span: Mutex<Option<Arc<dyn TelemetrySpan>>>,
}

struct StreamState {
    /// TS `FifoQueue<T> queue`。
    queue: VecDeque<AssistantMessageEvent>,
    /// TS `FifoQueue<(r: IteratorResult<T>) => void> waiting`。
    waiting: VecDeque<Waiter>,
}

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AssistantMessageEventStream {
    /// `new AssistantMessageEventStream()`。
    pub fn new() -> Self {
        let (result_tx, _result_rx) = tokio::sync::watch::channel(None);
        AssistantMessageEventStream {
            shared: Arc::new(Shared {
                state: Mutex::new(StreamState {
                    queue: VecDeque::new(),
                    waiting: VecDeque::new(),
                }),
                done: AtomicBool::new(false),
                result: Mutex::new(None),
                result_tx,
                forward_taken: AtomicBool::new(false),
                started_at: Instant::now(),
                started_at_unix_ms: AtomicI64::new(now_unix_ms()),
                parent_span: Mutex::new(None),
            }),
        }
    }

    /// 绑定父 span（TS 的 `streamOptions.telemetry`）。
    ///
    /// 写入共享状态并返回 `self` 的克隆（builder 风格），因此对任一 clone 调用会改变所有
    /// 句柄看到的 span；只有最后一个写入者生效，没有合并逻辑。
    pub fn with_parent_span(&self, span: Option<Arc<dyn TelemetrySpan>>) -> Self {
        *lock(&self.shared.parent_span) = span;
        self.clone()
    }

    /// 请求开始时间（Unix 毫秒），对应 TS 的 `#startedAt`。
    pub fn started_at_unix_ms(&self) -> i64 {
        self.shared.started_at_unix_ms.load(Ordering::Relaxed)
    }

    /// 单调时钟的请求开始点，用于计算 `durationMs`。
    pub fn started_at(&self) -> Instant {
        self.shared.started_at
    }

    /// `push(event)`。同步；不阻塞。
    ///
    /// 与 TS 一一对应：`done` 为真时**静默忽略**（返回 `Ok(())`）；终态事件（`done`/`error`）
    /// 先按 TS `#time()` 的规则写入 `durationMs`，再置 `done`、提交 `result()`（顺序：
    /// `#time` → `done = true` → resolve result → 投递事件），事件本身**仍然投递**给最早注册的
    /// 等待者或入队。
    ///
    /// `done` 标记与事件投递在**同一个** `state` 临界区内完成：TS 是单线程，`done = true` 与
    /// `queue.enqueue`/`waiting.dequeue` 之间不存在可观察的中间态；Rust 若先置 `done` 再取锁，
    /// 并发的 `recv()` 可能观察到「`done` 为真但队列为空」而直接返回 `None`，把终态事件丢掉。
    ///
    /// 注意：开头的 `done` 快路径在锁外读取。若调用方让 `push` 与 `end` 跨线程并发，晚到的
    /// `push` 仍可能在 `end` 之后入队（TS 的“end 后丢弃”语义不被保证）。内置适配器通常在
    /// 单个后台任务里顺序调用（如 `api/bedrock_converse_stream` 的 `spawn_forward`）；需要
    /// 多线程写入时，调用方必须自行串行化 `push`/`end`。
    pub fn push(&self, event: AssistantMessageEvent) -> Result<(), StreamPushError> {
        if self.shared.done.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut event = event;
        let final_message = match &mut event {
            AssistantMessageEvent::Done { message, .. } => {
                self.time_message(message);
                Some((**message).clone())
            }
            AssistantMessageEvent::Error { error, .. } => {
                self.time_message(error);
                Some((**error).clone())
            }
            _ => None,
        };
        // 投递：最早注册的等待者，否则入队（TS `waiting.dequeue()` / `queue.enqueue()`）。
        //
        // 队列是**唯一**的存储点：`take_receiver` 的转发任务也是一个普通消费者（`recv()`），
        // 因此这里不直接写 mpsc——否则“先 try_send、失败即丢”会在通道满/接收端已丢弃时
        // 丢掉事件（终态事件也必须总能被消费到）。
        //
        // 终态事件在这里才置 `done`：保持与投递同一临界区，`recv()` 不会看到
        // 「`done` 为真 + 队列为空」的中间态（见方法文档）。
        let mut state = lock(&self.shared.state);
        if let Some(message) = final_message {
            // 与 TS 相同：`done` 标记在 `#time()` 之后（`#time` 自己检查 `this.done`）。
            self.shared.done.store(true, Ordering::SeqCst);
            self.resolve_result(message);
        }
        // 队列是唯一存储点：先入队，再按 FIFO 交给等待者。即使队列里已经有条目（例如
        // 被取消的等待者刚刚把事件放回队首），也会先投递更早的那个，保证注册序/FIFO
        // 不被取消路径打乱。
        state.queue.push_back(event);
        deliver_queued_events(&mut state);
        Ok(())
    }

    /// `end(result)`：提交最终消息并结束流（可重复调用；已提交的结果不会被覆盖）。
    pub fn end(&self, message: AssistantMessage) {
        let mut message = message;
        // TS `end(result)` 先 `#time(result)`（仅在流尚未完成时计时）。
        self.time_message(&mut message);
        self.shared.done.store(true, Ordering::SeqCst);
        self.resolve_result(message);
        self.wake_waiters();
    }

    /// TS 的无参 `end()`：结束流、唤醒所有等待者，但**不** resolve `result()`
    /// （与 TS 的 `finalResultPromise` 一样，没有结果的 `result()` 永远 pending；
    /// `result()` 的 future 始终借着一个句柄，因此结果通道的发送端不会在等待期间关闭）。
    pub fn end_without_result(&self) {
        self.shared.done.store(true, Ordering::SeqCst);
        self.wake_waiters();
    }

    /// `result()`：等待最终消息。与 TS 的 `finalResultPromise` 一样可重复调用：
    /// 已提交的结果对后续每次调用都立即可见；从未提交结果时**永远 pending**
    /// （`Result` 是冻结签名的一部分，`Err` 分支只在等待期间结果通道被关闭时可达）。
    pub async fn result(&self) -> Result<AssistantMessage, StreamEndedWithoutResult> {
        {
            let current = lock(&self.shared.result);
            if let Some(message) = current.as_ref() {
                return Ok(message.clone());
            }
        }
        // 订阅后再检查一次：结果可能恰好在“读槽位”与“订阅”之间提交。
        let mut rx = self.shared.result_tx.subscribe();
        loop {
            if let Some(message) = rx.borrow_and_update().clone() {
                return Ok(message);
            }
            // `changed` 只在“提交了结果”或“最后一个句柄丢弃（发送端关闭）”时返回。
            if rx.changed().await.is_err() {
                break;
            }
        }
        // 发送端关闭：若关闭前提交过结果仍返回它，否则报“无结果”。
        let current = lock(&self.shared.result);
        current.clone().ok_or(StreamEndedWithoutResult)
    }

    /// 接收下一个事件（`for await` 的一次迭代）。
    ///
    /// 语义与 TS 的异步迭代器一致：先排空队列；队列空且流未结束则注册一个等待者等待下一次
    /// `push`；流已结束且队列空则返回 `None`。多个并发 `recv()` 按注册序各得一个事件。
    ///
    /// # 取消是安全的
    ///
    /// future 在等待期间被 drop（例如 `select!` 输掉、超时取消）时，[`WaiterGuard`] 会把
    /// 自己的等待者条目从 `waiting` 里移除；若 `push` 已经把事件写进槽位但还没被消费，
    /// guard 会把事件放回队首。因此即使 `send` 已经成功、接收端在 poll 之前被 drop，
    /// 事件也不会丢失。需要独立消费循环时仍可用
    /// [`AssistantMessageEventStream::take_receiver`]。
    pub async fn recv(&self) -> Option<AssistantMessageEvent> {
        let (guard, wake) = {
            let mut state = lock(&self.shared.state);
            if let Some(event) = state.queue.pop_front() {
                return Some(event);
            }
            if self.shared.done.load(Ordering::SeqCst) {
                return None;
            }
            let slot = Arc::new(Mutex::new(None));
            let (tx, rx) = oneshot::channel();
            let id = next_waiter_id();
            state.waiting.push_back(Waiter {
                id,
                slot: Arc::clone(&slot),
                tx,
            });
            (
                WaiterGuard {
                    shared: Arc::clone(&self.shared),
                    id,
                    slot,
                },
                rx,
            )
        };
        // 等待期间不持锁（future 因此是 `Send`）。
        wake.await.ok();
        // `end()` 唤醒时槽位为空 → 迭代结束；`push` 唤醒时槽位里有事件。取走事件后
        // guard 的 Drop 只会做「从 waiting 移除条目」的空操作。
        let event = lock(&guard.slot).take();
        drop(guard);
        event
    }

    /// 取出接收端，供需要 `Stream` 实现的调用方使用（骨架里就有这个入口）。
    ///
    /// TS 的 `EventStream` 没有对应 API：Rust 侧把它实现成“把这条流的消费权交给一个
    /// `mpsc::Receiver`”。调用后由一个转发任务按 TS 迭代器的语义（先排空队列、再按注册序
    /// 等待新事件）把事件送进 mpsc；因此事件不丢不重，`end()` 让迭代结束、通道随之关闭
    /// （转发任务自己持有唯一的 `Sender`，任务结束时丢弃它）。
    ///
    /// 仅在 Tokio 运行时内可用（否则返回 `None`）；第二次调用返回 `None`。
    ///
    /// 接收端被提前 drop 时，转发任务不会立即察觉，事件去向取决于它停在哪种等待上：
    /// - 停在 `recv()` 等新事件时，下一次 `push` 会把事件直接交给它的 oneshot（不进入
    ///   `state.queue`）；转发任务随后在 `send()` 上发现接收端已 drop 并退出，**该事件被
    ///   丢弃**；
    /// - 阻塞在 `send()` 的背压上时（通道满、接收端尚未被观察到 drop），新事件继续进入
    ///   内部队列（队列无上限，生产者不会被阻塞），直到那次 `send()` 失败、任务退出；此后
    ///   入队的事件再无人取走。
    ///
    /// 也就是说“没有消费者”不等于“事件都安全地堆在队列里”：已被 `recv()` 取走的那一条会
    /// 随 `send()` 失败一起消失。
    pub fn take_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<AssistantMessageEvent>> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        if self.shared.forward_taken.swap(true, Ordering::SeqCst) {
            return None;
        }
        let (tx, rx) = tokio::sync::mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let stream = self.clone();
        handle.spawn(async move {
            while let Some(event) = stream.recv().await {
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });
        Some(rx)
    }

    /// 取出父 span。
    pub fn parent_span(&self) -> Option<Arc<dyn TelemetrySpan>> {
        lock(&self.shared.parent_span).clone()
    }

    /// 把「请求开始时间」重设为给定毫秒值（TS 在测试里 mock `Date.now`）。
    ///
    /// 会影响 [`AssistantMessageEventStream::started_at_unix_ms`] 的返回值；在计时上则只影响
    /// [`AssistantMessageEventStream::time_message`] 的 `timestamp` 门槛判断——`durationMs` 仍由
    /// 单调钟（`started_at`）计算，不受本值影响。
    pub fn set_started_at_unix_ms(&self, ms: i64) {
        self.shared.started_at_unix_ms.store(ms, Ordering::Relaxed);
    }

    /// `durationMs`：当前单调时间与开始点之差，按整数毫秒（与 TS 的 `Math.round` 一致）。
    pub fn elapsed_ms(&self) -> f64 {
        let micros = self.shared.started_at.elapsed().as_micros() as f64;
        (micros / 1000.0).round()
    }

    /// TS `resolveFinalResult`：只提交第一次的结果（TS 的 Promise resolve 也是幂等的）。
    fn resolve_result(&self, message: AssistantMessage) {
        let mut slot = lock(&self.shared.result);
        if slot.is_none() {
            *slot = Some(message.clone());
            let _ = self.shared.result_tx.send(Some(message));
        }
    }

    /// TS `end()` 的 “notify all waiting consumers”：每个等待者收到 `{done: true}`。
    fn wake_waiters(&self) {
        let waiters: Vec<Waiter> = {
            let mut state = lock(&self.shared.state);
            state.waiting.drain(..).collect()
        };
        for waiter in waiters {
            let _ = waiter.tx.send(());
        }
    }

    /// TS `#time(message)`：给最终消息写 `durationMs`，写入后返回 `true`。
    ///
    /// 规则（逐字对照 `utils/event-stream.ts`）：流已完成、消息已有 `durationMs`、或消息的
    /// `timestamp` 早于流的创建时刻时**不写**（后一条让“转发别处已开始的响应”（如稍后取回的
    /// deferred 结果）保持不计时）。`utils::event_stream::time_assistant_message` 直接复用它。
    pub fn time_message(&self, message: &mut AssistantMessage) -> bool {
        if self.shared.done.load(Ordering::SeqCst)
            || message.duration_ms.is_some()
            || message.timestamp < self.started_at_unix_ms()
        {
            return false;
        }
        // `Math.max(0, Math.round(...))`：`Instant` 单调，理论上不会为负，仍按 TS 夹到 0。
        message.duration_ms = Some(self.elapsed_ms().max(0.0));
        true
    }
}

// 锁中毒后取回内部值：流状态在其它线程 panic 之后仍然一致（全仓库的全局表/流都采用
// 这个约定），避免一次 panic 让所有消费者永久失败。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// 等待者的唯一 id：`WaiterGuard` 靠它精确移除自己被取消的 sender（同一流可以同时有多个
// `recv()`，不能用位置/相等性代替）。
fn next_waiter_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// 系统时钟早于 Unix epoch（理论情况）时返回 0，避免负的 `startedAt` 让后续时间戳比较
// 出现反直觉结果；正常时钟下就是 Unix 毫秒。
fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// TS `createAssistantMessageEventStream()`：扩展与 provider 使用的工厂函数。
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}
