//! `core/extensions/runner.ts`（1565 行）门面：`ExtensionRunner`、`bind_core`/
//! `bind_command_context`/`set_ui_context`、`invalidate`。
//!
//! # 线程模型与 `Send + Sync`
//!
//! `kiana-session` 以 `Arc<ExtensionRunner>` 共享 runner，`McpServerChangeListener` 与
//! `UiPromptHost` 都要求 `Send + Sync`，`ContextHost` 也要求 `Send + Sync`。而
//! `kiana-ext-api::Extension` 里包含 `Arc<dyn Fn ...>`（非 `Send`）的 handler，因此本模块把
//! **扩展集合**放进线程本地的表（[`ExtensionTable`]），runner 本身只持有表 id 与
//! 全部字段都是 `Send + Sync` 的 [`RunnerCore`]。所有扩展访问都发生在引擎线程（设计文档
//! §2.0 的单线程模型）；在别的线程上访问会带着明确的错误信息 panic，而不是产生数据竞争。
//!
//! # 与 TS 的已知差异
//!
//! - `ExtensionRunner` 的 13 个 emitter 返回 `Result`（TS 的 async 方法只会在两种情况下
//!   reject：`emitToolCall`/`emitUserBash` 的 handler 抛错；Rust 里把这两种情况作为
//!   `Err(DispatchError)`，其余基础设施错误也同样返回 `Err`）。
//! - `before_agent_start` 的 `event.systemPrompt` 渲染：TS 用 `buildSystemPrompt(currentOptions)`
//!   （`kiana-resources` 的实现，本 crate 不依赖 `kiana-resources`）。装配方（`kiana-session` 的
//!   `bindCore` 等价物）**必须**通过 [`ExtensionRunner::set_system_prompt_renderer`] 注入
//!   等价渲染器；未注入时只有 `forceSystemPrompt` 分支可复现（`buildSystemPrompt` 对
//!   `forceSystemPrompt !== undefined` 直接返回它），其余选项改动无法重新渲染。
//!   loader 适配器把 `event.systemPrompt` 物化成**访问器**（每次读取都调用注入的渲染器，
//!   并传入本事件对象的 `systemPromptOptions`——与 TS 的 getter 闭包语义一致），
//!   `ctx.getSystemPrompt()` 也被覆盖为同一按读时重渲染的实现。跨 handler 的链（前一个
//!   handler 返回 `systemPrompt` / 原地改 `systemPromptOptions`，后一个 handler 的
//!   `ctx.getSystemPrompt()` 看到结果）——链式可见性由共享的事件状态保证。
//! - `emit_tool_call` 接收 `&mut ToolCallEvent`：TS 的 `event.input` 是共享可变对象，
//!   handler 的原地修改写回调用方的事件（`kiana-agent-core` 的 `ToolArguments` 句柄）。
//! - `bind_core` 在**没有** `ProviderActions` 时回落到 `ModelRegistry`。`ProviderConfig` 到
//!   `kiana-config::ProviderConfigInput` 的转换覆盖数据字段（name/baseUrl/apiKey/api/
//!   headers/authHeader/models/images/classifiers/streamSimple/refreshModels/oauth）。OAuth
//!   的登录交互经 owned event/prompt 通道转发，JS future 由引擎线程的常驻 `LocalSet` 驱动；
//!   因此凭证存储派生的 `Send` 刷新任务不会跨线程移动 JS 对象。
//!   回落路径只在 `ProviderActions` 为 `None` 时可达；TS 的主程序（`_bindExtensionCore`）
//!   总是提供 provider actions，因此集成构建不经过它。ProviderActions 的 Rust 闭包用
//!   panic 表达“抛出”，`bind_core` 捕获后经线程本地槽同步抛回扩展代码。
//!
//! # 文件结构（阅读顺序）
//!
//! 本文件只放门面与线程本地状态；具体逻辑按子模块拆分：
//!
//! - [`dispatch`]：通用 `emit` 与 11 个专用派发器，合并语义在 `js/dispatch.js`；
//! - [`boundary`]：`emit_boundary` 的入口（`turn_end` / `agent_before_settle`）；
//! - [`context`]：`ExtensionContext` 与 `create_*_context_value` 的 JS 物化；
//! - [`commands`]、[`keybindings`]、[`renderers`]、[`mcp_registration`]、[`noop_ui`]、
//!   [`ui_prompt`]：门面方法背后的纯逻辑与包装器。
//!
//! # 主要数据结构
//!
//! - [`ExtensionRunner`]：对外门面；对外字段只有 [`ExtensionTable`] 句柄与
//!   `Arc<RunnerCore>`（另有一个 `owns_table` 标志，见下方生命周期说明），因此可以放进
//!   `Arc` 交给会话层共享。
//! - [`ExtensionTable`]：线程本地 `HashMap<u64, Vec<Extension>>` 的键（`Copy`）。扩展集合
//!   含非 `Send` 的 handler 闭包，数据留在创建线程，不能随 runner 移动；`Vec` 的顺序就是
//!   扩展加载顺序，也是 handler 的派发顺序。
//! - [`RunnerCore`]：全部 `Send + Sync` 的上下文状态（已绑定的动作、UI、stale 文案、错误
//!   监听器、`before_agent_start` 期间的系统提示覆盖栈等）。可变字段用 `Mutex` 保护，
//!   监听器 id 用 `AtomicU64` 单调递增。
//! - [`RunnerHost`]：每个 `ctx` 持有一份克隆，把 `ContextHost` 的读取转发给 `RunnerCore`。
//! - `CONTEXT_VALUE_FACTORIES`：`Arc<dyn ExtensionContextApi>` 的地址 → JS 物化工厂，
//!   让 loader 的上下文桥能在 JS 侧重建 `ctx`（见 [`RunnerHost::create_context`]）。
//!
//! # 一次典型调用（从装配到事件派发）
//!
//! 1. 会话层从 loader 取扩展集合，调用 [`ExtensionRunner::new`]：登记线程本地表、安装
//!    `ctx` 物化桥与错误上报器；
//! 2. [`ExtensionRunner::bind_core`] 写入动作并冲刷加载期排队的 provider/virtual model
//!    注册；
//! 3. [`ExtensionRunner::set_ui_context`] / [`ExtensionRunner::bind_command_context`] 补齐
//!    UI 与命令上下文；
//! 4. 会话事件通过 `emit*` 系列方法派发：每次取 handler 快照 → `js/dispatch.js` 合并 →
//!    把结果反序列化回 Rust 类型；
//! 5. runner 析构时注销线程本地表（仅当它拥有该表，见 [`ExtensionRunner`] 的生命周期说明）。
//!
//! # 失败模式速查
//!
//! - 非创建线程访问扩展：`with` panic，`try_with` / `is_registered` 返回 `None` / `false`；
//!   在别的线程 Drop 表只记 warning 并故意泄漏条目（不能跨线程析构非 `Send` 数据）。
//! - [`ExtensionRunner::invalidate`] 只接受第一次非空文案；此后 `assert_active` 与 `ctx`
//!   访问都返回同一文案。空串按“未设置”处理（TS 的 truthiness）。
//! - 未绑定动作时 [`RunnerHost`] 有确定的回落值（见 `impl ContextHost for RunnerHost` 上方
//!   的注释），不会 panic，但会得到空/`true` 之类的默认值。
//! - 派发错误分两类：handler 抛出的错误进 [`DispatchError`]；除 `emit_tool_call` 完全
//!   不捕获外，`emit_user_bash` 先上报 `emitError` 再以 [`DispatchError::Js`] 重抛；其余
//!   emitter 的 handler 错误在 JS 侧被捕获后只走 `emitError` 监听器。

pub mod boundary;
pub mod commands;
pub mod context;
pub mod dispatch;
pub mod keybindings;
pub mod mcp_registration;
pub mod noop_ui;
pub mod renderers;
pub mod ui_prompt;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::thread::ThreadId;

tokio::task_local! {
    /// [`with_cwd_override`] 的作用域值。
    static CWD_OVERRIDE: PathBuf;
}

/// 在 `cwd` 作用域内执行 `future`：期间通过 runner 创建的扩展/工具上下文把
/// `cwd()` 报告为该目录。
///
/// 子代理隔离工作区需要工具执行目录与权限检查目录一致；会话仍只有一个 runner，
/// 因此用任务局部覆盖而不是复制/重建 runner（复制会重复注册线程本地扩展表）。
/// 作用域之外（或非 tokio 任务）读取时回落到 runner 的构造期 cwd。
pub async fn with_cwd_override<F>(cwd: PathBuf, future: F) -> F::Output
where
    F: std::future::Future,
{
    CWD_OVERRIDE.scope(cwd, future).await
}

fn scoped_cwd() -> Option<PathBuf> {
    CWD_OVERRIDE.try_with(|cwd| cwd.clone()).ok()
}

/// 当前作用域里的 cwd 覆盖值（没有覆盖时为 `None`）。
///
/// 任务局部（task-local）值不会跨 `tokio::spawn` / `spawn_local` 传播。执行路径
/// 需要把工具交给新任务时（后台 bash、扩展直接调用 `ctx.tools[i].execute`），
/// 必须在派生点取一次覆盖值，再在子任务里用 [`with_cwd_override`] 重新包起来，
/// 否则子任务构造的工具上下文会退回 runner 的构造期 cwd（父仓库）。
pub fn current_cwd_override() -> Option<PathBuf> {
    scoped_cwd()
}

use indexmap::IndexMap;
use kiana_agent_core::{AgentMessage, AgentTool, ThinkingLevel};
use kiana_ai::models::provider::Provider;
use kiana_ai::types::{ImageContent, JsonValue, Model, ProviderHeaders};
use kiana_config::cache::warmer::CacheWarmingAction;
use kiana_config::models::composer::ProviderConfigInput;
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::resolver::ScopedModel;
use kiana_core::diagnostics::ResourceDiagnostic;
use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::events::SessionShutdownEvent;
use kiana_ext_api::system_prompt::{BuildSystemPromptOptions, NormalizedBuildSystemPromptOptions};
use kiana_ext_api::types::{
    CompactOptions, ContextUsage, ExtensionActions, ExtensionCommandContextActions,
    ExtensionCommandContextApi, ExtensionContextActions, ExtensionContextApi, ExtensionError,
    ExtensionFlag, ExtensionMode, ExtensionRuntime, ExtensionShortcut, ExtensionToolContextApi,
    FlagValue, NewSessionOptions, NewSessionResult, ProviderConfig, RegisteredTool,
    ResolvedCommand,
};
use kiana_ext_api::ui::ExtensionUiContext;
use kiana_ext_api::{
    EntryRenderer, Extension, LocalBoxFuture, MarkdownTransformer, MessageRenderer, ToolRenderers,
};
use kiana_session_core::messages::CustomMessage;
use kiana_session_core::session_manager::ReadonlySessionManager;
use kiana_session_core::virtual_models::VirtualModelDefinition;
use tokio_util::sync::CancellationToken;

use crate::loader;

use self::context::{ContextError, ContextHost, ExtensionContext};

/// 派发失败（TS 的 Promise rejection）。
///
/// 具体类型与各个变体见 [`dispatch::DispatchError`]；`emit*` 系列方法都返回它。
pub use self::dispatch::DispatchError;

// ============================================================================
// 公开类型（Rust-only helper）
// ============================================================================

/// 扩展运行期错误监听器。TS: `runner.ts#ExtensionErrorListener`。
pub type ExtensionErrorListener = Arc<dyn Fn(ExtensionError) + Send + Sync>;

/// 系统提示渲染器（`buildSystemPrompt` 的等价物）。
///
/// TS 的 `emitBeforeAgentStart` 直接调用 `kiana-resources` 的 `buildSystemPrompt(currentOptions)`；
/// `kiana-extensions`（L9）不能依赖 `kiana-resources`（L10），因此把渲染函数作为装配期注入的
/// 回调：[`ExtensionRunner::set_system_prompt_renderer`]。未注入时退回 runner 当前的系统
/// 提示（`getSystemPrompt` 动作），主会话路径下两者相等（`getSystemPrompt()` 就是
/// `buildSystemPrompt(_baseSystemPromptOptions)`）。
///
/// 实现必须与 `buildSystemPrompt` 一致：`options.forceSystemPrompt !== undefined` 时直接
/// 返回它，否则按 `customPrompt` / 段落 / 工具 / cwd / 技能等渲染。
pub type SystemPromptRenderer =
    Arc<dyn Fn(&NormalizedBuildSystemPromptOptions) -> String + Send + Sync>;

/// provider / 虚拟模型注册动作（TS `bindCore` 的可选第三参数）。
///
/// TS 的 `{ registerProvider?, registerNativeProvider?, unregisterProvider?,
/// registerVirtualModel?, unregisterVirtualModel? }`。
#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct ProviderActions {
    /// `registerProvider(name, config)`。
    pub register_provider: Option<Arc<dyn Fn(String, ProviderConfig) + Send + Sync>>,
    /// `registerNativeProvider(provider)`。
    pub register_native_provider: Option<Arc<dyn Fn(Arc<dyn Provider>) + Send + Sync>>,
    /// `unregisterProvider(name)`。
    pub unregister_provider: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// `registerVirtualModel(definition)`。
    pub register_virtual_model: Option<Arc<dyn Fn(VirtualModelDefinition) + Send + Sync>>,
    /// `unregisterVirtualModel(provider, id)`。
    pub unregister_virtual_model: Option<Arc<dyn Fn(String, String) + Send + Sync>>,
}

impl std::fmt::Debug for ProviderActions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderActions")
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

/// `emitBoundary` 的基事件（不含 `entries`/`continue`/`context` 三个共享字段）。
///
/// TS: `BoundaryBaseEvent = Omit<TurnEndEvent, "entries" | "continue" | "context"> |
/// Omit<AgentBeforeSettleEvent, "entries" | "continue" | "context">`。
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum BoundaryBaseEvent {
    /// `turn_end`。
    TurnEnd(kiana_ext_api::events::TurnEndEvent),
    /// `agent_before_settle`。
    AgentBeforeSettle(kiana_ext_api::events::AgentBeforeSettleEvent),
}

impl BoundaryBaseEvent {
    /// 事件名（`turn_end` / `agent_before_settle`）。
    pub fn event_type(&self) -> &'static str {
        match self {
            BoundaryBaseEvent::TurnEnd(_) => "turn_end",
            BoundaryBaseEvent::AgentBeforeSettle(_) => "agent_before_settle",
        }
    }

    /// 去掉三个共享字段后的 JSON（handler 事件由 JS 侧用 `{...base, entries, continue,
    /// context}` 重建），键序与 TS 调用点的对象字面量一致
    /// （`{type, turnIndex, message, toolResults, messageEntryId, toolResultEntryIds,
    /// outcome}`，`agent_before_settle` 是 `{type, outcome}`）。
    pub fn base_json(&self) -> Result<JsonValue, DispatchError> {
        let value = match self {
            BoundaryBaseEvent::TurnEnd(event) => serde_json::to_value(event),
            BoundaryBaseEvent::AgentBeforeSettle(event) => serde_json::to_value(event),
        }
        .map_err(|error| DispatchError::InvalidResult(error.to_string()))?;
        let JsonValue::Object(mut object) = value else {
            return Err(DispatchError::InvalidResult(
                "boundary event is not an object".to_owned(),
            ));
        };
        object.shift_remove("entries");
        object.shift_remove("continue");
        object.shift_remove("context");
        let order: &[&str] = match self {
            BoundaryBaseEvent::TurnEnd(_) => &[
                "type",
                "turnIndex",
                "message",
                "toolResults",
                "messageEntryId",
                "toolResultEntryIds",
                "outcome",
            ],
            BoundaryBaseEvent::AgentBeforeSettle(_) => &["type", "outcome"],
        };
        Ok(dispatch::ordered_object(object, order))
    }
}

/// `emitBoundary` 的返回值。TS: `runner.ts#BoundaryDispatchResult`。
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryDispatchResult {
    /// 最终草稿（无效时为 `[]`）。
    pub entries: Vec<kiana_ext_api::events::SessionBoundaryDraft>,
    /// 是否继续。
    pub r#continue: bool,
    /// 最后一次成功构建的上下文预览。
    pub context: kiana_ext_api::events::BoundaryContextPreview,
    /// 最后一次构建是否成功。
    pub valid: bool,
}

/// 边界上下文构建回调：`buildContext(entries) => preview`。
///
/// TS: `emitBoundary(baseEvent, buildContext)` 的第二个参数。返回 `Err(message)` 对应
/// TS 的抛出（`message` 进入 `Invalid boundary entries: <msg>`）。
pub type BuildBoundaryContext = Arc<
    dyn Fn(
            Vec<kiana_ext_api::events::SessionBoundaryDraft>,
        )
            -> LocalBoxFuture<'static, Result<kiana_ext_api::events::BoundaryContextPreview, String>>
        + Send
        + Sync,
>;

/// 一条带来源扩展的资源路径。TS: `resources_discover` 结果里的
/// `{ path, extensionPath }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePathEntry {
    /// 资源路径。
    pub path: String,
    /// 注册它的扩展路径。
    pub extension_path: String,
}

/// `emitResourcesDiscover` 的返回值。TS: `runner.ts` 的内联返回类型。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourcesDiscoverPaths {
    /// 技能路径。
    pub skill_paths: Vec<ResourcePathEntry>,
    /// prompt 模板路径。
    pub prompt_paths: Vec<ResourcePathEntry>,
    /// 主题路径。
    pub theme_paths: Vec<ResourcePathEntry>,
}

/// 启动一次 run 的触发信息（`before_agent_start` 事件去掉 prompt 字段）。
///
/// TS: `runner.ts#BeforeAgentStartTrigger`
/// （`Pick<BeforeAgentStartUserEvent, "messageType" | "prompt" | "images"> |
/// Pick<BeforeAgentStartCustomEvent, "messageType" | "message">`）。
#[derive(Debug, Clone, PartialEq)]
pub enum BeforeAgentStartTrigger {
    /// 用户提示。
    User {
        /// 展开后的原始用户提示。
        prompt: String,
        /// 附带的图片。
        images: Option<Vec<ImageContent>>,
    },
    /// 自定义消息。
    Custom {
        /// 启动本次 run 的自定义消息。
        message: CustomMessage,
    },
}

/// `emitBeforeAgentStart` 的合并结果。TS: `runner.ts#BeforeAgentStartCombinedResult`。
#[derive(Debug, Clone, PartialEq)]
pub struct BeforeAgentStartCombinedResult {
    /// 各 handler 返回的自定义消息（按 handler 顺序）。
    pub messages: Vec<kiana_ext_api::types::CustomMessagePayload>,
    /// 归一化后（并被 `forceSystemPrompt` 链式覆盖）的系统提示选项。
    pub system_prompt_options: NormalizedBuildSystemPromptOptions,
}

// ============================================================================
// JS 可见的上下文宿主（`ContextHost` 要求 `Send + Sync`）
// ============================================================================

/// `createContext()` 读时求值的宿主状态（全部 `Send + Sync`）。
///
/// TS 的 `ExtensionRunner` 字段分成两部分：上下文相关字段（cwd/sessionManager/
/// modelRegistry/动作）与扩展集合。前者在这里，后者在线程本地的 [`ExtensionTable`]。
///
/// 可变字段都用 `Mutex` 保护（监听器 id 除外，用 `AtomicU64`）：`RunnerCore` 由 `Arc`
/// 共享，错误监听器、UI prompt 调度等 `Send + Sync` 路径可能从别的线程读它。字段访问器
/// 返回动作/容器的**克隆**（动作容器的字段全是 `Arc`，`ui_context` 等容器本身就是
/// `Arc`），因此持锁时间
/// 只有一次赋值或一次克隆，调用动作本身发生在锁外。`lock` 帮助函数在锁中毒时取回内部
/// 值：锁内不执行用户代码，中毒只可能来自 OOM 级别的 panic。
pub struct RunnerCore {
    runtime: Arc<ExtensionRuntime>,
    cwd: PathBuf,
    session_manager: Arc<dyn ReadonlySessionManager>,
    model_registry: Arc<ModelRegistry>,
    actions: Mutex<Option<ExtensionContextActions>>,
    command_actions: Mutex<Option<ExtensionCommandContextActions>>,
    ui_context: Mutex<Arc<dyn ExtensionUiContext>>,
    mode: Mutex<ExtensionMode>,
    ui_prompt: Mutex<Option<Arc<dyn ExtensionUiContext>>>,
    stale_message: Mutex<Option<String>>,
    /// `before_agent_start` 期间的 `ctx.getSystemPrompt()` 覆盖（栈：嵌套派发各一层）。
    system_prompt_overrides: Mutex<Vec<String>>,
    error_listeners: Mutex<Vec<(u64, ExtensionErrorListener)>>,
    next_listener_id: AtomicU64,
    reported_mcp_servers: Mutex<HashSet<String>>,
    shortcut_diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    command_diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    /// `getThinkingLevel` 动作（TS 直接读 `runtime.getThinkingLevel()`；Rust 的 runtime
    /// 快照在未绑定时是抛错桩，因此单独保存以便 `None` 表示未绑定）。
    thinking_level: Mutex<Option<kiana_ext_api::GetThinkingLevelHandler>>,
    /// `before_agent_start` 的系统提示渲染器（装配期注入；见 [`SystemPromptRenderer`]）。
    system_prompt_renderer: Mutex<Option<SystemPromptRenderer>>,
}

impl std::fmt::Debug for RunnerCore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunnerCore")
            .field("cwd", &self.cwd)
            .field("stale", &self.stale_message())
            .finish_non_exhaustive()
    }
}

/// 逐项执行一次宿主回调，把 panic（TS 的 throw）转换成错误文本。
fn catch_item<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else if let Some(message) = payload.downcast_ref::<&str>() {
            (*message).to_owned()
        } else {
            "extension callback panicked".to_owned()
        }
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 锁内没有用户代码；毒化只可能来自 OOM 级别的 panic。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RunnerCore {
    fn new(
        runtime: Arc<ExtensionRuntime>,
        cwd: PathBuf,
        session_manager: Arc<dyn ReadonlySessionManager>,
        model_registry: Arc<ModelRegistry>,
    ) -> Self {
        Self {
            runtime,
            cwd,
            session_manager,
            model_registry,
            actions: Mutex::new(None),
            command_actions: Mutex::new(None),
            ui_context: Mutex::new(noop_ui::no_op_ui_context()),
            mode: Mutex::new(ExtensionMode::Print),
            ui_prompt: Mutex::new(None),
            stale_message: Mutex::new(None),
            system_prompt_overrides: Mutex::new(Vec::new()),
            error_listeners: Mutex::new(Vec::new()),
            next_listener_id: AtomicU64::new(1),
            reported_mcp_servers: Mutex::new(HashSet::new()),
            shortcut_diagnostics: Mutex::new(Vec::new()),
            command_diagnostics: Mutex::new(Vec::new()),
            thinking_level: Mutex::new(None),
            system_prompt_renderer: Mutex::new(None),
        }
    }

    fn runtime(&self) -> &Arc<ExtensionRuntime> {
        &self.runtime
    }

    /// 当前 cwd。
    ///
    /// 若调用栈处在 [`with_cwd_override`] 作用域内（子代理隔离工作区），优先返回覆盖
    /// 值：工具与扩展上下文都必须与实际执行目录一致，否则权限检查和文件写入会落在
    /// 不同目录。
    fn cwd(&self) -> PathBuf {
        scoped_cwd().unwrap_or_else(|| self.cwd.clone())
    }

    fn stale_message(&self) -> Option<String> {
        lock(&self.stale_message).clone()
    }

    fn assert_active(&self) -> Result<(), ContextError> {
        // TS: `if (this.staleMessage) throw` —— 空串是 falsy，不视为陈旧。
        match self.stale_message() {
            Some(message) if !message.is_empty() => Err(ContextError::new(message)),
            _ => Ok(()),
        }
    }

    /// 把实例标记为陈旧（TS 的 `if (!this.staleMessage)`：空串也算“未设置”）。
    fn invalidate(&self, message: &str) -> bool {
        let mut stale = lock(&self.stale_message);
        if stale.as_deref().is_some_and(|current| !current.is_empty()) {
            return false;
        }
        *stale = Some(message.to_owned());
        true
    }

    fn context_actions(&self) -> Option<ExtensionContextActions> {
        lock(&self.actions).clone()
    }

    fn command_actions(&self) -> Option<ExtensionCommandContextActions> {
        lock(&self.command_actions).clone()
    }

    fn ui(&self) -> Arc<dyn ExtensionUiContext> {
        Arc::clone(&lock(&self.ui_context))
    }

    fn set_ui(&self, ui: Arc<dyn ExtensionUiContext>) {
        *lock(&self.ui_context) = ui;
    }

    fn mode(&self) -> ExtensionMode {
        *lock(&self.mode)
    }

    fn set_mode(&self, mode: ExtensionMode) {
        *lock(&self.mode) = mode;
    }

    fn emit_error(&self, error: ExtensionError) {
        let listeners: Vec<ExtensionErrorListener> = lock(&self.error_listeners)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(error.clone());
        }
    }

    fn push_system_prompt_override(&self, prompt: String) {
        lock(&self.system_prompt_overrides).push(prompt);
    }

    fn replace_system_prompt_override(&self, prompt: String) {
        let mut stack = lock(&self.system_prompt_overrides);
        if let Some(last) = stack.last_mut() {
            *last = prompt;
        } else {
            stack.push(prompt);
        }
    }

    fn pop_system_prompt_override(&self) {
        lock(&self.system_prompt_overrides).pop();
    }
}

/// 一个 runner 上下文的 JS 物化工厂（已捕获 context 的 kind / toolCallId / signal）。
#[allow(clippy::type_complexity)] // HRTB 的闭包 trait 对象拆成别名也仍是同一复杂度
type ContextValueFactory = Arc<
    dyn for<'js> Fn(&rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> + Send + Sync,
>;

/// `Arc<dyn ExtensionContextApi>` 的地址 → （存活校验 + JS 物化工厂）。
///
/// loader 的 `ContextBridge` 只拿到 trait 对象（`kiana-ext-api` 冻结，无下转型 API），而
/// 具体类型 [`context::ExtensionContext`] 的 kind/toolCallId/signal 是私有的。runner 在
/// 创建上下文时登记一个重建 JS 值的闭包；桥按地址取回。`Weak` 保证上下文销毁后不会
/// 命中（地址复用由新的登记覆盖）。
#[allow(clippy::type_complexity)] // 同上：注册表值就是（存活校验, 工厂）
type ContextFactoryRegistry =
    Mutex<HashMap<usize, (Weak<dyn ExtensionContextApi>, ContextValueFactory)>>;

static CONTEXT_VALUE_FACTORIES: LazyLock<ContextFactoryRegistry> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn context_key(context: &Arc<dyn ExtensionContextApi>) -> usize {
    Arc::as_ptr(context) as *const () as usize
}

// 登记前顺手清掉已销毁的条目，避免地址复用后旧条目仍占着 key；查询时命中失效的 Weak
// 也会删除。表是进程级（不是线程本地）：键是堆地址 + Weak 存活校验，同一地址只可能对应
// 同一个仍存活的 Arc。
fn register_context_factory(context: &Arc<dyn ExtensionContextApi>, factory: ContextValueFactory) {
    let key = context_key(context);
    let mut factories = lock(&CONTEXT_VALUE_FACTORIES);
    factories.retain(|_, (weak, _)| weak.strong_count() > 0);
    factories.insert(key, (Arc::downgrade(context), factory));
}

fn context_factory(context: &Arc<dyn ExtensionContextApi>) -> Option<ContextValueFactory> {
    let key = context_key(context);
    let mut factories = lock(&CONTEXT_VALUE_FACTORIES);
    match factories.get(&key) {
        Some((weak, factory)) if weak.strong_count() > 0 => Some(Arc::clone(factory)),
        Some(_) => {
            factories.remove(&key);
            None
        }
        None => None,
    }
}

/// 把 `Arc<dyn ExtensionContextApi>` 还原成 JS `ctx`；没有工厂时返回 `undefined`。
///
/// loader 的上下文桥在 JS 侧需要 `ctx` 时调用它。找不到工厂意味着上下文不是本 runner
/// 创建的（或已销毁）：JS 侧拿到 `undefined`，对 `ctx` 成员的访问会自然失败。
fn context_value_from_api<'js>(
    ctx: &rquickjs::Ctx<'js>,
    context: &Arc<dyn ExtensionContextApi>,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    match context_factory(context) {
        Some(factory) => factory(ctx),
        None => Ok(rquickjs::Value::new_undefined(ctx.clone())),
    }
}

/// 安装 runner 的 `ctx` 物化桥（loader 的 `CONTEXT_BRIDGE` / `TOOL_CONTEXT_BRIDGE` /
/// `COMMAND_CONTEXT_BRIDGE`）。线程本地；每次 [`ExtensionRunner::new`] 覆盖为同一实现。
///
/// 三个桥共用 `context_value_from_api`：按 `Arc` 地址查登记表，查不到时返回
/// `undefined`（上下文不属于本 runner 或已销毁）。本函数还注册 live 快照推送钩子，
/// 让 `kiana.on()` / 退订在 handler 执行期间立即影响 runner 的快照（见函数体内注释）。
fn install_context_bridges() {
    loader::set_context_bridge(Some(Arc::new(|ctx, context| {
        context_value_from_api(ctx, &context)
    })));
    loader::set_tool_context_bridge(Some(Arc::new(|ctx, context| {
        let context: Arc<dyn ExtensionContextApi> = context;
        context_value_from_api(ctx, &context)
    })));
    loader::set_command_context_bridge(Some(Arc::new(|ctx, context| {
        let context: Arc<dyn ExtensionContextApi> = context;
        context_value_from_api(ctx, &context)
    })));
    // TS 的 `extension.handlers` 与 runner 派发用的是**同一个 Map**：handler 里的
    // `kiana.off()`/退订闭包立即影响后续派发。Rust 的扩展表是值快照，live 条目在最后一个
    // JS 句柄释放后就查不到了（`refresh_extension_handlers` 的 Weak 查找失败），因此
    // loader 在每次 handler 增删后把快照推过来，按 (path, resolvedPath) 对齐所有 runner 表。
    loader::set_extension_live_sync(Some(Box::new(|extension: &Extension| {
        EXTENSION_TABLES.with(|tables| {
            for extensions in tables.borrow_mut().values_mut() {
                for current in extensions.iter_mut() {
                    if current.path == extension.path
                        && current.resolved_path == extension.resolved_path
                    {
                        current.handlers = extension.handlers.clone();
                    }
                }
            }
        });
    })));
}

/// 安装 loader 的扩展错误上报器（command / shortcut handler 的 rejection）。
///
/// TS 在两个不同的调用点处理这类错误：`AgentSession._tryExecuteExtensionCommand`
/// 用 `runner.emitError({ extensionPath: "command:<name>", event: "command" })`
/// （`agent-session.ts:2193-2205`），交互模式对快捷键 rejection 显示
/// `Shortcut handler error: <message>`（`interactive-mode.ts:2289-2292`）。Rust 的
/// `CommandHandler` / `ShortcutHandler` 是冻结的 `Fn`（无错误通道），loader 适配器只能
/// 通过线程本地上报器转交；本函数把 report 原样映射成 [`ExtensionError`] 交给
/// `emitError`，`event` 保留 `"command"` / `"shortcut"` 供监听方区分。未安装时 loader
/// 退回 `tracing::error!`（独立使用 loader、没有 runner 的场景）。
///
/// 线程本地：同线程后创建的 runner 覆盖前一个（与 [`install_context_bridges`] 相同语义）；
/// TS 的每个 session 一个 runner，单线程装配下同样只有一个活跃 runner。
fn install_error_reporter(core: Arc<RunnerCore>) {
    loader::set_extension_error_reporter(Some(Arc::new(
        move |report: loader::ExtensionErrorReport| {
            core.emit_error(ExtensionError {
                extension_path: report.extension_path,
                event: report.event,
                error: report.error,
                stack: report.stack,
            });
        },
    )));
}

/// [`ContextHost`] 的实现（`Arc` 便宜克隆；每个 ctx 持有它）。
#[derive(Clone)]
pub struct RunnerHost {
    core: Arc<RunnerCore>,
}

impl std::fmt::Debug for RunnerHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RunnerHost").finish_non_exhaustive()
    }
}

impl RunnerHost {
    /// 创建上下文（TS 的 `createContext()`）。
    ///
    /// 同时登记“地址 → JS 物化工厂”（`CONTEXT_VALUE_FACTORIES`），供 loader 的上下文桥在
    /// JS 侧把同一个 `Arc<dyn ExtensionContextApi>` 还原成 `ctx` 对象。工厂持有 `Weak`：
    /// 上下文销毁后查询失败，JS 侧得到 `undefined`。
    pub fn create_context(&self) -> Arc<dyn ExtensionContextApi> {
        let host: Arc<dyn ContextHost> = Arc::new(self.clone());
        let context = ExtensionContext::base(Arc::clone(&host));
        let api: Arc<dyn ExtensionContextApi> = context;
        let factory_host = Arc::clone(&host);
        register_context_factory(
            &api,
            Arc::new(move |ctx| context::create_context_value(ctx, Arc::clone(&factory_host))),
        );
        api
    }

    /// 创建工具上下文（TS 的 `createToolContext(toolCallId, signal)`）。
    ///
    /// 与 [`RunnerHost::create_context`] 一样登记物化工厂；工厂额外捕获 `toolCallId` 与
    /// 取消信号，使 JS 侧 `ctx` 的 `tools` / `executeTool` 与取消行为可用。
    pub fn create_tool_context(
        &self,
        tool_call_id: &str,
        signal: Option<CancellationToken>,
    ) -> Arc<dyn ExtensionToolContextApi> {
        let host: Arc<dyn ContextHost> = Arc::new(self.clone());
        let context = ExtensionContext::tool(Arc::clone(&host), tool_call_id, signal.clone());
        let api: Arc<dyn ExtensionToolContextApi> = context;
        let api_base: Arc<dyn ExtensionContextApi> = api.clone();
        let factory_host = Arc::clone(&host);
        let factory_tool_call_id = tool_call_id.to_owned();
        register_context_factory(
            &api_base,
            Arc::new(move |ctx| {
                context::create_tool_context_value(
                    ctx,
                    Arc::clone(&factory_host),
                    &factory_tool_call_id,
                    signal.clone(),
                )
            }),
        );
        api
    }

    /// 创建命令上下文（TS 的 `createCommandContext()`）。
    ///
    /// 与 [`RunnerHost::create_context`] 一样登记物化工厂；命令上下文多出的
    /// `newSession` / `fork` / `switchSession` / `navigateTree` / `reload` 成员来自
    /// `ExtensionCommandContextActions`（`bind_command_context` 绑定的动作）。
    pub fn create_command_context(&self) -> Arc<dyn ExtensionCommandContextApi> {
        let host: Arc<dyn ContextHost> = Arc::new(self.clone());
        let context = ExtensionContext::command(Arc::clone(&host));
        let api: Arc<dyn ExtensionCommandContextApi> = context;
        let api_base: Arc<dyn ExtensionContextApi> = api.clone();
        let factory_host = Arc::clone(&host);
        register_context_factory(
            &api_base,
            Arc::new(move |ctx| {
                context::create_command_context_value(ctx, Arc::clone(&factory_host))
            }),
        );
        api
    }
}

// ContextHost 的读取全部转发到 `RunnerCore`。未绑定动作时（`bind_core` /
// `bind_command_context` 之前，或独立使用 runner 的测试）不会 panic，而是给确定回落值：
// - `model` / `signal` / `get_context_usage` / `thinking_level`：`None`；
// - `scoped_models` / `get_callable_tools`：空 `Vec`；
// - `is_idle` / `is_project_trusted`：`true`（没有动作就按空闲/已信任处理）；
// - `has_pending_messages`：`false`；
// - `get_system_prompt`：空串（`before_agent_start` 期间的覆盖栈优先，见
//   `RunnerCore::system_prompt_overrides`）；
// - `get_system_prompt_options`：`default_system_prompt_options(cwd)`；
// - future 型：立即完成；会话操作返回 `NewSessionResult { cancelled: false }`；
// - `execute_tool`：`None`（语义为“没有绑定 executeTool 动作”）；
// - `abort` / `shutdown` / `compact`：空操作。
// `is_project_trusted` 的 `true` 是回落值而非策略判断：装配方必须在依赖项目信任结果的
// 动作发生前完成 `bind_core`。`mode` 在 `set_ui_context` 之前是 `ExtensionMode::Print`，
// `ui` 是进程级 no-op 上下文。
impl ContextHost for RunnerHost {
    fn assert_active(&self) -> Result<(), ContextError> {
        self.core.assert_active()
    }

    fn ui(&self) -> Arc<dyn ExtensionUiContext> {
        self.core.ui()
    }

    fn mode(&self) -> ExtensionMode {
        self.core.mode()
    }

    fn has_ui(&self) -> bool {
        !noop_ui::is_no_op_ui_context(&self.core.ui())
    }

    fn cwd(&self) -> PathBuf {
        self.core.cwd()
    }

    fn session_manager(&self) -> Arc<dyn ReadonlySessionManager> {
        Arc::clone(&self.core.session_manager)
    }

    fn model_registry(&self) -> Arc<ModelRegistry> {
        Arc::clone(&self.core.model_registry)
    }

    fn model(&self) -> Option<Model> {
        self.core
            .context_actions()
            .and_then(|actions| (actions.get_model)())
    }

    fn scoped_models(&self) -> Vec<ScopedModel> {
        match self.core.context_actions() {
            Some(actions) => (actions.get_scoped_models)(),
            None => Vec::new(),
        }
    }

    fn thinking_level(&self) -> Option<ThinkingLevel> {
        let handler = lock(&self.core.thinking_level).clone()?;
        Some(handler())
    }

    fn is_idle(&self) -> bool {
        match self.core.context_actions() {
            Some(actions) => (actions.is_idle)(),
            None => true,
        }
    }

    fn is_project_trusted(&self) -> bool {
        match self.core.context_actions() {
            Some(actions) => (actions.is_project_trusted)(),
            None => true,
        }
    }

    fn signal(&self) -> Option<CancellationToken> {
        self.core
            .context_actions()
            .and_then(|actions| (actions.get_signal)())
    }

    fn abort(&self) {
        if let Some(actions) = self.core.context_actions() {
            (actions.abort)();
        }
    }

    fn has_pending_messages(&self) -> bool {
        match self.core.context_actions() {
            Some(actions) => (actions.has_pending_messages)(),
            None => false,
        }
    }

    fn shutdown(&self) {
        if let Some(actions) = self.core.context_actions() {
            (actions.shutdown)();
        }
    }

    fn get_context_usage(&self) -> Option<ContextUsage> {
        self.core
            .context_actions()
            .and_then(|actions| (actions.get_context_usage)())
    }

    fn compact(&self, options: Option<CompactOptions>) {
        if let Some(actions) = self.core.context_actions() {
            (actions.compact)(options);
        }
    }

    fn get_system_prompt(&self) -> String {
        // `before_agent_start` 期间 TS 覆盖 `ctx.getSystemPrompt` 为渲染当前选项。
        if let Some(prompt) = lock(&self.core.system_prompt_overrides).last() {
            return prompt.clone();
        }
        match self.core.context_actions() {
            Some(actions) => (actions.get_system_prompt)(),
            None => String::new(),
        }
    }

    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        match self
            .core
            .context_actions()
            .and_then(|actions| actions.get_system_prompt_options.clone())
        {
            Some(get) => get(),
            None => {
                let cwd = self.core.cwd();
                default_system_prompt_options(&cwd.to_string_lossy())
            }
        }
    }

    fn wait_for_idle(&self) -> LocalBoxFuture<'static, ()> {
        match self.core.command_actions() {
            Some(actions) => (actions.wait_for_idle)(),
            None => Box::pin(async {}),
        }
    }

    fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        match self.core.command_actions() {
            Some(actions) => (actions.new_session)(options),
            None => Box::pin(async { NewSessionResult { cancelled: false } }),
        }
    }

    fn fork(
        &self,
        entry_id: String,
        options: Option<kiana_ext_api::types::ForkOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        match self.core.command_actions() {
            Some(actions) => (actions.fork)(entry_id, options),
            None => Box::pin(async { NewSessionResult { cancelled: false } }),
        }
    }

    fn navigate_tree(
        &self,
        target_id: String,
        options: Option<kiana_ext_api::types::NavigateTreeOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        match self.core.command_actions() {
            Some(actions) => (actions.navigate_tree)(target_id, options),
            None => Box::pin(async { NewSessionResult { cancelled: false } }),
        }
    }

    fn switch_session(
        &self,
        session_path: String,
        options: Option<kiana_ext_api::types::SwitchSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        match self.core.command_actions() {
            Some(actions) => (actions.switch_session)(session_path, options),
            None => Box::pin(async { NewSessionResult { cancelled: false } }),
        }
    }

    fn reload(&self) -> LocalBoxFuture<'static, ()> {
        match self.core.command_actions() {
            Some(actions) => (actions.reload)(),
            None => Box::pin(async {}),
        }
    }

    fn get_callable_tools(&self) -> Vec<AgentTool> {
        match self
            .core
            .context_actions()
            .and_then(|actions| actions.get_callable_tools.clone())
        {
            Some(get) => get(),
            None => Vec::new(),
        }
    }

    fn execute_tool(
        &self,
        tool_call_id: &str,
        name: &str,
        args: JsonValue,
        options: kiana_ext_api::types::ExecuteToolOptions,
    ) -> Option<kiana_ext_api::BoxFuture<'static, kiana_agent_core::AgentToolCallOutcome>> {
        let execute = self
            .core
            .context_actions()
            .and_then(|actions| actions.execute_tool.clone())?;
        Some(execute(
            tool_call_id.to_owned(),
            name.to_owned(),
            args,
            options,
        ))
    }
}

/// `emit_ui_prompt_event` 的调度宿主（TS 的 `queueMicrotask(() => void this.emit(event))`）。
///
/// [`ExtensionRunner::set_ui_context`] 用 [`ui_prompt::UiPromptContext`] 包装真实 UI 时把
/// 本宿主交给包装层；对话框深度 `0 -> 1` / `1 -> 0` 时包装层经调度器回调到这里。事件不
/// 直接派发，而是交给 `spawn_dispatch_event` 异步执行；该函数要求调用点处于本线程的
/// `LocalSet` 上下文（`spawn_local` 的前提）。调度器已经延后过一次，这里再 `spawn` 一次
/// 仍是“不同步发出”，符合 `ui_prompt` 模块文档的约定。
struct RunnerUiPromptHost {
    table: ExtensionTable,
    core: Arc<RunnerCore>,
}

impl ui_prompt::UiPromptHost for RunnerUiPromptHost {
    fn emit_ui_prompt(&self, event: kiana_ext_api::events::ExtensionEvent) {
        spawn_dispatch_event(self.table, Arc::clone(&self.core), event);
    }
}

// ============================================================================
// 线程本地的扩展集合
// ============================================================================

static NEXT_EXTENSION_TABLE: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// runner id → 扩展集合（`Extension` 含非 `Send` 的 handler 闭包）。
    static EXTENSION_TABLES: RefCell<HashMap<u64, Vec<Extension>>> = RefCell::new(HashMap::new());
}

/// 扩展集合的句柄（`Send + Sync`；数据在线程本地表里）。
///
/// 所有访问都必须发生在创建它的线程上（设计文档 §2.0 的单线程模型）。
///
/// 句柄是 `Copy` 的：复制不复制扩展数据，只共享同一个线程本地条目。跨线程使用时
/// [`ExtensionTable::with`] 会 panic，[`ExtensionTable::try_with`] 返回 `None`，
/// [`ExtensionTable::is_registered`] 返回 `false`；注销在非创建线程只记 warning 并放任
/// 条目泄漏，避免跨线程析构非 `Send` 数据（见 `unregister` 的实现）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionTable {
    id: u64,
    thread: ThreadId,
}

impl ExtensionTable {
    fn register(extensions: Vec<Extension>) -> Self {
        let id = NEXT_EXTENSION_TABLE.fetch_add(1, Ordering::SeqCst);
        EXTENSION_TABLES.with(|tables| {
            tables.borrow_mut().insert(id, extensions);
        });
        Self {
            id,
            thread: std::thread::current().id(),
        }
    }

    fn unregister(self) {
        if std::thread::current().id() != self.thread {
            // 在别的线程上 Drop：留着条目会泄漏，但绝不能跨线程访问非 Send 数据。
            tracing::warn!(
                "extension table {} dropped on a different thread; leaking its extensions",
                self.id
            );
            return;
        }
        EXTENSION_TABLES.with(|tables| {
            tables.borrow_mut().remove(&self.id);
        });
    }

    /// 当前线程是否是创建该表的线程。
    pub fn is_owner_thread(self) -> bool {
        std::thread::current().id() == self.thread
    }

    /// 表是否仍登记在当前线程（runner 已 Drop / 换线程时为 false）。
    pub fn is_registered(self) -> bool {
        if !self.is_owner_thread() {
            return false;
        }
        EXTENSION_TABLES.with(|tables| tables.borrow().contains_key(&self.id))
    }

    /// 与 [`ExtensionTable::with`] 相同，但表缺失或线程不符时返回 `None`
    /// （用于 fire-and-forget 的监听器/延迟派发）。
    pub fn try_with<R>(self, f: impl FnOnce(&mut [Extension]) -> R) -> Option<R> {
        if !self.is_owner_thread() {
            return None;
        }
        EXTENSION_TABLES.with(|tables| {
            let mut tables = tables.borrow_mut();
            let extensions = tables.get_mut(&self.id)?;
            Some(f(extensions.as_mut_slice()))
        })
    }

    /// 在创建线程上访问扩展集合。
    pub fn with<R>(self, f: impl FnOnce(&mut [Extension]) -> R) -> R {
        assert!(
            std::thread::current().id() == self.thread,
            "extension data can only be accessed on the thread that created the ExtensionRunner"
        );
        EXTENSION_TABLES.with(|tables| {
            let mut tables = tables.borrow_mut();
            let extensions = tables
                .get_mut(&self.id)
                .expect("extension table is no longer registered");
            f(extensions.as_mut_slice())
        })
    }
}

// ============================================================================
// ExtensionRunner
// ============================================================================

/// 扩展运行时门面。TS: `runner.ts#ExtensionRunner`。
///
/// 持有两样东西：
/// - [`ExtensionTable`]：线程本地扩展集合的句柄（本身 `Send + Sync`，数据不在对象里）；
/// - `Arc<RunnerCore>`：上下文状态与已绑定的动作，被所有 `ctx` 共享。
///
/// runner 本身可以 `Arc` 跨线程共享（会话层就这么做），但访问扩展集合
/// （[`ExtensionRunner::with_extensions`]）必须在创建表的线程上。
///
/// # 生命周期
///
/// [`ExtensionRunner::new`] 登记一张表并设置 `owns_table = true`；`Drop` 时注销该表。
/// crate 内的 `from_parts` 构造的临时 runner（用于异步派发）共享同一张表但
/// `owns_table = false`，避免一次 fire-and-forget 派发结束时拆掉仍在使用的表。
pub struct ExtensionRunner {
    table: ExtensionTable,
    core: Arc<RunnerCore>,
    /// 是否负责注销线程本地的扩展集合（`from_parts` 构造的临时 runner 不拥有它）。
    owns_table: bool,
}

impl std::fmt::Debug for ExtensionRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionRunner")
            .field("cwd", &self.core.cwd())
            .field("extensions", &self.get_extension_paths())
            .finish_non_exhaustive()
    }
}

impl Drop for ExtensionRunner {
    fn drop(&mut self) {
        if !self.owns_table {
            return;
        }
        // TS 不清除 MCP change listener：旧 runner 的监听器会被新 runner 的 bindCore 覆盖。
        // 这里同样保留（表已注销时监听器/延迟派发通过 `try_with` 静默跳过）。
        self.table.unregister();
    }
}

impl ExtensionRunner {
    /// `new ExtensionRunner(extensions, runtime, cwd, sessionManager, modelRegistry)`。
    ///
    /// 副作用都在线程本地，重复创建会覆盖前一个 runner 的安装：
    /// - 安装 `ctx` 物化桥（`install_context_bridges`）与扩展错误上报器
    ///   （`install_error_reporter`）；
    /// - 确保本线程有一个常驻的 `LocalSet`（`ensure_extension_local`，供 OAuth 的
    ///   `Send` 任务驱动 JS future；没有 tokio 运行时时此步失败但不影响其余功能）；
    /// - 登记扩展集合（[`ExtensionTable`]），由本 runner 负责在 `Drop` 时注销。
    ///
    /// `extensions` 的顺序就是后续派发与收集（工具/命令/flag）的顺序。
    pub fn new(
        extensions: Vec<Extension>,
        runtime: Arc<ExtensionRuntime>,
        cwd: PathBuf,
        session_manager: Arc<dyn ReadonlySessionManager>,
        model_registry: Arc<ModelRegistry>,
    ) -> Self {
        install_context_bridges();
        let _ = ensure_extension_local();
        let core = Arc::new(RunnerCore::new(
            runtime,
            cwd,
            session_manager,
            model_registry,
        ));
        install_error_reporter(Arc::clone(&core));
        Self {
            table: ExtensionTable::register(extensions),
            core,
            owns_table: true,
        }
    }

    /// 由 facade/其它单元用相同状态重建一个 runner（`spawn` 的事件派发使用）。
    pub(crate) fn from_parts(table: ExtensionTable, core: Arc<RunnerCore>) -> Self {
        Self {
            table,
            core,
            owns_table: false,
        }
    }

    /// 共享状态。
    pub(crate) fn core(&self) -> Arc<RunnerCore> {
        Arc::clone(&self.core)
    }

    /// 在创建线程上访问扩展集合。
    ///
    /// 不做 live 快照刷新：看到的是创建 runner 时的副本（外加 live-sync 钩子推送的
    /// handler 表）。需要看到工厂结束后动态注册的工具/命令时用内部 `with_live_extensions`
    /// （[`ExtensionRunner::get_all_registered_tools`] 等门面方法已经内置刷新）。
    ///
    /// # Panics
    ///
    /// 在非创建线程调用时 panic（`ExtensionTable::with` 的断言）。
    pub fn with_extensions<R>(&self, f: impl FnOnce(&mut [Extension]) -> R) -> R {
        self.table.with(f)
    }

    /// 访问前先把快照刷成 loader 的实时扩展对象（TS 的 `extension` 是同一个对象：
    /// 工厂结束后再 `kiana.registerTool` / `registerCommand` / `registerShortcut` 等直接生效）。
    fn with_live_extensions<R>(&self, f: impl FnOnce(&mut [Extension]) -> R) -> R {
        self.with_extensions(|extensions| {
            loader::refresh_extensions(extensions);
            f(extensions)
        })
    }

    /// 绑定核心动作（TS `bindCore`）。
    ///
    /// 分五步（对应函数体内编号注释）：
    /// 1. 把 `ExtensionActions` 写进共享 `ExtensionRuntime`，并重新安装 createContext
    ///    工厂；
    /// 2. 保存上下文动作与 `get_thinking_level`（runtime 里的快照在未绑定时是抛错桩，
    ///    单独保存才能用 `None` 表达“未绑定”）；
    /// 3. 安装 MCP server 变更监听：先检查扩展表是否仍登记，再异步派发
    ///    `mcp_servers_change` 并做未处理告警；
    /// 4. 冲刷加载期排队的注册（provider / native provider / virtual model），逐项把失败
    ///    交给错误监听器，单项失败不中断后续项；
    /// 5. 绑定运行期 provider 动作：有 `ProviderActions` 时调用注入闭包，否则直接操作
    ///    `ModelRegistry`；失败经线程本地槽同步抛回 JS 调用点。
    ///
    /// 重复调用是覆盖式的（动作与 MCP 监听器都会被替换）；`take_*` 队列只在第一次被取空。
    pub fn bind_core(
        &self,
        actions: ExtensionActions,
        context_actions: ExtensionContextActions,
        provider_actions: Option<ProviderActions>,
    ) {
        let runtime = Arc::clone(self.core.runtime());

        // 1. 复制 15 个动作进共享 runtime（所有扩展 API 引用它）。
        loader::bind_runtime_actions(&runtime, actions.clone());
        loader::bind_runtime_refresh_tools(&runtime, actions.refresh_tools.clone());
        let host = RunnerHost {
            core: Arc::clone(&self.core),
        };
        let create_host = host.clone();
        loader::bind_runtime_create_context(
            &runtime,
            Arc::new(move || create_host.create_context()),
        );

        // 2. 上下文动作。
        *lock(&self.core.thinking_level) = Some(actions.get_thinking_level.clone());
        *lock(&self.core.actions) = Some(context_actions);

        // 3. MCP 变更监听：注册之后立即能触达“连接它的那个扩展”。
        let listener_table = self.table;
        let listener_core = Arc::clone(&self.core);
        runtime
            .mcp_servers
            .set_change_listener(Some(Arc::new(move || {
                if !listener_table.is_registered() {
                    // 旧 runner 已销毁（或换线程）：TS 的旧监听器也只是捕获旧 runner。
                    return;
                }
                let servers = listener_core.runtime().mcp_servers.list();
                spawn_dispatch_event(
                    listener_table,
                    Arc::clone(&listener_core),
                    kiana_ext_api::events::ExtensionEvent::McpServersChange(
                        kiana_ext_api::events::McpServersChangeEvent {
                            event_type:
                                kiana_ext_api::events::McpServersChangeEventType::McpServersChange,
                            servers,
                        },
                    ),
                );
                report_unhandled_mcp_servers(listener_table, &listener_core);
            })));

        // 4. 冲刷加载期排队的注册（逐项错误隔离）。
        for error in loader::take_runtime_provider_config_errors(&runtime) {
            self.core.emit_error(ExtensionError {
                extension_path: error.extension_path,
                event: "register_provider".to_owned(),
                error: error.message,
                stack: None,
            });
        }
        for registration in loader::take_runtime_pending_provider_registrations(&runtime) {
            let outcome = match provider_actions
                .as_ref()
                .and_then(|actions| actions.register_provider.clone())
            {
                Some(register) => {
                    catch_item(|| register(registration.name.clone(), registration.config.clone()))
                }
                None => register_provider_fallback(
                    &self.core.model_registry,
                    &registration.name,
                    &registration.config,
                ),
            };
            if let Err(message) = outcome {
                self.core.emit_error(ExtensionError {
                    extension_path: registration.extension_path,
                    event: "register_provider".to_owned(),
                    error: message,
                    stack: None,
                });
            }
        }
        for registration in loader::take_runtime_pending_native_provider_registrations(&runtime) {
            let outcome = match provider_actions
                .as_ref()
                .and_then(|actions| actions.register_native_provider.clone())
            {
                Some(register) => catch_item(|| register(Arc::clone(&registration.provider))),
                None => self
                    .core
                    .model_registry
                    .register_native_provider(Arc::clone(&registration.provider))
                    .map_err(|error| error.to_string()),
            };
            if let Err(message) = outcome {
                self.core.emit_error(ExtensionError {
                    extension_path: registration.extension_path,
                    event: "register_provider".to_owned(),
                    error: message,
                    stack: None,
                });
            }
        }
        for registration in loader::take_runtime_pending_virtual_model_registrations(&runtime) {
            let outcome = match provider_actions
                .as_ref()
                .and_then(|actions| actions.register_virtual_model.clone())
            {
                Some(register) => catch_item(|| register(registration.definition.clone())),
                None => self
                    .core
                    .model_registry
                    .register_virtual_model(registration.definition.clone())
                    .map_err(|error| error.to_string()),
            };
            if let Err(message) = outcome {
                self.core.emit_error(ExtensionError {
                    extension_path: registration.extension_path,
                    event: "register_virtual_model".to_owned(),
                    error: message,
                    stack: None,
                });
            }
        }

        // 5. 此后的注册/注销立即生效（不再需要 /reload）。
        //
        // TS 把这些方法替换成直接调用 `ModelRegistry` 的闭包；注册失败会**同步抛出**到扩展
        // 的 `kiana.registerProvider(...)` 调用点（未捕获时由所在事件 handler 的 catch 上报
        // 到该扩展/事件）。`kiana-ext-api` 的 Runtime 动作签名返回 `()`（冻结契约），因此失败
        // 经线程本地槽交回 JS 宿主原语（`loader::set_provider_action_error`），由
        // `register_*_checked` 转成同步的 JS 异常。注入的 `ProviderActions` 是 Rust 闭包，
        // 其“抛出”形态是 panic：用 `catch_item` 捕获后走同一通道。
        let fallback_registry = Arc::clone(&self.core.model_registry);
        loader::bind_runtime_provider_actions(
            &runtime,
            loader::ProviderRuntimeActions {
                register_provider: Some(
                    match provider_actions
                        .as_ref()
                        .and_then(|actions| actions.register_provider.clone())
                    {
                        Some(register) => Arc::new(move |name: String, config: ProviderConfig| {
                            if let Err(message) = catch_item(|| register(name, config)) {
                                loader::set_provider_action_error(message);
                            }
                        })
                            as Arc<dyn Fn(String, ProviderConfig) + Send + Sync>,
                        None => {
                            let registry = Arc::clone(&fallback_registry);
                            Arc::new(move |name: String, config: ProviderConfig| {
                                if let Err(message) =
                                    register_provider_fallback(&registry, &name, &config)
                                {
                                    loader::set_provider_action_error(message);
                                }
                            })
                        }
                    },
                ),
                register_native_provider: Some(
                    match provider_actions
                        .as_ref()
                        .and_then(|actions| actions.register_native_provider.clone())
                    {
                        Some(register) => Arc::new(move |provider: Arc<dyn Provider>| {
                            if let Err(message) = catch_item(|| register(provider)) {
                                loader::set_provider_action_error(message);
                            }
                        })
                            as Arc<dyn Fn(Arc<dyn Provider>) + Send + Sync>,
                        None => {
                            let registry = Arc::clone(&fallback_registry);
                            Arc::new(move |provider: Arc<dyn Provider>| {
                                if let Err(error) = registry.register_native_provider(provider) {
                                    loader::set_provider_action_error(error.to_string());
                                }
                            })
                        }
                    },
                ),
                unregister_provider: Some(
                    match provider_actions
                        .as_ref()
                        .and_then(|actions| actions.unregister_provider.clone())
                    {
                        Some(unregister) => Arc::new(move |name: String| {
                            if let Err(message) = catch_item(|| unregister(name)) {
                                loader::set_provider_action_error(message);
                            }
                        })
                            as Arc<dyn Fn(String) + Send + Sync>,
                        None => {
                            let registry = Arc::clone(&fallback_registry);
                            Arc::new(move |name: String| registry.unregister_provider(&name))
                        }
                    },
                ),
                register_virtual_model: Some(
                    match provider_actions
                        .as_ref()
                        .and_then(|actions| actions.register_virtual_model.clone())
                    {
                        Some(register) => Arc::new(move |definition: VirtualModelDefinition| {
                            if let Err(message) = catch_item(|| register(definition)) {
                                loader::set_provider_action_error(message);
                            }
                        })
                            as Arc<dyn Fn(VirtualModelDefinition) + Send + Sync>,
                        None => {
                            let registry = Arc::clone(&fallback_registry);
                            Arc::new(move |definition: VirtualModelDefinition| {
                                if let Err(error) = registry.register_virtual_model(definition) {
                                    loader::set_provider_action_error(error.to_string());
                                }
                            })
                        }
                    },
                ),
                unregister_virtual_model: Some(
                    match provider_actions
                        .as_ref()
                        .and_then(|actions| actions.unregister_virtual_model.clone())
                    {
                        Some(unregister) => Arc::new(move |provider: String, id: String| {
                            if let Err(message) = catch_item(|| unregister(provider, id)) {
                                loader::set_provider_action_error(message);
                            }
                        })
                            as Arc<dyn Fn(String, String) + Send + Sync>,
                        None => {
                            let registry = Arc::clone(&fallback_registry);
                            Arc::new(move |provider: String, id: String| {
                                registry.unregister_virtual_model(&provider, &id);
                            })
                        }
                    },
                ),
            },
        );
    }

    /// 绑定命令上下文动作（TS `bindCommandContext`）。
    ///
    /// `None` 时命令 ctx 的 `newSession`/`fork`/`switchSession`/`navigateTree`/`reload`/
    /// `waitForIdle` 回落到立即完成的 future（见 `impl ContextHost for RunnerHost` 的注释），
    /// 不会报错。动作本身是 `Arc` 闭包，克隆后存进 [`RunnerCore`]。
    pub fn bind_command_context(&self, actions: Option<ExtensionCommandContextActions>) {
        *lock(&self.core.command_actions) = actions;
    }

    /// 设置 UI 上下文（TS `setUIContext`）。
    ///
    /// `Some(ui)` 时先用 [`ui_prompt::UiPromptContext`] 包装：扩展通过
    /// `ctx.ui.select/confirm/input/editor/custom` 打开阻塞式对话框时，包装层维护深度并在
    /// 边界（延迟）发出 `ui_prompt_start` / `ui_prompt_end`（调度规则见 [`ui_prompt`] 模块
    /// 文档）。包装后的上下文存进 [`RunnerCore`]（同时写入 `ui_prompt` 槽；该槽在本 crate
    /// 内只写不读）。
    ///
    /// `None` 时回落到进程级 [`noop_ui::no_op_ui_context`]；[`ExtensionRunner::has_ui`] 用
    /// `Arc::ptr_eq` 与这个单例比较来判断“当前没有 UI”。
    ///
    /// `mode` 单独保存，不参与包装；它决定 `ctx.mode` 的返回值（未调用本方法前默认
    /// `ExtensionMode::Print`）。
    pub fn set_ui_context(&self, ui: Option<Arc<dyn ExtensionUiContext>>, mode: ExtensionMode) {
        let context = match ui {
            Some(ui) => {
                let host: Arc<dyn ui_prompt::UiPromptHost> = Arc::new(RunnerUiPromptHost {
                    table: self.table,
                    core: Arc::clone(&self.core),
                });
                let wrapped = ui_prompt::UiPromptContext::wrap(ui, host);
                *lock(&self.core.ui_prompt) = Some(wrapped.clone());
                wrapped
            }
            None => {
                *lock(&self.core.ui_prompt) = None;
                noop_ui::no_op_ui_context()
            }
        };
        self.core.set_ui(context);
        self.core.set_mode(mode);
    }

    /// 当前 UI 上下文（`noOpUIContext` 在无 UI 时）。
    ///
    /// 有 UI 时返回的是 [`ExtensionRunner::set_ui_context`] 包装后的对象（带 `ui_prompt`
    /// 深度计数），不是调用方传入的原始 `Arc`；没有 UI 时是进程级 no-op 单例。
    pub fn get_ui_context(&self) -> Arc<dyn ExtensionUiContext> {
        self.core.ui()
    }

    /// 是否有对话框 UI（TS 的引用比较：当前 UI 不是进程级 no-op 单例）。
    pub fn has_ui(&self) -> bool {
        !noop_ui::is_no_op_ui_context(&self.core.ui())
    }

    /// 全部扩展路径（加载顺序）。
    pub fn get_extension_paths(&self) -> Vec<String> {
        self.with_extensions(|extensions| {
            extensions
                .iter()
                .map(|extension| extension.path.clone())
                .collect()
        })
    }

    /// 全部已注册工具（同名第一次注册获胜）。
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        self.with_live_extensions(|extensions| {
            let mut tools: IndexMap<String, RegisteredTool> = IndexMap::new();
            for extension in extensions.iter() {
                for tool in extension.tools.values() {
                    tools
                        .entry(tool.definition.name.clone())
                        .or_insert_with(|| tool.clone());
                }
            }
            tools.into_values().collect()
        })
    }

    /// 按名取工具定义（第一个注册它的扩展）。
    pub fn get_tool_definition(&self, tool_name: &str) -> Option<kiana_ext_api::ToolDefinition> {
        self.with_live_extensions(|extensions| {
            extensions
                .iter()
                .find_map(|extension| extension.tools.get(tool_name))
                .map(|tool| tool.definition.clone())
        })
    }

    /// 全部 flag（同名第一次注册获胜）。
    pub fn get_flags(&self) -> IndexMap<String, ExtensionFlag> {
        self.with_live_extensions(|extensions| {
            let mut flags: IndexMap<String, ExtensionFlag> = IndexMap::new();
            for extension in extensions.iter() {
                for (name, flag) in &extension.flags {
                    if !flags.contains_key(name) {
                        flags.insert(name.clone(), flag.clone());
                    }
                }
            }
            flags
        })
    }

    /// 写 flag 值（TS `setFlagValue`）。
    pub fn set_flag_value(&self, name: &str, value: FlagValue) {
        loader::runtime_set_flag_value(self.core.runtime(), name, value);
    }

    /// 读 flag 值（TS `getFlagValues`）。
    pub fn get_flag_values(&self) -> IndexMap<String, FlagValue> {
        loader::runtime_flag_values(self.core.runtime())
    }

    /// 解析扩展快捷键（TS `getShortcuts`）。
    ///
    /// - `has_ui()` 只影响警告文本：无 UI 时每条冲突诊断额外产生一条 `console.warn`
    ///   等价文本（有 UI 时 `warnings` 为空），不改变注册结果；
    /// - 诊断写入 runner，每次调用覆盖上一次（[`ExtensionRunner::get_shortcut_diagnostics`]）；
    /// - `warnings` 逐条用 `tracing::warn!` 输出（TS 的 `console.warn`）。
    ///
    /// 快捷键键名会 `to_lowercase()` 归一，同名后注册者胜出（见 [`keybindings`]）。
    pub fn get_shortcuts(
        &self,
        resolved_keybindings: &kiana_config::keybindings::KeybindingsConfig,
    ) -> IndexMap<String, ExtensionShortcut> {
        let has_ui = self.has_ui();
        let resolution = self.with_live_extensions(|extensions| {
            keybindings::get_shortcuts(extensions, resolved_keybindings, has_ui)
        });
        *lock(&self.core.shortcut_diagnostics) = resolution.diagnostics;
        for warning in resolution.warnings {
            // TS 的 `console.warn(message)`。
            tracing::warn!("{warning}");
        }
        resolution.shortcuts
    }

    /// 上一次 `getShortcuts` 产生的诊断。
    pub fn get_shortcut_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        lock(&self.core.shortcut_diagnostics).clone()
    }

    /// 标记陈旧（TS `invalidate`）。只在第一次**非空**文案生效；后续调用是 no-op。
    ///
    /// - `None` 使用 [`loader::DEFAULT_STALE_MESSAGE`] 的逐字文案；
    /// - 空串按 TS 的 truthiness 视为“未设置”，`assert_active` 不会失败；但只要
    ///   `RunnerCore` 接受了本次设置（此前没有非空文案），仍会调用 runtime 的
    ///   `invalidate`，runtime 照常退订它跟踪的 `events.on` 订阅；
    /// - 非空文案会写进共享 runtime：runtime 进入 stale 状态（JS 宿主原语据此抛错），并
    ///   退订跟踪的全部 `events.on` 订阅。
    ///
    /// 本方法只负责标记与退订；`ctx` 对象本身不受影响，之后的访问由
    /// `RunnerCore::assert_active` 统一返回同一文案。
    pub fn invalidate(&self, message: Option<String>) {
        let message = message.unwrap_or_else(|| loader::DEFAULT_STALE_MESSAGE.to_owned());
        if self.core.invalidate(&message) {
            loader::runtime_invalidate(self.core.runtime(), Some(&message));
        }
    }

    /// 静态断言当前实例仍然有效（TS `assertActive`）。
    pub fn assert_active(&self) -> Result<(), ContextError> {
        self.core.assert_active()
    }

    /// 注入 `before_agent_start` 的系统提示渲染器（`buildSystemPrompt` 的等价物）。
    ///
    /// 装配方（`kiana-session`）在构造 runner 后调用一次；传 `None` 恢复“使用 runner 当前
    /// 系统提示”的回落。见 [`SystemPromptRenderer`]。
    pub fn set_system_prompt_renderer(&self, renderer: Option<SystemPromptRenderer>) {
        *lock(&self.core.system_prompt_renderer) = renderer;
    }

    /// 当前渲染器（dispatch 用）。
    pub(crate) fn system_prompt_renderer(&self) -> Option<SystemPromptRenderer> {
        lock(&self.core.system_prompt_renderer).clone()
    }

    /// 注册错误监听器；返回退订函数（TS `onError`）。
    ///
    /// 监听器按注册顺序收到每个 [`ExtensionError`]；返回的退订函数只按内部 id 移除，
    /// 可以安全地多次调用。`emit_error` 先克隆监听器列表再逐个调用，因此在回调里退订
    /// 自己不会影响本轮派发。
    pub fn on_error(&self, listener: ExtensionErrorListener) -> Unsubscribe {
        let core = Arc::clone(&self.core);
        let id = core.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&core.error_listeners).push((id, listener));
        Box::new(move || {
            lock(&core.error_listeners).retain(|(existing, _)| *existing != id);
        })
    }

    /// 发出一个扩展错误（TS `emitError`）。
    ///
    /// 同步调用全部已注册监听器（顺序 = 注册顺序）。没有监听器时错误被静默丢弃；
    /// loader 的错误上报器（`install_error_reporter`）最终也走到这里。
    pub fn emit_error(&self, error: ExtensionError) {
        self.core.emit_error(error);
    }

    /// 对未处理的 MCP server 报错（TS `reportUnhandledMcpServers`）。
    ///
    /// 仅当**没有任何**扩展注册 `mcp_servers_change` handler 时才产生诊断；同一个 server
    /// 名在本 runner 生命周期内只报一次（`reported_mcp_servers` 去重）。扩展表已注销
    /// （旧 runner / 非创建线程）时直接返回，不发任何错误。
    pub fn report_unhandled_mcp_servers(&self) {
        report_unhandled_mcp_servers(self.table, &self.core);
    }

    /// 是否有某个事件类型的 handler。
    pub fn has_handlers(&self, event_type: &str) -> bool {
        self.with_live_extensions(|extensions| {
            extensions.iter().any(|extension| {
                extension
                    .handlers
                    .get(event_type)
                    .is_some_and(|handlers| !handlers.is_empty())
            })
        })
    }

    /// 按 `customType` 取消息渲染器。
    pub fn get_message_renderer(&self, custom_type: &str) -> Option<MessageRenderer> {
        self.with_live_extensions(|extensions| {
            renderers::get_message_renderer(extensions, custom_type)
        })
    }

    /// 全部 markdown 变换器（加载顺序）。
    pub fn get_markdown_transformers(&self) -> Vec<MarkdownTransformer> {
        self.with_live_extensions(|extensions| renderers::get_markdown_transformers(extensions))
    }

    /// 解析工具渲染器链。
    pub fn resolve_tool_renderers(
        &self,
        tool_name: &str,
        base: &dyn Fn() -> Option<ToolRenderers>,
    ) -> Option<ToolRenderers> {
        self.with_live_extensions(|extensions| {
            renderers::resolve_tool_renderers(extensions, tool_name, base)
        })
    }

    /// 按 `customType` 取条目渲染器。
    pub fn get_entry_renderer(&self, custom_type: &str) -> Option<EntryRenderer> {
        self.with_live_extensions(|extensions| {
            renderers::get_entry_renderer(extensions, custom_type)
        })
    }

    /// 全部已注册命令（含 `invocationName`）。
    ///
    /// 顺序 = 扩展加载顺序 × 每个扩展内 `commands` 的插入顺序。同名命令出现多次时所有
    /// 实例都带 `:n` 后缀（第一个是 `name:1`），只与其它名字撞车时向后找空位；细节见
    /// [`commands`] 模块文档。
    ///
    /// 每次调用都会把 `command_diagnostics` 覆盖为 `commands::get_command_diagnostics()`
    /// 的结果（当前实现恒为空）。
    pub fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        let resolved = self
            .with_live_extensions(|extensions| commands::resolve_registered_commands(extensions));
        *lock(&self.core.command_diagnostics) = commands::get_command_diagnostics();
        resolved
    }

    /// 上一次命令解析的诊断（当前恒为空）。
    pub fn get_command_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        lock(&self.core.command_diagnostics).clone()
    }

    /// 按 `invocationName` 取命令。
    ///
    /// 只精确匹配解析后的 `invocationName`，不匹配注册名：同名命令存在多个时
    /// `get_command("name")` 得到 `None`，应使用 `"name:1"` 等。
    pub fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        self.with_live_extensions(|extensions| commands::get_command(extensions, name))
    }

    /// 模型注册表。
    pub fn get_model_registry(&self) -> Arc<ModelRegistry> {
        Arc::clone(&self.core.model_registry)
    }

    /// 请求优雅关闭（TS `shutdown`）。
    ///
    /// 只调用绑定的 `shutdown` 上下文动作；不做任何本地清理（不注销扩展表、不动引擎）。
    /// 未绑定动作时是空操作。
    pub fn shutdown(&self) {
        if let Some(actions) = self.core.context_actions() {
            (actions.shutdown)();
        }
    }

    /// 当前 active tools（TS `getActiveTools`）。
    ///
    /// 先做 `assertActive()`（`spec/ca-extensions.md` §2.3.9 的
    /// `getActiveTools()` 映射），再读 runtime：实例已失效时这里直接失败，而不是返回空
    /// 列表。`kiana-ext-api` 的返回类型没有错误通道，因此用 panic 保留“陈旧实例不可继续
    /// 使用”的语义。
    ///
    /// # Panics
    ///
    /// 实例被 [`ExtensionRunner::invalidate`] 标记之后调用。
    pub fn get_active_tools(&self) -> Vec<String> {
        self.assert_active()
            .unwrap_or_else(|error| panic!("{}", error.message()));
        (self.core.runtime().get_active_tools)()
    }

    /// `createContext()`（TS）。
    pub fn create_context(&self) -> Arc<dyn ExtensionContextApi> {
        self.host().create_context()
    }

    /// `createToolContext(toolCallId, signal)`（TS）。
    pub fn create_tool_context(
        &self,
        tool_call_id: &str,
        signal: Option<CancellationToken>,
    ) -> Arc<dyn ExtensionToolContextApi> {
        self.host().create_tool_context(tool_call_id, signal)
    }

    /// `createCommandContext()`（TS）。
    pub fn create_command_context(&self) -> Arc<dyn ExtensionCommandContextApi> {
        self.host().create_command_context()
    }

    /// 当前生效的系统提示（`before_agent_start` 的初始 `event.systemPrompt`）。
    pub(crate) fn current_system_prompt(&self) -> String {
        RunnerHost {
            core: Arc::clone(&self.core),
        }
        .get_system_prompt()
    }

    /// 扩展错误落点（dispatch 表用）。
    pub(crate) fn error_sink(&self) -> dispatch::ErrorSink {
        let core = Arc::clone(&self.core);
        Arc::new(move |error: ExtensionError| core.emit_error(error))
    }

    /// `before_agent_start` 的系统提示覆盖落点。
    pub(crate) fn system_prompt_sink(&self) -> Option<Arc<dyn Fn(String) + Send + Sync>> {
        let core = Arc::clone(&self.core);
        Some(Arc::new(move |prompt: String| {
            core.replace_system_prompt_override(prompt);
        }))
    }

    fn host(&self) -> RunnerHost {
        RunnerHost {
            core: Arc::clone(&self.core),
        }
    }

    // ------------------------------------------------------------------
    // 事件派发入口（实现在 `dispatch.rs`，合并语义在 `js/dispatch.js`）
    // ------------------------------------------------------------------

    /// 泛型 `emit`（仅 `session_before_*` 取返回值；`cancel` 短路）。
    ///
    /// `None` 表示没有 handler 返回结果（JS 侧 `undefined`）。handler 返回了非
    /// `undefined` 的结果、但事件不是 `session_before_switch` / `fork` / `compact` /
    /// `tree` 时，才会得到 [`DispatchError::InvalidResult`]（TS 只读 `cancel`，Rust 需要
    /// 强类型结果）——实际 JS 侧本就忽略这类返回值（最终返回 `null`），此为防御分支。
    pub async fn emit(
        &self,
        event: kiana_ext_api::events::ExtensionEvent,
    ) -> Result<Option<kiana_ext_api::events::ExtensionEventResult>, DispatchError> {
        dispatch::emit(self, event).await
    }

    /// `emitBoundary`（`turn_end` / `agent_before_settle`）。
    ///
    /// `build_context` 在 handler 之间被调用以校验共享草稿：构建失败会把当前 `valid` 置为
    /// `false`（后续 handler 仍可修复草稿，下一次成功构建让 `valid` 回到 `true`）；结束时
    /// 若仍无效，返回 `entries = []`、`continue = false`、`valid = false`，错误文案与共享
    /// 草稿的保留规则见 [`boundary`]。`context` 是最后一次成功构建的预览。
    pub async fn emit_boundary(
        &self,
        base_event: BoundaryBaseEvent,
        build_context: BuildBoundaryContext,
    ) -> Result<BoundaryDispatchResult, DispatchError> {
        boundary::emit_boundary(self, base_event, build_context).await
    }

    /// `emitCacheWarmingDecision`。
    ///
    /// handler 结果字符串只有 `"stop"` 映射为 `Stop`；其他值（含 `null`、未知字符串、
    /// 未赋值）都等价于 `Warm`——唯一消费者按 `=== "stop"` 判定。
    pub async fn emit_cache_warming_decision(
        &self,
        event: kiana_ext_api::CacheWarmingDecisionEvent,
    ) -> Result<CacheWarmingAction, DispatchError> {
        dispatch::emit_cache_warming_decision(self, event).await
    }

    /// `emitMessageEnd`。
    ///
    /// `None` 表示没有 handler 改写消息（结果是 `undefined`）；`Some` 是替换后的消息。
    pub async fn emit_message_end(
        &self,
        event: kiana_ext_api::events::MessageEndEvent,
    ) -> Result<Option<AgentMessage>, DispatchError> {
        dispatch::emit_message_end(self, event).await
    }

    /// `emitToolResult`。
    ///
    /// `None` 表示没有 handler 返回补丁；`Some` 是要合并进工具结果的对象。
    pub async fn emit_tool_result(
        &self,
        event: kiana_ext_api::events::ToolResultEvent,
    ) -> Result<Option<kiana_ext_api::events::ToolResultEventResult>, DispatchError> {
        dispatch::emit_tool_result(self, event).await
    }

    /// `emitToolCall`（handler 错误向上穿透）。
    ///
    /// `event.input` 与 TS 一样是共享可变对象：handler 的就地修改写回 `event`，调用方
    /// 用最终 `event.input` 执行工具，后续 handler 也看到前面的修改。
    ///
    /// 返回 `None` 表示没有 handler 提供结果；`Some` 是 handler 给出的
    /// `ToolCallEventResult`。
    pub async fn emit_tool_call(
        &self,
        event: &mut kiana_ext_api::events::ToolCallEvent,
    ) -> Result<Option<kiana_ext_api::events::ToolCallEventResult>, DispatchError> {
        dispatch::emit_tool_call(self, event).await
    }

    /// `emitUserBash`（唯一向上抛的 emitter）。
    ///
    /// 返回 `Operations`（handler 接管 bash 执行）或 `Result`（handler 给出结果）；
    /// 都没有时返回 `None`（调用方本地执行）。handler 抛错以 [`DispatchError::Js`] 返回；
    /// 结果既非 `undefined` 也不含 `result` 时是 [`DispatchError::InvalidResult`]。
    pub async fn emit_user_bash(
        &self,
        event: kiana_ext_api::events::UserBashEvent,
    ) -> Result<Option<kiana_ext_api::events::UserBashEventResult>, DispatchError> {
        dispatch::emit_user_bash(self, event).await
    }

    /// `emitContext`（两阶段）。
    ///
    /// phase 1 用 `context` handler 快照；phase 2 的 `context_with_system` 快照在 phase 1
    /// 结束后、phase 2 开始时才向 Rust 索取（对应 TS `runner.ts:1336`），因此 phase 1 中
    /// 动态 `kiana.on()` / `off()` 的增删会反映到 phase 2。返回最终的完整消息列表。
    pub async fn emit_context(
        &self,
        messages: Vec<AgentMessage>,
    ) -> Result<Vec<AgentMessage>, DispatchError> {
        dispatch::emit_context(self, messages).await
    }

    /// `emitBeforeProviderRequest`。
    ///
    /// handler 未声明 `replaced` 时原样返回输入 `payload`；否则返回 handler 替换后的值
    /// （缺 `payload` 键时为 `null`）。
    pub async fn emit_before_provider_request(
        &self,
        payload: JsonValue,
    ) -> Result<JsonValue, DispatchError> {
        dispatch::emit_before_provider_request(self, payload).await
    }

    /// `emitBeforeProviderHeaders`。
    ///
    /// handler 没有返回 `headers` 时原样返回输入；否则返回修改后的 headers。
    pub async fn emit_before_provider_headers(
        &self,
        headers: ProviderHeaders,
    ) -> Result<ProviderHeaders, DispatchError> {
        dispatch::emit_before_provider_headers(self, headers).await
    }

    /// `emitBeforeAgentStart`。
    ///
    /// 初始 `systemPrompt` 由注入的 [`SystemPromptRenderer`] 渲染；未注入时退回
    /// `forceSystemPrompt`（若有）或 runner 当前系统提示（见模块文档“与 TS 的已知差异”）。
    /// handler 之间通过共享的 `systemPromptOptions` 链式覆盖。
    ///
    /// 返回合并的自定义消息（按 handler 顺序）与最终选项；handler 结果缺少
    /// `systemPromptOptions` 时是 [`DispatchError::InvalidResult`]。
    pub async fn emit_before_agent_start(
        &self,
        trigger: BeforeAgentStartTrigger,
        system_prompt_options: BuildSystemPromptOptions,
    ) -> Result<BeforeAgentStartCombinedResult, DispatchError> {
        dispatch::emit_before_agent_start(self, trigger, system_prompt_options).await
    }

    /// `emitResourcesDiscover`。
    ///
    /// 三组路径分别取 handler 结果的 `skillPaths` / `promptPaths` / `themePaths`；缺少
    /// `path` 字段的条目被跳过，缺少 `extensionPath` 时该字段记为空串。对应数组缺失或
    /// 不是数组时该组为空。
    pub async fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: kiana_ext_api::events::ResourcesDiscoverReason,
    ) -> Result<ResourcesDiscoverPaths, DispatchError> {
        dispatch::emit_resources_discover(self, cwd, reason).await
    }

    /// `emitInput`。
    ///
    /// `transform` 的 `text` 可以是任意 JS 值（按 `String()` 归一化）；`images` /
    /// `streamingBehavior` 未提供时对应键在 JS 事件对象里不存在（读作 `undefined`，
    /// 不是 `null`）。
    pub async fn emit_input(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        source: kiana_ext_api::events::InputSource,
        streaming_behavior: Option<kiana_ext_api::events::StreamingBehavior>,
    ) -> Result<kiana_ext_api::events::InputEventResult, DispatchError> {
        dispatch::emit_input(self, text, images, source, streaming_behavior).await
    }
}

// ============================================================================
// 自由函数
// ============================================================================

/// 发出 `session_shutdown` 事件；返回是否真的发出了（TS `emitSessionShutdownEvent`）。
///
/// 只有存在 `session_shutdown` handler 时才派发，否则直接返回 `false`。派发错误被丢弃
/// （`let _ = ...`），因此返回值只表示“是否派发过”，不表示成功。
pub async fn emit_session_shutdown_event(
    extension_runner: &ExtensionRunner,
    event: SessionShutdownEvent,
) -> bool {
    if extension_runner.has_handlers("session_shutdown") {
        let _ = extension_runner
            .emit(kiana_ext_api::events::ExtensionEvent::Session(
                kiana_ext_api::events::SessionEvent::Shutdown(event),
            ))
            .await;
        return true;
    }
    false
}

/// `reportUnhandledMcpServers` 的自由函数形态（监听器闭包无法捕获 `&self`）。
///
/// 表未登记（旧 runner / 非创建线程）时直接返回；判定“无 handler”前先刷新 live handler
/// 快照。同一个 server 名用 `core.reported_mcp_servers` 去重，只报一次。
fn report_unhandled_mcp_servers(table: ExtensionTable, core: &RunnerCore) {
    let runtime = core.runtime();
    let Some(has_handlers) = table.try_with(|extensions| {
        loader::refresh_extensions(extensions);
        extensions.iter().any(|extension| {
            extension
                .handlers
                .get("mcp_servers_change")
                .is_some_and(|handlers| !handlers.is_empty())
        })
    }) else {
        return;
    };
    if has_handlers {
        return;
    }
    for server in runtime.mcp_servers.list() {
        if !lock(&core.reported_mcp_servers).insert(server.name.clone()) {
            continue;
        }
        core.emit_error(ExtensionError {
            extension_path: server.extension_path,
            event: "register_mcp_server".to_owned(),
            error: format!(
                "MCP server \"{}\" is registered, but no loaded extension connects MCP servers; another extension may have replaced the built-in MCP support",
                server.name
            ),
            stack: None,
        });
    }
}

/// 异步发出一个事件（TS 的 `void this.emit(event)`）。
///
/// - 扩展表已注销（`is_registered` 为 false）时直接返回；
/// - 当前线程没有 tokio 运行时：记 warning 并丢弃事件，不 panic；
/// - 否则 `spawn_local` 一个任务，任务内用 `ExtensionRunner::from_parts` 构造不拥有表的
///   临时 runner 并 `emit`；派发错误只记 warning。
///
/// `spawn_local` 还要求调用时处于 tokio `LocalSet` 上下文（tokio 的约束）；本函数只检查
/// 运行时是否存在，不检查 `LocalSet`。
pub(crate) fn spawn_dispatch_event(
    table: ExtensionTable,
    core: Arc<RunnerCore>,
    event: kiana_ext_api::events::ExtensionEvent,
) {
    if !table.is_registered() {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("extension event dropped: no tokio runtime on this thread");
        return;
    }
    tokio::task::spawn_local(async move {
        let runner = ExtensionRunner::from_parts(table, core);
        if let Err(error) = runner.emit(event).await {
            tracing::warn!("extension event dispatch failed: {error}");
        }
    });
}

/// `normalizeBuildSystemPromptOptions({ cwd })` 的 `BuildSystemPromptOptions` 形态
/// （`ctx.getSystemPromptOptions()` 的默认值）。
///
/// 归一化后把每个可选字段都包成 `Some(..)`：调用方拿到的是显式值而不是 `None`；
/// `selected_tools` 的默认列表是 `["read", "bash", "edit", "write"]`（与
/// `normalize_build_system_prompt_options` 的缺省一致）。
pub fn default_system_prompt_options(cwd: &str) -> BuildSystemPromptOptions {
    let normalized = normalize_build_system_prompt_options(&BuildSystemPromptOptions {
        cwd: cwd.to_owned(),
        ..BuildSystemPromptOptions::default()
    });
    BuildSystemPromptOptions {
        custom_prompt: normalized.custom_prompt,
        force_system_prompt: normalized.force_system_prompt,
        selected_tools: Some(normalized.selected_tools),
        hidden_tools: Some(normalized.hidden_tools),
        tool_snippets: Some(normalized.tool_snippets),
        tool_guidelines: Some(normalized.tool_guidelines),
        prompt_guidelines: Some(normalized.prompt_guidelines),
        append_system_prompt: Some(normalized.append_system_prompt),
        sections: Some(normalized.sections),
        cwd: normalized.cwd,
        context_files: Some(normalized.context_files),
        skills: Some(normalized.skills),
    }
}

/// `normalizeBuildSystemPromptOptions`。
///
/// TS: `core/system-prompt.ts#normalizeBuildSystemPromptOptions`（`kiana-resources` 的实现在
/// 该 crate 里；这里只需要纯数据映射，避免新增依赖）。
///
/// 缺省值为当前稳定契约（测试 `normalizes_system_prompt_options_with_ts_defaults` 锁定）：
/// `selected_tools` → `["read", "bash", "edit", "write"]`；其余集合字段 → 空集合，
/// `append_system_prompt` → 空串；`cwd`、`force_system_prompt`、`custom_prompt` 原样
/// 保留（后两者交给调用方/渲染器决定优先级）。本函数不执行任何渲染。
pub fn normalize_build_system_prompt_options(
    input: &BuildSystemPromptOptions,
) -> NormalizedBuildSystemPromptOptions {
    NormalizedBuildSystemPromptOptions {
        custom_prompt: input.custom_prompt.clone(),
        force_system_prompt: input.force_system_prompt.clone(),
        selected_tools: input.selected_tools.clone().unwrap_or_else(|| {
            vec![
                "read".to_owned(),
                "bash".to_owned(),
                "edit".to_owned(),
                "write".to_owned(),
            ]
        }),
        hidden_tools: input.hidden_tools.clone().unwrap_or_default(),
        tool_snippets: input.tool_snippets.clone().unwrap_or_default(),
        tool_guidelines: input.tool_guidelines.clone().unwrap_or_default(),
        prompt_guidelines: input.prompt_guidelines.clone().unwrap_or_default(),
        append_system_prompt: input.append_system_prompt.clone().unwrap_or_default(),
        sections: input.sections.clone().unwrap_or_default(),
        cwd: input.cwd.clone(),
        context_files: input.context_files.clone().unwrap_or_default(),
        skills: input.skills.clone().unwrap_or_default(),
    }
}

// ============================================================================
// provider 回落（无 ProviderActions 时直接进 ModelRegistry）
// ============================================================================

fn register_provider_fallback(
    registry: &ModelRegistry,
    name: &str,
    config: &ProviderConfig,
) -> Result<(), String> {
    registry
        .register_provider(name, Some(provider_config_input(config)))
        .map_err(|error| error.to_string())
}

// ============================================================================
// 扩展直接执行工具的权限钩子
// ============================================================================

/// 扩展通过 `ctx.tools[i].execute(...)` / `create*Tool().execute(...)` **直接**执行工具
/// （不经过 `ctx.executeTool` / 嵌套调用管线）前的权限检查；返回 `Some(reason)` 表示拒绝。
///
/// 闭包捕获 `Weak<Core>` 等非 `Send` 状态，因此只能在扩展 JS 所在的线程上使用：宿主在
/// 绑定扩展运行时时安装，未安装（没有权限层）时这些路径保持原有行为。
pub type ToolPermissionCheck =
    Rc<dyn Fn(String, JsonValue) -> LocalBoxFuture<'static, Option<String>>>;

thread_local! {
    static TOOL_PERMISSION_CHECK: RefCell<Option<ToolPermissionCheck>> = const { RefCell::new(None) };
}

/// 安装/清除权限钩子。必须在扩展 JS 运行的线程上调用。
pub fn set_tool_permission_check(check: Option<ToolPermissionCheck>) {
    let _ = TOOL_PERMISSION_CHECK.try_with(|slot| *slot.borrow_mut() = check);
}

/// 运行权限钩子；未安装时返回 `None`（放行）。
pub(crate) fn check_tool_permission(
    name: &str,
    args: &JsonValue,
) -> Option<LocalBoxFuture<'static, Option<String>>> {
    TOOL_PERMISSION_CHECK
        .try_with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|check| check(name.to_owned(), args.clone()))
        })
        .ok()
        .flatten()
}

// ============================================================================
// Extension-local executor (OAuth runs in spawned, Send credential tasks)
// ============================================================================

thread_local! {
    static EXTENSION_LOCAL: std::cell::RefCell<Option<std::rc::Rc<tokio::task::LocalSet>>> =
        const { std::cell::RefCell::new(None) };
}

/// 槽位守卫：由驱动 `LocalSet` 的任务持有。
///
/// Drop 时仅当槽仍指向同一个 `LocalSet`（按 `Rc` 地址比较）才清空槽，避免误删后来安装的
/// 新 executor。只存地址而不存 `Rc`，因此守卫本身不会延长 `LocalSet` 的生命周期。
struct ExtensionLocalGuard {
    id: usize,
}

impl Drop for ExtensionLocalGuard {
    fn drop(&mut self) {
        EXTENSION_LOCAL.with(|slot| {
            let stale = slot
                .borrow()
                .as_ref()
                .is_some_and(|local| std::rc::Rc::as_ptr(local) as *const () as usize == self.id);
            if stale {
                *slot.borrow_mut() = None;
            }
        });
    }
}

/// Install one persistent local executor on the JavaScript engine thread.
///
/// Credential refresh is awaited by `Send` storage tasks, not necessarily by the
/// task which loaded the extension. A temporary LocalSet in that caller would
/// neither own the engine thread nor be driven. This nested set is continuously
/// driven by a task on the host's LocalSet; only its oneshot results cross the
/// Send boundary. JavaScript objects never move to another thread.
pub(crate) fn ensure_extension_local() -> Option<std::rc::Rc<tokio::task::LocalSet>> {
    EXTENSION_LOCAL.with(|slot| {
        if let Some(local) = slot.borrow().as_ref() {
            return Some(std::rc::Rc::clone(local));
        }
        let local = std::rc::Rc::new(tokio::task::LocalSet::new());
        let driver = std::rc::Rc::clone(&local);
        let guard = ExtensionLocalGuard {
            id: std::rc::Rc::as_ptr(&local) as *const () as usize,
        };
        let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::spawn_local(async move {
                let _guard = guard;
                driver.run_until(std::future::pending::<()>()).await;
            });
        }));
        if spawned.is_err() {
            return None;
        }
        *slot.borrow_mut() = Some(std::rc::Rc::clone(&local));
        Some(local)
    })
}

fn local_scheduler_error() -> kiana_ai::auth::types::AuthError {
    kiana_ai::auth::types::AuthError::new(
        "Extension OAuth requires the JavaScript engine's LocalSet",
    )
}

/// 把一个可能持有 JS 对象的 future 放到本线程常驻的 `LocalSet` 上执行，只把 `T`
/// （要求 `Send`）经 oneshot 送回调用方。
///
/// 这是 `Send` 的凭证刷新任务与 `!Send` 的 JS future 之间唯一的交界：JS 对象不跨线程。
/// 失败模式：
/// - `ensure_extension_local` 返回 `None`（没有 tokio 运行时，或当前线程未进入任何
///   `LocalSet`——两种情况下 `spawn_local` 的 panic 都被其内部的 `catch_unwind` 吞掉）→
///   立即返回错误，不执行 future；
/// - 调用方丢弃返回的 future 时，`AbortOnDrop` 取消内部任务；挂起的 oneshot 随发送端
///   销毁，接收端得到 `RecvError` 并映射为 "Extension engine is unavailable"。
fn schedule_local<T>(
    future: impl std::future::Future<Output = T> + 'static,
) -> impl std::future::Future<Output = Result<T, kiana_ai::auth::types::AuthError>> + Send + 'static
where
    T: Send + 'static,
{
    let Some(local) = ensure_extension_local() else {
        return futures::future::Either::Left(async { Err(local_scheduler_error()) });
    };
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task = local.spawn_local(async move {
        let _ = sender.send(future.await);
    });
    let abort = task.abort_handle();
    futures::future::Either::Right(async move {
        struct AbortOnDrop(tokio::task::AbortHandle);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _guard = AbortOnDrop(abort);
        receiver
            .await
            .map_err(|_| kiana_ai::auth::types::AuthError::new("Extension engine is unavailable"))
    })
}

/// Adapt a borrowed host interaction to owned channels. The JavaScript login
/// future owns only Send channel endpoints, so neither it nor its callbacks
/// borrow `ExtensionOAuthCallbacks`. Dropping the receiver aborts a pending
/// prompt if the auth operation is cancelled.
///
/// `Notify` 是单向事件；`Prompt` 需要宿主回答后登录才能继续。
///
/// `Prompt.result` 是回传宿主回答的 oneshot；宿主不再接收（登录 future 被丢弃）时发送端
/// 失败，等待中的 JS 登录得到取消错误。
enum ExtensionLoginEvent {
    Notify(kiana_ai::auth::types::AuthEvent),
    Prompt {
        request: kiana_ai::auth::types::AuthPromptRequest,
        result: tokio::sync::oneshot::Sender<Result<String, kiana_ai::auth::types::AuthError>>,
    },
}

/// 把扩展登录产生的通知/提示转交给宿主 `AuthInteraction`，直到通道关闭。
///
/// 串行处理：通知立即 `notify`，提示逐个等待宿主回答并把结果送回 oneshot。通道关闭
/// （JS 登录 future 结束或被丢弃）时循环退出。
async fn forward_login_interaction(
    interaction: &dyn kiana_ai::auth::types::AuthInteraction,
    mut events: tokio::sync::mpsc::UnboundedReceiver<ExtensionLoginEvent>,
) {
    while let Some(event) = events.recv().await {
        match event {
            ExtensionLoginEvent::Notify(event) => interaction.notify(&event),
            ExtensionLoginEvent::Prompt { request, result } => {
                let _ = result.send(interaction.prompt(request).await);
            }
        }
    }
}

/// 构造扩展登录用的 `OAuthLoginCallbacks`：每个回调只捕获无界通道的克隆，不借用宿主
/// 交互，因此可以被 `Send` 的登录/刷新任务持有。
///
/// 通道已关闭时：通知类静默丢弃（`let _ = send`），prompt 类返回
/// "Extension login was cancelled"；`signal` 原样透传，供宿主取消登录。
fn extension_login_callbacks(
    sender: tokio::sync::mpsc::UnboundedSender<ExtensionLoginEvent>,
    signal: kiana_ai::utils::abort::AbortScope,
) -> kiana_ai::compat::extension_oauth_types::OAuthLoginCallbacks {
    use kiana_ai::auth::types::{AuthError, AuthEvent, AuthPrompt, AuthPromptRequest};
    use kiana_ai::compat::extension_oauth_types::OAuthLoginCallbacks;

    fn notify(sender: &tokio::sync::mpsc::UnboundedSender<ExtensionLoginEvent>, event: AuthEvent) {
        let _ = sender.send(ExtensionLoginEvent::Notify(event));
    }

    fn prompt(
        sender: &tokio::sync::mpsc::UnboundedSender<ExtensionLoginEvent>,
        prompt: AuthPrompt,
    ) -> kiana_ai::compat::extension_oauth_types::OAuthCallbackFuture<String> {
        let (result, receiver) = tokio::sync::oneshot::channel();
        if sender
            .send(ExtensionLoginEvent::Prompt {
                request: AuthPromptRequest::new(prompt),
                result,
            })
            .is_err()
        {
            return Box::pin(async { Err(AuthError::new("Extension login was cancelled")) });
        }
        Box::pin(async move {
            receiver
                .await
                .map_err(|_| AuthError::new("Extension login was cancelled"))?
        })
    }

    let on_auth = sender.clone();
    let on_device_code = sender.clone();
    let on_prompt = sender.clone();
    let on_progress = sender.clone();
    let on_manual_code_input = sender.clone();
    let on_select = sender;
    OAuthLoginCallbacks {
        on_auth: Arc::new(move |info| {
            notify(
                &on_auth,
                AuthEvent::AuthUrl {
                    url: info.url,
                    instructions: info.instructions,
                },
            );
        }),
        on_device_code: Arc::new(move |info| {
            notify(
                &on_device_code,
                AuthEvent::DeviceCode {
                    user_code: info.user_code,
                    verification_uri: info.verification_uri,
                    open_browser_on_confirm: info.open_browser_on_confirm,
                    copy_code_on_confirm: info.copy_code_on_confirm,
                    interval_seconds: info.interval_seconds,
                    expires_in_seconds: info.expires_in_seconds,
                },
            );
        }),
        on_prompt: Arc::new(move |request| {
            prompt(
                &on_prompt,
                AuthPrompt::Text {
                    message: request.message,
                    placeholder: request.placeholder,
                },
            )
        }),
        on_progress: Some(Arc::new(move |message| {
            notify(&on_progress, AuthEvent::Progress { message });
        })),
        on_manual_code_input: Some(Arc::new(move || {
            prompt(
                &on_manual_code_input,
                AuthPrompt::ManualCode {
                    message: "Paste the authorization code".to_owned(),
                    placeholder: None,
                },
            )
        })),
        on_select: Arc::new(move |request| {
            let future = prompt(
                &on_select,
                AuthPrompt::Select {
                    message: request.message,
                    options: request
                        .options
                        .into_iter()
                        .map(|option| kiana_ai::auth::types::AuthSelectOption {
                            id: option.id,
                            label: option.label,
                            description: None,
                        })
                        .collect(),
                },
            );
            Box::pin(async move { future.await.map(Some) })
        }),
        signal: Some(signal),
    }
}

/// Adapt the public extension OAuth callbacks to the auth interaction used by kiana-config.
///
/// - `login`：每次登录在引擎线程的常驻 `LocalSet` 上调用扩展的 `login(callbacks)`
///   （`schedule_local`）；宿主交互经 `forward_login_interaction` 转发，JS 侧只持有
///   `Send` 的通道端点。`tokio::select!` 让“登录先结束”与“转发器先结束”都能收尾，后者
///   仍会等登录 future 的结果；
/// - `refresh_token`：同样经 `LocalSet` 调度；
/// - `name` / `is_subscription` / `uses_callback_server` 直接复制，
///   `get_api_key` / `modify_models` 原样转发。
///
/// 错误统一转成 `kiana_ai::auth::types::AuthError`（消息取 `to_string()`）。
pub fn extension_oauth_config(
    config: &kiana_ext_api::types::ProviderOAuthConfig,
) -> kiana_config::models::composer::ExtensionOAuthConfig {
    use kiana_config::models::composer::{ExtensionOAuthCallbacks, ExtensionOAuthConfig};

    let login = Arc::clone(&config.login);
    let refresh_token = Arc::clone(&config.refresh_token);
    ExtensionOAuthConfig {
        name: config.name.clone(),
        is_subscription: config.is_subscription,
        uses_callback_server: config.uses_callback_server,
        login: Arc::new(move |callbacks: &ExtensionOAuthCallbacks<'_>| {
            let login = Arc::clone(&login);
            let signal = callbacks.signal();
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let forwarding = forward_login_interaction(callbacks.interaction(), receiver);
            Box::pin(async move {
                let ui = extension_login_callbacks(sender, signal);
                let scheduled = schedule_local(async move { login(ui).await });
                // This future borrows the host interaction. Dropping it drops
                // the receiver, which releases pending prompt replies and the
                // JavaScript login's sender even when the caller cancels us.
                let mut forwarding = std::pin::pin!(forwarding);
                let mut scheduled = std::pin::pin!(scheduled);
                let result = tokio::select! {
                    result = scheduled.as_mut() => result,
                    () = forwarding.as_mut() => {
                        // JS can enqueue its last notification before settling;
                        // keep the scheduled result alive while the forwarder
                        // observes channel closure, then await that result.
                        scheduled.await
                    }
                };
                result?.map_err(|error| kiana_ai::auth::types::AuthError::new(error.to_string()))
            })
        }),
        refresh_token: Arc::new(move |credentials, signal| {
            let refresh_token = Arc::clone(&refresh_token);
            Box::pin(async move {
                let future =
                    schedule_local(async move { refresh_token(credentials, signal.token()).await });
                future
                    .await
                    .map_err(|error| kiana_ai::auth::types::AuthError::new(error.to_string()))?
                    .map_err(|error| kiana_ai::auth::types::AuthError::new(error.to_string()))
            })
        }),
        get_api_key: Arc::clone(&config.get_api_key),
        modify_models: config.modify_models.clone(),
    }
}

/// `kiana-ext-api::ProviderConfig` → `kiana-config::ProviderConfigInput`。
///
/// 字段映射：
/// - `api`：`Api` → `to_string()`；
/// - `stream_simple` / `refresh_models` / `oauth` 是扩展提供的闭包，这里重新包成
///   `kiana-config` 的函数对象（`ExtensionStreamSimpleFn` / `ExtensionRefreshModelsFn` /
///   `ExtensionOAuthConfig`）；`refresh_models` 的错误统一压成 `AiErrorKind::Other`；
/// - `images_defined` / `classifiers_defined` 记录原 `Option` 是否 `Some`，用于区分
///   “显式空表”与“未提供”；
/// - `models` 逐项经 `provider_model_config` 转换。
///
/// 纯数据映射，不执行注册；注册由 `bind_core` 或调用方负责。
pub fn provider_config_input(config: &ProviderConfig) -> ProviderConfigInput {
    ProviderConfigInput {
        name: config.name.clone(),
        base_url: config.base_url.clone(),
        api_key: config.api_key.clone(),
        api: config.api.as_ref().map(|api| api.to_string()),
        stream_simple: config.stream_simple.as_ref().map(|stream| {
            let stream = Arc::clone(stream);
            let simple: Arc<kiana_config::models::composer::ExtensionStreamSimpleFn> = Arc::new(
                move |model: &kiana_ai::types::Model,
                      context: &kiana_ai::types::TranscriptContext,
                      options: kiana_ai::types::SimpleStreamOptions|
                      -> kiana_ai::event_stream::AssistantMessageEventStream {
                    stream(model, context, options)
                },
            );
            simple
        }),
        images: config.images.clone().unwrap_or_default(),
        images_defined: config.images.is_some(),
        classifiers: config.classifiers.clone().unwrap_or_default(),
        classifiers_defined: config.classifiers.is_some(),
        headers: config.headers.clone(),
        auth_header: config.auth_header,
        oauth: config.oauth.as_ref().map(extension_oauth_config),
        models: config
            .models
            .as_ref()
            .map(|models| models.iter().map(provider_model_config).collect::<Vec<_>>()),
        refresh_models: config.refresh_models.as_ref().map(|refresh| {
            let refresh = Arc::clone(refresh);
            let future_fn = move |context: kiana_ai::models::RefreshModelsContext| {
                let refresh = Arc::clone(&refresh);
                let future: kiana_ext_api::BoxFuture<
                    'static,
                    Result<
                        Vec<kiana_config::models::composer::ProviderModelConfig>,
                        kiana_ai::error::AiError,
                    >,
                > = Box::pin(async move {
                    refresh(context)
                        .await
                        .map(|models| models.iter().map(provider_model_config).collect())
                        .map_err(|error| {
                            kiana_ai::error::AiError::new(
                                kiana_ai::error::AiErrorKind::Other,
                                error.to_string(),
                            )
                        })
                });
                future
            };
            Arc::new(future_fn) as Arc<kiana_config::models::composer::ExtensionRefreshModelsFn>
        }),
    }
}

/// 单个模型配置的映射（Chat / Image / Classifier 三个分支）。
///
/// `context_window` / `max_tokens` 在 `kiana-config` 里是 `f64`，这里做 `as f64` 转换；
/// Chat 的 `sampling_params_by_thinking_level` 与 Classifier 的 `compat` 没有对应的扩展
/// API 字段，固定为 `None`；`declared_type` 由分支决定。
fn provider_model_config(
    model: &kiana_ext_api::ProviderModelConfig,
) -> kiana_config::models::composer::ProviderModelConfig {
    use kiana_config::models::composer::{
        ProviderChatModelConfig, ProviderClassifierModelConfig, ProviderImageModelConfig,
        ProviderModelConfigBase,
    };
    #[allow(clippy::too_many_arguments)]
    fn base(
        id: &str,
        name: &str,
        api: &Option<kiana_ai::types::Api>,
        base_url: &Option<String>,
        input: &[kiana_ai::types::ModelInputModality],
        input_limits: &Option<kiana_ai::types::ModelInputLimits>,
        cost: &kiana_ai::types::ModelCost,
        headers: &Option<IndexMap<String, String>>,
        model_type: kiana_ai::types::ModelType,
    ) -> ProviderModelConfigBase {
        ProviderModelConfigBase {
            id: id.to_owned(),
            name: name.to_owned(),
            api: api.as_ref().map(|api| api.to_string()),
            base_url: base_url.clone(),
            input: input.to_vec(),
            input_limits: input_limits.clone(),
            cost: cost.clone(),
            headers: headers.clone(),
            declared_type: Some(model_type),
        }
    }
    match model {
        kiana_ext_api::ProviderModelConfig::Chat(chat) => {
            kiana_config::models::composer::ProviderModelConfig::Chat(ProviderChatModelConfig {
                base: base(
                    &chat.id,
                    &chat.name,
                    &chat.api,
                    &chat.base_url,
                    &chat.input,
                    &chat.input_limits,
                    &chat.cost,
                    &chat.headers,
                    kiana_ai::types::ModelType::Chat,
                ),
                reasoning: chat.reasoning,
                thinking_level_map: chat.thinking_level_map.clone(),
                prompt_cache: chat.prompt_cache.clone(),
                context_window: chat.context_window as f64,
                max_tokens: chat.max_tokens as f64,
                sampling_params: chat.sampling_params.clone(),
                sampling_params_by_thinking_level: None,
                compat: chat.compat.clone(),
            })
        }
        kiana_ext_api::ProviderModelConfig::Image(image) => {
            kiana_config::models::composer::ProviderModelConfig::Image(ProviderImageModelConfig {
                base: base(
                    &image.id,
                    &image.name,
                    &image.api,
                    &image.base_url,
                    &image.input,
                    &image.input_limits,
                    &image.cost,
                    &image.headers,
                    kiana_ai::types::ModelType::Image,
                ),
                output: image.output.clone(),
            })
        }
        kiana_ext_api::ProviderModelConfig::Classifier(classifier) => {
            kiana_config::models::composer::ProviderModelConfig::Classifier(
                ProviderClassifierModelConfig {
                    base: base(
                        &classifier.id,
                        &classifier.name,
                        &classifier.api,
                        &classifier.base_url,
                        &classifier.input,
                        &classifier.input_limits,
                        &classifier.cost,
                        &classifier.headers,
                        kiana_ai::types::ModelType::Classifier,
                    ),
                    context_window: classifier.context_window as f64,
                    compat: None,
                },
            )
        }
    }
}

/// 安装 `js/dispatch.js` 与 `__kiana_dispatch_host`（[`dispatch::install`] 的再导出）。
///
/// 由 crate 装配器在 `Generation::with_config` 第 8 步调用；同一引擎重复调用是 no-op，
/// 只把该引擎重新登记为当前引擎（`Weak` 登记，不延长引擎生命周期）。
pub use self::dispatch::install as install_dispatch;

#[cfg(test)]
mod cwd_override_tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[tokio::test]
    async fn cwd_override_is_scoped_to_the_future() {
        let scoped = with_cwd_override(PathBuf::from("/worktree"), async { scoped_cwd() }).await;
        assert_eq!(scoped.as_deref(), Some(Path::new("/worktree")));
        assert_eq!(scoped_cwd(), None, "override must not leak past the scope");

        let nested = with_cwd_override(PathBuf::from("/outer"), async {
            let outer = scoped_cwd();
            let inner = with_cwd_override(PathBuf::from("/inner"), async { scoped_cwd() }).await;
            (outer, inner)
        })
        .await;
        assert_eq!(nested.0.as_deref(), Some(Path::new("/outer")));
        assert_eq!(nested.1.as_deref(), Some(Path::new("/inner")));
    }
}
