//! 手写 Promise 协议：JS 建 Promise、Rust 存 waiter；`settle`/reject 与未处理拒绝。
//!
//! TS 对应物：无（Node 的 V8 微任务队列 + `process.on("unhandledRejection")` 由上层
//! 打印告警）。设计文档 §2.3、§2.4、§10.5。
//!
//! JS 协议（`js/prelude_core.js`，与设计文档 §2.3 的 `globalThis.__kiana` 逐字一致）：
//!
//! ```js
//! globalThis.__kiana = { seq, waiters: Map, hostStart, promise(kind, payload), settle(id, ok, value) };
//! ```
//!
//! Rust 侧只做三件事：
//!
//! 1. `__kiana_host.hostStart(id, kind, payload)`：查 kind 注册表 → **先把 payload 转成 Rust
//!    类型**（由 handler 自己做，见 [`HostCall::payload_json`]）→ 再 `spawn_local`；
//!    未知 kind 抛 `unknown host kind: <kind>`（逐字，设计文档 §2.3）。抛错发生在
//!    `__kiana.promise` 内部但在 Promise executor 之外，所以对调用方是**同步 throw**
//!    （§2.3 的冻结协议）；`prelude_core.js` 会先删掉 waiter 再重新抛出。
//! 2. 完成队列 [`HostCompletion`]：宿主任务把结果投回主循环；主循环调用
//!    [`JsEngine::pump_completions`]（或 [`JsEngine::try_drain_completions`]），
//!    引擎执行 `__kiana.settle(id, ok, value)` 后立刻 [`super::pump_jobs_with`]（§2.3 的“完成事件
//!    在主循环里回填：settle 之后立刻泵 job”）。`HostCompletion::Invoke` 的回调抛错
//!    同样进入错误队列（[`JsErrorKind::Host`]），不会被静默吞掉。
//! 3. 未处理拒绝：`Ctx::execute_pending_job` 会**吞掉** job 抛出的错误（§2.4），所以必须
//!    安装 `set_host_promise_rejection_tracker`；[`JsEngine::take_errors`] 把它们
//!    （以及 `__kiana.recordError` 收上来的定时器异常）按 `error instanceof Error ?
//!    error.message : String(error)` 的语义物化成 [`JsError`]。
//!
//! # 数据结构与完整调用路径
//!
//! 同一个 promise 有两份状态，靠 id 关联：
//!
//! - JS 侧 `__kiana.waiters: Map<id, {resolve, reject}>` 是 settle 的唯一依据；
//! - Rust 侧 `EngineShared::pending: HashMap<u64, PendingPromise>` 只做诊断/去重
//!   （见 [`PendingPromise`]），不参与 resolve/reject 的决定。
//!
//! 一次宿主调用的路径：
//!
//! 1. JS 调 `__kiana.promise(kind, payload)`：分配 id、在 `waiters` 里登记，然后同步调用
//!    `__kiana_host.hostStart`（`js/prelude_core.js`）；
//! 2. 本文件的 `host_start` 查 `host_kinds`、登记 `pending`，把 `HostCall`（payload +
//!    带 runtime 锚的 [`HostCompletionSender`]）交给 handler；
//! 3. handler 在 `spawn_local` 之前用 `HostCall::payload_json` 解析 payload；任务结束后
//!    用 `resolve`/`reject`（流式场景用 `invoke`）把 [`HostCompletion`] 投进无界通道；
//! 4. 主循环 [`JsEngine::pump_completions`]（或 [`JsEngine::try_drain_completions`]）一次
//!    取走队列里的全部事件，交给 `apply_completions` 在**一个** `with_js` 事务里依次
//!    `__kiana.settle` / 调用回调，批次结束后统一 `pump_jobs_with` 一次。
//!
//! 主循环里的消费者是 `runner::dispatch` 的 `await_promise` 与 `loader` 的
//! `await_js_promise`（以及同一推进循环的原始值版本 `await_js_promise_value`；三者都是
//! 私有函数），它们按 `pump_jobs → try_drain_completions → 定时器` 的顺序推进。
//!
//! # 注意事项
//!
//! - 完成事件按通道 FIFO 顺序处理；同一批的 settle 全部应用后才运行 `then` 回调
//!   （定时器路径相反：每个回调一次事务，见 `super::timers`）。
//! - `EngineShared::completions` 里保存的发送端**不带** runtime 锚；带锚的副本由
//!   `JsEngine::completion_sender` / `HostCall::completion_sender` 现做现发，不存回共享状态。
//!   `HostCompletionSender` 的每个克隆都会延长 runtime 的生命周期。
//! - 引擎容器里只有 `errors` 与 `HostCompletion::Invoke` 持有 `Persistent`；宿主任务经
//!   [`HostCompletionSender::capture`] 持有的 [`HostCallback`] 自带 runtime 锚、不在引擎
//!   容器里；`pending` 里没有 JS 值。`Drop` 的清理顺序见 [`super::shutdown`]。

use std::rc::Rc;
use std::time::Instant;

use rquickjs::function::{Args, This};
use rquickjs::{Ctx, Exception, Function, IntoJs, Object, Persistent, Runtime, Value};

use super::{EngineResult, EngineShared, HostCall, JsEngine};

/// 已发起、等待宿主完成的 promise 的 Rust 侧记录。
///
/// JS 的 waiter（`resolve`/`reject`）留在 `__kiana.waiters`；这里只是诊断/去重表：
/// `host_start` 用 id 查重，应用完成事件时按 id 删除，[`JsEngine::pending_count`] 与
/// [`JsEngine::invalidate`] 的返回值读它的长度，`Drop` 时整体清空。
///
/// 注意 [`JsEngine::invalidate`] **不会** reject 或移除在飞行中的条目：它们照常按真实
/// 结果 settle（`tests/engine_shutdown.rs` 的
/// `invalidate_records_the_stale_message_and_lets_in_flight_work_finish` 锁定该行为）。
#[derive(Clone, Debug)]
pub struct PendingPromise {
    /// 发起时的 host kind。
    pub kind: String,
    /// 发起时刻（诊断用）。
    pub started: Instant,
}

/// 宿主完成事件的返回值。
///
/// 宿主任务不能持有 `Value<'js>`（会跨出 `with_js` 闭包）；这里用 Rust 拥有类型，
/// 由主循环在 `with_js` 内物化成 JS 值。
///
/// 便捷转换（`From`）覆盖 `()`、`serde_json::Value`、`String`/`&str`、`bool`、`f64` 与
/// `i32`/`u32`/`i64`/`u64`；[`HostCompletionSender::resolve`] 等接受 `impl Into<HostValue>`。
#[derive(Clone, Debug, PartialEq)]
pub enum HostValue {
    /// JS `undefined`。
    Undefined,
    /// JS `null`。
    Null,
    /// 布尔。
    Bool(bool),
    /// 数字（JS 的 Number 就是 f64；整数也走这里以避免 i32 边界）。
    ///
    /// `From<i64>` / `From<u64>` 用 `as f64` 转换：绝对值超过 2^53 的整数会丢失精度，
    /// 这与 JS Number 本身的精确范围一致。
    Number(f64),
    /// 字符串。
    String(String),
    /// JSON 值（对象按键序物化；`serde_json` 开了 `preserve_order`）。
    Json(serde_json::Value),
    /// `Error` 对象（`name` 默认 `"Error"`）。
    Error {
        /// `error.name`。
        name: String,
        /// `error.message`。
        message: String,
    },
}

impl From<()> for HostValue {
    fn from((): ()) -> Self {
        HostValue::Undefined
    }
}

impl From<serde_json::Value> for HostValue {
    fn from(value: serde_json::Value) -> Self {
        HostValue::Json(value)
    }
}

impl From<String> for HostValue {
    fn from(value: String) -> Self {
        HostValue::String(value)
    }
}

impl From<&str> for HostValue {
    fn from(value: &str) -> Self {
        HostValue::String(value.to_owned())
    }
}

impl From<bool> for HostValue {
    fn from(value: bool) -> Self {
        HostValue::Bool(value)
    }
}

impl From<f64> for HostValue {
    fn from(value: f64) -> Self {
        HostValue::Number(value)
    }
}

impl From<i32> for HostValue {
    fn from(value: i32) -> Self {
        HostValue::Number(f64::from(value))
    }
}

impl From<u32> for HostValue {
    fn from(value: u32) -> Self {
        HostValue::Number(f64::from(value))
    }
}

impl From<i64> for HostValue {
    fn from(value: i64) -> Self {
        HostValue::Number(value as f64)
    }
}

impl From<u64> for HostValue {
    fn from(value: u64) -> Self {
        HostValue::Number(value as f64)
    }
}

impl HostValue {
    /// 物化成 JS 值。只能在 `with_js` 内调用。
    ///
    /// 错误：字符串转换失败、或 `HostValue::Json` 序列化失败会返回 `Err`；后者已经
    /// 抛出一个 `failed to serialize host value: <detail>` 的 JS 异常，调用方通常
    /// `ctx.catch()` 后继续。`HostValue::Error` 优先走 prelude 的 `__kiana.makeError`
    /// （能还原 DOMException / TypeError 等 Node 错误类），prelude 尚未安装时退回普通对象。
    pub(crate) fn into_js<'js>(self, ctx: &Ctx<'js>) -> EngineResult<Value<'js>> {
        match self {
            HostValue::Undefined => Ok(Value::new_undefined(ctx.clone())),
            HostValue::Null => Ok(Value::new_null(ctx.clone())),
            HostValue::Bool(value) => Ok(Value::new_bool(ctx.clone(), value)),
            HostValue::Number(value) => Ok(Value::new_number(ctx.clone(), value)),
            HostValue::String(value) => Ok(value.into_js(ctx)?),
            HostValue::Error { name, message } => Ok(make_error(ctx, &name, &message)),
            HostValue::Json(value) => json_to_js(ctx, &value),
        }
    }
}

// 优先复用 prelude 的 `makeError`：它按错误名构造 DOMException / 标准 Error 子类，
// Node 的 abort 与网络错误依赖 `instanceof`、`error.code` 分支。回退路径有两处：
// `__kiana.makeError` 还不存在（prelude 在本文件安装之后才求值，见
// `JsEngine::install_host_primitives`），或调用本身抛错（异常被 `ctx.catch()` 吞掉）。
// 回退产物只是带 `name`/`message` 的普通对象，不是 `instanceof Error`；连对象都建不出来时
// 返回 undefined（宿主环境已不可用）。
fn make_error<'js>(ctx: &Ctx<'js>, name: &str, message: &str) -> Value<'js> {
    if let Ok(kiana) = ctx.globals().get::<_, Object>("__kiana")
        && let Ok(make) = kiana.get::<_, Function>("makeError")
    {
        match make.call::<_, Value>((name, message)) {
            Ok(value) => return value,
            Err(_) => {
                let _ = ctx.catch();
            }
        }
    }
    let error = Object::new(ctx.clone());
    match error {
        Ok(error) => {
            let _ = error.set("name", name);
            let _ = error.set("message", message);
            error.into_value()
        }
        Err(_) => {
            let _ = ctx.catch();
            Value::new_undefined(ctx.clone())
        }
    }
}

/// `HostValue::Json` → JS 值。
///
/// 走 `JSON.parse` 而不是逐键 `Object::set`：这样才能拿到与 JS 完全一致的对象语义
/// （`"__proto__"` 是自有属性而不是改原型、数字按 JSON 语法解析、键序保持插入序）。
///
/// 失败路径：`serde_json::to_string` 失败先抛出 `failed to serialize host value: ...`
/// 的 JS 异常；`ctx.json_parse` 的语法错误（正常输入不会出现，因为文本来自
/// `serde_json`）按 rquickjs 的错误传播。
fn json_to_js<'js>(ctx: &Ctx<'js>, value: &serde_json::Value) -> EngineResult<Value<'js>> {
    let text = serde_json::to_string(value).map_err(|error| {
        Exception::throw_message(ctx, &format!("failed to serialize host value: {error}"))
    })?;
    ctx.json_parse(text)
}

/// 宿主任务 → 主循环的完成事件。
///
/// 事件按投递顺序处理，同一批在**一个** `with_js` 事务里应用（见 `apply_completions`）。
/// `Settle` 应用后总会按 id 移除 `EngineShared::pending` 条目，即使 JS 侧的 waiter
/// 已经不存在（重复 settle；此时 JS 的 `settle` 返回 `false`）。`Invoke` 只用于流式
/// 回调，不涉及 promise。
pub enum HostCompletion {
    /// `__kiana.settle(id, ok, value)`。
    Settle {
        /// `__kiana.promise` 分配的 id。
        id: u64,
        /// `true` → resolve，`false` → reject。
        ok: bool,
        /// 结果值。
        value: HostValue,
    },
    /// 调用一个保存的 JS 回调（流式 chunk、`onUpdate` 等）。
    Invoke {
        /// 回调（`Persistent` 只能在同线程恢复）。
        callback: Persistent<Function<'static>>,
        /// 参数（按序物化）。
        args: Vec<HostValue>,
    },
}

impl std::fmt::Debug for HostCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostCompletion::Settle { id, ok, value } => f
                .debug_struct("Settle")
                .field("id", id)
                .field("ok", ok)
                .field("value", value)
                .finish(),
            HostCompletion::Invoke { args, .. } => f
                .debug_struct("Invoke")
                .field("args", args)
                .finish_non_exhaustive(),
        }
    }
}

/// 完成队列的发送端（可克隆；宿主任务持有一份）。
///
/// # runtime 锚（设计文档 §2.8 的硬约束）
///
/// 宿主任务经常要跨 `await` 持有 JS 回调（流式 chunk、`onUpdate`）。`Persistent`
/// 活过 `Runtime` 会触发 `JS_FreeRuntime: Assertion 'list_empty(&rt->gc_obj_list)'
/// failed` 并 abort 进程，而引擎销毁时无法知道宿主任务何时结束。所以**发给宿主任务的
/// 发送端**（[`JsEngine::completion_sender`] / [`HostCall::completion_sender`]）会克隆一份
/// `Runtime` 句柄：只要任务还持有发送端，runtime 就不会被释放，任务手里的 JS 值仍然安全；
/// 任务结束后发送端析构，runtime 才真正销毁（`JS_FreeRuntime` 会先跑一次 GC）。
///
/// 引擎内部（`EngineShared`）保存的发送端**不带** runtime 锚，否则 `Runtime` →
/// JS 宿主函数闭包 → `Rc<EngineShared>` → 发送端 → `Runtime` 会形成引用环，整代泄漏。
/// 锚句柄只存在于任务的局部变量里。需要跨 `await` 持有回调时优先用
/// [`HostCompletionSender::capture`]（[`HostCallback`] 保证先释放 JS 值、再释放 runtime）。
///
/// # 克隆语义
///
/// `Clone` 会复制一份可选的 `Runtime` 句柄（rquickjs 的 `Runtime` 是引用计数的），所以每个
/// 克隆都独立延长 runtime 的生命周期；宿主任务结束时应丢弃发送端，不要把它存进长生命周期
/// 容器。`send` 返回 `false` 表示引擎已关闭，任务可以据此提前退出。
#[derive(Clone)]
pub struct HostCompletionSender {
    tx: tokio::sync::mpsc::UnboundedSender<HostCompletion>,
    /// `Some` 表示这个发送端锚住了 runtime（只能来自引擎的公开发送端）。
    runtime: Option<Runtime>,
}

impl std::fmt::Debug for HostCompletionSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCompletionSender")
            .field("anchored", &self.runtime.is_some())
            .field("closed", &self.tx.is_closed())
            .finish()
    }
}

impl HostCompletionSender {
    /// 从 tokio 通道发送端构造（引擎内部用；不带 runtime 锚）。
    pub(crate) fn from_sender(tx: tokio::sync::mpsc::UnboundedSender<HostCompletion>) -> Self {
        Self { tx, runtime: None }
    }

    /// 克隆一份并锚住 `runtime`（引擎的公开发送端用）。
    pub(crate) fn anchored(&self, runtime: &Runtime) -> Self {
        Self {
            tx: self.tx.clone(),
            runtime: Some(runtime.clone()),
        }
    }

    /// 是否锚住了 runtime（宿主任务持有的发送端为 `true`）。
    pub fn is_anchored(&self) -> bool {
        self.runtime.is_some()
    }

    /// 引擎是否已关闭（接收端已回收）。宿主任务可用它提前结束。
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// 投递任意完成事件。返回 `false` 表示引擎已关闭（接收端已回收）。
    ///
    /// 发送失败时事件（包括其中的 `Persistent`）在返回前就被丢弃，所以调用方即使
    /// 在引擎销毁之后发送也不会让 JS 值活过 runtime。
    pub fn send(&self, completion: HostCompletion) -> bool {
        self.tx.send(completion).is_ok()
    }

    /// resolve 一个宿主 promise。
    ///
    /// 返回 `false` 只表示引擎已关闭（事件没有入队），**不**代表 JS 侧已经 settle：
    /// id 不存在（重复 settle、或 pending 已被清理）时 JS 的 `settle` 返回 `false`，
    /// 但事件本身仍然入队成功。
    pub fn resolve(&self, id: u64, value: impl Into<HostValue>) -> bool {
        self.send(HostCompletion::Settle {
            id,
            ok: true,
            value: value.into(),
        })
    }

    /// reject 一个宿主 promise（值为任意 [`HostValue`]）。
    ///
    /// 返回值语义同 [`HostCompletionSender::resolve`]：只表示事件是否入队。
    pub fn reject(&self, id: u64, value: impl Into<HostValue>) -> bool {
        self.send(HostCompletion::Settle {
            id,
            ok: false,
            value: value.into(),
        })
    }

    /// reject 一个宿主 promise，值为 `Error(message)`（`name` 默认 `"Error"`）。
    ///
    /// 返回值语义同 [`HostCompletionSender::resolve`]：只表示事件是否入队。
    pub fn reject_message(&self, id: u64, message: impl Into<String>) -> bool {
        self.reject(
            id,
            HostValue::Error {
                name: "Error".to_owned(),
                message: message.into(),
            },
        )
    }

    /// 调用一个保存的 JS 回调（流式 chunk 等）。
    ///
    /// 低层原语：`Persistent` 必须来自当前事务，且宿主任务必须保证在最后一个锚住的
    /// 发送端析构**之前**释放它。要跨 `await` 持有回调请用
    /// [`HostCompletionSender::capture`]（[`HostCallback`] 把释放顺序固定在类型里）。
    pub fn invoke(&self, callback: Persistent<Function<'static>>, args: Vec<HostValue>) -> bool {
        self.send(HostCompletion::Invoke { callback, args })
    }

    /// 捕获一个 JS 回调，返回可跨 `await` 持有的 [`HostCallback`]（流式 chunk、
    /// `onUpdate` 等宿主任务的推荐用法）。
    ///
    /// 只能捕获当前事务里的函数（`Function<'js>` 不能逃出 `with_js`）。返回的
    /// [`HostCallback`] 自己带一份 runtime 锚，所以引擎在宿主任务仍持有它时被销毁
    /// （`/reload`、会话替换）既不会 abort 进程，也不会悬空指针。
    pub fn capture(&self, function: Function<'_>) -> HostCallback {
        // 只有引擎的公开发送端带 runtime 锚；内部发送端不会被交给宿主任务。
        debug_assert!(
            self.is_anchored(),
            "HostCompletionSender::capture requires an engine-anchored sender"
        );
        let ctx = function.ctx().clone();
        let persistent = Persistent::save(&ctx, function);
        HostCallback {
            callback: Some(persistent),
            sender: self.clone(),
        }
    }
}

/// 宿主任务跨 `await` 持有的 JS 回调。
///
/// 与裸 [`Persistent`] 的区别是它同时持有一份由 [`HostCompletionSender`] 锚住的
/// `Runtime` 句柄，并且**显式保证先释放 JS 值、再释放 runtime**
/// （`Drop` 里先清空 `callback` 字段）。这样即使引擎已经被销毁：
///
/// - 继续调用 [`HostCallback::invoke`] 只会得到 `false`（通道已关闭），JS 值在
///   `invoke` 返回前就被释放；
/// - 任务结束丢弃它时，`JS_FreeValue` 使用的 `Ctx` 仍然有效，runtime 最后一次
///   `JS_FreeRuntime` 之前对象已经全部释放，不会触发 `list_empty(&rt->gc_obj_list)` 断言。
pub struct HostCallback {
    // 字段顺序即析构顺序：callback 先于 sender（另见显式 `Drop`）。
    callback: Option<Persistent<Function<'static>>>,
    sender: HostCompletionSender,
}

impl HostCallback {
    /// 调用回调；返回 `false` 表示引擎已关闭（回调已在返回前释放）。
    ///
    /// 这是**入队**而不是同步调用：真正的 JS 调用发生在主循环应用完成事件时；每次
    /// `invoke` 都把 `Persistent` 的克隆放进事件里。引擎已关闭时 `send` 失败，事件与
    /// 克隆的 JS 值在返回前一起释放（`Persistent` 仍由本对象持有，可再次 `invoke`）。
    pub fn invoke(&self, args: Vec<HostValue>) -> bool {
        match &self.callback {
            Some(callback) => self.sender.invoke(callback.clone(), args),
            None => false,
        }
    }

    /// 立即释放 JS 值（之后 [`HostCallback::invoke`] 返回 `false`）。
    ///
    /// 只释放本对象持有的那一份；已经入队的 `Invoke` 事件各自持有克隆，仍会在主循环里执行。
    pub fn release(&mut self) {
        self.callback = None;
    }

    /// JS 值是否已释放。
    pub fn is_released(&self) -> bool {
        self.callback.is_none()
    }

    /// 捕获时使用的发送端。
    pub fn sender(&self) -> &HostCompletionSender {
        &self.sender
    }
}

impl std::fmt::Debug for HostCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCallback")
            .field("released", &self.callback.is_none())
            .field("closed", &self.sender.is_closed())
            .finish()
    }
}

impl Drop for HostCallback {
    fn drop(&mut self) {
        // 先断开 JS 值的引用链，sender 里的 runtime 锚在字段析构时才释放。
        self.callback = None;
    }
}

/// 引擎错误事件的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsErrorKind {
    /// 未处理的 promise 拒绝（rejection tracker）。
    UnhandledRejection,
    /// 定时器回调抛出的异常。
    Timer,
    /// 宿主原语通过 `__kiana.recordError` 上报的异常。
    Host,
}

impl JsErrorKind {
    /// 协议里的字符串名（`__kiana.recordError(kind, ...)`）。
    pub fn as_str(self) -> &'static str {
        match self {
            JsErrorKind::UnhandledRejection => "unhandledRejection",
            JsErrorKind::Timer => "timer",
            JsErrorKind::Host => "host",
        }
    }

    /// 解析协议字符串；未知值归入 [`JsErrorKind::Host`]。
    ///
    /// 协议里只有 `"unhandledRejection"` / `"timer"` / `"host"` 三个名字；本函数只显式
    /// 匹配前两个，`"host"` 与任何未知字符串都落到 [`JsErrorKind::Host`]。
    /// `"unhandledRejection"` 实际由 Rust 的 rejection tracker 直接构造，不经过这里；
    /// JS 侧（`js/prelude_core.js`、`js/context.js`）上报的是 `"timer"` 与 `"host"`。
    pub fn parse(kind: &str) -> Self {
        match kind {
            "unhandledRejection" => JsErrorKind::UnhandledRejection,
            "timer" => JsErrorKind::Timer,
            _ => JsErrorKind::Host,
        }
    }
}

/// 从 JS 侧收上来的错误（已按 TS 的 `error instanceof Error ? error.message : String(error)` 物化）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsError {
    /// 来源。
    pub kind: JsErrorKind,
    /// `error.name`（非 Error 值固定 `"Error"`）。
    pub name: String,
    /// 错误文本；错误落点（`ca-extensions.md` §2.8.4 / §3.8）里的 `<error>` 就是它。
    pub message: String,
    /// `error.stack`（若有）。
    pub stack: Option<String>,
}

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// 尚未物化的错误：保留原始 JS 值（`Persistent`），等主循环取走时再格式化。
///
/// 四个产生点（rejection tracker、`__kiana.recordError` 宿主函数、`Invoke` 回调失败、
/// 定时器看门狗中断——后者由 `engine/timers.rs` 的 `fire_due_timers` 构造，kind 为
/// [`JsErrorKind::Timer`]）
/// 都只做 `Persistent::save`：rejection tracker 的回调里不能运行 JS（见
/// `install_rejection_tracker`），统一延迟到 [`JsEngine::take_errors`] 在 `with_js` 里
/// 调用 [`PendingJsError::materialize`]。
pub(crate) struct PendingJsError {
    pub(crate) kind: JsErrorKind,
    /// promise 身份（仅未处理拒绝有；用于 `is_handled=true` 时移除）。
    ///
    /// 比较的是 `Persistent<Value>` 的 `PartialEq`（runtime 指针 + JSValue 位模式），
    /// 对对象即“同一个 JS 对象”。
    pub(crate) promise: Option<Persistent<Value<'static>>>,
    pub(crate) reason: Persistent<Value<'static>>,
}

impl PendingJsError {
    /// 在 `with_js` 的事务里恢复并格式化；`restore` 失败（runtime 已不是同一个）时
    /// 返回保留 `kind` 的兜底错误，message 固定为 `"Failed to format error"`。
    pub(crate) fn materialize(self, ctx: &Ctx<'_>) -> JsError {
        let Ok(value) = self.reason.restore(ctx) else {
            return JsError {
                kind: self.kind,
                name: "Error".to_owned(),
                message: "Failed to format error".to_owned(),
                stack: None,
            };
        };
        js_error_from_value(ctx, self.kind, value)
    }
}

/// 取走当前挂起的 JS 异常并物化成 [`JsError`]（会清除挂起状态）。
///
/// 供宿主函数/求值失败路径使用；必须在已经进入 JS 的 `Ctx` 上调用。
pub fn take_exception(ctx: &Ctx<'_>, kind: JsErrorKind) -> JsError {
    let value = ctx.catch();
    js_error_from_value(ctx, kind, value)
}

/// 把任意 JS 值按 TS 的规则物化成 [`JsError`]。
///
/// 优先调用 prelude 的 `__kiana.errorText`（在 JS 里做 `instanceof Error` / `String(value)`，
/// 保真 JS 的数字/对象字符串化）；prelude 尚未安装时退回 Rust 侧的属性读取。
pub(crate) fn js_error_from_value<'js>(
    ctx: &Ctx<'js>,
    kind: JsErrorKind,
    value: Value<'js>,
) -> JsError {
    if let Some((name, message, stack)) = error_text_via_prelude(ctx, &value) {
        return JsError {
            kind,
            stack: stack_with_header(&name, &message, stack),
            name,
            message,
        };
    }
    fallback_error_from_value(ctx, kind, &value)
}

/// 归一化 `error.stack` 的首行：V8 的 `error.stack` 以 `Name: message` 开头
/// （`message` 为空时只有 `Name`），QuickJS 的原始 stack 从第一帧开始。
///
/// `ca-extensions.md` §4.9 / `interactive-mode.ts` 的 `showExtensionError` 会
/// `stack.split("\n").slice(1)`（注释：跳过与 message 重复的首行），所以这里必须补齐，
/// 否则每个扩展错误都会少掉第一个真实栈帧。幂等：首行已经是 header 时原样返回。
fn stack_with_header(name: &str, message: &str, stack: Option<String>) -> Option<String> {
    let stack = stack.filter(|stack| !stack.is_empty())?;
    let header = if message.is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {message}")
    };
    let first_line = stack.lines().next().unwrap_or_default();
    if first_line == header {
        Some(stack)
    } else {
        Some(format!("{header}\n{stack}"))
    }
}

/// 通过 prelude 的 `__kiana.errorText` 取 `(name, message, stack)`。
///
/// 返回 `None` 的三种情况：prelude 未安装、`errorText` 调用抛错（异常被
/// `ctx.catch()` 清掉）、返回对象缺少 `name`/`message`。调用方随后走
/// `fallback_error_from_value`。
fn error_text_via_prelude<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> Option<(String, String, Option<String>)> {
    let kiana = ctx.globals().get::<_, Object>("__kiana").ok()?;
    let error_text = kiana.get::<_, Function>("errorText").ok()?;
    let result: Object = match error_text.call((value.clone(),)) {
        Ok(result) => result,
        Err(_) => {
            let _ = ctx.catch();
            return None;
        }
    };
    let name: String = result.get("name").ok()?;
    let message: String = result.get("message").ok()?;
    let stack: Option<String> = result
        .get::<_, Option<String>>("stack")
        .ok()
        .flatten()
        .filter(|stack| !stack.is_empty());
    Some((name, message, stack))
}

/// prelude 不可用时的近似实现：字符串直接作为 message；对象读 `name`/`message`/`stack`
/// （`name`/`message` 读取抛错时用默认值并清掉挂起异常，`stack` 读取失败按没有 stack 处理）；
/// 其它值用 `JSON.stringify`，`undefined` / `null` 特判。对象分支同样经过
/// `stack_with_header`，所以 stack 首行格式与 prelude 路径一致。
fn fallback_error_from_value<'js>(
    ctx: &Ctx<'js>,
    kind: JsErrorKind,
    value: &Value<'js>,
) -> JsError {
    if let Some(text) = value.as_string() {
        let message = text.to_string().unwrap_or_default();
        return JsError {
            kind,
            name: "Error".to_owned(),
            message,
            stack: None,
        };
    }
    if let Some(object) = value.as_object() {
        let name = object.get::<_, String>("name").unwrap_or_else(|_| {
            let _ = ctx.catch();
            "Error".to_owned()
        });
        let message = object.get::<_, String>("message").unwrap_or_else(|_| {
            let _ = ctx.catch();
            String::new()
        });
        let stack = object
            .get::<_, Option<String>>("stack")
            .ok()
            .flatten()
            .filter(|stack| !stack.is_empty());
        return JsError {
            kind,
            name: name.clone(),
            message: message.clone(),
            stack: stack_with_header(&name, &message, stack),
        };
    }
    let message = if value.is_undefined() {
        "undefined".to_owned()
    } else if value.is_null() {
        "null".to_owned()
    } else {
        ctx.json_stringify(value.clone())
            .ok()
            .flatten()
            .and_then(|text| text.to_string().ok())
            .unwrap_or_else(|| "Failed to format error".to_owned())
    };
    JsError {
        kind,
        name: "Error".to_owned(),
        message,
        stack: None,
    }
}

/// 安装 `Runtime::set_host_promise_rejection_tracker`。
///
/// 回调**不能** panic 到 C 侧：rquickjs 的 wrapper 用 `catch_unwind` 兜底，但 panic 会
/// 被记到 runtime 的 opaque 上并在下一次进入 JS 时重新抛出；这里只做 `Persistent::save`
/// 与 `RefCell` 操作，不运行 JS。
///
/// 两条分支：
///
/// - `is_handled == false`：登记一条 `UnhandledRejection`，promise 与 reason 各存一份
///   `Persistent`；
/// - `is_handled == true`（这个 promise 后来装上了 handler）：按 promise 身份从 `errors`
///   里移除之前登记的条目，避免“已处理”的拒绝仍被 [`JsEngine::take_errors`] 报出来；
/// - 引擎 `Drop` 期间（`shutting_down`）两条都不做，防止新的 `Persistent` 活过 runtime。
pub(crate) fn install_rejection_tracker(rt: &Runtime, shared: Rc<EngineShared>) {
    rt.set_host_promise_rejection_tracker(Some(Box::new(
        move |ctx, promise, reason, is_handled| {
            // 引擎正在 `Drop`：任何新 `Persistent` 都会活过 runtime 并 abort 进程
            // （设计文档 §2.8）。关闭期间产生的拒绝只用于断开引用链，不再登记。
            if shared.shutting_down.get() {
                return;
            }
            if is_handled {
                let promise = Persistent::save(&ctx, promise);
                shared
                    .errors
                    .borrow_mut()
                    .retain(|entry| entry.promise.as_ref() != Some(&promise));
                return;
            }
            shared.errors.borrow_mut().push(PendingJsError {
                kind: JsErrorKind::UnhandledRejection,
                promise: Some(Persistent::save(&ctx, promise)),
                reason: Persistent::save(&ctx, reason),
            });
        },
    )));
}

/// 安装 `__kiana_host.hostStart` 与 `__kiana_host.recordError`。
///
/// 前置条件：`__kiana_host` 对象已由 `JsEngine::install_host_primitives` 创建。
///
/// 时序约束：必须在 `js/prelude_core.js` 求值**之前**调用。prelude 在顶层执行
/// `hostStart: host.hostStart`，把当前的函数对象快照进 `__kiana.hostStart`；之后再替换
/// `__kiana_host.hostStart`，已安装的 `__kiana` 看不到。`recordError` 则是每次调用时
/// 现查 `host.recordError`（`js/prelude_core.js` 的 `kiana.recordError`），不受该快照约束。
pub(crate) fn install_host_promise(engine: &JsEngine) -> EngineResult<()> {
    let shared = Rc::clone(&engine.shared);
    engine.with_js(move |ctx| -> EngineResult<()> {
        let host: Object<'_> = ctx.globals().get("__kiana_host")?;
        let start = make_host_start(ctx.clone(), Rc::clone(&shared))?;
        host.set("hostStart", start)?;
        let record = make_record_error(ctx.clone(), shared)?;
        host.set("recordError", record)?;
        Ok(())
    })
}

/// 构造 `hostStart`。`Ctx<'js>` 与 `Value<'js>` 必须共享同一个 `'js`，所以宿主函数
/// 在泛型函数里创建（闭包本身不是生命周期泛型的）。
fn make_host_start<'js>(ctx: Ctx<'js>, shared: Rc<EngineShared>) -> EngineResult<Function<'js>> {
    Function::new(
        ctx,
        move |ctx: Ctx<'js>, id: u64, kind: String, payload: Value<'js>| -> EngineResult<()> {
            host_start(&shared, &ctx, id, kind, payload)
        },
    )
}

/// 构造 `recordError`（只保存原始值，不运行 JS）。
fn make_record_error<'js>(ctx: Ctx<'js>, shared: Rc<EngineShared>) -> EngineResult<Function<'js>> {
    Function::new(
        ctx,
        move |ctx: Ctx<'js>, kind: String, value: Value<'js>| -> EngineResult<()> {
            shared.errors.borrow_mut().push(PendingJsError {
                kind: JsErrorKind::parse(&kind),
                promise: None,
                reason: Persistent::save(&ctx, value),
            });
            Ok(())
        },
    )
}

/// `hostStart` 的实现：查 kind → 登记 `pending` → 把 [`HostCall`] 交给 handler。
///
/// 所有失败都以同步 JS 异常的形式返回（未知 kind、重复 id、handler 自己返回的 `Err`）；
/// `prelude_core.js` 的 `__kiana.promise` 会捕获它、删除 waiter 后重新抛给调用方，所以
/// 调用方看到的是 `__kiana.promise(...)` 的同步 throw。
fn host_start<'js>(
    shared: &Rc<EngineShared>,
    ctx: &Ctx<'js>,
    id: u64,
    kind: String,
    payload: Value<'js>,
) -> EngineResult<()> {
    // 借用只在这个块里：`handler.start` 是任意宿主代码，调用期间不持有 `host_kinds`
    // 的 `RefCell` 借用。
    let handler = {
        let kinds = shared.host_kinds.borrow();
        kinds.get(&kind).cloned()
    };
    let Some(handler) = handler else {
        return Err(Exception::throw_message(
            ctx,
            &format!("unknown host kind: {kind}"),
        ));
    };
    {
        let mut pending = shared.pending.borrow_mut();
        // 防御性查重：id 由 JS 的 `++this.seq` 单调分配，正常不会重复；重复时同步抛错，
        // 不覆盖已有条目。
        if pending.contains_key(&id) {
            return Err(Exception::throw_message(
                ctx,
                &format!("duplicate host call id: {id}"),
            ));
        }
        // 在启动 handler 之前登记：条目一直留到完成事件被应用（`apply_completions` 按 id
        // 删除），`pending_count` 因此在 handler 同步返回后立即反映这次调用。
        pending.insert(
            id,
            PendingPromise {
                kind: kind.clone(),
                started: Instant::now(),
            },
        );
    }
    let call = HostCall {
        ctx: ctx.clone(),
        id,
        kind,
        payload,
        // 宿主任务会克隆这个发送端并跨 `await` 持有 JS 回调，所以带 runtime 锚。
        completions: shared.anchored_sender(),
    };
    match handler.start(call) {
        Ok(()) => Ok(()),
        Err(error) => {
            // handler 失败：撤销登记再向上抛；JS 侧 prelude 也会删掉 waiter，两侧都不留孤儿。
            shared.pending.borrow_mut().remove(&id);
            Err(error)
        }
    }
}

/// 调用 `__kiana.settle`。`settle` 是 `__kiana` 的方法（内部用 `this.waiters`），
/// 所以必须用 [`This`] 绑定接收者，不能把函数取出来后裸调。
///
/// 返回 JS 的布尔结果：`false` 表示 waiter 不存在（已被 settle 或从未注册）。JS 侧抛出的
/// 异常以 `Err` 返回，调用方负责 `ctx.catch()` 清掉挂起状态。
pub(crate) fn call_settle<'js>(
    kiana: &Object<'js>,
    id: u64,
    ok: bool,
    value: Value<'js>,
) -> EngineResult<bool> {
    let settle: Function<'js> = kiana.get("settle")?;
    settle.call((This(kiana.clone()), id, ok, value))
}

/// 把一批完成事件应用到 JS：`settle` / 回调调用，然后泵 job。
///
/// 必须在 `with_js` 之外调用（本函数内部进入一次 `with_js`）。
///
/// # 执行顺序
///
/// 1. `__kiana` 全局只查一次；缺失（prelude 未安装/安装失败）时 `Settle` 仍会删除
///    `pending` 条目，`Invoke` 仍会执行。
/// 2. 按队列顺序逐个处理：`Settle` 物化值后调用 `__kiana.settle(id, ok, value)`；
///    `Invoke` 恢复回调、物化参数并 `call_arg`。
/// 3. 整批处理完后只调用一次 `pump_jobs_with`：本批所有 `then`/`catch` 回调在这里才
///    运行（顺序与 promise 被 settle 的顺序一致）。
///
/// # 失败模式
///
/// - `Settle` 的值物化失败时用 `undefined` 继续 settle，不会让 promise 永远挂起；
/// - `settle` 本身抛错只清掉挂起异常，不记入错误队列；
/// - `Invoke` 的回调 `restore` 失败（runtime 不匹配）时静默跳过该事件；
/// - 参数物化失败时该参数被跳过（回调收到的实参可能少于 `args.len()`）；
/// - `Invoke` 的回调抛错会记成 [`JsErrorKind::Host`]，不静默丢弃。
///
/// 返回值是输入 `batch` 的长度，包含被跳过/失败的事件。
pub(crate) fn apply_completions(engine: &JsEngine, batch: Vec<HostCompletion>) -> usize {
    if batch.is_empty() {
        return 0;
    }
    let count = batch.len();
    engine.with_js(|ctx| {
        let kiana = match ctx.globals().get::<_, Object>("__kiana") {
            Ok(kiana) => Some(kiana),
            Err(_) => {
                let _ = ctx.catch();
                None
            }
        };
        // 整批共享一个事务：每次 settle 只是把 then/catch 的 job 放进队列，最后统一泵一次。
        for completion in batch {
            match completion {
                HostCompletion::Settle { id, ok, value } => {
                    if let Some(kiana) = &kiana {
                        let value = match value.into_js(&ctx) {
                            Ok(value) => value,
                            Err(_) => {
                                let _ = ctx.catch();
                                Value::new_undefined(ctx.clone())
                            }
                        };
                        if call_settle(kiana, id, ok, value).is_err() {
                            let _ = ctx.catch();
                        }
                    }
                    // 无论 JS waiter 是否存在、settle 是否抛错，Rust 侧条目都要移除：
                    // 它只跟踪“宿主任务尚未交付结果”。
                    engine.shared.pending.borrow_mut().remove(&id);
                }
                HostCompletion::Invoke { callback, args } => {
                    let Ok(callback) = callback.restore(&ctx) else {
                        continue;
                    };
                    let mut js_args = Args::new(ctx.clone(), args.len());
                    for arg in args {
                        match arg.into_js(&ctx) {
                            Ok(value) => {
                                if js_args.push_arg(value).is_err() {
                                    let _ = ctx.catch();
                                }
                            }
                            Err(_) => {
                                let _ = ctx.catch();
                            }
                        }
                    }
                    if callback.call_arg::<Value>(js_args).is_err() {
                        // 回调抛出的异常不能静默丢弃：TS 里它会让被 await 的
                        // tool/stream 路径 reject，最终成为扩展错误（设计文档 §2.3/§2.4）。
                        let value = ctx.catch();
                        engine.shared.errors.borrow_mut().push(PendingJsError {
                            kind: JsErrorKind::Host,
                            promise: None,
                            reason: Persistent::save(&ctx, value),
                        });
                    }
                }
            }
        }
        super::pump_jobs_with(&ctx);
    });
    count
}
