//! ``api/lazy.ts``：``LazyStreamObserver`` / ``lazyStream`` / ``lazyApi``。
//!
//! TS: `packages/ai/src/api/lazy.ts`（逐条对应 `spec/ai-api-other.md` §2.1）。
//!
//! `Models.stream()` / `streamSimple()` / `streamDeferred()` 的「同步返回、异步 setup」
//! 语义依赖 [`lazy_stream`]；[`lazy_api`] 把「首次调用时才加载 API 实现模块」包装成
//! [`ProviderStreams`]。
//!
//! 语义（逐条对应 TS）：
//! 1. 同步返回 outer 流；`setup()` 先同步 poll 一次（TS 直接同步调用），其第一个 `await`
//!    之前的同步前缀在返回前执行；之后的转发在后台任务里跑。
//! 2. 转发内层事件：先 `push` 到 outer，再 `observer.onEvent`。
//! 3. 内层结束后读取内层 `result()`（覆盖事件里见过的 done/error 结果），`outer.end(result)`，
//!    再 `observer.onResult(result)`（只调用一次）。
//! 4. setup 失败：构造 `stopReason: "error"` 的空消息（`timestamp = 开始时间`），push `error`
//!    事件、通知 observer、`outer.end(message)`、`observeResult(message)`。
//!
//! ## Rust 里的「动态 import」
//!
//! Rust 是静态链接，`lazyApi(() => import("./x.ts"))` 的等价物是调用方传入一个返回
//! `ProviderStreams` 的**异步加载器**：[`LazyApiLoad`]。模块缓存（Node 的 import cache）
//! 由加载器的实现负责（通常用 `OnceLock`）；`lazy_api` 每次调用 `load()` 都会执行加载器，
//! 与 TS 每次 `await load()` 一致。
//!
//! 15 个 `*.lazy.ts` 工厂（`anthropicMessagesApi()` 等）的归属见 `ARCHITECTURE.md` §5：
//! 它们与各自的适配器文件一起交付，本文件只提供通用包装。
//!
//! # 这个模块在系统中的位置
//!
//! 主要调用方：
//!
//! - `models::ModelsImpl::stream` / `stream_simple` 把「查 provider、解析凭证」放进
//!   [`LazySetup`]，并传入 telemetry observer（`models` 模块是本文件最重要的使用方）。
//! - 使用惰性入口的适配器工厂（`anthropic_messages`、`openai_responses`、`openai_completions`、
//!   `azure_openai_responses`、`openai_codex_responses` 等）用 [`lazy_api`] 包装「首次调用时
//!   才加载实现」的入口；其余工厂（`mistral_conversations`、`kiana_messages`、
//!   `bedrock_converse_stream`、`google_generative_ai` 等）直接构造 `ProviderStreams`。
//! - 需要把模块级同步 `Err` 变成流上 `error` 事件的适配器用
//!   [`lazy_stream_error`]（`compat`、`bedrock_provider` 等）。
//!
//! # 执行流程（一次 [`lazy_stream`]）
//!
//! 1. 记录开始时间（失败消息的 `timestamp` 来源），创建 `outer` 流。
//! 2. 在 tokio 上下文里同步 poll `setup` 一次；没有 ambient runtime 时先创建一个
//!    current-thread runtime（`enable_all`）并 enter，再 poll。这是为了复刻 TS 里
//!    `setup()` 同步调用时「第一个 `await` 之前的同步前缀立即执行」的语义。
//! 3. 立即返回 `outer`。setup 的剩余部分与事件转发在后台执行：tokio 内用
//!    `Handle::spawn`，tokio 外用专用线程 + `block_on`。
//! 4. 后台任务原样转发内层事件（先 `push` 到 `outer`，再 `observer.onEvent`）；内层结束后
//!    读取 `source.result()`，调用 `outer.end` 与 `observer.onResult`。setup 失败或 observer
//!    回调 panic 时，改走「合成错误消息」分支。
//!
//! # 线程与回调约束
//!
//! - observer 的两个回调在**同一个后台执行体**里按事件顺序同步调用；调用期间持有内部
//!   `Mutex`，因此同一 observer 的回调不会并发执行。回调应快速返回，不能长时间占用
//!   tokio worker。
//! - 转发路径上的回调 panic 被 `catch_unwind` 捕获并转成错误流；例外是
//!   `finish_with_error` 里对合成 `error` 事件的直接 `onEvent` 调用（没有捕获，见该函数
//!   说明）。setup future 自身的 panic 也不在捕获范围内（首次 poll 时传播给
//!   [`lazy_stream`] 的调用方，后台 poll 时终止后台任务）。
//! - 没有 tokio runtime 的同步调用方会走「每个流一个线程 + 一个 current-thread runtime」
//!   的兜底路径。它不是零成本路径，但保证同步 API 也能立即拿到流。
//!
//! # 失败模式与注意事项
//!
//! - setup 返回 `Err` 或 observer 回调 panic 时，会合成一条 `stopReason: "error"` 的
//!   assistant 消息。流尚未结束时，它作为 `error` 事件 push 并 `end`，消费者能收到；流已
//!   结束（终态事件已 push，或已调用 `end`）时，`push` 被静默忽略，合成事件只由 observer
//!   的 `onEvent` 直接收到（`onResult` 若未触发过也会收到该消息），消费者已提交的结果不会
//!   被覆盖。`outer.result()` 不会以 `Err` 结束（最坏情况是保持 pending）。
//! - 内层流已经正常结束后再发生的 `onResult` panic 不会覆盖已提交的结果（结果只提交
//!   一次），但 observer 仍会直接收到补发的 `error` 事件。
//! - 丢弃返回的流**不会**取消后台转发：任务自己持有 `outer` 的克隆，直到内层流结束。
//! - 极端情况下（没有 ambient runtime 且新建 runtime 失败），后台转发不会启动，流不会
//!   结束；这是资源耗尽级别的失败，正常路径不可达。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use async_trait::async_trait;

use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::models::ProviderStreams;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, DeferredCancelOptions,
    DeferredFetchOptions, DeferredHandle, ErrorReason, Model, SimpleStreamOptions, StopReason,
    StreamOptions, TranscriptContext, Usage, UsageCost,
};

/// `onEvent` 回调：每个事件 push 到 `outer` 之后同步调用。
///
/// 在转发任务里执行，返回值被忽略。正常转发路径上的 panic 会被捕获并转成错误事件；但
/// `finish_with_error` 为合成 `error` 事件直接调用本回调时没有 `catch_unwind` 保护。
pub type LazyEventCallback = Box<dyn Fn(&AssistantMessageEvent) + Send + Sync>;
/// `onResult` 回调：终态结果只报告一次。
///
/// 参数为 `None` 表示内层流结束时没有结果（TS 的 `undefined`）；回调 panic 与
/// [`LazyEventCallback`] 相同，进入错误分支。
pub type LazyResultCallback = Box<dyn Fn(Option<&AssistantMessage>) + Send + Sync>;

/// 同步、只读的流观察者。
///
/// 两个字段都是可选的；observer 随 [`lazy_stream`] 移入后台任务，调用方之后不再访问它。
/// `onEvent` 按事件顺序先于终止事件的 `onResult` 执行。
///
/// TS: `src/api/lazy.ts#LazyStreamObserver`
#[derive(Default)]
pub struct LazyStreamObserver {
    /// 每个事件 push 之后同步调用。
    pub on_event: Option<LazyEventCallback>,
    /// 终态结果只报告一次。
    pub on_result: Option<LazyResultCallback>,
}

// 手写 `Debug`：两个回调没有 `Debug` 实现，只报告字段是否存在，避免日志里出现闭包内容。
impl std::fmt::Debug for LazyStreamObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyStreamObserver")
            .field("has_on_event", &self.on_event.is_some())
            .field("has_on_result", &self.on_result.is_some())
            .finish()
    }
}

/// `setup()` 的返回：成功时得到内层流，失败时得到 error 消息的来源错误。
pub type LazySetup =
    Pin<Box<dyn Future<Output = Result<AssistantMessageEventStream, AiError>> + Send + 'static>>;

/// `createSetupErrorMessage(model, error, timestamp)`。
///
/// 构造 setup 失败时的合成 assistant 消息：内容为空、`stopReason = Error`、`usage` 全 0；
/// `errorMessage` 只取 `error.message`（`kind`/`name` 不进入消息）。`timestamp` 由调用方
/// 传入：[`lazy_stream`] 传本次请求的开始时间（对应 TS 的 `startedAt`），`stream_result`
/// 兜底传当前时间。
pub fn create_setup_error_message(
    model: &Model,
    error: &AiError,
    timestamp: i64,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Error,
        error_message: Some(error.message.clone()),
        timestamp,
        ..AssistantMessage::default()
    }
}

/// 在后台执行 setup 并转发。
///
/// 这是适配器启动「生产者循环」的统一入口：有 tokio runtime 时 `spawn` 到当前 runtime，
/// 否则起一个专用线程并用 current-thread runtime 驱动。future 必须是 `Send + 'static`。
/// 没有错误返回通道：future 内部负责把错误写进事件流。
pub(crate) fn spawn_forward<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    spawn_forward_with_runtime(future, None);
}

/// 同 [`spawn_forward`]，但可沿用调用方提前创建并 enter 过的 runtime。
///
/// `runtime` 只在当前线程没有 tokio 上下文时使用：由 [`lazy_stream`] 为「没有 ambient
/// runtime」的调用方提前创建，保证 setup 同步前缀里注册的 IO/定时器与后续 `block_on` 用
/// 的是同一个 runtime。若当前线程已有 ambient runtime，则直接 `spawn`，不会使用传入的
/// `runtime`。
pub(crate) fn spawn_forward_with_runtime<F>(future: F, runtime: Option<tokio::runtime::Runtime>)
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
        return;
    }
    // TS 的 `setup().then(...)` 不要求调用方处于事件循环；Rust 在没有 runtime 时用
    // 专用线程 + current-thread runtime 跑完转发（同步返回语义不变）。
    //
    // `runtime` 是 [`lazy_stream`] 为了同步执行 setup 前缀而提前创建、并已用于 enter 的
    // runtime（若有）；沿用它可以保证同步前缀里注册的定时器/IO 仍被驱动。
    std::thread::spawn(move || match runtime {
        Some(runtime) => {
            runtime.block_on(future);
        }
        None => {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                runtime.block_on(future);
            }
            // 构建 runtime 失败（资源耗尽级别）：future 在这里被丢弃，调用方的事件流
            // 不会结束。本函数没有错误返回通道，无法把该失败上报给流。
        }
    });
}

/// 同步 poll `setup` 一次（TS 在 [`lazy_stream`] 里直接调用 `setup()`）。
///
/// 用 `Waker::noop()` 构造一次性 context，目的是让 setup 在第一个 `await` 之前的同步前缀
/// 在 [`lazy_stream`] 返回前执行。`Poll::Ready` 表示 setup 已完成；`Poll::Pending` 表示
/// 前缀已执行、future 仍需继续 poll，之后由后台任务 `await` 同一份 future（正常的异步
/// 原语在每次 poll 时会重新注册唤醒源，因此这次 noop waker 不构成唤醒丢失）。
///
/// 是否具备 runtime 上下文由 [`lazy_stream`] 决定；本函数自身不做检查，也不创建 runtime。
fn poll_setup_once(setup: &mut LazySetup) -> Poll<Result<AssistantMessageEventStream, AiError>> {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    setup.as_mut().poll(&mut context)
}

/// `lazyStream` 的 `.catch` 分支：push error 事件、通知 observer、`outer.end(message)`、
/// `observeResult(message)`。
///
/// TS 里 setup 失败与 `forwardStream` 拒绝（内层迭代抛错 / observer 回调抛错）共用这段代码；
/// 两者都得到 `createSetupErrorMessage(model, error, startedAt)`。
///
/// 执行顺序固定：构造消息 → `outer.push(error)` → `observer.onEvent` → `outer.end(message)`
/// → `observeResult(message)`。`outer` 已结束时 `push`/`end` 都是 no-op（结果只提交一次），
/// 因此本分支不会覆盖已经正常结束的流，但 observer 仍会直接收到这条 `error` 事件。
/// [`observe_result`] 的 `observed_result` 守卫保证 `onResult` 不会因此被第二次调用。
///
/// # 例外：observer 在这里 panic
///
/// 这段代码是 TS catch 分支的直译，`onEvent` 是**直接调用**（没有 `catch_unwind`）：若它
/// panic，后台任务终止，后面的 `outer.end` / `observeResult` 不再执行。由于
/// `outer.push(error)` 已先提交错误结果（或流此前已正常结束），调用方仍能拿到结果，但
/// `onResult` 可能不会收到通知。
fn finish_with_error(
    outer: &AssistantMessageEventStream,
    model: &Model,
    started_at: i64,
    observer: &Mutex<Option<LazyStreamObserver>>,
    observed_result: &AtomicBool,
    error: &AiError,
) {
    let message = create_setup_error_message(model, error, started_at);
    let event = AssistantMessageEvent::Error {
        reason: ErrorReason::Error,
        error: Box::new(message.clone()),
    };
    // `outer` 可能已经结束（`onResult` 抛错发生在正常 `end()` 之后）：TS 的 `push` 在
    // `done` 之后静默忽略，事件只经由下面的 `observer.onEvent` 直接送达。
    let _ = outer.push(event.clone());
    {
        let observer = observer.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(on_event) = observer.as_ref().and_then(|o| o.on_event.as_ref()) {
            on_event(&event);
        }
    }
    // TS `end(result)` 的 `resolveFinalResult` 对已 resolve 的 Promise 是 no-op：正常路径
    // 已提交的结果不会被这里的合成错误消息覆盖。
    outer.end(message.clone());
    // TS 的 `observeResult` 用 `observedResult` 保证 `onResult` 只调用一次；抛错发生在
    // 这里时是 unhandled rejection，不影响已结束的流。
    let _ = observe_result(observer, observed_result, Some(&message));
}

/// setup 结束后的收尾：成功则转发内层事件，失败则按 TS 的 catch 分支结束 `outer`。
///
/// 成功分支里的 [`forward_stream`] 只在 observer 回调 panic 时返回 `Err`，此时也走与
/// setup 失败相同的合成错误消息（TS 里两者共用 `lazyStream` 的 `.catch`）。
async fn complete_lazy_stream(
    outer: AssistantMessageEventStream,
    model: Model,
    started_at: i64,
    observer: Mutex<Option<LazyStreamObserver>>,
    observed_result: &AtomicBool,
    result: Result<AssistantMessageEventStream, AiError>,
) {
    match result {
        Ok(inner) => {
            if let Err(message) = forward_stream(&outer, inner, &observer, observed_result).await {
                // observer 回调 panic（TS 的「回调抛错」）→ 与 setup 失败共用 catch 分支。
                finish_with_error(
                    &outer,
                    &model,
                    started_at,
                    &observer,
                    observed_result,
                    &AiError::new(AiErrorKind::Other, message),
                );
            }
        }
        Err(error) => {
            finish_with_error(
                &outer,
                &model,
                started_at,
                &observer,
                observed_result,
                &error,
            );
        }
    }
}

/// Returns a stream synchronously while running async setup behind it.
///
/// TS 的 `setup()` 是**同步调用**的：其第一个 `await` 之前的同步前缀在 `lazyStream` 返回前
/// 执行。Rust 里 `setup` 是 future，因此这里先同步 poll 一次再交给后台任务；若首次 poll
/// 就完成，转发仍和 TS 的 `.then` 一样异步发生。
///
/// # 参数与返回
///
/// - `model`：只用于失败时填充合成消息的 `api`/`provider`/`model` 字段；会克隆进后台任务。
/// - `setup`：异步产出内层流，`Send + 'static`；函数返回前最多 poll 一次（见文件级说明）。
/// - `observer`：可选，移入后台任务后调用方不再持有。
///
/// 返回的 `outer` 立即可读，最终结果通过 `outer.result()` 获取；函数本身不返回错误。
///
/// # 注意事项
///
/// - 在 tokio runtime 内调用时，同步前缀在该 runtime 的上下文中执行；否则临时创建一个
///   current-thread runtime（`enable_all`）。后者是同步调用方的兜底路径。
/// - `setup` future 自身的 panic 不在捕获范围内：首次 poll 时 panic 会传播给调用方，
///   后台 poll 时 panic 会终止后台任务、`outer` 不会结束。可预期的失败应返回 `AiError`。
/// - 丢弃 `outer` 不会取消后台转发；本函数没有取消参数，需要中止时应通过内层流的
///   取消机制（例如请求选项里的 `signal`）。
///
/// TS: `src/api/lazy.ts#lazyStream`
pub fn lazy_stream(
    model: &Model,
    mut setup: LazySetup,
    observer: Option<LazyStreamObserver>,
) -> AssistantMessageEventStream {
    let started_at = now_ms();
    let outer = AssistantMessageEventStream::new();
    let outer_for_task = outer.clone();
    let model = model.clone();
    let observer = Mutex::new(observer);
    // TS `observedResult`：`observeResult` 至多真正调用一次 `onResult`。
    let observed_result = AtomicBool::new(false);

    // 沿用调用方的 tokio runtime；没有 runtime 时按 spawn_forward 的线程路径提前创建，
    // 以便同步 poll 里的定时器/IO 注册到同一个 runtime。
    let (runtime, first_poll) = match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let _entered = handle.enter();
            (None, Some(poll_setup_once(&mut setup)))
        }
        Err(_) => match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => {
                let first_poll = {
                    let _entered = runtime.enter();
                    poll_setup_once(&mut setup)
                };
                (Some(runtime), Some(first_poll))
            }
            Err(_) => (None, None),
        },
    };

    spawn_forward_with_runtime(
        async move {
            let result = match first_poll {
                Some(Poll::Ready(result)) => result,
                // 首次 poll 已返回 Ready 时 future 已完成，其余情况继续 await 同一份 future。
                Some(Poll::Pending) | None => setup.await,
            };
            complete_lazy_stream(
                outer_for_task,
                model,
                started_at,
                observer,
                &observed_result,
                result,
            )
            .await;
        },
        runtime,
    );
    outer
}

/// 构造一个立即失败（在 setup 中抛错）的惰性流。
///
/// 供 `createProvider` 的「没有 API 实现」错误与 deferred 能力错误使用；适配器的
/// [`ProviderStreams`] 实现也用它把模块级入口同步返回的 `Err`（例如缺少 API key）映射成
/// 流上的 `error` 事件，使冻结的 trait 签名保持「同步返回流」。
///
/// 错误事件由后台任务异步产生（与 TS 的 `.catch` 一致），所以调用方应先消费流、再等
/// `result()`。合成消息只保留 `error.message`，`error.kind`/`name` 不会出现在事件里。
pub fn lazy_stream_error(model: &Model, error: AiError) -> AssistantMessageEventStream {
    lazy_stream(model, Box::pin(async move { Err(error) }), None)
}

/// `forwardStream(target, source, observer, observeResult)`。
///
/// 失败路径：TS 的 `forwardStream` Promise 在内层迭代抛错或 observer 回调（`onEvent` /
/// `onResult`）抛错时拒绝，由 [`lazy_stream`] 的 `.catch` 分支收尾。Rust 的内层 `recv()`
/// 不返回错误（`EventStream` 的异步迭代器同样只会结束或等待），所以唯一可观察的抛错来源是
/// observer 回调 panic；这里用 `catch_unwind` 把它映射到同一条 `Err` 路径。返回 `Err` 时
/// **不**调用 `target.end()`，由调用方结束 `outer`：
/// `target.end` 只在正常路径执行；`onResult` 的抛错发生在它之后，此时流已带正常结果结束。
///
/// # 执行阶段
///
/// 1. 循环 `recv()`：先 `target.push(event.clone())`，再通知 observer；同时把 `done` /
///    `error` 事件里的消息记为候选结果。
/// 2. 内层迭代结束后等待 `source.result()`，它覆盖候选结果（TS 的
///    `if (hasResult(source)) result = await source.result()`）。只要内层流在结束前 push 过
///    终态事件（`Done`/`Error`）或调用过 `end(message)`，`result()` 就已就绪；本 crate 的
///    适配器遵循这一约定，因此这里不会永久等待。自定义实现若只调用
///    `end_without_result()` 且没有终态事件，需要自行避免这条路径。
/// 3. 有结果：`target.end(message)`，再 `observeResult(Some(message))`；无结果：
///    `target.end_without_result()`，再 `observeResult(None)`。
///
/// `target.push` 的返回值被忽略：TS 的 `push` 在流已结束后静默丢弃，Rust 的
/// `AssistantMessageEventStream::push` 同样返回 `Ok(())`。
async fn forward_stream(
    target: &AssistantMessageEventStream,
    source: AssistantMessageEventStream,
    observer: &Mutex<Option<LazyStreamObserver>>,
    observed_result: &AtomicBool,
) -> Result<(), String> {
    let mut result: Option<AssistantMessage> = None;
    while let Some(event) = source.recv().await {
        // TS: `target.push(event); observer?.onEvent?.(event);`（push 在前）。
        let _ = target.push(event.clone());
        // TS 里 `onEvent` 抛错会让整个 `forwardStream` 拒绝；Rust 回调没有 Result，
        // panic 是它的等价物，消息按 `error.message` 取 panic payload。
        notify_event(observer, &event)?;
        match &event {
            AssistantMessageEvent::Done { message, .. } => result = Some((**message).clone()),
            AssistantMessageEvent::Error { error, .. } => result = Some((**error).clone()),
            _ => {}
        }
    }
    // TS: `if (hasResult(source)) result = await source.result();` —— 事件结果被内层
    // `result()` 覆盖。Rust 的内层流总是有 `result()`；没有结果时保留事件里的值。
    // 本 crate 的适配器会在结束前 push 终态事件（`Done`/`Error`）或调用 `end(message)`，
    // 这会让 `result()` 就绪，所以这里的等待不会无限挂起；自定义实现若只调用
    // `end_without_result()` 且从不 push 终态事件，需要自行避免这条路径。
    if let Ok(message) = source.result().await {
        result = Some(message);
    }
    match result {
        Some(message) => {
            // TS: `target.end(result); observeResult(result);` —— `onResult` 在 `end()` 之后
            // 调用，它抛错时 `target` 已正常结束，由 `lazyStream` 的 catch 分支补错误事件。
            target.end(message.clone());
            observe_result(observer, observed_result, Some(&message))?;
        }
        None => {
            // TS: `target.end(result); observeResult(result);` —— 无结果时也报告一次 undefined。
            target.end_without_result();
            observe_result(observer, observed_result, None)?;
        }
    }
    Ok(())
}

/// `observer?.onEvent?.(event)`：panic（TS 的「回调抛错」）转成错误文本。
///
/// 持锁调用回调，保证同一 observer 的回调串行；`catch_unwind` 在临界区内捕获 panic，因此
/// 回调 panic 不会让转发任务崩溃，也不会把锁标记为中毒（panic 没有越过临界区）。锁若因
/// 其它原因中毒，所有访问点都用 `into_inner` 恢复，不 panic。
fn notify_event(
    observer: &Mutex<Option<LazyStreamObserver>>,
    event: &AssistantMessageEvent,
) -> Result<(), String> {
    let observer = observer.lock().unwrap_or_else(|e| e.into_inner());
    let Some(on_event) = observer.as_ref().and_then(|o| o.on_event.as_ref()) else {
        return Ok(());
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_event(event)))
        .map_err(panic_payload_message)
}

/// panic payload 的消息文本（TS: `error instanceof Error ? error.message : String(error)`）。
///
/// `panic!("…")` 的 payload 是 `&str`，`panic!("{}", x)`/`anyhow` 风格是 `String`；其它
/// payload 没有可用的消息，用固定文本记录（正常 panic 不会走到这里）。
fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "Unknown error".to_string()
    }
}

/// `observeResult(result)`：只报告一次。
///
/// TS 的 `observedResult` 守卫在调用回调**之前**置位，因此 `onResult` 抛错后 catch 分支里的
/// 第二次 `observeResult` 不会再次调用它。回调 panic（TS 的「抛错」）用 `catch_unwind`
/// 映射成 `Err(error.message)`，与 `onEvent` 的 panic 走同一条 `forwardStream` 拒绝路径。
///
/// 注意 `swap(true)` 在检查 `on_result` 是否存在之前执行：即使没有回调，这次调用也算
/// 「已报告」，与 TS 的守卫顺序一致。
fn observe_result(
    observer: &Mutex<Option<LazyStreamObserver>>,
    observed_result: &AtomicBool,
    result: Option<&AssistantMessage>,
) -> Result<(), String> {
    if observed_result.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let observer = observer.lock().unwrap_or_else(|e| e.into_inner());
    let Some(on_result) = observer.as_ref().and_then(|o| o.on_result.as_ref()) else {
        return Ok(());
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_result(result)))
        .map_err(panic_payload_message)
}

/// `Date.now()`：Unix 毫秒，时钟早于 epoch 时退化为 0。
///
/// 只用于失败消息的 `timestamp`，不参与计时；亚毫秒部分被 `as_millis` 截断。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

// ---------------------------------------------------------------------------
// lazyApi
// ---------------------------------------------------------------------------

/// `LazyApiCapabilities`：声明包装后的实现暴露哪些可选能力。
///
/// TS 用「对象上有没有这个方法」判断；Rust 用 [`ProviderStreams::has_fetch_deferred`] /
/// [`ProviderStreams::has_cancel_deferred`] 谓词表达同一语义。
///
/// 两个开关只表示「包装层是否暴露能力」，不检查被加载的实现是否真的支持。调用时会做
/// 两次检查：先看本结构，`load()` 之后再检查被加载实现的同名谓词。声明为 `true` 但实现
/// 不支持时，`fetch_deferred` 得到 `error` 流，`cancel_deferred` 得到 `Some(Err(..))`
/// （错误文案是当前稳定契约，改动需同步测试）。
///
/// TS: `src/api/lazy.ts#LazyApiCapabilities`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LazyApiCapabilities {
    /// 暴露 `fetchDeferred`。
    pub fetch_deferred: bool,
    /// 暴露 `cancelDeferred`。
    pub cancel_deferred: bool,
}

/// `load()`：加载 API 实现模块（异步解析出 [`ProviderStreams`]）。
///
/// 失败时返回的 `AiError` 与 TS 的 import rejection 一样进入 [`lazy_stream`] 的错误事件。
/// future 必须是 `Send + 'static`，因为它会被移进后台任务。
pub type LazyApiLoadFuture =
    Pin<Box<dyn Future<Output = Result<Arc<dyn ProviderStreams>, AiError>> + Send + 'static>>;
/// [`LazyApiLoadFuture`] 的工厂（每次调用触发一次模块加载）。
///
/// 用 `Arc<dyn Fn>` 而不是 `FnMut`：同一 [`lazy_api`] 返回值可能被多个请求并发使用，
/// 加载器必须可跨线程共享。模块缓存/去重（Node 的 import cache 等价物）由闭包自身负责，
/// 通常是内部的 `OnceLock`/`OnceCell`。
pub type LazyApiLoad = Arc<dyn Fn() -> LazyApiLoadFuture + Send + Sync>;

/// `lazyApi(load, capabilities?)`。
///
/// 每次 `stream` / `streamSimple`（以及声明过的 deferred 调用）执行一次 `load()` —— 与 TS
/// 每次 `await load()` 一致，模块缓存去重由加载器负责；加载失败以 `error` 事件结束流。
///
/// # 错误与能力
///
/// - `stream` / `stream_simple` / `fetch_deferred` 中 `load()` 返回的 `AiError` 进入
///   [`lazy_stream`] 的错误事件，流以 `stopReason: "error"` 结束，而不是 panic；
///   `cancel_deferred` 没有流，错误直接放在 `Some(Err(..))` 里返回（见下一条）。
/// - `capabilities` 为 `None` 等价于两项都为 `false`（`LazyApiCapabilities::default()`）：
///   此时 `fetch_deferred` / `cancel_deferred` 返回 `None`，并且不会触发 `load()`。
/// - `cancel_deferred` 的 `Option<Result<(), AiError>>` 要区分来源：`None` = 包装层未声明
///   该能力（不会触发 `load()`）；`Some(Ok(()))` = 实现成功取消；`Some(Err(error))` =
///   `load()` 失败，错误原样返回；`Some(Err(Unsupported))` = 声明了能力但实现不支持
///   （实现谓词为 `false`，或实现自身返回 `None`）；`Some(Err(other))` = 实现自身的取消失败，
///   原样透传。
///
/// TS: `src/api/lazy.ts#lazyApi`
pub fn lazy_api(
    load: LazyApiLoad,
    capabilities: Option<LazyApiCapabilities>,
) -> Arc<dyn ProviderStreams> {
    Arc::new(LazyApi {
        load,
        capabilities: capabilities.unwrap_or_default(),
    })
}

/// [`lazy_api`] 返回的实现。
struct LazyApi {
    /// 每次流式/延迟调用时执行的加载器（缓存语义见 [`LazyApiLoad`]）。
    load: LazyApiLoad,
    /// 包装层声明暴露的能力；未声明的能力对调用方表现为 `None`。
    capabilities: LazyApiCapabilities,
}

/// 实现模块没有该方法时的错误（`fetchDeferred`）。
///
/// 消息是当前稳定契约，测试断言依赖它；不要改写。
const FETCH_DEFERRED_UNSUPPORTED: &str = "API does not support deferred responses";
/// 实现模块没有该方法时的错误（`cancelDeferred`）。
///
/// 消息是当前稳定契约，测试断言依赖它；不要改写。
const CANCEL_DEFERRED_UNSUPPORTED: &str = "API cannot cancel deferred responses";

/// 构造 `Unsupported` 类错误（能力声明与实现不一致时使用）。
fn unsupported(message: &str) -> AiError {
    AiError::new(AiErrorKind::Unsupported, message)
}

#[async_trait]
impl ProviderStreams for LazyApi {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // `lazy_stream` 只借用 model 来生成错误消息，而 async move 块需要 `'static` 的副本；
        // context 克隆后由 async move 块持有，再以 `&context` 借给被加载的实现。
        let load = self.load.clone();
        let model = model.clone();
        let context = context.clone();
        let setup_model = model.clone();
        lazy_stream(
            &model,
            Box::pin(async move {
                let implementation = load().await?;
                Ok(implementation.stream(&setup_model, &context, options))
            }),
            None,
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // 与 `stream` 相同：每次调用都执行一次 `load()`，不缓存实现（缓存由加载器负责）。
        let load = self.load.clone();
        let model = model.clone();
        let context = context.clone();
        let setup_model = model.clone();
        lazy_stream(
            &model,
            Box::pin(async move {
                let implementation = load().await?;
                Ok(implementation.stream_simple(&setup_model, &context, options))
            }),
            None,
        )
    }

    fn has_fetch_deferred(&self) -> bool {
        self.capabilities.fetch_deferred
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        // 包装层没声明该能力时与 TS「对象上没有 fetchDeferred 方法」等价：同步返回 None，
        // 连 load() 都不执行。
        if !self.capabilities.fetch_deferred {
            return None;
        }
        let load = self.load.clone();
        let model = model.clone();
        let handle = handle.clone();
        let setup_model = model.clone();
        Some(lazy_stream(
            &model,
            Box::pin(async move {
                let implementation = load().await?;
                // 包装层声明了能力但实际实现没有：load 之后返回错误，这里变成流上的
                // error 事件。实现返回 None 也走同一条错误。
                if !implementation.has_fetch_deferred() {
                    return Err(unsupported(FETCH_DEFERRED_UNSUPPORTED));
                }
                match implementation.fetch_deferred(&setup_model, &handle, options) {
                    Some(stream) => Ok(stream),
                    None => Err(unsupported(FETCH_DEFERRED_UNSUPPORTED)),
                }
            }),
            None,
        ))
    }

    fn has_cancel_deferred(&self) -> bool {
        self.capabilities.cancel_deferred
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Option<Result<(), AiError>> {
        // 与 fetch_deferred 不同，取消没有事件流可承载错误：load 失败与「实现不支持」都
        // 通过 Some(Err(..)) 返回；None 只表示包装层未声明该能力（不触发 load）。
        if !self.capabilities.cancel_deferred {
            return None;
        }
        let load = self.load.clone();
        let model = model.clone();
        let handle = handle.clone();
        let implementation = match load().await {
            Ok(implementation) => implementation,
            Err(error) => return Some(Err(error)),
        };
        // 同 fetch_deferred：声明了能力但实现没有，返回 Unsupported 而不是静默成功。
        if !implementation.has_cancel_deferred() {
            return Some(Err(unsupported(CANCEL_DEFERRED_UNSUPPORTED)));
        }
        match implementation
            .cancel_deferred(&model, &handle, options)
            .await
        {
            Some(result) => Some(result),
            // 实现自己返回 None 表示它不提供取消：与「方法缺失」同一错误，不当作成功。
            None => Some(Err(unsupported(CANCEL_DEFERRED_UNSUPPORTED))),
        }
    }
}
