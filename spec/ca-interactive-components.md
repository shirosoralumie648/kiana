# ca-interactive-components 单元移植规格（TypeScript -> Rust）

本文件是 kiana coding-agent **交互模式（interactive TUI）组件层**的完整移植规格，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

- 单元 id：`ca-interactive-components`
- 源包：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`（`@earendil-works/kiana-coding-agent` v1.4.0，
  ESM，`engines.node >= 22.19.0`）
- 源目录（只读参考）：`packages/coding-agent/src/modes/interactive/components/`
- 目标工作区：`/home/ctyun/code/kiana_rev/kiana`

> **约定**：文中所有标识符、字段名、错误文案、用户可见字符串均按 TypeScript 源码**逐字**抄录，
> Rust 实现必须输出完全一致的文本（除另有说明）。凡标 **【逐字】** 的字符串，测试会逐字节比对。
> `\x1b` 表示 U+001B，`\x07` 表示 U+0007（BEL），`\x00` 表示 U+0000。
> `→`/`←`/`↑`/`↓`/`›`/`•`/`·`/`✓`/`○`/`◉`/`└`/`├`/`│`/`─`/`⊞`/`⊟`/`▀`/`█`/`▄`/`░`/`▒`/`▓`/
> `…`/`↑` 等全部是**真实的 UTF-8 字符**，不是 ASCII 近似。
> 中文只用于解释语义，不进入任何输出。

单元内所有组件都遵循 TUI 框架的唯一硬约束：**`render(width)` 返回的每一行可见宽度必须 ≤ width**。
主屏渲染器检测到超宽行时会写崩溃日志并抛错（错误文本由 `tui-core` 单元产生）。本单元的每个组件都必须
保证不触发它；本规格在每个文件小节里列出该组件的宽度不变式。

---

## 目录

0. 单元边界与阅读顺序
1. 概览
2. 逐文件规格（导出符号 + 行为）
3. 用户可见表面（逐字文本）
4. 外部格式（磁盘 / 环境变量 / 子进程 / 协议）
5. 跨单元接口
6. npm 依赖与 Rust crate 映射
7. 测试清单
8. Rust 模块布局与最难复现的部分
9. 附录 A/B/C（组件清单、常量速查、逐字字符串总表）

> 阅读提示：§2 的编号顺序是「首轮通读 §2.1–§2.50」→「逐行复核 §2.51–§2.60」。**§2.5x 永远覆盖 §2.1–§2.50**
> 的对应内容；两者的差异是有意保留的（便于审阅时看到修订轨迹），不要合并删除。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部 48 个，合计 11,856 行）

```
src/modes/interactive/components/
  index.ts                         37
  dynamic-border.ts                25
  themed-text.ts                   32
  markdown-transform.ts            35
  auth-url.ts                      39
  countdown-timer.ts               39
  kiana-logo.ts                       40
  keybinding-hints.ts              48
  show-images-selector.ts          50
  earendil-announcement.ts         53
  theme-selector.ts                67
  bordered-loader.ts               68
  skill-invocation-message.ts      68
  custom-entry.ts                  69
  branch-summary-message.ts        71
  compaction-summary-message.ts    72
  user-message.ts                  83
  mermaid.ts                       89
  extension-input.ts               94
  visual-truncate.ts               98
  custom-message.ts               114
  radius-login-selector.ts        114
  extension-selector.ts           117
  status-indicator.ts             123
  trust-selector.ts               134
  extension-editor.ts             141
  diff.ts                         147
  custom-editor.ts                148
  first-time-setup.ts             148
  thinking-selector.ts            154
  user-message-selector.ts        155
  assistant-message.ts            216
  bash-execution.ts               223
  oauth-selector.ts               224
  settings-submenu.ts             258
  session-selector-search.ts      260
  footer.ts                       302
  login-dialog.ts                 322
  armin.ts                        384
  scoped-models-selector.ts       403
  tool-execution.ts               417
  model-selector.ts               423
  tree-selector.ts               1428
  easter-egg-3d.ts               1292
  config-selector.ts              951
  settings-selector.ts           1009
  session-selector.ts            1045
  easter-egg-3d.lazy.ts            27
```

（上面按行数近似排序，便于排期；编号与 §2 的小节一一对应，§2 用固定顺序。）

**注意「公开面」与「内部件」的区分**（决定 Rust 侧 `pub` 可见性）：

`index.ts` **只**再导出以下符号（37 行全量）：

```ts
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

`index.ts` **不**导出（但 `interactive-mode.ts` 直接按路径 import，故必须实现）：
`CustomEntryComponent`、`EarendilAnnouncementComponent`、`createMermaidMarkdownTransformer`、
`kianaLogoLines`/`kianaWordmark`/`supportsKianaLogo`、`CountdownTimer`、`createLoginMenuSelector`、
`SelectSubmenu`/`SteppedSubmenu`、`session-selector-search.ts` 的全部导出、`createMarkdownTransform`、
`ThemedText`、`StatusIndicator` 及其四个子类、`IdleStatus`、`VisualLinePreview`、
`playKianaLogo3d`/`playArmin3d`/`playEasterEgg3d`、`isArminPixel`/`ARMIN_WIDTH`/`ARMIN_HEIGHT`、
`formatAuthSelectorProviderType`/`formatAuthSelectorProviderStatus`、`formatTokens`/`formatCwdForFooter`。

**【补全】还有一批「文件内 export、barrel 未导出」的类型/接口**（Rust 侧应 `pub` 但**不要**放进
`components::mod` 的公开再导出清单，二者要区分）：

| 文件 | 未进 barrel 的导出 |
|---|---|
| `keybinding-hints.ts` | `KeyTextFormatOptions`、`formatKeyText`（`keyDisplayText` 也**未**导出！barrel 只有 `keyHint`/`keyText`/`rawKeyHint`） |
| `extension-input.ts` | `ExtensionInputOptions` |
| `extension-selector.ts` | `ExtensionSelectorOptions` |
| `trust-selector.ts` | `TrustSelection`、`TrustSelectorOptions` |
| `oauth-selector.ts` | `AuthSelectorProvider`、`formatAuthSelectorProviderType`、`formatAuthSelectorProviderStatus` |
| `status-indicator.ts` | `StatusIndicatorKind`、`CompactionStatusReason`、`StatusIndicator`、`WorkingStatusIndicator`、`RetryStatusIndicator`、`CompactionStatusIndicator`、`BranchSummaryStatusIndicator`、`IdleStatus` |
| `visual-truncate.ts` | `VisualLinePreview`、`VisualLinePreviewOptions`（barrel 只导出 `truncateToVisualLines` + `VisualTruncateResult`） |
| `settings-submenu.ts` | `SelectSubmenuOptions`、`SteppedSubmenuStep`（两个类也未导出） |
| `mermaid.ts` | `createMermaidMarkdownTransformer`（`MermaidTransformerOptions` 是 **interface 且未 export**，见 §2.59） |
| `easter-egg-3d.ts` | `EasterEgg3d`、`playEasterEgg3d`、`EasterEgg3dAnimation` |
| `easter-egg-3d.lazy.ts` | `playKianaLogo3d`、`playArmin3d` |
| `armin.ts` | `ARMIN_WIDTH`、`ARMIN_HEIGHT`、`isArminPixel`（`ArminComponent` 已导出） |
| `scoped-models-selector.ts` | 内部纯函数全部私有（见 §2.53.2） |
| `session-selector-search.ts` | `SortMode`、`NameFilter`、`ParsedSearchQuery`、`MatchResult`、`hasSessionName`、`parseSearchQuery`、`matchSession`、`filterAndSortSessions` |
| `config-selector.ts` | `ScopedResolvedPaths`（`ConfigSelectorComponent` 已导出；`ResourceList`/`ConfigSelectorHeader` 未导出，见 §2.41） |
| `radius-login-selector.ts` | 只有 `createLoginMenuSelector`；`radiusShimmer`/`RadiusLoginMenuComponent`/`RadiusOption` 均**未**导出（§2.54.5） |
| `theme-selector.ts` / `show-images-selector.ts` | 只有组件类（`THEME_SELECT_LIST_LAYOUT`/`SHOW_IMAGES_SELECT_LIST_LAYOUT` 私有） |
| `first-time-setup.ts` | `FirstTimeSetupOptions`、`FirstTimeSetupResult`（**barrel 已导出**；签名见 §2.52.6） |
| `earendil-announcement.ts` | 只有 `EarendilAnnouncementComponent`（`BLOG_URL`/`IMAGE_FILENAME`/`loadImageBase64` 私有） |
| `themed-text.ts` | 只有 `ThemedText` |
| `kiana-logo.ts` | `kianaLogoLines`、`supportsKianaLogo`、`kianaWordmark` |
| `markdown-transform.ts` | `createMarkdownTransform` |
| `countdown-timer.ts` | `CountdownTimer` |
| `easter-egg-3d.ts`（补充） | `Model`/`ScreenCell`/`Star`/`Box`/`Pose` 均为**模块私有 interface**（`Model` 出现在 `EasterEgg3dAnimation` 构造签名里但类型未导出 → Rust 侧要么同样私有、要么 `pub(crate)`) |

### 0.2 明确**不**属于本单元（由其他单元移植，但本单元依赖其符号）

框架层（`@earendil-works/kiana-tui`，单元 **tui-core** / **tui-components** / **tui-text** / **tui-terminal**）：

- `Component`、`Focusable`、`Container`、`TUI`、`TuiMouseEvent`、`TuiMouseEventResult`、
  `TuiMainScreen`、`TuiAltScreen`、`OverlayOptions`（tui-core）。
- `Text`、`TruncatedText`、`Box`、`Spacer`、`Markdown`（含 `MarkdownTheme`/`MarkdownOptions`/
  `DefaultTextStyle`/`transform` 钩子）、`Loader`、`CancellableLoader`、`Input`、`Editor`（含
  `EditorOptions`/`EditorTheme`、`protected renderTopBorder(width, hiddenLineCount)`、
  `isShowingAutocomplete()`、`getText()`/`setText()`、`onSubmit`）、`SelectList`
  （`SelectItem`/`SelectListTheme`/`SelectListLayoutOptions`）、`SettingsList`（`SettingItem`/
  `SettingsListTheme`/`SettingsListOptions`）、`MouseRegion`、`Image`（tui-components）。
- `visibleWidth`、`truncateToWidth`、`sliceByColumn`、`sliceWithWidth`、`wrapTextWithAnsi`、
  `stripTerminalSequences`（tui-core / tui-text）。
- `getKeybindings`、`KeybindingsManager`、`TUI_KEYBINDINGS`、`Keybinding`、`KeyId`、`matchesKey`、
  `Key`（tui-core）。
- `fuzzyMatch`、`fuzzyFilter`（tui-core）。
- `hyperlink`、`imageFallback`、`getImageDimensions`、`getCapabilities`、`isAppleTerminalSession`、
  `foregroundAnsi`、`backgroundAnsi`、`rgbColor`、`indexedColor`、`mixColors`、`parseColor`、
  `colorToRgb`、`Color`（tui-terminal / tui-core）。
- `Marked`（`marked` 的再导出，`Markdown` lexer 用）。

coding-agent 其他单元：

- `modes/interactive/theme/theme.ts` 与 `theme-tokens.ts`（单元 **ca-theme**，若不存在则并入 tui 主题单元）：
  `theme` 全局代理、`Theme` 类、`getMarkdownTheme`、`getSelectListTheme`、`getEditorTheme`、
  `getSettingsListTheme`、`getAvailableThemes`、`initTheme`、`parseAutoThemeSetting`、
  `SYSTEM_THEME_NAME`、`TerminalTheme`、`THEME_TOKENS`。
- `core/keybindings.ts`（单元 **ca-cli** 或 **ca-session**）：`APP_KEYBINDINGS`、`KEYBINDINGS`、
  `AppKeybinding`、`KeybindingsManager`（coding-agent 子类，`create()` 读 `keybindings.json`）。
- `core/session-manager.ts`（单元 **ca-session**）：`SessionInfo`、`SessionTreeNode`、`SessionEntry`
  （全部 entry 变体）、`CustomEntry`、`SessionListProgress`、`SessionManager`、`ReadonlySessionManager`。
- `core/messages.ts`（单元 **ca-session**）：`BashExecutionMessage`、`CustomMessage`、
  `BranchSummaryMessage`、`CompactionSummaryMessage`、`TextContent`/`ImageContent`。
- `core/settings-manager.ts`（单元 **ca-config-models**）：`SettingsManager`、`PackageSource`、
  `WarningSettings`、`MermaidRenderingMode`、`CacheWarmingMode`、`DefaultProjectTrust`、
  `FullscreenExitOutput`、`TuiMode`、`SessionShareDestination`、`QuietStartup`、`CACHE_WARMING_MODES`。
- `core/settings-defaults.ts`：`SETTINGS_DEFAULTS`。
- `core/trust-manager.ts`（单元 **ca-cli**）：`getProjectTrustOptions`、`ProjectTrustOption`、
  `ProjectTrustStoreEntry`。
- `core/package-manager.ts`（单元 **ca-resources**）：`PathMetadata`、`ResolvedPaths`、`ResolvedResource`。
- `core/source-info.ts`：`BUILTIN_PATH_PREFIX`。
- `core/tools/truncate.ts`（单元 **ca-tools-compaction-export**）：`truncateTail`、`TruncationResult`、
  `DEFAULT_MAX_LINES`（2000）、`DEFAULT_MAX_BYTES`（51200）。
- `core/tools/render-utils.ts`（同单元）：`formatToolCallWithArgs`、`getTextOutput`、`formatReadLineRange`。
- `core/extensions/types.ts`（单元 **ca-session**）：`MarkdownTransformContext`、`MarkdownTransformer`、
  `MessageRenderer`、`EntryRenderer`、`MessageRenderOptions`、`ToolRenderContext`、`ToolRenderers`、
  `WorkingIndicatorOptions`、`ContextUsage`、`ToolRendererResolver`。
- `core/agent-session.ts`（单元 **ca-session**）：`AgentSession`、`ParsedSkillBlock`、`parseSkillBlock`。
- `core/footer-data-provider.ts`：`ReadonlyFooterDataProvider`（`getGitBranch()`、
  `getExtensionStatuses(): Map<string,string>`、`getAvailableProviderCount()`）。
- `core/usage-totals.ts`（单元 **ca-tools-compaction-export**）：`UsageTotals`、`createUsageTotals`、
  `addUsageToTotals`。
- `core/http-dispatcher.ts`：`HTTP_IDLE_TIMEOUT_CHOICES`、`formatHttpIdleTimeoutMs`。
- `core/experimental.ts`：`areExperimentalFeaturesEnabled()`。
- `core/model-runtime.ts`（单元 **ca-config-models**）：`ModelRuntime`（`getAvailableSnapshot()`、
  `getModel(provider, id)`、`isUsingSubscription(provider)`、`refresh({signal})`、`getError()`）。
- `modes/interactive/model-search.ts`、`model-catalog-refresh.ts`、`external-editor.ts`（同属本单元范围外的
  同目录文件；本规格 §2 末尾给出它们的接口摘要，实现由 **ca-interactive-core** 或本单元负责——见 §8.5 Q1）。
- `utils/clipboard.ts`（`copyToClipboard`）、`utils/open-browser.ts`（`openBrowser`）、`utils/ansi.ts`
  （`stripAnsi`）、`utils/image-convert.ts`（`ensurePngTranscoder`）、`utils/paths.ts`
  （`canonicalizePath`、`isLocalPath`、`resolvePath`）、`config.ts`
  （`APP_NAME`、`CONFIG_DIR_NAME`、`getBundledInteractiveAssetPath`）。
- `packages/ai`（单元 **ai-core**）：`Model`、`modelsAreEqual`、`getSupportedThinkingLevels`、
  `AuthCheck`、`ApiKeyAuth`、`OAuthAuth`、`OAuthDeviceCodeInfo`、`AuthInfoLink`、`Usage`、`Transport`、
  `ModelsRefreshResult`。
- `packages/agent`（单元 **agent-core**）：`ThinkingLevel`。
- `packages/tui` 的 `TuiAltScreen`（`getScreenLines()`、`hasOverlay()`、`queryTerminalColors()`）。

### 0.3 建议阅读顺序

`dynamic-border.ts` → `themed-text.ts` → `keybinding-hints.ts` → `markdown-transform.ts` → `kiana-logo.ts` →
`countdown-timer.ts` → `visual-truncate.ts` → `diff.ts` → `user-message.ts` → `assistant-message.ts` →
`branch-summary-message.ts` / `compaction-summary-message.ts` / `skill-invocation-message.ts` /
`custom-message.ts` / `custom-entry.ts` → `bash-execution.ts` → `tool-execution.ts` →
`status-indicator.ts` → `custom-editor.ts` → 各 selector（show-images/theme/thinking/trust/extension/
user-message/first-time-setup/oauth/model/scoped-models/settings-submenu）→ `settings-selector.ts` →
`config-selector.ts` → `session-selector-search.ts` → `session-selector.ts` → `tree-selector.ts` →
`footer.ts` → `login-dialog.ts` / `auth-url.ts` → `mermaid.ts` → `armin.ts` / `easter-egg-3d.ts` /
`easter-egg-3d.lazy.ts` → `bordered-loader.ts` / `extension-input.ts` / `extension-editor.ts` →
`radius-login-selector.ts` / `earendil-announcement.ts` / `index.ts`。

---

## 1. 概览

### 1.1 目的

本单元是 kiana 交互式终端界面的**视图层**：把会话状态、流式模型输出、工具执行、扩展 UI 请求渲染成
ANSI 行，并接收键盘/鼠标输入。它不含 agent 逻辑、不含网络、不含文件解析（除 §4 列出的少数直接 I/O），
但它是**用户可见行为的主要载体**：所有面板文案、快捷键提示、选择器布局、动画帧都在这里。

### 1.2 与其他包/单元的关系

```
kiana-tui（Component/TUI/Text/Box/Markdown/Input/Editor/SelectList/SettingsList/Loader/Image/utils/keys/keybindings/fuzzy/ANSI）
   │
kiana-ai（Model/Auth*/Usage/ThinkingLevel）      kiana-agent-core（ThinkingLevel）
   │                                              │
coding-agent core（session-manager/settings-manager/trust-manager/package-manager/
                   footer-data-provider/usage-totals/model-runtime/extensions types）
   │
   └── ca-interactive-components（本单元）
         ├── interactive-mode.ts 是主要宿主：构造并挂载全部组件
         ├── core/tools/renderers/* 通过 ToolExecutionComponent 渲染（renderCall/renderResult）
         └── 扩展通过 ExtensionUIContext 间接触发本单元的选择器/输入框/编辑器
```

**宿主 call site 清单**（`interactive-mode.ts`，7105 行；用于确认构造参数顺序与必需行为）：

| 行 | 构造 |
|---|---|
| 659 | `new FooterComponent(this.session, this.footerDataProvider)` |
| 2741 | `new ExtensionSelectorComponent(title, options, onSelect, onCancel)`（扩展 `select()`） |
| 2798 | `new ExtensionInputComponent(...)`（扩展 `input()`） |
| 2818 | `new ExtensionEditorComponent(...)`（扩展 `editor()`） |
| 3466 | `new AssistantMessageComponent(undefined, hideThinkingBlock, mdTheme, hiddenThinkingLabel, outputPad, transformers)` |
| 3489 / 3568 / 4012 | `new ToolExecutionComponent(name, id, args, {showImages,imageWidthCells,outputPad}, definition, ui, cwd)` |
| 3843 | `new BashExecutionComponent(command, ui, excludeFromContext, outputPad)` |
| 3864 | `new CustomMessageComponent(message, renderer, mdTheme, outputPad)` |
| 3918 / 3929 | `new UserMessageComponent(text, mdTheme, outputPad, transformers, messageId?, timestamp?)` |
| 3946 | `new AssistantMessageComponent(message, hide, mdTheme, label, outputPad, transformers, messageId?, timestamp?)` |
| 4907 | `new SettingsSelectorComponent(config, callbacks)` |
| 5175 | `new ThinkingSelectorComponent(current, available, onSelect, onCancel, onSelectAsDefault?, default?)` |
| 5318 | `new TrustSelectorComponent({cwd, savedDecision, projectTrusted, onSelect, onCancel})` |
| 5356 | `new ModelSelectorComponent(tui, currentModel, runtime, scoped, onSelect, onCancel, initialSearch?, onSelectAsDefault?, defaultModel?)` |
| 5423 | `new ScopedModelsSelectorComponent({allModels, enabledModelIds, refreshStatus?}, {onChange,onPersist,onCancel})` |
| 5507 | `new UserMessageSelectorComponent(messages, onSelect, onCancel, initialSelectedId?)` |
| 5566 | `new TreeSelectorComponent(tree, currentLeafId, terminalHeight, onSelect, onCancel, onLabelChange?, initialSelectedId?, initialFilterMode?)` |
| 5706 | `new SessionSelectorComponent(currentLoader, allLoader, onSelect, onCancel, onExit, requestRender, options?, currentSessionFilePath?)` |
| 5952 | `new ExtensionSelectorComponent(title, options, onSelect, onCancel)`（无 opts） |
| 5971 / 6023 | `new OAuthSelectorComponent(mode, providers, onSelect, onCancel, initialSearch?)` |
| 6166 | `new LoginDialogComponent(tui, providerId, onComplete, providerNameOverride?, titleOverride?)` |
| 另 | `new ConfigSelectorComponent(...)`、`new ShowImagesSelectorComponent(...)`、`new ThemeSelectorComponent(...)`、`new FirstTimeSetupComponent(...)`、`new BorderedLoader(...)`、`new EarendilAnnouncementComponent()`、`new ArminComponent(ui)`、`createLoginMenuSelector(...)`、`new CompactionSummaryMessageComponent(...)`、`new BranchSummaryMessageComponent(...)`、`new SkillInvocationMessageComponent(...)`、`new CustomEntryComponent(entry, renderer, outputPad)`、`createMermaidMarkdownTransformer({getMode, theme})`。 |

**`outputPad` 贯穿全单元**：`0 | 1`（设置项 `outputPad`），默认 1。所有「消息/工具/命令输出」组件都有
`setOutputPad(n)` 并在变更时重建内容；`outputPad.test.ts` 断言：把 `0` 与 `1` 的渲染结果逐行比较，
`1` 的每个非空行**恰好**多一个前导空格。

### 1.3 关键不变量（Rust 实现必须逐条成立）

1. **宽度不变式**：每个组件 `render(width)` 的每一行 `visibleWidth ≤ width`（对 `width = 1, 4, 10, 20, 80, 120`
   都必须成立，`status-indicator.test.ts` 与 `tree-selector.test.ts` 明确测试）。`DynamicBorder` 例外地保证
   `width >= 1`（`Math.max(1, width)` 个 `─`，即 width=0 时输出 1 列，宿主不会传 0）。
2. **主题延迟绑定**：所有颜色都必须**在 render 时**通过全局 `theme` 解析，不能在构造时缓存颜色字符串
   （`ThemedText`、`FirstTimeSetupComponent`、`Markdown` 重渲染都依赖这一点）。唯一的例外是
   `easter-egg-3d.ts` 的 `ansiCache`（在 `invalidate()` 时清空）与 `kiana-logo.ts`（每次调用时读 `getColorMode()`）。
3. **`invalidate()` 语义**：每次调用都必须丢弃全部缓存并按当前状态重建子树；`Markdown` 的 token 缓存可以保留
   （由 tui-components 决定），但颜色必须重放。
4. **OSC 133 区域标记**：`UserMessageComponent` 无条件加；`AssistantMessageComponent` 仅当消息**不含**
   `toolCall` 且行数 > 0 时加。标记放在**首行前缀**与**末行前缀**，不新增行（`user-message.test.ts`
   断言高度不变）。
5. **点击折叠**：`MouseRegion` 包裹的可折叠组件只响应 `event.type === "click" && event.button === "left"`，
   返回 `{ handled: true }`；其它事件返回 `undefined`（让上层继续派发）。
6. **键位匹配顺序**：所有组件的 `handleInput` 用 `getKeybindings().matches(data, "<keybinding id>")`，
   **不得**硬编码键字符串，唯一例外是源码中显式列出的别名（`"k"`、`"j"`、`"\n"`、`" "`、`"ctrl+c"`）。
7. **重建而非增量**：所有列表类组件（selector/list）在选中项/过滤/开关变化时**清空并重建**子组件树；
   没有虚拟 DOM、没有 diff。Rust 侧用「重建 `Vec<Box<dyn Component>>`」实现，不要引入增量更新。
8. **单例全局状态**：`theme`、`getKeybindings()`、`getCapabilities()`、`playing`（3D 彩蛋）、
   `cachedImageBase64`/`attemptedImageLoad`（earendil 公告）都是进程级单例。Rust 用 `OnceLock`/
   `RwLock` 或显式上下文注入（§8.2）。
9. **异步与取消**：`session-selector.ts`、`model-selector.ts`、`login-dialog.ts`、
   `custom-editor`/`extension-editor` 的外部编辑器都使用 `AbortController`/Promise 竞态；Rust 用
   `tokio_util::sync::CancellationToken` + `JoinHandle`，且必须保持「结果落地前检查 isActive」的语义。
10. **无浮点不确定性**：3D 彩蛋与 Radius 微光依赖 `performance.now()`、`Math.random()`、
    `Math.imul` 与 `Float32Array` 舍入。Rust 必须用 `f32`（不是 `f64`）复刻光栅缓冲，见 §8.3。

### 1.4 术语

- **visual line**：按终端宽度换行后的实际屏幕行（`Text.render(width)` 的输出行）。
- **logical line**：以 `\n` 分隔的源文本行。
- **tier score**（会话搜索）：`tier * 1_000_000 + fuzzyScore`，见 §2.36。
- **shell**（工具渲染）：`renderShell: "default" | "self"`，决定 `ToolExecutionComponent` 是否自己画外框。
- **RPC/扩展 UI 契约**：扩展调用 `ui.select/input/editor/custom` 时宿主构造本单元组件的会话。
- **renderInBorder**（状态指示器）：把 loader 的单行输出嵌入编辑器上边框的字符串（不是整行）。

---

## 2. 逐文件规格

> **重要阅读指引**：§2.1–§2.50 是首轮通读的记录；§2.51–§2.59 是对其中 **31 个文件**逐行二次核对后的
> 更正（§2.57 `diff.ts`、§2.58 `visual-truncate.ts`、§2.59 `mermaid.ts` 为最后一轮补读）。
> **凡两者冲突，一律以 §2.51–§2.59 为准**（涉及 `tool-execution`、`model-selector`、
> `scoped-models-selector`、`oauth-selector`、`thinking-selector`、`settings-selector`、`settings-submenu`、
> `config-selector`、`first-time-setup`、`session-selector`、`tree-selector`、`extension-input`、
> `extension-selector`、`extension-editor`、`radius-login-selector`、`bordered-loader`、
> `earendil-announcement`、`user-message-selector`、`trust-selector`、`theme-selector`、
> `show-images-selector`、`assistant-message`、`bash-execution`、`user-message`、`status-indicator`、
> `diff`、`visual-truncate`、`mermaid`）。
> 实现者在读对应文件时，**先看本节正文，再看 §2.5x 的更正**。
>
> 另：§2.60 是最后一批「未二次核对文件」的逐行核对结果（`footer.ts`、`login-dialog.ts`、`auth-url.ts`、
> `session-selector-search.ts`、`skill-invocation-message.ts`、`custom-entry.ts`、`custom-message.ts`、
> `branch-summary-message.ts`、`compaction-summary-message.ts`、`armin.ts`、`easter-egg-3d.ts`、
> `easter-egg-3d.lazy.ts`、`keybinding-hints.ts`、`settings-submenu.ts`），同样以该节为准。

> 每个小节：先列导出面（签名），再列行为（含边界、默认值、错误路径），最后列宽度/缓存/时序要点。
> 类型 `Component` 的 Rust 对应见 §8.2：`trait Component { fn render(&mut self, width: usize) -> Vec<String>;
> fn invalidate(&mut self); fn handle_input(&mut self, data: &str) {} ... }`。

### 2.1 `index.ts`（37 行）

纯 barrel 再导出，内容见 §0.1 的逐字清单。Rust 侧对应 `pub use` 汇总模块 `mod.rs`。

- 无运行时行为。
- **注意**：`CustomEntryComponent`、`EarendilAnnouncementComponent`、`mermaid`、`kiana-logo` 等未导出，
  但仍被 `interactive-mode.ts` 直接按路径 import，必须实现且可被同 crate 访问。

### 2.2 `dynamic-border.ts`（25 行）— 自适应宽度的水平边框

```ts
export class DynamicBorder implements Component {
  constructor(color: (str: string) => string = (str) => theme.fg("border", str));
  invalidate(): void;   // 空实现
  render(width: number): string[];
}
```

- `render(width)` 返回**恰好一行**：`this.color("─".repeat(Math.max(1, width)))`。
- 默认 `color` 是**惰性**的（每次 render 时调 `theme.fg("border", ...)`，所以主题切换后无需重建）。
- 注释明确：从 jiti 加载的扩展里全局 `theme` 可能是 undefined，因此**为扩展导出的组件必须显式传 color 函数**。
  这就是为什么 `EarendilAnnouncementComponent`（accent）、`SessionSelectorComponent`（accent）、
  `BashExecutionComponent`（`dim`/`bashMode`）都显式传色。
- 宽度不变式：`width = 0` → 1 列（唯一允许溢出的组件，但宿主不会传 0）。

### 2.3 `themed-text.ts`（32 行）— 延迟构建的 `Text`

```ts
export class ThemedText extends Text {
  constructor(build: () => string, paddingX = 1, paddingY = 1);
  override invalidate(): void;   // super.invalidate(); this.stale = true;
  override render(width: number): string[];
}
```

- 构造时 `super("", paddingX, paddingY)`，`stale = true`。
- `render(width)`：若 `stale` 则 `stale = false; setText(build())`，再 `super.render(width)`。
- **契约**：`build` 每次必须返回内容相同、只有颜色不同的字符串（主题 preview 时 UI 会 `invalidate` 全部组件）。
- 用于 `interactive-mode.ts` 的扩展状态行/提示行，使系统主题收到终端颜色后能重上色。
- `themed-text.test.ts`：断言首次 render 才调用 `build`；`invalidate()` 后再次 render 会重新调用 `build`
  并采用新主题颜色。

### 2.4 `markdown-transform.ts`（35 行）

```ts
export function createMarkdownTransform(
  messageType: "user" | "assistant" | "assistant-thinking",
  isStreaming: boolean,
  transformers: readonly MarkdownTransformer[],
  messageId?: string,
  timestamp?: string,
): (markdown: string, availableWidth: number) => string;
```

内部 `applyMarkdownTransformers(markdown, context, transformers)`：

```
let out = markdown;
for (t of transformers) {
  try { const r = t(out, context); if (typeof r === "string") out = r; } catch { /* 保留 out，继续下一个 */ }
}
return out;
```

- `context` 每次调用**新建**一个对象：`{ messageType, isStreaming, availableWidth, messageId, timestamp }`。
- 异常吞掉（不打印），链继续（`assistant-message.test.ts` 断言 `calls === ["first","throw","last"]` 且
  输出为 `remains visible after error`）。
- 返回非 string（extension 返回 undefined 等）时**不替换**。
- `availableWidth` 是 `Markdown` 内容区宽度（`width - paddingX*2`），见 `tui-components` 的
  `render` 实现：`transform?.(this.text, contentWidth)`。

### 2.5 `auth-url.ts`（39 行）— 登录 URL + 复制提示

```ts
export class AuthUrlComponent extends Container {
  readonly url: string;
  constructor(tui: TUI, url: string);
  async copy(): Promise<void>;
}
```

构造函数加入两个 `Text(..., 1, 0)`（paddingX=1, paddingY=0）：

1. `theme.fg("accent", hyperlink(url, url))`
2. hint（见下）

`setHint(suffix)`：

```ts
const clickHint = process.platform === "darwin" ? "Cmd+click to open" : "Ctrl+click to open";
this.hint.setText(`${theme.fg("dim", hyperlink(clickHint, this.url))} ${theme.fg("dim", "•")} ${suffix}`);
this.tui.requestRender();
```

构造时 `setHint(keyHint("app.message.copy", "to copy"))`。

`copy()`：

- `await copyToClipboard(this.url)` → `setHint(theme.fg("success", "Copied URL to clipboard"))`；
- 抛错 → `setHint(theme.fg("error", error instanceof Error ? error.message : String(error)))`。

**【逐字】** `Cmd+click to open`、`Ctrl+click to open`、`•`（U+2022）、`to copy`、
`Copied URL to clipboard`。

**超链接格式**（`@earendil-works/kiana-tui` 的 `hyperlink`）：
`\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\`（**ST 是 ESC + 反斜杠**，不是 BEL）。注意与
`login-dialog.ts` 里手写的 `\x1b]8;;url\x07text\x1b]8;;\x07`（BEL 结尾）不同，必须分别照抄。

宿主：`login-dialog.ts` 在 `app.message.copy` 时调用 `copy()`；`auth-url-copy.test.ts` 断言
「登录对话框复制 auth URL 而不是把按键喂给 code 输入框」。

### 2.6 `countdown-timer.ts`（39 行）

```ts
export class CountdownTimer {
  constructor(timeoutMs: number, tui: TUI | undefined,
              onTick: (seconds: number) => void, onExpire: () => void);
  dispose(): void;
}
```

- `remainingSeconds = Math.ceil(timeoutMs / 1000)`；构造时**立即** `onTick(remainingSeconds)`。
- `setInterval(..., 1000)`：每次 `remainingSeconds--; onTick(n); tui?.requestRender();`，
  然后若 `n <= 0`：`dispose(); onExpire();`。
- **顺序要点**：先 tick、再 requestRender、再判断过期（所以 `0` 秒会被 tick 出来，然后过期）。
- `dispose()`：`clearInterval` 并把 id 置 undefined（幂等）。
- `timer.unref?.()` **没有**调用（与 `radius-login-selector`/`easter-egg-3d` 不同）。Rust 侧用
  `tokio::time::interval` + `JoinHandle::abort()`，帧语义保持一致（首帧立即、之后每 1000ms）。
- 使用方：`extension-input.ts`（标题倒计时）、`extension-selector.ts`、`status-indicator.ts` 的
  `RetryStatusIndicator`（1 秒粒度倒计时）。

### 2.7 `kiana-logo.ts`（40 行）

```ts
export function kianaLogoLines(): [string, string];
export function supportsKianaLogo(): boolean;
export function kianaWordmark(): string;
```

常量：

```ts
const CORAL  = rgbColor(228, 138, 122);
const BLUE   = rgbColor(79, 142, 179);
const YELLOW = rgbColor(234, 182, 93);
const RESET  = "\x1b[0m";
```

`kianaLogoLines()`（**逐字**模板，注意 `▀`/`█` 与空格）：

```ts
const mode = theme.getColorMode();
const fg = (color) => foregroundAnsi(color, mode);
const top    = `${fg(CORAL)}${backgroundAnsi(BLUE, mode)}▀${RESET}${fg(CORAL)}▀█${RESET} `;
const bottom = `${fg(BLUE)}█▀${RESET} ${fg(YELLOW)}█${RESET}`;
return [top, bottom];
```

含义：4 列 × 2 行，每个字符格用半块表示上下两个像素：

```
coral coral coral .
blue  .     coral .
blue  blue  .     yellow
blue  .     .     yellow
```

（`▀` = 上像素亮；`█` = 上下都亮；背景蓝色的 `▀` 用来画第一行第二格的上半蓝、下半珊瑚。）

`supportsKianaLogo()`：`return !isAppleTerminalSession();`（Apple Terminal 行间有缝、半块错位，
改用文字 wordmark）。

`kianaWordmark()`：`` `${fg(CORAL)}P${RESET}${fg(YELLOW)}i${RESET}` ``。

- 品牌色**不随主题变化**，只随终端色彩模式（`truecolor` / `256` / `16`）变化。
- 使用方：`interactive-mode.ts` 的启动 header；`easter-egg-3d` 的 kiana logo 模型（颜色来自 RGB 常量）。

### 2.8 `keybinding-hints.ts`（48 行）— 键位提示格式化（全单元的基础设施）

```ts
// 【精读更正】6 个 export（其中 formatKeys 是模块私有）；barrel 只再导出 keyHint/keyText/rawKeyHint。
export interface KeyTextFormatOptions { capitalize?: boolean }
export function formatKeyText(key: string, options?: KeyTextFormatOptions): string;
export function keyText(keybinding: Keybinding): string;
export function keyDisplayText(keybinding: Keybinding): string;
export function keyHint(keybinding: Keybinding, description: string): string;
export function rawKeyHint(key: string, description: string): string;
```

`formatKeyPart(part, options)`：

```ts
const displayPart = process.platform === "darwin" && part.toLowerCase() === "alt" ? "option" : part;
return options.capitalize ? displayPart.charAt(0).toUpperCase() + displayPart.slice(1) : displayPart;
```

`formatKeyText(key, options)`：先按 `"/"` 分段，再对每段按 `"+"` 分段，逐段 `formatKeyPart`，用 `"+"` 连接，
段间用 `"/"` 连接。即 `"ctrl+shift+up"` → `"ctrl+shift+up"`；`"ctrl+shift+up/ctrl+up"` →
`"ctrl+shift+up/ctrl+up"`。

`formatKeys(keys: KeyId[], options)`：`keys.length === 0` → `""`；否则 `formatKeyText(keys.join("/"), options)`。

- `keyText(kb)` = `formatKeys(getKeybindings().getKeys(kb))`
- `keyDisplayText(kb)` = 同上但 `{ capitalize: true }`
- `keyHint(kb, desc)` = `theme.fg("dim", keyText(kb)) + theme.fg("muted", " " + desc)`
- `rawKeyHint(key, desc)` = `theme.fg("dim", formatKeyText(key)) + theme.fg("muted", " " + desc)`
- **`formatKeys(keys: KeyId[], options = {})` 是模块私有**（不导出）：`keys.length === 0 → ""`，
  否则 `formatKeyText(keys.join("/"), options)`。因此「未绑定任何键」的提示退化为只有描述的空键位
  （dim 段为空串，仍会输出一个前导空格）。

**【逐字】** 分隔符是半角空格 `" "`（不是 `·`）；`keyDisplayText` 只把每段**首字母**大写
（`escape` → `Escape`，`ctrl+s` → `Ctrl+S`）。

**Rust 注意**：`charAt(0).toUpperCase()` 在 JS 里对 `ß` 这类字符不做全大写展开；
Rust 用 `char::to_uppercase().next()` 取第一个标量即可对齐（键位名都是 ASCII，实际无差异）。

### 2.9 `show-images-selector.ts`（50 行）

```ts
export class ShowImagesSelectorComponent extends Container {
  constructor(currentValue: boolean, onSelect: (show: boolean) => void, onCancel: () => void);
  getSelectList(): SelectList;
}
```

布局：`DynamicBorder` → `SelectList(items, 5, getSelectListTheme(), {minPrimaryColumnWidth:12, maxPrimaryColumnWidth:32})`
→ `DynamicBorder`。

items **【逐字】**：

```ts
{ value: "yes", label: "Yes", description: "Show images inline in terminal" }
{ value: "no",  label: "No",  description: "Show text placeholder instead" }
```

`setSelectedIndex(currentValue ? 0 : 1)`；`onSelect = (item) => onSelect(item.value === "yes")`；
`onCancel = onCancel`。

### 2.10 `earendil-announcement.ts`（53 行）

```ts
export class EarendilAnnouncementComponent extends Container { constructor(); }
```

常量 **【逐字】**：

```ts
const BLOG_URL = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";
const IMAGE_FILENAME = "clankolas.png";
```

模块级缓存：`cachedImageBase64: string | undefined`、`attemptedImageLoad = false`。
`loadImageBase64()` 只在首次调用时读 `getBundledInteractiveAssetPath("clankolas.png")`，
`readFileSync(...).toString("base64")`；失败（文件缺失/读错误）→ `undefined`，之后不再重试。

子组件顺序：

1. `DynamicBorder((t) => theme.fg("accent", t))`
2. `Text(theme.bold(theme.fg("accent", "kiana has joined Earendil")), 1, 0)`
3. `Spacer(1)`
4. `Text(theme.fg("muted", "Read the blog post:"), 1, 0)`
5. `Text(theme.fg("mdLink", BLOG_URL), 1, 0)`
6. `Spacer(1)`
7. 若图片 base64 存在：`Image(base64, "image/png", { fallbackColor: (t) => theme.fg("muted", t) },
   { maxWidthCells: 56, filename: "clankolas.png" })`，再 `Spacer(1)`
8. `DynamicBorder((t) => theme.fg("accent", t))`

**【逐字】** `kiana has joined Earendil`、`Read the blog post:`、URL 如上。

### 2.11 `theme-selector.ts`（67 行）

```ts
export class ThemeSelectorComponent extends Container {
  constructor(currentTheme: string,
              onSelect: (themeName: string) => void,
              onCancel: () => void,
              onPreview: (themeName: string) => void);
  getSelectList(): SelectList;
}
```

- items 来自 `getAvailableThemes()`：`{ value: name, label: name, description: name === currentTheme ? "(current)" : undefined }`。
- `SelectList(items, 10, getSelectListTheme(), {minPrimaryColumnWidth:12, maxPrimaryColumnWidth:32})`。
- `currentIndex = themes.indexOf(currentTheme)`；`!== -1` 时 `setSelectedIndex(currentIndex)`。
- `onSelect` → `onSelect(item.value)`；`onCancel` → `onCancel()`；`onSelectionChange` → `onPreview(item.value)`
  （**每次移动光标都会切换实际主题**，由宿主调用 `initTheme` 做预览）。
- 布局：`DynamicBorder` → list → `DynamicBorder`。

### 2.12 `bordered-loader.ts`（68 行）

```ts
export class BorderedLoader extends Container {
  constructor(tui: TUI, theme: Theme, message: string, options?: { cancellable?: boolean });
  get signal(): AbortSignal;
  set onAbort(fn: (() => void) | undefined);
  handleInput(data: string): void;
  dispose(): void;
}
```

- `cancellable = options?.cancellable ?? true`。
- `borderColor = (s) => theme.fg("border", s)`（**构造参数 `theme` 是快照对象**，不是全局代理；扩展场景下全局
  theme 可能 undefined）。
- cancellable：`new CancellableLoader(tui, accent, muted, message)`；否则
  `new AbortController()` + `new Loader(tui, accent, muted, message)`。
- 子组件：`DynamicBorder(borderColor)` → loader →（可取消时）`Spacer(1)` +
  `Text(keyHint("tui.select.cancel", "cancel"), 1, 0)` → `Spacer(1)` → `DynamicBorder(borderColor)`。
- `signal`：可取消时返回 loader 的 `signal`；否则 `signalController.signal`（不可取消时这个 signal 永不 abort）。
- `onAbort` setter：只在可取消时透传。
- `handleInput`：只在可取消时转给 `CancellableLoader`，否则**忽略所有输入**。
- `dispose()`：优先调 loader 的 `dispose()`，否则 `stop()`。
- **【逐字】** `cancel`。

### 2.13 `skill-invocation-message.ts`（68 行）— `[skill]` 可折叠块

```ts
export class SkillInvocationMessageComponent extends Box {
  constructor(skillBlock: ParsedSkillBlock, markdownTheme?: MarkdownTheme, outputPad?: number);
  setExpanded(expanded: boolean): void;
  setOutputPad(outputPad: number): void;   // → setPaddingX(outputPad)
  override invalidate(): void;             // super.invalidate(); updateDisplay();
}
```

- 继承 `Box`，构造 `super(outputPad, 1, (t) => theme.bg("customMessageBg", t))`（paddingY=1）。
- `ParsedSkillBlock = { name: string; location: string; content: string; userMessage: string | undefined }`。
- `updateDisplay()`：`clear()` 后建 `Container content`：
  - **expanded**：`Text(theme.fg("customMessageLabel", "\x1b[1m[skill]\x1b[22m"), 0, 0)`，然后
    `Markdown("**" + name + "**\n\n" + content, 0, 0, markdownTheme, {color: (t)=>theme.fg("customMessageText", t)})`。
  - **collapsed**（单行）：
    ```
    theme.fg("customMessageLabel", "\x1b[1m[skill]\x1b[22m ")
    + theme.fg("customMessageText", name)
    + theme.fg("dim", " (" + keyText("app.tools.expand") + " to expand)")
    ```
  - 用 `MouseRegion(content, click → setExpanded(!expanded); {handled:true})` 包起来加入自身。
- `setOutputPad` 只改 paddingX，**不重建**内容（Box 自己处理）。
- **【逐字】** `[skill]`、` to expand`。
- 注意 `\x1b[1m` / `\x1b[22m` 是**手写加粗**（不用 `theme.bold`），因为要在 label 内嵌在已着色的字符串中，
  且 `[22m` 只关闭 bold 不重置颜色。

### 2.14 `custom-entry.ts`（69 行）

```ts
export class CustomEntryComponent extends Container {
  constructor(entry: CustomEntry<unknown>, renderer: EntryRenderer, outputPad?: number);
  hasContent(): boolean;
  setExpanded(expanded: boolean): void;   // 只有变化时才 rebuild
  setOutputPad(outputPad: number): void;  // 总是 rebuild
  override invalidate(): void;            // super.invalidate(); rebuild();
}
```

`rebuild()`：

1. `clear()`；`customComponent = undefined`。
2. `try { component = renderer(entry, { expanded: this._expanded }, theme) } catch (error) { ... }`
   错误分支构造 `Box(outputPad, 1, (t)=>theme.bg("customMessageBg", t))`，内含
   `Text(theme.fg("error", `[${entry.customType}] renderer failed: ${message}`), 0, 0)`，
   其中 `message = error instanceof Error ? error.message : String(error)`。
3. `!component` → 直接返回（空容器，不占空间）。
4. 否则 `addChild(new Spacer(1)); addChild(component)`。

**【逐字】** `[${customType}] renderer failed: ${message}`。

- 宿主负责转录间距：注释明确「renderer output should provide only its content」，因此这里自己加 `Spacer(1)`。

### 2.15 `branch-summary-message.ts`（71 行）

```ts
export class BranchSummaryMessageComponent extends Box {
  constructor(message: BranchSummaryMessage, markdownTheme?: MarkdownTheme, outputPad?: number);
  setExpanded(expanded: boolean): void;
  setOutputPad(outputPad: number): void;  // setPaddingX
  override invalidate(): void;
}
```

`BranchSummaryMessage = { role: "branchSummary"; summary: string; fromId: string | null; timestamp: number }`。

`updateDisplay()`：

- `clear()`，建 `Container content`。
- `Text(theme.fg("customMessageLabel", "\x1b[1m[branch]\x1b[22m"), 0, 0)`，`Spacer(1)`。
- expanded：`Markdown("**Branch Summary**\n\n" + summary, 0, 0, markdownTheme,
  {color: t => theme.fg("customMessageText", t)})`。
- collapsed：
  ```
  theme.fg("customMessageText", "Branch summary (")
  + theme.fg("dim", keyText("app.tools.expand"))
  + theme.fg("customMessageText", " to expand)")
  ```
- `MouseRegion` 点击切换。

**【逐字】** `[branch]`、`**Branch Summary**\n\n`、`Branch summary (`、` to expand)`。

### 2.16 `compaction-summary-message.ts`（72 行）

```ts
export class CompactionSummaryMessageComponent extends Box {
  constructor(message: CompactionSummaryMessage, markdownTheme?: MarkdownTheme, outputPad?: number);
  setExpanded(expanded: boolean): void;
  setOutputPad(outputPad: number): void;
  override invalidate(): void;
}
```

`CompactionSummaryMessage = { role: "compactionSummary"; summary: string; tokensBefore: number; timestamp: number }`。

- `tokenStr = message.tokensBefore.toLocaleString()` —— **JS `toLocaleString()` 在默认 locale 下是千分位
  逗号分组**（`1234` → `"1,234"`；**与 locale 无关地固定为逗号**，见 §6.3 与 §8.5 Q12 的说明）。
  Rust 必须实现同样分组（§8.3 第 7 项）。
- label `\x1b[1m[compaction]\x1b[22m` + `Spacer(1)`。
- expanded：`Markdown("**Compacted from " + tokenStr + " tokens**\n\n" + summary, ...)`。
- collapsed：`theme.fg("customMessageText", "Compacted from " + tokenStr + " tokens (") + dim(keyText) +
  " to expand)"`。
- `MouseRegion` 点击切换。

**【逐字】** `[compaction]`、`**Compacted from `、` tokens**`、`Compacted from `、` tokens (`、` to expand)`。

**测试**：`collapsible-message-components.test.ts` 断言三个可折叠组件（compaction/branch/skill）在
collapsed 时不包含正文、点击 `[label]` 所在行后展开、再点收起；点击用 `x:2, y:row`，组件必须
`handleMouse(event)?.handled === true`。`compaction-summary-message` 的 `tokensBefore: 1234` 渲染后
collapsed 文本含 `"1,234"`。

### 2.17 `user-message.ts`（83 行）

```ts
// 【精读更正】这三个常量是 **模块私有**，本文件只有 `UserMessageComponent` 一个 export。
const OSC133_ZONE_START = "\x1b]133;A\x07";
const OSC133_ZONE_END   = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL = "\x1b]133;C\x07";

export class UserMessageComponent extends Container {
  constructor(text: string, markdownTheme?: MarkdownTheme, outputPad?: number,
              transformers?: readonly MarkdownTransformer[],
              messageId?: string, timestamp?: string);
  setOutputPad(outputPad: number): void;
  override render(width: number): string[];
  // 没有 override invalidate()
}
```

`rebuild()`：

```ts
this.clear();
this.addChild(new Markdown(
  this.text,
  this.outputPad,          // paddingX
  1,                       // paddingY
  this.markdownTheme,
  { color: (t) => theme.fg("userMessageText", t),
    bgColor: (t) => theme.bg("userMessageBg", t) },
  { preserveOrderedListMarkers: true,
    preserveBackslashEscapes: true,
    transform: createMarkdownTransform("user", false, this.transformers, this.messageId, this.timestamp) },
));
```

`render(width)`：

```ts
const lines = super.render(width);
if (lines.length === 0) return lines;
lines[0] = OSC133_ZONE_START + lines[0];
const last = lines.length - 1;
lines[last] = OSC133_ZONE_END + OSC133_ZONE_FINAL + lines[last];
return lines;
```

- **不新增行**，两个标记都是「零宽序列」，`visibleWidth` 不变（`user-message.test.ts` 断言
  「从 1 行 Markdown 得来的高度不变」；终端把行首 OSC 序列消费掉）。
- **顺序**：`B` 后紧跟 `C`（终结标记在行首、在 B 之后）。
- `setOutputPad` → 存值 + `rebuild()`。
- **`invalidate()` 未覆盖**（`Container`/`Component` 默认实现只转发给子组件）。**【更正】** 旧稿写有
  `override invalidate(): void { super.invalidate(); this.rebuild(); }` —— **源码里没有这个方法**。
  Transformer 的重放发生在 `Markdown` 自身 `invalidate()` 里（它重跑 `transform`），
  `user-message.test.ts:46` 的 "reapplies Markdown transformers when invalidated" 正是断言这一点，
  不需要 `UserMessageComponent` 重建。
- `messageId`/`timestamp` 会出现在每个 transformer 的 context 中；**live 消息**（尚未落盘）两者都是
  `undefined`，而转录里的历史消息有值（测试断言 `messageId` 在历史消息中传递、live 消息为 undefined）。

### 2.18 `mermaid.ts`（89 行）— Mermaid 图的终端渲染（grok-mermaid）

> **⚠ 本节有误，以 §2.59 为准。**（`styleSpan` 的 token、`codeSpan` 的反引号围栏算法、流式/最终模式分支）


```ts
import { Marked } from "@earendil-works/kiana-tui";
export interface MermaidMarkdownTransformerOptions {
  getMode: () => MermaidRenderingMode;         // "off" | "final" | "streaming"
  theme: MarkdownTheme;
}
export function createMermaidMarkdownTransformer(
  options: MermaidMarkdownTransformerOptions,
): (markdown: string, context: MarkdownTransformContext) => string;
```

模块级 `const markdownParser = new Marked();`（复用同一个实例以便 lexer 缓存）。

`isMermaid(token)`：

```ts
token.type === "code" &&
token.lang?.trim().split(/\s+/, 1)[0]?.toLowerCase() === "mermaid"
```

（`lang` 可以是 `"mermaid title=..."`，只看第一个空白分隔的单词。）

`codeSpan(line)`：

```ts
const content = line || " ";          // 空行用不换行空格，否则 markdown 会吞掉
const longestBacktickRun = Math.max(0, ...(content.match(/`+/g) ?? []).map((run) => run.length));
const fence = "`".repeat(longestBacktickRun + 1);
const padding = content.startsWith("`") || content.endsWith("`") ? " " : "";
return `${fence}${padding}${content}${padding}${fence}`;
```

`styleSpan(span, theme)`：按 `span.cls` 映射

| cls | 样式 |
|---|---|
| `"border"` | `theme.fg("borderMuted", text)` |
| `"text"` | `theme.fg("text", text)` |
| `"edge"` | `theme.fg("accent", text)` |
| `"edgeLabel"` | `theme.fg("muted", text)` |
| `"title"` | `theme.bold(theme.fg("accent", text))` |
| 其他/`"none"` | 原样 text |

`span` 形状：`{ text: string; cls: string }`；`art` 形状：`{ width: number; styled: Span[][]; plain: string[];
warnings: string[] }`（由 `grok-mermaid` 的 `renderMermaid(code)` 返回；该库**不在**本仓库 node_modules 中，
移植时需按此契约实现，参见 §8.5 Q3）。

转换器整体流程：

1. `mode = options.getMode()`。
2. 若 `mode === "off"` 或 `context.messageType === "assistant-thinking"` 或
   （`context.isStreaming && mode !== "streaming"`）→ 直接 `return markdown`（完全不 lex）。
3. `const tokens = markdownParser.lexer(markdown)`；逐 token：
   - 非 mermaid → 追加 `token.raw`；
   - mermaid → `const art = renderMermaid(token.text)`；
     - `!art` 或 `art.width > context.availableWidth` → 追加 `token.raw`；
     - `art.warnings.length > 0` 且**不**在流式（`!context.isStreaming`）→ 只显示第一条警告：
       ```
       const suffix = art.warnings.length > 1 ? ` (+${art.warnings.length - 1} more)` : "";
       const styledWarning = theme.fg("warning", `Mermaid diagram not rendered: ${art.warnings[0]}${suffix}`);
       out += `${token.raw}\n${codeSpan(styledWarning)}  \n`;
       ```
     - 否则（无警告或流式中）→
       ```
       out += art.styled.map((spans) => codeSpan(spans.map(styleSpan).join(""))).join("  \n") + "\n";
       ```
4. 返回拼接结果。

- **【逐字】** `Mermaid diagram not rendered: `、` (+N more)`、行间分隔 `"  \n"`（两个空格 = Markdown 硬换行）。
- **测试**（`mermaid.test.ts` 8 项）：简单图替换为 `│ Start ├───▶│ Done │`；`pie` 图与超宽图保持原样；
  主题 span 出现 `<borderMuted>`/`<accent>` 标记；流式中未闭合的图渲染 `───▶`；`classDef` 赋值图渲染
  `│ Foo ├───▶│ Bar │`；警告文本形如
  `` ```\n`Mermaid diagram not rendered 'dropped, expected a link: "invalid"'`  ``；`(+1 more)` 汇总；
  `off` / `final`+非流式 / `assistant-thinking` 三种模式保持原 markdown。
- **注意**：`codeSpan` 用围栏长度 = 最长反引号 run + 1，所以警告（含反引号）也能安全内联。

### 2.19 `extension-input.ts`（94 行）

```ts
// 【精读更正】本文件两个 export：ExtensionInputOptions、ExtensionInputComponent。
export interface ExtensionInputOptions {
  tui?: TUI;
  timeout?: number;
  initialValue?: string;
  description?: string;
}
export class ExtensionInputComponent extends Container implements Focusable {
  constructor(title: string, _placeholder: string | undefined,
              onSubmit: (value: string) => void, onCancel: () => void,
              opts?: ExtensionInputOptions);
  handleInput(data: string): void;
  dispose(): void;                // 只 dispose 倒计时
  focused: boolean;               // getter/setter 代理 input.focused
}
```

子组件顺序：

1. `DynamicBorder(theme)`（默认 border 色）
2. `Spacer(1)`
3. `Text(theme.fg("accent", title), 1, 0)` —— baseTitle；有倒计时时改为
   `` `${baseTitle} (${seconds}s)` `` 仍是 accent
4. 若 `description`：`Spacer(1)` + `Text(theme.fg("text", description), 1, 0)`
5. `Spacer(1)`
6. `Input`（`initialValue` 预填；**不设置 `onSubmit`/`onCancel` 回调** —— 全部由组件自身 `handleInput` 处理）
7. `Spacer(1)`
8. `Text(`${keyHint("tui.select.confirm","submit")}  ${keyHint("tui.select.cancel","cancel")}`, 1, 0)`（两 hint 间**两个空格**）
9. `Spacer(1)`
10. `DynamicBorder()`

- `_placeholder` **完全未使用**（参数名带下划线）。
- 倒计时：`opts.timeout && opts.timeout > 0 && opts.tui` 三者同时满足才构造
  `CountdownTimer(timeout, tui, s => titleText.setText(theme.fg("accent", `${baseTitle} (${s}s)`)), () => onCancelCallback())`。
  文本格式 **【逐字】** ` (${s}s)`（前置一个空格、小写 s）。
- **【更正】** 第 3 项与倒计时之间还有一次 `Spacer(1)` 的顺序细节：`DynamicBorder` → `Spacer(1)` → title →
  （description 时）`Spacer(1)` + desc → `Spacer(1)` → input → `Spacer(1)` → hints → `Spacer(1)` → `DynamicBorder`。
  即**未提供 description 时 title 后仍有一个 `Spacer(1)`**。
- `handleInput`：
  ```ts
  if (getKeybindings().matches(data, "tui.select.confirm") || data === "\n") { this.onSubmit(this.input.getValue()); return; }
  if (getKeybindings().matches(data, "tui.select.cancel")) { this.onCancel(); return; }
  this.input.handleInput(data);
  ```
  **注意**：`"\n"` 是硬编码兜底（很多终端把 Enter 发成 `\r`，`Input` 自己处理，这里只为 `tui.select.confirm`
  为 Enter 的系统兜底）。
- `dispose()`：倒计时 `dispose()`。
- **【逐字】** `submit`、`cancel`。

### 2.20 `visual-truncate.ts`（98 行）

```ts
export interface VisualTruncateResult { visualLines: string[]; skippedCount: number; }
export function truncateToVisualLines(
  text: string,
  maxVisualLines: number,
  width: number,
  paddingX: number = 0,
  keep: "start" | "end" = "end",
): VisualTruncateResult;

// 【精读更正】构造函数只接受一个 options 对象，不是四个位置参数
export interface VisualLinePreviewOptions {
  text: string;                        // 已上色的文本，可含 \n
  maxVisualLines: number;
  keep: "start" | "end";               // hint 放在保留段之前（end）还是之后（start）
  formatHint: (hidden: number) => string;
}
export class VisualLinePreview implements Component {
  constructor(options: VisualLinePreviewOptions);
  render(width: number): string[];
  invalidate(): void;
}
```

`truncateToVisualLines`（**逐字**）：

```ts
if (!text) return { visualLines: [], skippedCount: 0 };      // ← 空串特判，不构造 Text
const tempText = new Text(text, paddingX, 0);                 // paddingY 固定 0
const allVisualLines = tempText.render(width);
if (allVisualLines.length <= maxVisualLines) return { visualLines: allVisualLines, skippedCount: 0 };
const truncatedLines = keep === "start" ? allVisualLines.slice(0, maxVisualLines)
                                        : allVisualLines.slice(-maxVisualLines);
const skippedCount = allVisualLines.length - maxVisualLines;
return { visualLines: truncatedLines, skippedCount };
```

`VisualLinePreview`（**逐字**）：

```ts
render(width: number): string[] {
  if (this.cachedLines === undefined || this.cachedWidth !== width) {
    const { text, maxVisualLines, keep, formatHint } = this.options;
    const preview = truncateToVisualLines(text, maxVisualLines, width, 0, keep);   // paddingX 恒 0
    const lines = preview.visualLines;
    if (preview.skippedCount > 0) {
      const hint = truncateToWidth(formatHint(preview.skippedCount), width, "...");
      this.cachedLines = keep === "start" ? [...lines, hint] : [hint, ...lines];
    } else {
      this.cachedLines = lines;
    }
    this.cachedWidth = width;
  }
  return this.cachedLines;
}
invalidate(): void { this.cachedWidth = undefined; this.cachedLines = undefined; }
```

- **【更正】** 没有 `getText` 回调、没有 `formatHint` 之外的选项：文本是**构造时按引用捕获**的
  `options.text`。原地改 `options.text`（或 `options.maxVisualLines`/`keep`）后**必须 `invalidate()`**，
  否则同宽度下继续返回旧缓存。这正是 `tool-execution`/`bash-execution` 在文本变化时调 `invalidate()` 的原因。
- **【更正】** hint 位置**跟随 `keep`**（`start` → hint 在后；`end` → hint 在前），不是固定 end 语义。
- hint 用 `truncateToWidth(hint, width, "...")` 截断（省略号 `"..."`，无 padding）。
- 空文本 → `visualLines: []`、`skippedCount: 0` → 无 hint 行、渲染 0 行。
- 使用方：`bash-execution.ts` 的折叠预览（`formatHint = hidden => `... ${hidden} more lines (${keyText("app.tools.expand")} to expand)`）、
  `tool-execution.ts` 的通用折叠输出。

### 2.21 `custom-message.ts`（114 行）

```ts
export class CustomMessageComponent extends Container {
  constructor(message: CustomMessage<unknown>, customRenderer?: MessageRenderer,
              markdownTheme?: MarkdownTheme, outputPad?: number);
  setExpanded(expanded: boolean): void;   // 只在变化时 rebuild
  setOutputPad(outputPad: number): void;  // 只在变化时 rebuild
  override invalidate(): void;            // super.invalidate(); rebuild();
}
```

- 构造：`addChild(new Spacer(1))` 一次；`this.box = new Box(1, 1, (t) => theme.bg("customMessageBg", t))`
  —— **Box 的 paddingX 恒为 1**（构造后不再改），`outputPad` 通过 `rebuild()` 里的
  `this.box.setPaddingX(this.outputPad)` 生效。
- **`rebuild()` 用 `removeChild` 而不是 `clear()`**（**【更正】** 旧稿说 `clear()`）：
  ```ts
  if (this.customComponent) { this.removeChild(this.customComponent); this.customComponent = undefined; }
  this.removeChild(this.box);
  if (this.customRenderer) {
    try {
      const component = this.customRenderer(this.message, { expanded: this._expanded, outputPad: this.outputPad }, theme);
      if (component) { this.customComponent = component; this.addChild(component); return; }
    } catch { /* 静默，退回默认渲染 */ }
  }
  this.addChild(this.box);
  this.box.clear();
  this.box.setPaddingX(this.outputPad);
  const label = theme.fg("customMessageLabel", `\x1b[1m[${this.message.customType}]\x1b[22m`);
  this.box.addChild(new Text(label, 0, 0));
  this.box.addChild(new Spacer(1));
  let text: string;
  if (typeof this.message.content === "string") text = this.message.content;
  else text = this.message.content.filter((c): c is TextContent => c.type === "text").map((c) => c.text).join("\n");
  this.box.addChild(new Markdown(text, 0, 0, this.markdownTheme,
    { color: (t) => theme.fg("customMessageText", t) }));
  ```
- **自定义渲染器命中时完全不出现 `Box`**（既无 customMessageBg 背景也无内边距），并且 `Spacer(1)` 仍在
  最前面（构造时加的，不会被 rebuild 移除）。`renderer` 返回 `undefined` 或抛异常都回退到默认渲染。
- **【更正】** `setOutputPad` **只在值变化时** rebuild（`if (this.outputPad !== outputPad)`）。
- `CustomMessage = { role: "custom"; customType: string; content: string | (TextContent | ImageContent)[]; display: boolean; timestamp: number }`。
- **【逐字】** `[${customType}]`（`\x1b[1m`…`\x1b[22m` 手写粗体）。
- `custom-message.test.ts`：断言 `renderer` 收到 `{expanded:false, outputPad:1}`；`setOutputPad(0)` 后
  同一渲染器的输出变窄且无前导空格（说明 renderer 的 `outputPad` 参数也要生效）。

### 2.22 `radius-login-selector.ts`（114 行）

> **⚠ 本节签名与实现多处有误，以 §2.54.5 为准。** 摘要更正：
> 只有 `createLoginMenuSelector` 导出；`radiusShimmer` 与 `RadiusLoginMenuComponent` 均**不导出**；
> `radiusOption` 是独立的 `{ label, text }` 对象（不追加进 `options`）；`onSelect` 是**单参数**；
> `radiusShimmer` 用 `for...of`（**按码点**，不是 `text[i]` 的 UTF-16 code unit）。

```ts
// 以下为 §2.22 旧稿（有误，保留以便对照）：
export function radiusShimmer(text: string, elapsedMs: number): string;
export class RadiusLoginMenuComponent extends ExtensionSelectorComponent { ... }
export function createLoginMenuSelector(
  tui: TUI, title: string, options: string[],
  radiusOption: string,
  onSelect: (option: string, index: number) => void,
  onCancel: () => void,
): ExtensionSelectorComponent;
```

常量：

```ts
const RADIUS_COLORS = ["#4d9abf", "#83ccd2", "#f1be57", "#f09082"].map((c) => parseColor(c));
const CHARS_PER_COLOR = 4;
const CHARS_PER_SECOND = 10;
const ANIMATION_FRAME_MS = 50;
```

`radiusShimmer(text, elapsedMs)`（**【更正】** 用 `for...of`，**按 Unicode 码点**迭代，不是 `text[i]`）：

```ts
const mode = theme.getColorMode();                          // ← 循环外取一次
const cycle = RADIUS_COLORS.length * CHARS_PER_COLOR;       // 16
const offset = (elapsedMs / 1000) * CHARS_PER_SECOND;       // 每秒 10 字符
let result = "";
let index = 0;
for (const char of text) {                                  // 码点
  const position = (((index - offset) % cycle) + cycle) % cycle;
  const band = Math.floor(position / CHARS_PER_COLOR);      // 0..3
  const t = position / CHARS_PER_COLOR - band;              // 0..1 平滑
  const amount = t * t * (3 - 2 * t);                       // smoothstep
  const from = RADIUS_COLORS[band];
  const to = RADIUS_COLORS[(band + 1) % RADIUS_COLORS.length];
  result += foregroundAnsi(mixColors(from, to, amount, "srgb"), mode) + char;
  index++;
}
return `${result}\x1b[39m`;
```

- **【更正】** 是 `for...of`（按码点），因此 Rust 用 `chars()` 完全等价（原 §2.22 说「`text[i]` 拆代理对、
  Rust 实现有差异」的警告不成立）。
- 结束用 `"\x1b[39m"`（只重置前景色），不是 `\x1b[0m`。
- `theme.getColorMode()` 在循环**外**取一次。

`RadiusLoginMenuComponent extends ExtensionSelectorComponent`（**【精读更正】**）：

- 私有字段：`radiusOption: RadiusOption`、`animationStart = performance.now()`、
  `animationTimer`、`animating = false`。
- 构造 `(tui, title, options: string[], radiusOption: RadiusOption, onSelect: (option: string) => void,
  onCancel: () => void)`：`super(title, options, wrappedOnSelect, wrappedOnCancel)` —— 两个回调都先
  `stopAnimation()` 再调宿主；`setInterval(ANIMATION_FRAME_MS)` 只在 `animating` 为真时 `tui.requestRender()`，
  并 `unref?.()`。
- 覆盖 `render(width)`：
  ```ts
  const lines = super.render(width);
  const { label, text } = this.radiusOption;
  const selectedLine = new Text(theme.fg("accent", "→ ") + theme.fg("accent", label), 1, 0).render(width)[0];
  const index = selectedLine === undefined ? -1 : lines.indexOf(selectedLine);
  this.animating = index >= 0;
  if (this.animating) {
    const shimmer = radiusShimmer(text, performance.now() - this.animationStart);
    const animatedLine = theme.fg("accent", "→ ") + shimmer + label.slice(text.length);
    lines[index] = new Text(animatedLine, 1, 0).render(width)[0] ?? "";
  }
  return lines;
  ```
  —— 匹配用 **`lines.indexOf(精确相等)`**（不是「包含」）；替换时保留 `→ ` 前缀与 `label` 在 `text` 之后的
  剩余部分（`label.slice(text.length)`，`text` 必须是 `label` 的前缀）。**只要某行的完整渲染串与
  `selectedLine` 相等**才会动画；行样式/换行变化即自动退化为静态。
- `stopAnimation()`：`clearInterval` + `undefined` + `animating = false`（**幂等**）。
- `override dispose()`：`stopAnimation()` 后 `super.dispose()`。
  `elapsed = performance.now() - startTime`。
- `createLoginMenuSelector(...)`：把 `radiusOption` 追加到 `options` 末尾并返回
  `new RadiusLoginMenuComponent(tui, title, options, onSelect, onCancel)`；
  `onSelect(option, index)` 的 index 是**原 options + radius** 的索引。
- 使用方：`/login` 的 Radius provider 菜单。

### 2.23 `extension-selector.ts`（117 行）

```ts
// 【精读更正】本文件两个 export；组件**不实现 Focusable**（没有 focused 字段/属性）
export interface ExtensionSelectorOptions {
  tui?: TUI;
  timeout?: number;
  onToggleToolsExpanded?: () => void;
  description?: string;
}
export class ExtensionSelectorComponent extends Container {
  constructor(title: string, options: string[],
              onSelect: (option: string) => void,      // 只有一个参数（无 index）
              onCancel: () => void,
              opts?: ExtensionSelectorOptions);
  handleInput(data: string): void;
  dispose(): void;                                     // 只 dispose 倒计时
}
```

子组件顺序：

1. `DynamicBorder()`
2. `Spacer(1)`
3. `Text(theme.fg("accent", theme.bold(title)), 1, 0)`（**【更正】** 先 bold 后 fg；baseTitle）
4. 可选 `description`：`Spacer(1)` + `Text(theme.fg("text", description), 1, 0)`
5. `Spacer(1)`
6. `listContainer`（`Container`）
7. `Spacer(1)`
8. hints：`Text(rawKeyHint("↑↓", "navigate") + "  " + keyHint("tui.select.confirm","select") + "  " + keyHint("tui.select.cancel","cancel"), 1, 0)`
9. `Spacer(1)`
10. `DynamicBorder()`
11. 构造末尾 `updateList()`

- 倒计时：`opts.timeout && opts.timeout > 0 && opts.tui` 时构造
  `CountdownTimer(timeout, tui, s => titleText.setText(theme.fg("accent", theme.bold(`${baseTitle} (${s}s)`))), () => onCancelCallback())`。
  到期后**只**调 `onCancel`（`countdown` 字段不清空 —— 与 `RetryStatusIndicator` 不同）。
- 列表行（`updateList()`，重建）：
  ```ts
  const isSelected = i === this.selectedIndex;
  const text = isSelected ? theme.fg("accent", "→ ") + theme.fg("accent", this.options[i])
                          : `  ${theme.fg("text", this.options[i])}`;
  listContainer.addChild(new Text(text, 1, 0));
  ```
  空 options 时无子行（也不显示任何占位消息）。
- `handleInput` 顺序（**【更正】** up/down 是 **clamp**，不是环绕；`k`/`j`/`"\n"` 是硬编码兜底）：
  1. `app.tools.expand` → `onToggleToolsExpanded?.()`（**无论是否有回调都 return**，不再落到后续分支）
  2. `tui.select.up` 或 `data === "k"` → `selectedIndex = Math.max(0, selectedIndex - 1)`
  3. `tui.select.down` 或 `data === "j"` → `selectedIndex = Math.min(options.length - 1, selectedIndex + 1)`
  4. `tui.select.confirm` 或 `data === "\n"` → `const sel = options[selectedIndex]; if (sel) onSelectCallback(sel)`
  5. `tui.select.cancel` → `onCancelCallback()`
  - **注意**：空 options 时第 3 步的 `Math.min(-1, ...)` 会把 `selectedIndex` 变成 **-1**（源码未守卫，
    可复现的边界 bug，Rust 若要照抄需保持一致或在文档里注明）。
- **【逐字】** `navigate`、`select`、`cancel`、`→ `（箭头+空格）、两空格前缀、` (${s}s)`。

### 2.24 `status-indicator.ts`（123 行）

```ts
// 【精读更正】类型名是 StatusIndicatorKind（不是 StatusKind），Kind 是构造函数的**第一个**参数。
export type StatusIndicatorKind = "working" | "retry" | "compaction" | "branchSummary";

export class StatusIndicator extends Loader {
  readonly kind: StatusIndicatorKind;
  constructor(
    kind: StatusIndicatorKind,
    ui: TUI,
    spinnerColorFn: (str: string) => string,
    messageColorFn: (str: string) => string,
    message: string,
    indicator?: WorkingIndicatorOptions,      // 透传给 Loader（spinner 帧/速度等）
  );
  renderInBorder(width: number): string;
  renderSpinnerInBorder(width: number): string;
  dispose(): void;                            // → this.stop()
}

export class WorkingStatusIndicator extends StatusIndicator {
  // 【更正】只有一个可选 colorFn，同时用于 spinner 和 message（没有独立 messageColorFn）
  constructor(ui: TUI, message: string, indicator?: WorkingIndicatorOptions, colorFn?: (text: string) => string);
}
export class RetryStatusIndicator extends StatusIndicator {
  constructor(ui: TUI, attempt: number, maxAttempts: number, delayMs: number);
  override dispose(): void;                   // 先 countdown.dispose() 再 super.dispose()
}
export type CompactionStatusReason = "manual" | "threshold" | "overflow";
export class CompactionStatusIndicator extends StatusIndicator {
  constructor(ui: TUI, reason: CompactionStatusReason, colorFn?: (text: string) => string);
}
export class BranchSummaryStatusIndicator extends StatusIndicator {
  constructor(ui: TUI);
}
export class IdleStatus implements Component {   // 不继承 StatusIndicator
  invalidate(): void;                            // 空实现
  render(width: number): string[];               // [" ".repeat(width), " ".repeat(width)]
}
```

`renderInBorder(width)`：

```ts
const line = super.render(width + 2)[1] ?? "";
const cleaned = line.startsWith(" ") ? line.slice(1).trimEnd() : line.trimEnd();
return truncateToWidth(cleaned, width, "");
```

（因为 `Loader.render` 返回 `["", "<spinner> <message>"]`，所以取 `[1]`；`width + 2` 是为了让 loader 的消息不被
过早截断——loader 自己按 width 截断 message；取回后再裁到目标宽度。`trimEnd` 去掉尾部 padding。）

`renderSpinnerInBorder(width)` = `truncateToWidth(this.getRenderedIndicator(), width, "")`。

- `getRenderedIndicator()` 是 `Loader` 的 protected 方法（返回当前帧字符串，如 `"⠋"`）。
- `dispose()`：`this.stop()`（停 interval）。

各子类的消息 **【逐字】**：

| 子类 | 颜色 | 消息 |
|---|---|---|
| `WorkingStatusIndicator(ui, message, indicator?, colorFn?)` | spinner **与** message 都是 `colorFn ?? accent` / `colorFn ?? muted`（【更正】同一 colorFn 分别包两处，未给时 spinner accent、message muted） | `message` |
| `RetryStatusIndicator(ui, attempt, maxAttempts, delayMs)` | spinner `warning` | `` `Retrying (${attempt}/${maxAttempts}) in ${seconds}s... (${keyText("app.interrupt")} to cancel)` ``，`seconds` 由 `CountdownTimer(ceil(delayMs/1000))` 每秒更新；到期后文本保持 `0s`（宿主会替换成新 indicator） |
| `CompactionStatusIndicator(ui, "manual", colorFn?)` | spinner `colorFn ?? accent`，message `colorFn ?? muted` | `` `Compacting context... (${keyText("app.interrupt")} to cancel)` `` |
| `CompactionStatusIndicator(ui, "threshold"\|"overflow", colorFn?)` | spinner `colorFn ?? accent` |
 `` `${reason === "overflow" ? "Context overflow detected, " : ""}Auto-compacting... (${cancelHint})` ``，`cancelHint = keyText("app.interrupt") + " to cancel"` |
| `BranchSummaryStatusIndicator(ui)` | spinner `accent` | `` `Summarizing branch... (${keyText("app.interrupt")} to cancel)` `` |

`IdleStatus.render(width)` → 两行满宽空格；用于「不显示状态时保持编辑器高度」。

- 测试要点（`status-indicator.test.ts`）：
  - `IdleStatus().render(20)` 恰为 `[" ".repeat(20), " ".repeat(20)]`。
  - 未开启 `CustomEditor({embedWorkingStatus:true})` 时上边框是纯 `"─".repeat(20)`。
  - 开启后 `stripAnsi(editor.render(20)[0]) === "── ⠋ Working ───────"`（20 列），并且
    `topBorder.split(theme.getFgAnsi("thinkingHigh")).length === 5`（4 段 thinkingHigh 着色：`── `、status、
    左侧填充、右侧填充——具体见 2.27 的 border 组装）。
  - 五种 indicator × width `[1,4,10,20,80,120]`：`visibleWidth(editor.render(width)[0]) === width`。
  - `RetryStatusIndicator(tui,1,3,3000)` 初始 `in 3s`，1000ms 后 `in 2s`。
  - `dispose()` 后 `requestRender` 调用次数不再增加。

- **【逐字】** `Retrying (`、`/`、`) in `、`s... (`、` to cancel)`、`Compacting context... (`、
  `Context overflow detected, `、`Auto-compacting... (`、`Summarizing branch... (`、`Working`。

### 2.25 `trust-selector.ts`（134 行）

```ts
// 【精读更正】三个 export：TrustSelection、TrustSelectorOptions、TrustSelectorComponent。
// formatDecision 是**模块私有**（不导出）。
export type TrustSelection = Pick<ProjectTrustOption, "trusted" | "updates">;
export interface TrustSelectorOptions {
  cwd: string;
  savedDecision: ProjectTrustStoreEntry | null;
  projectTrusted: boolean;
  onSelect: (selection: TrustSelection) => void;
  onCancel: () => void;
}
export class TrustSelectorComponent extends Container {   // ← 不实现 Focusable
  constructor(options: TrustSelectorOptions);
  handleInput(data: string): void;
}
```

`ProjectTrustStoreEntry = { path: string; decision: boolean | null }`；
`ProjectTrustUpdate = { path: string; decision: boolean | null }`；
`ProjectTrustOption = { label: string; trusted: boolean; updates: ProjectTrustUpdate[]; savedPath?: string }`。

`formatDecision(trustPath: string | undefined, decision)`（模块私有）：

```ts
if (decision === null) return "none";                    // 源码用 === null（undefined 会走下面并抛错）
const label = decision.decision ? "trusted" : "untrusted";
if (trustPath !== undefined && decision.path !== trustPath) return `${label} (inherited from ${decision.path})`;
return `${label} (${decision.path})`;
```

布局（**逐字**，所有 `Text` paddingX = 1，先 bold 后 fg）：

1. `DynamicBorder()`
2. `Spacer(1)`
3. `Text(theme.fg("accent", theme.bold("Project trust")), 1, 0)`
4. `Text(theme.fg("muted", options.cwd), 1, 0)`
5. `Spacer(1)`
6. `Text(theme.fg("muted", `Saved decision: ${formatDecision(trustOptions[0]?.savedPath, savedDecision)}`), 1, 0)`
   —— **【更正】** `trustPath` 实参是**第一个选项的 `savedPath`**（即 normalizeCwd(cwd)），不是逐项计算。
7. `Text(theme.fg("muted", `Current session: ${projectTrusted ? "trusted" : "untrusted"}`), 1, 0)`
8. `Spacer(1)`
9. `listContainer`（`Container`）
10. `Spacer(1)`
11. hints：`rawKeyHint("↑↓","navigate") + "  " + keyHint("tui.select.confirm","save") + "  " + keyHint("tui.select.cancel","cancel")`
12. `Spacer(1)`
13. `DynamicBorder()`
14. `updateList()`

选项来源：**`getProjectTrustOptions(options.cwd)`** —— **【更正】** 源码**没有**传
`{ includeSessionOnly: true }`（旧稿写有）。顺序固定为：

```
"Trust"                                   trust=true   updates=[{cwd,true}]          savedPath=cwd
"Trust parent folder (<parent>)"          trust=true   updates=[{parent,true},{cwd,null}] savedPath=parent   （仅当有 parent）
"Do not trust"                            trust=false  updates=[{cwd,false}]           savedPath=cwd
```

（`getProjectTrustOptions` 的 `includeSessionOnly` 分支会额外插入
`"Trust (this session only)"` 与 `"Do not trust (this session only)"`，本组件不启用。）

**初始选中**：`Math.max(0, trustOptions.findIndex(o => isSavedOption(o)))` —— 即定位到与 `savedDecision`
完全一致的那一项；找不到则 0。

`isSavedOption(option)`：

```ts
option.savedPath !== undefined &&
this.savedDecision?.decision === option.trusted &&
this.savedDecision.path === option.savedPath
```

—— **【更正】** 这就是「当前」标记的唯一判据（`✓`），并**不**比较 `projectTrusted`，也不识别
`updates.length === 0` 的 session-only 选项。

行格式（**逐字**）：

```ts
const isSelected = i === this.selectedIndex;
const isCurrent = this.isSavedOption(option);
const currentMarker = isCurrent ? theme.fg("accent", "✓ ") : "  ";
const prefix = isSelected ? theme.fg("accent", "→ ") : "  ";
const label = isSelected ? theme.fg("accent", option.label) : theme.fg("text", option.label);
listContainer.addChild(new Text(`${prefix}${currentMarker}${label}`, 1, 0));
```

- 键（**clamp**，不环绕；`k`/`j`/`"\n"` 硬编码兜底）：
  `tui.select.up`|`"k"` → `max(0, i-1)`；`tui.select.down`|`"j"` → `min(len-1, i+1)`；
  `tui.select.confirm`|`"\n"` → `onSelect({ trusted, updates })`（**丢掉 `label`/`savedPath`**）；
  `tui.select.cancel` → `onCancel()`。
- 空列表时 down 会把 `selectedIndex` 压到 **-1**（同 `extension-selector` 的边界问题）。
- **【逐字】** `Project trust`、`Saved decision: `、`Current session: `、`trusted`、`untrusted`、`none`、
  ` (inherited from `、`)`、`Trust`、`Trust parent folder (`、`Trust (this session only)`、`Do not trust`、
  `Do not trust (this session only)`、`navigate`、`save`、`cancel`、`→ `、`✓ `。
- 测试（`trust-selector.test.ts`）：保存为 trusted 时 `Trust` 行带 `✓`；确认返回
  `{trusted:true, updates:[...]}`；继承决策显示 `trusted (inherited from /parent)`；
  `getProjectTrustOptions` 含父目录选项时列表出现 `Trust parent folder (/parent)`（label 由 trust-manager 生成）。

### 2.26 `extension-editor.ts`（141 行）

**【精读更正 · 完整签名】**（原 §2.26 少了第 2 个 `keybindings` 参数与第 8 个 `externalEditorCommand`）：

```ts
export interface ExtensionEditorOptions extends EditorOptions { description?: string }

export class ExtensionEditorComponent extends Container implements Focusable {
  constructor(
    tui: TUI,
    keybindings: KeybindingsManager,          // ← 原文缺失；用于 app.editor.external
    title: string,
    prefill: string | undefined,
    onSubmit: (value: string) => void,
    onCancel: () => void,
    options?: ExtensionEditorOptions,         // 含 description + 所有 EditorOptions
    externalEditorCommand?: string,           // ← 原文缺失；第 8 个**位置**参数，不在 options 里
  );
  handleInput(data: string): void;
  focused: boolean;                           // getter/setter，代理给 editor.focused
  // 【没有 dispose()】（原 §2.26 写的 dispose 不存在）
}
```

外部编辑器命令解析（**逐字**；`externalEditorCommand` 是构造参数，**不是** `options` 字段）：

```ts
this.externalEditorCommand =
  externalEditorCommand ||
  process.env.VISUAL ||
  process.env.EDITOR ||
  (process.platform === "win32" ? "notepad" : "nano");
```

布局（**逐字顺序**）：

1. `DynamicBorder()`（无参数 → 用主题默认 `borderMuted`/`border`）
2. `Spacer(1)`
3. `Text(theme.fg("accent", title), 1, 0)`
4. **若 `description` 为真**：`Spacer(1)` → `Text(theme.fg("text", description), 1, 0)`（**【更正】** description
   前面还有一个 `Spacer(1)`；原文写成「可选 description + Spacer(1)」顺序不对）
5. `Spacer(1)`
6. `Editor(tui, getEditorTheme(), editorOptions)` —— `editorOptions` 是 `options` **去掉 `description` 后的其余字段**；
   `prefill` 为真时 `setText(prefill)`；`editor.onSubmit = (text) => onSubmitCallback(text)`
   （**【更正】** **没有** `editor.onCancel = onCancel`；取消只由本组件的 `handleInput` 处理）
7. `Spacer(1)`
8. `Text(hint, 1, 0)`，`hint = keyHint("tui.select.confirm","submit") + "  " +
   keyHint("tui.input.newLine","newline") + "  " + keyHint("tui.select.cancel","cancel") +
   `  ${keyHint("app.editor.external","external editor")}``
9. `Spacer(1)`
10. `DynamicBorder()`

`handleInput(keyData)`（**顺序即行为**；参数名是 `keyData`）：

1. `getKeybindings().matches(keyData, "tui.select.cancel")` → `onCancelCallback()`。
2. **`this.keybindings.matches(keyData, "app.editor.external")`**（用**注入的** keybindings，不是全局）→
   `void this.handleOpenExternalEditor()`。
3. 否则 `this.editor.handleInput(keyData)`。

- `handleOpenExternalEditor()` 是 **`private async`**，返回 `Promise<void>`。

`handleOpenExternalEditor()`（**【更正】** 判据是 `result.status === "complete"`，不是 `result.kind === "ok"`）：

```ts
const content = this.editor.getText();
this.tui.stop();
try {
  const result = await editInExternalEditor({ command: this.externalEditorCommand, content });
  if (result.status === "complete") this.editor.setText(result.content);
} finally {
  this.tui.start();
  this.tui.requestRender(true);        // 注意参数 true（强制全量重绘）
}
```

- 失败/取消（`status !== "complete"`）→ 内容不变，也不报错（静默）。
- `editInExternalEditor` 来自 `../external-editor.ts`（宿主层），返回 `{ status, content }`。

- **【逐字】** `submit`、`newline`、`cancel`、`external editor`。
- `tui.requestRender(true)` 的 `true` = 强制全量重绘（外部编辑器会刷屏）。

### 2.27 `diff.ts`（147 行）

```ts
// 【精读更正】两个 export；内部函数 parseDiffLine/replaceTabs/renderIntraLineDiff 均不导出。
export interface RenderDiffOptions { filePath?: string; }   // 保留但未使用（参数名 `_options`）
export function renderDiff(diffText: string, _options: RenderDiffOptions = {}): string;
```

`parseDiffLine(line)`（**逐字**）：

```ts
const match = line.match(/^([+-\s])(\s*\d*)\s(.*)$/);
if (!match) return null;                     // 返回 null，不是 undefined
return { prefix: match[1], lineNum: match[2], content: match[3] };   // 字段名是 prefix
```

`replaceTabs(text)`：`text.replace(/\t/g, "   ")`（**三个空格**）。

`renderIntraLineDiff(oldContent, newContent)`（**【精读更正】** 只有**第一个** removed/added 片段剥前导空白）：

```ts
const wordDiff = Diff.diffWords(oldContent, newContent);
let removedLine = "", addedLine = "";
let isFirstRemoved = true, isFirstAdded = true;
for (const part of wordDiff) {
  if (part.removed) {
    let value = part.value;
    if (isFirstRemoved) {
      const leadingWs = value.match(/^(\s*)/)?.[1] || "";
      value = value.slice(leadingWs.length);
      removedLine += leadingWs;
      isFirstRemoved = false;
    }
    if (value) removedLine += theme.inverse(value);
  } else if (part.added) {
    let value = part.value;
    if (isFirstAdded) {
      const leadingWs = value.match(/^(\s*)/)?.[1] || "";
      value = value.slice(leadingWs.length);
      addedLine += leadingWs;
      isFirstAdded = false;
    }
    if (value) addedLine += theme.inverse(value);
  } else {
    removedLine += part.value;
    addedLine += part.value;
  }
}
return { removedLine, addedLine };            // 返回对象字段是 removedLine / addedLine
```

- **【更正】** 旧稿说「每个片段都剥前导空白」——错了，**只有首个** removed / 首个 added 片段剥一次；
  后续片段的空白**照常**包进 `theme.inverse`。
- 首个片段若整段都是空白，`value` 变空 → 不输出 inverse 包裹（`if (value)`）。
- `diffWords` 是 jsdiff 的 `diffWords`（按词边界、忽略空白差异）；Rust 侧需复刻其打分/LCS 行为，见 §6/§8.3。

`renderDiff(diffText, _options = {})`（**逐字**）：

```ts
const lines = diffText.split("\n");
const result: string[] = [];
let i = 0;
while (i < lines.length) {
  const line = lines[i];
  const parsed = parseDiffLine(line);
  if (!parsed) { result.push(theme.fg("toolDiffContext", line)); i++; continue; }   // ← 不 replaceTabs
  if (parsed.prefix === "-") {
    const removedLines = []; while (i < lines.length) { const p = parseDiffLine(lines[i]); if (!p || p.prefix !== "-") break; removedLines.push(p); i++; }
    const addedLines   = []; while (i < lines.length) { const p = parseDiffLine(lines[i]); if (!p || p.prefix !== "+") break; addedLines.push(p); i++; }
    if (removedLines.length === 1 && addedLines.length === 1) {
      const { removedLine, addedLine } = renderIntraLineDiff(replaceTabs(removedLines[0].content),
                                                             replaceTabs(addedLines[0].content));
      result.push(theme.fg("toolDiffRemoved", `-${removedLines[0].lineNum} ${removedLine}`));
      result.push(theme.fg("toolDiffAdded",   `+${addedLines[0].lineNum} ${addedLine}`));
    } else {
      for (const r of removedLines) result.push(theme.fg("toolDiffRemoved", `-${r.lineNum} ${replaceTabs(r.content)}`));
      for (const a of addedLines)   result.push(theme.fg("toolDiffAdded",   `+${a.lineNum} ${replaceTabs(a.content)}`));
    }
  } else if (parsed.prefix === "+") {          // 【更正】独立的 + 行有单独分支
    result.push(theme.fg("toolDiffAdded", `+${parsed.lineNum} ${replaceTabs(parsed.content)}`));
    i++;
  } else {
    result.push(theme.fg("toolDiffContext", ` ${parsed.lineNum} ${replaceTabs(parsed.content)}`));
    i++;
  }
}
return result.join("\n");
```

- 行的前缀 **【逐字】**：`-`、`+`、` `（各接 `lineNum`，再接一个空格，再接内容）。
  注意：未匹配行是**原样** `theme.fg("toolDiffContext", line)`（无前导空格、无 replaceTabs）；
  匹配到的 context 行才补前导空格并替换 tab。
- 颜色 token：`toolDiffContext`、`toolDiffRemoved`、`toolDiffAdded`；行内变更用 `theme.inverse`（无额外颜色，
  由外层 fg 决定前景）。
- **【更正】** 旧稿把「独立 `+` 行」并入 context 分支 —— 源码有独立 `else if (parsed.prefix === "+")` 分支。
- 连续 `-` 块后紧跟非 `+` 行：`-` 已消费并输出，随后重新进入循环处理该行。
- `tool-execution.ts` 的 edit renderer 用它渲染 diff。

### 2.28 `custom-editor.ts`（148 行）

```ts
// 【精读更正】两个 export。onEscape/onCtrlD/onPasteImage/onExtensionShortcut 是**实例公有字段**，
// 不在 options 里。borderColor 继承自基类 Editor（custom-editor.ts 未声明它）。
export type CustomEditorOptions = EditorOptions & {
  embedWorkingStatus?: boolean;
};
export class CustomEditor extends Editor {
  public readonly embedWorkingStatus: boolean;
  public actionHandlers: Map<AppKeybinding, () => void>;   // 构造时 new Map()
  public onEscape?: () => void;
  public onCtrlD?: () => void;
  public onPasteImage?: () => void;
  public onExtensionShortcut?: (data: string) => boolean;  // 返回 true 表示已消费
  constructor(tui: TUI, theme: EditorTheme, keybindings: KeybindingsManager, options?: CustomEditorOptions);
  setWorkingStatusIndicator(indicator: StatusIndicator | undefined): void;   // 只存引用，**不** invalidate
  onAction(action: AppKeybinding, handler: () => void): void;               // → actionHandlers.set
  handleInput(data: string): void;
  protected override renderTopBorder(width: number, hiddenLineCount: number): string;
}
```

- `setWorkingStatusIndicator` **没有** `invalidate()`（旧稿说「存引用并立刻 invalidate」是错的）；
  重绘由宿主的 `tui.requestRender()` 负责。
- `borderColor` 是基类 `Editor` 的公有字段，构造时初始化为 `theme.borderColor`；宿主可随时改
  （主题/思考等级变化时）。因此 `renderTopBorder` 里**没有** `?? fallback`。

`renderTopBorder(width, hiddenLineCount)` 完整算法（**逐字**，注意 `canFitOverflow` 是**闭包**，
在 spinner 降级后会用新的 `statusWidth` 重新求值）：

```ts
if (!this.embedWorkingStatus || !this.workingStatusIndicator || width <= 0) {
  return super.renderTopBorder(width, hiddenLineCount);
}

let status = this.workingStatusIndicator.renderInBorder(Math.max(1, width - 5));
let statusWidth = visibleWidth(status);
if (statusWidth === 0) return super.renderTopBorder(width, hiddenLineCount);

const overflowLabel = hiddenLineCount > 0 ? ` ↑ ${hiddenLineCount} more ` : undefined;
const overflowLabelWidth = overflowLabel ? visibleWidth(overflowLabel) : 0;
const overflowStart = Math.floor((width - overflowLabelWidth) / 2);      // 无 label 时也计算（= floor(width/2)）
const canFitOverflow = () =>
  overflowLabel !== undefined &&
  overflowLabelWidth + 2 <= width &&
  overflowStart - (3 + statusWidth + 1) >= 1;                            // 3 + statusWidth + 1 == leftBlockWidth

if (overflowLabel && !canFitOverflow()) {          // ← 只把 status 降级为 spinner，**不 return**
  status = this.workingStatusIndicator.renderSpinnerInBorder(width);
  statusWidth = visibleWidth(status);
}

if (canFitOverflow()) {
  const leftBlockWidth = 3 + statusWidth + 1;
  return (
    this.borderColor("── ") +
    status +
    this.borderColor(
      ` ${"─".repeat(overflowStart - leftBlockWidth)}${overflowLabel}${"─".repeat(width - overflowStart - overflowLabelWidth)}`,
    )
  );
}

if (width >= statusWidth + 5) {
  return this.borderColor("── ") + status + this.borderColor(` ${"─".repeat(width - statusWidth - 4)}`);
}

status = this.workingStatusIndicator.renderSpinnerInBorder(width);
statusWidth = visibleWidth(status);
const prefixWidth = Math.min(3, Math.max(0, width - statusWidth));
return (
  this.borderColor("─".repeat(prefixWidth)) +
  status +
  this.borderColor("─".repeat(Math.max(0, width - prefixWidth - statusWidth)))
);
```

- 输出总宽严格 = `width`（测试断言 `visibleWidth === width`，width ∈ {1,4,10,20,80,120}）。
- 默认 `borderColor` 是基类主题的 `theme.borderColor`；测试里显式设为 `theme.getThinkingBorderColor("high")`，
  并断言 `"── "`、status、填充都是 `thinkingHigh` 色，段数 4（`split(...).length === 5`）。
- `renderSpinnerInBorder` 只取 spinner 字符（无消息），用于窄宽度/溢出标签放不下时。
- **【更正】** 旧稿把「溢出标签放不下」写成一个直接 return spinner 边框的分支 —— 实际是先降级 status 为
  spinner，再继续走后面三个分支；最终可能仍是完整的 `"── " + status + " " + 填充` 形态。

`handleInput(data)` 判定顺序（**顺序即行为**）：

1. `this.onExtensionShortcut?.(data)` 返回 true → return。
2. `keybindings.matches(data, "app.clipboard.pasteImage")` → `this.onPasteImage?.(); return;`（**无条件 return**）。
3. `app.interrupt`：
   ```ts
   if (!this.isShowingAutocomplete()) {
     const handler = this.onEscape ?? this.actionHandlers.get("app.interrupt");
     if (handler) { handler(); return; }
   }
   super.handleInput(data);   // 补全中 / 无 handler → 交给基类
   return;
   ```
   **注意**：无 handler 时也**不**继续往下（`return` 在 super 之后）。
4. `app.exit`：`if (this.getText().length === 0) { const h = this.onCtrlD ?? actionHandlers.get("app.exit"); if (h) { h(); return; } }`
   —— 非空时**不 return**，落到 editor 处理（Ctrl+D = delete-char-forward）。
5. `tui.editor.historyPrevious` / `tui.editor.historyNext` → `super.handleInput(data); return;`
   （显式历史绑定优先于 app action，见测试）。
6. 遍历 `actionHandlers`：`action !== "app.interrupt" && action !== "app.exit" && keybindings.matches(data, action)`
   → `handler(); return;`。
7. 兜底 `super.handleInput(data)`。

- 测试（`custom-editor-history-keybindings.test.ts`）：显式 `tui.editor.historyPrevious` 绑定必须优先于
  `app.model.cycleBackward`，即第 5 步先于第 6 步。注意：`app.model.cycleBackward` 在 Windows 上是 `alt+p`，
  其它平台是 `shift+ctrl+p`；测试显式绑定 `alt+p` 到 history 后，按 `alt+p` 应翻历史而不是切模型。

### 2.29 `tool-execution.ts`（417 行）— 工具调用/结果渲染容器

```ts
export interface ToolExecutionOptions {
  showImages?: boolean;      // 默认 true
  imageWidthCells?: number;   // 默认 60
  outputPad?: number;         // 默认 1
}
export class ToolExecutionComponent extends Container {
  constructor(toolName: string, toolCallId: string, args: any,
              options: ToolExecutionOptions = {},
              toolDefinition: ToolRenderers | ToolDefinition | undefined,
              ui: TUI, cwd: string);
  updateArgs(args: any): void;
  markExecutionStarted(): void;
  setArgsComplete(): void;
  updateResult(result: { content: ..., details?: any, isError: boolean, durationMs?: number }, isPartial?: boolean): void;
  setExpanded(expanded: boolean): void;
  setOutputPad(outputPad: number): void;
  setShowImages(show: boolean): void;
  setImageWidthCells(width: number): void;
  override invalidate(): void;
  override render(width: number): string[];
  override handleMouse(event: TuiMouseEvent): ...;
  // 内部：getCallRenderer/getResultRenderer/hasRendererDefinition/getRenderShell/getRenderContext/
  //       createCallFallback/createResultFallback/createResultRegion/updateDisplay/getTextOutput/formatToolExecution
  private hideComponent = false;
}
```

常量 `FALLBACK_PREVIEW_LINES = 10`。

**字段与默认值**（逐字）：

| 字段 | 初值 | 说明 |
|---|---|---|
| `contentBox` | `new Box(1, 1, t => theme.bg("toolPendingBg", t))` | 「default shell」容器（paddingY=1） |
| `contentText` | `new Text("", 1, 1, t => theme.bg("toolPendingBg", t))` | 无 toolDefinition 时的通用兜底 |
| `contentTextRegion` | `createResultRegion(contentText)` | 点击展开区域 |
| `selfRenderContainer` | `new Container()` | `renderShell === "self"` 时用 |
| `selfRenderHeight` | `0` | 上次 self 渲染的行数（鼠标坐标换算） |
| `rendererState` | `{}` | 跨帧共享给 renderer 的 `state`（用于流式累计） |
| `imageSources` | `[]` | 复用已转码图片的输入快照 |
| `expanded` | `false` | |
| `isPartial` | `true` | 构造后、收到最终结果前 |
| `executionStarted` | `false` | |
| `argsComplete` | `false` | |
| `argsPending` | `false` | #9265 的逐帧合帧标志 |
| `result` | `undefined` | |
| `showImages` | `options.showImages ?? true` | |
| `imageWidthCells` | `options.imageWidthCells ?? 60` | |
| `outputPad` | `options.outputPad ?? 1` | |

**构造**：

1. `super()`（Container）。
2. `addChild(new Spacer(1))`。
3. 创建三个 shell 变体（全部创建，见上表）。
4. 若 `hasRendererDefinition()`（即 `toolDefinition !== undefined`）→ `addChild(renderShell === "self" ? selfRenderContainer : contentBox)`；
   否则 `addChild(contentTextRegion)`。
5. `updateDisplay()`。

**`getRenderContext(lastComponent)`** 返回的 `ToolRenderContext`（**逐字字段**）：

```ts
{
  args, toolCallId,
  invalidate: () => { this.invalidate(); this.ui.requestRender(); },
  lastComponent,
  state: this.rendererState,
  cwd,
  executionStarted,
  argsComplete,
  isPartial,
  expanded,
  showImages,
  isError: this.result?.isError ?? false,
  durationMs: this.isPartial ? undefined : this.result?.durationMs,
  outputPad,
}
```

- `lastComponent` 让 renderer 复用上一帧组件（例如 `Text` 就地 `setText`），是实现流式低开销的关键；
  每次调用 renderer 后都更新 `callRendererComponent` / `resultRendererComponent`。
- renderer 抛异常 → 该槽位置 `undefined` 并改用兜底组件（**不崩溃**）。

**兜底渲染**：

- `createCallFallback()`：`new Text(formatToolCallWithArgs(toolName, args, theme, expanded), 0, 0)`
  （`formatToolCallWithArgs` 在 `core/tools/render-utils.ts`，单元 ca-tools-compaction-export）。
- `createResultFallback()`：
  ```ts
  const output = this.getTextOutput();       // getTextOutput from render-utils
  if (!output) return undefined;
  const lines = output.split("\n");
  const displayLines = this.expanded ? lines : lines.slice(0, 10);
  const remaining = lines.length - displayLines.length;
  let text = displayLines.map(l => theme.fg("toolOutput", l)).join("\n");
  if (remaining > 0) text += `${theme.fg("muted", `\n... (${remaining} more lines,`)} ${keyHint("app.tools.expand","to expand")}${theme.fg("muted",")")}`;
  return new Text(text, 0, 0);
  ```
  **【逐字】** `\n... (`、` more lines,`、`to expand`、`)`。
- `createResultRegion(component)`：`new MouseRegion(component, event => ...)`，仅当
  **已有 result** 且 `event.type === "click"` 且 `event.button === "left"` 时 `setExpanded(!expanded)` 并返回
  `{ handled: true }`；否则 `undefined`。

**公共方法**：

- `updateArgs(args)`：存 args、`argsPending = true`、`ui.requestRender()`。**不**立即重建（#9265：两个渲染帧之间
  多个 arg delta 只渲染一次）。
- `markExecutionStarted()`：`executionStarted = true`；`updateDisplay()`；`requestRender()`。
- `setArgsComplete()`：`argsComplete = true`；`updateDisplay()`；`requestRender()`。
- `updateResult(result, isPartial = false)`：存 result 与 isPartial；`updateDisplay()`（宿主随后 requestRender）。
- `setExpanded` / `setOutputPad` / `setShowImages`：存值 + `updateDisplay()`。
- `setImageWidthCells(width)`：`Math.max(1, Math.floor(width))` + `updateDisplay()`。
- `invalidate()`：`super.invalidate(); updateDisplay();`

**`render(width)`**：

```ts
if (this.argsPending) this.updateDisplay();          // 逐帧应用 latest args
if (this.hideComponent) return [];                   // 零高度

if (hasRendererDefinition() && getRenderShell() === "self") {
  const contentLines = this.selfRenderContainer.render(width);
  this.selfRenderHeight = contentLines.length;
  if (contentLines.length === 0 && this.imageComponents.length === 0) return [];
  const lines: string[] = [];
  if (contentLines.length > 0) { lines.push(""); lines.push(...contentLines); }   // 前置空行
  for (let i = 0; i < this.imageComponents.length; i++) {
    const spacer = this.imageSpacers[i]; if (spacer) lines.push(...spacer.render(width));
    const img = this.imageComponents[i]; if (img) lines.push(...img.render(width));
  }
  return lines;
}
return super.render(width);
```

- **self shell 的前置空行**是转录间距：`lines.push("")` 在内容之前。
- **default shell** 由 `super.render`（Container）拼装：`Spacer(1)` 在前，再 `contentBox`（Box 自带 paddingY=1）。

**`handleMouse(event)`**（仅 self shell 有自定义包装）：

```ts
if (!hasRendererDefinition() || getRenderShell() !== "self") return super.handleMouse(event);
if (event.y <= 0 || event.y > this.selfRenderHeight) return undefined;
return this.selfRenderContainer.handleMouse({ ...event, y: event.y - 1, height: this.selfRenderHeight });
```

（`y` 减 1 因为 render 在最前插了一行空行；`y` 是 1-based，`y <= 0` 无效。）

**`updateDisplay()`** 完整流程（每次状态变化都跑，是 Renderer 契约的执行点）：

1. `argsPending = false`。
2. `bgFn`：
   ```ts
   isPartial ? t => theme.bg("toolPendingBg", t)
   : result?.isError ? t => theme.bg("toolErrorBg", t)
   : t => theme.bg("toolSuccessBg", t)
   ```
   **注意**：未收到 result 但 `isPartial=false` 时走 success 背景（实际不会发生）。
3. `hasContent = false; hideComponent = false;`
4. 有 toolDefinition：
   - `renderContainer = renderShell === "self" ? selfRenderContainer : contentBox`。
   - 若是 `Box` → `setBgFn(bgFn)` + `setPaddingX(outputPad)`（self shell 是 Container，不设背景）。
   - `renderContainer.clear()`。
   - call 渲染：
     - 无 `renderCall` → `addChild(createResultRegion(createCallFallback())); hasContent = true;`
     - 有 → `try { c = renderCall(args, theme, getRenderContext(callRendererComponent)); callRendererComponent = c;
       addChild(createResultRegion(c)); hasContent = true; } catch { callRendererComponent = undefined;
       addChild(createResultRegion(createCallFallback())); hasContent = true; }`
   - result 渲染（仅当 `this.result`）：
     - 无 `renderResult` → `c = createResultFallback(); if (c) { addChild(createResultRegion(c)); hasContent = true; }`
     - 有 → `try { c = renderResult({content, details}, {expanded, isPartial}, theme, getRenderContext(resultRendererComponent));
       resultRendererComponent = c; addChild(createResultRegion(c)); hasContent = true; } catch { resultRendererComponent = undefined;
       c = createResultFallback(); if (c) { ... } }`
5. 无 toolDefinition：
   - `contentText.setCustomBgFn(bgFn); contentText.setPaddingX(outputPad);`
   - `contentText.setText(formatToolExecution())`；`hasContent = true`。
   - `formatToolExecution()`：
     ```ts
     let text = theme.fg("toolTitle", theme.bold(this.toolName));
     const content = JSON.stringify(this.args, null, 2);
     if (content) text += `\n\n${content}`;
     const output = this.getTextOutput();
     if (output) text += `\n${output}`;
     return text;
     ```
     **注意**：`JSON.stringify(undefined, null, 2)` → `undefined`（falsy）→ 不追加；`args` 为 `null` → `"null"`（truthy）→ 追加。
6. 图片重建（**关键复用逻辑**）：
   ```ts
   const previousImages = this.imageComponents;
   const previousSources = this.imageSources;
   for (const img of this.imageComponents) this.removeChild(img);
   this.imageComponents = []; this.imageSources = [];
   for (const spacer of this.imageSpacers) this.removeChild(spacer);
   this.imageSpacers = [];

   if (this.result) {
     const imageBlocks = this.result.content.filter(c => c.type === "image");
     const caps = getCapabilities();
     for (const img of imageBlocks) {
       if (caps.images && this.showImages && img.data && img.mimeType) {
         const spacer = new Spacer(1); this.addChild(spacer); this.imageSpacers.push(spacer);
         const source = { data: img.data, mimeType: img.mimeType, widthCells: this.imageWidthCells };
         const index = this.imageComponents.length;
         const previous = previousSources[index];
         const imageComponent =
           previous?.data === source.data && previous.mimeType === source.mimeType && previous.widthCells === source.widthCells
             ? previousImages[index]
             : new Image(source.data, source.mimeType,
                         { fallbackColor: s => theme.fg("toolOutput", s) },
                         { maxWidthCells: source.widthCells });
         if (source.mimeType !== "image/png") {
           ensurePngTranscoder(() => { this.invalidate(); this.ui.requestRender(); });
         }
         this.imageComponents.push(imageComponent); this.imageSources.push(source); this.addChild(imageComponent);
       }
     }
   }
   ```
   - 复用判定按**索引**比较 `{data, mimeType, widthCells}`：内容完全相同才复用（保留 Image 内部已转码的 PNG，
     避免每帧重转码 → `tool-execution-component.test.ts` 的 "non-PNG transcode once"）。
   - `ensurePngTranscoder(cb)`：按需初始化 WASM 转码器（非 PNG 图片）；完成时调用 cb 让组件重渲染。
     只有非 PNG 才触发。`caps.images === false` 时完全不创建图片组件。
7. `if (hasRendererDefinition() && !hasContent && imageComponents.length === 0) hideComponent = true;`
   → `render()` 返回 `[]`（零高度，测试 "empty self-render zero height"）。

**`getTextOutput()`** = `getRenderedTextOutput(this.result, this.showImages)`
（`render-utils.getTextOutput`：拼接 text 块，`showImages` 时把 image 块替换成占位文本如 `[Image: ...]`）。

**测试要覆盖的行为**（`tool-execution-component.test.ts` 26 项，逐条核对清单）：

1. 非 PNG 图片只转码一次（多次 render/invalidate 不重复）。
2. 自定义 renderCall/renderResult 叠加渲染（两者都出现，顺序 call 后 result）。
3. self shell 空自渲染 → 0 行高。
4. built-in renderer 可被覆盖（definition 优先于 fallback）。
5. legacy `file_path` 参数（read 工具旧字段）仍能渲染。
6. bash 工具初始 partial 状态输出为空（不显示占位）。
7. 不重复显示截断详情（`.truncated` 与 `fullOutputPath` 只出现一次）。
8. 时长格式（`durationMs` → `1.2s` 等，由 `render-utils` 负责）。
9. 不重复 header。
10. `offset`/`limit` 为 null 时不显示行范围。
11. renderer 槽位可继承（`lastComponent`）。
12. `state` 跨帧共享（同一对象）。
13. context 中的 `args` 是当前 args。
14. 无 renderCall 时的 fallback 调用头。
15. 折叠态 fallback 输出。
16. 尾部空行裁剪（Trailing blank trimming）。
17. 每帧参数合并（#9265：多个 `updateArgs` 只触发一次 renderer 调用）。
18. read 错误结果不做语法高亮。
19. 点击展开（MouseRegion）。
20. compact read 结果 + 展开提示中的 read range。
21-26. 其余组合（见源码 test 文件）。

- **宽度不变式**：self shell 完全由 renderer 保证；default shell 的 Box 会按其宽度约束子组件。
  `contentText` 的 Text 自带换行。Rust 侧不得在 `ToolExecutionComponent` 层面再截断（否则重复截断测试会失败）。

### 2.30 `model-selector.ts`（423 行）

```ts
// 【精读更正】scopedModels 是 { model, thinkingLevel? }[]；defaultModel 是 { provider, id } 引用
export class ModelSelectorComponent extends Container implements Focusable {
  constructor(
    tui: TUI,
    currentModel: Model<any> | undefined,
    modelRuntime: ModelRuntime,
    scopedModels: ReadonlyArray<{ model: Model<any>; thinkingLevel?: string }>,
    onSelect: (model: Model<any>) => void,
    onCancel: () => void,
    initialSearchInput?: string,
    onSelectAsDefault?: (model: Model<any>) => void,
    defaultModel?: { provider: string; id: string },
  );
  handleInput(data: string): void;
  dispose(): void;      // 幂等：closed 后直接 return；clearTimeout + abort
  focused: boolean;     // getter/setter 代理给 searchInput.focused
}
```

- `onSelectAsDefault` 的注释在源码里写的是「`app.models.save`（`onSelectAsDefault` 存在时）→
  `dispose(); onSelectAsDefault(selectedModel)`」；`dispose()` 会把 `closed = true` 并 abort 刷新。
- 构造末尾：`loadModelsFromSnapshot()` →（有 `initialSearchInput` 则 `filterModels(it)` 否则 `updateList()`）
  → `tui.requestRender()` → **`void this.refreshModels()`（总是后台刷新，不只 `initialSearchInput === undefined` 时）**。

**构造期子组件**（顺序）：

1. `DynamicBorder()`
2. `Spacer(1)`
3. 作用域提示：
   - `scopedModels.length > 0` 时：
     - `Text(getScopeText(), 0, 0)` —— **paddingX = 0**，其中 `getScopeText()` 构造
       `${theme.fg("muted","Scope: ")}` + `theme.fg(scope==="all"?"accent":"muted","all")` + `theme.fg("muted"," | ")`
       + `theme.fg(scope==="scoped"?"accent":"muted","scoped")`。
       **【更正】** 非激活色是 `"muted"`，不是 `"dim"`；`active` 只对 scoped 判断（all 用同一表达式）。
     - `Text(keyHint("tui.input.tab","scope") + theme.fg("muted"," (all/scoped)"), 0, 0)`
   - 否则：`Text(theme.fg("warning", "Only showing models from configured providers. Use /login to add providers."), 0, 0)`
4. `Spacer(1)`
5. 搜索 `Input`：`initialSearchInput` 存在时 `setValue(initialSearchInput)`；**`onSubmit`（不是 `onChange`）**
   选中 `filteredModels[selectedIndex]` 并 `handleSelect(...model)`。**输入并不会触发过滤**——过滤发生在
   `handleInput` 的 else 分支（`searchInput.handleInput(keyData)` 之后显式 `filterModels(getValue())`）。
6. `Spacer(1)`
7. `listContainer`
8. `Spacer(1)`
9. 若 `onSelectAsDefault`：
   `Text(theme.fg("dim", `  ${keyDisplayText("tui.select.confirm")} to select · ${keyDisplayText("app.models.save")} to set as default · ${keyDisplayText("tui.select.cancel")} to cancel`), 0, 0)`
10. `DynamicBorder()`

**【逐字】** `Scope: `、`all`、` | `、`scoped`、`scope`、` (all/scoped)`、
`Only showing models from configured providers. Use /login to add providers.`、
`  `、` to select · `、` to set as default · `、` to cancel`。

**模型来源与排序**：

- `loadModelsFromSnapshot()`（**逐字**）：
  ```ts
  const models = modelRuntime.getAvailableSnapshot().map(m => ({ provider: m.provider, id: m.id, model: m }));
  this.allModels = this.sortModels(models);
  this.scopedModels = this.scopedModels.map(s => {
    const refreshed = modelRuntime.getModel(s.model.provider, s.model.id);
    return refreshed ? { ...s, model: refreshed } : s;      // 拿不到新实例则保留旧对象
  });
  this.scopedModelItems = this.scopedModels.map(s => ({ provider: ..., id: ..., model: s.model }));
  this.activeModels = this.scope === "scoped" ? this.scopedModelItems : this.allModels;
  this.filteredModels = this.activeModels;
  const currentIndex = this.filteredModels.findIndex(i => modelsAreEqual(this.currentModel, i.model));
  this.selectedIndex = currentIndex >= 0 ? currentIndex
    : Math.min(this.selectedIndex, Math.max(0, this.filteredModels.length - 1));
  ```
- 排序（`sortModels`，**逐字**）：`[...models].sort`，比较器依次：当前模型优先（`modelsAreEqual`）→
  默认模型其次（`isDefaultModel`，比 provider+id）→ **`a.provider.localeCompare(b.provider)`**。
  **【更正】** 源码注释说「then by provider」，**没有** id 级 tie-break；`Array.sort` 稳定 → 同 provider 内保持
  快照顺序。
- 作用域过滤：`scope === "scoped"` 时只用 `scopedModels` 解析出的模型；`"all"` 用全部。
- 初始 scope：`scopedModels.length > 0 ? "scoped" : "all"`。
- `app.models.save`（`onSelectAsDefault` 存在时）→ `dispose(); onSelectAsDefault(selectedModel)`。
- `tui.input.tab` → `setScope(scope === "all" ? "scoped" : "all")`（同值直接 return）；`setScope` 会重算
  `activeModels`、把选中定位到当前模型（找不到则 0）、再 `filterModels(searchInput.getValue())`。
  **注意**：`setScope` 对 `scoped` 无「空列表守卫」——若无 scopedModels，列表会变空。

**搜索/过滤**（`filterModels(query)`，**【精读更正】** 用的是 tui 的 `fuzzyFilter`，不是 `includes`）：

```ts
if (query) {
  const filtered = fuzzyFilter(this.activeModels, query, (item) => {
    const defaultText = this.isDefaultModel(item.model) ? " default" : "";
    return `${getModelSelectorSearchText({ id: item.id, provider: item.provider, name: item.model.name })}${defaultText}`;
  });
  if (this.isDefaultSearch(query)) {
    const defaultItems = this.activeModels.filter((item) => this.isDefaultModel(item.model));
    const defaultKeys = new Set(defaultItems.map((item) => `${item.provider}\0${item.id}`));
    this.filteredModels = [...defaultItems, ...filtered.filter((item) => !defaultKeys.has(`${item.provider}\0${item.id}`))];
  } else this.filteredModels = filtered;
} else this.filteredModels = this.activeModels;
this.selectedIndex = query ? 0 : Math.min(this.selectedIndex, Math.max(0, this.filteredModels.length - 1));
this.updateList();
```

- `isDefaultSearch(query)`：`normalized = query.trim().toLowerCase()`；`normalized.length > 0 &&
  "default".startsWith(normalized)`（所以 `d`、`de`、…、`default` 都命中）。
- `isDefaultModel(model)`：`defaultModel?.provider === model.provider && defaultModel.id === model.id`
  （**不用** `modelsAreEqual`）。
- `setScope(scope)`：同值直接 return；否则 `activeModels = scope === "scoped" ? scopedModelItems : allModels`；
  `selectedIndex` = 当前模型在 `activeModels` 中的下标（`modelsAreEqual` 查找），找不到则 **0**；
  再 `filterModels(searchInput.getValue())` 并刷新 `scopeText`。
- **【更正】** 搜索文本末尾会追加 `" default"`（仅默认模型），因此输入 `default` 时 fuzzyFilter 也能命中。
- **选中项重置**：查询非空时 `selectedIndex = 0`；查询清空时保持 0（不回跳）。测试
  `7209-model-selector-filter-resets-selection` 断言输入过滤后选中回到第一项。
- `getModelSelectorSearchText` 的搜索串以 **provider 优先**，因此 `anthropic` 能匹配所有 anthropic 模型。

**列表渲染**（`updateList()`，重建 `listContainer`）：

- `maxVisible = 10`；`start = Math.max(0, Math.min(selectedIndex - Math.floor(maxVisible/2), filtered.length - maxVisible))`。
- 行文本（**逐字**）：
  ```ts
  const defaultBadge = isDefault ? theme.fg("muted", " · default") : "";
  const cursor        = isSelected ? theme.fg("accent", "→ ") : "  ";
  const currentMarker = isCurrent ? theme.fg("accent", "✓ ") : "  ";
  const modelText     = isSelected ? theme.fg("accent", item.id) : item.id;   // 【更正】未选中是裸 id
  const name          = getDistinctModelName({ id: item.id, name: item.model.name });
  const nameText      = name ? theme.fg("muted", ` (${name})`) : "";
  const providerBadge = theme.fg("muted", `[${item.provider}]`);
  const line = `${cursor}${currentMarker}${modelText}${nameText} ${providerBadge}${defaultBadge}`;
  ```
  （**【精读更正】** 实参是 `getDistinctModelName({ id: item.id, name: item.model.name })` —— **单参**，
  第二个参数 `allModels` 不存在。`model-search.ts` 的判定：把 `id` 与 `name` 都按 `/[^\p{L}\p{N}]+/u`
  小写分词；若 name 的**每个词**都被 id 覆盖（整词命中，或长度 > 1 且是压缩 id 的子串）→ 返回 `undefined`
  （名字只是 id 的换写法）；否则返回**原 name**（含空格/大小写）。）
- 分支顺序（**逐字**）：
  1. `if (this.errorMessage)`：按 `\n` 拆分，每行 `Text(theme.fg("error", line), 0, 0)`；**此时不渲染空列表提示**。
  2. `else if (this.filteredModels.length === 0)`：`Text(theme.fg("muted", "  No matching models"), 0, 0)`。
  3. `else`：`Spacer(1)` + `Text(theme.fg("muted", `  Model Name: ${selected.model.name}`), 0, 0)`；
     `selected = filteredModels[selectedIndex]`（越界时为 undefined —— 源码未防护，仅当列表非空才进此分支）。
- 滚动指示：`if (startIndex > 0 || endIndex < filteredModels.length)` 追加
  `Text(theme.fg("muted", `  (${selectedIndex+1}/${filteredModels.length})`), 0, 0)`（**【更正】** muted 不是 dim）。
- 刷新状态：`if (refreshStatusMessage)` 追加 `Spacer(1)` +
  `Text(theme.fg(refreshStatusSuccess ? "success" : "muted", `  ${refreshStatusMessage}`), 0, 0)`。
- 所有 `Text` 均 `(text, 0, 0)`（paddingX = 0，行内前导空格由字符串自带）。

**刷新模型目录**（`refreshModels()`，构造末尾**总是** `void` 调用一次 —— 【更正】不是仅在
`initialSearchInput === undefined` 时）：

```ts
const timeoutMs = 15_000;
let timedOut = false;
this.refreshTimeout = setTimeout(() => { timedOut = true; this.refreshAbortController.abort(); }, timeoutMs);
try {
  const result = await refreshModelCatalogs(this.modelRuntime, this.refreshAbortController.signal);
  if (this.closed) return;
  this.refreshStatusMessage = "";
  if (result.aborted && timedOut) {
    this.errorMessage = "Model refresh timed out; showing cached models.";
  } else if (result.errors.size === 1) {
    this.errorMessage = `Could not refresh ${result.errors.keys().next().value}; showing cached models.`;
  } else if (result.errors.size > 1) {
    this.errorMessage = `Could not refresh ${result.errors.size} model catalogs (${[...result.errors.keys()].join(", ")}); showing cached models.`;
  } else {
    this.errorMessage = this.modelRuntime.getError();
    if (!this.errorMessage) { this.refreshStatusMessage = "Model catalogs refreshed."; this.refreshStatusSuccess = true; }
  }
  this.loadModelsFromSnapshot();
  this.filterModels(this.searchInput.getValue());
  this.tui.requestRender();
} catch (error) {
  if (this.closed) return;
  this.refreshStatusMessage = "";
  this.errorMessage = timedOut
    ? "Model refresh timed out; showing cached models."
    : `Could not refresh model catalogs: ${error instanceof Error ? error.message : String(error)}`;
  this.updateList();
  this.tui.requestRender();
} finally {
  if (this.refreshTimeout) clearTimeout(this.refreshTimeout);
}
```

- **【更正】** 不存在 `setStatus()` 方法、不存在 `result.failed` 数组；`refreshAbortController` 是构造期
  就创建好的字段（`readonly`），不是每次 new。失败信息写入 **`errorMessage`** 并渲染成 `error` 色行；
  成功才写入 `refreshStatusMessage`。超时**只** `abort()`，`timedOut` 标志区分「超时」与「调用方取消」。
- 字段初值：`refreshStatusMessage = "Refreshing model catalogs…"`（**U+2026 省略号**），
  `refreshStatusSuccess = false`。构造末尾先 `updateList()` 再 `void refreshModels()`，而清空发生在
  `await` 之后 → **首次渲染确实会在列表底部显示 `  Refreshing model catalogs…`（muted）**。
- 失败/成功后都会 `loadModelsFromSnapshot()` + `filterModels(当前输入)`（catch 分支只 `updateList()`）。

- `refreshModelCatalogs`（`model-catalog-refresh.ts`）：进程内共享协调器（`WeakMap<ModelRuntime, ...>`），
  多组件同时刷新时只有一个真实网络请求；等待者用引用计数；`raceWithAbortSignal(fn, signal)`。
  Rust 侧等价：全局 `Mutex<HashMap<RuntimeId, Arc<RefreshState>>>` + `Notify`，见 §5。
- **【逐字】** `Model refresh timed out; showing cached models.`、`Model catalogs refreshed.`、
  `Could not refresh `、`; showing cached models.`、` model catalogs (`、`); showing cached models.`、
  `Could not refresh model catalogs: `、`Refreshing model catalogs…`。
- **测试**（`model-selector.test.ts` 4 项）：当前模型在浏览时仍有 `✓`；显示 distinct name；
  配置了保存键位时提示含 `Ctrl+S`；**列出每一个失败的 catalog**（多失败时用 `, ` 连接）。

**`handleInput(data)`**（**逐字**）：

```ts
if (kb.matches(keyData, "tui.input.tab")) {
  if (this.scopedModelItems.length > 0) {
    const nextScope = this.scope === "all" ? "scoped" : "all";
    this.setScope(nextScope);
    if (this.scopeHintText) this.scopeHintText.setText(this.getScopeHintText());
  }
  return;
}
if (kb.matches(keyData, "tui.select.up")) {
  if (this.filteredModels.length === 0) return;
  this.selectedIndex = this.selectedIndex === 0 ? this.filteredModels.length - 1 : this.selectedIndex - 1;
  this.updateList();
} else if (kb.matches(keyData, "tui.select.down")) {
  if (this.filteredModels.length === 0) return;
  this.selectedIndex = this.selectedIndex === this.filteredModels.length - 1 ? 0 : this.selectedIndex + 1;
  this.updateList();
} else if (kb.matches(keyData, "tui.select.confirm")) { /* 选中 filteredModels[selectedIndex] → handleSelect */ }
else if (kb.matches(keyData, "tui.select.cancel")) { this.dispose(); this.onCancelCallback(); }
else if (kb.matches(keyData, "app.models.save") && this.onSelectAsDefaultCallback) { /* dispose(); 回调 */ }
else {
  this.searchInput.handleInput(keyData);
  this.filterModels(this.searchInput.getValue());
}
```

- **【更正】** tab 的守卫是 `scopedModelItems.length > 0`（不是 `scopedModels.length`）；`setScope` 内部
  对同值直接 return，所以 tab 实际是切换。每次 tab 后都会 `scopeHintText.setText(...)`（**内容不变**，
  `getScopeHintText()` 只是 keyHint + `" (all/scoped)"`，不随 scope 变化 —— 这行是冗余刷新）。
- 上下**环绕**（wrap，用「是否在端点」的三元，不是取模）；空列表时 up/down 直接 return。
- `handleSelect()`：`dispose(); onSelectCallback(model)`。
- `dispose()`：`if (this.closed) return; this.closed = true;` 然后 `clearTimeout(refreshTimeout)` +
  `refreshAbortController.abort()`。**没有**「状态定时器」；超时定时器是唯一定时器。
- `getSearchInput(): Input`（public，测试/IME 用）。
- 另有 `handleSelect` 走的是 `filteredModels[selectedIndex]`，若越界（undefined）则不动作。

### 2.31 `scoped-models-selector.ts`（403 行）

> **⚠ 本节多处有误（导出面、布局 paddingX、列表行颜色、`○` 状态符、`refreshStatus` 类型），
> 以 §2.53.2 为准。** 下面保留初稿供对照；凡与 §2.53.2 冲突一律按 §2.53.2。

```ts
// 【精读更正】本文件只有 3 个 export：ModelsConfig、ModelsCallbacks、ScopedModelsSelectorComponent。
// 没有 `EnabledModelIds` 这个导出类型 —— 内部叫 `EnabledIds`（模块私有 type alias）。
export interface ModelsConfig {
  allModels: Model<any>[];
  enabledModelIds: string[] | null;
  refreshStatus?: string;               // 纯字符串（不是 { message, kind }）
}
export interface ModelsCallbacks {
  onChange: (enabledModelIds: string[] | null) => void | Promise<void>;
  onPersist: (enabledModelIds: string[] | null) => void | Promise<void>;
  onCancel: () => void;
}
export class ScopedModelsSelectorComponent extends Container implements Focusable {
  constructor(config: ModelsConfig, callbacks: ModelsCallbacks);
  updateModels(models: readonly Model<any>[], enabledModelIds?: string[] | null): void;
  setRefreshStatus(message: string, kind: "muted" | "success" | "warning"): void;   // 两个参数都必填
  getSearchInput(): Input;
  handleInput(data: string): void;
  focused: boolean;                    // getter/setter 代理 searchInput.focused
}
```

**纯函数（模块私有，不导出；完整逐字实现见 §2.53.2）**：

```ts
type EnabledIds = string[] | null;                 // 模块私有
function isEnabled(enabledIds, id): boolean;
function normalizeEnabled(result: string[], allIds: string[]): EnabledIds;
function toggle(enabledIds, allIds, id): EnabledIds;
function enableAll(enabledIds, allIds, targetIds?): EnabledIds;
function clearAll(enabledIds, allIds, targetIds?): EnabledIds;
function move(enabledIds, id, delta): EnabledIds;
function getSortedIds(enabledIds, allIds): string[];
```

- `fullId(model) = `${model.provider}/${model.id}``。
- `normalizeEnabled` 的「全启用 → null」等价关系保证幂等。
- `moveModel` 只在 `enabledIds !== null` 时有效（全部启用时顺序由 provider 排序决定）。

**布局**（顺序；**【更正】所有 `Text` 的 paddingX = 0**，标题是 `theme.fg("accent", theme.bold(...))`
—— **先 bold 后 fg**，不是 `theme.bold(theme.fg(...))`）：

1. `DynamicBorder()`（默认 border 色）
2. `Spacer(1)`
3. `Text(theme.fg("accent", theme.bold("Model Configuration")), 0, 0)`
4. `Text(theme.fg("muted", `Session-only. ${keyDisplayText("app.models.save")} to save to settings.`), 0, 0)`
5. `Spacer(1)`
6. 搜索 `Input`（**没有** `onChange`；过滤发生在 `handleInput` 末尾显式 `refresh()`）
7. `Spacer(1)`
8. `listContainer`
9. `Spacer(1)`
10. 可选刷新状态：仅当 `config.refreshStatus` 为真时创建
    `Text(theme.fg("muted", `  ${config.refreshStatus}`), 0, 0)`；之后由 `setRefreshStatus` 用传入的 kind 上色
11. `footerText = Text(getFooterText(), 0, 0)`
12. `DynamicBorder()`
13. `updateList()`

**footer parts**（**逐字**，按顺序；`countText` 在**最后**）：

```
`${keyDisplayText("tui.select.confirm")} toggle`
`${keyDisplayText("app.models.enableAll")} all`
`${keyDisplayText("app.models.clearAll")} clear`
`${keyDisplayText("app.models.toggleProvider")} provider`
`${keyDisplayText("app.models.reorderUp")}/${keyDisplayText("app.models.reorderDown")} reorder`
`${keyDisplayText("app.models.save")} save`
计数：`all enabled` 或 `${enabledCount}/${total} enabled`（+ ` · ${unavailable} unavailable`）
```

- dirty 时：`theme.fg("dim", `  ${parts.join(" · ")} `)`（**末尾多一个空格**）+ `theme.fg("warning", "(unsaved)")`。
  所有 part **总是**全部出现（不存在「缺省项省略」）。
- `enabledCount = enabledIds?.filter(id => modelsById.has(id)).length ?? allIds.length`；
  `unavailableCount = enabledIds?.filter(id => !modelsById.has(id)).length ?? 0`。

**【逐字】** `Model Configuration`、`Session-only. `、` to save to settings.`、`  `、` · `、` (unsaved)`、
` (unsaved)`、` toggle`、` all`、` clear`、` provider`、` reorder`、` save`、`all enabled`、` enabled`、
` unavailable`、`  No matching models`、`  Model Name: `、`  Model unavailable`、` [unavailable]`、
`→ `、`✓ `、`  (`、`/`、`)`。

**列表行**（`updateList()`，**【精读更正】** 与 §2.53.2 一致）：

```ts
const item = filteredItems[i];
const isSelected = i === selectedIndex;
const prefix = isSelected ? theme.fg("accent", "→ ") : "  ";
const id = item.model?.id ?? item.fullId;          // 可用时只显示 id（不含 provider）
const styledId = item.model ? id : theme.strikethrough(id);
const modelText = isSelected ? theme.fg("accent", styledId) : styledId;
const name = item.model ? getDistinctModelName(item.model) : undefined;
const nameText = name ? theme.fg("muted", ` (${name})`) : "";
const providerBadge = theme.fg("muted", item.model ? ` [${item.model.provider}]` : " [unavailable]");
const status = item.model && item.enabled ? theme.fg("accent", "✓ ") : "  ";   // ← 未启用是两空格，不是 ○
`${prefix}${status}${modelText}${nameText}${providerBadge}`
```

- `getDistinctModelName(item.model)` —— **直接传 Model 对象**（有 `id` 和 `name` 字段，结构化匹配），
  不像 model-selector 那样包一层 `{ id, name }`。
- 空 `filteredItems`：`Text(theme.fg("muted", "  No matching models"), 0, 0)` 后**立即 return**
  （无滚动指示、无名称行）。**【更正】** 旧稿说空列表也加名称行，错。
- 名称行（列表非空时**总是**追加）：`Spacer(1)` +
  `Text(theme.fg("muted", `  ${selected.model ? `Model Name: ${selected.model.name}` : "Model unavailable"}`), 0, 0)`
  —— 不区分选中项是否可用；也没有旧稿所说的「选中项若可用」条件。
- 滚动指示：`if (startIndex > 0 || endIndex < len)` 追加
  `Text(theme.fg("muted", `  (${selectedIndex+1}/${len})`), 0, 0)`（**【更正】** muted，不是 dim）。
- **unavailable 的判定**：`allIds` 来自「模型目录」而非「配置里的 enabled 列表」，因此旧配置里已删除的模型
  仍显示但呈 strikethrough + ` [unavailable]`（`6949-unavailable-scoped-model`）。

**键位**（真实顺序见 §2.53.2；要点）：

```ts
up / down                     → 空列表 return；否则环绕（端点三元）；updateList()
reorderUp / reorderDown       → enabledIds === null 直接 return；当前项须已启用；新下标越界不动；
                                move(); isDirty = true; selectedIndex += delta; refresh(); notifyChange()
tui.select.confirm            → toggle(); isDirty = true; refresh(); notifyChange()
app.models.enableAll          → targetIds = 搜索非空 ? filteredItems.map(fullId) : undefined; enableAll(); dirty; refresh; notify
app.models.clearAll           → 同上，clearAll()
app.models.toggleProvider     → 当前项有 model 时：providerIds = allIds 中同 provider 者；
                                allEnabled = providerIds.every(isEnabled)；
                                全启用 → clearAll(providerIds)，否则 enableAll(providerIds)
app.models.save               → onPersist(副本或 null)；isDirty = false；footerText.setText(getFooterText())
Ctrl+C                        → 搜索非空则清空 + refresh，否则 onCancel()
Escape                        → onCancel()
其它                           → searchInput.handleInput(data); refresh()
```

- **【更正】** confirm **只**匹配 `tui.select.confirm`，源码里**没有** `|| "\n"` 的硬编码别名。
- **【更正】** 重排不做 `moveModel(..., direction, allIds)`；实际调用 `move(enabledIds, fullId, delta)`，
  且边界检查在调用方做；`selectedIndex += delta` 让选中跟随（不重新查找）。
- `notifyChange()` = `callbacks.onChange(enabledIds === null ? null : [...enabledIds])`（**每次变更立即回调**，
  宿主实时更新 settings 但不落盘）。
- 「enableAll/clearAll 在搜索非空时只作用于过滤结果」是 `scoped-models-selector.test.ts` 的
  "clear then enable selected" 行为。
- 测试（`scoped-models-selector.test.ts` 5 项）：同名模型显示 distinct name；enableAll 标记每一项；
  disable 只影响选中项；清空后启用选中项；恢复全启用 → `enabledModelIds === null`（`all enabled` 文案）。
  另加回归：`3217-scoped-model-order`（2 项：重排回写会话状态；`/model` scoped 页保留顺序）、
  `6949-unavailable-scoped-model`（4 项：不可用模型显示与移除；未匹配 settings 模式合并解析；
  仅有会话级不可用模型时也打开；不可用模型不清空部分 scope）、
  `7153-scoped-models-refresh`（2 项：先渲染缓存再后台更新；关闭时取消后台刷新）。

### 2.32 `oauth-selector.ts`（224 行）

> **⚠ 本节签名有误，以 §2.53.3 为准。**（`formatAuthSelectorProviderType` 是两参；`onSelect` 是
> `(providerId, authType)`；`method` 是 `ApiKeyAuth | OAuthAuth`；列表行用 `TruncatedText(..., 1, 0)`。）

```ts
// 【精读更正】本文件恰好 3 个 export；`AuthCheck` / `ApiKeyAuth` / `OAuthAuth` 从 kiana-ai 导入，不重导出。
export type AuthSelectorProvider = {
  id: string;
  name: string;
  authType: "oauth" | "api_key";
  method?: ApiKeyAuth | OAuthAuth;      // 不是 { name: string }
  status?: AuthCheck;
  subscription?: boolean;               // false → OAuth 标为 "account"（默认 "subscription"）
};
export function formatAuthSelectorProviderType(
  authType: "oauth" | "api_key",
  subscription?: boolean,
): string;                               // 不是接收整个 provider
export function formatAuthSelectorProviderStatus(provider: AuthSelectorProvider): string;
export class OAuthSelectorComponent extends Container implements Focusable {
  constructor(
    mode: "login" | "logout",
    providers: AuthSelectorProvider[],
    onSelect: (providerId: string, authType: "oauth" | "api_key") => void,   // 两个参数
    onCancel: () => void,
    initialSearchInput?: string,
  );
  handleInput(data: string): void;
  focused: boolean;                     // getter/setter 代理 searchInput.focused
}
```

`formatAuthSelectorProviderType(authType, subscription?)`：

```ts
if (authType === "api_key") return "API key";
return subscription === false ? "account" : "subscription";
```

`formatAuthSelectorProviderStatus(provider)`（返回**带 ANSI 的字符串**，永不为空）：

```ts
if (!provider.status) return theme.fg("muted", " • not configured");
if (provider.status.type !== provider.authType) {
  const label = `${formatAuthSelectorProviderType(provider.status.type, provider.subscription)} configured`;
  return theme.fg("muted", " • ") + theme.fg("warning", label);
}
if (!provider.status.source || provider.status.source === "OAuth" || provider.status.source === "stored credential") {
  return theme.fg("success", " ✓ configured");
}
const source = /^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$/.test(provider.status.source)
  ? `env: ${provider.status.source}` : provider.status.source;
return theme.fg("success", ` ✓ ${source}`);
```

- **【更正】** 类型不匹配时**不是**固定 `OAuth`/`API key`；而是 `formatAuthSelectorProviderType(status.type,
  subscription)` → `oauth` status 得到 `subscription` 或 `account`，`api_key` status 得到 `API key`，
  再拼 ` configured`（整体 warning 色）。
- `AuthCheck = { type: "oauth"|"api_key"; source?: string }`（kiana-ai 定义）。
- **【逐字】** ` • not configured`、` • `、` configured`、` ✓ configured`、` ✓ `、`env: `、
  `API key`、`account`、`subscription`、`OAuth`、`stored credential`。
- 环境变量识别正则：全大写、下划线、逗号+空格分隔的**多个**变量名（如 `OPENAI_API_KEY`、
  `AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY`）→ 加 `env: ` 前缀；其它 source（如 `models.json`、
  `command: foo`）直接原文。

布局（**paddingX：标题与列表行 1，其余无 Text**）：

1. `DynamicBorder()`
2. `Spacer(1)`
3. `TruncatedText(theme.fg("accent", theme.bold(title)), 1, 0)` —— **先 bold 后 fg**
   `title = mode === "login" ? "Select provider to configure:" : "Select provider to logout:"`
4. `Spacer(1)`
5. 搜索 `Input`：`initialSearchInput` 存在时预填；**`onSubmit` → `onSelectCallback(selectedProvider.id,
   selectedProvider.authType)`**（不是“选中当前项”这么模糊，且**没有** `onChange` 过滤）
6. `Spacer(1)`
7. `listContainer`
8. `Spacer(1)`
9. `DynamicBorder()`
10. 构造末尾 `filterProviders(initialSearchInput ?? "")`（**初始就过滤并渲染**）

**列表**：

- `showAuthTypeLabels = new Set(providers.map(p => p.authType)).size > 1`（两种 authType 并存时才显示标签）。
- `filterProviders(query)`：`query ? fuzzyFilter(allProviders, query, p => `${p.name} ${p.id} ${p.authType} ${p.method?.name ?? ""}`) : allProviders`；
  然后 `selectedIndex = max(0, min(selectedIndex, max(0, len-1)))`（clamp，**不重置到 0**），`updateList()`。
- `maxVisible = 8`；`startIndex = max(0, min(sel - floor(maxVisible/2), len - maxVisible))`；`endIndex = min(start+8, len)`。
- 行（**逐字**）：
  ```ts
  const statusIndicator = formatAuthSelectorProviderStatus(provider);
  const authTypeLabel = showAuthTypeLabels
    ? theme.fg("muted", ` [${formatAuthSelectorProviderType(provider.authType, provider.subscription)}]`)
    : "";
  let line = "";
  if (isSelected) line = theme.fg("accent", "→ ") + theme.fg("accent", provider.name) + authTypeLabel + statusIndicator;
  else            line = `  ${theme.fg("text", provider.name)}` + authTypeLabel + statusIndicator;
  listContainer.addChild(new TruncatedText(line, 1, 0));
  ```
  **注意**：未选中行**也**显示 authType/status（只是名字不是 accent）。
- 滚动：`TruncatedText(theme.fg("muted", `  (${selectedIndex+1}/${filtered.length})`), 1, 0)`（**【更正】** muted + paddingX=1）。
- 空消息（三档，**条件是 `filteredProviders.length === 0`**）：
  - `allProviders.length === 0 && mode === "login"` → `"No providers available"`
  - `allProviders.length === 0 && mode === "logout"` → `"No providers logged in. Use /login first."`
  - 否则 → `"No matching providers"`
  最终 `TruncatedText(theme.fg("muted", `  ${message}`), 1, 0)`。

**`handleInput`**（**【更正】** up/down **clamp**，不环绕；空列表直接 return）：

```ts
if (up)            { if (len === 0) return; selectedIndex = Math.max(0, selectedIndex - 1); updateList(); }
else if (down)     { if (len === 0) return; selectedIndex = Math.min(len - 1, selectedIndex + 1); updateList(); }
else if (confirm)  { const p = filtered[selectedIndex]; if (p) onSelectCallback(p.id, p.authType); }
else if (cancel)   { onCancelCallback(); }
else               { searchInput.handleInput(keyData); filterProviders(searchInput.getValue()); }
```

- 测试（`oauth-selector.test.ts` 6 项，**逐字标题**）：
  1. `projects provider-owned auth options without provider-specific filtering`
  2. `renders an option without compiled auth status as not configured`
  3. `shows OAuth auth distinctly in the API key selector`（断言含 `subscription configured`）
  4. `shows environment API key auth as configured`（`env: OPENAI_API_KEY`）
  5. `shows models.json API key auth as configured`（原文 ` ✓ models.json`）
  6. `shows models.json command auth as configured`

### 2.33 `settings-submenu.ts`（258 行）

```ts
// 【精读更正】4 个 export。SelectSubmenuOptions **不是**「一个 options 对象含 title/items」；
// SelectSubmenu 是 8 个位置参数 + 一个可选 submenuOptions。
export interface SelectSubmenuOptions {
  searchable?: boolean;                      // 类型到即过滤
  layout?: SelectListLayoutOptions;          // 覆盖列宽
}
export class SelectSubmenu extends Container {
  constructor(
    title: string,
    description: string,
    options: SelectItem[],
    currentValue: string,
    onSelect: (value: string) => void,
    onCancel: () => void,
    onSelectionChange?: (value: string) => void,
    submenuOptions?: SelectSubmenuOptions,
  );
}
export interface SteppedSubmenuStep {
  key: string;
  title: string | ((context: Record<string, string>) => string);
  description: string | ((context: Record<string, string>) => string);
  options: (context: Record<string, string>) => SelectItem[];
  preselect?: (context: Record<string, string>) => string | undefined;
  searchable?: boolean;
  layout?: SelectListLayoutOptions;
}
export class SteppedSubmenu extends Container {
  constructor(steps: SteppedSubmenuStep[],
              onComplete: (context: Record<string, string>) => void,
              onCancel: () => void,
              opts?: { startAtStep?: number; initialContext?: Record<string, string>; loop?: boolean });
}
```

（`SteppedSubmenuOptions` 是**模块私有** interface，`opts` 用的是它。）

`SUBMENU_SELECT_LIST_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 }`（模块私有）。

**`SelectSubmenu`** 布局（**paddingX 全部 0**）：

1. `Text(theme.bold(theme.fg("accent", title)), 0, 0)` —— **先 fg 后 bold**（与其它组件相反）
2. 若 `description` 非空：`Spacer(1)` + `Text(theme.fg("muted", description), 0, 0)`
3. 若 `searchable`：`Spacer(1)` + `Input`（**只设 `onSubmit = () => selectList.handleInput("\r")`**；
   **没有** `onChange`，过滤在 `handleInput` 里显式做）
4. `Spacer(1)`
5. `SelectList(options, Math.min(options.length, 10), getSelectListTheme(), layout)`
6. `Spacer(1)`
7. hint：`Text(theme.fg("dim", searchable ? "  Type to filter \u00b7 Enter to select \u00b7 Esc to go back"
                                                 : "  Enter to select \u00b7 Esc to go back"), 0, 0)`
8. （**注意：末尾没有 Spacer** —— 旧稿多写了一个 `Spacer(1)`。）

- `buildSelectList(options, preselect)`：
  ```ts
  const list = new SelectList(options, Math.min(options.length, 10), getSelectListTheme(), this.listLayout);
  const idx = options.findIndex(o => o.value === preselect);
  if (idx !== -1) list.setSelectedIndex(idx);
  list.onSelect = (item) => this.onSelectCb(item.value);
  list.onCancel = this.onCancelCb;
  if (this.onSelectionChangeCb) list.onSelectionChange = (item) => cb(item.value);
  ```
- `applyFilter(query)`：`fuzzyFilter(allOptions, query, i => `${i.label} ${i.description ?? ""}`)`；
  `buildSelectList(filtered, "")`（**preselect 传空串 → 不预选，选中回到 0**）；
  **直接赋值** `this.children[this.listChildIndex] = newList`（不调用 `removeChild`/`addChild`；
  `listChildIndex` 在构造时记为 `this.children.length`，插入前的位置）。
- `handleInput(data)`（**逐字**）：
  ```ts
  if (this.searchInput) {
    const isNav = kb.matches(data, "tui.select.up") || kb.matches(data, "tui.select.down")
               || kb.matches(data, "tui.select.confirm") || kb.matches(data, "tui.select.cancel");
    if (isNav) this.selectList.handleInput(data);
    else { this.searchInput.handleInput(data); this.applyFilter(this.searchInput.getValue()); }
  } else {
    this.selectList.handleInput(data);
  }
  ```
  **【更正】** 只有这 4 个 action 被视为导航（**不含** pageUp/pageDown），其它键全部喂给搜索框。

**【逐字】** `  Type to filter · Enter to select · Esc to go back`、`  Enter to select · Esc to go back`、
` · `（U+00B7，源码写作 `\u00b7`）。

**`SteppedSubmenu`**：

- 状态：`steps`、`onComplete`、`onCancel`、`opts`、`activeComponent`、`context = { ...(opts.initialContext ?? {}) }`。
  构造时 `activeComponent = buildStep(opts.startAtStep ?? 0)`（**没有 `stepIndex` 字段** —— 步号只在
  `buildStep` 的闭包里）。
- `buildStep(stepIndex)`：
  ```ts
  const step = this.steps[stepIndex];
  const total = this.steps.length;
  const stepLabel = total > 1 ? `Step ${stepIndex + 1}/${total} · ` : "";
  const title = typeof step.title === "function" ? step.title(this.context) : step.title;
  const desc  = typeof step.description === "function" ? step.description(this.context) : step.description;
  const items = step.options(this.context);
  const preselect = step.preselect?.(this.context) ?? "";
  return new SelectSubmenu(
    title,
    `${stepLabel}${desc}`,                 // ← 步号前缀加在 **description** 上，不是 title
    items,
    preselect,
    (value) => {
      this.context[step.key] = value;
      if (stepIndex < total - 1) this.activeComponent = this.buildStep(stepIndex + 1);
      else {
        this.onComplete({ ...this.context });
        if (this.opts.loop) { this.context = {}; this.activeComponent = this.buildStep(0); }
        else this.onCancel();
      }
    },
    () => {
      if (stepIndex > 0) { delete this.context[step.key]; this.activeComponent = this.buildStep(stepIndex - 1); }
      else this.onCancel();
    },
    undefined,                              // onSelectionChange 不传
    step.searchable || step.layout ? { searchable: step.searchable, layout: step.layout } : undefined,
  );
  ```
- **【更正】** 最后一步完成时：先 `onComplete({...context})`，再 **`loop` 为假时调用 `onCancel()`**
  （不是「什么都不做/自动关闭」；宿主若把 onCancel 当成「关闭面板」则行为等价）。
  `loop` 为真时 `context = {}`（**清空，不保留 initialContext**）并从 step 0 重建。
- Esc 上一级时 `delete context[step.key]`，回到上一 step 重建。
- `render(width)` → `this.activeComponent.render(width)`；
  `handleInput(data)` → `this.activeComponent.handleInput?.(data)`；
  `invalidate()` → `this.activeComponent.invalidate?.()`。**均不做额外包装。**
- **【逐字】** `Step `、`/${total} · `。

### 2.34 `session-selector-search.ts`（260 行）

**【精读更正 · 导出面】**（类型名与多处签名与源码不符；本节已整体重写为权威版）：

```ts
export type SortMode = "threaded" | "recent" | "relevance";        // 不是 SessionSortMode
export type NameFilter = "all" | "named";                          // 不是 SessionNameFilter
export interface ParsedSearchQuery {
  mode: "tokens" | "regex";                                        // 【原文缺失】
  tokens: { kind: "fuzzy" | "phrase"; value: string }[];           // 【原文缺失】token 带 kind
  regex: RegExp | null;                                            // 不是可选属性
  error?: string;
}
export interface MatchResult { matches: boolean; score: number }   // 【原文缺失】
export function hasSessionName(session: SessionInfo): boolean;    // 【原文缺失】
export function parseSearchQuery(query: string): ParsedSearchQuery;
export function matchSession(session: SessionInfo, parsed: ParsedSearchQuery): MatchResult;  // 不是 number|undefined
export function filterAndSortSessions(
  sessions: SessionInfo[], query: string, sortMode: SortMode, nameFilter: NameFilter = "all",
): SessionInfo[];                                                  // 第 4 参有默认值 "all"
```

`session-selector.ts` 里通过 `import { filterAndSortSessions, hasSessionName, type NameFilter,
type SortMode } from "./session-selector-search.ts"` 使用；`SessionSortMode`/`SessionNameFilter`
这两个名字在仓库里**不存在**。

**常量与缓存**（**【更正】** 缓存结构不是 `lowerCache`，而是按 `allMessagesText` 失效的 WeakMap）：

```ts
const TIER_NAME = 0;    // query 文本出现在 name
const TIER_TEXT = 1;    // 出现在 id / messages / cwd
const TIER_FUZZY = 2;   // 字符散落命中 name 或 cwd
const TIER_WEIGHT = 1_000_000;

interface LowercaseCache { source: string; lower: string; normalized?: string }
const messageTextCache = new WeakMap<SessionInfo, LowercaseCache>();
function getMessageText(session): LowercaseCache      // cached.source !== session.allMessagesText 时重建
function getNormalizedMessageText(session): string    // cached.normalized ??= lower.replace(/\s+/g, " ").trim()
function normalizeWhitespaceLower(text): string        // text.toLowerCase().replace(/\s+/g, " ").trim()
function tierScore(tier, score = 0) = tier * TIER_WEIGHT + score      // 越小越好
```

- **【重点】** 只有 **消息文本** 走缓存（可能几 MB）；`name`/`id`/`cwd` 每次现算 `toLowerCase()`。
- `name` 的短语匹配用 `normalizeWhitespaceLower(session.name ?? "")`；消息与 cwd 的短语匹配分别用
  `getNormalizedMessageText` 与 `normalizeWhitespaceLower(session.cwd)`。

**token 语法**（`parseSearchQuery`，**逐字行为**）：

1. `trimmed = query.trim()`；空 → `{ mode: "tokens", tokens: [], regex: null }`。
2. `trimmed.startsWith("re:")`（**区分大小写**）：
   `pattern = trimmed.slice(3).trim()`；
   - 空 pattern → `{ mode: "regex", tokens: [], regex: null, error: "Empty regex" }`；
   - `new RegExp(pattern, "i")` 成功 → `{ mode: "regex", tokens: [], regex }`；
   - 抛错 → `{ mode: "regex", tokens: [], regex: null, error: err.message ?? String(err) }`。
3. 否则按字符扫描分词（`"` 开关短语、引号外 `\s` 触发 flush、其余字符累积进 `buf`，flush 时
   `buf.trim()` 为空则丢弃）：
   - 遇到 `"`：若已在引号内 → `flush("phrase")` 并退出引号；否则先 `flush("fuzzy")` 再进入引号；
   - 不在引号内的 `\s` → `flush("fuzzy")`；
   - 扫描结束若仍在引号内 → `hadUnclosedQuote = true`；
   - 最后再 `flush(inQuote ? "phrase" : "fuzzy")`。
4. **未闭合引号 → 整体回退**为普通空白分词：
   `trimmed.split(/\s+/).map(t => t.trim()).filter(t => t.length > 0).map(t => ({ kind: "fuzzy", value: t }))`。
5. token 保留原大小写（匹配时对小写文本做 `includes` 前会 `value.toLowerCase()`；见下）。

**匹配函数**（**【更正】** literal 也看 **id**，且 name 命中还带 fuzzy 分数）：

```ts
matchLiteralToken(session, value):
  name = session.name?.toLowerCase() ?? "";
  if (name.includes(value)) return tierScore(TIER_NAME, fuzzyMatch(value, name).score);
  if (session.id.toLowerCase().includes(value)) return tierScore(TIER_TEXT);
  if (getMessageText(session).lower.includes(value)) return tierScore(TIER_TEXT);
  if (session.cwd.toLowerCase().includes(value)) return tierScore(TIER_TEXT);
  return undefined;

matchFuzzyToken(session, value):
  const literal = matchLiteralToken(session, value.toLowerCase());
  if (literal !== undefined) return literal;
  best = undefined;
  for (const field of [session.name ?? "", session.cwd]) {          // 【注意】只用 name 与 cwd
    const m = fuzzyMatch(value, field);
    if (m.matches && (best === undefined || m.score < best)) best = m.score;
  }
  return best === undefined ? undefined : tierScore(TIER_FUZZY, best);

matchPhraseToken(session, phrase):                                  // phrase 已 normalizeWhitespaceLower
  if (normalizeWhitespaceLower(session.name ?? "").includes(phrase)) return tierScore(TIER_NAME, fuzzyMatch(phrase, normalizedName).score);
  if (session.id.toLowerCase().includes(phrase)) return tierScore(TIER_TEXT);
  if (getNormalizedMessageText(session).includes(phrase)) return tierScore(TIER_TEXT);
  if (normalizeWhitespaceLower(session.cwd).includes(phrase)) return tierScore(TIER_TEXT);
  return undefined;

matchRegex(session, regex):
  if (session.name && regex.test(session.name)) return tierScore(TIER_NAME);
  if (regex.test(session.id) || regex.test(session.allMessagesText) || regex.test(session.cwd))
    return tierScore(TIER_TEXT);
  return undefined;
```

- **【注意】** `matchLiteralToken`/`matchPhraseToken` 的 name 分支在命中时仍调用 `fuzzyMatch(value, name).score`
  作为同 tier 内的次级分数（值相同则 tie）。
- 短语匹配是**跨换行归一化**的：`"foo   bar"` 能匹配消息里的 `foo\nbar`（测试 "filters by quoted phrase with
  whitespace normalization"）。
- fuzzy **不跨字段拼接**：散落字符只在 `name` 或 `cwd` **单个字段内**匹配（测试 "does not match scattered
  characters in session ids"）；消息与 id 只接受**连续子串**。

**`matchSession(session, parsed): MatchResult`**：

```ts
if (parsed.mode === "regex") {
  if (!parsed.regex) return { matches: false, score: 0 };
  const score = matchRegex(session, parsed.regex);
  return score === undefined ? { matches: false, score: 0 } : { matches: true, score };
}
let totalScore = 0;
for (const token of parsed.tokens) {
  let score;
  if (token.kind === "phrase") {
    const phrase = normalizeWhitespaceLower(token.value);
    if (!phrase) continue;
    score = matchPhraseToken(session, phrase);
  } else {
    score = matchFuzzyToken(session, token.value);
  }
  if (score === undefined) return { matches: false, score: 0 };     // AND 语义
  totalScore += score;
}
return { matches: true, score: totalScore };
```

- **【更正】** 没有「token 含空格就按短语匹配」的判定——kind 由**引号**决定。
- 不匹配时 `score` 恒为 **0**（不是 undefined）。

**`filterAndSortSessions(sessions, query, sortMode, nameFilter = "all")`**（**逐字**）：

```ts
const nameFiltered = nameFilter === "all" ? sessions : sessions.filter(s => matchesNameFilter(s, nameFilter));
const trimmed = query.trim();
if (!trimmed) return nameFiltered;                    // 空查询：保持输入顺序（任何 sortMode 都一样）
const parsed = parseSearchQuery(query);
if (parsed.error) return [];                          // 解析失败 → 空列表
if (sortMode === "recent") {                          // 只过滤，保持输入顺序
  const filtered = []; for (const s of nameFiltered) if (matchSession(s, parsed).matches) filtered.push(s);
  return filtered;
}
// relevance（以及 threaded 带查询时）
const scored = []; for (const s of nameFiltered) { const r = matchSession(s, parsed); if (r.matches) scored.push({s, score: r.score}); }
scored.sort((a, b) => a.score !== b.score ? a.score - b.score : b.session.modified.getTime() - a.session.modified.getTime());
return scored.map(r => r.session);
```

- `matchesNameFilter(session, filter)`：`filter === "all" || hasSessionName(session)`；
  `hasSessionName = Boolean(session.name?.trim())`（**空白名不算有名字**）。
- **【更正】** 空查询返回的是 `nameFiltered`（已应用 name filter），而非「原数组」。
- `threaded` + 有查询 → 走 relevance 分支（与 `relevance` 相同）；`threaded` + 无查询 → 第一条直接返回输入序，
  由 `session-selector` 自己构树。

- **测试**（`session-selector-search.test.ts` **14** 项）：空白归一化短语；`re:` 不区分大小写；recent 保序；
  relevance 平分时按 modified；位置无关（name 命中优先于 text，text 优先于 fuzzy）；
  不匹配分散的 id 字符；tier 排序（name → message → 散落 name 命中）；逐字段 token（token 不跨字段）；
  regex 命中 name 排前；非法 regex → `[]`；name filter 的 all/named/组合/空白名四项。
- **Rust 注意**：`fuzzyMatch` 打分必须与 tui-core 实现逐值一致（同一算法），否则排序会变。正则方言用
  `regex` crate 时需把 JS 的 `\p{...}`、lookahead 等不支持的语法当作「不匹配」（见 §8.3）。

### 2.35 `footer.ts`（302 行）

> **⚠ 本节多处有误，以 §2.60.3 为准。** 摘要：`sanitizeStatusText` **不导出**；
> `latestCacheHitRate` 是「每遇 assistant 消息整体覆盖（可能覆盖成 undefined）」，不是「cache>0 才更新」；
> 推理能力判据是 `state.model?.reasoning`，不是 `getSupportedThinkingLevels(...).length > 0`；
> 扩展状态行**不套** `theme.fg("dim")`；stats 字段是 `statsParts` 数组逐个 push 而非连续拼接。

```ts
export function sanitizeStatusText(text: string): string;   // ← 实际是模块私有，不导出
export function formatTokens(count: number): string;
export function formatCwdForFooter(cwd: string, home: string | undefined): string;
export class FooterComponent implements Component {
  constructor(session: AgentSession, footerDataProvider: ReadonlyFooterDataProvider);
  setSession(session: AgentSession): void;
  setAutoCompactEnabled(enabled: boolean): void;
  invalidate(): void;   // no-op
  dispose(): void;      // no-op
  render(width: number): string[];
}
```

`sanitizeStatusText(text)`：`text.replace(/[\r\n\t]/g, " ").replace(/ +/g, " ").trim()`。

`formatTokens(count)`：

| 区间 | 输出 |
|---|---|
| `< 1000` | `String(count)`（无小数） |
| `< 10_000` | `(count/1000).toFixed(1) + "k"`（如 `1.5k`、`9.9k`） |
| `< 1_000_000` | `Math.round(count/1000) + "k"`（如 `10k`、`999k`） |
| `< 10_000_000` | `(count/1e6).toFixed(1) + "M"` |
| 其它 | `Math.round(count/1e6) + "M"` |

- **负值/NaN 未定义**（上游保证非负）。
- `toFixed(1)` 的真实语义**不是**「半值向远离 0」：它把**二进制精确值**按「恰好一半时进位」
  转成十进制（实测 `1.25→"1.3"` 但 `1.15→"1.1"`、`2.675→"2.67"`）。Rust 的 `format!("{:.1}")` 是
  half-to-even，**不能直接用**；必须实现 `js_to_fixed`（锚点单测见 §6.3 与 §8.3 第 7 项）。
  分档实测：`999→"999"`、`1000→"1.0k"`、`9999→"10.0k"`、`10000→"10k"`、`999999→"1000k"`、
  `1000000→"1.0M"`、`9999999→"10.0M"`、`10000000→"10M"`、`12345678→"12M"`。

`formatCwdForFooter(cwd, home)`：

```ts
if (!home) return cwd;
const resolvedHome = resolve(home);
const resolvedCwd = resolve(cwd);
let rel = relative(resolvedHome, resolvedCwd);
const inside = rel === "" || (!rel.startsWith(".." + sep) && rel !== ".." && !isAbsolute(rel));
if (!inside) return cwd;
if (rel === "") return "~";
return `~${sep}${rel}`;
```

- Windows 上 `sep` 是 `\`；Linux 上 `/`。Rust 用 `std::path::Path::strip_prefix` + 相对化，
  注意「`..foo` 目录」不应被误判为父级（源码用 `".." + sep` 前缀判断，所以 `..foo` 安全）。
- 测试（`footer-width.test.ts`）：兄弟目录 `/a/bc` 相对 `/a/b` 不得显示成 `~c`（必须是绝对路径）。

**`FooterComponent` 状态缓存**：

```ts
private cachedStats?: { session; sessionId; leafId; entryCount; limitsModel; stats };
```

- `getSessionStats()`：键 = 上述 5 项；命中直接返回。
- 遍历 `sessionManager.getEntries()` 累计：
  - `entry.type === "usage"` → `addUsageToTotals(entry.usage)`（若有）；
  - `message` 且 `role === "assistant"` → `input += usage.input`、`output += usage.output`、
    `cacheRead += usage.cacheRead`、`cacheWrite += usage.cacheWrite`、`cost += usage.cost.total`；
    若 `cacheRead + cacheWrite > 0` 更新 `latestCacheHitRate = cacheRead / (input + cacheRead + cacheWrite) * 100`
    （**分母用 input，不含 output**）；
  - `toolResult` 带 `usage` → 同样累计；
  - `branch_summary` / `compaction` 带 `usage` → 同样累计。
- `contextUsage = session.getContextUsage()`（`{tokens: number|null, contextWindow, percent: number|null}`）。

**`render(width)`**：

```
contextWindow  = contextUsage?.contextWindow ?? state.model?.contextWindow ?? 0
contextPercent = contextUsage?.percent !== null && !== undefined ? percent.toFixed(1) : "?"
modelName      = state.model?.id || "no-model"
reasoning      = state.model && getSupportedThinkingLevels(state.model).length > 0
thinkingSuffix = reasoning ? (thinkingLevel === "off" ? " • thinking off" : ` • ${thinkingLevel}`) : ""
autoSuffix     = autoCompactEnabled ? " (auto)" : ""
```

1. **pwd 行**：`pwd = formatCwdForFooter(sessionManager.getCwd(), process.env.HOME || process.env.USERPROFILE)`；
   若有 git branch 追加 ` (${branch})`；若有 session name 追加 ` • ${name}`。
   `pwdLine = truncateToWidth(theme.fg("dim", pwd), width, theme.fg("dim", "..."))`。
2. **stats 左半**（连续拼接，**逐字**）：
   ```
   ↑${formatTokens(input)} ↓${formatTokens(output)} R${formatTokens(cacheRead)} W${formatTokens(cacheWrite)}
   ```
   若 `cacheRead + cacheWrite > 0` 且 `latestCacheHitRate !== undefined` → 追加 ` CH${rate.toFixed(1)}%`。
   若 `cost > 0` 或 `usingSubscription` → 追加 ` $${cost.toFixed(3)}` + (` (sub)` if usingSubscription)。
   `usingSubscription = session.model?.provider === "kimi-coding" || modelRuntime.isUsingSubscription(provider)`。
   上下文：`contextPercent === "?"` → `?/${formatTokens(contextWindow)}`，否则 `${percent}%/${formatTokens(window)}`，
   再加 `autoSuffix`。**颜色**：`percent > 90` → `theme.fg("error")`；`> 70` → `theme.fg("warning")`；
   否则 `theme.fg("dim")`。注意只有**上下文这一小段**着色，其余 stats 是 `dim`。
3. 若 `areExperimentalFeaturesEnabled()`：stats 前追加 `${theme.fg("dim","•")} ${theme.bold(theme.fg("warning","xp"))}`。
4. `statsLeft = truncateToWidth(statsLeftRaw, width, "...")`（**整体**截断）。
5. **右半**：`modelName + thinkingSuffix`；若 `session.getRoutedModel()` 返回的模型与当前不同 →
   追加 ` → ${routed.model.id}` 且若 routed 有 level 追加 ` • ${level}`。
   若 `availableProviderCount > 1` 且能放下 → 前缀 `(${provider}) `。
   放不下时退回不带 provider 的版本。
6. **布局**：
   ```
   padding = Math.max(2, width - visibleWidth(left) - visibleWidth(right))
   line2 = dimLeft + " ".repeat(padding) + dimRight
   ```
   若总宽超 → 截断左侧留右侧（minPadding = 2）；具体：
   - `maxLeftWidth = width - visibleWidth(right) - minPadding`；
   - 左侧超过 → `truncateToWidth(left, maxLeftWidth, "...")`；
   - 右侧仍放不下 → 截断右侧。
   **注释明确**：`statsLeft` 与 remainder **分别**套 `dim`，因为上下文百分比段会重置颜色（`\x1b[39m`），
   若整体再套 dim 会被重置掉。
7. **扩展状态行**：`footerDataProvider.getExtensionStatuses()` → `Map<string,string>`；
   entries 按 **key 的 `localeCompare` 排序**；每个 `sanitizeStatusText`；用 `" "` join；
   `truncateToWidth(theme.fg("dim", joined), width, theme.fg("dim", "..."))`。
   有状态时作为第 3 行；否则只有 2 行。
8. 返回行数组（2 或 3 行）。**每行必须 ≤ width。**

- **测试**（`footer-width.test.ts` 11 项）：兄弟路径；宽模型名；routed 模型显示；summary/toolResult 计入 cost；
  追加消息后缓存更新（entryCount 变化）；cache hit rate；Kimi Coding `(sub)`；subscription auth 显示 `(sub)`；
  通用 OAuth **不**显示 `(sub)`。
- **【逐字】** `↑`、`↓`、`R`、`W`、` CH`、`%`、` $`、` (sub)`、`?/`、`/`、` (auto)`、`no-model`、
  ` • thinking off`、` • `、` → `、`(`、`) `、`xp`、`•`、`...`、` (~` 等。

### 2.36 `login-dialog.ts`（322 行）

> **⚠ 本节多处有误，以 §2.60.2 为准。** 摘要：`input` 不在构造时加入容器；`cancel()` 是私有；
> `showManualInput` **不** clear；`showDetails` paddingX=1；`showInfo` 纯追加；device-code URL 校验用
> `new URL()` + protocol 白名单；`startWith("http://")` 与 `addChild(this.input)` 均不成立。


```ts
export class LoginDialogComponent extends Container implements Focusable {
  constructor(tui: TUI, providerId: string, onComplete: (success: boolean, message?: string) => void,
              providerNameOverride?: string, titleOverride?: string);
  get signal(): AbortSignal;
  cancel(): void;
  showAuth(url: string, instructions?: string): void;
  showDeviceCode(info: OAuthDeviceCodeInfo): void;
  setDeviceCodeStatus(message: string): void;
  showManualInput(prompt: string): Promise<string>;
  showPrompt(message: string, placeholder?: string): Promise<string>;
  showDetails(lines: string[]): void;
  showInfo(message: string, links?: AuthInfoLink[], showCloseHint?: boolean): void;
  showWaiting(message: string): void;
  showProgress(message: string): void;
  handleInput(data: string): void;
  focused: boolean;
}
```

构造：

```ts
super();
this.title = titleOverride ?? `Login to ${providerNameOverride || providerId}`;
this.abortController = new AbortController();
addChild(new DynamicBorder(theme));
addChild(new Text(theme.bold(theme.fg("accent", this.title)), 1, 0));
addChild(this.contentContainer);     // Container
addChild(this.input);                // Input
addChild(new DynamicBorder(theme));
```

- `input.onSubmit = (value) => { if (this.inputResolver) { const r = this.inputResolver; this.inputResolver = undefined;
  this.contentContainer.removeChild(this.input); this.contentContainer.addChild(new Text(`> ${value}`, 0, 0)); r(value); } }`
  —— 提交后把输入框替换成只读回显行。
- **【逐字】** `Login to `、`> `。

`cancel()`：

```ts
this.clearDeviceCodeAction();
this.abortController.abort();
if (this.inputResolver) { this.inputResolver = undefined; reject(new Error("Login cancelled")); }
this.onComplete(false, "Login cancelled");
```

- **【逐字】** `Login cancelled`（既作 Promise rejection message，也作 onComplete 的第二参数）。

`showAuth(url, instructions?)`：

```ts
clearDeviceCodeAction();
contentContainer.clear();
addChild(new Spacer(1));
addChild(new AuthUrlComponent(this.tui, url));
if (instructions) { addChild(new Spacer(1)); addChild(new Text(theme.fg("warning", instructions), 1, 0)); }
openBrowser(url);                  // 立即尝试打开浏览器（失败静默）
this.tui.requestRender();
```

`showDeviceCode(info)`：

```ts
clearDeviceCodeAction();
contentContainer.clear();
addChild(new Spacer(1));
addChild(new Text(theme.fg("accent", osc8(info.verificationUri, info.verificationUri)), 1, 0));
addChild(new Text(theme.fg("dim", osc8(clickHint, info.verificationUri)), 1, 0));
addChild(new Spacer(1));
addChild(new Text(theme.fg("warning", `Enter code: ${info.userCode}`), 1, 0));
if (!this.aborted && (this.openBrowserOnConfirm || this.copyCodeOnConfirm)) {
  this.deviceCodeAction = { info, hint: new Container() };
  const label = this.openBrowserOnConfirm && this.copyCodeOnConfirm ? "to open browser and copy code"
              : this.openBrowserOnConfirm ? "to open browser" : "to copy code";
  this.deviceCodeAction.hint.addChild(new TruncatedText(`(${keyHint("tui.select.confirm", label)})`, 1, 0));
  this.contentContainer.addChild(this.deviceCodeAction.hint);
}
```

- `osc8(url, text)` 是**手写**的 `\x1b]8;;${url}\x07${text}\x1b]8;;\x07`（BEL 结尾，非 ST）。
- `clickHint = process.platform === "darwin" ? "Cmd+click to open" : "Ctrl+click to open"`。
- 默认 `openBrowserOnConfirm = true`、`copyCodeOnConfirm = true`（可通过公开字段改）。
- **【逐字】** `Enter code: `、`(`, `)`, `to open browser and copy code`、`to open browser`、`to copy code`。

`setDeviceCodeStatus(message)`：**守卫** `!aborted && focused && deviceCodeAction 的 hint 仍是
contentContainer 的子节点`；通过则 `hint.clear(); hint.addChild(new TruncatedText(theme.fg("dim", message), 1, 0));`

`activateDeviceCode()`：

```ts
const status = this.copyCodeOnConfirm ? "Copying code..." : "Browser open requested";
this.setDeviceCodeStatus(status);
if (this.copyCodeOnConfirm) {
  try { await copyToClipboard(info.userCode); this.setDeviceCodeStatus("Code copied to clipboard"); }
  catch { this.setDeviceCodeStatus("Copy code manually"); }
}
if (this.openBrowserOnConfirm) {
  const url = info.verificationUri;
  if (url.startsWith("http://") || url.startsWith("https://")) {
    try { await openBrowser(url); } catch { /* 静默 */ }
  }
}
```

- **【逐字】** `Copying code...`、`Browser open requested`、`Code copied to clipboard`、`Copy code manually`。

`showManualInput(prompt)`：

```ts
clearDeviceCodeAction();
this.input.setValue("");
contentContainer.clear();
addChild(new Spacer(1));
addChild(new Text(theme.fg("dim", prompt), 1, 0));
addChild(this.input);
addChild(new Text(`(${keyHint("tui.select.cancel", "to cancel")})`, 1, 0));
return new Promise(resolve => { this.inputResolver = resolve; });
```

`showPrompt(message, placeholder?)`：**追加**（不 clear）：

```ts
addChild(new Spacer(1));
addChild(new Text(theme.fg("text", message), 1, 0));
if (placeholder) addChild(new Text(theme.fg("dim", `e.g., ${placeholder}`), 1, 0));
addChild(this.input);
addChild(new Text(`(${keyHint("tui.select.cancel", "to cancel,")} ${keyHint("tui.select.confirm", "to submit")})`, 1, 0));
this.input.setValue("");
return new Promise(resolve => { this.inputResolver = resolve; });
```

- **【逐字】** `e.g., `、`to cancel`、`to cancel,`（注意逗号在引号**内**）、`to submit`。

`showDetails(lines)`：`clearDeviceCodeAction(); contentContainer.clear();` 然后每行
`new Text(line, 0, 0)`（**原样输出，不着色**）。

`showInfo(message, links = [], showCloseHint = false)`：

```ts
addChild(new Spacer(1));
addChild(new Text(theme.fg("text", message), 1, 0));
for (const link of links) {
  const label = link.label ? `${link.label}: ${link.url}` : link.url;
  addChild(new Text(theme.fg("accent", osc8(link.url, label)), 1, 0));
}
if (showCloseHint) { addChild(new Spacer(1)); addChild(new Text(`(${keyHint("tui.select.cancel", "to close")})`, 1, 0)); }
```

`showWaiting(message)`：`Spacer(1)` + `Text(theme.fg("dim", message), 1, 0)` +
`Text(`(${keyHint("tui.select.cancel","to cancel")})`, 1, 0)`。

`showProgress(message)`：`Text(theme.fg("dim", message), 1, 0)`（**不 clear、不加 Spacer**，用于原地更新；
实际实现是替换最后一行还是追加由源码决定——源码为 `contentContainer.addChild(new Text(...))`，即追加）。

`handleInput(data)`：

```ts
if (matches cancel) { this.cancel(); return; }
if (this.authUrl && matches app.message.copy) { void this.authUrl.copy(); return; }
if (this.deviceCodeAction && !this.inputResolver && !this.aborted && this.focused && matches confirm) {
  const action = this.deviceCodeAction;
  this.deviceCodeAction = undefined;       // 只消费一次
  void this.activateDeviceCode(action);
  return;
}
this.input.handleInput(data);
```

- **测试**（`login-dialog-device-code.test.ts` 6 项 + `auth-url-copy.test.ts` 3 项）：
  确认后 browser+copy 各触发一次；切到 device code 会清掉之前的 auth URL 动作；切回 browser auth 清掉
  device code 动作；`showDetails` 清掉两者；取消无副作用；手动输入在失败后仍可用；
  登录对话框里按复制键复制 URL（不把按键字符喂给输入框）。

### 2.37 `user-message-selector.ts`（155 行）— 从用户消息 fork

> **⚠ 本节签名有误，以 §2.54.8 为准。** 摘要：`UserMessageList` **不导出**；条目类型是本地
> `UserMessageItem = { id, text, timestamp? }`（宿主已投影成纯文本，**不是** kiana-ai 的 `UserMessage`）；
> 构造是 `(messages, initialSelectedId?)`，回调是**公开字段** `onSelect?`/`onCancel?`（不是构造参数）；
> 每消息 **3 行**（正文 + `Message i of N` + 空行）；up/down **环绕**；有 **100 ms 空列表自动取消**。

```ts
// §2.37 旧稿（有误，保留对照）：
export class UserMessageList implements Component {
  constructor(messages: UserMessage[], onSelect: (messageId: string) => void, onCancel: () => void);
  invalidate(): void;
  render(width: number): string[];
  handleInput(data: string): void;
  getSelectedMessage(): UserMessage | undefined;
}
export class UserMessageSelectorComponent extends Container {
  constructor(messages: UserMessage[], onSelect: (messageId: string) => void, onCancel: () => void,
              initialSelectedId?: string);
  getMessageList(): UserMessageList;
  handleInput(data: string): void;
}
```

真实类型（**权威**）：`interface UserMessageItem { id: string; text: string; timestamp?: string }`（模块内，
**不导出**）；`maxVisible = 10` 硬编码。

**`UserMessageList.render(width)`**：

- `messages.length === 0` → `[theme.fg("muted", "  No user messages found")]`。
- 滚动起点：`start = Math.max(0, Math.min(selectedIndex - Math.floor(maxVisible / 2), messages.length - maxVisible))`。
- 每条可见消息产生 **3 行**（**【更正】** 原写「两行」但有尾随空行）：
  ```ts
  const selected = i === selectedIndex;
  const cursor = selected ? theme.fg("accent", "› ") : "  ";
  const normalized = message.text.replace(/\n/g, " ").trim();     // 只替换 \n，不替换 \t
  const display = truncateToWidth(normalized, width - 2);          // 无第三参 → 默认 "..."
  const body = selected ? theme.bold(display) : display;
  lines.push(cursor + body);
  lines.push(theme.fg("muted", `  Message ${i + 1} of ${this.messages.length}`));
  lines.push("");                                                  // 空行分隔
  ```
  - `message.text` 是**宿主已投影好的纯文本**（组件不做 content 块拼接）。
  - **message 序号 `i+1` 是全局索引**（不是可见窗口内序号）。
- 需要滚动时最后一行 `theme.fg("muted", `  (${selectedIndex+1}/${N})`)`（**【更正】** muted，不是 dim；
  且**不带** `truncateToWidth` 包装）。
- **【逐字】** `  No user messages found`、`› `、`  Message `、` of `、`  (`、`/`、`)`。

**键位**：

```ts
up:      selectedIndex = selectedIndex === 0 ? N - 1 : selectedIndex - 1       // 环绕
down:    selectedIndex = selectedIndex === N - 1 ? 0 : selectedIndex + 1       // 环绕
confirm: selected && onSelect?.(selected.id)
cancel:  onCancel?.()
// 其它键一律忽略
```

- `initialSelectedId` 命中时以其为初值；否则 `Math.max(0, messages.length - 1)`（**最后一条 = 最新**）。

**`UserMessageSelectorComponent` 布局**：

1. `Spacer(1)`
2. `Text(theme.bold("Fork from Message"), 1, 0)`
3. `Text(theme.fg("muted", "Select a user message to copy the active path up to that point into a new session"), 1, 0)`
4. `Spacer(1)`
5. `DynamicBorder(theme)`
6. `Spacer(1)`
7. `UserMessageList`
8. `Spacer(1)`
9. `DynamicBorder(theme)`

- **空列表自动取消**：构造末尾 `if (messages.length === 0) setTimeout(() => onCancel(), 100);`
  （**【更正】** 条件在**外层**判断，定时器回调里不再判；定时器**未 unref**）。
  注意宿主 `interactive-mode` 在构造前已用 `userMessages.length === 0` 早退，所以实际生产路径不会走到这里。
- `getMessageList()` 返回内部 list（供宿主设焦点）。
- `UserMessageSelectorComponent` **不重写** `handleInput`（Container 默认分发，实测由宿主把焦点设在 list 上）。
- **【逐字】** `Fork from Message`、`Select a user message to copy the active path up to that point into a new session`。

### 2.38 `assistant-message.ts`（216 行）

```ts
export class AssistantMessageComponent extends Container {
  constructor(message?: AssistantMessage, hideThinkingBlock?: boolean,
              markdownTheme?: MarkdownTheme, hiddenThinkingLabel?: string,
              outputPad?: number, transformers?: readonly MarkdownTransformer[],
              messageId?: string, timestamp?: string);
  setHideThinkingBlock(hide: boolean): void;          // 并 clear thinkingVisibilityOverrides
  setHiddenThinkingLabel(label: string): void;
  setOutputPad(outputPad: number): void;
  updateContent(message: AssistantMessage, isStreaming?: boolean): void;
  override invalidate(): void;
  override render(width: number): string[];
}
```

模块常量（**与 user-message 相同的三行**，此处为私有副本）：

```ts
const OSC133_ZONE_START = "\x1b]133;A\x07";
const OSC133_ZONE_END   = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL = "\x1b]133;C\x07";
```

- 构造：`super()`；`contentContainer = new Container()`；`addChild(contentContainer)`。
- 默认值：`hideThinkingBlock = false`、`hiddenThinkingLabel = "Thinking..."`、`outputPad = 1`、
  `transformers = []`、`isStreaming = false`、`lastMessage = message`（**构造时不赋值**，只在
  `updateContent` 里写；无 message 时是 `undefined`）、`hasToolCalls = false`、
  `thinkingVisibilityOverrides = new Map()`。
- 若 `message` 存在 → `updateContent(message)`。
- **`thinkingRunIndex` 是 `updateContent` 的局部变量**，每次调用从 0 开始（**【更正】** 旧稿说「跨
  updateContent 持续、不重置」——错）。因此 override 的 key 是「本次渲染中第几个 thinking run」，
  流式追加文本时 run 编号可能变化（这是已知的 JS 行为，Rust 照抄即可）。

**`updateContent(message, isStreaming = this.isStreaming)`** 完整流程（逐字）：

1. `this.lastMessage = message; this.isStreaming = isStreaming;`
2. `contentContainer.clear()`。
3. `const hasVisibleContent = message.content.some(c => (c.type === "text" && c.text.trim()) || (c.type === "thinking" && c.thinking.trim()))`；
   若为真 → `addChild(new Spacer(1))`。
4. 遍历 `message.content`：
   - `type === "text"` 且 `text.trim()` 非空 → `Markdown(text.trim(), outputPad, 0, markdownTheme, undefined,
     { transform: createMarkdownTransform("assistant", isStreaming, transformers, messageId, timestamp) })`。
     **注意**：`paddingY = 0`（间距由 Spacer 负责）；**未**传自定义 style（用 Markdown 默认主题样式）。
   - `type === "thinking"` → **合并连续 thinking 块**：从当前位置收集所有连续 `type === "thinking"` 的块，
     过滤 `thinking.trim()` 非空，`join("\n\n")`；`runIndex = thinkingRunIndex++`（**按调用顺序**递增，
     跨 updateContent 持续，不重置）。
     然后：
     ```ts
     const hidden = this.thinkingVisibilityOverrides.get(runIndex) ?? this.hideThinkingBlock;
     ```
     - hidden：
       `new Text(theme.italic(theme.fg("thinkingText", this.hiddenThinkingLabel)), outputPad, 0)`
     - 可见：`new Markdown(thinkingBlocks.join("\n\n"), outputPad, 0, markdownTheme,
       { color: t => theme.fg("thinkingText", t), italic: true },
       { transform: createMarkdownTransform("assistant-thinking", isStreaming, transformers, messageId, timestamp) })`
     - 两者都用 `MouseRegion` 包裹（**【逐字】** 用**已解析的** `hidden`，不是重新查 map）：
       ```ts
       new MouseRegion(thinkingComponent, (event) => {
         if (event.type !== "click" || event.button !== "left") return undefined;
         this.thinkingVisibilityOverrides.set(runIndex, !hidden);
         if (this.lastMessage) this.updateContent(this.lastMessage);
         return { handled: true };
       })
       ```
       **注意**：`hidden` 是闭包捕获的值；若在两次渲染之间 `hideThinkingBlock` 变了但 override 未变，
       点击仍按旧 `hidden` 取反（语义正确）。
     - 若该 run 之后还有**可见的** text/thinking 内容 → 追加 `Spacer(1)`（**【更正】** 判定**不含**
       toolCall，`toolCall` 不算「可见内容」）：
       ```ts
       const hasVisibleContentAfter = message.content.slice(i + 1)
         .some(c => (c.type === "text" && c.text.trim()) || (c.type === "thinking" && c.thinking.trim()));
       ```
       `thinkingBlocks.length === 0` 时 `continue`（**不**加 Spacer、不占 runIndex）。
   - `type === "toolCall"` → 不在此渲染（由 `ToolExecutionComponent` 负责），但会置 `hasToolCalls = true`。
5. `const hasToolCalls = message.content.some(c => c.type === "toolCall");`
6. 收尾文本：
   - `message.stopReason === "length"` → `Spacer(1)` + `Text(theme.fg("error", "Response was truncated before completion."), outputPad, 0)`。
   - 否则若 `!hasToolCalls`：
     - `stopReason === "aborted"`：
       ```ts
       const abortMessage = message.errorMessage && message.errorMessage !== "Request was aborted"
         ? message.errorMessage : "Operation aborted";
       addChild(new Text(theme.fg("error", abortMessage), outputPad, 0));
       ```
     - `stopReason === "error"`：
       ```ts
       const errorMsg = message.errorMessage || "Unknown error";
       addChild(new Text(theme.fg("error", `Error: ${errorMsg}`), outputPad, 0));
       ```
   - **顺序要点**：`aborted`/`error` 只在**没有 toolCall** 时显示（有 toolCall 时工具的错误自己显示）。

**`render(width)`**（**【更正】** 用的是实例字段 `this.hasToolCalls`，由 `updateContent` 写入，
**不是**每次 render 重新扫 `lastMessage`）：

```ts
const lines = super.render(width);
if (this.hasToolCalls || lines.length === 0) return lines;
lines[0] = OSC133_ZONE_START + lines[0];
lines[lines.length - 1] = OSC133_ZONE_END + OSC133_ZONE_FINAL + lines[lines.length - 1];
return lines;
```

**`invalidate()`**：`super.invalidate(); if (this.lastMessage) this.updateContent(this.lastMessage);`
（重建以重放主题色 / transformer 结果。`assistant-message.test.ts` 断言 width 从 78 变 58 后 transformer
收到新 `availableWidth`。）

**setter 语义**：

- `setHideThinkingBlock(hide)`：`hideThinkingBlock = hide; thinkingVisibilityOverrides.clear(); updateContent(lastMessage)`。
- `setHiddenThinkingLabel(label)`：存值 + `updateContent`。
- `setOutputPad(pad)`：存值 + `updateContent`。

**测试（`assistant-message.test.ts` 12 项，逐字标题）**：
1. `adds OSC 133 zone markers to assistant messages without tool calls`
2. `does not add OSC 133 zone markers when assistant message contains tool calls`
3. `renders length stops with neutral truncation wording`
4. `coalesces adjacent thinking blocks into one hidden thinking label`
5. `collapses individual thinking runs when clicked`
6. `uses configured output padding for text and thinking`
7. `chains Markdown transformers in registration order`
8. `identifies partial assistant Markdown as streaming`
9. `reapplies Markdown transformers when available width changes`
10. `continues the Markdown transformer chain when a transformer throws`
11. `transforms text and thinking Markdown without mutating the original message`
12. `uses configured output padding for user messages`

- **【逐字】** `Thinking...`、`Response was truncated before completion.`、`Request was aborted`、
  `Operation aborted`、`Unknown error`、`Error: `。

### 2.39 `bash-execution.ts`（223 行）

> **⚠ 本节部分有误（`appendOutput` 是否重建、构造期 loader 位置、`isComplete` 字段是否存在），
> 以 §2.55.2 为准。**

```ts
export class BashExecutionComponent extends Container {
  constructor(command: string, ui: TUI, excludeFromContext?: boolean, outputPad?: number);
  appendOutput(chunk: string): void;
  setComplete(exitCode: number | undefined, cancelled: boolean,
              truncationResult?: TruncationResult, fullOutputPath?: string): void;
  setExpanded(expanded: boolean): void;
  setOutputPad(outputPad: number): void;
  getOutput(): string;
  getCommand(): string;
  override invalidate(): void;
}
```

常量 `PREVIEW_LINES = 20`；`colorKey = excludeFromContext ? "dim" : "bashMode"`。

**构造**（**【更正】** 无 `isComplete` 参数/字段；loader **总是**创建，但只在 running 时被加入容器）：

1. 字段初始化：`status = "running"`、`exitCode = undefined`、`expanded = false`、`outputLines = []`。
2. `addChild(new Spacer(1))`
3. `addChild(new DynamicBorder(borderColor))`，`borderColor = t => theme.fg(colorKey, t)`
4. `contentContainer = new Container()`，`addChild(contentContainer)`
5. **无条件**创建 `loader = new Loader(ui, spinner => theme.fg(colorKey, spinner),
   text => theme.fg("muted", text), `Running... (${keyText("tui.select.cancel")} to cancel)`)`
   （**不加入**子节点；由 `updateDisplay()` 在 `status === "running"` 时加入）
6. `addChild(new DynamicBorder(borderColor))`
7. `updateDisplay()`

**【逐字】** `` `Running... (${keyText} to cancel)` ``（注意省略号是三个 ASCII 句点）、`$ `（见下）。

**`appendOutput(chunk)`**：

```ts
const normalized = stripAnsi(chunk).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
const lines = normalized.split("\n");
if (this.outputLines.length === 0) this.outputLines.push(lines[0] ?? "");
else this.outputLines[this.outputLines.length - 1] += lines[0] ?? "";
for (let i = 1; i < lines.length; i++) this.outputLines.push(lines[i]);
```

- 语义：**首段接到当前最后一行**（因为流式 chunk 可能在同一行内断开），其余成新行。
- `\r\n` → `\n` 先于 `\r` → `\n`（顺序不可交换，否则 `\r\n` 变两行）。
- 所有 ANSI 被剥掉（`stripAnsi`）。
- **【更正】`appendOutput` 末尾会调用 `this.updateDisplay()`**（旧稿说「不重建」是错的）。
  即每个 chunk 都重建 contentContainer。

**`setComplete(exitCode, cancelled, truncationResult?, fullOutputPath?)`**：

```ts
this.exitCode = exitCode;
this.status = cancelled
  ? "cancelled"
  : exitCode !== 0 && exitCode !== undefined && exitCode !== null
    ? "error"
    : "complete";
this.truncationResult = truncationResult;
this.fullOutputPath = fullOutputPath;
this.loader.stop();
this.updateDisplay();
```

状态**在 `setComplete` 里一次性算好并存入 `status` 字段**（`updateDisplay` 只读它，不重新推导）。

**`updateDisplay()`**：

```ts
const full = this.outputLines.join("\n");
const contextTruncation = truncateTail(full, { maxLines: DEFAULT_MAX_LINES, maxBytes: DEFAULT_MAX_BYTES });
const availableLines = contextTruncation.content ? contextTruncation.content.split("\n") : [];   // 【更正】空内容特判
const previewLogicalLines = availableLines.slice(-PREVIEW_LINES);
const hiddenLineCount = availableLines.length - previewLogicalLines.length;
contentContainer.clear();
const header = new Text(theme.fg(colorKey, theme.bold(`$ ${this.command}`)), outputPad, 0);
contentContainer.addChild(header);
if (this.expanded) {
  const displayText = availableLines.map(l => theme.fg("muted", l)).join("\n");
  contentContainer.addChild(new Text(`\n${displayText}`, outputPad, 0));
}
```

更精确的分支：

- **header**：`Text(theme.fg(colorKey, theme.bold(`$ ${command}`)), outputPad, 0)`。
- **expanded**：`Text("\n" + availableLines.map(l => theme.fg("muted", l)).join("\n"), outputPad, 0)`
  —— **前置一个 `\n`**（与 header 之间空一行）。
- **collapsed**：自定义匿名 `Component`，缓存 `{width → lines}`：
  ```ts
  render(width) {
    if (cachedWidth !== width) {
      const t = truncateToVisualLines(styledInput, PREVIEW_LINES, width, outputPad);
      cached = t.visualLines; cachedWidth = width;
    }
    return cached;
  }
  ```
  即**物理行**（visual lines）截断，不是逻辑行；宽度变化必须重算（`bash-execution-width.test.ts`）。
- **状态行**（`statusParts: string[]`）：
  - `hiddenLineCount > 0`：
    - expanded → `theme.fg("muted", "(") + keyHint("app.tools.expand", "to collapse") + theme.fg("muted", ")")`
    - collapsed → `theme.fg("muted", `... ${hiddenLineCount} more lines (`) + keyHint("app.tools.expand", "to expand") +
      theme.fg("muted", ")")`
  - `cancelled` → `theme.fg("warning", "(cancelled)")`
  - `status === "error"` → `theme.fg("error", `(exit ${exitCode})`)`
  - `(truncationResult?.truncated || contextTruncation.truncated) && fullOutputPath` →
    `theme.fg("warning", `Output truncated. Full output: ${fullOutputPath}`)`
  - 有 parts → `contentContainer.addChild(new Text("\n" + parts.join("\n"), outputPad, 0))`
    （前置 `\n` 与输出分隔）。
- **运行中**：`loader` 已加入；不显示状态行（loader 自己显示 `Running...`）。
- **`invalidate()`**：`super.invalidate(); updateDisplay();`（重建以重放颜色）

**【逐字】** `$ `、`\n`、`(`、`)`、`... `、` more lines (`、`to expand`、`to collapse`、`(cancelled)`、
`(exit `、`)`、`Output truncated. Full output: `、`Running... (`、` to cancel)`。

- `getOutput()` = `outputLines.join("\n")`（原始未截断、已剥 ANSI）。
- `getCommand()` = 命令原文。
- 测试：`bash-execution-width.test.ts` 首次渲染后改变宽度，折叠预览必须按新宽度重算。
  `output-pad.test.ts` 断言 pad 0 无前导空格、pad 1 每行多一个空格。

### 2.40 `settings-selector.ts`（1009 行）— `/settings` 主面板

> **⚠ 本节多处有误，以 §2.52.3 / §2.52.4 为准。** 摘要：`SelectSubmenu` 是 8 个位置参数；
> `modelItemLabel` 带已着色的 `[provider]`；`MODEL_PICKER_LAYOUT.maxPrimaryColumnWidth = 46`；
> `ThemeSubmenu` 是自绘 Container；`SteppedSubmenu` 完成最后一步后还会调 `onCancel()`。


```ts
export interface SettingsConfig { ... }        // 见下（逐字段）
export interface SettingsCallbacks { ... }     // 见下
export class SettingsSelectorComponent extends Container {
  constructor(config: SettingsConfig, callbacks: SettingsCallbacks);
  getSettingsList(): SettingsList;
}
```

**常量**：

```ts
const MODEL_PICKER_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 46 };
const CLEAR_OVERRIDE_VALUE = "__clear__";
const DEFAULT_PROJECT_TRUST_LABELS = { ask: "Ask", always: "Always trust", never: "Never trust" };
const DEFAULT_PROJECT_TRUST_BY_LABEL = new Map(Object.entries(DEFAULT_PROJECT_TRUST_LABELS).map(([k, v]) => [v, k]));
const THINKING_DESCRIPTIONS: Record<ThinkingLevel, string> = {
  off:     "No reasoning",
  minimal: "Very brief reasoning (~1k tokens)",
  low:     "Light reasoning (~2k tokens)",
  medium:  "Moderate reasoning (~8k tokens)",
  high:    "Deep reasoning (~16k tokens)",
  xhigh:   "Extra-high reasoning (~32k tokens)",
  max:     "Maximum reasoning",
};
```

**辅助函数**：

- `modelSettingKey(m) = `${m.provider}/${m.id}``
- `modelDisplayLabel(m) = `${m.id} [${m.provider}]``
- `modelThinkingOverridesSummary(levels)`：空 → `"none"`，否则 `` `${Object.keys(levels).length} configured` ``。
- `modelItemLabel(m) = `${m.id} ${theme.fg("muted", `[${m.provider}]`)}``（**【更正】** 带已着色的
  `[provider]`，不是纯文本 `id [provider]`；`modelDisplayLabel` 才是纯文本）。
- `themeItems(themes, currentTheme)`：
  ```ts
  themes.map(name => ({
    value: name,
    label: `${name === currentTheme ? "✓ " : "  "}${name}`,
    ...(name === SYSTEM_THEME_NAME ? { description: "Theme created from your terminal's colors" } : {}),
  }))
  ```
- `AUTOMATIC_THEME_VALUE = "/"`。
- `singleModeThemeItems`：`system` 在最前，然后 `{ value: "/", label: "  automatic", description: "Use separate themes for light and dark terminal appearance" }`，再其余主题（**不含 system 重复**）。
- `preferredTheme(terminalTheme)` / `defaultAutomaticThemes()`：依据 `terminalTheme.appearance` 选 light/dark 默认主题名。
- `formatHttpIdleTimeoutMs`/`HTTP_IDLE_TIMEOUT_CHOICES`（来自 `core/http-dispatcher.ts`）：
  `[{label:"30 sec",timeoutMs:30_000},{label:"1 min",60_000},{label:"2 min",120_000},{label:"5 min",300_000},{label:"disabled",0}]`。

**`SettingsConfig` 字段（全部，逐字）**：

```
autoCompact, defaultModel, currentModel?, availableDefaultModels,
showImages, imageWidthCells, autoResizeImages, blockImages,
enableSkillCommands, steeringMode, followUpMode, transport, sessionShareDestination,
httpIdleTimeoutMs, cacheWarmingMode, thinkingLevel, availableThinkingLevels, modelThinkingLevels,
currentTheme, terminalTheme, availableThemes, hideThinkingBlock, mermaidRenderingMode,
showCacheMissNotices, collapseChangelog, enableInstallTelemetry, doubleEscapeAction,
treeFilterMode, showHardwareCursor, editorPaddingX, outputPad, autocompleteMaxVisible,
quietStartup, defaultProjectTrust, clearOnShrink, showTerminalProgress, tuiMode,
fullscreenExitOutput, fullscreenScrollbar, fullscreenCopyOnSelect, fullscreenWheelScrollLines,
warnings
```

**`SettingsCallbacks` 字段（全部，逐字）**：

```
onAutoCompactChange, onShowImagesChange, onImageWidthCellsChange, onAutoResizeImagesChange,
onBlockImagesChange, onEnableSkillCommandsChange, onSteeringModeChange, onFollowUpModeChange,
onTransportChange, onSessionShareDestinationChange, onHttpIdleTimeoutMsChange, onCacheWarmingModeChange,
onHideThinkingBlockChange, onMermaidRenderingModeChange, onShowCacheMissNoticesChange,
onCollapseChangelogChange, onQuietStartupChange, onEnableInstallTelemetryChange,
onDefaultProjectTrustChange, onDoubleEscapeActionChange, onTreeFilterModeChange,
onShowHardwareCursorChange, onEditorPaddingXChange, onOutputPadChange, onAutocompleteMaxVisibleChange,
onClearOnShrinkChange, onShowTerminalProgressChange, onTuiModeChange, onFullscreenExitOutputChange,
onFullscreenScrollbarChange, onFullscreenCopyOnSelectChange, onFullscreenWheelScrollLinesChange,
onThemeChange, onThemePreview?, onModelThinkingLevelChange, onModelThinkingLevelRemove,
onWarningsChange, onCancel
```

**主列表项（最终顺序，逐字）**：

| # | id | label | description | values |
|---|---|---|---|---|
| 1 | `autocompact` | `Auto-compact` | `Automatically compact context when it gets too large` | `["true","false"]` |
| 2* | `show-images` | `Show images` | `Render images inline in terminal` | `["true","false"]` |
| 3* | `image-width-cells` | `Image width` | `Preferred inline image width in terminal cells` | `["60","80","120"]` |
| 4 | `auto-resize-images` | `Auto-resize images` | `Resize large images to 2000x2000 max for better model compatibility` | `["true","false"]` |
| 5 | `block-images` | `Block images` | `Prevent images from being sent to LLM providers` | `["true","false"]` |
| 6 | `skill-commands` | `Skill commands` | `Register skills as /skill:name commands` | `["true","false"]` |
| 7 | `show-hardware-cursor` | `Show hardware cursor` | `Use the terminal cursor instead of Kiana's drawn cursor` | `["true","false"]` |
| 8 | `editor-padding` | `Editor padding` | `Horizontal padding for input editor (0-3)` | `["0","1","2","3"]` |
| 9 | `output-padding` | `Output padding` | `Horizontal padding for messages, tool output, and command output` | `["0","1"]` |
| 10 | `autocomplete-max-visible` | `Autocomplete max items` | `Max visible items in autocomplete dropdown (3-20)` | `["3","5","7","10","15","20"]` |
| 11 | `clear-on-shrink` | `Clear on shrink` | `Clear empty rows when content shrinks (may cause flicker)` | `["true","false"]` |
| 12 | `terminal-progress` | `Terminal progress` | `Show OSC 9;4 progress indicators in the terminal tab bar` | `["true","false"]` |
| 13 | `steering-mode` | `Steering mode` | `Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.` | `["one-at-a-time","all"]` |
| 14 | `follow-up-mode` | `Follow-up mode` | `` `${followUpKey} queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.` `` | `["one-at-a-time","all"]` |
| 15 | `transport` | `Transport` | `Preferred transport for providers that support multiple transports` | `["sse","websocket","websocket-cached","auto"]` |
| 16 | `share-destination` | `Session share destination` | `Where to upload sessions with /share (auto prefers Radius when authenticated, otherwise GitHub Gist)` | `["auto","gist","radius"]` |
| 17 | `http-idle-timeout` | `HTTP idle timeout` | `Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five minutes.` | `HTTP_IDLE_TIMEOUT_CHOICES` 的 labels |
| 18 | `cache-warming-mode` | `Cache warming` | `off; streaming while the agent runs; idle also between runs while continuation stays profitable` | `CACHE_WARMING_MODES` |
| 19 | `hide-thinking` | `Hide thinking` | `Hide thinking blocks in assistant responses` | `["true","false"]` |
| 20 | `mermaid-rendering` | `Mermaid diagrams` | `Render Mermaid code blocks as Unicode diagrams` | `["off","final","streaming"]` |
| 21 | `cache-miss-notices` | `Cache miss notices` | `Show transcript notices for cache costs and provider recovery diagnostics` | `["true","false"]` |
| 22 | `collapse-changelog` | `Collapse changelog` | `Show condensed changelog after updates` | `["true","false"]` |
| 23 | `quiet-startup` | `Quiet startup` | `Disable verbose printing at startup (header: keep only the startup header)` | `["true","header","false"]` |
| 24 | `install-telemetry` | `Install telemetry` | `Send an anonymous version/update ping after changelog-detected updates` | `["true","false"]` |
| 25 | `default-project-trust` | `Default project trust` | `Fallback behavior when no extension or saved trust decision decides project trust` | `["Ask","Always trust","Never trust"]` |
| 26 | `double-escape-action` | `Double-escape action` | `Action when pressing Escape twice with empty editor` | `["tree","fork","none"]` |
| 27 | `tree-filter-mode` | `Tree filter mode` | `Default filter when opening /tree` | `["default","no-tools","user-only","labeled-only","all"]` |
| 28 | `warnings` | `Warnings` | `Enable or disable individual warnings` | currentValue `"configure"`，**submenu** |
| 29 | `model-thinking` | `Default thinking level per model` | `` `Override the default thinking level for specific models. ${cycleThinkingKey} cycles in-session.` `` | currentValue = `modelThinkingOverridesSummary(...)`，**submenu** |
| 30 | `tui-mode` | `TUI mode` | `Interface layout; regular mode uses the terminal's normal scrollback` | `["regular","fullscreen"]` |
| 31 | `fullscreen-exit-output` | `Fullscreen exit output` | `Print the transcript or only a session resume hint when exiting fullscreen mode` | `["transcript","resume-hint"]` |
| 32 | `fullscreen-scrollbar` | `Fullscreen scrollbar` | `Scrollbar behavior in fullscreen mode; has no effect in regular mode` | `["auto","always","hidden"]` |
| 33 | `fullscreen-copy-on-select` | `Fullscreen copy on select` | `Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X` | `["true","false"]` |
| 34 | `fullscreen-wheel-scroll-lines` | `Fullscreen wheel scrolling` | `Lines per mouse-wheel event in fullscreen mode; 'auto' speeds up fast wheel spins where the terminal does not` | `["auto", ...sortedUnique([1,2,3,5,10,current])]` |
| 35 | `theme` | `Theme` | `Color theme for the interface` | currentValue `config.currentTheme`，**submenu** |

`*` = 仅当 `getCapabilities().images` 为真时插入（插在 `autocompact` 之后，即索引 1、2）。
`auto-resize-images` 等其余项用 `findIndex` 级联插入，确保顺序稳定。

`fullscreen-wheel-scroll-lines` 的 values 生成（**逐字**）：

```ts
["auto", ...([...new Set([1, 2, 3, 5, 10, config.fullscreenWheelScrollLines])]
  .filter(lines => lines !== "auto")
  .sort((a, b) => a - b)
  .map(String))]
```

（`config.fullscreenWheelScrollLines` 类型是 `number | "auto"`；`"auto"` 被过滤掉后若为数字则参与排序去重。）

**`onChange` 映射**（逐字，`SettingsList` 的 onChange 回调）：

| id | 调用 |
|---|---|
| `autocompact` | `onAutoCompactChange(newValue === "true")` |
| `show-images` | `onShowImagesChange(newValue === "true")` |
| `image-width-cells` | `onImageWidthCellsChange(parseInt(newValue, 10))` |
| `auto-resize-images` | `onAutoResizeImagesChange(newValue === "true")` |
| `block-images` | `onBlockImagesChange(newValue === "true")` |
| `skill-commands` | `onEnableSkillCommandsChange(newValue === "true")` |
| `steering-mode` | `onSteeringModeChange(newValue as "all" \| "one-at-a-time")` |
| `follow-up-mode` | `onFollowUpModeChange(...)` |
| `transport` | `onTransportChange(newValue as Transport)` |
| `share-destination` | `onSessionShareDestinationChange(...)` |
| `http-idle-timeout` | 查 `HTTP_IDLE_TIMEOUT_CHOICES.find(c => c.label === newValue)` → `onHttpIdleTimeoutMsChange(choice.timeoutMs)`（找不到则不调用） |
| `cache-warming-mode` | `onCacheWarmingModeChange(...)` |
| `hide-thinking` | `onHideThinkingBlockChange(newValue === "true")` |
| `mermaid-rendering` | `onMermaidRenderingModeChange(...)` |
| `cache-miss-notices` | `onShowCacheMissNoticesChange(newValue === "true")` |
| `collapse-changelog` | `onCollapseChangelogChange(newValue === "true")` |
| `quiet-startup` | `onQuietStartupChange(newValue === "header" ? "header" : newValue === "true")` |
| `install-telemetry` | `onEnableInstallTelemetryChange(newValue === "true")` |
| `default-project-trust` | 查 `DEFAULT_PROJECT_TRUST_BY_LABEL` → `onDefaultProjectTrustChange(key)`（找不到不调用） |
| `double-escape-action` | `onDoubleEscapeActionChange(newValue as "fork" \| "tree")` |
| `tree-filter-mode` | `onTreeFilterModeChange(newValue as ...)` |
| `show-hardware-cursor` | `onShowHardwareCursorChange(newValue === "true")` |
| `editor-padding` | `onEditorPaddingXChange(parseInt(newValue, 10))` |
| `output-padding` | `onOutputPadChange(newValue === "0" ? 0 : 1)` |
| `autocomplete-max-visible` | `onAutocompleteMaxVisibleChange(parseInt(newValue, 10))` |
| `clear-on-shrink` | `onClearOnShrinkChange(newValue === "true")` |
| `terminal-progress` | `onShowTerminalProgressChange(newValue === "true")` |
| `tui-mode` | `onTuiModeChange(newValue as TuiMode)` |
| `fullscreen-exit-output` | `onFullscreenExitOutputChange(...)` |
| `fullscreen-scrollbar` | `onFullscreenScrollbarChange(newValue as ScrollViewScrollbar)` |
| `fullscreen-copy-on-select` | `onFullscreenCopyOnSelectChange(newValue === "true")` |
| `fullscreen-wheel-scroll-lines` | `onFullscreenWheelScrollLinesChange(newValue === "auto" ? "auto" : parseInt(newValue, 10))` |
| `theme` | `onThemeChange(newValue)` |

**布局**：`DynamicBorder()` → `SettingsList(items, 10, getSettingsListTheme(), onChange, callbacks.onCancel,
{ enableSearch: true })` → `DynamicBorder()`。**没有** Spacer。

**`WarningSettingsSubmenu`**（**【更正】继承 `Container`，不是 `SelectSubmenu`**；模块私有）：

```ts
class WarningSettingsSubmenu extends Container {
  private settingsList: SettingsList;
  private state: WarningSettings;                 // 构造时 {...warnings}
  constructor(warnings: WarningSettings, onChange: (warnings: WarningSettings) => void,
              onCancel: () => void);
  handleInput(data: string): void;                // → settingsList.handleInput(data)
}
```

- 子组件只有 `SettingsList`（**无 DynamicBorder**）；`SettingsList(items, Math.min(items.length, 10),
  getSettingsListTheme(), onChange, onCancel)`。
- 单项 **【逐字】**：`{ id: "anthropic-extra-usage", label: "Anthropic extra usage",
  description: "Warn when Anthropic subscription auth may use paid extra usage", values: ["true","false"] }`
  —— **含 `paid`**（§2.40 旧文写「子菜单里没有 paid」是**反了**；`paid` 在子菜单项里）。
  `currentValue` = `(state.anthropicExtraUsage ?? SETTINGS_DEFAULTS.warnings.anthropicExtraUsage) ? "true" : "false"`。
- 切换时：`state = { ...state, anthropicExtraUsage: newValue === "true" }` → `onChange({ ...state })`。
  （源码没有另行调用 `onDone`；宿主把 `onChange` 接到 `onWarningsChange`。）

**`ThemeSubmenu`**（**【更正】继承 `Container`**，自绘 + `setContent(render, input)` 两层；模块私有）：

```ts
class ThemeSubmenu extends Container {
  private inputComponent: Component | undefined;
  private mode: "single" | "automatic";
  private singleTheme: string; private lightTheme: string; private darkTheme: string;
  constructor(currentThemeSetting: string, terminalTheme: TerminalTheme, availableThemes: string[],
              callbacks: SettingsCallbacks, onDone: (selectedValue?: string) => void);
  handleInput(data: string): void;              // → inputComponent?.handleInput?.(data)
  private setContent(renderComponent: Component, inputComponent: Component = renderComponent): void; // clear + addChild + 记录输入目标
  private getThemeSetting(): string;            // automatic ? `${light}/${dark}` : singleTheme
  private getActiveAutomaticTheme(): string;    // terminalTheme === "light" ? lightTheme : darkTheme
  private getAutomaticThemeSetting(): string;   // `${lightTheme}/${darkTheme}`
  private apply(themeSetting: string): void;    // onDone(themeSetting)
  private cancel(): void;                       // onThemePreview?.(originalThemeSetting); onDone()
}
```

- 判断 `parseAutoThemeSetting(currentThemeSetting)`：
  - `null` → **single 模式**：`SelectSubmenu("Theme", "Select a theme, or choose automatic to follow terminal appearance.",
    singleModeThemeItems(availableThemes, singleTheme), singleTheme, onSelect, () => cancel(), onPreview)`。
  - 否则 → **automatic 模式**：标题 `theme.bold(theme.fg("accent","Automatic Theme"))`，
    description（muted，两行）：**`Choose themes for terminal light and dark appearance.`** +
    `Light/dark detection requires terminal support.`
    （**【更正】** 不是 "for light and dark terminal appearance"），items：
    - `{ value: "light-theme", label: "Light theme", description: "Theme to use in automatic mode when the terminal is light" }`（submenu）
    - `{ value: "dark-theme", label: "Dark theme", description: "Theme to use in automatic mode when the terminal is dark" }`（submenu）
    - `{ value: "apply", label: "Apply", description: "Save and go back", currentValue: "save and go back", values: ["save and go back"] }`
    - `{ value: "single-mode", label: "Change mode", description: "Switch to one theme for light and dark", currentValue: "switch to single theme", values: ["switch to single theme"] }`
    - light/dark 两项的 `submenu` 分别 `createThemeSelect("Light Theme",
      "Select the theme to use for light terminal appearance", ...)` 与
      `createThemeSelect("Dark Theme", "Select the theme to use for dark terminal appearance", ...)`；
      选中后先写回 `lightTheme`/`darkTheme`，再 `onThemePreview?.(getThemeSetting())`，再 `done(value)`。
- `onPreview`（`callbacks.onThemePreview?.(name)`）在光标移动时调用；`cancel()` 会
  `onThemePreview(originalThemeSetting)` 恢复原主题（**逐字**：`this.callbacks.onThemePreview?.(this.originalThemeSetting)`）。
- `apply(themeSetting)` → `onDone(themeSetting)`。
- 选 `single-mode`（automatic 菜单里）→ 直接：`mode = "single"`、
  `singleTheme = getActiveAutomaticTheme()`、`onThemePreview?.(singleTheme)`、`showSingleMenu()`。
  **不**调用 `apply`。
- single 菜单里选 `AUTOMATIC_THEME_VALUE`（`"/"`）→ `mode = "automatic"`、
  `onThemePreview?.(getThemeSetting())`、`showAutomaticMenu()`（**不** onDone）。

**逐字字符串汇总**：`none`、` configured`、`Automatic Theme`、
`Choose themes for light and dark terminal appearance.`、
`Light/dark detection requires terminal support.`、`Light theme`、`Dark theme`、`Apply`、`Save and go back`、
`save and go back`、`Change mode`、`Switch to one theme for light and dark`、`switch to single theme`、
`Theme created from your terminal's colors`、`  automatic`、`✓ `、`  `、`  (clear override)`、`(clear override)`、
`Revert to global default (`、`)`、`Per-Model Thinking Level`、`Select a model to configure`、
`No models available`、`Log in to a provider or configure an API key first`、
`Select default thinking level for this model`、`Thinking Level for `、`Anthropic extra usage`、
`Warn when Anthropic subscription auth may use extra usage`、`configure`、`__clear__`。

- 测试（`settings-selector.test.ts` 5 项）：fullscreen 循环（wheel scroll lines 值集含 auto 与排序后数字）；
  share destinations 列表；固定主题时当前项带 `✓`；automatic 主题项；每模型思考等级带 `✓`。

### 2.41 `config-selector.ts`（951 行）— `/config` 资源管理

> **⚠ 本节多处有误，以 §2.52.5 为准。** 摘要：`ResourceList` **自带搜索框**；checkbox 用 `[+]`/`[-]`；
> 光标是 `> `；`pageUp`/`pageDown` 的方向与直觉相反；`filterItems` 只保留命中 item 与其祖先头。


**【精读更正 · 导出面】** 本文件**只导出 2 个符号**：

```ts
export type ScopedResolvedPaths = Record<ConfigWriteScope, ResolvedPaths>;
export class ConfigSelectorComponent extends Container implements Focusable { ... }
```

其余全部是模块私有：`type ResourceType`（文件内局部定义，**不是**从 `RESOURCE_TYPES` 推导）、
`type ConfigWriteScope = "global" | "project"`、`type SettingsScope = "user" | "project"`、
`type ProjectOverrideState = "inherit" | "load" | "unload"`、
`const RESOURCE_TYPES`、`const RESOURCE_TYPE_LABELS`、
`interface ResourceItem`、`interface ResourceSubgroup`、`interface ResourceGroup`、
`type FlatEntry`、`class ConfigSelectorHeader`、`class ResourceList`、函数 `formatBaseDir(baseDir: string)` /
`getGroupLabel(metadata: PathMetadata, agentDir: string)` /
`buildGroups(resolved: ResolvedPaths, agentDir: string): ResourceGroup[]`。
Rust 侧如果要给这些内部件写单测，必须显式提权（`pub(crate)`）并**不算**公共 API。

```ts
const RESOURCE_TYPES = ["extensions", "skills", "prompts", "themes"] as const satisfies readonly ResourceType[];
type ResourceType = "extensions" | "skills" | "prompts" | "themes";
```

`ConfigSelectorComponent` 的公开成员：`focused` getter/setter（代理到 `resourceList.focused`）、
`getResourceList(): ResourceList`。**没有** `setQuery`/`setState`/`setWriteScope` 公开方法。

**`RESOURCE_TYPES` 与显示名**：

```ts
const RESOURCE_TYPE_LABELS: Record<ResourceType, string> = {
  extensions: "Extensions",
  skills: "Skills",
  prompts: "Prompts",
  themes: "Themes",
};
```

**`formatBaseDir(baseDir)`**（**【更正】** 用的是 `os.homedir()`，**不读** `HOME`/`USERPROFILE` 环境变量）：

```ts
const homeDir = homedir();                 // node:os
let displayPath: string;
if (baseDir === homeDir) {
  displayPath = "~";                       // 精确等于 home → 单个 ~（随后补 / → "~/")
} else if (baseDir.startsWith(homeDir)) {
  const rest = baseDir.slice(homeDir.length);
  displayPath = `~${rest.replace(/\\/g, "/")}`;
} else {
  displayPath = baseDir.replace(/\\/g, "/");
}
return displayPath.endsWith("/") ? displayPath : `${displayPath}/`;
```

（home 内 → `~/...`；否则原路径反斜杠转斜杠；**保证以 `/` 结尾**。注意 `baseDir === homeDir` 是**单独分支**
（得到 `"~/"`），而 `startsWith` 分支对 `rest === ""` 也会得到 `"~"` + `"/"` = `"~/"`，两条路径结果一致，
但 Rust 侧必须按上面三段式写，因为 `baseDir = homeDir + "x"` 之类的边界只在 `startsWith` 分支里成立。）

**`getGroupLabel(metadata, agentDir)`**（**【更正】** 判定 builtin/auto 用的是 `metadata.source`，**不是**
`metadata.origin`；`PathMetadata.origin` 只有 `"package" | "top-level"` 两种取值）：

```ts
if (metadata.origin === "package") return `${metadata.source} (${metadata.scope})`;
if (metadata.source === "builtin")
  return metadata.scope === "user" ? "Built-in" : "Built-in (project override)";
if (metadata.source === "auto") {
  if (metadata.baseDir) return metadata.scope === "user"
    ? `User (${formatBaseDir(metadata.baseDir)})`
    : `Project (${formatBaseDir(metadata.baseDir)})`;
  return metadata.scope === "user"
    ? `User (${formatBaseDir(agentDir)})`
    : `Project (${CONFIG_DIR_NAME}/)`;                    // CONFIG_DIR_NAME = ".kiana"
}
return metadata.scope === "user" ? "User settings" : "Project settings";
```

**`buildGroups(resolved, agentDir)`**（**【更正】** 入参是 `ResolvedPaths`，内部对四条资源数组分别
`addToGroup(resolved.extensions,"extensions")` / `skills` / `prompts` / `themes`）：

- `groupKey = `${metadata.origin}:${metadata.scope}:${metadata.source}:${metadata.baseDir ?? ""}``。
  组对象同时记 `{key, label: getGroupLabel(metadata, agentDir), scope, origin, source, subgroups: []}`。
- 每组内按 `RESOURCE_TYPES` 声明顺序分子组（子组在首次出现该 type 时惰性创建，因此**不会**出现空子组）。
- 条目显示名（`displayName`）（**【更正】** 首条判定用 `metadata.source === "builtin"`）：
  - `metadata.source === "builtin"` → `path.slice(BUILTIN_PATH_PREFIX.length)`（`BUILTIN_PATH_PREFIX = "builtin:"`）；
  - `extensions` 且 `parentFolder !== "extensions"` → `` `${parentFolder}/${fileName}` ``；
  - `skills` 且 `fileName === "SKILL.md"` → `parentFolder`；
  - 其它 → `fileName`。
- 组排序：`origin === "package"` 优先 → `scope === "user"` 优先 → `source.localeCompare`。
- 子组排序：`RESOURCE_TYPES` 顺序。
- 条目排序：`displayName.localeCompare`。

**`ConfigSelectorHeader implements Component`**（**【更正】** 位置参数构造，不是 state 对象；方法名是
`setWriteScope`，**没有** `setState`）：

```ts
constructor(writeScope: ConfigWriteScope /* "global" | "project" */, projectModeAvailable: boolean);
setWriteScope(writeScope: ConfigWriteScope): void;
invalidate(): void {}                  // 空实现
render(width: number): string[];       // 恰好 2 行
```

- `title = theme.bold(writeScope === "project" ? "Project Local Resources" : "Global Resources")`
  —— **只看 `writeScope`**，与 `projectModeAvailable` 无关。
- `sep = theme.fg("muted", " · ")`。
- `switchHint = projectModeAvailable ? keyHint("tui.input.tab","switch mode") + sep : ""`。
- `actionHint = writeScope === "project" ? rawKeyHint("space","cycle inherit/+/-") : rawKeyHint("space","toggle")`。
- `hint = switchHint + actionHint + sep + rawKeyHint("esc","close")`。
- 第一行：`title` + 填充空格（`spacing = Math.max(1, width - visibleWidth(title) - visibleWidth(hint))`）+ `hint`。
- 第二行（scopeHint）：
  - project → `theme.fg("muted", `${CONFIG_DIR_NAME}/settings.json · inherited global resources are dimmed`)`
  - user → `theme.fg("muted", `~/${CONFIG_DIR_NAME}/agent/settings.json`)`
- 两行都 `truncateToWidth(..., width, "")` —— **【更正】** 省略号是**空串**（不截断出 `…`），
  不是 §3.4 旧文所写的 `…` U+2026。
- **【逐字】** `Project Local Resources`、`Global Resources`、` · `、`switch mode`、`cycle inherit/+/-`、`toggle`、
  `close`、`.kiana/settings.json · inherited global resources are dimmed`、`~/.kiana/agent/settings.json`。

**`ResourceList implements Component, Focusable`**（**【更正】** 位置参数构造；搜索框是**组件内部**的 `Input`，
没有 `setQuery`/`getGroups`/`getItemState` 回调）：

```ts
constructor(
  groupsByScope: Record<ConfigWriteScope, ResourceGroup[]>,
  settingsManager: SettingsManager,
  cwd: string,
  agentDir: string,
  terminalHeight?: number,
  writeScope: ConfigWriteScope = "global",
);
// 公开成员：focused getter/setter（代理给 searchInput.focused）、
// setWriteScope(scope)、invalidate()、updateItem(item, enabled)、
// render(width)、handleInput(data)；
// 公开可变回调字段：onCancel? / onExit? / onToggle?(item, newEnabled) / onSwitchMode?。
// 私有：groups getter、buildInheritedEnabledMap、buildFlatList、findNextItem、
// filterItems、selectFirstItem、toggleResource、toggleTopLevelResource、togglePackageResource、
// renderCheckbox、getItemSuffix、isDimmedItem、setProjectResourceOverride、
// setProjectTopLevelOverride、setProjectTopLevelPaths、setProjectPackageOverride、
// getNextOverrideState、getProjectOverrideState、getOverrideStateFromEntries、
// getInheritedEnabled、isInheritedGlobalItem、getTopLevelOverridePatterns、
// getResourcePatternForScope、createPackageOverrideSource、packageSourceStringMatches、
// findMatchingPackageSource、getPatternEntryTarget、getResourceItemKey、getItemScope、
// getTopLevelBaseDir、getResourcePattern、getPackageResourcePattern。
```

- `chrome = 8`（注释：top spacer + top border + spacer + header 2 行 + spacer + bottom spacer + bottom border）；
  `maxVisible = Math.max(5, (terminalHeight ?? 24) - 8)`。
- `inheritedEnabledByKey` 在构造时由 `groupsByScope.global` 建立（key = `getResourceItemKey` =
  `` `${resourceType}:${canonicalizePath(path)}` ``），project 视图的「继承值」全部查这张表。
- 扁平结构：`group` → `subgroup` → `item` 顺序 push；`selectedIndex` 初始 =
  `flatItems.findIndex(e => e.type === "item")`，无 item 时置 0。
- 过滤由**内部 `searchInput`** 驱动：`handleInput` 末尾把未匹配键交给 `searchInput.handleInput(data)`，
  然后用 `filterItems(searchInput.getValue())`：
  - `query.trim()` 为空 → `filteredItems = [...flatItems]` 并 `selectFirstItem()`；
  - 否则 `q = query.toLowerCase()`；item 命中条件：`displayName`/`resourceType`/`path` 任一 `toLowerCase().includes(q)`；
    再扫一遍组树收集命中的 subgroup/group，最后按 `flatItems` 原序重建 `filteredItems`（保留命中项的祖先头）；
  - 无论哪条分支都 `selectFirstItem()`（选中项在每次按键后跳回第一个 item）。
- **渲染（顺序即行为）**：
  1. `searchInput.render(width)` 的所有行；
  2. 一个空行 `""`；
  3. `filteredItems.length === 0` → 只再 push `theme.fg("muted", "  No resources found")` 并 **return**
     （**【原文缺失】** 空态文案 `  No resources found`，两个前导空格）；
  4. 可视窗口：`startIndex = Math.max(0, Math.min(selectedIndex - Math.floor(maxVisible/2),
     filteredItems.length - maxVisible))`，`endIndex = Math.min(startIndex + maxVisible, filteredItems.length)`
     —— 注意 `maxVisible` 计的是**扁平条目数**（含 group/subgroup 头），不是 item 数；
  5. group 行：`inherited = writeScope === "project" && group.scope === "user"`；
     `label = theme.bold(`${group.label}${inherited ? " · inherited global" : ""}`)`；
     再整体 `theme.fg(inherited ? "dim" : "accent", label)`；行 = `truncateToWidth(`  ${groupLine}`, width, "")`
     —— **【更正】** 两个前导空格、省略号 `""`，且 ` · inherited global` 在 bold 之内。
  6. subgroup 行：`color = (writeScope === "project" && group.scope === "user") ? "dim" : "muted"`；
     行 = `truncateToWidth(`    ${theme.fg(color, subgroup.label)}`, width, "")`（4 个前导空格）。
  7. item 行 = `truncateToWidth(`${cursor}    ${checkbox} ${name}${suffix}`, width, "...")` —— **【更正】**
     这里的省略号是 `"..."`（三个 ASCII 点），不是 `…`：
     - `cursor = isSelected ? "> " : "  "`（**未**着色）；
     - `dimmed = writeScope === "project" && isInheritedGlobalItem(item) && getProjectOverrideState(item) === "inherit"`；
     - `nameText = isSelected && !dimmed ? theme.bold(item.displayName) : item.displayName`；
       `name = dimmed ? theme.fg("dim", nameText) : nameText`；
     - `renderCheckbox`：
       - project：state `load` → `theme.fg("success","[+]")`；`unload` → `theme.fg("warning","[-]")`；
         其它 → `theme.fg("dim", item.enabled ? "[x]" : "[ ]")`（用 **item.enabled**，不是 inherited 值）；
       - 非 project：`item.enabled ? theme.fg("success","[x]") : theme.fg("dim","[ ]")`；
     - `getItemSuffix`（非 project 恒为 `""`）：`load` → `theme.fg("muted","  project load")`；
       `unload` → `theme.fg("muted","  project unload")`；否则若 `isInheritedGlobalItem` →
       `theme.fg("dim","  inherited global")`；否则 `""`。
  8. 滚动条：`if (startIndex > 0 || endIndex < filteredItems.length)` →
     `theme.fg("dim", `  (${currentItemIndex}/${itemCount})`)`，其中
     `itemCount = filteredItems.filter(e => e.type === "item").length`、
     `currentItemIndex = filteredItems.slice(0, selectedIndex).filter(e => e.type === "item").length + 1`
     —— **1 基**的 item 序号（非行号），两个前导空格。
- **`getNextOverrideState(state, inheritedEnabled)`**（**签名更正**：实际是 `getNextOverrideState(item)`，
  内部先 `getProjectOverrideState(item)` 再 `getInheritedEnabled(item)`）：
  ```ts
  if (state === "inherit") return inheritedEnabled ? "unload" : "load";
  if (state === "unload") return inheritedEnabled ? "load" : "inherit";
  /* load */               return inheritedEnabled ? "inherit" : "unload";
  ```
  即三态循环：inherit → load（全局未启用）/ unload（全局已启用）→ unload/load → …
- 切换（`toggleResource(item): boolean | undefined`，**【补充返回值语义】**）：
  - `writeScope === "project"`：`state = getNextOverrideState(item)`；
    `setProjectResourceOverride(item, state)` 返回 false → 整体返回 `undefined`（上层不更新 UI）；
    否则返回 `state === "inherit" ? getInheritedEnabled(item) : state === "load"`（即切换后的**有效启用值**）。
  - 非 project：`enabled = !item.enabled`；`origin === "top-level"` → `toggleTopLevelResource(item, enabled)`，
    否则 `togglePackageResource(item, enabled)`；返回 `enabled`。
  - 上层（`handleInput`）只在新值 `!== undefined` 时执行 `updateItem(item, newEnabled)` 与
    `onToggle?.(item, newEnabled)`。
  - project scope → 改 project settings 的 `+pattern`/`-pattern`（**注意：不是**改动 `+`/`-` 的语义，
    而是见 `setProjectTopLevelOverride` / `setProjectPackageOverride`）：
    - `load` → 追加 `+pattern`；
    - `unload` → 追加 `-pattern`；
    - `inherit` → **不追加**，只把该 pattern 的 `!`/`+`/`-` 覆盖全部删掉。
    - `setProjectTopLevelOverride` 的过滤规则（**逐字**）：
      ```ts
      const pattern = isInheritedGlobalItem(item) ? item.path : getResourcePatternForScope(item, "project");
      const patterns = getTopLevelOverridePatterns(item, "project");
      const updated = current.filter((entry) => {
        const target = getPatternEntryTarget(entry);
        if ((entry.startsWith("!") || entry.startsWith("+") || entry.startsWith("-")) && patterns.has(target))
          return false;
        return !(state === "inherit" && isInheritedGlobalItem(item) && target === pattern);
      });
      if (state !== "inherit") {
        // project 条目要「点名」被覆盖的继承文件；builtin 路径无需条目
        if (isInheritedGlobalItem(item) && item.metadata.source !== "builtin" && !updated.includes(pattern))
          updated.push(pattern);                     // 先 push 裸 pattern（点名）
        updated.push(`${state === "load" ? "+" : "-"}${pattern}`);   // 再 push 带符号条目
      }
      ```
      —— **【重要】** 对「继承来的全局资源」在 project 侧覆盖时，写入的是**两条**：裸 `pattern` +
      `+pattern`/`-pattern`，除非 `metadata.source === "builtin"`（builtin 只写带符号那条）。
    - `setProjectPackageOverride`：找不到 package 且 `state === "inherit"` → 返回 **false**；
      找不到且非 inherit → `packages.push(createPackageOverrideSource(item))`（本地路径会转成
      相对 project 的路径 + `autoload: false`；非本地路径 → `{ source, autoload: false }`）。
      写入后若该 pkg 四个过滤数组**全为 undefined**：`autoload === false` → `packages.splice(pkgIndex,1)`，
      否则 `packages[pkgIndex] = pkg.source`（还原为字符串）。
  - `getTopLevelOverridePatterns(item, scope)` 的集合 = `{ getResourcePatternForScope(item, scope), item.path,
    relative(baseDir, item.path) }`，且若 `metadata.baseDir` 存在再加 `relative(metadata.baseDir, item.path)`；
    `baseDir = getTopLevelBaseDir(scope)`（project → `join(cwd, CONFIG_DIR_NAME)`；user → `agentDir`）。
  - `getResourcePatternForScope(item, scope)`：`scope !== getItemScope(item) || metadata.source === "builtin"`
    → 返回 `item.path`；否则 `relative(metadata.baseDir ?? getTopLevelBaseDir(sourceScope), item.path)`。
  - user scope（非 project 模式，或 project 模式下的「用户级资源」）→ `toggleTopLevelResource`：
    顶层 `extensions`/`skills`/... 数组里先剔除同 pattern 的既有项（剥掉 `!`/`+`/`-` 前缀后比较），
    再按 `enabled` push `+pattern` 或 `-pattern`（**【更正】** 顶层也用 `+`/`-` 前缀，不是裸 pattern）。
    **注意**：project 模式下对 user-scope 资源调用的是 `toggleResource` 的 **project 分支**（三态覆盖），
    因此 `toggleTopLevelResource` 只会在全局模式下走到。
  - package 的开关（全局模式）→ `togglePackageResource`：改 `settings.packages` 中 `source` 完全相等
    （`typeof pkg === "string" ? pkg : pkg.source === item.metadata.source`）的那一项；
    先剥离同 pattern 的既有项再 push `+`/`-`；
    `pkg[arrayKey] = updated.length > 0 ? updated : undefined`；
    四个数组全 undefined 时：`autoload === false` → 删除条目，否则还原为字符串形式。
  - 辅助（全部**私有方法**）：`getTopLevelOverridePatterns`、`getResourcePatternForScope`、
    `createPackageOverrideSource`、`packageSourceStringMatches`、`findMatchingPackageSource`、
    `getPatternEntryTarget`（剥 `!`/`+`/`-` 前缀）、`getResourceItemKey`、
    `getItemScope`、`getTopLevelBaseDir`、`getResourcePattern`、`getPackageResourcePattern`
    （`relative(metadata.baseDir ?? dirname(item.path), item.path)`）。**这些模式串写入 settings 的确切文本**
    是外部格式的一部分（§4.2）。
- 键位（**实际顺序**，见 `handleInput`）：
  ```ts
  tui.select.up        → selectedIndex = findNextItem(selectedIndex, -1)   // 只在 item 间移动
  tui.select.down      → findNextItem(+1)
  tui.select.pageUp    → target = max(0, selectedIndex - maxVisible); 向下找第一个 item；找到则选中
  tui.select.pageDown  → target = min(len-1, selectedIndex + maxVisible); 向上找第一个 item；找到则选中
  tui.select.cancel    → onCancel?.()
  ctrl+c (matchesKey)  → onExit?.()
  tui.input.tab        → onSwitchMode?.()
  " " || tui.select.confirm → 仅当选中项是 item 且 (writeScope==="project" || getItemScope(item)==="user")
                          → toggleResource → updateItem + onToggle(item, newEnabled)
  其它                  → searchInput.handleInput(data); filterItems(searchInput.getValue())
  ```
  **【注意】** pageUp/pageDown 用 `tui.select.pageUp` / `tui.select.pageDown`（不是
  `tui.editor.cursorLeft/Right`，后者是 tree-selector 的约定）；up/down **不环绕**（`findNextItem` 到了
  边界返回原索引）。
- **【逐字】** `[+]`、`[-]`、`[x]`、`[ ]`、`  project load`、`  project unload`、`  inherited global`、
  `> `、` · inherited global`、`  No resources found`、`Extensions`、`Skills`、`Prompts`、`Themes`、
  `Project Local Resources`、`Global Resources`。

**`ConfigSelectorComponent`**（**【更正】** 完整签名）：

```ts
constructor(
  resolvedPaths: ScopedResolvedPaths,        // { global: ResolvedPaths, project: ResolvedPaths }
  settingsManager: SettingsManager,
  cwd: string,
  agentDir: string,
  onClose: () => void,                       // 对应 ResourceList.onCancel（esc）
  onExit: () => void,                        // 对应 ResourceList.onExit（ctrl+c）
  requestRender: () => void,
  terminalHeight?: number,
  writeScope: ConfigWriteScope = "global",
  projectModeAvailable = true,
);
```

- 渲染顺序：`Spacer(1)` → `DynamicBorder()` → `Spacer(1)` → `header` → `Spacer(1)` → `resourceList` →
  `Spacer(1)` → `DynamicBorder()`。
- 构造时 `buildGroups(resolvedPaths.global, agentDir)` 与 `buildGroups(resolvedPaths.project, agentDir)` 各算一次，
  存为 `groupsByScope`；`ResourceList` 内部按 `writeScope` 切换（`setWriteScope` 会 `buildFlatList()` +
  用现有搜索词重新 `filterItems`）。
- `resourceList.onToggle = () => requestRender()`（**忽略两个参数**）；
  `onSwitchMode` **仅在 `projectModeAvailable` 为真时挂载**，回调 = `switchWriteScope() + requestRender()`。
- `switchWriteScope()`：`global ⇄ project`，然后 `header.setWriteScope(...)` + `resourceList.setWriteScope(...)`
  —— **【更正】** 不是 `header.setState`，也不重建 `ResourceList` 实例。
- `getResourceList(): ResourceList` 暴露列表。
- `focused` setter 只代理给 `resourceList.focused`（`ResourceList.focused` 再代理给 `searchInput.focused`）。

### 2.42 `session-selector.ts`（1045 行）— `/resume` 会话选择器

> **⚠ 本节多处有误，以 §2.60.10 为准。**（`SessionSelectorHeader` 位置参数与 setter、`loadScope` 的
> `isActive()` 竞态、`toggleScope` 不 abort、rename/delete 流程）


本文件最大，含 4 个类 + 2 个纯函数 + 文件删除逻辑。Rust 侧建议拆成 `session_selector/{header,list,search_tree,delete}.rs`。

**【精读更正 · 导出面】** 本文件**只导出 1 个符号**：`export class SessionSelectorComponent extends Container
implements Focusable`。其余全是模块私有：

```ts
type SessionScope = "current" | "all";
type SessionsLoader = (onProgress?: SessionListProgress, signal?: AbortSignal) => Promise<SessionInfo[]>;
function shortenPath(path: string): string;
function formatSessionDate(date: Date): string;
function canonicalizePath(path: string | undefined): string | undefined;   // 包装 utils/paths 并透传 undefined
interface SessionTreeNode { ... }
interface FlatSessionNode { ... }
function buildSessionTree(sessions: SessionInfo[]): SessionTreeNode[];
function flattenSessionTree(roots: SessionTreeNode[]): FlatSessionNode[];
class SessionSelectorHeader implements Component { ... }
class SessionList implements Component, Focusable { ... }
async function deleteSessionFile(sessionPath: string): Promise<{ ok: boolean; method: "trash" | "unlink"; error?: string }>;
```

`SessionSelectorComponent` 公开成员：`handleInput(data)`（**重写**，见下）、`focused` getter/setter
（代理到 `sessionList.focused` 与 `renameInput.focused`）、`getSessionList(): SessionList`。
`SessionSelectorHeader` 的真实 API（**位置参数构造，逐 setter**）：

```ts
constructor(scope: SessionScope, sortMode: SortMode, nameFilter: NameFilter, requestRender: () => void);
setScope(scope) / setSortMode(sortMode) / setNameFilter(nameFilter)
setLoading(loading: boolean)          // 同时把 loadProgress 置 null
setProgress(loaded: number, total: number)
setShowPath(showPath) / setShowRenameHint(show)
setConfirmingDeletePath(path: string | null)   // 【更正】不是 setDeleteConfirmation
setStatusMessage(msg, autoHideMs?): void
invalidate(): void {}                 // 空实现
render(width: number): string[]       // 恰好 3 行
```

**`shortenPath(path)`**（**【更正】** 用 `os.homedir()`，不读 `HOME`/`USERPROFILE`）：

```ts
const home = os.homedir();
if (!path) return path;                 // 空串/undefined 原样返回
if (path.startsWith(home)) return `~${path.slice(home.length)}`;
return path;
```

（注意**不是** `~` + `/` 的规范化，也没有 Windows 反斜杠处理；`path === home` → `"~"`。）

**`formatSessionDate(date)`**（**【更正】** 用三个独立差值，不是逐级 `Math.floor` 链；结果等价，
但 Rust 侧照抄可避免负数分钟链式误差）：

```ts
const diffMs = new Date().getTime() - date.getTime();
const diffMins = Math.floor(diffMs / 60000);
const diffHours = Math.floor(diffMs / 3600000);
const diffDays = Math.floor(diffMs / 86400000);
if (diffMins < 1) return "now";
if (diffMins < 60) return `${diffMins}m`;
if (diffHours < 24) return `${diffHours}h`;
if (diffDays < 7) return `${diffDays}d`;
if (diffDays < 30) return `${Math.floor(diffDays / 7)}w`;
if (diffDays < 365) return `${Math.floor(diffDays / 30)}mo`;
return `${Math.floor(diffDays / 365)}y`;
```

（`60_000` 写法等价于 `60000`。）

**`SessionSelectorHeader`**：

- 状态字段（全私有）：`scope`、`sortMode`、`nameFilter`、`requestRender`、`loading = false`、
  `loadProgress = null`、`showPath = false`、`confirmingDeletePath = null`、`statusMessage = null`、
  `statusTimeout = null`、`showRenameHint = false`。
- `setStatusMessage(msg, autoHideMs?)`（**【更正】** **没有默认值 2000**）：
  先 `clearStatusTimeout()`；`statusMessage = msg`；**`if (!msg || !autoHideMs) return;`**
  —— 即不传 `autoHideMs` 时消息**永久显示**，只有显式传值才 `setTimeout(clear + requestRender, autoHideMs)`。
  定时器回调里 `statusMessage = null; statusTimeout = null; requestRender()`。
  调用方实际都显式传了值（info 2000 / error 3000~4000）。
- `setLoading(bool)` 会**清空** `loadProgress`（进度只属于当前这次加载）。
- `setConfirmingDeletePath(path)`（**【更正】** 方法名）。
- `render(width)` 返回 **3 行**：
  1. `left = theme.bold(title)`；`right = truncateToWidth(`${scopeText}  ${nameText}  ${sortText}`, width, "")`；
     `availableLeft = max(0, width - visibleWidth(right) - 1)`；`left = truncateToWidth(left, availableLeft, "")`；
     `spacing = max(0, width - visibleWidth(left) - visibleWidth(right))`；
     行 = `left + " ".repeat(spacing) + right`。
     - `title`：`"Resume Session (Current Folder)"` / `"Resume Session (All)"`。
     - `sortLabel`：`"Threaded"` / `"Recent"` / `"Fuzzy"`；`sortText = muted("Sort: ") + accent(sortLabel)`。
     - `nameLabel`：`"All"` / `"Named"`；`nameText = muted("Name: ") + accent(nameLabel)`。
     - `scopeText`：
       - loading：`muted("○ Current Folder | ") + accent(`Loading ${loaded}/${total}`)`；`loadProgress` 为
         null 时用 `"..."` → `` `Loading ...` ``。
       - current：`accent("◉ Current Folder") + muted(" | ○ All")`。
       - all：`muted("○ Current Folder | ") + accent("◉ All")`。
  2. `hintLine1`：
     - `confirmingDeletePath !== null` → `theme.fg("error", truncateToWidth(`Delete session? ${keyHint("tui.select.confirm","confirm")} · ${keyHint("tui.select.cancel","cancel")}`, width, "…"))`
     - 否则 `statusMessage` → `theme.fg(status.type === "error" ? "error" : "accent", truncateToWidth(message, width, "…"))`
     - 否则 `hint1 = keyHint("tui.input.tab","scope") + sep + muted('re:<pattern> regex · "phrase" exact')`，
       `sep = muted(" · ")`；`truncateToWidth(hint1, width, "…")`。
  3. `hintLine2`：
     - 前两种状态 → `""`；
     - 否则 `hint2Parts = [keyHint("app.session.toggleSort","sort"), keyHint("app.session.toggleNamedFilter","named"),
       keyHint("app.session.delete","delete"), keyHint("app.session.togglePath", `path ${pathState}`)]`，
       `pathState = showPath ? "(on)" : "(off)"`；`showRenameHint` 时追加 `keyHint("app.session.rename","rename")`；
       `hint2 = parts.join(sep)`；`truncateToWidth(hint2, width, "…")`。
- **【逐字】** `Resume Session (Current Folder)`、`Resume Session (All)`、`Sort: `、`Threaded`、`Recent`、
  `Fuzzy`、`Name: `、`All`、`Named`、`○ Current Folder | `、`Loading `、`/`、`...`、`◉ Current Folder`、
  ` | ○ All`、`◉ All`、`Delete session? `、`confirm`、`cancel`、` · `、`re:<pattern> regex · "phrase" exact`、
  `scope`、`sort`、`named`、`delete`、`path (on)`、`path (off)`、`rename`、`…`、`  `（两空格分隔 right 三段）。

**树构建（模块内函数，导出仅供测试）**：

```ts
interface SessionTreeNode { session: SessionInfo; children: SessionTreeNode[]; latestActivity: number }

function buildSessionTree(sessions: SessionInfo[]): SessionTreeNode[] {
  // 以 canonicalizePath(session.path) （失败时原 path）为键
  // parentSessionPath 指向本集合内某 session → 成为其子节点，否则为根
  // latestActivity = max(自身 modified.getTime(), 所有后代 latestActivity)
  // 根按 latestActivity 降序排序；子节点同样降序
}

interface FlatSessionNode { session: SessionInfo; depth: number; isLast: boolean; ancestorContinues: boolean[] }

function flattenSessionTree(roots: SessionTreeNode[]): FlatSessionNode[] {
  // DFS：ancestorContinues 记录每层是否为「该层最后一个」
}
```

- **`buildTreePrefix(node)`**：
  ```ts
  if (node.depth === 0) return "";
  return node.ancestorContinues.map(c => c ? "│  " : "   ").join("") + (node.isLast ? "└─ " : "├─ ");
  ```
- 父子关系按**规范化路径**匹配（符号链接别名测试：`session-selector-path-delete.test.ts` 的 symlink aliases）。
- `latestActivity` 取子树最大值 → 「threaded 排序按子树最新活动」测试。

**`SessionList implements Component, Focusable`**（**【精读补充】** 真实 API）：

```ts
constructor(
  sessions: SessionInfo[], showCwd: boolean, sortMode: SortMode, nameFilter: NameFilter,
  keybindings: KeybindingsManager,                 // 宿主注入（不是全局 getKeybindings）
  currentSessionFilePath?: string,
);
// 公开方法：getSelectedSessionPath(): string | undefined、setSortMode、setNameFilter、
//           setSessions(sessions, showCwd)、invalidate()（空）、render(width)、handleInput(data)、
//           focused getter/setter（代理给 searchInput.focused）
// 公开可变回调字段：onSelect? / onCancel? / onExit（默认 () => {}）/ onToggleScope? / onToggleSort? /
//   onToggleNameFilter? / onTogglePath?(showPath) / onDeleteConfirmationChange?(path) /
//   onDeleteSession?(path): Promise<void> / onRenameSession?(path) / onError?(message)
// 私有：allSessions[]、filteredSessions: FlatSessionNode[]、selectedIndex=0、selectionTouched=false、
//   searchInput、showCwd=false、sortMode="threaded"、nameFilter="all"、keybindings、showPath=false、
//   confirmingDeletePath=null、currentSessionCanonicalPath?、maxVisible = 10（硬编码，无构造参数）
```

- 构造：`searchInput = new Input()`；**只设 `searchInput.onSubmit`**（选中当前项 → `onSelect(path)`），
  **没有** `onChange`（过滤靠 `handleInput` 末尾手动调用；见 §2.42 原文「onChange → filterSessions」是**错的**）。
  构造末尾立即 `filterSessions("")`。
- `setSessions(sessions, showCwd)`：先按 `selectionTouched` 记录旧选中路径 →
  `allSessions = sessions; showCwd = showCwd; filterSessions(searchInput.getValue())`；
  若 `!selectionTouched` → `selectedIndex = 0`；否则若旧路径仍存在 → 选中它。
  —— **【重点】** 这是「加载完成后不覆盖用户选择」的实现点（`selectionTouched` 初值 false，
  首次 `handleInput` 走到移动分支前就置 true）。
- `maxVisible = 10` **硬编码**（`// Max sessions visible (one line each)`），宿主无法配置。
- `render(width)`：
  1. `searchInput.render(width)` 的行；
  2. **一个空行** `""`；
  3. 若 `filteredSessions.length === 0`：按上文四条空消息（**逐字**）：
     - named + showCwd：`` `  No named sessions found. Press ${toggleKey} to show all.` ``
     - named + !showCwd：`` `  No named sessions in current folder. Press ${toggleKey} to show all, or Tab to view all.` ``
     - showCwd：`  No sessions found`
     - else：`  No sessions in current folder. Press Tab to view all.`
     均 `theme.fg("muted", truncateToWidth(msg, width, "…"))`，然后**返回**（只有这些行）。
  4. 可见范围：`maxVisible = 10`（**【更正】** 硬编码字段，非构造参数；`Math.max(5, floor(terminalHeight/2))`
     是 `tree-selector` 的规则，不要混用）；
     `startIndex = max(0, min(selectedIndex - floor(maxVisible/2), len - maxVisible))`；
     `endIndex = min(startIndex + maxVisible, len)`。
  5. 每行（**逐字构造顺序**）：
     ```ts
     const prefix = buildTreePrefix(node);
     const displayText = session.name ?? session.firstMessage;
     const normalizedMessage = displayText.replace(/[\x00-\x1f\x7f]/g, " ").trim();
     const age = formatSessionDate(session.modified);
     let rightPart = `${session.messageCount} ${age}`;
     if (showCwd && session.cwd) rightPart = `${shortenPath(session.cwd)} ${rightPart}`;
     if (showPath) rightPart = `${shortenPath(session.path)} ${rightPart}`;
     const cursor = isSelected ? theme.fg("accent", "› ") : "  ";
     const availableForMsg = width - 2 - visibleWidth(prefix) - (visibleWidth(rightPart) + 2);
     const truncatedMsg = truncateToWidth(normalizedMessage, Math.max(10, availableForMsg), "…");
     let messageColor = isConfirmingDelete ? "error" : isCurrent ? "accent" : hasName ? "warning" : null;
     let styledMsg = messageColor ? theme.fg(messageColor, truncatedMsg) : truncatedMsg;
     if (isSelected) styledMsg = theme.bold(styledMsg);
     const leftPart = cursor + theme.fg("dim", prefix) + styledMsg;
     const spacing = Math.max(1, width - visibleWidth(leftPart) - visibleWidth(rightPart));
     const styledRight = theme.fg(isConfirmingDelete ? "error" : "dim", rightPart);
     let line = leftPart + " ".repeat(spacing) + styledRight;
     if (isSelected) line = theme.bg("selectedBg", line);
     lines.push(truncateToWidth(line, width));
     ```
  6. 有滚动：`theme.fg("muted", truncateToWidth(`  (${selectedIndex+1}/${len})`, width, ""))`。
- **注意**：`availableForMsg` 可为负 → `Math.max(10, ...)` 保证最小 10；`truncateToWidth` 的省略号是 `…`；
  最终整行再 `truncateToWidth(line, width)`（**无省略号**，直接硬截）。
- 排序模式：
  - `threaded && query.trim() === ""` → `buildSessionTree(nameFiltered)` + `flattenSessionTree`；
  - 其它 → `filterAndSortSessions(nameFiltered, query, sortMode, "all")` 并包成
    `{session, depth: 0, isLast: true, ancestorContinues: []}`。
- `selectedIndex = min(selectedIndex, max(0, len - 1))`（过滤后收敛）。

**`handleInput`（SessionList）**：

**【精读更正】** 实际代码（顺序 + 精确调用）：

```ts
const kb = getKeybindings();
if (this.confirmingDeletePath !== null) {          // 拦截所有键
  if (kb.matches(keyData, "tui.select.confirm")) {
    const pathToDelete = this.confirmingDeletePath;
    this.setConfirmingDeletePath(null);            // 先清空（并通知 onDeleteConfirmationChange(null)）
    void this.onDeleteSession?.(pathToDelete);     // 再异步删除（不 await）
    return;
  }
  if (kb.matches(keyData, "tui.select.cancel")) { this.setConfirmingDeletePath(null); return; }
  return;                                          // 其它键吞掉
}
if (kb.matches(keyData, "tui.input.tab")) { this.onToggleScope?.(); return; }
if (kb.matches(keyData, "app.session.toggleSort")) { this.onToggleSort?.(); return; }
if (this.keybindings.matches(keyData, "app.session.toggleNamedFilter")) { this.onToggleNameFilter?.(); return; }
                                                     // ↑ 【注意】这一条用注入的 this.keybindings，其余用全局 kb
if (kb.matches(keyData, "app.session.togglePath")) {
  this.showPath = !this.showPath; this.onTogglePath?.(this.showPath); return;
}
if (kb.matches(keyData, "app.session.delete")) { this.startDeleteConfirmationForSelectedSession(); return; }
if (kb.matches(keyData, "app.session.rename")) {
  const selected = this.filteredSessions[this.selectedIndex];
  if (selected) this.onRenameSession?.(selected.session.path);      // 【更正】只传 path，不传 name
  return;
}
if (kb.matches(keyData, "app.session.deleteNoninvasive")) {
  if (this.searchInput.getValue().length > 0) {                      // 【更正】用 .length > 0
    this.searchInput.handleInput(keyData);
    this.filterSessions(this.searchInput.getValue());                // 【补充】回填后立即重新过滤
    return;
  }
  this.startDeleteConfirmationForSelectedSession();
  return;
}
this.selectionTouched = true;
if (up)        this.selectedIndex = Math.max(0, this.selectedIndex - 1);
else if (down) this.selectedIndex = Math.min(this.filteredSessions.length - 1, this.selectedIndex + 1);
else if (pageUp)   this.selectedIndex = Math.max(0, this.selectedIndex - this.maxVisible);
else if (pageDown) this.selectedIndex = Math.min(this.filteredSessions.length - 1, this.selectedIndex + this.maxVisible);
else if (confirm) { const s = this.filteredSessions[this.selectedIndex]; if (s && this.onSelect) this.onSelect(s.session.path); }
else if (cancel)  this.onCancel?.();
else { this.searchInput.handleInput(keyData); this.filterSessions(this.searchInput.getValue()); }
```

- **【更正】** up/down/pageUp/pageDown **不环绕**（clamp 到 `[0, len-1]`）；原 §2.42 写的 `wrap` 是错的。
- **【更正】** `selectionTouched = true` 在**所有**非特殊键到达后都会置位（包括 confirm/cancel/普通字符），
  不只是移动键。
- **`selectionTouched`**：抑制 `setSessions` 把 `selectedIndex` 重置为 0 或重定位到旧路径
  （`session-selector-path-delete.test.ts` 的 "scope toggle races"）。
- `startDeleteConfirmationForSelectedSession()`：无选中项 → 直接 return；选中项是当前会话 →
  `onError("Cannot delete the currently active session")` 并 **return（不进入确认态）**；
  否则 `setConfirmingDeletePath(selected.session.path)`（会同步回调 `onDeleteConfirmationChange`）。
- 当前会话比较用 `canonicalizePath(path) ?? path` 与构造时缓存的 `currentSessionCanonicalPath` 比较；
  `currentSessionFilePath` 为空时 `isCurrentSessionPath` 恒 false。
- **【补充】** `render` 里选中行的光标是 `› `（U+203A + 空格，accent），未选中是两空格；
  `isConfirmingDelete` 时**消息文本与右侧部分都变 error 色**，且整行仍套 `selectedBg`（选中时）。
- **【补充】** 空态文案里的 `toggleKey = keyText("app.session.toggleNamedFilter")`（**只**取主键文本，
  不是 `keyHint`，因此没有描述后缀）。

**`deleteSessionFile(sessionPath)`**（模块私有 async）：

```ts
const trashArgs = sessionPath.startsWith("-") ? ["--", sessionPath] : [sessionPath];
const trashResult = spawnSync("trash", trashArgs, { encoding: "utf-8" });
const getTrashErrorHint = () => {
  const parts: string[] = [];
  if (trashResult.error) parts.push(trashResult.error.message);
  const stderr = trashResult.stderr?.trim();
  if (stderr) parts.push(stderr.split("\n")[0] ?? stderr);
  return parts.length === 0 ? null : `trash: ${parts.join(" · ").slice(0, 200)}`;
};
if (trashResult.status === 0 || !existsSync(sessionPath)) return { ok: true, method: "trash" };
try { await unlink(sessionPath); return { ok: true, method: "unlink" }; }
catch (err) {
  const unlinkError = err instanceof Error ? err.message : String(err);
  const hint = getTrashErrorHint();
  return { ok: false, method: "unlink", error: hint ? `${unlinkError} (${hint})` : unlinkError };
}
```

- **【逐字】** `trash: `、` · `、` (`、`)`。
- 子进程语义：`spawnSync` **同步**阻塞、`encoding: "utf-8"`；`trash` 不存在时 `status === null` 且
  `error` 有值 → 直接走 unlink 分支。Rust 用 `std::process::Command`（同步，保持阻塞语义）；
  参数以 `-` 开头时插 `--` 防止被当选项。
- 成功后回调宿主：`result.method === "trash"` → `"Session moved to trash"` 否则 `"Session deleted"`
  （info，2000 ms）；失败 → `` `Failed to delete: ${errorMessage}` ``（error，3000 ms）。
  加载失败 → `` `Failed to load sessions: ${message}` ``（error，4000 ms）。
  **【逐字】** `Session moved to trash`、`Session deleted`、`Failed to delete: `、`Failed to load sessions: `、
  `Cannot delete the currently active session`。

**`SessionSelectorComponent`**：

**【精读更正 · 完整签名】**

```ts
constructor(
  currentSessionsLoader: SessionsLoader, allSessionsLoader: SessionsLoader,
  onSelect: (sessionPath: string) => void, onCancel: () => void, onExit: () => void,
  requestRender: () => void,
  options?: {
    renameSession?: (sessionPath: string, currentName: string | undefined) => Promise<void>;
    showRenameHint?: boolean;
    keybindings?: KeybindingsManager;          // 【原文缺失】注入给 SessionList 用
  },
  currentSessionFilePath?: string,             // 第 8 个**位置**参数，不在 options 里
);
```

- **`options` 里没有** `currentSessionFilePath`、`maxVisible`、`onDeleteConfirmationChange`（原 §2.42 有误）。
  `maxVisible` 在 `SessionList` 里硬编码为 10；`onDeleteConfirmationChange` 是 `SessionList` 自己的公开字段，
  由 `SessionSelectorComponent` 在内部接上（转发给 `header.setConfirmingDeletePath` + `requestRender`）。
- `this.keybindings = options?.keybindings ?? KeybindingsManager.create()`（**不是**全局 `getKeybindings()`）。
- `canRename = !!options?.renameSession`；`header.setShowRenameHint(options?.showRenameHint ?? canRename)`。
- `SessionsLoader = (onProgress?: SessionListProgress, signal?: AbortSignal) => Promise<SessionInfo[]>`。
- `SessionListProgress = (loaded: number, total: number, partialSessions?: SessionInfo[]) => void`。
- 布局（`buildBaseLayout(content, options?)`，每次进入/退出 rename 都会 `this.clear()` 重建）：
  `Spacer(1)` → `DynamicBorder((s) => theme.fg("accent", s))` → `Spacer(1)` →
  （`options?.showHeader ?? true` 时 `header` + `Spacer(1)`）→ `content` → `Spacer(1)` →
  `DynamicBorder((s) => theme.fg("accent", s))`。rename 面板用 `showHeader: false`。
- 加载：`loadScope(scope)`（**不是** `loadSessions()`），每次 scope 一个独立 `AbortController`
  （`currentLoad` / `allLoad` 两个槽）：
  ```ts
  if (scope === "current" ? this.currentLoad : this.allLoad) return;   // 已有加载则不重复发起
  const controller = new AbortController();
  (scope === "current" ? this.currentLoad = controller : this.allLoad = controller);
  this.header.setScope(scope);
  this.header.setLoading(true);
  this.requestRender();
  const isActive = () => (scope === "current" ? this.currentLoad : this.allLoad) === controller;
  const onProgress: SessionListProgress = (loaded, total, partialSessions) => {
    if (!isActive()) return;
    if (partialSessions) {                       // 增量结果立刻写入缓存并（若 scope 匹配）刷列表
      const sessions = [...partialSessions];
      if (scope === "current") this.currentSessions = sessions; else this.allSessions = sessions;
      if (scope === this.scope) this.sessionList.setSessions(sessions, showCwd);
    }
    if (scope !== this.scope) return;            // 非当前 scope 只更新缓存，不动 header
    this.header.setProgress(loaded, total);
    this.requestRender();
  };
  try {
    const sessions = await (scope === "current" ? this.currentSessionsLoader(onProgress, controller.signal)
                                                 : this.allSessionsLoader(onProgress, controller.signal));
    if (!isActive()) return;                     // 竞态保护（不是 controller.signal.aborted）
    if (scope === "current") { this.currentSessions = sessions; this.currentLoad = null; }
    else { this.allSessions = sessions; this.allLoad = null; }
    if (scope !== this.scope) return;            // 结果只缓存，不落地到 UI
    this.header.setLoading(false);
    this.sessionList.setSessions(sessions, showCwd);
    this.requestRender();
  } catch (err) {
    if (!isActive()) return;
    if (scope === "current") { this.currentLoad = null; this.currentSessions = null; }
    else { this.allLoad = null; this.allSessions = null; }
    if (scope !== this.scope) return;
    const message = err instanceof Error ? err.message : String(err);
    this.header.setLoading(false);
    this.header.setStatusMessage({ type: "error", message: `Failed to load sessions: ${message}` }, 4000);
    this.sessionList.setSessions([], showCwd);
    this.requestRender();
  }
  ```
  **【重点】** 竞态保护靠 `isActive()` **身份比较**（槽里还是不是这个 controller），而不是 `signal.aborted`；
  scope 不匹配时**成功结果仍写缓存**但不刷新 UI（切回来就能秒显）。
- 构造末尾 `void this.loadScope("current")`（立即开始加载）。
- `toggleScope()`：
  ```ts
  this.scope = this.scope === "current" ? "all" : "current";
  const sessions = this.scope === "current" ? this.currentSessions : this.allSessions;
  const loading = (this.scope === "current" ? this.currentLoad : this.allLoad) !== null;
  this.header.setScope(this.scope);
  this.header.setLoading(loading);
  this.sessionList.setSessions(sessions ?? [], this.scope === "all");
  this.requestRender();
  if (sessions === null && !loading) void this.loadScope(this.scope);
  ```
  **【更正】** 切换 scope **不 abort 任何请求**、**不重置搜索词**、**不重置选中项**（选中项由
  `selectionTouched` + `setSessions` 的路径重定位逻辑决定）。两个 scope 的加载互不取消，靠 `isActive()`
  与 `scope !== this.scope` 双保险。
- `toggleSortMode()`：`threaded → recent → relevance → threaded`（`relevance` 的显示标签是 `Fuzzy`）；
  同时 `header.setSortMode` + `sessionList.setSortMode` + `requestRender`。
- `toggleNameFilter()`：`all ↔ named`；同步 header + list。
- `refreshSessionsAfterMutation()`：`cancelLoads()`（abort 两个槽并把缓存置 null）→
  `await this.loadScope(this.scope)`。删除成功与重命名成功后都会调用。
- **rename 模式**：`enterRenameMode(sessionPath, currentName)` / `exitRenameMode()` / `confirmRename(value)`；
  面板（`buildBaseLayout(panel, { showHeader: false })`）：
  `Text(theme.bold("Rename Session"), 1, 0)` → `Spacer(1)` → `renameInput` → `Spacer(1)` →
  `Text(theme.fg("muted", `${keyText("tui.select.confirm")} to save · ${keyText("tui.select.cancel")} to cancel`), 1, 0)`。
  `confirmRename`：`next = value.trim()`；**`if (!next) return;`（空输入直接返回、停留在 rename 模式，
  不退出也不保存）**；无 target 或无 renameSession → `exitRenameMode()`；否则
  `try { await renameSession(target, next); await refreshSessionsAfterMutation(); } finally { exitRenameMode(); }`
  —— 即**失败也会退出** rename 模式（异常向上抛，但 `finally` 保证退出）。
  **注意**：`renameInput` 是**同一个 Input 实例**；进入时 `setValue(currentName ?? "")` + `focused = true`；
  退出时**没有**显式 `focused = false`（`focused` setter 会重新设置：`value && mode === "rename"` 时才置 true，
  但 `exitRenameMode` 里 mode 已为 `"list"`，因此靠 `buildBaseLayout` 后宿主下一次 `focused` 赋值）。
- `onRenameSession` 的守卫：没有 `renameSession` → return；**当前 scope 正在加载 → return**（避免在
  缓存不完整时改名）；然后从对应缓存里 `find(s => s.path === sessionPath)` 取 `name`。
- `handleInput(data)`（**重写**）：`mode === "rename"` 时 —— `tui.select.cancel` → `exitRenameMode()`；
  其它全部交给 `renameInput.handleInput(data)`（**注意 `tui.select.confirm` 由 `renameInput.onSubmit` 触发**）；
  非 rename 模式 → `this.sessionList.handleInput(data)`。
- 选择/取消/退出时会先 `header.setStatusMessage(null)` 清掉状态消息、再 `cancelLoads()`、再回调宿主。
- 删除流程（`sessionList.onDeleteSession`）：`await deleteSessionFile(path)` →
  成功：从两个缓存里 filter 掉该 path → 用当前 scope 的缓存 `setSessions(sessions, showCwd)` →
  `header.setStatusMessage({type:"info", message: method === "trash" ? "Session moved to trash" : "Session deleted"}, 2000)`
  → `await refreshSessionsAfterMutation()`；
  失败：`result.error ?? "Unknown error"` → `` `Failed to delete: ${errorMessage}` ``（error，3000）。最后 `requestRender()`。
- `getSessionList(): SessionList` 暴露列表。
- **【逐字】** `Rename Session`、` to save · `、` to cancel`、`Cannot delete the currently active session`（3000 ms）、
  `Session moved to trash`、`Session deleted`、`Failed to delete: `、`Unknown error`、`Failed to load sessions: `。

**测试**（`session-selector-rename.test.ts` 3 项、`session-selector-path-delete.test.ts` 8 项、
`session-selector-search.test.ts` 14 项）：Ctrl+Backspace 在有查询时作为编辑键；Ctrl+D 进入确认；
scope 切换竞态（旧结果不落地、选中不跳）；符号链接别名视为同一会话；threaded 按子树最新活动排序；
当前会话别名匹配（不提供删除）；重命名模式输入/取消。

### 2.43 `tree-selector.ts`（1428 行）— `/tree` 会话树

> **⚠ 本节多处有误，以 §2.51 为准。**（`TREE_GUTTER_WIDTH` 是视口预留而非缩进、`renderHorizontalViewport`
> 的锚点平移、`flattenTree` 缩进规则、`applyFilter` 七步、`getSearchableText` 真值表）


最大的单文件。Rust 侧建议拆成 `tree_selector/{model,render,filter,help,label}.rs`。

**【精读更正 · 导出面】** 本文件**只导出 2 个符号**：

```ts
export type FilterMode = "default" | "no-tools" | "user-only" | "labeled-only" | "all";
export class TreeSelectorComponent extends Container implements Focusable { ... }
```

- 类型名是 **`FilterMode`**（不是 `TreeFilterMode`）；`TreeSelectorComponent` 构造参数里也写 `FilterMode`。
- 五个常量全部**模块私有**：`TREE_GUTTER_WIDTH = 2`、`MIN_VISIBLE_ANCHOR_CONTENT_WIDTH = 4`、
  `MAX_VISIBLE_ANCHOR_CONTENT_WIDTH = 20`、`MIN_ANCHOR_CONTEXT_WIDTH = 2`、`MAX_ANCHOR_CONTEXT_WIDTH = 12`
  —— **`TREE_GUTTER_WIDTH` 不是「每层缩进列数」**（每层缩进是 **3 列**，见 `FlatNode.indent` 注释
  `each level = 3 chars`）；它是 `renderHorizontalViewport` 里**为 gutter 预留的视口宽度**。
- 模块私有：`interface GutterInfo {position: number; show: boolean}`、`interface FlatNode`、
  `interface HorizontalViewportRow`、`interface ToolCallInfo`、`function renderHorizontalViewport`、
  `class TreeList`、`class SearchLine`、`class TreeHelp`、`const TREE_HELP_ITEMS`、
  `function formatHelpKeys`、`function compactRawKeys`、`class LabelInput`。
- `formatLabelTimestamp` 是 `TreeList` 的**私有方法**（`private formatLabelTimestamp(timestamp: string)`，
  入参是**字符串**，不是 `Date`）；`formatToolCall` / `formatReadLineRange` 均**不在本文件**：
  `formatReadLineRange` 来自 `core/tools/render-utils.ts`（单元 ca-tools-compaction-export）。
- **`TreeSelectorComponent` 公开成员**：`handleInput(data)`（重写）、`invalidate()`（重写）、
  `render(width)`（继承）、`focused` getter/setter（代理给 `TreeList` 的 `focused`? —— 见 §2.43 末）、
  `getTreeList(): TreeList`（**仅供测试/宿主**）。
- `TreeList` 公开成员：`onSelect?`、`onCancel?`、`onCopy?(text: string | undefined)`、
  `onLabelEdit?(entryId, currentLabel)` 四个回调 + `handleInput` / `render` / `invalidate`。

**`renderHorizontalViewport(rows, width)`**（**【更正】** 不是固定把 body 截到 `width-2`；只有选中行
anchor 太靠右时才横向平移到「刚好露出 anchor 后一小段上下文」）：

```ts
// rows: { gutter: string; body: string; anchorCol: number; bodyWidth: number; isSelected: boolean }[]
const viewportWidth = Math.max(0, width - TREE_GUTTER_WIDTH);
const maxBodyWidth = rows.reduce((max, row) => Math.max(max, row.bodyWidth), 0);
const maxHorizontalScroll = Math.max(0, maxBodyWidth - viewportWidth);
const selectedRow = rows.find((row) => row.isSelected);

let horizontalScroll = 0;
if (selectedRow && maxHorizontalScroll > 0) {
  const minVisibleAnchorContentWidth = Math.min(
    MAX_VISIBLE_ANCHOR_CONTENT_WIDTH,                                   // 20
    Math.max(MIN_VISIBLE_ANCHOR_CONTENT_WIDTH, Math.floor(viewportWidth / 3)),   // ≥4
  );
  if (selectedRow.anchorCol > viewportWidth - minVisibleAnchorContentWidth) {
    const anchorContextWidth = Math.min(
      MAX_ANCHOR_CONTEXT_WIDTH,                                         // 12
      Math.max(MIN_ANCHOR_CONTEXT_WIDTH, Math.floor(viewportWidth / 4)), // ≥2
    );
    horizontalScroll = Math.min(maxHorizontalScroll, selectedRow.anchorCol - anchorContextWidth);
  }
}

return rows.map((row) => {
  const line =
    horizontalScroll > 0
      ? `${row.gutter}${sliceByColumn(row.body, horizontalScroll, viewportWidth, true)}\x1b[0m`
      : row.gutter + row.body;
  return truncateToWidth(line, width, "");
});
```

- 水平滚动只作用于 **body**（gutter 固定）；`sliceByColumn(..., strict = true)` 按列切（宽字符安全）；
  滚动开启时末尾补 `\x1b[0m` 防止颜色泄漏到右侧（`horizontalScroll === 0` 时**不加**）。
- 最终每行 `truncateToWidth(line, width, "")` —— 省略号为**空串**。
- **【注意】** `anchorCol` 是「该行 entry 文本起点」的列号，由 `TreeList.render` 计算；
  `bodyWidth` 是该行 body 的**可见宽度**（不是字符串长度）。Rust 侧若用字符索引会漏掉宽字符修正。

**`TreeList`** 状态（**【精读更正】** 真实私有字段；`horizontalScroll` 是 `renderHorizontalViewport` 的**局部变量**，
不是字段；label 直接存在 `flatNode.node.label` / `labelTimestamp` 上，**没有** `labels` Map）：

```ts
flatNodes: FlatNode[]              // flattenTree 的原序结果（保留全部节点，含被过滤的）
filteredNodes: FlatNode[]          // 当前可见节点（过滤 + 折叠后的顺序）
selectedIndex = 0
currentLeafId: string | null
maxVisibleLines: number            // 构造参数（宿主传 Math.max(5, floor(terminalHeight/2))）
filterMode: FilterMode = "default" // 构造时可被 initialFilterMode 覆盖
searchQuery = ""
toolCallMap: Map<toolCallId, {name, arguments}>  // 从 assistant 消息的 toolCall 块收集
multipleRoots = false
showLabelTimestamps = false
activePathIds: Set<string>
visibleParentMap: Map<nodeId, nodeId | null>
visibleChildrenMap: Map<nodeId | null, nodeId[]>
lastSelectedId: string | null
foldedNodes: Set<string>           // 【更正】名字是 foldedNodes，不是 foldedIds
// 公开回调：onSelect?(entryId) / onCancel?() / onCopy?(text|undefined) / onLabelEdit?(entryId, currentLabel)
// 公开方法：handleInput / render / invalidate（空）/ getSearchQuery() / getSelectedNode()
//           / copySelected() / updateNodeLabel(entryId, label, labelTimestamp?)
```

`FlatNode = { node: SessionTreeNode; indent: number; showConnector: boolean; isLast: boolean;
gutters: GutterInfo[]; isVirtualRootChild: boolean }`，`GutterInfo = { position: number; show: boolean }`。
**`indent` 不是 `depth`**：它是「显示层级」（每层 **3 字符**），由分支规则算出（见下）。

**`flattenTree(roots)`**（**【精读更正】** 核心算法，逐条）：

- 缩进规则（源码注释，**必须照抄**）：
  - `indent 0`：保持 0，除非父节点有 >1 个子节点（则 +1）；
  - `indent 1`：子节点**总是** +1 到 2（子树的视觉分组）；
  - `indent 2+`：单子链保持 flat，只有父节点分支时才 +1。
  即 `childIndent = multipleChildren ? indent + 1 : (justBranched && indent > 0) ? indent + 1 : indent`。
- 根数量 > 1 时用**隐式虚拟根**：每个根 `indent = 1`、`justBranched = true`、`showConnector = true`、
  `isVirtualRootChild = true`（渲染时 `connectorDisplayed = showConnector && !isVirtualRootChild` → **不画**）；
  `isLast` 按逆序 push 的索引算（最后一个根 `i === len-1`）。
- 根顺序：`[...roots].sort((a,b) => Number(containsActive(b)) - Number(containsActive(a)))`
  （`Array.sort` 稳定 → active 优先，其余保持原序）。**子节点顺序**同理：
  `[...prioritized(含 active), ...rest]`。
- `containsActive` 用**自建的前序+逆序遍历**模拟后序（不是递归，避免栈溢出）。
- `gutters`：`connectorDisplayed` 时向子节点传
  `[...gutters, { position: connectorPosition, show: !isLast }]`，否则原样传。
  `currentDisplayIndent = multipleRoots ? Math.max(0, indent - 1) : indent`；
  `connectorPosition = Math.max(0, currentDisplayIndent - 1)`。
- 每个节点入 `result` 时就从 assistant 消息里抽取 `toolCall` 块填 `toolCallMap`（`block.type === "toolCall"`
  → `{name, arguments}`，key 为 `tc.id`）。
- 折叠节点（`foldedNodes` 含其 id）的后代**全部跳过** —— 这一步在 `applyFilter` 里做，不在 `flattenTree`。
- **渲染语义**：`prefix` 按 `displayIndent * 3` 个字符逐字符拼；每层第 0 位放 `│`/空格（该层有 gutter 时）
  或连接符 `└`/`├`（该层是 connector 层时），第 1 位放 `⊞`/`⊟`/`─`（connector 层）或空格，第 2 位空格。

**`applyFilter()`**（**【精读更正】** 顺序与条件）：

1. 若当前 `filteredNodes` 非空，把当前选中项 id 记入 `lastSelectedId`（**保留选择**用）。
2. `searchTokens = searchQuery.toLowerCase().split(/\s+/).filter(Boolean)`。
3. 对 `flatNodes` 逐项过滤：
   - **永久隐藏**：`entry.type === "usage"`。
   - **只含 toolCall 的 assistant 消息**（`hasTextContent(content)` 为假，且不是当前叶，且
     `stopReason` 为空/`"stop"`/`"toolUse"`）→ 隐藏。**这条对所有 filterMode 都生效**（先于 switch）。
   - `isSettingsEntry = type ∈ {label, context_edit, custom, model_change, thinking_level_change, session_info}`。
   - switch：
     - `user-only` → **仅** `entry.type === "message" && role === "user"`（**没有**活动路径例外，
       设置类条目自然全被排除）；
     - `no-tools` → `!isSettingsEntry && !(message && role === "toolResult")`
       （**保留** `bashExecution` 消息，原文说「隐藏 bash 类」是错的）；
     - `labeled-only` → `node.label !== undefined`（label 为 `undefined` 即不显示；空串 label 仍显示）；
     - `all` → true；
     - `default` → `!isSettingsEntry`。
   - 搜索：每个 token 都必须出现在 `getSearchableText(node).toLowerCase()` 中（AND）。
4. 折叠：先把 `foldedNodes` 的全部后代 id 收进 `skipSet`（一遍 `flatNodes`，父 id 在 `foldedNodes`
   或已在 `skipSet` 中即传染），再从 `filteredNodes` 中剔除。
5. `recalculateVisualStructure()`：以**可见节点**重建 `visibleParent` / `visibleChildren`（父链向上找
   最近可见祖先），重新跑与 `flattenTree` 相同的 indent/gutter/connector 算法（**单子链不右漂**），
   并据此重算 `multipleRoots = visibleChildren.get(null).length > 1`。
6. 选中恢复：`lastSelectedId` 存在 → `findNearestVisibleIndex(lastSelectedId)`（沿父链向上找第一个可见祖先，
   找不到则取**最后一个**可见项）；否则若 `selectedIndex` 越界 → clamp 到 `len-1`。
7. 最后把选中项的 id 回写 `lastSelectedId`。

**`getSearchableText(node)`**（**【精读更正】** 逐字拼接，`parts.join(" ")`）：

| 条目 | parts |
|---|---|
| 所有节点 | 若 `node.label` 存在 → push `node.label` |
| message | `msg.role`；`"content" in msg && msg.content` → `extractContent(content)`（截 200）；`role === "bashExecution"` → 再 push `command` |
| custom_message | `entry.customType`；content 为 string → 原样，否则 `extractContent` |
| compaction | **字面量 `"compaction"`**（不 push summary） |
| branch_summary | `"branch summary"`、`entry.summary` |
| session_info | `"title"`；`entry.name` 存在再 push |
| model_change | `"model"`、`entry.modelId` |
| thinking_level_change | `"thinking"`、`entry.thinkingLevel` |
| custom | `"custom"`、`entry.customType` |
| context_edit | `"context edit"`、`entry.replacement === null ? "omit" : "replace"`、`entry.targetId` |
| label | `"label"`、`entry.label ?? ""` |

（**没有** toolResult 的 toolName/输出、没有 bash 的 output、没有 thinking 文本、没有 toolCall JSON ——
搜索只覆盖上述字段。）

**折叠清除时机**：`searchQuery` 变化（输入字符、backspace、esc 清空）与**每一次** filter 键（含 toggle 回
default、cycle 前后）都会 `foldedNodes.clear()`；`app.tree.toggleLabelTimestamp` **不会**清。

**`render(width)`**（**【精读更正】** 完整流程）：

- 空（`filteredNodes.length === 0`）：**两行**，都是 `muted`：
  `truncateToWidth(theme.fg("muted", "  No entries found"), width)` 与
  `truncateToWidth(theme.fg("muted", `  (0/0)${getStatusLabels()}`), width)`
  —— **【更正】** 第二行是 `muted`（原文写 `dim` 是错的），且两条都**不带省略号**（`truncateToWidth` 两参）。
- 可视窗口：`startIndex = Math.max(0, Math.min(selectedIndex - Math.floor(maxVisibleLines / 2),
  filteredNodes.length - maxVisibleLines))`，`endIndex = Math.min(startIndex + maxVisibleLines, len)`。
- 每行构建（**逐字**）：
  ```ts
  const cursor = isSelected ? theme.fg("accent", "› ") : "  ";
  const displayIndent = multipleRoots ? Math.max(0, flatNode.indent - 1) : flatNode.indent;
  const connector = flatNode.showConnector && !flatNode.isVirtualRootChild
    ? (flatNode.isLast ? "└─ " : "├─ ") : "";
  const connectorPosition = connector ? displayIndent - 1 : -1;
  // prefix 逐字符（见上），共 displayIndent * 3 个字符
  const isFolded = foldedNodes.has(entry.id);
  const showsFoldInConnector = flatNode.showConnector && !flatNode.isVirtualRootChild;
  const foldMarker = isFolded && !showsFoldInConnector ? theme.fg("accent", "⊞ ") : "";
  const pathMarker = activePathIds.has(entry.id) ? theme.fg("accent", "• ") : "";
  const label = flatNode.node.label ? theme.fg("warning", `[${flatNode.node.label}] `) : "";
  const labelTimestamp = showLabelTimestamps && flatNode.node.label && flatNode.node.labelTimestamp
    ? theme.fg("muted", `${formatLabelTimestamp(flatNode.node.labelTimestamp)} `) : "";
  const content = getEntryDisplayText(flatNode.node, isSelected);
  const prefixPart = theme.fg("dim", prefix) + foldMarker + pathMarker;
  const anchorCol = visibleWidth(prefixPart);
  let gutter = cursor;
  let body = prefixPart + label + labelTimestamp + content;
  if (isSelected) { gutter = theme.bg("selectedBg", gutter); body = theme.bg("selectedBg", body); }
  ```
- **fold indicator 是在 prefix 的 connector 第 1 位里**（不是整个 `─` 替换）：
  - `posInLevel === 0`：`└` / `├`（按 `isLast`）；
  - `posInLevel === 1`：`isFolded ? "⊞" : (isFoldable(entry.id) ? "⊟" : "─")`；
  - `posInLevel === 2`：空格。
  没有连接线的根节点若被折叠，用独立 `foldMarker` = `accent "⊞ "`。
- **`isFoldable(entryId)`**：有可见子节点，且（无可见父 → 根 → 可折叠）或（可见父的可见子节点数 > 1）。
- **pathMarker**：`theme.fg("accent", "• ")`（U+2022）。
- **label**：`theme.fg("warning", `[${label}] `)`（含尾随空格）；
  **labelTimestamp**：`theme.fg("muted", `${formatLabelTimestamp(ts)} `)`（尾随空格），仅在
  `showLabelTimestamps && label && labelTimestamp` 时出现。
- 底部行：`theme.fg("muted", `  (${selectedIndex+1}/${filteredNodes.length})${getStatusLabels()}`)`，
  `truncateToWidth(..., width)`；`getStatusLabels()` 返回
  `" [no-tools]"` / `" [user]"` / `" [labeled]"` / `" [all]"`（默认模式 `""`），再追加 `" [+label time]"`。
- **空过滤保留**：过滤后无条目时仍显示 `  No entries found` 与 `  (0/0)`（测试 "empty filter preservation"），
  且 `getStatusLabels()` 会带上当前过滤模式标签（如 `  (0/0) [user]`）。

**`getEntryDisplayText(node, isSelected)`**（**【精读更正】** 逐字 + 颜色；`normalize = s.replace(/[\n\t]/g, " ").trim()`，
`extractContent = extractFullContent(content).slice(0, 200)`）：

| 条目 | 文本 |
|---|---|
| user 消息 | `accent("user: ")` + `normalize(extractContent(content))` |
| assistant 消息 | `success("assistant: ")` + 下列之一：有文本 → `normalize(extractContent(content))`；`stopReason === "aborted"` → `muted("(aborted)")`；有 `errorMessage` → `error(normalize(errorMessage).slice(0,80))`；否则 `muted("(no content)")` |
| toolResult | 有匹配的 toolCall → `muted(formatToolCall(name, arguments))`；否则 `muted(`[${toolName ?? "tool"}]`)` |
| `role === "bashExecution"` | `dim(`[bash]: ${normalize(command ?? "")}`)` |
| 其它 role | `dim(`[${role}]`)` |
| custom_message | `customMessageLabel(`[${customType}]: `)` + `normalize(content)`；content 为数组时**只拼 `type === "text"` 块**的 `text` |
| compaction | `borderAccent(`[compaction: ${Math.round(entry.tokensBefore / 1000)}k tokens]`)`（**【更正】** 字段名 `tokensBefore`，颜色 `borderAccent`） |
| branch_summary | `warning("[branch summary]: ")` + `normalize(entry.summary)`（**【补充】** 后面接摘要文本） |
| model_change | `dim(`[model: ${entry.modelId}]`)` |
| thinking_level_change | `dim(`[thinking: ${entry.thinkingLevel}]`)` |
| custom | `dim(`[custom: ${entry.customType}]`)` |
| context_edit | `dim(`[context ${entry.replacement === null ? "omit" : "replace"}: ${entry.targetId}]`)` |
| label | `dim(`[label: ${entry.label ?? "(cleared)"}]`)` |
| session_info | 有 name → `dim("[title: ") + dim(name) + dim("]")`；无 name → `dim("[title: ") + italic(dim("empty")) + dim("]")`（**【更正】** 是 `italic(dim(...))` 嵌套，不是「italic dim 两个独立样式」） |
| default | `""` |

- 返回前：`isSelected ? theme.bold(result) : result` —— **bold 包住整段带色文本**（不是只包纯文本）。

**`formatToolCall(name, args)`**（**逐字**；本文件的 `shortenPath` 是**局部闭包**，用的是
`process.env.HOME || process.env.USERPROFILE || ""` —— **与 §2.42 `/resume` 的 `os.homedir()` 不同**）：

| 工具 | 文本（**逐字**） |
|---|---|
| `read` | `` `[read: ${shortenPath(String(args.path \|\| args.file_path \|\| ""))}${formatReadLineRange(args.offset, args.limit)}]` `` |
| `write` | `` `[write: ${shortenPath(String(args.path \|\| args.file_path \|\| ""))}]` `` |
| `edit` | `` `[edit: ${shortenPath(String(args.path \|\| args.file_path \|\| ""))}]` `` |
| `bash` | `` `[bash: ${cmd}${rawCmd.length > 50 ? "..." : ""}]` ``，其中 `rawCmd = String(args.command \|\| "")`、`cmd = rawCmd.replace(/[\n\t]/g, " ").trim().slice(0, 50)`（**【更正】** 有 `.trim()`，且省略号判定用**原始** `rawCmd.length > 50`） |
| `grep` | `` `[grep: /${String(args.pattern \|\| "")}/ in ${shortenPath(String(args.path \|\| "."))}]` `` |
| `find` | `` `[find: ${String(args.pattern \|\| "")} in ${shortenPath(String(args.path \|\| "."))}]` `` |
| `ls` | `` `[ls: ${shortenPath(String(args.path \|\| "."))}]` `` |
| 默认 | `argsStr = JSON.stringify(args).slice(0, 40)`；`` `[${name}: ${argsStr}${JSON.stringify(args).length > 40 ? "..." : ""}]` ``（**两次** `JSON.stringify`，第二次只为测长） |

- **【补充】** `read`/`write`/`edit` 的 path 取值顺序是 `args.path` → `args.file_path` → `""`（空串也照样进
  `shortenPath`）；`grep`/`find`/`ls` 缺省是 `"."`。`shortenPath` 只处理 `HOME`/`USERPROFILE` 前缀 → `~`，
  **不**做反斜杠规范化。
- `formatReadLineRange(offset, limit)` 是 `core/tools/render-utils.ts` 的导出（单元 ca-tools-compaction-export），
  测试表：`[25,13,":25-37"]`、`["25","13",":25-37"]`、`[null,"13",":1-13"]`、`["25",null,":25"]`、
  `[undefined,undefined,""]`、`["abc",13,""]`、`[25,"",""]`、`[25,"Infinity",""]`。
  （即：`offset` 必须是正整数，`limit` 必须是正有限整数；`offset=1` 时只显示 `:limit`? 由上表反推：
  `null + "13"` → `:1-13`；`"25" + null` → `:25`。）

**`formatLabelTimestamp(timestamp: string)`**（**【更正】** 是 `TreeList` 的 **private 方法**，入参是
**ISO 字符串**，不是 `Date`；判定同日用三个 `getFullYear/getMonth/getDate` 比较，不是 `toDateString()`）：

```ts
const date = new Date(timestamp);
const now = new Date();
const hours = date.getHours().toString().padStart(2, "0");
const minutes = date.getMinutes().toString().padStart(2, "0");
const time = `${hours}:${minutes}`;
if (date.getFullYear() === now.getFullYear() && date.getMonth() === now.getMonth() && date.getDate() === now.getDate())
  return time;
const month = date.getMonth() + 1;
const day = date.getDate();
if (date.getFullYear() === now.getFullYear()) return `${month}/${day} ${time}`;
const year = date.getFullYear().toString().slice(-2);
return `${year}/${month}/${day} ${time}`;
```

- 时/分**补零**（`HH:mm`）；月/日**不补零**（`M/D`）；跨年时年份取后两位（`YY/M/D`）。
- **【逐字】** `HH:mm`、`M/D HH:mm`、`YY/M/D HH:mm`。
- label 时间戳来源：`updateNodeLabel(entryId, label, labelTimestamp?)` 在 label 非空时写
  `labelTimestamp ?? new Date().toISOString()`，label 为空时置 `undefined`。

**`handleInput(data)`**（顺序即行为）：

**【精读更正】** 真实实现（`if / else if` 单链，顺序即优先级；**没有** `data === "\n"` 别名，也**没有**
`showLabelInput`）：

```ts
const kb = getKeybindings();
if (kb.matches(keyData, "tui.select.up"))
  selectedIndex = selectedIndex === 0 ? filteredNodes.length - 1 : selectedIndex - 1;   // 环绕
else if (kb.matches(keyData, "tui.select.down"))
  selectedIndex = selectedIndex === filteredNodes.length - 1 ? 0 : selectedIndex + 1;   // 环绕
else if (kb.matches(keyData, "app.tree.foldOrUp")) {
  const id = filteredNodes[selectedIndex]?.node.entry.id;
  if (id && isFoldable(id) && !foldedNodes.has(id)) { foldedNodes.add(id); applyFilter(); }   // 折叠
  else selectedIndex = findBranchSegmentStart("up");                                          // 否则分支段起点
}
else if (kb.matches(keyData, "app.tree.unfoldOrDown")) {
  const id = filteredNodes[selectedIndex]?.node.entry.id;
  if (id && foldedNodes.has(id)) { foldedNodes.delete(id); applyFilter(); }                    // 展开
  else selectedIndex = findBranchSegmentStart("down");
}
else if (kb.matches(keyData, "tui.editor.cursorLeft") || kb.matches(keyData, "tui.select.pageUp"))
  selectedIndex = Math.max(0, selectedIndex - maxVisibleLines);
else if (kb.matches(keyData, "tui.editor.cursorRight") || kb.matches(keyData, "tui.select.pageDown"))
  selectedIndex = Math.min(filteredNodes.length - 1, selectedIndex + maxVisibleLines);
else if (kb.matches(keyData, "tui.select.confirm")) { onSelect(filteredNodes[selectedIndex].node.entry.id) }
else if (kb.matches(keyData, "app.message.copy")) copySelected();
else if (kb.matches(keyData, "tui.select.cancel")) {
  if (searchQuery) { searchQuery = ""; foldedNodes.clear(); applyFilter(); } else onCancel?.();
}
else if (kb.matches(keyData, "app.tree.filter.default"))     { filterMode = "default";      foldedNodes.clear(); applyFilter(); }
else if (kb.matches(keyData, "app.tree.filter.noTools"))     { filterMode = filterMode === "no-tools"     ? "default" : "no-tools";     foldedNodes.clear(); applyFilter(); }
else if (kb.matches(keyData, "app.tree.filter.userOnly"))    { filterMode = filterMode === "user-only"    ? "default" : "user-only";    foldedNodes.clear(); applyFilter(); }
else if (kb.matches(keyData, "app.tree.filter.labeledOnly")) { filterMode = filterMode === "labeled-only" ? "default" : "labeled-only"; foldedNodes.clear(); applyFilter(); }
else if (kb.matches(keyData, "app.tree.filter.all"))         { filterMode = filterMode === "all"         ? "default" : "all";          foldedNodes.clear(); applyFilter(); }
else if (kb.matches(keyData, "app.tree.filter.cycleBackward")) {
  const modes: FilterMode[] = ["default","no-tools","user-only","labeled-only","all"];
  filterMode = modes[(modes.indexOf(filterMode) - 1 + modes.length) % modes.length]; foldedNodes.clear(); applyFilter();
}
else if (kb.matches(keyData, "app.tree.filter.cycleForward")) {
  const modes: FilterMode[] = ["default","no-tools","user-only","labeled-only","all"];
  filterMode = modes[(modes.indexOf(filterMode) + 1) % modes.length]; foldedNodes.clear(); applyFilter();
}
else if (kb.matches(keyData, "tui.editor.deleteCharBackward")) {
  if (searchQuery.length > 0) { searchQuery = searchQuery.slice(0, -1); foldedNodes.clear(); applyFilter(); }
}
else if (kb.matches(keyData, "app.tree.editLabel")) {
  const selected = filteredNodes[selectedIndex];
  if (selected && onLabelEdit) onLabelEdit(selected.node.entry.id, selected.node.label);
}
else if (kb.matches(keyData, "app.tree.toggleLabelTimestamp")) showLabelTimestamps = !showLabelTimestamps;
else {
  const hasControlChars = [...keyData].some((ch) => {
    const code = ch.charCodeAt(0);
    return code < 32 || code === 0x7f || (code >= 0x80 && code <= 0x9f);
  });
  if (!hasControlChars && keyData.length > 0) { searchQuery += keyData; foldedNodes.clear(); applyFilter(); }
}
```

- **【更正】** 四个 `app.tree.filter.*` 直接键是 **toggle ↔ default**，不是「设为该模式」；
  `app.tree.filter.default` 才是无条件设 default。全部清折叠。
- **【更正】** `tui.select.confirm` 之后**没有** `|| data === "\n"` 这种别名（`\n` 由 keybinding 表映射到
  `tui.select.confirm`）。
- **【更正】** page up/down 有**双别名**：`tui.editor.cursorLeft/Right` **或** `tui.select.pageUp/pageDown`。
- **【更正】** `app.tree.editLabel` **不调用** `TreeList` 自己的 `showLabelInput`（`TreeList` 没有该方法）；
  它只 `onLabelEdit(entryId, currentLabel)` 回调给 `TreeSelectorComponent`，由后者挂 `LabelInput`。
- **`copySelected()`**（【更正】签名）：
  ```ts
  const node = this.getSelectedNode();          // filteredNodes[selectedIndex]?.node
  this.onCopy?.(node ? this.getEntryCopyText(node) : undefined);
  ```
  剪贴板写入由**宿主**在 `onCopy` 里做（组件自己不 `await copyToClipboard`）。
  `getEntryCopyText(node)`（**逐字**）：
  - `bashExecution` 消息 → `message.command`；
  - 其它 `"content" in message` → `extractFullContent(content)`；**若为空且 role 是 assistant** →
    退回 `message.errorMessage`；
  - `custom_message` → `extractFullContent(entry.content)`；
  - `compaction` / `branch_summary` → `entry.summary`；
  - 其它类型 → `undefined`；
  - 返回前 `text?.trim() ? text : undefined`（**全空白视为无内容**）。
- **`findBranchSegmentStart(direction)`**（**【更正】** 实际语义）：
  - `"down"`：从当前节点沿**可见子节点**一直下降，遇 `children.length === 0` → 停在当前；
    遇 `children.length > 1` → 返回**第一个子节点**的索引；`children.length === 1` → 继续下降。
  - `"up"`：沿 `visibleParentMap` 向上走；每到一个父节点，若其可见子节点数 > 1 且**当前节点索引 <
    selectedIndex**（即往上找）→ 返回该当前节点的索引；父为 `null` → 返回根索引。
  - **【注意】** up 分支的第一个参数不是 `selectedIndex`，是起点 id；返回的是 `filteredNodes` 索引。
- **测试**（`interactive-mode-tree-navigation.test.ts` 4 项 + `tree-selector-*.test.ts`）：model_change/
  thinking_level_change 的最近可见祖先；隐藏的 context edits + labels；过滤切换后的遍历；语义化帮助行；
  copy ctrl+x；label 时间戳切换；空过滤保留（单个与多个）；ctrl+arrow 折叠/展开/别名；嵌套折叠保持；
  非活动分支；多根；过滤掉的中间节点；搜索重置折叠；过滤切换重置折叠。**完整清单见 §7.1。**

**`SearchLine implements Component`**（**【精读补充】** 构造需注入 `TreeList`；`handleInput` 为空实现）：

```ts
constructor(treeList: TreeList);
render(width) => {
  const query = this.treeList.getSearchQuery();
  if (query) return [truncateToWidth(`  ${theme.fg("muted", "Type to search:")} ${theme.fg("accent", query)}`, width)];
  return [truncateToWidth(`  ${theme.fg("muted", "Type to search:")}`, width)];
}
```
截断到 width（**两参 `truncateToWidth`，省略号为空**）。**【逐字】** `Type to search:`、两前导空格、
query 前的**一个空格**。

**`TreeHelp implements Component`**（**【精读更正】**）：

- `TREE_HELP_ITEMS`（**逐字**，按显示顺序；`labelFirst` 决定 key 在 label 前还是后）：

| 标签 | 键位来源 | labelFirst |
|---|---|---|
| `move` | `tui.select.up` / `tui.select.down` | false → `up/down move` |
| `page` | `tui.editor.cursorLeft` / `tui.editor.cursorRight` | false |
| `branch` | `app.tree.foldOrUp` / `app.tree.unfoldOrDown` | false |
| `copy` | `app.message.copy` | false |
| `label` | `app.tree.editLabel` | false |
| `label time` | `app.tree.toggleLabelTimestamp` | false |
| `filters` | `app.tree.filter.default` / `noTools` / `userOnly` / `labeledOnly` / `all`（**5 个 filter 键**） | **true** → `filters <keys>` |
| `cycle` | `app.tree.filter.cycleForward` / `cycleBackward` | **true** |

（**【更正】** `filters` 的键不是 `tui.input.tab`；渲染时 `labelFirst ? `${label} ${text}` : `${text} ${label}``，
`text` 为 `formatHelpKeys(keys)`，为空则**只显示 label**。）

- `formatHelpKeys(keybindings)`：对每个 keybinding `getKeybindings().getKeys(keybinding)[0]`（**第一个** key，
  undefined 跳过）；全部为空 → `""`；否则 `formatKeyText(compactRawKeys(keys))` 后按**词边界**替换：
  ```ts
  .replace(/\bpageUp\b/g, "pgup").replace(/\bpageDown\b/g, "pgdn")
  .replace(/\bup\b/g, "↑").replace(/\bdown\b/g, "↓")
  .replace(/\bleft\b/g, "←").replace(/\bright\b/g, "→")
  ```
- `compactRawKeys(keys)`：`keys.length === 1` → 直接返回该键；否则对每键按 **`lastIndexOf("+")`** 切
  `{prefix, suffix}`（无 `+` → `prefix = ""`）；若 `prefix = parts[0].prefix` 非空且**所有** part 的 prefix
  都等于它 → `` `${prefix}${suffixes.join("/")}` ``，否则 `keys.join("/")`。
  （**【更正】** `prefix` 已**含**末尾 `+`，所以模板是 `prefix + suffixes`，不是 `prefix+"+"+suffixes`。）
- 渲染（**【更正】** 不是简单 `wrapTextWithAnsi(line, width-2)`+缩进）：`availableWidth = Math.max(1, width)`，
  `indent = "  "`，`separator = " · "`；逐个 item 拼行：
  ```ts
  const candidate = currentLine
    ? `${currentLine}${separator}${item}`
    : visibleWidth(`${indent}${item}`) <= availableWidth ? `${indent}${item}` : item;
  if (!currentLine || visibleWidth(candidate) <= availableWidth) { currentLine = candidate; continue; }
  lines.push(...wrapTextWithAnsi(currentLine.trimEnd(), availableWidth));
  currentLine = visibleWidth(`${indent}${item}`) <= availableWidth ? `${indent}${item}` : item;
  // 循环结束后再 flush currentLine
  return lines.map((line) => theme.fg("muted", line));
  ```
  即：**首行**带 2 空格缩进（放不下时省略缩进），换行由 `wrapTextWithAnsi(..., availableWidth)` 做（宽度
  **不是** `width-2`），整行再统一 `muted`。

**`LabelInput implements Component, Focusable`**（**【精读更正】** 构造是 `(entryId, currentLabel)`，
`onSubmit` 带 **entryId**）：

```ts
constructor(entryId: string, currentLabel: string | undefined);   // currentLabel 非空时 input.setValue(currentLabel)
public onSubmit?: (entryId: string, label: string | undefined) => void;
public onCancel?: () => void;
focused getter/setter                              // 代理给内部 Input

render(width) => {
  const indent = "  ";
  const availableWidth = width - indent.length;
  return [
    truncateToWidth(`${indent}${theme.fg("muted", "Label (empty to remove):")}`, width),
    ...this.input.render(availableWidth).map((line) => truncateToWidth(`${indent}${line}`, width)),
    truncateToWidth(`${indent}${keyHint("tui.select.confirm", "save")}  ${keyHint("tui.select.cancel", "cancel")}`, width),
  ];
}

handleInput(keyData) {
  if (matches "tui.select.confirm") {
    const value = this.input.getValue().trim();
    this.onSubmit?.(this.entryId, value || undefined);     // 空串 → undefined（删除 label）
  } else if (matches "tui.select.cancel") this.onCancel?.();
  else this.input.handleInput(keyData);
}
```

- **【逐字】** `Label (empty to remove):`、`save`、`cancel`；两前导空格；两个 `keyHint` 之间是**两个空格**。
- **【更正】** 输入框每行都 `truncateToWidth` 到 `width`（不是直接把 `input.render(width-2)` 拼上）。

**`TreeSelectorComponent extends Container implements Focusable`**（**【精读更正】**）：

```ts
constructor(
  tree: SessionTreeNode[], currentLeafId: string | null, terminalHeight: number,
  onSelect: (entryId: string) => void, onCancel: () => void,
  onLabelChange?: (entryId: string, label: string | undefined) => void,
  initialSelectedId?: string, initialFilterMode?: FilterMode,
);
public onCopy?: (text: string | undefined) => void;     // 【原文缺失】对外暴露的剪贴板回调
handleInput(keyData): void;   // labelInput 存在 → 转给它；否则 → treeList.handleInput
getTreeList(): TreeList;
focused getter/setter;        // setter 只在 labelInput 存在时下发给它（不下发给 treeList！）
```

- `maxVisibleLines = Math.max(5, Math.floor(terminalHeight / 2))`（构造时算，传给 `TreeList`）。
- 构造顺序（**逐字**）：
  `treeList = new TreeList(tree, currentLeafId, maxVisibleLines, initialSelectedId, initialFilterMode)`；
  `treeList.onSelect = onSelect`；`treeList.onCancel = onCancel`；
  `treeList.onCopy = (text) => this.onCopy?.(text)`；
  `treeList.onLabelEdit = (entryId, currentLabel) => this.showLabelInput(entryId, currentLabel)`。
- 布局：
  1. `Spacer(1)`
  2. `DynamicBorder()`
  3. `Text(theme.bold("  Session Tree"), 1, 0)` —— **label 内含两个前导空格**，`paddingX = 1`（总 3 列）。
  4. `TreeHelp`
  5. `SearchLine(treeList)`
  6. `DynamicBorder()`
  7. `Spacer(1)`
  8. `treeContainer`（内含 `treeList`）
  9. `labelInputContainer`（空）
  10. `Spacer(1)`
  11. `DynamicBorder()`
- 空树（`tree.length === 0`）→ 构造后 `setTimeout(() => onCancel(), 100)`（**未 unref**，Rust 侧需自行决定）。
- `showLabelInput(entryId, currentLabel)`（**【更正】** 参数是 id + label 字符串，不是 entry 对象）：
  ```ts
  this.labelInput = new LabelInput(entryId, currentLabel);
  this.labelInput.onSubmit = (id, label) => {
    this.treeList.updateNodeLabel(id, label);       // 先改内存里的 label + labelTimestamp(now)
    this.onLabelChangeCallback?.(id, label);        // 再通知宿主持久化
    this.hideLabelInput();
  };
  this.labelInput.onCancel = () => this.hideLabelInput();
  this.labelInput.focused = this._focused;          // 继承当前焦点
  this.treeContainer.clear();                       // 【关键】树列表被临时摘掉
  this.labelInputContainer.clear();
  this.labelInputContainer.addChild(this.labelInput);
  ```
- `hideLabelInput()`：`labelInput = null`；清空两个容器后把 `treeList` 重新加回 `treeContainer`。
- `handleInput`：`labelInput` 存在时**所有键**都给它（含 `tui.select.cancel` → `hideLabelInput`）；
  否则给 `treeList`。
- **【逐字】** `  Session Tree`。

### 2.44 `armin.ts`（384 行）— ASCII 像素画动画

```ts
export const ARMIN_WIDTH = 31;
export const ARMIN_HEIGHT = 36;
export function isArminPixel(x: number, y: number): boolean;
export class ArminComponent implements Component {
  constructor(tui: TUI);
  invalidate(): void;
  render(width: number): string[];
  dispose(): void;
}
```

- `BITS`：**144 字节**的 XBM 位图（`36 行 × ceil(31/8)=4 字节`），LSB-first，**1 = 背景，0 = 前景**。
  `isArminPixel(x, y)`（**【补充】** 有 `y >= HEIGHT` 守卫，但**没有** `x` 守卫——`x >= 31` 时会读到下一行的位，
  照抄即可）：
  ```ts
  if (y >= HEIGHT) return false;
  const byteIndex = y * BYTES_PER_ROW + Math.floor(x / 8);
  return ((BITS[byteIndex] >> (x % 8)) & 1) === 0;
  ```
  （`BYTES_PER_ROW = Math.ceil(ARMIN_WIDTH / 8) = 4`。）
- `DISPLAY_HEIGHT = Math.ceil(HEIGHT / 2)` = **18**（半块渲染）。
- `WIDTH`/`HEIGHT` 是 `ARMIN_WIDTH`/`ARMIN_HEIGHT` 的模块内别名（私有）。
- `EFFECTS = ["typewriter", "scanline", "rain", "fade", "crt", "glitch", "dissolve"]`；
  构造时 `Math.random()` 选一个。
- 帧率：`glitch` → 60 fps（`setInterval(1000/60)`），其它 → 30 fps（`setInterval(1000/30)`）。
  定时器回调：`done = tickEffect(); updateDisplay(); ui.requestRender(); if (done) stopAnimation();`
  —— **【补充】** 每帧无论是否有变化都 `requestRender()`，且 `done` 后自停。
- `updateDisplay()`：`gridVersion++`（**每帧都 +1**，不只是内容变化时）。
- `buildFinalGrid()`：36×31 的最终像素网格（`getChar` 半块打包）。
- 每帧 `tickEffect()`（**【精读更正】** 逐字；返回 `true` = 动画结束，随后 `stopAnimation()`）：
  - `typewriter`：`effectState = { pos: 0 }`；每帧 **3 个 cell**（不是 3 个像素；cell = 半块字符），
    `row = floor(pos / WIDTH)`、`x = pos % WIDTH`，**行优先**；`row >= DISPLAY_HEIGHT` 时 `return true`；
    每写一个 cell `pos++`。
  - `scanline`：`{ row: 0 }`；每帧复制**整行** `finalGrid[row]` 后 `row++`；`row >= DISPLAY_HEIGHT` → true（下一帧）。
  - `rain`：`{ drops: [{ y, settled }] × WIDTH }`，初始
    `y = -Math.floor(Math.random() * DISPLAY_HEIGHT * 2)`（**每列独立**）、`settled = 0`。
    每帧**先清空** `currentGrid = createEmptyGrid()`，然后逐列：
    1. 从底部往上画 `settled` 个像素（`for row = DISPLAY_HEIGHT-1; row >= DISPLAY_HEIGHT - settled; row--`）；
    2. `settled >= DISPLAY_HEIGHT` 的列视为完成（继续）；
    3. `targetRow` = 在 `[0, DISPLAY_HEIGHT-1-settled]` 内**最低的非空格**（`finalGrid[row][x] !== " "`）；
    4. `drop.y++`；若 `0 <= y < DISPLAY_HEIGHT`：
       - `targetRow >= 0 && y >= targetRow` → **settle**：`settled = DISPLAY_HEIGHT - targetRow`；
         `y = -Math.floor(Math.random() * 5) - 1`；
       - 否则 → 该 cell 画 `"▓"`。
    全部列 `settled >= DISPLAY_HEIGHT` → true。
  - `fade`：初始把 `DISPLAY_HEIGHT × WIDTH` 个 cell 做 **Fisher-Yates 洗牌**（`j = floor(random*(i+1))`，
    从后往前）；每帧取前 **15** 个未显示位置填充 `finalGrid`，`idx += 15`；`idx >= 总数` → true。
  - `crt`：`{ expansion: 0 }`；`midRow = floor(DISPLAY_HEIGHT / 2)` = 9；每帧**清空**后画
    `[max(0, midRow-expansion), min(DISPLAY_HEIGHT-1, midRow+expansion)]` 的行；`expansion++`；
    返回 `expansion > DISPLAY_HEIGHT`（即 19 帧后停）。
  - `glitch`：`{ phase: 0, glitchFrames: 8 }`；`phase < 8` 时**逐行**生成：
    `offset = floor(random*7) - 3`（**每行重新取**）；
    若 `random < 0.3` → `slice(offset).concat(slice(0, offset)).slice(0, WIDTH)`（负 offset 即从尾部取，
    行为照抄）；否则若 `random < 0.2` → 整行替换为 `finalGrid[floor(random*DISPLAY_HEIGHT)]` 的拷贝；
    否则原行。`phase++`；`phase >= 8` 的下一帧：`currentGrid = finalGrid 深拷贝`，`return true`。
    —— **【注意】** 每行最多消耗 **2 次** `Math.random()`（水平偏移分支命中时不再取第二个），幅度顺序照抄。
  - `dissolve`：构造时 `currentGrid` 先填满噪声字符（`[" ","░","▒","▓","█","▀","▄"]` 随机）并做同样的
    Fisher-Yates 洗牌；每帧取 **20** 个位置填 `finalGrid`，`idx += 20`；`idx >= 总数` → true。
    （**【更正】** 噪声只出现在**初始化**，不是每帧重刷。）
- `render(width)`（**【精读更正】逐字实现；原 §2.44 的 `Math.max(1, width-1)` 是错的**）：
  ```ts
  if (width === this.cachedWidth && this.cachedVersion === this.gridVersion) return this.cachedLines;
  const padding = 1;
  const availableWidth = width - padding;                 // 可为负/0，无 Math.max
  this.cachedLines = this.currentGrid.map((row) => {
    const clipped = row.slice(0, availableWidth).join(""); // 负值 → slice(0,负数) = 去掉尾部，行为照抄
    const padRight = Math.max(0, width - padding - clipped.length);
    return ` ${theme.fg("accent", clipped)}${" ".repeat(padRight)}`;
  });
  const message = "ARMIN SAYS HI";
  const msgPadRight = Math.max(0, width - padding - message.length);
  this.cachedLines.push(` ${theme.fg("accent", message)}${" ".repeat(msgPadRight)}`);
  this.cachedWidth = width; this.cachedVersion = this.gridVersion;
  return this.cachedLines;
  ```
  - 缓存键 = `(cachedWidth, cachedVersion)`，`cachedVersion` 对齐 `gridVersion`；命中直接返回同一数组引用
    （**调用方不可修改返回值**）。
  - **【逐字】** `ARMIN SAYS HI`（13 字符）。
  - `ARMIN SAYS HI` 行**不截断**：`width < 14` 时该行溢出（已知例外）。
  - `padRight` 是**纯空格**（不套 `accent`），因此行尾空格无色。
- 半块字符：`getChar(x, row)` → `upper&&lower ? "█" : upper ? "▀" : lower ? "▄" : " "`
  （`upper = isArminPixel(x, row*2)`、`lower = isArminPixel(x, row*2+1)`）。
- **`invalidate()`**：**只** `this.cachedWidth = 0`（**【补充】** 不动 `cachedVersion`/`cachedLines`，
  因此下一帧必重建）。
- **`dispose()`**：`stopAnimation()`（`clearInterval` + `interval = null`）。**幂等**。
- **构造即启动**：`initEffect()` + `startAnimation()`；动画播完（`tickEffect` 返回 true）后自停，
  但组件仍可 `render`（显示最终网格）。
- **注意**：`render` 的裁剪用 `availableWidth = width - 1`（**没有** `Math.max(1, ...)`，见 §2.60.6），
  再在左侧加 1 空格，因此 `width >= 31` 时总宽 = width。
  **`ARMIN SAYS HI` 不截断**（§2.44 原写法错误）：`width < 14` 时该行溢出，必须照抄。

### 2.45 `easter-egg-3d.ts`（1292 行）— 3D 半块/盲文旋转动画

**这是整个单元最难逐位复刻的部分。** Rust 侧建议单独 crate 模块 + 与 TS 的黄金输出比对测试。

**【精读更正 · 导出面与签名】**（与 §2.60.7 一致；此处旧接口作废）：

```ts
export type EasterEgg3d = { kind: "kiana-logo"; column: number; row: number } | { kind: "armin" };
export async function playEasterEgg3d(tui: TUI, screen: readonly string[], egg: EasterEgg3d): Promise<void>;
export class EasterEgg3dAnimation implements Component {
  constructor(tui: TUI, screen: readonly string[], model: Model,
              colors: { foreground: Rgb; background: Rgb }, onDone: () => void);
  close(): void;
  handleInput(data: string): void;
  handleMouse(event: TuiMouseEvent): TuiMouseEventResult;
  invalidate(): void;
  render(width: number): string[];
  // 【没有 dispose()】
}
```

- **没有 `EasterEgg3dContext`**；`tui` 是 `TUI`（不是 `TuiAltScreen` —— alt-screen 判定在 lazy 层）。
- **`EasterEgg3dAnimation` 没有 `dispose()`**（§2.45 旧文写的 `dispose()` 不存在）：清理靠在
  `render` 期间 `lastRender` 超过 1000 ms 或退出进度 ≥ 1 时 `finish()`（清 interval + `onDone()`）。
- `EasterEgg3dAnimation` 私有字段（**权威**）：
  ```ts
  tui; screen: readonly string[]; model: Model; foreground: Rgb; background: Rgb; onDone: () => void;
  startTime = performance.now(); lastRender = performance.now();
  timer: ReturnType<typeof setInterval> | undefined;       // FRAME_MS，unref?.()
  exit?: { start; time; yaw; targetYaw; offsets: [number,number,number][] };
  shuffle?: { cycle: number; steps: Cell3[][] };           // 按 cycle 缓存的 puzzle 帧
  screenWidth = -1; screenHeight = -1;
  stars: Array<Star | undefined>; cells: ScreenCell[][];
  ansiCache: Map<number, string>; raster: BlockRaster;
  ```

**常量（逐字）**：

```ts
DEPTH = 0.7;
FLY_START = 0.1;
FLY_DURATION = 1.3;
START_SCALE = 0.1;
PUZZLE_START = FLY_START + FLY_DURATION + 3;   // 4.4
PUZZLE_STEP = 0.3;
PUZZLE_STEPS = 12;
PUZZLE_RETURN = 1;
PUZZLE_HOLD = 0.6;
PUZZLE_CYCLE = PUZZLE_STEPS * PUZZLE_STEP + PUZZLE_RETURN + PUZZLE_HOLD;   // 3.6 + 1 + 0.6 = 5.2
FRAME_MS = 1000 / 30;
DUST_DURATION = 0.35;
WAVE_SPREAD = 0.55;
WAVE_JITTER = 0.12;
BACKGROUND_FADE = 0.5;
EXIT_DURATION = 1.1;
STARS_START = 2.5;
STARS_FADE = 2;
STAR_DENSITY = 0.018;
LOGO_COLOR_STEP = 5;
LIGHT_HALO_STRENGTH = 0.3;
HALO_RADIUS_X = 6;
HALO_RADIUS_Y = 3;
HALO_LEVELS = 32;
STAR_ALPHA_LEVELS = 8;
STAR_FLICKER_LEVELS = 3;
HINT_FADE = 0.5;
SPIN_RAMP = 1.4;
SPIN_SPEED = (2 * Math.KIANA) / PUZZLE_CYCLE;
SPIN_LINGER = 0.6;
```

**盲文常量**：

```ts
const DOT_BITS = [0x01, 0x08, 0x02, 0x10, 0x04, 0x20, 0x40, 0x80];   // 2x4 点阵 → 位
const BRAILLE = Array.from({length: 256}, (_, i) => String.fromCharCode(0x2800 + i));
```

（点 (col,row) 的位：`DOT_BITS[row*2+col]`，col∈0..1，row∈0..3。）

**颜色**：

- `CORAL`、`BLUE`、`YELLOW` 的 RGB 与 `kiana-logo.ts` 相同（228,138,122 / 79,142,179 / 234,182,93）。
- 光照：`LIGHT` 方向向量、`HALF_VECTOR`（视线与光线的半向量）**归一化**后缓存；
  高光 = `Math.pow(dot(normal, HALF_VECTOR), 24) * 0.6 * 255`。
- 模型：
  - `kianaLogoModel`：从 `KIANA_LOGO_PIXELS = ["ccc.","b.c.","bb.y","b..y"]` 构造体素；`cameraDistance = 10`、
    `widthShare = 0.35`、`puzzleMoves = random < 0.4 ? 2 : 1`。
  - `arminModel`：`cameraDistance = 80`、`widthShare = 0.45`、
    `puzzleMoves = Math.round(blockCount * (0.03 + Math.random() * 0.04))`。
- `shuffleSteps(model, cycle)`：**确定性**伪随机 —— `let random = cycle * 7919 + 1`，
  每步 `random = hash(random)`，用 `random` 决定转动的层与方向。`hash` 用 `Math.imul`：
  ```ts
  function hash(value: number): number {          // ← 权威实现（§2.45 旧写法是错的，以此为准）
    let x = Math.imul(value ^ 0x9e3779b9, 0x85ebca6b);
    x ^= x >>> 13;
    x = Math.imul(x, 0xc2b2ae35);
    x ^= x >>> 16;
    return (x >>> 0) / 0x100000000;               // 0..1 浮点
  }
  ```
  Rust 必须用 `u32` 的 `wrapping_mul` + `>>` 复刻（`>>>` 是无符号右移）；`value` 为整数值，用
  `value as i64 as i32 as u32` 做 JS 的 ToInt32。**返回 `f64`**（0..1），不是 u32。

  **黄金值（Node 22 实测，用于 Rust 单测，精度 9 位）**：

  | value | hash(value) |
  |---|---|
  | 0 | 0.114787745 |
  | 1 | 0.246789969 |
  | 2 | 0.023305689 |
  | 7919 | 0.094376345 |
  | 7920 | 0.837748315 |
  | 100 | 0.268851383 |
  | 12345 | 0.110088808 |
  | 0x51ed | 0.831175008 |
  | 0x9e37 | 0.573397580 |


**`parseScreen`**（把当前屏幕的 ANSI 行解析成带颜色的单元格网格）：

- 解析 SGR：`38;2;r;g;b`、`48;2;r;g;b`（`;` 与 `:` 两种分隔）、`38;5;n`、`48;5;n`、`30-37`、`90-97`、
  `40-47`、`100-107`、`39`、`49`、`0`、`1`、`2`、`22`、`7`、`27`。
- 跳过 OSC / APC / DCS 字符串（直到 BEL 或 ST `\x1b\\`）。
- 用 `Intl.Segmenter` 切 grapheme；宽字符（东亚宽）占两列，第二列 push 一个 **width 0 的伴随单元**。
- 输出 `{ char, fg, bg, dim, bold, inverse, width }` 网格。

**`BlockRaster`**：

- 每帧的光栅缓冲：`dots`（`Uint8` 累积位）、`depth`（`Float32Array`）、颜色缓冲。
- `dirty rect` 清除（只清上一帧画过的区域）。
- 每个点做一次**射线投射**：对模型每个可见面做求交（AABB 体素）；深度测试；写颜色。
- `visibleFaces`：背面剔除 + 被完全遮挡的面剔除。
- 光照：`diffuse`（`max(0, dot(normal, LIGHT))`）+ `rim` + `specular`（见上）。
- 点采样：对每个终端**点**（2×4 per braille cell）求射线，而不是每个 cell。
- 透明/背景特殊处理：`light` 背景时颜色混白。

**`renderHalo`**：可分离的 tent 滤波器（`HALO_RADIUS_X/Y`），把发光点扩散，量化到 `HALO_LEVELS` 级。

**`playEasterEgg3d(tui, screen, egg)`**（**【精读更正】** 无 `Promise.race` 外部 timeout，只有
`queryTerminalColors({ timeoutMs: 100 })` 自身超时；颜色兜底来自 **theme** 而非硬编码黑/白）：

```ts
if (playing) return;                              // 模块级单例
playing = true;
const reported = await tui.queryTerminalColors({ timeoutMs: 100 });
const dark = theme.appearance === "dark";
const toRgbTuple = (rgb, fallback) => rgb ? [rgb.r, rgb.g, rgb.b] : fallback;
const colors = {
  foreground: toRgbTuple(reported.foreground, toRgb(theme.colors.text)),
  background: toRgbTuple(reported.background, dark ? [0, 0, 0] : [255, 255, 255]),
};
if (tui.hasOverlay()) { playing = false; return; }       // 查询后再判一次，重复播放直接退出
const model = egg.kind === "armin"
  ? arminModel(toRgb(theme.colors.accent))
  : kianaLogoModel({ column: egg.column, row: egg.row });
const animation = new EasterEgg3dAnimation(tui, screen, model, colors, () => { playing = false; overlay.hide(); });
const overlay = tui.showOverlay(animation, { anchor: "top-left", width: "100%", maxHeight: "100%" });
```

- overlay 选项是 `{ anchor: "top-left", width: "100%", maxHeight: "100%" }`（**不是** `x/y/height`）。
- `playing` 在动画 `finish()`（退出进度 ≥ 1 / 1 秒无渲染 / `close()` 二次调用）时通过 `onDone` 复位。
- **【注意】** `overlay` 在 `animation` 的 `onDone` 闭包里被引用（TDZ 边界）：Rust 侧用
  `Rc<RefCell<Option<OverlayHandle>>>` 之类即可。

**`EasterEgg3dAnimation`**（**【精读更正】** 逐方法权威实现）：

- **`elapsed()`** = `(performance.now() - startTime) / 1000`。
- **`exitProgress()`** = `exit ? clamp01((now - exit.start) / 1000 / EXIT_DURATION) : 0`
  （**【更正】** 没有 `closeRequested`/`exitStartTime` 字段，只有一个 `exit` 对象）。
- **`finish()`**：`if (!timer) return; clearInterval(timer); timer = undefined; onDone();`
  （**幂等**：重复调用不会再触发 `onDone`）。
- **`close()`**：`if (this.exit) { this.finish(); return; }` —— 第二次调用**立即结束**（跳过退出动画）。
  首次：`exit = { start: performance.now(), time: elapsed(), yaw: spinAngle(time),
  targetYaw: Math.ceil(yaw / (2π)) * 2π, offsets: blockOffsets(time) }`。
- **`render(width)`**（**签名只有 width**，height 从 `tui.terminal.rows` 取）：
  ```ts
  const height = Math.max(1, this.tui.terminal.rows);
  this.lastRender = performance.now();
  if (width !== screenWidth || height !== screenHeight) {
    screenWidth = width; screenHeight = height;
    cells = prepareCells(width, foreground);       // 只在尺寸变化时重建
    stars = prepareStars(width, height);
  }
  ```
  分支：
  ```ts
  if (this.exit) {
    const progress = this.exitProgress();
    const landing = clamp01(progress / 0.8);
    const settle = 1 - smooth(landing);
    dissolveTime = Math.min(this.exit.time, DISSOLVE_END) * (1 - progress);
    pose = this.pose(width, height, this.flyProgress(this.exit.time) * settle, this.exit.time);
    pose.yaw = this.exit.yaw + (this.exit.targetYaw - this.exit.yaw) * easeOut(landing);
    pose.pitch *= settle; pose.roll *= settle;
    hintAlpha = this.hintAlpha(this.exit.time) * (1 - smooth(progress / 0.2));
    starAlpha = this.starAlpha(this.exit.time) * (1 - smooth(progress / 0.3));
    const gather = 1 - smooth(progress / 0.5);
    offsets = this.exit.offsets.map(([x, y, z]) => [x * gather, y * gather, z * gather]);
  } else {
    const time = this.elapsed();
    dissolveTime = time;
    pose = this.pose(width, height, this.flyProgress(time), time);
    hintAlpha = this.hintAlpha(time);
    starAlpha = this.starAlpha(time);
    offsets = this.blockOffsets(time);
  }
  ```
  然后 `boxes`（每块 `min = [home0 - columns/2 + dx, home1 - rows/2 + dy, dz - DEPTH/2]`、
  `max = [x+1, y+1, dz + DEPTH/2]`）→ `raster.render(width, height, pose, boxes, background,
  isLight(background) ? LIGHT_HALO_STRENGTH : 0)` → halo 回调 → 逐行 SGR 拼装。
- **`pose(width, height, progress, time)`**（**逐字**，与 §2.45 旧文的「缩放 START_SCALE→1」完全不同）：
  ```ts
  const reach = radius * (cameraDistance / (cameraDistance - radius));
  const startScale = origin ? (2 * (cameraDistance - DEPTH / 2)) / cameraDistance : START_SCALE;
  const endScale = Math.max(startScale,
    Math.min(width * 2 * widthShare, (height * 4 - 8) * 0.48) / reach);
  const endX = width;
  const endY = height * 2 - 2;
  const startX = origin ? origin.column * 2 + columns : endX;
  const startY = origin ? origin.row * 4 + rows : endY;
  return {
    centerX: startX + (endX - startX) * progress,
    centerY: startY + (endY - startY) * progress,
    scale: startScale * (endScale / startScale) ** progress,      // 几何插值
    yaw: spinAngle(time),
    pitch: 0.3 * Math.sin(spinPhase(time)) * progress,
    roll: 0.06 * Math.sin(2 * spinPhase(time)) * progress,
  };
  ```
  坐标是 **dot 空间**（1 cell = 2 列 × 4 行 dot），所以 `endX = width`、`endY = height*2 - 2`。
- **`flyProgress(time)`** = `smooth((time - FLY_START) / FLY_DURATION)`（**【更正】** 用 `smooth`，不是 clamp）。
- **`starAlpha(time)`** = `smooth((time - STARS_START) / STARS_FADE)`。
- **`hintAlpha(time)`** = `smooth((time - FLY_START - FLY_DURATION) / HINT_FADE)`（**没有淡出**；退出时才乘 `1-smooth`）。
- **`blockOffsets(time)`**（**【原文完全缺失】** puzzle 位移）：
  ```ts
  if (time < PUZZLE_START) return blocks.map(() => [0,0,0]);
  const cycle = Math.floor((time - PUZZLE_START) / PUZZLE_CYCLE);
  const local = time - PUZZLE_START - cycle * PUZZLE_CYCLE;
  if (this.shuffle?.cycle !== cycle) this.shuffle = { cycle, steps: shuffleSteps(this.model, cycle) };
  const steps = this.shuffle.steps;
  const offset = (position, index) => position - blocks[index].home;    // 逐分量
  const shuffleEnd = PUZZLE_STEP * PUZZLE_STEPS;                        // 3.6
  if (local < shuffleEnd) {                    // 每 0.3s 一步，smooth 插值到下一步
    const step = Math.floor(local / PUZZLE_STEP);
    const progress = smooth((local - step * PUZZLE_STEP) / PUZZLE_STEP);
    return steps[step].map((from, i) => lerpVec(offset(from,i), offset(steps[step+1][i], i), progress));
  }
  if (local < shuffleEnd + PUZZLE_RETURN) {    // 所有块同时飞回，前后交替绕深度弧线
    const u = (local - shuffleEnd) / PUZZLE_RETURN;
    const remaining = 1 - smooth(u);
    const arc = Math.sin(Math.KIANA * clamp01(u)) * 0.8;
    return steps[PUZZLE_STEPS].map((position, i) => {
      const [x,y,z] = offset(position, i);
      return [x*remaining, y*remaining, z*remaining + (i % 2 === 0 ? arc : -arc)];
    });
  }
  return blocks.map(() => [0,0,0]);            // HOLD 段归位
  ```
  `shuffle` 按 `cycle` 缓存（跨帧复用）。
- **`hint(width, height, alpha, background)`**（**【原文完全缺失】** 返回 hint 的几何 + 颜色）：
  ```ts
  if (alpha <= 0) return undefined;
  const key = formatKeyText(getKeybindings().getKeys("tui.select.cancel")[0] ?? "escape");
  const text = `${key} to return`;                 // 【逐字】"<key> to return"
  if (text.length > width) return undefined;
  return { row: height - 2, start: Math.floor((width - text.length) / 2), text,
           keyLength: key.length,
           keyColor: mix(background, toRgb(theme.colors.muted), alpha),
           color:   mix(background, toRgb(theme.colors.dim), alpha) };
  ```
- **`prepareStars(width, height)`**（**逐字**）：
  ```ts
  const stars = new Array(width * height);
  for (let index = 0; index < width * height; index++) {
    if (hash(index * 3 + 0x51ed) >= STAR_DENSITY) continue;
    const random = (salt) => hash(index * 7 + salt * 0x9e37);
    stars[index] = {
      glyph: BRAILLE[DOT_BITS[Math.floor(random(1) * 8)]],
      tint: STAR_TINTS[Math.floor(random(2) * STAR_TINTS.length)],
      strength: 0.1 + random(3) * 0.12,
      phase: random(4) * Math.KIANA * 2,
      speed: 0.4 + random(5) * 0.8,
    };
  }
  ```
- **`prepareCells(width, foreground)`**：`parseScreen(this.screen, width, foreground, background)`；
  `centerX = origin ? origin.column + columns/2 : width/2`；
  `centerY = origin ? origin.row + rows/4 : height/2`（**【注意】** `/4`，不是 `/2`）；
  `farthest = hypot(max(centerX, width-centerX), max(centerY, height-centerY) * 2)`；
  逐 cell：`owner = cell.width === 0 ? line[column-1] : cell`；
  `seed = hash(row * 65_537 + column)`；非零宽 cell
  `delay = FLY_START + hypot(column-centerX, (row-centerY)*2) / farthest * WAVE_SPREAD + seed * WAVE_JITTER`，
  零宽 cell `delay = owner.delay`；`cell.seed = row*65_537 + column`、`cell.ink = glyphInk(owner.text, seed)`。
  `origin` 存在时把 header 区域（`origin.row .. origin.row + ceil(rows/2) - 1`、`origin.column ..
  origin.column + columns - 1`）清成 `text=" "、width=1、ink=0、bg=undefined`（保留 fg）。
- **`pack(color)`** = `(round(r) << 16) | (round(g) << 8) | round(b)`；
  **`ansi(key, isBackground)`**：缓存键 `key * 2 + (isBackground ? 1 : 0)`；
  未命中时 `rgbColor((key>>16)&255, (key>>8)&255, key&255)` + `theme.getColorMode()` +
  `foregroundAnsi`/`backgroundAnsi`。
- **`emit(text, fg, bg)`**（**【原文缺失】** 逐字行为）：
  - 前景**只在 `text !== " "` 时**发（空格永不改前景）；背景**总是**在 key 变化时发；
  - 首帧 `currentFg = -1`、`currentBg = -1`；
  - 每行结尾 `\x1b[0m` 并 `lines.push(line + "\x1b[0m")`。
- `handleInput(data)`：`tui.select.cancel` **或** `app.clear` → `close()`（不返回值）。
- `handleMouse(event)`：**任意类型**都返回 `{ handled: true, render: false }`，但只有
  `event.type === "click"` 才 `close()`。
- `invalidate()`：`screenWidth = -1; screenHeight = -1; ansiCache.clear()`（**不**清 stars/cells，
  下次 `render` 因尺寸 -1 而重建）。
- **没有 `dispose()`**。
- **`glyphInk(text, seed)`**（`prepareCells` 用；**【更正】** 正则逐字）：
  ```ts
  if (text.trim() === "") return 0;
  if (/^[.,:;'`\-_·]$/.test(text)) return 2;
  if (/^[─-╿]$/.test(text)) return 3;
  return 4 + Math.floor(seed * 3);
  ```
- **`DISSOLVE_END = FLY_START + WAVE_SPREAD + WAVE_JITTER + DUST_DURATION = 1.12`**
  （`textFade`/`dissolve` 的截止时间；**【原文缺失】**）。

**测试建议（当前无自动化测试）**：用固定 `Math.random` 种子 + 固定 `performance.now()` 生成 10 帧快照，
逐字符比对（Rust 必须与 TS 完全一致，含 `Float32` 舍入）。见 §8.3。

### 2.46 `easter-egg-3d.lazy.ts`（27 行）

**【精读更正 · 导出面】** 本文件**只导出 2 个同步函数**，`playEasterEgg3d` 是**模块私有**：

```ts
export function playKianaLogo3d(tui: TUI, column: number, row: number): void;
export function playArmin3d(tui: TUI): boolean;

function playEasterEgg3d(tui: TUI, egg: EasterEgg3d): boolean;   // 私有
```

- `playEasterEgg3d(tui, egg)`（**同步**，返回 `boolean`）：
  ```ts
  if (!(tui instanceof TuiAltScreen)) return false;
  if (tui.hasOverlay()) return true;                 // 已有 overlay → 视作「已处理」，返回 true
  const screen = tui.getScreenLines();               // 【关键】在动态 import **之前**抓屏
  import("./easter-egg-3d.ts").then(
    (module) => module.playEasterEgg3d(tui, screen, egg),   // 注意：模块内函数同名的另一签名（3 参）
    () => {},                                              // 动态导入失败静默（被拒绝的回调）
  );
  return true;
  ```
- `playKianaLogo3d(tui, column, row)`：`void` 返回；内部 `playEasterEgg3d(tui, { kind: "kiana-logo", column, row })`。
- `playArmin3d(tui)`：`boolean` 返回；内部 `playEasterEgg3d(tui, { kind: "armin" })` —— 调用方据此决定
  是否回退到内联版 armin。
- **语义**：不能因彩蛋崩溃；不能阻塞输入（**异步动态导入**，本文件不 `await`）；只在 alt-screen 且无
  现有 overlay 时播放；`screen` 用的是 **alt-screen 当前渲染行快照**。

### 2.47 `radius-login-selector.ts` — 见 §2.22

### 2.48 `first-time-setup.ts`（148 行）— 首次启动向导

> **⚠ 本节签名有误，以 §2.52.6 为准。**（构造函数是单个 `options` 对象，不是位置参数）


```ts
export interface FirstTimeSetupOptions {
  onThemePreview?: (themeName: string) => void;
  initialTheme?: string;
  initialShareAnalytics?: boolean;
}
export interface FirstTimeSetupResult { theme: string; shareAnalytics: boolean }
export class FirstTimeSetupComponent extends Container implements Focusable {
  constructor(onComplete: (result: FirstTimeSetupResult) => void, onCancel: () => void,
              options?: FirstTimeSetupOptions);
  handleInput(data: string): void;
  focused: boolean;
}
```

常量：

```ts
const SETUP_LOGO_LINES = ["██████", "██  ██", "████  ██", "██    ██"];
const THEME_OPTIONS = [
  { value: "system", label: "System (matches your terminal colors)" },
  { value: "dark",   label: "Dark" },
  { value: "light",  label: "Light" },
];
const ANALYTICS_OPTIONS = [
  { value: true,  label: "Share anonymous usage data" },
  { value: false, label: "Don't share" },
];
```

步骤：`step: "theme" | "analytics"`（初始 `"theme"`）。

布局：

1. `DynamicBorder()`
2. `Spacer(1)`
3. logo 4 行（`theme.fg("accent", line)`，`Text(..., 1, 0)`）
4. `Spacer(1)`
5. `Text(theme.bold(theme.fg("accent", `Welcome to ${APP_NAME}, the minimal coding agent.`)), 1, 0)`
6. `Spacer(1)`
7. 当前步骤提示：
   - theme：`Text("Pick a theme.", 1, 0)`
   - analytics：`Text("Opt-in to anonymous usage data sharing?", 1, 0)` + 4 行 muted 段落（见下）
8. `Spacer(1)`
9. 选项行（逐行 `Text(..., 1, 0)`）：
   `prefix = selected ? theme.fg("accent","→ ") : "  "` + label（选中时 accent? 源码：`prefix + theme.fg(selected ? "accent" : "text", label)`）
10. `Spacer(1)`
11. hints：`rawKeyHint("↑↓","navigate") + "  " + keyHint("tui.select.confirm", step === "theme" ? "continue" : "finish") + "  " + keyHint("tui.select.cancel","skip setup")`
12. `Spacer(1)`
13. `DynamicBorder()`

Analytics 段落（**逐字**，4 行，muted，`Text(..., 1, 0)`）：

```
Opting in stores a tracking identifier in settings.json and enables anonymous
usage analytics. This helps us to better debug, reproduce, and resolve issues
and bugs within Kiana. You can observe what is shared using /privacy and make
changes anytime in settings.json.
```

（注意断行位置：第一行以 `anonymous` 结尾、第二行以 `issues` 结尾、第三行以 `make` 结尾。）

- 键位：up/`"k"`、down/`"j"`、confirm/`"\n"`、cancel。
  - confirm 在 theme 步 → 记录 `theme`，切到 analytics 步并重建；在 analytics 步 →
    `onComplete({ theme, shareAnalytics })`。
  - 移动光标时若在 theme 步 → `options.onThemePreview?.(value)`（实时预览主题）。
  - cancel → `onCancel()`。
- **【逐字】** `Welcome to ${APP_NAME}, the minimal coding agent.`（`APP_NAME` 默认 `kiana`，可被配置覆盖）、
  `Pick a theme.`、`Opt-in to anonymous usage data sharing?`、`System (matches your terminal colors)`、`Dark`、
  `Light`、`Share anonymous usage data`、`Don't share`、`navigate`、`continue`、`finish`、`skip setup`。

### 2.49 `bordered-loader.ts` / 其它小文件

见 §2.12（bordered-loader）、§2.22（radius-login-selector）、§2.10（earendil-announcement）、
§2.7（kiana-logo）、§2.3（themed-text）、§2.2（dynamic-border）、§2.4（markdown-transform）。

### 2.50 `thinking-selector.ts`（154 行）— `/thinking` 选择器

```ts
export class ThinkingSelectorComponent extends Container implements Focusable {
  constructor(currentLevel: ThinkingLevel, availableLevels: ThinkingLevel[],
              onSelect: (level: ThinkingLevel) => void, onCancel: () => void,
              onSelectAsDefault?: (level: ThinkingLevel) => void,
              defaultThinkingLevel?: ThinkingLevel);
  handleInput(data: string): void;
  getSelectList(): SelectList;     // 【补充】过滤后返回新实例
  focused: boolean;                // getter/setter 代理 searchInput.focused
}
```

常量：

```ts
const LEVEL_DESCRIPTIONS: Record<ThinkingLevel, string> = {
  off:     "No reasoning",
  minimal: "Very brief reasoning (~1k tokens)",
  low:     "Light reasoning (~2k tokens)",
  medium:  "Moderate reasoning (~8k tokens)",
  high:    "Deep reasoning (~16k tokens)",
  xhigh:   "Extra-high reasoning (~32k tokens)",
  max:     "Maximum reasoning",
};
const LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 };
```

**布局**：

1. `DynamicBorder()`
2. `Spacer(1)`
3. `Text("Thinking Level", 0, 0)`
4. `Spacer(1)`
5. `Text(keyDisplayText("app.thinking.cycle") + " cycles thinking levels in-session", 0, 0)`
6. `Spacer(1)`
7. 搜索 `Input`（**只有** `onSubmit = () => selectList.handleInput("\r")`；**没有 `onChange`**，
   过滤在 `handleInput` 末尾显式 `applyFilter(getValue())`）
8. `Spacer(1)`
9. `SelectList(items, Math.max(1, items.length), getSettingsListTheme? 或 getSelectListTheme(), LAYOUT)`
   → 源码用 `getSelectListTheme()`。
10. `Spacer(1)`
11. hint：`Text(theme.fg("dim", `  ${keyDisplayText("tui.select.confirm")} to select · ${keyDisplayText("app.thinking.save")} to set as default · ${keyDisplayText("tui.select.cancel")} to cancel`), 0, 0)`
12. `DynamicBorder()`

**items**（每个可用 level 一项）：

```ts
{
  value: level,
  label: `${level === currentLevel ? "✓ " : "  "}${level}`,
  description: LEVEL_DESCRIPTIONS[level] + (level === defaultThinkingLevel ? " · default" : ""),
}
```

- **【逐字】** `Thinking Level`、` cycles thinking levels in-session`、`  `、` to select · `、
  ` to set as default · `、` to cancel`、`✓ `、` · default`、`No reasoning`、`Very brief reasoning (~1k tokens)`、
  `Light reasoning (~2k tokens)`、`Moderate reasoning (~8k tokens)`、`Deep reasoning (~16k tokens)`、
  `Extra-high reasoning (~32k tokens)`、`Maximum reasoning`。

**过滤**：`fuzzyFilter(allItems, query, item => `${item.value} ${item.description ?? ""}`)`；
过滤后**保持当前选中项**（若仍在结果中），否则回到 0。

**键位**：

```ts
if (onSelectAsDefault && matches app.thinking.save) { onSelectAsDefault(selected.value); return; }
if (nav keys: up/down/pageUp/pageDown/tui.select.confirm/"\n"/tui.select.cancel) → 转交 selectList
else searchInput.handleInput(data) → 过滤
```

- **【更正】** nav 集合**只有** 4 个：`tui.select.up`/`tui.select.down`/`tui.select.confirm`/`tui.select.cancel`
  （**没有** `"\n"` 硬编码，也**没有** pageUp/pageDown —— 与 §2.53.4 一致）。
- `app.thinking.save` 分支在 nav **之前**，且要求 `onSelectAsDefault` 存在；
  取 `selectList.getSelectedItem()`，无选中项时不动作。
- 测试（2 项）：当前等级带 `✓`；`Ctrl+S` 触发 `onSelectAsDefault`。

---
---

### 2.51 对 §2.43 / §2.50 的精读更正（以本节为准）

上述两节为速写；以下是逐行核对源码后的**权威细节**，实现时以本节为准。

#### 2.51.1 `tree-selector.ts` 的缩进单位是 3 字符

- `FlatNode.indent` 的注释原文：`/** Indentation level (each level = 3 chars) */`。
  **每层 3 列**，不是 2。（`TREE_GUTTER_WIDTH = 2` 是**水平视口**保留给 gutter 的固定宽度，
  与树缩进单位无关。）
- `render` 里 `const totalChars = displayIndent * 3;`，逐字符构造前缀：
  ```ts
  for (let i = 0; i < totalChars; i++) {
    const level = Math.floor(i / 3);
    const posInLevel = i % 3;
    const gutter = flatNode.gutters.find(g => g.position === level);
    if (gutter) {
      prefixChars.push(posInLevel === 0 ? (gutter.show ? "│" : " ") : " ");
    } else if (connector && level === connectorPosition) {
      if (posInLevel === 0) prefixChars.push(flatNode.isLast ? "└" : "├");
      else if (posInLevel === 1) {
        const foldable = this.isFoldable(entry.id);
        prefixChars.push(isFolded ? "⊞" : foldable ? "⊟" : "─");
      } else prefixChars.push(" ");
    } else prefixChars.push(" ");
  }
  ```
- `connector = flatNode.showConnector && !flatNode.isVirtualRootChild ? (flatNode.isLast ? "└─ " : "├─ ") : ""`；
  `connectorPosition = connector ? displayIndent - 1 : -1`。
- **折叠指示符的两种位置**：
  - 有 connector 时，`posInLevel === 1` 的字符替换为 `⊞`（已折叠）/ `⊟`（可折叠未折叠）/ `─`；
  - 无 connector（根节点）时，折叠的节点额外加前缀 `foldMarker = theme.fg("accent","⊞ ")`。
- 行组装：
  ```ts
  const pathMarker = isOnActivePath ? theme.fg("accent", "• ") : "";
  const label = node.label ? theme.fg("warning", `[${label}] `) : "";
  const labelTimestamp = showLabelTimestamps && node.label && node.labelTimestamp
    ? theme.fg("muted", `${formatLabelTimestamp(node.labelTimestamp)} `) : "";
  const content = getEntryDisplayText(node, isSelected);
  const prefixPart = theme.fg("dim", prefix) + foldMarker + pathMarker;
  const anchorCol = visibleWidth(prefixPart);
  let gutter = cursor;                                    // "› " 或 "  "
  let body = prefixPart + label + labelTimestamp + content;
  if (isSelected) { gutter = theme.bg("selectedBg", gutter); body = theme.bg("selectedBg", body); }
  rows.push({ gutter, body, anchorCol, bodyWidth: visibleWidth(body), isSelected });
  ```
- 底部行：`truncateToWidth(theme.fg("muted", `  (${sel+1}/${total})${getStatusLabels()}`), width)`
  —— 注意底部用 **muted**（空列表时 `  (0/0)` 也是 muted）。

#### 2.51.2 `renderHorizontalViewport` 的真实算法（水平平移）

```ts
const viewportWidth = Math.max(0, width - TREE_GUTTER_WIDTH);      // TREE_GUTTER_WIDTH = 2
const maxBodyWidth = rows.reduce((max, r) => Math.max(max, r.bodyWidth), 0);
const maxHorizontalScroll = Math.max(0, maxBodyWidth - viewportWidth);
const selectedRow = rows.find(r => r.isSelected);

let horizontalScroll = 0;
if (selectedRow && maxHorizontalScroll > 0) {
  const minVisibleAnchorContentWidth = Math.min(
    MAX_VISIBLE_ANCHOR_CONTENT_WIDTH,                                  // 20
    Math.max(MIN_VISIBLE_ANCHOR_CONTENT_WIDTH, Math.floor(viewportWidth / 3)),   // ≥4
  );
  if (selectedRow.anchorCol > viewportWidth - minVisibleAnchorContentWidth) {
    const anchorContextWidth = Math.min(
      MAX_ANCHOR_CONTEXT_WIDTH,                                        // 12
      Math.max(MIN_ANCHOR_CONTEXT_WIDTH, Math.floor(viewportWidth / 4)), // ≥2
    );
    horizontalScroll = Math.min(maxHorizontalScroll, selectedRow.anchorCol - anchorContextWidth);
  }
}

return rows.map(row => {
  const line = horizontalScroll > 0
    ? `${row.gutter}${sliceByColumn(row.body, horizontalScroll, viewportWidth, true)}\x1b[0m`
    : row.gutter + row.body;
  return truncateToWidth(line, width, "");
});
```

- 语义：只有当**选中行**的正文起点（`anchorCol`，即缩进+标记之后的列）太靠右、导致右侧可见内容少于
  `minVisibleAnchorContentWidth` 时，才把正文整体左移；左移量保证锚点左侧仍留 `anchorContextWidth` 列上下文。
- `gutter`（`› ` 或 `  `）**永不左移**；`sliceByColumn(..., strict=true)` 按列切（宽字符安全），
  末尾 `\x1b[0m` 防止颜色溢出。
- 之前的 §2.43 描述不完整，以此为准。

#### 2.51.3 `flattenTree` 的完整规则

```ts
// 缩进规则（原文注释）：
// - At indent 0: stay at 0 unless parent has >1 children (then +1)
// - At indent 1: children always go to indent 2 (visual grouping of subtree)
// - At indent 2+: stay flat for single-child chains, +1 only if parent branches
```

- `containsActive`：先**前序**收集所有节点（`preOrderStack`，子节点逆序入栈），再**逆序**遍历做后序聚合：
  `has = (leafId !== null && node.entry.id === leafId) || 任一 child 的 containsActive`。
- 根排序：`orderedRoots = [...roots].sort((a,b) => Number(containsActive(b)) - Number(containsActive(a)))`
  （稳定排序，active 分支在前）。
- 多根：把每个根当作「虚拟根」的子节点压栈，参数 `indent = 1`、`justBranched = multipleRoots`、
  `showConnector = multipleRoots`、`isVirtualRootChild = multipleRoots`。
- 子节点压栈顺序：`orderedChildren`（active 优先，其余保序）**逆序**压栈，`childIsLast = (i === len-1)`。
- 子缩进：
  ```ts
  let childIndent: number;
  if (multipleChildren) childIndent = indent + 1;
  else if (justBranched && indent > 0) childIndent = indent + 1;
  else childIndent = indent;
  ```
- gutter 传递：
  ```ts
  const connectorDisplayed = showConnector && !isVirtualRootChild;
  const currentDisplayIndent = multipleRoots ? Math.max(0, indent - 1) : indent;
  const connectorPosition = Math.max(0, currentDisplayIndent - 1);
  const childGutters = connectorDisplayed
    ? [...gutters, { position: connectorPosition, show: !isLast }]
    : gutters;
  ```
- `toolCallMap`：遍历 assistant 消息的 `toolCall` 块，记录 `id → {name, arguments}`，供 toolResult 显示。

#### 2.51.4 `applyFilter` 的完整规则（含折叠后代剔除）

1. 若 `filteredNodes.length > 0`，先把 `lastSelectedId` 更新为当前选中 id（**空结果时保留旧选择**）。
2. `searchTokens = searchQuery.toLowerCase().split(/\s+/).filter(Boolean)`。
3. 基线过滤（**所有模式共用**）：
   ```ts
   if (entry.type === "usage") return false;
   const isCurrentLeaf = entry.id === currentLeafId;
   if (entry.type === "message" && entry.message.role === "assistant" && !isCurrentLeaf) {
     const hasText = this.hasTextContent(msg.content);
     const isErrorOrAborted = msg.stopReason && msg.stopReason !== "stop" && msg.stopReason !== "toolUse";
     if (!hasText && !isErrorOrAborted) return false;
   }
   ```
   （即：只有 toolCall、无文本、且 stopReason 正常、且不是当前叶的 assistant 消息被隐藏。）
4. 模式过滤：
   ```ts
   const isSettingsEntry = entry.type === "label" || entry.type === "context_edit" || entry.type === "custom"
     || entry.type === "model_change" || entry.type === "thinking_level_change" || entry.type === "session_info";
   switch (filterMode) {
     case "user-only":    passesFilter = entry.type === "message" && entry.message.role === "user"; break;
     case "no-tools":     passesFilter = !isSettingsEntry && !(entry.type === "message" && entry.message.role === "toolResult"); break;
     case "labeled-only": passesFilter = flatNode.node.label !== undefined; break;
     case "all":          passesFilter = true; break;
     default:             passesFilter = !isSettingsEntry; break;
   }
   ```
   **注意**：`user-only` **只**保留 user 消息 —— 设置类条目（含 context_edit、label）被排除；
   这正是测试「隐藏 context edits + labels」的断言。
5. 搜索：`nodeText = getSearchableText(node).toLowerCase()`；每个 token 都要 `includes`。
6. **折叠后代替换**：
   ```ts
   if (foldedNodes.size > 0) {
     const skipSet = new Set<string>();
     for (const flatNode of flatNodes) {
       const { id, parentId } = flatNode.node.entry;
       if (parentId != null && (foldedNodes.has(parentId) || skipSet.has(parentId))) skipSet.add(id);
     }
     filteredNodes = filteredNodes.filter(n => !skipSet.has(n.node.entry.id));
   }
   ```
   （按 `flatNodes` 原序遍历，因此祖先先于后代被判定。）
7. `recalculateVisualStructure()`：按**可见**父子关系重算 `indent`/`showConnector`/`isLast`/`gutters`，
   规则与 `flattenTree` 相同（单子链不右移），并构建 `visibleParentMap` / `visibleChildrenMap`。
   子节点保持 `filteredNodes` 中的顺序。
8. 选中恢复：`selectedIndex = findNearestVisibleIndex(lastSelectedId)`；只在无 `lastSelectedId` 时
   才 clamp。`findNearestVisibleIndex` 沿 `parentId` 上溯到第一个可见祖先，找不到则取**最后一项**。

#### 2.51.5 `isFoldable` / `findBranchSegmentStart`（精读）

```ts
private isFoldable(entryId: string): boolean {
  const children = this.visibleChildrenMap.get(entryId);
  if (!children || children.length === 0) return false;
  const parentId = this.visibleParentMap.get(entryId);
  if (parentId === null || parentId === undefined) return true;      // 根可折叠
  const siblings = this.visibleChildrenMap.get(parentId);
  return siblings !== undefined && siblings.length > 1;              // 仅在分支点可折叠
}

private findBranchSegmentStart(direction: "up" | "down"): number {
  const selectedId = this.filteredNodes[this.selectedIndex]?.node.entry.id;
  if (!selectedId) return this.selectedIndex;
  const indexByEntryId = new Map(this.filteredNodes.map((n, i) => [n.node.entry.id, i]));
  let currentId = selectedId;
  if (direction === "down") {
    while (true) {
      const children = this.visibleChildrenMap.get(currentId) ?? [];
      if (children.length === 0) return indexByEntryId.get(currentId)!;
      if (children.length > 1) return indexByEntryId.get(children[0])!;
      currentId = children[0];
    }
  }
  while (true) {
    const parentId = this.visibleParentMap.get(currentId) ?? null;
    if (parentId === null) return indexByEntryId.get(currentId)!;
    const children = this.visibleChildrenMap.get(parentId) ?? [];
    if (children.length > 1) {
      const segmentStart = indexByEntryId.get(currentId)!;
      if (segmentStart < this.selectedIndex) return segmentStart;
    }
    currentId = parentId;
  }
}
```

- `foldOrUp`：若当前节点**可折叠且未折叠** → 折叠（`foldedNodes.add` + `applyFilter`），**否则**跳到上一个
  分支段起点。
- `unfoldOrDown`：若当前节点**已折叠** → 展开（`delete` + `applyFilter`），**否则**跳到下一个分支段起点。

#### 2.51.6 `handleInput` 的精确条件（更正 §2.43 的速写）

- 翻页键是 **`tui.editor.cursorLeft` / `tui.editor.cursorRight`**，并**额外接受**
  `tui.select.pageUp` / `tui.select.pageDown`（`||` 连接），步长 = `maxVisibleLines`。
- `confirm` 不额外接受 `"\n"`（`tui.select.confirm` 默认就是 Enter）。
- 过滤器按键是**切换语义**（不是直接设定）：
  `default` → 直接设为 `"default"`；`noTools`/`userOnly`/`labeledOnly`/`all` → 当前已是该模式则回
  `"default"`，否则切到该模式；`cycleForward` → `default → no-tools → user-only → labeled-only → all → default`；
  `cycleBackward` → 反向。**每次切换都 `foldedNodes.clear()`**。
- `deleteCharBackward`：仅当 `searchQuery.length > 0` 时退格 + 清折叠 + `applyFilter`（无查询时**吞掉**该键，
  不 cancel）。
- `app.tree.editLabel` → `onLabelEdit(entryId, currentLabel)`（宿主 `TreeSelectorComponent` 打开
  `LabelInput`）。
- 兜底字符输入（**逐字**）：
  ```ts
  const hasControlChars = [...keyData].some(ch => {
    const code = ch.charCodeAt(0);
    return code < 32 || code === 0x7f || (code >= 0x80 && code <= 0x9f);
  });
  if (!hasControlChars && keyData.length > 0) { searchQuery += keyData; foldedNodes.clear(); applyFilter(); }
  ```
  （含 C1 控制字符 0x80–0x9F；按 **UTF-16 code unit** 判定，Rust 用 `chars()` 但需同样排除这些码点。）
- `tui.select.cancel`：`searchQuery` 非空 → 清查询 + 清折叠 + `applyFilter`；否则 `onCancel()`。
- `app.message.copy` → `copySelected()`：内部取 `getEntryCopyText` 并 `copyToClipboard`；
  文本为 undefined 时**不写剪贴板**（`onCopy?.(text)` 也仅在宿主设置时调用）。
  实际实现：`TreeList.copySelected()` 调用 `this.onCopy?.(text)`；由 `TreeSelectorComponent` 转给宿主的
  `onCopy`，宿主执行剪贴板写入。

#### 2.51.7 `SearchLine` / `TreeHelp` / `LabelInput` 的精确输出

```ts
// SearchLine
query ? `  ${theme.fg("muted","Type to search:")} ${theme.fg("accent", query)}`
      : `  ${theme.fg("muted","Type to search:")}`
// 均 truncateToWidth(..., width)

// TreeHelp：items = TREE_HELP_ITEMS.map(({keys,label,labelFirst}) => {
//   const text = formatHelpKeys(keys); if (!text) return label;
//   return labelFirst ? `${label} ${text}` : `${text} ${label}`;
// });
// availableWidth = Math.max(1, width); indent = "  "; separator = " · ";
// 贪心装行：candidate = currentLine ? currentLine + separator + item
//                       : (visibleWidth(indent+item) <= availableWidth ? indent + item : item);
// 若 currentLine 存在且 candidate 超宽 → push wrapTextWithAnsi(currentLine.trimEnd(), availableWidth)，
//   再以 indent+item（若放得下）或 item 开新行；
// 结束 push 最后一行；所有行 theme.fg("muted", line)。

// LabelInput.render(width)
const indent = "  ";
const availableWidth = width - indent.length;
[
  truncateToWidth(`${indent}${theme.fg("muted","Label (empty to remove):")}`, width),
  ...this.input.render(availableWidth).map(l => truncateToWidth(`${indent}${l}`, width)),
  truncateToWidth(`${indent}${keyHint("tui.select.confirm","save")}  ${keyHint("tui.select.cancel","cancel")}`, width),
]
```

- `LabelInput` 的 `focused` 代理到内部 `Input`；构造时有 `currentLabel` 则 `setValue(currentLabel)`。
- 提交：`value = input.getValue().trim()`；`onSubmit(entryId, value || undefined)`（**空串提交 undefined，
  即删除 label**）。
- `TreeSelectorComponent.showLabelInput` 会把 `treeContainer.clear()`（隐藏树）并把 `LabelInput` 放进
  `labelInputContainer`；`hideLabelInput` 反向恢复。
- `TreeSelectorComponent.focused` 的 setter 会传播给当前 `labelInput`。
- `TreeSelectorComponent` 空树：`setTimeout(() => onCancel(), 100)`。
- `TreeSelectorComponent.getTreeList()` 暴露 `TreeList`（测试用）。

#### 2.51.8 `tree-selector` 的 `getEntryDisplayText` 颜色更正

实际颜色（**逐字**，与 §2.43 表格中的描述有出入，以此为准）：

| 条目 | 颜色/文本 |
|---|---|
| user | `theme.fg("accent","user: ")` + 内容（**accent**，不是默认色） |
| assistant 文本 | `theme.fg("success","assistant: ")` + 内容 |
| assistant aborted | `theme.fg("success","assistant: ")` + `theme.fg("muted","(aborted)")` |
| assistant error | `theme.fg("success","assistant: ")` + `theme.fg("error", errMsg.slice(0,80))` |
| assistant 无内容 | `theme.fg("success","assistant: ")` + `theme.fg("muted","(no content)")` |
| toolResult 有 toolCall | `theme.fg("muted", formatToolCall(name,args))` |
| toolResult 无 toolCall | `theme.fg("muted", `[${toolName ?? "tool"}]`)` |
| bashExecution | `theme.fg("dim", `[bash]: ${command}`)`（command 经 `normalize`） |
| 其它 role | `theme.fg("dim", `[${role}]`)` |
| custom_message | `theme.fg("customMessageLabel", `[${customType}]: `)` + `normalize(content)`；数组 content 的文本块用 `""`（**空串，不是换行**）join |
| compaction | `theme.fg("borderAccent", `[compaction: ${Math.round(tokensBefore/1000)}k tokens]`)` |
| branch_summary | `theme.fg("warning", `[branch summary]: `)` + `normalize(summary)` |
| model_change | `theme.fg("dim", `[model: ${modelId}]`)` |
| thinking_level_change | `theme.fg("dim", `[thinking: ${level}]`)` |
| custom | `theme.fg("dim", `[custom: ${customType}]`)` |
| context_edit | `theme.fg("dim", `[context ${replacement === null ? "omit" : "replace"}: ${targetId}]`)` |
| label | `theme.fg("dim", `[label: ${label ?? "(cleared)"}]`)` |
| session_info 有 name | `[theme.fg("dim","[title: "), theme.fg("dim", name), theme.fg("dim","]")].join("")` |
| session_info 空 name | `[theme.fg("dim","[title: "), theme.italic(theme.fg("dim","empty")), theme.fg("dim","]")].join("")` |
| 其它 | `""` |

- `normalize(s) = s.replace(/[\n\t]/g, " ").trim()`（**只**替换换行与制表符，不处理其它控制字符）。
- 选中时 `theme.bold(result)` 包住**整个**已着色字符串。
- `extractContent(content) = extractFullContent(content).slice(0, 200)`；
  `extractFullContent` 只拼接 `type === "text"` 的块（无分隔符），字符串 content 原样返回。
- `formatToolCall` 的路径经 `shortenPath`（HOME/USERPROFILE 前缀 → `~`）；
  `read/write/edit` 优先 `args.path`，回退 `args.file_path`；`grep/find/ls` 的 `path` 默认 `"."`；
  bash 的 `cmd` 是 `rawCmd.replace(/[\n\t]/g," ").trim().slice(0,50)`，**省略号条件用 `rawCmd.length > 50`**
  （原文长度，不是 trim 后）。
- 默认工具：`argsStr = JSON.stringify(args).slice(0,40)`，省略号条件 `JSON.stringify(args).length > 40`
  （**调用两次 `JSON.stringify`**，Rust 侧只需语义一致）。

#### 2.51.9 `formatLabelTimestamp`（tree 版，参数是字符串）

```ts
private formatLabelTimestamp(timestamp: string): string {
  const date = new Date(timestamp);
  const now = new Date();
  const time = `${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
  if (sameYear && sameMonth && sameDate) return time;
  const month = date.getMonth() + 1, day = date.getDate();
  if (sameYear) return `${month}/${day} ${time}`;
  return `${String(date.getFullYear()).slice(-2)}/${month}/${day} ${time}`;
}
```

（日期**不补零**；年份后两位；与 §2.42 的 `formatSessionDate` 是不同函数。）

#### 2.51.10 `thinking-selector.ts` 的精确行为

- 布局（**逐个 addChild**）：`DynamicBorder()` → `Spacer(1)` → `Text("Thinking Level", 0, 0)` → `Spacer(1)` →
  `Text(`${keyDisplayText("app.thinking.cycle")} cycles thinking levels in-session`, 0, 0)` → `Spacer(1)` →
  `searchInput` → `Spacer(1)` → `selectList` → `Spacer(1)` → hint `Text(theme.fg("dim", `  ${confirm} to select · ${save} to set as default · ${cancel} to cancel`), 0, 0)` →
  `DynamicBorder()`。
- **所有 Text 的 paddingX 都是 0**（不是 1）；hint 自带两个前导空格。
- `selectListChildIndex = this.children.length`（在 addChild 之前记录），`applyFilter` 用
  `this.children[this.selectListChildIndex] = newList` **原地替换**（保持其它子组件索引不变）。
- `buildSelectList(items, preselect)`：`new SelectList(items, Math.max(1, items.length), getSelectListTheme(),
  {minPrimaryColumnWidth:12, maxPrimaryColumnWidth:32})`；`preselect` 命中则 `setSelectedIndex`。
- `applyFilter(query)`：`query` 为空 → 全部；否则
  `fuzzyFilter(allItems, query, item => `${item.value} ${item.description ?? ""}`)`；
  **保持当前选中值**（`selectList.getSelectedItem()?.value` 作为新列表的 preselect）。
- `handleInput` 顺序：**先**判 `app.thinking.save`（且 `onSelectAsDefault` 存在）→ 取当前项并回调，
  **立即返回**；然后判 `isNav`（up/down/confirm/cancel 四个）→ 转交列表；其它 → 搜索框 + `applyFilter`。
- **注意**：`isNav` **不含** pageUp/pageDown；翻页键会进入搜索框（`Input` 忽略它们）。

---

### 2.52 对 §2.33 / §2.40 / §2.41 / §2.48 的精读更正（以本节为准）

#### 2.52.1 `settings-submenu.ts`：`SelectSubmenu` 的真实签名

源码签名（**8 个位置参数**，最后一个是可选的 `submenuOptions`；【更正】原文写 9 个是笔误）：

```ts
class SelectSubmenu extends Container {
  constructor(
    title: string,
    description: string,
    options: SelectItem[],
    currentValue: string,
    onSelect: (value: string) => void,
    onCancel: () => void,
    onSelectionChange?: (value: string) => void,
    submenuOptions?: { searchable?: boolean; layout?: SelectListLayoutOptions },
  );
}
```

- `SUBMENU_SELECT_LIST_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 }`（默认）。
- 布局（**paddingX 全部为 0**）：
  `Text(theme.bold(theme.fg("accent", title)), 0, 0)` → 若有 description：`Spacer(1)` +
  `Text(theme.fg("muted", description), 0, 0)` → 若 searchable：`Spacer(1)` + `Input`（`onSubmit` →
  `selectList.handleInput("\r")`）→ `Spacer(1)` → `SelectList(options, Math.min(options.length, 10),
  getSelectListTheme(), layout)` → `Spacer(1)` → hint
  `Text(theme.fg("dim", searchable ? "  Type to filter · Enter to select · Esc to go back"
                                     : "  Enter to select · Esc to go back"), 0, 0)`。
  **末尾无 Spacer**。
- `listChildIndex = this.children.length`（在 addChild 前记录），`applyFilter` 用
  `this.children[this.listChildIndex] = newList` 原地替换，**且新列表 preselect 传 `""`**（即过滤后
  不保留选中项 → 回到第一项）。
- `handleInput`：有 searchInput 时，**仅** up/down/confirm/cancel 交给列表，其它一律进搜索框并
  `applyFilter`；无 searchInput 时全部交给列表。
- `onSelectionChange` 若提供，列表每次移动都会回调（用于实时预览主题）。
- **【逐字】** `  Type to filter · Enter to select · Esc to go back`、`  Enter to select · Esc to go back`。
  注意点号是 `·`（U+00B7）。

#### 2.52.2 `SteppedSubmenu` 的精确行为

```ts
interface SteppedSubmenuOptions { startAtStep?: number; initialContext?: Record<string,string>; loop?: boolean }
```

- `context` 类型是 `Record<string, string>`（不是 unknown）。
- `buildStep(stepIndex)`：
  - `stepLabel = total > 1 ? `Step ${stepIndex+1}/${total} · ` : ""`；
  - `title` = 函数则 `step.title(context)`，否则字符串；
  - `description` 传给 `SelectSubmenu` 的是 **`${stepLabel}${desc}`**（`Step i/N · ` 拼在 description 前，
    **不是**标题前！）；
  - `items = step.options(context)`；`preselect = step.preselect?.(context) ?? ""`；
  - onSelect：
    ```ts
    context[step.key] = value;
    if (stepIndex < total - 1) activeComponent = buildStep(stepIndex + 1);
    else {
      onComplete({ ...context });
      if (opts.loop) { context = {}; activeComponent = buildStep(0); }
      else onCancel();          // ← 注意：非 loop 时最后一步完成后调用 onCancel()，不是 onComplete 后静默
    }
    ```
  - onCancel（Esc）：`stepIndex > 0` → `delete context[step.key]` + `buildStep(stepIndex - 1)`；否则 `onCancel()`。
  - 第 8 参数：`step.searchable || step.layout ? { searchable: step.searchable, layout: step.layout } : undefined`；
    第 7 参数（onSelectionChange）为 `undefined`。
- `render` / `handleInput` / `invalidate` 全部委托 `activeComponent`。
- **【逐字】** `Step `、`/`、` · `。
- **更正**：§2.33/§2.40 里「loop 时重置 context & step 0」正确，但「非 loop 时只 onComplete」**错误** ——
  实际还会调用 `onCancel()`（宿主据此关闭 submenu）。主题子菜单的 `onDone` 路径不受影响
  （它直接 `apply()` → `onDone`，不经过 SteppedSubmenu）。

#### 2.52.3 `ThemeSubmenu` 的真实结构（更正 §2.40 的简化描述）

`ThemeSubmenu` 是**自绘 Container**（不是 `SelectSubmenu` 子类），有 `inputComponent` 指针。

- 状态：`mode: "single" | "automatic"`、`singleTheme`、`lightTheme`、`darkTheme`、`originalThemeSetting`。
- 构造：
  ```ts
  autoTheme = parseAutoThemeSetting(currentThemeSetting);                 // {light,dark} | null
  automaticThemes = defaultAutomaticThemes(currentThemeSetting, availableThemes);
  fixedTheme = autoTheme || currentThemeSetting.includes("/") ? undefined : currentThemeSetting;
  mode = autoTheme ? "automatic" : "single";
  lightTheme = automaticThemes.lightTheme; darkTheme = automaticThemes.darkTheme;
  singleTheme = preferredTheme(availableThemes, fixedTheme ?? (autoTheme ? getActiveAutomaticTheme() : undefined),
                               SYSTEM_THEME_NAME);
  mode === "automatic" ? showAutomaticMenu() : showSingleMenu();
  ```
- `defaultAutomaticThemes(setting, themes)`：
  ```ts
  const auto = parseAutoThemeSetting(setting); if (auto) return auto;
  const fixed = setting.includes("/") ? undefined : setting;
  const name = preferredTheme(themes, fixed, SYSTEM_THEME_NAME);
  return { lightTheme: name, darkTheme: name };
  ```
- `preferredTheme(themes, preferred, fallback)`：`preferred` 在 themes 中 → 用它；否则 `fallback` 在 themes 中 →
  用它；否则 `themes[0] ?? fallback`。
- `singleTheme` 的初始值经过 `preferredTheme(..., SYSTEM_THEME_NAME)`，**因此默认是 `system`**。
- `showSingleMenu()`：
  ```ts
  new SelectSubmenu(
    "Theme",                                                       // title
    "Select a theme, or choose automatic to follow terminal appearance.",  // description
    singleModeThemeItems(availableThemes, singleTheme),            // options
    singleTheme,                                                   // currentValue
    (value) => {
      if (value === AUTOMATIC_THEME_VALUE) { mode = "automatic"; onThemePreview?.(getThemeSetting()); showAutomaticMenu(); return; }
      singleTheme = value; apply(value);                            // onDone(value)
    },
    () => cancel(),
    (value) => onThemePreview?.(value === AUTOMATIC_THEME_VALUE ? getAutomaticThemeSetting() : value),
  );
  ```
- `showAutomaticMenu()`：`Container` 内含
  `Text(theme.bold(theme.fg("accent","Automatic Theme")), 0, 0)`、`Spacer(1)`、
  `Text(theme.fg("muted","Choose themes for terminal light and dark appearance."), 0, 0)`、
  `Text(theme.fg("muted","Light/dark detection requires terminal support."), 0, 0)`、`Spacer(1)`、
  `SettingsList(items, min(4,10), getSettingsListTheme(), onChange, () => cancel())`。
  **注意第二行文案是 `Choose themes for terminal light and dark appearance.`**（不是
  “for light and dark terminal appearance”）；§2.40 的旧描述有误，以本节为准。
  items（4 项）：
  | id | label | description | currentValue / values |
  |---|---|---|---|
  | `light-theme` | `Light theme` | `Theme to use in automatic mode when the terminal is light` | `lightTheme`，**submenu** |
  | `dark-theme` | `Dark theme` | `Theme to use in automatic mode when the terminal is dark` | `darkTheme`，**submenu** |
  | `apply` | `Apply` | `Save and go back` | `"save and go back"` / `["save and go back"]` |
  | `single-mode` | `Change mode` | `Switch to one theme for light and dark` | `"switch to single theme"` / `["switch to single theme"]` |
  onChange：
  - `single-mode` → `mode = "single"; singleTheme = getActiveAutomaticTheme(); onThemePreview?.(singleTheme); showSingleMenu();`
  - `apply` → `apply(getAutomaticThemeSetting())`，即 `onDone(`${lightTheme}/${darkTheme}`)`
- `createThemeSelect(title, description, currentValue, done, onSelect)`：
  `new SelectSubmenu(title, description, themeItems(availableThemes, currentValue), currentValue, onSelect,
  () => { onThemePreview?.(getThemeSetting()); done(); }, (v) => onThemePreview?.(v))`。
  用于 Light/Dark 主题子菜单，标题 `Light Theme` / `Dark Theme`，描述
  `Select the theme to use for light terminal appearance` / `Select the theme to use for dark terminal appearance`。
- `getActiveAutomaticTheme()` = `terminalTheme === "light" ? lightTheme : darkTheme`。
- `getAutomaticThemeSetting()` = `` `${lightTheme}/${darkTheme}` ``。
- `cancel()`：`onThemePreview?.(originalThemeSetting)` 然后 `onDone()`（**无参数** → 不改变设置）。
- `handleInput` 委托 `inputComponent`。
- **【逐字】** `Select a theme, or choose automatic to follow terminal appearance.`、`Automatic Theme`、
  `Choose themes for terminal light and dark appearance.`、`Light/dark detection requires terminal support.`、
  `Light Theme`、`Dark Theme`、`Select the theme to use for light terminal appearance`、
  `Select the theme to use for dark terminal appearance`、`  automatic`、
  `Use separate themes for light and dark terminal appearance`、`switch to single theme`、`save and go back`。

#### 2.52.4 `settings-selector.ts` 的其它更正

- `modelItemLabel(model) = `${model.id} ${theme.fg("muted", `[${model.provider}]`)}``
  —— 注意是**空格 + 已着色的 `[provider]`**（不是 `id [provider]` 的纯文本）。
  `modelDisplayLabel`（用于子菜单标题）则是纯文本 `` `${id} [${provider}]` ``。
- `modelThinkingOverridesSummary`：`"none"` 或 `` `${count} configured` ``。
- `themeItems(themes, current)`：`label = `${current ? "✓ " : "  "}${name}``，system 主题额外带
  `description: "Theme created by your terminal's colors"`? ← 源码为
  **`"Theme created from your terminal's colors"`**（逐字）。
- `singleModeThemeItems(themes, current)`：把 system 项**移到最前**（`splice` 取出），然后插入
  `{ value: "/", label: "  automatic", description: "Use separate themes for light and dark terminal appearance" }`，
  再接其余项（保持原序）。
- `MODEL_PICKER_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 46 }`（**46**，不是 32）。
- `WarningSettingsSubmenu` 是**独立 Container**（不是 SelectSubmenu 子类），内含一个
  `SettingsList(items, Math.min(items.length,10), getSettingsListTheme(), onChange, onCancel)`；
  description 为 **`"Warn when Anthropic subscription auth may use paid extra usage"`**（含 `paid`，与 §2.40
  的旧记录相反 —— 主列表里 `warnings` 项没有该 description，只有子菜单项有）。
- `SettingsConfig.doubleEscapeAction` 的类型是 `"fork" | "tree" | "none"`（**含 `none`**）。
- `SettingsConfig.outputPad` 是 `0 | 1`；`SettingsConfig.fullscreenWheelScrollLines` 是 `WheelScrollLines`
  （`number | "auto"`，来自 kiana-tui）。
- `settings-selector` 的三个 submenu 项（`warnings` / `model-thinking` / `theme`）的 `currentValue`：
  `warnings` → `"configure"`；`model-thinking` → `modelThinkingOverridesSummary(...)`；`theme` → `config.currentTheme`。
- 布局：`DynamicBorder()` → `SettingsList(...)` → `DynamicBorder()`（**无 Spacer**）。
- `model-thinking` 的步进子菜单（`SteppedSubmenu`，`{ loop: true }`）：
  - 第 1 步 `key: "model"`，`title: "Per-Model Thinking Level"`，
    `description: "Select a model to configure"`，`searchable: true`，`layout: MODEL_PICKER_LAYOUT`；
    options 由 `availableDefaultModels` 排序（当前模型 → 默认模型 → `provider.localeCompare`）后映射为
    `{ value: modelSettingKey, label: modelItemLabel(model), description: override }`；
    空 → 单项 `{ value:"__none__", label:"No models available",
    description:"Log in to a provider or configure an API key first" }`；
    `preselect: () => currentModelKey ?? currentDefaultModelKey`。
  - 第 2 步 `key: "level"`，`title: ctx => `Thinking Level for ${model ? modelDisplayLabel(model) : ctx.model}``，
    `description: "Select default thinking level for this model"`；
    options：无模型 → `[]`；否则 `levels = model.reasoning ? getSupportedThinkingLevels(model) : ["off"]`，
    每项 `label: `${level === activeLevel ? "✓ " : "  "}${level}``，`description: THINKING_DESCRIPTIONS[level]`；
    若已有 override → 追加 `{ value: "__clear__", label: "  (clear override)",
    description: `Revert to global default (${config.thinkingLevel})` }`；
    `preselect: ctx => currentModelThinkingLevels[ctx.model]`。
  - onComplete：`model = defaultModelByValue.get(selections.model)`；无则返回；
    `level === "__clear__"` → `onModelThinkingLevelRemove(provider, id)` + `delete currentModelThinkingLevels[key]`；
    否则 `onModelThinkingLevelChange(provider, id, level)` + 写回本地 map。
  - onCancel：`done(modelThinkingOverridesSummary(currentModelThinkingLevels))`（刷新列表项当前值）。
  - **顺序要点**：`onComplete` 之后因 `loop: true` 会回到第 1 步（不关闭），只有 `onCancel` 才 `done(summary)`。

#### 2.52.5 `config-selector.ts` 的精读更正

- **`ResourceList` 自带搜索框！** `render` 的第一行是 `this.searchInput.render(width)`，随后一个空行，
  再是列表。之前的 §2.41 说「搜索框由父组件设 query」**错误**。
- 空列表：`theme.fg("muted", "  No resources found")`（**未**截断到 width —— 实际调用
  `lines.push(theme.fg("muted", "  No resources found"))`；宽度不变式由宿主保证，建议 Rust 侧仍截断，
  但**文本必须一致**）。
- `maxVisible = Math.max(5, (terminalHeight ?? 24) - 8)`；`chrome = 8`（注释：top spacer + top border +
  spacer + header(2 行) + ...）。
- 滚动：`startIndex = max(0, min(sel - floor(maxVisible/2), len - maxVisible))`；
  `endIndex = min(startIndex + maxVisible, len)`。
- group 行：`truncateToWidth(`  ${groupLine}`, width, "")`，其中
  ```ts
  const inherited = writeScope === "project" && group.scope === "user";
  const label = theme.bold(`${group.label}${inherited ? " · inherited global" : ""}`);
  const groupLine = theme.fg(inherited ? "dim" : "accent", label);   // bold 在 fg 内层
  ```
- subgroup 行：`truncateToWidth(`    ${theme.fg(color, subgroup.label)}`, width, "")`，
  `color = writeScope === "project" && group.scope === "user" ? "dim" : "muted"`。
- item 行：`truncateToWidth(`${cursor}    ${renderCheckbox(item)} ${name}${getItemSuffix(item)}`, width, "...")`
  - `cursor = isSelected ? "> " : "  "`（**`> ` 两个字符，无颜色**）；
  - `dimmed = isDimmedItem(item)`；
  - `nameText = isSelected && !dimmed ? theme.bold(item.displayName) : item.displayName`；
    `name = dimmed ? theme.fg("dim", nameText) : nameText`；
  - `renderCheckbox(item)`：
    ```ts
    if (writeScope === "project") {
      const state = getProjectOverrideState(item);
      if (state === "load") return theme.fg("success", "[+]");
      if (state === "unload") return theme.fg("warning", "[-]");
      return theme.fg("dim", item.enabled ? "[x]" : "[ ]");      // ← 用 item.enabled（继承后的最终状态）
    }
    return item.enabled ? theme.fg("success", "[x]") : theme.fg("dim", "[ ]");
    ```
  - `getItemSuffix(item)`：
    ```ts
    if (writeScope !== "project") return "";
    const state = getProjectOverrideState(item);
    if (state === "load") return theme.fg("muted", "  project load");
    if (state === "unload") return theme.fg("muted", "  project unload");
    return isInheritedGlobalItem(item) ? theme.fg("dim", "  inherited global") : "";
    ```
  - `isDimmedItem(item)` = `writeScope === "project" && isInheritedGlobalItem(item) &&
    getProjectOverrideState(item) === "inherit"`。
- 滚动行：`theme.fg("dim", `  (${currentItemIndex}/${itemCount})`)`，其中
  `itemCount` 是**过滤后 item 数**，`currentItemIndex = 前 selectedIndex 项中 item 的个数 + 1`。
- 键位（**完整顺序**）：
  ```ts
  tui.select.up        → selectedIndex = findNextItem(sel, -1)       // 只落在 item 上；找不到保持原位
  tui.select.down      → findNextItem(sel, +1)
  tui.select.pageUp    → target = max(0, sel - maxVisible); while(target < len && type !== "item") target++;
                         若 target < len 则 selectedIndex = target
  tui.select.pageDown  → target = min(len-1, sel + maxVisible); while(target >= 0 && type !== "item") target--;
                         若 target >= 0 则 selectedIndex = target
  tui.select.cancel    → onCancel()
  matchesKey(data,"ctrl+c") → onExit()
  tui.input.tab        → onSwitchMode()
  data === " " || tui.select.confirm →
      若 entry 是 item 且（writeScope === "project" || getItemScope(item) === "user"）：
          newEnabled = toggleResource(item); 若 !== undefined → updateItem + onToggle(item, newEnabled)
      return
  否则 → searchInput.handleInput(data); filterItems(searchInput.getValue())
  ```
- **注意**：`pageUp`/`pageDown` 的方向 —— `pageUp` 向上找**下一个** item（向后），`pageDown` 向下找
  **前一个** item（向前）。这是源码的实际行为（与直觉相反），必须照抄。
- `filterItems(query)`：
  - `!query.trim()` → `filteredItems = [...flatItems]` + `selectFirstItem()`；
  - 否则 `lowerQuery = query.toLowerCase()`；
    item 命中：`displayName` / `resourceType` / `path` 任一 `.toLowerCase().includes(lowerQuery)`；
    保留包含命中 item 的 subgroup 与 group（**【已定论】** 源码 `filteredItems` 逐条重建，item 只在
    `matchingItems.has(item)` 时保留 —— 即**只保留命中的 item** 和其祖先头，同组其它 item 不显示）；
    最后 `selectFirstItem()`。
  - `selectFirstItem()`：第一个 `type === "item"` 的索引，没有则 0。
- `toggleResource(item)`：
  ```ts
  if (writeScope === "project") {
    const state = getNextOverrideState(item);
    if (!setProjectResourceOverride(item, state)) return undefined;
    return state === "inherit" ? getInheritedEnabled(item) : state === "load";
  }
  const enabled = !item.enabled;
  item.metadata.origin === "top-level" ? toggleTopLevelResource(item, enabled) : togglePackageResource(item, enabled);
  return enabled;
  ```
- `toggleTopLevelResource(item, enabled)`（**【精读更正】** 这个函数**只处理全局模式**的顶层资源；
  写回**不是**「直接赋值 `settings[arrayKey] = updated`」，而是按 `scope` × `resourceType` 调 8 个
  `SettingsManager` setter 之一；`getResourcePattern` 在这里是**正确**的 helper，**不是**
  `getResourcePatternForScope`）：
  ```ts
  const scope = item.metadata.scope as "user" | "project";
  const settings = scope === "project" ? this.settingsManager.getProjectSettings()
                                       : this.settingsManager.getGlobalSettings();
  const arrayKey = item.resourceType as "extensions" | "skills" | "prompts" | "themes";
  const current = (settings[arrayKey] ?? []) as string[];
  const pattern = this.getResourcePattern(item);       // builtin → item.path；否则 relative(baseDir, path)
  const disablePattern = `-${pattern}`;
  const enablePattern  = `+${pattern}`;
  const updated = current.filter((p) => {
    const stripped = p.startsWith("!") || p.startsWith("+") || p.startsWith("-") ? p.slice(1) : p;
    return stripped !== pattern;
  });
  if (enabled) updated.push(enablePattern); else updated.push(disablePattern);
  if (scope === "project") {
    extensions → setProjectExtensionPaths(updated)
    skills     → setProjectSkillPaths(updated)
    prompts    → setProjectPromptTemplatePaths(updated)
    themes     → setProjectThemePaths(updated)
  } else {
    extensions → setExtensionPaths(updated)
    skills     → setSkillPaths(updated)
    prompts    → setPromptTemplatePaths(updated)
    themes     → setThemePaths(updated)
  }
  ```
  - **没有** `settingsManager.save()` / `write()` 之类的显式落盘调用 —— setter 自己负责。
  - 逐条 `if/else if` 链（**不是** 动态属性赋值），Rust 侧就是 `match (scope, resourceType)` 8 分支。

- `getProjectOverrideState(item)`（**【原文缺失】**；`ResourceList` 的 checkbox/suffix/dimmed 全依赖它）：
  ```ts
  if (this.writeScope !== "project") return "inherit";
  if (item.metadata.origin === "top-level") {
    return this.getOverrideStateFromEntries(
      (this.settingsManager.getProjectSettings()[item.resourceType] ?? []) as string[],
      this.getTopLevelOverridePatterns(item, "project"),
      false,                                    // ← 顶层：空数组 = inherit，不是 unload
    );
  }
  const pkg = this.findMatchingPackageSource(item, "project");
  if (typeof pkg !== "object") return "inherit";
  const entries = pkg[item.resourceType];
  if (entries === undefined) return "inherit";
  return this.getOverrideStateFromEntries(
    entries,
    new Set([this.getPackageResourcePattern(item)]),
    pkg.autoload !== false,                     // ← package：autoload 未显式 false 时空数组 = unload
  );
  ```
- `getOverrideStateFromEntries(entries, patterns, emptyArrayIsUnload)`（**【原文缺失】**，含**后写覆盖**语义）：
  ```ts
  if (entries.length === 0 && emptyArrayIsUnload) return "unload";
  let state: ProjectOverrideState = "inherit";
  for (const entry of entries) {
    if (!patterns.has(this.getPatternEntryTarget(entry))) continue;
    if (entry.startsWith("!") || entry.startsWith("-")) state = "unload";
    else state = "load";                        // 裸 pattern 与 "+pattern" 都算 load
  }
  return state;                                 // 多条命中时以**最后一条**为准
  ```
  **注意**：`!pattern` 与 `-pattern` 都判为 `unload`（`!` 不是「inherit」）；`+pattern` 与裸 `pattern`
  都判为 `load`。这条规则只在 project scope 生效。
- `getInheritedEnabled(item)`（**【原文缺失】**；`inheritedEnabledByKey` 的**三层回退**）：
  ```ts
  return this.inheritedEnabledByKey.get(this.getResourceItemKey(item))
      ?? (this.getItemScope(item) === "user" ? item.enabled : true);
  ```
  —— 表里没有该 key 时：user-scope 资源用自身 `item.enabled`，**project-scope 资源默认 `true`**。
  这是 §2.41 只写「查表」时漏掉的回退。
- `isInheritedGlobalItem(item)` = `getItemScope(item) === "user" || inheritedEnabledByKey.has(getResourceItemKey(item))`
  —— 注意**所有** user-scope 资源都算「继承来的全局项」，不限于表里存在的。
- `buildInheritedEnabledMap(groups)`：遍历 `[group, subgroup, item]` 三层，写入
  `key = `${resourceType}:${canonicalizePath(path)}`` → `item.enabled`（**全局组**的值，构造时快照一次；
  之后 project 侧切换**不会**更新这张表）。
- `updateItem(item, enabled)`（公开）：改 `item.enabled` 后**按 `path + resourceType` 再次查找**同名条目
  （`i.path === item.path && i.resourceType === item.resourceType`）并同步 `found.enabled`，找到即 `return`
  —— 因此**同一路径在不同 group 的副本只有第一份被更新**（平铺列表里的 `item` 与组树里的对象是同一引用时
  无副作用；不同引用时会出现「当前视图已改、重新 build 后回退」的差异，Rust 侧建议直接共用 `Rc<RefCell>`）。
- `setWriteScope(scope)`（公开）：`writeScope = scope; buildFlatList(); filterItems(searchInput.getValue())`
  —— **重建平铺列表会重置 `selectedIndex` 到第一个 item**，再被 `filterItems` 的 `selectFirstItem()` 覆盖。
- `ResourceList` 的 `groups` getter：`get groups() { return this.groupsByScope[this.writeScope]; }`
  —— 所有 `buildFlatList`/`filterItems` 都通过它取当前 scope 的组。
- `getNextOverrideState(item)`（**三态循环，依赖继承状态**）：
  ```ts
  const inheritedEnabled = getInheritedEnabled(item);
  if (state === "inherit") return inheritedEnabled ? "unload" : "load";
  if (state === "unload")  return inheritedEnabled ? "load"   : "inherit";
  /* load */               return inheritedEnabled ? "inherit": "unload";
  ```
- **【逐字】** `  No resources found`、` · inherited global`、`> `、`    `（4 空格 item 缩进）、
  `[+]`、`[-]`、`[x]`、`[ ]`、`  project load`、`  project unload`、`  inherited global`、
  `  (`、`/`、`)`、`Project Local Resources`、`Global Resources`、` · `、`switch mode`、
  `cycle inherit/+/-`、`toggle`、`close`、`.kiana/settings.json · inherited global resources are dimmed`、
  `~/.kiana/agent/settings.json`、`Extensions`、`Skills`、`Prompts`、`Themes`。

#### 2.52.6 `first-time-setup.ts` 的精读更正

**构造函数签名完全不同**（不是 `(onComplete, onCancel, options)`）：

```ts
export interface FirstTimeSetupOptions {
  onThemePreview: (themeName: string) => void;    // 必填
  onSubmit: (result: FirstTimeSetupResult) => void;
  onCancel: () => void;
}
export class FirstTimeSetupComponent extends Container {
  constructor(options: FirstTimeSetupOptions);
}
```

- **不实现 `Focusable`**；`invalidate()` → `this.update(); super.invalidate();`（**先重建再 super**）。
- `update()` **每次重建整个子树**（`clear()` + 重新 addChild）：
  1. `DynamicBorder()`
  2. `Spacer(1)`
  3. `Text(theme.fg("accent", SETUP_LOGO_LINES.join("\n")), 1, 0)` —— **四行 logo 是一个 Text**（`\n` 连接），
     不是四个 Text。
  4. `Spacer(1)`
  5. `Text(theme.fg("accent", theme.bold(`Welcome to ${APP_NAME}, the minimal coding agent.`)), 1, 0)`
     —— 注意**先 bold 再 fg**（与常见写法相反，但输出等价：`\x1b[1m` 包在外）。
  6. `Spacer(1)`
  7. theme 步：`Text(theme.fg("text", "Pick a theme."), 1, 0)` + `Spacer(1)` + 选项行；
     analytics 步：`Text(theme.fg("text", "Opt-in to anonymous usage data sharing?"), 1, 0)` +
     `Text(theme.fg("muted", <四行段落>), 1, 0)` + `Spacer(1)` + 选项行。
  8. `Spacer(1)`
  9. hints `Text(rawKeyHint("↑↓","navigate") + "  " + keyHint("tui.select.confirm",
     step === "theme" ? "continue" : "finish") + "  " + keyHint("tui.select.cancel","skip setup"), 1, 0)`
  10. `Spacer(1)`
  11. `DynamicBorder()`
- 选项行（`addOptionList`）：每项 `Text(`${prefix}${label}`, 1, 0)`，其中
  `prefix = isSelected ? theme.fg("accent","→ ") : "  "`；
  `label = isSelected ? theme.fg("accent", labels[i]) : theme.fg("text", labels[i])`。
- 选择移动**不环绕**（clamp）：
  ```ts
  theme 步：next = clamp(0, 2, themeIndex + delta)；仅当 next !== current 时更新并 onThemePreview(value)
  analytics 步：analyticsIndex = clamp(0, 1, analyticsIndex + delta)
  ```
- 键位：`tui.select.up` **或 `"k"`**、`tui.select.down` **或 `"j"`**、
  `tui.select.confirm` **或 `"\n"`**、`tui.select.cancel`。
- **默认选中**：`themeIndex = 0`（system）、`analyticsIndex = 0`（**true**，即默认高亮 “Share anonymous
  usage data”）。
- 结果：`{ theme: THEME_OPTIONS[themeIndex].value, shareAnalytics: ANALYTICS_OPTIONS[analyticsIndex].value }`。
- 段落**逐字**（4 行，`\n` 分隔，单个 Text）：
  ```
  Opting in stores a tracking identifier in settings.json and enables anonymous
  usage analytics. This helps us to better debug, reproduce, and resolve issues
  and bugs within Kiana. You can observe what is shared using /privacy and make
  changes anytime in settings.json.
  ```
- `SETUP_LOGO_LINES = ["██████", "██  ██", "████  ██", "██    ██"]`；
  `THEME_OPTIONS` 的 system 值是 `SYSTEM_THEME_NAME`（`"system"`）。

### 2.53 对 §2.30 / §2.31 / §2.32 / §2.50 的精读更正（以本节为准）

#### 2.53.1 `model-selector.ts` 的精确行为（更正 §2.30 多处）

**构造**（参数顺序）：

```ts
constructor(
  tui: TUI,
  currentModel: Model<any> | undefined,
  modelRuntime: ModelRuntime,
  scopedModels: ReadonlyArray<ScopedModelItem>,      // { model, thinkingLevel? }
  onSelect: (model: Model<any>) => void,
  onCancel: () => void,
  initialSearchInput?: string,
  onSelectAsDefault?: (model: Model<any>) => void,
  defaultModel?: DefaultModelReference,              // { provider, id }（不是 Model 对象！）
);
```

- **初始 scope**：`scopedModels.length > 0 ? "scoped" : "all"`（§2.30 说是 "scoped"/"all" 但默认未明确）。
- 布局（**paddingX 全部为 0**）：`DynamicBorder()` → `Spacer(1)` → scope 两行（或 warning 一行）→
  `Spacer(1)` → `searchInput` → `Spacer(1)` → `listContainer` → `Spacer(1)` → 可选 hint → `DynamicBorder()`。
- `getScopeText()`（**逐字**）：
  ```ts
  `${theme.fg("muted","Scope: ")}${scope==="all"?theme.fg("accent","all"):theme.fg("muted","all")}` +
  `${theme.fg("muted"," | ")}${scope==="scoped"?theme.fg("accent","scoped"):theme.fg("muted","scoped")}`
  ```
  （未选中的那侧用 **muted**，不是 dim。）
- `getScopeHintText()` = `keyHint("tui.input.tab","scope") + theme.fg("muted"," (all/scoped)")`。
- 无 scoped 时的一行 warning：`Text(theme.fg("warning", "Only showing models from configured providers. Use /login to add providers."), 0, 0)`。
- 可选 hint（有 `onSelectAsDefault` 时）：
  `Text(theme.fg("dim", `  ${confirm} to select · ${save} to set as default · ${cancel} to cancel`), 0, 0)`。
- 构造末尾：
  ```ts
  loadModelsFromSnapshot();
  if (initialSearchInput) filterModels(initialSearchInput); else updateList();
  tui.requestRender();
  void refreshModels();        // ← 总是刷新（不论 initialSearchInput）
  ```
  （§2.30 说「initialSearchInput === undefined 时自动调用」**错误**：**总是**调用。）

**`loadModelsFromSnapshot()`**：

```ts
const models = modelRuntime.getAvailableSnapshot().map(m => ({ provider: m.provider, id: m.id, model: m }));
this.allModels = sortModels(models);
this.scopedModels = this.scopedModels.map(s => { const r = modelRuntime.getModel(s.model.provider, s.model.id);
                                                 return r ? { ...s, model: r } : s; });
this.scopedModelItems = this.scopedModels.map(s => ({ provider: s.model.provider, id: s.model.id, model: s.model }));
this.activeModels = this.scope === "scoped" ? this.scopedModelItems : this.allModels;
this.filteredModels = this.activeModels;
const currentIndex = this.filteredModels.findIndex(item => modelsAreEqual(this.currentModel, item.model));
this.selectedIndex = currentIndex >= 0 ? currentIndex : Math.min(this.selectedIndex, Math.max(0, len - 1));
```

（加载快照后**会**把选中定位到当前模型。）

**`sortModels`**：current 优先 → default 优先 → `a.provider.localeCompare(b.provider)`。
**没有** provider 内的 id 次级排序（依赖 `Array.sort` 稳定 → 保持快照顺序）。

**`refreshModels()`**（注意 `refreshModelCatalogs` 的返回结构是 `{ aborted, errors: Map }`）：

```ts
let timedOut = false;
this.refreshTimeout = setTimeout(() => { timedOut = true; this.refreshAbortController.abort(); }, 15_000);
try {
  const result = await refreshModelCatalogs(this.modelRuntime, this.refreshAbortController.signal);
  if (this.closed) return;
  this.refreshStatusMessage = "";
  if (result.aborted && timedOut) {
    this.errorMessage = "Model refresh timed out; showing cached models.";
  } else if (result.errors.size === 1) {
    this.errorMessage = `Could not refresh ${result.errors.keys().next().value}; showing cached models.`;
  } else if (result.errors.size > 1) {
    this.errorMessage = `Could not refresh ${result.errors.size} model catalogs (${[...result.errors.keys()].join(", ")}); showing cached models.`;
  } else {
    this.errorMessage = this.modelRuntime.getError();          // 可能是 undefined
    if (!this.errorMessage) { this.refreshStatusMessage = "Model catalogs refreshed."; this.refreshStatusSuccess = true; }
  }
  this.loadModelsFromSnapshot();
  this.filterModels(this.searchInput.getValue());
  this.tui.requestRender();
} catch (error) {
  if (this.closed) return;
  this.refreshStatusMessage = "";
  this.errorMessage = timedOut ? "Model refresh timed out; showing cached models."
                              : `Could not refresh model catalogs: ${msg}`;
  this.updateList();
  this.tui.requestRender();
} finally { if (this.refreshTimeout) clearTimeout(this.refreshTimeout); }
```

- **初始 `refreshStatusMessage = "Refreshing model catalogs…"`**（带 `…` U+2026），`refreshStatusSuccess = false`；
  因此列表末尾一开始就有 `  Refreshing model catalogs…`（muted）。
- 失败时 `errorMessage` 显示在**列表之后、行内**（红色，按 `\n` 拆成多行），且 `refreshStatusMessage` 被清空。
- `dispose()`：`closed = true`；清 timeout；`abort()`（幂等，`closed` 后直接返回）。

**`filterModels(query)`**（**总是**用 `fuzzyFilter`，不是 `includes`！）：

```ts
if (query) {
  const filtered = fuzzyFilter(this.activeModels, query, (item) => {
    const defaultText = this.isDefaultModel(item.model) ? " default" : "";
    return `${getModelSelectorSearchText({ id: item.id, provider: item.provider, name: item.model.name })}${defaultText}`;
  });
  if (this.isDefaultSearch(query)) {
    const defaultItems = this.activeModels.filter(item => this.isDefaultModel(item.model));
    const defaultKeys = new Set(defaultItems.map(item => `${item.provider}\0${item.id}`));
    this.filteredModels = [...defaultItems, ...filtered.filter(item => !defaultKeys.has(`${item.provider}\0${item.id}`))];
  } else this.filteredModels = filtered;
} else this.filteredModels = this.activeModels;
this.selectedIndex = query ? 0 : Math.min(this.selectedIndex, Math.max(0, this.filteredModels.length - 1));
this.updateList();
```

- **`isDefaultSearch` 的 effect**：不是「只显示 default」，而是**把 default 模型置顶**，其余模糊匹配结果照常保留。
- `selectedIndex`：有 query → **0**；无 query → clamp 到列表长度（**不**回到 0）。

**`updateList()`**（**paddingX = 0**，行内自带 `  `）：

```ts
const maxVisible = 10;
const startIndex = max(0, min(sel - floor(maxVisible/2), len - maxVisible));
const endIndex = min(startIndex + maxVisible, len);
for (i = startIndex; i < endIndex; i++) {
  const isSelected = i === selectedIndex;
  const isCurrent = modelsAreEqual(currentModel, item.model);
  const isDefault = isDefaultModel(item.model);
  const defaultBadge = isDefault ? theme.fg("muted", " · default") : "";
  const cursor = isSelected ? theme.fg("accent", "→ ") : "  ";
  const currentMarker = isCurrent ? theme.fg("accent", "✓ ") : "  ";
  const modelText = isSelected ? theme.fg("accent", item.id) : item.id;
  const name = getDistinctModelName({ id: item.id, name: item.model.name });
  const nameText = name ? theme.fg("muted", ` (${name})`) : "";
  const providerBadge = theme.fg("muted", `[${item.provider}]`);
  addChild(new Text(`${cursor}${currentMarker}${modelText}${nameText} ${providerBadge}${defaultBadge}`, 0, 0));
}
if (startIndex > 0 || endIndex < len) addChild(new Text(theme.fg("muted", `  (${sel+1}/${len})`), 0, 0));
if (errorMessage) { for (const line of errorMessage.split("\n")) addChild(new Text(theme.fg("error", line), 0, 0)); }
else if (len === 0) addChild(new Text(theme.fg("muted", "  No matching models"), 0, 0));
else {
  addChild(new Spacer(1));
  addChild(new Text(theme.fg("muted", `  Model Name: ${selected.model.name}`), 0, 0));
}
if (refreshStatusMessage) {
  addChild(new Spacer(1));
  addChild(new Text(theme.fg(refreshStatusSuccess ? "success" : "muted", `  ${refreshStatusMessage}`), 0, 0));
}
```

- 滚动指示**在**错误/空/名称行**之前**。
- `selected.model.name` 可能为空串 → 仍输出 `  Model Name: `。
- `getDistinctModelName({ id, name })` 的签名是**单个对象参数**（§2.30 写成 `(model, allModels)` 有误；
  实际实现只看 id 与 name，不需要全量列表）。

**`handleInput`**（**逐字顺序**）：

```ts
1. tui.input.tab → 若 scopedModelItems.length > 0：setScope(scope === "all" ? "scoped" : "all") + 更新 scopeHintText；return
2. tui.select.up → 空列表 return；环绕上移；updateList()
3. tui.select.down → 空列表 return；环绕下移；updateList()
4. tui.select.confirm → 有选中则 handleSelect(model)（dispose + onSelect）
5. tui.select.cancel → dispose() + onCancel()
6. app.models.save（且 onSelectAsDefault 存在）→ 有选中则 dispose() + onSelectAsDefault(model)
7. 其它 → searchInput.handleInput(keyData); filterModels(searchInput.getValue())
```

- `setScope`：切换后 `selectedIndex` 定位到当前模型或 **0**，然后 `filterModels(currentQuery)`，
  并刷新 `scopeText`。
- `getSearchInput()` 暴露搜索框（测试用）。

#### 2.53.2 `scoped-models-selector.ts` 的精确行为（更正 §2.31）

**纯函数（**不是导出**，都是模块私有！）**：

```ts
type EnabledIds = string[] | null;
function isEnabled(enabledIds, id): boolean;                 // null → true
function normalizeEnabled(result: string[], allIds: string[]): EnabledIds;
function toggle(enabledIds, allIds, id): EnabledIds;
function enableAll(enabledIds, allIds, targetIds?): EnabledIds;
function clearAll(enabledIds, allIds, targetIds?): EnabledIds;
function move(enabledIds, id, delta): EnabledIds;            // 越界/未找到 → 原列表副本
function getSortedIds(enabledIds, allIds): string[];
```

源码（**逐字**，注意与 §2.31 的差异）：

```ts
function normalizeEnabled(result, allIds) {
  return result.length === allIds.length && result.every(id => allIds.includes(id)) ? null : result;
}
function toggle(enabledIds, allIds, id) {
  if (enabledIds === null) return allIds.filter(modelId => modelId !== id);   // ← 不加 normalize
  const index = enabledIds.indexOf(id);
  if (index >= 0) return [...enabledIds.slice(0, index), ...enabledIds.slice(index + 1)];
  return normalizeEnabled([...enabledIds, id], allIds);
}
function enableAll(enabledIds, allIds, targetIds?) {
  if (enabledIds === null) return null;                                       // 已全开
  const targets = targetIds ?? allIds;
  const result = [...enabledIds];
  for (const id of targets) if (!result.includes(id)) result.push(id);
  return normalizeEnabled(result, allIds);
}
function clearAll(enabledIds, allIds, targetIds?) {
  if (enabledIds === null) {
    return targetIds ? allIds.filter(id => !targetIds.includes(id)) : [];
  }
  const targets = new Set(targetIds ?? enabledIds);
  return enabledIds.filter(id => !targets.has(id));
}
function move(enabledIds, id, delta) {
  if (enabledIds === null) return null;
  const list = [...enabledIds];
  const index = list.indexOf(id);
  if (index < 0) return list;
  const newIndex = index + delta;
  if (newIndex < 0 || newIndex >= list.length) return list;
  const result = [...list];
  [result[index], result[newIndex]] = [result[newIndex], result[index]];
  return result;
}
function getSortedIds(enabledIds, allIds) {
  if (enabledIds === null) return allIds;                                     // ← 返回原数组（不是副本）
  const enabledSet = new Set(enabledIds);
  return [...enabledIds, ...allIds.filter(id => !enabledSet.has(id))];
}
```

**这些函数不导出**（`export` 只在 class 上）。Rust 侧可 `pub(crate)` 以便单测，但不要进公共 API。

**`ModelsConfig` / `ModelsCallbacks`（逐字，与 §2.31 不同）**：

```ts
export interface ModelsConfig {
  allModels: Model<any>[];
  enabledModelIds: string[] | null;
  refreshStatus?: string;                       // 纯字符串，无 kind
}
export interface ModelsCallbacks {
  onChange: (enabledModelIds: string[] | null) => void | Promise<void>;
  onPersist: (enabledModelIds: string[] | null) => void | Promise<void>;
  onCancel: () => void;
}
```

- `onChange`/`onPersist` 可以是 **async**（返回 Promise）；组件**不 await**（fire-and-forget）。

**构造**：`maxVisible = 8`（**不是 10**）。

布局（**paddingX = 0**）：
`DynamicBorder()` → `Spacer(1)` → `Text(theme.fg("accent", theme.bold("Model Configuration")), 0, 0)` →
`Text(theme.fg("muted", `Session-only. ${keyDisplayText("app.models.save")} to save to settings.`), 0, 0)` →
`Spacer(1)` → `searchInput` → `Spacer(1)` → `listContainer` → `Spacer(1)` →
（`config.refreshStatus` 存在时）`refreshStatusText = Text(theme.fg("muted", `  ${config.refreshStatus}`), 0, 0)` →
`footerText = Text(getFooterText(), 0, 0)` → `DynamicBorder()` → `updateList()`。

**`getFooterText()`**（**逐字**，注意 countText 在**最后**，`(unsaved)` 的细节）：

```ts
const enabledCount = enabledIds?.filter(id => modelsById.has(id)).length ?? allIds.length;
const unavailableCount = enabledIds?.filter(id => !modelsById.has(id)).length ?? 0;
const allEnabled = enabledIds === null;
const countText = allEnabled ? "all enabled"
  : `${enabledCount}/${allIds.length} enabled${unavailableCount ? ` · ${unavailableCount} unavailable` : ""}`;
const parts = [
  `${keyDisplayText("tui.select.confirm")} toggle`,
  `${keyDisplayText("app.models.enableAll")} all`,
  `${keyDisplayText("app.models.clearAll")} clear`,
  `${keyDisplayText("app.models.toggleProvider")} provider`,
  `${keyDisplayText("app.models.reorderUp")}/${keyDisplayText("app.models.reorderDown")} reorder`,
  `${keyDisplayText("app.models.save")} save`,
  countText,
];
return isDirty
  ? theme.fg("dim", `  ${parts.join(" · ")} `) + theme.fg("warning", "(unsaved)")
  : theme.fg("dim", `  ${parts.join(" · ")}`);
```

- **dirty 时 dim 段末尾多一个空格**，`(unsaved)` 在 dim 之外（warning）。

**`updateList()`**（**逐字**）：

```ts
listContainer.clear();
if (len === 0) { addChild(Text(theme.fg("muted", "  No matching models"), 0, 0)); return; }   // ← 提前返回，无滚动/名称行
const startIndex = max(0, min(sel - floor(maxVisible/2), len - maxVisible));
const endIndex = min(startIndex + maxVisible, len);
for (i...) {
  const prefix = isSelected ? theme.fg("accent","→ ") : "  ";
  const id = item.model?.id ?? item.fullId;                    // 有模型用 id（不含 provider），否则 fullId
  const styledId = item.model ? id : theme.strikethrough(id);
  const modelText = isSelected ? theme.fg("accent", styledId) : styledId;
  const name = item.model ? getDistinctModelName(item.model) : undefined;
  const nameText = name ? theme.fg("muted", ` (${name})`) : "";
  const providerBadge = theme.fg("muted", item.model ? ` [${item.model.provider}]` : " [unavailable]");
  const status = item.model && item.enabled ? theme.fg("accent", "✓ ") : "  ";
  addChild(Text(`${prefix}${status}${modelText}${nameText}${providerBadge}`, 0, 0));
}
if (startIndex > 0 || endIndex < len) addChild(Text(theme.fg("muted", `  (${sel+1}/${len})`), 0, 0));
const selected = filteredItems[selectedIndex];
addChild(new Spacer(1));
addChild(Text(theme.fg("muted", `  ${selected.model ? `Model Name: ${selected.model.name}` : "Model unavailable"}`), 0, 0));
```

- **未启用但可用**的模型：`status` 是**两个空格**（不是 `○`）；§2.31 里的 `○` 有误。
- 不可用模型的 `nameText` 为**空**（无 distinct name），只显示 strikethrough fullId + ` [unavailable]`。
- 名称行**总是**显示（列表非空时），不区分选中项是否可用。

**`refresh()`**：

```ts
const query = searchInput.getValue();
const items = buildItems();                    // getSortedIds(enabledIds, allIds).map(...)
this.filteredItems = query
  ? fuzzyFilter(items, query, item => item.model
      ? getModelSearchText({ id: item.model.id, provider: item.model.provider, name: item.model.name })
      : item.fullId)
  : items;
this.selectedIndex = min(selectedIndex, max(0, filteredItems.length - 1));
this.updateList();
this.footerText.setText(this.getFooterText());
```

- 模糊搜索用 `getModelSearchText`（**不含** provider 优先？见 model-search 单元），
  不可用模型只用 `fullId` 作为搜索文本。

**`handleInput`**（**逐字顺序与细节**）：

```ts
1. tui.select.up → 空 return；环绕；updateList()；return
2. tui.select.down → 同上
3. reorderUp / reorderDown：
   if (enabledIds === null) return;                       // 全开时不可重排
   item = filteredItems[selectedIndex];
   if (item && isEnabled(enabledIds, item.fullId)) {
     delta = reorderUp ? -1 : 1;
     currentIndex = enabledIds.indexOf(item.fullId);
     newIndex = currentIndex + delta;
     if (newIndex >= 0 && newIndex < enabledIds.length) {
       enabledIds = move(enabledIds, item.fullId, delta);
       isDirty = true;
       selectedIndex += delta;                            // ← 跟随移动
       refresh(); notifyChange();
     }
   }
   return
4. tui.select.confirm → toggle + isDirty + refresh + notifyChange；return
5. app.models.enableAll → targetIds = query ? filteredItems.map(fullId) : undefined；enableAll(...)；isDirty；refresh；notify；return
6. app.models.clearAll → 同上；return
7. app.models.toggleProvider → 当前项有 model 时：providerIds = allIds 中同 provider 的；
   allEnabled = providerIds.every(isEnabled)；
   enabledIds = allEnabled ? clearAll(..., providerIds) : enableAll(..., providerIds)；isDirty；refresh；notify；return
8. app.models.save → onPersist(enabledIds 副本或 null)；isDirty = false；footerText.setText(getFooterText())；return
9. matchesKey(data, Key.ctrl("c")) → 搜索非空则清空 + refresh，否则 onCancel()；return
10. matchesKey(data, Key.escape) → onCancel()；return
11. 其它 → searchInput.handleInput(data); refresh()
```

- **`refresh()` 里 `updateList` 会重置 `footerText`**，所以第 8 步之后单独再 set 一次（幂等）。
- `updateModels(models, enabledModelIds?)`：
  ```ts
  const selectedId = filteredItems[selectedIndex]?.fullId;
  if (enabledModelIds !== undefined) enabledIds = enabledModelIds === null ? null : [...enabledModelIds];
  // 重建 modelsById / allIds
  refresh();
  const refreshedIndex = selectedId ? filteredItems.findIndex(i => i.fullId === selectedId) : -1;
  if (refreshedIndex >= 0) { selectedIndex = refreshedIndex; updateList(); }
  ```
- `setRefreshStatus(message, kind: "muted" | "success" | "warning")`：
  `refreshStatusText?.setText(theme.fg(kind, `  ${message}`))`（**只在构造时提供了 refreshStatus 才有文本节点**，
  否则是 no-op）。
- `getSearchInput()` 暴露搜索框。
- **【逐字】** `Model Configuration`、`Session-only. `、` to save to settings.`、`  No matching models`、
  `  (`、`/`、`)`、`Model Name: `、`Model unavailable`、` [unavailable]`、`all enabled`、` enabled`、
  ` · `、` unavailable`、` (unsaved)`、`toggle`、` all`、` clear`、` provider`、` reorder`、` save`。

#### 2.53.3 `oauth-selector.ts` 的精确行为（更正 §2.32）

- **`formatAuthSelectorProviderType` 的签名是两个参数**：
  ```ts
  export function formatAuthSelectorProviderType(
    authType: "oauth" | "api_key",
    subscription?: boolean,
  ): string {
    if (authType === "api_key") return "API key";
    return subscription === false ? "account" : "subscription";
  }
  ```
  （不是接收整个 provider。）
- `formatAuthSelectorProviderStatus`：
  ```ts
  if (!provider.status) return theme.fg("muted", " • not configured");
  if (provider.status.type !== provider.authType) {
    const label = `${formatAuthSelectorProviderType(provider.status.type, provider.subscription)} configured`;
    return theme.fg("muted", " • ") + theme.fg("warning", label);
  }
  if (!provider.status.source || provider.status.source === "OAuth" || provider.status.source === "stored credential") {
    return theme.fg("success", " ✓ configured");
  }
  const source = /^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$/.test(provider.status.source)
    ? `env: ${provider.status.source}` : provider.status.source;
  return theme.fg("success", ` ✓ ${source}`);
  ```
  - **类型不匹配时的 label 用的是 `formatAuthSelectorProviderType(status.type, subscription)`**，
    即对 `oauth` status 会得到 `subscription`/`account`，对 `api_key` status 得到 `API key`。
    （§2.32 里写的「`OAuth` / `API key`」有误。）
- **`onSelect` 回调签名是两个参数**：`(providerId: string, authType: "oauth" | "api_key") => void`。
- 构造：`filterProviders(initialSearchInput ?? "")`（**初始就过滤**）；`selectedIndex` 初值 0。
- 布局（paddingX 见下）：
  `DynamicBorder()` → `Spacer(1)` → `TruncatedText(theme.fg("accent", theme.bold(title)), 1, 0)` →
  `Spacer(1)` → `searchInput` → `Spacer(1)` → `listContainer` → `Spacer(1)` → `DynamicBorder()`。
  列表行全部用 `TruncatedText(line, 1, 0)`（**paddingX = 1**）。
- `updateList()`：
  - `maxVisible = 8`；
  - `authTypeLabel = showAuthTypeLabels ? theme.fg("muted", ` [${formatAuthSelectorProviderType(provider.authType, provider.subscription)}]`) : ""`；
  - 选中：`theme.fg("accent","→ ") + theme.fg("accent", name) + authTypeLabel + status`；
  - 未选中：`` `  ${theme.fg("text", name)}` `` + authTypeLabel + status；
  - 滚动：`TruncatedText(theme.fg("muted", `  (${sel+1}/${len})`), 1, 0)`；
  - 空消息：`theme.fg("muted", `  ${message}`)` 包成 `TruncatedText(..., 1, 0)`。
- `handleInput`：up/down **clamp**（不环绕，注意与 model-selector 的**环绕**相反）；
  confirm → `onSelectCallback(id, authType)`；cancel → onCancel；其它 → 搜索 + `filterProviders`。
- `filterProviders`：过滤后 `selectedIndex = max(0, min(sel, max(0, len-1)))`（clamp，不重置到 0）。
- **【逐字】** `Select provider to configure:`、`Select provider to logout:`、` • not configured`、` • `、
  ` configured`、` ✓ configured`、` ✓ `、`env: `、`API key`、`account`、`subscription`、
  `No providers available`、`No providers logged in. Use /login first.`、`No matching providers`、
  `  `、`→ `、` [`、`]`。

#### 2.53.4 `thinking-selector.ts` 的补充

- **`getSelectList()` 是公开方法**（§2.50 未列）：返回当前 `selectList`（过滤后是新实例）。
- 构造里 `list.onSelect = item => onSelect(item.value as ThinkingLevel)`、
  `list.onCancel = () => onCancel()`（`onCancel` 直接透传，**不**先 dispose）。
- **`handleInput` 的 nav 集合是 4 个**（up/down/confirm/cancel），**不含** pageUp/pageDown（§2.50 正确）。
- 过滤用 `fuzzyFilter`，**保留选中值**（`buildSelectList(filtered, selectedValue)`）。
- 所有 `Text` 的 `paddingX = 0`、`paddingY = 0`。

### 2.54 对 §2.11 / §2.9 / §2.19 / §2.20 / §2.23 / §2.25 / §2.26 / §2.37 / §2.22 / §2.12 / §2.10 的精读更正

以下均为逐行核对源码后的**最终结论**，与前面小节冲突时以本节为准。

#### 2.54.1 `theme-selector.ts`

- `THEME_SELECT_LIST_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 }`。
- items：`label` **就是主题名本身**（不带 `✓ ` 前缀）；只有 `description` 是 `"(current)"`。
- 构造顺序：`DynamicBorder()` → `SelectList(items, 10, getSelectListTheme(), LAYOUT)` →
  `setSelectedIndex(themes.indexOf(currentTheme))`（仅 `!== -1`）→ **先设 `onSelect`/`onCancel`，
  再设 `onSelectionChange`** → `DynamicBorder()`。
- **无 Spacer**。`getSelectList()` 公开。

#### 2.54.2 `show-images-selector.ts`

- `SHOW_IMAGES_SELECT_LIST_LAYOUT = {12, 32}`；`SelectList(items, 5, ...)`。
- 顺序：`DynamicBorder()` → `SelectList(...)` → `setSelectedIndex(currentValue ? 0 : 1)` → 回调 → `DynamicBorder()`。
- **无 Spacer**；`getSelectList()` 公开。
- items 逐字：`Yes` / `Show images inline in terminal`，`No` / `Show text placeholder instead`。

#### 2.54.3 `extension-input.ts`（更正 §2.19）

- `ExtensionInputOptions` 是**导出接口**：`{ tui?: TUI; timeout?: number; initialValue?: string; description?: string }`。
- `_placeholder` **完全未使用**（参数保留但忽略）。**Rust 侧也应接受并忽略**（不要传给 Input）。
- `focused` 代理到 `input`。
- 顺序：`DynamicBorder()` → `Spacer(1)` → `titleText = Text(theme.fg("accent", title), 1, 0)` →
  （description：`Spacer(1)` + `Text(theme.fg("text", description), 1, 0)`）→ `Spacer(1)` →
  **倒计时创建（在 input 之前）** → `input`（`initialValue` 时 `setValue`）→ `Spacer(1)` →
  hints `Text(`${keyHint("tui.select.confirm","submit")}  ${keyHint("tui.select.cancel","cancel")}`, 1, 0)` →
  `Spacer(1)` → `DynamicBorder()`。
- 倒计时条件：`opts?.timeout && opts.timeout > 0 && opts.tui`（`timeout = 0` 不启用）。
  回调：`titleText.setText(theme.fg("accent", `${baseTitle} (${s}s)`))`；`onTick` 首次立即执行。
- 取消回调**直接**是 `onCancel`（不经宿主包装）。
- `handleInput`：`console` 顺序 `confirm || keyData === "\n"` → `onSubmit(input.getValue())`；
  `cancel` → `onCancel()`；否则 `input.handleInput`。
- `dispose()`：`countdown?.dispose()`（无其它）。

#### 2.54.4 `extension-selector.ts`（更正 §2.23 —— 关键：**不环绕**）

- `ExtensionSelectorOptions` 是导出接口：`{ tui?; timeout?; onToggleToolsExpanded?; description? }`。
- **不实现 `Focusable`**（无 `focused`）。
- `onSelect` 回调只有**一个**参数（`option: string`），**没有 index**。
- 标题：`Text(theme.fg("accent", theme.bold(title)), 1, 0)`（accent 在外、bold 在内）。
  倒计时：`titleText.setText(theme.fg("accent", theme.bold(`${baseTitle} (${s}s)`)))`。
- 顺序：`DynamicBorder()` → `Spacer(1)` → title →（description：`Spacer(1)` + `Text(fg("text",desc),1,0)`）→
  `Spacer(1)` → 倒计时创建 → `listContainer` → `Spacer(1)` → hints → `Spacer(1)` → `DynamicBorder()` →
  `updateList()`。
- 列表行：选中 `theme.fg("accent","→ ") + theme.fg("accent", option)`；未选中 `` `  ${theme.fg("text", option)}` ``；
  都是 `Text(..., 1, 0)`。
- **`handleInput` 的 up/down 是 clamp（不环绕）**：
  ```ts
  up:   selectedIndex = Math.max(0, selectedIndex - 1)
  down: selectedIndex = Math.min(options.length - 1, selectedIndex + 1)
  ```
  （§2.23 写的 `% n` 环绕**错误**。）
- 顺序：`app.tools.expand`（先，且不影响选中）→ `up || "k"` → `down || "j"` → `confirm || "\n"` →
  `cancel`。
- `dispose()`：`countdown?.dispose()`。

#### 2.54.5 `radius-login-selector.ts`（更正 §2.22 —— 签名与实现差异较大）

- **`radiusShimmer` 不导出**（模块私有）；`RadiusLoginMenuComponent` 也不导出。
  唯一导出是 `createLoginMenuSelector`。
- `RadiusOption = { label: string; text: string }`；`createLoginMenuSelector` 的参数是
  **`options: string[]` 与独立的 `radiusOption: RadiusOption`**（不是把 radiusOption 追加进 options）。
  `onSelect: (option: string) => void`（**单参数**，无 index）。
- `radiusShimmer(text, elapsedMs)`：
  ```ts
  const cycle = RADIUS_COLORS.length * CHARS_PER_COLOR;        // 16
  const offset = (elapsedMs / 1000) * CHARS_PER_SECOND;        // 10 字符/秒
  let result = "", index = 0;
  for (const char of text) {                                   // ← for...of = 按 Unicode 码点！
    const position = (((index - offset) % cycle) + cycle) % cycle;
    const band = Math.floor(position / CHARS_PER_COLOR);
    const t = position / CHARS_PER_COLOR - band;
    const amount = t * t * (3 - 2 * t);
    const from = RADIUS_COLORS[band];
    const to = RADIUS_COLORS[(band + 1) % RADIUS_COLORS.length];
    result += foregroundAnsi(mixColors(from, to, amount, "srgb"), mode) + char;
    index++;
  }
  return `${result}\x1b[39m`;
  ```
  **`for...of` 按码点迭代**（不是 UTF-16 code unit）→ Rust 的 `chars()` **正确**。
- `RadiusLoginMenuComponent` 构造：
  ```ts
  super(tui, title, options, (option) => { this.stopAnimation(); onSelect(option); },
                              () => { this.stopAnimation(); onCancel(); });
  this.radiusOption = radiusOption;
  this.animationTimer = setInterval(() => { if (this.animating) tui.requestRender(); }, 50);
  this.animationTimer.unref?.();
  ```
  （无 timeout 参数。）
- `render(width)`：
  ```ts
  const lines = super.render(width);
  const { label, text } = this.radiusOption;
  const selectedLine = new Text(theme.fg("accent", "→ ") + theme.fg("accent", label), 1, 0).render(width)[0];
  const index = selectedLine === undefined ? -1 : lines.indexOf(selectedLine);
  this.animating = index >= 0;
  if (this.animating) {
    const shimmer = radiusShimmer(text, performance.now() - this.animationStart);
    const animatedLine = theme.fg("accent", "→ ") + shimmer + label.slice(text.length);
    lines[index] = new Text(animatedLine, 1, 0).render(width)[0] ?? "";
  }
  return lines;
  ```
  - 匹配用**完整渲染串相等**（依赖 Text 输出稳定）；`indexOf` 找**第一次出现**。
  - 动画行 = `→ `（accent） + 微光的 `text` 部分 + `label` 的剩余后缀（**原样，不加色**）。
- `stopAnimation()`：`clearInterval` + 置 `undefined` + `animating = false`。
- `dispose()`：`stopAnimation()` 再 `super.dispose()`。
- `animationStart = performance.now()` 在**构造时**记录。

#### 2.54.6 `bordered-loader.ts`（更正 §2.12）

- 构造：`borderColor = (s) => theme.fg("border", s)`（**来自构造参数 `theme`，不是全局**）。
- 子组件顺序：`DynamicBorder(borderColor)` → loader →（cancellable 时）`Spacer(1)` +
  `Text(keyHint("tui.select.cancel","cancel"), 1, 0)` → `Spacer(1)` → `DynamicBorder(borderColor)`。
- `CancellableLoader(tui, accent, muted, message)` / `Loader(tui, accent, muted, message)`（spinner accent、
  message muted）。
- `signal`：cancellable → `loader.signal`；否则 `signalController?.signal ?? new AbortController().signal`
  （**每次访问可能返回新 signal**，若 `signalController` 意外为空 —— 实际不会发生）。
- `onAbort` setter 仅 cancellable 时透传。
- `handleInput` 仅 cancellable 时转发。
- `dispose()`：`"dispose" in loader && typeof === "function"` → `dispose()`；否则 `"stop" in loader` → `stop()`。

#### 2.54.7 `earendil-announcement.ts`（更正 §2.10 —— 无 Spacer 在图片前）

- 顺序：`DynamicBorder(accent)` → `Text(theme.bold(theme.fg("accent","kiana has joined Earendil")), 1, 0)` →
  `Spacer(1)` → `Text(theme.fg("muted","Read the blog post:"), 1, 0)` →
  `Text(theme.fg("mdLink", BLOG_URL), 1, 0)` → `Spacer(1)` →
  （图片：`Image(base64,"image/png",{fallbackColor: muted},{maxWidthCells:56, filename:"clankolas.png"})` +
  `Spacer(1)`）→ `DynamicBorder(accent)`。
- `loadImageBase64()`：`attemptedImageLoad` 置 true 在 try **之前**；失败 → `undefined`；之后不再重试。
- `fs.readFileSync(...).toString("base64")`（标准 base64，无换行）。
- **图片前没有额外 Spacer**（`Spacer(1)` 在 URL 之后统一加，图片后再加一个）。

#### 2.54.8 `user-message-selector.ts`（更正 §2.37）

- `UserMessageItem = { id: string; text: string; timestamp?: string }`（**不是** `UserMessage`；
  由宿主预先投影成纯文本）。
- `UserMessageList` **不导出**；`UserMessageSelectorComponent.getMessageList()` 返回它（类型在 TS 里可用但
  类不导出）。
- `maxVisible = 10` 硬编码。
- 初始选中：`initialSelectedId` 命中则用其索引，否则 **`Math.max(0, length - 1)`（最后一条 = 最新）**。
- `handleInput` **不重建**（只改 `selectedIndex`；`render` 每次重算）。
- 键位：up 环绕到 `length-1`；down 环绕到 0；confirm → `onSelect(id)`；cancel → `onCancel()`。
- 布局（**注意有 Spacer 在顶部、无 description 之外的间隔**）：
  `Spacer(1)` → `Text(theme.bold("Fork from Message"), 1, 0)` →
  `Text(theme.fg("muted","Select a user message to copy the active path up to that point into a new session"), 1, 0)` →
  `Spacer(1)` → `DynamicBorder()` → `Spacer(1)` → `messageList` → `Spacer(1)` → `DynamicBorder()`。
- **【再次更正 · 本条原先写反了】** **有** 100 ms 自动取消：`UserMessageSelectorComponent` 构造末尾
  `if (messages.length === 0) setTimeout(() => onCancel(), 100);`（与 tree-selector 空树、§2.37 一致）。
  `UserMessageList` 本身不自动取消，只渲染 `theme.fg("muted", "  No user messages found")`（单行）。
- 每消息 3 行：`cursor + body`、`theme.fg("muted", `  Message ${i+1} of ${N}`)`、`""`。
- 滚动行在**最后**：`theme.fg("muted", `  (${sel+1}/${N})`)`（muted，不是 dim）。

#### 2.54.9 `trust-selector.ts`（更正 §2.25 —— 关键差异）

- `TrustSelection = Pick<ProjectTrustOption, "trusted" | "updates">`（导出类型）。
- **`formatDecision` 不导出**（模块私有）。
- **`getProjectTrustOptions(options.cwd)` 只用 cwd 一个参数**（**没有** `{ includeSessionOnly: true }`）。
- `savedDecision` 语义：
  ```ts
  this.savedDecision = options.savedDecision;                       // null = 无
  this.trustOptions = getProjectTrustOptions(options.cwd);
  this.selectedIndex = Math.max(0, trustOptions.findIndex(o => this.isSavedOption(o)));
  ```
  `isSavedOption(option)`：
  ```ts
  option.savedPath !== undefined &&
  this.savedDecision?.decision === option.trusted &&
  this.savedDecision.path === option.savedPath
  ```
- 「Saved decision:」行用的是 **`trustOptions[0]?.savedPath`**（第一个选项的 savedPath）作为 `trustPath`
  去调 `formatDecision`：
  ```ts
  `Saved decision: ${formatDecision(this.trustOptions[0]?.savedPath, options.savedDecision)}`
  ```
- `formatDecision`：
  ```ts
  if (decision === null) return "none";
  const label = decision.decision ? "trusted" : "untrusted";
  if (trustPath !== undefined && decision.path !== trustPath) return `${label} (inherited from ${decision.path})`;
  return `${label} (${decision.path})`;
  ```
  （`decision` 为 `undefined` 时**会**访问 `.decision` 抛错 —— 但 `trustOptions[0]?.savedPath` 与
  `options.savedDecision` 都是 `null | 对象`，`null` 走第一支；安全。）
- **行渲染与 §2.25 不同**：选中项 label 用 **accent**（未选中 `text`）：
  ```ts
  const prefix = isSelected ? theme.fg("accent", "→ ") : "  ";
  const currentMarker = isCurrent ? theme.fg("accent", "✓ ") : "  ";
  const label = isSelected ? theme.fg("accent", option.label) : theme.fg("text", option.label);
  Text(`${prefix}${currentMarker}${label}`, 1, 0)
  ```
- 键位 up/down **clamp**（`Math.max(0, i-1)` / `Math.min(len-1, i+1)`），**不环绕**。
- 顺序：`DynamicBorder()` → `Spacer(1)` → title（accent+bold）→ `Text(theme.fg("muted", cwd), 1, 0)` →
  `Spacer(1)` → Saved decision → Current session → `Spacer(1)` → listContainer → `Spacer(1)` → hints →
  `Spacer(1)` → `DynamicBorder()`。

#### 2.54.10 `extension-editor.ts`（更正 §2.26 —— 签名）

```ts
export interface ExtensionEditorOptions extends EditorOptions { description?: string }
constructor(
  tui: TUI,
  keybindings: KeybindingsManager,            // ← 额外的第 2 参数
  title: string,
  prefill: string | undefined,
  onSubmit: (value: string) => void,
  onCancel: () => void,
  options?: ExtensionEditorOptions,
  externalEditorCommand?: string,             // ← 第 8 个参数（不是 options 字段）
);
```

- `externalEditorCommand = externalEditorCommand || process.env.VISUAL || process.env.EDITOR ||
  (process.platform === "win32" ? "notepad" : "nano")`。
- `const { description, ...editorOptions } = options ?? {}` → `new Editor(tui, getEditorTheme(), editorOptions)`。
- 布局：`DynamicBorder()` → `Spacer(1)` → `Text(theme.fg("accent", title), 1, 0)` →
  （description：`Spacer(1)` + `Text(theme.fg("text", description), 1, 0)`）→ `Spacer(1)` → `editor` →
  `Spacer(1)` → hint → `Spacer(1)` → `DynamicBorder()`。
- hint（**逐字拼接**）：`keyHint("tui.select.confirm","submit") + "  " +
  keyHint("tui.input.newLine","newline") + "  " + keyHint("tui.select.cancel","cancel") + `  ${keyHint("app.editor.external","external editor")}``。
- `handleInput`：`tui.select.cancel` → onCancel（**先**）；`keybindings.matches(keyData,
  "app.editor.external")`（用**注入的 keybindings**，不是全局 `getKeybindings()`）→ 异步外部编辑器；
  否则 `editor.handleInput`。
- `handleOpenExternalEditor()`：
  ```ts
  const content = this.editor.getText();
  this.tui.stop();
  try {
    const result = await editInExternalEditor({ command: this.externalEditorCommand, content });
    if (result.status === "complete") this.editor.setText(result.content);
  } finally { this.tui.start(); this.tui.requestRender(true); }
  ```
  （`editInExternalEditor` 返回 `{ status: "complete" | ... , content }`，**不是** `kind`。）
- `focused` 代理到 `editor`。
- **无 `dispose()`**（编辑器无需清理）。

#### 2.54.11 `user-message.ts` / `assistant-message.ts` 的 `invalidate` 细节

- **`UserMessageComponent.invalidate()`：源码里根本没有这个方法**（grep 无 `invalidate`），
  Container 默认实现只转发给子组件（`Markdown` 自己会重跑 `transform`）。**【更正】** 旧稿在这里写
  「确认有 `super.invalidate(); this.rebuild();`」是**错的**（与 §2.17 的正确结论矛盾，现统一为「没有」）。
- `AssistantMessageComponent.invalidate()`：
  ```ts
  super.invalidate();
  if (this.lastMessage) this.updateContent(this.lastMessage);
  ```
  **【已定论】** `thinkingRunIndex` **不是**组件字段：`updateContent` 开头是
  `let thinkingRunIndex = 0;`（**局部变量**），因此每次重建（含 `invalidate` 触发的重建）都从 0 开始，
  `thinkingVisibilityOverrides` 的键**稳定**。§2.38 的「跨 updateContent 持续」**错误**，以本处为准。

### 2.55 对 §2.38 / §2.39 / §2.17 / §2.24 的精读更正（最终版，以此为准）

#### 2.55.1 `assistant-message.ts`（逐行核对后的权威描述）

```ts
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
}
```

- **`thinkingRunIndex` 是 `updateContent` 内的局部变量**（`let thinkingRunIndex = 0;` 在循环前），每次重建
  从 0 开始 → `thinkingVisibilityOverrides: Map<usize,bool>` 的键**跨重建稳定**。
- **thinking 运行的收集方式（细节）**：
  ```ts
  } else if (content.type === "thinking") {
    const thinkingBlocks: string[] = [];
    for (; i < message.content.length; i++) {
      const thinkingContent = message.content[i];
      if (thinkingContent.type !== "thinking") break;
      const thinking = thinkingContent.thinking.trim();
      if (thinking) thinkingBlocks.push(thinking);
    }
    i--;                                     // ← 回退，让外层 for 的 i++ 指向非 thinking 块
    if (thinkingBlocks.length === 0) continue;   // 空 thinking run：不产生组件，也不消耗 runIndex
    const hasVisibleContentAfter = message.content.slice(i + 1)
      .some(c => (c.type === "text" && c.text.trim()) || (c.type === "thinking" && c.thinking.trim()));
    const runIndex = thinkingRunIndex++;
    const hidden = this.thinkingVisibilityOverrides.get(runIndex) ?? this.hideThinkingBlock;
    ...
    if (hasVisibleContentAfter) this.contentContainer.addChild(new Spacer(1));
  }
  ```
  - **空 run 不递增 runIndex**（`continue` 在 `runIndex` 之前）。
  - `hasVisibleContentAfter` 只检查 `text`/`thinking`（**不**检查 `toolCall`），所以 thinking 后紧跟
    toolCall 时**不加** Spacer。
- `MouseRegion` 的点击处理：
  ```ts
  this.thinkingVisibilityOverrides.set(runIndex, !hidden);   // ← 用捕获的 hidden，不是重新读取
  if (this.lastMessage) this.updateContent(this.lastMessage);
  ```
- 收尾（**逐字**）：
  - `hasToolCalls` 在遍历**后**计算并写入 `this.hasToolCalls`（`render` 用该字段，不是每次重算）。
  - `stopReason === "length"` → `Spacer(1)` + `Text(fg("error","Response was truncated before completion."), outputPad, 0)`。
  - `else if (!hasToolCalls)`：
    - `"aborted"`：`Spacer(1)` + `Text(fg("error", abortMessage), outputPad, 0)`，其中
      `abortMessage = errorMessage && errorMessage !== "Request was aborted" ? errorMessage : "Operation aborted"`。
      **`Spacer(1)` 在 if 分支内**（有 toolCalls 时不加）。
    - `"error"`：`Spacer(1)` + `Text(theme.fg("error", `Error: ${errorMsg}`), outputPad, 0)`，
      `errorMsg = errorMessage || "Unknown error"`（空串也走默认）。
- `render` 使用字段 `this.hasToolCalls`（不是重新计算）：
  ```ts
  const lines = super.render(width);
  if (this.hasToolCalls || lines.length === 0) return lines;
  lines[0] = OSC133_ZONE_START + lines[0];
  lines[lines.length-1] = OSC133_ZONE_END + OSC133_ZONE_FINAL + lines[lines.length-1];
  ```
- `setHideThinkingBlock(hide)`：**无条件** `thinkingVisibilityOverrides.clear()`，然后 `if (lastMessage)
  updateContent(lastMessage)`。
- `setHiddenThinkingLabel` / `setOutputPad`：同样 `if (lastMessage)` 守卫。
- 默认 `markdownTheme = getMarkdownTheme()`（**默认参数求值时会调用全局函数**，Rust 侧用 `Option` 或
  `impl Fn` 惰性获取）。
- `updateContent` 的 `isStreaming` 默认值是**当前字段**（`isStreaming = this.isStreaming`），因此
  `invalidate()` 调用 `updateContent(lastMessage)` 会保持上一次的流式状态。

#### 2.55.2 `bash-execution.ts`（逐行核对）

- 字段 `status: "running" | "complete" | "cancelled" | "error"`（**单一状态字段**，不是每次从 exitCode 推导）。
- 构造：`Spacer(1)` → `DynamicBorder(borderColor)` → `contentContainer` → `DynamicBorder(borderColor)`；
  **loader 在构造时就创建**（`new Loader(ui, spinner=colorKey, message=muted, `Running... (${keyText}
  to cancel)`)`），但**只在 `status === "running"` 时加入** contentContainer（`updateDisplay` 末尾）。
  `loader.stop()` 在 `setComplete` 中调用。
- `appendOutput(chunk)` **末尾会调用 `updateDisplay()`**（§2.39 说「不重建」**错误**）。
  精确逻辑：
  ```ts
  const clean = stripAnsi(chunk).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const newLines = clean.split("\n");
  if (this.outputLines.length > 0 && newLines.length > 0) {
    this.outputLines[this.outputLines.length - 1] += newLines[0];
    this.outputLines.push(...newLines.slice(1));
  } else {
    this.outputLines.push(...newLines);
  }
  this.updateDisplay();
  ```
  （`newLines.length > 0` 恒真，因为 `split` 至少返回 1 个元素。）
- `setComplete`：`exitCode` 存入；`status = cancelled ? "cancelled" : (exitCode !== 0 && exitCode !== undefined
  && exitCode !== null) ? "error" : "complete"`；`loader.stop()`；`updateDisplay()`。
- `updateDisplay()`（**注意 `availableLines` 的空串处理**）：
  ```ts
  const availableLines = contextTruncation.content ? contextTruncation.content.split("\n") : [];
  const previewLogicalLines = availableLines.slice(-20);
  const hiddenLineCount = availableLines.length - previewLogicalLines.length;
  ```
- **header 前不加换行**：`Text(theme.fg(colorKey, theme.bold(`$ ${command}`)), outputPad, 0)` 直接加入。
- **expanded / collapsed 的 `\n` 前缀**：
  - expanded：`new Text(`\n${displayText}`, outputPad, 0)`；
  - collapsed：`styledInput = `\n${styledOutput}``，再交给 `truncateToVisualLines(styledInput, 20, width,
    outputPad)` —— 也就是说**折叠预览的第 1 行是空行**。
  - 因此两种模式都在 header 与输出之间有一个空行。
- **statusParts 的 `hiddenLineCount > 0` 分支**：
  ```ts
  expanded ? `${muted("(")}${keyHint("app.tools.expand","to collapse")}${muted(")")}`
           : `${muted(`... ${hiddenLineCount} more lines (`)}${keyHint("app.tools.expand","to expand")}${muted(")")}`
  ```
- 状态行：`new Text(`\n${statusParts.join("\n")}`, outputPad, 0)`。
- `loader` 只在 running 时加入；非 running 时**不再**加入（contentContainer 被 clear）。
- `wasTruncated = truncationResult?.truncated || contextTruncation.truncated` → 与 `fullOutputPath` 同时存在才显示。
- **`invalidate()` → `super.invalidate(); updateDisplay();`**（重建以重放颜色；折叠预览的匿名组件缓存
  通过 `super.invalidate()` 传播失效）。
- `getOutput()` 返回 `outputLines.join("\n")`（**已剥 ANSI、已归一换行**）。
- `getCommand()` 返回原文。

#### 2.55.3 `user-message.ts`（最终）

- **没有 `invalidate()` 覆写**（`Container.invalidate()` 会传播给 Markdown，Markdown 会重渲染并重放
  transform；`user-message.test.ts` 的「reapplies Markdown transformers when invalidated」正是依赖这一点）。
- 字段：`text`、`markdownTheme`、`outputPad`、`markdownTransformers`、`messageId`、`timestamp`。
- 构造：`super()` → 存字段 → `rebuild()`。
- `rebuild()`：`clear()` → 加 **一个** `Markdown(text, outputPad, 1, markdownTheme, {color userMessageText,
  bgColor userMessageBg}, {preserveOrderedListMarkers: true, preserveBackslashEscapes: true,
  transform: createMarkdownTransform("user", false, ...)})`。**没有 Box**（注释说明 Markdown 自己处理背景）。
- `render`：`lines.length === 0` 直接返回；否则首行前缀 `A`、末行前缀 `B` + `C`。
- `setOutputPad(pad)`：存值 + `rebuild()`（**不**保留旧组件）。

#### 2.55.4 `status-indicator.ts`（逐行核对）

```ts
export type StatusIndicatorKind = "working" | "retry" | "compaction" | "branchSummary";

export class StatusIndicator extends Loader {
  readonly kind: StatusIndicatorKind;
  constructor(kind, ui: TUI, spinnerColorFn, messageColorFn, message: string,
              indicator?: WorkingIndicatorOptions);
  renderInBorder(width: number): string;
  renderSpinnerInBorder(width: number): string;
  dispose(): void;                       // this.stop()
}
```

- **构造签名是 `(kind, ui, spinnerColorFn, messageColorFn, message, indicator?)`**（kind 在前）。
- `WorkingStatusIndicator(ui, message, indicator?: WorkingIndicatorOptions, colorFn?: (text)=>string)`：
  **`indicator` 在 `colorFn` 之前**（§2.24 的顺序相反，必须更正）。
  - 当 `colorFn` 提供时，**spinner 与 message 都用同一个 `colorFn`**（不是只给 spinner）。
  - 未提供：spinner `accent`、message `muted`。
- `RetryStatusIndicator(ui, attempt, maxAttempts, delayMs)`：
  - 初始 message = `retryMessage(Math.ceil(delayMs / 1000))`；
  - spinner `warning`、message `muted`；
  - `CountdownTimer(delayMs, ui, seconds => setMessage(retryMessage(seconds)), () => { countdown = undefined; })`
    —— **到期回调只置空引用，不调用 dispose**（内部 timer 已在 `dispose()` 前自清）。
  - `retryMessage(s) = `Retrying (${attempt}/${maxAttempts}) in ${s}s... (${keyText("app.interrupt")} to cancel)``。
  - `dispose()`：`countdown?.dispose(); countdown = undefined; super.dispose();`
- `CompactionStatusIndicator(ui, reason, colorFn?)`：
  - `cancelHint = `(${keyText("app.interrupt")} to cancel)``；
  - `label = reason === "manual" ? `Compacting context... ${cancelHint}`
    : `${reason === "overflow" ? "Context overflow detected, " : ""}Auto-compacting... ${cancelHint}``；
  - spinner `accent`（或 colorFn）、message `muted`（或 colorFn）。
- `BranchSummaryStatusIndicator(ui)`：`Summarizing branch... (${keyText} to cancel)`；accent/muted。
- `IdleStatus implements Component`：`invalidate()` 空；`render(width)` 返回 **同一个字符串实例两次**
  （`const emptyLine = " ".repeat(width); return [emptyLine, emptyLine]`）。
- **所有子类都经 `super(ui, spinnerColorFn, messageColorFn, message, indicator)` 传参**，`indicator` 只有
  Working 传（其它为 undefined）。
- `renderInBorder` / `renderSpinnerInBorder` 见 §2.24（`super.render(width + 2)[1] ?? ""`；去掉首空格 +
  `trimEnd`；`truncateToWidth(..., width, "")`）。

---

### 2.56 `custom-editor.ts` 的精读更正（以本节为准）

```ts
export type CustomEditorOptions = EditorOptions & {
  embedWorkingStatus?: boolean;
};

export class CustomEditor extends Editor {
  public readonly embedWorkingStatus: boolean;         // 构造后不可变
  public actionHandlers: Map<AppKeybinding, () => void>;
  public onEscape?: () => void;
  public onCtrlD?: () => void;
  public onPasteImage?: () => void;
  public onExtensionShortcut?: (data: string) => boolean;

  constructor(tui: TUI, theme: EditorTheme, keybindings: KeybindingsManager, options?: CustomEditorOptions);
  setWorkingStatusIndicator(indicator: StatusIndicator | undefined): void;
  onAction(action: AppKeybinding, handler: () => void): void;
  protected override renderTopBorder(width: number, hiddenLineCount: number): string;
  override handleInput(data: string): void;
}
```

- **`keybindings` 是构造注入的**（不是全局 `getKeybindings()`）；所有匹配都走
  `this.keybindings.matches(...)`。
- `borderColor` 来自 `EditorTheme`（**父类字段** `public borderColor: (str) => string`，由
  `editorTheme.borderColor` 初始化）。宿主可随时改写（主题/思考等级变化）。
  因此 `renderTopBorder` 里的 `this.borderColor(...)` 是**父类字段**，不是自定义方法。
- `embedWorkingStatus = options?.embedWorkingStatus ?? false`；其它 `options` 透传给 `Editor`。
- **`setWorkingStatusIndicator` 不调用 `invalidate()`**（只赋值；下一次 render 自然生效）。
- `renderTopBorder` 的**权威算法**（注意 `canFitOverflow` 是**函数**，且首次计算 `overflowStart` 无条件）：

```ts
if (!embedWorkingStatus || !workingStatusIndicator || width <= 0)
  return super.renderTopBorder(width, hiddenLineCount);      // 父类：hidden>0 时 createScrollBorder("↑", n, width)

let status = this.workingStatusIndicator.renderInBorder(Math.max(1, width - 5));
let statusWidth = visibleWidth(status);
if (statusWidth === 0) return super.renderTopBorder(width, hiddenLineCount);

const overflowLabel = hiddenLineCount > 0 ? ` ↑ ${hiddenLineCount} more ` : undefined;
const overflowLabelWidth = overflowLabel ? visibleWidth(overflowLabel) : 0;
const overflowStart = Math.floor((width - overflowLabelWidth) / 2);
const canFitOverflow = () =>
  overflowLabel !== undefined
  && overflowLabelWidth + 2 <= width
  && overflowStart - (3 + statusWidth + 1) >= 1;

if (overflowLabel && !canFitOverflow()) {
  status = this.workingStatusIndicator.renderSpinnerInBorder(width);
  statusWidth = visibleWidth(status);
}

if (canFitOverflow()) {
  const leftBlockWidth = 3 + statusWidth + 1;
  return this.borderColor("── ") + status +
         this.borderColor(` ${"─".repeat(overflowStart - leftBlockWidth)}${overflowLabel}` +
                          `${"─".repeat(width - overflowStart - overflowLabelWidth)}`);
}

if (width >= statusWidth + 5) {
  return this.borderColor("── ") + status + this.borderColor(` ${"─".repeat(width - statusWidth - 4)}`);
}

status = this.workingStatusIndicator.renderSpinnerInBorder(width);
statusWidth = visibleWidth(status);
const prefixWidth = Math.min(3, Math.max(0, width - statusWidth));
return this.borderColor("─".repeat(prefixWidth)) + status +
       this.borderColor("─".repeat(Math.max(0, width - prefixWidth - statusWidth)));
```

- **`canFitOverflow()` 会被调用两次**，第一次调用后 `status`/`statusWidth` 可能已被替换为 spinner，
  因此第二次判断基于**新的 statusWidth**。Rust 实现必须保留这个「先算、可能需要降级、再判断」的顺序。
- `overflowStart - leftBlockWidth` 在 `canFitOverflow()` 为真时 `>= 1`，因此 `repeat` 非负。
- 宽度恒等：`3 + statusWidth + 1 + (overflowStart - leftBlockWidth) + overflowLabelWidth +
  (width - overflowStart - overflowLabelWidth)` = `width`（第一分支）；
  `3 + statusWidth + 1 + (width - statusWidth - 4)` = `width`（第二分支）；第三分支同理。
- **`handleInput` 的权威顺序**（与 §2.28 略有不同：`app.exit` 非空时**继续往下走**而不是直接 super）：
  ```ts
  1. if (this.onExtensionShortcut?.(data)) return;
  2. if (keybindings.matches(data, "app.clipboard.pasteImage")) { this.onPasteImage?.(); return; }
  3. if (keybindings.matches(data, "app.interrupt")) {
       if (!this.isShowingAutocomplete()) {
         const handler = this.onEscape ?? this.actionHandlers.get("app.interrupt");
         if (handler) { handler(); return; }
       }
       super.handleInput(data); return;                  // ← 无论 autocomplete 与否，都 super 并 return
     }
  4. if (keybindings.matches(data, "app.exit")) {
       if (this.getText().length === 0) {
         const handler = this.onCtrlD ?? this.actionHandlers.get("app.exit");
         if (handler) handler();
         return;                                          // ← 即使没有 handler 也 return！
       }
       // 非空 → 继续（落到第 5 步）
     }
  5. if (keybindings.matches(data, "tui.editor.historyPrevious")
       || keybindings.matches(data, "tui.editor.historyNext")) { super.handleInput(data); return; }
  6. for (const [action, handler] of this.actionHandlers) {
       if (action !== "app.interrupt" && action !== "app.exit" && keybindings.matches(data, action)) {
         handler(); return;
       }
     }
  7. super.handleInput(data);
  ```
  - **第 3 步没有 handler 且 autocomplete 未开**时仍然 `super.handleInput` 并 return（Esc 会传给 Editor）。
  - **第 4 步空文本且无 handler** → return（不传 super）。
  - **第 6 步的迭代顺序是 Map 插入顺序**（JS Map 保序）→ Rust 用 `Vec<(AppKeybinding, Handler)>` 或
    `IndexMap` 保持插入顺序；**不能**用 `HashMap`（无序会导致同键冲突时行为不同）。
- `onAction(action, handler)` 是注册入口（`actionHandlers.set`）。
- 测试 `custom-editor-history-keybindings.test.ts`：显式把 `tui.editor.historyPrevious` 绑到 `alt+p`
  （默认 `app.model.cycleBackward` 在 Windows 是 `alt+p`），断言第 5 步先于第 6 步 → 翻历史而非切模型。

### 2.57 对 §2.27 `diff.ts` 的精读更正（以本节为准）

逐行核对 `diff.ts`（147 行）后的权威描述。§2.27 的结构正确，但**字段名、首段去空白逻辑、非匹配行是否
`replaceTabs`、独立 `+` 分支**有偏差，以下为最终结论。

#### 2.57.1 签名

```ts
import * as Diff from "diff";
import { theme } from "../theme/theme.ts";

function parseDiffLine(line: string): { prefix: string; lineNum: string; content: string } | null
function replaceTabs(text: string): string
function renderIntraLineDiff(oldContent: string, newContent: string): { removedLine: string; addedLine: string }

export interface RenderDiffOptions {
  /** File path (unused, kept for API compatibility) */
  filePath?: string;
}
export function renderDiff(diffText: string, _options: RenderDiffOptions = {}): string
```

- 返回类型是 `null`（**不是** `undefined`）；`_options` **默认值 `{}`**（Rust 侧用 `Option<&RenderDiffOptions>`
  或 `RenderDiffOptions::default()` 皆可）。
- 三个内部函数**均不导出**（`parseDiffLine` / `replaceTabs` / `renderIntraLineDiff`）。
- 字段名逐字：`prefix` / `lineNum` / `content`；`renderIntraLineDiff` 返回 `removedLine` / `addedLine`。

#### 2.57.2 `renderIntraLineDiff` 的**首段**去空白语义（关键差异）

```ts
const wordDiff = Diff.diffWords(oldContent, newContent);
let removedLine = "", addedLine = "";
let isFirstRemoved = true, isFirstAdded = true;

for (const part of wordDiff) {
  if (part.removed) {
    let value = part.value;
    if (isFirstRemoved) {                       // ← 只有 **第一个** removed 片段
      const leadingWs = value.match(/^(\s*)/)?.[1] || "";
      value = value.slice(leadingWs.length);
      removedLine += leadingWs;                 // 空白原样输出（不着色）
      isFirstRemoved = false;
    }
    if (value) removedLine += theme.inverse(value);
  } else if (part.added) {
    let value = part.value;
    if (isFirstAdded) { /* 对称 */ }
    if (value) addedLine += theme.inverse(value);
  } else {
    removedLine += part.value;
    addedLine += part.value;
  }
}
return { removedLine, addedLine };
```

- 与 §2.27 的差异：§2.27 写成「每个 removed/added 片段都去前导空白」，**错误**。实际只有
  **第一次**出现的 removed 片段去前导空白；后续 removed 片段**整体**（含前导空白）都被 `inverse`。
  对称地只有第一个 added 片段去空白。
- 若第一个片段去掉空白后为空串（`value` 为空），**不**输出 `inverse("")`；但 `isFirstRemoved` 已经置 false，
  后续片段照常整段着色。
- `part.removed` / `part.added` 为真时**互斥**（jsdiff 的 Change 对象不会同时有两者）；无标记的片段是公共部分，
  **同一份 `part.value` 同时追加到两行**。
- `theme.inverse(text)` 是全局主题的 inverse 包装（见 §3.4），输出形如 `\x1b[7m...\x1b[27m`（具体序列由主题层决定）。

#### 2.57.3 `renderDiff` 主循环

```ts
const lines = diffText.split("\n");
const result: string[] = [];
let i = 0;
while (i < lines.length) {
  const line = lines[i]!;
  const parsed = parseDiffLine(line);
  if (!parsed) { result.push(theme.fg("toolDiffContext", line)); i++; continue; }   // ← 不 replaceTabs
  if (parsed.prefix === "-") {
    const removedLines: {lineNum,content}[] = [];
    while (i < lines.length) { const p = parseDiffLine(lines[i]!); if (!p || p.prefix !== "-") break;
                               removedLines.push({lineNum: p.lineNum, content: p.content}); i++; }
    const addedLines: {lineNum,content}[] = [];
    while (i < lines.length) { const p = parseDiffLine(lines[i]!); if (!p || p.prefix !== "+") break;
                               addedLines.push({lineNum: p.lineNum, content: p.content}); i++; }
    if (removedLines.length === 1 && addedLines.length === 1) {
      const { removedLine, addedLine } =
        renderIntraLineDiff(replaceTabs(removedLines[0].content), replaceTabs(addedLines[0].content));
      result.push(theme.fg("toolDiffRemoved", `-${removedLines[0].lineNum} ${removedLine}`));
      result.push(theme.fg("toolDiffAdded",   `+${addedLines[0].lineNum} ${addedLine}`));
    } else {
      for (const r of removedLines) result.push(theme.fg("toolDiffRemoved", `-${r.lineNum} ${replaceTabs(r.content)}`));
      for (const a of addedLines)   result.push(theme.fg("toolDiffAdded",   `+${a.lineNum} ${replaceTabs(a.content)}`));
    }
  } else if (parsed.prefix === "+") {
    result.push(theme.fg("toolDiffAdded", `+${parsed.lineNum} ${replaceTabs(parsed.content)}`));
    i++;
  } else {
    result.push(theme.fg("toolDiffContext", ` ${parsed.lineNum} ${replaceTabs(parsed.content)}`));
    i++;
  }
}
return result.join("\n");
```

逐点确认：

1. **非匹配行**（不满足正则，例如 `\ No newline at end of file`、`--- a/x`、`+++ b/x`、空串）→
   `theme.fg("toolDiffContext", line)`，**不做 `replaceTabs`**（tab 原样保留）。§2.27 的写法有误。
2. `--- a/file` / `+++ b/file` 这类行：`parseDiffLine` 的正则 `^([+-\s])(\s*\d*)\s(.*)$` 需要第二个字符起为
   「可选空白 + 数字」，`--- a/file` 的第二字符是 `-`，不匹配 `\s*\d*` 后跟 `\s`，所以落到**非匹配分支**，
   以 context 色输出整行。**但** `+1` 这类无空格行也不匹配（要求末尾有一个空格），同样走非匹配分支。
3. `prefix === "-"` 分支里，连续 `-` 行收集完之后**紧跟的连续 `+` 行**被一并消费；`-`/`+` 之间若夹了
   context 行，则 `addedLines` 为空数组，走「分别输出」分支。
4. 单删单增时：先 `replaceTabs` 再 `renderIntraLineDiff`（**tab 在词 diff 之前就替换成 3 空格**）。
   输出顺序固定为「先 removed 行，后 added 行」。
5. 多行时：**先全部 removed 行，再全部 added 行**（即便原始 diff 是交错排列，但交错时会被 context 拆成多个块，
   每个块内部仍是先 `-` 后 `+`）。
6. 行首拼接：removed `-${lineNum} `、added `+${lineNum} `、context `` ` ${lineNum} ` ``（**一个前导空格**）。
   `lineNum` 是正则第二捕获组（可能为空串、可能带前导空白，例如 ` 12` 里的 `12`；`\s*\d*` 会贪婪吃掉空格）。
   Rust 侧不要「解析成数字再格式化」——直接保留捕获的原始字符串，否则前导空格/缺失会不一致。
7. 末尾空行：`"abc\n"` split 后产生尾部空串 → 该空串不匹配正则 → 输出一个**空内容的 context 行**，
   最终 `join("\n")` 保留尾部换行效果（多一个空行）。

#### 2.57.4 颜色 token

`toolDiffRemoved` / `toolDiffAdded` / `toolDiffContext`（三个都在主题的 `fg` 表里，见 §3.4）。
`theme.inverse` 不改变前景色，只切换 SGR 7。

#### 2.57.5 Rust 对应

```rust
pub struct RenderDiffOptions { pub file_path: Option<String> }
struct ParsedLine { prefix: u8, line_num: String, content: String }
fn parse_diff_line(line: &str) -> Option<ParsedLine>          // 需复刻 JS 正则语义（见下）
fn replace_tabs(text: &str) -> String                        // '\t' -> "   "
fn render_intra_line_diff(old: &str, new: &str) -> (String, String)   // 依赖 diffWords 移植
pub fn render_diff(diff_text: &str, _options: &RenderDiffOptions) -> String
```

- `parse_diff_line` 用等价正则 `^([+\-\s])(\s*\d*)\s(.*)$`（`regex` crate，`(?s)` 使 `.` 匹配除换行外的任意字符，
  JS 的 `.` 默认不匹配 `\n`，而这里输入是单行，故无差异）。注意 JS 的 `\s` 包含 `﻿` 与 Unicode 空白，
  `regex` crate 的 `\s` 默认是 Unicode 的 `White_Space`（**不含** `﻿`）——若需完全一致，显式写成
  `[\s\u{FEFF}]`。捕获组 2 与 3 必须**原样**保留（`content` 允许为空）。
- `renderIntraLineDiff` 必须用 §6.3 中 jsdiff `diffWords` 的移植版；`similar` crate 的结果不同（分词与合并规则不同），
  **不可直接替换**。
- `split('\n')` 在 Rust 用 `diff_text.split('\n')`（**不要**用 `lines()`：`lines()` 会去掉 `\r` 且不产生尾部空串）。
  JS 侧 `\r` 会保留在行内容里，最终出现在输出中。

### 2.58 对 §2.20 `visual-truncate.ts` 的精读更正（以本节为准）

§2.20 的 `VisualLinePreview` 构造函数签名为**旧版**，实际是 options 对象 + `keep`：

```ts
export interface VisualTruncateResult { visualLines: string[]; skippedCount: number; }

export function truncateToVisualLines(
  text: string,
  maxVisualLines: number,
  width: number,
  paddingX: number = 0,
  keep: "start" | "end" = "end",
): VisualTruncateResult

export interface VisualLinePreviewOptions {
  /** Styled text; may contain newlines. */
  text: string;
  maxVisualLines: number;
  /** Which visual lines to keep. The hint goes before kept end lines and after kept start lines. */
  keep: "start" | "end";
  /** Styled hint line for the given number of hidden visual lines. */
  formatHint: (hidden: number) => string;
}

export class VisualLinePreview implements Component {
  constructor(options: VisualLinePreviewOptions);
  render(width: number): string[];
  invalidate(): void;
}
```

- **没有 `getText` 闭包、没有 `paddingX` 字段**；`text` 是构造时给定的**不可变**字符串。
- `truncateToVisualLines` 实现：
  ```ts
  if (!text) return { visualLines: [], skippedCount: 0 };      // ← 空串早退
  const tempText = new Text(text, paddingX, 0);
  const allVisualLines = tempText.render(width);
  if (allVisualLines.length <= maxVisualLines) return { visualLines: allVisualLines, skippedCount: 0 };
  const truncatedLines = keep === "start" ? allVisualLines.slice(0, maxVisualLines)
                                          : allVisualLines.slice(-maxVisualLines);
  return { visualLines: truncatedLines, skippedCount: allVisualLines.length - maxVisualLines };
  ```
  - **空串早退**（`!text` 也涵盖 `""`）→ 返回空数组；此时**不**创建 `Text`。
  - `Text(text, paddingX, 0)` 的第三参数是 `paddingY = 0`。
  - `maxVisualLines <= 0` 时（调用方未如此用，但需一致）：`slice(0, 0)` → 空；`slice(-0)` = `slice(0)` → **全部**，
    且 `skippedCount = len - 0 = len`。Rust 侧 `usize` 下 `-0` 不存在，需特判 `max == 0` 时 `keep=="end"` 返回全部。
- `VisualLinePreview.render(width)`（**缓存按 width**）：
  ```ts
  if (this.cachedLines === undefined || this.cachedWidth !== width) {
    const { text, maxVisualLines, keep, formatHint } = this.options;
    const preview = truncateToVisualLines(text, maxVisualLines, width, 0, keep);   // paddingX 固定 0
    const lines = preview.visualLines;
    if (preview.skippedCount > 0) {
      const hint = truncateToWidth(formatHint(preview.skippedCount), width, "...");
      this.cachedLines = keep === "start" ? [...lines, hint] : [hint, ...lines];
    } else {
      this.cachedLines = lines;                       // ← 直接复用 preview.visualLines 数组
    }
    this.cachedWidth = width;
  }
  return this.cachedLines;
  ```
  - **`keep === "start"` 时 hint 在末尾，`keep === "end"` 时 hint 在开头**（§2.20「固定用 end 语义」错误）。
  - `paddingX` 传给 `truncateToVisualLines` 恒为 `0`（因为调用方会放在 Box 里，Box 自带 padding）。
  - 缓存键只有 `width`；`options.text` 若在外部被替换（对象不可变，但调用方可传新 options 实例的情形不存在）不会失效，
    必须 `invalidate()`。`invalidate()` 把 `cachedWidth` 与 `cachedLines` 都置 `undefined`。
  - `formatHint` 的返回值先 `truncateToWidth(..., width, "...")` 再插入。
- **使用方**（`bash-execution.ts` / `tool-execution.ts`）：`formatHint = (skipped) => theme.fg("muted", `... ${skipped} more lines (${keyText("app.tools.expand")} to expand)`)` 之类；
  具体逐字串见 §3.2 对应小节。
- **测试**：无直接测试文件；§7.3 要求新增 `truncateToVisualLines` 的 `keep=start`/`keep=end` 单测与
  `VisualLinePreview` 的宽度缓存测试（同一 width 连续 render 应返回同一数组内容、换 width 后重算）。

### 2.59 对 §2.18 `mermaid.ts` 的精读更正（以本节为准）

```ts
import { Marked, type Token } from "@earendil-works/kiana-tui";
import { type MermaidArt, render, type Span } from "grok-mermaid";
import type { MarkdownTransformer } from "../../../core/extensions/types.ts";
import type { MermaidRenderingMode } from "../../../core/settings-manager.ts";
import type { Theme } from "../theme/theme.ts";

const markdownParser = new Marked();

interface MermaidTransformerOptions {          // ← 不导出
  getMode: () => MermaidRenderingMode;
  theme?: Theme;                                // ← **可选**
}

function isMermaid(token: Token): token is Token & { type: "code"; text: string; lang?: string }
function codeSpan(line: string): string
function styleSpan(span: Span, theme: Theme): string
function themedLines(art: MermaidArt, theme: Theme): string[]
export function createMermaidMarkdownTransformer(options: MermaidTransformerOptions): MarkdownTransformer
```

- `MermaidTransformerOptions` 是**接口但未导出**；`theme` 可选。`styleSpan` / `themedLines` / `isMermaid` /
  `codeSpan` 均不导出。
- `codeSpan(line)` 逐字：
  ```ts
  const content = line || " ";                                    // 空行 → NBSP（不是普通空格！）
  const longestBacktickRun = Math.max(0, ...Array.from(content.matchAll(/`+/g), (m) => m[0].length));
  const fence = "`".repeat(longestBacktickRun + 1);
  const padding = content.startsWith("`") || content.endsWith("`") ? " " : "";
  return `${fence}${padding}${content}${padding}${fence}`;
  ```
  - **`Math.max(0, ...[])` = `0`**（§6.3 已列）；Rust 用 `runs.iter().map(len).max().unwrap_or(0)`。
  - `padding` 判断在 **NBSP 替换之后**（NBSP 既不是 `` ` `` 所以无影响）。
  - 若 `line` 为 `""`，`content` 是单个 U+00A0，`fence` 为 `` ` ``。
- `styleSpan(span, theme)` 的 `title` 分支实际是 **`theme.fg("accent", theme.bold(span.text))`**（accent 在外、
  bold 在内），§2.18 表格写成 `theme.bold(theme.fg("accent", text))` 是**顺序颠倒**——ANSI 序列顺序不同，
  但可见效果相同；为逐字节一致，以 **`fg` 在外** 为准。
  完整映射：
  | `span.cls` | 返回 |
  |---|---|
  | `"border"` | `theme.fg("borderMuted", span.text)` |
  | `"text"` | `theme.fg("text", span.text)` |
  | `"edge"` | `theme.fg("accent", span.text)` |
  | `"edgeLabel"` | `theme.fg("muted", span.text)` |
  | `"title"` | `theme.fg("accent", theme.bold(span.text))` |
  | `"none"` | `span.text`（原样） |
  - `switch` **没有 `default`**；未知 `cls` 返回 `undefined`（TS 类型上不可达，因为 `Span["cls"]` 是联合类型）。
    Rust 侧用 `match` 覆盖全部变体，未知值按 `"none"` 处理即可（不可达）。
- `themedLines(art, theme)`：`art.styled.map((row) => row.map((span) => styleSpan(span, theme)).join(""))`
  —— **每行内各 span 拼接**，行间不拼接。
- 转换器主体（`createMermaidMarkdownTransformer` 返回的 `(markdown, context)`）：
  ```ts
  const mode = options.getMode();
  if (mode === "off" || context.messageType === "assistant-thinking"
      || (context.isStreaming && mode !== "streaming")) return markdown;
  return markdownParser.lexer(markdown).map((token) => {
    if (!isMermaid(token)) return token.raw;                       // ← token.raw，非 token.text
    const art = render(token.text);
    if (!art || art.width > context.availableWidth) return token.raw;
    if (!context.isStreaming && art.warnings.length > 0) {
      const suffix = art.warnings.length > 1 ? ` (+${art.warnings.length - 1} more)` : "";
      const warning = `Mermaid diagram not rendered: ${art.warnings[0]}${suffix}`;
      const styledWarning = options.theme ? options.theme.fg("warning", warning) : warning;
      return `${token.raw}\n${codeSpan(styledWarning)}  \n`;
    }
    const lines = options.theme ? themedLines(art, options.theme) : art.plain;
    return `${lines.map(codeSpan).join("  \n")}\n`;
  }).join("");
  ```
  逐点：
  1. **提前返回**发生在 lex 之前，返回**原始 markdown 字符串**（不 lex，不做任何处理）。
  2. 只处理 **顶层 token**（`lexer()` 的顶层数组；代码块内的 mermaid 不会被单独识别，因为 code token 本身是顶层）。
  3. 非 mermaid token 用 **`token.raw`**（含原始空白与围栏），保证未改动部分逐字节不变。
  4. `render` 是 `grok-mermaid` 的导出函数（`import { render }`），别名 `renderMermaid`；返回 `MermaidArt | null/undefined`。
  5. `art.width > context.availableWidth` → 保持原样（**严格大于**；等于时仍渲染）。
  6. 警告分支**仅在非流式**；流式中即便有 warnings 也走正常渲染。
  7. 警告行格式：**`token.raw` + `"\n"` + codeSpan(警告) + `"  \n"`**（两个空格 = Markdown 硬换行，
     末尾 `\n` 让下一个 token 另起一行）。
  8. 正常分支：**`lines.map(codeSpan).join("  \n") + "\n"`**；即每行包成 code span，行间两空格换行，整体末尾 `\n`。
  9. `options.theme` 缺失时用 `art.plain`（无 ANSI 的纯文本行）。
  10. 整个结果 `join("")` —— 各 token 的替换结果首尾相接。
- **【逐字】** `Mermaid diagram not rendered: `、` (+N more)`、行间 `"  \n"`、尾部 `"\n"`、空行 NBSP `" "`。
- **测试**（`mermaid.test.ts`，8 项）：上述行为均被覆盖；断言里主题标记形如 `<borderMuted>...</borderMuted>` 之类
  （测试用 mock 主题包装 `fg` 为 `<token>text</token>`）。渲染库本身的行为由 `grok-mermaid` 的测试保证，
  本单元只需保证「契约调用 + 拼接」正确。
- **不可得依赖**：`grok-mermaid` 不在仓库 node_modules 中（§8.5 Q3）。Rust 侧需按
  `MermaidArt { width: usize, styled: Vec<Vec<Span>>, plain: Vec<String>, warnings: Vec<String> }` 与
  `Span { text: String, cls: SpanClass }` 实现等价渲染器；验收以 `mermaid.test.ts` 的 8 个用例为准
  （简单图 `│ Start ├───▶│ Done │`、pie 图不渲染、超宽不渲染、classDef、流式未闭合、警告与 `(+N more)`、三种模式保持原文）。

### 2.60 最后一批文件的逐行核对（`footer` / `login-dialog` / `auth-url` / `session-selector` / `session-selector-search` / 四个消息块 / `armin` / `easter-egg-3d` / `keybinding-hints` / `settings-submenu`）

以下均按源码逐行核对，**与 §2.5–§2.16、§2.34–§2.36、§2.42、§2.44–§2.46、§2.52.1–§2.52.2 冲突时以本节为准**。

#### 2.60.1 `auth-url.ts`（39 行）

- 唯一导出 `AuthUrlComponent extends Container`，字段 `readonly url: string`。
- 构造顺序（**只有两个子组件，无 Spacer、无 DynamicBorder**）：
  1. `new Text(theme.fg("accent", hyperlink(url, url)), 1, 0)` —— **`hyperlink(text, url)` 的参数顺序是
     text 在前**（`packages/tui/src/terminal-image.ts:745`）：
     `\x1b]8;;${url}\x1b\\${text}\x1b]8;;\x1b\\`（ST 结尾）。
  2. `this.hint = new Text("", 1, 0)`，再 `setHint(keyHint("app.message.copy", "to copy"))`。
- `setHint(suffix)`：`hint.setText(`${theme.fg("dim", hyperlink(clickHint, this.url))} ${theme.fg("dim", "•")} ${suffix}`)`
  —— 注意 `suffix` **不再套色**（调用方已传带色字符串）。然后 `this.tui.requestRender()`。
- `copy()`：`await copyToClipboard(this.url)`；成功 → `setHint(theme.fg("success", "Copied URL to clipboard"))`；
  失败 → `setHint(theme.fg("error", msg))`。**`copy()` 没有 try/finally 之外的额外行为**，也不吞 rendering 错误。
- `clickHint` 在 **每次 `setHint` 时重新计算**（不是构造时缓存）。

#### 2.60.2 `login-dialog.ts`（322 行）

导出面只有 `LoginDialogComponent`。逐点核对（更正 §2.36 若干处）：

- 字段：`contentContainer`、`input`、`tui`、`authUrl?`、`abortController = new AbortController()`、
  `inputResolver?`、`inputRejecter?`、`deviceCodeAction?: { info, hint: Container }`、`deviceCodeHint?`、
  `onComplete`。**`deviceCodeHint` 与 `deviceCodeAction.hint` 指向同一 Container**（冗余字段，仅用于
  `clearDeviceCodeAction()` 的 `removeChild`）；`clearDeviceCodeAction()` 先 `removeChild(deviceCodeHint)`
  再两个字段都置 `undefined`。
- 构造：
  ```ts
  const providerName = providerNameOverride || providerId;              // "" 也会回退到 providerId
  const title = titleOverride ?? `Login to ${providerName}`;             // 注意是 ?? 不是 ||
  addChild(new DynamicBorder());                                        // 无参 → theme.fg("border")
  addChild(new Text(theme.fg("accent", theme.bold(title)), 1, 0));       // fg 在外、bold 在内
  this.contentContainer = new Container(); addChild(this.contentContainer);
  this.input = new Input();                                             // ← input **不是**构造时加进容器
  this.input.onSubmit = ...
  this.input.onEscape = () => this.cancel();
  addChild(new DynamicBorder());
  ```
  - **`input` 只在 `showManualInput` / `showPrompt` 时才被 addChild**（§2.36 写「addChild(this.input)」是错的）。
  - `onSubmit` 只在 `inputResolver` 存在时生效：取 `value` → `replaceInputWithSubmittedText(value)` →
    `inputResolver(value)` → 两个 resolver 字段清空。
  - `replaceInputWithSubmittedText(value)`：把 `contentContainer.children` 里**等于 `this.input` 的那一项**
    替换成 `new Text(`> ${value}`, 0, 0)`（`map` + 恒等比较；paddingX=0）。
- `get signal()` 返回同一个 `AbortController.signal`（全程一个）。
- `cancel()`（**私有**，不是公开 API）：
  ```ts
  clearDeviceCodeAction();
  abortController.abort();
  if (inputRejecter) { inputRejecter(new Error("Login cancelled")); inputResolver = undefined; inputRejecter = undefined; }
  onComplete(false, "Login cancelled");
  ```
  - 拒的是 **inputRejecter**（§2.36 的伪代码写成 `inputResolver = undefined; reject(...)`，语义正确但对象名有误）。
  - 若没有 pending input，`onComplete(false, "Login cancelled")` 仍然调用。
- `showAuth(url, instructions?)`：`clearDeviceCodeAction()` → `contentContainer.clear()` → `Spacer(1)` →
  `authUrl = new AuthUrlComponent(tui, url)` + addChild →（instructions：`Spacer(1)` +
  `Text(theme.fg("warning", instructions), 1, 0)`）→ `openBrowser(url)`（**不 await、错误静默**）→
  `tui.requestRender()`。
- `showDeviceCode(info)`：
  ```ts
  this.authUrl = undefined;                                   // 先清引用，防止 copy 键复制旧 URL
  info = { ...info, openBrowserOnConfirm: info.openBrowserOnConfirm ?? true,
                     copyCodeOnConfirm: info.copyCodeOnConfirm ?? true };
  clearDeviceCodeAction(); contentContainer.clear(); addChild(new Spacer(1));
  const linkedUrl = `\x1b]8;;${info.verificationUri}\x07${info.verificationUri}\x1b]8;;\x07`;   // BEL！
  addChild(new Text(theme.fg("accent", linkedUrl), 1, 0));
  const clickHint = process.platform === "darwin" ? "Cmd+click to open" : "Ctrl+click to open";
  const hyperlink = `\x1b]8;;${info.verificationUri}\x07${clickHint}\x1b]8;;\x07`;
  addChild(new Text(theme.fg("dim", hyperlink), 1, 0));
  addChild(new Spacer(1));
  addChild(new Text(theme.fg("warning", `Enter code: ${info.userCode}`), 1, 0));
  if (!this.signal.aborted && (info.openBrowserOnConfirm || info.copyCodeOnConfirm)) {
    const actionLabel = openBrowserOnConfirm ? (copyCodeOnConfirm ? "to open browser and copy code" : "to open browser")
                                             : "to copy code";
    const hint = new Container();
    hint.addChild(new TruncatedText(`(${keyHint("tui.select.confirm", actionLabel)})`, 1, 0));
    contentContainer.addChild(hint);
    this.deviceCodeHint = hint; this.deviceCodeAction = { info, hint };
  }
  this.tui.requestRender();
  ```
  - `hint` 用 **`Container` 包一层 `TruncatedText`**（不是直接 Text），便于后续原地替换子节点。
  - 判定用的是**重组后**的 `info`（默认值已填）。
- `setDeviceCodeStatus(hint, message)`（私有，两参数）：
  `if (signal.aborted || !this.focused || !contentContainer.children.includes(hint)) return;`
  → `hint.children = [new TruncatedText(theme.fg("dim", message), 1, 0)];` → `tui.requestRender()`。
  - **直接赋值 `children` 数组**（绕过 removeChild/addChild），Rust 侧对应「整体替换子 Vec」。
  - `!this.focused` 守卫意味着：对话框失焦时**不会**更新状态文案。
- `activateDeviceCode(info, hint)`（私有，两参数；**同步调用 `copyToClipboard` 的 Promise 链，不 await**）：
  ```ts
  setDeviceCodeStatus(hint, info.copyCodeOnConfirm ? "Copying code..." : "Browser open requested");
  if (info.copyCodeOnConfirm) void copyToClipboard(info.userCode)
      .then(() => setDeviceCodeStatus(hint, "Code copied to clipboard"))
      .catch(() => setDeviceCodeStatus(hint, "Copy code manually"));
  if (info.openBrowserOnConfirm) {
    try { const url = new URL(info.verificationUri);
          if (url.protocol === "https:" || url.protocol === "http:") openBrowser(url.href); } catch {}
  }
  ```
  - **URL 校验用 `new URL()` + protocol 白名单**（§2.36 写的 `startsWith("http://")` 是错的），
    因此 `HTTP://`、`https://evil`、`javascript:` 等按 URL 解析后的 protocol 判断；解析失败静默。
  - `openBrowser` 传的是 **`url.href`**（规范化后的字符串，可能与原串不同，例如末尾补 `/`）。
- `showManualInput(prompt)`：
  ```ts
  clearDeviceCodeAction();
  this.input.setValue("");                       // ← 先清值
  addChild(new Spacer(1));
  addChild(new Text(theme.fg("dim", prompt), 1, 0));
  addChild(this.input);                          // 复用同一 Input 实例
  addChild(new Text(`(${keyHint("tui.select.cancel", "to cancel")})`, 1, 0));
  this.tui.requestRender();
  return new Promise((resolve, reject) => { this.inputResolver = resolve; this.inputRejecter = reject; });
  ```
  - **不 clear 内容**（与 §2.36 的 `contentContainer.clear()` 相反）——`showManualInput` 是追加。
- `showPrompt(message, placeholder?)`：同样**追加**；顺序 `Spacer(1)` → `Text(theme.fg("text", message), 1, 0)` →
  （placeholder）`Text(theme.fg("dim", `e.g., ${placeholder}`), 1, 0)` → `addChild(this.input)` →
  `Text(`(${keyHint("tui.select.cancel", "to cancel,")} ${keyHint("tui.select.confirm", "to submit")})`, 1, 0)` →
  `this.input.setValue("")` → requestRender → Promise。
- `showDetails(lines)`：`authUrl = undefined`；`clearDeviceCodeAction()`；`contentContainer.clear()`；
  `Spacer(1)`；每行 `new Text(line, 1, 0)`（**paddingX = 1**，§2.36 写 0 是错的；不着色）；requestRender。
- `showInfo(message, links = [], showCloseHint = false)`：
  **不 clear、不调用 `clearDeviceCodeAction()`**（纯追加）；`Spacer(1)` → `Text(theme.fg("text", message),1,0)` →
  每个 link：`const text = link.label ? `${link.label}: ${link.url}` : link.url;`
  `hyperlink = `\x1b]8;;${link.url}\x07${text}\x1b]8;;\x07``（BEL）→ `Text(theme.fg("accent", hyperlink),1,0)` →
  （showCloseHint）`Spacer(1)` + `Text(`(${keyHint("tui.select.cancel", "to close")})`,1,0)` → requestRender。
- `showWaiting(message)`：**追加** `Spacer(1)` + `Text(theme.fg("dim", message),1,0)` +
  `Text(`(${keyHint("tui.select.cancel", "to cancel")})`,1,0)` + requestRender。
- `showProgress(message)`：**追加** `Text(theme.fg("dim", message),1,0)` + requestRender（无 Spacer）。
- `handleInput(data)`（权威顺序）：
  ```ts
  const kb = getKeybindings();
  if (kb.matches(data, "tui.select.cancel")) { this.cancel(); return; }               // ① 最高优先
  if (this.authUrl && kb.matches(data, "app.message.copy")) { void this.authUrl.copy(); return; }   // ②
  if (this.deviceCodeAction && !this.inputResolver && !this.signal.aborted && this.focused
      && kb.matches(data, "tui.select.confirm")) {                                    // ③
    const action = this.deviceCodeAction; this.deviceCodeAction = undefined;          // 一次性消费
    this.activateDeviceCode(action.info, action.hint);
    this.tui.requestRender();
    return;
  }
  this.input.handleInput(data);                                                       // ④ 兜底
  ```
  - ③ 消费后**不重置 `deviceCodeHint`**（只清 `deviceCodeAction`），所以 `clearDeviceCodeAction()` 仍能移除该行。
  - ③ 的守卫含 `this.focused`；未聚焦时 Enter 落到 ④ 传给 input。
- **【逐字】** `Login to `、`> `、`Login cancelled`、`No named…`（无）、`Cmd+click to open`、`Ctrl+click to open`、
  `Enter code: `、`to open browser and copy code`、`to open browser`、`to copy code`、`Copying code...`、
  `Browser open requested`、`Code copied to clipboard`、`Copy code manually`、`e.g., `、`to cancel`、`to cancel,`、
  `to submit`、`to close`、`(`、`)`。

#### 2.60.3 `footer.ts`（302 行）

- 导出：`formatTokens`、`formatCwdForFooter`、`FooterComponent`。**`sanitizeStatusText` 不导出**（模块私有）。
- `formatTokens` 逐字：
  ```ts
  if (count < 1000) return count.toString();
  if (count < 10000) return `${(count / 1000).toFixed(1)}k`;
  if (count < 1000000) return `${Math.round(count / 1000)}k`;
  if (count < 10000000) return `${(count / 1000000).toFixed(1)}M`;
  return `${Math.round(count / 1000000)}M`;
  ```
  - 注意前两个分支的边界是 **10000**（不是 1000 的十倍写法差异），第三段 `>= 1e6` 用 `Math.round/1000` 已不适用。
- `formatCwdForFooter(cwd, home)`：
  ```ts
  if (!home) return cwd;
  const resolvedCwd = resolve(cwd), resolvedHome = resolve(home);
  const relativeToHome = relative(resolvedHome, resolvedCwd);
  const isInsideHome = relativeToHome === "" || (relativeToHome !== ".."
      && !relativeToHome.startsWith(`..${sep}`) && !isAbsolute(relativeToHome));
  if (!isInsideHome) return cwd;
  return relativeToHome === "" ? "~" : `~${sep}${relativeToHome}`;
  ```
  逐点：空串判断在前；`".."` 单独判（否则 `..` 目录会被 `startsWith(".."+sep)` 漏掉）；绝对路径视为不在 home 内；
  返回值用**原始 `cwd`**（不是 `resolvedCwd`）。
- `FooterComponent` 字段：`autoCompactEnabled = true`、`session`、`footerData`、`sessionStats?`。
  `invalidate()` / `dispose()` 都是**空实现**（注释说明 git 缓存已移到 provider）。
- `getSessionStats()` 缓存键**逐项**：`session`（引用相等）、`sessionId`、`leafId`、`entryCount`、`limitsModel`
  （`session.routedModel?.model ?? session.model`）。**注意 §2.35 说的 5 项正确，但顺序/字段名需照抄**。
- 遍历 `sessionManager.getEntries()` 累计（精确分支）：
  ```ts
  if (entry.type === "usage") addUsageToTotals(usageTotals, entry.usage);
  else if (entry.type === "message" && entry.message.role === "assistant") {
    addUsageToTotals(usageTotals, entry.message.usage);
    const latestPromptTokens = usage.input + usage.cacheRead + usage.cacheWrite;
    latestCacheHitRate = latestPromptTokens > 0 ? (usage.cacheRead / latestPromptTokens) * 100 : undefined;
  } else if (entry.type === "message" && entry.message.role === "toolResult" && entry.message.usage)
    addUsageToTotals(usageTotals, entry.message.usage);
  else if ((entry.type === "branch_summary" || entry.type === "compaction") && entry.usage)
    addUsageToTotals(usageTotals, entry.usage);
  ```
  - `latestCacheHitRate` **每次遇到 assistant 消息就整体覆盖**（包括覆盖成 `undefined`），不是取 max，也不是
    只在 cache>0 时更新。§2.35 的「若 cacheRead+cacheWrite>0 更新」**错误**。
  - `addUsageToTotals` 见 `core/usage-totals.ts`：`input/output/cacheRead/cacheWrite` 直接加，`cost += usage.cost.total`。
- `render(width)` 逐字流程：
  ```ts
  const state = this.session.state;
  const { usageTotals, latestCacheHitRate, contextUsage } = this.getSessionStats();
  const contextWindow = contextUsage?.contextWindow ?? state.model?.contextWindow ?? 0;
  const contextPercentValue = contextUsage?.percent ?? 0;
  const contextPercent = contextUsage?.percent !== null ? contextPercentValue.toFixed(1) : "?";
  ```
  - **`contextUsage?.percent !== null`**：`contextUsage` 为 `undefined` 时 `undefined !== null` 为真，
    `contextPercentValue` 为 0 → `"0.0"`；只有 `percent` **显式是 null** 才是 `"?"`。这是易错点。
  - `pwd = formatCwdForFooter(sessionManager.getCwd(), process.env.HOME || process.env.USERPROFILE)`；
    有 branch → `pwd = `${pwd} (${branch})``；有 sessionName → `pwd = `${pwd} • ${sessionName}``。
  - stats 片段（顺序固定，**逐个 push**）：
    ```ts
    if (usageTotals.input)      push(`↑${formatTokens(input)}`);
    if (usageTotals.output)     push(`↓${formatTokens(output)}`);
    if (usageTotals.cacheRead)  push(`R${formatTokens(cacheRead)}`);
    if (usageTotals.cacheWrite) push(`W${formatTokens(cacheWrite)}`);
    if ((cacheRead > 0 || cacheWrite > 0) && latestCacheHitRate !== undefined) push(`CH${rate.toFixed(1)}%`);
    ```
    订阅判定：`usingSubscription = state.model ? (state.model.provider === "kimi-coding" ||
      session.modelRuntime.isUsingSubscription(profile)) : false`；若 `cost || usingSubscription` →
    `push(`$${cost.toFixed(3)}${usingSubscription ? " (sub)" : ""}`)`。
    → `statsParts.push(contextPercentStr)`，其中
    ```ts
    const autoIndicator = this.autoCompactEnabled ? " (auto)" : "";
    const contextPercentDisplay = contextPercent === "?"
      ? `?/${formatTokens(contextWindow)}${autoIndicator}`
      : `${contextPercent}%/${formatTokens(contextWindow)}${autoIndicator}`;
    contextPercentStr = contextPercentValue > 90 ? theme.fg("error", contextPercentDisplay)
                      : contextPercentValue > 70 ? theme.fg("warning", contextPercentDisplay)
                      : contextPercentDisplay;                       // ← 低占用时**不加色**（不是 dim）
    ```
    实验特性：`if (areExperimentalFeaturesEnabled()) statsParts.push(`${theme.fg("dim","•")} ${theme.bold(theme.fg("warning","xp"))}`)`。
    `statsLeft = statsParts.join(" ")`；若 `visibleWidth(statsLeft) > width` →
    `statsLeft = truncateToWidth(statsLeft, width, "...")` 并重算宽度。
  - 右半：
    ```ts
    const modelName = state.model?.id || "no-model";
    let rightSideWithoutProvider = modelName;
    if (state.model?.reasoning) {
      const level = state.thinkingLevel || "off";
      rightSideWithoutProvider = level === "off" ? `${modelName} • thinking off` : `${modelName} • ${level}`;
    }
    const routed = this.session.routedModel;
    if (routed) {
      const level = routed.thinkingLevel ? ` • ${routed.thinkingLevel}` : "";
      rightSideWithoutProvider += ` → ${routed.model.id}${level}`;
    }
    let rightSide = rightSideWithoutProvider;
    if (footerData.getAvailableProviderCount() > 1 && state.model) {
      rightSide = `(${state.model.provider}) ${rightSideWithoutProvider}`;
      if (statsLeftWidth + 2 + visibleWidth(rightSide) > width) rightSide = rightSideWithoutProvider;
    }
    ```
    - **thinking 用 `state.model?.reasoning` 布尔判断**（§2.35 写「`getSupportedThinkingLevels(...).length > 0`」是错的）。
  - 布局（**权威**）：
    ```ts
    const rightSideWidth = visibleWidth(rightSide);
    const totalNeeded = statsLeftWidth + 2 + rightSideWidth;
    let statsLine: string;
    if (totalNeeded <= width) {
      const padding = " ".repeat(width - statsLeftWidth - rightSideWidth);
      statsLine = statsLeft + padding + rightSide;
    } else {
      const availableForRight = width - statsLeftWidth - 2;
      if (availableForRight > 0) {
        const truncatedRight = truncateToWidth(rightSide, availableForRight, "");
        const padding = " ".repeat(Math.max(0, width - statsLeftWidth - visibleWidth(truncatedRight)));
        statsLine = statsLeft + padding + truncatedRight;
      } else statsLine = statsLeft;
    }
    ```
    - 右半截断省略号是 **`""`**（硬截，无 `...`）。
  - 着色拼接（**关键**）：
    ```ts
    const dimStatsLeft = theme.fg("dim", statsLeft);
    const remainder = statsLine.slice(statsLeft.length);        // 按**字符串长度**切（含 ANSI 也算）
    const dimRemainder = theme.fg("dim", remainder);
    const pwdLine = truncateToWidth(theme.fg("dim", pwd), width, theme.fg("dim", "..."));
    const lines = [pwdLine, dimStatsLeft + dimRemainder];
    ```
    - `statsLine.slice(statsLeft.length)` 用的是 **UTF-16 长度**，而 `statsLine` 以 `statsLeft` 原样开头，
      所以切点正确。Rust 侧必须按 **UTF-16 code unit 数**切（或直接构造 `padding + rightSide`，语义等价）。
  - 扩展状态行：
    ```ts
    const extensionStatuses = footerData.getExtensionStatuses();
    if (extensionStatuses.size > 0) {
      const sortedStatuses = Array.from(extensionStatuses.entries())
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([, text]) => sanitizeStatusText(text));
      lines.push(truncateToWidth(sortedStatuses.join(" "), width, theme.fg("dim", "...")))
    }
    ```
    - **注意：这里没有给状态行套 `theme.fg("dim", ...)`**（§2.35 说有，是错的）；只把省略号染成 dim。
    - `localeCompare` 默认 locale 排序（与 `sort()` 的码点序不同），见 §8.3 第 9 项。
  - `sanitizeStatusText`：`text.replace(/[\r\n\t]/g, " ").replace(/ +/g, " ").trim()`（`trim` 去 U+FEFF，见 §6.3）。
- 测试（`footer-width.test.ts` 11 项）与本实现一致；注意「兄弟路径」断言依赖 `relative()` 不以 `..` 前缀误判。

#### 2.60.4 `session-selector-search.ts`（260 行）

导出：`SortMode`、`NameFilter`、`ParsedSearchQuery`、`MatchResult`、`hasSessionName`、`parseSearchQuery`、
`matchSession`、`filterAndSortSessions`。逐点（更正 §2.34）：

- 类型逐字：
  ```ts
  export type SortMode = "threaded" | "recent" | "relevance";
  export type NameFilter = "all" | "named";
  export interface ParsedSearchQuery {
    mode: "tokens" | "regex";
    tokens: { kind: "fuzzy" | "phrase"; value: string }[];
    regex: RegExp | null;
    error?: string;
  }
  export interface MatchResult { matches: boolean; score: number }
  ```
  → **`parseSearchQuery` 返回的是对象而非字符串数组**；`matchSession` 返回 `MatchResult` 而非
  `number | undefined`。§2.34 的签名**全部作废**。
- 缓存：`const messageTextCache = new WeakMap<SessionInfo, { source, lower, normalized? }>()`；
  `getMessageText(session)` 以 `session.allMessagesText` 为 `source`，变了才重算 `lower`；
  `getNormalizedMessageText` 惰性算 `lower.replace(/\s+/g," ").trim()`。
- 分层常量：`TIER_NAME=0`、`TIER_TEXT=1`、`TIER_FUZZY=2`、`TIER_WEIGHT=1_000_000`；
  `tierScore(tier, score = 0) = tier * TIER_WEIGHT + score`。
- `normalizeWhitespaceLower(text) = text.toLowerCase().replace(/\s+/g," ").trim()`。
- `matchLiteralToken(session, value)`（`value` 由调用方小写）：
  ```ts
  const name = session.name?.toLowerCase() ?? "";
  if (name.includes(value)) return tierScore(TIER_NAME, fuzzyMatch(value, name).score);
  if (session.id.toLowerCase().includes(value)) return tierScore(TIER_TEXT);
  if (getMessageText(session).lower.includes(value)) return tierScore(TIER_TEXT);
  if (session.cwd.toLowerCase().includes(value)) return tierScore(TIER_TEXT);
  return undefined;
  ```
  - **id 也参与 literal 匹配**（§2.34 漏了 id，只写了 messages/cwd）。
- `matchFuzzyToken(session, value)`：先 `matchLiteralToken(session, value.toLowerCase())`；
  否则遍历 `[session.name ?? "", session.cwd]` 求 `fuzzyMatch(value, field)`，取 **命中且 score 最小** 者，
  返回 `tierScore(TIER_FUZZY, best)`。**不跨字段**、**不匹配 id/messages**。
- `matchPhraseToken(session, phrase)`（phrase 已 `normalizeWhitespaceLower`）：
  name（normalized）→ `tierScore(TIER_NAME, fuzzyMatch(phrase, name).score)`；id → TIER_TEXT；
  `getNormalizedMessageText(session)` → TIER_TEXT；`normalizeWhitespaceLower(cwd)` → TIER_TEXT。
- `matchRegex(session, regex)`：`session.name && regex.test(name)` → TIER_NAME；
  否则 `regex.test(id) || regex.test(allMessagesText) || regex.test(cwd)` → TIER_TEXT；否则 undefined。
  - **`regex.test` 有 `lastIndex` 状态**（带 `g`/`y` 标志时），但这里构造时只有 `"i"`，无状态。
- `hasSessionName(session) = Boolean(session.name?.trim())`。
- `parseSearchQuery(query)`：
  ```ts
  const trimmed = query.trim();
  if (!trimmed) return { mode: "tokens", tokens: [], regex: null };
  if (trimmed.startsWith("re:")) {
    const pattern = trimmed.slice(3).trim();
    if (!pattern) return { mode: "regex", tokens: [], regex: null, error: "Empty regex" };
    try { return { mode: "regex", tokens: [], regex: new RegExp(pattern, "i") }; }
    catch (err) { return { mode: "regex", tokens: [], regex: null,
                           error: err instanceof Error ? err.message : String(err) }; }
  }
  // token 模式：逐字符扫描 trimmed
  const tokens = []; let buf = "", inQuote = false, hadUnclosedQuote = false;
  const flush = (kind) => { const v = buf.trim(); buf = ""; if (!v) return; tokens.push({ kind, value: v }); };
  for (let i = 0; i < trimmed.length; i++) {
    const ch = trimmed[i];
    if (ch === '"') { if (inQuote) { flush("phrase"); inQuote = false; } else { flush("fuzzy"); inQuote = true; } continue; }
    if (!inQuote && /\s/.test(ch)) { flush("fuzzy"); continue; }
    buf += ch;
  }
  if (inQuote) hadUnclosedQuote = true;
  if (hadUnclosedQuote) {
    return { mode: "tokens", regex: null,
             tokens: trimmed.split(/\s+/).map(t => t.trim()).filter(t => t.length > 0)
                            .map(t => ({ kind: "fuzzy" as const, value: t })) };
  }
  flush(inQuote ? "phrase" : "fuzzy");                 // ← inQuote 此时必为 false，等价 flush("fuzzy")
  return { mode: "tokens", tokens, regex: null };
  ```
  - 引号**不保留**；空短语被丢弃；`re:` 前缀**区分大小写**；`"re:foo"`（带引号）走 token 模式。
  - 未闭合引号 → 回退为**纯空白分词**（已收集的短语 token 全部丢弃）。
- `matchSession(session, parsed)`：
  ```ts
  if (parsed.mode === "regex") {
    if (!parsed.regex) return { matches: false, score: 0 };
    const score = matchRegex(session, parsed.regex);
    return score === undefined ? { matches: false, score: 0 } : { matches: true, score };
  }
  let totalScore = 0;
  for (const token of parsed.tokens) {
    let score;
    if (token.kind === "phrase") {
      const phrase = normalizeWhitespaceLower(token.value);
      if (!phrase) continue;
      score = matchPhraseToken(session, phrase);
    } else score = matchFuzzyToken(session, token.value);
    if (score === undefined) return { matches: false, score: 0 };
    totalScore += score;
  }
  return { matches: true, score: totalScore };
  ```
- `filterAndSortSessions(sessions, query, sortMode, nameFilter = "all")`：
  ```ts
  const nameFiltered = nameFilter === "all" ? sessions : sessions.filter(s => matchesNameFilter(s, nameFilter));
  const trimmed = query.trim();
  if (!trimmed) return nameFiltered;                                  // 空查询：原序返回（不做排序）
  const parsed = parseSearchQuery(query);
  if (parsed.error) return [];
  if (sortMode === "recent") { ...保序过滤... }
  // relevance（以及 threaded 带查询）：
  scored.sort((a, b) => a.score !== b.score ? a.score - b.score : b.session.modified.getTime() - a.session.modified.getTime());
  ```
  - `matchesNameFilter(s, "named")` = `hasSessionName(s)`；`"all"` 恒真。
  - **`threaded` 带查询时也走 relevance 排序**（无单独分支）。
  - 排序用 `Array.prototype.sort`（V8 稳定排序），分数相同再按 `modified` 降序；`modified` 也相同时保持输入序
    → Rust 用 `sort_by`（**稳定**）而不是 `sort_unstable_by`。
- 测试 `session-selector-search.test.ts` 14 项覆盖上述全部行为；Rust 侧 `fuzzyMatch` 必须与 tui 单元逐值一致。

#### 2.60.5 `skill-invocation-message.ts` / `branch-summary-message.ts` / `compaction-summary-message.ts` / `custom-entry.ts` / `custom-message.ts`

（§2.13–§2.16、§2.21 基本正确，以下补充/更正）

**`skill-invocation-message.ts`（68 行）**

- `ParsedSkillBlock` 来自 `core/agent-session.ts`：`{ name: string; location: string; content: string; userMessage: string | undefined }`。
- 继承 `Box`，`super(outputPad, 1, t => theme.bg("customMessageBg", t))`。
- **collapsed 行只有一个 Text**（`Text(line, 0, 0)`，paddingX=0，因为 Box 已有 paddingX）：
  `theme.fg("customMessageLabel", "\x1b[1m[skill]\x1b[22m ") + theme.fg("customMessageText", name) +
   theme.fg("dim", ` (${keyText("app.tools.expand")} to expand)`)`
  - 注意 label 后**一个空格在 label 的 fg 内**（`"[skill]\x1b[22m "`），不是因为 Text 分隔。
- expanded：`Text(label, 0, 0)` + `Markdown(`**${name}**\n\n${content}`, 0, 0, markdownTheme, {color: t => theme.fg("customMessageText", t)})`。
- `invalidate()` → `super.invalidate(); updateDisplay();`；`setExpanded` 无论是否变化都 `updateDisplay()`（无守卫）。
- `setOutputPad` 只 `setPaddingX`，**不重建**。
- 鼠标：`MouseRegion(content, event => { if (event.type !== "click" || event.button !== "left") return undefined;
  this.setExpanded(!this.expanded); return { handled: true }; })`。

**`branch-summary-message.ts`（71 行）**

- 顺序：`Text(fg("customMessageLabel", "\x1b[1m[branch]\x1b[22m"), 0, 0)` → `Spacer(1)` → 内容。
- 内容 expanded：`Markdown("**Branch Summary**\n\n" + summary, ...)`；
  collapsed：单个 `Text(theme.fg("customMessageText","Branch summary (") + theme.fg("dim", keyText("app.tools.expand"))
  + theme.fg("customMessageText"," to expand)"), 0, 0)`（三段分别着色）。
- `BranchSummaryMessage = { role: "branchSummary"; summary; fromId: string | null; timestamp }`。

**`compaction-summary-message.ts`（72 行）**

- `tokenStr = message.tokensBefore.toLocaleString()`（默认 locale 千分位；Rust 需 `js_to_locale_string`）。
- 顺序同 branch：label `[compaction]` + `Spacer(1)`。
- expanded header：`` `**Compacted from ${tokenStr} tokens**\n\n` ``。
- collapsed：`fg("customMessageText", `Compacted from ${tokenStr} tokens (`)` + `fg("dim", keyText(...))` +
  `fg("customMessageText", " to expand)")`。

**`custom-entry.ts`（69 行）**

- `CustomEntryComponent extends Container`；**`setOutputPad` 没有变化守卫**（总是 `rebuild()`），
  `setExpanded` 有守卫（`if (this._expanded !== expanded)`）。
- `rebuild()`：
  1. `this.clear()`（清空所有子节点，包括 Spacer）；
  2. `this.customComponent = undefined`；
  3. `try { component = renderer(entry, { expanded: this._expanded }, theme) } catch (error) { ... }`
     - 错误分支：`Box(this.outputPad, 1, t => theme.bg("customMessageBg", t))` 内含
       `Text(theme.fg("error", `[${entry.customType}] renderer failed: ${message}`), 0, 0)`；
       `message = error instanceof Error ? error.message : String(error)`。
  4. `if (!component) return;`（**不追加任何东西** → `hasContent()` 为 false → 宿主跳过该条目）；
  5. `addChild(new Spacer(1)); addChild(component);`。
- `hasContent() = this.customComponent !== undefined`。

**`custom-message.ts`（114 行）**

- 构造：`super()`（Container）→ `addChild(new Spacer(1))` → **创建 `this.box = new Box(1, 1, t => theme.bg("customMessageBg", t))`**
  （Box 初始 paddingX=1）→ `this.rebuild()`。
- `rebuild()` 与 §2.21 的关键差异：
  ```ts
  if (this.customComponent) { this.removeChild(this.customComponent); this.customComponent = undefined; }
  this.removeChild(this.box);                       // Box 每次都先从容器摘掉
  if (this.customRenderer) {
    try { const component = this.customRenderer(this.message,
              { expanded: this._expanded, outputPad: this.outputPad }, theme);
          if (component) { this.customComponent = component; this.addChild(component); return; } } catch {}
  }
  this.addChild(this.box);
  this.box.clear();
  this.box.setPaddingX(this.outputPad);             // ← 用 outputPad 覆盖 Box 的 paddingX
  this.box.addChild(new Text(label, 0, 0));
  this.box.addChild(new Spacer(1));
  this.box.addChild(new Markdown(text, 0, 0, this.markdownTheme, { color: t => theme.fg("customMessageText", t) }));
  ```
  - **renderer 成功时 Box 不在容器里**，也不加 Spacer 之外的东西（Spacer(1) 始终在 index 0）。
  - label = `theme.fg("customMessageLabel", `\x1b[1m[${customType}]\x1b[22m`)`。
  - 正文：`typeof content === "string"` → 原样；否则 `content.filter(c => c.type === "text").map(c => c.text).join("\n")`。
- `setExpanded` **有**变化守卫；`setOutputPad` **有**守卫（`if (this.outputPad !== outputPad)`）——
  §2.21 写「总是 rebuild」是错的。
- `invalidate()` → `super.invalidate(); this.rebuild();`。
- 测试 `custom-message.test.ts`（1 项）已在 §7.1；注意断言 `optionsSeen` 精确等于 `[{expanded:false, outputPad:1}]`
  → 构造时**恰好调用一次** renderer（`rebuild` 只在构造里调一次）。

#### 2.60.6 `armin.ts`（384 行）

- 导出：`ARMIN_WIDTH = 31`、`ARMIN_HEIGHT = 36`、`isArminPixel`、`ArminComponent`。
- `BITS` 恰好 **144 字节**（`36 × ceil(31/8) = 36 × 4`），LSB-first，**1 = 背景、0 = 前景**（XBM 习惯）。
- `BYTES_PER_ROW = 4`；`DISPLAY_HEIGHT = ceil(36/2) = 18`。
- `isArminPixel(x, y)`：
  ```ts
  if (y >= HEIGHT) return false;                                  // ← 有上界检查，无 x 检查
  const byteIndex = y * BYTES_PER_ROW + Math.floor(x / 8);
  const bitIndex = x % 8;
  return ((BITS[byteIndex] >> bitIndex) & 1) === 0;
  ```
  - `x >= 31` 时 `Math.floor(x/8) = 3`（x ∈ 24..31），仍读到有效字节但 bitIndex 越界 → 结果不可靠；
    实际调用方 `getChar` 只传 `x < 31`。Rust 侧建议同样不做 x 边界检查（保持未定义行为一致）或直接返回 false
    并在注释说明（测试不覆盖）。
- `getChar(x,row)`：`upper && lower → "█"`、`upper → "▀"`、`lower → "▄"`、否则 `" "`。
- `render(width)`（**权威**）：
  ```ts
  if (width === this.cachedWidth && this.cachedVersion === this.gridVersion) return this.cachedLines;
  const padding = 1;
  const availableWidth = width - padding;
  this.cachedLines = this.currentGrid.map((row) => {
    const clipped = row.slice(0, availableWidth).join("");                 // ← 注意 <=0 时 slice(0,<=0) = []
    const padRight = Math.max(0, width - padding - clipped.length);
    return ` ${theme.fg("accent", clipped)}${" ".repeat(padRight)}`;
  });
  const message = "ARMIN SAYS HI";
  const msgPadRight = Math.max(0, width - padding - message.length);
  this.cachedLines.push(` ${theme.fg("accent", message)}${" ".repeat(msgPadRight)}`);
  this.cachedWidth = width; this.cachedVersion = this.gridVersion;
  return this.cachedLines;
  ```
  - **没有 `Math.max(1, width-1)`**（§2.44 写有是错的）；`width=0` 时 `availableWidth=-1`，
    `slice(0,-1)` 会**去掉最后一列**（JS 语义），Rust 用 `usize` 时必须特判（`width==0` → 空行 + 空格）。
  - `ARMIN SAYS HI` **不截断**（只补右侧空格；宽度不足时该行超宽）。§2.44 写「要截断」是错的。
  - 行内容里 `clipped.length` 用的是 **UTF-16 长度**（半块字符都是 BMP 单 code unit，等价于字符数）。
  - `theme.fg("accent", clipped)` 即使 `clipped` 为空也输出 `\x1b[...m\x1b[39m`。
- 构造：`effect = EFFECTS[Math.floor(Math.random() * EFFECTS.length)]`；`finalGrid = buildFinalGrid()`；
  `currentGrid = createEmptyGrid()`；`initEffect()`；`startAnimation()`。
- 帧率：`glitch` 60fps，其它 30fps；**定时器未 `unref()`**。
- 各 effect 的精确步进量：`typewriter` 3 像素/帧（行优先）；`scanline` 1 行/帧；`fade` 15 像素/帧（随机序）；
  `dissolve` 20 像素/帧（随机序）；`crt` 每帧 `expansion++`，`top = midRow - expansion`、
  `bottom = midRow + expansion`（`midRow = floor(18/2) = 9`），返回 `expansion > DISPLAY_HEIGHT`（即 19 帧后停）；
  `rain` 每列独立（`drop.y` 初始 `-floor(random*18*2)`，落定后 `drop.settled = 18 - targetRow`、
  `drop.y = -floor(random*5) - 1`，下落时画 `"▓"`）；`glitch` 前 8 帧：每行 30% 概率水平位移
  （`offset = floor(random*7)-3`，`slice(offset).concat(slice(0,offset))` 再 `slice(0,31)`）、
  否则 20% 概率整行替换为随机行，最后返回干净帧并结束。
- `tickRain` 的 `allSettled` 初值 true，任一列未落定则 false（空图也会立刻结束）。
- `dispose()` → `stopAnimation()`（`clearInterval` + `interval = null`）。
- `updateDisplay()` 仅 `gridVersion++`（不触发 render；渲染由 `ui.requestRender()` 驱动）。

**黄金数据（Node 22 实测，可直接作为 Rust 单测夹具）**：

`buildFinalGrid()` 的 18 行（每行 31 列，`█`/`▀`/`▄`/空格）：

```
        ▄▄▄▄                   
         ▀▄ ▀▄                 
           █  ▀▄▄              
     ▄▀▀▀▀▀     █              
     █▄   ▄▄▄▄▀▀▀▀▄            
   ▄▀  ▀▀▀  ▄▄▄▄▀▀ █           
   █              ▄█▄          
   ▀▄   ▄▄▄▀▀▀▀▀▀▀▀▄█  ▄▄▄▄▄   
 ▄▄▄▀▀▀▀▄▄▄▄▄▄▄▄▄▄█▄ ▄▀   ▄ ▀▄ 
 █▄▄▄▄        ▀▀▀▀  █     ▀█ █ 
     ██ ▄█      █   █ ▄      █ 
     ▀█████▄▄▄▄███▄▄▀▄▀▄▄▄  ▄▀ 
       ▀██████▀▀▀▀   ▀▄▄▄▄▄▀   
         ▀████████▀  ▄▄ ▄▄▄    
           ▀█████▀  ██▀ ██▄    
                    ██  ▄▄▄    
                     █  ▀█▀    
                      ▀▀▀
```

`render(width)` 的可见宽度实测（`theme.fg` 不计宽）：

| width | 像素行可见宽 | `ARMIN SAYS HI` 行可见宽 |
|---|---|---|
| 0 | **31**（`slice(0,-1)` → 30 列 + 1 前导空格 → 溢出） | **14**（溢出：`1 + 13`） |
| 1 | 1 | 14（溢出） |
| 2 | 2 | 14（溢出） |
| 10 | 10 | 14（溢出） |
| 13 | 13 | 14（溢出） |
| 14 | 14 | 14（刚好） |
| 15 | 15 | 15 |
| 20 | 20（所有行） | 20 |
| 31 | 31 | 31 |
| 32 | 32 | 32 |
| 40 | 40 | 40 |

→ **Rust 必须复刻 `width < 14` 时消息行溢出**（这是 TS 的既有行为，不能「顺手修正」），
以及 `width === 0` 时像素行 31 列的 `slice(0,-1)` 语义（`avail = -1`）。
另外：18 行像素行在 `width >= 32` 时全部等宽 = width；`ARMIN SAYS HI` 行同理。

#### 2.60.7 `easter-egg-3d.ts`（1292 行）与 `easter-egg-3d.lazy.ts`（27 行）

**lazy 文件（更正 §2.46 的签名）**：

```ts
function playEasterEgg3d(tui: TUI, egg: EasterEgg3d): boolean {     // ← 私有、同步、返回 boolean
  if (!(tui instanceof TuiAltScreen)) return false;
  if (tui.hasOverlay()) return true;
  const screen = tui.getScreenLines();
  import("./easter-egg-3d.ts").then(
    (module) => module.playEasterEgg3d(tui, screen, egg),
    () => {},
  );
  return true;
}
export function playKianaLogo3d(tui: TUI, column: number, row: number): void { playEasterEgg3d(tui, { kind: "kiana-logo", column, row }); }
export function playArmin3d(tui: TUI): boolean { return playEasterEgg3d(tui, { kind: "armin" }); }
```

- **`playKianaLogo3d` 返回 `void`**（不是 boolean）；`playArmin3d` 返回 `boolean`。
- **已有 overlay 时返回 `true` 但什么都不播**（`hasOverlay()` 分支）；非 alt-screen 返回 `false` 并放弃。
- 动态导入失败静默。

**`easter-egg-3d.ts` 的导出面**（更正 §2.45 的接口名）：

```ts
export type EasterEgg3d = { kind: "kiana-logo"; column: number; row: number } | { kind: "armin" };
export async function playEasterEgg3d(tui: TUI, screen: readonly string[], egg: EasterEgg3d): Promise<void>;
export class EasterEgg3dAnimation implements Component { ... }
```

- **没有 `EasterEgg3dContext` 接口**（§2.45 与之冲突处作废）；颜色从 `theme` 与 `tui.queryTerminalColors` 现取。
- 模块级 `let playing = false`；`playEasterEgg3d` 进入即置 true，`if (tui.hasOverlay()) { playing = false; return; }`。
- 颜色：
  ```ts
  const reported = await tui.queryTerminalColors({ timeoutMs: 100 });     // 只 await 这一处
  const dark = theme.appearance === "dark";
  const colors = {
    foreground: toRgbTuple(reported.foreground, toRgb(theme.colors.text)),
    background: toRgbTuple(reported.background, dark ? [0,0,0] : [255,255,255]),
  };
  const model = egg.kind === "armin" ? arminModel(toRgb(theme.colors.accent))
                                     : kianaLogoModel({ column: egg.column, row: egg.row });
  const animation = new EasterEgg3dAnimation(tui, screen, model, colors, () => { playing = false; overlay.hide(); });
  const overlay = tui.showOverlay(animation, { anchor: "top-left", width: "100%", maxHeight: "100%" });
  ```
  - **没有 `Promise.race` 的外部 timeout**（超时由 `queryTerminalColors` 自身负责）。
  - `theme.appearance` 决定兜底背景（§2.45 写「light 主题下反转」意思相同）。
- `Model` 结构：`{ columns, rows, blocks: {home: Cell3, color}[] , cameraDistance, radius, widthShare, puzzleMoves(random), origin? }`；
  `radius = Math.hypot(columns/2, rows/2, 1 + DEPTH/2)`。
- `kianaLogoModel`：4×4，`KIANA_LOGO_PIXELS = ["ccc.","b.c.","bb.y","b..y"]`，`c=CORAL(228,138,122)`、
  `b=BLUE(79,142,179)`、`y=YELLOW(234,182,93)`，`cameraDistance=10`、`widthShare=0.35`、
  `puzzleMoves = random < 0.4 ? 2 : 1`、`origin` 传入。
- `arminModel(color)`：31×36，`isArminPixel(column,row)` 为真才建块，`cameraDistance=80`、`widthShare=0.45`、
  `puzzleMoves = Math.round(blockCount * (0.03 + random * 0.04))`、`origin: undefined`。
- `shuffleSteps(model, cycle)`：**与 §2.45 的描述不同**（真正的实现）：
  ```ts
  let positions = model.blocks.map(b => b.home);
  const steps = [positions];
  const lastMoved = new Set<number>();
  let random = cycle * 7_919 + 1;
  const next = () => hash(random++);
  const key = ([x,y,z]) => ((z + 1) * rows + y) * columns + x;
  for (let step = 0; step < PUZZLE_STEPS; step++) {
    const occupied = new Set(positions.map(key));
    const nextPositions = [...positions];
    const moved = new Set<number>();
    const moveCount = model.puzzleMoves(next());
    for (let move = 0; move < moveCount; move++) {
      const candidates = [];
      for (let block = 0; block < positions.length; block++) {
        if (moved.has(block)) continue;
        for (const [dx,dy,dz] of PUZZLE_MOVES) {
          const target = [x+dx, y+dy, z+dz];
          if (out of 0..columns-1 / 0..rows-1) continue;
          if (z outside -1..1 || occupied.has(key(target))) continue;
          candidates.push({ block, target });
        }
      }
      const fresh = candidates.filter(c => !lastMoved.has(c.block));
      const pool = fresh.length > 0 ? fresh : candidates;
      const choice = pool[Math.floor(next() * pool.length)];
      if (!choice) break;
      occupied.add(key(choice.target)); nextPositions[choice.block] = choice.target; moved.add(choice.block);
    }
    lastMoved.clear(); for (const block of moved) lastMoved.add(block);
    positions = nextPositions; steps.push(positions);
  }
  return steps;
  ```
  - **`hash`**（权威实现，见上文 §2.60.7 的代码块）：
    `x = imul(value ^ 0x9e3779b9, 0x85ebca6b)` → `x ^= x >>> 13` → `x = imul(x, 0xc2b2ae35)` →
    `x ^= x >>> 16` → 返回 `(x >>> 0) / 0x100000000`（**0..1 浮点**，不是 u32）。
    注意第一段是「先异或再乘」，第二段是「先乘再异或右移」——不是对称的三段 imul。
  - `PUZZLE_MOVES` 是 10 个方向（+x,-x,+y,-y,+x,-x,+y,-y,+z,-z）。
  - `PUZZLE_CYCLE = PUZZLE_STEP * PUZZLE_STEPS + PUZZLE_RETURN + PUZZLE_HOLD = 0.3*12 + 1 + 0.6 = 5.2`。
- 其它常量（权威，逐字来源）：
  `DISSOLVE_END = FLY_START + WAVE_SPREAD + WAVE_JITTER + DUST_DURATION = 0.1+0.55+0.12+0.35 = 1.12`；
  `STAR_TINTS = [[255,255,255],[170,195,255],[255,225,170],[255,190,205]]`；
  `FIRST_FRONT_VIEW = PUZZLE_START + PUZZLE_STEP*PUZZLE_STEPS + PUZZLE_RETURN + PUZZLE_HOLD/2 = 4.4+3.6+1+0.3 = 9.3`；
  `SPIN_SPEED = (Math.KIANA * 2) / PUZZLE_CYCLE`；`SPIN_ALIGNMENT = Math.KIANA*2 - (baseSpinPhase(FIRST_FRONT_VIEW) % (Math.KIANA*2))`。
  注意：`PUZZLE_START = FLY_START + FLY_DURATION + 3 = 0.1+1.3+3 = 4.4`。
- 关键函数（逐字公式，Rust 必须照抄）：
  ```ts
  normalize(v) = v / Math.hypot(...)                    // LIGHT = normalize([-0.45,-0.6,0.75]); HALF_VECTOR = normalize([L0,L1,L2+1])
  clamp01(v) = v < 0 ? 0 : v > 1 ? 1 : v
  smooth(v) = t^3 * (t*(t*6-15)+10)                     // quintic, t=clamp01(v)
  easeOut(v) = 1 - (1-t)^3, t=clamp01(v)
  mix(a,b,amount) = a + (b-a)*amount                     // 逐分量
  isLight([r,g,b]) = 0.2126r + 0.7152g + 0.0722b > 128
  glyphInk(text, seed):
    if (text.trim() === "") return 0
    if (/^[.,:;'`\-_·]$/.test(text)) return 2
    if (/^[─-╿]$/.test(text)) return 3
    return 4 + Math.floor(seed * 3)
  baseSpinPhase(time):
    elapsed = time - FLY_START; if (elapsed <= 0) return 0
    u = elapsed / SPIN_RAMP
    u < 1 → SPIN_SPEED * SPIN_RAMP * (u^6 - 3u^5 + 2.5u^4)
    else  → SPIN_SPEED * (SPIN_RAMP*0.5 + elapsed - SPIN_RAMP)
  spinPhase(t) = baseSpinPhase(t) + SPIN_ALIGNMENT * smooth((t - FLY_START)/FLY_DURATION)
  spinAngle(t) = spinPhase(t) - SPIN_LINGER * Math.sin(spinPhase(t))
  ```
- `parseScreen` 的 SGR 解析细节（与 §2.45 一致，补充）：
  - `params === "" ? ["0"] : params.split(";")`；
  - 每个 code 再按 `":"` 切子参数（`38:2::r:g:b` 风格）；`parts.length > 1` 时用子参数并**丢弃 NaN**；
    否则从后续 `codes` 取 `kind===5 ? 2 : kind===2 ? 4 : 1` 个参数并 `i += count`；
  - 支持 `0/2/22/7/27/39/49/30-37/40-47/90-97/100-107` 与 `38/48`；
  - `pushText`：`for (const { segment } of segmenter.segment(text))`（`Intl.Segmenter` 默认 grapheme）；
    `width===0` 或 `cells.length + glyphWidth > width` 时跳过；`inverse` 交换 fg/bg；
    `dim` 时 `cellFg = mix(cellFg, cellBg ?? background, 0.4)`；宽字符再 push 一个 `{...cell, text:"", width:0}`
    （**浅拷贝**，因此后续修改 `owner` 的 `delay` 需要特殊处理，见 `prepareCells` 的 `owner` 逻辑）。
  - 转义扫描：`\x1b[` 到首个 `0x40..0x7e` 字节；`]`/`_`/`P`/`^` 字符串序列到 BEL 或 `\x1b\\`；
    其它 `\x1bX` 跳过 2 字符。
- `render(width)`（每帧）：
  - `height = Math.max(1, this.tui.terminal.rows)`；
  - 尺寸变化时 `prepareCells(width, foreground)` + `prepareStars(width, height)`；
  - `dissolveTime`：正常 = `elapsed()`；退出 = `Math.min(exit.time, DISSOLVE_END) * (1 - progress)`；
  - `pose`、`hintAlpha`、`starAlpha`、`offsets` 见 §2.45 与上文；
  - `boxes`：`block.home[0] - columns/2 + dx`、`block.home[1] - rows/2 + dy`，z 为 `dz ± DEPTH/2`；
  - `raster.render(width, height, pose, boxes, background, isLight(background) ? 0.3 : 0)`；
  - `ansiCache` 键 = `pack(fg) * 2 + (bg ? 1 : 0)`；`ansi()` 里 `rgbColor((key>>16)&255, ...)` +
    `foregroundAnsi/backgroundAnsi(color, theme.getColorMode())`；
  - `emit(text, fg, bg)`：**前景只在 `text !== " "` 时发**（空格不改前景），背景**总是**在 key 变化时发；
    首帧 `currentFg = -1`、`currentBg = -1`；
  - 行尾 `\x1b[0m`；`lines.push(`${line}\x1b[0m`)`。
  - `close()`：`if (this.exit) { this.finish(); return; }` —— 第二次调用**直接结束**（跳过退出动画）。
  - 定时器：`setInterval(FRAME_MS)` + `unref()`；条件 `(exit && exitProgress() >= 1) || performance.now() - lastRender > 1000`
    → `finish()`（1 秒未渲染自动收尾）。
  - `handleInput`：`tui.select.cancel` **或** `app.clear` → `close()`。
  - `handleMouse`：`event.type === "click"` → `close()`；恒返回 `{ handled: true, render: false }`。
  - `invalidate()`：`screenWidth = -1; screenHeight = -1; ansiCache.clear()`。
- `prepareCells` 的 dust 时序：
  ```ts
  centerX = origin ? origin.column + columns/2 : width/2;
  centerY = origin ? origin.row + rows/4 : height/2;
  farthest = Math.hypot(Math.max(centerX, width - centerX), Math.max(centerY, height - centerY) * 2);
  seed = hash(row * 65_537 + column);
  delay = FLY_START + (Math.hypot(column-centerX, (row-centerY)*2) / farthest) * WAVE_SPREAD + seed * WAVE_JITTER;
  cell.seed = row * 65_537 + column; cell.ink = glyphInk(owner.text, seed);
  ```
  宽字符的第二个单元 `delay = owner.delay`、`ink = glyphInk(owner.text, seed)`。
  `origin` 存在时，把 header 区域（`origin.row..origin.row+ceil(rows/2)-1`、`origin.column..+columns-1`）清成
  `text=" "、width=1、ink=0、bg=undefined`。
- `prepareStars(width,height)`：`hash(index*3 + 0x51ed) >= 0.018` 跳过；
  `random(salt) = hash(index*7 + salt*0x9e37)`；
  `glyph = BRAILLE[DOT_BITS[floor(random(1)*8)]]`、`tint = STAR_TINTS[floor(random(2)*4)]`、
  `strength = 0.1 + random(3)*0.12`、`phase = random(4)*2π`、`speed = 0.4 + random(5)*0.8`。
- `BlockRaster.visibleFaces` 的隐藏面判定（**【更正】** 贴邻面要求**完全覆盖**，不是两个 `<=`）：
  背面 `(origin[a] - plane) * side <= 0` 剔除；贴邻面用
  ```ts
  const plane = side > 0 ? box.max[a] : box.min[a];
  const covered = boxes.some((other) =>
    other !== box &&
    (side > 0 ? other.min[a] : other.max[a]) === plane &&   // 必须精确相接（浮点相等）
    other.min[u] <= box.min[u] && other.max[u] >= box.max[u] &&
    other.min[v] <= box.min[v] && other.max[v] >= box.max[v]);
  ```
  即 `other` 在 u/v 两个轴上**包含**本面（`<=` + `>=` 成对），仅一端重叠不算遮挡。
  面的屏幕包围盒：四角投影后 `minX = max(0, floor(minX))`、`minY = max(0, floor(minY))`、
  `maxX = min(dotWidth-1, ceil(maxX))`、`maxY = min(dotHeight-1, ceil(maxY))`；
  `maxX < minX || maxY < minY` 的面直接丢弃。透视：`perspective = (pose.scale * cameraDistance) / (cameraDistance - z)`，
  `screenX = pose.centerX + x * perspective`。
- 光照逐字：`diffuse = max(0, n·LIGHT)`；`rim = max(0, 0.8nx - 0.3nz)`；
  `specular = lightBg ? 0 : max(0, n·HALF_VECTOR)^24 * 0.6 * 255`；
  `light` 的两分支系数不同（浅色背景分支的 diffuse 系数是 **0.45**、rim 系数 **0.1**，并与 1 取 min）—— **权威**：
  ```ts
  const light = lightBackground ? Math.min(1, 0.55 + diffuse * 0.45 + rim * 0.1)
                                : 0.45 + diffuse * 0.78 + rim * 0.25;
  ```
  `red/green/blue = min(255, color * light + specular)`。
- 雾化：`fog = clamp01(0.15 - cameraDistance * (1 - depth) * 0.12) * (light ? 0.5 : 1)`；
  颜色 = `face.color + (background - face.color) * fog`。
- Halo（仅 light 背景）：可分离 tent 滤波，`weightsX = tent(6)`、`weightsY = tent(3)`；
  水平 pass 的权重再 `/8`；垂直 pass 后对「单元格自身覆盖」取 `max(count/8, coverage)`；
  `amount = round(strength * min(1,coverage)^0.7 * 32) / 32`；颜色量化到 `LOGO_COLOR_STEP = 5`。
- **`Float32Array` 语义**：`raster.rgb`、`haloAmount`、`haloRgb`、`depth` 都是 `Float32`，每次累加/写入都经过
  f32 舍入。Rust 必须用 `f32` 存这些缓冲（中间计算可用 f64，但**写回缓冲时转 f32**），否则逐字符对拍会差 1 个色阶。

#### 2.60.8 `keybinding-hints.ts`（48 行）

- 导出：`KeyTextFormatOptions`、`formatKeyText`、`keyText`、`keyDisplayText`、`keyHint`、`rawKeyHint`。
- `formatKeyPart`：`process.platform === "darwin" && part.toLowerCase() === "alt"` → `"option"`；
  `capitalize` 时 `charAt(0).toUpperCase() + slice(1)`。
- `formatKeyText`：`key.split("/").map(k => k.split("+").map(formatKeyPart).join("+")).join("/")`
  —— **先 `/` 后 `+`**，空段也会保留（`"a//b"` → `"a//b"`）。
- `formatKeys(keys)`（私有）：`keys.length === 0 ? "" : formatKeyText(keys.join("/"), options)`。
- `keyText` / `keyDisplayText` / `keyHint` / `rawKeyHint` 见 §2.8（正确）。
- **【逐字】** `"option"`、`" "`（keyHint 中的空格）。

#### 2.60.9 `settings-submenu.ts`（258 行）补充

- `SelectSubmenu` 签名（**8 个位置参数**，§2.52.1 写 9 个是笔误；第 7 是 `onSelectionChange`，第 8 是
  `submenuOptions`）：
  ```ts
  constructor(title: string, description: string, options: SelectItem[], currentValue: string,
              onSelect: (value: string) => void, onCancel: () => void,
              onSelectionChange?: (value: string) => void,
              submenuOptions?: SelectSubmenuOptions)
  ```
  `SelectSubmenuOptions = { searchable?: boolean; layout?: SelectListLayoutOptions }`（**导出接口**）。
- 布局：`Text(theme.bold(theme.fg("accent", title)), 0, 0)` → 有 description 时 `Spacer(1)` +
  `Text(theme.fg("muted", description), 0, 0)` → searchable 时 `Spacer(1)` + `Input` →
  `Spacer(1)` → `SelectList(options, Math.min(options.length, 10), getSelectListTheme(), listLayout)` →
  `Spacer(1)` → `Text(theme.fg("dim", hint), 0, 0)`。
  hint 逐字：searchable → `"  Type to filter · Enter to select · Esc to go back"`；
  否则 → `"  Enter to select · Esc to go back"`（**两个前导空格**）。
- `listChildIndex = this.children.length`（在 `addChild(this.selectList)` **之前**记录）。
- `buildSelectList(options, preselect)`：`findIndex(o => o.value === preselect)`；`!== -1` 时
  `setSelectedIndex(idx)`；设 `onSelect = item => onSelectCb(item.value)`、`onCancel = onCancelCb`；
  `onSelectionChange` 存在时 `list.onSelectionChange = item => cb(item.value)`。
- `applyFilter(query)`：`query ? fuzzyFilter(allOptions, query, item => `${item.label} ${item.description ?? ""}`)
  : allOptions`；`buildSelectList(filtered, "")`；**原地替换** `this.children[this.listChildIndex]`。
  - 注意过滤后 `preselect` 是 `""`（选中项回到第一项）。
- `handleInput`：有 searchInput 时，只有 `tui.select.up/down/confirm/cancel` 四类键交给列表，其它全部进搜索框
  并重新 `applyFilter`；无 searchInput 时全部交给列表。
- `SteppedSubmenu`（见 §2.52.2，本节确认）：`buildStep` 第 7 参数 `undefined`；第 8 参数仅当
  `step.searchable || step.layout` 时构造；`render/handleInput/invalidate` 全委托 `activeComponent`。
  `startAtStep` 与 `initialContext` 均来自 `opts`；`context = { ...(opts.initialContext ?? {}) }`。

---

## 3. 用户可见表面（逐字）

> 本单元**不定义** CLI 参数、不定义 slash 命令解析（属 ca-cli / interactive-mode），但它定义了这些命令
> 打开的面板的**全部文案与键位**。下表是按「用户看到什么」整理的验收清单。

### 3.1 触发这些面板的 slash 命令（由宿主提供，本单元只负责面板）

| 命令 | 打开的本单元组件 |
|---|---|
| `/settings` | `SettingsSelectorComponent` |
| `/config`（含 `/extensions`、`/skills`、`/prompts`、`/themes` 别名） | `ConfigSelectorComponent` |
| `/model` | `ModelSelectorComponent` |
| `/models`（scoped models） | `ScopedModelsSelectorComponent` |
| `/thinking` | `ThinkingSelectorComponent` |
| `/resume` | `SessionSelectorComponent` |
| `/tree` | `TreeSelectorComponent` |
| `/fork` | `UserMessageSelectorComponent` |
| `/login` | `OAuthSelectorComponent` → `LoginDialogComponent`（Radius 时 `createLoginMenuSelector`） |
| `/theme` | `ThemeSelectorComponent` |
| `/images`（show images 开关） | `ShowImagesSelectorComponent` |
| `/trust` | `TrustSelectorComponent` |
| 首次启动 | `FirstTimeSetupComponent` |
| `/armin` | `ArminComponent` |
| `ctrl+shift+.`（或宿主绑定） | `playEasterEgg3d` |

### 3.2 全部用户可见字符串（按文件，逐字）

> 凡与源码不一致即测试失败。`${...}` 表示插值，`\n` 为真实换行。

**dynamic-border / themed-text / markdown-transform**：无字符串（只输出 `─`）。

**kiana-logo**：无文字（纯图形）；`kianaWordmark()` 输出 `P`（coral）+ `i`（yellow）。

**keybinding-hints**：分隔符 `" "`；`keyHint` 为 `dim(键) + muted(" " + 描述)`。

**show-images-selector**
```
Yes            Show images inline in terminal
No             Show text placeholder instead
```

**earendil-announcement**
```
kiana has joined Earendil
Read the blog post:
https://mariozechner.at/posts/2026-04-08-ive-sold-out/
```
图片 fallback 文本（`Image` 的 `imageFallback`）：`clankolas.png`（文件名）。

**theme-selector**：主题名列表 + `(current)`。

**bordered-loader**：`message` 由调用方提供；提示 `cancel`。

**skill-invocation-message**
```
[skill]
[skill] <name> (<expandKey> to expand)
```

**custom-entry**：`[<customType>] renderer failed: <message>`

**branch-summary-message**
```
[branch]
**Branch Summary**

<summary>
Branch summary (<expandKey> to expand)
```

**compaction-summary-message**
```
[compaction]
**Compacted from <tokens> tokens**

<summary>
Compacted from <tokens> tokens (<expandKey> to expand)
```

**user-message**：无额外文案；OSC 133 序列 `\x1b]133;A\x07` / `\x1b]133;B\x07` / `\x1b]133;C\x07`。

**mermaid**
```
Mermaid diagram not rendered: <warning>[ (+N more)]
```

**extension-input**
```
<title>            （倒计时时：<title> (<n>s)）
<description>      （可选）
[confirm] submit  [cancel] cancel
```

**visual-truncate**：`<hint>` 由调用方提供；省略号 `...`。

**custom-message**：`[<customType>]`。

**radius-login-selector**：菜单文本为 provider 名；选中项以 `→ ` 前缀 + 微光。

**extension-selector**
```
<title>            （倒计时时：<title> (<n>s)）
<description>      （可选）
→ <selected>
  <other>
↑↓ navigate  <confirm> select  <cancel> cancel
```

**status-indicator**：见 §2.24 表格。

**trust-selector**
```
Project trust
<cwd>
Saved decision: none|trusted (<path>)|untrusted (<path>)|trusted (inherited from <path>)|untrusted (inherited from <path>)
Current session: trusted|untrusted
→ ✓ <label>
↑↓ navigate  <confirm> save  <cancel> cancel
```

**extension-editor**
```
<title>
<description>      （可选）
[confirm] submit  [newline] newline  [cancel] cancel  [external] external editor
外部编辑器启动时 stdout：
Launching external editor: <command>
Kiana will resume when the editor exits.
```

**diff**：行前缀 `-<n> `、`+<n> `、` <n> `；省略号与提示无。

**custom-editor**：无文案（继承 Editor 的提示）。

**first-time-setup**：见 §2.48。

**thinking-selector**：见 §2.50。

**user-message-selector**
```
  No user messages found
Fork from Message
Select a user message to copy the active path up to that point into a new session
› <message>
  Message <i> of <N>
  (<i>/<N>)
```

**assistant-message**
```
Thinking...
Response was truncated before completion.
Operation aborted
Request was aborted      （被过滤，不显示）
Unknown error
Error: <message>
```

**bash-execution**
```
$ <command>
<output lines>
... <n> more lines (<expandKey> to expand)
(<expandKey> to collapse)
(cancelled)
(exit <n>)
Output truncated. Full output: <path>
Running... (<cancelKey> to cancel)
```

**oauth-selector**
```
Select provider to configure:
Select provider to logout:
→ <name> [subscription|account|API key] • not configured
→ <name> [..] • <OAuth|API key> configured
→ <name> [..] ✓ configured
→ <name> [..] ✓ env: <VARS>
→ <name> [..] ✓ <source>
  No providers available
  No providers logged in. Use /login first.
  No matching providers
  (<i>/<N>)
```

**settings-submenu**
```
Step <i>/<N> · <title>
<description>
  Type to filter · Enter to select · Esc to go back
  Enter to select · Esc to go back
```

**session-selector-search**：无 UI 字符串；`Empty regex` 为唯一错误文本（作为解析结果返回）。

**footer**
```
<dim>~/path (branch) • session-name</dim>
<dim>↑1.2k ↓3.4k R5.6k W7.8k CH12.3% $0.123 (sub)</dim>  <context>12.3%/128k (auto)</context>  <right>(provider) model-id • high</right>
ext-status-1 ext-status-2          ← 第 3 行**不套 dim**（只有其省略号是 dim），见 §2.60.3
```

- 第 2 行实际由 `theme.fg("dim", statsLeft) + theme.fg("dim", remainder)` 两段拼成（上下文百分比段
  自带 `error`/`warning` 色或**无色**），因此恰好有 4 个 `dim` 边界。
- `formatTokens` 的 `k`/`M` 后缀与 `↑ ↓ R W CH $ % / (auto) • → (sub) no-model • thinking off` 均逐字；
  详见 §2.60.3。

**login-dialog**
```
Login to <providerName>
> <submitted value>
Login cancelled
Cmd+click to open | Ctrl+click to open
Enter code: <userCode>
to open browser and copy code | to open browser | to copy code
Copying code... | Browser open requested | Code copied to clipboard | Copy code manually
e.g., <placeholder>
to cancel | to cancel, | to submit | to close
```
（`titleOverride` / `providerNameOverride` 可覆盖；`parentheses` `(` `)` 包住 keyHint。逐字清单见 §2.60.2。）

**armin**：`ARMIN SAYS HI`

**tree-selector**
```
  Session Tree
Type to search:
  No entries found
  (0/0)
  (<i>/<N>)[ [no-tools]| [user]| [labeled]| [all]][ [+label time]]
Label (empty to remove):
```
帮助行：`move`、`page`、`branch`、`copy`、`label`、`label time`、`filters`、`cycle` + 键位缩写
（`↑`/`↓`/`←`/`→`/`pgup`/`pgdn`）。

**easter-egg-3d**：`<cancelKey> to return`（退出提示，居中于 `height-2` 行）。

**easter-egg-3d.lazy**：无字符串。

**config-selector**：见 §2.41。

**settings-selector**：见 §2.40。

**session-selector**：见 §2.42。

**tool-execution**
```
<toolTitle bold name>
... (<n> more lines, <expandKey> to expand)
```

**model-selector**
```
Scope: all | scoped
<tab> scope (all/scoped)
Only showing models from configured providers. Use /login to add providers.
→ ✓ <model-id> (<distinct-name>) [<provider>] · default
  No matching models
  Model Name: <name>
  (<i>/<N>)
  <confirm> to select · <save> to set as default · <cancel> to cancel
Model refresh timed out; showing cached models.
Model catalogs refreshed.
Could not refresh <key>; showing cached models.
Could not refresh <n> model catalogs (<keys>); showing cached models.
Could not refresh model catalogs: <message>
```

**scoped-models-selector**
```
Model Configuration
Session-only. <save> to save to settings.
→ ✓ <provider>/<id> (<distinct>) [<provider>]|[unavailable]
○ <provider>/<id> ...
  Model Name: <name>
  Model unavailable
  No matching models
  (<i>/<N>)
  <confirm> toggle · <enableAll> all · <clearAll> clear · <toggleProvider> provider · <up>/<down> reorder · <save> save · <n>/<m> enabled[ · <k> unavailable][ (unsaved)]
all enabled
```

### 3.3 键位表（`APP_KEYBINDINGS`，来自 `core/keybindings.ts`，逐字）

> 本单元所有组件都用 `getKeybindings().matches()` / `keybindings.matches()` 查询，因此这张表就是
> 「按什么键有效」的唯一来源。**下表已按源码逐字核对（2026-10 核对）**，`description` 是源码原文
> （用于 `keybindings.json` 的校验错误与文档，不直接显示在 UI 上）。

**平台判定（关键）**：

```ts
export function useWindowsKeybindings(platform = process.platform, env = process.env): boolean {
  return platform === "win32" || (platform === "linux" && Boolean(env.WSL_DISTRO_NAME || env.WSL_INTEROP));
}
const windowsKeybindings = useWindowsKeybindings();       // 模块加载时求值一次
```

→ **WSL 也算「windows keybindings」**（`app.model.cycleBackward`、`app.message.followUp`、
`app.message.dequeue`、`app.clipboard.pasteImage`、以及 `KEYBINDINGS` 的 4 个覆盖都受影响）。
但 `app.suspend` 与 `app.tree.foldOrUp`/`unfoldOrDown` 用的是**原始 `process.platform`**（WSL 不算 win32）。

| id | `defaultKeys`（逐字） | `description`（逐字） |
|---|---|---|
| `app.interrupt` | `"escape"` | `Cancel or abort` |
| `app.clear` | `"ctrl+c"` | `Clear editor` |
| `app.exit` | `"ctrl+d"` | `Exit when editor is empty` |
| `app.suspend` | `process.platform === "win32" ? [] : "ctrl+z"` | `Suspend to background` |
| `app.thinking.cycle` | `"shift+tab"` | `Cycle thinking level` |
| `app.thinking.save` | `"ctrl+s"` | `Save thinking level` |
| `app.model.cycleForward` | `"ctrl+p"` | `Cycle to next model` |
| `app.model.cycleBackward` | `windowsKeybindings ? "alt+p" : "shift+ctrl+p"` | `Cycle to previous model` |
| `app.model.select` | `"ctrl+l"` | `Open model selector` |
| `app.tools.expand` | `"ctrl+o"` | `Toggle tool output` |
| `app.thinking.toggle` | `"ctrl+t"` | `Toggle thinking blocks` |
| `app.session.toggleNamedFilter` | `"ctrl+n"` | `Toggle named session filter` |
| `app.editor.external` | `"ctrl+g"` | `Open external editor` |
| `app.message.copy` | `"ctrl+x"` | `Copy selection or last assistant message` |
| `app.message.followUp` | `windowsKeybindings ? "ctrl+q" : "alt+enter"` | `Queue follow-up message` |
| `app.message.dequeue` | `windowsKeybindings ? "alt+q" : "alt+up"` | `Restore queued messages` |
| `app.clipboard.pasteImage` | `windowsKeybindings ? "alt+v" : "ctrl+v"` | `Paste files on macOS, images, or text from clipboard` |
| `app.session.new` | `[]` | `Start a new session` |
| `app.session.tree` | `[]` | `Open session tree` |
| `app.session.fork` | `[]` | `Fork current session` |
| `app.session.resume` | `[]` | `Resume a session` |
| `app.tree.foldOrUp` | darwin: `["alt+left","ctrl+left"]`；其它: `["ctrl+left","alt+left"]` | `Fold tree branch or move up` |
| `app.tree.unfoldOrDown` | darwin: `["alt+right","ctrl+right"]`；其它: `["ctrl+right","alt+right"]` | `Unfold tree branch or move down` |
| `app.tree.editLabel` | `"shift+l"` | `Edit tree label` |
| `app.tree.toggleLabelTimestamp` | `"shift+t"` | `Toggle tree label timestamps` |
| `app.session.togglePath` | `"ctrl+p"` | `Toggle session path display` |
| `app.session.toggleSort` | `"ctrl+s"` | `Toggle session sort mode` |
| `app.session.rename` | `"ctrl+r"` | `Rename session` |
| `app.session.delete` | `"ctrl+d"` | `Delete session` |
| `app.session.deleteNoninvasive` | `"ctrl+backspace"` | `Delete session when query is empty` |
| `app.models.save` | `"ctrl+s"` | `Save model selection` |
| `app.models.enableAll` | `"ctrl+a"` | `Enable all models` |
| `app.models.clearAll` | `"ctrl+x"` | `Clear all models` |
| `app.models.toggleProvider` | `"ctrl+p"` | `Toggle all models for provider` |
| `app.models.reorderUp` | `"alt+up"` | `Move model up in order` |
| `app.models.reorderDown` | `"alt+down"` | `Move model down in order` |
| `app.tree.filter.default` | `"ctrl+d"` | `Tree filter: default view` |
| `app.tree.filter.noTools` | `"ctrl+t"` | `Tree filter: hide tool results` |
| `app.tree.filter.userOnly` | `"ctrl+u"` | `Tree filter: user messages only` |
| `app.tree.filter.labeledOnly` | `"ctrl+l"` | `Tree filter: labeled entries only` |
| `app.tree.filter.all` | `"ctrl+a"` | `Tree filter: show all entries` |
| `app.tree.filter.cycleForward` | `"ctrl+o"` | `Tree filter: cycle forward` |
| `app.tree.filter.cycleBackward` | `"shift+ctrl+o"` | `Tree filter: cycle backward` |

**`KEYBINDINGS` 覆盖**（`export const KEYBINDINGS = { ...TUI_KEYBINDINGS, <4 个覆盖>, ...APP_KEYBINDINGS }`）：

```ts
"tui.editor.undo": process.platform === "win32" ? "ctrl+z" : windowsKeybindings ? "alt+z" : "ctrl+-"
"tui.altScreen.previousPrompt": windowsKeybindings ? "ctrl+up" : ["ctrl+shift+up", "ctrl+up"]
"tui.altScreen.nextPrompt":     windowsKeybindings ? "ctrl+down" : ["ctrl+shift+down", "ctrl+down"]
"tui.altScreen.search":         windowsKeybindings ? "ctrl+f" : "ctrl+shift+f"
```

**TUI 层（`TUI_KEYBINDINGS`，本单元大量使用的，逐字）**：

| id | 默认键 |
|---|---|
| `tui.select.up` / `tui.select.down` | `up` / `down` |
| `tui.select.pageUp` / `tui.select.pageDown` | `pageup` / `pagedown` |
| `tui.select.confirm` | `enter` |
| `tui.select.cancel` | **`["escape", "ctrl+c"]`**（两个键！） |
| `tui.input.tab` | `tab` |
| `tui.input.newLine` | `shift+enter`, `ctrl+j` |
| `tui.input.submit` | `enter` |
| `tui.editor.cursorLeft` / `cursorRight` | `["left","ctrl+b"]` / `["right","ctrl+f"]` |
| `tui.editor.cursorUp` / `cursorDown` | `up` / `down` |
| `tui.editor.historyPrevious` / `historyNext` | **`[]` / `[]`（默认无键）** |
| `tui.input.deleteCharBackward` | `backspace` |

> **关键**：`tui.editor.historyPrevious` / `historyNext` 在 `TUI_KEYBINDINGS` 里默认是**空数组**，
> 但 `coding-agent` 在构造 `KEYBINDINGS` 时**没有**给它们加默认键 —— 那么 Editor 的 ↑/↓ 翻历史是怎么来的？
> 答：`tui/components/editor.ts` 在 `handleInput` 里**先**匹配 `tui.editor.historyPrevious/Next`（空则永不命中），
> **然后**匹配 `tui.editor.cursorUp/cursorDown`，回落条件逐字为：
> ```ts
> if (kb.matches(data, "tui.editor.cursorUp")) {
>   if (this.isOnFirstVisualLine() && (this.isEditorEmpty() || this.historyIndex > -1 || this.state.cursorCol === 0))
>     this.navigateHistory(-1);
>   else if (this.isOnFirstVisualLine()) this.moveToLineStart();
>   else this.moveCursor(-1, 0);
>   return;
> }
> if (kb.matches(data, "tui.editor.cursorDown")) {
>   if (this.historyIndex > -1 && this.isOnLastVisualLine()) this.navigateHistory(1);
>   else if (this.isOnLastVisualLine()) this.moveToLineEnd();
>   else this.moveCursor(1, 0);
>   return;
> }
> ```
> 注意 `cursorUp` 的三个「允许进入历史」条件（空编辑器 / 已在翻历史 / 光标在第 0 列），仅「首行」不够。
> `custom-editor.ts` 的 handleInput 第 5 步显式把 `historyPrevious/historyNext` 直接交给 `super.handleInput`，
> 正是为了支持用户把它们显式绑定到 ↑/↓（此时无条件翻历史，不走光标移动）。
> Rust 侧必须保留「显式绑定 → 直接翻历史；未绑定 → 交给 cursorUp/Down 的回落逻辑」这条路径。
> `tui.select.cancel` 含 `ctrl+c`，意味着所有选择器的取消都能用 Ctrl+C（`app.clear` 也是 `ctrl+c`，
> 但选择器先消费）；`CustomEditor` 里 `ctrl+c` 走 `app.clear`（编辑器清空），二者不冲突因为组件树不同层。

> 其余 `tui.*` 键（`tui.editor.undo/cursorUp/cursorDown/wordLeft/wordRight/lineStart/lineEnd/delete/...`、
> `tui.altScreen.*`）由 **tui-core** 单元定义；本单元只消费上表这些。`tui.select.pageUp/pageDown` 在
> `session-selector.ts` 与 `tree-selector.ts` 中被使用，必须存在。

**`KeybindingsManager`**（`core/keybindings.ts` 重新导出 tui 的实现）：

- `KeybindingsManager.create(agentDir?)`：读 `$agentDir/keybindings.json`（默认 agent dir），按 `KEYBINDINGS`
  校验；非法 → 回退默认并（由宿主）报错。
- `matches(data: string, binding: Keybinding): boolean`。
- `getKeys(binding): KeyId[]`。
- 用户可覆盖任何 id（包括 `app.*` 与 `tui.*`）；值为字符串或字符串数组。

### 3.4 渲染细节（ANSI / 颜色 / 宽度）

**颜色 token**（本单元出现的全部，均由 `theme.fg`/`theme.bg` 解析）：

```
accent, text, muted, dim, border, borderMuted, borderAccent, error, warning, success,
thinkingText, thinkingHigh, userMessageText, userMessageBg,
customMessageText, customMessageBg, customMessageLabel,
bashMode, toolTitle, toolOutput, toolPendingBg, toolSuccessBg, toolErrorBg,
toolDiffContext, toolDiffRemoved, toolDiffAdded, mdLink, selectedBg
```

（**【补充】** `borderAccent` 出现在 `tree-selector` 的 compaction 行（`[compaction: Nk tokens]`），
`session-selector` 的 `DynamicBorder((s) => theme.fg("accent", s))` 用的是 `accent` 而非此 token。）

**ANSI 序列**：

- 前景色：`\x1b[38;2;r;g;bm`（truecolor）或 `\x1b[38;5;nm`（256）或基础 30-37/90-97（16）。
- 背景色：`\x1b[48;2;…`/`\x1b[48;5;n`/40-47/100-107。
- 重置：`\x1b[0m`（全部）、`\x1b[39m`（前景）、`\x1b[49m`（背景）。
- 样式：bold `\x1b[1m` / `\x1b[22m`（关闭）；italic `\x1b[3m`/`\x1b[23m`；underline `\x1b[4m`/`\x1b[24m`；
  inverse `\x1b[7m`/`\x1b[27m`；strikethrough `\x1b[9m`/`\x1b[29m`。
- 超链接（OSC 8）：`\x1b]8;;<url>\x1b\\<text>\x1b]8;;\x1b\\`（`hyperlink()`）与
  手写版 `\x1b]8;;<url>\x07<text>\x1b]8;;\x07`（`login-dialog.ts`）。
- shell 集成（OSC 133）：`\x1b]133;A\x07`、`\x1b]133;B\x07`、`\x1b]133;C\x07`。

**Spinner**（`Loader`）：帧序列 `["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"]`，间隔 **80 ms**，
输出格式 `` `${frame} ${messageColorFn(message)}` ``（`Loader.render` 返回 `["", <该行>]`）；
`renderInBorder` 取第 2 行。**Rust 必须用同一帧表与间隔。**

**布局宽度常量**：

| 组件 | 常量 |
|---|---|
| `DynamicBorder` | 满宽 |
| show-images / theme-selector | `SelectList` maxVisible 5 / 10；layout `{12, 32}` |
| thinking-selector | maxVisible = items 数量；layout `{12, 32}` |
| extension-selector | 无 maxVisible（全部） |
| oauth-selector | maxVisible 8 |
| model-selector | maxVisible 10 |
| settings-selector | `SettingsList(items, 10, …)`；`MODEL_PICKER_LAYOUT {12, 46}` |
| settings-submenu | maxVisible `min(items, 10)` |
| session-selector | **硬编码 10**（`SessionList.maxVisible = 10`，构造参数无此项；`options` 里也没有 `maxVisible`） |
| tree-selector | `maxVisibleLines = max(5, floor(terminalHeight/2))`；每层缩进 **3** 列（`TREE_GUTTER_WIDTH=2` 是水平视口为 gutter 预留的宽度，不是缩进） |
| config-selector | `maxVisible = max(5, (terminalHeight ?? 24) - 8)`；chrome 8 |
| user-message-selector | maxVisible 10 |
| bash-execution | 折叠预览 20 物理行；上下文截断 2000 行 / 50 KiB |
| tool-execution | fallback 预览 10 逻辑行；图片默认 60 列 |
| armin | 宽 31、高 36（显示 18 行）；像素行 `" "+clip(宽度-1，可为负)+右补`；`ARMIN SAYS HI` **不截断**（width<14 溢出） |
| earendil 公告 | 图片 56 列 |
| footer | minPadding 2 |

**盒模型**：

- `Box(paddingX, paddingY, bgFn)`：左右各 paddingX 空格，上下各 paddingY 空行，整行套 bgFn。
- `Text(text, paddingX, paddingY)`：行内换行后左右 paddingX、上下 paddingY。
- `Container`：子组件顺序拼接，无额外间距。
- `Spacer(n)`：n 行空行。
- 消息组件的 `outputPad` 只作用于**内容**（`Box`/`Text` 的 paddingX），不作用于 `DynamicBorder`。

**省略号**（**【精读更正】**）：`truncateToWidth` 默认 `"..."`（三个 ASCII 句点）。实际用法：

| 位置 | 省略号 |
|---|---|
| `session-selector` 的 header 三段右对齐（`right`/`left`） | `""`（不截断出省略号） |
| `session-selector` 的 header `hintLine1`/`hintLine2`（含删除确认、状态消息） | `"…"`（U+2026） |
| `session-selector` 的 list 空态文案 | `"…"` |
| `session-selector` 的 list 消息文本 | `"…"` |
| `session-selector` 的 list 整行最终截断 / 滚动条 | **无第三参**（默认 `"..."`/ 未传） |
| `config-selector` 的 header 两行 | `""` |
| `config-selector` 的 group/subgroup 行 | `""` |
| `config-selector` 的 item 行 | `"..."`（三个 ASCII 点） |
| `config-selector` 的搜索输入行 | 由 `Input` 自己处理 |
| `tree-selector` 的 `SearchLine` / `No entries found` / 滚动条 | 无第三参（默认） |
| `tree-selector` 的 `renderHorizontalViewport` | `""` |

**结论**：`…`（U+2026）只用于 **session-selector 的 hint 行与 list 文本**；`config-selector` 的 header
与列表**都不是** `…`（§3.4 旧文写「config-selector 的 header/list 用 `…`」是错的）。**不可混用。**

### 3.5 动画/阈值

| 位置 | 值 |
|---|---|
| `Loader` 帧间隔 | 80 ms |
| `CountdownTimer` 滴答 | 1000 ms，`ceil(timeoutMs/1000)` 起 |
| `ArminComponent` 帧率 | 30 fps（glitch 60 fps） |
| `radius` 微光 | 50 ms；每字符 10 字符/秒；4 色 × 4 字符带宽 |
| `easter-egg-3d` | 30 fps（`FRAME_MS = 1000/30`） |
| `model-selector` 刷新超时 | 15 000 ms |
| `login-dialog` 终端颜色查询 | 100 ms |
| `user-message-selector` 空列表自动取消 | 100 ms |
| `tree-selector` 空树自动取消 | 100 ms |
| `session-selector` 状态消息 | info 2000 ms，delete 失败 3000 ms，加载失败 4000 ms |
| footer 上下文百分比阈值 | >70 warning，>90 error |

---

## 4. 外部格式（磁盘 / 环境变量 / 子进程 / 协议）

本单元**直接**触盘/触网的地方很少，但以下文件与协议必须逐字一致。

### 4.1 会话文件（`.kiana/sessions/*.jsonl`）

由 ca-session 负责读写，本单元**读取** `SessionInfo` 与条目：

```ts
interface SessionInfo {
  path: string; id: string; cwd: string; name?: string;
  parentSessionPath?: string; created: Date; modified: Date;
  messageCount: number; firstMessage: string; allMessagesText: string;
}
```

- 文件是 **JSONL**（每行一个 JSON 对象），首行是 `{"type":"session", ...}` 头，其余为条目。
- 本单元依赖的条目 `type`：`message`（`role: "user"|"assistant"|"toolResult"|"bashExecution"`）、
  `custom_message`、`compaction`、`branch_summary`、`session_info`、`model_change`、
  `thinking_level_change`、`custom`、`context_edit`、`label`、`usage`。
- `SessionListProgress = (loaded: number, total: number, partialSessions?: SessionInfo[]) => void`。
- **只读**：本单元不写会话文件（删除/重命名由宿主完成；本单元只发起 `onSelect(path)` / `deleteSessionFile`）。

### 4.2 `settings.json`（本单元通过 config-selector 写模式串）

- 路径：用户级 `~/.kiana/agent/settings.json`；项目级 `<cwd>/.kiana/settings.json`（`CONFIG_DIR_NAME = ".kiana"`）。
- config-selector 写入的字段：
  - `extensions: string[]`、`skills: string[]`、`prompts: string[]`、`themes: string[]`（顶层绝对开关）。
  - `packages: (string | { source: string; extensions?: string[]; skills?: string[]; prompts?: string[];
     themes?: string[]; autoload?: boolean })[]`。
- **模式串前缀**（逐字）：`+` 启用、`-` 禁用、`!` 显式继承（内部剥离时用）。写入前会剥离既有 `!`/`+`/`-`
  前缀再比较。
- 写入由 `SettingsManager` 完成（ca-config-models），本单元只调用其 API；但**产生的字符串**（如
  `+extensions/foo.ts`、`-skills/bar`、`package@1.0.0`）属于外部格式的一部分。
- `agent/settings.json` 中的路径显示：`~/.kiana/agent/settings.json`（`~` + `/` + CONFIG_DIR_NAME + `/agent/settings.json`）；
  项目显示 `<CONFIG_DIR_NAME>/settings.json`。

### 4.3 信任决策（`trust.json` / settings）

由 `getProjectTrustOptions(cwd, { includeSessionOnly: true })` 产生：

```ts
{ label: string; trusted: boolean; updates: { path: string; decision: boolean | null }[]; savedPath?: string }
```

- 选项（顺序）：`Trust`、可选 `Trust parent folder (<parent>)`、可选 `Trust (this session only)`、
  `Do not trust`、可选 `Do not trust (this session only)`。
- `decision: null` 表示「清除该级决策」（父目录选项把父设 true、当前设 null）。
- 本单元只渲染与返回 `updates`，落盘由 ca-cli/trust-manager 完成。

### 4.4 键位文件 `keybindings.json`

- `KeybindingsManager.create(agentDir)` 读 `~/.kiana/agent/keybindings.json`（或 `ENV_AGENT_DIR`）。
- 结构：`{ "<binding id>": ["key1","key2"] }`；未知 id 忽略；非法键忽略。
- `KEYBINDING_NAME_MIGRATIONS` 迁移旧名（如 `app.message.copyAll` → `app.message.copy`）。
- 本单元通过 `getKeybindings().getKeys(id)` 渲染提示，所以**自定义键位必须反映在 UI 里**
  （`'<custom key>' + " " + desc`）。

### 4.5 环境变量

**【精读更正】** 本单元源码**直接**读取的环境变量只有 4 个（`grep process.env` 全量）：

| 变量 | 读取点 | 用途 |
|---|---|---|
| `HOME` / `USERPROFILE` | `footer.ts:165`、`tree-selector.ts:949` | footer pwd 与 tree 的 `formatToolCall` 路径缩写（`HOME \|\| USERPROFILE \|\| ""`） |
| `VISUAL` / `EDITOR` | `extension-editor.ts` | 外部编辑器命令（`VISUAL \|\| EDITOR`，Windows 回退 `notepad`，否则 `nano`） |

**注意（【更正】）**：

- `formatBaseDir`（`config-selector.ts`）用 **`os.homedir()`**，`shortenPath`（`session-selector.ts`）也用
  **`os.homedir()`** —— 二者**不读**环境变量（homedir 本身在 Node 里会读 `HOME`/`USERPROFILE`，但这是
  `node:os` 的行为，Rust 侧用 `dirs`/`home` crate 时不保证等价，需自行实现同样的回退链）。
- `tree-selector.ts` 的 `shortenPath`（`formatToolCall` 内的局部闭包）**是**读 `process.env.HOME ||
  process.env.USERPROFILE` —— **同一单元里三处路径缩写的实现并不一致**（两处 `homedir()`、一处 env）。
- **`ENV_AGENT_DIR` / `ENV_SESSION_DIR` 是 `config.ts` 里的常量名，不是环境变量名**；实际变量名是
  `` `${APP_NAME.toUpperCase()}_CODING_AGENT_DIR` `` 与 `` `${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR` ``，
  `APP_NAME` 默认 `"kiana"` → **`KIANA_CODING_AGENT_DIR`**、**`KIANA_CODING_AGENT_SESSION_DIR`**。
  它们由 ca-cli / core 读取，**本单元组件目录内没有任何文件读这两个变量**。
- 本单元也**不**读 `WSL_DISTRO_NAME`/`WSL_INTEROP`（WSL 判定在 `core/keybindings.ts` 的
  `tui.editor.undo` 别名的上游 ca-cli 层；组件目录无引用）。
- `process.platform === "darwin"` 在 3 处影响**用户可见文本**：`auth-url.ts:26`、`login-dialog.ts:152`
  → 点击提示选 `"Cmd+click to open"` / `"Ctrl+click to open"`；`keybinding-hints.ts:13`
  → `alt` 显示为 `option`。Rust 侧必须按目标平台复刻。
- `ANTHROPIC_*` 等：**不**由本单元读取；`oauth-selector` 只显示 `AuthCheck.source` 字符串。

### 4.6 子进程

| 场景 | 命令 | 参数/stdio | 失败处理 |
|---|---|---|---|
| 删除会话 | `trash` | `[sessionPath]`，若以 `-` 开头则 `["--", sessionPath]`；`spawnSync` + `encoding:"utf-8"` | `status !== 0` 且文件仍在 → `fs.unlink`；再失败 → `Failed to delete: <unlinkError> (trash: <hint>)`，hint 截断 200 字符 |
| 外部编辑器 | `$VISUAL` / `$EDITOR` / `notepad`(win32) / `nano` | 继承 stdio；win32 `shell: true`；临时目录 `kiana-editor-*`，文件 `prompt.md` | 退出码非 0 → 不修改内容；TUI `stop()` → 编辑 → `start()` + 全量重绘 |
| 打开浏览器 | `openBrowser(url)`（utils） | 平台探测 `open`/`xdg-open`/`start` | 静默失败 |
| git branch（footer） | `git symbolic-ref --short HEAD`（或 reftable 读取） | `execFile` | 失败显示无 branch |

### 4.7 终端能力探测（影响显示）

- `getCapabilities()`：`{ images: boolean, trueColor: boolean, ... }` —— 决定是否显示图片、
  `show-images` 设置项是否出现。
- `isAppleTerminalSession()`：决定 `supportsKianaLogo()` 与半块安全策略。
- `theme.getColorMode()`：`"truecolor" | "256" | "16"` —— 决定 `foregroundAnsi`/`backgroundAnsi` 的降级。
- `tui.queryTerminalColors({ timeoutMs: 100 })`：OSC 10/11 查询终端前景/背景色，供 3D 彩蛋与主题使用。
- 图片协议：`Image` 组件用 iTerm2 / Kitty / Sixel / 半块降级（由 tui-terminal 决定）；本单元只传
  `{ maxWidthCells, filename, fallbackColor }`。

### 4.8 扩展 UI 协议（跨进程不涉及，但跨单元）

`ExtensionUIContext` 的调用形状（**【精读更正】** 以 `core/extensions/types.ts` 为准；本单元的组件只被
下面 3 个方法使用，`ui.custom` **不经**本单元任何组件）：

```ts
// core/extensions/types.ts
export interface ExtensionUIDialogOptions {
  signal?: AbortSignal;          // 程序化关闭
  timeout?: number;              // 毫秒；对话框自动关闭并显示倒计时
}
ui.select(title: string, options: string[], opts?: ExtensionUIDialogOptions): Promise<string | undefined>;
ui.confirm(title: string, message: string, opts?: ExtensionUIDialogOptions): Promise<boolean>;
ui.input(title: string, placeholder?: string, opts?: ExtensionUIDialogOptions): Promise<string | undefined>;
ui.editor(title: string, prefill?: string): Promise<string | undefined>;   // ← 没有 opts 参数
ui.custom<T>(
  factory: (tui, theme, keybindings: KeybindingsManager, done: (result: T) => void) =>
    (Component & { dispose?(): void }) | Promise<Component & { dispose?(): void }>,
  options?: { overlay?: boolean; overlayOptions?: OverlayOptions | (() => OverlayOptions);
              onHandle?: (handle: OverlayHandle) => void },
): Promise<T>;
```

- **【更正】** `description` 与 `initialValue` **不是** `ExtensionUIDialogOptions` 的字段（旧稿写错）；
  `description` 属于各组件自己的 `ExtensionSelectorOptions` / `ExtensionInputOptions` /
  `ExtensionEditorOptions`，由宿主按需传（`showExtensionSelector` 只传 `tui` + `timeout` +
  `onToggleToolsExpanded`）。
- **【更正】** `ui.editor` 在宿主里落到 `showExtensionEditor(title, prefill)`，构造
  `new ExtensionEditorComponent(this.ui, this.keybindings, title, prefill, onSubmit, onCancel, undefined,
  settingsManager.getExternalEditorCommand())` —— 即 **options 恒为 `undefined`**、第 8 参是设置里的外部编辑器命令。
- **【更正】** `ui.custom` 的 factory 是 **4 参**（`tui, theme, keybindings, done`），返回
  `Component & { dispose?() }` 或 Promise；宿主 `showExtensionCustom` 负责 `showOverlay` /
  push dialog / `restoreEditor()` / `component.dispose?.()`。它**不**经过 `CustomEntryComponent`
  （旧稿说反了：`CustomEntryComponent` 只用于会话条目 `custom_entry` 的渲染）。
- 取消 → `undefined`（`ExtensionSelectorComponent` / `ExtensionInputComponent` / `ExtensionEditorComponent`
  的 `onCancel` 由宿主 resolve undefined）；`ui.confirm` 取消 → `false`。
- `opts.signal` 已 abort 时 **同步** resolve（`showExtensionSelector` 开头 `if (opts?.signal?.aborted)`）；
  否则 `addEventListener("abort", onAbort, { once: true })`，`finish` 里 `removeEventListener` + `selector.dispose()`。

### 4.9 图片资源

- `getBundledInteractiveAssetPath("clankolas.png")`（earendil 公告）。
- 非 PNG 图片经 `ensurePngTranscoder()`（`@silvia-odwyer/photon-node` WASM）转码为 PNG 后再交给 `Image`。
  转码完成回调触发 `invalidate()` + `requestRender()`。
- 本单元不落盘图片。
## 5. 跨单元接口

> Rust 侧建议：把下列类型放进共享 crate `kiana-core-types`（或 `kiana-tui` 的 `types` 模块），
> 本单元只依赖 trait/struct 定义，不依赖其实现细节。

### 5.1 本单元**消费**的类型（必须由其它单元提供）

**来自 tui（tui-core / tui-components / tui-text / tui-terminal）**：

```rust
trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn invalidate(&mut self);
    fn handle_input(&mut self, _data: &str) {}
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<MouseResult> { None }
    fn dispose(&mut self) {}
}
trait Focusable { fn focused(&self) -> bool; fn set_focused(&mut self, v: bool); }

struct Container;               // add_child/clear/remove_child/children
struct Box;                     // new(padding_x, padding_y, bg_fn), set_bg_fn, set_padding_x
struct Text;                    // new(text, padding_x, padding_y[, bg_fn]), set_text, set_padding_x, set_custom_bg_fn
struct TruncatedText;
struct Spacer;                  // new(n)
struct Markdown;                // new(text, padding_x, padding_y, theme, style, options)
struct Loader;                  // new(tui, spinner_color, message_color, message), stop, set_message, get_rendered_indicator
struct CancellableLoader;       // 同上 + signal + dispose
struct Input;                   // set_value/get_value/on_submit/on_change/focused
struct Editor;                  // set_text/get_text/on_submit/on_cancel/is_showing_autocomplete/render_top_border
struct SelectList;              // new(items, max_visible, theme, layout), set_selected_index, set_filter, handle_input, on_select/on_cancel/on_selection_change, get_selected_item
struct SettingsList;            // new(items, max_visible, theme, on_change, on_cancel, opts{enable_search})
struct MouseRegion;             // new(component, handler)
struct Image;                   // new(base64, mime, opts{fallback_color}, opts{max_width_cells, filename})
struct TuiMouseEvent { x: i32, y: i32, button: MouseButton, kind: MouseEventKind, .. }
enum MouseButton { Left, Right, Middle }
struct Tui;                     // request_render(force: bool), stop(), start(), terminal_rows()
struct TuiAltScreen;            // get_screen_lines(), has_overlay(), query_terminal_colors(timeout), show_overlay(...)
struct SelectItem { value: String, label: String, description: Option<String> }
struct SelectListLayoutOptions { min_primary_column_width: usize, max_primary_column_width: usize }
struct MarkdownTheme; struct MarkdownOptions { preserve_ordered_list_markers, preserve_backslash_escapes, transform, render_latex }
struct SettingItem { id, label, description, current_value, values, submenu }
struct Capabilities { images: bool, true_color: bool, .. }
struct Color; fn rgb(u8,u8,u8) -> Color; fn foreground_ansi(&Color, ColorMode) -> String;
fn background_ansi(&Color, ColorMode) -> String; fn mix_colors(a,b,t,space) -> Color;
fn parse_color(&str) -> Color; fn hyperlink(url,text) -> String; fn image_fallback(...) -> String;
fn visible_width(&str) -> usize; fn truncate_to_width(s, w, ellipsis) -> String;
fn slice_by_column(s, start, len, strict) -> String; fn wrap_text_with_ansi(s, w) -> Vec<String>;
fn strip_terminal_sequences(s) -> String; fn fuzzy_match(q,t) -> Option<Match>; fn fuzzy_filter(...) -> Vec<T>;
fn get_keybindings() -> &KeybindingsManager; fn matches_key(data, key_id) -> bool;
enum Key { Ctrl(char), Escape, Enter, Tab, .. }
```

**来自 kiana-ai（ai-core）**：

```rust
struct Model { id: String, name: String, provider: String, context_window: usize, reasoning: bool, .. }
fn models_are_equal(a,b) -> bool;
fn get_supported_thinking_levels(&Model) -> Vec<ThinkingLevel>;
enum ThinkingLevel { Off, Minimal, Low, Medium, High, Xhigh, Max }
struct Usage { input: u64, output: u64, cache_read: u64, cache_write: u64, cost: Cost }
struct AuthCheck { kind: AuthKind, source: Option<String> }
enum AuthKind { OAuth, ApiKey }
struct OAuthDeviceCodeInfo { verification_uri: String, user_code: String, .. }
struct AuthInfoLink { label: Option<String>, url: String }
struct AssistantMessage { content: Vec<ContentBlock>, stop_reason: StopReason, error_message: Option<String>, usage: Usage, .. }
enum ContentBlock { Text{text}, Thinking{thinking}, ToolCall{id,name,arguments}, Image{data,mime_type} }
enum StopReason { Stop, Length, ToolUse, Aborted, Error }
```

**来自 coding-agent core**：

```rust
struct SessionInfo { path, id, cwd, name, parent_session_path, created, modified, message_count, first_message, all_messages_text }
struct SessionTreeNode { entry: SessionEntry, children: Vec<SessionTreeNode>, label: Option<String>, label_timestamp: Option<String> }
struct CustomEntry { custom_type: String, .. }
enum SessionEntry { Message(..), CustomMessage(..), Compaction(..), BranchSummary(..), SessionInfo(..),
                    ModelChange(..), ThinkingLevelChange(..), Custom(..), ContextEdit(..), Label(..), Usage(..) }
type SessionListProgress = Box<dyn Fn(usize, usize, Option<&[SessionInfo]>)>;
struct ParsedSkillBlock { name, location, content, user_message }
struct ContextUsage { tokens: Option<u64>, context_window: u64, percent: Option<f64> }
struct ToolRenderContext<'a> { args: &'a Value, tool_call_id, invalidate: Box<dyn Fn()>, last_component, state: &'a mut Value,
                                cwd, execution_started, args_complete, is_partial, expanded, show_images, is_error,
                                duration_ms: Option<u64>, output_pad: usize }
struct MarkdownTransformContext<'a> { message_type, is_streaming, available_width, message_id, timestamp }
trait MessageRenderer { fn render(&self, msg, opts: MessageRenderOptions, theme) -> Option<Box<dyn Component>>; }
trait EntryRenderer   { fn render(&self, entry, opts: EntryRenderOptions, theme) -> Option<Box<dyn Component>>; }
struct TruncationResult { content: String, truncated: bool, .. }
fn truncate_tail(s, opts) -> TruncationResult;
fn format_tool_call_with_args(name, args, theme, expanded) -> String;
fn get_text_output(result, show_images) -> String;
fn format_read_line_range(offset, limit) -> String;
fn get_project_trust_options(cwd, include_session_only) -> Vec<ProjectTrustOption>;
struct ProjectTrustOption { label: String, trusted: bool, updates: Vec<ProjectTrustUpdate>, saved_path: Option<String> }
```

**来自 theme（ca-theme）**：

```rust
static THEME: ThemeProxy;                     // 全局；fg(k) -> String 闭包着色
struct Theme;
impl Theme {
  fn fg(&self, key: &str, s: &str) -> String;
  fn bg(&self, key: &str, s: &str) -> String;
  fn bold(&self, s: &str) -> String;      // 支持嵌套（关闭时用 22 而非 0）
  fn italic(&self, s: &str) -> String;
  fn underline(&self, s: &str) -> String;
  fn inverse(&self, s: &str) -> String;
  fn strikethrough(&self, s: &str) -> String;
  fn get_fg_ansi(&self, key: &str) -> String;     // 测试用：断言颜色出现次数
  fn get_bg_ansi(&self, key: &str) -> String;
  fn get_color_mode(&self) -> ColorMode;
  fn get_thinking_border_color(&self, level) -> Box<dyn Fn(&str)->String>;
  fn get_bash_mode_border_color(&self) -> Box<dyn Fn(&str)->String>;
}
fn get_markdown_theme() -> MarkdownTheme;
fn get_select_list_theme() -> SelectListTheme;
fn get_editor_theme() -> EditorTheme;
fn get_settings_list_theme() -> SettingsListTheme;
fn get_available_themes() -> Vec<String>;
fn parse_auto_theme_setting(s: &str) -> Option<(String, String)>;   // "light/dark"
const SYSTEM_THEME_NAME: &str = "system";
```

**注意 `theme.bold` 的嵌套语义**：`theme.bold(theme.fg("x", s))` 必须产生
`\x1b[1m` + 前景色 + s + `\x1b[22m`（或 `\x1b[0m` 后再补前景？）。测试
`status-indicator.test.ts` 断言 `topBorder.split(theme.getFgAnsi("thinkingHigh")).length === 5`，
说明**同一行里 foreground ANSI 出现 4 次**，即每次 `borderColor(...)` 调用都独立包裹（不是整行包一次）。
`Theme` 的 bold/italic 实现见 ca-theme 单元；本单元只依赖其输出可嵌套。

### 5.2 本单元**暴露**的接口（供 interactive-mode / 扩展宿主使用）

```rust
// 消息组件
pub struct UserMessageComponent;
pub struct AssistantMessageComponent;
pub struct ToolExecutionComponent;
pub struct BashExecutionComponent;
pub struct CustomMessageComponent;
pub struct CustomEntryComponent;
pub struct BranchSummaryMessageComponent;
pub struct CompactionSummaryMessageComponent;
pub struct SkillInvocationMessageComponent;

// 选择器 / 面板
pub struct SettingsSelectorComponent;
pub struct ConfigSelectorComponent;
pub struct ModelSelectorComponent;
pub struct ScopedModelsSelectorComponent;
pub struct ThinkingSelectorComponent;
pub struct SessionSelectorComponent;
pub struct TreeSelectorComponent;
pub struct UserMessageSelectorComponent;
pub struct OAuthSelectorComponent;
pub struct LoginDialogComponent;
pub struct ThemeSelectorComponent;
pub struct ShowImagesSelectorComponent;
pub struct TrustSelectorComponent;
pub struct FirstTimeSetupComponent;
pub struct ExtensionSelectorComponent;
pub struct ExtensionInputComponent;
pub struct ExtensionEditorComponent;

// 基础设施
pub struct FooterComponent;
pub struct CustomEditor;         // extends Editor
pub struct BorderedLoader;
pub struct DynamicBorder;
pub struct ArminComponent;
pub struct EarendilAnnouncementComponent;
pub struct AuthUrlComponent;
pub struct CountdownTimer;
pub struct StatusIndicator;      // + Working/Retry/Compaction/BranchSummary/IdleStatus
pub struct ThemedText;           // 【原文缺失】themed-text.ts
pub struct VisualLinePreview;    // 【原文缺失】visual-truncate.ts
pub struct SelectSubmenu;        // 【原文缺失】settings-submenu.ts
pub struct SteppedSubmenu;       // 【原文缺失】settings-submenu.ts
pub struct EasterEgg3dAnimation;// 【原文缺失】easter-egg-3d.ts

// 导出类型（TS 的 `export type` / `export interface`，Rust 侧对应 pub struct/enum）
pub enum EasterEgg3d { KianaLogo { column: usize, row: usize }, Armin }
pub enum FilterMode { Default, NoTools, UserOnly, LabeledOnly, All }          // tree-selector
pub enum StatusIndicatorKind { Working, Retry, Compaction, BranchSummary }
pub enum CompactionStatusReason { Manual, Threshold, Overflow }
pub enum TrustSelection { ... }                                               // Pick<ProjectTrustOption,"trusted"|"updates">
pub struct TrustSelectorOptions;
pub enum SortMode { Threaded, Recent, Relevance }                             // session-selector-search
pub enum NameFilter { All, Named }
pub struct ParsedSearchQuery;
pub struct MatchResult { pub matches: bool, pub score: i64 }
pub struct ModelsConfig;         // scoped-models-selector
pub struct ModelsCallbacks;      // scoped-models-selector
pub struct SettingsConfig;       // settings-selector
pub struct SettingsCallbacks;    // settings-selector
pub struct ExtensionSelectorOptions;
pub struct ExtensionInputOptions;
pub struct ExtensionEditorOptions;
pub struct FirstTimeSetupOptions;
pub struct FirstTimeSetupResult { pub theme: String, pub share_analytics: bool }
pub struct ToolExecutionOptions;
pub struct ToolRenderers;        // tool-execution.ts 的 re-export
pub struct RenderDiffOptions;    // diff.ts
pub struct VisualTruncateResult; // visual-truncate.ts
pub struct VisualLinePreviewOptions;
pub struct SelectSubmenuOptions;
pub struct SteppedSubmenuStep;
pub struct AuthSelectorProvider; // oauth-selector.ts
pub struct ScopedResolvedPaths;  // config-selector.ts
pub struct CustomEditorOptions;  // custom-editor.ts

// 函数
pub fn render_diff(diff_text: &str, options: Option<&RenderDiffOptions>) -> String;
pub fn truncate_to_visual_lines(text, max_visual_lines, width, padding_x, keep) -> VisualTruncateResult;
pub fn create_markdown_transform(...) -> impl Fn(&str, usize) -> String;
pub fn create_mermaid_markdown_transformer(opts) -> impl Fn(&str, &MarkdownTransformContext) -> String;
pub fn key_hint(kb, desc) -> String;
pub fn key_text(kb) -> String;
pub fn key_display_text(kb) -> String;
pub fn raw_key_hint(key, desc) -> String;
pub fn format_key_text(key, opts) -> String;      // 【原文缺失】keybinding-hints.ts
pub fn create_login_menu_selector(...) -> ExtensionSelectorComponent;
pub fn kiana_logo_lines() -> [String; 2];
pub fn supports_kiana_logo() -> bool;
pub fn kiana_wordmark() -> String;
// 3D 彩蛋有两个入口，语义不同（见 §2.60.7）：
//   - lazy 入口（同步）：非 alt-screen 立刻 false；已有 overlay 立刻 true 但不播；否则 kick off 动态加载并返回 true
//   - eager 入口（async）：真正播放；重复调用（模块级 `playing`）直接返回
// 【更正】lazy 里的 playEasterEgg3d 是**私有**，不是导出；Rust 侧如需对拍可以 pub(crate)
fn play_easter_egg_3d_lazy(tui: &mut Tui, egg: EasterEgg3d) -> bool;      // 私有
pub fn play_kiana_logo_3d(tui: &mut Tui, column: usize, row: usize);         // 返回 ()！
pub fn play_armin_3d(tui: &mut Tui) -> bool;
pub async fn play_easter_egg_3d(tui: Rc<...>, screen: &[String], egg: EasterEgg3d);  // 真正播放
pub fn format_auth_selector_provider_type(p) -> String;
pub fn format_auth_selector_provider_status(p) -> String;
// `formatDecision` 未导出（模块私有）；公开的只有 `getProjectTrustOptions(cwd)` 与组件本身。
pub fn format_session_date(date) -> String;       // 【更正】模块私有，仅测试可访问
pub fn shorten_path(path) -> String;              // 【更正】模块私有
// 【更正】以下三个在源码里均为**模块私有**，不属于公开 API：
//   format_base_dir / get_group_label / build_groups（config-selector.ts）
//   normalize_enabled / toggle / enable_all / clear_all / move / get_sorted_ids（scoped-models-selector.ts）
// 若 Rust 侧要单测它们，用 `pub(crate)` + `#[cfg(test)]` 提权，不要放进公共 crate 表面。
// `sanitize_status_text` 在 footer.ts 里是**模块私有**，不对外导出（本单元内部用）。
pub fn format_tokens(count) -> String;
pub fn format_cwd_for_footer(cwd, home) -> String;
pub fn parse_search_query(query) -> ParsedSearchQuery;
pub fn match_session(session, parsed) -> MatchResult;          // { matches: bool, score: i64 }
pub fn filter_and_sort_sessions(sessions, query, sort_mode, name_filter) -> Vec<SessionInfo>;
pub fn has_session_name(session) -> bool;                     // 【原文缺失】session-selector-search.ts
pub fn is_armin_pixel(x, y) -> bool;
pub const ARMIN_WIDTH: usize = 31;
pub const ARMIN_HEIGHT: usize = 36;
pub const TREE_GUTTER_WIDTH: usize = 2;          // 【更正】模块私有
pub const MIN_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 4;
pub const MAX_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 20;
pub const MIN_ANCHOR_CONTEXT_WIDTH: usize = 2;
pub const MAX_ANCHOR_CONTEXT_WIDTH: usize = 12;
```

### 5.3 宿主必须提供的回调（每个组件构造时）

见各节构造函数签名。**关键约定**：

- 所有 `on_select` / `on_cancel` / `on_change` 都是**同步**回调（除 `SessionsLoader`、
  `renameSession`、`onPersist` 等显式 async）。
- `request_render` 由宿主注入（`tui.request_render(false)`）；组件内部只调它。
- `on_complete`（`LoginDialogComponent` / `FirstTimeSetupComponent`）在结束前调用一次。
- `dispose()` 必须幂等（重复调用不 panic、不重复停定时器）。

### 5.4 事件顺序契约（测试会断言）

1. `ToolExecutionComponent::update_args` → **不**立即重建；下一次 `render` 时应用（#9265）。
2. `update_result` → **立即** `update_display()`（宿主随后 request_render）。
3. `BashExecutionComponent::append_output` → **会**调用 `updateDisplay()`（重建，见 §2.55.2）；
   但只在 `render` 时才按当前宽度生成行（预览行数/宽度依赖 render 的 width）。
4. `CustomEditor::handle_input`：`on_extension_shortcut` 优先于一切（返回 true 即消费）。
5. `LoginDialogComponent::handle_input`：`cancel` 优先；`app.message.copy` 次之（authUrl 存在时）；
   device code confirm 再次；最后才喂给 Input。
6. `SessionList::handle_input`：删除确认态拦截**所有**键（confirm 触发删除、cancel 取消，其余吞掉）。
7. `ExtensionSelectorComponent::handle_input`：`app.tools.expand` 优先且**不影响选中**；随后
   `up|k` → `down|j` → `confirm|"\n"` → `cancel`。
8. `CustomEditor::handle_input`：`app.exit` 在文本为空时**即使没有 handler 也 return**（不落到 super）。
9. **up/down 语义分三派（【更正】原第 9 条把 UserMessageSelector 归错了）**：
   - **环绕**：`UserMessageList`（`up` 在首项 → 末项；`down` 在末项 → 首项）、`TreeList`（同样环绕）、
     `SelectList`（tui 通用列表）、`ScopedModelsSelector`（`selectedIndex === 0 ? len-1 : -1` 风格）。
   - **clamp**：`OAuthSelector`（`Math.max(0, i-1)` / `Math.min(len-1, i+1)`）、`ExtensionSelector`（同）、
     `TrustSelector`、`SessionList`、`ConfigSelector` 的 `ResourceList`（`findNextItem` 到边界返回原索引）、
     `SettingsList`（tui）。
   - **其它**：`FirstTimeSetup` 用 `k`/`j` 别名 + clamp。
10. `LoginDialog` 的 `activateDeviceCode` 是**同步发起、异步更新**：状态文案先同步设置，`copyToClipboard`
    与 `openBrowser` 的结果稍后通过 `setDeviceCodeStatus` 更新（受 `focused && !aborted && hint 仍在树上` 守卫）。

---

## 6. npm 依赖与 Rust crate 映射

### 6.1 `packages/coding-agent/package.json` 的 runtime 依赖（逐字）

```json
"dependencies": {
  "@earendil-works/kiana-agent-core": "1.4.0",
  "@earendil-works/kiana-ai": "1.4.0",
  "@earendil-works/kiana-codemode": "1.4.0",
  "@earendil-works/kiana-mcp": "1.4.0",
  "@earendil-works/kiana-tui": "1.4.0",
  "@silvia-odwyer/photon-node": "0.3.4",
  "brace-expansion": "5.0.12",
  "chalk": "6.0.0",
  "cross-spawn": "7.0.6",
  "diff": "8.0.4",
  "grok-mermaid": "0.2.3",
  "highlight.js": "10.7.3",
  "hosted-git-info": "9.0.3",
  "ignore": "7.0.8",
  "jiti": "2.7.0",
  "minimatch": "10.2.6",
  "proper-lockfile": "4.1.2",
  "quickjs-wasi": "3.6.2",
  "semver": "7.8.5",
  "typebox": "1.3.27",
  "undici": "8.10.2",
  "yaml": "2.9.0"
}
"devDependencies": { "marked": "18.0.11", "vitest": "4.1.12", ... }
```

本单元实际 import 的：

| npm 包 | 用途（本单元） | Rust 替代 | 行为桥接要点 |
|---|---|---|---|
| `@earendil-works/kiana-tui` | 全部基础组件/工具 | 自研 crate `kiana-tui`（`crossterm` 后端） | 见 §6.2 |
| `chalk` | 间接（主题） | 自研 ANSI（`ansi_term`? 不用，自己写） | 必须自己产出与 chalk 相同的序列 |
| `diff`（`Diff.diffWords`） | `diff.ts` 行内 diff | `similar` crate 的 `TextDiff::from_words`（**算法不同**）或自研 jsdiff 移植 | **必须自研**：jsdiff 的 diffWords 先做「词级 LCS」，再把 `+`/`-` 归并，且空白处理不同。见 §6.3 |
| `grok-mermaid`（`renderMermaid`） | `mermaid.ts` | 自研（`mermaid-rs` 风格） | 库不在仓库内；只按契约与测试输出实现 |
| `marked`（`Marked` lexer） | `mermaid.ts` 需要 `lexer()` 与 `token.raw` | `pulldown-cmark` + 自研「raw slice」 | **必须保留原始文本切片**（`token.raw`）：pulldown-cmark 给 range，可用 `&text[range]` |
| `highlight.js` | 间接（Markdown 代码块高亮，由 kiana-tui 的 Markdown 用） | `syntect` | 语法高亮颜色映射须一致（见 ca-theme） |
| `@silvia-odwyer/photon-node` | `ensurePngTranscoder`（非 PNG → PNG） | `image` crate | 转码结果不必逐字节一致（只要视觉正确），但**只转一次**的缓存语义必须一致 |
| `node:fs` / `node:path` / `node:os` | 读图片、路径、`existsSync`/`unlink` | `std::fs` / `std::path` | `resolve`/`relative` 语义见 §6.3 |
| `node:child_process` | `spawnSync("trash")`、外部编辑器 | `std::process::Command` / `tokio::process` | `spawnSync` 是**同步阻塞**，Rust 用 `std::process::Command::output()` 保持阻塞 |
| `node:perf_hooks`（`performance.now`） | 动画 | `std::time::Instant` | 单调时钟；换算 f64 毫秒 |
| `setInterval`/`setTimeout` | 动画、倒计时 | `tokio::time::{interval, sleep}` + `JoinHandle::abort` | 见 §6.4 |
| `Intl.Segmenter` | `easter-egg-3d` 的 grapheme 切分 | `unicode-segmentation` | 按扩展 grapheme cluster 切分 |
| `WeakMap` | 会话搜索缓存 | `HashMap<SessionId, Cached>` + 显式失效 | `SessionInfo` 在 Rust 中按 id 索引，缓存随 session 对象生命周期 |
| `process.env` | `HOME`/`USERPROFILE`/`VISUAL`/`EDITOR`/`WSL_*` | `std::env::var` | 大小写与 Windows 兼容见 §6.3 |
| `AbortController`/`AbortSignal` | 会话加载、模型刷新、登录取消 | `tokio_util::sync::CancellationToken` | 需要 `race_with_abort_signal` 语义：信号触发即放弃结果 |
| `Promise`/`async` | 各选择器 | `async fn` + `tokio::select!` | 见 §6.4 |

### 6.2 自研 crate 结构建议

```
kiana/
  crates/
    kiana-tui/            # 对应 @earendil-works/kiana-tui（tui-core/tui-components/tui-text/tui-terminal 单元）
    kiana-ai/             # 对应 @earendil-works/kiana-ai（ai-core 单元）
    kiana-agent/          # 对应 @earendil-works/kiana-agent-core（agent-core 单元）
    kiana-coding-agent/   # 对应 coding-agent
      src/
        modes/interactive/
          components/  # ← 本单元（见 §8.1）
          theme/
          model_search.rs
          model_catalog_refresh.rs
          external_editor.rs
        core/
        utils/
```

本单元**不新增 crate**，只在 `kiana-coding-agent` crate 内新增模块树（§8.1）。

### 6.3 JS 行为桥接（必须逐项处理）

| JS 行为 | 影响位置 | Rust 方案 |
|---|---|---|
| **字符串是 UTF-16**：`s.length`、`s[i]`、`slice` 按 code unit | `tree-selector` 的 `keyData.length > 0`、`charCodeAt(0)`、`radiusShimmer`、`truncateToWidth` 内部 | 所有**用户可见文本宽度**用 `unicode-width`（按 char/列）而不是 `len()`；对 surrogate pair 只影响 emoji，需在 `visible_width` 层处理（tui-text 负责）。`radiusShimmer` 按 char 迭代（ASCII 输入等价） |
| **`Math.round`**：`.5` 向 **+∞** 取整（实测 Node 22：`2.5→3`、`1.5→2`、`-0.5→-0`） | `formatTokens`、`compaction` 的 `Math.round(tokens/1000)`、`durationMs` | `f64::round()` 是「远离零」，**不同**（`(-0.5).round() = -1`）。实现 `js_round(x) = (x + 0.5).floor()`（对负数同样成立：`(-0.5+0.5).floor() = 0`）。注意结果可能是 `-0.0`，格式化时要归一成 `"0"`。 |
| **`toFixed(n)`**：先取符号，再对**精确二进制值**做「恰好一半时进位（half-up on magnitude）」；`|x| >= 1e21` 时改用指数形式 | `formatTokens`、footer `percent.toFixed(1)`、`cost.toFixed(3)`、`CH{rate}` | Rust `format!("{:.1}", 1.25)` = `"1.2"`（half-to-even），**不一致**。实测 Node 22 锚点值（实现必须逐条通过）：`(1.25).toFixed(1)="1.3"`、`(1.15).toFixed(1)="1.1"`（1.15 二进制略小）、`(2.675).toFixed(2)="2.67"`、`(9.995).toFixed(2)="9.99"`、`(0.135).toFixed(2)="0.14"`、`(-1.25).toFixed(1)="-1.3"`、`(-0.5).toFixed(0)="-1"`、`(0.5).toFixed(0)="1"`、`(-0).toFixed(1)="0.0"`、`(1e-7).toFixed(3)="0.000"`、`(1e21).toFixed(1)="1e+21"`。→ 自研 `js_to_fixed(v: f64, n: u32) -> String`。**不能用最短往返串**（`1.15` 的最短串是 `"1.15"`，按 `>=5` 进位会得到 `"1.2"`，而 JS 给 `"1.1"`）。正确算法（对齐 ECMA-262 的 `Number.prototype.toFixed`）：取 `|v|` 的**精确十进制展开**（f64 是 `m * 2^e`，其十进制展开有限且精确；用 `num-bigint` 的 `BigUint` 精确缩放，或 Rust 的固定精度格式化 `format!("{:.*}", n+k, v)` 取足够多位），在**精确值**上做「恰好一半时取较大者」（即 half-up on magnitude，这与 ECMA 的 "pick the larger n" 一致），再补足 n 位小数。实操捷径：Rust `format!("{:.*}", n, v)` 已是**精确值 + half-to-even**，与 JS 只在「精确平局」时不同；因此只需检测平局（精确展开中第 n+1 位起恰好是 `5000...`）并把结果 +1 个 ulp 的最后一位。负号前置；`-0` → `"0.0"`（无负号）；`|v| >= 1e21` → 指数形式（`1e+21`）；`NaN` → `"NaN"`；`±Infinity` → `"Infinity"`。**必须用上面 11 个锚点写单测。** |
| **`Number.prototype.toLocaleString()`**：千分位逗号，**locale 随环境**（实测 `zh-CN` 与 `en-US` 都输出 `1,234`；`1234567 → 1,234,567`） | `compaction-summary-message` 的 `tokensBefore` | 手写 `format_thousands(i64)`（每 3 位插 `,`，负号在首）。zh-CN / en-US / de-DE 都在千位用 `,`；但 `de-DE` 会用 `.`——需产品确认固定为 `,`（见 §8.5 Q12）。**不要**用 `num-format` 的 locale 数据。 |
| **`localeCompare`**：locale-aware 排序（默认 locale；测试环境为 `en-US`，见 §8.3 第 9 项） | `footer` 的扩展状态 key、`provider.localeCompare`、`displayName.localeCompare`、`source.localeCompare` | 实测 Node 22：`["a","B"] → ["a","B"]`（不区分大小写）；`["A","a"] → ["a","A"]`（小写在前，非稳定保留）；`["a10","a2"] → ["a10","a2"]`（**不**按数值）。实现 `js_locale_compare(a,b)`：先比 `a.to_lowercase()` 与 `b.to_lowercase()` 的**码点序**，相等再比原串码点序。该近似对 ASCII 全部实测用例成立；非 ASCII 需加测（§8.3 第 9 项）。 |
| **`JSON.stringify`**：键序 = 插入序；`undefined` 值被省略；`JSON.stringify(undefined)` → `undefined` | `tool-execution.formatToolExecution`、`tree-selector.formatToolCall`、`diff` | `serde_json::Map` 默认 **BTreeMap** → 会重排！必须启用 `preserve_order` feature（`indexmap`）以保持插入序。`Value::Null` 对应 `null`；无值用 `Option`。 |
| **`Date` 格式化**：本地时区、`getMonth()` 从 0 开始、`getFullYear()` | `formatSessionDate`、`formatLabelTimestamp`、`tree` 版 | `chrono::Local` + `DateTime<Local>`；`%H:%M`、`%m/%d`（注意 `tree` 版 **不补零**）、`%y`。DST 边界需与 JS 相同（都用系统本地时区）。 |
| **`parseInt(s, 10)`**：前缀数字解析、忽略尾部 | `settings-selector` 各 `parseInt(newValue, 10)` | `s.trim_start().parse::<i64>()` 不行（遇到非数字即失败）。实现 `js_parse_int`：可选符号 + 连续 ASCII 数字，取前缀；无数字 → NaN（Rust 用 `Option<i64>`）。 |
| **`String.prototype.trim()`**：去 Unicode 空白（含 `﻿`） | 到处 | `str::trim()` 只去 `char::is_whitespace`；`﻿` 不是 `White_Space`。需额外 `trim_matches('\u{FEFF}')`。见 §8.3。 |
| **正则 `\s`**：JS 的 `\s` = `[ \t\n\v\f\r   -     　﻿]` | `session-selector-search` 的 `split(/\s+/)`、`tree` 的 token 化 | `regex` crate 的 `\s` 默认是 Unicode `White_Space`（**不含 `﻿`**）。用显式字符类。 |
| **正则 `[\x00-\x1f\x7f]`** | `session-selector` 的 `normalize` | `[\x00-\x1f\x7f]` 直接可用（`regex` crate 支持 `\x7f`） |
| **正则 `^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$`** | `oauth-selector` 环境变量识别 | 直接可用 |
| **正则 `/^([+-\s])(\s*\d*)\s(.*)$/`** | `diff.parseDiffLine` | `regex` crate 的 `.` 不匹配 `\n`（这里按行处理，无影响）；`\s` 需替换为显式类 |
| **`RegExp(pattern, "i")`** | `session-selector-search` 的 `re:` | `regex::RegexBuilder::new(p).case_insensitive(true)`；**JS 与 Rust 正则方言不同**：JS 支持 lookahead/lookbehind、`\p{...}`（较新）、命名组语法 `(?<n>)`；Rust `regex` 不支持 lookaround 与反向引用。遇到不支持的语法 → 视为「无匹配」（返回空列表），并在 spec 中记录差异。 |
| **`Array.prototype.sort`**：**稳定**（ES2019+） | 各处排序 | `sort_by` 是稳定排序（`slice::sort_by`），`sort_unstable_by` 不稳定。用 `sort_by`。 |
| **`Math.imul`**：32 位有符号乘法 | `easter-egg-3d.hash` | `(a as i32).wrapping_mul(b as i32)` 或 `u32::wrapping_mul` |
| **`>>>`**：无符号右移 | `hash` | `u32` 的 `>>` |
| **`Float32Array`** | `easter-egg-3d` 光栅 | `f32` 数组；所有中间运算**不得**提升为 f64 |
| **`Math.pow(x, 24)`** | 高光 | `x.powi(24)`（f32 或 f64？源码是 f64 再写回 Float32 缓冲 —— 见 §8.3） |
| **`Number.POSITIVE_INFINITY`** | `format_read_line_range` 的 `"Infinity"` 字符串 | 该函数在 render-utils（另一单元），本单元只消费 |
| **`String(session.modified)`** 无 | 无 | — |
| **`encodeURIComponent`** | 无（超链接不做编码） | 直接拼接 |
| **`process.platform`** | `darwin`/`win32` 分支 | `cfg!(target_os = ...)`；**必须保留** macOS/Windows 文案与键位差异 |
| **`new Date().toISOString()`** | `tree-selector.updateNodeLabel` 的 `labelTimestamp` | `chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)` → `2026-10-26T12:34:56.789Z` |
| **`String.prototype.padStart(2, "0")`** | 时间格式化 | `format!("{:02}", v)` |
| **`Array.from({length:256})`** | BRAILLE 表 | 常量数组或 `char::from_u32(0x2800 + i)` |
| **`Math.max(0, ...(arr))`**（空数组 → `-Infinity`） | `mermaid.codeSpan` 的 `Math.max(0, ...runs)` | 手写 `.max().unwrap_or(0)` |

### 6.4 异步/定时器映射

| TS | Rust |
|---|---|
| `setInterval(fn, ms)` + `clearInterval` | `tokio::spawn(async move { let mut iv = interval(Duration::from_millis(ms)); loop { iv.tick().await; ... } })` + `JoinHandle::abort()`；或 `tokio::time::sleep` 循环 |
| `setTimeout(fn, ms)`（一次性，如 100ms 自动取消、2000ms 状态清除） | `tokio::spawn(async { sleep(ms).await; ... })` + 保存 `JoinHandle` 以便取消 |
| `timer.unref()`（3D 彩蛋、radius） | Rust 无「不阻止退出」的等价物；用 `tokio::spawn` 且不 `await`，进程退出时自然终止。**语义差异可接受**（不影响渲染） |
| `AbortController.abort()` | `CancellationToken::cancel()`；await 点用 `tokio::select! { _ = token.cancelled() => return, r = fut => r }` |
| `Promise` rejection（`Login cancelled`） | `Result<T, LoginError::Cancelled>` |
| `await import("./easter-egg-3d.ts")` 失败静默 | 直接调用同 crate 函数（无动态导入）→ 天然不失败；但保留 `Result` 接口 |
| `spawnSync("trash", ...)` | `std::process::Command::new("trash").args(...).output()`（同步，在 async 上下文中用 `tokio::task::spawn_blocking`） |

### 6.5 建议的 `Cargo.toml` 依赖（本单元相关）

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync", "process"] }
tokio-util = "0.7"            # CancellationToken
crossterm = "0.28"            # 终端后端（kiana-tui）
unicode-width = "0.2"
unicode-segmentation = "1"
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }   # 关键：保持键序
regex = "1"
chrono = { version = "0.4", features = ["clock"] }
similar = "2"                 # 参考；diffWords 需自研
pulldown-cmark = "0.12"       # markdown lexer（mermaid raw 切片）
image = "0.25"                # PNG 转码
syntect = "5"                 # 代码高亮（Markdown）
unicode-normalization = "0.1"
once_cell = "1"
thiserror = "2"
```

**不使用**：`clap`（CLI 在 ca-cli）、`reqwest`（无网络）、`rusqlite`（无 DB）、`rquickjs`/`oxc`（本单元无 JS 执行）。

---

## 7. 测试清单

> 全部位于 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/test/`（`test/suite/regressions/` 另行标注）。
> 每项都必须在 Rust 侧有对应测试（可复用同一组断言）。标 ★ 的是**本单元独有**且最关键的行为。

### 7.1 直接覆盖本单元的测试文件

| 文件 | 用例数 | 覆盖组件 | 关键断言 |
|---|---|---|---|
| `assistant-message.test.ts` | 12 | AssistantMessageComponent | ★ 有/无 OSC 133；`length` 文案；thinking 合并；逐 run 点击；outputPad；transformer 顺序/上下文/异常/不修改原消息；宽度重放 |
| `user-message.test.ts` | 4 | UserMessageComponent | ★ 移动 OSC 标记后高度不变；transformer 链；invalidate 重放；历史有 messageId / live 无 |
| `status-indicator.test.ts` | 5 | StatusIndicator + CustomEditor | ★ IdleStatus 2 行空格；未 opt-in 上边框纯净；嵌入 `── ⠋ Working ───────` 且 thinkingHigh 出现 4 次；5 种 indicator × 6 宽度；retry 倒计时；dispose 停更新 |
| `collapsible-message-components.test.ts` | 3 | Compaction/Branch/Skill | ★ 点击展开/收起 |
| `themed-text.test.ts` | 1 | ThemedText | 延迟构建 + invalidate 重建 |
| `custom-message.test.ts` | 1 | CustomMessageComponent | renderer 收到 outputPad 并在更新后变化 |
| `bash-execution-width.test.ts` | 2 | BashExecutionComponent | ★ 折叠预览按 render 时宽度 |
| `output-pad.test.ts` | 1 个 `test.each(components)`，components 数组 **5 项**（`bash execution` / `tool execution` / `tool execution without a definition` / `self-rendered edit result` / `compaction summary`）× 2 个 pad 值 | 同上 | ★ pad 0 无前导空格；pad 1 每行恰好 +1；`renderLines` 用 `render(60)` + `stripAnsi` + `trimEnd`，过滤条件 `/[\w$(]/`（跳过空行与整行边框） |
| `tool-execution-component.test.ts` | **26 个 `test(...)` 声明**，展开成 **43** 个运行时用例（`test.each` 10 项时长表 + `for` 循环 5 + 2 项 read range 表） | ToolExecutionComponent | ★ 全部 renderer 槽位/继承/复用/合帧/点击/图片转码一次 |
| `tree-selector.test.ts` | 18 | TreeSelector / TreeList | ★ 最近可见祖先；过滤模式；折叠；多根；帮助行；复制；label 时间戳；空过滤保留 |
| `session-selector-search.test.ts` | 14 | 搜索纯函数 | ★ tier 排序；短语/正则；nameFilter |
| `session-selector-path-delete.test.ts` | 8 | SessionList / 删除 | ★ Ctrl+Backspace 有查询时不删；Ctrl+D 确认；作用域竞态；符号链接；threaded 子树活动；当前会话别名 |
| `session-selector-rename.test.ts` | 3 | SessionSelectorComponent | rename hint 显示条件；Ctrl+R 进入；Enter 提交 |
| `footer-width.test.ts` | 11 | FooterComponent / format* | ★ 兄弟路径不缩写；宽度不变；routed 模型；cost 汇总；缓存命中率；`(sub)` 判定 |
| `model-selector.test.ts` | 4 | ModelSelectorComponent | 当前模型标记；distinct name；save 绑定；列出所有失败 catalog |
| `scoped-models-selector.test.ts` | 5 | ScopedModelsSelector | distinct name；enableAll；单项禁用；清空后启用；恢复全启用 |
| `oauth-selector.test.ts` | 6 | OAuthSelector + format* | 投影；not configured；OAuth in API-key；env；models.json |
| `settings-selector.test.ts` | 5 | SettingsSelectorComponent | fullscreen 循环；share destination；固定/自动主题标记；per-model thinking |
| `thinking-selector.test.ts` | 2 | ThinkingSelectorComponent | 当前等级 `✓`；save 绑定 |
| `trust-selector.test.ts` | 4 | TrustSelectorComponent | 保存决策标记；选择结果；inherited 文案；父目录选项 |
| `mermaid.test.ts` | 8 | createMermaidMarkdownTransformer | ★ 替换/保持/主题/流式/class/警告/汇总/模式 |
| `login-dialog-device-code.test.ts` | **7 个声明**（其中一个 `it.each` 3 项 → 9 个运行时用例） | LoginDialogComponent | ★ 各动作清理与一次性消费；`browser=%s / clipboard=%s` 覆盖 |
| `auth-url-copy.test.ts` | 3 | AuthUrlComponent / LoginDialog | ★ 复制 URL 不进输入框 |
| `custom-editor-history-keybindings.test.ts` | 1 | CustomEditor | ★ 历史键优先于模型循环 |
| `read-line-range-display.test.ts` | 2 | formatReadLineRange（+ TUI 展示） | 参数表；TUI/树/HTML 均显示 `:25-37` |
| `first-time-setup.test.ts` | 8 | 首次启动判定（宿主逻辑） | experimental + 默认 agent dir + 无 settings.json |
| `first-time-setup-fork.test.ts` | 1 | 同上 | fork 包不提示 |
| `radius.test.ts` | 6 | Radius 模型目录（provider 层） | 与 UI 组件无直接断言（本单元无测试） |
| `interactive-mode-tree-navigation.test.ts` | **4**（【更正】原写 3） | TreeSelector（经宿主） | compaction 完成时仍可导航；中止响应后重查 |
| `interactive-mode-status.test.ts` | **36 个声明**（含 1 个 `test.each` 2 项 → 37 个运行时用例） | 状态行/启动列表（多数属宿主） | 状态合并；`custom editor 未 opt-in 用独立行` 等。**【更正】** 旧稿写 35 |
| `interactive-tui.test.ts` | **11 个声明** → **16 个运行时用例**（2 个 `it.each`：`[true,false]` 2 项 + 4 种 indicator = 6 个额外展开） | 宿主 + CustomEditor + 全屏 | ★ `uses the standalone row for a custom editor that has not opted in`；alt-screen 选择；剪贴板括号粘贴；copy-on-select；`routes every status through the editor opt-in (%s)`；`does not reserve separate status height for an embedded %s indicator` |
| `interactive-mode-extension-dialogs.test.ts` | 4 | ExtensionSelector/Input 栈 | 对话框层叠、信号中止移除 |
| `footer-data-provider.test.ts` | **8**（【更正】原写 ~10） | FooterDataProvider（非本单元） | git branch 解析（footer 依赖） |
| `model-search.test.ts` | 5 | model-search（本单元依赖） | distinct name 规则 |
| `edit-tool-no-full-redraw.test.ts` | 3 | ToolExecutionComponent | ★ edit 渲染器**不**触发全量重绘（#9265 的回归） |
| `codemode-renderer.test.ts` | 5 | ToolExecutionComponent | codemode 工具渲染器槽位与输出 |
| `tool-renderer-examples.test.ts` | 2 | ToolExecutionComponent | 渲染器示例快照 |
| `bug-report.test.ts` | 3 | ExtensionEditorComponent / ExtensionSelectorComponent | `/bug` 对话框（编辑器预填、选择器选项） |
| `package-command-paths.test.ts` | **35** | ConfigSelectorComponent | ★ 包资源路径 → `/config` 分组与显示名（§2.41 的主要覆盖） |
| `suite/regressions/9068-user-bash-fail-closed.test.ts` | 2 | InteractiveMode + RPC（mock `bash-execution`） | 用户 bash 失败时的 fail-closed 行为 |

**【更正】** 原表把 `output-pad.test.ts` 记成 `5×2`：实际是 **1 个 `test.each(components)`**，`components`
数组里有 **5 个**组件（Bash / Tool 两种 / Edit / Compaction）× 2 个 pad 值 = 10 个运行时用例。
【更正】`first-time-setup.test.ts` 是 **8** 项（附录 A 写「宿主 9 项」有误）。

### 7.2 suite 回归（与本单元相关）

| 文件 | 用例数 | 关注点 |
|---|---|---|
| `3217-scoped-model-order.test.ts` | 2 | scoped 模型顺序（`getSortedIds`） |
| `4167-thinking-toggle-pending-tool-render.test.ts` | 2 | 思考开关与待渲染工具（AssistantMessage thinking overrides） |
| `5433-extension-oauth-prompt-input.test.ts` | 5 | 扩展 OAuth 提示输入（ExtensionInput / LoginDialog） |
| `6949-unavailable-scoped-model.test.ts` | 4 | 不可用模型显示（strikethrough/`[unavailable]`） |
| `6999-models-json-hot-reload.test.ts` | 1 | models.json 热重载触发选择器刷新 |
| `7153-scoped-models-refresh.test.ts` | 2 | scoped 刷新状态文案 |
| `7209-model-selector-filter-resets-selection.test.ts` | 2 | 过滤后选中归零 |
| `8611-thinking-toggle-pending-bash-output.test.ts` | 1 | 思考开关与 bash 输出（BashExecution） |
| `10285-mcp-tool-renderers.test.ts` | 2 | MCP 工具 renderer 复用（ToolExecution） |
| `5303-bash-output-truncation.test.ts` | 2 | bash 输出截断文案 |
| `5208-late-bash-output.test.ts` | 1 | 迟到输出（appendOutput 语义） |
| `3688-tree-cancel-compacting.test.ts` | 1 | 树在 compaction 中取消 |
| `9178-tree-during-compaction.test.ts` | 2 | compaction 期间打开树 |
| `8537-custom-message-tool-result-ordering.test.ts` | 3 | 自定义消息与工具结果顺序（CustomMessage/CustomEntry） |
| `6324-branch-summary-ambient-auth.test.ts` | 1 | branch summary 消息 |
| `8989-fork-compaction-label-boundary.test.ts` | 1 | fork + compaction label（UserMessageSelector） |
| `8261-subagent-project-trust.test.ts` | 2 | 信任选择（TrustSelector） |
| `5996-session-name-newlines.test.ts` | 2 | 会话名换行（SessionList 归一化） |
| `7497-session-discovery-symlink.test.ts` | **3**（2 个 `it` + 1 个 `it.skipIf(process.platform === "win32")`，非 Windows 才跑） | 符号链接会话（树/列表）：目录链接保留别名路径；坏链接不隐藏有效会话；指向文件的链接被忽略。**【更正】** 文件名以 `7497-` 开头；`5497-` 不存在 |
| `tree-during-streaming.test.ts` | 1 | 流式中打开树（**无数字前缀**，也在 `suite/regressions/`） |
| `8724-in-memory-fork-active-tool.test.ts` | 1 | 内存 fork（UserMessageSelector） |
| `9068-user-bash-fail-closed.test.ts` | 2 | 用户 bash 失败 fail-closed（mock `bash-execution`） |

### 7.3 无直接测试、必须靠**人工/黄金快照**验证的组件

| 组件 | 原因 | 建议 |
|---|---|---|
| `ConfigSelectorHeader` / `ResourceList` / `ConfigSelectorComponent` | 仅经 `package-command-paths.test.ts`（35 项，资源标签）间接覆盖 | 新增快照测试：构造固定 `ResolvedPaths`，渲染 80 列，比对整屏 |
| `SelectSubmenu` / `SteppedSubmenu` | 无独立测试 | 通过 settings-selector 的 submenu 路径间接覆盖；建议补「Step 1/N」断言 |
| `RadiusLoginMenuComponent` / `radiusShimmer` | 无测试（`radius.test.ts` 测 provider） | 固定 `elapsedMs` 的黄金字符串 |
| `BorderedLoader` | 无测试 | 快照 |
| `ExtensionInputComponent` / `ExtensionEditorComponent` | 仅 `5433` 间接 | 快照 + 键位测试 |
| `FirstTimeSetupComponent` | `first-time-setup.test.ts` 测宿主判定，不测 UI | 快照 |
| `ThemeSelectorComponent` / `ShowImagesSelectorComponent` | 无测试 | 快照 |
| `ArminComponent` | 无测试（`Math.random` 不可控） | 注入 RNG（trait `Rng`），固定种子后逐帧快照 |
| `EasterEgg3dAnimation` / `playEasterEgg3d` | 无测试 | 注入 RNG + 时钟，逐帧 f32 黄金快照（§8.3） |
| `earendil-announcement` | 无测试 | 快照（无图片 / 有图片两态） |
| `diff.renderDiff` | 无直接测试（经 edit renderer 间接） | 逐行黄金表（含 intra-line） |
| `session-selector` 的 header 状态行 | 无独立测试 | 快照（loading/confirm/status 三态） |
| `visual-truncate` | 经 bash/code 间接 | 单元测试 `truncateToVisualLines` 的 keep=start/end（§2.58） |
| `footer` 第 3 行（扩展状态） | 无直接测试 | 断言**整行不套 dim**、按 `localeCompare` 排序、`sanitizeStatusText` 折叠空白 |
| `login-dialog` 的 `showInfo`/`showDetails`/`showWaiting` | 无直接测试 | 快照（`showDetails` 的 paddingX=1、`showInfo` 纯追加） |
| `session-selector-search` 的 `WeakMap` 缓存 | 无直接测试 | 断言 `allMessagesText` 变化后重新小写化（可观测：匹配结果改变） |
| `armin` 的 `render` 边界 | 无测试 | width=0/1/2/31/32 的黄金行（`slice(0,-1)` 语义） |

### 7.4 测试基建要求（Rust 侧）

- 需要一个 `VirtualTerminal`（记录 ANSI 的假 TUI）：`request_render` 计数、`terminal_rows` 可设。
- `init_theme("dark")` / `init_theme("light")` 可在测试中切换；`theme.get_fg_ansi(key)` 可取颜色串做断言。
- `strip_ansi(s)` 与 `visible_width(s)` 必须与 TS 完全一致（用于断言）。
- 键盘输入用字符串常量（`"\x1b"`、`"\r"`、`"\t"`、`" "`、`"j"`、`"k"` 等）直接调用 `handle_input`。
- 时间：`tokio::time::pause()` + `advance()` 复刻 vitest 的 `vi.useFakeTimers()`。
- 随机：3D/armin 用可注入 `Rng`（`rand::rngs::StdRng` 固定种子），TS 侧需同步改成可注入以做对拍
  （或仅接受「视觉一致」而非逐帧一致，见 §8.5 Q5）。

---

## 8. Rust 模块布局与最难复现的部分

### 8.1 建议模块树（`kiana-coding-agent/src/modes/interactive/components/`）

```
components/
  mod.rs                      # 对应 index.ts：pub use 汇总
  border.rs                   # dynamic_border
  themed_text.rs
  markdown_transform.rs
  keybinding_hints.rs
  kiana_logo.rs
  countdown.rs                # CountdownTimer
  visual_truncate.rs
  diff.rs                     # render_diff + jsdiff 词级 diff
  mermaid.rs                  # create_mermaid_markdown_transformer + code_span/style_span

  messages/
    mod.rs
    user.rs                   # UserMessageComponent
    assistant.rs              # AssistantMessageComponent
    branch_summary.rs
    compaction_summary.rs
    skill_invocation.rs
    custom.rs                 # CustomMessageComponent
    custom_entry.rs
    bash_execution.rs
    tool_execution.rs

  editor/
    mod.rs
    custom_editor.rs          # CustomEditor + render_top_border
    extension_editor.rs       # ExtensionEditorComponent
    auth_url.rs
    status_indicator.rs       # StatusIndicator + 5 子类

  selectors/
    mod.rs
    extension_selector.rs
    extension_input.rs
    thinking_selector.rs
    show_images_selector.rs
    theme_selector.rs
    trust_selector.rs
    first_time_setup.rs
    oauth_selector.rs
    login_dialog.rs           # LoginDialogComponent + device code 状态机
    radius_login.rs           # radiusShimmer + RadiusLoginMenuComponent
    user_message_selector.rs
    model_selector.rs
    scoped_models_selector.rs
    settings_submenu.rs       # SelectSubmenu + SteppedSubmenu
    settings_selector.rs
    config_selector.rs
    session_selector/
      mod.rs                  # SessionSelectorComponent
      header.rs               # SessionSelectorHeader
      list.rs                 # SessionList + render
      tree.rs                 # buildSessionTree / flattenSessionTree
      search.rs               # session-selector-search.rs
      delete.rs               # deleteSessionFile (trash → unlink)

  tree_selector/
    mod.rs                    # TreeSelectorComponent
    list.rs                   # TreeList
    flatten.rs                # flattenTree / recalculateVisualStructure / active path
    filter.rs                 # applyFilter / getSearchableText / modes
    render.rs                 # render + renderHorizontalViewport
    text.rs                   # getEntryDisplayText / formatToolCall / formatLabelTimestamp / extract*
    help.rs                   # TreeHelp + TREE_HELP_ITEMS + formatHelpKeys + compactRawKeys
    label_input.rs
    search_line.rs

  footer.rs
  loader.rs                   # BorderedLoader
  armin.rs
  easter_egg/
    mod.rs                    # eager 入口（playEasterEgg3d）
    lazy.rs                   # alt-screen 守卫 + 延迟入口
    parse_screen.rs           # SGR/OSC 解析 + grapheme 网格
    raster.rs                 # BlockRaster + 光照 + braille
    animation.rs              # EasterEgg3dAnimation
    hash.rs                   # Math.imul 复刻
  earendil_announcement.rs
```

**关键 trait / 类型**：

```rust
/// 组件基接口（由 kiana-tui 提供；此处重复以标明本单元的用法）
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn invalidate(&mut self);
    fn handle_input(&mut self, _data: &str) {}
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> { None }
    fn dispose(&mut self) {}
}

/// 主题在 render 时解析：不在构造期缓存颜色。
pub type StyleFn = Box<dyn Fn(&str) -> String>;

/// 可折叠消息的公共状态（compaction/branch/skill/custom 复用）
pub struct ExpandableState {
    pub expanded: bool,
    pub output_pad: usize,
}

/// 列表滚动窗口（session/tree/config/model 复用）
pub struct Window {
    pub selected: usize,
    pub max_visible: usize,
    pub len: usize,
}
impl Window {
    /// start = max(0, min(sel - max/2, len - max))
    pub fn start(&self) -> usize;
    /// 具体组件的上下移动语义不同，**不要**在 Window 里统一成环绕：
    /// - `SelectList`（tui）：up/down **环绕**（`(i + n - 1) % n`、`(i + 1) % n`）；
    /// - `ExtensionSelector` / `OAuthSelector` / `TrustSelector`：**clamp**（`max(0,..)`/`min(len-1,..)`）；
    /// - `SessionList`：clamp；
    /// - `TreeList`：**环绕**（`i === 0 ? len-1 : i-1` / `i === len-1 ? 0 : i+1`）；
    /// - `UserMessageList`：**环绕**（同上两端跳转，不是 clamp）；
    /// - `ModelSelector`：列表 **环绕**（空列表直接 return）；过滤后 `selectedIndex = query ? 0 : clamp(len-1)`；
    /// - `ScopedModelsSelector`：列表**环绕**，且排序移动不环绕（`move` 到边界返回原列表）。
    /// 因此 `Window` 只提供 `clamp_up()` / `clamp_down()` / `wrap_up()` / `wrap_down()` 四个显式方法，
    /// 由各组件按上面清单选择。    ///
    /// `SelectList` 的环绕实现是「在两端跳转」而非取模：
    /// `up: i === 0 ? len-1 : i-1`；`down: i === len-1 ? 0 : i+1`（空列表时 `len-1 = -1`，需特判为 0）。
    /// 注意 `SelectList` **没有** pageUp/pageDown 处理（翻页键被忽略）。

    pub fn clamp_up(&mut self);
    pub fn clamp_down(&mut self);
    pub fn wrap_up(&mut self);
    pub fn wrap_down(&mut self);
    pub fn page_up(&mut self, step: usize);
    pub fn page_down(&mut self, step: usize);
    pub fn clamp_after_filter(&mut self);
}

/// 三级过滤器（tree / session / settings 共用一部分）
pub trait Filter {
    fn passes(&self, entry: &SessionEntry) -> bool;
}
```

**并发模型**：

- 所有组件都在 **UI 线程**（单线程渲染循环）上运行；`render` 不做 I/O。
- 异步任务（会话加载、模型刷新、删除文件）通过 `tokio::spawn` + `mpsc` 把结果送回 UI 线程，
  **不**在任务里直接改组件（对应 TS 的 `await` 后改状态，但 Rust 需要显式消息化）。
  或使用 `Rc<RefCell<...>>` 单线程运行时（`LocalSet`），更贴近 TS 语义。**建议后者**：
  整个 interactive 模式跑在 `tokio::task::LocalSet` 上，组件用 `Rc<RefCell>` 共享，
  这样 `async` 回调可以直接改状态，与 TS 一致。
- 定时器（`Loader` 帧、`CountdownTimer`、动画）统一由 `kiana-tui` 的 frame scheduler 驱动，
  或每个组件持 `JoinHandle` 并在 `dispose` 中 `abort()`。

### 8.2 全局状态

| TS | Rust |
|---|---|
| `theme` 全局代理 | `static THEME: RwLock<Theme>`；或线程局部 `RefCell`（单线程 UI） |
| `getKeybindings()` | `static KEYBINDINGS: RwLock<KeybindingsManager>` |
| `getCapabilities()` | `OnceLock<Capabilities>` |
| `playing`（3D） | `static PLAYING: AtomicBool` |
| `cachedImageBase64`/`attemptedImageLoad` | `OnceLock<Option<String>>`（`attempted` 用 `Once`） |
| `messageTextCache`（WeakMap<SessionInfo, {source, lower, normalized?}>） | `thread_local! { HashMap<SessionId, LowerCache> }`，**按 `source`（`allMessagesText` 全文）判断是否失效**，不是按 id 永远缓存 |

### 8.3 最难复现的部分（按难度排序）

#### ★★★★★ 1. `easter-egg-3d.ts`（1292 行）的逐位一致

难点：

- **`Float32Array` 逐点写入**：TS 里 `depth` 缓冲是 `Float32Array`，每次写入都会**舍入到 f32**；
  若 Rust 用 `f64` 累加再比较，深度测试的胜负可能在边界翻转，导致盲文点位不同。
  → 必须用 `Vec<f32>`，且**每次写入**都 `as f32`。
- **`Math.pow(x, 24)`**：JS 用 f64 计算后存回 f32；Rust 用 `(x as f64).powi(24) as f32` 才能一致。
- **`Math.imul` 链**：`hash()` 的 3 次 `imul` + `>>>` 必须用 `u32::wrapping_mul` 精确复刻。
- **`Intl.Segmenter`** 与**宽字符**：屏幕解析时 grapheme 切分与东亚宽字符占位；Rust
  `unicode-segmentation` 的 UAX #29 版本需与 Node 22 的 ICU 一致（差异极小，但 emoji ZWJ 序列可能不同）。
- **射线投射的浮点顺序**：`dot`、`length`、`normalize` 的运算顺序必须一致；`LIGHT`/`HALF_VECTOR`
  的归一化在 JS 里是模块加载时算一次（f64），存为 f64 常量；Rust 同样用 f64 常量。
- **`Math.random()`**：动画的模型选择、`puzzleMoves`、星点分布、`glitch` 偏移都依赖随机。
  **不可能逐帧对拍**，除非：
  1. 把 TS 改成可注入 RNG（需改 TS，但 TS 是只读参考 → 不可行）；或
  2. 仅对**确定性部分**对拍（`hash`、`shuffleSteps`、光栅、`parseScreen`），随机部分做「不变量测试」
     （每帧宽度正确、字符集属于盲文/空格、颜色在允许集合内）。
- **退出动画**：`targetYaw = Math.ceil(yaw / (2π)) * 2π`，`yaw` 是累积浮点 → 需要同样的 f64 累加。

**验收策略**：
1. `hash`、`shuffleSteps(model, cycle)` 用 TS 输出做黄金表（可在 Node 里跑一段脚本生成，不修改 TS 源码）。
2. `parseScreen` 用固定 ANSI 输入 + 黄金网格。
3. `BlockRaster` 用固定模型 + 固定 yaw/pitch + 固定尺寸，比对每点颜色（f32 逐位）。
4. 整帧：固定 `now` 序列（注入时钟）比对（随机部分固定种子——若无法从 TS 侧注入，则接受人工目视）。

#### ★★★★★ 2. Markdown 渲染逐字符一致

本单元所有消息组件的可见文本都经 `kiana-tui` 的 `Markdown`。难点：

- `marked` lexer 的**分词边界**（尤其嵌套列表、表格、HTML、链接引用、代码围栏的闭合规则）。
  `pulldown-cmark` 的 CommonMark 一致度很高，但与 `marked` 在边缘语法上有差异。
- **有序列表标记保留**（`preserveOrderedListMarkers`）：`marked` 的 `start` 属性；
  必须输出原编号（如 `1.`、`2.` 而非重新编号）。
- **反斜杠转义保留**（`preserveBackslashEscapes`）：不把 `\*` 渲染成 `*`。
- **`transform` 钩子**：在渲染前对整段 Markdown 字符串做变换，且**availableWidth 依赖内容宽度**
  （宽度变化要重放）。
- 行内代码/代码块高亮（`cli-highlight` → `syntect`）的颜色类名映射。
- **软换行**：`marked` 默认 `breaks: false`（软换行不产生 `<br>`），`pulldown-cmark` 默认同样；
  但 `Markdown` 组件的折行策略（按 `visible_width` 折）必须一致。

**验收策略**：为每个测试用到的 markdown 片段（约 60 个）生成「TS 渲染的 ANSI 行」黄金文件，
Rust 侧逐行比对。这是最稳的方式，也是唯一能保证「TUI 文本完全一致」的方式。

#### ★★★★☆ 3. `diff.ts` 的 `diffWords` 与行内高亮

- jsdiff 的 `diffWords` 实现：先用 `diffWordsWithSpace` 的简化版——按 `/(\s+|\b)/` 切词，
  再对 token 序列做 **Myers diff**，然后**归并**相邻同类型块，最后**忽略空白差异**（把纯空白变化
  合并进未变更部分）。
- `similar` crate 的 `TextDiff::from_words` 用的是「按空白分词 + Myers」，**算法接近但不保证相同**。
  必须用黄金表逐例验证；差异出现时用手写实现（Myers 的 `O(ND)` 变体 + jsdiff 的归并规则）。
- 另外：`renderIntraLineDiff` 只对「单删 + 单增」行做词级 diff，且**只有第一个 removed/added 片段**
  的前导空白不着色（后续片段整段 inverse）；非匹配行**不做** tab 替换。详见 §2.57。
- 已知的 jsdiff `diffWords` 关键细节（须照抄）：
  1. 先用 `/(\s+|[^\s]+)/` 之类的规则切词，但 **`diffWords` 会调用 `diffWordsWithSpace` 后合并空白差异**：
     实际上 jsdiff 的实现是 `diffWordsWithSpace(old, new, {ignoreWhitespace: true})` 的等价物 ——
     **纯空白差异被吸收进「公共」片段**，因此 `"a b"` vs `"a  b"` 不产生 added/removed。
  2. token 级 Myers（`diff` 的 `diff` 核心），再 `buildValues` 归并相邻同类块。
  3. 返回值顺序：removed 在 added **之前**（同一处变更先 `-` 后 `+`）。
  → 对拍用例至少要覆盖：纯空白差异、单词替换、单词插入、多词替换、中文（无空格）整串替换、
  行首缩进差异（验证「首段去空白」逻辑）。

#### ★★★★☆ 4. ANSI/宽度的精确一致

- `visible_width`：东亚宽字符、emoji、变体选择符、零宽连接符、组合字符、`\t`（按 0？）、
  ANSI 序列（zero width）。Rust `unicode-width` 的 Unicode 版本需与 Node 的 `string-width` 一致。
- `truncateToWidth`：省略号占 3 列；宽字符在边界处如何处理（整字符丢弃 + 补空格？）；
  `pad` 参数。
- `sliceByColumn`（严格模式）。
- `wrapTextWithAnsi`：ANSI 状态跨行保持（每行结尾重置、下一行重放）。
- **所有行必须 ≤ width**，这是 `status-indicator.test.ts` 与 `tree-selector.test.ts` 的显式断言。

#### ★★★☆☆ 5. Mermaid 图的 Unicode 输出

- `grok-mermaid` 不在仓库内 → 只能按契约实现（`MermaidArt { width, styled, plain, warnings }`）。
- 测试断言了具体图形：`│ Start ├───▶│ Done │`、`───▶`、`│ Foo ├───▶│ Bar │`。这些是**布局算法**
  （节点框、箭头、间距）的结果，必须与库一致。建议：先实现「只支持测试覆盖的图类型」（flowchart LR/TD、
  简单 classDef），逐步扩展；不支持的图返回 `undefined`（保持原 code block）。
- 警告文本 `'dropped, expected a link: "invalid"'` 必须逐字一致。

#### ★★★☆☆ 6. 会话/模型选择器的异步竞态

- `SessionSelectorComponent`：快速切换 scope 时旧请求必须 abort 且结果不得落地；`loadProgress`
  回调期间 `header` 的重绘。
- `ModelSelectorComponent`：15 s 超时 + 共享协调器（多组件同时刷新只发一次请求）+ abort 后
  `result` 可能为 `undefined` 的判断。
- `LoginDialogComponent`：`inputResolver` 的 Promise 生命周期（cancel 时 reject，后续 `showPrompt` 可重入）。
- Rust 里用 `CancellationToken` + `JoinHandle` 模拟，但**必须显式检查「结果落地前 is_active」**，
  否则会出现「切回后旧列表覆盖」。

#### ★★★☆☆ 7. JS 数字/字符串格式化

- `Math.round`（半数向 +∞）、`toFixed(1)`（二进制精确 + 半数向远零）、`toLocaleString`（千分位）、
  `parseInt`（前缀）、`trim`（`﻿`）。
- 实现一组 `js_num`/`js_str` 辅助函数并**逐值单测**（边界：`0.05`、`1.25`、`1.15`、`2.675`、`2.5`、
  `-0.5`、`999.95`、`1e21`、`Infinity`、`NaN`、`-0`）。
- `formatTokens` 的分档边界（**实测 Node 22 已验证**）：
  `999→"999"`、`1000→"1.0k"`、`1500→"1.5k"`、`9999→"10.0k"`（`9999/1000=9.999`，`toFixed(1)` 进位到 `10.0`）、
  `10000→"10k"`、`999999→"1000k"`（注意**没有**升到 M，因为分支是 `< 1e6`）、`1000000→"1.0M"`、
  `9999999→"10.0M"`、`10000000→"10M"`、`12345678→"12M"`。
- `footer` 的 `cost.toFixed(3)` 与 `percent.toFixed(1)` 用同一个 `js_to_fixed`；`CH{rate}` 也是 `.toFixed(1)`。

#### ★★★☆☆ 8. 主题的两种色彩模式

- `truecolor` / `256` / `16` 三档下 `foreground_ansi`/`background_ansi` 的降级算法（由 tui-terminal 决定，
  但本单元的黄金输出依赖它）。
- `theme.bold(s)` 的「可嵌套」实现：在已着色的字符串外面加 `\x1b[1m`/`\x1b[22m`，不能破坏内部颜色。
- 亮/暗主题下所有 token 的具体 ANSI 值（`ca-theme` 提供，本单元只消费）。

#### ★★★☆☆ 9. `localeCompare` 排序

- 影响：`model-selector` 的 provider 排序、`config-selector` 的组/条目排序、`footer` 的扩展状态排序。
- Node 默认 locale 依赖 `LANG`/`LC_ALL`（**本仓库测试环境是 `en-US`**；spec 早期稿写 `zh-CN` 是另一台机器
  的实测环境，两者在 ASCII 用例上结果一致）。Rust 用：
  ```rust
  fn locale_compare(a: &str, b: &str) -> Ordering {
      let al = a.to_lowercase(); let bl = b.to_lowercase();
      al.cmp(&bl).then_with(|| a.cmp(b))
  }
  ```
  这只是近似。**建议**：在 §8.4 验收时用实际数据（主题名、provider 名、模型 id）跑一遍 TS 排序，
  生成黄金顺序表，Rust 用表驱动测试。

#### ★★☆☆☆ 10. `spawnSync("trash")` 与外部编辑器

- `spawnSync` 的阻塞语义与 `encoding: "utf-8"`（无效 UTF-8 如何处理）。
- `trash` 不存在时 `status === null`、`error` 有值、`stderr` 为 undefined。
- 外部编辑器：`mkdtemp("kiana-editor-")` 的目录名格式（`kiana-editor-XXXXXX`）、文件 `prompt.md`、
  Windows `shell: true`、退出码 0 才读取、`stripBom` 与去尾换行。
- 这些是**跨平台**行为，Linux 测试为主，Windows/macOS 分支需 `cfg` 编译。

### 8.4 验收清单（Definition of Done）

> 每条都必须可勾选。勾选标准：Rust 实现通过对应测试/快照/手工核对。

**A. 基础组件**
- [ ] `DynamicBorder`：默认 theme border 色、自定义色、宽度 `max(1,w)`、`invalidate` 空实现
- [ ] `ThemedText`：首次 render 才 build、invalidate 后重建、padding 正确
- [ ] `keyText`/`keyDisplayText`/`keyHint`/`rawKeyHint`：darwin `alt`→`option`、capitalize、
      `" "` 分隔、dim/muted 着色
- [ ] `createMarkdownTransform`：链式、异常吞掉、context 字段、availableWidth 透传
- [ ] `kianaLogoLines`/`supportsKianaLogo`/`kianaWordmark`：三色、`▀`/`█`、Apple Terminal 判定
- [ ] `CountdownTimer`：首帧立即 tick、1 s 递减、0 秒后过期、dispose 幂等
- [ ] `truncateToVisualLines`：keep=start/end、skippedCount、paddingX、空串早退；`VisualLinePreview` 按 width 缓存、
      keep=start 时 hint 在后（见 §2.58）

**B. 消息组件**
- [ ] `UserMessageComponent`：OSC 133 A/B/C 位置与顺序、高度不变、user 样式、transform
- [ ] `AssistantMessageComponent`：OSC 133 条件、thinking 合并与逐 run 折叠、四种 stopReason 文案、
      outputPad、transformer 重放
- [ ] `BranchSummaryMessageComponent` / `CompactionSummaryMessageComponent`：`[branch]`/`[compaction]`、
      `toLocaleString` 千分位、点击折叠
- [ ] `SkillInvocationMessageComponent`：`[skill]`、expanded markdown、collapsed 单行
- [ ] `CustomMessageComponent` / `CustomEntryComponent`：renderer 优先、异常兜底、`[type]` 标签
- [ ] `BashExecutionComponent`：`$ cmd`、ANSI 剥离、`\r\n`/`\r` 归一、append 语义、20 行预览、
      宽度重算、四态状态行、`getOutput`/`getCommand`
- [ ] `ToolExecutionComponent`：双 shell、renderer 槽位继承、`lastComponent`/`state`、兜底、
      图片复用与一次性转码、`argsPending` 合帧、self shell 鼠标坐标、零高度

**C. 编辑器与状态**
- [ ] `CustomEditor.renderTopBorder`：四分支、总宽 = width、thinkingHigh 出现 4 次
- [ ] `CustomEditor.handleInput`：7 步顺序、autocomplete 例外、空文本 Ctrl+D
- [ ] `StatusIndicator` 5 个子类：文案、颜色、倒计时、`renderInBorder`/`renderSpinnerInBorder`
- [ ] `AuthUrlComponent`：OSC 8 ST 形式、clickHint 平台差异、复制成功/失败文案
- [ ] `ExtensionEditorComponent`：外部编辑器命令优先级、TUI stop/start、失败静默

**D. 选择器**
- [ ] `ShowImagesSelector` / `ThemeSelector`：items、maxVisible、layout、预览回调
- [ ] `ThinkingSelector`：✓ 标记、`· default`、Ctrl+S、过滤保选中、paddingX=0
- [ ] `TrustSelector`：`getProjectTrustOptions(cwd)`、`formatDecision` 四态（none/trusted/untrusted/inherited）、
      选中标签用 accent、up/down clamp、updates 返回（见 §2.54.9）
- [ ] `FirstTimeSetup`：logo 4 行、两步、analytics 4 行段落逐字、主题预览
- [ ] `OAuthSelector`：`formatAuthSelectorProviderType/Status` 全分支、三档空消息、maxVisible 8
- [ ] `ExtensionSelector` / `ExtensionInput`：倒计时标题、hints、键位（含 `k`/`j`/`\n`）、**上/下 clamp 不环绕**、`onSelect` 单参数
- [ ] `UserMessageSelector`：每消息 3 行（正文/`Message i of N`/空行）、**up/down 环绕**（不是 clamp）、
      **空列表 100 ms 自动取消**、滚动行 muted（见 §2.54.8 的再次更正）
- [ ] `ModelSelector`：scope 切换、默认搜索、distinct name、成功/失败/超时文案、选中归零
- [ ] `ScopedModelsSelector`：`normalizeEnabled` 幂等、reorder 限制、unavailable 显示、footer 全 parts、
      搜索范围限定、`ctrl+c` 清搜索/取消
- [ ] `SelectSubmenu` / `SteppedSubmenu`：`Step i/N` 拼在 **description** 前、Esc 回退删 context、
      loop 重置、**非 loop 完成后还调用 `onCancel()`**
- [ ] `SettingsSelector`：35 项顺序/文案/values 逐字、onChange 映射、ThemeSubmenu 三态、Warnings 子菜单
- [ ] `ConfigSelector`：`formatBaseDir`、`getGroupLabel` 全分支、三态循环、`[+]`/`[-]`/`[x]`/`[ ]`、
      `> ` 光标、chrome 8、tab 切换、pattern 写入
- [ ] `SessionSelector`：header 3 行全分支、四种空消息、树前缀 `│  `/`└─ `/`├─ `、右栏内容、
      `selectedBg`、scroll 指示、删除确认拦截、`trash`→`unlink`、`trash: ` hint 200 截断、
      rename 模式、scope 竞态、符号链接规范化
- [ ] `session-selector-search`：`ParsedSearchQuery{mode,tokens,regex,error}` / `MatchResult` 形状、
      tier 常量与打分、短语归一化、`re:`、nameFilter、非法 regex → `[]`、**稳定排序**（见 §2.60.4）
- [ ] `FooterComponent` 缓存键 5 项、`latestCacheHitRate` 每次 assistant 覆盖、`contextPercent` 的
      `!== null` 语义、状态行不套 dim（见 §2.60.3）
- [ ] `TreeSelector`：flatten 缩进规则（3 列/层）、gutter、connector、`⊞`/`⊟`、水平视口平移、
      active path `• `、label `[x] `、时间戳、底部 `  (i/N)[ labels]`、5 种过滤、搜索 AND、
      折叠/展开/段跳、`ctrl+x` 复制、`LabelInput`
- [ ] `LoginDialog`：showAuth/showDeviceCode/showManualInput/showPrompt/showDetails/showInfo/
      showWaiting/showProgress、`osc8` BEL 形式、device code 一次性、`Login cancelled`
- [ ] `RadiusLoginMenu` / `radiusShimmer`：smoothstep、4 色、`\x1b[39m` 结尾、50 ms 动画

**E. 页脚与装饰**
- [ ] `FooterComponent`：两/三行、`formatTokens` 五档、`formatCwdForFooter`、stats 全字段、
      `(sub)` 判定、上下文颜色阈值、xp 徽标、右侧 provider 回退、扩展状态排序（`localeCompare`）与
      **状态行整行不套 dim**、`latestCacheHitRate` 每遇 assistant 覆盖
- [ ] `ArminComponent`：144 字节位图、7 种效果、半块渲染、`ARMIN SAYS HI`（**不截断**）、width+gridVersion 缓存
- [ ] `EasterEgg3d`：常量、`hash`（返回 f32 0..1）、`shuffleSteps`（moves 池 + lastMoved）、`parseScreen`、
      光栅（f32 缓冲）、braille、halo、退出动画（`close()` 二次调用直接结束）
- [ ] `EarendilAnnouncement`：文案、图片可选、边框 accent
- [ ] `BorderedLoader`：cancellable 两态、hints、signal、dispose
- [ ] `diff.renderDiff`：parseDiffLine（`prefix/lineNum/content`）、intra-line 仅首段去空白、三种前缀、
      **非匹配行不 replaceTabs**、颜色 token（见 §2.57）

**F. 非功能**
- [ ] 每个组件在 width ∈ {1,4,10,20,80,120} 下所有行 `visible_width ≤ width`
- [ ] 所有颜色在 render 时解析（主题切换后无需重建即可变色；除 `easter-egg` 的 ansiCache）
- [ ] 所有 `dispose()` 幂等
- [ ] 无 panic：空列表、空消息、缺字段、超长输入、`undefined` 等价物的 `Option`
- [ ] 无不必要阻塞：`render` 中无 I/O、无 `sleep`
- [ ] 所有用户可见字符串与 §3 逐字一致（用统一快照文件比对）

### 8.5 未决问题

1. **`model-search.ts` / `model-catalog-refresh.ts` / `external-editor.ts` 的归属**：
   它们不在 `components/` 目录内，但本单元的 `model-selector`/`extension-editor` 直接依赖。
   建议由 **ca-interactive-core** 单元负责（或并入本单元）。需与其他单元协调，避免重复实现。
2. **`theme` 与 `theme-tokens` 的单元归属**：本单元大量依赖 `theme.fg`/`getMarkdownTheme` 等。
   若 ca-theme 单元未定义，本单元需自带（不建议）。
3. **`grok-mermaid` 源码不可得**：仓库无 `node_modules`，且该包不在 npm 公共镜像中（内部包？）。
   需要产品方提供：要么给出源码，要么接受「只复刻测试覆盖的图形 + 其余回退为原 code block」。
4. **`easter-egg-3d` 的随机性对拍**：TS 侧不可注入 RNG（只读），因此逐帧一致不可验证。
   需产品决策：是否接受「视觉一致但非逐位一致」。
5. **`Intl.Segmenter` / `string-width` 的 Unicode 版本**：Node 22 的 ICU 版本 vs Rust `unicode-width`
   的版本可能有差异（emoji ZWJ、新的东亚宽字符）。需在测试机上比对一批样本。
6. **平台分支**：本规格以 Linux 为准；macOS（`platform === "darwin"`）与 Windows（`"win32"`、`WSL`）
   的文案/键位/命令（`trash`、`notepad`、`shell: true`）需要各自 CI 覆盖。
7. **`getCapabilities()` 与图片**：Rust 侧如何探测终端图片协议（iTerm2/Kitty/Sixel/半块）需与
   tui-terminal 单元对齐；`show-images` 设置项**是否存在**取决于此。
8. **`tui.queryTerminalColors` 的 OSC 查询时序**：3D 彩蛋依赖 100 ms 内返回；Rust 需要在
   alt-screen 上同步读取 stdin 响应，可能与输入循环冲突。需 tui-terminal 单元提供 API。
9. **`SettingsConfig.availableDefaultModels` 与 `modelThinkingLevels` 的键格式**
   （`${provider}/${id}`）由宿主提供；若 provider 含 `/` 会歧义（现实现未处理）。保持现状即可。
10. **`footer` 的 `getRoutedModel()`**：虚拟模型路由显示 `→ <model>`，需 ca-config-models 提供
    `routed_model()`；若未提供则该行缺失，测试 `shows the physical model a virtual model routed to` 会失败。
11. **`tui.select.cancel` 的默认键是 `["escape","ctrl+c"]`**：因此所有选择器里 Ctrl+C 都是「取消」
    而**不是**复制/中断。`CustomEditor` 里 Ctrl+C 走 `app.clear`（清空编辑器），两者不冲突（组件树不同层），
    但如果 Rust 侧把「Ctrl+C 全局中断」做成顶层优先，会破坏选择器的取消语义。需 interactive-mode 单元确认
    全局分发的层级。
12. **`toLocaleString()` 的 locale**：`compaction-summary-message` 用默认 locale 的千分位。
    Node 的默认 locale 来自 `LANG`/`LC_ALL`（测试机若为 `C` 则可能**不带**千分位）。Rust 侧应固定为
    en-US 风格（`1,234`）并写死，避免依赖环境；需产品确认（测试 `tokensBefore: 1234` 断言 `"1,234"`）。

---

## 附录 A. 组件清单（按 §2 小节索引）

| § | 文件 | 主要导出 | 有测试 |
|---|---|---|---|
| 2.1 | `index.ts` | barrel | — |
| 2.2 | `dynamic-border.ts` | `DynamicBorder` | 间接 |
| 2.3 | `themed-text.ts` | `ThemedText` | ✓ |
| 2.4 | `markdown-transform.ts` | `createMarkdownTransform` | 间接（assistant/user） |
| 2.5 | `auth-url.ts` | `AuthUrlComponent` | ✓ |
| 2.6 | `countdown-timer.ts` | `CountdownTimer` | 间接（status-indicator） |
| 2.7 | `kiana-logo.ts` | `kianaLogoLines`/`supportsKianaLogo`/`kianaWordmark` | — |
| 2.8 | `keybinding-hints.ts` | `keyText`/`keyDisplayText`/`keyHint`/`rawKeyHint`/`formatKeyText` | 间接 |
| 2.9 / 2.54.2 | `show-images-selector.ts` | `ShowImagesSelectorComponent` | — |
| 2.10 / 2.54.7 | `earendil-announcement.ts` | `EarendilAnnouncementComponent` | — |
| 2.11 / 2.54.1 | `theme-selector.ts` | `ThemeSelectorComponent` | — |
| 2.12 / 2.54.6 | `bordered-loader.ts` | `BorderedLoader` | — |
| 2.13 | `skill-invocation-message.ts` | `SkillInvocationMessageComponent` | ✓（折叠） |
| 2.14 | `custom-entry.ts` | `CustomEntryComponent` | 间接 |
| 2.15 | `branch-summary-message.ts` | `BranchSummaryMessageComponent` | ✓（折叠） |
| 2.16 | `compaction-summary-message.ts` | `CompactionSummaryMessageComponent` | ✓（折叠/output-pad） |
| 2.17 / 2.55.3 | `user-message.ts` | **仅** `UserMessageComponent`（三个 OSC 常量是模块私有） | ✓（4） |
| 2.18 | `mermaid.ts` | `createMermaidMarkdownTransformer` | ✓ |
| 2.19 / 2.54.3 | `extension-input.ts` | `ExtensionInputComponent` | 间接（5433） |
| 2.20 | `visual-truncate.ts` | `truncateToVisualLines`/`VisualLinePreview` | 间接 |
| 2.21 | `custom-message.ts` | `CustomMessageComponent` | ✓ |
| 2.22 / 2.54.5 | `radius-login-selector.ts` | `createLoginMenuSelector` | — |
| 2.23 / 2.54.4 | `extension-selector.ts` | `ExtensionSelectorComponent` | 间接 |
| 2.24 / 2.55.4 | `status-indicator.ts` | `StatusIndicator` + 4 子类 + `IdleStatus` | ✓ |
| 2.25 / 2.54.9 | `trust-selector.ts` | `TrustSelectorComponent`/`TrustSelection` | ✓ |
| 2.26 / 2.54.10 | `extension-editor.ts` | `ExtensionEditorComponent` | — |
| 2.27 | `diff.ts` | `renderDiff` | 间接 |
| 2.28 | `custom-editor.ts` | `CustomEditor` | ✓ |
| 2.29 / 2.51 | `tool-execution.ts` | `ToolExecutionComponent` | ✓（26） |
| 2.30 / 2.53.1 | `model-selector.ts` | `ModelSelectorComponent` | ✓ |
| 2.31 / 2.53.2 | `scoped-models-selector.ts` | `ScopedModelsSelectorComponent` + `ModelsConfig`/`ModelsCallbacks` | ✓ |
| 2.32 / 2.53.3 | `oauth-selector.ts` | `OAuthSelectorComponent` + 2 格式化函数 | ✓ |
| 2.33 / 2.52.1-2 | `settings-submenu.ts` | `SelectSubmenu`/`SteppedSubmenu` | 间接 |
| 2.34 | `session-selector-search.ts` | `parseSearchQuery`/`matchSession`/`filterAndSortSessions` | ✓（14） |
| 2.35 | `footer.ts` | `FooterComponent` + `formatTokens`/`formatCwdForFooter`（`sanitizeStatusText` **私有**） | ✓（11） |
| 2.36 | `login-dialog.ts` | `LoginDialogComponent` | ✓（`login-dialog-device-code` 7 声明/9 运行 + `auth-url-copy` 3） |
| 2.37 / 2.54.8 | `user-message-selector.ts` | `UserMessageSelectorComponent`（`UserMessageList` 未导出） | —（无直接单测；空列表自动取消见 §2.54.8） |
| 2.38 / 2.55.1 | `assistant-message.ts` | `AssistantMessageComponent` | ✓（12） |
| 2.39 / 2.55.2 | `bash-execution.ts` | `BashExecutionComponent` | ✓（2+output-pad） |
| 2.40 / 2.52.3-4 | `settings-selector.ts` | `SettingsSelectorComponent` + `SettingsConfig`/`SettingsCallbacks` | ✓（5） |
| 2.41 / 2.52.5 | `config-selector.ts` | **仅** `ConfigSelectorComponent` + `type ScopedResolvedPaths`（`ResourceList`/`ConfigSelectorHeader` **未导出**） | 间接（35，`package-command-paths.test.ts`） |
| 2.42 | `session-selector.ts` | **仅** `SessionSelectorComponent`（header/list/两个纯函数均未导出） | ✓（25 = 14+8+3） |
| 2.43 / 2.51 | `tree-selector.ts` | **仅** `TreeSelectorComponent` + `type FilterMode`（`TreeList`/`SearchLine`/`TreeHelp`/`LabelInput` 未导出） | ✓（18） |
| 2.44 | `armin.ts` | `ArminComponent`/`isArminPixel`/`ARMIN_WIDTH`/`ARMIN_HEIGHT` | — |
| 2.45 | `easter-egg-3d.ts` | `type EasterEgg3d`/`playEasterEgg3d`/`EasterEgg3dAnimation`（**无 `dispose()`**） | — |
| 2.46 | `easter-egg-3d.lazy.ts` | `playKianaLogo3d`/`playArmin3d`（`playEasterEgg3d` 是**私有**） | — |
| 2.48 / 2.52.6 | `first-time-setup.ts` | `FirstTimeSetupComponent`/`FirstTimeSetupResult`/`FirstTimeSetupOptions` | ✓（`first-time-setup.test.ts` 8 + `first-time-setup-fork.test.ts` 1） |
| 2.50 / 2.53.4 | `thinking-selector.ts` | `ThinkingSelectorComponent` | ✓（2） |

## 附录 B. 关键常量速查

```
PREVIEW_LINES (bash)                    = 20
FALLBACK_PREVIEW_LINES (tool)           = 10
SelectList maxVisible: show-images 5, theme 10, thinking = items.length, oauth 8,
                        model 10, scoped-model 8, settings submenu min(len,10),
                        settings list 10, settings theme automatic 4
ConfigSelector chrome                   = 8
TreeSelector maxVisibleLines            = max(5, floor(rows/2))
SessionSelector maxVisible              = 10（硬编码，宿主**不可**覆盖）
tree indent 单位                        = 3 列/层
tree 水平视口 gutter                    = 2 列
ModelSelector refresh timeout           = 15_000 ms
LoginDialog 终端颜色查询                 = 100 ms
user-message-selector 自动取消           = 100 ms（空列表，组件构造时起定时器）
tree-selector 空树自动取消               = 100 ms
session-selector 状态消息 2000/3000/4000 ms
Loader 帧间隔                           = 80 ms
CountdownTimer 滴答                     = 1000 ms
Armin fps                               = 30（glitch 60）
Radius 动画帧                            = 50 ms；10 字符/秒；4 色 × 4 字符
3D 彩蛋 fps                              = 30（FRAME_MS = 1000/30）
TREE_GUTTER_WIDTH                       = 2
MIN/MAX_VISIBLE_ANCHOR_CONTENT_WIDTH    = 4 / 20
MIN/MAX_ANCHOR_CONTEXT_WIDTH            = 2 / 12
ARMIN_WIDTH/HEIGHT                      = 31 / 36（显示 18 行）
DEFAULT_MAX_LINES / DEFAULT_MAX_BYTES   = 2000 / 51200
```

## 附录 C. 逐字字符串总表（以源码为准，供快照测试引用）

> 下面按「出现顺序」列出，便于生成 golden 文件。`{}` 为插值。

```
  No resources found                     config-selector 空列表
  No matching models                     model / scoped-models 空列表
  No entries found                       tree 空
  No user messages found                 user-message-selector 空
  No named sessions found. Press {} to show all.
  No named sessions in current folder. Press {} to show all, or Tab to view all.
  No sessions found
  No sessions in current folder. Press Tab to view all.
  No providers available
  No providers logged in. Use /login first.
  No matching providers
  No matching commands                   SelectList（tui）
Model unavailable
Model Name: {}
Refreshing model catalogs…
Model catalogs refreshed.
Model refresh timed out; showing cached models.
Could not refresh {}; showing cached models.
Could not refresh {} model catalogs ({}); showing cached models.
Could not refresh model catalogs: {}
Only showing models from configured providers. Use /login to add providers.
Scope: all | scoped
all
scoped
  automatic
Automatic Theme
Choose themes for terminal light and dark appearance.
Light/dark detection requires terminal support.
Select a theme, or choose automatic to follow terminal appearance.
Theme created from your terminal's colors
Use separate themes for light and dark terminal appearance
Light theme / Dark theme / Light Theme / Dark Theme
Select the theme to use for light terminal appearance
Select the theme to use for dark terminal appearance
Theme to use in automatic mode when the terminal is light
Theme to use in automatic mode when the terminal is dark
Apply / Save and go back / save and go back
Change mode / Switch to one theme for light and dark / switch to single theme
none / {} configured
Auto-compact
Automatically compact context when it gets too large
Steering mode
Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.
Follow-up mode
{} queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.
Transport
Preferred transport for providers that support multiple transports
Session share destination
Where to upload sessions with /share (auto prefers Radius when authenticated, otherwise GitHub Gist)
HTTP idle timeout
Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five minutes.
Cache warming
off; streaming while the agent runs; idle also between runs while continuation stays profitable
Hide thinking / Hide thinking blocks in assistant responses
Mermaid diagrams / Render Mermaid code blocks as Unicode diagrams
Cache miss notices / Show transcript notices for cache costs and provider recovery diagnostics
Collapse changelog / Show condensed changelog after updates
Quiet startup / Disable verbose printing at startup (header: keep only the startup header)
Install telemetry / Send an anonymous version/update ping after changelog-detected updates
Default project trust / Fallback behavior when no extension or saved trust decision decides project trust
Ask / Always trust / Never trust
Double-escape action / Action when pressing Escape twice with empty editor
Tree filter mode / Default filter when opening /tree
Warnings / Enable or disable individual warnings
Anthropic extra usage / Warn when Anthropic subscription auth may use paid extra usage
Default thinking level per model
Override the default thinking level for specific models. {} cycles in-session.
Per-Model Thinking Level / Select a model to configure
No models available / Log in to a provider or configure an API key first
Thinking Level for {} / Select default thinking level for this model
  (clear override) / Revert to global default ({})
TUI mode / Interface layout; regular mode uses the terminal's normal scrollback
Fullscreen exit output / Print the transcript or only a session resume hint when exiting fullscreen mode
Fullscreen scrollbar / Scrollbar behavior in fullscreen mode; has no effect in regular mode
Fullscreen copy on select / Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X
Fullscreen wheel scrolling
Lines per mouse-wheel event in fullscreen mode; 'auto' speeds up fast wheel spins where the terminal does not
Theme / Color theme for the interface
Show images / Render images inline in terminal
Image width / Preferred inline image width in terminal cells
Auto-resize images / Resize large images to 2000x2000 max for better model compatibility
Block images / Prevent images from being sent to LLM providers
Skill commands / Register skills as /skill:name commands
Show hardware cursor / Use the terminal cursor instead of Kiana's drawn cursor
Editor padding / Horizontal padding for input editor (0-3)
Output padding / Horizontal padding for messages, tool output, and command output
Autocomplete max items / Max visible items in autocomplete dropdown (3-20)
Clear on shrink / Clear empty rows when content shrinks (may cause flicker)
Terminal progress / Show OSC 9;4 progress indicators in the terminal tab bar
configure
Session-only. {} to save to settings.
Model Configuration
  Type to filter · Enter to select · Esc to go back
  Enter to select · Esc to go back
Step {}/{} · 
Project trust
Saved decision: {}
Current session: {}
trusted / untrusted / none
 (inherited from {})
Login to {}
Login cancelled
Cmd+click to open / Ctrl+click to open
Copied URL to clipboard
Enter code: {}
Copying code...
Browser open requested
Code copied to clipboard
Copy code manually
e.g., {}
to cancel / to cancel, / to submit / to close
to open browser and copy code / to open browser / to copy code
Session moved to trash / Session deleted
Failed to delete: {}
Failed to load sessions: {}
Cannot delete the currently active session
Delete session? {} · {}
Resume Session (Current Folder) / Resume Session (All)
Sort: {} / Name: {}
Threaded / Recent / Fuzzy / All / Named
○ Current Folder |  / ◉ Current Folder /  | ○ All / ◉ All
Loading {}/{} 
re:<pattern> regex · "phrase" exact
scope / sort / named / delete / path (on) / path (off) / rename
Rename Session
{} to save · {} to cancel
trash: {}
  Session Tree
Type to search:
Label (empty to remove):
save / cancel
move / page / branch / copy / label / label time / filters / cycle
pgup / pgdn / ↑ / ↓ / ← / →
[no-tools] / [user] / [labeled] / [all] / [+label time]
user:  / assistant:  / (aborted) / (no content) / (cleared)
[bash]: {} / [bash: {}] / [read: {}{}] / [write: {}] / [edit: {}] / [grep: /{}/ in {}] / [find: {} in {}] / [ls: {}]
[compaction: {}k tokens] / [branch summary]:  / [model: {}] / [thinking: {}]
[custom: {}] / [context omit: {}] / [context replace: {}] / [label: {}] / [title: {}]
empty
{ } to return
kiana has joined Earendil
Read the blog post:
https://mariozechner.at/posts/2026-04-08-ive-sold-out/
[skill]
 to expand
[branch]
**Branch Summary**
Branch summary (
[compaction]
**Compacted from {} tokens**
Compacted from {} tokens (
[customType] renderer failed: {}
Mermaid diagram not rendered: {}
 (+{} more)
Thinking...
Response was truncated before completion.
Operation aborted
Unknown error
Error: {}
$ {}
... {} more lines ({} to expand)
({} to collapse)
(cancelled)
(exit {})
Output truncated. Full output: {}
Running... ({} to cancel)
ARMIN SAYS HI
Select provider to configure:
Select provider to logout:
API key / account / subscription
 • not configured /  • {} configured /  ✓ configured /  ✓ env: {} /  ✓ {}
submit / newline / external editor
navigate / select / continue / finish / skip setup
Pick a theme.
Opt-in to anonymous usage data sharing?
Welcome to {}, the minimal coding agent.
System (matches your terminal colors) / Dark / Light
Share anonymous usage data / Don't share
Opting in stores a tracking identifier in settings.json and enables anonymous
usage analytics. This helps us to better debug, reproduce, and resolve issues
and bugs within Kiana. You can observe what is shared using /privacy and make
changes anytime in settings.json.
Thinking Level
{} cycles thinking levels in-session
{} to select · {} to set as default · {} to cancel
No reasoning / Very brief reasoning (~1k tokens) / Light reasoning (~2k tokens) /
Moderate reasoning (~8k tokens) / Deep reasoning (~16k tokens) /
Extra-high reasoning (~32k tokens) / Maximum reasoning
 · default
Fork from Message
Select a user message to copy the active path up to that point into a new session
  Message {} of {}
Yes / Show images inline in terminal
No / Show text placeholder instead
(current)
Show images / Image width / ...
(unsaved)
all enabled / {}/{} enabled / · {} unavailable
toggle / all / clear / provider / reorder / save
  Model Name: {} / Model unavailable
  No user messages found
  No entries found
  (0/0)
```

（本表是**索引**，不是替换源码的权威来源；任何冲突以源码与 §2.5x 为准。）

#### 2.60.10 `session-selector.ts`（1045 行）—— 逐行权威描述

导出面：**仅** `SessionSelectorComponent`。`SessionSelectorHeader`、`SessionList`、`buildSessionTree`、
`flattenSessionTree`、`shortenPath`、`formatSessionDate`、`deleteSessionFile`、`SessionsLoader` 都是
**模块私有**（§2.42 把它们当导出是错误的）。

**顶层工具函数**：

```ts
function shortenPath(path: string): string {
  const home = os.homedir();                 // ← 用 os.homedir()，**不是** process.env.HOME
  if (!path) return path;
  if (path.startsWith(home)) return `~${path.slice(home.length)}`;
  return path;
}

function formatSessionDate(date: Date): string {
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMs / 3600000);
  const diffDays = Math.floor(diffMs / 86400000);
  if (diffMins < 1) return "now";
  if (diffMins < 60) return `${diffMins}m`;
  if (diffHours < 24) return `${diffHours}h`;
  if (diffDays < 7) return `${diffDays}d`;
  if (diffDays < 30) return `${Math.floor(diffDays / 7)}w`;
  if (diffDays < 365) return `${Math.floor(diffDays / 30)}mo`;
  return `${Math.floor(diffDays / 365)}y`;
}

function canonicalizePath(path: string | undefined): string | undefined {
  if (!path) return path;
  return _canonicalizePath(path);            // utils/paths.ts
}
```

- `shortenPath` 的 `home` 是 `os.homedir()`（Rust: `dirs::home_dir()`），**不是** 环境变量；
  与 `footer.ts` 的 `formatCwdForFooter`（用 `HOME`/`USERPROFILE` + `path.relative`）**不同实现**，不要合并。
- `formatSessionDate` 的 `diffMs` 为负（未来时间）时 `diffMins < 1` → `"now"`。

**`SessionSelectorHeader implements Component`**（私有类）：

```ts
constructor(scope: "current"|"all", sortMode: SortMode, nameFilter: NameFilter, requestRender: () => void)
setScope(x) / setSortMode(x) / setNameFilter(x)
setLoading(loading: boolean)                 // 同时把 loadProgress 置 null
setProgress(loaded, total)
setShowPath(b) / setShowRenameHint(b) / setConfirmingDeletePath(path | null)
setStatusMessage(msg: {type:"info"|"error"; message:string} | null, autoHideMs?: number)
invalidate(): void {}                        // 空
render(width): string[]                      // 恰好 3 行
```

- 字段：`loading`、`loadProgress: {loaded,total} | null`、`showPath`、`confirmingDeletePath`、
  `statusMessage`、`statusTimeout`、`showRenameHint`。
- `setStatusMessage`：先 `clearStatusTimeout()`；`this.statusMessage = msg`；`if (!msg || !autoHideMs) return;`
  否则 `setTimeout(() => { statusMessage = null; statusTimeout = null; requestRender(); }, autoHideMs)`。
  - **默认 `autoHideMs` 未定义**（不是 2000）→ 不传 autoHide 的消息**永久显示**，直到被显式清空。
    实际调用方都传了（2000/3000/4000），但 Rust 签名要允许 `None`。
- `render(width)` 的三行（与 §2.42 一致，逐字复核）：
  1. `title = scope === "current" ? "Resume Session (Current Folder)" : "Resume Session (All)"`；
     `leftText = theme.bold(title)`；
     `sortLabel = "threaded"→"Threaded" | "recent"→"Recent" | 其它→"Fuzzy"`；
     `sortText = fg("muted","Sort: ") + fg("accent", sortLabel)`；
     `nameLabel = all→"All" | named→"Named"`；`nameText = fg("muted","Name: ") + fg("accent", nameLabel)`；
     `scopeText`：loading → `fg("muted","○ Current Folder | ") + fg("accent", `Loading ${progressText}`)`，
     `progressText = loadProgress ? `${loaded}/${total}` : "..."`；current →
     `fg("accent","◉ Current Folder") + fg("muted"," | ○ All")`；all →
     `fg("muted","○ Current Folder | ") + fg("accent","◉ All")`；
     `rightText = truncateToWidth(`${scopeText}  ${nameText}  ${sortText}`, width, "")`（**两空格**分隔）；
     `availableLeft = Math.max(0, width - visibleWidth(rightText) - 1)`；
     `left = truncateToWidth(leftText, availableLeft, "")`；
     `spacing = Math.max(0, width - visibleWidth(left) - visibleWidth(rightText))`；
     行 = `left + " ".repeat(spacing) + rightText`。
  2. `confirmingDeletePath !== null` →
     `theme.fg("error", truncateToWidth(`Delete session? ${keyHint("tui.select.confirm","confirm")} · ${keyHint("tui.select.cancel","cancel")}`, width, "…"))`；
     否则 `statusMessage` → `theme.fg(statusMessage.type === "error" ? "error" : "accent", truncateToWidth(message, width, "…"))`；
     否则 `keyHint("tui.input.tab","scope") + sep + theme.fg("muted", 're:<pattern> regex · "phrase" exact')`
     再 `truncateToWidth(..., width, "…")`，`sep = theme.fg("muted"," · ")`。
  3. 前两种状态 → `""`；否则
     `[keyHint("app.session.toggleSort","sort"), keyHint("app.session.toggleNamedFilter","named"),
       keyHint("app.session.delete","delete"), keyHint("app.session.togglePath", `path ${showPath ? "(on)" : "(off)"}`)]`
     （`showRenameHint` 时再 append `keyHint("app.session.rename","rename")`），`join(sep)`，
     `truncateToWidth(..., width, "…")`。
- **`invalidate()` 为空**；`render` 不缓存。

**树构建（私有）**：

```ts
function buildSessionTree(sessions: SessionInfo[]): SessionTreeNode[] {
  const byPath = new Map<string, SessionTreeNode>();
  for (const session of sessions) {
    const sessionPath = canonicalizePath(session.path) ?? session.path;
    byPath.set(sessionPath, { session, children: [], latestActivity: session.modified.getTime() });
  }
  const roots: SessionTreeNode[] = [];
  for (const session of sessions) {
    const sessionPath = canonicalizePath(session.path) ?? session.path;
    const node = byPath.get(sessionPath)!;
    const parentPath = canonicalizePath(session.parentSessionPath);
    if (parentPath && byPath.has(parentPath)) byPath.get(parentPath)!.children.push(node);
    else roots.push(node);
  }
  const updateLatestActivity = (node) => {
    let latest = node.session.modified.getTime();
    for (const child of node.children) latest = Math.max(latest, updateLatestActivity(child));
    node.latestActivity = latest; return latest;
  };
  for (const root of roots) updateLatestActivity(root);
  const sortNodes = (nodes) => { nodes.sort((a,b) => b.latestActivity - a.latestActivity);
                                 for (const n of nodes) sortNodes(n.children); };
  sortNodes(roots);
  return roots;
}

function flattenSessionTree(roots: SessionTreeNode[]): FlatSessionNode[] {
  const result = [];
  const walk = (node, depth, ancestorContinues, isLast) => {
    result.push({ session: node.session, depth, isLast, ancestorContinues });
    for (let i = 0; i < node.children.length; i++) {
      const childIsLast = i === node.children.length - 1;
      const continues = depth > 0 ? !isLast : false;          // ← 根的 continues 恒 false
      walk(node.children[i], depth + 1, [...ancestorContinues, continues], childIsLast);
    }
  };
  for (let i = 0; i < roots.length; i++) walk(roots[i], 0, [], i === roots.length - 1);
  return result;
}
```

- **同一个 path 出现两次**时 `byPath.set` 后写覆盖前写（第一个节点被丢弃但仍可能被 push 到 roots/children）。
  Rust 用 `HashMap<String, usize>` 时需注意「后写覆盖」语义（节点索引指向最后一个）。
- `canonicalizePath(undefined)` 返回 `undefined`；`canonicalizePath("")` 返回 `""`（falsy）。
- 排序是**递归**的、按键 `latestActivity` 降序；相同值保持原序（V8 稳定排序）。

**`SessionList implements Component, Focusable`**（私有类）：

字段：`allSessions`、`filteredSessions: FlatSessionNode[]`、`selectedIndex`、`selectionTouched`、
`searchInput: Input`、`showCwd`、`sortMode`、`nameFilter`、`keybindings: KeybindingsManager`、`showPath`、
`confirmingDeletePath`、`currentSessionCanonicalPath?`、`maxVisible = 10`（**硬编码 10，没有构造参数**）、
`_focused`，以及公开回调 `onSelect?`、`onCancel?`、`onExit = () => {}`、`onToggleScope?`、`onToggleSort?`、
`onToggleNameFilter?`、`onTogglePath?`、`onDeleteConfirmationChange?`、`onDeleteSession?`、`onRenameSession?`、
`onError?`。

- 构造 `(sessions, showCwd, sortMode, nameFilter, keybindings, currentSessionFilePath?)`：
  `searchInput = new Input()`；`currentSessionCanonicalPath = canonicalizePath(currentSessionFilePath)`；
  **`this.filterSessions("")`（构造时就过滤一次）**；`searchInput.onSubmit` → 有选中项则 `onSelect(path)`。
  - **`searchInput` 没有 `onChange` 回调**（§2.42 说有，错误）——过滤是在 `handleInput` 的兜底分支里
    手动 `handleInput + filterSessions(getValue())`。
- `getSelectedSessionPath()` 返回 `filteredSessions[selectedIndex]?.session.path`。
- `setSortMode` / `setNameFilter`：设值后 `filterSessions(searchInput.getValue())`。
- `setSessions(sessions, showCwd)`：
  ```ts
  const selectedPath = this.selectionTouched ? this.getSelectedSessionPath() : undefined;
  this.allSessions = sessions; this.showCwd = showCwd;
  this.filterSessions(this.searchInput.getValue());
  if (!this.selectionTouched) this.selectedIndex = 0;
  else if (selectedPath) {
    const i = this.filteredSessions.findIndex(n => n.session.path === selectedPath);
    if (i >= 0) this.selectedIndex = i;
  }
  ```
- `filterSessions(query)`：
  ```ts
  const trimmed = query.trim();
  const nameFiltered = this.nameFilter === "all" ? this.allSessions
                    : this.allSessions.filter(s => hasSessionName(s));
  if (this.sortMode === "threaded" && !trimmed) {
    this.filteredSessions = flattenSessionTree(buildSessionTree(nameFiltered));
  } else {
    const filtered = filterAndSortSessions(nameFiltered, query, this.sortMode, "all");   // ← 传 "all"
    this.filteredSessions = filtered.map(s => ({ session: s, depth: 0, isLast: true, ancestorContinues: [] }));
  }
  this.selectedIndex = Math.min(this.selectedIndex, Math.max(0, this.filteredSessions.length - 1));
  ```
  - 注意：**nameFilter 已在外层应用，传给 `filterAndSortSessions` 的是 `"all"`**（避免双重过滤）。
  - 传的是**原始 `query`**（不是 `trimmed`）。
- `setConfirmingDeletePath(path)`：设值 + `onDeleteConfirmationChange?.(path)`。
- `startDeleteConfirmationForSelectedSession()`：无选中 → 返回；`isCurrentSessionPath(path)` →
  `onError?.("Cannot delete the currently active session")` + 返回；否则进入确认态。
- `isCurrentSessionPath(path)`：无 `currentSessionCanonicalPath` → false；
  否则 `(canonicalizePath(path) ?? path) === currentSessionCanonicalPath`。
- `render(width)`（权威，见下）：
  ```ts
  const lines = [];
  lines.push(...this.searchInput.render(width));
  lines.push("");                                  // 搜索框后的空行
  if (this.filteredSessions.length === 0) {
    let emptyMessage;
    if (this.nameFilter === "named") {
      const toggleKey = keyText("app.session.toggleNamedFilter");
      emptyMessage = this.showCwd
        ? `  No named sessions found. Press ${toggleKey} to show all.`
        : `  No named sessions in current folder. Press ${toggleKey} to show all, or Tab to view all.`;
    } else if (this.showCwd) emptyMessage = "  No sessions found";
    else emptyMessage = "  No sessions in current folder. Press Tab to view all.";
    lines.push(theme.fg("muted", truncateToWidth(emptyMessage, width, "…")));
    return lines;                                   // ← 只有 search + 空行 + 1 行消息
  }
  const startIndex = Math.max(0, Math.min(this.selectedIndex - Math.floor(this.maxVisible / 2),
                                         this.filteredSessions.length - this.maxVisible));
  const endIndex = Math.min(startIndex + this.maxVisible, this.filteredSessions.length);
  for (let i = startIndex; i < endIndex; i++) {
    const node = this.filteredSessions[i];
    const session = node.session;
    const isSelected = i === this.selectedIndex;
    const isConfirmingDelete = session.path === this.confirmingDeletePath;
    const isCurrent = this.isCurrentSessionPath(session.path);
    const prefix = this.buildTreePrefix(node);
    const hasName = !!session.name;
    const displayText = session.name ?? session.firstMessage;
    const normalizedMessage = displayText.replace(/[\x00-\x1f\x7f]/g, " ").trim();
    const age = formatSessionDate(session.modified);
    const msgCount = String(session.messageCount);
    let rightPart = `${msgCount} ${age}`;
    if (this.showCwd && session.cwd) rightPart = `${shortenPath(session.cwd)} ${rightPart}`;
    if (this.showPath) rightPart = `${shortenPath(session.path)} ${rightPart}`;
    const cursor = isSelected ? theme.fg("accent", "› ") : "  ";
    const prefixWidth = visibleWidth(prefix);
    const rightWidth = visibleWidth(rightPart) + 2;
    const availableForMsg = width - 2 - prefixWidth - rightWidth;
    const truncatedMsg = truncateToWidth(normalizedMessage, Math.max(10, availableForMsg), "…");
    let messageColor = isConfirmingDelete ? "error" : isCurrent ? "accent" : hasName ? "warning" : null;
    let styledMsg = messageColor ? theme.fg(messageColor, truncatedMsg) : truncatedMsg;
    if (isSelected) styledMsg = theme.bold(styledMsg);
    const leftPart = cursor + theme.fg("dim", prefix) + styledMsg;
    const leftWidth = visibleWidth(leftPart);
    const spacing = Math.max(1, width - leftWidth - visibleWidth(rightPart));
    const styledRight = theme.fg(isConfirmingDelete ? "error" : "dim", rightPart);
    let line = leftPart + " ".repeat(spacing) + styledRight;
    if (isSelected) line = theme.bg("selectedBg", line);
    lines.push(truncateToWidth(line, width));        // ← 无省略号参数
  }
  if (startIndex > 0 || endIndex < this.filteredSessions.length) {
    lines.push(theme.fg("muted", truncateToWidth(`  (${this.selectedIndex + 1}/${this.filteredSessions.length})`, width, "")));
  }
  return lines;
  ```
  - 光标字符是 **`› `（U+203A + 空格）**，不是 `>`。Rust 注意 UTF-8。
  - `hasName` 用 `!!session.name`（空串也算 false），与 `hasSessionName`（trim 后）不同。
  - 行尾 `truncateToWidth(line, width)` 不传 ellipsis → 默认 `"..."`? **源码只有一个参数**，
    因此用 `truncateToWidth` 的默认省略号（tui 默认 `"..."`）。
- `buildTreePrefix(node)`：`depth === 0` → `""`；否则
  `node.ancestorContinues.map(c => c ? "│  " : "   ").join("") + (node.isLast ? "└─ " : "├─ ")`
  （gutter 宽 **3**：`│` + 2 空格 / 3 空格；branch `└─ ` / `├─ `）。
- `handleInput(keyData)`（权威顺序）：
  ```ts
  const kb = getKeybindings();
  if (this.confirmingDeletePath !== null) {                       // ① 拦截所有键
    if (kb.matches(keyData, "tui.select.confirm")) {
      const p = this.confirmingDeletePath; this.setConfirmingDeletePath(null); void this.onDeleteSession?.(p); return;
    }
    if (kb.matches(keyData, "tui.select.cancel")) { this.setConfirmingDeletePath(null); return; }
    return;
  }
  if (kb.matches(keyData, "tui.input.tab")) { this.onToggleScope?.(); return; }              // ②
  if (kb.matches(keyData, "app.session.toggleSort")) { this.onToggleSort?.(); return; }      // ③
  if (this.keybindings.matches(keyData, "app.session.toggleNamedFilter")) {                   // ④ 用注入的 manager
    this.onToggleNameFilter?.(); return; }
  if (kb.matches(keyData, "app.session.togglePath")) {                                       // ⑤
    this.showPath = !this.showPath; this.onTogglePath?.(this.showPath); return; }
  if (kb.matches(keyData, "app.session.delete")) { this.startDeleteConfirmationForSelectedSession(); return; }  // ⑥
  if (kb.matches(keyData, "app.session.rename")) {                                           // ⑦
    const s = this.filteredSessions[this.selectedIndex]; if (s) this.onRenameSession?.(s.session.path); return; }
  if (kb.matches(keyData, "app.session.deleteNoninvasive")) {                                // ⑧
    if (this.searchInput.getValue().length > 0) {
      this.searchInput.handleInput(keyData); this.filterSessions(this.searchInput.getValue()); return;
    }
    this.startDeleteConfirmationForSelectedSession(); return;
  }
  this.selectionTouched = true;                                                              // ⑨ 从这里开始才置位
  if (kb.matches(keyData, "tui.select.up")) this.selectedIndex = Math.max(0, this.selectedIndex - 1);
  else if (kb.matches(keyData, "tui.select.down")) this.selectedIndex = Math.min(this.filteredSessions.length - 1, this.selectedIndex + 1);
  else if (kb.matches(keyData, "tui.select.pageUp")) this.selectedIndex = Math.max(0, this.selectedIndex - this.maxVisible);
  else if (kb.matches(keyData, "tui.select.pageDown")) this.selectedIndex = Math.min(this.filteredSessions.length - 1, this.selectedIndex + this.maxVisible);
  else if (kb.matches(keyData, "tui.select.confirm")) { const s = this.filteredSessions[this.selectedIndex];
                                                       if (s && this.onSelect) this.onSelect(s.session.path); }
  else if (kb.matches(keyData, "tui.select.cancel")) { if (this.onCancel) this.onCancel(); }
  else { this.searchInput.handleInput(keyData); this.filterSessions(this.searchInput.getValue()); }
  ```
  - **④ 用 `this.keybindings`（注入的 manager），其它都用全局 `getKeybindings()`** —— 有意为之（可测性）。
  - **`selectionTouched` 只在 ⑨ 之后置位**：任何在 ①–⑧ 消费的键都**不会**标记「用户已手动移动」，
    因此加载完成后仍会自动定位/归零。`session-selector-path-delete.test.ts` 的「scope race」依赖此点。
  - up/down/pageUp/pageDown **全部 clamp**；**没有环绕**。
  - `filteredSessions.length === 0` 时 `Math.min(-1, i+1)` → `-1` → `selectedIndex = -1`；
    下一帧 `filteredSessions[-1]` 为 `undefined`，各处都用 `if (s)` 防护。Rust 用 `Option<usize>` 或 `isize`。
- `onExit` **从未在此类中被调用**（宿主可调用）；`SessionSelectorComponent` 绑定它但也从不触发
  （只有 onSelect/onCancel 会调用 `cancelLoads`）。保留字段以对齐 API。

**`deleteSessionFile(sessionPath)`**（私有 async）：

```ts
const trashArgs = sessionPath.startsWith("-") ? ["--", sessionPath] : [sessionPath];
const trashResult = spawnSync("trash", trashArgs, { encoding: "utf-8" });
const getTrashErrorHint = () => {
  const parts = [];
  if (trashResult.error) parts.push(trashResult.error.message);
  const stderr = trashResult.stderr?.trim();
  if (stderr) parts.push(stderr.split("\n")[0] ?? stderr);
  return parts.length === 0 ? null : `trash: ${parts.join(" · ").slice(0, 200)}`;
};
if (trashResult.status === 0 || !existsSync(sessionPath)) return { ok: true, method: "trash" };
try { await unlink(sessionPath); return { ok: true, method: "unlink" }; }
catch (err) {
  const unlinkError = err instanceof Error ? err.message : String(err);
  const hint = getTrashErrorHint();
  return { ok: false, method: "unlink", error: hint ? `${unlinkError} (${hint})` : unlinkError };
}
```

- `spawnSync` **没有传 `stdio`** → 默认 `pipe`，`encoding: "utf-8"` 让 `stdout/stderr` 为字符串。
- `status === 0` **或** 文件已不存在 → 视为成功（`method: "trash"`）。
- `slice(0, 200)` 是**UTF-16 长度**截断（Rust 侧按 `chars().take(200)` 会不同；为一致应按 UTF-16 code unit，
  或至少对 ASCII 相同——错误信息几乎都是 ASCII）。
- 返回 `method: "trash"` 即使实际是「文件已不存在」的情形。

**`SessionSelectorComponent extends Container implements Focusable`**：

```ts
constructor(
  currentSessionsLoader: SessionsLoader,
  allSessionsLoader: SessionsLoader,
  onSelect: (sessionPath: string) => void,
  onCancel: () => void,
  onExit: () => void,
  requestRender: () => void,
  options?: { renameSession?; showRenameHint?: boolean; keybindings?: KeybindingsManager },
  currentSessionFilePath?: string,
)
```

- **`maxVisible` 不在 options 里**（`SessionList.maxVisible` 固定 10）。
- `keybindings = options?.keybindings ?? KeybindingsManager.create()`（**默认 `create()` 读磁盘上的
  keybindings.json**，不是 `getKeybindings()`）。
- `canRename = !!options.renameSession`；`header.setShowRenameHint(options?.showRenameHint ?? this.canRename)`。
- `buildBaseLayout(content, { showHeader = true })`：
  ```ts
  this.clear();
  addChild(new Spacer(1));
  addChild(new DynamicBorder(s => theme.fg("accent", s)));
  addChild(new Spacer(1));
  if (showHeader) { addChild(this.header); addChild(new Spacer(1)); }
  addChild(content);
  addChild(new Spacer(1));
  addChild(new DynamicBorder(s => theme.fg("accent", s)));
  ```
  - **上下边框都是 accent**（不是 border）。
  - 构造时 `buildBaseLayout(this.sessionList)`。
- `focused` setter：`sessionList.focused = v; renameInput.focused = v; if (v && mode === "rename") renameInput.focused = true;`
- `handleInput`：`mode === "rename"` 时只处理 `tui.select.cancel`（→ `exitRenameMode()`）与转发给
  `renameInput`；否则 `sessionList.handleInput(data)`。
- 回调绑定（构造时）：
  - `sessionList.onSelect` → `clearStatusMessage(); cancelLoads(); onSelect(path);`
  - `onCancel` → `clearStatusMessage(); cancelLoads(); onCancel();`
  - `onExit` → 同上 + `onExit()`（**从不触发**）
  - `onToggleScope/Sort/NameFilter` → 对应 `toggleX()`；
  - `onRenameSession(path)`：无 `renameSession` → 返回；**当前 scope 的 load 仍在进行 → 返回**（守卫）；
    在 `allSessions ?? []` / `currentSessions ?? []` 里找 `s.path === path` 得 `name`，然后
    `enterRenameMode(path, name)`。
  - `onTogglePath` → `header.setShowPath(showPath); requestRender();`
  - `onDeleteConfirmationChange` → `header.setConfirmingDeletePath(path); requestRender();`
  - `onError(msg)` → `header.setStatusMessage({type:"error",message:msg}, 3000); requestRender();`
  - `onDeleteSession(path)`：
    ```ts
    const result = await deleteSessionFile(path);
    if (result.ok) {
      if (currentSessions) currentSessions = currentSessions.filter(s => s.path !== path);
      if (allSessions)     allSessions     = allSessions.filter(s => s.path !== path);
      const sessions = scope === "all" ? (allSessions ?? []) : (currentSessions ?? []);
      sessionList.setSessions(sessions, scope === "all");
      header.setStatusMessage({ type: "info",
                                 message: result.method === "trash" ? "Session moved to trash" : "Session deleted" }, 2000);
      await refreshSessionsAfterMutation();
    } else {
      header.setStatusMessage({ type: "error", message: `Failed to delete: ${result.error ?? "Unknown error"}` }, 3000);
    }
    requestRender();
    ```
  - 构造末尾 `void this.loadScope("current")`。
- `cancelLoads()`：abort + 置 null + 把对应 `currentSessions`/`allSessions` 置 null。
- `enterRenameMode(path, currentName)`：
  ```ts
  mode = "rename"; renameTargetPath = path; renameInput.setValue(currentName ?? ""); renameInput.focused = true;
  const panel = new Container();
  panel.addChild(new Text(theme.bold("Rename Session"), 1, 0));
  panel.addChild(new Spacer(1));
  panel.addChild(this.renameInput);
  panel.addChild(new Spacer(1));
  panel.addChild(new Text(theme.fg("muted", `${keyText("tui.select.confirm")} to save · ${keyText("tui.select.cancel")} to cancel`), 1, 0));
  buildBaseLayout(panel, { showHeader: false });
  requestRender();
  ```
- `exitRenameMode()`：`mode = "list"; renameTargetPath = null; buildBaseLayout(this.sessionList); requestRender();`
  （**`renameInput.focused` 不重置**，只由 `focused` setter 控制。）
- `confirmRename(value)`：
  ```ts
  const next = value.trim();
  if (!next) return;                            // ← 空值直接返回，停留在 rename 模式
  if (!target) { exitRenameMode(); return; }
  if (!renameSession) { exitRenameMode(); return; }
  try { await renameSession(target, next); await refreshSessionsAfterMutation(); }
  finally { exitRenameMode(); }
  ```
  - **空值不退出**（与 §2.42「空则视为不重命名」的表述不同：它连 rename 模式都不退）。
  - 回调签名是 `renameSession(path, nextName)`（两个参数；`currentName` 只用于预填）。
- `loadScope(scope)`：
  ```ts
  if (scope === "current" ? currentLoad : allLoad) return;         // 已有进行中的同 scope 加载
  const showCwd = scope === "all";
  const controller = new AbortController();
  (scope === "current" ? this.currentLoad = controller : this.allLoad = controller);
  header.setScope(scope); header.setLoading(true); requestRender();
  const isActive = () => (scope === "current" ? this.currentLoad : this.allLoad) === controller;
  const onProgress = (loaded, total, partialSessions?) => {
    if (!isActive()) return;
    if (partialSessions) {
      const sessions = [...partialSessions];
      (scope === "current" ? this.currentSessions = sessions : this.allSessions = sessions);
      if (scope === this.scope) sessionList.setSessions(sessions, showCwd);
    }
    if (scope !== this.scope) return;
    header.setProgress(loaded, total); requestRender();
  };
  try {
    const sessions = await (scope === "current" ? currentSessionsLoader(onProgress, controller.signal)
                                                 : allSessionsLoader(onProgress, controller.signal));
    if (!isActive()) return;
    (scope === "current" ? (this.currentSessions = sessions, this.currentLoad = null)
                         : (this.allSessions = sessions, this.allLoad = null));
    if (scope !== this.scope) return;
    header.setLoading(false); sessionList.setSessions(sessions, showCwd); requestRender();
  } catch (err) {
    if (!isActive()) return;
    (scope === "current" ? (this.currentLoad = null, this.currentSessions = null)
                         : (this.allLoad = null, this.allSessions = null));
    if (scope !== this.scope) return;
    const message = err instanceof Error ? err.message : String(err);
    header.setLoading(false);
    header.setStatusMessage({ type: "error", message: `Failed to load sessions: ${message}` }, 4000);
    sessionList.setSessions([], showCwd); requestRender();
  }
  ```
  - **`header.setScope(scope)` 无条件执行**（即使 `scope !== this.scope`，也会短暂把 header 改成本次加载的
    scope）。Rust 需照抄（渲染一帧后才被下一次 toggle 改回）。
  - **`isActive()` 是竞态核心**：结果只在「controller 仍是当前 scope 的 controller」时落地。
  - 失败时 `sessionList.setSessions([], showCwd)`（清空列表并显示空消息）。
- `toggleSortMode()`：`threaded → recent → relevance → threaded`；`header.setSortMode` + `list.setSortMode` + render。
  注意 **`relevance` 的显示标签是 `Fuzzy`**（header 内映射）。
- `toggleNameFilter()`：`all ↔ named`。
- `toggleScope()`：
  ```ts
  this.scope = this.scope === "current" ? "all" : "current";
  const sessions = this.scope === "current" ? this.currentSessions : this.allSessions;
  const loading = (this.scope === "current" ? this.currentLoad : this.allLoad) !== null;
  header.setScope(this.scope); header.setLoading(loading);
  sessionList.setSessions(sessions ?? [], this.scope === "all");
  requestRender();
  if (sessions === null && !loading) void this.loadScope(this.scope);
  ```
  - **切换时不 abort 另一 scope 的加载**（两边可并行）。
  - `sessions === null` 才触发加载（已有缓存则不重载）。
- `refreshSessionsAfterMutation()`：`cancelLoads(); currentSessions = null; allSessions = null; await loadScope(this.scope);`
- `getSessionList(): SessionList`（公开，供宿主 `setFocus`）。
- `invalidate()` **未覆盖**（用 Container 默认，转发给子组件）。

**测试映射**（`session-selector-path-delete.test.ts` 8 项、`session-selector-rename.test.ts` 3 项）：
- Ctrl+Backspace 在有查询时进搜索框、无查询时进删除确认；
- Ctrl+D 进入确认态、Enter 确认删除、Esc 取消；
- scope 切换竞态（旧请求结果不落地）；
- 符号链接：`canonicalizePath` 让父子关系与「当前会话」判定都成立；
- threaded 模式下子树活动时间；
- 当前会话别名（同一文件不同路径）；
- rename hint 仅在 `canRename` 时显示；Ctrl+R 进入；Enter 提交（trim）；空值不退出。


---

## Audit additions

> 本节由**第二轮对抗性完整性审计**追加：48 个源文件全部重读（含此前只扫过的
> `easter-egg-3d.ts` / `config-selector.ts` / `settings-selector.ts` / `tree-selector.ts` 内部实现），
> 并逐一核对 `packages/coding-agent/test/` 与 `test/suite/regressions/` 中所有相关测试。
> **本节与前面任何章节冲突时以本节为准**；本节只写前文缺失、写错或含混的内容。

### A.1 覆盖证明（先给结论：文件无遗漏）

- `wc -l` 48 个文件求和 = **11,856**，与 §0.1 完全一致；无未列入的文件。
- 导出面用 `grep -n "^export "` 全量比对（结果见 §0.1 的「未进 barrel 的导出」表，已按源码更正，
  其中 `mermaid.ts` 的 `MermaidTransformerOptions` **不是** `export`）。
- 组件目录 `grep -rn "process.env"` → **恰好 4 行**（`extension-editor.ts:59,60` 的 `VISUAL`/`EDITOR`，
  `footer.ts:165` 与 `tree-selector.ts:949` 的 `HOME || USERPROFILE`），与 §4.5 一致，**没有别的环境变量**。
- `process.platform` 全量 **4 处**：`auth-url.ts:26`、`login-dialog.ts:152`、`keybinding-hints.ts:13`
  影响**用户可见文本**（`Cmd+click`/`Ctrl+click`、`option`）；`extension-editor.ts:61` 只影响子进程默认命令
  （`notepad` vs `nano`），不影响文案。
- 非确定性来源只有三处：`Math.random()`（仅 `armin.ts`，**10 处**：L76、L130、L145、L162、L173、L288、
  L338、L342、L348、L349）、`performance.now()`（仅 `easter-egg-3d.ts` **7 处**：L879、L880、L910、L926、
  L954、L1111、L1115；+ `radius-login-selector.ts` **2 处**：L47、L85）、`setInterval`/`setTimeout`
  （`armin`、`countdown-timer`、`radius-login-selector`、`easter-egg-3d`、`user-message-selector`、
  `extension-selector`/`extension-input` 经 `CountdownTimer`）。Rust 侧只需为这三类注入可测时钟/RNG。
- `toLocaleString()` 全单元**只有** `compaction-summary-message.ts:40` 一处；`toFixed()` **只有**
  `footer.ts` 5 处（`:27,:29,:162,:186,:194`）—— §6.3 的 JS 语义表覆盖完整。

### A.2 §2.45/§2.60.7 缺失的核心：3D 逐单元渲染循环（**最重要的遗漏**）

前文只写了 `emit` 与 `boxes`，**没有**给出 `render(width)` 里逐 cell 的决策树。以下是权威实现
（`easter-egg-3d.ts` `render` 尾部）：

```ts
const halo = (index: number, base: Rgb | undefined): Rgb | undefined => {
  const amount = this.raster.haloAmount[index]!;
  if (amount <= 0) return base;
  const color = this.raster.haloRgb;
  return mix(base ?? background, [color[index*3]!, color[index*3+1]!, color[index*3+2]!], amount);
};
const backgroundFade = smooth(dissolveTime / BACKGROUND_FADE);     // 0.5
const textFade = smooth(dissolveTime / 0.6) * 0.3;                 // 注意系数 0.3
const textActive = dissolveTime < DISSOLVE_END || backgroundFade < 1;
const hint = this.hint(width, height, hintAlpha, background);
const frame = Math.floor(dissolveTime * 14);                       // 干扰动帧号：14/秒
const now = this.elapsed();
const starLevel = Math.round(starAlpha * STAR_ALPHA_LEVELS) / STAR_ALPHA_LEVELS;
const starfield = (index: number) => {
  if (starLevel <= 0) return undefined;
  const star = this.stars[index];
  if (!star) return undefined;
  const flicker = Math.round((0.5 + 0.5 * Math.sin(now * star.speed + star.phase)) * STAR_FLICKER_LEVELS)
                / STAR_FLICKER_LEVELS;
  return { text: star.glyph,
           fg: mix(background, mix(foreground, star.tint, 0.5),
                   star.strength * (0.7 + 0.3 * flicker) * starLevel) };
};

const lines: string[] = [];
for (let row = 0; row < height; row++) {
  const cells = this.cells[row] ?? [];
  let line = "", currentFg = -1, currentBg = -1;
  const emit = (text, fg, bg) => {
    const fgKey = fg ? this.pack(fg) : -1;
    const bgKey = bg ? this.pack(bg) : -1;
    if (fgKey !== currentFg && text !== " ") {         // ← 空格既不发光标也不更新 currentFg
      line += fg ? this.ansi(fgKey, false) : "\x1b[39m";
      currentFg = fgKey;
    }
    if (bgKey !== currentBg) {                         // 背景对空格也发
      line += bg ? this.ansi(bgKey, true) : "\x1b[49m";
      currentBg = bgKey;
    }
    line += text;
  };
  for (let column = 0; column < width; column++) {
    const index = row * width + column;
    const cell = textActive ? cells[column] : undefined;
    const cellBg = halo(index,
      cell?.bg && backgroundFade < 1 ? mix(cell.bg, background, backgroundFade) : undefined);
    // 1) hint 覆盖一切（含 logo 与文字）
    if (hint && row === hint.row && column >= hint.start && column < hint.start + hint.text.length) {
      const offset = column - hint.start;
      emit(hint.text[offset]!, offset < hint.keyLength ? hint.keyColor : hint.color, cellBg);
      continue;
    }
    // 2) logo 点（raster 未清零前，本帧已写入的 cell 优先于原屏幕文字）
    const dots = this.raster.bits[index]!;
    if (dots) {
      const step = LOGO_COLOR_STEP * this.raster.counts[index]!;      // ← 按点数求平均再量化
      const rgb = this.raster.rgb;
      const color: Rgb = [Math.round(rgb[index*3]! / step) * LOGO_COLOR_STEP,
                          Math.round(rgb[index*3+1]! / step) * LOGO_COLOR_STEP,
                          Math.round(rgb[index*3+2]! / step) * LOGO_COLOR_STEP];
      emit(BRAILLE[dots]!, color, cellBg);
      continue;
    }
    // 3) 屏外/已淡出的 cell → 星空或空格
    if (!cell) {
      const star = starfield(index);
      emit(star?.text ?? " ", star?.fg, cellBg);
      continue;
    }
    // 4) 还没到消散时刻：原字符（宽字符整格保留，并跳过伴随列）
    const dust = (dissolveTime - cell.delay) / DUST_DURATION;
    if (dust < 0 && cell.width === 2 && !this.raster.bits[index + 1]) {
      emit(cell.text, mix(cell.fg, background, textFade), cellBg);
      column++;
      continue;
    }
    if (dust < 0 && cell.width === 1) {
      emit(cell.text, mix(cell.fg, background, textFade), cellBg);
      continue;
    }
    // 5) 变成盲文尘：点数从 ink 线性减到 0
    const dotCount = dust < 0 ? cell.ink : Math.round(cell.ink * (1 - clamp01(dust)));
    if (dotCount <= 0) {
      const star = starfield(index);
      emit(star?.text ?? " ", star?.fg, cellBg);
      continue;
    }
    let bits = 0, placed = 0;
    for (let attempt = 0; placed < dotCount && attempt < 32; attempt++) {
      const bit = DOT_BITS[Math.floor(hash(cell.seed * 7919 + frame * 131 + attempt) * 8)]!;
      if (bits & bit) continue;
      bits |= bit;
      placed++;
    }
    const fade = textFade + (1 - textFade) * clamp01(dust) ** 0.8;
    emit(BRAILLE[bits]!, mix(cell.fg, background, fade), cellBg);
  }
  lines.push(`${line}\x1b[0m`);           // 每行强制 SGR reset
}
```

必须照抄的细节：

- **`emit` 的前景/背景不对称**：`text === " "` 时**既不发光标也不更新 `currentFg`**（`currentFg` 会保持旧值，
  后面的非空格若 key 与旧值相同就**不会**重发）；背景只看 key，与 `text` 无关。
- **优先级**：hint > logo dots > 星空/空格（无 cell 时）> 原文字 > 尘。宽字符分支用 `column++`
  跳过伴随列，且**仅当右侧 dot 未点亮**（`!raster.bits[index+1]`）才整格保留，否则该格进入尘埃逻辑。
- `frame = floor(dissolveTime * 14)` 与 `hash(seed*7919 + frame*131 + attempt)` 决定「取哪些盲文点」，
  每秒约变 14 次（视觉上的闪烁）。
- `hint.text[offset]` 是 **UTF-16 code unit** 索引；`hint.text.length`/`key.length` 也是 UTF-16 长度
  （默认键名 `esc`/`escape` 为 ASCII，无差异；Rust 用 `Vec<char>` 时需在同为 ASCII 的前提下等价）。
- `textFade` 的系数是 **0.3**（`smooth(t/0.6) * 0.3`），`backgroundFade = smooth(t / 0.5)`。
- `textActive` 为 false 时 `cell` 恒为 undefined → 走星空分支（**即使 raster 有 logo 点也仍优先 logo**）。

### A.3 `BlockRaster` 的完整内部结构（前文只写了「每帧光栅缓冲」）

```ts
class BlockRaster {
  bits = new Uint8Array(0);        // 每 cell 点亮的盲文点（OR 累积）
  counts = new Uint8Array(0);      // 每 cell 点亮的点数
  rgb = new Float32Array(0);       // 每 cell 的 RGB **求和**（未平均）
  haloAmount = new Float32Array(0);
  haloRgb = new Float32Array(0);
  private depth = new Float32Array(0);        // 每个 dot 的最近命中 t
  private faceIds = new Uint16Array(0);       // 每个 dot 的 faceIndex + 1，0 = 无
  private dirty?: { minX; minY; maxX; maxY };      // 本帧写过的 cell 包围盒
  private haloDirty?: { minX; minY; maxX; maxY };
  private width = 0; private height = 0;
  private readonly cameraDistance: number;
}
```

- 尺寸：`dotWidth = width*2`、`dotHeight = height*4`（1 cell = 2×4 dots）。
  **尺寸变化才重新分配**所有缓冲（`depth` 用 `.fill(Infinity)`）；否则按 `dirty`/`haloDirty` 逐行清零
  （`bits.fill(0, row*width+minX, row*width+maxX+1)`，`rgb.fill(0, (row*width+minX)*3, (row*width+maxX+1)*3)`）。
- `origin = [m[6]*cameraDistance, m[7]*cameraDistance, m[8]*cameraDistance]`（相机在相机空间 `(0,0,d)`）。
- `faces = visibleFaces(...)`；`faces.length === 0` 直接 `return`（**不清 dirty**，下一帧继续用旧的 dirty 清零，
  因此不会残留）。
- 面包围盒取所有面的 dot 坐标并集；`maxX < minX || maxY < minY` → return。
- **逐面射线步进**（`a = face.axis`、`u=(a+1)%3`、`v=(a+2)%3`）：
  ```ts
  const stepA = m[a] / scale, stepU = m[u] / scale, stepV = m[v] / scale;
  const sx = (face.minX + 0.5 - centerX) / scale;
  for (let dotY = face.minY; dotY <= face.maxY; dotY++) {
    const sy = (dotY + 0.5 - centerY) / scale;
    let directionA = m[a]*sx + m[3+a]*sy - m[6+a]*cameraDistance;    // U/V 同构
    let index = dotY * dotWidth + face.minX;
    for (let dotX = face.minX; dotX <= face.maxX; dotX++, index++) {
      const t = (face.plane - originA) / directionA;
      if (t > 0 && t < depth[index]) {
        const hitU = originU + t*directionU, hitV = originV + t*directionV;
        if (hitU >= face.uMin && hitU <= face.uMax && hitV >= face.vMin && hitV <= face.vMax) {
          depth[index] = t; faceIds[index] = faceIndex + 1;
        }
      }
      directionA += stepA; directionU += stepU; directionV += stepV;
    }
  }
  ```
  **步进是加法**（不是每点重算乘法）——浮点误差路径必须一致。
- **着色并打包成盲文**（再遍历一次包围盒）：
  ```ts
  const id = faceIds[index]; if (id === 0) continue;
  const face = faces[id - 1];
  const fog = clamp01(0.15 - cameraDistance * (1 - depth[index]) * 0.12) * (light ? 0.5 : 1);
  faceIds[index] = 0; depth[index] = Infinity;          // 边用边清
  const cellX = dotX >> 1, cellY = dotY >> 2, cell = cellY * width + cellX;
  bits[cell] |= DOT_BITS[(dotY & 3) * 2 + (dotX & 1)];
  counts[cell]++;
  rgb[cell*3 + k] += face.k + (background[k] - face.k) * fog;
  ```
  记录 cell 包围盒；`if (cellMaxX >= 0) this.dirty = {...}`；`if (haloStrength > 0) this.renderHalo(strength)`。
- `face` 对象字段：`{ axis, plane, uMin, uMax, vMin, vMax, minX, minY, maxX, maxY, red, green, blue }`
  （前文只写了包围盒与颜色）。
- `renderHalo(strength)`（**只有 light 背景会传非 0**）：
  - 区域 `minX = max(0, dirty.minX - 6)`、`maxX = min(width-1, dirty.maxX + 6)`、
    `minY = max(0, dirty.minY - 3)`、`maxY = min(height-1, dirty.maxY + 3)`。
  - `tent(radius)`：权重 `radius + 1 - |i - radius|`，再除以权重和（归一化）。
  - 水平 pass：只遍历 `y ∈ [dirty.minY, dirty.maxY]`，`x ∈ [minX, maxX]`，源列限制在
    `[dirty.minX, dirty.maxX]`，权重 `weightsX[dx+6] / 8`；写 `horizontal[(y-minY)*regionWidth + (x-minX)]` 的
    `coverage, red, green, blue`（长度为 `rows*regionWidth*4` 的 `Float32Array`）。
  - 垂直 pass：`y ∈ [minY,maxY]`、`x ∈ [minX,maxX]`，源行限制在 `[dirty.minY, dirty.maxY]`，权重 `weightsY[dy+3]`。
  - 自身覆盖兜底：`const count = counts[target]; if (count > 0 && count/8 > coverage) { coverage = count/8;
    red = (rgb[target*3]/count)*coverage; ... }`。
  - `if (coverage <= 0) continue; const amount = Math.round(strength * Math.min(1,coverage)**0.7 * HALO_LEVELS)
    / HALO_LEVELS; if (amount <= 0) continue;` 然后写 `haloAmount[target]`、`haloRgb[target*3+k] =
    Math.round(red/coverage/LOGO_COLOR_STEP) * LOGO_COLOR_STEP`。
  - `haloDirty = {minX,minY,maxX,maxY}`（**是裁剪后的区域**，下一帧据此清零）。

### A.4 `rotation` / `multiply`（前文完全没有，但决定所有投影坐标）

```ts
function rotation(yaw: number, pitch: number, roll: number): number[] {   // 行主序 3x3
  const [sy, cy, sx, cx, sz, cz] = [Math.sin(yaw), Math.cos(yaw), Math.sin(pitch),
                                    Math.cos(pitch), Math.sin(roll), Math.cos(roll)];
  const ry = [cy, 0, sy,  0, 1, 0,  -sy, 0, cy];
  const rx = [1, 0, 0,  0, cx, -sx,  0, sx, cx];
  const rz = [cz, -sz, 0,  sz, cz, 0,  0, 0, 1];
  return multiply(rz, multiply(rx, ry));        // Rz * Rx * Ry
}
function multiply(a: number[], b: number[]): number[] {
  const result = new Array<number>(9);
  for (let row = 0; row < 3; row++)
    for (let column = 0; column < 3; column++)
      result[row*3 + column] = a[row*3]*b[column] + a[row*3+1]*b[3+column] + a[row*3+2]*b[6+column];
  return result;
}
```

- 用法是**行主序矩阵**：第 `a` 列（`m[a]`、`m[3+a]`、`m[6+a]`）是物体轴 `a` 在相机空间的方向
  （法线）；`m[6],m[7],m[8]` 是矩阵第三行（相机朝向的反向）用于 `origin`。
- 角点投影：`x = m[0]*cx + m[1]*cy + m[2]*cz`、`y = m[3]*cx + m[4]*cy + m[5]*cz`、
  `z = m[6]*cx + m[7]*cy + m[8]*cz`；`perspective = pose.scale * cameraDistance / (cameraDistance - z)`；
  `screenX = pose.centerX + x*perspective`、`screenY = pose.centerY + y*perspective`。
- Rust 必须用 `[[f64;3];3]` 或扁平 `[f64;9]` 并保持**完全相同的乘加顺序**（否则射线交界处
  0.5 dot 的 `floor/ceil` 会翻转）。

### A.5 `config-selector.ts` 补遗（§2.52.5 未覆盖的部分）

- **`ResourceItem` 的 `groupKey`/`subgroupKey` 只写不读**；`ResourceGroup.key` 除插入 `Map` 外也不读。
  Rust 侧可省略（不影响行为），但若逐字段建模需保留以对齐调试输出。
- **`setProjectTopLevelPaths(resourceType, paths)`** 是 `if/else if` 链：
  `extensions → setProjectExtensionPaths`、`skills → setProjectSkillPaths`、
  `prompts → setProjectPromptTemplatePaths`、**`else → setProjectThemePaths`**（没有 `themes` 分支判断，
  是兜底）。§2.52.5 的 `toggleTopLevelResource` 已给出同一映射的全局版本。
- **`togglePackageResource`（全局模式）**：用 `typeof pkg === "string" ? pkg : pkg.source` 与
  **`item.metadata.source` 精确字符串相等**匹配（**不是** `packageSourceStringMatches` —— 那是 project
  模式才用的路径归一化比较）；`pkgIndex === -1` 时**直接 return**，不写任何设置。
- **`getProjectOverrideState` 对 package 项**：`findMatchingPackageSource(item, "project")` 返回
  **字符串**（`typeof pkg !== "object"`）时立刻 `"inherit"` —— 也就是说**字符串形式的 package 在 project
  视图里永远显示为 inherit**（因为没有按资源类型的数组可读）。这是用户可见行为（checkbox 显示 `[x]/[ ]`
  而不是 `[+]/[-]`）。
- **`getOverrideStateFromEntries` 的 `emptyArrayIsUnload`**：顶层传 `false`，package 传
  `pkg.autoload !== false`。即「package 被显式写了空数组且未 `autoload:false`」→ unload。
- **多条命中时后写覆盖前写**（循环不 break）：`!pattern`/`-pattern` → unload，裸 pattern/`+pattern` → load。
- **`setProjectPackageOverride` 的过滤**只比较 `getPatternEntryTarget(entry) !== pattern`（单条 pattern），
  **不用** patterns 集合；与 `setProjectTopLevelOverride` 的集合过滤不同。
- **`createPackageOverrideSource`**：非本地路径 → `{ source, autoload: false }`；本地路径 →
  `{ source: relative(getTopLevelBaseDir("project"), resolvePath(source, getTopLevelBaseDir(itemScope),
  { trim: true })) || ".", autoload: false }`（结果可能为空串，此时用 `"."`）。
- **`packageSourceStringMatches(leftSource, leftScope, rightSource, rightScope)`**：字符串相等 → true；
  否则两者都必须 `isLocalPath`，再各自 `resolvePath(..., getTopLevelBaseDir(scope), { trim: true })` 比绝对值。
- **`buildInheritedEnabledMap(groups)` 是构造时快照**：之后 project 侧切换**不会**更新它；
  而 `updateItem` 改的是 `item.enabled`。因此 `getInheritedEnabled` 在切换后仍返回**构造时**的全局值 ——
  Rust 侧若用 `Rc<RefCell>` 共享条目会改变行为，必须决定是否照抄（推荐照抄，保持对拍）。
- **`updateItem` 只更新第一份匹配**（`i.path === item.path && i.resourceType === item.resourceType`）后即
  `return`；若同一路径在两个组里各有一份，第二份不会更新。
- **`ResourceList` 的 `groups` getter**：`return this.groupsByScope[this.writeScope]` —— `buildFlatList`/
  `filterItems` 都经它取当前 scope。
- **`setWriteScope` 会重置选中**：`buildFlatList()` 内部把 `selectedIndex` 设为第一个 item，再被
  `filterItems(...)` 的 `selectFirstItem()` 覆盖。切 scope 后光标总回到第一项。
- **【逐字】** 新增确认：`Project Local Resources`、`Global Resources`、` · `、`switch mode`、
  `cycle inherit/+/-`、`toggle`、`close`、`.kiana/settings.json · inherited global resources are dimmed`、
  `~/.kiana/agent/settings.json`、`User (`、`)`、`Project (`、`Built-in`、`Built-in (project override)`、
  `User settings`、`Project settings`、` (project override)`。

### A.6 `armin.ts` 逐帧细节补遗（§2.44/§2.60.6 的边界）

- **`tickTypewriter`**：每帧循环 3 次，**循环体内**先算 `row = floor(pos/31)`、`x = pos % 31`，
  `row >= 18` 立即 `return true` —— 最后一帧可能只画 1~2 个 cell 后提前结束。
- **`tickScanline`**：先判 `state.row >= DISPLAY_HEIGHT → true`，否则复制整行再 `row++`。
- **`tickRain`**：每帧开头 `currentGrid = createEmptyGrid()`（**整屏重置**）；先按 `settled` 从底部向上重画；
  `settled >= DISPLAY_HEIGHT` 的列 `continue`（但仍算 `allSettled` 的真）；目标行搜索范围是
  `[DISPLAY_HEIGHT-1-settled, 0]` 内的最低非空格；`drop.y++` 后只有 `0 <= y < DISPLAY_HEIGHT` 才处理；
  未落定且 `y < targetRow` → 该格画 `"▓"`。落定时 `settled = DISPLAY_HEIGHT - targetRow`、
  `y = -floor(random*5) - 1`（**该列下次要重新下落**）。
- **`tickGlitch`**：每行**先无条件**取 `offset = floor(random*7) - 3`（消耗一次随机数），
  再 `random() < 0.3` 决定水平位移（`slice(offset).concat(slice(0, offset)).slice(0, 31)`，
  负 offset 即从尾部取，**照抄**）；不命中时再 `random() < 0.2` 决定整行替换
  （`finalGrid[floor(random*18)]` 的拷贝，消耗第三次随机数）；否则原行。`phase++` 后返回 false；
  `phase >= 8` 的**下一次**调用返回干净网格 + true。
- **`tickFade` / `tickDissolve`**：`for (i = 0; i < N; i++)` 内先检查 `idx >= positions.length → return true`，
  所以最后一帧可能不足 N 个。
- **`tickCrt`**：`expansion++` 后返回 `expansion > 18`（第 19 帧结束）。
- **构造顺序固定**：`effect = EFFECTS[floor(random*7)]` → `finalGrid = buildFinalGrid()` →
  `currentGrid = createEmptyGrid()` → `initEffect()`（`dissolve` 会把 `currentGrid` **替换**成随机噪声网格）→
  `startAnimation()`。`Math.random()` 的消耗次数因此依赖 effect 选择，Rust 对拍需同一 RNG 序列。
- **`invalidate()` 只置 `cachedWidth = 0`**，`cachedVersion` 保留 —— 若 `width === 0` 且 `gridVersion` 未变
  （即 `cachedVersion === gridVersion` 且 `cachedWidth === 0`），仍会命中缓存并返回旧数组。
- 半块字符 `getChar(x,row)`：`upper && lower → "█"`、`upper → "▀"`、`lower → "▄"`、否则 `" "`。

### A.7 测试清单补遗（宿主层；§7.1 只列「直接覆盖 components/**」的文件）

这些测试**不在** `components/**` 的覆盖统计里，但会驱动本单元组件，审计时已逐一核对数量：

| 文件 | 用例数 | 与本单元的关系 |
|---|---|---|
| `external-editor.test.ts` | 3 | `external-editor.ts`（宿主层，§0.2 已声明范围外）；覆盖 `VISUAL`/`EDITOR` 与临时文件 |
| `interactive-mode-extension-dialogs.test.ts` | 4 | `ExtensionSelector`/`ExtensionInput`/`ui.custom` 的层叠与 signal 中止（§7.1 已列） |
| `interactive-mode-startup-input.test.ts` | 3 | 启动输入（不直接触碰组件） |
| `interactive-mode-suspend.test.ts` | 3 | 挂起/恢复（`tui.stop()/start()` 路径） |
| `interactive-mode-anthropic-warning.test.ts` | 4 | `settings-selector` 的 `anthropic-extra-usage` 开关文案链路 |
| `interactive-mode-assistant-diagnostics.test.ts` | 2 | `AssistantMessageComponent` 的错误/诊断渲染 |
| `interactive-mode-bug-report-hint.test.ts` | 5 | 崩溃提示（`ExtensionEditor`/`ExtensionSelector` 的 `/bug` 流程） |
| `interactive-mode-clone-command.test.ts` | 2 | `/clone` 选择器路径 |
| `interactive-mode-compaction.test.ts` | 6 | compaction 消息与状态指示 |
| `interactive-mode-import-command.test.ts` | 6 | `/import` 选择器路径 |
| `program-status-reporter.test.ts` | 7 | 状态行（宿主） |
| `keybindings.test.ts` | 5 | `core/keybindings.ts`（本单元只消费） |
| `keybindings-migration.test.ts` | 3 | 同上（旧键名迁移） |

- **`radius.test.ts` 6 项**只测 provider 层（模型目录），不渲染 `RadiusLoginMenuComponent`。
- **`footer-data-provider.test.ts` 8 项**测 `FooterDataProvider`（非本单元），`footer-width.test.ts` 11 项才是
  `FooterComponent`。
- **`model-search.test.ts` 5 项**测 `getDistinctModelName`（`model-selector` 的依赖）。
- 审计复核后 §7.1/§7.2 的用例数**全部正确**（含 `output-pad.test.ts` 的 1×5、`tool-execution-component` 26 声明/
  43 运行时、`login-dialog-device-code` 7/9、`interactive-mode-status` 36、`interactive-tui` 11/16、`7497` 3）。

### A.8 前文未给出、但属于外部格式的写入细节（补充 §4.2）

`config-selector` 写 settings 时的完整 setter 映射（Rust 侧必须存在同名能力，**不是**直接改 JSON）：

| scope | extensions | skills | prompts | themes |
|---|---|---|---|---|
| user | `setExtensionPaths` | `setSkillPaths` | `setPromptTemplatePaths` | `setThemePaths` |
| project | `setProjectExtensionPaths` | `setProjectSkillPaths` | `setProjectPromptTemplatePaths` | `setProjectThemePaths` |

- 顶层 `toggleTopLevelResource` 用 `if/else if`（themes 兜底），project 的 `setProjectTopLevelPaths` 同样。
- `setProjectPackages` / `setPackages` 用于 package 过滤数组。
- **没有任何显式 `save()` 调用**：落盘由 `SettingsManager` 的 setter 负责（ca-config 单元）。

### A.8b `settings-selector.ts` 的构造顺序细节（§2.40 的表格是「最终顺序」，但插入是**级联 splice**）

源码不是一次静态数组，而是「基础数组 + 5 次级联 `splice`」；Rust 侧必须复刻同一插入算法（否则
`supportsImages === false` 时索引全错）：

```ts
const items: SettingItem[] = [autocompact, steering-mode, follow-up-mode, transport, share-destination,
  http-idle-timeout, cache-warming-mode, hide-thinking, mermaid-rendering, cache-miss-notices,
  collapse-changelog, quiet-startup, install-telemetry, default-project-trust, double-escape-action,
  tree-filter-mode, warnings, model-thinking, tui-mode, fullscreen-exit-output, fullscreen-scrollbar,
  fullscreen-copy-on-select, fullscreen-wheel-scroll-lines, theme];        // 24 项基础
if (supportsImages) {
  items.splice(1, 0, show-images);
  items.splice(2, 0, image-width-cells);
}
items.splice(supportsImages ? 3 : 1, 0, auto-resize-images);
items.splice(items.findIndex(i => i.id === "auto-resize-images") + 1, 0, block-images);
items.splice(items.findIndex(i => i.id === "block-images") + 1, 0, skill-commands);
items.splice(items.findIndex(i => i.id === "skill-commands") + 1, 0, show-hardware-cursor);
items.splice(items.findIndex(i => i.id === "show-hardware-cursor") + 1, 0, editor-padding);
items.splice(items.findIndex(i => i.id === "editor-padding") + 1, 0, output-padding);
items.splice(items.findIndex(i => i.id === "output-padding") + 1, 0, autocomplete-max-visible);
items.splice(items.findIndex(i => i.id === "autocomplete-max-visible") + 1, 0, clear-on-shrink);
items.splice(items.findIndex(i => i.id === "clear-on-shrink") + 1, 0, terminal-progress);
```

- 只有前两处用**硬编码下标**（`1`、`2`、`3`/`1`），其余 8 处都是 `findIndex(...) + 1` 级联。
- `SettingsList` 的 `maxVisible` 是**硬编码 `10`**（不是 `Math.min(items.length, 10)` —— 那是
  `WarningSettingsSubmenu`/`ThemeSubmenu` 的写法）。
- `onChange` 的 `switch` **没有 `default`**（未知 id 静默忽略）；`warnings`/`model-thinking`/`theme`
  三项**不在** `onChange` 里（它们是 `submenu`，由子菜单自己调 `callbacks`）。
- `getSettingsList(): SettingsList` 是公开方法。
- `followUpKey = keyDisplayText("app.message.followUp")`、`cycleThinkingKey = keyDisplayText("app.thinking.cycle")`
  在**构造开头**求值并嵌入 description（键位自定义会反映到文案）。
- `currentWarnings = { ...config.warnings }`、`currentModelThinkingLevels = { ...config.modelThinkingLevels }`
  是**构造内的可变副本**（子菜单改动会累积并用于 summary），但**不会**回写到 `config`。
- `defaultModelByValue: Map<key, Model>`、`currentDefaultModelKey`（`defaultModel` 不在 map 里则为 undefined）、
  `currentModelKey`（`config.currentModel ? modelSettingKey(...) : undefined`）。
- **【逐字】** 新增确认：`Enable or disable individual warnings`、`Override the default thinking level for
  specific models. `（末尾空格，后接 `{cycleKey} cycles in-session.`）、`Color theme for the interface`、
  `Save and go back`、`Switch to one theme for light and dark`、`switch to single theme`、`save and go back`、
  `Per-Model Thinking Level`、`Select a model to configure`、`Thinking Level for `、
  `Select default thinking level for this model`、`No models available`、
  `Log in to a provider or configure an API key first`、`  (clear override)`、`Revert to global default (`、
  `Automatic Theme`、`Light theme`、`Dark theme`、`Light Theme`、`Dark Theme`、
  `Select the theme to use for light terminal appearance`、`Select the theme to use for dark terminal appearance`、
  `Theme to use in automatic mode when the terminal is light`、
  `Theme to use in automatic mode when the terminal is dark`、
  `Theme created from your terminal's colors`、`Use separate themes for light and dark terminal appearance`、
  `Choose themes for terminal light and dark appearance.`、`Light/dark detection requires terminal support.`。

### A.9 仍无法从源码确定 / 需要在 Rust 侧决策的事项

1. `grok-mermaid` 不在仓库依赖中（§8.5 Q3 已有），`MermaidArt` 的 `width/styled/plain/warnings` 需自研等价实现。
2. `theme.appearance`、`theme.colors.*`、`tui.queryTerminalColors` 的**调用时序**（3D 彩蛋在 await 之后才读
   `theme`）—— 主题切换时的竞态需在宿主层保证。
3. `Intl.Segmenter` 的 grapheme 切分与 `visibleWidth` 的宽字符判定必须由 tui-text 单元先对齐（本单元只消费）。
4. `Diff.diffWords` 的 jsdiff 打分/LCS 细节（§6.3 已标「需自研」）—— 本单元只提供 1 条调用路径。
5. `getProjectTrustOptions` 的 `includeSessionOnly` 分支属 ca-cli/core；本单元只消费其输出（§2.54.9 已说明）。

### A.10 本轮审计的新增/更正计数（供流水线核对）

按小节统计的**新增条目**（每条 = 一个此前缺失的签名/分支/字面量/测试/注意事项）：

| 小节 | 新增 |
|---|---|
| A.1 覆盖证明 | 4 |
| A.2 3D 逐单元渲染循环 | 9 |
| A.3 `BlockRaster` 内部 | 6 |
| A.4 `rotation`/`multiply` | 2 |
| A.5 `config-selector` 补遗 | 10 |
| A.6 `armin` 逐帧细节 | 6 |
| A.7 测试清单补遗 | 13（测试文件） |
| A.8 settings setter 映射 | 2 |
| A.8b settings 级联插入 | 6 |
| 就地补写（§0.1 导出表 8 行、§2.52.5 补 9 个 helper 体、§4.8 重写 5 条、§2.54.11/开放问题 2 条） | 24 |
| **新增合计** | **82** |

**就地更正的错误陈述：47 处**，分布：
§0.1 导出表（`mermaid.ts` 的 `MermaidTransformerOptions` 非导出、新增 8 行）、§2.8、§2.17、§2.19、§2.20、
§2.21、§2.23、§2.24、§2.25、§2.26、§2.27、§2.28、§2.30、§2.31、§2.32、§2.33、§2.35、§2.38、§2.39、§2.40、
§2.41/§2.52.5（`toggleTopLevelResource` 的 setter 映射与 missing helpers）、§2.52.1（9→8 参数）、
§2.52.6、§2.54.11（`UserMessageComponent.invalidate` 实际不存在）、§4.8（`ExtensionUIDialogOptions` 字段、
`ui.editor` 签名、`ui.custom` 4 参）、§7.1/§7.2 用例数、附录 A。

> 审计结论：**文件覆盖完整（48/48 行数与 §0.1 一致）**，但首轮 §2.1–§2.50 的错误率约为 **1/3 的段落**
> 含至少一条错误陈述；§2.51–§2.60 的复核质量明显更高，仍发现 4 处错误（§2.52.5 的 helper 缺失、
> §2.54.11 的 `invalidate` 断言、§2.60.7 缺的逐 cell 循环与 `BlockRaster` 内部、§2.60.9 的 8 参笔误已修）。
> 移植时**以 §2.5x 与本节为准**。
