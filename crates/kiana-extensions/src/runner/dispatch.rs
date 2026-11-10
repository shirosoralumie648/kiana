//! 泛型 `emit` 与 13 个专用派发器（`emitBoundary`…`emitInput`、`emitProjectTrust`）。
//!
//! 合并语义（引用相等、`undefined` 键存在性、原地修改）在 `spec/design-extension-runtime.md`
//! §4.6 要求**用 JS 逐行移植**（`js/dispatch.js`），本模块只做 Rust 侧入口与桥接：
//!
//! 1. 每次派发取一次 handler 快照（扩展注册顺序 × 扩展内注册顺序），把 handler 闭包放进
//!    线程本地的派发表（handler 是 `kiana-ext-api` 的 `ExtensionHandlerFn`，可能持有非 `Send`
//!    的 JS 回调句柄）；
//! 2. 构造 `state`（token、事件 JSON、`paths`/`snapshot`）并调用
//!    `__kiana_dispatch.<emitter>(state)`；JS 侧用 `__kiana.promise("kiana_dispatch_call", …)` 回调
//!    Rust（host 桥，id 即 handler 下标）——JS 扩展与 Rust 原生 handler 因此有完全相同的
//!    顺序与引用语义；
//! 3. 等待 Promise settle（手写 Promise 协议），把结果按住事件类型反序列化。
//!
//! # 线程与生命周期
//!
//! 派发只能在安装过引擎的线程上运行（[`install`] 用 `Weak` 登记当前引擎，不延长引擎
//! 生命周期）。每次 `run_dispatch` 都会：
//!
//! 1. 把快照与上下文装进 `Rc<DispatchTable>`，以单调递增的 `token` 放进线程本地的
//!    `TABLES`；JS 侧后续的 `kiana_dispatch_call` / `kiana_dispatch_build` 只携带这个
//!    token 反查表（表不存在时宿主桥拒绝该 Promise，不会访问已释放的状态）；
//! 2. 用 `TableGuard` 保证无论正常返回、`?` 提前退出还是 future 被丢弃，表条目都会被
//!    移除，并调用 `loader::end_dispatch` 释放「派发中退订」保留的 JS 回调；
//! 3. 嵌套派发（handler 里再 emit）共用同一线程与同一事件循环。`AWAIT_DEPTH` 让只有
//!    最外层真正阻塞等待完成队列（接收端受 tokio `Mutex` 保护），内层改为
//!    `yield_now` 让外层推进 JS job 与宿主完成，避免自锁。
//!
//! # 结果约定
//!
//! - emitter 的 JS 返回值为 `null` 表示“本次没有结果”，各 Rust 入口据此返回 `None`
//!   或原值；`undefined` 与显式 `null` 的区分只存在于 host 桥的 handler 信封
//!   （`hasValue`），不出现在 emitter 的最终返回值里。
//! - `paths[i]` 与 `snapshot[i]` 平行：`snapshot[i]` 是第 i 个扩展在本事件上的 handler
//!   在扁平表中的下标；没有 handler 的扩展也保留一条空记录。
//! - 需要类型化结果的 emitter 会再做一次严格反序列化；形状不符返回
//!   [`DispatchError::InvalidResult`]，不会把半解析的对象交给调用方（`cache_warming`
//!   之类只读取单个字面量的 emitter 例外，见各自文档）。
//!
//! # 错误
//!
//! handler 抛出的错误在 JS 侧被 `catch` 后调用 `__kiana_dispatch_host.reportError(...)`
//! （即 [`ExtensionRunner::emit_error`]），`emitToolCall`/`emitUserBash` 例外：JS 重新抛出，
//! Rust 侧得到 [`DispatchError::Js`]。两者的细节不同：`emitToolCall` 完全不 `catch`
//! （因此不会先上报），`emitUserBash` 先 `reportError` 再重新抛出。
//!
//! 例外：`run_project_trust_dispatch` 由 [`crate::project_trust::emit_project_trust_event`]
//! 传入自己的收集器（不是 `ExtensionRunner::emit_error`），错误按发生顺序进入
//! `ProjectTrustOutcome.errors`。
//!
//! # 与 TS 的已知差异
//!
//! - JS handler 的原地修改由 loader 适配器在每次调用后读回整个事件对象（见
//!   `crate::loader::take_handler_event_mutation`），`handle_call` 放进结果信封的
//!   `event` 字段，`js/dispatch.js` 的 `callHandler` 再写回本地事件对象。**所有**事件
//!   （含 `message_update`/`tool_execution_update`/`provider_stream_event`）的共享对象
//!   都走同一套共享对象语义；
//!   handler 抛错前做的就地修改同样读回（拒绝值携带 `event`/`stack`/`messagesChanged`，
//!   `dispatch.js` 先应用再抛出）。`project_trust` 的适配器走独立的原始值通道，事件
//!   不做读回（其结果只读 `trusted`，跨 handler 的事件可见性未复现）。
//! - 高频事件的“`Persistent` 句柄”物化（设计文档 §4.6）未实现：`kiana-ext-api` 冻结的
//!   `ExtensionHandlerFn` 只接收类型化的 [`ExtensionEvent`]，loader 适配器仍按 JSON
//!   物化事件；句柄通道需要跨单元的接口变更。
//! - `user_bash` 的 `{ operations }` 无法跨 JSON：host 桥用 `{ operations: { __kianaOperations:
//!   true } }` 标记，真实 `BashOperations` 留在派发表里，`emit_user_bash` 返回时取回。
//!
//! # 阅读顺序
//!
//! 1. [`install`]：宿主全局与 host kind 的安装；
//! 2. `run_dispatch` → `call_emitter` → `await_promise`：一次派发的生命周期与 Promise 推进；
//! 3. 13 个专用 emitter 包装：各自如何把 JSON 结果映射成类型化返回值；
//! 4. `ts_event_json` / `extension_event_from_json`：事件在 Rust 与 JS 之间的双向物化。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kiana_ai::types::ProviderHeaders;
use kiana_ext_api::ExtensionEvent;
use kiana_ext_api::events::ExtensionEventKind;
use kiana_ext_api::events::ExtensionHandler;
use kiana_ext_api::events::{
    ExtensionEventResult, ExtensionHandlerFn, ProjectTrustContext, ProjectTrustEvent,
    ProjectTrustHandler, SessionBoundaryDraft, SessionEvent,
};
use kiana_ext_api::tool_result::BashOperations;
use kiana_ext_api::types::{ExtensionContextApi, ExtensionError};
use rquickjs::promise::PromiseState;
use rquickjs::{Ctx, Function, Object, Persistent, Promise, Value};
use serde_json::{Value as JsonValue, json};

use crate::engine::{EngineResult, HostCall, HostValue, JsEngine, JsErrorKind, take_exception};

use kiana_ext_api::system_prompt::NormalizedBuildSystemPromptOptions;

use super::{
    BeforeAgentStartCombinedResult, BeforeAgentStartTrigger, BoundaryBaseEvent,
    BoundaryDispatchResult, BuildBoundaryContext, ExtensionRunner, ResourcePathEntry,
    ResourcesDiscoverPaths, normalize_build_system_prompt_options,
};

/// `core/extensions/runner.ts` 各 emitter 的逐行移植；合并语义只在这里实现，修改时
/// 必须同时核对 TS 源与 `tests/dispatch.rs` 的断言。
const DISPATCH_JS: &str = include_str!("../../js/dispatch.js");

/// `__kiana.promise` 的 host kind：调用一个 handler。
const HOST_CALL_KIND: &str = "kiana_dispatch_call";
/// `__kiana.promise` 的 host kind：`emitBoundary` 的 `buildContext(entries)`。
const HOST_BUILD_KIND: &str = "kiana_dispatch_build";

/// 派发失败（TS 的 Promise rejection）。
///
/// 三类来源：当前线程没有登记引擎（调用顺序错误）、JS 异常或 handler 抛出值、Rust 侧
/// 对结果形状的拒绝。`Js` 的 `name`/`stack` 供调用方区分“真正的 `Error` 实例”与其它
/// 抛出值（`kiana-session` 的 `before_tool_call` 依赖它决定是否加 `Extension failed, …`
/// 前缀），`message` 是逐字错误文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// 当前线程没有可用引擎（`dispatch::install` 尚未调用，或登记的引擎已销毁）。
    EngineNotInstalled,
    /// JS 异常 / handler 抛出的错误。
    Js {
        /// `error.name`。
        name: String,
        /// `error.message`（非 Error 值是 `String(value)`）。
        message: String,
        /// `error.stack`。
        stack: Option<String>,
    },
    /// 结果无法按事件类型反序列化，或 emitter 未返回 Promise 等 Rust 侧形状错误
    /// （TS 在这些位置没有对应的错误类型）。
    InvalidResult(String),
}

impl DispatchError {
    /// 错误文本（逐字进入 `ExtensionError.error` / UI）。
    ///
    /// `Js` 变体返回原始 `error.message`（不追加前缀）；`EngineNotInstalled` 返回固定
    /// 英文文案；`InvalidResult` 返回其携带的文本。`Display` 与 `std::error::Error`
    /// 都转发到这里。
    pub fn message(&self) -> &str {
        match self {
            DispatchError::EngineNotInstalled => "Extension runtime engine is not installed",
            DispatchError::Js { message, .. } => message,
            DispatchError::InvalidResult(message) => message,
        }
    }
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for DispatchError {}

impl From<serde_json::Error> for DispatchError {
    fn from(error: serde_json::Error) -> Self {
        DispatchError::InvalidResult(error.to_string())
    }
}

/// 把 rquickjs 自身的错误包成 `Js`（没有 JS 侧的 `Error` 对象可读，故 `name = "Error"`、
/// 无 `stack`）。
fn js_error(error: rquickjs::Error) -> DispatchError {
    DispatchError::Js {
        name: "Error".to_owned(),
        message: error.to_string(),
        stack: None,
    }
}

/// 取走当前 `Ctx` 上挂起的 JS 异常，保留原始的 `name`/`message`/`stack`。
///
/// 与 [`js_error`] 的区别：这里处理的是 JS 代码真正 `throw` 的值（经 prelude 的
/// `__kiana.errorText` 物化）。真正的 `Error` 实例保留 `name` 与（可能为空的）`stack`；
/// 非 `Error` 的抛出值统一成 `name = "Error"`、无 `stack`。
fn take_js_error(ctx: &Ctx<'_>) -> DispatchError {
    let error: crate::engine::JsError = take_exception(ctx, JsErrorKind::Host);
    DispatchError::Js {
        name: error.name,
        message: error.message,
        stack: error.stack,
    }
}

// ============================================================================
// 线程本地：当前引擎与派发表
// ============================================================================

thread_local! {
    /// 当前线程登记的引擎（按安装顺序，末尾是当前引擎；`Weak` 见 loader 的同名登记）。
    static CURRENT_ENGINES: RefCell<Vec<RcWeak<JsEngine>>> = const { RefCell::new(Vec::new()) };
    /// 派发表：token → 本次派发的 handler 快照与上下文。
    ///
    /// 只在引擎线程上读写。`token` 由 `NEXT_TABLE` 单调分配并作为 `state.token` 交给
    /// JS，宿主桥回调时据此反查；条目由 `TableGuard` 在派发结束时移除（含提前返回与
    /// future 被丢弃）。
    static TABLES: RefCell<HashMap<u64, Rc<DispatchTable>>> = RefCell::new(HashMap::new());
    /// `await_promise` 的嵌套深度。
    ///
    /// 嵌套派发（handler 里再 `emit`）与外层在同一个单线程事件循环上：只有**最外层**可以
    /// 阻塞等待宿主完成队列（完成队列的接收端由一把 tokio `Mutex` 保护，两个 await 同时
    /// 等待会让内层永远拿不到锁）。内层改为让出执行权，由外层推进 JS job/宿主完成。
    static AWAIT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// 派发表 token 的单调序号。`TABLES` 是线程本地的，跨线程不共享表，因此只需保证单线程
// 内不重复；用全局原子量实现最简单。0 被 `report_error` 当作缺省值，所以从 1 开始。
static NEXT_TABLE: AtomicU64 = AtomicU64::new(1);

/// 取当前线程最近的存活引擎。
///
/// 从末尾向前查找并顺带剔除已析构的 `Weak` 登记；`None` 表示本线程没有存活引擎
/// （从未安装或已销毁），派发会得到 [`DispatchError::EngineNotInstalled`]。
fn current_engine() -> Option<Rc<JsEngine>> {
    CURRENT_ENGINES.with(|cell| {
        let mut engines = cell.borrow_mut();
        while let Some(weak) = engines.last() {
            if let Some(engine) = weak.upgrade() {
                return Some(engine);
            }
            engines.pop();
        }
        None
    })
}

/// 把 `engine` 登记为当前引擎（重复登记移到末尾），并剔除已销毁的登记。
fn register_current_engine(engine: &Rc<JsEngine>) {
    CURRENT_ENGINES.with(|cell| {
        let mut engines = cell.borrow_mut();
        engines.retain(|candidate| {
            candidate
                .upgrade()
                .is_some_and(|candidate| !Rc::ptr_eq(&candidate, engine))
        });
        engines.push(Rc::downgrade(engine));
    });
}

/// 一次 handler 的形态（标准事件 vs `project_trust`）。
#[derive(Clone)]
pub(crate) enum DispatchHandler {
    /// 标准事件 handler。
    Standard(ExtensionHandlerFn),
    /// `project_trust` handler。
    ProjectTrust(ProjectTrustHandler),
}

impl std::fmt::Debug for DispatchHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchHandler::Standard(_) => formatter.write_str("Standard(..)"),
            DispatchHandler::ProjectTrust(_) => formatter.write_str("ProjectTrust(..)"),
        }
    }
}

impl From<ExtensionHandler> for DispatchHandler {
    fn from(handler: ExtensionHandler) -> Self {
        match handler {
            ExtensionHandler::Standard(handler) => DispatchHandler::Standard(handler),
            ExtensionHandler::ProjectTrust(handler) => DispatchHandler::ProjectTrust(handler),
        }
    }
}

/// handler 的 `ctx`（TS 每次 emit 只创建一个）。
#[derive(Clone)]
pub(crate) enum TableContext {
    /// 标准事件：`ExtensionContext`。
    Standard(Arc<dyn ExtensionContextApi>),
    /// `project_trust`：`ProjectTrustContext`。
    ProjectTrust(ProjectTrustContext),
}

/// 扩展错误的落点（runner 的 `emitError`；`project_trust` 用收集器）。
pub(crate) type ErrorSink = Arc<dyn Fn(ExtensionError) + Send + Sync>;

/// 一次派发的 handler 快照 + 上下文。
pub(crate) struct DispatchTable {
    /// 扩展错误的落点：标准事件是 `ExtensionRunner::emit_error`，`project_trust` 是
    /// `project_trust.rs` 传入的收集器。
    on_error: ErrorSink,
    /// `before_agent_start` 的 `ctx.getSystemPrompt()` 覆盖落点（runner 用它替换覆盖栈
    /// 顶）；其它 emitter 为 `None`。
    on_system_prompt: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// handler 表。`emit_context` 的第二阶段在 phase 1 结束后才取快照
    /// （TS 的 `snapshotEventHandlers("context_with_system")` 在循环之后），因此可追加。
    handlers: RefCell<Vec<DispatchHandler>>,
    /// 本次派发共享的 `ctx`（TS 每次 emit 只创建一个，所有 handler 共享）。
    context: TableContext,
    /// `emit_boundary` 的 `buildContext` 回调；其它 emitter 为 `None`。
    build: Option<BuildBoundaryContext>,
    /// `user_bash` handler 返回的 `{ operations }`：真实 `BashOperations` 不能跨 JSON，
    /// 暂存在这里由 `emit_user_bash` 取回。
    user_bash_operations: Mutex<Option<Arc<dyn BashOperations>>>,
    system_prompt: Mutex<Option<String>>,
    /// 扩展集合句柄（`context_with_system` 的晚快照）。
    extensions: Option<crate::runner::ExtensionTable>,
    /// `before_agent_start` 的渲染器（`kiana-resources::system_prompt` 的等价物；
    /// 由装配方通过 `ExtensionRunner::set_system_prompt_renderer` 注入）。
    system_prompt_renderer: Option<crate::runner::SystemPromptRenderer>,
}

impl DispatchTable {
    /// 按扁平下标取本次派发快照里的 handler；越界返回 `None`（宿主桥据此拒绝调用）。
    fn handler(&self, id: u64) -> Option<DispatchHandler> {
        self.handlers.borrow().get(id as usize).cloned()
    }

    /// 追加一次晚快照；返回 JS 侧需要的 `paths` 与 handler id 列表（全局下标）。
    ///
    /// 返回的 id 列表与 `snapshot.paths` 一一对应，没有 handler 的扩展保留空列表。
    /// `snapshot.snapshot` 里的局部下标若在 `snapshot.handlers` 中越界会被跳过
    /// （不 panic）——两者来自同一次构造，正常不会发生。
    fn append_snapshot(&self, snapshot: HandlerSnapshot) -> (Vec<String>, Vec<Vec<u64>>) {
        let mut handlers = self.handlers.borrow_mut();
        let mut ids = Vec::with_capacity(snapshot.snapshot.len());
        for index in 0..snapshot.paths.len() {
            let mut extension_ids = Vec::new();
            for handler in snapshot
                .snapshot
                .get(index)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let local = *handler as usize;
                let Some(handler) = snapshot.handlers.get(local).cloned() else {
                    continue;
                };
                extension_ids.push(handlers.len() as u64);
                handlers.push(handler);
            }
            // 即使没有 handler 也要在 `paths` 里留一条与该扩展对应的记录。
            ids.push(extension_ids);
        }
        (snapshot.paths, ids)
    }
}

/// 快照：与 `paths` 平行的 handler id 列表 + 扁平 handler 表。
///
/// `snapshot[i]` 的每个元素是 `handlers` 的下标（第 i 个扩展内按注册顺序）；`paths[i]`
/// 是第 i 个扩展的显示路径。扁平化的原因：host 桥只传一个全局 `hid`，派发表用该下标
/// 直接定位 handler；没有 handler 的扩展仍占一条空 `paths` 记录以保持顺序对齐。
pub(crate) struct HandlerSnapshot {
    paths: Vec<String>,
    snapshot: Vec<Vec<u64>>,
    handlers: Vec<DispatchHandler>,
}

/// 取一次快照（`snapshotEventHandlers`）。嵌套派发重新取（每次 emit 调用一次）。
///
/// 先同步 loader 的实时扩展对象（TS 的 `ext.handlers` 是加载期返回的同一个 Map，
/// 工厂结束后 `kiana.on()` / `kiana.off()` 的晚注册直接生效）。Rust 的 `Extension` 是值类型，
/// `refresh_extension_handlers`（以及全量的 `refresh_extensions`）是对象身份等价物；这里
/// 只刷 handler 表，避免每个事件都克隆工具/命令/渲染器。
///
/// 返回的快照只包含 `event_type` 这一种事件的 handler；扩展顺序与 `runner` 的集合顺序
/// 一致，因此结果顺序对所有 emitter 是确定的。
pub(crate) fn snapshot_handlers(runner: &ExtensionRunner, event_type: &str) -> HandlerSnapshot {
    runner.with_extensions(|extensions| {
        for extension in extensions.iter_mut() {
            crate::loader::refresh_extension_handlers(extension);
        }
        snapshot_from_extensions(extensions, event_type)
    })
}

/// [`snapshot_handlers`] 的纯函数形态（`project_trust` 在 runner 之外调用）。
pub(crate) fn snapshot_from_extensions(
    extensions: &[kiana_ext_api::Extension],
    event_type: &str,
) -> HandlerSnapshot {
    let mut paths = Vec::new();
    let mut snapshot = Vec::new();
    let mut handlers = Vec::new();
    for extension in extensions.iter() {
        let registered = extension
            .handlers
            .get(event_type)
            .cloned()
            .unwrap_or_default();
        let mut ids = Vec::with_capacity(registered.len());
        for handler in registered {
            ids.push(handlers.len() as u64);
            handlers.push(DispatchHandler::from(handler));
        }
        paths.push(extension.path.clone());
        snapshot.push(ids);
    }
    HandlerSnapshot {
        paths,
        snapshot,
        handlers,
    }
}

fn lookup_table(id: u64) -> Option<Rc<DispatchTable>> {
    TABLES.with(|tables| tables.borrow().get(&id).cloned())
}

/// 派发表的生命周期守卫（`Drop` 时移除）。
///
/// 用 `Drop` 而不是在每个返回点手写清理：正常返回、`?` 提前退出与调用方丢弃 future
/// 都走同一条路径。`loader::end_dispatch` 递减派发深度，只在最外层归零时释放
/// 「派发中退订」保留的 JS 回调。
struct TableGuard(u64);

impl Drop for TableGuard {
    fn drop(&mut self) {
        TABLES.with(|tables| {
            tables.borrow_mut().remove(&self.0);
        });
        // 最外层派发结束时，释放「派发中退订」的 JS 回调保留（TS 的数组 splice 不影响
        // 本次派发已取出的函数引用；Rust 用保留表模拟）。
        crate::loader::end_dispatch();
    }
}

// ============================================================================
// 安装
// ============================================================================

/// 安装 `js/dispatch.js`、host kind 与 `__kiana_dispatch_host` 原语（幂等）。
///
/// 由 crate-facade 按 `design-extension-runtime.md` §9.2 的第 7 步（`api.js`/`context.js`/
/// `dispatch.js`）调用；同一引擎重复调用是 no-op（只把它恢复为当前引擎），新引擎会登记为
/// 当前引擎（`Weak`：登记不延长引擎生命周期）。
///
/// 调用时机：`Generation::with_config` 在 `engine::install`（`__kiana` Promise 协议）与
/// `runner::context::install` 之后调用本函数；`dispatch.js` 只在派发时才使用
/// `__kiana.promise`，安装本身不依赖 prelude 已求值。
///
/// 失败模式：host kind 与全局原语在脚本求值**之前**注册。若 `eval_script` 或
/// `register_global` 失败，函数返回错误但引擎上已留下部分安装状态；此时重复调用会因
/// `has_host_kind` 命中而直接返回 `Ok(())`，`__kiana_dispatch` 仍然缺失。调用方应把
/// 这次失败视为整代引擎不可用（crate-facade 的 `Generation::with_config` 会返回错误并
/// 丢弃这个引擎）。
pub fn install(engine: Rc<JsEngine>) -> EngineResult<()> {
    register_current_engine(&engine);
    if engine.has_host_kind(HOST_CALL_KIND) {
        return Ok(());
    }
    engine.register_host_kind(HOST_CALL_KIND, handle_call);
    engine.register_host_kind(HOST_BUILD_KIND, handle_build);
    engine.register_global("__kiana_dispatch_host", |ctx| {
        let host = Object::new(ctx.clone())?;
        let report = Function::new(ctx.clone(), report_error)?;
        host.set("reportError", report)?;
        let set_prompt = Function::new(ctx.clone(), set_system_prompt)?;
        host.set("setSystemPrompt", set_prompt)?;
        let render_prompt = Function::new(ctx.clone(), render_system_prompt)?;
        host.set("renderSystemPrompt", render_prompt)?;
        let snapshot = Function::new(ctx.clone(), snapshot_context_with_system)?;
        host.set("snapshotContextWithSystem", snapshot)?;
        Ok(host.into_value())
    })?;
    engine.eval_script::<()>(DISPATCH_JS, "<dispatch.js>")?;
    Ok(())
}

/// `__kiana_dispatch_host.reportError(payload)`：逐条落成一个扩展错误。
///
/// payload 由 `dispatch.js` 的 `report` / `reportRaw` 构造（`{token, extensionPath,
/// event, error, stack}`）；字段缺失按空串 / `None` 处理。token 对应的派发表已结束
/// （或从未存在）时静默丢弃：错误上报本身不能反过来让 emitter 失败。
fn report_error<'js>(ctx: Ctx<'js>, payload: Value<'js>) -> rquickjs::Result<()> {
    let value = value_to_json(&ctx, payload)?;
    let token = value.get("token").and_then(JsonValue::as_u64).unwrap_or(0);
    let Some(table) = lookup_table(token) else {
        return Ok(());
    };
    let extension_path = value
        .get("extensionPath")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let event = value
        .get("event")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let error = value
        .get("error")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let stack = value
        .get("stack")
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    (table.on_error)(ExtensionError {
        extension_path,
        event,
        error,
        stack,
    });
    Ok(())
}

/// `__kiana_dispatch_host.setSystemPrompt(token, prompt)`：`before_agent_start` 的 ctx 覆盖。
///
/// 写入表内字段后调用 `on_system_prompt`（runner 用它替换系统提示覆盖栈顶），使 handler
/// 执行期间 `ctx.getSystemPrompt()` 读到按当前选项渲染的文本。token 失效时静默忽略。
fn set_system_prompt(token: u64, prompt: String) {
    let Some(table) = lookup_table(token) else {
        return;
    };
    *table
        .system_prompt
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(prompt.clone());
    if let Some(sink) = &table.on_system_prompt {
        sink(prompt);
    }
}

/// `__kiana_dispatch_host.renderSystemPrompt(token, options)`：按**当前**选项渲染系统提示
/// （TS 的 `event.systemPrompt` getter / `ctx.getSystemPrompt()` 在 `before_agent_start`
/// 期间每次读取都重新执行 `buildSystemPrompt(currentOptions)`）。
///
/// 没有注入渲染器时返回 `undefined`，`dispatch.js` 退回缓存的提示文本。
///
/// 另外两种返回 `undefined` 的情形：token 已失效；`options` 无法反序列化成
/// `NormalizedBuildSystemPromptOptions` 且其中没有字符串 `forceSystemPrompt`。
fn render_system_prompt<'js>(
    ctx: Ctx<'js>,
    token: u64,
    options: Value<'js>,
) -> EngineResult<Value<'js>> {
    let Some(table) = lookup_table(token) else {
        return Ok(Value::new_undefined(ctx.clone()));
    };
    let options = match value_to_json(&ctx, options) {
        Ok(options) => options,
        Err(_) => return Ok(Value::new_undefined(ctx.clone())),
    };
    if let Some(renderer) = table.system_prompt_renderer.clone()
        && let Ok(normalized) =
            serde_json::from_value::<NormalizedBuildSystemPromptOptions>(options.clone())
    {
        let rendered = renderer(&normalized);
        return rquickjs::String::from_str(ctx.clone(), &rendered).map(|text| text.into_value());
    }
    // 没有注入渲染器时复现 `buildSystemPrompt` 的 `forceSystemPrompt` 分支（直接返回它，
    // 不渲染段落）。其余选项需要 `kiana-resources` 的渲染实现（见模块文档）。
    if let Some(force) = options.get("forceSystemPrompt").and_then(JsonValue::as_str) {
        return rquickjs::String::from_str(ctx.clone(), force).map(|text| text.into_value());
    }
    Ok(Value::new_undefined(ctx.clone()))
}

/// `__kiana_dispatch_host.snapshotContextWithSystem(token)`：phase 1 结束后才取
/// `context_with_system` 的 handler 快照（TS 在 `emitContext` 的第二个循环之前调用
/// `snapshotEventHandlers`），并追加进当前派发表。
///
/// 返回 `{paths, snapshot}`；token 失效或该派发表没有扩展句柄时返回 `undefined`，
/// `dispatch.js` 这时退回 `state.paths2` / `state.snapshot2`（fake host 的测试路径）。
fn snapshot_context_with_system<'js>(ctx: Ctx<'js>, token: u64) -> EngineResult<Value<'js>> {
    let Some(table) = lookup_table(token) else {
        return Ok(Value::new_undefined(ctx.clone()));
    };
    let Some(extensions) = table.extensions else {
        return Ok(Value::new_undefined(ctx.clone()));
    };
    let snapshot = extensions.with(|extensions| {
        for extension in extensions.iter_mut() {
            crate::loader::refresh_extension_handlers(extension);
        }
        snapshot_from_extensions(extensions, "context_with_system")
    });
    let (paths, snapshot) = table.append_snapshot(snapshot);
    json_value(&ctx, &json!({ "paths": paths, "snapshot": snapshot }))
}

// ============================================================================
// host kind：调用 handler / 构建边界上下文
// ============================================================================

#[derive(serde::Deserialize)]
struct CallPayload {
    token: u64,
    hid: u64,
    event: JsonValue,
}

/// host kind `kiana_dispatch_call`：调用派发表里的第 `hid` 个 handler。
///
/// payload 形状 `{token, hid, event}`。payload 必须在 `spawn_local` 之前解析
/// （`HostCall` 里的 JS `Value` 不能跨出 `with_js`），随后在 LocalSet 上运行 handler；
/// 完成时：
///
/// - 成功：`resolve({hasValue, value, event?, eventRefs?, messagesChanged?,
///   inputImagesSameRef?})`，`hasValue` 区分 `undefined` 与显式 `null`；
/// - 失败：`reject({__kianaDispatchError: true, name, message, stack, event?,
///   eventRefs?, messagesChanged?})`，`dispatch.js` 的 `callHandler` 先应用事件读回值
///   再抛出等价 `Error`。
///
/// payload 解析失败返回 `Err`（引擎把它转成 JS 异常）；token/hid 失效等异步失败只
/// `reject` 对应的 Promise，不会中断 JS 侧已启动的派发循环。
fn handle_call(call: HostCall<'_>) -> EngineResult<()> {
    let payload: CallPayload = call.payload_json()?;
    let sender = call.completion_sender();
    let id = call.id();
    let Some(table) = lookup_table(payload.token) else {
        sender.reject_message(id, "extension dispatch is no longer active");
        return Ok(());
    };
    let Some(handler) = table.handler(payload.hid) else {
        sender.reject_message(id, "unknown extension handler");
        return Ok(());
    };
    let event_json = payload.event;
    tokio::task::spawn_local(async move {
        match handler {
            DispatchHandler::Standard(handler) => {
                let event = match extension_event_from_json(&event_json) {
                    Ok(event) => event,
                    Err(message) => {
                        sender.reject_message(id, message);
                        return;
                    }
                };
                let TableContext::Standard(context) = &table.context else {
                    sender.reject_message(id, "extension handler context mismatch");
                    return;
                };
                // 清掉上一次（非派发路径的 handler 调用）可能留下的读回值。
                let _ = crate::loader::take_handler_event_mutation();
                let _ = crate::loader::take_handler_event_refs();
                let _ = crate::loader::take_handler_input_images_same_ref();
                let _ = crate::loader::take_handler_messages_changed();
                let _ = crate::loader::take_last_js_error_stack();
                match handler(event, Arc::clone(context)).await {
                    Ok(result) => {
                        // JS handler 会就地修改事件对象（headers/input/entries/options）；
                        // 适配器把整个事件对象读回，这里放进信封让 dispatch.js 写回。
                        let mutation = crate::loader::take_handler_event_mutation();
                        let refs = crate::loader::take_handler_event_refs();
                        let images_same_ref = crate::loader::take_handler_input_images_same_ref();
                        let messages_changed = crate::loader::take_handler_messages_changed();
                        let mut envelope = handler_result_envelope(&table, result, mutation);
                        if let Some(changed) = messages_changed {
                            envelope["messagesChanged"] = json!(changed);
                        }
                        if let Some(same_ref) = images_same_ref {
                            envelope["inputImagesSameRef"] = json!(same_ref);
                        }
                        if let Some(refs) = refs {
                            envelope["eventRefs"] = refs;
                        }
                        sender.resolve(id, HostValue::Json(envelope));
                    }
                    Err(error) => {
                        // TS 的 handler 与 runner 共享事件对象：抛错前做的就地修改**不会**
                        // 回滚。适配器在失败路径上同样读回了事件对象（见 loader 的
                        // `call_with_event_capture`），把它连同错误与 stack 一起拒绝，
                        // dispatch.js 的 `callHandler` 先应用修改再抛出。
                        let mutation = crate::loader::take_handler_event_mutation();
                        let refs = crate::loader::take_handler_event_refs();
                        let messages_changed = crate::loader::take_handler_messages_changed();
                        let stack = crate::loader::take_last_js_error_stack();
                        reject_handler(
                            &sender,
                            id,
                            error.as_ref(),
                            stack,
                            mutation,
                            refs,
                            messages_changed,
                        );
                    }
                }
            }
            DispatchHandler::ProjectTrust(handler) => {
                let event: ProjectTrustEvent = match serde_json::from_value(event_json) {
                    Ok(event) => event,
                    Err(error) => {
                        sender.reject_message(id, error.to_string());
                        return;
                    }
                };
                let TableContext::ProjectTrust(context) = &table.context else {
                    sender.reject_message(id, "extension handler context mismatch");
                    return;
                };
                let _ = crate::loader::take_project_trust_handler_failure();
                let _ = crate::loader::take_last_js_error_stack();
                let result = handler(event, context.clone()).await;
                // `ProjectTrustHandler` 没有错误通道：适配器把 TS 的抛出（含 null 结果
                // 的 TypeError 文案）放进线程本地槽；拒绝 Promise 让 dispatch.js 的
                // catch 按 `extensionPath`/`event: "project_trust"` 上报后继续。
                if let Some(message) = crate::loader::take_project_trust_handler_failure() {
                    let stack = crate::loader::take_last_js_error_stack();
                    let payload = json!({
                        "__kianaDispatchError": true,
                        "name": "Error",
                        "message": message,
                        "stack": stack,
                    });
                    sender.reject(id, HostValue::Json(payload));
                    return;
                }
                sender.resolve(
                    id,
                    HostValue::Json(envelope_value(
                        serde_json::to_value(result).unwrap_or(JsonValue::Null),
                    )),
                );
            }
        }
    });
    Ok(())
}

/// 拒绝一次 handler 调用，携带 TS 的 `err` 语义（`name`/`message`/`stack`）与抛错前的
/// 共享事件读回值。
///
/// dispatch.js 的 `callHandler` 把 `event` 写回本地事件对象后抛出一个等价的 `Error`
/// （`stack` 是扩展原始抛出的栈，而不是宿主桥新建 `Error` 的栈）。
///
/// payload 顶层的 `__kianaDispatchError: true` 是 `dispatch.js` 识别“宿主拒绝”的标记；
/// `name` 优先取 [`crate::loader::JsCallbackError`] 的 `name`，其它 Rust 错误回退
/// `"Error"`。
fn reject_handler(
    sender: &crate::engine::HostCompletionSender,
    id: u64,
    error: &(dyn std::error::Error + 'static + Send + Sync),
    stack: Option<String>,
    mutation: Option<JsonValue>,
    refs: Option<JsonValue>,
    messages_changed: Option<bool>,
) {
    let name = error
        .downcast_ref::<crate::loader::JsCallbackError>()
        .map_or_else(|| "Error".to_owned(), |error| error.name.clone());
    let mut payload = json!({
        "__kianaDispatchError": true,
        "name": name,
        "message": error.to_string(),
        "stack": stack,
    });
    if let Some(event) = mutation {
        payload["event"] = event;
    }
    if let Some(refs) = refs {
        payload["eventRefs"] = refs;
    }
    if let Some(changed) = messages_changed {
        payload["messagesChanged"] = json!(changed);
    }
    sender.reject(id, HostValue::Json(payload));
}

#[derive(serde::Deserialize)]
struct BuildPayload {
    token: u64,
    entries: Vec<SessionBoundaryDraft>,
}

/// host kind `kiana_dispatch_build`：`emitBoundary` 的 `buildContext(entries)`。
///
/// payload `{token, entries}`，`entries` 是当前共享草稿数组。回调在 LocalSet 上异步执行：
/// 成功 resolve 上下文预览 JSON；失败 `reject_message`（`dispatch.js` 捕获后上报
/// `Invalid boundary entries: …` 并把 `valid` 置 false）。token 失效或派发表没有构建
/// 回调时立即 reject。
fn handle_build(call: HostCall<'_>) -> EngineResult<()> {
    let payload: BuildPayload = call.payload_json()?;
    let sender = call.completion_sender();
    let id = call.id();
    let Some(table) = lookup_table(payload.token) else {
        sender.reject_message(id, "extension dispatch is no longer active");
        return Ok(());
    };
    let Some(build) = table.build.clone() else {
        sender.reject_message(id, "boundary context builder is not available");
        return Ok(());
    };
    let entries = payload.entries;
    tokio::task::spawn_local(async move {
        match build(entries).await {
            Ok(preview) => match serde_json::to_value(&preview) {
                Ok(value) => {
                    sender.resolve(id, HostValue::Json(value));
                }
                Err(error) => {
                    sender.reject_message(id, error.to_string());
                }
            },
            Err(message) => {
                sender.reject_message(id, message);
            }
        }
    });
    Ok(())
}

/// 把已有结果包成 `hasValue = true` 的信封（`project_trust` 结果没有 `None` 形态）。
fn envelope_value(value: JsonValue) -> JsonValue {
    json!({ "hasValue": true, "value": value })
}

/// 结果信封：`{ hasValue, value, event? }`。
///
/// `event` 是 JS handler 调用后读回的事件对象（就地修改的载体）；`hasValue` 区分
/// `undefined`（false）与显式返回值（true），`value` 是 handler 的返回值。
///
/// `user_bash` 的两条通道在这里特殊处理：`{operations}` 的真实 `BashOperations` 无法
/// JSON 化，只写 `{operations: {__kianaOperations: true}}` 标记并把对象留在派发表；
/// `{result}` 带 `__kianaNative` 标记，让 JS 侧跳过 `"exitCode" in result` 的字面校验
/// （类型化 `BashResult` 的 `None` 在 JSON 里表现为键缺失）。
fn handler_result_envelope(
    table: &DispatchTable,
    result: Option<ExtensionEventResult>,
    mutation: Option<JsonValue>,
) -> JsonValue {
    let has_value = result.is_some();
    let value = match result {
        None => JsonValue::Null,
        Some(ExtensionEventResult::UserBash(
            kiana_ext_api::events::UserBashEventResult::Operations(operations),
        )) => {
            *table
                .user_bash_operations
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(operations);
            json!({ "operations": { "__kianaOperations": true } })
        }
        // Rust 原生 handler 的 `BashResult` 是类型化构造的：`exitCode` 的 `None` 对应
        // TS 的属性 `undefined`，JSON 无法表达，故用 `__kianaNative` 标记跳过 JS 侧严格
        // 校验（JS handler 的结果已经在 loader 里按 TS 规则校验过）。
        Some(ExtensionEventResult::UserBash(
            kiana_ext_api::events::UserBashEventResult::Result(result),
        )) => match serde_json::to_value(&result) {
            Ok(value) => json!({ "result": value, "__kianaNative": true }),
            Err(_) => JsonValue::Null,
        },
        Some(other) => extension_event_result_json(other),
    };
    let mut envelope = json!({ "hasValue": has_value, "value": value });
    if let Some(event) = mutation {
        envelope["event"] = event;
    }
    envelope
}

/// handler 结果 → TS 结果对象（逐事件类型的序列化映射）。
///
/// 每个变体只会在对应的 emitter 里出现。`UserBash` 的两条通道由
/// [`handler_result_envelope`] 单独处理，因此这里的 `UserBash(_)` 分支不可达，仅为
/// 穷尽 match。序列化失败退化为 `null`（类型化结果正常不会失败）。
fn extension_event_result_json(result: ExtensionEventResult) -> JsonValue {
    match result {
        ExtensionEventResult::ResourcesDiscover(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::SessionBeforeSwitch(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::SessionBeforeFork(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::SessionBeforeCompact(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::SessionBeforeTree(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::Context(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::CacheWarming(value) => match value.action {
            Some(action) => json!({ "action": action.as_str() }),
            None => json!({}),
        },
        ExtensionEventResult::BeforeProviderRequest(value) => value,
        ExtensionEventResult::BeforeAgentStart(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::TurnEnd(value) | ExtensionEventResult::AgentBeforeSettle(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::MessageEnd(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::ToolCall(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::ToolResult(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
        ExtensionEventResult::UserBash(_) => JsonValue::Null,
        ExtensionEventResult::Input(value) => {
            serde_json::to_value(value).unwrap_or(JsonValue::Null)
        }
    }
}

// ============================================================================
// 调用 JS emitter 并等待 Promise
// ============================================================================

/// JSON → JS 值（经 `JSON.parse`，对象/数组/数字按 JSON 语义物化）。
fn json_value<'js>(ctx: &Ctx<'js>, value: &JsonValue) -> EngineResult<Value<'js>> {
    let text = serde_json::to_string(value)
        .map_err(|error| rquickjs::Exception::throw_message(ctx, &error.to_string()))?;
    ctx.json_parse(text)
}

/// JS 值 → JSON（经 `JSON.stringify`）。`undefined` 返回 `null`。
///
/// 调用点要么在信封里单独携带 `hasValue`，要么该位置本来就不区分 `null` / `undefined`；
/// 需要保留这一区分的地方（handler 返回值）走 `handler_result_envelope`。
fn value_to_json<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<JsonValue> {
    match ctx.json_stringify(value) {
        Ok(Some(text)) => {
            let text = text.to_string()?;
            Ok(serde_json::from_str(&text).unwrap_or(JsonValue::Null))
        }
        Ok(None) => Ok(JsonValue::Null),
        Err(error) => Err(error),
    }
}

/// 调用 `globalThis.__kiana_dispatch.<method>(state)` 并保存返回的 Promise。
///
/// `this` 绑定为 dispatch 对象（`dispatch.js` 的 emitter 不依赖 `this`）。失败路径：
/// 全局/方法缺失、参数物化失败、JS 抛异常都映射成 [`DispatchError::Js`]（经
/// [`take_js_error`] 保留原始 `name`/`message`/`stack`）；方法返回非 Promise 是 Rust
/// 侧的 [`DispatchError::InvalidResult`]（`dispatch.js` 注册的 emitter 都是 `async` 函数）。
fn call_emitter(
    engine: &JsEngine,
    method: &str,
    state: &JsonValue,
) -> Result<Persistent<Promise<'static>>, DispatchError> {
    engine.with_js(
        |ctx| -> Result<Persistent<Promise<'static>>, DispatchError> {
            let dispatch: Object<'_> = ctx.globals().get("__kiana_dispatch").map_err(js_error)?;
            let function: Function<'_> = dispatch.get(method).map_err(js_error)?;
            let argument = json_value(&ctx, state).map_err(|_| take_js_error(&ctx))?;
            let returned: Value<'_> = function
                .call((rquickjs::function::This(dispatch.clone()), argument))
                .map_err(|_| take_js_error(&ctx))?;
            match returned.as_promise() {
                Some(promise) => Ok(Persistent::save(&ctx, promise.clone())),
                None => Err(DispatchError::InvalidResult(format!(
                    "dispatch emitter {method} did not return a promise"
                ))),
            }
        },
    )
}

/// 驱动一个 JS Promise 到 settle（与 loader 的 `await_js_promise` 同一循环）。
///
/// # 推进顺序
///
/// 每一轮先在 `with_js` 里检查 Promise 状态（Resolved 取 JSON、Rejected 取挂起异常）；
/// Pending 时依次尝试：泵微任务（`pump_jobs`）→ 无等待地应用宿主完成
/// （`try_drain_completions`）→ 触发到期定时器。仍无进展时：
///
/// - 嵌套派发（`AWAIT_DEPTH > 1`）只 `yield_now`，由最外层推进同一事件循环；
/// - 最外层按最近的定时器 `select!`（完成队列 vs 睡眠）；没有定时器就阻塞等待完成
///   队列，完成队列已关闭（引擎销毁）时退化为让出，避免空转。
///
/// # 失败模式
///
/// Promise 被拒绝时用 `take_exception` 物化，`name`/`stack` 来自扩展抛出的原始值，
/// 返回 [`DispatchError::Js`]。`AWAIT_DEPTH` 由 `Drop` 守卫归零，提前返回也会恢复。
async fn await_promise(
    engine: &JsEngine,
    promise: Persistent<Promise<'static>>,
) -> Result<JsonValue, DispatchError> {
    // 记录嵌套深度：最外层负责真正阻塞等待，内层只让出执行权。
    let depth = AWAIT_DEPTH.with(|cell| {
        let depth = cell.get() + 1;
        cell.set(depth);
        depth
    });
    let _depth_guard = AwaitDepthGuard;
    loop {
        // 每轮都在一个新的 JS 事务里读状态；`Value` 不出闭包。
        let state = engine.with_js(|ctx| -> Result<PromiseView, DispatchError> {
            let promise = promise.clone().restore(&ctx).map_err(js_error)?;
            match promise.state() {
                PromiseState::Resolved => {
                    let value = promise
                        .result::<Value<'_>>()
                        .expect("resolved promise has a result")
                        .map_err(|_| take_js_error(&ctx))?;
                    let json = value_to_json(&ctx, value).map_err(|_| take_js_error(&ctx))?;
                    Ok(PromiseView::Value(json))
                }
                PromiseState::Rejected => {
                    let _ = promise.result::<Value<'_>>();
                    Err(take_js_error(&ctx))
                }
                PromiseState::Pending => Ok(PromiseView::Pending),
            }
        })?;
        match state {
            PromiseView::Value(value) => return Ok(value),
            PromiseView::Pending => {}
        }

        // 先推进微任务：emitter 的同步部分与 handler 的 Promise 链都在这里前进。
        if engine.pump_jobs() > 0 {
            continue;
        }
        // 再应用已完成的宿主调用（settle 会触发更多 job，下一轮再泵）。
        if engine.try_drain_completions() > 0 {
            continue;
        }
        // 到期定时器可能 resolve handler 里的 sleep 等 Promise。
        if let Some(deadline) = engine.next_timer_deadline()
            && deadline <= tokio::time::Instant::now()
        {
            engine.fire_due_timers();
            continue;
        }
        if depth > 1 {
            // 嵌套派发：外层负责阻塞等待与 JS job；这里只让出执行权后重新检查。
            tokio::task::yield_now().await;
            continue;
        }
        // 最外层：没有可立即推进的工作，阻塞等待宿主完成或下一个定时器。
        match engine.next_timer_deadline() {
            Some(deadline) => {
                tokio::select! {
                    _ = engine.pump_completions() => {}
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            }
            None => {
                // 没有待触发的定时器：等宿主任务完成；若完成队列已关闭则让出执行权，
                // 避免空转（引擎销毁后 emitter 不可能再 settle）。
                if engine.pump_completions().await == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }
    }
}

enum PromiseView {
    Value(JsonValue),
    Pending,
}

/// [`AWAIT_DEPTH`] 的归零守卫。
struct AwaitDepthGuard;

impl Drop for AwaitDepthGuard {
    fn drop(&mut self) {
        AWAIT_DEPTH.with(|cell| cell.set(cell.get().saturating_sub(1)));
    }
}

/// 一次派发的结果 + 留在派发表里的 `user_bash` operations。
struct DispatchOutcome {
    /// emitter 的最终 JSON 返回值。
    value: JsonValue,
    /// `user_bash` handler 返回的 operations（派发表暂存值的副本）。
    user_bash_operations: Option<Arc<dyn BashOperations>>,
}

/// 派发表的可选部件（大多数 emitter 不需要）。
#[derive(Default)]
struct DispatchConfig {
    /// 扩展集合句柄（`emit_context` 的第二阶段晚快照）。
    extensions: Option<crate::runner::ExtensionTable>,
    /// `before_agent_start` 的当前选项渲染器。
    system_prompt_renderer: Option<crate::runner::SystemPromptRenderer>,
}

/// 通用派发：注册表 → 调 JS → 等结果 → 收集 `user_bash` operations。
///
/// 输入：emitter 方法名（`dispatch.js` 的键）、事件类型（用于取快照与 `state.type`）、
/// 已取好的快照、handler 共享的 `ctx`、可选的边界构建回调、emitter 需要的额外
/// `state` 字段，以及错误与系统提示的落点。
///
/// 阶段：
/// 1. 把快照装进 `DispatchTable`，用新 token 登记到线程本地表；
/// 2. 往 `state` 写入 `token`/`type`/`paths`/`snapshot`；
/// 3. 取当前线程引擎（缺失 → [`DispatchError::EngineNotInstalled`]），调用 emitter 并
///    等待 Promise settle；
/// 4. 取回 `user_bash` 暂存的 operations，随结果返回。
///
/// 生命周期：`TableGuard` 在函数返回或 future 被丢弃时移除表并结束派发；派发结束后
/// 到达的宿主回调会收到 `extension dispatch is no longer active` 的拒绝，而不是访问
/// 悬空状态。`state` 必须是对象，否则第 2 步的插入不生效（当前所有调用点都传 `json!`
/// 对象）。
#[allow(clippy::too_many_arguments)]
async fn run_dispatch(
    method: &str,
    event_type: &str,
    snapshot: HandlerSnapshot,
    context: TableContext,
    build: Option<BuildBoundaryContext>,
    state: JsonValue,
    on_error: ErrorSink,
    on_system_prompt: Option<Arc<dyn Fn(String) + Send + Sync>>,
    config: DispatchConfig,
) -> Result<DispatchOutcome, DispatchError> {
    let HandlerSnapshot {
        paths,
        snapshot,
        handlers,
    } = snapshot;
    let table = Rc::new(DispatchTable {
        on_error,
        on_system_prompt,
        handlers: RefCell::new(handlers),
        context,
        build,
        user_bash_operations: Mutex::new(None),
        system_prompt: Mutex::new(None),
        extensions: config.extensions,
        system_prompt_renderer: config.system_prompt_renderer,
    });
    // 先登记再调用 JS：JS 侧的任何宿主回调都只能通过 token 反查这张表。
    let token = register_table_rc(Rc::clone(&table));
    let _guard = TableGuard(token);
    crate::loader::begin_dispatch();

    // 把派发元数据并入 emitter 的 state；对象以外的形状无法承载 token（当前调用点
    // 都是 `json!({...})` 对象）。
    let mut state = state;
    if let Some(object) = state.as_object_mut() {
        object.insert("token".to_owned(), json!(token));
        object.insert("type".to_owned(), json!(event_type));
        object.insert("paths".to_owned(), json!(paths));
        object.insert("snapshot".to_owned(), json!(snapshot));
    }

    let engine = current_engine().ok_or(DispatchError::EngineNotInstalled)?;
    let promise = call_emitter(&engine, method, &state)?;
    let value = await_promise(&engine, promise).await?;
    let user_bash_operations = table
        .user_bash_operations
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    Ok(DispatchOutcome {
        value,
        user_bash_operations,
    })
}

/// 分配 token 并登记派发表（`NEXT_TABLE` 单调递增；表是线程本地的）。
fn register_table_rc(table: Rc<DispatchTable>) -> u64 {
    let id = NEXT_TABLE.fetch_add(1, Ordering::SeqCst);
    TABLES.with(|tables| {
        tables.borrow_mut().insert(id, table);
    });
    id
}

// ============================================================================
// 13 个 emitter
// ============================================================================

fn event_json(event: &ExtensionEvent) -> Result<JsonValue, DispatchError> {
    Ok(ts_event_json(event))
}

/// 事件 → JS 可见的 JSON，键序按 TS 的**对象字面量构造顺序**（`Object.keys`/`JSON.stringify`
/// 可观察）。
///
/// `kiana-ext-api` 的结构体字段顺序与 TS 的构造顺序不一致（`ToolCallEvent` 的 `toolName`
/// 在最后、`TurnEndEvent` 的 `outcome` 在 `type` 之前），因此这里按 TS 调用点重建键序：
/// `agent-session.ts` 的 `tool_call`/`tool_result`/`turn_end`/`agent_before_settle`
/// 对象字面量。其它事件的字段顺序已经一致，原样返回。
pub(crate) fn ts_event_json(event: &ExtensionEvent) -> JsonValue {
    let value = serde_json::to_value(event).unwrap_or(JsonValue::Null);
    let JsonValue::Object(object) = value else {
        return value;
    };
    match event {
        ExtensionEvent::ToolCall(_) => ordered_object(
            object,
            &[
                "type",
                "toolName",
                "toolCallId",
                "parentToolCallId",
                "input",
            ],
        ),
        ExtensionEvent::ToolResult(_) => ordered_object(
            object,
            &[
                "type",
                "toolName",
                "toolCallId",
                "parentToolCallId",
                "input",
                "content",
                "details",
                "structuredContent",
                "isError",
                "usage",
            ],
        ),
        ExtensionEvent::TurnEnd(_) => ordered_object(
            object,
            &[
                "type",
                "turnIndex",
                "message",
                "toolResults",
                "messageEntryId",
                "toolResultEntryIds",
                "outcome",
                "entries",
                "continue",
                "context",
            ],
        ),
        ExtensionEvent::AgentBeforeSettle(_) => ordered_object(
            object,
            &["type", "outcome", "entries", "continue", "context"],
        ),
        // TS: `{ ...trigger, type, get systemPrompt, systemPromptOptions }`——触发字段
        // （`messageType`/`prompt`/`images` 或 `messageType`/`message`）在展开时先插入。
        ExtensionEvent::BeforeAgentStart(kiana_ext_api::events::BeforeAgentStartEvent::User(_)) => {
            ordered_object(
                object,
                &[
                    "messageType",
                    "prompt",
                    "images",
                    "type",
                    "systemPromptOptions",
                    "systemPrompt",
                ],
            )
        }
        ExtensionEvent::BeforeAgentStart(kiana_ext_api::events::BeforeAgentStartEvent::Custom(
            _,
        )) => ordered_object(
            object,
            &[
                "messageType",
                "message",
                "type",
                "systemPromptOptions",
                "systemPrompt",
            ],
        ),
        ExtensionEvent::ProviderStreamEvent(_) => {
            ordered_object(object, &["data", "type", "provider", "api", "model"])
        }
        _ => JsonValue::Object(object),
    }
}

/// 按 `order` 重排对象键；`order` 里不存在的键留在末尾（保持原相对顺序）。
pub(crate) fn ordered_object(
    mut object: serde_json::Map<String, JsonValue>,
    order: &[&str],
) -> JsonValue {
    let mut result = serde_json::Map::new();
    for key in order {
        if let Some(value) = object.shift_remove(*key) {
            result.insert((*key).to_owned(), value);
        }
    }
    for (key, value) in object {
        result.insert(key, value);
    }
    JsonValue::Object(result)
}

/// 按 `serde` 反序列化结果；失败映射成 [`DispatchError::InvalidResult`]。
fn from_json<T: serde::de::DeserializeOwned>(value: JsonValue) -> Result<T, DispatchError> {
    serde_json::from_value(value).map_err(|error| DispatchError::InvalidResult(error.to_string()))
}

/// 泛型 `emit`。
///
/// 只接受 `session_before_*` 系列的结果：JS 循环只对这类事件保留最后一个 handler 的
/// 返回值，并在 `cancel` 为真时短路；其它事件即使 handler 返回了值也会被 JS 忽略
/// （`state.type` 决定是否收集）。JS 返回 `null`（没有结果）映射为 `Ok(None)`；Rust
/// 侧收到非 null 却无法归类时返回 [`DispatchError::InvalidResult`]（防御路径）。
///
/// handler 抛错不在这里返回 `Err`：JS 侧逐个 `catch` 后 `reportError`，继续下一个
/// handler（错误进入 `ExtensionRunner` 的错误监听器）。
pub async fn emit(
    runner: &ExtensionRunner,
    event: ExtensionEvent,
) -> Result<Option<ExtensionEventResult>, DispatchError> {
    let kind = event.kind();
    let event_type = kind.as_str();
    let snapshot = snapshot_handlers(runner, event_type);
    let state = json!({ "event": event_json(&event)? });
    let outcome = run_dispatch(
        "emit",
        event_type,
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    if outcome.value.is_null() {
        return Ok(None);
    }
    session_before_result(kind, outcome.value).map(Some)
}

/// `session_before_*` 的结果归一化与反序列化。
///
/// `coerce_handler_result` 按 JS 语义修正 `cancel` 等字段（真值化、丢弃无法解析的
/// `summary.usage`），避免一条病态结果中断整次派发。其它事件类型没有结果类型，返回
/// [`DispatchError::InvalidResult`]。
fn session_before_result(
    kind: ExtensionEventKind,
    value: JsonValue,
) -> Result<ExtensionEventResult, DispatchError> {
    // TS 只读 `cancel`（真值）并原样传递结果对象；Rust 的结果类型是强类型的，因此先按
    // JS 语义归一化（`{cancel: 1}` → cancel，部分 `summary.usage` 丢弃等），避免整条结果
    // 使整次派发失败。
    let value = crate::loader::coerce_handler_result(kind, value);
    Ok(match kind {
        ExtensionEventKind::SessionBeforeSwitch => {
            ExtensionEventResult::SessionBeforeSwitch(from_json(value)?)
        }
        ExtensionEventKind::SessionBeforeFork => {
            ExtensionEventResult::SessionBeforeFork(from_json(value)?)
        }
        ExtensionEventKind::SessionBeforeCompact => {
            ExtensionEventResult::SessionBeforeCompact(from_json(value)?)
        }
        ExtensionEventKind::SessionBeforeTree => {
            ExtensionEventResult::SessionBeforeTree(from_json(value)?)
        }
        other => {
            return Err(DispatchError::InvalidResult(format!(
                "{} handlers must not return a result",
                other.as_str()
            )));
        }
    })
}

/// `emitCacheWarmingDecision`。
///
/// JS 从事件自带的 `action` 起步，后续每个 handler 返回的 `action` 覆盖前值（最后一次
/// 获胜）。Rust 侧只映射消费者可观察的语义：字面量 `"stop"` → `Stop`，其余（含未
/// 覆盖、`null`、未知字符串）→ `Warm`；`kiana_config::CacheWarmingAction` 只有两个字面量。
pub async fn emit_cache_warming_decision(
    runner: &ExtensionRunner,
    event: kiana_ext_api::CacheWarmingDecisionEvent,
) -> Result<kiana_config::cache::warmer::CacheWarmingAction, DispatchError> {
    let snapshot = snapshot_handlers(runner, "cache_warming_decision");
    let event_value = cache_warming_event_json(&event);
    let state = json!({ "event": event_value });
    let outcome = run_dispatch(
        "emitCacheWarmingDecision",
        "cache_warming_decision",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    // TS 原样返回 handler 赋给 `result.action` 的值；唯一的消费者（cache warmer）用
    // `=== "stop"` 判定，因此任何非 `"stop"`（含未知字符串 / null / 未赋值）都等价于
    // `warm`。`kiana_config::CacheWarmingAction` 只能表达两个字面量，这里按该可观察语义映射。
    Ok(match outcome.value.as_str() {
        Some("stop") => kiana_config::cache::warmer::CacheWarmingAction::Stop,
        _ => kiana_config::cache::warmer::CacheWarmingAction::Warm,
    })
}

/// `CacheWarmingDecisionEvent` 没有实现 `Serialize`（`kiana-ext-api` 在事件联合里手写），
/// 这里按 TS 的对象字面量顺序构造同样的 JSON。
fn cache_warming_event_json(event: &kiana_ext_api::CacheWarmingDecisionEvent) -> JsonValue {
    json!({
        "type": "cache_warming_decision",
        "warmCost": event.warm_cost,
        "missCost": event.miss_cost,
        "continuationProbability": event.continuation_probability,
        "action": event.action.as_str(),
    })
}

/// `emitMessageEnd`。
///
/// JS 逐个 handler 传递当前的 `message`；返回 `message` 的 handler 必须保持 `role`
/// 不变（否则上报错误并跳过）。没有任何修改时 JS 返回 `null` → `Ok(None)`；否则返回
/// 最终消息。
pub async fn emit_message_end(
    runner: &ExtensionRunner,
    event: kiana_ext_api::events::MessageEndEvent,
) -> Result<Option<kiana_agent_core::AgentMessage>, DispatchError> {
    let snapshot = snapshot_handlers(runner, "message_end");
    let state = json!({ "event": serde_json::to_value(&event)? });
    let outcome = run_dispatch(
        "emitMessageEnd",
        "message_end",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    if outcome.value.is_null() {
        return Ok(None);
    }
    from_json(outcome.value).map(Some)
}

/// `emitToolResult`。
///
/// JS 在同一个补丁对象上合并各 handler 的 `content`/`details`/`structuredContent`/
/// `isError`/`usage`（替换 `content` 且未同时给 `structuredContent` 时删除旧值）。没有
/// 修改时返回 `null` → `Ok(None)`；有修改时先按 JS 语义归一化再反序列化成
/// `ToolResultEventResult`（病态补丁字段被忽略，而不是中断整次派发）。
pub async fn emit_tool_result(
    runner: &ExtensionRunner,
    event: kiana_ext_api::events::ToolResultEvent,
) -> Result<Option<kiana_ext_api::events::ToolResultEventResult>, DispatchError> {
    let snapshot = snapshot_handlers(runner, "tool_result");
    let state = json!({ "event": serde_json::to_value(&event)? });
    let outcome = run_dispatch(
        "emitToolResult",
        "tool_result",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    if outcome.value.is_null() {
        return Ok(None);
    }
    // TS 的补丁对象无类型（`content` 非数组、`usage` 缺字段也原样返回）；按 JS 语义
    // 归一化后再反序列化，避免一条病态结果中断整次派发。
    let value = crate::loader::coerce_handler_result(ExtensionEventKind::ToolResult, outcome.value);
    from_json(value).map(Some)
}

/// `emitToolCall`（无 try/catch）。
///
/// `event.input` 是 TS 的共享可变参数对象：handler 原地修改后，后续 handler 看到修改，
/// 调用方拿到的 `event` 也带着最终参数（`ToolArguments` 的共享语义）。没有 handler 修改时
/// `event` 保持不变。
///
/// 错误路径：`dispatch.js` 的 `emitToolCall` 没有 `try/catch`，任一 handler 抛错都会让
/// 整个 Promise reject，Rust 返回 `Err(DispatchError::Js)`（不会先经 `reportError`）。
/// JS 侧 `block` 为真时立即短路；没有结果时 `hasResult=false` → `Ok(None)`。
pub async fn emit_tool_call(
    runner: &ExtensionRunner,
    event: &mut kiana_ext_api::events::ToolCallEvent,
) -> Result<Option<kiana_ext_api::events::ToolCallEventResult>, DispatchError> {
    let snapshot = snapshot_handlers(runner, "tool_call");
    let state = json!({ "event": serde_json::to_value(&*event)? });
    let outcome = run_dispatch(
        "emitToolCall",
        "tool_call",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    if let Some(input) = outcome.value.get("input").cloned() {
        apply_tool_call_input(event, input)?;
    }
    if !outcome
        .value
        .get("hasResult")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    from_json(
        outcome
            .value
            .get("result")
            .cloned()
            .unwrap_or(JsonValue::Null),
    )
    .map(Some)
}

/// 把 JS 侧最终 `event.input` 写回类型化的 [`kiana_ext_api::events::ToolCallEvent`]。
///
/// `Custom` 直接替换 `input`；内置工具的输入是具体结构体，重新反序列化整个事件
/// （TS 的 handler 也可能改写其它字段，但只有 `input` 参与执行）。
fn apply_tool_call_input(
    event: &mut kiana_ext_api::events::ToolCallEvent,
    input: JsonValue,
) -> Result<(), DispatchError> {
    let JsonValue::Object(input) = input else {
        return Err(DispatchError::InvalidResult(
            "tool_call input must be an object".to_owned(),
        ));
    };
    if let kiana_ext_api::events::ToolCallEvent::Custom(custom) = event {
        custom.input = serde_json::from_value(JsonValue::Object(input))
            .map_err(|error| DispatchError::InvalidResult(error.to_string()))?;
        return Ok(());
    }
    let mut value = serde_json::to_value(&*event)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("input".to_owned(), JsonValue::Object(input));
    }
    *event = serde_json::from_value(value).map_err(|error| {
        DispatchError::InvalidResult(format!("invalid tool_call input: {error}"))
    })?;
    Ok(())
}

/// `emitUserBash`（唯一“先上报再重新抛出”的 emitter）。
///
/// handler 抛错时 JS 先 `reportError` 再重新抛出，因此 Rust 得到 `Err`，而错误监听器
/// 也收到一条记录。对照：`emitToolCall` 完全不捕获（只抛不报），泛型 `emit` 捕获并
/// 上报后继续（不抛）。返回值：
///
/// - `{ operations }`：真实 `BashOperations` 留在派发表里，返回
///   `UserBashEventResult::Operations`；
/// - `{ result }`：按 `BashResult` 反序列化；
/// - `undefined`（emitter 最终返回 `null`）→ `Ok(None)`，调用方走本地执行；
/// - 其它形状：JS 侧抛出【逐字】的 `Invalid user_bash handler result: …`；Rust 侧
///   “value 不是 null 却缺少 result 键”的分支是防御路径，返回同样的错误文案。
pub async fn emit_user_bash(
    runner: &ExtensionRunner,
    event: kiana_ext_api::events::UserBashEvent,
) -> Result<Option<kiana_ext_api::events::UserBashEventResult>, DispatchError> {
    let snapshot = snapshot_handlers(runner, "user_bash");
    let state = json!({ "event": serde_json::to_value(&event)? });
    let outcome = run_dispatch(
        "emitUserBash",
        "user_bash",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    if let Some(operations) = outcome.user_bash_operations {
        return Ok(Some(
            kiana_ext_api::events::UserBashEventResult::Operations(operations),
        ));
    }
    if outcome.value.is_null() {
        return Ok(None);
    }
    let Some(result) = outcome.value.get("result") else {
        return Err(DispatchError::InvalidResult(
            "Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object"
                .to_owned(),
        ));
    };
    from_json(result.clone())
        .map(kiana_ext_api::events::UserBashEventResult::Result)
        .map(Some)
}

/// `emitContext`（两阶段）。
///
/// phase 1 的 `context` handler 只看到非 system 消息；phase 2 的 `context_with_system`
/// 在 phase 1 结束后重新取快照（JS 通过 `snapshotContextWithSystem` 向 Rust 要），因此
/// phase 1 里动态 `on/off` 的结果参与本次派发。单个 handler 的错误上报后继续；返回
/// 最终消息数组，数组形状不符才返回 [`DispatchError::InvalidResult`]。
pub async fn emit_context(
    runner: &ExtensionRunner,
    messages: Vec<kiana_agent_core::AgentMessage>,
) -> Result<Vec<kiana_agent_core::AgentMessage>, DispatchError> {
    // TS: phase 1 的 `context` 快照在循环前取；phase 2 的 `context_with_system` 快照在
    // phase 1 结束之后才取（`runner.ts:1336`）。JS 循环通过
    // `__kiana_dispatch_host.snapshotContextWithSystem` 在 phase 2 开始时向 Rust 要快照，
    // 因此 phase 1 里动态 `kiana.on()/off()` 的增删会反映到 phase 2。
    let context_snapshot = snapshot_handlers(runner, "context");
    let state = json!({
        "messages": serde_json::to_value(&messages)?,
        // 没有 host 快照回调的测试环境（fake host）可自带 `paths2`/`snapshot2`。
        "paths2": [],
        "snapshot2": [],
    });
    let outcome = run_dispatch(
        "emitContext",
        "context",
        context_snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig {
            extensions: Some(runner.table),
            system_prompt_renderer: None,
        },
    )
    .await?;
    from_json(outcome.value)
}

/// `emitBeforeProviderRequest`。
///
/// handler 可以返回替换后的 payload，也可以原地修改 `event.payload`；两者都会让 JS 返回
/// `{payload, replaced: true}`。`replaced` 为 false 时直接返回调用方传入的 `payload`
/// 原值（不经过 JSON 往返）。
pub async fn emit_before_provider_request(
    runner: &ExtensionRunner,
    payload: JsonValue,
) -> Result<JsonValue, DispatchError> {
    let snapshot = snapshot_handlers(runner, "before_provider_request");
    let state = json!({
        "event": { "type": "before_provider_request", "payload": payload },
    });
    let outcome = run_dispatch(
        "emitBeforeProviderRequest",
        "before_provider_request",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    let replaced = outcome
        .value
        .get("replaced")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    if !replaced {
        return Ok(payload);
    }
    Ok(outcome
        .value
        .get("payload")
        .cloned()
        .unwrap_or(JsonValue::Null))
}

/// `emitBeforeProviderHeaders`。
///
/// handler 只通过原地修改 `headers` 生效，返回值被忽略；每个 handler 之后 JS 读回同一
/// 事件对象。结果没有 `headers` 键时返回调用方传入的 `headers` 原值。
pub async fn emit_before_provider_headers(
    runner: &ExtensionRunner,
    headers: ProviderHeaders,
) -> Result<ProviderHeaders, DispatchError> {
    let snapshot = snapshot_handlers(runner, "before_provider_headers");
    let state = json!({
        "event": { "type": "before_provider_headers", "headers": serde_json::to_value(&headers)? },
    });
    let outcome = run_dispatch(
        "emitBeforeProviderHeaders",
        "before_provider_headers",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    match outcome.value.get("headers") {
        Some(value) => from_json(value.clone()),
        None => Ok(headers),
    }
}

/// `emitBeforeAgentStart`。
///
/// 流程：先按当前选项渲染初始系统提示（注入渲染器优先，否则 `forceSystemPrompt` 或
/// runner 当前提示），把覆盖压入 runner 的栈；派发期间 `event.systemPrompt` 是访问器、
/// `ctx.getSystemPrompt()` 读同一渲染结果。派发结束后弹栈并归一化结果。
///
/// 返回各 handler 追加的自定义消息与最终选项：`messages` 缺省或非数组时视为空；
/// 结果缺少 `systemPromptOptions` 或选项无法反序列化时返回
/// [`DispatchError::InvalidResult`]。
///
/// 注意：覆盖栈的 push/pop 不是 RAII；若本 future 在派发中途被丢弃，`pop` 不会执行，
/// 覆盖值会留在 runner 上。
pub async fn emit_before_agent_start(
    runner: &ExtensionRunner,
    trigger: BeforeAgentStartTrigger,
    system_prompt_options: kiana_ext_api::system_prompt::BuildSystemPromptOptions,
) -> Result<BeforeAgentStartCombinedResult, DispatchError> {
    let current_options = normalize_build_system_prompt_options(&system_prompt_options);
    let renderer = runner.system_prompt_renderer();
    // TS: `renderCurrentSystemPrompt = () => buildSystemPrompt(currentOptions)`，初始值也由
    // 当前选项渲染（`forceSystemPrompt` 优先，由 `buildSystemPrompt` 自身处理）。没有注入
    // 渲染器时退回 runner 当前的系统提示（`kiana-extensions` 不能依赖 `kiana-resources`，见
    // 模块文档）。
    let initial_prompt = match &renderer {
        Some(render) => render(&current_options),
        None => current_options
            .force_system_prompt
            .clone()
            .unwrap_or_else(|| runner.current_system_prompt()),
    };
    let event_type = "before_agent_start";
    let snapshot = snapshot_handlers(runner, event_type);

    let trigger_json = match &trigger {
        BeforeAgentStartTrigger::User { prompt, images } => {
            let mut value = json!({ "messageType": "user", "prompt": prompt });
            if let Some(images) = images {
                value["images"] = serde_json::to_value(images)?;
            }
            value
        }
        BeforeAgentStartTrigger::Custom { message } => {
            json!({ "messageType": "custom", "message": serde_json::to_value(message)? })
        }
    };
    let state = json!({
        "trigger": trigger_json,
        "systemPrompt": initial_prompt,
        "systemPromptOptions": serde_json::to_value(&current_options)?,
    });

    runner.core().push_system_prompt_override(initial_prompt);
    let outcome = run_dispatch(
        "emitBeforeAgentStart",
        event_type,
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig {
            extensions: None,
            system_prompt_renderer: renderer,
        },
    )
    .await;
    runner.core().pop_system_prompt_override();
    let outcome = outcome?;

    let messages: Vec<kiana_ext_api::types::CustomMessagePayload> =
        match outcome.value.get("messages") {
            Some(JsonValue::Array(values)) => values
                .iter()
                .cloned()
                .map(from_json)
                .collect::<Result<Vec<_>, _>>()?,
            _ => Vec::new(),
        };
    let options = outcome
        .value
        .get("systemPromptOptions")
        .cloned()
        .ok_or_else(|| {
            DispatchError::InvalidResult("before_agent_start returned no options".to_owned())
        })?;
    Ok(BeforeAgentStartCombinedResult {
        messages,
        system_prompt_options: from_json(options)?,
    })
}

/// `emitResourcesDiscover`。
///
/// JS 逐个 handler 调用并收集三类路径，每条都补上来源扩展的 `extensionPath`。返回顺序
/// 是扩展顺序 × 扩展内注册顺序；`parse_paths` 会跳过没有字符串 `path` 的条目。
pub async fn emit_resources_discover(
    runner: &ExtensionRunner,
    cwd: &str,
    reason: kiana_ext_api::events::ResourcesDiscoverReason,
) -> Result<ResourcesDiscoverPaths, DispatchError> {
    let snapshot = snapshot_handlers(runner, "resources_discover");
    let reason_json = serde_json::to_value(reason)?;
    let state = json!({
        "event": { "type": "resources_discover", "cwd": cwd, "reason": reason_json },
    });
    let outcome = run_dispatch(
        "emitResourcesDiscover",
        "resources_discover",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    Ok(ResourcesDiscoverPaths {
        skill_paths: parse_paths(&outcome.value, "skillPaths")?,
        prompt_paths: parse_paths(&outcome.value, "promptPaths")?,
        theme_paths: parse_paths(&outcome.value, "themePaths")?,
    })
}

/// 解析 `resources_discover` 结果里的一个路径数组。
///
/// 条目必须是对象且 `path` 是字符串，否则跳过；`extensionPath` 缺失时为空串。
fn parse_paths(value: &JsonValue, key: &str) -> Result<Vec<ResourcePathEntry>, DispatchError> {
    let Some(JsonValue::Array(entries)) = value.get(key) else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(path) = entry.get("path").and_then(JsonValue::as_str) else {
            continue;
        };
        let extension_path = entry
            .get("extensionPath")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        paths.push(ResourcePathEntry {
            path: path.to_owned(),
            extension_path: extension_path.to_owned(),
        });
    }
    Ok(paths)
}

/// `emitInput`。
///
/// JS 依次调用 handler（`handled` 立即短路）；`transform` 链式替换文本/图片，最后与
/// 调用前的值比较，有变化才返回 `Transform`，否则 `Continue`。`images` 为 `None` 时
/// 不写 `images` 键（JS 读到 `undefined`），`streamingBehavior` 同理。
pub async fn emit_input(
    runner: &ExtensionRunner,
    text: &str,
    images: Option<Vec<kiana_ai::types::ImageContent>>,
    source: kiana_ext_api::events::InputSource,
    streaming_behavior: Option<kiana_ext_api::events::StreamingBehavior>,
) -> Result<kiana_ext_api::events::InputEventResult, DispatchError> {
    let snapshot = snapshot_handlers(runner, "input");
    let mut state = json!({
        "text": text,
        "source": serde_json::to_value(source)?,
    });
    // TS 的 `InputEvent.images` 在调用方传 undefined 时是 `undefined`（键存在、值为
    // undefined），不是 `null`。JSON 无法表达 undefined：省略键后 JS 读到的同样是
    // `undefined`。
    if let Some(images) = &images {
        state["images"] = serde_json::to_value(images)?;
    }
    if let Some(behavior) = streaming_behavior {
        state["streamingBehavior"] = serde_json::to_value(behavior)?;
    }
    let outcome = run_dispatch(
        "emitInput",
        "input",
        snapshot,
        TableContext::Standard(runner.create_context()),
        None,
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    // `transform` 的 `text` 在 TS 里可以是任意 JS 值（`String()` 归一化）。
    let value = crate::loader::coerce_handler_result(ExtensionEventKind::Input, outcome.value);
    from_json(value)
}

/// `emitBoundary` 的派发（由 `boundary.rs` 调用）。
///
/// `base_event` 去掉 `entries`/`continue`/`context` 后作为每个 handler 事件的基座；
/// 草稿数组、继续标志与上下文预览全部在 `dispatch.js` 里维护（每个 handler 之后重新
/// `buildContext`）。返回 JS 的最终裁决：`valid=false` 时 JS 已把 entries 清空、
/// `continue` 置 false，但 `context` 保留最后一次构建结果。
pub(crate) async fn run_boundary_dispatch(
    runner: &ExtensionRunner,
    base_event: BoundaryBaseEvent,
    build_context: BuildBoundaryContext,
) -> Result<BoundaryDispatchResult, DispatchError> {
    let event_type = base_event.event_type();
    let snapshot = snapshot_handlers(runner, event_type);
    let state = json!({
        "base": base_event.base_json()?,
        "entries": [],
        "context": JsonValue::Null,
    });
    let outcome = run_dispatch(
        "emitBoundary",
        event_type,
        snapshot,
        TableContext::Standard(runner.create_context()),
        Some(build_context),
        state,
        runner.error_sink(),
        runner.system_prompt_sink(),
        DispatchConfig::default(),
    )
    .await?;
    Ok(BoundaryDispatchResult {
        entries: from_json(
            outcome
                .value
                .get("entries")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )?,
        r#continue: outcome
            .value
            .get("continue")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        context: from_json(
            outcome
                .value
                .get("context")
                .cloned()
                .unwrap_or(JsonValue::Null),
        )?,
        valid: outcome
            .value
            .get("valid")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
    })
}

/// `emitProjectTrust` 的派发（由 `project_trust.rs` 调用；不需要 `ExtensionRunner`）。
///
/// 与标准事件一致，先刷新 live 扩展并取快照；handler 顺序执行，第一个
/// `trusted != "undecided"` 的结果获胜（其余不再调用），单个 handler 的错误经
/// `on_error` 收集后继续。JS 返回 `null` → `Ok(None)`。
pub(crate) async fn run_project_trust_dispatch(
    extensions: &[kiana_ext_api::Extension],
    event: ProjectTrustEvent,
    context: ProjectTrustContext,
    on_error: ErrorSink,
) -> Result<Option<kiana_ext_api::events::ProjectTrustEventResult>, DispatchError> {
    // 与标准事件一致：先取 loader 的实时快照（晚注册的 `kiana.on("project_trust", ...)`）。
    let mut live = extensions.to_vec();
    for extension in live.iter_mut() {
        crate::loader::refresh_extension_handlers(extension);
    }
    let snapshot = snapshot_from_extensions(&live, "project_trust");
    let state = json!({ "event": serde_json::to_value(&event)? });
    let outcome = run_dispatch(
        "emitProjectTrust",
        "project_trust",
        snapshot,
        TableContext::ProjectTrust(context),
        None,
        state,
        on_error,
        None,
        DispatchConfig::default(),
    )
    .await?;
    if outcome.value.is_null() {
        return Ok(None);
    }
    from_json(outcome.value).map(Some)
}

// ============================================================================
// 事件 JSON → `ExtensionEvent`
// ============================================================================

/// 把 JS 侧的事件对象反序列化成 `ExtensionEvent`。
///
/// 用于 host 桥的回程：`dispatch.js` 把每次 handler 调用的 `event` JSON 交给 Rust，由
/// `handle_call` 还原成类型化事件再调用 Rust 原生 handler。`type` 必须是字符串且在
/// 已知事件集合内；未知 type 与字段缺失都返回错误文本（`handle_call` 用它拒绝该次
/// handler 调用的 Promise）。
pub(crate) fn extension_event_from_json(value: &JsonValue) -> Result<ExtensionEvent, String> {
    let kind = value
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing string field `type`".to_owned())?
        .to_owned();
    fn parse<T: serde::de::DeserializeOwned>(value: &JsonValue) -> Result<T, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
    Ok(match kind.as_str() {
        "project_trust" => ExtensionEvent::ProjectTrust(parse(value)?),
        "resources_discover" => ExtensionEvent::ResourcesDiscover(parse(value)?),
        "mcp_servers_change" => ExtensionEvent::McpServersChange(parse(value)?),
        "session_start" => ExtensionEvent::Session(SessionEvent::Start(parse(value)?)),
        "session_info_changed" => ExtensionEvent::Session(SessionEvent::InfoChanged(parse(value)?)),
        "session_before_switch" => {
            ExtensionEvent::Session(SessionEvent::BeforeSwitch(parse(value)?))
        }
        "session_before_fork" => ExtensionEvent::Session(SessionEvent::BeforeFork(parse(value)?)),
        "session_before_compact" => {
            ExtensionEvent::Session(SessionEvent::BeforeCompact(parse(value)?))
        }
        "session_compact" => ExtensionEvent::Session(SessionEvent::Compact(parse(value)?)),
        "session_compact_failed" => {
            ExtensionEvent::Session(SessionEvent::CompactFailed(parse(value)?))
        }
        "session_shutdown" => ExtensionEvent::Session(SessionEvent::Shutdown(parse(value)?)),
        "session_before_tree" => ExtensionEvent::Session(SessionEvent::BeforeTree(parse(value)?)),
        "session_tree" => ExtensionEvent::Session(SessionEvent::Tree(parse(value)?)),
        "context" => ExtensionEvent::Context(parse(value)?),
        "context_with_system" => ExtensionEvent::ContextWithSystem(parse(value)?),
        "cache_warming_decision" => {
            // `kiana-ext-api` 只给该事件实现了 Serialize；按 TS 对象形状手工重建。
            let sender_field = |name: &str| {
                value
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("missing field `{name}`"))
            };
            let number = |name: &str| -> Result<f64, String> {
                sender_field(name)?
                    .as_f64()
                    .ok_or_else(|| format!("field `{name}` must be a number"))
            };
            let action = match value.get("action").and_then(JsonValue::as_str) {
                Some("warm") => kiana_config::cache::warmer::CacheWarmingAction::Warm,
                Some("stop") => kiana_config::cache::warmer::CacheWarmingAction::Stop,
                other => return Err(format!("invalid cache warming action: {other:?}")),
            };
            ExtensionEvent::CacheWarmingDecision(kiana_ext_api::CacheWarmingDecisionEvent {
                warm_cost: number("warmCost")?,
                miss_cost: number("missCost")?,
                continuation_probability: number("continuationProbability")?,
                action,
            })
        }
        "before_provider_request" => ExtensionEvent::BeforeProviderRequest(parse(value)?),
        "before_provider_headers" => ExtensionEvent::BeforeProviderHeaders(parse(value)?),
        "after_provider_response" => ExtensionEvent::AfterProviderResponse(parse(value)?),
        "provider_stream_event" => ExtensionEvent::ProviderStreamEvent(parse(value)?),
        "before_agent_start" => ExtensionEvent::BeforeAgentStart(parse(value)?),
        "agent_start" => ExtensionEvent::AgentStart(parse(value)?),
        "agent_end" => ExtensionEvent::AgentEnd(parse(value)?),
        "agent_before_settle" => ExtensionEvent::AgentBeforeSettle(parse(value)?),
        "agent_settled" => ExtensionEvent::AgentSettled(parse(value)?),
        "ui_prompt_start" => ExtensionEvent::UIPromptStart(parse(value)?),
        "ui_prompt_end" => ExtensionEvent::UIPromptEnd(parse(value)?),
        "turn_start" => ExtensionEvent::TurnStart(parse(value)?),
        "turn_end" => ExtensionEvent::TurnEnd(parse(value)?),
        "message_start" => ExtensionEvent::MessageStart(parse(value)?),
        "message_update" => ExtensionEvent::MessageUpdate(parse(value)?),
        "message_end" => ExtensionEvent::MessageEnd(parse(value)?),
        "tool_execution_start" => ExtensionEvent::ToolExecutionStart(parse(value)?),
        "tool_execution_update" => ExtensionEvent::ToolExecutionUpdate(parse(value)?),
        "tool_execution_end" => ExtensionEvent::ToolExecutionEnd(parse(value)?),
        "model_select" => ExtensionEvent::ModelSelect(parse(value)?),
        "thinking_level_select" => ExtensionEvent::ThinkingLevelSelect(parse(value)?),
        "user_bash" => ExtensionEvent::UserBash(parse(value)?),
        "input" => ExtensionEvent::Input(parse(value)?),
        "tool_call" => ExtensionEvent::ToolCall(parse(value)?),
        "tool_result" => ExtensionEvent::ToolResult(parse(value)?),
        other => return Err(format!("unknown extension event type: {other}")),
    })
}
