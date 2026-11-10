//! 再导出面。
//!
//! TS: `core/extensions/types.ts` 的 186 个唯一导出名（175 个声明 + 11 个再导出，见
//! `spec/ca-extensions.md` §2.4）。
//!
//! 本模块是**纯 barrel**：逐项列出 TS 表面，不做 glob 再导出（模块里另有 Rust-only helper
//! ——[`crate::ui::WidgetContent`]、[`crate::types::SendMessageOptions`] 等——它们不属于
//! types.ts 的导出面，只能经模块路径访问）。
//!
//! 命名：Rust trait 使用 `*Api` 后缀（[`ExtensionContextApi`]、[`ExtensionApi`]……）与
//! `ExtensionUiContext`（`ARCHITECTURE.md` §5.2 的冻结名）；函数用 snake_case。
//! 其余名字与 TS 一一对应。
//!
//! # 为什么手写清单，而不是 glob
//!
//! TS 的 `types.ts` 是扩展作者唯一的 import 入口；Rust 侧对应地要求
//! `kiana_ext_api::` 与 `kiana_ext_api::<模块>::` 两个路径都能解析到同一批名字
//! （`tests/public_surface.rs` 逐个导入 186 个名字并在编译期检查）。
//! 如果这里用 glob 再导出，模块里的 Rust-only helper（[`crate::ui::WidgetContent`]、
//! [`crate::types::SendMessageOptions`] 等）也会出现在“TS 表面”上，
//! “公开名字 = types.ts 导出名”这条约束就失效了，因此每个名字都逐项列出。
//! crate 根的 `pub use index::*;` 只是把这批名字提升到根路径，不增加新名字。
//!
//! # 维护方式
//!
//! 新增一个 TS 导出名时：先在拥有该类型的模块写好 `///`，再加进下面相应的分组。
//! 分组注释标出了它对应的 `types.ts` 段落；跨 crate 的名字放在最后一节“顶部再导出”，
//! 与 TS 的 `export … from` 一一对应。删除或改名会让 `tests/public_surface.rs` 编译失败。
//!
//! **不要**在这里添加 loader/runner/wrapper 名字（那些属于 `kiana-extensions` 的 index），
//! 也不要添加 `SlashCommandInfo`/`SourceInfo`（types.ts 只 import 它们、不导出），
//! 以及 `Skill`/`ReadonlyFooterDataProvider`/`BashResult`/`BashOperations` 或任何
//! Rust-only helper。

// --- UI 上下文（TS: types.ts 的 UI 段）------------------------------------
pub use crate::ui::{
    AutocompleteProviderFactory, EditorFactory, ExtensionUIDialogOptions, ExtensionUiContext,
    ExtensionWidgetOptions, PermissionCardChoice, PermissionCardRequest, PermissionScopeKind,
    TerminalInputHandler, WidgetPlacement, WorkingIndicatorOptions,
};

// --- 工具类型（TS: types.ts 的 Tool Types 段）----------------------------
pub use crate::tool_types::{
    ToolAnnotations, ToolDefinition, ToolExposure, ToolLoadout, ToolLoadoutChanges, ToolNamespace,
    ToolRenderContext, ToolRenderResultOptions, ToolRendererResolver, ToolRenderers, define_tool,
};

// --- 启动 / 资源 / 会话 / agent / 工具事件与守卫 -------------------------
pub use crate::events::{
    AfterProviderResponseEvent, AgentActivityOutcome, AgentBeforeSettleEvent,
    AgentBeforeSettleEventResult, AgentEndEvent, AgentSettledEvent, AgentStartEvent,
    BashToolCallEvent, BashToolResultEvent, BeforeAgentStartCustomEvent, BeforeAgentStartEvent,
    BeforeAgentStartEventResult, BeforeAgentStartUserEvent, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, BeforeProviderRequestEventResult, BoundaryContextPreview,
    BoundaryResult, BoundaryState, CompactionEntryDraft, ContextEditEntryDraft, ContextEvent,
    ContextEventResult, ContextWithSystemEvent, CustomEntryDraft, CustomMessageEntryDraft,
    CustomToolCallEvent, CustomToolResultEvent, EditToolCallEvent, EditToolResultEvent,
    ExtensionEvent, ExtensionHandler, FindToolCallEvent, FindToolResultEvent, GrepToolCallEvent,
    GrepToolResultEvent, InputEvent, InputEventResult, InputSource, LsToolCallEvent,
    LsToolResultEvent, McpServersChangeEvent, MessageEndEvent, MessageEndEventResult,
    MessageStartEvent, MessageUpdateEvent, ModelSelectEvent, ModelSelectSource,
    PowerShellToolCallEvent, PowerShellToolResultEvent, ProjectTrustContext, ProjectTrustEvent,
    ProjectTrustEventDecision, ProjectTrustEventResult, ProjectTrustHandler, ProviderStreamEvent,
    ReadToolCallEvent, ReadToolResultEvent, ResourcesDiscoverEvent, ResourcesDiscoverResult,
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionBoundaryDraft, SessionCompactEvent,
    SessionCompactFailedEvent, SessionEvent, SessionInfoChangedEvent, SessionShutdownEvent,
    SessionStartEvent, SessionTreeEvent, ThinkingLevelSelectEvent, ToolCallEvent,
    ToolCallEventResult, ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
    ToolResultEvent, ToolResultEventResult, TreePreparation, TurnEndEvent, TurnEndEventResult,
    TurnStartEvent, UIPromptEndEvent, UIPromptKind, UIPromptStartEvent, UserBashEvent,
    UserBashEventResult, WriteToolCallEvent, WriteToolResultEvent, is_bash_tool_result,
    is_edit_tool_result, is_find_tool_result, is_grep_tool_result, is_ls_tool_result,
    is_power_shell_tool_result, is_read_tool_result, is_tool_call_event_type, is_write_tool_result,
};

// --- 上下文 / 渲染 / 命令 / API / provider / 加载产物 ---------------------
pub use crate::types::{
    AppendEntryHandler, CompactOptions, ContextUsage, EntryRenderOptions, EntryRenderer,
    ExecuteToolOptions, Extension, ExtensionActions, ExtensionApi, ExtensionCommandContextActions,
    ExtensionCommandContextApi, ExtensionContextActions, ExtensionContextApi, ExtensionError,
    ExtensionFactory, ExtensionFlag, ExtensionMode, ExtensionRuntime, ExtensionRuntimeState,
    ExtensionShortcut, ExtensionToolContextApi, ExtensionVirtualModel, GetActiveToolsHandler,
    GetAllToolsHandler, GetCommandsHandler, GetSessionNameHandler, GetSettingsHandler,
    GetThinkingLevelHandler, InlineExtension, LoadExtensionsResult, MarkdownTransformContext,
    MarkdownTransformer, MessageRenderOptions, MessageRenderer, ProviderChatModelConfig,
    ProviderClassifierModelConfig, ProviderConfig, ProviderImageModelConfig, ProviderModelConfig,
    RefreshToolsHandler, RegisteredCommand, RegisteredTool, ReplacedSessionContextApi,
    ResolvedCommand, SendMessageHandler, SendUserMessageHandler, SetActiveToolsHandler,
    SetLabelHandler, SetModelHandler, SetSessionNameHandler, SetThinkingLevelHandler, ToolInfo,
};

// --- 顶部再导出（TS: types.ts 的 4 组 + cache-warmer 一组，共 11 个名字）----

/// TS: `types.ts:1452` 再导出 `core/cache-warmer.ts` 的 2 个名字。
/// 权威再导出点是 [`crate::events`]（事件联合与 `ExtensionEventResult` 都引用它们），
/// 这里从那里转发，保证 `kiana_ext_api::events::*` 与 crate 根两个路径都可用。
pub use crate::events::{CacheWarmingDecisionEvent, CacheWarmingDecisionEventResult};
/// TS: `types.ts:105` 再导出 `core/system-prompt.ts#BuildSystemPromptOptions`。
pub use crate::system_prompt::{BuildSystemPromptOptions, NormalizedBuildSystemPromptOptions};
/// TS: `types.ts:104` 再导出 `core/exec.ts#ExecOptions`。
pub use crate::tool_result::{ExecOptions, ExecResult};
/// TS: `types.ts:106` 再导出 `@earendil-works/kiana-agent-core` 的 3 个名字。
pub use kiana_agent_core::{AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode};
/// TS: `types.ts:107` 再导出 `core/keybindings.ts` 的 2 个名字。
pub use kiana_config::keybindings::{AppKeybinding, KeybindingsManager};
