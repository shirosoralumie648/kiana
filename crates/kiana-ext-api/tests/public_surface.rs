//! 编译期可达性测试：`types.ts` 的 186 个公开名字在 `kiana_ext_api::` 与
//! `kiana_ext_api::<module>::` 两个路径上都可解析；并钉住对象安全与 [`ExtensionApi`] 的 33 个成员。
//!
//! `ARCHITECTURE.md` §4.8 规则 9：每个 crate 的公开 API 至少被一个测试触达。本文件用
//! “导入全部名字”的方式满足该规则；导入后不实际使用，因此允许 `unused_imports`
//! （局部例外，理由即本文件的唯一目的）。

#![allow(unused_imports)]

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_agent_core::ThinkingLevel;
use kiana_ai::types::{JsonValue, Model, UserContent};
use kiana_config::settings::Settings;
use kiana_core::event_bus::{EventBus, Unsubscribe};
use kiana_core::mcp_servers::{McpServerConfig, RegisteredMcpServer};
use kiana_core::slash_commands::SlashCommandInfo;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::events::{
    ExtensionEventKind, ExtensionEventResult, ExtensionHandler, ExtensionHandlerFn,
};
use kiana_ext_api::tool_result::BashOperations;
use kiana_ext_api::types::{
    FlagValue, ProviderRegistration, RegisterCommandOptions, RegisterFlagOptions,
    RegisterShortcutOptions, SendMessageOptions, SendUserMessageOptions,
};
use kiana_ext_api::ui::ReadonlyFooterDataProvider;
use kiana_ext_api::{
    EntryRenderer, ExecOptions, ExecResult, ExtensionApi, ExtensionCommandContextApi,
    ExtensionToolContextApi, ExtensionUiContext, ExtensionVirtualModel, MarkdownTransformer,
    MessageRenderer, ReplacedSessionContextApi, ToolDefinition, ToolInfo, ToolLoadout,
    ToolRendererResolver,
};

// TS 在事件段内再导出 `cache-warmer.ts` 的两个名字：`kiana_ext_api::events::*` 与 crate 根
// 必须解析到同一个类型（事件联合与 `ExtensionEventResult::CacheWarming` 都暴露它们）。
const _: fn(
    kiana_ext_api::events::CacheWarmingDecisionEvent,
) -> kiana_ext_api::CacheWarmingDecisionEvent = std::convert::identity;
const _: fn(
    kiana_ext_api::events::CacheWarmingDecisionEventResult,
) -> kiana_ext_api::CacheWarmingDecisionEventResult = std::convert::identity;

/// 根路径可达性：186 个名字全部从 `kiana_ext_api::` 解析。
mod root_surface {
    #![allow(unused_imports)]
    use kiana_ext_api::{
        AfterProviderResponseEvent, AgentActivityOutcome, AgentBeforeSettleEvent,
        AgentBeforeSettleEventResult, AgentEndEvent, AgentSettledEvent, AgentStartEvent,
        AgentToolResult, AgentToolUpdateCallback, AppKeybinding, AppendEntryHandler,
        AutocompleteProviderFactory, BashToolCallEvent, BashToolResultEvent,
        BeforeAgentStartCustomEvent, BeforeAgentStartEvent, BeforeAgentStartEventResult,
        BeforeAgentStartUserEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
        BeforeProviderRequestEventResult, BoundaryContextPreview, BoundaryResult, BoundaryState,
        BuildSystemPromptOptions, CacheWarmingDecisionEvent, CacheWarmingDecisionEventResult,
        CompactOptions, CompactionEntryDraft, ContextEditEntryDraft, ContextEvent,
        ContextEventResult, ContextUsage, ContextWithSystemEvent, CustomEntryDraft,
        CustomMessageEntryDraft, CustomToolCallEvent, CustomToolResultEvent, EditToolCallEvent,
        EditToolResultEvent, EditorFactory, EntryRenderOptions, EntryRenderer, ExecOptions,
        ExecResult, ExecuteToolOptions, Extension, ExtensionActions, ExtensionApi,
        ExtensionCommandContextActions, ExtensionCommandContextApi, ExtensionContextActions,
        ExtensionContextApi, ExtensionError, ExtensionEvent, ExtensionFactory, ExtensionFlag,
        ExtensionHandler, ExtensionMode, ExtensionRuntime, ExtensionRuntimeState,
        ExtensionShortcut, ExtensionToolContextApi, ExtensionUIDialogOptions, ExtensionUiContext,
        ExtensionVirtualModel, ExtensionWidgetOptions, FindToolCallEvent, FindToolResultEvent,
        GetActiveToolsHandler, GetAllToolsHandler, GetCommandsHandler, GetSessionNameHandler,
        GetSettingsHandler, GetThinkingLevelHandler, GrepToolCallEvent, GrepToolResultEvent,
        InlineExtension, InputEvent, InputEventResult, InputSource, KeybindingsManager,
        LoadExtensionsResult, LsToolCallEvent, LsToolResultEvent, MarkdownTransformContext,
        MarkdownTransformer, McpServersChangeEvent, MessageEndEvent, MessageEndEventResult,
        MessageRenderOptions, MessageRenderer, MessageStartEvent, MessageUpdateEvent,
        ModelSelectEvent, ModelSelectSource, NormalizedBuildSystemPromptOptions,
        PowerShellToolCallEvent, PowerShellToolResultEvent, ProjectTrustContext, ProjectTrustEvent,
        ProjectTrustEventDecision, ProjectTrustEventResult, ProjectTrustHandler,
        ProviderChatModelConfig, ProviderClassifierModelConfig, ProviderConfig,
        ProviderImageModelConfig, ProviderModelConfig, ProviderStreamEvent, ReadToolCallEvent,
        ReadToolResultEvent, RefreshToolsHandler, RegisteredCommand, RegisteredTool,
        ReplacedSessionContextApi, ResolvedCommand, ResourcesDiscoverEvent,
        ResourcesDiscoverResult, SendMessageHandler, SendUserMessageHandler,
        SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
        SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
        SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionBoundaryDraft, SessionCompactEvent,
        SessionCompactFailedEvent, SessionEvent, SessionInfoChangedEvent, SessionShutdownEvent,
        SessionStartEvent, SessionTreeEvent, SetActiveToolsHandler, SetLabelHandler,
        SetModelHandler, SetSessionNameHandler, SetThinkingLevelHandler, TerminalInputHandler,
        ThinkingLevelSelectEvent, ToolAnnotations, ToolCallEvent, ToolCallEventResult,
        ToolDefinition, ToolExecutionEndEvent, ToolExecutionMode, ToolExecutionStartEvent,
        ToolExecutionUpdateEvent, ToolExposure, ToolInfo, ToolLoadout, ToolLoadoutChanges,
        ToolNamespace, ToolRenderContext, ToolRenderResultOptions, ToolRendererResolver,
        ToolRenderers, ToolResultEvent, ToolResultEventResult, TreePreparation, TurnEndEvent,
        TurnEndEventResult, TurnStartEvent, UIPromptEndEvent, UIPromptKind, UIPromptStartEvent,
        UserBashEvent, UserBashEventResult, WidgetPlacement, WorkingIndicatorOptions,
        WriteToolCallEvent, WriteToolResultEvent, define_tool, is_bash_tool_result,
        is_edit_tool_result, is_find_tool_result, is_grep_tool_result, is_ls_tool_result,
        is_power_shell_tool_result, is_read_tool_result, is_tool_call_event_type,
        is_write_tool_result,
    };
}

/// 模块路径可达性：`kiana_ext_api::ui::*`。
mod ui_surface {
    #![allow(unused_imports)]
    use kiana_ext_api::ui::{
        AutocompleteProviderFactory, CustomDoneFn, CustomFactory, CustomOptions, EditorFactory,
        ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
        HeaderFactory, NotifyKind, OverlayOptionsOrFn, ReadonlyFooterDataProvider,
        TerminalInputHandler, TerminalInputResult, ThemeInfo, ThemeOrName, ThemeResult,
        WidgetContent, WidgetFactory, WidgetPlacement, WorkingIndicatorOptions,
    };
}

/// 模块路径可达性：`kiana_ext_api::tool_types::*`。
mod tool_types_surface {
    #![allow(unused_imports)]
    use kiana_ext_api::tool_types::{
        BashToolDetails, BashToolInput, BashToolName, EditToolDetails, EditToolInput, EditToolName,
        FindToolDetails, FindToolInput, FindToolName, GrepToolDetails, GrepToolInput, GrepToolName,
        LsToolDetails, LsToolInput, LsToolName, NextToolRenderersFn, PowerShellToolDetails,
        PowerShellToolInput, PowerShellToolName, PrepareArgumentsFn, PrepareLoadoutFn,
        ReachesToolsFn, ReachesToolsResult, ReadToolDetails, ReadToolInput, ReadToolName,
        RenderCallFn, RenderResultFn, RenderShell, RenderState, ReplaceEdit, ToolAnnotations,
        ToolDefinition, ToolDefinitionExecute, ToolExposure, ToolLoadout, ToolLoadoutChanges,
        ToolNamespace, ToolRenderContext, ToolRenderResultOptions, ToolRendererResolver,
        ToolRenderers, TruncatedBy, TruncationResult, WriteToolInput, WriteToolName, define_tool,
    };
}

/// 模块路径可达性：`kiana_ext_api::tool_result::*`。
mod tool_result_surface {
    #![allow(unused_imports)]
    use kiana_ext_api::tool_result::{
        BashExecOptions, BashExecResult, BashOperations, BashResult, ExecOptions, ExecResult,
    };
}

/// 模块路径可达性：`kiana_ext_api::system_prompt::*`。
mod system_prompt_surface {
    #![allow(unused_imports)]
    use kiana_ext_api::system_prompt::{
        BuildSystemPromptOptions, ContextFile, NormalizedBuildSystemPromptOptions, Skill,
    };
}

/// 模块路径可达性：`kiana_ext_api::types::*`。
mod types_surface {
    #![allow(unused_imports)]
    use kiana_ext_api::types::{
        AppendEntryHandler, ArgumentCompletionsFn, CommandHandler, CompactOptions, ContextUsage,
        CustomMessagePayload, DeliverAs, EntryRenderOptions, EntryRenderer, ExecuteToolOptions,
        Extension, ExtensionActions, ExtensionApi, ExtensionCommandContextActions,
        ExtensionCommandContextApi, ExtensionContextActions, ExtensionContextApi, ExtensionError,
        ExtensionFactory, ExtensionFlag, ExtensionMode, ExtensionRuntime, ExtensionRuntimeState,
        ExtensionShortcut, ExtensionToolContextApi, ExtensionVirtualModel, FlagType, FlagValue,
        ForkOptions, GetActiveToolsHandler, GetAllToolsHandler, GetCommandsHandler,
        GetSessionNameHandler, GetSettingsHandler, GetThinkingLevelHandler, InlineExtension,
        LoadError, LoadExtensionsResult, LoadWarning, MarkdownTransformContext,
        MarkdownTransformer, MessageRenderOptions, MessageRenderer, NavigateTreeOptions,
        NewSessionOptions, NewSessionResult, OAuthLoginFn, OAuthRefreshTokenFn,
        PendingNativeProviderRegistration, PendingProviderRegistration,
        PendingVirtualModelRegistration, ProviderChatModelConfig, ProviderClassifierModelConfig,
        ProviderConfig, ProviderImageModelConfig, ProviderModelConfig, ProviderOAuthConfig,
        ProviderRegistration, RefreshModelsFn, RefreshToolsHandler, RegisterCommandOptions,
        RegisterFlagOptions, RegisterShortcutOptions, RegisteredCommand, RegisteredTool,
        ReplacedSessionContextApi, ResolvedCommand, SendMessageHandler, SendMessageOptions,
        SendUserMessageDeliverAs, SendUserMessageHandler, SendUserMessageOptions,
        SetActiveToolsHandler, SetLabelHandler, SetModelHandler, SetSessionNameHandler,
        SetThinkingLevelHandler, SetupFn, ShortcutHandler, SwitchSessionOptions, ToolInfo,
        VirtualModelRouteFn, WithSessionFn,
    };
}

/// 模块路径可达性：`kiana_ext_api::events::*`。
mod events_surface {
    #![allow(unused_imports)]
    use kiana_ext_api::events::{
        AfterProviderResponseEvent, AfterProviderResponseEventType, AgentActivityOutcome,
        AgentBeforeSettleEvent, AgentBeforeSettleEventResult, AgentBeforeSettleEventType,
        AgentEndEvent, AgentEndEventType, AgentSettledEvent, AgentSettledEventType,
        AgentStartEvent, AgentStartEventType, BashToolCallEvent, BashToolResultEvent,
        BeforeAgentStartCustomEvent, BeforeAgentStartCustomMessageType, BeforeAgentStartEvent,
        BeforeAgentStartEventResult, BeforeAgentStartEventType, BeforeAgentStartUserEvent,
        BeforeAgentStartUserMessageType, BeforeProviderHeadersEvent,
        BeforeProviderHeadersEventType, BeforeProviderRequestEvent,
        BeforeProviderRequestEventResult, BeforeProviderRequestEventType, BoundaryContextPreview,
        BoundaryResult, BoundaryState, CacheWarmingDecisionEventType, CompactionEntryDraft,
        CompactionEntryDraftType, CompactionReason, ContextEditEntryDraft,
        ContextEditEntryDraftType, ContextEvent, ContextEventResult, ContextEventType,
        ContextWithSystemEvent, ContextWithSystemEventType, CustomEntryDraft, CustomEntryDraftType,
        CustomMessageEntryDraft, CustomMessageEntryDraftType, CustomToolCallEvent,
        CustomToolResultEvent, EditToolCallEvent, EditToolResultEvent, ExtensionEvent,
        ExtensionEventResult, ExtensionHandler, ExtensionHandlerFn, FindToolCallEvent,
        FindToolResultEvent, ForkPosition, GrepToolCallEvent, GrepToolResultEvent, InputEvent,
        InputEventResult, InputEventType, InputSource, LsToolCallEvent, LsToolResultEvent,
        McpServersChangeEvent, McpServersChangeEventType, MessageEndEvent, MessageEndEventResult,
        MessageEndEventType, MessageStartEvent, MessageStartEventType, MessageUpdateEvent,
        MessageUpdateEventType, ModelSelectEvent, ModelSelectEventType, ModelSelectSource,
        OptionalPatch, ParseExtensionEventKindError, PowerShellToolCallEvent,
        PowerShellToolResultEvent, ProjectTrustContext, ProjectTrustEvent,
        ProjectTrustEventDecision, ProjectTrustEventResult, ProjectTrustEventType,
        ProjectTrustHandler, ProjectTrustUi, ProviderStreamEvent, ProviderStreamEventType,
        ReadToolCallEvent, ReadToolResultEvent, ResourcesDiscoverEvent, ResourcesDiscoverEventType,
        ResourcesDiscoverReason, ResourcesDiscoverResult, SessionBeforeCompactEvent,
        SessionBeforeCompactEventType, SessionBeforeCompactResult, SessionBeforeForkEvent,
        SessionBeforeForkEventType, SessionBeforeForkResult, SessionBeforeSwitchEvent,
        SessionBeforeSwitchEventType, SessionBeforeSwitchReason, SessionBeforeSwitchResult,
        SessionBeforeTreeEvent, SessionBeforeTreeEventType, SessionBeforeTreeResult,
        SessionBoundaryDraft, SessionCompactEvent, SessionCompactEventType,
        SessionCompactFailedEvent, SessionCompactFailedEventType, SessionEvent,
        SessionInfoChangedEvent, SessionInfoChangedEventType, SessionShutdownEvent,
        SessionShutdownEventType, SessionShutdownReason, SessionStartEvent, SessionStartEventType,
        SessionStartReason, SessionTreeEvent, SessionTreeEventType, StreamingBehavior,
        ThinkingLevelSelectEvent, ThinkingLevelSelectEventType, ToolCallEvent, ToolCallEventResult,
        ToolCallEventType, ToolExecutionEndEvent, ToolExecutionEndEventType,
        ToolExecutionStartEvent, ToolExecutionStartEventType, ToolExecutionUpdateEvent,
        ToolExecutionUpdateEventType, ToolResultEvent, ToolResultEventResult, ToolResultEventType,
        TreePreparation, TreeSummary, TurnEndEvent, TurnEndEventResult, TurnEndEventType,
        TurnStartEvent, TurnStartEventType, UIPromptEndEvent, UIPromptEndEventType, UIPromptKind,
        UIPromptReason, UIPromptStartEvent, UIPromptStartEventType, UserBashEvent,
        UserBashEventResult, UserBashEventType, WriteToolCallEvent, WriteToolResultEvent,
        is_bash_tool_result, is_edit_tool_result, is_find_tool_result, is_grep_tool_result,
        is_ls_tool_result, is_power_shell_tool_result, is_read_tool_result,
        is_tool_call_event_type, is_write_tool_result,
    };
}

/// 编译期断言：8 个行为接口都能以 `&dyn` 使用（对象安全）。
///
/// 不调用：运行时构造 UI/会话实现是 `tests/ui_contract.rs` 的职责；本函数只做类型检查。
#[allow(dead_code)] // 只做类型检查；运行时调用需要 UI 实现，见 tests/ui_contract.rs。
#[allow(clippy::type_complexity)] // 元组逐项列出 8 个 trait 对象，正是断言本身。
fn assert_object_safe(
    _traits: (
        &dyn ExtensionApi,
        &dyn ExtensionUiContext,
        &dyn ExtensionToolContextApi,
        &dyn ExtensionCommandContextApi,
        &dyn ReplacedSessionContextApi,
        &dyn ToolLoadout,
        &dyn ReadonlyFooterDataProvider,
        &dyn BashOperations,
    ),
) {
}

/// `ExtensionApi` 的最简实现：钉住 `on` + 31 个方法 + `events`（33 个成员）。
struct MockExtensionApi;

impl ExtensionApi for MockExtensionApi {
    fn on(&self, _kind: ExtensionEventKind, _handler: ExtensionHandler) -> Unsubscribe {
        Box::new(|| {})
    }
    fn register_tool(&self, _tool: ToolDefinition) {}
    fn register_command(&self, _name: &str, _options: RegisterCommandOptions) {}
    fn register_shortcut(&self, _shortcut: &str, _options: RegisterShortcutOptions) {}
    fn register_flag(&self, _name: &str, _options: RegisterFlagOptions) {}
    fn get_flag(&self, _name: &str) -> Option<FlagValue> {
        None
    }
    fn register_message_renderer(&self, _custom_type: &str, _renderer: MessageRenderer) {}
    fn register_markdown_transformer(&self, _transformer: MarkdownTransformer) {}
    fn register_entry_renderer(&self, _custom_type: &str, _renderer: EntryRenderer) {}
    fn register_tool_renderer(&self, _resolver: ToolRendererResolver) {}
    fn send_message(&self, _options: SendMessageOptions) {}
    fn send_user_message(&self, _content: UserContent, _options: Option<SendUserMessageOptions>) {}
    fn append_entry(&self, _custom_type: &str, _data: Option<JsonValue>) {}
    fn set_session_name(&self, _name: &str) {}
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn set_label(&self, _entry_id: &str, _label: Option<&str>) {}
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _args: &'a [String],
        _options: Option<ExecOptions>,
    ) -> LocalBoxFuture<'a, ExecResult> {
        Box::pin(async {
            ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 0,
                killed: false,
            }
        })
    }
    fn get_active_tools(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_all_tools(&self) -> Vec<ToolInfo> {
        Vec::new()
    }
    fn get_settings(&self) -> Settings {
        Settings::default()
    }
    fn set_active_tools(&self, _tool_names: Vec<String>) {}
    fn get_commands(&self) -> Vec<SlashCommandInfo> {
        Vec::new()
    }
    fn set_model(&self, _model: Model) -> LocalBoxFuture<'static, bool> {
        Box::pin(async { false })
    }
    fn get_thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::default()
    }
    fn set_thinking_level(&self, _level: ThinkingLevel) {}
    fn register_provider(&self, _registration: ProviderRegistration) {}
    fn unregister_provider(&self, _name: &str) {}
    fn register_mcp_server(&self, _name: &str, _config: McpServerConfig) {}
    fn unregister_mcp_server(&self, _name: &str) {}
    fn get_mcp_servers(&self) -> Vec<RegisteredMcpServer> {
        Vec::new()
    }
    fn register_virtual_model(&self, _model: ExtensionVirtualModel) {}
    fn unregister_virtual_model(&self, _provider: &str, _id: &str) {}
    fn events(&self) -> Arc<dyn EventBus> {
        kiana_core::event_bus::create_event_bus()
    }
}

#[test]
fn extension_api_object_safe_with_all_members() {
    let api: Arc<dyn ExtensionApi> = Arc::new(MockExtensionApi);
    assert!(api.get_active_tools().is_empty());
    assert!(api.events().as_ref() as *const dyn EventBus as *const () as usize != 0);
}

#[test]
fn traitlets_are_object_safe() {
    fn take_api(_: &dyn ExtensionApi) {}
    take_api(&MockExtensionApi);
}

#[test]
fn extension_handlers_store_standard_and_project_trust() {
    // TS: `Extension.handlers: Map<string, HandlerFn[]>`（`types.ts#Extension`）。标准 handler
    // 与 `project_trust` handler 存在**同一个 map** 里：`runner.ts` 的
    // `snapshotEventHandlers` 对两种事件都从 `ext.handlers.get(event)?.slice()` 取。
    let standard: ExtensionHandlerFn = Arc::new(|_event, _ctx| {
        Box::pin(async {
            Ok::<Option<ExtensionEventResult>, Box<dyn std::error::Error + Send + Sync>>(None)
        })
    });
    let trust: kiana_ext_api::ProjectTrustHandler = Arc::new(|_event, _ctx| {
        Box::pin(async {
            kiana_ext_api::ProjectTrustEventResult {
                trusted: kiana_ext_api::ProjectTrustEventDecision::Undecided,
                remember: None,
            }
        })
    });
    let mut handlers: IndexMap<String, Vec<ExtensionHandler>> = IndexMap::new();
    handlers.insert(
        "tool_call".to_string(),
        vec![ExtensionHandler::Standard(standard)],
    );
    handlers.insert(
        "project_trust".to_string(),
        vec![ExtensionHandler::ProjectTrust(trust)],
    );

    let extension = kiana_ext_api::Extension {
        path: "x.ts".to_string(),
        resolved_path: "/x.ts".to_string(),
        hidden: None,
        replaceable: None,
        source_info: SourceInfo {
            path: "/x.ts".to_string(),
            source: "local".to_string(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        handlers,
        tools: IndexMap::new(),
        message_renderers: IndexMap::new(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: IndexMap::new(),
        flags: IndexMap::new(),
        shortcuts: IndexMap::new(),
    };

    assert!(matches!(
        &extension.handlers["tool_call"][0],
        ExtensionHandler::Standard(_)
    ));
    assert!(matches!(
        &extension.handlers["project_trust"][0],
        ExtensionHandler::ProjectTrust(_)
    ));
    // `Extension` 仍然可以克隆（handler 是 `Arc`）。
    let _cloned = extension.clone();
}

/// 让 `LocalBoxFuture` 可在 mock 里使用。
type LocalBoxFuture<'a, T> = kiana_ext_api::LocalBoxFuture<'a, T>;
