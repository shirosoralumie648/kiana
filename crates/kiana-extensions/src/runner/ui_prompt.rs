//! `with_ui_prompt` / `emit_ui_prompt_event`。
//!
//! TS 对应物：`packages/coding-agent/src/core/extensions/runner.ts` 的
//! `wrapUIPromptContext` / `withUIPrompt` / `emitUIPromptEvent`（`ca-extensions.md` §2.3.8、
//! `design-extension-runtime.md` §4.5、§2.5）。
//!
//! 每当扩展通过 `ctx.ui.select/confirm/input/editor/custom` 打开一个阻塞式对话框时，把当前的
//! prompt 深度加一；深度 `0 -> 1` 时发出一次 `ui_prompt_start`，归零时发出一次
//! `ui_prompt_end`。嵌套对话框（例如 `custom` 组件里再开 `confirm`）只发**外层**的一对事件，
//! 且 `end` 的 `kind`/`title` 取外层开始时捕获的 prompt（TS 的 `activeUIPrompt`）。
//!
//! `title` 为空字符串或 `undefined` 时事件里**不含** `title` 字段（TS 的 `...(title ? { title }
//! : {})`，空串是 falsy）。
//!
//! 事件通过 [`UiPromptScheduler`] 延后到微任务（`design-extension-runtime.md` §2.5）：默认
//! [`DeferredScheduler`] 在 tokio 运行时里用一个 FIFO 队列 + 单个先 `yield_now()` 的任务排空，
//! **没有运行时时交给进程级后台线程**（同样不在调用方栈上执行；测试可注入
//! [`scheduler_from_fn`] 或自己的调度器，包括引擎的 `queueMicrotask`）。
//! `UiPromptHost::emit_ui_prompt` 本身是同步的；`runner-core` 的实现负责把它交给通用
//! `emit`（自身的 spawn 也是允许的，双重延后仍然保持“不是同步发出”）。
//!
//! 出错/取消：`UiPromptGuard` 在 `Drop` 时归零深度，因此同步 panic（TS 的 `run()` 同步抛错）
//! 与 future 被丢弃都会发出配对的 `end`，深度不会泄漏。
//!
//! # 状态机
//!
//! 状态只有两个字段：`depth`（当前打开的阻塞式对话框数）与 `active`（最外层 prompt 的
//! `kind`/`title`）。
//!
//! 1. [`UiPromptContext::enter`]：归一化标题后加锁；`depth == 0` 时记录 `active` 并调度
//!    `ui_prompt_start`，否则只加计数。返回的 [`UiPromptGuard`] 持有本次 kind/title。
//! 2. [`UiPromptGuard`] 的 `Drop` 调用私有的 `finish`：`depth` 减一；仍大于 0 时不发事件；
//!    归零时取走 `active` 并调度 `ui_prompt_end`（`active` 缺失时用 guard 的值兜底）。
//! 3. 因此嵌套 prompt 只产生最外层的一对事件，`end` 的 kind/title 也来自最外层。
//!
//! 深度在调用点**同步**增加（不是等 future 首次 poll），事件则经调度器延后。future 被
//! 丢弃、取消或同步 panic 都会在 `Drop` 中配对发出 `end`，深度不会泄漏。
//!
//! # 5 个阻塞成员与其余成员
//!
//! 只有 `select`/`confirm`/`input`/`editor`/`custom` 进入上述状态机；其余成员直接转发给被
//! 包装的上下文。`select`/`confirm`/`input` 会在调用点检查 `opts.signal` 是否已取消，已取消
//! 则不调用内层实现，直接以 `None`/`false` 完成——但 `start` 已经入队，`end` 随 future
//! 完成配对发出（TS 的 `withUIPrompt` 包住了整段早退逻辑）。`editor`/`custom` 的签名没有
//! `opts`，不做该检查。
//!
//! # 调度器契约
//!
//! [`UiPromptScheduler::schedule`] 只负责“稍后执行”，不得阻塞；事件绝不在调用方的同步栈上
//! 发出。默认 [`DeferredScheduler`] 保证同一实例上的任务按入队顺序（FIFO）执行：有 tokio
//! 运行时用一个排空任务，没有运行时时用进程级后台线程。单个任务 panic 不会丢失后续任务；
//! tokio 路径在排空后把第一个 panic 重新展开，后台线程路径则吞掉它（线程不能因单个事件
//! 退出）。
//!
//! # 调用方
//!
//! [`ExtensionRunner::set_ui_context`](super::ExtensionRunner::set_ui_context) 在 `Some(ui)`
//! 时用 [`UiPromptContext::wrap`] 包装真实 UI，并注入 `RunnerUiPromptHost`（把事件转给
//! 通用 `emit`，见 `runner` 模块）。包装后的上下文与原始 `Arc` **不是**同一个指针，
//! `has_ui()` 因此仍为 true；`set_ui_context(None, mode)` 则回落到 no-op 单例，不经过本模块。

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::events::{
    ExtensionEvent, UIPromptEndEvent, UIPromptEndEventType, UIPromptKind, UIPromptReason,
    UIPromptStartEvent, UIPromptStartEventType,
};
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, TerminalInputHandler, ThemeInfo, ThemeOrName, ThemeResult,
    WidgetContent, WorkingIndicatorOptions,
};
use kiana_theme::theme::Theme;

/// `ui_prompt_start` / `ui_prompt_end` 的接收方。
///
/// `runner-core` 为 `ExtensionRunner` 实现本 trait：同步地把事件交给通用 `emit`
/// （通常是 `spawn` 一个 `void this.emit(event)` 的等价任务）。本 trait 的方法**不得**
/// 阻塞；`UiPromptContext` 在状态锁之外调用它。实现以 `Arc<dyn UiPromptHost>` 长期持有，
/// 因此必须是 `Send + Sync`（trait 上界），并且要能接受调度器线程/异步任务上的调用。
pub trait UiPromptHost: Send + Sync {
    /// 同步发出一个 `ui_prompt_start` / `ui_prompt_end` 事件。
    ///
    /// 默认调度器 [`DeferredScheduler`] 保证事件按入队顺序到达；自定义调度器按自己的
    /// 执行顺序调用，宿主实现不能假设所有调度器都是 FIFO。
    fn emit_ui_prompt(&self, event: ExtensionEvent);
}

/// 把“稍后执行”的调度策略注入 [`UiPromptContext`]（TS 的 `queueMicrotask`）。
///
/// 实现必须是线程安全的：trait 上界是 `Send + Sync`，调度器以 `Arc` 长期共享。`schedule`
/// 只登记任务，不得阻塞，也不得在调用栈上同步执行任务（见
/// [`scheduler_from_fn`] 的说明）。
pub trait UiPromptScheduler: Send + Sync {
    /// 安排 `task` 在当前同步栈之后执行；不得阻塞。
    fn schedule(&self, task: Box<dyn FnOnce() + Send + 'static>);
}

/// 默认调度器：tokio 运行时里用一个 FIFO 队列 + 单个 `spawn` 出来的排空任务；没有运行时时
/// 交给进程级后台线程（同样不在调用方栈上执行，顺序不变）。
///
/// 设计文档 §2.5 要求事件不在当前同步栈里发出；`yield_now()` 让同一 tick 里排队的其它任务
/// 先跑（近似微任务顺序）。tokio 的 current-thread 调度器对刚 `spawn` 的任务近似 LIFO，
/// 因此**不能**每次 `schedule` 都 `spawn` 一个任务：先排入的 `ui_prompt_start` 可能晚于
/// `ui_prompt_end` 执行。这里把任务放进队列，只在没有排空任务时 `spawn` 一次，由排空循环
/// 按入队顺序执行（与 TS 的 `queueMicrotask` FIFO 一致）。
///
/// `Clone` 共享同一个队列与排空任务（内部状态是 `Arc`）：从任意克隆入队的任务都进入同一
/// FIFO 顺序，不会各自启动独立的排空循环。
///
/// 队列没有容量上限/背压：生产者远快于排空时任务会在内存中累积；进程退出时未执行的
/// 任务直接丢失。
#[derive(Clone, Default)]
pub struct DeferredScheduler {
    state: Arc<DeferredState>,
}

#[derive(Default)]
struct DeferredState {
    inner: Mutex<DeferredInner>,
}

#[derive(Default)]
struct DeferredInner {
    queue: std::collections::VecDeque<Box<dyn FnOnce() + Send + 'static>>,
    flush_pending: bool,
}

impl UiPromptScheduler for DeferredScheduler {
    fn schedule(&self, task: Box<dyn FnOnce() + Send + 'static>) {
        let state = Arc::clone(&self.state);
        // 入队与“是否需要启动排空任务”的决定在同一把锁内完成：并发 schedule 时只有第一个
        // 会拿到 spawn 许可，其余只入队，因此不会出现两个排空循环抢同一队列。
        let spawn = {
            let mut inner = lock(&state.inner);
            inner.queue.push_back(task);
            if inner.flush_pending {
                false
            } else {
                inner.flush_pending = true;
                true
            }
        };
        if !spawn {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    tokio::task::yield_now().await;
                    if let Some(payload) = drain(&state) {
                        // 队列状态已经复位（`flush_pending = false`），后续 schedule 会重新
                        // 启动排空任务；这里把第一个 panic 继续展开给调用方（与无调度器时
                        // 直接执行任务一致）。
                        std::panic::resume_unwind(payload);
                    }
                });
            }
            // 没有运行时（例如只在一个同步的 `with_js` 事务里调用 UI）：交给进程级后台
            // 线程。绝不能内联执行——§2.5 要求事件不在当前同步栈上发出。
            Err(_) => background::dispatch(state),
        }
    }
}

/// 无 tokio 运行时的后备：一个按 FIFO 处理排空请求的后台线程。
mod background {
    use std::sync::{Arc, LazyLock, Mutex};

    use super::{DeferredState, drain};

    type Queue = std::sync::mpsc::Sender<Arc<DeferredState>>;

    static SENDER: LazyLock<Mutex<Queue>> = LazyLock::new(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<Arc<DeferredState>>();
        std::thread::Builder::new()
            .name("kiana-ui-prompt".to_owned())
            .spawn(move || {
                while let Ok(state) = receiver.recv() {
                    // 单个任务 panic 不能杀死调度线程，也不能卡住后续事件。
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _ = drain(&state);
                    }));
                }
            })
            .expect("spawn the ui prompt scheduler thread");
        Mutex::new(sender)
    });

    pub(super) fn dispatch(state: Arc<DeferredState>) {
        let _ = SENDER
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .send(state);
    }
}

/// 按入队顺序执行队列里的任务；排空期间新入队的任务由同一个循环继续处理。
///
/// 返回第一个 panic 的 payload（任务已经全部排空、`flush_pending` 已复位），由调用方决定
/// 是否继续展开。单个任务 panic 不会让后续的 `ui_prompt_*` 事件永久丢失。
fn drain(state: &DeferredState) -> Option<Box<dyn std::any::Any + Send>> {
    let mut panic = None;
    loop {
        let task = lock(&state.inner).queue.pop_front();
        match task {
            Some(task) => {
                if let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(task))
                    && panic.is_none()
                {
                    panic = Some(payload);
                }
            }
            None => {
                // pop 与“是否真的空了”分两次加锁：若在两者之间有新任务入队，这里会看到
                // 非空并继续循环，避免把任务留在队列里而 `flush_pending` 已经复位。
                let mut inner = lock(&state.inner);
                if inner.queue.is_empty() {
                    inner.flush_pending = false;
                    return panic;
                }
            }
        }
    }
}

/// 用闭包构造调度器（引擎的 `queueMicrotask`、测试的手动队列等）。
///
/// 本函数**不提供任何延后语义**：任务是否延后、在哪个线程执行、是否保持 FIFO，全部由
/// 传入的闭包决定。如果闭包同步调用 `task()`，`ui_prompt_*` 事件就会在调用点同步发出，
/// 违反 `design-extension-runtime.md` §2.5 的“不在当前同步栈上发出”约定；需要该约定的
/// 调用方应使用 [`DeferredScheduler`] 或自己实现真正的延后。
pub fn scheduler_from_fn<F>(schedule: F) -> Arc<dyn UiPromptScheduler>
where
    F: Fn(Box<dyn FnOnce() + Send + 'static>) + Send + Sync + 'static,
{
    struct FnScheduler<F>(F);
    impl<F> UiPromptScheduler for FnScheduler<F>
    where
        F: Fn(Box<dyn FnOnce() + Send + 'static>) + Send + Sync,
    {
        fn schedule(&self, task: Box<dyn FnOnce() + Send + 'static>) {
            (self.0)(task);
        }
    }
    Arc::new(FnScheduler(schedule))
}

/// 计数器的内部状态。
#[derive(Debug, Default)]
struct PromptState {
    depth: usize,
    active: Option<ActivePrompt>,
}

/// 外层 prompt 的 `kind`/`title`（TS 的 `activeUIPrompt`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePrompt {
    /// prompt 种类。
    pub kind: UIPromptKind,
    /// prompt 标题；空标题为 `None`。
    pub title: Option<String>,
}

/// 包装任意 [`ExtensionUiContext`]，为 5 个阻塞式对话框维护 `ui_prompt_*` 深度。
///
/// 创建一次并长期持有（`runner-core` 的 `set_ui_context` 用它包装真实 UI）；内部状态
/// 只有深度与当前 prompt，线程安全。
///
/// 所有 trait 方法都是 `&self`：状态迁移在内部 `Mutex` 内完成，事件调度在锁外进行，
/// 因此可以在同一线程上重入（嵌套对话框就是重入的 `enter`）。被包装的上下文负责真正的
/// 交互；本类型只做计数与事件。
pub struct UiPromptContext {
    inner: Arc<dyn ExtensionUiContext>,
    host: Arc<dyn UiPromptHost>,
    scheduler: Arc<dyn UiPromptScheduler>,
    state: Mutex<PromptState>,
}

impl std::fmt::Debug for UiPromptContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiPromptContext")
            .field("depth", &self.depth())
            .field("active", &self.active_prompt())
            .finish_non_exhaustive()
    }
}

impl UiPromptContext {
    /// 用默认调度器（[`DeferredScheduler`]）包装 `inner`。
    pub fn new(inner: Arc<dyn ExtensionUiContext>, host: Arc<dyn UiPromptHost>) -> Arc<Self> {
        Self::with_scheduler(inner, host, Arc::new(DeferredScheduler::default()))
    }

    /// 指定调度器包装 `inner`（测试或引擎 `queueMicrotask`）。
    pub fn with_scheduler(
        inner: Arc<dyn ExtensionUiContext>,
        host: Arc<dyn UiPromptHost>,
        scheduler: Arc<dyn UiPromptScheduler>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner,
            host,
            scheduler,
            state: Mutex::new(PromptState::default()),
        })
    }

    /// 包装成 `Arc<dyn ExtensionUiContext>`（`set_ui_context` 的落点）。
    ///
    /// 返回的指针与 `inner` 不同；它也不是 no-op 单例，因此 `has_ui()` 会判定为有 UI。
    /// 需要保留具体类型（调用 [`UiPromptContext::depth`] 等）时用 [`UiPromptContext::new`]。
    pub fn wrap(
        inner: Arc<dyn ExtensionUiContext>,
        host: Arc<dyn UiPromptHost>,
    ) -> Arc<dyn ExtensionUiContext> {
        Self::new(inner, host)
    }

    /// 被包装的原始 UI 上下文。
    pub fn inner(&self) -> &Arc<dyn ExtensionUiContext> {
        &self.inner
    }

    /// 当前 prompt 深度（0 表示没有阻塞式对话框，>1 表示有嵌套）。
    pub fn depth(&self) -> usize {
        lock(&self.state).depth
    }

    /// 当前外层 prompt（深度为 0 时 `None`）。
    ///
    /// 返回克隆；修改它不会影响内部状态。
    pub fn active_prompt(&self) -> Option<ActivePrompt> {
        lock(&self.state).active.clone()
    }

    /// 进入一次 prompt：深度加一；`0 -> 1` 时捕获 prompt 并（延后）发出 `ui_prompt_start`。
    ///
    /// 标题按 TS 的 `...(title ? { title } : {})` 归一：空字符串与 `None` 都变成 `None`。
    /// 状态修改在锁内完成，`ui_prompt_start` 的调度在锁外进行（宿主实现可能重入本上下文）。
    ///
    /// 返回的 guard 在 `Drop` 时归零，因此同步 panic 与取消都会配对发出 `ui_prompt_end`。
    pub fn enter(&self, kind: UIPromptKind, title: Option<&str>) -> UiPromptGuard<'_> {
        let title = normalize_title(title);
        let start = {
            let mut state = lock(&self.state);
            let outer = state.depth == 0;
            state.depth += 1;
            if outer {
                state.active = Some(ActivePrompt {
                    kind,
                    title: title.clone(),
                });
                true
            } else {
                false
            }
        };
        // 只有最外层负责发 start；嵌套层只加计数，避免出现不成对的事件。
        if start {
            self.emit(UiPromptEvent::Start {
                kind,
                title: title.clone(),
            });
        }
        UiPromptGuard {
            ctx: self,
            kind,
            title,
        }
    }

    /// 归零深度；归零时（延后）发出 `ui_prompt_end`。
    ///
    /// 由 `UiPromptGuard` 的 `Drop` 实现调用。`depth` 先减一；仍有嵌套时直接返回。归零时
    /// 取走 `active`（外层记录的 kind/title），缺失时用 guard 自带的值兜底——正常路径下
    /// `active` 一定存在，兜底只防御手工构造或状态被外部破坏的情况。
    fn finish(&self, fallback_kind: UIPromptKind, fallback_title: Option<String>) {
        let prompt = {
            let mut state = lock(&self.state);
            if state.depth > 0 {
                state.depth -= 1;
            }
            if state.depth > 0 {
                return;
            }
            state.depth = 0;
            let prompt = state.active.take();
            match prompt {
                Some(prompt) => prompt,
                None => ActivePrompt {
                    kind: fallback_kind,
                    title: fallback_title,
                },
            }
        };
        // 与 enter 对称：状态锁已释放，事件交给调度器异步发出。
        self.emit(UiPromptEvent::End {
            kind: prompt.kind,
            title: prompt.title,
        });
    }

    /// 把事件排进调度器；`UiPromptHost` 在锁外被调用。
    ///
    /// 先克隆 `host` 的 `Arc` 再交给任务：调度器可能在任意线程/稍后执行，任务不能借用
    /// `self`。
    fn emit(&self, event: UiPromptEvent) {
        let host = Arc::clone(&self.host);
        self.scheduler.schedule(Box::new(move || {
            host.emit_ui_prompt(event.into_event());
        }));
    }

    /// 跑 `run` 并维护深度（TS 的 `withUIPrompt`）。
    ///
    /// `run` 在**本函数返回前同步调用**（只负责创建 future），因此它同步 panic 时能在这里
    /// 捕获：先 drop guard（调度配对的 `end`），再把 panic 原样展开。正常路径下 guard 被
    /// 移进返回的 future；future 完成或被丢弃时 guard 析构，深度归零。
    ///
    /// 注意 `run` 本身拿不到 `UiPromptGuard`，不能提前结束深度；提前完成只能通过返回一个
    /// 已完成的 future。
    pub fn with_prompt<'a, T: 'a>(
        &'a self,
        kind: UIPromptKind,
        title: Option<&str>,
        run: impl FnOnce() -> LocalBoxFuture<'a, T> + 'a,
    ) -> LocalBoxFuture<'a, T> {
        let guard = self.enter(kind, title);
        match std::panic::catch_unwind(AssertUnwindSafe(run)) {
            Ok(future) => Box::pin(async move {
                let result = future.await;
                drop(guard);
                result
            }),
            Err(payload) => {
                drop(guard);
                std::panic::resume_unwind(payload)
            }
        }
    }
}

/// [`UiPromptContext::enter`] 的守卫：`Drop` 时归零深度。
///
/// `kind`/`title` 是本次进入时捕获的值，只在 `active` 缺失时作为 `end` 的兜底；正常路径
/// 下 `end` 使用最外层 prompt 的值。guard 借用上下文，保证上下文活得比它久；它不可克隆，
/// 因此正常析构下每次 `enter` 恰好对应一次 `finish`。如果调用方 `std::mem::forget` 或
/// 故意泄漏 guard，深度不会归零，后续 prompt 将被当作嵌套（这对应用是误用，本类型不阻止）。
pub struct UiPromptGuard<'a> {
    ctx: &'a UiPromptContext,
    kind: UIPromptKind,
    title: Option<String>,
}

impl std::fmt::Debug for UiPromptGuard<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiPromptGuard")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Drop for UiPromptGuard<'_> {
    fn drop(&mut self) {
        self.ctx.finish(self.kind, self.title.take());
    }
}

/// `with_ui_prompt(kind, title, run)`（TS 的自由函数形态）。
///
/// 等价于 [`UiPromptContext::with_prompt`]；保留自由函数是为了与 TS 的调用点一一对应。
/// 同步 panic 时仍然调度配对的 `ui_prompt_end` 并继续展开。
pub fn with_ui_prompt<'a, T: 'a>(
    ctx: &'a UiPromptContext,
    kind: UIPromptKind,
    title: Option<&str>,
    run: impl FnOnce() -> LocalBoxFuture<'a, T> + 'a,
) -> LocalBoxFuture<'a, T> {
    ctx.with_prompt(kind, title, run)
}

/// 事件载荷（构造 `ExtensionEvent` 前的 Rust 形态，便于测试断言与跨线程移动）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiPromptEvent {
    /// `ui_prompt_start`。
    Start {
        /// prompt 种类。
        kind: UIPromptKind,
        /// prompt 标题；空标题为 `None`（事件里不含 `title` 键）。
        title: Option<String>,
    },
    /// `ui_prompt_end`。
    End {
        /// prompt 种类。
        kind: UIPromptKind,
        /// prompt 标题；空标题为 `None`。
        title: Option<String>,
    },
}

impl UiPromptEvent {
    /// 转成 `kiana-ext-api` 的事件（`reason` 恒为 `"ui_prompt"`）。
    pub fn into_event(self) -> ExtensionEvent {
        match self {
            UiPromptEvent::Start { kind, title } => {
                ExtensionEvent::UIPromptStart(UIPromptStartEvent {
                    event_type: UIPromptStartEventType::UIPromptStart,
                    reason: UIPromptReason::UiPrompt,
                    kind,
                    title,
                })
            }
            UiPromptEvent::End { kind, title } => ExtensionEvent::UIPromptEnd(UIPromptEndEvent {
                event_type: UIPromptEndEventType::UIPromptEnd,
                reason: UIPromptReason::UiPrompt,
                kind,
                title,
            }),
        }
    }
}

/// `opts.signal?.aborted`（TS 的 dialog 早退条件）。
fn dialog_aborted(opts: Option<&ExtensionUIDialogOptions>) -> bool {
    opts.and_then(|opts| opts.signal.as_ref())
        .is_some_and(|signal| signal.is_cancelled())
}

/// TS 的 `...(title ? { title } : {})`：空字符串是 falsy。
fn normalize_title(title: Option<&str>) -> Option<String> {
    match title {
        Some(title) if !title.is_empty() => Some(title.to_owned()),
        _ => None,
    }
}

/// 不可能被 poison 的状态锁：即使持锁线程 panic，状态仍然保持一致（只做计数）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

// ---------------------------------------------------------------------------
// 5 个阻塞式对话框 + 23 个直通成员
// ---------------------------------------------------------------------------

// 5 个阻塞成员共享同一形状：调用点同步 `enter` → 把参数克隆进 future → future 内调用内层
// → `drop(guard)`。guard 必须被移进 future（而不是留在函数局部），这样调用方丢弃未完成的
// future 时也会触发 `finish`，深度不泄漏、`end` 不缺。`select`/`confirm`/`input` 额外在
// future 创建前检查 `opts.signal`：已取消就不调用内层，直接完成默认值。
//
// 其余 23 个成员只做转发；它们不改变深度，也不产生 `ui_prompt_*` 事件。

impl ExtensionUiContext for UiPromptContext {
    fn select<'a>(
        &'a self,
        title: &str,
        options: &[String],
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'a, Option<String>> {
        // 深度在调用点同步加一（TS 的 `this.uiPromptDepth++ === 0`），参数克隆进 future，
        // 因为 trait 的返回 future 只借用 `&self`。
        let guard = self.enter(UIPromptKind::Select, Some(title));
        let title = title.to_owned();
        let options = options.to_vec();
        // TS 的 `showExtensionSelector` 在 `withUIPrompt` **内部**早退：`ui_prompt_start`
        // 已经发出，立即以 `undefined` 完成（随后 `finish` 发 `ui_prompt_end`）。
        let aborted = dialog_aborted(opts.as_ref());
        Box::pin(async move {
            let result = if aborted {
                None
            } else {
                self.inner.select(&title, &options, opts).await
            };
            drop(guard);
            result
        })
    }

    fn confirm<'a>(
        &'a self,
        title: &str,
        message: &str,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'a, bool> {
        let guard = self.enter(UIPromptKind::Confirm, Some(title));
        let title = title.to_owned();
        let message = message.to_owned();
        let aborted = dialog_aborted(opts.as_ref());
        Box::pin(async move {
            let result = if aborted {
                false
            } else {
                self.inner.confirm(&title, &message, opts).await
            };
            drop(guard);
            result
        })
    }

    fn input<'a>(
        &'a self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'a, Option<String>> {
        let guard = self.enter(UIPromptKind::Input, Some(title));
        let title = title.to_owned();
        let placeholder = placeholder.map(str::to_owned);
        let aborted = dialog_aborted(opts.as_ref());
        Box::pin(async move {
            let result = if aborted {
                None
            } else {
                self.inner.input(&title, placeholder.as_deref(), opts).await
            };
            drop(guard);
            result
        })
    }

    fn editor<'a>(
        &'a self,
        title: &str,
        prefill: Option<&str>,
    ) -> LocalBoxFuture<'a, Option<String>> {
        let guard = self.enter(UIPromptKind::Editor, Some(title));
        let title = title.to_owned();
        let prefill = prefill.map(str::to_owned);
        Box::pin(async move {
            let result = self.inner.editor(&title, prefill.as_deref()).await;
            drop(guard);
            result
        })
    }

    fn custom<'a>(
        &'a self,
        factory: CustomFactory,
        options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'a, Box<dyn std::any::Any + Send>> {
        let guard = self.enter(UIPromptKind::Custom, None);
        Box::pin(async move {
            let result = self.inner.custom(factory, options).await;
            drop(guard);
            result
        })
    }

    fn notify(&self, message: &str, kind: Option<NotifyKind>) {
        self.inner.notify(message, kind);
    }

    fn on_terminal_input(
        &self,
        handler: TerminalInputHandler,
    ) -> kiana_core::event_bus::Unsubscribe {
        self.inner.on_terminal_input(handler)
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.inner.set_status(key, text);
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.inner.set_working_message(message);
    }

    fn set_working_visible(&self, visible: bool) {
        self.inner.set_working_visible(visible);
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        self.inner.set_working_indicator(options);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.inner.set_hidden_thinking_label(label);
    }

    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    ) {
        self.inner.set_widget(key, content, options);
    }

    fn set_footer(&self, factory: Option<FooterFactory>) {
        self.inner.set_footer(factory);
    }

    fn set_header(&self, factory: Option<HeaderFactory>) {
        self.inner.set_header(factory);
    }

    fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    fn paste_to_editor(&self, text: &str) {
        self.inner.paste_to_editor(text);
    }

    fn set_editor_text(&self, text: &str) {
        self.inner.set_editor_text(text);
    }

    fn get_editor_text(&self) -> String {
        self.inner.get_editor_text()
    }

    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) {
        self.inner.add_autocomplete_provider(factory);
    }

    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        self.inner.set_editor_component(factory);
    }

    fn get_editor_component(&self) -> Option<EditorFactory> {
        self.inner.get_editor_component()
    }

    fn theme(&self) -> &Theme {
        self.inner.theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        self.inner.get_all_themes()
    }

    fn get_theme(&self, name: &str) -> Option<Theme> {
        self.inner.get_theme(name)
    }

    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult {
        self.inner.set_theme(theme)
    }

    fn get_tools_expanded(&self) -> bool {
        self.inner.get_tools_expanded()
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.inner.set_tools_expanded(expanded);
    }
}
