//! agent-loop.ts。
//!
//! TS 源：`packages/agent/src/agent-loop.ts`。
//!
//! 这里是整个 agent 包的核心：`runLoop` 的状态机、`streamAssistantResponse` 的流式上下
//! 文维护、`declareToolChanges` 的工具装载差量，以及顺序/并行工具批次的执行。
//!
//! ## 与 TS 的对应关系
//!
//! - [`agent_loop`] / [`agent_loop_continue`]：`agentLoop` / `agentLoopContinue`。它们同步
//!   返回一个可消费、可等待结果的 [`AgentEventStream`]；真正的循环在一个 tokio 任务里跑。
//!   TS 里 `runAgentLoop(...).then(msgs => stream.end(msgs))` 没有 `catch`——循环 reject 时
//!   流**不会**结束。Rust 侧完全保留这一点：任务在 [`run_agent_loop`] 返回 `Err`（或 panic）
//!   时直接退出，不 `end()`、不合成错误事件，`result()` 永远 pending。
//! - [`run_agent_loop`] / [`run_agent_loop_continue`]：对应的 `Promise` 形态，供 `Agent`
//!   之类的调用方自己拿消息数组（而不是事件流）。
//! - [`run_tool_call`]：`runToolCall`，让工具内部发起的调用复用同一套 prepare/validate/
//!   hooks/execute/finalize 流程。
//!
//! 事件顺序、`durationMs` 的有无、并行批次「end 按完成序 / 消息按源序」等细节都按 TS
//! 逐条保持（见 `spec/agent-codemode.md` §2.1.4 与 §8.2 第 5 点）。
//!
//! ## 在 crate 中的位置
//!
//! - 上游：`crate::agent::Agent` 用 [`run_agent_loop`] / [`run_agent_loop_continue`] 驱动一次
//!   run：它自己消费返回的消息数组，并经自身的 `event_sink`（`crate::agent` 里的
//!   `process_events`）转发事件。[`agent_loop`] / [`agent_loop_continue`] 只是对外的事件流
//!   封装（内部 spawn 循环任务并在正常结束时 `end()` 流），仓库内目前只有测试直接调用它们，
//!   **不是** `Agent` 的调用路径；工具实现（以及测试里的嵌套工具 runner）用
//!   [`run_tool_call`] 复用同一套 prepare/validate/hooks/execute/finalize 管线。
//! - 下游：provider 流是 `kiana-ai::event_stream::AssistantMessageEventStream`；工具声明的
//!   回放与差量来自 `kiana-ai::utils::transcript`；参数校验来自
//!   `kiana-ai::utils::validation`。本模块不直接接触 HTTP 或 provider 适配器。
//!
//! ## 一次 run 的执行流程
//!
//! 1. [`run_agent_loop`] 先对 `(context, prompts)` 跑 `declareToolChanges`：把「运行时可执行
//!    工具集」与「transcript 里已声明的工具集」的差量写进 pending 消息（必要时新插一条
//!    system 消息）；这些消息既是本轮的初始消息，也会被追加进上下文。
//! 2. 发 `agent_start`、`turn_start`，并为每条初始消息发 `message_start` / `message_end`。
//! 3. 进入 `run_loop`：`prepareNextTurn` → 投递 pending 消息 → `prepareRequest` → 流式
//!    assistant 请求 → 工具批次 → `finishTurn` → `turn_end`，按结果决定继续内层、回到外层
//!    还是结束整个 run。
//! 4. 正常停止时发 `agent_end`；[`AgentEventStream`] 以此事件为完成信号并提交 `result()`。
//!
//! ## 并发与取消
//!
//! - 事件生产是单任务串行的：`emit` 被逐次 `await`，事件顺序就是源码里的调用顺序。
//! - 并行工具批次不会为每个工具 `spawn` 任务：`join_all_ordered` 在同一个任务里按源顺序
//!   协作式轮询所有 future。因此工具可以并发等待 I/O，但某个工具内部同步阻塞会拖住整批。
//! - `signal` 会被克隆进 provider 流选项与每个工具钩子；工具管线在 `beforeToolCall` 返回后、
//!   `prepare_tool_call` 返回 `Prepared` 之前，以及每个工具之间检查取消（`is_aborted`，
//!   即 `CancellationToken::is_cancelled`）。在 `prepare_tool_call` 内部，查工具、
//!   `prepareArguments`、校验与进入 `beforeToolCall` 这些步骤之前没有取消检查。取消不会
//!   打断正在 `await` 的 `execute()`——工具是否响应取消由工具实现自己决定。
//!
//! ## 失败模式
//!
//! - [`run_agent_loop`] / [`run_agent_loop_continue`] 的 `Err` 来自：`run_agent_loop_continue`
//!   开头两条前置校验（[`AgentLoopError::NoMessages`] / [`AgentLoopError::AssistantRole`]）、
//!   默认流函数未配置、`stream_fn` 返回错误。[`AgentLoopError::StreamEnded`] 只是防御分支：
//!   调用 `result()` 期间流句柄一直存活，kiana-ai 的契约下该错误不会返回；assistant 流
//!   没有最终结果时 `result()` 会永远 pending（run 挂起），而不是得到 `Err`。钩子签名
//!   不可失败；工具 `execute` 的 `Err` 会被转成 `isError: true` 的工具结果，不会冒泡成
//!   [`AgentLoopError`]。
//! - [`agent_loop`] / [`agent_loop_continue`] 把循环放进 `tokio::spawn`：任务里出现 `Err`
//!   或 panic 时不会调用 `end()`，之后 `recv()` 在队列排空后永远等待、`result()` 永不返回
//!   （与 TS 无 `catch` 的行为一致）。
//!
//! ## 阅读顺序
//!
//! 1. [`AgentEventStream`]（事件流原语）→ 2. [`agent_loop`] / [`run_agent_loop`]（入口与初始
//!    事件）→ 3. `run_loop`（主状态机）→ 4. `declare_tool_changes`（工具差量）→
//! 5. `stream_assistant_response`（流式上下文）→ 6. `prepare_tool_call` 开始的工具管线 →
//! 7. [`run_tool_call`]（复用同一管线的公开入口）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::{AssistantMessageEventStream, StreamEndedWithoutResult};
use kiana_ai::types::ThinkingLevel as AiThinkingLevel;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message,
    ModelThinkingLevel, SimpleStreamOptions, StopReason, SystemContent, SystemMessage, SystemRole,
    TextContent, TextKind, Tool, ToolResultMessage, ToolResultRole, UserContentBlock,
};
use kiana_ai::utils::identity::merge_client_identity;
use kiana_ai::utils::transcript::{
    ToolStateChanges, get_current_tools, get_tool_state_changes, normalize_context,
    to_tool_declaration,
};
use kiana_ai::utils::validation::{
    check_json_schema, coerce_with_json_schema, collect_json_schema_errors, format_validation_path,
    normalize_optional_nulls, validate_tool_arguments, value_convert,
};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::identity::default_agent_identity;
use crate::stream_fn::{DefaultStreamFnError, get_default_stream_fn};
use crate::types::{
    AfterToolCallContext, AfterToolCallFn, AfterToolCallResult, AgentContext, AgentEvent,
    AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage, AgentRequestUpdate, AgentTool,
    AgentToolCall, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback,
    AgentTurnContext, AgentTurnDecision, BeforeToolCallContext, BeforeToolCallFn, BoxFuture,
    JsonObject, JsonValue, PrepareRequestContext, StreamFn, ToolArguments, ToolExecutionMode,
};

// ---------------------------------------------------------------------------
// 事件流
// ---------------------------------------------------------------------------

/// 一条事件的等待者（对应 TS `FifoQueue<(r: IteratorResult<T>) => void>` 的一项）。
///
/// 事件先写进 `slot` 再用 `tx` 唤醒，而不是把事件塞进 oneshot：接收端可能在 `send`
/// 成功之后、poll 之前被 drop（`select!`/timeout），oneshot 会把事件一起丢掉且无法取回；
/// `slot` 让 [`AgentWaiterGuard`] 在取消时把未消费的事件放回队列。
struct Waiter {
    id: u64,
    slot: Arc<Mutex<Option<AgentEvent>>>,
    tx: oneshot::Sender<()>,
}

/// `AgentEventStream::recv` 注册等待者后的清理守卫。
///
/// future 被 drop 时移除等待者条目；若事件已写进槽位但还没被消费，把事件放回队首并
/// 立即尝试交给下一个等待者，保证取消 `recv` 不丢事件、也不打乱 FIFO。
struct AgentWaiterGuard {
    shared: Arc<AgentStreamShared>,
    id: u64,
    slot: Arc<Mutex<Option<AgentEvent>>>,
}

impl Drop for AgentWaiterGuard {
    fn drop(&mut self) {
        // 加锁顺序与 `push` 一致（state → slot）。
        let mut state = lock(&self.shared.state);
        if let Some(index) = state.waiting.iter().position(|waiter| waiter.id == self.id) {
            state.waiting.remove(index);
        }
        if let Some(event) = lock(&self.slot).take() {
            state.queue.push_front(event);
            deliver_queued_events(&mut state);
        }
    }
}

/// 把队列里的事件按 FIFO 逐个交给等待者（`push` 与取消回收共用，调用方须持 `state` 锁）。
fn deliver_queued_events(state: &mut AgentStreamState) {
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
            // 接收端已 drop；`AgentWaiterGuard` 的 Drop 持有同一把 state 锁，取不走槽位。
            if let Some(event) = lock(&waiter.slot).take() {
                state.queue.push_front(event);
            }
        }
    }
}

fn next_waiter_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// 一个 [`AgentEventStream`] 的全部共享状态；流句柄只持有这个 `Arc`。
///
/// `state` 与 `result` 是两把独立的锁：事件队列的读写不会挡住 `result()` 的订阅者。
/// 唯一的嵌套是 `push` 在持有 `state` 时调用 `commit_result` 获取 `result` 锁
/// （顺序 `state` → `result`）；不要构造反方向的持锁嵌套。
struct AgentStreamShared {
    state: Mutex<AgentStreamState>,
    /// TS `finalResultPromise` 的结果槽位（可重复读取；`None` = 尚未提交）。
    result: Mutex<Option<Vec<AgentMessage>>>,
    /// `result()` 的唤醒通道。发送端一直由流自身持有：未 `end()` 时 `result()` 永远 pending。
    result_tx: watch::Sender<Option<Vec<AgentMessage>>>,
}

/// 事件队列、等待者与结束标记。
///
/// 三者必须在这同一把锁下修改：`recv()` 依赖「队列为空且 `done`」这个组合快照，只有同一
/// 临界区才能保证消费者不会看到二者不一致的中间状态。
struct AgentStreamState {
    queue: VecDeque<AgentEvent>,
    waiting: VecDeque<Waiter>,
    /// TS `EventStream.done`：`push` 在 `done` 之后被静默忽略。
    done: bool,
}

/// `EventStream<AgentEvent, AgentMessage[]>` 的 Rust 形态。
///
/// 与 TS 的 `EventStream` 一样：同步 `push`、`end` 提交最终消息并唤醒所有迭代器、
/// `result()` 可重复调用、`recv()` 对应 `for await` 的一次迭代。句柄可克隆、可 drop，
/// 不会 panic。
#[derive(Clone)]
pub struct AgentEventStream {
    shared: Arc<AgentStreamShared>,
    /// TS `EventStream.isComplete` + `extractResult`：判定终态事件并取出最终结果。
    ///
    /// `createAgentStream()` 传的是 `(event) => event.type === "agent_end"` /
    /// `(event) => event.messages`：`agent_end` 一入流，`done` 与 `result()` 就提交。
    is_complete: AgentEventCompletion,
}

/// `EventStream.isComplete` + `extractResult` 的合并形态（返回 `Some` = 终态）。
type AgentEventCompletion = Arc<dyn Fn(&AgentEvent) -> Option<Vec<AgentMessage>> + Send + Sync>;

impl Default for AgentEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEventStream {
    /// `new EventStream<AgentEvent, AgentMessage[]>(...)`。
    ///
    /// 与 `createAgentStream()` 一样以 `agent_end` 为终态：`push(agent_end)` 立即提交
    /// `result()`（= `event.messages`）并把流标记为 `done`；之后 `push` 被静默忽略。
    pub fn new() -> Self {
        let (result_tx, _result_rx) = watch::channel(None);
        AgentEventStream {
            shared: Arc::new(AgentStreamShared {
                state: Mutex::new(AgentStreamState {
                    queue: VecDeque::new(),
                    waiting: VecDeque::new(),
                    done: false,
                }),
                result: Mutex::new(None),
                result_tx,
            }),
            is_complete: Arc::new(|event: &AgentEvent| match event {
                AgentEvent::AgentEnd { messages } => Some(messages.clone()),
                _ => None,
            }),
        }
    }

    /// `push(event)`：同步写入；`done` 之后静默忽略。事件先入队，再按 FIFO 交给最早
    /// 注册的等待者；没有等待者时留在队列里。
    ///
    /// 与 TS `EventStream.push` 一样，先判定终态（`agent_end` → `done = true`、提交结果），
    /// 再把事件本身投递给等待者或入队（终态事件同样会被消费到）。
    pub fn push(&self, event: AgentEvent) {
        let completion = (self.is_complete)(&event);
        let mut state = lock(&self.shared.state);
        if state.done {
            return;
        }
        if let Some(messages) = completion {
            state.done = true;
            self.commit_result(messages);
        }
        // 先入队再按 FIFO 交给等待者：队列里可能已有被取消的等待者回收的事件，先投递
        // 更早的那个才不会被取消路径打乱顺序。
        state.queue.push_back(event);
        deliver_queued_events(&mut state);
    }

    /// 提交最终结果（TS `resolveFinalResult`：Promise resolve 幂等，只认第一次）。
    fn commit_result(&self, messages: Vec<AgentMessage>) {
        let mut slot = lock(&self.shared.result);
        if slot.is_none() {
            *slot = Some(messages.clone());
            let _ = self.shared.result_tx.send(Some(messages));
        }
    }

    /// `end(messages)`：提交最终消息、结束流并唤醒所有等待中的迭代器。
    ///
    /// 与 TS 一样只提交第一次的结果（Promise resolve 幂等）。
    ///
    /// 与 `push(agent_end)` 的区别：`end()` 不投递任何事件，只置 `done` 并唤醒等待者；
    /// 已经排队的事件仍会被 `recv()` 依次取走，之后才返回 `None`。
    pub fn end(&self, messages: Vec<AgentMessage>) {
        self.commit_result(messages);
        let waiters: Vec<Waiter> = {
            let mut state = lock(&self.shared.state);
            state.done = true;
            state.waiting.drain(..).collect()
        };
        for waiter in waiters {
            let _ = waiter.tx.send(());
        }
    }

    /// `result()`：等待 `agent_end` 里的最终消息数组。
    ///
    /// 与 TS 的 `finalResultPromise` 一样可重复调用；从未 `end()` 时永远 pending。
    ///
    /// 需要等待时，每次调用各自订阅一份 `watch` 接收端，因此多个消费者可以并发等待同一个
    /// 结果，各自拿到一份消息数组的克隆。
    pub async fn result(&self) -> Vec<AgentMessage> {
        {
            let slot = lock(&self.shared.result);
            if let Some(messages) = slot.as_ref() {
                return messages.clone();
            }
        }
        let mut rx = self.shared.result_tx.subscribe();
        loop {
            if let Some(messages) = rx.borrow_and_update().clone() {
                return messages;
            }
            if rx.changed().await.is_err() {
                // 发送端由流自身持有；只有整个 `Arc` 被释放才可能走到这里。
                std::future::pending::<()>().await;
            }
        }
    }

    /// 接收下一个事件（`for await` 的一次迭代）；流已结束且队列为空时返回 `None`。
    ///
    /// 等待期间 future 被 drop（`select!`、timeout）是安全的：[`AgentWaiterGuard`] 会
    /// 移除等待者条目，并把已投递但未消费的事件放回队首、转交给下一个等待者。
    pub async fn recv(&self) -> Option<AgentEvent> {
        let (guard, wake) = {
            let mut state = lock(&self.shared.state);
            if let Some(event) = state.queue.pop_front() {
                return Some(event);
            }
            if state.done {
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
                AgentWaiterGuard {
                    shared: Arc::clone(&self.shared),
                    id,
                    slot,
                },
                rx,
            )
        };
        // `end()` 唤醒时槽位为空 → 迭代结束；`push` 唤醒时槽位里有事件。
        wake.await.ok();
        let event = lock(&guard.slot).take();
        drop(guard);
        event
    }
}

/// 取互斥锁；锁中毒时取回内部数据继续使用，而不是把 panic 传播给之后的所有调用者。
///
/// 中毒只可能来自本模块持锁期间的 panic。`AgentEventStream` 是跨任务共享的流原语，
/// 让一次意外 panic 之后所有 `push`/`recv` 都永久失败没有收益，因此这里取回数据继续用。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `AgentEventSink`：事件消费者。异步、按调用顺序 await（TS `(event) => Promise<void> | void`）。
///
/// 循环必须等一次 `emit` 的 future 完成才发下一个事件，因此 sink 是事件顺序的唯一定序点。
/// sink 自身 panic 会沿调用栈展开：在 [`agent_loop`] 的 spawn 任务里会直接终止该任务
/// （流不会 `end`），在 [`run_agent_loop`] 里则原样传播给调用方。
pub type AgentEventSink = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>;

/// `runAgentLoop` / `runAgentLoopContinue` 的错误。
///
/// 低层循环里唯一会失败的异步步骤是取流函数（未配置默认流函数）与 provider 流本身；
/// 其余钩子按 TS 的类型都是不可失败的（TS 侧抛错行为在 Rust 的 `Fn` 契约里不存在）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AgentLoopError {
    /// `agentLoopContinue` 的前置校验（逐字）。
    #[error("Cannot continue: no messages in context")]
    NoMessages,
    /// `agentLoopContinue` 的前置校验（逐字）。
    #[error("Cannot continue from message role: assistant")]
    AssistantRole,
    /// `getDefaultStreamFn()` 未配置。
    #[error("{0}")]
    DefaultStreamFn(#[from] DefaultStreamFnError),
    /// provider 流创建失败（TS 的 `streamFunction` reject）。
    #[error("{0}")]
    Ai(Box<AiError>),
    /// 流结束但没有最终结果（TS 里对应的 `result()` 永远 pending）。
    ///
    /// kiana-ai 的 `result()` 只在结果通道关闭（最后一个流句柄释放）时返回该错误；
    /// `end_without_result()` 造成的“无结果”会让 `result()` 永远 pending，不会走到这里
    /// （见 `crates/kiana-ai/src/event_stream.rs` 的 `result()`）。
    #[error("assistant message event stream ended without a result")]
    StreamEnded(#[from] StreamEndedWithoutResult),
    /// loop guard 硬停。已经产生的消息（含此前的 tool result）仍已通过事件发出。
    #[error("loop detected ({}): {}", .0.kind.as_str(), .0.message)]
    LoopDetected(Box<crate::loop_guard::LoopDetection>),
}

// ---------------------------------------------------------------------------
// agentLoop / agentLoopContinue / runAgentLoop / runAgentLoopContinue
// ---------------------------------------------------------------------------

/// `agentLoop(prompts, context, config, signal, streamFn)`。
///
/// Prompt 被追加进上下文并为它们发出 `message_start`/`message_end`；返回的流在循环正常
/// 结束时以 `agent_end.messages` 结束。循环内部失败（如 `convertToLlm` panic、provider
/// 流报错）时流**不会**结束（TS 无 `catch`）。
///
/// 必须在 tokio 运行时内调用（内部会 spawn 循环任务）。
///
/// 返回时只是创建了流并把循环交给了 tokio：`prompts` 与 `context` 已被移入任务，事件是
/// 之后异步产生的。循环内部失败（包括默认流函数未配置——它在初始事件之后才解析）都不会
/// 结束返回的流。
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> AgentEventStream {
    let stream = AgentEventStream::new();
    let sink_stream = stream.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        sink_stream.push(event);
        Box::pin(async {})
    });
    let task_stream = stream.clone();
    tokio::spawn(async move {
        if let Ok(messages) =
            run_agent_loop(prompts, context, config, emit, signal, stream_fn).await
        {
            task_stream.end(messages);
        }
    });
    stream
}

/// `agentLoopContinue(context, config, signal, streamFn)`。
///
/// 同步前置校验在创建流之前完成，失败时不创建流：
/// - 上下文没有消息 → [`AgentLoopError::NoMessages`]
/// - 最后一条是 `assistant` → [`AgentLoopError::AssistantRole`]
///
/// 与 [`agent_loop`] 不同，入口处不追加任何 prompt，也不预计算初始工具声明差量；但每次
/// 请求前的 `declareToolChanges` 仍由 `run_loop` 执行——`run_loop` 每轮内层迭代都会调用
/// `declare_tool_changes`，缺声明时照样新建 system 消息并追加进上下文。校验通过后的失败
/// 同样不会结束返回的流。
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> Result<AgentEventStream, AgentLoopError> {
    validate_continue(&context)?;
    let stream = AgentEventStream::new();
    let sink_stream = stream.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        sink_stream.push(event);
        Box::pin(async {})
    });
    let task_stream = stream.clone();
    tokio::spawn(async move {
        if let Ok(messages) =
            run_agent_loop_continue(context, config, emit, signal, stream_fn).await
        {
            task_stream.end(messages);
        }
    });
    Ok(stream)
}

/// `runAgentLoop(prompts, context, config, emit, signal, streamFn)`。
///
/// `stream_fn` 为 `None` 时在发出初始事件之后才解析默认流函数（TS 的惰性求值位置）。
///
/// 步骤：
/// 1. `declare_tool_changes(&context, &prompts)` 得出本轮的初始消息（可能含新插入的 system
///    消息）；它们同时追加进 `context.messages` 与返回值 `new_messages`。
/// 2. 发 `agent_start`、`turn_start`，并为每条初始消息发 `message_start`/`message_end`。
/// 3. 解析流函数（此时事件已经发出；失败会直接返回 `Err`，不会补发 `agent_end`）。
/// 4. 进入 `run_loop`，正常返回后把 `new_messages` 作为本次 run 的全部新增消息返回。
///
/// `context` 按值传入，函数内部会扩展它的消息列表；这些修改对调用方不可见。返回值只包含
/// 新增消息，不包含原始 `context.messages`。
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    mut context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentLoopError> {
    // 初始消息 = 调用方 prompts 加上必要的工具声明差量；它们既追加进上下文，也计入
    // 本次 run 的返回值。
    let initial_messages = declare_tool_changes(&context, &prompts);
    let mut new_messages: Vec<AgentMessage> = initial_messages.clone();
    context.messages.extend(initial_messages.iter().cloned());

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    // 初始消息不经过流式请求，直接成对发出 `message_start` / `message_end`。
    for message in &initial_messages {
        emit(AgentEvent::MessageStart {
            message: message.clone(),
        })
        .await;
        emit(AgentEvent::MessageEnd {
            message: message.clone(),
        })
        .await;
    }

    let stream_fn = resolve_stream_fn(stream_fn)?;
    run_loop(context, &mut new_messages, config, signal, emit, stream_fn).await?;
    Ok(new_messages)
}

/// `runAgentLoopContinue(context, config, emit, signal, streamFn)`。
///
/// 与 [`run_agent_loop`] 不同，这里不追加任何 prompt，也不预计算初始工具声明差量；进入
/// `run_loop` 之后，每次请求前的 `declareToolChanges` 仍由 `run_loop` 执行。它重复一遍
/// [`agent_loop_continue`] 的两条前置校验（失败时直接返回 `Err`，此时还没有任何事件），
/// 然后发 `agent_start`/`turn_start` 进入 `run_loop`。
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentLoopError> {
    validate_continue(&context)?;
    let mut new_messages: Vec<AgentMessage> = Vec::new();
    let mut current_context = context;

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;

    let stream_fn = resolve_stream_fn(stream_fn)?;
    // `current_context` 之后不再使用：用 `mem::take` 把上下文按值移进 `run_loop`。
    run_loop(
        std::mem::take(&mut current_context),
        &mut new_messages,
        config,
        signal,
        emit,
        stream_fn,
    )
    .await?;
    Ok(new_messages)
}

/// 两条同步前置校验（`runAgentLoopContinue` 也重复一遍，与 TS 相同）。
///
/// 只按 `role()` 判断最后一条消息：自定义消息的 role 来自其 JSON 的 `role` 字段，缺失时
/// 是空串，因此任何非 `"assistant"` 的自定义消息都算可继续（测试
/// `should_allow_custom_message_types_as_last_message` 锁定这一点）。
fn validate_continue(context: &AgentContext) -> Result<(), AgentLoopError> {
    if context.messages.is_empty() {
        return Err(AgentLoopError::NoMessages);
    }
    if context
        .messages
        .last()
        .map(AgentMessage::role)
        .is_some_and(|role| role == "assistant")
    {
        return Err(AgentLoopError::AssistantRole);
    }
    Ok(())
}

/// `streamFn ?? getDefaultStreamFn()`：显式传入的优先，否则读进程级默认值。
///
/// 这是 TS 的惰性求值点：`run_agent_loop` / `run_agent_loop_continue` 在发完初始事件之后
/// 才调用它，所以未配置默认流函数时调用方会先看到 `agent_start` 等事件，然后得到 `Err`；
/// 经由 `agent_loop` / `agent_loop_continue` 时该错误被 spawn 任务吞掉，流不会 `end`
/// （见 `crate::stream_fn`）。
fn resolve_stream_fn(stream_fn: Option<StreamFn>) -> Result<StreamFn, AgentLoopError> {
    match stream_fn {
        Some(stream_fn) => Ok(stream_fn),
        None => {
            get_default_stream_fn().ok_or(AgentLoopError::DefaultStreamFn(DefaultStreamFnError))
        }
    }
}

// ---------------------------------------------------------------------------
// runLoop
// ---------------------------------------------------------------------------

/// `runLoop`：主循环（伪代码见 `spec/agent-codemode.md` §2.1.4）。
///
/// 硬退出（`error`/`aborted`）仍会跑 `finishTurn` 并发出 `turn_end`/`agent_end`，且不再
/// 轮询 steering/follow-up；正常结束的 turn 才可能设置 `explicitContinuation`。
///
/// # 状态
///
/// - `last_completed_turn`：上一个完成 turn 的快照（消息、工具结果、上下文、新增消息）；
///   内层每次迭代开始时若存在就调用一次 `prepareNextTurn`。
/// - `explicit_continuation`：`finishTurn` 返回 `Continue`、且本轮没有其他自然请求
///   （无待继续的工具批次、无排队消息）时为真；外层据此再发一次“只用当前上下文”的请求。
/// - `pending_messages`：steering 轮询结果；经 `declareToolChanges` 投递给模型后清空。
///
/// # 两层循环
///
/// - 外层：内层跑空后检查 follow-up 队列；有消息就带着它们回到内层，否则运行结束。
/// - 内层：条件 `has_more_tool_calls || !pending_messages.is_empty()`。前者由工具批次的
///   `terminate` 决定，后者由 steering 是否取到消息决定。
///
/// # 退出
///
/// - assistant 的 `stop_reason` 是 `error`/`aborted`：硬退出（见上）。
/// - 正常 turn 且 `finishTurn` 返回 `End`：发 `turn_end` 后直接 `agent_end` 返回。
/// - 内层条件不成立、follow-up 为空、也没有 `explicit_continuation`：跳出外层，发
///   `agent_end` 返回。
async fn run_loop(
    initial_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    initial_config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    emit: AgentEventSink,
    stream_fn: StreamFn,
) -> Result<(), AgentLoopError> {
    let mut current_context = initial_context;
    let mut config = initial_config;
    let mut last_completed_turn: Option<AgentTurnContext> = None;
    let mut explicit_continuation = false;
    // 进入外层循环之前先轮询一次 steering（用户可能在等待期间已输入）。
    let mut pending_messages = poll_steering(&config).await;

    // 外层迭代：内层把工具调用与排队消息耗尽后，检查 follow-up；有消息就带回去重跑内层。
    'outer: loop {
        // 每次外层迭代都从 `true` 开始：即使 follow-up 没有触发工具调用，也要先发起一次
        // 请求，否则内层循环一次都不会执行。
        let mut has_more_tool_calls = true;

        // 内层循环：处理工具调用与 steering 消息。
        while has_more_tool_calls || !pending_messages.is_empty() {
            // 每次内层迭代 = 一次 provider 请求 = 一个 budget turn。turn / wall-clock
            // 超限时在这里直接停止，不产生额外的 assistant 消息。
            let mut budget_reminders: Vec<String> = Vec::new();
            let mut budget_force_final: Option<crate::budget::BudgetStopReason> = None;
            if let Some(budget) = &config.budget {
                let decision = {
                    let mut controller = budget
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    controller.begin_turn()
                };
                if let Some(reason) = decision.stop {
                    let mut controller = budget
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    controller.mark_capped(reason);
                    break 'outer;
                }
                budget_force_final = decision.force_final;
                budget_reminders.extend(
                    decision
                        .warnings
                        .iter()
                        .map(crate::budget::BudgetWarning::reminder),
                );
                if let Some(reason) = budget_force_final {
                    budget_reminders.push(crate::budget::final_turn_instruction(reason));
                    // 100%：移除工具列表。`declare_tool_changes` 会把“工具被移除”的差量写进
                    // system 消息，因此模型下一次请求看不到任何工具声明。
                    current_context.tools = None;
                }
            }

            let mut prepared_messages: Vec<AgentMessage> = Vec::new();
            // 首个 turn 之前没有可用快照；`prepareNextTurn` 只在完成过 turn 之后调用。
            if let Some(turn) = &last_completed_turn {
                let snapshot: Option<AgentLoopTurnUpdate> = match config.prepare_next_turn.clone() {
                    Some(prepare) => prepare(turn.clone()).await,
                    None => None,
                };
                // 快照的每个字段独立可选：`None` 表示保留当前值（TS 的 `??` 语义）。
                if let Some(snapshot) = snapshot {
                    if let Some(context) = snapshot.context {
                        current_context = context;
                    }
                    prepared_messages = snapshot.messages.unwrap_or_default();
                    if let Some(model) = snapshot.model {
                        config.model = model;
                    }
                    if let Some(level) = snapshot.thinking_level {
                        config.options.reasoning = model_level_to_ai(level);
                    }
                }
                // 准备过程可能很慢（例如压缩），期间排队的 steering 在这里补取；
                // 仅当之前那次轮询没有返回内容时才轮询，避免 one-at-a-time 模式一次送达两条。
                if pending_messages.is_empty() {
                    pending_messages = poll_steering(&config).await;
                }
                emit(AgentEvent::TurnStart).await;
            }

            // 先处理 prepared 与排队消息，再请求下一次 assistant 回复。
            // prepared 在前、steering 在后；两者一起过 `declareToolChanges`，因此工具差量
            // 可以合并进其中的 system 消息，而不是另起一条。
            let mut combined = prepared_messages;
            combined.append(&mut pending_messages);
            for message in declare_tool_changes(&current_context, &combined) {
                emit(AgentEvent::MessageStart {
                    message: message.clone(),
                })
                .await;
                emit(AgentEvent::MessageEnd {
                    message: message.clone(),
                })
                .await;
                current_context.messages.push(message.clone());
                new_messages.push(message);
            }
            pending_messages.clear();

            // loop guard 的提醒只在**下一次请求前**注入；此时上一条 assistant(tool_calls)
            // 与所有 toolResult 都已写入上下文，绝不会插在两者之间。
            if let Some(guard) = &config.loop_guard {
                let reminder = {
                    let mut guard = guard
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.take_reminder()
                };
                if let Some(reminder) = reminder {
                    budget_reminders.push(reminder);
                }
            }
            for reminder in budget_reminders {
                let message = AgentMessage::Standard(Message::System(SystemMessage {
                    role: SystemRole::System,
                    content: SystemContent::Text(reminder),
                    sections: None,
                    tools_added: None,
                    tools_removed: None,
                    timestamp: now_unix_ms(),
                }));
                emit(AgentEvent::MessageStart {
                    message: message.clone(),
                })
                .await;
                emit(AgentEvent::MessageEnd {
                    message: message.clone(),
                })
                .await;
                current_context.messages.push(message.clone());
                new_messages.push(message);
            }

            // `prepareRequest` 看到的是已包含本轮新消息的上下文；返回的更新只覆盖给出的字段。
            let update: Option<AgentRequestUpdate> = match config.prepare_request.clone() {
                Some(prepare) => {
                    prepare(
                        PrepareRequestContext {
                            context: current_context.clone(),
                            model: config.model.clone(),
                            thinking_level: reasoning_or_off(&config),
                        },
                        signal.clone(),
                    )
                    .await
                }
                None => None,
            };
            if let Some(update) = update {
                if let Some(context) = update.context {
                    current_context = context;
                }
                if let Some(model) = update.model {
                    config.model = model;
                }
                if let Some(level) = update.thinking_level {
                    config.options.reasoning = model_level_to_ai(level);
                }
            }
            if budget_force_final.is_some() {
                // `prepareRequest` 可能替换整个 context；final turn 仍然必须无工具。
                current_context.tools = None;
            }

            // 流式 assistant 回复。final turn 的 cap 由这里写入 assistant 消息的
            // `stop_reason`（`completed`/`failed` 不会被覆盖）。
            let mut message = stream_assistant_response(
                &mut current_context,
                &config,
                signal.clone(),
                &emit,
                &stream_fn,
                budget_force_final,
            )
            .await?;

            if let Some(budget) = &config.budget {
                let mut controller = budget
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                controller.record_usage(&message.usage);
            }
            // `stream_assistant_response` 会把流式 partial 与最终消息都写入
            // `current_context`（过程中反复替换最后一条，最终留下最终消息），但不写
            // `new_messages`；返回值这一侧还要显式收一份（对应 TS 的
            // `newMessages.push(message)`）。
            new_messages.push(AgentMessage::Standard(Message::Assistant(message.clone())));

            // 硬退出：`error`/`aborted` 的 turn 不做工具调用、不轮询队列，但仍走
            // `finishTurn` 与 `turn_end`/`agent_end` 的正常收尾。
            if message.stop_reason == StopReason::Error
                || message.stop_reason == StopReason::Aborted
            {
                let turn = AgentTurnContext {
                    message: message.clone(),
                    tool_results: Vec::new(),
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };
                if let Some(finish) = config.finish_turn.clone() {
                    let _ = finish(turn, signal.clone()).await;
                }
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Standard(Message::Assistant(message)),
                    tool_results: Vec::new(),
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            // loop guard：在工具真正执行之前观察本次 assistant 输出（工具参数、thought、
            // 文本）。硬停时不执行这批工具调用，已经产生的 tool result 保留在 transcript 中。
            if let Some(guard) = &config.loop_guard {
                let outcome = {
                    let mut guard = guard
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.observe_assistant(&message)
                };
                if let crate::loop_guard::LoopGuardOutcome::Stop(detection) = outcome {
                    return Err(AgentLoopError::LoopDetected(Box::new(detection)));
                }
            }

            // 检查工具调用。
            let tool_calls: Vec<AgentToolCall> = message
                .content
                .iter()
                .filter_map(|content| match content {
                    AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
                    _ => None,
                })
                .collect();

            // token / cost 达到 100%：final turn 不执行任何工具，只把未执行的调用记成失败
            // tool result（保持 transcript 的 assistant(tool_calls) → toolResult 形状），
            // 然后以 cap stop reason 结束 run。
            if let Some(reason) = budget_force_final {
                let mut tool_results: Vec<ToolResultMessage> = Vec::new();
                for call in &tool_calls {
                    let result = ToolResultMessage {
                        role: ToolResultRole::ToolResult,
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        content: vec![UserContentBlock::Text(TextContent {
                            kind_text: TextKind::Text,
                            text: format!(
                                "Tool call `{}` was not executed: the {} budget is exhausted. \
                                 Respond with a final answer without tools.",
                                call.name,
                                reason.as_str()
                            ),
                            text_signature: None,
                        })],
                        details: None,
                        usage: None,
                        nested_calls: None,
                        is_error: true,
                        timestamp: now_unix_ms(),
                        duration_ms: None,
                    };
                    tool_results.push(result.clone());
                    current_context
                        .messages
                        .push(AgentMessage::Standard(Message::ToolResult(result.clone())));
                    new_messages.push(AgentMessage::Standard(Message::ToolResult(result)));
                }
                // `observe_assistant` 已经把本批 tool call 记入 `pending_result`；跳过
                // 执行时必须补上对应的合成结果，否则下一轮的 tool result 会被错误地
                // 匹配到这批过期条目（停滞/失败检测会归因到错误的调用）。
                if let Some(guard) = &config.loop_guard {
                    let mut outcome = crate::loop_guard::LoopGuardOutcome::Continue;
                    for (call, result) in tool_calls.iter().zip(&tool_results) {
                        let output = crate::loop_guard::tool_result_text(result);
                        let observed = {
                            let mut guard = guard
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            guard.observe_tool_result(
                                &call.name,
                                &call.arguments,
                                &output,
                                result.is_error,
                            )
                        };
                        outcome = outcome.merge(observed);
                    }
                    if let crate::loop_guard::LoopGuardOutcome::Stop(detection) = outcome {
                        return Err(AgentLoopError::LoopDetected(Box::new(detection)));
                    }
                }
                if let Some(budget) = &config.budget {
                    let mut controller = budget
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    controller.finalize(message.stop_reason);
                }
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Standard(Message::Assistant(message)),
                    tool_results,
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                // `length` 表示输出被 token 上限截断：消息里的工具调用参数可能不完整，
                // 全部按失败处理，而不是执行可能已损坏的调用。
                let batch = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, &emit).await
                } else {
                    execute_tool_calls(
                        &current_context,
                        &mut message,
                        &config,
                        signal.clone(),
                        &emit,
                    )
                    .await
                };
                // `prepareArguments` 就地改写的是 assistant 消息里的同一个 toolCall 对象
                // （TS 引用共享）：把改写后的调用同步进上下文与本轮返回值里的副本。
                sync_assistant_message(&mut current_context, new_messages, &message);
                tool_results.extend(batch.messages.iter().cloned());
                has_more_tool_calls = !batch.terminate;

                for result in &batch.messages {
                    current_context
                        .messages
                        .push(AgentMessage::Standard(Message::ToolResult(result.clone())));
                    new_messages.push(AgentMessage::Standard(Message::ToolResult(result.clone())));
                }

                // loop guard：工具结果写入 transcript 之后再观察（失败重复、shell 停滞等）。
                // 硬停返回 `LoopDetected`，前 N 个 tool result 已经保留。
                if let Some(guard) = &config.loop_guard {
                    let mut outcome = crate::loop_guard::LoopGuardOutcome::Continue;
                    for result in &batch.messages {
                        let Some(call) = tool_calls
                            .iter()
                            .find(|call| call.id == result.tool_call_id)
                        else {
                            continue;
                        };
                        let output = crate::loop_guard::tool_result_text(result);
                        let observed = {
                            let mut guard = guard
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            guard.observe_tool_result(
                                &call.name,
                                &call.arguments,
                                &output,
                                result.is_error,
                            )
                        };
                        outcome = outcome.merge(observed);
                    }
                    if let crate::loop_guard::LoopGuardOutcome::Stop(detection) = outcome {
                        return Err(AgentLoopError::LoopDetected(Box::new(detection)));
                    }
                }
            }

            // 快照在工具结果写入之后构造：`finishTurn` 能看到完整的 `tool_results` 与
            // 已追加消息的上下文。
            let turn = AgentTurnContext {
                message: message.clone(),
                tool_results: tool_results.clone(),
                context: current_context.clone(),
                new_messages: new_messages.clone(),
            };
            last_completed_turn = Some(turn.clone());
            let decision: Option<AgentTurnDecision> = match config.finish_turn.clone() {
                Some(finish) => finish(turn, signal.clone()).await,
                None => None,
            };
            emit(AgentEvent::TurnEnd {
                message: AgentMessage::Standard(Message::Assistant(message)),
                tool_results,
            })
            .await;

            // `end`：立即结束整个 run，不再轮询队列，也不准备下一轮。
            if decision == Some(AgentTurnDecision::End) {
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            // 有自然请求（工具批次还要继续 / steering 非空）时，`continue` 已经被这次请求
            // 兑现，不应再额外发一次“只带上下文”的请求。
            explicit_continuation = decision == Some(AgentTurnDecision::Continue);
            pending_messages = poll_steering(&config).await;
            if has_more_tool_calls || !pending_messages.is_empty() {
                explicit_continuation = false;
            }
        }

        // agent 本应在此停止：检查 follow-up 消息。
        // follow-up 优先于 `explicit_continuation`：前者带回消息，后者只是重复当前上下文。
        let follow_up_messages = match config.get_follow_up_messages.clone() {
            Some(get_messages) => get_messages().await,
            None => Vec::new(),
        };
        if !follow_up_messages.is_empty() {
            explicit_continuation = false;
            pending_messages = follow_up_messages;
            continue 'outer;
        }

        // 没有任何自然请求满足 continue 决策时，用一次仅使用当前上下文的请求兑现它。
        if explicit_continuation {
            explicit_continuation = false;
            continue 'outer;
        }

        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    })
    .await;
    Ok(())
}

/// 把可能被 `prepareArguments` 就地改写的 assistant 消息同步进上下文与本轮返回值。
///
/// TS 里 assistant 消息、`newMessages` 与 `currentContext.messages` 引用同一个消息对象，
/// `prepareArguments` 对 toolCall 的就地改写对三者同时可见；Rust 的值语义需要在工具批次
/// 执行后显式写回。此时上下文与本轮返回值的最后一条都是这条 assistant 消息
/// （`streamAssistantResponse` 刚把它放进上下文，runLoop 刚把它 push 进 `newMessages`）。
///
/// 不变量：调用点必须保证 `current_context.messages` 与 `new_messages` 的最后一条都仍是
/// 这条 assistant 消息（工具结果要在本函数之后才追加）；否则这里会覆盖错对象。
fn sync_assistant_message(
    current_context: &mut AgentContext,
    new_messages: &mut [AgentMessage],
    message: &AssistantMessage,
) {
    if let Some(last) = current_context.messages.last_mut() {
        *last = AgentMessage::Standard(Message::Assistant(message.clone()));
    }
    if let Some(last) = new_messages.last_mut() {
        *last = AgentMessage::Standard(Message::Assistant(message.clone()));
    }
}

/// 轮询 steering 队列：钩子未配置时返回空列表（TS 的 `?? []`）。
///
/// 只负责取消息，不去重、不清空调用方持有的队列；投递与清空由 `run_loop` 完成。
async fn poll_steering(config: &AgentLoopConfig) -> Vec<AgentMessage> {
    match config.get_steering_messages.clone() {
        Some(get_messages) => get_messages().await,
        None => Vec::new(),
    }
}

/// `config.reasoning ?? "off"`（`PrepareRequestContext.thinkingLevel`）。
fn reasoning_or_off(config: &AgentLoopConfig) -> ModelThinkingLevel {
    config
        .options
        .reasoning
        .map(ai_level_to_model)
        .unwrap_or(ModelThinkingLevel::Off)
}

/// TS `thinkingLevel === "off" ? undefined : thinkingLevel`。
fn model_level_to_ai(level: ModelThinkingLevel) -> Option<AiThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(AiThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(AiThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(AiThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(AiThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(AiThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(AiThinkingLevel::Max),
    }
}

/// [`model_level_to_ai`] 的逆映射：`AiThinkingLevel` 不含 `"off"`，因此结果不会出现
/// `ModelThinkingLevel::Off`。
fn ai_level_to_model(level: AiThinkingLevel) -> ModelThinkingLevel {
    match level {
        AiThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        AiThinkingLevel::Low => ModelThinkingLevel::Low,
        AiThinkingLevel::Medium => ModelThinkingLevel::Medium,
        AiThinkingLevel::High => ModelThinkingLevel::High,
        AiThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        AiThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

// ---------------------------------------------------------------------------
// declareToolChanges
// ---------------------------------------------------------------------------

/// `declareToolChanges(context, pendingMessages)`：把可执行工具集与 transcript 声明之间的
/// 差量写进 system 消息。
///
/// `context.tools` 是运行时可执行的工具；transcript 里的 system 消息声明模型可以调用的
/// 工具。若 pending 消息里已有 system 消息，它的 `toolsAdded`/`toolsRemoved` 被视作意图，
/// 替换为「已提交 transcript」与「可执行集合」之间的差量；否则在第一条非 system 消息之前
/// 插入一条新的 system 消息。
///
/// TS 的 system 判定只看 `message.role === "system"`：自定义消息只要 role 为 `"system"`
/// 同样参与合并与工具回放（`getCurrentTools` 只看 role）。
///
/// 算法（对应 `spec/agent-codemode.md` §2.1.4 的逐条伪代码）：
/// 1. 从后往前找最后一条 `role == "system"` 的 pending 消息；把它的工具字段抹掉后作为
///    `baseline`，再与 `context.messages` 拼接并回放出“已提交的工具集”。
/// 2. 把 `context.tools` 投影成声明，与上一步的集合求差。
/// 3. 有 pending system 消息：差量为空且它自己也没声明变更 → 内容原样返回（Rust 值语义下
///    是克隆，但键序与额外字段都不变）；否则把差量写回这条消息。
/// 4. 没有 pending system 消息且差量为空 → 原样返回。
/// 5. 否则新建一条空 system 消息承载差量，插到第一条非 system 消息之前（全是 system 时
///    追加到末尾）。
///
/// 返回值是新的 pending 消息列表；调用方负责把它逐条写进上下文与 `newMessages`。
fn declare_tool_changes(
    context: &AgentContext,
    pending_messages: &[AgentMessage],
) -> Vec<AgentMessage> {
    // 与 TS 的 `findLast` 一致取最后一条：多个 pending system 消息时只有最后一条承载
    // 本次差量，其余原样保留。
    let system_index = (0..pending_messages.len())
        .rev()
        .find(|&index| pending_messages[index].role() == "system");
    let no_changes = ToolStateChanges::default();
    // 先把最后那条 system 消息复制成“无工具变更”的版本再参与回放：它自带的
    // `toolsAdded`/`toolsRemoved` 表达的是本次意图，不属于已提交的 transcript。
    let baseline: Vec<AgentMessage> = match system_index {
        Some(index) => pending_messages
            .iter()
            .enumerate()
            .map(|(current, item)| {
                if current == index {
                    with_tool_changes(&pending_messages[index], &no_changes)
                } else {
                    item.clone()
                }
            })
            .collect(),
        None => pending_messages.to_vec(),
    };

    let transcript_messages = collect_transcript_messages(&context.messages, &baseline);
    // 回放 `context.messages` 与 baseline 里的 system 消息得到的工具集。
    let previous = get_current_tools(&transcript_messages);
    // 当前可执行工具集投影成声明；`context.tools = None` 视作空集。
    let current: Vec<Tool> = context
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(agent_tool_declaration).collect())
        .unwrap_or_default();
    let changes = get_tool_state_changes(&previous, &current);
    let unchanged = changes.tools_added.is_empty() && changes.tools_removed.is_empty();

    if let Some(index) = system_index {
        // 调用方消息本来就没有声明任何工具变更、差量也为空时，内容原样返回（键序与额外
        // 字段都不动）；否则把差量写回这条消息。
        if unchanged && pending_declares_no_tool_changes(&pending_messages[index]) {
            return pending_messages.to_vec();
        }
        return baseline
            .into_iter()
            .enumerate()
            .map(|(current, item)| {
                if current == index {
                    with_tool_changes(&pending_messages[index], &changes)
                } else {
                    item
                }
            })
            .collect();
    }

    // 没有 pending system 消息且差量为空：不需要声明，原样返回（不插入空 system 消息）。
    if unchanged {
        return pending_messages.to_vec();
    }

    // 有新工具需要声明：新建一条空 system 消息承载差量。
    let update = with_tool_changes(
        &AgentMessage::Standard(Message::System(SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text(String::new()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: now_unix_ms(),
        })),
        &changes,
    );
    let insert_index = pending_messages
        .iter()
        .position(|message| message.role() != "system")
        .unwrap_or(pending_messages.len());
    let mut declared = Vec::with_capacity(pending_messages.len() + 1);
    declared.extend_from_slice(&pending_messages[..insert_index]);
    declared.push(update);
    declared.extend_from_slice(&pending_messages[insert_index..]);
    declared
}

/// TS `!pending.toolsAdded?.length && !pending.toolsRemoved?.length`。
fn pending_declares_no_tool_changes(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::Standard(Message::System(system)) => {
            system
                .tools_added
                .as_ref()
                .is_none_or(|tools| tools.is_empty())
                && system
                    .tools_removed
                    .as_ref()
                    .is_none_or(|tools| tools.is_empty())
        }
        AgentMessage::Custom(object) => {
            !js_length_truthy(object.get("toolsAdded"))
                && !js_length_truthy(object.get("toolsRemoved"))
        }
        // 调用点只会在 `system_index`（按 role 定位的 system 消息）上调用本函数；
        // 标准 user/assistant/toolResult 消息在这里不可达。
        _ => true,
    }
}

/// JS `Boolean(value?.length)`：JSON 值上 `.length` 的取值及其真值。
///
/// TS 的 `!pending.toolsAdded?.length` 对任意 JSON 值都成立：数组按元素个数、字符串按
/// 长度、普通对象取 `length` 属性（再按 JS 真值判断），`null`/缺省/数字/布尔没有
/// `length`，一律为假。非数组但 `length` 为真的值（`"x"`、`{length:1}`）在 TS 里同样
/// 走 `withToolChanges` 重建路径并丢掉这个伪字段；只有这里也判为真才等价。
fn js_length_truthy(value: Option<&JsonValue>) -> bool {
    match value {
        Some(JsonValue::Array(items)) => !items.is_empty(),
        Some(JsonValue::String(text)) => !text.is_empty(),
        Some(JsonValue::Object(object)) => kiana_json::is_truthy(object.get("length")),
        _ => false,
    }
}

/// 把 `[...context.messages, ...baseline]` 投影成 transcript 回放能消费的 `Message[]`。
///
/// `getCurrentTools` 只关心 `role === "system"`；自定义消息只要 role 是 `"system"` 同样
/// 参与回放（`toolsAdded`/`toolsRemoved` 按字段读取），这里把它投影成 [`SystemMessage`]。
///
/// 顺序是 `context.messages` 在前、`baseline`（pending）在后，与 TS 的
/// `[...context.messages, ...baseline]` 一致；非 system 消息在这里全部丢弃。
fn collect_transcript_messages(
    context_messages: &[AgentMessage],
    baseline: &[AgentMessage],
) -> Vec<Message> {
    context_messages
        .iter()
        .chain(baseline.iter())
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::System(system)) => {
                Some(Message::System(system.clone()))
            }
            AgentMessage::Custom(object) if message.role() == "system" => {
                Some(Message::System(custom_system_projection(object)))
            }
            _ => None,
        })
        .collect()
}

/// 把 role 为 `"system"` 的自定义消息投影成 [`SystemMessage`]（回放只需要工具字段）。
///
/// 字段类型不符时按缺省处理而不是报错：`content` 非字符串 → 空文本，
/// `sections`/`toolsAdded`/`toolsRemoved` 反序列化失败 → `None`，`timestamp` 非整数 → 0。
/// 这里选择静默降级而不是让整个 run 失败：回放只读取能解析出来的工具字段。
fn custom_system_projection(object: &JsonObject) -> SystemMessage {
    SystemMessage {
        role: SystemRole::System,
        content: match object.get("content") {
            Some(JsonValue::String(content)) => SystemContent::Text(content.clone()),
            _ => SystemContent::Text(String::new()),
        },
        sections: object
            .get("sections")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        tools_added: object
            .get("toolsAdded")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        tools_removed: object
            .get("toolsRemoved")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        timestamp: object
            .get("timestamp")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0),
    }
}

/// `toToolDeclaration(agentTool)`：把 `AgentTool` 投影成模型可见的工具声明。
fn agent_tool_declaration(tool: &AgentTool) -> Tool {
    to_tool_declaration(&Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        constrained_sampling: tool.constrained_sampling.clone(),
    })
}

/// `withToolChanges(message, changes)`：复制 system 消息，用 `changes` 替换工具字段。
///
/// 空列表省略字段（TS `...(toolsAdded.length > 0 ? { toolsAdded } : {})`）。标准消息重建为
/// [`SystemMessage`]；自定义消息按 TS 的解构 + 展开语义处理：先删掉 `toolsAdded`/
/// `toolsRemoved`，非空时再按 `toolsAdded`、`toolsRemoved` 的顺序追加到末尾。
///
/// 调用点只传 system 消息（`declare_tool_changes` 已按 role 过滤）：标准消息重建为
/// [`SystemMessage`]，自定义消息重建为 `Custom` 对象，其他变体原样克隆以避免误改。
fn with_tool_changes(message: &AgentMessage, changes: &ToolStateChanges) -> AgentMessage {
    match message {
        AgentMessage::Standard(Message::System(system)) => {
            let mut system = system.clone();
            system.tools_added = if changes.tools_added.is_empty() {
                None
            } else {
                Some(changes.tools_added.clone())
            };
            system.tools_removed = if changes.tools_removed.is_empty() {
                None
            } else {
                Some(changes.tools_removed.clone())
            };
            AgentMessage::Standard(Message::System(system))
        }
        AgentMessage::Custom(object) => {
            let mut object = object.clone();
            object.shift_remove("toolsAdded");
            object.shift_remove("toolsRemoved");
            if !changes.tools_added.is_empty()
                && let Ok(tools) = serde_json::to_value(&changes.tools_added)
            {
                object.insert("toolsAdded".to_string(), tools);
            }
            if !changes.tools_removed.is_empty()
                && let Ok(tools) = serde_json::to_value(&changes.tools_removed)
            {
                object.insert("toolsRemoved".to_string(), tools);
            }
            AgentMessage::Custom(object)
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// streamAssistantResponse
// ---------------------------------------------------------------------------

/// `streamAssistantResponse`：把 `AgentMessage[]` 转成 `Message[]` 并发起一次带流式事件的
/// assistant 请求。
///
/// 关键点（逐条对应 TS）：
/// - `transformContext` 先于 `convertToLlm`；
/// - `getApiKey(provider) || config.apiKey`（空字符串按 JS 假值处理）；
/// - `identity` 默认值逐字段合并；
/// - `start` 事件把 partial 推入上下文，后续 delta 替换最后一条，`done`/`error` 再替换为
///   最终消息；`message_update` 携带 partial 的浅拷贝；
/// - 最终消息强制写入本次请求的 `thinkingLevel`（`config.reasoning ?? "off"`）。
///
/// 副作用：流式过程中的 partial 与最终消息都写进 `context.messages`（最后留下最终消息），
/// 但**不**写 `new_messages`——调用方负责把返回的消息收进返回值。`transform_context`
/// 拿到的是 `context.messages` 的克隆，`convert_to_llm` 也只看该克隆，因此两者都无法
/// 修改运行中的上下文（TS 的引用语义在这里不成立，是值语义下的既定差异）。
async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<CancellationToken>,
    emit: &AgentEventSink,
    stream_fn: &StreamFn,
    stop_reason_override: Option<crate::budget::BudgetStopReason>,
) -> Result<AssistantMessage, AgentLoopError> {
    // AgentMessage[] → AgentMessage[]（可选的上下文变换）。
    let messages = match config.transform_context.clone() {
        Some(transform) => transform(context.messages.clone(), signal.clone()).await,
        None => context.messages.clone(),
    };

    // AgentMessage[] → Message[]（LLM 兼容消息）。
    let llm_messages = (config.convert_to_llm)(messages);
    // `system_prompt` 与 `tools` 都是 `None`：`normalize_context` 不会前插 system 消息，
    // 等价于把消息列表原样包装成 `TranscriptContext`。
    let llm_context = normalize_context(&Context {
        system_prompt: None,
        messages: llm_messages,
        tools: None,
    });

    // 解析 API key（过期 token 场景很重要）。
    let resolved_api_key = match config.get_api_key.clone() {
        Some(get_api_key) => get_api_key(config.model.provider.clone())
            .await
            .filter(|key| !key.is_empty()),
        None => None,
    }
    .or_else(|| config.options.base.api_key.clone());

    // 对应 TS 的 `{...config, apiKey, identity, signal}`：先整体复制，再覆盖这三项。
    let mut options = clone_stream_options(&config.options);
    options.base.api_key = resolved_api_key;
    options.base.identity = merge_client_identity([
        Some(default_agent_identity()),
        config.options.base.identity.clone(),
    ]);
    options.base.signal = signal.clone();

    let requested_level = config.options.reasoning;
    let response = stream_fn(config.model.clone(), llm_context, Some(options))
        .await
        .map_err(|error| AgentLoopError::Ai(Box::new(error)))?;

    let mut partial_message: Option<AssistantMessage> = None;
    let mut added_partial = false;

    // 消费 provider 事件直到事件通道结束。`recv()` 返回 `None` 只说明流已结束，不代表
    // 一定出现过 `done`/`error`——循环之后还有一次兜底取值。
    while let Some(event) = response.recv().await {
        match &event {
            AssistantMessageEvent::Start { partial } => {
                // `start` 建立草稿：partial 先进入上下文，之后的增量都替换这最后一条。
                let partial = (**partial).clone();
                context
                    .messages
                    .push(AgentMessage::Standard(Message::Assistant(partial.clone())));
                partial_message = Some(partial.clone());
                added_partial = true;
                emit(AgentEvent::MessageStart {
                    message: AgentMessage::Standard(Message::Assistant(partial)),
                })
                .await;
            }
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                // 两种终态走同一分支：不直接使用事件里的 payload，而是向流要最终结果
                // （kiana-ai 在 push 终态事件时已经把结果提交进流）。
                let final_message = apply_stop_override(
                    assistant_result(&response, requested_level).await?,
                    stop_reason_override,
                );
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::Standard(Message::Assistant(final_message.clone()));
                    }
                } else {
                    context
                        .messages
                        .push(AgentMessage::Standard(Message::Assistant(
                            final_message.clone(),
                        )));
                }
                if !added_partial {
                    emit(AgentEvent::MessageStart {
                        message: AgentMessage::Standard(Message::Assistant(final_message.clone())),
                    })
                    .await;
                }
                emit(AgentEvent::MessageEnd {
                    message: AgentMessage::Standard(Message::Assistant(final_message.clone())),
                })
                .await;
                return Ok(final_message);
            }
            _ => {
                // 其余增量事件只在已有草稿时生效；没有 `start` 的 delta 被忽略。
                let Some(partial) = event.partial() else {
                    continue;
                };
                let partial = partial.clone();
                if partial_message.is_some() {
                    partial_message = Some(partial.clone());
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::Standard(Message::Assistant(partial.clone()));
                    }
                    emit(AgentEvent::MessageUpdate {
                        message: AgentMessage::Standard(Message::Assistant(partial)),
                        assistant_message_event: event,
                    })
                    .await;
                }
            }
        }
    }

    // 流结束但没有 done/error：与 TS 一样再取一次结果（若流从未提交结果则永远 pending）。
    let final_message = apply_stop_override(
        assistant_result(&response, requested_level).await?,
        stop_reason_override,
    );
    if added_partial {
        if let Some(last) = context.messages.last_mut() {
            *last = AgentMessage::Standard(Message::Assistant(final_message.clone()));
        }
    } else {
        context
            .messages
            .push(AgentMessage::Standard(Message::Assistant(
                final_message.clone(),
            )));
        emit(AgentEvent::MessageStart {
            message: AgentMessage::Standard(Message::Assistant(final_message.clone())),
        })
        .await;
    }
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::Standard(Message::Assistant(final_message.clone())),
    })
    .await;
    Ok(final_message)
}

/// `Object.assign(await response.result(), { thinkingLevel: config.reasoning ?? "off" })`。
///
/// 已知差异（冻结的 kiana-ai API 决定）：`AssistantMessageEventStream::result()` 返回流内结果的
/// **副本**（`kiana-ai/src/event_stream.rs` 的既定契约），因此这里只能改写本次循环拿到的副本；
/// TS 的 `Object.assign` 会就地改写流里已提交的结果，持有该 assistant 流的调用方在 TS 里能看到
/// 被强制的 `thinkingLevel`，Rust 里看不到。要消除该差异需要 kiana-ai 暴露“改写已提交结果”的 API
/// （不在本 crate 的所有权内）。
///
/// 错误来源：kiana-ai 的 `result()` 只在结果通道关闭（最后一个流句柄释放）时返回
/// `StreamEndedWithoutResult`；`end_without_result()` 路径下的 `result()` 会永远 pending。
/// 调用本函数期间 `response` 引用一直存活。
async fn assistant_result(
    response: &AssistantMessageEventStream,
    requested_level: Option<AiThinkingLevel>,
) -> Result<AssistantMessage, AgentLoopError> {
    let mut message = response.result().await?;
    message.thinking_level = Some(
        requested_level
            .map(ai_level_to_model)
            .unwrap_or(ModelThinkingLevel::Off),
    );
    Ok(message)
}

/// 写入预算 cap；`completed` / `failed` 优先，不会被覆盖。
///
/// 精确的 cap 变体写入 `stop_reason`（`TurnCapped` / `TokenCapped` / `CostCapped` /
/// `TimeCapped`），`raw_stop_reason` 同时保留 camelCase 文本。
fn apply_stop_override(
    mut message: AssistantMessage,
    stop_reason_override: Option<crate::budget::BudgetStopReason>,
) -> AssistantMessage {
    let Some(reason) = stop_reason_override else {
        return message;
    };
    if matches!(
        message.stop_reason,
        StopReason::Error | StopReason::Aborted | StopReason::Pending
    ) {
        return message;
    }
    message.stop_reason = reason.stop_reason();
    message.raw_stop_reason = Some(reason.as_str().to_string());
    message
}

/// `{ ...config, apiKey, identity, signal }` 在 Rust 侧落在 `SimpleStreamOptions` 上。
///
/// `SimpleStreamOptions` 没有实现 `Clone`（冻结类型），这里逐字段复制；回调与句柄都是
/// `Arc`，克隆很便宜。
///
/// 这里用显式结构体字面量而不是 `..Default::default()`：上游给 `StreamOptions` /
/// `SimpleStreamOptions` 新增字段时本函数会编译失败，提醒同步复制，避免静默丢字段。
fn clone_stream_options(options: &SimpleStreamOptions) -> SimpleStreamOptions {
    // 字段清单集中在 `kiana_ai::types::SimpleStreamOptions::duplicate`，避免多处拷贝
    // 漂移（新增字段时只需改那一处）。
    options.duplicate()
}

// ---------------------------------------------------------------------------
// 工具执行
// ---------------------------------------------------------------------------

/// 一个工具批次的执行结果。
struct ExecutedToolCallBatch {
    /// 按源顺序排列的工具结果消息，与 assistant 消息里 toolCall 的顺序一致。
    messages: Vec<ToolResultMessage>,
    /// 本批是否可以终止后续请求（见 `should_terminate_tool_batch`）。
    terminate: bool,
}

/// `FinalizedToolCallOutcome`（= `AgentToolCallOutcome`）。
#[derive(Clone)]
struct FinalizedToolCallOutcome {
    tool_call: AgentToolCall,
    result: AgentToolResult<JsonValue>,
    /// 结果是否按失败呈现：来自工具返回的 `isError`、抛错、校验/拦截/abort，
    /// 或 `afterToolCall` 的覆盖。
    is_error: bool,
    /// 仅当工具真的运行过（含抛错）时存在。
    duration_ms: Option<f64>,
}

/// `PreparedToolCall | ImmediateToolCallOutcome`。
///
/// `Prepared` 还会经过 `execute` 与 `afterToolCall`，因此最终结果带 `durationMs`；
/// `Immediate` 是准备阶段就确定的失败/拦截结果，**不**执行工具、也**没有** `durationMs`。
enum PreparedToolCall {
    Prepared {
        /// TS 的 `toolCall`：assistant 消息里的**原始**调用对象（`prepareArguments` 的就地
        /// 改写已反映在 `arguments` 上）。钩子、事件与最终消息都用它，而不是校验用的参数。
        tool_call: AgentToolCall,
        tool: Box<AgentTool>,
        /// 校验后的参数（`beforeToolCall` 可就地改写，随后原样交给 `execute`）。
        args: ToolArguments,
    },
    Immediate {
        /// assistant 消息里的原始调用（可能已被 `prepareArguments` 就地改写）。
        tool_call: AgentToolCall,
        /// 装箱以避免枚举变体尺寸失衡（立即结果只在错误/拦截路径上产生）。
        result: Box<AgentToolResult<JsonValue>>,
        is_error: bool,
    },
}

impl PreparedToolCall {
    /// 本次准备的原始工具调用（`Prepared` 与 `Immediate` 都携带）。
    fn tool_call(&self) -> &AgentToolCall {
        match self {
            PreparedToolCall::Prepared { tool_call, .. }
            | PreparedToolCall::Immediate { tool_call, .. } => tool_call,
        }
    }
}

/// `ExecutedToolCallOutcome`：`execute()` 的结果加上真实耗时（总是存在）。
struct ExecutedToolCallOutcome {
    result: AgentToolResult<JsonValue>,
    is_error: bool,
    duration_ms: f64,
}

/// `ToolCallHooks`：`Pick<AgentLoopConfig, "beforeToolCall" | "afterToolCall">`。
#[derive(Clone, Default)]
pub struct ToolCallHooks {
    /// `beforeToolCall`：可经 [`ToolArguments`] 改写参数，或拦截本次调用。
    pub before_tool_call: Option<BeforeToolCallFn>,
    /// `afterToolCall`：可覆盖结果的各个字段；返回 `None` 表示沿用执行结果。
    pub after_tool_call: Option<AfterToolCallFn>,
}

impl ToolCallHooks {
    /// 从 [`AgentLoopConfig`] 取出两个工具钩子，其余配置字段忽略。
    fn from_config(config: &AgentLoopConfig) -> Self {
        ToolCallHooks {
            before_tool_call: config.before_tool_call.clone(),
            after_tool_call: config.after_tool_call.clone(),
        }
    }
}

/// 工具流式回报的接收端（TS 的 `ToolUpdateSink`）。
///
/// 回调本身被同步调用；它返回的 future 会被收集，在 `execute()` 返回后按调用顺序
/// `await`（见 `execute_prepared_tool_call`），所以不要假设异步上报在工具结束前跑完。
pub type ToolUpdateSink =
    Arc<dyn Fn(AgentToolResult<JsonValue>) -> BoxFuture<'static, ()> + Send + Sync>;

/// Options for [`run_tool_call`]。
#[derive(Clone)]
pub struct RunToolCallOptions {
    /// 调用解析所依据的工具集。
    pub tools: Vec<AgentTool>,
    /// 作为「发起调用的消息」传给钩子。
    pub assistant_message: AssistantMessage,
    /// 作为「当前 agent 上下文」传给钩子。
    pub context: AgentContext,
    /// 取消令牌：传给钩子与工具 `execute`。已取消时该调用会在准备阶段以
    /// `Operation aborted` 收尾（未知工具、校验失败等更早出现的立即结果优先）；
    /// `beforeToolCall` 返回后也是先查取消，未取消才处理它的 `block`。
    pub signal: Option<CancellationToken>,
    /// 工具运行中的增量回报；`None` 表示调用方不接收更新（这些回报被丢弃）。
    pub on_update: Option<ToolUpdateSink>,
    /// 与 [`AgentLoopConfig::before_tool_call`] 同类型，但由调用方直接提供：
    /// `run_tool_call` 不读 `AgentLoopConfig`。
    pub before_tool_call: Option<BeforeToolCallFn>,
    /// 与 [`AgentLoopConfig::after_tool_call`] 同类型，由调用方直接提供。
    pub after_tool_call: Option<AfterToolCallFn>,
}

/// `runToolCall(toolCall, options)`。
///
/// 与模型发起的调用走同一流程：参数预处理、schema 校验、`beforeToolCall`、执行、
/// `afterToolCall`；不发事件、不加消息。工具失败（未知工具、校验失败、被拦截、抛错）
/// 一律以 `isError: true` 返回，**永不** reject。
///
/// 与循环内的调用相比：不产生 `ToolResultMessage`、不写任何上下文，直接返回
/// [`AgentToolCallOutcome`]。`duration_ms` 只在真正执行过工具时是 `Some`
/// （立即结果没有耗时）。
///
/// `options.assistant_message` 在函数内被克隆：`prepareArguments` 的就地改写只影响这份
/// 内部副本，调用方手里的原值不会被修改。
pub async fn run_tool_call(
    tool_call: AgentToolCall,
    options: RunToolCallOptions,
) -> AgentToolCallOutcome {
    let hooks = ToolCallHooks {
        before_tool_call: options.before_tool_call.clone(),
        after_tool_call: options.after_tool_call.clone(),
    };
    // TS 的 `runToolCall` 拿到的是调用方传入的 `options.assistantMessage` 对象；若该调用就在
    // 消息里，`prepareArguments` 的就地改写对钩子可见。Rust 用一份副本承接同样的效果。
    let mut assistant_message = options.assistant_message.clone();
    let preparation = prepare_tool_call(
        &options.context,
        &mut assistant_message,
        None,
        &tool_call,
        &hooks,
        options.signal.clone(),
        &options.tools,
    )
    .await;
    match preparation {
        PreparedToolCall::Immediate {
            tool_call,
            result,
            is_error,
        } => AgentToolCallOutcome {
            tool_call,
            result: *result,
            is_error,
            duration_ms: None,
        },
        PreparedToolCall::Prepared {
            tool_call: prepared_call,
            tool,
            args,
        } => {
            let on_update = options.on_update.clone().unwrap_or_else(noop_update_sink);
            let prepared = PreparedToolCall::Prepared {
                tool_call: prepared_call,
                tool,
                args,
            };
            let executed =
                execute_prepared_tool_call(&prepared, options.signal.clone(), &on_update).await;
            let finalized = finalize_executed_tool_call(
                &options.context,
                &assistant_message,
                &prepared,
                executed,
                &hooks,
                options.signal.clone(),
            )
            .await;
            AgentToolCallOutcome {
                tool_call: finalized.tool_call,
                result: finalized.result,
                is_error: finalized.is_error,
                duration_ms: finalized.duration_ms,
            }
        }
    }
}

fn noop_update_sink() -> ToolUpdateSink {
    Arc::new(|_partial| Box::pin(async {}))
}

/// `failToolCallsFromTruncatedMessage`：`length` 截断消息里的工具调用全部按失败处理。
///
/// 每个调用仍按正常顺序发 `tool_execution_start` → `tool_execution_end` → 结果消息的
/// `message_start`/`message_end`，但不执行工具、不带 `durationMs`。`terminate` 恒为
/// `false`，循环会继续，让模型重发完整调用（测试
/// `should_not_execute_tool_calls_from_a_length_truncated_assistant_message` 锁定这一点）。
async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[AgentToolCall],
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages = Vec::with_capacity(tool_calls.len());
    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: arguments_to_json(&tool_call.arguments),
        })
        .await;
        let finalized = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            is_error: true,
            duration_ms: None,
        };
        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

/// assistant 消息里所有 `toolCall` 内容块的下标（按源顺序）。
///
/// 用下标而不是克隆出来的调用列表，是为了让 `prepareArguments` 的就地改写写回同一个内容块
/// （TS 的 `toolCalls` 是 `content.filter` 出来的同一批对象引用）。
fn tool_call_content_indices(message: &AssistantMessage) -> Vec<usize> {
    message
        .content
        .iter()
        .enumerate()
        .filter_map(|(index, content)| {
            matches!(content, AssistantContentBlock::ToolCall(_)).then_some(index)
        })
        .collect()
}

/// 读取某个内容块位置的工具调用（前提：下标来自 [`tool_call_content_indices`]）。
fn tool_call_at(message: &AssistantMessage, index: usize) -> AgentToolCall {
    match message.content.get(index) {
        Some(AssistantContentBlock::ToolCall(tool_call)) => tool_call.clone(),
        _ => panic!("tool call index must point at a toolCall content block"),
    }
}

/// `executeToolCalls`：按配置与工具自身的 `executionMode` 选择顺序/并行。
///
/// 顺序的条件：`config.tool_execution == Some(Sequential)`，或任一调用对应的工具在
/// `current_context.tools` 里声明了 `execution_mode: Sequential`。未知工具不会命中这个
/// 查找，最终会由 `prepare_tool_call` 产出一个 “not found” 的立即结果。
async fn execute_tool_calls(
    current_context: &AgentContext,
    assistant_message: &mut AssistantMessage,
    config: &AgentLoopConfig,
    signal: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let tool_call_indices = tool_call_content_indices(assistant_message);
    let has_sequential_tool_call = tool_call_indices.iter().any(|&index| {
        let name = &tool_call_at(assistant_message, index).name;
        current_context
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find(|tool| &tool.name == name))
            .and_then(|tool| tool.execution_mode)
            == Some(ToolExecutionMode::Sequential)
    });
    let hooks = ToolCallHooks::from_config(config);
    if config.tool_execution == Some(ToolExecutionMode::Sequential) || has_sequential_tool_call {
        execute_tool_calls_sequential(
            current_context,
            assistant_message,
            &tool_call_indices,
            &hooks,
            signal,
            emit,
        )
        .await
    } else {
        execute_tool_calls_parallel(
            current_context,
            assistant_message,
            &tool_call_indices,
            &hooks,
            signal,
            emit,
        )
        .await
    }
}

/// `executeToolCallsSequential`：逐个 start → prepare → execute → finalize → end。
///
/// 每个调用结束后立刻发 `tool_execution_end` 与结果消息，因此事件顺序与调用顺序一致。
/// `Immediate` 结果（未知工具、校验失败、被拦截、abort）不执行工具、没有 `durationMs`，
/// 也不会调用 `afterToolCall`。
///
/// 每处理完一个调用检查一次取消：已取消就 `break`，剩余调用既不会产生结果消息，也不会
/// 进入 `terminate` 的判定（`finalized_calls` 只包含已收尾的调用）。
async fn execute_tool_calls_sequential(
    current_context: &AgentContext,
    assistant_message: &mut AssistantMessage,
    tool_call_indices: &[usize],
    hooks: &ToolCallHooks,
    signal: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for &content_index in tool_call_indices {
        // `tool_execution_start` 在准备之前发出：`args` 是尚未被 `prepareArguments` 改写的对象。
        let start_call = tool_call_at(assistant_message, content_index);
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: start_call.id.clone(),
            tool_name: start_call.name.clone(),
            args: arguments_to_json(&start_call.arguments),
        })
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            Some(content_index),
            &start_call,
            hooks,
            signal.clone(),
            current_context.tools.as_deref().unwrap_or(&[]),
        )
        .await;
        let finalized = match preparation {
            PreparedToolCall::Immediate {
                tool_call,
                result,
                is_error,
            } => FinalizedToolCallOutcome {
                tool_call,
                result: *result,
                is_error,
                duration_ms: None,
            },
            prepared @ PreparedToolCall::Prepared { .. } => {
                // TS 的 `emitToolExecutionUpdate(toolCall, emit)` 捕获的是原始调用对象
                // （`prepareArguments` 的就地改写已经反映在它的 `arguments` 上）。
                let on_update = emit_tool_execution_update(prepared.tool_call(), emit);
                let executed =
                    execute_prepared_tool_call(&prepared, signal.clone(), &on_update).await;
                finalize_executed_tool_call(
                    current_context,
                    assistant_message,
                    &prepared,
                    executed,
                    hooks,
                    signal.clone(),
                )
                .await
            }
        };

        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        finalized_calls.push(finalized);
        messages.push(tool_result_message);

        if is_aborted(&signal) {
            break;
        }
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}

/// 并行批次的条目：立即结果或延迟执行的已准备调用。
///
/// `Immediate` 在预检阶段就已经收尾并发出 `tool_execution_end`；`Deferred` 留到并发阶段
/// 才执行。装箱是为了避免枚举变体尺寸失衡（`PreparedToolCall` 明显更大）。
enum ParallelEntry {
    Immediate(Box<FinalizedToolCallOutcome>),
    Deferred(Box<PreparedToolCall>),
}

/// `executeToolCallsParallel`：先顺序预检，再并发执行所有已准备好的调用。
///
/// 三个阶段：
/// 1. 顺序地对每个调用发 `tool_execution_start` 并 `prepare_tool_call`。立即结果在这里就
///    发 `tool_execution_end`（可能与后续调用的 start 交错）；每步之后检查取消，可能 break。
/// 2. 按源顺序把条目变成 future：立即结果直接完成，deferred 条目交给
///    `run_prepared_tool_call`；由 `join_all_ordered` 在同一个任务里并发轮询。
/// 3. 等全部完成后，按**源顺序**构造结果消息并发送；而 `tool_execution_end` 的顺序由各
///    future 的实际完成顺序决定（测试
///    `should_emit_tool_execution_end_in_completion_order_but_persist_tool_results_in_source_order`
///    锁定这一点）。
///
/// 已取消的 token 会让尚未开始的 deferred 调用在 `run_prepared_tool_call` 开头直接产出
/// `Operation aborted`。
async fn execute_tool_calls_parallel(
    current_context: &AgentContext,
    assistant_message: &mut AssistantMessage,
    tool_call_indices: &[usize],
    hooks: &ToolCallHooks,
    signal: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut entries: Vec<ParallelEntry> = Vec::new();

    // 预检阶段：顺序地 start + prepare；立即结果（未知工具、校验失败、被拦截、已 abort）
    // 在这里就发出 tool_execution_end。
    for &content_index in tool_call_indices {
        let start_call = tool_call_at(assistant_message, content_index);
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: start_call.id.clone(),
            tool_name: start_call.name.clone(),
            args: arguments_to_json(&start_call.arguments),
        })
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            Some(content_index),
            &start_call,
            hooks,
            signal.clone(),
            current_context.tools.as_deref().unwrap_or(&[]),
        )
        .await;
        match preparation {
            PreparedToolCall::Immediate {
                tool_call,
                result,
                is_error,
            } => {
                let finalized = FinalizedToolCallOutcome {
                    tool_call,
                    result: *result,
                    is_error,
                    duration_ms: None,
                };
                emit_tool_execution_end(&finalized, emit).await;
                entries.push(ParallelEntry::Immediate(Box::new(finalized)));
                if is_aborted(&signal) {
                    break;
                }
            }
            prepared @ PreparedToolCall::Prepared { .. } => {
                entries.push(ParallelEntry::Deferred(Box::new(prepared)));
                if is_aborted(&signal) {
                    break;
                }
            }
        }
    }

    // `Promise.all(...)`：按源顺序启动，全部并发跑，结果按源顺序收集。
    //
    // 延迟闭包在 TS 里捕获 `assistantMessage` 的**引用**，执行时能看到预检阶段所有
    // `prepareArguments` 的就地改写，因此这里在预检全部结束后再克隆消息。
    let mut futures: Vec<BoxFuture<'static, FinalizedToolCallOutcome>> =
        Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            ParallelEntry::Immediate(finalized) => {
                futures.push(Box::pin(async move { *finalized }));
            }
            ParallelEntry::Deferred(prepared) => {
                let future = run_prepared_tool_call(
                    *prepared,
                    current_context.clone(),
                    assistant_message.clone(),
                    hooks.clone(),
                    signal.clone(),
                    emit.clone(),
                );
                futures.push(Box::pin(future));
            }
        }
    }
    let ordered_finalized_calls = join_all_ordered(futures).await;

    let mut messages: Vec<ToolResultMessage> = Vec::with_capacity(ordered_finalized_calls.len());
    for finalized in &ordered_finalized_calls {
        let tool_result_message = create_tool_result_message(finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&ordered_finalized_calls),
    }
}

/// 并行执行一个已准备好的工具调用（延迟闭包的函数体）。
///
/// TS 闭包捕获的循环变量 `toolCall` 就是 `prepared.toolCall`（原始对象，可能被
/// `prepareArguments` 就地改写）：`tool_execution_update` 的 `args`、abort 结果与
/// `afterToolCall` 的 `toolCall` 都用它，而不是校验用的 `args`。
///
/// 取消检查只在开头做一次：已取消则直接产出 `Operation aborted`（`durationMs` 为
/// `None`），不执行工具、不调用 `afterToolCall`，但仍发 `tool_execution_end`。
async fn run_prepared_tool_call(
    prepared: PreparedToolCall,
    current_context: AgentContext,
    assistant_message: AssistantMessage,
    hooks: ToolCallHooks,
    signal: Option<CancellationToken>,
    emit: AgentEventSink,
) -> FinalizedToolCallOutcome {
    if is_aborted(&signal) {
        let finalized = FinalizedToolCallOutcome {
            tool_call: prepared.tool_call().clone(),
            result: create_error_tool_result("Operation aborted"),
            is_error: true,
            duration_ms: None,
        };
        emit_tool_execution_end(&finalized, &emit).await;
        return finalized;
    }
    let on_update = emit_tool_execution_update(prepared.tool_call(), &emit);
    let executed = execute_prepared_tool_call(&prepared, signal.clone(), &on_update).await;
    let finalized = finalize_executed_tool_call(
        &current_context,
        &assistant_message,
        &prepared,
        executed,
        &hooks,
        signal.clone(),
    )
    .await;
    emit_tool_execution_end(&finalized, &emit).await;
    finalized
}

/// `finalizedCalls.length > 0 && every(f => f.result.terminate === true)`。
///
/// 空批次返回 `false`：没有工具调用的正常 turn 不应被当成“终止”。
fn should_terminate_tool_batch(finalized_calls: &[FinalizedToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}

/// `prepareToolCallArguments`：调用 `prepareArguments` 并区分「返回同一对象」与「返回新值」。
///
/// TS 的闭包收到 `toolCall.arguments` 的同一个对象引用，可以就地改写（edit 工具的
/// `args.edits = parsed`）；返回值与 `toolCall.arguments` 是同一引用时沿用原调用，否则
/// 构造 `{...toolCall, arguments: preparedArguments}`。Rust 用返回 [`Option`] 表达这一判断
/// （`None` = 返回同一对象，就地改写保留）。
///
/// 返回 `(原始调用, 用于校验的参数值)`：原始调用保留就地改写后的参数（TS 的同一个对象）；
/// `Some` 分支的参数值就是 TS 的 `preparedArguments`（可能是非对象，交给校验器报错），
/// `None` 分支则是原始调用里的对象参数。
fn prepare_tool_call_arguments(
    tool: &AgentTool,
    tool_call: &AgentToolCall,
) -> (AgentToolCall, JsonValue) {
    let Some(prepare_arguments) = &tool.prepare_arguments else {
        return (tool_call.clone(), arguments_to_json(&tool_call.arguments));
    };
    let mut arguments = arguments_to_json(&tool_call.arguments);
    let replacement = prepare_arguments(&mut arguments);
    // 闭包可以整体替换 `&mut`（TS 的闭包做不到：它只能改写对象本身）。原始调用的
    // `arguments` 字段是对象类型，装不下非对象值，只能保留原值。
    // - 返回 `Some(value)`：校验/执行使用 `value`（即使不是对象，也交给校验器报错）；
    // - 返回 `None`：按“返回同一对象”处理，校验使用 `original.arguments`——若闭包把
    //   `&mut` 换成了非对象又返回 `None`，那个非对象值会被丢弃。
    let original = match &arguments {
        JsonValue::Object(object) => AgentToolCall {
            arguments: object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            ..tool_call.clone()
        },
        _ => tool_call.clone(),
    };
    match replacement {
        // `preparedArguments === toolCall.arguments`：原调用就是参数对象（就地改写后）。
        None => {
            let prepared = arguments_to_json(&original.arguments);
            (original, prepared)
        }
        Some(value) => (original, value),
    }
}

/// 在 assistant 消息里按 id 找 toolCall 内容块（`runToolCall` 没有内容块下标时的兜底）。
fn find_tool_call_index(message: &AssistantMessage, tool_call_id: &str) -> Option<usize> {
    message.content.iter().position(|content| {
        matches!(content, AssistantContentBlock::ToolCall(tool_call) if tool_call.id == tool_call_id)
    })
}

/// `validateToolArguments(tool, preparedToolCall)`。
///
/// 对象参数直接交给 kiana-ai；非对象参数（TS 的 `{...toolCall, arguments: <非对象>}`）按同一
/// 算法在 JSON 值上校验。工具的 schema 都是对象类型时，产出当前稳定格式的
/// `Validation failed for tool "X": ...\n\nReceived arguments:\n<pretty>`。
///
/// 细节：
/// - 对象路径把参数包成 [`AgentToolCall`]（保留 `arguments_error` 等其余字段）后交给
///   `validate_tool_arguments`，因此“模型参数文本不是合法 JSON 对象”的错误同样优先。
/// - 非对象路径手工重放 `normalizeOptionalNulls → value_convert → coerceWithJsonSchema →
///   checkJsonSchema`：coerce 结果只有在“与输入不同且自身通过校验”时才采用，否则退回
///   原值再 Check；最后按 `  - <path>: <message>` 列表报错。
// `AiError` 的尺寸由 ARCHITECTURE.md §4.1 的公开错误契约固定（与 kiana-ai 的
// `validate_tool_arguments` 相同的局部例外）。
#[allow(clippy::result_large_err)]
fn validate_prepared_tool_arguments(
    tool: &Tool,
    tool_call: &AgentToolCall,
    prepared_arguments: &JsonValue,
) -> Result<JsonValue, AiError> {
    if let JsonValue::Object(arguments) = prepared_arguments {
        let prepared = AgentToolCall {
            arguments: arguments
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            ..tool_call.clone()
        };
        return validate_tool_arguments(tool, &prepared);
    }
    if let Some(arguments_error) = &tool_call.arguments_error {
        return Err(AiError::new(
            AiErrorKind::Request,
            format!(
                "Invalid arguments for tool \"{}\": {arguments_error}",
                tool_call.name
            ),
        ));
    }
    // TS 的 top-level 管线：normalizeOptionalNulls → Value.Convert → coerceWithJsonSchema →
    // Check → 按 `  - <path>: <message>` 列表报错。非对象在 normalize/convert 阶段基本是
    // 空操作，coerce 可能把它转换成合法值（与 TS 一样先于 Check 生效）。
    let mut args = prepared_arguments.clone();
    normalize_optional_nulls(&mut args, &tool.parameters);
    value_convert(&tool.parameters, &mut args);
    let coerced = coerce_with_json_schema(args.clone(), &tool.parameters);
    if !kiana_json::js_object::deep_equal(&coerced, &args)
        && check_json_schema(&tool.parameters, &coerced)
    {
        return Ok(coerced);
    }
    if check_json_schema(&tool.parameters, &args) {
        return Ok(args);
    }
    let errors = collect_json_schema_errors(&tool.parameters, &args)
        .iter()
        .map(|error| format!("  - {}: {}", format_validation_path(error), error.message))
        .collect::<Vec<_>>()
        .join("\n");
    let errors = if errors.is_empty() {
        "Unknown validation error".to_string()
    } else {
        errors
    };
    Err(AiError::new(
        AiErrorKind::Request,
        format!(
            "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
            tool_call.name,
            errors,
            kiana_json::js_object::stringify_pretty(prepared_arguments, 2)
        ),
    ))
}

/// `prepareToolCall`：查工具、预处理参数、写回 assistant 消息、校验、`beforeToolCall`。
///
/// `content_index` 是调用方已知的内容块下标；为 `None` 时按 `tool_call.id` 在
/// `assistant_message` 里查找（`run_tool_call` 的路径）。找不到工具就返回
/// `Tool <name> not found` 的立即结果（`<name>` 处是实际工具名）。
///
/// 顺序：
/// 1. `prepareArguments`（可选）就地改写参数；结果先写回 assistant 消息里的内容块，
///    这样后续校验与钩子看到的是同一份参数。
/// 2. 校验（见 `validate_prepared_tool_arguments`）；失败即立即结果，错误文本是当前稳定契约（改动需同步测试）。
/// 3. `beforeToolCall`。钩子返回后**先**检查取消：已取消即返回 `Operation aborted`，即使
///    钩子同时返回了 `block: true` 也一样（取消的立即结果优先）。未取消才处理钩子的
///    返回值：`block: true` 时用 `reason`（空或缺省则用默认文案）造错误结果，
///    `terminate: true` 会带到结果上。
/// 4. 最后再检查一次取消，然后返回 `Prepared`（工具已被克隆，随结果一起返回）。
///
/// `tools` 是查找与执行使用的工具列表（由调用方决定）；`current_context` 只用于给钩子
/// 构造上下文快照，不会从它取工具。
async fn prepare_tool_call(
    current_context: &AgentContext,
    assistant_message: &mut AssistantMessage,
    content_index: Option<usize>,
    tool_call: &AgentToolCall,
    hooks: &ToolCallHooks,
    signal: Option<CancellationToken>,
    tools: &[AgentTool],
) -> PreparedToolCall {
    let Some(tool) = tools.iter().find(|tool| tool.name == tool_call.name) else {
        return PreparedToolCall::Immediate {
            tool_call: tool_call.clone(),
            result: Box::new(create_error_tool_result(format!(
                "Tool {} not found",
                tool_call.name
            ))),
            is_error: true,
        };
    };
    // 克隆一份：`PreparedToolCall` 需要拥有工具（返回后还要用它执行）。
    let tool = tool.clone();

    let (mutated_call, prepared_arguments) = prepare_tool_call_arguments(&tool, tool_call);
    // TS 的 `prepareArguments` 就地改写 assistant 消息里的同一个 toolCall 对象：先写回消息，
    // 再跑校验与钩子，这样 `beforeToolCall.assistantMessage` 与 `toolCall` 都看到改写结果。
    let write_back_index =
        content_index.or_else(|| find_tool_call_index(assistant_message, &mutated_call.id));
    if let Some(index) = write_back_index
        && let Some(AssistantContentBlock::ToolCall(current)) =
            assistant_message.content.get_mut(index)
    {
        *current = mutated_call.clone();
    }

    let validated_args = match validate_prepared_tool_arguments(
        &agent_tool_declaration(&tool),
        &mutated_call,
        &prepared_arguments,
    ) {
        Ok(args) => args,
        Err(error) => {
            return PreparedToolCall::Immediate {
                tool_call: mutated_call,
                result: Box::new(create_error_tool_result(error.message)),
                is_error: true,
            };
        }
    };

    // TS 把校验后的参数对象原样交给钩子（同一引用）：钩子的就地修改会直接传给
    // `execute()`，不再校验。Rust 用共享句柄承载同一份参数（见 [`ToolArguments`]）。
    let args = ToolArguments::new(validated_args);
    if let Some(before_tool_call) = &hooks.before_tool_call {
        let before_result = before_tool_call(
            BeforeToolCallContext {
                assistant_message: assistant_message.clone(),
                tool_call: mutated_call.clone(),
                args: args.clone(),
                context: current_context.clone(),
            },
            signal.clone(),
        )
        .await;
        if is_aborted(&signal) {
            return PreparedToolCall::Immediate {
                tool_call: mutated_call,
                result: Box::new(create_error_tool_result("Operation aborted")),
                is_error: true,
            };
        }
        if let Some(before_result) = before_result.filter(|result| result.block == Some(true)) {
            let mut result = Box::new(create_error_tool_result(
                before_result
                    .reason
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or_else(|| "Tool execution was blocked".to_string()),
            ));
            if before_result.terminate == Some(true) {
                result.terminate = Some(true);
            }
            return PreparedToolCall::Immediate {
                tool_call: mutated_call,
                result,
                is_error: true,
            };
        }
    }
    if is_aborted(&signal) {
        return PreparedToolCall::Immediate {
            tool_call: mutated_call,
            result: Box::new(create_error_tool_result("Operation aborted")),
            is_error: true,
        };
    }

    PreparedToolCall::Prepared {
        tool_call: mutated_call,
        tool: Box::new(tool),
        args,
    }
}

/// `emitToolExecutionUpdate(toolCall, emit)`：工具运行中的增量回报。
///
/// TS 在工具回调里**同步**调用 `emit(...)`，只把返回的 Promise 收集起来稍后 `await`
/// （`updateEvents.push(Promise.resolve(onUpdate(partial)))`）。因此这里也必须在返回
/// future 之前同步构造事件并调用 sink：事件在工具运行期间送达，而不是等 `execute()`
/// 返回后才补发。
///
/// sink 捕获创建当时的工具 id/name/arguments；此后对原始调用的再改写不会反映到这条
/// sink 发出的 update 事件里。
fn emit_tool_execution_update(tool_call: &AgentToolCall, emit: &AgentEventSink) -> ToolUpdateSink {
    let tool_call = tool_call.clone();
    let emit = emit.clone();
    Arc::new(move |partial_result| {
        emit(AgentEvent::ToolExecutionUpdate {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: arguments_to_json(&tool_call.arguments),
            partial_result: to_json(&partial_result),
        })
    })
}

/// 收集一次工具执行期间 `onUpdate` 产生的 future（见 [`execute_prepared_tool_call`]）。
struct ToolUpdateCollection {
    /// 工具还没 settle：接受新更新。
    accepting: bool,
    /// 已经开始（已登记）但尚未提交 future 的回调数；settle 等它归零。
    in_flight: usize,
    /// 已提交、待按调用顺序 await 的更新事件。
    events: Vec<BoxFuture<'static, ()>>,
}

impl ToolUpdateCollection {
    fn accepting() -> Self {
        Self {
            accepting: true,
            in_flight: 0,
            events: Vec::new(),
        }
    }
}

/// 一次已开始的 `onUpdate` 回调的注销守卫。
///
/// 用 Drop 而不是在提交处手写 `in_flight -= 1`：宿主 sink panic 时计数也必须归零，
/// 否则 settle 的等待循环永远退不出去。
struct ToolUpdateCallGuard {
    shared: Arc<Mutex<ToolUpdateCollection>>,
}

impl Drop for ToolUpdateCallGuard {
    fn drop(&mut self) {
        lock(&self.shared).in_flight -= 1;
    }
}

/// `executePreparedToolCall`：计时并执行工具，收集（并在 settle 后 await）`onUpdate` 回报。
///
/// `durationMs` 只围绕 `execute()` 本身；工具抛错时同样保留（因此 `tool_execution_end`
/// 会带 `durationMs`）。
///
/// 机制：
/// - `onUpdate` 被同步调用，返回的 future 收集进 `Arc<Mutex<Vec<...>>>`；工具 settle 后
///   停止接受新更新，再把已收集的 future 按调用顺序 `await` 完。
/// - 计时在 await 更新之前截断，因此 `durationMs` 不含 `execute()` 返回后 await 已收集
///   更新 future 的耗时；但 `onUpdate` 是在工具的 `execute()` 内部被同步调用的，这段同步
///   开销发生在 `execute().await` 期间，仍会被计入。`durationMs` 也不含
///   `beforeToolCall`/`afterToolCall`。
/// - 工具返回 `Err` 时同样保留耗时，并把错误消息转成 `isError: true` 的结果。
/// - 更新回调可能来自工具内部的其它线程（[`AgentToolUpdateCallback`] 是 `Send + Sync`），
///   所以共享状态用 `Mutex` 保护。
async fn execute_prepared_tool_call(
    prepared: &PreparedToolCall,
    signal: Option<CancellationToken>,
    on_update: &ToolUpdateSink,
) -> ExecutedToolCallOutcome {
    let PreparedToolCall::Prepared {
        tool_call,
        tool,
        args,
    } = prepared
    else {
        // 调用点只在 `Prepared` 分支调用本函数；`Immediate` 在更早的分支就收尾了。
        unreachable!("execute_prepared_tool_call requires a prepared call");
    };

    // 更新回调可能在工具内部的任意线程被调用：收集列表与“是否继续接受”放在**同一把锁**
    // 里。原先是 `AtomicBool` 检查 + 另一个锁 push，两步之间与 settle 时的“置 false +
    // drain”存在窗口：检查通过、drain 完成后才 push 的 future 永远不会被 await。
    //
    // 但 `on_update`（宿主 sink）不能在持锁时调用：sink 可能变慢或重入。改用 `in_flight`
    // 计数：回调在锁外构造事件，提交时如果 settle 还没开始就绝不丢弃；settle 先置
    // `accepting = false`，再等所有已开始的回调提交完，最后取走列表——既不丢「已开始」
    // 的回调，也不会让任意宿主代码在锁内执行。
    let updates = Arc::new(Mutex::new(ToolUpdateCollection::accepting()));
    let started_at = Instant::now();
    let elapsed = || (started_at.elapsed().as_secs_f64() * 1000.0).round();

    let update_callback: AgentToolUpdateCallback = {
        let updates = updates.clone();
        let on_update = on_update.clone();
        Arc::new(move |partial_result| {
            // 阶段一：登记“已开始”；guard 在回调返回（含 panic 展开）时注销。
            {
                let mut state = lock(&updates);
                if !state.accepting {
                    return;
                }
                state.in_flight += 1;
            }
            let _call = ToolUpdateCallGuard {
                shared: Arc::clone(&updates),
            };
            let future = on_update(partial_result);
            // 阶段二：无条件提交（阶段一已经登记成功，settle 会等 `in_flight` 归零）。
            // 这里不能再看 `accepting`：settle 在等待期间已经把新回调拒之门外，如果在
            // 这里也检查，会让「execute 返回前开始、返回后才构造完」的更新被丢掉。
            lock(&updates).events.push(future);
        })
    };

    // 工具拿到的是 `args` 的当前值（`beforeToolCall` 可能已改写），且不再校验。
    let executor = tool.execute.clone();
    let result = executor
        .execute(
            &tool_call.id,
            args.get(),
            signal.as_ref(),
            Some(update_callback),
        )
        .await;
    // 计时只覆盖 `execute()` 的 await，不含钩子与更新回调。
    let duration_ms = elapsed();
    // 工具已 settle：停止接受新回调，并等待所有“已经开始”的回调提交完毕，再取走列表。
    // `execute()` 返回后 `in_flight` 只可能由尚未完成 `on_update` 的线程持有；逐个
    // yield 让它们推进，不忙等。
    let pending_updates: Vec<BoxFuture<'static, ()>> = loop {
        let taken = {
            let mut state = lock(&updates);
            if state.accepting {
                state.accepting = false;
            }
            if state.in_flight == 0 {
                Some(std::mem::take(&mut state.events))
            } else {
                None
            }
        };
        match taken {
            Some(events) => break events,
            None => tokio::task::yield_now().await,
        }
    };
    join_all_ordered(pending_updates).await;

    match result {
        Ok(result) => ExecutedToolCallOutcome {
            is_error: result.is_error == Some(true),
            result,
            duration_ms,
        },
        Err(error) => ExecutedToolCallOutcome {
            result: create_error_tool_result(error.message),
            is_error: true,
            duration_ms,
        },
    }
}

/// `finalizeExecutedToolCall`：`afterToolCall` 的逐字段合并（`??` 语义：`null` 等同缺省）。
///
/// `assistant_message` 在两条路径上来源不同：并行路径是 `prepareArguments` 全部改写完成后
/// `assistant_message.clone()` 的快照（`execute_tool_calls_parallel`），顺序路径则直接传入
/// 当前的 `&mut assistant_message` 引用（`execute_tool_calls_sequential`），不是副本。
/// `args` 在调用钩子时读取当前值。钩子返回 `None` 时原样保留执行结果。
/// 钩子签名不可失败，因此不存在 TS 的 catch-转错误结果路径。
async fn finalize_executed_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &PreparedToolCall,
    executed: ExecutedToolCallOutcome,
    hooks: &ToolCallHooks,
    signal: Option<CancellationToken>,
) -> FinalizedToolCallOutcome {
    let PreparedToolCall::Prepared {
        tool_call, args, ..
    } = prepared
    else {
        unreachable!("finalize_executed_tool_call requires a prepared call");
    };

    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_tool_call) = &hooks.after_tool_call {
        let after_result = after_tool_call(
            AfterToolCallContext {
                assistant_message: assistant_message.clone(),
                tool_call: tool_call.clone(),
                args: args.get(),
                result: result.clone(),
                is_error,
                context: current_context.clone(),
            },
            signal,
        )
        .await;
        if let Some(after_result) = after_result {
            let merged = merge_after_tool_call(result.clone(), is_error, after_result);
            result = merged.0;
            is_error = merged.1;
        }
    }

    FinalizedToolCallOutcome {
        tool_call: tool_call.clone(),
        result,
        is_error,
        duration_ms: Some(executed.duration_ms),
    }
}

/// `afterResult` 与已执行结果的合并（TS 的 `??`：`null`/缺省都表示「沿用以执行结果」）。
///
/// 逐字段规则：
/// - `content`：给出（`Some`）时整体替换；
/// - `details`：`after.details` 非 null 时用它，否则沿用原 `details`（原值为 null 时保持 null）；
/// - `usage` / `terminate`：`Some` 时替换；
/// - `isError`：`after.is_error` 优先，否则沿用；
/// - `structuredContent`：显式给出（非 null）时用它；否则若这次替换了 `content` 就丢弃旧值
///   （内容已变、结构不再匹配）；否则沿用原值（原值为 null 时保持缺省）。
fn merge_after_tool_call(
    mut result: AgentToolResult<JsonValue>,
    is_error: bool,
    after: AfterToolCallResult,
) -> (AgentToolResult<JsonValue>, bool) {
    // 结构与内容不再匹配时丢弃 structuredContent。
    let structured_content = normalize_null(after.structured_content).or_else(|| {
        if after.content.is_some() {
            None
        } else {
            normalize_null(result.structured_content.clone())
        }
    });
    if let Some(content) = after.content {
        result.content = content;
    }
    let details = normalize_null(after.details)
        .or_else(|| (!result.details.is_null()).then(|| result.details.clone()));
    result.details = details.unwrap_or(JsonValue::Null);
    if after.usage.is_some() {
        result.usage = after.usage;
    }
    if after.terminate.is_some() {
        result.terminate = after.terminate;
    }
    result.structured_content = structured_content;
    let is_error = after.is_error.unwrap_or(is_error);
    (result, is_error)
}

/// `??` 语义：JSON `null` 视为未提供。
fn normalize_null(value: Option<JsonValue>) -> Option<JsonValue> {
    value.filter(|value| !value.is_null())
}

/// `createErrorToolResult(message)`：`{ content: [{type:"text",text}], details: {} }`。
///
/// `details` 固定为空对象（不是 null），正常情况下会原样进入工具结果消息。只有
/// `execute()` 返回 `Err` 的路径会经 `finalize_executed_tool_call` 收尾并调用
/// `afterToolCall`，它的 `details` 可被该钩子覆盖；未知工具、校验失败、`beforeToolCall`
/// 拦截、abort 与 length 截断产生的立即结果都在更早的分支直接收尾，不调用
/// `afterToolCall`。`isError` 不在这里设置，而由 `FinalizedToolCallOutcome.is_error` 携带。
fn create_error_tool_result(message: impl Into<String>) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: message.into(),
            text_signature: None,
        })],
        details: JsonValue::Object(Default::default()),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// `AgentToolCall.arguments`（`IndexMap<String, JsonValue>`）→ `JsonValue`（事件字段）。
///
/// 键序与插入序一致（事件里的参数对象保持解析时的插入顺序）；序列化失败时退回
/// `null` 只是防御性兜底。
fn arguments_to_json(arguments: &crate::types::JsonObject) -> JsonValue {
    serde_json::to_value(arguments).unwrap_or(JsonValue::Null)
}

/// `emitToolExecutionEnd(finalized)`。
///
/// `duration_ms` 为 `None` 时事件序列化会省略该字段（`AgentEvent` 上的
/// `skip_serializing_if`），与 TS 的“没跑过就不带 `durationMs`”一致。
async fn emit_tool_execution_end(finalized: &FinalizedToolCallOutcome, emit: &AgentEventSink) {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: to_json(&finalized.result),
        is_error: finalized.is_error,
        duration_ms: finalized.duration_ms,
    })
    .await;
}

/// `createToolResultMessage(finalized)`。
///
/// `timestamp` 取当前时间；`duration_ms` 只来自真正执行过的调用。`structuredContent`
/// 不在消息结构里（它只随事件与调用结果流转），因此这里没有对应字段。
fn create_tool_result_message(finalized: &FinalizedToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        // TS 的 `details: undefined` 被 `JSON.stringify` 省略；Rust 用 `null` 表示缺省。
        details: if finalized.result.details.is_null() {
            None
        } else {
            Some(finalized.result.details.clone())
        },
        usage: finalized.result.usage.clone(),
        nested_calls: None,
        is_error: finalized.is_error,
        timestamp: now_unix_ms(),
        duration_ms: finalized.duration_ms,
    }
}

/// `emitToolResultMessage(message)`：`message_start` + `message_end`。
async fn emit_tool_result_message(message: &ToolResultMessage, emit: &AgentEventSink) {
    emit(AgentEvent::MessageStart {
        message: AgentMessage::Standard(Message::ToolResult(message.clone())),
    })
    .await;
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::Standard(Message::ToolResult(message.clone())),
    })
    .await;
}

/// 是否已取消：`None` 视为未取消；`Some` 时读取共享令牌（可能被其它任务取消）。
fn is_aborted(signal: &Option<CancellationToken>) -> bool {
    signal.as_ref().is_some_and(CancellationToken::is_cancelled)
}

/// 把工具结果写成事件字段用的 JSON（与 TS 事件里传对象等价）。
fn to_json<T: serde::Serialize>(value: &T) -> JsonValue {
    serde_json::to_value(value).unwrap_or(JsonValue::Null)
}

/// `Promise.all` 语义的并发等待：按源顺序轮询所有 future，全部完成时按源顺序返回。
///
/// 不引入额外依赖（`futures` 只是 dev-dependency）。
///
/// 所有 future 在**同一个任务**里协作式轮询：每次唤醒都按索引顺序 poll 尚未完成的
/// future。它们可以并发等待 I/O，但不会占用多个线程；某个 future 内部同步阻塞会拖住
/// 整批。返回顺序固定为输入顺序，与完成顺序无关。
///
/// 任一 future panic 会沿 `poll_fn` 直接展开，剩余 future 被丢弃（没有 catch）。
async fn join_all_ordered<T>(futures: Vec<BoxFuture<'static, T>>) -> Vec<T> {
    let mut futures = futures;
    let mut results: Vec<Option<T>> = (0..futures.len()).map(|_| None).collect();
    let mut remaining = futures.len();
    if remaining == 0 {
        return Vec::new();
    }
    std::future::poll_fn(|context| {
        for (index, future) in futures.iter_mut().enumerate() {
            // 已完成的 future 不能再 poll（poll 一个返回过 Ready 的 future 会 panic）。
            if results[index].is_some() {
                continue;
            }
            if let std::task::Poll::Ready(value) = future.as_mut().poll(context) {
                results[index] = Some(value);
                remaining -= 1;
            }
        }
        if remaining == 0 {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    // `poll_fn` 只在 `remaining == 0` 时返回 `Ready(())`，所以每个槽位都必然被填过。
    results
        .into_iter()
        .map(|result| result.expect("every future completed before poll_fn returned Ready"))
        .collect()
}

/// 当前 Unix 毫秒时间戳；系统时间早于 epoch 时返回 0。
fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    /// 回归：`recv()` 在收到事件之后、被 poll 之前取消时，事件必须回到队列。
    #[tokio::test]
    async fn cancelling_a_woken_recv_requeues_its_event() {
        let stream = AgentEventStream::new();
        let mut first = Box::pin(stream.recv());
        assert!(futures::poll!(first.as_mut()).is_pending());
        stream.push(AgentEvent::AgentStart);
        drop(first);
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.recv())
            .await
            .expect("the event must be requeued")
            .expect("some event");
        assert!(matches!(event, AgentEvent::AgentStart));
    }

    /// 回归：取消两个等待者中的第一个后，它的事件要立即转交给第二个，而不是等下一次
    /// `push`（否则第二个消费者会先收到更晚的事件）。
    #[tokio::test]
    async fn cancelling_the_first_of_two_waiters_delivers_its_event_to_the_second() {
        let stream = AgentEventStream::new();
        let mut first = Box::pin(stream.recv());
        assert!(futures::poll!(first.as_mut()).is_pending());
        let second = {
            let stream = stream.clone();
            tokio::spawn(async move { stream.recv().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        stream.push(AgentEvent::AgentStart);
        drop(first);
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .expect("the second waiter must be woken")
            .expect("survivor task")
            .expect("some event");
        assert!(matches!(event, AgentEvent::AgentStart));
    }
}
