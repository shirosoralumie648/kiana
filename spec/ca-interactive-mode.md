# `ca-interactive-mode` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-coding-agent` 中 **ca-interactive-mode** 单元（交互式 TUI 模式全部实现：交互主控、聊天视口、全部 UI 组件、主题系统、会话分享、bug 上报、程序状态上报）的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`ca-interactive-mode`。
参考实现（只读，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/modes/interactive`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。
> Rust 实现必须输出完全一致的字节序列（除另有说明）。中文用于解释语义。
> 凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B；`\x07` 表示 U+0007（BEL）；`\x1b\\` 表示 ESC + 反斜杠（ST）。
> 文中 `APP_NAME` / `APP_TITLE` / `CONFIG_DIR_NAME` / `VERSION` 是运行期常量（见 `ca-cli` 规范 §2.6）：
> `APP_NAME = kianaConfigName || "kiana"`、`APP_TITLE = kianaConfigName ? APP_NAME : "π"`（**只有未配置 `kianaConfig.name` 时才是 `π`**）、
> `CONFIG_DIR_NAME = pkg.kianaConfig?.configDir || ".kiana"`、`VERSION = pkg.version || "0.0.0"`（当前仓库 `package.json` 为 `1.4.0`）。
> 注意 `APP_NAME`/`APP_TITLE` **不是同一个值**：派生发行版（配置了 `kianaConfig.name`）下二者相同，默认构建下分别是 `kiana` 与 `π`。
> 规模（实测，逐文件 `wc -l`）：**62 个 `.ts` 文件 + 2 个 `.json` 主题** = 64 个源文件；其中 9 个顶层 `.ts`（8 006 行）、48 个 `components/*.ts`（11 856 行）、5 个 `theme/*.ts`（2 261 行）、2 个 `theme/*.json`（156 行），合计 **22 279 行**；另有 1 个二进制资源 `assets/clankolas.png`（640×537，539 053 字节 / 526.4 KiB，sha256 `169acd0dfe6fbb8d8742ed24a3fc654fd0b2e2d4223c733249c5493723f1b72d`）。导出语句 **222** 条（`grep -c '^export '`），去重后导出符号 **192** 个（旧版写"约 185 个"是估值，实测 192）。

本单元是**纯粹的“表现层 + 交互编排层”**：它不实现业务逻辑（会话、模型、工具、扩展由 `ca-session` / `ca-config-models` / `ca-tools-compaction-export` / `ca-resources` 提供），
但它是**所有用户可见字节的唯一产地**，因此逐字保真度要求最高。

> **与 `ca-interactive-components` 规格的关系**：工作区中另有 `spec/ca-interactive-components.md`（单元 id `ca-interactive-components`），
> 覆盖 `modes/interactive/components/`（48 个文件、11 856 行）作为独立单元。本规范按任务要求把 `modes/interactive/**` **整目录**
> 纳入范围，因此 §2.13–§2.49 与 §2.51–§2.59 与那份规格**有意重叠**。两者冲突时以本规范为准（本规范额外给出了跨文件调用顺序、
> 组件与 `interactive-mode.ts` 的交互契约、以及 §3–§8 的整体视图）。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
packages/coding-agent/src/modes/interactive/
  interactive-mode.ts                 (7105) 交互主控：InteractiveMode 类
  chat-viewport.ts                    (46)   全屏 transcript + 输入 dock 布局
  external-editor.ts                  (46)   外部编辑器（$VISUAL/$EDITOR）
  model-catalog-refresh.ts            (51)   并发模型目录刷新协调器
  model-search.ts                     (44)   模型搜索文本 / 去重名称
  program-status-reporter.ts          (111)  OSC 7501 程序状态上报
  session-share.ts                    (224)  /share：Radius artifact / GitHub gist
  tui-renderer.ts                     (81)   createInteractiveTui / createInteractiveTuiReference
  bug-report.ts                       (298)  /bug 上报流程
  theme/
    dark.json                         (78)   内置 dark 主题
    light.json                        (78)   内置 light 主题
    system-theme.ts                   (655)  由终端颜色生成 system 主题
    theme.ts                          (1086) Theme 类、加载、全局单例、TUI 主题工厂
    theme-controller.ts               (253)  InteractiveThemeController
    theme-schema.ts                   (146)  typebox 主题 JSON 校验
    theme-tokens.ts                   (121)  56 个颜色 token 元数据
  components/  (48 个文件)
    armin.ts(384) assistant-message.ts(216) auth-url.ts(39) bash-execution.ts(223)
    bordered-loader.ts(68) branch-summary-message.ts(71) compaction-summary-message.ts(72)
    config-selector.ts(951) countdown-timer.ts(39) custom-editor.ts(148) custom-entry.ts(69)
    custom-message.ts(114) diff.ts(147) dynamic-border.ts(25) earendil-announcement.ts(53)
    easter-egg-3d.lazy.ts(27) easter-egg-3d.ts(1292) extension-editor.ts(141)
    extension-input.ts(94) extension-selector.ts(117) first-time-setup.ts(148) footer.ts(302)
    index.ts(37) keybinding-hints.ts(48) login-dialog.ts(322) markdown-transform.ts(35)
    mermaid.ts(89) model-selector.ts(423) oauth-selector.ts(224) kiana-logo.ts(40)
    radius-login-selector.ts(114) scoped-models-selector.ts(403) session-selector-search.ts(260)
    session-selector.ts(1045) settings-selector.ts(1009) settings-submenu.ts(258)
    show-images-selector.ts(50) skill-invocation-message.ts(68) status-indicator.ts(123)
    themed-text.ts(32) theme-selector.ts(67) thinking-selector.ts(154) tool-execution.ts(417)
    tree-selector.ts(1428) trust-selector.ts(134) user-message-selector.ts(155)
    user-message.ts(83) visual-truncate.ts(98)
  assets/
    clankolas.png                     (640x537, 526.4K)  /dementedelves 使用的图片
```

相关文档（用于校对文案与语义）：`packages/coding-agent/docs/{themes,tui,terminal-setup,tmux,keybindings,slash-commands,settings}.md`。

### 0.2 阅读顺序建议

1. `theme/theme-tokens.ts` → `theme/theme.ts` → `theme/system-theme.ts` → `theme/theme-controller.ts`：一切渲染都经 `theme` 全局单例，先理解颜色体系。
2. `components/keybinding-hints.ts`（所有提示文案的格式）、`components/dynamic-border.ts`、`components/themed-text.ts`：最基础的三个可见单元。
3. `interactive-mode.ts` 的字段区（第 500–610 行）与 `init()` / `run()` / `handleEvent()` / `setupEditorSubmitHandler()` / `setupKeyHandlers()`：理解生命周期与事件流。
4. 其余组件按“对话框 → 选择器 → 消息组件”顺序。
5. `tui-renderer.ts` + `chat-viewport.ts`：TUI 组合根。
6. `bug-report.ts` / `session-share.ts` / `program-status-reporter.ts`：三个独立子系统。

### 0.3 不属于本单元但必须调用的东西

| 依赖单元 | 用途 |
|---|---|
| `tui-core` | `TUI`、`TuiMainScreen`、`TuiAltScreen`、`ProcessTerminal`、`Component`、`Container`、`VStack`、`Box`、`Spacer`、`Text`、`TruncatedText`、`Loader`、`CancellableLoader`、`ScrollView`、`MouseRegion`、`Input`、`Editor`、`SelectList`、`SettingsList`、`Markdown`、`Image`、`Key`、`matchesKey`、`getKeybindings`/`setKeybindings`、`getCapabilities`/`setCapabilityOverrides`、`visibleWidth`/`truncateToWidth`/`sliceByColumn`、`wrapTextWithAnsi`、`hyperlink`、`fuzzyFilter`/`fuzzyMatch`、`isViewportTUI`、`TuiLayouts`、`styleTextWithAnsi`、`colorToRgb`/`colorToOklch`/`parseColor`/`rgbColor`/`okhslColor`/`colorToHex`/`foregroundAnsi`/`backgroundAnsi`/`indexedColor`/`mixColors`、`isAppleTerminalSession`、`Marked`/`Token`、`getTerminalColorMode`、`TerminalColors`、`ProgramStatus` |
| `tui-components` | `MarkdownTheme`、`SelectListTheme`、`SettingsListTheme`、`EditorTheme`、`EditorOptions`、`SelectListLayoutOptions`、`SettingItem`、`AutocompleteProvider`、`CombinedAutocompleteProvider`、`SlashCommand`、`AutocompleteItem` |
| `tui-text` | `Markdown` 渲染器、`TruncatedText`、`strikethrough` 等文本能力 |
| `ca-session` | `AgentSession`、`AgentSessionEvent`、`AgentSessionRuntime`、`SessionManager`、`SessionEntry`、`parseSkillBlock`、`createCustomMessage`、`createCompactionSummaryMessage`、`sessionEntryToContextMessages`、`MissingSessionCwdError`、`SessionImportFileNotFoundError`、`formatMissingSessionCwdPrompt`、`ProjectTrustStore`、`getProjectTrustOptions`、`hasTrustRequiringProjectResources`、`FooterDataProvider`、`cache-stats`、`cache-warmer`、`crash-log`、`usage-totals`、`http-dispatcher`、`keybindings`（`KeybindingsManager`、`AppKeybinding`）、`slash-commands`（`BUILTIN_SLASH_COMMANDS`）、`defaults`（`DEFAULT_THINKING_LEVEL`、`THINKING_LEVEL_OPTIONS`）、`model-resolver`、`model-runtime`（`ModelRuntime`、`CredentialSynchronizationError`）、`package-manager`、`radius`、`resource-loader`、`source-info`、`telemetry`、`trust-manager`、`experimental`、`session-cwd`、`session-export`、`bug-report`、`bug-report-upload` |
| `ca-config-models` | `settings-manager`（全部 `get*/set*`）、`settings-defaults`（`SETTINGS_DEFAULTS`） |
| `ca-resources` | `resourceLoader.get*()`、`withBuiltInRenderers`、`extensionRunner`、`ExtensionRunner`、`ExtensionUIContext`、`ExtensionContext`、`EntryRenderer`、`MessageRenderer`、`MarkdownTransformer`、`ToolRenderers`、`EditorFactory`、`WorkingIndicatorOptions` |
| `ca-tools-compaction-export` | `truncate`（`truncateTail`、`DEFAULT_MAX_LINES`、`DEFAULT_MAX_BYTES`、`TruncationResult`）、`render-utils`（`formatToolCallWithArgs`、`getTextOutput`） |
| `ca-cli` | `config.ts`（`APP_NAME`、`APP_TITLE`、`CONFIG_DIR_NAME`、`VERSION`、`getAgentDir`、`getAuthPath`、`getChangelogPath`、`getDebugLogPath`、`getDocsPath`、`getThemesDir`、`getCustomThemesDir`、`getBundledInteractiveAssetPath`、`detectInstallChange`）、`cli/auth-command.ts`（`getAuthCredential`）、`utils/*`（`clipboard`、`clipboard-image`、`open-browser`、`ansi`、`text`、`paths`、`git`、`syntax-highlight`、`changelog`、`tools-manager`、`version-check`、`image-convert`、`abort`、`fs-watch`、`shell`、`kiana-user-agent`） |
| `ai-core` / `ai-api-*` / `ai-auth` | `AssistantMessage`、`ImageContent`、`Message`、`Model`、`Usage`、`modelsAreEqual`、`isRetryableAssistantError`、`getSupportedThinkingLevels`、`AuthEvent`、`AuthPrompt`、`AuthInfoLink`、`OAuthDeviceCodeInfo`、`ModelRuntime.refresh` 的 `ModelsRefreshResult`、`DEFAULT_RADIUS_GATEWAY` |
| `agent-codemode` | `ThinkingLevel`、`AgentMessage`、`crypto` 无关 |
| `telemetry-mcp` | `McpHttpServerConfig`、`addMcpServerConfig`、`loadMcpConfig`、`RADIUS_MCP_URL` |

---


## 1. 概览

### 1.1 这个单元做什么

`InteractiveMode` 是 `kiana` 默认的交互式前端。它的职责可以精确划分为 6 类：

1. **TUI 组合与生命周期**：创建 `TuiMainScreen`（regular）或 `TuiAltScreen`（fullscreen），挂载组件树（`documentContainer` → header / loadedResources / chat）、输入区（`editorContainer`）、待发消息区、状态区、widget 区、footer 区；支持运行期在两种模式间切换（`switchTuiMode`）。
2. **事件渲染**：订阅 `AgentSession` 的事件流（`agent_start` … `summarization_retry_finished`），把模型输出、工具调用/结果、压缩、重试、队列状态渲染成组件树；每次变更调用 `ui.requestRender()`。
3. **输入与命令分发**：把编辑器提交的文本按 24 个内置斜杠命令 + bash（`!` / `!!`）+ 普通消息分流；实现全部应用级键绑定动作。
4. **对话框/选择器**：以“替换编辑器槽位”的方式显示选择器（`showSelector`）或扩展对话框栈（`pushExtensionDialog`），包括模型、思考等级、主题、会话、树、信任、登录、设置、作用域模型、用户消息、图片开关等。
5. **主题系统**：`Theme` 全局单例、内置 dark/light、由终端颜色生成的 `system` 主题、亮暗自动配对（`light/dark`）、终端颜色查询与热重载。
6. **附属流程**：`/bug`（点击式上报 + zip 导出）、`/share`（Radius artifact / gist）、程序状态上报（OSC 7501）、外部编辑器、剪贴板粘贴、全屏搜索/复制。

### 1.2 与其他包的关系

```
        ┌──────────────────────────────────────────┐
        │ cli/main.ts (ca-cli)                     │
        │  └─ new InteractiveMode(runtimeHost, …)  │
        └───────────────┬──────────────────────────┘
                        │
        ┌───────────────▼──────────────────────────┐
        │ InteractiveMode (本单元)                 │
        │  ├─ AgentSession / AgentSessionRuntime   │  ← ca-session
        │  ├─ ModelRuntime / ResourceLoader        │  ← ca-config-models / ca-resources
        │  ├─ SettingsManager                      │  ← ca-config-models
        │  ├─ theme (全局单例)                     │
        │  └─ components/**                        │
        └───────────────┬──────────────────────────┘
                        │ 组件 → 字符串行
        ┌───────────────▼──────────────────────────┐
        │ @earendil-works/kiana-tui (tui-*)           │
        └──────────────────────────────────────────┘
```

- **上游**：`main.ts`（`ca-cli`）在交互模式下构造 `AgentSessionRuntime` 后 `new InteractiveMode(runtimeHost, options)`，随后 `await mode.run()`。
- **下游**：本单元只消费 `kiana-tui` 的渲染原语（`Component.render(width): string[]`）与 `kiana-ai` 的类型；不直接写进程 stdout（除少数明确位置：外部编辑器启动提示、`process.stdout.write` 的 resume 提示、`/session` 之外的 console.log 见 §3.7）。
- **与 RPC 模式的关系**：两者共享 `AgentSession`，但 UI 完全不同。RPC 模式不属于本单元。

### 1.3 入口点

| 入口 | 文件 | 说明 |
|---|---|---|
| `new InteractiveMode(runtimeHost, options?)` | `interactive-mode.ts` | 构造函数即创建 TUI、主题控制器、编辑器、footer；不启动渲染。 |
| `await mode.init()` | `interactive-mode.ts:945` | 幂等（`isInitialized`）。挂载组件、`ui.start()`、构建 header、安装 fd/rg、绑定扩展、渲染初始消息。 |
| `await mode.run()` | `interactive-mode.ts:1148` | `await init()`，随后启动后台检查（版本/包更新/tmux/模型目录刷新），打印启动警告，处理初始终端消息，进入 `while(true) getUserInput() → session.prompt()` 主循环。 |
| `mode.stop(fullscreenExitOutput?)` | `interactive-mode.ts:7086` | 释放选择器、进度、状态、终端输入监听、footer、订阅，停止 TUI，卸载信号处理器。 |
| `createInteractiveTui(options)` | `tui-renderer.ts` | 供其他 presentation 复用的组合根（导出到 `index.ts` 之外）。 |
| `createInteractiveTuiReference(getTui)` | `tui-renderer.ts` | 稳定 `TUI` 代理，转发到当前 renderer。 |
| `reportBug(context, hint?)` | `bug-report.ts` | `/bug` 全流程。 |
| `shareSession(context)` | `session-share.ts` | `/share` 全流程。 |
| `refreshModelCatalogs(modelRuntime, signal)` | `model-catalog-refresh.ts` | 并发共享刷新。 |

### 1.4 生命周期（逐条）

```
constructor
  ├─ setCapabilityOverrides(settings.getTerminalCapabilityOverrides())
  ├─ tuiMode = options.tuiMode ?? settings.getTuiMode()
  ├─ runtimeHost.setBeforeSessionInvalidate(() => resetExtensionUI())
  ├─ runtimeHost.setRebindSession(async () => { rebindCurrentSession({renderBeforeBind:true}); themeController.applyFromSettings(); })
  ├─ renderer = createInteractiveTui({...})
  ├─ ui = createInteractiveTuiReference(() => this.renderer)
  ├─ ui.setClearOnShrink(settings.getClearOnShrink())
  ├─ 容器：headerContainer, loadedResourcesContainer, chatContainer, documentContainer(= above 3 顺序 addChild)
  ├─ pendingMessagesContainer, statusContainer, widgetContainerAbove, widgetContainerBelow
  ├─ keybindings = KeybindingsManager.create(); setKeybindings(keybindings)
  ├─ defaultEditor = new CustomEditor(ui, getEditorTheme(), keybindings, {paddingX, autocompleteMaxVisible, embedWorkingStatus:true})
  ├─ editor = defaultEditor; editorContainer.addChild(editor)
  ├─ footerDataProvider = new FooterDataProvider(sessionManager.getCwd())
  ├─ footer = new FooterComponent(session, footerDataProvider); footer.setAutoCompactEnabled(session.autoCompactionEnabled)
  ├─ footerContainer.addChild(footer)
  ├─ hideThinkingBlock = settings.getHideThinkingBlock(); outputPad = settings.getOutputPad()
  ├─ setRegisteredThemes(session.resourceLoader.getThemes().themes)
  └─ themeController = new InteractiveThemeController(ui, {...})
init()
  ├─ registerSignalHandlers()
  ├─ changelogMarkdown = getChangelogForDisplay()
  ├─ if (scopedModels.length>0 && shouldShowStartupDetails()) console.log(theme.fg("dim", `Model scope: ${modelList}${cycleHint}`))
  ├─ renderWidgets(); viewport = createChatViewport({...}); transcriptScrollView/fullscreenLayoutRoot
  ├─ mountInteractiveTui(renderer, [document, pendingMessages, status, widgetsAbove, editor, widgetsBelow, footer])
  ├─ defaultEditor.onAction("app.clear", handleCtrlC); onCtrlD; onSubmit = handleStartupSubmit; ui.setFocus(editor)
  ├─ ui.start(); isInitialized = true
  ├─ programStatus.report(); ensurePngTranscoder()
  ├─ themeController.applyFromSettings(); await themeController.waitForTerminalColors()
  ├─ if (shouldShowStartupHeader()) 构建 BuiltInHeader ... else builtInHeader = new Text("",0,0)
  ├─ ui.requestRender()
  ├─ [fdPath] = await Promise.all([ensureTool("fd",...), ensureTool("rg",...)])
  ├─ setupKeyHandlers(); setupEditorSubmitHandler(); ui.requestRender()
  ├─ await rebindCurrentSession()
  ├─ renderInitialMessages()
  ├─ onThemeChange(() => { ui.invalidate(); updateEditorBorderColor(); ui.requestRender(); })
  ├─ footerDataProvider.onBranchChange(() => ui.requestRender())
  ├─ await updateAvailableProviderCount()
  ├─ ui.renderNow()
  └─ void loadAllHighlightLanguages().then(...)
run()
  ├─ await init()
  ├─ if (!KIANA_OFFLINE) 15s AbortController → refreshModelCatalogs(...).then(updateAvailableProviderCount).catch(noop).finally(clearTimeout)
  ├─ checkForNewKianaVersion(version).then(r => r && showNewVersionNotification(r))
  ├─ checkForPackageUpdates().then(u => u.length && showPackageUpdateNotification(u)).finally(win32 → updateTerminalTitle)
  ├─ checkTmuxKeyboardSetup().then(w => w && showWarning(w))
  ├─ for startupDiagnostics: error→showError / warning→showWarning / 其它→showStatus
  ├─ migratedProviders?.length → showWarning(`Migrated credentials to auth.json: ${list}`)
  ├─ modelRuntime.getError() → showError(`models.json error: ${err}`)
  ├─ modelFallbackMessage → showWarning(...)
  ├─ crash = takeUnnotifiedCrash() → showWarning(`${APP_NAME} crashed on ${when} (${crash.message}). Run /bug to report it; the crash details are attached automatically.`)
  ├─ void maybeWarnAboutAnthropicSubscriptionAuth()
  ├─ initialMessage → session.prompt(initialMessage, {images: initialImages})  (catch → showError)
  ├─ initialMessages → 逐个 session.prompt
  └─ while(true) { const input = await getUserInput(); try await session.prompt(input) catch showError }
shutdown(options?)
stop(fullscreenExitOutput?)
```

**关键不变量**

- `init()` 与 `run()` 的每一步顺序不可调整：header 必须在终端颜色到达后构建（因为颜色被烘进字符串）；`ensureTool` 必须在 `setupKeyHandlers` 之前完成（否则下载期间的按键会进入未初始化路径）。
- `handleEvent()` 的第一行是 `if (!this.isInitialized) await this.init();`，因此事件先于 `init()` 到达时会自动初始化。
- 所有对 UI 的改动之后都要 `ui.requestRender()`；组件自身只在 `invalidate()` 时重建缓存。

---


## 2. 逐文件规范

本节按文件给出**每一个导出（以及重要的内部）符号**的签名与精确行为。函数签名用 TS 原样书写（去掉 `export` 关键字时用 `internal` 标注）。

### 2.1 `theme/theme-tokens.ts`

```ts
export interface ThemeTokenDescriptor {
  slot: "foreground" | "background";
  description: string;
  fallback?: string;
}
export type ThemeTokenDescriptors = Readonly<Record<string, ThemeTokenDescriptor>>;
export const THEME_TOKENS: /* 见下表，顺序即声明顺序 */
export type ThemeToken = keyof typeof THEME_TOKENS;
export type ThemeColor = { /* slot === "foreground" 的 token */ };
export type ThemeBg = Exclude<ThemeToken, ThemeColor>;
export type ThemeColorValues<Value> = { /* required 全必填 */ } & { /* optional 全可选 */ };
export type ResolvedThemeColorValues<Value> = Record<ThemeToken, Value>;
```

**内部** `defineThemeTokens(tokens)`：模块加载时校验；若某个 token 的 `fallback` 指向不存在的 token → 抛
`Theme token ${name} has unknown fallback ${descriptor.fallback}`；若 fallback 的 slot 不同 → 抛
`Theme token ${name} has fallback ${descriptor.fallback} in a different color slot`。

**THEME_TOKENS 全表（56 项，顺序敏感——决定主题文件、JSON Schema 与 `Object.entries` 顺序）**

| # | token | slot | fallback | description（逐字） |
|---|---|---|---|---|
| 1 | accent | foreground | — | Primary accent color (logo, selected items, cursor) |
| 2 | border | foreground | — | Normal borders |
| 3 | borderAccent | foreground | — | Highlighted borders |
| 4 | borderMuted | foreground | — | Subtle borders |
| 5 | success | foreground | — | Success states |
| 6 | error | foreground | — | Error states |
| 7 | warning | foreground | — | Warning states |
| 8 | muted | foreground | — | Secondary/dimmed text |
| 9 | dim | foreground | — | Very dimmed text (more subtle than muted) |
| 10 | text | foreground | — | Default text color (usually empty string) |
| 11 | thinkingText | foreground | — | Thinking block text color |
| 12 | scrollbarTrack | foreground | muted | Fullscreen scrollbar track foreground (falls back to muted when omitted) |
| 13 | scrollbarThumb | foreground | text | Fullscreen scrollbar thumb foreground (falls back to text when omitted) |
| 14 | selectedBg | background | — | Selected item background |
| 15 | searchMatchBg | background | selectedBg | Transcript search match background and current-match text (falls back to selectedBg when omitted) |
| 16 | searchMatchText | foreground | text | Transcript search match text and current-match background (falls back to text when omitted) |
| 17 | userMessageBg | background | — | User message background |
| 18 | userMessageText | foreground | — | User message text color |
| 19 | customMessageBg | background | — | Custom message background (hook-injected messages) |
| 20 | customMessageText | foreground | — | Custom message text color |
| 21 | customMessageLabel | foreground | — | Custom message type label color |
| 22 | toolPendingBg | background | — | Tool execution box (pending state) |
| 23 | toolSuccessBg | background | — | Tool execution box (success state) |
| 24 | toolErrorBg | background | — | Tool execution box (error state) |
| 25 | toolTitle | foreground | — | Tool execution box title color |
| 26 | toolOutput | foreground | — | Tool execution box output text color |
| 27 | mdHeading | foreground | — | Markdown heading text |
| 28 | mdLink | foreground | — | Markdown link text |
| 29 | mdLinkUrl | foreground | — | Markdown link URL |
| 30 | mdCode | foreground | — | Markdown inline code |
| 31 | mdCodeBlock | foreground | — | Markdown code block content |
| 32 | mdCodeBlockBorder | foreground | — | Markdown code block fences |
| 33 | mdQuote | foreground | — | Markdown blockquote text |
| 34 | mdQuoteBorder | foreground | — | Markdown blockquote border |
| 35 | mdHr | foreground | — | Markdown horizontal rule |
| 36 | mdListBullet | foreground | — | Markdown list bullets/numbers |
| 37 | toolDiffAdded | foreground | — | Added lines in tool diffs |
| 38 | toolDiffRemoved | foreground | — | Removed lines in tool diffs |
| 39 | toolDiffContext | foreground | — | Context lines in tool diffs |
| 40 | syntaxComment | foreground | — | Syntax highlighting: comments |
| 41 | syntaxKeyword | foreground | — | Syntax highlighting: keywords |
| 42 | syntaxFunction | foreground | — | Syntax highlighting: function names |
| 43 | syntaxVariable | foreground | — | Syntax highlighting: variable names |
| 44 | syntaxString | foreground | — | Syntax highlighting: string literals |
| 45 | syntaxNumber | foreground | — | Syntax highlighting: number literals |
| 46 | syntaxType | foreground | — | Syntax highlighting: type names |
| 47 | syntaxOperator | foreground | — | Syntax highlighting: operators |
| 48 | syntaxPunctuation | foreground | — | Syntax highlighting: punctuation |
| 49 | thinkingOff | foreground | — | Thinking level border: off |
| 50 | thinkingMinimal | foreground | — | Thinking level border: minimal |
| 51 | thinkingLow | foreground | — | Thinking level border: low |
| 52 | thinkingMedium | foreground | — | Thinking level border: medium |
| 53 | thinkingHigh | foreground | — | Thinking level border: high |
| 54 | thinkingXhigh | foreground | — | Thinking level border: xhigh |
| 55 | thinkingMax | foreground | thinkingXhigh | Thinking level border: max (falls back to thinkingXhigh when omitted) |
| 56 | bashMode | foreground | — | Editor border color in bash mode |

**Rust 建议**：用 `enum ThemeToken`（56 个变体，`#[derive(Copy,Clone,PartialEq,Eq,Hash)]`）+ `const THEME_TOKENS: &[(ThemeToken, Slot, &str, Option<ThemeToken>)]` 保持声明顺序；在 `const fn` 或 `once_cell::Lazy` 中做 fallback 校验（或直接 `#[test]`）。

### 2.2 `theme/theme.ts`

#### 2.2.1 类型

```ts
export interface ThemeStyle extends TextAttributes { fg?: ThemeColor | Color; bg?: ThemeBg | Color; }
export type ThemeJsonValidator = (label: string, json: unknown) => ThemeJson;
export type ThemeAppearance = TerminalTheme;              // "dark" | "light"
export type TerminalTheme = "dark" | "light";
export interface ThemeInfo { name: string; path: string | undefined; }
export type { ValidatedThemeJson as ThemeJson } from "./theme-schema.ts";
export type { ThemeBg, ThemeColor, ThemeToken } from "./theme-tokens.ts";
export { SYSTEM_THEME_NAME } from "./system-theme.ts";     // = "system"
```

`ThemeStyle` 的注释（逐字）：`Tokens are only accepted in their own slot, because "" (terminal default) means the default foreground or background depending on the slot. Use theme.colors[token] to use a token's color in the other slot.`

#### 2.2.2 模块级可变状态

```ts
let themeJsonValidator: ThemeJsonValidator | undefined;
let terminalColors: TerminalColors = {};               // 整体替换，从不 mutate — 主题按身份缓存
let terminalColorsPending = false;                     // 终端查询在途 → system 主题先灰阶
let terminalColorScheme: TerminalTheme | undefined;    // mode 2031 报告，仅在终端未报告背景时用
let BUILTIN_THEMES: Record<string, ThemeJson> | undefined;
let currentThemeName: string | undefined;
let themeWatcher: fs.FSWatcher | undefined;
let themeReloadTimer: NodeJS.Timeout | undefined;
let onThemeChangeCallback: (() => void) | undefined;
const registeredThemes = new Map<string, Theme>();
let cachedHighlightThemeFor: Theme | undefined;
let cachedCliHighlightTheme: CliHighlightTheme | undefined;
const THEME_KEY = Symbol.for("@earendil-works/kiana-coding-agent:theme");
const THEME_KEY_OLD = Symbol.for("@mariozechner/kiana-coding-agent:theme");
```

#### 2.2.3 颜色工具（内部）

- `resolveVarRefs(value: ColorValue, vars, visited = new Set()): string | number`
  - 若 `value` 是 number、`""`、以 `#` 开头、或匹配 `/^ok(lch|hsl)\(/i` → 直接返回。
  - `visited.has(value)` → 抛 `Circular variable reference detected: ${value}`。
  - `!(value in vars)` → 抛 `Variable reference not found: ${value}`。
  - 否则递归（带 visited）。
- `resolveThemeColors(colors, vars = {})`：对每个 entry 调 `resolveVarRefs`，返回同 key 的新对象。
- `withThemeTokenFallbacks<Value>(values)`：按 `THEME_TOKENS` 声明顺序遍历；若 token 有 `fallback` 且未定义，取 fallback 的值；fallback 也未定义 → 抛 `Theme token ${rawName} has unresolved fallback ${descriptor.fallback}`。返回新对象（**不修改入参**）。
- `averageLightness(colors: Color[]): number | undefined`：过滤 `color.kind === "indexed" && color.index < 16`（0–15 跟随用户调色板，不能说明主题）；剩余为空 → `undefined`；否则求 `colorToOklch(color).l` 的算术平均。
- `detectAppearance(foregrounds, backgrounds): ThemeAppearance | undefined`
  - 两者都有 → `bg < fg ? "dark" : "light"`
  - 只有 bg → `bg < 0.5 ? "dark" : "light"`
  - 只有 fg → `fg > 0.5 ? "dark" : "light"`
  - 都没有 → `undefined`

#### 2.2.4 `GUESSED_DEFAULT_COLORS`

```ts
const GUESSED_DEFAULT_COLORS: Record<ThemeAppearance, {foreground: Color; background: Color}> = {
  dark:  { foreground: parseColor("#e5e5e7"), background: parseColor("#000000") },
  light: { foreground: parseColor("#000000"), background: parseColor("#ffffff") },
};
```

#### 2.2.5 `setThemeJsonValidator(validator)`

仅赋值模块变量。用于 `main.ts` 在加载用户资源前安装 typebox 校验器（避免仅用内置主题的 presentation 为此付出 ~17 MB 模块图）。

#### 2.2.6 `class Theme`

```ts
export class Theme {
  readonly name?: string;
  readonly sourcePath?: string;
  sourceInfo?: SourceInfo;
  private mode: TerminalColorMode;
  private readonly fgAnsi = new Map<ThemeColor, string>();
  private readonly bgAnsi = new Map<ThemeBg, string>();
  private readonly concreteColors: Partial<Record<ThemeToken, Color>> = {};
  private readonly defaultForegroundTokens: ThemeToken[] = [];
  private readonly defaultBackgroundTokens: ThemeToken[] = [];
  private readonly dimTokens: ReadonlySet<ThemeColor>;
  private readonly ownAppearance: ThemeAppearance | undefined;
  private resolvedColors: { terminal: TerminalColors; colors: Readonly<Record<ThemeToken, Color>> } | undefined;

  constructor(
    fgColors: Pick<ThemeColorValues<string|number>, ThemeColor>,
    bgColors: Pick<ThemeColorValues<string|number>, ThemeBg>,
    mode: TerminalColorMode,
    options?: { name?; sourcePath?; sourceInfo?; appearance?: ThemeAppearance; dim?: readonly ThemeColor[] },
  );
}
```

**构造逻辑（顺序）**

1. 保存 name/sourcePath/sourceInfo/mode；`dimTokens = new Set(options.dim)`。
2. `const { fgColors: foregrounds, bgColors: backgrounds } = splitThemeColors(withThemeTokenFallbacks({ ...fgColors, ...bgColors }))`
3. `addToken(token, value, isBackground)`：
   - `value === ""` → 推入 `defaultBackgroundTokens` 或 `defaultForegroundTokens`，返回 `"\x1b[49m"` 或 `"\x1b[39m"`。
   - 否则 `parseColor(value)` → 存入 `concreteColors[token]`，推入 `concreteBackgrounds`/`concreteForegrounds`；返回 `backgroundAnsi(color, mode)` 或 `foregroundAnsi(color, mode)`。
4. 遍历 `foregrounds` 的 entries 写 `fgAnsi`，再遍历 `backgrounds` 写 `bgAnsi`（**顺序：先全部前景，再全部背景**）。
5. `ownAppearance = options.appearance ?? detectAppearance(concreteForegrounds, concreteBackgrounds)`。

**getter `appearance`** → `this.ownAppearance ?? getTerminalTheme()`。

**getter `colors`**（按 `terminalColors` 身份缓存）：

```
if (this.resolvedColors?.terminal !== terminalColors) {
  const guess = GUESSED_DEFAULT_COLORS[this.appearance];
  const foreground = terminal.foreground ? rgbColor(...) : guess.foreground;
  const background = terminal.background ? rgbColor(...) : guess.background;
  const colors = { ...this.concreteColors };
  for (token of defaultForegroundTokens) colors[token] = foreground;
  for (token of defaultBackgroundTokens) colors[token] = background;
  for (token of dimTokens) { const c = colors[token]; if (c) colors[token] = mixColors(c, background, 0.4); }
  this.resolvedColors = { terminal, colors: Object.freeze(colors) };
}
return this.resolvedColors.colors;
```

注意：**dim token 的 `colors` 值是向背景混合 40%**，而 `fg()` 输出的是“原色 + SGR 2”。两者是不同表达，但视觉意图一致。测试断言：`theme.fg("muted","x") === "\x1b[39m\x1b[2mx\x1b[22;39m"`（当 muted 为空串且属于 dim 集合时）。

**`style(text, options: ThemeStyle): string`**
- 若 `typeof options.fg === "string" && dimTokens.has(fg)` → `options = { ...options, dim: true }`。
- 调 `styleTextWithAnsi(text, fgAnsi?, bgAnsi?, options)`；fg/bg 为字符串 token 时用 `this.tokenAnsi(map, token)`，为 `Color` 对象时用 `foregroundAnsi/backgroundAnsi(color, this.mode)`。

**`fg(color: ThemeColor, text: string): string`**
```
const ansi = this.tokenAnsi(this.fgAnsi, color);
if (this.dimTokens.has(color)) return `${ansi}\x1b[2m${text}\x1b[22;39m`;
return `${ansi}${text}\x1b[39m`;
```

**`bg(color: ThemeBg, text: string): string`** → `` `${ansi}${text}\x1b[49m` ``。

**`tokenAnsi(map, token)`** → 未定义时抛 `Unknown theme color: ${token}`（逐字；测试断言 `Unknown theme color: notAToken`、`Unknown theme color: userMessageBg`——后者是把 background token 当前景用）。

**`bold/italic/underline/inverse/strikethrough`** → 直接 `chalk.<x>(text)`（见 §6 的 chalk→Rust 对应）。

**`getFgAnsi(color)`** → 同 ansi，但 dim token 追加 `\x1b[2m`（**不**带关闭序列）。
**`getBgAnsi(color)`** → 同 bg ansi。
**`getColorMode()`** → `this.mode`。
**`getThinkingBorderColor(level: ThinkingLevel): (str: string) => string`** → 映射：off→thinkingOff、minimal→thinkingMinimal、low→thinkingLow、medium→thinkingMedium、high→thinkingHigh、xhigh→thinkingXhigh、max→thinkingMax，default→thinkingOff。
**`getBashModeBorderColor()`** → `(str) => this.fg("bashMode", str)`。

#### 2.2.7 主题加载

- `getBuiltinThemes(): Record<string, ThemeJson>`：懒加载 `getThemesDir()/dark.json` 与 `light.json`，`JSON.parse(stripBom(readFileSync(p,"utf-8")))`。仅两份，key 为 `"dark"`/`"light"`。
- `getAvailableThemes(): string[]` → `getAvailableThemesWithPaths().map(({name}) => name)`。
- `getAvailableThemesWithPaths(): ThemeInfo[]`：
  1. 加入 `{name: SYSTEM_THEME_NAME, path: undefined}`（system 主题是生成的，无文件）。
  2. 遍历 `Object.keys(getBuiltinThemes())`，`path = path.join(themesDir, name + ".json")`。
  3. 遍历 `getCustomThemeInfos()`。
  4. 遍历 `registeredThemes.entries()`，`{name, path: theme.sourcePath}`。
  - 用 `seen: Set` 去重（先到先得）。
  - 排序：`a.name === SYSTEM_THEME_NAME ? -1 : b.name === SYSTEM_THEME_NAME ? 1 : a.name.localeCompare(b.name)`（**system 永远第一**）。
- `getCustomThemeInfos(): ThemeInfo[]`：若 `getCustomThemesDir()` 不存在 → `[]`；遍历 `fs.readdirSync`，只取 `.json`；对每个 `loadThemeFromPath(p)`，成功且有 `name` → `{name: theme.name, path: p}`；任何异常**静默忽略**（资源加载器会在启动/重载时报告）。
- `assertThemeNameIsValid(name)`：`name.includes("/")` → 抛
  `Invalid theme name "${name}": theme names cannot contain "/" because it is reserved for automatic light/dark theme settings.` **【逐字】**
- `parseThemeJson(label, json)`：有 validator → 交给 validator；否则若 `typeof json !== "object" || json === null || !("colors" in json)` → 抛
  `Invalid theme "${label}": expected an object with a "colors" map.` **【逐字】**
- `parseThemeJsonContent(label, content)`：`JSON.parse(stripBom(content))` 失败 → 抛
  `Failed to parse theme ${label}: ${error}`（`error` 为 Error 的字符串化，即 `SyntaxError: ...`）。**【逐字】**
- `loadThemeJson(name: string): ThemeJson`：
  1. 内置命中 → 返回。
  2. `registeredThemes.get(name)` 且有 `sourcePath` → 读文件并 `parseThemeJsonContent(sourcePath, content)`。
  3. 已注册但无 sourcePath → 抛 `Theme "${name}" does not have a source path for export`。**【逐字】**
  4. `customThemesDir/${name}.json` 不存在 → 抛 `Theme not found: ${name}`。**【逐字】**
  5. 读取并 `parseThemeJsonContent(name, content)`。
- `splitThemeColors(colors)`：按 `THEME_TOKENS[key].slot` 分到 `bgColors`（"background"）或 `fgColors`。
- `createTheme(themeJson, mode?, sourcePath?)`：`colorMode = mode ?? getTerminalColorMode()`；`resolvedColors = withThemeTokenFallbacks(resolveThemeColors(themeJson.colors, themeJson.vars))`；`splitThemeColors`；`new Theme(fgColors, bgColors, colorMode, {name, sourcePath, appearance: themeJson.appearance})`。
- `createSystemTheme(mode?)`：`generateSystemThemeColors({...terminalColors, saturation: terminalColorsPending ? 0 : 1, appearanceHint: getTerminalTheme()})` → `withThemeTokenFallbacks` → `splitThemeColors` → `new Theme(..., {name: SYSTEM_THEME_NAME, appearance: generated.appearance, dim: generated.dim})`。
- `loadThemeFromPath(themePath, mode?)`：读文件 → `parseThemeJsonContent(themePath, content)` → `createTheme(themeJson, mode, themePath)`。
- `loadTheme(name, mode?)`：`name === SYSTEM_THEME_NAME` → `createSystemTheme(mode)`（**system 名称保留，优先于同名自定义主题**）；已注册 → 直接返回该实例；否则 `createTheme(loadThemeJson(name), mode)`。
- `getThemeByName(name)` → `try loadTheme(name) catch → undefined`。
- `parseAutoThemeSetting(themeSetting: string | undefined): {lightTheme, darkTheme} | undefined`：
  - 空 → undefined。
  - `slashIndex = indexOf("/")`；无 `/` 或还有第二个 `/` → undefined。
  - 两侧 `trim()`；任一为空 → undefined。
- `resolveThemeSetting(themeSetting, terminalTheme)`：若有 auto → 返回对应侧；若 `themeSetting?.includes("/")` → undefined；若 string → 原样；否则 undefined。
- `detectColorFgBgTheme(env = process.env)`：
  - `bg = env.COLORFGBG?.split(";").at(-1)?.trim()`；空或 `!/^\d{1,2}$/` → undefined；`index > 15` → undefined；`index <= 6 || index === 8 ? "dark" : "light"`。
- `detectTerminalTheme(colors = {}, reportedScheme?, env = process.env)`：`background` 存在 → `terminalAppearance(background, foreground)`；否则 `reportedScheme ?? detectColorFgBgTheme(env) ?? "dark"`。
- `getTerminalTheme()` → `detectTerminalTheme(terminalColors, terminalColorScheme)`。
- `setTerminalColors(colors)` → `terminalColors = {...colors}; terminalColorsPending = false;`
- `setTerminalColorScheme(scheme)` → 赋值。
- `markTerminalColorsPending()` → `terminalColorsPending = true`。

#### 2.2.8 全局单例 `theme`

```ts
export const theme: Theme = new Proxy({} as Theme, {
  get(_t, prop) {
    const t = globalThis[THEME_KEY];
    if (!t) throw new Error("Theme not initialized. Call initTheme() first.");
    return t[prop];
  },
});
function setGlobalTheme(t: Theme) { globalThis[THEME_KEY] = t; globalThis[THEME_KEY_OLD] = t; }
```
**【逐字】** 错误消息 `Theme not initialized. Call initTheme() first.`。使用 `globalThis` + `Symbol.for` 是为了 node/jiti 双模块加载器共享同一实例；Rust 用 `static THEME: RwLock<Arc<Theme>>`（或 `once_cell::Lazy<ArcSwap<Theme>>`）即可等价。

#### 2.2.9 全局主题设置/监听

- `setRegisteredThemes(themes: Theme[])`：清空 map；对每个有 `name` 的主题 `assertThemeNameIsValid(name)` 后 `registeredThemes.set(name, theme)`。
- `initTheme(themeName?, enableWatcher = false)`：
  - `name = themeName ?? SYSTEM_THEME_NAME`；`currentThemeName = name`。
  - 成功 → `setGlobalTheme(loadTheme(name))`，若 `enableWatcher` 则 `startThemeWatcher()`。
  - 失败 → **静默**回退：`currentThemeName = SYSTEM_THEME_NAME; setGlobalTheme(loadTheme(SYSTEM_THEME_NAME))`；不启动 watcher。
- `setTheme(name, enableWatcher = false): {success, error?}`：
  - 成功 → 同上 + 触发 `onThemeChangeCallback` + `{success:true}`。
  - 失败 → 回退 system（不启 watcher），返回 `{success:false, error: error instanceof Error ? error.message : String(error)}`。
  - **注意**：`currentThemeName` 在失败后被设为 `"system"`（不是传入的名字）。
- `setThemeInstance(themeInstance)` → `setGlobalTheme(t)`；`currentThemeName = "<in-memory>"`；`stopThemeWatcher()`；触发回调。
- `onThemeChange(cb)` → 保存单回调（**只保留最后一个**）。

#### 2.2.10 主题文件热重载（`startThemeWatcher` / `stopThemeWatcher`）

```
startThemeWatcher():
  stopThemeWatcher()
  if (!currentThemeName || name in {"dark","light",SYSTEM_THEME_NAME}) return
  customThemesDir = getCustomThemesDir(); watchedThemeName = currentThemeName
  watchedFileName = `${watchedThemeName}.json`; themeFile = join(customThemesDir, watchedFileName)
  if (!existsSync(themeFile)) return
  scheduleReload():
    if (themeReloadTimer) clearTimeout(themeReloadTimer)
    themeReloadTimer = setTimeout(() => {
      themeReloadTimer = undefined
      if (currentThemeName !== watchedThemeName) return      // 忽略过期定时器
      if (!existsSync(themeFile)) return                     // 保留最后一次成功加载的主题
      try {
        reloaded = loadThemeFromPath(themeFile)
        registeredThemes.set(watchedThemeName, reloaded)
        setGlobalTheme(reloaded)
        onThemeChangeCallback?.()
      } catch { /* 编辑中可能非法，忽略 */ }
    }, 100)
  themeWatcher = watchWithErrorHandler(customThemesDir, (eventType, filename) => {
      if (currentThemeName !== watchedThemeName) return
      if (!filename) { scheduleReload(); return }
      if (filename !== watchedFileName) return
      scheduleReload()
    }, () => { closeWatcher(themeWatcher); themeWatcher = undefined })
```
**去抖 100 ms**（测试用 fake timer 断言）。`stopThemeWatcher()` 清定时器 + 关 watcher。

#### 2.2.11 HTML 导出辅助

- `getResolvedThemeColors(themeName?): Record<string,string>`：`loadTheme(name ?? currentThemeName ?? SYSTEM_THEME_NAME).colors` → 每个值 `colorToHex(color)`。**保持 token 声明顺序**（`Object.fromEntries(Object.entries(...))`）。
- `isLightTheme(themeName?)` → `loadTheme(...).appearance === "light"`。
- `getThemeExportColors(themeName?): {pageBg?, cardBg?, infoBg?}`：
  - 名称为 system → `{}`（**逐字行为**：system 无 export 色）。
  - 读取 themeJson.export；无 → `{}`。
  - `resolve(value)`：undefined → undefined；解析变量；number → `colorToHex(indexedColor(n))`；`""` → undefined；`/^okhsl\(/i` → `colorToHex(parseColor(v))`；其它原样。
  - 任何异常 → `{}`。

#### 2.2.12 CLI 语法高亮主题

`buildCliHighlightTheme(t)` 把 highlight.js 的 scope 名映射到主题 token：

| scope | 处理 |
|---|---|
| keyword | `fg("syntaxKeyword")` |
| built_in | `fg("syntaxType")` |
| literal | `fg("syntaxNumber")` |
| number | `fg("syntaxNumber")` |
| regexp | `fg("syntaxString")` |
| string | `fg("syntaxString")` |
| subst | `fg("text")` |
| comment | `fg("syntaxComment")` |
| doctag | `fg("syntaxComment")` |
| meta | `fg("muted")` |
| function | `fg("syntaxFunction")` |
| title | `fg("syntaxFunction")` |
| class | `fg("syntaxType")` |
| type | `fg("syntaxType")` |
| tag | `fg("syntaxPunctuation")` |
| name | `fg("syntaxKeyword")` |
| attr | `fg("syntaxVariable")` |
| variable | `fg("syntaxVariable")` |
| params | `fg("syntaxVariable")` |
| operator | `fg("syntaxOperator")` |
| punctuation | `fg("syntaxPunctuation")` |
| emphasis | `italic` |
| strong | `bold` |
| link | `underline` |
| addition | `fg("toolDiffAdded")` |
| deletion | `fg("toolDiffRemoved")` |

`getCliHighlightTheme(t)` 按 `Theme` 实例身份缓存一份。

`highlightCode(code, lang?)`：
- `validLang = lang && supportsLanguage(lang) ? lang : undefined`。
- 无有效语言 → `code.split("\n").map(line => theme.fg("mdCodeBlock", line))`（**不**自动检测语言）。
- 有 → `highlight(code, {language: validLang, ignoreIllegals:true, theme: getCliHighlightTheme(theme)}).split("\n")`；异常 → `code.split("\n")`（**不加颜色**）。

`getLanguageFromPath(filePath): string | undefined`：取 `filePath.split(".").pop()?.toLowerCase()`；映射表（逐字，顺序如下）：

```
ts/tsx→typescript, js/jsx/mjs/cjs→javascript, py→python, rb→ruby, rs→rust, go→go,
java→java, kt→kotlin, swift→swift, c→c, h→c, cpp/cc/cxx→cpp, hpp→cpp, cs→csharp,
php→php, sh/bash/zsh→bash, fish→fish, ps1→powershell, sql→sql, html/htm→html,
css→css, scss→scss, sass→sass, less→less, json→json, yaml/yml→yaml, toml→toml,
xml→xml, md/markdown→markdown, dockerfile→dockerfile, makefile→makefile, cmake→cmake,
lua→lua, perl→perl, r→r, scala→scala, clj→clojure, ex/exs→elixir, erl→erlang,
hs→haskell, ml→ocaml, vim→vim, graphql→graphql, proto→protobuf, tf/hcl→hcl
```

#### 2.2.13 TUI 主题工厂

```ts
getMarkdownTheme(): MarkdownTheme {
  heading: fg("mdHeading"), link: fg("mdLink"), linkUrl: fg("mdLinkUrl"),
  code: fg("mdCode"), codeBlock: fg("mdCodeBlock"), codeBlockBorder: fg("mdCodeBlockBorder"),
  quote: fg("mdQuote"), quoteBorder: fg("mdQuoteBorder"), hr: fg("mdHr"),
  listBullet: fg("mdListBullet"), bold: theme.bold, italic: theme.italic,
  underline: theme.underline, strikethrough: theme.strikethrough,
  highlightCode: (code, lang?) => { /* 同 highlightCode，但回退分支是
      code.split("\n").map(line => theme.fg("mdCodeBlock", line))（带颜色） */ }
}
getSelectListTheme(): SelectListTheme {
  selectedPrefix: fg("accent"), selectedText: fg("accent"),
  description: fg("muted"), scrollInfo: fg("muted"), noMatch: fg("muted")
}
getEditorTheme(): EditorTheme { borderColor: fg("borderMuted"), selectList: getSelectListTheme() }
getSettingsListTheme(): SettingsListTheme {
  label: (text, selected) => selected ? fg("accent", text) : text,
  value: (text, selected) => selected ? fg("accent", text) : fg("muted", text),
  description: fg("dim"), cursor: fg("accent", "→ "), hint: fg("dim")
}
```
注意 `highlightCode` 的两条回退分支不同：`theme.highlightCode()` 无语言时**加** mdCodeBlock 色，`MarkdownTheme.highlightCode` 的 catch 分支才加；无语言时同样加。

---


### 2.3 `theme/system-theme.ts`（655 行）

这个文件是**整个单元中最难逐位复刻的部分**：它用一组拟合多项式把终端颜色映射成完整的 56 色主题，且测试用 WCAG 对比度与 OKLab 数值断言。必须逐位精确复现所有常数与循环。

#### 2.3.1 导出

```ts
export const SYSTEM_THEME_NAME = "system";
export interface SystemThemeInput {
  foreground?: RgbColor; background?: RgbColor;
  palette?: RgbColor[];          // ANSI colors 0-15
  saturation?: number;           // 0..1，首帧灰阶
  appearanceHint?: ThemeAppearance;
}
export interface SystemThemeColors {
  colors: Record<ThemeToken, string | number>;  // hex、palette index 或 ""
  dim: ThemeColor[];                            // 终端未上报颜色时用 SGR 2 的前景 token
  appearance: ThemeAppearance | undefined;
}
export function relativeLuminance({r,g,b}: RgbColor): number;
export function wcagContrast(first: RgbColor, second: RgbColor): number;
export function terminalAppearance(background: RgbColor, foreground?: RgbColor): ThemeAppearance;
export function generateSystemThemeColors(input: SystemThemeInput): SystemThemeColors;
```

#### 2.3.2 FAMILIES（14 族，逐字数值）

| family | hue | saturation.min | saturation.max | slot |
|---|---|---|---|---|
| neutral | 231.49 | 0.02 | 0.08 | 8 |
| blue | 231.49 | 0.1 | 0.68 | 4 |
| green | 158.68 | 0.1 | 0.76 | 2 |
| red | 20 | 0.1 | 0.92 | 1 |
| yellow | 82.36 | 0.5 | 1 | 3 |
| orange | 52 | 0.12 | 0.85 | 3 |
| violet | 295 | 0.2 | 0.6 | 5 |
| calamine | 202.43 | 0.1 | 0.74 | 6 |
| thinkingSlate | 231.49 | 0.08 | 0.2 | 4 |
| thinkingBlue | 231.49 | 0.2 | 0.45 | 4 |
| thinkingPeriwinkle | 263.25 | 0.3 | 0.6 | 6 |
| thinkingViolet | 295 | 0.4 | 0.75 | 5 |
| thinkingMagenta | 337.5 | 0.5 | 0.85 | 13 |
| thinkingRed | 20 | 0.95 | 1 | 1 |

#### 2.3.3 TOKEN_FAMILIES（token → family，**声明顺序决定输出 key 顺序**）

按源码顺序（分组之间空行是注释分隔，不影响顺序）：

```
selectedBg:blue, searchMatchBg:orange, userMessageBg:blue, customMessageBg:violet,
toolPendingBg:neutral, toolSuccessBg:green, toolErrorBg:red,
text:neutral, userMessageText:neutral, customMessageText:neutral, toolTitle:neutral,
syntaxOperator:neutral, syntaxPunctuation:neutral, muted:neutral, dim:neutral,
thinkingText:neutral, toolOutput:neutral, mdLinkUrl:neutral, mdQuote:neutral,
mdQuoteBorder:neutral, mdHr:neutral, mdCodeBlockBorder:neutral, toolDiffContext:neutral,
syntaxComment:neutral, scrollbarTrack:neutral, scrollbarThumb:neutral,
searchMatchText:neutral, borderMuted:neutral,
accent:violet, borderAccent:violet, customMessageLabel:violet, mdCode:violet,
mdListBullet:violet, syntaxType:violet, border:blue, mdLink:blue, syntaxKeyword:blue,
syntaxVariable:calamine, success:green, mdCodeBlock:green, toolDiffAdded:green,
bashMode:green, syntaxNumber:green, error:red, toolDiffRemoved:red, warning:yellow,
mdHeading:yellow, syntaxFunction:yellow, syntaxString:orange,
thinkingOff:neutral, thinkingMinimal:thinkingSlate, thinkingLow:thinkingBlue,
thinkingMedium:thinkingPeriwinkle, thinkingHigh:thinkingViolet,
thinkingXhigh:thinkingMagenta, thinkingMax:thinkingRed
```

`TOKEN_SLOTS = { syntaxString: 2, syntaxNumber: 5, searchMatchBg: 3 }`（覆盖 family.slot）。

#### 2.3.4 LEVELS：目标亮度曲线（6 次多项式，系数顺序为 0..5 次幂）

`levelTarget(level, appearance, surfaceL)`：若 `surfaceL < reachable[0] || surfaceL > reachable[1]` → `undefined`；否则
`Σ coefficients[i] * surfaceL^i`（i 从 0 到 5）。

| level | appearance | coefficients（c0..c5） | reachable |
|---|---|---|---|
| panel | dark | 0.29131, -0.39746, 2.33185, -0.85524, -1.2076, 0.86276 | [0, 0.979] |
| panel | light | -3.74073, 27.94549, -78.44258, 112.6798, -79.60015, 22.11277 | [0.348, 1] |
| track | dark | 0.39028, -0.23015, 0.83573, 2.43829, -4.38292, 2.01582 | [0, 0.946] |
| track | light | -5.24921, 38.37322, -107.28833, 152.10005, -106.17127, 29.18061 | [0.368, 1] |
| thinking0 | dark | 0.52988, -0.05809, -0.30924, 4.63567, -6.52933, 2.89108 | [0, 0.873] |
| thinking0 | light | -28.27749, 182.85284, -469.62416, 603.15916, -384.59976, 97.35147 | [0.51, 1] |
| thinking1 | dark | 0.55278, -0.03667, -0.45659, 4.95347, -6.90265, 3.0706 | [0, 0.858] |
| thinking1 | light | -37.10484, 235.86282, -596.62344, 754.3633, -474.00763, 118.3551 | [0.535, 1] |
| thinking2 | dark | 0.57486, -0.01765, -0.58987, 5.25227, -7.27175, 3.25532 | [0, 0.842] |
| thinking2 | light | -59.89653, 377.05024, -945.07843, 1182.03145, -734.96375, 181.68658 | [0.556, 1] |
| thinking3 | dark | 0.59621, -0.00062, -0.71148, 5.53588, -7.6392, 3.44606 | [0, 0.827] |
| thinking3 | light | -72.07122, 445.84082, -1099.57352, 1353.88793, -829.53392, 202.26164 | [0.58, 1] |
| thinking4 | dark | 0.61691, 0.01462, -0.82288, 5.80651, -8.00641, 3.64333 | [0, 0.811] |
| thinking4 | light | -110.14338, 674.21488, -1645.75941, 2004.32367, -1215.15899, 293.3183 | [0.6, 1] |
| thinking5 | dark | 0.63702, 0.02826, -0.92498, 6.06465, -8.37246, 3.84651 | [0, 0.795] |
| thinking5 | light | -175.47701, 1063.54495, -2570.70594, 3098.80776, -1860.15527, 444.76392 | [0.62, 1] |
| thinking6 | dark | 0.65658, 0.04044, -1.01835, 6.30989, -8.73529, 4.05439 | [0, 0.779] |
| thinking6 | light | -183.81712, 1094.70055, -2602.68539, 3088.71276, -1826.91131, 430.75931 | [0.643, 1] |
| subtle | dark | 0.56762, -0.02475, -0.5383, 5.12628, -7.10931, 3.17324 | [0, 0.848] |
| subtle | light | -232.85459, 1376.54473, -3249.11801, 3827.91186, -2248.29472, 526.55751 | [0.657, 1] |
| thumb | dark | 0.60323, 0.00278, -0.73328, 5.57157, -7.68067, 3.46933 | [0, 0.823] |
| thumb | light | -82.89897, 511.01355, -1255.98095, 1540.76821, -940.68087, 228.58523 | [0.586, 1] |
| readable | dark | 0.66937, 0.04704, -1.06871, 6.43941, -8.9332, 4.17229 | [0, 0.77] |
| readable | light | -1554.52576, 8733.56817, -19604.93507, 21977.72696, -12300.99599, 2749.81288 | [0.751, 1] |
| emphasis | dark | 0.7303, 0.07695, -1.31626, 7.1681, -10.14436, 4.92846 | [0, 0.712] |
| emphasis | light | -4948.31942, 26870.91986, -58334.48399, 63280.17197, -34298.01053, 7430.30146 | [0.811, 1] |
| textOnPanel | dark | 0.86713, 0.05232, -0.89428, 4.79014, -5.5432, 1.75023 | [0, 0.542] |
| textOnPanel | light | -8570.89457, 43954.60805, -90084.00702, 92220.6791, -47152.15802, 9632.27113 | [0.867, 1] |
| text | dark | 0.89242, 0.02311, -0.44862, 2.34417, -0.06084, -2.63844 | [0, 0.5] |
| text | light | -2004.67048, 6664.47299, -6060.70202, -1792.61209, 5133.82359, -1939.85583 | [0.894, 1] |

#### 2.3.5 常量集合与 RULES

```ts
const TOOL_PANELS: Surface[] = ["toolPendingBg","toolSuccessBg","toolErrorBg"];
const MESSAGE_PANELS: Surface[] = ["userMessageBg","customMessageBg"];
const PANELS: ThemeBg[] = ["userMessageBg","toolPendingBg","toolSuccessBg","toolErrorBg","selectedBg","searchMatchBg","customMessageBg"];
const THINKING: ThemeColor[] = ["thinkingOff","thinkingMinimal","thinkingLow","thinkingMedium","thinkingHigh","thinkingXhigh","thinkingMax"];
const THINKING_LEVELS: Level[] = ["thinking0","thinking1","thinking2","thinking3","thinking4","thinking5","thinking6"];
const READABLE_FLOOR: Record<ThemeAppearance, Level> = { dark: "readable", light: "subtle" };
const FOREGROUND_LEVEL: Level = "emphasis";
const FOREGROUND_TOKENS: ThemeColor[] = ["text","userMessageText","toolTitle"];
const TEXT_MINIMUM_WCAG_CONTRAST = 4.5;
```

RULES（顺序敏感，`SOLVE_ORDER` 由它推导）：

```
each(PANELS, ["background"], "panel")
{ text,             on: ["background"],                                    level: "text" }
{ text,             on: ["selectedBg"],                                    level: "textOnPanel" }
{ userMessageText,  on: ["userMessageBg"],                                 level: "textOnPanel" }
{ toolTitle,        on: TOOL_PANELS,                                       level: "textOnPanel" }
each(["accent","success","error","warning"], ["background","selectedBg", ...TOOL_PANELS], "readable")
{ muted,            on: ["background","selectedBg","customMessageBg", ...TOOL_PANELS], level: "readable" }
{ dim,              on: ["background","selectedBg","customMessageBg", ...TOOL_PANELS], level: "subtle" }
{ thinkingText,     on: ["background"],                                    level: "readable" }
{ customMessageText,on: ["customMessageBg", ...TOOL_PANELS],               level: "readable" }
{ customMessageLabel,on:["background","customMessageBg","selectedBg", ...TOOL_PANELS], level: "readable" }
{ toolOutput,       on: ["background", ...TOOL_PANELS],                    level: "readable" }
each(["mdHeading","mdLink","mdLinkUrl","mdCode","mdQuote","mdCodeBlockBorder","mdListBullet"],
     ["background", ...MESSAGE_PANELS], "readable")
{ mdCodeBlock,      on: ["background", ...MESSAGE_PANELS, ...TOOL_PANELS], level: "readable" }
each(["toolDiffAdded","toolDiffRemoved","toolDiffContext"], ["background", ...TOOL_PANELS], "readable")
each(["syntaxComment","syntaxKeyword","syntaxFunction","syntaxVariable","syntaxString",
      "syntaxNumber","syntaxType","syntaxOperator","syntaxPunctuation"],
     ["background", ...MESSAGE_PANELS, ...TOOL_PANELS], "readable")
{ searchMatchText,  on: ["searchMatchBg"],                                 level: "readable" }
each(["bashMode","border","borderAccent"], ["background"],                 level: "readable")
{ borderMuted,      on: ["background"],                                    level: "subtle" }
each(["mdQuoteBorder","mdHr"], ["background", ...MESSAGE_PANELS, ...TOOL_PANELS], "readable")
{ scrollbarTrack,   on: ["background"],                                    level: "track" }
{ scrollbarThumb,   on: ["scrollbarTrack"],                                level: "thumb" }
THINKING.map((token,i) => ({ token, on: ["background"], level: THINKING_LEVELS[i] }))
```

`SOLVE_ORDER`：深度优先后序（DFS post-order），对每条 rule 的 token 调 `visit`；`visit(token)` 先对所有 `on` 中非 `"background"` 的 surface 递归，再 push token。**必须保持这个顺序**，否则 `solve()` 读到未定义 surface 会退化。

#### 2.3.6 数学与工具函数

```ts
const clamp = (v, min, max) => Math.min(max, Math.max(min, v));

/** OKLab lightness of an sRGB color, 0-1 */
function oklabLightness(color) { return colorToOklch(rgbColor(color.r,color.g,color.b)).l }

/** WCAG 2 relative luminance */
export function relativeLuminance({r,g,b}) {
  const linear = (channel) => { const v = channel/255; return v <= 0.04045 ? v/12.92 : ((v+0.055)/1.055)**2.4; };
  return 0.2126*linear(r) + 0.7152*linear(g) + 0.0722*linear(b);
}
export function wcagContrast(a,b) {
  const [x,y] = [relativeLuminance(a), relativeLuminance(b)];
  return (Math.max(x,y)+0.05)/(Math.min(x,y)+0.05);
}
export function terminalAppearance(background, foreground?) {
  const white = {r:255,g:255,b:255}, black = {r:0,g:0,b:0};
  const whiteContrast = wcagContrast(white, background), blackContrast = wcagContrast(black, background);
  if (foreground) {
    const fgL = oklabLightness(foreground), bgL = oklabLightness(background);
    if (Math.abs(fgL - bgL) > 0.05) {
      const appearance = fgL > bgL ? "dark" : "light";
      const best = appearance === "dark" ? whiteContrast : blackContrast;
      if (best >= TEXT_MINIMUM_WCAG_CONTRAST) return appearance;
    }
  }
  return whiteContrast >= blackContrast ? "dark" : "light";
}
function hexOf({r,g,b}) {
  return `#${[r,g,b].map(c => Math.round(c).toString(16).padStart(2,"0")).join("")}`;
}
/** 高斯（中心 0.5，σ 0.25）归一化到 [0,1] */
function bellWeight(lightness) {
  const gaussian = (x) => Math.exp(-((x-0.5)**2)/(2*0.25**2));
  return (gaussian(lightness) - gaussian(0)) / (1 - gaussian(0));
}
function saturationCurve({saturation:{min,max}}, lightness) {
  const floor = max > 0 ? min/max : 1;
  return floor + (1-floor)*bellWeight(lightness);
}
function levelTarget(level, appearance, surfaceL) { /* 见 2.3.4 */ }
```

**`anchored(source, family, lightness, saturation)`**（关键：chroma 上限）

```ts
const anchor = saturationCurve(family, source.l);
const falloff = anchor > 0 ? Math.min(1, saturationCurve(family, lightness) / anchor) : 1;
const color = okhslColor(source.h, source.s * falloff * saturation, lightness);
const cap = source.chroma * falloff * saturation;
const { l, c } = colorToOklch(color);
return c <= cap ? color : colorToRgb(oklchColor(l, cap, source.h));
```
`sourceOf(color)` = `{...colorToOkhsl(color), chroma: colorToOklch(color).c}`。

**`withTextContrast(color, surfaces, lighter)`**：若 `meets(color)` 直接返回；否则取 `{h,s,l} = okhslOf(color)`，`at(L) = okhslColor(h,s,L)`；`extreme = lighter ? 1 : 0`；若 `!meets(at(extreme))` → `at(extreme)`；否则在 `[l, extreme]` 上二分 20 次找最小满足点（`meets(at(middle))` → high=middle else low=middle），返回 `at(high)`。

#### 2.3.7 `generateSystemThemeColors(input)` 主体

```
saturation = clamp(input.saturation ?? 1, 0, 1)
if (!background) return indexedColors(saturation, input.appearanceHint)
palette = input.palette?.length === 16 ? input.palette.map(sourceOf) : undefined
appearance = terminalAppearance(background, foreground)
lighter = appearance === "dark"
extreme = lighter ? 1 : 0
backgroundL = oklabLightness(background)

paint(token, oklabL):
  lightness = oklabToOkhslLightness(oklabL)
  family = FAMILIES[TOKEN_FAMILIES[token]]
  if (!palette) return okhslColor(family.hue, (min + (max-min)*bellWeight(lightness)) * saturation, lightness)
  return anchored(palette[TOKEN_SLOTS[token] ?? family.slot], family, lightness, saturation)

target(level, surfaceL, t):
  reached = levelTarget(level, appearance, surfaceL)
  if (reached === undefined && t === 0) return undefined
  distance = (reached ?? extreme) - surfaceL
  floor = (levelTarget(READABLE_FLOOR[appearance], appearance, surfaceL) ?? extreme) - surfaceL
  compressed = |distance| > |floor| ? distance - (distance - floor) * min(t,1) : distance
  return surfaceL + compressed * (1 - max(0, t-1))

extremeText = lighter ? white : black
readable(color) = wcagContrast(extremeText, color) >= 4.5
limitPanel(token, l):
  color = paint(token, l); if (readable(color)) return color
  [low, high] = [backgroundL, l]
  for 20 次: middle=(low+high)/2; if readable(paint(token,middle)) low=middle else high=middle
  return paint(token, low)

solve(t):
  colors = Map([["background", background]])
  for token of SOLVE_ORDER:
    targets = []
    for rule of RULES where rule.token === token:
      for surface of rule.on:
        value = target(rule.level, oklabLightness(colors.get(surface) ?? background), t)
        if (value === undefined || value < 0 || value > 1) return undefined
        targets.push(value)
    l = lighter ? max(targets) : min(targets)
    colors.set(token, PANELS.includes(token) ? limitPanel(token, l) : paint(token, l))
  return colors

relaxation = 0
colors = solve(0)
if (!colors) {
  [low, high] = [0, 2]; colors = solve(high)
  for 20 次: middle=(low+high)/2; attempt=solve(middle)
    if (attempt) { high = middle; colors = attempt } else low = middle
  relaxation = high
}
solved = colors ?? new Map()

// 输出：按 TOKEN_FAMILIES 的 key 顺序
for token of keys(TOKEN_FAMILIES): result[token] = solved.get(token) ? hexOf(color) : ""

// 前景微调（text / userMessageText / toolTitle）
for token of FOREGROUND_TOKENS:
  surfaces = RULES.filter(r => r.token===token).flatMap(r => r.on.map(s => solved.get(s) ?? background))
  let text = solved.get(token)
  if (foreground) {
    targets = surfaces.map(s => target(FOREGROUND_LEVEL, oklabLightness(s), relaxation))
    if (targets.every(v => v !== undefined && v >= 0 && v <= 1)) {
      needed = lighter ? max(targets) : min(targets)
      foregroundL = oklabLightness(foreground)
      if (lighter ? foregroundL >= needed : foregroundL <= needed) { result[token] = ""; continue }
      text = anchored(sourceOf(foreground), FAMILIES.neutral, oklabToOkhslLightness(needed), saturation)
    }
  }
  if (text) result[token] = hexOf(withTextContrast(text, surfaces, lighter))
return { colors: result, dim: [], appearance }
```

**`indexedColors(saturation, appearance)`**（终端什么都没报告）

```
for [token, familyName] of entries(TOKEN_FAMILIES):
  if (PANELS.includes(token)) { colors[token] = ""; continue }
  neutral = familyName === "neutral"
  colors[token] = (!neutral && saturation > 0) ? (TOKEN_SLOTS[token] ?? FAMILIES[familyName].slot) : ""
  if (neutral && !FOREGROUND_TOKENS.includes(token)) dim.push(token)
return { colors, dim, appearance }
```

因此**无颜色终端**下：panels 为空串（终端默认背景，无背景色），非中性 token 用 0–15 的 ANSI 索引（数字，不是字符串），中性 token 为空串且除 `text/userMessageText/toolTitle` 外全部进入 `dim`（渲染为 SGR 2）。

---


### 2.4 `theme/theme-controller.ts`（253 行）

```ts
type ThemeResult = { success: boolean; error?: string };
const TERMINAL_QUERY_TIMEOUT_MS = 100;

export function requestTerminalColors(ui: TUI, apply: (colors: TerminalColors) => void): Promise<void>;
export class InteractiveThemeController {
  constructor(ui: TUI, options: {
    getSettingsManager: () => SettingsManager;
    showError: (message: string) => void;
    onChanged: () => void;
    initialThemeSetting?: string;
  });
  rebindTui(): void;
  applyFromSettings(): void;
  waitForTerminalColors(): Promise<void>;
  getThemeSelection(): string | undefined;
  setThemeName(themeName: string, showError = false): ThemeResult;
  setThemeSetting(themeSetting: string): void;
  setThemeInstance(themeInstance: Theme): ThemeResult;
  preview(themeSettingOrName: string): void;
  disableAutoSync(): void;
  dispose(): void;
  getTerminalTheme(): TerminalTheme;
}
```

**`requestTerminalColors`**
```
try { query = ui.queryTerminalColors({ timeoutMs: 100, onLateReply: apply }) }
catch { query = Promise.resolve({}) }   // 查询失败等同终端不上报
return query.then(apply, () => apply({}))
```

**`sameTerminalColors(a,b)`**：foreground/background 逐通道相等（`undefined` 与 `undefined` 视为相等）；`palette` 引用相等即 true；否则长度相等且逐项 RGB 相等。

**构造**
1. 保存 ui/getSettingsManager/showError/onChanged/`currentThemeSetting = options.initialThemeSetting`。
2. `activeThemeName = resolveThemeName()`（`resolveThemeSetting(getThemeSetting(), getTerminalTheme()) ?? SYSTEM_THEME_NAME`）。
3. `markTerminalColorsPending()`（system 主题先灰阶）。
4. `initTheme(activeThemeName, true)`。
5. `bindTerminalColorSchemeListener()`。

**`rebindTui()`**：先退订旧监听，重新 `bind`，`ui.setTerminalColorSchemeNotifications(autoSyncEnabled)`。

**`applyFromSettings()`**
```
themeSetting = getThemeSetting()
themeName = resolveThemeName()
setAutoSync(parseAutoThemeSetting(themeSetting) !== undefined || themeName === SYSTEM_THEME_NAME)
applyThemeName(themeName, themeSetting !== undefined)   // showError 只在“设置里显式指定了主题”时为真
queryTerminalColors()
```

**`waitForTerminalColors()`** → 返回最新一次查询的 promise。

**`getThemeSelection()`** → `currentThemeSetting ?? settingsManager.getThemeSetting() ?? activeThemeName`。

**`setThemeName(name, showError=false)`**：`setAutoSync(name === SYSTEM_THEME_NAME)`；`applyThemeName`；成功则 `currentThemeSetting = name`。

**`setThemeSetting(setting)`** → 存值后 `applyFromSettings()`。

**`setThemeInstance(t)`** → `setAutoSync(false)`；`theme.setThemeInstance(t)`；`activeThemeName = "<in-memory>"`；`notifyChanged()`；`{success:true}`。

**`preview(themeSettingOrName)`**：`themeName = resolveThemeSetting(arg, getTerminalTheme()) ?? activeThemeName`；`setTheme(themeName, true)` 成功 → `ui.invalidate(); ui.requestRender()`。

**`private applyThemeName(name, showError=false)`**
```
result = setTheme(name, true)                      // theme.ts 的全局 setTheme
activeThemeName = result.success ? name : SYSTEM_THEME_NAME
notifyChanged()
if (!result.success && showError) showError(`Failed to load theme "${name}": ${result.error}\nFell back to the system theme.`)
return result
```
**【逐字】** 错误消息。

**`private applyTerminalColors(reported)`**
```
previous = this.terminalColors
next = { foreground: reported.foreground ?? previous?.foreground,
         background: reported.background ?? previous?.background,
         palette:    reported.palette    ?? previous?.palette }
if (previous && sameTerminalColors(previous, next)) return   // 超时/无变化 → 不重绘
this.terminalColors = next
setTerminalColors(next)
reapplyForTerminal()
ui.invalidate(); ui.requestRender()
```

**`private reapplyForTerminal()`**：`activeThemeName === "<in-memory>"` → 直接返回；否则 `themeName = resolveThemeName()`；若 `themeName === SYSTEM_THEME_NAME || themeName !== activeThemeName` → `applyThemeName(themeName)`。

**`private applyTerminalColorSchemeChange(terminalTheme)`**：`!autoSyncEnabled` → 返回；记录 `previous = getTerminalTheme()`；`setTerminalColorScheme(terminalTheme)`；若主题改变 → `reapplyForTerminal()`；随后 `queryTerminalColors()`（终端颜色也变了，需重新查询）。

**`private notifyChanged()`** → `ui.invalidate(); onChanged()`。

**测试对应**（`theme-controller.test.ts`，全部必须通过）：
- `uses the initial theme without persisting it`
- `applies the theme immediately and lets startup wait for the colors`
- `falls back to palette indices, then applies colors that arrive after the timeout`
- `re-queries the colors on appearance changes and lets them decide`
- `uses the reported scheme for the system theme when the terminal reports no colors`
- `re-renders only when the reported colors change`
- `disables terminal appearance updates when disposed`
- `lets an explicit selection replace the initial theme`
- `reloads theme settings when no initial theme was supplied`

### 2.5 `theme/theme-schema.ts`（146 行）

```ts
export const ColorValueSchema;               // union(string, integer 0..255)
export type ThemeColorValue = Static<typeof ColorValueSchema>;
export const ThemeJsonSchema;                // typebox object
export type ValidatedThemeJson = Static<typeof ThemeJsonSchema>;
export function validateThemeJson(label: string, json: unknown): ValidatedThemeJson;
```

- `colorValue(description?)` → `Type.Union([Type.String({description: "Hex color (#RGB or #RRGGBB), OKLCH or OKHSL color, variable reference, or empty string for terminal default"}), Type.Integer({minimum:0, maximum:255, description:"256-color palette index (0-255)"})], description ? {description} : {})`。
- `ThemeColorsSchema` = `Type.Object(themeTokenProperties(THEME_TOKENS), {description: "Theme color definitions (scrollbar, thinkingMax, and search highlight colors are optional and use compatible fallbacks)", additionalProperties: false})`；每个 token 的 schema 在 `descriptor.fallback !== undefined` 时包成 `Type.Optional`。
- `ThemeJsonSchema`（对象，`additionalProperties: false`，title `Kiana Coding Agent Theme`，description `Theme schema for Kiana coding agent`）字段：
  - `$schema?`: string，description `JSON schema reference`
  - `name`: string，`pattern: "^[^/]+$"`，description `Theme name. Must not contain '/' because it is reserved for automatic light/dark theme settings.`
  - `appearance?`: `"dark" | "light"`，description `Background the theme is designed for. Detected from the theme colors when omitted.`
  - `vars?`: `Record<string, ColorValueSchema>`，description `Reusable color variables`
  - `colors`: ThemeColorsSchema（必填）
  - `export?`: `{ pageBg?, cardBg?, infoBg? }`，description `Optional colors for HTML export (defaults derived from userMessageBg if not specified)`，additionalProperties false；三个字段分别描述 `Page background color` / `Card/container background color` / `Info sections background (system prompt, notices)`
- `validateThemeJson(label, json)`：
  1. 若 `typeof json === "object" && json !== null && "name" in json && typeof json.name === "string" && json.name.includes("/")` → 抛
     `Invalid theme name "${json.name}": theme names cannot contain "/" because it is reserved for automatic light/dark theme settings.`
  2. `compiledThemeSchema.Check(json)` 失败 → 汇总错误：
     - 对 `error.keyword === "required" && error.instancePath === "/colors"` 的错误，把 `params.requiredProperties` 里每项加入 `missingColors`（Set）。
     - 其余：`- ${instancePath || "/"}: ${error.message}` 加入 `otherErrors`。
     - 组装：
       ```
       Invalid theme "${label}":
       ```
       若 `missingColors.size > 0` 追加：
       ```
       \n\nMissing required color tokens:\n
         - <按字母排序的缺失 token，每行一个>
       \n\nPlease add these colors to your theme's "colors" object.
       \nSee the built-in themes (dark.json, light.json) for reference values.
       ```
       若 `otherErrors.length > 0` 追加 `\n\nOther errors:\n${otherErrors.join("\n")}`。
  3. 通过 → 原样返回 cast。

**Rust 建议**：不必复刻 typebox；直接手写校验器产出**完全相同**的错误文本（缺失 token 按字母排序、路径格式 `/colors/xxx`）。JSON Schema 文件本身（`theme.schema.json`）由 `ca-resources` 负责发布，本单元只需能读取 `$schema` 字段。

### 2.6 `theme/dark.json` 与 `theme/light.json`（逐字）

两个文件都是内置主题，**必须逐字节照抄**（含 `$schema` 行与缩进，见附录 A）。结构：

```json
{
  "$schema": "https://kiana.dev/schemas/theme.schema.json",
  "name": "dark",
  "appearance": "dark",
  "vars": { "text": ..., "muted": ..., "violet": ..., "blue": ..., "green": ..., "red": ..., "yellow": ..., "blueBg": ... },
  "colors": { /* 56 个 token */ },
  "export": { "pageBg": ..., "cardBg": ..., "infoBg": ... }
}
```

dark 的 `vars`：`text=okhsl(234 3% 89%)`、`muted=okhsl(229 6% 67%)`、`violet=okhsl(295 50% 67%)`、`blue=okhsl(232 54% 67%)`、`green=okhsl(159 59% 67%)`、`red=okhsl(20 72% 67%)`、`yellow=okhsl(83 88% 67%)`、`blueBg=okhsl(233 41% 24%)`。
light 的 `vars`：`text=okhsl(225 5% 27%)`、`muted=okhsl(229 8% 47%)`、`violet=okhsl(295 60% 46%)`、`blue=okhsl(231 68% 47%)`、`green=okhsl(159 75% 46%)`、`red=okhsl(20 91% 47%)`、`yellow=okhsl(83 99% 47%)`、`blueBg=okhsl(235 19% 91%)`。

其余 values 见附录 A（完整 JSON 副本）。

### 2.7 `chat-viewport.ts`（46 行）

```ts
export interface ChatViewportOptions {
  readonly document: Component;
  readonly pendingMessages: Component;
  readonly status: Component;
  readonly editor: Component;
  readonly footer: Component;
  readonly widgetsAbove?: Component;
  readonly widgetsBelow?: Component;
  readonly scrollbar?: ScrollViewScrollbar;
  readonly scrollbarTrackStyle?: (text: string) => string;
  readonly scrollbarThumbStyle?: (text: string) => string;
}
export interface ChatViewport { readonly root: Component; readonly transcript: ScrollView; }
export function createChatViewport(options: ChatViewportOptions): ChatViewport;
```

实现（逐字语义）：
```ts
const transcript = new ScrollView(options.document, {
  follow: "end", primary: true, overscroll: "chain",
  scrollbar: options.scrollbar ?? "auto",
  ...(options.scrollbarTrackStyle === undefined ? {} : { scrollbarTrackStyle: options.scrollbarTrackStyle }),
  ...(options.scrollbarThumbStyle === undefined ? {} : { scrollbarThumbStyle: options.scrollbarThumbStyle }),
});
const dock = new VStack([
  { component: options.pendingMessages, shrink: 1, minSize: 0 },
  { component: options.status,          shrink: 1, minSize: 0 },
  ...(options.widgetsAbove === undefined ? [] : [{ component: options.widgetsAbove, shrink: 1, minSize: 0 }]),
  { component: options.editor,          shrink: 1, minSize: 3 },
  ...(options.widgetsBelow === undefined ? [] : [{ component: options.widgetsBelow, shrink: 1, minSize: 0 }]),
  { component: options.footer,          shrink: 1, minSize: 0 },
]);
return {
  transcript,
  root: new VStack([
    { component: transcript, basis: 0, grow: 1, shrink: 1, minSize: 1 },
    { component: dock, basis: "auto", grow: 0, shrink: 1, minSize: 1 },
  ]),
};
```
注意：`dock` 里 **editor 的 `minSize: 3`**，其余 0；root 的 transcript `basis: 0, grow: 1`，dock `basis: "auto", grow: 0`。

### 2.8 `external-editor.ts`（46 行）

```ts
export interface ExternalEditorOptions { command: string; content: string; }
export type ExternalEditorResult = { status: "complete"; content: string } | { status: "failed" };
export async function editInExternalEditor(options: ExternalEditorOptions): Promise<ExternalEditorResult>;
```

```
directory = mkdtempSync(join(tmpdir(), "kiana-editor-"))
filePath = join(directory, "prompt.md")
try {
  writeFileSync(filePath, content, "utf-8")
  [editor, ...editorArgs] = command.split(" ")
  process.stdout.write(`Launching external editor: ${command}\nKiana will resume when the editor exits.\n`)   // 【逐字】
  exitCode = await new Promise(resolve => {
    child = spawn(editor, [...editorArgs, filePath], { stdio: "inherit", shell: process.platform === "win32" })
    child.on("error", () => resolve(null))
    child.on("close", code => resolve(code))
  })
  if (exitCode !== 0) return { status: "failed" }
  return { status: "complete", content: stripBom(readFileSync(filePath,"utf-8")).replace(/\n$/, "") }
} finally { try { rmSync(directory, {recursive:true, force:true}) } catch {} }
```
- `stdio: "inherit"`；Windows 上 `shell: true`。
- **不使用 `spawnSync`**（注释解释：Windows 上同步子进程调用会在父进程暂停 stdin 后保持控制台输入读取活跃，与 vim/nvim 争抢输入缓冲）。
- 末尾**只**去掉**一个** `\n`。BOM 被剥离。
- 临时目录前缀 `kiana-editor-`，文件名 `prompt.md`，`finally` 中 best-effort 删除。

### 2.9 `model-search.ts`（44 行）

```ts
export interface ModelSearchItem { id: string; provider: string; name?: string; }
export function getModelSearchText(item: ModelSearchItem): string;
export function getDistinctModelName(item: {id: string; name?: string}): string | undefined;
export function getModelSelectorSearchText(item: ModelSearchItem): string;
const splitModelLabel = (value: string): string[] =>
  value.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean);
```

- `getModelSearchText`: `` `${id} ${provider} ${provider}/${id} ${provider} ${id}${name}` ``，其中 `name = item.name ? ` ${item.name}` : ""`。（注意 `provider` 出现两次、`id` 出现两次——这是刻意的模糊匹配加权。）
- `getDistinctModelName`：
  ```
  name = item.name?.trim(); if (!name) return undefined
  idWords = splitModelLabel(item.id); idTokens = new Set(idWords); compactId = idWords.join("")
  isCovered = (word) => idTokens.has(word) || (word.length > 1 && compactId.includes(word))
  return splitModelLabel(name).every(isCovered) ? undefined : name
  ```
  即：名字里每个词都被 ID 覆盖（或长于 1 的单词作为子串出现在紧凑 ID 中）→ 视为“只是换个写法的 ID”，返回 `undefined`。单字符词必须整词匹配（`"4"` 不被 `"mistral-small-2603"` 覆盖）。
- `getModelSelectorSearchText`: `` `${provider} ${provider}/${id} ${provider} ${id}${name}` ``（**注意开头是 provider 而不是 id**，目的是让 `/model` 的精确 provider 前缀查询排在 `openrouter/openai/gpt-5` 这类代理 ID 之前）。

### 2.10 `model-catalog-refresh.ts`（51 行）

```ts
type ModelCatalogRuntime = Pick<ModelRuntime, "refresh">;
interface ActiveModelCatalogRefresh { controller: AbortController; promise: Promise<ModelsRefreshResult>; waiters: number; }
class ModelCatalogRefreshCoordinator {
  private readonly activeByRuntime = new WeakMap<ModelCatalogRuntime, ActiveModelCatalogRefresh>();
  refresh(modelRuntime, signal): Promise<ModelsRefreshResult>;
}
export function refreshModelCatalogs(modelRuntime: ModelCatalogRuntime, signal: AbortSignal): Promise<ModelsRefreshResult>;
```

`refresh` 逐字语义：
```
signal.throwIfAborted()
active = activeByRuntime.get(modelRuntime)
if (!active) {
  controller = new AbortController()
  operation = modelRuntime.refresh({ signal: controller.signal })
  promise = raceWithAbortSignal(operation, controller.signal).finally(() => {
    if (activeByRuntime.get(modelRuntime) === created) activeByRuntime.delete(modelRuntime)
  })
  created = { controller, promise, waiters: 0 }
  active = created; activeByRuntime.set(modelRuntime, active)
}
active.waiters++
return raceWithAbortSignal(active.promise, signal).finally(() => {
  active.waiters--
  if (active.waiters === 0 && activeByRuntime.get(modelRuntime) === active) active.controller.abort()
})
```
- 同一 `modelRuntime` 上的并发调用共享一次底层 `refresh`。
- 每个调用者的取消互相独立（`raceWithAbortSignal`）。
- **最后一个等待者离开时中止共享刷新**，这样下一次调用会新建一次。
- `raceWithAbortSignal` 来自 `ca-cli` 的 `utils/abort.ts`。

测试（`model-catalog-refresh.test.ts`）：
- `shares one runtime refresh between concurrent callers`
- `keeps the shared refresh alive when one caller stops waiting`
- `aborts an abandoned refresh and allows a later refresh to start`

### 2.11 `program-status-reporter.ts`（111 行）

```ts
export type BlockedStatus = { kind: NonNullable<ProgramStatus["kind"]>; message: string };
export class ProgramStatusReporter {
  constructor(getTerminal: () => Terminal, getSessionName: () => string | undefined);
  handleEvent(event: AgentSessionEvent): void;
  setBlocked(source: string, status: BlockedStatus | undefined): void;
  reset(): void;
  report(): void;
}
function firstLine(text: string | undefined): string {
  return text?.split(/\r?\n/, 1)[0]?.trim() || "Error";
}
```

字段：`runActive=false`、`compacting=false`、`runResult={state:"done"}`、`restingStatus={state:"idle"}`、`blocked: Map<string,BlockedStatus>`（插入顺序）、`lastReport: string | undefined`。

`handleEvent(event)` 的 switch：
- `agent_start` → `runActive = true; runResult = {state:"done"}`。
- `message_end` → 若 `event.message.role !== "assistant"` 直接 return（**不 report**）；否则 `runResult = stopReason === "error" ? {state:"error", message: firstLine(errorMessage)} : {state:"done"}`。
- `compaction_start` → `compacting = true`。
- `compaction_end` → `compacting = false`；若 `runActive`：`aborted` → `runResult={state:"idle"}`；否则 `errorMessage` → `runResult={state:"error", message:firstLine(...)}`；若 `!runActive`：`aborted` → `restingStatus={state:"idle"}`；否则 `reason === "manual"` → `restingStatus = errorMessage ? {state:"error",...} : {state:"done"}`。
- `agent_settled` → `runActive = false; restingStatus = event.aborted ? {state:"idle"} : runResult`。
- `session_info_changed` → **break**（后面仍会 report，因为会话名是 working/done 消息的一部分）。
- default → return（不 report）。

之后统一 `this.report()`。

`setBlocked(source, status)`：`blocked.delete(source)`；若有 status 则 `blocked.set(source, status)`（**重新插入到末尾**）；`report()`。

`reset()`：全字段复位（runActive=false、compacting=false、runResult=done、restingStatus=idle），`report()`。

`report()`：
```
const status = { ...this.currentStatus(), app: APP_NAME }
const key = JSON.stringify(status)
if (key === this.lastReport) return
this.lastReport = key
this.getTerminal().setProgramStatus(status)
```

`currentStatus()`：
```
const blocked = [...this.blocked.values()].at(-1)
if (blocked) return { state: "blocked", ...blocked }
if (this.compacting) return { state: "working", message: "Compacting context" }   // 【逐字】
const status = this.runActive ? { state: "working" } : this.restingStatus
if (status.state === "working" || status.state === "done") return { ...status, message: this.getSessionName() }
return status
```
**注意**：
- 只有 `working`/`done` 附带会话名（可能 `undefined` → JSON 序列化时省略该 key）。
- `error` 不附带会话名。
- `JSON.stringify` 的 key 顺序：`state` 优先（spread 顺序），`message` 次之，`app` 始终最后（`{...current, app}`）。

测试（`program-status-reporter.test.ts`）：
- `reports idle, working during a run, and done once it settles`
- `reports only the outcome of the run: retried errors, final errors, and aborts`
- `reports a failed recovery compaction as the run's error unless a later response succeeds`
- `reports compaction inside a run and the result of a manual compaction`
- `reports the most recent open dialog and the underlying state once all close`
- `sends each status once and follows session name changes`
- `returns to idle when the session is replaced`

### 2.12 `tui-renderer.ts`（81 行）

```ts
export interface InteractiveTuiOptions {
  readonly tuiMode: "regular" | "fullscreen";
  readonly showHardwareCursor: boolean;
  readonly logDirectory: string;
  readonly terminal?: Terminal;
  readonly onRightClickPaste?: () => void;
  readonly fullscreenCopyOnSelect?: boolean;
  readonly fullscreenWheelScrollLines?: WheelScrollLines;
}
export function createInteractiveTui(options: InteractiveTuiOptions & {tuiMode:"fullscreen"}): TuiAltScreen;
export function createInteractiveTui(options: InteractiveTuiOptions & {tuiMode:"regular"}): TuiMainScreen;
export function createInteractiveTui(options: InteractiveTuiOptions): TuiMainScreen | TuiAltScreen;
export function createInteractiveTuiReference(getTui: () => TUI): TUI;
```

**fullscreen 分支**（`new TuiAltScreen(terminal, showHardwareCursor, logDirectory, opts)`）：
```ts
const styleSearchMatch = (text) => theme.bg("searchMatchBg", theme.fg("searchMatchText", text));
{
  searchMatchStyle: (text) => theme.underline(styleSearchMatch(text)),
  searchCurrentMatchStyle: (text) => theme.bold(theme.inverse(styleSearchMatch(text))),
  searchNavigationButtonStyle: (text, hovered) => hovered ? theme.underline(text) : text,
  scrollToEndIndicator: () => {
    const shortcut = keyDisplayText("tui.altScreen.bottom");
    const label = ` ↓ Jump to latest message${shortcut ? ` · ${shortcut}` : ""} `;   // 【逐字】前后各一个空格，前导 ↓
    return theme.bg("selectedBg", theme.fg("text", label));
  },
  openUrl: openBrowser,
  onRightClickPaste: options.onRightClickPaste,
  copyOnSelect: options.fullscreenCopyOnSelect,
  wheelScrollLines: options.fullscreenWheelScrollLines ?? "auto",
  copySelection: async (text) => {
    try { await copyToClipboard(text); return true }
    catch (error) { return error instanceof Error ? error.message : String(error) }
  },
}
```
`copySelection` 返回 `true` 表示成功，返回 `string` 表示失败消息。

**regular 分支** → `new TuiMainScreen(terminal, showHardwareCursor, logDirectory)`。

**`createInteractiveTuiReference`**：`Proxy` 转发 get/set/has/getPrototypeOf 到 `getTui()`；方法包装带**缓存失效检查**：若当前 tui 与上次不同，重新取方法；若新方法不是函数 → 抛 `TypeError: TUI property ${String(property)} is not callable`。**【逐字】**

### 2.13 `components/keybinding-hints.ts`（48 行）

```ts
export interface KeyTextFormatOptions { capitalize?: boolean }
export function formatKeyText(key: string, options?: KeyTextFormatOptions): string;
export function keyText(keybinding: Keybinding): string;
export function keyDisplayText(keybinding: Keybinding): string;
export function keyHint(keybinding: Keybinding, description: string): string;
export function rawKeyHint(key: string, description: string): string;
```

- `formatKeyPart(part, options)`：darwin 上 `alt` → `option`（大小写不敏感比较）；`capitalize` 时首字母大写。
- `formatKeyText(key, options)`：按 `/` 分组（一个 keybinding 可有多个绑定），每组按 `+` 分部件，逐部件 `formatKeyPart`，再用 `+` 拼回，最后用 `/` 拼。
- `formatKeys(keys: KeyId[], options)`（内部）：空 → `""`；否则 `formatKeyText(keys.join("/"), options)`。
- `keyText(kb)` = `formatKeys(getKeybindings().getKeys(kb))`（**不**大写）。
- `keyDisplayText(kb)` = `formatKeys(..., {capitalize:true})`。
- `keyHint(kb, desc)` = `theme.fg("dim", keyText(kb)) + theme.fg("muted", " " + desc)`。
- `rawKeyHint(key, desc)` = `theme.fg("dim", formatKeyText(key)) + theme.fg("muted", " " + desc)`。

### 2.14 `components/kiana-logo.ts`（40 行）

```ts
const CORAL = rgbColor(228,138,122);
const BLUE  = rgbColor(79,142,179);
const YELLOW= rgbColor(234,182,93);
const RESET = "\x1b[0m";
export function kianaLogoLines(): [string, string];
export function supportsKianaLogo(): boolean;
export function kianaWordmark(): string;
```

```
kianaLogoLines():
  mode = theme.getColorMode(); fg = (c) => foregroundAnsi(c, mode)
  top    = `${fg(CORAL)}${backgroundAnsi(BLUE, mode)}▀${RESET}${fg(CORAL)}▀█${RESET} `    // 末尾一个空格
  bottom = `${fg(BLUE)}█▀${RESET} ${fg(YELLOW)}█${RESET}`
  return [top, bottom]
supportsKianaLogo() = !isAppleTerminalSession()
kianaWordmark() = `${foregroundAnsi(CORAL,mode)}P${RESET}${foregroundAnsi(YELLOW,mode)}i${RESET}`
```
图案（4 格宽 × 2 行，每格上下两个半块像素）：
```
coral coral coral .
blue  .     coral .
blue  blue  .     yellow
blue  .     .     yellow
```
（上行第四格为空，因此用空格补齐到与下行同宽。）

### 2.15 `components/dynamic-border.ts`（25 行）

```ts
export class DynamicBorder implements Component {
  constructor(color: (str: string) => string = (str) => theme.fg("border", str));
  invalidate(): void;                    // no-op
  render(width: number): string[];       // [this.color("─".repeat(Math.max(1, width)))]
}
```
注释提醒：扩展经 jiti 加载时全局 `theme` 可能 undefined，因此给扩展用的组件**必须显式传 color 函数**。

### 2.16 `components/themed-text.ts`（32 行）

```ts
export class ThemedText extends Text {
  private readonly build: () => string;
  private stale = true;
  constructor(build: () => string, paddingX = 1, paddingY = 1);
  override invalidate(): void { super.invalidate(); this.stale = true }
  override render(width: number): string[] {
    if (this.stale) { this.stale = false; this.setText(this.build()) }
    return super.render(width)
  }
}
```
测试（`themed-text.test.ts`）：`builds lazily and rebuilds with the current theme after invalidation` —— 构造时不调用 `build`，首次 `render` 调用一次，`invalidate()` 后下一次 render 再调用。

### 2.17 `components/markdown-transform.ts`（35 行）

```ts
export function createMarkdownTransform(
  messageType: MarkdownTransformContext["messageType"],
  isStreaming: boolean,
  transformers: readonly MarkdownTransformer[],
  messageId?: string,
  timestamp?: string,
): (markdown: string, availableWidth: number) => string;
```
返回的函数调用 `applyMarkdownTransformers(markdown, {messageType, isStreaming, availableWidth, messageId, timestamp}, transformers)`：
```
let out = markdown
for (t of transformers) {
  try { const r = t(out, context); if (typeof r === "string") out = r }
  catch { /* 保留当前 markdown，继续下一个 */ }
}
return out
```
`context` 的 key 顺序：`messageType, isStreaming, availableWidth, messageId, timestamp`。

---


### 2.18 `components/index.ts`（37 行）

```ts
// UI Components for extensions
export { ArminComponent } from "./armin.ts";
export { AssistantMessageComponent } from "./assistant-message.ts";
export { BashExecutionComponent } from "./bash-execution.ts";
export { BorderedLoader } from "./bordered-loader.ts";
export { BranchSummaryMessageComponent } from "./branch-summary-message.ts";
export { CompactionSummaryMessageComponent } from "./compaction-summary-message.ts";
export { CustomEditor, type CustomEditorOptions } from "./custom-editor.ts";
export { CustomMessageComponent } from "./custom-message.ts";
export { type RenderDiffOptions, renderDiff } from "./diff.ts";
export { DynamicBorder } from "./dynamic-border.ts";
export { ExtensionEditorComponent } from "./extension-editor.ts";
export { ExtensionInputComponent } from "./extension-input.ts";
export { ExtensionSelectorComponent } from "./extension-selector.ts";
export { FirstTimeSetupComponent, type FirstTimeSetupOptions, type FirstTimeSetupResult } from "./first-time-setup.ts";
export { FooterComponent } from "./footer.ts";
export { keyHint, keyText, rawKeyHint } from "./keybinding-hints.ts";
export { LoginDialogComponent } from "./login-dialog.ts";
export { ModelSelectorComponent } from "./model-selector.ts";
export { OAuthSelectorComponent } from "./oauth-selector.ts";
export { type ModelsCallbacks, type ModelsConfig, ScopedModelsSelectorComponent } from "./scoped-models-selector.ts";
export { SessionSelectorComponent } from "./session-selector.ts";
export { type SettingsCallbacks, type SettingsConfig, SettingsSelectorComponent } from "./settings-selector.ts";
export { ShowImagesSelectorComponent } from "./show-images-selector.ts";
export { SkillInvocationMessageComponent } from "./skill-invocation-message.ts";
export { ThemeSelectorComponent } from "./theme-selector.ts";
export { ThinkingSelectorComponent } from "./thinking-selector.ts";
export { ToolExecutionComponent, type ToolExecutionOptions } from "./tool-execution.ts";
export { TreeSelectorComponent } from "./tree-selector.ts";
export { TrustSelectorComponent } from "./trust-selector.ts";
export { UserMessageComponent } from "./user-message.ts";
export { UserMessageSelectorComponent } from "./user-message-selector.ts";
export { truncateToVisualLines, type VisualTruncateResult } from "./visual-truncate.ts";
```
**注意**：`ConfigSelectorComponent`（config-selector.ts）、`Attachment`/`ArminComponent` 之外的若干组件**未**从 index 导出；`SessionSelectorComponent` 等在 `interactive-mode.ts` 中直接按路径导入。

### 2.19 `components/assistant-message.ts`（216 行）

```ts
const OSC133_ZONE_START = "\x1b]133;A\x07";
const OSC133_ZONE_END   = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL = "\x1b]133;C\x07";

export class AssistantMessageComponent extends Container {
  constructor(
    message?: AssistantMessage,
    hideThinkingBlock = false,
    markdownTheme: MarkdownTheme = getMarkdownTheme(),
    hiddenThinkingLabel = "Thinking...",
    outputPad = 1,
    markdownTransformers: readonly MarkdownTransformer[] = [],
    messageId?: string,
    timestamp?: string,
  );
  override invalidate(): void;                 // super.invalidate() 后若有 lastMessage 则 updateContent(lastMessage)
  setHideThinkingBlock(hide: boolean): void;    // 清空 thinkingVisibilityOverrides，并重建
  setHiddenThinkingLabel(label: string): void;
  setOutputPad(padding: number): void;
  override render(width: number): string[];
  updateContent(message: AssistantMessage, isStreaming = this.isStreaming): void;
}
```

内部字段：`contentContainer: Container`（唯一子节点）、`lastMessage?`、`hasToolCalls`、`isStreaming`、`messageId?`、`timestamp?`、`thinkingVisibilityOverrides = new Map<number, boolean>()`。

**`render(width)`**：
```
lines = super.render(width)
if (this.hasToolCalls || lines.length === 0) return lines
lines[0] = OSC133_ZONE_START + lines[0]
lines[lines.length-1] = OSC133_ZONE_END + OSC133_ZONE_FINAL + lines[lines.length-1]
return lines
```
即：**没有 toolCall 的助手消息**被 OSC 133 的 A/B/C 标记包裹以支持 shell 集成；有 toolCall 时**不加**（因为工具块会拆开消息）。

**`updateContent(message, isStreaming)`** 逐条：

1. 保存 `lastMessage`、`isStreaming`；`contentContainer.clear()`。
2. `hasVisibleContent = message.content.some(c => (c.type==="text" && c.text.trim()) || (c.type==="thinking" && c.thinking.trim()))`；为真则 `addChild(new Spacer(1))`。
3. 遍历 `message.content`（索引 i，手动递增）：
   - `type === "text"` 且 `text.trim()` 非空 → `new Markdown(text.trim(), outputPad, 0, markdownTheme, undefined, { transform: createMarkdownTransform("assistant", isStreaming, transformers, messageId, timestamp) })`。（paddingY = 0，避免工具块前的多余空行；文本被 trim。）
   - `type === "thinking"` → 连续收集所有后续 thinking 块（内层循环前进 i，遇到非 thinking 就 `break` 并 `i--`）；每个块的 `thinking.trim()` 非空才入 `thinkingBlocks`；全空则 `continue`。
     - `hasVisibleContentAfter` = 剩余内容（`slice(i+1)`）中是否有可见 text/thinking。
     - `runIndex = thinkingRunIndex++`；`hidden = thinkingVisibilityOverrides.get(runIndex) ?? hideThinkingBlock`。
     - hidden → `new Text(theme.italic(theme.fg("thinkingText", hiddenThinkingLabel)), outputPad, 0)`。
     - 非 hidden → `new Markdown(thinkingBlocks.join("\n\n"), outputPad, 0, markdownTheme, {color: t => theme.fg("thinkingText", t), italic: true}, {transform: createMarkdownTransform("assistant-thinking", isStreaming, transformers, messageId, timestamp)})`。
     - 包在 `MouseRegion(component, event => { if (event.type!=="click" || event.button!=="left") return undefined; this.thinkingVisibilityOverrides.set(runIndex, !hidden); if (this.lastMessage) this.updateContent(this.lastMessage); return {handled:true} })`。
     - `hasVisibleContentAfter` → `addChild(new Spacer(1))`。
4. `hasToolCalls = message.content.some(c => c.type === "toolCall")`；存字段。
5. `stopReason === "length"` → Spacer(1) + `Text(theme.fg("error", "Response was truncated before completion."), outputPad, 0)` **【逐字】**
   否则若 `!hasToolCalls`：
   - `stopReason === "aborted"` → `abortMessage = (errorMessage && errorMessage !== "Request was aborted") ? errorMessage : "Operation aborted"`；Spacer(1) + `Text(theme.fg("error", abortMessage), outputPad, 0)`。**【逐字】** `Operation aborted` 与特殊豁免字符串 `Request was aborted`。
   - `stopReason === "error"` → `errorMsg = errorMessage || "Unknown error"`；Spacer(1) + `Text(theme.fg("error", `Error: ${errorMsg}`), outputPad, 0)`。**【逐字】**

**测试**（`assistant-message.test.ts`）：
- `adds OSC 133 zone markers to assistant messages without tool calls`
- `does not add OSC 133 zone markers when assistant message contains tool calls`
- `renders length stops with neutral truncation wording`
- `coalesces adjacent thinking blocks into one hidden thinking label`
- `collapses individual thinking runs when clicked`
- `uses configured output padding for text and thinking`
- `chains Markdown transformers in registration order`
- `identifies partial assistant Markdown as streaming`
- `reapplies Markdown transformers when available width changes`
- `continues the Markdown transformer chain when a transformer throws`
- `transforms text and thinking Markdown without mutating the original message`
- `uses configured output padding for user messages`

### 2.20 `components/user-message.ts`（83 行）

```ts
export class UserMessageComponent extends Container {
  constructor(text: string, markdownTheme = getMarkdownTheme(), outputPad = 1,
              markdownTransformers: readonly MarkdownTransformer[] = [], messageId?: string, timestamp?: string);
  setOutputPad(padding: number): void;   // rebuild
  override render(width: number): string[];  // OSC 133 包裹（只要 lines 非空）
}
```
`rebuild()`：
```ts
this.clear()
this.addChild(new Markdown(
  this.text, this.outputPad, 1, this.markdownTheme,
  { color: (c) => theme.fg("userMessageText", c), bgColor: (c) => theme.bg("userMessageBg", c) },
  { preserveOrderedListMarkers: true, preserveBackslashEscapes: true,
    transform: createMarkdownTransform("user", false, this.markdownTransformers, this.messageId, this.timestamp) },
))
```
- **paddingY = 1**（对比助手文本的 0）。
- `isStreaming` 恒为 `false`（用户消息不会流式）。
- 注释：不在外层套 Box，因为 Markdown 自己 pad + 上背景，Box 会保留一份整行副本。
- `render`：`lines.length === 0` 直接返回；否则同 assistant 的 OSC 133 包裹（**无条件**加，因为没有 toolCall 概念）。

测试（`user-message.test.ts`）：`keeps user message height stable while moving closing OSC markers off line end`、`chains Markdown transformers with user message context`、`reapplies Markdown transformers when invalidated`、`passes persisted entry identity in transformer context; live leaves it undefined`。

### 2.21 `components/bash-execution.ts`（223 行）

```ts
const PREVIEW_LINES = 20;
export class BashExecutionComponent extends Container {
  constructor(command: string, ui: TUI, excludeFromContext = false, outputPad = 1);
  setExpanded(expanded: boolean): void;
  setOutputPad(outputPad: number): void;
  override invalidate(): void;
  appendOutput(chunk: string): void;
  setComplete(exitCode: number | undefined, cancelled: boolean,
              truncationResult?: TruncationResult, fullOutputPath?: string): void;
  getOutput(): string;
  getCommand(): string;
}
```
字段：`command`、`outputLines: string[]`、`status: "running"|"complete"|"cancelled"|"error"`、`exitCode`、`loader: Loader`、`truncationResult?`、`fullOutputPath?`、`expanded=false`、`contentContainer`、`colorKey: "dim"|"bashMode"`（`excludeFromContext` 时 `"dim"`，注释：`dim` marks `!!` commands, whose output is excluded from the model context）、`outputPad`。

**构造**：`Spacer(1)` → `DynamicBorder(borderColor)` → `contentContainer` → `Loader(ui, spinner => theme.fg(colorKey, spinner), text => theme.fg("muted", text), `Running... (${keyText("tui.select.cancel")} to cancel)`)` → `DynamicBorder(borderColor)`；最后 `updateDisplay()`。

**`appendOutput(chunk)`**
```
clean = stripAnsi(chunk).replace(/\r\n/g,"\n").replace(/\r/g,"\n")
newLines = clean.split("\n")
if (this.outputLines.length > 0 && newLines.length > 0) {
  this.outputLines[last] += newLines[0]
  this.outputLines.push(...newLines.slice(1))
} else this.outputLines.push(...newLines)
this.updateDisplay()
```
（未完成行续接到上一行。）

**`setComplete(exitCode, cancelled, truncationResult?, fullOutputPath?)`**
```
this.exitCode = exitCode
this.status = cancelled ? "cancelled"
  : (exitCode !== 0 && exitCode !== undefined && exitCode !== null) ? "error" : "complete"
this.truncationResult = truncationResult; this.fullOutputPath = fullOutputPath
this.loader.stop(); this.updateDisplay()
```

**`updateDisplay()`**
```
fullOutput = outputLines.join("\n")
contextTruncation = truncateTail(fullOutput, { maxLines: DEFAULT_MAX_LINES, maxBytes: DEFAULT_MAX_BYTES })
availableLines = contextTruncation.content ? contextTruncation.content.split("\n") : []
previewLogicalLines = availableLines.slice(-20)
hiddenLineCount = availableLines.length - previewLogicalLines.length
contentContainer.clear()
// header
new Text(theme.fg(colorKey, theme.bold(`$ ${command}`)), outputPad, 0)
// output
if (availableLines.length > 0) {
  if (expanded) new Text(`\n` + availableLines.map(l => theme.fg("muted", l)).join("\n"), outputPad, 0)
  else 一个匿名组件：按 width 缓存 truncateToVisualLines(`\n`+styled, PREVIEW_LINES, width, outputPad)
}
// 状态区
if (status === "running") addChild(loader)
else {
  statusParts = []
  if (hiddenLineCount > 0) {
    expanded ? `${fg(muted,"(")}${keyHint("app.tools.expand","to collapse")}${fg(muted,")")}`
             : `${fg(muted, `... ${hiddenLineCount} more lines (`)}${keyHint("app.tools.expand","to expand")}${fg(muted,")")}`
  }
  if (status === "cancelled") statusParts.push(theme.fg("warning", "(cancelled)"))
  else if (status === "error") statusParts.push(theme.fg("error", `(exit ${exitCode})`))
  wasTruncated = truncationResult?.truncated || contextTruncation.truncated
  if (wasTruncated && fullOutputPath) statusParts.push(theme.fg("warning", `Output truncated. Full output: ${fullOutputPath}`))
  if (statusParts.length > 0) addChild(new Text("\n" + statusParts.join("\n"), outputPad, 0))
}
```
**【逐字】** 文案：`$ ${command}`、`... N more lines (`、`to expand`、`to collapse`、`(cancelled)`、`(exit N)`、`Output truncated. Full output: ${path}`、`Running... (${key} to cancel)`。

`excludeFromContext=true` 时**所有**颜色（边框、`$` 头部、spinner）用 `dim`；输出正文仍是 `muted`。

测试（`bash-execution-width.test.ts`）：`collapsed preview lines respect render-time width, not construction-time width`、`re-computes lines when width changes between renders` —— 说明**不能**在构造时固化宽度，必须按 render 宽度缓存。

### 2.22 `components/bordered-loader.ts`（68 行）

```ts
export class BorderedLoader extends Container {
  constructor(tui: TUI, theme: Theme, message: string, options?: {cancellable?: boolean});
  get signal(): AbortSignal;
  set onAbort(fn: (() => void) | undefined);
  handleInput(data: string): void;
  dispose(): void;
}
```
布局：`DynamicBorder(theme.fg("border"))` → （cancellable ? `CancellableLoader(tui, accent, muted, message)` : `Loader(...)` 且自建 `AbortController`）→ cancellable 时 `Spacer(1)` + `Text(keyHint("tui.select.cancel","cancel"),1,0)` → `Spacer(1)` → `DynamicBorder`。
`signal`：cancellable → loader.signal；否则 `signalController?.signal ?? new AbortController().signal`。
`dispose`：loader 有 `dispose` 调 dispose，否则有 `stop` 调 stop。

### 2.23 `components/branch-summary-message.ts` / `compaction-summary-message.ts` / `skill-invocation-message.ts`

三者结构相同（`Box` 子类，`paddingY=outputPad`、`paddingX=1`、背景 `theme.bg("customMessageBg")`，整块包在 `MouseRegion` 里左键点击切换 `expanded`）。

**BranchSummaryMessageComponent**
```ts
constructor(message: BranchSummaryMessage, markdownTheme = getMarkdownTheme(), outputPad = 1)
setExpanded(b); setOutputPad(p) { this.setPaddingX(p) }; invalidate()
```
`updateDisplay()`：
```
clear(); content = new Container()
label = theme.fg("customMessageLabel", `\x1b[1m[branch]\x1b[22m`)   // 【逐字】含手写 SGR
content.addChild(new Text(label, 0, 0)); content.addChild(new Spacer(1))
expanded:
  header = "**Branch Summary**\n\n"
  Markdown(header + message.summary, 0, 0, markdownTheme, { color: t => theme.fg("customMessageText", t) })
else:
  Text(theme.fg("customMessageText","Branch summary (") + theme.fg("dim", keyText("app.tools.expand"))
       + theme.fg("customMessageText"," to expand)"), 0, 0)
addChild(MouseRegion(content, click => { setExpanded(!expanded); return {handled:true} }))
```

**CompactionSummaryMessageComponent**：同上，但
- `tokenStr = message.tokensBefore.toLocaleString()`
- label `\x1b[1m[compaction]\x1b[22m`
- 展开 header `` `**Compacted from ${tokenStr} tokens**\n\n` ``
- 折叠 `` `Compacted from ${tokenStr} tokens (` + dim[keyText(app.tools.expand)] + ` to expand)` ``

**SkillInvocationMessageComponent**
```ts
constructor(skillBlock: ParsedSkillBlock, markdownTheme = getMarkdownTheme(), outputPad = 1)
```
- 展开：label `\x1b[1m[skill]\x1b[22m` + `` `**${skillBlock.name}**\n\n` `` + Markdown(content)。
- 折叠：单行 `` `\x1b[1m[skill]\x1b[22m ` + customMessageText(name) + dim(` (${keyText("app.tools.expand")} to expand)`) ``。
- 注释：**只渲染 skill 块**；用户消息由 `UserMessageComponent` 单独渲染。

测试（`collapsible-message-components.test.ts`）：`toggles a compaction summary when clicked` / `toggles a branch summary when clicked` / `toggles a skill invocation when clicked`。

### 2.24 `components/countdown-timer.ts`（39 行）

```ts
export class CountdownTimer {
  constructor(timeoutMs: number, tui: TUI | undefined,
              onTick: (seconds: number) => void, onExpire: () => void);
  dispose(): void;
}
```
```
remainingSeconds = Math.ceil(timeoutMs / 1000)
onTick(remainingSeconds)                       // 立刻先 tick 一次
intervalId = setInterval(() => {
  remainingSeconds--
  onTick(remainingSeconds)
  tui?.requestRender()
  if (remainingSeconds <= 0) { dispose(); onExpire() }
}, 1000)
```
`dispose()` 幂等（`intervalId` 置 undefined）。

### 2.25 `components/custom-editor.ts`（148 行）

```ts
export type CustomEditorOptions = EditorOptions & { embedWorkingStatus?: boolean };
export class CustomEditor extends Editor {
  readonly embedWorkingStatus: boolean;
  actionHandlers: Map<AppKeybinding, () => void>;
  onEscape?: () => void;
  onCtrlD?: () => void;
  onPasteImage?: () => void;
  onExtensionShortcut?: (data: string) => boolean;
  constructor(tui: TUI, theme: EditorTheme, keybindings: KeybindingsManager, options?: CustomEditorOptions);
  setWorkingStatusIndicator(indicator: StatusIndicator | undefined): void;
  protected override renderTopBorder(width: number, hiddenLineCount: number): string;
  onAction(action: AppKeybinding, handler: () => void): void;
  handleInput(data: string): void;
}
```

**`renderTopBorder(width, hiddenLineCount)`**（把 spinner 嵌进编辑器上边框；`embedWorkingStatus` 为 false、无 indicator、或 `width <= 0` → `super.renderTopBorder`）：
```
status = indicator.renderInBorder(Math.max(1, width - 5))
statusWidth = visibleWidth(status)
if (statusWidth === 0) return super.renderTopBorder(width, hiddenLineCount)
overflowLabel = hiddenLineCount > 0 ? ` ↑ ${hiddenLineCount} more ` : undefined
overflowLabelWidth = overflowLabel ? visibleWidth(overflowLabel) : 0
overflowStart = Math.floor((width - overflowLabelWidth) / 2)
canFitOverflow = () => overflowLabel !== undefined && overflowLabelWidth + 2 <= width
                   && overflowStart - (3 + statusWidth + 1) >= 1
if (overflowLabel && !canFitOverflow()) { status = indicator.renderSpinnerInBorder(width); statusWidth = visibleWidth(status) }
if (canFitOverflow()) {
  leftBlockWidth = 3 + statusWidth + 1
  return borderColor("── ") + status
       + borderColor(` ${"─".repeat(overflowStart - leftBlockWidth)}${overflowLabel}${"─".repeat(width - overflowStart - overflowLabelWidth)}`)
}
if (width >= statusWidth + 5)
  return borderColor("── ") + status + borderColor(` ${"─".repeat(width - statusWidth - 4)}`)
status = indicator.renderSpinnerInBorder(width); statusWidth = visibleWidth(status)
prefixWidth = Math.min(3, Math.max(0, width - statusWidth))
return borderColor("─".repeat(prefixWidth)) + status + borderColor("─".repeat(Math.max(0, width - prefixWidth - statusWidth)))
```
**【逐字】** `" ↑ ${hiddenLineCount} more "`（前后各一个空格）。

**`handleInput(data)`（顺序即优先级）**
1. `onExtensionShortcut?.(data)` 返回 true → return。
2. `keybindings.matches(data, "app.clipboard.pasteImage")` → `onPasteImage?.()`；return。
3. `keybindings.matches(data, "app.interrupt")`：若**不在** autocomplete 中（`!isShowingAutocomplete()`），取 `onEscape ?? actionHandlers.get("app.interrupt")`，有则调用并 return；否则落到 `super.handleInput(data)`（让父类处理 autocomplete 取消）后 return。
4. `keybindings.matches(data, "app.exit")`：文本为空 → `onCtrlD ?? actionHandlers.get("app.exit")` 存在则调用并 return；**非空则贯穿到后续处理**（让 Ctrl+D 表现为 delete-char-forward）。注释：`Fall through to editor handling for delete-char-forward when not empty`。
5. `keybindings.matches(data, "tui.editor.historyPrevious") || matches("tui.editor.historyNext")` → `super.handleInput(data)`；return。（显式历史绑定优先于应用动作，因此可以绑 Ctrl+P 而不触发模型轮换。）
6. 遍历 `actionHandlers`（插入顺序），跳过 `app.interrupt`/`app.exit`，第一个 `matches` 命中的 handler 调用并 return。
7. `super.handleInput(data)`。

测试（`custom-editor-history-keybindings.test.ts`）：`gives an explicit history binding precedence over model cycling`。

### 2.26 `components/custom-entry.ts`（69 行）

```ts
export class CustomEntryComponent extends Container {
  constructor(entry: CustomEntry<unknown>, renderer: EntryRenderer, outputPad = 1);
  hasContent(): boolean;
  setExpanded(expanded: boolean): void;   // 仅在变化时 rebuild
  setOutputPad(outputPad: number): void;  // 无条件 rebuild
  override invalidate(): void;
}
```
`rebuild()`：
```
clear(); customComponent = undefined
try { component = renderer(entry, { expanded: this._expanded }, theme) }
catch (error) {
  message = error instanceof Error ? error.message : String(error)
  box = new Box(outputPad, 1, t => theme.bg("customMessageBg", t))
  box.addChild(new Text(theme.fg("error", `[${entry.customType}] renderer failed: ${message}`), 0, 0))
  component = box
}
if (!component) return
customComponent = component
addChild(new Spacer(1)); addChild(component)
```
**【逐字】** `[${entry.customType}] renderer failed: ${message}`。
注释：宿主负责 transcript 间距，renderer 只提供内容。

### 2.27 `components/custom-message.ts`（114 行）

```ts
export class CustomMessageComponent extends Container {
  constructor(message: CustomMessage<unknown>, customRenderer?: MessageRenderer,
              markdownTheme = getMarkdownTheme(), outputPad = 1);
  setExpanded(expanded: boolean): void;   // 变化时 rebuild
  setOutputPad(outputPad: number): void;  // 变化时 rebuild
  override invalidate(): void;
}
```
构造：`addChild(new Spacer(1))`；`box = new Box(1, 1, t => theme.bg("customMessageBg", t))`；`rebuild()`。

`rebuild()`：
```
if (customComponent) { removeChild(customComponent); customComponent = undefined }
removeChild(box)
if (customRenderer) {
  try {
    component = customRenderer(message, { expanded: _expanded, outputPad: this.outputPad }, theme)
    if (component) { customComponent = component; addChild(component); return }
  } catch { /* 落到默认渲染 */ }
}
addChild(box); box.clear(); box.setPaddingX(outputPad)
label = theme.fg("customMessageLabel", `\x1b[1m[${message.customType}]\x1b[22m`)
box.addChild(new Text(label,0,0)); box.addChild(new Spacer(1))
text = typeof content === "string" ? content : content.filter(c => c.type==="text").map(c => c.text).join("\n")
box.addChild(new Markdown(text, 0, 0, markdownTheme, { color: t => theme.fg("customMessageText", t) }))
```
测试（`custom-message.test.ts`）：`provides output padding to custom renderers and updates it` —— renderer 收到 `{expanded, outputPad}`。

### 2.28 `components/diff.ts`（147 行）

```ts
export interface RenderDiffOptions { filePath?: string }   // 未使用，仅为 API 兼容
export function renderDiff(diffText: string, _options?: RenderDiffOptions): string;
```

内部：
- `parseDiffLine(line)`：`/^([+-\s])(\s*\d*)\s(.*)$/` → `{prefix, lineNum, content}`；不匹配 → `null`。
- `replaceTabs(text)`：`\t` → 三个空格。
- `renderIntraLineDiff(oldContent, newContent)`：`Diff.diffWords(old, new)`（`diff` 包）；对第一个 removed/added part 剥掉前导空白（未高亮缩进）；removed/added 的其余部分用 `theme.inverse(...)`；未变部分两边都原样追加。

`renderDiff` 主循环：
```
lines = diffText.split("\n"); i = 0
while (i < lines.length):
  parsed = parseDiffLine(lines[i])
  if (!parsed) { result.push(theme.fg("toolDiffContext", line)); i++; continue }
  if (prefix === "-") {
    收集连续 "-" 行 → removedLines（推进 i）
    收集连续 "+" 行 → addedLines（推进 i）
    if (removedLines.length === 1 && addedLines.length === 1) {
      {removedLine, addedLine} = renderIntraLineDiff(replaceTabs(removed.content), replaceTabs(added.content))
      push fg(toolDiffRemoved, `-${removed.lineNum} ${removedLine}`)
      push fg(toolDiffAdded,   `+${added.lineNum} ${addedLine}`)
    } else {
      先所有 removed：fg(toolDiffRemoved, `-${lineNum} ${replaceTabs(content)}`)
      再所有 added：  fg(toolDiffAdded,   `+${lineNum} ${replaceTabs(content)}`)
    }
  } else if (prefix === "+") { push fg(toolDiffAdded, `+${lineNum} ${replaceTabs(content)}`); i++ }
  else { push fg(toolDiffContext, ` ${lineNum} ${replaceTabs(content)}`); i++ }
return result.join("\n")
```
注意上下文行前缀是**一个空格** + lineNum + 空格（原本 `prefix` 就是空格，被替换为字面空格）。

### 2.29 `components/earendil-announcement.ts`（53 行）

```ts
const BLOG_URL = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";
const IMAGE_FILENAME = "clankolas.png";
export class EarendilAnnouncementComponent extends Container { constructor(); }
```
`loadImageBase64()`：模块级 `cachedImageBase64` + `attemptedImageLoad` 双标志；首次调用 `fs.readFileSync(getBundledInteractiveAssetPath("clankolas.png")).toString("base64")`；异常 → `undefined`（此后不再重试）。

布局：
```
DynamicBorder(accent)
Text(theme.bold(theme.fg("accent", "kiana has joined Earendil")), 1, 0)         // 【逐字】
Spacer(1)
Text(theme.fg("muted", "Read the blog post:"), 1, 0)                          // 【逐字】
Text(theme.fg("mdLink", BLOG_URL), 1, 0)
Spacer(1)
[若有图片] Image(base64, "image/png", { fallbackColor: t => theme.fg("muted", t) }, { maxWidthCells: 56, filename: IMAGE_FILENAME })
[若有图片] Spacer(1)
DynamicBorder(accent)
```

### 2.30 `components/extension-editor.ts`（141 行）

```ts
export interface ExtensionEditorOptions extends EditorOptions { description?: string }
export class ExtensionEditorComponent extends Container implements Focusable {
  constructor(tui: TUI, keybindings: KeybindingsManager, title: string,
              prefill: string | undefined,
              onSubmit: (value: string) => void, onCancel: () => void,
              options?: ExtensionEditorOptions, externalEditorCommand?: string);
  get focused(): boolean; set focused(v: boolean);       // 转发给内部 Editor
  handleInput(keyData: string): void;
}
```
- 外部编辑器命令：`externalEditorCommand || process.env.VISUAL || process.env.EDITOR || (win32 ? "notepad" : "nano")`。
- 布局：`DynamicBorder()` → `Spacer(1)` → `Text(theme.fg("accent", title), 1, 0)` → [有 description: Spacer(1) + `Text(theme.fg("text", description), 1, 0)`] → `Spacer(1)` → `Editor(tui, getEditorTheme(), editorOptions)`（有 prefill 则 `setText`）→ `Spacer(1)` → hint `keyHint("tui.select.confirm","submit") + "  " + keyHint("tui.input.newLine","newline") + "  " + keyHint("tui.select.cancel","cancel") + `  ${keyHint("app.editor.external","external editor")}` → `Spacer(1)` → `DynamicBorder()`。
- `handleInput`：`getKeybindings().matches(data,"tui.select.cancel")` → onCancel；`keybindings.matches(data,"app.editor.external")` → `void handleOpenExternalEditor()`；否则 `editor.handleInput(data)`。
- `handleOpenExternalEditor()`：`ui.stop()` → `editInExternalEditor({command, content: editor.getText()})` → 成功则 `editor.setText(result.content)` → `finally { ui.start(); ui.requestRender(true) }`。

### 2.31 `components/extension-input.ts`（94 行）

```ts
export interface ExtensionInputOptions { tui?: TUI; timeout?: number; initialValue?: string; description?: string }
export class ExtensionInputComponent extends Container implements Focusable {
  constructor(title: string, _placeholder: string | undefined,
              onSubmit: (value: string) => void, onCancel: () => void, opts?: ExtensionInputOptions);
  handleInput(keyData: string): void;
  dispose(): void;
}
```
布局：`DynamicBorder()` → `Spacer(1)` → `titleText = Text(theme.fg("accent", title),1,0)` → [description: Spacer(1) + `Text(theme.fg("text", desc),1,0)`] → `Spacer(1)` →
[`opts.timeout > 0 && opts.tui` → `CountdownTimer(timeout, tui, s => titleText.setText(theme.fg("accent", `${baseTitle} (${s}s)`)), () => onCancel())`] →
`Input()`（有 initialValue 则 setValue） → `Spacer(1)` → `Text(`${keyHint("tui.select.confirm","submit")}  ${keyHint("tui.select.cancel","cancel")}`,1,0)` → `Spacer(1)` → `DynamicBorder()`。
注意 `_placeholder` **未使用**（下划线前缀表明刻意忽略）。
`handleInput`：confirm 或 `"\n"` → `onSubmit(input.getValue())`；cancel → onCancel；否则 `input.handleInput`。

### 2.32 `components/extension-selector.ts`（117 行）

```ts
export interface ExtensionSelectorOptions {
  tui?: TUI; timeout?: number; onToggleToolsExpanded?: () => void; description?: string;
}
export class ExtensionSelectorComponent extends Container {
  constructor(title: string, options: string[], onSelect: (option: string) => void,
              onCancel: () => void, opts?: ExtensionSelectorOptions);
  handleInput(keyData: string): void;
  dispose(): void;
}
```
布局：`DynamicBorder()` → `Spacer(1)` → `titleText = Text(theme.fg("accent", theme.bold(title)), 1, 0)` → [description: Spacer(1)+`Text(theme.fg("text", desc),1,0)`] → `Spacer(1)` → [timeout → CountdownTimer，标题格式 `theme.fg("accent", theme.bold(`${baseTitle} (${s}s)`))`] → `listContainer` → `Spacer(1)` → `Text(rawKeyHint("↑↓","navigate") + "  " + keyHint("tui.select.confirm","select") + "  " + keyHint("tui.select.cancel","cancel"), 1, 0)` → `Spacer(1)` → `DynamicBorder()`。

`updateList()` 逐项：
```
selected: theme.fg("accent","→ ") + theme.fg("accent", option)
else:     `  ${theme.fg("text", option)}`
```
`handleInput` 顺序：`app.tools.expand` → `onToggleToolsExpanded?.()`；`tui.select.up` 或 `"k"` → `index = max(0, index-1)`；`tui.select.down` 或 `"j"` → `min(len-1, index+1)`；confirm 或 `"\n"` → 选中项存在则 onSelect；cancel → onCancel。

### 2.33 `components/first-time-setup.ts`（148 行）

```ts
export interface FirstTimeSetupResult { theme: string; shareAnalytics: boolean }
export interface FirstTimeSetupOptions {
  onThemePreview: (themeName: string) => void;
  onSubmit: (result: FirstTimeSetupResult) => void;
  onCancel: () => void;
}
export class FirstTimeSetupComponent extends Container { constructor(options: FirstTimeSetupOptions); }
```
常量：
```ts
const THEME_OPTIONS = [
  { value: SYSTEM_THEME_NAME, label: "System (matches your terminal colors)" },
  { value: "dark", label: "Dark" },
  { value: "light", label: "Light" },
];
const ANALYTICS_OPTIONS = [
  { value: true,  label: "Share anonymous usage data" },
  { value: false, label: "Don't share" },
];
const SETUP_LOGO_LINES = ["██████", "██  ██", "████  ██", "██    ██"];
```
`update()`（每次变更整块重建，以便主题预览重着色全部文字）：
```
clear()
DynamicBorder()
Spacer(1)
Text(theme.fg("accent", SETUP_LOGO_LINES.join("\n")), 1, 0)
Spacer(1)
Text(theme.fg("accent", theme.bold(`Welcome to ${APP_NAME}, the minimal coding agent.`)), 1, 0)   // 【逐字】
Spacer(1)
step === "theme":
  Text(theme.fg("text","Pick a theme."),1,0)
  Spacer(1)
  addOptionList(THEME_OPTIONS.map(o=>o.label), themeIndex)
else:
  Text(theme.fg("text","Opt-in to anonymous usage data sharing?"),1,0)
  Text(theme.fg("muted",
    "Opting in stores a tracking identifier in settings.json and enables anonymous\n" +
    "usage analytics. This helps us to better debug, reproduce, and resolve issues\n" +
    "and bugs within Kiana. You can observe what is shared using /privacy and make\n" +
    "changes anytime in settings.json."), 1, 0)                      // 【逐字】4 行，含硬换行
  Spacer(1)
  addOptionList(ANALYTICS_OPTIONS.map(o=>o.label), analyticsIndex)
Spacer(1)
Text(rawKeyHint("↑↓","navigate") + "  " + keyHint("tui.select.confirm", step==="theme" ? "continue" : "finish")
     + "  " + keyHint("tui.select.cancel","skip setup"), 1, 0)
Spacer(1)
DynamicBorder()
```
`addOptionList(labels, selectedIndex)`：选中 → `theme.fg("accent","→ ")` + `theme.fg("accent", label)`；否则 `"  "` + `theme.fg("text", label)`；每行 `Text(...,1,0)`。
`moveSelection(delta)`：theme 步骤内 clamp 到 `[0,2]`，变化时调用 `onThemePreview(THEME_OPTIONS[themeIndex].value)`；analytics 步骤 clamp 到 `[0,1]`；随后 `update()`。
`invalidate()`：先 `update()` 再 `super.invalidate()`（**顺序与常规相反**，刻意）。
`handleInput`：up/`k` → -1；down/`j` → +1；confirm/`"\n"` → theme 步骤进 analytics（`update()`），否则 `onSubmit({theme, shareAnalytics})`；cancel → `onCancel()`。

### 2.34 `components/footer.ts`（302 行）

```ts
export function formatTokens(count: number): string;
export function formatCwdForFooter(cwd: string, home: string | undefined): string;
export class FooterComponent implements Component {
  constructor(session: AgentSession, footerData: ReadonlyFooterDataProvider);
  setSession(session: AgentSession): void;
  setAutoCompactEnabled(enabled: boolean): void;
  invalidate(): void;   // no-op（git 分支缓存由 provider 处理）
  dispose(): void;      // no-op
  render(width: number): string[];
}
```
内部 `sanitizeStatusText(text)`（未导出）：`replace(/[\r\n\t]/g," ")` → `replace(/ +/g," ")` → `trim()`。

**`formatTokens(count)`**（逐字分支）：
```
< 1000        → count.toString()
< 10000       → `${(count/1000).toFixed(1)}k`
< 1000000     → `${Math.round(count/1000)}k`
< 10000000    → `${(count/1000000).toFixed(1)}M`
else          → `${Math.round(count/1000000)}M`
```

**`formatCwdForFooter(cwd, home)`**：
```
if (!home) return cwd
resolvedCwd = resolve(cwd); resolvedHome = resolve(home)
relativeToHome = relative(resolvedHome, resolvedCwd)
isInsideHome = relativeToHome === "" || (relativeToHome !== ".." && !startsWith(`..${sep}`) && !isAbsolute(relativeToHome))
if (!isInsideHome) return cwd
return relativeToHome === "" ? "~" : `~${sep}${relativeToHome}`
```
（不缩写只是共享 home 前缀的兄弟路径，例如 `/home/user2`。）

**`getSessionStats()` 缓存键**：`(session, sessionId, leafId, entryCount, limitsModel)`，其中 `limitsModel = session.routedModel?.model ?? session.model`（按引用比较）。命中则返回缓存。

**统计累加**（遍历 `sessionManager.getEntries()`）：
- `entry.type === "usage"` → `addUsageToTotals(usageTotals, entry.usage)`。
- `entry.type === "message" && role === "assistant"` → 累加 usage；`latestPromptTokens = input + cacheRead + cacheWrite`；`latestCacheHitRate = latestPromptTokens > 0 ? (cacheRead / latestPromptTokens) * 100 : undefined`（**每次遇到 assistant 都覆盖**，即“最新命中率”）。
- `entry.type === "message" && role === "toolResult" && entry.message.usage` → 累加。
- `(entry.type === "branch_summary" || entry.type === "compaction") && entry.usage` → 累加。
- `contextUsage = session.getContextUsage()`。

**`render(width)`**
```
state = session.state
{usageTotals, latestCacheHitRate, contextUsage} = getSessionStats()
contextWindow = contextUsage?.contextWindow ?? state.model?.contextWindow ?? 0
contextPercentValue = contextUsage?.percent ?? 0
contextPercent = contextUsage?.percent !== null ? contextPercentValue.toFixed(1) : "?"
pwd = formatCwdForFooter(sessionManager.getCwd(), process.env.HOME || process.env.USERPROFILE)
branch = footerData.getGitBranch(); if (branch) pwd = `${pwd} (${branch})`
sessionName = sessionManager.getSessionName(); if (sessionName) pwd = `${pwd} • ${sessionName}`
statsParts = []
  if (usageTotals.input)      push `↑${formatTokens(input)}`
  if (usageTotals.output)     push `↓${formatTokens(output)}`
  if (usageTotals.cacheRead)  push `R${formatTokens(cacheRead)}`
  if (usageTotals.cacheWrite) push `W${formatTokens(cacheWrite)}`
  if ((cacheRead > 0 || cacheWrite > 0) && latestCacheHitRate !== undefined) push `CH${latestCacheHitRate.toFixed(1)}%`
usingSubscription = state.model ? (state.model.provider === "kimi-coding" || modelRuntime.isUsingSubscription(state.model.provider)) : false
  if (usageTotals.cost || usingSubscription) push `$${cost.toFixed(3)}${usingSubscription ? " (sub)" : ""}`
autoIndicator = autoCompactEnabled ? " (auto)" : ""
contextPercentDisplay = contextPercent === "?" ? `?/${formatTokens(contextWindow)}${autoIndicator}`
                                              : `${contextPercent}%/${formatTokens(contextWindow)}${autoIndicator}`
contextPercentStr = contextPercentValue > 90 ? theme.fg("error", display)
                  : contextPercentValue > 70 ? theme.fg("warning", display)
                  : display
push contextPercentStr
if (areExperimentalFeaturesEnabled()) push `${theme.fg("dim","•")} ${theme.bold(theme.fg("warning","xp"))}`
statsLeft = statsParts.join(" ")
modelName = state.model?.id || "no-model"
statsLeftWidth = visibleWidth(statsLeft)
if (statsLeftWidth > width) { statsLeft = truncateToWidth(statsLeft, width, "..."); statsLeftWidth = visibleWidth(statsLeft) }
minPadding = 2
rightSideWithoutProvider = modelName
if (state.model?.reasoning) {
  level = state.thinkingLevel || "off"
  rightSideWithoutProvider = level === "off" ? `${modelName} • thinking off` : `${modelName} • ${level}`
}
routed = session.routedModel
if (routed) rightSideWithoutProvider += ` → ${routed.model.id}${routed.thinkingLevel ? ` • ${routed.thinkingLevel}` : ""}`
rightSide = rightSideWithoutProvider
if (footerData.getAvailableProviderCount() > 1 && state.model) {
  rightSide = `(${state.model.provider}) ${rightSideWithoutProvider}`
  if (statsLeftWidth + minPadding + visibleWidth(rightSide) > width) rightSide = rightSideWithoutProvider
}
rightSideWidth = visibleWidth(rightSide); totalNeeded = statsLeftWidth + minPadding + rightSideWidth
if (totalNeeded <= width) {
  padding = " ".repeat(width - statsLeftWidth - rightSideWidth)
  statsLine = statsLeft + padding + rightSide
} else {
  availableForRight = width - statsLeftWidth - minPadding
  if (availableForRight > 0) {
    truncatedRight = truncateToWidth(rightSide, availableForRight, "")
    padding = " ".repeat(max(0, width - statsLeftWidth - visibleWidth(truncatedRight)))
    statsLine = statsLeft + padding + truncatedRight
  } else statsLine = statsLeft
}
dimStatsLeft = theme.fg("dim", statsLeft)
remainder = statsLine.slice(statsLeft.length)          // padding + rightSide（用长度切片，不解析 ANSI）
dimRemainder = theme.fg("dim", remainder)
pwdLine = truncateToWidth(theme.fg("dim", pwd), width, theme.fg("dim", "..."))
lines = [pwdLine, dimStatsLeft + dimRemainder]
extensionStatuses = footerData.getExtensionStatuses()
if (extensionStatuses.size > 0) {
  sortedStatuses = [...entries].sort(([a],[b]) => a.localeCompare(b)).map(([,text]) => sanitizeStatusText(text))
  lines.push(truncateToWidth(sortedStatuses.join(" "), width, theme.fg("dim","...")))
}
return lines
```
注意 `remainder = statsLine.slice(statsLeft.length)` 依赖 `statsLeft` 是 `statsLine` 的**前缀**（是的，两种分支都如此），因此可直接按 UTF-16 长度切片。

测试（`footer-width.test.ts`）：
- `does not abbreviate sibling paths that share the home prefix`
- `abbreviates the home directory and descendants`
- `keeps all lines within width for wide session names`
- `keeps stats line within width for wide model and provider names`（含 CJK 宽字符）
- `shows the physical model a virtual model routed to`（`auto • high → gpt-5.6-luna • medium`）
- `includes summary and tool result usage in the total cost`
- `updates cached usage totals after an entry is appended`
- `shows the latest cache hit rate when cache usage is present`
- `marks Kimi Coding costs as subscription estimates`
- `marks explicitly identified subscription auth`
- `does not mark generic OAuth sign-in as a subscription`

---


### 2.35 `components/auth-url.ts`（39 行）

```ts
export class AuthUrlComponent extends Container {
  readonly url: string;
  constructor(tui: TUI, url: string);
  async copy(): Promise<void>;
}
```
布局：`Text(theme.fg("accent", hyperlink(url, url)), 1, 0)` + `hint: Text("",1,0)`。
初始 hint：`` `${theme.fg("dim", hyperlink(clickHint, this.url))} ${theme.fg("dim","•")} ${suffix}` ``，其中 `clickHint = platform === "darwin" ? "Cmd+click to open" : "Ctrl+click to open"`，初始 `suffix = keyHint("app.message.copy","to copy")`。
`copy()`：成功 → hint suffix `theme.fg("success","Copied URL to clipboard")`；失败 → `theme.fg("error", message)`；每次 setHint 后 `tui.requestRender()`。

测试（`auth-url-copy.test.ts`，3 条，**实际名称**）：`login dialog copies the auth URL instead of typing into the code input`、`login dialog ignores the copy key without an auth URL`、`MCP sign-in screen copies the authorization URL`。

### 2.36 `components/visual-truncate.ts`（98 行）

```ts
export interface VisualTruncateResult { visualLines: string[]; skippedCount: number }
export function truncateToVisualLines(text: string, maxVisualLines: number, width: number,
                                      paddingX = 0, keep: "start" | "end" = "end"): VisualTruncateResult;
export interface VisualLinePreviewOptions {
  text: string; maxVisualLines: number; keep: "start" | "end";
  formatHint: (hidden: number) => string;
}
export class VisualLinePreview implements Component {
  constructor(options: VisualLinePreviewOptions);
  render(width: number): string[];
  invalidate(): void;
}
```
`truncateToVisualLines`：空文本 → `{[], 0}`；`new Text(text, paddingX, 0).render(width)` 得全部视觉行；`<= maxVisualLines` 返回全部；否则 `keep==="start" ? slice(0,max) : slice(-max)`，`skippedCount = 全部 - max`。
`VisualLinePreview.render`：按宽度缓存；若 `skippedCount > 0`，`hint = truncateToWidth(formatHint(skipped), width, "...")`，`keep==="start"` 放末尾否则放开头。

### 2.37 `components/show-images-selector.ts`（50 行）/ `theme-selector.ts`（67 行）

**ShowImagesSelectorComponent**
```ts
constructor(currentValue: boolean, onSelect: (show: boolean) => void, onCancel: () => void);
getSelectList(): SelectList;
```
布局：`DynamicBorder()` → `SelectList(items, 5, getSelectListTheme(), {minPrimaryColumnWidth:12,maxPrimaryColumnWidth:32})` → `DynamicBorder()`。
items：`{value:"yes",label:"Yes",description:"Show images inline in terminal"}`、`{value:"no",label:"No",description:"Show text placeholder instead"}`；`setSelectedIndex(currentValue ? 0 : 1)`；`onSelect` → `onSelect(item.value === "yes")`。

**ThemeSelectorComponent**
```ts
constructor(currentTheme: string, onSelect: (themeName: string) => void,
            onCancel: () => void, onPreview: (themeName: string) => void);
getSelectList(): SelectList;
```
items = `getAvailableThemes()` 映射，`description: name === currentTheme ? "(current)" : undefined`；`SelectList(themeItems, 10, ...)`；预选 `themes.indexOf(currentTheme)`（-1 则不设）；`onSelectionChange` → `onPreview(item.value)`。

测试（`theme-picker.test.ts`、`theme-detection.test.ts`、`theme-style.test.ts`、`theme-export.test.ts`）：已在前文整理。

### 2.38 `components/mermaid.ts`（89 行）

```ts
export function createMermaidMarkdownTransformer(options: {
  getMode: () => MermaidRenderingMode;   // "off" | "final" | "streaming"
  theme?: Theme;
}): MarkdownTransformer;
```
- 模块级 `const markdownParser = new Marked();`
- `isMermaid(token)`：`token.type === "code"` 且 `token.lang?.trim().split(/\s+/,1)[0]?.toLowerCase() === "mermaid"`。
- 跳过条件（返回原 markdown）：`mode === "off"`、`context.messageType === "assistant-thinking"`、`context.isStreaming && mode !== "streaming"`。
- 用 `markdownParser.lexer(markdown)` 顶层 token 逐个处理，非 mermaid 原样 `token.raw`。
- `art = render(token.text)`（`grok-mermaid` 的 `render`）；`!art || art.width > context.availableWidth` → 原样。
- 非流式且 `art.warnings.length > 0` → 原样 + `` `\n${codeSpan(warning)}  \n` ``，其中
  `warning = `Mermaid diagram not rendered: ${art.warnings[0]}${art.warnings.length > 1 ? ` (+${art.warnings.length-1} more)` : ""}``，若 `options.theme` 存在则 `theme.fg("warning", warning)`。**【逐字】**
- 否则 `lines = theme ? art.styled.map(row => row.map(styleSpan).join("")) : art.plain`，输出 `` `${lines.map(codeSpan).join("  \n")}\n` ``（**每行之间两个空格 + 换行** = Markdown 硬换行）。

**`codeSpan(line)`**：`content = line || " "`；取内容中最长连续反引号长度 `n`，`fence = "`".repeat(n+1)`；若内容首/尾是反引号则前后各加一个空格；返回 `fence + padding + content + padding + fence`。

**`styleSpan(span)`** 映射：
| `span.cls` | 渲染 |
|---|---|
| `border` | `theme.fg("borderMuted", text)` |
| `text` | `theme.fg("text", text)` |
| `edge` | `theme.fg("accent", text)` |
| `edgeLabel` | `theme.fg("muted", text)` |
| `title` | `theme.fg("accent", theme.bold(text))` |
| `none` | 原样 |

测试（`mermaid.test.ts`，8 条，**实际名称**）：`replaces Mermaid code blocks with Unicode diagrams`、`leaves unsupported and oversized diagrams unchanged`、`maps semantic spans through the Kiana theme`、`renders incomplete Mermaid blocks during streaming`、`renders diagrams with class assignments`、`falls back to the code block with a warning after streaming`、`summarizes additional partial-render warnings`、`respects rendering modes and skips thinking blocks`。

### 2.39 `components/status-indicator.ts`（123 行）

```ts
export type StatusIndicatorKind = "working" | "retry" | "compaction" | "branchSummary";
export class StatusIndicator extends Loader {
  readonly kind: StatusIndicatorKind;
  constructor(kind, ui: TUI, spinnerColorFn: (s:string)=>string, messageColorFn: (s:string)=>string,
              message: string, indicator?: WorkingIndicatorOptions);
  renderInBorder(width: number): string;
  renderSpinnerInBorder(width: number): string;
  dispose(): void;   // stop()
}
export class WorkingStatusIndicator extends StatusIndicator { constructor(ui, message, indicator?, colorFn?) }
export class RetryStatusIndicator extends StatusIndicator { constructor(ui, attempt, maxAttempts, delayMs) }
export type CompactionStatusReason = "manual" | "threshold" | "overflow";
export class CompactionStatusIndicator extends StatusIndicator { constructor(ui, reason, colorFn?) }
export class BranchSummaryStatusIndicator extends StatusIndicator { constructor(ui) }
export class IdleStatus implements Component { render(width) => [" ".repeat(width), " ".repeat(width)] }
```
- `renderInBorder(width)`：`line = super.render(width + 2)[1] ?? ""`；若以 `" "` 开头则 `line.slice(1).trimEnd()`，否则 `line.trimEnd()`；再 `truncateToWidth(..., width, "")`。
- `renderSpinnerInBorder(width)`：`truncateToWidth(this.getRenderedIndicator(), width, "")`。
- `WorkingStatusIndicator` 默认颜色：spinner `theme.fg("accent")`、message `theme.fg("muted")`；若给 `colorFn` 则两者都用它。
- `RetryStatusIndicator`：spinner `warning`、message `muted`；文案 `` `Retrying (${attempt}/${maxAttempts}) in ${seconds}s... (${keyText("app.interrupt")} to cancel)` `` **【逐字】**；创建 `CountdownTimer(delayMs, ui, seconds => setMessage(...), () => countdown = undefined)`；`dispose()` 先 dispose countdown。
- `CompactionStatusIndicator`：`cancelHint = `(${keyText("app.interrupt")} to cancel)``；`manual` → `` `Compacting context... ${cancelHint}` ``；否则 `` `${reason === "overflow" ? "Context overflow detected, " : ""}Auto-compacting... ${cancelHint}` ``。**【逐字】**
- `BranchSummaryStatusIndicator`：`` `Summarizing branch... (${keyText("app.interrupt")} to cancel)` ``。**【逐字】**

测试（`status-indicator.test.ts`，5 条，**实际名称**）：`keeps idle status at the same height as standalone status indicators`、`keeps the top border unchanged unless the editor opts in`、`embeds the working indicator when the editor opts in`、`embeds compaction, summary, and retry labels within the border width`、`disposes retry countdown updates`。

### 2.40 `components/thinking-selector.ts`（154 行）

```ts
const THINKING_SELECT_LIST_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 };
const LEVEL_DESCRIPTIONS: Record<ThinkingLevel, string> = {
  off: "No reasoning",
  minimal: "Very brief reasoning (~1k tokens)",
  low: "Light reasoning (~2k tokens)",
  medium: "Moderate reasoning (~8k tokens)",
  high: "Deep reasoning (~16k tokens)",
  xhigh: "Extra-high reasoning (~32k tokens)",
  max: "Maximum reasoning",
};
export class ThinkingSelectorComponent extends Container implements Focusable {
  constructor(currentLevel, availableLevels, onSelect, onCancel,
              onSelectAsDefault?, defaultThinkingLevel?);
  handleInput(keyData: string): void;
  getSelectList(): SelectList;
}
```
布局顺序：`DynamicBorder()` → `Spacer(1)` → `Text("Thinking Level",0,0)` → `Spacer(1)` → `` Text(`${keyDisplayText("app.thinking.cycle")} cycles thinking levels in-session`,0,0) `` → `Spacer(1)` → `searchInput: Input`（`onSubmit = () => selectList.handleInput("\r")`） → `Spacer(1)` → `selectList` → `Spacer(1)` → `` Text(theme.fg("dim", `  ${keyDisplayText("tui.select.confirm")} to select · ${keyDisplayText("app.thinking.save")} to set as default · ${keyDisplayText("tui.select.cancel")} to cancel`),0,0) `` → `DynamicBorder()`。
items：`label = `${level === currentLevel ? "✓ " : "  "}${level}``；`description = level === defaultThinkingLevel ? `${LEVEL_DESCRIPTIONS[level]} · default` : LEVEL_DESCRIPTIONS[level]`。
`buildSelectList(items, preselect)`：`SelectList(items, Math.max(1, items.length), getSelectListTheme(), LAYOUT)`；预选 `findIndex(item => item.value === preselect)`。
`applyFilter(query)`：`fuzzyFilter(allItems, query, item => `${item.value} ${item.description ?? ""}`)`；保留当前选中值；直接替换 `children[selectListChildIndex]`。
`handleInput` 顺序：`app.thinking.save` 且有 `onSelectAsDefault` → 调用并 return；导航键（up/down/confirm/cancel）→ 交给 selectList；其他 → 交给 searchInput 再 applyFilter。

测试（`thinking-selector.test.ts`）。

### 2.41 `components/trust-selector.ts`（134 行）

```ts
export type TrustSelection = Pick<ProjectTrustOption, "trusted" | "updates">;
export interface TrustSelectorOptions {
  cwd: string; savedDecision: ProjectTrustStoreEntry | null; projectTrusted: boolean;
  onSelect: (s: TrustSelection) => void; onCancel: () => void;
}
export class TrustSelectorComponent extends Container { constructor(options); }
```
`formatDecision(trustPath, decision)`：
- `decision === null` → `"none"`（逐字）
- `label = decision.decision ? "trusted" : "untrusted"`
- `trustPath !== undefined && decision.path !== trustPath` → `` `${label} (inherited from ${decision.path})` ``
- 否则 `` `${label} (${decision.path})` ``

布局：`DynamicBorder()` → `Spacer(1)` → `Text(theme.fg("accent", theme.bold("Project trust")),1,0)` → `Text(theme.fg("muted", cwd),1,0)` → `Spacer(1)` → `` Text(theme.fg("muted", `Saved decision: ${formatDecision(options.savedDecision?.path, options.savedDecision)}`),1,0) `` 【注意源码这里用的是 `this.trustOptions[0]?.savedPath` 而不是 `savedDecision?.path`，见下一行注】 → `` Text(theme.fg("muted", `Current session: ${options.projectTrusted ? "trusted" : "untrusted"}`),1,0) `` → `Spacer(1)` → `listContainer` → `Spacer(1)` → hints `` rawKeyHint("↑↓","navigate") + "  " + keyHint("tui.select.confirm","save") + "  " + keyHint("tui.select.cancel","cancel") `` → `Spacer(1)` → `DynamicBorder()`。

> **源码细节（必须照抄）**：`Saved decision:` 的参数实际是 `this.trustOptions[0]?.savedPath`（第一个选项的 `savedPath`），不是 `options.savedDecision?.path`。Rust 移植需保留这一行为。
初始选中：`Math.max(0, trustOptions.findIndex(option => isSavedOption(option)))`；`isSavedOption` 要求 `option.savedPath !== undefined && savedDecision?.decision === option.trusted && savedDecision.path === option.savedPath`。
每行：`prefix = selected ? accent("→ ") : "  "`；`currentMarker = isCurrent ? accent("✓ ") : "  "`；`label = selected ? accent(label) : text(label)`；拼接 `${prefix}${currentMarker}${label}`，`Text(..., 1, 0)`。
`handleInput`：up 或 `"k"` / down 或 `"j"`（clamp，不循环）；confirm 或 `"\n"` → `onSelect({trusted, updates})`；cancel → onCancel。

测试（`trust-selector.test.ts`）。

### 2.42 `components/user-message-selector.ts`（155 行）

```ts
export class UserMessageSelectorComponent extends Container {
  constructor(messages: Array<{id: string; text: string; timestamp?: string}>,
              onSelect: (entryId: string) => void, onCancel: () => void, initialSelectedId?: string);
  getMessageList(): UserMessageList;
}
```
布局：`Spacer(1)` → `Text(theme.bold("Fork from Message"),1,0)` → `` Text(theme.fg("muted","Select a user message to copy the active path up to that point into a new session"),1,0) `` → `Spacer(1)` → `DynamicBorder()` → `Spacer(1)` → `messageList` → `Spacer(1)` → `DynamicBorder()`。
`messages.length === 0` → `setTimeout(() => onCancel(), 100)`。
**UserMessageList**：`maxVisible = 10`；初始选中 = `initialSelectedId` 的下标，否则最后一条（最新）。
渲染：
- 空 → `theme.fg("muted","  No user messages found")`。
- 每条消息 3 行：`cursor = selected ? accent("› ") : "  "`；`normalized = text.replace(/\n/g," ").trim()`；`truncateToWidth(normalized, width-2)`；选中加 `theme.bold`；第二行 `` theme.fg("muted", `  Message ${i+1} of ${total}`) ``；第三行空串。
- 需要滚动时追加 `` theme.fg("muted", `  (${selectedIndex+1}/${total})`) ``。
`handleInput`：up 在顶部时**环绕到最后**；down 在底部时**环绕到第一**；confirm → onSelect(id)；cancel → onCancel。

测试（`user-message.test.ts` 中的 `Fork from Message` 相关；以及 `interactive-mode-clone-command.test.ts`）。

### 2.43 `components/session-selector.ts`（1045 行）

导出：`export class SessionSelectorComponent extends Container implements Focusable`，方法 `handleInput(data)`、`getSessionList()`。
未导出但必须实现的辅助：
```ts
type SessionScope = "current" | "all";
function shortenPath(path): string;              // ~ 前缀替换（home）
function formatSessionDate(date: Date): string;  // "now"|"Nm"|"Nh"|"Nd"|"Nw"|"Nmo"|"Ny"
class SessionSelectorHeader implements Component
interface SessionTreeNode { session; children; latestActivity }
interface FlatSessionNode { session; depth; isLast; ancestorContinues: boolean[] }
function buildSessionTree(sessions): SessionTreeNode[]
function flattenSessionTree(roots): FlatSessionNode[]
class SessionList implements Component, Focusable
async function deleteSessionFile(sessionPath): Promise<{ok, method:"trash"|"unlink", error?}>
```

**`formatSessionDate`** 阈值（逐字单位）：
```
diffMins < 1 → "now"
diffMins < 60 → `${diffMins}m`
diffHours < 24 → `${diffHours}h`
diffDays < 7 → `${diffDays}d`
diffDays < 30 → `${Math.floor(diffDays/7)}w`
diffDays < 365 → `${Math.floor(diffDays/30)}mo`
else → `${Math.floor(diffDays/365)}y`
```
（`diffMins = floor(ms/60000)`，`diffHours = floor(ms/3600000)`，`diffDays = floor(ms/86400000)`。）

**`buildSessionTree`**：以 `canonicalizePath(session.path) ?? session.path` 建 `byPath`；再遍历一次，父路径能规范化且 `byPath.has(parentPath)` 则挂为子节点，否则为根。`updateLatestActivity` 递归取子树最大 `modified`；`sortNodes` 按 `latestActivity` **降序**递归排序。

**`flattenSessionTree`**：`walk(node, depth, ancestorContinues, isLast)`：push 当前节点；对每个子节点 `continues = depth > 0 ? !isLast : false`，`walk(child, depth+1, [...ancestorContinues, continues], childIsLast)`。根节点 `walk(roots[i], 0, [], i === roots.length-1)`。

**SessionSelectorHeader.render(width)**：
```
title = scope === "current" ? "Resume Session (Current Folder)" : "Resume Session (All)"; leftText = theme.bold(title)
sortLabel = sortMode==="threaded" ? "Threaded" : sortMode==="recent" ? "Recent" : "Fuzzy"
sortText = theme.fg("muted","Sort: ") + theme.fg("accent", sortLabel)
nameLabel = nameFilter==="all" ? "All" : "Named"
nameText = theme.fg("muted","Name: ") + theme.fg("accent", nameLabel)
loading ? scopeText = `${fg("muted","○ Current Folder | ")}${fg("accent", `Loading ${loadProgress ? `${loaded}/${total}` : "..."}`)}`
        : current → `${fg("accent","◉ Current Folder")}${fg("muted"," | ○ All")}`
        :           `${fg("muted","○ Current Folder | ")}${fg("accent","◉ All")}`
rightText = truncateToWidth(`${scopeText}  ${nameText}  ${sortText}`, width, "")
availableLeft = max(0, width - visibleWidth(rightText) - 1)
left = truncateToWidth(leftText, availableLeft, "")
spacing = max(0, width - visibleWidth(left) - visibleWidth(rightText))
第一行：`${left}${" ".repeat(spacing)}${rightText}`
hintLine1/hintLine2（按状态，全部 truncateToWidth(...,width,"…")）：
  confirmingDeletePath !== null →
    hint1 = theme.fg("error", `Delete session? ${keyHint("tui.select.confirm","confirm")} · ${keyHint("tui.select.cancel","cancel")}`), hint2 = ""
  statusMessage →
    hint1 = theme.fg(type==="error"?"error":"accent", message), hint2 = ""
  否则 →
    hint1 = keyHint("tui.input.tab","scope") + fg("muted"," · ") + fg("muted", `re:<pattern> regex · "phrase" exact`)
    hint2 = [keyHint("app.session.toggleSort","sort"), keyHint("app.session.toggleNamedFilter","named"),
             keyHint("app.session.delete","delete"),
             keyHint("app.session.togglePath", `path ${showPath ? "(on)" : "(off)"}`)]
            (+ showRenameHint 时 keyHint("app.session.rename","rename"))
            以 fg("muted"," · ") 连接
返回 [line1, hint1, hint2]
```
`setStatusMessage(msg, autoHideMs?)`：先清旧定时器；`msg && autoHideMs` 时 `setTimeout(autoHide, autoHideMs)` 内清消息并 `requestRender()`。
`setLoading(loading)` 会**清空** `loadProgress`。

**SessionList** 关键行为：
- 字段：`maxVisible = 10`、`selectionTouched = false`、`showPath = false`、`sortMode = "threaded"`、`nameFilter = "all"`、`currentSessionCanonicalPath`。
- `filterSessions(query)`：`nameFilter === "named"` 时先 `filter(session => hasSessionName(session))`；`sortMode === "threaded" && !trimmed` → 树；否则 `filterAndSortSessions(nameFiltered, query, sortMode, "all")` 转平铺（depth 0、isLast true）。之后 `selectedIndex = min(selectedIndex, max(0, length-1))`。
- `setSessions(sessions, showCwd)`：未触碰过选择则 `selectedIndex = 0`；否则尽量保留同一路径。
- `render(width)`：
  - 先 `searchInput.render(width)` 加一个空行。
  - 空列表文案（**逐字**）：
    - `nameFilter === "named"`：
      - `showCwd` → `` `  No named sessions found. Press ${toggleKey} to show all.` ``
      - 否则 → `` `  No named sessions in current folder. Press ${toggleKey} to show all, or Tab to view all.` ``
    - `showCwd` → `"  No sessions found"`
    - 否则 → `"  No sessions in current folder. Press Tab to view all."`
    - 全部 `theme.fg("muted", truncateToWidth(msg, width, "…"))`
  - 行构造：`prefix = buildTreePrefix(node)`（`depth===0 → ""`；否则 `ancestorContinues.map(c => c ? "│  " : "   ").join("") + (isLast ? "└─ " : "├─ ")`）。
    `displayText = session.name ?? session.firstMessage`；`normalizedMessage = displayText.replace(/[\x00-\x1f\x7f]/g," ").trim()`。
    `age = formatSessionDate(session.modified)`；`rightPart = `${messageCount} ${age}``；`showCwd && session.cwd` → 前置 `shortenPath(cwd) + " "`；`showPath` → 再前置 `shortenPath(session.path) + " "`（顺序：先 cwd 再 path，最终 `path cwd count age`）。
    `cursor = selected ? accent("› ") : "  "`。
    `availableForMsg = width - 2 - visibleWidth(prefix) - (visibleWidth(rightPart) + 2)`；`truncateToWidth(normalizedMessage, max(10, availableForMsg), "…")`。
    颜色优先级：`isConfirmingDelete → "error"`；否则 `isCurrent → "accent"`；否则 `hasName → "warning"`；否则无色。选中再 `theme.bold`。
    `leftPart = cursor + fg("dim", prefix) + styledMsg`；`spacing = max(1, width - visibleWidth(leftPart) - visibleWidth(rightPart))`；`styledRight = fg(isConfirmingDelete ? "error":"dim", rightPart)`；选中整行 `theme.bg("selectedBg", line)`；最后 `truncateToWidth(line, width)`。
  - 滚动指示：`` theme.fg("muted", truncateToWidth(`  (${selectedIndex+1}/${total})`, width, "")) ``。
- `handleInput` 顺序（**确认删除态优先拦截所有键**）：
  1. `confirmingDeletePath !== null`：confirm → 清状态并 `void onDeleteSession(path)`；cancel → 清状态；其他键**全部忽略**。
  2. `tui.input.tab` → `onToggleScope()`。
  3. `app.session.toggleSort` → `onToggleSort()`。
  4. `app.session.toggleNamedFilter` → `onToggleNameFilter()`。
  5. `app.session.togglePath` → 翻转 `showPath` 并 `onTogglePath(showPath)`。
  6. `app.session.delete` → `startDeleteConfirmationForSelectedSession()`（当前会话 → `onError("Cannot delete the currently active session")`）。
  7. `app.session.rename` → `onRenameSession(path)`。
  8. `app.session.deleteNoninvasive`：搜索框非空 → 转发给输入框并重新过滤；否则同 delete。
  9. 之后才设 `selectionTouched = true`；up/down（clamp，不环绕）、pageUp/pageDown（±maxVisible，clamp）、confirm → onSelect、cancel → onCancel、其他 → 搜索框。
- `deleteSessionFile`：先 `spawnSync("trash", sessionPath.startsWith("-") ? ["--", sessionPath] : [sessionPath], {encoding:"utf-8"})`；`status === 0 || !existsSync(path)` → `{ok:true, method:"trash"}`；否则 `unlink`；失败错误串：`unlinkError`（若有 trash 提示则 `` `${unlinkError} (${trashErrorHint})` ``，`trashErrorHint = `trash: ${parts.join(" · ").slice(0,200)}``，parts = [error.message?, stderr 第一行?]）。

**SessionSelectorComponent**
```ts
type SessionsLoader = (onProgress?: SessionListProgress, signal?: AbortSignal) => Promise<SessionInfo[]>;
constructor(currentSessionsLoader, allSessionsLoader, onSelect, onCancel, onExit, requestRender,
            options?: { renameSession?: (path, currentName) => Promise<void>;
                        showRenameHint?: boolean; keybindings?: KeybindingsManager },
            currentSessionFilePath?: string);
```
- 布局：`Spacer(1)` → `DynamicBorder(s => theme.fg("accent", s))` → `Spacer(1)` → [`header` + `Spacer(1)`（`showHeader` 默认 true）] → `content` → `Spacer(1)` → `DynamicBorder(accent)`。
- `rename` 模式：`handleInput` 交给 `renameInput`（cancel 退出 rename 模式）；面板：`Text(theme.bold("Rename Session"),1,0)` → `Spacer(1)` → `renameInput` → `Spacer(1)` → `` Text(theme.fg("muted", `${keyText("tui.select.confirm")} to save · ${keyText("tui.select.cancel")} to cancel`),1,0) ``，布局 `showHeader:false`。
- `confirmRename(value)`：`next = value.trim()`；空则直接返回；成功 `await renameSession(target, next)` 后 `refreshSessionsAfterMutation()`；`finally` 退出 rename 模式。
- `loadScope(scope)`：已有进行中的 load 则返回；`AbortController`；`header.setScope`、`setLoading(true)`、`requestRender`；`onProgress(loaded, total, partialSessions)`：active 才处理，`partialSessions` 存在则缓存并（若为当前 scope）`sessionList.setSessions`；`scope === this.scope` 时 `setProgress` + render。成功：缓存、`setLoading(false)`、`setSessions`。失败：`setLoading(false)`、`setStatusMessage({type:"error", message: `Failed to load sessions: ${message}`}, 4000)`、`setSessions([], showCwd)`。
- 删除成功文案：`"Session moved to trash"`（trash）/ `"Session deleted"`（unlink），info 2000ms；失败：`` `Failed to delete: ${errorMessage}` `` error 3000ms。
- `toggleSortMode`：`threaded → recent → relevance → threaded`。
- `toggleNameFilter`：`all ↔ named`。
- `toggleScope`：`current ↔ all`；若该 scope 无缓存且未在加载则 `loadScope`。
- 构造末尾 `void this.loadScope("current")`。

测试（`session-selector-search.test.ts`、`session-selector-rename.test.ts`、`session-selector-path-delete.test.ts`）。

### 2.44 `components/tree-selector.ts`（1428 行）

导出：
```ts
export type FilterMode = "default" | "no-tools" | "user-only" | "labeled-only" | "all";
export class TreeSelectorComponent extends Container implements Focusable {
  constructor(tree: SessionTreeNode[], currentLeafId: string | null, terminalHeight: number,
              onSelect: (entryId: string) => void, onCancel: () => void,
              onLabelChange?: (entryId, label: string | undefined) => void,
              initialSelectedId?: string, initialFilterMode?: FilterMode);
  handleInput(keyData: string): void;
  getTreeList(): TreeList;
  public onCopy?: (text: string | undefined) => void;
}
```
常量：`TREE_GUTTER_WIDTH = 2`、`MIN_VISIBLE_ANCHOR_CONTENT_WIDTH = 4`、`MAX_VISIBLE_ANCHOR_CONTENT_WIDTH = 20`、`MIN_ANCHOR_CONTEXT_WIDTH = 2`、`MAX_ANCHOR_CONTEXT_WIDTH = 12`。

**`renderHorizontalViewport(rows, width)`**
```
viewportWidth = max(0, width - 2)
maxBodyWidth = max(row.bodyWidth)
maxHorizontalScroll = max(0, maxBodyWidth - viewportWidth)
selectedRow = rows.find(r => r.isSelected)
horizontalScroll = 0
if (selectedRow && maxHorizontalScroll > 0) {
  minVisible = min(20, max(4, floor(viewportWidth/3)))
  if (selectedRow.anchorCol > viewportWidth - minVisible) {
    anchorContext = min(12, max(2, floor(viewportWidth/4)))
    horizontalScroll = min(maxHorizontalScroll, selectedRow.anchorCol - anchorContext)
  }
}
每行：scroll > 0 ? `${gutter}${sliceByColumn(body, scroll, viewportWidth, true)}\x1b[0m` : gutter + body
     再 truncateToWidth(line, width, "")
```

**TreeList**（未导出）：`maxVisibleLines`、`filterMode`、`searchQuery`、`toolCallMap`、`multipleRoots`、`showLabelTimestamps`、`activePathIds`、`visibleParentMap`、`visibleChildrenMap`、`lastSelectedId`、`foldedNodes`。

`flattenTree(roots)` 规则（**必须逐字实现**）：
- 先做后序计算 `containsActive`（当前叶所在子树），再按 `containsActive` 把根/子节点**排前**。
- 多根时视为虚拟根的子节点：根 `indent = 1`、`justBranched = true`、`showConnector = true`、`isVirtualRootChild = true`。
- 子缩进：`multipleChildren → indent+1`；`justBranched && indent > 0 → indent+1`；否则 `indent`。
- gutter：`connectorDisplayed = showConnector && !isVirtualRootChild`；`currentDisplayIndent = multipleRoots ? max(0, indent-1) : indent`；`connectorPosition = max(0, currentDisplayIndent - 1)`；`childGutters = connectorDisplayed ? [...gutters, {position: connectorPosition, show: !isLast}] : gutters`。
- 同时收集 assistant 消息里的 `toolCall` 到 `toolCallMap`。

`applyFilter()`：
- `filteredNodes.length > 0` 时先更新 `lastSelectedId`。
- 搜索 token：`searchQuery.toLowerCase().split(/\s+/).filter(Boolean)`，全部 `includes` 才保留。
- 永远排除 `entry.type === "usage"`。
- 非当前叶的纯 toolCall assistant 消息（`hasText === false && (!stopReason || stopReason === "stop" || stopReason === "toolUse")`）隐藏。
- `isSettingsEntry = ["label","context_edit","custom","model_change","thinking_level_change","session_info"].includes(entry.type)`。
- 过滤模式：`user-only` → 仅 user 消息；`no-tools` → `!isSettingsEntry && !toolResult`；`labeled-only` → `node.label !== undefined`；`all` → 全过；`default` → `!isSettingsEntry`。
- 折叠：先按 `flatNodes` 顺序计算 `skipSet`（父在 `foldedNodes` 或已在 `skipSet` 则加入），再剔除。
- `recalculateVisualStructure()` 用可见树重新走一遍与 `flattenTree` 相同的缩进/gutter 逻辑，并重建 `visibleParentMap` / `visibleChildrenMap`，更新 `multipleRoots`。
- 光标恢复：`lastSelectedId` → `findNearestVisibleIndex`（沿 parentId 上溯找可见祖先，找不到则**最后一行**）；否则越界时 clamp。

`getSearchableText(node)` 拼接（按 entry 类型）：
- `node.label`（若有）
- `message`：`msg.role`、`extractContent(msg.content)`，`bashExecution` 再拼 `command`
- `custom_message`：`customType` + 内容
- `compaction`：`"compaction"`；`branch_summary`：`"branch summary"` + `summary`
- `session_info`：`"title"` + `name?`；`model_change`：`"model"` + `modelId`；`thinking_level_change`：`"thinking"` + `thinkingLevel`
- `custom`：`"custom"` + `customType`；`context_edit`：`"context edit"` + (`omit`|`replace`) + `targetId`；`label`：`"label"` + (`label ?? ""`)

`render(width)`：
- 空：`theme.fg("muted","  No entries found")` + `` theme.fg("muted", `  (0/0)${statusLabels}`) ``。
- 行构造：`cursor = selected ? accent("› ") : "  "`；`displayIndent = multipleRoots ? max(0, indent-1) : indent`；`connector = showConnector && !isVirtualRootChild ? (isLast ? "└─ " : "├─ ") : ""`，`connectorPosition = connector ? displayIndent - 1 : -1`。
- 逐字符构建 `prefix`（长度 `displayIndent * 3`）：`level = floor(i/3)`，`posInLevel = i%3`；该 level 有 gutter → `posInLevel===0 ? (show?"│":" ") : " "`；否则 `connector && level === connectorPosition` → `["└"|"├", isFolded?"⊞":isFoldable?"⊟":"─", " "]`；否则 `" "`。
- `foldMarker = isFolded && !showsFoldInConnector ? accent("⊞ ") : ""`；
- `pathMarker = activePathIds.has(id) ? accent("• ") : ""`；
- `label = node.label ? warning(`[${label}] `) : ""`；
- `labelTimestamp`（开启时）`muted(`${formatLabelTimestamp(ts)} `)`；
- `content = getEntryDisplayText(...)`；`prefixPart = fg("dim", prefix) + foldMarker + pathMarker`；`anchorCol = visibleWidth(prefixPart)`；
- 选中时 `gutter` 与 `body` 都加 `theme.bg("selectedBg", ...)`；`bodyWidth = visibleWidth(body)`。
- 末尾 `` theme.fg("muted", `  (${selectedIndex+1}/${filteredNodes.length})${statusLabels}`) ``。

`getStatusLabels()`：`no-tools → " [no-tools]"`、`user-only → " [user]"`、`labeled-only → " [labeled]"`、`all → " [all]"`；`showLabelTimestamps` 追加 `" [+label time]"`。

`getEntryDisplayText(node, isSelected)`（**逐字**）：
| entry | 文本 |
|---|---|
| message/user | `accent("user: ")` + normalize(content) |
| message/assistant 有文本 | `success("assistant: ")` + normalize(text) |
| assistant 无文本且 `stopReason==="aborted"` | `success("assistant: ")` + `muted("(aborted)")` |
| assistant 有 errorMessage | `success("assistant: ")` + `error(normalize(errorMessage).slice(0,80))` |
| assistant 其他 | `success("assistant: ")` + `muted("(no content)")` |
| toolResult（有 toolCall） | `muted(formatToolCall(name,args))` |
| toolResult（无） | `muted(`[${toolName ?? "tool"}]`)` |
| bashExecution | `dim(`[bash]: ${normalize(command)}`)` |
| 其他 role | `dim(`[${role}]`)` |
| custom_message | `customMessageLabel(`[${customType}]: `)` + normalize(content) |
| compaction | `borderAccent(`[compaction: ${Math.round(tokensBefore/1000)}k tokens]`)` |
| branch_summary | `warning("[branch summary]: ")` + normalize(summary) |
| model_change | `dim(`[model: ${modelId}]`)` |
| thinking_level_change | `dim(`[thinking: ${thinkingLevel}]`)` |
| custom | `dim(`[custom: ${customType}]`)` |
| context_edit | `dim(`[context ${omit|replace}: ${targetId}]`)` |
| label | `dim(`[label: ${label ?? "(cleared)"}]`)` |
| session_info | `[title: ` + (name 或 `italic(dim("empty"))`) + `]` |
| default | `""` |
选中时整行 `theme.bold(result)`。

`formatLabelTimestamp(ts)`：同日 → `HH:MM`；同年 → `M/D HH:MM`；否则 `YY/M/D HH:MM`（不补零，除时分）。

`formatToolCall(name,args)`（**逐字**）：
```
read  → `[read: ${path}${formatReadLineRange(offset, limit)}]`
write → `[write: ${path}]`
edit  → `[edit: ${path}]`
bash  → `[bash: ${cmd.slice(0,50)}${raw.length>50?"...":""}]`   // cmd = raw.replace(/[\n\t]/g," ").trim()
grep  → `[grep: /${pattern}/ in ${path ?? "."}]`
find  → `[find: ${pattern} in ${path ?? "."}]`
ls    → `[ls: ${path ?? "."}]`
default → `[${name}: ${JSON.stringify(args).slice(0,40)}${len>40?"...":""}]`
```
路径 `shortenPath`：`HOME || USERPROFILE` 前缀换成 `~`。

`handleInput` 顺序（极长，必须照抄）：up / down（环绕）、`app.tree.foldOrUp`（可折叠且未折叠 → 折叠；否则 `findBranchSegmentStart("up")`）、`app.tree.unfoldOrDown`（已折叠 → 展开；否则 `findBranchSegmentStart("down")`）、`tui.editor.cursorLeft || tui.select.pageUp` → `-maxVisibleLines`、`tui.editor.cursorRight || tui.select.pageDown` → `+maxVisibleLines`、confirm → onSelect、`app.message.copy` → `copySelected()`、cancel（有搜索词 → 清空搜索与折叠；否则 onCancel）、5 个 filter 直接/切换键、`app.tree.filter.cycleBackward|cycleForward`、`tui.editor.deleteCharBackward`（删搜索末字符）、`app.tree.editLabel`、`app.tree.toggleLabelTimestamp`、最后：无控制字符且非空 → 追加到 `searchQuery`。

`isFoldable(id)`：有可见子节点且（无可见父 **或** 父有多个可见子节点）。
`findBranchSegmentStart(direction)`：`down` 沿第一个子节点下行直到分叉或叶；`up` 沿父链上行，遇到父有多个子且自身索引 < 当前选中索引时返回自身索引，否则继续上行；到根返回当前索引。

**TREE_HELP_ITEMS**（顺序即渲染顺序）：
```
[{up,down},"move"], [{cursorLeft,cursorRight},"page"], [{foldOrUp,unfoldOrDown},"branch"],
[copy,"copy"], [editLabel,"label"], [toggleLabelTimestamp,"label time"],
[{filter.default,filter.noTools,filter.userOnly,filter.labeledOnly,filter.all},"filters",labelFirst],
[{filter.cycleForward,filter.cycleBackward},"cycle",labelFirst]
```
`formatHelpKeys`：每项取 `getKeys(binding)[0]`，`compactRawKeys` 后 `formatKeyText`，再做替换：`pageUp→pgup`、`pageDown→pgdn`、`up→↑`、`down→↓`、`left→←`、`right→→`（均为词边界正则）。
`compactRawKeys`：单键原样；多键若公共前缀（最后一个 `+` 之前）相同则 `${prefix}${后缀用/连接}`，否则 `keys.join("/")`。

**TreeHelp.render**：按 `" · "` 拼接，`"  "` 缩进，超过宽度用 `wrapTextWithAnsi` 换行；全部 `theme.fg("muted", ...)`。
**SearchLine.render**：`` `  ${fg("muted","Type to search:")} ${fg("accent", query)}` `` 或只有前半段。
**LabelInput**：`"  "` 缩进，标题 `Label (empty to remove):`，`input.render(width-2)` 每行加缩进，hint `` keyHint("tui.select.confirm","save") + "  " + keyHint("tui.select.cancel","cancel") ``。confirm 时 `value.trim()`，空 → `undefined`。

**TreeSelectorComponent** 布局：`Spacer(1)` → `DynamicBorder()` → `Text(theme.bold("  Session Tree"),1,0)` → `TreeHelp()` → `SearchLine(treeList)` → `DynamicBorder()` → `Spacer(1)` → `treeContainer` → `labelInputContainer` → `Spacer(1)` → `DynamicBorder()`。`maxVisibleLines = Math.max(5, Math.floor(terminalHeight / 2))`。`tree.length === 0` → `setTimeout(onCancel, 100)`。

测试（`tree-selector.test.ts`，**实测 18 条**，全部在 `describe` 内）：`focuses nearest visible ancestor when currentLeafId is a model_change with sibling branch`、`hides context edits by default and labels them in all mode`、`focuses nearest visible ancestor when currentLeafId is a thinking_level_change entry`、`switches to nearest visible user message when changing to user-only filter`、`returns to nearest visible ancestor when switching back to default filter`、`renders semantic help rows without truncating narrow terminal controls`、`copies the full selected message with ctrl+x`、`toggles label timestamps for labeled nodes`、`preserves selection when switching to empty labeled filter and back`、`preserves selection through multiple empty filter switches`、`ctrl+right unfolds a folded node, then does segment jump when unfolded`、`alt+left/right are aliases for fold and unfold navigation`、`folding root hides entire subtree, nested fold preserved on unfold`、`fold and navigate on non-active branch`、`fold and navigate with multiple roots`、`folding root hides descendants even when intermediate nodes are filtered out`、`search resets fold state`、`filter mode change resets fold state`。（**注意**：`horizontal viewport 平移` 与 `active path 标记` **没有独立测试**；不存在"19 个用例"之说。）

### 2.45 `components/config-selector.ts`（951 行）

```ts
export type ScopedResolvedPaths = Record<"global" | "project", ResolvedPaths>;
export class ConfigSelectorComponent extends Container implements Focusable {
  constructor(resolvedPaths: ScopedResolvedPaths, settingsManager: SettingsManager, cwd: string, agentDir: string,
              onClose: () => void, onExit: () => void, requestRender: () => void,
              terminalHeight?: number, writeScope: "global" | "project" = "global",
              projectModeAvailable = true);
  getResourceList(): ResourceList;
}
```
常量与结构：`RESOURCE_TYPES = ["extensions","skills","prompts","themes"]`；`RESOURCE_TYPE_LABELS = {extensions:"Extensions", skills:"Skills", prompts:"Prompts", themes:"Themes"}`。

`formatBaseDir(baseDir)`：`=== homedir` → `"~"`；以 homedir 开头 → `` `~${rest.replace(/\\/g,"/")}` ``；否则 `baseDir.replace(/\\/g,"/")`；末尾补 `/`。

`getGroupLabel(metadata, agentDir)`（**逐字**）：
- `origin === "package"` → `` `${metadata.source} (${metadata.scope})` ``
- `source === "builtin"` → `scope==="user" ? "Built-in" : "Built-in (project override)"`
- `source === "auto"`：有 `baseDir` → `User (<baseDir/>)` 或 `Project (<baseDir/>)`；否则 `User (<agentDir/>)` 或 `` `Project (${CONFIG_DIR_NAME}/)` ``
- 否则 → `"User settings"` / `"Project settings"`

`buildGroups`：按 `groupKey = `${origin}:${scope}:${source}:${baseDir ?? ""}`` 分组；子组按 resourceType；显示名规则：
- `builtin` → `path.slice(BUILTIN_PATH_PREFIX.length)`
- `extensions` 且父目录名不是 `extensions` → `` `${parentFolder}/${fileName}` ``
- `skills` 且文件名 `SKILL.md` → 父目录名
- 否则文件名
排序：package 先于 top-level；user 先于 project；同层按 `source.localeCompare`；子组按类型顺序（extensions, skills, prompts, themes）；条目按 `displayName.localeCompare`。

`ConfigSelectorHeader.render(width)`：
```
title = theme.bold(writeScope==="project" ? "Project Local Resources" : "Global Resources")
sep = fg("muted"," · ")
switchHint = projectModeAvailable ? keyHint("tui.input.tab","switch mode") + sep : ""
actionHint = writeScope==="project" ? rawKeyHint("space","cycle inherit/+/-") : rawKeyHint("space","toggle")
hint = switchHint + actionHint + sep + rawKeyHint("esc","close")
spacing = max(1, width - visibleWidth(title) - visibleWidth(hint))
scopeHint = writeScope==="project"
  ? fg("muted", `${CONFIG_DIR_NAME}/settings.json · inherited global resources are dimmed`)
  : fg("muted", `~/${CONFIG_DIR_NAME}/agent/settings.json`)
返回 [truncateToWidth(`${title}${" ".repeat(spacing)}${hint}`, width, ""), truncateToWidth(scopeHint, width, "")]
```

`ResourceList`：`chrome = 8`，`maxVisible = Math.max(5, (terminalHeight ?? 24) - chrome)`。`buildFlatList` 把 group / subgroup / item 依次压平，初始选中第一个 item。
渲染（**逐字**）：
- group 行：`inherited = writeScope==="project" && group.scope==="user"`；`label = theme.bold(`${group.label}${inherited ? " · inherited global" : ""}`)`；`truncateToWidth(`  ${fg(inherited?"dim":"accent", label)}`, width, "")`
- subgroup 行：颜色 `writeScope==="project" && group.scope==="user" ? "dim" : "muted"`；`truncateToWidth(`    ${coloredLabel}`, width, "")`
- item 行：`cursor = selected ? "> " : "  "`；`dimmed = isDimmedItem(item)`；名称选中且未 dim 时加粗；`` truncateToWidth(`${cursor}    ${checkbox} ${name}${suffix}`, width, "...") ``
- 滚动：`` fg("dim", `  (${currentItemIndex}/${itemCount})`) ``（按 item 计数）
- 空：`fg("muted","  No resources found")`

复选框（**逐字**）：project 模式 `load → fg("success","[+]")`、`unload → fg("warning","[-]")`、inherit → `fg("dim", enabled ? "[x]" : "[ ]")`；global 模式 `enabled ? fg("success","[x]") : fg("dim","[ ]")`。
后缀：project 模式 `load → fg("muted","  project load")`、`unload → fg("muted","  project unload")`、继承全局 → `fg("dim","  inherited global")`。

`handleInput`：up/down 用 `findNextItem`（只落在 item 上）；pageUp/pageDown（先跳 maxVisible 再找最近 item）；cancel → onClose；`ctrl+c` → onExit；`tui.input.tab` → onSwitchMode；`" "` 或 confirm → 若为 item 且（project 模式或 item 属 user scope）→ 切换并 `updateItem` + `onToggle`；其他 → 搜索框。

开关逻辑（不改行为，但需要完整照抄的语义）：
- global：top-level → 在对应 settings 数组里剔除以 `!`/`+`/`-` 开头的同 pattern 项，再 push `+pattern` / `-pattern`；package → 在 `packages` 对应对象里做同样处理，空则删除该 key，若四种资源都空则把对象还原成字符串 `source`。
- project：三态循环 `inherit → unload|load`（取决于继承值）、`unload → load|inherit`、`load → inherit|unload`；top-level 会先写项目条目（继承来的文件在非 inherit 状态下需显式列出 pattern，builtin 除外），再写 `+pattern` / `-pattern`；package 用 `{source, autoload:false}` 形式创建覆盖项。
- pattern 生成：builtin → `path`；否则相对 `metadata.baseDir ?? getTopLevelBaseDir(scope)`；package → 相对 `metadata.baseDir ?? dirname(path)`。

**ScopedResolvedPaths** 里的 `ResolvedResource`/`PathMetadata` 由 `ca-resources` 单元提供（见 §5）。

### 2.46 `components/armin.ts`（384 行）

```ts
export const ARMIN_WIDTH = 31;
export const ARMIN_HEIGHT = 36;
export function isArminPixel(x: number, y: number): boolean;
export class ArminComponent implements Component {
  constructor(ui: TUI);
  invalidate(): void;
  render(width: number): string[];
  dispose(): void;
}
```
- XBM 位图按 LSB 优先，`isArminPixel = ((BITS[y * BYTES_PER_ROW + floor(x/8)] >> (x % 8)) & 1) === 0`（**0 = 前景**）；`y >= HEIGHT` → false。`BITS` 数组见源文件（必须逐字节照抄，共 144 字节）。
- `DISPLAY_HEIGHT = ceil(36/2) = 18`；`getChar` 上下两像素打包：`█`（both）、`▀`（upper）、`▄`（lower）、空格。
- `Math.random()` 从 7 种效果中选一种：`typewriter | scanline | rain | fade | crt | glitch | dissolve`。
- 动画帧率：`glitch` 60fps，其余 30fps；每帧 `tickEffect()` → `updateDisplay()`（`gridVersion++`）→ `ui.requestRender()`；完成则 `stopAnimation()`。
- 各效果参数（**逐字**）：typewriter 3 px/帧；scanline 1 行/帧；rain 每列随机初始 `y = -floor(random*36)`、`settled` 从底部向上；落点 `▓`；settle 后 `drop.y = -floor(random*5)-1`；fade 15 px/帧（Fisher-Yates 打乱位置）；crt 从中间行向上下扩张，`expansion > 18` 结束；glitch 8 帧乱序（每行 `offset = floor(random*7)-3`、30% 水平位移、20% 交换到随机行）；dissolve 20 px/帧，初始随机 `[" ","░","▒","▓","█","▀","▄"]`。
- `render(width)`：按 `(width, gridVersion)` 缓存；`padding = 1`，每行 ` ${theme.fg("accent", row.slice(0, width-1).join(""))}${" ".repeat(max(0, width-1-clipped.length))}`；最后追加 ` "ARMIN SAYS HI"`（同样补空格）。**【逐字】**

### 2.47 `components/easter-egg-3d.ts`（1292 行）+ `easter-egg-3d.lazy.ts`（27 行）

**lazy 入口**
```ts
function playEasterEgg3d(tui: TUI, egg: EasterEgg3d): boolean {
  if (!(tui instanceof TuiAltScreen)) return false;
  if (tui.hasOverlay()) return true;
  const screen = tui.getScreenLines();
  import("./easter-egg-3d.ts").then(m => m.playEasterEgg3d(tui, screen, egg), () => {});
  return true;
}
export function playKianaLogo3d(tui: TUI, column: number, row: number): void;
export function playArmin3d(tui: TUI): boolean;
```

**主模块**
```ts
export type EasterEgg3d = { kind: "kiana-logo"; column: number; row: number } | { kind: "armin" };
export async function playEasterEgg3d(tui: TUI, screen: readonly string[], egg: EasterEgg3d): Promise<void>;
export class EasterEgg3dAnimation implements Component {
  constructor(tui, screen, model, colors: {foreground: Rgb; background: Rgb}, onDone: () => void);
  close(): void;
  handleInput(data: string): void;
  handleMouse(event: TuiMouseEvent): TuiMouseEventResult;
  invalidate(): void;
  render(width: number): string[];
}
```
常量（**全部逐字**）：
```ts
const DEPTH = 0.7; const FLY_START = 0.1; const FLY_DURATION = 1.3; const START_SCALE = 0.1;
const CORAL=[228,138,122]; const BLUE=[79,142,179]; const YELLOW=[234,182,93];
const KIANA_LOGO_PIXELS = ["ccc.", "b.c.", "bb.y", "b..y"];
const KIANA_LOGO_COLORS = { c: CORAL, b: BLUE, y: YELLOW };
const PUZZLE_START = FLY_START + FLY_DURATION + 3;   // 4.4
const PUZZLE_STEP = 0.3; const PUZZLE_STEPS = 12; const PUZZLE_RETURN = 1; const PUZZLE_HOLD = 0.6;
const PUZZLE_CYCLE = PUZZLE_STEP * PUZZLE_STEPS + PUZZLE_RETURN + PUZZLE_HOLD;   // 5.2
const FRAME_MS = 1000 / 30; const DUST_DURATION = 0.35; const WAVE_SPREAD = 0.55; const WAVE_JITTER = 0.12;
const DISSOLVE_END = FLY_START + WAVE_SPREAD + WAVE_JITTER + DUST_DURATION;      // 1.12
const BACKGROUND_FADE = 0.5; const EXIT_DURATION = 1.1;
const STARS_START = 2.5; const STARS_FADE = 2; const STAR_DENSITY = 0.018;
const LOGO_COLOR_STEP = 5; const LIGHT_HALO_STRENGTH = 0.3;
const HALO_RADIUS_X = 6; const HALO_RADIUS_Y = 3; const HALO_LEVELS = 32;
const STAR_ALPHA_LEVELS = 8; const STAR_FLICKER_LEVELS = 3; const HINT_FADE = 0.5;
const DOT_BITS = [0x01,0x08,0x02,0x10,0x04,0x20,0x40,0x80];
const BRAILLE = Array.from({length:256}, (_, bits) => String.fromCharCode(0x2800 + bits));
const LIGHT = normalize([-0.45,-0.6,0.75]); const HALF_VECTOR = normalize([LIGHT[0],LIGHT[1],LIGHT[2]+1]);
const SPIN_RAMP = 1.4; const SPIN_SPEED = (Math.KIANA*2)/PUZZLE_CYCLE; const SPIN_LINGER = 0.6;
const FIRST_FRONT_VIEW = PUZZLE_START + PUZZLE_STEP*PUZZLE_STEPS + PUZZLE_RETURN + PUZZLE_HOLD/2;
const SPIN_ALIGNMENT = Math.KIANA*2 - (baseSpinPhase(FIRST_FRONT_VIEW) % (Math.KIANA*2));
```
数学工具（**逐字**；旧版规格把 `smooth` 写成三次 smoothstep，**是错的**）：
```ts
/** Quintic smoothstep. */
function smooth(value: number): number {
    const t = clamp01(value);
    return t * t * t * (t * (t * 6 - 15) + 10);
}
function easeOut(value: number): number { const t = 1 - clamp01(value); return 1 - t * t * t; }
function mix(a: Rgb, b: Rgb, amount: number): Rgb {
    return [a[0] + (b[0]-a[0])*amount, a[1] + (b[1]-a[1])*amount, a[2] + (b[2]-a[2])*amount];
}
function hash(value: number): number {
    let x = Math.imul(value ^ 0x9e3779b9, 0x85ebca6b);
    x ^= x >>> 13;
    x = Math.imul(x, 0xc2b2ae35);
    x ^= x >>> 16;
    return (x >>> 0) / 0x100000000;
}
function isLight([r, g, b]: Rgb): boolean { return 0.2126*r + 0.7152*g + 0.0722*b > 128; }
```
其余工具：`normalize`（`Math.hypot` 单位化）、`clamp01`、`toRgb(color)`（`colorToRgb` → 三元组）、`glyphInk(text, seed)`（字符 → 盲文点数量）、`parseScreen`（把屏幕行解析成 `ScreenCell[][]`，含 ANSI 前景/背景还原与宽字符处理，`Intl.Segmenter` 分段）、`rotation(yaw,pitch,roll)` / `multiply(a,b)`（3x3 矩阵）、`baseSpinPhase(time)` / `spinPhase` / `spinAngle`、`shuffleSteps(model, cycle)`、`BlockRaster`（按盲文点光栅化方块、Z 缓冲、halo）。

**`Model` 接口与 `createModel`（旧版规格完全漏掉）**：
```ts
interface Model {
  columns: number; rows: number;
  blocks: Array<{ home: Cell3; color: Rgb }>;
  cameraDistance: number;   // 相机到模型中心的像素距离
  radius: number;           // 任何方块角到中心的最远距离（含外层深度）
  widthShare: number;       // 旋转模型占屏宽的最大比例
  puzzleMoves: (random: number) => number;
  origin: { column: number; row: number } | undefined;   // undefined = 从中心长出
}
function createModel(columns, rows, pixel: (column,row) => Rgb | undefined,
                     options: Pick<Model,"cameraDistance"|"widthShare"|"origin"> &
                              { puzzleMoves: (random: number, blockCount: number) => number }): Model
// 块按 row 外层、column 内层扫描，pixel 返回非 undefined 才生成 { home:[column,row,0], color }
// radius = Math.hypot(columns/2, rows/2, 1 + DEPTH/2)
```
两个模型（**逐字**，旧版规格漏掉）：
```ts
function kianaLogoModel(origin): Model {
  return createModel(4, 4, (column,row) => KIANA_LOGO_COLORS[KIANA_LOGO_PIXELS[row]![column]!], {
    cameraDistance: 10, widthShare: 0.35,
    puzzleMoves: (random) => (random < 0.4 ? 2 : 1),      // 40% 概率一步走 2 块
    origin,
  });
}
function arminModel(color: Rgb): Model {
  return createModel(ARMIN_WIDTH, ARMIN_HEIGHT, (column,row) => (isArminPixel(column,row) ? color : undefined), {
    // 相机足够远，约 36 块高的角色透视才温和
    cameraDistance: 80, widthShare: 0.45,
    puzzleMoves: (random, blockCount) => Math.round(blockCount * (0.03 + random * 0.04)),
    origin: undefined,
  });
}
```
`ARMIN_WIDTH`/`ARMIN_HEIGHT`/`isArminPixel` 来自 `components/armin.ts`（本单元内，见 §2.46）。
`PUZZLE_MOVES: Cell3[]`（**逐字**，10 项，**顺序有意义**，候选按此顺序枚举）：
```ts
const PUZZLE_MOVES: Cell3[] = [[1,0,0],[-1,0,0],[0,1,0],[0,-1,0],[1,0,0],[-1,0,0],[0,1,0],[0,-1,0],[0,0,1],[0,0,-1]];
```
`STAR_TINTS: Rgb[]`（**逐字**）：`[[255,255,255],[170,195,255],[255,225,170],[255,190,205]]`（中性白、淡蓝、淡金、淡玫瑰；每颗星再与终端前景各混一半）。

`shuffleSteps(model, cycle)` 的**精确算法**（伪随机必须逐位一致）：
```
random = cycle * 7_919 + 1; next = () => hash(random++)
key([x,y,z]) = ((z + 1) * rows + y) * columns + x
对 step ∈ [0, PUZZLE_STEPS):
  moveCount = model.puzzleMoves(next())
  重复 moveCount 次：
    枚举所有未移动块 × PUZZLE_MOVES：跳过越界 (x∈[0,columns), y∈[0,rows), z∈[-1,1]) 与已占用目标
    fresh = 候选里块不在 lastMoved；pool = fresh.length > 0 ? fresh : candidates
    choice = pool[floor(next() * pool.length)]；无则 break
    标记 occupied / nextPositions / moved
  lastMoved = moved（上一轮刚动过的，下轮降权）
返回 steps（steps[0] = 全部 home，共 PUZZLE_STEPS+1 项）
```
注释原文："Prefer blocks that did not just move, so the puzzle does not look like one block jittering."

`playEasterEgg3d(tui, screen, egg)`：
1. `if (playing) return;`（模块级 `playing` 标志）
2. `await tui.queryTerminalColors({ timeoutMs: 100 })`
3. `dark = theme.appearance === "dark"`；`foreground` 未取到 → `toRgb(theme.colors.text)`；`background` 未取到 → `dark ? [0,0,0] : [255,255,255]`
4. `if (tui.hasOverlay()) { playing = false; return; }`
5. model：armin → `arminModel(toRgb(theme.colors.accent))`；kiana-logo → `kianaLogoModel({column,row})`
6. `new EasterEgg3dAnimation(tui, screen, model, colors, () => { playing = false; overlay.hide(); })`
7. `tui.showOverlay(animation, { anchor: "top-left", width: "100%", maxHeight: "100%" })`

`EasterEgg3dAnimation` 关键点：
- 构造即 `setInterval(..., FRAME_MS)`，`unref?.()`；每帧：退出动画完成或 `performance.now() - lastRender > 1000` → `finish()`；否则 `tui.requestRender()`。
- `close()`：第二次调用直接 `finish()`；否则记录 `{start, time: elapsed(), yaw: spinAngle(time), targetYaw: ceil(yaw/2π)*2π, offsets: blockOffsets(time)}`。
- `handleInput`：`tui.select.cancel` 或 `app.clear` → `close()`。
- `handleMouse`：click → `close()`；**始终**返回 `{handled:true, render:false}`。
- `render(width)`：`height = max(1, tui.terminal.rows)`；尺寸变化时重建 `cells` 与 `stars`；按 exit/entry 两条路径计算 `dissolveTime/pose/hintAlpha/starAlpha/offsets`；构造 boxes → `raster.render(...)` → 逐 cell `emit(text, fg, bg)`（ANSI 状态机：`\x1b[39m` / `\x1b[49m` 复位），行尾追加 `\x1b[0m`。
- 提示行：`key = formatKeyText(getKeybindings().getKeys("tui.select.cancel")[0] ?? "escape")`；`text = `${key} to return`；位于 `height - 2` 行，水平居中；`keyColor` 用 `muted`、`color` 用 `dim` 与背景混合。
- 星场：`hash(index*3 + 0x51ed) < STAR_DENSITY` 时生成星，`random(salt) = hash(index*7 + salt*0x9e37)`。
- `prepareCells` 里 `origin` 存在时把 header logo 占用的半块单元格清空（`text=" "、width=1、ink=0、bg=undefined`）。

> **移植难度极高**：整段是浮点数学 + 光栅化 + 动画时间线，必须逐常数、逐公式照抄，且依赖 `performance.now()` 与 30fps 定时器。建议单列一个 `interactive::easter_egg_3d` 模块并在 Rust 中同样用 `std::f64` 与显式常量表。

### 2.48 `bug-report.ts`（298 行）

```ts
interface BugReportContext { session; ui: TUI; editorContainer: Container; editor: EditorComponent;
                             keybindings; showStatus(msg); showError(msg) }
interface BugReportOptions { hint?: string; includeSession: boolean; includeSummary: boolean;
                             delivery: "upload" | "zip" }
export async function reportBug(context: BugReportContext, initialHint?: string): Promise<void>;
```
**逐字常量**：
```ts
const DISCLAIMER = "This report goes to the Kiana developers (Earendil) and is not shared publicly. It includes your kiana version, operating system, the current model and provider configuration (without API keys), loaded extensions, settings, and provider error diagnostics from this session.";
const TRANSCRIPT_NOTE = "The transcript contains your messages, model output, tool calls and their results, including file contents and command output read during this session.";
```
流程：
1. `promptForOptions`：
   - `input(context, "Report a bug", `${DISCLAIMER}\n\nWhat went wrong? (optional)`, initialHint)`；`null` → 取消。
   - `choose("Include the session transcript?", ["Yes, include the transcript", "No"], TRANSCRIPT_NOTE)`；空 → 取消；`includeSession = transcript !== "No"`。
   - 若不含 transcript：`choose(`Attach a summary written by ${model?.name ?? "the current model"} instead?`, ["Yes, generate a summary","No"], "The transcript is sent to ${model?.provider ?? "your provider"} with your credentials and tokens. Only the generated summary is attached; the transcript stays on your machine.")`；空 → 取消；`includeSummary = summary !== "No"`。
   - `description = hint.trim()`。
   - `choose("Bug report", ["Upload Report","Export as Zip","Cancel"], `Description: ${description || "none"}\nTranscript: ${includeSession ? "included" : "not included"}\nSummary: ${includeSummary ? `written by ${model?.name ?? "the current model"}` : "none"}\n\nUpload sends the report to ${new URL(getRadiusGatewayUrl()).host}. Export writes a zip archive to the current directory instead.`)`。
2. 取消或 `delivery === "upload" && process.env.KIANA_OFFLINE` → `showError("Uploading bug reports requires online mode. Use Export as Zip instead.")`。
3. 需 summary：loader `` `Writing summary with ${model?.name ?? "the current model"}...` ``；`session.summarizeForBugReport({hint, signal})`；失败 → `Failed to write bug report summary: ${msg}`；aborted → `"Bug report cancelled"`。
4. `buildBundle` 失败 → `Failed to build bug report: ${msg}`。
5. upload 失败 → `choose("Upload failed", ["Export as Zip","Cancel"], `${failure}\n\nExport the report as a zip archive instead?`)`；不是 Export 则 `"Bug report cancelled"`。
6. `exportZip`：`archivePath = path.join(process.cwd(), bugReportArchiveFileName(bundle.metadata.id))`；失败 → `Failed to write bug report: ${msg}`；成功 → `Bug report exported to: ${archivePath}\nReport ID: ${bundle.metadata.id}`；`recordInSession`（`appendCustomEntry(BUG_REPORT_CUSTOM_ENTRY_TYPE, {...})`，若有 crash 则 `clearCrashLog()`）。
7. upload 成功 → `Bug report uploaded. Report ID: ${result.id}`。

overlay 组件：`input` → `ExtensionEditorComponent`（描述为 `description`、外部编辑器命令 `settingsManager.getExternalEditorCommand()`）；`choose` → `ExtensionSelectorComponent(title, options, finish, () => finish(), {tui, description})`；`showLoader` → `BorderedLoader(ui, theme, message)`。`showOverlay`：`editorContainer.clear()` → addChild → `ui.setFocus` → `requestRender`；`restoreEditor`：`dispose?.()` → clear → addChild(editor) → setFocus → requestRender。

测试（`bug-report.test.ts`、`interactive-mode-bug-report-hint.test.ts`）。

### 2.49 `session-share.ts`（224 行）

```ts
interface SessionShareContext { session; ui; editorContainer; editor; showStatus; showError }
export function createShareTrailingEntries(session, parentId: string | null, timestamp: string): object[];
export function exportSessionForShare(filePath: string, session): void;
export async function shareSession(context): Promise<void>;
```
`createShareTrailingEntries` 返回单元素数组：
```json
{ "type": "custom", "customType": "kiana.share", "id": "<uuid 前 8 位>", "parentId": <parentId>,
  "timestamp": <timestamp>,
  "data": { "systemPrompt": <session.state.systemPrompt>,
            "tools": [{ "name", "description", "parameters" }] } }
```
`shareSession` 流程（**逐字字符串**）：
1. `tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "kiana-share-"))`；`session.jsonl`、`session.html`。
2. `exportSessionForShare` 失败 → `Failed to export session: ${message ?? "Unknown error"}`。
3. `destination = settingsManager.getSessionShareDestination()`；`!== "gist"` 时尝试 `tryShareViaRadius`；失败且 `destination === "radius"` → `Radius authentication is not configured. Run '/login radius' first.`
4. gist 前置检查：`spawnSync("gh", ["auth","status"])` 非 0 → `GitHub CLI is not logged in. Run 'gh auth login' first.`；抛异常 → `GitHub CLI (gh) is not installed. Install it from https://cli.github.com/`。
5. `session.exportToHtml(htmlFile, { themeName: theme.name })` 失败 → 同 2 的文案。
6. `shareViaGist`。
7. `finally` 递归删除 tempDir（忽略错误）。

`tryShareViaRadius`：
- provider `radius` 不存在 → false；token 取 `getAuthCredential(await modelRuntime.getAuth("radius", { minOAuthValidityMs: 5*60_000 }))`，无 → false。
- loader 文案 `"Uploading to Radius..."`；`onAbort` → 恢复编辑器 + `"Share cancelled"`。
- `url = new URL("/v1/artifacts", DEFAULT_RADIUS_GATEWAY)`；query `visibility=organization`、`title=Kiana session`。
- `POST`，headers：`Authorization: Bearer <token>`、`Content-Type: application/x-ndjson`、`Content-Length: <bytes>`，body 为 jsonl 字节，`signal: loader.signal`。
- aborted → 返回 true（静默）。
- 非 ok 或无 `json.artifact` → `Failed to upload Radius artifact: ${json?.error || response.statusText || response.status}`。
- 成功 → `Share URL: ${hyperlink(canonical_url, canonical_url)}`。
- 异常且未 abort → `Failed to upload Radius artifact: ${message ?? "Unknown error"}`。

`shareViaGist`：loader `"Creating gist..."`；`spawn("gh", ["gist","create","--public=false",tmpFile])`，收集 stdout/stderr，`onAbort` → `proc.kill()` + `"Share cancelled"`。
- 非 0 → `Failed to create gist: ${stderr.trim() || "Unknown error"}`。
- `gistId = url.split("/").pop()`；缺失 → `"Failed to parse gist ID from gh output"`。
- 成功 → `` `Share URL: ${hyperlink(previewUrl)}\nGist: ${hyperlink(gistUrl)}` ``，`previewUrl = getShareViewerUrl(gistId)`。

测试（`session-share.test.ts`）。

### 2.50 `interactive-mode.ts`（7105 行）—— 核心

#### 2.50.1 模块级导出与常量

```ts
export { createInteractiveTui, createInteractiveTuiReference } from "./tui-renderer.ts";
export function formatCrashExtensionHint(extensionMatches: readonly string[] | undefined): string | undefined;
export function formatResumeCommand(sessionManager: SessionManager): string | undefined;
export interface InteractiveModeOptions {
  migratedProviders?: string[];
  startupDiagnostics?: AgentSessionRuntimeDiagnostic[];
  modelFallbackMessage?: string;
  autoTrustOnReloadCwd?: string;
  initialMessage?: string;
  initialImages?: ImageContent[];
  initialMessages?: string[];
  verbose?: boolean;
  tuiMode?: TuiMode;
  initialThemeSetting?: string;
  terminal?: Terminal;
}
export class InteractiveMode { ... }
```

`formatCrashExtensionHint(matches)`（**逐字**）：
```
若 !Array.isArray 或 length === 0 → undefined
quoted = matches.map(m => `\`${m}\``)
labels = 1 个 → quoted[0]；2 个 → `${q0} and ${q1}`；3+ → `${q0}, ${q1}, and ${q2}...`（最后一项前加 "and"）
noun = length === 1 ? "extension" : "extensions"; pronoun = length === 1 ? "it" : "them"
返回 `A stack frame came from loaded ${noun} ${labels}, which may be involved. Try disabling ${pronoun} with \`${APP_NAME} config\`, or run \`${APP_NAME} -ne\` to confirm.`
```

其他模块级常量：
```ts
const DEAD_TERMINAL_ERROR_CODES = new Set(["EIO","EPIPE","ENOTCONN","ENOTTY"]);
const ANTHROPIC_SUBSCRIPTION_AUTH_WARNING =
  "Anthropic subscription auth is active. Third-party harness usage draws from extra usage and is billed per token, not your Claude plan limits. Manage extra usage at https://claude.ai/settings/usage. Disable this warning in /settings.";
const RADIUS_LOGIN_INTRO = "Radius is a service crafted for Kiana by the builders of Kiana, Earendil Works";
const AUTH_TYPE_ORDER = { oauth: 0, api_key: 1 };
type CompactionQueuedMessage = { text: string; mode: "steer" | "followUp" };
type CompactionCostNotice = { type: "compaction_cost"; kind: "compaction" | "branch_summary"; usage: Usage };
type RenderSessionItem = AgentMessage | Extract<SessionEntry, {type:"custom"|"usage"}> | CompactionCostNotice;
```

`isAnthropicSubscriptionAuthKey(key) = typeof key === "string" && key.startsWith("sk-ant-oat")`
`isUnknownModel(model) = !!model && model.provider === "unknown" && model.id === "unknown" && model.api === "unknown"`
`quoteIfNeeded(value)`：`value.length > 0 && !/[^a-zA-Z0-9_\-./~:@]/.test(value)` → 原样；否则 `'${value.replace(/'/g, `'\\''`)}'`
`formatResumeCommand(sessionManager)`（**逐字**）：
```
if (!process.stdout.isTTY) return undefined
if (!sessionManager.isPersisted()) return undefined
sessionFile = sessionManager.getSessionFile(); if (!sessionFile || !fs.existsSync(sessionFile)) return undefined
args = [APP_NAME]
if (!sessionManager.usesDefaultSessionDir()) args.push("--session-dir", quoteIfNeeded(sessionManager.getSessionDir()))
args.push("--session", sessionManager.getSessionId())
return args.join(" ")
```

`ExpandableText` / `BuiltInHeader` / `IdleStatus` 等内部类：
```ts
class ExpandableText extends ThemedText implements Expandable {
  constructor(getCollapsedText, getExpandedText, expanded = false, paddingX = 0, paddingY = 0)
  setExpanded(expanded: boolean): void
}
class BuiltInHeader extends ExpandableText {
  onLogoClick: ((column: number, row: number) => void) | undefined;
  handleMouse(event: TuiMouseEvent): TuiMouseEventResult | undefined {
    if (event.type !== "click" || event.y > 1 || event.x < 1 || event.x > 4 || !this.onLogoClick)
      return undefined;
    this.onLogoClick(event.screenX - event.x + 1, event.screenY - event.y);
    return { handled: true };
  }
}
```
（**逐字**：命中条件是 `type === "click"` 且 `y <= 1`（无 `y >= 0` 下界）且 `1 <= x <= 4` 且 `onLogoClick` 已注册；**不检查 `event.button`**。回调参数是**屏幕绝对坐标**换算出的 logo 左上角：`column = screenX - x + 1`、`row = screenY - y`，**不是** `(event.x, event.y)`。命中后返回 `{ handled: true }`（不再是 `undefined`）。）

#### 2.50.2 字段清单（Rust 结构体需要一一对应）

| 字段 | 类型/初值 |
|---|---|
| `runtimeHost` | `AgentSessionRuntime` |
| `renderer` | `TuiMainScreen \| TuiAltScreen` |
| `ui` | `TUI` |
| `mainScreenRenderState` | `TuiMainScreenRenderState \| undefined` |
| `loadedResourcesContainer`, `chatContainer`, `documentContainer`, `pendingMessagesContainer`, `statusContainer`, `editorContainer`, `footerContainer`, `headerContainer`, `widgetContainerAbove`, `widgetContainerBelow` | `Container` |
| `transcriptScrollView` | `TuiLayouts.ScrollView \| undefined` |
| `fullscreenLayoutRoot` | `Component \| undefined` |
| `defaultEditor` | `CustomEditor` |
| `editor` | `ActiveEditor`（当前可能被扩展替换） |
| `editorComponentFactory` | `EditorFactory \| undefined` |
| `autocompleteProvider`, `autocompleteProviderWrappers` | `AutocompleteProvider \| undefined`、`AutocompleteProviderFactory[]` |
| `fdPath` | `string \| undefined` |
| `activeSelectorToken`, `activeSelectorDispose` | `object \| undefined`、`() => void \| undefined` |
| `footer` / `footerDataProvider` | `FooterComponent` / `FooterDataProvider` |
| `keybindings` | `KeybindingsManager` |
| `version` | `string` |
| `isInitialized`, `onInputCallback?`, `pendingUserInputs` | `bool`、`Option<fn>`、`Vec<String>` |
| `activeStatusIndicator`, `activeWorkingIndicatorEmbedded`, `idleStatus` | 状态指示器 / `IdleStatus` |
| `workingMessage`, `workingVisible`, `workingIndicatorOptions`, `defaultWorkingMessage = "Working"` | |
| `defaultHiddenThinkingLabel = "Thinking..."`, `hiddenThinkingLabel` | |
| `lastSigintTime`, `lastEscapeTime` | `f64`（ms） |
| `changelogMarkdown`, `startupNoticesShown`, `anthropicSubscriptionWarningShown` | |
| `lastStatusSpacer`, `lastStatusText`, `lastStatusMessage`, `managedToolStatusStarted` | |
| `streamingComponent`, `entriesRenderedByBoundaryCompaction: Set<string>`, `streamingMessage` | |
| `pendingTools = Map<string, ToolExecutionComponent>` | |
| `toolOutputExpanded`, `hideThinkingBlock`, `outputPad = 1` | |
| `mermaidMarkdownTransformer` | `createMermaidMarkdownTransformer({ getMode: () => this.settingsManager.getMermaidRenderingMode(), theme })` |
| `skillCommands = Map<string,string>` | 命令名 → skill 文件路径 |
| `unsubscribe?`, `signalCleanupHandlers: (()=>void)[]` | |
| `isBashMode`, `bashComponent`, `pendingBashComponents` | |
| `autoCompactionEscapeHandler?`, `retryEscapeHandler?` | |
| `compactionQueuedMessages: CompactionQueuedMessage[]` | |
| `shutdownRequested` | |
| `programStatus` | `new ProgramStatusReporter(...)` |
| `bugReportHintShown`, `installChangeWarningShown` | |
| `extensionDialogs: {component, blocked}[]` | 对话框栈 |
| `extensionTerminalInputSubscriptions: Set<...>` | |
| `extensionWidgetsAbove` / `extensionWidgetsBelow` | `Map<string, Component & {dispose?}>` |
| `customFooter`, `builtInHeader`, `customHeader` | |
| `options: InteractiveModeOptions` | |
| `autoTrustOnReloadCwd?` | |
| `themeController: InteractiveThemeController` | |
| `MAX_WIDGET_LINES = 10`（`static readonly`） | |

Getter：`session` → `runtimeHost.session`；`agent` → `session.agent`；`sessionManager` → `session.sessionManager`；`settingsManager` → `session.settingsManager`。

#### 2.50.3 构造与 `init()`

构造函数（610–676 行），**顺序敏感**，逐条如下：
1. `this.runtimeHost = runtimeHost`；`setCapabilityOverrides(this.settingsManager.getTerminalCapabilityOverrides())`（**构造期就调用一次**，不是只在 `init()` 里）。
2. `tuiMode = options.tuiMode ?? this.settingsManager.getTuiMode()`；`this.options = { ...options, tuiMode }`；`this.autoTrustOnReloadCwd = options.autoTrustOnReloadCwd`。
3. `runtimeHost.setBeforeSessionInvalidate(() => this.resetExtensionUI())`；`runtimeHost.setRebindSession(async () => { await this.rebindCurrentSession({ renderBeforeBind: true }); this.themeController.applyFromSettings(); })`。
4. `this.version = VERSION`；`this.renderer = createInteractiveTui({ tuiMode, showHardwareCursor, logDirectory: getAgentDir(), terminal: options.terminal, onRightClickPaste: this.onRightClickPaste, fullscreenCopyOnSelect, fullscreenWheelScrollLines })`；`this.ui = createInteractiveTuiReference(() => this.renderer)`；`this.ui.setClearOnShrink(settingsManager.getClearOnShrink())`。
5. 容器与挂载关系：`headerContainer`、`loadedResourcesContainer`、`chatContainer` 依次加入 `documentContainer`；另有 `pendingMessagesContainer`、`statusContainer`、`widgetContainerAbove`、`widgetContainerBelow`、`editorContainer`（内含 editor）、`footerContainer`（内含 footer）。
6. `this.keybindings = KeybindingsManager.create()`；`setKeybindings(this.keybindings)`（模块级单例注入）。
7. `const editorPaddingX = getEditorPaddingX(); const autocompleteMaxVisible = getAutocompleteMaxVisible();`
   `this.defaultEditor = new CustomEditor(this.ui, getEditorTheme(), this.keybindings, { paddingX: editorPaddingX, autocompleteMaxVisible, embedWorkingStatus: true })`；`this.editor = this.defaultEditor`；`editorContainer.addChild(this.editor)`。
   （旧版规格漏了 `autocompleteMaxVisible` 选项。）
8. `this.footerDataProvider = new FooterDataProvider(this.sessionManager.getCwd())`；`this.footer = new FooterComponent(this.session, this.footerDataProvider)`；`this.footer.setAutoCompactEnabled(this.session.autoCompactionEnabled)`；`footerContainer.addChild(this.footer)`。
9. `this.hideThinkingBlock = getHideThinkingBlock()`；`this.outputPad = getOutputPad()`。
10. `setRegisteredThemes(this.session.resourceLoader.getThemes().themes)`。
11. `this.themeController = new InteractiveThemeController(this.ui, { getSettingsManager: () => this.settingsManager, showError: (m) => this.showError(m), onChanged: () => this.updateEditorBorderColor(), initialThemeSetting: options.initialThemeSetting })`。

`init()`（945–1129 行）**实际执行顺序**（旧版规格列的 5 步是错的，实际是）：

1. `if (this.isInitialized) return;` → `this.registerSignalHandlers()`。
2. `this.changelogMarkdown = this.getChangelogForDisplay()`。
3. 若 `scopedModels.length > 0 && shouldShowStartupDetails()`：用 `console.log(theme.fg("dim", \`Model scope: ...\`))` 打印模型清单（见下）。
4. `this.renderWidgets()`（先放默认 Spacer）→ `createChatViewport({ document, pendingMessages, status, widgetsAbove, editor, widgetsBelow, footer, scrollbar: getFullscreenScrollbar(), scrollbarTrackStyle: t => theme.fg("scrollbarTrack", t), scrollbarThumbStyle: t => theme.fg("scrollbarThumb", t) })` → 缓存 `transcriptScrollView`/`fullscreenLayoutRoot` → `mountInteractiveTui(this.renderer, [...7 个容器...])`。
5. 启动期编辑器回调（"允许输入但只处理中断/退出/提交反馈"）：`defaultEditor.onAction("app.clear", () => this.handleCtrlC())`、`defaultEditor.onCtrlD = () => this.handleCtrlD()`、`defaultEditor.onSubmit = (text) => this.handleStartupSubmit(text)`、`ui.setFocus(this.editor)`。
6. `this.ui.start()`；`this.isInitialized = true`；`this.programStatus.report()`；`this.ensurePngTranscoder()`。
7. `this.themeController.applyFromSettings()`；`await this.themeController.waitForTerminalColors()`（注释：header 与启动通知把主题色烘进文本，所以必须等终端配色；等待终点是 DA1 回复或 100 ms 超时）。
8. 构建 Header（见下）；`else` 分支为 `new Text("", 0, 0)`；`this.ui.requestRender()`。
9. `await Promise.all([ensureTool("fd", s => this.showManagedToolStatus(s)), ensureTool("rg", s => this.showManagedToolStatus(s))])` → `this.fdPath = fdPath`（注释：挂载 TUI 之后才安装 fd/rg，避免慢下载看起来像启动卡死）。
10. `this.setupKeyHandlers()`、`this.setupEditorSubmitHandler()`、`this.ui.requestRender()`（真正的按键处理在托管工具安装完成后才启用）。
11. `await this.rebindCurrentSession()`（**先**初始化扩展，这样 loaded resources 能显示在消息之前）。`rebindCurrentSession()` 内部：`unsubscribe?.()` → `programStatus.reset()` → `applyRuntimeSettings()` →（`renderBeforeBind` 时先 `renderCurrentSessionState()` + `subscribeToAgent()`）→ `await bindCurrentSessionExtensions()`（其末尾调用 `showLoadedResources({ force: false, showDiagnosticsWhenQuiet: true })`）→ 会话未变则 `subscribeToAgent()` → `await updateAvailableProviderCount()` → `updateEditorBorderColor()` → `updateTerminalTitle()`。
12. `this.renderInitialMessages()`（**在** loaded resources 之后；内部已含 `renderProjectTrustWarningIfNeeded()`）。
13. `onThemeChange(() => { this.ui.invalidate(); this.updateEditorBorderColor(); this.ui.requestRender(); })`（主题文件监听；句柄为全局单例，后注册者生效）。
14. `this.footerDataProvider.onBranchChange(() => this.ui.requestRender())`。
15. `await this.updateAvailableProviderCount()`。
16. `this.ui.renderNow()`；随后 `void loadAllHighlightLanguages().then(() => { if (!this.isInitialized) return; this.ui.invalidate(); this.ui.requestRender(); })`。

注意 `subscribeToAgent()`/`applyRuntimeSettings()`/`updateTerminalTitle()` 都**不是**在 `init()` 里直接调的，而是经 `rebindCurrentSession()`；
`showLoadedResources()` 也不是 `init()` 直接调，而是 `bindCurrentSessionExtensions()` 的末尾。`init()` 里**没有**显式的 `setCapabilityOverrides` 调用（构造期和 `applyRuntimeSettings()` 各调一次）。

Header 显示判定（两个独立开关）：
- `shouldShowStartupHeader(): boolean` = `options.verbose === true || getQuietStartup() !== true`（`"header"` 视为显示）。
- `shouldShowStartupDetails(): boolean` = `options.verbose === true || getQuietStartup() === false`（**只有显式 `false` 才显示**模型清单/loaded resources）。
- `QuietStartup` 的取值是 `boolean | "header"`（`settings-schema.ts`），`getQuietStartup()` 把非法值归一为默认值。

**启动 Header 文案（逐字）**

展开态（`app.tools.expand` 之后）的指令列表共 **19 行**，以 `"\n"` 连接（**逐字**，`hint(...)` = `keyHint(binding, description)`，`rawKeyHint(keys, description)` 直接输出 keys 文本）：

```
hint("app.interrupt", "to interrupt")
hint("app.clear", "to clear")
rawKeyHint(`${keyText("app.clear")} twice`, "to exit")
hint("app.exit", "to exit (empty)")
hint("app.suspend", "to suspend")
keyHint("tui.editor.deleteToLineEnd", "to delete to end")
hint("app.thinking.cycle", "to cycle thinking level")
rawKeyHint(`${keyText("app.model.cycleForward")}/${keyText("app.model.cycleBackward")}`, "to cycle models")
hint("app.model.select", "to select model")
hint("app.tools.expand", "to expand tools")
hint("app.thinking.toggle", "to expand thinking")
hint("app.editor.external", "for external editor")
rawKeyHint("/", "for commands")
rawKeyHint("!", "to run bash")
rawKeyHint("!!", "to run bash (no context)")
hint("app.message.followUp", "to queue follow-up")
hint("app.message.dequeue", "to edit all queued messages")
hint("app.clipboard.pasteImage", "to paste files on macOS, images, or text")
rawKeyHint("drop files", "to attach")
```

折叠态（5 条，以 `theme.fg("muted"," · ")` 连接，**逐字**）：

```
hint("app.interrupt", "interrupt")
rawKeyHint(`${keyText("app.clear")}/${keyText("app.exit")}`, "clear/exit")
rawKeyHint("/", "commands")
rawKeyHint("!", "bash")
hint("app.tools.expand", "more")
```

Header 组件构造（`BuiltInHeader(getCollapsedText, getExpandedText, expanded, paddingX = 1, paddingY = 0)`）：
```
collapsed = `${withLogo(compactInstructions())}\n${compactOnboarding()}\n\n${onboarding()}`
expanded  = `${withLogo(expandedInstructions())}\n\n${onboarding()}`
expanded  = getStartupExpansionState()
showLogo  → header.onLogoClick = (column, row) => playKianaLogo3d(this.renderer, column, row)
headerContainer: Spacer(1) → builtInHeader → Spacer(1)
```
`shouldShowStartupHeader()` 为假时：`builtInHeader = new Text("", 0, 0)`，`headerContainer` 只加它（无 Spacer）。

```
compactOnboarding = theme.fg("dim", `Press ${keyText("app.tools.expand")} to show full startup help${showDetails ? " and loaded resources" : ""}.`)
onboarding = theme.fg("dim", "Kiana can explain its own features and look up its docs. Ask it how to use or extend Kiana.")
```
有 logo 时：
```
`${top} ${theme.fg("dim", `v${version}`)}\n${bottom} ${hints}`
```
无 logo 时：
```
`${kianaWordmark()} ${theme.fg("dim", `v${version}`)}\n${hints}`
```
Model scope 行（`console.log`）：`` theme.fg("dim", `Model scope: ${modelList}${cycleHint}`) ``，`cycleHint = theme.fg("muted", ` (${formatKeyText(cycleKeys.join("/"), {capitalize:true})} to cycle)`)`。

`updateTerminalTitle()`（1134–1141 行，逐字）：
```ts
const cwdBasename = path.basename(this.sessionManager.getCwd());
const sessionName = this.sessionManager.getSessionName();
if (sessionName) {
    this.ui.terminal.setTitle(`${APP_TITLE} - ${sessionName} - ${cwdBasename}`);
} else {
    this.ui.terminal.setTitle(`${APP_TITLE} - ${cwdBasename}`);
}
```
注意用的是 **`APP_TITLE`** 而不是字面量 `π`：`APP_TITLE = kianaConfigName ? APP_NAME : "π"`（`config.ts`），即默认 `π`，但被配置了 `kianaConfig.name` 的发行版会显示其 `APP_NAME`。调用点：`rebindCurrentSession()` 末尾、`session_info_changed` 事件、以及 win32 上 `checkForPackageUpdates().finally()` 里（`if (process.platform === "win32" && this.isInitialized) this.updateTerminalTitle()`）。

#### 2.50.4 `run()`

```ts
async run(): Promise<void> {
  await this.init();
  if (!process.env.KIANA_OFFLINE) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 15_000);
    void refreshModelCatalogs(this.session.modelRuntime, controller.signal)
      .then(() => this.updateAvailableProviderCount()).catch(() => {})
      .finally(() => clearTimeout(timeout));
  }
  checkForNewKianaVersion(this.version).then(newRelease => { if (newRelease) this.showNewVersionNotification(newRelease); });
  this.checkForPackageUpdates().then(updates => { if (updates.length > 0) this.showPackageUpdateNotification(updates); })
    .finally(() => { if (process.platform === "win32" && this.isInitialized) this.updateTerminalTitle(); });
  this.checkTmuxKeyboardSetup().then(warning => { if (warning) this.showWarning(warning); });

  const { migratedProviders, startupDiagnostics, modelFallbackMessage,
          initialMessage, initialImages, initialMessages } = this.options;
  for (const diagnostic of startupDiagnostics ?? []) {
    diagnostic.type === "error"   ? this.showError(diagnostic.message)
    : diagnostic.type === "warning" ? this.showWarning(diagnostic.message)
    : this.showStatus(diagnostic.message);
  }
  if (migratedProviders?.length) this.showWarning(`Migrated credentials to auth.json: ${migratedProviders.join(", ")}`);
  const modelsJsonError = this.session.modelRuntime.getError();
  if (modelsJsonError) this.showError(`models.json error: ${modelsJsonError}`);
  if (modelFallbackMessage) this.showWarning(modelFallbackMessage);

  const crash = takeUnnotifiedCrash();
  if (crash) {
    const when = new Date(crash.timestamp).toLocaleString();
    this.showWarning(`${APP_NAME} crashed on ${when} (${crash.message}). Run /bug to report it; the crash details are attached automatically.`);
  }
  void this.maybeWarnAboutAnthropicSubscriptionAuth();

  if (initialMessage) {
    try { await this.session.prompt(initialMessage, { images: initialImages }); }
    catch (e) { this.showError(e instanceof Error ? e.message : "Unknown error occurred"); }
  }
  if (initialMessages) {
    for (const message of initialMessages) {
      try { await this.session.prompt(message); }
      catch (e) { this.showError(e instanceof Error ? e.message : "Unknown error occurred"); }
    }
  }
  while (true) {
    const userInput = await this.getUserInput();
    try { await this.session.prompt(userInput); }
    catch (e) { this.showError(e instanceof Error ? e.message : "Unknown error occurred"); }
  }
}
```
**注意**：`migratedProviders` 用 `showWarning`（不是 `console.log`）；`models.json error:` 用 `showError`；崩溃时间用 `new Date(ts).toLocaleString()`（locale 相关，Rust 需固定为与 Node 默认 ICU 相同格式；在 `LC_ALL=C` 下为 `M/D/YYYY, h:mm:ss AM/PM`）。错误回退字符串是 `"Unknown error occurred"`（区别于 bug-report 的 `"Unknown error"`）。

**tmux 提示**（`checkTmuxKeyboardSetup`，1283–1331 行）：
- 非 tmux（无 `TMUX` 环境变量）→ 直接 `undefined`，不提示。
- 并行 `tmux show -gv extended-keys` 与 `tmux show -gv extended-keys-format`（`spawn("tmux", ["show","-gv",option], { stdio: ["ignore","pipe","ignore"] })`，**每个查询 2000 ms 超时后 `proc.kill()` 并 resolve `undefined`**；spawn error 同样 resolve `undefined`；只有 exit code 0 才用 `stdout.trim()`）。
- **无法查询（timeout / sandbox / 命令不存在）时静默不提示**：`if (extendedKeys === undefined) return undefined;`
- `extendedKeys !== "on" && extendedKeys !== "always"` 时才警告（**`always` 也算正确**，旧版规格漏了这一点）：
  `tmux extended-keys is off. Modified Enter keys may not work. Add \`set -g extended-keys on\` to ~/.tmux.conf and restart tmux.`
- 仅当 `extendedKeysFormat === "xterm"` 时警告（`csi-u`、空、其它值都不警告）：
  `tmux extended-keys-format is xterm. Kiana works best with csi-u. Add \`set -g extended-keys-format csi-u\` to ~/.tmux.conf and restart tmux.`
- 两个检查**先 extended-keys 后 format**，第一条命中即返回，不会同时提示两条。

**安装遥测**（`reportInstallTelemetry`）：

> **Rust 实现差异（品牌隔离）**：目标 URL 为 `KIANA_API_BASE_URL/api/report-install`；
> 未配置时跳过上报。

```
GET https://kiana.dev/api/report-install?version=${encodeURIComponent(version)}
header: User-Agent: getKianaUserAgent(version)
signal: AbortSignal.timeout(5000)
```

**包更新检查**（`checkForPackageUpdates(): Promise<string[]>`，1263–1281 行，旧版只列了调用点没写实现）：
```ts
if (process.env.KIANA_OFFLINE) return [];
try {
    const packageManager = new DefaultPackageManager({
        cwd: this.sessionManager.getCwd(), agentDir: getAgentDir(), settingsManager: this.settingsManager,
    });
    const updates = await packageManager.checkForAvailableUpdates();
    return updates.map((update) => update.displayName);
} catch { return []; }
```
即：**返回的是 `displayName` 数组**（供 `showPackageUpdateNotification` 直接 `- ${p}` 列表），任何异常都吞成空数组；`KIANA_OFFLINE` 时连 `DefaultPackageManager` 都不构造。

#### 2.50.5 事件处理 `handleEvent(event)`（3363–3761 行）

**入口序言（逐字，所有事件先执行）**：
```ts
private async handleEvent(event: AgentSessionEvent): Promise<void> {
  if (!this.isInitialized) { await this.init(); }
  this.footer.invalidate();
  this.programStatus.handleEvent(event);
  switch (event.type) { ... }
}
```
> Rust 侧等价：`handle_event` 必须先确保初始化完成（首次事件可能早于 `init()`），且**每个**事件都先 `footer.invalidate()` 与 `programStatus.handleEvent(event)`。`programStatus.handleEvent` 内部会对 `agent_start`/`message_end`/`compaction_start`/`compaction_end`/`agent_settled`/`session_info_changed` 触发 OSC 7501 上报；其余事件直接返回（见 §2.11）。

| 事件 | 行为（**逐字字符串标出**） |
|---|---|
| `agent_start` | `pendingTools.clear()`；**仅**恢复 `retryEscapeHandler`（若存在则 `defaultEditor.onEscape = retryEscapeHandler` 并清空字段）。**不碰** `autoCompactionEscapeHandler`（压缩期的 escape 拦截要一直维持到 `compaction_end`） |
| `turn_start` | 若 `settingsManager.getShowTerminalProgress()` → `ui.terminal.setProgress(true)`；若 `this.workingVisible`：仅当 `activeStatusIndicator?.kind !== "working"` 时 `showWorkingStatusIndicator()`（**已在 working 则不动，避免重建 spinner**）；否则 `clearStatusIndicator()`（**不带 kind，清掉任意类型的指示器**，不是 `clearStatusIndicator("working")`）；`ui.requestRender()` |
| `queue_update` | `updatePendingMessagesDisplay()`；`requestRender()` |
| `entry_appended` | 先 `if (this.entriesRenderedByBoundaryCompaction.delete(event.entry.id)) break;`（见下），再按 entry.type 分派 |
| `session_info_changed` | `updateTerminalTitle()`；`footer.invalidate()`；`requestRender()` |
| `thinking_level_changed` | `footer.invalidate()`；`updateEditorBorderColor()`（**不** `requestRender`） |
| `message_start` | 分 `custom` / `user` / `assistant`：assistant 创建 `streamingComponent`（带 `hideThinkingBlock`、`getMarkdownThemeWithSettings()`、`hiddenThinkingLabel`、`outputPad`、`getMarkdownTransformers()`）并 `updateContent(msg, true)` |
| `message_update` | `streamingComponent.updateContent(msg, true)`；遍历 `streamingMessage.content` 里所有 `toolCall`：未在 `pendingTools` 中则**新建** `ToolExecutionComponent`（options 为 `{showImages, imageWidthCells, outputPad}`，definition 来自 `getRegisteredToolDefinition(name)`）并 `addChild` + 记录；已存在则 `updateArgs(args)` |
| `message_end` | 收尾、错误提示、thinking 丢弃提示、缓存未命中提示（见下） |
| `bash_execution_update` | no-op（注释：bash 执行回调自己处理 TUI 输出） |
| `tool_execution_start` | 若 `event.parentToolCallId` 存在则**直接跳过**（codemode 嵌套调用由父行显示）；否则复用/新建 `ToolExecutionComponent` 并 `markExecutionStarted()` |
| `tool_execution_update` | `component.updateResult({ ...event.partialResult, isError: false }, true)`（**isError 强制 false**） |
| `tool_execution_end` | 若 `event.isError` 先 `maybeShowInstallChangeWarning()`；`updateResult({ ...event.result, isError: event.isError, durationMs: event.durationMs })`（默认 `isPartial=false`）；从 `pendingTools` 删除 |
| `agent_end` | 若 `getShowTerminalProgress()` → `terminal.setProgress(false)`；`clearStatusIndicator("working")`（**带 kind**）；移除 `streamingComponent` 并清 `streamingMessage`；`pendingTools.clear()` |
| `agent_settled` | `await this.checkShutdownRequested()` |
| `compaction_start` | 若 `getShowTerminalProgress()` → `setProgress(true)`；`autoCompactionEscapeHandler = defaultEditor.onEscape`；`defaultEditor.onEscape = () => session.abortCompaction()`；`showStatusIndicator(new CompactionStatusIndicator(ui, event.reason, colorFn))`，`colorFn` 仅在**编辑器内嵌状态**时给出：`text => (editor.borderColor ?? theme.getThinkingBorderColor(session.thinkingLevel \|\| "off"))(text)`，否则 `undefined` |
| `compaction_end` | 见下方**逐字展开** |
| `auto_retry_start` | `retryEscapeHandler = defaultEditor.onEscape`；`defaultEditor.onEscape = () => session.abortRetry()`；`showStatusIndicator(new RetryStatusIndicator(ui, event.attempt, event.maxAttempts, event.delayMs))` |
| `auto_retry_end` | 恢复 `retryEscapeHandler`（并清空）；`clearStatusIndicator("retry")`；`!event.success` → `showError(\`Retry failed after ${event.attempt} attempts: ${event.finalError \|\| "Unknown error"}\`)` 【逐字，用 `event.attempt` 而非 `maxAttempts`】 |
| `summarization_retry_scheduled` | `showError(event.errorMessage)` + `new RetryStatusIndicator(ui, event.attempt, event.maxAttempts, event.delayMs)` |
| `summarization_retry_attempt_start` | `clearStatusIndicator("retry")`；`source === "branchSummary"` → `BranchSummaryStatusIndicator(ui)`，否则 `CompactionStatusIndicator(ui, event.reason, colorFn)`（`colorFn` 同上） |
| `summarization_retry_finished` | `clearStatusIndicator("retry")` |

**`message_end` 细节（逐字）**：
- `role === "user"` → **直接 break**（不处理，也不 `requestRender`）。
- 只有 `streamingComponent && role === "assistant"` 才处理；先 `streamingMessage = event.message`。
- `stopReason === "aborted"`：`retryAttempt = session.retryAttempt`；`errorMessage = retryAttempt > 0 ? \`Aborted after ${retryAttempt} retry attempt${retryAttempt > 1 ? "s" : ""}\` : "Operation aborted"`；并把 `streamingMessage.errorMessage` 设为该串。
- **然后**才 `streamingComponent.updateContent(streamingMessage, false)`（即先补 errorMessage 再渲染最终帧）。
- `stopReason === "aborted" || "error"`：`if (!errorMessage) errorMessage = streamingMessage.errorMessage || "Error"`（回退串是 **`"Error"`**，不是 "Unknown error"）；对每个 `pendingTools` 调用 `updateResult({ content: [{ type: "text", text: errorMessage }], isError: true })`；`pendingTools.clear()`；`maybeSuggestBugReport(streamingMessage)`。
- 否则：对每个 pending tool `setArgsComplete()`（触发 edit 工具 diff 计算）；`maybeShowThinkingDropNotice(streamingMessage)`；`maybeShowCacheMissNotice(streamingMessage)`。
- 收尾：`streamingComponent = undefined; streamingMessage = undefined; footer.invalidate(); requestRender()`。

**`entry_appended` 细节（逐字）**：
- **第一条语句**：`if (this.entriesRenderedByBoundaryCompaction.delete(event.entry.id)) break;`——边界压缩后已单独渲染过的后续条目，其迟到事件必须被吞掉（`Set.delete` 返回值本身即"是否命中"）。
- `custom` → `addCustomEntryToChat(entry)` + render
- `usage` 且 `entry.kind === "cache_warm"` → `addCacheWarmingUsage(entry)` + render
- `custom_message` 且 `entry.display` → `addMessageToChat(createCustomMessage(customType, content, display, details, timestamp))` + render
- `compaction`：
  1. `const entries = this.sessionManager.buildContextEntries();`
  2. `if (entries[0]?.id !== event.entry.id) break;`——**只有"当前最新压缩"才触发重建**；历史压缩的迟到事件直接丢弃。
  3. `this.chatContainer.clear();`
  4. `branch = getBranch()`；`compactionIndex = branch.findIndex(e => e.id === event.entry.id)`；`entriesAfterCompaction = new Set(branch.slice(compactionIndex + 1).map(e => e.id))`；`retainedEntries = entries.slice(1)`。
  5. `renderSessionEntries(retainedEntries.filter(e => !entriesAfterCompaction.has(e.id)))`（压缩**之前**保留下来的条目）
  6. `addMessageToChat(createCompactionSummaryMessage(event.entry.summary, event.entry.tokensBefore, event.entry.timestamp))`（用**条目自带 timestamp**，不是 `now`）
  7. 若 `event.entry.usage` → `addCompactionCostNotice({ type: "compaction_cost", kind: "compaction", usage })`
  8. `renderSessionEntries(retainedEntries.filter(e => entriesAfterCompaction.has(e.id)))`（压缩**之后**的条目）
  9. `for (const entryId of entriesAfterCompaction) this.entriesRenderedByBoundaryCompaction.add(entryId);`（登记，供第 1 步吞掉它们的后续 `entry_appended`）
  10. `footer.invalidate()` + `requestRender()`

**`compaction_end` 细节（逐字）**：
```ts
if (this.settingsManager.getShowTerminalProgress()) this.ui.terminal.setProgress(false);
if (this.autoCompactionEscapeHandler) { this.defaultEditor.onEscape = this.autoCompactionEscapeHandler;
                                        this.autoCompactionEscapeHandler = undefined; }
this.clearStatusIndicator("compaction");
if (event.aborted) {
  if (event.reason === "manual") this.showError("Compaction cancelled");
  else this.showStatus("Auto-compaction cancelled");
} else if (event.result) {
  const entries = this.sessionManager.buildContextEntries();
  if (entries[0]?.type !== "compaction") throw new Error("Completed compaction is missing from the session context");
  this.chatContainer.clear();
  // 最新压缩被前置到模型上下文，这里按时间顺序把它追加到底部
  this.renderSessionEntries(entries.slice(1));
  this.addMessageToChat(createCompactionSummaryMessage(event.result.summary, event.result.tokensBefore,
                                                      new Date().toISOString()));
  if (event.result.usage) this.addCompactionCostNotice({ type: "compaction_cost", kind: "compaction",
                                                        usage: event.result.usage });
  this.footer.invalidate();
} else if (event.errorMessage) {
  if (event.reason === "manual") this.showError(event.errorMessage);
  else {
    this.chatContainer.addChild(new Spacer(1));
    const errorMessage = event.errorMessage;
    this.chatContainer.addChild(new ThemedText(() => theme.fg("error", errorMessage), 1, 0));
  }
}
void this.flushCompactionQueue({ willRetry: event.willRetry });
this.ui.requestRender();
```
> 移植要点：① `entries[0]` 不是压缩 → **抛异常**（消息逐字）；② 成功路径的 summary 时间戳是 **`new Date().toISOString()`**（而 `entry_appended` 路径用 `entry.timestamp`）；③ 自动压缩失败时的错误行是 **`error`** 色 `ThemedText`（非 `showError`），手动压缩失败才走 `showError`；④ `flushCompactionQueue` 用 `void`，不 await。

#### 2.50.6 聊天区渲染

**`showStatus(message)`** —— 去重逻辑（**逐字**）：
```ts
const children = this.chatContainer.children;
const last = children.length > 0 ? children[children.length - 1] : undefined;
const secondLast = children.length > 1 ? children[children.length - 2] : undefined;
if (last && secondLast && last === this.lastStatusText && secondLast === this.lastStatusSpacer) {
  this.lastStatusMessage = message;
  this.lastStatusText.invalidate();
  this.ui.requestRender();
  return;
}
const spacer = new Spacer(1);
this.lastStatusMessage = message;
const text = new ThemedText(() => theme.fg("dim", this.lastStatusMessage), 1, 0);
this.chatContainer.addChild(spacer);
this.chatContainer.addChild(text);
this.lastStatusSpacer = spacer;
this.lastStatusText = text;
this.ui.requestRender();
```
要点：去重判断用**对象恒等**（`===`）而非"最后两个子节点任意"；文本颜色是 **`dim`**（不是 `muted`）；闭包读取的是**字段** `this.lastStatusMessage`，因此去重更新只需改字段 + `invalidate()`，不必重建组件。
**`showManagedToolStatus(status)`**：首次调用先加 `Spacer(1)`；`message = status.type === "warning" ? `Warning: ${status.message}` : status.message`；颜色 `warning`（warning）/`dim`（其他）；同时**清空** `lastStatusSpacer` / `lastStatusText` 字段（避免与后续 `showStatus` 的去重逻辑串味）。

**`addCustomEntryToChat(entry)`**：按 `entry.customType` 查找 entry renderer；找到则 `CustomEntryComponent` 插到 `streamingComponent` **之前**（`chatContainer.children.indexOf(streamingComponent)`）。

**`addMessageToChat(message, options)`** 分派：
- `bashExecution` → `BashExecutionComponent`
- `custom`（有 renderer）→ `CustomMessageComponent`
- `compactionSummary` → `CompactionSummaryMessageComponent` + `Spacer(1)`
- `branchSummary` → `BranchSummaryMessageComponent` + `Spacer(1)`
- `system` → 跳过
- `user` → 若 `parseSkillBlock` 命中 → `SkillInvocationMessageComponent`（必要时再加 `UserMessageComponent`）
- `assistant` → `AssistantMessageComponent`
- `toolResult` → 跳过（由 ToolExecutionComponent 承担）

**`renderSessionEntries(entries, options)`**（**逐字**）：
```ts
options: { updateFooter?: boolean; populateHistory?: boolean } = {}
// 选区坐标指向被替换的 transcript（#9311）
if (this.renderer instanceof TuiAltScreen) this.renderer.resetTextSelection();
const messageMeta = new Map<AgentMessage, { id: string; timestamp: string }>();
const items = entries.flatMap((entry): RenderSessionItem[] => {
  if (entry.type === "custom" || (entry.type === "usage" && entry.kind === "cache_warm")) return [entry];
  const messages = sessionEntryToContextMessages(entry);
  for (const msg of messages) messageMeta.set(msg, { id: entry.id, timestamp: entry.timestamp });
  if ((entry.type === "compaction" || entry.type === "branch_summary") && entry.usage && messages.length > 0) {
    return [...messages, { type: "compaction_cost", kind: entry.type, usage: entry.usage }];
  }
  return messages;
});
this.renderSessionItems(items, messageMeta, options);
```
要点：`custom` 与 `cache_warm` usage 条目**原样保留**为 item（不走 `sessionEntryToContextMessages`）；`messageMeta` 以 **AgentMessage 对象恒等**为键（Rust 需改用索引/句柄）；`compaction_cost` 只有在 `entry.usage` 存在**且** `messages.length > 0` 时才追加，且 `kind` 直接取 `entry.type`（`"compaction"` / `"branch_summary"`）。

**`renderSessionItems(items, messageMeta?, options)`**（**逐字**；`renderSessionEntries` 的实际渲染器）：
```
this.pendingTools.clear()                    // 重建前清空，避免旧组件泄漏
renderedPendingTools = new Map<string, ToolExecutionComponent>()
// 缓存未命中不持久化（与成功的 cache-warm usage 不同），必须重新推导并插到"付费的"assistant 消息之后
cacheMisses = getShowCacheMissNotices()
  ? collectCacheMisses(this.sessionManager.getEntries(), this.session.modelRuntime)
  : new Map<AssistantMessage, CacheMiss>()
if (options.updateFooter) { this.footer.invalidate(); this.updateEditorBorderColor(); }
for (item of items) {
  if (isCustomSessionEntry(item))      { this.addCustomEntryToChat(item); continue; }
  if (isUsageSessionEntry(item))       { this.addCacheWarmingUsage(item); continue; }
  if (isCompactionCostNotice(item))    { this.addCompactionCostNotice(item); continue; }
  if (message.role === "assistant") {
    this.addMessageToChat(message, { messageId: meta?.id, timestamp: meta?.timestamp });
    for (content of message.content) if (content.type === "toolCall") {
      component = new ToolExecutionComponent(content.name, content.id, content.arguments,
                    { showImages, imageWidthCells, outputPad }, getRegisteredToolDefinition(content.name), ui, cwd);
      component.setExpanded(this.toolOutputExpanded);
      this.chatContainer.addChild(component);
      if (message.stopReason === "aborted" || message.stopReason === "error") {
        errorMessage = stopReason === "aborted"
          ? (retryAttempt > 0 ? `Aborted after ${retryAttempt} retry attempt${retryAttempt > 1 ? "s" : ""}`
                              : "Operation aborted")
          : message.errorMessage || "Error";
        component.updateResult({ content: [{ type: "text", text: errorMessage }], isError: true });
      } else {
        renderedPendingTools.set(content.id, component);
      }
    }
    if (message.stopReason !== "aborted" && message.stopReason !== "error") {
      const miss = cacheMisses.get(message); if (miss) this.addCacheMissNotice(miss);
    }
  } else if (message.role === "toolResult") {
    const component = renderedPendingTools.get(message.toolCallId);
    if (component) { component.updateResult(message); renderedPendingTools.delete(message.toolCallId); }
  } else {
    this.addMessageToChat(message, { ...options, messageId: meta?.id, timestamp: meta?.timestamp });
  }
}
for ([toolCallId, component] of renderedPendingTools) this.pendingTools.set(toolCallId, component);  // 遗留的待定工具
this.ui.requestRender();
```
> 移植要点：① `toolResult` **通过 `toolCallId` 匹配**到前面 assistant 消息里创建的组件；② 匹配不上的（无对应 toolCall）**不渲染**，直接被丢弃；③ 结束时仍有未匹配的 toolCall 组件会留在 `pendingTools` 里等待流式结果；④ 非 assistant/toolResult 的消息把 `options` 透传给 `addMessageToChat`（因此 `populateHistory` 只对 user 消息生效）。

**`addCompactionCostNotice(notice)`**（**逐字**）：
```ts
if (!this.settingsManager.getShowCacheMissNotices()) return;   // 重要：受设置门控
const { usage } = notice;
const tokens = usage.input + usage.output + usage.cacheRead + usage.cacheWrite;
const cost = usage.cost.total >= 0.01 ? ` (~$${usage.cost.total.toFixed(2)})` : "";
const label = notice.kind === "compaction" ? "Compaction" : "Branch summary";
this.chatContainer.addChild(new Spacer(1));
this.chatContainer.addChild(
  new ThemedText(() => theme.fg("warning", `${label}: ${formatTokens(tokens)} tokens billed${cost}`), 1, 0),
);
```
要点：颜色是 **`warning`**（不是 `muted`）；`tokens` 是 **四项之和**（`input + output + cacheRead + cacheWrite`，与 footer 的总 token 口径一致）；`usage.cost.total` 在类型上**非可选**（`cost.total`），无需 `?.`；`toFixed(2)`；受 `getShowCacheMissNotices()` 门控（测试 `uses the cache miss notice setting for compaction and branch summary costs`）。
**`countDroppedThinkingBlocks(message)`**（`private static`）：遍历 `message.diagnostics`，只统计 `diagnostic.type === "anthropic_input_transformations"` 且 `details.transformations` 为数组时 `type === "thinking_dropped"` 的元素个数；其他一律忽略。
**`maybeShowThinkingDropNotice(message)`**（受 `getShowCacheMissNotices()` 门控）：
```
droppedCount = countDroppedThinkingBlocks(message); if (droppedCount === 0) return
// message_end 早于持久化，因此 branch 里最后一条 assistant 消息就是"上一条"
previousDroppedCount = 0
for (i = branch.length-1; i >= 0; i--) if (branch[i] 是 assistant 消息) { previousDroppedCount = count(...); break }
if (droppedCount <= previousDroppedCount) return          // 只在数字"增加"时提示
noun = droppedCount === 1 ? "thinking block" : "thinking blocks"
Spacer(1) + ThemedText(() => theme.fg("warning", `Anthropic dropped ${droppedCount} ${noun} (details in session)`), 1, 0)
```
**`maybeShowCacheMissNotice(message)`**（受 `getShowCacheMissNotices()` 门控）：`detectCacheMiss(this.sessionManager.getEntries(), message, this.session.modelRuntime)`，有结果才 `addCacheMissNotice`。**`addCacheMissNotice(miss)`**：阈值 `miss.missedTokens < 20_000 && miss.missedCost < 0.1` 时**直接 return**（即 `>= 20000 tokens` 或 `>= $0.1` 任一满足才显示）；`cost = miss.missedCost >= 0.01 ? ` (~$${miss.missedCost.toFixed(2)})` : ""`；标签：
- `miss.modelChanged` → `Cache miss after model switch`（**优先于 idle 分支**）
- 否则 `miss.idleMs >= CACHE_TTL_MS` → `` `Cache miss after ${Math.round(miss.idleMs / 60_000)}m idle` ``（**必须加这个门控**，否则短 idle 也会误报）
- 否则 `Cache miss`

文本：`` `${label}: ${formatTokens(miss.missedTokens)} tokens re-billed${cost}` ``，颜色 **`warning`**，前置 `Spacer(1)`。
**`renderInitialMessages()`**（**逐字**）：
```ts
const entries = this.sessionManager.buildContextEntries();
this.renderSessionEntries(entries, { updateFooter: true, populateHistory: true });
this.renderProjectTrustWarningIfNeeded();
const compactionCount = this.sessionManager.getEntries().filter((e) => e.type === "compaction").length;
if (compactionCount > 0) {
  const times = compactionCount === 1 ? "1 time" : `${compactionCount} times`;
  this.showStatus(`Session compacted ${times}`);
}
```
（注意单数形式 **`1 time`**、复数 **`N times`**；`renderProjectTrustWarningIfNeeded()` 在 `showStatus` **之前**被调用。）
**`renderProjectTrustWarningIfNeeded()`**：`settingsManager.isProjectTrusted()` 或 `!hasTrustRequiringProjectResources(cwd)` → 直接 return；否则 **仅当 chat 已非空**时先加 `Spacer(1)`，然后 `ThemedText`（颜色 **`warning`**）输出 `This project is not trusted. Project ${CONFIG_DIR_NAME} resources and packages are ignored. Use /trust to save a trust decision, then restart kiana.` 【逐字】

#### 2.50.7 生命周期与信号

**`handleCtrlC()`**：500ms 内第二次 → `shutdown()`；否则 `clearEditor()` 并提示。**`handleCtrlD()`**：编辑器为空 → `shutdown()`。

**`shutdown(options?: { fromSignal?: boolean })`**（**逐字**；注意源码**没有**调用 `stopThemeWatcher`/`programStatus.clear`，只有 `stop()` 链内的动作）：
```ts
private async shutdown(options?: { fromSignal?: boolean }): Promise<void> {
  if (this.isShuttingDown) return;
  this.isShuttingDown = true;

  if (options?.fromSignal) {
    await this.runtimeHost.dispose();
    this.themeController.disableAutoSync();
    await this.ui.terminal.drainInput(1000);
    this.stop();
    process.exit(0);
  }

  this.themeController.disableAutoSync();
  await this.ui.terminal.drainInput(1000);

  this.stop();
  await this.runtimeHost.dispose();

  const resumeCommand = formatResumeCommand(this.sessionManager);
  if (resumeCommand) process.stdout.write(`${chalk.dim("To resume this session:")} ${resumeCommand}\n`);

  process.exit(0);
}
```
`chalk.dim` 在 truecolor 下为 `\x1b[2m…\x1b[22m`（chalk 6 默认按终端能力降级；无颜色时原样）。
**`emergencyTerminalExit()`**（**逐字**）：`isShuttingDown = true` → `unregisterSignalHandlers()` → `killTrackedDetachedChildren()` → `process.exit(129)`。**不写任何恢复序列**（终端已死）。
**`uncaughtCrash(error)`**（**逐字**）：
```
if (isDeadTerminalError(error)) this.emergencyTerminalExit()
if (this.isShuttingDown) process.exit(1)
this.isShuttingDown = true
try { this.unregisterSignalHandlers() } catch {}
try { killTrackedDetachedChildren() } catch {}
try { this.ui.stop() } catch {}
console.error(`${APP_NAME} exiting due to uncaughtException:`)
console.error(error)
const extensionHint = this.getCrashExtensionHint(error)
if (extensionHint) console.error(`\n${extensionHint}`)
if (this.recordCrash("uncaught_exception", error)) console.error(`\n${this.crashReportInstructions()}`)
process.exit(1)
```
**`registerSignalHandlers()`**（**逐字**）：先 `unregisterSignalHandlers()`；对 `["SIGTERM"]`（非 win32 再加 `"SIGHUP"`）逐个 `process.prependListener(signal, handler)`，handler = `() => { killTrackedDetachedChildren(); void this.shutdown({ fromSignal: true }); }`，并把 `() => process.off(signal, handler)` 存入 `signalCleanupHandlers`。再对 `process.stdin/stdout/stderr` 各挂 `error` 监听：`isDeadTerminalError(error) ? emergencyTerminalExit() : throw error`。最后 `process.prependListener("uncaughtException", e => this.uncaughtCrash(e))`。
`isDeadTerminalError(error)`：`error && typeof error === "object" && "code" in error && DEAD_TERMINAL_ERROR_CODES.has(error.code)`。
**`handleCtrlZ()`**：
```
win32 → showStatus("Suspend to background is not supported on Windows"); return
keepAlive = setInterval(() => {}, 2 ** 30)
process.on("SIGINT", noop)
process.once("SIGCONT", () => { clearInterval(keepAlive); process.removeListener("SIGINT", noop);
                                ui.start(); ui.requestRender(true) })
try { ui.stop(); process.kill(0, "SIGTSTP") }
catch (e) { clearInterval(keepAlive); removeListener; throw e }
```

#### 2.50.8 输入与命令

**`setupKeyHandlers()`（逐条，顺序无关但集合完整）**

`defaultEditor` 上的动作绑定：

| 绑定 | 处理器 |
|---|---|
| `onEscape` | 见下方 Escape 优先级链 |
| `onAction("app.clear")` | `handleCtrlC()` |
| `onCtrlD` | `handleCtrlD()` |
| `onAction("app.suspend")` | `handleCtrlZ()` |
| `onAction("app.thinking.cycle")` | `cycleThinkingLevel()` |
| `onAction("app.model.cycleForward")` | `cycleModel("forward")` |
| `onAction("app.model.cycleBackward")` | `cycleModel("backward")` |
| `onAction("app.model.select")` | `showModelSelector()` |
| `onAction("app.tools.expand")` | `toggleToolOutputExpansion()` |
| `onAction("app.thinking.toggle")` | `toggleThinkingBlockVisibility()` |
| `onAction("app.editor.external")` | `void handleOpenExternalEditor()` |
| `onAction("app.message.copy")` | `void handleCopyCommand({ flashConfirmation: true, preferSelection: true })` |
| `onAction("app.message.followUp")` | `handleFollowUp()` |
| `onAction("app.message.dequeue")` | `handleDequeue()` |
| `onAction("app.session.new")` | `handleClearCommand()` |
| `onAction("app.session.tree")` | `showTreeSelector()` |
| `onAction("app.session.fork")` | `showUserMessageSelector()` |
| `onAction("app.session.resume")` | `showSessionSelector()` |
| `ui.onDebug` | `handleDebugCommand()` |
| `onChange(text)` | `wasBashMode = isBashMode`；`isBashMode = text.trimStart().startsWith("!")`；变化则 `updateEditorBorderColor()` |
| `onPasteImage` | `void handleClipboardPaste()` |

**`onEscape` 优先级链（逐条，顺序即优先级）**：
```
1. session.isStreaming            → restoreQueuedMessagesToEditor({ abort: true })
2. session.isBashRunning          → session.abortBash()
3. isBashMode                     → editor.setText(""); isBashMode = false; updateEditorBorderColor()
4. editor 文本 trim 后为空：
     action = settingsManager.getDoubleEscapeAction()          // "tree" | "fork" | "none"
     if (action !== "none") {
       now = Date.now()
       if (now - lastEscapeTime < 500) { action === "tree" ? showTreeSelector() : showUserMessageSelector();
                                          lastEscapeTime = 0 }
       else lastEscapeTime = now
     }
```
（注意第 4 步只在文本为空时进入；双击窗口为 **500 ms**。）

**`handleClipboardPaste()`（Ctrl+V）** —— 三级回退，**逐字**：
1. `readClipboardFilePaths()` 有值：
   - 任一路径匹配 `/\p{Cc}/u`（控制字符）→ 抛 `Error("Clipboard file path contains control characters")`
   - `paths = isBashMode ? filePaths.map(quoteIfNeeded).join(" ") : filePaths.join("\n")`
   - 依据光标前后字符决定是否补空格：前一字符非空白 → 前导 `" "`；后一字符非空白 → 尾部 `" "`
   - `editor.insertTextAtCursor?.(`${leadingSpace}${paths}${trailingSpace}`)`；`requestRender()`；**return**
2. `readClipboardImage()` 有图：
   - `ext = extensionForImageMimeType(mimeType) ?? "png"`
   - `fileName = `kiana-clipboard-${crypto.randomUUID()}.${ext}``，写入 `os.tmpdir()`
   - `insertTextAtCursor(filePath)`；**return**
3. `readClipboardText()` 有文本 → `insertTextAtCursor(text)`
4. 任何异常 → `showError(`Failed to paste from clipboard: ${msg}`)`

**`handleRightClickPaste()`**：取当前焦点组件；`readClipboardText()`；若焦点未变且有文本 → 以括号粘贴序列 `\x1b[200~${text}\x1b[201~` 调用其 `handleInput`；异常静默忽略。

**`handleStartupSubmit(text)`**（启动期间的回车）：`editor.setText(text)` + `showStatus("Startup is still in progress")`。

**`handleFollowUp()`**：
```
压缩中 → isExtensionCommand(text) ? session.prompt(text) : queueCompactionMessage(text, "followUp")
流式中 → session.prompt(editorText, { streamingBehavior: "followUp" })
否则 → onSubmit()
```
**`handleDequeue()`**：无队列 → `"No queued messages to restore"`；否则 `` `Restored ${n} queued message${s} to editor` ``。
**`updateEditorBorderColor()`**（**逐字**）：
```ts
if (this.isBashMode) this.editor.borderColor = theme.getBashModeBorderColor();
else {
  const level = this.session.thinkingLevel || "off";
  this.editor.borderColor = theme.getThinkingBorderColor(level);   // 不是"默认边框色"
}
this.activeStatusIndicator?.invalidate();
this.ui.requestRender();
```
（主题 API 名是 **`getBashModeBorderColor()`**，见 §2.2 第 455 行；旧名 `getBashBorderColor()` 不存在。非 bash 模式下边框色**随 thinking level 变化**，并会让当前状态指示器失效重绘。）
**`cycleThinkingLevel()`**：不支持思考 → `"Current model does not support thinking"`；成功 `` `Thinking level: ${newLevel}` ``。
**`cycleModel(direction)`**：`"Only one model in scope"` / `"Only one model available"`；成功 `` `Switched to ${model.name || model.id}${thinkingLevel && level !== "off" ? ` (thinking: ${level})` : ""}` ``。
**`setToolsExpanded(expanded)`**：`` `Tool output: ${expanded ? "expanded" : "collapsed"}` ``；同步刷新所有 `ToolExecutionComponent`。
**`toggleThinkingBlockVisibility()`**：`` `Thinking blocks: ${hidden ? "hidden" : "visible"}` ``。
**`showError(msg)`**：`chatContainer.addChild(new Spacer(1))` 然后 `new ThemedText(() => theme.fg("error", `Error: ${msg}`), this.outputPad, 0)`，最后 `requestRender()`。**先加 Spacer 再加文本（顺序不能反）**。
**`showWarning(msg)`**：同样两行，但颜色 `warning`、文案 `` `Warning: ${msg}` ``、`paddingX = 1`（不是 outputPad）。
**`showNewVersionNotification(release)`**（**逐字顺序**）：
```
Spacer(1)
DynamicBorder(text => theme.fg("warning", text))
ThemedText(() => `${theme.bold(theme.fg("warning","Update Available"))}\n${updateInstruction()}`, 1, 0)
  其中 updateInstruction = theme.fg("muted", `New version ${release.version} is available. Run `) + theme.fg("accent", `${APP_NAME} update`)
[release.note?.trim() 非空时]
  Spacer(1)
  Markdown(note, 1, 0, getMarkdownThemeWithSettings(), { color: t => theme.fg("muted", t) })
  Spacer(1)
ThemedText(changelogLine, 1, 0)
  其中 changelogUrl = "https://kiana.dev/changelog"      // Rust：KIANA_API_BASE_URL/changelog；未配置时显示 "not configured"
       changelogLink = getCapabilities().hyperlinks ? hyperlink(theme.fg("accent", changelogUrl), changelogUrl)
                                                   : theme.fg("accent", changelogUrl)
       changelogLine = theme.fg("muted","Changelog: ") + changelogLink
DynamicBorder(text => theme.fg("warning", text))
```
**`showPackageUpdateNotification(packages)`**（**逐字顺序**）：
```
Spacer(1)
DynamicBorder(warning)
ThemedText(() => `${theme.bold(theme.fg("warning","Package Updates Available"))}\n` +
                  `${theme.fg("muted","Package updates are available. Run ")}${theme.fg("accent", `${APP_NAME} update --extensions`)}\n` +
                  `${theme.fg("muted","Packages:")}\n${packages.map(p => `- ${p}`).join("\n")}`, 1, 0)
DynamicBorder(warning)
```

**队列**：
- `getAllQueuedMessages()`：合并 session 队列（steering/followUp）与 `compactionQueuedMessages`。
- `clearAllQueues()`：清两处并返回内容。
- `updatePendingMessagesDisplay()`：steering → `` `Steering: ${msg}` ``；followUp → `` `Follow-up: ${msg}` ``；全部后面加 `` `↳ ${dequeueHint} to edit all queued messages` ``（`dequeueHint` 为 `keyText("app.message.dequeue")`）。
- `restoreQueuedMessagesToEditor(options?)`：以 `"\n\n"` 连接，写入编辑器，返回条数。
- `queueCompactionMessage(text, mode)`：push 并 `showStatus("Queued message for after compaction")`。
- `isExtensionCommand(text)`（4763–4771 行，**逐字**；旧版规格说"或 skill 命令"是**错的**，只看扩展命令）：
  ```ts
  if (!text.startsWith("/")) return false;
  const extensionRunner = this.session.extensionRunner;
  const spaceIndex = text.indexOf(" ");
  const commandName = spaceIndex === -1 ? text.slice(1) : text.slice(1, spaceIndex);
  return !!extensionRunner.getCommand(commandName);
  ```
  只在**流式响应中**的 `app.message.followUp`/steering 路径使用：是扩展命令就立即 `session.prompt(text)`（可插队执行），否则进 `queueCompactionMessage`。
  skill 命令、prompt 模板**不走**这条捷径（注释原文："This handles extension commands (execute immediately), prompt template expansion, and queueing" —— 提示模板展开与排队由 `session.prompt` 内部的其它分支处理）。
- `flushCompactionQueue(options?)`：把队列还原到编辑器后重新提交；失败 `` `Failed to send queued message${s}: ${msg}` ``。
- `flushPendingBashComponents()`：把 `pendingBashComponents` 移入 chat。

**选择器基础设施**：
```ts
private showSelector(create: (done: (result: unknown) => void) => {
  component: Component & {dispose?(): void}; focus?: Component }): void
```
用 `activeSelectorToken = {}` 做令牌保护（避免旧选择器的回调影响新选择器）；`disposeActiveSelector()` 调旧 dispose。

#### 2.50.9 `/settings`（`showSettingsSelector`，4901–5139 行）

构造 `SettingsConfig`/`SettingsCallbacks`（字段全集见 §2.34 与本节），使用 `SettingsSelectorComponent`。回调中需**逐字**包含：
- `` `HTTP idle timeout: ${formatHttpIdleTimeoutMs(t)}` ``
- `` `Cache warming: ${mode}` ``
- 切换 TUI 模式时若有活动 overlay → `"Close active overlays before changing TUI mode"`，否则 `switchTuiMode(mode)` 并 `` `TUI mode: ${mode}` ``
- 所有其它回调直接写 settings 并刷新（`applyRuntimeSettings()`）

#### 2.50.10 `/thinking`

`handleThinkingCommand(searchTerm?)`（**逐字**）：
```ts
const availableLevels = this.session.getAvailableThinkingLevels();
if (!searchTerm) { this.showThinkingSelector(); return; }
const normalized = searchTerm.trim().toLowerCase();
const level = availableLevels.find((candidate) => candidate.toLowerCase() === normalized);
if (!level) {
    this.showError(`Unknown thinking level "${searchTerm}". Available levels: ${availableLevels.join(", ")}.`);
    return;
}
this.selectThinkingLevel(level, false);
```
注意：匹配的是 **`availableLevels`**（不是全部 7 个 level），且 `searchTerm` 只在报错里保留**原样**（未 trim/lowercase）。

`selectThinkingLevel(level, persist)`：`try { session.setThinkingLevel(level, { persist }); footer.invalidate(); updateEditorBorderColor(); showStatus(persist ? \`Default thinking level: ${level}\` : \`Thinking level: ${level}\`) } catch { showError(message) }`。

`showThinkingSelector()`：`showSelector((done) => ...)`，`selectLevel = (level, persist) => { this.selectThinkingLevel(level, persist); done(); }`；构造 **6 个参数**（旧版规格漏了最后两个）：
```ts
new ThinkingSelectorComponent(
  this.session.thinkingLevel ?? DEFAULT_THINKING_LEVEL,       // 1 currentLevel
  this.session.getAvailableThinkingLevels(),                  // 2 availableLevels
  (level) => selectLevel(level, false),                       // 3 onSelect（不持久化）
  () => { done(); this.ui.requestRender(); },                 // 4 onCancel
  (level) => selectLevel(level, true),                        // 5 onSelectAsDefault（持久化 → "Default thinking level: ..."）
  this.settingsManager.getDefaultThinkingLevel() ?? DEFAULT_THINKING_LEVEL,  // 6 defaultThinkingLevel（用于 "· default" 标记）
);
return { component: selector, focus: selector };
```
`focus` 是 `selector` 本身（组件实现 `Focusable`），不是内部的 `SelectList`。

#### 2.50.11 `/model` 与 `/scoped-models`

`handleModelCommand(searchTerm?)`：无参数 → `showModelSelector()`；有参数 → `findExactModelMatch`。
`findExactModelMatch(searchTerm)`：
```
有网络时先 `showStatus("Refreshing model catalogs…")` 并 refreshModelCatalogs（15s）
超时 → `"Model refresh timed out; searching cached models."`
部分失败 → `Could not refresh ${keys}; searching cached models.`
```
`handleModelCommand` 无匹配 → `` `Model: ${model.id}` ``（提示当前模型）。
`updateAvailableProviderCount()`：更新 footer 的 provider 计数。
`showModelSelector(initialSearchInput?)`：`ModelSelectorComponent`；选择后保存运行时模型并提示（默认模型路径 `` `Default model: ${provider}/${id}` ``，否则 `` `Model: ${id}` ``）。
`showModelsSelector()`：`ScopedModelsSelectorComponent`，`onPersist` → `"Model selection saved to settings"`；`onChange` 即时生效；刷新状态文案见 §2.53 的 `setRefreshStatus`。

#### 2.50.12 `/trust`、`/login`、`/logout`

`maybeWarnAboutAnthropicSubscriptionAuth(key)`：`isAnthropicSubscriptionAuthKey(key)` 且未提示过 → `showWarning(ANTHROPIC_SUBSCRIPTION_AUTH_WARNING)`。
`maybeSaveImplicitProjectTrustAfterReload()`：`autoTrustOnReloadCwd` 命中 → 保存并返回 true；失败 `` `Could not save project trust after reload: ${msg}` ``。
`showTrustSelector()`：`TrustSelectorComponent`；保存后 `` `Saved trust decision: ${trusted ? "trusted" : "untrusted"}. Restart ${APP_NAME} for this to take effect.` ``。
`getLoginProviderOptions(authType?)` / `getLogoutProviderOptions()` / `findLoginProviderOptions(providerRef)`：构造 `AuthSelectorProvider[]`（含 `authTypes`、`subscription`、`status`）。
`handleLoginCommand(providerRef?)`：有 ref → 找 provider 后 `startProviderLogin`；否则 `showLoginAuthTypeSelector()`。
`startProviderLogin(providerOption, onBack?)`：按 `authTypes` 分派到 OAuth 或 API key 流程。
`showLoginAuthTypeSelector(providerOptions?)`：标题 `"Sign in with <name>"`；选项 `"Sign in with an account"` / `"Sign in with an API key"`；描述 `"Select authentication method for <name>:"` / `"Select authentication method:"`；空 → `"No login methods available."`。
`showLoginProviderSelector(authType?, initialSearchInput?)`：空文案 `"No account providers available."` / `"No API key providers available."` / `"No login providers available."`。
`showOAuthSelector(mode)`：`OAuthSelectorComponent`；`logout` 模式失败文案：
- `Could not read stored credentials: ${msg}`
- `No stored credentials to remove. /logout only removes credentials saved by /login; environment variables and models.json config are unchanged.`
- `Logged out of ${name}`
- `Removed stored API key for ${name}. Environment variables and models.json config are unchanged.`
- `Credentials removed for ${name}, but local model state could not be synchronized: ${msg}`
- `Logout failed: ${msg}`

`completeProviderAuthentication(providerId, providerName, ...)`（**逐字**）：
```
成功：`${actionLabel}. Credentials saved to ${getAuthPath()}`
选了模型：`${actionLabel}. Selected ${id}. Credentials saved to ${getAuthPath()}`
刷新中：`... Refreshing model catalog…`
llama.cpp → llamaCppPostLoginGuidance
无默认模型：`no default model is configured for provider "${id}". Use /model to select a model.`
无模型：`no models are available for that provider...`
默认模型不可用：`its default model "${id}" is not available...`
选择默认失败：`selecting its default model failed: ${e}. Use /model...`
刷新超时：`... its model catalog refresh timed out; using cached models.`
刷新失败：`... could not be refreshed; using cached models.` / `... could not be refreshed: ${msg}`
```
`showAmbientAuthDialog`：`` `${methodName} is configured outside ${APP_NAME}.` ``
`showApiKeyLoginDialog`：amazon-bedrock 追加说明：
```
"You can also use an AWS profile, IAM keys, or role-based credentials."
"See:"
`  ${docsPath}/providers.md`
```
错误：`Saved API key for ${name}, but local model state could not be synchronized: ${e}` / `Failed to save API key for ${name}: ${e}`。
`notifyAuthDialog(dialog, event)`（**逐字分派**，旧版只写了"驱动对应方法"）：
```ts
event.type === "auth_url"     → dialog.showAuth(event.url, event.instructions)
event.type === "device_code"  → dialog.showDeviceCode(event); dialog.showWaiting("Waiting for authentication...")
event.type === "info"         → dialog.showInfo(event.message, event.links)
else                          → dialog.showProgress(event.message)
```
`showAuthPrompt(dialog, prompt, providerId)`：无 `prompt.signal` → 直接返回响应；`signal.aborted` → **抛 `Error("Login cancelled")`**；
否则 `Promise.race([response, aborted])`，`aborted` 为在 `signal` 的 `abort` 上 `reject(new Error("Login cancelled"))` 的 Promise（`{once:true}`），
`finally` 里 `removeEventListener`。
`showAuthSelect` / `showAuthPrompt` / `notifyAuthDialog`：驱动 `LoginDialogComponent` 的对应方法。
`loginProvider(providerId, ...)`：`programStatus.setBlocked("login", { kind: "auth", message: `Log in to ${name}` })`。
`showLoginDialog(...)`：错误 `Logged in to ${name}, but local model state could not be synchronized: ${e}` / `Failed to login to ${name}: ${e}`。
`offerRadiusMcpServer(providerId, providerName)`：`Configure ${name} MCP in ${mcpPath}?`；失败 `Could not update ${mcpPath}: ${e}`。

#### 2.50.13 `/reload`

`handleReloadCommand()`（**逐字**）：
- 流式中 → `"Wait for the current response to finish before reloading."`
- 压缩中 → `"Wait for compaction to finish before reloading."`
- 显示 box：`"Reloading keybindings, extensions, skills, prompts, themes, and context files..."`
- 成功：`"Reloaded keybindings, extensions, skills, prompts, themes, and context files"`，若保存了项目信任则追加 `"; saved project trust"`
- 失败：`Reload failed: ${msg}`

#### 2.50.14 `/export` `/import` `/share` `/bug` `/copy` `/name`

- `handleExportCommand(text)`：`getPathCommandArgument(text, "/export")`；成功 `Session exported to: ${path}`；失败 `Failed to export session: ${msg}`。
- `getPathCommandArgument(text, command)`：`command` 之后的部分 trim；含空格时支持引号（去引号）；空 → undefined。
- `handleImportCommand(text)`：无参数 → `"Usage: /import <path.jsonl>"`；确认框标题 `"Import session"`、消息 `Replace current session with ${path}?`；取消 `"Import cancelled"`；成功 `Session imported from: ${path}`；失败 `Failed to import session: ${msg}`。
- `handleShareCommand()` → `shareSession({...})`（见 §2.49）。
- `handleBugCommand(hint)` → `reportBug({...}, hint)`。
- `handleCopyCommand(options: { flashConfirmation?: boolean; preferSelection?: boolean } = {})`（**逐字**）：
  ```ts
  if (options.preferSelection && this.ui instanceof TuiAltScreen && !this.ui.getCopyOnSelect() && this.ui.hasActiveSelection()) {
      await this.ui.copyActiveSelectionToClipboard();
      return;
  }
  const text = this.session.getLastAssistantText();
  if (!text) { this.showError("No agent messages to copy yet."); return; }
  try {
      await copyToClipboard(text);
      if (options.flashConfirmation && this.ui instanceof TuiAltScreen) this.ui.flash("Copied!");
      else this.showStatus("Copied last agent message to clipboard");
  } catch (error) { this.showError(error instanceof Error ? error.message : String(error)); }
  ```
  即：**优先复制 alt-screen 的鼠标选区**（且仅当 `copyOnSelect` 关闭、确有选区时），否则复制最后一条 assistant 文本；`key` 触发（`app.message.copy`）传 `{flashConfirmation:true, preferSelection:true}`，`/copy` 斜杠命令**不传参**（走 status 消息而非 flash）。旧版规格只写了 `/copy` 分支的文案，漏了两个选项与选区优先路径。
- `handleNameCommand(text)`：
  - 无参数 → `"Usage: /name <name>"`；已有名字则 `` `Session name: ${current}` ``
  - 规范化后不同 → `` `Session name was normalized from ${JSON.stringify(input)} to ${JSON.stringify(sessionName)}` ``
  - 成功 `` `Session name set: ${displayName}` ``

#### 2.50.15 `/session`（完整模板，**逐字**）

文本按需构建（`ThemedText(renderInfo, 1, 0)`，随主题变化重渲染）。`renderInfo()` 拼接顺序（**逐字**，`${fg("dim", X)}` 即 `theme.fg("dim", X)`）：

```
`${bold("Session Info")}\n\n`
if (sessionName)  `${fg("dim","Name:")} ${sessionName}\n`
`${fg("dim","File:")} ${stats.sessionFile ?? "In-memory"}\n`
`${fg("dim","ID:")} ${stats.sessionId}\n\n`

`${bold("Messages")}\n`
`${fg("dim","Total:")} ${totalMessages}\n`
`${fg("dim","User:")} ${userMessages}\n`
`${fg("dim","Assistant:")} ${assistantMessages}\n`
`${fg("dim","Tools:")} ${toolCalls} calls, ${toolResults} results\n\n`

`${bold("Tokens")}\n`
`${fg("dim","Input:")} ${promptTokens.toLocaleString()}\n`          // promptTokens = input + cacheRead + cacheWrite
if (promptTokens > 0 && (cacheRead > 0 || cacheWrite > 0)) {
  `  ${fg("dim","Cached:")} ${cacheRead.toLocaleString()} ${fg("dim", `(${((cacheRead/promptTokens)*100).toFixed(1)}%)`)}\n`
  `  ${fg("dim","Uncached:")} ${(input+cacheWrite).toLocaleString()}${cacheWrite > 0 ? ` ${fg("dim", `(${cacheWrite.toLocaleString()} written to cache)`)}` : ""}\n`
}
`${fg("dim","Output:")} ${output.toLocaleString()}\n`
`${fg("dim","Total:")} ${total.toLocaleString()}\n`

`\n${bold("Cache Warming")}\n`
`${fg("dim","Mode:")} ${cacheWarmingMode}\n`
`${fg("dim","Status:")} ${cacheWarmingStatus ? formatCacheWarmingStatus(cacheWarmingStatus) : "Inactive (cache warming unavailable)"}\n`
if (cacheWarmingStatus?.decision?.economicsAvailable) {
  `${fg("dim","Cache miss penalty:")} $${decision.missCost.toFixed(3)}\n`
  `${fg("dim","Refresh cost:")} $${decision.warmCost.toFixed(3)}\n`
}

if (stats.cost > 0 || cacheWaste.missedTokens > 0) {
  `\n${bold("Cost")}\n`
  `${fg("dim","Total:")} $${stats.cost.toFixed(3)}`
  if (usageBreakdown.length > 1 || usageBreakdown[0]?.key !== selectedModelKey)   // selectedModelKey = `${model?.provider}/${model?.id}`
    for (entry of usageBreakdown)
      `\n  ${fg("dim", `${entry.key}:`)} $${entry.cost.toFixed(3)} ${fg("dim", `(${formatTokens(entry.tokens)} tokens)`)}`
  if (cacheWaste.missedTokens > 0) {
    missLabel = missCount === 1 ? "1 miss" : `${missCount} misses`
    detail = `${missedTokens.toLocaleString()} tokens, ${missLabel}`
    missedCost >= 0.0001
      ? `\n${fg("dim","Cache Re-billed:")} $${missedCost.toFixed(3)} ${fg("dim", `(${detail})`)}`
      : `\n${fg("dim","Cache Re-billed:")} ${detail}`
  }
}
```
最后 `chatContainer.addChild(new Spacer(1))` → `ThemedText(renderInfo, 1, 0)` → `requestRender()`。

（`toLocaleString()` 为 JS 默认 locale 的分组格式（`en-US` 环境即 `1,234,567`），Rust 需实现同样的千分位；`toFixed(3)` 与 `toFixed(1)` 为四舍五入到定点小数。）

#### 2.50.16 `/changelog` `/hotkeys` `/new` `/debug` `/arminsayshi` `/dementedelves`

- `handleChangelogCommand()`：`Spacer(1)` → `DynamicBorder()` → `ThemedText(() => theme.bold(theme.fg("accent","What's New")), 1, 0)` → `Spacer(1)` → `Markdown(changelogMarkdown, 1, 1, getMarkdownThemeWithSettings())` → `DynamicBorder()`。`changelogMarkdown` = `parseChangelog(getChangelogPath())` 逆序、每项 `normalizeChangelogLinks(e.content, e)`、以 `"\n\n"` 连接；空 → `"No changelog entries found."`。
- `handleHotkeysCommand()`：**完整逐字模板**见下。`Spacer(1)` → `DynamicBorder()` → `ThemedText(() => theme.bold(theme.fg("accent","Keyboard Shortcuts")), 1, 0)` → `Spacer(1)` → `Markdown(hotkeys.trim(), 1, 1, ...)` → `DynamicBorder()`。
- `handleClearCommand()`：先 `clearStatusIndicator()`，再 `runtimeHost.newSession()`；成功 → `theme.fg("accent", "✓ New session started")` 的 ThemedText（`Spacer(1)` 前置）。
- `handleDebugCommand()`：**不写日期文件名**，直接写 `getDebugLogPath()`（即 `<agent-dir>/kiana-debug.log`）。内容：
  ```
  `Debug output at ${new Date().toISOString()}`
  `Terminal: ${width}x${height}`
  `Total lines: ${allLines.length}`
  ""
  "=== All rendered lines with visible widths ==="
  每行：`[${idx}] (w=${visibleWidth(line)}) ${JSON.stringify(line)}`
  ""
  "=== Agent messages (JSONL) ==="
  session.messages 每项 `JSON.stringify(msg)`
  ""
  ```
  以 `"\n"` 连接；`fs.mkdirSync(dirname, {recursive:true})` 后 `writeFileSync`。完成后加 `Spacer(1)` + `ThemedText(() => `${theme.fg("accent","✓ Debug log written")}\n${theme.fg("muted", debugLogPath)}`, 1, 1)`（注意 paddingY = 1）。
- `handleArminSaysHi()`：`if (playArmin3d(this.renderer)) return;` 否则 `Spacer(1)` + `new ArminComponent(this.ui)`，`requestRender()`。
- `handleDementedDelves()`：`Spacer(1)` + `new EarendilAnnouncementComponent()`，`requestRender()`。

**`/hotkeys` 模板（逐字，模板字符串原样，含开头换行）**：
```
\n**Navigation**
| Key | Action |
|-----|--------|
| `${cursorUp}` / `${cursorDown}` / `${cursorLeft}` / `${cursorRight}` | Move cursor / browse history |
| `${cursorWordLeft}` / `${cursorWordRight}` | Move by word |
| `${cursorLineStart}` | Start of line |
| `${cursorLineEnd}` | End of line |
| `${jumpForward}` | Jump forward to character |
| `${jumpBackward}` | Jump backward to character |
| `${pageUp}` / `${pageDown}` | Scroll by page |

**Editing**
| Key | Action |
|-----|--------|
| `${submit}` | Send message |
| `${newLine}` | New line${win32 ? " (Ctrl+Enter on Windows Terminal)" : ""} |
| `${deleteWordBackward}` | Delete word backwards |
| `${deleteWordForward}` | Delete word forwards |
| `${deleteToLineStart}` | Delete to start of line |
| `${deleteToLineEnd}` | Delete to end of line |
| `${yank}` | Paste the most-recently-deleted text |
| `${yankPop}` | Cycle through the deleted text after pasting |
| `${undo}` | Undo |

**Other**
| Key | Action |
|-----|--------|
| `${tab}` | Path completion / accept autocomplete |
| `${interrupt}` | Cancel autocomplete / abort streaming |
| `${clear}` | Clear editor (first) / exit (second) |
| `${exit}` | Exit (when editor is empty) |
| `${suspend}` | Suspend to background |
| `${cycleThinkingLevel}` | Cycle thinking level |
| `${cycleModelForward}` / `${cycleModelBackward}` | Cycle models |
| `${selectModel}` | Open model selector |
| `${expandTools}` | Toggle tool output expansion |
| `${toggleThinking}` | Toggle thinking block visibility |
| `${externalEditor}` | Edit message in external editor |
| `${copyMessage}` | Copy selection or last assistant message |
| `${followUp}` | Queue follow-up message |
| `${dequeue}` | Restore queued messages |
| `${pasteImage}` | Paste files on macOS, images, or text from clipboard |
| `/` | Slash commands |
| `!` | Run bash command |
| `!!` | Run bash command (excluded from context)
```
扩展快捷键存在时再追加：
```
\n**Extensions**
| Key | Action |
|-----|--------|
| `${formatKeyText(key,{capitalize:true})}` | `${shortcut.description ?? shortcut.extensionPath}` |
```
（`key` 来自 `extensionRunner.getShortcuts(keybindings.getEffectiveConfig())` 的 Map 键。）

#### 2.50.17 bash 与 `/compact`

`handleBashCommand(command, excludeFromContext)`（**逐条**）：
1. `extensionRunner.emitUserBash({ type: "user_bash", command, excludeFromContext, cwd: sessionManager.getCwd() })`；**抛异常则直接 return**（不落回本地执行）。
2. 若 `eventResult?.result` 是完整结果：创建 `BashExecutionComponent(command, ui, excludeFromContext, outputPad)`；`session.isStreaming` → 加到 `pendingMessagesContainer` 并 push 到 `pendingBashComponents`；否则加 `chatContainer`；`result.output` 非空 → `appendOutput`；`setComplete(result.exitCode, result.cancelled, result.truncated ? {truncated:true, content:result.output} : undefined, result.fullOutputPath)`；`session.recordBashResult(command, result, { excludeFromContext })`；清 `bashComponent`；`requestRender()`；return。
3. 否则走正常执行：`isDeferred = session.isStreaming`；同样创建组件并挂到 pending 区或 chat；`requestRender()`。
4. `await session.executeBash(command, chunk => { bashComponent?.appendOutput(chunk); requestRender(); }, { excludeFromContext, operations: eventResult?.operations })`；成功 → `setComplete(exitCode, cancelled, truncated?…, fullOutputPath)`。
5. 异常 → `setComplete(undefined, false)` + `showError(`Bash command failed: ${msg ?? "Unknown error"}`)`。
6. 最后清 `bashComponent` 并 `requestRender()`。

`handleCompactCommand(customInstructions?)`：`clearStatusIndicator()`；`await session.compact(customInstructions)`；**异常静默吞掉**（结果通过事件呈现）。

`stop(fullscreenExitOutput = settingsManager.getFullscreenExitOutput())`（**逐条顺序**）：
```
1. disposeActiveSelector()
2. if (settingsManager.getShowTerminalProgress()) ui.terminal.setProgress(false)
3. clearStatusIndicator()
4. themeController.disableAutoSync()
5. clearExtensionTerminalInputListeners()
6. footer.dispose()
7. footerDataProvider.dispose()
8. unsubscribe?.()
9. if (isInitialized) { stopInteractiveTui(fullscreenExitOutput); isInitialized = false }
10. unregisterSignalHandlers()
```
（注意：`stop()` **不**调用 `stopThemeWatcher()` 也不清 widget；那些在 `shutdown()` 里。）

#### 2.50.18 扩展 UI 上下文（`createExtensionUIContext`，2633–2700 行）

**实际返回对象的成员（逐字，共 **28** 项——旧版写 32 是错的；顺序即源码顺序；类型定义见 `core/extensions/types.ts: ExtensionUIContext`，本单元只负责实现）**：
```ts
return {
  select: (title, options, opts) => this.showExtensionSelector(title, options, opts),
  confirm: (title, message, opts) => this.showExtensionConfirm(title, message, opts),
  input: (title, placeholder, opts) => this.showExtensionInput(title, placeholder, opts),
  notify: (message, type) => this.showExtensionNotify(message, type),
  onTerminalInput: (handler) => this.addExtensionTerminalInputListener(handler),
  setStatus: (key, text) => this.setExtensionStatus(key, text),
  setWorkingMessage: (message) => {
    this.workingMessage = message;
    if (this.activeStatusIndicator?.kind === "working")
      this.activeStatusIndicator.setMessage(message ?? this.defaultWorkingMessage);
  },
  setWorkingVisible: (visible) => this.setWorkingVisible(visible),
  setWorkingIndicator: (options) => this.setWorkingIndicator(options),
  setHiddenThinkingLabel: (label) => this.setHiddenThinkingLabel(label),
  setWidget: (key, content, options) => this.setExtensionWidget(key, content, options),
  setFooter: (factory) => this.setExtensionFooter(factory),
  setHeader: (factory) => this.setExtensionHeader(factory),
  setTitle: (title) => this.ui.terminal.setTitle(title),          // 直接设终端标题
  custom: (factory, options) => this.showExtensionCustom(factory, options),
  pasteToEditor: (text) => this.editor.handleInput(`\x1b[200~${text}\x1b[201~`),   // 走 bracketed paste
  setEditorText: (text) => this.editor.setText(text),
  getEditorText: () => this.editor.getExpandedText?.() ?? this.editor.getText(),   // 优先"展开后的"全文
  editor: (title, prefill) => this.showExtensionEditor(title, prefill),
  addAutocompleteProvider: (factory) => { this.autocompleteProviderWrappers.push(factory);
                                          this.setupAutocompleteProvider(); },       // 立即重建
  setEditorComponent: (factory) => this.setCustomEditorComponent(factory),
  getEditorComponent: () => this.editorComponentFactory,
  get theme() { return theme; },                                                 // getter，非字段
  getAllThemes: () => getAvailableThemesWithPaths(),                             // 带 path
  getTheme: (name) => getThemeByName(name),
  setTheme: (themeOrName) => {
    if (themeOrName instanceof Theme) return this.themeController.setThemeInstance(themeOrName);
    const result = this.themeController.setThemeName(themeOrName);
    if (result.success && this.settingsManager.getTheme() !== themeOrName) this.settingsManager.setTheme(themeOrName);
    return result;                                                               // {success, error?}
  },
  getToolsExpanded: () => this.toolOutputExpanded,
  setToolsExpanded: (expanded) => this.setToolsExpanded(expanded),
};
```
> **勘误**：旧版本本节列出的 `addMarkdownTransformer` 与 `registerShortcuts` **在源码中不存在**（`ExtensionUIContext` 里也没有这两个方法）；扩展的 Markdown 变换器走 `extensionRunner.getMarkdownTransformers()`，快捷键走 `extensionRunner.getShortcuts()`（见 §2.50.20 `setupExtensionShortcuts`）。另需补齐 `pasteToEditor`、`setEditorText`、`getEditorText`、`setTitle`、`getAllThemes`、`getTheme`、`setTheme`、`addAutocompleteProvider`、`getEditorComponent`、`theme` getter、`setWorkingMessage`、`setWorkingVisible`、`getToolsExpanded`、`setToolsExpanded`。
> `setTheme` 的 `Theme` 分支**不写设置**（仅切换当前实例）；字符串分支成功时写入 `settingsManager`（先比较避免无谓写入）。

**对话框栈（逐字）**：
- `pushExtensionDialog(component, blocked?)`：`disposeActiveSelector()` → `extensionDialogs.push({ component, blocked })` → `showTopExtensionDialog()`。
- `removeExtensionDialog(component)`：按 `component` 恒等找下标，找不到直接 return；`splice(index, 1)`；**仅当** `index === extensionDialogs.length`（即移除的是**栈顶**）才 `showTopExtensionDialog()`。
- `showTopExtensionDialog()`：`top = extensionDialogs.at(-1)`；`component = top?.component ?? this.editor`；`editorContainer.clear()` → `addChild(component)` → `ui.setFocus(component)` → `programStatus.setBlocked("extension-dialog", top?.blocked)` → `requestRender()`。
- `showExtensionSelector(title, options, opts?, blocked = { kind: "question", message: title })`：`opts?.signal?.aborted` 时立即 `resolve(undefined)`（**不弹窗**）；`finish()` 负责 `removeEventListener` + `selector.dispose()` + `removeExtensionDialog` + resolve；`ExtensionSelectorComponent(..., { tui, timeout: opts?.timeout, onToggleToolsExpanded: () => this.toggleToolOutputExpansion() })`。
- `showExtensionConfirm(title, message, opts?)`：**复用 select**，标题为 `` `${title}\n${message}` ``、选项 `["Yes","No"]`、`blocked = { kind: "permission", message: title }`；返回 `result === "Yes"`。
- `showExtensionInput` / `showExtensionEditor` 的 `blocked` 均为 `{ kind: "question", message: title }`；后者把 `settingsManager.getExternalEditorCommand()` 作为第 8 个参数传入（Ctrl+G 外部编辑器）。

`showExtensionCustom` 的实现要点：`factory(tui, theme, keybindings, done)` 可返回 **Promise**（类型上是 `(Component & {dispose?}) | Promise<...>`）；`done(result)` 复位编辑器并 resolve；`options` 支持 `overlay`、`overlayOptions`（可为函数）、`onHandle(handle)`。

**测试**（`interactive-mode-extension-dialogs.test.ts`，4 条，实际名称）：`shows the earlier confirm again after the later one is answered`、`removes a hidden dialog when its signal aborts`、`stacks concurrent inline custom components`、`shows a confirm opened from an inline custom component and then restores the component`。

---

#### 2.50.19 崩溃/错误提示辅助

- `getCrashExtensionHint(error)`：`formatCrashExtensionHint(findExtensionStackMatches(error?.stack, session.resourceLoader.getExtensions().extensions))`；异常 → `undefined`。
- `recordCrash(kind, error)`：调用 `core/crash-log.ts` 的 `recordCrash({kind, error, sessionFile, cwd})`，返回是否写入成功（异常 → false）。
- `crashReportInstructions()`（**逐字**）：
  ```
  resume = session.sessionFile ? `run \`${APP_NAME} -r\` to resume the session, then` : "start kiana and"
  return `To report this crash: ${resume} run /bug. The crash details are attached automatically.`
  ```
- `suggestBugReport()`：仅一次（`bugReportHintShown`）；文案 `` `If this looks like a ${APP_NAME} bug, /bug sends a report to the developers.` ``（`muted`，`outputPad`）。
- `maybeSuggestBugReport(message)`：条件 —— `stopReason === "error"`、非 `isRetryableAssistantError(message)`、`errorMessage` 不匹配 `/\b(?:abort(?:ed)?|cancel(?:l?ed)?)\b/i`、`maybeShowInstallChangeWarning()` 返回 false。
- `maybeShowInstallChangeWarning()`：`installChangeWarningShown` 已置 → **返回 true**（表示已处理）；`detectInstallChange()` 为 undefined → false；否则置位并：
  ```
  cause = change.kind === "updated"
    ? `${APP_NAME} was updated to ${change.version} while this session was running (${VERSION})`
    : `The ${APP_NAME} installation this session runs from was removed or replaced`
  restart = resumeCommand ? `Restart with \`${resumeCommand}\` to continue this session.` : `Restart ${APP_NAME}.`
  showWarning(`${cause}. Features that load code on demand can fail until restart. ${restart}`)
  ```
- `renderCurrentSessionState()`：清 `loadedResourcesContainer` / `chatContainer` / `pendingMessagesContainer`，清 `compactionQueuedMessages`、`streamingComponent`、`streamingMessage`、`pendingTools`，再 `renderInitialMessages()`。
- `getRegisteredToolDefinition(toolName)`：`extensionRunner.resolveToolRenderers(toolName, () => withBuiltInRenderers(toolName, session.getToolDefinition(toolName)))`。

#### 2.50.20 扩展 UI 上下文（续）与对话框栈

```ts
private pushExtensionDialog(component: ExtensionDialogComponent, blocked?: BlockedStatus): void;
private removeExtensionDialog(component: ExtensionDialogComponent): void;
private showTopExtensionDialog(): void;
private showExtensionSelector(title, options, opts?): Promise<string | undefined>;
private showExtensionConfirm(title, message, opts?): Promise<boolean>;
private showExtensionInput(title, placeholder?, opts?): Promise<string | undefined>;
private showExtensionEditor(title, prefill?): Promise<string | undefined>;
private showExtensionNotify(message, type?): void;
private showExtensionCustom<T>(factory, opts?): Promise<T>;
private showExtensionError(extensionPath, error, stack?): void;
private setCustomEditorComponent(factory: EditorFactory | undefined): void;
private setExtensionWidget(key, content, options?): void;
private setExtensionFooter(factory?): void;
private setExtensionHeader(factory?): void;
private addExtensionTerminalInputListener(listener): () => void;
```

对话框栈行为（**是测试直接覆盖的行为**）：
- `pushExtensionDialog(component, blocked)`：先 `disposeActiveSelector()`（关闭编辑器槽里正在显示的内置选择器），把**当前可见**对话框从 `editorContainer` 摘掉（但不 dispose、不 reject），压栈；`programStatus.setBlocked("extension-dialog", blocked)`（**source 常量逐字为 `"extension-dialog"`**；`blocked` 为 undefined 时会清除该 source）；`showTopExtensionDialog()`。
- `removeExtensionDialog(component)`：从栈中移除该组件；若它是栈顶则 `showTopExtensionDialog()` 显示新的栈顶；栈空则恢复 `this.editor` 并 `setFocus(editor)`。
- 每个对话框的 Promise **恰好 resolve 一次**；被隐藏的对话框仍可应答（因为组件仍持有状态与输入通道）。
- 扩展传入的 `AbortSignal` 已中止 → 移除该对话框（`removes a hidden dialog when its signal aborts`）。
- `showExtensionCustom` 支持**嵌套**：在自定义组件内部再次调用 `ui.confirm(...)` 会压入新对话框，结束后恢复原组件（`shows a confirm opened from an inline custom component and then restores the component`、`stacks concurrent inline custom components`）。

`setExtensionWidget(key, content, options?)`（**逐字**）：
- `placement = options?.placement ?? "aboveEditor"`（取值 `"aboveEditor" | "belowEditor"`，**不是** `"above"/"below"`）。
- 先 `removeExisting(this.extensionWidgetsAbove)` **和** `removeExisting(this.extensionWidgetsBelow)`（同一个 key 在两个 map 中都会被移除，且 `dispose?.()`）；只有随后才决定放进哪个 map。
- `content === undefined` → `renderWidgets()` 后 return（即仅删除）。
- `content` 为 `string[]` 时先 `content.slice(0, InteractiveMode.MAX_WIDGET_LINES)` 逐行 `new Text(line, 1, 0)` 包进 `Container`；**若 `content.length > MAX_WIDGET_LINES`** 再追加 `new ThemedText(() => theme.fg("muted", "... (widget truncated)"), 1, 0)`（**逐字**，缺这句会让超长 widget 静默截断）。
- 否则 `component = content(this.ui, theme)`（工厂函数）。
- 放入 `placement === "belowEditor" ? extensionWidgetsBelow : extensionWidgetsAbove`，然后 `renderWidgets()`。
- `clearExtensionWidgets()`：遍历两个 map 逐个 `dispose?.()` 后清空（`setExtensionFooter(undefined)` 不调用它，仅由 `stop()`/切换会话路径使用）。

`setExtensionFooter(factory)`（**逐字**）：`this.customFooter?.dispose?.()` → `footerContainer.clear()` → 有 factory：`customFooter = factory(this.ui, theme, this.footerDataProvider)`（**第三个参数是 `ReadonlyFooterDataProvider`**，供自定义 footer 读 git 分支 / 扩展状态 / context usage）并 `addChild`；否则 `customFooter = undefined` 并 `addChild(this.footer)`（内置）。最后 `requestRender()`。
`setExtensionHeader(factory)`：**若 `builtInHeader` 尚未创建（早期初始化阶段）直接 return**；否则 dispose 旧 `customHeader`，在 `headerContainer.children` 里按原下标原位替换（`customHeader || builtInHeader` 的 index），保持 header 顺序；`undefined` → 恢复内置。
`showExtensionError(extensionPath, error, stack?)`（**逐字**）：
```ts
const errorMsg = `Extension "${extensionPath}" error: ${error}`;
this.chatContainer.addChild(new ThemedText(() => theme.fg("error", errorMsg), 1, 0));
if (stack) {
  const stackLines = stack.split("\n").slice(1);            // 跳过第一行（与 errorMsg 重复）
  if (stackLines.length > 0) {
    const renderStack = () => stackLines.map((line) => theme.fg("dim", `  ${line.trim()}`)).join("\n");
    this.chatContainer.addChild(new ThemedText(renderStack, 1, 0));
  }
}
this.ui.requestRender();
```
（**不是** `Warning:` 前缀，也**不**包含路径以外的"managed tool"格式；错误行用 `error` 色，栈行用 `dim` + 两空格缩进 + 每行 `trim()`。）

`setCustomEditorComponent(factory)`：`undefined` → `this.editor = this.defaultEditor`；否则 `factory(tui, theme, keybindings)` 创建新编辑器，保留文本/历史，重新绑定所有 `onAction`、`onSubmit`、`onChange`、`onEscape`、`onCtrlD`、`onPasteImage`，并 `setAutocompleteProvider(provider)`；`editorContainer` 中替换；`ui.setFocus(newEditor)`。

---


#### 2.50.21 资源列表与路径显示（`showLoadedResources` 及其辅助函数）

**开关逻辑**
```
showListing     = options?.force || shouldShowStartupDetails()
showDiagnostics = showListing || options?.showDiagnosticsWhenQuiet === true
if (!showListing && !showDiagnostics) return
loadedResourcesContainer.clear()          // 该容器与 chat 分离，chat 清空不影响它
```
`getStartupExpansionState() = options.verbose || toolOutputExpanded`
`shouldShowStartupHeader()  = options.verbose === true || settingsManager.getQuietStartup() !== true`
`shouldShowStartupDetails() = options.verbose === true || settingsManager.getQuietStartup() === false`
（`quietStartup` 三态：`true` 只留 header 隐藏细节；`"header"` 连 header 一起隐藏；`false` 全显示。）

**通用渲染件**
```
sectionHeader(name, color = "mdHeading") = theme.fg(color, `[${name}]`)
formatCompactList(items, {sort = true}) =
   labels = items.map(trim).filter(nonEmpty); sort !== false 时 localeCompare 排序
   theme.fg("dim", `  ${labels.join(", ")}`)
addLoadedSection(name, collapsedBody, expandedBody = collapsedBody, color = "mdHeading") =
   ExpandableText(() => `${sectionHeader(name,color)}\n${collapsedBody()}`,
                  () => `${sectionHeader(name,color)}\n${expandedBody()}`,
                  getStartupExpansionState(), 0, 0)
   + Spacer(1)
```

**章节顺序（showListing 时）**：`[Context]`、`[Skills]`、`[Prompts]`、`[Extensions]`，每节前可有一个 `Spacer(1)`（Context 节前显式加，其它节由 `addLoadedSection` 尾部加）。

| 章节 | 折叠态 | 展开态 |
|---|---|---|
| `Context` | `formatCompactList(contextFiles.map(formatContextPath), {sort:false})` | 每行 `theme.fg("dim", `  ${formatDisplayPath(f.path)}`)` |
| `Skills` | `formatCompactList(skills.map(s => s.name))` | `formatScopeGroups(groups, {formatPath: formatDisplayPath, formatPackagePath: getShortPath})` |
| `Prompts` | `` formatCompactList(templates.map(t => `/${t.name}`)) `` | 同上，但路径替换为 `` `/${template.name}` `` |
| `Extensions` | `formatCompactList(getCompactExtensionLabels(extensions))` | `formatScopeGroups(..., {formatPath: formatExtensionDisplayPath, formatPackagePath: formatExtensionDisplayPath ∘ getShortPath})` |

`contextFiles` 组成顺序（**决定渲染顺序**）：`systemPromptSource`（若有）→ `getAppendSystemPromptSources()` → `getAgentsFiles().agentsFiles`。

**诊断章节（showDiagnostics 时，顺序固定）**：`[Skill conflicts]`、`[Prompt conflicts]`、`[Extension issues]`、`[Theme conflicts]`，每节：
```
ThemedText(() => `${theme.fg("warning", `[${name}]`)}\n${formatDiagnostics(list, sourceInfos)}`, 0, 0) + Spacer(1)
```
`Extension issues` 的诊断来源**按序拼接**：`extensionsResult.errors` → `extensionsResult.warnings` → `extensionRunner.getCommandDiagnostics()` → `getBuiltInCommandConflictDiagnostics(extensionRunner)` → `extensionRunner.getShortcutDiagnostics()`。

**`formatDiagnostics(diagnostics, sourceInfos)`**（**逐字**）：先按 `d.collision.name` 分组（仅 `d.type === "collision" && d.collision`），其余进 `otherDiagnostics`：
```
每个碰撞组：
  `  "${name}" collision:`                                   （warning）
  `    ${theme.fg("success","✓")} ${formatPathWithSource(winnerPath, findSourceInfoForPath(...))}`   （dim 包裹）
  每个 loser：
  `    ${theme.fg("warning","✗")} ${formatPathWithSource(loserPath, ...)} (skipped)`                  （dim 包裹）
其它诊断：
  有 path：
    `  ${formattedPath}`（error→"error"，否则 "warning"）
    `    ${d.message}`（同色）
  无 path：
    `  ${d.message}`
```
**`formatPathWithSource(p, sourceInfo)`**：有 sourceInfo → `` `${labelText} ${getShortPath(p, sourceInfo)}` ``，`labelText = scopeLabel ? `${label} (${scopeLabel})` : label`；否则 `formatDisplayPath(p)`。

**`getDisplaySourceInfo(sourceInfo)`**：
| source | scope | 返回 |
|---|---|---|
| `local` | `user` | `{label:"user", color:"muted"}` |
| `local` | `project` | `{label:"project", color:"muted"}` |
| `local` | `temporary` | `{label:"path", scopeLabel:"temp", color:"muted"}` |
| `local` | 其他 | `{label:"path", color:"muted"}` |
| `cli` | 任意 | `{label:"path", scopeLabel: scope==="temporary" ? "temp" : undefined, color:"muted"}` |
| 其他（npm:/git:） | 任意 | `{label: source, scopeLabel: user/project/temp/undefined, color:"accent"}` |

**`getScopeGroup(sourceInfo)`**：`source === "cli" || scope === "temporary"` → `"path"`；`user` → `"user"`；`project` → `"project"`；否则 `"path"`。

**`buildScopeGroups(items)`**：固定返回顺序 `[project, user, path]`（过滤空组）；每个组内 `paths` 与 `packages: Map<source, items[]>`。
**`formatScopeGroups(groups, options)`**（**逐字**）：
```
`  ${theme.fg("accent", group.scope)}`
paths 按 path localeCompare 排序：`theme.fg("dim", `    ${options.formatPath(item)}`)`
packages 按 source localeCompare 排序：
  `    ${theme.fg("mdLink", source)}`
  组内项按 path 排序：`theme.fg("dim", `      ${options.formatPackagePath(item, source)}`)`
```

**路径格式化**
- `formatDisplayPath(p)`：`p.startsWith(home)` → `` `~${p.slice(home.length)}` ``。
- `formatExtensionDisplayPath(p)`：再 `replace(/\/index\.ts$/,"")`、`replace(/\/index\.js$/,"")`。
- `formatContextPath(p)`：绝对化后 `getCwdRelativePath(absolute, cwd)`；不可相对化则 `formatDisplayPath(absolute)`。
- `getShortPath(fullPath, sourceInfo)`：
  1. 反斜杠转 `/`；
  2. 若 `baseDir` 存在且 `isPackageSource`：`baseDir` 匹配 `^(.*\/node_modules)\/(@?[^/]+(?:\/[^/]+)?)$` 且 fullPath 同根 → `posix.relative(baseDir, fullPath)`；否则若 `relative(resolve(baseDir), resolve(fullPath))` 非空、非 `.`、不以 `..` 开头、非绝对 → 返回之（转 `/`）；
  3. `source` 以 `npm:` 开头且路径匹配 `node_modules\/(@?[^/]+(?:\/[^/]+)?)\/(.*)` → 第 2 组；
  4. `source` 以 `git:` 开头且匹配 `git\/[^/]+\/[^/]+\/(.*)` → 第 1 组；
  5. 否则 `formatDisplayPath(fullPath)`。
- `getCompactPathLabel(path, sourceInfo)`：`getShortPath` 后按 `/` 分段，去掉空段与 `"~"`，返回最后一段（无段则返回整个 shortPath）。
- `getCompactPackageSourceLabel(sourceInfo)`：`npm:` 前缀去掉；否则 `parseGitUrl(source)` 的 `path || source`；否则 `source`。
- `getCompactExtensionLabel(path, sourceInfo)`：非 package → `getCompactPathLabel`；package → `sourceLabel`，若 shortPath（去掉 `extensions/` 前缀后）`path.posix.parse` 的 `name === "index"` → `sourceLabel` 或 `` `${sourceLabel}:${dir}` ``，否则 `` `${sourceLabel}:${packagePath}` ``。
- `getCompactNonPackageExtensionLabel(path, index, allPaths)`：从尾部取 1..n 段，找到在**所有**其它项中不重复的最短后缀；都不唯一则用完整段。
- `getCompactExtensionLabels(extensions)`：非 package 项先构造 segments（若长度 > 1 且末段是 `index.ts`/`index.js` 则 pop）；每个扩展按上面两条规则取标签。
- `findSourceInfoForPath(p, sourceInfos)`：精确命中；否则逐级向上找父目录（`while (current.includes("/"))`）。
- `isPackageSource(sourceInfo)`：`source.startsWith("npm:") || source.startsWith("git:")`。

**与内置命令冲突诊断**（`getBuiltInCommandConflictDiagnostics`）：
```
Extension command '/${name}' conflicts with built-in interactive command. Skipping in autocomplete.
Extension command '/${name}' conflicts with built-in interactive command. Available as '/${invocationName}'.
```
（第二条用于 `name !== invocationName` 的情况。）

**测试覆盖点**（`interactive-mode-status.test.ts` 的 `showLoadedResources` describe，共 20 条）：compact 列表默认折叠、展开、verbose 时展开、扩展名缩写、混合布局、同名 index.ts 逐级消歧、npm 兄弟扩展相对声明包、Windows 路径、context 相对 cwd、system prompt 在项目 context 之前、quiet startup 下隐藏/保留/仅 header、reload 时不显示 verbose。

---

### 编号说明（§2.51–§2.59）

> 以下 9 个文件与 §2.19–§2.49 同级（均在 `components/` 下），编号接在 §2.50 之后仅为写作顺序，**不代表**它们依赖 `interactive-mode.ts`。实际阅读顺序上应先读它们，再读 §2.50。

### 2.51 `components/tool-execution.ts`（417 行）

```ts
export type { ToolRenderers };   // re-export from ../../../core/extensions/types.ts
export interface ToolExecutionOptions {
  showImages?: boolean;      // 默认 true
  imageWidthCells?: number;  // 默认 60
  outputPad?: number;        // 默认 1
}
export class ToolExecutionComponent extends Container {
  constructor(toolName: string, toolCallId: string, args: any, options?: ToolExecutionOptions,
              toolDefinition: ToolRenderers | ToolDefinition<any,any,any> | undefined, ui: TUI, cwd: string);
  updateArgs(args: any): void;
  markExecutionStarted(): void;
  setArgsComplete(): void;
  updateResult(result: {content: Array<{type:string; text?:string; data?:string; mimeType?:string}>;
                         details?: any; isError: boolean; durationMs?: number}, isPartial?: boolean): void;
  setExpanded(expanded: boolean): void;
  setOutputPad(outputPad: number): void;
  setShowImages(show: boolean): void;
  setImageWidthCells(width: number): void;
  override invalidate(): void;
  override render(width: number): string[];
  override handleMouse(event: TuiMouseEvent): ReturnType<Container["handleMouse"]>;
}
```

**模块常量**：`const FALLBACK_PREVIEW_LINES = 10;`

**时长显示（旧版规格完全漏掉，但属于本单元可见输出）**：`tool-execution.ts` 不自带格式化逻辑，它把 `durationMs` 透传给 `core/tools/render-utils.ts`：
```ts
export function formatToolDuration(ms: number): string {
    const seconds = ms / 1000;
    if (seconds < 60) return `${seconds.toFixed(1)}s`;
    const totalSeconds = Math.floor(seconds);
    const minutes = Math.floor(totalSeconds / 60);
    const remainder = totalSeconds % 60;
    if (minutes < 60) return `${minutes}m ${remainder}s`;
    return `${Math.floor(minutes / 60)}h ${minutes % 60}m ${remainder}s`;
}
interface ToolTimerState { startedAt?: number; endedAt?: number; interval?: ReturnType<typeof setInterval> }
export function startToolTimer(context): void;      // executionStarted && startedAt===undefined → startedAt=Date.now()
export function toolTimerText(context, isPartial): string | undefined;
```
`toolTimerText` 的**精确语义**（决定工具行尾部文本）：
1. `startedAt !== undefined && isPartial && !state.interval` → `setInterval(() => context.invalidate(), 1000)`（每秒重绘）。**注意：interval 存进 `state`，必须随组件销毁清理。**
2. `if (!isPartial || context.isError)` → `state.endedAt ??= Date.now()`，并 `clearInterval` 置 `undefined`。
3. **最终结果且有记录时长** → `` `Took ${formatToolDuration(context.durationMs)}` ``（注释原文："A final result's recorded duration wins: it is monotonic and survives reloads."）。
4. `startedAt === undefined` → `undefined`（无计时行）。
5. 否则 → `` `${isPartial ? "Elapsed" : "Took"} ${formatToolDuration(endTime - state.startedAt)}` ``，`endTime = state.endedAt ?? Date.now()`。
   即：**流式期间是 `Elapsed 4.2s`，结束后变 `Took 4.2s`**；错误结果提前停表但仍显示 `Took`。
`formatToolDuration` 的分支边界（测试已覆盖，见 §7.3 `test.each` 十例）：`59.999s` 仍按 `< 60` 走 `toFixed(1)` → `60.0s`；`60s` 起走 `1m 0s`；`3_599_999ms` → `59m 59s`；`3_600_000ms` → `1h 0m 0s`。

**私有字段（Rust 结构体逐字段对应）**：
```
contentBox: Box; contentText: Text; contentTextRegion: MouseRegion; selfRenderContainer: Container;
selfRenderHeight = 0; callRendererComponent?: Component; resultRendererComponent?: Component;
rendererState: any = {}; imageComponents: Image[] = []; 
imageSources: Array<{data:string; mimeType:string; widthCells:number}> = [];
imageSpacers: Spacer[] = []; toolName: string; toolCallId: string; args: any; expanded = false;
showImages: boolean; imageWidthCells: number; outputPad: number; isPartial = true;
toolDefinition?: ToolRenderers; ui: TUI; cwd: string; executionStarted = false; argsComplete = false;
argsPending = false; result?: {content; isError; details?; durationMs?}; hideComponent = false;
```

**构造顺序（逐字，影响渲染）**：
1. 赋值字段（默认值如上）。
2. `this.addChild(new Spacer(1))`。
3. 始终创建全部三种外壳：
   - `contentBox = new Box(1, 1, (text) => theme.bg("toolPendingBg", text))`
   - `contentText = new Text("", 1, 1, (text) => theme.bg("toolPendingBg", text))`
   - `contentTextRegion = this.createResultRegion(this.contentText)`
   - `selfRenderContainer = new Container()`
4. 若有 `toolDefinition`：`addChild(getRenderShell() === "self" ? selfRenderContainer : contentBox)`；否则 `addChild(contentTextRegion)`。
5. `this.updateDisplay()`。

**`getRenderShell()`**：`toolDefinition?.renderShell ?? "default"`。
**`hasRendererDefinition()`**：`toolDefinition !== undefined`。

**`getRenderContext(lastComponent)` 返回对象（逐字字段名）**：
```
{ args, toolCallId, invalidate: () => { this.invalidate(); this.ui.requestRender(); }, lastComponent,
  state: this.rendererState, cwd, executionStarted, argsComplete, isPartial, expanded, showImages,
  isError: this.result?.isError ?? false,
  durationMs: this.isPartial ? undefined : this.result?.durationMs, outputPad }
```
> `durationMs` 在 partial 时**恒为 undefined**，即使 `result.durationMs` 有值。

**`createCallFallback()`**：`new Text(formatToolCallWithArgs(this.toolName, this.args, theme, this.expanded), 0, 0)`。
`formatToolCallWithArgs`（在 `core/tools/render-utils.ts`，本单元消费）：
```
header = theme.fg("toolTitle", theme.bold(title))
args == null → header
entries = (object && !Array) ? Object.entries(args) : [["args", args]]
entries.length === 0 → header
expanded:
  lines = entries.map(([k,v]) => `  ${k}: ${typeof v === "string" ? v
            : (JSON.stringify(v,null,2) ?? String(v))}` 经 replaceTabs(→3空格) 与去 \r，
          再把 "\n" 替换为 "\n    ")
  → `${header}\n${theme.fg("muted", lines.join("\n"))}`
collapsed:
  pairs = entries.map(([k,v]) => `${k}=${JSON.stringify(v) ?? String(v)}`).join(" ")
  preview = pairs.length > 100 ? `${pairs.slice(0,97)}...` : pairs      // COLLAPSED_ARGS_CHARS = 100
  → `${header} ${theme.fg("muted", preview)}`
```

**`createResultFallback()`**：
```
output = this.getTextOutput(); if (!output) return undefined
lines = output.split("\n")
displayLines = expanded ? lines : lines.slice(0, 10)
remaining = lines.length - displayLines.length
text = displayLines.map(line => theme.fg("toolOutput", line)).join("\n")
if (remaining > 0)
  text += `${theme.fg("muted", `\n... (${remaining} more lines,`)} ${keyHint("app.tools.expand","to expand")}${theme.fg("muted", ")")}`
return new Text(text, 0, 0)
```
（展开提示串**逐字**：`... (N more lines,` + 空格 + 键位提示 + `to expand` + `)`。）

**`createResultRegion(component)`**：`new MouseRegion(component, event => { if (!this.result || event.type !== "click" || event.button !== "left") return undefined; this.setExpanded(!this.expanded); return { handled: true }; })`。
注意：**结果区在 `this.result` 存在后可点击折叠/展开；调用区也用同一个包装**。

**`updateArgs(args)`**：`this.args = args; this.argsPending = true; this.ui.requestRender();`
> 注释原文：每次 `render()` 开头若 `argsPending` 才 `updateDisplay()`，从而使流式参数增量每帧只渲染一次（issue #9265）。

**`markExecutionStarted()` / `setArgsComplete()`**：置标志 → `updateDisplay()` → `ui.requestRender()`。
**`updateResult(result, isPartial = false)`**：`this.result = result; this.isPartial = isPartial; this.updateDisplay();`（**不**调用 `requestRender`）。
**`setExpanded` / `setOutputPad` / `setShowImages`**：赋值 → `updateDisplay()`。
**`setImageWidthCells(width)`**：`Math.max(1, Math.floor(width))` → `updateDisplay()`。
**`invalidate()`**：`super.invalidate(); this.updateDisplay();`

**`render(width)`**：
```
if (this.argsPending) this.updateDisplay()
if (this.hideComponent) return []
if (hasRendererDefinition() && getRenderShell() === "self") {
  contentLines = selfRenderContainer.render(width)
  this.selfRenderHeight = contentLines.length
  if (contentLines.length === 0 && imageComponents.length === 0) return []
  lines = []
  if (contentLines.length > 0) { lines.push(""); lines.push(...contentLines) }
  for i in 0..imageComponents.length:
    if (imageSpacers[i]) lines.push(...imageSpacers[i].render(width))
    if (imageComponents[i]) lines.push(...imageComponents[i].render(width))
  return lines
}
return super.render(width)
```
> self shell 下**首行永远是空行**（`""`）。

**`handleMouse(event)`**：
```
if (!hasRendererDefinition() || getRenderShell() !== "self") return super.handleMouse(event)
if (event.y <= 0 || event.y > this.selfRenderHeight) return undefined
return selfRenderContainer.handleMouse({...event, y: event.y - 1, height: this.selfRenderHeight})
```
（`y-1` 抵消首行空行；`y` 为 1-based。）

**`updateDisplay()`（核心，逐条）**：
1. `this.argsPending = false`。
2. `bgFn` 按序：`isPartial` → `theme.bg("toolPendingBg")`；否则 `result?.isError` → `toolErrorBg`；否则 `toolSuccessBg`。
3. `hasContent = false; hideComponent = false`。
4. 有 renderer：
   - `renderContainer = self ? selfRenderContainer : contentBox`
   - 若 `renderContainer instanceof Box` → `setBgFn(bgFn)` + `setPaddingX(this.outputPad)`
   - `renderContainer.clear()`
   - 调用区：无 `renderCall` → `addChild(createResultRegion(createCallFallback()))`，`hasContent = true`；否则 `try { c = renderCall(args, theme, getRenderContext(callRendererComponent)); callRendererComponent = c; addChild(createResultRegion(c)); hasContent = true } catch { callRendererComponent = undefined; addChild(createResultRegion(createCallFallback())); hasContent = true }`
   - 结果区（仅 `this.result` 存在）：无 `renderResult` → `createResultFallback()`（可能为 undefined，则不加）；否则 `try { c = renderResult({content, details}, {expanded, isPartial}, theme, getRenderContext(resultRendererComponent)); ... } catch { ... fallback ... }`
     **注意**：传给 `renderResult` 的第一个参数是**新构造的对象** `{ content: this.result.content, details: this.result.details }`（不含 `isError`/`durationMs`）。
5. 无 renderer：`contentText.setCustomBgFn(bgFn)` + `setPaddingX(this.outputPad)` + `setText(this.formatToolExecution())`，`hasContent = true`。
6. 图像重建：保存 `previousImages` / `previousSources`；移除并清空 `imageComponents`、`imageSpacers`。
7. `if (this.result)`：`imageBlocks = result.content.filter(c => c.type === "image")`；`caps = getCapabilities()`；对每个块，**仅当** `caps.images && this.showImages && img.data && img.mimeType`：
   - `spacer = new Spacer(1)`；`addChild(spacer)`；push 到 `imageSpacers`
   - `source = { data, mimeType, widthCells: this.imageWidthCells }`；`index = imageComponents.length`
   - `previous = previousSources[index]`；若 `previous.data/mimeType/widthCells` 三者全等 → 复用 `previousImages[index]`，否则 `new Image(source.data, source.mimeType, { fallbackColor: s => theme.fg("toolOutput", s) }, { maxWidthCells: source.widthCells })`
   - 若 `source.mimeType !== "image/png"` → `ensurePngTranscoder(() => { this.invalidate(); this.ui.requestRender(); })`
   - push 到 `imageComponents` / `imageSources`；`addChild(imageComponent)`
8. `if (hasRendererDefinition() && !hasContent && imageComponents.length === 0) this.hideComponent = true`。

**`getTextOutput()`**：`getRenderedTextOutput(this.result, this.showImages)`（见下）。
**`formatToolExecution()`**：
```
text = theme.fg("toolTitle", theme.bold(this.toolName))
content = JSON.stringify(this.args, null, 2); if (content) text += `\n\n${content}`
output = getTextOutput(); if (output) text += `\n${output}`
```

**`getTextOutput(result, showImages)`（`core/tools/render-utils.ts`，逐字）**：
```
if (!result) return ""
textBlocks / imageBlocks 分别 filter
output = textBlocks.map(c => sanitizeBinaryOutput(stripAnsi(c.text || "")).replace(/\r/g,"")).join("\n")
if (imageBlocks.length > 0 && (!getCapabilities().images || !showImages))
  imageIndicators = imageBlocks.map(img => {
     mimeType = img.mimeType ?? "image/unknown"
     dims = (img.data && img.mimeType) ? (getImageDimensions(img.data, img.mimeType) ?? undefined) : undefined
     return imageFallback(mimeType, dims)
  }).join("\n")
  output = output ? `${output}\n${imageIndicators}` : imageIndicators
return output
```

**`ensurePngTranscoder(onRegistered)`（`utils/image-convert.ts`）**：
```
if (pngTranscoderRegistered || getCapabilities().images !== "kitty") return
pngTranscoderLoad ??= loadPngTranscoder().then(t => { if (!t) return false; setImageTranscoder(t); pngTranscoderRegistered = true; return true })
void pngTranscoderLoad.then(ok => { if (ok) onRegistered() })
```

**测试**（`tool-execution-component.test.ts`，约 26 条）：默认渲染、stacked renderers、self-render（首行空行 / 鼠标 y 偏移 / 图片附加）、bash partial update、read 行号区间、点击展开、参数增量每帧一次、`hideComponent` 空内容、图片复用、非 PNG 触发转码等。

### 2.52 `components/model-selector.ts`（423 行）

```ts
interface ModelItem { provider: string; id: string; model: Model<any>; }
interface ScopedModelItem { model: Model<any>; thinkingLevel?: string; }
interface DefaultModelReference { provider: string; id: string; }
type ModelScope = "all" | "scoped";

export class ModelSelectorComponent extends Container implements Focusable {
  constructor(tui: TUI, currentModel: Model<any> | undefined, modelRuntime: ModelRuntime,
              scopedModels: ReadonlyArray<ScopedModelItem>,
              onSelect: (model) => void, onCancel: () => void, initialSearchInput?: string,
              onSelectAsDefault?: (model) => void, defaultModel?: DefaultModelReference);
  dispose(): void;
  handleInput(keyData: string): void;
  getSearchInput(): Input;
  // Focusable：focused getter/setter 转发到 searchInput
}
```

**构造布局顺序（逐字）**：
1. `DynamicBorder()`
2. `Spacer(1)`
3. 若 `scopedModels.length > 0`：`scopeText = Text(getScopeText())`、`scopeHintText = Text(getScopeHintText())`；否则 `Text(theme.fg("warning", "Only showing models from configured providers. Use /login to add providers."))` **【逐字】**
4. `Spacer(1)`
5. `searchInput = new Input()`；有 `initialSearchInput` 则 `setValue`；`onSubmit = () => { if (filteredModels[selectedIndex]) handleSelect(...) }`
6. `Spacer(1)`
7. `listContainer = new Container()`
8. `Spacer(1)`
9. **仅当** `onSelectAsDefaultCallback` 存在：`Text(theme.fg("dim", `` `  ${keyDisplayText("tui.select.confirm")} to select · ${keyDisplayText("app.models.save")} to set as default · ${keyDisplayText("tui.select.cancel")} to cancel` ``))` **【逐字】**
10. `DynamicBorder()`
11. `loadModelsFromSnapshot()`；有初值则 `filterModels(initial)` 否则 `updateList()`；`tui.requestRender()`；`void refreshModels()`。

**初始 `scope`**：`scopedModels.length > 0 ? "scoped" : "all"`。

**`loadModelsFromSnapshot()`**：
```
models = modelRuntime.getAvailableSnapshot().map(m => ({provider:m.provider, id:m.id, model:m}))
allModels = sortModels(models)
scopedModels = scopedModels.map(s => { r = modelRuntime.getModel(s.model.provider, s.model.id); return r ? {...s, model:r} : s })
scopedModelItems = scopedModels.map(s => ({provider, id, model}))
activeModels = scope === "scoped" ? scopedModelItems : allModels
filteredModels = activeModels
currentIndex = filteredModels.findIndex(i => modelsAreEqual(currentModel, i.model))
selectedIndex = currentIndex >= 0 ? currentIndex : Math.min(selectedIndex, Math.max(0, filteredModels.length - 1))
```

**`refreshModels()`**：
```
timeoutMs = 15_000
timedOut = false
refreshTimeout = setTimeout(() => { timedOut = true; refreshAbortController.abort() }, 15000)
try {
  result = await refreshModelCatalogs(modelRuntime, refreshAbortController.signal)
  if (closed) return
  refreshStatusMessage = ""
  if (result.aborted && timedOut)        errorMessage = "Model refresh timed out; showing cached models."
  else if (result.errors.size === 1)     errorMessage = `Could not refresh ${result.errors.keys().next().value}; showing cached models.`
  else if (result.errors.size > 1)       errorMessage = `Could not refresh ${result.errors.size} model catalogs (${[...keys].join(", ")}); showing cached models.`
  else { errorMessage = modelRuntime.getError(); if (!errorMessage) { refreshStatusMessage = "Model catalogs refreshed."; refreshStatusSuccess = true } }
  loadModelsFromSnapshot(); filterModels(searchInput.getValue()); tui.requestRender()
} catch (error) {
  if (closed) return
  refreshStatusMessage = ""
  errorMessage = timedOut ? "Model refresh timed out; showing cached models."
                          : `Could not refresh model catalogs: ${error instanceof Error ? error.message : String(error)}`
  updateList(); tui.requestRender()
} finally { if (refreshTimeout) clearTimeout(refreshTimeout) }
```
**初始 `refreshStatusMessage = "Refreshing model catalogs…"`**（注意是 U+2026 单字符省略号，不是三个点）。

**`dispose()`**：幂等（`closed` 标志）；清定时器；`refreshAbortController.abort()`。

**`sortModels(models)`**：稳定排序比较器：current 优先 → default 优先（`defaultModel?.provider === model.provider && defaultModel.id === model.id`）→ `a.provider.localeCompare(b.provider)`。

**`getScopeText()`**：
```
allText    = scope === "all"    ? accent("all")    : muted("all")
scopedText = scope === "scoped" ? accent("scoped") : muted("scoped")
`${muted("Scope: ")}${allText}${muted(" | ")}${scopedText}`
```
**`getScopeHintText()`**：`keyHint("tui.input.tab", "scope") + theme.fg("muted", " (all/scoped)")`。

**`isDefaultSearch(query)`**：`n = query.trim().toLowerCase(); n.length > 0 && "default".startsWith(n)`。

**`setScope(scope)`**：同值直接返回；改 `scope`、`activeModels`；`currentIndex` 命中则用它否则 0；`filterModels(searchInput.getValue())`；刷新 `scopeText`。

**`filterModels(query)`**：
```
if (query) {
  filtered = fuzzyFilter(activeModels, query, item => `${getModelSelectorSearchText({id,provider,name})}${isDefaultModel(item.model) ? " default" : ""}`)
  if (isDefaultSearch(query)) {
    defaultItems = activeModels.filter(isDefaultModel)
    defaultKeys = Set(`${provider}\0${id}`)
    filteredModels = [...defaultItems, ...filtered.filter(i => !defaultKeys.has(`${i.provider}\0${i.id}`))]
  } else filteredModels = filtered
} else filteredModels = activeModels
selectedIndex = query ? 0 : Math.min(selectedIndex, Math.max(0, filteredModels.length - 1))
updateList()
```
> 分隔符是 **NUL**（`\0`），Rust 用 `(String, String)` 元组或 `format!("{provider}\u{0}{id}")` 等价实现。

**`updateList()`**：
```
listContainer.clear()
maxVisible = 10
startIndex = Math.max(0, Math.min(selectedIndex - 5, filteredModels.length - maxVisible))
endIndex   = Math.min(startIndex + maxVisible, filteredModels.length)
for i in startIndex..endIndex:
  isSelected = i === selectedIndex; isCurrent = modelsAreEqual(currentModel, item.model); isDefault = ...
  defaultBadge  = isDefault ? muted(" · default") : ""
  cursor        = isSelected ? accent("→ ") : "  "
  currentMarker = isCurrent  ? accent("✓ ") : "  "
  modelText     = isSelected ? accent(item.id) : item.id
  name          = getDistinctModelName({id, name: item.model.name})
  nameText      = name ? muted(` (${name})`) : ""
  providerBadge = muted(`[${item.provider}]`)
  line = `${cursor}${currentMarker}${modelText}${nameText} ${providerBadge}${defaultBadge}`
  addChild(Text(line, 0, 0))
if (startIndex > 0 || endIndex < filteredModels.length)
  addChild(Text(muted(`  (${selectedIndex+1}/${filteredModels.length})`), 0, 0))
if (errorMessage)                → 按 "\n" 拆分，每行 Text(fg("error", line), 0, 0)
else if (filteredModels.length === 0) → Text(muted("  No matching models"), 0, 0)
else { Spacer(1); Text(muted(`  Model Name: ${selected.model.name}`), 0, 0) }
if (refreshStatusMessage) { Spacer(1); Text(fg(refreshStatusSuccess ? "success" : "muted", `  ${refreshStatusMessage}`), 0, 0) }
```
> 注意 `modelText` / `nameText` 之间**无空格**，`nameText` 与 `providerBadge` 之间有**一个空格**，`providerBadge` 与 `defaultBadge` **无空格**（defaultBadge 自带前置空格）。

**`handleInput(keyData)`（顺序即优先级）**：
1. `tui.input.tab`：若 `scopedModelItems.length > 0` → 切 `scope`（all↔scoped）；刷新 `scopeHintText`；**始终 return**。
2. `tui.select.up`：空列表 return；`selectedIndex === 0 ? len-1 : idx-1`（**环绕**）；`updateList()`。
3. `tui.select.down`：同上环绕。
4. `tui.select.confirm`：`handleSelect(filteredModels[selectedIndex].model)`。
5. `tui.select.cancel`：`dispose(); onCancelCallback()`。
6. `app.models.save` 且存在 `onSelectAsDefaultCallback`：`dispose(); onSelectAsDefaultCallback(model)`。
7. 其他：`searchInput.handleInput(keyData)` + `filterModels(searchInput.getValue())`。

**`handleSelect(model)`**：`dispose()` 后 `onSelectCallback(model)`。

**`refreshModelCatalogs(modelRuntime, signal)`（`model-catalog-refresh.ts`，逐字）**：
```
coordinator 按 runtime 缓存 active = {controller, promise, waiters}
refresh(runtime, signal):
  signal.throwIfAborted()
  active = map.get(runtime)
  if (!active) {
    controller = new AbortController()
    operation = runtime.refresh({signal: controller.signal})
    promise = raceWithAbortSignal(operation, controller.signal)
                .finally(() => { if (map.get(runtime) === created) map.delete(runtime) })
    created = {controller, promise, waiters: 0}; active = created; map.set(runtime, active)
  }
  active.waiters++
  return raceWithAbortSignal(active.promise, signal)
           .finally(() => { active.waiters--; if (active.waiters === 0 && map.get(runtime) === active) active.controller.abort() })
```
> 语义：多个 `/model` 选择器并发打开时共享同一次刷新，但各自的取消互不影响；最后一个 waiter 离开时才真正 abort 底层请求。

**测试**（`model-selector.test.ts`，4 条）。

### 2.53 `components/scoped-models-selector.ts`（403 行）

```ts
type EnabledIds = string[] | null;      // null = 全启用（无过滤），string[] = 显式有序列表

export interface ModelsConfig {
  allModels: Model<any>[];
  enabledModelIds: string[] | null;
  refreshStatus?: string;
}
export interface ModelsCallbacks {
  onChange: (enabledModelIds: string[] | null) => void | Promise<void>;   // 会话内变更
  onPersist: (enabledModelIds: string[] | null) => void | Promise<void>;  // 持久化到 settings
  onCancel: () => void;
}
export class ScopedModelsSelectorComponent extends Container implements Focusable {
  constructor(config: ModelsConfig, callbacks: ModelsCallbacks);
  updateModels(models: readonly Model<any>[], enabledModelIds?: string[] | null): void;
  setRefreshStatus(message: string, kind: "muted" | "success" | "warning"): void;
  handleInput(data: string): void;
  getSearchInput(): Input;
}
```

**纯函数（必须逐字移植，含边界）**：
```
isEnabled(ids, id)              = ids === null || ids.includes(id)
normalizeEnabled(result, allIds)= (result.length === allIds.length && result.every(id => allIds.includes(id))) ? null : result
toggle(ids, allIds, id):
  ids === null → allIds.filter(m => m !== id)
  index >= 0   → 删除该元素
  否则          → normalizeEnabled([...ids, id], allIds)
enableAll(ids, allIds, targetIds?):
  ids === null → null
  targets = targetIds ?? allIds
  result = [...ids]；对 targets 中不在 result 的依次 push
  return normalizeEnabled(result, allIds)
clearAll(ids, allIds, targetIds?):
  ids === null → (targetIds ? allIds.filter(id => !targetIds.includes(id)) : [])
  targets = new Set(targetIds ?? ids)；return ids.filter(id => !targets.has(id))
move(ids, id, delta):
  ids === null → null
  index < 0 → 返回副本
  newIndex 越界 → 返回副本
  否则交换 index 与 newIndex
getSortedIds(ids, allIds):
  ids === null → allIds
  否则 [...ids, ...allIds.filter(id => !enabledSet.has(id))]
```

**构造布局**：`DynamicBorder()` → `Spacer(1)` → `Text(theme.fg("accent", theme.bold("Model Configuration")), 0, 0)` → `Text(theme.fg("muted", `` `Session-only. ${keyDisplayText("app.models.save")} to save to settings.` ``), 0, 0)` **【逐字】** → `Spacer(1)` → `searchInput` → `Spacer(1)` → `listContainer` → `Spacer(1)` → [若 `config.refreshStatus`：`refreshStatusText = Text(theme.fg("muted", `  ${config.refreshStatus}`))`] → `footerText` → `DynamicBorder()` → `updateList()`。

**`maxVisible = 8`**。

**`getFooterText()`**：
```
enabledCount     = enabledIds?.filter(id => modelsById.has(id)).length ?? allIds.length
unavailableCount = enabledIds?.filter(id => !modelsById.has(id)).length ?? 0
allEnabled       = enabledIds === null
countText = allEnabled ? "all enabled"
          : `${enabledCount}/${allIds.length} enabled${unavailableCount ? ` · ${unavailableCount} unavailable` : ""}`
parts = [ `${keyDisplayText("tui.select.confirm")} toggle`,
          `${keyDisplayText("app.models.enableAll")} all`,
          `${keyDisplayText("app.models.clearAll")} clear`,
          `${keyDisplayText("app.models.toggleProvider")} provider`,
          `${keyDisplayText("app.models.reorderUp")}/${keyDisplayText("app.models.reorderDown")} reorder`,
          `${keyDisplayText("app.models.save")} save`,
          countText ]
isDirty ? theme.fg("dim", `  ${parts.join(" · ")} `) + theme.fg("warning", "(unsaved)")
        : theme.fg("dim", `  ${parts.join(" · ")}`)
```
> 脏态时 **`·` 后有一个尾随空格**（`` `  ${...} ` ``），再拼 `(unsaved)`。

**`refresh()`**：
```
query = searchInput.getValue()
items = buildItems()
filteredItems = query ? fuzzyFilter(items, query, item => item.model
                    ? getModelSearchText({id: item.model.id, provider: item.model.provider, name: item.model.name})
                    : item.fullId) : items
selectedIndex = Math.min(selectedIndex, Math.max(0, filteredItems.length - 1))
updateList(); footerText.setText(getFooterText())
```

**`notifyChange()`**：`callbacks.onChange(enabledIds === null ? null : [...enabledIds])`。

**`updateList()`**：
```
clear()
空 → Text(muted("  No matching models"), 0, 0) 并 return
startIndex = Math.max(0, Math.min(selectedIndex - 4, len - 8)); endIndex = Math.min(startIndex + 8, len)
每行：
  prefix = isSelected ? accent("→ ") : "  "
  id = item.model?.id ?? item.fullId
  styledId = item.model ? id : theme.strikethrough(id)
  modelText = isSelected ? accent(styledId) : styledId
  name = item.model ? getDistinctModelName(item.model) : undefined
  nameText = name ? muted(` (${name})`) : ""
  providerBadge = muted(item.model ? ` [${item.model.provider}]` : " [unavailable]")
  status = (item.model && item.enabled) ? accent("✓ ") : "  "
  Text(`${prefix}${status}${modelText}${nameText}${providerBadge}`, 0, 0)
滚动指示：Text(muted(`  (${selectedIndex+1}/${len})`), 0, 0)
非空：Spacer(1); Text(muted(`  ${selected.model ? `Model Name: ${selected.model.name}` : "Model unavailable"}`), 0, 0)
```
> `[unavailable]` 的 providerBadge **自带前导空格**，而行内 `nameText` 之后没有额外空格——与 §2.52 的 model-selector 略有差异，勿混用。

**`handleInput(data)`（顺序即优先级，每分支都 return）**：
1. `tui.select.up` / `tui.select.down`：空列表 return；环绕移动；`updateList()`。
2. `app.models.reorderUp` / `app.models.reorderDown`：`enabledIds === null` → return；取当前项，若已启用：`delta = up ? -1 : 1`；`newIndex = currentIndex + delta`；仅当 `0 <= newIndex < enabledIds.length` 才 `enabledIds = move(...)`、`isDirty = true`、`selectedIndex += delta`、`refresh()`、`notifyChange()`。
3. `tui.select.confirm`：`toggle` → `isDirty = true` → `refresh()` → `notifyChange()`。
4. `app.models.enableAll`：`targetIds = searchInput.getValue() ? filteredItems.map(i => i.fullId) : undefined` → `enableAll` → 同上。
5. `app.models.clearAll`：同 4 用 `clearAll`。
6. `app.models.toggleProvider`：取当前项 `item?.model`；`provider = item.model.provider`；`providerIds = allIds.filter(id => modelsById.get(id)!.provider === provider)`；`allEnabled = providerIds.every(isEnabled)`；`allEnabled ? clearAll(providerIds) : enableAll(providerIds)` → 同上。
7. `app.models.save`：`callbacks.onPersist(...)`；`isDirty = false`；刷新 footer。
8. `Key.ctrl("c")`（**用 `matchesKey`，不是 keybinding id**）：有搜索词 → 清空并 `refresh()`；否则 `onCancel()`。
9. `Key.escape`：`onCancel()`。
10. 其他：`searchInput.handleInput(data)` + `refresh()`。

**`updateModels(models, enabledModelIds?)`**：先记 `selectedId`；若传了 `enabledModelIds`（**含 `null`**，用 `!== undefined` 区分）则重设；重建 `modelsById`/`allIds`；`refresh()`；若原选中 id 仍在 → 恢复 `selectedIndex` 并 `updateList()`。

**`setRefreshStatus(message, kind)`**：`refreshStatusText?.setText(theme.fg(kind, `  ${message}`))`。

**测试**（`scoped-models-selector.test.ts`，5 条）。

### 2.54 `components/settings-selector.ts`（1009 行）

```ts
const MODEL_PICKER_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 46 };

const THINKING_DESCRIPTIONS: Record<ThinkingLevel, string> = {
  off: "No reasoning",
  minimal: "Very brief reasoning (~1k tokens)",
  low: "Light reasoning (~2k tokens)",
  medium: "Moderate reasoning (~8k tokens)",
  high: "Deep reasoning (~16k tokens)",
  xhigh: "Extra-high reasoning (~32k tokens)",
  max: "Maximum reasoning",
};

const DEFAULT_PROJECT_TRUST_LABELS: Record<DefaultProjectTrust, string> = {
  ask: "Ask", always: "Always trust", never: "Never trust",
};
const DEFAULT_PROJECT_TRUST_BY_LABEL = new Map(Object.entries(DEFAULT_PROJECT_TRUST_LABELS).map(([v,l]) => [l, v]));

const CLEAR_OVERRIDE_VALUE = "__clear__";
const AUTOMATIC_THEME_VALUE = "/";
```
> `THINKING_DESCRIPTIONS` 与 §2.40 `thinking-selector.ts` 的 `LEVEL_DESCRIPTIONS` **内容完全相同**（两处独立定义，移植时可共享一个常量，但字符串必须逐字一致）。

**`SettingsConfig` 字段（逐字，顺序即声明顺序）**：
```
autoCompact: boolean; defaultModel: string; currentModel?: Model; availableDefaultModels: readonly Model[];
showImages: boolean; imageWidthCells: number; autoResizeImages: boolean; blockImages: boolean;
enableSkillCommands: boolean; steeringMode: "all"|"one-at-a-time"; followUpMode: "all"|"one-at-a-time";
transport: Transport; sessionShareDestination: SessionShareDestination; httpIdleTimeoutMs: number;
cacheWarmingMode: CacheWarmingMode; thinkingLevel: ThinkingLevel; availableThinkingLevels: ThinkingLevel[];
modelThinkingLevels: Record<string, ThinkingLevel>; currentTheme: string; terminalTheme: TerminalTheme;
availableThemes: string[]; hideThinkingBlock: boolean; mermaidRenderingMode: MermaidRenderingMode;
showCacheMissNotices: boolean; collapseChangelog: boolean; enableInstallTelemetry: boolean;
doubleEscapeAction: "fork"|"tree"|"none";
treeFilterMode: "default"|"no-tools"|"user-only"|"labeled-only"|"all";
showHardwareCursor: boolean; editorPaddingX: number; outputPad: 0|1; autocompleteMaxVisible: number;
quietStartup: QuietStartup; defaultProjectTrust: DefaultProjectTrust; clearOnShrink: boolean;
showTerminalProgress: boolean; tuiMode: TuiMode; fullscreenExitOutput: FullscreenExitOutput;
fullscreenScrollbar: ScrollViewScrollbar; fullscreenCopyOnSelect: boolean;
fullscreenWheelScrollLines: WheelScrollLines; warnings: WarningSettings;
```
**`SettingsCallbacks` 字段**：`onAutoCompactChange`、`onShowImagesChange`、`onImageWidthCellsChange`、`onAutoResizeImagesChange`、`onBlockImagesChange`、`onEnableSkillCommandsChange`、`onSteeringModeChange`、`onFollowUpModeChange`、`onTransportChange`、`onSessionShareDestinationChange`、`onHttpIdleTimeoutMsChange`、`onCacheWarmingModeChange`、`onModelThinkingLevelChange(provider, modelId, level)`、`onModelThinkingLevelRemove(provider, modelId)`、`onThemeChange`、`onThemePreview?`、`onHideThinkingBlockChange`、`onMermaidRenderingModeChange`、`onShowCacheMissNoticesChange`、`onCollapseChangelogChange`、`onEnableInstallTelemetryChange`、`onDoubleEscapeActionChange`、`onTreeFilterModeChange`、`onShowHardwareCursorChange`、`onEditorPaddingXChange`、`onOutputPadChange`、`onAutocompleteMaxVisibleChange`、`onQuietStartupChange`、`onDefaultProjectTrustChange`、`onClearOnShrinkChange`、`onShowTerminalProgressChange`、`onTuiModeChange`、`onFullscreenExitOutputChange`、`onFullscreenScrollbarChange`、`onFullscreenCopyOnSelectChange`、`onFullscreenWheelScrollLinesChange`、`onWarningsChange`、`onCancel`。

**辅助函数**：
```
modelSettingKey(model)        = `${model.provider}/${model.id}`
modelDisplayLabel(model)      = `${model.id} [${model.provider}]`
modelThinkingOverridesSummary(overrides) = Object.keys(overrides).length === 0 ? "none" : `${count} configured`
modelItemLabel(model)         = `${model.id} ${theme.fg("muted", `[${model.provider}]`)}`
themeItems(availableThemes, currentTheme) = availableThemes.map(name => ({
    value: name,
    label: `${name === currentTheme ? "✓ " : "  "}${name}`,
    ...(name === SYSTEM_THEME_NAME ? { description: "Theme created from your terminal's colors" } : {}) }))
singleModeThemeItems(themes, current): system 项提到最前 → { value: "/", label: "  automatic",
    description: "Use separate themes for light and dark terminal appearance" } → 其余项
preferredTheme(themes, preferred, fallback): preferred ∈ themes → preferred; fallback ∈ themes → fallback; 否则 themes[0] ?? fallback
defaultAutomaticThemes(setting, themes): parseAutoThemeSetting(setting) ?? {
    lightTheme/darkTheme = preferredTheme(themes, setting.includes("/") ? undefined : setting, SYSTEM_THEME_NAME) }
```

**`WarningSettingsSubmenu`（私有类）**：
- 单项：`{ id: "anthropic-extra-usage", label: "Anthropic extra usage",
  description: "Warn when Anthropic subscription auth may use paid extra usage",
  currentValue: (state.anthropicExtraUsage ?? SETTINGS_DEFAULTS.warnings.anthropicExtraUsage) ? "true" : "false",
  values: ["true", "false"] }`
- `new SettingsList(items, Math.min(items.length, 10), getSettingsListTheme(), (id, v) => …, onCancel)`
- 变更：`state = {...state, anthropicExtraUsage: v === "true"}; onChange({...state})`

**`ThemeSubmenu`（私有类）**：
状态：`mode: "single"|"automatic"`、`singleTheme`、`lightTheme`、`darkTheme`、`originalThemeSetting`。
构造：
```
autoTheme = parseAutoThemeSetting(currentThemeSetting)
automaticThemes = defaultAutomaticThemes(currentThemeSetting, availableThemes)
fixedTheme = (autoTheme || currentThemeSetting.includes("/")) ? undefined : currentThemeSetting
mode = autoTheme ? "automatic" : "single"
lightTheme/darkTheme = automaticThemes.*
singleTheme = preferredTheme(availableThemes, fixedTheme ?? (autoTheme ? getActiveAutomaticTheme() : undefined), SYSTEM_THEME_NAME)
mode === "automatic" ? showAutomaticMenu() : showSingleMenu()
```
`getActiveAutomaticTheme()`：`terminalTheme === "light" ? lightTheme : darkTheme`。
`getAutomaticThemeSetting()`：`` `${lightTheme}/${darkTheme}` ``。
`getThemeSetting()`：automatic 模式返回上者，否则 `singleTheme`。
`apply(setting)` → `onDone(setting)`；`cancel()` → `onThemePreview?.(originalThemeSetting)` 后 `onDone()`（**无参**，表示未改）。

`showSingleMenu()`：`new SelectSubmenu("Theme", "Select a theme, or choose automatic to follow terminal appearance.", singleModeThemeItems(...), singleTheme, onSelect, () => this.cancel(), onSelectionChange)`
- onSelect：`value === "/"` → `mode = "automatic"`；`onThemePreview?.(getThemeSetting())`；`showAutomaticMenu()`；否则 `singleTheme = value; apply(value)`。
- onSelectionChange：`onThemePreview?.(value === "/" ? getAutomaticThemeSetting() : value)`。

`showAutomaticMenu()`：`Container` 内容：
```
Text(theme.bold(theme.fg("accent", "Automatic Theme")), 0, 0)
Spacer(1)
Text(theme.fg("muted", "Choose themes for terminal light and dark appearance."), 0, 0)
Text(theme.fg("muted", "Light/dark detection requires terminal support."), 0, 0)
Spacer(1)
SettingsList(items, Math.min(items.length, 10), getSettingsListTheme(), onChange, () => this.cancel())
```
items（逐字）：
| id | label | description | currentValue | 说明 |
|---|---|---|---|---|
| `light-theme` | `Light theme` | `Theme to use in automatic mode when the terminal is light` | `lightTheme` | submenu `createThemeSelect("Light Theme", "Select the theme to use for light terminal appearance", …)`，选中后 `lightTheme = v; onThemePreview?.(getThemeSetting()); done(v)` |
| `dark-theme` | `Dark theme` | `Theme to use in automatic mode when the terminal is dark` | `darkTheme` | submenu `createThemeSelect("Dark Theme", "Select the theme to use for dark terminal appearance", …)` |
| `apply` | `Apply` | `Save and go back` | `"save and go back"` | `values: ["save and go back"]` |
| `single-mode` | `Change mode` | `Switch to one theme for light and dark` | `"switch to single theme"` | `values: ["switch to single theme"]` |

onChange：`single-mode` → `mode = "single"; singleTheme = getActiveAutomaticTheme(); onThemePreview?.(singleTheme); showSingleMenu()`；`apply` → `apply(getAutomaticThemeSetting())`。

`createThemeSelect(title, description, currentValue, done, onSelect)` → `new SelectSubmenu(title, description, themeItems(availableThemes, currentValue), currentValue, onSelect, () => { onThemePreview?.(getThemeSetting()); done() }, v => onThemePreview?.(v))`。

**主组件 `SettingsSelectorComponent`**：
```ts
export class SettingsSelectorComponent extends Container {
  constructor(config: SettingsConfig, callbacks: SettingsCallbacks);
  getSettingsList(): SettingsList;
}
```
构造前计算的局部量：
```
supportsImages = getCapabilities().images
followUpKey = keyDisplayText("app.message.followUp")
cycleThinkingKey = keyDisplayText("app.thinking.cycle")
currentWarnings = {...config.warnings}
currentModelThinkingLevels = {...config.modelThinkingLevels}
defaultModelByValue = Map(availableDefaultModels.map(m => [modelSettingKey(m), m]))
currentDefaultModelKey = defaultModelByValue.has(config.defaultModel) ? config.defaultModel : undefined
currentModelKey = config.currentModel ? modelSettingKey(config.currentModel) : undefined
```

**主 items 顺序（逐字；`splice` 会插在中间，见后）**：
| # | id | label | description（逐字） | currentValue | values |
|---|---|---|---|---|---|
| 0 | `autocompact` | `Auto-compact` | `Automatically compact context when it gets too large` | `"true"/"false"` | `["true","false"]` |
| 1 | `steering-mode` | `Steering mode` | `Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.` | `config.steeringMode` | `["one-at-a-time","all"]` |
| 2 | `follow-up-mode` | `Follow-up mode` | `` `${followUpKey} queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.` `` | `config.followUpMode` | `["one-at-a-time","all"]` |
| 3 | `transport` | `Transport` | `Preferred transport for providers that support multiple transports` | `config.transport` | `["sse","websocket","websocket-cached","auto"]` |
| 4 | `share-destination` | `Session share destination` | `Where to upload sessions with /share (auto prefers Radius when authenticated, otherwise GitHub Gist)` | `config.sessionShareDestination` | `["auto","gist","radius"]` |
| 5 | `http-idle-timeout` | `HTTP idle timeout` | `Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five minutes.` | `formatHttpIdleTimeoutMs(config.httpIdleTimeoutMs)` | `HTTP_IDLE_TIMEOUT_CHOICES.map(c => c.label)` |
| 6 | `cache-warming-mode` | `Cache warming` | `off; streaming while the agent runs; idle also between runs while continuation stays profitable` | `config.cacheWarmingMode` | `[...CACHE_WARMING_MODES]` |
| 7 | `hide-thinking` | `Hide thinking` | `Hide thinking blocks in assistant responses` | `"true"/"false"` | `["true","false"]` |
| 8 | `mermaid-rendering` | `Mermaid diagrams` | `Render Mermaid code blocks as Unicode diagrams` | `config.mermaidRenderingMode` | `["off","final","streaming"]` |
| 9 | `cache-miss-notices` | `Cache miss notices` | `Show transcript notices for cache costs and provider recovery diagnostics` | `"true"/"false"` | `["true","false"]` |
| 10 | `collapse-changelog` | `Collapse changelog` | `Show condensed changelog after updates` | `"true"/"false"` | `["true","false"]` |
| 11 | `quiet-startup` | `Quiet startup` | `Disable verbose printing at startup (header: keep only the startup header)` | `String(config.quietStartup)` | `["true","header","false"]` |
| 12 | `install-telemetry` | `Install telemetry` | `Send an anonymous version/update ping after changelog-detected updates` | `"true"/"false"` | `["true","false"]` |
| 13 | `default-project-trust` | `Default project trust` | `Fallback behavior when no extension or saved trust decision decides project trust` | `DEFAULT_PROJECT_TRUST_LABELS[config.defaultProjectTrust]` | `Object.values(DEFAULT_PROJECT_TRUST_LABELS)` = `["Ask","Always trust","Never trust"]` |
| 14 | `double-escape-action` | `Double-escape action` | `Action when pressing Escape twice with empty editor` | `config.doubleEscapeAction` | `["tree","fork","none"]` |
| 15 | `tree-filter-mode` | `Tree filter mode` | `Default filter when opening /tree` | `config.treeFilterMode` | `["default","no-tools","user-only","labeled-only","all"]` |
| 16 | `warnings` | `Warnings` | `Enable or disable individual warnings` | `"configure"` | submenu → `WarningSettingsSubmenu` |
| 17 | `model-thinking` | `Default thinking level per model` | `` `Override the default thinking level for specific models. ${cycleThinkingKey} cycles in-session.` `` | `modelThinkingOverridesSummary(...)` | submenu（见下） |
| 18 | `tui-mode` | `TUI mode` | `Interface layout; regular mode uses the terminal's normal scrollback` | `config.tuiMode` | `["regular","fullscreen"]` |
| 19 | `fullscreen-exit-output` | `Fullscreen exit output` | `Print the transcript or only a session resume hint when exiting fullscreen mode` | `config.fullscreenExitOutput` | `["transcript","resume-hint"]` |
| 20 | `fullscreen-scrollbar` | `Fullscreen scrollbar` | `Scrollbar behavior in fullscreen mode; has no effect in regular mode` | `config.fullscreenScrollbar` | `["auto","always","hidden"]` |
| 21 | `fullscreen-copy-on-select` | `Fullscreen copy on select` | `Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X` | `"true"/"false"` | `["true","false"]` |
| 22 | `fullscreen-wheel-scroll-lines` | `Fullscreen wheel scrolling` | `Lines per mouse-wheel event in fullscreen mode; 'auto' speeds up fast wheel spins where the terminal does not` | `String(config.fullscreenWheelScrollLines)` | `["auto", ...[...new Set([1,2,3,5,10, config.fullscreenWheelScrollLines])].filter(l => l !== "auto").sort((a,b)=>a-b).map(String)]` |
| 23 | `theme` | `Theme` | `Color theme for the interface` | `config.currentTheme` | submenu → `new ThemeSubmenu(currentValue, config.terminalTheme, config.availableThemes, callbacks, done)` |

**`model-thinking` 子菜单（`SteppedSubmenu`，`{loop: true}`）**：
step 1 `key: "model"`：
- `title: "Per-Model Thinking Level"`；`description: "Select a model to configure"`
- `options()`：对 `availableDefaultModels` 排序：`currentModelKey` 最前 → `currentDefaultModelKey` 次之 → `a.provider.localeCompare(b.provider)`；每项 `{ value: modelSettingKey(m), label: modelItemLabel(m), description: currentModelThinkingLevels[key] ?? undefined }`
- 空表则 push `{ value: "__none__", label: "No models available", description: "Log in to a provider or configure an API key first" }`
- `preselect: () => currentModelKey ?? currentDefaultModelKey`；`searchable: true`；`layout: MODEL_PICKER_LAYOUT`
step 2 `key: "level"`：
- `title(ctx) = `Thinking Level for ${m ? modelDisplayLabel(m) : ctx.model}`（m = defaultModelByValue.get(ctx.model)）`
- `description: "Select default thinking level for this model"`
- `options(ctx)`：`model` 不存在 → `[]`；否则 `levels = model.reasoning ? getSupportedThinkingLevels(model) : ["off"]`；每项 `{ value: level, label: `${level === activeLevel ? "✓ " : "  "}${level}`, description: THINKING_DESCRIPTIONS[level] }`；若该模型已有覆盖则追加 `{ value: "__clear__", label: "  (clear override)", description: `Revert to global default (${config.thinkingLevel})` }`
- `preselect: ctx => currentModelThinkingLevels[ctx.model]`
完成回调：`model = defaultModelByValue.get(selections.model); if (!model) return;`；`selections.level === "__clear__"` → `onModelThinkingLevelRemove(provider, id)` + `delete currentModelThinkingLevels[selections.model]`；否则 `onModelThinkingLevelChange(provider, id, level)` + 同步本地副本。
取消回调：`done(modelThinkingOverridesSummary(currentModelThinkingLevels))`。

**splice 插入逻辑（决定最终顺序，必须照抄次序）**：
1. 若 `supportsImages`：在 index **1** 插入 `show-images`（`Show images` / `Render images inline in terminal` / values `["true","false"]`），再在 index **2** 插入 `image-width-cells`（`Image width` / `Preferred inline image width in terminal cells` / values `["60","80","120"]`）。
2. `auto-resize-images`（`Auto-resize images` / `Resize large images to 2000x2000 max for better model compatibility` / `["true","false"]`）插在 `supportsImages ? 3 : 1`。
3. `block-images`（`Block images` / `Prevent images from being sent to LLM providers`）插在 `auto-resize-images` 之后。
4. `skill-commands`（`Skill commands` / `Register skills as /skill:name commands`）插在 `block-images` 之后。
5. `show-hardware-cursor`（`Show hardware cursor` / `Use the terminal cursor instead of Kiana's drawn cursor`）插在 `skill-commands` 之后。
6. `editor-padding`（`Editor padding` / `Horizontal padding for input editor (0-3)` / `["0","1","2","3"]`，currentValue `String(config.editorPaddingX)`）插在 `show-hardware-cursor` 之后。
7. `output-padding`（`Output padding` / `Horizontal padding for messages, tool output, and command output` / `["0","1"]`）插在 `editor-padding` 之后。
8. `autocomplete-max-visible`（`Autocomplete max items` / `Max visible items in autocomplete dropdown (3-20)` / `["3","5","7","10","15","20"]`）插在 `output-padding` 之后。
9. `clear-on-shrink`（`Clear on shrink` / `Clear empty rows when content shrinks (may cause flicker)`）插在 `autocomplete-max-visible` 之后。
10. `terminal-progress`（`Terminal progress` / `Show OSC 9;4 progress indicators in the terminal tab bar`）插在 `clear-on-shrink` 之后。

最终顺序：`autocompact` → [show-images → image-width-cells] → auto-resize-images → block-images → skill-commands → show-hardware-cursor → editor-padding → output-padding → autocomplete-max-visible → clear-on-shrink → terminal-progress → steering-mode → follow-up-mode → transport → … → theme。

**组装与回调**：`addChild(new DynamicBorder())` → `settingsList = new SettingsList(items, 10, getSettingsListTheme(), onChange, callbacks.onCancel, { enableSearch: true })` → `addChild(settingsList)` → `addChild(new DynamicBorder())`。
> 注意：主列表的 page size 是**硬编码 10**（子菜单用 `Math.min(items.length, 10)`）。

**onChange 分派（逐字映射）**：
```
autocompact            → onAutoCompactChange(v === "true")
show-images            → onShowImagesChange(v === "true")
image-width-cells      → onImageWidthCellsChange(parseInt(v, 10))
auto-resize-images     → onAutoResizeImagesChange(v === "true")
block-images           → onBlockImagesChange(v === "true")
skill-commands         → onEnableSkillCommandsChange(v === "true")
steering-mode          → onSteeringModeChange(v)
follow-up-mode         → onFollowUpModeChange(v)
transport              → onTransportChange(v)
share-destination      → onSessionShareDestinationChange(v)
http-idle-timeout      → choice = HTTP_IDLE_TIMEOUT_CHOICES.find(c => c.label === v); if (choice) onHttpIdleTimeoutMsChange(choice.timeoutMs)
cache-warming-mode     → onCacheWarmingModeChange(v)
hide-thinking          → onHideThinkingBlockChange(v === "true")
mermaid-rendering      → onMermaidRenderingModeChange(v)
cache-miss-notices     → onShowCacheMissNoticesChange(v === "true")
collapse-changelog     → onCollapseChangelogChange(v === "true")
quiet-startup          → onQuietStartupChange(v === "header" ? "header" : v === "true")
install-telemetry      → onEnableInstallTelemetryChange(v === "true")
default-project-trust  → t = DEFAULT_PROJECT_TRUST_BY_LABEL.get(v); if (t) onDefaultProjectTrustChange(t)
double-escape-action   → onDoubleEscapeActionChange(v as "fork"|"tree")
tree-filter-mode       → onTreeFilterModeChange(v)
show-hardware-cursor   → onShowHardwareCursorChange(v === "true")
editor-padding         → onEditorPaddingXChange(parseInt(v, 10))
output-padding         → onOutputPadChange(v === "0" ? 0 : 1)
autocomplete-max-visible → onAutocompleteMaxVisibleChange(parseInt(v, 10))
clear-on-shrink        → onClearOnShrinkChange(v === "true")
terminal-progress      → onShowTerminalProgressChange(v === "true")
tui-mode               → onTuiModeChange(v)
fullscreen-exit-output → onFullscreenExitOutputChange(v)
fullscreen-scrollbar   → onFullscreenScrollbarChange(v)
fullscreen-copy-on-select → onFullscreenCopyOnSelectChange(v === "true")
fullscreen-wheel-scroll-lines → onFullscreenWheelScrollLinesChange(v === "auto" ? "auto" : parseInt(v, 10))
theme                  → onThemeChange(v)
```
> 移植陷阱：`double-escape-action` 的 TS 断言写成 `as "fork" | "tree"`（缺少 `"none"`），但运行时值仍可能是 `"none"`——Rust 侧应接受三个值。

**测试**（`settings-selector.test.ts`，5 条）。

### 2.55 `components/settings-submenu.ts`（258 行）

```ts
const SUBMENU_SELECT_LIST_LAYOUT: SelectListLayoutOptions = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 };

export interface SelectSubmenuOptions { searchable?: boolean; layout?: SelectListLayoutOptions; }

export class SelectSubmenu extends Container {
  constructor(title: string, description: string, options: SelectItem[], currentValue: string,
              onSelect: (value: string) => void, onCancel: () => void,
              onSelectionChange?: (value: string) => void, submenuOptions?: SelectSubmenuOptions);
  handleInput(data: string): void;
}

export interface SteppedSubmenuStep {
  key: string;
  title: string | ((context: Record<string,string>) => string);
  description: string | ((context: Record<string,string>) => string);
  options: (context: Record<string,string>) => SelectItem[];
  preselect?: (context: Record<string,string>) => string | undefined;
  searchable?: boolean;
  layout?: SelectListLayoutOptions;
}
interface SteppedSubmenuOptions { startAtStep?: number; initialContext?: Record<string,string>; loop?: boolean; }

export class SteppedSubmenu extends Container {
  constructor(steps: SteppedSubmenuStep[], onComplete: (context: Record<string,string>) => void,
              onCancel: () => void, opts?: SteppedSubmenuOptions);
  render(width: number): string[];
  handleInput(data: string): void;
  invalidate(): void;
}
```

**`SelectSubmenu` 布局顺序**：
1. `Text(theme.bold(theme.fg("accent", title)), 0, 0)`（注意**先 bold 再 fg**）
2. 若有 `description`：`Spacer(1)` + `Text(theme.fg("muted", description), 0, 0)`
3. 若 `searchable`：`Spacer(1)` + `Input`（`onSubmit = () => selectList.handleInput("\r")`）
4. `Spacer(1)`
5. `selectList = buildSelectList(options, currentValue)`；记录 `listChildIndex = children.length`
6. `Spacer(1)`
7. 提示（逐字）：searchable → `"  Type to filter · Enter to select · Esc to go back"`；否则 `"  Enter to select · Esc to go back"`，颜色 `dim`。**`·` 为 U+00B7**。

`buildSelectList(options, preselect)`：
```
list = new SelectList(options, Math.min(options.length, 10), getSelectListTheme(), this.listLayout)
idx = options.findIndex(o => o.value === preselect); if (idx !== -1) list.setSelectedIndex(idx)
list.onSelect = item => onSelectCb(item.value)
list.onCancel = onCancelCb
if (onSelectionChangeCb) list.onSelectionChange = item => cb(item.value)
```
`applyFilter(query)`：
```
filtered = query ? fuzzyFilter(allOptions, query, item => `${item.label} ${item.description ?? ""}`) : allOptions
newList = buildSelectList(filtered, "")     // 过滤后用空预选
children[listChildIndex] = newList
selectList = newList
```
`handleInput(data)`：
```
searchable:
  isNav = matches(tui.select.up) || matches(tui.select.down) || matches(tui.select.confirm) || matches(tui.select.cancel)
  isNav ? selectList.handleInput(data) : (searchInput.handleInput(data); applyFilter(getValue()))
否则 selectList.handleInput(data)
```

**`SteppedSubmenu`**：`context = {...(opts.initialContext ?? {})}`；`activeComponent = buildStep(opts.startAtStep ?? 0)`。
`buildStep(stepIndex)`：
```
step = steps[stepIndex]; total = steps.length
stepLabel = total > 1 ? `Step ${stepIndex+1}/${total} · ` : ""
title = typeof step.title === "function" ? step.title(context) : step.title
desc  = typeof step.description === "function" ? step.description(context) : step.description
items = step.options(context)
preselect = step.preselect?.(context) ?? ""
new SelectSubmenu(title, `${stepLabel}${desc}`, items, preselect,
  value => {
    context[step.key] = value
    if (stepIndex < total - 1) activeComponent = buildStep(stepIndex + 1)
    else {
      onComplete({...context})
      if (opts.loop) { context = {}; activeComponent = buildStep(0) }
      else onCancel()
    }
  },
  () => {                                  // Esc
    if (stepIndex > 0) { delete context[step.key]; activeComponent = buildStep(stepIndex - 1) }
    else onCancel()
  },
  undefined,                               // 无 onSelectionChange
  (step.searchable || step.layout) ? { searchable: step.searchable, layout: step.layout } : undefined)
```
`render` / `handleInput` / `invalidate` 全部委托给 `activeComponent`（`handleInput` 用 `?.` 可选调用）。

**逐字细节**：
- 步骤前缀 `Step ${i}/${n} · ` **仅当 `total > 1`** 才加。
- 完成最后一步时**先** `onComplete({...context})` **再** `onCancel()`（非 loop 模式）——调用方必须容忍“先成功回调再取消回调”的配对。
- loop 模式下 `context` 被重置为 `{}`（**不是** `initialContext`）。

### 2.56 `components/oauth-selector.ts`（224 行）

```ts
export type AuthSelectorProvider = {
  id: string; name: string; authType: "oauth" | "api_key";
  method?: ApiKeyAuth | OAuthAuth; status?: AuthCheck;
  subscription?: boolean;    // false → "account"；未设置 → "subscription"
};
export function formatAuthSelectorProviderType(authType, subscription?): string;
export function formatAuthSelectorProviderStatus(provider: AuthSelectorProvider): string;
export class OAuthSelectorComponent extends Container implements Focusable {
  constructor(mode: "login"|"logout", providers: AuthSelectorProvider[],
              onSelect: (providerId: string, authType) => void, onCancel: () => void,
              initialSearchInput?: string);
  handleInput(keyData: string): void;
}
```

`formatAuthSelectorProviderType`：`api_key` → `"API key"`；否则 `subscription === false ? "account" : "subscription"`。

`formatAuthSelectorProviderStatus`（**逐字**）：
```
!provider.status                                    → theme.fg("muted", " • not configured")
status.type !== provider.authType                   → theme.fg("muted"," • ") + theme.fg("warning",
                                                         `${formatAuthSelectorProviderType(status.type, subscription)} configured`)
status.source 为假 | === "OAuth" | === "stored credential" → theme.fg("success", " ✓ configured")
否则                                               → source = /^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$/.test(status.source)
                                                              ? `env: ${status.source}` : status.source
                                                       theme.fg("success", ` ✓ ${source}`)
```
> 环境变量正则允许多个变量以 `", "` 连接（例如 `ANTHROPIC_API_KEY, OPENAI_API_KEY`）。

**构造布局**：`DynamicBorder()` → `Spacer(1)` → `TruncatedText(theme.fg("accent", theme.bold(title)), 1, 0)` → `Spacer(1)` → `Input`（可选初值；`onSubmit` 选中当前项） → `Spacer(1)` → `listContainer` → `Spacer(1)` → `DynamicBorder()` → `filterProviders(initialSearchInput ?? "")`。
标题：`mode === "login" ? "Select provider to configure:" : "Select provider to logout:"` **【逐字】**。
`showAuthTypeLabels = new Set(providers.map(p => p.authType)).size > 1`（**基于全量 providers，不是过滤后的**）。

`filterProviders(query)`：`fuzzyFilter(allProviders, query, p => `${p.name} ${p.id} ${p.authType} ${p.method?.name ?? ""}`)`；`selectedIndex = Math.max(0, Math.min(selectedIndex, Math.max(0, len-1)))`；`updateList()`。

`updateList()`：`maxVisible = 8`；`startIndex = Math.max(0, Math.min(selectedIndex - 4, len - 8))`；`endIndex = Math.min(startIndex + 8, len)`。
每行：
```
authTypeLabel = showAuthTypeLabels ? theme.fg("muted", ` [${formatAuthSelectorProviderType(p.authType, p.subscription)}]`) : ""
selected: theme.fg("accent","→ ") + theme.fg("accent", p.name) + authTypeLabel + statusIndicator
otherwise: `  ${theme.fg("text", p.name)}` + authTypeLabel + statusIndicator
→ new TruncatedText(line, 1, 0)
```
滚动指示：`TruncatedText(theme.fg("muted", `  (${selectedIndex+1}/${len})`), 1, 0)`。
空表文案（逐字，三级）：
```
allProviders.length === 0 ? (mode === "login" ? "No providers available" : "No providers logged in. Use /login first.")
                         : "No matching providers"
→ TruncatedText(theme.fg("muted", `  ${message}`), 1, 0)
```

`handleInput`：`tui.select.up` → `Math.max(0, idx-1)`（**不环绕**）；`tui.select.down` → `Math.min(len-1, idx+1)`（**不环绕**）；`tui.select.confirm` → `onSelectCallback(id, authType)`；`tui.select.cancel` → `onCancelCallback()`；其他 → 搜索输入 + `filterProviders`。
> 与 §2.52/§2.53 的环绕行为不同，这里**不环绕**。

`onSubmit`（Input 上）：选中当前项 → `onSelectCallback(id, authType)`（**不调用** onCancel，由上层关闭）。

**测试**（`oauth-selector.test.ts`，6 条）。

### 2.57 `components/login-dialog.ts`（322 行）

```ts
export class LoginDialogComponent extends Container implements Focusable {
  constructor(tui: TUI, providerId: string, onComplete: (success: boolean, message?: string) => void,
              providerNameOverride?: string, titleOverride?: string);
  get signal(): AbortSignal;
  showAuth(url: string, instructions?: string): void;
  showDeviceCode(info: OAuthDeviceCodeInfo): void;
  showManualInput(prompt: string): Promise<string>;
  showPrompt(message: string, placeholder?: string): Promise<string>;
  showDetails(lines: string[]): void;
  showInfo(message: string, links?: readonly AuthInfoLink[], showCloseHint?: boolean): void;
  showWaiting(message: string): void;
  showProgress(message: string): void;
  handleInput(data: string): void;
}
```

**字段**：`contentContainer`、`input: Input`、`tui`、`authUrl?: AuthUrlComponent`、`abortController`、`inputResolver?`、`inputRejecter?`、`deviceCodeAction?: {info, hint}`、`deviceCodeHint?: Container`、`onComplete`。

**构造**：`providerName = providerNameOverride || providerId`；`title = titleOverride ?? `Login to ${providerName}``。
布局：`DynamicBorder()` → `Text(theme.fg("accent", theme.bold(title)), 1, 0)` → `contentContainer`（空） → **不 addChild input**（input 只在需要时插入 contentContainer） → `DynamicBorder()`。
`input.onSubmit`：若有 `inputResolver` → 取值 → `replaceInputWithSubmittedText(value)` → resolve → 清空 resolver/rejecter。
`input.onEscape`：`this.cancel()`。

`replaceInputWithSubmittedText(value)`：把 `contentContainer.children` 中**等于 `this.input` 的那一项**替换为 `new Text(`> ${value}`, 0, 0)`（原地替换，其它子项不动）。
`cancel()`：`clearDeviceCodeAction()` → `abortController.abort()` → 若有 rejecter 则 `reject(new Error("Login cancelled"))` → `onComplete(false, "Login cancelled")`。
> **幂等性**：`cancel()` 可被多次触发（Esc + onComplete 回调），`abort()` 与 `onComplete` 都会重复执行；上层需自行容忍。

`showAuth(url, instructions?)`：`clearDeviceCodeAction()` → `contentContainer.clear()` → `Spacer(1)` → `authUrl = new AuthUrlComponent(tui, url)` → 若 `instructions`：`Spacer(1)` + `Text(theme.fg("warning", instructions), 1, 0)` → `openBrowser(url)` → `tui.requestRender()`。

`showDeviceCode(info)`：
```
this.authUrl = undefined
info = {...info, openBrowserOnConfirm: info.openBrowserOnConfirm ?? true, copyCodeOnConfirm: info.copyCodeOnConfirm ?? true}
clearDeviceCodeAction(); contentContainer.clear(); addChild(Spacer(1))
linkedUrl = `\x1b]8;;${info.verificationUri}\x07${info.verificationUri}\x1b]8;;\x07`
addChild(Text(theme.fg("accent", linkedUrl), 1, 0))
clickHint = process.platform === "darwin" ? "Cmd+click to open" : "Ctrl+click to open"
addChild(Text(theme.fg("dim", `\x1b]8;;${uri}\x07${clickHint}\x1b]8;;\x07`), 1, 0))
addChild(Spacer(1)); addChild(Text(theme.fg("warning", `Enter code: ${info.userCode}`), 1, 0))
if (!signal.aborted && (openBrowserOnConfirm || copyCodeOnConfirm)) {
  actionLabel = openBrowserOnConfirm ? (copyCodeOnConfirm ? "to open browser and copy code" : "to open browser")
                                     : "to copy code"
  hint = new Container(); hint.addChild(new TruncatedText(`(${keyHint("tui.select.confirm", actionLabel)})`, 1, 0))
  addChild(hint); deviceCodeHint = hint; deviceCodeAction = {info, hint}
}
tui.requestRender()
```

`setDeviceCodeStatus(hint, message)`：**仅当** `!signal.aborted && focused && contentContainer.children.includes(hint)` 才把 `hint.children = [new TruncatedText(theme.fg("dim", message), 1, 0)]` 并 `requestRender()`。

`activateDeviceCode(info, hint)`：
```
setDeviceCodeStatus(hint, info.copyCodeOnConfirm ? "Copying code..." : "Browser open requested")
if (copyCodeOnConfirm) void copyToClipboard(info.userCode)
    .then(() => setDeviceCodeStatus(hint, "Code copied to clipboard"))
    .catch(() => setDeviceCodeStatus(hint, "Copy code manually"))
if (openBrowserOnConfirm) try { url = new URL(info.verificationUri);
    if (url.protocol === "https:" || url.protocol === "http:") openBrowser(url.href) } catch { /* 忽略 */ }
```
> 剪贴板与开浏览器的顺序：**先**写剪贴板（异步、不 await）**再**开浏览器，两者互不阻塞。

`showManualInput(prompt)`：`clearDeviceCodeAction()` → `input.setValue("")` → `Spacer(1)` → `Text(theme.fg("dim", prompt), 1, 0)` → `addChild(this.input)` → `` Text(`(${keyHint("tui.select.cancel","to cancel")})`, 1, 0) `` → `requestRender()` → 返回 Promise（存 resolver/rejecter）。

`showPrompt(message, placeholder?)`：`clearDeviceCodeAction()` → `Spacer(1)` → `Text(theme.fg("text", message), 1, 0)` → 若有 placeholder：`Text(theme.fg("dim", `e.g., ${placeholder}`), 1, 0)` → `addChild(this.input)` → `` Text(`(${keyHint("tui.select.cancel","to cancel,")} ${keyHint("tui.select.confirm","to submit")})`, 1, 0) `` → `input.setValue("")` → `requestRender()` → Promise。
> 注意提示串第一个键位说明是 **`"to cancel,"`（带逗号）**，第二个是 `"to submit"`。

`showDetails(lines)`：`authUrl = undefined` → `clearDeviceCodeAction()` → `contentContainer.clear()` → `Spacer(1)` → 每行 `Text(line, 1, 0)`（**原样，不加主题色**）→ `requestRender()`。

`showInfo(message, links = [], showCloseHint = false)`：**不清空**（追加） → `Spacer(1)` → `Text(theme.fg("text", message), 1, 0)` → 每个 link：`` text = link.label ? `${link.label}: ${link.url}` : link.url ``，`hyperlink = `\x1b]8;;${link.url}\x07${text}\x1b]8;;\x07``，`Text(theme.fg("accent", hyperlink), 1, 0)` → 若 `showCloseHint`：`Spacer(1)` + `` Text(`(${keyHint("tui.select.cancel","to close")})`, 1, 0) `` → `requestRender()`。

`showWaiting(message)`：追加 `Spacer(1)` + `Text(theme.fg("dim", message), 1, 0)` + `` Text(`(${keyHint("tui.select.cancel","to cancel")})`, 1, 0) `` → `requestRender()`。

`showProgress(message)`：追加 `Text(theme.fg("dim", message), 1, 0)` → `requestRender()`。

**`handleInput(data)` 顺序（逐字优先级）**：
1. `tui.select.cancel` → `cancel()`，return。
2. `this.authUrl && app.message.copy` → `void this.authUrl.copy()`，return。
3. `deviceCodeAction && !inputResolver && !signal.aborted && focused && tui.select.confirm` → 取出 action，**立刻置 `deviceCodeAction = undefined`**（消费一次，防重复 Enter 触发多份剪贴板/浏览器），`activateDeviceCode(info, hint)`，`requestRender()`，return。
4. 其他 → `this.input.handleInput(data)`。

**测试**（`login-dialog-device-code.test.ts`，6 条）。

### 2.58 `components/radius-login-selector.ts`（114 行）

```ts
const RADIUS_COLORS: readonly Color[] = ["#4d9abf","#83ccd2","#f1be57","#f09082"].map(parseColor);
const CHARS_PER_COLOR = 4;
const CHARS_PER_SECOND = 10;
const ANIMATION_FRAME_MS = 50;

type RadiusOption = { label: string; text: string };   // label 以 text 开头

export function createLoginMenuSelector(tui: TUI, title: string, options: string[],
                                        radiusOption: RadiusOption,
                                        onSelect: (option: string) => void,
                                        onCancel: () => void): ExtensionSelectorComponent;
```
内部类 `RadiusLoginMenuComponent extends ExtensionSelectorComponent`。

`radiusShimmer(text, elapsedMs)`（**逐字算法**）：
```
mode = theme.getColorMode()
cycle = 4 * 4 = 16
offset = (elapsedMs / 1000) * 10
result = ""; index = 0
for (const char of text) {            // 按 Unicode 码点迭代，不是 UTF-16 单元
  position = (((index - offset) % cycle) + cycle) % cycle
  band = Math.floor(position / 4)
  t = position / 4 - band
  amount = t*t*(3-2*t)                // smoothstep
  from = RADIUS_COLORS[band]; to = RADIUS_COLORS[(band+1) % 4]
  result += foregroundAnsi(mixColors(from, to, amount, "srgb"), mode) + char
  index++
}
return `${result}\x1b[39m`            // 末尾复位前景色
```
> `index` 按**码点**递增，而 `text` 用 `for...of` 迭代——Rust 中应用 `chars()` 而非字节索引。

**构造**：调用 `super(title, options, wrappedOnSelect, wrappedOnCancel)`（两个 wrapper 各自先 `stopAnimation()` 再转发）；`animationStart = performance.now()`；建立 `setInterval(() => { if (this.animating) tui.requestRender() }, 50)` 并调用 `this.animationTimer.unref?.()`。

**`render(width)` 覆写**：
```
lines = super.render(width)
selectedLine = new Text(theme.fg("accent","→ ") + theme.fg("accent", label), 1, 0).render(width)[0]
index = selectedLine === undefined ? -1 : lines.indexOf(selectedLine)
this.animating = index >= 0
if (animating) {
  shimmer = radiusShimmer(text, performance.now() - animationStart)
  animatedLine = theme.fg("accent","→ ") + shimmer + label.slice(text.length)
  lines[index] = new Text(animatedLine, 1, 0).render(width)[0] ?? ""
}
return lines
```
> 命中条件是**整行字符串完全相等**：若选择器行样式（颜色/前缀/内边距）改变，或 label 被折行，则 `indexOf` 找不到 → 该选项按普通样式渲染（`animating = false`，定时器空转）。Rust 移植必须保持“按渲染后首行字符串匹配”这一策略，而不是按选项下标。

`stopAnimation()`：`clearInterval`；`animationTimer = undefined`；`animating = false`。
`dispose()`：`stopAnimation()` 后 `super.dispose()`。

**测试**：`theme-style.test.ts` 中覆盖 shimmer 的颜色带与复位序列（若有）。

### 2.59 `components/session-selector-search.ts`（260 行）

```ts
export type SortMode = "threaded" | "recent" | "relevance";
export type NameFilter = "all" | "named";

export interface ParsedSearchQuery {
  mode: "tokens" | "regex";
  tokens: { kind: "fuzzy" | "phrase"; value: string }[];
  regex: RegExp | null;
  error?: string;      // 解析失败时置位，匹配一律为 false
}
export interface MatchResult { matches: boolean; score: number; }

export function hasSessionName(session: SessionInfo): boolean;
export function parseSearchQuery(query: string): ParsedSearchQuery;
export function matchSession(session: SessionInfo, parsed: ParsedSearchQuery): MatchResult;
export function filterAndSortSessions(sessions: SessionInfo[], query: string, sortMode: SortMode,
                                      nameFilter?: NameFilter): SessionInfo[];   // 默认 "all"
```

**分档常量（逐字）**：
```
TIER_NAME = 0;  TIER_TEXT = 1;  TIER_FUZZY = 2;  TIER_WEIGHT = 1_000_000
tierScore(tier, score = 0) = tier * TIER_WEIGHT + score
```

**缓存**：`messageTextCache: WeakMap<SessionInfo, {source, lower, normalized?}>`，键是 `session.allMessagesText` 的**字符串恒等**；若 `source` 变了则重建 `lower = text.toLowerCase()`；`normalized` 懒计算 = `lower.replace(/\s+/g," ").trim()`。
> Rust 无 WeakMap 语义：可用 `HashMap<SessionId, (String /*source*/, String /*lower*/, Option<String> /*normalized*/)>` 以 `allMessagesText` 内容哈希/长度比对，或每次过滤时重建（注意 `allMessagesText` 可能达数 MB，避免每次按键重复小写化）。

**`normalizeWhitespaceLower(text)`**：`text.toLowerCase().replace(/\s+/g," ").trim()`。

**`parseSearchQuery(query)`**：
1. `trimmed = query.trim()`；空 → `{ mode:"tokens", tokens:[], regex:null }`。
2. 以 `re:` 开头：`pattern = trimmed.slice(3).trim()`；空 → `{ mode:"regex", tokens:[], regex:null, error:"Empty regex" }`；`new RegExp(pattern, "i")` 成功 → `{mode:"regex", tokens:[], regex}`；抛错 → `error = err.message`（Error 取 message，否则 `String(err)`）。
3. 否则词法扫描（`"` 切换短语模式；引号外空白 flush `fuzzy`；引号内字符全部累积）：
```
tokens = []; buf = ""; inQuote = false; hadUnclosedQuote = false
flush(kind): v = buf.trim(); buf = ""; if (v) tokens.push({kind, value: v})
for ch of trimmed:
  '"' → inQuote ? (flush("phrase"); inQuote = false) : (flush("fuzzy"); inQuote = true)
  引号外空白 → flush("fuzzy")
  否则 buf += ch
if (inQuote) hadUnclosedQuote = true
if (hadUnclosedQuote) → 回退：trimmed.split(/\s+/).map(trim).filter(len>0).map(t => ({kind:"fuzzy", value:t}))
flush(inQuote ? "phrase" : "fuzzy")
```
> 末尾 `inQuote` 在回退分支已 return，故最终 `flush` 实际总是 `fuzzy`。

**`matchLiteralToken(session, value)`**（`value` 必须已小写）：
```
name = session.name?.toLowerCase() ?? ""
name.includes(value)                     → tierScore(TIER_NAME, fuzzyMatch(value, name).score)
session.id.toLowerCase().includes(value) → tierScore(TIER_TEXT)
lower(allMessagesText).includes(value)   → tierScore(TIER_TEXT)
session.cwd.toLowerCase().includes(value)→ tierScore(TIER_TEXT)
否则 undefined
```
**`matchFuzzyToken(session, value)`**：先 `matchLiteralToken(session, value.toLowerCase())`；命中则返回；否则对 `[session.name ?? "", session.cwd]` 求 `fuzzyMatch(value, field)`，取 `matches && score 最小` 者，最后 `tierScore(TIER_FUZZY, best)`。
**`matchPhraseToken(session, phrase)`**（`phrase` 已 normalize + 小写）：name（normalized）/ id / normalizedMessageText / normalized cwd 依次 `includes`；name 命中返回 `tierScore(TIER_NAME, fuzzyMatch(phrase, name).score)`，其余 `tierScore(TIER_TEXT)`。
**`matchRegex(session, regex)`**：`session.name && regex.test(name)` → `tierScore(TIER_NAME)`；否则 `regex.test(id) || regex.test(allMessagesText) || regex.test(cwd)` → `tierScore(TIER_TEXT)`；否则 undefined。
> 正则**带 `g` 标志时 `test` 有 lastIndex 副作用**；这里始终用 `"i"` 标志，无副作用。Rust 的 `regex` crate 不支持回溯引用/lookaround，需在文档中记录该差异，或改用 `fancy-regex`。

**`hasSessionName(session)`**：`Boolean(session.name?.trim())`（全空白视为无名字）。
`matchesNameFilter(session, filter)`：`"all"` → true；否则 `hasSessionName`。

**`matchSession(session, parsed)`**：
```
regex 模式：!parsed.regex → {matches:false, score:0}；否则 matchRegex 结果 undefined → false，否则 {true, score}
tokens 模式：totalScore = 0
  for token: phrase → phrase = normalizeWhitespaceLower(value); 空则 continue; score = matchPhraseToken(...)
              fuzzy  → score = matchFuzzyToken(...)
              score === undefined → return {matches:false, score:0}
              totalScore += score
  return {matches:true, score: totalScore}
```
> 语义：**不同 token 可以命中不同字段**，但单个 token 不能跨字段匹配。

**`filterAndSortSessions(sessions, query, sortMode, nameFilter = "all")`**：
```
nameFiltered = nameFilter === "all" ? sessions : sessions.filter(matchesNameFilter)
trimmed = query.trim(); 空 → 直接返回 nameFiltered（保持输入顺序）
parsed = parseSearchQuery(query); parsed.error → []
sortMode === "recent" → 过滤但保持输入顺序
否则（relevance，也用于 "threaded"）→ 按 score 升序，同分按 session.modified.getTime() 降序
```
> `"threaded"` 与 `"relevance"` 在此函数中走**同一分支**（`recent` 是唯一特例）；树形重排在 `session-selector.ts` 里完成（见 §2.43）。

**测试**（`session-selector-search.test.ts`，13 条）：`parseSearchQuery` 的空串/短语/未闭合引号/`re:`/空正则/非法正则；`hasSessionName`；分档优先级（name 命中优于 text 命中优于 fuzzy）；`recent` 保持顺序；`relevance` 同分按时间；`nameFilter` 过滤；错误查询返回空。

---


## 3. 用户可见面（逐字）

### 3.1 内置斜杠命令（`BUILTIN_SLASH_COMMANDS`，定义在 `core/slash-commands.ts`，本单元消费）

| name | description（逐字） | argumentHint |
|---|---|---|
| `settings` | `Open settings menu` | — |
| `model` | `Select model (opens selector UI)` | `<provider/model>` |
| `tree` | `Navigate session tree (switch branches)` | — |
| `thinking` | `Set thinking level` | `<level>` |
| `scoped-models` | `Enable/disable models for Ctrl+P cycling` | — |
| `export` | `Export session (HTML default, or specify path: .html/.jsonl)` | — |
| `import` | `Import and resume a session from a JSONL file` | — |
| `share` | `Share session via a Radius artifact or a GitHub gist` | — |
| `bug` | `Report a bug to the Kiana developers` | `<description>` |
| `copy` | `Copy last agent message to clipboard` | — |
| `name` | `Set session display name` | — |
| `session` | `Show session info and stats` | — |
| `changelog` | `Show changelog entries` | — |
| `hotkeys` | `Show all keyboard shortcuts` | — |
| `fork` | `Create a new fork from a previous user message` | — |
| `clone` | `Duplicate the current session at the current position` | — |
| `trust` | `Save project trust decision for future sessions` | — |
| `login` | `Configure provider authentication` | `<provider>` |
| `logout` | `Remove provider authentication` | — |
| `new` | `Start a new session` | — |
| `compact` | `Manually compact the session context` | — |
| `resume` | `Resume a different session` | — |
| `reload` | `Reload keybindings, extensions, skills, prompts, themes, and context files` | — |
| `quit` | `` `Quit ${APP_NAME}` ``（即 `Quit kiana`） | — |

另有两个**未进自动补全但可执行**的彩蛋命令：`/debug`、`/arminsayshi`、`/dementedelves`（见 §2.50.16）。扩展命令与 skill 命令（`skill:<name>`）追加在模板命令之后；与内置同名的扩展命令**不进**自动补全（见 §2.50.18「与内置冲突诊断」）。

### 3.2 提交分派（`setupEditorSubmitHandler`）

`text = text.trim()`；空串直接返回。命令按**下列顺序**逐条精确比较（`startsWith` 的命令允许带参数）：

| 条件 | 动作 |
|---|---|
| `"/settings"` | `showSettingsSelector()` |
| `"/scoped-models"` | `await showModelsSelector()` |
| `"/model"` 或 `"/model "` 前缀 | `await handleModelCommand(searchTerm)` |
| `"/thinking"` 或 `"/thinking "` 前缀 | `handleThinkingCommand(searchTerm)` |
| `"/export"` 或 `"/export "` 前缀 | `await handleExportCommand(text)` |
| `"/import"` 或 `"/import "` 前缀 | `await handleImportCommand(text)` |
| `"/share"` | `await handleShareCommand()` |
| `"/bug"` 或 `"/bug "` 前缀 | `await handleBugCommand(hint)` |
| `"/copy"` | `await handleCopyCommand()` |
| `"/name"` 或 `"/name "` 前缀 | `handleNameCommand(text)` |
| `"/session"` | `handleSessionCommand()` |
| `"/changelog"` | `handleChangelogCommand()` |
| `"/hotkeys"` | `handleHotkeysCommand()` |
| `"/fork"` | `showUserMessageSelector()` |
| `"/clone"` | `await handleCloneCommand()` |
| `"/tree"` | `showTreeSelector()` |
| `"/trust"` | `showTrustSelector()` |
| `"/login"` 或 `"/login "` 前缀 | `await handleLoginCommand(providerRef)` |
| `"/logout"` | `showOAuthSelector("logout")` |
| `"/new"` | `await handleClearCommand()` |
| `"/compact"` 或 `"/compact "` 前缀 | `await handleCompactCommand(customInstructions)` |
| `"/reload"` | `await handleReloadCommand()` |
| `"/debug"` | `handleDebugCommand()` |
| `"/arminsayshi"` | `handleArminSaysHi()` |
| `"/dementedelves"` | `handleDementedDelves()` |
| `"/resume"` | `showSessionSelector()` |
| `"/quit"` | `await shutdown()` |
| 以 `"!"` 开头 | bash 模式（见下） |
| 其他 | 见「非命令输入」 |

参数切片长度（**逐字**）：`/model ` → `slice(7)`；`/thinking ` → `slice(10)`；`/bug` → `slice("/bug".length)`；`/login ` → `slice(7)`；`/compact ` → `slice(9)`。

**bash 模式**：`isExcluded = text.startsWith("!!")`；`command = isExcluded ? text.slice(2).trim() : text.slice(1).trim()`；非空时：
- `session.isBashRunning` → `showWarning("A bash command is already running. Press Esc to cancel it first.")`，并把原文本**放回编辑器**；return。
- 否则 `editor.addToHistory?.(text)` → `await handleBashCommand(command, isExcluded)` → `isBashMode = false` → `updateEditorBorderColor()`。

**非命令输入**：
1. `session.isCompacting`：扩展命令 → `addToHistory` + 清空编辑器 + `await session.prompt(text)`；否则 `queueCompactionMessage(text, "steer")`。
2. `session.isStreaming`：`addToHistory` + 清空 + `await session.prompt(text, { streamingBehavior: "steer" })` + `updatePendingMessagesDisplay()` + `requestRender()`。
3. 正常：`flushPendingBashComponents()` → 有 `onInputCallback` 则回调，否则 `pendingUserInputs.push(text)` → `editor.addToHistory?.(text)`。

命令处理器执行后都会 `editor.setText("")`（`/export`、`/import`、`/share`、`/bug`、`/copy`、`/name`、`/session`、`/changelog`、`/hotkeys`、`/fork`、`/clone`、`/tree`、`/trust`、`/login`、`/logout`、`/new`、`/compact`、`/reload`、`/debug`、`/arminsayshi`、`/dementedelves`、`/resume`、`/quit`）。`/settings`、`/scoped-models`、`/model`、`/thinking` 也清空（在调用前）。

### 3.3 键位（默认值，来自 `docs/keybindings.md` 与 `core/keybindings.ts`）

#### 主编辑器 / TUI

| 动作 | 默认键 |
|---|---|
| `tui.editor.cursorUp` | `up` |
| `tui.editor.cursorDown` | `down` |
| `tui.editor.historyPrevious` | 无 |
| `tui.editor.historyNext` | 无 |
| `tui.editor.cursorLeft` | `left`, `ctrl+b` |
| `tui.editor.cursorRight` | `right`, `ctrl+f` |
| `tui.editor.cursorWordLeft` | `alt+left`, `ctrl+left`, `alt+b` |
| `tui.editor.cursorWordRight` | `alt+right`, `ctrl+right`, `alt+f` |
| `tui.editor.cursorLineStart` | `home`, `ctrl+a` |
| `tui.editor.cursorLineEnd` | `end`, `ctrl+e` |
| `tui.editor.jumpForward` | `ctrl+]` |
| `tui.editor.jumpBackward` | `ctrl+alt+]` |
| `tui.editor.pageUp` | `pageUp`, `ctrl+pageUp` |
| `tui.editor.pageDown` | `pageDown`, `ctrl+pageDown` |
| `tui.editor.deleteCharBackward` | `backspace` |
| `tui.editor.deleteCharForward` | `delete`, `ctrl+d` |
| `tui.editor.deleteWordBackward` | `ctrl+w`, `alt+backspace` |
| `tui.editor.deleteWordForward` | `alt+d`, `alt+delete` |
| `tui.editor.deleteToLineStart` | `ctrl+u` |
| `tui.editor.deleteToLineEnd` | `ctrl+k` |
| `tui.editor.yank` | `ctrl+y` |
| `tui.editor.yankPop` | `alt+y` |
| `tui.editor.undo` | `ctrl+-`（Windows `ctrl+z`，WSL `alt+z`） |
| `tui.input.newLine` | `shift+enter`, `ctrl+j` |
| `tui.input.submit` | `enter` |
| `tui.input.tab` | `tab` |
| `tui.input.copy` | `ctrl+c` |
| `tui.select.up` | `up` |
| `tui.select.down` | `down` |
| `tui.select.pageUp` | `pageUp` |
| `tui.select.pageDown` | `pageDown` |
| `tui.select.confirm` | `enter` |
| `tui.select.cancel` | `escape`, `ctrl+c` |

#### App

| 动作 | 默认键 | 说明 |
|---|---|---|
| `app.interrupt` | `escape` | Cancel / abort |
| `app.clear` | `ctrl+c` | Clear editor (first) / exit (second) |
| `app.exit` | `ctrl+d` | Exit (when editor empty) |
| `app.suspend` | `ctrl+z`（Windows 无） | Suspend to background |
| `app.editor.external` | `ctrl+g` | Open in external editor |
| `app.clipboard.pasteImage` | `ctrl+v`（Windows/WSL `alt+v`） | Paste files on macOS, images, or text |
| `app.session.new` | 无 | Start a new session |
| `app.session.tree` | 无 | Open session tree navigator |
| `app.session.fork` | 无 | Fork current session |
| `app.session.resume` | 无 | Open session resume picker |
| `app.session.togglePath` | `ctrl+p` | Toggle path display |
| `app.session.toggleSort` | `ctrl+s` | Toggle sort mode |
| `app.session.toggleNamedFilter` | `ctrl+n` | Toggle named-only filter |
| `app.session.rename` | `ctrl+r` | Rename session |
| `app.session.delete` | `ctrl+d` | Delete session |
| `app.session.deleteNoninvasive` | `ctrl+backspace` | Delete session when query is empty |
| `app.model.select` | `ctrl+l` | Open model selector |
| `app.model.cycleForward` | `ctrl+p` | Cycle to next model |
| `app.model.cycleBackward` | `shift+ctrl+p`（Windows/WSL `alt+p`） | Cycle to previous model |
| `app.models.save` | `ctrl+s` | Save default model / scoped config |
| `app.thinking.cycle` | `shift+tab` | Cycle thinking level |
| `app.thinking.save` | `ctrl+s` | Save thinking level to settings |
| `app.thinking.toggle` | `ctrl+t` | Collapse or expand thinking blocks |
| `app.tools.expand` | `ctrl+o` | Collapse or expand tool output |
| `app.message.copy` | `ctrl+x` | Copy selected message / fullscreen selection / last assistant message / OAuth URL |
| `app.message.followUp` | `alt+enter`（Windows/WSL `ctrl+q`） | Queue follow-up message |
| `app.message.dequeue` | `alt+up`（Windows/WSL `alt+q`） | Restore queued messages to editor |
| `app.tree.foldOrUp` | `ctrl+left`, `alt+left` | Fold segment or jump to previous segment start |
| `app.tree.unfoldOrDown` | `ctrl+right`, `alt+right` | Unfold segment or jump to next segment start |
| `app.tree.editLabel` | `shift+l` | Edit label |
| `app.tree.toggleLabelTimestamp` | `shift+t` | Toggle label timestamps |
| `app.tree.filter.default` | `ctrl+d` | Default filter |
| `app.tree.filter.noTools` | `ctrl+t` | Toggle no-tools filter |
| `app.tree.filter.userOnly` | `ctrl+u` | Toggle user-only filter |
| `app.tree.filter.labeledOnly` | `ctrl+l` | Toggle labeled-only filter |
| `app.tree.filter.all` | `ctrl+a` | Toggle all filter |
| `app.tree.filter.cycleForward` | `ctrl+o` | Cycle filter forward |
| `app.tree.filter.cycleBackward` | `shift+ctrl+o` | Cycle filter backward |
| `app.models.enableAll` | `ctrl+a` | Enable all (or filtered) |
| `app.models.clearAll` | `ctrl+x` | Clear all (or filtered) |
| `app.models.toggleProvider` | `ctrl+p` | Toggle all models for provider |
| `app.models.reorderUp` | `alt+up` | Move model up |
| `app.models.reorderDown` | `alt+down` | Move model down |

#### Fullscreen（`tui.altScreen.*`）

| 动作 | 默认键 |
|---|---|
| `pageUp` / `pageDown` | `pageUp` / `pageDown` |
| `halfPageUp` / `halfPageDown` | 无 |
| `lineUp` / `lineDown` | 无 |
| `previousPrompt` | `ctrl+shift+up`, `ctrl+up` |
| `nextPrompt` | `ctrl+shift+down`, `ctrl+down` |
| `search` | `ctrl+shift+f`（Windows/WSL `ctrl+f`） |
| `searchNext` | `enter`, `ctrl+g` |
| `searchPrevious` | `shift+enter`, `ctrl+shift+g` |
| `searchClose` | `escape` |
| `top` / `bottom` | `ctrl+home` / `ctrl+end` |

**键位语法**（`keybindings.json`）：`modifier+key`，modifier ∈ `ctrl|shift|alt|super`；键名见 `docs/keybindings.md`（字母 `a-z`、数字 `0-9`、`escape/esc/enter/return/tab/space/backspace/delete/insert/clear/home/end/pageUp/pageDown/up/down/left/right`、`f1-f12`、符号集）。配置值**替换**默认；空数组表示禁用。文件位置 `<agent-dir>/keybindings.json`（默认 `~/.kiana/agent/keybindings.json`）。

### 3.4 Spinner 与加载文案

`Loader` 默认帧（`@earendil-works/kiana-tui` 的 `DEFAULT_FRAMES`，**逐字节**）：
```
["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"]
```
帧间隔 `80 ms`。渲染格式：`${coloredFrame} ${coloredMessage}`（帧为空串时不加空格）；`Loader.render` 在正文前**多一行空串**（`["", ...super.render(width)]`）。

本单元出现的所有 loader 文案：
| 位置 | 文案 |
|---|---|
| bash 运行中 | `Running... (${keyText("tui.select.cancel")} to cancel)` |
| 分享 → Radius | `Uploading to Radius...` |
| 分享 → gist | `Creating gist...` |
| bug 报告摘要 | `Writing summary with ${model?.name ?? "the current model"}...` |
| bug 报告上传 | `Uploading bug report...` |
| 重试倒计时 | `Retrying (${attempt}/${maxAttempts}) in ${seconds}s... (${keyText("app.interrupt")} to cancel)` |
| 压缩 | `Compacting context... (${keyText("app.interrupt")} to cancel)` / `Context overflow detected, Auto-compacting... (...)` / `Auto-compacting... (...)` |
| 分支摘要 | `Summarizing branch... (${keyText("app.interrupt")} to cancel)` |
| reload | `Reloading keybindings, extensions, skills, prompts, themes, and context files...` |

### 3.5 状态指示器嵌入编辑器边框

见 §2.25 `renderTopBorder` 与 §2.39。要点：
- 只有 `CustomEditor` 的 `embedWorkingStatus` 为 true 才嵌入；否则 spinner 单独占一行（`Loader` 的空行 + 文本行）。
- 边框色与 `── ` 前缀来自 `EditorTheme.borderColor`。
- 折叠提示 `" ↑ ${n} more "` 居中放置，若空间不足则退回只显示 spinner。
- `renderInBorder` 取 `Loader.render(width+2)[1]`（第 2 行，即 spinner 行），去掉前导空格并 `trimEnd`。

### 3.6 消息组件的 ANSI 细节

| 组件 | 序列/样式 |
|---|---|
| `AssistantMessageComponent`（无 toolCall） | 行首 `\x1b]133;A\x07`，行尾前置 `\x1b]133;B\x07\x1b]133;C\x07` |
| `UserMessageComponent` | 同上（无条件） |
| 折叠消息标签 | `\x1b[1m[branch]\x1b[22m`、`\x1b[1m[compaction]\x1b[22m`、`\x1b[1m[skill]\x1b[22m` |
| 自定义消息标签 | `\x1b[1m[${customType}]\x1b[22m` |
| 超链接（`hyperlink()`） | `\x1b]8;;<url>\x1b\\<text>\x1b]8;;\x1b\\`（`kiana-tui` 版）；本单元内几处手写为 `\x1b]8;;<url>\x07<text>\x1b]8;;\x07` |
| 程序状态（OSC 7501） | `\x1b]7501;state=<state>[:app=<app>][:kind=<kind>][:msg=<base64>]\x1b\\` |
| 终端进度（OSC 9;4） | 激活 `\x1b]9;4;3\x07`，清除 `\x1b]9;4;0\x07` |
| 全屏水平裁剪行尾 | `"\x1b[0m"` |
| 3D 彩蛋每行结尾 | `"\x1b[0m"`；前景复位 `\x1b[39m`；背景复位 `\x1b[49m` |
| 边框字符 | `─`；树连接符 `├─ `、`└─ `、`│`、折叠 `⊟`/`⊞`；活跃路径 `• `；游标 `› `、`→ `、`> `；选中标记 `✓ `；复选框 `[x]`/`[ ]`/`[+]`/`[-]` |
| 进度/成功标记 | `◉`（当前 scope）、`○`（非当前）、`●`?（未用）、`⠋` 等 spinner |
| Radius 动画 | 每字符 `foregroundAnsi(mixColors(...))`，结尾 `\x1b[39m` |

### 3.7 布局与宽度常量

| 常量 | 值 | 位置 |
|---|---|---|
| `MAX_WIDGET_LINES` | `10` | 扩展 widget 最大行数 |
| `PREVIEW_LINES` | `20` | bash 折叠预览逻辑行数 |
| `FALLBACK_PREVIEW_LINES` | `10` | 工具结果回退折叠行数 |
| `DISPLAY_HEIGHT`（armin） | `18` | 半块渲染 |
| `maxVisible`（SessionList） | `10` | |
| `maxVisible`（ModelSelector） | `10` | |
| `maxVisible`（ScopedModels） | `8` | |
| `maxVisible`（OAuthSelector） | `8` | |
| `maxVisible`（UserMessageList） | `10` | |
| `maxVisibleLines`（TreeList） | `max(5, floor(terminalHeight/2))` | |
| `maxVisible`（ConfigSelector） | `max(5, (terminalHeight ?? 24) - 8)` | |
| `SelectList`（Thinking/ShowImages/Theme/Submenu） | 高度 5 / 5 / 10 / `min(len,10)` | |
| `SelectListLayoutOptions`（多数选择器） | `{minPrimaryColumnWidth:12, maxPrimaryColumnWidth:32}` | |
| `MODEL_PICKER_LAYOUT` | `{12, 46}` | `settings-selector.ts` |
| `SUBMENU_SELECT_LIST_LAYOUT` | `{12, 32}` | `settings-submenu.ts` |
| tree 缩进 | 每层 3 列，gutter 宽 2 | |
| tree 水平视口 | `MIN_VISIBLE_ANCHOR_CONTENT_WIDTH=4`、`MAX=20`、context `2..12` | |
| footer `minPadding` | `2` | |
| 图片默认 `maxWidthCells` | `60` | 工具结果；earendil 公告 `56` |
| Radius 动画 | `CHARS_PER_COLOR=4`、`CHARS_PER_SECOND=10`、`ANIMATION_FRAME_MS=50` | |
| Armin 动画 | 30 fps（`glitch` 60 fps） | |
| 3D 动画 | 30 fps | |

### 3.8 全部用户可见错误/提示字符串索引

已在前述各节逐字列出；此处给出**按类型**的快速索引（便于抽样核对）：
- 启动：`Migrated credentials to auth.json: ...`、`models.json error: ...`、`kiana crashed on ...`、`Startup is still in progress`、两条 tmux 提示、新版本/包更新通知。
- 命令：`Usage: /import <path.jsonl>`、`Usage: /name <name>`、`No agent messages to copy yet.`、`No changelog entries found.`、`Nothing to clone yet`、`No entries in session`、`No messages to fork from`、`Already at this point`、`Wait for the current response to finish before reloading.`、`Wait for compaction to finish before reloading.`、`Unknown thinking level "..."`。
- 会话：`Cannot delete the currently active session`、`Failed to delete: ...`、`Failed to load sessions: ...`、`Session moved to trash`、`Session deleted`、`Resumed session`、`Resume cancelled`、`Resumed session in current cwd`、`Failed to resume session`、`Forked to new session`、`Cloned to new session`、`Session imported from: ...`、`Import cancelled`、`Session exported to: ...`。
- 认证：`Login cancelled`、`Logged out of ...`、`No login methods available.`、`No stored credentials to remove. ...`、`Radius authentication is not configured. Run '/login radius' first.`、`GitHub CLI is not logged in. Run 'gh auth login' first.`、`GitHub CLI (gh) is not installed. Install it from https://cli.github.com/`、`Failed to parse gist ID from gh output`。
- 模型：`Only one model in scope`、`Only one model available`、`Current model does not support thinking`、`Model refresh timed out; showing cached models.`、`Could not refresh ...; showing cached models.`、`Model catalogs refreshed.`、`Only showing models from configured providers. Use /login to add providers.`、`No matching models`、`No matching providers`、`No providers available`、`No providers logged in. Use /login first.`。
- 工具/状态：`A bash command is already running. Press Esc to cancel it first.`、`Bash command failed: ...`、`Tool output: expanded|collapsed`、`Thinking blocks: hidden|visible`、`No queued messages to restore`、`Restored N queued message(s) to editor`、`Queued message for after compaction`、`Failed to send queued message(s): ...`。
- 全屏/终端：`Suspend to background is not supported on Windows`、`Close active overlays before changing TUI mode`、`Copied!`。
- 信任：`This project is not trusted. ...`、`Saved trust decision: trusted|untrusted. Restart kiana for this to take effect.`。

---



## 4. 外部格式（逐字）

本节列出本单元读写的一切**进程外**格式：环境变量、磁盘文件、HTTP 请求、终端控制序列、子进程调用。凡标注「逐字」的字符串必须按字节一致地产生。

### 4.1 环境变量

| 变量 | 读取点 | 语义 |
|---|---|---|
| `KIANA_CODING_AGENT_DIR`（`ENV_AGENT_DIR`） | `config.ts:getAgentDir()` | 覆盖 agent 目录；值经 `expandTildePath` 展开 `~`。未设置时为 `join(homedir(), ".kiana", "agent")` |
| `KIANA_CODING_AGENT_SESSION_DIR`（`ENV_SESSION_DIR`） | `config.ts` | 覆盖会话目录（本单元仅间接使用） |
| `KIANA_OFFLINE` | `interactive-mode.ts:1151`、`1266`；`bug-report.ts:56`；`main.ts:579` | 真值判断用 `isTruthyEnvFlag`。**用途**：① `/bug` 的 `delivery === "upload"` 时若置位 → `showError("Uploading bug reports requires online mode. Use Export as Zip instead.")`；② `reportInstallTelemetry` 直接 return；③ main 里 `--offline` 会把 `KIANA_OFFLINE` 设为 `"1"` |
| `KIANA_SHARE_VIEWER_URL` | `config.ts:getShareViewerUrl` | 分享查看器基址；未设置时 `DEFAULT_SHARE_VIEWER_URL = "https://kiana.dev/session/"`。拼装（**逐字**）：`baseUrl = process.env.KIANA_SHARE_VIEWER_URL \|\| DEFAULT_SHARE_VIEWER_URL`，返回 `` `${baseUrl}#${gistId}` ``——注意分隔符是 **`#`（片段）**，**不**补斜杠、不做 URL 编码 |
| `VISUAL` / `EDITOR` | `components/extension-editor.ts:59-60` | 外部编辑器命令，优先 `VISUAL`，其次 `EDITOR`，再次平台默认（见 §2.30） |
| `TMUX` | 启动提示 | 若存在且在 tmux 内则给出一条启动提示（见 §2.50.4） |
| `HOME` / `USERPROFILE` | `os.homedir()` | `~` 展开与路径缩短（`formatDisplayPath`、`shortenPath`） |
| `HTTP_PROXY` / `HTTPS_PROXY` | `core/http-dispatcher.ts:applyHttpProxySettings` | 由设置项写入（`??=`，不覆盖已有值） |
| `NO_COLOR` / `FORCE_COLOR` 等 | kiana-tui 能力探测 | 影响 `getCapabilities()` 的颜色与图形能力（详见 tui 单元） |

**旧版规格漏掉的以下变量**（都在本单元的可达路径上，移植时必须实现，否则启动行为不一致）：

| 变量 | 读取点 | 语义（**逐字/逐值**） |
|---|---|---|
| `KIANA_HARDWARE_CURSOR` | `core/settings-manager.ts:1379` `getShowHardwareCursor()` | `settings.showHardwareCursor ?? process.env.KIANA_HARDWARE_CURSOR === "1"`。**只认字符串 `"1"`**（`"true"` 不算）。命中后 `ui.setShowHardwareCursor(true)`（构造 `createInteractiveTui({showHardwareCursor})` 与 `applyRuntimeSettings()` 各用一次） |
| `KIANA_CLEAR_ON_SHRINK` | `settings-manager.ts:1230` `getClearOnShrink()` | 优先级：`settings.terminal.clearOnShrink`（若已设）> `process.env.KIANA_CLEAR_ON_SHRINK === "1"` > 默认 `false`。影响 `ui.setClearOnShrink()`（`false` 且无 `activeStatusIndicator` 时清空 `statusContainer`） |
| `KIANA_PROGRAM_STATUS` | `kiana-tui/src/terminal.ts:300`（`queryAndEnableKittyProtocol`） | `"1"` → 直接认为支持 OSC 7501，**不发查询**；`"0"` → 直接认为不支持，**不发查询**；其它/未设 → 发查询并等 DA1 哨兵。决定 `ProgramStatusReporter` 的 `report()` 是否真的写出 OSC 7501 |
| `KIANA_TRUE_COLOR` / `KIANA_HYPERLINKS` / `KIANA_IMAGE_PROTOCOL` | `kiana-tui/src/terminal-image.ts:151–162`（`detectCapabilities`） | 能力覆盖：`parseBooleanCapabilityOverride` 接受 `"1"/"0"/"auto"`；`KIANA_IMAGE_PROTOCOL` 小写化后与协议名比较（`kitty`、`iterm`、`none`…）。三者经 `settingsManager.getTerminalCapabilityOverrides()` 也可由设置覆盖（`setCapabilityOverrides`） |
| `KIANA_TUI_WRITE_LOG` | `kiana-tui/src/terminal.ts:177`（模块级 `writeLogPath` IIFE） | 非空时把 TUI 写出的原始字节记到文件。**若是已存在的目录**，文件名生成 `` `tui-${YYYY}-${MM}-${DD}_${HH}-${mm}-${ss}-${pid}.log` ``（各段 `padStart(2,"0")`）；否则当作文件路径原样使用。路径在**模块加载时**计算一次 |
| `KIANA_TELEMETRY` | `core/telemetry.ts:10` `isInstallTelemetryEnabled` | 优先级最高：`telemetryEnv !== undefined ? isTruthyEnvFlag(telemetryEnv) : settings.getEnableInstallTelemetry()`。`isTruthyEnvFlag` = `"1"` 或小写 `"true"`/`"yes"` |
| `KIANA_SKIP_VERSION_CHECK` | `utils/version-check.ts:98` | 任何非空值 → `checkForNewKianaVersion` 直接返回 `undefined`（`--offline` 也会把它设为 `"1"`，见 `main.ts:581-582`） |
| `KIANA_CODING_AGENT` | `rpc-entry.ts:7`、`cli/setup.ts:6` | 设为 `"true"`，标记"运行在 coding agent 内" |
| `KIANA_PACKAGE_DIR` | `config.ts:395` | 包根目录覆盖（影响 `getShortPath` 的相对化基准） |
| `KIANA_EXPERIMENTAL` | `core/experimental.ts:2` | `=== "1"` 时启用实验特性 |
| `KIANA_TIMING` | `core/timings.ts:6`（模块加载时常量 `ENABLED`） | `=== "1"` 时打印计时 |
| `KIANA_STARTUP_BENCHMARK` | `main.ts:934` | `isTruthyEnvFlag` → 启动基准模式 |
| `KIANA_CODEMODE_MAX_WORKERS` | `extensions/codemode/execute.ts:62` | `Number(...)`，codemode worker 上限（仅在 codemode 扩展启用时） |

> 注意区分：`KIANA_HARDWARE_CURSOR`/`KIANA_CLEAR_ON_SHRINK` 是 **`=== "1"` 严格比较**，而 `KIANA_OFFLINE`/`KIANA_TELEMETRY`/`KIANA_STARTUP_BENCHMARK` 走 `isTruthyEnvFlag`（`1`/`true`/`yes`）。Rust 里不能用同一个 helper 了事。

### 4.2 磁盘文件

#### 4.2.1 `<agent-dir>/<APP_NAME>-debug.log`

`/debug` 命令（`handleDebugCommand`）**直接写入**该路径。路径来自 `config.ts:getDebugLogPath()`，**逐字**为 `` join(getAgentDir(), `${APP_NAME}-debug.log`) ``；`APP_NAME` 默认 `"kiana"`（`pkg.kianaConfig?.name` 存在时改用该名），因此默认文件名是 **`kiana-debug.log`**，但派生产品会变成 `<name>-debug.log`——移植时**不可硬编码** `kiana-debug.log`。

文件内容是单个字符串（`array.join("\n")` 后 `writeFileSync`），**逐行格式**：

```
Debug output at ${new Date().toISOString()}
Terminal: ${width}x${height}
Total lines: ${allLines.length}
(空行)
=== All rendered lines with visible widths ===
[idx] (w=${visibleWidth(line)}) ${JSON.stringify(line)}      ← 每个渲染行一条
(空行)
=== Agent messages (JSONL) ===
${JSON.stringify(msg)}                                       ← session.messages 每项一条
(空行)
```

其中 `width/height` 来自 `ui.terminal.columns/rows`，`allLines = ui.render(width)`。写文件前 `fs.mkdirSync(path.dirname(debugLogPath), { recursive: true })`。

> **勘误**：本节旧文写"其后为 `formatDiagnostics()` 的输出"是**错的**——`formatDiagnostics` 只用于资源加载诊断（§2.50.21），与 `/debug` 无关。

命令在 chat 中回显的是 **两行**：`✓ Debug log written`（`accent` 色）+ 日志路径（`muted` 色），包在 `ThemedText(..., 1, 1)` 里（`paddingX=1, paddingY=1`），前面有 `Spacer(1)`。**不是** `Debug output at <ISO>`。

#### 4.2.2 `<agent-dir>/crashes.json`（崩溃日志，本单元读写）

JSON 数组，格式 `JSON.stringify(records, null, 2)` + 末尾换行。单条记录：

```ts
interface CrashRecord {
  timestamp: string;                                   // new Date().toISOString()
  version: string;                                     // VERSION，即 "1.4.0"
  kind: "uncaught_exception" | "fatal_error";
  message: string;                                     // error instanceof Error ? (error.message || error.name) : String(error)
  stack: string | null;                                // Error 且有 stack → stack，否则 null
  sessionFile: string | null;
  cwd: string;
  notified?: boolean;
}
```

- `MAX_CRASH_RECORDS = 5`：写入时 `[...old, record].slice(-5)`。
- `MAX_AGE = 7 * 24 * 60 * 60 * 1000`（7 天）。
- `readCrashLog` 容错：非数组 → `[]`；数组元素须为对象且 `timestamp`、`message` 为字符串。
- `takeUnnotifiedCrash(path, now)`：从**末尾向前**找第一条 `!notified` 且 `now - Date.parse(timestamp) <= MAX_AGE` 的记录；找到则把所有未 `notified` 的记录标记 `notified: true` 并写回；写回失败可忽略（会重复提示）。
- `clearCrashLog`：`rmSync(path, { force: true })`，异常吞掉。

#### 4.2.3 `<agent-dir>/keybindings.json`

```json
{ "$schema": "...", "<keybinding-id>": "<key-id>" | ["<key-id>", "..."] }
```

加载规则（`core/keybindings.ts`，本单元消费）：
1. 文件不存在或 JSON 解析失败或非对象 → `{}`（静默）。
2. `migrateKeybindingsConfig`：把旧名（`cursorUp`、`submit`、`interrupt`、`treeFoldOrUp` 等 51 个）迁移到新 id；若新旧同名同时存在，**保留新名并丢弃旧名**。
3. `orderKeybindingsConfig`：先按 `KEYBINDINGS` 声明顺序输出已存在的 key，再把其余 key **按字典序**追加。
4. `toKeybindingsConfig`：跳过 `"$schema"`；值必须通过 `KeybindingValueSchema`（字符串或字符串数组，每项匹配正则
   `^(?!.*(?:ctrl\+.*ctrl\+|shift\+.*shift\+|alt\+.*alt\+|super\+.*super\+))(?:(?:ctrl|shift|alt|super)\+){0,4}(?:[a-z0-9]|<Key 枚举值转义>)$`），否则**该条丢弃**。

#### 4.2.4 `<agent-dir>/mcp.json`

`/login <provider>` 完成后若 provider 是 Radius，会询问是否把 Radius MCP 服务器写入全局 `mcp.json`（`offerRadiusMcpServer`）：
- 目标 URL：`RADIUS_MCP_URL = `${normalizeRadiusGatewayUrl(DEFAULT_RADIUS_GATEWAY)}/mcp``（默认 `https://radius.kiana.dev/mcp`）。
- 比较 URL 前会去掉末尾 `/`（`normalizeUrl`）。
- 若已存在同 URL 且 `auth.provider === providerId` → **不再询问**。
- 命名：沿用已有项的名字；若不存在且 `"radius"` 已被别的服务器占用 → 用 `"radius-mcp"`。
- 写入的配置：`existing ? {...existing.config, auth:{provider}} : {url: RADIUS_MCP_URL, auth:{provider}}`，并 `delete config.oauth`。
- 询问对话框标题逐字：`` `Configure ${providerName} MCP in ${mcpPath}?` ``，选项 `["Yes", "No"]`。
- 写入失败：`` `Could not update ${mcpPath}: ${message}` ``。
- 写入成功后 `void this.handleReloadCommand()`（MCP 扩展在会话启动时读 `mcp.json`）。
- 文件格式由 `addMcpServerConfig(path, name, config)` 维护：保留原有缩进与其它字段，只改 `mcpServers` 段。

#### 4.2.5 `<agent-dir>/settings.json`（本单元只读字段 + 写回）

本单元通过 `SettingsManager` 读写；与本单元相关的字段与默认值（`SETTINGS_DEFAULTS`）：

```jsonc
{
  "transport": "auto",                       // "sse" | "websocket" | "websocket-cached" | "auto"
  "sessionShareDestination": "auto",         // "auto" | "gist" | "radius"
  "steeringMode": "one-at-a-time",           // "one-at-a-time" | "all"
  "followUpMode": "one-at-a-time",           // "one-at-a-time" | "all"
  "compaction": { "enabled": true, "reserveTokens": 16384, "keepRecentTokens": 20000 },
  "branchSummary": { "reserveTokens": 16384, "skipPrompt": false },
  "retry": { "enabled": true, "maxRetries": 3, "baseDelayMs": 2000,
             "maxAgentDelayMs": <DEFAULT_MAX_AGENT_RETRY_DELAY_MS>,
             "provider": { "maxRetryDelayMs": 60000 } },
  "hideThinkingBlock": false,
  "showCacheMissNotices": false,
  "quietStartup": false,                     // false | true | "header"
  "defaultProjectTrust": "ask",              // "ask" | "always" | "never"
  "collapseChangelog": false,
  "enableInstallTelemetry": true,
  "enableAnalytics": false,
  "enableSkillCommands": true,
  "terminal": { "showImages": true, "imageWidthCells": 60, "clearOnShrink": false, "showTerminalProgress": false },
  "images": { "autoResize": true, "blockImages": false },
  "doubleEscapeAction": "tree",              // "fork" | "tree" | "none"
  "treeFilterMode": "default",               // "default" | "no-tools" | "user-only" | "labeled-only" | "all"
  "editorPaddingX": 0,
  "outputPad": 1,
  "autocompleteMaxVisible": 5,
  "markdown": { "codeBlockIndent": "  ", "mermaid": "streaming" },   // "off" | "final" | "streaming"
  "warnings": { "anthropicExtraUsage": true },
  "codemode": { "mode": "on", "inlineBudget": 3000 },
  "cacheWarming": "streaming",
  "tuiMode": "fullscreen",                   // "regular" | "fullscreen"
  "fullscreenExitOutput": "transcript",      // "transcript" | "resume-hint"
  "fullscreenScrollbar": "auto",             // "auto" | "always" | "hidden"
  "fullscreenCopyOnSelect": true,
  "fullscreenWheelScrollLines": "auto"       // "auto" | 数字
}
```

`HTTP_IDLE_TIMEOUT_CHOICES`（**逐字**，用于 `http-idle-timeout` 设置项的显示值与解析）：
```
{ label: "30 sec",   timeoutMs: 30_000 }
{ label: "1 min",    timeoutMs: 60_000 }
{ label: "2 min",    timeoutMs: 120_000 }
{ label: "5 min",    timeoutMs: 300_000 }
{ label: "disabled", timeoutMs: 0 }
```
`formatHttpIdleTimeoutMs(ms)`：命中选项返回其 label；否则 `` `${ms / 1000} sec` ``（注意不是整数化，`45000` → `"45 sec"`）。
`parseHttpIdleTimeoutMs`：字符串 `"disabled"`（大小写不敏感、trim 后）→ `0`；空串 → `undefined`；否则按数字递归。

#### 4.2.6 主题文件 `<agent-dir>/themes/*.json`

见 §2.5（schema）与 §2.6 + 附录 A（内置两套）。自定义主题从 `getCustomThemesDir()` = `join(getAgentDir(), "themes")` 读取。

#### 4.2.7 临时文件

| 路径模板 | 用途 | 生命周期 |
|---|---|---|
| `<tmpdir>/kiana-editor-XXXXXX/prompt.md` | 外部编辑器（`editInExternalEditor`） | `finally` 中 `rmSync(directory, {recursive:true, force:true})` |
| `<tmpdir>/kiana-share-XXXXXX/session.jsonl` + `session.html` | `/share` 导出 | `finally` 中 `rmSync(tempDir, {recursive:true, force:true})` |
| `<tmpdir>/kiana-clipboard-<uuid>.<ext>` | 粘贴剪贴板图片时写入后把路径插入编辑器 | **不删除**（由用户/系统清理）；`ext` 来自 `extensionForImageMimeType`，未知时 `"png"` |
| `<tmpdir>/kiana-wsl-clip-<uuid>.txt` / `.png` | WSL 剪贴板桥 | 由 clipboard 工具清理 |
| `<tmpdir>/<prefix>-<16位hex><ext>`、mode `0o600`、flag `"wx"` | 工具输出文件（`utils/output-files.ts`） | 长期保留，供模型引用 |
| `<cwd>/kiana-bug-report-<id>.zip` | `/bug` 的“导出为 Zip” | 长期保留；文件名 `bugReportArchiveFileName(id) = `kiana-bug-report-${id}.zip`` |

### 4.3 HTTP 请求

#### 4.3.1 安装遥测

```
POST? 否 —— GET
URL:  https://kiana.dev/api/report-install?version=<encodeURIComponent(version)>
请求头: User-Agent: getKianaUserAgent(version)      // formatUserAgent({userAgent:"detailed", version})
超时: AbortSignal.timeout(5000)
返回: 忽略（.then(()=>undefined).catch(()=>undefined)）
前置: 若 process.env.KIANA_OFFLINE 为真 → 不请求；若 !isInstallTelemetryEnabled(settingsManager) → 不请求
```

#### 4.3.2 Radius 分享上传

```
URL:  new URL("/v1/artifacts", DEFAULT_RADIUS_GATEWAY)      // https://radius.kiana.dev/v1/artifacts
查询: visibility=organization, title=Kiana session            // 均为 searchParams.set，按此顺序
方法: POST
头:   Authorization: Bearer <token>
      Content-Type: application/x-ndjson
      Content-Length: <body.byteLength>
体:   session.jsonl 的原始字节
信号: loader.signal（BorderedLoader 的 abort）
响应: JSON { artifact?: { canonical_url: string }, error?: string }
错误: `Failed to upload Radius artifact: ${json?.error || response.statusText || response.status}`
成功: showStatus(`Share URL: ${hyperlink(shareUrl, shareUrl)}`)
```

#### 4.3.3 Radius 缺陷报告上传

```
URL:  new URL("/v1/bug-reports", options.gatewayUrl ?? getRadiusGatewayUrl())
方法: POST
头:   有 token 时 Authorization: Bearer <token>
体:   multipart/form-data；每个文件 append(file.name, new Blob([file.data], {type: file.contentType}), file.name)
响应: { ok?: boolean, bug_report?: { id?: string }, error?: string, description?: string }
成功条件: response.ok && json.ok === true && typeof json.bug_report.id === "string"
错误串: `Bug report upload failed: ${detail || response.statusText || response.status}`
        其中 detail = (json && !json.ok) ? (json.description || json.error) : undefined
```

### 4.4 子进程调用

| 命令 | 参数 | 用途 | 失败处理 |
|---|---|---|---|
| `gh` | `auth status` | `/share` 前检查登录（`spawnSync`，`encoding: "utf-8"`） | `status !== 0` → `GitHub CLI is not logged in. Run 'gh auth login' first.`；抛异常（ENOENT）→ `GitHub CLI (gh) is not installed. Install it from https://cli.github.com/` |
| `gh` | `gist create --public=false <file>` | 创建私有 gist（`spawn`，监听 stdout/stderr/close） | `code !== 0` → `Failed to create gist: ${stderr.trim() || "Unknown error"}` |
| `trash` | `<path>`，若路径以 `-` 开头则 `-- <path>` | 删除会话文件（`spawnSync`） | 见 §2.43；失败回退 `fs.unlink`，错误串 `` `${unlinkError} (${trashErrorHint})` ``，`trashErrorHint = `trash: ${parts.join(" · ").slice(0,200)}`` |
| `$VISUAL` / `$EDITOR` / 平台默认 | `<editorArgs...> <filePath>` | 外部编辑器（`spawn`，`stdio: "inherit"`，win32 用 `shell: true`） | 退出码非 0 或 spawn error → `{status:"failed"}` |

`gh` 的 `stdout` 被视为 gist URL，`gistId = gistUrl.split("/").pop()`；为空 → `Failed to parse gist ID from gh output`。

### 4.5 终端控制序列（本单元产生）

| 序列 | 逐字 | 位置 |
|---|---|---|
| OSC 8 超链接（BEL 结尾） | `\x1b]8;;<url>\x07<text>\x1b]8;;\x07` | `login-dialog.showDeviceCode`、`showInfo`、`auth-url.ts`、`hyperlink()` |
| OSC 8 超链接（ST 结尾） | `\x1b]8;;<url>\x1b\\<text>\x1b]8;;\x1b\\` | **本单元不产生**；仅由 kiana-tui 的 `hyperlink()` 决定。`auth-url.ts` 里**没有** `renderWithHyperlink`，`login-dialog.ts` 则是**手写 BEL 形式**（见 §4.5 说明） |
| OSC 133 提示符区标记 | `\x1b]133;A\x07` / `\x1b]133;B\x07` / `\x1b]133;C\x07` | 由 kiana-tui 的主屏渲染发出（本单元只触发渲染） |
| OSC 9;4 进度（进行中） | `\x1b]9;4;3\x07` | `terminal.ts` 的 `TERMINAL_PROGRESS_ACTIVE_SEQUENCE`；设置项 `terminal-progress` 控制 |
| OSC 9;4 进度（清除） | `\x1b]9;4;0\x07` | 同上 `TERMINAL_PROGRESS_CLEAR_SEQUENCE` |
| OSC 7501 程序状态查询 | `\x1b]7501;?\x1b\\` | kiana-tui 能力探测（`PROGRAM_STATUS_QUERY`） |
| OSC 7501 程序状态上报 | `\x1b]7501;state=<state>[:app=<app>][:kind=<kind>][:msg=<base64>]\x1b\\` | `ProgramStatusReporter` → `terminal.setProgramStatus` |

**OSC 7501 上报格式规则（逐字，`packages/tui/src/program-status.ts`）**：
```
pairs = [`state=${status.state}`]
若 status.app !== undefined 且 APP_PATTERN.test(app)     → pairs.push(`app=${app}`)
若 status.state === "blocked" 且 status.kind            → pairs.push(`kind=${kind}`)
message = truncateUtf8((status.message ?? "").replace(/[\u0000-\u001f\u007f-\u009f]+/g, " ").trim(), 2048)
若 message 非空                                          → pairs.push(`msg=${Buffer.from(message,"utf8").toString("base64")}`)
return `\x1b]7501;${pairs.join(":")}\x1b\\`
```
- `APP_PATTERN = /^[A-Za-z0-9_.+-]{1,32}$/`；不匹配则**省略** `app` 对（本单元恒传 `app = "kiana"`，必然匹配）。
- `MAX_MESSAGE_BYTES = 2048`（解码后字节数）；`truncateUtf8` **按码点**累加 UTF-8 字节，超限即停（不会切半个字符）。
- `kind` 仅允许 `"permission" | "question" | "auth"`。
- 能力探测：终端回复 `isProgramStatusReply(sequence)` = `/^\x1b\]7501;\?[^\x07\x1b]*(?:\x07|\x1b\\)$/`。

**本单元手写 OSC 8 的位置（逐字，绕过 `hyperlink()` 的两处）**：
- `login-dialog.ts:showDeviceCode`：`` `\x1b]8;;${info.verificationUri}\x07${info.verificationUri}\x1b]8;;\x07` ``（URL 文本）与 `` `\x1b]8;;${info.verificationUri}\x07${clickHint}\x1b]8;;\x07` ``（`Cmd+click to open` / `Ctrl+click to open`）。
- `login-dialog.ts:showInfo`：`` `\x1b]8;;${link.url}\x07${text}\x1b]8;;\x07` ``，`text = link.label ? `${link.label}: ${link.url}` : link.url`。
- 其余（`auth-url.ts`、`session-share`、`tool-execution` 的 `linkPath`）都调用 kiana-tui 的 `hyperlink(url, text?)`；该 helper 的终止符形式（BEL 或 ST）由 kiana-tui 决定，本单元**不自行拼接**。
- `hover` 提示：全屏模式下 OSC 8 链接优先于外层点击区域（见 `docs/tui.md`），本单元不额外处理。

### 4.6 剪贴板格式

- 文本：优先原生工具，回退 OSC 52（详见 tui 单元）。
- 图片：`readClipboardImage()` 返回 `{bytes, mimeType}`；粘贴时写入临时文件并把**绝对路径**插入编辑器。
- 复制确认提示：`Copied!`（**逐字**，见 §2.50.8）。

---

## 5. 跨单元接口（精确类型）

### 5.1 本单元对外暴露的符号（供 `main.ts` / 测试 / 扩展使用）

`modes/interactive/index.ts` 与 `interactive-mode.ts` 的导出：

```rust
// --- 入口 ---
pub struct InteractiveModeOptions { /* 见 §2.50.2 */ }
pub struct InteractiveMode { /* ... */ }
impl InteractiveMode {
    pub fn new(/* session runtime, ui, ... */) -> Self;
    pub async fn run(&mut self) -> Result<()>;
    pub async fn shutdown(&mut self);
    pub fn create_extension_ui_context(&mut self) -> ExtensionUiContext;
}

// 由 tui-renderer.ts 重导出
pub use crate::modes::interactive::tui_renderer::{create_interactive_tui, create_interactive_tui_reference};
pub fn create_interactive_tui(/* ... */) -> Result<InteractiveTui>;
pub fn create_interactive_tui_reference(/* ... */) -> InteractiveTuiReference;
```

### 5.2 主题子系统（`theme/theme.ts` 导出，被本单元与 `ca-cli` 的 export 复用）

```rust
pub const SYSTEM_THEME_NAME: &str;              // "system"
pub static THEME_COLOR_NAMES: [&str; 56];       // §2.1 顺序
pub struct Theme { pub name: String, /* colors: 56 项已解析 RGB + 可选 token */ }
impl Theme {
    pub fn fg(&self, color: ThemeColor, text: &str) -> String;
    pub fn bg(&self, color: ThemeColor, text: &str) -> String;
    pub fn bold(&self, text: &str) -> String;
    pub fn dim(&self, text: &str) -> String;
    pub fn italic(&self, text: &str) -> String;
    pub fn underline(&self, text: &str) -> String;
    pub fn strikethrough(&self, text: &str) -> String;
    pub fn get_color_mode(&self) -> ColorMode;
}
pub fn theme() -> &'static Theme;                        // 当前全局主题（进程内单例）
pub fn get_theme_by_name(name: &str) -> Option<Theme>;
pub fn get_available_themes() -> Vec<String>;
pub fn get_available_themes_with_paths() -> Vec<ThemeInfo>;
pub fn set_registered_themes(themes: Vec<ThemeDefinition>);
pub fn on_theme_change(cb: impl Fn(&str) + Send + Sync + 'static) -> Subscription;
pub fn stop_theme_watcher();
pub fn get_markdown_theme() -> MarkdownTheme;
pub fn get_editor_theme() -> EditorTheme;
pub fn get_select_list_theme() -> SelectListTheme;
pub fn get_settings_list_theme() -> SettingsListTheme;
pub fn parse_auto_theme_setting(value: &str) -> Option<AutoTheme>;   // "light/dark"
pub struct AutoTheme { pub light_theme: String, pub dark_theme: String }
```

`theme/theme-controller.ts`：
```rust
pub struct InteractiveThemeController { /* ... */ }
impl InteractiveThemeController {
    pub fn new(/* theme setting getter, terminal theme detector */) -> Self;
    pub fn apply(&mut self, setting: &str) -> Result<()>;
    pub fn on_terminal_theme_change(&mut self, theme: TerminalTheme);
    pub fn dispose(&mut self);
}
pub struct TerminalTheme { /* "light" | "dark" */ }
```

`theme/system-theme.ts`：`pub fn generate_system_theme(colors: &TerminalColors, appearance: Appearance) -> ThemeDefinition;`
`theme/theme-schema.ts`：`pub const THEME_SCHEMA_JSON: &str;`（`$schema` 指向 `https://kiana.dev/schemas/theme.schema.json`）
`theme/theme-tokens.ts`：`pub const THEME_TOKENS: [ThemeToken; 56]`（含 name/group/可选标记）

### 5.3 组件对外类型

```rust
// tool-execution.ts
pub use crate::core::extensions::types::ToolRenderers;
pub struct ToolExecutionOptions { pub show_images: Option<bool>, pub image_width_cells: Option<u32>, pub output_pad: Option<u32> }
pub struct ToolExecutionComponent { /* 见 §2.51 */ }

// model-selector.ts / scoped-models-selector.ts
pub struct ModelSelectorComponent;
pub struct ScopedModelsSelectorComponent;
pub struct ModelsConfig { pub all_models: Vec<Model>, pub enabled_model_ids: Option<Vec<String>>, pub refresh_status: Option<String> }
pub struct ModelsCallbacks {
    pub on_change: Box<dyn Fn(Option<Vec<String>>) -> BoxFuture<'static, ()> + Send + Sync>,
    pub on_persist: Box<dyn Fn(Option<Vec<String>>) -> BoxFuture<'static, ()> + Send + Sync>,
    pub on_cancel: Box<dyn Fn() + Send + Sync>,
}

// settings-selector.ts
pub struct SettingsConfig { /* §2.54 全字段，snake_case */ }
pub struct SettingsCallbacks { /* §2.54 全字段 */ }
pub struct SettingsSelectorComponent;

// settings-submenu.ts
pub struct SelectSubmenuOptions { pub searchable: Option<bool>, pub layout: Option<SelectListLayoutOptions> }
pub struct SteppedSubmenuStep {
    pub key: String,
    pub title: StepText,                 // enum { Fixed(String), Fn(Arc<dyn Fn(&Context)->String>) }
    pub description: StepText,
    pub options: Arc<dyn Fn(&Context) -> Vec<SelectItem>>,
    pub preselect: Option<Arc<dyn Fn(&Context) -> Option<String>>>,
    pub searchable: Option<bool>,
    pub layout: Option<SelectListLayoutOptions>,
}
pub type Context = std::collections::BTreeMap<String, String>;   // 注意：TS 是普通对象，插入序；见下注

// oauth-selector.ts
pub struct AuthSelectorProvider {
    pub id: String, pub name: String, pub auth_type: AuthType,
    pub method: Option<AuthMethod>, pub status: Option<AuthCheck>, pub subscription: Option<bool>,
}
pub fn format_auth_selector_provider_type(auth_type: AuthType, subscription: Option<bool>) -> String;
pub fn format_auth_selector_provider_status(provider: &AuthSelectorProvider) -> String;

// login-dialog.ts
pub struct LoginDialogComponent;
impl LoginDialogComponent {
    pub fn signal(&self) -> AbortSignal;
    pub fn show_auth(&mut self, url: &str, instructions: Option<&str>);
    pub fn show_device_code(&mut self, info: OAuthDeviceCodeInfo);
    pub fn show_manual_input(&mut self, prompt: &str) -> impl Future<Output = Result<String>>;
    pub fn show_prompt(&mut self, message: &str, placeholder: Option<&str>) -> impl Future<Output = Result<String>>;
    pub fn show_details(&mut self, lines: &[String]);
    pub fn show_info(&mut self, message: &str, links: &[AuthInfoLink], show_close_hint: bool);
    pub fn show_waiting(&mut self, message: &str);
    pub fn show_progress(&mut self, message: &str);
}

// radius-login-selector.ts
pub struct RadiusOption { pub label: String, pub text: String }
pub fn create_login_menu_selector(/* tui, title, options, radius_option, on_select, on_cancel */) -> ExtensionSelectorComponent;

// session-selector-search.ts
pub enum SortMode { Threaded, Recent, Relevance }
pub enum NameFilter { All, Named }
pub struct ParsedSearchQuery { pub mode: SearchMode, pub tokens: Vec<SearchToken>, pub regex: Option<Regex>, pub error: Option<String> }
pub enum SearchMode { Tokens, Regex }
pub enum SearchToken { Fuzzy(String), Phrase(String) }
pub struct MatchResult { pub matches: bool, pub score: f64 }
pub fn has_session_name(session: &SessionInfo) -> bool;
pub fn parse_search_query(query: &str) -> ParsedSearchQuery;
pub fn match_session(session: &SessionInfo, parsed: &ParsedSearchQuery) -> MatchResult;
pub fn filter_and_sort_sessions(sessions: Vec<SessionInfo>, query: &str, sort_mode: SortMode, name_filter: Option<NameFilter>) -> Vec<SessionInfo>;
```

> **注（`SteppedSubmenu` 的 context 顺序）**：TS 用普通对象，键按插入顺序；`{...context}` 展开保留插入顺序。Rust 若用 `BTreeMap` 会改变顺序，但该顺序**只**在 `title`/`description`/`options` 回调里被读取（均按 key 查找），不参与输出，故可用 `IndexMap` 或 `HashMap`；**建议用 `indexmap::IndexMap`** 以贴近语义。

### 5.4 依赖的外部单元契约（本单元**要求**对方提供）

| 单元 | 必须提供的类型/函数 | 精度要求 |
|---|---|---|
| `tui-core` | `TUI` trait（`request_render`、`set_focus`、`get_width`、`get_height`、`set_program_status`、`add_overlay`…）、`TuiMainScreen`、`TuiAltScreen`、`Component` trait（`render(width) -> Vec<String>`、`handle_input(&str)`、`invalidate`、`handle_mouse`、`dispose`）、`Container`、`Box`、`Spacer`、`Text`、`TruncatedText`、`Loader`、`ScrollView`、`MouseRegion`、`Input`、`Editor`、`SelectList`、`SettingsList`、`Markdown`、`Image`、`Key`、`matches_key`、`get_keybindings`/`set_keybindings`、`get_capabilities`/`set_capability_overrides`、`visible_width`/`truncate_to_width`/`slice_by_column`、`wrap_text_with_ansi`、`hyperlink`、`fuzzy_filter`/`fuzzy_match`、颜色函数族、`ProgramStatus` | **`fuzzy_match` 的 score 值**被 §2.59 用作排序依据，必须与 TS 实现**数值一致**（不是仅“相对次序一致”）；`truncate_to_width`/`visible_width` 必须按终端单元格（East Asian Width）计算 |
| `tui-components` | `SelectListTheme`、`SettingsListTheme`、`SettingItem`、`SelectItem`、`SelectListLayoutOptions`、`AutocompleteProvider`、`SlashCommand`、`MarkdownTheme`、`EditorTheme` | `SettingsList` 的 `enableSearch` 行为、子菜单进出语义需与 TS 一致 |
| `tui-text` | `Markdown` 渲染器、`strikethrough`、ANSI 样式 | 见 tui-text 单元 |
| `ca-session` | `AgentSession`（含 `state`、`settingsManager`、`modelRuntime`、`sessionManager`、`isBashRunning`、`summarizeForBugReport`、`exportToHtml` 等）、`AgentSessionEvent` 全部变体、`SessionInfo`（含 `allMessagesText`、`modified`、`name`、`cwd`、`id`、`messageCount`…）、`SessionManager`、`SessionEntry`、`FooterDataProvider`、`crash-log`（`readCrashLog`/`recordCrash`/`takeUnnotifiedCrash`/`clearCrashLog`/`findExtensionStackMatches`）、`keybindings`（`KeybindingsManager`、`KEYBINDINGS`、`KEYBINDING_NAME_MIGRATIONS`）、`slash-commands`、`model-runtime`（`getAvailableSnapshot`、`getModel`、`refresh`、`getError`、`getProvider`、`getAuth`）、`usage-totals`、`cache-stats`、`cache-warmer`、`http-dispatcher`（`HTTP_IDLE_TIMEOUT_CHOICES`、`formatHttpIdleTimeoutMs`、`configureHttpDispatcher`）、`radius`（`RADIUS_MCP_URL`、`RADIUS_PROVIDER_ID`、`getRadiusGatewayUrl`）、`trust-manager`、`session-export`、`bug-report`（`bugReportArchiveFileName`、`collectBugReportMetadata`、`collectBugReportDiagnostics`、`writeBugReportArchive`、`bugReportFiles`、`BUG_REPORT_CUSTOM_ENTRY_TYPE`）、`bug-report-upload`、`session-cwd` | `AgentSessionEvent` 的**字段名与可空性**逐字（§2.50.5）；`SessionInfo.allMessagesText` 是搜索性能关键 |
| `ca-config-models` | `SettingsManager` 全部 `get*/set*`、`SETTINGS_DEFAULTS` | 枚举字面量逐字（§4.2.5） |
| `ca-resources` | `ResourceLoader`、`ExtensionRunner`（`getExtensions`、`getCommand`、`getMessageRenderer`、`getEntryRenderer`、`getMarkdownTransformers`、`emit` 等）、`ExtensionUIContext`、`ToolRenderers`、`EditorFactory`、`WorkingIndicatorOptions` | 扩展 UI 上下文的**每个方法签名**见 §2.50.18/§2.50.20 |
| `ca-tools-compaction-export` | `formatToolCallWithArgs`、`getTextOutput`、`formatToolDuration`、`truncateTail` | 逐字（§2.51 已内联） |
| `ca-cli` | `APP_NAME`/`APP_TITLE`/`CONFIG_DIR_NAME`/`VERSION`、`getAgentDir`/`getAuthPath`/`getDebugLogPath`/`getDocsPath`/`getShareViewerUrl`、`getAuthCredential`、`utils::clipboard`（`copyToClipboard`、`readClipboardText`、`readClipboardFilePaths`）、`utils::clipboard_image`（`readClipboardImage`、`extensionForImageMimeType`）、`openBrowser`、`utils::ansi`、`utils::text`（`stripBom`、`stripAnsi`）、`utils::paths`、`utils::git`（`parseGitUrl`）、`utils::syntax_highlight`（`loadAllHighlightLanguages`）、`utils::changelog`、`utils::tools_manager`、`utils::version_check`、`utils::image_convert`（`ensurePngTranscoder`）、`utils::abort`（`raceWithAbortSignal`）、`utils::shell`（`killTrackedDetachedChildren`、`sanitizeBinaryOutput`）、`utils::kiana_user_agent` | `getDebugLogPath()` 必须返回 `<agent-dir>/kiana-debug.log` |
| `ai-core` / `ai-api-*` / `ai-auth` | `Model`、`modelsAreEqual`、`getSupportedThinkingLevels`、`isRetryableAssistantError`、`AssistantMessage`/`Message`/`Usage`、`AuthEvent`/`AuthPrompt`/`AuthInfoLink`/`OAuthDeviceCodeInfo`/`AuthCheck`/`ApiKeyAuth`/`OAuthAuth`、`Transport`、`ModelsRefreshResult`、`DEFAULT_RADIUS_GATEWAY`、`formatUserAgent` | `modelsAreEqual` 的比较语义（provider+id）必须一致 |
| `agent-codemode` | `ThinkingLevel`、`AgentMessage` | 枚举值逐字：`"off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"` |
| `telemetry-mcp` | `McpHttpServerConfig`、`addMcpServerConfig`、`loadMcpConfig` | `addMcpServerConfig` 需保留原文件缩进 |

### 5.5 本单元提供给 `ca-cli` 的复用点

- `createInteractiveTui` / `createInteractiveTuiReference`（`tui-renderer.ts` 重导出）——`main.ts` 据此创建 TUI。
- `theme/theme.ts` 的全套访问器——`/export` 的 HTML 模板与 `cli/` 下的主题列举都读同一份主题。
- `components/keybinding-hints.ts` 的 `formatKeyText` / `keyText` / `keyDisplayText` / `keyHint` / `rawKeyHint`——`/hotkeys` 与其它帮助文本共用。

### 5.6 事件与回调方向（谁调用谁）

```
AgentSession ──emit(AgentSessionEvent)──▶ InteractiveMode.handleEvent
                                       ├─▶ ProgramStatusReporter.handleEvent
                                       ├─▶ FooterDataProvider / 组件更新
                                       └─▶ ExtensionRunner.emit(...)

ExtensionRunner ──UI 请求──▶ ExtensionUIContext（本单元实现）
                            ├─ showSelector / showInput / showEditor / showCustom
                            ├─ setStatus / setWidget / setHeader / setFooter / setEditorComponent
                            ├─ setWorkingIndicator / setHiddenThinkingLabel
                            ├─ addMarkdownTransformer / registerShortcuts / onTerminalInput
                            └─ dialogs 栈（push/remove/showTop）

ThemeController ──onThemeChange──▶ InteractiveMode（重建 Markdown/Editor 主题、重绘）
Terminal ──终端主题变化──▶ ThemeController（自动模式切换 light/dark）
```

---

## 6. npm 依赖与 Rust crate 对应

### 6.1 本单元直接 import 的 npm 包

| npm 包 | 版本 | 用在本单元的哪里 | 说明 |
|---|---|---|---|
| `@earendil-works/kiana-tui` | 1.4.0 | 全部 UI 组件、颜色、模糊匹配、Markdown、键位、能力探测 | 见 `tui-core`/`tui-components`/`tui-text` 单元 |
| `@earendil-works/kiana-ai` | 1.4.0 | `Model`、`modelsAreEqual`、`getSupportedThinkingLevels`、`AuthCheck`/`ApiKeyAuth`/`OAuthAuth`/`AuthInfoLink`/`OAuthDeviceCodeInfo`、`Transport`、`ModelsRefreshResult` | 同时通过 `kiana-ai/compat` 取 `AssistantMessage`/`Message`/`Usage`/`isRetryableAssistantError` |
| `@earendil-works/kiana-ai/providers/radius-config` | 1.4.0 | `DEFAULT_RADIUS_GATEWAY` | 常量 `"https://radius.kiana.dev"` |
| `@earendil-works/kiana-agent-core` | 1.4.0 | `ThinkingLevel`、`AgentMessage` | 枚举 |
| `chalk` | 6.0.0 | `interactive-mode.ts`（`chalk.dim` 等少量直接调用） | 其余颜色走 `theme` |
| `diff` | 8.0.4 | `components/diff.ts`（`diffLines`/`diffWords` 等） | 见 §2.28 |
| `grok-mermaid` | 0.2.3 | `components/mermaid.ts`（Mermaid → Unicode 图） | 见 §2.38 |
| `highlight.js` | 10.7.3 | 经 `utils/syntax-highlight.ts`（`loadAllHighlightLanguages`） | 实现归 `kiana-core::syntax_highlight`（规格在 ca-cli 单元 §2.21.35） |
| `@silvia-odwyer/photon-node` | 0.3.4 | 经 `utils/image-convert.ts`（`ensurePngTranscoder`） | 由 ca-cli 单元提供 |
| `node:*`（内置） | — | `fs`、`os`、`path`、`crypto`、`child_process`、`url`、`util` | 见 6.2 |

本单元**不**直接 import：`yaml`、`semver`、`proper-lockfile`、`cross-spawn`、`hosted-git-info`、`ignore`、`minimatch`、`jiti`、`quickjs-wasi`、`typebox`、`undici`、`brace-expansion`、`jest` 等（这些属于其它单元）。

### 6.2 Node 内置模块 → Rust

| Node API | 本单元用法 | Rust 方案 |
|---|---|---|
| `fs.mkdtempSync` / `writeFileSync` / `readFileSync` / `rmSync` / `existsSync` | 外部编辑器、分享临时目录、崩溃日志、`mcp.json` | `tempfile::TempDir`、`std::fs`、`tokio::fs` |
| `os.tmpdir()` / `os.homedir()` | 临时文件、`~` 展开 | `std::env::temp_dir()`、`dirs::home_dir()`（`home` crate 亦可；必须与 Node 在 win32 用 `USERPROFILE` 的行为一致） |
| `path.join` / `path.posix` / `pathToFileURL` | 路径拼接、`getCompactPathLabel`、超链接 | `std::path::PathBuf`（**注意 win32 反斜杠**）；`getCompactPathLabel` 必须在 `/` 上分段，需先把 `\` 规范化为 `/` |
| `crypto.randomUUID()` | `/share` 的 trailing entry id 截取 8 字符、剪贴板图片文件名 | `uuid::Uuid::new_v4().to_string()`（格式一致：36 字符带连字符） |
| `child_process.spawn` / `spawnSync` | `gh`、`trash`、外部编辑器 | `tokio::process::Command` / `std::process::Command`；`spawnSync` 对应同步 `Command::output()` |
| `performance.now()` | Radius 动画、3D 动画 | `std::time::Instant`（单调时钟；语义等价） |
| `setInterval` / `setTimeout` + `.unref?.()` | Loader 动画、Radius shimmer、重试倒计时 | `tokio::time::interval` / `sleep`；Rust 无 `unref` 概念，spawn 的任务在运行时关闭时自然取消 |
| `AbortController` / `AbortSignal` | 模型刷新超时、登录取消、分享上传 | `tokio_util::sync::CancellationToken` + 每个选择器自持 token；`AbortSignal.timeout(5000)` → `tokio::time::timeout` |
| `fetch` | 安装遥测、Radius 上传 | `reqwest`（rustls-tls） |
| `URL` / `URLSearchParams` | Radius 上传 URL、device-code 协议校验 | `url::Url`（`query_pairs_mut().append_pair` 保持顺序） |
| `process.platform` | `win32` / `darwin` 分支（点击提示、外部编辑器 shell） | `cfg!(windows)` / `cfg!(target_os = "macos")` |
| `process.env` | 见 §4.1 | `std::env::var`（注意 Node 的 `undefined` vs Rust 的 `NotPresent`） |
| `Buffer.from(...).toString("base64")` / `Buffer.byteLength` | OSC 7501 消息编码/截断 | `base64::engine::general_purpose::STANDARD`（**带 `=` 填充**，与 Node 一致）；`str::len()`（UTF-8 字节） |
| `JSON.stringify(obj)` / `(obj, null, 2)` | 参数预览、崩溃日志、`lastReport` 去重键 | `serde_json` + `indexmap` 保序；**`JSON.stringify` 省略 `undefined` 字段**，Rust 需用 `skip_serializing_if = "Option::is_none"` 复刻 |
| `Intl`/`toLocaleString()` | `takeUnnotifiedCrash` 提示里的时间、footer 的 token 计数 | 见 6.3 |
| `Number.toFixed(1)` / `toFixed(2)` / `toFixed(3)` | `formatToolDuration`（`4.2s`）、成本显示、统计 | `format!("{:.1}", x)`；**边界值需逐例核对**（见 6.3） |

### 6.3 建议的 crate 清单

```toml
[package]
name = "kiana-interactive"
edition = "2021"

[dependencies]
# 异步与并发
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["rt"] }      # CancellationToken
futures = "0.3"

# 序列化
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }   # 复刻 JSON.stringify 的键序
indexmap = { version = "2", features = ["serde"] }              # SteppedSubmenu context / 有序 map

# 终端与文本
crossterm = "0.28"            # 或完全依赖自研 tui-core（推荐，见 tui 单元）
unicode-width = "0.2"         # 与 kiana-tui 的 visibleWidth 一致
unicode-segmentation = "1"    # 字素簇（AI 输出、markdown）
textwrap = "0.16"             # 仅作参考；换行必须自写以复刻 wrapTextWithAnsi

# 颜色
palette = "0.7"               # OKLab/OKLCH/OKHSL 转换（需核对与 kiana-tui 的公式一致性）
owo-colors = "4"              # ANSI 包装（真彩色/256/16 色降级需自写）

# 正则
regex = "1"                   # 大部分正则
fancy-regex = "0.14"          # 需要 lookaround/backreference 的少数（如 env 变量名正则其实不需要）

# 网络
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream", "multipart"] }
url = "2"

# 时间
chrono = { version = "0.4", features = ["clock"] }
time = "0.3"                  # 若需 ISO8601 严格输出（`toISOString()` 恒为 UTC + 毫秒 + `Z`）

# 文件与系统
tempfile = "3"
dirs = "5"
which = "7"
uuid = { version = "1", features = ["v4"] }
rand = "0.8"
base64 = "0.22"

# 外部依赖的移植点（由其它单元提供 crate）
# diff       → similar = "2"        （unified diff 生成需自写以逐字节对齐 `diff` 包输出）
# grok-mermaid → 无现成等价；需自写或移植（见 §8）
# highlight.js → syntect 不兼容，见 ca-cli 规范
```

### 6.4 需要桥接的 JS/Rust 行为差异（本单元特有）

| 差异 | 影响位置 | 桥接方式 |
|---|---|---|
| **UTF-16 长度 vs 字节长度** | `radiusShimmer` 用 `for...of`（码点）；`label.slice(text.length)` 用 **UTF-16 单元**下标 | Rust 用 `chars().count()` 遍历；`label.slice(text.length)` 必须按 **UTF-16 单元**切片，否则含 emoji（代理对）时切错位置。实现：`let units: Vec<u16> = label.encode_utf16().collect(); String::from_utf16_lossy(&units[text.encode_utf16().count()..])` |
| **`String.prototype.slice` 的参数语义** | `label.slice(text.length)`、`key.slice(3)`、`text.slice(1).trim()`、`pairs.slice(0, 97)` | JS 的 `slice` 对负数从末尾计；Rust `&s[a..b]` 按字节且会 panic。需写 `js_slice_utf16(s, start, end)` 辅助函数 |
| **`split(/\s+/)` 与 Unicode 空白** | `parseSearchQuery` 回退分词、`normalizeWhitespaceLower` | JS `\s` 含 ` `/`﻿`/各类 Unicode 空格；Rust `char::is_whitespace()` 集合略有差异（`​` 不算；` ` 算）。建议显式列出 JS `\s` 集合：`[\t\n\v\f\r    -     　﻿]` |
| **`String.prototype.toLowerCase()`** | 搜索、模型名去重 | Rust `str::to_lowercase()` 使用 Unicode 默认大小写映射，与 JS 在**土耳其语 İ** 等少数码点不同；对 ASCII 完全一致。`getDistinctModelName` 用 `/[^\p{L}\p{N}]+/u` 分词——Rust `regex` 支持 `\p{L}`/`\p{N}` |
| **`localeCompare`** | `sortModels`、`per-model thinking` 的模型排序 | Node 默认 ICU collation：ASCII 小写/大写/数字顺序与码点序**基本相同**，但 `"a" < "B"` 在 localeCompare 下为 true（忽略大小写差异），而码点序 `"B"(66) < "a"(97)`。**必须复刻**：用 `unicase` + `str::to_lowercase()` 比较，或调用 ICU4X。模型 id/provider 基本小写 ASCII，风险低但需在测试中固定 |
| **`JSON.stringify` 的 `undefined` 省略** | `ProgramStatusReporter.lastReport` 去重键、`formatToolCallWithArgs` 的 `JSON.stringify(value)`、`formatToolExecution` 的参数预览 | `serde_json` 默认会把 `Option::None` 序列化为 `null`；必须 `#[serde(skip_serializing_if = "Option::is_none")]` 并注意**顶层** `undefined` 时 `JSON.stringify` 返回 `undefined`（不是 `"null"`），`formatToolCallWithArgs` 里写作 `JSON.stringify(value) ?? String(value)` |
| **`JSON.stringify` 数字格式** | `JSON.stringify(0.1+0.2)`、duration 等 | JS 用最短往返（`1e-7` → `"1e-7"`，`1e21` → `"1e+21"`）；`serde_json` 用 Ryu，格式规则不同（`1e-7` → `1e-7` 相近，但 `1e21` → `1e21`）。若有数字进入 JSON 文本，需比对 |
| **`toFixed(1)`** | `formatToolDuration`：`"4.2s"` | JS `toFixed` 用“精确值的最近表示，正好一半时取**较大**的 n”（对正数即 half-up 于十进制表示）；Rust `format!("{:.1}")` 用 half-to-even 且有浮点表示误差。例如 `1.25.toFixed(1) === "1.3"`，Rust `format!("{:.1}", 1.25)` 给 `"1.2"`。**必须实现 JS toFixed 语义**（可用 `ryu`/`dtoa` 精确十进制展开后手写舍入） |
| **`toLocaleString()`** | 崩溃提示的时间、统计数字分组 | JS 在 `en-US` 下用 `,` 千分位；Rust 用 `icu_decimal` 或手写 `format_with_commas`。**注意 Node 默认 locale 来自 ICU 环境**，若用户 `LANG=de_DE` 会变 `1.234.567`——移植时固定用 `en-US` 并在文档标注差异 |
| **`Date.toLocaleString()`** | `/session`、footer 的时间显示 | Node 输出形如 `10/26/2026, 3:45:12 PM`（依赖 ICU + 时区）。Rust `chrono` 需手写同等格式并读取本地时区 |
| **`Date.toISOString()`** | 崩溃日志 `timestamp`、`/debug` 首行 | 恒为 UTC、毫秒 3 位、`Z` 结尾：`2026-10-26T08:12:34.567Z`。`chrono` 用 `Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ")` |
| **`Date.parse`** | `takeUnnotifiedCrash` 的 7 天窗口 | ISO8601 解析；`chrono::DateTime::parse_from_rfc3339` 接受，但 JS 还接受无时区串（按本地时区解释） |
| **`getTime()` 差值** | `filterAndSortSessions` 的 `modified` 降序 | 毫秒时间戳；用 `i64` 毫秒 |
| **正则 `\p{L}\p{N}` / `u` 标志** | `splitModelLabel` | Rust `regex` 的 Unicode 类默认开启，等价 |
| **正则 `lastIndex` 副作用** | `matchRegex` 的 `regex.test()` | 本单元正则恒定 `"i"` 无 `g`，无副作用；Rust `Regex::is_match` 无状态，安全 |
| **`String.prototype.includes`** | 各搜索函数 | `str::contains` 等价（按 UTF-8 字节匹配，语义等价于 UTF-16 子串） |
| **`Array.prototype.sort` 稳定性** | `sortModels`、`filterAndSortSessions`、`getSortedIds` | V8 的 `sort` 自 ES2019 起**稳定**；Rust `sort_by` 稳定，`sort_unstable_by` 不稳定。**必须用 `sort_by`** |
| **`Map` 的插入序** | `ProgramStatusReporter.blocked`、`defaultModelByValue` | `indexmap::IndexMap` |
| **`WeakMap` 缓存** | `session-selector-search.ts` 的 `messageTextCache` | `HashMap` + 显式失效（比对 `source` 字符串）或 `moka`；注意 `allMessagesText` 可能数 MB |
| **`Box`/`Container` 的 `children` 数组直接赋值** | `login-dialog.ts` 的 `replaceInputWithSubmittedText`、`radius-login-selector` 的 `lines[index] = ...`、`SelectSubmenu.applyFilter` 的 `children[i] = newList` | Rust 需提供可变 `children` 访问（`Vec<Box<dyn Component>>` 直接按索引替换） |
| **`Component.render` 返回 `string[]`（含 ANSI）** | 全部 | Rust 返回 `Vec<String>`，每行含转义序列；**切勿**在中间层 strip |
| **`\x1b[39m` 前景复位** | `radiusShimmer` 尾部 | 必须逐字输出；`owo-colors` 的 reset 常量不同，需手写 |
| **`chalk` 的 `dim`** | `interactive-mode.ts` 少量直接调用 | chalk `dim` 输出 `\x1b[2m...\x1b[22m`；`theme.fg("dim", …)` 是**颜色**（okhsl 灰）而非 `\x1b[2m`。两者含义不同，勿混用 |

### 6.5 打包与资源

- 内置主题 `dark.json` / `light.json` 必须以 `include_str!` 嵌入二进制（`kiana-theme` 的 `assets.rs`），内容见附录 A。
  注意：主题 JSON 的 crate 落点是 **`kiana-theme`**（`crates/kiana-theme/src/assets.rs`，`ARCHITECTURE.md` §5），
  本节 §8.1 树里不再列 `assets/themes/`；本单元通过 `kiana_theme` 读取。
- `docs/themes.md` 等文档若被 `/help` 或 `/themes` 引用，需以 `include_str!` 内嵌或随包分发。
  `getDocsPath` 的**规格**在 ca-cli 单元（`spec/ca-cli.md` §2.6），**实现**在 `kiana-core::config`
  （`ARCHITECTURE.md` §5）；本单元只调用 `kiana_core::config::get_docs_path()`。单二进制发布时
  它返回内嵌资产目录的虚拟路径，见 `spec/GAPS.md` 的 “G2.3 打包资源没有任何 crate 落点” 条目。
- `/bug` 的 zip 内容由 `ca-session` 单元的 `writeBugReportArchive` 生成，本单元只传 bundle。

---

## 7. 测试清单

所有测试位于 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/test/`，使用 vitest。Rust 侧应逐条移植为集成/单元测试；**测试名保留英文原文**便于对照。

> 说明：§2.x 的每个文件小节末尾也摘录了该文件对应的测试名（便于就地阅读）。本节是**完整清单**，两处内容重叠但以本节为准（条数为 `grep -c '^\s*\(it\|test\)(' 的实测值）。
>
> **规模勘误**：`grep -l "modes/interactive" test/**/*.test.ts` 实测命中 **90 个文件**，不是旧版自称的 54 个。分布：顶层 `test/*.test.ts` 69 个（其中 19 个旧版漏列，见 §7.3b）、`test/suite/regressions/*.test.ts` 21 个（旧版 §7.5 只列了 4 个对、另列了 3 个无关文件）。注意 `grep` 命中里有一部分只是 `vi.mock(".../modes/interactive/theme/theme.js")` 或引用 `theme/dark.json` 路径（如 `rpc-prompt-response-semantics.test.ts`、`5868-rpc-unknown-command-id.test.ts`、`2791-fswatch-error-crash.test.ts`），这类文件用来验证"交互单元的 theme 模块被 mock 后 RPC/main 侧仍工作"，移植时可以只做轻量对齐。

### 7.1 `interactive-mode-*.test.ts`（本单元核心，11 个文件）

#### `interactive-mode-anthropic-warning.test.ts`（4）
- `warns once when Anthropic subscription auth is detected`
- `warns when Anthropic OAuth is stored even if token refresh lookup would fail`
- `does not warn for non-Anthropic models`
- `does not warn when Anthropic extra usage warning is disabled`

#### `interactive-mode-assistant-diagnostics.test.ts`（2）
- `shows Anthropic thinking drops when cache miss notices are enabled`
- `does not repeat unchanged Anthropic thinking drops`

#### `interactive-mode-bug-report-hint.test.ts`（5）
- `identifies extensions with frames in a crash stack`
- `does not suggest reports for retryable provider failures`
- `does not suggest reports for cancellations`
- `suggests reports for unexpected errors`
- `shows the install change warning instead of a bug report hint`

#### `interactive-mode-clone-command.test.ts`（2）
- `clones the current leaf into a new session`
- `shows a status message when there is nothing to clone`

#### `interactive-mode-compaction.test.ts`（6）
- `uses the cache miss notice setting for compaction and branch summary costs`
- `renders each compaction cost after its summary`
- `renders retained entries and appends the latest summary cost at the bottom`
- `updates the working state when the same agent run resumes after compaction`
- `routes interactive response aborts through AgentSession`
- `preserves steering behavior when flushing into an active agent run`

#### `interactive-mode-extension-dialogs.test.ts`（4）
- `shows the earlier confirm again after the later one is answered`
- `removes a hidden dialog when its signal aborts`
- `stacks concurrent inline custom components`
- `shows a confirm opened from an inline custom component and then restores the component`

#### `interactive-mode-import-command.test.ts`（6）
- `strips quotes from /import path arguments`
- `preserves apostrophes in unquoted /import path arguments`
- `enforces command token boundaries`
- `passes unquoted path to runtimeHost.importFromJsonl`
- `passes unquoted apostrophe path to runtimeHost.importFromJsonl unchanged`
- `shows a non-fatal error when /import path does not exist`

#### `interactive-mode-startup-input.test.ts`（3）
- `restores a prompt submitted while managed-tool setup is running`
- `queues a normal prompt submitted before the input callback is installed`
- `returns queued startup input before installing a new input callback`

#### `interactive-mode-suspend.test.ts`（3）
- `shows a status message and skips suspend on Windows`
- `keeps the process alive while suspended and restores the TUI on SIGCONT`
- `cleans up the temporary handlers if suspension fails`

#### `interactive-mode-tree-navigation.test.ts`（3 个 `it` + 1 个 `it.each` 两参数 = 5 个用例）
- `preserves operation UI when choosing %s while busy`（`it.each(["Summarize", "No summary"])`，参数化）
- `allows navigation when compaction finishes while the dialog is open`
- `still aborts an active response before navigating`
- `rechecks availability after the response abort settles`

#### `interactive-mode-status.test.ts`（35 个 `it` + 1 个 `test.each` 两参数 = 37 个用例）
状态合并与布局：
- `coalesces immediately-sequential status messages`
- `appends a new status line if something else was added in between`
- `renders tool updates as one contiguous group`
- `applies expansion state to the active header and chat entries`

主题：
- `persists theme changes to settings manager`
- `does not persist invalid theme names`

扩展 UI：
- `overlay custom UI reclaims input after non-overlay custom UI closes`
- `closing a lower $lowerKind overlay custom UI removes its own overlay, not the topmost one`（`test.each`，`capturing` / `non-capturing` 两参数）

自动补全：
- `stores wrapper factories and rebuilds autocomplete immediately`
- `stacks wrapper factories over a fresh base provider`
- `merges triggerCharacters from wrapper factories`

命令参数匹配：
- `matches model command arguments across provider/model order`
- `matches login command arguments by provider id and name`

资源列表（`showLoadedResources`，20 条）：
- `shows a compact resource listing by default`
- `shows full resource listing when expanded`
- `shows full resource listing on verbose startup even when tool output is collapsed`
- `abbreviates extensions in compact listing`
- `captures mixed extension layouts in compact output`
- `adds more parent folders until local extension labels are unique`
- `strips index.ts from local extension label, showing parent dir`
- `strips index.js from local extension label, showing parent dir`
- `mixed single-file and subdirectory index.ts extensions strip index.ts`
- `multiple index.ts with unique parent dirs need no disambiguation`
- `multiple index.ts with same parent dir name disambiguated with grandparent`
- `non-index file in subdirectory stays as filename`
- `package extensions still strip index.ts correctly (regression guard)`
- `labels npm sibling extensions relative to the declaring package`
- `labels Windows npm sibling extensions relative to the declaring package`
- `captures mixed extension layouts in expanded output`
- `shows context paths relative to cwd while preserving full external paths`
- `shows system prompt context paths before project context files`
- `shows full context paths when expanded`
- `does not show verbose listing on quiet startup during reload`

quiet startup：
- `hides resource listing but keeps the startup header with header-only quiet startup`
- `hides the startup header with full quiet startup unless verbose`
- `still shows diagnostics on quiet startup when requested`

### 7.2 `interactive-tui.test.ts`（10）

渲染器与全屏：
- `selects the alternate-screen renderer only when requested`
- `shows the configured jump-to-bottom shortcut while scrolled up`
- `replaces the renderer and restores the previous screen for resume-hint exits`

剪贴板与选择：
- `feeds clipboard text to the focused component as a bracketed paste`
- `copies an active fullscreen selection when copy-on-select is disabled`
- `copies the last assistant message with an active fullscreen selection when copy-on-select is enabled`
- `flashes Copied! for the copy shortcut in fullscreen mode`
- `keeps the status-line confirmation for the copy shortcut in regular mode`
- `drops the fullscreen selection when session entries are re-rendered`
- `uses the standalone row for a custom editor that has not opted in`

### 7.3 组件测试（36 个文件）

#### `tool-execution-component.test.ts`（26 个 `test` + 1 个 `test.each` 十参数）
- `converts non-PNG tool images once the transcoder loads`
- `stacks custom call and result renderers like the old implementation`
- `self-rendered empty tool rows take no layout space`
- `uses built-in rendering for built-in overrides without custom renderers`
- `preserves legacy file_path rendering compatibility for built-in tools`
- `bash execute emits an initial empty partial update before output arrives`
- `bash renderer does not duplicate final full output truncation details`
- `bash renderer formats $ms ms as $formatted while running and after completion`（`test.each`，10 组输入：`0`→`0.0s`、`4_200`→`4.2s`、`59_900`→`59.9s`、`59_999`→`60.0s`、`60_000`→`1m 0s`、`90_900`→`1m 30s`、`1_592_200`→`26m 32s`、`3_599_999`→`59m 59s`、`3_600_000`→`1h 0m 0s`、`7_384_900`→`2h 3m 4s`）
- `bash renderer shows a result's recorded duration, also for a result restored without a live start`
- `does not duplicate built-in headers when passed the active built-in definition`
- `renders read calls with null offset and limit as full-file reads`
- `inherits missing built-in result renderer slot from the built-in tool`
- `inherits missing built-in call renderer slot from the built-in tool`
- `uses custom renderers for built-in overrides that reuse built-in definition parameters`
- `uses custom renderers for built-in overrides that reuse wrapped built-in tool parameters`
- `shares renderer state across custom call and result slots`
- `exposes args in render result context`
- `shows arguments in the fallback call header`
- `collapses fallback results until expanded`
- `trims trailing blank display lines from write previews`
- `renders streamed partial arguments once per frame, with the latest arguments`
- `trims trailing blank display lines from read results`
- `does not syntax-highlight read errors based on the requested file path`
- `expands a collapsed tool result when clicked`
- `collapses ordinary read results until expanded`
- `renders ${scenario.title} read results compactly until expanded`（参数化）
- `shows the read line range in compact ${scenario.title} reads before the expand hint`（参数化）

#### `tree-selector.test.ts`（18）
- `focuses nearest visible ancestor when currentLeafId is a model_change with sibling branch`
- `hides context edits by default and labels them in all mode`
- `focuses nearest visible ancestor when currentLeafId is a thinking_level_change entry`
- `switches to nearest visible user message when changing to user-only filter`
- `returns to nearest visible ancestor when switching back to default filter`
- `renders semantic help rows without truncating narrow terminal controls`
- `copies the full selected message with ctrl+x`
- `toggles label timestamps for labeled nodes`
- `preserves selection when switching to empty labeled filter and back`
- `preserves selection through multiple empty filter switches`
- `ctrl+right unfolds a folded node, then does segment jump when unfolded`
- `alt+left/right are aliases for fold and unfold navigation`
- `folding root hides entire subtree, nested fold preserved on unfold`
- `fold and navigate on non-active branch`
- `fold and navigate with multiple roots`
- `folding root hides descendants even when intermediate nodes are filtered out`
- `search resets fold state`
- `filter mode change resets fold state`

#### `session-selector-search.test.ts`（14）
- `filters by quoted phrase with whitespace normalization`
- `filters by regex (re:) and is case-insensitive`
- `recent sort preserves input order`
- `relevance sort orders by score and tie-breaks by modified desc`
- `ignores the position of a match in message text`
- `does not match scattered characters in session ids`
- `ranks name text, then message text, then scattered name matches`
- `matches each token within a single field`
- `ranks regex name matches first`
- `returns empty list for invalid regex`
- `returns all sessions when nameFilter is 'all'`
- `returns only named sessions when nameFilter is 'named'`
- `applies name filter before search query`
- `excludes whitespace-only names from named filter`

#### `program-status-reporter.test.ts`（7）
- `reports idle, working during a run, and done once it settles`
- `reports only the outcome of the run: retried errors, final errors, and aborts`
- `reports a failed recovery compaction as the run's error unless a later response succeeds`
- `reports compaction inside a run and the result of a manual compaction`
- `reports the most recent open dialog and the underlying state once all close`
- `sends each status once and follows session name changes`
- `returns to idle when the session is replaced`

#### 主题相关（6 个文件，35 条）
`theme-controller.test.ts`（9）：
- `uses the initial theme without persisting it`
- `applies the theme immediately and lets startup wait for the colors`
- `falls back to palette indices, then applies colors that arrive after the timeout`
- `re-queries the colors on appearance changes and lets them decide`
- `uses the reported scheme for the system theme when the terminal reports no colors`
- `re-renders only when the reported colors change`
- `disables terminal appearance updates when disposed`
- `lets an explicit selection replace the initial theme`
- `reloads theme settings when no initial theme was supplied`

`theme-detection.test.ts`（5）：
- `classifies the last field by palette index like Vim`
- `prefers the background, then the reported scheme, then COLORFGBG, then dark`
- `follows the foreground when text is readable that way`
- `uses terminal capabilities`
- `parses and resolves automatic theme settings`

`theme-style.test.ts`（6）：
- `renders theme tokens the same as the generic text styler`
- `rejects unknown tokens and tokens in the wrong slot`
- `loads OKLCH theme values`
- `loads OKHSL theme values, including through variables`
- `detects the appearance unless it is declared`
- `renders empty tokens as terminal defaults and reports concrete colors for them`

`theme-export.test.ts`（3）：
- `resolves export variable references using the same syntax as colors`
- `converts OKHSL export colors to hex because CSS does not support them`
- `resolves recursive vars and converts 256-color export values to hex`

`system-theme.test.ts`（8）：
- `keeps body text readable (WCAG 4.5:1) on the background and its panels`
- `orders foreground roles by contrast and keeps panels close to the background`
- `uses the terminal foreground and palette hues`
- `keeps pastel palette colors pastel at other lightnesses`
- `renders grayscale at zero saturation`
- `falls back to palette indices and faint text without a background`
- `is listed first, has no export colors, and is generated from the terminal colors`
- `renders faint tokens with SGR 2 and closes it`

`theme-picker.test.ts`（1）：
- `uses custom theme content names instead of file names`

`scrollbar-theme.test.ts`（3）：
- `uses explicitly configured scrollbar colors`
- `falls back to existing selection and text colors for search highlights`
- `uses explicitly configured search highlight colors`

#### 选择器（7 个文件，28 条）
`model-selector.test.ts`（4）：
- `keeps the current model marked while browsing`
- `shows model names that differ from the ID`
- `uses the configured save binding`
- `lists every catalog that failed to refresh`

`model-search.test.ts`（5）：
- `hides names that restate the ID`
- `shows names with words the ID lacks`
- `requires single-character words to match a whole ID token`
- `ranks models containing the query above scattered matches`
- `ranks the typed model above models sharing a long provider prefix`

`scoped-models-selector.test.ts`（5）：
- `shows model names that differ from the ID`
- `marks every model after enabling all`
- `disables only the selected model after enabling all`
- `enables only the selected model after clearing all`
- `restores the all-enabled state after re-enabling the last disabled model`

`oauth-selector.test.ts`（6）：
- `projects provider-owned auth options without provider-specific filtering`
- `renders an option without compiled auth status as not configured`
- `shows OAuth auth distinctly in the API key selector`
- `shows environment API key auth as configured`
- `shows models.json API key auth as configured`
- `shows models.json command auth as configured`

`settings-selector.test.ts`（5）：
- `cycles through fullscreen settings`
- `cycles through share destinations`
- `keeps the configured fixed theme marked while browsing`
- `keeps a configured automatic theme marked while browsing`
- `keeps the configured per-model thinking level marked while browsing`

`thinking-selector.test.ts`（2）：
- `keeps the current thinking level marked while browsing`
- `uses the configured save binding`

`trust-selector.test.ts`（4）：
- `keeps the saved trusted decision marked while browsing`
- `selects a trust decision`
- `labels saved ancestor decisions as inherited`
- `adds a trust parent option`

#### 会话选择器（3 个文件，19 条）
`session-selector-path-delete.test.ts`（8）：
- `does not treat Ctrl+Backspace as delete when search query is non-empty`
- `enters confirmation mode on Ctrl+D even with a non-empty search query`
- `enters confirmation mode on Ctrl+Backspace when search query is empty`
- `does not switch scope back to All when All load resolves after toggling back to Current`
- `does not start redundant All loads when toggling scopes while All is already loading`
- `threads sessions when parent and child paths use different symlink aliases`
- `sorts threaded sessions by latest activity in their subtree`
- `treats the current session as active across symlink aliases`

`session-selector-rename.test.ts`（3）：
- `shows rename hint in interactive /resume picker configuration`
- `does not show rename hint in --resume picker configuration`
- `enters rename mode on Ctrl+R and submits with Enter`

`session-selector-search.test.ts`：见 7.3 上（14 条）

#### 消息组件（7 个文件，35 条）
`assistant-message.test.ts`（12）：
- `adds OSC 133 zone markers to assistant messages without tool calls`
- `does not add OSC 133 zone markers when assistant message contains tool calls`
- `renders length stops with neutral truncation wording`
- `coalesces adjacent thinking blocks into one hidden thinking label`
- `collapses individual thinking runs when clicked`
- `uses configured output padding for text and thinking`
- `chains Markdown transformers in registration order`
- `identifies partial assistant Markdown as streaming`
- `reapplies Markdown transformers when available width changes`
- `continues the Markdown transformer chain when a transformer throws`
- `transforms text and thinking Markdown without mutating the original message`
- `uses configured output padding for user messages`

`user-message.test.ts`（4）：
- `keeps user message height stable while moving closing OSC markers off line end`
- `chains Markdown transformers with user message context`
- `reapplies Markdown transformers when invalidated`
- `passes persisted entry identity in transformer context; live leaves it undefined`

`collapsible-message-components.test.ts`（3）：
- `toggles a compaction summary when clicked`
- `toggles a branch summary when clicked`
- `toggles a skill invocation when clicked`

`custom-message.test.ts`（1）：
- `provides output padding to custom renderers and updates it`

`themed-text.test.ts`（1）：
- `builds lazily and rebuilds with the current theme after invalidation`

`bash-execution-width.test.ts`（2）：
- `collapsed preview lines respect render-time width, not construction-time width`
- `re-computes lines when width changes between renders`

`chat-viewport.test.ts`（1）：
- `defaults the transcript scrollbar to auto and accepts overrides`

`status-indicator.test.ts`（5）：
- `keeps idle status at the same height as standalone status indicators`
- `keeps the top border unchanged unless the editor opts in`
- `embeds the working indicator when the editor opts in`
- `embeds compaction, summary, and retry labels within the border width`
- `disposes retry countdown updates`

`output-pad.test.ts`（5 个参数化用例 × 1 断言组）：
- `bash execution renders at outputPad 0 and 1`
- `tool execution renders at outputPad 0 and 1`
- `tool execution without a definition renders at outputPad 0 and 1`
- `self-rendered edit result renders at outputPad 0 and 1`
- `compaction summary renders at outputPad 0 and 1`
  断言：`outputPad = 0` 时无非空行以空格开头；`setOutputPad(1)` 后每行**整体**多一个前导空格。

#### 其它（5 个文件，17 条）
`login-dialog-device-code.test.ts`（6）：
- `opens the verification page and copies the code once after confirmation`
- `clears the previous auth URL copy action when showing a device code`
- `clears device actions when switching to browser authentication`
- `clears both action types when showing details`
- `cancels without opening a browser or changing the clipboard`
- `keeps manual login available when browser and clipboard access fail`

`auth-url-copy.test.ts`（3）：
- `login dialog copies the auth URL instead of typing into the code input`
- `login dialog ignores the copy key without an auth URL`
- `MCP sign-in screen copies the authorization URL`

`mermaid.test.ts`（8）：
- `replaces Mermaid code blocks with Unicode diagrams`
- `leaves unsupported and oversized diagrams unchanged`
- `maps semantic spans through the Kiana theme`
- `renders incomplete Mermaid blocks during streaming`
- `renders diagrams with class assignments`
- `falls back to the code block with a warning after streaming`
- `summarizes additional partial-render warnings`
- `respects rendering modes and skips thinking blocks`

`bug-report.test.ts`（3）：
- `preserves line breaks in pasted descriptions`
- `removes URL credentials and secret query parameters`
- `redacts nested secret values without hiding token counts`

`session-share.test.ts`（3）：
- `keeps concurrent session exports isolated`
- `forces gist sharing without checking Radius authentication`
- `reports missing Radius authentication when Radius is forced`

`clipboard-paste-file-paths.test.ts`（4）：
- `Finder file paths take precedence over their icon image`
- `clipboard file paths containing terminal control characters are rejected`
- `native file-path errors are shown without falling through to the icon image`
- `bash mode shell-quotes file paths and inserts them as arguments`

`model-catalog-refresh.test.ts`（3）：
- `shares one runtime refresh between concurrent callers`
- `keeps the shared refresh alive when one caller stops waiting`
- `aborts an abandoned refresh and allows a later refresh to start`

### 7.3b 旧版规格**完全漏掉**的 36 个测试文件

> 审计方法：`grep -l "modes/interactive" test/**/*.test.ts` 共命中 **90** 个文件；旧版 §7.1–§7.5 只列了其中 54 个（且其中 4 个实际上并不 import `modes/interactive`，见 §7.5 勘误）。以下 36 个文件在本单元范围内、但旧版清单一条都没写，全部按文件名/条数/测试名原文补录。

**顶层 `test/*.test.ts`（19 个）**

`clipboard-image-native-errors.test.ts`（1）：
- `native image errors abort paste without reading text or changing the editor`

`codemode-renderer.test.ts`（5）：
- `hides the script header and shows the output`
- `shows the cost of model calls and their total`
- `shows results without a header, such as rejected options`
- `limits collapsed output to wrapped lines, not logical lines`
- `shows the elapsed time while the script runs and the total once it finished`

`config-schemas.test.ts`（12 个 `it` + 1 个 `it.each` 三行）：
- `matches the four committed artifacts`
- `preserves editor guidance in the generated theme schema`
- `preserves moved model and settings guidance`
- `rejects non-positive model token limits`
- `validates keybinding syntax`
- `rejects settings values that runtime accessors reject`
- `describes legacy settings migrations accurately`
- `accepts custom compatibility settings and rejects malformed known fields`
- `validates $schema in models.json at runtime`
- `accepts $schema in settings.json and preserves unknown settings`
- `accepts $schema in keybindings.json without treating it as an action`
- `accepts $schema in themes and rejects malformed themes`
- `validates representative $name documents`（`it.each`：`models.json` / `settings.json` / `keybindings.json`，每行一份 `valid`/`invalid` 文档）

`edit-tool-no-full-redraw.test.ts`（3）：
- `renders the large diff in the call preview and does not full-redraw when the result settles`
- `reconstructs the boxed preview from a settled result without argsComplete`
- `shows a preflight error without rendering a diff when the edits do not apply`

`export-html-cjk.test.ts`（2）：
- `renders emphasis delimited next to CJK punctuation`
- `keeps CommonMark flanking rules for non-CJK text`

`export-html-deep-tree.test.ts`（1）：
- `builds and flattens a single-child chain deeper than the call stack`

`export-html-whitespace.test.ts`（3）：
- `preserves whitespace for plain-text tool output lines without preserving template whitespace`
- `does not insert source whitespace between ANSI-rendered lines`
- `trims TUI spacing lines from custom tool result HTML`

`export-jsonl-share.test.ts`（1）：
- `adds presentation data without changing conversation IDs or links`

`external-editor.test.ts`（3）：
- `edits a prompt inside a private temporary directory`
- `keeps the original content when the editor exits unsuccessfully`
- `returns empty content when the editor clears the prompt`

`format-resume-command.test.ts`（8）：
- `returns a session resume command for default session dirs`
- `includes unquoted safe session dirs for non-default session dirs`
- `quotes session dirs containing spaces`
- `quotes session dirs containing single quotes`
- `returns undefined when stdout is not a TTY`
- `returns undefined for in-memory sessions`
- `returns undefined when the session file is missing`
- `returns undefined when the session file is not set`

`max-thinking.test.ts`（2）：
- `is accepted by CLI and settings`
- `falls back to thinkingXhigh for legacy themes`

`mcp-manager-view.test.ts`（2）：
- `cancels the running operation with the cancel key`
- `ignores the cancel key for operations that cannot be cancelled`

`package-command-paths.test.ts`（35）：
- `should persist global relative local package paths relative to settings.json`
- `should remove local packages using a path with a trailing slash`
- `skips untrusted project package settings`
- `uses remembered project trust for list`
- `overrides remembered trust for list with --no-approve`
- `approves project trust for list with --approve`
- `uses default project trust for list`
- `uses project_trust extensions for package commands`
- `does not prompt or ask extensions for project trust during update`
- `uses saved project trust during update`
- `lets trust.json override default project trust`
- `blocks local package changes when project is untrusted`
- `allows local package install to initialize fresh project settings`
- `shows install subcommand help`
- `refreshes only model catalogs with update --models`
- `rejects update --models combined with another update target`
- `cycles project package overrides in config local mode`
- `toggles built-in extensions in config global mode`
- `cycles project built-in extension overrides in config local mode`
- `shows a friendly error for unknown install options`
- `shows a friendly error for missing install source`
- `allows explicit self-update checks when automatic version checks are disabled`
- `retries a transient self-update version check`
- `updates installer-managed Kiana through a staged immutable release`
- `accepts a managed install marker with a UTF-8 BOM`
- `removes older managed releases after an update`
- `rejects a concurrent managed update`
- `rejects forced managed reinstalls`
- `keeps the managed release active when its update fails`
- `keeps npm self-updates non-managed when the managed environment is inherited`
- `uses the current package name when the update check omits packageName`
- `installs the active package name from the update check during self-update`
- `prints a pnpm metadata hint when self-update fails`
- `fails self-update when renamed npm package installation fails`
- `suggests the configured source when update input omits the npm prefix`

`read-line-range-display.test.ts`（2）：
- `formats raw read arguments`
- `shows string ranges in the TUI, session tree, and HTML export`

`resource-loader-theme.test.ts`（2 个 `it` + 1 个 `it.each` 两行）：
- `uses the $setting setting over a $environment environment`（`it.each`：`{environment:"256-color", environmentOverride:"0", setting:true, expected:"\x1b[48;2;60;53;68mx\x1b[49m"}` 与 `{environment:"truecolor", environmentOverride:"1", setting:false, expected:"\x1b[48;5;59mx\x1b[49m"}`；**设置项优先于 `KIANA_TRUE_COLOR` 环境变量**）
- `returns to automatic detection after an explicit setting is removed`
- `applies strict validation on initial load and reload`

`rpc-prompt-response-semantics.test.ts`（5 个 `it` + 2 个 `it.each` 各两行）：
- `emits one failure response when prompt preflight rejects`
- `emits one started response when prompt preflight succeeds`
- `reports extension commands and intercepted input as handled without starting a run`
- `emits one success response when prompt is queued during streaming`
- `reports %s as handled even when an extension queues another message`（`it.each(["steer","follow_up"])`，#9803）
- `reports %s as queued after input transformation`（`it.each(["steer","follow_up"])`）
- `returns and clears queued steering and follow-up messages`

`session-info-modified-timestamp.test.ts`（1）：
- `uses last user/assistant message timestamp instead of file mtime`

`syntax-highlight.test.ts`（9）：
- `loads the twenty most common languages at startup and defers the rest`
- `renders highlighted spans with the provided theme`
- `decodes HTML entities emitted by highlight.js`
- `inherits parent formatting for unmapped nested scopes`
- `keeps parent formatting across unscoped nested spans`
- `highlights code through highlight.js`
- `colors each line of Python docstrings independently`
- `colors diff additions and deletions in fenced diff blocks`
- `keeps cli-highlight default styled scopes mapped to theme styles`

`tool-renderer-examples.test.ts`（1 个 `it` + 1 个 `it.each` 两行）：
- `keeps the system prompt unchanged for the $name example`（`it.each`：`built-in-tool-renderer.ts`（read/bash/edit/write）、`minimal-mode.ts`（read/bash/write/edit/find/grep/ls））
- `keeps minimal mode's edit tool in the default shell`

**`test/suite/regressions/`（17 个）**

`10285-mcp-tool-renderers.test.ts`（2）：
- `renders calls to MCP tools that are not registered`
- `renders them in HTML exports too`

`2791-fswatch-error-crash.test.ts`（1）：
- `process should survive an error event on the theme FSWatcher`

`3217-scoped-model-order.test.ts`（2）：
- `propagates reordered scoped models back to the session state`
- `preserves scoped model order in the /model scoped tab`

`4167-thinking-toggle-pending-tool-render.test.ts`（2，直接测 `InteractiveMode.renderSessionEntries`）：
- `keeps unresolved rendered tool calls registered for live completion events`
- `does not keep completed historical tool calls registered as pending`

`5080-signal-shutdown-extension-cleanup.test.ts`（9 个 `test` + 1 个 `test.each` 三行）：
- `signal-triggered shutdown emits session_shutdown before terminal writes`
- `interactive quit stops the TUI before emitting session_shutdown`
- `interactive quit prints a resume hint for persisted sessions`
- `signal-triggered shutdown does not print a resume hint`
- `re-entrant shutdown is a no-op`
- `stdin %s exits quietly without recording a crash`（`test.each`：`["read EIO","EIO"]`、`["setRawMode EIO","EIO"]`、`["setRawMode ENOTTY","ENOTTY"]`）
- `other stdin errors still crash`
- `uncaught dead terminal errors are not recorded as crashes`
- `other uncaught errors are still recorded as crashes`
- `handlers are removed on unregister`

`5724-sigterm-signal-exit.test.ts`（1）：
- `keeps signal handlers registered while signal-triggered cleanup is pending`

`5868-rpc-unknown-command-id.test.ts`（1）：
- `preserves the request id on unknown command errors`

`5943-session-start-notify.test.ts`（7，全部直接针对 `InteractiveMode` 的启动/重绑顺序）：
- `renders loaded resources before restored messages without stale entries`
- `renders replacement session state before session_start handlers can notify`
- `subscribes before replacement session_start handlers send messages`
- `subscribes before replacement session_start handlers send user messages`
- `runs the reload render hook before reload session_start handlers can notify`
- `refreshes hideThinkingBlock before rebuilding chat during reload`
- `keeps the reload blocker focused until async reload completes`

`6949-unavailable-scoped-model.test.ts`（4）：
- `shows and removes an enabled model without a catalog entry`
- `passes unmatched settings patterns to the selector with one combined resolution`
- `opens when only a session-scoped model is unavailable`
- `does not clear a partial scope when an enabled model is unavailable`

`6999-models-json-hot-reload.test.ts`（1）：
- `reloads models.json when opening /model`

`7027-credential-refresh-hang.test.ts`（5 个 `it` + 1 个 `it.each` 两行）：
- `does not hold login behind an older stalled network catalog refresh`
- `completes interactive login before its bounded background refresh`
- `reports an empty catalog only after refresh`
- `selects $selected from the refreshed catalog $models`（`it.each`：`{models:["fast","balanced"], selected:"balanced"}`、`{models:["fast","powerful"], selected:"fast"}`）
- `preserves a model selected during refresh`
- `bounds refresh to 15 seconds`

`7443-model-command-cached-match.test.ts`（2）：
- `matches the availability snapshot without starting a catalog refresh`
- `uses a caller-owned deadline only after a cache miss`

`7731-tui-method-wrapping.test.ts`（2）：
- `calls the method captured before a replacement`
- `routes a captured method to a replacement renderer`

`7829-invalid-settings-warning.test.ts`（1）：
- `renders startup diagnostics inside the transcript`

`8611-thinking-toggle-pending-bash-output.test.ts`（1）：
- `preserves partial bash output`

`9068-user-bash-fail-closed.test.ts`（2 个 `test.each`：RPC 侧 N 行 + Interactive 侧两行）：
- `$name`（RPC `test.each(rpcCases)`，每行 `{extension, error, executeCount}`）
- `fails closed for %s when a handler returns an empty result`（`test.each`：`["!pwd", false]`、`["!!pwd", true]`——`user_bash` 扩展处理器返回空结果时**必须失败关闭**，不能回退到本地执行）

`startup-session-rebind-duplicate-subscription.test.ts`（1）：
- `does not subscribe from the stale startup rebind`

### 7.4 `footer-*.test.ts`（2 个文件，20 条）

`footer-data-provider.test.ts`（8）：
- `uses HEAD directly in a regular repo from a nested directory`
- `resolves the branch via git when HEAD is .invalid in a reftable repo`
- `resolves the branch via git in a reftable-backed worktree`
- `treats an unresolved .invalid reftable HEAD as detached`
- `does not notify listeners when reftable updates keep the same branch`
- `debounces rapid reftable updates into a single async refresh`
- `updates the cached branch when the reftable directory changes`
- `retries git watchers 5 seconds after an async fs.watch error`

`footer-width.test.ts`（11）：
- `does not abbreviate sibling paths that share the home prefix`
- `abbreviates the home directory and descendants`
- `keeps all lines within width for wide session names`
- `keeps stats line within width for wide model and provider names`
- `shows the physical model a virtual model routed to`
- `includes summary and tool result usage in the total cost`
- `updates cached usage totals after an entry is appended`
- `shows the latest cache hit rate when cache usage is present`
- `marks Kimi Coding costs as subscription estimates`
- `marks explicitly identified subscription auth`
- `does not mark generic OAuth sign-in as a subscription`

### 7.5 回归测试（在 `test/suite/regressions/`，与本单元相关）

**真正 import `modes/interactive` 的 21 个文件**（`grep -l "modes/interactive" test/suite/regressions/*.test.ts`）：

| 文件 | 场景 |
|---|---|
| `5433-extension-oauth-prompt-input.test.ts` | 扩展 OAuth 的 prompt 输入 |
| `5596-missing-theme-export.test.ts` | 主题缺 `export` 段 |
| `7153-scoped-models-refresh.test.ts` | scoped models 刷新 |
| `7209-model-selector-filter-resets-selection.test.ts` | 过滤后选择位置重置 |
| `10285-mcp-tool-renderers.test.ts` | 未注册 MCP 工具也能渲染（含 HTML 导出） |
| `2791-fswatch-error-crash.test.ts` | 主题 `FSWatcher` 的 `error` 事件不崩进程 |
| `3217-scoped-model-order.test.ts` | scoped model 顺序回写会话状态 |
| `4167-thinking-toggle-pending-tool-render.test.ts` | 未完成的工具调用保持注册，完成后不再 pending |
| `5080-signal-shutdown-extension-cleanup.test.ts` | 关闭顺序：`session_shutdown` 与终端写入的前后关系、resume hint、stdin 错误 |
| `5724-sigterm-signal-exit.test.ts` | signal 清理未完成前不注销 handler |
| `5868-rpc-unknown-command-id.test.ts` | 未知命令错误保留 request id |
| `5943-session-start-notify.test.ts` | 启动/重绑顺序：资源先于消息、先订阅再让 `session_start` 通知 |
| `6949-unavailable-scoped-model.test.ts` | 目录里不存在的 enabled 模型也要能显示/移除 |
| `6999-models-json-hot-reload.test.ts` | 打开 `/model` 时热重载 `models.json` |
| `7027-credential-refresh-hang.test.ts` | 登录不被旧的卡住刷新阻塞；刷新上限 15s |
| `7443-model-command-cached-match.test.ts` | `/model` 用缓存快照匹配，未命中才刷新 |
| `7731-tui-method-wrapping.test.ts` | 替换 renderer 后仍可调用先前捕获的方法 |
| `7829-invalid-settings-warning.test.ts` | 启动诊断渲染在 transcript 内 |
| `8611-thinking-toggle-pending-bash-output.test.ts` | 切换 thinking 显示时保住在跑的 bash 部分输出 |
| `9068-user-bash-fail-closed.test.ts` | `user_bash` 处理器返回空结果时失败关闭 |
| `startup-session-rebind-duplicate-subscription.test.ts` | 过期的启动重绑不得重复订阅 |

> **勘误（旧版 §7.5 列错的 3 项）**：`2753-reload-stale-resource-settings.test.ts`、`3616-settings-inmemory-reload.test.ts`、`8537-custom-message-tool-result-ordering.test.ts` 都**不** import `modes/interactive`（分别只测 `runtime.session.reload()`、内存设置语义、`core/messages.ts` 的 `convertToLlm`），属于别的单元，已从上表移除。`7.4` 里的 `footer-data-provider.test.ts` 同样 import 的是 `src/core/footer-data-provider.ts`（不在本单元目录），但它测的正是 `FooterComponent` 依赖的 provider，保留在 §7.4 仅作参照。

### 7.6 移植测试策略

| 层次 | 做法 |
|---|---|
| 纯函数（`model-search`、`session-selector-search`、`theme` 颜色转换、`formatKeyText`、OSC 7501 编码） | 逐条单测，断言**逐字符相等** |
| 组件渲染 | 构造 `VirtualTerminal`（宽 80×24 或测试指定宽度），`render(width)` 后 `stripAnsi` 比对快照；**同时**保留一份含 ANSI 的原始快照 |
| 交互 | 直接调用 `handle_input("...")` 喂原始字节（如 `"\x1b[A"`、`"\r"`、`"\x03"`），断言状态与渲染 |
| 时序 | 用 tokio 的 `tokio::time::pause()` 或注入可控 clock；`performance.now()` 相关（shimmer、loader）改为注入 `now` |
| 进程 | `gh`/`trash`/编辑器用注入的 `CommandRunner` trait mock，断言 argv 逐字 |
| HTTP | 用 `wiremock` 或自建 `TcpListener`；断言 URL、查询参数顺序、请求头、请求体字节 |
| 文件系统 | `tempfile::TempDir`；断言文件名模式、内容字节、权限位（`0o600`） |
| 崩溃日志 | 断言 `JSON.stringify(records, null, 2) + "\n"` 逐字 |
| 主题 JSON | 断言内置 `dark.json`/`light.json` 与仓库文件**字节一致**（用 `include_str!` 比对 hash） |

---

## 8. 建议的 Rust 模块布局与最难复刻之处

### 8.1 crate 与模块划分

建议把本单元拆成一个 crate `kiana-interactive`（`ARCHITECTURE.md` §3.7 采纳单 crate；依赖按 §2 的分层为 `kiana-agent-core`、`kiana-ai`、`kiana-builtin-extensions`、`kiana-config`、`kiana-core`、`kiana-export`、`kiana-ext-api`、`kiana-extensions`、`kiana-resources`、`kiana-session`、`kiana-session-core`、`kiana-theme`、`kiana-tools`、`kiana-tui`、`kiana-ui-kit`；spec 里旧写的 `kiana-tui-components`/`kiana-tui-text` 已并入 `kiana-tui`），内部按源文件一一对应：

```
crates/kiana-interactive/
  Cargo.toml
  assets/
    clankolas.png                    ← include_str!/include_bytes!（与 TS 仓库字节一致）
                                       # dark.json / light.json **不在这里**：主题 JSON 归 `kiana-theme`
                                       # （`crates/kiana-theme/src/assets.rs`，见 ARCHITECTURE.md §5）
  src/
    lib.rs                           // pub use 重导出（对应 components/index.ts + modes/interactive 导出面）
    interactive_mode/
      mod.rs                         // InteractiveMode 结构体、字段（§2.50.2）
      init.rs                        // init()（§2.50.3）
      run.rs                         // run()（§2.50.4）
      events.rs                      // handleEvent（§2.50.5）
      render.rs                      // 聊天区渲染（§2.50.6）
      lifecycle.rs                   // 信号、stop()、crash（§2.50.7 / §2.50.19）
      input.rs                       // setupKeyHandlers / onEscape / 粘贴（§2.50.8）
      commands/
        mod.rs                       // setupEditorSubmitHandler 分派表（§3.2）
        settings.rs                  // /settings（§2.50.9）
        thinking.rs                  // /thinking（§2.50.10）
        model.rs                     // /model、/scoped-models（§2.50.11）
        auth.rs                      // /trust、/login、/logout（§2.50.12）
        reload.rs                    // /reload（§2.50.13）
        export_import_share.rs       // /export /import /share /bug /copy /name（§2.50.14）
        session.rs                   // /session（§2.50.15）
        misc.rs                      // /changelog /hotkeys /new /debug /arminsayshi /dementedelves（§2.50.16）
        bash_compact.rs              // bash 与 /compact（§2.50.17）
        resources.rs                 // showLoadedResources（§2.50.21）
      extension_ui/
        mod.rs                       // ExtensionUIContext 实现（§2.50.18 + §2.50.20）
        dialog_stack.rs              // 对话框栈
        widgets.rs                   // setWidget/setStatus/setHeader/setFooter
      status.rs                      // showStatus/showError 合并、扩展诊断
    theme/
      mod.rs                         // Theme、全局单例、TUI 主题工厂（theme.ts）
      tokens.rs                      // 56 token（theme-tokens.ts）
      system.rs                      // system-theme.ts
      controller.rs                  // InteractiveThemeController
      schema.rs                      // theme-schema.ts（serde 校验替代 typebox）
      color_math.rs                  // OKHSL/OKLCH/OKLab、mix_colors、前景/背景降级
    components/
      mod.rs
      tool_execution.rs
      model_selector.rs
      scoped_models_selector.rs
      settings_selector.rs
      settings_submenu.rs
      oauth_selector.rs
      login_dialog.rs
      radius_login_selector.rs
      session_selector.rs
      session_selector_search.rs
      tree_selector.rs
      config_selector.rs
      footer.rs
      assistant_message.rs
      user_message.rs
      bash_execution.rs
      …（其余按 §0.1 一文件一模块）
    chat_viewport.rs
    external_editor.rs
    model_catalog_refresh.rs
    model_search.rs
    program_status_reporter.rs
    session_share.rs
    tui_renderer.rs
    bug_report.rs
```

### 8.2 关键 trait / 类型骨架

```rust
// 统一渲染契约（与 kiana-tui 的 Component 对应）
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn invalidate(&mut self) {}
    fn handle_input(&mut self, _data: &str) {}
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> { None }
    fn dispose(&mut self) {}
    fn is_focusable(&self) -> bool { false }
    fn set_focused(&mut self, _focused: bool) {}
}

// 主题单例：进程内一份，替换用 ArcSwap 或 RwLock
pub struct ThemeRegistry { /* current: Arc<Theme>, custom: Vec<ThemeDefinition>, watcher: Option<Watcher> */ }

// 键位提示
pub struct KeybindingHints;   // format_key_text / key_text / key_display_text / key_hint / raw_key_hint

// 对话框栈（§2.50.20）
pub struct DialogStack {
    visible: Vec<DialogEntry>,     // 栈顶为当前可见
    hidden: Vec<DialogEntry>,      // hidden 但仍可应答
}
```

### 8.3 最难复刻之处（按难度降序）

1. **`interactive-mode.ts`（7105 行）的整体控制流**：这是全仓库最大的单文件。它有约 200 个方法、几十个字段，且大量状态互相耦合（编辑器/状态行/覆盖层/全屏/对话框/队列/滚动/选择）。**没有任何抽象能消除它**——Rust 侧必须忠实保留同一组字段与同一套方法调用顺序，否则行为会在细节处漂移（例如“先 `requestRender` 再改状态”还是反之，会影响单帧输出）。建议：先用 `rustfmt` 允许的行宽上限放宽，一个模块一个 `impl InteractiveMode` 块，方法名保持 snake_case 对应关系并保留 TS 原文注释。
2. **`theme.ts`（1086 行）的颜色数学**：OKHSL/OKLCH/OKLab 与 sRGB 的双向转换、`mixColors` 的插值空间（`"srgb"` vs `"oklab"`）、`foregroundAnsi`/`backgroundAnsi` 在 truecolor/256/16 色三档下的量化算法、`indexedColor` 的调色板映射。任何一处四舍五入差异都会让**所有**屏幕输出不同。必须用同一套浮点公式（`f32` vs `f64` 也会不同——TS 用 `number` 即 f64，Rust 必须用 `f64`），并在测试中逐色比对 ANSI 字节。
3. **`system-theme.ts`（655 行）**：由终端上报的 16 色/256 色 + 前景背景色，生成一整套 56 token，且要满足 WCAG 4.5:1 对比度与“pastel 保持 pastel”。它内含多次迭代式调整（亮度扫描、饱和度压缩），必须逐行移植其搜索/收敛逻辑与迭代上限。
4. **`fuzzyMatch` / `fuzzyFilter` 的 score 数值**：`session-selector-search` 的排序直接依赖 `fuzzyMatch(...).score` 的**数值**（`tierScore(TIER_NAME, fuzzyMatch(value, name).score)`）。这属于 `tui-core` 单元，但本单元对其数值敏感。必须确认 `tui-core` 移植后的 score 与 TS 完全一致（不是单调一致）。
5. **`markdown` 渲染与 `MarkdownTransformer` 链**：`assistant-message` / `user-message` 会把 transformer 链应用到文本与 thinking；transformer 抛异常时要**继续链**并保留原文（测试 `continues the Markdown transformer chain when a transformer throws`）。Rust 的 `pulldown-cmark` 事件模型与 `Marked` 不同，需在 `tui-text` 层做完整映射。
6. **`tree-selector.ts`（1428 行）**：树构建、过滤模式、折叠状态、祖先可见性搜索、`ctrl+right` 的“先展开再分段跳”、标签时间戳、复制、空过滤时保持选择。折叠逻辑与过滤逻辑相互影响（`folding root hides descendants even when intermediate nodes are filtered out`），是最容易出错的算法之一。
7. **`easter-egg-3d.ts`（1292 行）**：CPU 三角光栅化 + 30 fps 动画 + Kiana logo 与 Armin 两个场景。需要逐像素一致（若测试比对帧）。建议按 §2.47 的公式直接移植，颜色量化走 `theme` 的同一套降级。
8. **`diff.ts`**：`diff` 包（jsdiff）的 `diffLines`/`diffWords` 输出**具体分块**由 Myers 算法 + 特定 tie-break 决定。Rust `similar` 的默认算法与 jsdiff 不同，可能产生不同分块。需要逐字节一致时，建议移植 jsdiff 的 `diffLines` 实现（含 `newlineIsToken`、`ignoreWhitespace` 等选项语义）。
9. **`grok-mermaid`（Mermaid → Unicode 图）**：0.2.3 是外部包，内部实现（布局、字符集、边路由）必须逐步复刻；`mermaid.test.ts` 有 8 条含快照的断言。
10. **`radiusShimmer` 的 UTF-16 切片语义**：`label.slice(text.length)` 按 UTF-16 单元切；`for...of` 按码点遍历。混用时含 emoji 的 label 会错位。Rust 必须显式实现 `js_slice_utf16`。
11. **`JSON.stringify` 的键序与 `undefined` 省略**：`ProgramStatusReporter` 的去重键、参数预览、崩溃日志都依赖它。`serde_json` 需 `preserve_order` + `skip_serializing_if`，并注意顶层 `undefined` 与 `null` 的区别。
12. **`toFixed` / `toLocaleString` / `Date.toLocaleString`**：见 §6.4。`formatToolDuration` 的 `4.2s`、footer 的 token 数分组、`/session` 的时间戳都可能因舍入/本地化不同而失配。
13. **`process.platform` 分支**：`darwin`（`Cmd+click`、`option` 键名）、`win32`（不做 SIGTERM/SIGHUP、外部编辑器 `shell: true`、`Suspend to background is not supported on Windows`、tmux 提示不显示）。Rust 用 `cfg!` 后必须在 CI 中覆盖三平台（至少逻辑分支单测）。
14. **`AbortController` 的精确语义**：登录对话框的 `signal` 被传给 provider 的 OAuth 流；`cancel()` 可重入且会重复 `onComplete`。`tokio_util::CancellationToken` 是“一次性”的，语义近似但**不会**在 `abort()` 后再触发监听者；需确认所有监听点都只关心“是否已取消”。
15. **`setInterval(...).unref?.()`**：Node 的 unref 让定时器不阻止进程退出。Rust 的 tokio 任务在 runtime drop 时被取消，但**主动** `exit()` 前的动画任务需要显式 `abort()`，否则可能在退出路径上多渲染一帧（影响 `/quit` 的最后输出）。
16. **`box`/`container` 的 `children` 原地替换**：`login-dialog.replaceInputWithSubmittedText`、`SelectSubmenu.applyFilter`、`radius-login-selector` 都直接改 `children[i]`。Rust 用 `Vec<Box<dyn Component>>` 按索引赋值即可，但 `render` 期间不能可变借用——需在 `render` 前完成替换（TS 也是这么做的，注意顺序）。
17. **OSC 133 区域标记**：`assistant-message`/`user-message` 在**不含 tool call** 的助手消息上插入 `\x1b]133;A/B/C` 标记，且要把“结束标记从行尾移开以保持高度稳定”（`keeps user message height stable while moving closing OSC markers off line end`）。这是与 tui-core 渲染器协同的细节。
18. **`chalk` 的 `dim` vs `theme.fg("dim")`**：前者是 SGR 2（faint 属性），后者是具体颜色。`system-theme.test.ts` 有 `renders faint tokens with SGR 2 and closes it`，说明两套机制都要有。
19. **`tests` 的快照**：现有测试大量使用 `stripAnsi` 后再断言。移植时应**同时**保留原始 ANSI 快照，否则颜色回归无法发现。
20. **`/debug` 与崩溃日志的路径**：`getDebugLogPath()` 必须为 `<agent-dir>/kiana-debug.log`；`crashes.json` 的数组截断与 `notified` 标记必须逐字。

### 8.4 推荐实施顺序

1. `theme/*`（含颜色数学、schema、system 生成、controller）——所有渲染的基础。
2. `components/keybinding-hints`、`dynamic-border`、`themed-text`、`markdown-transform`——最基础可见单元。
3. `program-status-reporter`、`model-search`、`model-catalog-refresh`、`chat-viewport`、`external-editor`、`tui-renderer`——无 UI 依赖的小模块，可先跑通测试。
4. 消息组件（`assistant-message`、`user-message`、`bash-execution`、`branch/compaction/skill`）与 `tool-execution`。
5. 选择器族（`model-selector`、`scoped-models-selector`、`settings-selector` + `settings-submenu`、`oauth-selector`、`login-dialog`、`radius-login-selector`、`thinking-selector`、`trust-selector`、`user-message-selector`、`show-images`、`theme-selector`）。
6. 大组件（`session-selector` + `session-selector-search`、`tree-selector`、`config-selector`、`footer`）。
7. `bug-report`、`session-share`。
8. `interactive-mode.ts` 逐块移植（按 §2.50.1 → §2.50.21 的顺序）。
9. 彩蛋（`armin`、`easter-egg-3d`、`kiana-logo`、`first-time-setup`、`earendil-announcement`）。

---

## 附录 A：`theme/dark.json` 与 `theme/light.json`（逐字）

> 以下内容必须与 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/modes/interactive/theme/dark.json` 和 `light.json` **字节一致**（含 tab 缩进、`$schema` 行、末尾无换行差异）。建议以 `include_str!` 引入并以 SHA-256 校验。

### A.1 `dark.json`（78 行）

```json
{
	"$schema": "https://kiana.dev/schemas/theme.schema.json",
	"name": "dark",
	"appearance": "dark",
	"vars": {
		"text": "okhsl(234 3% 89%)",
		"muted": "okhsl(229 6% 67%)",
		"violet": "okhsl(295 50% 67%)",
		"blue": "okhsl(232 54% 67%)",
		"green": "okhsl(159 59% 67%)",
		"red": "okhsl(20 72% 67%)",
		"yellow": "okhsl(83 88% 67%)",
		"blueBg": "okhsl(233 41% 24%)"
	},
	"colors": {
		"accent": "violet",
		"border": "okhsl(231 57% 65%)",
		"borderAccent": "okhsl(295 53% 64%)",
		"borderMuted": "okhsl(229 8% 53%)",
		"success": "green",
		"error": "red",
		"warning": "yellow",
		"muted": "muted",
		"dim": "okhsl(229 8% 56%)",
		"text": "text",
		"thinkingText": "okhsl(226 7% 65%)",
		"selectedBg": "blueBg",
		"scrollbarTrack": "okhsl(237 7% 33%)",
		"scrollbarThumb": "okhsl(232 7% 65%)",
		"searchMatchBg": "okhsl(53 51% 24%)",
		"searchMatchText": "muted",
		"userMessageBg": "blueBg",
		"userMessageText": "text",
		"customMessageBg": "okhsl(295 42% 24%)",
		"customMessageText": "muted",
		"customMessageLabel": "violet",
		"toolPendingBg": "okhsl(229 5% 24%)",
		"toolSuccessBg": "okhsl(158 46% 25%)",
		"toolErrorBg": "okhsl(19 54% 25%)",
		"toolTitle": "text",
		"toolOutput": "muted",
		"mdHeading": "yellow",
		"mdLink": "blue",
		"mdLinkUrl": "muted",
		"mdCode": "violet",
		"mdCodeBlock": "green",
		"mdCodeBlockBorder": "muted",
		"mdQuote": "muted",
		"mdQuoteBorder": "muted",
		"mdHr": "muted",
		"mdListBullet": "violet",
		"toolDiffAdded": "green",
		"toolDiffRemoved": "red",
		"toolDiffContext": "muted",
		"syntaxComment": "muted",
		"syntaxKeyword": "blue",
		"syntaxFunction": "yellow",
		"syntaxVariable": "okhsl(202 58% 67%)",
		"syntaxString": "okhsl(52 67% 67%)",
		"syntaxNumber": "green",
		"syntaxType": "violet",
		"syntaxOperator": "muted",
		"syntaxPunctuation": "muted",
		"thinkingOff": "okhsl(229 8% 49%)",
		"thinkingMinimal": "okhsl(232 20% 52%)",
		"thinkingLow": "okhsl(232 45% 54%)",
		"thinkingMedium": "okhsl(263 59% 56%)",
		"thinkingHigh": "okhsl(295 73% 59%)",
		"thinkingXhigh": "okhsl(337 81% 61%)",
		"thinkingMax": "okhsl(20 99% 63%)",
		"bashMode": "okhsl(159 64% 65%)"
	},
	"export": {
		"pageBg": "okhsl(262 14% 16%)",
		"cardBg": "okhsl(264 13% 19%)",
		"infoBg": "okhsl(53 51% 24%)"
	}
}
```

### A.2 `light.json`（78 行）

```json
{
	"$schema": "https://kiana.dev/schemas/theme.schema.json",
	"name": "light",
	"appearance": "light",
	"vars": {
		"text": "okhsl(225 5% 27%)",
		"muted": "okhsl(229 8% 47%)",
		"violet": "okhsl(295 60% 46%)",
		"blue": "okhsl(231 68% 47%)",
		"green": "okhsl(159 75% 46%)",
		"red": "okhsl(20 91% 47%)",
		"yellow": "okhsl(83 99% 47%)",
		"blueBg": "okhsl(235 19% 91%)"
	},
	"colors": {
		"accent": "violet",
		"border": "okhsl(231 67% 55%)",
		"borderAccent": "okhsl(295 59% 55%)",
		"borderMuted": "okhsl(235 7% 66%)",
		"success": "green",
		"error": "red",
		"warning": "yellow",
		"muted": "muted",
		"dim": "okhsl(229 7% 59%)",
		"text": "text",
		"thinkingText": "okhsl(234 8% 55%)",
		"selectedBg": "blueBg",
		"scrollbarTrack": "okhsl(248 3% 90%)",
		"scrollbarThumb": "okhsl(226 7% 65%)",
		"searchMatchBg": "okhsl(56 22% 91%)",
		"searchMatchText": "muted",
		"userMessageBg": "blueBg",
		"userMessageText": "text",
		"customMessageBg": "okhsl(295 25% 91%)",
		"customMessageText": "muted",
		"customMessageLabel": "violet",
		"toolPendingBg": "okhsl(248 3% 91%)",
		"toolSuccessBg": "okhsl(156 21% 91%)",
		"toolErrorBg": "okhsl(24 23% 91%)",
		"toolTitle": "text",
		"toolOutput": "muted",
		"mdHeading": "yellow",
		"mdLink": "blue",
		"mdLinkUrl": "muted",
		"mdCode": "violet",
		"mdCodeBlock": "green",
		"mdCodeBlockBorder": "muted",
		"mdQuote": "muted",
		"mdQuoteBorder": "muted",
		"mdHr": "muted",
		"mdListBullet": "violet",
		"toolDiffAdded": "green",
		"toolDiffRemoved": "red",
		"toolDiffContext": "muted",
		"syntaxComment": "muted",
		"syntaxKeyword": "blue",
		"syntaxFunction": "yellow",
		"syntaxVariable": "okhsl(203 73% 46%)",
		"syntaxString": "okhsl(52 84% 46%)",
		"syntaxNumber": "green",
		"syntaxType": "violet",
		"syntaxOperator": "muted",
		"syntaxPunctuation": "muted",
		"thinkingOff": "okhsl(223 5% 80%)",
		"thinkingMinimal": "okhsl(229 14% 78%)",
		"thinkingLow": "okhsl(232 33% 76%)",
		"thinkingMedium": "okhsl(264 48% 74%)",
		"thinkingHigh": "okhsl(295 62% 72%)",
		"thinkingXhigh": "okhsl(337 74% 70%)",
		"thinkingMax": "okhsl(20 98% 68%)",
		"bashMode": "okhsl(159 74% 55%)"
	},
	"export": {
		"pageBg": "okhsl(17 3% 94%)",
		"cardBg": "okhsl(17 5% 97%)",
		"infoBg": "okhsl(56 22% 91%)"
	}
}
```

> 两个文件的 `colors` 均含 **56 个 token**（与 `THEME_COLORS` 顺序一致）；`export` 各 3 个。除 `scrollbarTrack`/`scrollbarThumb`/`searchMatchBg`/`searchMatchText`/`thinkingMax` 之外，其余 token 都是**必需**的（5 个可选项的 fallback 见 §2.2）。

---

## 附录 B：文档校对点

移植时以下文档是文案与语义的**第二来源**，出现分歧时以源码为准、以文档为提示：

| 文档 | 校对内容 |
|---|---|
| `packages/coding-agent/docs/keybindings.md` | 全部默认键位表（`tui.*` / `app.*`）与键名语法（`ctrl+`、`shift+`、`alt+`、`super+`、`/` 表示多键，如 `"ctrl+a/ctrl+b"`）；§3.3 的表格须与之逐行一致 |
| `packages/coding-agent/docs/themes.md` | 主题文件的结构（`$schema`/`name`/`appearance`/`vars`/`colors`/`export`）、6 种颜色写法（`#rrggbb`、`#rgb`、`rgb()`、`hsl()`、`okhsl()`、`oklch()`、变量引用）、5 个可选 token 的 fallback、token 分组 |
| `packages/coding-agent/docs/tui.md` | TUI 模式（regular/fullscreen）、滚动、选择与复制、鼠标行为 |
| `packages/coding-agent/docs/terminal-setup.md` | 终端能力探测与图片协议（kitty/iTerm2/sixel） |
| `packages/coding-agent/docs/tmux.md` | tmux 下的启动提示文案与 `set -g allow-passthrough on` 建议 |
| `packages/coding-agent/docs/slash-commands.md` | 内置命令表（与 §3.1 对照） |
| `packages/coding-agent/docs/settings.md` | 设置项名称与取值范围（与 §4.2.5 对照） |

---

## 附录 C：本规范的自检清单

移植完成后，用以下清单逐项验证与 TS 版**功能等价**：

- [ ] `dark.json` / `light.json` 字节一致（SHA-256 相同）
- [ ] 56 个 token 全部解析成功；5 个可选 token 缺省时 fallback 正确
- [ ] `system` 主题由终端颜色生成，且排在主题列表**第一位**、无 `export` 颜色
- [ ] 三种颜色模式（truecolor / 256 / 16）下所有 `fg`/`bg` 输出的 ANSI 字节一致
- [ ] 全部键位默认值与 `keybindings.json` 覆盖/迁移规则一致
- [ ] `/hotkeys` 输出与 §2.50.16 模板逐字一致
- [ ] 全部斜杠命令的分派顺序与参数切片与 §3.2 一致
- [ ] 全部 spinner 帧、loader 文案、倒计时文案与 §3.4 一致
- [ ] 全部错误串与 §3.8 索引一致
- [ ] OSC 7501 上报格式（含 base64、2048 字节截断、`app` 校验）逐字节一致
- [ ] OSC 8 超链接两种终止符都支持
- [ ] OSC 9;4 进行中/清除序列一致
- [ ] `kiana-debug.log` 首行与 `crashes.json` 格式一致
- [ ] `mcp.json` 写入保留原缩进与其它字段
- [ ] `gh` / `trash` / 外部编辑器的 argv 与错误串一致
- [ ] 安装遥测 URL、UA、5s 超时、`KIANA_OFFLINE` 短路一致
- [ ] Radius 上传的 URL 查询串顺序、头、body 字节一致
- [ ] `formatToolCallWithArgs` 折叠 100 字符阈值与展开的多行缩进一致
- [ ] 工具结果折叠预览 10 行 + `... (N more lines, <key> to expand)` 逐字一致
- [ ] 全部组件在 `outputPad` 0/1 下的前导空格行为一致
- [ ] 全屏模式的复制提示（`Copied!` vs 状态行）一致
- [ ] Windows/macOS/Linux 三平台分支文案一致

---

---

## Audit additions（对抗式审计补充）

> 本节由完整性审计追加。上面各节已就地修正的错误不再重复；这里收纳**结构性缺口**（整块缺失、分散在多处、或放到 §2.50.x 会打乱顺序的内容）。
> 核对方法：对 `interactive-mode.ts`（7 105 行）逐行通读 + AST 级方法清单比对（`^\t(private |public |...)?name(` 共 **209** 个方法，旧版规范覆盖 191 个，缺 18 个），并对全部 64 个源文件、90 个相关测试、6 篇文档逐份过。

### Audit-A.1 `InteractiveMode` 中旧版规范**一字未提**的 18 个方法（必须实现）

按源码行号顺序。省略这些不会报错，但会导致**可观察行为缺失**（启动顺序、扩展能力、错误恢复）。

| 方法（行号） | 行为（逐字要点） |
|---|---|
| `getAutocompleteSourceTag(sourceInfo?)`（678） | 返回补全描述前缀标签：`!sourceInfo \|\| sourceInfo.source === "builtin"` → `undefined`；`scopePrefix = scope==="user" ? "u" : scope==="project" ? "p" : "t"`；`source === "auto"\|"local"\|"cli"` → 仅 `scopePrefix`；`source.startsWith("npm:")` → `` `${scopePrefix}:${source}` ``；`parseGitUrl(source)` 成功 → `` `${scopePrefix}:git:${host}/${path}${ref ? "@"+ref : ""}` ``；其它 → `scopePrefix` |
| `prefixAutocompleteDescription(description?, sourceInfo?)`（704） | 无 tag → 原样返回 `description`；有 tag → `` description ? `[${tag}] ${description}` : `[${tag}]` `` |
| `createBaseAutocompleteProvider()`（727） | 组装 4 类命令：① `BUILTIN_SLASH_COMMANDS`（**浅拷贝**到 `SlashCommand`，仅 `name/description/argumentHint`）；给 `model` 挂 `getArgumentCompletions`（来源 `scopedModels` 或 `modelRuntime.getAvailableSnapshot()`，空 → `null`，`label = provider/id`，走 `createFuzzyAutocompleteItems` + `getModelSearchText`）；给 `thinking` 挂（`getAvailableThinkingLevels()`）；给 `login` 挂（`getLoginProviderCompletionOptions(this.getLoginProviderOptions())` + `getLoginProviderSearchText` + `formatLoginProviderCompletionDescription`）。② `session.promptTemplates` → `SlashCommand`（描述经 `prefixAutocompleteDescription`）。③ `extensionRunner.getRegisteredCommands()` **过滤掉与内置同名的**（`builtinCommandNames`），`name` 用 **`invocationName`**。④ 若 `settingsManager.getEnableSkillCommands()`：遍历 `resourceLoader.getSkills().skills`，`commandName = skill:${skill.name}`，同时写入 `this.skillCommands.set(commandName, skill.filePath)`。最后 `new CombinedAutocompleteProvider([...slash, ...template, ...extension, ...skill], cwd, this.fdPath)` |
| `setupAutocompleteProvider()`（827） | 依次套用 `autocompleteProviderWrappers`；收集每个 wrapper 的 `provider.triggerCharacters` 去重（`[...new Set(...)]`）后写回；存入 `this.autocompleteProvider`，并同时 `setAutocompleteProvider` 到 `defaultEditor` 与（若不同）`this.editor` |
| `showStartupNoticesIfNeeded()`（845） | 幂等（`startupNoticesShown`）；无 `changelogMarkdown` → 直接返回；chat 非空先加 `Spacer(1)`；`DynamicBorder()`；`getCollapseChangelog()` 为真 → `Text(`Updated to v${latestVersion}. Use ${theme.bold("/changelog")} to view full changelog.`, 1, 0)`（`latestVersion` 由 `/##\s+\[?(\d+\.\d+\.\d+)\]?/` 从 changelog 抓，抓不到用 `this.version`）；否则 `ThemedText(theme.bold(theme.fg("accent", "What's New")))` + `Spacer(1)` + `Markdown(changelogMarkdown.trim(), 1, 0, getMarkdownThemeWithSettings())` + `Spacer(1)`；最后 `DynamicBorder()` |
| `getCompactDisplayPathSegments(resourcePath)`（1519） | `formatDisplayPath(p).replace(/\\/g,"/").split("/").filter(s => s.length > 0 && s !== "~")`（**过滤掉 `~` 段**，这是与 `formatDisplayPath` 的关键差异） |
| `applyFullscreenScrollbarSetting()`（2050） | `this.transcriptScrollView?.setScrollbar(this.settingsManager.getFullscreenScrollbar())`（`transcriptScrollView` 可能尚未创建，必须 optional） |
| `handleFatalRuntimeError(prefix, error): Promise<never>`（2120） | `showError(`${prefix}: ${message}`)` → 若有 `getCrashExtensionHint(error)` 加一行 `warning` 色 ThemedText（`outputPad`）→ 若 `recordCrash("fatal_error", error)` 再加一行 `muted` 色的 `crashReportInstructions()` → `stopThemeWatcher()` → `this.stop("transcript")` → `process.exit(1)`。**永不返回** |
| `setEditorWorkingStatusIndicator(indicator)`（2307） | `defaultEditor.setWorkingStatusIndicator(undefined)`；若 `!isEmbeddedStatus(this.editor)` 返回 `false`；否则 `this.editor.setWorkingStatusIndicator(indicator)` 返回 `true`（即"状态指示器能否嵌进编辑器边框"的唯一判定点） |
| `renderWidgetContainer(container, widgets, spacerWhenEmpty, leadingSpacer)`（2498） | `container.clear()`；`widgets.size === 0` → 仅当 `spacerWhenEmpty` 时 `addChild(Spacer(1))` 并返回；否则若 `leadingSpacer` 先加 `Spacer(1)`，再按 Map **插入顺序** `addChild` 每个组件。调用方：`renderWidgets()` → above `(container, map, true, true)`、below `(container, map, false, false)` |
| `rebindExtensionTerminalInputListeners()`（2603） | 对每个订阅：`unsubscribe()` 后用 **新 renderer** 重新 `this.ui.addInputListener(handler)` 并写回 `subscription.unsubscribe`。`switchTuiMode()` 必须调用它，否则切模式后扩展终端输入失效 |
| `createProjectTrustContext(cwd)`（2618） | 返回 `{ cwd, mode: "tui", hasUI: true, ui: { select, confirm, input, notify } }`（**只暴露 4 个方法**，其余从 `ExtensionUIContext` 里挑） |
| `promptForMissingSessionCwd(error)`（2767） | `showExtensionConfirm("Session cwd not found", formatMissingSessionCwdPrompt(error.issue))`；确认 → `error.issue.fallbackCwd`，取消 → `undefined` |
| `getUserMessageText(message)`（3764） | 非 user → `""`；`content` 为字符串 → 直接；否则 `filter(c => c.type === "text")`；`map(c => c.text).join("")`（**不带分隔符**，多 text 块直接相连） |
| `rebuildChatFromMessages()`（4252） | `chatContainer.clear()` + `renderSessionEntries(sessionManager.buildContextEntries())`。调用点：`/settings` 里 `onShowCacheMissNoticesChange`（要重算 cache miss 通知）、`/reload` 的 `beforeSessionStart` 回调（`restoreChatBeforeSessionStart`） |
| `updateThinkingBlockVisibility()`（4572） | 遍历 `chatContainer.children`，对 `instanceof AssistantMessageComponent` 调 `setHideThinkingBlock(this.hideThinkingBlock)`；再 `requestRender()`。调用点：`toggleThinkingBlockVisibility()`、`onHideThinkingBlockChange` |
| `handleResumeSession(sessionPath, options?): Promise<{cancelled}>`（5747） | `clearStatusIndicator()` → `runtimeHost.switchSession(path, { withSession, projectTrustContextFactory: cwd => createProjectTrustContext(cwd) })`；`result.cancelled` 直接返回；否则 `showStatus("Resumed session")`。捕获 `MissingSessionCwdError` → `promptForMissingSessionCwd`：取消 → `showStatus("Resume cancelled")` + `{cancelled:true}`；确认 → 带 `cwdOverride` 重试成功 → `showStatus("Resumed session in current cwd")`。其它错误 → `handleFatalRuntimeError("Failed to resume session", error)` |
| `getAppKeyDisplay(action)`（6794）/ `getEditorKeyDisplay(action)`（6801） | 二者都只是 `keyDisplayText(action)` 的薄封装；用于 `/hotkeys` 文案（**App 与 Editor 两套 keybinding 命名空间**不要混用） |

### Audit-A.2 `ExtensionUIContext` 的完整成员与"实现体"（旧版只给了名字）

旧版 §2.50.18 给的是 28 个成员的**签名**（原文误写 32），但多个成员的真实实现有语义细节，缺了会行为不一致：

- `setWorkingMessage(message)`：写 `this.workingMessage`；若当前 `activeStatusIndicator?.kind === "working"` 立即 `setMessage(message ?? this.defaultWorkingMessage)`（**不 requestRender**；下一帧自然重绘）。
- `setWorkingVisible(visible)`：写 `workingVisible`；`false` → `clearStatusIndicator("working")` + `requestRender()` 并**直接 return**；`true` → 仅当 `session.isStreaming && activeStatusIndicator?.kind !== "working"` 才 `showWorkingStatusIndicator()`；最后 `requestRender()`。
- `setWorkingIndicator(options)`：保存 `workingIndicatorOptions`；若当前是 working 指示器则 `setIndicator(options)`；`requestRender()`。
- `setHiddenThinkingLabel(label)`：`hiddenThinkingLabel = label ?? defaultHiddenThinkingLabel`；遍历 chat 里所有 `AssistantMessageComponent` 与 `streamingComponent` 调 `setHiddenThinkingLabel`；`requestRender()`。
- `setTitle(title)`：**直接** `this.ui.terminal.setTitle(title)`（不走 `updateTerminalTitle`，即扩展设置后不会被会话名覆盖，直到下次 `session_info_changed`）。
- `pasteToEditor(text)`：**模拟 bracketed paste**：`this.editor.handleInput("\x1b[200~" + text + "\x1b[201~")`（不是 `setText`，会经过编辑器的粘贴逻辑）。
- `getEditorText()`：`this.editor.getExpandedText?.() ?? this.editor.getText()`（**优先展开态**）。
- `addAutocompleteProvider(factory)`：append 到 `autocompleteProviderWrappers` **并立即** `setupAutocompleteProvider()` 重建。
- `getEditorComponent()`：返回 `editorComponentFactory`（**不是**当前实例）。
- `setTheme(themeOrName)`：`instanceof Theme` → `themeController.setThemeInstance(...)` 且**不写设置**；字符串 → `setThemeName(...)`，`success` 且设置不同才 `settingsManager.setTheme(...)`；返回 `result`。
- `theme` 是 **getter**（`get theme() { return theme; }`），不是字段。
- `setToolsExpanded(expanded)` → `this.setToolsExpanded(expanded)`（**同名转发**，注意别写成无限递归的 `this.setToolsExpanded = ...`）。

### Audit-A.3 启动/重绑顺序（旧版最容易搞错的一处）

- `init()` 里 **`renderInitialMessages()` 在 `rebindCurrentSession()` 之后**（原注释：`// Initialize extensions first so resources are shown before messages`）。
- `bindCurrentSessionExtensions()` **末尾**调用 `showLoadedResources({ force: false, showDiagnosticsWhenQuiet: true })`。
- `showStartupNoticesIfNeeded()`（changelog）的调用点在 **`bindCurrentSessionExtensions()`** 内、`showLoadedResources` 附近（不在 `init()` 主流程里直接调）。
- `rebindCurrentSession({ renderBeforeBind: true })`（用于 `/reload`、切换会话）：**先** `renderCurrentSessionState()` + `subscribeToAgent()`，**再** `bindCurrentSessionExtensions()`；结束后还要 `updateAvailableProviderCount()`、`updateEditorBorderColor()`、`updateTerminalTitle()`。
- 这条顺序链条由 `5943-session-start-notify.test.ts` 的 7 条测试逐条钉死（见 §7.3b）。

### Audit-A.4 数据/渲染细节（补充）

- `renderWidgetContainer` 的 above 容器在**空时也占 1 行 Spacer**（`spacerWhenEmpty=true`），below 容器空时**不占行**——这决定了编辑器上方始终有一行留白。
- `MAX_WIDGET_LINES = 10` 是 `InteractiveMode` 的 **static** 字段（`private static readonly`），不是模块常量。
- `showManagedToolStatus()` 与 `showStatus()` 互斥：`showManagedToolStatus` 末尾把 `lastStatusSpacer`/`lastStatusText` **置 undefined**，使后续 `showStatus` 不复用上一行（即"托管工具状态打断了状态去重链"）。
- `/debug` 的文件名依赖 `APP_NAME`（派生发行版会变）；`updateTerminalTitle` 依赖 **`APP_TITLE`**（派生发行版会变，默认 `π`）。两者**不是同一个常量**。
- `registerSignalHandlers()` 的 stdin 错误监听**必须挂在 `process.stdin` 上**：终端死掉后 `setRawMode`/`read` 抛 `EIO`/`ENOTTY`，若无监听会变成 `uncaughtException`，测试 `stdin %s exits quietly without recording a crash` 断言这类错误**不写崩溃日志**。
- `escape`/`q` 关闭 3D 动画、`tui.select.cancel` 取 `getKeys(...)[0] ?? "escape"` 见 §2.47。

### Audit-A.5 文档校对补遗

- `docs/tui.md` 描述的全屏布局与 `chat-viewport.ts` 的 `createChatViewport({...})` 参数一一对应（`scrollbar`、`scrollbarTrackStyle`、`scrollbarThumbStyle` 三个由 `settingsManager.getFullscreenScrollbar()` 与 theme 决定）；`docs/themes.md` 的 56 token 表与 `theme-tokens.ts` 一致。
- `docs/terminal-setup.md` 的"能力覆盖"小节与 §4.1 新增的 `KIANA_TRUE_COLOR`/`KIANA_HYPERLINKS`/`KIANA_IMAGE_PROTOCOL` 一致；`KIANA_HARDWARE_CURSOR` 文档写 `1`，与源码 `=== "1"` 相符。
- `docs/tmux.md` 只提到 `extended-keys`，**未**提 `extended-keys-format`；源码两条都查（见 §2.50.4 修正后的 tmux 段）。
- `docs/keybindings.md` 未列出扩展快捷键（`extensionRunner.getShortcuts()`）的匹配优先级——源码里扩展快捷键在 `defaultEditor.onExtensionShortcut` 中**先于**编辑器自身键位处理（`matchesKey(data, shortcutStr)` 命中即 `return true`）。

### Audit-A.6 选择器接线（`showTreeSelector` / `showUserMessageSelector` / `showSessionSelector` / `handleCloneCommand`）——旧版只有单行"命令 → 方法"映射

这四个方法承载了 `/fork`、`/tree`、`/resume`、`/clone` 与 `app.session.*` 键位的**全部可观察行为**，旧版规范只写了命令名与少数状态串。逐字补全：

**`showUserMessageSelector()`**（5496–5532 行）：
```ts
const userMessages = this.session.getUserMessagesForForking();
if (userMessages.length === 0) { this.showStatus("No messages to fork from"); return; }
const initialSelectedId = userMessages[userMessages.length - 1]?.entryId;   // 默认选中最新一条
this.showSelector((done) => {
    const selector = new UserMessageSelectorComponent(
        userMessages.map((m) => ({ id: m.entryId, text: m.text })),
        async (entryId) => {
            done();
            try {
                const result = await this.runtimeHost.fork(entryId);
                if (result.cancelled) { this.ui.requestRender(); return; }
                this.editor.setText(result.selectedText ?? "");
                this.showStatus("Forked to new session");
            } catch (error) { this.showError(error instanceof Error ? error.message : String(error)); }
        },
        () => { done(); this.ui.requestRender(); },
        initialSelectedId,
    );
    return { component: selector, focus: selector.getMessageList() };
});
```
注意：传给组件的 `text` **不带 `entryId` 之外的任何字段**（`timestamp` 未传）；`focus` 是 `getMessageList()`。

**`handleCloneCommand()`**（5534–5553 行）：
```ts
const leafId = this.sessionManager.getLeafId();
if (!leafId) { this.showStatus("Nothing to clone yet"); return; }
try {
    const result = await this.runtimeHost.fork(leafId, { position: "at" });
    if (result.cancelled) { this.ui.requestRender(); return; }
    this.editor.setText("");                       // 注意：清空编辑器，不是写入 selectedText
    this.showStatus("Cloned to new session");
} catch (error) { this.showError(...); }
```
`/clone` 用 **`position: "at"`**（在叶节点"处"复制），而 `showUserMessageSelector` 的 fork **不传 position**。

**`showTreeSelector(initialSelectedId?)`**（5555–5699 行）：
```
tree = sessionManager.getTree(); realLeafId = sessionManager.getLeafId();
initialFilterMode = settingsManager.getTreeFilterMode();
if (tree.length === 0) { showStatus("No entries in session"); return; }
new TreeSelectorComponent(tree, realLeafId, this.ui.terminal.rows, onSelect, onCancel, onLabelChange, initialSelectedId, initialFilterMode)
```
`onSelect(entryId)` 的**完整流程**（顺序敏感）：
1. `entryId === sessionManager.getLeafId()` → `done()` + `showStatus("Already at this point")` + return。
2. `done()`（**先**关闭选择器）。
3. 若 `!settingsManager.getBranchSummarySkipPrompt()`：循环 `showExtensionSelector("Summarize branch?", ["No summary", "Summarize", "Summarize with custom prompt"])`：
   - `undefined`（Esc）→ `showTreeSelector(entryId)` 重新打开并**保持同一选中**，return；
   - `wantsSummary = summaryChoice !== "No summary"`；
   - `"Summarize with custom prompt"` → `customInstructions = await showExtensionEditor("Custom summarization instructions")`，`undefined` → `continue`（回到选择器）。
4. `session.isStreaming` → `restoreQueuedMessagesToEditor()` + `await session.abort()`。
5. **再次**检查 `session.isCompacting` → `showError("Wait for the current compaction or tree navigation to finish before navigating the session tree.")` 并 return。
6. `wantsSummary` 时：`defaultEditor.onEscape = () => session.abortBranchSummary()`；`chatContainer.addChild(new Spacer(1))`；`showStatusIndicator(new BranchSummaryStatusIndicator(ui))`。
7. `result = await session.navigateTree(entryId, { summarize: wantsSummary, customInstructions })`：
   - `result.aborted` → `showStatus("Branch summarization cancelled")` + `showTreeSelector(entryId)`；
   - `result.cancelled` → `showStatus("Navigation cancelled")`；
   - 成功 → `chatContainer.clear()` + `renderInitialMessages()`；`result.editorText && !editor.getText().trim()` 时 `editor.setText(result.editorText)`；`showStatus("Navigated to selected point")`；`void flushCompactionQueue({ willRetry: false })`。
8. `catch` → `showError(msg)`；`finally` → 若显示过指示器 `clearStatusIndicator("branchSummary")`，恢复 `defaultEditor.onEscape = originalOnEscape`。
`onLabelChange(entryId, label)` → `sessionManager.appendLabelChange(entryId, label)` + `requestRender()`。
`selector.onCopy = async (text) => { if (!text) { showError("Selected entry has no text to copy"); return; } try { await copyToClipboard(text); showStatus("Copied selected message to clipboard") } catch (e) { showError(...) } }`。
`onCancel` → `done()` + `requestRender()`。

**`showSessionSelector()`**（5704–5742 行）：`new SessionSelectorComponent(currentLoader, allLoader, onSelect, onCancel, onExit, requestRender, options, currentSessionFilePath)`，其中：
- `currentLoader = (onProgress, signal) => SessionManager.list(cwd, sessionDir, onProgress, signal)`
- `allLoader = (onProgress, signal) => usesDefaultSessionDir() ? SessionManager.listAll(onProgress, signal) : SessionManager.listAll(sessionDir, onProgress, signal)`
- `onSelect(sessionPath)` → `done()` + `await handleResumeSession(sessionPath)`
- `onExit` → `void this.shutdown()`（**不是** cancel；resume 选择器里的"退出"直接关会话）
- `options = { renameSession: async (path, nextName) => { const next = (nextName ?? "").trim(); if (!next) return; const mgr = SessionManager.open(path); mgr.appendSessionInfo(next); }, showRenameHint: true, keybindings: this.keybindings }`
- 末参 `currentSessionFilePath = sessionManager.getSessionFile()`（用于列表里高亮/标记当前会话）
- `focus: selector`（**不是**内部 list）

**`bindCurrentSessionExtensions()` 的 `commandContextActions`**（1977–2035 行，扩展可调用的会话操作）：`newSession`（失败 → `handleFatalRuntimeError("Failed to create session", error)`）、`fork`（成功且未取消 → `editor.setText(result.selectedText ?? "")` + `showStatus("Forked to new session")`；失败 → `handleFatalRuntimeError("Failed to fork session", error)`）、`navigateTree`（成功 → `chatContainer.clear()` + `renderInitialMessages()` + 可选 editorText + `showStatus("Navigated to selected point")` + `flushCompactionQueue({ willRetry: false })`）、`switchSession`（→ `handleResumeSession`）、`reload`（→ `handleReloadCommand`）。这三个 `handleFatalRuntimeError` 的 **prefix 是用户可见文案**，必须逐字。

### Audit-A.7 TUI 挂载/切换的精确规则（旧版只在 §1 一句话带过）

**`mountInteractiveTui(tui, components)`**（875–882 行）：
```ts
for (const component of components) tui.addChild(component);
if (TuiLayouts.isViewportTUI(tui)) {
    if (!this.fullscreenLayoutRoot) throw new Error("Fullscreen layout is not initialized");
    tui.setLayoutRoot(this.fullscreenLayoutRoot);
}
```
`fullscreenLayoutRoot` 由 `createChatViewport(...).root` 提供；**fullscreen 模式下没初始化 layout root 是硬错误**（抛异常，不是降级）。

**`stopInteractiveTui(fullscreenExitOutput)`**（884–891 行）：
```ts
if (this.renderer.mode === "fullscreen" && fullscreenExitOutput === "transcript") {
    while (this.renderer.hasOverlayEntries) this.renderer.hideOverlay();   // 逐个关闭，不是一次清空
    this.switchTuiMode("regular", false, false);
    this.renderer.renderNow();
}
this.ui.stop({ preserveScreen: this.renderer.mode === "fullscreen" });
```

**`switchTuiMode(mode, restoreProgress = true, startRenderer = true): boolean`**（893–941 行，返回是否成功）：
```
if (mode === previousUi.mode) return true;
if (previousUi.hasOverlayEntries) return false;          // 有 overlay 时拒绝切换（调用方负责报错文案）
components = [...previousUi.children]; focus = getFocusedComponent(); terminal = previousUi.terminal;
showHardwareCursor / clearOnShrink / onDebug 全部沿用旧 renderer
if (previousUi instanceof TuiMainScreen) this.mainScreenRenderState = previousUi.captureRenderState();
previousUi.stop({ preserveScreen: true }); setFocus(null); clear();
if (isViewportTUI(previousUi)) previousUi.setLayoutRoot(undefined);
nextUi = createInteractiveTui({ tuiMode: mode, showHardwareCursor, logDirectory: getAgentDir(), terminal, onRightClickPaste, fullscreenCopyOnSelect, fullscreenWheelScrollLines });
nextUi.setClearOnShrink(clearOnShrink); nextUi.onDebug = onDebug;
if (nextUi instanceof TuiMainScreen && mainScreenRenderState) nextUi.restoreRenderState(mainScreenRenderState);
this.renderer = nextUi; this.options.tuiMode = mode;
mountInteractiveTui(nextUi, components); nextUi.invalidate(); nextUi.setFocus(focus);
if (!startRenderer) return true;
nextUi.start();
this.themeController.rebindTui();
this.rebindExtensionTerminalInputListeners();
if (restoreProgress && getShowTerminalProgress() && (session.isStreaming || session.isCompacting)) terminal.setProgress(true);
return true;
```
要点：**同一个组件树被复用并 remount**（不是重建）；`mainScreenRenderState` 用于把 regular 模式的滚动/渲染状态带到新实例；`transcriptScrollView` / `fullscreenLayoutRoot` **不会**被 `switchTuiMode` 重建（它们属于 `createChatViewport`，只在 `init()` 建一次）；`themeController.rebindTui()` 必须紧跟 `start()`。
