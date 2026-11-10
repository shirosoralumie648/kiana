//! `JsEngine`：rquickjs 运行时、单线程 `with_js` 入口、Promise 协议、限制与关闭顺序。
//!
//! 设计文档 §2.1–§2.8。拆分：
//!
//! - [`limits`]：内存/栈/中断预算、100 ms 看门狗、30 s/10 s deadline；
//! - [`promise`]：`__kiana` Promise 协议、宿主 kind 注册表、完成队列、未处理拒绝；
//! - [`timers`]：定时器堆（`setTimeout`/`setInterval` 的 Rust 侧）；
//! - [`shutdown`]：`Persistent` 清理顺序、代际重载、`invalidate`。
//!
//! # 单线程所有权（§2.0、§2.2）
//!
//! `JsEngine` 不是 `Send`/`Sync`（`rquickjs::Persistent` 是 `!Send`），必须与 TUI、tokio
//! `current_thread` 一起跑在同一个 `LocalSet` 上。宿主异步只能 `spawn_local` 后把结果投回
//! [`HostCompletionSender`]，绝不阻塞 [`JsEngine::with_js`]。
//!
//! [`JsEngine::with_js`] 是**唯一**允许进入 JS 的入口：
//!
//! - 不得嵌套（`Context::with` 内部是 `RefCell`，嵌套会 `panicked at ... RefCell already
//!   borrowed`）；宿主函数一律接收 `Ctx` 参数，用 `Ctx` 继续 `eval`/`execute_pending_job`；
//! - `Value<'js>` 不得逃出闭包；出闭包前转成 Rust 拥有类型或 `Persistent`；
//! - `rt.run_gc()` 只能在 `ctx.with` 之外（[`JsEngine::run_gc`]）。
//!
//! # 安装（设计文档 §9.2）
//!
//! [`JsEngine::new`] 只建 runtime/context、设限制、注入 `__kiana_host` 原语；
//! [`install`] 声明并求值 `js/prelude_core.js`（`__kiana` 协议、定时器、AbortController/Signal、
//! `setImmediate`/`nextTick` → `queueMicrotask`）。其余单元各自暴露
//! `pub fn install(engine: &JsEngine) -> EngineResult<()>`，由 crate-facade 按 §9.2 的顺序调用
//! （intl 最先，其次本模块的 [`install`]，然后 node globals / 虚拟模块 / api·context·dispatch / ui）。
//!
//! 本模块**不** `include_str!` 其它单元的 JS；[`JsEngine::register_module_source`] 只保存
//! 模块源（`name -> &'static str`），供 loader 在 `Module::declare` 时取用。
//!
//! # 执行流程（构造与一次事务）
//!
//! 构造（[`JsEngine::with_config`]）按固定顺序完成：
//!
//! 1. `Runtime::new` 建 QuickJS runtime；
//! 2. 装内存/栈上限与中断处理器（`limits::install_runtime_limits`），必须在任何 JS
//!    执行之前（两者都是 runtime 级状态）；
//! 3. 取 `WeakRuntime` 并建 `EngineShared`；
//! 4. 在 runtime 上登记未处理拒绝跟踪器（`engine/promise.rs`）；
//! 5. 建 `Context::full`，随后注入 `__kiana_host` 的 Promise/定时器原语。
//!
//! [`JsEngine::new`] 只做 1–5，不装 prelude；`install` 与其它单元的 `install` 由
//! crate-facade 按 §9.2 顺序调用。
//!
//! 一次事务（[`JsEngine::with_js`]）：`limits::enter_watchdog` 快照并按需改写 deadline →
//! `Context::with` 执行闭包 → 守卫析构恢复 deadline。`with_js` 本身不泵微任务；调用方
//! 显式用 `pump_jobs` / `pump_jobs_with`，而完成事件由 `pump_completions` 批量应用后
//! 顺带泵一次 job（`engine/promise.rs` 的 `apply_completions`）。
//!
//! # 共享状态与所有权
//!
//! `EngineShared` 由 `Rc` 持有，引擎与注册进 JS 的宿主函数闭包共享同一份。可变字段全是
//! `Cell`/`RefCell`（单线程模型），借用不得跨越一次 `with_js`：JS 执行期间宿主函数会
//! 再次借用同一容器（错误队列、句柄表等），在事务外长期持有 `Ref`/`RefMut` 会让那次
//! 借用 panic（`RefCell already borrowed`）。需要跨事务保留的 JS 值一律转成
//! `Persistent` 并寄存（`put_persistent` / `store_handle`），由 `Drop` 统一清理。
//!
//! 模块源注册表只保存 `&'static str`（[`JsEngine::register_module_source`]），不持有 JS
//! 对象；`loader::install` 在装 loader 时把整表快照进自己的源表
//! （`loader/mod.rs` 的 `register_source`），`Module::declare` 从那里取源。
//!
//! # 失败模式（本模块可见的部分）
//!
//! - 构造或注入失败返回 [`EngineResult`]；未返回的局部值按 Rust 规则清理（已建好的
//!   `JsEngine` 按字段声明顺序析构）。
//! - 求值失败时 `Ctx` 上可能残留 pending exception，后续模块加载会误读；`eval_script`
//!   与 `install` 在返回 `Err` 前主动 `ctx.catch()` 清空，`eval_script_error` 则把异常
//!   物化成 [`JsError`]（需要逐字错误文本）。
//! - 未知 host kind 是同步 throw，文案 `unknown host kind: <kind>`；同一 promise id 重复
//!   发起是 `duplicate host call id: <id>`（`engine/promise.rs` 的 `host_start`）。
//! - 宿主任务跨 `await` 持有裸 `Persistent` 会让引擎销毁时的 `JS_FreeRuntime` 断言失败
//!   （进程 abort），见 [`HostCall`] 与 `engine/shutdown.rs`。

pub mod limits;
pub mod promise;
pub mod shutdown;
pub mod timers;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use rquickjs::context::EvalOptions;
use rquickjs::{Context, Ctx, Persistent, Runtime, Value};
use tokio::sync::mpsc;

use limits::InterruptState;
pub use limits::{
    DEFAULT_MAX_STACK_SIZE, DEFAULT_MEMORY_LIMIT, DEFAULT_WATCHDOG, DeadlineGuard, EngineConfig,
    FACTORY_DEADLINE, HANDLER_DEADLINE,
};
use promise::PendingJsError;
pub use promise::{
    HostCallback, HostCompletion, HostCompletionSender, HostValue, JsError, JsErrorKind,
    PendingPromise, take_exception,
};

/// 引擎级结果类型。
///
/// 引擎只做 rquickjs 宿主层，错误就是 `rquickjs::Error`（`Exception` 表示 JS 侧异常，
/// 用 `ctx.catch()` 取原始值）；上层把 [`JsError`] 映射成 `ca-extensions.md` §3.8 的三种落点。
pub type EngineResult<T> = rquickjs::Result<T>;

/// 引擎持有的 JS 对象句柄 id（[`JsEngine::store_handle`]）。
pub type JsHandleId = u64;

/// prelude 核心源（`crates/kiana-extensions/js/prelude_core.js`）。
const PRELUDE_CORE: &str = include_str!("../../js/prelude_core.js");

/// 引擎内部共享状态（宿主函数闭包与引擎本体共同持有；同线程 `Rc`）。
///
/// 这里的所有容器都随引擎 `Drop` 统一清空（见 `engine/shutdown.rs`），所以把
/// `Persistent` 放进这些槽位就等于把它的生命周期绑到这一代引擎上。
///
/// 借用约定：任何 `borrow`/`borrow_mut` 都必须在进入 JS 之前结束。JS 执行期间宿主函数
/// 会再次借用同一容器（例如未处理拒绝 tracker 向 `errors` push），在 `with_js` 外持着
/// `RefMut` 会让那次借用 panic。
pub(crate) struct EngineShared {
    /// 建引擎时的配置快照；`watchdog` 在每次 `with_js` 时从这里读取。
    pub(crate) config: EngineConfig,
    /// 中断状态（看门狗/外部中断）。
    pub(crate) interrupt: std::sync::Arc<InterruptState>,
    /// 定时器堆。
    pub(crate) timers: RefCell<timers::TimerQueue>,
    /// 已发起、未完成的宿主 promise。
    ///
    /// 键是 JS 侧传入的 promise id；重复 id 会被 `host_start` 以
    /// `duplicate host call id: <id>` 拒绝（同步 throw），因此这里不会覆盖在途记录。
    pub(crate) pending: RefCell<HashMap<u64, PendingPromise>>,
    /// `Persistent<Object>` 句柄槽。
    ///
    /// 由 [`JsEngine::store_handle`] 写入；本仓库里调用它的是测试（`tests/engine.rs`、
    /// `tests/engine_shutdown.rs`）。UI 的 `JsComponent` 用的是 JS 侧 `__kiana_refs`
    /// 的引用 id（见 `ui/adapter.rs`），不走这张表。
    pub(crate) handles: RefCell<HashMap<JsHandleId, Persistent<rquickjs::Object<'static>>>>,
    /// 句柄 id 序号；新 id 从 1 开始，0 不会被分配。
    pub(crate) handle_seq: Cell<JsHandleId>,
    /// 通用持久槽：任何单元都可以把 `Persistent` 寄存在这里，`Drop` 时统一清理。
    pub(crate) persistents: RefCell<HashMap<String, Box<dyn Any>>>,
    /// host kind → handler（`__kiana.promise(kind, payload)` 的分发表）。
    pub(crate) host_kinds: RefCell<HashMap<String, Rc<dyn HostKindHandler>>>,
    /// 宿主完成队列（单线程；接收端在 `JsEngine` 里）。
    ///
    /// 这个发送端**不带** runtime 锚：它被宿主函数闭包（JS 对象）间接持有，
    /// 持有 `Runtime` 会形成“Runtime → JS 闭包 → EngineShared → Runtime”的引用环。
    /// 发给宿主任务的副本由 [`EngineShared::anchored_sender`] 现做现用。
    pub(crate) completions: HostCompletionSender,
    /// runtime 的弱引用：给宿主任务现做带锚的 [`HostCompletionSender`] 用。
    ///
    /// 必须是弱引用（设计文档 §2.8）：`EngineShared` 被 JS 宿主函数闭包持有，
    /// 强引用会让整代 runtime 无法销毁。
    pub(crate) weak_runtime: rquickjs::runtime::WeakRuntime,
    /// 未物化的错误（未处理拒绝 + `__kiana.recordError`）。
    pub(crate) errors: RefCell<Vec<PendingJsError>>,
    /// 模块源注册表（`name -> &'static str`；虚拟模块/预置模块用）。
    pub(crate) module_sources: RefCell<HashMap<String, &'static str>>,
    /// `scoped_module_name` 的序号。
    pub(crate) module_seq: Cell<u64>,
    /// GC 节流计数。
    pub(crate) gc_tick: Cell<u32>,
    /// prelude 是否已安装。
    pub(crate) installed: Cell<bool>,
    /// `invalidate()` 的 stale 文案。
    pub(crate) stale: RefCell<Option<String>>,
    /// `Drop` 已开始：rejection tracker 不再登记新的 `Persistent`（§2.8）。
    pub(crate) shutting_down: std::cell::Cell<bool>,
}

impl EngineShared {
    /// 给宿主任务用的完成队列发送端：克隆一份并锚住 runtime（§2.8 的关闭约束）。
    ///
    /// `WeakRuntime` 升级失败只可能发生在引擎销毁之后（此时宿主函数不可能再被调用），
    /// 退回不带锚的发送端即可。
    pub(crate) fn anchored_sender(&self) -> HostCompletionSender {
        match self.weak_runtime.try_ref() {
            Some(runtime) => self.completions.anchored(&runtime),
            None => self.completions.clone(),
        }
    }
}

/// `__kiana.promise(kind, payload)` 的 Rust 侧处理器。
///
/// 实现者**必须**在 `spawn_local` 之前把 payload 解析成 Rust 类型（设计文档 §2.3）；
/// 处理器在 JS 调用栈里运行，不得再进 [`JsEngine::with_js`]（可以用 [`HostCall::ctx`]
/// 在同一个事务里继续读 JS）。完成时用 [`HostCall::completion_sender`] 投递结果。
///
/// 返回 `Err` 时引擎会撤销刚建立的 pending 记录，并把错误作为同步 throw 传回 JS
/// （`engine/promise.rs` 的 `host_start`）。
pub trait HostKindHandler: 'static {
    /// 处理一次宿主调用。
    fn start<'js>(&self, call: HostCall<'js>) -> EngineResult<()>;
}

impl<F> HostKindHandler for F
where
    F: for<'js> Fn(HostCall<'js>) -> EngineResult<()> + 'static,
{
    fn start<'js>(&self, call: HostCall<'js>) -> EngineResult<()> {
        self(call)
    }
}

/// 一次 `__kiana.promise(kind, payload)` 的宿主侧视图。
///
/// # 跨 `await` 持有 JS 值的规则（设计文档 §2.8 的硬约束）
///
/// 宿主任务里任何跨 `await` 的 [`rquickjs::Persistent`] 都必须让任务同时持有
/// [`HostCall::completion_sender`]（它锚住 runtime，引擎销毁时不会 abort 进程），并且
/// **先释放 JS 值、再释放发送端**。推荐直接用 `completion_sender().capture(...)`
/// （[`HostCompletionSender::capture`] 返回的 [`HostCallback`] 把顺序固定在类型里）。
/// 只带裸 `Persistent` 的任务在引擎 `/reload` 时会让 `JS_FreeRuntime` 断言失败。
pub struct HostCall<'js> {
    ctx: Ctx<'js>,
    id: u64,
    kind: String,
    payload: Value<'js>,
    completions: HostCompletionSender,
}

impl<'js> HostCall<'js> {
    /// promise id（`__kiana.settle(id, ...)` 用）。
    pub fn id(&self) -> u64 {
        self.id
    }

    /// host kind。
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// 当前事务的 `Ctx`（可以继续读 JS；不要再 `with_js`）。
    pub fn ctx(&self) -> &Ctx<'js> {
        &self.ctx
    }

    /// 原始 payload。
    pub fn payload(&self) -> &Value<'js> {
        &self.payload
    }

    /// 取出 payload（`'js` 生命期与 `Ctx` 一致）。
    pub fn into_payload(self) -> Value<'js> {
        self.payload
    }

    /// 完成队列发送端（克隆进 `spawn_local` 的任务里）。
    pub fn completion_sender(&self) -> HostCompletionSender {
        self.completions.clone()
    }

    /// 把 payload 按 JSON 语义解析成 Rust 类型。
    ///
    /// 在 `spawn_local` **之前**调用（payload 里的 `Value` 不能跨出 `with_js`）。
    /// `undefined` 视为 `null`；`JSON.stringify` 不产生文本的值（函数、Symbol 等）走同一
    /// 分支，也按 `null` 处理。解析失败返回可抛出的 JS 异常，文案是
    /// `invalid host payload for <kind>: <serde 错误>`。
    pub fn payload_json<T: serde::de::DeserializeOwned>(&self) -> EngineResult<T> {
        let json = match self.ctx.json_stringify(self.payload.clone()) {
            Ok(Some(text)) => text.to_string()?,
            Ok(None) => "null".to_owned(),
            Err(error) => return Err(error),
        };
        serde_json::from_str(&json).map_err(|error| {
            rquickjs::Exception::throw_message(
                &self.ctx,
                &format!("invalid host payload for {}: {error}", self.kind),
            )
        })
    }
}

/// 引擎本体。
///
/// **字段顺序就是析构顺序**：`ctx` 必须在 `rt` 之前（见 [`shutdown`] 的 `Drop` 说明）。
pub struct JsEngine {
    // 1. 完成队列接收端先析构：排队的 `HostCompletion::Invoke` 里可能有
    //    `Persistent<Function>`，必须在 runtime 之前释放。
    completions: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<HostCompletion>>>,
    // 2. 再析构 context（释放 JS 堆对象）
    ctx: Context,
    // 3. 再析构 runtime（`JS_FreeRuntime` 要求 gc_obj_list 为空）
    rt: Runtime,
    // 4. 共享状态（宿主函数闭包仍可能持有引用）
    shared: Rc<EngineShared>,
}

impl JsEngine {
    /// 默认配置创建引擎，并注入 `__kiana_host` 原语。
    ///
    /// 不安装 prelude（intl 必须先于 prelude 安装，见设计文档 §9.2）；调用 [`install`]
    /// 或由 crate-facade 按顺序调用各单元的 `install`。
    pub fn new() -> EngineResult<Self> {
        Self::with_config(EngineConfig::default())
    }

    /// 指定配置创建引擎。
    ///
    /// 顺序见模块文档的“执行流程”。失败不会留下半建状态：未返回的局部值按 Rust 的
    /// 声明逆序析构（`ctx` 先于 `rt`）。
    ///
    /// 配置里的限制只在这里生效；`watchdog` 例外，它在每次 `with_js` 时读取。
    pub fn with_config(config: EngineConfig) -> EngineResult<Self> {
        // 1. QuickJS runtime：限制、loader、中断处理器都挂在它上面。
        let rt = Runtime::new()?;

        // 2. 限制与中断处理器必须在任何 JS 执行之前装好（两者都是 runtime 级状态）。
        let interrupt = std::sync::Arc::new(InterruptState::new());
        limits::install_runtime_limits(
            &rt,
            std::sync::Arc::clone(&interrupt),
            config.memory_limit,
            config.max_stack_size,
        );

        // 3. 完成队列：接收端由引擎持有，发送端进共享状态（不带 runtime 锚，见字段文档）。
        let (tx, rx) = mpsc::unbounded_channel();
        // 4. 弱引用必须在 `rt` 被移动进 `Self` 之前取（`WeakRuntime` 不阻止 runtime 销毁）。
        let weak_runtime = rt.weak();
        let shared = Rc::new(EngineShared {
            config,
            interrupt,
            timers: RefCell::new(timers::TimerQueue::default()),
            pending: RefCell::new(HashMap::new()),
            handles: RefCell::new(HashMap::new()),
            handle_seq: Cell::new(0),
            persistents: RefCell::new(HashMap::new()),
            host_kinds: RefCell::new(HashMap::new()),
            completions: HostCompletionSender::from_sender(tx),
            weak_runtime,
            errors: RefCell::new(Vec::new()),
            module_sources: RefCell::new(HashMap::new()),
            module_seq: Cell::new(0),
            gc_tick: Cell::new(0),
            installed: Cell::new(false),
            stale: RefCell::new(None),
            shutting_down: Cell::new(false),
        });

        // 5. 拒绝跟踪器挂在 runtime 上；先于 Context 登记，第一条 JS 指令起就能记录。
        promise::install_rejection_tracker(&rt, Rc::clone(&shared));

        // 6. full Context（内建对象齐全，prelude/虚拟模块稍后在它里面求值）。
        let ctx = Context::full(&rt)?;
        let engine = Self {
            completions: tokio::sync::Mutex::new(Some(rx)),
            ctx,
            rt,
            shared,
        };
        engine.install_host_primitives()?;
        Ok(engine)
    }

    /// 注入 `__kiana_host`（Promise 协议原语 + 定时器原语）。
    ///
    /// 顺序是硬性的：先建空的 `__kiana_host` 对象，两个 installer 才能往它上面挂成员；
    /// 稍后求值的 prelude 再从这个对象取原语。
    fn install_host_primitives(&self) -> EngineResult<()> {
        self.with_js(|ctx| -> EngineResult<()> {
            let host = rquickjs::Object::new(ctx.clone())?;
            ctx.globals().set("__kiana_host", host)?;
            Ok(())
        })?;
        // 两个 installer 都从全局取 `__kiana_host` 再 set 成员，依赖上一步先建好对象。
        promise::install_host_promise(self)?;
        timers::install_host_timers(self)
    }

    /// **唯一**允许进入 JS 的函数。同步；绝不跨 `await` 持有。
    ///
    /// 每次进入都安装默认看门狗（[`DEFAULT_WATCHDOG`]）；若调用方已用
    /// [`JsEngine::with_deadline`] / [`JsEngine::deadline_guard`] 设了 deadline，
    /// 则沿用调用方的值（显式优先于默认，即使它比 100 ms 短）。
    ///
    /// 需要给单次事务放宽上限时优先用 [`JsEngine::with_js_deadline`]：它进入前设置、
    /// 退出后立刻恢复，deadline 不会泄漏到同一主循环里的其它事务（渲染、定时器、
    /// 其它扩展的完成事件）。[`JsEngine::deadline_guard`] 是引擎级状态，**不得跨
    /// `.await` 持有**（见 `engine/limits.rs` 的 `DeadlineGuard` 文档）。
    pub fn with_js<R>(&self, f: impl for<'js> FnOnce(Ctx<'js>) -> R) -> R {
        // 守卫覆盖整个 `ctx.with`：闭包 panic 时也会随栈展开析构，恢复旧 deadline。
        let _watchdog = limits::enter_watchdog(self);
        self.ctx.with(f)
    }

    /// 在 `global` 作用域求值一段脚本（跑 prelude / 测试用小段代码）。
    ///
    /// 模块加载不走这里（loader 用 `Module::declare`）；`filename` 只影响错误栈。
    ///
    /// 求值失败时会取走并丢弃挂起的 JS 异常：`Ctx` 上残留的异常会被后续的模块加载
    /// （"already loaded" 检查）和 `execute_pending_job` 误读，所以 `eval_script`
    /// 保持“返回 `Err` 即上下文干净”。需要错误文本时用 [`JsEngine::eval_script_error`]。
    pub fn eval_script<T>(&self, source: &str, filename: &str) -> EngineResult<T>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.with_js(|ctx| {
            match ctx.eval_with_options(source, script_options(filename)) {
                Ok(value) => Ok(value),
                Err(error) => {
                    // 清掉 pending exception（`catch` 在 QuickJS 里返回并重置异常槽）。
                    let _ = ctx.catch();
                    Err(error)
                }
            }
        })
    }

    /// 全局脚本求值，错误按 TS 的 `error instanceof Error ? error.message : String(error)`
    /// 物化成 [`JsError`]（`ca-extensions.md` §2.8.4 / §3.8 的 `<error>`）。
    ///
    /// 模块加载/工厂调用失败时用它取【逐字】错误文本。
    pub fn eval_script_error<T>(&self, source: &str, filename: &str) -> Result<T, JsError>
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.with_js(
            |ctx| match ctx.eval_with_options::<T, _>(source, script_options(filename)) {
                Ok(value) => Ok(value),
                Err(_) => Err(promise::take_exception(&ctx, JsErrorKind::Host)),
            },
        )
    }

    /// 安装一个全局：`globalThis.<name> = build(ctx)`。
    ///
    /// ```ignore
    /// engine.register_global("console", |ctx| {
    ///     let console = Object::new(ctx.clone())?;
    ///     Ok(console.into_value())
    /// })?;
    /// ```
    pub fn register_global<F>(&self, name: &str, build: F) -> EngineResult<()>
    where
        F: for<'js> FnOnce(&Ctx<'js>) -> EngineResult<Value<'js>>,
    {
        self.with_js(|ctx| {
            let value = build(&ctx)?;
            ctx.globals().set(name, value)
        })
    }

    /// 枚举全局对象上的自有键（测试与调试用）。
    ///
    /// 只包含可枚举的自有字符串键：prelude 用 `Object.defineProperty` 把
    /// `__kiana`/`__kiana_host` 定义成不可枚举（设计文档 §9.3），因此它们不在这里出现
    /// （`tests/engine.rs` 的 `prelude_installs_protocol_timers_and_abort` 有断言）。
    pub fn global_keys(&self) -> Vec<String> {
        self.with_js(|ctx| {
            let globals = ctx.globals();
            globals.keys::<String>().flatten().collect()
        })
    }

    /// 注册一个 host kind（`__kiana.promise(kind, payload)` 的 Rust 处理器）。
    ///
    /// 覆盖同名 kind。未知 kind 的错误文案是 `unknown host kind: <kind>`（逐字）。
    ///
    /// handler 以 `Rc` 保存：替换或注销不会影响已经开始的那次调用（`host_start` 先克隆
    /// `Rc` 再调用），只影响之后的 `__kiana.promise`。
    pub fn register_host_kind<H>(&self, kind: impl Into<String>, handler: H)
    where
        H: HostKindHandler,
    {
        self.shared
            .host_kinds
            .borrow_mut()
            .insert(kind.into(), Rc::new(handler));
    }

    /// 注销 host kind；返回是否确实存在。
    pub fn unregister_host_kind(&self, kind: &str) -> bool {
        self.shared.host_kinds.borrow_mut().remove(kind).is_some()
    }

    /// 是否已注册该 host kind。
    pub fn has_host_kind(&self, kind: &str) -> bool {
        self.shared.host_kinds.borrow().contains_key(kind)
    }

    /// 已注册的 host kind 名（按注册顺序不保证；仅调试/测试）。
    pub fn host_kinds(&self) -> Vec<String> {
        self.shared.host_kinds.borrow().keys().cloned().collect()
    }

    /// 完成队列发送端（宿主任务持有；主循环用 [`JsEngine::pump_completions`]）。
    ///
    /// 返回的发送端锚住 runtime（见 [`HostCompletionSender`] 的文档）：宿主任务跨
    /// `await` 持有 JS 回调时，引擎销毁（`/reload`、会话替换）不会 abort 进程。
    pub fn completion_sender(&self) -> HostCompletionSender {
        self.shared.anchored_sender()
    }

    /// 等待至少一个完成事件，应用**所有**已排队的事件（settle/invoke），然后泵 job。
    ///
    /// 主循环的 `select!` 分支：`n = engine.pump_completions() => {}`。
    /// 返回应用的事件数；通道关闭（引擎销毁）时返回 0。
    ///
    /// 先阻塞等第一个事件，再把当时已排队的全部取走：一次唤醒处理整批，主循环按批推进。
    /// 应用阶段会进入一次 `with_js`；`HostCompletion::Invoke` 的回调抛出的异常进错误
    /// 队列（`JsErrorKind::Host`），不会被静默丢弃。
    pub async fn pump_completions(&self) -> usize {
        let mut batch = Vec::new();
        {
            // 锁只保护接收端：把当前队列取空后立即释放，进入 JS 的应用阶段不持锁。
            let mut guard = self.completions.lock().await;
            let Some(receiver) = guard.as_mut() else {
                return 0;
            };
            match receiver.recv().await {
                Some(first) => batch.push(first),
                None => return 0,
            }
            while let Ok(more) = receiver.try_recv() {
                batch.push(more);
            }
        }
        // 批量应用：内部进入一次 `with_js`，settle/invoke 之后顺带泵一次 job。
        promise::apply_completions(self, batch)
    }

    /// 不等待地应用当前已排队的完成事件（测试与 `select!` 前的快速路径）。
    ///
    /// 必须在 `with_js` 之外调用（内部会进入一次 `with_js`）。
    /// `try_lock` 失败（另一个调用正在取事件）时直接返回 0，不等待。
    pub fn try_drain_completions(&self) -> usize {
        let mut batch = Vec::new();
        {
            let Ok(mut guard) = self.completions.try_lock() else {
                return 0;
            };
            let Some(receiver) = guard.as_mut() else {
                return 0;
            };
            while let Ok(more) = receiver.try_recv() {
                batch.push(more);
            }
        }
        promise::apply_completions(self, batch)
    }

    /// 泵空微任务队列，返回执行的 job 数。
    ///
    /// `Ctx::execute_pending_job` 会吞掉 job 抛出的错误（§2.4）；未处理拒绝由
    /// rejection tracker 收进 [`JsEngine::take_errors`]。
    pub fn pump_jobs(&self) -> usize {
        self.with_js(|ctx| pump_jobs_with(&ctx))
    }

    /// 手动运行 GC。**必须在 `with_js` 之外**调用。
    pub fn run_gc(&self) {
        self.rt.run_gc();
    }

    /// 底层 runtime 的只读引用。
    ///
    /// 用途仅限 loader：`rquickjs::Runtime::set_loader` 是 0.14 里唯一的模块解析/加载钩子，
    /// 而 `Ctx` 没有 loader setter（设计文档 §1.2、§3.1）。`loader-runtime` /
    /// `loader-compile` 单元用 [`JsEngine::set_loader`] 或本方法把
    /// `Resolver + Loader` 装到 runtime 上。其它 runtime 配置（限制/中断/GC）应走本模块的
    /// API，直接调用可能破坏 `with_js` 的约定。
    ///
    /// **必须在 `with_js` 之外调用**：runtime 的方法会锁住 rquickjs 的内部互斥量，
    /// 在 `ctx.with` 事务里调用会死锁。
    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }

    /// 安装模块解析器与加载器（设计文档 §3.1 的 `Resolver -> Loader::load ->
    /// Module::declare` 流水线）。
    ///
    /// `Runtime::set_loader` 在 runtime 上只保留一份；重复调用会替换旧的
    /// resolver/loader。**代际重载**（[`JsEngine::reload`]）会新建 runtime，调用方必须在
    /// 新引擎上重新安装（crate-facade 按 §9.2 顺序重跑各单元的 `install`）。
    ///
    /// **必须在 `with_js` 之外调用**（内部会锁 runtime，见 [`JsEngine::runtime`]）。
    pub fn set_loader<R, L>(&self, resolver: R, loader: L)
    where
        R: rquickjs::loader::Resolver + 'static,
        L: rquickjs::loader::Loader + 'static,
    {
        self.rt.set_loader(resolver, loader);
    }

    /// 节流 GC：每 100 次调用运行一次。返回本次是否运行。
    ///
    /// 设计文档 §2.1 的主循环每轮调用一次（`rt.run_gc_if_needed()`）；rquickjs 没有这个
    /// 方法，这里的节流是等价物。
    ///
    /// 计数是调用次数，从 1 开始：第 100、200… 次返回 `true` 并执行 GC
    /// （`tests/engine.rs` 的 `run_gc_if_needed_runs_every_hundred_ticks` 用 250 次
    /// 断言恰好 2 次）。计数器是 `u32`，回绕到 0 时也会触发一次。
    pub fn run_gc_if_needed(&self) -> bool {
        let tick = self.shared.gc_tick.get().wrapping_add(1);
        self.shared.gc_tick.set(tick);
        if tick.is_multiple_of(100) {
            self.rt.run_gc();
            true
        } else {
            false
        }
    }

    /// 取走并物化所有已记录的错误（未处理拒绝 + `__kiana.recordError`）。
    ///
    /// 必须在 `with_js` 之外调用（内部会进入一次 `with_js`）。
    pub fn take_errors(&self) -> Vec<JsError> {
        // 先把错误 drain 到局部 Vec 并结束对 `RefCell` 的借用：物化会调用 prelude 的
        // `__kiana.errorText`（执行 JS），期间宿主函数可能再次向 `errors` push；
        // 若 `RefMut` 还活着，那一次借用会 panic。
        let pending: Vec<PendingJsError> = {
            let mut errors = self.shared.errors.borrow_mut();
            if errors.is_empty() {
                return Vec::new();
            }
            errors.drain(..).collect()
        };
        self.with_js(|ctx| {
            pending
                .into_iter()
                .map(|entry| entry.materialize(&ctx))
                .collect()
        })
    }

    /// 未取走的错误数量。
    pub fn error_count(&self) -> usize {
        self.shared.errors.borrow().len()
    }

    /// 寄存一个 `Persistent`：`Drop` 时统一清理（设计文档 §2.8 的 registry/theme 回调）。
    ///
    /// 同一 key 会被覆盖。取回用 [`JsEngine::take_persistent`]。
    pub fn put_persistent<T: 'static>(&self, key: impl Into<String>, value: Persistent<T>) {
        self.shared
            .persistents
            .borrow_mut()
            .insert(key.into(), Box::new(value));
    }

    /// 取回寄存的 `Persistent`（类型必须与寄存时一致，否则返回 `None`）。
    ///
    /// 类型不符时条目**仍会被移除并析构**（不是“取回失败但保留原值”）：`Box<dyn Any>`
    /// 的 downcast 失败会把装箱值还回来，这里直接丢弃。
    pub fn take_persistent<T: 'static>(&self, key: &str) -> Option<Persistent<T>> {
        self.shared
            .persistents
            .borrow_mut()
            .remove(key)?
            .downcast::<Persistent<T>>()
            .ok()
            .map(|boxed| *boxed)
    }

    /// 删除寄存项；返回是否存在。
    pub fn remove_persistent(&self, key: &str) -> bool {
        self.shared.persistents.borrow_mut().remove(key).is_some()
    }

    /// 寄存槽数量。
    pub fn persistent_slot_count(&self) -> usize {
        self.shared.persistents.borrow().len()
    }

    /// 寄存一个 JS 对象句柄，返回 id。
    ///
    /// 必须在 `with_js` 事务内调用：`Object<'js>` 只在该事务里有效，`Persistent::save`
    /// 也需要这次事务的 `Ctx`。句柄内部的 `Persistent` 由引擎持有，`Drop` 时清理。
    ///
    /// id 从 1 开始（0 不会被分配）。本仓库中调用方是测试
    /// （`tests/engine.rs`、`tests/engine_shutdown.rs`）；UI 的 `JsComponent` 走的是
    /// JS 侧 `__kiana_refs` 引用 id（见 `ui/adapter.rs`），不使用这张表。
    pub fn store_handle(&self, object: rquickjs::Object<'_>) -> JsHandleId {
        // 自增后取 max(1)：u64 回绕到 0 时跳过 0，保证 id 永不为 0。
        let id = self.shared.handle_seq.get().wrapping_add(1).max(1);
        self.shared.handle_seq.set(id);
        let ctx = object.ctx().clone();
        let persistent = Persistent::save(&ctx, object);
        self.shared.handles.borrow_mut().insert(id, persistent);
        id
    }

    /// 在 `with_js` 内恢复句柄并调用 `f`。句柄不存在时返回 `Ok(None)`。
    ///
    /// 必须在 `with_js` 之外调用（内部会进入一次 `with_js`）。
    pub fn with_handle<R>(
        &self,
        id: JsHandleId,
        f: impl for<'js> FnOnce(rquickjs::Object<'js>) -> EngineResult<R>,
    ) -> EngineResult<Option<R>> {
        // 先 clone 出 `Persistent`：临时 `Ref` 在本语句结束时析构，进入 JS 后不再持有
        // 句柄表的借用（`f` 内部可能再次访问句柄表）。
        let Some(persistent) = self.shared.handles.borrow().get(&id).cloned() else {
            return Ok(None);
        };
        self.with_js(|ctx| {
            let object = persistent.restore(&ctx)?;
            Ok(Some(f(object)?))
        })
    }

    /// 丢弃句柄；返回是否存在。
    pub fn drop_handle(&self, id: JsHandleId) -> bool {
        self.shared.handles.borrow_mut().remove(&id).is_some()
    }

    /// 句柄数量。
    pub fn handle_count(&self) -> usize {
        self.shared.handles.borrow().len()
    }

    /// 注册模块源（`name -> &'static str`）。
    ///
    /// loader 在安装时把整表快照进自己的源表（`loader/mod.rs` 的 `register_source`），
    /// `Module::declare` 从那里取源；因此本方法应在 `loader::install` 之前调用。
    pub fn register_module_source(&self, name: impl Into<String>, source: &'static str) {
        self.shared
            .module_sources
            .borrow_mut()
            .insert(name.into(), source);
    }

    /// 查模块源。
    pub fn module_source(&self, name: &str) -> Option<&'static str> {
        self.shared.module_sources.borrow().get(name).copied()
    }

    /// 所有模块源（顺序不保证）。
    pub fn module_sources(&self) -> Vec<(String, &'static str)> {
        self.shared
            .module_sources
            .borrow()
            .iter()
            .map(|(name, source)| (name.clone(), *source))
            .collect()
    }

    /// 删除模块源；返回旧的源。
    pub fn remove_module_source(&self, name: &str) -> Option<&'static str> {
        self.shared.module_sources.borrow_mut().remove(name)
    }
}

/// 在**已经进入** JS 的宿主函数里泵 job 队列。
///
/// 宿主函数拿到的是 `Ctx<'js>`，不能在函数体内再调用 [`JsEngine::with_js`]
/// （`RefCell already borrowed`）；需要推进微任务时用本函数。
pub fn pump_jobs_with(ctx: &Ctx<'_>) -> usize {
    let mut executed = 0;
    while ctx.execute_pending_job() {
        executed += 1;
    }
    executed
}

/// 安装 `js/prelude_core.js`（幂等）。
///
/// 内容：`__kiana` 协议、`setTimeout`/`setInterval`/`clearTimeout`/`setImmediate`/
/// `process.nextTick` → `queueMicrotask`、`AbortController`/`AbortSignal`。
/// 只依赖 `__kiana_host`，不 import 任何虚拟模块（设计文档 §9.3）。
///
/// 安装顺序（§9.2）：intl 的 `install` 必须先于本函数；node globals 的 `install` 在其后。
///
/// 只有求值成功才把引擎的 `installed` 置位；失败路径会先 `ctx.catch()` 清掉挂起异常，
/// 所以失败不会污染后续的重试或模块加载（否则残留异常会被它们误读）。
pub fn install(engine: &JsEngine) -> EngineResult<()> {
    if engine.shared.installed.get() {
        return Ok(());
    }
    engine.with_js(|ctx| {
        match ctx.eval_with_options::<(), _>(PRELUDE_CORE, script_options("<prelude_core>")) {
            Ok(()) => {
                engine.shared.installed.set(true);
                Ok(())
            }
            Err(error) => {
                // 失败不得留下挂起异常（否则重试 install / 后续模块加载都会中毒）。
                let _ = ctx.catch();
                Err(error)
            }
        }
    })
}

/// prelude 源文本（测试与体积断言用）。
pub fn prelude_core_source() -> &'static str {
    PRELUDE_CORE
}

/// 全局脚本求值选项（`global` + `strict` + `backtrace_barrier` + 文件名）。
///
/// - `global = true`：按脚本（global code）求值，而不是 module；
/// - `strict = true`：强制严格模式；
/// - `backtrace_barrier = true`：错误栈不包含本次 eval 之前的帧；
/// - `filename`：只影响错误栈/错误信息里的文件名。
///
/// 未启用 `promise`（top-level await 需要它）。`EvalOptions` 是 `#[non_exhaustive]`，
/// 只能先 `default()` 再改字段。
fn script_options(filename: &str) -> EvalOptions {
    let mut options = EvalOptions::default();
    options.global = true;
    options.strict = true;
    options.backtrace_barrier = true;
    options.filename = Some(filename.to_owned());
    options
}
