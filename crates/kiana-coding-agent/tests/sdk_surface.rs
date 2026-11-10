//! SDK 导出面的编译期冒烟测试：`crates/kiana-coding-agent/src/sdk_exports.rs`
//! （= `packages/coding-agent/src/index.ts`，498 行纯再导出）。
//!
//! 这里只验证符号存在、类型可用，不重复任何行为测试。

#![allow(unused_imports)]

use kiana_coding_agent::{
    // core/agent-session.ts
    AgentSession,
    AgentSessionConfig,
    AgentSessionEvent,
    AgentSessionEventListener,
    // core/sdk.ts
    AgentSessionRuntime,
    AgentSessionRuntimeDiagnostic,
    AgentSessionServices,
    // cli/args.ts
    Args,
    // modes/interactive/components/index.ts
    ArminComponent,
    AssistantMessageComponent,
    BorderedLoader,
    // config.ts
    CONFIG_DIR_NAME,
    // core/session-manager.ts
    CURRENT_SESSION_VERSION,
    // extensions
    CodemodeExtensionOptions,
    ContextEditableContent,
    CreateAgentSessionOptions,
    CreateAgentSessionResult,
    CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeResult,
    CreateAgentSessionServicesOptions,
    // core/model-runtime.ts
    CreateModelRuntimeOptions,
    CustomEditor,
    // core/compaction
    DEFAULT_COMPACTION_SETTINGS,
    // core/event-bus.ts
    EventBus,
    EventBusController,
    // index.ts 的接口原名（Rust 侧是 *Api trait 的别名）
    ExtensionAPI,
    // core/extensions
    ExtensionApi,
    ExtensionCommandContext,
    ExtensionContext,
    ExtensionFactory,
    ExtensionFlag,
    ExtensionRunner,
    ExtensionRuntime,
    ExtensionToolContext,
    ExtensionUIContext,
    FooterComponent,
    // modes
    InteractiveMode,
    InteractiveModeOptions,
    LoadExtensionsResult,
    // main.ts
    MainOptions,
    ModelCycleResult,
    ModelRuntime,
    // core/model-resolver.ts
    ModelScopeDiagnostic,
    ModelSelectorComponent,
    ParsedSkillBlock,
    PrintModeOptions,
    // core/trust-manager.ts
    ProjectTrustStore,
    PromptOptions,
    ResolveCliModelResult,
    ResolveModelScopeResult,
    RpcClient,
    RpcClientOptions,
    ScopedModel,
    SessionEntry,
    SessionEntryBase,
    SessionHeader,
    SessionInfo,
    SessionManager,
    SessionSelectorComponent,
    SessionStats,
    // core/settings-manager.ts
    SettingsManager,
    SettingsManagerCreateOptions,
    // theme
    Theme,
    ThemeColor,
    ThemeSelectorComponent,
    ToolDefinition,
    ToolExecutionComponent,
    TuiMode,
    VERSION,
    buildSessionContext,
    compact,
    // utils
    copyToClipboard,
    createAgentSession,
    createAgentSessionFromServices,
    createAgentSessionRuntime,
    createAgentSessionServices,
    createBashTool,
    createCodemodeExtension,
    createEventBus,
    createExtensionRuntime,
    createMcpExtension,
    createReadTool,
    createToolSearchExtension,
    createWriteTool,
    defineTool,
    estimateTokens,
    getAgentDir,
    getDocsPath,
    getExamplesPath,
    getMarkdownTheme,
    getPackageDir,
    getReadmePath,
    hasTrustRequiringProjectResources,
    highlightCode,
    initTheme,
    // modes/interactive/components/index.ts 的 camelCase 别名
    keyHint,
    keyText,
    main,
    parseArgs,
    parseFrontmatter,
    parseSkillBlock,
    rawKeyHint,
    readStoredCredential,
    renderDiff,
    resolveCliModel,
    resolveModelScopeWithDiagnostics,
    runPrintMode,
    runRpcMode,
    shouldCompact,
    stripFrontmatter,
    truncateToVisualLines,
};

#[test]
fn sdk_reexports_the_release_version_and_public_types() {
    assert_eq!(VERSION, "1.4.0");
    assert_eq!(CONFIG_DIR_NAME, ".kiana");
    assert_eq!(CURRENT_SESSION_VERSION, 3.0);

    // 类型可用性（编译期即可，运行期不做真实 I/O）。
    let _: Option<Args> = None;
    let _: Option<MainOptions> = None;
    let _: Option<PrintModeOptions> = None;
    let _: Option<InteractiveModeOptions> = None;
    let _: Option<SettingsManagerCreateOptions> = None;
    let _: Option<CreateAgentSessionOptions> = None;
    let _: Option<ContextEditableContent> = None;
    let _: Option<SessionEntryBase> = None;
    // 接口原名以 trait 别名形式可用（`index.ts` 的 5 个类型名）。
    let _: Option<&dyn ExtensionAPI> = None;
    let _: Option<&dyn ExtensionContext> = None;
    let _: Option<&dyn ExtensionCommandContext> = None;
    let _: Option<&dyn ExtensionToolContext> = None;
    let _: Option<&dyn ExtensionUIContext> = None;
}

/// 函数再导出必须以函数项的形式可用（编译期检查）。
#[allow(dead_code)]
fn function_reexports_are_callable() {
    let _ = main;
    let _ = parseArgs;
    let _ = resolveCliModel;
    let _ = resolveModelScopeWithDiagnostics;
    let _ = createAgentSession;
    let _ = createAgentSessionFromServices;
    let _ = createAgentSessionRuntime;
    let _ = createAgentSessionServices;
    let _ = createCodemodeExtension;
    let _ = createMcpExtension;
    let _ = createToolSearchExtension;
    let _ = createEventBus;
    let _ = createExtensionRuntime;
    let _ = defineTool;
    let _ = compact;
    let _ = estimateTokens;
    let _ = shouldCompact;
    let _ = buildSessionContext;
    let _ = hasTrustRequiringProjectResources;
    let _ = runPrintMode;
    let _ = runRpcMode;
    let _ = getMarkdownTheme;
    let _ = highlightCode;
    let _ = initTheme;
    let _ = copyToClipboard;
    let _ = parseFrontmatter;
    let _ = stripFrontmatter;
    let _ = getAgentDir;
    let _ = getDocsPath;
    let _ = getExamplesPath;
    let _ = getPackageDir;
    let _ = getReadmePath;
    let _ = parseSkillBlock;
    let _ = readStoredCredential;
    let _ = keyHint;
    let _ = keyText;
    let _ = rawKeyHint;
    let _ = renderDiff;
    let _ = truncateToVisualLines;
}
