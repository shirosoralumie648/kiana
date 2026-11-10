//! `harness/util.ts`：按 key 的挂起等待、分页扫描聚合、关闭错误与可选扩展字段。
//!
//! TS 的 `Waiters` 用 `Promise.withResolvers()` 与 `AbortSignal` 的 `abort` 监听实现“每个等待
//! 只结算一次”。Rust 侧用 `tokio::sync::oneshot` 表达同一个等待：结算值放在通道里，取消时
//! 由 `AbortSignal::on_abort` 的同步回调把取消原因送进去，之后订阅随等待项一起丢弃。
//!
//! # 职责与边界
//!
//! 这里只放与调度状态机无关的通用工具，`harness` 下的模块都可以直接使用：
//!
//! - [`Waiters`]：按 key 分组的挂起等待表，服务 `wait_for_task`、`wait_for_idle` 与
//!   `Submissions::wait`。它只做“登记 → 结算/取消”的簿记，不判断任务是否终态，也不缓存
//!   已经结算过的 key；
//! - [`scan_all`]：把 Storage 的分页扫描拼成 `Vec`，调用方包括 `inspect`、`usage`、任务图
//!   构建、`open` 与事件流；
//! - [`closed_error`] / [`HarnessClosed`]：关闭（或 Session 已失败）时的错误选择；
//! - [`optional_extension`] / [`OptionalExtension`]：TS `optionalExtension()` 的对应项，
//!   表示报告里 `extension` 键“存在才写”的形状。
//!
//! # 主要流程与关键约束
//!
//! [`Waiters::add`] 的时序是正确性的核心：先向 `AbortSignal` 订阅取消回调，再在等待表的
//! 状态锁内复查 `is_aborted()` 并插入等待项。`AbortSignal::on_abort` 不会给已经中止的信号
//! 补发通知，所以“先检查、后订阅”会丢掉恰好落在两步之间的 abort，让等待项永远不结算；
//! 当前的顺序让 abort 要么被复查看见，要么由回调在插入之后结算（文件末尾的测试
//! `an_abort_racing_the_subscription_still_settles_the_waiter` 专门锁定这一条）。
//!
//! 结算分三路且互斥：`resolve` 取走整个 key，`reject_all` 取走整张表，取消回调按 id 取走
//! 单个等待项。每一路都通过 `Option::take()` 拿走 `oneshot::Sender`，因此同一个等待项至多
//! 被发送一次。
//!
//! # 注意事项
//!
//! - [`Waiters`] 不记忆已结算的 key。调用方必须把“目标状态检查”与 `add` 放进同一个串行域
//!   （例如 `TaskScheduler::wait_for_task` 在 `SessionImpl::read_on_line` 内先查 `live`、
//!   再 `add`），否则两步之间发生的结算不会补发，等待只能等下一次结算或取消；
//! - 所有 `send` 都在状态锁之外执行，被唤醒的等待者可以安全地再次调用 [`Waiters`]；
//! - 锁中毒时取回内部值继续（见下方的 `lock`），不保证 panic 之后表仍然一致。

use std::error::Error as StdError;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;
use kiana_chord::context::{AbortSignal, Context, abort_error};
use tokio::sync::oneshot;

use crate::session::SessionImpl;
use crate::types::{BoxError, Cursor, Page, SessionFailed};

// 取等待表的状态锁。锁中毒（持锁期间 panic）时取回内部值继续，避免一次 panic 之后所有
// 等待操作都因 PoisonError 失败；代价是恢复出的表可能停在 panic 当时的状态，不做一致性
// 保证（与 scheduler 中的同名帮助函数同一取舍）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 「Harness 已关闭」且 Session 没有失败时的错误（TS: `new Error("Harness is closed")`）。
///
/// 把它和 [`SessionFailed`] 分开，是为了让调用方区分“Session 故障”与“只是关闭”
/// （[`closed_error`] 负责这个选择）。`ConversationHost::upgrade`（harness.rs）与
/// [`closed_error`] 都会构造它。经 [`Waiters::reject_all`] 分发给多个等待者时，每个等待者
/// 拿到的是包了一层的共享错误，`downcast` 回本类型会失败，只能比较 `Display` 文本。
#[derive(Debug, Clone, thiserror::Error)]
#[error("Harness is closed")]
pub struct HarnessClosed;

/// 把一个共享错误重新装箱成每个等待者各自的 [`BoxError`]，保留 `Display` 文本与 `source()`。
///
/// 代价是原始错误的具体类型被藏在 `Arc` 后面：`downcast` 只能看到这一层包装，需要按具体
/// 类型分支（例如把 [`HarnessClosed`] 与其他错误分开）的代码必须在分发之前完成判断。
#[derive(Debug)]
struct SharedError(Arc<dyn StdError + Send + Sync>);

impl std::fmt::Display for SharedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl StdError for SharedError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0.source()
    }
}

/// 为单个等待者制造独立的 [`BoxError`]：外层包装共享同一个源错误。
fn clone_error(error: &Arc<dyn StdError + Send + Sync>) -> BoxError {
    Box::new(SharedError(Arc::clone(error)))
}

/// 一个等待项的结算值：`Ok(T)` 或取消/关闭错误。
///
/// 这是 `oneshot` 通道的**内层**值；接收端 `.await` 的外层 `Err` 表示发送端在结算前消失
/// （见 [`Waiters::add`]），调用方要分别处理两层。
pub type WaitResult<T> = Result<T, BoxError>;

/// 一个已登记的等待：`sender` 被任一条结算路径 `take()` 之后不再发送，保证每个等待只结算
/// 一次。
struct WaiterEntry<T> {
    id: u64,
    sender: Option<oneshot::Sender<WaitResult<T>>>,
    /// `signal.removeEventListener("abort", onAbort)` 的等价物：等待项消失时自动解绑。
    /// 只作为 RAII 守卫持有（`AbortSubscription` 的 `Drop` 移除监听器），从不读取。
    _subscription: Option<kiana_chord::context::AbortSubscription>,
}

/// 等待表的状态：`next_id` 给每个等待项分配唯一编号（取消回调据此精确定位），`sets` 按 key
/// 分组；`IndexMap` 保留 key 的插入顺序，[`Waiters::keys`] 依赖这一点。
struct WaiterState<K, T> {
    next_id: u64,
    sets: IndexMap<K, Vec<WaiterEntry<T>>>,
}

impl<K, T> Default for WaiterState<K, T> {
    fn default() -> Self {
        WaiterState {
            next_id: 0,
            sets: IndexMap::new(),
        }
    }
}

/// Pending waits by key。每个只结算一次：通过 [`Waiters::resolve`]、[`Waiters::reject_all`]，或
/// 其上下文的取消（TS: `Waiters`）。
///
/// `Clone` 共享同一张表（内部是 `Arc<Mutex<...>>`）：scheduler 与 `Submissions` 都靠克隆把
/// 同一个表交给异步等待方和关闭监听器。key 保存在 `IndexMap` 里，[`Waiters::keys`] 的返回
/// 顺序是**当前表内** key 的插入序，不是首次登记的顺序：`resolve`、`reject_all` 或取消把
/// key 移除后，再对同一个 key 调用 `add` 会重新插入到末尾。例如依次 `add("a")`、
/// `add("b")`、`resolve("a")`、`add("a")` 之后，`keys()` 返回 `["b", "a"]`。
///
/// 类型参数：`K: Clone` 因为取消回调和登记路径需要复制 key；`T: Clone` 因为一次 `resolve`
/// 要把同一个值发给该 key 下的所有等待者。`Send + Sync + 'static` 来自共享方式——表放在
/// `Arc<Mutex<...>>` 中跨任务共享，取消回调可能在 `AbortSignal::abort` 的任意调用线程上执行。
pub struct Waiters<K, T> {
    state: Arc<Mutex<WaiterState<K, T>>>,
}

impl<K, T> Default for Waiters<K, T> {
    fn default() -> Self {
        Waiters {
            state: Arc::new(Mutex::new(WaiterState::default())),
        }
    }
}

impl<K, T> Clone for Waiters<K, T> {
    fn clone(&self) -> Self {
        Waiters {
            state: Arc::clone(&self.state),
        }
    }
}

impl<K, T> Waiters<K, T>
where
    K: Clone + Eq + std::hash::Hash + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
{
    /// 新建一个空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 在 `key` 上挂起一个等待；上下文已取消时立刻以取消原因失败。
    ///
    /// 返回值是 `oneshot` 的接收端。正常结算得到 [`WaitResult`]：`Ok` 是
    /// [`Waiters::resolve`] 给的值，`Err` 来自 [`Waiters::reject_all`] 或上下文取消。若发送端
    /// 在结算前被丢弃，`.await` 得到的是 tokio 的 `oneshot::error::RecvError`，**不是**
    /// `WaitResult::Err`；因此调用方要把外层错误也映射成“Harness 已关闭”（scheduler 的
    /// `wait_for_task` 与 submissions 的 `wait` 都显式处理了 `Err(_)`）。发送端“未发送就被
    /// 丢弃”只发生在**没有** abort 订阅的等待项上：上下文不带信号（例如 `BACKGROUND_CONTEXT`）
    /// 时，丢弃最后一个 [`Waiters`] 句柄会释放整张表，表里的 `oneshot::Sender` 随之被丢弃。
    ///
    /// 带 `AbortSignal` 的等待项不会因为丢弃 [`Waiters`] 句柄而关闭通道：`subscribe_abort`
    /// 把 `Arc<Mutex<WaiterState>>` move 进 `on_abort` 回调，回调由信号的监听器列表持有，
    /// 与等待项里的 `AbortSubscription` 构成 `SignalInner → 回调 → WaiterState → 等待项 →
    /// AbortSubscription → SignalInner` 的强引用环，等待表因此不会被释放。接收端会一直
    /// pending，直到信号 abort 时回调把取消错误送进去；信号若永不中止，接收端就永远等不到
    /// 结果。
    ///
    /// # 错误
    ///
    /// 上下文信号已经中止时直接返回 `Err(abort_error(signal))`，此时不会登记任何等待项。
    /// `context.abort_signal()` 返回 `None` 的上下文（如 `BACKGROUND_CONTEXT`）无法取消，
    /// 只能由 `resolve` / `reject_all` 结算。
    ///
    /// # 取消竞态
    ///
    /// 订阅取消回调发生在插入等待项之前，而 `on_abort` 不会给已中止的信号补发通知，所以
    /// “插入前在状态锁内复查 `is_aborted`”是必要步骤，不能删（原因见函数体内的注释）。
    pub fn add(
        &self,
        key: K,
        context: &dyn Context,
    ) -> Result<oneshot::Receiver<WaitResult<T>>, BoxError> {
        let signal = context.abort_signal();
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut state = lock(&self.state);
            state.next_id += 1;
            state.next_id
        };
        // 先注册取消监听，再在同一把状态锁内检查 `aborted` 与插入等待项：TS 的 `add()` 是
        // 同步的单线程代码，「检查 `aborted`」与 `addEventListener` 之间不存在竞态；这里有。
        // 反过来做（先检查、后订阅）会丢掉落在两步之间的 abort，因为这个 `on_abort` 不会
        // 补发已中止的信号，等待项就永远不会结算。先订阅后检查则没有这个窗口：abort 回调
        // 要么已经看到 `aborted`，要么由监听器送达（回调在同一把锁上排队）。
        let subscription = signal.as_ref().map(|signal| {
            subscribe_abort(Arc::clone(&self.state), key.clone(), id, signal.clone())
        });
        {
            let mut state = lock(&self.state);
            if let Some(signal) = &signal
                && signal.is_aborted()
            {
                return Err(Box::new(abort_error(signal)));
            }
            state.sets.entry(key).or_default().push(WaiterEntry {
                id,
                sender: Some(sender),
                _subscription: subscription,
            });
        }
        Ok(receiver)
    }

    /// 现有 key，按插入序（TS: `keys()`）。
    ///
    /// 只有仍至少有一个等待项的 key 才会出现：`resolve`、`reject_all` 或等待项被全部取消之
    /// 后 key 即从表中移除。返回的是克隆出来的快照，之后的增删不影响它。
    pub fn keys(&self) -> Vec<K> {
        lock(&self.state).sets.keys().cloned().collect()
    }

    /// 先删 key，再结算该 key 的全部等待者（TS: `resolve()`）。
    ///
    /// 每个等待者都收到 `Ok(value.clone())`（包括第一个），接收端已被丢弃的等待者被静默跳过。
    /// 取表在锁内、发送在锁外，所以被唤醒的任务可以立刻再次使用 [`Waiters`]。
    ///
    /// 删除 key 之后才登记的等待者不属于本次结算：本类型不记录已结算的 key，迟到的 `add`
    /// 会一直挂起，直到下一次 `resolve`、`reject_all` 或上下文取消。
    pub fn resolve(&self, key: &K, value: T) {
        let entries = lock(&self.state).sets.shift_remove(key);
        for mut entry in entries.unwrap_or_default() {
            if let Some(sender) = entry.sender.take() {
                let _ = sender.send(Ok(value.clone()));
            }
        }
    }

    /// 清空全部等待并逐个失败（TS: `rejectAll()`）。
    ///
    /// 先在锁内 `mem::take` 走整张表，再在锁外逐个发送，因此调用期间新登记的等待项不受本次
    /// 结算影响（调用方通常先置关闭标记，让新的 `add` 走关闭检查）。错误只被共享一次：
    /// `BoxError` 不能克隆，先转成 `Arc<dyn Error + Send + Sync>`，再给每个等待者包一层
    /// `SharedError`。这层包装转发 `Display` 与 `source()`，但不再是原始具体类型——等待者
    /// 不能把错误 `downcast` 回 [`HarnessClosed`] 之类，只能按文本判断。
    pub fn reject_all(&self, error: BoxError) {
        let sets = {
            let mut state = lock(&self.state);
            std::mem::take(&mut state.sets)
        };
        let shared: Arc<dyn StdError + Send + Sync> = error.into();
        for (_, entries) in sets {
            for mut entry in entries {
                if let Some(sender) = entry.sender.take() {
                    let _ = sender.send(Err(clone_error(&shared)));
                }
            }
        }
    }
}

/// 注册一个取消监听：把等待项从集合里移除，并以取消原因结算它。
///
/// 回调在调用 `AbortSignal::abort` 的线程上同步执行。它在状态锁内按 `id` 定位并取出等待项，
/// 释放锁之后再 `send`；被取出的 `WaiterEntry` 同时释放其 `AbortSubscription`，该等待项之后
/// 不会再收到取消通知。`abort_error` 读的是回调触发时的信号原因（`abort` 会在调用监听器
/// 之前写入 reason）。
fn subscribe_abort<K, T>(
    state: Arc<Mutex<WaiterState<K, T>>>,
    key: K,
    id: u64,
    signal: AbortSignal,
) -> kiana_chord::context::AbortSubscription
where
    K: Clone + Eq + std::hash::Hash + Send + Sync + 'static,
    T: Send + 'static,
{
    let callback_signal = signal.clone();
    signal.on_abort(move |_reason| {
        let mut waiter = {
            let mut state = lock(&state);
            let mut waiter = None;
            if let Some(entries) = state.sets.get_mut(&key) {
                if let Some(position) = entries.iter().position(|entry| entry.id == id) {
                    waiter = Some(entries.remove(position));
                }
                if entries.is_empty() {
                    state.sets.shift_remove(&key);
                }
            }
            waiter
        };
        if let Some(waiter) = waiter.as_mut()
            && let Some(sender) = waiter.sender.take()
        {
            let _ = sender.send(Err(Box::new(abort_error(&callback_signal))));
        }
    })
}

/// 分页扫描并聚合：反复调用 `scan`，直到某一页的 `next` 为 `None`（TS: `scanAll`；`scan`
/// 参数对应 TS 的扫描回调）。
///
/// 第一页传入 `None`，之后传入上一页 `next` 的克隆；[`Cursor`] 的内容对本函数不透明，只负责
/// 原样转交。结果按页序、页内原序追加到 `Vec`，不排序、不去重。`scan` 是 `FnMut`，允许闭包
/// 跨页保存状态（本文件测试用迭代器逐页供给固定页面）。
///
/// # 错误与边界
///
/// 任一页返回 `Err` 就立即向上传播，已收集的条目随之丢弃。循环只以 `next == None` 结束，
/// 不检测游标是否推进：若 `scan` 反复返回同一个 `next`，这里会无限循环，分页终止由 Storage
/// 实现负责。
pub async fn scan_all<T, F, Fut>(mut scan: F) -> Result<Vec<T>, BoxError>
where
    F: FnMut(Option<Cursor>) -> Fut,
    Fut: Future<Output = Result<Page<T>, BoxError>>,
{
    let mut items: Vec<T> = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = scan(cursor.clone()).await?;
        items.extend(page.items);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(items)
}

/// 一个已关闭 Harness 拒绝时用的错误：Session 因存储失败关闭时是带 cause 的
/// [`SessionFailed`]，否则是 [`HarnessClosed`]（TS: `closedError()`）。
///
/// 判定顺序是先看 `session.failure()`：Session 既失败又已关闭时失败优先，与
/// `HarnessImpl::assert_usable` 的选择一致（后者只是不经过本函数）。失败分支的 cause 由
/// `Failure::error()` 重新构造，只携带失败文本（`ReportedError`），最初那个 Storage 错误
/// 对象已不在 `source()` 链上。`session.failure()` 在内部锁下读快照，因此本函数的结果对应
/// 调用那一刻。
pub fn closed_error(session: &SessionImpl) -> BoxError {
    match session.failure() {
        None => Box::new(HarnessClosed),
        Some(failure) => Box::new(SessionFailed::new(Box::new(failure.error()))),
    }
}

/// `{ extension }` 当扩展已知（TS: `optionalExtension()`）。
///
/// 序列化时 `extension` 为 `None` 就完全省略该键（`skip_serializing_if`），反序列化时缺失
/// 该键得到 `None`（`serde(default)`）。crate 内目前没有调用方；它对应 TS `util.ts` 的公开
/// 导出。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OptionalExtension {
    /// 扩展名；缺省时不写该键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

/// `{ extension }` 当扩展已知。
///
/// `None` 返回默认值（序列化为 `{}`），`Some` 复制一份字符串。对应 TS 的
/// `extension === undefined ? {} : { extension }`。
pub fn optional_extension(extension: Option<&str>) -> OptionalExtension {
    OptionalExtension {
        extension: extension.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    // 测试锁定六条行为：同一 key 的全部等待者都收到同一个值且只结算一次；reject_all 让所有
    // 等待者共享同一错误文本；已中止的上下文让 add 立即失败；已登记的等待在 abort 时被结算
    // 并从表中移除；abort 落在 add 的订阅窗口内仍会结算并从表中移除；scan_all 按页序聚合。
    // AbortOnFirstClone 专门制造 abort 落在订阅窗口内这一种竞态。
    use super::*;
    use kiana_chord::context::BACKGROUND_CONTEXT;

    #[tokio::test]
    async fn resolves_every_waiter_of_a_key_once() {
        let waiters: Waiters<String, u32> = Waiters::new();
        let first = waiters.add("a".to_owned(), &**BACKGROUND_CONTEXT).unwrap();
        let second = waiters.add("a".to_owned(), &**BACKGROUND_CONTEXT).unwrap();
        assert_eq!(waiters.keys(), vec!["a".to_owned()]);
        waiters.resolve(&"a".to_owned(), 7);
        assert_eq!(first.await.unwrap().unwrap(), 7);
        assert_eq!(second.await.unwrap().unwrap(), 7);
        assert!(waiters.keys().is_empty());
    }

    #[tokio::test]
    async fn rejects_all_with_the_same_message() {
        let waiters: Waiters<String, u32> = Waiters::new();
        let first = waiters.add("a".to_owned(), &**BACKGROUND_CONTEXT).unwrap();
        let second = waiters.add("b".to_owned(), &**BACKGROUND_CONTEXT).unwrap();
        waiters.reject_all(Box::new(HarnessClosed));
        assert_eq!(
            first.await.unwrap().unwrap_err().to_string(),
            "Harness is closed"
        );
        assert_eq!(
            second.await.unwrap().unwrap_err().to_string(),
            "Harness is closed"
        );
        assert!(waiters.keys().is_empty());
    }

    #[tokio::test]
    async fn an_already_aborted_context_rejects_at_once() {
        let waiters: Waiters<String, u32> = Waiters::new();
        let signal = AbortSignal::new();
        let error = std::io::Error::other("cancelled");
        signal.abort(Some(kiana_chord::context::AbortReason::error(error)));
        let context =
            kiana_chord::context::with_abort_signal(signal, Arc::clone(&BACKGROUND_CONTEXT));
        let receiver = waiters.add("a".to_owned(), context.as_ref()).unwrap_err();
        assert_eq!(receiver.to_string(), "cancelled");
        assert!(waiters.keys().is_empty());
    }

    #[tokio::test]
    async fn cancellation_settles_a_pending_waiter_and_removes_it() {
        let waiters: Waiters<String, u32> = Waiters::new();
        let signal = AbortSignal::new();
        let context = kiana_chord::context::with_abort_signal(
            signal.clone(),
            Arc::clone(&BACKGROUND_CONTEXT),
        );
        let receiver = waiters.add("a".to_owned(), context.as_ref()).unwrap();
        assert_eq!(waiters.keys(), vec!["a".to_owned()]);
        signal.abort(Some(kiana_chord::context::AbortReason::error(
            std::io::Error::other("cancelled"),
        )));
        let error = receiver.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "cancelled");
        assert!(waiters.keys().is_empty());
    }

    /// 一个键，第一次 `clone()` 时中止自己的信号：模拟「abort 正好落在 `add()` 订阅取消
    /// 监听的那一步」。TS 的 `add()` 是同步单线程代码，检查 `aborted` 与 `addEventListener`
    /// 之间没有窗口；Rust 必须保证即使 abort 落在这个窗口里，等待项也以取消原因结算
    /// （先订阅、再在状态锁内检查 `aborted` 并插入）。
    struct AbortOnFirstClone {
        signal: AbortSignal,
        aborted: Arc<std::sync::atomic::AtomicBool>,
        id: u32,
    }

    impl Clone for AbortOnFirstClone {
        fn clone(&self) -> Self {
            if !self.aborted.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.signal
                    .abort(Some(kiana_chord::context::AbortReason::error(
                        std::io::Error::other("cancelled"),
                    )));
            }
            AbortOnFirstClone {
                signal: self.signal.clone(),
                aborted: Arc::clone(&self.aborted),
                id: self.id,
            }
        }
    }

    impl PartialEq for AbortOnFirstClone {
        fn eq(&self, other: &Self) -> bool {
            self.id == other.id
        }
    }

    impl Eq for AbortOnFirstClone {}

    impl std::hash::Hash for AbortOnFirstClone {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.id.hash(state);
        }
    }

    #[tokio::test]
    async fn an_abort_racing_the_subscription_still_settles_the_waiter() {
        let waiters: Waiters<AbortOnFirstClone, u32> = Waiters::new();
        let signal = AbortSignal::new();
        let context = kiana_chord::context::with_abort_signal(
            signal.clone(),
            Arc::clone(&BACKGROUND_CONTEXT),
        );
        let key = AbortOnFirstClone {
            signal: signal.clone(),
            aborted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            id: 1,
        };
        match waiters.add(key, context.as_ref()) {
            Err(error) => assert_eq!(error.to_string(), "cancelled"),
            Ok(receiver) => {
                let settled =
                    tokio::time::timeout(std::time::Duration::from_secs(1), receiver).await;
                let outcome = settled.expect("an abort during add() must settle the waiter");
                assert_eq!(outcome.unwrap().unwrap_err().to_string(), "cancelled");
            }
        }
        assert!(
            waiters.keys().is_empty(),
            "an aborted waiter leaves the map"
        );
    }

    #[tokio::test]
    async fn scan_all_collects_every_page_in_order() {
        let pages = vec![
            Page {
                items: vec![1, 2],
                next: Some(serde_json::from_value(serde_json::json!({"after": 2})).unwrap()),
            },
            Page {
                items: vec![3],
                next: None,
            },
        ];
        let mut remaining = pages.into_iter();
        let items = scan_all(|_cursor| {
            let page = remaining.next().unwrap();
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(items, vec![1, 2, 3]);
    }
}
