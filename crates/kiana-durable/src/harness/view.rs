//! `harness/view.ts`：会话视图挂载（活动转录 + 五个内建文档）。
//!
//! TS: `packages/durable/src/harness/view.ts`。视图在 Session 的变更线上按第一次观察构建，
//! 随之后的每一次已存提交推进；延迟提交（只在内存里采纳）也会到达这里，因为它们先于存储被看到。
//!
//! 本文件还提供两个通用的 watch 实现（`JsonWatch` / `NativeWatch`，对应 chord 的 `Watch` 与
//! `NativeWatch`）：`harness/events.rs` 与 `harness/task_graph.rs` 共用它们，语义与
//! `session/session.rs` 的文档 watch 一致——串行投递、折叠未投递的 op、超限时以快照替换。
//!
//! # 解决什么问题
//! 一次提交可以同时改会话记录、若干条目与任意数量的内建文档；消费方（事件流、任务图与会话
//! watch）需要的是"合并后的当前视图"，而不是各自重放每个提交。本文件的 [`ConversationViews`]
//! 把视图挂在 Session 的提交观察上：第一个观察者到来时构建一份完整基线，之后每次发布只把变化
//! 翻译成 `Op` 就地推进；最后一个观察者注销时挂载消失，下一次观察重新从已提交状态构建。
//!
//! # 边界
//! - 只有五个内建会话文档参与挂载（`mounted_tokens` 给出的 agent、live、inbox、provider、
//!   usage），且只接受 `record.key` 为 `None` 的单例；家族文档、任务文档与其他表的变化一律
//!   忽略。
//! - [`ConversationViews`] 由 `harness/harness.rs` 创建并作为 `HarnessImpl` 的字段持有；每个
//!   会话至多一个 `Mount`，由第一个观察者在 [`ConversationViews::attach`] 的变更线上构建，
//!   并登记进 `ConversationViews.mounts`。
//! - `harness/events.rs` 通过 `conversation_views(harness)` 取回 [`ConversationViews`] 后调用
//!   [`ConversationViews::attach`] 注册观察者；`harness/task_graph.rs` 使用自己的
//!   `TaskGraphView`，不经过本文件的挂载。
//! - `ConversationView.conversation` 只在构建挂载时读取；发布里的 `Conversation` 表变化不参与
//!   推进（该记录的字段都是创建时确定的）。
//!
//! # 执行流程
//! 1. [`ConversationViews::watch`] → [`ConversationViews::attach`]：在 Session 的变更线上
//!    （`read_on_line`）查找或构建挂载，取当前视图作为 watch 的初始值，再创建并登记观察者。
//!    注册与提交串行，因此初始值与之后的第一帧之间没有缺口。
//! 2. Session 的每次发布（已存提交或只在内存里采纳的延迟提交）同步调用
//!    [`ConversationViews::new`] 注册的内部监听器：对每个挂载跑一次私有的 `advance`，把变化
//!    折成文档 op 与条目 op，应用到挂载的 JSON 投影后反序列化回
//!    [`crate::harness::types::ConversationView`]；然后先给每个观察者 `advance`（仅当 op
//!    非空），再给每个观察者 `publication`（每帧都调用）。
//! 3. Session 关闭或失败时，关闭监听器对每个观察者调用 `end`，随后清空挂载表。
//!
//! # 重要约束与失败模式
//! - 推导出的 op 无法应用（`apply_immutable` 出错）或反序列化失败时，内部监听器调用
//!   `SessionImpl::fail_with_context` 让整个 Session 失败：失败的那个挂载本帧不通知观察者，
//!   挂载快照里排在它后面的挂载也不再推进（排在前面的已经收到本帧）。
//! - 观察者回调一律在挂载的 `Mutex` 之外调用；单个观察者 panic 由 `contained` 转换成错误
//!   交给报告通道，不影响其他观察者收到同一帧。watch 的监听器 panic 则由投递任务捕获，该
//!   watch 以 [`WatchEnd::ListenerError`] 落定。
//! - 每个 watch 的投递是独立 Tokio 任务里的串行循环：`stop()`/`closed()` 只反映终态，不等待
//!   正在执行的监听器调用返回。
//!
//! # 注册表
//! 只能拿到 `&dyn Harness` 的调用方（`harness/events.rs`）通过进程级 `VIEWS` 映射找回挂载：
//! 键是 Harness 对象地址，值是 `Weak<ConversationViews>`，因此登记本身不会延长挂载的生命周期。
//!
//! # 阅读顺序
//! 先看 [`ConversationViews`] 与它的 `attach`/`watch`，再看私有的 `advance`（提交如何变成 op），
//! 最后看 `WatchCore` 的排程与排空循环；`JsonWatch` 与 `NativeWatch` 只是两种队列策略的薄壳。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use kiana_chord::context::{Context, ContextRef, abort_error, without_abort_signal};
use kiana_chord::delta::Op;
use kiana_chord::delta::index::apply_immutable;
use kiana_chord::delta::pending::{PendingOps, PendingTake};
use kiana_chord::delta::{Path, Seg};

use crate::harness::agent::AGENT_DOC;
use crate::harness::context::{active_entries, capture_context_bounds};
use crate::harness::inbox::INBOX_DOC;
use crate::harness::live::LIVE_DOC;
use crate::harness::provider::PROVIDER_DOC;
use crate::harness::types::ConversationView;
use crate::harness::usage::USAGE_DOC;
use crate::harness::util::closed_error;
use crate::session::SessionImpl;
use crate::session::session::ContextSource;
use crate::session::session::{Failure, ObservedCommit, contained};
use crate::types::{
    BoxError, CommitChange, ConversationId, DocTarget, DocumentId, JsonObject, Storage,
    TableCommitChange, WatchEnd, WatchHandle, WatchListener,
};

// ---------------------------------------------------------------------------
// watch 基础设施
// ---------------------------------------------------------------------------

/// 监听器错误的报告通道（TS: `new Watch(..., report)` 的 `report`）。
///
/// 只在监听器 panic 被捕获后调用，且此时终止尚未被 stop/cancel/end 抢先（否则不报告）；
/// `None` 表示没有报告方（watch 仍以 [`WatchEnd::ListenerError`] 落定）。报告函数自身的
/// panic 不在本文件的捕获范围内。
pub type WatchReport = Arc<dyn Fn(BoxError) + Send + Sync>;

/// watch 的终态（内部，可重复读取）。
///
/// 公开的 [`WatchEnd`] 不可克隆，而 `stop()` 与 `closed()` 可以被多次调用、各自取走一份终态，
/// 因此内部先保存这个可克隆的镜像，`to_end` 在取出时重建。`Failed` 只保留错误的 `Display`
/// 文本，重建出的 [`WatchEnd::Failed`] 是 `ReportedError`，不再保留原始错误类型。
#[derive(Debug, Clone)]
pub(crate) enum WatchEndKind {
    Stopped,
    Cancelled,
    Ended,
    ListenerError(String),
    Failed(String),
}

impl WatchEndKind {
    fn to_end(&self) -> WatchEnd {
        match self {
            WatchEndKind::Stopped => WatchEnd::Stopped,
            WatchEndKind::Cancelled => WatchEnd::Cancelled,
            WatchEndKind::Ended => WatchEnd::Ended,
            WatchEndKind::ListenerError(message) => WatchEnd::ListenerError(message.clone()),
            WatchEndKind::Failed(message) => {
                WatchEnd::Failed(Box::new(crate::session::ReportedError(message.clone())))
            }
        }
    }
}

// 锁中毒时直接取回内部值继续运行，而不是让 watch 失败：本文件的临界区没有在 panic 时回滚
// 的机制；监听器、观察者回调、detach、report 都在锁外执行。这与 `session/session.rs` 的
// 同名辅助保持一致。
//
// 例外：`state` 锁内会同步调用调用方提供的回调——`JsonWatch` 的 `to_json`（`drain` 与
// `advance` 构造根替换时）和 `NativeWatch` 的 `replace`（队列溢出时）。它们必须保持纯函数，
// 且不得重入本 watch（例如在回调里调用 `value()`/`advance()`/`stop()`）：标准库 `Mutex`
// 不可重入，重取同一把 `state` 锁会死锁。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// 监听器 panic 的载荷按"信息量从高到低"还原：payload 本身就是 `BoxError` 时直接取回 →
// `String`（`panic!("{e}")`）→ `&'static str`（`panic!("literal")`）→ 固定兜底文本。
// 与 `session/session.rs` 的 `panic_error` 相比多支持 `BoxError`，兜底文案也不同（那里是
// `callback panicked`，这里是 `listener panicked`）。
fn panic_error(payload: Box<dyn std::any::Any + Send>) -> BoxError {
    match payload.downcast::<BoxError>() {
        Ok(error) => *error,
        Err(payload) => match payload.downcast::<String>() {
            Ok(message) => Box::new(crate::session::ReportedError(*message)),
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => Box::new(crate::session::ReportedError((*message).to_owned())),
                Err(_) => Box::new(crate::session::ReportedError(
                    "listener panicked".to_owned(),
                )),
            },
        },
    }
}

/// 已捕获、等待串行投递的一帧（ops 相对它之前的那一帧的值）。
///
/// `value` 是这一帧的完整新值（投递时监听器收到的就是它），`ops` 把上一版变成 `value`；
/// `context` 是捕获时的发布上下文。挂载推进路径在调用 `advance` 前已用 `without_abort_signal`
/// 遮蔽它的中止信号，因此监听器无法从投递上下文里拿到发布方（触发这一帧的提交）的取消
/// 信号；watch 自身在 `observe_cancellation` 里安装的取消信号从不进入帧上下文。
pub(crate) struct FoldedFrame<T> {
    value: Arc<T>,
    ops: Vec<Op>,
    context: ContextRef,
}

/// 待投递队列：JSON watch 按捕获的帧串行投递，native watch 保留每一帧（超过上限时替换为一次快照）。
///
/// 变体与 watch 种类一一对应：`JsonWatch` 只构造 `Folded`，`NativeWatch` 只构造 `Native`，
/// 所以两个 `advance` 里针对另一种变体的分支是 `unreachable!()`。两种队列的共同点是帧在捕获
/// 时就固定了 `Arc<T>` 与 `ContextRef`，投递任务只负责按顺序交给监听器。
pub(crate) enum Queue<T> {
    /// JSON：`start()` 之前到达的帧折叠成一次投递（TS 的 `PendingOps`）；之后每一次推进捕获
    /// 一帧，按顺序投递——监听器空闲时每个提交一次调用。帧数超过上限时折叠成一次根替换。
    Folded {
        /// `start()` 之前折叠后的 op。
        ops: PendingOps,
        /// `start()` 之前的最新一帧。
        newest: Option<(Arc<T>, ContextRef)>,
        /// 已捕获、等待串行投递的帧。
        ready: VecDeque<FoldedFrame<T>>,
        /// 溢出时把值转成 JSON 根替换。
        to_json: Arc<dyn Fn(&T) -> serde_json::Value + Send + Sync>,
    },
    /// 原生：每帧一次调用。
    Native {
        /// 待投递帧。
        frames: VecDeque<(Arc<T>, ContextRef)>,
        /// 溢出时的替换值。
        replace: Option<Arc<dyn Fn() -> T + Send + Sync>>,
        /// 上限（chord 的 100）。
        limit: usize,
    },
}

/// chord `NativeWatch` 的待投递上限。
///
/// 队列已满时 `NativeWatch::advance` 会清空积压、只留下一个替换帧（有 `replace` 回调就用它，
/// 否则用触发溢出的新值），因此这个常量既是"最多保留多少帧"的界，也是"溢出后只投递一帧"
/// 的分界。
pub(crate) const MAX_PENDING_NATIVE_VALUES: usize = 100;

/// JSON watch 已捕获但尚未投递的帧数上限：投递任务迟迟得不到执行时（例如运行时被占满），
/// 超出的帧折叠成一次根替换，内存不随提交数无界增长（与 `NativeWatch` 的 100 一致）。
///
/// 只统计 `start()` 之后的 `ready` 队列；`start()` 之前的帧由 `PendingOps` 自行折叠，不计入
/// 这里。达到上限时先清空已排队的帧，再压入一个 `Op::Replace(to_json(&value))` 帧，因此监听器
/// 看到的是一次全量快照，而不是被悄悄丢弃的中间提交。
pub(crate) const MAX_PENDING_JSON_FRAMES: usize = 100;

/// `WatchCore` 的共享状态。
///
/// 状态机：`started` 之前到达的帧按队列策略暂存；`started` 之后由 `scheduled`/`running` 保证
/// 至多一个排空任务在跑；`end` 一旦置位（stop/cancel/end/监听器 panic，先到者胜）就不再投递，
/// 队列被清空，`detach` 至多执行一次。所有字段都由同一个 `Mutex` 保护。
///
/// `value` 是最近一次**投递**的值，不是最新捕获的值：`advance` 只入队，`drain` 在调用监听器
/// 之前才更新它，因此 `WatchHandle::value()` 看到的是"已送达"的修订。
struct WatchState<T> {
    value: Arc<T>,
    queue: Queue<T>,
    started: bool,
    /// 已 spawn 的排空任务尚未开始运行（同一时刻只允许一个排程在途）。
    scheduled: bool,
    /// 排空任务正在运行；此时新的帧由循环自己接着处理，不需要再排程。
    running: bool,
    end: Option<WatchEndKind>,
    listener: Option<Arc<dyn WatchListener<T>>>,
    detach: Option<Box<dyn FnOnce() + Send + Sync>>,
    cancellation: Option<kiana_chord::context::AbortSubscription>,
    /// 终止广播：`closed()` 订阅它，`send_replace` 后所有等待者醒来。
    closed: tokio::sync::watch::Sender<Option<WatchEndKind>>,
    report: Option<WatchReport>,
}

/// 串行投递的 watch 内核（TS: `WatchLifecycle`）。
///
/// 与 `session/session.rs` 的文档 watch 一样：`value()` 交出的引用在句柄存活期间保持有效
/// （值保存在 `retained` 里），终止时清空待投递帧并调用 `detach`。
///
/// # 并发
/// - 所有可变状态在 `state: Mutex<...>` 里，`retained` 单独一把锁；两把锁从不嵌套持有。
///   监听器调用、`detach`、`report` 都在锁外执行，因此回调里可以安全地调 `value()`、
///   `advance()`、`stop()`。
/// - 每个 watch 至多一个排空任务：`scheduled`/`running` 在锁内切换，`advance` 只在两者都为
///   假时排程；`drain` 退出前若发现监听器运行期间又入队了新帧会再排一次，不依赖监听器侧
///   的通知，因此不会丢帧。
/// - 排程用 `Weak<WatchCore>`：句柄全部释放后，等待中的任务直接空转返回，不会延长 watch 的
///   生命周期。排程依赖 `tokio::runtime::Handle::try_current()`；失败时不 spawn 也不复位
///   `scheduled`，该 watch 不会再排程投递（本文件不做错误上报）。
pub(crate) struct WatchCore<T: Clone + Send + Sync + 'static> {
    state: Mutex<WatchState<T>>,
    /// 通过 `value()` 交出过的值：保证返回的 `&T` 在 `&self` 借用期内有效。只在 `WatchCore`
    /// 释放时清空，见 `current`。
    retained: Mutex<Vec<Arc<T>>>,
}

impl<T: Clone + Send + Sync + 'static> WatchCore<T> {
    fn new(
        value: T,
        queue: Queue<T>,
        detach: Box<dyn FnOnce() + Send + Sync>,
        report: Option<WatchReport>,
    ) -> Self {
        let (closed, _) = tokio::sync::watch::channel(None);
        WatchCore {
            state: Mutex::new(WatchState {
                value: Arc::new(value),
                queue,
                started: false,
                scheduled: false,
                running: false,
                end: None,
                listener: None,
                detach: Some(detach),
                cancellation: None,
                closed,
                report,
            }),
            retained: Mutex::new(Vec::new()),
        }
    }

    /// 待投递队列与投递任务是否都为空。
    ///
    /// `NativeWatch::advance` 在锁内入队并可能置 `scheduled`；投递任务在运行期间置
    /// `running`。只有三者都清空才算真正“排空”，此时监听器已经看到所有已提交帧。
    fn is_idle(&self) -> bool {
        let state = lock(&self.state);
        let empty = match &state.queue {
            Queue::Native { frames, .. } => frames.is_empty(),
            Queue::Folded {
                ops, newest, ready, ..
            } => ops.size() == 0 && newest.is_none() && ready.is_empty(),
        };
        empty && !state.scheduled && !state.running
    }

    /// 排程：把状态的所有权交给一个投递任务。
    ///
    /// 只有 `started`、未终止、且当前没有已排程/正在运行的任务时才置 `scheduled` 并 spawn。
    /// 标志在锁内先置位，避免短时间内多次 `advance` 重复 spawn 出多个排空任务；用 `Weak`
    /// 升级，句柄全部释放后任务空转。
    fn schedule_arc(inner: &Arc<WatchCore<T>>) {
        {
            let mut state = lock(&inner.state);
            if state.scheduled || state.running || state.end.is_some() || !state.started {
                return;
            }
            state.scheduled = true;
        }
        let weak = Arc::downgrade(inner);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Some(inner) = weak.upgrade() {
                    WatchCore::drain(inner).await;
                }
            });
        }
    }

    /// 排空任务：按队列顺序把帧逐个交给监听器。
    ///
    /// 阶段 1 认领运行权；阶段 2 每轮在锁内挑一帧并更新 `value`，随后放锁投递；阶段 3 处理
    /// 监听器 panic；阶段 4 释放运行权并按需补排一次。
    async fn drain(inner: Arc<WatchCore<T>>) {
        // 阶段 1：认领运行权。`scheduled` 由本任务消费掉；若已有别的任务在跑、watch 已终止或
        // 尚未 start，本次排程到此为止（队列里的帧由正在跑的任务或下一次排程负责）。
        {
            let mut state = lock(&inner.state);
            state.scheduled = false;
            if state.running || state.end.is_some() || !state.started {
                return;
            }
            state.running = true;
        }
        loop {
            // 阶段 2：在锁内挑选并弹出下一帧。这里只做内存操作并更新 `value`（`Folded` 需要
            // 构造根替换时还会调用调用方的 `to_json`，见 `lock` 的说明）；监听器在锁外
            // 调用，因此可以安全地重入 `value()`/`advance()`/`stop()`。
            let next = {
                let mut state = lock(&inner.state);
                if state.end.is_some() {
                    None
                } else {
                    let listener = state.listener.clone();
                    match listener {
                        None => None,
                        Some(listener) => {
                            let delivery = match &mut state.queue {
                                Queue::Folded {
                                    ops,
                                    newest,
                                    ready,
                                    to_json,
                                } => {
                                    // `start()` 之前折叠的那一帧先交；之后按捕获顺序逐帧投递。
                                    if let Some((value, context)) = newest.take() {
                                        let batch = ops.take();
                                        let ops = match batch {
                                            PendingTake::Ops(ops) => ops,
                                            PendingTake::Replace => {
                                                vec![Op::Replace(to_json(&*value))]
                                            }
                                        };
                                        Some((listener, value, ops, context))
                                    } else {
                                        ready.pop_front().map(|frame| {
                                            (listener, frame.value, frame.ops, frame.context)
                                        })
                                    }
                                }
                                Queue::Native { frames, .. } => frames
                                    .pop_front()
                                    .map(|(value, context)| (listener, value, Vec::new(), context)),
                            };
                            match delivery {
                                None => None,
                                Some((listener, value, ops, context)) => {
                                    // `value` 反映最近一次送达的修订；已捕获但未投递的帧不影响
                                    // 它，所以 `value()` 不会越过监听器"提前"看到新值。
                                    state.value = Arc::clone(&value);
                                    Some((listener, value, ops, context))
                                }
                            }
                        }
                    }
                }
            };
            let Some((listener, value, ops, context)) = next else {
                break;
            };
            // 阶段 3：投递。投递上下文去掉中止信号（监听器不能把获取 watch 的取消信号当成
            // 自己的取消来源）；监听器 panic 按抛错处理——只结束这一个 watch，
            // 报告一次并执行 detach，其他 watch 不受影响。
            let delivery_context: Arc<dyn Context> = without_abort_signal(context);
            let outcome = std::panic::AssertUnwindSafe(listener.call(
                (*value).clone(),
                ops,
                delivery_context,
            ))
            .catch_unwind()
            .await;
            if let Err(payload) = outcome {
                let error = panic_error(payload);
                let mut state = lock(&inner.state);
                // 若终止已被 stop/cancel/end 抢先，则不覆盖既有终态也不再报告。
                if state.end.is_none() {
                    let report = state.report.clone();
                    state.end = Some(WatchEndKind::ListenerError(error.to_string()));
                    state.listener = None;
                    state.cancellation = None;
                    let detach = state.detach.take();
                    state
                        .closed
                        .send_replace(Some(WatchEndKind::ListenerError(error.to_string())));
                    drop(state);
                    if let Some(report) = report {
                        report(error);
                    }
                    if let Some(detach) = detach {
                        detach();
                    }
                    Self::release_queue(&inner);
                }
                break;
            }
        }
        // 阶段 4：释放运行权。监听器运行期间到达的帧没有别的排程者（此时 `running` 为真），
        // 所以退出前若队列非空必须补排一次，否则这些帧会一直躺着。
        let mut state = lock(&inner.state);
        state.running = false;
        let reschedule = state.end.is_none() && state.started && {
            match &state.queue {
                Queue::Folded { newest, ready, .. } => newest.is_some() || !ready.is_empty(),
                Queue::Native { frames, .. } => !frames.is_empty(),
            }
        };
        drop(state);
        if reschedule {
            Self::schedule_arc(&inner);
        }
    }

    /// 终止或监听器 panic 后清空待投递帧，避免 `Arc` 值与上下文滞留。调用方不得持有 `state`
    /// 锁（本函数自己加锁）；`terminate` 里有一段等价的内联清理，因为它已经持有锁。
    fn release_queue(inner: &Arc<WatchCore<T>>) {
        let mut state = lock(&inner.state);
        match &mut state.queue {
            Queue::Folded {
                ops, newest, ready, ..
            } => {
                let _ = ops.take();
                *newest = None;
                ready.clear();
            }
            Queue::Native { frames, .. } => frames.clear(),
        }
    }

    /// 幂等终止：先到者胜。
    ///
    /// 置 `end`、丢弃监听器与取消订阅、清空队列、向 `closed` 广播终态，并执行一次 `detach`
    /// （`Option::take` 保证至多一次）。不等待正在执行的监听器调用返回；该调用结束后
    /// `drain` 会在下一轮循环条件处发现终止并退出。
    fn terminate(&self, end: WatchEndKind) {
        let mut state = lock(&self.state);
        if state.end.is_some() {
            return;
        }
        state.end = Some(end.clone());
        state.listener = None;
        state.cancellation = None;
        match &mut state.queue {
            Queue::Folded {
                ops, newest, ready, ..
            } => {
                let _ = ops.take();
                *newest = None;
                ready.clear();
            }
            Queue::Native { frames, .. } => frames.clear(),
        }
        let detach = state.detach.take();
        state.closed.send_replace(Some(end));
        drop(state);
        if let Some(detach) = detach {
            detach();
        }
    }

    /// 获取 watch 的上下文被取消（终态 `Cancelled`，区别于 `end(None)` 的 `Ended`）。
    fn cancel(&self) {
        self.terminate(WatchEndKind::Cancelled);
    }

    /// 源结束：`None` 是正常关闭（`Ended`），`Some` 是失败（只保留 `Failure::message()` 的
    /// 文本，重建为 `Failed` 时错误类型是 `ReportedError`）。
    fn end(&self, failure: Option<Failure>) {
        match failure {
            Some(failure) => self.terminate(WatchEndKind::Failed(failure.message().to_owned())),
            None => self.terminate(WatchEndKind::Ended),
        }
    }

    /// 宿主显式停止（`stop()`）：终态 `Stopped`；对已终止的 watch 是空操作。
    fn stop_handle(&self) {
        self.terminate(WatchEndKind::Stopped);
    }

    /// 等终态。先订阅再读当前值，因此终止发生在调用之前也能立即返回；发送端消失（防御分支）
    /// 时按 `Ended` 处理。
    async fn closed(&self) -> WatchEnd {
        let mut receiver = lock(&self.state).closed.subscribe();
        loop {
            if let Some(end) = receiver.borrow().clone() {
                return end.to_end();
            }
            if receiver.changed().await.is_err() {
                return WatchEnd::Ended;
            }
        }
    }

    /// 当前（最近送达的）值，并把它的 `Arc` 记进 `retained`，让 `value()` 能安全返回 `&T`
    /// 直到 `&self` 借用结束。
    ///
    /// 去重只比较 `retained` 的最后一个元素，所以反复取同一个值不会增长列表；但每次取到不同
    /// 的修订都会新增一项，而列表从不删除——它随 `WatchCore` 一起释放。这是 `value()` 能用
    /// 裸指针（见两个 `WatchHandle::value` 的 SAFETY 注释）的原因，也是长期存活的 watch 在
    /// 反复 `value()` 后会多保留若干历史修订的原因。
    fn current(&self) -> Arc<T> {
        let value = Arc::clone(&lock(&self.state).value);
        {
            let mut retained = lock(&self.retained);
            if !retained
                .last()
                .is_some_and(|held| Arc::ptr_eq(held, &value))
            {
                retained.push(Arc::clone(&value));
            }
        }
        value
    }

    /// 安装唯一监听器并把 `started` 置真；重复 start 或已终止时 panic（TS 的
    /// `Watch is already started` / `Watch is stopped`），不会静默替换或忽略。
    fn start_listener(&self, listener: Arc<dyn WatchListener<T>>) {
        {
            let mut state = lock(&self.state);
            if state.started {
                panic!("Watch is already started");
            }
            if state.end.is_some() {
                panic!("Watch is stopped");
            }
            state.started = true;
            state.listener = Some(listener);
        }
    }
}

/// 一个需要 `Arc` 自身才能安装取消监听的 watch 句柄。
///
/// `observe_cancellation` 的监听器需要回指 watch 以终止它，因此公开的句柄都是 `Arc<Self>`。
///
/// 顺序是"先登记监听器、再排程"：排程要求 `started`，所以必须先 start；若 watch 已终止，
/// `start_listener` 直接 panic，不会静默忽略。
pub(crate) fn start_watch<T: Clone + Send + Sync + 'static>(
    watch: &Arc<WatchCore<T>>,
    listener: Arc<dyn WatchListener<T>>,
) {
    watch.start_listener(listener);
    WatchCore::schedule_arc(watch);
}

/// 观察获取上下文的取消（TS: `watch.observeCancellation(signal)`）。
///
/// 只能安装一次，第二次调用 panic（`Watch cancellation is already installed`）。已终止的 watch
/// 直接返回，不安装订阅。`AbortSignal::on_abort` 与 DOM 一样不会对已中止的信号补发回调，因此
/// 安装之后再查一次 `is_aborted()` 并立即 `cancel()`；订阅保存在 watch 的状态里，随终止
/// （`terminate` 清空 `cancellation`）或 watch 释放而移除。回调只持有 `Weak`，不延长 watch
/// 的生命周期。
pub(crate) fn observe_watch_cancellation<T: Clone + Send + Sync + 'static>(
    watch: &Arc<WatchCore<T>>,
    signal: &kiana_chord::context::AbortSignal,
) {
    {
        let state = lock(&watch.state);
        if state.cancellation.is_some() {
            panic!("Watch cancellation is already installed");
        }
        if state.end.is_some() {
            return;
        }
    }
    let weak = Arc::downgrade(watch);
    let subscription = signal.on_abort(move |_reason| {
        if let Some(watch) = weak.upgrade() {
            watch.cancel();
        }
    });
    {
        let mut state = lock(&watch.state);
        if state.end.is_some() {
            return;
        }
        state.cancellation = Some(subscription);
    }
    if signal.is_aborted() {
        watch.cancel();
    }
}

/// JSON 值的 watch（TS: chord 的 `Watch<T>`）：折叠 op，投递最新值。
///
/// # 投递语义
/// - `start()` 之前的帧由 `PendingOps` 折叠成一次投递：ops 从初始值直达最新值。
/// - `start()` 之后每次 `advance` 捕获一帧，按捕获顺序逐帧投递；不超过积压上限时不合并，
///   监听器空闲时每个推进恰好一次调用（`tests/harness_view.rs` 对帧数与 op 重放有断言）。
/// - 已捕获的帧超过 `MAX_PENDING_JSON_FRAMES` 时，积压被清空并替换成一帧根替换。
///
/// `value()` 返回初始值或最近一次**送达**的值。`start` 只能调用一次（重复或终止后 panic），
/// `stop` 幂等；两者都不等待正在执行的监听器。
pub(crate) struct JsonWatch<T: Clone + Send + Sync + 'static> {
    core: Arc<WatchCore<T>>,
}

impl<T: Clone + Send + Sync + 'static> JsonWatch<T> {
    /// 新建；`to_json` 用于溢出时构造根替换。
    ///
    /// 参数：`value` 是初始修订（`start()` 之前 `value()` 返回它）；`detach` 在终止时执行
    /// 一次（通常把观察者从挂载上注销）；`report` 接收监听器 panic；`to_json` 把当前值序列化
    /// 成根替换的 payload。返回 `Arc<Self>`：调用方把它作为共享句柄保存（本文件的
    /// `ConversationWatch` 就是 `Arc<dyn WatchHandle<...>>`）。
    pub fn new(
        value: T,
        detach: Box<dyn FnOnce() + Send + Sync>,
        report: Option<WatchReport>,
        to_json: Arc<dyn Fn(&T) -> serde_json::Value + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(JsonWatch {
            core: Arc::new(WatchCore::new(
                value,
                Queue::Folded {
                    ops: PendingOps::new(),
                    newest: None,
                    ready: VecDeque::new(),
                    to_json,
                },
                detach,
                report,
            )),
        })
    }

    /// 排入一帧：`ops` 把上一版变成 `value`。
    ///
    /// `start()` 之前到达的帧折叠成一次投递（TS 的 `PendingOps`）；之后每一帧各自排队，串行
    /// 投递——监听器空闲时每个提交恰好一次调用，即使投递任务被调度器推迟也不会把相邻提交
    /// 合并。已捕获的帧超过 [`MAX_PENDING_JSON_FRAMES`] 时折叠成一次根替换。
    ///
    /// 三种分支：
    /// 1. 未 start：`ops` 交给 `PendingOps` 折叠，`newest` 只保留最新值；
    /// 2. 已 start 且 `ready` 未满：按原 ops 入队，投递时原样交给监听器；
    /// 3. 已 start 且 `ready` 已满：清空积压，改投一帧 `Op::Replace(to_json(&value))`。
    ///
    /// 已终止的 watch 直接忽略新帧；入队后若没有排程中的任务就唤醒一个。
    pub fn advance(&self, value: T, ops: &[Op], context: ContextRef) {
        let mut state = lock(&self.core.state);
        if state.end.is_some() {
            return;
        }
        let started = state.started;
        match &mut state.queue {
            Queue::Folded {
                ops: pending,
                newest,
                ready,
                to_json,
            } => {
                if !started {
                    pending.push(ops);
                    *newest = Some((Arc::new(value), context));
                } else if ready.len() >= MAX_PENDING_JSON_FRAMES {
                    ready.clear();
                    ready.push_back(FoldedFrame {
                        value: Arc::new(value.clone()),
                        ops: vec![Op::Replace(to_json(&value))],
                        context,
                    });
                } else {
                    ready.push_back(FoldedFrame {
                        value: Arc::new(value),
                        ops: ops.to_vec(),
                        context,
                    });
                }
            }
            Queue::Native { .. } => unreachable!(),
        }
        if started && !state.scheduled && !state.running {
            drop(state);
            WatchCore::schedule_arc(&self.core);
        }
    }

    /// 取消（获取上下文被取消）：终态 `Cancelled`，区别于 `end(None)` 的 `Ended`。
    pub fn cancel(&self) {
        self.core.cancel();
    }

    /// 源结束：`None` 是正常关闭（`Ended`），`Some` 是失败（文本来自 `Failure::message()`）。
    pub fn end(&self, failure: Option<Failure>) {
        self.core.end(failure);
    }

    /// 观察获取上下文的取消；只能安装一次，已终止的 watch 是空操作（见
    /// `observe_watch_cancellation`）。
    pub fn observe_cancellation(&self, signal: &kiana_chord::context::AbortSignal) {
        observe_watch_cancellation(&self.core, signal);
    }

    /// 安装唯一监听器；重复调用或已终止时 panic（`Watch is already started` /
    /// `Watch is stopped`）。
    pub fn start(&self, listener: Arc<dyn WatchListener<T>>) {
        start_watch(&self.core, listener);
    }

    /// 幂等停止，返回既有终态：本次调用真正触发终止时是 `Stopped`；对已终止的 watch 返回
    /// 既有终态（`Failed`/`ListenerError`/`Cancelled`/`Ended`）；不等待正在执行的监听器调用。
    pub async fn stop(&self) -> WatchEnd {
        self.core.stop_handle();
        self.core.closed().await
    }

    /// 终止时落定：等 `stop`/`cancel`/`end`/监听器 panic 任一发生；可重复调用。
    pub async fn closed(&self) -> WatchEnd {
        self.core.closed().await
    }
}

#[async_trait]
impl<T: Clone + Send + Sync + 'static> WatchHandle<T> for JsonWatch<T> {
    fn value(&self) -> &T {
        let value = self.core.current();
        let pointer = Arc::as_ptr(&value);
        // SAFETY: `Arc` 的堆分配地址稳定，且同一个 `Arc` 已保存在句柄的 `retained` 里；返回的
        // 引用生命周期绑定到 `&self`，不会比保留更强（与 session 的文档 watch 同一手法）。
        // `retained` 在本 watch 的整个生命周期内不清空（见 `WatchCore::current`），而 `&self`
        // 保证 `core` 与它的保留列表都还活着，因此指针在借用期内始终有效。
        unsafe { &*pointer }
    }

    fn start(&self, listener: Arc<dyn WatchListener<T>>) {
        JsonWatch::start(self, listener);
    }

    async fn stop(&self) -> WatchEnd {
        JsonWatch::stop(self).await
    }

    async fn closed(&self) -> WatchEnd {
        JsonWatch::closed(self).await
    }

    fn is_idle(&self) -> bool {
        self.core.is_idle()
    }
}

/// 原生值的 watch（TS: chord 的 `NativeWatch<T>`）：每帧一次调用，超限时以替换值折叠。
///
/// 与 `JsonWatch` 不同，`start()` 之前到达的帧也逐帧排队（不折叠）：监听器启动后按顺序收到
/// 它们（`tests/harness_events.rs` 的 `delivers_each_of_100_pending_batches_in_its_own_call_in_order`
/// 对 100 帧的顺序有断言），只有积压达到 `MAX_PENDING_NATIVE_VALUES` 时才丢弃全部积压并只留下
/// 一个替换帧。`replace` 由调用方提供（`harness/events.rs` 用它构造一次当前视图快照）；为
/// `None` 时替换帧就是触发溢出的那个新值。
pub(crate) struct NativeWatch<T: Clone + Send + Sync + 'static> {
    core: Arc<WatchCore<T>>,
}

impl<T: Clone + Send + Sync + 'static> NativeWatch<T> {
    /// 新建；`replace` 是溢出时构造替换值的函数。
    ///
    /// 其余参数与 `JsonWatch::new` 同义：`value` 是初始值，`detach` 在终止时执行一次，
    /// `report` 接收监听器 panic。`replace` 为 `None` 时溢出帧使用触发溢出的新值本身。
    pub fn new(
        value: T,
        detach: Box<dyn FnOnce() + Send + Sync>,
        report: Option<WatchReport>,
        replace: Option<Arc<dyn Fn() -> T + Send + Sync>>,
    ) -> Arc<Self> {
        Arc::new(NativeWatch {
            core: Arc::new(WatchCore::new(
                value,
                Queue::Native {
                    frames: VecDeque::new(),
                    replace,
                    limit: MAX_PENDING_NATIVE_VALUES,
                },
                detach,
                report,
            )),
        })
    }

    /// 排入一帧。
    ///
    /// 未满上限时追加到队尾；已满时清空积压并只留下一个替换帧（`replace` 的返回值，或没有
    /// `replace` 时用新值），因此内存里至多保留 100 帧。与 `JsonWatch` 一样，未 start 的帧
    /// 也会入队（但不折叠），已终止的 watch 忽略新帧。
    pub fn advance(&self, value: T, context: ContextRef) {
        {
            let mut state = lock(&self.core.state);
            if state.end.is_some() {
                return;
            }
            let arc = Arc::new(value);
            match &mut state.queue {
                Queue::Native {
                    frames,
                    replace,
                    limit,
                } => {
                    if frames.len() < *limit {
                        frames.push_back((arc, context));
                    } else {
                        frames.clear();
                        let replacement = match replace {
                            Some(replace) => Arc::new(replace()),
                            None => arc,
                        };
                        frames.push_back((replacement, context));
                    }
                }
                Queue::Folded { .. } => unreachable!(),
            }
            if state.started && !state.scheduled && !state.running {
                drop(state);
                WatchCore::schedule_arc(&self.core);
            }
        }
    }

    /// 取消（获取上下文被取消）：终态 `Cancelled`。
    pub fn cancel(&self) {
        self.core.cancel();
    }

    /// 源结束（关闭或失败）：`None` → `Ended`，`Some` → `Failed`。
    pub fn end(&self, failure: Option<Failure>) {
        self.core.end(failure);
    }

    /// 观察获取上下文的取消；只能安装一次，已终止的 watch 是空操作。
    pub fn observe_cancellation(&self, signal: &kiana_chord::context::AbortSignal) {
        observe_watch_cancellation(&self.core, signal);
    }

    /// 安装唯一监听器；重复调用或已终止时 panic（`Watch is already started` /
    /// `Watch is stopped`）。
    pub fn start(&self, listener: Arc<dyn WatchListener<T>>) {
        start_watch(&self.core, listener);
    }

    /// 幂等停止，返回既有终态：本次调用真正触发终止时是 `Stopped`；对已终止的 watch 返回
    /// 既有终态（`Failed`/`ListenerError`/`Cancelled`/`Ended`）；不等待正在执行的监听器调用。
    pub async fn stop(&self) -> WatchEnd {
        self.core.stop_handle();
        self.core.closed().await
    }

    /// 终止时落定；可重复调用。
    pub async fn closed(&self) -> WatchEnd {
        self.core.closed().await
    }

    /// 是否已排空：没有待投递帧，也没有排程中/运行中的投递任务。
    ///
    /// 宿主/测试用它在“提交已落定”之后等待异步 watch 回调真正赶上，而不是 sleep 固定
    /// 毫秒数（后者在慢机器上不稳定）。
    pub fn is_idle(&self) -> bool {
        self.core.is_idle()
    }
}

#[async_trait]
impl<T: Clone + Send + Sync + 'static> WatchHandle<T> for NativeWatch<T> {
    fn value(&self) -> &T {
        let value = self.core.current();
        let pointer = Arc::as_ptr(&value);
        // SAFETY: 同上（`retained` 持有同一个 `Arc`）。
        unsafe { &*pointer }
    }

    fn start(&self, listener: Arc<dyn WatchListener<T>>) {
        NativeWatch::start(self, listener);
    }

    async fn stop(&self) -> WatchEnd {
        NativeWatch::stop(self).await
    }

    async fn closed(&self) -> WatchEnd {
        NativeWatch::closed(self).await
    }

    fn is_idle(&self) -> bool {
        NativeWatch::is_idle(self)
    }
}

// ---------------------------------------------------------------------------
// ConversationViews
// ---------------------------------------------------------------------------

/// 接收每次挂载修订与 Session 的关闭（TS: `ViewObserver`）。
///
/// 三个回调都由 [`ConversationViews`] 在 Session 的内部监听器上调用：
/// - `advance` 只在本次发布改变了挂载（推导出的 op 非空）时调用，参数是新完整视图与
///   "从上一版到这一版"的 op；
/// - `publication` 对**每一次**发布都调用（op 可能为空，此时 `before == after`），参数
///   `publication` 是触发这一帧的 [`ObservedCommit`]（已存提交或只在内存里采纳的延迟提交）；
/// - `end` 在 Session 关闭（`None`）或失败（`Some`）时调用一次。
///
/// 同一帧里先对所有观察者调用 `advance`，再对所有观察者调用 `publication`；观察者按注册顺序
/// 排列。回调都在挂载的 `Mutex` 之外执行，单个实现 panic 会被 `contained` 捕获并交给报告
/// 通道，其他观察者仍会收到同一帧——因此不要指望自己的异常能中断其他观察者或让 Session
/// 失败。`context` 是发布上下文去掉中止信号后的副本，不能当作观察者自己的取消来源。
///
/// `advance` 是同步函数，运行在 Session 的发布路径上；它阻塞多久，后续提交的发布就被推迟
/// 多久。
pub trait ViewObserver: Send + Sync {
    /// 每次挂载推进后调用；`ops` 把上一版变成 `value`。
    ///
    /// 只有 op 非空的发布才会调用；`value` 是推进后的完整视图。
    fn advance(&self, _value: &ConversationView, _ops: &[Op], _context: &ContextRef) {}

    /// 每次发布之后调用；`ops` 是本次挂载的 op（可能为空）。
    ///
    /// 与 `advance` 不同，即使本次发布与本会话无关（op 为空、`before == after`）也会调用，
    /// 因此适合做"按提交推进"的逻辑；不要在 `advance` 里做这件事。
    fn publication(
        &self,
        _before: &ConversationView,
        _after: &ConversationView,
        _ops: &[Op],
        _publication: &ObservedCommit,
        _context: &ContextRef,
    ) {
    }

    /// Session 关闭或以 `failure` 失败。
    ///
    /// 由关闭监听器对每个已挂载的观察者调用一次；`failure` 为 `None` 表示正常关闭。此回调
    /// panic 也只会被报告，不会阻止其他观察者收到 `end`。
    fn end(&self, failure: Option<&Failure>);
}

/// 挂载的注销函数（TS: `detach`）。可重复调用（移除已不存在的观察者是空操作）。
///
/// 注销最后一个观察者时挂载本身会从 [`ConversationViews`] 的表中移除（仅当映射里仍是同一个
/// 挂载时），下一次观察重新从已提交状态构建。调用方负责在观察者生命周期结束时调用它；
/// `ConversationViews::watch` 会把它装进 watch 的终止回调，watch 终止时自动执行。
pub type Detach = Arc<dyn Fn() + Send + Sync>;

/// 一次挂载：当前修订、它显示的文档化身，以及观察者。
///
/// 一个挂载对应一个会话，由第一个观察者构建、最后一个观察者注销时丢弃。内部状态共用一把
/// `Mutex`：推进与观察者列表的增删互斥，观察者回调在锁外执行。
struct Mount {
    state: Mutex<MountState>,
}

/// 挂载的可变部分。
///
/// 不变量：`docs` 里的 kind 与 `value.docs` 的键一致（构建时一起填充，推进时同步删除/新增）。
/// `docs` 记录每个 kind 当前显示的化身 `(id, version)`，用来判断一次文档变化是同一化身的
/// 增量更新（应用 op）还是换化身/换定义版本（整体 `Set`）。
struct MountState {
    value: ConversationView,
    /// 每个 kind 的已挂载化身与定义版本；换化身或换版本都整体 set。
    docs: IndexMap<String, MountedIncarnation>,
    /// 观察者，按注册顺序排列（推进与 `end` 都按这个顺序回调）。
    observers: IndexMap<u64, Arc<dyn ViewObserver>>,
    /// 观察者 ID 的下一个值；注销只删除条目，不复用 ID。
    next_observer_id: u64,
}

/// 一个 kind 当前挂载的文档化身与它的定义版本。
///
/// `version` 来自 `DocumentCommitChange::Document::version`（即 `LoadedDocument::value_version`，
/// 表示值形状对应的定义版本）或构建挂载时的返回值；版本不同意味着不能沿用旧形状上的增量 op。
#[derive(Clone, Copy)]
struct MountedIncarnation {
    id: DocumentId,
    version: i64,
}

/// Harness 的会话视图挂载：每个会话至多一个，由第一个观察者在 Session 线上构建，随最后一个观察者
/// 消失（TS: `ConversationViews`）。
///
/// 生命周期：由 `harness/harness.rs` 创建并作为 `HarnessImpl` 的字段持有；Harness 构造时把
/// **Harness 的对象地址**与一个 `Weak<ConversationViews>` 登记进 `VIEWS`。本对象构造时注册
/// 两个 Session 监听器（提交、关闭），字段里保留它们的注销句柄；注意 `Unsubscribe` 是
/// `Box<dyn FnOnce()>`，丢弃句柄不会执行注销，真正的清理在 Session 关闭时（关闭任务清空
/// 内部监听器表、关闭监听器已被取走）。
///
/// # 并发
/// `mounts` 与每个挂载的 `state` 各有一把 `std::sync::Mutex`；内部监听器先在 `mounts` 锁内
/// 拷贝一份挂载列表，释放锁后再逐个推进，观察者回调则在挂载锁之外执行。
pub struct ConversationViews {
    /// 在变更线上注册/构建挂载与读取文档；内部监听器持有它的克隆，推进失败时用克隆让
    /// Session 失败。
    session: SessionImpl,
    /// 构建挂载时读会话记录与活动条目。
    storage: Arc<dyn Storage>,
    /// 按会话的挂载表；迭代顺序即挂载创建顺序。
    mounts: Arc<Mutex<IndexMap<ConversationId, Arc<Mount>>>>,
    /// 关闭监听器先置位；`attach` 在 `create` 之后复查它。复查只能缩小竞态窗口：关闭监听器
    /// 在调用 `close()`/`fail()` 的线程上同步执行（可能是其他线程，不受变更线串行化），
    /// 检查与登记之间仍可能被关闭抢先，因此它不构成“关闭开始后一定没有新观察者”的保证。
    closed: Arc<AtomicBool>,
    /// 观察者/监听器错误的报告通道（Harness 的 listener-error 报告）。
    report: Arc<dyn Fn(BoxError) + Send + Sync>,
    /// 提交监听器的注销句柄；只作为字段保留（见类型文档：丢弃不会执行注销）。
    _commit_unsubscribe: crate::types::Unsubscribe,
    /// 关闭监听器的注销句柄；同上。
    _close_unsubscribe: crate::types::Unsubscribe,
}

impl ConversationViews {
    /// 新建并挂上 Session 的提交观察与关闭观察。
    ///
    /// 顺序：先注册提交内部监听器，再注册关闭监听器。两个注册都可能因 Session 已关闭或已失败
    /// 返回错误；若第二个失败，第一个监听器不会被显式注销（`Unsubscribe` 丢弃不执行），但此时
    /// Session 已进入关闭流程，正在运行的关闭任务会清空内部监听器表。
    ///
    /// 注册的两个监听器：
    /// - 提交监听器遍历挂载快照，对每个挂载调用私有的 `advance`；某个挂载推导 op 失败时调用
    ///   `SessionImpl::fail_with_context` 并停止处理其余挂载（对应 TS 的"抛出使 Session 失败、
    ///   后续挂载不再推进"）。该监听器整体还被 Session 用 `contained` 包裹，因此它内部的
    ///   panic 同样会让 Session 失败。
    /// - 关闭监听器先置 `closed`，再对每个挂载的每个观察者调用 `end`（每个调用单独 `contained`，
    ///   panic 只报告），最后清空挂载表。
    ///
    /// `report` 是观察者/监听器错误的报告通道，随本对象一直保留。
    pub fn new(
        session: SessionImpl,
        storage: Arc<dyn Storage>,
        report: Arc<dyn Fn(BoxError) + Send + Sync>,
    ) -> Result<Self, BoxError> {
        let mounts: Arc<Mutex<IndexMap<ConversationId, Arc<Mount>>>> =
            Arc::new(Mutex::new(IndexMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let listener_mounts = Arc::clone(&mounts);
        let listener_report = Arc::clone(&report);
        let listener_session = session.clone();
        let listener: crate::session::session::InternalListener =
            Arc::new(move |publication, context| {
                let snapshot: Vec<(ConversationId, Arc<Mount>)> = listener_mounts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .iter()
                    .map(|(id, mount)| (*id, Arc::clone(mount)))
                    .collect();
                for (id, mount) in snapshot {
                    // TS：`applyImmutable` 抛出时整个内部监听器抛出，Session 以该错误失败，
                    // 其余挂载不再推进。
                    if let Err(error) = advance(id, &mount, publication, context, &listener_report)
                    {
                        listener_session.fail_with_context(error, &**context);
                        break;
                    }
                }
            });
        let commit_unsubscribe = session.observe_commits(listener)?;
        let close_mounts = Arc::clone(&mounts);
        let close_closed = Arc::clone(&closed);
        let close_session = session.clone();
        let close_report = Arc::clone(&report);
        let close_unsubscribe = session.subscribe_close(Arc::new(move || {
            close_closed.store(true, Ordering::SeqCst);
            let failure = close_session.failure();
            let mounts: Vec<Arc<Mount>> = close_mounts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .values()
                .cloned()
                .collect();
            for mount in mounts {
                let observers: Vec<Arc<dyn ViewObserver>> = mount
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .observers
                    .values()
                    .cloned()
                    .collect();
                for observer in observers {
                    let report = Arc::clone(&close_report);
                    contained(
                        || {
                            observer.end(failure.as_ref());
                        },
                        move |error| report(error),
                    );
                }
            }
            close_mounts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        }))?;
        Ok(ConversationViews {
            session,
            storage,
            mounts,
            closed,
            report,
            _commit_unsubscribe: commit_unsubscribe,
            _close_unsubscribe: close_unsubscribe,
        })
    }

    /// 通过 Harness 报告观察者的监听器错误。
    pub fn report(&self, error: BoxError) {
        (self.report)(error);
    }

    /// 视图的串行 watch；取消 `context` 会停止它。
    ///
    /// 成功时 watch 的 `value()` 是注册那一刻的完整视图，之后每次改变该会话挂载的发布会作为
    /// 一帧送达（`ConversationWatch` 即 `Arc<dyn WatchHandle<ConversationView>>`）。`_detach`
    /// 在这里被丢弃是安全的：`create` 已把同一个注销句柄装进 `JsonWatch` 的终止回调。
    ///
    /// 错误：
    /// - 构建挂载失败（会话不存在、读条目/文档失败）或 `create` 失败，原样返回；
    /// - `context` 在注册期间被取消：先 `cancel()` watch（同时注销观察者）再返回 `AbortError`；
    /// - Session 在注册期间关闭：返回关闭错误（无失败时是 `Harness is closed`，Session 已失败
    ///   时是失败错误）。
    ///
    /// 注册成功后把 `context` 的取消信号安装到 watch：取消让 watch 以 `Cancelled` 落定并注销
    /// 观察者。
    pub async fn watch(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<crate::harness::types::ConversationWatch, BoxError> {
        let watch_report = Arc::clone(&self.report);
        let (observer, _detach) = self
            .attach::<ConversationWatchObserver, _>(
                id,
                move |value, release, _storage| {
                    Box::pin(async move {
                        let watch = JsonWatch::new(
                            value,
                            Box::new(move || release()),
                            Some(watch_report),
                            Arc::new(conversation_view_to_json),
                        );
                        Ok(ConversationWatchObserver { watch })
                    })
                },
                context,
            )
            .await?;
        if let Some(signal) = context.abort_signal()
            && signal.is_aborted()
        {
            observer.watch.cancel();
            return Err(Box::new(abort_error(&signal)));
        }
        if let Some(signal) = context.abort_signal() {
            observer.watch.observe_cancellation(&signal);
        }
        Ok(Arc::clone(&observer.watch) as crate::harness::types::ConversationWatch)
    }

    /// 在 Session 线上原子地注册一个从当前修订构造的观察者：它只看到之后的发布，看不到之前的。
    ///
    /// # 参数
    /// - `id`：要挂载的会话。挂载不存在时在**变更线上**构建（读会话记录、活动条目与五个内建
    ///   文档）；构建失败直接返回错误，且不会把半成品挂载登记进表。
    /// - `create`：拿到当前视图、注销句柄与 `Storage` 后构造观察者。它在变更线上被 await，
    ///   期间没有其他提交或挂载构建插队。但传入的 `Storage` 只是 `GuardedStorage`，读写直接
    ///   委托后端，只反映已持久化状态；初始视图里的文档来自 Session 的内存 tracker
    ///   （`conversation_document_on_line`），可能包含尚未持久化的 held 变更（只有纯文档事务
    ///   能被延迟采纳）。因此文档要经 Session 读取才能看到内存里已采纳的延迟提交，直接读
    ///   `Storage` 可能得到更旧的值；会话、条目与任务等表记录不受影响。
    /// - `context`：用于取 `ContextRef`（构建挂载与 `create` 里的存储读取都用这份上下文），
    ///   并在 `create` 完成后检查它是否已取消；是否把取消信号装到调用方的 watch 上由调用方
    ///   决定（`watch` 就是这么做的）。
    ///
    /// # 返回
    /// `(observer, detach)`。`detach` 可重复调用：移除该观察者，若它是最后一个则把挂载从表里
    /// 移除（仅当表里仍是同一个挂载时，避免删掉并发重建的新挂载）。`observer` 是 `Arc`，
    /// 调用方可以自行克隆保存。
    ///
    /// # 错误与竞态
    /// - `create` 返回错误：不登记观察者，错误原样返回。
    /// - `create` 完成后发现 `closed` 已置位（Session 在等待期间开始关闭）或上下文已取消：
    ///   返回关闭/取消错误，同样不登记。关闭检查放在 `create` 之后是必须的，因为这段 await
    ///   期间关闭可能已经开始。
    /// - 观察者 ID 在 `create` 之前分配并自增；即使后续失败，ID 也不会回退（只影响编号，
    ///   不影响回调顺序）。
    pub async fn attach<O, F>(
        &self,
        id: ConversationId,
        create: F,
        context: &dyn Context,
    ) -> Result<(Arc<O>, Detach), BoxError>
    where
        O: ViewObserver + 'static,
        F: FnOnce(
                ConversationView,
                Detach,
                Arc<dyn Storage>,
            ) -> BoxFuture<'static, Result<O, BoxError>>
            + Send,
    {
        let storage = Arc::clone(&self.storage);
        let mounts = Arc::clone(&self.mounts);
        let closed = Arc::clone(&self.closed);
        let session = self.session.clone();
        let context_ref = context.context_ref();
        let report = Arc::clone(&self.report);
        self.session
            .read_on_line(async move {
                let existing = mounts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&id)
                    .cloned();
                let mount = match existing {
                    Some(mount) => mount,
                    None => {
                        let built =
                            build_mount(&session, storage.as_ref(), id, &*context_ref).await?;
                        Arc::new(Mount {
                            state: Mutex::new(built),
                        })
                    }
                };
                let observer_id = {
                    let mut state = lock(&mount.state);
                    state.next_observer_id += 1;
                    state.next_observer_id
                };
                let detach_mount = Arc::clone(&mount);
                let detach_mounts = Arc::clone(&mounts);
                let release: Detach = Arc::new(move || {
                    let empty = {
                        let mut state = lock(&detach_mount.state);
                        state.observers.shift_remove(&observer_id);
                        state.observers.is_empty()
                    };
                    if empty {
                        // 最后一个观察者离开：删除挂载，下一次观察从已提交状态重建。只在映射
                        // 仍指向这个挂载时删除，避免误删并发重建出来的新挂载。
                        let mut map = detach_mounts
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if map
                            .get(&id)
                            .is_some_and(|mounted| Arc::ptr_eq(mounted, &detach_mount))
                        {
                            map.shift_remove(&id);
                        }
                    }
                });
                // 先取初始值再 await create：这次注册仍占着变更线（没有提交能推进挂载），
                // 所以这个值就是 create 之后第一帧的"上一版"，初始值与后续 op 能无缝衔接。
                let value = lock(&mount.state).value.clone();
                let create_release = Arc::clone(&release);
                let observer = match create(value, create_release, Arc::clone(&storage)).await {
                    Ok(observer) => observer,
                    Err(error) => {
                        // create 失败：不注册任何东西（TS 的 await 抛出同样什么都不登记）。
                        let _ = report;
                        return Err(error);
                    }
                };
                // create 期间 Session 可能开始关闭：关闭监听器由 `begin_close` 在调用
                // `close()`/`fail()` 的线程上同步执行，可能来自其他线程而不受这条变更线
                // 串行化，因此可能已经完成快照并清空挂载表。这里复查 closed 只能缩小竞态
                // 窗口——检查为假之后、下面 insert 之前仍可能被关闭抢先，新观察者会漏掉
                // end；复用已有挂载时挂载本身早已在表里，尚未登记的只是这个观察者。
                if closed.load(Ordering::SeqCst) {
                    return Err(closed_error(&session));
                }
                if let Some(signal) = context_ref.abort_signal()
                    && signal.is_aborted()
                {
                    return Err(Box::new(abort_error(&signal)));
                }
                let observer = Arc::new(observer);
                mounts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(id, Arc::clone(&mount));
                lock(&mount.state)
                    .observers
                    .insert(observer_id, Arc::clone(&observer) as Arc<dyn ViewObserver>);
                Ok((observer, release))
            })
            .await
    }
}

/// 把 `ConversationView` 转成 JSON（折叠 watch 溢出时的根替换）。
///
/// 序列化失败时退化为 `Null`：`ConversationView` 的字段都能表示成 JSON 值，这个分支只是
/// 防御；但真发生时溢出帧的 op（根替换为 `null`）会与同一帧的 `value` 不一致。
pub(crate) fn conversation_view_to_json(view: &ConversationView) -> serde_json::Value {
    serde_json::to_value(view).unwrap_or(serde_json::Value::Null)
}

/// 把挂载推进转换成 `JsonWatch` 帧的观察者：每个 op 非空的发布推进一帧。
///
/// `end` 把 Session 的失败/关闭原样转成 watch 的终态（`Some(failure)` → `Failed`，
/// `None` → `Ended`）。
struct ConversationWatchObserver {
    watch: Arc<JsonWatch<ConversationView>>,
}

impl ViewObserver for ConversationWatchObserver {
    fn advance(&self, value: &ConversationView, ops: &[Op], context: &ContextRef) {
        self.watch.advance(value.clone(), ops, Arc::clone(context));
    }

    fn end(&self, failure: Option<&Failure>) {
        self.watch.end(failure.cloned());
    }
}

/// 从已提交状态构建一个挂载的初始快照（在 Session 变更线上调用）。
///
/// 阶段：
/// 1. 读会话记录；不存在时报逐字错误 `Conversation <id> does not exist`。
/// 2. `capture_context_bounds` 固定活动范围，`active_entries` 取原始活动条目（head 标记在最前，
///    其余按 id 从旧到新），不派生模型消息。
/// 3. 按 `mounted_tokens()` 的顺序逐个读五个内建会话文档；没有记录（加载返回 `None`）的 kind
///    不放进 `docs`。
///
/// `ConversationView.docs` 与 `MountState.docs`（化身表）在这里一起填充，之后必须保持键一致。
/// 任何 Storage 读取错误原样返回，不会留下部分构建的挂载。
async fn build_mount(
    session: &SessionImpl,
    storage: &dyn Storage,
    id: ConversationId,
    context: &dyn Context,
) -> Result<MountState, BoxError> {
    let Some(conversation) = storage.conversation(id, context).await? else {
        return Err(Box::new(crate::session::ReportedError(format!(
            "Conversation {id} does not exist"
        ))));
    };
    let bounds = capture_context_bounds(storage, id, context, None).await?;
    let entries = active_entries(storage, id, bounds.as_ref(), context).await?;
    let mut docs: IndexMap<String, JsonObject> = IndexMap::new();
    let mut incarnations: IndexMap<String, MountedIncarnation> = IndexMap::new();
    for token in mounted_tokens() {
        let target = DocTarget::conversation(token, id);
        let loaded = session
            .conversation_document_on_line(&target, context)
            .await?;
        let Some((record, version, value)) = loaded else {
            continue;
        };
        docs.insert(record.kind.clone(), value);
        incarnations.insert(
            record.kind.clone(),
            MountedIncarnation {
                id: record.id,
                version,
            },
        );
    }
    Ok(MountState {
        value: ConversationView {
            conversation,
            entries,
            docs,
        },
        docs: incarnations,
        observers: IndexMap::new(),
        next_observer_id: 0,
    })
}

/// 挂载的内建文档，按 TS 的 `MOUNTED` 顺序。
///
/// 顺序也决定新挂载里 `ConversationView.docs` 的插入顺序（`build_mount` 按它遍历）。这里只列
/// 单例 token，家族文档不会挂载。
pub(crate) fn mounted_tokens() -> [&'static crate::types::DocToken; 5] {
    [&AGENT_DOC, &LIVE_DOC, &INBOX_DOC, &PROVIDER_DOC, &USAGE_DOC]
}

/// 已挂载文档的 kind 集合（TS: `MOUNTED_KINDS`）。
///
/// 由 `mounted_tokens()` 一次性派生并缓存，`advance` 用它过滤文档变化；集合内容就是
/// `kiana.agent`、`kiana.live`、`kiana.inbox`、`kiana.provider`、`kiana.usage` 五个 kind。
fn mounted_kinds() -> &'static HashSet<String> {
    static KINDS: LazyLock<HashSet<String>> = LazyLock::new(|| {
        mounted_tokens()
            .into_iter()
            .map(|token| token.definition.kind.clone())
            .collect()
    });
    &KINDS
}

fn seg(key: &str) -> Seg {
    Seg::Key(key.to_owned())
}

// 条目在视图 JSON 里的形状由 `EntryRecord` 的自定义 `Serialize` 决定（含隐藏的键序提示），
// 与 TS 的持久形状一致。序列化失败时退化为 `Null`：该条目 op 仍会入队，但随后的整帧
// `from_value::<ConversationView>` 会因此失败，调用方让 Session 失败（不会静默丢条目）。
fn entry_value(entry: &crate::types::EntryRecord) -> serde_json::Value {
    serde_json::to_value(entry).unwrap_or(serde_json::Value::Null)
}

/// 由一次发布派生挂载的 op、应用它们，并把修订交给每个观察者；抛错者通过 `report` 报告，
/// 其他观察者仍然收到（TS: `advance`）。
///
/// `apply_immutable` 失败在 TS 里会从 `advance` 抛出，被 Session 内部监听器的 `contained`
/// 捕获并使整个会话失败（`fail`）；这里把错误返回给监听器做同一件事，且本帧不通知观察者。
///
/// # 阶段
/// 1. 在挂载锁内遍历发布的变化，生成两组 op，并把文档化身表 `state.docs` 同步到新状态：
///    - 条目（`TableCommitChange::Entry`）：只处理本会话。`head` 为 `None` 的条目追加到
///      `entries` 末尾；带 `head` 的条目作为新的 head 标记排在最前，并丢弃 id 小于 `head`
///      目标的非标记条目（保留目标及其后的后缀）。算法依赖 `active_entries` 给出的顺序：
///      head 标记在最前，其余按 id 升序。截断只作用于**已挂载**的条目，因此目标早于当前
///      活动范围时，结果可能与重新构建的挂载不同（测试
///      `keeps_only_mounted_entries_for_a_raw_head_write_before_the_active_range` 锁定该行为）。
///    - 文档（`DocumentCommitChange::Document`）：只处理本会话、kind 在挂载集合内、且
///      `record.key` 为 `None` 的单例。`value: None` 是退役：仅当当前挂载的化身 id 匹配时
///      删除 `docs/<kind>`。`value: Some` 时，若化身 id 与定义版本都与已挂载的一致，就把文档
///      自己的 op 用 `prefixed` 移到 `docs/<kind>` 下应用；否则（换化身、换定义版本或首次
///      出现）对该 kind 整体 `Set`，并把新化身记入 `state.docs`。版本参与比较，是因为文档的
///      增量 op 只对同一个定义版本的值形状有效。
///    - 其他表（Conversation/Task/Submission）、`DocumentCopy` 与其他会话的变化一律跳过。
/// 2. 文档 op 在前、条目 op 在后拼成一个批次；两者路径不相交（`docs/...` 与 `entries`），
///    所以拼接顺序不影响结果。批次为空时跳过应用，`before == after`。
/// 3. 非空时把整个视图序列化成 JSON、用 `apply_immutable` 应用、再反序列化回
///    `ConversationView`。这是类型化视图与 JSON 级 delta 之间的桥；任何应用或反序列化错误
///    都原样返回，调用方据此让 Session 失败。
/// 4. 在锁外通知观察者：先对每个观察者调用 `advance`（仅非空 op），再对每个观察者调用
///    `publication`（每帧都调用）。每次调用都包在 `contained` 里，单个 panic 只报告、不影响
///    其他观察者；观察者列表在这一步开始时拷贝一次，本帧内不变。
fn advance(
    id: ConversationId,
    mount: &Mount,
    publication: &ObservedCommit,
    context: &ContextRef,
    report: &Arc<dyn Fn(BoxError) + Send + Sync>,
) -> Result<(), BoxError> {
    // 阶段 1-3 在挂载锁内完成：推导 op、更新文档化身表、应用并反序列化。锁只保护这段同步
    // 内存操作；下面的观察者回调一律在锁外，所以回调里调用 `watch.advance` 不会自锁。
    let (before, after, ops) = {
        let mut state = lock(&mount.state);
        let before = state.value.clone();
        let mut doc_ops: Vec<Op> = Vec::new();
        let mut entry_ops: Vec<Op> = Vec::new();
        let mut entries: Vec<crate::types::EntryRecord> = state.value.entries.clone();
        let kinds = mounted_kinds();
        for change in publication.changes() {
            match change {
                CommitChange::Table(TableCommitChange::Entry { value: entry }) => {
                    if entry.conversation_id != id {
                        continue;
                    }
                    match entry.head {
                        None => {
                            entry_ops.push(Op::Splice {
                                path: vec![seg("entries")],
                                index: entries.len(),
                                remove: 0,
                                items: vec![entry_value(&entry)],
                            });
                            entries.push(entry);
                        }
                        Some(target) => {
                            // head marker 放在最前，保留从 head 起的非 head 后缀。
                            // `kept` 是第一个 id >= target 的**非标记**条目下标：它之前的条目
                            // 被丢弃（多数比目标更旧，旧 head 标记也在其中），标记条目本身
                            // 由新条目替换。缓存里没有匹配项时全部丢弃（`unwrap_or(len)`）。
                            // 索引 0 的 splice 同时完成"删旧前缀 + 插入新 head"。注意这只操作
                            // 已挂载的条目：目标早于当前活动范围时，不会把更早的历史条目补回来。
                            let kept = entries
                                .iter()
                                .position(|candidate| {
                                    candidate.head.is_none() && candidate.id >= target
                                })
                                .unwrap_or(entries.len());
                            entry_ops.push(Op::Splice {
                                path: vec![seg("entries")],
                                index: 0,
                                remove: kept,
                                items: vec![entry_value(&entry)],
                            });
                            let mut next = vec![entry];
                            next.extend(entries.into_iter().skip(kept));
                            entries = next;
                        }
                    }
                }
                CommitChange::Document(change) => {
                    let crate::types::DocumentCommitChange::Document {
                        record,
                        conversation_id,
                        version,
                        value,
                        ops,
                    } = &change
                    else {
                        continue;
                    };
                    if *conversation_id != Some(id) {
                        continue;
                    }
                    let kind = &record.kind;
                    if !kinds.contains(kind) || record.key.is_some() {
                        continue;
                    }
                    let path = vec![seg("docs"), seg(kind)];
                    let mounted = state.docs.get(kind).copied();
                    // 化身表决定"这是同一个文档的增量更新"还是"换了一个文档/定义版本"：
                    // - 退役只认当前挂载的化身 id，旧化身的退役不影响现在显示的值；
                    // - 同化身 + 同版本才复用文档自带的 op（它们是相对文档根计算的，需要
                    //   `prefixed` 移到 `docs/<kind>` 下）；否则整体 `Set`，避免把旧形状的
                    //   增量套到新形状上。
                    match value {
                        None => {
                            if mounted.map(|mounted| mounted.id) != Some(record.id) {
                                continue;
                            }
                            state.docs.shift_remove(kind);
                            doc_ops.push(Op::Delete(path));
                        }
                        Some(value) => {
                            let same = mounted.is_some_and(|mounted| {
                                mounted.id == record.id
                                    && version.is_some_and(|version| mounted.version == version)
                            });
                            if same {
                                for op in ops.iter() {
                                    doc_ops.push(prefixed(op, &path));
                                }
                            } else {
                                // 按 `DocumentCommitChange::Document` 的字段文档，带值的文档
                                // 变化里 `version` 是 `Some`；`unwrap_or(0)` 只是防御。
                                state.docs.insert(
                                    kind.clone(),
                                    MountedIncarnation {
                                        id: record.id,
                                        version: version.unwrap_or(0),
                                    },
                                );
                                doc_ops.push(Op::Set(
                                    path,
                                    serde_json::Value::Object(value.clone().into_iter().collect()),
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // 文档 op 在前、条目 op 在后；两组路径分别落在 `docs/<kind>` 与 `entries` 下，互不
        // 相交，所以顺序不影响应用结果。
        let mut ops = doc_ops;
        ops.extend(entry_ops);
        if ops.is_empty() {
            (before, state.value.clone(), ops)
        } else {
            // 类型化视图 → JSON → 应用 delta → 反序列化。这一步失败（路径非法、形状不匹配）
            // 会原样返回给监听器，由它让 Session 失败；`state.value` 只在成功后才替换。
            let encoded = serde_json::to_value(&state.value).unwrap_or(serde_json::Value::Null);
            let next = apply_immutable(encoded, &ops)
                .map_err(|error| Box::new(error) as BoxError)
                .and_then(|next| {
                    serde_json::from_value::<ConversationView>(next)
                        .map_err(|error| Box::new(error) as BoxError)
                })?;
            state.value = next;
            (before, state.value.clone(), ops)
        }
    };
    // 阶段 4：在锁外通知观察者。`frame_context` 去掉中止信号，观察者与随后的 watch 监听器
    // 都拿不到发布方的取消信号；观察者列表先拷贝，回调期间不持有挂载锁。
    let frame_context = without_abort_signal(Arc::clone(context));
    let observers: Vec<Arc<dyn ViewObserver>> = {
        let state = lock(&mount.state);
        state.observers.values().cloned().collect()
    };
    if !ops.is_empty() {
        for observer in &observers {
            let report = Arc::clone(report);
            contained(
                || observer.advance(&after, &ops, &frame_context),
                move |error| report(error),
            );
        }
    }
    for observer in &observers {
        let report = Arc::clone(report);
        contained(
            || observer.publication(&before, &after, &ops, publication, &frame_context),
            move |error| report(error),
        );
    }
    Ok(())
}

/// `op` 移到 `prefix` 之下；根替换变成对前缀的 set（TS: `prefixed`）。
///
/// 文档变化携带的 op 是相对于**文档值本身**的（例如 `Set(["text"], "hi")` 表示改文档的
/// `text` 字段），挂载需要把它们放到 `docs/<kind>` 之下。`Replace` 没有路径，语义是"整个
/// 文档换成新值"，所以映射成 `Set(prefix, value)`；其余变体把相对路径拼到 `prefix` 之后。
/// 拼接只做前缀 + 原路径的连接，不做规范化或校验——路径是否合法由 `apply_immutable` 在应用
/// 时判定（非法会让整帧失败，见 `advance`）。
pub(crate) fn prefixed(op: &Op, prefix: &Path) -> Op {
    let at = |path: &Path| -> Path {
        let mut joined = prefix.clone();
        joined.extend(path.iter().cloned());
        joined
    };
    match op {
        Op::Replace(value) => Op::Set(prefix.clone(), value.clone()),
        Op::Splice {
            path,
            index,
            remove,
            items,
        } => Op::Splice {
            path: at(path),
            index: *index,
            remove: *remove,
            items: items.clone(),
        },
        Op::Permute(path, permutation) => Op::Permute(at(path), permutation.clone()),
        Op::Set(path, value) => Op::Set(at(path), value.clone()),
        Op::Delete(path) => Op::Delete(at(path)),
        Op::Append(path, text) => Op::Append(at(path), text.clone()),
        Op::Truncate(path, count) => Op::Truncate(at(path), *count),
    }
}

// ---------------------------------------------------------------------------
// VIEWS 注册表（TS 的 WeakMap<object, ConversationViews>）
// ---------------------------------------------------------------------------

/// 进程级的 Harness → `ConversationViews` 映射（TS 的 `WeakMap<object, ConversationViews>`）。
///
/// 键是 Harness 对象的数据地址，值只持有 `Weak`：登记不会延长挂载的生命周期。Rust 没有与
/// JS 对象身份等价的键，因此用地址代替；同一个地址再次登记时以最后一次为准，地址对应的
/// 挂载已被释放时 `conversation_views` 的 `Weak::upgrade` 会失败。
static VIEWS: LazyLock<Mutex<HashMap<usize, Weak<ConversationViews>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 把一个 Harness 的视图挂载登记到它的对象地址上（TS 的 `VIEWS.set(session, this)`）。
///
/// 先清理已经失效（`strong_count == 0`）的项，再插入/覆盖 `key`。`key` 必须是 Harness 对象的
/// 数据地址：`harness/harness.rs` 在 `Arc::new_cyclic` 里用 `Weak::as_ptr()` 取得（此时对象
/// 尚未构造完成，但堆地址已经固定），而 `conversation_views` 用 trait object 的数据指针查找，
/// 两者必须指向同一个对象。
pub fn register_views(key: usize, views: &Arc<ConversationViews>) {
    let mut registry = lock(&VIEWS);
    registry.retain(|_, weak| weak.strong_count() > 0);
    registry.insert(key, Arc::downgrade(views));
}

/// 从一个 Harness 对象取回它的挂载；未知对象报【逐字】`Not a Harness`。
///
/// TS: `conversationViews(harness)`
///
/// 查找键是 `harness` trait object 的数据指针：只有由 `register_views` 登记过的**同一个**
/// 具体对象（`HarnessImpl`）才能命中；其他实现或从未登记的 Harness 返回
/// `ReportedError("Not a Harness")`。挂载已被释放（`Weak` 无法升级）时同样返回该错误。
pub fn conversation_views(
    harness: &dyn crate::harness::types::Harness,
) -> Result<Arc<ConversationViews>, BoxError> {
    let key = harness as *const dyn crate::harness::types::Harness as *const () as usize;
    let found = lock(&VIEWS).get(&key).and_then(Weak::upgrade);
    match found {
        Some(views) => Ok(views),
        None => Err(Box::new(crate::session::ReportedError(
            "Not a Harness".to_owned(),
        ))),
    }
}
