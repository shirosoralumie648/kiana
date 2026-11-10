//! `create_context` / `create_tool_context` / `create_command_context`（18 个成员，读时求值）。
//!
//! TS 对应物：`packages/coding-agent/src/core/extensions/runner.ts` 的 `createContext`、
//! `createToolContext`、`createCommandContext`（`ca-extensions.md` §2.3.10、
//! `design-extension-runtime.md` §4.4/§5.2–§5.3、`ARCHITECTURE.md` §4.9）。
//!
//! # 合并契约（`runner-core` 在本单元之后合并）
//!
//! 本文件**不引用** `ExtensionRunner`。状态全部通过 [`ContextHost`] 读取；`runner-core` 为
//! `ExtensionRunner` 实现它。每次访问都先 `host.assert_active()`，值从不快照（TS 的 getter）。
//!
//! 两层 API：
//!
//! 1. **Rust 视图** [`ExtensionContext`]：实现 [`ExtensionContextApi`]/[`ExtensionToolContextApi`]/
//!    [`ExtensionCommandContextApi`]/[`ReplacedSessionContextApi`]，供 `kiana-tools`/`kiana-session`
//!    的 Rust 回调使用。`ExtensionContextApi::ui`/`session_manager`/`model_registry` 的返回类型是
//!    `&dyn`，而 runner 的这三个字段在构造后不再改变（`ui` 除外——见下），所以视图在**创建时**
//!    捕获它们的 `Arc`；其余成员每次读时调用宿主。
//! 2. **JS 外观**：原生类 `ExtensionContext`（`#[rquickjs::class]`）承载全部状态，再套一层
//!    `js/context.js` 的 `makeContext(native, kind)`。TS 的 `createContext()` 返回对象字面量：
//!    成员是**自有可枚举**属性、四种形状只带各自的成员（base 上 `ctx.tools === undefined`）。
//!    JS 外观重建这个形状（`Object.keys`/展开/`typeof` 特性检测保持相同语义），每个 getter 读取
//!    时回调 [`ContextHost`]。
//!
//! # 与 TS 的已知差异（逐条）
//!
//! - TS 的 `createCommandContext()` 用 `Object.getOwnPropertyDescriptors` 保持 getter 惰性；
//!   Rust 天然是方法/trait 转发，无快照问题（`ca-extensions.md` §2.3.10 的注释）。
//! - 缺成员的调用错误文案是 QuickJS 的 `not a function`（V8 是 `<name> is not a function`）。
//! - `ctx.ui` 的 JS 值由 `ui-adapter`（`ui/mod.rs`）的 JS 桥提供：本单元把
//!   `Arc<dyn ExtensionUiContext>` 登记成数字句柄（[`register_ui_context`]），并调用 JS 全局
//!   `__kiana_ctx_make_ui(handle)`。桥未安装时：无 UI（no-op）返回与 `noOpUIContext` 等价的 JS
//!   对象；有 UI 则抛错（宁可显式失败，也不要静默吞掉对话框）。
//! - `ctx.signal` 是**真正的 AbortSignal**（原型是引擎的 `AbortSignal.prototype`，`instanceof`
//!   与 `AbortSignal.any()` 都成立）。宿主用 `CancellationToken`（`ARCHITECTURE.md` §4.2）表达
//!   信号，同一个 token 复用同一个 JS 对象；`aborted`/`reason` 读取时同步查询 token，监听器
//!   由 16 ms 轮询转发（只在存在 `abort` 监听器时运行，abort 或退订后立即停止）。
//! - 命令方法的 `withSession(ctx)` 通过回调桥执行：宿主**等待** JS 回调的 Promise 结束，
//!   回调抛错会让命令 Promise reject（`finishSessionReplacement` 的 `await withSession(...)`）。
//!   若宿主交回的 `ReplacedSessionContextApi` 是本单元构造的 `ExtensionContext`（正常路径），
//!   JS 值直接用它**自己的** `ContextHost`（新 runner 的 ui / modelRegistry / stale 检查）。
//! - `setup(sessionManager)` 同样被等待、失败中止命令；JS 侧拿到 `SessionManager` 的 15 个
//!   只读成员（可写桥属于 session 单元，见报告剩余缺口）。
//! - `compact(options)` 的 `onComplete`/`onError` 通过回调桥执行；TS 的 `compactFn` 是同步调用，
//!   这里经 host op 在引擎线程上执行（时序上晚一个任务，扩展通常不依赖该顺序）。回调表项在
//!   任一回调触发或宿主丢弃 options 时释放。
//! - `getSystemPromptOptions()` / `scopedModels` / `model` 在 JS 侧按 JSON 快照做 interning：
//!   宿主状态不变时返回同一个对象（引用身份与扩展对 `selectedTools` 的修改都保留），状态变化
//!   时刷新（读时求值）。
//! - `modelRegistry.stream`/`streamSimple` 返回 `for await` + `result()` 可用的 JS 事件流；
//!   `getProvider(id).stream`/`streamSimple` 复用同一事件流桥。
//! - `modelRegistry.registerProvider`（两个重载）/`registerVirtualModel` 复用 loader 的 JS
//!   provider 回调桥与 `kiana-config::ProviderConfigInput` 转换；`getRegisteredProviderConfig`
//!   物化数据字段（`streamSimple` 等函数字段无法从 Rust 结构逆向重建，见报告剩余缺口）。
//!
//! # 一次调用的执行流程
//!
//! **同步成员**（`ctx.mode`、`ctx.model`、`ctx.sessionManager` 等）：`js/context.js` 的外观
//! getter → 原生类的 `require_active`（或 JS 外观缓存型 getter 先调 `assertActive`）→
//! [`ContextHost`] 读取。JSON 形状的成员（`model`/`scopedModels`/`thinkingLevel`/
//! `getSystemPromptOptions`）经 `serde_json` 序列化、再 `JSON.parse` 回 JS；
//! `mode`/`cwd`/`hasUI` 等标量直接转换，`sessionManager`/`modelRegistry` 读时用
//! `Class::instance` 构造原生类实例。每次读取都重新询问宿主，因此没有快照；
//! stale 文案原样抛成 JS `Error`。
//!
//! **异步成员**（`executeTool`、`waitForIdle`、`newSession`、`fork`、`sendMessage`、
//! `modelRegistry.complete` 等）：原生方法把参数转成 JSON payload（JS 函数另登记成数字 id）
//! → `__kiana_ctx.invoke` → `__kiana.promise("kiana_ctx", …)` → `handle_context_call`
//! **在 JS 线程**解析 payload 并按 `op` 分派 → `tokio::task::spawn_local` 的宿主任务调用
//! [`ContextHost`]（或 `ModelRegistry`）→ 通过完成队列 `resolve`/`reject` 结算 JS Promise。
//!
//! # 回调桥与句柄
//!
//! JS 函数不能进 JSON payload（`JSON.stringify` 会丢弃函数），所以 `onUpdate`/`onComplete`/
//! `onError`/`withSession`/`setup` 先由 `js/context.js` 存表，只在 payload 里传数字 id；宿主
//! 需要触发时把请求投进 `CallbackBridge` 的通道，由引擎线程上的泵任务调用 JS，完成后用
//! `__kiana_ctx_callback_done(callId, ok, payload)` 唤醒等待中的 Rust future。
//!
//! 所有跨边界对象（上下文、模型注册表、UI、signal token、事件流）都经过本文件的进程级
//! 句柄表（`Mutex` 保护）。上下文与模型注册表用 `Weak`（JS 对象回收后句柄失效），UI 用
//! 强引用（扩展可以保留引用、在上下文销毁后继续使用），signal 以 `CancellationToken` 的
//! 克隆值保存。
//!
//! # 失败模式与边界
//!
//! - `assert_active()` 失败：同步方法（含命令上下文的 `waitForIdle`/`fork` 等）按 TS 的
//!   非 async 箭头语义**同步**抛 `Error`；`executeTool`/`sendMessage` 等 async 成员返回
//!   **rejected Promise**（`rejected_promise`）。
//! - 上下文句柄失效（JS 原生对象已被回收）：宿主任务返回 `context is no longer available`，
//!   并先释放 payload 里已登记的回调 id（`release_payload_callbacks`）。
//! - 未知 `op`：reject `unknown context operation: …` / `unknown model registry operation: …`。
//!   新增 JS 成员时必须同步在 `handle_context_call`/`spawn_context_op` 加分支，否则调用只在
//!   运行期失败。
//! - `withSession`/`setup` 的返回类型没有错误通道：回调抛错写进 `CallbackErrorSlot`，宿主
//!   动作返回后由 `spawn_context_op` 把命令 Promise 拒绝。
//!
//! # 阅读顺序
//!
//! 先看 [`ContextHost`] 与 [`ExtensionContext`]（Rust 视图），再看 [`JsExtensionContext`] 的
//! 原生方法与 `js/context.js` 的外观，最后看 `handle_context_call` 的分派与各 `spawn_*` 宿主任务。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use kiana_agent_core::{AgentTool, AgentToolCallOutcome, ThinkingLevel};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{
    Context, JsonObject, JsonValue, Model, ModelType, SimpleStreamOptions, TextContent, TextKind,
    ToolCall, ToolCallKind, UserContent,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::models::composer::ProviderConfigInput;
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::resolver::ScopedModel;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::system_prompt::BuildSystemPromptOptions;
use kiana_ext_api::types::{
    CompactOptions, ContextUsage, CustomMessagePayload, ExecuteToolOptions,
    ExtensionCommandContextApi, ExtensionContextApi, ExtensionMode, ExtensionToolContextApi,
    ForkOptions, NavigateTreeOptions, NewSessionOptions, NewSessionResult,
    ReplacedSessionContextApi, SendMessageOptions, SendUserMessageOptions, SwitchSessionOptions,
};
use kiana_ext_api::ui::ExtensionUiContext;
use kiana_session_core::session_manager::{ReadonlySessionManager, SessionEntry};
use rquickjs::class::Trace;
use rquickjs::function::{Opt, This};
use rquickjs::{Class, Ctx, Exception, Function, IntoJs, JsLifetime, Object, Value};
use tokio_util::sync::CancellationToken;

use crate::engine::{EngineResult, HostCall, HostValue, JsEngine};

// ===========================================================================
// 上下文种类
// ===========================================================================

/// 扩展上下文的种类（TS 的四种 `ctx` 形状）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextKind {
    /// `createContext()`（事件 handler 的默认上下文）。
    #[default]
    Base,
    /// `createToolContext()`：额外 `tools` / `executeTool`。
    Tool,
    /// `createCommandContext()`：额外 7 个会话控制方法。
    Command,
    /// `ReplacedSessionContext`：命令上下文 + `sendMessage`/`sendUserMessage`。
    Replaced,
}

impl ContextKind {
    /// 调试与日志用的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            ContextKind::Base => "base",
            ContextKind::Tool => "tool",
            ContextKind::Command => "command",
            ContextKind::Replaced => "replaced",
        }
    }

    /// 是否是命令上下文（`ReplacedSessionContext extends ExtensionCommandContext`）。
    pub fn is_command(self) -> bool {
        matches!(self, ContextKind::Command | ContextKind::Replaced)
    }
}

/// `assert_active()` 的错误（TS 的 `throw new Error(this.staleMessage)`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextError {
    message: String,
}

impl ContextError {
    /// 用 stale 文案构造。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 错误文本（逐字进入 JS `Error.message`）。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ContextError {}

// ===========================================================================
// 宿主契约
// ===========================================================================

/// 上下文读取的宿主（`runner-core` 为 `ExtensionRunner` 实现）。
///
/// 每个方法对应 TS 的一个字段/getter；实现方负责 `assert_active` 的 stale 判定（TS 的
/// `runner.assertActive()`）。`Send + Sync`：`kiana-session` 通过 `Arc<Self>` 共享 runner，原生类
/// 也持有 `Arc<dyn ContextHost>`。
pub trait ContextHost: Send + Sync {
    /// stale 检查（`invalidate()` 之后所有访问抛同一文案）。
    fn assert_active(&self) -> Result<(), ContextError>;
    /// 当前（已被 UI prompt 包装的）UI 上下文。TS: `runner.uiContext`。
    fn ui(&self) -> Arc<dyn ExtensionUiContext>;
    /// 当前模式。TS: `runner.mode`。
    fn mode(&self) -> ExtensionMode;
    /// 是否有对话框 UI。TS: `runner.hasUI()`。
    ///
    /// 判定方式由实现方决定：`runner` 的 `RunnerHost` 以当前 UI 上下文是否为 no-op 判定。
    fn has_ui(&self) -> bool;
    /// 工作目录。TS: `runner.cwd`。
    fn cwd(&self) -> PathBuf;
    /// 只读会话管理器。TS: `runner.sessionManager`。
    fn session_manager(&self) -> Arc<dyn ReadonlySessionManager>;
    /// 模型注册表。TS: `runner.modelRegistry`。
    fn model_registry(&self) -> Arc<ModelRegistry>;
    /// 当前模型。TS: `getModel()`。
    fn model(&self) -> Option<Model>;
    /// 会话作用域模型。TS: `getScopedModels()`。
    fn scoped_models(&self) -> Vec<ScopedModel>;
    /// 当前 thinking level。TS: `runtime.getThinkingLevel()`。
    fn thinking_level(&self) -> Option<ThinkingLevel>;
    /// agent 是否空闲。TS: `isIdleFn()`。
    fn is_idle(&self) -> bool;
    /// 项目本地信任。TS: `isProjectTrustedFn()`。
    fn is_project_trusted(&self) -> bool;
    /// 当前 abort signal。TS: `getSignalFn()`。
    fn signal(&self) -> Option<CancellationToken>;
    /// 中止当前 agent 操作。TS: `abortFn()`。
    fn abort(&self);
    /// 是否有排队消息。TS: `hasPendingMessagesFn()`。
    fn has_pending_messages(&self) -> bool;
    /// 优雅退出。TS: `shutdownHandler()`。
    fn shutdown(&self);
    /// 上下文用量。TS: `getContextUsageFn()`。
    fn get_context_usage(&self) -> Option<ContextUsage>;
    /// 触发压缩（不等待）。TS: `compactFn(options)`。
    fn compact(&self, options: Option<CompactOptions>);
    /// 当前系统提示。TS: `getSystemPromptFn()`。
    fn get_system_prompt(&self) -> String;
    /// 系统提示构造选项（命令上下文）。TS: `getSystemPromptOptionsFn()`。
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions;
    /// 等待 agent 空闲（命令上下文）。TS: `waitForIdleFn()`。
    fn wait_for_idle(&self) -> LocalBoxFuture<'static, ()>;
    /// 开始新会话。TS: `newSessionHandler(options)`。
    fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult>;
    /// fork。TS: `forkHandler(entryId, options)`。
    fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult>;
    /// 导航会话树。TS: `navigateTreeHandler(targetId, options)`。
    fn navigate_tree(
        &self,
        target_id: String,
        options: Option<NavigateTreeOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult>;
    /// 切换会话。TS: `switchSessionHandler(sessionPath, options)`。
    fn switch_session(
        &self,
        session_path: String,
        options: Option<SwitchSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult>;
    /// 重新加载。TS: `reloadHandler()`。
    fn reload(&self) -> LocalBoxFuture<'static, ()>;
    /// `executeTool` 可调用的工具。TS: `getCallableToolsFn()`。
    fn get_callable_tools(&self) -> Vec<AgentTool>;
    /// 运行嵌套工具调用。TS: `executeToolFn`；`None` 表示没有绑定动作
    /// （`createToolContext` 的回退结果）。
    fn execute_tool(
        &self,
        tool_call_id: &str,
        name: &str,
        args: JsonValue,
        options: ExecuteToolOptions,
    ) -> Option<kiana_ext_api::BoxFuture<'static, AgentToolCallOutcome>>;
}

/// `ReplacedSessionContext` 的 `sendMessage`/`sendUserMessage` 落点。
///
/// TS 里它们由**会话替换后的新** `AgentSession` 提供（`agent-session.ts#createReplacedSessionContext`），
/// 与 `kiana.sendMessage`（同步、fire-and-forget）同名不同义：这里返回 `Promise<void>`
/// （`LocalBoxFuture`）。`runner-core`/`kiana-session` 负责在替换后构造它。
pub trait ReplacedSessionActions: Send + Sync {
    /// `sendCustomMessage(message, options)`（Promise）。
    fn send_message(
        &self,
        message: SendMessageOptions,
    ) -> LocalBoxFuture<'static, Result<(), String>>;
    /// `sendUserMessage(content, options)`（Promise）。
    fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> LocalBoxFuture<'static, Result<(), String>>;
}

// ===========================================================================
// Rust 视图：ExtensionContext
// ===========================================================================

/// `ExtensionContext` 的 Rust 形态（读时求值；`Send + Sync`）。
///
/// 由 [`ExtensionContext::base`] / [`ExtensionContext::tool`] / [`ExtensionContext::command`] /
/// [`ExtensionContext::replaced`] 构造；`runner-core` 的 `create_context()` 返回
/// `Arc<dyn ExtensionContextApi>` 时直接使用它。
///
/// 所有 trait 成员在访问前调用 `ensure_active`：宿主报告 stale 时以 **panic** 表达 TS 的
/// `throw`（这些 trait 方法没有错误通道）；JS 外观走原生类的 `require_active`，把同一文案
/// 抛成 JS `Error` / rejected Promise。
pub struct ExtensionContext {
    host: Arc<dyn ContextHost>,
    kind: ContextKind,
    tool_call_id: Option<String>,
    signal: Option<CancellationToken>,
    replaced: Option<Arc<dyn ReplacedSessionActions>>,
    // 构造期固定的 runner 字段（见模块文档的已知差异）。
    cwd: PathBuf,
    session_manager: Arc<dyn ReadonlySessionManager>,
    model_registry: Arc<ModelRegistry>,
    ui: Arc<dyn ExtensionUiContext>,
}

impl std::fmt::Debug for ExtensionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionContext")
            .field("kind", &self.kind)
            .field("tool_call_id", &self.tool_call_id)
            .finish_non_exhaustive()
    }
}

// 只有 `Replaced` 上下文会按对象地址登记进 `REPLACED_CONTEXTS`（见 `build`），因此释放时
// 只需摘除该条目；条目里的 `Weak` 升级失败或条目不存在时，`withSession` 退回兜底宿主。
impl Drop for ExtensionContext {
    fn drop(&mut self) {
        if self.kind == ContextKind::Replaced {
            let key = self as *const Self as *const () as usize;
            lock(&REPLACED_CONTEXTS).remove(&key);
        }
    }
}

// ===========================================================================
// 会话替换后的新上下文登记表
// ===========================================================================
//
// `WithSessionFn` 只拿得到 `Arc<dyn ReplacedSessionContextApi>`（trait 对象），而
// `ReplacedSessionContextApi`（`kiana-ext-api` 冻结）没有暴露 `ui`/`modelRegistry` 的
// `Arc` 或 stale 检查。TS 里 withSession 收到的是**新 runner** 的 `createCommandContext()`；
// 本单元在构造 `ContextKind::Replaced` 时按对象地址登记一个 Weak，宿主回调里再取回来，
// 就能把新上下文的 `ContextHost`（ui / modelRegistry / assertActive）原样交给 JS。
// 未登记（例如其它实现方自建了 `ReplacedSessionContextApi`）时退回旧快照行为。
//
// 键取 `Arc::as_ptr` 的 data 指针。键一致的前提是：宿主交回的正是本单元构造的
// `Arc<ExtensionContext>`（单元测试即此模型），此时 `Arc<dyn ReplacedSessionContextApi>`
// 由它强制转换而来，data 指针不变，登记键与 `with_session_callback` 的查找键一致。
// 宿主若再包装（如 kiana-session 把 `ExtensionContext` 包进自己的 `SessionReplacedContext`
// 再交给 `WithSessionFn`），数据指针就是外层分配的地址，查找必然落空，走上一段的兜底路径。
// `Weak` 保证上下文释放后不会误命中；同一地址被新对象复用时由新登记覆盖。

static REPLACED_CONTEXTS: LazyLock<
    Mutex<std::collections::HashMap<usize, Weak<ExtensionContext>>>,
> = LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn context_key(context: &Arc<ExtensionContext>) -> usize {
    Arc::as_ptr(context) as *const () as usize
}

fn register_replaced_context(context: &Arc<ExtensionContext>) {
    lock(&REPLACED_CONTEXTS).insert(context_key(context), Arc::downgrade(context));
}

fn resolve_replaced_context(key: usize) -> Option<Arc<ExtensionContext>> {
    lock(&REPLACED_CONTEXTS).get(&key)?.upgrade()
}

impl ExtensionContext {
    fn build(
        host: Arc<dyn ContextHost>,
        kind: ContextKind,
        tool_call_id: Option<String>,
        signal: Option<CancellationToken>,
        replaced: Option<Arc<dyn ReplacedSessionActions>>,
    ) -> Arc<Self> {
        // `ExtensionRunner` 的 cwd/sessionManager/modelRegistry 是构造期字段（TS 同名字段只在
        // 构造函数里赋值），所以这里捕获 `Arc` 与 TS 的引用语义一致；`ui` 在 TS 里可由
        // `setUIContext()` 替换，但 `ExtensionContextApi::ui()` 返回 `&dyn`，无法返回临时
        // `Arc` 的内部引用，因此同样在创建时捕获（每次 dispatch 都新建 ctx，影响面最小）。
        // `Replaced` 上下文还要登记地址供 `withSession` 桥取回；登记只存 `Weak`，生命周期
        // 仍由调用方持有的 `Arc` 决定。
        let cwd = host.cwd();
        let session_manager = host.session_manager();
        let model_registry = host.model_registry();
        let ui = host.ui();
        let context = Arc::new(Self {
            host,
            kind,
            tool_call_id,
            signal,
            replaced,
            cwd,
            session_manager,
            model_registry,
            ui,
        });
        if kind == ContextKind::Replaced {
            register_replaced_context(&context);
        }
        context
    }

    /// base 上下文（事件 handler）。
    pub fn base(host: Arc<dyn ContextHost>) -> Arc<Self> {
        Self::build(host, ContextKind::Base, None, None, None)
    }

    /// 工具执行上下文（`toolCallId` + 嵌套调用的默认 signal）。
    pub fn tool(
        host: Arc<dyn ContextHost>,
        tool_call_id: &str,
        signal: Option<CancellationToken>,
    ) -> Arc<Self> {
        Self::build(
            host,
            ContextKind::Tool,
            Some(tool_call_id.to_owned()),
            signal,
            None,
        )
    }

    /// 命令上下文。
    pub fn command(host: Arc<dyn ContextHost>) -> Arc<Self> {
        Self::build(host, ContextKind::Command, None, None, None)
    }

    /// 会话替换后的命令上下文（额外 `sendMessage`/`sendUserMessage`）。
    pub fn replaced(
        host: Arc<dyn ContextHost>,
        actions: Arc<dyn ReplacedSessionActions>,
    ) -> Arc<Self> {
        Self::build(host, ContextKind::Replaced, None, None, Some(actions))
    }

    /// 宿主。
    pub fn host(&self) -> &Arc<dyn ContextHost> {
        &self.host
    }

    /// 种类。
    pub fn kind(&self) -> ContextKind {
        self.kind
    }

    /// `toolCallId`（仅 tool ctx）。
    pub fn tool_call_id(&self) -> Option<&str> {
        self.tool_call_id.as_deref()
    }

    /// 嵌套调用的默认 signal（仅 tool ctx）。
    pub fn default_signal(&self) -> Option<CancellationToken> {
        self.signal.clone()
    }

    /// 会话替换后的动作（仅 replaced ctx）。
    pub fn replaced_actions(&self) -> Option<&Arc<dyn ReplacedSessionActions>> {
        self.replaced.as_ref()
    }

    /// `assertActive()`；Rust 侧没有异常，stale 访问按 panic 处理（与 TS 的 throw 对应）。
    fn ensure_active(&self) {
        if let Err(error) = self.host.assert_active() {
            panic!("{}", error.message());
        }
    }
}

impl ExtensionContextApi for ExtensionContext {
    fn ui(&self) -> &dyn ExtensionUiContext {
        self.ensure_active();
        &*self.ui
    }

    fn mode(&self) -> ExtensionMode {
        self.ensure_active();
        self.host.mode()
    }

    fn has_ui(&self) -> bool {
        self.ensure_active();
        self.host.has_ui()
    }

    fn cwd(&self) -> &Path {
        self.ensure_active();
        &self.cwd
    }

    fn session_manager(&self) -> &dyn ReadonlySessionManager {
        self.ensure_active();
        &*self.session_manager
    }

    fn model_registry(&self) -> &ModelRegistry {
        self.ensure_active();
        &self.model_registry
    }

    fn model(&self) -> Option<Model> {
        self.ensure_active();
        self.host.model()
    }

    fn scoped_models(&self) -> Vec<ScopedModel> {
        self.ensure_active();
        self.host.scoped_models()
    }

    fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.ensure_active();
        self.host.thinking_level()
    }

    fn is_idle(&self) -> bool {
        self.ensure_active();
        self.host.is_idle()
    }

    fn is_project_trusted(&self) -> bool {
        self.ensure_active();
        self.host.is_project_trusted()
    }

    fn signal(&self) -> Option<CancellationToken> {
        self.ensure_active();
        self.host.signal()
    }

    fn abort(&self) {
        self.ensure_active();
        self.host.abort();
    }

    fn has_pending_messages(&self) -> bool {
        self.ensure_active();
        self.host.has_pending_messages()
    }

    fn shutdown(&self) {
        self.ensure_active();
        self.host.shutdown();
    }

    fn get_context_usage(&self) -> Option<ContextUsage> {
        self.ensure_active();
        self.host.get_context_usage()
    }

    fn compact(&self, options: Option<CompactOptions>) {
        self.ensure_active();
        self.host.compact(options);
    }

    fn get_system_prompt(&self) -> String {
        self.ensure_active();
        self.host.get_system_prompt()
    }
}

impl ExtensionToolContextApi for ExtensionContext {
    fn tools(&self) -> Vec<AgentTool> {
        self.ensure_active();
        if self.kind != ContextKind::Tool {
            panic!("ctx.tools is only available on a tool context");
        }
        self.host.get_callable_tools()
    }

    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        args: JsonValue,
        options: Option<ExecuteToolOptions>,
    ) -> kiana_ext_api::BoxFuture<'a, AgentToolCallOutcome> {
        self.ensure_active();
        let tool_call_id = self.tool_call_id.clone().unwrap_or_default();
        let options = merge_execute_tool_options(options, self.signal.clone());
        match self
            .host
            .execute_tool(&tool_call_id, name, args.clone(), options)
        {
            Some(future) => future,
            None => Box::pin(async move { nested_call_fallback(&tool_call_id, name, args) }),
        }
    }
}

impl ExtensionCommandContextApi for ExtensionContext {
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        self.ensure_active();
        self.host.get_system_prompt_options()
    }

    fn wait_for_idle(&self) -> LocalBoxFuture<'static, ()> {
        self.ensure_active();
        self.host.wait_for_idle()
    }

    fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        self.ensure_active();
        self.host.new_session(options)
    }

    fn fork<'a>(
        &'a self,
        entry_id: &'a str,
        options: Option<ForkOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult> {
        self.ensure_active();
        self.host.fork(entry_id.to_owned(), options)
    }

    fn navigate_tree<'a>(
        &'a self,
        target_id: &'a str,
        options: Option<NavigateTreeOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult> {
        self.ensure_active();
        self.host.navigate_tree(target_id.to_owned(), options)
    }

    fn switch_session<'a>(
        &'a self,
        session_path: &'a str,
        options: Option<SwitchSessionOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult> {
        self.ensure_active();
        self.host.switch_session(session_path.to_owned(), options)
    }

    fn reload(&self) -> LocalBoxFuture<'static, ()> {
        self.ensure_active();
        self.host.reload()
    }
}

impl ReplacedSessionContextApi for ExtensionContext {
    fn send_message(&self, message: SendMessageOptions) -> LocalBoxFuture<'static, ()> {
        let future = self.send_message_checked(message);
        Box::pin(async move {
            let _ = future.await;
        })
    }

    fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> LocalBoxFuture<'static, ()> {
        let future = self.send_user_message_checked(content, options);
        Box::pin(async move {
            let _ = future.await;
        })
    }

    fn send_message_checked(
        &self,
        message: SendMessageOptions,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        self.ensure_active();
        let Some(actions) = self.replaced.clone() else {
            panic!("ctx.sendMessage is only available on a replaced session context");
        };
        actions.send_message(message)
    }

    fn send_user_message_checked(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        self.ensure_active();
        let Some(actions) = self.replaced.clone() else {
            panic!("ctx.sendUserMessage is only available on a replaced session context");
        };
        actions.send_user_message(content, options)
    }
}

/// `options.signal` 缺省时继承调用工具的 signal（TS: `options.signal ?? signal`）。
fn merge_execute_tool_options(
    options: Option<ExecuteToolOptions>,
    default_signal: Option<CancellationToken>,
) -> ExecuteToolOptions {
    match options {
        Some(mut options) => {
            if options.signal.is_none() {
                options.signal = default_signal;
            }
            options
        }
        None => ExecuteToolOptions {
            signal: default_signal,
            on_update: None,
        },
    }
}

/// 没有绑定 `executeToolFn` 时的回退结果（`ca-extensions.md` §2.3.10，逐字）。
///
/// 结果是一个 `is_error: true` 的固定 outcome：`tool_call.id` 为 `"{tool_call_id}/0"`，
/// `result.content` 只有一句 `Nested tool calls are not available in this context`，
/// `arguments` 为空对象。
pub fn nested_call_fallback(
    tool_call_id: &str,
    name: &str,
    _args: JsonValue,
) -> AgentToolCallOutcome {
    AgentToolCallOutcome {
        tool_call: ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: format!("{tool_call_id}/0"),
            name: name.to_owned(),
            arguments: JsonObject::new(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        },
        result: kiana_agent_core::AgentToolResult {
            content: vec![kiana_ai::types::UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "Nested tool calls are not available in this context".to_owned(),
                text_signature: None,
            })],
            details: object_value(JsonObject::new()),
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        },
        is_error: true,
        duration_ms: None,
    }
}

// ===========================================================================
// 句柄注册表
// ===========================================================================
//
// JS 侧的对象是 id-keyed 的（`design-extension-runtime.md` §5.8 的线程约束）：原生类实例保存
// 数字句柄，异步方法把句柄放进 `__kiana.promise("kiana_ctx", {...})` 的 payload，宿主任务再用
// [`resolve_context`] 取回 Rust 对象。句柄在原生类实例 `Drop`（JS GC/finalizer）时注销。

/// 对象句柄与事件流 id 共用的递增序号（从 1 开始，进程内唯一）。
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// 句柄表的条目。
enum RegisteredObject {
    /// 扩展上下文（Weak：JS 对象销毁后句柄失效）。
    Context(Weak<ExtensionContext>),
    /// 模型注册表（`ctx.modelRegistry` 的异步方法）。
    ModelRegistry(Weak<ModelRegistry>),
    /// `withSession` 收到的新会话上下文（`Arc`：宿主交给我们后由 JS 使用）。
    ///
    /// 取出即删除：`__kiana_ctx_make_replaced` 创建 JS 上下文后不再需要。
    Replaced {
        context: Arc<dyn ReplacedSessionContextApi>,
        ui: Arc<dyn ExtensionUiContext>,
        model_registry: Arc<ModelRegistry>,
    },
    /// `newSession({ setup })` 收到的新会话管理器（可写；JS 只读 15 成员）。
    ///
    /// 取出即删除：`__kiana_ctx_make_session_manager` 创建 JS 对象后不再需要。
    SetupSession {
        manager: Arc<Mutex<kiana_session_core::session_manager::SessionManager>>,
    },
}

/// 对象句柄表：`Context`/`ModelRegistry` 存 `Weak`，`Replaced`/`SetupSession` 在取用时删除。
static OBJECTS: LazyLock<Mutex<std::collections::HashMap<u64, RegisteredObject>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn register_object(object: RegisteredObject) -> u64 {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
    lock(&OBJECTS).insert(handle, object);
    handle
}

fn unregister_object(handle: u64) {
    lock(&OBJECTS).remove(&handle);
}

/// 句柄 → 扩展上下文（`Weak` 升级失败表示 JS 对象已经销毁）。
pub fn resolve_context(handle: u64) -> Option<Arc<ExtensionContext>> {
    match lock(&OBJECTS).get(&handle) {
        Some(RegisteredObject::Context(weak)) => weak.upgrade(),
        _ => None,
    }
}

/// 句柄 → 模型注册表。
pub fn resolve_model_registry(handle: u64) -> Option<Arc<ModelRegistry>> {
    match lock(&OBJECTS).get(&handle) {
        Some(RegisteredObject::ModelRegistry(weak)) => weak.upgrade(),
        _ => None,
    }
}

/// `Arc<dyn ExtensionUiContext>` 的句柄表。
///
/// 按 `Arc` 指针去重：同一个 UI 上下文永远得到同一个句柄（JS 桥按句柄缓存对象身份）。
/// 条目持有**强引用**：扩展可以 `const ui = ctx.ui` 后在 ctx 销毁之后继续使用它（TS 的
/// 引用语义），因此不能在上下文的 `Drop` 里释放。
///
/// 注意：表没有移除路径，登记的 UI 上下文会一直保留到进程结束；句柄按 `Arc` 指针去重，
/// 所以同一 UI 重复读取不会继续增长。
#[derive(Default)]
struct UiHandles {
    next: u64,
    entries: Vec<(usize, u64, Arc<dyn ExtensionUiContext>)>,
}

static UI_HANDLES: LazyLock<Mutex<UiHandles>> = LazyLock::new(|| Mutex::new(UiHandles::default()));

/// 登记一个 UI 上下文，返回稳定句柄（`ui-adapter` 的 JS 桥用 [`resolve_ui_context`] 取回）。
pub fn register_ui_context(ui: Arc<dyn ExtensionUiContext>) -> u64 {
    let pointer = Arc::as_ptr(&ui) as *const () as usize;
    let mut handles = lock(&UI_HANDLES);
    if let Some((_, handle, _)) = handles.entries.iter().find(|(ptr, _, _)| *ptr == pointer) {
        return *handle;
    }
    handles.next += 1;
    let handle = handles.next;
    handles.entries.push((pointer, handle, ui));
    handle
}

/// 句柄 → UI 上下文。
pub fn resolve_ui_context(handle: u64) -> Option<Arc<dyn ExtensionUiContext>> {
    lock(&UI_HANDLES)
        .entries
        .iter()
        .find(|(_, id, _)| *id == handle)
        .map(|(_, _, ui)| Arc::clone(ui))
}

/// `CancellationToken` 的句柄表（`ctx.signal` 的 JS 对象按 token 去重）。
#[derive(Default)]
struct SignalHandles {
    next: u64,
    ids: std::collections::HashMap<CancellationToken, u64>,
    tokens: std::collections::HashMap<u64, CancellationToken>,
}

static SIGNAL_HANDLES: LazyLock<Mutex<SignalHandles>> =
    LazyLock::new(|| Mutex::new(SignalHandles::default()));

/// 登记（或复用）一个 signal token 的句柄。
pub fn register_signal_handle(token: &CancellationToken) -> u64 {
    let mut handles = lock(&SIGNAL_HANDLES);
    if let Some(handle) = handles.ids.get(token) {
        return *handle;
    }
    handles.next += 1;
    let handle = handles.next;
    handles.ids.insert(token.clone(), handle);
    handles.tokens.insert(handle, token.clone());
    handle
}

/// 句柄 → signal token。
pub fn resolve_signal_handle(handle: u64) -> Option<CancellationToken> {
    lock(&SIGNAL_HANDLES).tokens.get(&handle).cloned()
}

/// 句柄对应的 token 是否已取消（`__kiana_ctx_signal_aborted` 的宿主原语）。
pub fn signal_handle_aborted(handle: u64) -> bool {
    resolve_signal_handle(handle).is_some_and(|token| token.is_cancelled())
}

// ===========================================================================
// `ctx.modelRegistry.stream` / `streamSimple` 的事件流
// ===========================================================================
//
// TS 返回 `AssistantMessageEventStream`（`for await` + `result()`）。Rust 侧持有真实的
// `AssistantMessageEventStream`，JS 外观按 id 逐个拉取事件（`streamNext`），并等待最终消息
// （`streamResult`）。`next` 用一把 tokio Mutex 串行化，避免并发拉取打乱顺序。
//
// 表项由 `modelRegistry.streamClose` 移除；`js/context.js` 的流外观在 `for await` 提前退出
// （`return()`）以及迭代读到 `done` 之后调用关闭。只调用 `result()` 而不迭代、或两者都不
// 调用的消费者不会触发关闭，表项会一直保留到进程结束。

/// 一条 JS 可见的流。
struct JsStreamSlot {
    stream: AssistantMessageEventStream,
    next: tokio::sync::Mutex<()>,
}

static STREAMS: LazyLock<Mutex<std::collections::HashMap<u64, Arc<JsStreamSlot>>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn register_stream(stream: AssistantMessageEventStream) -> u64 {
    let id = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
    lock(&STREAMS).insert(
        id,
        Arc::new(JsStreamSlot {
            stream,
            next: tokio::sync::Mutex::new(()),
        }),
    );
    id
}

fn resolve_stream(id: u64) -> Option<Arc<JsStreamSlot>> {
    lock(&STREAMS).get(&id).cloned()
}

fn unregister_stream(id: u64) {
    lock(&STREAMS).remove(&id);
}

// ===========================================================================
// 宿主 → JS 回调桥（onUpdate / onComplete / onError / withSession）
// ===========================================================================
//
// JS 函数不能进 JSON payload（`JSON.stringify` 丢掉函数），因此原生方法在 JS 线程上把
// 回调登记到 `js/context.js` 的表里，只把数字 id 传给宿主任务。宿主任务需要调用回调时
// 把请求投进本桥；桥的 `spawn_local` 泵任务（引擎线程）通过完成队列触发
// `__kiana_ctx.applyCallback(id, args, callId)`。JS 侧调用完成后用
// `__kiana_ctx_callback_done(callId, ok, payload)` 唤醒等待中的 Rust future。
//
// `HostCallback`（`engine::promise`）负责 runtime 锚与“先释放 JS 值、再释放 runtime”的
// 关闭顺序；泵任务只在引擎的 LocalSet 里运行，因此 `Persistent` 不会被跨线程使用。

/// 一次回调请求。
enum CallbackCommand {
    /// 调用 JS 回调（`reply` 为 `Some` 时等待它的结果）。
    Invoke {
        callback_id: u64,
        args: JsonValue,
        call_id: u64,
        ctx_handle: u64,
        /// 回调抛错时是否额外 `kiana.recordError("host", error)`。
        ///
        /// `withSession`/`setup` 的拒绝会转成命令 Promise 的 rejection（TS 的
        /// `await withSession(...)` 语义），不需要再记一条扩展错误；`onUpdate` 等
        /// fire-and-forget 回调没有别的地方体现错误，仍然记录。
        record_error: bool,
        reply: Option<tokio::sync::oneshot::Sender<Result<JsonValue, String>>>,
    },
    /// 释放 JS 侧的回调表项（不再调用）。
    Release { callback_id: u64 },
}

/// 宿主任务手里的回调桥（`Send + Sync`；函数本体留在 JS 线程的泵任务里）。
#[derive(Clone)]
struct CallbackBridge {
    tx: tokio::sync::mpsc::UnboundedSender<CallbackCommand>,
}

/// 全局回调调用序号。
///
/// `CALLBACK_WAITERS` 是进程级表，`__kiana_ctx_callback_done(callId, …)` 只带这个数字；
/// 若每个桥各自从 1 开始，两个桥并发调用时会互相消费等待者（错投 payload 或超时）。
static NEXT_CALL_ID: AtomicU64 = AtomicU64::new(1);

impl CallbackBridge {
    /// 在当前 JS 事务里启动泵任务；`__kiana_ctx` 未安装时 `get` 失败、错误原样返回。
    ///
    /// 正常装配顺序下 `install` 已先求值 `js/context.js`，因此实际总是返回 `Some`（返回
    /// 类型里的 `Option` 是历史形状）。两个 JS 函数用 `completion_sender().capture` 固定成
    /// “先释放 JS 值、再释放 runtime”的顺序；泵任务随桥的最后一个克隆释放而结束。
    fn start<'js>(
        ctx: &Ctx<'js>,
        sender: &crate::engine::HostCompletionSender,
    ) -> EngineResult<Option<Self>> {
        let kiana: Object<'js> = ctx.globals().get("__kiana_ctx")?;
        let apply: Function<'js> = kiana.get("applyCallback")?;
        let release: Function<'js> = kiana.get("releaseCallback")?;
        let apply = sender.capture(apply);
        let release = sender.capture(release);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CallbackCommand>();
        tokio::task::spawn_local(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    CallbackCommand::Invoke {
                        callback_id,
                        args,
                        call_id,
                        ctx_handle,
                        record_error,
                        reply,
                    } => {
                        // 先登记等待者，再调用 JS：`applyCallback` 在回调同步抛出（或返回
                        // 已决 Promise）时会**同步**调用 `__kiana_ctx_callback_done`，之后
                        // 再登记就永远等不到结果。
                        if let Some(reply) = reply {
                            lock(&CALLBACK_WAITERS).insert(call_id, reply);
                        }
                        let sent = apply.invoke(vec![
                            HostValue::Number(callback_id as f64),
                            HostValue::Json(args),
                            HostValue::Number(call_id as f64),
                            HostValue::Number(ctx_handle as f64),
                            HostValue::Bool(record_error),
                        ]);
                        if !sent && let Some(reply) = lock(&CALLBACK_WAITERS).remove(&call_id) {
                            let _ = reply.send(Err("extension runtime is shut down".to_owned()));
                        }
                    }
                    CallbackCommand::Release { callback_id } => {
                        release.invoke(vec![HostValue::Number(callback_id as f64)]);
                    }
                }
            }
        });
        Ok(Some(Self { tx }))
    }

    /// 触发回调（不等待结果）。
    ///
    /// `call_id` 固定为 0、`record_error = true`：JS 侧完成后调用
    /// `__kiana_ctx_callback_done(0, …)` 找不到等待者（正常现象）；回调抛错则记入
    /// `kiana.recordError("host", …)`（这类 fire-and-forget 回调没有别的错误出口）。
    fn fire(&self, callback_id: u64, args: JsonValue) {
        let _ = self.tx.send(CallbackCommand::Invoke {
            callback_id,
            args,
            call_id: 0,
            ctx_handle: 0,
            record_error: true,
            reply: None,
        });
    }

    /// 调用回调并等待它（及其返回的 Promise）结束。
    ///
    /// `call_id` 由进程级 `NEXT_CALL_ID` 分配（等待表是全局的）；等待者由泵任务在调用 JS
    /// **之前**登记，以覆盖“同步抛出/已决 Promise 会同步回信”的路径。
    fn call(
        &self,
        callback_id: u64,
        args: JsonValue,
        ctx_handle: u64,
    ) -> LocalBoxFuture<'static, Result<JsonValue, String>> {
        let (reply, wait) = tokio::sync::oneshot::channel();
        let call_id = NEXT_CALL_ID.fetch_add(1, Ordering::SeqCst);
        let sent = self
            .tx
            .send(CallbackCommand::Invoke {
                callback_id,
                args,
                call_id,
                ctx_handle,
                record_error: false,
                reply: Some(reply),
            })
            .is_ok();
        Box::pin(async move {
            if !sent {
                return Err("extension runtime is shut down".to_owned());
            }
            // future 被丢弃（宿主任务取消）时把等待者从全局表里摘掉，避免 JS 永远不回信
            // 时泄漏 sender；正常路径上 `callback_done` 已经先移除了它。
            let _guard = CallbackWaiterGuard(call_id);
            match wait.await {
                Ok(result) => result,
                Err(_) => Err("callback was cancelled".to_owned()),
            }
        })
    }

    /// 释放 JS 回调表项。
    fn release(&self, callback_id: u64) {
        if callback_id != 0 {
            let _ = self.tx.send(CallbackCommand::Release { callback_id });
        }
    }
}

/// `compact` 的 `onComplete`/`onError` 释放守卫。
///
/// 两个回调共享一份 id；任一被调用即释放两者。宿主丢弃 options（永不回调）时 `Drop`
/// 释放，避免 JS 函数永久留在回调表里（TS 靠 GC 回收这些闭包）。
struct CompactCallbacks {
    bridge: Option<CallbackBridge>,
    ids: Mutex<(u64, u64)>,
}

impl CompactCallbacks {
    fn new(bridge: Option<CallbackBridge>, complete: u64, error: u64) -> Arc<Self> {
        Arc::new(Self {
            bridge,
            ids: Mutex::new((complete, error)),
        })
    }

    fn release(&self) {
        let (complete, error) = std::mem::take(&mut *lock(&self.ids));
        if let Some(bridge) = &self.bridge {
            bridge.release(complete);
            bridge.release(error);
        }
    }
}

impl Drop for CompactCallbacks {
    fn drop(&mut self) {
        self.release();
    }
}

/// `__kiana_ctx_callback_done` 的等待者。
type CallbackWaiter = tokio::sync::oneshot::Sender<Result<JsonValue, String>>;

/// 待回调的 `__kiana_ctx_callback_done` 等待者。
static CALLBACK_WAITERS: LazyLock<Mutex<std::collections::HashMap<u64, CallbackWaiter>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 等待者守卫：future 被丢弃时摘掉全局表中的 sender。
struct CallbackWaiterGuard(u64);

impl Drop for CallbackWaiterGuard {
    fn drop(&mut self) {
        lock(&CALLBACK_WAITERS).remove(&self.0);
    }
}

/// `__kiana_ctx_callback_done(callId, ok, payload)` 的宿主原语（在 JS 线程上同步执行）。
fn callback_done(call_id: u64, ok: bool, payload: String) {
    let Some(sender) = lock(&CALLBACK_WAITERS).remove(&call_id) else {
        return;
    };
    let value = if ok {
        match serde_json::from_str::<JsonValue>(&payload) {
            Ok(value) => Ok(value),
            Err(error) => Err(format!("invalid callback result: {error}")),
        }
    } else {
        Err(payload)
    };
    let _ = sender.send(value);
}

// ===========================================================================
// `withSession` 的新会话上下文
// ===========================================================================

/// `Arc<dyn ReplacedSessionContextApi>` → `Arc<dyn ReadonlySessionManager>` 的适配
/// （trait 方法返回 `&dyn`，而 JS 桥需要拥有型句柄）。
struct SessionVia(Arc<dyn ReplacedSessionContextApi>);

impl ReadonlySessionManager for SessionVia {
    fn get_cwd(&self) -> &str {
        self.0.session_manager().get_cwd()
    }
    fn get_session_dir(&self) -> &str {
        self.0.session_manager().get_session_dir()
    }
    fn get_session_id(&self) -> &str {
        self.0.session_manager().get_session_id()
    }
    fn get_session_file(&self) -> Option<&str> {
        self.0.session_manager().get_session_file()
    }
    fn get_leaf_id(&self) -> Option<&str> {
        self.0.session_manager().get_leaf_id()
    }
    fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        self.0.session_manager().get_leaf_entry()
    }
    fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.0.session_manager().get_entry(id)
    }
    fn get_label(&self, id: &str) -> Option<&str> {
        self.0.session_manager().get_label(id)
    }
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        self.0.session_manager().get_branch(from_id)
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        self.0.session_manager().build_context_entries()
    }
    fn build_session_projection(&self) -> kiana_session_core::session_manager::SessionProjection {
        self.0.session_manager().build_session_projection()
    }
    fn get_header(&self) -> Option<&kiana_session_core::session_manager::SessionHeader> {
        self.0.session_manager().get_header()
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.0.session_manager().get_entries()
    }
    fn get_tree(&self) -> Vec<kiana_session_core::session_manager::SessionTreeNode> {
        self.0.session_manager().get_tree()
    }
    fn get_session_name(&self) -> Option<String> {
        self.0.session_manager().get_session_name()
    }
}

/// 用宿主交给 `withSession` 的新会话上下文实现 [`ContextHost`]（只用于 JS 外观）。
///
/// 正常路径下 `withSession` 拿到的是本单元构造的 `ExtensionContext`，它会直接用自己的
/// host（新 runner 的 ui / modelRegistry / stale 检查，见 [`REPLACED_CONTEXTS`]）。只有
/// 其它实现方自建 `ReplacedSessionContextApi`（无法取回 `Arc`）时才走到这里，此时
/// `ui` / `model_registry` 用调用前捕获的值；其余成员一律读新上下文。
struct ReplacedHost {
    ctx: Arc<dyn ReplacedSessionContextApi>,
    ui: Arc<dyn ExtensionUiContext>,
    model_registry: Arc<ModelRegistry>,
}

impl ContextHost for ReplacedHost {
    fn assert_active(&self) -> Result<(), ContextError> {
        Ok(())
    }
    fn ui(&self) -> Arc<dyn ExtensionUiContext> {
        Arc::clone(&self.ui)
    }
    fn mode(&self) -> ExtensionMode {
        self.ctx.mode()
    }
    fn has_ui(&self) -> bool {
        self.ctx.has_ui()
    }
    fn cwd(&self) -> PathBuf {
        self.ctx.cwd().to_path_buf()
    }
    fn session_manager(&self) -> Arc<dyn ReadonlySessionManager> {
        Arc::new(SessionVia(Arc::clone(&self.ctx)))
    }
    fn model_registry(&self) -> Arc<ModelRegistry> {
        Arc::clone(&self.model_registry)
    }
    fn model(&self) -> Option<Model> {
        self.ctx.model()
    }
    fn scoped_models(&self) -> Vec<ScopedModel> {
        self.ctx.scoped_models()
    }
    fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.ctx.thinking_level()
    }
    fn is_idle(&self) -> bool {
        self.ctx.is_idle()
    }
    fn is_project_trusted(&self) -> bool {
        self.ctx.is_project_trusted()
    }
    fn signal(&self) -> Option<CancellationToken> {
        self.ctx.signal()
    }
    fn abort(&self) {
        self.ctx.abort();
    }
    fn has_pending_messages(&self) -> bool {
        self.ctx.has_pending_messages()
    }
    fn shutdown(&self) {
        self.ctx.shutdown();
    }
    fn get_context_usage(&self) -> Option<ContextUsage> {
        self.ctx.get_context_usage()
    }
    fn compact(&self, options: Option<CompactOptions>) {
        self.ctx.compact(options);
    }
    fn get_system_prompt(&self) -> String {
        self.ctx.get_system_prompt()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        self.ctx.get_system_prompt_options()
    }
    fn wait_for_idle(&self) -> LocalBoxFuture<'static, ()> {
        self.ctx.wait_for_idle()
    }
    fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        self.ctx.new_session(options)
    }
    fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        let ctx = Arc::clone(&self.ctx);
        Box::pin(async move { ctx.fork(&entry_id, options).await })
    }
    fn navigate_tree(
        &self,
        target_id: String,
        options: Option<NavigateTreeOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        let ctx = Arc::clone(&self.ctx);
        Box::pin(async move { ctx.navigate_tree(&target_id, options).await })
    }
    fn switch_session(
        &self,
        session_path: String,
        options: Option<SwitchSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        let ctx = Arc::clone(&self.ctx);
        Box::pin(async move { ctx.switch_session(&session_path, options).await })
    }
    fn reload(&self) -> LocalBoxFuture<'static, ()> {
        self.ctx.reload()
    }
    fn get_callable_tools(&self) -> Vec<AgentTool> {
        Vec::new()
    }
    fn execute_tool(
        &self,
        _tool_call_id: &str,
        _name: &str,
        _args: JsonValue,
        _options: ExecuteToolOptions,
    ) -> Option<kiana_ext_api::BoxFuture<'static, AgentToolCallOutcome>> {
        None
    }
}

/// 新会话上下文的 `sendMessage` / `sendUserMessage`（`ReplacedSessionContext` 语义）。
struct ReplacedSessionVia(Arc<dyn ReplacedSessionContextApi>);

impl ReplacedSessionActions for ReplacedSessionVia {
    fn send_message(
        &self,
        message: SendMessageOptions,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        self.0.send_message_checked(message)
    }
    fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        self.0.send_user_message_checked(content, options)
    }
}

/// `__kiana_ctx_make_replaced(handle)`：新建 `ReplacedSessionContext` 的 JS 值。
///
/// `handle` 有两种来源：
/// - {@link RegisteredObject::Context}：本单元构造的、绑定**新** runner 的
///   `ReplacedSessionContext`（正常路径），直接用它的 host（新 ui / modelRegistry /
///   stale 检查）生成 JS 值；
/// - {@link RegisteredObject::Replaced}：其它实现方提供的 trait 对象（兜底），用调用前捕获
///   的 ui / modelRegistry 包装。
///
/// 句柄已被取走（例如同一个回调被调用两次）或上下文已销毁时，抛
/// `replaced session context is no longer available`。
fn make_replaced_value<'js>(ctx: Ctx<'js>, handle: u64) -> EngineResult<Value<'js>> {
    let registered = lock(&OBJECTS).remove(&handle);
    let (replaced, ui, model_registry) = match registered {
        Some(RegisteredObject::Context(weak)) => match weak.upgrade() {
            Some(context) => {
                return context_to_value(&ctx, context, ContextKind::Replaced);
            }
            None => {
                return Err(Exception::throw_message(
                    &ctx,
                    "replaced session context is no longer available",
                ));
            }
        },
        Some(RegisteredObject::Replaced {
            context,
            ui,
            model_registry,
        }) => (context, ui, model_registry),
        _ => {
            return Err(Exception::throw_message(
                &ctx,
                "replaced session context is no longer available",
            ));
        }
    };
    let host: Arc<dyn ContextHost> = Arc::new(ReplacedHost {
        ctx: Arc::clone(&replaced),
        ui,
        model_registry,
    });
    let actions: Arc<dyn ReplacedSessionActions> = Arc::new(ReplacedSessionVia(replaced));
    context_to_value(
        &ctx,
        ExtensionContext::replaced(host, actions),
        ContextKind::Replaced,
    )
}

/// `__kiana_ctx_make_session_manager(handle)`：`newSession({ setup })` 的参数。
///
/// 句柄已被取走（同一个 setup 回调重复调用）或缺失时抛
/// `session manager is no longer available`。
fn make_session_manager_value<'js>(ctx: Ctx<'js>, handle: u64) -> EngineResult<Value<'js>> {
    let manager = match lock(&OBJECTS).remove(&handle) {
        Some(RegisteredObject::SetupSession { manager }) => manager,
        _ => {
            return Err(Exception::throw_message(
                &ctx,
                "session manager is no longer available",
            ));
        }
    };
    Class::instance(
        ctx.clone(),
        JsSessionManager {
            source: SessionManagerSource::Setup(manager),
        },
    )?
    .into_js(&ctx)
}

// ===========================================================================
// executeTool 的 onUpdate / 命令方法的回调参数
// ===========================================================================

/// 把 `AgentToolUpdateCallback`（JS 函数 id）包装成 Send+Sync 回调。
fn on_update_callback(
    bridge: &CallbackBridge,
    callback_id: u64,
) -> kiana_agent_core::AgentToolUpdateCallback {
    let bridge = bridge.clone();
    Arc::new(move |result| {
        bridge.fire(
            callback_id,
            JsonValue::Array(vec![tool_result_json(&result)]),
        );
    })
}

/// 回调失败记录。
///
/// `WithSessionFn` / `SetupFn`（`kiana-ext-api` 冻结）的返回类型没有错误通道；回调里抛出的
/// 错误写进这个槽，`spawn_context_op` 在宿主动作返回后检查它并**拒绝**命令的 Promise
/// （TS 的 `await withSession(...)` / `await options.setup(...)` 会把 rejection 传给调用方）。
type CallbackErrorSlot = Arc<Mutex<Option<String>>>;

/// 记录回调错误；只保留**第一条**（多个回调先后失败时，命令 Promise 报告最先的错误）。
fn record_callback_error(slot: &CallbackErrorSlot, message: String) {
    let mut slot = lock(slot);
    if slot.is_none() {
        *slot = Some(message);
    }
}

/// 把 `withSession(ctx)` 的回调 id 包成 `kiana_ext_api` 的 `WithSessionFn`。
///
/// - 回调的 Promise 被**真正等待**（`CallbackBridge::call` 等 `__kiana_ctx_callback_done`）；
/// - 抛错写进 `error_slot`（命令 Promise 随后 reject）；
/// - `ctx` 若是本单元构造的 `ReplacedSessionContext`，用它的新 `ContextHost` 生成 JS 值
///   （新 runner 的 ui / modelRegistry / stale 检查），否则退回旧快照。
fn with_session_callback(
    bridge: &CallbackBridge,
    callback_id: u64,
    ui: Arc<dyn ExtensionUiContext>,
    model_registry: Arc<ModelRegistry>,
    error_slot: CallbackErrorSlot,
) -> kiana_ext_api::types::WithSessionFn {
    let bridge = bridge.clone();
    Arc::new(move |replaced: Arc<dyn ReplacedSessionContextApi>| {
        // `setup` 已经失败：TS 的 `await options.setup(...)` 会先抛出，`finishSessionReplacement`
        // 根本不会调用 withSession。宿主（`SetupFn` 没有错误通道）可能仍会走到这里，跳过。
        if lock(&error_slot).is_some() {
            return Box::pin(async {}) as LocalBoxFuture<'static, ()>;
        }
        let bridge = bridge.clone();
        let error_slot = Arc::clone(&error_slot);
        let ui = Arc::clone(&ui);
        let model_registry = Arc::clone(&model_registry);
        let key = Arc::as_ptr(&replaced) as *const () as usize;
        let concrete = resolve_replaced_context(key);
        // 句柄只用于这一次回调：`make_replaced_value` 取出即删；没有被取出（回调 id 已失效）
        // 也要在回调结束后注销。
        let handle = match &concrete {
            Some(context) => register_object(RegisteredObject::Context(Arc::downgrade(context))),
            None => register_object(RegisteredObject::Replaced {
                context: replaced,
                ui,
                model_registry,
            }),
        };
        let future = bridge.call(
            callback_id,
            JsonValue::Array(vec![serde_json::json!({ "__kianaCtxRef": "replaced" })]),
            handle,
        );
        Box::pin(async move {
            if let Err(message) = future.await {
                record_callback_error(&error_slot, message);
            }
            bridge.release(callback_id);
            unregister_object(handle);
        })
    })
}

/// 把 `setup(sessionManager)` 的回调 id 包成 `kiana_ext_api` 的 `SetupFn`。
///
/// 参数是可写的 `SessionManager` 句柄；JS 侧通过 `__kiana_ctx_make_session_manager(handle)`
/// 得到 `SessionManager` 类实例（本单元提供 15 个只读成员）。回调的 Promise 被等待，抛错
/// 会中止 `newSession`（与 TS 的 `await options.setup(...)` 一致）。
fn setup_callback(
    bridge: &CallbackBridge,
    callback_id: u64,
    error_slot: CallbackErrorSlot,
) -> kiana_ext_api::types::SetupFn {
    let bridge = bridge.clone();
    Arc::new(
        move |manager: Arc<Mutex<kiana_session_core::session_manager::SessionManager>>| {
            let bridge = bridge.clone();
            let error_slot = Arc::clone(&error_slot);
            let handle = register_object(RegisteredObject::SetupSession {
                manager: Arc::clone(&manager),
            });
            let future = bridge.call(
                callback_id,
                JsonValue::Array(vec![serde_json::json!({
                    "__kianaCtxRef": "setupSessionManager",
                    "handle": handle,
                })]),
                0,
            );
            Box::pin(async move {
                if let Err(message) = future.await {
                    record_callback_error(&error_slot, message);
                }
                bridge.release(callback_id);
                unregister_object(handle);
            })
        },
    )
}

/// 状态锁的 poison 处理（只维护纯数据，panic 后状态仍然自洽）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

// ===========================================================================
// JS 原生类 ExtensionContext
// ===========================================================================

const CONTEXT_JS: &str = include_str!("../../js/context.js");

/// `__kiana.promise` 的 host kind（`js/context.js` 的 `invoke` 使用）。
pub const HOST_KIND: &str = "kiana_ctx";

/// UI 句柄缓存（同一个 `Arc` 指针复用同一个句柄）。
#[derive(Default)]
struct UiHandleCache {
    current: Option<(usize, u64)>,
}

/// JS 可见的 `ExtensionContext` 原生类。
///
/// 构造函数不安装任何 JS 可见字段（`design-extension-runtime.md` §5.3）；成员全部是原型上的
/// getter/方法，每次读取都回落到 [`ContextHost`]（TS 的读时求值）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ExtensionContext")]
pub struct JsExtensionContext {
    #[qjs(skip_trace)]
    inner: Arc<ExtensionContext>,
    #[qjs(skip_trace)]
    handle: u64,
    #[qjs(skip_trace)]
    ui_cache: Mutex<UiHandleCache>,
}

impl Drop for JsExtensionContext {
    fn drop(&mut self) {
        unregister_object(self.handle);
    }
}

/// 把 Rust 视图包装成 JS 原生类实例，再套上 `__kiana_ctx.makeContext` 的 JS 外观。
///
/// TS 的 `createContext()` 返回对象字面量：成员是**自有可枚举**属性，且四种形状只带各自的
/// 成员（base 上 `ctx.tools === undefined`）。原生类的方法在原型上且所有种类共用，因此由
/// `js/context.js` 的 `makeContext(native, kind)` 重建字面量形状（`Object.keys`/展开/种类
/// 特性检测保持相同语义）；原生实例仍是全部状态与宿主调用的落点。
fn context_to_value<'js>(
    ctx: &Ctx<'js>,
    context: Arc<ExtensionContext>,
    kind: ContextKind,
) -> EngineResult<Value<'js>> {
    let handle = register_object(RegisteredObject::Context(Arc::downgrade(&context)));
    let data = JsExtensionContext {
        inner: context,
        handle,
        ui_cache: Mutex::new(UiHandleCache::default()),
    };
    let native: Value<'js> = Class::instance(ctx.clone(), data)?.into_js(ctx)?;
    let kiana: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let make: Function<'js> = kiana.get("makeContext")?;
    make.call((rquickjs::function::This(kiana), native, kind.as_str()))
}

/// `createContext()` 的 JS 值。
pub fn create_context_value<'js>(
    ctx: &Ctx<'js>,
    host: Arc<dyn ContextHost>,
) -> EngineResult<Value<'js>> {
    context_to_value(ctx, ExtensionContext::base(host), ContextKind::Base)
}

/// `createToolContext(toolCallId, signal)` 的 JS 值。
pub fn create_tool_context_value<'js>(
    ctx: &Ctx<'js>,
    host: Arc<dyn ContextHost>,
    tool_call_id: &str,
    signal: Option<CancellationToken>,
) -> EngineResult<Value<'js>> {
    context_to_value(
        ctx,
        ExtensionContext::tool(host, tool_call_id, signal),
        ContextKind::Tool,
    )
}

/// `createCommandContext()` 的 JS 值。
pub fn create_command_context_value<'js>(
    ctx: &Ctx<'js>,
    host: Arc<dyn ContextHost>,
) -> EngineResult<Value<'js>> {
    context_to_value(ctx, ExtensionContext::command(host), ContextKind::Command)
}

/// `ReplacedSessionContext` 的 JS 值。
pub fn create_replaced_context_value<'js>(
    ctx: &Ctx<'js>,
    host: Arc<dyn ContextHost>,
    actions: Arc<dyn ReplacedSessionActions>,
) -> EngineResult<Value<'js>> {
    context_to_value(
        ctx,
        ExtensionContext::replaced(host, actions),
        ContextKind::Replaced,
    )
}

// ---- 小工具 ---------------------------------------------------------------

/// `assertActive()`；失败时把 stale 文案作为 JS `Error` 抛出。
fn require_active(ctx: &Ctx<'_>, host: &Arc<dyn ContextHost>) -> EngineResult<()> {
    host.assert_active()
        .map_err(|error| Exception::throw_message(ctx, error.message()))
}

fn undefined<'js>(ctx: &Ctx<'js>) -> Value<'js> {
    Value::new_undefined(ctx.clone())
}

/// 把 JS 函数登记到 `js/context.js` 的回调表，返回主机侧使用的数字 id。
fn js_callback_id<'js>(ctx: &Ctx<'js>, value: Option<&Value<'js>>) -> EngineResult<u64> {
    let Some(function) = value.and_then(|value| value.as_function().cloned()) else {
        return Ok(0);
    };
    let kiana: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let register: Function<'js> = kiana.get("registerCallback")?;
    register.call((function,))
}

/// `options.signal`（任意 JS `AbortSignal`）→ 宿主 `CancellationToken` 句柄。
///
/// TS 把调用方的 signal 原样转交给宿主；Rust 侧用 `CancellationToken` 表达
/// （`ARCHITECTURE.md` §4.2），因此在 JS 线程上登记 token 并挂一个 abort 监听器：signal
/// 中止时取消 token。已经中止的 signal 立即取消。
///
/// 每个调用都新建 token 并登记，而 `SIGNAL_HANDLES` 没有注销路径，所以反复传入新的
/// `AbortSignal` 会让两个句柄表持续增长；挂上的监听器闭包持有 token 的克隆。
fn signal_handle_from_value<'js>(
    ctx: &Ctx<'js>,
    value: Option<&Value<'js>>,
) -> EngineResult<Option<u64>> {
    let Some(value) = value else { return Ok(None) };
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(Exception::throw_type(
            ctx,
            "options.signal is not an AbortSignal",
        ));
    };
    let token = CancellationToken::new();
    let handle = register_signal_handle(&token);
    let aborted: bool = object.get("aborted")?;
    if aborted {
        token.cancel();
        return Ok(Some(handle));
    }
    let add: Function<'js> = object.get("addEventListener")?;
    let listener_token = token.clone();
    let listener = Function::new(ctx.clone(), move || listener_token.cancel())?;
    add.call::<_, ()>(("abort", listener))?;
    Ok(Some(handle))
}

/// 从 JS options 对象的某个字段读函数并登记成回调 id（非函数 → 0）。
fn option_callback_id<'js>(
    ctx: &Ctx<'js>,
    options: Option<&Value<'js>>,
    name: &str,
) -> EngineResult<u64> {
    let Some(object) = options.and_then(|value| value.as_object()) else {
        return Ok(0);
    };
    let value: Option<Value<'js>> = match object.get(name) {
        Ok(value) => Some(value),
        Err(error) => {
            let _ = ctx.catch();
            return Err(error);
        }
    };
    js_callback_id(ctx, value.as_ref())
}

/// JSON options 对象 + 一个 JS 回调字段（函数被 JSON 丢弃，这里补上数字 id）。
fn with_callback_field<'js>(
    ctx: &Ctx<'js>,
    mut base: JsonValue,
    options: Option<&Value<'js>>,
    name: &str,
) -> EngineResult<JsonValue> {
    let id = option_callback_id(ctx, options, name)?;
    if id == 0 {
        return Ok(base);
    }
    if !base.is_object() {
        base = object_value(JsonObject::new());
    }
    if let Some(object) = base.as_object_mut() {
        object.insert(name.to_owned(), JsonValue::Number(id.into()));
    }
    Ok(base)
}

/// 从 JS options 对象的某个字段读 signal 句柄。
fn option_signal_handle<'js>(
    ctx: &Ctx<'js>,
    options: Option<&Value<'js>>,
    name: &str,
) -> EngineResult<Option<u64>> {
    let Some(object) = options.and_then(|value| value.as_object()) else {
        return Ok(None);
    };
    let value: Value<'js> = match object.get(name) {
        Ok(value) => value,
        Err(error) => {
            let _ = ctx.catch();
            return Err(error);
        }
    };
    signal_handle_from_value(ctx, Some(&value))
}

/// `AgentToolResult` → JSON（传给 JS 的 `onUpdate`）。
fn tool_result_json(result: &kiana_agent_core::AgentToolResult<JsonValue>) -> JsonValue {
    serde_json::to_value(result).unwrap_or(JsonValue::Null)
}

/// `serde_json::Value` → JS（走 `JSON.parse`：键序、`__proto__`、数字语义都按 JS）。
fn json_value<'js>(ctx: &Ctx<'js>, value: &JsonValue) -> EngineResult<Value<'js>> {
    let text = serde_json::to_string(value).map_err(|error| {
        Exception::throw_message(ctx, &format!("failed to serialize context value: {error}"))
    })?;
    ctx.json_parse(text)
}

/// `kiana-ai` 的 `JsonObject`（`IndexMap`）→ `serde_json::Value::Object`（保持插入序）。
fn object_value(map: JsonObject) -> JsonValue {
    JsonValue::Object(serde_json::Map::from_iter(map))
}

/// JS 值 → `serde_json::Value`（`JSON.stringify` 语义：`undefined`/函数 → `null`）。
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

/// `__kiana_ctx.invoke(handle, op, args)` → `Promise`。
fn invoke_promise<'js>(
    ctx: &Ctx<'js>,
    handle: u64,
    op: &str,
    args: Value<'js>,
) -> EngineResult<Value<'js>> {
    let object: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let invoke: Function<'js> = object.get("invoke")?;
    invoke.call((This(object), handle, op, args))
}

/// `__kiana_ctx.rejected(message)` → rejected `Promise`（async 方法里的 stale 错误）。
fn rejected_promise<'js>(ctx: &Ctx<'js>, message: &str) -> EngineResult<Value<'js>> {
    let object: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let rejected: Function<'js> = object.get("rejected")?;
    rejected.call((This(object), message))
}

/// `__kiana_ctx.signal(handle)` → AbortSignal 兼容对象。
fn signal_object<'js>(ctx: &Ctx<'js>, handle: u64) -> EngineResult<Value<'js>> {
    let object: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let signal: Function<'js> = object.get("signal")?;
    signal.call((This(object), handle))
}

/// `__kiana_ctx.swallow(promise)`：吞掉内部 Promise 的 rejection（TS 的 void 方法没有返回值）。
fn swallow_promise<'js>(ctx: &Ctx<'js>, promise: Value<'js>) -> EngineResult<()> {
    let object: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let swallow: Function<'js> = object.get("swallow")?;
    swallow.call::<_, ()>((This(object), promise))?;
    Ok(())
}

/// `__kiana_ctx.makeUi(handle)`：`ui-adapter` 安装的 JS 桥；未安装时返回 `undefined`。
fn make_ui_object<'js>(ctx: &Ctx<'js>, handle: u64) -> EngineResult<Value<'js>> {
    let object: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let make: Function<'js> = object.get("makeUi")?;
    make.call((This(object), handle))
}

/// `__kiana_ctx.noopUi()`：与 `noOpUIContext` 等价的 JS 单例。
fn noop_ui_object<'js>(ctx: &Ctx<'js>) -> EngineResult<Value<'js>> {
    let object: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let noop: Function<'js> = object.get("noopUi")?;
    noop.call((This(object),))
}

fn mode_as_str(mode: ExtensionMode) -> &'static str {
    match mode {
        ExtensionMode::Tui => "tui",
        ExtensionMode::Rpc => "rpc",
        ExtensionMode::Json => "json",
        ExtensionMode::Print => "print",
    }
}

/// `ScopedModel` → TS `{ model, thinkingLevel? }`（可选字段缺省时不输出该键）。
fn scoped_model_to_json(model: &ScopedModel) -> JsonValue {
    let mut object = JsonObject::new();
    object.insert(
        "model".to_owned(),
        serde_json::to_value(&model.model).unwrap_or(JsonValue::Null),
    );
    if let Some(level) = model.thinking_level {
        object.insert(
            "thinkingLevel".to_owned(),
            serde_json::to_value(level).unwrap_or(JsonValue::Null),
        );
    }
    object_value(object)
}

// ---- 原生类成员 -----------------------------------------------------------

#[rquickjs::methods]
impl JsExtensionContext {
    /// 内部：只做 stale 检查（JS 外观的缓存型 getter 每次读取前调用）。
    #[qjs(rename = "assertActive")]
    fn assert_active_js(&self, ctx: Ctx<'_>) -> EngineResult<()> {
        require_active(&ctx, &self.inner.host)
    }

    /// `ctx.ui`（读时求值；`ui-adapter` 的 JS 桥按句柄构造 JS 对象）。
    #[qjs(get, rename = "ui")]
    fn ui<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        let ui = self.inner.host.ui();
        // 句柄按 `Arc` 指针缓存：同一个 UI 上下文每次读取都得到同一句柄（JS 桥据此保持对象
        // 身份）；runner 替换 UI 后指针变化，缓存自然失效并重新登记。
        let pointer = Arc::as_ptr(&ui) as *const () as usize;
        let cached = lock(&self.ui_cache).current;
        let handle = match cached {
            Some((ptr, handle)) if ptr == pointer => handle,
            _ => {
                let handle = register_ui_context(Arc::clone(&ui));
                lock(&self.ui_cache).current = Some((pointer, handle));
                handle
            }
        };
        let bridged = make_ui_object(&ctx, handle)?;
        if !bridged.is_undefined() {
            return Ok(bridged);
        }
        if !self.inner.host.has_ui() || crate::runner::noop_ui::is_no_op_ui_context(&ui) {
            return noop_ui_object(&ctx);
        }
        Err(Exception::throw_message(
            &ctx,
            "ctx.ui is not available: the extension UI bridge is not installed",
        ))
    }

    /// `ctx.mode`。
    #[qjs(get, rename = "mode")]
    fn mode<'js>(&self, ctx: Ctx<'js>) -> EngineResult<&'static str> {
        require_active(&ctx, &self.inner.host)?;
        Ok(mode_as_str(self.inner.host.mode()))
    }

    /// `ctx.hasUI`。
    #[qjs(get, rename = "hasUI")]
    fn has_ui(&self, ctx: Ctx<'_>) -> EngineResult<bool> {
        require_active(&ctx, &self.inner.host)?;
        Ok(self.inner.host.has_ui())
    }

    /// `ctx.cwd`。
    #[qjs(get, rename = "cwd")]
    fn cwd(&self, ctx: Ctx<'_>) -> EngineResult<String> {
        require_active(&ctx, &self.inner.host)?;
        Ok(self.inner.host.cwd().to_string_lossy().into_owned())
    }

    /// `ctx.sessionManager`（读时构造原生类实例）。
    ///
    /// 每次读取都新建原生实例；`js/context.js` 的外观会缓存首个实例，但读取前仍调用
    /// `assertActive()`，所以 stale 之后照样抛错。
    #[qjs(get, rename = "sessionManager")]
    fn session_manager<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        Class::instance(
            ctx.clone(),
            JsSessionManager {
                source: SessionManagerSource::Readonly(self.inner.host.session_manager()),
            },
        )?
        .into_js(&ctx)
    }

    /// `ctx.modelRegistry`（读时构造原生类实例）。
    ///
    /// 每次读取都登记一个新的 `Weak` 句柄；`js/context.js` 的外观只缓存首个实例，但读取前
    /// 仍调用 `assertActive()`，所以 stale 之后照样抛错。
    #[qjs(get, rename = "modelRegistry")]
    fn model_registry<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        let registry = self.inner.host.model_registry();
        let handle = register_object(RegisteredObject::ModelRegistry(Arc::downgrade(&registry)));
        let inner = Arc::clone(&self.inner);
        let context: Arc<dyn ExtensionContextApi> = inner;
        Class::instance(
            ctx.clone(),
            JsModelRegistry {
                registry,
                handle,
                context,
            },
        )?
        .into_js(&ctx)
    }

    /// `ctx.model`。
    #[qjs(get, rename = "model")]
    fn model<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        match self.inner.host.model() {
            Some(model) => {
                let value = serde_json::to_value(&model).unwrap_or(JsonValue::Null);
                json_value(&ctx, &value)
            }
            None => Ok(undefined(&ctx)),
        }
    }

    /// `ctx.scopedModels`。
    #[qjs(get, rename = "scopedModels")]
    fn scoped_models<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        let models = self.inner.host.scoped_models();
        let values: Vec<JsonValue> = models.iter().map(scoped_model_to_json).collect();
        json_value(&ctx, &JsonValue::Array(values))
    }

    /// `ctx.thinkingLevel`。
    #[qjs(get, rename = "thinkingLevel")]
    fn thinking_level<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        match self.inner.host.thinking_level() {
            Some(level) => {
                let value = serde_json::to_value(level).unwrap_or(JsonValue::Null);
                json_value(&ctx, &value)
            }
            None => Ok(undefined(&ctx)),
        }
    }

    /// `ctx.signal`（同一 token 复用同一个 JS 对象）。
    #[qjs(get, rename = "signal")]
    fn signal<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        match self.inner.host.signal() {
            Some(token) => {
                let handle = register_signal_handle(&token);
                signal_object(&ctx, handle)
            }
            None => Ok(undefined(&ctx)),
        }
    }

    /// `ctx.isIdle()`。
    #[qjs(rename = "isIdle")]
    fn is_idle(&self, ctx: Ctx<'_>) -> EngineResult<bool> {
        require_active(&ctx, &self.inner.host)?;
        Ok(self.inner.host.is_idle())
    }

    /// `ctx.isProjectTrusted()`。
    #[qjs(rename = "isProjectTrusted")]
    fn is_project_trusted(&self, ctx: Ctx<'_>) -> EngineResult<bool> {
        require_active(&ctx, &self.inner.host)?;
        Ok(self.inner.host.is_project_trusted())
    }

    /// `ctx.abort()`。
    #[qjs(rename = "abort")]
    fn abort(&self, ctx: Ctx<'_>) -> EngineResult<()> {
        require_active(&ctx, &self.inner.host)?;
        self.inner.host.abort();
        Ok(())
    }

    /// `ctx.hasPendingMessages()`。
    #[qjs(rename = "hasPendingMessages")]
    fn has_pending_messages(&self, ctx: Ctx<'_>) -> EngineResult<bool> {
        require_active(&ctx, &self.inner.host)?;
        Ok(self.inner.host.has_pending_messages())
    }

    /// `ctx.shutdown()`。
    #[qjs(rename = "shutdown")]
    fn shutdown(&self, ctx: Ctx<'_>) -> EngineResult<()> {
        require_active(&ctx, &self.inner.host)?;
        self.inner.host.shutdown();
        Ok(())
    }

    /// `ctx.getContextUsage()`。
    #[qjs(rename = "getContextUsage")]
    fn get_context_usage<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_active(&ctx, &self.inner.host)?;
        match self.inner.host.get_context_usage() {
            Some(usage) => {
                let value = serde_json::to_value(&usage).unwrap_or(JsonValue::Null);
                json_value(&ctx, &value)
            }
            None => Ok(undefined(&ctx)),
        }
    }

    /// `ctx.compact(options?)`（TS 返回 void：触发压缩但不等待完成）。
    ///
    /// `onComplete`/`onError` 是 JS 回调：登记成 id 交给宿主任务（`compact` host op），
    /// 压缩完成后由回调桥触发（`examples/extensions/trigger-compact.ts` 依赖两者）。
    #[qjs(rename = "compact")]
    fn compact<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> EngineResult<()> {
        require_active(&ctx, &self.inner.host)?;
        let mut payload = JsonObject::new();
        let mut custom_instructions: Option<String> = None;
        let mut on_complete = 0;
        let mut on_error = 0;
        if let Some(value) = options.0.as_ref() {
            // `value_to_json` 走 `JSON.stringify`，函数字段会消失；因此
            // `customInstructions` 从 JSON 快照读，`onComplete`/`onError` 必须从原始 JS 值
            // 登记成回调 id。
            let json = value_to_json(&ctx, value.clone())?;
            custom_instructions = json
                .get("customInstructions")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            if let Some(instructions) = &custom_instructions {
                payload.insert(
                    "customInstructions".to_owned(),
                    JsonValue::String(instructions.clone()),
                );
            }
            on_complete = option_callback_id(&ctx, options.0.as_ref(), "onComplete")?;
            on_error = option_callback_id(&ctx, options.0.as_ref(), "onError")?;
            if on_complete != 0 {
                payload.insert(
                    "onComplete".to_owned(),
                    JsonValue::Number(on_complete.into()),
                );
            }
            if on_error != 0 {
                payload.insert("onError".to_owned(), JsonValue::Number(on_error.into()));
            }
        }
        if on_complete == 0 && on_error == 0 {
            // 没有 JS 回调：与 TS 的 `compactFn(options)` 一样在调用栈上同步触发。
            let options = if custom_instructions.is_none() {
                None
            } else {
                Some(CompactOptions {
                    custom_instructions,
                    ..Default::default()
                })
            };
            self.inner.host.compact(options);
            return Ok(());
        }
        // 有回调时必须经 host op（回调桥需要宿主完成队列）。返回值不属于 TS 的 void 契约：
        // 这个 Promise 只表示宿主是否受理，扩展拿不到它，吞掉 rejection（真正的错误走
        // `onError` 回调）。
        let promise = invoke_promise(
            &ctx,
            self.handle,
            "compact",
            json_value(&ctx, &object_value(payload))?,
        )?;
        swallow_promise(&ctx, promise)?;
        Ok(())
    }

    /// `ctx.getSystemPrompt()`。
    ///
    /// TS 在 `before_agent_start` 期间把该槽替换成 `() => buildSystemPrompt(currentOptions)`：
    /// 每次读取都按**当前**（可能已被 handler 原地修改的）选项重新渲染。dispatch.js 在派发
    /// 期间安装 `__kiana_before_agent_start_render`（闭包持有活的选项对象）；存在时优先调用它，
    /// 否则退回 runner 的覆盖槽（同步入口时写入的渲染值）。
    #[qjs(rename = "getSystemPrompt")]
    fn get_system_prompt(&self, ctx: Ctx<'_>) -> EngineResult<String> {
        require_active(&ctx, &self.inner.host)?;
        let render: Option<Function<'_>> =
            ctx.globals().get("__kiana_before_agent_start_render")?;
        if let Some(render) = render {
            return render.call::<_, String>(());
        }
        Ok(self.inner.host.get_system_prompt())
    }

    /// `ctx.tools`（仅 tool ctx）：完整的 `AgentTool`（含 `execute`/`prepareArguments`）。
    ///
    /// JS 对象只投影 `name`/`label`/`description`/`parameters`/`outputSchema`/`executionMode`/
    /// `replay`/`prepareArguments`/`execute`（`constrained_sampling` 不在 JS 形状里）。
    /// `execute` 走 host op `tool.execute`；`prepareArguments` 是同步 JS 函数（TS 的同步
    /// 适配器），不返回 Promise。
    #[qjs(get, rename = "tools")]
    fn tools<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        if self.inner.kind != ContextKind::Tool {
            return Err(Exception::throw_type(
                &ctx,
                "ctx.tools is only available on an extension tool context",
            ));
        }
        require_active(&ctx, &self.inner.host)?;
        let tools = self.inner.host.get_callable_tools();
        let array = rquickjs::Array::new(ctx.clone())?;
        for (index, tool) in tools.iter().enumerate() {
            let object = Object::new(ctx.clone())?;
            object.set("name", tool.name.clone())?;
            object.set("label", tool.label.clone())?;
            object.set("description", tool.description.clone())?;
            object.set("parameters", json_value(&ctx, &tool.parameters)?)?;
            if let Some(schema) = &tool.output_schema {
                object.set("outputSchema", json_value(&ctx, schema)?)?;
            }
            if let Some(mode) = tool.execution_mode {
                object.set(
                    "executionMode",
                    match mode {
                        kiana_agent_core::ToolExecutionMode::Sequential => "sequential",
                        kiana_agent_core::ToolExecutionMode::Parallel => "parallel",
                    },
                )?;
            }
            if let Some(replay) = tool.replay {
                object.set(
                    "replay",
                    match replay {
                        kiana_agent_core::types::ReplayPolicy::Never => "never",
                        kiana_agent_core::types::ReplayPolicy::Safe => "safe",
                    },
                )?;
            }
            if let Some(prepare) = tool.prepare_arguments.clone() {
                // TS 的 `prepareArguments` 可以就地改写并返回同一对象，也可以返回新对象；
                // Rust 用 `Option` 区分（`None` = 同一对象，保留 `&mut` 改写；`Some` = 用
                // 新值）。这里是同步 JS 函数，不经过 Promise。
                let function = Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'js>, args: Value<'js>| -> EngineResult<Value<'js>> {
                        let mut json = value_to_json(&ctx, args)?;
                        match prepare(&mut json) {
                            Some(prepared) => json_value(&ctx, &prepared),
                            None => json_value(&ctx, &json),
                        }
                    },
                )?;
                object.set("prepareArguments", function)?;
            }
            // `execute` 保持 TS 的四个位置参数（toolCallId、params、signal、onUpdate）：
            // signal 转成 token 句柄、onUpdate 登记成回调 id，二者都不能直接进 JSON payload。
            // 工具名与上下文句柄按值捕获（不捕获工具对象本身）；调用经 host op
            // `tool.execute` 由宿主按名字从当前可调用工具里重新查找，因此使用的是
            // 调用时刻的工具实现（见 `start_tool_execute`）。
            let tool_name = tool.name.clone();
            let handle = self.handle;
            let execute = Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      tool_call_id: String,
                      params: Opt<Value<'js>>,
                      signal: Opt<Value<'js>>,
                      on_update: Opt<Value<'js>>|
                      -> EngineResult<Value<'js>> {
                    let mut payload = JsonObject::new();
                    payload.insert("toolName".to_owned(), JsonValue::String(tool_name.clone()));
                    payload.insert("toolCallId".to_owned(), JsonValue::String(tool_call_id));
                    payload.insert(
                        "args".to_owned(),
                        match params.0 {
                            Some(value) => value_to_json(&ctx, value)?,
                            None => JsonValue::Null,
                        },
                    );
                    if let Some(handle) = signal_handle_from_value(&ctx, signal.0.as_ref())? {
                        payload.insert("signal".to_owned(), JsonValue::Number(handle.into()));
                    }
                    let on_update = js_callback_id(&ctx, on_update.0.as_ref())?;
                    if on_update != 0 {
                        payload.insert("onUpdate".to_owned(), JsonValue::Number(on_update.into()));
                    }
                    invoke_promise(
                        &ctx,
                        handle,
                        "tool.execute",
                        json_value(&ctx, &object_value(payload))?,
                    )
                },
            )?;
            object.set("execute", execute)?;
            array.set(index, object)?;
        }
        Ok(array.into_value())
    }

    /// `ctx.executeTool(name, args, options?)` → `Promise<AgentToolCallOutcome>`。
    ///
    /// `options` 是活 JS 值：`signal` 转成宿主句柄、`onUpdate` 登记成回调 id（函数不能进
    /// JSON payload）。TS 把调用方 options 原样转交（`{...options, signal: options.signal ??
    /// signal}`），这里两者都保留。
    ///
    /// 种类错误（非 tool ctx）同步抛 `TypeError`；stale 时返回 rejected Promise（TS 的
    /// `async` 方法语义）。
    #[qjs(rename = "executeTool")]
    fn execute_tool<'js>(
        &self,
        ctx: Ctx<'js>,
        name: String,
        args: Opt<Value<'js>>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        if self.inner.kind != ContextKind::Tool {
            return Err(Exception::throw_type(
                &ctx,
                "ctx.executeTool is only available on an extension tool context",
            ));
        }
        if let Err(error) = self.inner.host.assert_active() {
            return rejected_promise(&ctx, error.message());
        }
        let arguments = match args.0 {
            Some(value) => value_to_json(&ctx, value)?,
            None => JsonValue::Null,
        };
        let signal = option_signal_handle(&ctx, options.0.as_ref(), "signal")?;
        let on_update = option_callback_id(&ctx, options.0.as_ref(), "onUpdate")?;
        let mut payload = JsonObject::new();
        payload.insert(
            "toolCallId".to_owned(),
            JsonValue::String(self.inner.tool_call_id.clone().unwrap_or_default()),
        );
        payload.insert("name".to_owned(), JsonValue::String(name));
        payload.insert("arguments".to_owned(), arguments);
        if let Some(handle) = signal {
            payload.insert("signal".to_owned(), JsonValue::Number(handle.into()));
        }
        if on_update != 0 {
            payload.insert("onUpdate".to_owned(), JsonValue::Number(on_update.into()));
        }
        let payload = json_value(&ctx, &object_value(payload))?;
        invoke_promise(&ctx, self.handle, "executeTool", payload)
    }

    /// `ctx.getSystemPromptOptions()`（仅命令上下文）。
    #[qjs(rename = "getSystemPromptOptions")]
    fn get_system_prompt_options<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        require_command(&ctx, self.inner.kind, "getSystemPromptOptions")?;
        require_active(&ctx, &self.inner.host)?;
        let options = self.inner.host.get_system_prompt_options();
        let value = serde_json::to_value(&options).unwrap_or(JsonValue::Null);
        json_value(&ctx, &value)
    }

    /// `ctx.waitForIdle()` → `Promise<void>`。
    #[qjs(rename = "waitForIdle")]
    fn wait_for_idle<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        self.command_promise(&ctx, "waitForIdle", JsonValue::Null)
    }

    /// `ctx.newSession(options?)` → `Promise<{ cancelled }>`。
    #[qjs(rename = "newSession")]
    fn new_session<'js>(
        &self,
        ctx: Ctx<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        // stale/种类检查必须在登记 JS 回调**之前**：否则抛错时回调表里会留下无人释放的项。
        require_command(&ctx, self.inner.kind, "newSession")?;
        require_active(&ctx, &self.inner.host)?;
        let mut payload = match options.0.as_ref() {
            Some(value) => value_to_json(&ctx, value.clone())?,
            None => JsonValue::Null,
        };
        if !payload.is_object() {
            payload = object_value(JsonObject::new());
        }
        if let Some(object) = payload.as_object_mut() {
            // 回调函数不能进 JSON：单独登记成 id（0 = 未给出）。
            let with_session = option_callback_id(&ctx, options.0.as_ref(), "withSession")?;
            let setup = option_callback_id(&ctx, options.0.as_ref(), "setup")?;
            if with_session != 0 {
                object.insert(
                    "withSession".to_owned(),
                    JsonValue::Number(with_session.into()),
                );
            }
            if setup != 0 {
                object.insert("setup".to_owned(), JsonValue::Number(setup.into()));
            }
        }
        self.command_promise(&ctx, "newSession", payload)
    }

    /// `ctx.fork(entryId, options?)` → `Promise<{ cancelled }>`。
    #[qjs(rename = "fork")]
    fn fork<'js>(
        &self,
        ctx: Ctx<'js>,
        entry_id: String,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        require_command(&ctx, self.inner.kind, "fork")?;
        require_active(&ctx, &self.inner.host)?;
        let options_json = match options.0.as_ref() {
            Some(value) => value_to_json(&ctx, value.clone())?,
            None => JsonValue::Null,
        };
        let mut payload = JsonObject::new();
        payload.insert("entryId".to_owned(), JsonValue::String(entry_id));
        payload.insert(
            "options".to_owned(),
            with_callback_field(&ctx, options_json, options.0.as_ref(), "withSession")?,
        );
        self.command_promise(&ctx, "fork", object_value(payload))
    }

    /// `ctx.navigateTree(targetId, options?)` → `Promise<{ cancelled }>`。
    #[qjs(rename = "navigateTree")]
    fn navigate_tree<'js>(
        &self,
        ctx: Ctx<'js>,
        target_id: String,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let options = match options.0 {
            Some(value) => value_to_json(&ctx, value)?,
            None => JsonValue::Null,
        };
        let mut payload = JsonObject::new();
        payload.insert("targetId".to_owned(), JsonValue::String(target_id));
        payload.insert("options".to_owned(), options);
        self.command_promise(&ctx, "navigateTree", object_value(payload))
    }

    /// `ctx.switchSession(sessionPath, options?)` → `Promise<{ cancelled }>`。
    #[qjs(rename = "switchSession")]
    fn switch_session<'js>(
        &self,
        ctx: Ctx<'js>,
        session_path: String,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        require_command(&ctx, self.inner.kind, "switchSession")?;
        require_active(&ctx, &self.inner.host)?;
        let options_json = match options.0.as_ref() {
            Some(value) => value_to_json(&ctx, value.clone())?,
            None => JsonValue::Null,
        };
        let mut payload = JsonObject::new();
        payload.insert("sessionPath".to_owned(), JsonValue::String(session_path));
        payload.insert(
            "options".to_owned(),
            with_callback_field(&ctx, options_json, options.0.as_ref(), "withSession")?,
        );
        self.command_promise(&ctx, "switchSession", object_value(payload))
    }

    /// `ctx.reload()` → `Promise<void>`。
    #[qjs(rename = "reload")]
    fn reload<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        self.command_promise(&ctx, "reload", JsonValue::Null)
    }

    /// `ctx.sendMessage(message, options?)` → `Promise<void>`（仅 replaced）。
    #[qjs(rename = "sendMessage")]
    fn send_message<'js>(
        &self,
        ctx: Ctx<'js>,
        message: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let message = value_to_json(&ctx, message)?;
        let options = match options.0 {
            Some(value) => value_to_json(&ctx, value)?,
            None => JsonValue::Null,
        };
        self.replaced_promise(&ctx, "sendMessage", message, options)
    }

    /// `ctx.sendUserMessage(content, options?)` → `Promise<void>`（仅 replaced）。
    #[qjs(rename = "sendUserMessage")]
    fn send_user_message<'js>(
        &self,
        ctx: Ctx<'js>,
        content: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let content = value_to_json(&ctx, content)?;
        let options = match options.0 {
            Some(value) => value_to_json(&ctx, value)?,
            None => JsonValue::Null,
        };
        self.replaced_promise(&ctx, "sendUserMessage", content, options)
    }
}

impl JsExtensionContext {
    /// 命令上下文方法的统一入口：种类检查 → stale 检查 → Promise。
    ///
    /// TS 把这些方法写成**非 async** 箭头（`context.waitForIdle = () => { this.assertActive();
    /// return this.waitForIdleFn(); }`），因此 stale 时是**同步 throw**，而不是返回一个被拒绝
    /// 的 Promise；`try { ctx.waitForIdle() } catch` 与 fire-and-forget 调用都依赖这个区别。
    fn command_promise<'js>(
        &self,
        ctx: &Ctx<'js>,
        op: &str,
        args: JsonValue,
    ) -> EngineResult<Value<'js>> {
        require_command(ctx, self.inner.kind, op)?;
        require_active(ctx, &self.inner.host)?;
        invoke_promise(ctx, self.handle, op, json_value(ctx, &args)?)
    }

    /// `ReplacedSessionContext` 方法的统一入口。
    fn replaced_promise<'js>(
        &self,
        ctx: &Ctx<'js>,
        op: &str,
        value: JsonValue,
        options: JsonValue,
    ) -> EngineResult<Value<'js>> {
        if self.inner.kind != ContextKind::Replaced {
            return Err(Exception::throw_type(
                ctx,
                &format!("ctx.{op} is only available on a replaced session context"),
            ));
        }
        if let Err(error) = self.inner.host.assert_active() {
            // TS 的 `sendMessage` / `sendUserMessage` 是 async（`this.sendCustomMessage(...)`），
            // stale/失败以 rejected Promise 表达。
            return rejected_promise(ctx, error.message());
        }
        let mut payload = JsonObject::new();
        payload.insert("value".to_owned(), value);
        payload.insert("options".to_owned(), options);
        invoke_promise(
            ctx,
            self.handle,
            op,
            json_value(ctx, &object_value(payload))?,
        )
    }
}

fn require_command(ctx: &Ctx<'_>, kind: ContextKind, method: &str) -> EngineResult<()> {
    if kind.is_command() {
        Ok(())
    } else {
        Err(Exception::throw_type(
            ctx,
            &format!("ctx.{method} is only available on an extension command context"),
        ))
    }
}

// ===========================================================================
// 安装：JS 全局原语 + host kind
// ===========================================================================

/// 安装 `js/context.js`、宿主原语与 `kiana_ctx` host kind（幂等）。
///
/// `crate-facade` 按 `design-extension-runtime.md` §9.2 的第 8 步调用。
///
/// 必须在 `engine::install`（`prelude_core.js` 提供 `__kiana` Promise 协议）之后调用；
/// `js/context.js` 顶层只依赖它，不 import 任何虚拟模块。`has_host_kind` 早退保证幂等。
pub fn install(engine: &JsEngine) -> EngineResult<()> {
    if engine.has_host_kind(HOST_KIND) {
        return Ok(());
    }
    engine.register_global("__kiana_ctx_signal_aborted", |ctx| {
        let function = Function::new(ctx.clone(), |handle: u64| signal_handle_aborted(handle))?;
        function.into_js(ctx)
    })?;
    engine.register_global("__kiana_ctx_callback_done", |ctx| {
        let function = Function::new(ctx.clone(), callback_done)?;
        function.into_js(ctx)
    })?;
    engine.register_global("__kiana_ctx_make_replaced", |ctx| {
        let function = Function::new(ctx.clone(), make_replaced_value)?;
        function.into_js(ctx)
    })?;
    engine.register_global("__kiana_ctx_make_session_manager", |ctx| {
        let function = Function::new(ctx.clone(), make_session_manager_value)?;
        function.into_js(ctx)
    })?;
    engine.register_host_kind(HOST_KIND, handle_context_call);
    engine.eval_script::<()>(CONTEXT_JS, "<context.js>")?;
    Ok(())
}

/// `__kiana.promise("kiana_ctx", payload)` 的 payload。
#[derive(serde::Deserialize)]
struct ContextRequest {
    handle: u64,
    op: String,
    #[serde(default)]
    args: JsonValue,
}

/// payload 里是否带 JS 回调 id（`onUpdate`/`onComplete`/`onError`/`withSession`/`setup`）。
fn has_callback_values(value: &JsonValue) -> bool {
    match value {
        JsonValue::Object(map) => map.iter().any(|(key, value)| {
            (matches!(
                key.as_str(),
                "onUpdate" | "onComplete" | "onError" | "withSession" | "setup"
            ) && value.as_u64().is_some_and(|id| id != 0))
                || has_callback_values(value)
        }),
        JsonValue::Array(values) => values.iter().any(has_callback_values),
        _ => false,
    }
}

/// 读一个数字字段（回调 id / signal 句柄）。
fn field_u64(args: &JsonValue, name: &str) -> u64 {
    args.get(name).and_then(JsonValue::as_u64).unwrap_or(0)
}

// `__kiana.promise("kiana_ctx", payload)` 的分派点（JS 线程；payload 在 spawn_local 之前
// 解析成 Rust 类型，见 `engine::HostKindHandler`）。payload 里出现任一非零回调 id 时先启动
// 回调桥；没有回调的调用不建泵任务。
//
// 路由：
// - `executeTool` / `tool.execute`：工具路径，各自的上下文失效/查找失败错误文案不同；
// - `modelRegistry.*`：只认模型注册表句柄（`ctx.modelRegistry` 是读时构造的临时对象）；
// - `modelRegistry.streamNext` / `streamResult` / `streamClose`：只认流 id，payload 的
//   `handle` 由 JS 侧固定为 0（`__kiana_ctx.invoke(0, …)`）；
// - 其余：`spawn_context_op` 的通用上下文操作。
fn handle_context_call<'js>(call: HostCall<'js>) -> EngineResult<()> {
    let request: ContextRequest = call.payload_json()?;
    let sender = call.completion_sender();
    let id = call.id();
    let bridge = if has_callback_values(&request.args) {
        CallbackBridge::start(call.ctx(), &sender)?
    } else {
        None
    };
    match request.op.as_str() {
        "executeTool" => start_execute_tool(&request, sender, id, bridge),
        "tool.execute" => start_tool_execute(&request, sender, id, bridge),
        "modelRegistry.getApiKeyAndHeaders"
        | "modelRegistry.getProviderAuth"
        | "modelRegistry.getApiKeyForProvider"
        | "modelRegistry.refresh"
        | "modelRegistry.complete"
        | "modelRegistry.getAvailableOfType"
        | "modelRegistry.classify"
        | "modelRegistry.generateImages"
        | "modelRegistry.stream"
        | "modelRegistry.streamSimple" => {
            spawn_model_registry_op(request, sender, id);
        }
        "modelRegistry.streamNext" | "modelRegistry.streamResult" | "modelRegistry.streamClose" => {
            spawn_stream_op(request, sender, id);
        }
        _ => spawn_context_op(request, sender, id, bridge),
    }
    Ok(())
}

/// 把 JSON 结果 settle 成 JS Promise（序列化失败按错误拒绝）。
fn resolve_json<T: serde::Serialize>(
    sender: &crate::engine::HostCompletionSender,
    id: u64,
    value: &T,
) {
    match serde_json::to_value(value) {
        Ok(value) => {
            sender.resolve(id, HostValue::Json(value));
        }
        Err(error) => {
            sender.reject_message(id, format!("failed to serialize context result: {error}"));
        }
    }
}

fn parse_arg<T: serde::de::DeserializeOwned>(value: &JsonValue, what: &str) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|error| format!("invalid {what}: {error}"))
}

fn field_str(args: &JsonValue, name: &str) -> Option<String> {
    args.get(name)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn field_bool(args: &JsonValue, name: &str) -> Option<bool> {
    args.get(name).and_then(JsonValue::as_bool)
}

fn nested(args: &JsonValue, name: &str) -> JsonValue {
    args.get(name).cloned().unwrap_or(JsonValue::Null)
}

/// 宿主调用 payload 里的 signal 句柄 → token。
fn request_signal(args: &JsonValue) -> Option<CancellationToken> {
    args.get("signal")
        .and_then(JsonValue::as_u64)
        .and_then(resolve_signal_handle)
}

/// `newSession(options)` 的 JS payload → [`NewSessionOptions`]。
///
/// payload 是 `null` 时返回 `None`（没有 options）。`withSession`/`setup` 的数字 id 只有
/// 在回调桥存在时才包装成 `kiana_ext_api` 的回调类型；`bridge` 为 `None` 表示本次调用
/// 没有 JS 回调，对应字段保持 `None`。JS 侧即使没传 options 也会发送空对象，因此 JS 路径
/// 通常得到 `Some(NewSessionOptions { … 全为 None })`。
fn parse_new_session_options(
    args: &JsonValue,
    bridge: Option<&CallbackBridge>,
    ui: &Arc<dyn ExtensionUiContext>,
    model_registry: &Arc<ModelRegistry>,
    errors: &CallbackErrorSlot,
) -> Option<NewSessionOptions> {
    if args.is_null() {
        return None;
    }
    let with_session = match (field_u64(args, "withSession"), bridge) {
        (0, _) | (_, None) => None,
        (id, Some(bridge)) => Some(with_session_callback(
            bridge,
            id,
            Arc::clone(ui),
            Arc::clone(model_registry),
            Arc::clone(errors),
        )),
    };
    let setup = match (field_u64(args, "setup"), bridge) {
        (0, _) | (_, None) => None,
        (id, Some(bridge)) => Some(setup_callback(bridge, id, Arc::clone(errors))),
    };
    Some(NewSessionOptions {
        parent_session: field_str(args, "parentSession"),
        setup,
        with_session,
    })
}

/// `fork(entryId, options)` 的 `options` → [`ForkOptions`]。
///
/// `options` 为 `null` 时返回 `None`（可选参数缺省）；`withSession` 的处理同
/// `parse_new_session_options`。`position` 反序列化失败时落为 `None`，不报错。
fn parse_fork_options(
    args: &JsonValue,
    bridge: Option<&CallbackBridge>,
    ui: &Arc<dyn ExtensionUiContext>,
    model_registry: &Arc<ModelRegistry>,
    errors: &CallbackErrorSlot,
) -> Option<ForkOptions> {
    let options = nested(args, "options");
    if options.is_null() {
        return None;
    }
    let with_session = match (field_u64(&options, "withSession"), bridge) {
        (0, _) | (_, None) => None,
        (id, Some(bridge)) => Some(with_session_callback(
            bridge,
            id,
            Arc::clone(ui),
            Arc::clone(model_registry),
            Arc::clone(errors),
        )),
    };
    Some(ForkOptions {
        position: options
            .get("position")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        with_session,
    })
}

/// `navigateTree(targetId, options)` 的 `options` → [`NavigateTreeOptions`]。
///
/// TS 的这个方法没有 `withSession`（只有数据字段），因此不接受回调桥参数。
fn parse_navigate_tree_options(args: &JsonValue) -> Option<NavigateTreeOptions> {
    let options = nested(args, "options");
    if options.is_null() {
        return None;
    }
    Some(NavigateTreeOptions {
        summarize: field_bool(&options, "summarize"),
        custom_instructions: field_str(&options, "customInstructions"),
        replace_instructions: field_bool(&options, "replaceInstructions"),
        label: field_str(&options, "label"),
    })
}

/// `switchSession(sessionPath, options)` 的 `options` → [`SwitchSessionOptions`]。
///
/// `options` 为 `null` 时返回 `None`；`withSession` 的处理同
/// `parse_new_session_options`。
fn parse_switch_session_options(
    args: &JsonValue,
    bridge: Option<&CallbackBridge>,
    ui: &Arc<dyn ExtensionUiContext>,
    model_registry: &Arc<ModelRegistry>,
    errors: &CallbackErrorSlot,
) -> Option<SwitchSessionOptions> {
    let options = nested(args, "options");
    if options.is_null() {
        return None;
    }
    let with_session = match (field_u64(&options, "withSession"), bridge) {
        (0, _) | (_, None) => None,
        (id, Some(bridge)) => Some(with_session_callback(
            bridge,
            id,
            Arc::clone(ui),
            Arc::clone(model_registry),
            Arc::clone(errors),
        )),
    };
    Some(SwitchSessionOptions { with_session })
}

/// `ctx.sendMessage(message, options?)` 的 JS 形状 → [`SendMessageOptions`]。
///
/// TS: `message` 是 `Pick<CustomMessage, "customType" | "content" | "display" | "details">`，
/// `options` 单独给 `{ triggerTurn?, deliverAs? }`；Rust 的 [`SendMessageOptions`] 把两者
/// 合在一个结构里。
///
/// TS 的会话层会归一化无类型的输入（`agent-session.ts#sendCustomMessage`）：
/// `content: message.content ?? []`；`display` / `details` **原样传递**——`details: null`
/// 保留为 `null`（不是被丢掉），缺失的 `customType` 按 `undefined` 处理（Rust 的会话条目
/// 字段是 `String`，这里用空串表达）。`CustomMessagePayload.display` 是冻结契约里的
/// `bool`，因此缺失时落为 `false`（见报告 deviations）。
fn parse_send_message_options(args: &JsonValue) -> Result<SendMessageOptions, String> {
    let value = nested(args, "value");
    if !value.is_object() {
        return Err("invalid sendMessage message: expected an object".to_owned());
    }
    let custom_type = value
        .get("customType")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let content = match value.get("content") {
        None | Some(JsonValue::Null) => UserContent::Blocks(Vec::new()),
        Some(content) => serde_json::from_value(content.clone())
            .map_err(|error| format!("invalid sendMessage message: {error}"))?,
    };
    let message = CustomMessagePayload {
        custom_type,
        content,
        // TS: `display: message.display`（`undefined` 保留为 undefined）；Rust 的冻结类型
        // 是 `bool`，`serde_json` 的 `as_bool` 对 undefined 给 `None`。
        display: value
            .get("display")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        // TS: `details: message.details` —— 显式 `null` 原样保留。
        details: value.get("details").cloned(),
    };
    let options = nested(args, "options");
    if options.is_null() {
        return Ok(SendMessageOptions {
            message,
            trigger_turn: None,
            deliver_as: None,
        });
    }
    Ok(SendMessageOptions {
        message,
        trigger_turn: field_bool(&options, "triggerTurn"),
        deliver_as: options
            .get("deliverAs")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
    })
}

/// `sendUserMessage(content, options)` 的 `options` → [`SendUserMessageOptions`]。
///
/// `options` 为 `null`/缺失，或反序列化失败时返回 `None`（调用方按缺省 options 处理）。
fn parse_send_user_message_options(args: &JsonValue) -> Option<SendUserMessageOptions> {
    let options = nested(args, "options");
    if options.is_null() {
        return None;
    }
    serde_json::from_value(options).ok()
}

/// 释放 payload 里登记的所有 JS 回调 id。
///
/// JS 侧在调用 host op **之前**就把 `onUpdate`/`withSession`/… 登记进了回调表（函数不能
/// 进 JSON）。宿主操作提前失败（上下文已销毁、工具不存在）时必须把这些表项释放掉，否则
/// JS 函数会一直留在表里（TS 靠 GC 回收）。
fn release_payload_callbacks(bridge: &Option<CallbackBridge>, value: &JsonValue) {
    fn walk(bridge: &CallbackBridge, value: &JsonValue) {
        match value {
            JsonValue::Object(map) => {
                for (key, value) in map {
                    if matches!(
                        key.as_str(),
                        "onUpdate" | "onComplete" | "onError" | "withSession" | "setup"
                    ) && let Some(id) = value.as_u64()
                    {
                        bridge.release(id);
                    }
                    walk(bridge, value);
                }
            }
            JsonValue::Array(values) => {
                for value in values {
                    walk(bridge, value);
                }
            }
            _ => {}
        }
    }
    if let Some(bridge) = bridge {
        walk(bridge, value);
    }
}

// `ctx.executeTool(name, args, options?)` 的 host op 入口（在 JS 线程的调用栈里执行）。
//
// 步骤：
// 1. 用数字句柄取回上下文；失效时先释放 payload 里已登记的 JS 回调、再 reject；
// 2. `onUpdate` 包装成 `AgentToolUpdateCallback`（`fire` 不等待结果）；
// 3. signal 优先级：调用方 `options.signal` ?? 工具上下文构造时捕获的 signal；
// 4. 宿主的 `execute_tool` 返回 `None`（没有绑定动作）时用 `nested_call_fallback` 的固定
//    结果（JS 路径传 `JsonValue::Null` 作 args，回退实现并不读取它）；
// 5. 成功路径在宿主 future 完成后释放 `onUpdate` 回调表项，避免 JS 函数滞留在回调表里。
fn start_execute_tool(
    request: &ContextRequest,
    sender: crate::engine::HostCompletionSender,
    id: u64,
    bridge: Option<CallbackBridge>,
) {
    let Some(context) = resolve_context(request.handle) else {
        release_payload_callbacks(&bridge, &request.args);
        sender.reject_message(id, "context is no longer available");
        return;
    };
    let tool_call_id = field_str(&request.args, "toolCallId").unwrap_or_default();
    let name = field_str(&request.args, "name").unwrap_or_default();
    let arguments = nested(&request.args, "arguments");
    let on_update_id = field_u64(&request.args, "onUpdate");
    let on_update = match (&bridge, on_update_id) {
        (Some(bridge), 1..) => Some(on_update_callback(bridge, on_update_id)),
        _ => None,
    };
    // TS: `{...options, signal: options.signal ?? signal}`。
    let signal = request
        .args
        .get("signal")
        .and_then(JsonValue::as_u64)
        .and_then(resolve_signal_handle)
        .or_else(|| context.default_signal());
    let options = ExecuteToolOptions { signal, on_update };
    match context
        .host()
        .execute_tool(&tool_call_id, &name, arguments, options)
    {
        Some(future) => {
            // 任务局部 cwd 覆盖不会跨 spawn_local 传播：隔离子代理通过
            // `ctx.executeTool` 发起的嵌套工具调用必须在同一覆盖下执行，否则会按
            // 父会话 cwd 解析路径、走父会话的权限检查。
            let cwd_override = crate::runner::current_cwd_override();
            tokio::task::spawn_local(async move {
                let run = async move {
                    let outcome = future.await;
                    resolve_json(&sender, id, &outcome);
                    if let Some(bridge) = &bridge {
                        bridge.release(on_update_id);
                    }
                };
                match cwd_override {
                    Some(cwd) => crate::runner::with_cwd_override(cwd, run).await,
                    None => run.await,
                }
            });
        }
        None => {
            let outcome = nested_call_fallback(&tool_call_id, &name, JsonValue::Null);
            resolve_json(&sender, id, &outcome);
            if let Some(bridge) = &bridge {
                bridge.release(on_update_id);
            }
        }
    }
}

/// `ctx.tools[i].execute(toolCallId, params, signal, onUpdate)` 的 host op 入口（JS 线程；
/// `AgentTool.execute` 语义：抛错 → Promise reject，成功 → `AgentToolResult`）。
///
/// 与 `start_execute_tool` 的区别：这里按 `name` 从**宿主当前**的可调用工具里重新查找
/// `AgentTool`（`ctx.tools` 数组只是读取那一刻的快照），直接调用工具自己的 `execute`。
/// 工具抛错按 `AgentToolError` 的 name（缺省 `"Error"`）/message reject；查找失败与上下文
/// 失效同样先释放 payload 里已登记的回调。
fn start_tool_execute(
    request: &ContextRequest,
    sender: crate::engine::HostCompletionSender,
    id: u64,
    bridge: Option<CallbackBridge>,
) {
    let Some(context) = resolve_context(request.handle) else {
        release_payload_callbacks(&bridge, &request.args);
        sender.reject_message(id, "context is no longer available");
        return;
    };
    let name = field_str(&request.args, "toolName").unwrap_or_default();
    let Some(tool) = context
        .host()
        .get_callable_tools()
        .into_iter()
        .find(|tool| tool.name == name)
    else {
        release_payload_callbacks(&bridge, &request.args);
        sender.reject_message(
            id,
            format!("Tool \"{name}\" is not available in this context"),
        );
        return;
    };
    let tool_call_id = field_str(&request.args, "toolCallId").unwrap_or_default();
    let params = nested(&request.args, "args");
    let on_update_id = field_u64(&request.args, "onUpdate");
    let on_update = match (&bridge, on_update_id) {
        (Some(bridge), 1..) => Some(on_update_callback(bridge, on_update_id)),
        _ => None,
    };
    let signal = request
        .args
        .get("signal")
        .and_then(JsonValue::as_u64)
        .and_then(resolve_signal_handle);
    // `ctx.tools[i].execute(...)` 直接调用 `AgentTool::execute`，不经过
    // `executeTool` 的嵌套调用管线；权限层必须在这里补上，否则 read-only / Deny 规则
    // 会被扩展发起的直接执行绕过。
    let permission = crate::runner::check_tool_permission(&name, &params);
    // 任务局部 cwd 覆盖不会跨 `spawn_local` 传播（隔离子代理直接调用
    // `ctx.tools[i].execute` 时会走到这里）：在派生点取一次，子任务里重新包起来。
    let cwd_override = crate::runner::current_cwd_override();
    tokio::task::spawn_local(async move {
        let run = async move {
            if let Some(check) = permission
                && let Some(reason) = check.await
            {
                sender.reject_message(id, reason);
                if let Some(bridge) = &bridge {
                    bridge.release(on_update_id);
                }
                return;
            }
            let result = tool
                .execute
                .execute(&tool_call_id, params, signal.as_ref(), on_update)
                .await;
            match result {
                Ok(result) => match serde_json::to_value(&result) {
                    Ok(value) => {
                        sender.resolve(id, HostValue::Json(value));
                    }
                    Err(error) => {
                        sender.reject_message(
                            id,
                            format!("failed to serialize tool result: {error}"),
                        );
                    }
                },
                Err(error) => {
                    sender.reject(
                        id,
                        HostValue::Error {
                            name: error.name.unwrap_or_else(|| "Error".to_owned()),
                            message: error.message,
                        },
                    );
                }
            }
            if let Some(bridge) = &bridge {
                bridge.release(on_update_id);
            }
        };
        match cwd_override {
            Some(cwd) => crate::runner::with_cwd_override(cwd, run).await,
            None => run.await,
        }
    });
}

// 通用上下文操作（waitForIdle/newSession/fork/navigateTree/switchSession/compact/reload/
// sendMessage/sendUserMessage）。任务体在引擎线程的 LocalSet 上运行；公共步骤是：
// 取上下文 → 捕获 ui/modelRegistry（仅作 `withSession` 未登记时的兜底）→ 执行宿主动作 →
// 结算 Promise。
//
// `newSession`/`fork`/`switchSession` 在宿主动作返回后还要检查 `callback_errors`：JS 回调
// 抛错时用它的文案 reject 命令 Promise（即使宿主动作本身成功，与 TS 的 `await` 语义一致）。
// `compact` 只表示宿主是否受理（完成/失败走 onComplete/onError），因此不等压缩就 resolve。
fn spawn_context_op(
    request: ContextRequest,
    sender: crate::engine::HostCompletionSender,
    id: u64,
    bridge: Option<CallbackBridge>,
) {
    tokio::task::spawn_local(async move {
        let Some(context) = resolve_context(request.handle) else {
            release_payload_callbacks(&bridge, &request.args);
            sender.reject_message(id, "context is no longer available");
            return;
        };
        let host = Arc::clone(context.host());
        // 新会话上下文里的 `ui`/`modelRegistry` 是 runner 的构造期字段（TS 同名字段只在
        // 构造函数里赋值），替换前捕获（仅作为未登记 replaced ctx 的兜底）。
        let ui = host.ui();
        let registry = host.model_registry();
        // `WithSessionFn` / `SetupFn` 没有错误通道：回调抛错写进这个槽，宿主动作返回后
        // 由这里 reject（TS 的 `await withSession(...)` 会把 rejection 传给命令调用方）。
        let callback_errors: CallbackErrorSlot = Arc::new(Mutex::new(None));
        match request.op.as_str() {
            "waitForIdle" => {
                host.wait_for_idle().await;
                sender.resolve(id, HostValue::Undefined);
            }
            "newSession" => {
                let options = parse_new_session_options(
                    &request.args,
                    bridge.as_ref(),
                    &ui,
                    &registry,
                    &callback_errors,
                );
                let result = host.new_session(options).await;
                if let Some(message) = lock(&callback_errors).take() {
                    sender.reject_message(id, message);
                } else {
                    resolve_json(&sender, id, &result);
                }
            }
            "fork" => {
                let entry_id = field_str(&request.args, "entryId").unwrap_or_default();
                let options = parse_fork_options(
                    &request.args,
                    bridge.as_ref(),
                    &ui,
                    &registry,
                    &callback_errors,
                );
                let result = host.fork(entry_id, options).await;
                if let Some(message) = lock(&callback_errors).take() {
                    sender.reject_message(id, message);
                } else {
                    resolve_json(&sender, id, &result);
                }
            }
            "navigateTree" => {
                let target_id = field_str(&request.args, "targetId").unwrap_or_default();
                let options = parse_navigate_tree_options(&request.args);
                let result = host.navigate_tree(target_id, options).await;
                resolve_json(&sender, id, &result);
            }
            "switchSession" => {
                let session_path = field_str(&request.args, "sessionPath").unwrap_or_default();
                let options = parse_switch_session_options(
                    &request.args,
                    bridge.as_ref(),
                    &ui,
                    &registry,
                    &callback_errors,
                );
                let result = host.switch_session(session_path, options).await;
                if let Some(message) = lock(&callback_errors).take() {
                    sender.reject_message(id, message);
                } else {
                    resolve_json(&sender, id, &result);
                }
            }
            "compact" => {
                let custom_instructions = field_str(&request.args, "customInstructions");
                let on_complete_id = field_u64(&request.args, "onComplete");
                let on_error_id = field_u64(&request.args, "onError");
                let has_callback = on_complete_id != 0 || on_error_id != 0;
                let options = if custom_instructions.is_none() && !has_callback {
                    // TS 的 `ctx.compact()` 把 `undefined` 交给 compactFn。
                    None
                } else {
                    // TS 里 `onComplete` / `onError` 只会有一个被调用；这里谁先到就同时释放
                    // 两个 JS 表项。宿主丢弃 options（永不回调）时由 Drop 守卫释放。
                    let callbacks =
                        CompactCallbacks::new(bridge.clone(), on_complete_id, on_error_id);
                    Some(CompactOptions {
                        custom_instructions,
                        on_complete: match (&bridge, on_complete_id) {
                            (Some(bridge), 1..) => {
                                let bridge = bridge.clone();
                                let callbacks = Arc::clone(&callbacks);
                                Some(Arc::new(move |result| {
                                    let value =
                                        serde_json::to_value(&result).unwrap_or(JsonValue::Null);
                                    bridge.fire(on_complete_id, JsonValue::Array(vec![value]));
                                    callbacks.release();
                                }))
                            }
                            _ => None,
                        },
                        on_error: match (&bridge, on_error_id) {
                            (Some(bridge), 1..) => {
                                let bridge = bridge.clone();
                                let callbacks = Arc::clone(&callbacks);
                                Some(Arc::new(
                                    move |error: Box<dyn std::error::Error + Send + Sync>| {
                                        bridge.fire(
                                            on_error_id,
                                            JsonValue::Array(vec![serde_json::json!({
                                                "__kianaError": error.to_string()
                                            })]),
                                        );
                                        callbacks.release();
                                    },
                                ))
                            }
                            _ => None,
                        },
                    })
                };
                host.compact(options);
                sender.resolve(id, HostValue::Undefined);
            }
            "reload" => {
                host.reload().await;
                sender.resolve(id, HostValue::Undefined);
            }
            "sendMessage" => {
                let Some(actions) = context.replaced_actions().cloned() else {
                    sender.reject_message(
                        id,
                        "ctx.sendMessage is only available on a replaced session context",
                    );
                    return;
                };
                let message = match parse_send_message_options(&request.args) {
                    Ok(message) => message,
                    Err(error) => {
                        sender.reject_message(id, error);
                        return;
                    }
                };
                match actions.send_message(message).await {
                    Ok(()) => sender.resolve(id, HostValue::Undefined),
                    // TS：`context.sendMessage` 就是会话的 `sendCustomMessage`，失败 reject。
                    Err(error) => sender.reject_message(id, error),
                };
            }
            "sendUserMessage" => {
                let Some(actions) = context.replaced_actions().cloned() else {
                    sender.reject_message(
                        id,
                        "ctx.sendUserMessage is only available on a replaced session context",
                    );
                    return;
                };
                let content: UserContent =
                    match parse_arg(&nested(&request.args, "value"), "sendUserMessage content") {
                        Ok(content) => content,
                        Err(error) => {
                            sender.reject_message(id, error);
                            return;
                        }
                    };
                let options = parse_send_user_message_options(&request.args);
                match actions.send_user_message(content, options).await {
                    Ok(()) => sender.resolve(id, HostValue::Undefined),
                    Err(error) => sender.reject_message(id, error),
                };
            }
            other => {
                sender.reject_message(id, format!("unknown context operation: {other}"));
            }
        }
    });
}

// `ctx.modelRegistry` 的异步成员（认证、refresh、complete/classify/generateImages、
// getAvailableOfType、stream/streamSimple）。注册表句柄是 `Weak`：JS 侧临时对象被回收后
// 调用会得到 `model registry is no longer available`。
//
// `stream`/`streamSimple` 在这里只创建 Rust 事件流、登记 id 并把它 resolve 给 JS；后续
// 拉取/结果/关闭走 `spawn_stream_op`（那三个 op 不需要注册表句柄）。
fn spawn_model_registry_op(
    request: ContextRequest,
    sender: crate::engine::HostCompletionSender,
    id: u64,
) {
    tokio::task::spawn_local(async move {
        let Some(registry) = resolve_model_registry(request.handle) else {
            sender.reject_message(id, "model registry is no longer available");
            return;
        };
        match request.op.as_str() {
            "modelRegistry.getApiKeyAndHeaders" => {
                let model: Model = match parse_arg(&nested(&request.args, "model"), "model") {
                    Ok(model) => model,
                    Err(error) => {
                        sender.reject_message(id, error);
                        return;
                    }
                };
                let auth = registry.get_api_key_and_headers(&model).await;
                sender.resolve(id, HostValue::Json(resolved_auth_to_json(&auth)));
            }
            "modelRegistry.getProviderAuth" => {
                let provider = field_str(&request.args, "provider").unwrap_or_default();
                match registry.get_provider_auth(&provider).await {
                    Ok(auth) => match auth {
                        Some(auth) => resolve_json(&sender, id, &auth),
                        None => {
                            sender.resolve(id, HostValue::Undefined);
                        }
                    },
                    Err(error) => {
                        sender.reject_message(id, error.message);
                    }
                }
            }
            "modelRegistry.getApiKeyForProvider" => {
                let provider = field_str(&request.args, "provider").unwrap_or_default();
                match registry.get_api_key_for_provider(&provider).await {
                    Some(api_key) => {
                        sender.resolve(id, HostValue::String(api_key));
                    }
                    None => {
                        sender.resolve(id, HostValue::Undefined);
                    }
                }
            }
            "modelRegistry.refresh" => {
                let options = kiana_ai::models::ModelsRefreshOptions {
                    allow_network: field_bool(&request.args, "allowNetwork"),
                    providers: request
                        .args
                        .get("providers")
                        .and_then(|value| serde_json::from_value(value.clone()).ok()),
                    force: field_bool(&request.args, "force"),
                    signal: request_signal(&request.args).map(AbortScope::from),
                };
                match registry.refresh(options).await {
                    Ok(result) => {
                        let mut errors = JsonObject::new();
                        for (provider, error) in &result.errors {
                            errors
                                .insert(provider.clone(), JsonValue::String(error.message.clone()));
                        }
                        let mut object = JsonObject::new();
                        object.insert("aborted".to_owned(), JsonValue::Bool(result.aborted));
                        object.insert("errors".to_owned(), object_value(errors));
                        sender.resolve(id, HostValue::Json(object_value(object)));
                    }
                    Err(error) => {
                        sender.reject_message(id, error.message);
                    }
                }
            }
            "modelRegistry.complete" => {
                let model: Model = match parse_arg(&nested(&request.args, "model"), "model") {
                    Ok(model) => model,
                    Err(error) => {
                        sender.reject_message(id, error);
                        return;
                    }
                };
                let context: kiana_ai::types::Context =
                    match parse_arg(&nested(&request.args, "context"), "context") {
                        Ok(context) => context,
                        Err(error) => {
                            sender.reject_message(id, error);
                            return;
                        }
                    };
                let options = parse_stream_options(
                    &nested(&request.args, "options"),
                    request_signal(&request.args),
                );
                let message = registry.complete(&model, &context, options).await;
                resolve_json(&sender, id, &message);
            }
            "modelRegistry.classify" => {
                let model: Model = match parse_arg(&nested(&request.args, "model"), "model") {
                    Ok(model) => model,
                    Err(error) => {
                        sender.reject_message(id, error);
                        return;
                    }
                };
                let context: kiana_ai::types::ClassifierContext =
                    match parse_arg(&nested(&request.args, "context"), "classifier context") {
                        Ok(context) => context,
                        Err(error) => {
                            sender.reject_message(id, error);
                            return;
                        }
                    };
                let stream = parse_stream_options(
                    &nested(&request.args, "options"),
                    request_signal(&request.args),
                );
                let options = kiana_ai::models::ModelsClassifierOptions {
                    base: stream.base,
                    ..Default::default()
                };
                let result = registry.classify(&model, &context, options).await;
                resolve_json(&sender, id, &result);
            }
            "modelRegistry.generateImages" => {
                let model: Model = match parse_arg(&nested(&request.args, "model"), "model") {
                    Ok(model) => model,
                    Err(error) => {
                        sender.reject_message(id, error);
                        return;
                    }
                };
                let context: kiana_ai::types::ImagesContext =
                    match parse_arg(&nested(&request.args, "context"), "images context") {
                        Ok(context) => context,
                        Err(error) => {
                            sender.reject_message(id, error);
                            return;
                        }
                    };
                let stream = parse_stream_options(
                    &nested(&request.args, "options"),
                    request_signal(&request.args),
                );
                let options = kiana_ai::models::ModelsImagesOptions {
                    base: stream.base,
                    ..Default::default()
                };
                let result = registry.generate_images(&model, &context, options).await;
                resolve_json(&sender, id, &result);
            }
            "modelRegistry.getAvailableOfType" => {
                let model_type: ModelType =
                    match parse_arg(&nested(&request.args, "type"), "model type") {
                        Ok(model_type) => model_type,
                        Err(error) => {
                            sender.reject_message(id, error);
                            return;
                        }
                    };
                let provider = field_str(&request.args, "provider");
                let options = kiana_ai::auth::types::AuthOperationOptions {
                    signal: request_signal(&request.args).map(AbortScope::from),
                };
                match registry
                    .get_available_of_type(model_type, provider.as_deref(), options)
                    .await
                {
                    Ok(models) => resolve_json(&sender, id, &models),
                    Err(error) => {
                        sender.reject_message(id, error.message);
                    }
                }
            }
            "modelRegistry.stream" | "modelRegistry.streamSimple" => {
                let model: Model = match parse_arg(&nested(&request.args, "model"), "model") {
                    Ok(model) => model,
                    Err(error) => {
                        sender.reject_message(id, error);
                        return;
                    }
                };
                let context: Context = match parse_arg(&nested(&request.args, "context"), "context")
                {
                    Ok(context) => context,
                    Err(error) => {
                        sender.reject_message(id, error);
                        return;
                    }
                };
                let signal = request_signal(&request.args);
                let stream = if request.op == "modelRegistry.stream" {
                    registry.stream(
                        &model,
                        &context,
                        parse_stream_options(&nested(&request.args, "options"), signal),
                    )
                } else {
                    registry.stream_simple(
                        &model,
                        &context,
                        parse_simple_stream_options(&nested(&request.args, "options"), signal),
                    )
                };
                let stream_id = register_stream(stream);
                sender.resolve(id, HostValue::Number(stream_id as f64));
            }
            other => {
                sender.reject_message(id, format!("unknown model registry operation: {other}"));
            }
        }
    });
}

/// 事件流的拉取 / 结果 / 关闭（不依赖模型注册表句柄：`ctx.modelRegistry` 可能是临时对象）。
///
/// `streamNext` 用每个 slot 自己的 tokio `Mutex` 串行化并发拉取，保证事件按到达顺序交付；
/// `streamResult` 等待最终消息（`Ok` → resolve，`Err` → reject）；`streamClose`（以及任何
/// 未匹配的 op）从表里移除流，重复关闭无副作用。流 id 未知时拉取/结果都 reject。
fn spawn_stream_op(request: ContextRequest, sender: crate::engine::HostCompletionSender, id: u64) {
    tokio::task::spawn_local(async move {
        let stream_id = field_u64(&request.args, "stream");
        match request.op.as_str() {
            "modelRegistry.streamNext" => {
                let Some(slot) = resolve_stream(stream_id) else {
                    sender.reject_message(id, "event stream is no longer available");
                    return;
                };
                // 并发拉取按到达顺序串行化（TS 的迭代器同样是有状态的）。
                let _guard = slot.next.lock().await;
                match slot.stream.recv().await {
                    Some(event) => {
                        let mut object = JsonObject::new();
                        object.insert("done".to_owned(), JsonValue::Bool(false));
                        object.insert(
                            "value".to_owned(),
                            serde_json::to_value(&event).unwrap_or(JsonValue::Null),
                        );
                        sender.resolve(id, HostValue::Json(object_value(object)));
                    }
                    None => {
                        let mut object = JsonObject::new();
                        object.insert("done".to_owned(), JsonValue::Bool(true));
                        sender.resolve(id, HostValue::Json(object_value(object)));
                    }
                }
            }
            "modelRegistry.streamResult" => {
                let Some(slot) = resolve_stream(stream_id) else {
                    sender.reject_message(id, "event stream is no longer available");
                    return;
                };
                match slot.stream.result().await {
                    Ok(message) => resolve_json(&sender, id, &message),
                    Err(error) => {
                        sender.reject_message(id, error.to_string());
                    }
                }
            }
            _ => {
                unregister_stream(stream_id);
                sender.resolve(id, HostValue::Undefined);
            }
        }
    });
}

/// `ResolvedRequestAuth` → TS 的 `{ ok: true, apiKey?, headers?, baseUrl?, env? }` /
/// `{ ok: false, error }`（`undefined` 字段被省略）。
fn resolved_auth_to_json(auth: &kiana_config::models::registry::ResolvedRequestAuth) -> JsonValue {
    let mut object = JsonObject::new();
    match auth {
        kiana_config::models::registry::ResolvedRequestAuth::Ok(auth) => {
            object.insert("ok".to_owned(), JsonValue::Bool(true));
            if let Some(api_key) = &auth.api_key {
                object.insert("apiKey".to_owned(), JsonValue::String(api_key.clone()));
            }
            if let Some(headers) = &auth.headers
                && let Ok(value) = serde_json::to_value(headers)
            {
                object.insert("headers".to_owned(), value);
            }
            if let Some(base_url) = &auth.base_url {
                object.insert("baseUrl".to_owned(), JsonValue::String(base_url.clone()));
            }
            if let Some(env) = &auth.env
                && let Ok(value) = serde_json::to_value(env)
            {
                object.insert("env".to_owned(), value);
            }
        }
        kiana_config::models::registry::ResolvedRequestAuth::Err { error } => {
            object.insert("ok".to_owned(), JsonValue::Bool(false));
            object.insert("error".to_owned(), JsonValue::String(error.clone()));
        }
    }
    object_value(object)
}

// ===========================================================================
// 只读会话管理器桥（`ctx.sessionManager`，15 个方法）
// ===========================================================================

/// `ctx.sessionManager` 的底层来源。
enum SessionManagerSource {
    /// runner 的只读管理器（`ctx.sessionManager`）。
    Readonly(Arc<dyn ReadonlySessionManager>),
    /// `newSession({ setup })` 收到的可写管理器（JS 只暴露 15 个只读成员；可写桥属于
    /// session 单元）。
    Setup(Arc<Mutex<kiana_session_core::session_manager::SessionManager>>),
}

impl SessionManagerSource {
    /// 在管理器上执行 `read`，返回值必须已经脱离借用（JS 侧返回拥有型值）。
    fn with<R>(&self, read: impl FnOnce(&dyn ReadonlySessionManager) -> R) -> R {
        match self {
            SessionManagerSource::Readonly(manager) => read(&**manager),
            SessionManagerSource::Setup(manager) => {
                let guard = lock(manager);
                read(&*guard)
            }
        }
    }
}

/// JS 可见的只读会话管理器（`session-manager.ts:245-262` 的 15 个方法）。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ReadonlySessionManager")]
pub struct JsSessionManager {
    #[qjs(skip_trace)]
    source: SessionManagerSource,
}

/// `SessionEntry` → JS 值（序列化失败时落为 `null`，与其它 `unwrap_or(JsonValue::Null)` 一致）。
fn entry_json(entry: &SessionEntry) -> JsonValue {
    serde_json::to_value(entry).unwrap_or(JsonValue::Null)
}

/// 会话树节点 → JS `{ entry, children, label?, labelTimestamp? }`（`children` 递归）。
fn tree_node_json(node: &kiana_session_core::session_manager::SessionTreeNode) -> JsonValue {
    let mut object = JsonObject::new();
    object.insert("entry".to_owned(), entry_json(&node.entry));
    object.insert(
        "children".to_owned(),
        JsonValue::Array(node.children.iter().map(tree_node_json).collect()),
    );
    if let Some(label) = &node.label {
        object.insert("label".to_owned(), JsonValue::String(label.clone()));
    }
    if let Some(timestamp) = &node.label_timestamp {
        object.insert(
            "labelTimestamp".to_owned(),
            JsonValue::String(timestamp.clone()),
        );
    }
    object_value(object)
}

/// `SessionProjection` → TS `buildSessionProjection()` 的返回形状：
/// `{ entries: [{ sourceEntry, messages }], messages, thinkingLevel, model }`，
/// 其中 `model` 是 `{ provider, modelId }` 或 `null`。
fn projection_json(
    projection: &kiana_session_core::session_manager::SessionProjection,
) -> JsonValue {
    let mut object = JsonObject::new();
    let entries: Vec<JsonValue> = projection
        .entries
        .iter()
        .map(|entry| {
            let mut item = JsonObject::new();
            item.insert("sourceEntry".to_owned(), entry_json(&entry.source_entry));
            item.insert(
                "messages".to_owned(),
                JsonValue::Array(
                    entry
                        .messages
                        .iter()
                        .map(|message| serde_json::to_value(message).unwrap_or(JsonValue::Null))
                        .collect(),
                ),
            );
            object_value(item)
        })
        .collect();
    object.insert("entries".to_owned(), JsonValue::Array(entries));
    object.insert(
        "messages".to_owned(),
        JsonValue::Array(
            projection
                .messages
                .iter()
                .map(|message| serde_json::to_value(message).unwrap_or(JsonValue::Null))
                .collect(),
        ),
    );
    object.insert(
        "thinkingLevel".to_owned(),
        JsonValue::String(projection.thinking_level.clone()),
    );
    object.insert(
        "model".to_owned(),
        match &projection.model {
            Some(model) => {
                let mut item = JsonObject::new();
                item.insert(
                    "provider".to_owned(),
                    JsonValue::String(model.provider.clone()),
                );
                item.insert(
                    "modelId".to_owned(),
                    JsonValue::String(model.model_id.clone()),
                );
                object_value(item)
            }
            None => JsonValue::Null,
        },
    );
    object_value(object)
}

#[rquickjs::methods]
impl JsSessionManager {
    /// `getCwd()`。
    #[qjs(rename = "getCwd")]
    fn get_cwd(&self) -> String {
        self.source.with(|manager| manager.get_cwd().to_owned())
    }

    /// `getSessionDir()`。
    #[qjs(rename = "getSessionDir")]
    fn get_session_dir(&self) -> String {
        self.source
            .with(|manager| manager.get_session_dir().to_owned())
    }

    /// `getSessionId()`。
    #[qjs(rename = "getSessionId")]
    fn get_session_id(&self) -> String {
        self.source
            .with(|manager| manager.get_session_id().to_owned())
    }

    /// `getSessionFile()`。
    #[qjs(rename = "getSessionFile")]
    fn get_session_file(&self) -> Option<String> {
        self.source
            .with(|manager| manager.get_session_file().map(str::to_owned))
    }

    /// `getLeafId()`（TS: `string | null`，空会话是 `null` 而不是 `undefined`）。
    #[qjs(rename = "getLeafId")]
    fn get_leaf_id<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        match self
            .source
            .with(|manager| manager.get_leaf_id().map(str::to_owned))
        {
            Some(id) => Ok(rquickjs::String::from_str(ctx.clone(), &id)?.into_value()),
            None => Ok(Value::new_null(ctx)),
        }
    }

    /// `getLeafEntry()`。
    #[qjs(rename = "getLeafEntry")]
    fn get_leaf_entry<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        match self
            .source
            .with(|manager| manager.get_leaf_entry().cloned())
        {
            Some(entry) => json_value(&ctx, &entry_json(&entry)),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `getEntry(id)`。
    #[qjs(rename = "getEntry")]
    fn get_entry<'js>(&self, ctx: Ctx<'js>, id: String) -> EngineResult<Value<'js>> {
        match self.source.with(|manager| manager.get_entry(&id).cloned()) {
            Some(entry) => json_value(&ctx, &entry_json(&entry)),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `getLabel(id)`。
    #[qjs(rename = "getLabel")]
    fn get_label(&self, id: String) -> Option<String> {
        self.source
            .with(|manager| manager.get_label(&id).map(str::to_owned))
    }

    /// `getBranch(fromId?)`。
    ///
    /// TS: `getBranch(fromId?: string)` 用 `fromId ?? this.leafId`，所以显式
    /// `undefined`/`null` 与省略等价（`Opt<String>` 会把它们变成转换错误）。
    #[qjs(rename = "getBranch")]
    fn get_branch<'js>(&self, ctx: Ctx<'js>, from_id: Opt<Value<'js>>) -> EngineResult<Value<'js>> {
        let from_id = match from_id.0.as_ref() {
            Some(value) if !value.is_undefined() && !value.is_null() => {
                Some(value.get::<String>()?)
            }
            _ => None,
        };
        let entries = self
            .source
            .with(|manager| manager.get_branch(from_id.as_deref()));
        let values: Vec<JsonValue> = entries.iter().map(entry_json).collect();
        json_value(&ctx, &JsonValue::Array(values))
    }

    /// `buildContextEntries()`。
    #[qjs(rename = "buildContextEntries")]
    fn build_context_entries<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let entries = self.source.with(|manager| manager.build_context_entries());
        let values: Vec<JsonValue> = entries.iter().map(entry_json).collect();
        json_value(&ctx, &JsonValue::Array(values))
    }

    /// `buildSessionProjection()`。
    #[qjs(rename = "buildSessionProjection")]
    fn build_session_projection<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let projection = self
            .source
            .with(|manager| manager.build_session_projection());
        json_value(&ctx, &projection_json(&projection))
    }

    /// `getHeader()`（TS: `SessionHeader | null`）。
    #[qjs(rename = "getHeader")]
    fn get_header<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        match self.source.with(|manager| manager.get_header().cloned()) {
            Some(header) => {
                let value = serde_json::to_value(&header).unwrap_or(JsonValue::Null);
                json_value(&ctx, &value)
            }
            None => Ok(Value::new_null(ctx)),
        }
    }

    /// `getEntries()`。
    #[qjs(rename = "getEntries")]
    fn get_entries<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let entries = self.source.with(|manager| manager.get_entries());
        let values: Vec<JsonValue> = entries.iter().map(entry_json).collect();
        json_value(&ctx, &JsonValue::Array(values))
    }

    /// `getTree()`。
    #[qjs(rename = "getTree")]
    fn get_tree<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let tree = self.source.with(|manager| manager.get_tree());
        let values: Vec<JsonValue> = tree.iter().map(tree_node_json).collect();
        json_value(&ctx, &JsonValue::Array(values))
    }

    /// `getSessionName()`。
    #[qjs(rename = "getSessionName")]
    fn get_session_name(&self) -> Option<String> {
        self.source.with(|manager| manager.get_session_name())
    }
}

// ===========================================================================
// 模型注册表桥（`ctx.modelRegistry`）
// ===========================================================================
//
// 同步的数据方法直接回调 [`ModelRegistry`]；需要 I/O 的方法（`getApiKeyAndHeaders`、
// `complete`、`refresh`…）走 `__kiana.promise` + host kind。`stream`/`streamSimple` 需要
// `AssistantMessageEventStream` 的 JS 事件流桥（属 `ai-*` 的 JS 绑定），本单元明确报错，
// 见模块文档的剩余缺口。

/// JS 可见的 `ModelRegistry` 门面。
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "ModelRegistry")]
pub struct JsModelRegistry {
    #[qjs(skip_trace)]
    registry: Arc<ModelRegistry>,
    #[qjs(skip_trace)]
    handle: u64,
    /// 虚拟模型 `route(request, ctx)` 收到的扩展上下文（TS 的 route 第二参）。
    #[qjs(skip_trace)]
    context: Arc<dyn ExtensionContextApi>,
}

impl Drop for JsModelRegistry {
    fn drop(&mut self) {
        unregister_object(self.handle);
    }
}

fn model_to_json(model: &Model) -> JsonValue {
    serde_json::to_value(model).unwrap_or(JsonValue::Null)
}

fn parse_model_value<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<Model> {
    let json = value_to_json(ctx, value)?;
    serde_json::from_value(json)
        .map_err(|error| Exception::throw_type(ctx, &format!("invalid model: {error}")))
}

fn parse_model_type(ctx: &Ctx<'_>, value: &str) -> EngineResult<ModelType> {
    match value {
        "chat" => Ok(ModelType::Chat),
        "image" => Ok(ModelType::Image),
        "classifier" => Ok(ModelType::Classifier),
        other => Err(Exception::throw_type(
            ctx,
            &format!("invalid model type: {other}"),
        )),
    }
}

/// `__kiana_ctx.invoke` 的模型注册表异步操作。
fn registry_promise<'js>(
    ctx: &Ctx<'js>,
    handle: u64,
    op: &str,
    args: JsonValue,
) -> EngineResult<Value<'js>> {
    invoke_promise(ctx, handle, op, json_value(ctx, &args)?)
}

/// `ctx.modelRegistry.getProvider(id)` → JS `Provider` 视图。
///
/// TS 的 `Provider` 是带方法的对象；这里给出 `id`/`name`/`baseUrl`/`headers` 与
/// `getModels()`/`getAllModels()`（`stream`/`streamSimple` 需要 `AssistantMessageEventStream`
/// 的 JS 桥，属 ai 绑定单元，见剩余缺口：调用时明确报错）。
fn provider_to_js<'js>(
    ctx: &Ctx<'js>,
    provider: Arc<dyn kiana_ai::models::provider::Provider>,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("id", provider.id().to_owned())?;
    object.set("name", provider.name().to_owned())?;
    if let Some(base_url) = provider.base_url() {
        object.set("baseUrl", base_url.to_owned())?;
    }
    if let Some(headers) = provider.headers()
        && let Ok(value) = serde_json::to_value(headers)
    {
        object.set("headers", json_value(ctx, &value)?)?;
    }
    let models_provider = Arc::clone(&provider);
    let get_models = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            let models: Vec<JsonValue> = models_provider
                .get_models()
                .iter()
                .map(model_to_json)
                .collect();
            json_value(&ctx, &JsonValue::Array(models))
        },
    )?;
    object.set("getModels", get_models)?;
    let all_provider = Arc::clone(&provider);
    let get_all_models = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            let models: Vec<JsonValue> = all_provider
                .get_all_models()
                .iter()
                .map(|model| model_to_json(model.model()))
                .collect();
            json_value(&ctx, &JsonValue::Array(models))
        },
    )?;
    object.set("getAllModels", get_all_models)?;
    // TS 的 `provider.stream(model, context, options?)` 同步返回 `AssistantMessageEventStream`；
    // 与 `ctx.modelRegistry.stream` 共用同一套 JS 事件流桥（按 id 拉取事件）。
    let stream_provider = Arc::clone(&provider);
    let stream = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>,
              model: Value<'js>,
              context: Value<'js>,
              options: Opt<Value<'js>>|
              -> EngineResult<Value<'js>> {
            let model = parse_model_value(&ctx, model)?;
            let context = parse_transcript_context(&ctx, context)?;
            let (options_json, signal) = dialog_stream_options(&ctx, options)?;
            let stream = stream_provider.stream(
                &model,
                &context,
                parse_stream_options(&options_json, signal).base,
            );
            let id = register_stream(stream);
            event_stream_object(&ctx, json_value(&ctx, &JsonValue::Number(id.into()))?)
        },
    )?;
    object.set("stream", stream)?;
    let simple_provider = Arc::clone(&provider);
    let stream_simple = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>,
              model: Value<'js>,
              context: Value<'js>,
              options: Opt<Value<'js>>|
              -> EngineResult<Value<'js>> {
            let model = parse_model_value(&ctx, model)?;
            let context = parse_transcript_context(&ctx, context)?;
            let (options_json, signal) = dialog_stream_options(&ctx, options)?;
            let stream = simple_provider.stream_simple(
                &model,
                &context,
                parse_simple_stream_options(&options_json, signal).base,
            );
            let id = register_stream(stream);
            event_stream_object(&ctx, json_value(&ctx, &JsonValue::Number(id.into()))?)
        },
    )?;
    object.set("streamSimple", stream_simple)?;
    Ok(object.into_value())
}

/// `kiana-config::ProviderConfigInput` → JS `ProviderConfigInput`（`getRegisteredProviderConfig`）。
///
/// 键序与 TS 接口一致：name、baseUrl、apiKey、api、streamSimple、images、classifiers、
/// headers、authHeader、oauth、models、refreshModels。没有数据字段的键（`undefined`）省略。
/// 回调字段（`streamSimple` 除外）无法从 Rust 结构逆向重建，见方法文档。
fn provider_config_input_to_js<'js>(
    ctx: &Ctx<'js>,
    config: &ProviderConfigInput,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    if let Some(name) = &config.name {
        object.set("name", name.clone())?;
    }
    if let Some(base_url) = &config.base_url {
        object.set("baseUrl", base_url.clone())?;
    }
    if let Some(api_key) = &config.api_key {
        object.set("apiKey", api_key.clone())?;
    }
    if let Some(api) = &config.api {
        object.set("api", api.clone())?;
    }
    if let Some(stream) = &config.stream_simple {
        let stream = Arc::clone(stream);
        let function = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  model: Value<'js>,
                  context: Value<'js>,
                  options: Opt<Value<'js>>|
                  -> EngineResult<Value<'js>> {
                let model = parse_model_value(&ctx, model)?;
                let context = parse_transcript_context(&ctx, context)?;
                let (options_json, signal) = dialog_stream_options(&ctx, options)?;
                let stream = stream(
                    &model,
                    &context,
                    parse_simple_stream_options(&options_json, signal).base,
                );
                let id = register_stream(stream);
                event_stream_object(&ctx, json_value(&ctx, &JsonValue::Number(id.into()))?)
            },
        )?;
        object.set("streamSimple", function)?;
    }
    if let Some(headers) = &config.headers {
        object.set(
            "headers",
            json_value(
                ctx,
                &serde_json::to_value(headers).unwrap_or(JsonValue::Null),
            )?,
        )?;
    }
    if let Some(auth_header) = config.auth_header {
        object.set("authHeader", auth_header)?;
    }
    if let Some(models) = &config.models {
        let values: Vec<JsonValue> = models.iter().map(provider_model_config_to_json).collect();
        object.set("models", json_value(ctx, &JsonValue::Array(values))?)?;
    }
    Ok(object.into_value())
}

/// `kiana-config::ProviderModelConfig` → TS `ProviderModelConfig` 的 JSON 形状。
///
/// 公共字段（`id`/`name`/`api`/`baseUrl`/`input`/`inputLimits`/`cost`/`headers`）对所有种类
/// 输出。`type` 是否输出取决于 `base.declared_type` 是否为 `Some`：image/classifier
/// 总是输出；chat 模型在扩展注册路径（`provider_model_config`，见 runner/mod.rs）
/// 恒写 `Some(ModelType::Chat)`，因此经注册的 chat 配置也总是输出 `"type":"chat"`——
/// 与 TS 省略 `type` 的默认分支写法不同（记录在案的偏差，待主文档核实）。
/// chat/classifier 的 f64 数值经 `Number::from_f64` 转 JSON number，非有限值落为 `null`。
fn provider_model_config_to_json(
    config: &kiana_config::models::composer::ProviderModelConfig,
) -> JsonValue {
    use kiana_config::models::composer::ProviderModelConfig;
    let base = config.base();
    let mut object = serde_json::Map::new();
    match config {
        ProviderModelConfig::Chat(_) => {
            if base.declared_type.is_some() {
                object.insert("type".to_owned(), JsonValue::String("chat".to_owned()));
            }
        }
        ProviderModelConfig::Image(_) => {
            object.insert("type".to_owned(), JsonValue::String("image".to_owned()));
        }
        ProviderModelConfig::Classifier(_) => {
            object.insert(
                "type".to_owned(),
                JsonValue::String("classifier".to_owned()),
            );
        }
    }
    object.insert("id".to_owned(), JsonValue::String(base.id.clone()));
    object.insert("name".to_owned(), JsonValue::String(base.name.clone()));
    if let Some(api) = &base.api {
        object.insert("api".to_owned(), JsonValue::String(api.clone()));
    }
    if let Some(base_url) = &base.base_url {
        object.insert("baseUrl".to_owned(), JsonValue::String(base_url.clone()));
    }
    if let Ok(input) = serde_json::to_value(&base.input) {
        object.insert("input".to_owned(), input);
    }
    if let Some(limits) = &base.input_limits
        && let Ok(value) = serde_json::to_value(limits)
    {
        object.insert("inputLimits".to_owned(), value);
    }
    if let Ok(cost) = serde_json::to_value(&base.cost) {
        object.insert("cost".to_owned(), cost);
    }
    if let Some(headers) = &base.headers
        && let Ok(value) = serde_json::to_value(headers)
    {
        object.insert("headers".to_owned(), value);
    }
    match config {
        ProviderModelConfig::Chat(chat) => {
            object.insert("reasoning".to_owned(), JsonValue::Bool(chat.reasoning));
            if let Some(map) = &chat.thinking_level_map
                && let Ok(value) = serde_json::to_value(map)
            {
                object.insert("thinkingLevelMap".to_owned(), value);
            }
            if let Some(cache) = &chat.prompt_cache
                && let Ok(value) = serde_json::to_value(cache)
            {
                object.insert("promptCache".to_owned(), value);
            }
            object.insert(
                "contextWindow".to_owned(),
                serde_json::Number::from_f64(chat.context_window)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null),
            );
            object.insert(
                "maxTokens".to_owned(),
                serde_json::Number::from_f64(chat.max_tokens)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null),
            );
            if let Some(params) = &chat.sampling_params
                && let Ok(value) = serde_json::to_value(params)
            {
                object.insert("samplingParams".to_owned(), value);
            }
            if let Some(params) = &chat.sampling_params_by_thinking_level
                && let Ok(value) = serde_json::to_value(params)
            {
                object.insert("samplingParamsByThinkingLevel".to_owned(), value);
            }
            if let Some(compat) = &chat.compat {
                object.insert("compat".to_owned(), compat.clone());
            }
        }
        ProviderModelConfig::Image(image) => {
            if let Ok(output) = serde_json::to_value(&image.output) {
                object.insert("output".to_owned(), output);
            }
        }
        ProviderModelConfig::Classifier(classifier) => {
            object.insert(
                "contextWindow".to_owned(),
                serde_json::Number::from_f64(classifier.context_window)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null),
            );
            if let Some(compat) = &classifier.compat {
                object.insert("compat".to_owned(), compat.clone());
            }
        }
    }
    JsonValue::Object(object)
}

/// JS `context` → [`kiana_ai::types::TranscriptContext`]（TS 在调用点同步消费）。
fn parse_transcript_context<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> EngineResult<kiana_ai::types::TranscriptContext> {
    let json = value_to_json(ctx, value)?;
    serde_json::from_value(json)
        .map_err(|error| Exception::throw_type(ctx, &format!("invalid context: {error}")))
}

/// `stream(model, context, options?)` 的可选参数：JSON 与 `signal` 句柄。
fn dialog_stream_options<'js>(
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
) -> EngineResult<(JsonValue, Option<CancellationToken>)> {
    match options.0.as_ref() {
        Some(value) => Ok((
            value_to_json(ctx, value.clone())?,
            option_signal_handle(ctx, Some(value), "signal")?.and_then(resolve_signal_handle),
        )),
        None => Ok((JsonValue::Null, None)),
    }
}

/// JS `StreamOptions`（camelCase）→ [`kiana_ai::types::StreamOptions`]。
///
/// 具名字段按 TS 的字段名逐个映射；其余数据字段进 `extra`（TS 的
/// `ProviderStreamOptions = StreamOptions & Record<string, unknown>` 透传尾部）。函数字段
/// （`onPayload`/`transformHeaders`…）没有 JS 回调桥，保留为缺口。
fn parse_stream_options(
    raw: &JsonValue,
    signal: Option<CancellationToken>,
) -> kiana_ai::models::ModelsStreamOptions {
    let mut options = kiana_ai::models::ModelsStreamOptions::default();
    let base = &mut options.base;
    if let Some(object) = raw.as_object() {
        if let Some(value) = object.get("apiKey").and_then(JsonValue::as_str) {
            base.api_key = Some(value.to_owned());
        }
        if let Some(value) = object.get("env")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            base.env = Some(value);
        }
        if let Some(value) = object.get("headers")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            base.headers = Some(value);
        }
        if let Some(value) = object.get("identity")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            base.identity = Some(value);
        }
        if let Some(value) = object.get("timeoutMs").and_then(JsonValue::as_u64) {
            base.timeout_ms = Some(value);
        }
        if let Some(value) = object.get("maxRetries").and_then(JsonValue::as_u64) {
            base.max_retries = Some(value as u32);
        }
        if let Some(value) = object.get("maxRetryDelayMs").and_then(JsonValue::as_u64) {
            base.max_retry_delay_ms = Some(value);
        }
        if let Some(value) = object.get("temperature").and_then(JsonValue::as_f64) {
            base.temperature = Some(value);
        }
        if let Some(value) = object.get("samplingParams")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            base.sampling_params = Some(value);
        }
        if let Some(value) = object.get("maxTokens").and_then(JsonValue::as_u64) {
            base.max_tokens = Some(value);
        }
        if let Some(value) = object.get("transport")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            base.transport = Some(value);
        }
        if let Some(value) = object.get("cacheRetention")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            base.cache_retention = Some(value);
        }
        if let Some(value) = object.get("sessionId").and_then(JsonValue::as_str) {
            base.session_id = Some(value.to_owned());
        }
        if let Some(value) = object
            .get("websocketConnectTimeoutMs")
            .and_then(JsonValue::as_u64)
        {
            base.websocket_connect_timeout_ms = Some(value);
        }
        if let Some(value) = object.get("metadata")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            base.metadata = Some(value);
        }
        if let Some(value) = object.get("reasoning").and_then(JsonValue::as_str) {
            options.reasoning = Some(value.to_owned());
        }
        // TS 的 `ProviderStreamOptions = StreamOptions & Record<string, unknown>`：具名字段
        // 逐个映射；其余数据字段透传进 `extra`。函数字段没有 JS 回调桥、`signal` 由
        // `CancellationToken` 单独安装，二者都不能进 `extra`。
        const NAMED: &[&str] = &[
            "apiKey",
            "env",
            "headers",
            "identity",
            "timeoutMs",
            "maxRetries",
            "maxRetryDelayMs",
            "temperature",
            "samplingParams",
            "maxTokens",
            "transport",
            "cacheRetention",
            "sessionId",
            "websocketConnectTimeoutMs",
            "metadata",
            "reasoning",
        ];
        const NON_DATA: &[&str] = &[
            "signal",
            "onPayload",
            "onResponse",
            "onProviderStreamEvent",
            "fetch",
            "transformHeaders",
        ];
        for (key, value) in object {
            if NAMED.contains(&key.as_str()) || NON_DATA.contains(&key.as_str()) {
                continue;
            }
            base.extra.insert(key.clone(), value.clone());
        }
    }
    base.signal = signal;
    options
}

/// JS `ModelsSimpleStreamOptions` → [`kiana_ai::models::ModelsSimpleStreamOptions`]。
///
/// `base` 复用 [`parse_stream_options`]（两者共享 `StreamOptions`）；`toolChoice`、
/// `reasoning`、`deferred`、`thinkingBudgets` 按 TS 字段名反序列化。
fn parse_simple_stream_options(
    raw: &JsonValue,
    signal: Option<CancellationToken>,
) -> kiana_ai::models::ModelsSimpleStreamOptions {
    let base = parse_stream_options(raw, signal).base;
    let mut options = kiana_ai::models::ModelsSimpleStreamOptions {
        base: SimpleStreamOptions {
            base,
            ..Default::default()
        },
        ..Default::default()
    };
    if let Some(object) = raw.as_object() {
        if let Some(value) = object.get("toolChoice")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            options.base.tool_choice = Some(value);
        }
        if let Some(value) = object.get("reasoning")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            options.base.reasoning = Some(value);
        }
        if let Some(value) = object.get("deferred")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            options.base.deferred = Some(value);
        }
        if let Some(value) = object.get("thinkingBudgets")
            && let Ok(value) = serde_json::from_value(value.clone())
        {
            options.base.thinking_budgets = Some(value);
        }
        // `thinkingBudgets`/`toolChoice`/`deferred`/`reasoning` 不属于 `StreamOptions` 的
        // `extra`（TS 的 SimpleStreamOptions 把它们放在外层）。
        for key in ["toolChoice", "deferred", "thinkingBudgets"] {
            options.base.base.extra.shift_remove(key);
        }
    }
    options
}

/// `ctx.modelRegistry.refresh()` 的宿主结果 → TS 的 `Map<string, Error>` 形状。
///
/// 宿主 resolve 的是 `{ aborted, errors: { provider: message } }`；`js/context.js` 的
/// `withRefreshMap` 把 `errors` 转成 `Map<string, Error>`（TS 的
/// `ReadonlyMap<string, Error>`）。
fn with_refresh_map<'js>(ctx: &Ctx<'js>, promise: Value<'js>) -> EngineResult<Value<'js>> {
    let kiana: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let map: Function<'js> = kiana.get("withRefreshMap")?;
    map.call((rquickjs::function::This(kiana), promise))
}

#[rquickjs::methods]
impl JsModelRegistry {
    /// `getError()`。
    #[qjs(rename = "getError")]
    fn get_error<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        match self.registry.get_error() {
            Some(error) => Ok(error.into_js(&ctx)?),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `getAll()`。
    #[qjs(rename = "getAll")]
    fn get_all<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let models: Vec<JsonValue> = self.registry.get_all().iter().map(model_to_json).collect();
        json_value(&ctx, &JsonValue::Array(models))
    }

    /// `getAvailable()`。
    #[qjs(rename = "getAvailable")]
    fn get_available<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let models: Vec<JsonValue> = self
            .registry
            .get_available()
            .iter()
            .map(model_to_json)
            .collect();
        json_value(&ctx, &JsonValue::Array(models))
    }

    /// `find(provider, modelId)`。
    #[qjs(rename = "find")]
    fn find<'js>(
        &self,
        ctx: Ctx<'js>,
        provider: String,
        model_id: String,
    ) -> EngineResult<Value<'js>> {
        match self.registry.find(&provider, &model_id) {
            Some(model) => json_value(&ctx, &model_to_json(&model)),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `findOfType(type, provider, modelId)`。
    #[qjs(rename = "findOfType")]
    fn find_of_type<'js>(
        &self,
        ctx: Ctx<'js>,
        model_type: String,
        provider: String,
        model_id: String,
    ) -> EngineResult<Value<'js>> {
        let model_type = parse_model_type(&ctx, &model_type)?;
        match self.registry.find_of_type(model_type, &provider, &model_id) {
            Some(model) => json_value(&ctx, &model_to_json(&model)),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `getModelsOfType(type, provider?)`。
    #[qjs(rename = "getModelsOfType")]
    fn get_models_of_type<'js>(
        &self,
        ctx: Ctx<'js>,
        model_type: String,
        provider: Option<String>,
    ) -> EngineResult<Value<'js>> {
        let model_type = parse_model_type(&ctx, &model_type)?;
        let models: Vec<JsonValue> = self
            .registry
            .get_models_of_type(model_type, provider.as_deref())
            .iter()
            .map(model_to_json)
            .collect();
        json_value(&ctx, &JsonValue::Array(models))
    }

    /// `getModelOfType(type, provider, modelId)`。
    #[qjs(rename = "getModelOfType")]
    fn get_model_of_type<'js>(
        &self,
        ctx: Ctx<'js>,
        model_type: String,
        provider: String,
        model_id: String,
    ) -> EngineResult<Value<'js>> {
        let model_type = parse_model_type(&ctx, &model_type)?;
        match self
            .registry
            .get_model_of_type(model_type, &provider, &model_id)
        {
            Some(model) => json_value(&ctx, &model_to_json(&model)),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `hasConfiguredAuth(model)`。
    #[qjs(rename = "hasConfiguredAuth")]
    fn has_configured_auth<'js>(&self, ctx: Ctx<'js>, model: Value<'js>) -> EngineResult<bool> {
        let model = parse_model_value(&ctx, model)?;
        Ok(self.registry.has_configured_auth(&model))
    }

    /// `getProviderAuthStatus(provider)`。
    #[qjs(rename = "getProviderAuthStatus")]
    fn get_provider_auth_status<'js>(
        &self,
        ctx: Ctx<'js>,
        provider: String,
    ) -> EngineResult<Value<'js>> {
        let status = self.registry.get_provider_auth_status(&provider);
        let value = serde_json::to_value(&status).unwrap_or(JsonValue::Null);
        json_value(&ctx, &value)
    }

    /// `getProviderDisplayName(provider)`。
    #[qjs(rename = "getProviderDisplayName")]
    fn get_provider_display_name(&self, provider: String) -> String {
        self.registry.get_provider_display_name(&provider)
    }

    /// `getProvider(provider)` → JS `Provider`（未知 provider 为 `undefined`）。
    #[qjs(rename = "getProvider")]
    fn get_provider<'js>(&self, ctx: Ctx<'js>, provider: String) -> EngineResult<Value<'js>> {
        match self.registry.get_provider(&provider) {
            Some(provider) => provider_to_js(&ctx, provider),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `isUsingOAuth(model)`。
    #[qjs(rename = "isUsingOAuth")]
    fn is_using_oauth<'js>(&self, ctx: Ctx<'js>, model: Value<'js>) -> EngineResult<bool> {
        let model = parse_model_value(&ctx, model)?;
        Ok(self.registry.is_using_oauth(&model))
    }

    /// `getRegisteredProviderIds()`。
    #[qjs(rename = "getRegisteredProviderIds")]
    fn get_registered_provider_ids<'js>(&self, ctx: Ctx<'js>) -> EngineResult<Value<'js>> {
        let values: Vec<JsonValue> = self
            .registry
            .get_registered_provider_ids()
            .into_iter()
            .map(JsonValue::String)
            .collect();
        json_value(&ctx, &JsonValue::Array(values))
    }

    /// `getApiKeyAndHeaders(model)` → `Promise<{ ok, ... }>`。
    #[qjs(rename = "getApiKeyAndHeaders")]
    fn get_api_key_and_headers<'js>(
        &self,
        ctx: Ctx<'js>,
        model: Value<'js>,
    ) -> EngineResult<Value<'js>> {
        let model = value_to_json(&ctx, model)?;
        let mut args = JsonObject::new();
        args.insert("model".to_owned(), model);
        registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.getApiKeyAndHeaders",
            object_value(args),
        )
    }

    /// `getProviderAuth(provider)` → `Promise<AuthResult | undefined>`。
    #[qjs(rename = "getProviderAuth")]
    fn get_provider_auth<'js>(&self, ctx: Ctx<'js>, provider: String) -> EngineResult<Value<'js>> {
        let mut args = JsonObject::new();
        args.insert("provider".to_owned(), JsonValue::String(provider));
        registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.getProviderAuth",
            object_value(args),
        )
    }

    /// `getApiKeyForProvider(provider)` → `Promise<string | undefined>`。
    #[qjs(rename = "getApiKeyForProvider")]
    fn get_api_key_for_provider<'js>(
        &self,
        ctx: Ctx<'js>,
        provider: String,
    ) -> EngineResult<Value<'js>> {
        let mut args = JsonObject::new();
        args.insert("provider".to_owned(), JsonValue::String(provider));
        registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.getApiKeyForProvider",
            object_value(args),
        )
    }

    /// `refresh(options?)` → `Promise<{ aborted, errors: Map<string, Error> }>`。
    #[qjs(rename = "refresh")]
    fn refresh<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> EngineResult<Value<'js>> {
        let mut args = JsonObject::new();
        if let Some(value) = options.0.as_ref() {
            let json = value_to_json(&ctx, value.clone())?;
            for field in ["allowNetwork", "providers", "force"] {
                if let Some(field_value) = json.get(field) {
                    args.insert(field.to_owned(), field_value.clone());
                }
            }
            if let Some(handle) = option_signal_handle(&ctx, Some(value), "signal")? {
                args.insert("signal".to_owned(), JsonValue::Number(handle.into()));
            }
        }
        let promise = registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.refresh",
            object_value(args),
        )?;
        with_refresh_map(&ctx, promise)
    }

    /// `getAvailableOfType(type, provider?, options?)` → `Promise<Model[]>`。
    #[qjs(rename = "getAvailableOfType")]
    fn get_available_of_type<'js>(
        &self,
        ctx: Ctx<'js>,
        model_type: String,
        provider: Option<String>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let mut args = JsonObject::new();
        args.insert("type".to_owned(), JsonValue::String(model_type));
        if let Some(provider) = provider {
            args.insert("provider".to_owned(), JsonValue::String(provider));
        }
        if let Some(value) = options.0.as_ref()
            && let Some(handle) = option_signal_handle(&ctx, Some(value), "signal")?
        {
            args.insert("signal".to_owned(), JsonValue::Number(handle.into()));
        }
        registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.getAvailableOfType",
            object_value(args),
        )
    }

    /// `complete(model, context, options?)` → `Promise<AssistantMessage>`。
    #[qjs(rename = "complete")]
    fn complete<'js>(
        &self,
        ctx: Ctx<'js>,
        model: Value<'js>,
        context: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let model = value_to_json(&ctx, model)?;
        let context = value_to_json(&ctx, context)?;
        let options_json = match options.0.as_ref() {
            Some(value) => value_to_json(&ctx, value.clone())?,
            None => JsonValue::Null,
        };
        let signal = match options.0.as_ref() {
            Some(value) => option_signal_handle(&ctx, Some(value), "signal")?,
            None => None,
        };
        let mut args = JsonObject::new();
        args.insert("model".to_owned(), model);
        args.insert("context".to_owned(), context);
        args.insert("options".to_owned(), options_json);
        if let Some(handle) = signal {
            args.insert("signal".to_owned(), JsonValue::Number(handle.into()));
        }
        registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.complete",
            object_value(args),
        )
    }

    /// `classify(model, context, options?)` → `Promise<ClassifierResult>`。
    ///
    /// 注册表层的失败（非 classifier 模型、provider 无分类能力等）编码在结果里、不 reject；
    /// 但宿主解析 model/context 或序列化结果失败时，这个 Promise 仍会 reject。
    #[qjs(rename = "classify")]
    fn classify<'js>(
        &self,
        ctx: Ctx<'js>,
        model: Value<'js>,
        context: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let model = value_to_json(&ctx, model)?;
        let context = value_to_json(&ctx, context)?;
        let options_json = match options.0.as_ref() {
            Some(value) => value_to_json(&ctx, value.clone())?,
            None => JsonValue::Null,
        };
        let signal = match options.0.as_ref() {
            Some(value) => option_signal_handle(&ctx, Some(value), "signal")?,
            None => None,
        };
        let mut args = JsonObject::new();
        args.insert("model".to_owned(), model);
        args.insert("context".to_owned(), context);
        args.insert("options".to_owned(), options_json);
        if let Some(handle) = signal {
            args.insert("signal".to_owned(), JsonValue::Number(handle.into()));
        }
        registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.classify",
            object_value(args),
        )
    }

    /// `generateImages(model, context, options?)` → `Promise<AssistantImages>`。
    ///
    /// 注册表层的失败（非图片模型、provider 无图像能力等）编码在结果里、不 reject；但
    /// 宿主解析 model/context 或序列化结果失败时，这个 Promise 仍会 reject。
    #[qjs(rename = "generateImages")]
    fn generate_images<'js>(
        &self,
        ctx: Ctx<'js>,
        model: Value<'js>,
        context: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let model = value_to_json(&ctx, model)?;
        let context = value_to_json(&ctx, context)?;
        let options_json = match options.0.as_ref() {
            Some(value) => value_to_json(&ctx, value.clone())?,
            None => JsonValue::Null,
        };
        let signal = match options.0.as_ref() {
            Some(value) => option_signal_handle(&ctx, Some(value), "signal")?,
            None => None,
        };
        let mut args = JsonObject::new();
        args.insert("model".to_owned(), model);
        args.insert("context".to_owned(), context);
        args.insert("options".to_owned(), options_json);
        if let Some(handle) = signal {
            args.insert("signal".to_owned(), JsonValue::Number(handle.into()));
        }
        registry_promise(
            &ctx,
            self.handle,
            "modelRegistry.generateImages",
            object_value(args),
        )
    }

    /// `stream(model, context, options?)` → `AssistantMessageEventStream`。
    ///
    /// model/context 在调用点同步校验（TS 的 `runtime.stream` 同步失败）；事件流自身在
    /// 宿主任务里创建，JS 外观按 `streamNext` 拉取事件。
    #[qjs(rename = "stream")]
    fn stream<'js>(
        &self,
        ctx: Ctx<'js>,
        model: Value<'js>,
        context: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let promise = self.stream_promise(&ctx, "modelRegistry.stream", model, context, options)?;
        event_stream_object(&ctx, promise)
    }

    /// `streamSimple(model, context, options?)` → `AssistantMessageEventStream`。
    #[qjs(rename = "streamSimple")]
    fn stream_simple<'js>(
        &self,
        ctx: Ctx<'js>,
        model: Value<'js>,
        context: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let promise =
            self.stream_promise(&ctx, "modelRegistry.streamSimple", model, context, options)?;
        event_stream_object(&ctx, promise)
    }

    /// `registerProvider(provider)` / `registerProvider(name, config)`。
    ///
    /// TS 的两个重载按 `typeof providerOrName === "string"` 分派；配置里的 JS 回调
    /// （`streamSimple`/`refreshModels`/`images`/`classifiers`/`oauth`）走与 `kiana.registerProvider`
    /// 相同的 JS 桥。
    #[qjs(rename = "registerProvider")]
    fn register_provider<'js>(
        &self,
        ctx: Ctx<'js>,
        provider_or_name: Value<'js>,
        config: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        match provider_or_name.as_string() {
            Some(name) => {
                let name = name.to_string()?;
                let Some(config) = config
                    .0
                    .as_ref()
                    .filter(|value| !value.is_undefined() && !value.is_null())
                    .cloned()
                else {
                    return Err(Exception::throw_message(
                        &ctx,
                        "Provider config is required when registering by name",
                    ));
                };
                let (config, models_error) = crate::loader::provider_config_from_js(&ctx, config)?;
                if let Some(message) = models_error {
                    return Err(Exception::throw_message(&ctx, &message));
                }
                let input = crate::runner::provider_config_input(&config);
                self.registry
                    .register_provider(&name, Some(input))
                    .map_err(|error| Exception::throw_message(&ctx, &error.message()))?;
            }
            None => {
                let provider = crate::loader::native_provider_from_js(&ctx, provider_or_name)?;
                self.registry
                    .register_native_provider(provider)
                    .map_err(|error| Exception::throw_message(&ctx, &error.message()))?;
            }
        }
        Ok(undefined(&ctx))
    }

    /// `unregisterProvider(providerName)`。
    #[qjs(rename = "unregisterProvider")]
    fn unregister_provider(&self, provider_name: String) {
        self.registry.unregister_provider(&provider_name);
    }

    /// `registerVirtualModel(definition)`。
    ///
    /// TS 的 `route(request, ctx)` 在路由时收到扩展上下文；这里捕获当前上下文并在每次
    /// 路由时传给 JS 回调（与 `kiana.registerVirtualModel` 的语义一致）。
    #[qjs(rename = "registerVirtualModel")]
    fn register_virtual_model<'js>(
        &self,
        ctx: Ctx<'js>,
        model: Value<'js>,
    ) -> EngineResult<Value<'js>> {
        let model = crate::loader::virtual_model_from_js(&ctx, model)?;
        let context = Arc::clone(&self.context);
        let route = Arc::clone(&model.route);
        let definition = kiana_session_core::virtual_models::VirtualModelDefinition {
            provider: model.provider,
            id: model.id,
            name: model.name,
            thinking_levels: model.thinking_levels,
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            input: model.input,
            route: Some(Arc::new(move |request| {
                let route = Arc::clone(&route);
                let context = Arc::clone(&context);
                Box::pin(async move { route(request, context).await })
            })),
        };
        self.registry
            .register_virtual_model(definition)
            .map_err(|error| Exception::throw_message(&ctx, &error.message()))?;
        Ok(undefined(&ctx))
    }

    /// `unregisterVirtualModel(providerName, id)`。
    #[qjs(rename = "unregisterVirtualModel")]
    fn unregister_virtual_model(&self, provider_name: String, id: String) {
        self.registry.unregister_virtual_model(&provider_name, &id);
    }

    /// `getRegisteredProviderConfig(providerName)`。
    ///
    /// 数据字段按 TS `ProviderConfigInput` 的键序物化；`streamSimple`/`refreshModels`/
    /// `images`/`classifiers`/`oauth` 的 JS 回调无法从 Rust 结构逆向重建，返回对象里省略
    /// （见单元报告）。
    #[qjs(rename = "getRegisteredProviderConfig")]
    fn get_registered_provider_config<'js>(
        &self,
        ctx: Ctx<'js>,
        provider_name: String,
    ) -> EngineResult<Value<'js>> {
        match self.registry.get_registered_provider_config(&provider_name) {
            Some(config) => provider_config_input_to_js(&ctx, &config),
            None => Ok(undefined(&ctx)),
        }
    }

    /// `getRegisteredNativeProvider(providerName)` → JS `Provider | undefined`。
    #[qjs(rename = "getRegisteredNativeProvider")]
    fn get_registered_native_provider<'js>(
        &self,
        ctx: Ctx<'js>,
        provider_name: String,
    ) -> EngineResult<Value<'js>> {
        match self.registry.get_registered_native_provider(&provider_name) {
            Some(provider) => provider_to_js(&ctx, provider),
            None => Ok(undefined(&ctx)),
        }
    }
}

impl JsModelRegistry {
    /// `stream`/`streamSimple` 的共同入口：同步校验 → 宿主任务创建流 → Promise<流 id>。
    fn stream_promise<'js>(
        &self,
        ctx: &Ctx<'js>,
        op: &str,
        model: Value<'js>,
        context: Value<'js>,
        options: Opt<Value<'js>>,
    ) -> EngineResult<Value<'js>> {
        let model = value_to_json(ctx, model)?;
        let context = value_to_json(ctx, context)?;
        // TS 的 `runtime.stream(model, …)` 同步消费 model/context；非法值在这里就抛 TypeError。
        serde_json::from_value::<Model>(model.clone())
            .map_err(|error| Exception::throw_type(ctx, &format!("invalid model: {error}")))?;
        serde_json::from_value::<Context>(context.clone())
            .map_err(|error| Exception::throw_type(ctx, &format!("invalid context: {error}")))?;
        let (options_json, signal) = match options.0.as_ref() {
            Some(value) => (
                value_to_json(ctx, value.clone())?,
                option_signal_handle(ctx, Some(value), "signal")?,
            ),
            None => (JsonValue::Null, None),
        };
        let mut args = JsonObject::new();
        args.insert("model".to_owned(), model);
        args.insert("context".to_owned(), context);
        args.insert("options".to_owned(), options_json);
        if let Some(handle) = signal {
            args.insert("signal".to_owned(), JsonValue::Number(handle.into()));
        }
        registry_promise(ctx, self.handle, op, object_value(args))
    }
}

/// `__kiana_ctx.eventStream(promise)` → JS `AssistantMessageEventStream` 外观。
fn event_stream_object<'js>(ctx: &Ctx<'js>, promise: Value<'js>) -> EngineResult<Value<'js>> {
    let object: Object<'js> = ctx.globals().get("__kiana_ctx")?;
    let make: Function<'js> = object.get("eventStream")?;
    make.call((This(object), promise))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stream_options_forwards_known_fields_and_extra() {
        let raw = serde_json::json!({
            "sessionId": "s1",
            "cacheRetention": "short",
            "temperature": 0.5,
            "maxTokens": 123,
            "maxRetries": 4,
            "timeoutMs": 1000,
            "reasoningEffort": "high",
            "reasoning": "medium",
            "signal": { "aborted": true },
            "onPayload": null
        });
        let token = CancellationToken::new();
        let options = parse_stream_options(&raw, Some(token.clone()));
        assert_eq!(options.base.session_id.as_deref(), Some("s1"));
        assert_eq!(
            options.base.cache_retention,
            Some(kiana_ai::types::CacheRetention::Short)
        );
        assert_eq!(options.base.temperature, Some(0.5));
        assert_eq!(options.base.max_tokens, Some(123));
        assert_eq!(options.base.max_retries, Some(4));
        assert_eq!(options.base.timeout_ms, Some(1000));
        assert_eq!(options.reasoning.as_deref(), Some("medium"));
        assert_eq!(
            options.base.extra.get("reasoningEffort"),
            Some(&serde_json::json!("high"))
        );
        assert!(!options.base.extra.contains_key("signal"));
        assert!(!options.base.extra.contains_key("onPayload"));
        assert_eq!(
            options
                .base
                .signal
                .as_ref()
                .map(CancellationToken::is_cancelled),
            Some(false)
        );
    }
}
