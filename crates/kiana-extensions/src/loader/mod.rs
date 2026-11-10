//! `core/extensions/loader.ts`（879 行）的门面：发现、缓存、策略选择与扩展 API 事务。
//!
//! TS 源：`packages/coding-agent/src/core/extensions/loader.ts`。规范：
//! `spec/ca-extensions.md` §2.2、§3.4–§3.5、§4.1–§4.2、§8.4；
//! `spec/design-extension-runtime.md` §4.1–§4.3、§3.8、§10.1。
//!
//! # 本模块的职责
//!
//! - [`create_extension_runtime`]：动作方法全部抛错桩的共享 runtime（`refreshTools` 例外，
//!   是空函数；`setModel` 例外，返回 rejected promise 的等价物）。
//! - 每个扩展的**事务性 API**（`createExtensionAPI`）：loading / active / failed 三态，
//!   `commit()` 按注册顺序应用 pending 变更，`discard()` 只回滚**本扩展**的变更。
//! - `createExtension` / `initializeExtension` / `loadExtension` / 批量加载。
//! - 目录发现（[`discovery`]）与工厂缓存（[`factory_cache`]）。
//! - 模块导入走 [`loader_impl::ExtensionLoader`]（`rquickjs::Loader`），工厂函数调用与
//!   异步等待在本模块完成。
//!
//! # 为什么 `ExtensionRuntime` 的字段是「快照」
//!
//! TS 的 `createExtensionRuntime()` 返回一个**可变对象**：`runtime.flagValues.set(...)`、
//! `runtime.pendingProviderRegistrations = [...]`、`bindCore()` 替换动作方法。`kiana-ext-api`
//! 冻结的 [`ExtensionRuntime`] 是普通结构体（字段是 `IndexMap` / `Vec`，动作是 `Arc<dyn Fn>`），
//! 通过 `Arc` 共享后无法原地修改。本模块把**全部可变状态**放进 [`RuntimeShared`]
//! （`Arc<Mutex<...>>`，闭包捕获它），`ExtensionRuntime` 结构体是某一时刻的**快照**：
//!
//! - [`create_extension_runtime`] 返回的空快照仍可作为句柄传给所有加载函数
//!   （闭包指向同一个 `RuntimeShared`）；
//! - [`load_extensions`] / [`load_extensions_cached`] / [`discover_and_load_extensions`]
//!   返回的 `result.runtime` 是**加载结束时的快照**，因此 `pendingProviderRegistrations`、
//!   `flagValues` 等字段与 TS 读到的值一致；
//! - 需要读**实时**状态（例如 bindCore 冲刷完 pending 之后的调用、runner 的 `setFlagValue`）
//!   用本模块的 `runtime_*` 访问器：[`runtime_flag_values`]、
//!   [`runtime_pending_provider_registrations`]、[`runtime_set_flag_value`] 等。它们通过
//!   进程级的「`Arc<ExtensionRuntime>` 指针 → `RuntimeShared`」注册表找回共享状态。
//!
//! 这是 Rust 映射的已知偏离（报告里单列）；`kiana-ext-api` 的公开字段类型未改。
//!
//! # JS 注册如何变成 Rust 回调（`Send + Sync` 约束）
//!
//! 每个 `kiana-ext-api` 回调别名都是 `Send + Sync`，而 `rquickjs::Persistent` 是 `!Send`。
//! **绝不能**把 `Persistent` 放进这些闭包。本模块的机制：
//!
//! 1. JS 函数登记到线程本地的 `RETAINED_CALLBACKS` 表（见下），得到 `u64` id；闭包只捕获 id
//!    （`Send + Sync` 天然成立）。
//! 2. 闭包被调用时（handler / tool execute / renderer），通过线程本地的
//!    `current_engine` 取得当前 `JsEngine`（线程本地登记，`Weak`），在**同一线程**上 `with_js` 恢复
//!    `Persistent` 并调用。返回 `LocalBoxFuture` 的回调在 future 轮询时（主线程 LocalSet）
//!    才解析引擎，避免在 JS 调用栈里重入 `with_js`。
//! 3. 需要 `Send` 的 future（[`kiana_ext_api::BoxFuture`]，如 `ToolDefinitionExecute`）同样在
//!    轮询时解析线程本地引擎；调用方必须在 `kiana-extensions` 的单线程 `LocalSet` 上轮询它们
//!    （`ARCHITECTURE.md` §4.2 已要求本 crate 如此运行）。
//!
//! 回调 `Persistent` 寄存在**引擎自己的持久槽**（`JsEngine::put_persistent`），因此随引擎
//! `Drop` 一起清理，不会活过 runtime。
//!
//! `kiana.on()` 返回的退订闭包必须是 `Send + Sync`（`kiana_core::event_bus::Unsubscribe`），但删除
//! 动作需要 `Rc<ApiEntry>`；闭包只捕获 `u64` id，真正的删除动作放在线程本地
//! `HANDLER_REMOVERS` 里（本 crate 在主线程 LocalSet 上调用退订；跨线程调用是 no-op）。
//!
//! # 跨单元桥接钩子
//!
//! 有些值必须由其它单元物化成 JS 对象（本模块只给出窄接口，安装方在装配阶段调用）：
//!
//! | 钩子 | 安装方 | 用途 |
//! |---|---|---|
//! | [`set_context_bridge`] | runner | 基础 `ctx`（事件 handler / 快捷键） |
//! | [`set_tool_context_bridge`] | runner | 工具 `execute` 的 `ctx`（含 `tools` / `executeTool`） |
//! | [`set_command_context_bridge`] | runner | 命令 handler 的 `ctx`（含 `newSession` / `reload` …） |
//! | [`set_theme_bridge`] | ui | 渲染器的 `theme`（`fg` / `bold` …） |
//! | [`set_component_bridge`] | ui | 渲染器返回值的 TUI 组件 |
//! | [`set_extension_error_reporter`] | runner | command / shortcut 错误 → 用户可见的<br>`Extension "<path>" error: <message>` |
//!
//! 未安装时对应参数按 `undefined` 传递（与 TS 里字段缺失一致）；错误上报退回 `tracing::error!`。
//!
//! # 扩展对象身份（晚注册）
//!
//! TS 的 `createExtensionAPI` 闭包里的 `extension` 就是 `initializeExtension` 返回的对象：
//! 工厂结束后再调用 `kiana.registerTool` / `kiana.on` / `kiana.registerCommand` 会直接改变 runner
//! 手里的 `Extension`。Rust 的 [`Extension`] 是值类型（`kiana-ext-api` §4.9 冻结），`load_*`
//! 返回的是快照。本模块保留每个成功加载实例的实时 `ApiEntry`，并提供
//! [`live_extension`]（按路径取实时快照）、[`refresh_extension`] / [`refresh_extensions`]
//! （把调用方持有的快照同步到最新）。runner 在派发前调用 `refresh_extension` 即可看到晚
//! 注册，使晚注册在后续派发中生效。
//!
//! # 已知桥接缺口（冻结契约 / 其它单元边界）
//!
//! - **原生 provider 对象**：`kiana.registerProvider(providerObject)` 物化为 `JsNativeProvider`
//!   （id/name/baseUrl/headers/getModels/stream/streamSimple）。扩展自带的 `auth`
//!   （OAuth/API-key 交互回调）仍属 kiana-ai 绑定单元；依赖 auth 回调解析凭据的原生 provider
//!   暂按 `ProviderAuth::default()` 注册。
//! - **事件对象的就地修改**：`ExtensionHandlerFn` 冻结为 `Fn(ExtensionEvent, ctx)`（按值），
//!   适配器在调用后把同一个 JS 事件对象整体读回，经
//!   `take_handler_event_mutation` 交给 runner 的 host 桥放进结果信封；`dispatch.js`
//!   再把字段写回自己的事件对象。`headers` / `tool_call.input` /
//!   `before_agent_start.systemPromptOptions` / 边界 `entries` / `payload` / context
//!   `messages` 因此与 TS 的共享对象一致。逐 handler 重建的事件对象另带顶层属性引用快照
//!   （`take_handler_event_refs`）：只有**原地修改**的嵌套共享对象被采纳，属性重新赋值
//!   与 TS 一样被忽略；`context` 的 `messagesChanged` 也只比较原数组对象的元素引用。
//! - **宿主发起的 `events.emit`**：`kiana_core::event_bus` 的载荷是 `JsonValue`（每 handler
//!   一份克隆），只有 JS 发起的 emit 能保持引用相等。
//!
//! # 事务性状态机（`loader.ts:247–560` 逐行）
//!
//! `ApiEntry` 同时服务两条路径，避免两份语义：
//!
//! - **Rust 定义的扩展**：本模块的 `ExtensionApi` 实现（`LoaderExtensionApi`）；
//! - **JS 定义的扩展**：`js/api.js` 的 `makeApi(host, path)` 返回 `kiana` 对象；`host` 是每个
//!   扩展一个的 Rust 闭包集合（`build_host_object`），两者调用同一批 `ApiEntry` 方法。
//!   `js/api.js` 只做参数投影（两个 TS 重载 → 一个宿主调用）与 `setModel` 的
//!   rejected-promise 特例。
//!
//! | 状态 | 行为 |
//! |---|---|
//! | `loading` | 注册立即写入 `Extension`；flag 默认值进 `pendingFlagValues`；provider / MCP / virtual model 变更进 `pendingRuntimeChanges`；`events.on` 的退订进 `loadingUnsubscribers` |
//! | `active` | 变更立即应用；动作方法委托给绑定后的 runtime 动作 |
//! | `failed` | 任何 API 调用抛 `Extension "<path>" failed to load and its API is no longer active.` |
//!
//! `commit()`：先 `runtime.assertActive()`，再按顺序应用 pending flag 默认值与 pending 变更，
//! 置 `active` 并清空。`discard()`：置 `failed`、退订 loading 期建立的订阅、清空 pending
//! （只动本扩展自己的队列，因此并发成功的工厂不受影响——#8423）。
//!
//! # 调用方
//!
//! - 装配：`crate::Generation::with_config` 的第 5 步调用 [`install`]（`src/lib.rs` 的
//!   `assembly` 模块），之后才安装虚拟模块与 runner；
//! - 加载：`kiana-resources` 的 loader 调用 [`load_extensions_cached`]（显式路径）与
//!   [`load_extension_from_factory`]（`builtin:<name>` 的内联工厂），把 `extensions` /
//!   `errors` 合并进资源加载结果，并把返回的 `runtime` 继续传给内联扩展；
//! - 测试：`tests/extensions_loader.rs` / `tests/extensions_discovery.rs` 直接驱动本模块的
//!   公开入口（显式路径加载、发现顺序、逐字错误文案）。
//!
//! # 一次典型加载（执行顺序）
//!
//! 1. [`install`]：把 `loader_impl::ExtensionLoader` 挂到引擎 runtime、求值 `js/api.js`、
//!    注册四个 host kind（`loader.setModel` / `loader.exec` / `loader.projectTrustUi` /
//!    `loader.oauth`）。
//! 2. [`load_extensions`] / [`load_extensions_cached`] **串行**处理每条路径：路径解析 → 模块导入
//!    → 取默认导出工厂（[`discover_and_load_extensions`] 只是先发现路径，再调用
//!    [`load_extensions`]）。
//! 3. 工厂执行：`js/api.js` 的 `makeApi(host, path)` 建出 `kiana` 对象；JS 扩展走
//!    `initialize_js_extension`，Rust 扩展走 `initialize_rust_extension`。
//! 4. 成功则 `commit()` 并登记进 live 表（[`live_extension`]）；失败则 `discard()`，错误文本
//!    收进 `LoadError`。单条路径失败不影响其余路径。
//! 5. 返回值里的 `runtime` 是加载结束时的快照；`warnings` 在本函数返回时恒为 `Some(vec![])`
//!    （调用方会把自己的警告合并进去）。
//!
//! # 阅读顺序
//!
//! 1. [`create_extension_runtime`] / [`RuntimeShared`]：可变 runtime 状态与实时访问器；
//! 2. `ApiEntry`：单扩展事务与 loading / active / failed 三态；
//! 3. [`install`] 与 [`load_extensions`] 系列：装配与批量入口；
//! 4. `JsCallback`：`Send + Sync` 的回调句柄（id → 引擎持久槽）；
//! 5. `build_host_object` 与各 `js_*` 适配器：JS 侧注册如何落成 `kiana-ext-api` 类型。

pub mod cjs;
pub mod discovery;
pub mod factory_cache;
pub mod js_host;
pub mod loader_impl;
pub mod resolver;
pub mod transpile;
/// 可选 `WebAssembly` 宿主绑定（`feature = "webassembly"`，wasmtime）。
#[cfg(feature = "webassembly")]
pub mod wasm;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use indexmap::IndexMap;
use kiana_agent_core::{AgentToolError, AgentToolResult, AgentToolUpdateCallback, ThinkingLevel};
use kiana_ai::AssistantMessageEventStream;
use kiana_ai::types::{JsonValue, Model, ModelInputModality, ModelThinkingLevel, UserContent};
use kiana_config::settings::Settings;
use kiana_core::config::{CONFIG_DIR_NAME, get_agent_dir};
use kiana_core::event_bus::{EventBus, Unsubscribe, create_event_bus};
use kiana_core::exec::{ExecOptions as CoreExecOptions, exec_command};
use kiana_core::mcp_servers::{
    McpServerConfig, McpServerRegistry, RegisteredMcpServer, mcp_namespace,
};
use kiana_core::paths::{PathInputOptions, resolve_path};
use kiana_core::slash_commands::SlashCommandInfo;
use kiana_core::source_info::{
    SourceInfo, SyntheticSourceInfoOptions, create_synthetic_source_info,
    get_synthetic_path_source, is_synthetic_path,
};
use kiana_core::timings::{TimingLabel, time};
use kiana_ext_api::events::{
    ExtensionEventKind, ExtensionEventResult, ExtensionHandler, ProjectTrustContext,
    ProjectTrustEvent, ProjectTrustEventResult,
};
use kiana_ext_api::tool_result::{BashExecOptions, BashExecResult, BashOperations, BashResult};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute, ToolRendererResolver};
use kiana_ext_api::types::{
    CommandHandler, EntryRenderer, Extension, ExtensionApi, ExtensionCommandContextApi,
    ExtensionContextApi, ExtensionFlag, ExtensionRuntime, ExtensionShortcut,
    ExtensionToolContextApi, ExtensionVirtualModel, FlagType, FlagValue, LoadError,
    LoadExtensionsResult, LoadWarning, MarkdownTransformer, MessageRenderer,
    PendingNativeProviderRegistration, PendingProviderRegistration,
    PendingVirtualModelRegistration, ProviderConfig, ProviderRegistration, RefreshToolsHandler,
    RegisterCommandOptions, RegisterFlagOptions, RegisterShortcutOptions, RegisteredCommand,
    RegisteredTool, SendMessageOptions, ShortcutHandler,
};
use kiana_ext_api::{BoxFuture, LocalBoxFuture};
use kiana_session_core::virtual_models::{
    ModelRoute, ModelRouteRequest, RouteFuture, VirtualModelDefinition, VirtualModelRouteError,
};
use rquickjs::{Ctx, Exception, Function, Object, Persistent, Promise, Value};
use serde_json::json;

use crate::engine::{
    EngineResult, HostCall, HostValue, JsEngine, JsError, JsErrorKind, take_exception,
};
use crate::loader::discovery::{discover_extensions_in_dir, resolve_extension_entries};
use crate::loader::factory_cache::{ExtensionCacheToken, use_extension_cache_cwd};
use crate::loader::loader_impl::{ExtensionLoader, LoaderConfig};
use crate::loader::resolver::{ModuleResolver, ResolverOptions};
use crate::runner::mcp_registration::prepare_mcp_server;

// ===========================================================================
// 逐字文案（`spec/ca-extensions.md` §3.5、§2.2.3）
// ===========================================================================

/// `NotInitialized` 动作桩的【逐字】文案（`loader.ts:162`）。
pub const NOT_INITIALIZED_MESSAGE: &str =
    "Extension runtime not initialized. Action methods cannot be called during extension loading.";

/// `setModel` 桩的 rejected promise 文案（`loader.ts:186`）。
pub const SET_MODEL_NOT_INITIALIZED_MESSAGE: &str = "Extension runtime not initialized";

/// runtime 失效后的默认 stale 文案（`loader.ts:195`，逐字）。
pub const DEFAULT_STALE_MESSAGE: &str = "This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().";

/// 加载失败后任何 API 调用的【逐字】文案（`loader.ts:259`）。
fn failed_api_message(extension_path: &str) -> String {
    format!("Extension \"{extension_path}\" failed to load and its API is no longer active.")
}

/// 无效默认导出（`loader.ts:650`）。
fn invalid_factory_message(extension_path: &str) -> String {
    format!("Extension does not export a valid factory function: {extension_path}")
}

/// 任意加载错误的前缀（`loader.ts:658`）。
fn failed_load_message(message: &str) -> String {
    format!("Failed to load extension: {message}")
}

const LOADING_DEFAULT_CWD: &str = "<unknown>";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 锁内没有用户代码（只有内存操作与已捕获的闭包）；毒化只可能来自 OOM 级别的 panic。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `path.resolve(p)`（不展开 `~`，不做其它规范化）。
fn resolve_path_resolved(input: &str) -> String {
    let process_cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    resolve_path(input, &process_cwd, &PathInputOptions::default())
}

// ===========================================================================
// 共享 runtime 状态
// ===========================================================================

/// `runtime.createContext` 的工厂（`Send + Sync`：`ExtensionRuntime` 结构体在 `Arc` 里共享）。
pub type RuntimeCreateContextFn = Arc<dyn Fn() -> Arc<dyn ExtensionContextApi> + Send + Sync>;

/// 全部可变的 runtime 状态（TS 的 `ExtensionRuntime` 对象本体）。
///
/// 字段与 `kiana-ext-api` 的 [`ExtensionRuntime`] 一一对应；差异在于这里是可共享可变的
/// （闭包捕获 `Arc<RuntimeShared>`），而 [`ExtensionRuntime`] 是快照。见模块文档。
///
/// 各字段各自加锁，锁内只做内存操作；`invalidate` 先把退订表取空，再在锁外逐个调用
/// 退订闭包（锁不跨回调）。
pub struct RuntimeShared {
    flag_values: Mutex<IndexMap<String, FlagValue>>,
    pending_provider_registrations: Mutex<Vec<PendingProviderRegistration>>,
    pending_native_provider_registrations: Mutex<Vec<PendingNativeProviderRegistration>>,
    pending_virtual_model_registrations: Mutex<Vec<PendingVirtualModelRegistration>>,
    stale: Mutex<Option<String>>,
    /// 事件总线订阅（`trackEventBusSubscription` 的集合；id → 退订）。
    event_bus_unsubscribers: Mutex<HashMap<u64, Unsubscribe>>,
    next_subscription_id: AtomicU64,
    /// 绑定后的动作（`bindCore` 之前为 `None`）。
    actions: Mutex<Option<kiana_ext_api::types::ExtensionActions>>,
    refresh_tools: Mutex<Option<RefreshToolsHandler>>,
    create_context: Mutex<Option<RuntimeCreateContextFn>>,
    /// MCP server 注册表（`Clone` 是共享别名；TS 的 `runtime.mcpServers` 是同一个对象）。
    mcp_servers: McpServerRegistry,
    /// 加载期无法在 JS 侧完成的 provider 配置校验错误（延迟到 `bindCore` 上报）。
    ///
    /// TS 在加载期只把**原始**配置对象排队，校验发生在 `runner.bindCore` 的冲刷循环里
    /// （`runner.ts:463-476`）：抛错只产生一条 `register_provider` 扩展错误，扩展本身保持
    /// 已加载。Rust 的 `ProviderConfig` 是类型化的，`models` 必须在排队前解析，因此解析
    /// 失败时跳过该注册并在这里记下错误，供 `bindCore` 用
    /// [`take_runtime_provider_config_errors`] 取出上报。
    provider_config_errors: Mutex<Vec<PendingProviderConfigError>>,
    /// `bindCore` 之后替换 provider / 虚拟模型注册动作的函数组。
    ///
    /// TS 在 `bindCore` 的冲刷结束后**替换** `runtime.registerProvider` 等五个方法
    /// （`runner.ts:525-550`）；Rust 的 `ExtensionRuntime` 快照字段不可原地改写，因此把替换后的
    /// 动作放进共享状态，五个闭包每次调用时优先走这里。没有这组动作时（加载期）仍排队。
    provider_actions: Mutex<Option<ProviderRuntimeActions>>,
}

/// 加载期延迟的 provider 配置校验错误（TS 在 `bindCore` 冲刷时抛出的错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingProviderConfigError {
    /// `registerProvider` 的名字。
    pub name: String,
    /// 注册它的扩展路径。
    pub extension_path: String,
    /// 错误文本（`Failed to load extension` 之外的原始消息，由 `bindCore` 包成
    /// `Extension "<path>" error: <message>`）。
    pub message: String,
}

/// `bindCore` 之后立即生效的 provider / 虚拟模型动作（TS `runner.bindCore` 的方法替换）。
#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct ProviderRuntimeActions {
    /// `registerProvider(name, config)`。
    pub register_provider: Option<Arc<dyn Fn(String, ProviderConfig) + Send + Sync>>,
    /// `registerNativeProvider(provider)`。
    pub register_native_provider:
        Option<Arc<dyn Fn(Arc<dyn kiana_ai::models::Provider>) + Send + Sync>>,
    /// `unregisterProvider(name)`。
    pub unregister_provider: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// `registerVirtualModel(definition)`。
    pub register_virtual_model: Option<Arc<dyn Fn(VirtualModelDefinition) + Send + Sync>>,
    /// `unregisterVirtualModel(provider, id)`。
    pub unregister_virtual_model: Option<Arc<dyn Fn(String, String) + Send + Sync>>,
}

impl std::fmt::Debug for ProviderRuntimeActions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderRuntimeActions")
            .field("register_provider", &self.register_provider.is_some())
            .field(
                "register_native_provider",
                &self.register_native_provider.is_some(),
            )
            .field("unregister_provider", &self.unregister_provider.is_some())
            .field(
                "register_virtual_model",
                &self.register_virtual_model.is_some(),
            )
            .field(
                "unregister_virtual_model",
                &self.unregister_virtual_model.is_some(),
            )
            .finish()
    }
}

impl std::fmt::Debug for RuntimeShared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeShared")
            .field("flag_values", &*lock(&self.flag_values))
            .field("stale", &*lock(&self.stale))
            .finish_non_exhaustive()
    }
}

impl Default for RuntimeShared {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeShared {
    fn new() -> Self {
        Self {
            flag_values: Mutex::new(IndexMap::new()),
            pending_provider_registrations: Mutex::new(Vec::new()),
            pending_native_provider_registrations: Mutex::new(Vec::new()),
            pending_virtual_model_registrations: Mutex::new(Vec::new()),
            stale: Mutex::new(None),
            event_bus_unsubscribers: Mutex::new(HashMap::new()),
            next_subscription_id: AtomicU64::new(0),
            actions: Mutex::new(None),
            refresh_tools: Mutex::new(None),
            create_context: Mutex::new(None),
            mcp_servers: McpServerRegistry::new(),
            provider_config_errors: Mutex::new(Vec::new()),
            provider_actions: Mutex::new(None),
        }
    }

    fn flag_values(&self) -> IndexMap<String, FlagValue> {
        lock(&self.flag_values).clone()
    }

    fn has_flag(&self, name: &str) -> bool {
        lock(&self.flag_values).contains_key(name)
    }

    fn set_flag(&self, name: &str, value: FlagValue) {
        lock(&self.flag_values).insert(name.to_string(), value);
    }

    /// 记下一条延迟的 provider 配置错误（见 [`PendingProviderConfigError`]）。
    fn defer_provider_config_error(&self, name: &str, extension_path: &str, message: String) {
        lock(&self.provider_config_errors).push(PendingProviderConfigError {
            name: name.to_string(),
            extension_path: extension_path.to_string(),
            message,
        });
    }

    fn provider_config_errors(&self) -> Vec<PendingProviderConfigError> {
        lock(&self.provider_config_errors).clone()
    }

    fn take_provider_config_errors(&self) -> Vec<PendingProviderConfigError> {
        std::mem::take(&mut *lock(&self.provider_config_errors))
    }

    fn pending_provider_registrations(&self) -> Vec<PendingProviderRegistration> {
        lock(&self.pending_provider_registrations).clone()
    }

    fn pending_native_provider_registrations(&self) -> Vec<PendingNativeProviderRegistration> {
        lock(&self.pending_native_provider_registrations).clone()
    }

    fn pending_virtual_model_registrations(&self) -> Vec<PendingVirtualModelRegistration> {
        lock(&self.pending_virtual_model_registrations).clone()
    }

    fn take_pending_provider_registrations(&self) -> Vec<PendingProviderRegistration> {
        std::mem::take(&mut *lock(&self.pending_provider_registrations))
    }

    fn take_pending_native_provider_registrations(&self) -> Vec<PendingNativeProviderRegistration> {
        std::mem::take(&mut *lock(&self.pending_native_provider_registrations))
    }

    fn take_pending_virtual_model_registrations(&self) -> Vec<PendingVirtualModelRegistration> {
        std::mem::take(&mut *lock(&self.pending_virtual_model_registrations))
    }

    /// `assertActive()`：陈旧实例抛第一份 stale 文案。
    ///
    /// TS `if (state.staleMessage) throw`：空串是 falsy，**不**视为陈旧（与 runner 的
    /// `RunnerCore::assert_active` 同一规则）。
    fn assert_active(&self) {
        if let Some(message) = lock(&self.stale).as_deref()
            && !message.is_empty()
        {
            panic!("{message}");
        }
    }

    fn assert_active_result(&self) -> Result<(), String> {
        match lock(&self.stale).as_deref() {
            Some(message) if !message.is_empty() => Err(message.to_string()),
            _ => Ok(()),
        }
    }

    /// `invalidate(message?)`：首次生效；退订全部已跟踪的事件总线订阅。
    ///
    /// TS `if (state.staleMessage) return;` 同样按 truthiness 判断：已经存了空串时再来一次
    /// `invalidate()` 会继续执行（清理已空的订阅集合是 no-op）。
    fn invalidate(&self, message: Option<String>) {
        {
            let mut stale = lock(&self.stale);
            if stale.as_deref().is_some_and(|current| !current.is_empty()) {
                return;
            }
            *stale = Some(message.unwrap_or_else(|| DEFAULT_STALE_MESSAGE.to_string()));
        }
        let unsubscribers: Vec<Unsubscribe> = {
            let mut map = lock(&self.event_bus_unsubscribers);
            map.drain().map(|(_, unsubscribe)| unsubscribe).collect()
        };
        for unsubscribe in unsubscribers {
            unsubscribe();
        }
    }

    fn track_event_bus_subscription_with(
        shared: Arc<RuntimeShared>,
        unsubscribe: Unsubscribe,
    ) -> Unsubscribe {
        let id = shared.next_subscription_id.fetch_add(1, Ordering::Relaxed);
        lock(&shared.event_bus_unsubscribers).insert(id, unsubscribe);
        Box::new(move || {
            if let Some(unsubscribe) = lock(&shared.event_bus_unsubscribers).remove(&id) {
                unsubscribe();
            }
        })
    }
}

/// 当前的运行时动作（未绑定则抛 `NOT_INITIALIZED_MESSAGE`）。
fn runtime_actions(shared: &Arc<RuntimeShared>) -> kiana_ext_api::types::ExtensionActions {
    match lock(&shared.actions).clone() {
        Some(actions) => actions,
        None => panic!("{NOT_INITIALIZED_MESSAGE}"),
    }
}

fn runtime_create_context(shared: &Arc<RuntimeShared>) -> Option<Arc<dyn ExtensionContextApi>> {
    let factory = lock(&shared.create_context).clone()?;
    Some(factory())
}

fn runtime_refresh_tools(shared: &Arc<RuntimeShared>) {
    if let Some(refresh) = lock(&shared.refresh_tools).clone() {
        refresh();
    }
}

// ===========================================================================
// `createExtensionRuntime`
// ===========================================================================

/// `Arc<ExtensionRuntime>` 指针 → 共享状态。见模块文档「为什么字段是快照」。
///
/// 进程级（不是线程本地）：TS 的 runtime 是**一个**对象，跨线程读取也必须看到同一份状态
/// （例如 session 线程读 `runtime.flagValues`、主线程上的扩展写它）。键是 `Arc` 的地址，
/// 值持 `Weak`，因此不会延长 runtime 的生命周期；`register_shared` 顺带清理已死的键。
static RUNTIME_REGISTRY: std::sync::OnceLock<Mutex<HashMap<usize, Weak<RuntimeShared>>>> =
    std::sync::OnceLock::new();

fn runtime_registry() -> &'static Mutex<HashMap<usize, Weak<RuntimeShared>>> {
    RUNTIME_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn runtime_key(runtime: &Arc<ExtensionRuntime>) -> usize {
    Arc::as_ptr(runtime) as usize
}

fn register_shared(runtime: &Arc<ExtensionRuntime>, shared: &Arc<RuntimeShared>) {
    let mut registry = lock(runtime_registry());
    // 快照（每次批量加载新建）会随调用方释放；顺手清理已死的指针键，避免无界增长。
    registry.retain(|_, weak| weak.strong_count() > 0);
    registry.insert(runtime_key(runtime), Arc::downgrade(shared));
}

fn shared_for(runtime: &Arc<ExtensionRuntime>) -> Option<Arc<RuntimeShared>> {
    lock(runtime_registry())
        .get(&runtime_key(runtime))
        .and_then(Weak::upgrade)
}

/// 取 `runtime` 的共享状态；未注册时按快照字段重建，并**登记**这份重建的状态。
///
/// 登记是关键：`kiana-ext-api` 的 [`ExtensionRuntime`] 是公开结构体，其它单元可以直接构造；
/// 若重建后不登记，每次 `shared_of()` 都会得到一份新的 `RuntimeShared`，flag 默认值与
/// pending provider/MCP/virtual-model 变更会落进互不相干的副本（TS 只有一份可变 runtime）。
fn shared_of(runtime: &Arc<ExtensionRuntime>) -> Arc<RuntimeShared> {
    if let Some(shared) = shared_for(runtime) {
        return shared;
    }
    let shared = Arc::new(RuntimeShared::new());
    *lock(&shared.flag_values) = runtime.flag_values.clone();
    *lock(&shared.pending_provider_registrations) = runtime.pending_provider_registrations.clone();
    *lock(&shared.pending_native_provider_registrations) =
        runtime.pending_native_provider_registrations.clone();
    *lock(&shared.pending_virtual_model_registrations) =
        runtime.pending_virtual_model_registrations.clone();
    for server in runtime.mcp_servers.list() {
        shared.mcp_servers.register(server);
    }
    // 单锁内做「再检查 + 登记」，并发首次调用不会各建一份。
    let mut registry = lock(runtime_registry());
    if let Some(existing) = registry.get(&runtime_key(runtime)).and_then(Weak::upgrade) {
        return existing;
    }
    registry.retain(|_, weak| weak.strong_count() > 0);
    registry.insert(runtime_key(runtime), Arc::downgrade(&shared));
    shared
}

/// 用共享状态构造一个 `ExtensionRuntime` 快照（闭包全部指向 `shared`）。
fn build_runtime(shared: &Arc<RuntimeShared>) -> Arc<ExtensionRuntime> {
    // 每个动作闭包都捕获 `shared`、调用时才查当前动作，因此 `bind_runtime_actions` 之后
    // 同一个句柄立即看到新行为。provider / 虚拟模型的动作额外优先查 `provider_actions`
    // （绑定后立即生效；未绑定时把参数推进 pending 队列，等 bindCore 冲刷）。
    let runtime = Arc::new(ExtensionRuntime {
        flag_values: shared.flag_values(),
        pending_provider_registrations: shared.pending_provider_registrations(),
        pending_native_provider_registrations: shared.pending_native_provider_registrations(),
        pending_virtual_model_registrations: shared.pending_virtual_model_registrations(),
        create_context: {
            let shared = Arc::clone(shared);
            Arc::new(move || match runtime_create_context(&shared) {
                Some(context) => context,
                None => panic!("{NOT_INITIALIZED_MESSAGE}"),
            })
        },
        assert_active: {
            let shared = Arc::clone(shared);
            Arc::new(move || shared.assert_active())
        },
        invalidate: {
            let shared = Arc::clone(shared);
            Arc::new(move |message: Option<String>| shared.invalidate(message))
        },
        track_event_bus_subscription: {
            let shared = Arc::clone(shared);
            Arc::new(move |unsubscribe: Unsubscribe| {
                RuntimeShared::track_event_bus_subscription_with(Arc::clone(&shared), unsubscribe)
            })
        },
        register_provider: {
            let shared = Arc::clone(shared);
            Arc::new(
                move |name: String, config: ProviderConfig, extension_path: Option<String>| {
                    let immediate = lock(&shared.provider_actions)
                        .as_ref()
                        .and_then(|actions| actions.register_provider.clone());
                    if let Some(register) = immediate {
                        register(name, config);
                        return;
                    }
                    lock(&shared.pending_provider_registrations).push(
                        PendingProviderRegistration {
                            name,
                            config,
                            extension_path: extension_path
                                .unwrap_or_else(|| LOADING_DEFAULT_CWD.to_string()),
                        },
                    );
                },
            )
        },
        register_native_provider: {
            let shared = Arc::clone(shared);
            Arc::new(
                move |provider: Arc<dyn kiana_ai::models::Provider>,
                      extension_path: Option<String>| {
                    let immediate = lock(&shared.provider_actions)
                        .as_ref()
                        .and_then(|actions| actions.register_native_provider.clone());
                    if let Some(register) = immediate {
                        register(provider);
                        return;
                    }
                    lock(&shared.pending_native_provider_registrations).push(
                        PendingNativeProviderRegistration {
                            provider,
                            extension_path: extension_path
                                .unwrap_or_else(|| LOADING_DEFAULT_CWD.to_string()),
                        },
                    );
                },
            )
        },
        unregister_provider: {
            let shared = Arc::clone(shared);
            Arc::new(move |name: String, _extension_path: Option<String>| {
                let immediate = lock(&shared.provider_actions)
                    .as_ref()
                    .and_then(|actions| actions.unregister_provider.clone());
                if let Some(unregister) = immediate {
                    unregister(name);
                    return;
                }
                lock(&shared.pending_provider_registrations)
                    .retain(|registration| registration.name != name);
                lock(&shared.pending_native_provider_registrations)
                    .retain(|registration| registration.provider.id() != name);
            })
        },
        mcp_servers: shared.mcp_servers.clone(),
        register_virtual_model: {
            let shared = Arc::clone(shared);
            Arc::new(
                move |definition: VirtualModelDefinition, extension_path: Option<String>| {
                    let immediate = lock(&shared.provider_actions)
                        .as_ref()
                        .and_then(|actions| actions.register_virtual_model.clone());
                    if let Some(register) = immediate {
                        register(definition);
                        return;
                    }
                    lock(&shared.pending_virtual_model_registrations).push(
                        PendingVirtualModelRegistration {
                            definition,
                            extension_path: extension_path
                                .unwrap_or_else(|| LOADING_DEFAULT_CWD.to_string()),
                        },
                    );
                },
            )
        },
        unregister_virtual_model: {
            let shared = Arc::clone(shared);
            Arc::new(move |provider: String, id: String| {
                let immediate = lock(&shared.provider_actions)
                    .as_ref()
                    .and_then(|actions| actions.unregister_virtual_model.clone());
                if let Some(unregister) = immediate {
                    unregister(provider, id);
                    return;
                }
                lock(&shared.pending_virtual_model_registrations).retain(|registration| {
                    registration.definition.provider != provider || registration.definition.id != id
                });
            })
        },
        send_message: {
            let shared = Arc::clone(shared);
            Arc::new(move |options: SendMessageOptions| {
                (runtime_actions(&shared).send_message)(options);
            })
        },
        send_user_message: {
            let shared = Arc::clone(shared);
            Arc::new(move |content: UserContent, options| {
                (runtime_actions(&shared).send_user_message)(content, options);
            })
        },
        append_entry: {
            let shared = Arc::clone(shared);
            Arc::new(move |custom_type: String, data: Option<JsonValue>| {
                (runtime_actions(&shared).append_entry)(custom_type, data);
            })
        },
        set_session_name: {
            let shared = Arc::clone(shared);
            Arc::new(move |name: String| {
                (runtime_actions(&shared).set_session_name)(name);
            })
        },
        get_session_name: {
            let shared = Arc::clone(shared);
            Arc::new(move || (runtime_actions(&shared).get_session_name)())
        },
        set_label: {
            let shared = Arc::clone(shared);
            Arc::new(move |entry_id: String, label: Option<String>| {
                (runtime_actions(&shared).set_label)(entry_id, label);
            })
        },
        get_active_tools: {
            let shared = Arc::clone(shared);
            Arc::new(move || (runtime_actions(&shared).get_active_tools)())
        },
        get_all_tools: {
            let shared = Arc::clone(shared);
            Arc::new(move || (runtime_actions(&shared).get_all_tools)())
        },
        get_settings: {
            let shared = Arc::clone(shared);
            Arc::new(move || (runtime_actions(&shared).get_settings)())
        },
        set_active_tools: {
            let shared = Arc::clone(shared);
            Arc::new(move |tool_names: Vec<String>| {
                (runtime_actions(&shared).set_active_tools)(tool_names);
            })
        },
        refresh_tools: {
            let shared = Arc::clone(shared);
            Arc::new(move || runtime_refresh_tools(&shared))
        },
        get_commands: {
            let shared = Arc::clone(shared);
            Arc::new(move || (runtime_actions(&shared).get_commands)())
        },
        set_model: {
            let shared = Arc::clone(shared);
            Arc::new(move |model: Model| {
                if lock(&shared.actions).is_none() {
                    // TS 的 `() => Promise.reject(new Error("Extension runtime not initialized"))`。
                    return Box::pin(async { panic!("{SET_MODEL_NOT_INITIALIZED_MESSAGE}") });
                }
                (runtime_actions(&shared).set_model)(model)
            })
        },
        get_thinking_level: {
            let shared = Arc::clone(shared);
            Arc::new(move || (runtime_actions(&shared).get_thinking_level)())
        },
        set_thinking_level: {
            let shared = Arc::clone(shared);
            Arc::new(move |level: ThinkingLevel| {
                (runtime_actions(&shared).set_thinking_level)(level);
            })
        },
    });
    register_shared(&runtime, shared);
    runtime
}

/// `createExtensionRuntime()`：动作方法全部是抛错桩的共享 runtime。
///
/// `refreshTools` 是空函数（加载期注册工具合法）；`setModel` 未绑定时返回一个被 poll 后
/// panic 的 future（JS 侧在 `js/api.js` 里直接用
/// `Promise.reject` 表达同一文案）。其它动作方法 panic
/// [`NOT_INITIALIZED_MESSAGE`]（Rust 的 trait 方法没有 `Result`，这是唯一等价形态）。
///
/// TS: `loader.ts#createExtensionRuntime`。
pub fn create_extension_runtime() -> Arc<ExtensionRuntime> {
    build_runtime(&Arc::new(RuntimeShared::new()))
}

/// 建立 runtime 并返回其共享状态句柄（loader 内部使用；也供 runner 的 bindCore 使用）。
pub fn create_extension_runtime_shared() -> (Arc<ExtensionRuntime>, Arc<RuntimeShared>) {
    let shared = Arc::new(RuntimeShared::new());
    let runtime = build_runtime(&shared);
    (runtime, shared)
}

// ---- 实时状态访问器（见模块文档） -----------------------------------------

/// 当前的 flag 值（实时）。
pub fn runtime_flag_values(runtime: &Arc<ExtensionRuntime>) -> IndexMap<String, FlagValue> {
    shared_of(runtime).flag_values()
}

/// 写入一个 flag 值（TS `runner.setFlagValue`）。
pub fn runtime_set_flag_value(runtime: &Arc<ExtensionRuntime>, name: &str, value: FlagValue) {
    shared_of(runtime).set_flag(name, value);
}

/// 加载期排队、尚未被 bindCore 冲刷的 provider 注册（实时）。
pub fn runtime_pending_provider_registrations(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingProviderRegistration> {
    shared_of(runtime).pending_provider_registrations()
}

/// 加载期无法解析的 provider 配置错误（实时）。`bindCore` 应逐条上报为
/// `register_provider` 扩展错误（TS `runner.ts:463-476` 的 try/catch）。
pub fn runtime_provider_config_errors(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingProviderConfigError> {
    shared_of(runtime).provider_config_errors()
}

/// [`runtime_provider_config_errors`] 的取走版本。
pub fn take_runtime_provider_config_errors(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingProviderConfigError> {
    shared_of(runtime).take_provider_config_errors()
}

/// 加载期排队的原生 provider 注册（实时）。
pub fn runtime_pending_native_provider_registrations(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingNativeProviderRegistration> {
    shared_of(runtime).pending_native_provider_registrations()
}

/// 加载期排队的虚拟模型注册（实时）。
pub fn runtime_pending_virtual_model_registrations(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingVirtualModelRegistration> {
    shared_of(runtime).pending_virtual_model_registrations()
}

/// 取走并清空 provider 注册队列（bindCore 的冲刷步骤使用）。
pub fn take_runtime_pending_provider_registrations(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingProviderRegistration> {
    shared_of(runtime).take_pending_provider_registrations()
}

/// 取走并清空原生 provider 注册队列。
pub fn take_runtime_pending_native_provider_registrations(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingNativeProviderRegistration> {
    shared_of(runtime).take_pending_native_provider_registrations()
}

/// 取走并清空虚拟模型注册队列。
pub fn take_runtime_pending_virtual_model_registrations(
    runtime: &Arc<ExtensionRuntime>,
) -> Vec<PendingVirtualModelRegistration> {
    shared_of(runtime).take_pending_virtual_model_registrations()
}

/// 当前 stale 文案（`None` 表示有效）。
pub fn runtime_stale_message(runtime: &Arc<ExtensionRuntime>) -> Option<String> {
    lock(&shared_of(runtime).stale).clone()
}

/// `runtime.invalidate(message?)` 的便捷入口（与 runtime 快照里的闭包同一语义）。
pub fn runtime_invalidate(runtime: &Arc<ExtensionRuntime>, message: Option<&str>) {
    shared_of(runtime).invalidate(message.map(str::to_string));
}

/// 绑定动作（TS `bindCore` 的“替换 runtime 方法”部分）。
pub fn bind_runtime_actions(
    runtime: &Arc<ExtensionRuntime>,
    actions: kiana_ext_api::types::ExtensionActions,
) {
    let shared = shared_of(runtime);
    *lock(&shared.actions) = Some(actions);
}

/// 绑定 `refreshTools`（TS `bindCore` 的 `runtime.refreshTools = actions.refreshTools`）。
pub fn bind_runtime_refresh_tools(runtime: &Arc<ExtensionRuntime>, refresh: RefreshToolsHandler) {
    let shared = shared_of(runtime);
    *lock(&shared.refresh_tools) = Some(refresh);
}

/// 绑定 provider / 虚拟模型动作（TS `bindCore` 的“替换 runtime 方法”部分，`runner.ts:525-550`）。
///
/// 调用方（runner 的 `bind_core`）必须**先**用 `take_runtime_pending_*` 冲刷加载期队列，再调用
/// 本函数；此后扩展的 `registerProvider` / `unregisterProvider` / `registerVirtualModel` /
/// `unregisterVirtualModel` 立即生效（动作写入共享状态，后续派发直接读取）。
pub fn bind_runtime_provider_actions(
    runtime: &Arc<ExtensionRuntime>,
    actions: ProviderRuntimeActions,
) {
    let shared = shared_of(runtime);
    *lock(&shared.provider_actions) = Some(actions);
}

/// 绑定 `createContext`（TS `bindCore` 的 `runtime.createContext = () => this.createContext()`）。
pub fn bind_runtime_create_context(
    runtime: &Arc<ExtensionRuntime>,
    create_context: RuntimeCreateContextFn,
) {
    let shared = shared_of(runtime);
    *lock(&shared.create_context) = Some(create_context);
}

// ===========================================================================
// 每个扩展的 API 事务（`createExtensionAPI`）
// ===========================================================================

/// 扩展 API 的三态（TS `state: "loading" | "active" | "failed"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadState {
    Loading,
    Active,
    Failed,
}

struct ApiState {
    state: LoadState,
    pending_flag_values: IndexMap<String, FlagValue>,
    pending_runtime_changes: Vec<Box<dyn FnOnce()>>,
    loading_unsubscribers: Vec<Unsubscribe>,
}

/// 一个扩展的 API 事务：`Extension`（可变）、共享 runtime、事件总线、状态机。
///
/// 同时服务 Rust 定义的扩展（[`LoaderExtensionApi`]）与 JS 定义的扩展
/// （[`install`] 求值的 `js/api.js` 的 `kiana` 对象，宿主原语由 [`build_host_object`] 提供）。
/// 见模块文档。
pub(crate) struct ApiEntry {
    extension: Rc<RefCell<Extension>>,
    runtime: Arc<ExtensionRuntime>,
    /// 扩展实例 id（`API_ENTRIES` 的键；`Drop` 时清理）。
    entry_id: u64,
    shared: Arc<RuntimeShared>,
    event_bus: Arc<dyn EventBus>,
    cwd: String,
    state: RefCell<ApiState>,
    /// JS `host.on` 注册的 handler：id → (事件名, 适配器)。
    js_handlers: RefCell<HashMap<u64, (String, ExtensionHandler)>>,
    /// `host.on` 的 handler id → JS 回调 id（退订时释放引擎持久槽）。
    js_callback_ids: RefCell<HashMap<u64, u64>>,
    /// flag 的 **JS Map 键**（区分 `5` 与 `"5"`）→ `extension.flags` 的存储键。
    ///
    /// TS 的 `Map` 按键的 SameValueZero 比较：`registerFlag(5, …)` 之后 `getFlag(5)`
    /// 命中、`getFlag("5")` 不命中。Rust 的 `IndexMap<String, _>` 无法表达，因此在
    /// `ApiEntry` 里保留一张规范化键表（见 [`js_map_key`]）。
    flag_keys: RefCell<HashMap<String, String>>,
}

impl ApiEntry {
    fn new(
        extension: Extension,
        runtime: Arc<ExtensionRuntime>,
        entry_id: u64,
        shared: Arc<RuntimeShared>,
        event_bus: Arc<dyn EventBus>,
        cwd: String,
    ) -> Rc<Self> {
        let entry = Rc::new(Self {
            extension: Rc::new(RefCell::new(extension)),
            runtime,
            entry_id,
            shared,
            event_bus,
            cwd,
            state: RefCell::new(ApiState {
                state: LoadState::Loading,
                pending_flag_values: IndexMap::new(),
                pending_runtime_changes: Vec::new(),
                loading_unsubscribers: Vec::new(),
            }),
            js_handlers: RefCell::new(HashMap::new()),
            js_callback_ids: RefCell::new(HashMap::new()),
            flag_keys: RefCell::new(HashMap::new()),
        });
        API_ENTRIES.with(|entries| {
            entries.borrow_mut().insert(entry_id, Rc::downgrade(&entry));
        });
        entry
    }

    fn path(&self) -> String {
        self.extension.borrow().path.clone()
    }

    /// `assertActive()`：failed 状态先抛本扩展文案，再检查 runtime 的 stale 文案。
    fn check_active(&self) -> Result<(), String> {
        if self.state.borrow().state == LoadState::Failed {
            return Err(failed_api_message(&self.path()));
        }
        self.shared.assert_active_result()
    }

    /// [`ApiEntry::check_active`] 的 panic 形态：`kiana-ext-api` 的 `ExtensionApi` 方法没有
    /// 错误通道，panic 是 Rust 侧“调用即抛异常”的等价物（JS 侧走 `throw_message`）。
    fn ensure_active(&self) {
        if let Err(message) = self.check_active() {
            panic!("{message}");
        }
    }

    /// `applyRuntimeChange(change)`：loading 期排队，其它状态立即执行。
    fn apply_runtime_change(&self, change: impl FnOnce() + 'static) {
        {
            let mut state = self.state.borrow_mut();
            if state.state == LoadState::Loading {
                state.pending_runtime_changes.push(Box::new(change));
                return;
            }
        }
        change();
    }

    /// `commit()`：应用 pending flag 默认值与 pending 变更，然后置 active。
    ///
    /// 非 `Loading` 状态调用是无操作，因此重复 `commit` 幂等。`assertActive` 先于任何应用：
    /// runtime 已陈旧时返回 `Err` 且 pending 原样保留（调用方随后应 `discard`）。
    fn commit(&self) -> Result<(), String> {
        if self.state.borrow().state != LoadState::Loading {
            return Ok(());
        }
        self.shared.assert_active_result()?;
        let (flags, changes) = {
            let mut state = self.state.borrow_mut();
            (
                std::mem::take(&mut state.pending_flag_values),
                std::mem::take(&mut state.pending_runtime_changes),
            )
        };
        // 默认值只在 runtime 尚无同名 flag 时写入：已存在的值（更早的扩展或宿主写入）
        // 不会被后来的默认值覆盖。
        for (name, value) in flags {
            if !self.shared.has_flag(&name) {
                self.shared.set_flag(&name, value);
            }
        }
        // 按注册顺序执行：每个闭包把 loading 期累积的变更投给 runtime 闭包。若 runtime 尚未
        // `bindCore`，provider / 原生 provider / 虚拟模型的注册会落进 runtime 的 pending
        // 队列等 bindCore 冲刷；已经绑定则立即生效。
        for change in changes {
            change();
        }
        {
            let mut state = self.state.borrow_mut();
            state.state = LoadState::Active;
            state.pending_flag_values.clear();
            state.pending_runtime_changes.clear();
            state.loading_unsubscribers.clear();
        }
        Ok(())
    }

    /// `discard()`：置 failed，退订 loading 期订阅，清空 pending（只动本扩展自己的）。
    fn discard(&self) {
        let unsubscribers = {
            let mut state = self.state.borrow_mut();
            if state.state != LoadState::Loading {
                return;
            }
            state.state = LoadState::Failed;
            let unsubscribers = std::mem::take(&mut state.loading_unsubscribers);
            state.pending_flag_values.clear();
            state.pending_runtime_changes.clear();
            unsubscribers
        };
        for unsubscribe in unsubscribers {
            unsubscribe();
        }
    }

    // ---- on / off ---------------------------------------------------------

    fn add_handler(&self, event: &str, handler: ExtensionHandler) -> u64 {
        let id = next_handler_id();
        let mut extension = self.extension.borrow_mut();
        extension
            .handlers
            .entry(event.to_string())
            .or_default()
            .push(handler);
        id
    }

    fn remove_handler(&self, event: &str, handler: &ExtensionHandler) {
        let mut extension = self.extension.borrow_mut();
        let Some(handlers) = extension.handlers.get_mut(event) else {
            return;
        };
        if let Some(index) = handlers
            .iter()
            .position(|candidate| handlers_equal(candidate, handler))
        {
            handlers.remove(index);
        }
        if handlers.is_empty() {
            extension.handlers.shift_remove(event);
        }
    }

    /// JS `host.on`：登记适配器并返回 id（`host.off(id)` 精确移除这一条）。
    fn add_js_handler(
        &self,
        event: &str,
        handler: ExtensionHandler,
        callback: Option<JsCallback>,
    ) -> u64 {
        let id = next_handler_id();
        self.add_handler(event, handler.clone());
        self.js_handlers
            .borrow_mut()
            .insert(id, (event.to_string(), handler));
        if let Some(callback) = callback {
            self.js_callback_ids.borrow_mut().insert(id, callback.id);
        }
        self.notify_live_extension();
        id
    }

    fn remove_js_handler(&self, id: u64) {
        let callback_id = self.js_callback_ids.borrow_mut().remove(&id);
        if let Some(callback_id) = callback_id {
            // 派发进行中：保留 `Persistent`，让本次派发已取快照的 handler 仍可调用
            // （TS 的数组 splice 不影响已取出的函数引用）。
            if DISPATCH_DEPTH.with(|depth| depth.get()) > 0 {
                if let Some(engine) = current_engine()
                    && let Some(persistent) =
                        engine.take_persistent::<Function<'static>>(&callback_key(callback_id))
                {
                    RETAINED_CALLBACKS.with(|callbacks| {
                        callbacks.borrow_mut().insert(callback_id, persistent);
                    });
                }
            } else if let Some(engine) = current_engine() {
                engine.remove_persistent(&callback_key(callback_id));
            }
        }
        let removed = self.js_handlers.borrow_mut().remove(&id);
        if let Some((event, handler)) = removed {
            self.remove_handler(&event, &handler);
            self.notify_live_extension();
        }
    }

    /// 把 handler 表变化推给 runner 的扩展表（TS 里两者是同一个 `Map`）。
    fn notify_live_extension(&self) {
        let has_hook = LIVE_EXTENSION_SYNC.with(|slot| slot.borrow().is_some());
        if !has_hook {
            return;
        }
        let snapshot = self.extension.borrow().clone();
        LIVE_EXTENSION_SYNC.with(|slot| {
            if let Some(hook) = slot.borrow().as_ref() {
                hook(&snapshot);
            }
        });
    }

    /// JS `events.on`：`eventBus.on` + `runtime.trackEventBusSubscription`，返回 id。
    fn register_event_subscription(
        &self,
        channel: &str,
        handler: EventHandlerFn,
        callback: Option<JsCallback>,
    ) -> Result<u64, String> {
        self.check_active()?;
        let unsubscribe = self.event_bus.on(channel, handler);
        let tracked = (self.runtime.track_event_bus_subscription)(unsubscribe);
        let id = next_handler_id();
        JS_SUBSCRIPTIONS.with(|subscriptions| {
            subscriptions.borrow_mut().insert(id, (tracked, callback));
        });
        // loading 期建立的订阅必须能随 `discard()` 撤销：把「从订阅表移除 + 退订 + 释放
        // 回调」压进 loading_unsubscribers；`commit()` 只清空该表（订阅转正后不再回滚）。
        if self.state.borrow().state == LoadState::Loading {
            self.state
                .borrow_mut()
                .loading_unsubscribers
                .push(Box::new(move || {
                    JS_SUBSCRIPTIONS.with(|subscriptions| {
                        if let Some((unsubscribe, callback)) =
                            subscriptions.borrow_mut().remove(&id)
                        {
                            unsubscribe();
                            if let Some(callback) = callback {
                                callback.release();
                            }
                        }
                    });
                }));
        }
        Ok(id)
    }

    // ---- 注册型 API -------------------------------------------------------

    fn register_tool_checked(&self, tool: ToolDefinition) -> Result<(), String> {
        self.check_active()?;
        if !tool.parameters.is_object() {
            return Err(format!(
                "Tool \"{}\" registered by extension \"{}\" must define an object parameter schema.",
                tool.name,
                self.path()
            ));
        }
        let source_info = self.extension.borrow().source_info.clone();
        self.extension.borrow_mut().tools.insert(
            tool.name.clone(),
            RegisteredTool {
                definition: tool,
                source_info,
            },
        );
        runtime_refresh_tools(&self.shared);
        Ok(())
    }

    fn register_command_checked(
        &self,
        name: &str,
        description: Option<String>,
        get_argument_completions: Option<kiana_ext_api::types::ArgumentCompletionsFn>,
        handler: CommandHandler,
    ) -> Result<(), String> {
        self.check_active()?;
        if name.is_empty() {
            return Err(format!(
                "Command registered by extension \"{}\" must have a non-empty string name. Use kiana.registerCommand(\"name\", {{ description, handler }}).",
                self.path()
            ));
        }
        let command = RegisteredCommand {
            name: name.to_string(),
            source_info: self.extension.borrow().source_info.clone(),
            description,
            get_argument_completions,
            handler,
        };
        self.extension
            .borrow_mut()
            .commands
            .insert(name.to_string(), command);
        Ok(())
    }

    fn register_shortcut_checked(
        &self,
        shortcut: &str,
        description: Option<String>,
        handler: ShortcutHandler,
    ) -> Result<(), String> {
        self.check_active()?;
        let path = self.path();
        self.extension.borrow_mut().shortcuts.insert(
            shortcut.to_string(),
            ExtensionShortcut {
                shortcut: shortcut.to_string(),
                description,
                handler,
                extension_path: path,
            },
        );
        Ok(())
    }

    fn register_flag_checked(
        &self,
        name: &str,
        description: Option<String>,
        flag_type: FlagType,
        default: Option<FlagValue>,
    ) -> Result<(), String> {
        self.register_flag_keyed(name, &string_map_key(name), description, flag_type, default)
    }

    /// `registerFlag` 的实际实现。`js_key` 是 JS `Map` 的键（[`js_map_key`] 的规范化形式），
    /// `name` 是存储键（`String(name)`，即 `ExtensionFlag.name`）。
    fn register_flag_keyed(
        &self,
        name: &str,
        js_key: &str,
        description: Option<String>,
        flag_type: FlagType,
        default: Option<FlagValue>,
    ) -> Result<(), String> {
        self.check_active()?;
        let path = self.path();
        self.flag_keys
            .borrow_mut()
            .insert(js_key.to_string(), name.to_string());
        self.extension.borrow_mut().flags.insert(
            name.to_string(),
            ExtensionFlag {
                name: name.to_string(),
                description,
                r#type: flag_type,
                default: default.clone(),
                extension_path: path,
            },
        );
        if let Some(value) = default
            && !self.shared.has_flag(name)
        {
            let mut state = self.state.borrow_mut();
            if state.state == LoadState::Loading {
                state
                    .pending_flag_values
                    .entry(name.to_string())
                    .or_insert(value);
            } else {
                drop(state);
                self.shared.set_flag(name, value);
            }
        }
        Ok(())
    }

    fn get_flag_checked(&self, name: &str) -> Result<Option<FlagValue>, String> {
        self.get_flag_keyed(&string_map_key(name))
    }

    /// `getFlag`：按 JS `Map` 键查找。未注册 → `undefined`；否则优先 `runtime.flagValues`，
    /// 再退回加载期暂存的默认值（TS 的 `pendingFlagValues`）。
    fn get_flag_keyed(&self, js_key: &str) -> Result<Option<FlagValue>, String> {
        self.check_active()?;
        let Some(name) = self.flag_keys.borrow().get(js_key).cloned() else {
            return Ok(None);
        };
        if !self.extension.borrow().flags.contains_key(&name) {
            return Ok(None);
        }
        if let Some(value) = lock(&self.shared.flag_values).get(&name) {
            return Ok(Some(value.clone()));
        }
        Ok(self.state.borrow().pending_flag_values.get(&name).cloned())
    }

    fn register_message_renderer_checked(
        &self,
        custom_type: &str,
        renderer: MessageRenderer,
    ) -> Result<(), String> {
        self.check_active()?;
        self.extension
            .borrow_mut()
            .message_renderers
            .insert(custom_type.to_string(), renderer);
        Ok(())
    }

    fn register_markdown_transformer_checked(
        &self,
        transformer: MarkdownTransformer,
    ) -> Result<(), String> {
        self.check_active()?;
        self.extension.borrow_mut().markdown_transformer = Some(transformer);
        Ok(())
    }

    fn register_entry_renderer_checked(
        &self,
        custom_type: &str,
        renderer: EntryRenderer,
    ) -> Result<(), String> {
        self.check_active()?;
        let mut extension = self.extension.borrow_mut();
        extension
            .entry_renderers
            .get_or_insert_with(IndexMap::new)
            .insert(custom_type.to_string(), renderer);
        Ok(())
    }

    fn register_tool_renderer_checked(&self, resolver: ToolRendererResolver) -> Result<(), String> {
        self.check_active()?;
        let mut extension = self.extension.borrow_mut();
        extension
            .tool_renderers
            .get_or_insert_with(Vec::new)
            .push(resolver);
        Ok(())
    }

    // ---- provider / MCP / virtual model ----------------------------------

    /// 记下加载期无法完成的 provider 配置校验（延迟到 `bindCore` 上报）。
    ///
    /// 加载期返回 `Ok`（TS 只是把原始配置排队，等 bindCore 抛错）；已绑定后 TS 的
    /// `runtime.registerProvider` 会立即抛错，因此这里返回 `Err` 让调用方转成 JS 异常。
    fn defer_provider_config_error(&self, name: &str, message: String) -> Result<(), String> {
        if self.state.borrow().state == LoadState::Loading {
            self.shared
                .defer_provider_config_error(name, &self.path(), message);
            Ok(())
        } else {
            Err(message)
        }
    }

    fn register_provider_checked(&self, registration: ProviderRegistration) -> Result<(), String> {
        self.check_active()?;
        clear_provider_action_error();
        match registration {
            ProviderRegistration::Config { name, config } => {
                let runtime = Arc::clone(&self.runtime);
                let path = self.path();
                self.apply_runtime_change(move || {
                    (runtime.register_provider)(name, config, Some(path));
                });
            }
            ProviderRegistration::Native(provider) => {
                let runtime = Arc::clone(&self.runtime);
                let path = self.path();
                self.apply_runtime_change(move || {
                    (runtime.register_native_provider)(provider, Some(path));
                });
            }
        }
        // bindCore 之后的即时动作失败要像 TS 一样同步抛出（见 [`PROVIDER_ACTION_ERROR`]）。
        match take_provider_action_error() {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    fn unregister_provider_checked(&self, name: &str) -> Result<(), String> {
        self.check_active()?;
        let runtime = Arc::clone(&self.runtime);
        let name = name.to_string();
        let path = self.path();
        clear_provider_action_error();
        self.apply_runtime_change(move || {
            (runtime.unregister_provider)(name, Some(path));
        });
        match take_provider_action_error() {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    fn register_mcp_server_typed(&self, name: &str, config: McpServerConfig) -> Result<(), String> {
        self.check_active()?;
        let path = self.path();
        let registry = &self.runtime.mcp_servers;
        if let Some(owner) = registry.get(name).map(|server| server.extension_path)
            && owner != path
        {
            return Err(format!(
                "MCP server \"{name}\" is already registered by extension \"{owner}\""
            ));
        }
        let namespace = mcp_namespace(name);
        if let Some(clash) = registry
            .list()
            .into_iter()
            .find(|server| server.name != name && mcp_namespace(&server.name) == namespace)
        {
            return Err(format!(
                "MCP server \"{name}\" conflicts with registered server \"{}\"",
                clash.name
            ));
        }
        let server = RegisteredMcpServer {
            name: name.to_string(),
            config,
            extension_path: path,
        };
        let runtime = Arc::clone(&self.runtime);
        self.apply_runtime_change(move || runtime.mcp_servers.register(server));
        Ok(())
    }

    fn register_mcp_server_raw(&self, name: &str, config: &JsonValue) -> Result<(), String> {
        self.check_active()?;
        let path = self.path();
        let server = prepare_mcp_server(&self.runtime.mcp_servers, &path, name, config)
            .map_err(|error| error.to_string())?;
        let runtime = Arc::clone(&self.runtime);
        self.apply_runtime_change(move || runtime.mcp_servers.register(server));
        Ok(())
    }

    fn unregister_mcp_server_checked(&self, name: &str) -> Result<(), String> {
        self.check_active()?;
        let runtime = Arc::clone(&self.runtime);
        let name = name.to_string();
        let path = self.path();
        self.apply_runtime_change(move || runtime.mcp_servers.unregister(&name, &path));
        Ok(())
    }

    fn get_mcp_servers_checked(&self) -> Result<Vec<RegisteredMcpServer>, String> {
        self.check_active()?;
        Ok(self.runtime.mcp_servers.list())
    }

    fn register_virtual_model_checked(&self, model: ExtensionVirtualModel) -> Result<(), String> {
        self.check_active()?;
        let shared = Arc::clone(&self.shared);
        let route = Arc::clone(&model.route);
        let definition = VirtualModelDefinition {
            provider: model.provider,
            id: model.id,
            name: model.name,
            thinking_levels: model.thinking_levels,
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            input: model.input,
            route: Some(Arc::new(move |request: ModelRouteRequest| {
                let Some(context) = runtime_create_context(&shared) else {
                    return Box::pin(async {
                        Err(VirtualModelRouteError::new(NOT_INITIALIZED_MESSAGE))
                    }) as RouteFuture;
                };
                route(request, context)
            })),
        };
        let runtime = Arc::clone(&self.runtime);
        let path = self.path();
        clear_provider_action_error();
        self.apply_runtime_change(move || {
            (runtime.register_virtual_model)(definition, Some(path));
        });
        match take_provider_action_error() {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    fn unregister_virtual_model_checked(&self, provider: &str, id: &str) -> Result<(), String> {
        self.check_active()?;
        let runtime = Arc::clone(&self.runtime);
        let provider = provider.to_string();
        let id = id.to_string();
        clear_provider_action_error();
        self.apply_runtime_change(move || {
            (runtime.unregister_virtual_model)(provider, id);
        });
        match take_provider_action_error() {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    // ---- 事件总线 ---------------------------------------------------------

    fn events_emit_checked(&self, channel: &str, data: JsonValue) -> Result<(), String> {
        self.check_active()?;
        self.event_bus.emit(channel, data);
        Ok(())
    }
}

impl Drop for ApiEntry {
    fn drop(&mut self) {
        // `Drop` 可能在 rquickjs 的 extern "C" finalizer 里运行（引擎本身在 TLS 析构阶段被
        // 释放时，JS 堆里的 `kiana` 对象仍引用宿主函数）。此时线程本地存储可能已经销毁，
        // `LocalKey::with` 会 panic；panic 穿过 C 边界会 abort 进程（TS 没有等价析构步骤，
        // 必须干净退出）。用 `try_with` 并忽略 `AccessError`：表项是 `Weak`/只读清理，
        // 跳过不影响正确性。
        let _ = API_ENTRIES.try_with(|entries| {
            if let Ok(mut entries) = entries.try_borrow_mut() {
                entries.remove(&self.entry_id);
            }
        });
        let handler_ids: Vec<u64> = self.js_handlers.borrow().keys().copied().collect();
        if !handler_ids.is_empty() {
            let _ = HANDLER_REMOVERS.try_with(|removers| {
                if let Ok(mut removers) = removers.try_borrow_mut() {
                    for id in handler_ids {
                        removers.remove(&id);
                    }
                }
            });
        }
    }
}

/// 事件处理函数（`kiana_core::event_bus::EventHandler` 的别名，便于在本模块写类型）。
pub(crate) type EventHandlerFn = Arc<dyn Fn(JsonValue) + Send + Sync>;

/// JS 注册的 handler 计数（`on()` 返回的 id 只用于 `host.off`）。
fn next_handler_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn handlers_equal(left: &ExtensionHandler, right: &ExtensionHandler) -> bool {
    match (left, right) {
        (ExtensionHandler::Standard(left), ExtensionHandler::Standard(right)) => {
            Arc::ptr_eq(left, right)
        }
        (ExtensionHandler::ProjectTrust(left), ExtensionHandler::ProjectTrust(right)) => {
            Arc::ptr_eq(left, right)
        }
        _ => false,
    }
}

// ===========================================================================
// Rust 定义的扩展：kiana-ext-api 的 `ExtensionApi` 实现
// ===========================================================================

/// 传给 Rust `ExtensionFactory` 的 API。所有方法走 [`ApiEntry`] 的同一套事务语义。
struct LoaderExtensionApi {
    entry: Rc<ApiEntry>,
    entry_id: u64,
}

impl LoaderExtensionApi {
    fn new(entry: Rc<ApiEntry>, entry_id: u64) -> Self {
        Self { entry, entry_id }
    }
}

impl ExtensionApi for LoaderExtensionApi {
    fn on(&self, kind: ExtensionEventKind, handler: ExtensionHandler) -> Unsubscribe {
        self.entry.ensure_active();
        let event = kind.as_str().to_string();
        let id = self.entry.add_js_handler(&event, handler, None);
        // 退订闭包必须 `Send + Sync`（`kiana_core::event_bus::Unsubscribe`），因此只能捕获
        // id；真正的删除动作放在线程本地表里（本 crate 跑在主线程 LocalSet 上）。
        let weak = Rc::downgrade(&self.entry);
        HANDLER_REMOVERS.with(|removers| {
            removers.borrow_mut().insert(
                id,
                Box::new(move || {
                    if let Some(entry) = weak.upgrade() {
                        entry.remove_js_handler(id);
                    }
                }),
            );
        });
        Box::new(move || {
            HANDLER_REMOVERS.with(|removers| {
                if let Some(remove) = removers.borrow_mut().remove(&id) {
                    remove();
                }
            });
        })
    }

    fn register_tool(&self, tool: ToolDefinition) {
        if let Err(message) = self.entry.register_tool_checked(tool) {
            panic!("{message}");
        }
    }

    fn register_command(&self, name: &str, options: RegisterCommandOptions) {
        if let Err(message) = self.entry.register_command_checked(
            name,
            options.description,
            options.get_argument_completions,
            options.handler,
        ) {
            panic!("{message}");
        }
    }

    fn register_shortcut(&self, shortcut: &str, options: RegisterShortcutOptions) {
        if let Err(message) =
            self.entry
                .register_shortcut_checked(shortcut, options.description, options.handler)
        {
            panic!("{message}");
        }
    }

    fn register_flag(&self, name: &str, options: RegisterFlagOptions) {
        let result = match options {
            RegisterFlagOptions::Boolean {
                description,
                default,
            } => self.entry.register_flag_checked(
                name,
                description,
                FlagType::Boolean,
                default.map(FlagValue::Bool),
            ),
            RegisterFlagOptions::String {
                description,
                default,
            } => self.entry.register_flag_checked(
                name,
                description,
                FlagType::String,
                default.map(FlagValue::Str),
            ),
        };
        if let Err(message) = result {
            panic!("{message}");
        }
    }

    fn get_flag(&self, name: &str) -> Option<FlagValue> {
        match self.entry.get_flag_checked(name) {
            Ok(value) => value,
            Err(message) => panic!("{message}"),
        }
    }

    fn register_message_renderer(&self, custom_type: &str, renderer: MessageRenderer) {
        if let Err(message) = self
            .entry
            .register_message_renderer_checked(custom_type, renderer)
        {
            panic!("{message}");
        }
    }

    fn register_markdown_transformer(&self, transformer: MarkdownTransformer) {
        if let Err(message) = self
            .entry
            .register_markdown_transformer_checked(transformer)
        {
            panic!("{message}");
        }
    }

    fn register_entry_renderer(&self, custom_type: &str, renderer: EntryRenderer) {
        if let Err(message) = self
            .entry
            .register_entry_renderer_checked(custom_type, renderer)
        {
            panic!("{message}");
        }
    }

    fn register_tool_renderer(&self, resolver: ToolRendererResolver) {
        if let Err(message) = self.entry.register_tool_renderer_checked(resolver) {
            panic!("{message}");
        }
    }

    fn send_message(&self, options: SendMessageOptions) {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).send_message)(options);
    }

    fn send_user_message(
        &self,
        content: UserContent,
        options: Option<kiana_ext_api::types::SendUserMessageOptions>,
    ) {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).send_user_message)(content, options);
    }

    fn append_entry(&self, custom_type: &str, data: Option<JsonValue>) {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).append_entry)(custom_type.to_string(), data);
    }

    fn set_session_name(&self, name: &str) {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).set_session_name)(name.to_string());
    }

    fn get_session_name(&self) -> Option<String> {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).get_session_name)()
    }

    fn set_label(&self, entry_id: &str, label: Option<&str>) {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).set_label)(
            entry_id.to_string(),
            label.map(str::to_string),
        );
    }

    fn exec<'a>(
        &'a self,
        command: &'a str,
        args: &'a [String],
        options: Option<kiana_ext_api::tool_result::ExecOptions>,
    ) -> LocalBoxFuture<'a, kiana_ext_api::tool_result::ExecResult> {
        let entry = Rc::clone(&self.entry);
        Box::pin(async move {
            entry.ensure_active();
            let cwd = options
                .as_ref()
                .and_then(|options| options.cwd.clone())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(&entry.cwd));
            let core_options = options.as_ref().map(|options| CoreExecOptions {
                signal: options.signal.clone(),
                // TS 的 `setTimeout` 对小数毫秒也生效（0.5ms → 下一个定时器轮次）；
                // `exec_command` 只接受整毫秒，向上取整并至少 1ms（与 JS 宿主路径一致）。
                timeout: options
                    .timeout
                    .filter(|timeout| *timeout > 0.0 && timeout.is_finite())
                    .map(|timeout| (timeout.ceil() as u64).max(1)),
                cwd: options.cwd.clone().map(std::path::PathBuf::from),
            });
            // TS 的 `execCommand` 是异步的（spawn + Promise）：不得阻塞 LocalSet/事件循环。
            // `kiana_core::exec_command` 是同步的（签名冻结），放到阻塞线程池执行。
            let command = command.to_string();
            let args = args.to_vec();
            let result = match tokio::task::spawn_blocking(move || {
                exec_command(&command, &args, &cwd, core_options.as_ref())
            })
            .await
            {
                Ok(result) => result,
                Err(_) => kiana_core::exec::ExecResult {
                    stdout: String::new(),
                    stderr: "exec task failed".to_string(),
                    code: 1,
                    killed: false,
                },
            };
            kiana_ext_api::tool_result::ExecResult {
                stdout: result.stdout,
                stderr: result.stderr,
                code: result.code,
                killed: result.killed,
            }
        })
    }

    fn get_active_tools(&self) -> Vec<String> {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).get_active_tools)()
    }

    fn get_all_tools(&self) -> Vec<kiana_ext_api::types::ToolInfo> {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).get_all_tools)()
    }

    fn get_settings(&self) -> Settings {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).get_settings)()
    }

    fn set_active_tools(&self, tool_names: Vec<String>) {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).set_active_tools)(tool_names);
    }

    fn get_commands(&self) -> Vec<SlashCommandInfo> {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).get_commands)()
    }

    fn set_model(&self, model: Model) -> LocalBoxFuture<'static, bool> {
        if let Err(message) = self.entry.check_active() {
            return Box::pin(async move { panic!("{message}") });
        }
        if lock(&self.entry.shared.actions).is_none() {
            return Box::pin(async { panic!("{SET_MODEL_NOT_INITIALIZED_MESSAGE}") });
        }
        (runtime_actions(&self.entry.shared).set_model)(model)
    }

    fn get_thinking_level(&self) -> ThinkingLevel {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).get_thinking_level)()
    }

    fn set_thinking_level(&self, level: ThinkingLevel) {
        self.entry.ensure_active();
        (runtime_actions(&self.entry.shared).set_thinking_level)(level);
    }

    fn register_provider(&self, registration: ProviderRegistration) {
        if let Err(message) = self.entry.register_provider_checked(registration) {
            panic!("{message}");
        }
    }

    fn unregister_provider(&self, name: &str) {
        if let Err(message) = self.entry.unregister_provider_checked(name) {
            panic!("{message}");
        }
    }

    fn register_mcp_server(&self, name: &str, config: McpServerConfig) {
        if let Err(message) = self.entry.register_mcp_server_typed(name, config) {
            panic!("{message}");
        }
    }

    fn unregister_mcp_server(&self, name: &str) {
        if let Err(message) = self.entry.unregister_mcp_server_checked(name) {
            panic!("{message}");
        }
    }

    fn get_mcp_servers(&self) -> Vec<RegisteredMcpServer> {
        match self.entry.get_mcp_servers_checked() {
            Ok(servers) => servers,
            Err(message) => panic!("{message}"),
        }
    }

    fn register_virtual_model(&self, model: ExtensionVirtualModel) {
        if let Err(message) = self.entry.register_virtual_model_checked(model) {
            panic!("{message}");
        }
    }

    fn unregister_virtual_model(&self, provider: &str, id: &str) {
        if let Err(message) = self.entry.unregister_virtual_model_checked(provider, id) {
            panic!("{message}");
        }
    }

    fn events(&self) -> Arc<dyn EventBus> {
        Arc::new(ApiEventBus {
            entry_id: self.entry_id,
        })
    }
}

// ===========================================================================
// `createExtension`
// ===========================================================================

/// Node `path.dirname`（POSIX 语义）：`"my-ext"` → `"."`，`"/a/b"` → `"/a"`，
/// `"/"` → `"/"`，`""` → `"."`。
fn js_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        // `""` → `"."`；`"/"`、`"///"` → `"/"`。
        return if path.is_empty() {
            ".".to_string()
        } else {
            "/".to_string()
        };
    }
    match std::path::Path::new(trimmed).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    }
}

/// 建一个空集合的 `Extension`。
///
/// `source` 取 `getSyntheticPathSource(extensionPath) ?? "local"`；合成路径的 `baseDir` 为
/// `None`，否则是 `path.dirname(resolvedPath)`。`createSyntheticSourceInfo` 补齐
/// `scope: "temporary"`、`origin: "top-level"`。
///
/// TS: `loader.ts#createExtension`。
pub fn create_extension(extension_path: &str, resolved_path: &str) -> Extension {
    let source = get_synthetic_path_source(extension_path).unwrap_or_else(|| "local".to_string());
    let base_dir = if is_synthetic_path(extension_path) {
        None
    } else {
        // `path.dirname(resolvedPath)`：裸相对名（`my-ext`）的父目录是 `"."`，不是 `""`。
        Some(js_dirname(resolved_path))
    };
    let source_info: SourceInfo = create_synthetic_source_info(
        extension_path,
        SyntheticSourceInfoOptions {
            source,
            scope: None,
            origin: None,
            base_dir,
        },
    );

    Extension {
        path: extension_path.to_string(),
        resolved_path: resolved_path.to_string(),
        hidden: None,
        replaceable: None,
        source_info,
        handlers: IndexMap::new(),
        tools: IndexMap::new(),
        message_renderers: IndexMap::new(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: Some(IndexMap::new()),
        commands: IndexMap::new(),
        flags: IndexMap::new(),
        shortcuts: IndexMap::new(),
    }
}

// ===========================================================================
// JS 回调桥（`Send + Sync` 约束的核心机制）
// ===========================================================================

/// `extension ctx` 的 JS 物化桥（runner 单元安装）。
///
/// 事件 handler / 快捷键的上下文是 `Arc<dyn ExtensionContextApi>`，只有 runner 知道它的
/// 具体类型（`runner::context::ExtensionContext`）。runner 在 `bind_core` 之后调用
/// [`set_context_bridge`] 安装本回调；工具与命令上下文用各自更具体的钩子
/// （[`set_tool_context_bridge`] / [`set_command_context_bridge`]），避免向上转型擦掉 vtable。
pub type ContextBridge = Arc<
    dyn for<'js> Fn(&Ctx<'js>, Arc<dyn ExtensionContextApi>) -> EngineResult<Value<'js>>
        + Send
        + Sync,
>;

/// 工具 `execute()` 的 `ctx` 物化桥（runner 单元安装）。
///
/// 与 [`ContextBridge`] 分开是因为 `ExtensionToolContextApi` 多出 `tools` / `executeTool`；
/// 先向上转型成 `Arc<dyn ExtensionContextApi>` 会擦掉具体 vtable，JS 侧就永远拿不到这些成员
/// （examples 的 `ctx.executeTool` 依赖它）。
pub type ToolContextBridge = Arc<
    dyn for<'js> Fn(&Ctx<'js>, Arc<dyn ExtensionToolContextApi>) -> EngineResult<Value<'js>>
        + Send
        + Sync,
>;

/// 命令 / 快捷键 handler 的 `ctx` 物化桥（runner 单元安装）。
///
/// `ExtensionCommandContextApi` 多出 `newSession` / `fork` / `switchSession` / `navigateTree` /
/// `reload` / `waitForIdle` / `getSystemPromptOptions`。
pub type CommandContextBridge = Arc<
    dyn for<'js> Fn(&Ctx<'js>, Arc<dyn ExtensionCommandContextApi>) -> EngineResult<Value<'js>>
        + Send
        + Sync,
>;

/// 主题物化桥（ui 单元安装）：把 Rust [`kiana_theme::theme::Theme`] 变成 JS 主题对象
/// （`theme.fg()`/`theme.bold()` 等）。未安装时渲染器收到 `undefined`。
pub type ThemeBridge = Arc<
    dyn for<'js> Fn(&Ctx<'js>, &kiana_theme::theme::Theme) -> EngineResult<Value<'js>>
        + Send
        + Sync,
>;

/// 扩展错误上报器（runner 单元安装）。
///
/// TS 的 command / shortcut handler 抛错经 `runner.emitError` 变成用户可见的
/// `Extension "<path>" error: <message>`（chat / stderr / RPC `extension_error`）。loader 的
/// 适配器拿不到 runner 的错误通道，因此提供一个可安装的上报钩子；未安装时退回 tracing。
pub type ExtensionErrorReporter = Arc<dyn Fn(ExtensionErrorReport) + Send + Sync>;

/// [`ExtensionErrorReporter`] 的载荷（字段对齐 TS `emitError` 的入参）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionErrorReport {
    /// `emitError` 的 `extensionPath`：command 用 TS 的 `command:<name>`，shortcut 用扩展
    /// 文件路径（TS 的快捷键 rejection 只显示 `Shortcut handler error: <message>`）。
    pub extension_path: String,
    /// 事件名（`"command"` / `"shortcut"`）。
    pub event: String,
    /// `error.message`。
    pub error: String,
    /// `error.stack`（Rust 侧通常为 `None`）。
    pub stack: Option<String>,
}

/// JS 渲染器返回值的组件物化桥（ui 单元安装）。
///
/// `MessageRenderer`/`EntryRenderer`/`RenderCallFn` 要求返回 `Box<dyn Component>`；把任意 JS
/// 值变成 TUI 组件是 `ui/adapter.rs` 的职责。未安装桥时，非 null 的返回值按 `None` 处理并
/// 记一条 `tracing::warn!`（已记录的桥接缺口）。
pub type ComponentBridge = Arc<
    dyn for<'js> Fn(&Ctx<'js>, Value<'js>) -> EngineResult<Box<dyn kiana_tui::tui::Component>>
        + Send
        + Sync,
>;

thread_local! {
    /// 当前线程登记的引擎（按安装顺序，末尾是当前引擎）。`Rc` 保证引擎地址稳定；
    /// 本 crate 必须跑在单线程 `LocalSet` 上（`ARCHITECTURE.md` §4.2）。
    ///
    /// 登记用 `Weak`：强引用只由装配方（`Generation`/`ExtensionHost`）持有，登记本身不
    /// 延长 JS 堆的生命周期。已销毁的登记由 [`current_engine`] 惰性剔除，因此同一线程上
    /// 同时存在多个宿主时，销毁后安装的那个会自然回落到前一个仍存活的引擎（TS 的 loader
    /// 之间没有进程级引擎，见 `packages/coding-agent/src/core/extensions/loader.ts`）。
    static CURRENT_ENGINES: RefCell<Vec<RcWeak<JsEngine>>> = const { RefCell::new(Vec::new()) };
    /// 当前引擎的模块 loader（`load_extensions` 的模块导入入口）；与引擎成对登记。
    static CURRENT_LOADERS: RefCell<Vec<(RcWeak<JsEngine>, ExtensionLoader)>> =
        const { RefCell::new(Vec::new()) };
    /// 事件总线订阅表：id → 已跟踪的退订闭包。
    static JS_SUBSCRIPTIONS: RefCell<HashMap<u64, JsSubscription>> = RefCell::new(HashMap::new());
    /// `kiana.on()` 返回的退订闭包：id → 删除动作（捕获 `Weak<ApiEntry>`，故只能放这里）。
    static HANDLER_REMOVERS: RefCell<HashMap<u64, Box<dyn FnOnce()>>> = RefCell::new(HashMap::new());
    /// 回调 id 序号。
    static NEXT_JS_CALLBACK_ID: Cell<u64> = const { Cell::new(1) };
    /// 扩展实例 id → `ApiEntry`（host kind 的异步动作按 id 找回条目）。
    static API_ENTRIES: RefCell<HashMap<u64, std::rc::Weak<ApiEntry>>> = RefCell::new(HashMap::new());
    /// 成功加载的扩展实例（保持顺序；用于 [`live_extension`] / [`refresh_extension`]）。
    static LIVE_EXTENSIONS: RefCell<Vec<LiveExtensionRecord>> = const { RefCell::new(Vec::new()) };
    /// 扩展实例 id 序号。
    static NEXT_ENTRY_ID: Cell<u64> = const { Cell::new(1) };
    /// `ctx` 物化桥。
    static CONTEXT_BRIDGE: RefCell<Option<ContextBridge>> = const { RefCell::new(None) };
    /// 工具 `ctx` 物化桥。
    static TOOL_CONTEXT_BRIDGE: RefCell<Option<ToolContextBridge>> = const { RefCell::new(None) };
    /// 命令 `ctx` 物化桥。
    static COMMAND_CONTEXT_BRIDGE: RefCell<Option<CommandContextBridge>> = const { RefCell::new(None) };
    /// 组件物化桥。
    static COMPONENT_BRIDGE: RefCell<Option<ComponentBridge>> = const { RefCell::new(None) };
    /// 主题物化桥。
    static THEME_BRIDGE: RefCell<Option<ThemeBridge>> = const { RefCell::new(None) };
    /// 扩展错误上报器。
    static ERROR_REPORTER: RefCell<Option<ExtensionErrorReporter>> = const { RefCell::new(None) };
    /// 当前正在执行的 JS 事务的 `Ctx`（仅 `kiana.events.emit` 的同步重入路径使用）。
    static SYNC_JS_CTX: Cell<Option<usize>> = const { Cell::new(None) };
    /// `kiana.events.emit` 派发期间当前 channel 的**活对象**（`Value<'js>` 指针）。
    ///
    /// TS 的 `EventEmitter.emit(channel, data)` 把同一个对象引用交给该 channel 的所有 handler；
    /// `kiana_core::event_bus` 的冻结契约只能传 `JsonValue`（每 handler 一份克隆），因此 JS 发起的
    /// emit 在当前同步调用栈里额外记录活对象：JS handler 适配器直接用这个对象（引用相等、
    /// `undefined`/函数保留、handler 之间的就地修改可见），Rust handler 仍收到 JSON 克隆。
    /// 栈式保存以支持 handler 内再次 `emit`（嵌套 emit 结束后恢复外层对象）。
    static LIVE_EVENT_VALUES: RefCell<Vec<(String, usize)>> = const { RefCell::new(Vec::new()) };
    /// 最近一次标准事件 handler 调用后读回的**完整事件对象**（JSON）。
    ///
    /// TS 的 handler 拿到的是 runner 构造的事件对象本身，`before_provider_headers.headers`、
    /// `tool_call.input`、`before_agent_start.systemPromptOptions`、边界事件的
    /// `event.entries` 都是共享对象：原地修改对后续 handler 与最终返回值可见。
    /// `kiana-ext-api` 的 `ExtensionHandlerFn` 按值传事件且只能返回结果，所以适配器在调用后
    /// 把同一个 JS 事件对象整体读回，由 `runner/dispatch.rs` 的 host 桥放进结果信封；JS 侧
    /// 的 `callHandler` 再把字段写回自己的事件对象，恢复共享语义。
    static HANDLER_EVENT_MUTATION: RefCell<Option<JsonValue>> = const { RefCell::new(None) };
    /// `context` / `context_with_system` handler 是否按**引用**改动了事件的 `messages`
    /// 数组（TS 的 `sameMessages` 引用比较；只在同一个 JS 运行时里可测）。
    static HANDLER_MESSAGES_CHANGED: RefCell<Option<bool>> = const { RefCell::new(None) };
    /// 最近一次标准事件 handler 调用后，事件各顶层属性的**引用变化**（`same`/`changed`）。
    ///
    /// TS 对逐 handler 重建的事件对象（`{ ...event, message: currentMessage }`、
    /// `{ type, headers }` 等）只观察嵌套共享对象的**原地修改**；handler 重新赋值
    /// `event.headers` 不影响 runner。JSON 读回无法区分两者，因此在 JS 运行时里逐属性
    /// 比较调用前后的 JS 值引用，交给 `dispatch.js` 决定是否采纳。
    static HANDLER_EVENT_REFS: RefCell<Option<JsonValue>> = const { RefCell::new(None) };
    /// `input` handler 返回的 `images` 是否与调用前事件的 `images` 是同一个 JS 引用
    /// （TS 的 `result.images ?? currentImages` + `currentImages !== images` 判定）。
    static HANDLER_INPUT_IMAGES_SAME_REF: RefCell<Option<bool>> = const { RefCell::new(None) };
    /// 最近一次 JS 异常被物化成 [`JsCallbackError`] 时的 `error.stack`。
    ///
    /// `JsCallbackError` 只有 `name`/`message`（`kiana-ext-api` 的错误通道是 boxed error），
    /// 但 TS 的 `ExtensionError.stack` 记录的是扩展原始抛出的栈，`runner/dispatch.rs`
    /// 在拒绝 handler 调用时取走它。
    static LAST_JS_ERROR_STACK: RefCell<Option<String>> = const { RefCell::new(None) };
    /// `bindCore` 之后 `runtime.registerProvider(...)` 等即时动作的失败文本。
    ///
    /// TS 把这些方法替换成直接调用 `ModelRegistry` 的闭包，注册失败会**同步抛出**到扩展
    /// 的 `kiana.registerProvider(...)` 调用点（未捕获时由所在事件 handler 的 catch 上报）。
    /// `kiana-ext-api` 的 `ExtensionRuntime` 动作签名返回 `()`（冻结契约），因此失败经这个
    /// 线程本地槽回到 JS 宿主原语，由 `register_*_checked` 转成 JS 异常。
    static PROVIDER_ACTION_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
    /// `project_trust` handler 的失败文本（`ProjectTrustHandler` 没有错误通道）。
    static PROJECT_TRUST_FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// `user_bash` 的 `{ operations }` 里 `exec` 传递给 JS `onData` 的宿主回调
    /// （id → `&dyn Fn(&[u8])` 的地址；只在对应 `exec` 的 future 存活期间登记）。
    static BASH_ON_DATA: RefCell<HashMap<u64, (usize, usize)>> = RefCell::new(HashMap::new());
    /// `kiana.on()` / `host.off()` 改变 live 扩展的 handler 表时，把扩展快照推给 runner
    /// 的扩展表（runner 安装在装配期）。TS 的 `extension.handlers` 与 runner 看到的是
    /// **同一个 Map**，所以 handler 里的自我退订对后续派发立即生效；Rust 的 runner 表是
    /// 值快照，只能靠这个钩子把「退订发生在 live 条目被 GC 之前」的那次修改推送过去
    /// （之后 `refresh_extension_handlers` 的 Weak 查找可能已经找不到条目）。
    static LIVE_EXTENSION_SYNC: RefCell<Option<ExtensionLiveSyncHook>> = const { RefCell::new(None) };
    /// 派发进行中被退订的 JS 回调：`Persistent` 从引擎槽移到这里，派发结束再统一释放。
    ///
    /// TS 的派发持有 handler 函数本身，`kiana.off()` 只从 `extension.handlers` 数组里
    /// `splice`，**当前**派发仍会调用它（见 `extensions-runner.test.ts` 的
    /// "keeps removed pending handlers in the current dispatch"）。Rust 的 `Persistent`
    /// 是显式所有权，直接删除会让待派发的 handler 报
    /// `Extension callback is no longer registered`；因此延迟到派发表归零时释放。
    static RETAINED_CALLBACKS: RefCell<HashMap<u64, Persistent<Function<'static>>>> =
        RefCell::new(HashMap::new());
    /// 当前线程上活跃的派发表数量（`run_dispatch` 进入/离开时增减）。
    static DISPATCH_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// 取走最近一次标准事件 handler 读回的事件对象（由 `runner/dispatch.rs` 的 host 桥调用）。
pub(crate) fn take_handler_event_mutation() -> Option<JsonValue> {
    HANDLER_EVENT_MUTATION.with(|slot| slot.borrow_mut().take())
}

fn set_handler_event_mutation(event: JsonValue) {
    HANDLER_EVENT_MUTATION.with(|slot| *slot.borrow_mut() = Some(event));
}

/// 取走最近一次标准事件 handler 的顶层属性引用变化（由 host 桥放进结果信封）。
pub(crate) fn take_handler_event_refs() -> Option<JsonValue> {
    HANDLER_EVENT_REFS.with(|slot| slot.borrow_mut().take())
}

fn set_handler_event_refs(refs: JsonValue) {
    HANDLER_EVENT_REFS.with(|slot| *slot.borrow_mut() = Some(refs));
}

/// 取走 `input` handler 返回的 `images` 是否与调用前同一引用。
pub(crate) fn take_handler_input_images_same_ref() -> Option<bool> {
    HANDLER_INPUT_IMAGES_SAME_REF.with(|slot| slot.borrow_mut().take())
}

fn set_handler_input_images_same_ref(value: Option<bool>) {
    HANDLER_INPUT_IMAGES_SAME_REF.with(|slot| *slot.borrow_mut() = value);
}

/// 取走最近一次 `context` handler 是否按引用改动 `messages` 的标志（由 host 桥调用）。
pub(crate) fn take_handler_messages_changed() -> Option<bool> {
    HANDLER_MESSAGES_CHANGED.with(|slot| slot.borrow_mut().take())
}

fn set_handler_messages_changed(changed: bool) {
    HANDLER_MESSAGES_CHANGED.with(|slot| *slot.borrow_mut() = Some(changed));
}

/// 读取（不取走）最近一次 `context` handler 的引用变更标志；host 桥随后用
/// [`take_handler_messages_changed`] 取走它。
fn handler_messages_changed() -> Option<bool> {
    HANDLER_MESSAGES_CHANGED.with(|slot| *slot.borrow())
}

/// 取走最近一次 JS 异常的 `stack`（由 host 桥在拒绝 handler 调用时调用）。
pub(crate) fn take_last_js_error_stack() -> Option<String> {
    LAST_JS_ERROR_STACK.with(|slot| slot.borrow_mut().take())
}

fn set_last_js_error_stack(stack: Option<String>) {
    LAST_JS_ERROR_STACK.with(|slot| *slot.borrow_mut() = stack);
}

/// 记录一次即时 provider / 虚拟模型注册动作的失败（由 runner 的 `bind_core` 闭包调用）。
pub(crate) fn set_provider_action_error(message: String) {
    PROVIDER_ACTION_ERROR.with(|slot| *slot.borrow_mut() = Some(message));
}

fn take_provider_action_error() -> Option<String> {
    PROVIDER_ACTION_ERROR.with(|slot| slot.borrow_mut().take())
}

fn clear_provider_action_error() {
    PROVIDER_ACTION_ERROR.with(|slot| *slot.borrow_mut() = None);
}

/// 取走 `project_trust` handler 的失败文本（由 `runner/dispatch.rs` 的 host 桥调用）。
pub(crate) fn take_project_trust_handler_failure() -> Option<String> {
    PROJECT_TRUST_FAILURE.with(|slot| slot.borrow_mut().take())
}

fn set_project_trust_handler_failure(message: String) {
    PROJECT_TRUST_FAILURE.with(|slot| *slot.borrow_mut() = Some(message));
}

// ---------------------------------------------------------------------------
// 实时扩展对象（TS 的对象身份等价物）
// ---------------------------------------------------------------------------

/// 一个成功加载的扩展实例。
///
/// TS 的 `initializeExtension` 返回的 `Extension` 与 `createExtensionAPI` 闭包里的对象是
/// **同一个**：工厂结束后再调用 `kiana.registerTool` / `kiana.on` / `kiana.registerCommand` 会直接
/// 改变 runner 手里的 `Extension`。Rust 的 [`Extension`] 是值类型（`kiana-ext-api` §4.9 冻结），
/// `load_*` 返回的是快照；本表保留实时 [`ApiEntry`]，用 [`live_extension`] /
/// [`refresh_extension`] 取回晚注册的结果。
struct LiveExtensionRecord {
    /// `ApiEntry` 的 id（`API_ENTRIES` 的键）。
    entry_id: u64,
    /// 扩展显示路径。
    path: String,
    /// 解析后的真实路径。
    resolved_path: String,
}

/// 登记一次成功加载（`commit()` 之后调用）。
fn register_live_extension(entry: &Rc<ApiEntry>) {
    let record = LiveExtensionRecord {
        entry_id: entry.entry_id,
        path: entry.path(),
        resolved_path: entry.extension.borrow().resolved_path.clone(),
    };
    LIVE_EXTENSIONS.with(|records| {
        let mut records = records.borrow_mut();
        // 死条目（API 句柄与 JS `kiana` 对象都已释放）不再可写，直接清理。
        records.retain(|record| entry_for_id(record.entry_id).is_some());
        // 同 (path, resolvedPath) 的更新实例替换旧实例（`reload` 之后 runner 持有的是新实例）。
        records.retain(|existing| {
            existing.path != record.path || existing.resolved_path != record.resolved_path
        });
        records.push(record);
    });
}

fn live_extension_record(path: &str, resolved_path: &str) -> Option<Rc<ApiEntry>> {
    LIVE_EXTENSIONS.with(|records| {
        records
            .borrow()
            .iter()
            .rev()
            .find(|record| record.path == path && record.resolved_path == resolved_path)
            .and_then(|record| entry_for_id(record.entry_id))
    })
}

/// 取一个已加载扩展的**实时**状态（按显示路径匹配最近一次成功加载）。
///
/// 与 `load_*` 返回的快照不同，这里包含工厂结束后通过捕获的 `kiana` 注册的工具 / 命令 /
/// handler / flag / 渲染器（TS 的对象身份语义）。扩展已不再持有可调用的 `kiana` 句柄
/// （工厂没有保存 `Arc<dyn ExtensionApi>`、JS 侧也没有保留 `kiana` 对象）时返回 `None`。
pub fn live_extension(extension_path: &str) -> Option<Extension> {
    LIVE_EXTENSIONS.with(|records| {
        records
            .borrow()
            .iter()
            .rev()
            .filter(|record| record.path == extension_path)
            .find_map(|record| entry_for_id(record.entry_id))
            .map(|entry| entry.extension.borrow().clone())
    })
}

/// 用实时状态刷新一个 `load_*` 返回的 [`Extension`] 快照（晚注册的工具/命令/handler 等）。
///
/// 返回是否找到了实时实例；找不到时保持原值。
pub fn refresh_extension(extension: &mut Extension) -> bool {
    let Some(entry) = live_extension_record(&extension.path, &extension.resolved_path) else {
        return false;
    };
    *extension = entry.extension.borrow().clone();
    true
}

/// [`refresh_extension`] 的批量形式；返回刷新的条数。
pub fn refresh_extensions(extensions: &mut [Extension]) -> usize {
    let mut refreshed = 0;
    for extension in extensions.iter_mut() {
        if refresh_extension(extension) {
            refreshed += 1;
        }
    }
    refreshed
}

/// 只同步 handler 表（runner 的派发热路径；避免每个事件都克隆工具/命令/渲染器）。
///
/// TS 的 `ext.handlers` 是 `kiana.on()` 返回的同一个 `Map`：工厂结束后、甚至 handler
/// 执行期间动态 `kiana.on()` / `kiana.off()` 都直接改变它。runner 的快照是值类型，派发前只需
/// 把这一张表刷成实时状态；`ExtensionHandler` 是 `Arc`，克隆开销与 handler 数成正比。
pub(crate) fn refresh_extension_handlers(extension: &mut Extension) -> bool {
    let Some(entry) = live_extension_record(&extension.path, &extension.resolved_path) else {
        return false;
    };
    extension.handlers = entry.extension.borrow().handlers.clone();
    true
}

/// 安装 `ctx` 的 JS 物化桥（runner 单元调用；传 `None` 卸载）。
pub fn set_context_bridge(bridge: Option<ContextBridge>) {
    CONTEXT_BRIDGE.with(|slot| *slot.borrow_mut() = bridge);
}

/// 安装 JS 值的组件物化桥（ui 单元调用；传 `None` 卸载）。
pub fn set_component_bridge(bridge: Option<ComponentBridge>) {
    COMPONENT_BRIDGE.with(|slot| *slot.borrow_mut() = bridge);
}

/// 安装工具 `ctx` 物化桥（runner 单元调用；传 `None` 卸载）。
pub fn set_tool_context_bridge(bridge: Option<ToolContextBridge>) {
    TOOL_CONTEXT_BRIDGE.with(|slot| *slot.borrow_mut() = bridge);
}

/// 安装命令 `ctx` 物化桥（runner 单元调用；传 `None` 卸载）。
pub fn set_command_context_bridge(bridge: Option<CommandContextBridge>) {
    COMMAND_CONTEXT_BRIDGE.with(|slot| *slot.borrow_mut() = bridge);
}

/// live 扩展快照的推送钩子：`kiana.on` / `host.off` 改变 handler 表后调用。
///
/// runner 单元在装配期安装它，把自己的扩展表与 live 条目对齐（TS 里两者是同一个
/// `Map`；见 `LIVE_EXTENSION_SYNC` 的说明）。
pub type ExtensionLiveSyncHook = Box<dyn Fn(&Extension)>;

/// 安装 live 扩展快照的推送钩子（runner 单元调用；传 `None` 卸载）。
pub fn set_extension_live_sync(hook: Option<ExtensionLiveSyncHook>) {
    LIVE_EXTENSION_SYNC.with(|slot| *slot.borrow_mut() = hook);
}

/// 进入一次事件派发（`runner::dispatch::run_dispatch` 调用）。
///
/// 派发期间的退订把 JS 回调移到 [`RETAINED_CALLBACKS`]，本次派发仍可调用；最后一层派发
/// 结束时统一释放。
pub(crate) fn begin_dispatch() {
    DISPATCH_DEPTH.with(|depth| depth.set(depth.get() + 1));
}

/// 离开一次事件派发；最外层结束时释放保留的 JS 回调。
pub(crate) fn end_dispatch() {
    DISPATCH_DEPTH.with(|depth| {
        let next = depth.get().saturating_sub(1);
        depth.set(next);
        if next == 0 {
            let _ = RETAINED_CALLBACKS.try_with(|callbacks| {
                if let Ok(mut callbacks) = callbacks.try_borrow_mut() {
                    callbacks.clear();
                }
            });
        }
    });
}

/// 安装主题物化桥（ui 单元调用；传 `None` 卸载）。
pub fn set_theme_bridge(bridge: Option<ThemeBridge>) {
    THEME_BRIDGE.with(|slot| *slot.borrow_mut() = bridge);
}

/// 安装扩展错误上报器（runner 单元调用；传 `None` 卸载）。
pub fn set_extension_error_reporter(reporter: Option<ExtensionErrorReporter>) {
    ERROR_REPORTER.with(|slot| *slot.borrow_mut() = reporter);
}

/// 把 handler 错误交给已安装的上报器；未安装时退回 `tracing::error!`（与旧行为一致）。
fn report_extension_error(report: ExtensionErrorReport) {
    let reporter = ERROR_REPORTER.with(|slot| slot.borrow().clone());
    match reporter {
        Some(reporter) => reporter(report),
        None => tracing::error!(
            "Extension \"{}\" error: {}",
            report.extension_path,
            report.error
        ),
    }
}

/// 在 `kiana.events.emit` 的宿主调用期间记录当前 `Ctx`，供事件 handler 适配器同步重入 JS。
///
/// TS 的 EventEmitter 在 `emit` 里同步调用 handler（可能又是 JS）。Rust 的
/// `Ctx::with` 不可嵌套（`RefCell` 已借出），因此适配器在同一次事务里用宿主调用拿到的
/// `Ctx` 直接调用函数，而不是再进一次 `JsEngine::with_js`。
struct SyncCtxGuard {
    previous: Option<usize>,
}

impl SyncCtxGuard {
    fn enter<'js>(ctx: &Ctx<'js>) -> Self {
        let pointer = ctx as *const Ctx<'js> as usize;
        let previous = SYNC_JS_CTX.with(|slot| slot.replace(Some(pointer)));
        Self { previous }
    }
}

impl Drop for SyncCtxGuard {
    fn drop(&mut self) {
        // 恢复外层事务的 `Ctx`：TS 的 EventEmitter 可重入，JS handler 里再 `emit` 时，
        // 内层 emit 结束后外层 emit 的后续 handler 必须继续用外层的 `Ctx`。
        SYNC_JS_CTX.with(|slot| slot.set(self.previous));
    }
}

/// 在一次 `kiana.events.emit` 的同步派发期间记录活对象（见 [`LIVE_EVENT_VALUES`]）。
struct LiveEventGuard<'js> {
    _value: std::marker::PhantomData<&'js ()>,
}

impl<'js> LiveEventGuard<'js> {
    fn enter(channel: &str, value: &Value<'js>) -> Self {
        LIVE_EVENT_VALUES.with(|stack| {
            stack
                .borrow_mut()
                .push((channel.to_string(), value as *const Value<'js> as usize));
        });
        Self {
            _value: std::marker::PhantomData,
        }
    }
}

impl Drop for LiveEventGuard<'_> {
    fn drop(&mut self) {
        LIVE_EVENT_VALUES.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// 当前 channel 是否有一个正在派发的活对象；有则返回其指针。
fn live_event_value(channel: &str) -> Option<usize> {
    LIVE_EVENT_VALUES.with(|stack| {
        stack
            .borrow()
            .last()
            .filter(|(name, _)| name == channel)
            .map(|(_, pointer)| *pointer)
    })
}

/// 调一个事件 handler：若当前已在 JS 事务里，用该事务的 `Ctx`（同步重入），否则新开
/// 一次 `with_js`。`kiana.events.emit` 派发期间改用该次 emit 的**活对象**（引用相等）。
fn call_event_handler(
    callback: &JsCallback,
    channel: &str,
    args: Vec<JsArg>,
) -> Result<JsCallResult, JsCallbackError> {
    let mut args = args;
    if let Some(pointer) = live_event_value(channel) {
        args = vec![JsArg::LiveValue(pointer)];
    }
    let pointer = SYNC_JS_CTX.with(|slot| slot.get());
    match pointer {
        Some(pointer) => {
            // SAFETY: `SyncCtxGuard` 保证该指针在 `emit` 的同步调用栈内有效；`call_in_ctx`
            // 只在本次调用内使用 `Ctx`（`Value` 不逃逸出函数）。
            let ctx: &Ctx<'static> = unsafe { &*(pointer as *const Ctx<'static>) };
            callback.call_in_ctx(ctx, args)
        }
        None => callback.call(args),
    }
}

fn next_entry_id() -> u64 {
    NEXT_ENTRY_ID.with(|cell| {
        let id = cell.get();
        cell.set(id.wrapping_add(1));
        id
    })
}

fn next_js_callback_id() -> u64 {
    NEXT_JS_CALLBACK_ID.with(|cell| {
        let id = cell.get();
        cell.set(id.wrapping_add(1));
        id
    })
}

/// 一个 JS 回调的句柄：只有 id，因此 `Send + Sync`。
#[derive(Clone, Copy)]
pub(crate) struct JsCallback {
    id: u64,
}

impl std::fmt::Debug for JsCallback {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("JsCallback").field(&self.id).finish()
    }
}

/// `fn.bind(receiver)`：把一个 JS 函数固定到某个 `this`（TS 的方法调用语义）。
fn bind_function<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
    receiver: Value<'js>,
) -> EngineResult<Function<'js>> {
    let bind: Function<'js> = ctx.eval("(fn, receiver) => fn.bind(receiver)")?;
    bind.call((function, receiver))
}

/// `registerCommand` 保存的 `{ name, sourceInfo, ...options }` 对象（命令 handler 的 `this`）。
///
/// TS `extension.commands.set(name, { name, sourceInfo, ...options })`，`agent-session.ts`
/// 以 `command.handler(args, ctx)` 调用；`sourceInfo` 在 JS 侧没有对应的公开物化通道，这里
/// 只复制 options 的可枚举自有属性（`options.name` 按展开顺序覆盖 `name`）。
fn command_receiver<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    options: &Object<'js>,
) -> EngineResult<Object<'js>> {
    let make: Function<'js> = ctx.eval("(name, options) => Object.assign({ name }, options)")?;
    make.call((name.to_string(), options.clone()))
}

impl JsCallback {
    /// 把 JS 函数寄存到**引擎**的持久槽（`JsEngine::put_persistent`；引擎 `Drop` 时统一
    /// 清理，`Persistent` 因此不会活过 runtime）。
    fn new<'js>(ctx: &Ctx<'js>, function: Function<'js>) -> Self {
        let engine =
            current_engine().expect("JsEngine must be installed before registering callbacks");
        let id = next_js_callback_id();
        engine.put_persistent(callback_key(id), Persistent::save(ctx, function));
        Self { id }
    }

    /// 以 `receiver` 为 `this` 寄存 JS 函数（TS 的方法调用语义）。
    ///
    /// TS 的这几个回调是**对象方法**：`command.handler(args, ctx)`
    /// （`agent-session.ts:2193`）、`definition.execute(...)`
    /// （`tools/tool-definition-wrapper.ts:22`）、`model.route(request, …)`
    /// （`extensions/loader.ts:509`）。Rust 适配器原先用 `Function::call_arg`（`this` 为
    /// `undefined`），实现里读 `this` 的扩展会抛 TypeError。这里用
    /// `Function.prototype.bind` 把函数绑定到物化后的对象，所有既有调用路径无需改动。
    /// 绑定失败时退回未绑定函数（与 TS 注册时不校验方法实现的宽松行为一致）。
    fn new_with_this<'js>(ctx: &Ctx<'js>, function: Function<'js>, receiver: Value<'js>) -> Self {
        let bound = bind_function(ctx, function.clone(), receiver).unwrap_or_else(|_| {
            let _ = ctx.catch();
            function
        });
        Self::new(ctx, bound)
    }

    /// 在 `with_js` 事务内取出函数、调用 `f`、再把 `Persistent` 放回引擎槽。
    fn with_function<'js, R>(
        &self,
        ctx: &Ctx<'js>,
        f: impl FnOnce(&Ctx<'js>, Function<'js>) -> R,
    ) -> Result<R, JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        let key = callback_key(self.id);
        // 引擎槽里的原件优先；派发期间被退订的回调退回到 `RETAINED_CALLBACKS`
        // （本次派发仍要调用它，见 `remove_js_handler`）。
        let (persistent, retained): (Persistent<Function<'static>>, bool) =
            match engine.take_persistent::<Function<'static>>(&key) {
                Some(persistent) => (persistent, false),
                None => match RETAINED_CALLBACKS
                    .with(|callbacks| callbacks.borrow().get(&self.id).cloned())
                {
                    Some(persistent) => (persistent, true),
                    None => {
                        return Err(JsCallbackError {
                            name: "Error".to_string(),
                            message: "Extension callback is no longer registered".to_string(),
                        });
                    }
                },
            };
        // `Persistent` 是 `Clone`：先把原件放回槽，再恢复副本调用，同一 handler 的重入
        // （handler 里再 emit 同一事件）仍然能取到函数（保留表里的副本不回收）。
        let clone = persistent.clone();
        if !retained {
            engine.put_persistent(key, persistent);
        }
        match clone.restore(ctx) {
            Ok(function) => Ok(f(ctx, function)),
            Err(error) => Err(JsCallbackError {
                name: "Error".to_string(),
                message: error.to_string(),
            }),
        }
    }

    /// 同步调用；返回值可能是 Promise（由调用方决定是否等待）。
    fn call(&self, args: Vec<JsArg>) -> Result<JsCallResult, JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        engine.with_js(|ctx| self.call_in_ctx(&ctx, args))
    }

    fn call_in_ctx<'js>(
        &self,
        ctx: &Ctx<'js>,
        args: Vec<JsArg>,
    ) -> Result<JsCallResult, JsCallbackError> {
        self.with_function(ctx, |ctx, function| {
            let collected = build_js_args(ctx, args)?;
            match function.call_arg::<Value<'js>>(collected) {
                Ok(value) => match value.as_promise().cloned() {
                    Some(promise) => Ok(JsCallResult::Pending(Persistent::save(ctx, promise))),
                    None => value_to_json(ctx, value)
                        .map(JsCallResult::Value)
                        .map_err(|_| js_error_from_ctx(ctx)),
                },
                Err(_) => Err(js_error_from_ctx(ctx)),
            }
        })?
    }

    /// 异步调用：Promise 会被驱动到 settle。
    async fn call_async(&self, args: Vec<JsArg>) -> Result<JsonValue, JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        match self.call(args)? {
            JsCallResult::Value(value) => Ok(value),
            JsCallResult::Pending(promise) => await_js_promise(&engine, promise).await,
        }
    }

    /// 同步调用一个事件 handler，并保留**原始 JS 值**（区分 `undefined`/`null`，保留函数）。
    fn call_raw(&self, args: Vec<JsArg>) -> Result<JsRawCall, JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        engine.with_js_handler_deadline(|ctx| self.call_raw_in_ctx(&ctx, args))
    }

    fn call_raw_in_ctx<'js>(
        &self,
        ctx: &Ctx<'js>,
        args: Vec<JsArg>,
    ) -> Result<JsRawCall, JsCallbackError> {
        self.with_function(ctx, |ctx, function| {
            let collected = build_js_args(ctx, args)?;
            match function.call_arg::<Value<'js>>(collected) {
                Ok(value) => match value.as_promise().cloned() {
                    Some(promise) => Ok(JsRawCall::Pending(Persistent::save(ctx, promise))),
                    None => Ok(JsRawCall::Value(Persistent::save(ctx, value))),
                },
                Err(_) => Err(js_error_from_ctx(ctx)),
            }
        })?
    }

    /// 异步调用一个事件 handler 并返回原始值句柄（Promise 会被驱动到 settle）。
    async fn call_handler_raw(
        &self,
        args: Vec<JsArg>,
    ) -> Result<Persistent<Value<'static>>, JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        match self.call_raw(args)? {
            JsRawCall::Value(value) => Ok(value),
            JsRawCall::Pending(promise) => await_js_promise_value(&engine, promise).await,
        }
    }

    /// 调用一个事件 handler，保留原始返回值（函数不丢）并读回**同一个**事件对象。
    ///
    /// `user_bash` 的 `{ operations }` 携带 JS 函数，返回值不能 JSON 化；事件对象的共享
    /// 语义（TS 里 handler 与 runner 是同一个对象）仍需要读回。失败时事件对象同样读回
    /// 并放进线程本地槽（抛错前的就地修改不回滚）。
    #[allow(clippy::type_complexity)]
    async fn call_handler_raw_with_event(
        &self,
        event_json: JsonValue,
        context: Option<Arc<dyn ExtensionContextApi>>,
    ) -> Result<(Persistent<Value<'static>>, JsonValue), JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        let (promise, sync_value, event_handle, call_error) = engine.with_js_handler_deadline(
            |ctx| -> Result<
                (
                    Option<Persistent<Promise<'static>>>,
                    Option<Persistent<Value<'static>>>,
                    Persistent<Object<'static>>,
                    Option<JsCallbackError>,
                ),
                JsCallbackError,
            > {
                self.with_function(&ctx, |ctx, function| {
                    let event_object = json_value(ctx, &event_json)?
                        .as_object()
                        .cloned()
                        .ok_or_else(|| JsCallbackError {
                            name: "Error".to_string(),
                            message: "event is not an object".to_string(),
                        })?;
                    let ctx_value = context_arg_value(ctx, context.clone())
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    let mut args = rquickjs::function::Args::new_unsized(ctx.clone());
                    args.push_arg(event_object.clone())
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    args.push_arg(ctx_value)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    let event_handle = Persistent::save(ctx, event_object);
                    match function.call_arg::<Value<'_>>(args) {
                        Ok(value) => match value.as_promise().cloned() {
                            Some(promise) => Ok((
                                Some(Persistent::save(ctx, promise)),
                                None,
                                event_handle,
                                None,
                            )),
                            None => {
                                Ok((None, Some(Persistent::save(ctx, value)), event_handle, None))
                            }
                        },
                        Err(_) => {
                            let error = js_error_from_ctx(ctx);
                            Ok((None, None, event_handle, Some(error)))
                        }
                    }
                })?
            },
        )?;
        let result = match call_error {
            Some(error) => Err(error),
            None => match promise {
                Some(promise) => await_js_promise_value(&engine, promise).await,
                None => Ok(sync_value.expect("sync handler value")),
            },
        };
        let event = engine.with_js(|ctx| -> Result<JsonValue, JsCallbackError> {
            let event = event_handle
                .clone()
                .restore(&ctx)
                .map_err(|error| JsCallbackError {
                    name: "Error".to_string(),
                    message: error.to_string(),
                })?;
            value_to_json(&ctx, event.into_value()).map_err(|_| js_error_from_ctx(&ctx))
        })?;
        match result {
            Ok(value) => Ok((value, event)),
            Err(error) => {
                set_handler_event_mutation(event);
                Err(error)
            }
        }
    }

    /// 调用一个标准事件 handler，并在返回后读回**同一个**事件对象（`context` 事件允许
    /// handler 就地修改 `event.messages`，TS 会把该修改合并给后续 handler）。
    ///
    /// 返回 `(handler 返回值, 事件对象当前的 messages 字段)`。
    #[allow(clippy::type_complexity)]
    #[cfg_attr(not(test), allow(dead_code))] // 只被 `regression_tests` 使用
    async fn call_with_event_mutation(
        &self,
        event_json: JsonValue,
        context: Option<Arc<dyn ExtensionContextApi>>,
    ) -> Result<(JsonValue, JsonValue), JsCallbackError> {
        let (result, event, _) = self
            .call_with_event_capture(event_json, context, false)
            .await?;
        Ok((
            result,
            event.get("messages").cloned().unwrap_or(JsonValue::Null),
        ))
    }

    /// 调用标准事件 handler 并读回**整个**事件对象（TS 的共享对象语义）。
    ///
    /// 返回 `(handler 返回值, 事件对象当前的完整 JSON)`。事件上的 getter（如
    /// `before_agent_start.systemPrompt`）在 `json_stringify` 时求值，与 TS 每次读取
    /// 时求值的行为在跨 handler 的粒度上一致。
    #[allow(clippy::type_complexity)]
    async fn call_with_event_capture(
        &self,
        event_json: JsonValue,
        context: Option<Arc<dyn ExtensionContextApi>>,
        track_messages: bool,
    ) -> Result<(JsonValue, JsonValue, JsonValue), JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        let captured =
            engine.with_js_handler_deadline(|ctx| -> Result<CapturedEvent, JsCallbackError> {
                self.with_function(&ctx, |ctx, function| {
                    let event_object = handler_event_object(ctx, &event_json)?;
                    // `context` 事件的 `sameMessages` 引用比较只能在 JS 运行时里做：
                    // 记录调用前 `event.messages` 数组本体与元素引用，调用后比较**原数组**。
                    let (messages_array, messages_snapshot) = if track_messages {
                        match snapshot_message_refs(ctx, &event_object) {
                            Some((array, refs)) => (Some(array), Some(refs)),
                            None => (None, None),
                        }
                    } else {
                        (None, None)
                    };
                    // 顶层属性的 JS 值快照：区分「handler 原地修改共享对象」与「重新赋值」。
                    // TS 对逐 handler 重建的事件对象只采纳前者的嵌套修改。
                    let saved_values = snapshot_event_values(ctx, &event_object, &event_json);
                    let ctx_value = context_arg_value(ctx, context.clone())
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    // `before_agent_start` 的 `ctx.getSystemPrompt()` 也必须按**当前**选项
                    // 重渲染（TS 用 `() => buildSystemPrompt(currentOptions)` 覆盖该槽）。
                    install_before_agent_start_ctx_override(
                        ctx,
                        &event_object,
                        &ctx_value,
                        &event_json,
                    )?;
                    let mut args = rquickjs::function::Args::new_unsized(ctx.clone());
                    args.push_arg(event_object.clone())
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    args.push_arg(ctx_value)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    let event_handle = Persistent::save(ctx, event_object);
                    match function.call_arg::<Value<'_>>(args) {
                        Ok(returned) => match returned.as_promise().cloned() {
                            Some(promise) => Ok(CapturedEvent {
                                promise: Some(Persistent::save(ctx, promise)),
                                returned: None,
                                event: event_handle,
                                error: None,
                                saved_values,
                                messages_array,
                                messages_snapshot,
                            }),
                            None => Ok(CapturedEvent {
                                promise: None,
                                returned: Some(Persistent::save(ctx, returned)),
                                event: event_handle,
                                error: None,
                                saved_values,
                                messages_array,
                                messages_snapshot,
                            }),
                        },
                        Err(_) => {
                            // handler 同步抛错：事件对象仍然要读回（抛错前的就地修改）。
                            let error = js_error_from_ctx(ctx);
                            Ok(CapturedEvent {
                                promise: None,
                                returned: None,
                                event: event_handle,
                                error: Some(error),
                                saved_values,
                                messages_array,
                                messages_snapshot,
                            })
                        }
                    }
                })?
            })?;
        let result: Result<Option<Persistent<Value<'static>>>, JsCallbackError> =
            match captured.error {
                Some(error) => Err(error),
                None => match captured.promise {
                    Some(promise) => await_js_promise_value(&engine, promise).await.map(Some),
                    None => Ok(captured.returned),
                },
            };
        // 调用后的第二个 JS 事务：恢复事件对象与 handler 返回值，做引用比较
        // （messages 数组、顶层属性、`images`），并把事件对象整体读回 JSON。
        // 失败路径也在这里读回（抛错前的就地修改不回滚），然后交给 host 桥。
        match result {
            Ok(returned) => {
                let (value, refs, event) = engine.with_js(
                    |ctx| -> Result<(JsonValue, JsonValue, JsonValue), JsCallbackError> {
                        let event = captured.event.clone().restore(&ctx).map_err(|error| {
                            JsCallbackError {
                                name: "Error".to_string(),
                                message: error.to_string(),
                            }
                        })?;
                        if let (Some(array), Some(snapshot)) =
                            (&captured.messages_array, &captured.messages_snapshot)
                        {
                            set_handler_messages_changed(messages_changed_for_saved_array(
                                &ctx, array, snapshot,
                            ));
                        }
                        let returned_value = match &returned {
                            Some(handle) => {
                                Some(handle.clone().restore(&ctx).map_err(|error| {
                                    JsCallbackError {
                                        name: "Error".to_string(),
                                        message: error.to_string(),
                                    }
                                })?)
                            }
                            None => None,
                        };
                        set_handler_input_images_same_ref(input_images_same_ref(
                            &ctx,
                            returned_value.as_ref(),
                            &captured.saved_values,
                        ));
                        let value = match returned_value {
                            Some(value) => {
                                value_to_json(&ctx, value).map_err(|_| js_error_from_ctx(&ctx))?
                            }
                            None => JsonValue::Null,
                        };
                        let refs = event_value_refs(&ctx, &event, &captured.saved_values);
                        let readback = value_to_json(&ctx, event.into_value())
                            .map_err(|_| js_error_from_ctx(&ctx))?;
                        Ok((value, refs, readback))
                    },
                )?;
                Ok((value, event, refs))
            }
            Err(error) => {
                let (refs, event) =
                    engine.with_js(|ctx| -> Result<(JsonValue, JsonValue), JsCallbackError> {
                        let event = captured.event.clone().restore(&ctx).map_err(|error| {
                            JsCallbackError {
                                name: "Error".to_string(),
                                message: error.to_string(),
                            }
                        })?;
                        if let (Some(array), Some(snapshot)) =
                            (&captured.messages_array, &captured.messages_snapshot)
                        {
                            set_handler_messages_changed(messages_changed_for_saved_array(
                                &ctx, array, snapshot,
                            ));
                        }
                        let refs = event_value_refs(&ctx, &event, &captured.saved_values);
                        let readback = value_to_json(&ctx, event.into_value())
                            .map_err(|_| js_error_from_ctx(&ctx))?;
                        Ok((refs, readback))
                    })?;
                // 失败路径：把读回的事件对象留给 host 桥（dispatch.js 先应用再抛出）。
                set_handler_event_mutation(event);
                set_handler_event_refs(refs);
                Err(error)
            }
        }
    }

    /// `Send` 版本的异步调用：把 JS 工作投给同线程的 `LocalSet`（`spawn_local`），外层
    /// future 只持有 `oneshot::Receiver`（`Send`）。
    ///
    /// `ToolDefinitionExecute` / `VirtualModelRouteFn` 的返回值要求 `Send`
    /// （`ARCHITECTURE.md` §4.2），而 `Persistent`/`JsEngine` 是 `!Send`；调用方必须在
    /// `kiana-extensions` 的单线程 `LocalSet` 上轮询（本 crate 的既定运行方式）。
    async fn call_async_send(&self, args: Vec<JsArg>) -> Result<JsonValue, JsCallbackError> {
        let callback = *self;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::task::spawn_local(async move {
            let result = callback.call_async(args).await;
            let _ = sender.send(result);
        });
        receiver.await.map_err(|_| engine_missing_error())?
    }

    /// 释放引擎持久槽里的函数（注册被移除 / 一次性回调完成后调用）。
    ///
    /// TS 依赖 GC；Rust 的 `Persistent` 必须显式移除，否则槽位活到引擎销毁（reload /
    /// unsubscribe 会持续增长）。
    fn release(&self) {
        // 保留表（派发中退订）里的副本同样要释放，否则派发结束后会被清掉、这里提前调用
        // 也要立即生效。
        let _ = RETAINED_CALLBACKS.try_with(|callbacks| {
            if let Ok(mut callbacks) = callbacks.try_borrow_mut() {
                callbacks.remove(&self.id);
            }
        });
        // 可能从 TLS 析构（工厂缓存先于引擎销毁、或进程退出）触发：`with` 会 panic，必须
        // 用 `try_with`。
        let _ = CURRENT_ENGINES.try_with(|cell| {
            if let Ok(engines) = cell.try_borrow() {
                // id 是线程内全局唯一的：对每个仍存活的引擎尝试移除即可，命中的只会是
                // 登记该回调的那个引擎（多宿主时不会误删别的引擎，也不会漏删自己的）。
                for engine in engines.iter().filter_map(RcWeak::upgrade) {
                    engine.remove_persistent(&callback_key(self.id));
                }
            }
        });
    }

    /// 在 `with_js` 事务内把 `Ctx` 与恢复后的函数一起交给 `f`。
    ///
    /// 渲染器需要构造多个互相引用的 JS 值（args、theme、context）并在调用后读回
    /// `context.state`，因此不能只走 `build_js_args`。
    fn with_call<R>(
        &self,
        f: impl for<'js> FnOnce(&Ctx<'js>, Function<'js>) -> Result<R, JsCallbackError>,
    ) -> Result<R, JsCallbackError> {
        // 已在一个 JS 事务里（例如 JS 调用保存下来的 `next()` 时经 `eval_script` 重入）：
        // `Ctx::with` 不可嵌套，直接复用当前事务的 `Ctx`（与 `kiana.events.emit` 的同步
        // 重入路径相同）。
        if let Some(pointer) = SYNC_JS_CTX.with(|slot| slot.get()) {
            // SAFETY: 指针由 `SyncCtxGuard` 在同步调用栈内登记，指向仍然存活的 `Ctx`。
            let ctx: &Ctx<'static> = unsafe { &*(pointer as *const Ctx<'static>) };
            return self
                .with_function(ctx, |ctx, function| f(ctx, function))
                .and_then(|result| result);
        }
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        engine.with_js(
            |ctx| match self.with_function(&ctx, |ctx, function| f(ctx, function)) {
                Ok(result) => result,
                Err(error) => Err(error),
            },
        )
    }
}

fn callback_key(id: u64) -> String {
    format!("loader.api.callback.{id}")
}

/// 基础 `ctx` 的 JS 物化（未安装桥时 `undefined`）。
fn context_arg_value<'js>(
    ctx: &Ctx<'js>,
    context: Option<Arc<dyn ExtensionContextApi>>,
) -> EngineResult<Value<'js>> {
    let Some(context) = context else {
        return Ok(Value::new_undefined(ctx.clone()));
    };
    match CONTEXT_BRIDGE.with(|bridge| bridge.borrow().as_ref().map(|bridge| bridge(ctx, context)))
    {
        Some(Ok(value)) => Ok(value),
        Some(Err(_)) => Err(Exception::throw_message(
            ctx,
            "failed to materialize extension context",
        )),
        None => Ok(Value::new_undefined(ctx.clone())),
    }
}

/// 构造 JS 调用实参（`this` 为 `undefined`）。
fn build_js_args<'js>(
    ctx: &Ctx<'js>,
    args: Vec<JsArg>,
) -> Result<rquickjs::function::Args<'js>, JsCallbackError> {
    let mut collected = rquickjs::function::Args::new_unsized(ctx.clone());
    for arg in args {
        let pushed = match arg {
            JsArg::Json(json) => json_value(ctx, &json).and_then(|value| collected.push_arg(value)),
            JsArg::Undefined => collected.push_arg(Value::new_undefined(ctx.clone())),
            JsArg::Context(context) => match context_arg_value(ctx, Some(context)) {
                Ok(value) => collected.push_arg(value),
                Err(_) => return Err(js_error_from_ctx(ctx)),
            },
            JsArg::ToolContext(context) => {
                // 专用桥优先；只安装了基础桥时退回基础视图（多出的成员会缺失，但基线
                // `ExtensionContext` 仍然可用）。
                let tool = TOOL_CONTEXT_BRIDGE.with(|bridge| {
                    bridge
                        .borrow()
                        .as_ref()
                        .map(|bridge| bridge(ctx, context.clone()))
                });
                match tool {
                    Some(Ok(value)) => collected.push_arg(value),
                    Some(Err(_)) => return Err(js_error_from_ctx(ctx)),
                    None => {
                        let base: Arc<dyn ExtensionContextApi> = context;
                        match CONTEXT_BRIDGE
                            .with(|bridge| bridge.borrow().as_ref().map(|bridge| bridge(ctx, base)))
                        {
                            Some(Ok(value)) => collected.push_arg(value),
                            Some(Err(_)) => return Err(js_error_from_ctx(ctx)),
                            None => collected.push_arg(Value::new_undefined(ctx.clone())),
                        }
                    }
                }
            }
            JsArg::CommandContext(context) => {
                let command = COMMAND_CONTEXT_BRIDGE.with(|bridge| {
                    bridge
                        .borrow()
                        .as_ref()
                        .map(|bridge| bridge(ctx, context.clone()))
                });
                match command {
                    Some(Ok(value)) => collected.push_arg(value),
                    Some(Err(_)) => return Err(js_error_from_ctx(ctx)),
                    None => {
                        let base: Arc<dyn ExtensionContextApi> = context;
                        match CONTEXT_BRIDGE
                            .with(|bridge| bridge.borrow().as_ref().map(|bridge| bridge(ctx, base)))
                        {
                            Some(Ok(value)) => collected.push_arg(value),
                            Some(Err(_)) => return Err(js_error_from_ctx(ctx)),
                            None => collected.push_arg(Value::new_undefined(ctx.clone())),
                        }
                    }
                }
            }
            JsArg::Build(build) => match build(ctx) {
                Ok(value) => collected.push_arg(value),
                Err(_) => return Err(js_error_from_ctx(ctx)),
            },
            JsArg::LiveValue(pointer) => {
                // SAFETY: 只由 `call_event_handler` 在一次 `kiana.events.emit` 的同步调用栈内
                // 注入；`LiveEventGuard` 保证该 `Value<'js>` 在本调用期间存活。
                let value: &Value<'js> = unsafe { &*(pointer as *const Value<'js>) };
                collected.push_arg(value.clone())
            }
        };
        pushed.map_err(|_| js_error_from_ctx(ctx))?;
    }
    Ok(collected)
}

fn engine_missing_error() -> JsCallbackError {
    JsCallbackError {
        name: "Error".to_string(),
        message: "Extension runtime engine is not installed".to_string(),
    }
}

/// 一次 JS 调用的结果：已完成的值，或待 settle 的 Promise。
enum JsCallResult {
    Value(JsonValue),
    Pending(Persistent<Promise<'static>>),
}

/// 保留原始 JS 值的一次调用结果（`undefined` 与 `null` 可区分，函数不丢失）。
enum JsRawCall {
    Value(Persistent<Value<'static>>),
    Pending(Persistent<Promise<'static>>),
}

/// JS 调用的错误（`Error.message` / 非 Error 值的 `String(value)`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsCallbackError {
    /// `error.name`（非 Error 值为 `"Error"`）。
    pub name: String,
    /// `error.message`（非 Error 值为 `String(value)`）。
    pub message: String,
}

impl std::fmt::Display for JsCallbackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JsCallbackError {}

/// TS 注册了非函数值后，调用时的 V8 TypeError（`handler(...)` / `renderer(...)`）。
fn invalid_handler_error(message: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(JsCallbackError {
        name: "TypeError".to_string(),
        message: message.to_string(),
    })
}

impl From<rquickjs::Error> for JsCallbackError {
    fn from(error: rquickjs::Error) -> Self {
        JsCallbackError {
            name: "Error".to_string(),
            message: error.to_string(),
        }
    }
}

/// 在 `Ctx` 里构造一个 JS 实参（用于主题、`onUpdate` 等无法 JSON 化的值）。
pub type JsArgBuilder = Arc<dyn for<'js> Fn(&Ctx<'js>) -> EngineResult<Value<'js>> + Send + Sync>;

/// JS 调用的参数（只能是 Rust 拥有类型；`Context` 经 [`ContextBridge`] 物化）。
enum JsArg {
    Json(JsonValue),
    Undefined,
    Context(Arc<dyn ExtensionContextApi>),
    /// 工具 `execute()` 的 `ctx`（保留 `ExtensionToolContextApi` 的完整 vtable）。
    ToolContext(Arc<dyn ExtensionToolContextApi>),
    /// 命令 handler 的 `ctx`（保留 `ExtensionCommandContextApi` 的完整 vtable）。
    CommandContext(Arc<dyn ExtensionCommandContextApi>),
    /// 在目标 `Ctx` 里构造任意 JS 值。
    Build(JsArgBuilder),
    /// `kiana.events.emit` 派发期间的活对象（只由 [`call_event_handler`] 注入）。
    LiveValue(usize),
}

/// 当前线程最后一个仍存活的引擎（已销毁的登记顺手剔除）。
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

/// 把 `engine` 登记为当前引擎：已登记的同一引擎移到末尾（恢复为当前），否则追加。
/// 同时剔除已销毁的登记。
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

fn entry_for_id(id: u64) -> Option<Rc<ApiEntry>> {
    API_ENTRIES.with(|entries| entries.borrow().get(&id).and_then(std::rc::Weak::upgrade))
}

/// `kiana.events` 的 Rust 路径实现：把订阅登记到 `runtime.trackEventBusSubscription`
/// （TS 的 `createExtensionAPI` 对 `events` 字段做了同样的包装）。
///
/// 只持有 `entry_id`（`Send + Sync`），方法在调用线程查 [`API_ENTRIES`]。
struct ApiEventBus {
    entry_id: u64,
}

impl EventBus for ApiEventBus {
    fn emit(&self, channel: &str, data: JsonValue) {
        if let Some(entry) = entry_for_id(self.entry_id) {
            let _ = entry.events_emit_checked(channel, data);
        }
    }

    fn on(&self, channel: &str, handler: EventHandlerFn) -> Unsubscribe {
        let Some(entry) = entry_for_id(self.entry_id) else {
            return Box::new(|| {});
        };
        match entry.register_event_subscription(channel, handler, None) {
            Ok(id) => Box::new(move || {
                JS_SUBSCRIPTIONS.with(|subscriptions| {
                    if let Some((unsubscribe, callback)) = subscriptions.borrow_mut().remove(&id) {
                        unsubscribe();
                        if let Some(callback) = callback {
                            callback.release();
                        }
                    }
                });
            }),
            Err(_) => Box::new(|| {}),
        }
    }

    fn clear(&self) {
        if let Some(entry) = entry_for_id(self.entry_id) {
            entry.event_bus.clear();
        }
    }
}

/// `serde_json::Value` → JS（走 `JSON.parse`，保留键序与 JS 数字语义）。
fn json_value<'js>(ctx: &Ctx<'js>, value: &JsonValue) -> EngineResult<Value<'js>> {
    let text = serde_json::to_string(value).map_err(|error| {
        Exception::throw_message(ctx, &format!("failed to serialize value: {error}"))
    })?;
    ctx.json_parse(text)
}

/// JS 值 → `serde_json::Value`（`JSON.stringify` 语义；`undefined`/函数 → `null`）。
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

fn js_error_from_ctx(ctx: &Ctx<'_>) -> JsCallbackError {
    let error: JsError = take_exception(ctx, JsErrorKind::Host);
    // 原始异常的 stack 单独留存：`JsCallbackError` 承载不了它，但 `ExtensionError.stack`
    // 需要（TS 的 `err instanceof Error ? err.stack : undefined`）。
    set_last_js_error_stack(error.stack.clone());
    JsCallbackError {
        name: error.name,
        message: error.message,
    }
}

/// 构造 handler 收到的事件对象。
///
/// TS 的 `before_agent_start` 事件对象是 `{ ...trigger, type, get systemPrompt() {…},
/// systemPromptOptions }`：`systemPrompt` 是**访问器**（每次读取按当前选项重新渲染，且
/// 没有 setter），`images` 键在 user 触发器上始终存在（无图片时为 `undefined`）。Node/V8
/// 把访问器排在字面量里的数据属性之后，因此 getter 最后定义。
fn handler_event_object<'js>(ctx: &Ctx<'js>, event_json: &JsonValue) -> EngineResult<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    let is_before_agent_start =
        event_json.get("type").and_then(JsonValue::as_str) == Some("before_agent_start");
    let needs_images = is_before_agent_start
        && event_json.get("messageType").and_then(JsonValue::as_str) == Some("user")
        && event_json.get("images").is_none();
    if let Some(map) = event_json.as_object() {
        for (key, value) in map {
            if needs_images && key == "type" {
                object.set("images", Value::new_undefined(ctx.clone()))?;
            }
            object.set(key.clone(), json_value(ctx, value)?)?;
        }
    }
    if !is_before_agent_start {
        return Ok(object);
    }
    let original: Option<String> = object.get::<_, Option<String>>("systemPrompt")?;
    // TS 的 getter 闭包捕获 `currentOptions` 对象本身（不是 `this.systemPromptOptions`）：
    // handler 重新赋值 `event.systemPromptOptions` 不影响渲染，但对该对象的**原地修改**可见。
    let options: Value<'js> = object.get("systemPromptOptions")?;
    object.remove("systemPrompt")?;
    let getter = move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
        // dispatch.js 在 `before_agent_start` 派发期间安装当前渲染器；把捕获的选项对象传给
        // 它，这样 handler 在读取前对选项做的原地修改也能看到。直接调用（不经 dispatch.js）
        // 时退回入口时的渲染值。
        let render: Option<Function<'js>> =
            ctx.globals().get("__kiana_before_agent_start_render")?;
        if let Some(render) = render {
            return render.call::<_, Value<'js>>((options.clone(),));
        }
        match &original {
            Some(text) => {
                rquickjs::String::from_str(ctx.clone(), text).map(|text| text.into_value())
            }
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    };
    object.prop(
        "systemPrompt",
        rquickjs::object::Accessor::from(getter)
            .enumerable()
            .configurable(),
    )?;
    Ok(object)
}

/// `before_agent_start`：把物化出的 `ctx.getSystemPrompt` 换成按事件对象**当前**选项渲染。
///
/// TS: `ctx.getSystemPrompt = () => buildSystemPrompt(currentOptions)`；`currentOptions` 就是
/// handler 拿到的 `event.systemPromptOptions`（同一个对象）。跨 JSON 桥后选项是独立副本，
/// 因此在 handler 调用前覆盖 ctx 方法、读取本事件对象的选项（dispatch.js 安装在全局的
/// `__kiana_before_agent_start_render(options)`）。没有安装渲染器时调用原始方法。
fn install_before_agent_start_ctx_override<'js>(
    ctx: &Ctx<'js>,
    event: &Object<'js>,
    ctx_value: &Value<'js>,
    event_json: &JsonValue,
) -> EngineResult<()> {
    if event_json.get("type").and_then(JsonValue::as_str) != Some("before_agent_start") {
        return Ok(());
    }
    let Some(ctx_object) = ctx_value.as_object().cloned() else {
        return Ok(());
    };
    let original: Option<Function<'js>> = ctx_object.get("getSystemPrompt")?;
    if original.is_none() {
        return Ok(());
    }
    // 与事件 getter 一样捕获当前选项对象（重新赋值不影响渲染，原地修改可见）。
    let options: Value<'js> = event.get("systemPromptOptions")?;
    let getter = move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
        let render: Option<Function<'js>> =
            ctx.globals().get("__kiana_before_agent_start_render")?;
        if let Some(render) = render {
            return render.call::<_, Value<'js>>((options.clone(),));
        }
        // `makeContext` 的方法闭包捕获 `native`，不依赖 `this`。
        match &original {
            Some(original) => original.call::<_, Value<'js>>(()),
            None => Ok(Value::new_undefined(ctx.clone())),
        }
    };
    ctx_object.set("getSystemPrompt", Function::new(ctx.clone(), getter)?)?;
    Ok(())
}

/// 调用前记录事件对象各顶层属性的 JS 值（`systemPrompt` 访问器除外）。
fn snapshot_event_values<'js>(
    ctx: &Ctx<'js>,
    event: &Object<'js>,
    event_json: &JsonValue,
) -> Vec<(String, Persistent<Value<'static>>)> {
    let mut saved = Vec::new();
    let Some(map) = event_json.as_object() else {
        return saved;
    };
    for key in map.keys() {
        if key == "systemPrompt" {
            continue;
        }
        let Ok(Some(value)) = event.get::<_, Option<Value<'js>>>(key.as_str()) else {
            continue;
        };
        saved.push((key.clone(), Persistent::save(ctx, value)));
    }
    saved
}

/// 调用后比较顶层属性：`same`（可能是原地修改）、`changed`（重新赋值）、`deleted`。
///
/// dispatch.js 对逐 handler 重建的事件对象只采纳 `same` 属性的嵌套内容；缺少的键
/// （handler 新加）按 `changed` 处理。
fn event_value_refs<'js>(
    ctx: &Ctx<'js>,
    event: &Object<'js>,
    saved: &[(String, Persistent<Value<'static>>)],
) -> JsonValue {
    let mut refs = serde_json::Map::new();
    for (key, saved_value) in saved {
        let status = match (
            saved_value.clone().restore(ctx),
            event.get::<_, Option<Value<'js>>>(key.as_str()),
        ) {
            (Ok(saved_value), Ok(Some(current))) if current == saved_value => "same",
            (Ok(_), Ok(Some(_))) => "changed",
            _ => "deleted",
        };
        refs.insert(key.clone(), JsonValue::String(status.to_owned()));
    }
    JsonValue::Object(refs)
}

/// `input` handler 返回的 `images` 与调用前事件的 `images` 是否为同一 JS 引用。
///
/// TS: `currentImages = result.images ?? currentImages` 之后按引用比较
/// `currentImages !== images`；跨 JSON 桥只有这里能测出「返回的就是原数组」。
fn input_images_same_ref<'js>(
    ctx: &Ctx<'js>,
    returned: Option<&Value<'js>>,
    saved: &[(String, Persistent<Value<'static>>)],
) -> Option<bool> {
    let object = returned?.as_object()?;
    let returned_images = object
        .get::<_, Option<Value<'js>>>("images")
        .ok()
        .flatten()?;
    let (_, original) = saved.iter().find(|(key, _)| key == "images")?;
    let original = original.clone().restore(ctx).ok()?;
    Some(returned_images == original)
}

/// 记录 `event.messages` 数组本体与元素引用（TS 的 `visibleMessages.slice()`）。
///
/// 只在 `context` / `context_with_system` 的适配器里使用：调用后比较**原数组对象**的
/// 元素引用。handler 重新赋值 `event.messages` 时原数组不变，`sameMessages` 仍为 true
/// （TS 也只观察 `visibleMessages` 本身）。
fn snapshot_message_refs<'js>(
    ctx: &Ctx<'js>,
    event: &Object<'js>,
) -> Option<(Persistent<Value<'static>>, Vec<Persistent<Value<'static>>>)> {
    let messages = event.get::<_, Value<'js>>("messages").ok()?;
    let array = messages.as_array()?;
    let mut snapshot = Vec::with_capacity(array.len());
    for index in 0..array.len() {
        let value = array.get::<Value<'js>>(index).ok()?;
        snapshot.push(Persistent::save(ctx, value));
    }
    Some((Persistent::save(ctx, messages), snapshot))
}

/// TS 的 `sameMessages(current, snapshot)`：长度相同且逐元素**引用**相等。
fn messages_same_refs<'js>(
    ctx: &Ctx<'js>,
    array_value: Value<'js>,
    snapshot: &[Persistent<Value<'static>>],
) -> bool {
    let Some(array) = array_value.as_array() else {
        return false;
    };
    if array.len() != snapshot.len() {
        return false;
    }
    for (index, saved) in snapshot.iter().enumerate() {
        let Ok(value) = array.get::<Value<'js>>(index) else {
            return false;
        };
        let Ok(saved) = saved.clone().restore(ctx) else {
            return false;
        };
        // `Value` 的 `PartialEq` 比较原始 JSValue（对象按指针，即 `===`）。
        if value != saved {
            return false;
        }
    }
    true
}

/// 调用前保存的 `messages` 数组是否按引用被改动（元素增删/替换）。
fn messages_changed_for_saved_array(
    ctx: &Ctx<'_>,
    array: &Persistent<Value<'static>>,
    snapshot: &[Persistent<Value<'static>>],
) -> bool {
    match array.clone().restore(ctx) {
        Ok(value) => !messages_same_refs(ctx, value, snapshot),
        Err(_) => true,
    }
}

/// 一次事件 handler 调用需要读回的状态（跨同步调用与 Promise settle 两个 JS 事务）。
struct CapturedEvent {
    /// handler 返回的 Promise（异步 handler）。
    promise: Option<Persistent<Promise<'static>>>,
    /// handler 的原始返回值（同步返回值或 Promise settle 后的值）。
    returned: Option<Persistent<Value<'static>>>,
    /// 同一个事件对象（调用后读回）。
    event: Persistent<Object<'static>>,
    /// 同步抛出物。
    error: Option<JsCallbackError>,
    /// 调用前各顶层属性的 JS 值。
    saved_values: Vec<(String, Persistent<Value<'static>>)>,
    /// 调用前 `event.messages` 数组本体。
    messages_array: Option<Persistent<Value<'static>>>,
    /// 调用前 `event.messages` 的元素引用。
    messages_snapshot: Option<Vec<Persistent<Value<'static>>>>,
}

/// 驱动一个 JS Promise 到 settle，返回 resolved 值的 JSON。
///
/// 这是本模块所有异步 JS 调用（handler / 工厂 / `streamSimple` 迭代）共用的等待器：
/// 每轮在 `with_js` 里临时 `restore` Promise，JS 值不跨出事务。
async fn await_js_promise(
    engine: &JsEngine,
    promise: Persistent<Promise<'static>>,
) -> Result<JsonValue, JsCallbackError> {
    loop {
        // 每轮先看状态；Pending 时依次给微任务、宿主完成队列、到期定时器机会，仍有
        // 待等的工作才阻塞（完成事件到达或下一个定时器到期）。
        let state = engine.with_js(|ctx| -> Result<PromiseView, JsCallbackError> {
            let promise = promise
                .clone()
                .restore(&ctx)
                .map_err(|error| JsCallbackError {
                    name: "Error".to_string(),
                    message: error.to_string(),
                })?;
            match promise.state() {
                rquickjs::promise::PromiseState::Resolved => {
                    let value = promise
                        .result::<Value<'_>>()
                        .expect("resolved promise has a result")
                        .map_err(|_| js_error_from_ctx(&ctx))?;
                    let json = value_to_json(&ctx, value).map_err(|_| js_error_from_ctx(&ctx))?;
                    Ok(PromiseView::Value(json))
                }
                rquickjs::promise::PromiseState::Rejected => {
                    let _ = promise.result::<Value<'_>>();
                    Err(js_error_from_ctx(&ctx))
                }
                rquickjs::promise::PromiseState::Pending => Ok(PromiseView::Pending),
            }
        })?;
        match state {
            PromiseView::Value(value) => return Ok(value),
            PromiseView::Pending => {}
        }

        if engine.pump_jobs() > 0 {
            continue;
        }
        if engine.try_drain_completions() > 0 {
            continue;
        }
        if let Some(deadline) = engine.next_timer_deadline()
            && deadline <= tokio::time::Instant::now()
        {
            engine.fire_due_timers();
            continue;
        }
        match engine.next_timer_deadline() {
            Some(deadline) => {
                tokio::select! {
                    _ = engine.pump_completions() => {}
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            }
            None => {
                // 没有宿主任务也没有定时器时仍等待完成队列：宿主任务（fetch / exec / 子进程）
                // 完成时会唤醒这里。
                engine.pump_completions().await;
            }
        }
    }
}

/// [`await_js_promise`] 的原始值版本：resolved 的 JS `Value` 以 `Persistent` 返回，
/// 调用方可以在 `with_js` 里恢复后判断 `undefined`/`null`/函数。
///
/// 轮询顺序与 [`await_js_promise`] 相同；区别只在 resolved 值不经过 `JSON.stringify`，
/// 因此 `undefined`、函数等保真（`user_bash` 的 `{ operations }` 与事件 handler 依赖它）。
async fn await_js_promise_value(
    engine: &JsEngine,
    promise: Persistent<Promise<'static>>,
) -> Result<Persistent<Value<'static>>, JsCallbackError> {
    loop {
        // 与 [`await_js_promise`] 相同的推进顺序：状态检查 → 微任务 → 宿主完成队列 →
        // 到期定时器 → 阻塞等待。
        let state = engine.with_js(|ctx| -> Result<RawPromiseView, JsCallbackError> {
            let promise = promise
                .clone()
                .restore(&ctx)
                .map_err(|error| JsCallbackError {
                    name: "Error".to_string(),
                    message: error.to_string(),
                })?;
            match promise.state() {
                rquickjs::promise::PromiseState::Resolved => {
                    let value = promise
                        .result::<Value<'_>>()
                        .expect("resolved promise has a result")
                        .map_err(|_| js_error_from_ctx(&ctx))?;
                    Ok(RawPromiseView::Value(Persistent::save(&ctx, value)))
                }
                rquickjs::promise::PromiseState::Rejected => {
                    let _ = promise.result::<Value<'_>>();
                    Err(js_error_from_ctx(&ctx))
                }
                rquickjs::promise::PromiseState::Pending => Ok(RawPromiseView::Pending),
            }
        })?;
        match state {
            RawPromiseView::Value(value) => return Ok(value),
            RawPromiseView::Pending => {}
        }

        if engine.pump_jobs() > 0 {
            continue;
        }
        if engine.try_drain_completions() > 0 {
            continue;
        }
        if let Some(deadline) = engine.next_timer_deadline()
            && deadline <= tokio::time::Instant::now()
        {
            engine.fire_due_timers();
            continue;
        }
        match engine.next_timer_deadline() {
            Some(deadline) => {
                tokio::select! {
                    _ = engine.pump_completions() => {}
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            }
            None => {
                engine.pump_completions().await;
            }
        }
    }
}

enum RawPromiseView {
    Value(Persistent<Value<'static>>),
    Pending,
}

enum PromiseView {
    Value(JsonValue),
    Pending,
}

// ===========================================================================
// JS 值 → kiana-ext-api 类型（注册期转换）
// ===========================================================================

fn get_string<'js>(object: &Object<'js>, key: &str) -> EngineResult<Option<String>> {
    match object.get::<_, Option<Value<'js>>>(key)? {
        Some(value) => match value.as_string() {
            Some(text) => Ok(Some(text.to_string()?)),
            None => Ok(None),
        },
        None => Ok(None),
    }
}

fn get_json<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> EngineResult<Option<JsonValue>> {
    match object.get::<_, Option<Value<'js>>>(key)? {
        Some(value) if !value.is_undefined() && !value.is_null() => {
            value_to_json(ctx, value).map(Some)
        }
        _ => Ok(None),
    }
}

fn get_function<'js>(object: &Object<'js>, key: &str) -> EngineResult<Option<Function<'js>>> {
    Ok(object
        .get::<_, Option<Value<'js>>>(key)?
        .and_then(|value| value.as_function().cloned()))
}

fn parse_json<T: serde::de::DeserializeOwned>(
    ctx: &Ctx<'_>,
    value: JsonValue,
    what: &str,
) -> EngineResult<T> {
    serde_json::from_value(value)
        .map_err(|error| Exception::throw_message(ctx, &format!("invalid {what}: {error}")))
}

/// 字符串键的规范化形式（TS `Map` 里 `registerFlag("5", …)` 的键）。
fn string_map_key(name: &str) -> String {
    format!("s:{name}")
}

/// JS `Map` 键的规范化形式：`5` 与 `"5"` 是**不同**的键（SameValueZero），Rust 的
/// `IndexMap<String, _>` 无法表达，因此键带类型前缀。
///
/// 对象键在 TS 里按**引用**比较；这里退化为 `String(value)`（`"[object Object]"`）。
/// 只有 flag 的 `registerFlag`/`getFlag` 使用本键，其它注册（事件名、渲染器 customType）
/// 在运行时只按字符串查找，不会观察到引用键的差异。
fn js_map_key(value: &Value<'_>) -> String {
    match value.type_of() {
        rquickjs::Type::String => format!(
            "s:{}",
            value
                .as_string()
                .and_then(|text| text.to_string().ok())
                .unwrap_or_default()
        ),
        rquickjs::Type::Int => format!("n:{}", value.as_int().unwrap_or_default()),
        rquickjs::Type::Float => {
            format!(
                "n:{}",
                kiana_json::format_js_number(value.as_float().unwrap_or(f64::NAN))
            )
        }
        rquickjs::Type::Bool => format!("b:{}", value.as_bool().unwrap_or(false)),
        rquickjs::Type::Null => "null".to_string(),
        rquickjs::Type::Undefined => "undefined".to_string(),
        rquickjs::Type::Symbol => "symbol".to_string(),
        rquickjs::Type::BigInt => format!(
            "bigint:{}",
            js_string_coerce(value).unwrap_or_else(|| "0".to_string())
        ),
        _ => format!("o:{}", js_string_coerce(value).unwrap_or_default()),
    }
}

/// JS `String(value)`（在引擎里调用全局 `String`；失败时返回 `None`）。
fn js_string_coerce(value: &Value<'_>) -> Option<String> {
    let ctx = value.ctx().clone();
    let to_string: Function<'_> = ctx.globals().get("String").ok()?;
    to_string.call::<_, String>((value.clone(),)).ok()
}

/// TS 模板字符串插值（`${value}`）：`undefined`/`null` 的文本与 `String()` 相同，
/// 但 `String(null)` 也是 `"null"`，这里显式区分只是为了不依赖引擎调用。
fn js_template_value(value: &Value<'_>) -> String {
    if value.is_undefined() {
        "undefined".to_string()
    } else if value.is_null() {
        "null".to_string()
    } else {
        js_string_coerce(value).unwrap_or_default()
    }
}

/// JS `typeof` 的名字（flag 默认值校验错误里逐字使用）。
fn js_typeof(value: &Value<'_>) -> &'static str {
    match value.type_of() {
        rquickjs::Type::Bool => "boolean",
        rquickjs::Type::Int | rquickjs::Type::Float => "number",
        rquickjs::Type::String => "string",
        rquickjs::Type::Function => "function",
        rquickjs::Type::Undefined => "undefined",
        rquickjs::Type::Null | rquickjs::Type::Array | rquickjs::Type::Object => "object",
        rquickjs::Type::Symbol => "symbol",
        rquickjs::Type::BigInt => "bigint",
        rquickjs::Type::Exception => "object",
        rquickjs::Type::Constructor => "function",
        rquickjs::Type::Promise => "object",
        _ => "object",
    }
}

/// 缺省 `execute` 的占位（JS 工具没写 execute 时调用会得到 TS TypeError 等价的错误）。
struct MissingToolExecute {
    name: String,
}

impl ToolDefinitionExecute for MissingToolExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        let message = format!("tool.execute is not a function: {}", self.name);
        Box::pin(async move {
            Err(AgentToolError {
                message,
                name: Some("TypeError".to_string()),
                stack: None,
            })
        })
    }
}

/// 把 [`CancellationToken`] 物化成 JS `AbortSignal`。
///
/// 优先用 `js/context.js` 的 `__kiana_ctx.signal(handle)`（`instanceof AbortSignal`、`reason`、
/// `AbortSignal.any` 都成立；按 16 ms 轮询宿主 token）。context 单元未安装时退回一个普通的
/// `AbortController` + 一个 `spawn_local` 观察者（仅在 LocalSet 上可用）。
fn abort_signal_value<'js>(
    ctx: &Ctx<'js>,
    token: &tokio_util::sync::CancellationToken,
) -> EngineResult<Value<'js>> {
    if let Ok(Some(kiana_ctx)) = ctx.globals().get::<_, Option<Object<'js>>>("__kiana_ctx")
        && let Ok(signal) = kiana_ctx.get::<_, Function<'js>>("signal")
    {
        let handle = crate::runner::context::register_signal_handle(token);
        return signal.call((handle,));
    }

    let controller: Object<'js> = ctx.eval("new AbortController()")?;
    let signal: Value<'js> = controller.get("signal")?;
    if token.is_cancelled() {
        let abort: Function<'js> = ctx.eval("(controller) => controller.abort()")?;
        abort.call::<_, ()>((controller,))?;
        return Ok(signal);
    }
    let make: Function<'js> = ctx.eval("(controller) => () => controller.abort()")?;
    let abort: Function<'js> = make.call((controller,))?;
    let callback = JsCallback::new(ctx, abort);
    let token = token.clone();
    tokio::task::spawn_local(async move {
        token.cancelled().await;
        let _ = callback.call(Vec::new());
        callback.release();
    });
    Ok(signal)
}

/// 把 `onUpdate` 物化成 JS 函数：JS 侧调用时把参数解析成 [`AgentToolResult`] 后转给宿主。
fn on_update_function<'js>(
    ctx: &Ctx<'js>,
    callback: AgentToolUpdateCallback,
) -> EngineResult<Function<'js>> {
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, update: Value<'js>| -> EngineResult<()> {
            let json = value_to_json(&ctx, update)?;
            let result: AgentToolResult<JsonValue> =
                serde_json::from_value(json).map_err(|error| {
                    Exception::throw_message(&ctx, &format!("invalid tool update: {error}"))
                })?;
            callback(result);
            Ok(())
        },
    )
}

fn on_update_value(on_update: AgentToolUpdateCallback) -> JsArgBuilder {
    Arc::new(move |ctx: &Ctx<'_>| -> EngineResult<Value<'_>> {
        Ok(on_update_function(ctx, Arc::clone(&on_update))?.into_value())
    })
}

/// JS `tool.execute(...)` 的适配器。
struct JsToolExecute {
    callback: JsCallback,
}

impl ToolDefinitionExecute for JsToolExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a tokio_util::sync::CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        let callback = self.callback;
        let tool_call_id = tool_call_id.to_string();
        let signal = signal.cloned();
        Box::pin(async move {
            // `signal` / `onUpdate` 无法 JSON 化：用 `Build` 在目标 `Ctx` 里现造 JS 值。
            let signal_arg = match signal {
                Some(token) => JsArg::Build(Arc::new(move |ctx: &Ctx<'_>| {
                    abort_signal_value(ctx, &token)
                })),
                None => JsArg::Undefined,
            };
            let update_arg = match on_update {
                Some(on_update) => JsArg::Build(on_update_value(on_update)),
                None => JsArg::Undefined,
            };
            let args = vec![
                JsArg::Json(JsonValue::String(tool_call_id)),
                JsArg::Json(params),
                signal_arg,
                update_arg,
                JsArg::ToolContext(ctx),
            ];
            let value = callback
                .call_async_send(args)
                .await
                .map_err(|error| AgentToolError {
                    message: error.message,
                    name: Some(error.name),
                    stack: None,
                })?;
            // TS：`ToolResultMessage.content = finalized.result.content ?? []`（未类型化 JS
            // 工具可以省略 content，agent loop 用空数组兜底）。这里在反序列化前补齐，避免
            // 把缺 content 的工具结果误判为非法结果（lax-message-content.test.ts case 1）。
            let value = match value {
                JsonValue::Object(mut object) => {
                    if object.get("content").is_none_or(JsonValue::is_null) {
                        object.insert("content".to_owned(), JsonValue::Array(Vec::new()));
                    }
                    JsonValue::Object(object)
                }
                other => other,
            };
            serde_json::from_value::<AgentToolResult<JsonValue>>(value).map_err(|error| {
                AgentToolError {
                    message: format!("invalid tool result: {error}"),
                    name: Some("TypeError".to_string()),
                    stack: None,
                }
            })
        })
    }
}

/// JS `prepareLoadout(loadout)` 适配器。
///
/// 三个集合是 `AgentTool[]` 的 JS 视图；三个按名查询是同步函数。TS 的 `loadout` 在
/// 本次同步调用期间有效，但扩展可以把它保存下来（例如 `saved = loadout`）并在之后调用
/// 查询方法；因此这里把查询所需的映射**复制**进 JS 闭包，而不是捕获指向会话栈帧的
/// 借用。这样即使 `prepareLoadout` 已返回、`SessionToolLoadout` 已释放，闭包仍然只
/// 访问自己拥有的数据，不会出现 use-after-free。
fn js_prepare_loadout<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
) -> kiana_ext_api::tool_types::PrepareLoadoutFn {
    let callback = JsCallback::new(ctx, function);
    Arc::new(
        move |loadout: &dyn kiana_ext_api::tool_types::ToolLoadout| {
            callback
                .with_call(|ctx, function| {
                    let declared = loadout.declared().to_vec();
                    let callable = loadout.callable().to_vec();
                    let registered = loadout.registered().to_vec();

                    // 按工具名快照三个查询方法的返回值。`registered()` 覆盖全部已注册
                    // 工具；`declared()`/`callable()` 是它的子集，但仍显式并入，避免
                    // 某个实现让两者不同步时丢掉名字。
                    let mut names: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for tool in declared
                        .iter()
                        .chain(callable.iter())
                        .chain(registered.iter())
                    {
                        names.insert(tool.name.clone());
                    }
                    let mut exposures = HashMap::new();
                    let mut namespaces = HashMap::new();
                    let mut prompt_guidelines = HashMap::new();
                    for name in names {
                        exposures.insert(name.clone(), loadout.get_exposure(&name));
                        if let Some(namespace) = loadout.get_namespace(&name) {
                            namespaces.insert(name.clone(), namespace.clone());
                        }
                        prompt_guidelines
                            .insert(name.clone(), loadout.get_prompt_guidelines(&name).to_vec());
                    }

                    let object = Object::new(ctx.clone())?;
                    object.set("declared", agent_tools_value(ctx, &declared)?)?;
                    object.set("callable", agent_tools_value(ctx, &callable)?)?;
                    object.set("registered", agent_tools_value(ctx, &registered)?)?;
                    object.set(
                        "getExposure",
                        Function::new(ctx.clone(), move |name: String| -> EngineResult<String> {
                            let exposure = exposures
                                .get(&name)
                                .copied()
                                .unwrap_or(kiana_ext_api::tool_types::ToolExposure::Direct);
                            let value = serde_json::to_value(exposure).unwrap_or(JsonValue::Null);
                            Ok(value.as_str().unwrap_or("direct").to_string())
                        })?,
                    )?;
                    object.set("getNamespace", loadout_namespace_function(ctx, namespaces)?)?;
                    object.set(
                        "getPromptGuidelines",
                        loadout_guidelines_function(ctx, prompt_guidelines)?,
                    )?;
                    let mut args = rquickjs::function::Args::new_unsized(ctx.clone());
                    args.push_arg(object).map_err(|_| js_error_from_ctx(ctx))?;
                    let value = function
                        .call_arg::<Value<'_>>(args)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    if value.is_null() || value.is_undefined() {
                        return Ok(None);
                    }
                    let json = value_to_json(ctx, value).map_err(|_| js_error_from_ctx(ctx))?;
                    serde_json::from_value(json)
                        .map(Some)
                        .map_err(|_| js_error_from_ctx(ctx))
                })
                .ok()
                .flatten()
        },
    )
}

/// `ToolLoadout.getNamespace(name)` 的 JS 函数。
fn loadout_namespace_function<'js>(
    ctx: &Ctx<'js>,
    namespaces: HashMap<String, kiana_ext_api::tool_types::ToolNamespace>,
) -> EngineResult<Function<'js>> {
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, name: String| -> EngineResult<Value<'js>> {
            match namespaces.get(&name) {
                Some(namespace) => json_value(
                    &ctx,
                    &serde_json::to_value(namespace).unwrap_or(JsonValue::Null),
                ),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )
}

/// `ToolLoadout.getPromptGuidelines(name)` 的 JS 函数。
fn loadout_guidelines_function<'js>(
    ctx: &Ctx<'js>,
    guidelines: HashMap<String, Vec<String>>,
) -> EngineResult<Function<'js>> {
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, name: String| -> EngineResult<Value<'js>> {
            let values = guidelines.get(&name).cloned().unwrap_or_default();
            json_value(
                &ctx,
                &serde_json::to_value(values).unwrap_or_else(|_| JsonValue::Array(Vec::new())),
            )
        },
    )
}

/// JS 工具定义 → [`ToolDefinition`]（TS 的 `registerTool({...})`）。
///
/// 逐字段读取；`execute` 缺省时用 `MissingToolExecute`（调用时返回 `TypeError` 等价的错误，
/// 与 TS 调用非函数成员一致），其余回调缺省即 `None`。`parameters` 缺省为
/// `JsonValue::Null`，对象校验由调用方（`register_tool_checked` / host `registerTool`）负责。
fn tool_definition_from_js<'js>(
    ctx: &Ctx<'js>,
    tool: &Object<'js>,
) -> EngineResult<ToolDefinition> {
    let name = get_string(tool, "name")?.unwrap_or_default();
    let label = get_string(tool, "label")?.unwrap_or_else(|| name.clone());
    let description = get_string(tool, "description")?.unwrap_or_default();
    let prompt_snippet = get_string(tool, "promptSnippet")?;
    let prompt_guidelines = match get_json(ctx, tool, "promptGuidelines")? {
        Some(value) => Some(parse_json::<Vec<String>>(
            &ctx.clone(),
            value,
            "promptGuidelines",
        )?),
        None => None,
    };
    let parameters = get_json(ctx, tool, "parameters")?.unwrap_or(JsonValue::Null);
    let constrained_sampling = get_json(ctx, tool, "constrainedSampling")?
        .map(|value| parse_json(&ctx.clone(), value, "constrainedSampling"))
        .transpose()?;
    let render_shell = get_json(ctx, tool, "renderShell")?
        .map(|value| parse_json(&ctx.clone(), value, "renderShell"))
        .transpose()?;
    let prepare_arguments = get_function(tool, "prepareArguments")?.map(|function| {
        let callback = JsCallback::new(ctx, function);
        Arc::new(move |params: &mut JsonValue| -> Option<JsonValue> {
            let value = callback.call(vec![JsArg::Json(params.clone())]).ok()?;
            match value {
                JsCallResult::Value(JsonValue::Null) => None,
                JsCallResult::Value(value) => {
                    *params = value.clone();
                    Some(value)
                }
                JsCallResult::Pending(_) => None,
            }
        }) as kiana_ext_api::tool_types::PrepareArgumentsFn
    });
    let output_schema = get_json(ctx, tool, "outputSchema")?;
    let exposure = get_json(ctx, tool, "exposure")?
        .map(|value| parse_json(&ctx.clone(), value, "exposure"))
        .transpose()?;
    let namespace = get_json(ctx, tool, "namespace")?
        .map(|value| parse_json(&ctx.clone(), value, "namespace"))
        .transpose()?;
    let annotations = get_json(ctx, tool, "annotations")?
        .map(|value| parse_json(&ctx.clone(), value, "annotations"))
        .transpose()?;
    let default_active = match tool.get::<_, Option<Value<'js>>>("defaultActive")? {
        Some(value) => value.as_bool(),
        None => None,
    };
    let execution_mode = get_json(ctx, tool, "executionMode")?
        .map(|value| parse_json(&ctx.clone(), value, "executionMode"))
        .transpose()?;
    let reaches_tools = get_function(tool, "reachesTools")?.map(|function| {
        let callback = JsCallback::new(ctx, function);
        Arc::new(
            move |input: &JsonValue| -> kiana_ext_api::tool_types::ReachesToolsResult {
                match callback.call(vec![JsArg::Json(input.clone())]) {
                    Ok(JsCallResult::Value(JsonValue::String(text))) if text == "all" => {
                        kiana_ext_api::tool_types::ReachesToolsResult::All
                    }
                    Ok(JsCallResult::Value(JsonValue::Array(names))) => {
                        kiana_ext_api::tool_types::ReachesToolsResult::Names(
                            names
                                .into_iter()
                                .filter_map(|name| name.as_str().map(str::to_string))
                                .collect(),
                        )
                    }
                    _ => kiana_ext_api::tool_types::ReachesToolsResult::Names(Vec::new()),
                }
            },
        ) as kiana_ext_api::tool_types::ReachesToolsFn
    });
    let execute: Arc<dyn ToolDefinitionExecute> = match get_function(tool, "execute")? {
        Some(function) => Arc::new(JsToolExecute {
            // TS `definition.execute(...)`（tool-definition-wrapper.ts:22）以工具定义对象
            // 为 `this`；扩展把辅助数据挂在工具对象上时依赖它。
            callback: JsCallback::new_with_this(ctx, function, tool.clone().into_value()),
        }),
        None => Arc::new(MissingToolExecute { name: name.clone() }),
    };
    let render_call =
        get_function(tool, "renderCall")?.map(|function| js_render_call(ctx, function));
    let render_result =
        get_function(tool, "renderResult")?.map(|function| js_render_result(ctx, function));
    let prepare_loadout =
        get_function(tool, "prepareLoadout")?.map(|function| js_prepare_loadout(ctx, function));

    Ok(ToolDefinition {
        name,
        label,
        description,
        prompt_snippet,
        prompt_guidelines,
        parameters,
        constrained_sampling,
        render_shell,
        prepare_arguments,
        output_schema,
        exposure,
        namespace,
        reaches_tools,
        annotations,
        default_active,
        prepare_loadout,
        execution_mode,
        execute,
        render_call,
        render_result,
    })
}

/// JS 渲染器的通用调用：同步执行，非 null 值交给 [`ComponentBridge`]。
fn call_component_bridge<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> Option<Box<dyn kiana_tui::tui::Component>> {
    COMPONENT_BRIDGE.with(|bridge| {
        let bridge = bridge.borrow().clone()?;
        match bridge(ctx, value) {
            Ok(component) => Some(component),
            Err(_) => {
                let _ = ctx.catch();
                None
            }
        }
    })
}

/// 主题的 JS 物化（未安装 [`ThemeBridge`] 时为 `undefined`）。
fn theme_value<'js>(ctx: &Ctx<'js>, theme: &kiana_theme::theme::Theme) -> EngineResult<Value<'js>> {
    let bridge = THEME_BRIDGE.with(|slot| slot.borrow().clone());
    match bridge {
        Some(bridge) => bridge(ctx, theme),
        None => Ok(Value::new_undefined(ctx.clone())),
    }
}

/// `ToolRenderContext` → JS 对象（字段名/顺序为当前稳定契约，改动需同步测试）。
fn tool_render_context_value<'js>(
    ctx: &Ctx<'js>,
    context: &kiana_ext_api::tool_types::ToolRenderContext,
) -> EngineResult<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("args", json_value(ctx, &context.args)?)?;
    object.set("toolCallId", context.tool_call_id.clone())?;
    let invalidate = Arc::clone(&context.invalidate);
    object.set(
        "invalidate",
        Function::new(ctx.clone(), move || invalidate())?,
    )?;
    // TS `context.lastComponent`：Rust 组件无法反向物化成 JS，按 `undefined` 给出。
    object.set("lastComponent", Value::new_undefined(ctx.clone()))?;
    object.set("state", json_value(ctx, &lock(&context.state).clone())?)?;
    object.set("cwd", context.cwd.to_string_lossy().into_owned())?;
    object.set("executionStarted", context.execution_started)?;
    object.set("argsComplete", context.args_complete)?;
    object.set("isPartial", context.is_partial)?;
    object.set("expanded", context.expanded)?;
    object.set("showImages", context.show_images)?;
    object.set("isError", context.is_error)?;
    match context.duration_ms {
        Some(duration) => object.set("durationMs", duration)?,
        None => object.set("durationMs", Value::new_undefined(ctx.clone()))?,
    }
    object.set("outputPad", context.output_pad)?;
    Ok(object)
}

/// 渲染器可能就地修改 `context.state`（TS 的 `TState` 共享对象）；调用后写回。
fn store_render_state<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    context: &kiana_ext_api::tool_types::ToolRenderContext,
) {
    if let Ok(value) = object.get::<_, Value<'js>>("state")
        && let Ok(json) = value_to_json(ctx, value)
    {
        *lock(&context.state) = json;
    }
}

/// 调一个渲染器函数并物化组件（用调用方提供的 `Ctx`）。
fn render_component_with<'js>(
    ctx: &Ctx<'js>,
    function: &Function<'js>,
    args: rquickjs::function::Args<'js>,
) -> Option<Box<dyn kiana_tui::tui::Component>> {
    match function.call_arg::<Value<'js>>(args) {
        Ok(value) => {
            if value.is_null() || value.is_undefined() {
                return None;
            }
            call_component_bridge(ctx, value)
        }
        Err(_) => {
            let _ = ctx.catch();
            None
        }
    }
}

/// JS `renderCall(args, theme, context)` → 组件。
fn js_render_call<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
) -> kiana_ext_api::tool_types::RenderCallFn {
    let callback = JsCallback::new(ctx, function);
    let renderer: kiana_ext_api::tool_types::RenderCallFn = Arc::new(
        move |args: &JsonValue,
              theme: &kiana_theme::theme::Theme,
              context: &kiana_ext_api::tool_types::ToolRenderContext| {
            callback
                .with_call(|ctx, function| {
                    let theme = theme_value(ctx, theme).map_err(|_| js_error_from_ctx(ctx))?;
                    let render_context = tool_render_context_value(ctx, context)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    let mut collected = rquickjs::function::Args::new_unsized(ctx.clone());
                    collected
                        .push_arg(json_value(ctx, args).map_err(|_| js_error_from_ctx(ctx))?)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    collected
                        .push_arg(theme)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    collected
                        .push_arg(render_context.clone())
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    let component = render_component_with(ctx, &function, collected);
                    store_render_state(ctx, &render_context, context);
                    Ok(component)
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| Box::new(NullComponent))
        },
    );
    register_renderer(&renderer, callback);
    renderer
}

/// JS `renderResult(result, options, theme, context)` → 组件。
fn js_render_result<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
) -> kiana_ext_api::tool_types::RenderResultFn {
    let callback = JsCallback::new(ctx, function);
    let renderer: kiana_ext_api::tool_types::RenderResultFn = Arc::new(
        move |result: &AgentToolResult<JsonValue>,
              options: &kiana_ext_api::tool_types::ToolRenderResultOptions,
              theme: &kiana_theme::theme::Theme,
              context: &kiana_ext_api::tool_types::ToolRenderContext| {
            callback
                .with_call(|ctx, function| {
                    let theme = theme_value(ctx, theme).map_err(|_| js_error_from_ctx(ctx))?;
                    let render_context = tool_render_context_value(ctx, context)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    let result_json = serde_json::to_value(result).unwrap_or(JsonValue::Null);
                    let options_json = serde_json::to_value(options).unwrap_or(JsonValue::Null);
                    let mut collected = rquickjs::function::Args::new_unsized(ctx.clone());
                    for value in [
                        json_value(ctx, &result_json),
                        json_value(ctx, &options_json),
                    ] {
                        collected
                            .push_arg(value.map_err(|_| js_error_from_ctx(ctx))?)
                            .map_err(|_| js_error_from_ctx(ctx))?;
                    }
                    collected
                        .push_arg(theme)
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    collected
                        .push_arg(render_context.clone())
                        .map_err(|_| js_error_from_ctx(ctx))?;
                    let component = render_component_with(ctx, &function, collected);
                    store_render_state(ctx, &render_context, context);
                    Ok(component)
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| Box::new(NullComponent))
        },
    );
    register_renderer(&renderer, callback);
    renderer
}

thread_local! {
    /// JS 定义的渲染器闭包地址 → 原始 JS 函数。
    ///
    /// `ToolRendererResolver` 的 `next()` 需要把 Rust 的 `ToolRenderers` 还原成 JS 对象；
    /// 对 JS 注册的渲染器可以恢复原函数（引用身份一致）。Rust 内置渲染器没有 JS 函数，
    /// 在 `next()` 结果里按 `undefined` 给出（已记录的桥接缺口：需要 ui 单元的反向组件桥）。
    static RENDERER_REGISTRY: RefCell<HashMap<usize, JsCallback>> = RefCell::new(HashMap::new());
}

/// 泛型注册（两种渲染器闭包类型不同，指针都取数据地址）。
fn register_renderer<T: ?Sized>(renderer: &Arc<T>, callback: JsCallback) {
    RENDERER_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(Arc::as_ptr(renderer) as *const () as usize, callback);
    });
}

/// 渲染器闭包 → 当前 `Ctx` 里的 JS 函数（非 JS 注册的渲染器返回 `None`）。
fn renderer_js_function<'js>(ctx: &Ctx<'js>, key: usize) -> Option<Function<'js>> {
    let callback = RENDERER_REGISTRY.with(|registry| registry.borrow().get(&key).copied())?;
    callback.with_function(ctx, |_ctx, function| function).ok()
}

/// `ToolRenderers` → JS 值（resolver 的 `next()`）。
fn tool_renderers_value<'js>(
    ctx: &Ctx<'js>,
    renderers: &kiana_ext_api::tool_types::ToolRenderers,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    match &renderers.render_shell {
        Some(shell) => object.set(
            "renderShell",
            json_value(ctx, &serde_json::to_value(shell).unwrap_or(JsonValue::Null))?,
        )?,
        None => object.set("renderShell", Value::new_undefined(ctx.clone()))?,
    }
    match &renderers.render_call {
        Some(render_call) => {
            let key = Arc::as_ptr(render_call) as *const () as usize;
            match renderer_js_function(ctx, key) {
                Some(function) => object.set("renderCall", function)?,
                None => object.set("renderCall", Value::new_undefined(ctx.clone()))?,
            }
        }
        None => object.set("renderCall", Value::new_undefined(ctx.clone()))?,
    }
    match &renderers.render_result {
        Some(render_result) => {
            let key = Arc::as_ptr(render_result) as *const () as usize;
            match renderer_js_function(ctx, key) {
                Some(function) => object.set("renderResult", function)?,
                None => object.set("renderResult", Value::new_undefined(ctx.clone()))?,
            }
        }
        None => object.set("renderResult", Value::new_undefined(ctx.clone()))?,
    }
    Ok(object.into_value())
}

/// 无组件桥时的占位组件（空渲染）。渲染器桥接缺口在报告里单列。
struct NullComponent;

impl kiana_tui::tui::Component for NullComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn handles_input(&self) -> bool {
        false
    }
}

// ===========================================================================
// JS handler / command / shortcut / renderer 适配器
// ===========================================================================

fn event_kind_for_name(event: &str) -> Option<ExtensionEventKind> {
    ExtensionEventKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == event)
}

/// 按 JS 语义归一化 handler 结果里 TS 只按“真值/字符串”使用的字段（见
/// [`handler_result_from_json`] 的注释）。
///
/// `runner/dispatch.rs` 用同一函数处理 fake-host / 原生 handler 路径的最终结果。
pub(crate) fn coerce_handler_result(kind: ExtensionEventKind, value: JsonValue) -> JsonValue {
    let JsonValue::Object(mut object) = value else {
        return value;
    };
    match kind {
        ExtensionEventKind::SessionBeforeSwitch
        | ExtensionEventKind::SessionBeforeFork
        | ExtensionEventKind::SessionBeforeCompact
        | ExtensionEventKind::SessionBeforeTree
        | ExtensionEventKind::TurnEnd
        | ExtensionEventKind::AgentBeforeSettle => {
            for key in [
                "cancel",
                "continue",
                "skipConversationRestore",
                "replaceInstructions",
            ] {
                if let Some(field) = object.get(key)
                    && !field.is_boolean()
                {
                    let truthy = kiana_json::is_truthy(Some(field));
                    object.insert(key.to_owned(), JsonValue::Bool(truthy));
                }
            }
            if matches!(kind, ExtensionEventKind::SessionBeforeTree)
                && let Some(summary) = object.get_mut("summary")
            {
                match summary {
                    JsonValue::Object(summary_object) => {
                        // 部分 `usage` 在 TS 里原样传递；Rust 的 `Usage` 要求全部字段，
                        // 解析失败时丢弃它（TS 的消费者也只读其中的数字字段）。
                        if let Some(usage) = summary_object.get("usage")
                            && serde_json::from_value::<kiana_ai::types::Usage>(usage.clone())
                                .is_err()
                        {
                            summary_object.shift_remove("usage");
                        }
                        if let Some(text) = summary_object.get("summary")
                            && !text.is_string()
                        {
                            let text = kiana_json::js_string(Some(text));
                            summary_object.insert("summary".to_owned(), JsonValue::String(text));
                        }
                    }
                    // 非对象摘要没有可用的 `summary` 文本：忽略。
                    _ => {
                        object.shift_remove("summary");
                    }
                }
            }
        }
        ExtensionEventKind::BeforeAgentStart => {
            // TS: `_prepareRun` 把注入消息的 `content` 归一为 `content ?? []`（未类型化扩展
            // 可能传 null/缺省）。
            if let Some(JsonValue::Object(message)) = object.get_mut("message")
                && message.get("content").is_none_or(JsonValue::is_null)
            {
                message.insert("content".to_owned(), JsonValue::Array(Vec::new()));
            }
        }
        ExtensionEventKind::ToolResult => {
            // TS 的补丁对象是无类型的：`content` 可以是任意值（非数组时整条结果仍会返回给
            // 调用方）。Rust 的 `OptionalPatch<Vec<UserContentBlock>>` 无法承载，非数组时
            // 忽略该补丁（调用方保留原 content），而不是拒绝整条结果。
            if let Some(content) = object.get("content")
                && !content.is_array()
                && !content.is_null()
            {
                object.shift_remove("content");
            }
            if let Some(is_error) = object.get("isError")
                && !is_error.is_boolean()
                && !is_error.is_null()
            {
                let truthy = kiana_json::is_truthy(Some(is_error));
                object.insert("isError".to_owned(), JsonValue::Bool(truthy));
            }
            if let Some(usage) = object.get("usage")
                && !usage.is_null()
                && serde_json::from_value::<kiana_ai::types::Usage>(usage.clone()).is_err()
            {
                object.shift_remove("usage");
            }
        }
        ExtensionEventKind::Input
            if object.get("action").and_then(JsonValue::as_str) == Some("transform") =>
        {
            // TS 把 `result.text` 直接当作新文本；Rust 的 `text: String` 用 JS
            // `String()` 归一化（缺失 → `"undefined"`）。
            if !object.get("text").is_some_and(JsonValue::is_string) {
                let text = match object.get("text") {
                    Some(field) => kiana_json::js_string(Some(field)),
                    None => "undefined".to_owned(),
                };
                object.insert("text".to_owned(), JsonValue::String(text));
            }
            if let Some(images) = object.get("images")
                && !images.is_array()
                && !images.is_null()
            {
                object.shift_remove("images");
            }
        }
        _ => {}
    }
    JsonValue::Object(object)
}

fn handler_result_from_json(
    kind: ExtensionEventKind,
    value: JsonValue,
) -> Result<Option<ExtensionEventResult>, JsCallbackError> {
    if value.is_null() {
        return Ok(None);
    }
    // TS 的 handler 结果是无类型的：调用方只读它需要的字段，`{ cancel: 1 }` 这样的真值
    // 也能取消。Rust 的结果类型是强类型的，因此先把 JS 语义（truthiness / String()）
    // 归一化，避免整条结果因字段类型不符被拒绝（`DispatchError` 会中断整次派发）。
    let value = coerce_handler_result(kind, value);
    let parse = || -> Result<Option<ExtensionEventResult>, serde_json::Error> {
        Ok(Some(match kind {
            ExtensionEventKind::ResourcesDiscover => {
                ExtensionEventResult::ResourcesDiscover(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::SessionBeforeSwitch => {
                ExtensionEventResult::SessionBeforeSwitch(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::SessionBeforeFork => {
                ExtensionEventResult::SessionBeforeFork(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::SessionBeforeCompact => {
                ExtensionEventResult::SessionBeforeCompact(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::SessionBeforeTree => {
                ExtensionEventResult::SessionBeforeTree(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::Context | ExtensionEventKind::ContextWithSystem => {
                ExtensionEventResult::Context(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::CacheWarmingDecision => {
                // TS 原样返回 handler 赋给 `result.action` 的值；消费者只认 `=== "stop"`。
                // `CacheWarmingDecisionEventResult` 只能表达两个字面量，因此把任何
                // 存在且非 "stop" 的值映射为 `Warm`（可观察语义相同），只有 `action`
                // 为 `undefined`（键缺失）才表示“不覆盖”。
                // 键缺失（JS 的 `undefined`，JSON 序列化时被丢弃）→ 不覆盖；
                // `null` 是显式值 → 非 "stop" → warm。
                let action = value.get("action").map(|action| match action.as_str() {
                    Some("stop") => kiana_config::cache::warmer::CacheWarmingAction::Stop,
                    _ => kiana_config::cache::warmer::CacheWarmingAction::Warm,
                });
                ExtensionEventResult::CacheWarming(
                    kiana_config::cache::warmer::CacheWarmingDecisionEventResult { action },
                )
            }
            ExtensionEventKind::BeforeProviderRequest => {
                ExtensionEventResult::BeforeProviderRequest(value.clone())
            }
            ExtensionEventKind::BeforeAgentStart => {
                ExtensionEventResult::BeforeAgentStart(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::TurnEnd => {
                ExtensionEventResult::TurnEnd(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::AgentBeforeSettle => {
                ExtensionEventResult::AgentBeforeSettle(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::MessageEnd => {
                ExtensionEventResult::MessageEnd(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::ToolCall => {
                ExtensionEventResult::ToolCall(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::ToolResult => {
                ExtensionEventResult::ToolResult(serde_json::from_value(value.clone())?)
            }
            ExtensionEventKind::UserBash => {
                // 生产路径在 `call_user_bash_js_handler` 里用原始 JS 值校验（保留
                // `{ operations }` 的函数）。这里只是形状正确的 `{ result }` 的兜底。
                let result = value.get("result").cloned().ok_or_else(|| {
                    serde_json::Error::io(std::io::Error::other(INVALID_USER_BASH_RESULT))
                })?;
                ExtensionEventResult::UserBash(kiana_ext_api::events::UserBashEventResult::Result(
                    serde_json::from_value(result)?,
                ))
            }
            ExtensionEventKind::Input => {
                ExtensionEventResult::Input(serde_json::from_value(value.clone())?)
            }
            // 没有结果类型的事件（`agent_start` 等）忽略返回值（TS 的 `void`）。
            _ => return Ok(None),
        }))
    };
    match parse() {
        Ok(result) => Ok(result),
        Err(error) => Err(JsCallbackError {
            name: "Error".to_string(),
            message: format!("invalid handler result: {error}"),
        }),
    }
}

/// 标准事件 handler 适配器（JS 函数 → [`ExtensionHandler::Standard`]）。
fn js_standard_handler_adapter(callback: JsCallback, kind: ExtensionEventKind) -> ExtensionHandler {
    ExtensionHandler::Standard(Arc::new(move |event, ctx| {
        let callback = callback;
        Box::pin(async move {
            // 键序与 TS 的运行时事件对象一致（`Object.keys`/`JSON.stringify` 可观察）。
            let event_json = crate::runner::dispatch::ts_event_json(&event);
            // runner 每次 emit 只创建一个 ctx（TS 的 `this.createContext()`），同一个事件
            // 的所有 handler 共享它；直接使用调用方传入的 ctx，而不是每个 handler 重建。
            if kind == ExtensionEventKind::UserBash {
                return call_user_bash_js_handler(callback, event_json, Some(ctx)).await;
            }
            // TS 把**同一个**事件对象交给所有 handler：任何就地修改都对后续 handler 与
            // runner 的返回值可见。适配器在每次调用后读回整个事件对象，由 host 桥交回
            // dispatch.js 写回本地事件对象。`message_update` / `tool_execution_update` /
            // `provider_stream_event` 同样走这条路径：设计文档 §4.6 的 `Persistent` 句柄
            // 物化受 kiana-ext-api 冻结契约限制未实现，但它们与其它事件一样共享对象语义
            // （代价是每次 delta 多一次事件对象读回）。
            let (value, mutated_event, event_refs) = callback
                .call_with_event_capture(
                    event_json,
                    Some(ctx),
                    matches!(
                        kind,
                        ExtensionEventKind::Context | ExtensionEventKind::ContextWithSystem
                    ),
                )
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
            set_handler_event_mutation(mutated_event.clone());
            set_handler_event_refs(event_refs);
            let mut result = handler_result_from_json(kind, value)
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
            // `context` / `context_with_system`：handler 可以只就地修改 `event.messages`
            // 而不返回值。JS 侧还会收到读取回的事件对象；这里同时把它折进结果，保证
            // 直接调用适配器的路径（不经 dispatch.js）也成立。
            //
            // 只有**按引用**改动（splice/push/替换元素）才折进结果：TS 的
            // `sameMessages` 是引用比较，消息对象的原地内容修改不算“返回了新数组”
            // （内容修改通过共享对象语义生效，且不能触发首部 system 消息重建）。
            if matches!(
                kind,
                ExtensionEventKind::Context | ExtensionEventKind::ContextWithSystem
            ) {
                let mutated_messages = mutated_event
                    .get("messages")
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                if mutated_messages.is_array() && handler_messages_changed() == Some(true) {
                    let messages: Vec<kiana_agent_core::AgentMessage> =
                        serde_json::from_value(mutated_messages).map_err(|error| {
                            Box::new(error) as Box<dyn std::error::Error + Send + Sync>
                        })?;
                    match &mut result {
                        None => {
                            result = Some(ExtensionEventResult::Context(
                                kiana_ext_api::events::ContextEventResult {
                                    messages: Some(messages),
                                },
                            ));
                        }
                        Some(ExtensionEventResult::Context(existing))
                            if existing.messages.is_none() =>
                        {
                            existing.messages = Some(messages);
                        }
                        _ => {}
                    }
                }
            }
            Ok(result)
        })
    }))
}

/// `user_bash` 结果的【逐字】非法文案（`runner.ts:1278`）。
const INVALID_USER_BASH_RESULT: &str = "Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object";

fn user_bash_error() -> JsCallbackError {
    JsCallbackError {
        name: "Error".to_string(),
        message: INVALID_USER_BASH_RESULT.to_string(),
    }
}

fn boxed_callback_error(error: JsCallbackError) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(error) as Box<dyn std::error::Error + Send + Sync>
}

/// 调用 JS `user_bash` handler 并把返回值按 TS 的 `isUserBashEventResult` 严格校验。
///
/// 与普通事件不同，`user_bash` 的 `{ operations }` 携带 JS 函数（`exec`），不能走 JSON；
/// 用 [`JsCallback::call_handler_raw`] 保留原始 JS 值：
/// - `undefined` → `None`（本地执行）；
/// - 合法 `{ operations: { exec } }` → [`ExtensionEventResult::UserBash`] 的
///   [`kiana_ext_api::events::UserBashEventResult::Operations`]（`exec` 由 [`JsBashOperations`]
///   桥回 JS）；
/// - 合法 `{ result }` → `Result`；
/// - 其余（含 `null`、`{}`、缺 `exitCode`、`exitCode: null`、XOR 违反）→
///   【逐字】`Invalid user_bash handler result: …`（调用方负责上报并向上抛）。
async fn call_user_bash_js_handler(
    callback: JsCallback,
    event_json: JsonValue,
    context: Option<Arc<dyn ExtensionContextApi>>,
) -> Result<Option<ExtensionEventResult>, Box<dyn std::error::Error + Send + Sync>> {
    // 与其它标准事件一致：保留同一个 JS 事件对象并在调用后读回（共享对象语义）。
    let (value, mutated_event) = callback
        .call_handler_raw_with_event(event_json, context)
        .await
        .map_err(boxed_callback_error)?;
    set_handler_event_mutation(mutated_event);
    let engine = current_engine()
        .ok_or_else(engine_missing_error)
        .map_err(boxed_callback_error)?;
    engine
        .with_js(
            |ctx| -> Result<Option<ExtensionEventResult>, JsCallbackError> {
                let value = value.restore(&ctx).map_err(|error| JsCallbackError {
                    name: "Error".to_string(),
                    message: error.to_string(),
                })?;
                user_bash_result_from_js(&ctx, value)
            },
        )
        .map_err(boxed_callback_error)
}

/// TS `isUserBashEventResult` 的逐字移植（基于原始 JS 值，函数与 `undefined` 可区分）。
fn user_bash_result_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> Result<Option<ExtensionEventResult>, JsCallbackError> {
    if value.is_undefined() {
        return Ok(None);
    }
    if !value.is_object() || value.is_null() {
        return Err(user_bash_error());
    }
    let object = value.into_object().ok_or_else(user_bash_error)?;
    let operations = object
        .get::<_, Value<'js>>("operations")
        .map_err(|_| user_bash_error())?;
    let result = object
        .get::<_, Value<'js>>("result")
        .map_err(|_| user_bash_error())?;
    let has_operations = !operations.is_undefined();
    let has_result = !result.is_undefined();
    if has_operations == has_result {
        return Err(user_bash_error());
    }
    if has_operations {
        if !operations.is_object() || operations.is_null() {
            return Err(user_bash_error());
        }
        let operations = operations.into_object().ok_or_else(user_bash_error)?;
        let exec = operations
            .get::<_, Value<'js>>("exec")
            .map_err(|_| user_bash_error())?;
        let Some(exec) = exec.as_function().cloned() else {
            return Err(user_bash_error());
        };
        let callback = JsCallback::new(ctx, exec);
        return Ok(Some(ExtensionEventResult::UserBash(
            kiana_ext_api::events::UserBashEventResult::Operations(Arc::new(JsBashOperations {
                exec: callback,
            })),
        )));
    }
    if !result.is_object() || result.is_null() {
        return Err(user_bash_error());
    }
    let result_object = result.into_object().ok_or_else(user_bash_error)?;
    let output = result_object
        .get::<_, Value<'js>>("output")
        .map_err(|_| user_bash_error())?;
    let Some(output) = output.as_string().and_then(|text| text.to_string().ok()) else {
        return Err(user_bash_error());
    };
    let has_exit_code = result_object
        .contains_key("exitCode")
        .map_err(|_| user_bash_error())?;
    if !has_exit_code {
        return Err(user_bash_error());
    }
    let exit_code = result_object
        .get::<_, Value<'js>>("exitCode")
        .map_err(|_| user_bash_error())?;
    let exit_code = if exit_code.is_undefined() {
        None
    } else if let Some(number) = exit_code.as_number() {
        Some(number as i32)
    } else {
        return Err(user_bash_error());
    };
    let cancelled = result_object
        .get::<_, Value<'js>>("cancelled")
        .map_err(|_| user_bash_error())?;
    let Some(cancelled) = cancelled.as_bool() else {
        return Err(user_bash_error());
    };
    let truncated = result_object
        .get::<_, Value<'js>>("truncated")
        .map_err(|_| user_bash_error())?;
    let Some(truncated) = truncated.as_bool() else {
        return Err(user_bash_error());
    };
    let full_output_path = result_object
        .get::<_, Value<'js>>("fullOutputPath")
        .map_err(|_| user_bash_error())?;
    let full_output_path = if full_output_path.is_undefined() {
        None
    } else {
        match full_output_path
            .as_string()
            .and_then(|text| text.to_string().ok())
        {
            Some(path) => Some(path),
            None => return Err(user_bash_error()),
        }
    };
    Ok(Some(ExtensionEventResult::UserBash(
        kiana_ext_api::events::UserBashEventResult::Result(BashResult {
            output,
            exit_code,
            cancelled,
            truncated,
            full_output_path,
            execution_status: None,
        }),
    )))
}

// ---------------------------------------------------------------------------
// `user_bash` 的 `{ operations }`：JS `exec` → Rust [`BashOperations`]
// ---------------------------------------------------------------------------

/// `onData` 转发回调的登记（见 [`JsBashOperations::exec`]）。
///
/// `BashExecOptions::on_data` 是 `&dyn Fn`（只在 `exec` 的 future 存活期间有效），而 JS
/// 函数必须 `'static`。登记表把回调的胖指针拆成 `(data, vtable)` 两个 `usize`；守卫在
/// `exec` 返回（或被取消）时移除登记，因此 JS 侧在 `exec` 结束后再调用 `onData` 是安全的
/// no-op，绝不会解引用悬空指针。
struct OnDataRegistration {
    id: u64,
}

thread_local! {
    static NEXT_BASH_ON_DATA: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

impl OnDataRegistration {
    fn register_raw(data: usize, vtable: usize) -> Self {
        let id = NEXT_BASH_ON_DATA.with(|cell| {
            let id = cell.get();
            cell.set(id.wrapping_add(1));
            id
        });
        let _ = BASH_ON_DATA.try_with(|map| {
            map.borrow_mut().insert(id, (data, vtable));
        });
        Self { id }
    }
}

/// 按 id 转发一段输出；登记已被移除时是 no-op（`exec` 结束后 JS 再调用 `onData`）。
fn call_registered_on_data(id: u64, bytes: &[u8]) {
    // `try_with`：线程退出时 JS 仍可能持有 `onData` 函数，TLS 析构后调用是 no-op。
    let _ = BASH_ON_DATA.try_with(|map| {
        let Some((data, vtable)) = map.borrow().get(&id).copied() else {
            return;
        };
        // SAFETY: `register` 只在对应 `exec` 的 future 存活期间登记该回调；`Drop`
        // 在 future 结束（含取消）时移除，之后本函数不会再取到指针。
        let callback: &(dyn Fn(&[u8]) + Send + Sync) =
            unsafe { std::mem::transmute((data, vtable)) };
        callback(bytes);
    });
}

impl Drop for OnDataRegistration {
    fn drop(&mut self) {
        let _ = BASH_ON_DATA.try_with(|map| {
            map.borrow_mut().remove(&self.id);
        });
    }
}

/// TS `Buffer | Uint8Array | ArrayBuffer | string` → 字节。
fn js_data_bytes<'js>(value: &Value<'js>) -> Vec<u8> {
    if let Some(text) = value.as_string() {
        return text
            .to_string()
            .map(|text| text.into_bytes())
            .unwrap_or_default();
    }
    if let Ok(typed) = rquickjs::TypedArray::<u8>::from_value(value.clone()) {
        // SAFETY: 在返回前复制，期间不再运行 JS。
        if let Some(bytes) = unsafe { typed.as_bytes() } {
            return bytes.to_vec();
        }
    }
    if let Some(bytes) = value
        .as_object()
        .and_then(|object| object.as_array_buffer())
        .and_then(|buffer| buffer.as_raw())
    {
        // SAFETY: 同上，复制后立即返回。
        return unsafe { bytes.as_ref() }.to_vec();
    }
    Vec::new()
}

/// JS `{ operations }` 的 `exec` 桥。`exec(command, cwd, { onData, signal, timeout, env })`。
struct JsBashOperations {
    exec: JsCallback,
}

impl std::fmt::Debug for JsBashOperations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JsBashOperations(..)")
    }
}

impl BashOperations for JsBashOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>> {
        let callback = self.exec;
        let command = command.to_string();
        let cwd = cwd.to_string();
        let timeout = options.timeout;
        let env = options.env.clone();
        let signal = options.signal.clone();
        // `on_data` 是只在本次 `exec` 期间有效的借用；把胖指针拆开搬进 `spawn_local`
        // （局部任务与外层 future 在同一线程，见 `ARCHITECTURE.md` §4.2）。
        // SAFETY: 外层 future 存活期间登记有效；局部任务在 `exec` settle 前持守卫。
        let (data, vtable): (usize, usize) = unsafe { std::mem::transmute(options.on_data) };
        // `BoxFuture` 要求 `Send`，而 JS 调用持有 `Rc<JsEngine>`；与 `call_async_send`
        // 相同，把工作投给同线程 `LocalSet`，外层只等 `oneshot`。
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            tokio::task::spawn_local(async move {
                let registration = OnDataRegistration::register_raw(data, vtable);
                let on_data_id = registration.id;
                let args = vec![
                    JsArg::Json(JsonValue::String(command)),
                    JsArg::Json(JsonValue::String(cwd)),
                    JsArg::Build(Arc::new(move |ctx| {
                        let object = Object::new(ctx.clone())?;
                        let on_data = Function::new(
                            ctx.clone(),
                            move |_ctx: Ctx<'_>, value: Value<'_>| -> rquickjs::Result<()> {
                                let bytes = js_data_bytes(&value);
                                call_registered_on_data(on_data_id, &bytes);
                                Ok(())
                            },
                        )?;
                        object.set("onData", on_data)?;
                        if let Some(timeout) = timeout {
                            object.set("timeout", timeout)?;
                        }
                        if let Some(env) = &env {
                            let value = serde_json::to_value(env).unwrap_or(JsonValue::Null);
                            object.set("env", json_value(ctx, &value)?)?;
                        }
                        if let Some(signal) = &signal {
                            object.set("signal", abort_signal_value(ctx, signal)?)?;
                        }
                        Ok(object.into_value())
                    })),
                ];
                let result = callback.call_async(args).await;
                let _ = sender.send(result);
                drop(registration);
            });
            let result = receiver.await.map_err(|_| {
                Box::new(engine_missing_error()) as Box<dyn std::error::Error + Send + Sync>
            })??;
            let exit_code = result
                .get("exitCode")
                .and_then(JsonValue::as_i64)
                .map(|code| code as i32);
            Ok(BashExecResult {
                exit_code,
                execution_status: None,
            })
        })
    }
}

/// 从 JS `AbortSignal` 造一个 [`CancellationToken`]（已中止时立即取消）。
fn cancellation_token_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Option<&Value<'js>>,
) -> EngineResult<Option<tokio_util::sync::CancellationToken>> {
    let Some(value) = value else { return Ok(None) };
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let token = tokio_util::sync::CancellationToken::new();
    let aborted: bool = object.get::<_, Option<bool>>("aborted")?.unwrap_or(false);
    if aborted {
        token.cancel();
        return Ok(Some(token));
    }
    if let Ok(add) = object.get::<_, Function<'js>>("addEventListener") {
        let listener_token = token.clone();
        let listener = Function::new(ctx.clone(), move || listener_token.cancel())?;
        // `EventTarget.addEventListener` 需要 `this` 是 signal 对象。
        add.call::<_, ()>((rquickjs::function::This(object.clone()), "abort", listener))?;
    }
    Ok(Some(token))
}

/// 事件总线订阅表的值：已跟踪的退订闭包 + 可选的 JS 回调（退订时释放持久槽）。
type JsSubscription = (Box<dyn FnOnce()>, Option<JsCallback>);

/// `project_trust` 的 `ctx.ui` 处理器表（`js_project_trust_handler_adapter` 的 JS 函数在
/// 调用时按 id 找回 Rust 回调）。
#[allow(clippy::type_complexity)]
enum ProjectTrustUiHandler {
    Select(
        Arc<
            dyn Fn(
                    String,
                    Vec<String>,
                    Option<kiana_ext_api::ui::ExtensionUIDialogOptions>,
                ) -> LocalBoxFuture<'static, Option<String>>
                + Send
                + Sync,
        >,
    ),
    Confirm(
        Arc<
            dyn Fn(
                    String,
                    String,
                    Option<kiana_ext_api::ui::ExtensionUIDialogOptions>,
                ) -> LocalBoxFuture<'static, bool>
                + Send
                + Sync,
        >,
    ),
    Input(
        Arc<
            dyn Fn(
                    String,
                    Option<String>,
                    Option<kiana_ext_api::ui::ExtensionUIDialogOptions>,
                ) -> LocalBoxFuture<'static, Option<String>>
                + Send
                + Sync,
        >,
    ),
}

thread_local! {
    static PROJECT_TRUST_UI_HANDLERS: RefCell<HashMap<u64, ProjectTrustUiHandler>> =
        RefCell::new(HashMap::new());
}

fn store_project_trust_ui_handler(handler: ProjectTrustUiHandler) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    PROJECT_TRUST_UI_HANDLERS.with(|handlers| {
        handlers.borrow_mut().insert(id, handler);
    });
    id
}

fn dialog_options_from_json<'js>(
    ctx: &Ctx<'js>,
    options: Option<&Value<'js>>,
) -> EngineResult<Option<kiana_ext_api::ui::ExtensionUIDialogOptions>> {
    let Some(options) = options.filter(|value| value.is_object()) else {
        return Ok(None);
    };
    let object = options.as_object().expect("checked object");
    let timeout = object
        .get::<_, Option<f64>>("timeout")?
        .filter(|timeout| timeout.is_finite());
    let signal = object.get::<_, Option<Value<'js>>>("signal")?;
    let signal = cancellation_token_from_js(ctx, signal.as_ref())?;
    Ok(Some(kiana_ext_api::ui::ExtensionUIDialogOptions {
        signal,
        timeout,
    }))
}

fn notify_kind_from_value(value: Option<&Value<'_>>) -> Option<kiana_ext_api::ui::NotifyKind> {
    let value = value?;
    let text = value.as_string()?.to_string().ok()?;
    Some(match text.as_str() {
        "warning" => kiana_ext_api::ui::NotifyKind::Warning,
        "error" => kiana_ext_api::ui::NotifyKind::Error,
        _ => kiana_ext_api::ui::NotifyKind::Info,
    })
}

/// 建 `project_trust` handler 的 `ctx.ui` JS 对象。
fn project_trust_ui_value<'js>(
    ctx: &Ctx<'js>,
    ui: &kiana_ext_api::events::ProjectTrustUi,
) -> EngineResult<Object<'js>> {
    let object = Object::new(ctx.clone())?;

    let select = Arc::clone(&ui.select);
    object.set(
        "select",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  title: String,
                  options: Value<'js>,
                  opts: rquickjs::function::Opt<Value<'js>>|
                  -> EngineResult<Value<'js>> {
                let options: Vec<String> = options_to_strings(&ctx, options)?;
                let dialog = dialog_options_from_json(&ctx, opts.0.as_ref())?;
                let id = store_project_trust_ui_handler(ProjectTrustUiHandler::Select(
                    Arc::clone(&select),
                ));
                call_ui_promise(
                    &ctx,
                    "select",
                    json!({ "handler": id, "title": title, "options": options, "dialog": dialog_to_json(dialog) }),
                )
            },
        )?,
    )?;

    let confirm = Arc::clone(&ui.confirm);
    object.set(
        "confirm",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  title: String,
                  message: String,
                  opts: rquickjs::function::Opt<Value<'js>>|
                  -> EngineResult<Value<'js>> {
                let dialog = dialog_options_from_json(&ctx, opts.0.as_ref())?;
                let id = store_project_trust_ui_handler(ProjectTrustUiHandler::Confirm(
                    Arc::clone(&confirm),
                ));
                call_ui_promise(
                    &ctx,
                    "confirm",
                    json!({ "handler": id, "title": title, "message": message, "dialog": dialog_to_json(dialog) }),
                )
            },
        )?,
    )?;

    let input = Arc::clone(&ui.input);
    object.set(
        "input",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  title: String,
                  placeholder: rquickjs::function::Opt<Value<'js>>,
                  opts: rquickjs::function::Opt<Value<'js>>|
                  -> EngineResult<Value<'js>> {
                let placeholder = placeholder
                    .0
                    .as_ref()
                    .and_then(|value| value.as_string())
                    .and_then(|text| text.to_string().ok());
                let dialog = dialog_options_from_json(&ctx, opts.0.as_ref())?;
                let id = store_project_trust_ui_handler(ProjectTrustUiHandler::Input(
                    Arc::clone(&input),
                ));
                call_ui_promise(
                    &ctx,
                    "input",
                    json!({ "handler": id, "title": title, "placeholder": placeholder, "dialog": dialog_to_json(dialog) }),
                )
            },
        )?,
    )?;

    let notify = Arc::clone(&ui.notify);
    object.set(
        "notify",
        Function::new(
            ctx.clone(),
            move |message: String, kind: rquickjs::function::Opt<Value<'_>>| {
                let kind = notify_kind_from_value(kind.0.as_ref());
                notify(message, kind);
            },
        )?,
    )?;

    Ok(object)
}

fn options_to_strings<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> EngineResult<Vec<String>> {
    let json = value_to_json(ctx, value)?;
    match json {
        JsonValue::Array(values) => Ok(values
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()),
        _ => Ok(Vec::new()),
    }
}

fn dialog_to_json(options: Option<kiana_ext_api::ui::ExtensionUIDialogOptions>) -> JsonValue {
    match options {
        Some(options) => {
            let mut object = serde_json::Map::new();
            if let Some(timeout) = options.timeout {
                object.insert("timeout".to_string(), json!(timeout));
            }
            let signal = store_dialog_token(options.signal);
            if signal != 0 {
                object.insert("signal".to_string(), json!(signal));
            }
            JsonValue::Object(object)
        }
        None => JsonValue::Null,
    }
}

/// `__kiana.promise(kind, payload)`。
fn call_kiana_promise<'js>(
    ctx: &Ctx<'js>,
    kind: &str,
    payload: JsonValue,
) -> EngineResult<Value<'js>> {
    let kiana: Object<'js> = ctx.globals().get("__kiana")?;
    let promise: Function<'js> = kiana.get("promise")?;
    let payload = json_value(ctx, &payload)?;
    promise.call((rquickjs::function::This(kiana), kind, payload))
}

/// `ctx.ui.select/confirm/input` → `__kiana.promise("loader.projectTrustUi", payload)`。
fn call_ui_promise<'js>(
    ctx: &Ctx<'js>,
    op: &str,
    mut payload: JsonValue,
) -> EngineResult<Value<'js>> {
    if let JsonValue::Object(object) = &mut payload {
        object.insert("op".to_string(), JsonValue::String(op.to_string()));
    }
    call_kiana_promise(ctx, "loader.projectTrustUi", payload)
}

thread_local! {
    /// 进行中的 `kiana.exec` 取消令牌（id → token；0 表示没有 signal）。
    static EXEC_TOKENS: RefCell<HashMap<u64, tokio_util::sync::CancellationToken>> =
        RefCell::new(HashMap::new());
}

fn store_exec_token(token: Option<tokio_util::sync::CancellationToken>) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let Some(token) = token else { return 0 };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    EXEC_TOKENS.with(|tokens| {
        tokens.borrow_mut().insert(id, token);
    });
    id
}

fn take_exec_token(id: u64) -> Option<tokio_util::sync::CancellationToken> {
    if id == 0 {
        return None;
    }
    EXEC_TOKENS.with(|tokens| tokens.borrow_mut().remove(&id))
}

thread_local! {
    /// 进行中的项目信任对话框取消令牌（`ctx.ui.select/confirm/input` 的 `options.signal`）。
    static DIALOG_TOKENS: RefCell<HashMap<u64, tokio_util::sync::CancellationToken>> =
        RefCell::new(HashMap::new());
}

fn store_dialog_token(token: Option<tokio_util::sync::CancellationToken>) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let Some(token) = token else { return 0 };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    DIALOG_TOKENS.with(|tokens| {
        tokens.borrow_mut().insert(id, token);
    });
    id
}

fn take_dialog_token(id: u64) -> Option<tokio_util::sync::CancellationToken> {
    if id == 0 {
        return None;
    }
    DIALOG_TOKENS.with(|tokens| tokens.borrow_mut().remove(&id))
}

/// `loader.exec` 的 host kind 处理器：在线程池执行同步的 `exec_command`，不阻塞 JS。
///
/// host kind 处理器只负责「受理」：解析 payload 成功后立刻返回 `Ok`，把实际执行放进
/// `spawn_local` + `spawn_blocking`；完成时用 `call.completion_sender()` 的
/// `resolve` / `reject_message` 按 `call.id()` settle 对应的 JS Promise。
fn handle_exec(call: HostCall<'_>) -> EngineResult<()> {
    #[derive(serde::Deserialize)]
    struct ExecPayload {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: String,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        token: u64,
    }
    let payload: ExecPayload = call.payload_json()?;
    let id = call.id();
    let sender = call.completion_sender();
    let signal = take_exec_token(payload.token);
    let options = CoreExecOptions {
        signal,
        timeout: payload.timeout,
        cwd: if payload.cwd.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(payload.cwd))
        },
    };
    let cwd = options
        .cwd
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let command = payload.command;
    let args = payload.args;
    tokio::task::spawn_local(async move {
        let result = tokio::task::spawn_blocking(move || {
            exec_command(&command, &args, &cwd, Some(&options))
        })
        .await;
        match result {
            Ok(result) => {
                sender.resolve(
                    id,
                    json!({
                        "stdout": result.stdout,
                        "stderr": result.stderr,
                        "code": result.code,
                        "killed": result.killed,
                    }),
                );
            }
            Err(error) => {
                sender.reject_message(id, format!("exec failed: {error}"));
            }
        }
    });
    Ok(())
}

/// `loader.projectTrustUi` 的 host kind 处理器。
///
/// handler 按 payload 里的 `handler` id 从线程本地表**取走**（一次性）：同一 id 第二次调用
/// 会以 `"project_trust ui handler is no longer available"` 拒绝。`select` / `input` 的
/// `None` 结果映射为 JS `undefined`，`confirm` 映射为布尔。
fn handle_project_trust_ui(call: HostCall<'_>) -> EngineResult<()> {
    let payload: JsonValue = call.payload_json()?;
    let id = call.id();
    let sender = call.completion_sender();
    let Some(handler_id) = payload.get("handler").and_then(JsonValue::as_u64) else {
        sender.reject_message(id, "invalid project_trust ui call");
        return Ok(());
    };
    let Some(handler) =
        PROJECT_TRUST_UI_HANDLERS.with(|handlers| handlers.borrow_mut().remove(&handler_id))
    else {
        sender.reject_message(id, "project_trust ui handler is no longer available");
        return Ok(());
    };
    let dialog = payload.get("dialog").cloned().unwrap_or(JsonValue::Null);
    let dialog = if dialog.is_null() {
        None
    } else {
        Some(kiana_ext_api::ui::ExtensionUIDialogOptions {
            signal: take_dialog_token(
                dialog
                    .get("signal")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(0),
            ),
            timeout: dialog.get("timeout").and_then(JsonValue::as_f64),
        })
    };
    match handler {
        ProjectTrustUiHandler::Select(handler) => {
            let title = payload
                .get("title")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            let options = payload
                .get("options")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            tokio::task::spawn_local(async move {
                let result = handler(title, options, dialog).await;
                match result {
                    Some(value) => {
                        sender.resolve(id, HostValue::String(value));
                    }
                    None => {
                        sender.resolve(id, HostValue::Undefined);
                    }
                }
            });
        }
        ProjectTrustUiHandler::Confirm(handler) => {
            let title = payload
                .get("title")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            let message = payload
                .get("message")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            tokio::task::spawn_local(async move {
                let result = handler(title, message, dialog).await;
                sender.resolve(id, HostValue::Bool(result));
            });
        }
        ProjectTrustUiHandler::Input(handler) => {
            let title = payload
                .get("title")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            let placeholder = payload
                .get("placeholder")
                .and_then(JsonValue::as_str)
                .map(str::to_string);
            tokio::task::spawn_local(async move {
                let result = handler(title, placeholder, dialog).await;
                match result {
                    Some(value) => {
                        sender.resolve(id, HostValue::String(value));
                    }
                    None => {
                        sender.resolve(id, HostValue::Undefined);
                    }
                }
            });
        }
    }
    Ok(())
}

fn js_project_trust_handler_adapter(
    callback: JsCallback,
    _shared: Arc<RuntimeShared>,
) -> ExtensionHandler {
    ExtensionHandler::ProjectTrust(Arc::new(
        move |event: ProjectTrustEvent, ctx: ProjectTrustContext| {
            Box::pin(async move {
                let event_json = serde_json::to_value(&event).unwrap_or(JsonValue::Null);
                let ui = ctx.ui.clone();
                let cwd = ctx.cwd.clone();
                let mode = match ctx.mode {
                    kiana_ext_api::types::ExtensionMode::Tui => "tui",
                    kiana_ext_api::types::ExtensionMode::Rpc => "rpc",
                    kiana_ext_api::types::ExtensionMode::Json => "json",
                    kiana_ext_api::types::ExtensionMode::Print => "print",
                };
                let has_ui = ctx.has_ui;
                // `ctx.ui` 需要真实函数对象，不能 JSON 化：在目标事务里现造整个 context。
                let context_arg: JsArgBuilder = Arc::new(move |js_ctx: &Ctx<'_>| {
                    let object = Object::new(js_ctx.clone())?;
                    object.set("cwd", cwd.clone())?;
                    object.set("mode", mode)?;
                    object.set("hasUI", has_ui)?;
                    object.set("ui", project_trust_ui_value(js_ctx, &ui)?)?;
                    Ok(object.into_value())
                });
                let args = vec![JsArg::Json(event_json), JsArg::Build(context_arg)];
                // `ProjectTrustHandler`（kiana-ext-api 冻结契约）没有错误返回通道。TS 的
                // handler 抛错 / 返回 null 会被 `emitProjectTrustEvent` 收进 `errors` 并
                // 继续；这里把失败文本放进线程本地槽，`runner/dispatch.rs` 的 host 桥在
                // 同一个 handler 调用返回后取走并拒绝该 Promise（dispatch.js 捕获后按
                // `extensionPath`/`event` 上报，语义与 TS 相同）。
                let raw = callback.call_handler_raw(args).await;
                let Some(engine) = current_engine() else {
                    set_project_trust_handler_failure(engine_missing_error().message);
                    return ProjectTrustEventResult::default();
                };
                match raw {
                    Err(error) => {
                        set_project_trust_handler_failure(error.message);
                        ProjectTrustEventResult::default()
                    }
                    Ok(value) => engine
                        .with_js(|ctx| -> Result<ProjectTrustEventResult, String> {
                            let value = value.restore(&ctx).map_err(|error| error.to_string())?;
                            project_trust_result_from_js(value)
                        })
                        .unwrap_or_else(|message| {
                            set_project_trust_handler_failure(message);
                            ProjectTrustEventResult::default()
                        }),
                }
            })
        },
    ))
}

/// TS `emitProjectTrustEvent` 的返回读取语义：`handlerResult.trusted`。
///
/// - `undefined` / `null` → TS 读取 `.trusted` 抛 TypeError（由调用方收集为错误，继续）；
/// - 对象 / 其它值 → 只有字面量 `"undecided"` 继续下一个 handler；其余（包括缺失
///   `trusted` 与非法值）都作为决定返回，调用方按 `trusted === "yes"` 解释——Rust 的
///   类型化 `ProjectTrustEventDecision` 用 [`ProjectTrustEventDecision::No`] 表达
///   “非 yes”的同一可观察结果。
fn project_trust_result_from_js<'js>(value: Value<'js>) -> Result<ProjectTrustEventResult, String> {
    if value.is_undefined() {
        return Err("Cannot read properties of undefined (reading 'trusted')".to_string());
    }
    if value.is_null() {
        return Err("Cannot read properties of null (reading 'trusted')".to_string());
    }
    if !value.is_object() || value.is_function() {
        // 原始值读取 `.trusted` 是 `undefined`（不抛错）→ 作为“非 yes”的决定。
        return Ok(ProjectTrustEventResult {
            trusted: kiana_ext_api::events::ProjectTrustEventDecision::No,
            remember: None,
        });
    }
    let object = value
        .into_object()
        .ok_or_else(|| "Cannot read properties of null (reading 'trusted')".to_string())?;
    let trusted = object
        .get::<_, Value<'js>>("trusted")
        .map_err(|error| error.to_string())?;
    let trusted = match trusted
        .as_string()
        .and_then(|text| text.to_string().ok())
        .as_deref()
    {
        Some("undecided") => kiana_ext_api::events::ProjectTrustEventDecision::Undecided,
        Some("yes") => kiana_ext_api::events::ProjectTrustEventDecision::Yes,
        Some("no") => kiana_ext_api::events::ProjectTrustEventDecision::No,
        // TS: `handlerResult.trusted === "undecided"` 为假即作为决定返回；调用方只认
        // `"yes"`，因此缺失/非法值等价于 deny。
        _ => kiana_ext_api::events::ProjectTrustEventDecision::No,
    };
    let remember = object
        .get::<_, Value<'js>>("remember")
        .ok()
        .and_then(|value| value.as_bool());
    Ok(ProjectTrustEventResult { trusted, remember })
}

/// JS 命令 handler 适配器。
struct JsCommandHandler {
    callback: JsCallback,
    extension_path: String,
}

impl JsCommandHandler {
    fn into_handler(self) -> CommandHandler {
        Arc::new(
            move |args: String, ctx: Arc<dyn ExtensionCommandContextApi>| {
                let callback = self.callback;
                let extension_path = self.extension_path.clone();
                Box::pin(async move {
                    let result = callback
                        .call_async(vec![
                            JsArg::Json(JsonValue::String(args)),
                            JsArg::CommandContext(ctx),
                        ])
                        .await;
                    if let Err(error) = result {
                        // `CommandHandler` 的签名没有错误通道（冻结契约）；经 runner 安装的
                        // 上报器把错误变成 TS 的用户可见 `Extension "<path>" error: <message>`。
                        report_extension_error(ExtensionErrorReport {
                            extension_path,
                            event: "command".to_string(),
                            error: error.message,
                            stack: None,
                        });
                    }
                }) as LocalBoxFuture<'static, ()>
            },
        )
    }
}

/// JS 快捷键 handler 适配器。
struct JsShortcutHandler {
    callback: JsCallback,
    extension_path: String,
}

impl JsShortcutHandler {
    fn into_handler(self) -> ShortcutHandler {
        Arc::new(move |ctx: Arc<dyn ExtensionContextApi>| {
            let callback = self.callback;
            let extension_path = self.extension_path.clone();
            Box::pin(async move {
                // TS 的 `ExtensionShortcut.handler` 收 `ExtensionContext`（基础视图）。
                if let Err(error) = callback.call_async(vec![JsArg::Context(ctx)]).await {
                    report_extension_error(ExtensionErrorReport {
                        extension_path,
                        event: "shortcut".to_string(),
                        error: error.message,
                        stack: None,
                    });
                }
            }) as LocalBoxFuture<'static, ()>
        })
    }
}

/// 渲染器调用 `f(value, options, theme)`，第三参是真实主题对象。
fn call_renderer_with_theme(
    callback: &JsCallback,
    value_json: JsonValue,
    options_json: JsonValue,
    theme: &kiana_theme::theme::Theme,
) -> Option<Box<dyn kiana_tui::tui::Component>> {
    callback
        .with_call(|ctx, function| {
            let mut args = rquickjs::function::Args::new_unsized(ctx.clone());
            args.push_arg(json_value(ctx, &value_json).map_err(|_| js_error_from_ctx(ctx))?)
                .map_err(|_| js_error_from_ctx(ctx))?;
            args.push_arg(json_value(ctx, &options_json).map_err(|_| js_error_from_ctx(ctx))?)
                .map_err(|_| js_error_from_ctx(ctx))?;
            args.push_arg(theme_value(ctx, theme).map_err(|_| js_error_from_ctx(ctx))?)
                .map_err(|_| js_error_from_ctx(ctx))?;
            Ok(render_component_with(ctx, &function, args))
        })
        .ok()
        .flatten()
}

/// JS 消息渲染器适配器。
fn js_message_renderer<'js>(ctx: &Ctx<'js>, function: Function<'js>) -> MessageRenderer {
    let callback = JsCallback::new(ctx, function);
    Arc::new(
        move |message: &kiana_session_core::messages::CustomMessage,
              options: &kiana_ext_api::types::MessageRenderOptions,
              theme: &kiana_theme::theme::Theme| {
            let message_json = serde_json::to_value(message).unwrap_or(JsonValue::Null);
            let options_json = serde_json::to_value(options).unwrap_or(JsonValue::Null);
            call_renderer_with_theme(&callback, message_json, options_json, theme)
        },
    )
}

/// JS 条目渲染器适配器。
fn js_entry_renderer<'js>(ctx: &Ctx<'js>, function: Function<'js>) -> EntryRenderer {
    let callback = JsCallback::new(ctx, function);
    Arc::new(
        move |entry: &kiana_session_core::session_manager::CustomEntry,
              options: &kiana_ext_api::types::EntryRenderOptions,
              theme: &kiana_theme::theme::Theme| {
            let entry_json = serde_json::to_value(entry).unwrap_or(JsonValue::Null);
            let options_json = serde_json::to_value(options).unwrap_or(JsonValue::Null);
            call_renderer_with_theme(&callback, entry_json, options_json, theme)
        },
    )
}

/// JS markdown 变换器适配器。
fn js_markdown_transformer<'js>(ctx: &Ctx<'js>, function: Function<'js>) -> MarkdownTransformer {
    let callback = JsCallback::new(ctx, function);
    Arc::new(
        move |markdown: &str, context: &kiana_ext_api::types::MarkdownTransformContext| {
            let context_json = serde_json::to_value(context).unwrap_or(JsonValue::Null);
            match callback.call(vec![
                JsArg::Json(JsonValue::String(markdown.to_string())),
                JsArg::Json(context_json),
            ]) {
                Ok(JsCallResult::Value(JsonValue::String(text))) => text,
                _ => markdown.to_string(),
            }
        },
    )
}

/// JS 工具渲染器 resolver 适配器。
/// resolver 的 `next()` JS 函数（自有续延：JS 可以把函数保存到解析调用返回之后）。
fn next_renderers_function<'js>(
    ctx: &Ctx<'js>,
    next: crate::runner::renderers::NextContinuation,
) -> EngineResult<Function<'js>> {
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            // 该函数可能在 JS 事务里被调用（保存下来的 `next()`）：登记当前 `Ctx`，让解析器
            // 桥的 JS 回调同步重入，而不是再借一次 runtime。
            let _guard = SyncCtxGuard::enter(&ctx);
            match next() {
                Some(renderers) => tool_renderers_value(&ctx, &renderers),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )
}

/// 把「resolver 调用帧仍存活」标志在 drop（含 panic 展开）时置为 false。
struct AliveFlag(Rc<Cell<bool>>);

impl Drop for AliveFlag {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// 直接调用 resolver（不经 [`crate::runner::renderers::resolve_tool_renderers`]）时的
/// `next()` 兜底：借用只在本次 resolver 调用内有效，调用结束后再调用 `next()` 抛出
/// 可捕获的 JS 错误（而不是解引用已失效的栈帧）。
fn borrowed_next_renderers_function<'js>(
    ctx: &Ctx<'js>,
    next: &kiana_ext_api::tool_types::NextToolRenderersFn,
    alive: Rc<Cell<bool>>,
) -> EngineResult<Function<'js>> {
    let pointer = next as *const kiana_ext_api::tool_types::NextToolRenderersFn;
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
            if !alive.get() {
                return Err(Exception::throw_message(
                    &ctx,
                    "next() called after the tool renderer resolver returned",
                ));
            }
            // SAFETY: `alive` 只在 resolver 调用返回前置为 false，且 JS 引擎是单线程的；检查与
            // 调用之间不存在跨线程失效。调用发生在借用仍有效的窗口内。
            let next = unsafe { &*pointer };
            let _guard = SyncCtxGuard::enter(&ctx);
            match next() {
                Some(renderers) => tool_renderers_value(&ctx, &renderers),
                None => Ok(Value::new_undefined(ctx.clone())),
            }
        },
    )
}

fn js_tool_renderer_resolver<'js>(ctx: &Ctx<'js>, function: Function<'js>) -> ToolRendererResolver {
    let callback = JsCallback::new(ctx, function);
    Arc::new(
        move |tool_name: &str, next: &kiana_ext_api::tool_types::NextToolRenderersFn| {
            let tool_name = JsonValue::String(tool_name.to_string());
            // `resolve_tool_renderers` 在调用 resolver 前把自有续延压栈；取到它就能让 JS
            // 函数在调用返回后仍然安全可调用（TS 的闭包语义）。
            let continuation = crate::runner::renderers::continuation_for(next);
            let alive = Rc::new(Cell::new(true));
            // resolver 借用的 `next` 随本函数返回失效（panic 也不例外）。
            let _alive_guard = AliveFlag(Rc::clone(&alive));
            let result = callback.with_call(|ctx, function| {
                let mut args = rquickjs::function::Args::new_unsized(ctx.clone());
                args.push_arg(json_value(ctx, &tool_name).map_err(|_| js_error_from_ctx(ctx))?)
                    .map_err(|_| js_error_from_ctx(ctx))?;
                let next_fn = match &continuation {
                    Some(continuation) => next_renderers_function(ctx, Rc::clone(continuation))?,
                    None => borrowed_next_renderers_function(ctx, next, Rc::clone(&alive))?,
                };
                args.push_arg(next_fn).map_err(|_| js_error_from_ctx(ctx))?;
                let value = function
                    .call_arg::<Value<'_>>(args)
                    .map_err(|_| js_error_from_ctx(ctx))?;
                if value.is_null() || value.is_undefined() {
                    return Ok(None);
                }
                let object = value
                    .as_object()
                    .cloned()
                    .ok_or_else(|| js_error_from_ctx(ctx))?;
                let render_shell = object
                    .get::<_, Option<String>>("renderShell")
                    .ok()
                    .flatten()
                    .and_then(|shell| serde_json::from_value(JsonValue::String(shell)).ok());
                let render_call = object
                    .get::<_, Option<Function<'_>>>("renderCall")
                    .ok()
                    .flatten()
                    .map(|function| js_render_call(ctx, function));
                let render_result = object
                    .get::<_, Option<Function<'_>>>("renderResult")
                    .ok()
                    .flatten()
                    .map(|function| js_render_result(ctx, function));
                Ok(Some(kiana_ext_api::tool_types::ToolRenderers {
                    render_shell,
                    render_call,
                    render_result,
                }))
            });
            result.ok().flatten()
        },
    )
}

/// `project_trust` 的事件名（`ExtensionEventKind::ProjectTrust.as_str()`）。
const PROJECT_TRUST_EVENT: &str = "project_trust";

// ===========================================================================
// `js/api.js` 的宿主对象（每个扩展一个）
// ===========================================================================

fn throw_message<T>(ctx: &Ctx<'_>, message: impl Into<String>) -> EngineResult<T> {
    Err(Exception::throw_message(ctx, &message.into()))
}

// ---------------------------------------------------------------------------
// Provider 回调桥（streamSimple / images / classifiers / refreshModels / oauth）
// ---------------------------------------------------------------------------

/// `SimpleStreamOptions` → JS 对象。`StreamOptions` 整体不实现 `Serialize`（含回调/令牌），
/// 因此逐字段序列化，并把 `extra`（provider 专属顶层选项）展开到对象上。
fn stream_options_json(base: &kiana_ai::types::StreamOptions) -> JsonValue {
    let mut object = serde_json::Map::new();
    let mut put = |key: &str, value: JsonValue| {
        if !value.is_null() {
            object.insert(key.to_string(), value);
        }
    };
    put(
        "apiKey",
        serde_json::to_value(&base.api_key).unwrap_or(JsonValue::Null),
    );
    put(
        "env",
        serde_json::to_value(&base.env).unwrap_or(JsonValue::Null),
    );
    put(
        "headers",
        serde_json::to_value(&base.headers).unwrap_or(JsonValue::Null),
    );
    put(
        "identity",
        serde_json::to_value(&base.identity).unwrap_or(JsonValue::Null),
    );
    put(
        "timeoutMs",
        serde_json::to_value(base.timeout_ms).unwrap_or(JsonValue::Null),
    );
    put(
        "maxRetries",
        serde_json::to_value(base.max_retries).unwrap_or(JsonValue::Null),
    );
    put(
        "maxRetryDelayMs",
        serde_json::to_value(base.max_retry_delay_ms).unwrap_or(JsonValue::Null),
    );
    put(
        "temperature",
        serde_json::to_value(base.temperature).unwrap_or(JsonValue::Null),
    );
    put(
        "samplingParams",
        serde_json::to_value(&base.sampling_params).unwrap_or(JsonValue::Null),
    );
    put(
        "maxTokens",
        serde_json::to_value(base.max_tokens).unwrap_or(JsonValue::Null),
    );
    put(
        "transport",
        serde_json::to_value(base.transport).unwrap_or(JsonValue::Null),
    );
    put(
        "cacheRetention",
        serde_json::to_value(base.cache_retention).unwrap_or(JsonValue::Null),
    );
    put(
        "sessionId",
        serde_json::to_value(&base.session_id).unwrap_or(JsonValue::Null),
    );
    put(
        "websocketConnectTimeoutMs",
        serde_json::to_value(base.websocket_connect_timeout_ms).unwrap_or(JsonValue::Null),
    );
    put(
        "metadata",
        serde_json::to_value(&base.metadata).unwrap_or(JsonValue::Null),
    );
    // `extra` 的键是 TS 顶层字段（provider 专属选项）。
    for (key, value) in &base.extra {
        object.insert(key.clone(), value.clone());
    }
    JsonValue::Object(object)
}

fn simple_stream_options_json(options: &kiana_ai::types::SimpleStreamOptions) -> JsonValue {
    let mut object = match stream_options_json(&options.base) {
        JsonValue::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    for (key, value) in [
        ("toolChoice", serde_json::to_value(options.tool_choice)),
        ("reasoning", serde_json::to_value(options.reasoning)),
        ("deferred", serde_json::to_value(options.deferred)),
        (
            "thinkingBudgets",
            serde_json::to_value(&options.thinking_budgets),
        ),
    ] {
        if let Ok(value) = value
            && !value.is_null()
        {
            object.insert(key.to_string(), value);
        }
    }
    JsonValue::Object(object)
}

/// 把 Rust 侧的 provider 回调注入扩展 provider 的 options 对象。
///
/// TS 的 `StreamOptions` 契约要求扩展 provider 能调用
/// `options.onPayload(payload, model)` / `options.onResponse(response, model)` /
/// `options.onProviderStreamEvent(data, model)`；只序列化数据字段会让这些钩子静默失效
/// （provider 里 `options.onPayload?.(...)` 得到 `undefined`）。
fn add_provider_callbacks<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    on_payload: Option<kiana_ai::types::OnPayloadFn>,
    on_response: Option<kiana_ai::types::OnResponseFn>,
    on_stream_event: Option<kiana_ai::types::OnProviderStreamEventFn>,
) -> EngineResult<()> {
    if let Some(on_payload) = on_payload {
        object.set(
            "onPayload",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      payload: Value<'js>,
                      model: Value<'js>|
                      -> EngineResult<Value<'js>> {
                    let payload = value_to_json(&ctx, payload)?;
                    let Ok(model) = serde_json::from_value::<kiana_ai::types::Model>(
                        value_to_json(&ctx, model)?,
                    ) else {
                        return Ok(Value::new_undefined(ctx.clone()));
                    };
                    match on_payload(payload, &model) {
                        Some(next) => json_value(&ctx, &next),
                        None => Ok(Value::new_undefined(ctx.clone())),
                    }
                },
            )?,
        )?;
    }
    if let Some(on_response) = on_response {
        object.set(
            "onResponse",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      response: Value<'js>,
                      model: Value<'js>|
                      -> EngineResult<Value<'js>> {
                    let Ok(response) = serde_json::from_value::<kiana_ai::types::ProviderResponse>(
                        value_to_json(&ctx, response)?,
                    ) else {
                        return Ok(Value::new_undefined(ctx.clone()));
                    };
                    let Ok(model) = serde_json::from_value::<kiana_ai::types::Model>(
                        value_to_json(&ctx, model)?,
                    ) else {
                        return Ok(Value::new_undefined(ctx.clone()));
                    };
                    on_response(response, &model);
                    Ok(Value::new_undefined(ctx.clone()))
                },
            )?,
        )?;
    }
    if let Some(on_stream_event) = on_stream_event {
        object.set(
            "onProviderStreamEvent",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      data: Value<'js>,
                      model: Value<'js>|
                      -> EngineResult<Value<'js>> {
                    let data = value_to_json(&ctx, data)?;
                    let Ok(model) = serde_json::from_value::<kiana_ai::types::Model>(
                        value_to_json(&ctx, model)?,
                    ) else {
                        return Ok(Value::new_undefined(ctx.clone()));
                    };
                    on_stream_event(data, &model);
                    Ok(Value::new_undefined(ctx.clone()))
                },
            )?,
        )?;
    }
    Ok(())
}

/// `StreamOptions` → JS 实参（JSON + 真实 `AbortSignal`，后者无法 JSON 化）。
fn stream_options_arg(options: Option<&kiana_ai::types::StreamOptions>) -> JsArg {
    let json = options.map(stream_options_json).unwrap_or(JsonValue::Null);
    let token = options.and_then(|options| options.signal.clone());
    let on_payload = options.and_then(|options| options.on_payload.clone());
    let on_response = options.and_then(|options| options.on_response.clone());
    let on_stream_event = options.and_then(|options| options.on_provider_stream_event.clone());
    JsArg::Build(Arc::new(move |ctx: &Ctx<'_>| {
        let value = json_value(ctx, &json)?;
        if let Some(object) = value.as_object() {
            if let Some(token) = token.as_ref() {
                object.set("signal", abort_signal_value(ctx, token)?)?;
            }
            add_provider_callbacks(
                ctx,
                object,
                on_payload.clone(),
                on_response.clone(),
                on_stream_event.clone(),
            )?;
        }
        Ok(value)
    }))
}

/// `SimpleStreamOptions` → JS 实参（含真实 `AbortSignal`）。
fn simple_stream_options_arg(options: kiana_ai::types::SimpleStreamOptions) -> JsArg {
    let json = simple_stream_options_json(&options);
    let token = options.base.signal.clone();
    let on_payload = options.base.on_payload.clone();
    let on_response = options.base.on_response.clone();
    let on_stream_event = options.base.on_provider_stream_event.clone();
    JsArg::Build(Arc::new(move |ctx: &Ctx<'_>| {
        let value = json_value(ctx, &json)?;
        if let Some(object) = value.as_object() {
            if let Some(token) = token.as_ref() {
                object.set("signal", abort_signal_value(ctx, token)?)?;
            }
            add_provider_callbacks(
                ctx,
                object,
                on_payload.clone(),
                on_response.clone(),
                on_stream_event.clone(),
            )?;
        }
        Ok(value)
    }))
}

impl JsCallback {
    /// 调用并保存**原始**返回值（用于 `streamSimple` 返回的 JS 流对象）。
    fn call_persistent(
        &self,
        args: Vec<JsArg>,
    ) -> Result<Persistent<Value<'static>>, JsCallbackError> {
        let engine = current_engine().ok_or_else(engine_missing_error)?;
        engine.with_js(|ctx| {
            self.with_function(&ctx, |ctx, function| {
                let collected = build_js_args(ctx, args)?;
                match function.call_arg::<Value<'_>>(collected) {
                    Ok(value) => Ok(Persistent::save(ctx, value)),
                    Err(_) => Err(js_error_from_ctx(ctx)),
                }
            })?
        })
    }
}

/// 驱动一个 JS `AssistantMessageEventStream`（async iterable），把事件推入 Rust 流。
///
/// 按 async iterable 协议驱动：先取 `[Symbol.asyncIterator]()`，
/// 再反复 `next()` 直到 `done`。失败处理：取迭代器失败或 `next()` 被拒绝时记录
/// `tracing::error!` 并 `end_without_result()`；`done`、缺 `value`、Rust 流已关闭时
/// 直接结束循环；无法反序列化的事件记录 `tracing::warn!` 后跳过（TS 里自定义事件是透传的，
/// Rust 的枚举没有对应形态）。
async fn pump_js_stream(
    stream: kiana_ai::event_stream::AssistantMessageEventStream,
    callback: JsCallback,
    args: Vec<JsArg>,
) {
    let Some(engine) = current_engine() else {
        stream.end_without_result();
        return;
    };
    let value = match callback.call_persistent(args) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!("extension streamSimple failed: {}", error.message);
            stream.end_without_result();
            return;
        }
    };
    let iterator: Persistent<Object<'static>> = match engine.with_js(|ctx| {
        let value = value.restore(&ctx).map_err(|_| js_error_from_ctx(&ctx))?;
        if value.is_null() || value.is_undefined() {
            return Err(JsCallbackError {
                name: "TypeError".to_string(),
                message: "streamSimple did not return an event stream".to_string(),
            });
        }
        let get_iterator: Function<'_> =
            ctx.eval("(value) => { const method = value[Symbol.asyncIterator]; return method ? method.call(value) : undefined; }")?;
        let iterator: Value<'_> = get_iterator.call((value,))?;
        let iterator = iterator.as_object().cloned().ok_or_else(|| JsCallbackError {
            name: "TypeError".to_string(),
            message: "streamSimple result is not async iterable".to_string(),
        })?;
        Ok(Persistent::save(&ctx, iterator))
    }) {
        Ok(iterator) => iterator,
        Err(error) => {
            tracing::error!("extension streamSimple failed: {}", error.message);
            stream.end_without_result();
            return;
        }
    };

    loop {
        let promise = match engine.with_js(
            |ctx| -> Result<Persistent<Promise<'static>>, JsCallbackError> {
                let iterator = iterator
                    .clone()
                    .restore(&ctx)
                    .map_err(|error| JsCallbackError {
                        name: "Error".to_string(),
                        message: error.to_string(),
                    })?;
                let get_next: Function<'_> = ctx
                    .eval("(iterator) => iterator.next")
                    .map_err(|_| js_error_from_ctx(&ctx))?;
                let next: Function<'_> = get_next
                    .call((iterator.clone(),))
                    .map_err(|_| js_error_from_ctx(&ctx))?;
                let promise: Promise<'_> = next
                    .call((rquickjs::function::This(iterator),))
                    .map_err(|_| js_error_from_ctx(&ctx))?;
                Ok(Persistent::save(&ctx, promise))
            },
        ) {
            Ok(promise) => promise,
            Err(error) => {
                tracing::error!("extension streamSimple failed: {}", error.message);
                stream.end_without_result();
                return;
            }
        };
        let item = match await_js_promise(&engine, promise).await {
            Ok(item) => item,
            Err(error) => {
                tracing::error!("extension streamSimple failed: {}", error.message);
                stream.end_without_result();
                return;
            }
        };
        let done = item
            .get("done")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if done {
            break;
        }
        let Some(value) = item.get("value") else {
            break;
        };
        match serde_json::from_value::<kiana_ai::types::AssistantMessageEvent>(value.clone()) {
            Ok(event) => {
                if stream.push(event).is_err() {
                    break;
                }
            }
            Err(_error) => {
                // 无法识别的（自定义）事件在 TS 里是透传的；Rust 的枚举没有对应形态。
                tracing::warn!("extension streamSimple emitted an unrecognized event");
            }
        }
    }
}

/// JS `streamSimple(model, context, options)` → Rust 流（同步返回，后台驱动 JS 迭代器）。
fn provider_stream_simple_from_js<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
) -> kiana_ai::compat::ApiStreamSimpleFunction {
    let callback = JsCallback::new(ctx, function);
    Arc::new(
        move |model: &kiana_ai::types::Model,
              context: &kiana_ai::types::TranscriptContext,
              options: kiana_ai::types::SimpleStreamOptions| {
            let stream = kiana_ai::event_stream::AssistantMessageEventStream::new();
            let output = stream.clone();
            let args = vec![
                JsArg::Json(serde_json::to_value(model).unwrap_or(JsonValue::Null)),
                JsArg::Json(serde_json::to_value(context).unwrap_or(JsonValue::Null)),
                simple_stream_options_arg(options),
            ];
            tokio::task::spawn_local(async move {
                pump_js_stream(output, callback, args).await;
            });
            stream
        },
    )
}

/// JS `refreshModels(context)` → provider 模型列表。
fn provider_refresh_models_from_js<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
) -> kiana_ext_api::types::RefreshModelsFn {
    let callback = JsCallback::new(ctx, function);
    Arc::new(move |context: kiana_ai::models::RefreshModelsContext| {
        let callback = callback;
        let payload = json!({
            "credential": context.credential,
            "stored": context.stored,
            "allowNetwork": context.allow_network,
            "force": context.force,
            "baseUrl": context.base_url,
        });
        Box::pin(async move {
            let value = callback
                .call_async_send(vec![JsArg::Json(payload)])
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?;
            serde_json::from_value::<Vec<kiana_ext_api::types::ProviderModelConfig>>(value)
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })
        })
    })
}

/// JS 图像生成实现（`images[api] = { generateImages(...) }`）。
struct JsProviderImages {
    callback: JsCallback,
}

#[async_trait::async_trait]
impl kiana_ai::models::ProviderImages for JsProviderImages {
    async fn generate_images(
        &self,
        model: &kiana_ai::types::Model,
        context: &kiana_ai::types::ImagesContext,
        options: Option<&kiana_ai::models::ImagesOptions>,
    ) -> kiana_ai::types::AssistantImages {
        let model_json = serde_json::to_value(model).unwrap_or(JsonValue::Null);
        let args = vec![
            JsArg::Json(model_json.clone()),
            JsArg::Json(serde_json::to_value(context).unwrap_or(JsonValue::Null)),
            stream_options_arg(options),
        ];
        match self.callback.call_async_send(args).await {
            Ok(value) => serde_json::from_value(value)
                .unwrap_or_else(|error| images_error_result(&model_json, error.to_string())),
            Err(error) => images_error_result(&model_json, error.message),
        }
    }
}

fn images_error_result(
    model_json: &JsonValue,
    message: String,
) -> kiana_ai::types::AssistantImages {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    serde_json::from_value(json!({
        "api": model_json.get("api").cloned().unwrap_or(JsonValue::Null),
        "provider": model_json.get("provider").cloned().unwrap_or(JsonValue::Null),
        "model": model_json.get("id").cloned().unwrap_or(JsonValue::Null),
        "output": [],
        "stopReason": "error",
        "errorMessage": message,
        "timestamp": timestamp,
    }))
    .unwrap_or_else(|_| {
        serde_json::from_value(json!({
            "api": "kiana-virtual",
            "provider": "unknown",
            "model": "unknown",
            "output": [],
            "stopReason": "error",
            "errorMessage": message,
            "timestamp": timestamp,
        }))
        .expect("minimal AssistantImages")
    })
}

/// JS 分类实现（`classifiers[api] = { classify(...) }`）。
struct JsProviderClassifier {
    callback: JsCallback,
}

#[async_trait::async_trait]
impl kiana_ai::models::ProviderClassifier for JsProviderClassifier {
    async fn classify(
        &self,
        model: &kiana_ai::types::Model,
        context: &kiana_ai::types::ClassifierContext,
        options: Option<&kiana_ai::models::ClassifierOptions>,
    ) -> kiana_ai::types::ClassifierResult {
        let model_json = serde_json::to_value(model).unwrap_or(JsonValue::Null);
        let args = vec![
            JsArg::Json(model_json.clone()),
            JsArg::Json(serde_json::to_value(context).unwrap_or(JsonValue::Null)),
            stream_options_arg(options),
        ];
        match self.callback.call_async_send(args).await {
            Ok(value) => serde_json::from_value(value)
                .unwrap_or_else(|error| classifier_error_result(&model_json, error.to_string())),
            Err(error) => classifier_error_result(&model_json, error.message),
        }
    }
}

fn classifier_error_result(
    model_json: &JsonValue,
    message: String,
) -> kiana_ai::types::ClassifierResult {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    serde_json::from_value(json!({
        "api": model_json.get("api").cloned().unwrap_or(JsonValue::Null),
        "provider": model_json.get("provider").cloned().unwrap_or(JsonValue::Null),
        "model": model_json.get("id").cloned().unwrap_or(JsonValue::Null),
        "answers": {},
        "stopReason": "error",
        "errorMessage": message,
        "timestamp": timestamp,
    }))
    .unwrap_or_else(|_| {
        serde_json::from_value(json!({
            "api": "kiana-virtual",
            "provider": "unknown",
            "model": "unknown",
            "answers": {},
            "stopReason": "error",
            "errorMessage": message,
            "timestamp": timestamp,
        }))
        .expect("minimal ClassifierResult")
    })
}

/// JS 定义的**原生 provider 对象**（`kiana.registerProvider(provider)`）的 Rust 适配。
///
/// TS 直接把扩展给的 provider 对象放进 `pendingNativeProviderRegistrations`（bindCore 时
/// 交给 model registry）；Rust 的队列元素是 `Arc<dyn Provider>`，因此这里在注册时物化：
///
/// - `id` / `name` / `baseUrl` / `headers` 直接读取；
/// - `getModels()` 在注册时同步调用一次并缓存（`get_models` 允许在任意线程同步调用，
///   JS 引擎只在主线程可用，因此不保留实时回调）；
/// - `stream` / `streamSimple` 保留 `JsCallback`，经 [`pump_js_stream`] 驱动（与本文件对
///   `ProviderConfig.streamSimple` 的桥一致）；
/// - `auth` 固定为 `ProviderAuth::default()`：扩展自带的 auth 方法（OAuth/API-key 交互）
///   属于 kiana-ai 绑定单元的桥接范围；依赖 auth 回调的原生 provider 暂不解析凭据。
struct JsNativeProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<kiana_ai::types::ProviderHeaders>,
    auth: kiana_ai::auth::types::ProviderAuth,
    models: Vec<Model>,
    stream: Option<JsCallback>,
    stream_simple: Option<JsCallback>,
}

fn native_js_stream(callback: Option<JsCallback>, args: Vec<JsArg>) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let output = stream.clone();
    match callback {
        Some(callback) => {
            tokio::task::spawn_local(async move {
                pump_js_stream(output, callback, args).await;
            });
        }
        None => {
            tokio::task::spawn_local(async move {
                output.end_without_result();
            });
        }
    }
    stream
}

impl kiana_ai::models::Provider for JsNativeProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn headers(&self) -> Option<&kiana_ai::types::ProviderHeaders> {
        self.headers.as_ref()
    }

    fn auth(&self) -> &kiana_ai::auth::types::ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn stream(
        &self,
        model: &Model,
        context: &kiana_ai::types::TranscriptContext,
        options: kiana_ai::types::StreamOptions,
    ) -> AssistantMessageEventStream {
        let args = vec![
            JsArg::Json(serde_json::to_value(model).unwrap_or(JsonValue::Null)),
            JsArg::Json(serde_json::to_value(context).unwrap_or(JsonValue::Null)),
            stream_options_arg(Some(&options)),
        ];
        native_js_stream(self.stream, args)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &kiana_ai::types::TranscriptContext,
        options: kiana_ai::types::SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let args = vec![
            JsArg::Json(serde_json::to_value(model).unwrap_or(JsonValue::Null)),
            JsArg::Json(serde_json::to_value(context).unwrap_or(JsonValue::Null)),
            simple_stream_options_arg(options),
        ];
        native_js_stream(self.stream_simple, args)
    }
}

/// JS provider 对象 → [`JsNativeProvider`]。
pub(crate) fn native_provider_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> EngineResult<Arc<dyn kiana_ai::models::Provider>> {
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            return throw_message(ctx, "registerProvider(provider) requires a provider object");
        }
    };
    let id = get_string(object, "id")?.unwrap_or_default();
    let name = get_string(object, "name")?.unwrap_or_else(|| id.clone());
    let base_url = get_string(object, "baseUrl")?;
    let headers = match get_json(ctx, object, "headers")? {
        Some(JsonValue::Object(map)) => Some(IndexMap::from_iter(map.into_iter().filter_map(
            |(key, value)| match value {
                JsonValue::String(text) => Some((key, Some(text))),
                JsonValue::Null => Some((key, None)),
                _ => None,
            },
        ))),
        _ => None,
    };
    // `getModels()` / `getAllModels()`：注册时同步取一次（此时仍在 JS 事务里）。
    // TS 以 provider 对象为 `this` 调用（`provider.getModels()`）；扩展常把数据挂在
    // 对象上（`getModels() { return this.models; }`），未绑定的调用会得到
    // `this=undefined` 并抛 TypeError。
    let models = match get_function(object, "getModels")? {
        Some(function) => {
            match function.call::<_, Value<'js>>((rquickjs::function::This(object.clone()),)) {
                Ok(value) => value_to_json(ctx, value)
                    .ok()
                    .and_then(|json| serde_json::from_value::<Vec<Model>>(json).ok())
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        }
        None => Vec::new(),
    };
    let stream = get_function(object, "stream")?
        .map(|function| JsCallback::new_with_this(ctx, function, object.clone().into_value()));
    let stream_simple = get_function(object, "streamSimple")?
        .map(|function| JsCallback::new_with_this(ctx, function, object.clone().into_value()));
    Ok(Arc::new(JsNativeProvider {
        id,
        name,
        base_url,
        headers,
        auth: kiana_ai::auth::types::ProviderAuth::default(),
        models,
        stream,
        stream_simple,
    }))
}

/// `images` / `classifiers` 映射 → Rust 实现表。
fn provider_impl_map<'js, T: ?Sized>(
    ctx: &Ctx<'js>,
    value: Option<Value<'js>>,
    method: &str,
    wrap: impl Fn(JsCallback) -> Arc<T>,
) -> EngineResult<Option<IndexMap<String, Arc<T>>>> {
    let Some(value) = value.filter(|value| value.is_object()) else {
        return Ok(None);
    };
    let object = value.as_object().expect("checked object");
    let mut map = IndexMap::new();
    for key in object.keys::<String>() {
        let key = key?;
        let entry: Value<'js> = object.get(key.as_str())?;
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(function) = get_function(entry, method)? else {
            continue;
        };
        map.insert(key, wrap(JsCallback::new(ctx, function)));
    }
    Ok(Some(map))
}

// ---- OAuth 桥 --------------------------------------------------------------

enum OAuthUiHandler {
    Prompt(kiana_ai::compat::extension_oauth_types::OAuthPromptHandler),
    Select(kiana_ai::compat::extension_oauth_types::OAuthSelectHandler),
    Manual(kiana_ai::compat::extension_oauth_types::OAuthManualCodeInputHandler),
}

thread_local! {
    static OAUTH_UI_HANDLERS: RefCell<HashMap<u64, OAuthUiHandler>> = RefCell::new(HashMap::new());
}

fn store_oauth_ui_handler(handler: OAuthUiHandler) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    OAUTH_UI_HANDLERS.with(|handlers| {
        handlers.borrow_mut().insert(id, handler);
    });
    id
}

/// `loader.oauth` host kind：驱动 `onPrompt` / `onSelect` / `onManualCodeInput` 的 future。
///
/// 与 `loader.projectTrustUi` 相同的一次性语义：handler 按 `handler` id 取走后即失效
/// （重复调用被拒绝）。三种 `op` 分别对应 prompt（必返回值）、select（`None` → `undefined`）
/// 与 manual（无入参）；失败以 `error.message` 拒绝 Promise。
fn handle_oauth_ui(call: HostCall<'_>) -> EngineResult<()> {
    let payload: JsonValue = call.payload_json()?;
    let id = call.id();
    let sender = call.completion_sender();
    let Some(handler_id) = payload.get("handler").and_then(JsonValue::as_u64) else {
        sender.reject_message(id, "invalid oauth ui call");
        return Ok(());
    };
    let Some(handler) =
        OAUTH_UI_HANDLERS.with(|handlers| handlers.borrow_mut().remove(&handler_id))
    else {
        sender.reject_message(id, "oauth ui handler is no longer available");
        return Ok(());
    };
    match handler {
        OAuthUiHandler::Prompt(handler) => {
            let prompt = payload.get("prompt").cloned().unwrap_or(JsonValue::Null);
            tokio::task::spawn_local(async move {
                match serde_json::from_value(prompt) {
                    Ok(prompt) => match handler(prompt).await {
                        Ok(value) => {
                            sender.resolve(id, HostValue::String(value));
                        }
                        Err(error) => {
                            sender.reject_message(id, error.message);
                        }
                    },
                    Err(error) => {
                        sender.reject_message(id, error.to_string());
                    }
                }
            });
        }
        OAuthUiHandler::Select(handler) => {
            let prompt = payload.get("prompt").cloned().unwrap_or(JsonValue::Null);
            tokio::task::spawn_local(async move {
                match serde_json::from_value(prompt) {
                    Ok(prompt) => match handler(prompt).await {
                        Ok(Some(value)) => {
                            sender.resolve(id, HostValue::String(value));
                        }
                        Ok(None) => {
                            sender.resolve(id, HostValue::Undefined);
                        }
                        Err(error) => {
                            sender.reject_message(id, error.message);
                        }
                    },
                    Err(error) => {
                        sender.reject_message(id, error.to_string());
                    }
                }
            });
        }
        OAuthUiHandler::Manual(handler) => {
            tokio::task::spawn_local(async move {
                match handler().await {
                    Ok(value) => {
                        sender.resolve(id, HostValue::String(value));
                    }
                    Err(error) => {
                        sender.reject_message(id, error.message);
                    }
                }
            });
        }
    }
    Ok(())
}

/// `OAuthLoginCallbacks` → JS 回调对象。
fn oauth_callbacks_value<'js>(
    ctx: &Ctx<'js>,
    callbacks: &kiana_ai::compat::extension_oauth_types::OAuthLoginCallbacks,
) -> EngineResult<Object<'js>> {
    use kiana_ai::compat::extension_oauth_types::{
        OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthPrompt, OAuthSelectPrompt,
    };

    let object = Object::new(ctx.clone())?;

    let on_auth = Arc::clone(&callbacks.on_auth);
    object.set(
        "onAuth",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, info: Value<'js>| {
            if let Ok(info) = serde_json::from_value::<OAuthAuthInfo>(
                value_to_json(&ctx, info).unwrap_or(JsonValue::Null),
            ) {
                on_auth(info);
            }
        })?,
    )?;
    let on_device_code = Arc::clone(&callbacks.on_device_code);
    object.set(
        "onDeviceCode",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, info: Value<'js>| {
            if let Ok(info) = serde_json::from_value::<OAuthDeviceCodeInfo>(
                value_to_json(&ctx, info).unwrap_or(JsonValue::Null),
            ) {
                on_device_code(info);
            }
        })?,
    )?;
    if let Some(on_progress) = callbacks.on_progress.clone() {
        object.set(
            "onProgress",
            Function::new(ctx.clone(), move |message: String| {
                on_progress(message);
            })?,
        )?;
    }
    let on_prompt = Arc::clone(&callbacks.on_prompt);
    object.set(
        "onPrompt",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, prompt: Value<'js>| -> EngineResult<Value<'js>> {
                let prompt: OAuthPrompt = serde_json::from_value(value_to_json(&ctx, prompt)?)
                    .map_err(|error| {
                        Exception::throw_message(&ctx, &format!("invalid OAuthPrompt: {error}"))
                    })?;
                let id = store_oauth_ui_handler(OAuthUiHandler::Prompt(Arc::clone(&on_prompt)));
                call_kiana_promise(
                    &ctx,
                    "loader.oauth",
                    json!({ "handler": id, "op": "prompt", "prompt": prompt }),
                )
            },
        )?,
    )?;
    let on_select = Arc::clone(&callbacks.on_select);
    object.set(
        "onSelect",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, prompt: Value<'js>| -> EngineResult<Value<'js>> {
                let prompt: OAuthSelectPrompt =
                    serde_json::from_value(value_to_json(&ctx, prompt)?).map_err(|error| {
                        Exception::throw_message(
                            &ctx,
                            &format!("invalid OAuthSelectPrompt: {error}"),
                        )
                    })?;
                let id = store_oauth_ui_handler(OAuthUiHandler::Select(Arc::clone(&on_select)));
                call_kiana_promise(
                    &ctx,
                    "loader.oauth",
                    json!({ "handler": id, "op": "select", "prompt": prompt }),
                )
            },
        )?,
    )?;
    if let Some(on_manual) = callbacks.on_manual_code_input.clone() {
        object.set(
            "onManualCodeInput",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
                    let id = store_oauth_ui_handler(OAuthUiHandler::Manual(Arc::clone(&on_manual)));
                    call_kiana_promise(
                        &ctx,
                        "loader.oauth",
                        json!({ "handler": id, "op": "manual" }),
                    )
                },
            )?,
        )?;
    }
    if let Some(signal) = &callbacks.signal {
        object.set("signal", abort_signal_value(ctx, &signal.token())?)?;
    } else {
        object.set("signal", Value::new_undefined(ctx.clone()))?;
    }
    Ok(object)
}

/// JS `oauth` 配置 → [`kiana_ext_api::types::ProviderOAuthConfig`]。
///
/// `login` / `refreshToken` / `getApiKey` 缺失时抛 JS 异常；`modifyModels` 可选，
/// 缺省为 `None`。
fn provider_oauth_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> EngineResult<kiana_ext_api::types::ProviderOAuthConfig> {
    use kiana_ai::compat::extension_oauth_types::OAuthCredentials;

    let object = match value.as_object() {
        Some(object) => object,
        None => return throw_message(ctx, "Provider oauth must be an object"),
    };
    let name = get_string(object, "name")?.unwrap_or_default();
    let is_subscription = object
        .get::<_, Option<Value<'js>>>("isSubscription")?
        .and_then(|value| value.as_bool());
    let uses_callback_server = object
        .get::<_, Option<Value<'js>>>("usesCallbackServer")?
        .and_then(|value| value.as_bool());
    let login_function = get_function(object, "login")?
        .ok_or_else(|| Exception::throw_message(ctx, "Provider oauth must define login()"))?;
    let login_callback = JsCallback::new(ctx, login_function);
    let login: kiana_ext_api::types::OAuthLoginFn = Arc::new(move |callbacks| {
        let login_callback = login_callback;
        Box::pin(async move {
            let args_builder: JsArgBuilder = Arc::new(move |ctx: &Ctx<'_>| {
                let object = oauth_callbacks_value(ctx, &callbacks)?;
                Ok(object.into_value())
            });
            let value = login_callback
                .call_async_send(vec![JsArg::Build(args_builder)])
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?;
            serde_json::from_value::<OAuthCredentials>(value)
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })
        })
    });
    let refresh_function = get_function(object, "refreshToken")?.ok_or_else(|| {
        Exception::throw_message(ctx, "Provider oauth must define refreshToken()")
    })?;
    let refresh_callback = JsCallback::new(ctx, refresh_function);
    let refresh_token: kiana_ext_api::types::OAuthRefreshTokenFn = Arc::new(
        move |credentials: OAuthCredentials, signal: tokio_util::sync::CancellationToken| {
            let refresh_callback = refresh_callback;
            Box::pin(async move {
                let credentials = serde_json::to_value(&credentials).unwrap_or(JsonValue::Null);
                let token = signal.clone();
                let signal_arg: JsArgBuilder =
                    Arc::new(move |ctx: &Ctx<'_>| abort_signal_value(ctx, &token));
                let value = refresh_callback
                    .call_async_send(vec![JsArg::Json(credentials), JsArg::Build(signal_arg)])
                    .await
                    .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
                        Box::new(error)
                    })?;
                serde_json::from_value::<OAuthCredentials>(value).map_err(
                    |error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) },
                )
            })
        },
    );
    let get_api_key_function = get_function(object, "getApiKey")?
        .ok_or_else(|| Exception::throw_message(ctx, "Provider oauth must define getApiKey()"))?;
    let get_api_key_callback = JsCallback::new(ctx, get_api_key_function);
    let get_api_key: Arc<dyn Fn(&OAuthCredentials) -> String + Send + Sync> =
        Arc::new(move |credentials: &OAuthCredentials| {
            let json = serde_json::to_value(credentials).unwrap_or(JsonValue::Null);
            match get_api_key_callback.call(vec![JsArg::Json(json)]) {
                Ok(JsCallResult::Value(JsonValue::String(value))) => value,
                _ => String::new(),
            }
        });
    let modify_models = get_function(object, "modifyModels")?.map(|function| {
        let callback = JsCallback::new(ctx, function);
        Arc::new(move |models: Vec<Model>, credentials: &OAuthCredentials| {
            let models = serde_json::to_value(&models).unwrap_or(JsonValue::Null);
            let credentials = serde_json::to_value(credentials).unwrap_or(JsonValue::Null);
            match callback.call(vec![JsArg::Json(models), JsArg::Json(credentials)]) {
                Ok(JsCallResult::Value(value)) => serde_json::from_value(value).unwrap_or_default(),
                _ => Vec::new(),
            }
        }) as Arc<dyn Fn(Vec<Model>, &OAuthCredentials) -> Vec<Model> + Send + Sync>
    });
    Ok(kiana_ext_api::types::ProviderOAuthConfig {
        name,
        is_subscription,
        uses_callback_server,
        login,
        refresh_token,
        get_api_key,
        modify_models,
    })
}

/// JS `ProviderConfig` → [`ProviderConfig`]。
///
/// `models` 的解析是**延迟校验**的一部分：TS 在加载期只保存原始配置，校验发生在
/// `runner.bindCore` 的冲刷循环（抛错只产生一条扩展错误，扩展保持已加载）。Rust 的
/// `ProviderConfig` 是类型化的，无法携带原始对象，因此解析失败时返回
/// `(config, Some(message))`，由调用方记录到 [`RuntimeShared::provider_config_errors`] 并
/// 跳过该注册（效果等同于 bind 时抛错、provider 未注册）。
pub(crate) fn provider_config_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> EngineResult<(ProviderConfig, Option<String>)> {
    let object = match value.as_object() {
        Some(object) => object,
        None => return throw_message(ctx, "Provider config is required when registering by name"),
    };
    let headers = match get_json(ctx, object, "headers")? {
        Some(JsonValue::Object(map)) => Some(IndexMap::from_iter(
            map.into_iter()
                .filter_map(|(key, value)| value.as_str().map(|text| (key, text.to_string()))),
        )),
        _ => None,
    };
    let (models, models_error) = match get_json(ctx, object, "models")? {
        Some(value) => {
            match serde_json::from_value::<Vec<kiana_ext_api::types::ProviderModelConfig>>(value) {
                Ok(models) => (Some(models), None),
                Err(error) => (None, Some(format!("invalid models: {error}"))),
            }
        }
        None => (None, None),
    };
    let api = get_string(object, "api")?;
    let stream_simple = get_function(object, "streamSimple")?
        .map(|function| provider_stream_simple_from_js(ctx, function));
    let refresh_models = get_function(object, "refreshModels")?
        .map(|function| provider_refresh_models_from_js(ctx, function));
    let images = provider_impl_map(
        ctx,
        object.get::<_, Option<Value<'js>>>("images")?,
        "generateImages",
        |callback| {
            Arc::new(JsProviderImages { callback }) as Arc<dyn kiana_ai::models::ProviderImages>
        },
    )?;
    let classifiers = provider_impl_map(
        ctx,
        object.get::<_, Option<Value<'js>>>("classifiers")?,
        "classify",
        |callback| {
            Arc::new(JsProviderClassifier { callback })
                as Arc<dyn kiana_ai::models::ProviderClassifier>
        },
    )?;
    let oauth = match object.get::<_, Option<Value<'js>>>("oauth")? {
        Some(value) if !value.is_undefined() && !value.is_null() => {
            Some(provider_oauth_from_js(ctx, value)?)
        }
        _ => None,
    };
    Ok((
        ProviderConfig {
            name: get_string(object, "name")?,
            base_url: get_string(object, "baseUrl")?,
            api_key: get_string(object, "apiKey")?,
            api,
            stream_simple,
            images,
            classifiers,
            headers,
            auth_header: match object.get::<_, Option<Value<'js>>>("authHeader")? {
                Some(value) => value.as_bool(),
                None => None,
            },
            models,
            refresh_models,
            oauth,
        },
        models_error,
    ))
}

/// `ModelRouteRequest` → JS 对象（**逐字** TS 形状；`ModelRouteRequest` 没有实现 `Serialize`）。
///
/// 可选字段在 TS 里是 `undefined`（不是 `null`），`signal` 是真实 `AbortSignal`；这些都必须
/// 在 `Ctx` 里直接构造，不能走 JSON。
fn route_request_value<'js>(
    ctx: &Ctx<'js>,
    request: &ModelRouteRequest,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    // 键顺序对齐 TS `model-runtime.resolveModel` 的对象字面量：
    // `{ ...request, model, previous, failed, messages }`，其中 request 依次带
    // reason / thinkingLevel / signal / state（可选属性以 `undefined` 存在）。
    object.set(
        "reason",
        json_value(
            ctx,
            &serde_json::to_value(request.reason).unwrap_or(JsonValue::Null),
        )?,
    )?;
    object.set(
        "thinkingLevel",
        json_value(
            ctx,
            &serde_json::to_value(request.thinking_level).unwrap_or(JsonValue::Null),
        )?,
    )?;
    match &request.signal {
        Some(scope) => object.set("signal", abort_signal_value(ctx, &scope.token())?)?,
        None => object.set("signal", Value::new_undefined(ctx.clone()))?,
    }
    match &request.state {
        Some(state) => object.set("state", json_value(ctx, state)?)?,
        None => object.set("state", Value::new_undefined(ctx.clone()))?,
    }
    object.set(
        "model",
        json_value(
            ctx,
            &serde_json::to_value(&request.model).unwrap_or(JsonValue::Null),
        )?,
    )?;
    match &request.previous {
        Some(previous) => {
            let value = Object::new(ctx.clone())?;
            value.set(
                "model",
                json_value(
                    ctx,
                    &serde_json::to_value(&previous.model).unwrap_or(JsonValue::Null),
                )?,
            )?;
            match previous.thinking_level {
                Some(level) => value.set(
                    "thinkingLevel",
                    json_value(ctx, &serde_json::to_value(level).unwrap_or(JsonValue::Null))?,
                )?,
                None => value.set("thinkingLevel", Value::new_undefined(ctx.clone()))?,
            }
            object.set("previous", value)?;
        }
        None => object.set("previous", Value::new_undefined(ctx.clone()))?,
    }
    match &request.failed {
        Some(failed) => {
            let value = Object::new(ctx.clone())?;
            value.set(
                "model",
                json_value(
                    ctx,
                    &serde_json::to_value(&failed.model).unwrap_or(JsonValue::Null),
                )?,
            )?;
            match failed.thinking_level {
                Some(level) => value.set(
                    "thinkingLevel",
                    json_value(ctx, &serde_json::to_value(level).unwrap_or(JsonValue::Null))?,
                )?,
                None => value.set("thinkingLevel", Value::new_undefined(ctx.clone()))?,
            }
            value.set(
                "message",
                json_value(
                    ctx,
                    &serde_json::to_value(&failed.message).unwrap_or(JsonValue::Null),
                )?,
            )?;
            object.set("failed", value)?;
        }
        None => object.set("failed", Value::new_undefined(ctx.clone()))?,
    }
    object.set(
        "messages",
        json_value(
            ctx,
            &serde_json::to_value(&request.messages).unwrap_or(JsonValue::Null),
        )?,
    )?;
    Ok(object.into_value())
}

fn route_from_json(value: JsonValue) -> Result<ModelRoute, VirtualModelRouteError> {
    let object = value
        .as_object()
        .ok_or_else(|| VirtualModelRouteError::new("Virtual model route must return an object"))?;
    let model: Model = serde_json::from_value(
        object
            .get("model")
            .cloned()
            .ok_or_else(|| VirtualModelRouteError::new("Virtual model route is missing model"))?,
    )
    .map_err(|error| VirtualModelRouteError::new(format!("invalid route model: {error}")))?;
    let thinking_level: ModelThinkingLevel =
        serde_json::from_value(object.get("thinkingLevel").cloned().ok_or_else(|| {
            VirtualModelRouteError::new("Virtual model route is missing thinkingLevel")
        })?)
        .map_err(|error| {
            VirtualModelRouteError::new(format!("invalid route thinkingLevel: {error}"))
        })?;
    Ok(ModelRoute {
        model,
        thinking_level,
        state: object.get("state").cloned(),
    })
}

/// JS `route(request, ctx)` 适配器。
///
/// TS 的 `definition.route` 包装调用 `model.route(request, …)`（`loader.ts:509`），`this`
/// 是扩展传入的虚拟模型对象；实现里读 `this` 的路由函数在 Rust 侧曾收到 `undefined`。
fn js_route_fn<'js>(
    ctx: &Ctx<'js>,
    function: Function<'js>,
    receiver: Option<Object<'js>>,
) -> kiana_ext_api::types::VirtualModelRouteFn {
    let callback = match receiver {
        Some(receiver) => JsCallback::new_with_this(ctx, function, receiver.into_value()),
        None => JsCallback::new(ctx, function),
    };
    Arc::new(
        move |request: ModelRouteRequest, context: Arc<dyn ExtensionContextApi>| {
            let callback = callback;
            let request_arg: JsArgBuilder =
                Arc::new(move |ctx: &Ctx<'_>| route_request_value(ctx, &request));
            Box::pin(async move {
                let value = callback
                    .call_async_send(vec![JsArg::Build(request_arg), JsArg::Context(context)])
                    .await
                    .map_err(|error| VirtualModelRouteError {
                        name: error.name,
                        message: error.message,
                    })?;
                route_from_json(value)
            }) as RouteFuture
        },
    )
}

/// 虚拟模型定义 → [`ExtensionVirtualModel`]（JS 形状）。
pub(crate) fn virtual_model_from_js<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> EngineResult<ExtensionVirtualModel> {
    // TS 的 `{...model, route}` 用对象展开：`null`/`undefined`/原始值都不会抛错，展开后
    // `provider`/`id` 等字段是 `undefined`。Rust 的字符串字段用空串表达 `undefined`。
    let object = value.as_object();
    let provider = match &object {
        Some(object) => get_string(object, "provider")?.unwrap_or_default(),
        None => String::new(),
    };
    let id = match &object {
        Some(object) => get_string(object, "id")?.unwrap_or_default(),
        None => String::new(),
    };
    let name = match &object {
        Some(object) => get_string(object, "name")?.unwrap_or_default(),
        None => String::new(),
    };
    let thinking_levels = match &object {
        Some(object) => match get_json(ctx, object, "thinkingLevels")? {
            Some(value) => Some(parse_json::<Vec<ModelThinkingLevel>>(
                ctx,
                value,
                "thinkingLevels",
            )?),
            None => None,
        },
        None => None,
    };
    let input = match &object {
        Some(object) => match get_json(ctx, object, "input")? {
            Some(value) => Some(parse_json::<Vec<ModelInputModality>>(ctx, value, "input")?),
            None => None,
        },
        None => None,
    };
    let context_window = match &object {
        Some(object) => object
            .get::<_, Option<Value<'js>>>("contextWindow")?
            .and_then(|value| value.as_number())
            .map(|value| value as u64),
        None => None,
    };
    let max_tokens = match &object {
        Some(object) => object
            .get::<_, Option<Value<'js>>>("maxTokens")?
            .and_then(|value| value.as_number())
            .map(|value| value as u64),
        None => None,
    };
    let route = match &object {
        Some(object) => match get_function(object, "route")? {
            Some(function) => js_route_fn(ctx, function, Some((*object).clone())),
            None => Arc::new(|_, _| {
                Box::pin(async { Err(VirtualModelRouteError::new("route is not a function")) })
                    as RouteFuture
            }),
        },
        None => Arc::new(|_, _| {
            Box::pin(async { Err(VirtualModelRouteError::new("route is not a function")) })
                as RouteFuture
        }),
    };
    Ok(ExtensionVirtualModel {
        provider,
        id,
        name,
        thinking_levels,
        context_window,
        max_tokens,
        input,
        route,
    })
}

/// `AgentTool` 的 JS 视图（TS 的 `ToolLoadout.declared/callable/registered` 元素）。
fn agent_tool_value<'js>(
    ctx: &Ctx<'js>,
    tool: &kiana_agent_core::AgentTool,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("name", tool.name.clone())?;
    object.set("label", tool.label.clone())?;
    object.set("description", tool.description.clone())?;
    object.set("parameters", json_value(ctx, &tool.parameters)?)?;
    match &tool.constrained_sampling {
        Some(value) => object.set(
            "constrainedSampling",
            json_value(ctx, &serde_json::to_value(value).unwrap_or(JsonValue::Null))?,
        )?,
        None => object.set("constrainedSampling", Value::new_undefined(ctx.clone()))?,
    }
    match &tool.output_schema {
        Some(value) => object.set("outputSchema", json_value(ctx, value)?)?,
        None => object.set("outputSchema", Value::new_undefined(ctx.clone()))?,
    }
    match tool.execution_mode {
        Some(value) => object.set(
            "executionMode",
            json_value(ctx, &serde_json::to_value(value).unwrap_or(JsonValue::Null))?,
        )?,
        None => object.set("executionMode", Value::new_undefined(ctx.clone()))?,
    }
    match tool.replay {
        Some(value) => object.set(
            "replay",
            json_value(ctx, &serde_json::to_value(value).unwrap_or(JsonValue::Null))?,
        )?,
        None => object.set("replay", Value::new_undefined(ctx.clone()))?,
    }
    Ok(object.into_value())
}

fn agent_tools_value<'js>(
    ctx: &Ctx<'js>,
    tools: &[kiana_agent_core::AgentTool],
) -> EngineResult<Value<'js>> {
    let array = rquickjs::Array::new(ctx.clone())?;
    for (index, tool) in tools.iter().enumerate() {
        array.set(index, agent_tool_value(ctx, tool)?)?;
    }
    Ok(array.into_value())
}

/// `ToolInfo` 的 JS 视图（TS `getAllTools()` 的元素，含 `reachesTools` 函数）。
///
/// 键顺序与可选性逐字对齐 TS 的对象字面量：
/// `{name, description, parameters, promptGuidelines, exposure, ...(namespace),
/// ...(annotations), ...(reachesTools), sourceInfo}`。`promptGuidelines` 恒存在
/// （缺省为 `undefined`），其余三个可选键在缺省时**省略**。
fn tool_info_value<'js>(
    ctx: &Ctx<'js>,
    tool: kiana_ext_api::types::ToolInfo,
) -> EngineResult<Value<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("name", tool.name)?;
    object.set("description", tool.description)?;
    object.set("parameters", json_value(ctx, &tool.parameters)?)?;
    match tool.prompt_guidelines {
        Some(guidelines) => object.set(
            "promptGuidelines",
            json_value(
                ctx,
                &serde_json::to_value(guidelines).unwrap_or(JsonValue::Null),
            )?,
        )?,
        None => object.set("promptGuidelines", Value::new_undefined(ctx.clone()))?,
    }
    object.set(
        "exposure",
        json_value(
            ctx,
            &serde_json::to_value(tool.exposure).unwrap_or(JsonValue::Null),
        )?,
    )?;
    if let Some(namespace) = tool.namespace {
        object.set(
            "namespace",
            json_value(
                ctx,
                &serde_json::to_value(namespace).unwrap_or(JsonValue::Null),
            )?,
        )?;
    }
    if let Some(annotations) = tool.annotations {
        object.set(
            "annotations",
            json_value(
                ctx,
                &serde_json::to_value(annotations).unwrap_or(JsonValue::Null),
            )?,
        )?;
    }
    if let Some(reaches) = tool.reaches_tools {
        let reaches = Arc::clone(&reaches);
        object.set(
            "reachesTools",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, input: Value<'js>| -> EngineResult<Value<'js>> {
                    let input = value_to_json(&ctx, input)?;
                    let result = reaches(&input);
                    let json = match result {
                        kiana_ext_api::tool_types::ReachesToolsResult::All => {
                            JsonValue::String("all".to_string())
                        }
                        kiana_ext_api::tool_types::ReachesToolsResult::Names(names) => {
                            JsonValue::Array(names.into_iter().map(JsonValue::String).collect())
                        }
                    };
                    json_value(&ctx, &json)
                },
            )?,
        )?;
    }
    object.set(
        "sourceInfo",
        json_value(
            ctx,
            &serde_json::to_value(tool.source_info).unwrap_or(JsonValue::Null),
        )?,
    )?;
    Ok(object.into_value())
}

/// `kiana.getAllTools()`（需要 JS 函数值，因此不能走 JSON 化的 [`dispatch_action`]）。
fn get_all_tools_value<'js>(entry: &ApiEntry, ctx: &Ctx<'js>) -> Result<Value<'js>, String> {
    entry.check_active()?;
    if lock(&entry.shared.actions).is_none() {
        return Err(NOT_INITIALIZED_MESSAGE.to_string());
    }
    let actions = runtime_actions(&entry.shared);
    let tools = (actions.get_all_tools)();
    let array = rquickjs::Array::new(ctx.clone()).map_err(|error| error.to_string())?;
    for (index, tool) in tools.into_iter().enumerate() {
        let value = tool_info_value(ctx, tool).map_err(|error| error.to_string())?;
        array.set(index, value).map_err(|error| error.to_string())?;
    }
    Ok(array.into_value())
}

/// 动作分派（`kiana.*` 的同步动作方法）。未绑定时返回 [`NOT_INITIALIZED_MESSAGE`]。
fn dispatch_action(entry: &ApiEntry, name: &str, args: JsonValue) -> Result<JsonValue, String> {
    entry.check_active()?;
    if lock(&entry.shared.actions).is_none() {
        return Err(NOT_INITIALIZED_MESSAGE.to_string());
    }
    let actions = runtime_actions(&entry.shared);
    let args = args.as_array().cloned().unwrap_or_default();
    let argument = |index: usize| args.get(index).cloned().unwrap_or(JsonValue::Null);
    let from = |index: usize| -> JsonValue { argument(index) };
    match name {
        "sendMessage" => {
            let message: kiana_ext_api::types::CustomMessagePayload =
                serde_json::from_value(from(0))
                    .map_err(|error| format!("invalid sendMessage message: {error}"))?;
            let options = argument(1);
            let trigger_turn = options.get("triggerTurn").and_then(JsonValue::as_bool);
            let deliver_as = options
                .get("deliverAs")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid sendMessage options: {error}"))?;
            (actions.send_message)(SendMessageOptions {
                message,
                trigger_turn,
                deliver_as,
            });
            Ok(JsonValue::Null)
        }
        "sendUserMessage" => {
            let content: UserContent = serde_json::from_value(argument(0))
                .map_err(|error| format!("invalid sendUserMessage content: {error}"))?;
            let options = match argument(1) {
                JsonValue::Null => None,
                value => Some(
                    serde_json::from_value(value)
                        .map_err(|error| format!("invalid sendUserMessage options: {error}"))?,
                ),
            };
            (actions.send_user_message)(content, options);
            Ok(JsonValue::Null)
        }
        "appendEntry" => {
            let custom_type = argument(0).as_str().unwrap_or_default().to_string();
            let data = match argument(1) {
                JsonValue::Null => None,
                value => Some(value),
            };
            (actions.append_entry)(custom_type, data);
            Ok(JsonValue::Null)
        }
        "setSessionName" => {
            let name = argument(0).as_str().unwrap_or_default().to_string();
            (actions.set_session_name)(name);
            Ok(JsonValue::Null)
        }
        "getSessionName" => Ok(match (actions.get_session_name)() {
            Some(name) => JsonValue::String(name),
            None => JsonValue::Null,
        }),
        "setLabel" => {
            let entry_id = argument(0).as_str().unwrap_or_default().to_string();
            let label = argument(1).as_str().map(str::to_string);
            (actions.set_label)(entry_id, label);
            Ok(JsonValue::Null)
        }
        "getActiveTools" => {
            Ok(serde_json::to_value((actions.get_active_tools)()).unwrap_or(JsonValue::Null))
        }
        "getAllTools" => {
            let tools = (actions.get_all_tools)();
            Ok(JsonValue::Array(
                tools
                    .into_iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                            "promptGuidelines": tool.prompt_guidelines,
                            "exposure": tool.exposure,
                            "namespace": tool.namespace,
                            "annotations": tool.annotations,
                            "sourceInfo": tool.source_info,
                        })
                    })
                    .collect(),
            ))
        }
        "getSettings" => {
            Ok(serde_json::to_value((actions.get_settings)()).unwrap_or(JsonValue::Null))
        }
        "setActiveTools" => {
            let tool_names: Vec<String> = serde_json::from_value(argument(0))
                .map_err(|error| format!("invalid setActiveTools argument: {error}"))?;
            (actions.set_active_tools)(tool_names);
            Ok(JsonValue::Null)
        }
        "getCommands" => {
            Ok(serde_json::to_value((actions.get_commands)()).unwrap_or(JsonValue::Null))
        }
        "getThinkingLevel" => {
            Ok(serde_json::to_value((actions.get_thinking_level)()).unwrap_or(JsonValue::Null))
        }
        "setThinkingLevel" => {
            let level: ThinkingLevel = serde_json::from_value(argument(0))
                .map_err(|error| format!("invalid thinking level: {error}"))?;
            (actions.set_thinking_level)(level);
            Ok(JsonValue::Null)
        }
        other => Err(format!("Unknown extension action: {other}")),
    }
}

fn flags_value_to_js<'js>(ctx: &Ctx<'js>, value: Option<FlagValue>) -> EngineResult<Value<'js>> {
    match value {
        Some(FlagValue::Bool(value)) => Ok(Value::new_bool(ctx.clone(), value)),
        Some(FlagValue::Str(value)) => {
            rquickjs::String::from_str(ctx.clone(), &value).map(|s| s.into_value())
        }
        None => Ok(Value::new_undefined(ctx.clone())),
    }
}

/// 建一个扩展的宿主对象（`js/api.js` 的 `makeApi(host, path)` 的 `host`）。
///
/// 成员名是 `js/api.js` 的固定契约，按职责分组：
///
/// - `checkActive` / `commit` / `discard`：扩展事务的状态机；
/// - `on` / `off` / `eventsEmit` / `eventsOn` / `eventsOff`：handler 与事件总线；
/// - `register*` / `unregister*` / `getFlag` / `getMcpServers`：注册型 API；
/// - `action`：动作方法；`getAllTools` 走单独分支（每一项要带 JS 函数 `reachesTools`）；
/// - `setModel` / `exec`：返回 `__kiana.promise(...)`，由对应 host kind 异步完成；
/// - `isBound`：`bindCore` 是否已把动作绑定到本 runtime。
fn build_host_object<'js>(
    ctx: &Ctx<'js>,
    entry: Rc<ApiEntry>,
    entry_id: u64,
) -> EngineResult<Object<'js>> {
    let host = Object::new(ctx.clone())?;

    {
        let entry = Rc::clone(&entry);
        host.set(
            "checkActive",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> EngineResult<()> {
                match entry.check_active() {
                    Ok(()) => Ok(()),
                    Err(message) => throw_message(&ctx, message),
                }
            })?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "on",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, event: String, handler: Value<'js>| -> EngineResult<u64> {
                    if let Err(message) = entry.check_active() {
                        return throw_message(&ctx, message);
                    }
                    // TS 的 `on()` **不做**运行期类型检查：值原样存进 `extension.handlers`，
                    // 只有调用时才抛 `TypeError: handler is not a function`（由派发层捕获并
                    // 上报为扩展错误）。这里同样让加载成功，注册一个调用时报错的占位。
                    let Some(function) = handler.as_function().cloned() else {
                        let handler = if event == PROJECT_TRUST_EVENT {
                            ExtensionHandler::ProjectTrust(Arc::new(|_event, _ctx| {
                                Box::pin(async { ProjectTrustEventResult::default() })
                            }))
                        } else {
                            ExtensionHandler::Standard(Arc::new(|_event, _ctx| {
                                Box::pin(async {
                                    Err(invalid_handler_error("handler is not a function"))
                                })
                            }))
                        };
                        return Ok(entry.add_js_handler(&event, handler, None));
                    };
                    let callback = JsCallback::new(&ctx, function);
                    let handler = if event == PROJECT_TRUST_EVENT {
                        js_project_trust_handler_adapter(callback, Arc::clone(&entry.shared))
                    } else {
                        let kind = event_kind_for_name(&event);
                        match kind {
                            Some(kind) => js_standard_handler_adapter(callback, kind),
                            // TS 的 `on()` 接受任意字符串（类型仅在编译期约束）；未知事件
                            // 以 `agent_start` 的结果形态（无返回）保存。
                            None => js_standard_handler_adapter(
                                callback,
                                ExtensionEventKind::AgentStart,
                            ),
                        }
                    };
                    Ok(entry.add_js_handler(&event, handler, Some(callback)))
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "off",
            Function::new(ctx.clone(), move |id: u64| -> EngineResult<()> {
                entry.remove_js_handler(id);
                Ok(())
            })?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerTool",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, tool: Value<'js>| -> EngineResult<()> {
                    // TS: `typeof tool.parameters !== "object" ...`。`tool` 为 undefined/null 时
                    // 读取 `.parameters` 本身就是 TypeError（文案带具体值）。
                    if tool.is_undefined() || tool.is_null() {
                        let what = if tool.is_null() { "null" } else { "undefined" };
                        return throw_message(
                            &ctx,
                            format!("Cannot read properties of {what} (reading 'parameters')"),
                        );
                    }
                    let object = tool.as_object();
                    // `typeof 5..parameters` 是 `"undefined"`（不抛错），所以非对象值走
                    // “must define an object parameter schema”分支；名字按 TS 模板字符串插值
                    // （缺失 → `"undefined"`）。
                    let name_value = match &object {
                        Some(object) => object.get::<_, Value<'js>>("name")?,
                        None => Value::new_undefined(ctx.clone()),
                    };
                    let name = js_template_value(&name_value);
                    let invalid = match &object {
                        Some(object) => {
                            let parameters = object.get::<_, Value<'js>>("parameters")?;
                            // TS：`typeof p !== "object" || p === null || Array.isArray(p)`。
                            // QuickJS 的 `is_object()` 对函数也为 true，因此函数必须单独排除
                            // （`typeof fn === "function"`）。
                            !parameters.is_object()
                                || parameters.is_function()
                                || parameters.is_null()
                                || parameters.is_array()
                        }
                        None => true,
                    };
                    if invalid {
                        return throw_message(
                            &ctx,
                            format!(
                                "Tool \"{name}\" registered by extension \"{}\" must define an object parameter schema.",
                                entry.path()
                            ),
                        );
                    }
                    let object = object.expect("valid parameters implies an object tool");
                    let mut definition = tool_definition_from_js(&ctx, object)?;
                    // TS 以 `tool.name` 的原始值作为 Map 键（缺失即 `undefined`）；Rust 的键是
                    // 字符串，采用模板字符串的字符串化结果。
                    definition.name = name;
                    if let Err(message) = entry.register_tool_checked(definition) {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerCommand",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, name: Value<'js>, options: Value<'js>| -> EngineResult<()> {
                    let name = name.as_string().map(|text| text.to_string().unwrap_or_default());
                    if name.as_deref().unwrap_or("").is_empty() {
                        return throw_message(
                            &ctx,
                            format!(
                                "Command registered by extension \"{}\" must have a non-empty string name. Use kiana.registerCommand(\"name\", {{ description, handler }}).",
                                entry.path()
                            ),
                        );
                    }
                    let name = name.unwrap_or_default();
                    let options = options.as_object();
                    let handler = options
                        .and_then(|options| get_function(options, "handler").ok().flatten());
                    let Some(handler) = handler else {
                        return throw_message(
                            &ctx,
                            format!(
                                "Command \"/{name}\" registered by extension \"{}\" must define handler().",
                                entry.path()
                            ),
                        );
                    };
                    let description = options
                        .and_then(|options| get_string(options, "description").ok().flatten());
                    let get_argument_completions = options
                        .and_then(|options| get_function(options, "getArgumentCompletions").ok().flatten())
                        .map(|function| {
                            let callback = JsCallback::new(&ctx, function);
                            Arc::new(move |prefix: String| {
                                let callback = callback;
                                Box::pin(async move {
                                    let value = callback
                                        .call_async(vec![JsArg::Json(JsonValue::String(prefix))])
                                        .await
                                        .ok()?;
                                    if value.is_null() {
                                        return None;
                                    }
                                    let values = value.as_array()?;
                                    Some(
                                        values
                                            .iter()
                                            .map(|item| kiana_tui::autocomplete::AutocompleteItem {
                                                value: item
                                                    .get("value")
                                                    .and_then(|value| value.as_str())
                                                    .unwrap_or_default()
                                                    .to_string(),
                                                label: item
                                                    .get("label")
                                                    .and_then(|value| value.as_str())
                                                    .unwrap_or_default()
                                                    .to_string(),
                                                description: item
                                                    .get("description")
                                                    .and_then(|value| value.as_str())
                                                    .map(str::to_string),
                                            })
                                            .collect(),
                                    )
                                }) as LocalBoxFuture<'static, Option<Vec<kiana_tui::autocomplete::AutocompleteItem>>>
                            }) as kiana_ext_api::types::ArgumentCompletionsFn
                        });
                    let receiver = match options {
                        Some(options) => Some(command_receiver(&ctx, &name, options)?),
                        None => None,
                    };
                    let callback = match receiver {
                        Some(receiver) => JsCallback::new_with_this(&ctx, handler, receiver.into_value()),
                        None => JsCallback::new(&ctx, handler),
                    };
                    let handler = JsCommandHandler {
                        callback,
                        // TS `_tryExecuteExtensionCommand` 的 `emitError` 用
                        // `extensionPath: "command:" + commandName`（agent-session.ts:2197），
                        // 不是扩展文件路径。
                        extension_path: format!("command:{name}"),
                    };
                    if let Err(message) = entry.register_command_checked(
                        &name,
                        description,
                        get_argument_completions,
                        handler.into_handler(),
                    ) {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerShortcut",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, shortcut: String, options: Value<'js>| -> EngineResult<()> {
                    let options_object = options.as_object();
                    let handler = options_object
                        .and_then(|options| get_function(options, "handler").ok().flatten());
                    let description = options_object
                        .and_then(|options| get_string(options, "description").ok().flatten());
                    let handler: ShortcutHandler = match handler {
                        Some(function) => JsShortcutHandler {
                            callback: JsCallback::new(&ctx, function),
                            extension_path: entry.path(),
                        }
                        .into_handler(),
                        // TS 原样保存 `options.handler`（缺失/非函数也保存）；调用时才抛
                        // `TypeError: handler is not a function`。
                        None => {
                            Arc::new(|_| Box::pin(async { panic!("handler is not a function") }))
                        }
                    };
                    if let Err(message) =
                        entry.register_shortcut_checked(&shortcut, description, handler)
                    {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerFlag",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, name: Value<'js>, options: Value<'js>| -> EngineResult<()> {
                    // TS 的第一行是 `options.default`（`options` 为 undefined/null 时是
                    // TypeError，文案带具体的 `undefined` / `null`）。
                    if options.is_undefined() || options.is_null() {
                        let what = if options.is_null() { "null" } else { "undefined" };
                        return throw_message(
                            &ctx,
                            format!("Cannot read properties of {what} (reading 'default')"),
                        );
                    }
                    // 其它原始值（数字/字符串/布尔）没有 `default`/`type` 属性，TS 照常注册。
                    let object = options.as_object();
                    let display_name = js_string_coerce(&name).unwrap_or_default();
                    let js_key = js_map_key(&name);
                    let description = match &object {
                        Some(object) => get_string(object, "description")?,
                        None => None,
                    };
                    // TS 的类型是 `"boolean" | "string"`，但运行期不校验：缺省/未知类型原样存进
                    // `extension.flags`（Rust 的 `FlagType` 无法表达 undefined/null，按 boolean 存）。
                    // 错误文案里的 `expected ${options.type}` 用 JS 的 `String()` 语义
                    // （`null` → `"null"`，缺省 → `"undefined"`）。
                    let type_value = match &object {
                        Some(object) => object.get::<_, Value<'js>>("type")?,
                        None => Value::new_undefined(ctx.clone()),
                    };
                    let flag_type_name = if type_value.is_undefined() {
                        "undefined".to_string()
                    } else {
                        js_string_coerce(&type_value).unwrap_or_else(|| "undefined".to_string())
                    };
                    let flag_type = match flag_type_name.as_str() {
                        "string" => FlagType::String,
                        _ => FlagType::Boolean,
                    };
                    // TS 用 `options.default !== undefined` 判断是否提供了默认值：
                    // `null` 也是“提供了”（typeof null === "object"），必须参与校验。
                    let default_value = match &object {
                        Some(object) => object.get::<_, Value<'js>>("default")?,
                        None => Value::new_undefined(ctx.clone()),
                    };
                    let default = if default_value.is_undefined() {
                        None
                    } else {
                        let actual = js_typeof(&default_value);
                        if actual != flag_type_name {
                            return throw_message(
                                &ctx,
                                format!(
                                    "Invalid default for flag \"{display_name}\": expected {flag_type_name}, got {actual}"
                                ),
                            );
                        }
                        match flag_type {
                            FlagType::Boolean => Some(FlagValue::Bool(
                                default_value.as_bool().unwrap_or(false),
                            )),
                            FlagType::String => Some(FlagValue::Str(
                                default_value
                                    .as_string()
                                    .map(|text| text.to_string().unwrap_or_default())
                                    .unwrap_or_default(),
                            )),
                        }
                    };
                    if let Err(message) = entry.register_flag_keyed(
                        &display_name,
                        &js_key,
                        description,
                        flag_type,
                        default,
                    ) {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "getFlag",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, name: Value<'js>| -> EngineResult<Value<'js>> {
                    let js_key = js_map_key(&name);
                    match entry.get_flag_keyed(&js_key) {
                        Ok(value) => flags_value_to_js(&ctx, value),
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerMessageRenderer",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      custom_type: String,
                      renderer: Value<'js>|
                      -> EngineResult<()> {
                    // TS 不校验函数类型：值原样保存，调用时才抛 TypeError。
                    let renderer: MessageRenderer = match renderer.as_function().cloned() {
                        Some(function) => js_message_renderer(&ctx, function),
                        None => Arc::new(|_, _, _| -> Option<Box<dyn kiana_tui::Component>> {
                            panic!("renderer is not a function")
                        }),
                    };
                    if let Err(message) =
                        entry.register_message_renderer_checked(&custom_type, renderer)
                    {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerMarkdownTransformer",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, transformer: Value<'js>| -> EngineResult<()> {
                    let transformer: MarkdownTransformer = match transformer.as_function().cloned()
                    {
                        Some(function) => js_markdown_transformer(&ctx, function),
                        None => {
                            Arc::new(|_, _| -> String { panic!("transformer is not a function") })
                        }
                    };
                    if let Err(message) = entry.register_markdown_transformer_checked(transformer) {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerEntryRenderer",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      custom_type: String,
                      renderer: Value<'js>|
                      -> EngineResult<()> {
                    let renderer: EntryRenderer = match renderer.as_function().cloned() {
                        Some(function) => js_entry_renderer(&ctx, function),
                        None => Arc::new(|_, _, _| -> Option<Box<dyn kiana_tui::Component>> {
                            panic!("renderer is not a function")
                        }),
                    };
                    if let Err(message) =
                        entry.register_entry_renderer_checked(&custom_type, renderer)
                    {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerToolRenderer",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, resolver: Value<'js>| -> EngineResult<()> {
                    let resolver: ToolRendererResolver = match resolver.as_function().cloned() {
                        Some(function) => js_tool_renderer_resolver(&ctx, function),
                        None => Arc::new(|_, _| -> Option<kiana_ext_api::ToolRenderers> {
                            panic!("resolver is not a function")
                        }),
                    };
                    if let Err(message) = entry.register_tool_renderer_checked(resolver) {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerProvider",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      provider_or_name: Value<'js>,
                      config: Value<'js>|
                      -> EngineResult<()> {
                    entry
                        .check_active()
                        .map_err(|message| Exception::throw_message(&ctx, &message))?;
                    match provider_or_name.as_string() {
                        Some(name) => {
                            let name = name.to_string()?;
                            if config.is_undefined() || config.is_null() {
                                return throw_message(
                                    &ctx,
                                    "Provider config is required when registering by name",
                                );
                            }
                            let (config, models_error) = provider_config_from_js(&ctx, config)?;
                            if let Some(message) = models_error {
                                // TS 的校验在 `bindCore` 冲刷时抛错：provider 不注册，扩展保持
                                // 已加载。这里记下错误供 `bindCore` 上报（见
                                // `runtime_provider_config_errors`）。
                                if let Err(message) =
                                    entry.defer_provider_config_error(&name, message)
                                {
                                    return throw_message(&ctx, message);
                                }
                                return Ok(());
                            }
                            if let Err(message) =
                                entry.register_provider_checked(ProviderRegistration::Config {
                                    name,
                                    config,
                                })
                            {
                                return throw_message(&ctx, message);
                            }
                        }
                        None => {
                            // TS：`applyRuntimeChange(() => runtime.registerNativeProvider(provider, extension.path))`。
                            // 原生对象物化成 `Arc<dyn Provider>`（见 [`JsNativeProvider`]）。
                            let provider = native_provider_from_js(&ctx, provider_or_name)?;
                            if let Err(message) = entry
                                .register_provider_checked(ProviderRegistration::Native(provider))
                            {
                                return throw_message(&ctx, message);
                            }
                        }
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "unregisterProvider",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, name: String| -> EngineResult<()> {
                    match entry.unregister_provider_checked(&name) {
                        Ok(()) => Ok(()),
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerMcpServer",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, name: String, config: Value<'js>| -> EngineResult<()> {
                    let config = value_to_json(&ctx, config)?;
                    if let Err(message) = entry.register_mcp_server_raw(&name, &config) {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "unregisterMcpServer",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, name: String| -> EngineResult<()> {
                    match entry.unregister_mcp_server_checked(&name) {
                        Ok(()) => Ok(()),
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "getMcpServers",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>| -> EngineResult<Value<'js>> {
                    match entry.get_mcp_servers_checked() {
                        Ok(servers) => {
                            let json = serde_json::to_value(servers).unwrap_or(JsonValue::Null);
                            json_value(&ctx, &json)
                        }
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "registerVirtualModel",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, model: Value<'js>| -> EngineResult<()> {
                    let model = virtual_model_from_js(&ctx, model)?;
                    if let Err(message) = entry.register_virtual_model_checked(model) {
                        return throw_message(&ctx, message);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "unregisterVirtualModel",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, provider: String, id: String| -> EngineResult<()> {
                    match entry.unregister_virtual_model_checked(&provider, &id) {
                        Ok(()) => Ok(()),
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "action",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, name: String, args: Value<'js>| -> EngineResult<Value<'js>> {
                    // `getAllTools` 的每一项带 `reachesTools` 函数，只能就地物化。
                    if name == "getAllTools" {
                        return match get_all_tools_value(&entry, &ctx) {
                            Ok(value) => Ok(value),
                            Err(message) => throw_message(&ctx, message),
                        };
                    }
                    let args = value_to_json(&ctx, args)?;
                    match dispatch_action(&entry, &name, args) {
                        Ok(value) => json_value(&ctx, &value),
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "isBound",
            Function::new(ctx.clone(), move |name: String| -> bool {
                let _ = name;
                lock(&entry.shared.actions).is_some()
            })?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "setModel",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, model: Value<'js>| -> EngineResult<Value<'js>> {
                    if let Err(message) = entry.check_active() {
                        return throw_message(&ctx, message);
                    }
                    let model = value_to_json(&ctx, model)?;
                    let kiana: Object<'_> = ctx.globals().get("__kiana")?;
                    let promise: Function<'_> = kiana.get("promise")?;
                    let payload =
                        json_value(&ctx, &json!({ "entry_id": entry_id, "model": model }))?;
                    promise.call(("loader.setModel", payload))
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "exec",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      command: String,
                      args: Value<'js>,
                      options: Value<'js>|
                      -> EngineResult<Value<'js>> {
                    if let Err(message) = entry.check_active() {
                        return throw_message(&ctx, message);
                    }
                    let args_json = value_to_json(&ctx, args)?;
                    let mut arguments = Vec::new();
                    if let JsonValue::Array(values) = args_json {
                        for value in values {
                            match value.as_str() {
                                Some(value) => arguments.push(value.to_string()),
                                None => {
                                    return throw_message(&ctx, "exec args must be strings");
                                }
                            }
                        }
                    }
                    // TS 的 `execCommand` 是异步的：返回 Promise，事件循环继续运行。Rust 的
                    // `exec_command` 是同步的，因此把实际执行放进 `spawn_blocking`，JS 侧拿到
                    // 一个 `__kiana.promise`（不再冻结引擎/LocalSet）。
                    let options_object = options.as_object().cloned();
                    let cwd = options_object
                        .as_ref()
                        .and_then(|object| object.get::<_, Option<String>>("cwd").ok().flatten())
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::path::PathBuf::from(&entry.cwd));
                    // TS `options.timeout` 是毫秒（可以是小数）；`setTimeout` 对 0.5ms 也生效。
                    let timeout = options_object
                        .as_ref()
                        .and_then(|object| object.get::<_, Option<f64>>("timeout").ok().flatten())
                        .filter(|timeout| timeout.is_finite() && *timeout > 0.0)
                        .map(|timeout| (timeout.ceil() as u64).max(1));
                    let signal = match options_object.as_ref().and_then(|object| {
                        object.get::<_, Option<Value<'js>>>("signal").ok().flatten()
                    }) {
                        Some(value) => cancellation_token_from_js(&ctx, Some(&value))?,
                        None => None,
                    };
                    let token_id = store_exec_token(signal.clone());
                    let payload = json!({
                        "command": command,
                        "args": arguments,
                        "cwd": cwd.to_string_lossy(),
                        "timeout": timeout,
                        "token": token_id,
                    });
                    call_kiana_promise(&ctx, "loader.exec", payload)
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "eventsEmit",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, channel: String, data: Value<'js>| -> EngineResult<()> {
                    // Rust handler 仍走 JSON 总线（`BigInt` 等不可序列化值按 `null` 传递给它们；
                    // JS handler 拿到的始终是下面这个活对象）。
                    let json = match value_to_json(&ctx, data.clone()) {
                        Ok(json) => json,
                        Err(_) => {
                            // `BigInt` 等不可 JSON 化的值：Rust handler 收到 null，但 JS handler
                            // 仍拿活对象；清掉挂起的异常以免污染后续 JS 调用。
                            let _ = ctx.catch();
                            JsonValue::Null
                        }
                    };
                    let result = {
                        let _guard = SyncCtxGuard::enter(&ctx);
                        let _live = LiveEventGuard::enter(&channel, &data);
                        entry.events_emit_checked(&channel, json)
                    };
                    match result {
                        Ok(()) => Ok(()),
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "eventsOn",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, channel: String, handler: Value<'js>| -> EngineResult<u64> {
                    // TS 的 `events.on` 同样不做类型检查；非函数在派发时抛 TypeError。
                    let Some(function) = handler.as_function().cloned() else {
                        let channel_name = channel.clone();
                        let handler: EventHandlerFn = Arc::new(move |_: JsonValue| {
                            tracing::error!(
                                "Event handler error ({}): {}",
                                channel_name,
                                "handler is not a function"
                            );
                        });
                        return match entry.register_event_subscription(&channel, handler, None) {
                            Ok(id) => Ok(id),
                            Err(message) => throw_message(&ctx, message),
                        };
                    };
                    let callback = JsCallback::new(&ctx, function);
                    let channel_name = channel.clone();
                    let handler: EventHandlerFn = Arc::new(move |data: JsonValue| {
                        let callback = callback;
                        if let Err(error) =
                            call_event_handler(&callback, &channel_name, vec![JsArg::Json(data)])
                        {
                            tracing::error!(
                                "Event handler error ({}): {}",
                                channel_name,
                                error.message
                            );
                        }
                    });
                    match entry.register_event_subscription(&channel, handler, Some(callback)) {
                        Ok(id) => Ok(id),
                        Err(message) => throw_message(&ctx, message),
                    }
                },
            )?,
        )?;
    }

    {
        host.set(
            "eventsOff",
            Function::new(ctx.clone(), move |id: u64| {
                JS_SUBSCRIPTIONS.with(|subscriptions| {
                    if let Some((unsubscribe, callback)) = subscriptions.borrow_mut().remove(&id) {
                        unsubscribe();
                        if let Some(callback) = callback {
                            callback.release();
                        }
                    }
                });
            })?,
        )?;
    }

    {
        let entry = Rc::clone(&entry);
        host.set(
            "commit",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> EngineResult<()> {
                match entry.commit() {
                    Ok(()) => Ok(()),
                    Err(message) => throw_message(&ctx, message),
                }
            })?,
        )?;
    }
    {
        let entry = Rc::clone(&entry);
        host.set(
            "discard",
            Function::new(ctx.clone(), move || {
                entry.discard();
            })?,
        )?;
    }

    API_ENTRIES.with(|entries| {
        entries.borrow_mut().insert(entry_id, Rc::downgrade(&entry));
    });

    Ok(host)
}

/// `setModel` 的宿主调用 payload（`__kiana.promise("loader.setModel", ...)`）。
#[derive(serde::Deserialize)]
struct SetModelPayload {
    entry_id: u64,
    model: JsonValue,
}

/// `setModel` 的 host kind 处理器：把绑定后的动作 future 投给 `LocalSet`，完成后 settle。
fn handle_set_model(call: HostCall<'_>) -> EngineResult<()> {
    let payload: SetModelPayload = call.payload_json()?;
    let id = call.id();
    let sender = call.completion_sender();
    let Some(entry) = entry_for_id(payload.entry_id) else {
        sender.reject_message(id, NOT_INITIALIZED_MESSAGE);
        return Ok(());
    };
    let Some(actions) = lock(&entry.shared.actions).clone() else {
        sender.reject_message(id, SET_MODEL_NOT_INITIALIZED_MESSAGE);
        return Ok(());
    };
    let model: Model = match serde_json::from_value(payload.model) {
        Ok(model) => model,
        Err(error) => {
            sender.reject_message(id, format!("invalid model: {error}"));
            return Ok(());
        }
    };
    let future = (actions.set_model)(model);
    tokio::task::spawn_local(async move {
        let result = future.await;
        sender.resolve(id, result);
    });
    Ok(())
}

// ===========================================================================
// 安装（`js/api.js` + 模块 loader 接线）
// ===========================================================================

/// `js/api.js` 源。
const API_JS: &str = include_str!("../../js/api.js");

/// 安装扩展加载器与 `js/api.js`（幂等）。
///
/// 顺序（`spec/design-extension-runtime.md` §9.2）：`install_intl` → `engine::install`
/// （prelude）→ `js_host::install`（Node 全局）→ 本函数 → `virtual_modules::install`。
/// 本函数把 [`ExtensionLoader`] 装到引擎的 runtime 上（`Runtime::set_loader` 只有一份），
/// 并把 `js/api.js` 求值进全局；`virtual_modules::install` 的导入环需要这个 loader 已就位，
/// 因此本函数**先于**它调用（虚拟模块的源在这里登记，`virtual_modules::install` 会重复登记，
/// 幂等）。
///
/// 参数是 `Rc<JsEngine>`：回调闭包只保存 `u64` id，真正的 `Persistent` 寄存在引擎的持久槽里；
/// 引擎地址必须稳定（`Rc` 在堆上），否则线程本地的登记会悬垂。调用方在**本代存活期间**持有
/// 同一个 `Rc`（当前由 `crate::Generation` 在装配时建立；登记本身是 `Weak`）；`/reload`
/// 销毁旧代后引擎随之释放。
///
/// 具体安装内容：虚拟模块源码登记、`ExtensionLoader`（模块解析 + TS 转译）、`js/api.js`
/// 求值，以及四个 host kind（`loader.setModel` / `loader.exec` / `loader.projectTrustUi` /
/// `loader.oauth`）。
///
/// 必须在 `with_js` 之外调用。同一引擎重复调用是 no-op（只把它恢复为当前引擎）；新引擎
/// 调用会登记为当前引擎。线程上已有存活引擎时（同一线程的第二个宿主），不清空全局扩展
/// 登记——那些条目属于仍在运行的引擎（TS 的多个 `ResourceLoader` 互不影响）。只有登记的
/// 存活引擎为空时（装配第一代 / 上一代已全部销毁）才清理残留（旧一代的回调随其 `Drop`
/// 清理，调用方应先 `invalidate` 并按 §2.8 销毁旧代）。
pub fn install(engine: Rc<JsEngine>) -> EngineResult<()> {
    let replacing_last = current_engine().is_none();
    register_current_engine(&engine);
    if is_installed(&engine) {
        return Ok(());
    }
    // 只在「登记里已无存活引擎」时清表：同线程还有另一个宿主时，这些条目属于它，
    // 清掉会破坏仍在运行的扩展（见上方文档）。
    if replacing_last {
        JS_SUBSCRIPTIONS.with(|subscriptions| subscriptions.borrow_mut().clear());
        API_ENTRIES.with(|entries| entries.borrow_mut().clear());
        RENDERER_REGISTRY.with(|registry| registry.borrow_mut().clear());
        PROJECT_TRUST_UI_HANDLERS.with(|handlers| handlers.borrow_mut().clear());
        OAUTH_UI_HANDLERS.with(|handlers| handlers.borrow_mut().clear());
        EXEC_TOKENS.with(|tokens| tokens.borrow_mut().clear());
        DIALOG_TOKENS.with(|tokens| tokens.borrow_mut().clear());
    }

    // 虚拟模块的源先登记到引擎（幂等；`virtual_modules::install` 也会做）。
    crate::virtual_modules::register_module_sources(&engine)?;
    let virtual_fs = crate::virtual_modules::virtual_fs();
    let loader = ExtensionLoader::new(LoaderConfig::new(virtual_fs));
    for (name, source) in engine.module_sources() {
        loader.register_source(name, source);
    }
    loader.install_on(
        &engine,
        ModuleResolver::new(ResolverOptions {
            extension_dir: None,
            virtual_fs,
        }),
    );
    engine.with_js(|ctx| -> EngineResult<()> {
        loader.install(&ctx)?;
        Ok(())
    })?;
    engine.eval_script::<()>(API_JS, "<api.js>")?;
    engine.register_host_kind("loader.setModel", handle_set_model);
    engine.register_host_kind("loader.exec", handle_exec);
    engine.register_host_kind("loader.projectTrustUi", handle_project_trust_ui);
    engine.register_host_kind("loader.oauth", handle_oauth_ui);
    CURRENT_LOADERS.with(|cell| {
        let mut loaders = cell.borrow_mut();
        loaders.retain(|(weak, _)| {
            weak.upgrade()
                .is_some_and(|candidate| !Rc::ptr_eq(&candidate, &engine))
        });
        loaders.push((Rc::downgrade(&engine), loader));
    });
    Ok(())
}

/// `js/api.js` 是否已安装（`globalThis.__kiana_api` 存在）。
pub fn is_installed(engine: &JsEngine) -> bool {
    engine.with_js(|ctx| {
        matches!(
            ctx.globals().get::<_, Option<Object>>("__kiana_api"),
            Ok(Some(_))
        )
    })
}

fn current_loader() -> Option<ExtensionLoader> {
    let engine = current_engine()?;
    CURRENT_LOADERS.with(|cell| {
        cell.borrow()
            .iter()
            .rev()
            .find(|(weak, _)| {
                weak.upgrade()
                    .is_some_and(|candidate| Rc::ptr_eq(&candidate, &engine))
            })
            .map(|(_, loader)| loader.clone())
    })
}

/// 把 `rquickjs::Error` 物化成 TS 的 `error.message` 文本。
fn js_error_message(engine: &JsEngine, error: rquickjs::Error) -> String {
    if matches!(error, rquickjs::Error::Exception) {
        engine
            .with_js(|ctx| take_exception(&ctx, JsErrorKind::Host))
            .message
    } else {
        error.to_string()
    }
}

/// 缓存里的 JS 工厂：模块体已执行过，只剩默认导出函数。
struct CachedJsFactory {
    callback: JsCallback,
}

impl Drop for CachedJsFactory {
    fn drop(&mut self) {
        // `clear_extension_cache()` / cwd 变化 / 进程退出都会丢弃缓存项；TS 靠 GC 回收函数，
        // Rust 必须在引擎持久槽里显式移除（否则每次 reload 泄漏一个 `Persistent`）。
        self.callback.release();
    }
}

// ===========================================================================
// 模块导入与工厂调用
// ===========================================================================

/// `loadExtensionModule` 的 Rust 形态：求值入口模块并取默认导出函数。
///
/// 缓存命中时**不重新执行模块体**（复用缓存的函数）；模块没有有效默认导出时
/// 返回 `None` 且不写缓存。
struct LoadedFactory {
    callback: JsCallback,
    /// 工厂来自缓存（持久槽由 `CachedJsFactory` 持有，不能在这里释放）。
    cached: bool,
}

/// 导入扩展入口模块并取默认导出函数；缓存命中时不重新执行模块体。
///
/// `Ok(None)` 表示模块已求值但没有函数形式的默认导出（此时不写缓存）；`Err` 是导入 /
/// 求值失败的文本（调用方会加 `Failed to load extension: ` 前缀）。
async fn load_extension_module(
    engine: &JsEngine,
    resolved_path: &str,
    cache_token: Option<&ExtensionCacheToken>,
) -> Result<Option<LoadedFactory>, String> {
    // 缓存命中：直接复用模块的默认导出函数，既不重新执行模块体，也不新增持久槽。
    if let Some(cached) = factory_cache::get_cached::<CachedJsFactory>(cache_token, resolved_path) {
        return Ok(Some(LoadedFactory {
            callback: cached.callback,
            cached: true,
        }));
    }

    let Some(loader) = current_loader() else {
        return Err("extension loader is not installed".to_string());
    };

    // 导入阶段：`loader.load_module` 只声明模块（解析 / 转译 / 登记依赖），`module.eval()`
    // 才执行模块体；两者在同一个 `with_js` 事务里，随后跨 `await` 等求值 Promise。
    let evaluated = engine.with_js(
        |ctx| -> EngineResult<(Persistent<Promise<'static>>, Persistent<Object<'static>>)> {
            let name = engine.scoped_module_name(resolved_path);
            let module = loader.load_module(&ctx, &name)?;
            let (module, promise) = module.eval()?;
            let namespace = module.namespace()?;
            Ok((
                Persistent::save(&ctx, promise),
                Persistent::save(&ctx, namespace),
            ))
        },
    );
    let (promise, namespace) = match evaluated {
        Ok(evaluated) => evaluated,
        Err(error) => return Err(js_error_message(engine, error)),
    };
    if let Err(error) = await_js_promise(engine, promise).await {
        return Err(error.message);
    }

    let callback = engine.with_js(|ctx| -> EngineResult<Option<JsCallback>> {
        let namespace = namespace.restore(&ctx).map_err(|error| {
            rquickjs::Error::new_from_js_message("module", "object", error.to_string())
        })?;
        let default: Value<'_> = namespace.get("default")?;
        let Some(function) = default.as_function().cloned() else {
            return Ok(None);
        };
        Ok(Some(JsCallback::new(&ctx, function)))
    });
    let callback = match callback {
        Ok(callback) => callback,
        Err(error) => return Err(js_error_message(engine, error)),
    };
    let mut stored_in_cache = false;
    if let Some(callback) = callback {
        // 只有 token 仍有效时才构造缓存项：`CachedJsFactory::drop` 会释放持久槽，临时构造
        // 后立刻丢弃会把刚登记的回调删掉。
        if factory_cache::is_current_cache_token(cache_token) {
            factory_cache::put_cached(
                cache_token,
                resolved_path,
                Rc::new(CachedJsFactory { callback }),
            );
            stored_in_cache = true;
        }
    }
    Ok(callback.map(|callback| LoadedFactory {
        callback,
        // 缓存持有同一个持久槽；本次加载结束后不能释放。
        cached: stored_in_cache,
    }))
}

/// 用后释放未缓存的工厂回调（TS 靠 GC；Rust 的持久槽必须显式移除）。
struct FactoryRelease {
    callback: JsCallback,
}

impl Drop for FactoryRelease {
    fn drop(&mut self) {
        self.callback.release();
    }
}

/// 执行一个 JS 工厂：建 `kiana` 对象、调用工厂、等待 Promise、commit / discard。
#[allow(clippy::too_many_arguments)]
async fn initialize_js_extension(
    engine: &JsEngine,
    factory: JsCallback,
    factory_cached: bool,
    extension_path: &str,
    resolved_path: &str,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: Arc<ExtensionRuntime>,
) -> Result<Extension, String> {
    // 阶段：建 ApiEntry（loading）→ 在工厂 deadline 内建 host / `kiana` 对象并调用工厂 →
    // 把返回值统一包成 Promise 等待 → `commit()`（失败走 `discard()`）→ 登记 live 表。
    let extension = create_extension(extension_path, resolved_path);
    let shared = shared_of(&runtime);
    let entry_id = next_entry_id();
    let entry = ApiEntry::new(
        extension,
        Arc::clone(&runtime),
        entry_id,
        shared,
        event_bus,
        cwd.to_string(),
    );
    // 未缓存的工厂在本次调用后不再需要；函数已在下面 `with_function` 里同步调用完成。
    // 注意 `then_some` 会**先**构造 `FactoryRelease`（随后立即 Drop 并释放回调）；
    // 必须用惰性的 `then`。
    let _release_factory = (!factory_cached).then(|| FactoryRelease { callback: factory });

    // 工厂加载的同步段使用 10 s 的 FACTORY_DEADLINE（设计文档 §2.7），而不是普通
    // `with_js` 的 100 ms 看门狗。
    let call = engine.with_js_factory_deadline(
        |ctx| -> EngineResult<Option<Persistent<Promise<'static>>>> {
            let host = build_host_object(&ctx, Rc::clone(&entry), entry_id)?;
            let api_object: Object<'_> = ctx.globals().get("__kiana_api")?;
            let make: Function<'_> = api_object.get("makeApi")?;
            let api: Value<'_> = make.call((host, extension_path))?;
            let value = factory
                .with_function(&ctx, |_ctx, function| function.call::<_, Value<'_>>((api,)))
                .map_err(|error| Exception::throw_message(&ctx, &error.message))??;
            // TS 的 `await factory(load.api)` 会 assimilate **任意 thenable**。只检查
            // `as_promise()` 会把 `{ then(resolve, reject) {...} }` 当同步成功；这里统一走
            // `Promise.resolve(value)`（普通值也在下一个 microtask 完成，与 `await` 一致）。
            let promise_object: Object<'_> = ctx.globals().get("Promise")?;
            let resolve: Function<'_> = promise_object.get("resolve")?;
            // `Promise.resolve` 需要 `this` 是构造函数（裸调用会抛 TypeError）。
            let promise: Promise<'_> =
                resolve.call((rquickjs::function::This(promise_object), value))?;
            Ok(Some(Persistent::save(&ctx, promise)))
        },
    );

    let promise = match call {
        Ok(promise) => promise,
        Err(error) => {
            let message = js_error_message(engine, error);
            entry.discard();
            return Err(message);
        }
    };
    if let Some(promise) = promise
        && let Err(error) = await_js_promise(engine, promise).await
    {
        entry.discard();
        return Err(error.message);
    }
    if let Err(message) = entry.commit() {
        entry.discard();
        return Err(message);
    }
    register_live_extension(&entry);
    time(
        &format!("{extension_path} factory"),
        TimingLabel::Extensions,
    );
    Ok(entry.extension.borrow().clone())
}

/// panic payload → 文本（`&str` / `String` / 其它）。
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// 执行一个 Rust 工厂（`ExtensionFactory`）：panic 视为 TS 的 throw。
async fn initialize_rust_extension(
    factory: kiana_ext_api::types::ExtensionFactory,
    extension_path: &str,
    resolved_path: &str,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: Arc<ExtensionRuntime>,
) -> Result<Extension, String> {
    use futures::FutureExt;

    // 阶段与 JS 版相同（建 entry → 调工厂 → commit/discard → 登记 live）；工厂是 Rust
    // 闭包，下面的两层 catch_unwind 分别对应「建 future 时 panic」与「future 轮询时 panic」。
    let extension = create_extension(extension_path, resolved_path);
    let shared = shared_of(&runtime);
    let entry_id = next_entry_id();
    let entry = ApiEntry::new(
        extension,
        Arc::clone(&runtime),
        entry_id,
        shared,
        event_bus,
        cwd.to_string(),
    );
    // `ExtensionFactory` 的冻结签名要求 `Arc<dyn ExtensionApi>`；该 trait 无 `Send`/`Sync` 超 trait
    //（`kiana-ext-api` 的文档：实现跑在单线程 LocalSet 上）。
    #[allow(clippy::arc_with_non_send_sync)]
    let api: Arc<dyn ExtensionApi> = Arc::new(LoaderExtensionApi::new(Rc::clone(&entry), entry_id));
    // TS 的 `await factory(load.api)` 把**同步 throw** 和 future 的 rejection 放进同一个
    // try/catch；Rust 的工厂可能在返回 future 之前 panic，因此两层都要兜住。
    let future = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory(api))) {
        Ok(future) => future,
        Err(payload) => {
            entry.discard();
            return Err(panic_payload_message(payload.as_ref()));
        }
    };
    let outcome = std::panic::AssertUnwindSafe(future).catch_unwind().await;
    if let Err(payload) = outcome {
        entry.discard();
        return Err(panic_payload_message(payload.as_ref()));
    }
    if let Err(message) = entry.commit() {
        entry.discard();
        return Err(message);
    }
    register_live_extension(&entry);
    time(
        &format!("{extension_path} factory"),
        TimingLabel::Extensions,
    );
    Ok(entry.extension.borrow().clone())
}

/// 单条路径的加载结果（与 TS 的 `{ extension, error }` 对应）。
struct SingleLoad {
    extension: Option<Extension>,
    error: Option<String>,
}

async fn load_extension(
    engine: &JsEngine,
    extension_path: &str,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: Arc<ExtensionRuntime>,
    cache_token: Option<&ExtensionCacheToken>,
) -> SingleLoad {
    // 单条路径的完整流水线：解析路径 → 模块导入（缓存命中时跳过模块体）→ 执行工厂。
    // 失败折算成一条 `SingleLoad { extension: None, error }`：导入 / 工厂错误加
    // `Failed to load extension: ` 前缀，缺默认导出用固定的 `Extension does not export …`。
    let resolved_path = resolve_path(
        extension_path,
        cwd,
        &PathInputOptions {
            normalize_unicode_spaces: true,
            ..PathInputOptions::default()
        },
    );

    let factory = match load_extension_module(engine, &resolved_path, cache_token).await {
        Ok(factory) => factory,
        Err(message) => {
            return SingleLoad {
                extension: None,
                error: Some(failed_load_message(&message)),
            };
        }
    };
    time(
        &format!("{extension_path} module import"),
        TimingLabel::Extensions,
    );

    let Some(factory) = factory else {
        return SingleLoad {
            extension: None,
            error: Some(invalid_factory_message(extension_path)),
        };
    };

    match initialize_js_extension(
        engine,
        factory.callback,
        factory.cached,
        extension_path,
        &resolved_path,
        cwd,
        event_bus,
        runtime,
    )
    .await
    {
        Ok(extension) => SingleLoad {
            extension: Some(extension),
            error: None,
        },
        Err(message) => SingleLoad {
            extension: None,
            error: Some(failed_load_message(&message)),
        },
    }
}

// ===========================================================================
// 批量加载
// ===========================================================================

async fn load_extensions_internal(
    paths: &[String],
    cwd: &str,
    event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<Arc<ExtensionRuntime>>,
    use_cache: bool,
) -> LoadExtensionsResult {
    // 确保本线程存在 LocalSet 驱动（扩展里的 `spawn_local` 需要它）；返回值在此不使用。
    let _ = crate::runner::ensure_extension_local();
    // 缺省的事件总线 / runtime 各建一个：调用方可以传入共享实例（`kiana-resources` 传入
    // 它自己的 event bus），测试通常传 `None`。
    let event_bus = event_bus.unwrap_or_else(|| create_event_bus());
    let runtime = runtime.unwrap_or_else(create_extension_runtime);
    // 启用缓存时 `use_extension_cache_cwd` 返回 `{ cwd, generation }`：cwd 变化会清空缓存
    // 条目并把 generation +1（旧 token 失效）。本函数用 `token.cwd` 作为路径解析基准，
    // 使缓存作用域与实际加载使用的 cwd 一致。
    let cache_token = use_cache.then(|| use_extension_cache_cwd(cwd));
    let resolved_cwd = cache_token
        .as_ref()
        .map(|token| token.cwd.clone())
        .unwrap_or_else(|| resolve_path_resolved(cwd));

    let mut extensions: Vec<Extension> = Vec::new();
    let mut errors: Vec<LoadError> = Vec::new();

    // 串行加载：每个路径单独查询当前引擎（线程本地登记在两次迭代之间可能变化）；查不到
    // 时该路径记为错误而不是 panic。`extensions` 的顺序即输入顺序。
    for extension_path in paths {
        let engine = match current_engine() {
            Some(engine) => engine,
            None => {
                errors.push(LoadError {
                    path: extension_path.clone(),
                    error: failed_load_message("Extension runtime engine is not installed"),
                });
                continue;
            }
        };
        let loaded = load_extension(
            &engine,
            extension_path,
            &resolved_cwd,
            Arc::clone(&event_bus),
            Arc::clone(&runtime),
            cache_token.as_ref(),
        )
        .await;
        match (loaded.extension, loaded.error) {
            (Some(extension), _) => extensions.push(extension),
            (None, Some(error)) => errors.push(LoadError {
                path: extension_path.clone(),
                error,
            }),
            (None, None) => {}
        }
    }

    LoadExtensionsResult {
        extensions,
        errors,
        // TS 的 `warnings` 恒为空数组（`loadExtensionsInternal` 的返回值）。
        warnings: Some(Vec::<LoadWarning>::new()),
        // 加载结束时的 runtime 快照：`shared_of` 找回同一份共享状态，快照里的闭包仍读写
        // 实时值（见模块文档「为什么字段是快照」）。
        runtime: build_runtime(&shared_of(&runtime)),
    }
}

/// 加载显式路径（不做发现、不用工厂缓存）。
///
/// TS: `loader.ts#loadExtensions`。串行加载；顺序即结果顺序与 handler 顺序。
///
/// - `paths`：扩展入口路径；相对路径按 `cwd` 解析，空列表返回空结果；
/// - `cwd`：解析基准；内部先解析成绝对路径（启用缓存时以缓存 token 的 `cwd` 为准）；
/// - `event_bus`：`None` 时新建（扩展的 `kiana.events` 订阅挂在这个总线上）；
/// - `runtime`：`None` 时新建；本函数**不绑定**动作（`bindCore` 属于 runner）。
///
/// 每条路径独立成败：失败进 `errors` 并继续加载后续路径，不会提前返回。
pub async fn load_extensions(
    paths: &[String],
    cwd: &str,
    event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<Arc<ExtensionRuntime>>,
) -> LoadExtensionsResult {
    load_extensions_internal(paths, cwd, event_bus, runtime, false).await
}

/// 同 [`load_extensions`]，但启用工厂缓存（命中时跳过模块体、仍重跑工厂）。
///
/// 缓存是线程本地的，作用域是 cwd：`factory_cache::use_extension_cache_cwd` 在 cwd 变化时
/// 清空条目并让旧 token 失效；因此同一线程、同一 cwd 下的重复批量加载复用模块求值结果，
/// 换 cwd 后重新导入。
///
/// TS: `loader.ts#loadExtensionsCached`。
pub async fn load_extensions_cached(
    paths: &[String],
    cwd: &str,
    event_bus: Option<Arc<dyn EventBus>>,
    runtime: Option<Arc<ExtensionRuntime>>,
) -> LoadExtensionsResult {
    load_extensions_internal(paths, cwd, event_bus, runtime, true).await
}

/// 从内联工厂函数建一个 `Extension`。
///
/// `extensionPath` 缺省 `"<inline>"`；`resolvedPath` 就是 `extensionPath`
/// （`<inline>` / `<inline:name>` / `builtin:name` 都不是文件路径）。`cwd` 先 `resolvePath`。
///
/// TS: `loader.ts#loadExtensionFromFactory`。
///
/// 返回 `Err(String)` 是失败原因原文（工厂 panic / `commit` 失败），**不带**
/// `Failed to load extension: ` 前缀——与 [`load_extensions`] 的错误聚合不同，
/// 调用方（如 `kiana-resources` 加载 `builtin:<name>`）把它直接作为 `LoadError.error`。
pub async fn load_extension_from_factory(
    factory: kiana_ext_api::types::ExtensionFactory,
    cwd: &str,
    event_bus: Arc<dyn EventBus>,
    runtime: Arc<ExtensionRuntime>,
    extension_path: Option<&str>,
) -> Result<Extension, String> {
    let extension_path = extension_path.unwrap_or("<inline>");
    let resolved_cwd = resolve_path_resolved(cwd);
    initialize_rust_extension(
        factory,
        extension_path,
        extension_path,
        &resolved_cwd,
        event_bus,
        runtime,
    )
    .await
}

/// 发现并加载标准位置的扩展（项目本地 → 全局 → 显式配置路径），不使用工厂缓存。
///
/// TS: `loader.ts#discoverAndLoadExtensions`。
///
/// - 发现顺序：`<cwd>/.kiana/extensions` → `<agentDir>/extensions` → `configured_paths`；
///   同一入口只加载一次（按 `resolve_path_resolved` 后的绝对路径去重，保留首次出现的顺序）。
/// - `agent_dir` 为 `None` 时用 `get_agent_dir()`；`event_bus` 为 `None` 时新建。
/// - 配置路径是目录时，优先取 `package.json` 的 `extensions` 声明 / `index.ts` / `index.js`，
///   都没有才扫描目录一层；不存在或不是目录的路径原样加入，由加载阶段报错。
/// - 内部调用 [`load_extensions`]，因此同样**不使用**工厂缓存。
pub async fn discover_and_load_extensions(
    configured_paths: &[String],
    cwd: &str,
    agent_dir: Option<&str>,
    event_bus: Option<Arc<dyn EventBus>>,
) -> LoadExtensionsResult {
    let resolved_cwd = resolve_path_resolved(cwd);
    let resolved_agent_dir = match agent_dir {
        Some(agent_dir) => resolve_path_resolved(agent_dir),
        None => resolve_path_resolved(&get_agent_dir().to_string_lossy()),
    };

    let mut all_paths: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 去重键是解析后的绝对路径：同一入口经多个来源只出现一次，`all_paths` 保留首次出现的
    // 顺序（三个来源的加载顺序因此确定）。加入的是原始 `path`，加载阶段会再解析一次。
    let mut add_paths = |paths: Vec<String>| {
        for path in paths {
            let resolved = resolve_path_resolved(&path);
            if seen.insert(resolved) {
                all_paths.push(path);
            }
        }
    };

    // 1. 项目本地：<cwd>/.kiana/extensions
    let local_extension_dir = std::path::Path::new(&resolved_cwd)
        .join(CONFIG_DIR_NAME)
        .join("extensions");
    add_paths(discover_extensions_in_dir(&local_extension_dir));

    // 2. 全局：<agentDir>/extensions
    let global_extension_dir = std::path::Path::new(&resolved_agent_dir).join("extensions");
    add_paths(discover_extensions_in_dir(&global_extension_dir));

    // 3. 显式配置路径
    for configured in configured_paths {
        let resolved = resolve_path(
            configured,
            &resolved_cwd,
            &PathInputOptions {
                normalize_unicode_spaces: true,
                ..PathInputOptions::default()
            },
        );
        if std::path::Path::new(&resolved).exists() && std::path::Path::new(&resolved).is_dir() {
            // 目录：先看 `package.json` 的 `extensions` 声明 / `index.ts` / `index.js`
            // （`resolve_extension_entries`），都没有才扫描一层 `.ts` / `.js` 入口。
            if let Some(entries) = resolve_extension_entries(std::path::Path::new(&resolved)) {
                add_paths(entries);
                continue;
            }
            add_paths(discover_extensions_in_dir(std::path::Path::new(&resolved)));
            continue;
        }
        // 文件或不存在的路径：原样加入，由 `load_extension` 在导入阶段报错。
        add_paths(vec![resolved]);
    }

    load_extensions(&all_paths, &resolved_cwd, event_bus, None).await
}

#[cfg(all(test, feature = "runtime"))]
mod regression_tests {
    //! 无法从集成测试构造 trait 对象上下文的桥接单测（`JsToolExecute` 的 signal / onUpdate）。

    use super::*;
    use crate::engine::{EngineResult, JsEngine};

    fn test_model() -> kiana_ai::types::Model {
        serde_json::from_value(json!({
            "id": "m",
            "name": "M",
            "api": "anthropic-messages",
            "provider": "p",
            "baseUrl": "https://example.test",
            "input": ["text"],
            "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0 },
        }))
        .expect("model")
    }

    #[test]
    fn context_handler_in_place_mutation_is_read_back() {
        let engine = Rc::new(JsEngine::new().expect("engine"));
        register_current_engine(&engine);
        crate::engine::install(&engine).expect("install prelude");
        let callback = engine
            .with_js(|ctx| -> EngineResult<JsCallback> {
                let function: Function<'_> = ctx.eval(
                    r#"(event) => { event.messages.push({ role: "extension_note", text: "mutated" }); }"#,
                )?;
                Ok(JsCallback::new(&ctx, function))
            })
            .expect("callback");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let (result, messages) = local.block_on(&runtime, async {
            callback
                .call_with_event_mutation(json!({ "type": "context", "messages": [] }), None)
                .await
                .expect("call")
        });
        // 返回值为 `undefined`（TS 的 `void`），修改通过事件对象读回。
        assert_eq!(result, JsonValue::Null);
        assert_eq!(messages.as_array().map(Vec::len), Some(1));
        assert_eq!(messages[0]["role"], "extension_note");
    }

    #[test]
    fn extension_errors_use_the_installed_reporter() {
        let seen: Arc<Mutex<Vec<ExtensionErrorReport>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_for_reporter = Arc::clone(&seen);
        set_extension_error_reporter(Some(Arc::new(move |report| {
            seen_for_reporter.lock().unwrap().push(report);
        })));
        report_extension_error(ExtensionErrorReport {
            extension_path: "<ext>".to_string(),
            event: "command".to_string(),
            error: "boom".to_string(),
            stack: None,
        });
        set_extension_error_reporter(None);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].extension_path, "<ext>");
        assert_eq!(seen[0].event, "command");
        assert_eq!(seen[0].error, "boom");
    }

    #[test]
    fn stream_simple_pumps_js_events_into_the_rust_stream() {
        let engine = Rc::new(JsEngine::new().expect("engine"));
        register_current_engine(&engine);
        crate::engine::install(&engine).expect("install prelude");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let partial = serde_json::to_value(kiana_ai::types::AssistantMessage::default())
                .expect("partial");
            let event = json!({
                "type": "text_delta",
                "contentIndex": 0,
                "delta": "hi",
                "partial": partial,
            });
            let _: kiana_ai::types::AssistantMessageEvent =
                serde_json::from_value(event.clone()).expect("event parses");
            let source = format!(
                r#"(model, context, options) => {{
                    globalThis.__streamArgs = {{
                        modelId: model.id,
                        hasContext: typeof context === "object",
                        optionsType: typeof options,
                    }};
                    return {{
                        [Symbol.asyncIterator]() {{
                            let sent = false;
                            return {{
                                async next() {{
                                    if (sent) return {{ done: true, value: undefined }};
                                    sent = true;
                                    return {{ done: false, value: {event} }};
                                }},
                            }};
                        }},
                    }};
                }}"#
            );
            let stream_fn = engine
                .with_js(
                    |ctx| -> EngineResult<kiana_ai::compat::ApiStreamSimpleFunction> {
                        let function: Function<'_> = ctx.eval(source)?;
                        Ok(provider_stream_simple_from_js(&ctx, function))
                    },
                )
                .expect("stream fn");
            let stream = stream_fn(
                &test_model(),
                &kiana_ai::types::TranscriptContext::default(),
                kiana_ai::types::SimpleStreamOptions::default(),
            );
            let mut receiver = stream.take_receiver().expect("receiver");
            let event = receiver.recv().await.expect("event");
            match event {
                kiana_ai::types::AssistantMessageEvent::TextDelta { delta, .. } => {
                    assert_eq!(delta, "hi");
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });
        let args = engine
            .eval_script::<String>("JSON.stringify(globalThis.__streamArgs)", "<stream-args>")
            .expect("stream args");
        let args: JsonValue = serde_json::from_str(&args).expect("json");
        assert_eq!(args["modelId"], "m");
        assert_eq!(args["hasContext"], true);
        assert_eq!(args["optionsType"], "object");
    }

    #[test]
    fn route_request_keeps_signal_and_undefined_optionals() {
        let engine = Rc::new(JsEngine::new().expect("engine"));
        register_current_engine(&engine);
        crate::engine::install(&engine).expect("install prelude");
        let request = ModelRouteRequest {
            model: test_model(),
            thinking_level: kiana_ai::types::ModelThinkingLevel::Off,
            reason: kiana_serde_reason(),
            previous: None,
            failed: None,
            state: None,
            messages: Vec::new(),
            signal: Some(kiana_ai::utils::abort::AbortScope::new()),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let value = local
            .block_on(&runtime, async {
                engine.with_js(|ctx| -> EngineResult<String> {
                    let route = route_request_value(&ctx, &request)?;
                    let inspect: Function<'_> = ctx.eval(
                        r#"(request) => JSON.stringify({
                        keys: Object.keys(request),
                        signalType: typeof request.signal,
                        signalAborted: request.signal.aborted,
                        previousType: typeof request.previous,
                        stateType: typeof request.state,
                        failedType: typeof request.failed,
                    })"#,
                    )?;
                    inspect.call((route,))
                })
            })
            .expect("inspect");
        let value: JsonValue = serde_json::from_str(&value).expect("json");
        let keys = value["keys"].as_array().cloned().unwrap_or_default();
        let keys: Vec<&str> = keys.iter().filter_map(JsonValue::as_str).collect();
        // 回归：可选字段曾被显式序列化成 null，signal 曾完全缺失。
        assert_eq!(
            keys,
            vec![
                "reason",
                "thinkingLevel",
                "signal",
                "state",
                "model",
                "previous",
                "failed",
                "messages",
            ]
        );
        assert_eq!(value["signalType"], "object");
        assert_eq!(value["signalAborted"], false);
        assert_eq!(value["previousType"], "undefined");
        assert_eq!(value["stateType"], "undefined");
        assert_eq!(value["failedType"], "undefined");
    }

    fn kiana_serde_reason() -> kiana_session_core::virtual_models::ModelRouteReason {
        serde_json::from_value(JsonValue::String("user".to_string())).expect("reason")
    }

    #[test]
    fn tool_signal_and_on_update_are_bridged() {
        let engine = Rc::new(JsEngine::new().expect("engine"));
        register_current_engine(&engine);
        crate::engine::install(&engine).expect("install prelude");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let callback = engine
                .with_js(|ctx| -> EngineResult<JsCallback> {
                    let function: Function<'_> = ctx.eval(
                        r#"
(signal, update) => {
    globalThis.__toolArgsProbe = {
        aborted: signal.aborted,
        signalType: typeof signal.addEventListener,
        updateType: typeof update,
    };
    update({ content: [{ type: "text", text: "chunk" }] });
    return 1;
}
"#,
                    )?;
                    Ok(JsCallback::new(&ctx, function))
                })
                .expect("callback");

            let token = tokio_util::sync::CancellationToken::new();
            token.cancel();
            let updates = Arc::new(Mutex::new(Vec::new()));
            let updates_for_callback = Arc::clone(&updates);
            let on_update: AgentToolUpdateCallback = Arc::new(move |result| {
                updates_for_callback.lock().unwrap().push(result);
            });
            let args = vec![
                JsArg::Build(Arc::new(move |ctx: &Ctx<'_>| {
                    abort_signal_value(ctx, &token)
                })),
                JsArg::Build(on_update_value(on_update)),
            ];
            let value = callback.call_async(args).await.expect("call");
            assert_eq!(value, JsonValue::from(1));
            assert_eq!(updates.lock().unwrap().len(), 1);
        });

        let probe = engine
            .eval_script::<String>("JSON.stringify(globalThis.__toolArgsProbe)", "<probe>")
            .expect("probe");
        let probe: JsonValue = serde_json::from_str(&probe).expect("json");
        // 回归：两个参数曾经恒为 undefined。
        assert_eq!(probe["aborted"], true);
        assert_eq!(probe["signalType"], "function");
        assert_eq!(probe["updateType"], "function");
    }
}
