//! `utils/abort.ts`——可选中止信号与「中止即放弃等待」。
//!
//! TS 的 `AbortSignal` → [`CancellationToken`]（`ARCHITECTURE.md` §4.2）。`signal.reason` 用
//! [`AbortScope`] 承载：一个 token + 可选的 reason（`Arc<Mutex<Option<AbortReason>>>`）。
//!
//! `utils/abort.ts` 的三个函数：
//! - [`operation_signal`]：给可选 signal 的公开 API 造一个操作局部 signal（永不 abort）；
//! - [`race_with_abort_signal`]：signal 先到就放弃等待，但**继续观察**被放弃的 future，
//!   保证其后续错误不会变成 unhandled rejection（TS 的 `void operation.catch(() => {})`）；
//! - `abortReason(signal)`：`signal.reason`，缺省时是 `Error("The operation was aborted")` +
//!   `name = "AbortError"`（只对没有 reason 的手工 signal 生效）。
//!
//! Node 的 `new AbortController().abort()` 把 reason 设为 DOMException，消息是
//! **"This operation was aborted"**（spec/ai-auth.md §4.7）；两个缺省字符串分别由
//! [`AbortReason::default_abort_error`]（中止动作）与 [`AbortReason::fallback_abort_error`]
//! （无 reason 的 signal）承载。
//!
//! TS: `utils/abort.ts`
//!
//! # 为什么需要这一层
//!
//! tokio 的 [`CancellationToken`] 只表达「已取消 / 未取消」，没有 `AbortSignal` 的三项能力：
//! 携带任意 reason、同步派发 abort 事件、被 `combineAbortSignals` 组合。本模块把令牌包装成
//! [`AbortScope`] 补齐这三项，使 provider 适配器、OAuth 流程与凭证存储可以沿用 TS 的取消
//! 语义（`signal.throwIfAborted()`、`raceWithAbortSignal`）。
//!
//! # 主要类型与流程
//!
//! - [`AbortScope`] 由三份共享状态组成：取消令牌、`Option<AbortReason>`、监听器列表。
//!   `Clone` 共享这三份状态（令牌本身是句柄），不是深拷贝。
//! - 公开 API 的可选 signal 先用 [`operation_signal`] 归一成必有值，再在 await 点之间检查
//!   [`AbortScope::is_aborted`]；需要「等结果与等取消赛跑」时用 [`race_with_abort_signal`]。
//! - provider 适配层把 `CancellationToken` 经 `AbortScope::from` 转成 scope
//!   （`api/*` 的 `ProviderRetryOptions.signal`），因此「令牌被外部直接取消」也必须能表现
//!   出中止状态（见下面的注意事项）。
//!
//! # 注意事项（并发与生命周期）
//!
//! - 监听器在调用 [`AbortScope::abort_with`] 的线程上**同步**执行，且在派发前就整批从列表
//!   取走（`once` 语义）。持有其它锁时不要 abort：`models` 的
//!   `supersede_provider_refresh` 特意先释放表锁再 abort，否则监听器会在持锁状态下运行。
//! - 对已中止的 scope 再 [`AbortScope::add_abort_listener`] **不会补发**事件（与 DOM 一致）。
//!   「检查 `is_aborted()`」与「真正挂上监听」不是原子操作：跨线程时源可能在两者之间中止，
//!   这次监听就永远等不到事件。`combineAbortSignals` 的合并结果因此存在理论上的漏事件窗口；
//!   TS 的单线程事件循环没有这个交错。
//! - `reason` 只由 [`AbortScope::abort_with`] 写入。通过 [`AbortScope::token`] 拿到令牌后
//!   直接 `cancel()`，或 `From<CancellationToken>` 收到一个已取消的令牌，只会让
//!   [`AbortScope::is_aborted`] 变真：`reason()` 仍是 `None`，监听器也不会被派发。
//!   重建 `AiError`/`AuthError` 的读取端（`abort_reason`、`throw_if_aborted`、
//!   [`race_with_abort_signal`]）用 [`AbortReason::fallback_abort_error`] 兜底；而需要把
//!   「已中止但没有 reason」重新转成一次中止动作的地方——`combine_abort_signals`
//!   （`utils/abort_signals.rs`）与 `auth::resolve` 的锁等待传播——使用 DOM 的默认值
//!   [`AbortReason::default_abort_error`]（"This operation was aborted"），两种缺省文本
//!   不要混为一谈。
//! - [`race_with_abort_signal`] 只放弃等待，不取消任务：operation 继续运行到结束，其副作用
//!   仍然发生（凭证写入等调用点依赖「返回值可能先于副作用完成」这一语义）。

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::error::AiError;

/// `AbortSignal.reason` 的等价物：中止原因（默认是 `AbortError`）。
///
/// TS 的 reason 可以是任意值；Rust 侧只保留可渲染的字符串 + 名字（`Error.name`）。
/// `stack`/`code` 等附加信息在转换中丢失；确实需要时由调用方另存进 `AiError::detail`，
/// 本类型不携带该字段。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AbortReason {
    /// `error.name`（`"AbortError"`、`"TimeoutError"`、`"Error"`…）。
    pub name: String,
    /// `error.message`。
    pub message: String,
}

impl AbortReason {
    /// `AbortController.abort()` 的缺省 reason：DOMException `AbortError`。
    ///
    /// Node 的 message 是 "This operation was aborted"（spec/ai-auth.md §4.7）。
    ///
    /// 与 [`AbortReason::fallback_abort_error`] 只差消息首词：`This` 才是真正的
    /// DOMException 文本，`The` 是 TS `abortReason()` 手工构造的 `Error` 文本，
    /// 保真测试分别锁定了这两个字符串，不要合并。
    pub fn default_abort_error() -> Self {
        AbortReason {
            name: "AbortError".into(),
            message: "This operation was aborted".into(),
        }
    }

    /// TS `abortReason()` 的回退分支：没有 `reason` 的手工 signal。
    ///
    /// 读取端（`abortReason`、`throwIfAborted`、[`race_with_abort_signal`]）在没有 reason
    /// 时使用它；本值不是任何 abort 动作写入的，不要用它来执行 `abort_with`。
    pub fn fallback_abort_error() -> Self {
        AbortReason {
            name: "AbortError".into(),
            message: "The operation was aborted".into(),
        }
    }

    /// 从 [`AiError`] 构造。
    ///
    /// 只拷贝 `name`/`message`：`AiError` 的 `kind`/`detail` 不进入 reason（reason 的用途是
    /// 在重建错误时保留展示文本与名字）。
    pub fn from_error(error: &AiError) -> Self {
        AbortReason {
            name: error.name.clone(),
            message: error.message.clone(),
        }
    }

    /// 渲染成 [`AiError`]（`name` 保留）。
    ///
    /// 总是构造 `kind == Aborted` 的错误（[`AiError::aborted`]），即使 reason 来自
    /// `TimeoutError` 这类非中止原因；下游 [`AiError::is_aborted`] 会把它当中止处理。
    pub fn to_error(&self) -> AiError {
        AiError::aborted(self.message.clone()).with_name(self.name.clone())
    }
}

/// `AbortSignal`：取消令牌 + 可选原因 + abort 监听器，全部共享同一份状态。
///
/// # 不变量
///
/// - `token`/`reason`/`listeners` 三份状态由所有克隆共享；`Clone` 只增加引用计数。
/// - [`AbortScope::is_aborted`] 只反映令牌状态；[`AbortScope::reason`] 只有在经过
///   [`AbortScope::abort_with`] 之后才有值。「已中止但没有 reason」是合法状态
///   （令牌被外部直接取消）。
/// - 监听器不补发：中止发生在注册之前时，新注册的回调永远不会执行。
/// - 类型可以跨线程使用（状态由 `Mutex` 保护，[`CancellationToken`] 是 `Send + Sync`），
///   但回调在 abort 调用线程上执行，回调自身仍须保证线程安全且不阻塞。
#[derive(Clone, Default)]
pub struct AbortScope {
    token: CancellationToken,
    reason: Arc<Mutex<Option<AbortReason>>>,
    listeners: Arc<Mutex<Vec<AbortListenerFn>>>,
}

/// abort 监听器（`signal.addEventListener("abort", listener, { once: true })`）。
///
/// 回调是 `Fn` 而不是 `FnOnce`：`once` 语义由 [`AbortScope::abort_with`] 在派发前清空
/// 列表保证，而非类型系统。回调在 abort 调用线程上同步运行，应保持短小、避免阻塞。
pub type AbortListenerFn = Arc<dyn Fn() + Send + Sync>;

/// 已注册监听器的句柄；[`AbortListener::remove`] 可重复调用。
///
/// `remove` 用 `Arc::ptr_eq` 匹配监听器：同一个监听器被注册两次时，一次 `remove` 会把
/// 两处注册都移除（`retain` 删除所有指针相同的条目）。
#[derive(Clone)]
pub struct AbortListener {
    scope: AbortScope,
    listener: AbortListenerFn,
}

impl AbortListener {
    /// `removeEventListener("abort", listener)`。
    ///
    /// 对已派发（列表已被取走）或已移除的句柄调用是空操作。
    pub fn remove(&self) {
        self.scope.remove_abort_listener(&self.listener);
    }
}

impl std::fmt::Debug for AbortListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortListener").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AbortScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortScope")
            .field("token", &self.token)
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl AbortScope {
    /// 新建一个未中止的 scope。
    ///
    /// 不绑定 Tokio 运行时：`CancellationToken` 本身可以脱离 reactor 创建，因此本构造函数
    /// 可在任意线程调用（`abort_signals` 中「无运行时」的测试依赖这一点）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取消并记录原因（先到者胜，与 `AbortController.abort()` 一致）。
    ///
    /// 监听器在调用线程上**同步**执行（Node 的 abort 事件是同步派发的），并按 `once`
    /// 语义在派发前清空。
    ///
    /// # 执行顺序与并发
    ///
    /// 1. 在 `reason` 锁内检查令牌并写入 reason，随后立即释放锁；
    /// 2. 取消令牌，唤醒 `cancelled()` 的等待者；
    /// 3. 取走监听器列表（锁只覆盖取列表这一步），在锁外逐个同步调用。
    ///
    /// 监听器因此在锁外运行，可以安全调用 [`AbortScope::reason`]/[`AbortScope::is_aborted`]，
    /// 不会自锁。代价是 reason 的写入与令牌取消之间有一小段窗口：并发调用 `abort_with` 时
    /// 两次都可能通过检查，监听器仍只派发一次，但最终 reason 由最后写入者决定——
    /// 「先到者胜」只在串行调用下成立。
    ///
    /// 若令牌已由 [`AbortScope::token`] 直接 `cancel()` 取消，本方法会在第 1 步提前返回，
    /// 监听器**不会**被派发（但 scope 已处于中止状态）。
    ///
    /// 监听器 panic 会中断本次派发并把 panic 传播给 `abort_with` 的调用者；列表已经被取走，
    /// 剩余监听器不会再执行，也不会在后续 abort 中重试。
    pub fn abort_with(&self, reason: AbortReason) {
        // 先检查再写：已中止的 scope 不再覆盖已有 reason（串行调用下即「先到者胜」）。
        let mut slot = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        if self.token.is_cancelled() {
            return;
        }
        *slot = Some(reason);
        drop(slot);
        self.token.cancel();
        let listeners: Vec<AbortListenerFn> = {
            let mut listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *listeners)
        };
        // 列表已被 take：这实现了 `once`，也让并发的第二次 abort 拿到空列表。
        for listener in listeners {
            listener();
        }
    }

    /// `signal.addEventListener("abort", listener, { once: true })`。
    ///
    /// 返回的句柄用于 `removeEventListener`。已中止的 scope 不会再派发（与 DOM 一致：
    /// 调用方应先检查 [`AbortScope::is_aborted`]，`combineAbortSignals` 就是这么做的）。
    ///
    /// 检查与注册之间没有原子性：本方法只把回调追加进列表，不会在追加后复查状态。
    /// 跨线程场景下若 scope 恰好在「调用方检查」与「本次追加」之间中止，回调不会补发；
    /// 调用方若有硬性要求，只能在注册后**再查一次** `is_aborted()`（缩小而不能消除窗口）。
    pub fn add_abort_listener(&self, listener: AbortListenerFn) -> AbortListener {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&listener));
        AbortListener {
            scope: self.clone(),
            listener,
        }
    }

    fn remove_abort_listener(&self, listener: &AbortListenerFn) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|registered| !Arc::ptr_eq(registered, listener));
    }

    /// 用默认的 `AbortError` 取消。
    ///
    /// 等价于 `abort_with(AbortReason::default_abort_error())`，因此 reason 的消息是
    /// "This operation was aborted"（不是回退分支的 "The operation was aborted"）。
    pub fn abort(&self) {
        self.abort_with(AbortReason::default_abort_error());
    }

    /// 是否已中止。
    ///
    /// 只读令牌状态，不区分原因；直接取消令牌与经由 `abort_with` 都会让它变为 `true`。
    pub fn is_aborted(&self) -> bool {
        self.token.is_cancelled()
    }

    /// `signal.reason`：`None` 对应 TS 的 `undefined`。
    ///
    /// 注意 reason 在 `token.cancel()` **之前**写入：另一线程可能短暂观察到
    /// `Some(reason)` 而 `is_aborted() == false`。不要用 reason 是否存在来判断是否已中止。
    pub fn reason(&self) -> Option<AbortReason> {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 底层取消令牌（`HttpClient::execute` 等接收 `&CancellationToken` 的地方用）。
    ///
    /// 返回的是共享同一取消状态的克隆。通过它直接 `cancel()` **绕过**本类型的原因记录与
    /// 监听器派发：`reason()` 仍为 `None`，`add_abort_listener` 注册的回调不会执行
    /// （`combine_abort_signals` 的传播依赖监听器）。需要传播时请用
    /// [`AbortScope::abort`]/[`AbortScope::abort_with`]。
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// 订阅中止事件。
    ///
    /// 只等待令牌取消，不携带 reason；返回的 future 借用 `self`，不能越过 scope 的生命周期。
    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }
}

/// 包装一个外部令牌：scope 与令牌共享同一个取消状态。
///
/// 外部直接 `cancel()` 令牌时，scope 会变成「已中止但没有 reason」：`reason()` 为 `None`，
/// 且不会派发通过 [`AbortScope::add_abort_listener`] 注册的回调（回调只由
/// [`AbortScope::abort_with`] 派发）。需要监听传播时请改用 [`AbortScope::abort`]。
impl From<CancellationToken> for AbortScope {
    fn from(token: CancellationToken) -> Self {
        AbortScope {
            token,
            reason: Arc::new(Mutex::new(None)),
            listeners: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// `abortReason(signal)`：`signal.reason`，缺省时造一个 `Error("The operation was aborted")`。
fn abort_reason(scope: &AbortScope) -> AbortReason {
    scope
        .reason()
        .unwrap_or_else(AbortReason::fallback_abort_error)
}

/// `operationSignal(signal)`：给可选 signal 的公开 API 造一个操作局部 signal。
///
/// TS 的 `signal ?? new AbortController().signal`：**永不返回 undefined**，且新建的 signal
/// 永不 abort。
///
/// 传入 `Some` 时按值返回同一个 scope（不克隆，调用方需自行 `clone` 才能保留原句柄）；
/// 新建的 scope 不会自己中止，只有持有者显式调用 [`AbortScope::abort`] 才会。这一层只消除
/// `Option` 分支，不会改变传入 signal 已有的中止状态。
///
/// TS: `utils/abort.ts#operationSignal`
pub fn operation_signal(signal: Option<AbortScope>) -> AbortScope {
    signal.unwrap_or_default()
}

/// `raceWithAbortSignal(operation, signal)`：signal 中止就立即放弃等待。
///
/// 放弃后仍保留对 `operation` 的观察（`tokio::spawn` 的 JoinHandle 被丢弃，任务继续跑完），
/// 因此其后续失败不会变成 panic 或未处理的错误——对应 TS 的 `void operation.catch(() => {})`。
///
/// # 参数与返回值
///
/// - `operation`：要执行的 future，会在入口处 `tokio::spawn` 到当前运行时；要求
///   `Send + 'static`。
/// - `scope`：中止信号，允许传入已中止的 scope。
/// - 返回 `Ok(T)`（operation 正常完成）、`Err(AiError)`（中止，`kind == Aborted`）；
///   operation panic 时把 panic 原样 `resume_unwind` 到调用方线程。
///
/// # 生命周期语义
///
/// operation 在检查是否已中止**之前**就被 spawn：即使 `scope` 已中止也会启动——对应 TS 里
/// 「promise 已经创建，调用方只是不再等待」。中止分支返回时 `JoinHandle` 被丢弃，任务被
/// detach 继续运行；它的返回值无人接收，错误也不会冒泡。
///
/// # 边界与失败模式
///
/// - 必须在 Tokio 运行时内调用，否则 `tokio::spawn` 会 panic。
/// - 令牌被直接 cancel（没有 reason）时，错误文本回退到
///   [`AbortReason::fallback_abort_error`]。
/// - `select!` 在两个分支同时就绪时随机选择：中止与完成恰好同刻时，调用方不能假定哪一个
///   一定获胜（TS 的 `settled` 标志按事件循环顺序决定，两者不保证一致）。
/// - 任务因运行时关闭被取消（`JoinError::is_cancelled`）时返回 `AiErrorKind::Other`，
///   文本为 "operation task was cancelled"。
///
/// TS: `utils/abort.ts#raceWithAbortSignal`
// `AiError` 由 `ARCHITECTURE.md` §4.1 的公开错误契约固定（`Display` 必须等于 TS 的
// `error.message`，字段不能装箱），签名因此保持原样并关闭该 lint。
#[allow(clippy::result_large_err)]
pub async fn race_with_abort_signal<T, F>(operation: F, scope: &AbortScope) -> Result<T, AiError>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    // TS 里 operation 是一个**已经启动**的 promise；Rust 用 spawn 得到同样的生命期：
    // 放弃等待（drop JoinHandle）不会取消任务，任务继续跑完，其错误也不会冒泡。
    let mut handle = tokio::spawn(operation);
    // 先 spawn 再检查：即使已经中止也要让 operation 跑起来，只是不再等它。
    if scope.is_aborted() {
        return Err(abort_reason(scope).to_error());
    }
    let token = scope.token();
    // `select!` 的取消分支与完成分支竞争；中止分支返回时 handle 被丢弃，任务继续运行。
    tokio::select! {
        joined = &mut handle => match joined {
            Ok(value) => Ok(value),
            // 被放弃的 future panic：按原样向上传播，不吞掉 panic。
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            // 本函数从不 abort 该 handle，因此这里只可能来自运行时关闭导致的强制取消。
            Err(_) => Err(AiError::new(
                crate::error::AiErrorKind::Other,
                "operation task was cancelled",
            )),
        },
        _ = token.cancelled() => Err(abort_reason(scope).to_error()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_signal_is_never_aborted() {
        let scope = operation_signal(None);
        assert!(!scope.is_aborted());
        // 传入已中止的 signal 时原样返回。
        let aborted = AbortScope::new();
        aborted.abort();
        assert!(operation_signal(Some(aborted)).is_aborted());
    }

    #[test]
    fn abort_reason_falls_back_to_abort_error() {
        let scope = AbortScope::new();
        assert_eq!(abort_reason(&scope), AbortReason::fallback_abort_error());
        assert_eq!(abort_reason(&scope).message, "The operation was aborted");
        scope.abort_with(AbortReason {
            name: "TimeoutError".into(),
            message: "timed out".into(),
        });
        assert_eq!(abort_reason(&scope).name, "TimeoutError");
    }

    #[tokio::test]
    async fn race_returns_the_operation_value() {
        let scope = AbortScope::new();
        let value = race_with_abort_signal(async { 7 }, &scope)
            .await
            .expect("ok");
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn race_rejects_immediately_when_already_aborted() {
        let scope = AbortScope::new();
        scope.abort();
        let error = race_with_abort_signal(async { 7 }, &scope)
            .await
            .unwrap_err();
        assert_eq!(error.name, "AbortError");
        assert_eq!(error.message, "This operation was aborted");
    }

    #[test]
    fn abort_uses_the_node_dom_exception_message() {
        let scope = AbortScope::new();
        scope.abort();
        let reason = scope.reason().expect("reason");
        assert_eq!(reason.name, "AbortError");
        assert_eq!(reason.message, "This operation was aborted");
    }

    #[test]
    fn fallback_reason_keeps_the_ts_error_message_for_hand_rolled_signals() {
        let token = CancellationToken::new();
        let scope = AbortScope::from(token.clone());
        token.cancel();
        assert_eq!(abort_reason(&scope).message, "The operation was aborted");
    }

    #[test]
    fn abort_listeners_run_synchronously_and_only_once() {
        let scope = AbortScope::new();
        let calls = Arc::new(Mutex::new(0usize));
        let calls_inner = Arc::clone(&calls);
        let registration =
            scope.add_abort_listener(Arc::new(move || *calls_inner.lock().unwrap() += 1));
        scope.abort();
        assert_eq!(*calls.lock().unwrap(), 1);
        scope.abort();
        assert_eq!(*calls.lock().unwrap(), 1);
        // 已派发（once）后移除是幂等的。
        registration.remove();
        registration.remove();
    }

    #[tokio::test]
    async fn race_rejects_when_aborted_during_the_operation() {
        let scope = AbortScope::new();
        let token = scope.token();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            token.cancel();
        });
        let error = race_with_abort_signal(
            async {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                7
            },
            &scope,
        )
        .await
        .unwrap_err();
        assert!(error.is_aborted());
    }
}
