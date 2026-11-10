//! core/extensions/types.ts（2308 行，唯一权威）。
//!
//! TS 源：`core/extensions/types.ts`。本模块包含其余全部声明：
//! 扩展上下文 trait（[`ExtensionContextApi`] 等）、渲染器、命令注册、
//! [`ExtensionApi`]、provider 注册类型、加载产物与 runtime/actions 结构。
//!
//! # Rust 形态
//!
//! - TS 的行为接口变成对象安全 trait（[`ExtensionContextApi`]、[`ExtensionToolContextApi`]、
//!   [`ExtensionCommandContextApi`]、[`ReplacedSessionContextApi`]、[`ExtensionApi`]）；
//! - TS 的泛型方法全部擦除（trait 方法不能有泛型参数）：`sendMessage<T>` →
//!   [`ExtensionApi::send_message`] 接收 [`SendMessageOptions`]；`appendEntry<T>` →
//!   [`ExtensionApi::append_entry`] 接收 [`JsonValue`]；
//! - `registerProvider` 的两个重载折叠为**一个**方法名，参数是 [`ProviderRegistration`]；
//! - `AbortSignal` 一律是 [`CancellationToken`]（`ARCHITECTURE.md` §4.2）。
//!
//! # 在系统中的位置
//!
//! 本文件只声明契约，执行逻辑极少（例如 `ProviderModelConfig` 的 serde 分派、各 `Debug` 实现
//! 与两个默认方法）。上游（实现方）：`kiana-extensions` 的 loader/runner 实现
//! [`ExtensionApi`] 与 [`ExtensionContextApi`] 等上下文，并填充 [`ExtensionRuntime`] 里的闭包；
//! `kiana-session` 构造 [`ExtensionActions`] 与 [`ExtensionContextActions`]，把动作接到真实
//! 会话与模型运行时上。下游（使用方，逐个核对 `crates/*/Cargo.toml` 的直接依赖）：
//! `kiana-export`、`kiana-tools`、`kiana-extensions`、`kiana-builtin-extensions`（引入
//! [`InlineExtension`] / [`ExtensionFactory`] / [`ExtensionContextApi`]）、`kiana-resources`、
//! `kiana-session`、`kiana-interactive`、`kiana-rpc-print`、`kiana-coding-agent`，都从本 crate
//! 引入这些名字（crate 根经 `index.rs` 的 `pub use` 再导出；未再导出的项走 `types::` 路径），
//! 不得复制第二份定义（`ARCHITECTURE.md` §3.4/§3.5/§4.9）。
//!
//! # 一次加载里各类型的协作顺序
//!
//! 1. loader 先创建一个 [`ExtensionRuntime`]（状态字段 + 动作闭包）作为所有扩展共享的句柄；
//! 2. 每个扩展的工厂函数收到 [`ExtensionApi`]，在注册期调用 `on` / `register_tool` /
//!    `register_command` 等方法；
//! 3. 注册结果汇总到该扩展的 [`Extension`]：工具、命令、flag、handler 等集合都是 `IndexMap`，
//!    插入顺序即注册顺序；失败与警告分别进 `LoadExtensionsResult::errors` 与
//!    `LoadExtensionsResult::warnings`；
//! 4. 运行期 runner 把 [`ExtensionContextApi`]（工具里是 [`ExtensionToolContextApi`]、命令里是
//!    [`ExtensionCommandContextApi`]）交给扩展读取会话状态；
//! 5. 扩展通过 [`ExtensionApi`] 的动作方法或 [`ExtensionActions`] 里的闭包反过来改变会话。
//!
//! # 设计原因
//!
//! - **trait + `Arc<dyn Fn ...>`**：TS 的 interface 与函数类型在 Rust 里没有直接对应物；这些值
//!   要被多个模块共享、且生命周期跨越一次加载，因此统一用 trait 对象或 `Arc` 回调，而不是
//!   借用或泛型参数。
//! - **手写 `Debug`**：结构体里含 `Arc<dyn Fn ...>` 与 trait 对象，不能 derive `Debug`；多数
//!   `impl Debug` 只打印可打印字段，并用 `finish_non_exhaustive`（或 `{ .. }` 文本）表示省略；
//!   `ProviderRegistration::Config` 与 `InlineExtension::Named` 目前只用 `finish()` 输出部分
//!   字段，没有省略标记。
//! - **`serde` 属性即线上契约**：`rename_all = "camelCase"`、逐字段 `rename`、
//!   `skip_serializing_if` 与 `default` 决定扩展看到的 JSON 键名与可选性；改动这些属性会改变
//!   线上格式（TS 源固定为 `core/extensions/types.ts`）。
//!
//! # 注意事项
//!
//! - **`Option` 不等于“总是省略键”**：写 `null` 还是省略键由字段自己的 `skip_serializing_if`
//!   决定。例如 `ContextUsage::tokens` 未知时写出 `null`，而 `ExtensionError::stack` 为
//!   `None` 时整个键不出现。
//! - **[`ExtensionRuntime`] 的字段是快照**：`kiana-extensions` 把可变状态放在自己的共享结构里，
//!   `Arc<ExtensionRuntime>` 上的 `flag_values` / `pending_*` 是构造时的值；读实时状态要用动作
//!   闭包或该 crate 的 `runtime_*` 访问器（见 `kiana-extensions` 的 loader 模块文档）。
//! - **两种 future 的边界**：返回 [`BoxFuture`] 的回调可以跨线程；返回 [`LocalBoxFuture`] 的
//!   回调只能在单线程 `LocalSet` 上轮询（`lib.rs` 有完整说明）。不要把 `LocalBoxFuture` 交给
//!   要求 `Send` 的调用方。
//!
//! # 阅读顺序
//!
//! [`ExtensionContextApi`]（扩展能读到什么）→ [`ExtensionApi`]（扩展能做什么）→ provider 注册
//! （[`ProviderRegistration`]、[`ProviderModelConfig`]）→ 加载产物（[`Extension`]、
//! [`LoadExtensionsResult`]）→ 文件末尾的 Rust-only helper（[`SendMessageOptions`]、
//! [`NewSessionOptions`] 等）。

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::{AgentTool, AgentToolCallOutcome, AgentToolUpdateCallback, ThinkingLevel};
use kiana_ai::auth::types::OAuthCredentials;
use kiana_ai::compat::ApiStreamSimpleFunction;
use kiana_ai::compat::extension_oauth_types::OAuthLoginCallbacks;
use kiana_ai::models::RefreshModelsContext;
use kiana_ai::models::provider::{Provider, ProviderClassifier, ProviderImages};
use kiana_ai::types::{
    Api, JsonValue, Model, ModelCost, ModelInputLimits, ModelInputModality, ModelPromptCache,
    SamplingParams, ThinkingLevelMap, UserContent,
};
use kiana_compaction::CompactionResult;
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::resolver::ScopedModel;
use kiana_config::settings::Settings;
use kiana_core::event_bus::{EventBus, Unsubscribe};
use kiana_core::mcp_servers::{McpServerConfig, McpServerRegistry, RegisteredMcpServer};
use kiana_core::slash_commands::SlashCommandInfo;
use kiana_core::source_info::SourceInfo;
use kiana_session_core::messages::CustomMessage;
use kiana_session_core::session_manager::{CustomEntry, ReadonlySessionManager, SessionManager};
use kiana_session_core::virtual_models::{ModelRouteRequest, RouteFuture, VirtualModelDefinition};
use kiana_theme::theme::Theme;
use kiana_tui::autocomplete::AutocompleteItem;
use kiana_tui::tui::Component;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::events::{ExtensionEventKind, ExtensionHandler};
use crate::js_number;
use crate::system_prompt::BuildSystemPromptOptions;
use crate::tool_result::{ExecOptions, ExecResult};
use crate::tool_types::{
    ToolAnnotations, ToolDefinition, ToolExposure, ToolNamespace, ToolRendererResolver,
};
use crate::ui::ExtensionUiContext;
use crate::{BoxFuture, LocalBoxFuture};

// ============================================================================
// ExtensionContext 相关
// ============================================================================

/// 当前上下文用量。TS: `types.ts#ContextUsage`。
///
/// `tokens`/`percent` 是**必填键** `number | null`：未知时写出 `null`（不加 skip）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    /// 估算的上下文 token 数；未知时 `None`（写出 `null`）。
    pub tokens: Option<u64>,
    /// 上下文窗口大小。
    pub context_window: u64,
    /// 占上下文窗口的百分比；`tokens` 未知时 `None`（写出 `null`）。
    ///
    /// TS 是 `number | null`（`tokens / contextWindow * 100`，通常是小数）；按 JS
    /// `JSON.stringify` 的文本写出（[`kiana_json::js_num::format_js_number`]）。
    #[serde(serialize_with = "js_number::serialize_option")]
    pub percent: Option<f64>,
}

/// 触发压缩的选项。TS: `types.ts#CompactOptions`。
///
/// `compact()` 本身返回 `void`（触发后不等待完成）；回调在后台任务里执行。
#[derive(Default)]
pub struct CompactOptions {
    /// 自定义指令。
    pub custom_instructions: Option<String>,
    /// 压缩成功回调。
    pub on_complete: Option<Arc<dyn Fn(CompactionResult) + Send + Sync>>,
    /// 压缩失败回调（TS 的 `Error` 用 boxed error 表达）。
    pub on_error: Option<Arc<dyn Fn(Box<dyn std::error::Error + Send + Sync>) + Send + Sync>>,
}

impl std::fmt::Debug for CompactOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompactOptions")
            .field("custom_instructions", &self.custom_instructions)
            .field("on_complete", &self.on_complete.is_some())
            .field("on_error", &self.on_error.is_some())
            .finish()
    }
}

/// 当前运行模式。TS: `types.ts#ExtensionMode`（`"tui" | "rpc" | "json" | "print"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionMode {
    /// `"tui"`。
    #[default]
    Tui,
    /// `"rpc"`。
    Rpc,
    /// `"json"`。
    Json,
    /// `"print"`。
    Print,
}

/// [`ExtensionToolContextApi::execute_tool`] 的选项。
///
/// TS: `types.ts#ExecuteToolOptions`。`signal` 缺省取调用工具的 signal；`on_update` 接收
/// 嵌套工具的部分结果。
#[derive(Default)]
pub struct ExecuteToolOptions {
    /// 取消信号；缺省用调用工具的 signal。
    pub signal: Option<CancellationToken>,
    /// 部分结果回调。
    pub on_update: Option<AgentToolUpdateCallback>,
}

impl std::fmt::Debug for ExecuteToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecuteToolOptions")
            .field("signal", &self.signal)
            .field("on_update", &self.on_update.is_some())
            .finish()
    }
}

/// 事件处理器与工具执行上下文。
///
/// TS: `types.ts#ExtensionContext`（18 个成员）。所有值都是**读时求值**（TS 的 getter）：
/// `ctx.signal`、`ctx.model`、`ctx.getSystemPrompt()` 在调用时读取当前状态。
///
/// 对象安全；`kiana-extensions` 用 `Arc<dyn ExtensionContextApi>` 传递。
///
/// # 实现约定
///
/// - 可变状态（`model`、`signal`、`thinking_level`、`is_idle`、`get_system_prompt` 等）必须在
///   调用时读取（TS 的 getter 语义）；`ui`/`cwd`/`session_manager`/`model_registry` 因返回
///   借用，`kiana-extensions` 在构造上下文时捕获（该 crate 记录的已知差异，见
///   `runner/context.rs` 的 `build` 注释）。`kiana-extensions` 的 runner 还会在访问前检查实例
///   是否已被替换，陈旧实例的访问会 panic（对应 TS 的 throw，见该 crate 的
///   `runner/context.rs`）。
/// - `ui()`、`session_manager()`、`model_registry()` 返回借用，生命周期跟随 `&self`；实现方
///   必须保证底层对象至少与上下文活得一样久。
pub trait ExtensionContextApi: Send + Sync {
    /// 交互式 UI 上下文。TS: `ui`。
    fn ui(&self) -> &dyn ExtensionUiContext;
    /// 当前运行模式。TS: `mode`。
    fn mode(&self) -> ExtensionMode;
    /// 是否有可用的对话框 UI（TUI 与 RPC 为 true）。TS: `hasUI`。
    fn has_ui(&self) -> bool;
    /// 当前工作目录。TS: `cwd`（字符串 → `Path` 的边界）。
    fn cwd(&self) -> &std::path::Path;
    /// 只读会话管理器。TS: `sessionManager`。
    fn session_manager(&self) -> &dyn ReadonlySessionManager;
    /// 模型注册表。TS: `modelRegistry`。
    fn model_registry(&self) -> &ModelRegistry;
    /// 当前模型。TS: `model`（`Model<any> | undefined`）。
    fn model(&self) -> Option<Model>;
    /// 本会话作用域内的模型（只读快照）。TS: `scopedModels`。
    fn scoped_models(&self) -> Vec<ScopedModel>;
    /// 当前 thinking level。TS: `thinkingLevel?`。
    fn thinking_level(&self) -> Option<ThinkingLevel>;
    /// agent 是否空闲。TS: `isIdle()`。
    fn is_idle(&self) -> bool;
    /// 项目本地信任是否生效。TS: `isProjectTrusted()`。
    fn is_project_trusted(&self) -> bool;
    /// 当前中止信号；非流式时为 `None`。TS: `signal`。
    fn signal(&self) -> Option<CancellationToken>;
    /// 中止当前 agent 操作。TS: `abort()`。
    fn abort(&self);
    /// 是否有排队中的消息。TS: `hasPendingMessages()`。
    fn has_pending_messages(&self) -> bool;
    /// 优雅关闭 kiana 并退出。TS: `shutdown()`。
    fn shutdown(&self);
    /// 当前上下文用量。TS: `getContextUsage()`。
    fn get_context_usage(&self) -> Option<ContextUsage>;
    /// 触发压缩（不等待完成）。TS: `compact(options?)`。
    fn compact(&self, options: Option<CompactOptions>);
    /// 当前生效的系统提示。TS: `getSystemPrompt()`。
    fn get_system_prompt(&self) -> String;
}

/// 工具 `execute()` 得到的上下文：扩展上下文 + `executeTool()`。
///
/// TS: `types.ts#ExtensionToolContext`（extends [`ExtensionContextApi`]）。
pub trait ExtensionToolContextApi: ExtensionContextApi {
    /// [`Self::execute_tool`] 可以调用的工具。TS: `readonly tools`。
    fn tools(&self) -> Vec<AgentTool>;

    /// 通过同样的校验/hook/权限检查运行另一个工具。
    ///
    /// TS: `executeTool(name, args, options?) => Promise<AgentToolCallOutcome>`。
    /// 调用 id 为 `<calling id>/<n>`；对工具失败**永不 reject**（未知工具、校验错误、被阻止、
    /// 抛出的错误都以 `isError: true` 返回）。返回 [`BoxFuture`]（`Send`）：会跨进
    /// `kiana-tools`/`kiana-session` 的 async 管线。
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        args: JsonValue,
        options: Option<ExecuteToolOptions>,
    ) -> BoxFuture<'a, AgentToolCallOutcome>;
}

/// 命令 handler 的上下文（只允许在用户发起的命令里使用会话控制方法）。
///
/// TS: `types.ts#ExtensionCommandContext`（extends [`ExtensionContextApi`]）。
pub trait ExtensionCommandContextApi: ExtensionContextApi {
    /// 当前基础系统提示选项。TS: `getSystemPromptOptions()`。
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions;

    /// 等待 agent 结束流式。TS: `waitForIdle()`。
    fn wait_for_idle(&self) -> LocalBoxFuture<'static, ()>;

    /// 开始新会话。TS: `newSession(options?) => Promise<{cancelled}>`。
    fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult>;

    /// 从指定条目 fork。TS: `fork(entryId, options?) => Promise<{cancelled}>`。
    fn fork<'a>(
        &'a self,
        entry_id: &'a str,
        options: Option<ForkOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult>;

    /// 导航到会话树的另一个点。TS: `navigateTree(targetId, options?) => Promise<{cancelled}>`。
    fn navigate_tree<'a>(
        &'a self,
        target_id: &'a str,
        options: Option<NavigateTreeOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult>;

    /// 切换到另一个会话文件。TS: `switchSession(sessionPath, options?) => Promise<{cancelled}>`。
    fn switch_session<'a>(
        &'a self,
        session_path: &'a str,
        options: Option<SwitchSessionOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult>;

    /// 重新加载扩展、技能、prompt、主题与上下文文件。TS: `reload()`。
    fn reload(&self) -> LocalBoxFuture<'static, ()>;
}

/// 会话切换后绑定到新会话的命令上下文。
///
/// TS: `types.ts#ReplacedSessionContext`（extends [`ExtensionCommandContextApi`]）。
/// 与 [`ExtensionApi::send_message`] 的区别：这里返回 `Promise<void>`（异步），
/// API 版本返回 `void`（同步 fire-and-forget）。
pub trait ReplacedSessionContextApi: ExtensionCommandContextApi {
    /// 向会话发送自定义消息。TS: `sendMessage(message, options?) => Promise<void>`。
    fn send_message(&self, message: SendMessageOptions) -> LocalBoxFuture<'static, ()>;

    /// 向 agent 发送用户消息（总是触发一个 turn）。
    /// TS: `sendUserMessage(content, options?) => Promise<void>`。
    fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> LocalBoxFuture<'static, ()>;

    /// 带错误通道的 [`Self::send_message`]（TS 的 `Promise<void>` rejection）。
    ///
    /// 冻结的 [`Self::send_message`] 没有错误通道，而 `createReplacedSessionContext` 直接把
    /// 会话方法挂到 `ctx.sendMessage` 上，失败会 reject 扩展里等待的 Promise。默认实现保持
    /// 旧行为（丢弃错误）；会话实现覆盖它把错误交给扩展桥。
    fn send_message_checked(
        &self,
        message: SendMessageOptions,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        let future = self.send_message(message);
        Box::pin(async move {
            future.await;
            Ok(())
        })
    }

    /// 带错误通道的 [`Self::send_user_message`]（TS 的 `Promise<void>` rejection）。
    fn send_user_message_checked(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        let future = self.send_user_message(content, options);
        Box::pin(async move {
            future.await;
            Ok(())
        })
    }
}

// ============================================================================
// 渲染
// ============================================================================

/// 消息渲染选项。TS: `types.ts#MessageRenderOptions`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRenderOptions {
    /// 消息视图是否展开。
    pub expanded: bool,
    /// `outputPad` 设置的水平内缩。
    pub output_pad: usize,
}

/// markdown 变换时的消息类型（Rust-only helper）。
///
/// TS: `types.ts#MarkdownTransformContext.messageType`
/// （`"user" | "assistant" | "assistant-thinking"`）。`assistant-thinking` 是多词字面量，
/// 逐变体 rename（`rename_all = "lowercase"` 会得到 `assistantthinking`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MarkdownMessageType {
    /// `"user"`。
    #[default]
    #[serde(rename = "user")]
    User,
    /// `"assistant"`。
    #[serde(rename = "assistant")]
    Assistant,
    /// `"assistant-thinking"`。
    #[serde(rename = "assistant-thinking")]
    AssistantThinking,
}

/// markdown 变换上下文。TS: `types.ts#MarkdownTransformContext`。
///
/// `timestamp` 是 ISO-8601 字符串（`toISOString()` 形态：3 位小数 + `Z`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownTransformContext {
    /// 消息类型。
    pub message_type: MarkdownMessageType,
    /// 是否正在流式。
    pub is_streaming: bool,
    /// 可用宽度。
    pub available_width: usize,
    /// 持久会话条目 id（实时消息没有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// 持久会话条目的 ISO 时间戳（实时消息没有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// markdown 变换器。TS: `types.ts#MarkdownTransformer`。
///
/// 多个变换器按扩展加载顺序串联：前一个的输出是后一个的输入（`kiana-interactive` 的
/// `apply_markdown_transformers`）；某个变换器 panic 会被静默跳过，链继续。
pub type MarkdownTransformer = Arc<dyn Fn(&str, &MarkdownTransformContext) -> String + Send + Sync>;

/// 条目渲染选项。TS: `types.ts#EntryRenderOptions`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRenderOptions {
    /// 条目视图是否展开。
    pub expanded: bool,
}

/// 自定义消息渲染器。TS: `types.ts#MessageRenderer<T>`。
///
/// `T` 擦除为 `kiana-session-core` 的 [`CustomMessage`]（`details` 是任意 JSON）。
///
/// 返回 `None` 表示不自定义渲染：`kiana-interactive` 的 `CustomMessageComponent` 会退回内置
/// 消息框；渲染器 panic 也被捕获并走同一条回退路径。
pub type MessageRenderer = Arc<
    dyn Fn(&CustomMessage, &MessageRenderOptions, &Theme) -> Option<Box<dyn Component>>
        + Send
        + Sync,
>;

/// 自定义条目渲染器。TS: `types.ts#EntryRenderer<T>`。
///
/// 返回 `None` 表示该条目不渲染（`CustomEntryComponent` 视为无内容，调用方跳过）；panic 会
/// 显示为 `[customType] renderer failed: ...` 错误框（`customType` 处为条目类型名）。
pub type EntryRenderer = Arc<
    dyn Fn(&CustomEntry, &EntryRenderOptions, &Theme) -> Option<Box<dyn Component>> + Send + Sync,
>;

// ============================================================================
// 命令注册
// ============================================================================

/// 命令的参数补全回调。
///
/// TS: `getArgumentCompletions?: (prefix) => AutocompleteItem[] | null | Promise<...>`。
/// 返回 `None` 对应 TS 的 `null`。
pub type ArgumentCompletionsFn =
    Arc<dyn Fn(String) -> LocalBoxFuture<'static, Option<Vec<AutocompleteItem>>> + Send + Sync>;

/// 命令处理器。TS: `(args: string, ctx: ExtensionCommandContext) => Promise<void>`。
pub type CommandHandler = Arc<
    dyn Fn(String, Arc<dyn ExtensionCommandContextApi>) -> LocalBoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// 快捷键处理器。TS: `(ctx: ExtensionContext) => Promise<void> | void`。
pub type ShortcutHandler =
    Arc<dyn Fn(Arc<dyn ExtensionContextApi>) -> LocalBoxFuture<'static, ()> + Send + Sync>;

/// 已注册的命令。TS: `types.ts#RegisteredCommand`。
#[derive(Clone)]
pub struct RegisteredCommand {
    /// 命令名（不含 `/`）。
    pub name: String,
    /// 定义该命令的资源的来源信息。
    pub source_info: SourceInfo,
    /// 描述。
    pub description: Option<String>,
    /// 参数补全。
    pub get_argument_completions: Option<ArgumentCompletionsFn>,
    /// 处理器。
    pub handler: CommandHandler,
}

impl std::fmt::Debug for RegisteredCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredCommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// 解析后的命令：基类字段 + `invocationName`。TS: `types.ts#ResolvedCommand`。
#[derive(Clone)]
pub struct ResolvedCommand {
    /// 命令名（不含 `/`）。
    pub name: String,
    /// 定义该命令的资源的来源信息。
    pub source_info: SourceInfo,
    /// 描述。
    pub description: Option<String>,
    /// 参数补全。
    pub get_argument_completions: Option<ArgumentCompletionsFn>,
    /// 处理器。
    pub handler: CommandHandler,
    /// 调用名（可能与注册名不同）。
    ///
    /// 重名命令会带序号后缀（`name:1`、`name:2` …），由 `kiana-extensions` 的
    /// `resolve_commands` 计算并保证唯一。
    pub invocation_name: String,
}

impl std::fmt::Debug for ResolvedCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedCommand")
            .field("name", &self.name)
            .field("invocation_name", &self.invocation_name)
            .finish_non_exhaustive()
    }
}

/// `registerCommand` 的选项（`Omit<RegisteredCommand, "name" | "sourceInfo">`）。
#[derive(Clone)]
pub struct RegisterCommandOptions {
    /// 描述。
    pub description: Option<String>,
    /// 参数补全。
    pub get_argument_completions: Option<ArgumentCompletionsFn>,
    /// 处理器。
    pub handler: CommandHandler,
}

impl std::fmt::Debug for RegisterCommandOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisterCommandOptions")
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// `registerShortcut` 的选项。
///
/// TS: `types.ts#ExtensionAPI.registerShortcut` 的第 2 个参数
/// `{ description?: string; handler: (ctx) => Promise<void> | void }`。
#[derive(Clone)]
pub struct RegisterShortcutOptions {
    /// 描述。
    pub description: Option<String>,
    /// 处理器。
    pub handler: ShortcutHandler,
}

impl std::fmt::Debug for RegisterShortcutOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisterShortcutOptions")
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// `registerFlag` 的选项。
///
/// TS: `types.ts#ExtensionAPI.registerFlag` 的第 2 个参数（boolean/string 两态的联合）。
pub enum RegisterFlagOptions {
    /// `type: "boolean"`。
    Boolean {
        /// 描述。
        description: Option<String>,
        /// 默认值。
        default: Option<bool>,
    },
    /// `type: "string"`。
    String {
        /// 描述。
        description: Option<String>,
        /// 默认值。
        default: Option<String>,
    },
}

impl std::fmt::Debug for RegisterFlagOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterFlagOptions::Boolean {
                description,
                default,
            } => formatter
                .debug_struct("Boolean")
                .field("description", description)
                .field("default", default)
                .finish(),
            RegisterFlagOptions::String {
                description,
                default,
            } => formatter
                .debug_struct("String")
                .field("description", description)
                .field("default", default)
                .finish(),
        }
    }
}

// ============================================================================
// ExtensionAPI
// ============================================================================

/// ExtensionAPI：传给扩展工厂函数的 API。
///
/// TS: `types.ts#ExtensionAPI`（41 个 `on()` 重载 + 31 个非 `on` 方法名 + `events` 字段）。
///
/// 无 `Send`/`Sync` 超 trait：`kiana-extensions` 在单线程 `LocalSet` 上实现它，并持有 rquickjs 的
/// `!Send` 句柄。对象安全：所有泛型方法都已擦除。
///
/// # 调用语义
///
/// - 方法都没有 `Result`：调用失败由实现方处理。`kiana-extensions` 的 loader 把每个扩展的注册
///   放在 loading/active/failed 三态事务里，失败记入 `LoadExtensionsResult::errors`；实例
///   失效后继续调用会报错。
/// - [`Self::on`] 返回的 [`Unsubscribe`] 用于移除该 handler；加载期建立的订阅会在该扩展加载
///   失败、被丢弃时由实现方统一退订。
/// - 运行期动作（`send_message`、`append_entry`、`set_session_name` 等）只有 runtime 已绑定
///   动作后才真正生效；未绑定时实现方以抛错桩报告（`kiana-extensions` 的 loader 有两个例外：
///   runtime 动作字段 `refresh_tools` 为空操作、`set_model` 返回等价的失败结果）。
///   [`Self::exec`] 例外：实现里直接调 `kiana_core::exec_command`（`spawn_blocking`），只做
///   `ensure_active`，不经过 [`ExtensionActions`]/runtime 动作绑定，因此加载期调用同样可以执行。
pub trait ExtensionApi {
    // --- 事件订阅 -----------------------------------------------------------

    /// 订阅一个事件；返回退订闭包。
    ///
    /// TS 有 41 个 `on()` 重载（每个事件名一个），Rust 折叠为单个方法，事件名用
    /// [`ExtensionEventKind`] 表达，处理器用 [`ExtensionHandler`] 区分标准/项目信任两种形态。
    fn on(&self, kind: ExtensionEventKind, handler: ExtensionHandler) -> Unsubscribe;

    // --- 工具注册 -----------------------------------------------------------

    /// 注册一个 LLM 可调用的工具。TS: `registerTool<TParams,TDetails,TState>(tool)`。
    fn register_tool(&self, tool: ToolDefinition);

    // --- 命令 / 快捷键 / flag ----------------------------------------------

    /// 注册自定义命令。TS: `registerCommand(name, options)`。
    fn register_command(&self, name: &str, options: RegisterCommandOptions);

    /// 注册键盘快捷键。TS: `registerShortcut(shortcut: KeyId, options)`。
    fn register_shortcut(&self, shortcut: &str, options: RegisterShortcutOptions);

    /// 注册 CLI flag。TS: `registerFlag(name, options)`。
    fn register_flag(&self, name: &str, options: RegisterFlagOptions);

    /// 读取已注册 flag 的值。TS: `getFlag(name)`。
    fn get_flag(&self, name: &str) -> Option<FlagValue>;

    // --- 消息渲染 -----------------------------------------------------------

    /// 为 `CustomMessageEntry` 注册自定义渲染器。TS: `registerMessageRenderer<T>(customType, renderer)`。
    fn register_message_renderer(&self, custom_type: &str, renderer: MessageRenderer);

    /// 注册 markdown 变换器（单值，后写覆盖）。TS: `registerMarkdownTransformer(transformer)`。
    fn register_markdown_transformer(&self, transformer: MarkdownTransformer);

    /// 为 `CustomEntry` 注册自定义渲染器。TS: `registerEntryRenderer<T>(customType, renderer)`。
    fn register_entry_renderer(&self, custom_type: &str, renderer: EntryRenderer);

    /// 选择工具调用的绘制方式。TS: `registerToolRenderer(resolver)`。
    fn register_tool_renderer(&self, resolver: ToolRendererResolver);

    // --- 动作 ---------------------------------------------------------------

    /// 向会话发送自定义消息。TS: `sendMessage<T>(message, options?)`（返回 `void`）。
    fn send_message(&self, options: SendMessageOptions);

    /// 向 agent 发送用户消息（总是触发一个 turn）。
    /// TS: `sendUserMessage(content, options?)`（返回 `void`）。
    fn send_user_message(&self, content: UserContent, options: Option<SendUserMessageOptions>);

    /// 追加一条自定义条目用于状态持久化（不发给 LLM）。
    /// TS: `appendEntry<T>(customType, data?)`。
    fn append_entry(&self, custom_type: &str, data: Option<JsonValue>);

    // --- 会话元数据 ---------------------------------------------------------

    /// 设置会话显示名。TS: `setSessionName(name)`。
    fn set_session_name(&self, name: &str);

    /// 当前会话名。TS: `getSessionName()`。
    fn get_session_name(&self) -> Option<String>;

    /// 设置/清除条目标签。TS: `setLabel(entryId, label)`。
    fn set_label(&self, entry_id: &str, label: Option<&str>);

    /// 执行 shell 命令。TS: `exec(command, args, options?) => Promise<ExecResult>`。
    fn exec<'a>(
        &'a self,
        command: &'a str,
        args: &'a [String],
        options: Option<ExecOptions>,
    ) -> LocalBoxFuture<'a, ExecResult>;

    /// 当前 active（声明给模型）的工具名。TS: `getActiveTools()`。
    fn get_active_tools(&self) -> Vec<String>;

    /// 全部已配置工具（含参数 schema、guidelines、exposure、来源）。
    /// TS: `getAllTools() => ToolInfo[]`。
    fn get_all_tools(&self) -> Vec<ToolInfo>;

    /// 当前生效设置的副本（全局与项目合并）。TS: `getSettings() => Settings`。
    fn get_settings(&self) -> Settings;

    /// 按名设置 active 工具（未知与 `hidden` 忽略）。TS: `setActiveTools(toolNames)`。
    fn set_active_tools(&self, tool_names: Vec<String>);

    /// 当前会话可用的斜杠命令。TS: `getCommands() => SlashCommandInfo[]`。
    fn get_commands(&self) -> Vec<SlashCommandInfo>;

    // --- 模型与 thinking level ---------------------------------------------

    /// 设置当前会话的模型；provider 未配置认证时返回 false。
    /// TS: `setModel(model) => Promise<boolean>`。
    fn set_model(&self, model: Model) -> LocalBoxFuture<'static, bool>;

    /// 当前 thinking level。TS: `getThinkingLevel()`。
    fn get_thinking_level(&self) -> ThinkingLevel;

    /// 设置 thinking level（clamp 到模型能力）。TS: `setThinkingLevel(level)`。
    fn set_thinking_level(&self, level: ThinkingLevel);

    // --- provider 注册 ------------------------------------------------------

    /// 注册或覆盖模型 provider。
    ///
    /// TS 的两个重载（`registerProvider(provider)` 与 `registerProvider(name, config)`）折叠为
    /// **一个**方法名，参数是 [`ProviderRegistration`]。不要添加第二个公开方法名。
    fn register_provider(&self, registration: ProviderRegistration);

    /// 注销 provider。TS: `unregisterProvider(name)`。
    fn unregister_provider(&self, name: &str);

    // --- MCP servers --------------------------------------------------------

    /// 为本会话注册一个 MCP server。TS: `registerMcpServer(name, config)`。
    fn register_mcp_server(&self, name: &str, config: McpServerConfig);

    /// 移除本扩展注册的 MCP server 并关闭连接。TS: `unregisterMcpServer(name)`。
    fn unregister_mcp_server(&self, name: &str);

    /// 扩展注册的全部 MCP server。TS: `getMcpServers() => RegisteredMcpServer[]`。
    fn get_mcp_servers(&self) -> Vec<RegisteredMcpServer>;

    /// 注册虚拟模型。TS: `registerVirtualModel<TState>(model)`。
    fn register_virtual_model(&self, model: ExtensionVirtualModel);

    /// 移除虚拟模型。TS: `unregisterVirtualModel(provider, id)`。
    fn unregister_virtual_model(&self, provider: &str, id: &str);

    // --- 事件总线 -----------------------------------------------------------

    /// 扩展间通信的共享事件总线。TS 的字段 `events: EventBus`。
    ///
    /// Rust 用访问器方法表达属性（`ARCHITECTURE.md` §5.2 记录的偏离）。
    fn events(&self) -> Arc<dyn EventBus>;
}

/// 虚拟模型注册项。
///
/// TS: `types.ts#ExtensionVirtualModel<TState>` =
/// `Omit<VirtualModelDefinition<TState>, "route">` + 带扩展上下文的 `route`。
/// `TState` 擦除为任意 JSON（[`VirtualModelDefinition`] 的 `state` 是 `JsonValue`）。
pub struct ExtensionVirtualModel {
    /// 列在该 provider 下。
    pub provider: String,
    /// model id。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 可选择的 thinking level；缺省 `["off"]`。
    pub thinking_levels: Option<Vec<kiana_ai::types::ModelThinkingLevel>>,
    /// 首个响应前的上下文上限；未设为 0（未知）。
    pub context_window: Option<u64>,
    /// 首个响应前的输出上限；未设为 0。
    pub max_tokens: Option<u64>,
    /// 可接受的输入类型；缺省 text+image。
    pub input: Option<Vec<ModelInputModality>>,
    /// 选择物理模型与 thinking level（带扩展上下文）。
    pub route: VirtualModelRouteFn,
}

impl std::fmt::Debug for ExtensionVirtualModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionVirtualModel")
            .field("provider", &self.provider)
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// [`ExtensionVirtualModel::route`] 的回调类型。
///
/// TS: `(request, ctx: ExtensionContext) => ModelRoute | Promise<ModelRoute>`。
/// 返回 [`RouteFuture`]（`Send` 的 boxed future），错误类型是 `VirtualModelRouteError`。
pub type VirtualModelRouteFn =
    Arc<dyn Fn(ModelRouteRequest, Arc<dyn ExtensionContextApi>) -> RouteFuture + Send + Sync>;

// ============================================================================
// Provider 注册类型
// ============================================================================

/// provider 注册请求：[`ExtensionApi::register_provider`] 的两个 TS 重载。
// TS 联合的两个分支大小差异大；保留直接嵌套以便下游 match（§4.10 允许带理由的局部例外）。
#[allow(clippy::large_enum_variant)]
pub enum ProviderRegistration {
    /// `registerProvider(name, config)`。
    Config {
        /// provider 名。
        name: String,
        /// provider 配置。
        config: ProviderConfig,
    },
    /// `registerProvider(provider)`：原生 provider 对象。
    Native(Arc<dyn Provider>),
}

impl std::fmt::Debug for ProviderRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderRegistration::Config { name, .. } => formatter
                .debug_struct("Config")
                .field("name", name)
                .finish(),
            ProviderRegistration::Native(_) => formatter.write_str("Native(..)"),
        }
    }
}

/// 通过 [`ExtensionApi::register_provider`] 注册 provider 的配置。
///
/// TS: `types.ts#ProviderConfig`。含回调，没有 serde。字段顺序即 TS 声明顺序。
#[derive(Clone)]
pub struct ProviderConfig {
    /// UI 显示名。
    pub name: Option<String>,
    /// API 端点基地址（定义 models 时必填）。
    pub base_url: Option<String>,
    /// API key 字面量 / `$ENV` 插值 / 前导 `!command`。
    pub api_key: Option<String>,
    /// API 类型。
    pub api: Option<Api>,
    /// 自定义 API 的 `streamSimple` 处理器。
    ///
    /// TS 的 `options?` 可省略；Rust 侧调用方传 `SimpleStreamOptions::default()`。
    pub stream_simple: Option<ApiStreamSimpleFunction>,
    /// 按 image API 的图像生成实现。
    pub images: Option<IndexMap<String, Arc<dyn ProviderImages>>>,
    /// 按 classifier API 的分类实现。
    pub classifiers: Option<IndexMap<String, Arc<dyn ProviderClassifier>>>,
    /// 请求头。
    pub headers: Option<IndexMap<String, String>>,
    /// true 时添加 `Authorization: Bearer <resolved api key>`。
    pub auth_header: Option<bool>,
    /// 要注册的模型（提供则替换该 provider 的现有全部模型）。
    pub models: Option<Vec<ProviderModelConfig>>,
    /// 刷新该 provider 的模型列表。
    pub refresh_models: Option<RefreshModelsFn>,
    /// OAuth 支持（`/login`）；`id` 由 provider 名自动设置。
    pub oauth: Option<ProviderOAuthConfig>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api", &self.api)
            .finish_non_exhaustive()
    }
}

/// [`ProviderConfig::refresh_models`] 的回调类型。
///
/// TS: `refreshModels?(context: RefreshModelsContext) => Promise<ProviderModelConfig[]>`。
/// 抛出的错误以 boxed error 表达。
pub type RefreshModelsFn = Arc<
    dyn Fn(
            RefreshModelsContext,
        ) -> BoxFuture<
            'static,
            Result<Vec<ProviderModelConfig>, Box<dyn std::error::Error + Send + Sync>>,
        > + Send
        + Sync,
>;

/// [`ProviderConfig::oauth`] 的配置（Rust-only helper）。
///
/// TS: `types.ts#ProviderConfig.oauth` 的内联对象。`signal`/`AbortSignal` 一律用
/// [`CancellationToken`]。
#[derive(Clone)]
pub struct ProviderOAuthConfig {
    /// 登录 UI 的显示名。
    pub name: String,
    /// 是否由 provider 订阅支撑。
    pub is_subscription: Option<bool>,
    /// 已废弃：仅为源码兼容保留。
    pub uses_callback_server: Option<bool>,
    /// 运行登录流程，返回要持久化的凭证。
    pub login: OAuthLoginFn,
    /// 刷新过期凭证。
    pub refresh_token: OAuthRefreshTokenFn,
    /// 把凭证转成 provider 的 API key。
    pub get_api_key: Arc<dyn Fn(&OAuthCredentials) -> String + Send + Sync>,
    /// 旧版同步的、依赖凭证的模型投影。
    pub modify_models:
        Option<Arc<dyn Fn(Vec<Model>, &OAuthCredentials) -> Vec<Model> + Send + Sync>>,
}

impl std::fmt::Debug for ProviderOAuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderOAuthConfig")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// OAuth 登录回调。
///
/// TS: `login(callbacks: OAuthLoginCallbacks) => Promise<OAuthCredentials>`。
pub type OAuthLoginFn = Arc<
    dyn Fn(
            OAuthLoginCallbacks,
        ) -> LocalBoxFuture<
            'static,
            Result<OAuthCredentials, Box<dyn std::error::Error + Send + Sync>>,
        > + Send
        + Sync,
>;

/// OAuth 刷新回调。
///
/// TS: `refreshToken(credentials, signal: AbortSignal) => Promise<OAuthCredentials>`。
pub type OAuthRefreshTokenFn = Arc<
    dyn Fn(
            OAuthCredentials,
            CancellationToken,
        ) -> LocalBoxFuture<
            'static,
            Result<OAuthCredentials, Box<dyn std::error::Error + Send + Sync>>,
        > + Send
        + Sync,
>;

/// chat 模型配置的 `type` 字面量。TS: `ProviderChatModelConfig.type?: "chat"`。
///
/// TS 让错误的判别式不可表示（`type?: "chat"`），这里同样用单变体枚举：chat 配置只能
/// 携带 `"chat"` 或省略该键，不能写成 `"image"`/`"classifier"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProviderChatModelType {
    /// `"chat"`（省略 `type` 时按 chat 处理）。
    #[default]
    #[serde(rename = "chat")]
    Chat,
}

/// image 模型配置的 `type` 字面量。TS: `ProviderImageModelConfig.type: "image"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProviderImageModelType {
    /// `"image"`。
    #[default]
    #[serde(rename = "image")]
    Image,
}

/// classifier 模型配置的 `type` 字面量。TS: `ProviderClassifierModelConfig.type: "classifier"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProviderClassifierModelType {
    /// `"classifier"`。
    #[default]
    #[serde(rename = "classifier")]
    Classifier,
}

/// chat 模型配置。TS: `types.ts#ProviderChatModelConfig`（extends `ProviderModelConfigBase`）。
///
/// 字段顺序：基类字段（`id`、`name`、`api`、`baseUrl`、`input`、`inputLimits`、`cost`、`headers`），
/// 然后自己的 `type`、`reasoning`、`thinkingLevelMap`、`promptCache`、`contextWindow`、
/// `maxTokens`、`samplingParams`、`compat`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderChatModelConfig {
    /// 模型 id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// API 类型覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<Api>,
    /// API 端点覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 支持的输入类型。
    pub input: Vec<ModelInputModality>,
    /// provider 输入限制与图片预处理元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    /// 每百万 token 的价格与可选的请求级分级。
    pub cost: ModelCost,
    /// 该模型的自定义请求头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// `type?: "chat"`（省略即 chat；不能是其他判别式）。
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub model_type: Option<ProviderChatModelType>,
    /// 是否支持扩展 thinking。
    pub reasoning: bool,
    /// thinking level 到 provider 值的映射；`null` 标记不支持。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// prompt cache 生命周期（秒/等级）；未设禁用 cache warming。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    /// 上下文窗口大小（token）。
    pub context_window: u64,
    /// 最大输出 token。
    pub max_tokens: u64,
    /// 采样参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<SamplingParams>,
    /// OpenAI 兼容设置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<JsonValue>,
}

/// image 模型配置。TS: `types.ts#ProviderImageModelConfig`（extends `ProviderModelConfigBase`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImageModelConfig {
    /// 模型 id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// API 类型覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<Api>,
    /// API 端点覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 支持的输入类型。
    pub input: Vec<ModelInputModality>,
    /// provider 输入限制与图片预处理元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    /// 价格。
    pub cost: ModelCost,
    /// 该模型的自定义请求头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// 恒为 `"image"`。
    #[serde(rename = "type")]
    pub model_type: ProviderImageModelType,
    /// 输出类型。
    pub output: Vec<ModelInputModality>,
}

/// classifier 模型配置。
/// TS: `types.ts#ProviderClassifierModelConfig`（extends `ProviderModelConfigBase`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderClassifierModelConfig {
    /// 模型 id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// API 类型覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<Api>,
    /// API 端点覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 支持的输入类型。
    pub input: Vec<ModelInputModality>,
    /// provider 输入限制与图片预处理元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    /// 价格。
    pub cost: ModelCost,
    /// 该模型的自定义请求头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// 恒为 `"classifier"`。
    #[serde(rename = "type")]
    pub model_type: ProviderClassifierModelType,
    /// 上下文窗口大小。
    pub context_window: u64,
}

/// provider 内的模型配置联合。TS: `types.ts#ProviderModelConfig`。
///
/// 判别式是 `type`；chat 的 `type` 可以省略（归一化为 `"chat"`），因此手写
/// `Deserialize`（缺省或 `"chat"` → [`ProviderModelConfig::Chat`]）。
///
/// 反序列化的分派规则（见下面的实现）：
/// - `type` 缺失或不是字符串 → 按 `"chat"` 分派；
/// - `"chat"` / `"image"` / `"classifier"` → 对应变体；
/// - 其它字符串 → 错误 `unknown provider model type: <值>`（`kiana-ext-api/tests` 的
///   `provider_model_config_dispatch_by_type` 锁定该文案）。
///
/// 序列化时三个变体分别转发给各自的结构体，`type` 键是否出现由该结构体的 `model_type`
/// 字段决定（chat 省略时不写出）。
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ProviderModelConfig {
    /// chat 模型。
    Chat(ProviderChatModelConfig),
    /// image 模型。
    Image(ProviderImageModelConfig),
    /// classifier 模型。
    Classifier(ProviderClassifierModelConfig),
}

impl Serialize for ProviderModelConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 三个变体各自的配置结构体就是线上对象，直接转发；`type` 的出现与否由各结构体自己的
        // `model_type` 字段与 `skip_serializing_if` 决定，这里不做额外包装。
        match self {
            ProviderModelConfig::Chat(config) => config.serialize(serializer),
            ProviderModelConfig::Image(config) => config.serialize(serializer),
            ProviderModelConfig::Classifier(config) => config.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ProviderModelConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        // 阶段 1：先落到通用 JSON 值。必须先看判别式 `type` 才能决定用哪个结构体反序列化，
        // 而派生实现表达不了「`type` 可省略且缺省为 chat」的联合，所以手写分派。
        let value = JsonValue::deserialize(deserializer)?;
        // 阶段 2：读取判别式。键缺失、或值不是字符串时都退回 `"chat"`。
        let model_type = value
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or("chat");
        // 阶段 3：按判别式复用三个具体结构体的 `Deserialize`；阶段 4：未知判别式在这里拒绝
        // （文案由测试 `provider_model_config_dispatch_by_type` 锁定）。
        match model_type {
            "chat" => serde_json::from_value(value)
                .map(ProviderModelConfig::Chat)
                .map_err(D::Error::custom),
            "image" => serde_json::from_value(value)
                .map(ProviderModelConfig::Image)
                .map_err(D::Error::custom),
            "classifier" => serde_json::from_value(value)
                .map(ProviderModelConfig::Classifier)
                .map_err(D::Error::custom),
            other => Err(D::Error::custom(format!(
                "unknown provider model type: {other}"
            ))),
        }
    }
}

// ============================================================================
// 工厂 / flag / 快捷键 / ToolInfo
// ============================================================================

/// 扩展工厂函数（同步或异步初始化）。
///
/// TS: `types.ts#ExtensionFactory = (kiana: ExtensionAPI) => void | Promise<void>`。
/// 非 `Send` future：工厂在 `kiana-extensions` 的 `LocalSet` 上执行。
pub type ExtensionFactory = Arc<dyn Fn(Arc<dyn ExtensionApi>) -> LocalBoxFuture<'static, ()>>;

/// 内联扩展。TS: `types.ts#InlineExtension`。
pub enum InlineExtension {
    /// 直接给工厂函数。
    Factory(ExtensionFactory),
    /// 带元数据的具名扩展。
    Named {
        /// 启动列表与错误里显示的名字（`<inline:name>`）。
        name: String,
        /// 工厂函数。
        factory: ExtensionFactory,
        /// 从启动 Extensions 列表省略。
        hidden: Option<bool>,
        /// 另一个扩展注册同名 tool/command/flag 时整体省略本扩展（不报冲突）。
        replaceable: Option<bool>,
        /// 以 `builtin:<name>` 资源加载（在 project trust 解析之后）。
        builtin: Option<bool>,
    },
}

impl std::fmt::Debug for InlineExtension {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InlineExtension::Factory(_) => formatter.write_str("Factory(..)"),
            InlineExtension::Named { name, .. } => {
                formatter.debug_struct("Named").field("name", name).finish()
            }
        }
    }
}

/// 已注册的工具（定义 + 来源）。TS: `types.ts#RegisteredTool`。
#[derive(Clone)]
pub struct RegisteredTool {
    /// 工具定义。
    pub definition: ToolDefinition,
    /// 定义该工具的资源的来源信息。
    pub source_info: SourceInfo,
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredTool")
            .field("definition", &self.definition)
            .finish_non_exhaustive()
    }
}

/// CLI flag 的类型。TS: `types.ts#ExtensionFlag.type`（`"boolean" | "string"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagType {
    /// `"boolean"`。
    #[default]
    Boolean,
    /// `"string"`。
    String,
}

/// flag 的值。TS: `boolean | string`。
///
/// `#[serde(untagged)]`：JSON 布尔解析为 `Bool`，JSON 字符串解析为 `Str`；`Default` 是
/// `Bool(false)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FlagValue {
    /// boolean flag 的值。
    Bool(bool),
    /// string flag 的值。
    Str(String),
}

impl Default for FlagValue {
    fn default() -> Self {
        FlagValue::Bool(false)
    }
}

/// 已注册的 CLI flag。TS: `types.ts#ExtensionFlag`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionFlag {
    /// flag 名。
    pub name: String,
    /// 描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 类型。
    #[serde(rename = "type")]
    pub r#type: FlagType,
    /// 默认值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<FlagValue>,
    /// 注册该 flag 的扩展路径。
    pub extension_path: String,
}

/// 已注册的快捷键。TS: `types.ts#ExtensionShortcut`。
#[derive(Clone)]
pub struct ExtensionShortcut {
    /// 快捷键（`KeyId`；ASCII 精确比较）。
    pub shortcut: String,
    /// 描述。
    pub description: Option<String>,
    /// 处理器。
    pub handler: ShortcutHandler,
    /// 注册该快捷键的扩展路径。
    pub extension_path: String,
}

impl std::fmt::Debug for ExtensionShortcut {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionShortcut")
            .field("shortcut", &self.shortcut)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// 带 name/description/parameters/guidelines/来源的工具信息。
/// TS: `types.ts#ToolInfo`。
///
/// 它是内部工具注册表的只读投影（`kiana-session` 的 `get_all_tools`）：`exposure` 在查询时
/// 按当前配置计算，其余字段来自 `ToolDefinition` 与注册时的来源信息。
#[derive(Clone)]
pub struct ToolInfo {
    /// 工具名。
    pub name: String,
    /// 描述。
    pub description: String,
    /// 参数 schema。
    pub parameters: JsonValue,
    /// guideline 条目。
    pub prompt_guidelines: Option<Vec<String>>,
    /// 工具声明的可达性回调（来自 `ToolDefinition::reaches_tools`）。
    ///
    /// loader 把它原样暴露成 JS `getAllTools()` 结果上的函数 `reachesTools`，返回 `"all"` 或
    /// 工具/命名空间名数组；`None` 时该属性不出现。
    pub reaches_tools: Option<crate::tool_types::ReachesToolsFn>,
    /// 工具曝光级别（`direct` / `model-only` / `codemode` / `deferred` / `hidden`）。
    ///
    /// 决定是否声明给模型、是否可调用；`getAllTools()` 返回的是查询时按当前配置计算的值。
    pub exposure: ToolExposure,
    /// 命名空间。
    pub namespace: Option<ToolNamespace>,
    /// 行为提示。
    pub annotations: Option<ToolAnnotations>,
    /// 来源信息。
    pub source_info: SourceInfo,
}

impl std::fmt::Debug for ToolInfo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolInfo")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("exposure", &self.exposure)
            .finish_non_exhaustive()
    }
}

// ============================================================================
// 动作 handler 别名（15 个）
// ============================================================================

/// `sendMessage` 的动作实现。
/// TS: `types.ts#SendMessageHandler`（`<T>(message, options?) => void`，泛型擦除）。
pub type SendMessageHandler = Arc<dyn Fn(SendMessageOptions) + Send + Sync>;

/// `sendUserMessage` 的动作实现。TS: `types.ts#SendUserMessageHandler`。
pub type SendUserMessageHandler =
    Arc<dyn Fn(UserContent, Option<SendUserMessageOptions>) + Send + Sync>;

/// `appendEntry` 的动作实现。TS: `types.ts#AppendEntryHandler`。
pub type AppendEntryHandler = Arc<dyn Fn(String, Option<JsonValue>) + Send + Sync>;

/// `setSessionName` 的动作实现。TS: `types.ts#SetSessionNameHandler`。
pub type SetSessionNameHandler = Arc<dyn Fn(String) + Send + Sync>;

/// `getSessionName` 的动作实现。TS: `types.ts#GetSessionNameHandler`。
pub type GetSessionNameHandler = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// `getActiveTools` 的动作实现。TS: `types.ts#GetActiveToolsHandler`。
pub type GetActiveToolsHandler = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// `getAllTools` 的动作实现。TS: `types.ts#GetAllToolsHandler`。
pub type GetAllToolsHandler = Arc<dyn Fn() -> Vec<ToolInfo> + Send + Sync>;

/// `getSettings` 的动作实现。TS: `types.ts#GetSettingsHandler`。
pub type GetSettingsHandler = Arc<dyn Fn() -> Settings + Send + Sync>;

/// `getCommands` 的动作实现。TS: `types.ts#GetCommandsHandler`。
pub type GetCommandsHandler = Arc<dyn Fn() -> Vec<SlashCommandInfo> + Send + Sync>;

/// `setActiveTools` 的动作实现。TS: `types.ts#SetActiveToolsHandler`。
pub type SetActiveToolsHandler = Arc<dyn Fn(Vec<String>) + Send + Sync>;

/// `refreshTools` 的动作实现。TS: `types.ts#RefreshToolsHandler`。
pub type RefreshToolsHandler = Arc<dyn Fn() + Send + Sync>;

/// `setModel` 的动作实现。TS: `types.ts#SetModelHandler`（返回 `Promise<boolean>`）。
pub type SetModelHandler = Arc<dyn Fn(Model) -> LocalBoxFuture<'static, bool> + Send + Sync>;

/// `getThinkingLevel` 的动作实现。TS: `types.ts#GetThinkingLevelHandler`。
pub type GetThinkingLevelHandler = Arc<dyn Fn() -> ThinkingLevel + Send + Sync>;

/// `setThinkingLevel` 的动作实现。TS: `types.ts#SetThinkingLevelHandler`。
pub type SetThinkingLevelHandler = Arc<dyn Fn(ThinkingLevel) + Send + Sync>;

/// `setLabel` 的动作实现。TS: `types.ts#SetLabelHandler`。
pub type SetLabelHandler = Arc<dyn Fn(String, Option<String>) + Send + Sync>;

// ============================================================================
// 动作集合（ca-session 注入）
// ============================================================================

/// loader 创建、注册期与运行期共用的状态。
///
/// TS: `types.ts#ExtensionRuntimeState`。含回调，没有 serde。
///
/// # 闭包约定
///
/// - 这些闭包都没有 `Result` 错误通道（返回类型是 `()`、上下文 `Arc` 或退订闭包）：当前实现
///   （`kiana-extensions`）以 panic 表达失败，例如 `create_context` 在 runner 绑定上下文之前
///   被调用、`assert_active` 遇到已失效实例时。
/// - `register_provider` / `register_native_provider` / `register_virtual_model` 的
///   `Option<String>` 参数是发起注册的扩展路径，传 `None` 时由实现方填入默认值（loader 用
///   `LOADING_DEFAULT_CWD`）；`unregister_provider` 也有这个参数，但当前实现忽略它。
/// - `invalidate(Some(msg))` 记录 `msg` 作为陈旧文案，`None` 时使用实现方的默认文案；记录到
///   非空文案后后续调用不再改变状态，空串按“未失效”处理（与 TS 的真值判断一致）。
/// - `track_event_bus_subscription` 用于保留 [`EventBus`] 订阅：返回一个可提前退订的闭包，
///   runtime 失效时实现方会统一退订已跟踪的订阅。
pub struct ExtensionRuntimeState {
    /// flag 的值（注册期写入默认值，CLI 之后写入用户值）。
    pub flag_values: IndexMap<String, FlagValue>,
    /// 加载期排队的旧式 provider 配置注册。
    pub pending_provider_registrations: Vec<PendingProviderRegistration>,
    /// 加载期排队的原生 provider 注册。
    pub pending_native_provider_registrations: Vec<PendingNativeProviderRegistration>,
    /// 加载期排队的虚拟模型注册。
    pub pending_virtual_model_registrations: Vec<PendingVirtualModelRegistration>,
    /// 创建扩展上下文（runner 绑定前抛错）。
    pub create_context: Arc<dyn Fn() -> Arc<dyn ExtensionContextApi> + Send + Sync>,
    /// 运行时替换后，陈旧实例调用时抛错。
    pub assert_active: Arc<dyn Fn() + Send + Sync>,
    /// 标记本扩展实例为陈旧。
    pub invalidate: Arc<dyn Fn(Option<String>) + Send + Sync>,
    /// 保留事件总线订阅直到 runtime 失效。
    pub track_event_bus_subscription: Arc<dyn Fn(Unsubscribe) -> Unsubscribe + Send + Sync>,
    /// 注册/注销 provider。
    pub register_provider: Arc<dyn Fn(String, ProviderConfig, Option<String>) + Send + Sync>,
    /// 注册原生 provider。
    pub register_native_provider: Arc<dyn Fn(Arc<dyn Provider>, Option<String>) + Send + Sync>,
    /// 注销 provider。
    pub unregister_provider: Arc<dyn Fn(String, Option<String>) + Send + Sync>,
    /// 扩展注册的 MCP server 注册表。
    pub mcp_servers: McpServerRegistry,
    /// 注册虚拟模型。
    pub register_virtual_model: Arc<dyn Fn(VirtualModelDefinition, Option<String>) + Send + Sync>,
    /// 注销虚拟模型。
    pub unregister_virtual_model: Arc<dyn Fn(String, String) + Send + Sync>,
}

impl std::fmt::Debug for ExtensionRuntimeState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionRuntimeState")
            .field("flag_values", &self.flag_values)
            .finish_non_exhaustive()
    }
}

/// 加载期排队的 provider 配置注册。
///
/// TS: `ExtensionRuntimeState.pendingProviderRegistrations` 的元素
/// `{ name: string; config: ProviderConfig; extensionPath: string }`。
#[derive(Clone)]
pub struct PendingProviderRegistration {
    /// provider 名。
    pub name: String,
    /// 配置。
    pub config: ProviderConfig,
    /// 注册它的扩展路径。
    pub extension_path: String,
}

impl std::fmt::Debug for PendingProviderRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingProviderRegistration")
            .field("name", &self.name)
            .field("extension_path", &self.extension_path)
            .finish_non_exhaustive()
    }
}

/// 加载期排队的原生 provider 注册。
///
/// TS: `pendingNativeProviderRegistrations` 的元素
/// `{ provider: Provider; extensionPath: string }`。
#[derive(Clone)]
pub struct PendingNativeProviderRegistration {
    /// 原生 provider。
    pub provider: Arc<dyn Provider>,
    /// 注册它的扩展路径。
    pub extension_path: String,
}

impl std::fmt::Debug for PendingNativeProviderRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingNativeProviderRegistration")
            .field("extension_path", &self.extension_path)
            .finish_non_exhaustive()
    }
}

/// 加载期排队的虚拟模型注册。
///
/// TS: `pendingVirtualModelRegistrations` 的元素
/// `{ definition: VirtualModelDefinition; extensionPath: string }`。
#[derive(Clone)]
pub struct PendingVirtualModelRegistration {
    /// 虚拟模型定义。
    pub definition: VirtualModelDefinition,
    /// 注册它的扩展路径。
    pub extension_path: String,
}

impl std::fmt::Debug for PendingVirtualModelRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingVirtualModelRegistration")
            .field("extension_path", &self.extension_path)
            .finish_non_exhaustive()
    }
}

/// `kiana.*` API 方法的动作实现（15 个）。
///
/// TS: `types.ts#ExtensionActions`。由 runner.initialize() 提供、复制进共享 runtime。
#[derive(Clone)]
pub struct ExtensionActions {
    /// `sendMessage`。
    pub send_message: SendMessageHandler,
    /// `sendUserMessage`。
    pub send_user_message: SendUserMessageHandler,
    /// `appendEntry`。
    pub append_entry: AppendEntryHandler,
    /// `setSessionName`。
    pub set_session_name: SetSessionNameHandler,
    /// `getSessionName`。
    pub get_session_name: GetSessionNameHandler,
    /// `setLabel`。
    pub set_label: SetLabelHandler,
    /// `getActiveTools`。
    pub get_active_tools: GetActiveToolsHandler,
    /// `getAllTools`。
    pub get_all_tools: GetAllToolsHandler,
    /// `getSettings`。
    pub get_settings: GetSettingsHandler,
    /// `setActiveTools`。
    pub set_active_tools: SetActiveToolsHandler,
    /// `refreshTools`。
    pub refresh_tools: RefreshToolsHandler,
    /// `getCommands`。
    pub get_commands: GetCommandsHandler,
    /// `setModel`。
    pub set_model: SetModelHandler,
    /// `getThinkingLevel`。
    pub get_thinking_level: GetThinkingLevelHandler,
    /// `setThinkingLevel`。
    pub set_thinking_level: SetThinkingLevelHandler,
}

impl std::fmt::Debug for ExtensionActions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExtensionActions { .. }")
    }
}

/// `ExtensionContext` 的动作集合（`ctx.*` in event handlers）。
///
/// TS: `types.ts#ExtensionContextActions`。可选动作用 `Option` 表达 TS 的可选属性。
#[derive(Clone)]
pub struct ExtensionContextActions {
    /// 当前模型。
    pub get_model: Arc<dyn Fn() -> Option<Model> + Send + Sync>,
    /// 作用域模型。
    pub get_scoped_models: Arc<dyn Fn() -> Vec<ScopedModel> + Send + Sync>,
    /// 是否空闲。
    pub is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    /// 项目本地信任是否生效。
    pub is_project_trusted: Arc<dyn Fn() -> bool + Send + Sync>,
    /// 当前取消信号。
    pub get_signal: Arc<dyn Fn() -> Option<CancellationToken> + Send + Sync>,
    /// 中止当前操作。
    pub abort: Arc<dyn Fn() + Send + Sync>,
    /// 是否有排队消息。
    pub has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync>,
    /// 优雅关闭。
    pub shutdown: Arc<dyn Fn() + Send + Sync>,
    /// 当前上下文用量。
    pub get_context_usage: Arc<dyn Fn() -> Option<ContextUsage> + Send + Sync>,
    /// 触发压缩。
    pub compact: Arc<dyn Fn(Option<CompactOptions>) + Send + Sync>,
    /// 当前系统提示。
    pub get_system_prompt: Arc<dyn Fn() -> String + Send + Sync>,
    /// 当前基础系统提示选项（缺省时用 `normalizeBuildSystemPromptOptions({ cwd })`）。
    pub get_system_prompt_options: Option<Arc<dyn Fn() -> BuildSystemPromptOptions + Send + Sync>>,
    /// 支撑 `ExtensionToolContext.executeTool()`；缺省时嵌套调用失败。
    ///
    /// 参数依次是调用方 toolCallId、工具名、参数与选项（`kiana-session` 的闭包实现）。
    pub execute_tool: Option<
        Arc<
            dyn Fn(
                    String,
                    String,
                    JsonValue,
                    ExecuteToolOptions,
                ) -> BoxFuture<'static, AgentToolCallOutcome>
                + Send
                + Sync,
        >,
    >,
    /// 支撑 `ExtensionToolContext.tools`。
    pub get_callable_tools: Option<Arc<dyn Fn() -> Vec<AgentTool> + Send + Sync>>,
}

impl std::fmt::Debug for ExtensionContextActions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExtensionContextActions { .. }")
    }
}

/// `ExtensionCommandContext` 的动作集合（`ctx.*` in command handlers）。
///
/// TS: `types.ts#ExtensionCommandContextActions`。
#[derive(Clone)]
pub struct ExtensionCommandContextActions {
    /// 等待空闲。
    pub wait_for_idle: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    /// 开始新会话。
    pub new_session: Arc<
        dyn Fn(Option<NewSessionOptions>) -> LocalBoxFuture<'static, NewSessionResult>
            + Send
            + Sync,
    >,
    /// fork。
    pub fork: Arc<
        dyn Fn(String, Option<ForkOptions>) -> LocalBoxFuture<'static, NewSessionResult>
            + Send
            + Sync,
    >,
    /// 树导航。
    pub navigate_tree: Arc<
        dyn Fn(String, Option<NavigateTreeOptions>) -> LocalBoxFuture<'static, NewSessionResult>
            + Send
            + Sync,
    >,
    /// 切换会话。
    pub switch_session: Arc<
        dyn Fn(String, Option<SwitchSessionOptions>) -> LocalBoxFuture<'static, NewSessionResult>
            + Send
            + Sync,
    >,
    /// 重新加载资源。
    pub reload: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
}

impl std::fmt::Debug for ExtensionCommandContextActions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExtensionCommandContextActions { .. }")
    }
}

/// 完整 runtime = 状态 + 动作。
///
/// TS: `types.ts#ExtensionRuntime extends ExtensionRuntimeState, ExtensionActions`。
/// 按 `ARCHITECTURE.md` §4.3 的基类规则，这里**重复**两组字段（状态在前、动作在后），
/// 不使用组合包装。
///
/// # 快照语义（实现方约定）
///
/// 结构体本身不可变。`kiana-extensions` 把可变的 flag 值、pending 注册队列与动作绑定放在
/// 自己的共享状态里，本结构体是构造时的快照：
///
/// - `flag_values` / `pending_*` 读到的是快照，不反映之后的变更；
/// - `create_context`、`assert_active`、`invalidate`、`track_event_bus_subscription`、
///   `register_*` / `unregister_*` 是捕获共享状态的闭包，调用时看到最新状态；
/// - `mcp_servers` 是共享注册表的克隆（[`McpServerRegistry`] 内部用 `Arc<Mutex<...>>`），
///   读到的内容与实现方是同一份。
pub struct ExtensionRuntime {
    // --- ExtensionRuntimeState ---
    /// flag 的值。
    pub flag_values: IndexMap<String, FlagValue>,
    /// 加载期排队的 provider 配置注册。
    pub pending_provider_registrations: Vec<PendingProviderRegistration>,
    /// 加载期排队的原生 provider 注册。
    pub pending_native_provider_registrations: Vec<PendingNativeProviderRegistration>,
    /// 加载期排队的虚拟模型注册。
    pub pending_virtual_model_registrations: Vec<PendingVirtualModelRegistration>,
    /// 创建扩展上下文。
    pub create_context: Arc<dyn Fn() -> Arc<dyn ExtensionContextApi> + Send + Sync>,
    /// 陈旧实例调用时抛错。
    pub assert_active: Arc<dyn Fn() + Send + Sync>,
    /// 标记实例为陈旧。
    pub invalidate: Arc<dyn Fn(Option<String>) + Send + Sync>,
    /// 保留事件总线订阅。
    pub track_event_bus_subscription: Arc<dyn Fn(Unsubscribe) -> Unsubscribe + Send + Sync>,
    /// 注册/注销 provider。
    pub register_provider: Arc<dyn Fn(String, ProviderConfig, Option<String>) + Send + Sync>,
    /// 注册原生 provider。
    pub register_native_provider: Arc<dyn Fn(Arc<dyn Provider>, Option<String>) + Send + Sync>,
    /// 注销 provider。
    pub unregister_provider: Arc<dyn Fn(String, Option<String>) + Send + Sync>,
    /// MCP server 注册表。
    pub mcp_servers: McpServerRegistry,
    /// 注册虚拟模型。
    pub register_virtual_model: Arc<dyn Fn(VirtualModelDefinition, Option<String>) + Send + Sync>,
    /// 注销虚拟模型。
    pub unregister_virtual_model: Arc<dyn Fn(String, String) + Send + Sync>,

    // --- ExtensionActions ---
    /// `sendMessage`。
    pub send_message: SendMessageHandler,
    /// `sendUserMessage`。
    pub send_user_message: SendUserMessageHandler,
    /// `appendEntry`。
    pub append_entry: AppendEntryHandler,
    /// `setSessionName`。
    pub set_session_name: SetSessionNameHandler,
    /// `getSessionName`。
    pub get_session_name: GetSessionNameHandler,
    /// `setLabel`。
    pub set_label: SetLabelHandler,
    /// `getActiveTools`。
    pub get_active_tools: GetActiveToolsHandler,
    /// `getAllTools`。
    pub get_all_tools: GetAllToolsHandler,
    /// `getSettings`。
    pub get_settings: GetSettingsHandler,
    /// `setActiveTools`。
    pub set_active_tools: SetActiveToolsHandler,
    /// `refreshTools`。
    pub refresh_tools: RefreshToolsHandler,
    /// `getCommands`。
    pub get_commands: GetCommandsHandler,
    /// `setModel`。
    pub set_model: SetModelHandler,
    /// `getThinkingLevel`。
    pub get_thinking_level: GetThinkingLevelHandler,
    /// `setThinkingLevel`。
    pub set_thinking_level: SetThinkingLevelHandler,
}

impl std::fmt::Debug for ExtensionRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionRuntime")
            .field("flag_values", &self.flag_values)
            .finish_non_exhaustive()
    }
}

// ============================================================================
// 加载产物
// ============================================================================

/// 已加载的扩展及其全部注册项。TS: `types.ts#Extension`。
///
/// 没有 serde（含回调与 trait 对象）。
///
/// 各 `IndexMap` 的插入顺序即注册顺序，runner 的解析链依赖它：例如消息/条目渲染器按扩展
/// 加载顺序取第一个注册了对应 `customType` 的扩展（`kiana-extensions` 的
/// `runner/renderers.rs`）；同一扩展内后注册的同名渲染器覆盖前一个。
#[derive(Clone)]
pub struct Extension {
    /// 用于错误报告的显示路径（可能是 `builtin:`/`<inline:N>`）。
    pub path: String,
    /// 解析后的真实路径。
    pub resolved_path: String,
    /// 从启动列表隐藏。
    pub hidden: Option<bool>,
    /// 见 [`InlineExtension::Named`]。
    pub replaceable: Option<bool>,
    /// 来源信息。
    pub source_info: SourceInfo,
    /// 按事件名分组的 handler（插入序）。
    ///
    /// TS 的字段是 `handlers: Map<string, HandlerFn[]>`，其中
    /// `HandlerFn = (...args: unknown[]) => Promise<unknown>`（`loader.ts`）：标准事件与
    /// `project_trust` 的 handler **存在同一个 map 里**（`runner.ts` 的
    /// `snapshotEventHandlers` 从 `ext.handlers.get(event)?.slice()` 取两者）。
    /// 因此这里存 [`ExtensionHandler`] 枚举而不是只用
    /// [`ExtensionHandlerFn`](crate::events::ExtensionHandlerFn)，
    /// 否则 `emit_project_trust_event` 无法从已加载扩展集合取回 project_trust handler。
    pub handlers: IndexMap<String, Vec<ExtensionHandler>>,
    /// 工具。
    pub tools: IndexMap<String, RegisteredTool>,
    /// 自定义消息渲染器。
    pub message_renderers: IndexMap<String, MessageRenderer>,
    /// 工具渲染器 resolver（按注册顺序）。
    pub tool_renderers: Option<Vec<ToolRendererResolver>>,
    /// markdown 变换器（单值）。
    pub markdown_transformer: Option<MarkdownTransformer>,
    /// 自定义条目渲染器。
    pub entry_renderers: Option<IndexMap<String, EntryRenderer>>,
    /// 命令。
    pub commands: IndexMap<String, RegisteredCommand>,
    /// flag。
    pub flags: IndexMap<String, ExtensionFlag>,
    /// 快捷键（key 为 `KeyId` 字符串）。
    pub shortcuts: IndexMap<String, ExtensionShortcut>,
}

impl std::fmt::Debug for Extension {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Extension")
            .field("path", &self.path)
            .field("resolved_path", &self.resolved_path)
            .finish_non_exhaustive()
    }
}

/// 加载一个扩展的错误。TS: `LoadExtensionsResult.errors` 的元素
/// `{ path: string; error: string }`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadError {
    /// 扩展路径。
    pub path: String,
    /// 错误文本。
    pub error: String,
}

/// 加载一个扩展的警告。TS: `LoadExtensionsResult.warnings` 的元素
/// `{ path: string; warning: string }`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadWarning {
    /// 扩展路径。
    pub path: String,
    /// 警告文本。
    pub warning: String,
}

/// 加载扩展的结果。TS: `types.ts#LoadExtensionsResult`。
///
/// `runtime` 的动作在 `runner.initialize()` 之前是抛错桩。
///
/// 同一批加载的所有 [`Extension`] 共享同一个 `runtime`（`Arc`）；`errors` / `warnings` 是
/// 逐扩展的记录，与 `extensions` 并列返回。
#[derive(Clone)]
pub struct LoadExtensionsResult {
    /// 已加载的扩展。
    pub extensions: Vec<Extension>,
    /// 加载错误。
    pub errors: Vec<LoadError>,
    /// 加载警告（TS 的可选字段）。
    pub warnings: Option<Vec<LoadWarning>>,
    /// 共享 runtime。
    pub runtime: Arc<ExtensionRuntime>,
}

impl std::fmt::Debug for LoadExtensionsResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadExtensionsResult")
            .field("extensions", &self.extensions)
            .field("errors", &self.errors)
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

/// 扩展运行期错误。TS: `types.ts#ExtensionError`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionError {
    /// 扩展路径。
    pub extension_path: String,
    /// 事件名。
    pub event: String,
    /// 错误文本（TS 的 `error.message` 或 `String(err)`）。
    pub error: String,
    /// 栈。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

// ============================================================================
// Rust-only helper：内联对象字面量
// ============================================================================

/// `sendMessage` 的选项（Rust-only helper）。
///
/// TS: `ExtensionAPI.sendMessage` 的参数
/// `Pick<CustomMessage<T>, "customType" | "content" | "display" | "details">` +
/// `options?: { triggerTurn?: boolean; deliverAs?: "steer" | "followUp" | "nextTurn" }`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageOptions {
    /// 要发送的自定义消息。
    pub message: CustomMessagePayload,
    /// 是否触发一个 turn（仅空闲时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_turn: Option<bool>,
    /// 投递方式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<DeliverAs>,
}

/// 自定义消息的载荷（Rust-only helper）。
///
/// TS: `Pick<CustomMessage<T>, "customType" | "content" | "display" | "details">`。
/// 键顺序 `customType`、`content`、`display`、`details`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessagePayload {
    /// 扩展自定义的消息类型标识；`register_message_renderer` 按它匹配渲染器。
    pub custom_type: String,
    /// 内容（字符串或内容块数组）。
    pub content: UserContent,
    /// TUI 是否显示。
    pub display: bool,
    /// 扩展私有元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

/// `sendMessage` 的投递方式（Rust-only helper）。
///
/// TS: `"steer" | "followUp" | "nextTurn"`。多词字面量逐变体 rename。
///
/// 运行期语义（`kiana-session` 的 `send_custom_message`）：
/// - `NextTurn`：不参与当前 run，先排进待处理队列，下一次 prompt 时注入；
/// - `FollowUp`：agent 正在运行**且 `triggerTurn != Some(false)`** 时调用
///   `agent.follow_up(message)`；
/// - `Steer`：agent 正在运行**且 `triggerTurn != Some(false)`** 时调用 `agent.steer(message)`；
///   未指定 `deliverAs`（`None`）时也走这一分支（实现里是 `_ =>` 匹配臂）。
/// - agent 运行中但 `triggerTurn == Some(false)` 时**不**调用 `follow_up`/`steer`，而是进入
///   `pending_custom_messages` 待处理队列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DeliverAs {
    /// `"steer"`。
    #[default]
    #[serde(rename = "steer")]
    Steer,
    /// `"followUp"`。
    #[serde(rename = "followUp")]
    FollowUp,
    /// `"nextTurn"`。
    #[serde(rename = "nextTurn")]
    NextTurn,
}

/// `sendUserMessage` 的选项（Rust-only helper）。
///
/// TS: `options?: { deliverAs?: "steer" | "followUp"; expandPromptTemplates?: boolean }`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendUserMessageOptions {
    /// 投递方式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<SendUserMessageDeliverAs>,
    /// 是否展开 prompt 模板/技能命令（缺省 false）。
    ///
    /// `kiana-session` 的 `send_user_message` 用 `unwrap_or(false)`；测试
    /// `send_user_message_does_not_expand_prompt_templates_by_default` 固定该行为。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand_prompt_templates: Option<bool>,
}

/// `sendUserMessage` 的投递方式（Rust-only helper）。
///
/// TS: `"steer" | "followUp"`（没有 `nextTurn`）。
///
/// 运行期映射（`kiana-session` 的 `send_user_message`）：`Steer` / `FollowUp` 分别转成
/// `StreamingBehavior::Steer` / `StreamingBehavior::FollowUp`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SendUserMessageDeliverAs {
    /// `"steer"`。
    #[default]
    #[serde(rename = "steer")]
    Steer,
    /// `"followUp"`。
    #[serde(rename = "followUp")]
    FollowUp,
}

/// `newSession` 的选项（Rust-only helper）。
///
/// TS: `{ parentSession?: string; setup?: (sessionManager: SessionManager) => Promise<void>;
/// withSession?: (ctx: ReplacedSessionContext) => Promise<void> }`。
#[derive(Clone, Default)]
pub struct NewSessionOptions {
    /// 父会话文件。
    pub parent_session: Option<String>,
    /// 初始化回调（拿到可写的 [`SessionManager`]）。
    pub setup: Option<SetupFn>,
    /// 会话替换后的回调。
    pub with_session: Option<WithSessionFn>,
}

impl std::fmt::Debug for NewSessionOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewSessionOptions")
            .field("parent_session", &self.parent_session)
            .field("setup", &self.setup.is_some())
            .field("with_session", &self.with_session.is_some())
            .finish()
    }
}

/// `setup(sessionManager: SessionManager) => Promise<void>` 的 Rust 形态。
///
/// `kiana-session-core::SessionManager` 的可写句柄用 `Arc<Mutex<SessionManager>>` 表达
/// （`append_*` 方法需要 `&mut self`）。
pub type SetupFn =
    Arc<dyn Fn(Arc<Mutex<SessionManager>>) -> LocalBoxFuture<'static, ()> + Send + Sync>;

/// `withSession(ctx: ReplacedSessionContext) => Promise<void>` 的 Rust 形态。
pub type WithSessionFn =
    Arc<dyn Fn(Arc<dyn ReplacedSessionContextApi>) -> LocalBoxFuture<'static, ()> + Send + Sync>;

/// `fork` 的选项（Rust-only helper）。
///
/// TS: `{ position?: "before" | "at"; withSession?: (ctx) => Promise<void> }`。
#[derive(Clone, Default)]
pub struct ForkOptions {
    /// fork 位置。
    pub position: Option<crate::events::ForkPosition>,
    /// 会话替换后的回调。
    pub with_session: Option<WithSessionFn>,
}

impl std::fmt::Debug for ForkOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ForkOptions")
            .field("position", &self.position)
            .field("with_session", &self.with_session.is_some())
            .finish()
    }
}

/// `navigateTree` 的选项（Rust-only helper）。
///
/// TS: `{ summarize?: boolean; customInstructions?: string; replaceInstructions?: boolean;
/// label?: string }`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigateTreeOptions {
    /// 是否生成摘要。
    pub summarize: Option<bool>,
    /// 摘要的自定义指令。
    pub custom_instructions: Option<String>,
    /// `customInstructions` 是否替换默认 prompt。
    pub replace_instructions: Option<bool>,
    /// 附着到 branch summary 条目的标签。
    pub label: Option<String>,
}

/// `switchSession` 的选项（Rust-only helper）。
///
/// TS: `{ withSession?: (ctx) => Promise<void> }`。
#[derive(Clone, Default)]
pub struct SwitchSessionOptions {
    /// 会话替换后的回调。
    pub with_session: Option<WithSessionFn>,
}

impl std::fmt::Debug for SwitchSessionOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SwitchSessionOptions")
            .field("with_session", &self.with_session.is_some())
            .finish()
    }
}

/// 会话控制方法的返回值。TS: `Promise<{ cancelled: boolean }>`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResult {
    /// 用户是否取消了操作。
    pub cancelled: bool,
}
