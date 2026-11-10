//! SessionStartEvent/ProjectTrustEvent/ExtensionEvent。
//!
//! TS 源：`core/extensions/types.ts`（约 696–1535 行）。
//!
//! # serde 策略
//!
//! `ExtensionEvent` 不能用单个 serde tag 表达：判别式在第一层是 `type`，
//! `tool_call`/`tool_result` 在第二层是 `toolName`，`before_agent_start` 在第二层是
//! `messageType`。因此五个联合（[`ExtensionEvent`]、[`SessionEvent`]、
//! [`BeforeAgentStartEvent`]、[`ToolCallEvent`]、[`ToolResultEvent`]）手写
//! `Serialize`/`Deserialize`，各自按 TS 联合顺序分派；每个叶子结构体仍由 derive 处理。
//!
//! 每个事件结构体都带一个固定判别字段 `event_type`（字段名 `event_type`，`serde(rename = "type")`），
//! 类型是只有一个变体的 unit 枚举（对照 `kiana_ai::types::TextKind`），因此无法写错字面量。
//! `tool_call`/`tool_result` 的 9 个结构体另有固定的 `tool_name` unit 枚举（自定义工具用 `String`）。
//!
//! `AbortSignal` 字段（[`SessionBeforeCompactEvent::signal`]、[`SessionBeforeTreeEvent::signal`]）
//! 是 [`CancellationToken`]，不参与 serde：序列化时省略 `signal` 键，反序列化时新建一个未取消的
//! token（`ARCHITECTURE.md` §4.2；这是与 TS `JSON.stringify(AbortSignal) === "{}"` 的已记录偏离）。
//!
//! # 这个文件负责什么
//!
//! 只定义事件的**数据形状与判别规则**：结构体、两级联合、固定判别值、手写 serde，以及
//! [`ExtensionEventKind`]、[`OptionalPatch`] 等辅助类型。这里没有派发、合并或错误上报逻辑：
//! `kiana-extensions` 的 `runner::dispatch` 负责把事件序列化成 JSON 交给 handler，再把
//! handler 的返回值反序列化成 [`ExtensionEventResult`]。因此本文件的 serde 形状就是
//! **跨语言线上契约**：改字段名、键序或缺省规则会直接改变扩展看到的对象。
//!
//! # 在系统中的位置
//!
//! - 上游（事件产生方）：主要是 `kiana-session`，它在会话/agent 生命周期各节点构造事件并
//!   调用 `ExtensionRunner::emit`（例如 `AgentSession` 用当前分支构造
//!   `SessionBeforeCompactEvent`，并把压缩的中止 token 赋给 `signal`）；UI 层也会构造
//!   部分事件（如 `kiana-interactive` 的 `UserBashEvent`）。`kiana-config` 的 cache
//!   warmer 与 `kiana-core` 的 MCP 注册表分别提供 `CacheWarmingDecisionEvent`、
//!   `RegisteredMcpServer`。
//! - 下游（事件消费方）：`kiana-extensions`（派发与结果解析）、`kiana-interactive`
//!   （UI 与 `ProjectTrustUi`）、`kiana-rpc-print`、`kiana-builtin-extensions` 等。
//! - crate 根 barrel（`index.rs`）只再导出 TS `types.ts` 的 186 个公开名字；
//!   [`ExtensionEventKind`]、[`OptionalPatch`]、`*EventType` 等 Rust-only helper 只能经
//!   `kiana_ext_api::events::...` 路径访问。
//!
//! # 一次事件的生命周期
//!
//! 1. 产生方构造叶子结构体；`type` 字段用单变体的 unit 枚举，构造时写不错字面量。
//! 2. 需要整体传递时，联合的 `Serialize` 逐个变体转发给叶子结构体；叶子按字段声明顺序
//!    写出，`Option::None` 且带 `skip_serializing_if` 的键被省略（对应 TS 的 `undefined`）。
//! 3. 反向（JSON → Rust）时，联合先把整个对象读成 `JsonValue` 窥视判别字段：第一层是
//!    `type`，`tool_call`/`tool_result` 再看 `toolName`，`before_agent_start` 再看
//!    `messageType`；然后用 `serde_json::from_value` 解析具体结构体。未知 `type` 报错，
//!    未知 `toolName` 落到 `Custom` 变体（TS 的开放字符串联合）。
//!
//! # 修改注意
//!
//! - 新增事件名至少要同步：本文件顶部的固定判别枚举、事件结构体、[`ExtensionEventKind`]
//!   （变体 + [`ExtensionEventKind::ALL`] + `as_str` + `FromStr`）、[`ExtensionEvent`]
//!   的变体与两个手写 serde 实现，以及 `index.rs` 的再导出。`ALL` 的顺序与 `as_str`
//!   字面量被 `tests/event_guards.rs` 的两个测试钉住（与 TS `on()` 重载顺序一致）。
//! - “键缺省”与“键存在但值为 `null`”是两个不同概念：前者用
//!   `Option` + `skip_serializing_if`（或 [`OptionalPatch::Absent`]）表达，后者在
//!   `tool_result` 补丁里用 [`OptionalPatch::Null`] 保留。详见 [`OptionalPatch`] 的文档。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    Api, AssistantMessageEvent, ImageContent, JsonObject, JsonValue, Message as AiMessage, Model,
    ModelThinkingLevel, ProviderHeaders, ProviderId, ToolResultMessage, Usage, UserContent,
};
use kiana_compaction::{CompactionPreparation, CompactionResult};
use kiana_config::cache::warmer::CacheWarmingAction;
// TS: `types.ts` 在事件段内再导出 `core/cache-warmer.ts` 的两个名字（types.ts:60/1452）。
// 事件联合与 [`ExtensionEventResult::CacheWarming`] 都暴露它们，因此本模块也必须是
// 可用路径：`kiana_ext_api::events::CacheWarmingDecisionEvent`。
pub use kiana_config::cache::warmer::{CacheWarmingDecisionEvent, CacheWarmingDecisionEventResult};
use kiana_core::mcp_servers::{RegisteredMcpServer, validate_mcp_server_config};
use kiana_session_core::messages::CustomMessage;
use kiana_session_core::session_manager::{
    BranchSummaryEntry, CompactionEntry, ContextEditReplacement, ProjectedSessionEntry,
    SessionEntry,
};
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio_util::sync::CancellationToken;

use crate::LocalBoxFuture;
use crate::system_prompt::NormalizedBuildSystemPromptOptions;
use crate::tool_result::{BashOperations, BashResult};
use crate::tool_types::{
    BashToolDetails, BashToolInput, BashToolName, EditToolDetails, EditToolInput, EditToolName,
    FindToolDetails, FindToolInput, FindToolName, GrepToolDetails, GrepToolInput, GrepToolName,
    LsToolDetails, LsToolInput, LsToolName, PowerShellToolDetails, PowerShellToolInput,
    PowerShellToolName, ReadToolDetails, ReadToolInput, ReadToolName, WriteToolInput,
    WriteToolName,
};
use crate::types::{ExtensionContextApi, ExtensionMode};

// ============================================================================
// 固定判别值（unit 枚举，对照 kiana_ai::types::TextKind）
// ============================================================================

// 每个事件结构体的判别字段都用一个“只有一个变体的 unit 枚举”表示：类型层面把线上
// 字面量固定下来（写错 `type` 无法编译），`Default` 便于构造，`Serialize`/`Deserialize`
// 经 `#[serde(rename = ...)]` 固定为线上字面量（当前稳定契约，改动需同步测试）。41 个
// 枚举由宏生成，避免手写漂移。
macro_rules! event_type_enum {
    ($(#[$enum_doc:meta])* $name:ident, $variant:ident, $literal:literal, $variant_doc:literal) => {
        $(#[$enum_doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
        pub enum $name {
            #[doc = $variant_doc]
            #[default]
            #[serde(rename = $literal)]
            $variant,
        }
    };
}

event_type_enum!(
    /// `Type` 字段的固定值 `"project_trust"`。
    ProjectTrustEventType,
    ProjectTrust,
    "project_trust",
    "`\"project_trust\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"resources_discover"`。
    ResourcesDiscoverEventType,
    ResourcesDiscover,
    "resources_discover",
    "`\"resources_discover\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"mcp_servers_change"`。
    McpServersChangeEventType,
    McpServersChange,
    "mcp_servers_change",
    "`\"mcp_servers_change\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_start"`。
    SessionStartEventType,
    SessionStart,
    "session_start",
    "`\"session_start\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_info_changed"`。
    SessionInfoChangedEventType,
    SessionInfoChanged,
    "session_info_changed",
    "`\"session_info_changed\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_before_switch"`。
    SessionBeforeSwitchEventType,
    SessionBeforeSwitch,
    "session_before_switch",
    "`\"session_before_switch\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_before_fork"`。
    SessionBeforeForkEventType,
    SessionBeforeFork,
    "session_before_fork",
    "`\"session_before_fork\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_before_compact"`。
    SessionBeforeCompactEventType,
    SessionBeforeCompact,
    "session_before_compact",
    "`\"session_before_compact\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_compact"`。
    SessionCompactEventType,
    SessionCompact,
    "session_compact",
    "`\"session_compact\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_compact_failed"`。
    SessionCompactFailedEventType,
    SessionCompactFailed,
    "session_compact_failed",
    "`\"session_compact_failed\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_shutdown"`。
    SessionShutdownEventType,
    SessionShutdown,
    "session_shutdown",
    "`\"session_shutdown\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_before_tree"`。
    SessionBeforeTreeEventType,
    SessionBeforeTree,
    "session_before_tree",
    "`\"session_before_tree\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"session_tree"`。
    SessionTreeEventType,
    SessionTree,
    "session_tree",
    "`\"session_tree\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"context"`。
    ContextEventType,
    Context,
    "context",
    "`\"context\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"context_with_system"`。
    ContextWithSystemEventType,
    ContextWithSystem,
    "context_with_system",
    "`\"context_with_system\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"cache_warming_decision"`。
    CacheWarmingDecisionEventType,
    CacheWarmingDecision,
    "cache_warming_decision",
    "`\"cache_warming_decision\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"before_provider_request"`。
    BeforeProviderRequestEventType,
    BeforeProviderRequest,
    "before_provider_request",
    "`\"before_provider_request\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"before_provider_headers"`。
    BeforeProviderHeadersEventType,
    BeforeProviderHeaders,
    "before_provider_headers",
    "`\"before_provider_headers\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"after_provider_response"`。
    AfterProviderResponseEventType,
    AfterProviderResponse,
    "after_provider_response",
    "`\"after_provider_response\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"provider_stream_event"`。
    ProviderStreamEventType,
    ProviderStreamEvent,
    "provider_stream_event",
    "`\"provider_stream_event\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"before_agent_start"`。
    BeforeAgentStartEventType,
    BeforeAgentStart,
    "before_agent_start",
    "`\"before_agent_start\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"agent_start"`。
    AgentStartEventType,
    AgentStart,
    "agent_start",
    "`\"agent_start\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"agent_end"`。
    AgentEndEventType,
    AgentEnd,
    "agent_end",
    "`\"agent_end\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"agent_before_settle"`。
    AgentBeforeSettleEventType,
    AgentBeforeSettle,
    "agent_before_settle",
    "`\"agent_before_settle\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"agent_settled"`。
    AgentSettledEventType,
    AgentSettled,
    "agent_settled",
    "`\"agent_settled\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"ui_prompt_start"`。
    UIPromptStartEventType,
    UIPromptStart,
    "ui_prompt_start",
    "`\"ui_prompt_start\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"ui_prompt_end"`。
    UIPromptEndEventType,
    UIPromptEnd,
    "ui_prompt_end",
    "`\"ui_prompt_end\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"turn_start"`。
    TurnStartEventType,
    TurnStart,
    "turn_start",
    "`\"turn_start\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"turn_end"`。
    TurnEndEventType,
    TurnEnd,
    "turn_end",
    "`\"turn_end\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"message_start"`。
    MessageStartEventType,
    MessageStart,
    "message_start",
    "`\"message_start\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"message_update"`。
    MessageUpdateEventType,
    MessageUpdate,
    "message_update",
    "`\"message_update\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"message_end"`。
    MessageEndEventType,
    MessageEnd,
    "message_end",
    "`\"message_end\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"tool_execution_start"`。
    ToolExecutionStartEventType,
    ToolExecutionStart,
    "tool_execution_start",
    "`\"tool_execution_start\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"tool_execution_update"`。
    ToolExecutionUpdateEventType,
    ToolExecutionUpdate,
    "tool_execution_update",
    "`\"tool_execution_update\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"tool_execution_end"`。
    ToolExecutionEndEventType,
    ToolExecutionEnd,
    "tool_execution_end",
    "`\"tool_execution_end\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"model_select"`。
    ModelSelectEventType,
    ModelSelect,
    "model_select",
    "`\"model_select\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"thinking_level_select"`。
    ThinkingLevelSelectEventType,
    ThinkingLevelSelect,
    "thinking_level_select",
    "`\"thinking_level_select\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"user_bash"`。
    UserBashEventType,
    UserBash,
    "user_bash",
    "`\"user_bash\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"input"`。
    InputEventType,
    Input,
    "input",
    "`\"input\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"tool_call"`。
    ToolCallEventType,
    ToolCall,
    "tool_call",
    "`\"tool_call\"`。"
);
event_type_enum!(
    /// `Type` 字段的固定值 `"tool_result"`。
    ToolResultEventType,
    ToolResult,
    "tool_result",
    "`\"tool_result\"`。"
);

// ============================================================================
// 原因 / 结果枚举
// ============================================================================

/// 会话启动原因。TS: `types.ts#SessionStartEvent.reason`
/// （`"startup" | "reload" | "new" | "resume" | "fork"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStartReason {
    /// `"startup"`。
    #[default]
    Startup,
    /// `"reload"`。
    Reload,
    /// `"new"`。
    New,
    /// `"resume"`。
    Resume,
    /// `"fork"`。
    Fork,
}

/// 切换会话的原因。TS: `types.ts#SessionBeforeSwitchEvent.reason`（`"new" | "resume"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionBeforeSwitchReason {
    /// `"new"`。
    #[default]
    New,
    /// `"resume"`。
    Resume,
}

/// 压缩触发原因。TS: `types.ts#SessionBeforeCompactEvent.reason`
/// （`"manual" | "threshold" | "overflow"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    /// 手动 `/compact`。
    #[default]
    Manual,
    /// 上下文阈值。
    Threshold,
    /// 上下文溢出恢复。
    Overflow,
}

/// 会话关闭原因。TS: `types.ts#SessionShutdownEvent.reason`
/// （`"quit" | "reload" | "new" | "resume" | "fork"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionShutdownReason {
    /// `"quit"`。
    #[default]
    Quit,
    /// `"reload"`。
    Reload,
    /// `"new"`。
    New,
    /// `"resume"`。
    Resume,
    /// `"fork"`。
    Fork,
}

/// 资源发现原因。TS: `types.ts#ResourcesDiscoverEvent.reason`（`"startup" | "reload"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourcesDiscoverReason {
    /// `"startup"`。
    #[default]
    Startup,
    /// `"reload"`。
    Reload,
}

/// agent 运行的结果。TS: `types.ts#AgentActivityOutcome`
/// （`"completed" | "aborted" | "error"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentActivityOutcome {
    /// `"completed"`。
    #[default]
    Completed,
    /// `"aborted"`。
    Aborted,
    /// `"error"`。
    Error,
}

/// 阻塞式扩展 UI 提示的种类。
/// TS: `types.ts#UIPromptKind`（`"select" | "confirm" | "input" | "editor" | "custom"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UIPromptKind {
    /// `"select"`。
    #[default]
    Select,
    /// `"confirm"`。
    Confirm,
    /// `"input"`。
    Input,
    /// `"editor"`。
    Editor,
    /// `"custom"`。
    Custom,
}

/// UI 提示事件的原因字面量（Rust-only helper）。
///
/// TS: `types.ts:1044/1052` 的 `reason: "ui_prompt"`。它是多词字面量，不能用
/// `rename_all = "lowercase"`（那会得到 `"uiprompt"`），因此单独定义并逐变体 rename。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UIPromptReason {
    /// `"ui_prompt"`。
    #[default]
    #[serde(rename = "ui_prompt")]
    UiPrompt,
}

/// 流式期间输入的投递方式（Rust-only helper）。
///
/// TS: `types.ts#InputEvent.streamingBehavior`（`"steer" | "followUp"`）。`followUp` 是多词
/// 字面量，逐变体 rename。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StreamingBehavior {
    /// `"steer"`。
    #[default]
    #[serde(rename = "steer")]
    Steer,
    /// `"followUp"`。
    #[serde(rename = "followUp")]
    FollowUp,
}

/// 模型选择来源。TS: `types.ts#ModelSelectSource`（`"set" | "cycle" | "restore"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSelectSource {
    /// `"set"`。
    #[default]
    Set,
    /// `"cycle"`。
    Cycle,
    /// `"restore"`。
    Restore,
}

/// 用户输入来源。TS: `types.ts#InputSource`（`"interactive" | "rpc" | "extension"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputSource {
    /// `"interactive"`。
    #[default]
    Interactive,
    /// `"rpc"`。
    Rpc,
    /// `"extension"`。
    Extension,
}

/// fork 的位置。TS: `types.ts#SessionBeforeForkEvent.position`（`"before" | "at"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkPosition {
    /// `"before"`。
    #[default]
    Before,
    /// `"at"`。
    At,
}

/// 项目信任决定。TS: `types.ts#ProjectTrustEventDecision`（`"yes" | "no" | "undecided"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTrustEventDecision {
    /// `"yes"`。
    Yes,
    /// `"no"`。
    No,
    /// `"undecided"`。
    #[default]
    Undecided,
}

/// `project_trust` 处理器的 UI 视图（Rust-only helper）。
///
/// TS: `types.ts#ProjectTrustContext.ui` 的
/// `Pick<ExtensionUIContext, "select" | "confirm" | "input" | "notify">`。这里用四个回调字段
/// 表达，而不是再定义一套 UI trait。
#[derive(Clone)]
pub struct ProjectTrustUi {
    /// `select(title, options, opts?)`。
    pub select: Arc<
        dyn Fn(
                String,
                Vec<String>,
                Option<crate::ui::ExtensionUIDialogOptions>,
            ) -> LocalBoxFuture<'static, Option<String>>
            + Send
            + Sync,
    >,
    /// `confirm(title, message, opts?)`。
    pub confirm: Arc<
        dyn Fn(
                String,
                String,
                Option<crate::ui::ExtensionUIDialogOptions>,
            ) -> LocalBoxFuture<'static, bool>
            + Send
            + Sync,
    >,
    /// `input(title, placeholder?, opts?)`。
    pub input: Arc<
        dyn Fn(
                String,
                Option<String>,
                Option<crate::ui::ExtensionUIDialogOptions>,
            ) -> LocalBoxFuture<'static, Option<String>>
            + Send
            + Sync,
    >,
    /// `notify(message, type?)`。
    pub notify: Arc<dyn Fn(String, Option<crate::ui::NotifyKind>) + Send + Sync>,
}

impl std::fmt::Debug for ProjectTrustUi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProjectTrustUi { .. }")
    }
}

// ============================================================================
// 项目信任 / 资源发现 / MCP
// ============================================================================

/// 项目信任事件。TS: `types.ts#ProjectTrustEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEvent {
    /// 恒为 `"project_trust"`。
    #[serde(rename = "type")]
    pub event_type: ProjectTrustEventType,
    /// 工作目录。
    pub cwd: String,
}

/// 项目信任事件的结果。TS: `types.ts#ProjectTrustEventResult`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEventResult {
    /// 信任决定。
    pub trusted: ProjectTrustEventDecision,
    /// 是否记住决定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember: Option<bool>,
}

/// 项目信任处理器的上下文。TS: `types.ts#ProjectTrustContext`。
#[derive(Clone)]
pub struct ProjectTrustContext {
    /// 工作目录。
    pub cwd: String,
    /// 当前运行模式。
    pub mode: ExtensionMode,
    /// 是否有可用的对话框 UI。
    pub has_ui: bool,
    /// UI 的最小视图。
    pub ui: ProjectTrustUi,
}

impl std::fmt::Debug for ProjectTrustContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectTrustContext")
            .field("cwd", &self.cwd)
            .field("mode", &self.mode)
            .field("has_ui", &self.has_ui)
            .finish_non_exhaustive()
    }
}

/// 项目信任处理器。TS: `types.ts#ProjectTrustHandler`。
pub type ProjectTrustHandler = Arc<
    dyn Fn(
        ProjectTrustEvent,
        ProjectTrustContext,
    ) -> LocalBoxFuture<'static, ProjectTrustEventResult>,
>;

/// 资源发现事件（`session_start` 之后触发）。TS: `types.ts#ResourcesDiscoverEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverEvent {
    /// 恒为 `"resources_discover"`。
    #[serde(rename = "type")]
    pub event_type: ResourcesDiscoverEventType,
    /// 工作目录。
    pub cwd: String,
    /// 触发原因。
    pub reason: ResourcesDiscoverReason,
}

/// `resources_discover` 的结果。TS: `types.ts#ResourcesDiscoverResult`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverResult {
    /// 额外的技能路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_paths: Option<Vec<String>>,
    /// 额外的 prompt 模板路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_paths: Option<Vec<String>>,
    /// 额外的主题路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_paths: Option<Vec<String>>,
}

/// MCP server 注册变化事件。TS: `types.ts#McpServersChangeEvent`。
///
/// `RegisteredMcpServer`/`McpServerConfig`（`kiana-core`）只实现了 `Serialize`，因此本结构体
/// 手写 serde：序列化直接写出 `serde` 形状；反序列化用
/// [`validate_mcp_server_config`] 重建配置。
#[derive(Debug, Clone, PartialEq)]
pub struct McpServersChangeEvent {
    /// 恒为 `"mcp_servers_change"`。
    pub event_type: McpServersChangeEventType,
    /// 变化后的全部已注册 server。
    pub servers: Vec<RegisteredMcpServer>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpServersChangeEventWire<'a> {
    #[serde(rename = "type")]
    event_type: McpServersChangeEventType,
    servers: &'a [RegisteredMcpServer],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisteredMcpServerDe {
    name: String,
    config: JsonValue,
    extension_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpServersChangeEventDe {
    #[serde(rename = "type")]
    _event_type: McpServersChangeEventType,
    servers: Vec<RegisteredMcpServerDe>,
}

impl Serialize for McpServersChangeEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        McpServersChangeEventWire {
            event_type: self.event_type,
            servers: &self.servers,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for McpServersChangeEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = McpServersChangeEventDe::deserialize(deserializer)?;
        // `RegisteredMcpServer` 只有 Serialize（`config` 是已校验的 `McpServerConfig`），
        // 因此这里必须逐项用 `validate_mcp_server_config` 从原始 JSON 重建。校验错误串
        // 直接变成 serde 错误：任一 server 非法会让整个事件反序列化失败，不做部分接受。
        let mut servers = Vec::with_capacity(wire.servers.len());
        for server in wire.servers {
            let config = validate_mcp_server_config(&server.name, &server.config)
                .map_err(D::Error::custom)?;
            servers.push(RegisteredMcpServer {
                name: server.name,
                config,
                extension_path: server.extension_path,
            });
        }
        Ok(McpServersChangeEvent {
            event_type: McpServersChangeEventType::McpServersChange,
            servers,
        })
    }
}

// ============================================================================
// 会话事件
// ============================================================================

/// 会话启动/加载/重载事件。TS: `types.ts#SessionStartEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartEvent {
    /// 恒为 `"session_start"`。
    #[serde(rename = "type")]
    pub event_type: SessionStartEventType,
    /// 触发原因。
    pub reason: SessionStartReason,
    /// 之前活跃的会话文件（`new`/`resume`/`fork` 时存在）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_session_file: Option<String>,
}

/// 会话元数据变化事件。TS: `types.ts#SessionInfoChangedEvent`。
///
/// `name` 是 `string | undefined`：**省略键**（不是 `null`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoChangedEvent {
    /// 恒为 `"session_info_changed"`。
    #[serde(rename = "type")]
    pub event_type: SessionInfoChangedEventType,
    /// 归一化后的会话名；清除时省略键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 切换会话前事件（可取消）。TS: `types.ts#SessionBeforeSwitchEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeSwitchEvent {
    /// 恒为 `"session_before_switch"`。
    #[serde(rename = "type")]
    pub event_type: SessionBeforeSwitchEventType,
    /// 原因。
    pub reason: SessionBeforeSwitchReason,
    /// 目标会话文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_file: Option<String>,
}

/// fork 会话前事件（可取消）。TS: `types.ts#SessionBeforeForkEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeForkEvent {
    /// 恒为 `"session_before_fork"`。
    #[serde(rename = "type")]
    pub event_type: SessionBeforeForkEventType,
    /// fork 的条目 id。
    pub entry_id: String,
    /// fork 位置。
    pub position: ForkPosition,
}

/// 上下文压缩前事件（可取消或自定义）。TS: `types.ts#SessionBeforeCompactEvent`。
///
/// 构造方（`kiana-session` 的 `AgentSession`）把本次压缩的中止 token 赋给 `signal`；
/// `signal` 不参与 serde（见模块文档），反序列化得到的是一个新的未取消 token。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionBeforeCompactEvent {
    /// 恒为 `"session_before_compact"`。
    pub event_type: SessionBeforeCompactEventType,
    /// 压缩准备数据。
    pub preparation: CompactionPreparation,
    /// 当前分支的条目。
    pub branch_entries: Vec<SessionEntry>,
    /// 自定义指令。
    pub custom_instructions: Option<String>,
    /// 触发原因。
    pub reason: CompactionReason,
    /// 溢出恢复时被中止的 turn 是否会在压缩后重试。
    pub will_retry: bool,
    /// 取消信号（不参与 serde；反序列化为新建的未取消 token）。
    pub signal: CancellationToken,
}

// 手写 Default 而非 derive：`CompactionPreparation` 没有实现 Default，需要逐个字段
// 给出空值。
impl Default for SessionBeforeCompactEvent {
    fn default() -> Self {
        SessionBeforeCompactEvent {
            event_type: SessionBeforeCompactEventType::default(),
            preparation: CompactionPreparation {
                first_kept_entry_id: String::new(),
                messages_to_summarize: Vec::new(),
                turn_prefix_messages: Vec::new(),
                is_split_turn: false,
                tokens_before: 0,
                previous_summary: None,
                file_ops: Default::default(),
                settings: Default::default(),
            },
            branch_entries: Vec::new(),
            custom_instructions: None,
            reason: CompactionReason::default(),
            will_retry: false,
            signal: CancellationToken::new(),
        }
    }
}

impl Serialize for SessionBeforeCompactEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 手写 Wire：`signal`（CancellationToken）没有 serde 表示，必须从线上形状里剔除；
        // 其余字段按声明顺序写出，`custom_instructions` 为 `None` 时省略键。
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            #[serde(rename = "type")]
            event_type: SessionBeforeCompactEventType,
            preparation: &'a CompactionPreparation,
            branch_entries: &'a [SessionEntry],
            #[serde(skip_serializing_if = "Option::is_none")]
            custom_instructions: Option<&'a String>,
            reason: CompactionReason,
            will_retry: bool,
        }
        Wire {
            event_type: self.event_type,
            preparation: &self.preparation,
            branch_entries: &self.branch_entries,
            custom_instructions: self.custom_instructions.as_ref(),
            reason: self.reason,
            will_retry: self.will_retry,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SessionBeforeCompactEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // JSON 里没有 `signal` 键；解析出其余字段后补一个新的未取消 token（不会是
        // “已取消”状态，取消语义只能由构造方在 Rust 侧传递）。
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            #[serde(rename = "type")]
            event_type: SessionBeforeCompactEventType,
            preparation: CompactionPreparation,
            branch_entries: Vec<SessionEntry>,
            #[serde(default)]
            custom_instructions: Option<String>,
            reason: CompactionReason,
            will_retry: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(SessionBeforeCompactEvent {
            event_type: wire.event_type,
            preparation: wire.preparation,
            branch_entries: wire.branch_entries,
            custom_instructions: wire.custom_instructions,
            reason: wire.reason,
            will_retry: wire.will_retry,
            signal: CancellationToken::new(),
        })
    }
}

/// 上下文压缩成功后事件。TS: `types.ts#SessionCompactEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactEvent {
    /// 恒为 `"session_compact"`。
    #[serde(rename = "type")]
    pub event_type: SessionCompactEventType,
    /// 新增的压缩条目。
    pub compaction_entry: CompactionEntry,
    /// 是否由扩展生成。
    pub from_extension: bool,
    /// 触发原因。
    pub reason: CompactionReason,
    /// 溢出恢复时被中止的 turn 是否会在压缩后重试。
    pub will_retry: bool,
}

/// 上下文压缩失败/中止事件。TS: `types.ts#SessionCompactFailedEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactFailedEvent {
    /// 恒为 `"session_compact_failed"`。
    #[serde(rename = "type")]
    pub event_type: SessionCompactFailedEventType,
    /// 触发原因。
    pub reason: CompactionReason,
    /// 非中止原因的错误文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// 是否被取消/中止。
    pub aborted: bool,
    /// 被中止的 turn 是否会在压缩后重试。
    pub will_retry: bool,
    /// 失败内容是否来自 `session_before_compact` handler。
    pub from_extension: bool,
}

/// 扩展运行时拆除事件。TS: `types.ts#SessionShutdownEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionShutdownEvent {
    /// 恒为 `"session_shutdown"`。
    #[serde(rename = "type")]
    pub event_type: SessionShutdownEventType,
    /// 原因。
    pub reason: SessionShutdownReason,
    /// 会话替换时的目标会话文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_file: Option<String>,
}

/// 树导航准备数据。TS: `types.ts#TreePreparation`。
///
/// `oldLeafId`/`commonAncestorId` 是**必填键** `string | null`（`TreePreparation` 独有；
/// `BoundaryContextPreview` 没有这两个字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreePreparation {
    /// 目标条目 id。
    pub target_id: String,
    /// 当前 leaf id。
    pub old_leaf_id: Option<String>,
    /// 最近公共祖先 id。
    pub common_ancestor_id: Option<String>,
    /// 将被总结的条目。
    pub entries_to_summarize: Vec<SessionEntry>,
    /// 用户是否要求摘要。
    pub user_wants_summary: bool,
    /// 摘要的自定义指令。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// true 时 `customInstructions` 替换默认 prompt 而不是追加。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    /// 附着到 branch summary 条目的标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// 树导航前事件（可取消）。TS: `types.ts#SessionBeforeTreeEvent`。
///
/// 与 [`SessionBeforeCompactEvent`] 一样，`signal` 是 [`CancellationToken`]、不参与 serde；
/// 反序列化得到一个新的未取消 token。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionBeforeTreeEvent {
    /// 恒为 `"session_before_tree"`。
    pub event_type: SessionBeforeTreeEventType,
    /// 导航准备数据。
    pub preparation: TreePreparation,
    /// 取消信号（不参与 serde；反序列化为新建的未取消 token）。
    pub signal: CancellationToken,
}

// 手写理由同 [`SessionBeforeCompactEvent`]：`TreePreparation` 没有 Default，而下游测试
// 直接用 `SessionBeforeTreeEvent::default()` 构造事件。
impl Default for SessionBeforeTreeEvent {
    fn default() -> Self {
        SessionBeforeTreeEvent {
            event_type: SessionBeforeTreeEventType::default(),
            preparation: TreePreparation {
                target_id: String::new(),
                old_leaf_id: None,
                common_ancestor_id: None,
                entries_to_summarize: Vec::new(),
                user_wants_summary: false,
                custom_instructions: None,
                replace_instructions: None,
                label: None,
            },
            signal: CancellationToken::new(),
        }
    }
}

impl Serialize for SessionBeforeTreeEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 同 [`SessionBeforeCompactEvent`]：`signal` 不进线上形状，只写 `type` 与
        // `preparation`（`TreePreparation` 自身由 derive 处理）。
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            #[serde(rename = "type")]
            event_type: SessionBeforeTreeEventType,
            preparation: &'a TreePreparation,
        }
        Wire {
            event_type: self.event_type,
            preparation: &self.preparation,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SessionBeforeTreeEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 反序列化总是得到未取消的 token（JSON 没有 `signal` 键）。
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            #[serde(rename = "type")]
            event_type: SessionBeforeTreeEventType,
            preparation: TreePreparation,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(SessionBeforeTreeEvent {
            event_type: wire.event_type,
            preparation: wire.preparation,
            signal: CancellationToken::new(),
        })
    }
}

/// 树导航后事件。TS: `types.ts#SessionTreeEvent`。
///
/// `newLeafId`/`oldLeafId` 是**必填键** `string | null`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeEvent {
    /// 恒为 `"session_tree"`。
    #[serde(rename = "type")]
    pub event_type: SessionTreeEventType,
    /// 新的 leaf id。
    pub new_leaf_id: Option<String>,
    /// 旧的 leaf id。
    pub old_leaf_id: Option<String>,
    /// 新增的分支摘要条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_entry: Option<BranchSummaryEntry>,
    /// 是否由扩展触发。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_extension: Option<bool>,
}

/// 会话事件联合。TS: `types.ts#SessionEvent`（10 个）。
#[derive(Debug, Clone, PartialEq)]
// 变体大小差异来自各事件结构体的字段数；装箱会改变下游所有 match，保留直接嵌套。
#[allow(clippy::large_enum_variant)]
pub enum SessionEvent {
    /// `session_start`。
    Start(SessionStartEvent),
    /// `session_info_changed`。
    InfoChanged(SessionInfoChangedEvent),
    /// `session_before_switch`。
    BeforeSwitch(SessionBeforeSwitchEvent),
    /// `session_before_fork`。
    BeforeFork(SessionBeforeForkEvent),
    /// `session_before_compact`。
    BeforeCompact(SessionBeforeCompactEvent),
    /// `session_compact`。
    Compact(SessionCompactEvent),
    /// `session_compact_failed`。
    CompactFailed(SessionCompactFailedEvent),
    /// `session_shutdown`。
    Shutdown(SessionShutdownEvent),
    /// `session_before_tree`。
    BeforeTree(SessionBeforeTreeEvent),
    /// `session_tree`。
    Tree(SessionTreeEvent),
}

impl SessionEvent {
    /// 事件的 `type` 字面量。
    pub fn kind(&self) -> ExtensionEventKind {
        match self {
            SessionEvent::Start(_) => ExtensionEventKind::SessionStart,
            SessionEvent::InfoChanged(_) => ExtensionEventKind::SessionInfoChanged,
            SessionEvent::BeforeSwitch(_) => ExtensionEventKind::SessionBeforeSwitch,
            SessionEvent::BeforeFork(_) => ExtensionEventKind::SessionBeforeFork,
            SessionEvent::BeforeCompact(_) => ExtensionEventKind::SessionBeforeCompact,
            SessionEvent::Compact(_) => ExtensionEventKind::SessionCompact,
            SessionEvent::CompactFailed(_) => ExtensionEventKind::SessionCompactFailed,
            SessionEvent::Shutdown(_) => ExtensionEventKind::SessionShutdown,
            SessionEvent::BeforeTree(_) => ExtensionEventKind::SessionBeforeTree,
            SessionEvent::Tree(_) => ExtensionEventKind::SessionTree,
        }
    }
}

impl Serialize for SessionEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            SessionEvent::Start(event) => event.serialize(serializer),
            SessionEvent::InfoChanged(event) => event.serialize(serializer),
            SessionEvent::BeforeSwitch(event) => event.serialize(serializer),
            SessionEvent::BeforeFork(event) => event.serialize(serializer),
            SessionEvent::BeforeCompact(event) => event.serialize(serializer),
            SessionEvent::Compact(event) => event.serialize(serializer),
            SessionEvent::CompactFailed(event) => event.serialize(serializer),
            SessionEvent::Shutdown(event) => event.serialize(serializer),
            SessionEvent::BeforeTree(event) => event.serialize(serializer),
            SessionEvent::Tree(event) => event.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SessionEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 先整体读成 JsonValue 才能在不消耗字段的情况下窥视 `type`，随后同一份 value 再
        // 交给叶子结构体反序列化（因此每个事件会被解析两次）。`type` 缺失/非字符串由
        // `event_type_string` 报错；未知名字的错误文本见本函数末尾。
        let value = JsonValue::deserialize(deserializer)?;
        let kind = event_type_string(&value).map_err(D::Error::custom)?;
        match kind.as_str() {
            "session_start" => from_value(value).map(SessionEvent::Start),
            "session_info_changed" => from_value(value).map(SessionEvent::InfoChanged),
            "session_before_switch" => from_value(value).map(SessionEvent::BeforeSwitch),
            "session_before_fork" => from_value(value).map(SessionEvent::BeforeFork),
            "session_before_compact" => from_value(value).map(SessionEvent::BeforeCompact),
            "session_compact" => from_value(value).map(SessionEvent::Compact),
            "session_compact_failed" => from_value(value).map(SessionEvent::CompactFailed),
            "session_shutdown" => from_value(value).map(SessionEvent::Shutdown),
            "session_before_tree" => from_value(value).map(SessionEvent::BeforeTree),
            "session_tree" => from_value(value).map(SessionEvent::Tree),
            other => Err(D::Error::custom(format!(
                "unknown session event type: {other}"
            ))),
        }
    }
}

// ============================================================================
// Agent / provider 事件
// ============================================================================

/// 每次 LLM 调用前事件（不含 system 消息）。TS: `types.ts#ContextEvent`。
///
/// `messages` 是**过滤掉 system 消息**后的可见会话；handler 返回或原地修改的列表由派发层
/// 重新拼回一条首部 system 消息（`kiana-extensions/js/dispatch.js` 的 `emitContext`），
/// 因此这个事件里的 handler 看不到、也不能直接删除 system 消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEvent {
    /// 恒为 `"context"`。
    #[serde(rename = "type")]
    pub event_type: ContextEventType,
    /// 去掉 system 消息后的会话。
    pub messages: Vec<AgentMessage>,
}

/// 每次 LLM 调用前事件（`context` handler 之后，完整 transcript）。
/// TS: `types.ts#ContextWithSystemEvent`。
///
/// 与 [`ContextEvent`] 不同，`messages` **包含 system 消息**；handler 返回的 `messages`
/// 会直接替换本次要发送的列表（派发层对“删掉首部 system 消息”只上报错误，不中止请求）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWithSystemEvent {
    /// 恒为 `"context_with_system"`。
    #[serde(rename = "type")]
    pub event_type: ContextWithSystemEventType,
    /// 包含 system 消息的完整 transcript。
    pub messages: Vec<AgentMessage>,
}

/// provider 请求发送前事件（可替换 payload）。TS: `types.ts#BeforeProviderRequestEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderRequestEvent {
    /// 恒为 `"before_provider_request"`。
    #[serde(rename = "type")]
    pub event_type: BeforeProviderRequestEventType,
    /// 请求负载。
    pub payload: JsonValue,
}

/// provider 请求头组装后事件（handler 原地修改 `headers`，返回值被忽略）。
/// TS: `types.ts#BeforeProviderHeadersEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderHeadersEvent {
    /// 恒为 `"before_provider_headers"`。
    #[serde(rename = "type")]
    pub event_type: BeforeProviderHeadersEventType,
    /// 请求头；`null` 值删除该 header。
    pub headers: ProviderHeaders,
}

/// provider 响应收到后、流消费前事件。TS: `types.ts#AfterProviderResponseEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterProviderResponseEvent {
    /// 恒为 `"after_provider_response"`。
    #[serde(rename = "type")]
    pub event_type: AfterProviderResponseEventType,
    /// HTTP 状态码。
    pub status: u16,
    /// 响应头。
    pub headers: IndexMap<String, String>,
}

/// 解析后的 provider 流事件（归一化之前）。TS: `types.ts#ProviderStreamEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStreamEvent {
    /// 恒为 `"provider_stream_event"`。
    #[serde(rename = "type")]
    pub event_type: ProviderStreamEventType,
    /// provider id。
    pub provider: ProviderId,
    /// api id。
    pub api: Api,
    /// 模型 id。
    pub model: String,
    /// 适配器私有的流事件数据（只读）。
    pub data: JsonValue,
}

/// `before_agent_start` 的用户消息变体。
///
/// TS: `types.ts#BeforeAgentStartUserEvent`（extends `BeforeAgentStartEventBase`）。
/// 字段顺序：基类字段 `type`、`systemPrompt`、`systemPromptOptions`，然后自己的
/// `messageType`、`prompt`、`images`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartUserEvent {
    /// 恒为 `"before_agent_start"`。
    #[serde(rename = "type")]
    pub event_type: BeforeAgentStartEventType,
    /// 当前系统提示（由 `systemPromptOptions` 与之前 handler 的改动渲染）。
    pub system_prompt: String,
    /// 可变的 prompt 段（后续 handler 看到前面 handler 的改动）。
    pub system_prompt_options: NormalizedBuildSystemPromptOptions,
    /// 恒为 `"user"`。
    pub message_type: BeforeAgentStartUserMessageType,
    /// 展开后的原始用户提示。
    pub prompt: String,
    /// 用户提示附带的图片。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
}

/// `before_agent_start` 的自定义消息变体。
///
/// TS: `types.ts#BeforeAgentStartCustomEvent`（extends `BeforeAgentStartEventBase`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartCustomEvent {
    /// 恒为 `"before_agent_start"`。
    #[serde(rename = "type")]
    pub event_type: BeforeAgentStartEventType,
    /// 当前系统提示。
    pub system_prompt: String,
    /// 可变的 prompt 段。
    pub system_prompt_options: NormalizedBuildSystemPromptOptions,
    /// 恒为 `"custom"`。
    pub message_type: BeforeAgentStartCustomMessageType,
    /// 启动本次 run 的自定义消息。
    pub message: CustomMessage,
}

/// `BeforeAgentStartUserEvent.messageType` 的固定值 `"user"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BeforeAgentStartUserMessageType {
    /// `"user"`。
    #[default]
    #[serde(rename = "user")]
    User,
}

/// `BeforeAgentStartCustomEvent.messageType` 的固定值 `"custom"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BeforeAgentStartCustomMessageType {
    /// `"custom"`。
    #[default]
    #[serde(rename = "custom")]
    Custom,
}

/// `before_agent_start` 事件：按 `messageType` 区分的两级判别联合。
///
/// TS: `types.ts#BeforeAgentStartEvent = BeforeAgentStartUserEvent | BeforeAgentStartCustomEvent`。
/// 两个结构体的 `type` 都是 `"before_agent_start"`，只有 `messageType` 不同；手写
/// `Deserialize` 先读 `type` 再读 `messageType`。
#[derive(Debug, Clone, PartialEq)]
pub enum BeforeAgentStartEvent {
    /// `messageType: "user"`。
    User(BeforeAgentStartUserEvent),
    /// `messageType: "custom"`。
    Custom(BeforeAgentStartCustomEvent),
}

impl Serialize for BeforeAgentStartEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            BeforeAgentStartEvent::User(event) => event.serialize(serializer),
            BeforeAgentStartEvent::Custom(event) => event.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for BeforeAgentStartEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 两级判别：两个结构体的 `type` 都是 `"before_agent_start"`，只能靠第二层的
        // `messageType` 区分。缺 `messageType`（或不是字符串）报 missing_field，
        // 未知值报 `unknown before_agent_start messageType: ...`。
        let value = JsonValue::deserialize(deserializer)?;
        let kind = event_type_string(&value).map_err(D::Error::custom)?;
        if kind != "before_agent_start" {
            return Err(D::Error::custom(format!(
                "expected before_agent_start, got {kind}"
            )));
        }
        let message_type = value
            .get("messageType")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| D::Error::missing_field("messageType"))?;
        match message_type {
            "user" => from_value(value).map(BeforeAgentStartEvent::User),
            "custom" => from_value(value).map(BeforeAgentStartEvent::Custom),
            other => Err(D::Error::custom(format!(
                "unknown before_agent_start messageType: {other}"
            ))),
        }
    }
}

/// agent loop 开始事件。TS: `types.ts#AgentStartEvent`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentStartEvent {
    /// 恒为 `"agent_start"`。
    #[serde(rename = "type")]
    pub event_type: AgentStartEventType,
}

/// agent loop 结束事件。TS: `types.ts#AgentEndEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEndEvent {
    /// 恒为 `"agent_end"`。
    #[serde(rename = "type")]
    pub event_type: AgentEndEventType,
    /// 本次 run 的消息。
    pub messages: Vec<AgentMessage>,
}

/// 边界草稿：`custom`。TS: `types.ts#CustomEntryDraft`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntryDraft {
    /// 恒为 `"custom"`。
    #[serde(rename = "type")]
    pub event_type: CustomEntryDraftType,
    /// 扩展标识。
    pub custom_type: String,
    /// 扩展数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

/// `CustomEntryDraft.type` 的固定值 `"custom"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CustomEntryDraftType {
    /// `"custom"`。
    #[default]
    #[serde(rename = "custom")]
    Custom,
}

/// 边界草稿：`custom_message`。TS: `types.ts#CustomMessageEntryDraft`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntryDraft {
    /// 恒为 `"custom_message"`。
    #[serde(rename = "type")]
    pub event_type: CustomMessageEntryDraftType,
    /// 扩展标识。
    pub custom_type: String,
    /// 消息内容（字符串或内容块数组）。
    pub content: UserContent,
    /// TUI 是否显示。
    pub display: bool,
    /// 扩展私有数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

/// `CustomMessageEntryDraft.type` 的固定值 `"custom_message"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CustomMessageEntryDraftType {
    /// `"custom_message"`。
    #[default]
    #[serde(rename = "custom_message")]
    CustomMessage,
}

/// 边界草稿：`context_edit`。TS: `types.ts#ContextEditEntryDraft`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditEntryDraft {
    /// 恒为 `"context_edit"`。
    #[serde(rename = "type")]
    pub event_type: ContextEditEntryDraftType,
    /// 被编辑的条目 id。
    pub target_id: String,
    /// 替换内容；`null` 表示从模型上下文省略目标（**必填键**）。
    pub replacement: Option<ContextEditReplacement>,
}

/// `ContextEditEntryDraft.type` 的固定值 `"context_edit"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ContextEditEntryDraftType {
    /// `"context_edit"`。
    #[default]
    #[serde(rename = "context_edit")]
    ContextEdit,
}

/// 边界草稿：`compaction`。TS: `types.ts#CompactionEntryDraft`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntryDraft {
    /// 恒为 `"compaction"`。
    #[serde(rename = "type")]
    pub event_type: CompactionEntryDraftType,
    /// 摘要文本。
    pub summary: String,
    /// 保留区起点；`null` 表示自保持压缩（**必填键**，永远写出）。
    pub first_kept_entry_id: Option<String>,
    /// 扩展私有数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// 生成摘要的 LLM 用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// `CompactionEntryDraft.type` 的固定值 `"compaction"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompactionEntryDraftType {
    /// `"compaction"`。
    #[default]
    #[serde(rename = "compaction")]
    Compaction,
}

/// 会话边界草稿联合。TS: `types.ts#SessionBoundaryDraft`（4 个）。
#[derive(Debug, Clone, PartialEq)]
pub enum SessionBoundaryDraft {
    /// `type: "custom"`。
    Custom(CustomEntryDraft),
    /// `type: "custom_message"`。
    CustomMessage(CustomMessageEntryDraft),
    /// `type: "context_edit"`。
    ContextEdit(ContextEditEntryDraft),
    /// `type: "compaction"`。
    Compaction(CompactionEntryDraft),
}

impl Serialize for SessionBoundaryDraft {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            SessionBoundaryDraft::Custom(draft) => draft.serialize(serializer),
            SessionBoundaryDraft::CustomMessage(draft) => draft.serialize(serializer),
            SessionBoundaryDraft::ContextEdit(draft) => draft.serialize(serializer),
            SessionBoundaryDraft::Compaction(draft) => draft.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SessionBoundaryDraft {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 与其它联合相同的两段式：先读 `type` 再按名字分派；未知 type 报错——这些草稿
        // 会被 `kiana-session` 逐项追加成会话条目（`apply_boundary_drafts`），拼错不应被
        // 静默吞掉。
        let value = JsonValue::deserialize(deserializer)?;
        let kind = event_type_string(&value).map_err(D::Error::custom)?;
        match kind.as_str() {
            "custom" => from_value(value).map(SessionBoundaryDraft::Custom),
            "custom_message" => from_value(value).map(SessionBoundaryDraft::CustomMessage),
            "context_edit" => from_value(value).map(SessionBoundaryDraft::ContextEdit),
            "compaction" => from_value(value).map(SessionBoundaryDraft::Compaction),
            other => Err(D::Error::custom(format!(
                "unknown session boundary draft type: {other}"
            ))),
        }
    }
}

/// 边界上下文预览。TS: `types.ts#BoundaryContextPreview`。
///
/// `ProjectedSessionEntry`（`kiana-session-core`）没有实现 serde，因此本结构体手写 serde：
/// 每个条目按 TS 形状 `{ sourceEntry, messages }` 写出/读入。
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryContextPreview {
    /// 逐来源条目的投影。
    pub context_entries: Vec<ProjectedSessionEntry>,
    /// 上下文消息。
    pub context_messages: Vec<AgentMessage>,
    /// 发给 LLM 的消息。
    pub llm_messages: Vec<AiMessage>,
    /// 排队中的消息。
    pub pending_messages: Vec<AgentMessage>,
    /// 是否可以继续。
    pub can_continue: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectedSessionEntryWire<'a> {
    source_entry: &'a SessionEntry,
    messages: &'a [AgentMessage],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectedSessionEntryDe {
    source_entry: SessionEntry,
    messages: Vec<AgentMessage>,
}

impl Serialize for BoundaryContextPreview {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // `ProjectedSessionEntry` 没有 serde，这里按 TS 的 `{ sourceEntry, messages }`
        // 形状逐项拆写；其余字段直接借用切片。
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            context_entries: Vec<ProjectedSessionEntryWire<'a>>,
            context_messages: &'a [AgentMessage],
            llm_messages: &'a [AiMessage],
            pending_messages: &'a [AgentMessage],
            can_continue: bool,
        }
        let context_entries = self
            .context_entries
            .iter()
            .map(|entry| ProjectedSessionEntryWire {
                source_entry: &entry.source_entry,
                messages: &entry.messages,
            })
            .collect();
        Wire {
            context_entries,
            context_messages: &self.context_messages,
            llm_messages: &self.llm_messages,
            pending_messages: &self.pending_messages,
            can_continue: self.can_continue,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BoundaryContextPreview {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 与序列化镜像：把 `{ sourceEntry, messages }` 组装回 `ProjectedSessionEntry`。
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            context_entries: Vec<ProjectedSessionEntryDe>,
            context_messages: Vec<AgentMessage>,
            llm_messages: Vec<AiMessage>,
            pending_messages: Vec<AgentMessage>,
            can_continue: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(BoundaryContextPreview {
            context_entries: wire
                .context_entries
                .into_iter()
                .map(|entry| ProjectedSessionEntry {
                    source_entry: entry.source_entry,
                    messages: entry.messages,
                })
                .collect(),
            context_messages: wire.context_messages,
            llm_messages: wire.llm_messages,
            pending_messages: wire.pending_messages,
            can_continue: wire.can_continue,
        })
    }
}

/// 边界状态（`agent_before_settle`/`turn_end` 的基类）。
/// TS: `types.ts#BoundaryState`。
///
/// Rust 没有继承，因此本结构体只作为“字段清单”的单一来源：两个事件结构体把同样四个
/// 字段平铺在自己身上（不是嵌套本结构体），字段顺序也与 TS 基类展开后一致。
///
/// `continue` 是 Rust 关键字：字段名 `r#continue`，`serde(rename = "continue")`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryState {
    /// 边界草稿。
    pub entries: Vec<SessionBoundaryDraft>,
    /// 是否继续。
    #[serde(rename = "continue")]
    pub r#continue: bool,
    /// 上下文预览。
    pub context: BoundaryContextPreview,
    /// 本 turn 的结果。
    pub outcome: AgentActivityOutcome,
}

/// 边界 handler 的结果。TS: `types.ts#BoundaryResult`。
///
/// 多个 handler 的合并由派发层实现（`kiana-extensions/js/dispatch.js` 的
/// `emitBoundary`）：`entries` 取最后一个提供的结果（共享数组的原地修改也算），
/// `continue` 取最后一个非 `None` 的值，并在每次 handler 之后用新 `entries` 重建
/// `context`。`Some(true)` 表示请求再发起一次 provider 请求；`kiana-session` 在边界预览
/// 判定 `can_continue == false` 时会拒绝并上报。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryResult {
    /// 要追加的草稿。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<SessionBoundaryDraft>>,
    /// 是否确保一次“下一次 provider 请求”。
    #[serde(rename = "continue", default, skip_serializing_if = "Option::is_none")]
    pub r#continue: Option<bool>,
}

/// agent 最终结算前事件。TS: `types.ts#AgentBeforeSettleEvent`（extends `BoundaryState`）。
///
/// 字段顺序：基类字段（`entries`、`continue`、`context`、`outcome`），然后 `type`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentBeforeSettleEvent {
    /// 边界草稿。
    pub entries: Vec<SessionBoundaryDraft>,
    /// 是否继续。
    #[serde(rename = "continue")]
    pub r#continue: bool,
    /// 上下文预览。
    pub context: BoundaryContextPreview,
    /// 本 run 的结果。
    pub outcome: AgentActivityOutcome,
    /// 恒为 `"agent_before_settle"`。
    #[serde(rename = "type")]
    pub event_type: AgentBeforeSettleEventType,
}

/// agent run 完全结算后事件。TS: `types.ts#AgentSettledEvent`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentSettledEvent {
    /// 恒为 `"agent_settled"`。
    #[serde(rename = "type")]
    pub event_type: AgentSettledEventType,
    /// 是否因中止而结束（例如 Escape）。
    pub aborted: bool,
}

/// Kiana 开始等待阻塞式扩展 UI 提示的事件。TS: `types.ts#UIPromptStartEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIPromptStartEvent {
    /// 恒为 `"ui_prompt_start"`。
    #[serde(rename = "type")]
    pub event_type: UIPromptStartEventType,
    /// 恒为 `"ui_prompt"`（多词字面量，逐变体 rename）。
    pub reason: UIPromptReason,
    /// 提示种类。
    pub kind: UIPromptKind,
    /// 提示标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Kiana 不再等待阻塞式扩展 UI 提示的事件。TS: `types.ts#UIPromptEndEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIPromptEndEvent {
    /// 恒为 `"ui_prompt_end"`。
    #[serde(rename = "type")]
    pub event_type: UIPromptEndEventType,
    /// 恒为 `"ui_prompt"`。
    pub reason: UIPromptReason,
    /// 提示种类。
    pub kind: UIPromptKind,
    /// 提示标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// 每个 turn 开始时事件。TS: `types.ts#TurnStartEvent`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartEvent {
    /// 恒为 `"turn_start"`。
    #[serde(rename = "type")]
    pub event_type: TurnStartEventType,
    /// turn 下标。
    pub turn_index: u64,
    /// Unix 毫秒时间戳。
    pub timestamp: i64,
}

/// 每个 turn 结束时事件。TS: `types.ts#TurnEndEvent`（extends `BoundaryState`）。
///
/// 字段顺序：基类字段（`entries`、`continue`、`context`、`outcome`），然后自己的
/// `type`、`turnIndex`、`message`、`toolResults`、`messageEntryId`、`toolResultEntryIds`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndEvent {
    /// 边界草稿。
    pub entries: Vec<SessionBoundaryDraft>,
    /// 是否继续。
    #[serde(rename = "continue")]
    pub r#continue: bool,
    /// 上下文预览。
    pub context: BoundaryContextPreview,
    /// 本 turn 的结果。
    pub outcome: AgentActivityOutcome,
    /// 恒为 `"turn_end"`。
    #[serde(rename = "type")]
    pub event_type: TurnEndEventType,
    /// turn 下标。
    pub turn_index: u64,
    /// 本 turn 的最终消息。
    pub message: AgentMessage,
    /// 本 turn 的工具结果消息。
    pub tool_results: Vec<ToolResultMessage>,
    /// 消息条目 id。
    pub message_entry_id: String,
    /// 工具结果条目 id。
    pub tool_result_entry_ids: Vec<String>,
}

/// 消息开始事件。TS: `types.ts#MessageStartEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageStartEvent {
    /// 恒为 `"message_start"`。
    #[serde(rename = "type")]
    pub event_type: MessageStartEventType,
    /// 消息。
    pub message: AgentMessage,
}

/// 消息流式更新事件。TS: `types.ts#MessageUpdateEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageUpdateEvent {
    /// 恒为 `"message_update"`。
    #[serde(rename = "type")]
    pub event_type: MessageUpdateEventType,
    /// 消息。
    pub message: AgentMessage,
    /// provider 风格的 assistant 流事件。
    pub assistant_message_event: AssistantMessageEvent,
}

/// 消息结束事件。TS: `types.ts#MessageEndEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEndEvent {
    /// 恒为 `"message_end"`。
    #[serde(rename = "type")]
    pub event_type: MessageEndEventType,
    /// 最终消息。
    pub message: AgentMessage,
}

/// 工具开始执行事件。TS: `types.ts#ToolExecutionStartEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionStartEvent {
    /// 恒为 `"tool_execution_start"`。
    #[serde(rename = "type")]
    pub event_type: ToolExecutionStartEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 调用参数。
    pub args: JsonValue,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
}

/// 工具执行中事件。TS: `types.ts#ToolExecutionUpdateEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionUpdateEvent {
    /// 恒为 `"tool_execution_update"`。
    #[serde(rename = "type")]
    pub event_type: ToolExecutionUpdateEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 调用参数。
    pub args: JsonValue,
    /// 部分结果。
    pub partial_result: JsonValue,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
}

/// 工具执行结束事件。TS: `types.ts#ToolExecutionEndEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionEndEvent {
    /// 恒为 `"tool_execution_end"`。
    #[serde(rename = "type")]
    pub event_type: ToolExecutionEndEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 结果。
    pub result: JsonValue,
    /// 是否错误。
    pub is_error: bool,
    /// `execute()` 的耗时（毫秒，单调时钟）；工具未运行时省略键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
}

/// 模型选择事件。TS: `types.ts#ModelSelectEvent`。
///
/// `previousModel` 是 `Model | undefined`：省略键。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectEvent {
    /// 恒为 `"model_select"`。
    #[serde(rename = "type")]
    pub event_type: ModelSelectEventType,
    /// 新模型。
    pub model: Model,
    /// 之前的模型（省略键表示 undefined）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_model: Option<Model>,
    /// 选择来源。
    pub source: ModelSelectSource,
}

/// thinking level 选择事件。TS: `types.ts#ThinkingLevelSelectEvent`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelSelectEvent {
    /// 恒为 `"thinking_level_select"`。
    #[serde(rename = "type")]
    pub event_type: ThinkingLevelSelectEventType,
    /// 新的 thinking level。
    pub level: ModelThinkingLevel,
    /// 之前的 thinking level。
    pub previous_level: ModelThinkingLevel,
}

/// 用户 `!`/`!!` bash 事件。TS: `types.ts#UserBashEvent`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserBashEvent {
    /// 恒为 `"user_bash"`。
    #[serde(rename = "type")]
    pub event_type: UserBashEventType,
    /// 要执行的命令。
    pub command: String,
    /// 使用 `!!` 前缀时为 true（不进入 LLM 上下文）。
    pub exclude_from_context: bool,
    /// 工作目录。
    pub cwd: String,
}

/// 用户输入事件。TS: `types.ts#InputEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputEvent {
    /// 恒为 `"input"`。
    #[serde(rename = "type")]
    pub event_type: InputEventType,
    /// 输入文本。
    pub text: String,
    /// 附带的图片。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    /// 输入来源。
    pub source: InputSource,
    /// 流式期间如何投递；空闲时省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming_behavior: Option<StreamingBehavior>,
}

// ============================================================================
// 工具事件
// ============================================================================

/// `bash` 工具调用事件。TS: `types.ts#BashToolCallEvent`。
///
/// 字段顺序：基类 `type`、`toolCallId`、`parentToolCallId`，然后 `toolName`、`input`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"bash"`。
    pub tool_name: BashToolName,
    /// 调用参数。
    pub input: BashToolInput,
}

/// `powershell` 工具调用事件。TS: `types.ts#PowerShellToolCallEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerShellToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"powershell"`。
    pub tool_name: PowerShellToolName,
    /// 调用参数。
    pub input: PowerShellToolInput,
}

/// `read` 工具调用事件。TS: `types.ts#ReadToolCallEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"read"`。
    pub tool_name: ReadToolName,
    /// 调用参数。
    pub input: ReadToolInput,
}

/// `edit` 工具调用事件。TS: `types.ts#EditToolCallEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"edit"`。
    pub tool_name: EditToolName,
    /// 调用参数。
    pub input: EditToolInput,
}

/// `write` 工具调用事件。TS: `types.ts#WriteToolCallEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"write"`。
    pub tool_name: WriteToolName,
    /// 调用参数。
    pub input: WriteToolInput,
}

/// `grep` 工具调用事件。TS: `types.ts#GrepToolCallEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"grep"`。
    pub tool_name: GrepToolName,
    /// 调用参数。
    pub input: GrepToolInput,
}

/// `find` 工具调用事件。TS: `types.ts#FindToolCallEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"find"`。
    pub tool_name: FindToolName,
    /// 调用参数。
    pub input: FindToolInput,
}

/// `ls` 工具调用事件。TS: `types.ts#LsToolCallEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 恒为 `"ls"`。
    pub tool_name: LsToolName,
    /// 调用参数。
    pub input: LsToolInput,
}

/// 自定义工具调用事件。TS: `types.ts#CustomToolCallEvent`。
///
/// 未知 `toolName` 反序列化到本变体（TS 的开放字符串联合）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomToolCallEvent {
    /// 恒为 `"tool_call"`。
    #[serde(rename = "type")]
    pub event_type: ToolCallEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 工具名（任意字符串）。
    pub tool_name: String,
    /// 调用参数（`Record<string, unknown>`）。
    pub input: JsonObject,
}

/// 工具调用事件联合。TS: `types.ts#ToolCallEvent`（8 个内建 + 1 个自定义）。
#[derive(Debug, Clone, PartialEq)]
// 原因同 [`SessionEvent`]：各工具的 `input` 结构体大小不同，装箱会改变下游 match。
#[allow(clippy::large_enum_variant)]
pub enum ToolCallEvent {
    /// `toolName: "bash"`。
    Bash(BashToolCallEvent),
    /// `toolName: "powershell"`。
    PowerShell(PowerShellToolCallEvent),
    /// `toolName: "read"`。
    Read(ReadToolCallEvent),
    /// `toolName: "edit"`。
    Edit(EditToolCallEvent),
    /// `toolName: "write"`。
    Write(WriteToolCallEvent),
    /// `toolName: "grep"`。
    Grep(GrepToolCallEvent),
    /// `toolName: "find"`。
    Find(FindToolCallEvent),
    /// `toolName: "ls"`。
    Ls(LsToolCallEvent),
    /// 任意其它 `toolName`。
    Custom(CustomToolCallEvent),
}

impl ToolCallEvent {
    /// 事件的 `toolName`。
    pub fn tool_name(&self) -> &str {
        match self {
            ToolCallEvent::Bash(_) => "bash",
            ToolCallEvent::PowerShell(_) => "powershell",
            ToolCallEvent::Read(_) => "read",
            ToolCallEvent::Edit(_) => "edit",
            ToolCallEvent::Write(_) => "write",
            ToolCallEvent::Grep(_) => "grep",
            ToolCallEvent::Find(_) => "find",
            ToolCallEvent::Ls(_) => "ls",
            ToolCallEvent::Custom(event) => &event.tool_name,
        }
    }

    /// 事件的 `type` 字面量（恒为 `"tool_call"`）。
    pub fn kind(&self) -> ExtensionEventKind {
        ExtensionEventKind::ToolCall
    }
}

impl Serialize for ToolCallEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ToolCallEvent::Bash(event) => event.serialize(serializer),
            ToolCallEvent::PowerShell(event) => event.serialize(serializer),
            ToolCallEvent::Read(event) => event.serialize(serializer),
            ToolCallEvent::Edit(event) => event.serialize(serializer),
            ToolCallEvent::Write(event) => event.serialize(serializer),
            ToolCallEvent::Grep(event) => event.serialize(serializer),
            ToolCallEvent::Find(event) => event.serialize(serializer),
            ToolCallEvent::Ls(event) => event.serialize(serializer),
            ToolCallEvent::Custom(event) => event.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ToolCallEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `type` 必须精确等于 `tool_call`；随后按 `toolName` 字符串分派。8 个内建名进入
        // 类型化结构体（`input` 必须能反序列化成对应的 `*ToolInput`，字段类型/必填项不符
        // 会让整个事件反序列化失败）；其余任何名字（大小写不同也算）落到 `Custom`，
        // 保留原始 `JsonObject`。
        let value = JsonValue::deserialize(deserializer)?;
        let kind = event_type_string(&value).map_err(D::Error::custom)?;
        if kind != "tool_call" {
            return Err(D::Error::custom(format!("expected tool_call, got {kind}")));
        }
        let tool_name = value
            .get("toolName")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| D::Error::missing_field("toolName"))?
            .to_string();
        match tool_name.as_str() {
            "bash" => from_value(value).map(ToolCallEvent::Bash),
            "powershell" => from_value(value).map(ToolCallEvent::PowerShell),
            "read" => from_value(value).map(ToolCallEvent::Read),
            "edit" => from_value(value).map(ToolCallEvent::Edit),
            "write" => from_value(value).map(ToolCallEvent::Write),
            "grep" => from_value(value).map(ToolCallEvent::Grep),
            "find" => from_value(value).map(ToolCallEvent::Find),
            "ls" => from_value(value).map(ToolCallEvent::Ls),
            _ => from_value(value).map(ToolCallEvent::Custom),
        }
    }
}

/// `bash` 工具结果事件。TS: `types.ts#BashToolResultEvent`。
///
/// 字段顺序：基类 `type`、`toolCallId`、`parentToolCallId`、`input`、`content`、
/// `structuredContent`、`isError`、`usage`，然后 `toolName`、`details`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"bash"`。
    pub tool_name: BashToolName,
    /// 结果详情（`undefined` 时省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<BashToolDetails>,
}

/// `powershell` 工具结果事件。TS: `types.ts#PowerShellToolResultEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerShellToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"powershell"`。
    pub tool_name: PowerShellToolName,
    /// 结果详情（`undefined` 时省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<PowerShellToolDetails>,
}

/// `read` 工具结果事件。TS: `types.ts#ReadToolResultEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"read"`。
    pub tool_name: ReadToolName,
    /// 结果详情（`undefined` 时省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<ReadToolDetails>,
}

/// `edit` 工具结果事件。TS: `types.ts#EditToolResultEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"edit"`。
    pub tool_name: EditToolName,
    /// 结果详情（`undefined` 时省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<EditToolDetails>,
}

/// `write` 工具结果事件。TS: `types.ts#WriteToolResultEvent`。
///
/// TS 的 `details` 恒为 `undefined`，因此**没有** Rust 字段（JSON 里省略键）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"write"`。
    pub tool_name: WriteToolName,
}

/// `grep` 工具结果事件。TS: `types.ts#GrepToolResultEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"grep"`。
    pub tool_name: GrepToolName,
    /// 结果详情（`undefined` 时省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<GrepToolDetails>,
}

/// `find` 工具结果事件。TS: `types.ts#FindToolResultEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"find"`。
    pub tool_name: FindToolName,
    /// 结果详情（`undefined` 时省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<FindToolDetails>,
}

/// `ls` 工具结果事件。TS: `types.ts#LsToolResultEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 恒为 `"ls"`。
    pub tool_name: LsToolName,
    /// 结果详情（`undefined` 时省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<LsToolDetails>,
}

/// 自定义工具结果事件。TS: `types.ts#CustomToolResultEvent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomToolResultEvent {
    /// 恒为 `"tool_result"`。
    #[serde(rename = "type")]
    pub event_type: ToolResultEventType,
    /// 调用 id。
    pub tool_call_id: String,
    /// 嵌套调用时指向父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 调用参数。
    pub input: JsonObject,
    /// 内容块。
    pub content: Vec<kiana_ai::types::UserContentBlock>,
    /// 机器可读结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 是否错误。
    pub is_error: bool,
    /// 工具执行本身的用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 工具名（任意字符串）。
    pub tool_name: String,
    /// 结果详情（TS `unknown`，运行时可能是 `undefined`）。
    ///
    /// TS 用 `details: result.details` 构造事件（`agent-session.ts`）：没有结构化 details 的
    /// 工具给 `undefined`，`JSON.stringify` 会**省略**该键（spec `ca-extensions.md` 工具契约：
    /// “无结构化 details 用 `details: undefined`”）。因此这里是 `Option` 而不是必填
    /// `JsonValue`：缺键可反序列化，`None` 序列化时省略键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

/// 工具结果事件联合。TS: `types.ts#ToolResultEvent`（8 个内建 + 1 个自定义）。
#[derive(Debug, Clone, PartialEq)]
// 原因同 [`ToolCallEvent`]：内建结果的 `details` 类型大小不同。
#[allow(clippy::large_enum_variant)]
pub enum ToolResultEvent {
    /// `toolName: "bash"`。
    Bash(BashToolResultEvent),
    /// `toolName: "powershell"`。
    PowerShell(PowerShellToolResultEvent),
    /// `toolName: "read"`。
    Read(ReadToolResultEvent),
    /// `toolName: "edit"`。
    Edit(EditToolResultEvent),
    /// `toolName: "write"`。
    Write(WriteToolResultEvent),
    /// `toolName: "grep"`。
    Grep(GrepToolResultEvent),
    /// `toolName: "find"`。
    Find(FindToolResultEvent),
    /// `toolName: "ls"`。
    Ls(LsToolResultEvent),
    /// 任意其它 `toolName`。
    Custom(CustomToolResultEvent),
}

impl ToolResultEvent {
    /// 事件的 `toolName`。
    pub fn tool_name(&self) -> &str {
        match self {
            ToolResultEvent::Bash(_) => "bash",
            ToolResultEvent::PowerShell(_) => "powershell",
            ToolResultEvent::Read(_) => "read",
            ToolResultEvent::Edit(_) => "edit",
            ToolResultEvent::Write(_) => "write",
            ToolResultEvent::Grep(_) => "grep",
            ToolResultEvent::Find(_) => "find",
            ToolResultEvent::Ls(_) => "ls",
            ToolResultEvent::Custom(event) => &event.tool_name,
        }
    }

    /// 事件的 `type` 字面量（恒为 `"tool_result"`）。
    pub fn kind(&self) -> ExtensionEventKind {
        ExtensionEventKind::ToolResult
    }
}

impl Serialize for ToolResultEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ToolResultEvent::Bash(event) => event.serialize(serializer),
            ToolResultEvent::PowerShell(event) => event.serialize(serializer),
            ToolResultEvent::Read(event) => event.serialize(serializer),
            ToolResultEvent::Edit(event) => event.serialize(serializer),
            ToolResultEvent::Write(event) => event.serialize(serializer),
            ToolResultEvent::Grep(event) => event.serialize(serializer),
            ToolResultEvent::Find(event) => event.serialize(serializer),
            ToolResultEvent::Ls(event) => event.serialize(serializer),
            ToolResultEvent::Custom(event) => event.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ToolResultEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 与 [`ToolCallEvent`] 同一套规则：`type` 必须是 `tool_result`，内建名进入类型化
        // 结构体（字段/`details` 类型必须匹配），未知 `toolName` 落到 `Custom`。
        let value = JsonValue::deserialize(deserializer)?;
        let kind = event_type_string(&value).map_err(D::Error::custom)?;
        if kind != "tool_result" {
            return Err(D::Error::custom(format!(
                "expected tool_result, got {kind}"
            )));
        }
        let tool_name = value
            .get("toolName")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| D::Error::missing_field("toolName"))?
            .to_string();
        match tool_name.as_str() {
            "bash" => from_value(value).map(ToolResultEvent::Bash),
            "powershell" => from_value(value).map(ToolResultEvent::PowerShell),
            "read" => from_value(value).map(ToolResultEvent::Read),
            "edit" => from_value(value).map(ToolResultEvent::Edit),
            "write" => from_value(value).map(ToolResultEvent::Write),
            "grep" => from_value(value).map(ToolResultEvent::Grep),
            "find" => from_value(value).map(ToolResultEvent::Find),
            "ls" => from_value(value).map(ToolResultEvent::Ls),
            _ => from_value(value).map(ToolResultEvent::Custom),
        }
    }
}

/// 从事件 JSON 中读取 `type` 字段。
///
/// `type` 缺失或不是字符串都返回同一条错误文本（调用方用 `D::Error::custom` 转成 serde
/// 错误）。返回 `String` 是因为借用的 `JsonValue` 随后会被消费（交给具体结构体解析）。
fn event_type_string(value: &JsonValue) -> Result<String, String> {
    value
        .get("type")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| "missing string field `type`".to_string())
}

/// `serde_json::from_value` 的别名，便于在 `Deserialize` 实现里标注错误类型。
///
/// 输入是已经解析好的 `JsonValue`（联合派发时先窥视判别字段），因此这里是第二次解析；
/// 错误统一包成调用方要求的 serde 错误类型。
fn from_value<T: serde::de::DeserializeOwned, E: DeError>(value: JsonValue) -> Result<T, E> {
    serde_json::from_value(value).map_err(E::custom)
}

// ============================================================================
// 守卫
// ============================================================================

/// `tool_result` 的类型守卫：`toolName === "bash"`。
///
/// TS: `types.ts#isBashToolResult`。按**字符串**比较（自定义工具的 `toolName` 也可以是
/// `"bash"`，与 TS 的开放联合一致）。
pub fn is_bash_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "bash"
}

/// `tool_result` 的类型守卫：`toolName === "powershell"`。TS: `types.ts#isPowerShellToolResult`。
pub fn is_power_shell_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "powershell"
}

/// `tool_result` 的类型守卫：`toolName === "read"`。TS: `types.ts#isReadToolResult`。
pub fn is_read_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "read"
}

/// `tool_result` 的类型守卫：`toolName === "edit"`。TS: `types.ts#isEditToolResult`。
pub fn is_edit_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "edit"
}

/// `tool_result` 的类型守卫：`toolName === "write"`。TS: `types.ts#isWriteToolResult`。
pub fn is_write_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "write"
}

/// `tool_result` 的类型守卫：`toolName === "grep"`。TS: `types.ts#isGrepToolResult`。
pub fn is_grep_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "grep"
}

/// `tool_result` 的类型守卫：`toolName === "find"`。TS: `types.ts#isFindToolResult`。
pub fn is_find_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "find"
}

/// `tool_result` 的类型守卫：`toolName === "ls"`。TS: `types.ts#isLsToolResult`。
pub fn is_ls_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name() == "ls"
}

/// `tool_call` 按工具名的类型守卫（`event.toolName === toolName`）。
///
/// TS: `types.ts#isToolCallEventType`（8 个内建重载 + 1 个泛型重载，运行期只比较字符串）。
/// 注意：直接用 `event.tool_name() == "bash"` 收窄不成立，因为自定义事件的 `toolName` 是
/// 任意字符串；本函数是唯一正确的比较入口。
pub fn is_tool_call_event_type(tool_name: &str, event: &ToolCallEvent) -> bool {
    event.tool_name() == tool_name
}

// ============================================================================
// ExtensionEventKind
// ============================================================================

/// `ExtensionAPI.on()` 接受的 41 个事件名。
///
/// TS: `types.ts#ExtensionAPI.on` 的 41 个重载的事件名（顺序与源码一致）。
/// `before_agent_start` 只计一次（两个结构体），`tool_call`/`tool_result` 各计一次
/// （各 9 个结构体）。
///
/// 它只承载“事件名”这一个维度：不携带数据，也不能从类型上表达某个事件允许返回哪种结果
/// （那是 [`ExtensionEventResult`] 的职责）。`as_str`/`FromStr`/`Display` 三者必须与
/// [`ExtensionEventKind::ALL`] 保持同一组字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ExtensionEventKind {
    /// `"project_trust"`。
    ProjectTrust,
    /// `"resources_discover"`。
    ResourcesDiscover,
    /// `"session_start"`。
    SessionStart,
    /// `"session_info_changed"`。
    SessionInfoChanged,
    /// `"session_before_switch"`。
    SessionBeforeSwitch,
    /// `"session_before_fork"`。
    SessionBeforeFork,
    /// `"session_before_compact"`。
    SessionBeforeCompact,
    /// `"session_compact"`。
    SessionCompact,
    /// `"session_compact_failed"`。
    SessionCompactFailed,
    /// `"session_shutdown"`。
    SessionShutdown,
    /// `"mcp_servers_change"`。
    McpServersChange,
    /// `"session_before_tree"`。
    SessionBeforeTree,
    /// `"session_tree"`。
    SessionTree,
    /// `"context"`。
    Context,
    /// `"context_with_system"`。
    ContextWithSystem,
    /// `"cache_warming_decision"`。
    CacheWarmingDecision,
    /// `"before_provider_request"`。
    BeforeProviderRequest,
    /// `"before_provider_headers"`。
    BeforeProviderHeaders,
    /// `"after_provider_response"`。
    AfterProviderResponse,
    /// `"provider_stream_event"`。
    ProviderStreamEvent,
    /// `"before_agent_start"`。
    BeforeAgentStart,
    /// `"agent_start"`。
    AgentStart,
    /// `"agent_end"`。
    AgentEnd,
    /// `"agent_before_settle"`。
    AgentBeforeSettle,
    /// `"agent_settled"`。
    AgentSettled,
    /// `"ui_prompt_start"`。
    UIPromptStart,
    /// `"ui_prompt_end"`。
    UIPromptEnd,
    /// `"turn_start"`。
    TurnStart,
    /// `"turn_end"`。
    TurnEnd,
    /// `"message_start"`。
    MessageStart,
    /// `"message_update"`。
    MessageUpdate,
    /// `"message_end"`。
    MessageEnd,
    /// `"tool_execution_start"`。
    ToolExecutionStart,
    /// `"tool_execution_update"`。
    ToolExecutionUpdate,
    /// `"tool_execution_end"`。
    ToolExecutionEnd,
    /// `"model_select"`。
    ModelSelect,
    /// `"thinking_level_select"`。
    ThinkingLevelSelect,
    /// `"tool_call"`。
    ToolCall,
    /// `"tool_result"`。
    ToolResult,
    /// `"user_bash"`。
    UserBash,
    /// `"input"`。
    Input,
}

impl ExtensionEventKind {
    /// 全部 41 个事件名，按 `on()` 重载顺序。
    ///
    /// 顺序与 `as_str` 字面量被 `tests/event_guards.rs` 的
    /// `event_kind_all_matches_ts_on_overload_order` 钉住；新增事件名必须同时加入本数组，
    /// 否则按 `ALL` 线性查找事件名的调用方（如 `kiana-extensions` loader 的
    /// `event_kind_for_name`）会认不出新名字。
    pub const ALL: [ExtensionEventKind; 41] = [
        ExtensionEventKind::ProjectTrust,
        ExtensionEventKind::ResourcesDiscover,
        ExtensionEventKind::SessionStart,
        ExtensionEventKind::SessionInfoChanged,
        ExtensionEventKind::SessionBeforeSwitch,
        ExtensionEventKind::SessionBeforeFork,
        ExtensionEventKind::SessionBeforeCompact,
        ExtensionEventKind::SessionCompact,
        ExtensionEventKind::SessionCompactFailed,
        ExtensionEventKind::SessionShutdown,
        ExtensionEventKind::McpServersChange,
        ExtensionEventKind::SessionBeforeTree,
        ExtensionEventKind::SessionTree,
        ExtensionEventKind::Context,
        ExtensionEventKind::ContextWithSystem,
        ExtensionEventKind::CacheWarmingDecision,
        ExtensionEventKind::BeforeProviderRequest,
        ExtensionEventKind::BeforeProviderHeaders,
        ExtensionEventKind::AfterProviderResponse,
        ExtensionEventKind::ProviderStreamEvent,
        ExtensionEventKind::BeforeAgentStart,
        ExtensionEventKind::AgentStart,
        ExtensionEventKind::AgentEnd,
        ExtensionEventKind::AgentBeforeSettle,
        ExtensionEventKind::AgentSettled,
        ExtensionEventKind::UIPromptStart,
        ExtensionEventKind::UIPromptEnd,
        ExtensionEventKind::TurnStart,
        ExtensionEventKind::TurnEnd,
        ExtensionEventKind::MessageStart,
        ExtensionEventKind::MessageUpdate,
        ExtensionEventKind::MessageEnd,
        ExtensionEventKind::ToolExecutionStart,
        ExtensionEventKind::ToolExecutionUpdate,
        ExtensionEventKind::ToolExecutionEnd,
        ExtensionEventKind::ModelSelect,
        ExtensionEventKind::ThinkingLevelSelect,
        ExtensionEventKind::ToolCall,
        ExtensionEventKind::ToolResult,
        ExtensionEventKind::UserBash,
        ExtensionEventKind::Input,
    ];

    /// 线上字面量（当前稳定契约，改动需同步测试）。
    pub fn as_str(self) -> &'static str {
        match self {
            ExtensionEventKind::ProjectTrust => "project_trust",
            ExtensionEventKind::ResourcesDiscover => "resources_discover",
            ExtensionEventKind::SessionStart => "session_start",
            ExtensionEventKind::SessionInfoChanged => "session_info_changed",
            ExtensionEventKind::SessionBeforeSwitch => "session_before_switch",
            ExtensionEventKind::SessionBeforeFork => "session_before_fork",
            ExtensionEventKind::SessionBeforeCompact => "session_before_compact",
            ExtensionEventKind::SessionCompact => "session_compact",
            ExtensionEventKind::SessionCompactFailed => "session_compact_failed",
            ExtensionEventKind::SessionShutdown => "session_shutdown",
            ExtensionEventKind::McpServersChange => "mcp_servers_change",
            ExtensionEventKind::SessionBeforeTree => "session_before_tree",
            ExtensionEventKind::SessionTree => "session_tree",
            ExtensionEventKind::Context => "context",
            ExtensionEventKind::ContextWithSystem => "context_with_system",
            ExtensionEventKind::CacheWarmingDecision => "cache_warming_decision",
            ExtensionEventKind::BeforeProviderRequest => "before_provider_request",
            ExtensionEventKind::BeforeProviderHeaders => "before_provider_headers",
            ExtensionEventKind::AfterProviderResponse => "after_provider_response",
            ExtensionEventKind::ProviderStreamEvent => "provider_stream_event",
            ExtensionEventKind::BeforeAgentStart => "before_agent_start",
            ExtensionEventKind::AgentStart => "agent_start",
            ExtensionEventKind::AgentEnd => "agent_end",
            ExtensionEventKind::AgentBeforeSettle => "agent_before_settle",
            ExtensionEventKind::AgentSettled => "agent_settled",
            ExtensionEventKind::UIPromptStart => "ui_prompt_start",
            ExtensionEventKind::UIPromptEnd => "ui_prompt_end",
            ExtensionEventKind::TurnStart => "turn_start",
            ExtensionEventKind::TurnEnd => "turn_end",
            ExtensionEventKind::MessageStart => "message_start",
            ExtensionEventKind::MessageUpdate => "message_update",
            ExtensionEventKind::MessageEnd => "message_end",
            ExtensionEventKind::ToolExecutionStart => "tool_execution_start",
            ExtensionEventKind::ToolExecutionUpdate => "tool_execution_update",
            ExtensionEventKind::ToolExecutionEnd => "tool_execution_end",
            ExtensionEventKind::ModelSelect => "model_select",
            ExtensionEventKind::ThinkingLevelSelect => "thinking_level_select",
            ExtensionEventKind::ToolCall => "tool_call",
            ExtensionEventKind::ToolResult => "tool_result",
            ExtensionEventKind::UserBash => "user_bash",
            ExtensionEventKind::Input => "input",
        }
    }
}

impl std::fmt::Display for ExtensionEventKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for ExtensionEventKind {
    type Err = ParseExtensionEventKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // 逐个字面量精确匹配（大小写敏感）；未知名把原字符串原样存进错误类型，供调用方
        // 区分“不认识这个名字”。返回同一变体必须满足 `as_str` 往返一致（有测试锁定）。
        match value {
            "project_trust" => Ok(ExtensionEventKind::ProjectTrust),
            "resources_discover" => Ok(ExtensionEventKind::ResourcesDiscover),
            "session_start" => Ok(ExtensionEventKind::SessionStart),
            "session_info_changed" => Ok(ExtensionEventKind::SessionInfoChanged),
            "session_before_switch" => Ok(ExtensionEventKind::SessionBeforeSwitch),
            "session_before_fork" => Ok(ExtensionEventKind::SessionBeforeFork),
            "session_before_compact" => Ok(ExtensionEventKind::SessionBeforeCompact),
            "session_compact" => Ok(ExtensionEventKind::SessionCompact),
            "session_compact_failed" => Ok(ExtensionEventKind::SessionCompactFailed),
            "session_shutdown" => Ok(ExtensionEventKind::SessionShutdown),
            "mcp_servers_change" => Ok(ExtensionEventKind::McpServersChange),
            "session_before_tree" => Ok(ExtensionEventKind::SessionBeforeTree),
            "session_tree" => Ok(ExtensionEventKind::SessionTree),
            "context" => Ok(ExtensionEventKind::Context),
            "context_with_system" => Ok(ExtensionEventKind::ContextWithSystem),
            "cache_warming_decision" => Ok(ExtensionEventKind::CacheWarmingDecision),
            "before_provider_request" => Ok(ExtensionEventKind::BeforeProviderRequest),
            "before_provider_headers" => Ok(ExtensionEventKind::BeforeProviderHeaders),
            "after_provider_response" => Ok(ExtensionEventKind::AfterProviderResponse),
            "provider_stream_event" => Ok(ExtensionEventKind::ProviderStreamEvent),
            "before_agent_start" => Ok(ExtensionEventKind::BeforeAgentStart),
            "agent_start" => Ok(ExtensionEventKind::AgentStart),
            "agent_end" => Ok(ExtensionEventKind::AgentEnd),
            "agent_before_settle" => Ok(ExtensionEventKind::AgentBeforeSettle),
            "agent_settled" => Ok(ExtensionEventKind::AgentSettled),
            "ui_prompt_start" => Ok(ExtensionEventKind::UIPromptStart),
            "ui_prompt_end" => Ok(ExtensionEventKind::UIPromptEnd),
            "turn_start" => Ok(ExtensionEventKind::TurnStart),
            "turn_end" => Ok(ExtensionEventKind::TurnEnd),
            "message_start" => Ok(ExtensionEventKind::MessageStart),
            "message_update" => Ok(ExtensionEventKind::MessageUpdate),
            "message_end" => Ok(ExtensionEventKind::MessageEnd),
            "tool_execution_start" => Ok(ExtensionEventKind::ToolExecutionStart),
            "tool_execution_update" => Ok(ExtensionEventKind::ToolExecutionUpdate),
            "tool_execution_end" => Ok(ExtensionEventKind::ToolExecutionEnd),
            "model_select" => Ok(ExtensionEventKind::ModelSelect),
            "thinking_level_select" => Ok(ExtensionEventKind::ThinkingLevelSelect),
            "tool_call" => Ok(ExtensionEventKind::ToolCall),
            "tool_result" => Ok(ExtensionEventKind::ToolResult),
            "user_bash" => Ok(ExtensionEventKind::UserBash),
            "input" => Ok(ExtensionEventKind::Input),
            _ => Err(ParseExtensionEventKindError(value.to_string())),
        }
    }
}

/// [`ExtensionEventKind`] 的 `FromStr` 错误。
///
/// TS 没有对应的运行期解析（事件名是编译期字面量联合），因此错误文案是 Rust 侧新增的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseExtensionEventKindError(pub String);

impl std::fmt::Display for ParseExtensionEventKindError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "unknown extension event kind: {}", self.0)
    }
}

impl std::error::Error for ParseExtensionEventKindError {}

// ============================================================================
// ExtensionEvent
// ============================================================================

/// 全部扩展事件的联合。
///
/// TS: `types.ts#ExtensionEvent`（顺序与源码一致：ProjectTrust、ResourcesDiscover、
/// McpServersChange、SessionEvent、Context、ContextWithSystem、CacheWarmingDecision、
/// BeforeProviderRequest、BeforeProviderHeaders、AfterProviderResponse、ProviderStreamEvent、
/// BeforeAgentStart、AgentStart、AgentEnd、AgentBeforeSettle、AgentSettled、UIPromptStart、
/// UIPromptEnd、TurnStart、TurnEnd、MessageStart、MessageUpdate、MessageEnd、
/// ToolExecutionStart/Update/End、ModelSelect、ThinkingLevelSelect、UserBash、Input、
/// ToolCall、ToolResult）。
#[derive(Debug, Clone, PartialEq)]
// 事件枚举的变体大小差异来自 TS 事件形状本身（TurnEndEvent 字段最多）；
// 装箱会改变所有下游 match，因此保留直接嵌套并关闭该 lint（同 kiana-ai::Message）。
#[allow(clippy::large_enum_variant)]
pub enum ExtensionEvent {
    /// `project_trust`。
    ProjectTrust(ProjectTrustEvent),
    /// `resources_discover`。
    ResourcesDiscover(ResourcesDiscoverEvent),
    /// `mcp_servers_change`。
    McpServersChange(McpServersChangeEvent),
    /// 10 个会话事件之一。
    Session(SessionEvent),
    /// `context`。
    Context(ContextEvent),
    /// `context_with_system`。
    ContextWithSystem(ContextWithSystemEvent),
    /// `cache_warming_decision`。
    CacheWarmingDecision(CacheWarmingDecisionEvent),
    /// `before_provider_request`。
    BeforeProviderRequest(BeforeProviderRequestEvent),
    /// `before_provider_headers`。
    BeforeProviderHeaders(BeforeProviderHeadersEvent),
    /// `after_provider_response`。
    AfterProviderResponse(AfterProviderResponseEvent),
    /// `provider_stream_event`。
    ProviderStreamEvent(ProviderStreamEvent),
    /// `before_agent_start`（user 或 custom）。
    BeforeAgentStart(BeforeAgentStartEvent),
    /// `agent_start`。
    AgentStart(AgentStartEvent),
    /// `agent_end`。
    AgentEnd(AgentEndEvent),
    /// `agent_before_settle`。
    AgentBeforeSettle(AgentBeforeSettleEvent),
    /// `agent_settled`。
    AgentSettled(AgentSettledEvent),
    /// `ui_prompt_start`。
    UIPromptStart(UIPromptStartEvent),
    /// `ui_prompt_end`。
    UIPromptEnd(UIPromptEndEvent),
    /// `turn_start`。
    TurnStart(TurnStartEvent),
    /// `turn_end`。
    TurnEnd(TurnEndEvent),
    /// `message_start`。
    MessageStart(MessageStartEvent),
    /// `message_update`。
    MessageUpdate(MessageUpdateEvent),
    /// `message_end`。
    MessageEnd(MessageEndEvent),
    /// `tool_execution_start`。
    ToolExecutionStart(ToolExecutionStartEvent),
    /// `tool_execution_update`。
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    /// `tool_execution_end`。
    ToolExecutionEnd(ToolExecutionEndEvent),
    /// `model_select`。
    ModelSelect(ModelSelectEvent),
    /// `thinking_level_select`。
    ThinkingLevelSelect(ThinkingLevelSelectEvent),
    /// `user_bash`。
    UserBash(UserBashEvent),
    /// `input`。
    Input(InputEvent),
    /// 9 个工具调用事件之一。
    ToolCall(ToolCallEvent),
    /// 9 个工具结果事件之一。
    ToolResult(ToolResultEvent),
}

impl ExtensionEvent {
    /// 事件的 41 个 `on()` 名字之一。
    pub fn kind(&self) -> ExtensionEventKind {
        match self {
            ExtensionEvent::ProjectTrust(_) => ExtensionEventKind::ProjectTrust,
            ExtensionEvent::ResourcesDiscover(_) => ExtensionEventKind::ResourcesDiscover,
            ExtensionEvent::McpServersChange(_) => ExtensionEventKind::McpServersChange,
            ExtensionEvent::Session(event) => event.kind(),
            ExtensionEvent::Context(_) => ExtensionEventKind::Context,
            ExtensionEvent::ContextWithSystem(_) => ExtensionEventKind::ContextWithSystem,
            ExtensionEvent::CacheWarmingDecision(_) => ExtensionEventKind::CacheWarmingDecision,
            ExtensionEvent::BeforeProviderRequest(_) => ExtensionEventKind::BeforeProviderRequest,
            ExtensionEvent::BeforeProviderHeaders(_) => ExtensionEventKind::BeforeProviderHeaders,
            ExtensionEvent::AfterProviderResponse(_) => ExtensionEventKind::AfterProviderResponse,
            ExtensionEvent::ProviderStreamEvent(_) => ExtensionEventKind::ProviderStreamEvent,
            ExtensionEvent::BeforeAgentStart(_) => ExtensionEventKind::BeforeAgentStart,
            ExtensionEvent::AgentStart(_) => ExtensionEventKind::AgentStart,
            ExtensionEvent::AgentEnd(_) => ExtensionEventKind::AgentEnd,
            ExtensionEvent::AgentBeforeSettle(_) => ExtensionEventKind::AgentBeforeSettle,
            ExtensionEvent::AgentSettled(_) => ExtensionEventKind::AgentSettled,
            ExtensionEvent::UIPromptStart(_) => ExtensionEventKind::UIPromptStart,
            ExtensionEvent::UIPromptEnd(_) => ExtensionEventKind::UIPromptEnd,
            ExtensionEvent::TurnStart(_) => ExtensionEventKind::TurnStart,
            ExtensionEvent::TurnEnd(_) => ExtensionEventKind::TurnEnd,
            ExtensionEvent::MessageStart(_) => ExtensionEventKind::MessageStart,
            ExtensionEvent::MessageUpdate(_) => ExtensionEventKind::MessageUpdate,
            ExtensionEvent::MessageEnd(_) => ExtensionEventKind::MessageEnd,
            ExtensionEvent::ToolExecutionStart(_) => ExtensionEventKind::ToolExecutionStart,
            ExtensionEvent::ToolExecutionUpdate(_) => ExtensionEventKind::ToolExecutionUpdate,
            ExtensionEvent::ToolExecutionEnd(_) => ExtensionEventKind::ToolExecutionEnd,
            ExtensionEvent::ModelSelect(_) => ExtensionEventKind::ModelSelect,
            ExtensionEvent::ThinkingLevelSelect(_) => ExtensionEventKind::ThinkingLevelSelect,
            ExtensionEvent::UserBash(_) => ExtensionEventKind::UserBash,
            ExtensionEvent::Input(_) => ExtensionEventKind::Input,
            ExtensionEvent::ToolCall(_) => ExtensionEventKind::ToolCall,
            ExtensionEvent::ToolResult(_) => ExtensionEventKind::ToolResult,
        }
    }
}

// `CacheWarmingDecisionEvent`（kiana-config）与 `RegisteredMcpServer`（kiana-core）没有实现
// `Deserialize`/完整的 serde 面，因此这两个事件在这里手写。字段顺序均按 TS 源码里
// **对象字面量的构造顺序**（cache-warmer.ts:309：type、warmCost、missCost、
// continuationProbability、action）。
// `CacheWarmingAction`（kiana-config）同样没有 serde 实现；这里镜像成私有枚举，只在线上
// 形状里用 `warm`/`stop` 两个字面量，两个方向的转换都是穷尽 match。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum CacheWarmingActionWire {
    #[serde(rename = "warm")]
    Warm,
    #[serde(rename = "stop")]
    Stop,
}

impl From<CacheWarmingAction> for CacheWarmingActionWire {
    fn from(action: CacheWarmingAction) -> Self {
        match action {
            CacheWarmingAction::Warm => CacheWarmingActionWire::Warm,
            CacheWarmingAction::Stop => CacheWarmingActionWire::Stop,
        }
    }
}

impl From<CacheWarmingActionWire> for CacheWarmingAction {
    fn from(action: CacheWarmingActionWire) -> Self {
        match action {
            CacheWarmingActionWire::Warm => CacheWarmingAction::Warm,
            CacheWarmingActionWire::Stop => CacheWarmingAction::Stop,
        }
    }
}

// f64 字段走 `js_number`（ECMAScript `Number::toString` 的文本），让 `0.5`、整数、
// 指数区间等按扩展宿主期望的数值文本输出，而不是 serde 默认的 ryu 形式。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheWarmingDecisionEventWire {
    #[serde(rename = "type")]
    event_type: CacheWarmingDecisionEventType,
    #[serde(serialize_with = "crate::js_number::serialize_f64")]
    warm_cost: f64,
    #[serde(serialize_with = "crate::js_number::serialize_f64")]
    miss_cost: f64,
    #[serde(serialize_with = "crate::js_number::serialize_f64")]
    continuation_probability: f64,
    action: CacheWarmingActionWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheWarmingDecisionEventDe {
    #[serde(rename = "type")]
    _event_type: CacheWarmingDecisionEventType,
    warm_cost: f64,
    miss_cost: f64,
    continuation_probability: f64,
    action: CacheWarmingActionWire,
}

fn serialize_cache_warming<S: Serializer>(
    event: &CacheWarmingDecisionEvent,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    CacheWarmingDecisionEventWire {
        event_type: CacheWarmingDecisionEventType::CacheWarmingDecision,
        warm_cost: event.warm_cost,
        miss_cost: event.miss_cost,
        continuation_probability: event.continuation_probability,
        action: event.action.into(),
    }
    .serialize(serializer)
}

fn cache_warming_from_value<E: DeError>(value: JsonValue) -> Result<CacheWarmingDecisionEvent, E> {
    let wire: CacheWarmingDecisionEventDe = serde_json::from_value(value).map_err(E::custom)?;
    Ok(CacheWarmingDecisionEvent {
        warm_cost: wire.warm_cost,
        miss_cost: wire.miss_cost,
        continuation_probability: wire.continuation_probability,
        action: wire.action.into(),
    })
}

impl Serialize for ExtensionEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ExtensionEvent::ProjectTrust(event) => event.serialize(serializer),
            ExtensionEvent::ResourcesDiscover(event) => event.serialize(serializer),
            ExtensionEvent::McpServersChange(event) => event.serialize(serializer),
            ExtensionEvent::Session(event) => event.serialize(serializer),
            ExtensionEvent::Context(event) => event.serialize(serializer),
            ExtensionEvent::ContextWithSystem(event) => event.serialize(serializer),
            ExtensionEvent::CacheWarmingDecision(event) => {
                serialize_cache_warming(event, serializer)
            }
            ExtensionEvent::BeforeProviderRequest(event) => event.serialize(serializer),
            ExtensionEvent::BeforeProviderHeaders(event) => event.serialize(serializer),
            ExtensionEvent::AfterProviderResponse(event) => event.serialize(serializer),
            ExtensionEvent::ProviderStreamEvent(event) => event.serialize(serializer),
            ExtensionEvent::BeforeAgentStart(event) => event.serialize(serializer),
            ExtensionEvent::AgentStart(event) => event.serialize(serializer),
            ExtensionEvent::AgentEnd(event) => event.serialize(serializer),
            ExtensionEvent::AgentBeforeSettle(event) => event.serialize(serializer),
            ExtensionEvent::AgentSettled(event) => event.serialize(serializer),
            ExtensionEvent::UIPromptStart(event) => event.serialize(serializer),
            ExtensionEvent::UIPromptEnd(event) => event.serialize(serializer),
            ExtensionEvent::TurnStart(event) => event.serialize(serializer),
            ExtensionEvent::TurnEnd(event) => event.serialize(serializer),
            ExtensionEvent::MessageStart(event) => event.serialize(serializer),
            ExtensionEvent::MessageUpdate(event) => event.serialize(serializer),
            ExtensionEvent::MessageEnd(event) => event.serialize(serializer),
            ExtensionEvent::ToolExecutionStart(event) => event.serialize(serializer),
            ExtensionEvent::ToolExecutionUpdate(event) => event.serialize(serializer),
            ExtensionEvent::ToolExecutionEnd(event) => event.serialize(serializer),
            ExtensionEvent::ModelSelect(event) => event.serialize(serializer),
            ExtensionEvent::ThinkingLevelSelect(event) => event.serialize(serializer),
            ExtensionEvent::UserBash(event) => event.serialize(serializer),
            ExtensionEvent::Input(event) => event.serialize(serializer),
            ExtensionEvent::ToolCall(event) => event.serialize(serializer),
            ExtensionEvent::ToolResult(event) => event.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ExtensionEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 顶层分派：先看 `type`，再把同一份 JsonValue 交给对应结构体解析。10 个会话事件
        // 共享一个分支，交给 [`SessionEvent`] 自己按 `type` 再分派一次；`tool_call`/
        // `tool_result`/`before_agent_start` 的二级分派也由各自的联合完成。
        let value = JsonValue::deserialize(deserializer)?;
        let kind = event_type_string(&value).map_err(D::Error::custom)?;
        match kind.as_str() {
            "project_trust" => from_value(value).map(ExtensionEvent::ProjectTrust),
            "resources_discover" => from_value(value).map(ExtensionEvent::ResourcesDiscover),
            "mcp_servers_change" => from_value(value).map(ExtensionEvent::McpServersChange),
            "session_start"
            | "session_info_changed"
            | "session_before_switch"
            | "session_before_fork"
            | "session_before_compact"
            | "session_compact"
            | "session_compact_failed"
            | "session_shutdown"
            | "session_before_tree"
            | "session_tree" => from_value(value).map(ExtensionEvent::Session),
            "context" => from_value(value).map(ExtensionEvent::Context),
            "context_with_system" => from_value(value).map(ExtensionEvent::ContextWithSystem),
            "cache_warming_decision" => {
                cache_warming_from_value(value).map(ExtensionEvent::CacheWarmingDecision)
            }
            "before_provider_request" => {
                from_value(value).map(ExtensionEvent::BeforeProviderRequest)
            }
            "before_provider_headers" => {
                from_value(value).map(ExtensionEvent::BeforeProviderHeaders)
            }
            "after_provider_response" => {
                from_value(value).map(ExtensionEvent::AfterProviderResponse)
            }
            "provider_stream_event" => from_value(value).map(ExtensionEvent::ProviderStreamEvent),
            "before_agent_start" => from_value(value).map(ExtensionEvent::BeforeAgentStart),
            "agent_start" => from_value(value).map(ExtensionEvent::AgentStart),
            "agent_end" => from_value(value).map(ExtensionEvent::AgentEnd),
            "agent_before_settle" => from_value(value).map(ExtensionEvent::AgentBeforeSettle),
            "agent_settled" => from_value(value).map(ExtensionEvent::AgentSettled),
            "ui_prompt_start" => from_value(value).map(ExtensionEvent::UIPromptStart),
            "ui_prompt_end" => from_value(value).map(ExtensionEvent::UIPromptEnd),
            "turn_start" => from_value(value).map(ExtensionEvent::TurnStart),
            "turn_end" => from_value(value).map(ExtensionEvent::TurnEnd),
            "message_start" => from_value(value).map(ExtensionEvent::MessageStart),
            "message_update" => from_value(value).map(ExtensionEvent::MessageUpdate),
            "message_end" => from_value(value).map(ExtensionEvent::MessageEnd),
            "tool_execution_start" => from_value(value).map(ExtensionEvent::ToolExecutionStart),
            "tool_execution_update" => from_value(value).map(ExtensionEvent::ToolExecutionUpdate),
            "tool_execution_end" => from_value(value).map(ExtensionEvent::ToolExecutionEnd),
            "model_select" => from_value(value).map(ExtensionEvent::ModelSelect),
            "thinking_level_select" => from_value(value).map(ExtensionEvent::ThinkingLevelSelect),
            "user_bash" => from_value(value).map(ExtensionEvent::UserBash),
            "input" => from_value(value).map(ExtensionEvent::Input),
            "tool_call" => from_value(value).map(ExtensionEvent::ToolCall),
            "tool_result" => from_value(value).map(ExtensionEvent::ToolResult),
            other => Err(D::Error::custom(format!(
                "unknown extension event type: {other}"
            ))),
        }
    }
}

// ============================================================================
// 事件结果
// ============================================================================

/// `context`/`context_with_system` 的结果。TS: `types.ts#ContextEventResult`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEventResult {
    /// 替换后的消息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<AgentMessage>>,
}

/// `turn_end` 的结果。TS: `types.ts#TurnEndEventResult = BoundaryResult`。
pub type TurnEndEventResult = BoundaryResult;

/// `agent_before_settle` 的结果。TS: `types.ts#AgentBeforeSettleEventResult = BoundaryResult`。
pub type AgentBeforeSettleEventResult = BoundaryResult;

/// `before_provider_request` 的结果（替换后的 payload）。
/// TS: `types.ts#BeforeProviderRequestEventResult = unknown`。
pub type BeforeProviderRequestEventResult = JsonValue;

/// `tool_call` 的结果。TS: `types.ts#ToolCallEventResult`。
///
/// 多个 handler 的结果按注册顺序合并：最后一个返回非空结果的 handler 获胜；
/// 任一步返回 `block: true` 立即短路（`kiana-extensions/js/dispatch.js` 的
/// `emitToolCall`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEventResult {
    /// 阻止工具执行（要修改参数请原地改 `event.input`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    /// 阻止原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 提示 agent 在当前工具批次后停止（批次内每个最终结果都为 true 才提前结束）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// `user_bash` 的结果。TS: `types.ts#UserBashEventResult`。
///
/// 两个变体：提供自定义执行操作，或提供完整替换结果。含 [`BashOperations`] trait 对象，
/// 因此没有 serde。派发器按注册顺序取第一个返回合法结果的 handler；handler 抛出的错误
/// 会向上穿透（`kiana-extensions/js/dispatch.js` 的 `emitUserBash`）。
pub enum UserBashEventResult {
    /// `{ operations: BashOperations }`：用自定义操作执行。
    Operations(Arc<dyn BashOperations>),
    /// `{ result: BashResult }`：扩展已执行，用这个结果。
    Result(BashResult),
}

impl std::fmt::Debug for UserBashEventResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserBashEventResult::Operations(_) => formatter.write_str("Operations(..)"),
            UserBashEventResult::Result(result) => {
                formatter.debug_tuple("Result").field(result).finish()
            }
        }
    }
}

/// 一个“可能是 `undefined`、也可能显式为 `null`”的可选补丁值。
///
/// TS 的 `tool_result` 结果区分**键缺省**、**键存在但值为 `undefined`** 与**键存在且值为
/// `null`**：`undefined` 会删除 `structuredContent`（`types.ts#ToolResultEventResult` 的
/// 文档），而 `null` 与其它值一样会被应用（`runner.ts` 的 `emitToolResult` 用
/// `r.X !== undefined` 判断，`null` 满足该条件）。JSON 无法表达 `undefined`，因此：
///
/// - 内存中保留四态：`Absent`（键缺省）、`Undefined`（键存在、值为 undefined）、
///   `Null`（键存在、值为 null 但 `T` 无法表示 null）、`Value(T)`；
/// - 序列化时 `Absent` 与 `Undefined` **都省略**键（与 `JSON.stringify` 相同），
///   `Null` 写出 `null`；
/// - JSON 反序列化时**缺键 → `Absent`**；显式 `null` **永远是值**（绝不退化为
///   `Undefined`）：`T` 能表示 `null` 时（例如 `JsonValue`、`Option<_>`）得到
///   `Value(T)`，否则得到 [`OptionalPatch::Null`]。`Null` 保留“handler 显式写了
///   `null`”这一事实（JS 侧 `emitToolResult` 会把 `null` 写回事件对象），但最终应用
///   补丁时按 TS 的 `??` 语义把 `null` 与“未提供”同等对待：`kiana-session` 的
///   `after_tool_call` 只采用 `Value`，`kiana-agent-core` 的 `AfterToolCallResult`
///   文档也约定 JSON `null` 与 `undefined` 都表示沿用已执行结果。
///
/// `undefined`（键存在、值为 undefined）只能用 Rust 代码或 JS 桥直接构造；JSON 里没有
/// 对应形态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OptionalPatch<T> {
    /// 键缺省（保持原值）。
    #[default]
    Absent,
    /// 键存在但值为 `undefined`（删除）。
    Undefined,
    /// 键存在且值为 `null`，但 `T` 无法表示 `null`（保留“显式 null”这一事实；
    /// `tool_result` 的最终应用按 TS 的 `??` 把 null 与缺省同等对待，见类型文档）。
    Null,
    /// 新值。
    Value(T),
}

impl<T> OptionalPatch<T> {
    /// 是否为 `Absent`。
    pub fn is_absent(&self) -> bool {
        matches!(self, OptionalPatch::Absent)
    }

    /// 是否为 `Undefined`。
    pub fn is_undefined(&self) -> bool {
        matches!(self, OptionalPatch::Undefined)
    }

    /// 是否为显式 `null`（`T` 不能表示 null 的字段）。
    pub fn is_null(&self) -> bool {
        matches!(self, OptionalPatch::Null)
    }

    /// 是否在序列化时省略该键（`Absent`/`Undefined` 都省略；`Null` 不省略）。
    pub fn is_absent_or_undefined(&self) -> bool {
        matches!(self, OptionalPatch::Absent | OptionalPatch::Undefined)
    }

    /// 取出新值（其余状态返回 `None`）。
    pub fn value(&self) -> Option<&T> {
        match self {
            OptionalPatch::Value(value) => Some(value),
            OptionalPatch::Absent | OptionalPatch::Undefined | OptionalPatch::Null => None,
        }
    }
}

impl<T: Serialize> Serialize for OptionalPatch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // `Absent`/`Undefined` 由字段层的 `skip_serializing_if` 省略；若被直接序列化，
            // 两者按 `JSON.stringify` 的观察结果写出 null。
            OptionalPatch::Absent | OptionalPatch::Undefined | OptionalPatch::Null => {
                serializer.serialize_none()
            }
            OptionalPatch::Value(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for OptionalPatch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 本方法只在键**存在**时被调用；键缺省由使用方字段上的 `#[serde(default)]` 直接
        // 得到 `Absent`，不会走到这里。
        let value = JsonValue::deserialize(deserializer)?;
        if value.is_null() {
            // `null` 是普通值（TS 的 `!== undefined` 判断会应用它），不是 `undefined`：
            // `T` 能表示 `null` 时（例如 `JsonValue`）保留为 `Value(null)`；不能表示时
            // （`bool`/`Vec`/对象）保留为 `Null`，由应用方按“设为 null”处理。绝不静默丢弃。
            // 判断方式就是真的把 `null` 反序列化成 `T`：成功即认为 `T` 能表示 null。
            return Ok(serde_json::from_value(JsonValue::Null)
                .map(OptionalPatch::Value)
                .unwrap_or(OptionalPatch::Null));
        }
        serde_json::from_value(value)
            .map(OptionalPatch::Value)
            .map_err(D::Error::custom)
    }
}

/// `tool_result` handler 对结果的修改。TS: `types.ts#ToolResultEventResult`。
///
/// 五个字段都使用 [`OptionalPatch`]：省略的字段保持原样；替换 `content` 而未同时返回
/// `structuredContent` 会**丢弃**结构化内容（因为可能不再匹配）。
///
/// 多个 handler 按注册顺序把补丁作用到同一个事件上（`kiana-extensions/js/dispatch.js`
/// 的 `emitToolResult`）；只要至少一个字段被应用，最终结果就整体返回给调用方。
///
/// 显式 `null` 与 TS 的 `emitToolResult` 一样被当作“有补丁”：`details`/
/// `structuredContent` 用 `Value(JsonValue::Null)` 表示；`content`/`isError`/`usage`
/// 的 Rust 值类型不能表示 null（分别对应 TS 运行时的 `null` 数组/布尔/用量），
/// 用 [`OptionalPatch::Null`] 保留“这个字段被显式覆盖为 null”。最终应用时 `Null` 与
/// `Absent` 没有区别（TS 的 `??` 语义）；`structuredContent` 另有规则：替换 `content`
/// 而未提供新的结构化内容时，旧值被丢弃。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEventResult {
    /// 替换内容块；`Null` 对应 TS 运行时的 `content: null`。
    #[serde(default, skip_serializing_if = "OptionalPatch::is_absent_or_undefined")]
    pub content: OptionalPatch<Vec<kiana_ai::types::UserContentBlock>>,
    /// 替换详情。
    #[serde(default, skip_serializing_if = "OptionalPatch::is_absent_or_undefined")]
    pub details: OptionalPatch<JsonValue>,
    /// 替换机器可读结果。
    #[serde(default, skip_serializing_if = "OptionalPatch::is_absent_or_undefined")]
    pub structured_content: OptionalPatch<JsonValue>,
    /// 替换错误标志；`Null` 对应 TS 运行时的 `isError: null`（合并时按 `??` 视为未提供，
    /// 不是把错误标志设为 false）。
    #[serde(default, skip_serializing_if = "OptionalPatch::is_absent_or_undefined")]
    pub is_error: OptionalPatch<bool>,
    /// 替换用量；`Null` 对应 TS 运行时的 `usage: null`。
    #[serde(default, skip_serializing_if = "OptionalPatch::is_absent_or_undefined")]
    pub usage: OptionalPatch<Usage>,
}

/// `message_end` 的结果。TS: `types.ts#MessageEndEventResult`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEndEventResult {
    /// 替换最终消息（必须保持原 role）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<AgentMessage>,
}

/// `before_agent_start` 的结果。TS: `types.ts#BeforeAgentStartEventResult`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartEventResult {
    /// 用一条自定义消息启动本次 run。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<crate::types::CustomMessagePayload>,
    /// 替换本 turn 的完整系统提示（后续 handler 看到这个覆盖值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// `session_before_switch` 的结果。TS: `types.ts#SessionBeforeSwitchResult`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeSwitchResult {
    /// 取消切换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
}

/// `session_before_fork` 的结果。TS: `types.ts#SessionBeforeForkResult`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeForkResult {
    /// 取消 fork。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    /// 跳过会话恢复。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_conversation_restore: Option<bool>,
}

/// `session_before_compact` 的结果。TS: `types.ts#SessionBeforeCompactResult`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeCompactResult {
    /// 取消压缩。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    /// 扩展提供的压缩结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionResult>,
}

/// 树摘要（Rust-only helper）。
///
/// TS: `types.ts#SessionBeforeTreeResult.summary` 的内联对象
/// `{ summary: string; details?: unknown; usage?: Usage }`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeSummary {
    /// 摘要文本。
    pub summary: String,
    /// 扩展私有数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// 生成摘要的 LLM 用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// `session_before_tree` 的结果。TS: `types.ts#SessionBeforeTreeResult`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeTreeResult {
    /// 取消导航。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    /// 扩展提供的摘要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<TreeSummary>,
    /// 覆盖摘要的自定义指令。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// 覆盖 `customInstructions` 是否替换默认 prompt。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    /// 覆盖 branch summary 条目的标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// 用户输入事件的结果。TS: `types.ts#InputEventResult`。
///
/// 内部 tag 是 `action`（`continue`/`transform`/`handled`）；`Default` 是 `Continue`，
/// 因此缺省构造序列化为 `{"action":"continue"}`。其它 `action` 值反序列化时报错。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum InputEventResult {
    /// `{ action: "continue" }`。
    #[default]
    Continue,
    /// `{ action: "transform"; text; images? }`。
    Transform {
        /// 替换后的文本。
        text: String,
        /// 附带的图片。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
    /// `{ action: "handled" }`。
    Handled,
}

// ============================================================================
// handler
// ============================================================================

/// 标准事件处理器。
///
/// TS: `types.ts#ExtensionHandler<E, R>` 的 Rust 擦除形态：
/// `(event, ctx: ExtensionContext) => Promise<R | void> | R | void`。
/// 返回 `Ok(None)` 表示没有结果（TS 的 `void`/`undefined`）；`Err` 是 handler 抛出的任意值
/// （消息逐字保留，避免丢失 handler 抛出的原始错误上下文；`ARCHITECTURE.md` §4.1）。
///
/// 非 `Send`：kiana-extensions 在单线程 `LocalSet` 上调用，且回调可能持有 `!Send` 的
/// rquickjs 句柄。
///
/// 用 `Arc<dyn Fn>` 而非 `Box`/`FnMut`：每次派发会从扩展注册表克隆 handler 放进当次
/// 快照（`kiana-extensions` 的 `snapshot_handlers`），同一 handler 会被多次调用。
pub type ExtensionHandlerFn = Arc<
    dyn Fn(
        ExtensionEvent,
        Arc<dyn ExtensionContextApi>,
    ) -> LocalBoxFuture<
        'static,
        Result<Option<ExtensionEventResult>, Box<dyn std::error::Error + Send + Sync>>,
    >,
>;

/// `on()` 的两种处理器形态。
///
/// TS 有 40 个标准 handler（`event, ctx: ExtensionContext`）与 1 个特殊
/// `project_trust` handler（`event, ctx: ProjectTrustContext`）。为了保持
/// [`crate::types::ExtensionApi::on`] 只有一个对象安全方法，用本枚举区分。
/// `kiana-extensions` 拒绝 `ProjectTrust` 用于其它事件、`Standard` 用于 `project_trust`。
///
/// 两种形态都存入 [`crate::types::Extension::handlers`]（TS 的 `HandlerFn[]` 同时容纳二者）。
#[derive(Clone)]
pub enum ExtensionHandler {
    /// 40 个标准事件使用。
    Standard(ExtensionHandlerFn),
    /// `project_trust` 使用。
    ProjectTrust(ProjectTrustHandler),
}

impl std::fmt::Debug for ExtensionHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtensionHandler::Standard(_) => formatter.write_str("Standard(..)"),
            ExtensionHandler::ProjectTrust(_) => formatter.write_str("ProjectTrust(..)"),
        }
    }
}

/// 标准 handler 的返回值。
///
/// TS 的 `on()` 返回类型 `R | void` 与事件名绑定（每个事件有自己的结果类型）。Rust 的
/// `on()` 只有一个方法，因此用一个枚举承载“有结果的 16 个标准事件”的结果；
/// `context_with_system` 复用 [`ExtensionEventResult::Context`]。
///
/// 本枚举不实现 serde：[`UserBashEventResult`] 含 `Arc<dyn BashOperations>`，没有可逆的
/// JSON 表示。handler 结果由 `kiana-extensions` 在 `runner::dispatch` 里按事件类型逐个
/// 解析（并负责拒绝“该事件不允许返回结果”的 handler）。
#[derive(Debug)]
// 结果枚举直接嵌套各事件的结果类型（与 TS 联合一一对应）；装箱会改变下游 match。
#[allow(clippy::large_enum_variant)]
pub enum ExtensionEventResult {
    /// `resources_discover`。
    ResourcesDiscover(ResourcesDiscoverResult),
    /// `session_before_switch`。
    SessionBeforeSwitch(SessionBeforeSwitchResult),
    /// `session_before_fork`。
    SessionBeforeFork(SessionBeforeForkResult),
    /// `session_before_compact`。
    SessionBeforeCompact(SessionBeforeCompactResult),
    /// `session_before_tree`。
    SessionBeforeTree(SessionBeforeTreeResult),
    /// `context` / `context_with_system`。
    Context(ContextEventResult),
    /// `cache_warming_decision`。
    CacheWarming(CacheWarmingDecisionEventResult),
    /// `before_provider_request`（替换后的 payload）。
    BeforeProviderRequest(JsonValue),
    /// `before_agent_start`。
    BeforeAgentStart(BeforeAgentStartEventResult),
    /// `turn_end`。
    TurnEnd(BoundaryResult),
    /// `agent_before_settle`。
    AgentBeforeSettle(BoundaryResult),
    /// `message_end`。
    MessageEnd(MessageEndEventResult),
    /// `tool_call`。
    ToolCall(ToolCallEventResult),
    /// `tool_result`。
    ToolResult(ToolResultEventResult),
    /// `user_bash`。
    UserBash(UserBashEventResult),
    /// `input`。
    Input(InputEventResult),
}
