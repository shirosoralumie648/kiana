//! `index.ts`（498 行）：SDK 公共导出面（只 re-export，不含逻辑）。
//!
//! 本文件逐组对应 TS 的 `export { ... } from "<module>"`，把每个符号映射到 Rust 侧
//! 的权威定义（`ARCHITECTURE.md` §3.5/§5 的落点）。规则：
//!
//! - **类型名保持**（`AgentSession`、`SessionManager`、`Theme` 等）；
//! - **函数名用 snake_case**，并保留 TS 里的 camelCase 别名（Rust 的 `use` 重命名不受
//!   `non_snake_case` 检查，与 `kiana-session::index` 的做法一致）；
//! - 只做 `pub use`，不新增实现；依赖 crate 缺失的符号在 `spec/GAPS.md` 与最终报告里列出。
//!
//! TS 源：`packages/coding-agent/src/index.ts`。
//!
//! # 为什么单独成文件
//!
//! TS 的 `index.ts` 是包的入口：任何 `import { … } from "@earendil-works/kiana-coding-agent"`
//! 都从这里取符号。Rust 侧 crate 根 `lib.rs` 自身只额外提升二进制入口需要的少量符号，
//! 完整的 SDK 面集中在本文件，再由 `lib.rs` 的 `pub use crate::sdk_exports::*;` 提升到
//! crate 根。因此 `kiana_coding_agent::X` 与 `kiana_coding_agent::sdk_exports::X` 是同一个
//! 条目。下游 `kiana-evals` 直接 `use kiana_coding_agent::sdk_exports::{…}`（见
//! `crates/kiana-evals/src/harness.rs`）。
//!
//! 本文件唯一的本地条目是 `ContextEditableContent` 类型别名；其余全部是 `pub use`。
//! 权威定义分散在各依赖 crate，这里不实现任何逻辑。
//!
//! # 新增导出时的规则
//!
//! 1. 先确认符号的权威定义在哪个依赖 crate（类型定义、函数实现都不在这里）；
//! 2. 用 `pub use` 导出 snake_case 本名，再按 TS 原名加一条 `as camelCase` 别名；两者指向
//!    同一个条目，不会产生包装层或第二份实现；
//! 3. 保持按 TS 源文件分组的花括号注释，方便与 `index.ts` 逐组对照；
//! 4. 确实没有对应 Rust 实现的符号记录到 `spec/GAPS.md`，不要在这里造占位实现。
//!
//! # 注意事项
//!
//! - `modes/interactive/components/index::*` 是 glob 重导出，会把该模块的全部公开项带进来；
//!   同一模块内两个显式 `pub use` 若引入同名条目会导致编译错误，不会被静默覆盖。
//! - camelCase 别名只是同一个条目的第二个名字（rustdoc 显示为重导出），不增加类型身份。
//!   `tests/sdk_surface.rs` 对其中一部分别名（如 `parseArgs`、`keyHint`、`renderDiff`）
//!   做存在性检查；`calculateContextTokens`、`convertToLlm`、`createCodingTools` 等多数
//!   别名目前没有测试覆盖。
//! - 本文件没有运行时行为；新增/删除导出会直接改变下游（`kiana-evals` 等）的编译结果。

// ============================================================================
// cli/args.ts
// ============================================================================

pub use crate::cli::args::{Args, parse_args, parse_args as parseArgs};

// ============================================================================
// config.ts
// ============================================================================

pub use kiana_core::config::{
    CONFIG_DIR_NAME, VERSION, get_agent_dir, get_docs_path, get_examples_path, get_package_dir,
    get_readme_path,
};
pub use kiana_core::config::{
    get_agent_dir as getAgentDir, get_docs_path as getDocsPath,
    get_examples_path as getExamplesPath, get_package_dir as getPackageDir,
    get_readme_path as getReadmePath,
};

// ============================================================================
// core/agent-session.ts
// ============================================================================

pub use kiana_session::agent_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, AgentSessionEventListener,
    ModelCycleResult, ParsedSkillBlock, PromptOptions, SessionStats, parse_skill_block,
};
// `parseSkillBlock` 的 camelCase 别名。
pub use kiana_session::agent_session::parse_skill_block as parseSkillBlock;

// ============================================================================
// core/auth-storage.ts
// ============================================================================

pub use kiana_config::auth::storage::read_stored_credential;
// camelCase 别名（TS index.ts 的函数名 `readStoredCredential`）。
pub use kiana_config::auth::storage::read_stored_credential as readStoredCredential;

// ============================================================================
// core/cache-warmer.ts
// ============================================================================

pub use kiana_config::cache::warmer::{CacheWarmingDecision, CacheWarmingStatus};

// ============================================================================
// core/compaction/index.ts
// ============================================================================

pub use kiana_compaction::{
    BranchPreparation, BranchSummaryResult, CollectEntriesResult, CompactionResult, CutPointResult,
    DEFAULT_COMPACTION_SETTINGS, FileOperations, GenerateBranchSummaryOptions,
    calculate_context_tokens, collect_entries_for_branch_summary, compact, estimate_tokens,
    find_cut_point, find_turn_start_index, generate_branch_summary, generate_summary,
    generate_summary_with_usage, get_last_assistant_usage, prepare_branch_entries,
    serialize_conversation, should_compact,
};
// camelCase 别名（TS index.ts 的函数名）。
pub use kiana_compaction::calculate_context_tokens as calculateContextTokens;
pub use kiana_compaction::collect_entries_for_branch_summary as collectEntriesForBranchSummary;
pub use kiana_compaction::estimate_tokens as estimateTokens;
pub use kiana_compaction::find_cut_point as findCutPoint;
pub use kiana_compaction::find_turn_start_index as findTurnStartIndex;
pub use kiana_compaction::generate_branch_summary as generateBranchSummary;
pub use kiana_compaction::generate_summary as generateSummary;
pub use kiana_compaction::generate_summary_with_usage as generateSummaryWithUsage;
pub use kiana_compaction::get_last_assistant_usage as getLastAssistantUsage;
pub use kiana_compaction::prepare_branch_entries as prepareBranchEntries;
pub use kiana_compaction::serialize_conversation as serializeConversation;
pub use kiana_compaction::should_compact as shouldCompact;

// ============================================================================
// core/event-bus.ts
// ============================================================================

pub use kiana_core::event_bus::{
    EventBus, EventBusController, create_event_bus, create_event_bus as createEventBus,
};

// ============================================================================
// core/extensions/index.ts
// ============================================================================

pub use kiana_extensions::{
    AfterProviderResponseEvent, AgentActivityOutcome, AgentBeforeSettleEvent,
    AgentBeforeSettleEventResult, AgentEndEvent, AgentSettledEvent, AgentStartEvent,
    AgentToolResult, AgentToolUpdateCallback, AppKeybinding, AutocompleteProviderFactory,
    BashToolCallEvent, BashToolResultEvent, BeforeAgentStartCustomEvent, BeforeAgentStartEvent,
    BeforeAgentStartEventResult, BeforeAgentStartUserEvent, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, BeforeProviderRequestEventResult, BoundaryContextPreview,
    BoundaryResult, BoundaryState, BuildSystemPromptOptions, CacheWarmingDecisionEvent,
    CacheWarmingDecisionEventResult, CompactOptions, CompactionEntryDraft, ContextEditEntryDraft,
    ContextEvent, ContextEventResult, ContextUsage, ContextWithSystemEvent, CustomEntryDraft,
    CustomMessageEntryDraft, CustomToolCallEvent, CustomToolResultEvent, EditToolCallEvent,
    EditToolResultEvent, EntryRenderOptions, EntryRenderer, ExecOptions, ExecResult,
    ExecuteToolOptions, Extension, ExtensionActions, ExtensionApi, ExtensionCommandContextActions,
    ExtensionCommandContextApi, ExtensionContextActions, ExtensionContextApi, ExtensionError,
    ExtensionEvent, ExtensionFactory, ExtensionFlag, ExtensionHandler, ExtensionMode,
    ExtensionRunner, ExtensionRuntime, ExtensionShortcut, ExtensionToolContextApi,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionVirtualModel, ExtensionWidgetOptions,
    FindToolCallEvent, FindToolResultEvent, GrepToolCallEvent, GrepToolResultEvent,
    InlineExtension, InputEvent, InputEventResult, InputSource, KeybindingsManager,
    LoadExtensionsResult, LsToolCallEvent, LsToolResultEvent, MarkdownTransformContext,
    MarkdownTransformer, McpServersChangeEvent, MessageEndEvent, MessageEndEventResult,
    MessageRenderOptions, MessageRenderer, MessageStartEvent, MessageUpdateEvent, ModelSelectEvent,
    ModelSelectSource, NormalizedBuildSystemPromptOptions, PowerShellToolCallEvent,
    PowerShellToolResultEvent, ProjectTrustContext, ProjectTrustEvent, ProjectTrustEventDecision,
    ProjectTrustEventResult, ProjectTrustHandler, ProviderChatModelConfig,
    ProviderClassifierModelConfig, ProviderConfig, ProviderImageModelConfig, ProviderModelConfig,
    ProviderStreamEvent, ReadToolCallEvent, ReadToolResultEvent, RegisteredCommand, RegisteredTool,
    ReplacedSessionContextApi, ResolvedCommand, ResourcesDiscoverEvent, ResourcesDiscoverResult,
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchResult,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionBoundaryDraft, SessionCompactEvent,
    SessionCompactFailedEvent, SessionEvent, SessionInfoChangedEvent, SessionShutdownEvent,
    SessionStartEvent, SessionTreeEvent, SlashCommandInfo, SlashCommandSource, SourceInfo,
    TerminalInputHandler, ThinkingLevelSelectEvent, ToolAnnotations, ToolCallEvent,
    ToolCallEventResult, ToolDefinition, ToolExecutionEndEvent, ToolExecutionMode,
    ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolExposure, ToolInfo, ToolLoadout,
    ToolLoadoutChanges, ToolNamespace, ToolRenderResultOptions, ToolRendererResolver,
    ToolRenderers, ToolResultEvent, ToolResultEventResult, TurnEndEvent, TurnEndEventResult,
    TurnStartEvent, UIPromptEndEvent, UIPromptKind, UIPromptStartEvent, UserBashEvent,
    UserBashEventResult, WidgetPlacement, WorkingIndicatorOptions, WriteToolCallEvent,
    WriteToolResultEvent, create_extension_runtime, define_tool, discover_and_load_extensions,
    is_bash_tool_result, is_edit_tool_result, is_find_tool_result, is_grep_tool_result,
    is_ls_tool_result, is_power_shell_tool_result, is_read_tool_result, is_tool_call_event_type,
    is_write_tool_result, wrap_registered_tool, wrap_registered_tools,
};

// TS 的接口名（`index.ts` 导出的类型名）在 Rust 侧是 `*Api` / `*UiContext` trait；
// 按“类型名保持”的映射规则提供原名别名（同 `kiana-extensions::index` 的命名映射）。
pub use kiana_ext_api::types::{
    ExtensionApi as ExtensionAPI, ExtensionCommandContextApi as ExtensionCommandContext,
    ExtensionContextApi as ExtensionContext, ExtensionToolContextApi as ExtensionToolContext,
};
pub use kiana_ext_api::ui::ExtensionUiContext as ExtensionUIContext;

// camelCase 别名（函数）。
pub use kiana_extensions::create_extension_runtime as createExtensionRuntime;
pub use kiana_extensions::define_tool as defineTool;
pub use kiana_extensions::discover_and_load_extensions as discoverAndLoadExtensions;
pub use kiana_extensions::is_bash_tool_result as isBashToolResult;
pub use kiana_extensions::is_edit_tool_result as isEditToolResult;
pub use kiana_extensions::is_find_tool_result as isFindToolResult;
pub use kiana_extensions::is_grep_tool_result as isGrepToolResult;
pub use kiana_extensions::is_ls_tool_result as isLsToolResult;
pub use kiana_extensions::is_power_shell_tool_result as isPowerShellToolResult;
pub use kiana_extensions::is_read_tool_result as isReadToolResult;
pub use kiana_extensions::is_tool_call_event_type as isToolCallEventType;
pub use kiana_extensions::is_write_tool_result as isWriteToolResult;
pub use kiana_extensions::wrap_registered_tool as wrapRegisteredTool;
pub use kiana_extensions::wrap_registered_tools as wrapRegisteredTools;

// ============================================================================
// core/footer-data-provider.ts / core/mcp-servers.ts
// ============================================================================

pub use kiana_core::mcp_servers::RegisteredMcpServer;
pub use kiana_ext_api::ui::ReadonlyFooterDataProvider;

// ============================================================================
// core/messages.ts
// ============================================================================

pub use kiana_session_core::messages::convert_to_llm;
pub use kiana_session_core::messages::convert_to_llm as convertToLlm;

// ============================================================================
// core/model-registry.ts
// ============================================================================

pub use kiana_config::models::registry::ModelRegistry;

// ============================================================================
// core/model-resolver.ts
// ============================================================================

pub use kiana_config::models::resolver::resolve_cli_model as resolveCliModel;
pub use kiana_config::models::resolver::resolve_model_scope_with_diagnostics as resolveModelScopeWithDiagnostics;
pub use kiana_config::models::resolver::{
    ModelScopeDiagnostic, ResolveCliModelResult, ResolveModelScopeResult, ScopedModel,
    resolve_cli_model, resolve_model_scope_with_diagnostics,
};

// ============================================================================
// core/model-runtime.ts
// ============================================================================

pub use kiana_config::models::runtime::{
    CreateModelRuntimeOptions, CredentialSynchronizationError, CredentialSynchronizationOperation,
    ModelRuntime, ModelRuntimeAuthOverrides, ModelsPathOption,
};

// ============================================================================
// core/package-manager.ts
// ============================================================================

pub use kiana_resources::package::{
    DefaultPackageManager, PackageInstallFailure, PackageManager, PathMetadata, ProgressCallback,
    ProgressEvent, ResolvedPaths, ResolvedResource,
};

// ============================================================================
// core/resource-loader.ts
// ============================================================================

pub use kiana_resources::load_project_context_files as loadProjectContextFiles;
pub use kiana_resources::{
    DefaultResourceLoader, ResourceCollision, ResourceDiagnostic, ResourceLoader,
    load_project_context_files,
};

// ============================================================================
// core/sdk.ts
// ============================================================================

pub use kiana_resources::prompts::PromptTemplate;
pub use kiana_session::agent_session_runtime::{
    AgentSessionRuntime, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeResult,
    create_agent_session_runtime,
};
pub use kiana_session::agent_session_services::{
    AgentSessionRuntimeDiagnostic, AgentSessionServices, CreateAgentSessionFromServicesOptions,
    CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};
pub use kiana_session::sdk::{
    CreateAgentSessionOptions, CreateAgentSessionResult, NoTools, create_agent_session,
};
pub use kiana_tools::index::{
    create_bash_tool, create_coding_tools, create_edit_tool, create_find_tool, create_grep_tool,
    create_ls_tool, create_power_shell_tool, create_read_only_tools, create_read_tool,
    create_write_tool,
};
// camelCase 别名（函数）。
pub use kiana_session::agent_session_runtime::create_agent_session_runtime as createAgentSessionRuntime;
pub use kiana_session::agent_session_services::create_agent_session_from_services as createAgentSessionFromServices;
pub use kiana_session::agent_session_services::create_agent_session_services as createAgentSessionServices;
pub use kiana_session::sdk::create_agent_session as createAgentSession;
pub use kiana_tools::index::create_bash_tool as createBashTool;
pub use kiana_tools::index::create_coding_tools as createCodingTools;
pub use kiana_tools::index::create_edit_tool as createEditTool;
pub use kiana_tools::index::create_find_tool as createFindTool;
pub use kiana_tools::index::create_grep_tool as createGrepTool;
pub use kiana_tools::index::create_ls_tool as createLsTool;
pub use kiana_tools::index::create_power_shell_tool as createPowerShellTool;
pub use kiana_tools::index::create_read_only_tools as createReadOnlyTools;
pub use kiana_tools::index::create_read_tool as createReadTool;
pub use kiana_tools::index::create_write_tool as createWriteTool;

// ============================================================================
// core/session-manager.ts
// ============================================================================

pub use kiana_session_core::session_manager::build_context_entries as buildContextEntries;
pub use kiana_session_core::session_manager::build_session_context as buildSessionContext;
pub use kiana_session_core::session_manager::build_session_projection as buildSessionProjection;
pub use kiana_session_core::session_manager::get_latest_compaction_entry as getLatestCompactionEntry;
pub use kiana_session_core::session_manager::migrate_session_entries as migrateSessionEntries;
pub use kiana_session_core::session_manager::parse_session_entries as parseSessionEntries;
pub use kiana_session_core::session_manager::session_entry_to_context_messages as sessionEntryToContextMessages;
pub use kiana_session_core::session_manager::{
    BranchSummaryEntry, CURRENT_SESSION_VERSION, CompactionEntry, ContextEditEntry, CustomEntry,
    CustomMessageEntry, FileEntry, ModelChangeEntry, NewSessionOptions, ProjectedSessionEntry,
    SessionContext, SessionEntry, SessionHeader, SessionInfo, SessionInfoEntry, SessionManager,
    SessionMessageEntry, SessionProjection, SessionTreeNode, ThinkingLevelChangeEntry,
    build_context_entries, build_session_context, build_session_projection,
    get_latest_compaction_entry, migrate_session_entries, parse_session_entries,
    session_entry_to_context_messages,
};
// TS `SessionEntryBase`：所有条目的公共字段（`type`/`id`/`parentId`/`timestamp`）。
// Rust 用独立结构体 + [`SessionEntry`] 联合建模（无接口继承）；联合本身提供
// `kind()`/`id()`/`parent_id()`/`timestamp()`，是 TS 基接口在 Rust 侧的等价物。
pub use kiana_session_core::session_manager::SessionEntry as SessionEntryBase;
/// TS `ContextEditableContent`：append-only context edit 可替换的内容（字符串或内容块数组）。
/// Rust 的 `ContextEditReplacement.content` 用原始 JSON 承载。
pub type ContextEditableContent = serde_json::Value;

// ============================================================================
// core/settings-manager.ts
// ============================================================================

pub use kiana_config::settings::{
    CacheWarmingMode, CompactionModelOverride, CompactionSettings, DefaultProjectTrust,
    FullscreenExitOutput, ImageSettings, PackageSource, QuietStartup, RetrySettings,
    SettingsManager, SettingsManagerCreateOptions, TuiMode,
};

// ============================================================================
// core/skills.ts
// ============================================================================

pub use kiana_resources::skills::load_skills as loadSkills;
pub use kiana_resources::skills::load_skills_from_dir as loadSkillsFromDir;
pub use kiana_resources::skills::prompt::format_skills_for_prompt;
pub use kiana_resources::skills::prompt::format_skills_for_prompt as formatSkillsForPrompt;
pub use kiana_resources::skills::{
    LoadSkillsFromDirOptions, LoadSkillsResult, Skill, SkillFrontmatter, load_skills,
    load_skills_from_dir,
};

// ============================================================================
// core/source-info.ts
// ============================================================================

pub use kiana_resources::source_info::create_synthetic_source_info;
pub use kiana_resources::source_info::create_synthetic_source_info as createSyntheticSourceInfo;

// ============================================================================
// core/tools/edit-diff.ts
// ============================================================================

pub use kiana_tools::edit_diff::generate_diff_string as generateDiffString;
pub use kiana_tools::edit_diff::generate_unified_patch as generateUnifiedPatch;
pub use kiana_tools::edit_diff::{EditDiffResult, generate_diff_string, generate_unified_patch};

// ============================================================================
// core/tools/index.ts
// ============================================================================

pub use kiana_ext_api::tool_result::BashOperations;
pub use kiana_ext_api::tool_types::{
    BashToolDetails, BashToolInput, EditToolDetails, EditToolInput, FindToolDetails, FindToolInput,
    GrepToolDetails, GrepToolInput, LsToolDetails, LsToolInput, PowerShellToolDetails,
    PowerShellToolInput, ReadToolDetails, ReadToolInput, TruncationResult, WriteToolInput,
};
pub use kiana_tools::{
    BashSpawnContext, BashSpawnHook, BashToolOptions, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
    EditOperations, EditToolOptions, FindOperations, FindToolOptions, GrepOperations,
    GrepToolOptions, LsOperations, LsToolOptions, PowerShellOperations, PowerShellSpawnContext,
    PowerShellSpawnHook, PowerShellToolOptions, ReadOperations, ReadToolOptions, ToolsOptions,
    TruncationOptions, WriteOperations, WriteToolOptions, create_bash_tool_definition,
    create_edit_tool_definition, create_find_tool_definition, create_grep_tool_definition,
    create_local_bash_operations, create_local_power_shell_operations, create_ls_tool_definition,
    create_power_shell_tool_definition, create_read_tool_definition, create_write_tool_definition,
    format_size, truncate_head, truncate_line, truncate_tail, with_file_mutation_queue,
};

// camelCase 别名（TS index.ts 的 `core/tools/index.ts` 函数名）。
pub use kiana_tools::{
    create_bash_tool_definition as createBashToolDefinition,
    create_edit_tool_definition as createEditToolDefinition,
    create_find_tool_definition as createFindToolDefinition,
    create_grep_tool_definition as createGrepToolDefinition,
    create_local_bash_operations as createLocalBashOperations,
    create_local_power_shell_operations as createLocalPowerShellOperations,
    create_ls_tool_definition as createLsToolDefinition,
    create_power_shell_tool_definition as createPowerShellToolDefinition,
    create_read_tool_definition as createReadToolDefinition,
    create_write_tool_definition as createWriteToolDefinition, format_size as formatSize,
    truncate_head as truncateHead, truncate_line as truncateLine, truncate_tail as truncateTail,
    with_file_mutation_queue as withFileMutationQueue,
};

// ============================================================================
// core/tools/render-utils.ts
// ============================================================================

pub use kiana_tools::render_utils::format_read_line_range;
pub use kiana_tools::render_utils::format_read_line_range as formatReadLineRange;

// ============================================================================
// core/trust-manager.ts
// ============================================================================

pub use kiana_config::trust::manager::has_trust_requiring_project_resources as hasTrustRequiringProjectResources;
pub use kiana_config::trust::manager::{
    ProjectTrustDecision, ProjectTrustStore, ProjectTrustStoreEntry, ProjectTrustUpdate,
    has_trust_requiring_project_resources,
};

// ============================================================================
// core/virtual-models.ts
// ============================================================================

pub use kiana_session_core::virtual_models::{
    ModelRoute, ModelRouteReason, ModelRouteRequest, VIRTUAL_MODEL_STATE_ENTRY,
    VirtualModelDefinition, VirtualModelStateData,
};

// ============================================================================
// extensions/codemode、extensions/mcp、extensions/tool-search
// ============================================================================

pub use kiana_builtin_extensions::codemode::tool::{CodemodeStoreEntryData, CodemodeToolDetails};
pub use kiana_builtin_extensions::create_codemode_extension as createCodemodeExtension;
pub use kiana_builtin_extensions::create_mcp_extension as createMcpExtension;
pub use kiana_builtin_extensions::create_tool_search_extension as createToolSearchExtension;
pub use kiana_builtin_extensions::mcp::config::{
    LoadedMcpConfig, McpExposure, McpServerConfig, McpServerEntry,
};
pub use kiana_builtin_extensions::{
    CodemodeExtensionOptions, McpExtensionOptions, McpTransportFactory, create_codemode_extension,
    create_mcp_extension, create_tool_search_extension,
};

// ============================================================================
// main.ts
// ============================================================================

pub use crate::app::{MainOptions, main};

// ============================================================================
// modes/index.ts
// ============================================================================

pub use kiana_interactive::{InteractiveMode, InteractiveModeOptions};
pub use kiana_rpc_print::run_print_mode as runPrintMode;
pub use kiana_rpc_print::run_rpc_mode as runRpcMode;
pub use kiana_rpc_print::{
    JsonAgentSessionEvent, ModelInfo, PrintModeOptions, RpcClient, RpcClientOptions, RpcCommand,
    RpcEventListener, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse, RpcSessionState,
    run_print_mode, run_rpc_mode,
};

// ============================================================================
// modes/interactive/components/index.ts
// ============================================================================

pub use kiana_interactive::components::index::*;
// camelCase 别名（TS index.ts 的 `modes/interactive/components/index.ts` 函数名）。
pub use kiana_interactive::components::index::{
    key_hint as keyHint, key_text as keyText, raw_key_hint as rawKeyHint,
    render_diff as renderDiff, truncate_to_visual_lines as truncateToVisualLines,
};

// ============================================================================
// modes/interactive/theme/theme.ts
// ============================================================================

pub use kiana_theme::get_language_from_path as getLanguageFromPath;
pub use kiana_theme::get_markdown_theme as getMarkdownTheme;
pub use kiana_theme::get_select_list_theme as getSelectListTheme;
pub use kiana_theme::get_settings_list_theme as getSettingsListTheme;
pub use kiana_theme::highlight_code as highlightCode;
pub use kiana_theme::init_theme as initTheme;
pub use kiana_theme::{
    Theme, ThemeAppearance, ThemeBg, ThemeColor, ThemeStyle, ThemeToken, get_language_from_path,
    get_markdown_theme, get_select_list_theme, get_settings_list_theme, highlight_code, init_theme,
};

// ============================================================================
// utils/*
// ============================================================================

pub use kiana_core::clipboard::copy_to_clipboard;
pub use kiana_core::clipboard::copy_to_clipboard as copyToClipboard;
pub use kiana_core::frontmatter::{parse_frontmatter, strip_frontmatter};
pub use kiana_core::frontmatter::{
    parse_frontmatter as parseFrontmatter, strip_frontmatter as stripFrontmatter,
};
pub use kiana_core::image_process::{
    ResizedImage, convert_to_png, format_dimension_note, resize_image,
};
pub use kiana_core::image_process::{
    convert_to_png as convertToPng, format_dimension_note as formatDimensionNote,
    resize_image as resizeImage,
};
pub use kiana_core::mime::detect_supported_image_mime_type_from_file;
pub use kiana_core::mime::detect_supported_image_mime_type_from_file as detectSupportedImageMimeTypeFromFile;
pub use kiana_core::shell::{get_power_shell_config, get_shell_config};
pub use kiana_core::shell::{
    get_power_shell_config as getPowerShellConfig, get_shell_config as getShellConfig,
};

// ============================================================================
// 补充再导出（kiana-evals harness 无 kiana-session/kiana-config/kiana-resources/kiana-agent-core
// 直接依赖；与 `index.ts` 的对应符号语义相同）
// ============================================================================

pub use kiana_agent_core::types::AgentMessage;
pub use kiana_ext_api::events::{ExtensionEventKind, ExtensionEventResult};
pub use kiana_resources::loader::DefaultResourceLoaderOptions;
