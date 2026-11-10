//! `context/index.ts` 的实现体。
//!
//! `Context` 在 Rust 侧是对象安全 trait（`abort_signal`、`telemetry`、`value`、`to_string_value`）；
//! `EmptyContext` / `ContextValueContext` 是非公开实现；组合子与 TS 同名、**同参数顺序**。
//!
//! # 与 TS 的差异（Rust 没有对应语言机制的地方）
//!
//! 1. **`value<T>(key)`**：TS 是泛型方法，可以返回任意 `T`；对象安全的 Rust trait 不能有泛型方法，
//!    因此擦除为 [`ContextValue`]（`Arc<dyn Any + Send + Sync>`），由 [`ContextKey::get`] 做类型化
//!    取值（`T: Clone + Send + Sync + 'static`）。键的身份仍是 token 的 `Arc::ptr_eq`，
//!    与 TS 的 `Symbol` 身份一致。
//! 2. **`AbortSignal`**：ARCHITECTURE §4.2 规定 `CancellationToken` + 可选原因。本文件自带
//!    [`AbortSignal`]（`CancellationToken` + 原因 + 同步监听器），实现 DOM 的
//!    `abort(reason)`/`AbortSignal.any([…])`/`addEventListener` 语义，因为 `withAbortSignal`
//!    需要"任一信号中止即中止"。
//! 3. **`Promise`**：Rust 的 future 在 drop 时丢弃，而 TS 的 `awaitWithContext` **不取消**底层
//!    promise。因此有两个入口：[`await_with_context`]（drop 即放弃 future）与
//!    [`await_handle_with_context`]（等 `JoinHandle`；放弃等待时任务继续跑，因为 tokio 的 drop 只 detach）。
//! 4. **`traceIn` 的异常通道**：TS 的回调可以 `throw`，`startSpan` 原样传出 rejection 并把 span 记为
//!    error。Rust 的回调返回 `T`：[`trace_in`] 原样传出返回值；[`trace_in_try`] 让回调返回
//!    `Result`，`Err` 等价于 `throw`。两者都走 `startSpan`，**没有**"无 telemetry 分支"——
//!    NOOP telemetry 也一样走。
//! 5. **`toString()`** → [`Context::to_string_value`] + `impl Display for dyn Context`。
//!
//! # 职责与边界
//!
//! 本文件只做调用作用域的簿记，不执行 IO、不包含业务逻辑。它负责四件事：
//!
//! 1. **值分层**：[`Context`] 是一条不可变链，根是 `EmptyContext`，每次 [`with_context_value`]
//!    在链顶加一层 `ContextValueContext`。查找从链顶向根部走，命中即停。
//! 2. **取消**：[`AbortSignal`] 把 `CancellationToken`（可被 `select!` 等待）与 DOM 风格的
//!    reason／同步监听器绑在一起，并提供 [`AbortSignal::any`] 组合。
//! 3. **遥测**：每个上下文都有 `telemetry()` 成员，缺省是进程级 NOOP 单例；[`trace_in`] 用它
//!    在子上下文里开 span。
//! 4. **等待**：[`await_with_context`] 在调用方取消时提前返回，并 drop 它包装的 future；
//!    若需要"任务继续跑、只是放弃等待"，用 [`await_handle_with_context`]。
//!
//! 直接调用方包括 `kiana-durable` 的 session/scheduler/harness：例如 scheduler 用
//! [`with_abort_signal`] 把调用控制器挂进上下文，session 用 [`without_abort_signal`] 派生
//! 清理/提交上下文，二者都用 [`await_with_context`] 等待共享 future。本模块不反向依赖它们，
//! 只依赖标准库、`serde_json`、`tokio`/`tokio-util` 与 `kiana-telemetry`。
//!
//! # 数据结构与查找顺序
//!
//! [`ContextRef`] 是 `Arc<dyn Context>`；`with_*` 系列每次新建一层并共享父层（`Arc` 克隆），
//! 父层永远不被修改。键的相等性是**身份**而非描述：两次 `create_context_key("value")` 得到
//! 不同 token，互不命中；克隆键共享 token，因此仍然相等。查找代价与层数成正比，没有缓存。
//!
//! [`ContextKey::get`] 用 `downcast_ref::<T>()` 取回值：token 命中但存储值的实际类型不是 `T`
//! 时（例如把某个键 `erase()` 成 `ContextKey<()>` 后按 `()` 取值），返回 `None` 而不是 panic。
//!
//! # 取消语义
//!
//! - `abort` 只生效一次：`aborted` 的原子抢占成功者写入 reason、取消 token、同步调用监听器；
//!   后来的 `abort` 直接返回，不覆盖 reason。
//! - 组合信号（[`AbortSignal::any`]）是一个**新的**信号，来源只持有指向它的 `Weak`，因此
//!   不形成引用环；首个中止的来源提供 reason。若进入时已有来源中止，直接采用它的 reason
//!   并提前返回，不再订阅其余来源。
//! - [`without_abort_signal`] 不是"还原父信号"，而是在值链顶压入一个 `None` 遮蔽层；更外层
//!   的信号因此对这个上下文不可见。
//! - 两个等待入口对"底层计算"的处理不同：[`await_with_context`] 取消时返回 `AbortError`
//!   并 drop 包装的 future；[`await_handle_with_context`] 取消时只丢弃 `JoinHandle`
//!   （detach），已 spawn 的任务继续在运行时里执行。
//!
//! # 并发与失败模式
//!
//! `AbortSignal` 通过 `Arc` 共享，可跨线程克隆。内部只有三把 `std::sync::Mutex`（reason、
//! callbacks 与组合信号的 keep_alive）和若干原子量；`abort` 在调用监听器**之前**用
//! `mem::take` 取走整个回调列表，因此监听器执行期间不持有锁：监听器里再次 `abort`（幂等）、
//! 注册新监听器（已中止，不会再触发）或 drop 订阅都不会死锁。并发调用 `abort` 时由
//! `aborted` 的原子抢占串行化。
//!
//! 严格按"先 `is_aborted()`、后 `reason()`"观察的并发读者可能在两步之间看到
//! `aborted == true` 而 `reason == None`（reason 在抢占成功后写入）；等待 token 的路径不受
//! 影响，因为 `token.cancel()` 在 reason 写入之后才执行。锁中毒由 `lock_reason`/
//! `lock_callbacks` 用 `into_inner` 恢复（`AbortSubscription::drop` 例外：中毒时静默跳过
//! 移除），不会让信号永久不可用。
//!
//! # 错误模型
//!
//! 两套类型不要混用：[`AbortReason`] 是 `signal.reason` 的载体，由取消方给出；
//! [`AbortError`] 是 TS `abortError(signal)` 的结果，由等待方在取消时得到。`Error` 原因原样
//! 透传成 [`AbortError::Reason`]；`DOMException` 与非 `Error` 值分别走两条不同的 `Dom` 文案
//! （见 [`AbortError`] 的文档）。[`trace_in`]/[`trace_in_try`] 的失败统一是 `TelemetryError`，
//! 本模块不做转换。
//!
//! # 阅读顺序
//!
//! [`AbortReason`] → [`AbortError`] → [`AbortSignal`] → [`Context`]／[`ContextKey`] →
//! `with_*` 组合子 → `await_*` 与 [`trace_in`]。

use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use kiana_telemetry::index::{
    BoxFuture, SpanOptions, TelemetryContextExt, TelemetryError, TelemetrySpan,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// `Context` 的共享句柄（TS 的接口是引用语义，Rust 用 `Arc` 传递）。
///
/// 克隆只增加引用计数，所有克隆指向同一条不可变值链；`Arc::ptr_eq` 可判断两个句柄是不是
/// 同一层。`dyn Context` 是 `Send + Sync`，所以 `ContextRef` 可以跨任务、跨线程传递。
/// 取值用 [`ContextKey::get`]，取取消信号用 [`abort_signal_of`]。
pub type ContextRef = Arc<dyn Context>;

/// 一次中止的原因（`AbortSignal.reason`）。
///
/// TS 的 `signal.reason` 可以是任意值；`abortError` 只在它是 `Error` 实例时原样返回它。
///
/// `Error` 分支共享 `Arc`，`DomException` 分支复制两个 `String`，`Value` 分支是一次
/// `serde_json::Value` 克隆；`abort` 给每个监听器各克隆一份，`reason()` 也返回克隆，因此
/// 调用方拿到的副本与信号之间没有借用关系。
#[derive(Clone)]
pub enum AbortReason {
    /// 一个 `Error` 实例（`abortError` 原样返回它）。
    Error(Arc<dyn std::error::Error + Send + Sync>),
    /// 一个 `DOMException`（`Controller.abort()` 不传参数时的缺省原因）：
    /// `name = "AbortError"`、`message = "This operation was aborted"`。
    ///
    /// 它也是 `Error` 实例，因此 `abortError` **原样**返回它（Node 探针确认 `same: true`）；
    /// 与 `Value` 分支的回落 DOMException（`message = "The operation was aborted"`）不是同一个东西。
    DomException {
        /// `error.name`（缺省 `"AbortError"`）。
        name: String,
        /// `error.message`（缺省 `"This operation was aborted"`）。
        message: String,
    },
    /// 任意 JS 值（`abortError` 忽略它，返回 DOMException 等价物）。
    Value(Value),
}

impl AbortReason {
    /// 把一个错误包成原因。
    pub fn error(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        AbortReason::Error(Arc::new(error))
    }

    /// 把一个已共享的错误包成原因。
    pub fn error_arc(error: Arc<dyn std::error::Error + Send + Sync>) -> Self {
        AbortReason::Error(error)
    }

    /// 任意值。
    pub fn value(value: Value) -> Self {
        AbortReason::Value(value)
    }

    /// `controller.abort()`（不传参数）产生的缺省原因：
    /// `new DOMException("This operation was aborted", "AbortError")`。
    pub fn dom_exception() -> Self {
        AbortReason::DomException {
            name: "AbortError".to_owned(),
            message: "This operation was aborted".to_owned(),
        }
    }

    /// 一个自定义的 `DOMException`（`controller.abort(new DOMException(message, name))`）。
    ///
    /// 注意参数顺序是 `(name, message)`，与 DOM 构造函数 `new DOMException(message, name)`
    /// 相反，不要传反。
    pub fn dom_exception_with(name: impl Into<String>, message: impl Into<String>) -> Self {
        AbortReason::DomException {
            name: name.into(),
            message: message.into(),
        }
    }

    /// 该原因是不是 `Error` 实例（`DOMException` 在 JS 里也是 `Error` 实例）。
    pub fn is_error(&self) -> bool {
        !matches!(self, AbortReason::Value(_))
    }

    /// 该原因是不是 `Error` 实例，且能取出底层错误（`DOMException` 走 [`AbortError`] 的 `Dom`
    /// 分支，不在这里返回）。
    pub fn as_error(&self) -> Option<&Arc<dyn std::error::Error + Send + Sync>> {
        match self {
            AbortReason::Error(error) => Some(error),
            AbortReason::DomException { .. } | AbortReason::Value(_) => None,
        }
    }

    /// 该原因的 JSON 视图（`Value` 分支原样，`Error`/`DOMException` 分支是 `{name, message}`）。
    ///
    /// 注意：真实 JS 的 `JSON.stringify(new Error(...))` 是 `{}`；这里给出可用的
    /// `{name, message}` 视图（与 [`crate::errors::ChordError`] 的 `cause` 视图一致）。
    pub fn to_json(&self) -> Value {
        match self {
            AbortReason::Error(error) => serde_json::json!({
                "name": "Error",
                "message": error.to_string(),
            }),
            AbortReason::DomException { name, message } => serde_json::json!({
                "name": name,
                "message": message,
            }),
            AbortReason::Value(value) => value.clone(),
        }
    }
}

impl Debug for AbortReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AbortReason::Error(error) => f.debug_tuple("Error").field(&error.to_string()).finish(),
            AbortReason::DomException { name, message } => f
                .debug_struct("DomException")
                .field("name", name)
                .field("message", message)
                .finish(),
            AbortReason::Value(value) => f.debug_tuple("Value").field(value).finish(),
        }
    }
}

/// TS 的 `abortError(signal)` 的结果：原错误（若是 `Error`），或回落的
/// `new DOMException("The operation was aborted", "AbortError")`。
///
/// 两个 `Dom` 来源要分清（Node 探针逐项确认）：
/// - `controller.abort()` 的缺省原因本身是 `DOMException("This operation was aborted",
///   "AbortError")`，`abortError` 原样返回它 → `name/message` 来自 [`AbortReason::DomException`]；
/// - 原因不是 `Error` 实例（`abort("str")`、`abort(0)`、`abort(null)`）或没有原因时，
///   `abortError` **新建** `DOMException("The operation was aborted", "AbortError")`。
///
/// 等待路径（[`await_with_context`]、[`await_handle_with_context`]）在取消时返回它；它实现
/// 了 `std::error::Error`，可以原样交给 `?`/`Box<dyn Error>`。`Dom` 分支只保留
/// `name`/`message`：Rust 侧没有原 `DOMException` 的 stack/cause。
#[derive(Clone, Debug)]
pub enum AbortError {
    /// `signal.reason` 本身是一个 `Error`：原样传出（对应 `rejects.toBe(cancellation)`）。
    Reason(Arc<dyn std::error::Error + Send + Sync>),
    /// `new DOMException("The operation was aborted", "AbortError")`。
    Dom {
        /// `"AbortError"`。
        name: String,
        /// `"The operation was aborted"`。
        message: String,
    },
}

impl AbortError {
    /// `new DOMException("The operation was aborted", "AbortError")`（回落值）。
    pub fn dom() -> Self {
        AbortError::Dom {
            name: "AbortError".to_owned(),
            message: "The operation was aborted".to_owned(),
        }
    }

    /// TS 的 `error.name`。
    ///
    /// `Reason` 分支固定返回 `"Error"`：Rust 的 `dyn Error` 没有 `name` 字段，TS 里带自定义
    /// `name` 的错误（例如 `Object.assign(new Error(..), {name: "AbortError"})`）在包装成
    /// [`AbortReason::error`] 时不会保留 name。
    pub fn name(&self) -> &str {
        match self {
            AbortError::Dom { name, .. } => name,
            AbortError::Reason(_) => "Error",
        }
    }

    /// TS 的 `error.message`。
    pub fn message(&self) -> String {
        match self {
            AbortError::Dom { message, .. } => message.clone(),
            AbortError::Reason(error) => error.to_string(),
        }
    }

    /// 原始原因（`Reason` 分支）。
    pub fn reason(&self) -> Option<&Arc<dyn std::error::Error + Send + Sync>> {
        match self {
            AbortError::Reason(error) => Some(error),
            AbortError::Dom { .. } => None,
        }
    }

    /// 是否是 `AbortError`（`name === "AbortError"`）。
    ///
    /// `DOMException` 原因（缺省 `abort()`）与回落值都是 `AbortError`；`Error` 分支只看名字
    /// （TS 里原因可以是 `Object.assign(new Error(...), {name: "AbortError"})`）。
    ///
    /// 在本端口里 [`AbortError::Reason`] 的 `name()` 恒为 `"Error"`，因此经
    /// [`AbortReason::error`] 传入的原因永远不会被判为 `AbortError`；上面的 TS 构造没有
    /// 等价的 name 载体。
    pub fn is_abort_error(&self) -> bool {
        self.name() == "AbortError"
    }
}

impl std::fmt::Display for AbortError {
    /// `String(error)`。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AbortError::Dom { name, message } => write!(f, "{name}: {message}"),
            AbortError::Reason(error) => f.write_str(&error.to_string()),
        }
    }
}

impl std::error::Error for AbortError {}

type AbortCallback = Arc<dyn Fn(AbortReason) + Send + Sync>;

/// `addEventListener("abort", …)` 的注册句柄；drop 即 `removeEventListener`。
///
/// TS 的 `removeEventListener` 需要传回同一个函数引用；Rust 用 RAII 句柄代替：在信号中止
/// 之前，句柄存活期间监听器一定在列表中，drop 时按 id 精确移除，调用方不必保存原始闭包。
/// 句柄持有 `Arc<SignalInner>`，因此只要订阅还在，信号本体就不会被释放。
///
/// 在 `abort` 之后 drop 是安全的：此时回调列表已被整体取空，移除找不到目标也不会报错。
pub struct AbortSubscription {
    inner: Arc<SignalInner>,
    id: u64,
}

impl Drop for AbortSubscription {
    fn drop(&mut self) {
        // 按 id 精确移除（闭包无法比较，id 是唯一标识）；锁中毒时静默跳过。
        if let Ok(mut callbacks) = self.inner.callbacks.lock() {
            callbacks.retain(|(id, _)| *id != self.id);
        }
    }
}

impl Debug for AbortSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortSubscription")
            .field("id", &self.id)
            .finish()
    }
}

/// `AbortSignal` 的共享状态（包在 `Arc` 里）。
///
/// 不变量：`aborted` 一旦为 true 就不会回到 false；`reason` 只会由抢到 `aborted` 的
/// 那一次 `abort` 写入；`callbacks` 在 `abort` 时被整体取走，之后注册的不会再触发。
struct SignalInner {
    /// 首个 `abort` 把它从 false 置为 true，之后不再变化。
    aborted: AtomicBool,
    /// 中止原因；由 `aborted` 的原子抢占保护，正常只会写入一次。
    reason: Mutex<Option<AbortReason>>,
    /// `(id, callback)`；id 由 `next_id` 分配，供订阅 drop 时精确移除。
    callbacks: Mutex<Vec<(u64, AbortCallback)>>,
    /// 单调递增的监听器 id（只在 `on_abort` 里取号）。
    next_id: AtomicU64,
    /// 供 `await_*` / 调用方 `select!` 等待的 token；在 reason 写入之后才被取消。
    token: CancellationToken,
    /// `AbortSignal.any` 组合出来的信号保留来源上的订阅，drop 时一并移除。
    keep_alive: Mutex<Vec<AbortSubscription>>,
}

impl SignalInner {
    // 中毒只可能来自持锁期间的 panic；取回内部值继续用，避免信号永久失效。
    fn lock_callbacks(&self) -> std::sync::MutexGuard<'_, Vec<(u64, AbortCallback)>> {
        self.callbacks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // 同上：reason 是普通数据，中毒后仍可安全读取。
    fn lock_reason(&self) -> std::sync::MutexGuard<'_, Option<AbortReason>> {
        self.reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// TS 的 `AbortSignal`（ARCHITECTURE §4.2：`CancellationToken` + 可选原因）。
///
/// 语义与 DOM 一致：`abort` 只生效一次（首个原因胜出）；`AbortSignal.any` 在任一来源已中止时立即
/// 中止；监听器在 `abort` 时**同步**执行一次。
///
/// 克隆共享同一份内部状态（TS 的引用语义）：通过任一克隆 `abort`，所有克隆都能看到；
/// [`AbortSignal::same_signal`] 比较的正是这份内部状态的地址。
#[derive(Clone)]
pub struct AbortSignal {
    inner: Arc<SignalInner>,
}

impl Debug for AbortSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortSignal")
            .field("aborted", &self.is_aborted())
            .finish_non_exhaustive()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        AbortSignal::new()
    }
}

impl AbortSignal {
    /// 一个未中止的信号（TS 的 `new AbortController().signal`）。
    pub fn new() -> Self {
        AbortSignal {
            inner: Arc::new(SignalInner {
                aborted: AtomicBool::new(false),
                reason: Mutex::new(None),
                callbacks: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(0),
                token: CancellationToken::new(),
                keep_alive: Mutex::new(Vec::new()),
            }),
        }
    }

    /// `controller.abort(reason)`。只生效一次；随后同步调用全部监听器。
    ///
    /// `None` 对应 `controller.abort()`：`signal.reason` 在 DOM 里是一个
    /// `DOMException("This operation was aborted", "AbortError")`（`Error` 实例），因此这里用
    /// [`AbortReason::dom_exception`]；`abortError` 会原样返回它。
    ///
    /// 注意 JS 的 `controller.abort(undefined)` 与 `controller.abort()` 等价（都是缺省原因），
    /// 而 `controller.abort(null)` 是一个 `Value` 原因 → `abortError` 回落。
    ///
    /// # 顺序与重入
    ///
    /// 1. `aborted.swap(true)` 抢占：并发的第二个 `abort` 在这里返回，不会覆盖原因；
    /// 2. 写入 reason（`reason()` 从这里开始返回值）；
    /// 3. 取消 token：等待 `token().cancelled()` 的任务被唤醒时 reason 已就绪；
    /// 4. 取走并清空监听器列表，然后在**不持锁**的情况下同步逐个调用。
    ///
    /// 监听器里再调用 `abort` 是无操作；此时 `on_abort` 注册的监听器不会再被触发（列表已被
    /// 取走，且 `aborted` 已为 true）。监听器 panic 会向 `abort` 的调用者传播，同批剩余
    /// 监听器（已收进局部 `callbacks`）随栈展开被丢弃、不会执行。
    pub fn abort(&self, reason: Option<AbortReason>) {
        // 首个 abort 胜出：抢占失败说明信号已经中止。
        if self.inner.aborted.swap(true, Ordering::SeqCst) {
            return;
        }
        // 缺省原因（`abort()` / `abort(undefined)`）是 DOM 的 DOMException。
        let reason = reason.unwrap_or_else(AbortReason::dom_exception);
        // 先写 reason 再 cancel：从 token 醒来的等待者一定能读到原因。
        *self.inner.lock_reason() = Some(reason.clone());
        self.inner.token.cancel();
        // 一次性取走列表：回调期间不持锁；之后注册的监听器不会再触发。
        let callbacks: Vec<AbortCallback> = std::mem::take(&mut *self.inner.lock_callbacks())
            .into_iter()
            .map(|(_, callback)| callback)
            .collect();
        // 同步调用（DOM 语义），每个监听器拿到同一原因的克隆。
        for callback in callbacks {
            callback(reason.clone());
        }
    }

    /// `controller.abort()`（无原因）。
    pub fn abort_without_reason(&self) {
        self.abort(None);
    }

    /// `signal.aborted`。
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::SeqCst)
    }

    /// 两个句柄是否是**同一个**信号（TS 的 `signalA === signalB`）。
    ///
    /// `withAbortSignal(signal, ctx)` 在没有父信号时直接把 `signal` 挂上去，之后
    /// `ctx.abortSignal === signal` 成立；`withCancel` 的 `AbortSignal.any` 组合出的则是新信号。
    pub fn same_signal(&self, other: &AbortSignal) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// `signal.reason`（未中止时 `None`）。
    ///
    /// 返回克隆，调用方与信号之间没有借用；与 `is_aborted()` 的并发观察窗口见模块文档
    /// "并发与失败模式"。
    pub fn reason(&self) -> Option<AbortReason> {
        self.inner.lock_reason().clone()
    }

    /// 可以被 `await`/`select!` 的取消原语（`AbortSignal` 的 tokio 形态）。
    ///
    /// 返回借用，不增加引用计数；调用方通常 `clone()` 后放进 `select!`（`CancellationToken`
    /// 的克隆共享同一取消状态）。它只承载"已取消"这一位：绕过 [`AbortSignal::abort`]
    /// 直接 `token().cancel()` **不会**写入 reason，也不会触发 [`AbortSignal::on_abort`]
    /// 的监听器；若之后没有人再调用 `abort`，[`abort_error`] 只能走无原因回落。
    pub fn token(&self) -> &CancellationToken {
        &self.inner.token
    }

    /// `signal.addEventListener("abort", fn, { once: true })`。
    ///
    /// 已经中止的信号**不会**立即回调（与 DOM 一致；`await_with_context` 自己先检查 `aborted`）。
    ///
    /// 回调在调用 `abort` 的线程上同步执行。它与 `abort` 并发注册时，要么被本次取走执行，
    /// 要么落在取走之后而永不触发。返回的 [`AbortSubscription`] 需要保留到不再需要通知为止；
    /// 提前 drop 即注销。每个监听器拿到的 [`AbortReason`] 是独立克隆，可以保存或转发。
    pub fn on_abort(
        &self,
        callback: impl Fn(AbortReason) + Send + Sync + 'static,
    ) -> AbortSubscription {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        self.inner.lock_callbacks().push((id, Arc::new(callback)));
        AbortSubscription {
            inner: Arc::clone(&self.inner),
            id,
        }
    }

    /// `AbortSignal.any([a, b, …])`：任一来源中止 → 新信号以它的原因中止（首个生效）。
    ///
    /// 已经中止的来源让新信号立即中止。订阅挂在组合信号的 `keep_alive` 上，drop 组合信号即移除；
    /// 来源只持有指向组合信号的 `Weak`，因此不形成环。
    ///
    /// 空列表返回一个没有来源的新信号（不会因任何来源而中止；直接调用它的 `abort` 仍可
    /// 中止）。来源不去重：同一个信号传入两次会注册两个监听器，第二个触发时第一次的
    /// `abort` 已生效，因此没有额外效果。组合只观察来源，**不会**把自身的取消反向传播给
    /// 来源。
    pub fn any(signals: &[AbortSignal]) -> AbortSignal {
        let combined = AbortSignal::new();
        for signal in signals {
            if signal.is_aborted() {
                combined.abort(signal.reason());
                return combined;
            }
        }
        let weak: Weak<SignalInner> = Arc::downgrade(&combined.inner);
        let mut subscriptions = Vec::with_capacity(signals.len());
        for signal in signals {
            let weak = weak.clone();
            subscriptions.push(signal.on_abort(move |reason| {
                if let Some(inner) = weak.upgrade() {
                    AbortSignal { inner }.abort(Some(reason));
                }
            }));
        }
        combined
            .inner
            .keep_alive
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(subscriptions);
        combined
    }
}

/// 一个不可变的、调用作用域内的值（TS: `interface Context`）。
///
/// # 实现契约
///
/// - 实现必须不可变：`&self` 上的查询不得改变可观察状态，因为同一个 [`ContextRef`] 会被
///   多个任务并发共享；
/// - `value` 对未知的键必须返回 `None`（不能 panic），[`ContextKey::get`] 依赖这一点表示
///   "没有这个键"；
/// - `abort_signal` 返回的 [`AbortSignal`] 是共享句柄，克隆后仍指向同一信号；返回 `None`
///   表示这个上下文不可取消；
/// - `to_string_value` 只用于展示，不参与任何匹配。
///
/// 本 crate 只提供两个非公开实现（`EmptyContext` 与 `ContextValueContext`）；trait 本身是
/// 公开的，外部实现必须满足上面的契约，并在 `value` 中用 [`KeyIdentity`] 做身份匹配。
pub trait Context: Send + Sync + Debug {
    /// `AbortSignal | undefined`：这个上下文**看到**的取消信号。
    ///
    /// `None` 表示调用方不可取消；[`without_abort_signal`] 派生的上下文即使祖先有信号，
    /// 这里也返回 `None`（遮蔽）。
    fn abort_signal(&self) -> Option<AbortSignal>;

    /// `TelemetrySpan`：缺省是 `NOOP_TELEMETRY_CONTEXT`（进程级单例）。
    fn telemetry(&self) -> Arc<dyn TelemetrySpan>;

    /// `value(key)`：类型擦除的值（TS 的泛型方法）。键的身份是 [`KeyIdentity`]。
    ///
    /// 返回 `None` 表示"这个上下文里没有这个键"。泛型恢复由 [`ContextKey::get`] 完成。
    fn value(&self, key: &KeyIdentity) -> Option<ContextValue>;

    /// `toString()`（TS 的方法）。
    fn to_string_value(&self) -> String;
}

impl std::fmt::Display for dyn Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string_value())
    }
}

/// `String(context)` 的便捷入口。
pub fn context_string(context: &dyn Context) -> String {
    context.to_string_value()
}

/// `value()` 的类型擦除载体。
///
/// 值以 `Arc<dyn Any + Send + Sync>` 存放：值链的层持有一份共享所有权，[`ContextKey::get`]
/// 通过 `downcast_ref` 取引用再克隆出 `T`。`Send + Sync` 是 [`ContextRef`] 能跨任务传递的
/// 前提。
pub type ContextValue = Arc<dyn Any + Send + Sync>;

/// 键的身份：TS 的 `Symbol`。`value()` 只看它（泛型参数不参与运行期判别）。
///
/// 底层 `Arc<()>` 的分配地址就是身份，不存放任何数据；`same_token` 用 `Arc::ptr_eq` 比较。
/// 它本身不实现 `Clone`，克隆键统一走 [`ContextKey`] 的 `Clone`（共享同一个 token）。
pub struct KeyIdentity {
    token: Arc<()>,
}

impl KeyIdentity {
    /// 是否是同一个键（TS: `key.token === other.token`）。
    ///
    /// 只比较 token 指针；描述文本、泛型参数都不参与。
    pub fn same_token(&self, other: &KeyIdentity) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}

/// 类型化身份：`Context` 携带的一个值（TS: `ContextKey<T>`）。
///
/// 泛型参数只用于编译期约束：`PhantomData<fn() -> T>` 不持有 `T`，也不会把 `T` 的
/// `Send`/`Sync` 要求带进 `ContextKey<T>`。运行期身份只有 [`KeyIdentity`] 的 token，因此
/// 两个不同 `T` 的键只要 token 相同就指向同一个槽位。默认参数 `T = ()` 让值链可以存放
/// 类型擦除后的键（见 [`ContextKey::erase`]）。
pub struct ContextKey<T = ()> {
    identity: KeyIdentity,
    description: Option<String>,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> Clone for ContextKey<T> {
    /// 克隆键（token 是 `Arc`，两个克隆是同一个键）。
    fn clone(&self) -> Self {
        ContextKey {
            identity: KeyIdentity {
                token: Arc::clone(&self.identity.token),
            },
            description: self.description.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> Debug for ContextKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextKey")
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

impl<T: Clone + Send + Sync + 'static> ContextKey<T> {
    /// 在 `context` 里取这个键的值（TS: `context.value(key)`）。
    ///
    /// 对 `ContextKey<Option<X>>`（`withAbortSignal` 的键）返回 `Some(None)` 表示"遮蔽"，
    /// `None` 表示"这个上下文里没有这个键"。
    ///
    /// 返回 `None` 的另一种情况是类型不匹配：token 命中，但那一层存的值不是 `T`（例如对
    /// [`ContextKey::erase`] 出来的键按 `()` 取值），`downcast_ref::<T>()` 失败后静默返回
    /// `None`，不会 panic。命中时值被克隆一份（因此要求 `T: Clone`）；查找沿父链线性走查。
    pub fn get(&self, context: &dyn Context) -> Option<T> {
        context.value(&self.identity)?.downcast_ref::<T>().cloned()
    }
}

impl<T> ContextKey<T> {
    /// `Symbol(description).description`。
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// 键的身份（`Context::value` 用它匹配）。
    pub fn identity(&self) -> &KeyIdentity {
        &self.identity
    }

    /// `Symbol` 的身份（两个键只有 token 指针相等时才是同一个键）。
    pub fn same_token(&self, other: &ContextKey) -> bool {
        self.identity.same_token(&other.identity)
    }

    /// `key.token.description ?? "anonymous"`（`toString()` 用）。
    pub fn display_name(&self) -> String {
        self.description
            .clone()
            .unwrap_or_else(|| "anonymous".to_owned())
    }

    /// 擦除值类型，保留 token 与描述。
    ///
    /// 返回的键与原键 token 相同，因此仍指向同一个值；但它的静态类型是 `ContextKey<()>`，
    /// 用 [`ContextKey::get`] 按 `()` 取值通常只会得到 `None`（类型不匹配，除非那一层真的
    /// 存了 `()`）。值链内部只用擦除键做身份匹配，不做类型恢复。
    pub fn erase(&self) -> ContextKey {
        ContextKey {
            identity: KeyIdentity {
                token: Arc::clone(&self.identity.token),
            },
            description: self.description.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

/// `EmptyContext(name)`（非导出）。
///
/// 值链的根：`value()` 恒为 `None`，`abort_signal()` 恒为 `None`，`telemetry()` 恒为 NOOP
/// 单例。`BACKGROUND_CONTEXT` 与 `TODO_CONTEXT` 是它的两个进程级实例，只有名字不同
/// （测试 `provides_distinct_empty_root_contexts` 断言二者 `Arc::ptr_eq` 为假）。
#[derive(Debug)]
struct EmptyContext {
    /// 构造时固定，只用于 `to_string_value`。
    name: String,
}

impl Context for EmptyContext {
    fn abort_signal(&self) -> Option<AbortSignal> {
        None
    }

    fn telemetry(&self) -> Arc<dyn TelemetrySpan> {
        kiana_telemetry::noop::noop_telemetry_context()
    }

    fn value(&self, _key: &KeyIdentity) -> Option<ContextValue> {
        None
    }

    fn to_string_value(&self) -> String {
        self.name.clone()
    }
}

/// `ContextValue(parent, key, value)`（非导出）。
///
/// 值链上的一层：父上下文 + 一个键值对（TS `class ContextValue`）。层不可变；`value` 先比对
/// 本层 token，再委托父层，因此本层的同名键逐层遮蔽（父层本身不受影响）。
/// `to_string_value` 的形状是 `"{parent}.WithValue({description})"`，匿名键的 description
/// 显示为 `anonymous`；集成测试按这个字符串断言。
struct ContextValueContext {
    /// 上一层的共享句柄；本层不会修改它。
    parent: ContextRef,
    /// 已擦除类型参数的键，只用于 token 比较与展示。
    key: ContextKey,
    /// 值的类型擦除形式；[`ContextKey::get`] 负责 downcast 回具体类型。
    value: ContextValue,
}

impl Debug for ContextValueContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextValue")
            .field("parent", &*self.parent)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Context for ContextValueContext {
    /// TS 的 `BaseContext.abortSignal` getter：`value(ABORT_SIGNAL_CONTEXT_KEY)`。
    ///
    /// **必须**走 `value()`（先看自身、再委托父）：`withAbortSignal` / `withoutAbortSignal`
    /// 就是把"组合信号"或"遮蔽的 `undefined`"作为普通值挂在 `ABORT_SIGNAL_CONTEXT_KEY` 上。
    fn abort_signal(&self) -> Option<AbortSignal> {
        ABORT_SIGNAL_CONTEXT_KEY.get(self).flatten()
    }

    /// TS 的 `BaseContext.telemetry` getter：`value(TELEMETRY_CONTEXT_KEY) ?? NOOP_TELEMETRY_CONTEXT`。
    fn telemetry(&self) -> Arc<dyn TelemetrySpan> {
        TELEMETRY_CONTEXT_KEY
            .get(self)
            .unwrap_or_else(kiana_telemetry::noop::noop_telemetry_context)
    }

    fn value(&self, key: &KeyIdentity) -> Option<ContextValue> {
        if key.same_token(&self.key.identity) {
            return Some(Arc::clone(&self.value));
        }
        self.parent.value(key)
    }

    fn to_string_value(&self) -> String {
        format!(
            "{}.WithValue({})",
            self.parent.to_string_value(),
            self.key.display_name()
        )
    }
}

/// `ABORT_SIGNAL_CONTEXT_KEY`（不导出）。
///
/// 必须是进程级单例：`value()` 按 token 指针匹配，如果每次查询都新建键，就永远命中不了
/// [`with_abort_signal`] / [`without_abort_signal`] 写入的那一层。`LazyLock` 保证 token
/// 只创建一次。
static ABORT_SIGNAL_CONTEXT_KEY: LazyLock<ContextKey<Option<AbortSignal>>> =
    LazyLock::new(|| ContextKey {
        identity: KeyIdentity {
            token: Arc::new(()),
        },
        description: Some("chord.abortSignal".to_owned()),
        _marker: std::marker::PhantomData,
    });

/// `TELEMETRY_CONTEXT_KEY`（不导出）。
///
/// 与 `ABORT_SIGNAL_CONTEXT_KEY` 同理：进程级单例，token 只创建一次，否则
/// [`with_telemetry`] 写入的 span 永远不会被 `telemetry()` 读到。
static TELEMETRY_CONTEXT_KEY: LazyLock<ContextKey<Arc<dyn TelemetrySpan>>> =
    LazyLock::new(|| ContextKey {
        identity: KeyIdentity {
            token: Arc::new(()),
        },
        description: Some("chord.telemetry".to_owned()),
        _marker: std::marker::PhantomData,
    });

/// `BACKGROUND_CONTEXT = new EmptyContext("[Context BACKGROUND_CONTEXT]")`。
///
/// 进程级单例（`LazyLock`）：没有取消信号、没有业务值、telemetry 为 NOOP。克隆
/// [`ContextRef`] 只增加引用计数；派生上下文时它通常作为值链的根。
pub static BACKGROUND_CONTEXT: LazyLock<ContextRef> = LazyLock::new(|| {
    Arc::new(EmptyContext {
        name: "[Context BACKGROUND_CONTEXT]".to_owned(),
    }) as ContextRef
});

/// `TODO_CONTEXT = new EmptyContext("[Context TODO_CONTEXT]")`。
///
/// 与 [`BACKGROUND_CONTEXT`] 结构相同、语义同为"空根"，但它是**另一个**实例：名字不同，
/// `Arc::ptr_eq` 为假，`to_string_value` 也不同。
pub static TODO_CONTEXT: LazyLock<ContextRef> = LazyLock::new(|| {
    Arc::new(EmptyContext {
        name: "[Context TODO_CONTEXT]".to_owned(),
    }) as ContextRef
});

/// `createContextKey<T>(description)`：`Object.freeze({ token: Symbol(description) })`。
///
/// 每次调用都创建新 token：即使描述相同，两次创建出的键也互不匹配（`Symbol` 语义）。
/// `description` 只用于 `to_string_value` 展示与调试，不参与查找。
pub fn create_context_key<T>(description: impl Into<String>) -> ContextKey<T> {
    ContextKey {
        identity: KeyIdentity {
            token: Arc::new(()),
        },
        description: Some(description.into()),
        _marker: std::marker::PhantomData,
    }
}

/// `createContextKey<T>()`：`Symbol.description` 为 `undefined`（显示为 `anonymous`）。
///
/// 同样每次创建新 token；匿名键之间没有特殊关系，不会互相命中。
pub fn create_context_key_anonymous<T>() -> ContextKey<T> {
    ContextKey {
        identity: KeyIdentity {
            token: Arc::new(()),
        },
        description: None,
        _marker: std::marker::PhantomData,
    }
}

/// 派生一个"增加或替换一个值"的上下文。
///
/// TS: `withContextValue(key, value, parent)`（**参数顺序：key, value, parent**）。
///
/// 新层只持有父层的 `Arc`，并在前面加一个键值对；父上下文不被修改，可以继续被其他派生
/// 共享（测试 `layers_typed_values_without_modifying_parents`）。同一个键写在子层会遮蔽
/// 父层的旧值，但旧值仍可从父上下文读到。
///
/// `T` 必须 `Send + Sync + 'static`：值被擦除成 [`ContextValue`] 放进 `Arc`，而
/// [`ContextRef`] 需要跨线程/跨任务传递。构建是 O(1)，查找是 O(层数)。
pub fn with_context_value<T: Send + Sync + 'static>(
    key: ContextKey<T>,
    value: T,
    parent: ContextRef,
) -> ContextRef {
    let erased: ContextValue = Arc::new(value);
    Arc::new(ContextValueContext {
        parent,
        key: key.erase(),
        value: erased,
    })
}

/// `context.abortSignal`（`BaseContext` 的 getter）。
pub fn abort_signal_of(context: &dyn Context) -> Option<AbortSignal> {
    context.abort_signal()
}

/// `context.telemetry`（缺省 `NOOP_TELEMETRY_CONTEXT`）。
pub fn telemetry_of(context: &dyn Context) -> Arc<dyn TelemetrySpan> {
    context.telemetry()
}

/// 派生一个显式的 telemetry 父 span，不改变取消与业务值。
///
/// TS: `withTelemetry(telemetry, context)`（**参数顺序：telemetry, context**）。
///
/// telemetry 只是值链上的普通键（`TELEMETRY_CONTEXT_KEY`），所以本函数不触碰取消信号与
/// 其他业务键；在内层再调用一次会遮蔽外层 span（测试里原上下文没有显式 telemetry，因此
/// 断言它仍取到 NOOP、内层取到新 span）。Rust 版本返回擦除后的 [`ContextRef`]（TS 的返回
/// 类型是 `Context & { telemetry: T }`）；需要具体 span 的调用方自己保留 `Arc`。
pub fn with_telemetry(telemetry: Arc<dyn TelemetrySpan>, context: ContextRef) -> ContextRef {
    with_context_value(TELEMETRY_CONTEXT_KEY.clone(), telemetry, context)
}

/// 派生一个"父信号或给定信号任一取消即取消"的上下文；父上下文不变。
///
/// TS: `withAbortSignal(signal, context)`：父信号存在时 `AbortSignal.any([parent, signal])`，
/// 否则直接用 `signal`。
///
/// 两个分支的区别是身份：父上下文没有信号时，给定信号被**原样**挂到值链上（不复制、不
/// 包装），因此 `abort_signal_of` 取回的信号与 `signal` 满足 `same_signal`；父上下文有信号
/// 时生成一个新的组合信号，reason 取首个中止来源的 reason。返回的新上下文共享父层的一切
/// 其他值，父上下文本身不变。
pub fn with_abort_signal(signal: AbortSignal, context: ContextRef) -> ContextRef {
    let combined = match abort_signal_of(context.as_ref()) {
        None => signal,
        Some(parent) => AbortSignal::any(&[parent, signal]),
    };
    with_context_value(ABORT_SIGNAL_CONTEXT_KEY.clone(), Some(combined), context)
}

/// 派生一个保留所有值、但**不**受调用方取消影响的上下文。只用于必须执行的清理。
///
/// TS: `withoutAbortSignal(context)`（**遮蔽**而非移除）。
///
/// 实现是在值链顶压入 `ABORT_SIGNAL_CONTEXT_KEY = None` 的一层，而不是删除父层的信号：
/// 值查找命中本层后停止，于是父信号被遮蔽。其他键（包括 telemetry）与祖先层的业务值仍然
/// 可见（测试 `masks_caller_cancellation_for_mandatory_cleanup` 断言值被保留）。
/// 之后再调用 [`with_abort_signal`] 仍可在这层之上挂一个新的信号。
pub fn without_abort_signal(context: ContextRef) -> ContextRef {
    with_context_value(ABORT_SIGNAL_CONTEXT_KEY.clone(), None, context)
}

/// [`with_cancel`] 的返回值（TS: `{ readonly context; readonly cancel }`）。
///
/// `cancel` 中止的是 [`with_cancel`] 内部新建的 controller 信号，取消效果再经由 `context`
/// 传播到由它派生的子上下文。父上下文没有信号时，[`with_abort_signal`] 把这个 controller
/// 信号**原样**挂到 `context` 上，它就是 `abort_signal_of(context)` 返回的那个信号；
/// 父上下文已有信号时，`context` 看到的是 `AbortSignal::any([parent, controller])` 组合出的
/// **新**信号，`cancel` 通过组合信号的订阅传播使其中止（二者不是同一个信号，
/// `same_signal` 为假）。两条路径都不影响父上下文。`cancel` 是 `Arc<dyn Fn(..)>`，可以
/// 单独克隆出去；`CancelHandle` 本身不实现 `Clone`。
///
/// 重复 `cancel` 是无操作：第二次 `abort` 会输给第一次，reason 保持不变。
pub struct CancelHandle {
    /// 独立的子上下文。
    pub context: ContextRef,
    /// `cancel(reason?)`：中止子上下文的信号；`None` 表示缺省原因。
    pub cancel: Arc<dyn Fn(Option<AbortReason>) + Send + Sync>,
}

impl CancelHandle {
    /// `cancel()`（无原因）。
    ///
    /// 等价于 `controller.abort()`：写入的原因是 [`AbortReason::dom_exception`]
    /// （`DOMException("This operation was aborted", "AbortError")`）。
    pub fn cancel(&self) {
        (self.cancel)(None);
    }

    /// `cancel(reason)`。
    pub fn cancel_with(&self, reason: AbortReason) {
        (self.cancel)(Some(reason));
    }
}

impl Debug for CancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelHandle")
            .field("context", &*self.context)
            .finish_non_exhaustive()
    }
}

/// 派生一个可独立取消的子上下文。
///
/// TS: `withCancel(context)` → `{ context: withAbortSignal(controller.signal, context), cancel }`。
///
/// 父上下文有信号时，子上下文同时受父信号与自身控制器约束：父信号中止会传播下来；
/// `cancel` 只中止这个子上下文（及其派生），不影响父上下文或兄弟子上下文（测试
/// `inherits_parent_cancellation_and_isolates_child_cancellation`）。父上下文没有信号时，
/// 子上下文只受控制器约束。
pub fn with_cancel(context: ContextRef) -> CancelHandle {
    let controller = AbortSignal::new();
    let cancel_signal = controller.clone();
    CancelHandle {
        context: with_abort_signal(controller, context),
        cancel: Arc::new(move |reason| cancel_signal.abort(reason)),
    }
}

/// 观察一个 future 直到它 settle 或调用被取消。取消只 reject 这个等待者，不取消底层 promise。
///
/// TS: `awaitWithContext(promise, context)`。
///
/// 注意 Rust 的 future 随本函数一起被 drop（TS 里底层 promise 继续跑）；需要那一语义时用
/// [`await_handle_with_context`]。
///
/// # 行为
///
/// - 上下文没有信号：直接 `.await`，不注册任何监听；
/// - 调用时信号已中止：**不轮询** `future`，立即返回 [`abort_error`]；
/// - 等待期间中止：返回取消错误，`future` 随本函数一起被 drop；
/// - `select!` 用 `biased` 且业务分支在前：二者同时就绪时返回业务结果。
///
/// 这里用 `CancellationToken`（克隆句柄）而不是 `on_abort` 订阅：token 在已取消时
/// `cancelled()` 立即就绪，因此即使 `abort` 恰好发生在 `is_aborted()` 检查与 `select!`
/// 之间，也不会漏掉唤醒。
pub async fn await_with_context<T, F>(future: F, context: &dyn Context) -> Result<T, AbortError>
where
    F: Future<Output = T>,
{
    // 没有信号：不可取消，直接等待。
    let Some(signal) = abort_signal_of(context) else {
        return Ok(future.await);
    };
    // 调用前已中止：不轮询 future，立即以取消错误返回。
    if signal.is_aborted() {
        return Err(abort_error(&signal));
    }
    // 克隆 token 而不是持有订阅：已取消的 token 会让 cancelled() 立即就绪。
    let token = signal.token().clone();
    tokio::pin!(future);
    tokio::select! {
        biased;
        value = &mut future => Ok(value),
        _ = token.cancelled() => Err(abort_error(&signal)),
    }
}

/// [`await_with_context`] 的 `JoinHandle` 版本：取消只放弃等待，任务继续运行（tokio 的
/// `JoinHandle` drop 只 detach，任务仍由运行时继续执行）。
///
/// # 返回值
///
/// 外层 `Err(AbortError)` 表示等待被取消；`Ok(join_result)` 里的 `Result` 是任务本身的完成
/// 情况（含 panic/运行时取消的 `JoinError`），两者不要混淆。
///
/// 取消（或调用前已中止）时 `handle` 被丢弃：tokio 的 `JoinHandle` drop 只 detach，任务继续
/// 在运行时里执行，返回值无人接收。这正是 TS 里 promise 继续跑、只是没人 await 的语义。
pub async fn await_handle_with_context<T>(
    handle: tokio::task::JoinHandle<T>,
    context: &dyn Context,
) -> Result<Result<T, tokio::task::JoinError>, AbortError> {
    // 没有信号：直接等任务结束（外层 Ok 表示等待没有被取消）。
    let Some(signal) = abort_signal_of(context) else {
        return Ok(handle.await);
    };
    // 调用前已中止：不 await 任务；handle 在此被 drop（detach，任务继续跑）。
    if signal.is_aborted() {
        return Err(abort_error(&signal));
    }
    let token = signal.token().clone();
    tokio::select! {
        biased;
        value = handle => Ok(value),
        _ = token.cancelled() => Err(abort_error(&signal)),
    }
}

/// `signal.reason instanceof Error ? reason : new DOMException("The operation was aborted",
/// "AbortError")`。
///
/// `DOMException` 在 JS 里是 `Error` 实例，所以缺省原因（`controller.abort()`）走的是"原样
/// 返回"分支：`name`/`message` 是 `"AbortError"` / `"This operation was aborted"`。只有非
/// `Error` 的 JS 值原因（字符串、数字、`null`、对象）才回落到
/// `"The operation was aborted"`。
///
/// 未中止的信号（`reason()` 为 `None`）也走回落分支，返回回落 `DOMException` 而不是 panic。
/// 等待路径（[`await_with_context`] / [`await_handle_with_context`]）通常在 reason 已写入后
/// 才调用：`abort` 先写 reason 再取消 token，从 token 醒来的等待者能读到原因。但在并发
/// `abort` 置位 `aborted` 与写入 reason 之间的窗口里（见模块文档"并发与失败模式"），预检查
/// `is_aborted()` 已经通过而 `reason()` 仍是 `None`，此时等待路径同样命中回落分支；外部通过
/// [`AbortSignal::token`] 直接 `cancel()` 时，`select!` 的 token 分支也不会等到 reason。
/// 返回值拥有自己的数据（`Error` 分支共享 `Arc`），不借用信号。
pub fn abort_error(signal: &AbortSignal) -> AbortError {
    match signal.reason() {
        Some(AbortReason::Error(error)) => AbortError::Reason(error),
        Some(AbortReason::DomException { name, message }) => AbortError::Dom { name, message },
        Some(AbortReason::Value(_)) | None => AbortError::dom(),
    }
}

/// 在派生的子上下文里跑一个 child span，该上下文的 telemetry 成员就是这个 span。
///
/// TS: `traceIn(options, fn, context)` =
/// `context.telemetry.startSpan(options, span => fn(withTelemetry(span, context)))`。
///
/// 回调返回的 `T` 原样成为结果；telemetry 层错误以 `Err` 返回。**没有**
/// "无 telemetry 分支"：NOOP 也走 `startSpan`（TS 测试 `traceIn({name:"noop"}, …, BACKGROUND_CONTEXT)`
/// 断言回调里的 `child.telemetry.addLink(…)` 是可用但无副作用的）。
///
/// 具体流程：
/// 1. 从传入的 `context` 读一次 telemetry（[`telemetry_of`]）；
/// 2. 在该 telemetry 上调用 `start_span_typed`，由适配器创建 span 并决定何时结算；
/// 3. 回调收到 `with_telemetry(span, context)`：业务值与取消信号来自原上下文，telemetry
///    换成新 span（集成测试用 `same_signal` 断言取消信号没有被换掉）；
/// 4. 回调的返回值装箱后还原成 `T`，**不**参与 span 状态判定（要让 `Err` 标红用
///    [`trace_in_try`]）。
///
/// `T: Send + 'static` 与 `F: 'static` 来自 `kiana-telemetry` 的类型擦除：结果被装箱成
/// `Any`，因此不能借用调用栈上的数据。
pub async fn trace_in<T, F>(
    options: SpanOptions,
    f: F,
    context: ContextRef,
) -> Result<T, TelemetryError>
where
    T: Send + 'static,
    F: FnOnce(ContextRef) -> BoxFuture<'static, T> + Send + 'static,
{
    let telemetry = telemetry_of(context.as_ref());
    let parent = ContextRef::clone(&context);
    telemetry
        .start_span_typed(options, move |span| f(with_telemetry(span, parent)))
        .await
}

/// [`trace_in`] 的"异常通道"版本：回调返回 `Result`，`Err` 表示回调失败
/// （span 记为 error，错误原样传出）。
///
/// 与 [`trace_in`] 的唯一区别是回调输出类型：`Err` 走适配器的"回调抛出"路径（除非回调
/// 已显式设置 span 状态），`Ok` 正常结算。外层 `Err` 也可能来自适配器的拒绝：按
/// `TelemetryContext::start_span` 的契约，适配器拒绝与回调返回 `Err` 都沿 future 的
/// `Err` 通道传播，因此不一定都是回调返回的那个错误。
pub async fn trace_in_try<T, F>(
    options: SpanOptions,
    f: F,
    context: ContextRef,
) -> Result<T, TelemetryError>
where
    T: Send + 'static,
    F: FnOnce(ContextRef) -> BoxFuture<'static, Result<T, TelemetryError>> + Send + 'static,
{
    let telemetry = telemetry_of(context.as_ref());
    let parent = ContextRef::clone(&context);
    telemetry
        .start_span_try(options, move |span| f(with_telemetry(span, parent)))
        .await
}

// 单元测试：覆盖值分层、键身份、取消传播与 trace 的核心路径；断言是手写字面量，不读
// `tests/fixtures/context_golden.json`（对照 TS 生成的夹具测试在 `tests/context.rs`）。
#[cfg(test)]
mod tests {
    use super::*;
    use kiana_telemetry::{InMemoryTelemetryContext, SpanOptions};

    fn background() -> ContextRef {
        ContextRef::clone(&BACKGROUND_CONTEXT)
    }

    fn options(name: &str) -> SpanOptions {
        SpanOptions {
            name: name.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn provides_distinct_empty_root_contexts() {
        let key = create_context_key::<String>("value");
        assert!(!Arc::ptr_eq(&BACKGROUND_CONTEXT, &TODO_CONTEXT));
        assert!(TODO_CONTEXT.abort_signal().is_none());
        assert!(Arc::ptr_eq(
            &TODO_CONTEXT.telemetry(),
            &kiana_telemetry::noop::noop_telemetry_context()
        ));
        assert!(Arc::ptr_eq(
            &BACKGROUND_CONTEXT.telemetry(),
            &kiana_telemetry::noop::noop_telemetry_context()
        ));
        assert!(key.get(TODO_CONTEXT.as_ref()).is_none());
        assert_eq!(
            context_string(BACKGROUND_CONTEXT.as_ref()),
            "[Context BACKGROUND_CONTEXT]"
        );
        assert_eq!(
            context_string(TODO_CONTEXT.as_ref()),
            "[Context TODO_CONTEXT]"
        );
    }

    #[test]
    fn layers_typed_values_without_modifying_parents() {
        let first_key = create_context_key::<String>("first");
        let second_key = create_context_key::<u32>("second");
        let first = with_context_value(first_key.clone(), "one".to_owned(), background());
        let second = with_context_value(second_key.clone(), 2u32, ContextRef::clone(&first));
        let replaced = with_context_value(
            first_key.clone(),
            "updated".to_owned(),
            ContextRef::clone(&second),
        );

        assert!(first_key.get(BACKGROUND_CONTEXT.as_ref()).is_none());
        assert_eq!(first_key.get(first.as_ref()), Some("one".to_owned()));
        assert!(second_key.get(first.as_ref()).is_none());
        assert_eq!(first_key.get(second.as_ref()), Some("one".to_owned()));
        assert_eq!(second_key.get(second.as_ref()), Some(2));
        assert_eq!(first_key.get(replaced.as_ref()), Some("updated".to_owned()));
        assert_eq!(first_key.get(second.as_ref()), Some("one".to_owned()));
        assert_eq!(
            context_string(replaced.as_ref()),
            "[Context BACKGROUND_CONTEXT].WithValue(first).WithValue(second).WithValue(first)"
        );
    }

    #[test]
    fn anonymous_key_displays_as_anonymous() {
        let key = create_context_key_anonymous::<u8>();
        let context = with_context_value(key, 1u8, background());
        assert_eq!(
            context_string(context.as_ref()),
            "[Context BACKGROUND_CONTEXT].WithValue(anonymous)"
        );
    }

    #[test]
    fn distinct_keys_with_the_same_description_are_not_interchangeable() {
        let a = create_context_key::<String>("value");
        let b = create_context_key::<String>("value");
        let context = with_context_value(a.clone(), "a".to_owned(), background());
        assert_eq!(a.get(context.as_ref()), Some("a".to_owned()));
        assert!(b.get(context.as_ref()).is_none());
    }

    #[test]
    fn inherits_parent_cancellation_and_isolates_child_cancellation() {
        let parent_controller = AbortSignal::new();
        let parent = with_abort_signal(parent_controller.clone(), background());
        let child = with_cancel(ContextRef::clone(&parent));
        let sibling = with_cancel(ContextRef::clone(&parent));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let subscription = abort_signal_of(child.context.as_ref())
            .expect("child has a signal")
            .on_abort(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
            });

        child.cancel_with(AbortReason::value(serde_json::json!("child")));
        let child_signal = abort_signal_of(child.context.as_ref()).expect("child signal");
        assert!(child_signal.is_aborted());
        assert_eq!(
            child_signal.reason().map(|reason| reason.to_json()),
            Some(serde_json::json!("child"))
        );
        assert!(
            !abort_signal_of(sibling.context.as_ref())
                .expect("sibling signal")
                .is_aborted()
        );
        assert!(!parent_controller.is_aborted());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(subscription);

        parent_controller.abort(Some(AbortReason::value(serde_json::json!("parent"))));
        let sibling_signal = abort_signal_of(sibling.context.as_ref()).expect("sibling signal");
        assert!(sibling_signal.is_aborted());
        assert_eq!(
            sibling_signal.reason().map(|reason| reason.to_json()),
            Some(serde_json::json!("parent"))
        );
    }

    #[test]
    fn masks_caller_cancellation_for_mandatory_cleanup() {
        let controller = AbortSignal::new();
        let key = create_context_key::<String>("value");
        let context = with_context_value(
            key.clone(),
            "preserved".to_owned(),
            with_abort_signal(controller.clone(), background()),
        );
        let cleanup = without_abort_signal(ContextRef::clone(&context));

        controller.abort_without_reason();
        assert!(
            abort_signal_of(context.as_ref())
                .expect("signal")
                .is_aborted()
        );
        assert!(abort_signal_of(cleanup.as_ref()).is_none());
        assert_eq!(key.get(cleanup.as_ref()), Some("preserved".to_owned()));
    }

    /// 在一个 in-memory 上下文里起一个 span，在 span **仍开着** 的时候跑 `f`。
    ///
    /// kiana-telemetry 的行为：父 span 结算后再起的子 span 是惰性的（不记录），所以测试必须
    /// 把整个身体放进回调里（TS 测试的 `telemetry.startSpan({name:"caller"}, async (parent) =>
    /// …)` 就是这个形状）。
    async fn with_span<T, F>(telemetry: &InMemoryTelemetryContext, name: &str, f: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'static, T> + Send + 'static,
    {
        telemetry
            .start_span_typed(options(name), f)
            .await
            .expect("span")
    }

    #[tokio::test]
    async fn derives_telemetry_independently_from_values_and_cancellation() {
        let telemetry = InMemoryTelemetryContext::new();
        let recorder = telemetry.clone();
        with_span(&telemetry, "caller", move |caller_span| {
            Box::pin(async move {
                let key = create_context_key::<String>("principal");
                let controller = AbortSignal::new();
                let base = with_context_value(
                    key.clone(),
                    "owner".to_owned(),
                    with_abort_signal(controller.clone(), background()),
                );
                let context = with_telemetry(Arc::clone(&caller_span), ContextRef::clone(&base));
                let other = Arc::clone(&kiana_telemetry::noop::noop_telemetry_context());
                let replaced = with_telemetry(other, ContextRef::clone(&context));

                assert!(Arc::ptr_eq(
                    &base.telemetry(),
                    &kiana_telemetry::noop::noop_telemetry_context()
                ));
                assert!(Arc::ptr_eq(&context.telemetry(), &caller_span));
                assert_eq!(key.get(replaced.as_ref()), Some("owner".to_owned()));
                assert!(Arc::ptr_eq(
                    &abort_signal_of(replaced.as_ref()).expect("signal").inner,
                    &abort_signal_of(base.as_ref()).expect("signal").inner
                ));
                assert!(Arc::ptr_eq(
                    &without_abort_signal(ContextRef::clone(&context)).telemetry(),
                    &caller_span
                ));

                let outer_context = ContextRef::clone(&context);
                let key_outer = key.clone();
                trace_in(
                    options("outer"),
                    move |child| {
                        let key = key_outer;
                        Box::pin(async move {
                            assert_eq!(key.get(child.as_ref()), Some("owner".to_owned()));
                            assert!(Arc::ptr_eq(
                                &abort_signal_of(child.as_ref()).expect("signal").inner,
                                &abort_signal_of(outer_context.as_ref())
                                    .expect("signal")
                                    .inner
                            ));
                            child.telemetry().add_event("started", None);
                            let inner_parent = ContextRef::clone(&child);
                            let inner = trace_in(
                                options("inner"),
                                move |nested| {
                                    let link_parent = ContextRef::clone(&inner_parent);
                                    Box::pin(async move {
                                        nested.telemetry().add_link(kiana_telemetry::SpanLink {
                                            context: link_parent.telemetry().span_context_id(),
                                            attributes: None,
                                        });
                                        42
                                    })
                                },
                                ContextRef::clone(&child),
                            )
                            .await
                            .expect("inner trace");
                            assert_eq!(inner, 42);
                        })
                    },
                    ContextRef::clone(&context),
                )
                .await
                .expect("outer trace");
                assert!(Arc::ptr_eq(&context.telemetry(), &caller_span));

                let spans = recorder.get_spans();
                let caller = spans
                    .iter()
                    .find(|span| span.name == "caller")
                    .cloned()
                    .expect("caller");
                let outer_span = spans
                    .iter()
                    .find(|span| span.name == "outer")
                    .cloned()
                    .expect("outer");
                let inner_span = spans
                    .iter()
                    .find(|span| span.name == "inner")
                    .cloned()
                    .expect("inner");
                assert_eq!(outer_span.parent_id, Some(caller.id));
                assert_eq!(inner_span.parent_id, Some(outer_span.id));
                assert_eq!(inner_span.links[0].context, outer_span.context);

                controller.abort_without_reason();
                assert!(
                    abort_signal_of(context.as_ref())
                        .expect("signal")
                        .is_aborted()
                );
            })
        })
        .await;
    }

    #[tokio::test]
    async fn isolates_concurrent_trace_in_branches_and_preserves_results() {
        let telemetry = InMemoryTelemetryContext::new();
        let recorder = telemetry.clone();
        with_span(&telemetry, "root", move |span| {
            Box::pin(async move {
                let context = with_telemetry(span, background());
                let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                let gate = Arc::new(tokio::sync::Mutex::new(Some(rx)));

                let first_gate = Arc::clone(&gate);
                let first = trace_in(
                    options("first"),
                    move |child| {
                        Box::pin(async move {
                            let rx = first_gate.lock().await.take().expect("gate");
                            rx.await.ok();
                            trace_in(
                                options("first.child"),
                                |_| Box::pin(async { "first" }),
                                child,
                            )
                            .await
                            .expect("first child")
                        })
                    },
                    ContextRef::clone(&context),
                );
                let second = trace_in(
                    options("second"),
                    move |child| {
                        Box::pin(async move {
                            let result = trace_in(
                                options("second.child"),
                                |_| Box::pin(async { "second" }),
                                ContextRef::clone(&child),
                            )
                            .await
                            .expect("second child");
                            tx.send(()).ok();
                            result
                        })
                    },
                    ContextRef::clone(&context),
                );
                let (a, b) = tokio::join!(first, second);
                assert_eq!((a.unwrap(), b.unwrap()), ("first", "second"));

                let spans = recorder.get_spans();
                for name in ["first", "second"] {
                    let parent = spans
                        .iter()
                        .find(|span| span.name == name)
                        .expect("parent span");
                    let child = spans
                        .iter()
                        .find(|span| span.name == format!("{name}.child"))
                        .expect("child span");
                    assert_eq!(child.parent_id, Some(parent.id));
                }

                // 无 telemetry 的上下文也走 startSpan（没有 no-telemetry 分支）。
                let value = trace_in(
                    options("noop"),
                    |child| {
                        Box::pin(async move {
                            child.telemetry().add_link(kiana_telemetry::SpanLink {
                                context: child.telemetry().span_context_id(),
                                attributes: None,
                            });
                            42
                        })
                    },
                    background(),
                )
                .await
                .expect("noop trace");
                assert_eq!(value, 42);
            })
        })
        .await;
    }

    #[tokio::test]
    async fn trace_in_try_maps_err_to_a_throw() {
        let telemetry = InMemoryTelemetryContext::new();
        let recorder = telemetry.clone();
        with_span(&telemetry, "root", move |span| {
            Box::pin(async move {
                let context = with_telemetry(span, background());
                let failure = TelemetryError::new("boom");
                let error = trace_in_try(
                    options("throw"),
                    move |_| Box::pin(async move { Err::<(), TelemetryError>(failure.clone()) }),
                    ContextRef::clone(&context),
                )
                .await
                .unwrap_err();
                assert_eq!(error.message, "boom");
                let spans = recorder.get_spans();
                let span = spans
                    .iter()
                    .find(|span| span.name == "throw")
                    .expect("throw span");
                assert!(matches!(
                    span.status,
                    kiana_telemetry::SpanStatus::Error { .. }
                ));
            })
        })
        .await;
    }

    #[tokio::test]
    async fn stops_waiting_when_the_invocation_is_cancelled() {
        let controller = AbortSignal::new();
        let context = with_abort_signal(controller.clone(), background());
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let handle = tokio::spawn(async move { rx.await.unwrap_or_default() });
        let waiting = await_handle_with_context(handle, context.as_ref());

        #[derive(Debug)]
        struct Cancelled;
        impl std::fmt::Display for Cancelled {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("cancelled")
            }
        }
        impl std::error::Error for Cancelled {}

        controller.abort(Some(AbortReason::error(Cancelled)));
        let error = waiting.await.expect_err("cancelled");
        assert!(matches!(error, AbortError::Reason(_)));
        // 底层 promise 继续跑（任务没被取消）。
        tx.send("completed later".to_owned()).ok();
        std::thread::sleep(std::time::Duration::from_millis(10));

        assert_eq!(
            await_with_context(async { "completed" }, BACKGROUND_CONTEXT.as_ref())
                .await
                .expect("no signal"),
            "completed"
        );
    }

    #[tokio::test]
    async fn aborted_before_waiting_rejects_with_dom_error() {
        // `controller.abort()` 的缺省原因本身就是一个 `DOMException`，`abortError` 原样返回它。
        let controller = AbortSignal::new();
        controller.abort_without_reason();
        let context = with_abort_signal(controller, background());
        let error = await_with_context(async { 1 }, context.as_ref())
            .await
            .expect_err("aborted");
        assert!(error.is_abort_error());
        assert_eq!(error.name(), "AbortError");
        assert_eq!(error.message(), "This operation was aborted");
        assert_eq!(error.to_string(), "AbortError: This operation was aborted");

        // 非 `Error` 的 JS 值原因 → 回落 `new DOMException("The operation was aborted", …)`。
        let controller = AbortSignal::new();
        controller.abort(Some(AbortReason::value(serde_json::json!("plain string"))));
        let context = with_abort_signal(controller, background());
        let error = await_with_context(async { 1 }, context.as_ref())
            .await
            .expect_err("aborted");
        assert!(error.is_abort_error());
        assert_eq!(error.name(), "AbortError");
        assert_eq!(error.message(), "The operation was aborted");
        assert_eq!(error.to_string(), "AbortError: The operation was aborted");

        // `Error` 原因原样传出（`rejects.toBe(cancellation)`）。
        #[derive(Debug)]
        struct Cancelled;
        impl std::fmt::Display for Cancelled {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("cancelled")
            }
        }
        impl std::error::Error for Cancelled {}

        let controller = AbortSignal::new();
        controller.abort(Some(AbortReason::error(Cancelled)));
        let context = with_abort_signal(controller, background());
        let error = await_with_context(async { 1 }, context.as_ref())
            .await
            .expect_err("aborted");
        assert!(!error.is_abort_error());
        assert_eq!(error.name(), "Error");
        assert_eq!(error.message(), "cancelled");
    }

    #[test]
    fn abort_signal_any_first_wins() {
        let a = AbortSignal::new();
        let b = AbortSignal::new();
        let combined = AbortSignal::any(&[a.clone(), b.clone()]);
        assert!(!combined.is_aborted());
        a.abort(Some(AbortReason::value(serde_json::json!("a"))));
        assert!(combined.is_aborted());
        assert_eq!(
            combined.reason().map(|reason| reason.to_json()),
            Some(serde_json::json!("a"))
        );
        b.abort(Some(AbortReason::value(serde_json::json!("b"))));
        assert_eq!(
            combined.reason().map(|reason| reason.to_json()),
            Some(serde_json::json!("a")),
            "the first reason wins"
        );

        // 已中止的来源让组合信号立即中止。
        let already = AbortSignal::new();
        already.abort(Some(AbortReason::value(serde_json::json!("x"))));
        let combined = AbortSignal::any(&[AbortSignal::new(), already]);
        assert!(combined.is_aborted());
        assert_eq!(
            combined.reason().map(|reason| reason.to_json()),
            Some(serde_json::json!("x"))
        );
    }
}
