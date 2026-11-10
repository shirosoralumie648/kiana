# `ca-config-models` 单元移植规格（TypeScript → Rust）

本文件是 `@earendil-works/kiana-coding-agent` 中 **ca-config-models** 单元（设置分层与持久化、模型运行时 / 模型配置 / 模型解析、凭证存储、键位绑定、项目信任、缓存预热与缓存统计、用量汇总、HTTP dispatcher、客户端身份与遥测开关）的完整移植规格，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`ca-config-models`。
参考实现（只读，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。
> Rust 实现必须输出完全一致的字节序列（除另有说明）。中文用于解释语义。
> 凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B。
> 文中 `APP_NAME` / `APP_TITLE` / `CONFIG_DIR_NAME` / `VERSION` / `PACKAGE_NAME` 是运行期常量（见 `ca-cli` 单元 §2.6），
> 默认值分别为 `kiana` / `π` / `.kiana` / `1.4.0`（取自 `packages/coding-agent/package.json`）/ `@earendil-works/kiana-coding-agent`。
> `ENV_AGENT_DIR` = `KIANA_CODING_AGENT_DIR`，`ENV_SESSION_DIR` = `KIANA_CODING_AGENT_SESSION_DIR`。
> 路径常量：`<agent-dir>` 默认 `~/.kiana/agent`，由 `getAgentDir()` 决定（`KIANA_CODING_AGENT_DIR` 覆盖，`~` 展开）。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

`packages/coding-agent/src/core/` 下 30 个文件，共 **8089 行**：

```
settings-manager.ts            (1443 行)  SettingsManager / FileSettingsStorage / 迁移 / 工具选择
provider-composer.ts           ( 737 行)  builtin + models.json + extension 三层 provider 合成
model-resolver.ts              ( 689 行)  模型引用解析 / scope / CLI 解析 / 初始模型 / 会话恢复
auth-storage.ts                ( 507 行)  auth.json 存储（同步/异步锁、只读、内存）
settings-schema.ts             ( 482 行)  TypeBox SettingsSchema（生成 settings.schema.json 的来源）
cache-warmer.ts                ( 453 行)  prompt cache 预热状态机与格式化
keybindings.ts                 ( 359 行)  APP_KEYBINDINGS / KEYBINDINGS / 迁移 / KeybindingsManager
resolve-config-value.ts        ( 287 行)  $ENV / !command / 字面值解析
trust-manager.ts               ( 246 行)  trust.json 存储与 trust 需求检测
model-runtime.ts               (1049 行)  ModelRuntime（implements kiana-ai Models）
model-registry.ts              ( 244 行)  ModelRegistry 同步兼容外观
remote-catalog-provider.ts     ( 240 行)  kiana.dev 目录 overlay（withRemoteCatalog / withPersonalizedCatalog）
virtual-models.ts              ( 238 行)  虚拟模型（路由）目录条目与分支选择
model-config.ts                ( 219 行)  models.json（JSONC）加载与校验
cache-stats.ts                 ( 174 行)  cache miss 统计
models-store.ts                ( 147 行)  models-store.json 持久化
http-dispatcher.ts             ( 113 行)  undici dispatcher 配置与 httpProxy
usage-totals.ts                (  99 行)  Usage 汇总与成本分桶
project-trust.ts               (  97 行)  启动期项目信任决策
settings-defaults.ts           (  67 行)  SETTINGS_DEFAULTS
runtime-credentials.ts         (  52 行)  运行时 API key overlay
settings-diagnostics.ts        (  25 行)  设置诊断
auth-guidance.ts               (  25 行)  /login 引导文案
keybindings-schema.ts          (  21 行)  KeybindingsSchema（生成 keybindings.schema.json 的来源）
key-id-schema.ts               (  21 行)  KeyIdSchema（按键语法正则）
telemetry.ts                   (  13 行)  isInstallTelemetryEnabled
defaults.ts                    (  12 行)  DEFAULT_THINKING_LEVEL / THINKING_LEVEL_OPTIONS
radius.ts                      (  11 行)  Radius 常量与 gateway URL
experimental.ts                (   3 行)  areExperimentalFeaturesEnabled
client-identity.ts             (  16 行)  getClientIdentity
```

另有四个 **本单元所属的脚本**（不在 `src/core/` 下，但同属本单元，规范见 §2.31 / §2.32 / §A.1）：

```
packages/coding-agent/scripts/generate-schemas.ts    ( 163 行)  生成 4 个 JSON Schema（renderConfigSchemas）
packages/coding-agent/scripts/generate-references.ts ( 177 行)  生成 docs/models-configuration.md（renderReferences）
scripts/model-catalog-protocol.ts                    ( 260 行)  kiana/kiana.dev 共享的目录协议（parseModelCatalogRequest / selectModelCatalog / parseModelCatalogIndex ...）—— **仓库根目录**，不在 packages/coding-agent 下
scripts/model-catalog-protocol.test.ts               ( 120 行)  上述协议的 node:test 用例（7 条），由根 package.json 的 `npm run test:scripts` 执行
```

### 0.2 同仓配套资源（本单元负责生成 / 校验，但内容属于其他单元）

| 路径 | 归属 | 本单元职责 |
|---|---|---|
| `packages/coding-agent/schemas/models.schema.json` (44.9 KB) | 本单元（来源 `ModelsConfigSchema`） | 生成 + 校验 |
| `packages/coding-agent/schemas/settings.schema.json` (19.4 KB) | 本单元（来源 `SettingsSchema`） | 生成 + 校验 |
| `packages/coding-agent/schemas/keybindings.schema.json` (12.5 KB) | 本单元（来源 `KeybindingsSchema`） | 生成 + 校验 |
| `packages/coding-agent/schemas/theme.schema.json` (11.1 KB) | `theme` 单元（`ThemeJsonSchema`） | **仅**由本单元的 `scripts/generate-schemas.ts` 一起生成（顺序固定第 4 个） |
| `packages/coding-agent/scripts/generate-schemas.ts` | 本单元 | 生成上述 4 个 schema |
| `packages/coding-agent/scripts/generate-references.ts` | 本单元（`ModelsJsonSchemas` 是其唯一数据源之一） | 生成 `docs/models-configuration.md` |
| `scripts/model-catalog-protocol.ts` + `scripts/model-catalog-protocol.test.ts` | 本单元（**仓库根 `scripts/`**；源文件注释要求与 kiana.dev 的 `src/shared/models/protocol.ts` 逐字节相同） | 协议实现 + 7 条 node:test；由根 `package.json` 的 `"test:scripts": "node --test scripts/*.test.mjs scripts/*.test.ts"` 执行（`npm test` = `npm run test:scripts && npm run test --workspaces --if-present`） |
| `packages/coding-agent/docs/{settings,models,models-configuration,providers,keybindings,virtual-models,custom-provider,security,configuration,environment-variables}.md` | 文档 | 行为说明的权威来源，须逐字节保留 |

### 0.3 明确**不在**本单元范围（但被直接调用）

| 单元 id | 依赖内容 |
|---|---|
| `ai-core` | `Models` / `Provider` / `createModels` / `createProvider` / `ModelsError` / `lazyStream` / `normalizeContext` / `getSupportedThinkingLevels` / `clampThinkingLevel` / `modelsAreEqual` / `ModelsStore` / `ModelsRefreshResult` / `ModelsPublication` / `RefreshModelsContext` / `Model` / `AnyModel` / `Usage` / `calculateCost` / `formatUserAgent` / `KIANA_ATTRIBUTION` / `getOpenCodeSessionHeaders` / `getProviderEnvValue` / `ProviderCompatSchema` / `ModelCostSchema` / `ModelInputLimitsSchema` / `ModelPromptCacheSchema` / `ThinkingLevelMapSchema` / `ModelThinkingLevelSchema` / `DEFAULT_MAX_AGENT_RETRY_DELAY_MS` / `builtinProviders()` / `getBuiltinModelDataGeneratedAt()` / `radiusProvider()` / `DEFAULT_RADIUS_GATEWAY` / `normalizeRadiusGatewayUrl` / `personalizeOpenRouterCatalog` / `getApiProvider` |
| `ai-auth` | `CredentialStore` / `Credential` / `CredentialInfo` / `AuthCheck` / `AuthResult` / `AuthType` / `AuthInteraction` / `AuthOperationOptions` / `LoginOptions` / `ApiKeyAuth` / `OAuthAuth` / `OAuthCredentials` / `OAuthLoginCallbacks` / `ModelAuth` |
| `tui-core` | `Key` 枚举 / `KeyId` / `Keybinding` / `KeybindingDefinitions` / `KeybindingsConfig` / `TUI_KEYBINDINGS` / `KeybindingsManager`（基类） / `ScrollViewScrollbar` / `WheelScrollLines` / `TerminalCapabilities` |
| `ca-cli` | `VERSION` / `APP_NAME` / `APP_TITLE` / `CONFIG_DIR_NAME` / `getAgentDir` / `getDocsPath` / `getModelsPath` / `getAuthPath` / `getSettingsPath` / `getSessionsDir` / `normalizePath` / `resolvePath` / `canonicalizePath` / `getFileRevision` / `stripBom` / `stripJsonComments` / `fetchWithRetry` / `getKianaUserAgent` / `operationSignal` / `raceWithAbortSignal` / `getShellConfig` / `isValidThinkingLevel` / `runMigrations` |
| `ca-session` | `SessionEntry`（`message` / `model_change` / `usage` / `compaction` / `branch_summary` / `custom`） / `UsageEntry` / `SessionManager.appendUsage` / `SessionManager.getBranch` |
| `extensions` | `emitProjectTrustEvent` / `LoadExtensionsResult` / `ProjectTrustContext` / `ProjectTrustEventResult` |
| `agent-core` | `ThinkingLevel` / `Message` / `AgentMessage` |
| `theme` | `ThemeJsonSchema` / `ColorValueSchema`（仅 `generate-schemas.ts` 引用） |

### 0.4 关键不变量（必须逐条在 Rust 中成立）

1. `SettingsManager` 的所有**读**路径都不写盘；只有 setter 触发 `save()`，且写入串行（`writeQueue`）。
2. 保存时只写"本会话显式修改过的字段"（含嵌套字段），外部对其它字段的编辑必须被保留（`settings-manager-bug.test.ts` 锁定）。
3. 项目设置未受信时**读**得到 `{}`，**写**抛 `Project is not trusted; refusing to write project settings`。
4. `ModelRuntime` 的读访问（`getModels` / `getModel` / `getProviders`）是**同步**的，返回最后一次快照；网络只在 `refresh()` 发生。
5. provider 合成失败不抛出到 API 层：记录到 `compositionErrors` 并回退到 base provider（除 `registerProvider` 的主动校验会抛）。
6. 凭证操作按 **provider** 串行化；不同 provider 可并发；commit 后的本地同步失败必须抛 `CredentialSynchronizationError`。
7. 所有 auth.json / models-store.json / trust.json 写入都必须持锁；锁失败时**不得写入**（不得覆盖 malformed 文件）。
8. 所有 `:thinking` 后缀与 provider 前缀解析必须与 `model-resolver.test.ts` 的 40+ 用例逐条一致。
9. 键位默认值随平台（win32 / WSL / darwin / 其它）变化，`useWindowsKeybindings` 是唯一判定点。
10. `defaultModelPerProvider` 中的 `{{SECRET_*}}` 字符串是**本 checkout 中的脱敏占位**，必须逐字保留（测试断言字面值），不得替换或删除。

---

## 1. 概览

### 1.1 目的

本单元是 kiana 的**配置与模型管理层**：它把"用户在磁盘上写了什么"（settings.json / models.json / auth.json / keybindings.json / trust.json / models-store.json）变成运行期可用的设置对象、模型目录、凭证与按键绑定，并向上层（TUI、CLI、agent-session、extensions）提供稳定的读接口与流式请求入口。

### 1.2 在 monorepo 中的位置

- 被 `coding-agent` 的 `src/main.ts`、`src/modes/**`、`src/core/agent-session.ts`、`src/core/sdk.ts`、扩展 API（`ctx.modelRegistry`）广泛调用。
- 本单元是 `@earendil-works/kiana-ai` 的 `Models` 接口在 coding-agent 侧的唯一实现（`ModelRuntime implements Models`），`ModelRegistry` 是给扩展的同步轻量外观。
- 本单元**不**实现 wire protocol（`ai-api-*`）、OAuth 流程（`ai-auth`）、TUI 渲染（`tui-*`）、CLI 参数（`ca-cli`）。

### 1.3 入口点

| 导出来源 | 说明 |
|---|---|
| `core/settings-manager.ts` → `SettingsManager` | 全进程唯一设置对象；`create()` / `inMemory()` / `fromStorage()` |
| `core/model-runtime.ts` → `ModelRuntime.create()` | 异步构造，返回带快照的运行时 |
| `core/model-registry.ts` → `ModelRegistry` | 扩展 API 使用的同步外观 |
| `core/auth-storage.ts` → `AuthStorage` | `create()` / `inMemory()` / `fromStorage()` / `readStoredCredential()` |
| `core/keybindings.ts` → `KeybindingsManager.create()` | `/reload` 时 `reload()` |
| `core/trust-manager.ts` → `ProjectTrustStore` | `get` / `getEntry` / `set` / `setMany` |
| `core/project-trust.ts` → `resolveProjectTrusted()` | 启动期信任决策 |
| `core/cache-warmer.ts` → `CacheWarmer` | 由 agent 循环驱动 |
| `core/cache-stats.ts` / `usage-totals.ts` | 纯函数 |
| `core/http-dispatcher.ts` → `configureHttpDispatcher()` | 启动期一次 |
| `scripts/generate-schemas.ts` / `generate-references.ts` | `npm run generate:schemas` / `generate:references` |

### 1.4 依赖顺序（建议移植顺序）

1. 常量与纯函数：`defaults.ts`、`radius.ts`、`experimental.ts`、`settings-defaults.ts`、`telemetry.ts`、`client-identity.ts`、`auth-guidance.ts`、`usage-totals.ts`、`settings-diagnostics.ts`
2. 值解析：`resolve-config-value.ts`、`http-dispatcher.ts`
3. 存储：`auth-storage.ts` → `runtime-credentials.ts`、`models-store.ts`、`trust-manager.ts`
4. 设置：`settings-schema.ts` → `settings-manager.ts`
5. 模型：`model-config.ts` → `provider-composer.ts` → `remote-catalog-provider.ts` + `virtual-models.ts` → `model-runtime.ts` → `model-resolver.ts` → `model-registry.ts`
6. 键位：`key-id-schema.ts` → `keybindings.ts` → `keybindings-schema.ts`
7. 统计与维护：`cache-stats.ts`、`cache-warmer.ts`、`project-trust.ts`
8. 代码生成：`generate-schemas.ts`、`generate-references.ts`

---

## 2. 逐文件规范

### 2.1 `defaults.ts`（12 行）

```ts
export const DEFAULT_THINKING_LEVEL: ThinkingLevel = "medium";
export const THINKING_LEVEL_OPTIONS: readonly ThinkingLevel[] = [
	"off", "minimal", "low", "medium", "high", "xhigh", "max",
];
```

- `ThinkingLevel` 来自 `@earendil-works/kiana-agent-core`（`packages/agent/src/types.ts:349`）：
  `export type ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";`
  —— **含** `off`，共 7 个值。`DEFAULT_THINKING_LEVEL = "medium"` 是该联合的成员。
- `ModelThinkingLevel`（`packages/ai/src/providers/model-schema.ts:89`，`Static<typeof ModelThinkingLevelSchema>`）取值同 7 个：`MODEL_THINKING_LEVELS = ["off", ...THINKING_LEVELS]`，其中 kiana-ai 自己的 `THINKING_LEVELS`（`model-schema.ts:3`）是 `["minimal","low","medium","high","xhigh","max"]`（**不含** `off`，与 agent-core 的同名类型不是一回事）。`ThinkingLevelSchema = Type.Enum(THINKING_LEVELS)` 因此不含 `off`，只有 `ModelThinkingLevelSchema` 含 `off`。
- Rust 侧：一个 7 值枚举 `ThinkingLevel` 即可，`ModelThinkingLevel` 可为别名；但 `ThinkingLevelSchema`（model 级 `thinkingLevel` 字段校验）必须拒绝 `"off"`，`ModelThinkingLevelSchema` 必须接受 `"off"` —— 两者不能合并成一个校验器。`THINKING_LEVEL_OPTIONS` 使用 agent-core 的 `ThinkingLevel` 联合（含 `off`）。
- `THINKING_LEVEL_OPTIONS` 是 UI 展示顺序，**含** `off`，顺序不可改。

### 2.2 `experimental.ts`（3 行）

```ts
export function areExperimentalFeaturesEnabled(): boolean {
	return process.env.KIANA_EXPERIMENTAL === "1";
}
```

- 只认字符串 `"1"`；`"0"`、`""`、`"true"` 均为 `false`（`test/experimental.test.ts` 锁定）。

### 2.3 `radius.ts`（11 行）

```ts
export const RADIUS_PROVIDER_ID = "radius";
export const ENV_RADIUS_GATEWAY = "KIANA_RADIUS_GATEWAY";
export const RADIUS_MCP_URL = `${normalizeRadiusGatewayUrl(DEFAULT_RADIUS_GATEWAY)}/mcp`;
export function getRadiusGatewayUrl(): string {
	return normalizeRadiusGatewayUrl(process.env[ENV_RADIUS_GATEWAY] ?? DEFAULT_RADIUS_GATEWAY);
}
```

- `DEFAULT_RADIUS_GATEWAY = "https://radius.kiana.dev"`（`ai/providers/radius-config.ts`）。
- `normalizeRadiusGatewayUrl(value)`：若无 `^https?://`（`iu` 标志）前缀则加 `https://`，再删除结尾所有 `/`。
- 因此默认 `RADIUS_MCP_URL = "https://radius.kiana.dev/mcp"`。 **【逐字】**
- `getRadiusGatewayUrl()` 每次调用都读环境变量（不缓存）。

### 2.4 `telemetry.ts`（13 行）

```ts
function isTruthyEnvFlag(value: string | undefined): boolean {
	if (!value) return false;
	return value === "1" || value.toLowerCase() === "true" || value.toLowerCase() === "yes";
}
export function isInstallTelemetryEnabled(
	settingsManager: SettingsManager,
	telemetryEnv: string | undefined = process.env.KIANA_TELEMETRY,
): boolean {
	return telemetryEnv !== undefined ? isTruthyEnvFlag(telemetryEnv) : settingsManager.getEnableInstallTelemetry();
}
```

- `telemetryEnv` 只要**已定义**（含空串）就完全覆盖设置；空串 → `false`。
- 大小写不敏感；`"1"`、`"true"`、`"yes"` 为真；其余（含 `"0"`、`"false"`、`"no"`、`"2"`）为假。

### 2.5 `client-identity.ts`（16 行）

```ts
export function getClientIdentity(settingsManager: SettingsManager): ClientIdentity {
	return {
		userAgent: "detailed",
		version: VERSION,
		attribution: isInstallTelemetryEnabled(settingsManager) ? KIANA_ATTRIBUTION : null,
	};
}
```

- `KIANA_ATTRIBUTION = { name: "kiana", url: "https://kiana.dev", categories: ["cli-agent"] }`（`ai/utils/identity.ts`）。
- `version` 是 **coding-agent** 的 `VERSION`（`1.4.0`），不是 kiana-ai 的版本（两者当前相同）。
- `attribution: null` 表示明确关闭（合并身份时会覆盖默认）。
- 生成的实际请求头（`ai/utils/identity.ts`，供本单元测试比对）：
  - detailed UA：`kiana/<version> (<platform>; node/<process.version>; <arch>)`；非 Node/Bun 环境 `kiana/<version> (browser)`。
  - OpenRouter（`provider === "openrouter"` 或 `baseUrl` 含 `openrouter.ai`）：`HTTP-Referer: <url>`、`X-OpenRouter-Title: <name>`、`X-OpenRouter-Categories: <categories.join(",")>`（categories 为空/未定义时省略）。
  - NVIDIA（`provider === "nvidia"` 或 hostname 严格等于 `integrate.api.nvidia.com`）：`X-BILLING-INVOKE-ORIGIN: <Name>`（首字母大写）。
  - OpenCode（`provider` ∈ {`opencode`,`opencode-go`} 或 hostname 严格等于 `opencode.ai`）：`x-opencode-client: <name>`。
  - 会话头：`getOpenCodeSessionHeaders(target, sessionId)` → 仅当 `sessionId` 非空且目标为 OpenCode 时返回 `{ "x-opencode-session": sessionId }`。

### 2.6 `auth-guidance.ts`（25 行）

```ts
const UNKNOWN_PROVIDER = "unknown";

export function getProviderLoginHelp(): string {
	return [
		"Use /login to log into a provider via OAuth or API key. See:",
		`  ${join(getDocsPath(), "providers.md")}`,
		`  ${join(getDocsPath(), "models.md")}`,
	].join("\n");
}

export function formatNoModelsAvailableMessage(): string {
	return `No models available. ${getProviderLoginHelp()}`;
}

export function formatNoModelSelectedMessage(): string {
	return `No model selected.\n\n${getProviderLoginHelp()}\n\nThen use /model to select a model.`;
}

export function formatNoApiKeyFoundMessage(provider: string): string {
	const providerDisplay = provider === UNKNOWN_PROVIDER ? "the selected model" : provider;
	return `No API key found for ${providerDisplay}.\n\n${getProviderLoginHelp()}`;
}
```

**【逐字】** 完整展开（`<docs>` = `getDocsPath()` = `<package-dir>/docs`）：

```
Use /login to log into a provider via OAuth or API key. See:
  <docs>/providers.md
  <docs>/models.md
```

```
No models available. Use /login to log into a provider via OAuth or API key. See:
  <docs>/providers.md
  <docs>/models.md
```

```
No model selected.

Use /login to log into a provider via OAuth or API key. See:
  <docs>/providers.md
  <docs>/models.md

Then use /model to select a model.
```

```
No API key found for <provider>.

Use /login to log into a provider via OAuth or API key. See:
  <docs>/providers.md
  <docs>/models.md
```

- provider 字面量 `"unknown"` 被替换为 `the selected model`（`model-registry.ts` 的 `getApiKeyAndHeaders` 在 provider 未知时也走这条）。

### 2.7 `settings-defaults.ts`（67 行）

`SETTINGS_DEFAULTS` 在 `as const` 下逐字如下（键序即 source 顺序，测试与 JSON Schema 默认值均依赖）：

```ts
export const SETTINGS_DEFAULTS = {
	transport: "auto",
	sessionShareDestination: "auto",
	steeringMode: "one-at-a-time",
	followUpMode: "one-at-a-time",
	compaction: { enabled: true, reserveTokens: 16384, keepRecentTokens: 20000 },
	branchSummary: { reserveTokens: 16384, skipPrompt: false },
	retry: {
		enabled: true,
		maxRetries: 3,
		baseDelayMs: 2000,
		maxAgentDelayMs: DEFAULT_MAX_AGENT_RETRY_DELAY_MS,   // 60000
		provider: { maxRetryDelayMs: 60000 },
	},
	hideThinkingBlock: false,
	showCacheMissNotices: false,
	quietStartup: false,
	defaultProjectTrust: "ask",
	collapseChangelog: false,
	enableInstallTelemetry: true,
	enableAnalytics: false,
	enableSkillCommands: true,
	terminal: { showImages: true, imageWidthCells: 60, clearOnShrink: false, showTerminalProgress: false },
	images: { autoResize: true, blockImages: false },
	doubleEscapeAction: "tree",
	treeFilterMode: "default",
	editorPaddingX: 0,
	outputPad: 1,
	autocompleteMaxVisible: 5,
	markdown: { codeBlockIndent: "  ", mermaid: "streaming" },
	warnings: { anthropicExtraUsage: true },
	codemode: { mode: "on", inlineBudget: 3000 },
	cacheWarming: "streaming",
	tuiMode: "fullscreen",
	fullscreenExitOutput: "transcript",
	fullscreenScrollbar: "auto",
	fullscreenCopyOnSelect: true,
	fullscreenWheelScrollLines: "auto",
} as const;
```

注意：`markdown.codeBlockIndent` 是**两个空格**（`"  "`）。`retry.maxAgentDelayMs` 的默认值引用 kiana-ai 常量 `DEFAULT_MAX_AGENT_RETRY_DELAY_MS = 60_000`。

### 2.8 `settings-schema.ts`（482 行）

本文件只导出 TypeBox schema 与派生类型，是 `schemas/settings.schema.json` 的唯一来源。**运行期不校验**（settings.json 不做 schema 校验，见 §4.1）。

#### 2.8.1 辅助构造器

```ts
function nonNegativeSafeInteger(options: { default?: number } = {}) {
	return Type.Integer({ minimum: 0, maximum: Number.MAX_SAFE_INTEGER, ...options });
}
function timeoutSetting(options?: TSchemaOptions) {
	return Type.Union([Type.Number({ minimum: 0 }), Type.Literal("disabled")], options);
}
```

#### 2.8.2 子 schema（字段、默认值、描述逐字）

`CompactionModelOverrideSchema = Type.Object({ reserveTokens?: int>=0, keepRecentTokens?: int>=0 })`

`CompactionSettingsSchema`：
| 字段 | 类型 | 默认 | 描述 |
|---|---|---|---|
| `enabled` | boolean | `true` | — |
| `reserveTokens` | int [0, MAX_SAFE_INTEGER] | `16384` | — |
| `keepRecentTokens` | int [0, MAX_SAFE_INTEGER] | `20000` | — |
| `modelOverrides` | `Record<string, CompactionModelOverride>` | — | `Per-model overrides keyed by exact "provider/modelId" strings.` |

`BranchSummarySettingsSchema`：
| 字段 | 类型 | 默认 | 描述 |
|---|---|---|---|
| `reserveTokens` | number | `16384` | `Tokens reserved for the prompt and LLM response.` |
| `skipPrompt` | boolean | `false` | `When true, skips the "Summarize branch?" prompt and defaults to no summary.` |

`ProviderRetrySettingsSchema`：`timeoutMs?`（`SDK or provider request timeout in milliseconds.`）、`maxRetries?`（`SDK or provider retry attempts.`）、`maxRetryDelayMs?`（默认 `60000`，`Maximum server-requested delay before failing.`），均 `number`。

`RetrySettingsSchema`：`enabled?`(默认 true)、`maxRetries?`(默认 3)、`baseDelayMs?`(默认 2000，描述 `Exponential backoff base delay in milliseconds: 2s, 4s, 8s.`)、`maxAgentDelayMs?`(默认 60000)、`provider?`、`maxDelayMs?`（描述 `Legacy retry delay setting. Use provider.maxRetryDelayMs instead.`，**deprecated: true**）。

`TerminalSettingsSchema`：
| 字段 | 类型 | 默认 | 描述 |
|---|---|---|---|
| `showImages` | boolean | `true` | `Show images when the terminal supports them.` |
| `imageWidthCells` | number | `60` | `Preferred inline image width in terminal cells.` |
| `clearOnShrink` | boolean | `false` | `Clear empty rows when content shrinks.` |
| `showTerminalProgress` | boolean | `false` | `Show OSC 9;4 terminal progress indicators.` |
| `hyperlinks` | `boolean \| "auto"` | — | — |
| `images` | `"kitty" \| "iterm2" \| "auto" \| false` | — | — |
| `trueColor` | `boolean \| "auto"` | — | — |

`ImageSettingsSchema`：`autoResize?`(默认 true，`Resize images to 2000x2000 maximum for better model compatibility.`)、`blockImages?`(默认 false，`When true, prevents all images from being sent to LLM providers.`)。

`ThinkingBudgetsSettingsSchema = Type.Object({ minimal?: number, low?: number, medium?: number, high?: number })`。

`MarkdownSettingsSchema`：`codeBlockIndent?`(string，默认 `"  "`)、`mermaid?`(`"off" | "final" | "streaming"`，默认 `"streaming"`)。

`WarningSettingsSchema`：`anthropicExtraUsage?`(boolean，默认 true)。

`CodemodeModeSchema = Type.Union([Literal("on"), Literal("only")])`。

`CodemodeSettingsSchema`：
- `mode?`：默认 `"on"`，描述（**逐字**）：`How codemode presents tools. "on" keeps direct tools declared and annotates tools callable from scripts; codemode lists only tools without direct exposure. "only" lists every script-callable tool in codemode and does not declare active direct tools.`
- `inlineBudget?`：number，`minimum: 0`，默认 `3000`，描述 `Estimated tokens available for inline codemode tool declarations.`

`PackageSourceSchema`（union）：
- `string`，描述 `Load all resources from the package.`
- object：`source: string`（必填）、`autoload?: boolean`（`When false, start empty and only apply explicit resource patterns.`）、`extensions?/skills?/prompts?/themes?: string[]`
- 整体描述：`Package source for npm or git packages. Use the string form to load all resources or the object form to filter resources.`

`SkillsInputSchema`（union）：`string[]` 或 `{ enableSkillCommands?: boolean, customDirectories?: string[] }`（后者 `deprecated: true`）；整体描述 `Local skill file paths or directories.`

#### 2.8.3 `SettingsSchema` 顶层字段（顺序即生成 JSON 的属性顺序）

| 字段 | 类型 | 默认 | 描述（逐字，若有） |
|---|---|---|---|
| `$schema` | string | — | `JSON Schema reference.` |
| `lastChangelogVersion` | string | — | — |
| `defaultProvider` | string | — | — |
| `defaultModel` | string | — | — |
| `defaultThinkingLevel` | `ModelThinkingLevel` | — | — |
| `modelThinkingLevels` | `Record<string, ModelThinkingLevel>` | — | `Per-model default thinking level overrides keyed by "provider/modelId".` |
| `transport` | `"auto" \| "sse" \| "websocket" \| "websocket-cached"` | `"auto"` | — |
| `sessionShareDestination` | `"auto" \| "gist" \| "radius"` | `"auto"` | `Destination used by /share. "auto" uses Radius when authenticated and otherwise creates a GitHub gist.` |
| `steeringMode` | `"all" \| "one-at-a-time"` | `"one-at-a-time"` | — |
| `followUpMode` | `"all" \| "one-at-a-time"` | `"one-at-a-time"` | — |
| `theme` | string | — | — |
| `compaction` | object | — | — |
| `branchSummary` | object | — | — |
| `retry` | object | — | — |
| `hideThinkingBlock` | boolean | `false` | — |
| `showCacheMissNotices` | boolean | `false` | `Show cache cost and provider recovery notices.` |
| `externalEditor` | string | — | `Command for Ctrl+G external editor; takes precedence over VISUAL and EDITOR.` |
| `shellPath` | string | — | `Custom shell path, for example for Cygwin on Windows, with support for leading ~ expansion.` |
| `quietStartup` | `boolean \| "header"` | `false` | `When true, hide all startup output. When "header", keep only the startup header.` |
| `defaultProjectTrust` | `"ask" \| "always" \| "never"` | `"ask"` | `Global setting only.` |
| `shellCommandPrefix` | string | — | `Prefix prepended to every bash command, for example to enable shell aliases.` |
| `npmCommand` | string[] | — | `Command used for npm package lookup and installation, in argv form such as ["mise", "exec", "node@20", "--", "npm"].` |
| `collapseChangelog` | boolean | `false` | `Show the condensed changelog after update; use /changelog for the full changelog.` |
| `enableInstallTelemetry` | boolean | `true` | `Send an anonymous version and update ping after changelog-detected updates.` |
| `enableAnalytics` | boolean | `false` | `Opt in to analytics data sharing.` |
| `trackingId` | string | — | `Analytics tracking identifier, generated when analytics is enabled.` |
| `packages` | `PackageSource[]` | — | `npm or git package sources, as strings or objects with resource filtering.` |
| `extensions` | string[] | — | `Local extension file paths or directories.` |
| `deviceId` | string | — | `Stable installation UUID, generated when authentication first needs it. Global only.` |
| `skills` | `string[] \| object` | — | `Local skill file paths or directories.` |
| `prompts` | string[] | — | `Local prompt template file paths or directories.` |
| `themes` | string[] | — | `Local theme file paths or directories.` |
| `enableSkillCommands` | boolean | `true` | `Register skills as /skill:name commands.` |
| `terminal` | object | — | — |
| `images` | object | — | — |
| `enabledModels` | string[] | — | `Model patterns for cycling, in the same format as the --models CLI flag.` |
| `defaultTools` | string[] | — | `Initial tool selection. Plain names replace the inherited selection; +name and -name entries add or remove tools.` |
| `doubleEscapeAction` | `"fork" \| "tree" \| "none"` | `"tree"` | `Action for double-escape with an empty editor.` |
| `treeFilterMode` | `"default" \| "no-tools" \| "user-only" \| "labeled-only" \| "all"` | `"default"` | `Default filter when opening /tree.` |
| `thinkingBudgets` | object | — | `Custom token budgets for thinking levels.` |
| `editorPaddingX` | number | `0` | `Horizontal padding for the input editor.` |
| `outputPad` | `0 \| 1` | `1` | `Horizontal padding for transcript content.` |
| `autocompleteMaxVisible` | number | `5` | `Maximum visible items in the autocomplete dropdown.` |
| `showHardwareCursor` | boolean | — | `Show the terminal cursor while still positioning it for IME.` |
| `markdown` | object | — | — |
| `warnings` | object | — | — |
| `codemode` | object | — | — |
| `sessionDir` | string | — | `Custom session storage directory, in the same format as the --session-dir CLI flag.` |
| `httpProxy` | string | — | `Proxy URL applied as HTTP_PROXY and HTTPS_PROXY for Kiana-managed HTTP clients.` |
| `httpIdleTimeoutMs` | `number \| "disabled"` | — | `HTTP header or body idle timeout in milliseconds; 0 or "disabled" disables it.` |
| `cacheWarming` | `"off" \| "streaming" \| "idle"` | `"streaming"` | `Cache-warming profile. "idle" also warms between agent runs. Global only because each refresh costs money.` |
| `websocketConnectTimeoutMs` | `number \| "disabled"` | — | `WebSocket connect or open handshake timeout in milliseconds; 0 or "disabled" disables it.` |
| `tuiMode` | `"regular" \| "fullscreen"` | `"fullscreen"` | — |
| `fullscreenExitOutput` | `"transcript" \| "resume-hint"` | `"transcript"` | `No effect in regular TUI mode.` |
| `fullscreenScrollbar` | `"auto" \| "always" \| "hidden"` | `"auto"` | `No effect in regular TUI mode.` |
| `fullscreenCopyOnSelect` | boolean | `true` | `No effect in regular TUI mode.` |
| `fullscreenWheelScrollLines` | `number \| "auto"` | `"auto"` | `Lines scrolled per wheel event in fullscreen mode; numeric values are clamped from 1 to 100.` |
| `queueMode` | `"all" \| "one-at-a-time"` | — | `Legacy setting migrated to steeringMode.`（deprecated） |
| `websockets` | boolean | — | `Legacy setting migrated to transport.`（deprecated） |

上表共 **59** 行（`$schema` + 58 个设置项），与 `schemas/settings.schema.json` 的 `properties` 键数 59 完全一致；顺序即该 schema 中键的顺序。

顶层选项：`{ additionalProperties: true }` —— 未知键**允许**并原样保留（`config-schemas.test.ts` 的 `extensionSetting` 用例）。

#### 2.8.4 导出类型

```ts
type SettingsInput = Static<typeof SettingsSchema>;
export interface CompactionModelOverride ...
export interface CompactionSettings ...
export interface BranchSummarySettings ...
export interface ProviderRetrySettings ...
export interface RetrySettings extends Omit<Static<typeof RetrySettingsSchema>, "maxDelayMs"> {}
export interface TerminalSettings ...
export interface ImageSettings ...
export interface ThinkingBudgetsSettings ...
export type MermaidRenderingMode = NonNullable<Static<typeof MarkdownSettingsSchema>["mermaid"]>;
export interface MarkdownSettings ...
export interface WarningSettings ...
export type CodemodeMode = Static<typeof CodemodeModeSchema>;
export interface CodemodeSettings ...
export type DefaultProjectTrust = NonNullable<SettingsInput["defaultProjectTrust"]>;
export type QuietStartup = NonNullable<SettingsInput["quietStartup"]>;
export type TransportSetting = NonNullable<SettingsInput["transport"]>;
export type SessionShareDestination = NonNullable<SettingsInput["sessionShareDestination"]>;
export type PackageSource = Static<typeof PackageSourceSchema>;
export type FullscreenExitOutput = NonNullable<SettingsInput["fullscreenExitOutput"]>;
export type TuiMode = NonNullable<SettingsInput["tuiMode"]>;
export type CacheWarmingMode = NonNullable<SettingsInput["cacheWarming"]>;
export interface Settings extends Omit<SettingsInput, "queueMode" | "retry" | "skills" | "websockets"> {
	retry?: RetrySettings;
	skills?: string[];
}
```

**关键**：`Settings` 类型上 `skills` 是 `string[]`、`retry` 无 `maxDelayMs`、`queueMode` / `websockets` 不存在 —— 迁移在加载时已完成。Rust 侧建议 `Settings` 用 `#[serde(flatten)] extras: Map<String, Value>` 保留未知键（顺序：已知键按结构体顺序，未知键按插入顺序；见 §6 键序差异）。

### 2.9 `settings-manager.ts`（1443 行）

#### 2.9.1 常量与工具函数

```ts
export const CACHE_WARMING_MODES = ["off", "streaming", "idle"] as const;   // satisfies CacheWarmingMode[]
const DEFAULT_COMPACTION_TOKEN_SETTINGS: Required<CompactionModelOverride> = {
	reserveTokens: SETTINGS_DEFAULTS.compaction.reserveTokens,       // 16384
	keepRecentTokens: SETTINGS_DEFAULTS.compaction.keepRecentTokens, // 20000
};
export const DEFAULT_TOOL_NAMES: readonly string[] = ["read", "bash", "edit", "write"];
```

`isMergeableObject(v)`：`typeof v === "object" && v !== null && !Array.isArray(v)`。

`deepMergeObjects(base, overrides)`：
- 结果从 `{...base}` 开始；对 `Object.keys(overrides)` 逐键：
  - `overrideValue === undefined` → **跳过**（不删除 base 的键）；
  - base 与 override 都是可合并对象 → 递归；
  - 否则 → 用 override 值替换（**数组整体替换**，不做元素合并）。
- 键序：base 的键序在前，新增键按 override 的遍历顺序追加。

`isToolModifier(entry)`：`typeof entry === "string" && (entry.startsWith("+") || entry.startsWith("-"))`。

`getToolListError(entries)`：
```
modifiers = entries.filter(isToolModifier)
if modifiers.length === 0 -> undefined
if modifiers.length < entries.length -> "tool names cannot be mixed with +name or -name entries"
pattern = modifiers.find(e => e.includes("*"))
if pattern -> `+name and -name entries take exact tool names, not patterns: ${pattern}`
-> undefined
```

`applyToolModifiers(base, entries)`：
```
tools = [...base]
for entry of entries:
  if !isToolModifier(entry) continue
  name = entry.slice(1)
  index = tools.indexOf(name)
  if entry 以 "+" 开头 && index === -1 && name 非空 -> tools.push(name)
  else if entry 以 "-" 开头 && index !== -1 -> tools.splice(index, 1)
```
注意：`"+"`（name 为空）被忽略；重复 `+name` 不重复添加；`-name` 只删第一个匹配。

`mergeDefaultTools(base, overrides)`（**非导出**）：
```
if overrides === undefined -> base
if !Array.isArray(base) || !Array.isArray(overrides) || !overrides.every(isToolModifier) -> overrides
-> [...base, ...overrides]
```

`resolveDefaultTools(entries)`（**非导出**）：
```
plain = entries.filter(e => !isToolModifier(e))
return applyToolModifiers(plain.length > 0 || entries.length === 0 ? plain : DEFAULT_TOOL_NAMES, entries)
```
语义：有条目且全是修饰符 → 从 `DEFAULT_TOOL_NAMES` 起步；否则以 plain 列表为基（空列表 → 空基）。

`deepMergeSettings(base, overrides)`：`deepMergeObjects` 后单独处理 `defaultTools = mergeDefaultTools(base.defaultTools, overrides.defaultTools)`；非 undefined 时覆盖到结果上。

`parseTimeoutSetting(value, settingName)`：
```
timeoutMs = parseHttpIdleTimeoutMs(value)
if timeoutMs !== undefined -> timeoutMs
if value !== undefined -> throw `Invalid ${settingName} setting: ${String(value)}`
-> undefined
```
`String(value)` 是 JS 语义：`null` → `"null"`、`true` → `"true"`、`{}` → `"[object Object]"`、`[]` → `""`、`-1` → `"-1"`。

#### 2.9.2 类型

```ts
export type SettingsScope = "global" | "project";
export interface SettingsManagerCreateOptions { projectTrusted?: boolean; }
export interface SettingsStorage {
	withLock(scope: SettingsScope, fn: (current: string | undefined) => string | undefined): void;
}
export interface SettingsError { scope: SettingsScope; path?: string; error: Error; }
type SettingsPaths = Partial<Record<SettingsScope, string>>;
```

`toSettingsError(scope, error, path)`：`{ scope, ...(path ? { path } : {}), error: error instanceof Error ? error : new Error(String(error)) }`。

#### 2.9.3 `FileSettingsStorage`

构造：`cwd`、`agentDir` 都先 `resolvePath`；`globalSettingsPath = join(resolvedAgentDir, "settings.json")`，`projectSettingsPath = join(resolvedCwd, CONFIG_DIR_NAME, "settings.json")`。

`acquireLockSyncWithRetry(path)`：`proper-lockfile.lockSync(path, { realpath: false })`，`maxAttempts = 10`，`delayMs = 20`（同步忙等）。**仅** `code === "ELOCKED"` 且未到第 10 次时重试；其它错误立即抛出；耗尽后抛最后一次错误，否则 `new Error("Failed to acquire settings lock")`（该分支实际不可达）。

`withLock(scope, fn)`：
1. `path = scope === "global" ? global : project`；`dir = dirname(path)`。
2. `fileExists = existsSync(path)`；**若存在**才加锁（不存在时不建锁文件）。
3. `current = fileExists ? readFileSync(path, "utf-8") : undefined`。
4. `next = fn(current)`。
5. `next !== undefined` 时：若 `dir` 不存在则 `mkdirSync(dir, { recursive: true })`；若尚未加锁则现在加锁；`writeFileSync(path, next, "utf-8")`（**UTF-8，无 BOM，无结尾换行追加**）。
6. `finally` 释放锁。

#### 2.9.4 `InMemorySettingsStorage`

两个字段 `global` / `project`；`withLock` 直接以当前值调用 `fn`，`next !== undefined` 时写回。

#### 2.9.5 `SettingsManager` 私有状态

```
storage, globalSettings, projectSettings, settings (merged),
projectTrusted,
modifiedFields: Set<keyof Settings>,             // 全局
modifiedNestedFields: Map<keyof Settings, Set<string>>,
modifiedProjectFields: Set<keyof Settings>,
modifiedProjectNestedFields: Map<keyof Settings, Set<string>>,
globalSettingsLoadError: Error | null,
projectSettingsLoadError: Error | null,
writeQueue: Promise<void> = Promise.resolve(),
errors: SettingsError[],
settingsPaths: SettingsPaths
```

构造末：`settings = deepMergeSettings(globalSettings, projectSettings)`。

#### 2.9.6 构造工厂

`static create(cwd, agentDir = getAgentDir(), options = {})`：`resolvePath` 两者 → `new FileSettingsStorage(resolvedCwd, resolvedAgentDir)` → `fromStorageWithPaths(storage, options, { global: join(resolvedAgentDir,"settings.json"), project: join(resolvedCwd, CONFIG_DIR_NAME, "settings.json") })`。

`static fromStorage(storage, options = {})` → `fromStorageWithPaths(storage, options)`（paths 为空）。

`private static fromStorageWithPaths(storage, options, settingsPaths = {})`：
- `projectTrusted = options.projectTrusted ?? true`
- `globalLoad = tryLoadFromStorage(storage, "global")`；`projectLoad = tryLoadFromStorage(storage, "project", projectTrusted)`
- 两条错误按 **global 先、project 后** 顺序 push 进 `initialErrors`（带对应 path）。

`static inMemory(settings = {}, options = {})`：
- `storage = new InMemorySettingsStorage()`
- `initialSettings = migrateSettings(structuredClone(settings))`
- `storage.withLock("global", () => JSON.stringify(initialSettings, null, 2))`
- `fromStorage(storage, options)`
- 注意：`structuredClone` 会**丢弃函数/undefined 值以外的原型**；在 Rust 中用深拷贝。

`private static loadFromStorage(storage, scope, projectTrusted = true)`：
```
if scope === "project" && !projectTrusted -> {}
content = withLock 读出的字符串（fn 返回 undefined，不写）
if !content -> {}          // 空串也视为空
JSON.parse(stripBom(content)) 然后 migrateSettings
```
**不**捕获解析错误（由 `tryLoadFromStorage` 捕获）。

`tryLoadFromStorage` → `{ settings, error }`，任何 throw 都变成 `{ settings: {}, error }`。

#### 2.9.7 `migrateSettings(settings)` —— 迁移顺序不可变

1. **queueMode → steeringMode**：`"queueMode" in settings && !("steeringMode" in settings)` → `settings.steeringMode = settings.queueMode; delete settings.queueMode`。
2. **websockets → transport**：`!("transport" in settings) && typeof settings.websockets === "boolean"` → `settings.transport = settings.websockets ? "websocket" : "sse"; delete settings.websockets`。
3. **skills 对象 → 数组**：当 `settings.skills` 是**非 null、非数组的对象**：
   - 若 `skillsSettings.enableSkillCommands !== undefined && settings.enableSkillCommands === undefined` → 复制过去；
   - 若 `Array.isArray(customDirectories) && length > 0` → `settings.skills = customDirectories`；
   - 否则 `delete settings.skills`。
4. **retry.maxDelayMs → retry.provider.maxRetryDelayMs**：当 `settings.retry` 是非 null、非数组对象：
   - `providerSettings = (typeof retry.provider === "object" && retry.provider !== null) ? retry.provider : undefined`
   - 若 `typeof retry.maxDelayMs === "number"` 且 `(providerSettings?.maxRetryDelayMs === undefined || === null)` → `retry.provider = { ...(providerSettings ?? {}), maxRetryDelayMs: retry.maxDelayMs }`
   - **总是** `delete retrySettings.maxDelayMs`
5. 返回 `settings as Settings`（原地修改传入对象）。

注意：迁移是有条件的 `in` 检查，`undefined` 值也算"存在"（`"steeringMode" in settings` 为真）。

#### 2.9.8 读设置

- `getSettings()` / `getGlobalSettings()` / `getProjectSettings()` → `structuredClone` 副本（深拷贝；调用方可任意修改）。
- `isProjectTrusted()`。
- `setProjectTrusted(trusted)`：
  - 相同则直接返回；
  - 设置新值、清空 `modifiedProjectFields` / `modifiedProjectNestedFields`；
  - `trusted === false` → `projectSettings = {}`、`projectSettingsLoadError = null`、重算 `settings`，**不**记错误；
  - `trusted === true` → 重新 `tryLoadFromStorage(project, trusted)`，失败时 `recordError("project", err)`，重算 `settings`。

#### 2.9.9 重载与覆盖

- `async reload()`：先 `await writeQueue`；然后按 global → project 顺序分别调用 `tryLoadFromStorage`：
  - **成功**（`error === null`）→ 用新值替换该 scope 的 settings，并清对应 `*SettingsLoadError = null`；
  - **失败** → **不替换**该 scope 的 settings（保留上一次成功加载/内存中的旧值），只把错误写进 `*SettingsLoadError` 并 `recordError(scope, error, path)`。
  - 注意：`tryLoadFromStorage` 在 catch 时确实返回 `{ settings: {}, error }`，但 `reload()` 只在 `!error` 分支才赋值，所以 **reload 失败不会清空该 scope**——旧 settings 原样保留。`settings-manager.test.ts` 的 "should keep previous settings and report the file path when the file is invalid" 同时断言 `getTheme() === "dark"`（旧值保留）与 `drainErrors()` 里的 `{scope:"global", path}`。
  - 在两次加载之间**清空四个 modified 集合**；最后 `settings = deepMergeSettings(globalSettings, projectSettings)`。
  - Rust 侧对应：`Result` 的两个分支，`Err` 分支只更新 error 字段与 errors 列表，**不得**覆盖 settings 字段（可加单元测试：先成功加载 `{theme:"dark"}`，写入非法 JSON 后 reload，断言 `get_theme()` 仍为 `Some("dark")`）。
- `applyOverrides(overrides)`：`settings = deepMergeSettings(settings, overrides)`；**不**标记 modified，**不**写盘。

#### 2.9.10 修改标记与写盘

`markModified(field, nestedKey?)` / `markProjectModified(...)`：加入字段集；有嵌套键时加入该字段的嵌套集合。

`assertProjectTrustedForWrite()`：`!projectTrusted` → `throw new Error("Project is not trusted; refusing to write project settings")` **【逐字】**。

`recordError(scope, error)` → push `toSettingsError(scope, error, settingsPaths[scope])`。

`clearModifiedScope(scope)`：global → 清全局两个集合；否则清项目两个集合。

`enqueueWrite(scope, task)`：
```
writeQueue = writeQueue
  .then(() => { if project: assertProjectTrustedForWrite(); task(); clearModifiedScope(scope); })
  .catch(err => recordError(scope, err))
```
关键：**错误被吞掉并记录**，队列继续；写失败的字段 modified 标记**不清除**（测试 "should fail project settings writes when project is not trusted" 通过 `drainErrors` 观察）。

`persistScopedSettings(scope, snapshotSettings, modifiedFields, modifiedNestedFields)`：
```
storage.withLock(scope, current => {
  currentFileSettings = current ? migrateSettings(JSON.parse(stripBom(current))) : {}
  merged = {...currentFileSettings}
  for field of modifiedFields:
    value = snapshotSettings[field]
    if modifiedNestedFields.has(field) && typeof value === "object" && value !== null:
      baseNested = currentFileSettings[field] ?? {}
      inMemoryNested = value
      mergedNested = {...baseNested}
      for nestedKey of modifiedNestedFields.get(field): mergedNested[nestedKey] = inMemoryNested[nestedKey]
      merged[field] = mergedNested
    else:
      merged[field] = value
  return JSON.stringify(merged, null, 2)
})
```
- 顶层键序：`currentFileSettings` 的键序在前，新增字段按 `modifiedFields` 的**插入顺序**追加（JS `Set` 保序）。
- 输出为 2 空格缩进 JSON，**无结尾换行**（`FileSettingsStorage` 原样写入）。
- 注意 `migrateSettings` 对读到的文件内容也会执行，因此保存时会顺手把旧格式迁移成新格式写回。

`save()`（私有）：
```
settings = deepMergeSettings(globalSettings, projectSettings)
if globalSettingsLoadError -> return          // 文件损坏时不覆盖
snapshot = structuredClone(globalSettings)
enqueueWrite("global", () => persistScopedSettings("global", snapshot, new Set(modifiedFields), clone(modifiedNestedFields)))
```

`saveProjectSettings(settings)`：先 `assertProjectTrustedForWrite()`（**同步抛**）；`projectSettings = clone(settings)`；重算 `settings`；`projectSettingsLoadError` 存在则返回；否则 enqueue project 写。

`updateProjectSettings(field, update)`：assert trusted；`projectSettings` 深拷贝 → `update(copy)` → `markProjectModified(field)` → `saveProjectSettings(copy)`。

`async flush()`：`await writeQueue`。

`drainErrors()`：返回并清空 `errors`（保持顺序）。

#### 2.9.11 全部 getter / setter（逐条）

约定：所有 setter 都**写全局**（`globalSettings`）+ `markModified(field[, nested])` + `save()`，除非特别说明。所有 `??` 语义等价 Rust 的 `Option::unwrap_or(default)`，但注意 `||` 与 `??` 的区别（空串/0/false）。

| getter | 语义 |
|---|---|
| `getLastChangelogVersion()` | `settings.lastChangelogVersion` |
| `getSessionDir()` | 值存在且非空 → `normalizePath(value)`（展开 `~`、win32 路径转换）；空串/undefined 原样返回 |
| `getDefaultProvider()` / `getDefaultModel()` | 直读 |
| `getSteeringMode()` | `settings.steeringMode \|\| default`（空串 → 默认） |
| `getFollowUpMode()` | 同上 |
| `getThemeSetting()` | 仅当 `typeof === "string"` 返回值，否则 `undefined` |
| `getTheme()` | `getThemeSetting()`；含 `"/"` → `undefined`（自动主题 `system/...` 不当作固定主题） |
| `getDefaultThinkingLevel()` | 直读 |
| `getModelThinkingLevel(provider, modelId)` | `settings.modelThinkingLevels?.[`${provider}/${modelId}`]` |
| `getAllModelThinkingLevels()` | 浅拷贝对象 |
| `getTransport()` | `settings.transport ?? default` |
| `getSessionShareDestination()` | 仅 `"gist"`/`"radius"` 原样，其余 → `"auto"` |
| `getCompactionEnabled()` | `compaction?.enabled ?? true` |
| `getCompactionReserveTokens(model?)` / `getCompactionKeepRecentTokens(model?)` | 见下 |
| `getCompactionSettings(model?)` | `{ enabled, reserveTokens, keepRecentTokens }`，每个 token 独立解析 |
| `getBranchSummarySettings()` | `{ reserveTokens: ?? 16384, skipPrompt: ?? false }` |
| `getBranchSummarySkipPrompt()` | `?? false` |
| `getRetryEnabled()` | `retry?.enabled ?? true` |
| `getRetrySettings()` | `{ enabled, maxRetries ?? 3, baseDelayMs ?? 2000, maxAgentDelayMs ?? 60000 }` |
| `getHttpIdleTimeoutMs()` | `parseTimeoutSetting(settings.httpIdleTimeoutMs, "httpIdleTimeoutMs") ?? DEFAULT_HTTP_IDLE_TIMEOUT_MS (300000)` |
| `getCacheWarmingMode()` | **仅读 `globalSettings.cacheWarming`**；值在 `CACHE_WARMING_MODES` 内则原样，否则 `"streaming"` |
| `getProviderRetrySettings()` | `{ timeoutMs?, maxRetries?, maxRetryDelayMs: ?? 60000 }`（前两者不填默认） |
| `getWebSocketConnectTimeoutMs()` | `parseTimeoutSetting(settings.websocketConnectTimeoutMs, "websocketConnectTimeoutMs")` |
| `getHideThinkingBlock()` | `?? false` |
| `getShowCacheMissNotices()` | `?? false` |
| `getExternalEditorCommand()` | 非空（`trim() !== ""`）配置值 → 原样（**不 trim 返回**）；否则 `$VISUAL \|\| $EDITOR`；否则 win32 → `"notepad"`，其它 → `"nano"` |
| `getShellPath()` | 非空 → `normalizePath`；否则原样（`undefined`/`""`） |
| `getQuietStartup()` | `true` 或 `"header"` 原样，其余 → `false` |
| `getDefaultProjectTrust()` | **仅读 `globalSettings`**；`"always"`/`"never"` 原样，其余 → `"ask"` |
| `getShellCommandPrefix()` | 直读 |
| `getNpmCommand()` | 值存在 → 浅拷贝数组；否则 `undefined` |
| `getCollapseChangelog()` | `?? false` |
| `getEnableInstallTelemetry()` | `?? true` |
| `getEnableAnalytics()` | `?? false` |
| `getTrackingId()` | 直读 |
| `getOrCreateDeviceId()` | **仅全局**；无则 `randomUUID()` + `markModified("deviceId")` + `save()`；返回该值 |
| `getPackages()` | 浅拷贝数组，缺省 `[]` |
| `getExtensionPaths()` / `getSkillPaths()` / `getPromptTemplatePaths()` / `getThemePaths()` | 同上（`extensions` / `skills` / `prompts` / `themes`） |
| `getEnableSkillCommands()` | `?? true` |
| `getThinkingBudgets()` | 直读（对象引用，不拷贝） |
| `getTerminalCapabilityOverrides()` | 见下 |
| `getShowImages()` | `terminal?.showImages ?? true` |
| `getImageWidthCells()` | 非有限数 → `60`；否则 `max(1, floor(w))` |
| `getClearOnShrink()` | `terminal?.clearOnShrink !== undefined` → 该布尔；否则 `KIANA_CLEAR_ON_SHRINK === "1"` → true；否则 `false` |
| `getShowTerminalProgress()` | `?? false` |
| `getTuiMode()` | `=== "regular"` 原样，否则 `"fullscreen"` |
| `getFullscreenExitOutput()` | `=== "resume-hint"` 原样，否则 `"transcript"` |
| `getFullscreenScrollbar()` | `"always"`/`"hidden"` 原样，否则 `"auto"` |
| `getFullscreenCopyOnSelect()` | `?? true` |
| `getFullscreenWheelScrollLines()` | 有限数 → `max(1, min(100, floor))`；否则 `"auto"` |
| `getImageAutoResize()` | `?? true` |
| `getBlockImages()` | `?? false` |
| `getEnabledModels()` | 直读（引用） |
| `getDefaultTools()` | 见下 |
| `getDoubleEscapeAction()` | `?? "tree"` |
| `getTreeFilterMode()` | 值在 `["default","no-tools","user-only","labeled-only","all"]` 内则原样，否则 `"default"` |
| `getShowHardwareCursor()` | `settings.showHardwareCursor ?? (KIANA_HARDWARE_CURSOR === "1")` |
| `getEditorPaddingX()` | `?? 0`（**读取时不 clamp**） |
| `getOutputPad()` | `=== 0 ? 0 : 1` |
| `getAutocompleteMaxVisible()` | `?? 5` |
| `getCodeBlockIndent()` | `markdown?.codeBlockIndent ?? "  "` |
| `getMermaidRenderingMode()` | `"off"`/`"final"` 原样，否则 `"streaming"` |
| `getWarnings()` | `{...(settings.warnings ?? {})}` |

`getTerminalCapabilityOverrides()` **逐字**：
```ts
const terminal = this.settings.terminal;
const images = terminal?.images;
return {
	...(images === "kitty" || images === "iterm2" ? { images } : images === false ? { images: null } : {}),
	...(typeof terminal?.trueColor === "boolean" ? { trueColor: terminal.trueColor } : {}),
	...(typeof terminal?.hyperlinks === "boolean" ? { hyperlinks: terminal.hyperlinks } : {}),
};
```
即 `"auto"` 与未设置被省略；`false` → `images: null`。

`getDefaultTools()` **逐字**：
```ts
const tools = this.settings.defaultTools;
if (tools === undefined) return undefined;
return resolveDefaultTools(Array.isArray(tools) ? tools.filter(t => typeof t === "string") : []);
```

`getCompactionTokenSetting(field, model?)` —— **错误文案严格逐字**：
```
ordinary = settings.compaction?.[field]
if ordinary !== undefined && (typeof !== "number" || !Number.isSafeInteger || < 0):
    throw `Invalid compaction.${field} setting: ${String(ordinary)}. Expected a non-negative safe integer.`
modelKey = model ? `${model.provider}/${model.id}` : undefined
entry = modelKey !== undefined ? settings.compaction?.modelOverrides?.[modelKey] : undefined
if entry !== undefined && !isMergeableObject(entry):
    throw `Invalid compaction.modelOverrides["${modelKey}"] setting: ${String(entry)}. Expected an object.`
override = entry?.[field]
if override !== undefined && (typeof !== "number" || !Number.isSafeInteger || < 0):
    throw `Invalid compaction.modelOverrides["${modelKey}"].${field} setting: ${String(override)}. Expected a non-negative safe integer.`
return override ?? ordinary ?? DEFAULT_COMPACTION_TOKEN_SETTINGS[field]
```
行为要点（`settings-manager-compaction.test.ts` 锁定）：
- 模型键是**精确** `${provider}/${id}`（含 slash，不做 glob；`"provider/*"` 与 `"family/model"` 不匹配 `provider/family/model`）。
- 每个字段**独立**解析（override → ordinary → 默认）。
- ordinary 校验**先于**模型 override，即使 override 有效也会因 ordinary 非法而抛。
- 校验对**所有**模型（含其它 provider）都抛，因为 ordinary 先检查。
- `enabled` 不支持 per-model override（`modelOverrides[...].enabled` 被忽略）。
- NaN/Infinity 通过 `applyOverrides` 注入时同样抛（`Number.isSafeInteger(NaN) === false`）。
- `0` 是合法值。

#### 2.9.12 setter 特殊规则（其余按约定）

| setter | 特殊行为 |
|---|---|
| `setDefaultProvider` / `setDefaultModel` / `setDefaultModelAndProvider` | 后者同时 mark 两个字段（顺序 provider 先） |
| `setModelThinkingLevel(provider, modelId, level)` | 初始化 `modelThinkingLevels ??= {}`；写 `${provider}/${modelId}` |
| `removeModelThinkingLevel` | 无对象则**直接返回**（不 save）；删除后若对象为空则 `delete modelThinkingLevels`；仍 mark + save |
| `setCompactionEnabled(enabled)` | `compaction ??= {}`；`markModified("compaction","enabled")` |
| `setRetryEnabled(enabled)` | `retry ??= {}`；`markModified("retry","enabled")` |
| `setHttpIdleTimeoutMs(ms)` | `!Number.isFinite \|\| ms < 0` → `throw new Error(`Invalid httpIdleTimeoutMs setting: ${String(ms)}`)` **【逐字】**；写 `Math.floor(ms)` |
| `setCacheWarmingMode` | 写全局 |
| `setShowImages` / `setImageWidthCells` / `setClearOnShrink` / `setShowTerminalProgress` | `terminal ??= {}`；`markModified("terminal", <key>)`；`setImageWidthCells` 写 `max(1, floor(w))` |
| `setImageAutoResize` / `setBlockImages` | `images ??= {}`；`markModified("images", <key>)` |
| `setEditorPaddingX` | 写 `max(0, min(3, floor))` |
| `setAutocompleteMaxVisible` | 写 `max(3, min(20, floor))` |
| `setFullscreenWheelScrollLines` | `"auto"` 原样；否则 `max(1, min(100, floor))` |
| `setOutputPad` | 写 0 或 1 |
| `setMermaidRenderingMode` | `markdown ??= {}`；`markModified("markdown","mermaid")` |
| `setEnableAnalytics(enabled)` | mark `enableAnalytics`；若 `enabled && !trackingId` → `trackingId = randomUUID()` + mark `trackingId`；一次 save |
| `setNpmCommand` | 拷贝数组；`undefined` 保留 undefined |
| `setProjectPackages` / `setProjectExtensionPaths` / `setProjectSkillPaths` / `setProjectPromptTemplatePaths` / `setProjectThemePaths` | 走 `updateProjectSettings`（写项目 + 项目 modified 标记 + 未受信时同步抛） |
| `setPackages` / `setExtensionPaths` / ... | 直接赋值**传入数组引用**（不拷贝）到 globalSettings；getter 才拷贝 |
| `setWarnings` | 拷贝对象 |
| `setShellPath(path)` / `setShellCommandPrefix(prefix)` | 允许 `undefined`（显式写 undefined；JSON 序列化时该键消失） |

### 2.10 `settings-diagnostics.ts`（25 行）

```ts
export function collectSettingsDiagnostics(settingsManager: SettingsManager): AgentSessionRuntimeDiagnostic[] {
	return settingsManager.drainErrors().map(({ scope, path, error }) => ({
		type: "warning",
		message: path ? `Invalid settings file ${path}: ${error.message}` : `Invalid ${scope} settings: ${error.message}`,
	}));
}

export function deduplicateDiagnostics(diagnostics): AgentSessionRuntimeDiagnostic[] {
	const seen = new Set<string>();
	return diagnostics.filter((diagnostic) => {
		const key = `${diagnostic.type}\0${diagnostic.message}`;
		if (seen.has(key)) return false;
		seen.add(key);
		return true;
	});
}
```

- `AgentSessionRuntimeDiagnostic = { type: "info" | "warning" | "error", message: string }`（`core/agent-session-services.ts`）。
- 键用 **NUL 字符** 分隔（`"\0"`）；保留**首次**出现。
- 文案逐字：`Invalid settings file ${path}: ${message}` / `Invalid ${scope} settings: ${message}`（`scope` 是 `global`|`project`）。

### 2.11 `resolve-config-value.ts`（287 行）

用途：解析 `apiKey` / header 值 / `models.json` 中任何"配置值"，支持三种形态：字面值、`$ENV` 模板、`!command`。

#### 2.11.1 常量与内部类型

```ts
const commandResultCache = new Map<string, string | undefined>();     // 进程级，键是含 "!" 的完整 config
const ENV_VAR_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
const ENV_VAR_NAME_PREFIX_RE = /^[A-Za-z_][A-Za-z0-9_]*/;
type TemplatePart = { type: "literal"; value: string } | { type: "env"; name: string };
type ConfigValueReference = { type: "command"; config: string } | { type: "template"; parts: TemplatePart[] };
```

#### 2.11.2 `parseConfigValueTemplate(config)` —— 逐字符状态机（必须精确复刻）

```
parts = []; index = 0
while index < config.length:
  dollarIndex = config.indexOf("$", index)
  if dollarIndex < 0: appendLiteral(rest); break
  appendLiteral(config[index .. dollarIndex))
  nextChar = config[dollarIndex + 1]
  if nextChar === "$" || nextChar === "!":
      appendLiteral(nextChar); index = dollarIndex + 2; continue
  if nextChar === "{":
      endIndex = config.indexOf("}", dollarIndex + 2)
      if endIndex < 0: appendLiteral("$"); index = dollarIndex + 1; continue
      name = config[dollarIndex+2 .. endIndex)
      if ENV_VAR_NAME_RE.test(name): parts.push({env, name})
      else: appendLiteral(config[dollarIndex .. endIndex])         // 含花括号整段字面化
      index = endIndex + 1; continue
  match = config.slice(dollarIndex + 1).match(ENV_VAR_NAME_PREFIX_RE)
  if match: parts.push({env, name: match[0]}); index = dollarIndex + 1 + match[0].length; continue
  appendLiteral("$"); index = dollarIndex + 1
```

`appendLiteral(parts, value)`：空串直接返回；若最后一段已是 literal 则**拼接**（保证相邻字面量合并为一个 part）。

`parseConfigValueReference(config)`：`config.startsWith("!")` → `{command, config}`（**config 保留 `!`**），否则 `{template, parts}`。

#### 2.11.3 查询函数

```ts
function resolveEnvConfigValue(name, env?) { return env?.[name] || process.env[name] || undefined; }
```
注意 `||`：空串 env 值视为缺失，会回退到 `process.env`。

| 函数 | 行为 |
|---|---|
| `getConfigValueEnvVarName(config)` | 模板且**恰好一个** part 且是 env → 该 name；否则 undefined |
| `getConfigValueEnvVarNames(config)` | 模板 → 去重后的 env name 列表（按出现顺序）；command → `[]` |
| `getMissingConfigValueEnvVarNames(config, env?)` | 上述列表过滤掉已解析到（非空）的 |
| `isCommandConfigValue(config)` | `reference.type === "command"` |
| `isConfigValueConfigured(config, env?)` | `missing.length === 0`（command 恒为 true） |

#### 2.11.4 解析函数

`resolveConfigValue(config, env?)`：command → `executeCommand(config)`（**带缓存**）；template → `resolveTemplate(parts, env)`。

`resolveTemplate(parts, env)`：顺序拼字面量与环境值；任一 env 缺失 → 整个返回 `undefined`（不做部分替换）。

**命令执行**：

```
executeWithConfiguredShell(command):
  try:
    { shell, args, commandTransport } = getShellConfig()
    commandFromStdin = commandTransport === "stdin"
    result = spawnSync(shell, commandFromStdin ? args : [...args, command], {
       encoding: "utf-8",
       input: commandFromStdin ? command : undefined,
       timeout: 10000,
       stdio: [commandFromStdin ? "pipe" : "ignore", "pipe", "ignore"],
       shell: false, windowsHide: true })
    if result.error:
        code === "ENOENT" -> { executed: false, value: undefined }
        else -> { executed: true, value: undefined }
    if result.status !== 0 -> { executed: true, value: undefined }
    value = (stdout ?? "").trim(); return { executed: true, value: value || undefined }

executeWithDefaultShell(command):
  execSync(command, { encoding: "utf-8", timeout: 10000, stdio: ["ignore","pipe","ignore"] })
  return output.trim() || undefined
  任何异常 -> undefined

executeCommandUncached(commandConfig):
  command = commandConfig.slice(1)
  win32 -> ({executed, value} = executeWithConfiguredShell; executed ? value : executeWithDefaultShell)
  其它  -> executeWithDefaultShell(command)

executeCommand(commandConfig):
  if cache.has -> cache.get
  result = executeCommandUncached; cache.set; return result
```

- 超时 **10 000 ms**；空 stdout 或纯空白 → `undefined`；stderr 丢弃。
- 缓存跨进程生命周期，成功与失败都缓存；`clearConfigValueCache()` 清空（测试用）。
- **环境变量不缓存**（`resolveConfigValue` 每次都重读）。

`resolveConfigValueUncached(config, env?)`：与 `resolveConfigValue` 相同，但 command **不走缓存**（每次都执行）。

`resolveConfigValueOrThrow(config, description, env?)` —— 错误文案逐字：
```
值已解析 -> 返回
command -> `Failed to resolve ${description} from shell command: ${reference.config.slice(1)}`
             （注意：slice(1) 去掉了 "!"，即报错里是命令本身）
template:
  missing = getMissingConfigValueEnvVarNames(config, env)
  missing.length === 1 -> `Failed to resolve ${description} from environment variable: ${missing[0]}`
  missing.length > 1   -> `Failed to resolve ${description} from environment variables: ${missing.join(", ")}`
兜底 -> `Failed to resolve ${description}`
```

`resolveHeaders(headers, env?)`：`undefined` 输入 → `undefined`；逐项 `resolveConfigValue`，**只保留 truthy 值**（空串丢弃）；结果为空对象 → `undefined`。

`resolveHeadersOrThrow(headers, description, env?)`：`undefined` 输入 → `undefined`；逐项 `resolveConfigValueOrThrow(value, `${description} header "${key}"`, env)`；空对象 → `undefined`。注意它永不丢弃空串（因为 OrThrow 会抛或返回非 undefined）。

### 2.12 `http-dispatcher.ts`（113 行）

```ts
export const DEFAULT_HTTP_IDLE_TIMEOUT_MS = 300_000;
const DEFAULT_AUTO_SELECT_FAMILY_ATTEMPT_TIMEOUT_MS = 2_000;
export const HTTP_IDLE_TIMEOUT_CHOICES = [
	{ label: "30 sec", timeoutMs: 30_000 },
	{ label: "1 min", timeoutMs: 60_000 },
	{ label: "2 min", timeoutMs: 120_000 },
	{ label: "5 min", timeoutMs: 300_000 },
	{ label: "disabled", timeoutMs: 0 },
] as const;
```

`parseHttpIdleTimeoutMs(value)`：
```
string:
  trimmed = value.trim()
  trimmed.toLowerCase() === "disabled" -> 0
  trimmed === "" -> undefined
  -> parseHttpIdleTimeoutMs(Number(trimmed))          // 递归；"abc" -> Number("abc")=NaN -> undefined
number: !Number.isFinite || < 0 -> undefined；否则 Math.floor
其它类型 -> undefined
```
注意递归：`" 300000 "` → 300000；`"1e5"` → 100000；`"0x10"` → 16（JS `Number` 语义）。Rust 需复刻 ECMAScript 的 `Number(string)`（含 `0x`/`0b`/`0o`、`Infinity`、前后空白、空串→0）。

`formatHttpIdleTimeoutMs(ms)`：在 `HTTP_IDLE_TIMEOUT_CHOICES` 中找到**严格相等**的项 → `label`；否则 `` `${ms / 1000} sec` ``（JS 数字格式化，如 `45000 / 1000` → `"45 sec"`、`1500/1000` → `"1.5 sec"`）。

`applyHttpProxySettings(httpProxy)`：`proxy = httpProxy?.trim()`；空则返回；`process.env.HTTP_PROXY ??= proxy`；`process.env.HTTPS_PROXY ??= proxy`（**只在不存在的设**，已有值不覆盖）。

`configureHttpDispatcher(timeoutMs = DEFAULT_HTTP_IDLE_TIMEOUT_MS)`：
1. `normalized = parseHttpIdleTimeoutMs(timeoutMs)`；`undefined` → `throw new Error(`Invalid HTTP idle timeout: ${String(timeoutMs)}`)` **【逐字】**。
2. 创建 `undici.EnvHttpProxyAgent({ allowH2: false, proxyTunnel: true, bodyTimeout: normalized, connect: { autoSelectFamilyAttemptTimeout: 2_000 }, headersTimeout: normalized, clientFactory: createUndiciClient, factory: createUndiciOriginDispatcher })`，并用 `withUndiciErrorListener` 挂一个忽略错误的 `error` 监听（直接 `EventEmitter.prototype.on.call`，规避子类重写）。
3. `undici.setGlobalDispatcher(dispatcher)`。
4. 全局 fetch 安装：`installedGlobalFetch === undefined ? globalThis.fetch === originalGlobalFetch : globalThis.fetch === installedGlobalFetch` 为真时调用 `undici.install?.()` 并记录 `installedGlobalFetch`。目的：避免 Node 26 自带 fetch 与 npm undici dispatcher 混用导致压缩响应不解压。

`createUndiciClient(origin, options)` → `new undici.Client(origin, options)` 挂 error 监听。
`createUndiciOriginDispatcher(origin, options)`：`options.connections === 1` → `Client`；否则 `new undici.Pool(origin, {...options, factory: createUndiciClient})`（挂监听）。

**Rust 侧**：不再有 undici；用 `reqwest` 的 proxy（`EnvHttpProxyAgent` 等价 = 读 `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`），idle timeout 语义映射到 `reqwest::ClientBuilder::read_timeout(...)` / 连接的 `http1_header_read_timeout`（或 hyper 的 `header_read_timeout`）。`proxyTunnel: true` = 对 http 源也用 CONNECT（`reqwest::Proxy` 默认对 https 用隧道、对 http 直连；需显式 `Proxy::all`）。`allowH2: false` → `.http2_prior_knowledge()` 不启用且禁用 ALPN h2（`http2_adaptive_window` / `http1_only()` 近似，但应保持 h1-only 与 h2 仅通过协商禁用）。`autoSelectFamilyAttemptTimeout: 2_000` = Happy Eyeballs 每地址尝试超时 2 s。

### 2.13 `auth-storage.ts`（507 行）

#### 2.13.1 常量

```ts
type AuthStorageData = Record<string, Credential>;
const MAX_EPERM_RETRIES = 5;
const AUTH_FILE_WRITE_OPTIONS = { encoding: "utf-8", mode: 0o600 } as const;   // mode 仅创建时生效
```

`getRetryableLockErrorCode(error)`：`error instanceof Error && "code" in error` 时取 code；`"ELOCKED"` → 返回；`"EPERM"` 且 `process.platform === "win32"` → 返回；否则 undefined。

`type LockResult<T> = { result: T; next?: string }`。

共享读状态（跨实例合并）：
```ts
let sharedAuthFileReadState: { authPath: string; readState: AuthFileReadState } | undefined;
type AuthFileReload = { controller: AbortController; promise: Promise<AuthStorageData>; readers: number };
type AuthFileReadState = { data: AuthStorageData; revision?: string; reload?: AuthFileReload };
```

#### 2.13.2 `FileAuthStorageBackend`

`constructor(authPath = join(getAgentDir(), "auth.json"))` → `normalizePath`。

- `ensureParentDir()`：目录不存在 → `mkdirSync(dir, { recursive: true, mode: 0o700 })`。
- `ensureFileExists()`：文件不存在 → `writeFileSync(authPath, "{}", { encoding: "utf-8", mode: 0o600 })`。**内容就是 `{}`**（无换行）。
- `acquireLockSyncWithRetry(path)`：`lockfile.lockSync(path, { realpath: false })`；`maxAttempts = 10`，`delayMs = 20`；仅可重试错误码（ELOCKED，win32 上还有 EPERM）重试。
- `withLock(fn)`（同步）：
  1. `ensureParentDir()` → `ensureFileExists()`（**读也会创建空 auth.json**；`auth-check.test.ts` 的 "does not create an auth file or its parent directory" 走的是只读 `ReadOnlyAuthStorage`，不受影响）。
  2. 加锁；`current = existsSync ? readFileSync(utf-8) : undefined`。
  3. `{result, next} = fn(current)`；`next !== undefined` → `writeFileSync(authPath, next, AUTH_FILE_WRITE_OPTIONS)`。
  4. finally 释放。
- `acquireLockAsync(signal, onCompromised)`：
  ```
  staleMs = 30_000; maxDelayMs = 2_000; deadline = Date.now() + staleMs
  retry = 0; epermRetries = 0
  loop:
    signal?.throwIfAborted()
    try: release = await lockfile.lock(authPath, { realpath: false, retries: 0, stale: staleMs, onCompromised })
    catch error:
       signal?.throwIfAborted()
       code = getRetryableLockErrorCode(error)
       remainingMs = deadline - Date.now()
       if !code || remainingMs <= 0 -> throw
       if code === "EPERM" && ++epermRetries > 5 -> throw
       baseDelayMs = Math.min(10 * 2 ** retry, maxDelayMs / 2)     // 10,20,40,...,1000
       retry++
       delayMs = Math.min(Math.round(baseDelayMs * (1 + Math.random())), remainingMs)   // 抖动 [base, 2*base)
       sleep(delayMs)（有 signal 时用 { signal } 版本）
       continue
    if signal?.aborted: await release(); signal.throwIfAborted()
    return release
  ```
- `withLockAsync(fn, options?)`：
  ```
  options?.signal?.throwIfAborted(); ensureParentDir(); ensureFileExists()
  lockCompromised = false; lockCompromisedError = undefined
  throwIfCompromised() -> 若 compromised 抛 lockCompromisedError ?? new Error("Auth storage lock was compromised")
  try:
    release = await acquireLockAsync(signal, err => { lockCompromised = true; lockCompromisedError = err })
    throwIfCompromised(); signal?.throwIfAborted()
    current = existsSync ? readFileSync(utf-8) : undefined
    {result, next} = await fn(current)
    throwIfCompromised(); signal?.throwIfAborted()
    if next !== undefined -> writeFileSync(authPath, next, AUTH_FILE_WRITE_OPTIONS)
    throwIfCompromised(); return result
  finally: release 存在时 try { await release() } catch {}    // 锁被破坏时忽略解锁错误
  ```

#### 2.13.3 `ReadOnlyAuthStorage`

`load()`（惰性一次）：
```
data 已缓存 -> 返回
读文件（ENOENT -> data = {}）
JSON.parse(stripBom(...))；解析错误 -> `Failed to read auth.json: ${message}` **【逐字】**
非对象 / null / 数组 -> `Invalid auth.json: expected an object` **【逐字】**
逐 provider 校验：
  credential 非对象 / null / 数组 -> `Invalid auth.json credential for provider "${providerId}"`
  type === "api_key"：key 为 undefined 或 string，且 env 为 undefined 或（非 null 非数组的对象且所有值 string）-> 通过
  type === "oauth"：access string && refresh string && expires 有限 number -> 通过
  否则 -> `Invalid auth.json credential for provider "${providerId}"`
```
`read(providerId, options?)`：`signal?.throwIfAborted()` 前后各一次；credential 缺失 → undefined；若 `type !== "api_key"` 或 key 为空真值 或 `isCommandConfigValue(key)` → 返回 `structuredClone(credential)`（**不解析**，避免执行命令）；否则返回 `{...credential, key: resolveConfigValue(key, credential.env)}`。
`list()`：`{ providerId, type }[]`（按文件键序）。
`modify()` / `delete()`：抛 `Read-only credential storage cannot modify auth.json` **【逐字】**。

#### 2.13.4 `InMemoryAuthStorageBackend`

- `withLock(fn)`：同步调用、`next` 存在则写入 `this.value`。
- `withLockAsync(fn, options?)`：以 `asyncChain` 串行化；每步先 `await previous.catch(()=>{})`，再 `signal?.throwIfAborted()`，`await fn(this.value)`，再 `throwIfAborted()`，写回；返回 `raceWithAbortSignal(operation, signal)`。

#### 2.13.5 `AuthStorage`

构造：`authPath` 给出时若与 `sharedAuthFileReadState.authPath` 相同则复用 readState；否则新建 `{ data: {} }`，并在 `sharedAuthFileReadState` 为空时注册。若 `getFileRevision(authPath)` 与 `readState.revision` 相同则**跳过**首次 `reload()`。

工厂：`create(authPath = join(getAgentDir(),"auth.json"))` → `normalizePath` + `FileAuthStorageBackend`；`fromStorage(storage)`；`inMemory(data = {})` → `InMemoryAuthStorageBackend` 写入 `JSON.stringify(data, null, 2)`。

`parseStorageData(content)`：空 → `{}`；否则 `JSON.parse(stripBom(content))`（**不校验**）。

`reload()`（同步）：在 `withLock` 中读出 content 与 `getFileRevision`；`updateReadState(parseStorageData(content), revision)`；任何异常**吞掉**（保留旧快照）。

`reloadFromStorageAsync(options?)`：`withLockAsync` 内解析 + 更新 readState + 返回 `{result: data}`。

`readLatestData(options?)`：
```
signal?.throwIfAborted()
if !authPath:
    reload = reloadFromStorageAsync(options)
    return options?.signal ? reload : reload.catch(() => readState.data)
revision = getFileRevision(authPath)
if revision !== undefined && revision === readState.revision -> readState.data
if !readState.reload:
    创建 controller + reload { promise: reloadFromStorageAsync({signal: controller.signal}), readers: 0 }
    挂 then(onFulfilled/onRejected) 清除 readState.reload（仅当仍指向自己）
reload.readers++
try:
    result = raceWithAbortSignal(reload.promise, options?.signal)
    return options?.signal ? await result : await result.catch(() => readState.data)
finally:
    reload.readers--
    if readers === 0 && readState.reload === reload: 清除并 controller.abort()
```

`read(provider, options?)`：`readLatestData` 取 credential；`signal?.throwIfAborted()`；`type !== "api_key"` → 原样返回；`key === undefined` → 原样；否则 `{...credential, key: resolveConfigValue(credential.key, credential.env)}`。

`modify(provider, fn, options?)`：
```
latestData = readState.data; revision = undefined
result = await storage.withLockAsync(async content => {
   currentData = parseStorageData(content)
   next = await fn(currentData[provider])
   if next === undefined:
      latestData = currentData; revision = authPath ? getFileRevision(authPath) : undefined
      return { result: currentData[provider] }
   merged = {...currentData, [provider]: next}
   latestData = merged
   return { result: next, next: JSON.stringify(merged, null, 2) }
}, options)
updateReadState(latestData, revision); return result
```
- 关键：`fn` 返回 `undefined` 表示"不修改"，且**不写盘**（`auth.json` 保持原样，包括 malformed 内容不变 —— `"does not overwrite malformed auth files"` 依赖 `parseStorageData` 抛错导致 `withLockAsync` 失败）。
- `modify` 后 `latestData` 是**整份**合并后的数据（保留外部对其它 provider 的修改）。

`delete(provider, options?)`：withLockAsync 内 `delete currentData[provider]`，返回 `next: JSON.stringify(currentData, null, 2)`；`updateReadState(latestData)`（**不带 revision**，因此下次读会重新用 revision 比对；由于 revision 被清空，下一次调用会命中 reload 路径）。

`list(options?)`：`Object.entries(await readLatestData(options))` → `{providerId, type}`。

#### 2.13.6 `readStoredCredential(providerId, authPath = join(getAgentDir(),"auth.json"))`

一次性同步读：`JSON.parse(stripBom(readFileSync(normalizePath(authPath), "utf-8")))[providerId]`；任何异常 → `undefined`。不解析 key、不加锁、不创建文件。

### 2.14 `runtime-credentials.ts`（52 行）

```ts
export class RuntimeCredentials implements CredentialStore {
	private readonly store: CredentialStore;
	private readonly overrides = new Map<string, string>();   // providerId -> apiKey

	setRuntimeApiKey(providerId, apiKey): void          // overrides.set
	removeRuntimeApiKey(providerId): void               // overrides.delete
	hasRuntimeApiKey(providerId): boolean               // overrides.has

	async read(providerId, options?) {
		options?.signal?.throwIfAborted();
		const override = this.overrides.get(providerId);
		return override ? { type: "api_key", key: override } : this.store.read(providerId, options);
	}

	async list(options?) {
		const entries = new Map((await this.store.list(options)).map(e => [e.providerId, e]));
		options?.signal?.throwIfAborted();
		for (const providerId of this.overrides.keys()) entries.set(providerId, { providerId, type: "api_key" });
		return [...entries.values()];
	}

	modify(providerId, fn, options?) { return this.store.modify(providerId, fn, options); }

	async delete(providerId, options?) {
		options?.signal?.throwIfAborted();
		await this.store.delete(providerId, options);
		this.overrides.delete(providerId);
	}
}
```
- `list` 中 override **覆盖**同 provider 的底层条目，但保持底层位置（`Map.set` 不改变已有键的位置）；仅新增 provider 追加到末尾。
- `modify` 不触碰 overrides（因此"login 后 runtime key 仍优先"是刻意行为）。
- `delete` 清除 override。

### 2.15 `models-store.ts`（147 行）

```ts
type StoredModels = Record<string, ModelsStoreEntry>;
let sharedModelsFileReadState: { path: string; readState: ModelsFileReadState } | undefined;
```

`InMemoryCodingAgentModelsStore implements ModelsStore`：`Map<string, ModelsStoreEntry>`；`read` / `write` / `delete` 都先 `signal?.throwIfAborted()`；`read`/`write` 用 `structuredClone`（读写都深拷贝）。

`FileModelsStore implements ModelsStore`：
- 构造 `path = normalizePath(path ?? join(getAgentDir(), "models-store.json"))`；`storage = new FileAuthStorageBackend(path)`（复用 auth 的锁与 0600/0700 语义，但**不**预写 `{}` —— 实际 `FileAuthStorageBackend.withLockAsync` 会 `ensureFileExists()` 写 `{}`）；`readState` 与共享状态复用（仅当 path 相同）。共享状态只在 `sharedModelsFileReadState` 为空时注册（保留首个实例的 path）。
- `parse(content)`：空 → `{}`；否则 `JSON.parse(stripBom(content))`。
- `readLatest`：与 `AuthStorage.readLatestData` 同构（revision 比较、reload 合并、readers 计数、abort controller），但 `options?.signal` 缺失时**不**吞错（`return await raceWithAbortSignal(reload.promise, options?.signal)`，没有 `.catch(() => readState.data)` 分支）。
- `read(providerId, options?)`：`readLatest` 后取值，`signal?.throwIfAborted()`，有值则 `structuredClone`。
- `write(providerId, entry, options?)`：
  ```
  await storage.withLockAsync(async content => {
     current = parse(content); current[providerId] = structuredClone(entry)
     latest = current
     return { result: undefined, next: JSON.stringify(current, null, 2) }
  }, options)
  if latest: updateReadState(readState, latest)      // 注意：不更新 revision
  ```
- `delete` 同构，`delete current[providerId]`。

`ModelsStoreEntry`（kiana-ai）：`{ models: AnyModel[]; lastModified?: number; checkedAt?: number; etag?: string }`。文件格式见 §4.4。

### 2.16 `trust-manager.ts`（246 行）

```ts
export type ProjectTrustDecision = boolean | null;
export interface ProjectTrustStoreEntry { path: string; decision: boolean; }
export interface ProjectTrustUpdate { path: string; decision: ProjectTrustDecision; }
export interface ProjectTrustOption { label: string; trusted: boolean; updates: ProjectTrustUpdate[]; savedPath?: string; }
type TrustFile = Record<string, boolean | null | undefined>;
const TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES = [
	"settings.json", "mcp.json", "extensions", "skills", "prompts", "themes", "SYSTEM.md", "APPEND_SYSTEM.md",
] as const;
```

`normalizeCwd(cwd)` = `canonicalizePath(resolvePath(cwd))`（`realpathSync`，失败时回退原路径）。

`findNearestTrustEntry(data, cwd)`：从 `normalizeCwd(cwd)` 向上，首个 `value === true || value === false` 的条目返回 `{path: currentDir, decision: value}`；到达根（`dirname(x) === x`）→ null。**null 值被跳过**（视为"无决定"）。

`getProjectTrustParentPath(cwd)`：`trustPath = normalizeCwd`；`parent = dirname`；`parent === trustPath` → undefined，否则 parent。

`getProjectTrustOptions(cwd, { includeSessionOnly })` —— 顺序与文案**逐字**：
1. `{ label: "Trust", trusted: true, updates: [{path: trustPath, decision: true}], savedPath: trustPath }`
2. 有 parent 时：`{ label: `Trust parent folder (${parentPath})`, trusted: true, updates: [{path: parentPath, decision: true}, {path: trustPath, decision: null}], savedPath: parentPath }`
3. `includeSessionOnly` 时：`{ label: "Trust (this session only)", trusted: true, updates: [] }`
4. `{ label: "Do not trust", trusted: false, updates: [{path: trustPath, decision: false}], savedPath: trustPath }`
5. `includeSessionOnly` 时：`{ label: "Do not trust (this session only)", trusted: false, updates: [] }`

`readTrustFile(path)`：
- 不存在 → `{}`。
- 解析失败 → `Failed to read trust store ${path}: ${message}` **【逐字】**。
- 非对象 / null / 数组 → `Invalid trust store ${path}: expected an object` **【逐字】**。
- 值不是 true/false/null → `Invalid trust store ${path}: value for ${JSON.stringify(key)} must be true, false, or null` **【逐字】**（`JSON.stringify` 加引号并转义）。

`writeTrustFile(path, data)`：按键**字典序**（`Object.keys(data).sort()`，JS 默认 UTF-16 码元序）重排，只保留 true/false/null；`mkdirSync(dirname, {recursive:true})`；`writeFileSync(path, JSON.stringify(sorted, null, 2) + "\n", "utf-8")` —— **结尾有一个 `\n`**。

`acquireTrustLockSync(path)`：`trustDir = dirname(path)`；`mkdirSync(trustDir, {recursive:true})`；`lockfile.lockSync(trustDir, { realpath: false, lockfilePath: `${path}.lock` })`（**锁的是目录，但锁文件路径是 `<trust.json>.lock`**）；10 次 × 20 ms 重试，仅 ELOCKED。

`hasTrustRequiringProjectResources(cwd)`：
```
homeDir = canonicalizePath(resolvePath(process.env.HOME || homedir()))
userAgentsSkillsDir = join(homeDir, ".agents", "skills")
currentDir = canonicalizePath(resolvePath(cwd))
if 任一 TRUST_REQUIRING_* 存在于 join(currentDir, CONFIG_DIR_NAME) -> true
loop:
  agentsSkillsDir = join(currentDir, ".agents", "skills")
  if agentsSkillsDir !== userAgentsSkillsDir && existsSync(agentsSkillsDir) -> true
  parent = dirname(currentDir); if parent === currentDir -> false
  currentDir = parent
```
注意：`$HOME/.agents/skills` 永远不算（即使 cwd 是 `$HOME`）。

`ProjectTrustStore`：
- 构造：`trustPath = join(resolvePath(agentDir), "trust.json")`。
- `get(cwd)` → `getEntry(cwd)?.decision ?? null`。
- `getEntry(cwd)` → 加锁后读文件 + `findNearestTrustEntry`。
- `set(cwd, decision)` → `setMany([{path: cwd, decision}])`。
- `setMany(decisions)` → 加锁；读；逐项 `key = normalizeCwd(path)`，`decision === null` → `delete data[key]`，否则赋值；写回（排序 + 结尾换行）。

### 2.17 `project-trust.ts`（97 行）

```ts
export type AppMode = "interactive" | "print" | "json" | "rpc";
export interface ResolveProjectTrustedOptions {
	cwd: string;
	trustStore: ProjectTrustStore;
	trustOverride?: boolean;
	defaultProjectTrust?: DefaultProjectTrust;
	extensionsResult?: LoadExtensionsResult;
	projectTrustContext: ProjectTrustContext;
	onExtensionError?: (message: string) => void;
}
```

`formatProjectTrustPrompt(cwd)` **【逐字】**（`\n` 为真实换行）：
```
Trust project folder?
${cwd}

This allows ${APP_NAME} to load ${CONFIG_DIR_NAME} settings and resources, install missing project packages, and execute project extensions.
```
默认展开：`Trust project folder?\n<cwd>\n\nThis allows kiana to load .kiana settings and resources, install missing project packages, and execute project extensions.`

`selectProjectTrustOption(cwd, ctx)`：`getProjectTrustOptions(cwd, { includeSessionOnly: true })`；`ctx.ui.select(formatProjectTrustPrompt(cwd), options.map(o => o.label))`；按 label 匹配回 option（找不到 → undefined）。

`saveProjectTrustPromptResult(trustStore, result)`：`result.updates.length > 0` 时 `trustStore.setMany(result.updates)`。

`resolveProjectTrusted(options)` —— 判定顺序**不得改变**：
1. `trustOverride !== undefined` → 返回它。
2. `!hasTrustRequiringProjectResources(cwd)` → `true`。
3. 若 `extensionsResult` 存在：`emitProjectTrustEvent(extensionsResult, { type: "project_trust", cwd }, projectTrustContext)`；对每个 error 调 `onExtensionError(`Extension "${error.extensionPath}" project_trust error: ${error.error}`)` **【逐字】**；若 `result` 为真：`trusted = result.trusted === "yes"`；`result.remember === true` → `trustStore.set(cwd, trusted)`；返回 trusted。
4. `decision = trustStore.get(cwd)`；`decision !== null` → 返回它。
5. `switch (defaultProjectTrust ?? "ask")`：`"always"` → true；`"never"` → false；`"ask"` → 继续。
6. `!projectTrustContext.hasUI` → `false`。
7. UI 选择：选中 → `saveProjectTrustPromptResult` + 返回 `selected.trusted`；未选中 → `false`。

`ProjectTrustContext`（extensions 单元）：`{ cwd: string; mode: ExtensionMode; hasUI: boolean; ui: Pick<ExtensionUIContext, "select" | "confirm" | "input" | "notify"> }`。

### 2.18 `model-config.ts`（219 行）

#### 2.18.1 Schemas（`models.json` 的唯一校验来源）

内部 schema：
```ts
const SamplingParamsSchema = Type.Record(Type.String(), Type.Unknown());
const SamplingParamsByThinkingLevelSchema = Type.Object({
	off?, minimal?, low?, medium?, high?, xhigh?, max?: SamplingParamsSchema,
}, { description: "samplingParams per thinking level, merged over samplingParams." });
const SamplingParamsFieldSchema = Type.Record(Type.String(), Type.Unknown(), {
	description: "Fields merged into every request body. Applies only to OpenAI-compatible APIs." });
const HeadersSchema = Type.Record(Type.String(), Type.String(), {
	description: "Request headers. Values resolve like apiKey." });
```

`ModelDefinitionSchema`（models 条目；默认值只在这里生效）：
| 字段 | 类型 / 约束 | 默认 | 描述 |
|---|---|---|---|
| `id` | string, minLength 1 | — | `Model ID sent to the API.` |
| `name` | string, minLength 1 | — | `Display name, also matched by --model patterns.` |
| `api` | string, minLength 1 | — | `Overrides the provider api.` |
| `baseUrl` | string, minLength 1 | — | `Overrides the provider baseUrl.` |
| `reasoning` | boolean | `false` | `Whether the model supports thinking.` |
| `thinkingLevelMap` | `ThinkingLevelMapSchema` | — | — |
| `input` | `ModelInputModalitySchema[]` | `["text"]` | — |
| `inputLimits` | `ModelInputLimitsSchema` | — | — |
| `cost` | `ModelCostSchema` | — | — |
| `promptCache` | `ModelPromptCacheSchema` | — | — |
| `contextWindow` | number, `exclusiveMinimum: 0` | `128000` | `Context window in tokens.` |
| `maxTokens` | number, `exclusiveMinimum: 0` | `16384` | `Maximum output tokens.` |
| `samplingParams` | `SamplingParamsFieldSchema` | — | — |
| `samplingParamsByThinkingLevel` | 上述 | — | — |
| `headers` | `HeadersSchema` | — | — |
| `compat` | `ProviderCompatSchema` | — | — |

`ModelOverrideSchema`：同名字段但**无默认值**，`cost` 为 `Type.Partial(ModelCostSchema)`，`contextWindow`/`maxTokens` 也是 `exclusiveMinimum: 0` 无默认。

`ProviderConfigSchema`：
| 字段 | 类型 | 默认 | 描述 |
|---|---|---|---|
| `name` | string minLength 1 | — | `Display name.` |
| `baseUrl` | string minLength 1 | — | `API base URL. Also applies to the provider's built-in models.` |
| `apiKey` | string minLength 1 | — | `Literal key, "$NAME" environment interpolation, or "!command" output.` |
| `api` | string minLength 1 | — | `API for the provider's models, such as "openai-completions", "openai-responses", "anthropic-messages", or "google-generative-ai".` |
| `oauth` | `Literal("radius")` | — | `OAuth login flow. Requires baseUrl.` |
| `headers` | `HeadersSchema` | — | — |
| `compat` | `ProviderCompatSchema` | — | — |
| `authHeader` | boolean | `false` | `Whether to send the resolved API key as Authorization: Bearer.` |
| `models` | `ModelDefinitionSchema[]` | — | — |
| `modelOverrides` | `Record<string, ModelOverrideSchema>` | — | — |

`export const ModelsConfigSchema = Type.Object({ $schema?: string, providers: Record<string, ProviderConfigSchema> })` —— `providers` **必填**。
`export const ModelsJsonSchemas = { provider: ProviderConfigSchema, model: ModelDefinitionSchema, modelOverride: ModelOverrideSchema }`（`generate-references.ts` 的表格数据源）。

导出类型：`ModelsJsonModel`、`ModelsJsonModelOverride`、`ModelsJsonProvider`（`Static<>`）。

#### 2.18.2 `formatValidationPath(error)`（错误路径格式化，**逐字规则**）

```
if error.keyword === "required":
   requiredProperties = error.params.requiredProperties
   requiredProperty = requiredProperties[0]
   if requiredProperty:
       basePath = error.instancePath.replace(/^\//, "").replace(/\//g, ".")
       return basePath ? `${basePath}.${requiredProperty}` : requiredProperty
path = error.instancePath.replace(/^\//,"").replace(/\//g,".")
return path || "root"
```

#### 2.18.3 `deepFreeze(value)`

递归 `Object.values`；`typeof !== "object" || null || Object.isFrozen` 提前返回；最后 `Object.freeze`。Rust 侧用不可变 `Arc<ProviderConfig>` 即可（或 `serde_json::Value` 只读克隆）。

#### 2.18.4 `ModelConfig.load(modelsJsonPath)`

```
if !modelsJsonPath -> new ModelConfig(new Map())                    // 无错误
path = normalizePath(modelsJsonPath)
try content = await readFile(path, "utf-8")
catch error:
   code === "ENOENT" -> 空配置（无错误）
   否则 -> error = `Failed to load models.json: ${message}\n\nFile: ${path}`
parsed = JSON.parse(stripJsonComments(stripBom(content)))
   catch -> `Failed to parse models.json: ${message}\n\nFile: ${path}`
if !validateModelsConfig.Check(parsed):
   errors = Errors(parsed).map(e => `  - ${formatValidationPath(e)}: ${e.message}`).join("\n") || "Unknown schema error"
   -> `Invalid models.json schema:\n${errors}\n\nFile: ${path}`
providers = Map：逐 [providerId, provider] -> deepFreeze(structuredClone(provider))
```

三个错误前缀**逐字**：`Failed to load models.json: `、`Failed to parse models.json: `、`Invalid models.json schema:\n`；都以 `\n\nFile: ${path}` 结尾。

`stripJsonComments`（`utils/json.ts`）**逐字实现**：
```ts
input.replace(/"(?:\\.|[^"\\])*"|\/\/[^\n]*/g, m => (m[0] === '"' ? m : ""))
     .replace(/"(?:\\.|[^"\\])*"|,(\s*[}\]])/g, (m, tail) => tail ?? (m[0] === '"' ? m : ""));
```
即：删除 `//` 行注释（**不**支持 `/* */`），删除 `,` 后紧跟可选空白再 `}`/`]` 的尾逗号，字符串内的 `//` 与 `,}` 不受影响。

`getProvider(id)` / `getProviderIds()`（插入顺序）/ `getError()`。

**JSONC 细节**：`models.json` 使用上述 `stripJsonComments`，**不是**完整 JSON5/JSONC（无块注释、无单引号、无无引号键）。

### 2.19 `provider-composer.ts`（737 行）

职责：把「内置 provider（kiana-ai）→ models.json → 扩展注册 → models.json `modelOverrides`」四层合成为一个 `Provider` 对象；并处理 apiKey / OAuth 认证组合、请求头解析、图片与分类器实现分派。**本文件不读凭据**（`composeModelProvider` 注释明确 "without reading credentials"）。

#### 2.19.1 导出类型（逐字）

```ts
export interface ExtensionOAuthConfig {
	name: string;
	/** Whether access through this auth method is backed by a provider subscription. */
	isSubscription?: boolean;
	/** @deprecated Retained for extension source compatibility; ignored by canonical auth flows. */
	usesCallbackServer?: boolean;
	login(callbacks: OAuthLoginCallbacks): Promise<OAuthCredentials>;
	refreshToken(credentials: OAuthCredentials, signal: AbortSignal): Promise<OAuthCredentials>;
	getApiKey(credentials: OAuthCredentials): string;
	modifyModels?(models: Model<Api>[], credentials: OAuthCredentials): Model<Api>[];
}

interface ProviderModelConfigBase {
	id: string; name: string; api?: string; baseUrl?: string;
	input: ("text" | "image")[];
	inputLimits?: AnyModel["inputLimits"];
	cost: AnyModel["cost"];
	headers?: Record<string, string>;
}

export interface ProviderChatModelConfig extends ProviderModelConfigBase {
	type?: "chat";
	api?: Api;
	reasoning: boolean;
	thinkingLevelMap?: Model<Api>["thinkingLevelMap"];
	promptCache?: Model<Api>["promptCache"];
	contextWindow: number;
	maxTokens: number;
	samplingParams?: Model<Api>["samplingParams"];
	samplingParamsByThinkingLevel?: Model<Api>["samplingParamsByThinkingLevel"];
	compat?: Model<Api>["compat"];
}

export interface ProviderImageModelConfig extends ProviderModelConfigBase {
	type: "image";
	api?: ImageApi;
	output: ("text" | "image")[];
}

export interface ProviderClassifierModelConfig extends ProviderModelConfigBase {
	type: "classifier";
	api?: ClassifierApi;
	contextWindow: number;
	compat?: ClassifierModel<ClassifierApi>["compat"];
}

export type ProviderModelConfig = ProviderChatModelConfig | ProviderImageModelConfig | ProviderClassifierModelConfig;

/** Input type for the extension registerProvider API. */
export interface ProviderConfigInput {
	name?: string;
	baseUrl?: string;
	apiKey?: string;
	api?: Api;
	streamSimple?: (model: Model<Api>, context: TranscriptContext, options?: SimpleStreamOptions) => AssistantMessageEventStream;
	images?: Partial<Record<ImageApi, ProviderImages>>;
	classifiers?: Partial<Record<ClassifierApi, ProviderClassifier>>;
	headers?: Record<string, string>;
	authHeader?: boolean;
	oauth?: ExtensionOAuthConfig;
	models?: ProviderModelConfig[];
	refreshModels?(context: RefreshModelsContext): Promise<ProviderModelConfig[]>;
}

export type AuthStatus = {
	configured: boolean;
	source?: "stored" | "runtime" | "environment" | "fallback" | "models_json_key" | "models_json_command";
	label?: string;
};

export const clearApiKeyCache = clearConfigValueCache;   // 与 resolve-config-value 的缓存共享

export interface CompatibilityRequestConfig { headers?: ProviderHeaders; authHeader: boolean; }
```

#### 2.19.2 合并辅助（逐字语义）

- `getAllProviderModels(provider)`：`provider ? (provider.getAllModels?.() ?? provider.getModels()) : []`。即优先 `getAllModels`（含非 chat），否则 `getModels`。
- `mergeCompat(base, override)`：`override` 为假值返回 `base`；否则 `merged = {...base, ...override}`，再对键 `["openRouterRouting","vercelGatewayRouting","chatTemplateKwargs","chatTemplateArgs"]` 逐一看：若 base 侧或 override 侧任一为「object 且非 null」，则 `merged[key] = {...baseValue, ...overrideValue}`（**一层深合并**，不是递归）。
- `mergeInputLimits(base, override)`：`override` 假值返回 `base`；结果 `{...base, ...override, images: override.images ? {...base?.images, ...override.images, resize: override.images.resize ? {...base?.images?.resize, ...override.images.resize} : base?.images?.resize} : base?.images}`。注意 `resize` 只深合一层，其余 images 字段是浅覆盖。
- `mergeSamplingParamsByThinkingLevel(base, override)`：`override` 假值返回 `base`；`merged = {...base}`；按固定顺序 `off, minimal, low, medium, high, xhigh, max` 遍历，若 `override[level]` 存在则 `merged[level] = {...base?.[level], ...params}`。**顺序固定**，未列出的思维等级保持 base。
- `applyModelOverride(model, override)`：浅拷贝 model 后逐字段：
  - `name: override.name ?? model.name`
  - `reasoning: override.reasoning ?? model.reasoning`
  - `thinkingLevelMap: override.thinkingLevelMap ? {...model.thinkingLevelMap, ...override.thinkingLevelMap} : model.thinkingLevelMap`（**浅合并**；`null` 值会覆盖为 null，用于关闭某等级）
  - `input: override.input ?? model.input`
  - `inputLimits: mergeInputLimits(...)`；`cost`：逐 5 字段 `input/output/cacheRead/cacheWrite/tiers` 各自 `override.cost.X ?? model.cost.X`；`promptCache` 浅合并；`contextWindow`、`maxTokens` 覆盖；`samplingParams` 浅合并；`samplingParamsByThinkingLevel` 用上面的合并；`compat: mergeCompat`。
  - 未提及字段（`model.provider/api/id/baseUrl/headers/type/output` 等）原样保留。

#### 2.19.3 `modelFromJson(providerId, definition, providerConfig, defaults)`

从 models.json 的 model 定义构造 `Model<Api>`：
- `api = definition.api ?? providerConfig.api ?? defaults?.api`；若空 → 抛
  `Provider ${providerId}, model ${definition.id}: no "api" specified. Set at provider or model level.` 【逐字】
- `baseUrl = definition.baseUrl ?? providerConfig.baseUrl ?? defaults?.baseUrl`；若空 → 抛
  `Provider ${providerId}: "baseUrl" is required when defining custom models.`
- `definition.contextWindow !== undefined && <= 0` → `Provider ${providerId}, model ${definition.id}: invalid contextWindow`
- `definition.maxTokens !== undefined && <= 0` → `Provider ${providerId}, model ${definition.id}: invalid maxTokens`
- 返回对象固定字段：`id: definition.id`、`name: definition.name ?? definition.id`、`api`、`provider: providerId`、`baseUrl`、`reasoning: definition.reasoning ?? false`、`thinkingLevelMap`、`input: definition.input ?? ["text"]`、`inputLimits`、`cost: definition.cost ?? {input:0,output:0,cacheRead:0,cacheWrite:0}`（无 tiers）、`promptCache`、`contextWindow: definition.contextWindow ?? 128000`、`maxTokens: definition.maxTokens ?? 16384`、`samplingParams`、`samplingParamsByThinkingLevel`、`headers: undefined`、`compat: mergeCompat(providerConfig.compat, definition.compat)`。

#### 2.19.4 默认模型查找

- `findModelDefaults(models, modelId, api?)`：仅 chat 模型；依次（短路）
  1. `id === modelId` 的第一个；
  2. 若给定 api：`api === api` 的第一个；
  3. `api === "openai-completions"` 的第一个；
  4. `chatModels[0]`。
- `findExtensionModelDefaults(models, definition)`：`type = definition.type ?? "chat"`；候选为同 type；顺序同上，但第 3 步仅当 `type === "chat"` 才找 `openai-completions`。

#### 2.19.5 `extensionModelFromDefinition(providerId, models, config, definition)`

- `type = definition.type ?? "chat"`；`defaults = findExtensionModelDefaults(...)`。
- `api = definition.api ?? (type === "chat" ? config.api : undefined) ?? defaults?.api`；空 → 抛
  `Provider ${providerId}, model ${definition.id}: no "api" specified. Set it at model level${type === "chat" ? " or provider level" : ""}.`
  （chat 时末尾为 ` or provider level`；image/classifier 时**没有**该后缀，注意句号前无空格拼接结果 `...model level.`）
- `baseUrl = definition.baseUrl ?? config.baseUrl ?? defaults?.baseUrl`；空 → 同上 baseUrl 错误。
- 按 type 返回 `{...definition, api, provider: providerId, baseUrl, headers: undefined}`（definition 其余字段全保留；因此 image 保留 `output`，classifier 保留 `contextWindow`）。

#### 2.19.6 `applyModelsJson(providerId, baseModels, config)`

- `config` 为 undefined → `[...baseModels]`。
- `config.oauth && !config.baseUrl` → 抛 `Provider ${providerId}: "baseUrl" is required when "oauth" is set.`
- `hasOverrides = config.modelOverrides && Object.keys(...).length > 0`。
- 若 **全部** 满足：`!config.models?.length && !config.baseUrl && !config.headers && !config.compat && !hasOverrides && !config.apiKey && !config.oauth && config.authHeader === undefined` → 抛
  `Provider ${providerId}: must specify "baseUrl", "headers", "compat", "modelOverrides", or "models".`
  （注意：仅设置 `authHeader` 也视为「有配置」；错误消息不列 `apiKey`/`oauth`/`authHeader`）
- 对每个 base model 生成新对象：`baseUrl = config.oauth === "radius" ? model.baseUrl : (config.baseUrl ?? model.baseUrl)`（**radius OAuth 时不覆盖 baseUrl**）；chat → 额外 `compat: mergeCompat(model.compat, config.compat)`；classifier 且有 `config.compat` → `compat: {...model.compat, ...config.compat}`（**浅合并**，与 chat 的嵌套合并不同）；其它（image）只改 baseUrl。
- 遍历 `config.models ?? []`：`existingIndex = models.findIndex(m => isModelType(m,"chat") && m.id === definition.id)`；`defaults = findModelDefaults(models, definition.id, definition.api ?? config.api)`；`model = modelFromJson(...)`；`existingIndex >= 0` 则**替换该位置**（保持顺序），否则 push。
- 返回 `models`（新数组）。

#### 2.19.7 `applyExtension(providerId, models, config)`

- `config` undefined → `[...models]`。
- 无 `config.models`：若有 `config.baseUrl` 则全部模型 `{...model, baseUrl: config.baseUrl}`（**硬覆盖**），否则浅拷贝数组。
- 有 `config.models`：**完全丢弃**传入的 models，返回 `config.models.map(definition => extensionModelFromDefinition(...))`（扩展定义的模型列表是权威的）。

#### 2.19.8 OAuth 适配 `adaptOAuth(config)`

返回 `OAuthAuth`：
- `name`、`isSubscription` 原样。
- `login(callbacks)`：调用 `config.login({...})`，把 kiana-ai 的 `OAuthLoginCallbacks` 适配为扩展回调：
  - `onAuth: info => callbacks.notify({type:"auth_url", ...info})`
  - `onDeviceCode: info => callbacks.notify({type:"device_code", ...info})`
  - `onPrompt: prompt => callbacks.prompt({type:"text", ...prompt})`
  - `onProgress: message => callbacks.notify({type:"progress", message})`
  - `onManualCodeInput: () => callbacks.prompt({type:"manual_code", message:"Paste the authorization code"})` 【逐字】
  - `onSelect: prompt => callbacks.prompt({type:"select", ...prompt})`
  - `signal: callbacks.signal`
  返回 `{...credential, type:"oauth"}`。
- `refresh: async (credential, signal) => ({...(await config.refreshToken(credential, signal)), type:"oauth"})`
- `toAuth: async credential => ({apiKey: config.getApiKey(credential)})`
- `usesCallbackServer` 被**忽略**（不参与任何逻辑）。

#### 2.19.9 `withConfiguredAuth(auth, headers, authHeader)`

```
mergedHeaders = (auth.headers || headers) ? {...auth.headers, ...headers} : undefined
if authHeader:
    if !auth.apiKey -> throw Error("authHeader requires a resolved API key")   // 逐字
    mergedHeaders = {...mergedHeaders, Authorization: `Bearer ${auth.apiKey}`}
return {...auth, headers: mergedHeaders}
```
注意：`Authorization` 是**大写 A** 且会被显式覆盖已有同名键。

#### 2.19.10 配置来源辅助

- `configuredApiKey(config, extension)`：`extension?.apiKey ?? config?.apiKey`（**扩展优先**）。
- `configuredHeaders(config, extension)`：两者皆无 → undefined；否则 `{...config?.headers, ...extension?.headers}`（扩展键覆盖 config 同名键）。
- `configContextEnv(values, ctx, explicit?)`：`env = {...explicit}`；对 `values.flatMap(getConfigValueEnvVarNames)` 去重（`new Set`，**保持首次出现顺序**）后逐个：若 `env[name] !== undefined` 跳过；否则 `await ctx.env(name)`，非 undefined 则写入。返回 `Object.keys(env).length > 0 ? env : undefined`。

#### 2.19.11 `composeApiKeyAuth(providerId, base, config, extension)`

- `inherited = base?.auth.apiKey`；`rawKey = configuredApiKey(...)`；`oauth = extension?.oauth ?? base?.auth.oauth`。
- **若 `!inherited && rawKey === undefined && oauth`** → 返回 `undefined`（纯 OAuth provider 不伪造 apiKey 登录方式）。
- `rawHeaders = configuredHeaders(...)`；`authHeader = extension?.authHeader ?? config?.authHeader ?? false`。
- 返回 `ApiKeyAuth`：
  - `name: inherited?.name ?? "API key"` 【逐字】
  - `login: inherited?.login ?? (async interaction => ({type:"api_key", key: await interaction.prompt({type:"secret", message:"Enter API key"})}))` 【逐字 `Enter API key`】
  - `check(input)`：
    1. 有 `input.credential`：`inherited?.check` 存在则直接委托；否则 `credential.key` 非空 → `{type:"api_key", source:"stored credential"}`；否则 `resolved = await inherited?.resolve(input)`，有则 `{type:"api_key", source: resolved.source}`，无则 undefined。
    2. 否则若 `rawKey !== undefined`：`isCommandConfigValue(rawKey)` → `{type:"api_key", source:"configured API key"}`；否则取 `getConfigValueEnvVarNames(rawKey)` 逐个 `await input.ctx.env(name)`，**任一** undefined → 返回 undefined；全部存在 → `{type:"api_key", source:"configured API key"}`。
    3. 否则 `inherited?.check` 委托；再 `inherited?.resolve`，有则 `{type:"api_key", source: resolved.source}`，无则 undefined。
  - `resolve(input)`：
    1. 有 credential：`inherited` 存在 → 委托 `inherited.resolve(input)`；否则 `credential.key` → `{auth:{apiKey: credential.key}, env: credential.env, source:"stored credential"}`；否则 undefined。
    2. 否则若有 rawKey：`env = await configContextEnv([rawKey], input.ctx)`；`key = resolveConfigValueOrThrow(rawKey, \`API key for provider "${providerId}"\`, env)`；`inherited` 存在 → 用 `{...input, credential:{type:"api_key", key}}` 委托 `inherited.resolve`；否则 `{auth:{apiKey:key}, source:"configured API key"}`。
    3. 否则 `inherited?.resolve(input)`。
    4. `!result` → undefined。否则 `explicitEnv = {...input.credential?.env, ...result.env}`；`headerEnv = await configContextEnv(Object.values(rawHeaders ?? {}), input.ctx, explicitEnv)`；`headers = resolveHeadersOrThrow(rawHeaders, \`provider "${providerId}"\`, headerEnv)`；返回 `{...result, auth: withConfiguredAuth(result.auth, headers, authHeader)}`。

#### 2.19.12 `composeOAuthAuth(providerId, base, config, extension)`

- `oauth = extension?.oauth ? adaptOAuth(extension.oauth) : base?.auth.oauth`；无则 undefined。
- 返回 `{...oauth, toAuth: async credential => { const auth = await oauth.toAuth(credential); const env = credential.env; const headers = resolveHeadersOrThrow(rawHeaders, \`provider "${providerId}"\`, typeof env === "object" && env !== null ? env : undefined); return withConfiguredAuth(auth, headers, authHeader); }}`。
- 注意 `toAuth` **整体替换**（不是包装后调用 base.toAuth），且只在 toAuth 时解析头。

#### 2.19.13 `rawModelHeaders(model, config, extension)`

- `chatDefinition = isModelType(model,"chat") ? config?.models?.find(e => e.id === model.id) : undefined`
- `extensionModel = extension?.models?.find(e => (e.type ?? "chat") === (model.type ?? "chat") && e.id === model.id)`（**按 type+id 匹配**，避免跨类型 id 碰撞共享头）
- `headers = {...(chat 且 config.modelOverrides[model.id]?.headers), ...chatDefinition?.headers, ...extensionModel?.headers}`
- 返回 `Object.keys(headers).length > 0 ? headers : undefined`。

#### 2.19.14 导出函数

- `validateExtensionProvider(providerId, base, modelsConfig, extension): void`
  - `extension.streamSimple && !extension.api` → 抛 `Provider ${providerId}: "api" is required when registering streamSimple.` 【逐字】
  - 随后执行 `applyExtension(providerId, applyModelsJson(providerId, getAllProviderModels(base), modelsConfig), extension)`（仅为触发校验，丢弃结果）。

- `composeModelProvider(providerId, base, modelConfig, extension): Provider`
  - `config = modelConfig.getProvider(providerId)`。
  - 闭包状态：`extensionOAuthCredential?: OAuthCredentials`、`refreshedExtensionModels?: ProviderConfigInput["models"]`；`currentExtension()` 在有刷新模型时返回 `{...extension, models: refreshedExtensionModels}`。
  - `getAllModels()`：
    1. `models = applyExtension(providerId, applyModelsJson(providerId, getAllProviderModels(base), config), currentExtension())`
    2. 若 `extensionOAuthCredential && extension?.oauth?.modifyModels`：`models = [...modifyModels(chat 模型, credential), ...非 chat 模型]`（chat 在前，非 chat 在后；**顺序被重排**）
    3. 对每个 model：`override = config?.modelOverrides?.[model.id]`；仅 chat 且有 override 时 `applyModelOverride`。
  - **构造时立即调用 `getAllModels()`** 以尽早暴露结构错误。
  - `apiKey = composeApiKeyAuth(...)`；`oauth = composeOAuthAuth(...)`；两者皆空 → 抛 `Provider ${providerId}: no authentication method configured.` 【逐字】
  - `supportsBaseApi(model)`：`base?.getModels().some(e => e.api === model.api) ?? false`（基于 **getModels**，非 getAllModels）。
  - `streamWith(model, context, options, simple)`：返回 `lazyStream(model, async () => {...})`；分派顺序：
    1. `extension?.streamSimple && model.api === extension.api` → `extension.streamSimple(model, context, options as SimpleStreamOptions)`（**simple 与完整流共用同一实现**）
    2. `base && supportsBaseApi(model)` → `simple ? base.streamSimple(...) : base.stream(...)`
    3. `api = getApiProvider(model.api)`；无 → 抛 `No API provider registered for api: ${model.api}` 【逐字】
    4. `simple ? api.streamSimple(...) : api.stream(...)`
  - `provider` 字段：
    - `id: providerId`
    - `name: extension?.name ?? config?.name ?? base?.name ?? extension?.oauth?.name ?? providerId`（**优先级固定**）
    - `baseUrl: extension?.baseUrl ?? config?.baseUrl ?? base?.baseUrl`
    - `headers: base?.headers`（**仅内置**；models.json/扩展的 headers 走请求级解析）
    - `auth: {...(apiKey ? {apiKey} : {}), ...(oauth ? {oauth} : {})}`
    - `getModels: () => getAllModels().filter(m => isModelType(m,"chat"))`
    - `getAllModels`
    - `refreshModels`：仅当 `base?.refreshModels || extension?.refreshModels || extension?.oauth?.modifyModels` 才存在，实现：
      ```
      await base?.refreshModels?.(context)
      refreshed = extension?.refreshModels ? await extension.refreshModels(context) : undefined
      if context.signal.aborted -> return
      oauthCredential = context.credential?.type === "oauth" ? context.credential : undefined
      await context.publish({ update: () => {
          if refreshed:
              applyExtension(providerId, applyModelsJson(providerId, getAllProviderModels(base), config),
                             {...extension, models: refreshed})     // 发布前校验，抛错则不更新
              refreshedExtensionModels = refreshed
          extensionOAuthCredential = oauthCredential
      }})
      ```
      注意：`publish` 的 `update` 在同一回调内既可能抛错（校验失败）又负责赋值；`persist` 未使用。
    - `filterModels` / `filterAllModels`：仅当 base 提供时转发 `(models, credential) => base.filterModels!(models, credential)`。
    - `stream` / `streamSimple`：分别以 `simple=false/true` 调 `streamWith`。
  - 可选能力附加（均为「base 提供 **或** 扩展映射非空」才挂载）：
    - `fetchDeferred` / `cancelDeferred`：base 有则转发。
    - `generateImages`：`implementation = extension?.images?.[model.api]` 优先，其次 `base.generateImages`，否则 `Promise.resolve(imageErrorResult(model, new Error(\`Provider ${providerId} has no image implementation for "${model.api}"\`)))` 【逐字】。
    - `classify`：同理，`extension?.classifiers?.[model.api]` 优先，其次 `base.classify`，否则 `classifierErrorResult(model, new Error(\`Provider ${providerId} has no classifier implementation for "${model.api}"\`))`。
    - 判定条件为 `base?.generateImages || Object.keys(extensionImages ?? {}).length > 0`（classify 类似）。

- `resolveConfiguredModelHeaders(model, config, extension, env?)`：`resolveHeadersOrThrow(rawModelHeaders(...), \`model "${model.provider}/${model.id}"\`, env)`。

- `resolveCompatibilityRequestConfig(model, config, extension): CompatibilityRequestConfig`
  - `configured = resolveHeadersOrThrow({...configuredHeaders(config, extension), ...rawModelHeaders(model, config, extension)}, \`model "${model.provider}/${model.id}"\`)`
  - `headers: (model.headers || configured) ? {...model.headers, ...configured} : undefined`
  - `authHeader: extension?.authHeader ?? config?.authHeader ?? false`
  - 注意合并次序：**provider 级 headers 先，模型级 headers 覆盖**；model.headers 最底。

- `configuredRequestAuthStatus(config, extension): AuthStatus | undefined`
  - `value = configuredApiKey(...)`；undefined → undefined。
  - `isCommandConfigValue(value)` → `{configured:true, source:"models_json_command"}`
  - `names = getConfigValueEnvVarNames(value)`；长度 > 0 → `isConfigValueConfigured(value) ? {configured:true, source:"environment", label: names.join(", ")} : {configured:false}`
  - 否则 → `{configured:true, source: extension?.apiKey !== undefined ? "fallback" : "models_json_key"}`
  该函数是 `AuthStatus.source` 六个取值的唯一产出点（`stored`/`runtime` 由 model-runtime 产出）。

### 2.20 `model-runtime.ts`（1049 行）

`ModelRuntime implements Models`（kiana-ai 接口）：内置 provider + models.json + 原生扩展 provider + 扩展注册 + 虚拟模型 的组合层，负责凭据同步、可用性快照、请求准备（认证 + 头合并 + env）与直接流式调用。

#### 2.20.1 导出类型

```ts
export interface CreateModelRuntimeOptions {
	/** Credential storage. Defaults to the file at authPath. */
	credentials?: CredentialStore;
	authPath?: string;
	modelsPath?: string | null;
	modelsStore?: ModelsStore;
	modelsStorePath?: string;
	/** Allow create() to refresh model catalogs over the network. Defaults to false. */
	allowModelNetwork?: boolean;
	/** Timeout for the create-time network model refresh. */
	modelRefreshTimeoutMs?: number;
	catalogBaseUrl?: string;
	/** Optional caller cancellation for initial cache restoration and availability checks. */
	signal?: AbortSignal;
	/** Skip initial catalog and availability refresh. Static models remain available. */
	refreshOnCreate?: boolean;
}

export interface ModelRuntimeAuthOverrides extends AuthOperationOptions {
	apiKey?: string;
	env?: Record<string, string>;
	/** Require this much remaining OAuth-token validity; defaults to five minutes. */
	minOAuthValidityMs?: number;
}

export type CredentialSynchronizationOperation = "login" | "logout" | "setRuntimeApiKey" | "removeRuntimeApiKey";

export class CredentialSynchronizationError extends Error {
	readonly providerId: string;
	readonly operation: CredentialSynchronizationOperation;
	readonly credential: Credential | undefined;
	// message: `Credential ${operation} committed for ${providerId}, but local synchronization failed`
	// name: "CredentialSynchronizationError"; options 透传给 Error（cause）
}
```

#### 2.20.2 `mergeHeaders(base, override)`

- 两者皆空 → undefined。
- `merged = {...base}`；遍历 `Object.entries(override ?? {})`：先 `lowerName = name.toLowerCase()`，删除 merged 中所有大小写不敏感同名键，再 `merged[name] = value`。
- 语义：override 覆盖且**替换大小写形式**（例如 base 有 `authorization`、override 有 `Authorization`，结果只保留 `Authorization`）；override 内部的键保持其原始插入顺序。

#### 2.20.3 私有状态

```
models: MutableModels                 // createModels({credentials, modelsStore})
credentials: RuntimeCredentials
defaultBuiltins: ReadonlyMap<string, Provider>   // 构造时注入的 provider 列表
builtins: Map<string, Provider>                  // 可在 configureRadiusProviders 中替换
nativeExtensionProviders: Map<string, Provider>
extensionProviders: Map<string, ProviderConfigInput>
virtualModels: Map<providerId, Map<modelId, {model, route}>>
compositionErrors: Map<providerId, string>
modelsPath: string | undefined
modelNetworkEnabled: boolean
config: ModelConfig
snapshot: { all, available, configuredProviders:Set, storedProviders:Set, auth:Map<providerId, AuthCheck|undefined> }
availabilityRefreshSeq = 0
availabilityErrorSeq = 0
providerAvailabilitySeq: Map<providerId, number>
availabilityError: string | undefined
credentialOperations: Map<providerId, Promise<unknown>>
```
初始 snapshot：`{all: [], available: [], configuredProviders: new Set(), storedProviders: new Set(), auth: new Map()}`。

#### 2.20.4 `static async create(options)`

1. `credentials = new RuntimeCredentials(options.credentials ?? DefaultAuthStorage.create(options.authPath))`
2. `modelsPath = options.modelsPath === null ? undefined : (options.modelsPath ?? join(getAgentDir(), "models.json"))`
3. `config = await ModelConfig.load(modelsPath)`
4. `modelsStore = options.modelsStore ?? (modelsPath ? new FileModelsStore(options.modelsStorePath ?? join(dirname(modelsPath), "models-store.json")) : new InMemoryCodingAgentModelsStore())`
5. `builtinModelDataGeneratedAt = builtinProviderCatalog.getBuiltinModelDataGeneratedAt()`
6. 构造 providers：`builtinProviderCatalog.builtinProviders().map(p => p.id === "radius" ? p : p.id === "openrouter" ? withPersonalizedCatalog(p, personalizeOpenRouterCatalog, options.catalogBaseUrl, builtinModelDataGeneratedAt) : withRemoteCatalog(p, options.catalogBaseUrl, builtinModelDataGeneratedAt))`（radius 包装"原样直通"，OpenRouter 用个性化目录）
7. `new ModelRuntime(credentials, config, modelsPath, modelsStore, providers, process.env.KIANA_OFFLINE === undefined)`
   - 构造函数：保存字段；`defaultBuiltins = new Map(providers.map(p => [p.id, p]))`；`builtins` 复制一份；`models = createModels({credentials, modelsStore})`；`rebuildProviders()`。
8. `runtime.configureRadiusProviders()`；`runtime.rebuildProviders()`（构造后重建两次）
9. `refreshFromNetwork = runtime.modelNetworkEnabled && options.allowModelNetwork === true`
   - `controller = refreshFromNetwork && options.modelRefreshTimeoutMs !== undefined ? new AbortController() : undefined`
   - `timeout = controller ? setTimeout(() => controller.abort(), options.modelRefreshTimeoutMs) : undefined`
   - `signal = controller ? (options.signal ? AbortSignal.any([options.signal, controller.signal]) : controller.signal) : options.signal`
10. 若 `options.refreshOnCreate !== false`：`await runtime.refresh({allowNetwork: refreshFromNetwork, signal})`；`finally` 清 timeout。

注意：`KIANA_OFFLINE` 只要**存在**（任意值，包括空串）即禁用网络。

#### 2.20.5 `configureRadiusProviders()`

```
builtins.clear(); 重新填入所有 defaultBuiltins
for providerId of config.getProviderIds():
    config = this.config.getProvider(providerId)
    if config?.oauth !== "radius" || !config.baseUrl: continue
    builtins.set(providerId, radiusProvider({
        id: providerId,
        name: config.name ?? providerId,
        gateway: config.baseUrl.replace(/\/v1\/?$/u, ""),
    }))
```
即 radius 型 provider 用内置 radius 实现替换，baseUrl 去掉结尾的 `/v1` 或 `/v1/`。

#### 2.20.6 provider 集合与重组

- `providerIds()`：`builtins` ∪ `nativeExtensionProviders` ∪ `config.getProviderIds()` ∪ `extensionProviders` ∪ `virtualModels.keys()`（插入顺序，Set 去重）。
- `composeProvider(providerId)`：
  - `base = nativeExtensionProviders.get(id) ?? builtins.get(id)`
  - `extension = extensionProviders.get(id)`
  - **若 `!config.getProvider(id) && !extension`** → 清除 `compositionErrors[id]`，**原样返回 base**（无覆盖层时不重新合成，保证内置认证/流行为精确）
  - 否则 `composeModelProvider(...)`；成功清除错误并返回；失败则 `compositionErrors.set(id, error instanceof Error ? error.message : String(error))` 并**回退返回 base**。
- `recomposeProvider(providerId)`：
  - `provider = composeProvider(id)`
  - `virtual = [...(virtualModels.get(id)?.values() ?? [])].map(e => e.model)`
  - `virtual.length > 0` → `models.setProvider(withVirtualModels(id, provider, virtual))`
  - 否则 `provider` 存在 → `models.setProvider(provider)`
  - 否则 `models.deleteProvider(id)`
  - 返回 `provider`（**不含虚拟模型**）。
- `rebuildProviders()`：`models.clearProviders()`；`compositionErrors.clear()`；对每个 `providerIds()` 调 `recomposeProvider`；`updateModelSnapshot()`。
- `updateModelSnapshot()`：`all = [...models.getModels()]`；`snapshot = {...snapshot, all, available: all.filter(m => snapshot.configuredProviders.has(m.provider))}`（注意 available 由**旧** configuredProviders 推导）。

#### 2.20.7 可用性刷新（三组序号）

- `runAvailabilityRefresh(seq, errorSeq, signal)`：
  ```
  providers = models.getProviders()
  [available, checks, credentials] = await Promise.all([
      models.getAvailable(undefined, {signal}),
      Promise.all(providers.map(async p => [p.id, await models.checkAuth(p.id, {signal})])),
      credentials.list({signal}),
  ])
  if seq !== availabilityRefreshSeq: return                       // 丢弃过期结果
  auth = new Map(checks)
  configuredProviders = new Set(checks.filter(e => e[1] !== undefined).map(e => e[0]))
  snapshot = {all: [...models.getModels()], available: [...available], configuredProviders,
              storedProviders: new Set(credentials.map(e => e.providerId)), auth}
  if errorSeq === availabilityErrorSeq: availabilityError = undefined
  ```
- `queueAvailabilityRefresh(signal?)`：
  ```
  seq = ++availabilityRefreshSeq
  对 providerAvailabilitySeq 中每个 provider 的序号 +1（使单 provider 刷新失效）
  errorSeq = ++availabilityErrorSeq
  effectiveSignal = operationSignal(signal)
  runAvailabilityRefresh(...).catch(error => {
      if (errorSeq === availabilityErrorSeq && !effectiveSignal.aborted)
          availabilityError = message(error)
      throw error })
  ```
- `refreshProviderAvailability(providerId, signal)`：
  ```
  ++availabilityRefreshSeq                                      // 使进行中的全量刷新失效
  providerSeq = (providerAvailabilitySeq.get(id) ?? 0) + 1; set
  errorSeq = ++availabilityErrorSeq
  try {
      [available, auth, credential] = await Promise.all([getAvailable(id,{signal}), checkAuth(id,{signal}), credentials.read(id,{signal})])
      signal.throwIfAborted()
      if providerAvailabilitySeq.get(id) !== providerSeq: return
      复制 configuredProviders/storedProviders/auth
      auth ? (add id + set) : (delete id + delete)
      credential ? add : delete  storedProviders
      all = [...models.getModels()]
      availableById = Map([...snapshot.available.filter(m => m.provider !== id), ...available] → key `${provider}\0${id}`)
      snapshot = {all, available: all.flatMap(m => availableById.get(key) ?? []), ...}
      if errorSeq === availabilityErrorSeq: availabilityError = undefined
  } catch (error) {
      if (providerAvailabilitySeq.get(id) === providerSeq && errorSeq === availabilityErrorSeq && !signal.aborted)
          availabilityError = message(error)
      throw error }
  ```
  注意 available 的**最终顺序以 all 的顺序为准**（先按 key 建 map 再按 all 顺序 flatMap）。

#### 2.20.8 查询方法（全部转发 `this.models` 或 snapshot）

`getProviders()`、`getProvider(id)`、`getModels(providerId?)`、`getModel(providerId, modelId)`、`getModelsOfType(type, providerId?)`、`getModelOfType(type, providerId, modelId)`、`getAllModels(providerId?)`、`getAvailableOfType(type, providerId?, options?)`、`getAllAvailable(providerId?, options?)`、`checkAuth(providerId, options?)` 均直接 `return this.models.X(...)`。

- `getAvailable(providerId?, options?)`：
  - 有 providerId：`errorSeq = ++availabilityErrorSeq`；`await this.models.getAvailable(providerId, options)`；成功且 seq 仍最新 → 清 `availabilityError`；失败且 seq 最新且 `!options?.signal?.aborted` → 记录错误；重新抛出。
  - 无 providerId：`await queueAvailabilityRefresh(options?.signal)`；返回 `this.snapshot.available`。
- `getAvailableSnapshot()`：返回 `this.snapshot.available`（**不**触发刷新）。
- `getError()`：按序收集：`config.getError()`；每个 `compositionErrors` 项 → `` Provider "${providerId}": ${error} ``；`availabilityError` → `Availability refresh: ${error}`；非空则以 `"\n\n"` 连接，否则 undefined。
- `getRegisteredProviderConfig(id)`、`getRegisteredProviderIds()`（`[...new Set([...extensionProviders.keys(), ...nativeExtensionProviders.keys()])]`）、`getRegisteredNativeProvider(id)`。
- `getCompatibilityRequestConfig(model)`（标注 `@internal`）：`resolveCompatibilityRequestConfig(model, config.getProvider(model.provider), extensionProviders.get(model.provider))`。
- `isUsingOAuth(id)`：`snapshot.auth.get(id)?.type === "oauth"`。
- `isUsingSubscription(id)`：`isUsingOAuth(id) && models.getProvider(id)?.auth.oauth?.isSubscription === true`。
- `hasConfiguredAuth(id)`：`snapshot.configuredProviders.has(id)`。

#### 2.20.9 `getAuth(providerOrModel, overrides?)`（重载）

- 字符串 providerId → 直接 `this.models.getAuth(providerId, overrides)`。
- 模型对象 → `resolution = await this.models.getAuth(model, overrides)`；无 → undefined；否则计算
  `configuredHeaders = resolveConfiguredModelHeaders(model, config.getProvider(model.provider), extensionProviders.get(model.provider), {...(resolution.env ?? {}), ...(overrides.env ?? {})})`
  返回 `{...resolution, auth: {...resolution.auth, headers: mergeHeaders(resolution.auth.headers, configuredHeaders)}}`。

#### 2.20.10 凭据操作串行化

`enqueueCredentialOperation(providerId, signal, task)`：
```
previous = credentialOperations.get(id) ?? Promise.resolve()
started = 新 Promise，保存 markStarted
operation = (async () => { await previous.catch(()=>{}); signal.throwIfAborted(); markStarted(); return task() })()
tail = operation.catch(()=>{})
credentialOperations.set(id, tail)
void tail.then(() => { if (credentialOperations.get(id) === tail) credentialOperations.delete(id) })
return raceWithAbortSignal(started, signal).then(() => operation)
```
语义：同一 provider 的操作串行；调用方在**轮到自己开始**时即被 resolve（`started`），但若 signal 在此之前中止，`raceWithAbortSignal` 立即 reject 而任务仍在链上。

`synchronizeCredentialState(providerId, operation, credential, signal)`：
```
try {
    signal.throwIfAborted()
    recomposeProvider(providerId)
    compositionError = compositionErrors.get(id); if (compositionError) throw new Error(compositionError)
    result = await models.refresh({allowNetwork: false, providers: [providerId], signal})
    if (result.aborted) signal.throwIfAborted()
    refreshError = result.errors.get(providerId); if (refreshError) throw refreshError
    updateModelSnapshot()
    await refreshProviderAvailability(providerId, signal)
} catch (cause) {
    throw new CredentialSynchronizationError(providerId, operation, credential, {cause})
}
```

- `setRuntimeApiKey(providerId, apiKey, options?)`：`signal = operationSignal(options.signal)`；入队：`credentials.setRuntimeApiKey(id, apiKey)`；`synchronizeCredentialState(id, "setRuntimeApiKey", {type:"api_key", key: apiKey}, signal)`。
- `removeRuntimeApiKey(providerId, options?)`：`credentials.removeRuntimeApiKey(id)`；同步 `"removeRuntimeApiKey"`, credential `undefined`。
- `listCredentials(options?)`：`this.credentials.list(options)`。
- `getProviderAuthStatus(providerId): AuthStatus`（**顺序固定**）：
  1. `credentials.hasRuntimeApiKey(id)` → `{configured:true, source:"runtime"}`
  2. `snapshot.storedProviders.has(id)` → `{configured:true, source:"stored"}`
  3. `configuredRequestAuthStatus(config.getProvider(id), extensionProviders.get(id))` 非空 → 返回它（`models_json_command` / `environment` / `fallback` / `models_json_key` 或 `{configured:false}`）
  4. `snapshot.auth.get(id)` 存在 → `{configured:true, source:"environment", label: check.source}`
  5. 否则 `{configured:false}`

#### 2.20.11 `prepareRequest(model, options)`（private）

```
provider = models.getProvider(model.provider)
if !provider -> throw new ModelsError("provider", `Unknown provider: ${model.provider}`)      // 逐字
resolution = await getAuth(model, {apiKey: options?.apiKey, env: options?.env, signal: options?.signal})
if !resolution -> throw new ModelsError("auth", `Provider is not configured: ${model.provider}`) // 逐字
{transformHeaders, ...rawProviderOptions} = options ?? {}
requestModel = resolution.auth.baseUrl ? {...model, baseUrl: resolution.auth.baseUrl} : model
sessionId = ("sessionId" in providerOptions && typeof providerOptions.sessionId === "string") ? ... : undefined
headers = mergeHeaders(mergeHeaders(getOpenCodeSessionHeaders(requestModel, sessionId), resolution.auth.headers),
                       providerOptions.headers)
if transformHeaders: headers = await transformHeaders(headers ?? {})
env = (resolution.env || providerOptions.env) ? {...resolution.env, ...providerOptions.env} : undefined
return {provider, model: requestModel, options: {...providerOptions,
        apiKey: providerOptions.apiKey ?? resolution.auth.apiKey, headers, env}}
```
头合并次序（**逐字**）：OpenCode session 头 → auth 头 → 请求 options 头 → `transformHeaders`。`apiKey` 优先取调用方的。

#### 2.20.12 流式接口

- `stream(model, context, options?)`：`transcript = normalizeContext(context)`；返回 `lazyStream(model, async () => { assertChatModel(model); prepared = await prepareRequest(...); return prepared.provider.stream(prepared.model, transcript, prepared.options) })`。
- `complete(...)`：`stream(...).result()`。
- `streamSimple(model, context, options?)`：`transcript = normalizeContext(context)`。
  - 若 `isVirtualModel(model)`：返回 `lazyStream(model, async () => {...})`：
    ```
    route = await resolveModel(model, transcript.messages, {reason:"direct", thinkingLevel: options?.reasoning ?? "off", signal: options?.signal})
    limit = route.model.maxTokens
    maxTokens = options?.maxTokens && limit > 0 ? Math.min(options.maxTokens, limit) : options?.maxTokens
    reasoning = route.thinkingLevel === "off" ? undefined : route.thinkingLevel
    {apiKey, headers, env, ...rest} = options ?? {}
    auth = route.model.provider === model.provider ? {apiKey, headers, env} : {}     // 跨 provider 丢弃调用方凭据
    return this.streamSimple(route.model, context, {...rest, ...auth, maxTokens, reasoning})
    ```
  - 否则：`lazyStream` → `assertChatModel`；`prepareRequest`；`prepared.provider.streamSimple(prepared.model, transcript, prepared.options)`。
- `completeSimple(...)`：`.result()`。
- `streamDeferred(model, handle, options?)`：`lazyStream` → `assertChatModel` → prepare；无 `provider.fetchDeferred` → `new ModelsError("provider", \`Provider ${model.provider} does not support deferred responses\`)` 【逐字】；否则调用。
- `fetchDeferred(...)`：`streamDeferred(...).result()`。
- `cancelDeferred(model, handle, options?)`：`assertChatModel`；prepare；无 `cancelDeferred` → 同一 `does not support deferred responses` 错误；否则 await。
- `generateImages(model, context, options?)`：**整体 try/catch**：`assertImageModel`；prepare；无 `generateImages` → `new ModelsError("provider", \`Provider ${model.provider} does not support image generation\`)`；否则 await。catch 返回 `imageErrorResult(model, error, options?.signal?.aborted)`。
- `classify(model, context, options?)`：try/catch：`assertClassifierModel`；`assertClassifierInputSupported(model, context)`；prepare；无 `classify` → `... does not support classification`；否则 await。catch 返回 `classifierErrorResult(model, error, options?.signal?.aborted)`。
- `login(providerId, type, interaction, options?)`：`signal = operationSignal(interaction.signal)`；入队：`credential = await models.login(providerId, type, {...interaction, signal}, options)`；`synchronizeCredentialState(providerId, "login", credential, signal)`；返回 credential。
- `logout(providerId, options?)`：入队：`await models.logout(providerId, {signal})`；同步 `"logout"`, undefined。

#### 2.20.13 `refresh(options)`

```
config = await ModelConfig.load(modelsPath)          // 每次重读 models.json
configureRadiusProviders()
if options.providers:
    for providerId of new Set(options.providers): recomposeProvider(providerId)
    updateModelSnapshot()
else:
    rebuildProviders()
refreshOptions = {...options, allowNetwork: options.allowNetwork ?? this.modelNetworkEnabled}
result = (await models.refresh(refreshOptions)) ?? {aborted: refreshOptions.signal?.aborted ?? false, errors: new Map()}
errors = new Map(result.errors)
updateModelSnapshot()
if options.providers:
    await Promise.all([...new Set(options.providers)].map(async id => {
        try { await refreshProviderAvailability(id, operationSignal(options.signal)) }
        catch (error) { if (!options.signal?.aborted) errors.set(id, error instanceof Error ? error : new Error(String(error))) }
    }))
else:
    try { await queueAvailabilityRefresh(options.signal) } catch { /* 忽略；模型仍可用 */ }
return {aborted: result.aborted || (options.signal?.aborted ?? false), errors}
```

#### 2.20.14 注册 / 注销

- `registerNativeProvider(provider)`：
  - `!provider.id.trim()` → 抛 `Provider id must not be empty.` 【逐字】
  - `extensionProviders.delete(id)`；`nativeExtensionProviders.set(id, provider)`；`recomposeProvider`；`updateModelSnapshot`；
  - `markProvisionallyConfigured(id, configuredRequestAuthStatus(config.getProvider(id), undefined), provider.auth.oauth && !provider.auth.apiKey ? "oauth" : "api_key")`
  - `void this.refresh({allowNetwork: false})`（**不 await**）
- `markProvisionallyConfigured(providerId, configuredStatus, type)`（private）：
  - 若 `!snapshot.storedProviders.has(id) && !configuredStatus?.configured` → 返回。
  - `configuredProviders = new Set(snapshot.configuredProviders).add(id)`；`auth = new Map(snapshot.auth)`；**仅当 `!auth.get(id)`** 时 `auth.set(id, {type, source:"configured provider"})`（不覆盖真实结果）。
  - `snapshot = {...snapshot, auth, configuredProviders, available: snapshot.all.filter(m => configuredProviders.has(m.provider))}`。
- `registerProvider(providerId, config)`：
  1. **先** `validateExtensionProvider(providerId, builtins.get(providerId), config.getProvider(providerId), config)`（失败抛出且**不**改动已存配置）
  2. `nativeExtensionProviders.delete(id)`
  3. 重注册合并：`effective = {...previous}`；对 `Object.entries(config)` 中 `value !== undefined` 的键覆盖（undefined 键保留旧值）
  4. `extensionProviders.set(id, effective)`；`recomposeProvider`；`updateModelSnapshot`；
  5. `markProvisionallyConfigured(id, configuredRequestAuthStatus(config.getProvider(id), effective), effective.oauth && !effective.apiKey ? "oauth" : "api_key")`
  6. `void this.refresh({allowNetwork:false})`
- `unregisterProvider(providerId)`：删除两个 map 项；`recomposeProvider`；`updateModelSnapshot`；`void refresh({allowNetwork:false})`。
- `registerVirtualModel(definition)`：
  - `!provider.trim() || !id.trim()` → 抛 `Virtual model provider and id must not be empty.` 【逐字】
  - `existing = models.getModel(providerId, id)`；存在且非虚拟 → 抛 `Virtual model ${providerId}/${id} conflicts with a physical model.` 【逐字】
  - 写入 `virtualModels`：`{model: createVirtualModel(definition), route: request => definition.route(request)}`（**包一层再调**）
  - 若 `!recomposeProvider(providerId)` 且 `!snapshot.configuredProviders.has(providerId)`：`auth.set(id, {type:"api_key", source:"virtual"})` 与 `configuredProviders.add(id)`（纯虚拟 provider 无需凭据，立即视为已配置，供会话恢复使用）。
  - `updateModelSnapshot()`；`void refresh({allowNetwork:false})`。
- `unregisterVirtualModel(providerId, id)`：`models?.delete(id)` 返回 false 则**直接 return**（不触发任何刷新）；size 为 0 时删 provider 项；`recomposeProvider`；`updateModelSnapshot`；`void refresh({allowNetwork:false})`。

#### 2.20.15 `resolveModel(model, messages, options)`

```
name = `Virtual model ${model.provider}/${model.id}`
virtual = virtualModels.get(model.provider)?.get(model.id)
if !virtual -> throw new Error(`${name} is not registered.`)
{failed, ...request} = options
latest = findLatestResponse(messages)
previousModel = latest && getPhysicalModel(latest.provider, latest.model)
failedModel = failed && getPhysicalModel(failed.provider, failed.model)
route = await virtual.route({
    ...request, model,
    previous: previousModel && {model: previousModel, thinkingLevel: latest?.thinkingLevel},
    failed: failedModel && failed && {model: failedModel, thinkingLevel: failed.thinkingLevel, message: failed},
    messages,
})
target = getPhysicalModel(route.model.provider, route.model.id)
routed = `${name} routed to ${route.model.provider}/${route.model.id}`
if !target -> throw new Error(`${routed}, which is not a physical model.`)
if !hasConfiguredAuth(target.provider) -> throw new Error(`${routed}, which has no credentials.`)
return {model: target, thinkingLevel: clampThinkingLevel(target, route.thinkingLevel), state: route.state}
```
注意：路由失败消息中的虚拟模型名会让 `getPhysicalModel` 返回 undefined，因此不会误报 failed。

`getPhysicalModel(providerId, modelId)`：`models.getModel(...)` 且 `!isVirtualModel(model)`，否则 undefined。

### 2.21 `model-resolver.ts`（689 行）

模型引用解析（`provider/id[:thinking]`）、scope 模式展开、CLI 解析、初始模型选择与会话恢复。

#### 2.21.1 `defaultModelPerProvider`（逐字，导出常量）

```ts
export const defaultModelPerProvider: Partial<Record<KnownProvider, string>> = {
	"amazon-bedrock": "us.anthropic.claude-opus-4-6-v1",
	"ant-ling": "Ring-2.6-1T",
	anthropic: "claude-opus-4-8",
	openai: "gpt-5.5",
	azure: "gpt-5.4",
	"openai-codex": "gpt-6.1-sol",
	radius: "balanced",
	nvidia: "nvidia/nemotron-3.5-super-vl-120b-a12b",
	deepseek: "deepseek-v4-pro",
	google: "gemini-3.1-pro-preview",
	"google-vertex": "gemini-3.1-pro-preview",
	"github-copilot": "gpt-6.1-sol",
	openrouter: "moonshotai/kimi-k2.6",
	"vercel-ai-gateway": "zai/glm-5.1",
	xai: "grok-4.7",
	groq: "openai/gpt-oss-120b",
	cerebras: "gpt-oss-120b",
	zai: "glm-5.3",
	"zai-coding-cn": "glm-5.3",
	mistral: "devstral-medium-latest",
	minimax: "MiniMax-M2.7",
	"minimax-cn": "MiniMax-M2.7",
	moonshotai: "kimi-k2.6",
	"moonshotai-cn": "kimi-k2.6",
	huggingface: "moonshotai/Kimi-K2.6",
	fireworks: "accounts/fireworks/models/kimi-k3",
	together: "moonshotai/Kimi-K3",
	baseten: "zai-org/GLM-5.2",
	opencode: "kimi-k2.6",
	"opencode-go": "kimi-k3",
	"kimi-coding": "kimi-for-coding",
	meta: "muse-spark-1.3",
	"cloudflare-workers-ai": "@cf/moonshotai/kimi-k2.6",
	"cloudflare-ai-gateway": "workers-ai/@cf/moonshotai/kimi-k2.6",
	"qwen-token-plan": "qwen3.7-max",
	"qwen-token-plan-cn": "qwen3.7-max",
	"qwen-token-plan-individual": "qwen3.8-max",
	xiaomi: "mimo-v2.6-pro",
	"xiaomi-token-plan-cn": "mimo-v2.6-pro",
	"xiaomi-token-plan-ams": "mimo-v2.6-pro",
	"xiaomi-token-plan-sgp": "mimo-v2.6-pro",
};
```

> 安全说明：`qwen3.7-max`、`qwen3.8-max`、`mimo-v2.6-pro` 三处在**当前检出**中是脱敏占位符，测试按字面断言。Rust 侧必须逐字保留这三个字符串，既不得当作真实凭据外泄，也不得替换/清洗。键顺序即 `Object.keys` 顺序，`findInitialModel`/`restoreModelFromSession` 的第 3/4 步依赖该顺序。

#### 2.21.2 类型

```ts
export interface ScopedModel { model: Model<Api>; thinkingLevel?: ThinkingLevel; }

export interface ModelReferenceRequest {
	/** Bare model id or `provider/id`, optionally followed by `:<thinking>`. */
	text: string;
	/** Only consider models from this provider. */
	provider?: string;
	/** none | strict | lenient —— 见源码注释（OpenRouter 的 `qwen/qwen3-coder:exacto` 含冒号） */
	thinkingSuffix: "none" | "strict" | "lenient";
	/** Match id and name substrings when no exact match exists. */
	fuzzy: boolean;
}

export type ModelReferenceResult =
	| { status: "resolved"; model: Model<Api>; thinkingLevel?: ThinkingLevel; warning?: string }
	| { status: "ambiguous"; candidates: Model<Api>[]; usableCount: number; thinkingLevel?: ThinkingLevel; warning?: string }
	| { status: "not-found"; target?: ModelReferenceTarget };

export interface ModelReferenceTarget {
	provider: string;
	providerModels: [Model<Api>, ...Model<Api>[]];
	/** Text after the provider prefix as typed, including any `:<suffix>`. */
	modelText: string;
	/** `modelText` without a valid thinking suffix (unless `thinkingSuffix` is `none`). */
	modelId: string;
	thinkingLevel?: ThinkingLevel;
}

export interface ModelScopeDiagnostic {
	type: "warning";
	code: "no-match" | "invalid-thinking-level" | "ambiguous";
	message: string;
	pattern: string;
}
export interface ResolveModelScopeResult { scopedModels: ScopedModel[]; diagnostics: ModelScopeDiagnostic[]; }
export interface ResolveCliModelResult {
	model: Model<Api> | undefined;
	thinkingLevel?: ThinkingLevel;
	warning: string | undefined;
	/** Error message suitable for CLI display. When set, model will be undefined. */
	error: string | undefined;
}
export interface InitialModelResult { model: Model<Api> | undefined; thinkingLevel: ThinkingLevel; fallbackMessage: string | undefined; }
```

#### 2.21.3 `splitThinkingSuffix(text)`

`colonIndex = text.lastIndexOf(":")`；`suffix = text.substring(colonIndex + 1)`；若 `colonIndex === -1` 或 `!isValidThinkingLevel(suffix)` → undefined；否则 `{base: text.substring(0, colonIndex), thinkingLevel: suffix}`。注意哨兵判断在取 substring **之后**（`lastIndexOf` 返回 -1 时 `substring(0)` 即全文，随后被 `colonIndex === -1` 短路）。

#### 2.21.4 `splitProviderPrefix(text, models)`

```
lowerText = text.toLowerCase()
best = undefined
for ({provider} of models):                      // 遍历顺序为 models 顺序
    prefix = provider.trim().toLowerCase()
    if !prefix || (best && best.provider.length >= provider.length): continue
    slashIndex = lowerText.indexOf("/", prefix.length)
    if slashIndex === -1 || lowerText.substring(0, slashIndex).trim() !== prefix: continue
    modelText = text.substring(slashIndex + 1).trim()
    if modelText: best = {provider, modelText}
return best
```
要点：provider id 自身可含 `/`，用**最长前缀**胜出（比较 `best.provider.length`，即原始大小写长度）；要求 `provider/model` 中第一段去空白后等于 provider；`modelText` 为空则忽略。因为 provider 取自 models 列表，同一 provider 会重复出现，但 `best.provider.length >= provider.length` 会跳过同长度项，因此结果为**该 provider 在列表中首次出现的 provider 字符串**。

#### 2.21.5 `resolveModelReference(request, models, usable)`

```
text = request.text.trim()
if !text -> {status:"not-found"}
constraint = request.provider?.toLowerCase()
pool = constraint ? models.filter(m => m.provider.toLowerCase() === constraint) : models
result = resolveText(text, request, pool, usable)
if result.status !== "not-found" -> result
split = splitProviderPrefix(text, pool)
anchor = constraint ? pool[0] : pool.find(m => m.provider === split?.provider)
if !anchor -> result（not-found，无 target）
modelText = split?.modelText ?? text
suffix = request.thinkingSuffix === "none" ? undefined : splitThinkingSuffix(modelText)
return {status:"not-found", target: {
    provider: anchor.provider,
    providerModels: [anchor, ...pool.filter(m => m.provider === anchor.provider && m !== anchor)],
    modelText, modelId: suffix?.base ?? modelText, thinkingLevel: suffix?.thinkingLevel }}
```
`providerModels` 是**非空元组**：anchor 在首位，其余同 provider 模型按 pool 顺序。

#### 2.21.6 `resolveText`（递归核心）

```
reference = selectReference(text, pool, request.fuzzy, usable)
if reference -> reference
if request.fuzzy:
    fuzzy = selectFuzzy(fuzzyMatches(text, pool), usable)
    if fuzzy -> fuzzy
if request.thinkingSuffix === "none" -> {status:"not-found"}
thinking = splitThinkingSuffix(text)
if thinking:
    inner = resolveText(thinking.base, ...)
    if inner.status === "not-found" -> inner
    return {...inner, thinkingLevel: inner.warning ? undefined : thinking.thinkingLevel}
if request.thinkingSuffix === "strict" -> {status:"not-found"}     // 无效后缀保留为 id 一部分并失败
colonIndex = text.lastIndexOf(":")
if colonIndex === -1 -> {status:"not-found"}
suffix = text.substring(colonIndex + 1)
inner = resolveText(text.substring(0, colonIndex), ...)
if inner.status === "not-found" -> inner
return {...inner, thinkingLevel: undefined,
        warning: `Invalid thinking level "${suffix}" in pattern "${text}". Using default instead.`}   // 逐字
```
递归深度由冒号数量决定；`lenient` 模式会剥掉无效后缀并给出警告，`strict` 只会剥掉**有效**思维等级后缀。

#### 2.21.7 `selectReference(text, pool, fuzzy, usable)`

```
lowerText = text.toLowerCase()
raw = pool.filter(m => m.id.toLowerCase() === lowerText)        // 裸 id 精确匹配（忽略大小写）
split = splitProviderPrefix(text, pool)
named = undefined
if split:
    providerModels = pool.filter(m => m.provider === split.provider)
    lowerModelText = split.modelText.toLowerCase()
    named = providerModels.find(m => m.id.toLowerCase() === lowerModelText)     // provider 自己的字面 id 优先
         ?? raw.find(m => m.provider === split.provider)                        // 其次裸 id 且同 provider
         ?? (fuzzy ? bestFuzzyMatch(fuzzyMatches(split.modelText, providerModels)) : undefined)
others = raw.filter(m => m !== named)
candidates = named ? [named, ...others] : others
if candidates.length === 0 -> undefined
if Set(candidates.map(provider)).size === 1 -> {status:"resolved", model: candidates[0]}
if named && usable(named.provider) -> {status:"resolved", model: named}
usableOthers = others.filter(m => usable(m.provider))
if usableOthers.length === 1 -> {status:"resolved", model: usableOthers[0]}
if usableOthers.length === 0 && named -> {status:"resolved", model: named}
return {status:"ambiguous", candidates, usableCount: usableOthers.length}
```
语义（源码注释）：`provider/rest` 只是提示，全文也可能是别的 provider 的模型 id；命名的 provider 若可用则胜出，否则唯一可用的裸匹配胜出；命名的匹配即使不可用也优先于不可用的裸匹配（以便 `--api-key` 首次配置）。

#### 2.21.8 模糊匹配

- `fuzzyMatches(text, models)`：`model.id.toLowerCase().includes(lower) || model.name?.toLowerCase().includes(lower)`。
- `selectFuzzy(matches, usable)`：
  ```
  if matches.length === 0 -> undefined
  multipleProviders = Set(matches.map(provider)).size > 1
  usableMatches = multipleProviders ? matches.filter(m => usable(m.provider)) : []
  pool = usableMatches.length > 0 ? usableMatches : matches
  best = bestFuzzyMatch(pool); if !best -> undefined
  tied = pool.filter(m => m.id === best.id)
  if Set(tied.map(provider)).size === 1 -> {status:"resolved", model: best}
  return {status:"ambiguous", candidates: tied, usableCount: usableMatches.length > 0 ? tied.length : 0}
  ```
  注意：单 provider 时**不**过滤 usable（`usableMatches` 为空数组）。
- `bestFuzzyMatch(matches)`：`aliases = matches.filter(m => isAlias(m.id))`；取 `(aliases.length > 0 ? aliases : matches)` 按 `b.id.localeCompare(a.id)` **降序**排序后的第 0 项。
- `isAlias(id)`：`id.endsWith("-latest") || !/-\d{8}$/.test(id)`。即「别名=非 8 位日期后缀，或显式 -latest」，别名优先于日期版本。
- `buildFallbackModel(providerModels, modelId)`：
  ```
  defaultId = defaultModelPerProvider[providerModels[0].provider]
  baseModel = defaultId ? (providerModels.find(m => m.id === defaultId) ?? providerModels[0]) : providerModels[0]
  return {...baseModel, id: modelId, name: modelId}
  ```

#### 2.21.9 `findExactModelReferenceMatch(modelReference, models)`

`resolveModelReference({text, thinkingSuffix:"none", fuzzy:false}, models, () => true)`；`status === "resolved"` 时返回 model，否则 undefined。

#### 2.21.10 `resolveModelScopeFromModels(patterns, models)`

```
scopedModels = []; diagnostics = []
add(model, thinkingLevel): 若 scopedModels 中无 modelsAreEqual 的项则 push {model, thinkingLevel}   // 首次出现者胜，后到者忽略
noMatch(pattern) -> push {type:"warning", code:"no-match", message:`No models match pattern "${pattern}"`, pattern}
ambiguous(pattern, candidates) -> providers = [...Set(...)].sort(localeCompare);
    push {type:"warning", code:"ambiguous",
          message:`Pattern "${pattern}" matches several providers: ${providers.join(", ")}. Use provider/id to choose one.`, pattern}

for pattern of patterns:
    if pattern.includes("*") || pattern.includes("?") || pattern.includes("["):
        thinking = splitThinkingSuffix(pattern)
        globPattern = thinking?.base ?? pattern
        thinkingLevel = thinking?.thinkingLevel
        exact = resolveModelReference({text: globPattern, thinkingSuffix:"none", fuzzy:false}, models, ()=>true)
        matches = []
        if exact.status === "resolved": matches = [exact.model]
        else if exact.status === "ambiguous": matches = exact.candidates; ambiguous(pattern, matches)
        else: matches = models.filter(m => minimatch(`${m.provider}/${m.id}`, globPattern, {nocase:true})
                                       || minimatch(m.id, globPattern, {nocase:true}))
        if matches.length === 0: noMatch(pattern)
        for m of matches: add(m, thinkingLevel)
        continue
    result = resolveModelReference({text: pattern, thinkingSuffix:"lenient", fuzzy:true}, models, ()=>true)
    if result.status === "not-found": noMatch(pattern); continue
    if result.warning: push {type:"warning", code:"invalid-thinking-level", message: result.warning, pattern}
    if result.status === "ambiguous": ambiguous(pattern, result.candidates)
    matches = result.status === "resolved" ? [result.model] : result.candidates
    for m of matches: add(m, result.thinkingLevel)
```
要点：
- glob 判定为**包含** `*`/`?`/`[` 任一（`bracketed-model[1m]` 这类 id 会先尝试精确引用，成功则不当 glob）。
- glob 同时匹配 `provider/id` 与裸 `id`，`nocase: true`（minimatch 语义）。
- 非 glob 分支的 `thinkingSuffix` 为 `lenient`、`fuzzy` 为 `true`。
- 去重键为 `modelsAreEqual`（type + id + provider），**不看 thinkingLevel**。

#### 2.21.11 `resolveModelScopeWithDiagnostics` / `resolveModelScope`

- `resolveModelScopeWithDiagnostics(patterns, modelRuntime, options?)`：`resolveModelScopeFromModels(patterns, await modelRuntime.getAvailable(undefined, options))`。
- `resolveModelScope(patterns, modelRuntime, options?)`：调用上者，然后对每条诊断 `console.warn(chalk.yellow(\`Warning: ${diagnostic.message}\`))`，返回 `scopedModels`。

#### 2.21.12 `resolveCliModel(options)`

```
{cliProvider, cliModel, cliThinking, inlineApiKey, modelRuntime} = options
if !cliModel -> {model:undefined, warning:undefined, error:undefined}
models = [...modelRuntime.getModels()]                    // 用「全部」模型，便于 --api-key 首次配置
if models.length === 0 -> error: "No models available. Check your installation or add models to models.json."
provider = cliProvider ? models.find(m => m.provider.toLowerCase() === cliProvider.toLowerCase())?.provider : undefined
if cliProvider && !provider -> error: `Unknown provider "${cliProvider}". Use --list-models to see available providers/models.`
result = resolveModelReference({text: cliModel, provider, thinkingSuffix:"strict", fuzzy:true}, models,
                               inlineApiKey ? () => false : providerId => modelRuntime.hasConfiguredAuth(providerId))
resolved -> {model, thinkingLevel, warning, error: undefined}
ambiguous ->
    matches = candidates.map(m => `${m.provider}/${m.id}`).sort(localeCompare).join(", ")
    authHint = inlineApiKey ? "--api-key does not choose a provider."
             : result.usableCount === 0 ? "No matching provider is authenticated."
             : "More than one matching provider is authenticated."
    error: `Model "${cliModel}" is ambiguous across providers: ${matches}. ${authHint} Use --provider or provider/model.`
not-found 且无 target -> error: `Model "${cliModel}" not found. Use --list-models to see available models.`
not-found 且有 target ->
    modelId = cliThinking ? target.modelText : target.modelId
    fallbackThinking = cliThinking ? undefined : target.thinkingLevel
    fallbackModel = buildFallbackModel(target.providerModels, modelId)
    requestedThinking = cliThinking ?? fallbackThinking
    model = requestedThinking && requestedThinking !== "off" ? {...fallbackModel, reasoning: true} : fallbackModel
    thinkingLevel: fallbackThinking
    warning: `Model "${modelId}" not found for provider "${target.provider}". Using custom model id.`
    error: undefined
```
以上 6 条错误/警告字符串均**逐字**。

#### 2.21.13 `findInitialModel(options)`

优先级（源码注释即规范）：
1. `scopedModels.length > 0 && !isContinuing`：返回 `scopedModels[0].model`，`thinkingLevel = scopedModel.thinkingLevel ?? modelThinkingLevels[`${provider}/${id}`] ?? defaultThinkingLevel ?? DEFAULT_THINKING_LEVEL`，`fallbackMessage: undefined`。
2. `defaultProvider && defaultModelId`：`found = modelRuntime.getModel(defaultProvider, defaultModelId)`；若存在且 `hasConfiguredAuth(found.provider)`：`thinkingLevel` 优先 `modelThinkingLevels[`${defaultProvider}/${defaultModelId}`]`，其次 `defaultThinkingLevel`，否则仍为 `DEFAULT_THINKING_LEVEL`（初值）；返回。
3. `availableModels = [...modelRuntime.getAvailableSnapshot()]`；非空时按 `Object.keys(defaultModelPerProvider)` **声明顺序**找第一个 `provider` 与 `id` 都匹配的模型，命中即返回（等级 `DEFAULT_THINKING_LEVEL`）；否则返回 `availableModels[0]`。
4. 都无 → `{model: undefined, thinkingLevel: DEFAULT_THINKING_LEVEL, fallbackMessage: undefined}`。

注意 `isContinuing` 只影响第 1 步；第 2 步及以后总执行。

#### 2.21.14 `restoreModelFromSession(savedProvider, savedModelId, currentModel, shouldPrintMessages, modelRuntime)`

```
restoredModel = modelRuntime.getModel(savedProvider, savedModelId)
hasConfiguredAuth = restoredModel ? modelRuntime.hasConfiguredAuth(restoredModel.provider) : false
if restoredModel && hasConfiguredAuth:
    if shouldPrintMessages: console.log(chalk.dim(`Restored model: ${savedProvider}/${savedModelId}`))
    return {model: restoredModel, fallbackMessage: undefined}
reason = !restoredModel ? "model no longer exists" : "no auth configured"
if shouldPrintMessages: console.error(chalk.yellow(`Warning: Could not restore model ${savedProvider}/${savedModelId} (${reason}).`))
if currentModel:
    if shouldPrintMessages: console.log(chalk.dim(`Falling back to: ${currentModel.provider}/${currentModel.id}`))
    return {model: currentModel,
            fallbackMessage: `Could not restore model ${savedProvider}/${savedModelId} (${reason}). Using ${currentModel.provider}/${currentModel.id}.`}
availableModels = [...modelRuntime.getAvailableSnapshot()]
if availableModels.length > 0:
    fallbackModel = 按 defaultModelPerProvider 声明顺序的第一个匹配，否则 availableModels[0]
    if shouldPrintMessages: console.log(chalk.dim(`Falling back to: ${fallbackModel.provider}/${fallbackModel.id}`))
    return {model: fallbackModel,
            fallbackMessage: `Could not restore model ${savedProvider}/${savedModelId} (${reason}). Using ${fallbackModel.provider}/${fallbackModel.id}.`}
return {model: undefined, fallbackMessage: undefined}
```
颜色：`Restored model: ...` / `Falling back to: ...` 用 `chalk.dim`（stdout），警告用 `chalk.yellow`（stderr）。

### 2.22 `model-registry.ts`（244 行）

同步兼容门面（facade），暴露给扩展使用；coding-agent 内部直接用 `ModelRuntime`。

```ts
export type { ProviderConfigInput } from "./provider-composer.ts";
export type ResolvedRequestAuth =
	| { ok: true; apiKey?: string; headers?: ProviderHeaders; baseUrl?: string; env?: Record<string, string> }
	| { ok: false; error: string };
export { clearApiKeyCache } from "./provider-composer.ts";

export class ModelRegistry {
	constructor(runtime: ModelRuntime)
	refresh(options?): Promise<ModelsRefreshResult>           // runtime.refresh
	getError(): string | undefined
	getAll(): Model<Api>[]                                   // [...runtime.getModels()]
	getAvailable(): Model<Api>[]                             // [...runtime.getAvailableSnapshot()]
	find(provider, modelId)                                  // runtime.getModel
	findOfType(type, provider, modelId)                      // runtime.getModelOfType
	hasConfiguredAuth(model)                                 // runtime.hasConfiguredAuth(model.provider)
	getApiKeyAndHeaders(model): Promise<ResolvedRequestAuth>
	getProviderAuthStatus(provider): AuthStatus
	getProvider(provider): Provider | undefined
	stream(model, context, options?): AssistantMessageEventStream
	streamSimple(model, context, options?): AssistantMessageEventStream
	complete(model, context, options?): Promise<AssistantMessage>
	getModelsOfType(type, provider?)
	getAvailableOfType(type, provider?, options?)
	getModelOfType(type, provider, modelId)
	classify(model, context, options?): Promise<ClassifierResult>            // 永不 reject
	generateImages(model, context, options?): Promise<AssistantImages>       // 永不 reject
	getProviderDisplayName(provider): string                 // runtime.getProvider(provider)?.name ?? provider
	getProviderAuth(provider): Promise<AuthResult | undefined>
	getApiKeyForProvider(provider): Promise<string | undefined>   // try/catch -> undefined
	isUsingOAuth(model): boolean
	registerProvider(provider: Provider): void
	registerProvider(providerName: string, config: ProviderConfigInput): void
	registerProvider(providerOrName, config?)                // 见下
	unregisterProvider(providerName): void
	registerVirtualModel(definition): void
	unregisterVirtualModel(providerName, id): void
	getRegisteredProviderConfig(providerName): ProviderConfigInput | undefined
	getRegisteredNativeProvider(providerName): Provider | undefined
	getRegisteredProviderIds(): readonly string[]
}
```

#### 2.22.1 `getApiKeyAndHeaders(model)` 逐字算法

```
try {
    resolution = await runtime.getAuth(model)
    if (!resolution) {
        compatibility = runtime.getCompatibilityRequestConfig(model)
        if (compatibility.authHeader) return {ok:false, error:`No API key found for "${model.provider}"`}   // 逐字
        return {ok:true, headers: compatibility.headers}
    }
    return {ok:true, apiKey: resolution.auth.apiKey, headers: resolution.auth.headers,
            ...(resolution.auth.baseUrl ? {baseUrl: resolution.auth.baseUrl} : {}), env: resolution.env}
} catch (error) {
    cause = error instanceof Error ? error.cause : undefined
    message = cause instanceof Error ? cause.message : error instanceof Error ? error.message : String(error)
    return {ok:false,
            error: message === "authHeader requires a resolved API key" ? `No API key found for "${model.provider}"` : message}
}
```
即：底层 `authHeader requires a resolved API key` 被转换为 `No API key found for "<provider>"`（同一文案两处来源）。

#### 2.22.2 `registerProvider` 重载

`typeof providerOrName === "string"` 时：`!config` → 抛 `Provider config is required when registering by name` 【逐字】；否则 `runtime.registerProvider(name, config)`。否则 `runtime.registerNativeProvider(providerOrName)`。

### 2.23 `virtual-models.ts`（238 行）

虚拟模型是「目录条目」，把每个请求路由到某个物理模型。选择层（`model_change`、`agent.state.model`、`ctx.model`）可以命名虚拟模型；路由之后的一切只看到物理模型（provider 流式、assistant 消息记录）。虚拟模型永不到达 provider。

#### 2.23.1 常量与类型

```ts
export const VIRTUAL_MODEL_API = "kiana-virtual";
export const VIRTUAL_MODEL_STATE_ENTRY = "kiana.virtual-model-state";
export interface VirtualModelStateData<TState = unknown> { provider: string; modelId: string; state: TState; }

const THINKING_LEVELS: readonly ModelThinkingLevel[] =
	["off", "minimal", "low", "medium", "high", "xhigh", "max"];

export type ModelRouteReason = "user" | "continuation" | "retry" | "direct";
// user: 用户撰写消息后的首个请求（prompt/steering/follow-up）
// continuation: 代理循环中的其它请求（工具结果/扩展消息之后）
// retry: 失败请求后的自动重试（包括上下文溢出后的压缩）
// direct: 代理循环之外的请求（压缩摘要、扩展调用）

export interface ModelRouteRequest<TState = unknown> {
	model: Model<Api>;
	thinkingLevel: ModelThinkingLevel;
	reason: ModelRouteReason;
	previous?: { model: Model<Api>; thinkingLevel?: ModelThinkingLevel };
	failed?: { model: Model<Api>; thinkingLevel?: ModelThinkingLevel; message: AssistantMessage };
	state?: TState;
	messages: readonly Message[];
	signal?: AbortSignal;
}

export interface ModelRoute<TState = unknown> {
	model: Model<Api>;
	thinkingLevel: ModelThinkingLevel;
	state?: TState;   // 与 request.state 相同或 undefined 表示保持当前；必须 JSON 可序列化；direct 请求忽略
}

export interface VirtualModelDefinition<TState = unknown> {
	provider: string;    // 列在该 provider 下；可以是已有物理模型的 provider
	id: string;          // 不得与该 provider 的物理模型 id 相同
	name: string;
	thinkingLevels?: readonly ModelThinkingLevel[];   // 默认 ["off"]
	contextWindow?: number;                            // 首响应前的展示限制；未设视为 0（未知）
	maxTokens?: number;
	input?: ("text" | "image")[];                      // 默认 text+image
	route(request: ModelRouteRequest<TState>): ModelRoute<TState> | Promise<ModelRoute<TState>>;
}
```

#### 2.23.2 `isVirtualModel(model)` / `findLatestResponse(messages)`

- `isVirtualModel(model: {api: string})`：`model.api === "kiana-virtual"`。
- `findLatestResponse(messages)`：**从末尾向前**找第一条 `role === "assistant"` 且 `stopReason !== "error"` 且 `stopReason !== "aborted"` 的消息；无则 undefined。失败或中止的请求（含路由失败）被跳过。

#### 2.23.3 `getBranchSelection(branch, getModel)`

```
for i = branch.length-1 downto 0:
    entry = branch[i]
    if entry.type === "model_change":
        return {provider: entry.provider, modelId: entry.modelId}      // 见下方注释
    if entry.type === "message" && entry.message.role === "assistant" && !isVirtualModel(entry.message):
        response = {provider: entry.message.provider, modelId: entry.message.model}
        change = findLastModelChange(branch, i)
        model = change && getModel(change.provider, change.modelId)
        return change && model && isVirtualModel(model) ? change : response
return undefined
```
配合 `findLastModelChange(branch, before)`（从 `before-1` 向前找最近的 `model_change`）。
语义：
- 从末尾向前遇到的第一条 `model_change` 若就是最后一条，则直接采用（无论虚拟或物理）。
- 否则遇到第一条**物理** assistant 消息时，若其之前的最近 `model_change` 指向一个**仍注册的虚拟模型**，则采纳该 `model_change`（虚拟选择保持到下一个 `model_change`）；否则采纳该响应。
- 因为只有最后一个 `model_change` 可能"保持"，整个函数**最多一次**目录查询（测试 `looks up only the last model_change` 断言 `lookups === ["faux/small"]` 等）。
- 纯虚拟 provider 的虚拟模型（无物理模型）同样适用。

#### 2.23.4 `getVirtualModelState(branch, provider, modelId)`

从末尾向前找 `type === "custom"` 且 `customType === "kiana.virtual-model-state"` 的条目，`data?.provider === provider && data.modelId === modelId` 时返回 `data.state`；否则 undefined。

#### 2.23.5 `createVirtualModel(definition)`

```
levels = definition.thinkingLevels ?? ["off"]
thinkingLevelMap = {}
for level of THINKING_LEVELS: thinkingLevelMap[level] = levels.includes(level) ? level : null
return {
    id: definition.id,
    name: definition.name,
    api: VIRTUAL_MODEL_API,
    provider: definition.provider,
    baseUrl: "",
    reasoning: levels.some(level => level !== "off"),
    thinkingLevelMap,                                   // 未提供的等级显式 null
    input: definition.input ?? ["text", "image"],
    cost: {input:0, output:0, cacheRead:0, cacheWrite:0},
    contextWindow: definition.contextWindow ?? 0,
    maxTokens: definition.maxTokens ?? 0,
}
```
无 `headers`、无 `compat`、无 `type` 字段（因此 `getModelType` 返回默认 `"chat"`）。`thinkingLevelMap` 的键顺序固定为 `THINKING_LEVELS` 顺序。

#### 2.23.6 `unroutedStream(model)`

`lazyStream(model, async () => { throw new Error(\`Virtual model ${model.provider}/${model.id} must be routed before streaming\`) })` 【逐字】。

#### 2.23.7 `withVirtualModels(providerId, provider, virtualModels)`

- **无 provider**（纯虚拟 provider）：
  ```
  {id: providerId, name: providerId,
   auth: {apiKey: {name: "Virtual model", resolve: async () => ({auth: {}, source: "virtual"})}},
   getModels: () => virtualModels,
   stream: unroutedStream, streamSimple: unroutedStream}
  ```
  【逐字】`name: "Virtual model"`、`source: "virtual"`。注意没有 `getAllModels`，也没有 `baseUrl`。
- **有 provider**：
  ```
  ids = Set(virtualModels.map(m => m.id))
  physical(models) = models.filter(m => !isVirtualModel(m) && !(isModelType(m,"chat") && ids.has(m.id)))
      // 虚拟模型会隐藏同 id 的物理 chat 模型（目录刷新可能在注册后加入）
  virtual(models) = models.filter(m => isVirtualModel(m))
  {filterModels, filterAllModels} = provider
  return {
      ...provider,
      getModels: () => [...physical(provider.getModels()), ...virtualModels],
      getAllModels: () => [...physical(provider.getAllModels?.() ?? provider.getModels()), ...virtualModels],
      filterModels: (models, credential) => {
          real = physical(models); return [...(filterModels?.(real, credential) ?? real), ...virtual(models)] },
      filterAllModels: filterAllModels && ((models, credential) =>
          [...filterAllModels(physical(models), credential), ...virtual(models)]),
      stream: (model, context, options) => isVirtualModel(model) ? unroutedStream(model) : provider.stream(...),
      streamSimple: (model, context, options) => isVirtualModel(model) ? unroutedStream(model) : provider.streamSimple(...),
  }
  ```
  注意：`getModels` 追加的是**注册的 virtualModels 数组**（顺序=注册顺序），而 `filterModels` 里追加的是输入中已有的虚拟模型（`virtual(models)`）。

### 2.24 `remote-catalog-provider.ts`（240 行）

kiana.dev 远程目录覆盖层：把持久化的（可选个性化的）模型列表叠加到内置静态 provider 上。

> **Rust 实现差异（品牌隔离）**：`catalogBaseUrl` 缺省为 `KIANA_API_BASE_URL`；未配置时
> 不发起远端目录请求，只使用内置与已缓存模型。构造函数不再有隐式上游默认值。

#### 2.24.1 常量

```ts
const DEFAULT_CATALOG_BASE_URL = "https://kiana.dev";
const REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS = 4_000;
export const REMOTE_CATALOG_REFRESH_INTERVAL_MS = 4 * 60 * 60 * 1000;            // 4 小时
export const REMOTE_CATALOG_MODEL_TYPES: readonly ModelType[] = ["chat", "image", "classifier"];
```
`REMOTE_CATALOG_MODEL_TYPES` 会作为 `?types=` 发送，使服务端返回全类型分片；忽略该参数的旧服务端返回仅 chat 分片，客户端行为不变。

#### 2.24.2 辅助函数

- `isSupportedModelType(model)`：`model.type === undefined || (typeof type === "string" && REMOTE_CATALOG_MODEL_TYPES.includes(type))`。
- `mergeModels(baseline, dynamic)`：`Map` 键 `` `${getModelType(model)}\0${model.id}` ``（**类型 + NUL + id**），先插 baseline 再插 dynamic → 同键后者覆盖（保留**首次插入位置**），返回 `[...values()]`。
- `parseCatalog(providerId, value)`：
  ```
  entries = Array.isArray(value) ? value
          : (object && "models" in value && Array.isArray(value.models)) ? value.models
          : (object) ? Object.values(value) : undefined
  if !entries -> throw new Error(`Invalid model catalog for provider "${providerId}"`)      // 逐字
  return entries
      .filter(entry => typeof entry === "object" && entry !== null && "id" in entry)
      .filter(isSupportedModelType)
      .map(model => ({...model, provider: providerId}))
  ```
  `Object.values` 分支意味着「以 id 为键的对象映射」也被接受。`provider` 字段**强制**覆盖为请求的 providerId。
- `readCatalog(providerId, response)`：`lastModified = Date.parse(response.headers.get("last-modified") ?? "")`；返回 `{models: parseCatalog(providerId, await response.json()), lastModified: Number.isNaN(lastModified) ? 0 : lastModified, etag: response.headers.get("etag") ?? undefined}`。**缺失或非法的 Last-Modified 记为 0**（而不是 NaN）。
- `isNewer(lastModified, localGeneratedAt)`：`localGeneratedAt === undefined || (lastModified !== undefined && lastModified > localGeneratedAt)`。
- `remoteModels(entry, localGeneratedAt)`：`entry && isNewer(entry.lastModified, localGeneratedAt) ? entry.models : []`。
- `requestCatalog(providerId, catalogBaseUrl, signal, validator?)`：
  ```
  url = new URL(`/api/models/providers/${encodeURIComponent(providerId)}`, catalogBaseUrl)
  url.searchParams.set("types", REMOTE_CATALOG_MODEL_TYPES.join(","))       // "chat,image,classifier"（逗号被编码为 %2C）
  return fetchWithRetry(url, {
      headers: { accept: "application/json", "User-Agent": getKianaUserAgent(VERSION),
                 ...(validator ? {"if-none-match": validator} : {}) },
      signal,
  }, {attemptTimeoutMs: 4000})
  ```
  实际请求序列（`model-catalog-protocol.test.ts` 断言 `requests === [catalogUrl, \`${catalogUrl}&kiana-version=${VERSION}\`]`）：客户端**只**发第一个 URL；服务端（`scripts/model-catalog-protocol.ts` 的 `parseModelCatalogRequest`）看到 URL 无 `kiana-version` 且 `User-Agent` 里解析出合法 Kiana 版本时，返回 **307 redirect** 到追加了 `&kiana-version=<UA 中的版本>` 的同一 URL，客户端跟随重定向后才得到目录。因此：
  - 客户端**不**自己拼接 `kiana-version`；它依赖 `User-Agent`（`getKianaUserAgent(VERSION)`）被服务端识别。
  - Rust 侧必须保留默认的 307/308 跟随行为（`reqwest` 默认跟随最多 10 次重定向，且 307/308 保留方法与 body）。
  - 该测试同时断言 `refresh.errors` 为空、模型来自 `x-kiana-model-catalog-revision` 选中的 revision（`extended`/`legacy` 分片的选择在服务端完成）。

#### 2.24.3 `withRemoteCatalog(provider, catalogBaseUrl = "https://kiana.dev", localGeneratedAt?)`

闭包状态 `dynamicModels: readonly AnyModel[] = []`。返回 `{...provider, getModels, getAllModels, refreshModels}`：
- `getModels: () => mergeModels(provider.getModels(), dynamicModels.filter(m => isModelType(m,"chat")))`。
- `getAllModels: () => mergeModels(provider.getAllModels?.() ?? provider.getModels(), dynamicModels)`。
- `refreshModels(context)` 步骤（严格顺序）：
  1. `stored = context.stored`；`restored = remoteModels(stored, localGeneratedAt).filter(m => m.provider === provider.id)`。
  2. `publish({update: () => {dynamicModels = restored}})`；若返回 **falsy** → `return`（发布被拒绝则完全不继续）。
  3. `!context.allowNetwork || context.signal.aborted` → return。
  4. **TTL 短路**：若 `!context.force && stored?.checkedAt !== undefined && stored.lastModified !== undefined && Date.now() - stored.checkedAt < REMOTE_CATALOG_REFRESH_INTERVAL_MS` → return。
     （无 `lastModified` 的条目总是重新抓取：`remoteModels()` 会忽略它，等待只会让覆盖层保持为空。）
  5. `validator = stored && stored.models.length > 0 ? stored.etag : undefined`（**仅当有缓存 body 支撑时才发 if-none-match**，避免 304 导致覆盖层为空）。
  6. `response = await requestCatalog(provider.id, catalogBaseUrl, context.signal, validator)`；`context.signal.aborted` → return。
  7. `checkedAt = Date.now()`。
  8. `status === 304 && stored` → `publish({persist: {...stored, checkedAt}})`；return（**不**改 dynamicModels）。
  9. `status === 404 || status === 501` → `publish({persist: {...(stored ?? {models: []}), checkedAt, lastModified: 0, etag: undefined}})`；return。
  10. `!response.ok` → 先 `publish({persist: {...(stored ?? {models: []}), checkedAt}})`（保留原 models 与 etag），再 `throw new Error(\`Model catalog request failed for ${provider.id}: ${response.status}\`)` 【逐字】。
  11. `catalog = await readCatalog(provider.id, response)`；`signal.aborted` → return。
  12. `entry = {...catalog, checkedAt}`；`published = remoteModels(entry, localGeneratedAt)`。
  13. `publish({persist: entry, update: () => {dynamicModels = published}})`。

#### 2.24.4 `withPersonalizedCatalog(provider, personalize, catalogBaseUrl = "https://kiana.dev", localGeneratedAt?)`

```ts
export type PersonalizeCatalog = (catalog: readonly AnyModel[], context: RefreshModelsContext) => Promise<readonly AnyModel[]>;
```
闭包：`personalModels: readonly AnyModel[] = []`；`builtinModels()` = `provider.getAllModels?.() ?? provider.getModels()`；`getAllModels()` = `personalModels.length > 0 ? personalModels : builtinModels()`。
- `getModels: () => getAllModels().filter(m => isModelType(m,"chat"))`。
- `refreshModels(context)`：
  1. `restored = (context.stored?.models ?? []).filter(m => m.provider === provider.id)`
  2. `publish({update: () => {personalModels = restored}})`；falsy → return
  3. `!allowNetwork || aborted` → return
  4. `response = await requestCatalog(provider.id, catalogBaseUrl, context.signal)`（**不带 validator**）
  5. aborted → return
  6. `remote = []`；若 `status !== 404 && status !== 501`：`!response.ok` → 抛 `Model catalog request failed for ${provider.id}: ${response.status}`；否则 `catalog = readCatalog(...)`；`isNewer(catalog.lastModified, localGeneratedAt)` 时 `remote = catalog.models`
  7. `personalized = await personalize(mergeModels(builtinModels(), remote), context)`
  8. aborted → return
  9. `publish({persist: {models: personalized, checkedAt: Date.now()}, update: () => {personalModels = personalized}})`

差异（相对 `withRemoteCatalog`，源码注释即规范）：每次网络刷新都把（内置 + 更新的 kiana.dev 目录）交给 `personalize` 并保存结果；持久化条目保存的是**个性化列表而非 kiana.dev 目录**，且离线时也替换目录；**跳过刷新间隔与 etag 校验**；空列表（尚未保存，或旧 kiana 在 kiana.dev 失败/缺失后保存的空目录）回退到内置模型。

### 2.25 `key-id-schema.ts`（21 行）

```ts
const modifiers = ["ctrl", "shift", "alt", "super"] as const;
const baseKeyPattern = `(?:[a-z0-9]|${Object.values(Key).flatMap(v =>
    typeof v === "string" ? [v.replace(/[\\^$.*+?()[\]{}|]/g, "\\$&")] : []).join("|")})`;
const duplicateModifierPattern = modifiers.map(m => `${m}\\+.*${m}\\+`).join("|");

export const KeyIdSchema = Type.Unsafe<KeyId>(Type.String({
    pattern: `^(?!.*(?:${duplicateModifierPattern}))(?:(?:${modifiers.join("|")})\\+){0,4}${baseKeyPattern}$`,
    description: "Key identifier with optional ctrl, shift, alt, or super modifiers.",
}));

export function keybindingValueSchema(options?: TSchemaOptions) {
    return Type.Union([KeyIdSchema, Type.Array(KeyIdSchema)], options);
}
export const KeybindingValueSchema = keybindingValueSchema();
```

`Key` 的字符串值（来自 `kiana-tui` `keys.ts`，顺序即 `Object.values` 顺序）：`escape, esc, enter, return, tab, space, backspace, delete, insert, clear, home, end, pageUp, pageDown, up, down, left, right, f1..f12, ` `` ` ``, -, =, [, ], \\, ;, ', ,, ., /, !, @, #, $, %, ^, &, *, (, ), _, +, |, ~, {, }, :, <, >, ?`（`Key` 对象的函数成员被 `typeof v === "string"` 过滤掉）。

**生成的最终正则（逐字，来自 `schemas/keybindings.schema.json` `$defs.KeybindingValue.anyOf[0].pattern`）**：
```
^(?!.*(?:ctrl\+.*ctrl\+|shift\+.*shift\+|alt\+.*alt\+|super\+.*super\+))(?:(?:ctrl|shift|alt|super)\+){0,4}(?:[a-z0-9]|escape|esc|enter|return|tab|space|backspace|delete|insert|clear|home|end|pageUp|pageDown|up|down|left|right|f1|f2|f3|f4|f5|f6|f7|f8|f9|f10|f11|f12|`|-|=|\[|\]|\\|;|'|,|\.|/|!|@|#|\$|%|\^|&|\*|\(|\)|_|\+|\||~|\{|\}|:|<|>|\?)$
```
`description` 逐字：`Key identifier with optional ctrl, shift, alt, or super modifiers.`
注意语义：负向前瞻禁止**同一修饰键出现两次**；修饰键可重复 0–4 次（含不同键）；基础键大小写敏感（仅小写字母）；`KeyId` 是类型层面的受限联合，但运行时只由该正则校验。

### 2.26 `keybindings-schema.ts`（21 行）

```ts
export { KeybindingValueSchema } from "./key-id-schema.ts";

const properties: Record<string, TSchema> = { $schema: Type.Optional(Type.String()) };
for (const [id, definition] of Object.entries(KEYBINDINGS)) {
	properties[id] = Type.Optional(keybindingValueSchema({ description: definition.description }));
}
export const KeybindingsSchema = Type.Object(properties, { additionalProperties: KeybindingValueSchema });
```
生成的 `keybindings.schema.json`：`properties` 共 **91** 个键（`$schema` + 90 个 keybinding id），每个 keybinding 的 description 取自 `KEYBINDINGS[id].description`；`additionalProperties: {"$ref": "#/$defs/KeybindingValue"}`。90 个 id 的顺序（即 `properties` 顺序）：`tui.editor.*`(23) → `tui.input.*`(4) → `tui.select.*`(6) → `tui.altScreen.*`(14) → `app.*`(43)。共 47 + 43 = 90。

### 2.27 `keybindings.ts`（359 行）

#### 2.27.1 `useWindowsKeybindings(platform = process.platform, env = process.env)`

`platform === "win32" || (platform === "linux" && Boolean(env.WSL_DISTRO_NAME || env.WSL_INTEROP))`。
模块顶层 `const windowsKeybindings = useWindowsKeybindings()`（**模块加载时求值一次**）。

#### 2.27.2 `APP_KEYBINDINGS`（逐字，含平台差异；**模块级 `const`，未 `export`**）

> 注意：`APP_KEYBINDINGS` 在 `keybindings.ts:25` 是**非导出**的模块级常量（`const APP_KEYBINDINGS = {...}`），只能经由 `KEYBINDINGS`（`{...TUI_KEYBINDINGS, ...overrides, ...APP_KEYBINDINGS}`）间接访问，以及 `export type AppKeybinding = keyof typeof APP_KEYBINDINGS` 暴露键名联合。Rust 侧若要允许其他单元按名字引用，应提供常量 `APP_KEYBINDINGS`，但需明确其**不是** TS 包的公开导出（§5.2 的清单按 TS 导出面会给 Rust 多暴露一个符号；如严格对齐导出面，应只导出 `KEYBINDINGS` + `AppKeybinding`）。

| id | defaultKeys | description |
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
| `app.tree.foldOrUp` | `darwin ? ["alt+left","ctrl+left"] : ["ctrl+left","alt+left"]` | `Fold tree branch or move up` |
| `app.tree.unfoldOrDown` | `darwin ? ["alt+right","ctrl+right"] : ["ctrl+right","alt+right"]` | `Unfold tree branch or move down` |
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

导出 `type AppKeybinding = keyof typeof APP_KEYBINDINGS`；`type AppKeybindings = { [K in AppKeybinding]: true }`；并做模块声明合并 `interface Keybindings extends AppKeybindings {}`（**纯类型层**，无运行时效果）。

#### 2.27.3 `KEYBINDINGS`（TUI 默认 + 覆盖 + 应用层）

```ts
export const KEYBINDINGS = {
	...TUI_KEYBINDINGS,
	"tui.editor.undo": {...TUI_KEYBINDINGS["tui.editor.undo"],
		defaultKeys: process.platform === "win32" ? "ctrl+z" : windowsKeybindings ? "alt+z" : "ctrl+-"},
	"tui.altScreen.previousPrompt": {...TUI_KEYBINDINGS["tui.altScreen.previousPrompt"],
		defaultKeys: windowsKeybindings ? "ctrl+up" : ["ctrl+shift+up", "ctrl+up"]},
	"tui.altScreen.nextPrompt": {...TUI_KEYBINDINGS["tui.altScreen.nextPrompt"],
		defaultKeys: windowsKeybindings ? "ctrl+down" : ["ctrl+shift+down", "ctrl+down"]},
	"tui.altScreen.search": {...TUI_KEYBINDINGS["tui.altScreen.search"],
		defaultKeys: windowsKeybindings ? "ctrl+f" : "ctrl+shift+f"},
	...APP_KEYBINDINGS,
} as const satisfies KeybindingDefinitions;
```
- `tui.editor.undo` 三种情形：win32 → `ctrl+z`；WSL（linux 且有 WSL 环境变量）→ `alt+z`；其它 → `ctrl+-`。
- 键顺序决定 `KeybindingsSchema` 属性顺序与 `orderKeybindingsConfig` 的输出顺序。`TUI_KEYBINDINGS` 的 47 个 id 与定义见 `tui-core` 单元规范；此处仅覆盖上述 4 项，且**覆盖是在原位置展开**（`...TUI_KEYBINDINGS` 先给出全部，再以同名键覆盖，JS 对象保留**首次插入位置**）。

#### 2.27.4 `KEYBINDING_NAME_MIGRATIONS`（逐字，59 项）

```
cursorUp→tui.editor.cursorUp; cursorDown→tui.editor.cursorDown; cursorLeft→tui.editor.cursorLeft;
cursorRight→tui.editor.cursorRight; cursorWordLeft→tui.editor.cursorWordLeft; cursorWordRight→tui.editor.cursorWordRight;
cursorLineStart→tui.editor.cursorLineStart; cursorLineEnd→tui.editor.cursorLineEnd;
jumpForward→tui.editor.jumpForward; jumpBackward→tui.editor.jumpBackward;
pageUp→tui.editor.pageUp; pageDown→tui.editor.pageDown;
deleteCharBackward→tui.editor.deleteCharBackward; deleteCharForward→tui.editor.deleteCharForward;
deleteWordBackward→tui.editor.deleteWordBackward; deleteWordForward→tui.editor.deleteWordForward;
deleteToLineStart→tui.editor.deleteToLineStart; deleteToLineEnd→tui.editor.deleteToLineEnd;
yank→tui.editor.yank; yankPop→tui.editor.yankPop; undo→tui.editor.undo;
newLine→tui.input.newLine; submit→tui.input.submit; tab→tui.input.tab; copy→tui.input.copy;
selectUp→tui.select.up; selectDown→tui.select.down; selectPageUp→tui.select.pageUp; selectPageDown→tui.select.pageDown;
selectConfirm→tui.select.confirm; selectCancel→tui.select.cancel;
interrupt→app.interrupt; clear→app.clear; exit→app.exit; suspend→app.suspend;
cycleThinkingLevel→app.thinking.cycle; cycleModelForward→app.model.cycleForward; cycleModelBackward→app.model.cycleBackward;
selectModel→app.model.select; expandTools→app.tools.expand; toggleThinking→app.thinking.toggle;
toggleSessionNamedFilter→app.session.toggleNamedFilter; externalEditor→app.editor.external;
followUp→app.message.followUp; dequeue→app.message.dequeue; pasteImage→app.clipboard.pasteImage;
newSession→app.session.new; tree→app.session.tree; fork→app.session.fork; resume→app.session.resume;
treeFoldOrUp→app.tree.foldOrUp; treeUnfoldOrDown→app.tree.unfoldOrDown; treeEditLabel→app.tree.editLabel;
treeToggleLabelTimestamp→app.tree.toggleLabelTimestamp; toggleSessionPath→app.session.togglePath;
toggleSessionSort→app.session.toggleSort; renameSession→app.session.rename; deleteSession→app.session.delete;
deleteSessionNoninvasive→app.session.deleteNoninvasive
```
（另有一批旧名到新名映射由 `src/migrations.ts` 处理，不在本单元。）

#### 2.27.5 `toKeybindingsConfig(value)`

遍历 `Object.entries`：跳过 `"$schema"`；`validateKeybindingValue.Check(binding)` 通过则 `config[key] = binding`，否则**静默丢弃**。`validateKeybindingValue = Compile(KeybindingValueSchema)`（TypeBox 编译一次，模块级）。

#### 2.27.6 `migrateKeybindingsConfig(rawConfig)`

```
config = {}; migrated = false
for [key, value] of Object.entries(rawConfig):
    nextKey = isLegacyKeybindingName(key) ? KEYBINDING_NAME_MIGRATIONS[key] : key
    if nextKey !== key: migrated = true
    if key !== nextKey && Object.hasOwn(rawConfig, nextKey): migrated = true; continue     // 新名已存在 → 丢弃旧名
    config[nextKey] = value
return {config: orderKeybindingsConfig(config), migrated}
```
注：`isLegacyKeybindingName` 用 `in` 判断（因此 `"toString"` 之类原型链键会命中原型属性——`Record` 常量对象有 `Object.prototype`，`"toString" in {...}` 为 true，但 `KEYBINDING_NAME_MIGRATIONS["toString"]` 在 TS 类型层不可达；Rust 侧按「显式表命中」实现即可，无原型链）。`Object.hasOwn` 是**自有属性**判断。

#### 2.27.7 `orderKeybindingsConfig(config)`

```
ordered = {}
for keybinding of Object.keys(KEYBINDINGS):
    if Object.hasOwn(config, keybinding): ordered[keybinding] = config[keybinding]
extras = Object.keys(config).filter(k => !Object.hasOwn(ordered, k)).sort()      // 默认 JS 字符串排序（UTF-16 码元）
for k of extras: ordered[k] = config[k]
return ordered
```

#### 2.27.8 `loadRawConfig(path)`

`!existsSync(path)` → undefined；`JSON.parse(stripBom(readFileSync(path, "utf-8")))`；若结果不是对象或为 null → undefined；解析失败 → undefined（**静默**）。

#### 2.27.9 `KeybindingsManager extends TuiKeybindingsManager`

```ts
export class KeybindingsManager extends TuiKeybindingsManager {
	private configPath: string | undefined;
	constructor(userBindings: KeybindingsConfig = {}, configPath?: string) {
		super(KEYBINDINGS, userBindings); this.configPath = configPath;
	}
	static create(agentDir: string = getAgentDir()): KeybindingsManager {
		const configPath = join(agentDir, "keybindings.json");
		const userBindings = KeybindingsManager.loadFromFile(configPath);
		return new KeybindingsManager(userBindings, configPath);
	}
	reload(): void {
		if (!this.configPath) return;
		this.setUserBindings(KeybindingsManager.loadFromFile(this.configPath));
	}
	getEffectiveConfig(): KeybindingsConfig { return this.getResolvedBindings(); }
	private static loadFromFile(path: string): KeybindingsConfig {
		const rawConfig = loadRawConfig(path);
		if (!rawConfig) return {};
		return toKeybindingsConfig(migrateKeybindingsConfig(rawConfig).config);
	}
}
export type { Keybinding, KeyId, KeybindingsConfig };
```
`getResolvedBindings()` / `setUserBindings()` 由 `tui-core` 的 `KeybindingsManager` 提供（见该单元规范）：解析用户覆盖 + 默认值，返回每个 id 的最终 keys 数组。

### 2.28 `cache-warmer.ts`（453 行）

prompt cache 预热状态机：在缓存条目过期前，用「输出上限 1 token」重发一次原请求以续命；并把每次成功预热记为 `cache_warm` 用量。

#### 2.28.1 常量（逐字）

```ts
const MAX_WARMING_AGE_MS = 60 * 60_000;                 // 3 600 000
const MAX_IDLE_WARMING_AGE_MS = 30 * 60_000;            // 1 800 000
const CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS = 0.05;
const IDLE_CONTINUATION_PROBABILITY = 0.15;
```

#### 2.28.2 `getCacheWarmingDelayMs(ttlMs)`

`ttlMs <= 10_000` → undefined；否则 `Math.max(1, Math.floor(Math.min(ttlMs * 0.9, ttlMs - 10_000)))`。
（在 90% TTL 时刷新，同时保留至少 10 秒余量；结果下限 1 ms。）

#### 2.28.3 `getPromptCacheTtlMs(model, options)`

```
retention = options?.cacheRetention
         ?? (getProviderEnvValue("KIANA_CACHE_RETENTION", options?.env) === "long" ? "long" : "short")
if retention === "none" -> undefined
seconds = model.promptCache?.[retention]
return seconds === undefined ? undefined : seconds * 1000
```
即：未显式指定时，`KIANA_CACHE_RETENTION=long`（经 `getProviderEnvValue` 查 options.env 再 process.env）→ long，否则 short。

#### 2.28.4 `isReplayable(model, options)`

```
if (!options?.reasoning || model.api !== "anthropic-messages") return true
return model.compat?.forceAdaptiveThinking === true
```
即 Anthropic 且启用了 reasoning 时，只有 `compat.forceAdaptiveThinking` 才可安全重放（否则 budget_tokens 由 max_tokens 推导，重放会改变缓存键）。

#### 2.28.5 `lastPromptTokens(entries)` / `price(model, tokens)`

- `lastPromptTokens`：从末尾向前找第一条 `type === "message" && role === "assistant"`，返回 `usage.input + usage.cacheRead + usage.cacheWrite`；无则 0。
- `price(model, tokens)`：构造完整 `Usage`（`input/output/cacheRead/cacheWrite` = 0，`totalTokens` = 0，`cost` 全 0）后用 `{...tokens}` 覆盖，`calculateCost(model, usage).total`。

#### 2.28.6 类型

```ts
export type CacheWarmingAction = "warm" | "stop";

export interface CacheWarmingDecision {
	phase: "streaming" | "idle";
	warmCost: number;              // 本次刷新价格：prompt 的 cache read + 1 个输出 token
	missCost: number;              // 缓存丢失时下一次真实请求的额外价格
	continuationProbability: number;
	expectedSavings: number;       // continuationProbability * missCost - warmCost
	economicsAvailable: boolean;   // prompt 大小或模型价格未知时为 false
	action: CacheWarmingAction;    // expectedSavings >= $0.05 时为 "warm"
}

export interface CacheWarmingDecisionEvent
	extends Pick<CacheWarmingDecision, "warmCost"|"missCost"|"continuationProbability"|"action"> {
	type: "cache_warming_decision";
}
export interface CacheWarmingDecisionEventResult { action?: CacheWarmingAction; }

export interface CacheWarmingStatus {
	state: "inactive" | "scheduled" | "refreshing";
	reason?: string;
	nextWarmAt?: number;
	decision?: CacheWarmingDecision;
	extensionOverride?: boolean;
}

export interface CacheWarmRequest { model: Model<Api>; context: Context; options: ModelsSimpleStreamOptions; }
```

内部 `ActiveRun extends CacheWarmRequest`：`isCurrent() => boolean`、`ttlMs`、`delayMs`、`refreshDeadlineAt`、`startedAt`、`controller: AbortController`、`phase: "streaming"|"idle"`、`nextWarmAt`、`extensionOverride: boolean`、`timer?`。

#### 2.28.7 `class CacheWarmer`

构造：`(models: Pick<ModelRuntime,"streamSimple">, sessionManager: Pick<SessionManager,"appendUsage"|"getBranch">, getMode: () => CacheWarmingMode, decide = async event => event.action)`。初始 `inactive = {state:"inactive", reason:"waiting for first request"}`。`onWarmed?: (entry: UsageEntry) => void`。

**`get status(): CacheWarmingStatus`**（逐字顺序）
```
if getMode() === "off" -> {state:"inactive", reason:"cache warming disabled"}
run = this.run; if (!run) -> this.inactive
if !run.isCurrent() -> {state:"inactive", reason:"conversation context changed"}
decision = evaluate(run)
refreshing = run.timer === undefined
if !decision.economicsAvailable && !refreshing -> {state:"inactive", reason:"cache economics unavailable"}
return {state: refreshing ? "refreshing" : "scheduled", nextWarmAt: run.nextWarmAt, decision,
        extensionOverride: run.extensionOverride}
```

**`start(request, isCurrent)`**
```
clearRun()
mode = getMode()
if mode === "off"                        -> stop("cache warming disabled"); return
if !isReplayable(model, options)         -> stop("request cannot be replayed safely"); return
ttlMs = getPromptCacheTtlMs(...)
if ttlMs === undefined                   -> stop(options.cacheRetention === "none" ? "request disabled prompt caching"
                                                                             : "cache lifetime unavailable"); return
delayMs = getCacheWarmingDelayMs(ttlMs)
if delayMs === undefined                 -> stop("cache lifetime unavailable"); return
run = {...request, isCurrent, ttlMs, delayMs, refreshDeadlineAt: 0, startedAt: Date.now(),
       controller: new AbortController(), phase: "streaming", nextWarmAt: 0, extensionOverride: false}
schedule(run)
```

**`onAgentSettled()`**
```
run = this.run; if (!run) return
if getMode() === "streaming" -> stop("agent run settled"); return
run.phase = "idle"
deadline = run.startedAt + MAX_IDLE_WARMING_AGE_MS
if run.nextWarmAt > deadline || Date.now() >= deadline -> stop("30-minute idle safety limit reached")
```

**`onModeChanged()`**：无 run → return；`reason = getModeStopReason(run)`；有则 `stop(reason)`。
**`cancel()`**：`stop("inactive")`。

**私有 `clearRun()`**：`this.run = undefined`；`run.timer` 存在则 `clearTimeout`；`run.controller.abort()`（**先清引用再 abort**）。
**私有 `stop(reason, stopped?)`**：`clearRun()`；`inactive = {state:"inactive", reason, ...stopped}`。

**私有 `schedule(run)`**
```
run.extensionOverride = false
run.nextWarmAt = Date.now() + run.delayMs
run.refreshDeadlineAt = run.nextWarmAt + Math.floor((run.ttlMs - run.delayMs) / 2)   // 保留一半过期余量
deadline = run.startedAt + (run.phase === "idle" ? MAX_IDLE_WARMING_AGE_MS : MAX_WARMING_AGE_MS)
if run.nextWarmAt > deadline || Date.now() >= deadline:
    stop(run.phase === "idle" ? "30-minute idle safety limit reached" : "one-hour safety limit reached"); return
run.timer = setTimeout(() => void this.refresh(run), Math.max(0, run.nextWarmAt - Date.now()))
run.timer.unref?.()
```

**私有 `async refresh(run)`**
```
run.timer = undefined
if !validateRun(run) return
if refreshDeadlineMissed(run) return
decision = evaluate(run); {warmCost, missCost, continuationProbability} = decision
action = decision.action
try { action = await decide({type:"cache_warming_decision", warmCost, missCost, continuationProbability, action}) }
catch { /* 扩展失败回退到 kiana 的决定 */ }
if !validateRun(run) || refreshDeadlineMissed(run) return
extensionOverride = action !== decision.action
if action === "stop":
    reason = extensionOverride ? "stopped by extension"
           : decision.economicsAvailable ? "expected savings below threshold"
           : "cache economics unavailable"
    stop(reason, {decision, extensionOverride}); return
run.extensionOverride = extensionOverride
try {
    message = await models.streamSimple(run.model, run.context,
        {...run.options, maxTokens: 1, maxRetries: 0, signal: run.controller.signal}).result()
    if !validateRun(run) return
    if message.stopReason !== "error" && message.stopReason !== "aborted":
        entry = sessionManager.appendUsage("cache_warm", message.provider,
                    message.responseModel ?? message.model, message.usage,
                    extensionOverride ? "extension override" : undefined)
        onWarmed?.(entry)
} catch { /* 预热是 best-effort，绝不影响主代理 run */ }
if (this.run === run) schedule(run)
```

**私有 `refreshDeadlineMissed(run)`**：`Date.now() <= run.refreshDeadlineAt` → false；否则 `stop("cache refresh deadline missed")`，返回 true。
**私有 `validateRun(run)`**：`this.run !== run` → false；`reason = getModeStopReason(run) ?? (!run.isCurrent() ? "conversation context changed" : undefined)`；无 reason → true；否则 `stop(reason)` 并 false。
**私有 `getModeStopReason(run)`**：`mode === "off"` → `"cache warming disabled"`；`mode === "streaming" && run.phase === "idle"` → `"agent run settled"`；否则 undefined。

**私有 `evaluate(run)`**（逐字）
```
promptTokens = lastPromptTokens(sessionManager.getBranch())
cacheHitCost  = price(model, {cacheRead: promptTokens})
cacheMissCost = price(model, model.cost.cacheWrite > 0 ? {cacheWrite: promptTokens} : {input: promptTokens})
warmCost = price(model, {cacheRead: promptTokens, output: 1})
missCost = Math.max(0, cacheMissCost - cacheHitCost)
continuationProbability = run.phase === "idle" ? 0.15 : 1
economicsAvailable = promptTokens > 0 && (cacheHitCost > 0 || cacheMissCost > 0)
expectedSavings = continuationProbability * missCost - warmCost
action = expectedSavings >= 0.05 ? "warm" : "stop"
```

#### 2.28.8 状态原因字符串（**全部逐字**，共 14 条）

`cache warming disabled`、`waiting for first request`、`conversation context changed`、`cache economics unavailable`、`request cannot be replayed safely`、`request disabled prompt caching`、`cache lifetime unavailable`、`agent run settled`、`30-minute idle safety limit reached`、`one-hour safety limit reached`、`cache refresh deadline missed`、`stopped by extension`、`expected savings below threshold`、`inactive`。

#### 2.28.9 格式化函数

- `formatDollars(value)`：`value < 0 ? \`-$${Math.abs(value).toFixed(3)}\` : \`$${value.toFixed(3)}\``（**3 位小数**，负数把负号放 `$` 前）。
- `formatCacheWarmingEconomics(decision)`：
  - `!economicsAvailable` → `cache economics unavailable`
  - `probability = Math.round(continuationProbability * 100)`
  - `probabilityText = phase === "streaming" ? \`${probability}% continuation probability while agent is running\` : \`${probability}% continuation probability\``
  - `comparison = action === "warm" ? ">=" : "<"`
  - 返回 `` `${probabilityText}, expected savings ${formatDollars(expectedSavings)} ${comparison} $${CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS.toFixed(3)}` ``（即 `$0.050`）
- `formatCacheWarmingDecisionTime(nextWarmAt, now)`：
  - `nextWarmAt === undefined || nextWarmAt <= now` → `Decision now`
  - `remainingSeconds = Math.ceil((nextWarmAt - now)/1000)`；`hours = floor(/3600)`；`remaining %= 3600`；`minutes = floor(/60)`；`seconds = %60`
  - `parts`：hours>0 → `${hours}h`；minutes>0 → `${minutes}m`；seconds>0 **或** parts 为空 → `${seconds}s`
  - 返回 `` `Decision in ${parts.join(" ")}` ``（例：`Decision in 1h 2m 3s`）
- `formatCacheWarmingStatus(status, now = Date.now())`：
  ```
  decision = status.decision
  if (!decision || (status.state === "inactive" && !decision.economicsAvailable && !status.extensionOverride))
      return `Inactive (${status.reason ?? "unknown reason"})`
  details = status.extensionOverride ? `extension override, ${formatCacheWarmingEconomics(decision)}`
                                     : `${formatCacheWarmingEconomics(decision)} -> ${decision.action}`
  if state === "inactive"   -> `Stopped (${details})`
  if state === "refreshing" -> `Warming cache (${details})`
  return `${formatCacheWarmingDecisionTime(status.nextWarmAt, now)} (${details})`
  ```
- `formatCacheWarmingUsage(entry)`：
  ```
  note = entry.note ? ` (${entry.note})` : ""
  cost = entry.usage.cost.total.toFixed(6).replace(/(\.\d{3}\d*?)0+$/, "$1")
  return `Cache warmed${note}: $${cost}`
  ```
  即固定 6 位小数后把「小数点后第 4 位起的尾部 0」去掉，**但捕获组 `\.\d{3}` 至少吃掉 3 位**，因此结果总是保留 3 位小数（因为惰性 `\d*?` 让 `0+$` 直接吞掉剩余全部 0）。**已用 Node 实测**：`0`→`0.000`、`0.1`→`0.100`、`0.05`→`0.050`、`0.123`→`0.123`、`0.1234`→`0.1234`、`0.123456`→`0.123456`、`1`→`1.000`、`1.5`→`1.500`、`12.3456789`→`12.345679`、`100`→`100.000`。Rust 实现必须复刻此正则的效果（等价于「`format!("{:.6}")` 后，若小数部分全为 0 则保留 3 位，否则去掉第 4 位起的尾部 0，且至少 3 位」）。
  注意该正则只作用于含小数点的情况：没有小数点的值（如 `toFixed(6)` 永远有）不会走到。
- `usage kind` 为 `"cache_warm"`；扩展覆盖时 `note = "extension override"`。

### 2.29 `cache-stats.ts`（174 行）

```ts
export const CACHE_TTL_MS = 5 * 60 * 1000;      // 300000，Anthropic 默认缓存 TTL
const NOISE_FLOOR_TOKENS = 1024;                 // 每轮低于等于此值视为缓存断点粒度噪声

export interface CacheMiss {
	missedTokens: number;    // 上一轮 prompt 中存在但本次未从缓存读取的 prompt tokens
	missedCost: number;      // 相比完全命中多付的美元；价格未知时为 0
	idleMs: number;          // 距上次请求的毫秒数（上次请求刷新了缓存）
	modelChanged: boolean;   // 模型相对上次请求是否改变
}
export interface CacheWasteTotals { missedTokens: number; missedCost: number; missCount: number; }
export interface ModelPriceSource {
	getModel(provider: string, modelId: string): { cost: { cacheRead: number } } | undefined;
}
interface PreviousRequest {
	promptTokens: number; modelKey: string; timestamp: number; reportedCache: boolean;   // reportedCache 为 sticky
}
```

#### 2.29.1 `detectMiss(prev, message, models)`

```
usage = message.usage
promptTokens = usage.input + usage.cacheRead + usage.cacheWrite
if !prev || promptTokens <= 0 || (usage.cacheRead + usage.cacheWrite === 0 && !prev.reportedCache) -> undefined
missedTokens = Math.min(prev.promptTokens, promptTokens) - usage.cacheRead
if missedTokens <= 1024 -> undefined
paidTokens = usage.input + usage.cacheWrite
paidPerToken = paidTokens > 0 ? (usage.cost.input + usage.cost.cacheWrite) / paidTokens : 0
readPerToken = usage.cacheRead > 0 ? usage.cost.cacheRead / usage.cacheRead
                                  : (models.getModel(message.provider, message.model)?.cost.cacheRead ?? 0) / 1_000_000
return {missedTokens,
        missedCost: missedTokens * Math.max(0, paidPerToken - readPerToken),
        idleMs: Math.max(0, message.timestamp - prev.timestamp),
        modelChanged: `${message.provider}/${message.model}` !== prev.modelKey}
```

#### 2.29.2 `asPreviousRequest(message, reportedCache)`

`promptTokens = input+cacheRead+cacheWrite`；`<= 0` → undefined；否则 `{promptTokens, modelKey: \`${provider}/${model}\`, timestamp, reportedCache: reportedCache || cacheRead+cacheWrite > 0}`。

#### 2.29.3 `scan(entries, models)`

```
prev = undefined; totals = {0,0,0}; misses = new Map()
for entry of entries:
    if entry.type === "compaction" || entry.type === "branch_summary": prev = undefined; continue
    if entry.type === "usage" && entry.kind === "cache_warm":
        promptTokens = entry.usage.input + cacheRead + cacheWrite
        if promptTokens > 0: prev = {promptTokens, modelKey: `${entry.provider}/${entry.model}`,
                                     timestamp: Date.parse(entry.timestamp), reportedCache: true}
    else if entry.type === "message" && entry.message.role === "assistant":
        miss = detectMiss(prev, entry.message, models)
        if miss: totals += ; missCount += 1; misses.set(entry.message, miss)
        prev = asPreviousRequest(entry.message, prev?.reportedCache ?? false) ?? prev
return {prev, totals, misses}
```
注意：模型切换**不**重置 prev（会被计为 miss）；压缩与分支摘要会重置。

#### 2.29.4 导出函数

- `computeCacheWaste(entries, models): CacheWasteTotals` = `scan(...).totals`
- `collectCacheMisses(entries, models): Map<AssistantMessage, CacheMiss>` = `scan(...).misses`（按消息**引用**为键）
- `detectCacheMiss(entries, message, models)`：`detectMiss(scan(entries, models).prev, message, models)`；调用时 `entries` **尚不包含** `message`（`message_end` 早于持久化）。

### 2.30 `usage-totals.ts`（99 行）

```ts
export interface UsageTotals { input: number; output: number; cacheRead: number; cacheWrite: number; cost: number; }
export function createUsageTotals(): UsageTotals   // 全 0
export function addUsageToTotals(totals, usage): void {
	totals.input += usage.input; totals.output += usage.output;
	totals.cacheRead += usage.cacheRead; totals.cacheWrite += usage.cacheWrite;
	totals.cost += usage.cost.total;
}
```

#### 2.30.1 `combineUsage(first, second): Usage`

```
{
  input: a+b, output: a+b, cacheRead: a+b, cacheWrite: a+b,
  ...(first.cacheWrite1h !== undefined || second.cacheWrite1h !== undefined
      ? {cacheWrite1h: (first.cacheWrite1h ?? 0) + (second.cacheWrite1h ?? 0)} : {}),
  ...(first.reasoning !== undefined || second.reasoning !== undefined
      ? {reasoning: (first.reasoning ?? 0) + (second.reasoning ?? 0)} : {}),
  totalTokens: a+b,
  cost: {input, output, cacheRead, cacheWrite, total 各自相加,
         totalFromProvider: first.cost.totalFromProvider === true && second.cost.totalFromProvider === true},
}
```
要点：可选字段只在**任一侧**出现时才出现（否则键完全缺失，而非 0）；`totalFromProvider` 要求双方都为 `true`（缺失视为 false）。

#### 2.30.2 `getUsageCostBreakdown(entries): UsageCostBreakdownEntry[]`

```ts
export interface UsageCostBreakdownEntry { key: string; cost: number; tokens: number; }
```
分桶规则（按条目顺序，后者继续累加到同 key）：
| 条目 | key |
|---|---|
| `type === "message" && message.role === "assistant"` | `` `${message.provider}/${message.responseModel ?? message.model}` `` |
| `type === "usage"` | `` `${entry.provider}/${entry.model}` `` |
| `type === "message" && message.role === "toolResult" && message.usage` | `"Tools/summaries"` |
| (`type === "branch_summary"` 或 `"compaction"`) && `entry.usage` | `"Tools/summaries"` |
- 无 key 或无 usage → 跳过。
- 结果：`Array.from(totalsByKey, ([key, totals]) => ({key, cost: totals.cost, tokens: totals.input+output+cacheRead+cacheWrite}))`，过滤 `cost > 0 || tokens > 0`，按 `b.cost - a.cost` **降序**排序（同额时保持 Map 插入顺序 —— `Array.prototype.sort` 在 V8 中是稳定排序；Rust 需用稳定排序）。

### 2.31 `scripts/generate-schemas.ts`（163 行）

生成 4 个 JSON Schema 文件。导出 `renderConfigSchemas(): ReadonlyMap<string, string>`（路径 → 内容），CLI 入口在 `process.argv[1]` 解析后等于本模块 URL 时执行 `generateSchemas()`。

#### 2.31.1 常量（逐字）

```ts
const packageDirectory = fileURLToPath(new URL("..", import.meta.url));   // packages/coding-agent
const schemaBaseUrl = "https://kiana.dev/schemas";
const schemaDraft = "https://json-schema.org/draft/2020-12/schema";
const generatedComment =
	"This file is generated from TypeScript source. Do not edit it manually; run npm run generate:schemas.";
const schemaAnnotationKeys = new Set([
	"default", "deprecated", "description", "examples", "readOnly", "title", "writeOnly",
]);
```

#### 2.31.2 `schemaArtifacts`（顺序固定，逐字）

| # | filename | schema | definitions |
|---|---|---|---|
| 1 | `models.schema.json` | `ModelsConfigSchema` | `ModelCost`→`ModelCostSchema`、`ModelInputLimits`→`ModelInputLimitsSchema`、`ModelPromptCache`→`ModelPromptCacheSchema`、`ProviderCompat`→`ProviderCompatSchema`、`ThinkingLevelMap`→`ThinkingLevelMapSchema`（**键序按此**） |
| 2 | `settings.schema.json` | `SettingsSchema` | `ModelThinkingLevel`→`ModelThinkingLevelSchema` |
| 3 | `keybindings.schema.json` | `KeybindingsSchema` | `KeybindingValue`→`KeybindingValueSchema` |
| 4 | `theme.schema.json` | `ThemeJsonSchema` | `ColorValue`→`ColorValueSchema` |

#### 2.31.3 `splitAnnotations(schema)`

把对象自身的键分成 `shape`（非注解键）与 `annotations`（`schemaAnnotationKeys` 中的 7 个键），**保持原始键序**。

#### 2.31.4 `addDefinitions(schema, definitions)`

1. `preparedDefinitions = Object.entries(definitions).map(([name, definition]) => { name 必须匹配 /^[A-Za-z][A-Za-z0-9_-]*$/，否则抛 \`Invalid schema definition name: ${name}\`【逐字】; {name, shape, shapeKey: JSON.stringify(shape)} })`
2. `referencedDefinitions = new Set<string>()`
3. `replaceDefinitions(value, excludedName?)` 递归：
   - 数组 → 逐项递归。
   - 非对象 / null → 原样返回。
   - `splitAnnotations(value)` 后 `shapeKey = JSON.stringify(shape)`；在 `preparedDefinitions` 中找 **名称不等于 `excludedName`** 且 `shapeKey` 相同的第一项 → 命中则 `referencedDefinitions.add(name)`，返回 `{$ref: "#/$defs/<name>", ...annotations}`。
   - 否则返回 `Object.fromEntries(逐项：[key, schemaAnnotationKeys.has(key) ? child : replaceDefinitions(child, excludedName)])`（注解键的值**不**递归）。
4. `compactSchema = replaceDefinitions(schema)`。
5. `compactDefinitions = Object.fromEntries(preparedDefinitions.map(({name, shape}) => [name, replaceDefinitions(shape, name)]))`（**每个定义排除自身**，避免自我引用）。
6. `unusedDefinitions = preparedDefinitions 中未被引用的名字`；非空 → 抛 `Unused schema definitions: ${names.join(", ")}`【逐字】。
7. 返回 `{...compactSchema, $defs: compactDefinitions}`（`$defs` 追加在最后；若 schema 自身已有 `$defs` 会被覆盖）。

#### 2.31.5 `serializeSchema(artifact)`

```ts
schema = artifact.definitions ? addDefinitions(artifact.schema, artifact.definitions) : artifact.schema
return JSON.stringify({
	$schema: "https://json-schema.org/draft/2020-12/schema",
	$id: `https://kiana.dev/schemas/${artifact.filename}`,
	$comment: generatedComment,
	...schema,
}, null, 2) + "\n"
```
注意：`$schema`/`$id`/`$comment` 在前，随后展开 `schema` 的键（`schema` 若含同名键会覆盖前三者，实际不会）。文件**有**末尾换行。

#### 2.31.6 `generateSchemas()`

按 `renderConfigSchemas()` 的插入顺序写文件：`path = resolve(packageDirectory, relativePath)`、`mkdirSync(dirname(path), {recursive:true})`、`writeFileSync(path, content, "utf-8")`、`console.log(\`Generated packages/coding-agent/${relativePath}\`)`（stdout，逐字）。

### 2.32 `scripts/generate-references.ts`（177 行）

生成 `docs/models-configuration.md`。导出 `renderReferences(): ReadonlyMap<string, string>`；CLI 入口同上。

#### 2.32.1 常量

```ts
const generatedComment =
	"This file is generated from TypeScript source. Do not edit it manually; run npm run generate:references.";
```

#### 2.32.2 `compatSchemaApis`

```
compatSchemaApis = new Map<TSchema, string[]>()
for ([api, schema] of Object.entries(CompatSchemasByApi))
    compatSchemaApis.set(schema, [...(get(schema) ?? []), api])
```
即按 schema **对象身份**分组，值为使用该 schema 的 api 名字列表（首见顺序）。`formatList(items)`：长度 > 1 → `前 n-1 项以 ", " 连接` + ` and ` + 最后一项；否则 `items[0] ?? ""`。

#### 2.32.3 `referenceArtifacts`（唯一产物）

`path: "docs/models-configuration.md"`，`title: "Models Configuration"`，`intro` 为两段以 `"\n\n"` 连接（逐字）：
```
This reference lists the fields of `models.json`. See [Configure a compatible endpoint](models.md#configure-a-compatible-endpoint) for examples and [Configuration](configuration.md#agent-directory) for the file location.

Add `"$schema": "https://kiana.dev/schemas/models.schema.json"` to `models.json` for editor validation and completion.
```
`sections`（顺序固定）：

| # | heading | intro（逐字） | schema | links / collapsed |
|---|---|---|---|---|
| 1 | `Providers` | `Each key under `providers` is a provider ID. A built-in provider ID changes that provider. Any other ID adds a provider, which needs `baseUrl` and an `api` at provider or model level.` | `ModelsJsonSchemas.provider` | links：`models`→`#models`、`modelOverrides`→`#model-overrides`、`compat`→`#compatibility` |
| 2 | `Models` | `Each `models` entry adds a model to the provider. An entry whose `id` matches an existing model replaces it.` | `ModelsJsonSchemas.model` | links：`inputLimits`→`#input-limits`、`compat`→`#compatibility` |
| 3 | `Model overrides` | `Each `modelOverrides` key is the ID of a built-in or extension model. Unknown IDs are ignored. Object fields, such as `cost`, merge with the model's value field by field; other fields replace it.` | `ModelsJsonSchemas.modelOverride` | links：`inputLimits`、`compat` 同上 |
| 4 | `Input limits` | `See [Describe model input and caching](models.md#describe-model-input-and-caching).` | `ModelInputLimitsSchema` | — |
| 5 | `Compatibility` | `` `compat` describes verified differences from an API's standard request and response behavior. Model values override provider values per field. The fields that apply depend on the model's `api`. `` | —（无 schema，只是标题与 intro） | — |
| 6..n | `` ### `<api>` `` | `Also applies to ${formatList(aliases.map(a => `\`${a}\``))}.`（无 aliases 时无 intro） | 该 api 的 compat schema | collapsed：`["openRouterRouting", "vercelGatewayRouting"]` |

第 6 段起由 `[...compatSchemaApis].map(([schema, [api, ...aliases]]) => ...)` 生成；heading 以 `### ` 开头（渲染时不再加 `## `）。

#### 2.32.4 `referenceType(schema)`

```
if schema.const !== undefined -> JSON.stringify(schema.const)
if schema.enum              -> schema.enum.map(v => JSON.stringify(v)).join(" | ")
if schema.anyOf             -> [...new Set(schema.anyOf.map(referenceType))].join(" | ")
if schema.type === "array"  -> item = schema.items ? referenceType(schema.items) : "unknown";
                               item.includes(" | ") ? `(${item})[]` : `${item}[]`
return schema.type ?? "unknown"
```

#### 2.32.5 `documentedObject(schema)`

- 数组：`item = schema.items && documentedObject(schema.items)`；有则 `{suffix: `[]${item.suffix}`, schema: item.schema}`。
- 否则 `object = schema.properties ? schema : schema.anyOf?.find(b => b.properties)`。
- `documented = Object.values(object?.properties ?? {}).some(f => f.description !== undefined || f.default !== undefined)`。
- 返回 `object && documented ? {suffix: "", schema: object} : undefined`。

#### 2.32.6 `escapeTableText(text)`

`text.replaceAll("|", "\\|").replaceAll("<", "&lt;")`（**只转这两类**）。

#### 2.32.7 `referenceRows(schema, section, prefix = "")`

对 `Object.entries(schema.properties ?? {})` 逐个：
```
path = `${prefix}${name}`
link = prefix === "" ? section.links?.[name] : undefined
type = escapeTableText(referenceType(field))
typeText = /^[a-z]+$/.test(type) ? type : `\`${type}\``
defaultText = field.default === undefined ? "" : `\`${escapeTableText(JSON.stringify(field.default))}\``
description = [schema.required?.includes(name) ? "Required." : undefined, field.description]
              .filter(t => t !== undefined).join(" ")
row = `| \`${path}\` | ${link ? `[${typeText}](${link})` : typeText} | ${defaultText} | ${escapeTableText(description)} |`
rows.push(row)
if (link || (prefix === "" && section.collapsed?.includes(name))) continue
nested = documentedObject(field)
if (nested) rows.push(...referenceRows(nested.schema, section, `${path}${nested.suffix}.`))
```

#### 2.32.8 `renderReference(artifact)`

```
blocks = [`# ${title}`, `<!-- ${generatedComment} -->`, intro]
for section of sections:
    blocks.push(section.heading.startsWith("#") ? section.heading : `## ${section.heading}`)
    if section.intro: blocks.push(section.intro)
    if section.schema:
        rows = referenceRows(section.schema, section)
        blocks.push(["| Field | Type | Default | Description |", "|---|---|---|---|", ...rows].join("\n"))
return blocks.join("\n\n") + "\n"
```
即：块之间空行，文件**有**末尾换行；表格四列表头逐字 `| Field | Type | Default | Description |`，分隔行 `|---|---|---|---|`。

#### 2.32.9 `generateReferences()`

与 `generateSchemas()` 相同：`mkdirSync` + `writeFileSync` + `console.log(\`Generated packages/coding-agent/${relativePath}\`)`。

---

## 3. 用户可见面（逐字）

本单元不直接渲染 TUI 画面，但向 TUI/CLI 提供**所有**用户可见文案、键位默认值、状态行与磁盘格式错误。凡标注【逐字】者测试与快照会逐字节比对。

### 3.1 本单元直接输出的 stdout/stderr

仅 `model-resolver.ts` 有直接输出（其余通过返回值交给上层渲染）：

| 位置 | 流 | 颜色 | 文本 |
|---|---|---|---|
| `resolveModelScope` | **stderr**（`console.warn`） | `chalk.yellow` | `Warning: ${diagnostic.message}` |
| `restoreModelFromSession` 成功 | stdout（`console.log`） | `chalk.dim` | `Restored model: ${provider}/${id}` |
| `restoreModelFromSession` 失败 | **stderr**（`console.error`） | `chalk.yellow` | `Warning: Could not restore model ${provider}/${id} (${reason}).` |
| 回退到当前模型 | stdout | `chalk.dim` | `Falling back to: ${provider}/${id}` |
| 回退到可用模型 | stdout | `chalk.dim` | `Falling back to: ${provider}/${id}` |

`reason` 只能是 `model no longer exists` 或 `no auth configured`。
`chalk` 在非 TTY 或 `NO_COLOR`/`FORCE_COLOR` 下按 chalk 6 规则降级；Rust 侧需实现等价的颜色使能判定（见 `tui-core` 单元的颜色规范）。`yellow` = SGR 33，`dim` = SGR 2，重置 `\x1b[39m` / `\x1b[22m`。

### 3.2 模型相关的用户可见错误与警告（全部【逐字】）

**`resolveCliModel`**：
- `No models available. Check your installation or add models to models.json.`
- `Unknown provider "${cliProvider}". Use --list-models to see available providers/models.`
- `Model "${cliModel}" is ambiguous across providers: ${matches}. ${authHint} Use --provider or provider/model.`
  - `authHint` 三选一：`--api-key does not choose a provider.` / `No matching provider is authenticated.` / `More than one matching provider is authenticated.`
  - `matches` = `provider/id` 列表，按 `localeCompare` 升序、以 `", "` 连接
- `Model "${cliModel}" not found. Use --list-models to see available models.`
- 自定义模型回退警告：`Model "${modelId}" not found for provider "${target.provider}". Using custom model id.`

**scope 诊断**（`resolveModelScopeFromModels` → `diagnostics`，由 `resolveModelScope` 打成 `Warning: ...`）：
- `No models match pattern "${pattern}"`（code `no-match`）
- `Pattern "${pattern}" matches several providers: ${providers}. Use provider/id to choose one.`（code `ambiguous`，`providers` 去重后 `localeCompare` 升序、`", "` 连接）
- `Invalid thinking level "${suffix}" in pattern "${text}". Using default instead.`（code `invalid-thinking-level`）

**会话恢复**：
- `Could not restore model ${provider}/${id} (${reason}). Using ${fallback.provider}/${fallback.id}.`（`fallbackMessage`）

**模型运行时错误**（`ModelsError` 的 message）：
- `Unknown provider: ${model.provider}`
- `Provider is not configured: ${model.provider}`
- `Provider ${model.provider} does not support deferred responses`
- `Provider ${model.provider} does not support image generation`
- `Provider ${model.provider} does not support classification`
- `Credential ${operation} committed for ${providerId}, but local synchronization failed`（`CredentialSynchronizationError`）

**虚拟模型错误**：
- `Virtual model ${provider}/${id} must be routed before streaming`
- `Virtual model ${provider}/${id} conflicts with a physical model.`
- `Virtual model provider and id must not be empty.`
- `Virtual model ${provider}/${id} is not registered.`
- `Virtual model ${provider}/${id} routed to ${p}/${id}, which is not a physical model.`
- `Virtual model ${provider}/${id} routed to ${p}/${id}, which has no credentials.`

**provider 合成错误**（显示于 `ModelRuntime.getError()`）：
- `Provider ${id}, model ${modelId}: no "api" specified. Set at provider or model level.`
- `Provider ${id}, model ${modelId}: no "api" specified. Set it at model level.`（image/classifier 无后缀）
- `Provider ${id}, model ${modelId}: no "api" specified. Set it at model level or provider level.`（chat）
- `Provider ${id}: "baseUrl" is required when defining custom models.`
- `Provider ${id}: "baseUrl" is required when "oauth" is set.`
- `Provider ${id}: must specify "baseUrl", "headers", "compat", "modelOverrides", or "models".`
- `Provider ${id}: no authentication method configured.`
- `Provider ${id}: "api" is required when registering streamSimple.`
- `authHeader requires a resolved API key`
- `No API provider registered for api: ${model.api}`
- `Provider ${id} has no image implementation for "${model.api}"`
- `Provider ${id} has no classifier implementation for "${model.api}"`
- `Invalid model catalog for provider "${id}"`
- `Model catalog request failed for ${id}: ${status}`
- `Provider id must not be empty.`
- `Provider config is required when registering by name`
- `No API key found for "${provider}"`（`ModelRegistry.getApiKeyAndHeaders`）

**`getError()` 组装**：`configError` + 每个 `` Provider "${id}": ${error} `` + `Availability refresh: ${error}`，以 `"\n\n"` 连接。

**models.json 错误**：`Failed to load models.json: ...\n\nFile: ${path}` / `Failed to parse models.json: ...\n\nFile: ${path}` / `Invalid models.json schema:\n  - <path>: <message>\n\nFile: ${path}`（详见 §2.18）。

### 3.3 设置相关用户可见错误（全部【逐字】）

- `Invalid ${settingName} setting: ${String(value)}`（如 `Invalid editorPaddingX setting: 5`）
- `Invalid httpIdleTimeoutMs setting: ${String(timeoutMs)}`
- `Invalid HTTP idle timeout: ${String(timeoutMs)}`
- `Invalid compaction.${field} setting: ${String(ordinary)}. Expected a non-negative safe integer.`
- `Invalid compaction.modelOverrides["${modelKey}"] setting: ${String(entry)}. Expected an object.`
- `Invalid compaction.modelOverrides["${modelKey}"].${field} setting: ${String(override)}. Expected a non-negative safe integer.`
- `Project is not trusted; refusing to write project settings`
- `Failed to acquire settings lock`
- `tool names cannot be mixed with +name or -name entries`
- `` `+name and -name entries take exact tool names, not patterns: ${pattern}` ``
- `Invalid settings file ${path}: ${message}` / `Invalid ${scope} settings: ${message}`（诊断）
- settings 文件解析失败诊断：`Invalid settings file ${path}: ...`（`settings-diagnostics.test.ts` 断言含此前缀）

### 3.4 凭证存储用户可见错误（全部【逐字】）

- `Failed to read auth.json: ${message}`
- `Invalid auth.json: expected an object`
- `Invalid auth.json credential for provider "${providerId}"`
- `Read-only credential storage cannot modify auth.json`
- `Auth storage lock was compromised`

### 3.5 信任相关用户可见文案（全部【逐字】）

- 提示：`Trust project folder?\n${cwd}\n\nThis allows ${APP_NAME} to load ${CONFIG_DIR_NAME} settings and resources, install missing project packages, and execute project extensions.`
- 选项标签（顺序固定）：
  1. `Trust`
  2. `Trust parent folder (${parentPath})`（仅当存在父目录）
  3. `Trust (this session only)`
  4. `Do not trust`
  5. `Do not trust (this session only)`
- 错误：`Failed to read trust store ${path}: ${message}` / `Invalid trust store ${path}: expected an object` / `Invalid trust store ${path}: value for ${JSON.stringify(key)} must be true, false, or null` / `Failed to acquire trust store lock`
- 扩展错误：`Extension "${path}" project_trust error: ${error}`

### 3.6 配置值解析错误（全部【逐字】）

- `Failed to resolve ${description} from shell command: ${cmd}`
- `Failed to resolve ${description} from environment variable: ${name}`
- `Failed to resolve ${description} from environment variables: ${name1}, ${name2}`
- `Failed to resolve ${description}`
- description 形如：`API key for provider "${providerId}"`、`provider "${providerId}" header "${key}"`、`model "${provider}/${id}" header "${key}"`

### 3.7 `/session` 中的缓存预热状态行（逐字模板，来自 §2.28.9）

- 未激活：`Inactive (${reason})`，reason 缺省 `unknown reason`
- 已停止：`Stopped (${details})`
- 正在预热：`Warming cache (${details})`
- 已排期：`${"Decision now" | "Decision in 1h 2m 3s"} (${details})`
- `details` 为 `extension override, <economics>` 或 `<economics> -> warm|stop`
- `<economics>` = `cache economics unavailable` 或 `<probability>% continuation probability[ while agent is running], expected savings $X.XXX >=|< $0.050`
- 记录行：`Cache warmed[ (extension override)]: $<cost>`

### 3.8 键位（用户可见的按键行为）

见 §2.27.2 的 43 个 `app.*` 默认键与 §2.27.3 的 4 个 TUI 覆盖。TUI 中按键帮助（如 `/help`、状态栏 hint）读取的 `description` 字符串必须逐字一致（见 §2.27.2 表格右列）。

### 3.9 CLI 参数面（由 `ca-cli` 解析，本单元消费）

`--provider <id>`、`--model <pattern>`、`--thinking <level>`、`--api-key <key>`、`--models <p1,p2,...>`（逗号分隔、trim、去空）、`--list-models [search]`、`--offline`。
`--thinking` 非法值由 `ca-cli` 报错（`isValidThinkingLevel`）；`DEFAULT_THINKING_LEVEL = "medium"`，合法等级 `off|minimal|low|medium|high|xhigh|max`。

### 3.10 ANSI

本单元自身不产生 ANSI 序列，只经由 `chalk`（§3.1）。所有 TUI 渲染细节见 `tui-*` 单元。缓存预热/用量相关的**文本**由本单元给出，不含颜色。

---

## 4. 外部格式（逐字）

### 4.1 磁盘布局

| 路径 | 说明 |
|---|---|
| `<agent-dir>/settings.json` | 全局设置 |
| `<cwd>/.kiana/settings.json` | 项目设置（`CONFIG_DIR_NAME` = `.kiana`） |
| `<agent-dir>/models.json` | 自定义 provider/model（JSONC） |
| `<agent-dir>/models-store.json` | kiana.dev 目录缓存（与 models.json 同目录） |
| `<agent-dir>/auth.json` | 凭证（0600；目录 0700） |
| `<agent-dir>/keybindings.json` | 按键覆盖 |
| `<agent-dir>/trust.json` | 项目信任 |
| `<agent-dir>/trust.json.lock` | trust 锁文件（`lockfilePath: ${path}.lock`，锁**目录**） |
| `<agent-dir>/auth.json.lock` 等 | proper-lockfile 默认 `${file}.lock` |
| `~/.agents/skills`、`<cwd>/**/.agents/skills` | 视为需要信任的资源（`skills`），但 `$HOME/.agents/skills` **不**触发信任 |

`<agent-dir>` = `KIANA_CODING_AGENT_DIR` 或 `~/.kiana/agent`。所有路径经 `normalizePath`（Windows 下扩展名与大小写归一）后使用。

### 4.2 `settings.json`

- 编码 UTF-8，允许 BOM（`stripBom`）。
- 写入格式：`JSON.stringify(settings, null, 2)`，**无末尾换行**；仅在写入时 `mkdirSync(dirname, {recursive:true})`。
- 顶层键见 §2.8.3 的 **59** 个字段（含 `$schema`，与 `schemas/settings.schema.json` 的 `properties` 数量一致）；未知键被**保留**（读取时按 TypeBox 结构宽松解析，写回时按 `modifiedFields` 选择性写）。
- 合并：global 为底，project 覆盖；`deepMergeObjects` 规则见 §2.9（跳过 `undefined`，plain object 递归，数组整体替换）。
- 4 个迁移见 §2.9.7（`queueMode→steeringMode`、`websockets→transport`、`skills` 对象→数组、`retry.maxDelayMs→retry.provider.maxRetryDelayMs`）。

### 4.3 `models.json`（JSONC）

- 允许 `//` 行注释与尾逗号（**不允许** `/* */`、单引号、无引号键）。
- 顶层：`{ "$schema"?: string, "providers": { [providerId]: Provider } }`；`providers` 必需。
- `Provider` 字段（见 §2.18.1）：`name?`、`baseUrl?`、`apiKey?`、`api?`、`headers?`、`authHeader?`（默认 false）、`compat?`、`modelOverrides?`、`models?`、`oauth?: "radius"`。
- Model 字段：`id`（必填，minLength 1）、`name?`、`api?`、`baseUrl?`、`reasoning?`（默认 false）、`input?`（默认 `["text"]`）、`inputLimits?`、`cost?`（默认全 0）、`promptCache?`、`contextWindow?`（默认 128000，`exclusiveMinimum: 0`）、`maxTokens?`（默认 16384，`exclusiveMinimum: 0`）、`thinkingLevelMap?`、`samplingParams?`、`samplingParamsByThinkingLevel?`、`compat?`。
- 错误消息见 §3.2；`formatValidationPath` 规则见 §2.18.2。

### 4.4 `auth.json`

格式：`{ [providerId]: Credential }`，两种凭证：
```jsonc
{"type": "api_key", "key": "sk-...", "env": {"NAME": "value"}}   // key 可省略；env 值必须全是字符串
{"type": "oauth", "access": "...", "refresh": "...", "expires": 1234567890123, "env": {...}}  // expires 必须有限数字
```
（另可能包含 provider 特定字段如 OpenCode 的 `gatewayConfig`；校验只要求上述字段存在且类型正确，其余字段透传。）
- 权限：文件 **0600**，目录 **0700**（创建时设置）。
- 读取：`JSON.parse(stripBom(...))`；ENOENT → `{}`；顶层非对象/为数组 → `Invalid auth.json: expected an object`；单个凭证校验失败 → `Invalid auth.json credential for provider "${id}"`。
- 写入：`JSON.stringify(data, null, 2)`（无末尾换行）；持 proper-lockfile 同步锁（10 次 × 20 ms，仅 `ELOCKED` 重试）。
- `api_key` 且 `key` 非空且非 `!command` 时，`read()` 返回值中的 `key` 会被 `resolveConfigValue` 解析（`$ENV`）。

### 4.5 `models-store.json`

```jsonc
{ [providerId]: { "models": [...], "lastModified"?: number, "checkedAt"?: number, "etag"?: string } }
```
- 写入：`JSON.stringify(current, null, 2)`（无末尾换行），持锁（复用 `FileAuthStorageBackend`，因此首次写入/加锁时会 `ensureParentDir()` 以 **0700** 建目录、`ensureFileExists()` 以 **0600** 写入内容 `{}`）。
- 读取：结构化克隆（`structuredClone`）后返回；同一文件的并发读合并为一次真实读取（`readState.reload`，按 `getFileRevision` = `${dev}:${ino}:${size}:${mtimeNs}:${ctimeNs}` 判断是否需要重读），读者计数归零时 abort 该次读取控制器。

### 4.6 `trust.json`

```jsonc
{ "/absolute/path": true | false | null }
```
- 写入：键按 **默认字符串排序**（`Object.keys(data).sort()`，UTF-16 码元比较）；只写 `true/false/null` 值；`JSON.stringify(sorted, null, 2) + "\n"`（**有末尾换行**，与 settings/auth 不同）。
- 锁：`mkdirSync(dirname)` 后 `lockfile.lockSync(trustDir, {realpath:false, lockfilePath: `${path}.lock`})`，10 次 × 20 ms，仅 `ELOCKED` 重试。
- 读取：ENOENT → `{}`；解析失败 → `Failed to read trust store ${path}: ${message}`；非对象/数组 → `Invalid trust store ${path}: expected an object`；值非三者之一 → `Invalid trust store ${path}: value for ${JSON.stringify(key)} must be true, false, or null`。

### 4.7 `keybindings.json`

- `{ "$schema"?: string, [keybindingId]: KeyId | KeyId[] }`；未知键**保留**（`additionalProperties` 允许），但值必须是合法 KeyId 或 KeyId 数组，否则**静默丢弃**。
- 读取时：顶层非对象 → `{}`；JSON 解析失败 → `{}`（静默）；随后 `migrateKeybindingsConfig`（旧名→新名）→ `orderKeybindingsConfig`（按 `KEYBINDINGS` 顺序，其余按字典序追加）→ 逐值校验。

### 4.8 生成的 JSON Schema（4 个，逐字片段）

共同外壳（`serializeSchema`）：
```ts
JSON.stringify({ $schema: "https://json-schema.org/draft/2020-12/schema", $id, $comment, ...schema }, null, 2) + "\n"
```
- `$comment`（逐字）：`This file is generated from TypeScript source. Do not edit it manually; run npm run generate:schemas.`
- `$id`：`https://kiana.dev/schemas/models.schema.json`、`.../settings.schema.json`、`.../keybindings.schema.json`、`.../theme.schema.json`
- 生成顺序固定：models、settings、keybindings、theme。
- `keybindings.schema.json` 的 `properties` 为 `$schema` + 90 个 keybinding id（每个 `{anyOf:[KeyIdSchema, array of KeyIdSchema]}` 加 `description`），`additionalProperties: {$ref:"#/$defs/KeybindingValue"}`；`$defs.KeybindingValue` 即 §2.25 的 anyOf。
- 定义注入：`addDefinitions` 去重后以 `$ref` 引用，并把残留注解（`default, deprecated, description, examples, readOnly, title, writeOnly`）复制到 `$ref` 同级；错误：`Invalid schema definition name: ${name}`、`Unused schema definitions: ${names.join(", ")}`。

### 4.9 环境变量（本单元读取）

| 变量 | 读取点 | 语义 |
|---|---|---|
| `KIANA_CODING_AGENT_DIR` | `getAgentDir()` | `<agent-dir>` 覆盖 |
| `KIANA_CODING_AGENT_SESSION_DIR` | `getSessionsDir()` | 会话目录（本单元不直接用） |
| `KIANA_OFFLINE` | `ModelRuntime.create` | **存在**即禁用模型网络（`process.env.KIANA_OFFLINE === undefined` 取反） |
| `KIANA_TELEMETRY` | `isInstallTelemetryEnabled` | `1/true/yes`（大小写不敏感）为开，否则回落到 settings |
| `KIANA_EXPERIMENTAL` | `areExperimentalFeaturesEnabled` | 严格 `=== "1"` |
| `KIANA_CACHE_RETENTION` | `getPromptCacheTtlMs` → `getProviderEnvValue` | `long` → long 档 |
| `KIANA_HARDWARE_CURSOR` | `SettingsManager.getShowHardwareCursor` 回落 | 见 settings 单元 |
| `KIANA_CLEAR_ON_SHRINK` | `SettingsManager.getClearOnShrink` | 见 settings 单元 |
| `KIANA_RADIUS_GATEWAY` | `getRadiusGatewayUrl()`（`radius.ts`） | 覆盖 radius gateway |
| `HOME`（Windows 上由 `os.homedir()` / `USERPROFILE` 兜底） | `trust-manager.ts` 的 `hasTrustRequiringProjectResources`（局部变量，非具名函数） | `homeDir = canonicalizePath(resolvePath(process.env.HOME \|\| homedir()))`；用于判定 `$HOME/.agents/skills` 永不算信任需求（即使 cwd 是 `$HOME`） |
| `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY`（及小写变体） | `applyHttpProxySettings` → undici `EnvHttpProxyAgent` | 仅当未设置时 `??=` |
| `VISUAL` / `EDITOR` | `SettingsManager.getExternalEditor` 回落 | 见 settings 单元 |
| `WSL_DISTRO_NAME` / `WSL_INTEROP` | `useWindowsKeybindings` | linux 上的 Windows 键位判定 |
| `NO_COLOR` / `FORCE_COLOR` | chalk | 颜色使能（间接） |

### 4.10 kiana.dev 目录 HTTP 协议

- 方法 `GET`；URL：`${catalogBaseUrl}/api/models/providers/${encodeURIComponent(providerId)}?types=chat%2Cimage%2Cclassifier`（`catalogBaseUrl` 默认 `https://kiana.dev`；查询串由 `URL.searchParams.set` 生成，逗号编码为 `%2C`）。
- 请求头：`accept: application/json`、`User-Agent: <getKianaUserAgent(VERSION)>`、条件请求时 `if-none-match: <etag>`。
- 单次尝试超时 4 000 ms（`REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS`，以 `attemptTimeoutMs` 传入，**不改** `maxRetries`/`retryOnStatus`/`timeoutMs` 默认）；`fetchWithRetry` 默认最多重试 2 次、可重试状态 `{408,425,429,500,502,503,504}`、每次尝试独立 `AbortSignal.timeout` 并与外部 signal 用 `AbortSignal.any` 合并、重试前取消响应 body（完整选项见 §5.1.5）。
- 状态处理：`304`（有缓存）→ 只更新 `checkedAt`；`404`/`501` → 持久化 `{models: [], checkedAt, lastModified: 0, etag: undefined}`；其它非 2xx → 持久化 `checkedAt`（保留原 models/etag）并抛 `Model catalog request failed for ${id}: ${status}`；2xx → 解析并持久化。
- 响应体接受三种形状：数组；`{models: [...]}`；以 id 为键的对象映射。缺 `id` 的条目与不受支持的 `type` 被丢弃；`provider` 被强制改写为请求的 providerId。
- 新鲜度：`last-modified` 头解析为毫秒（`Date.parse`），非法/缺失 → 0；仅当 `lastModified > localGeneratedAt`（或 `localGeneratedAt` 未定义）时才作为覆盖层。

---

## 5. 跨单元接口（精确类型）

### 5.1 本单元**消费**的外部类型（必须由依赖单元提供，字段名逐字）

#### 5.1.1 来自 `ai-core`（`@earendil-works/kiana-ai`）

```ts
// 模型
export type Api = ...;                 // 字符串联合，如 "anthropic-messages" | "openai-completions" | ...
export type ImageApi = string;         // 图像 API 联合
export type ClassifierApi = string;
export type ModelType = "chat" | "image" | "classifier";
export interface ModelTypeMap { chat: Model<Api>; image: ImageModel<ImageApi>; classifier: ClassifierModel<ClassifierApi> }
export type AnyModel = Model<Api> | ImageModel<ImageApi> | ClassifierModel<ClassifierApi>;
export type KnownProvider = string;    // 已知 provider id 的联合（用于 defaultModelPerProvider 键）
export type ModelThinkingLevel = "off"|"minimal"|"low"|"medium"|"high"|"xhigh"|"max";

export interface Model<TApi extends Api> {
	id: string; name: string; api: TApi; provider: string; baseUrl: string;
	reasoning: boolean;
	thinkingLevelMap?: ThinkingLevelMap;          // Partial<Record<ModelThinkingLevel, string|null>>
	input: ("text" | "image")[];
	inputLimits?: InputLimits;
	cost: ModelCost;                              // {input,output,cacheRead,cacheWrite,tiers?} $/Mtok
	promptCache?: { short?: number; long?: number };  // 秒
	contextWindow: number; maxTokens: number;
	samplingParams?: Record<string, unknown>;
	samplingParamsByThinkingLevel?: Partial<Record<ModelThinkingLevel, Record<string, unknown>>>;
	headers?: Record<string, string>;
	compat?: ProviderCompat;                      // 含 forceAdaptiveThinking、openRouterRouting、vercelGatewayRouting、chatTemplateKwargs、chatTemplateArgs
	type?: ModelType;                             // 缺省视为 "chat"（getModelType）
}
export interface ImageModel<TApi> extends ... { type: "image"; output: ("text"|"image")[] }
export interface ClassifierModel<TApi> extends ... { type: "classifier"; contextWindow: number; compat?: ... }

export interface Usage {
	input: number; output: number; cacheRead: number; cacheWrite: number;
	cacheWrite1h?: number; reasoning?: number; totalTokens: number;
	cost: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number; totalFromProvider?: boolean };
}
export function calculateCost(model: Model<Api>, usage: Usage): Usage["cost"];
export function getModelType(model: { type?: ModelType }): ModelType;
export function isModelType<T>(model: {type?: ModelType}, type: T): boolean;
export function modelsAreEqual(a: Model<Api>, b: Model<Api>): boolean;      // type + id + provider
export function getSupportedThinkingLevels(model: Model<Api>): ModelThinkingLevel[];
export function clampThinkingLevel(model: Model<Api>, level: ModelThinkingLevel): ModelThinkingLevel;
export function normalizeContext(context: Context): TranscriptContext;
export function lazyStream(model: AnyModel, factory: () => Promise<AssistantMessageEventStream>): AssistantMessageEventStream;
export class ModelsError extends Error { constructor(code: "auth"|"provider"|"oauth", message: string, options?) }
export function formatUserAgent(identity: {userAgent: "detailed"; version: string}): string;
export const KIANA_ATTRIBUTION: ClientAttribution;   // = { name: "kiana", url: "https://kiana.dev", categories: ["cli-agent"] }（ai/utils/identity.ts:24，**对象常量，不是 string**）
export function getOpenCodeSessionHeaders(model: AnyModel, sessionId?: string): ProviderHeaders | undefined;
export function getProviderEnvValue(name: string, env?: Record<string,string>): string | undefined;
export function createModels(options: CreateModelsOptions): MutableModels;
export function getApiProvider(api: string): {stream, streamSimple} | undefined;   // 来自 "@earendil-works/kiana-ai/compat"
export function assertChatModel(model: AnyModel): void;         // 错误 `Model ${provider}/${id} is not a chat model`
export function assertImageModel(model: AnyModel): void;
export function assertClassifierModel(model: AnyModel): void;
export function assertClassifierInputSupported(model: ClassifierModel<any>, context: ClassifierContext): void;
export function imageErrorResult(model, error, aborted?): AssistantImages;
export function classifierErrorResult(model, error, aborted?): ClassifierResult;

export interface Provider {
	id: string; name: string; baseUrl?: string;
	headers?: ProviderHeaders;
	auth: { apiKey?: ApiKeyAuth; oauth?: OAuthAuth };
	getModels(): readonly Model<Api>[];
	getAllModels?(): readonly AnyModel[];
	refreshModels?(context: RefreshModelsContext): Promise<void>;
	filterModels?(models, credential?: Credential): readonly Model<Api>[];
	filterAllModels?(models, credential?: Credential): readonly AnyModel[];
	stream(model, context, options?): AssistantMessageEventStream;
	streamSimple(model, context, options?): AssistantMessageEventStream;
	fetchDeferred?(model, handle, options?): AssistantMessageEventStream;
	cancelDeferred?(model, handle, options?): Promise<void>;
	generateImages?(model, context, options?): Promise<AssistantImages>;
	classify?(model, context, options?): Promise<ClassifierResult>;
}

export interface ModelsStoreEntry { models: AnyModel[]; lastModified?: number; checkedAt?: number; etag?: string; }
export interface ModelsPublication { persist?: ModelsStoreEntry | null; update?: () => void; }
export interface RefreshModelsContext {
	credential?: Credential; stored?: ModelsStoreEntry; publish(publication: ModelsPublication): Promise<boolean>;
	allowNetwork: boolean; force?: boolean; signal: AbortSignal; baseUrl?: string;
}
export interface ModelsRefreshResult { aborted: boolean; errors: Map<string, Error>; }
export interface ModelsRefreshOptions { allowNetwork?: boolean; force?: boolean; signal?: AbortSignal; providers?: string[] }
export interface ModelsStore { read(id): Promise<ModelsStoreEntry|undefined>; write(id, entry): Promise<void>; delete(id): Promise<void> }
export const InMemoryModelsStore: new () => ModelsStore;
export function builtinProviders(): Provider[];
export function getBuiltinModelDataGeneratedAt(): number;
export function radiusProvider(options: {id: string; name: string; gateway: string}): Provider;
export function personalizeOpenRouterCatalog(catalog, ctx): Promise<readonly AnyModel[]>;
```

#### 5.1.2 来自 `ai-auth`

```ts
export interface ApiKeyCredential { type: "api_key"; key?: string; env?: Record<string,string>; }
export interface OAuthCredential { type: "oauth"; refresh: string; access: string; expires: number; [k: string]: unknown }
export type Credential = ApiKeyCredential | OAuthCredential;
export interface CredentialInfo { providerId: string; type: Credential["type"]; }
export interface AuthOperationOptions { signal?: AbortSignal; }
export interface LoginOptions { getDeviceId?: () => string; agentName?: string; }
export type AuthType = "api_key" | "oauth";
export interface AuthCheck { source?: string; type: AuthType }
export interface AuthResult { auth: ModelAuth; env?: ProviderEnv; source?: string }
export interface ModelAuth { apiKey?: string; headers?: ProviderHeaders; baseUrl?: string }
export interface ApiKeyAuth {
	name: string;
	login?(interaction: AuthInteraction): Promise<Credential>;
	check?(input: {credential?; ctx}): Promise<AuthCheck|undefined>;
	resolve(input: {credential?; ctx; env?}): Promise<AuthResult|undefined>;
}
export interface OAuthAuth {
	name: string; isSubscription?: boolean;
	login(callbacks: OAuthLoginCallbacks): Promise<OAuthCredentials>;
	refresh(credential: OAuthCredentials, signal: AbortSignal): Promise<OAuthCredentials>;
	toAuth(credential: OAuthCredentials): Promise<ModelAuth>;
}
export interface CredentialStore {
	read(providerId, options?): Promise<Credential|undefined>;
	list(options?): Promise<readonly CredentialInfo[]>;
	modify(providerId, fn, options?): Promise<Credential|undefined>;
	delete(providerId, options?): Promise<void>;
}
export type AuthPrompt = {signal?: AbortSignal} & ({type:"text"|"secret"|"manual_code"; message: string; placeholder?: string} | {type:"select"; message: string; options: readonly {id;label;description?}[]});
export interface AuthInteraction {
	signal?: AbortSignal;
	prompt(prompt: AuthPrompt): Promise<string>;
	notify(event: {type: "auth_url"|"device_code"|"progress"; ...}): void;
}
export interface AuthContext { env(name: string): Promise<string|undefined>; }
export interface OAuthLoginCallbacks { onAuth; onDeviceCode; onPrompt; onProgress; onManualCodeInput; onSelect; signal }
```

#### 5.1.3 来自 `ca-session`

```ts
export type SessionEntry =
	| { type: "message"; message: AgentMessage; ... }
	| { type: "model_change"; provider: string; modelId: string; ... }
	| { type: "usage"; kind: string; provider: string; model: string; usage: Usage; timestamp: string; note?: string; ... }
	| { type: "compaction"; usage?: Usage; ... }
	| { type: "branch_summary"; usage?: Usage; ... }
	| { type: "custom"; customType: string; data?: unknown; ... }
	| ...;
export interface UsageEntry { kind: string; provider: string; model: string; usage: Usage; timestamp: string; note?: string; }
// SessionManager 依赖面：
appendUsage(kind: string, provider: string, model: string, usage: Usage, note?: string): UsageEntry;
getBranch(): SessionEntry[];
```
`AssistantMessage`（来自 `ai-core`）需含：`role:"assistant"`、`api`、`provider`、`model`、`responseModel?`、`usage`、`timestamp: number`、`stopReason`、`errorMessage?`、`thinkingLevel?`。

#### 5.1.4 来自 `tui-core`

```ts
export const Key: { escape:"escape"; ...; question:"?" }      // 值见 §2.25
export type KeyId = ...;
export interface Keybinding { defaultKeys: KeyId | readonly KeyId[]; description: string }
export type KeybindingDefinitions = Record<string, Keybinding>;
export type KeybindingsConfig = Record<string, KeyId | readonly KeyId[]>;
export const TUI_KEYBINDINGS: KeybindingDefinitions;           // 47 个 id
export class KeybindingsManager {
	constructor(definitions: KeybindingDefinitions, userBindings?: KeybindingsConfig);
	setUserBindings(bindings: KeybindingsConfig): void;
	getResolvedBindings(): KeybindingsConfig;
	getKeys(id: string): readonly KeyId[];
}
```

#### 5.1.5 来自 `ca-cli`

```ts
export const VERSION: string;            // "1.4.0"
export const APP_NAME: string;           // "kiana"
export const APP_TITLE: string;          // "π"
export const CONFIG_DIR_NAME: string;    // ".kiana"
export function getAgentDir(): string;
export function getModelsPath(): string;
export function getAuthPath(): string;
export function getSettingsPath(): string;
export function getDocsPath(): string;
export function getSessionsDir(): string;
export function normalizePath(p: string): string;
export function resolvePath(p: string): string;
export function canonicalizePath(p: string): string;
export function getFileRevision(p: string): string | undefined;   // `${dev}:${ino}:${size}:${mtimeNs}:${ctimeNs}`（bigint stat；文件不存在/无权限 → undefined）
export function stripBom(s: string): string;
export function stripJsonComments(s: string): string;
export interface FetchRetryOptions {
	maxRetries?: number;       // 默认 2（非有限数 → 2；否则 max(0, floor(n))），即"额外尝试次数"
	retryOnStatus?: boolean;   // 默认 true；false 时只重试传输层失败
	timeoutMs?: number;        // 全部尝试共享的总预算（每次尝试前 throwIfAborted）；<=0 视为未设
	attemptTimeoutMs?: number; // 单次尝试超时（每次尝试新建 AbortSignal.timeout）；<=0 视为未设
}
export function fetchWithRetry(input: URL | string, init: RequestInit | undefined = undefined, options: FetchRetryOptions = {}): Promise<Response>;
// 语义：每次尝试把 [调用方 signal, timeoutMs signal, attemptTimeoutMs signal] 用 AbortSignal.any 合并；
// 状态码在 RETRYABLE_STATUS_CODES = {408,425,429,500,502,503,504} 且 attempt < maxRetries 时，先 response.body?.cancel() 再重试（cancel 失败忽略）；
// 调用方取消与 timeoutMs 是终态，attemptTimeoutMs 只终止当前尝试。
// 注意：spec 其它地方只提到 attemptTimeoutMs，Rust 侧应完整实现四个选项（`management-http.test.ts` 的 5 条用例覆盖：瞬态传输失败重试一次、超时预算跨尝试共享、重试尝试超时、重试瞬态 HTTP 响应、不重试调用方取消）。
export function getKianaUserAgent(version: string): string;
export function operationSignal(signal?: AbortSignal): AbortSignal;
export function raceWithAbortSignal<T>(promise: Promise<T>, signal: AbortSignal): Promise<T>;
export interface ShellConfig { shell: string; args: string[]; commandTransport?: "argv" | "stdin"; }
export function getShellConfig(customShellPath?: string): ShellConfig;
// getShellConfig 在 win32 上还会读 ProgramFiles / ProgramFiles(x86) / SystemRoot 等环境变量（Git Bash 探测），
// 自定义路径不存在时抛 `Custom shell path not found: ${customShellPath}`。本单元只用它来执行 `!command`（见 §2.11）。
export function isValidThinkingLevel(v: string): v is ThinkingLevel;
```

#### 5.1.6 来自 `extensions` / `agent-core` / `theme`

```ts
// extensions（`src/core/extensions/types.ts` 与 `runner.ts`，逐字）
export interface ProjectTrustEvent { type: "project_trust"; cwd: string; }
export type ProjectTrustEventDecision = "yes" | "no" | "undecided";
export interface ProjectTrustEventResult {
	trusted: ProjectTrustEventDecision;      // 必填，三值枚举（不是 boolean）
	remember?: boolean;
}
export interface ProjectTrustContext {
	cwd: string;
	mode: ExtensionMode;
	hasUI: boolean;
	ui: Pick<ExtensionUIContext, "select" | "confirm" | "input" | "notify">;
}
export type ProjectTrustHandler =
	(event: ProjectTrustEvent, ctx: ProjectTrustContext) => Promise<ProjectTrustEventResult> | ProjectTrustEventResult;
export function emitProjectTrustEvent(
	extensionsResult: LoadExtensionsResult,
	event: ProjectTrustEvent,
	ctx: ProjectTrustContext,
): Promise<{ result?: ProjectTrustEventResult; errors: ExtensionError[] }>;
export interface LoadExtensionsResult { ... }
// agent-core
export type ThinkingLevel = "off"|"minimal"|"low"|"medium"|"high"|"xhigh"|"max";
export interface Message { role: "user"|"assistant"|"toolResult"|"system"; ... }
export type AgentMessage = Message;
// theme（仅 generate-schemas）
export const ThemeJsonSchema: TSchema;
export const ColorValueSchema: TSchema;
```

### 5.2 本单元**暴露**的接口（其它单元依赖，签名必须一致）

| 导出 | 签名要点 |
|---|---|
| `SettingsManager` | `static create(cwd: string, agentDir = getAgentDir(), options: SettingsManagerCreateOptions = {}): SettingsManager`（**cwd 必填，无 `cwd?` 重载**）、`static inMemory(settings: Partial<Settings> = {}, options = {})`、`static fromStorage(storage: SettingsStorage, options: SettingsManagerCreateOptions = {})`（**没有 cwd 参数**）；**71 个强类型 getter**（`getTheme()`、`getCompactionReserveTokens(model?)`、`getDefaultTools()`、`getHttpIdleTimeoutMs()` 等，逐个命名，**不存在**泛型 `get<K>(key)` 方法）、**57 个 `setX` setter**；`reload()`、`flush()`、`drainErrors()`、`getGlobalSettings()`、`getProjectSettings()`、`getSettings()`、`isProjectTrusted()`、`setProjectTrusted()`、`applyOverrides()`；**`save()` / `saveProjectSettings()` / `persistScopedSettings()` 是 private**，只由 setter 内部调用，Rust 侧不应把它们暴露为公共 API |
| `SettingsManagerCreateOptions` | `{ projectTrusted?: boolean }`（§2.9.2） |
| `SETTINGS_DEFAULTS` | 只读对象（§2.7） |
| `Settings` / `SettingsInput` | TypeBox `Static<>` 派生 |
| `SettingsStorage` | `withLock(scope: SettingsScope, fn: (current: string \| undefined) => string \| undefined): void`（**无泛型、无 `{result, next}` 结构**；回调直接返回要写入的完整文本，返回 `undefined` 表示只读不写；§2.9.2 已逐字给出。`{result, next}` 形状属于 `ai-auth` 的 `AuthStorageBackend`，别混用） |
| `ModelRuntime` | 见 §2.20；`implements Models` |
| `ModelRegistry` | 见 §2.22 |
| `AuthStorage` | `create(path?)`、`inMemory()`、`fromStorage(backend, path?)`、`readStoredCredential(providerId: string, authPath: string = join(getAgentDir(), "auth.json")): Credential \| undefined`（**providerId 在前，authPath 在后且可省略**；§2.13 的 `AuthStorage.read` 是实例方法，签名不同）；`implements CredentialStore` |
| `RuntimeCredentials` | `implements CredentialStore`，叠加 runtime API key |
| `ProjectTrustStore` | `get(cwd)`、`getEntry(cwd)`、`set(cwd, trusted)`、`setMany(map)` |
| `resolveProjectTrusted(...)` | 见 §2.17 |
| `CacheWarmer` / `formatCacheWarmingStatus` / `formatCacheWarmingUsage` | 见 §2.28 |
| `computeCacheWaste` / `collectCacheMisses` / `detectCacheMiss` / `CACHE_TTL_MS` | 见 §2.29 |
| `createUsageTotals` / `addUsageToTotals` / `combineUsage` / `getUsageCostBreakdown` | 见 §2.30 |
| `resolveConfigValue` / `resolveConfigValueOrThrow` / `resolveHeaders` / `resolveHeadersOrThrow` / `clearConfigValueCache` / `getConfigValueEnvVarNames` / `isCommandConfigValue` / `isConfigValueConfigured` | 见 §2.11 |
| `configureHttpDispatcher` / `parseHttpIdleTimeoutMs` / `formatHttpIdleTimeoutMs` / `applyHttpProxySettings` / `HTTP_IDLE_TIMEOUT_CHOICES` / `DEFAULT_HTTP_IDLE_TIMEOUT_MS` | 见 §2.12 |
| `KEYBINDINGS` / `APP_KEYBINDINGS` / `KeybindingsManager` / `migrateKeybindingsConfig` / `useWindowsKeybindings` / `KeybindingsSchema` / `KeybindingValueSchema` / `KeyIdSchema` | 见 §2.25–2.27 |
| `getClientIdentity` / `isInstallTelemetryEnabled` / `areExperimentalFeaturesEnabled` | 见 §2.4–2.5 |
| `RADIUS_PROVIDER_ID` / `ENV_RADIUS_GATEWAY` / `RADIUS_MCP_URL` / `getRadiusGatewayUrl` | 见 §2.3 |
| `DEFAULT_THINKING_LEVEL` / `THINKING_LEVEL_OPTIONS` | 见 §2.1 |
| `defaultModelPerProvider` / `resolveModelReference` / `findExactModelReferenceMatch` / `resolveModelScope` / `resolveModelScopeWithDiagnostics` / `resolveModelScopeFromModels` / `resolveCliModel` / `findInitialModel` / `restoreModelFromSession` | 见 §2.21 |
| `VIRTUAL_MODEL_API` / `VIRTUAL_MODEL_STATE_ENTRY` / `isVirtualModel` / `findLatestResponse` / `getBranchSelection` / `getVirtualModelState` / `createVirtualModel` / `withVirtualModels` | 见 §2.23 |
| `withRemoteCatalog` / `withPersonalizedCatalog` / `REMOTE_CATALOG_REFRESH_INTERVAL_MS` / `REMOTE_CATALOG_MODEL_TYPES` | 见 §2.24 |
| `composeModelProvider` / `validateExtensionProvider` / `resolveConfiguredModelHeaders` / `resolveCompatibilityRequestConfig` / `configuredRequestAuthStatus` / `clearApiKeyCache` / 类型 `ProviderConfigInput`、`AuthStatus`、`ExtensionOAuthConfig` | 见 §2.19 |
| `ModelConfig` / `ModelsConfigSchema` / `ModelsJsonSchemas` | 见 §2.18 |
| `collectSettingsDiagnostics` / `deduplicateDiagnostics` | 见 §2.10 |
| `getAuthGuidance` 类文案 | 见 §2.6 |

---

## 6. 依赖与 Rust crate 映射

### 6.1 npm 依赖 → 推荐 crate

| npm 包 | 版本 | 用途（本单元） | 建议 crate | 必须桥接的行为差异 |
|---|---|---|---|---|
| `node:fs` / `node:path` / `node:os` | Node 22 | 设置/凭证/信任/缓存读写、路径 | `std::fs`、`std::path`、`dirs` | 路径大小写与分隔符；`normalizePath` 在 Windows 上的扩展名归一；`~` 展开由 `getAgentDir` 负责 |
| `proper-lockfile` | 4.1.2 | auth/models-store/trust/settings 的锁 | `fs4`（`flock`）或 `fslock`，或自实现 `.lock` 目录 | **必须复刻语义**：锁是**目录**（`mkdir` 原子），`realpath:false`，`lockfilePath` 可自定义（trust 用 `${path}.lock`，锁的是目录）；`ELOCKED` 才重试，其它错误立即抛出；`onCompromised` 回调 → `Auth storage lock was compromised`；陈旧锁 30 s（异步）。`fs4` 的 advisory lock 不是目录锁，若要让外部 `proper-lockfile` 互操作必须做目录锁 |
| `undici` | 8.10.2 | fetch、`EnvHttpProxyAgent`、`install()`、dispatcher 超时 | `reqwest`（默认 rustls/hyper） | `undici.install()` 替换 `globalThis.fetch` 无对应物；`EnvHttpProxyAgent(allowH2:false, proxyTunnel:true)` → `reqwest::Proxy::all`（HTTPS 走 CONNECT 隧道，等价 `proxyTunnel`）；`bodyTimeout/headersTimeout = idle` → `ClientBuilder::read_timeout`（不覆盖连接超时）；`autoSelectFamilyAttemptTimeout = 2000` → Happy Eyeballs `happy_eyeballs_timeout`；**不要**用 `timeout()`（那是整请求超时） |
| `typebox` | 1.3.27 | `SettingsSchema` / `ModelsConfigSchema` / `KeybindingsSchema` 及 `Compile` 校验 | `serde` + `serde_json` + 手写校验，或 `schemars`（生成）+ `jsonschema`（校验） | TypeBox 的 `Compile().Check()` 是运行时校验；`Static<>` 类型派生需手写 Rust 结构体；`Type.Unsafe<T>(Type.String({pattern}))` 仅类型层，运行时就是字符串 + ECMA 正则；数字约束 `exclusiveMinimum: 0` 需显式检查；**`SettingsSchema` 的顶层是 `Type.Object({...}, { additionalProperties: true })`**（未知键允许且必须原样保留）；`ModelsConfigSchema` 的 provider/model 对象也是宽松（未禁用额外键）；`KeybindingsSchema` 的 `additionalProperties` 是 `KeybindingValueSchema`（值必须仍满足 KeyId 语法） |
| `minimatch` | 10.2.6 | scope 模式 glob | `globset` 或 `glob` | minimatch 支持 `**`、`{a,b}`、`[abc]`、`!` 否定（本单元未用否定）；`nocase:true` → `globset::case_insensitive(true)`；minimatch 对 `[` 未闭合等边界行为与 globset 不同，需测试对齐 |
| `chalk` | 6.0.0 | `dim` / `yellow` 日志 | `owo-colors` / `anstyle` | 颜色使能判定：`NO_COLOR`、`FORCE_COLOR`、TTY、`TERM=dumb`、`CI`；chalk 6 默认 level 0–3；`yellow`=`\x1b[33m...\x1b[39m`、`dim`=`\x1b[2m...\x1b[22m` |
| `@earendil-works/kiana-tui` | workspace | `Key`、`TUI_KEYBINDINGS`、`KeybindingsManager` | 内部 crate `kiana-tui` | 见 `tui-core` 单元 |
| `@earendil-works/kiana-ai` | workspace | 全部模型类型与 `createModels` | 内部 crate `kiana-ai` | 见 `ai-core` 单元 |
| `structuredClone`（JS 内建） | — | models-store / auth 读结果、ModelConfig provider 冻结 | `serde_json::Value::clone` 或手写 `Clone` | JS 的 `structuredClone` 支持 Map/Set/Date/循环引用；本单元只用 JSON 结构，`Clone` 足够；**注意** `deepFreeze(structuredClone(provider))` 的冻结在 Rust 里用不可变绑定表达 |
| `crypto.randomUUID` | — | `getOrCreateDeviceId`、analytics `trackingId` | `uuid::Uuid::new_v4()` | 格式同为 8-4-4-4-12 小写；`randomUUID` 使用 CSPRNG（`getrandom`） |
| `AbortController` / `AbortSignal` | — | 取消、超时、`AbortSignal.any/timeout` | `tokio_util::sync::CancellationToken` + `tokio::time::timeout` | `AbortSignal.any([a,b])` → `CancellationToken` 的 child 或 `tokio::select!`；`AbortSignal.timeout(ms)` → `tokio::time::sleep` 竞速；`signal.throwIfAborted()` → `ensure_not_cancelled()?` 返回 `AbortError`；**取消后错误类型**必须是 DOMException 语义（`name:"AbortError"`）或与上层约定一致 |
| `EventEmitter` | — | undici dispatcher 错误监听 | `tokio` / 无对应 | Rust 中 body 错误经 `Stream` 返回，无需 `error` 监听 |
| `JSON.parse` / `JSON.stringify(v, null, 2)` | — | 全部磁盘格式 | `serde_json` | **键顺序**：JS 对象按插入顺序；Rust 用 `serde_json` 的 `Map`（默认 `BTreeMap`，需开启 `preserve_order` feature 用 `IndexMap`）。凡写盘需与 JS 字节一致的（settings/auth/models-store 保留读取时的键顺序；trust 显式排序）都必须 `preserve_order` |
| `Number` 解析与格式化 | — | `parseHttpIdleTimeoutMs`、`String(value)`、`toFixed` | `f64` + 手写 | JS `Number("0x10")===16`、`Number("1e5")===100000`、`Number(" 12 ")===12`、`Number("")===0`（但代码先判空串）；`Number.isFinite`；`Math.floor`；`(0.05).toFixed(3)==="0.050"`；`(1e21).toFixed(3)` 走指数；`String(1.0)==="1"`（Rust 需 `f64::to_string` 后裁剪 `.0`）；`toFixed` 的银行家舍入差异（JS 用最近舍入，部分值有偏差） |
| `Date.parse` / `Date.now()` | — | Last-Modified、`checkedAt`、时间戳 | `chrono` / `humantime` / `httpdate` | `Date.parse` 接受多种格式并返回毫秒；HTTP 头一般是 RFC 7231 格式 → `httpdate::parse_http_date` 再转毫秒；非法 → `NaN`（Rust 用 `Option<i64>`） |
| `localeCompare` | — | `bestFuzzyMatch` 排序、歧义 provider 排序 | `str::cmp`（字节序）或 `icu_collation` | `"a".localeCompare("b")` 依赖 ICU locale（默认环境），对 ASCII 通常等价于码点比较，但对 `-`、数字、大小写有差异。**必须确认测试只涉及 ASCII id**；若要严格一致需 `icu_collation` 的 root locale |
| `String.prototype.toLowerCase` | — | provider/model 大小写不敏感匹配 | `str::to_lowercase` | JS 用 Unicode Default Case Conversion（含特殊映射如 `İ`）；Rust `to_lowercase` 也是 Unicode 完整映射，但 JS 不做 locale 相关；边界上足够 |
| `String.prototype.trim` | — | 大量输入清理 | `str::trim` | JS `trim` 去除 WhiteSpace + LineTerminator（含 `﻿` BOM），Rust `trim` 用 `char::is_whitespace`（**不含** U+FEFF）。需自定义 `js_trim` 加入 `\u{FEFF}` 等 |
| 正则 | — | KeyId 校验、`stripJsonComments`、`splitThinkingSuffix` 等 | `regex`（无 lookaround）/ `fancy-regex` | KeyId 模式含**负向前瞻** `(?!.*(?:ctrl\+.*ctrl\+...))` → `regex` 不支持，需 `fancy-regex` 或手写校验（推荐手写：按 `+` 分割，检查修饰键不重复且基础键在集合内、修饰键 ≤4）；`stripJsonComments` 用前后瞻/替换 → 手写扫描器更安全 |
| JSONC | — | models.json | 自实现（`stripJsonComments` 移植） | 只支持 `//` 与尾逗号；**不要**引入完整 JSON5 解析器（会接受超出 JS 实现的输入） |

### 6.2 关键 JS 语义差异清单

1. **字符串索引是 UTF-16 码元**：`substring`/`lastIndexOf`/`length`。本单元的用法基本安全（`:`、`/`、`-` 均为 ASCII），但 `text.substring(0, colonIndex)` 在代理对边界上不会切开（因为分隔符是 ASCII），可安全用 Rust 的 `char_indices`/`find` 实现。仍需注意 `String.prototype.slice` 在负数索引上的行为（本单元未用）。
2. **正则方言**：ECMA 的 `\d` 只匹配 ASCII 数字（Rust `regex` 的 `\d` 默认匹配 Unicode 数字！需用 `(?-u)` 或 `[0-9]`）。`stripJsonComments` 与 `isAlias` 的 `-\d{8}$` 都依赖 ASCII 数字。
3. **JSON 键顺序**：见上，必须 `preserve_order`。
4. **数字格式化**：`toFixed`、`String(number)`、`JSON.stringify` 的数字输出（`1e21`、`-0`、整数值无 `.0`）。
5. **`Date` 格式化**：本单元不格式化日期，只 `${usage.timestamp}`（ISO 字符串，由 session 层给）。
6. **`localeCompare`**：见上。
7. **`proper-lockfile`**：目录锁 + 陈旧检测 + `onCompromised`。
8. **`structuredClone`**：深拷贝（本单元数据均为 JSON 可表达）。
9. **`AbortSignal`**：any/timeout/throwIfAborted，错误为 DOMException `AbortError`。
10. **`randomUUID`**：v4 CSPRNG。
11. **对象属性枚举顺序**：JS 对字符串键（非整数索引）保持插入顺序；**整数索引键会被排到前面并升序**。`defaultModelPerProvider`、`KEYBINDINGS`、`settings` 对象的键都不是整数，但 `models.json` 若用数字作 provider id 会触发该规则（`Object.entries` 顺序变化）。Rust 实现需在解析 JSON 对象时模拟：若键是全数字字符串，先按数值升序输出，再输出其余按插入顺序。`JSON.stringify` 同样遵守该规则。
12. **`Object.hasOwn`** vs `in`：`migrateKeybindingsConfig` 的 `isLegacyKeybindingName` 用 `in`（含原型链）；Rust 用显式表即可（无原型链，等价行为）。
13. **`Array.prototype.sort` 稳定性**：V8 自 ES2019 起稳定排序；Rust `sort_by` 稳定，`sort_unstable_by` 不稳定——一律用稳定版本。
14. **`console.warn`/`console.error` 写 stderr，`console.log` 写 stdout**；Rust 用 `eprintln!` / `println!`，注意 `chalk` 的颜色判定通常依据 stdout 是否 TTY，而 `console.warn` 的颜色判定 chalk 使用 `process.stdout` 的 isTTY（chalk 6 检测 stdout/stderr 分别？——需按 chalk 6 实现：chalk 实例默认基于 `process.stdout`）。为字节一致，建议统一用「stdout 为 TTY 时启用颜色」。

---

## 7. 测试清单

测试位于 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/test/`（vitest 4.1.12，Node ESM，直接跑 `.ts`）。用例数为 `it(`/`test(` 计数（`test.each` 计为 1）。Rust 侧需逐条复刻为 `#[test]`/`#[tokio::test]`；下表的「断言要点」是必须成立的行为。

### 7.1 直接覆盖本单元的测试文件

| 测试文件 | 用例数 | 主要断言 |
|---|---|---|
| `settings-manager.test.ts` | 51 | 外部字段保留（`enabledModels`/自定义键/`packages`/`extensions`/`shellCommandPrefix`）；`deviceId` 生成一次并跨进程复用（全局 only）；`packages` 迁移（保留 local-only 扩展、过滤对象）；`reload` 重读全局、文件非法时保留旧值并报告路径；`theme` 含 `/` 视为自动主题单独存储；`drainErrors` 收集并清空；项目信任（未受信跳过、信任后 reload、写抛错、默认信任只读全局、非法值回落 `ask`）；`.kiana` 目录仅在写时创建；终端能力覆盖映射（auto 省略）；`retry` 默认与覆盖（cap）；`httpIdleTimeoutMs` 默认 300000、合并 global+project、拒绝非法值；`cacheWarming` 默认 streaming 且忽略项目设置、全局持久化；`externalEditor` 优先级与平台默认；TUI 模式（默认 fullscreen、持久化 regular、非法回落 fullscreen、旧 `uiMode` 不识别、fullscreen 设置校验、滚轮行数）；`outputPad` 默认 1、二进制、非法回落；`markdown.mermaid` 默认 streaming、渲染模式、非法回落；`defaultTools`（全局默认、项目替换、空列表保留、`+/-` 应用） |
| `settings-manager-bug.test.ts` | 4 | 改无关字段时保留外部对 `packages`/`extensions` 数组的修改；项目字段同理；内存中的项目改动覆盖同字段的外部改动 |
| `settings-manager-compaction.test.ts` | 12 | 无 compaction 时用 `{16384,20000}`；每字段独立解析且单字段 getter 一致；缺失字段回落内置默认；`provider/model` 精确匹配（含含 `/` 的 id）；项目 `modelOverrides` 按字段合并后再回落；保存开关时保留 global 与覆盖；普通设置与 modelOverrides 接受 0 |
| `settings-diagnostics.test.ts` | 3 | 文件后端带路径 `Invalid settings file ${path}:`；无路径后端回落 `Invalid ${scope} settings: ...`；按 type+message 去重 |
| `auth-storage.test.ts` | 28 | 读取并解析存储的 api-key；命令型 key；OAuth 原样返回；credential-scoped env 优先且可检查；并发读者与不同存储实例**合并**文件重读；合并读在仍有读者时保持存活；`modify` 保留无关外部编辑；`modify` 返回 undefined 不改；并发修改串行化；`delete` 只删一个；内存后端等价；锁获取失败不写、重试可恢复；短暂争用重试；Windows EPERM 异步/同步重试与有界放弃；其它平台不重试 EPERM；锁被破坏报错；预中止操作不建文件不执行；等锁时中止不再补执行；并发获取+取消时先释放锁再中止；锁持有到已取消但已开始的回调结束且不提交；读取在等锁时可被取消；内存修改跨 provider 串行；排队的内存修改被取消后不执行；取消活跃 refresh 保留原凭证；refresh 失败转换并可重试；**不覆盖 malformed auth 文件** |
| `models-store.test.ts` | 4 | 持久化 provider 目录不替换其它 provider；并发读者与不同实例合并重读；仍有读者时保持存活；取消等锁的写入后不写 |
| `model-registry.test.ts` | 87 | 见 §7.2 分类 |
| `model-resolver.test.ts` | 29（含 `test.each(referenceCases)` 内部 ~40 例；含 `it.each` 的参数化组） | 见 §7.3 |
| `model-runtime-auth-options.test.ts` | 11 | 接受 kiana-ai `CredentialStore`；按 provider 的可用性读取并记录刷新失败；投影 provider 自己的方法/名称/状态；每个方法选项附带 active auth status；区分 subscription OAuth 与普通 OAuth；为扩展 api-key provider 构造 api-key 方法；用请求级 env 覆盖解析配置认证；显式 `Authorization` 头**大小写不敏感**覆盖 `authHeader`；`transformHeaders` 只对完全组装后的头调用一次且不外传；扩展 OAuth refresh **不**随请求取消；纯 OAuth 扩展 provider 不伪造 api-key 方法 |
| `model-runtime-credential-sync.test.ts` | 10 | login/logout resolve 前即发布本地一致的可用性；同 provider 操作按本地同步排序；不同 provider 并发；本地同步不等无关 provider 可用性；provider 级可用性期间取消会上报；凭据链内不跑网络刷新；provider 级刷新不使无关 provider 失效；已提交的修改在报告取消前先 settle；取消打断 commit 后同步 → typed error；本地同步失败时上报已提交凭证 |
| `model-runtime-images.test.ts` | 7 | OpenRouter 图像模型与 chat 分开列出；扩展模型列表替换未声明的各操作模型；注册扩展图像/分类器实现；通过原生 provider 生成图像（运行时解析认证）；chat 入口对图像模型在 provider 派发前即拒绝；图像生成保留内置 provider + models.json 覆盖；chat-only 合成 provider 不加图像/分类 |
| `model-runtime-classifiers.test.ts` | 3 | Jev 单独列出并可分类（运行时认证）；models.json 代理 OpenRouter 时保留 image 形态；把 provider compat 应用到 classifier |
| `model-runtime-cloudflare-compat.test.ts` | 2 | 通过 ModelRuntime 流式物化 Cloudflare 端点（含扩展式认证解析） |
| `model-runtime-modify-models-compat.test.ts` | 6 | 注册原生 kiana-ai provider 及其 auth；原生 provider 有已存凭证时注册即视为 configured；overlay 保留原生 deferred 方法；models.json 覆盖层在原生 provider 之上；`refreshModels` 结果发布但**不**强制 ModelsStore 持久化；旧式 OAuth `modifyModels` 在异步凭证初始化后应用 |
| `remote-catalog-provider.test.ts` | 10 | 解析键控目录、发送版本头、遵守刷新 TTL、支持强制刷新；叠加 image/classifier 并丢弃未知 type；generated 与 remote 取更新者；用 etag 重新校验且 304 保留 overlay；overlay 不可用时丢弃陈旧 etag；瞬时失败保留 etag 与 overlay；更新的请求绕过卡住的旧请求且不陈旧发布；kiana.dev 未实现路由视为不可用；persona 化：保存失败时不保存、恢复已存列表原样、仅空时回落内置 |
| `model-catalog-refresh.test.ts` | 3 | 并发调用共享一次 runtime refresh；一个调用方停止等待后仍存活；放弃的刷新被 abort 且后续可再启动 |
| `model-catalog-protocol.test.ts` | 1 | 与当前客户端协商目录并到达 OpenRouter API：断言请求为 `/api/models/providers/openrouter?types=chat%2Cimage%2Cclassifier` 与 `...&kiana-version=${VERSION}` |
| `virtual-models.test.ts` | 17 | 见 §7.4 |
| `keybindings.test.ts` | 5 | 原生 Windows / WSL（不依赖 Windows Terminal 检测）用 Windows 键位；仅 `WT_SESSION` 不算；其它平台保持非 Windows 默认；检测结果一致地应用 |
| `keybindings-migration.test.ts` | 3 | 旧名改写为命名空间 id；新旧同名时保留新名值；文件重写前在内存中加载旧名 |
| `cache-warmer.test.ts` | 9 | 由 retention 与 provider 行为推导资格与时序；重放有利请求并在重复刷新间保留 options；超过安全 deadline 不刷新；扩展决策后**再次**检查 deadline；经济性决策与扩展覆盖；不支持的请求/上下文变化/模式变化即停；替换请求会 abort 且失败刷新不记录；状态与 usage 条目的格式化；`ExtensionRunner.emitCacheWarmingDecision` 使用最后一个扩展覆盖 |
| `cache-stats.test.ts` | 11 | 跨轮累计 miss tokens/cost；健康会话不计；压缩后一轮跳过；模型切换计入 miss；不报告缓存活动的 provider 不计；`collectCacheMisses` 按引用映射；`detectCacheMiss` 检测刚完成消息（含 idle 时间）、标记模型切换、只用 `cache_warm` usage 作为刷新、健康轮与首轮返回 undefined |
| `trust-manager.test.ts` | 2 | 存储决策并从父目录继承；检测需要信任的项目资源 |
| `usage-totals.test.ts` | 1 | `combineUsage` 仅在两侧都 provider-reported 时标记 `totalFromProvider` |
| `resolve-config-value.test.ts` | 8 | 字面量/环境模板/转义；credential env 优先于 process.env；执行 shell 命令并 trim；失败命令（`!exit 1`、不存在的命令、空输出）报错；成功与失败命令都缓存直到 `clearConfigValueCache`；环境值不缓存；未缓存解析每次都执行；Windows shell 需要时用 stdin |
| `http-dispatcher.test.ts` | 5 | `httpProxy` 写入 `HTTP_PROXY`/`HTTPS_PROXY`；不覆盖已有值；忽略空值；代理 HTTP 源走隧道；HTTPS 连接尝试有 2 秒预算且不改 Node 默认 |
| `config-schemas.test.ts` | 13 | 4 个生成产物逐字节匹配；theme schema 保留编辑器指引；models/settings 指引迁移后保留；`it.each` 各 schema 断言；拒绝非正 token 限制；校验键位语法；拒绝运行时访问器拒绝的设置值；准确描述旧设置迁移；接受自定义 compat 并拒绝已知字段的畸形值；运行时校验 models.json 的 `$schema`；接受 settings.json 的 `$schema` 并保留未知设置；接受 keybindings.json 的 `$schema` 且不当作动作；接受 theme 的 `$schema` 并拒绝畸形 theme |
| `config-value-migration.test.ts` | 3（含 `it.each`） | auth.json 中大写 API key 值不变；`it.each` 各语法迁移；models.json 中大写 apiKey 与 header 值不变 |
| `default-tools-setting.test.ts` | 20 | 配置列表作为初始内置选择；可用 `powershell` 替代 `bash`；`+name` 激活未激活的扩展工具；保留扩展与 SDK 自定义工具；显式工具选项优先级；`+/-` 应用到默认选择；拒绝非法修饰符；reload 时只激活新增工具、保持 `-name` 移除、保留显式选项；resume 恢复记录的 loadout（含扩展工具、压缩前移除、树导航、allowlist 外保持不激活、不恢复排除项、显式选项优先、`+/-` 应用到记录 loadout、无 loadout 用默认、service 路径） |
| `radius.test.ts` | 6 | 无网络恢复旧凭据目录；为已配置 Radius auth 抓取并存储目录；默认不网络刷新；未配置认证时不抓取也不可用；models.json 自定义 gateway；自定义 gateway 要求 baseUrl |
| `experimental.test.ts` | 5 | `KIANA_EXPERIMENTAL` 未设/空/`0`/其它 → false；`1` → true |
| `kiana-user-agent.test.ts` | 1 | `getKianaUserAgent` 格式符合 kiana.dev 期望 |
| `sdk-client-identity.test.ts` | 5 | 详细 User-Agent + CLI 归属；关闭遥测时无归属；`KIANA_TELEMETRY=0` 同理；为自定义 OpenCode provider 加 session 头；配置的 OpenCode 头覆盖 session 头 |
| `auth-check.test.ts` | 10 | 判断已配置 provider 为 ready；从 `--model` 解析 provider；`--no-refresh` 时不刷新 OAuth；默认刷新 OAuth；未知 provider 不 ready；未解析的存储 env 引用不算已配置；畸形 auth 状态报告 invalid；**不创建** auth 文件或其父目录；支持 JSON 输出、credential 输出与 `--no-refresh`；auth-check runtime 不创建目录存储 |
| `references.test.ts` | 1 | `renderReferences()` 的键序列必须恰为 `["docs/models-configuration.md"]`，且每个产物与仓库中已提交文件**逐字节相同**（失败提示：`<path> is stale; run npm run generate:references`） |

> 上表之外，另有 19 个测试文件直接 import 本单元模块并以本单元行为为主要断言对象（`test/runtime-credentials.test.ts` 5、`test/suite/virtual-models.test.ts` 13、`suite/regressions/{2753,2835,2860,3217,3592,3616,5433,5596,5661,6949,6999,7027,7153,7187,7209,7572,8337}`），以及根目录 `scripts/model-catalog-protocol.test.ts`（7，node:test）。完整清单、逐文件用例数与断言要点见 **A.2 测试清单缺口**。（`test/agent-session-*`、`test/interactive-*`、`test/mcp-*`、`test/session-*` 等约 50 个文件只是通过 harness 间接用到 `SettingsManager`/`AuthStorage`，主要覆盖其他单元，未列入。）

### 7.2 `model-registry.test.ts`（87）分类

- `baseUrl override (no custom models)`（7）：保留全部内置模型、改写全部 URL、请求时解析 headers、仅 headers 覆盖、未配置兼容认证包含静态 model headers、只影响目标 provider、可与 models 合并、`refresh()` 拾取 baseUrl 变更。
- `custom models merge behavior`（15）：继承 api/baseUrl、非内置 provider 仍要求 baseUrl、上报每个 provider 合成错误、同名内置 provider 合并、同 id 自定义模型替换内置、不影响其它内置 provider、provider 级 baseUrl 作用于内置与自定义、provider 级 compat 作用于自定义/内置、model 级 compat 覆盖 provider 级、`thinkingLevelMap` 与 compat（`supportsStrictMode`、`cacheControlFormat`、chat template thinking、Anthropic eager tool input streaming、long cache retention）、model 级 baseUrl 覆盖、`modelOverrides` 与 models 共存、`refresh()` 重载、移除自定义模型后保留内置。
- `modelOverrides`（15）：单模型覆盖、Anthropic 覆盖替换 allowed fallback 元数据、空 allowed fallback 禁用服务端回退、采样参数、prompt cache 生命周期、深合并 image resize 限制、`compat.openRouterRouting`、`supportsFinishReason` 在 provider/model 两级、深合并 compat、同 provider 多个覆盖、与 baseUrl 覆盖组合、不存在 id 忽略、cost 部分覆盖、请求时加 headers、`refresh()` 拾取、移除覆盖恢复内置值。
- `dynamic provider lifecycle`（6）与 `dynamic provider override persistence`（4）：见 §7.1 表（display name 四种来源、覆盖作用于动态 provider、存储 api key 的 env 传播、大小写 apiKey/headers 为字面量、失败的 register 不持久化/不移除已有模型、unregister 去掉运行时 OAuth overlay、streamSimple overlay 不改全局 compat 注册表、四类持久化场景）。

### 7.3 `model-resolver.test.ts` 的 `referenceCases`（必须逐条复刻）

`test.each(referenceCases)` 以 `$name` 为用例名，覆盖 `resolveModelReference` 的：精确 id、大小写不敏感、`provider/id`、provider 含 `/` 的最长前缀、`:thinking` 的 none/strict/lenient 三模式、无效后缀保留为 id、模糊匹配（子串 id/name）、别名优先于日期版本（`-latest` 或非 `-\d{8}$`）、`localeCompare` 降序取最新、歧义（多 provider 同 id）与 `usable` 打破平局、无凭据时的唯一匹配仍可解析、`provider/rest` 作为提示时的四种裁决。
另有 `resolveModelScopeWithDiagnostics`（3）、`resolveModelScope` 保留 CLI 警告输出（1）、同 id 多 provider 全加并警告（1）、方括号 id 先精确匹配（2）、`resolveCliModel`（3）、重复裸 id 在已知 provider 前缀后（3，含 `--api-key` 不让存储凭据选 provider）、自定义模型 `:thinking` 回退（6）、默认模型选择（8：`openai`/`zai`/`minimax`/`cerebras`/`ant-ling`/`ai-gateway`/`xai`/`qwen-token-plan-individual` 的默认值需存在于生成目录、`findInitialModel` 选中 ai-gateway 默认、忽略未认证的已存默认）、持久化默认模型 scoping（3）。

### 7.4 `virtual-models.test.ts`（17）关键断言

- `getBranchSelection`：只查最后一次 `model_change`（`lookups` 恰为一个或零个）；无后续响应的最后 `model_change` 生效；无 `model_change` 时用最新**物理**响应（失败的虚拟模型消息被跳过）。
- 虚拟模型列出与路由：`contextWindow: 0`、`maxTokens: 0`、`input: ["text","image"]`、`getSupportedThinkingLevels` = 声明的等级；路由器请求的等级被 clamp（不推理模型 → `off`）；`previous` 报告最新成功响应的模型与等级；`retry` 的 `failed` 与最新成功响应分开报告（路由失败时 `failed` 为 undefined）。
- 同一 provider 下多个虚拟模型（在有物理模型的 provider 上也如此），顺序为物理在前、虚拟按注册顺序；纯虚拟 provider 无需凭据即可用；`faux/large` 冲突报错；`unregisterVirtualModel` 精确移除。
- 路由到虚拟/未知模型 → `which is not a physical model`。
- `completeSimple` 直连：`reason` 为 `direct`、maxTokens 被裁到路由模型上限（20000 → 5000）、响应 provider/model 为物理模型。
- 跨 provider 路由时**不**转发调用方的 apiKey/headers；同 provider 的虚拟模型保留调用方凭据。
- 未路由的 `complete` 返回 `stopReason: "error"` 且 message 含 `must be routed before streaming`。
- `createAgentSession`：恢复虚拟选择而非物理模型；紧邻会话打开前注册的虚拟模型也可恢复（`modelFallbackMessage` undefined）；未注册时回落物理模型；转写以路由失败结束时回落到最后的物理响应；树导航后仍恢复（`model_change` 留在另一分支）；请求被重定向（`prepareRequest` 换 model）时不每轮记录物理选择；恢复时显式 `model` 覆盖在下一轮 prompt 才写入。

### 7.5 相邻单元但**依赖**本单元行为的测试（本单元须使其通过）

`settings-selector.test.ts`（5）、`trust-selector.test.ts`（4）、`auth-check.test.ts`（10）、`agent-session-dynamic-provider.test.ts`（5）、`model-selector.test.ts`（4）、`scoped-models-selector.test.ts`（5）、`model-search.test.ts`（5）、`management-http.test.ts`（5：瞬态传输失败重试一次、超时预算跨尝试共享、重试尝试超时、重试瞬态 HTTP 响应、不重试调用方取消）、`config.test.ts`（17，多为安装/自更新，与本单元弱相关）。

---

## 8. 建议的 Rust 模块布局与最难复现的部分

### 8.1 Crate 与模块布局

本单元位于 **`kiana-config`** crate（`ARCHITECTURE.md` §3.3 已把 `core/{defaults,experimental,radius,telemetry}.ts` 与全部 settings/auth/models 从 `kiana-core` 移出）。本 spec §8.1 下面这棵树里的路径 `kiana/crates/kiana-coding-agent/src/config/` 是**旧版建议**；骨架按 `kiana/crates/kiana-config/src/` 落地（`settings/`、`auth/`、`models/`、`trust/`、`keybindings/`、`cache/`、`resolve_config_value.rs`、`defaults.rs` 等；`http_dispatcher.rs` 在 `kiana-core`）。`ca-cli`/`ca-session`/`ca-resources`/`ca-tools-*` 已各自独立成 crate，**不**与 config 共享代码目录；本单元只通过 `kiana-core`/`kiana-config` 的公开面被它们使用。另外 `core/{cache-stats,usage-totals,virtual-models}.ts` 实现在 `kiana-session-core`（§3.7），本 crate 只引用。

```
kiana/crates/kiana-config/
  Cargo.toml
  src/
    mod.rs                      # 统一 re-export（对齐 core/*.ts 的导出面）
    defaults.rs                 # 2.1  DEFAULT_THINKING_LEVEL / THINKING_LEVEL_OPTIONS
    experimental.rs             # 2.2  are_experimental_features_enabled
    radius.rs                   # 2.3  RADIUS_PROVIDER_ID / ENV_RADIUS_GATEWAY / RADIUS_MCP_URL / get_radius_gateway_url
    telemetry.rs                # 2.4  is_install_telemetry_enabled
    client_identity.rs          # 2.5  get_client_identity
    auth_guidance.rs            # 2.6  四段登录引导文案（含 docs 路径拼接）
    settings/
      mod.rs                    # SettingsManager 门面 + getter/setter 表
      schema.rs                 # 2.8  SettingsSchema 对应的 Rust 结构体 + 校验
      defaults.rs               # 2.7  SETTINGS_DEFAULTS（serde 反序列化的基底）
      storage.rs                # FileSettingsStorage / InMemorySettingsStorage / SettingsStorage trait
      migrations.rs             # 2.9.7 四个迁移
      diagnostics.rs            # 2.10 collect/deduplicate
      tools.rs                  # getToolListError / applyToolModifiers / mergeDefaultTools / resolveDefaultTools
      compaction.rs             # compaction token 解析与校验
    resolve_config_value.rs     # 2.11 $ENV / !command / 模板状态机 / 命令缓存（骨架名是 resolve_config_value.rs）
    # http_dispatcher.rs（2.12）**不在这里**：ARCHITECTURE.md §3.3 决定 `http-dispatcher.ts` 留在
    # `kiana-core`（`crates/kiana-core/src/http_dispatcher.rs`），`kiana-config` 只调用它的 Client 构建。
    auth/
      storage.rs                # 2.13 AuthStorage / FileAuthStorageBackend / ReadOnlyAuthStorage / InMemory
      runtime_credentials.rs    # 2.14 RuntimeCredentials
      models_store.rs           # 2.15 FileModelsStore / InMemoryCodingAgentModelsStore
      lock.rs                   # proper-lockfile 等价物（目录锁 + 重试 + compromised 回调）
    trust/
      manager.rs                # 2.16 ProjectTrustStore / trust.json / TRUST_REQUIRING_*
      project_trust.rs          # 2.17 resolve_project_trusted / prompt / selector helpers
    models/
      mod.rs                    # re-export
      config_file.rs            # 2.18 ModelConfig / ModelsConfigSchema / strip_json_comments / format_validation_path
      composer.rs               # 2.19 provider 四层合成 + 认证组合 + 请求头解析
      runtime.rs                # 2.20 ModelRuntime（implements Models）
      registry.rs               # 2.22 ModelRegistry 门面
      resolver.rs               # 2.21 引用解析 / scope / CLI / initial / restore
      virtual_models.rs         # 2.23 虚拟模型（骨架名；类型来自 kiana-session-core::virtual_models）
      remote_catalog.rs         # 2.24 withRemoteCatalog / withPersonalizedCatalog
      catalog_http.rs           # 2.24.2 的 requestCatalog/readCatalog/parseCatalog（基于 reqwest）
    keybindings/
      mod.rs                    # 2.27 KEYBINDINGS / migrations / KeybindingsManager
      key_id.rs                 # 2.25 KeyIdSchema 的正则/手写校验
      schema.rs                 # 2.26 KeybindingsSchema 构建
    cache/
      warmer.rs                 # 2.28 CacheWarmer + 三个格式化函数
      # stats.rs（2.29）/ usage_totals.rs（2.30）**不在本 crate**：compute/collect/detect 与
      # usage totals 由 kiana-session-core 实现（ARCHITECTURE.md §5 的 kiana-config 树只有 cache/warmer.rs）。
    bin/
      generate_schemas.rs          # 2.31 renderConfigSchemas 的等价物
      generate_references.rs       # 2.32 renderReferences 的等价物
  schemas/                        # 生成产物（见下）
```

**与其它 crate 的边界**：`kiana-ai`（`Models`/`Provider`/`Model`/`Usage`/`calculateCost`/`lazy_stream`/`ModelsError`）、`kiana_ai::auth`（**不是独立 crate**；`Credential`/`CredentialStore`/`ApiKeyAuth`/`OAuthAuth`，`ARCHITECTURE.md` §3.1 拒绝拆出 `kiana-ai-auth`）、`kiana-tui`（`Key`/`TUI_KEYBINDINGS`/`KeybindingsManager`）、`kiana-agent-core`（`ThinkingLevel`/`Message`）、`kiana-session-core`（`SessionEntry`/`UsageEntry`/`virtual_models`；骨架里 `kiana-session` 是 L11，config 在 L5，不能反向依赖）。`config` 模块**不得**反向依赖 CLI/UI 层；`errors` 用 `thiserror` 定义，但 `Display` 输出必须与 TS 的 `error.message` 逐字一致（因为会被拼进 `getError()` 与用户的错误框）。

**Schema 生成**：为对齐 `npm run generate:schemas`，建议一个二进制目标 `src/bin/generate_schemas.rs`（骨架的 `kiana-config` 尚无 `bin/`，见 `spec/GAPS.md` G3.4），从 Rust 结构体（经 `schemars` 或手写 `serde_json::Value` 构建）产出 4 个 JSON 文件，再与 `packages/coding-agent/schemas/*.json` 逐字节比对作为测试。四个文件必须**逐字节**一致（`config-schemas.test.ts` 的 `matches the four committed artifacts`），因此不要用 `schemars` 的默认输出——直接照抄 TS 的 `Type.Object` 结构（属性顺序、`anyOf` 而非 `oneOf`、`$defs` 名称、`description` 位置）最稳妥。

### 8.2 状态与并发模型（Rust 侧）

- `SettingsManager` 在 TS 里是**单实例、同步 getter、异步写队列**。Rust 用 `Arc<SettingsState>`，内部 `RwLock<SettingsData>` + `tokio::sync::Mutex<()>` 作为写串行化；getter 同步加 `RwLock` 读锁；setter 同步改内存并 `spawn` 一个写任务（必须保证**顺序**：用 `mpsc::UnboundedSender` 或 `Mutex<VecDeque>` 的 FIFO）。
- `ModelRuntime` 的读方法是同步的（返回最后快照）。用 `RwLock<Snapshot>` + `Mutex<...>` 保护 provider map；异步刷新在 `tokio::spawn` 中完成并把结果写回。三个序号（`availability_refresh_seq` / `availability_error_seq` / `provider_availability_seq`）必须用 `AtomicU64` 并在每次读写时严格遵守"取号/比对"的顺序，否则会丢掉过期判定。
- 凭证操作串行化：`HashMap<String, Pin<Box<dyn Future>>>` 的 promise 链在 Rust 里用 `HashMap<String, Arc<tokio::sync::Mutex<()>>>`（每 provider 一把锁）**无法**完全等价——因为 TS 的 `enqueueCredentialOperation` 会在**轮到自己开始**时 resolve 调用方的 `started`，并且 `raceWithAbortSignal(started, signal)` 让调用方可以先行返回/报取消。Rust 等价物：为每个 provider 维护一个 `tokio::sync::Semaphore(1)`；调用方 `select!` 于 `semaphore.acquire()` 与 `cancel.cancelled()` 之间；获得许可后 `ensure_not_cancelled()?` 再执行任务（任务本身在持有 permit 的 future 中跑，即"取消不取消已开始的任务"）。**注意** `model-runtime-credential-sync.test.ts` 的「已提交的修改在报告取消前先 settle」要求 permit 的释放与任务完成绑定。
- `AuthStorage` 的文件锁是**跨进程**的：Rust 用目录锁（`fs::create_dir(lock_path)` 原子性）模拟 proper-lockfile；必须实现 10×20 ms 同步重试（仅 `ELOCKED` 等价物 = `AlreadyExists`），Windows 上 `EPERM`→`PermissionDenied` 额外 5 次异步重试。
- 读合并（read coalescing）：`AuthStorage`/`FileModelsStore` 用「同一 revision 只读一次」+ 读者计数。Rust 用 `Mutex<ReadState>` 持 `Option<Arc<Shared<BoxFuture<...>>>>` + `AtomicUsize` 计数；最后一位读者离开时 `CancellationToken::cancel()`。

### 8.3 最难复现的部分（按难度排序）

1. **`SettingsManager` 的选择性持久化 + `deepMergeObjects` + 项目/全局双作用域**。`persistScopedSettings` 要同时满足：只写本会话修改过的字段（`modifiedFields`/`modifiedNestedFields` + 各自的 project 变体）、保留外部对其它字段（含数组元素）的编辑、项目未受信时读 `{}` 写抛错、写入串行、`.kiana` 目录只在写时创建。`settings-manager-bug.test.ts` 的 4 条用例会捕获任何"把整份内存对象写回"的偷懒实现。
2. **`ModelRuntime` 的三组序号与快照一致性**。`runAvailabilityRefresh` / `queueAvailabilityRefresh` / `refreshProviderAvailability` / `getAvailable` 四个入口各自增减序号，且 `available` 的最终顺序由 `all` 的顺序决定（先建 key→model 的 map 再按 all 过滤）。任何一处顺序颠倒都会让 `credential-sync` 与 `auth-options` 测试失败。
3. **provider 四层合成的精确顺序与回退**：`applyModelsJson`（内置 baseUrl/compat/radius 特例/自定义模型 upsert）→ `applyExtension`（扩展 models 是权威列表，`baseUrl` 硬覆盖）→ 旧式 OAuth `modifyModels`（chat 与非 chat **重排**）→ `modelOverrides`（仅 chat）。加上 `composeProvider` 的"无覆盖层时原样返回内置 provider"短路（保证内置认证行为逐字一致）。
4. **`resolveModelReference` 的裁决规则**（`selectReference` 的 6 步 + `selectFuzzy` 的 usable 过滤 + `bestFuzzyMatch` 的别名优先与 `localeCompare` 降序 + `resolveText` 的三模式递归）。40+ 条 `referenceCases` 逐条复刻；任何"更简单"的实现都会在某些 case 上选错 provider。
5. **`proper-lockfile` 的跨进程语义**：目录锁、`realpath:false`、自定义 `lockfilePath`（trust 锁的是目录但锁文件名是 `${path}.lock`）、`ELOCKED` 重试、Windows `EPERM` 特殊重试、`onCompromised` → `Auth storage lock was compromised`、陈旧锁 30 s。还要与 Node 版**互操作**（两个实现同时操作同一 `~/.kiana`）。
6. **`CacheWarmer` 的定时器语义**：`setTimeout` 可迟到（睡眠/事件循环阻塞），因此 `refreshDeadlineAt = nextWarmAt + floor((ttl - delay)/2)` 与"扩展决策后**再次**检查 deadline"是两处独立判定；`timer.unref()` 让进程不被定时器吊住；`start` 替换旧 run 时先 `clearRun()`（`clearTimeout` + `abort`）。
7. **`KeyId` 正则的负向前瞻**：ECMA `(?!...)` 在 Rust `regex` 不可用；推荐手写校验（按 `+` 分段：修饰键段数 ≤4、每个修饰键最多一次、最后一段在基础键集合内、基础键大小写敏感）。但 `KeybindingValueSchema` 会被 `config-schemas.test.ts` 与生成的 JSON schema 断言：JSON 里**必须**保留原样的正则字符串，所以生成 schema 时用字面量 pattern，运行时校验用等价手写逻辑。
8. **JSON 序列化的字节一致性**：`serde_json` 默认 `BTreeMap` 会重排键；必须开 `preserve_order`（`indexmap`），并模拟 JS 的「整数索引键前置升序」规则（provider id 若是纯数字）。`trust.json` 显式排序 + 末尾 `\n`，而 `settings.json` / `auth.json` / `models-store.json` **无**末尾换行。缩进一律 2 空格，满足 `JSON.stringify(v, null, 2)`（数组/对象换行、`": "` 分隔、空对象 `{}`、空数组 `[]`）。
9. **`$ENV` / `!command` 的模板状态机**：`$NAME`、`${NAME}`、`$$`→`$`、`$!`→`!`、裸 `$` 的处理、`appendLiteral` 的合并规则（相邻字面量合并以减少片段数）、`!command` 的 10 000 ms 超时 + stdout trim + 空输出 → undefined + **按进程缓存（成功与失败都缓存）** + Windows 用 stdin 传输命令。`resolve-config-value.test.ts` 的 8 个 `test(...)` 语句覆盖这些（其中 `test.each(["!exit 1","!nonexistent-command-12345","!printf ''"])` 参数化 3 例；运行时共 10 个用例；§7.1 的「8」按语句数计，与本条一致）。
10. **`localeCompare` 与 `toLowerCase` 的 Unicode 细节**：排序（`bestFuzzyMatch` 的 `b.id.localeCompare(a.id)`、歧义 provider 排序）与大小写不敏感匹配都依赖 JS 内建。测试数据基本是 ASCII；建议实现 `js_lower`（Unicode simple/full lowercase）与 `js_locale_compare`（先按 ICU root 的 collation，退化到码点比较）并加 ASCII 黄金测试。
11. **`undici` dispatcher 行为**：`EnvHttpProxyAgent(allowH2:false, proxyTunnel:true)`、`bodyTimeout/headersTimeout = idle`（`disabled` → 0 表示不超时）、`autoSelectFamilyAttemptTimeout = 2000`、`undici.install?.()` 只在尚未安装时执行（`installedGlobalFetch` 跟踪），以及 dispatcher 的 `error` 监听防止未捕获错误崩溃。Rust 侧用 `reqwest::ClientBuilder`（`read_timeout` / `proxy` / `happy_eyeballs_timeout`）近似，但没有"全局 fetch 替换"这一步——若上层依赖该副作用需改为显式传入 Client。
12. **`getBranchSelection` 的"最多一次目录查询"**：既要语义正确（虚拟选择保持到下一个 `model_change`、失败的虚拟消息跳过），又要满足测试对 `lookups` 数组的**精确断言**（0 或 1 个元素）。实现时严禁在循环里调用 `get_model`。
13. **`CacheWarmer` 的经济学与格式化**：`calculateCost` 的浮点累加顺序（`price()` 的 Usage 构造）、`toFixed(3)`/`toFixed(6)` 的舍入、`Cache warmed` 的正则 `/(\.\d{3}\d*?)0+$/` 裁剪。Rust 的 `format!("{:.3}")` 使用银行家舍入，与 JS 的 `toFixed` 在 .5 边界上不同；需要写与 V8 一致的 `js_to_fixed`（实现 `Number.prototype.toFixed` 的规范算法：取最接近的十进制表示，必要时用任意精度整数）。
14. **`withRemoteCatalog` 的 304/404/501/失败四分支与 etag 策略**：`validator` 仅在 `stored.models.length > 0` 时发送；失败时**保留** etag 与 overlay 但更新 `checkedAt`；`publish` 返回 falsy 时立即放弃（连 `dynamicModels` 赋值都发生在 `update` 回调内，必须由 publish 触发）。
15. **错误文案的 `String(value)` 语义**：`Invalid editorPaddingX setting: ${String(value)}` 等。JS 的 `String(1.0)` = `"1"`、`String(NaN)` = `"NaN"`、`String(null)` = `"null"`、`String(undefined)` = `"undefined"`、`String(true)` = `"true"`、`String([1,2])` = `"1,2"`（本单元多为数字/字符串）。Rust 需要 `js_string(value)` 辅助函数，不能直接 `format!("{}", f64)`。

### 8.4 验收建议

1. 逐条移植 §7 的测试；对每个【逐字】字符串建立黄金测试（`assert_eq!`）。
2. 对 4 个生成的 JSON Schema 做字节比对测试。
3. 对 `settings.json` / `auth.json` / `models-store.json` / `trust.json` / `keybindings.json` 各写"写入后逐字节比对"测试（含键顺序与末尾换行差异）。
4. 用 Node 版与 Rust 版同时操作同一个临时 `<agent-dir>` 做互操作测试（锁、迁移、读写合并）。
5. 对 `resolveModelReference` 的 40+ `referenceCases` 建表驱动测试（表直接从 TS 测试文件转录）。
6. 对 `CacheWarmer` 用可注入时钟（`now: () => i64`）与手动触发的 timer 复刻 9 条用例，避免真实等待。

---

## 附录 A：本单元导出符号统计

30 个源文件中**去重后**的导出符号共 **188** 个（按名字去重；同名的 `export type {}` 再导出与其定义只算一次）。逐文件（去重后）：`settings-manager.ts` 34、`settings-schema.ts` 23、`model-resolver.ts` 17、`provider-composer.ts` 14、`virtual-models.ts` 13、`cache-warmer.ts` 12、`resolve-config-value.ts` 11、`keybindings.ts` 9、`trust-manager.ts` 8、`cache-stats.ts` 7、`model-config.ts` 6、`auth-storage.ts` 6、`usage-totals.ts` 6、`http-dispatcher.ts` 6、`model-runtime.ts` 5、`remote-catalog-provider.ts` 5、`model-registry.ts` 4、`auth-guidance.ts` 4、`radius.ts` 4、`key-id-schema.ts` 3、`project-trust.ts` 3、`settings-diagnostics.ts` 2、`models-store.ts` 2、`keybindings-schema.ts` 2、`defaults.ts` 2、`settings-defaults.ts` 1、`runtime-credentials.ts` 1、`client-identity.ts` 1、`telemetry.ts` 1、`experimental.ts` 1。若按 `^export` **声明行**计数（不按名字去重）则为 190 条（`model-resolver.ts` 与 `project-trust.ts` 各多一条 `export type {}` 转发）。

另需加上两个脚本的导出：`scripts/generate-schemas.ts` 的 `renderConfigSchemas`、`scripts/generate-references.ts` 的 `renderReferences`（见 §2.31/§2.32）。

## 附录 B：文件行数核对

```
settings-manager.ts 1443   provider-composer.ts 737   model-resolver.ts 689   auth-storage.ts 507
settings-schema.ts   482   cache-warmer.ts      453   keybindings.ts       359   resolve-config-value.ts 287
trust-manager.ts     246   model-runtime.ts    1049   model-registry.ts    244   remote-catalog-provider.ts 240
virtual-models.ts    238   model-config.ts      219   cache-stats.ts       174   models-store.ts 147
http-dispatcher.ts   113   usage-totals.ts       99   project-trust.ts      97   settings-defaults.ts 67
runtime-credentials.ts 52  settings-diagnostics.ts 25 auth-guidance.ts      25   keybindings-schema.ts 21
key-id-schema.ts      21   client-identity.ts    16   telemetry.ts          13   defaults.ts 12
radius.ts             11   experimental.ts        3
```
合计 **8089** 行（与 §0.1 一致）。

---

## Audit additions（审计补充）

> 本节由完整性审计追加。上面各节已**原地修正**的错误在 §A.5 汇总；本节补充此前完全缺失的规范（§A.1 协议脚本、§A.2 测试清单缺口、§A.3 环境变量与依赖细节、§A.4 Rust 映射补充）。

### A.1 `scripts/model-catalog-protocol.ts`（260 行）与 `scripts/model-catalog-protocol.test.ts`（120 行 / 7 条）

**位置很重要**：这两个文件在**仓库根目录** `scripts/`，不在 `packages/coding-agent/scripts/` 下（此前 §2.24 只在正文里顺带提到 `parseModelCatalogRequest`，没有任何完整规范）。源文件头注释要求它与 kiana.dev 的 `src/shared/models/protocol.ts` **逐字节相同**、无 import、可在 Node.js 与 Cloudflare Workers 中运行。

#### A.1.1 常量（逐字）

```ts
export const MODEL_CATALOG_SCHEMA_VERSION = 1;
export const MODEL_CATALOG_PREFIX = `models/v${MODEL_CATALOG_SCHEMA_VERSION}`;   // "models/v1"
export const MODEL_CATALOG_INDEX_KEY = `${MODEL_CATALOG_PREFIX}/index.json`;      // "models/v1/index.json"
export const MODEL_CATALOG_REVISION_RE = /^sha256-[0-9a-f]{64}$/;

// 以下三个非导出
const KIANA_VERSION_RE = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/;
const KIANA_USER_AGENT_RE = /^kiana\/([^\s()]+)(?: \([^;()]+(?:;\s*[^;()]+(?:;\s*[^()]+)?)?\))?$/i;   // 注意 i 标志
const MODEL_TYPE_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
```

#### A.1.2 存储布局（`MODEL_CATALOG_PREFIX` 之下，逐字）

```
index.json                                    revision index（ModelCatalogIndex）
revisions/<revision>/models.json              chat models by provider and id
revisions/<revision>/models.all.json          models of every type by provider
revisions/<revision>/providers.json           sorted provider ids
revisions/<revision>/providers/<id>.json      chat models of one provider by id
revisions/<revision>/providers/<id>.all.json  models of every type of one provider
```
发送 `?types=` 的客户端拿 `typed`（`.all.json`）变体；老客户端拿 legacy（chat-only）变体。

#### A.1.3 类型

```ts
export type ModelCatalogArtifact = "models.json" | "models.all.json" | "providers.json";
export type ModelCatalogRepresentation = "legacy" | "typed";
export type ModelCatalogIndexEntry = { minimumKianaVersion: string; revision: string };
export type ModelCatalogIndex = { schemaVersion: number; defaultRevision: string; catalogs: ModelCatalogIndexEntry[] };
export type ModelCatalogRequest =
	| { kind: "catalog"; kianaVersion: string | undefined; representation: ModelCatalogRepresentation }
	| { kind: "redirect"; location: string }
	| { kind: "invalid"; error: string };
```

#### A.1.4 函数（逐条，错误文案【逐字】）

- `getModelCatalogArtifactKey(revision, artifact)` → `` `${MODEL_CATALOG_PREFIX}/revisions/${revision}/${artifact}` ``。
- `getModelCatalogProviderKey(revision, provider, representation)` → 后缀 `representation === "typed" ? ".all.json" : ".json"`，即 `` `models/v1/revisions/<rev>/providers/<provider>.json` `` 或 `.../providers/<provider>.all.json`。
- `isValidModelCatalogKianaVersion(version)` → `parseKianaVersion(version) !== undefined`。
- `compareModelCatalogKianaVersions(left, right)` → 两者先 `requireKianaVersion`（非法抛 `` `Invalid Kiana version: ${version}` ``），再 `compareKianaVersions`。
- `parseModelCatalogRepresentation(types)`：
  - `types === undefined || types === null` → `"legacy"`（不传 `?types=` 的客户端早于模型类型）；
  - `types === ""` 或 `types.split(",")` 任一不匹配 `MODEL_TYPE_RE` → `undefined`（**非法**）；
  - 否则 → `"typed"`。
- `parseModelCatalogRequest(url, userAgent)`（判定顺序固定）：
  1. `new URL(url)`；`representation = parseModelCatalogRepresentation(searchParams.get("types"))`；为 `undefined` → `{kind:"invalid", error:"Invalid model types."}` **【逐字】**。
  2. `kianaVersion = searchParams.get("kiana-version")`；为 `null`（参数缺失）时：
     - `userAgentVersion = KIANA_USER_AGENT_RE.exec(userAgent ?? "")?.[1]`；
     - 若存在且 `isValidModelCatalogKianaVersion` → 在**同一 URL** 上 `searchParams.set("kiana-version", userAgentVersion)`，返回 `{kind:"redirect", location: requestUrl.toString()}`；
     - 否则 → `{kind:"catalog", kianaVersion: undefined, representation}`。
  3. `kiana-version` 存在但不合法 → `{kind:"invalid", error:"Invalid Kiana version."}` **【逐字】**。
  4. 否则 → `{kind:"catalog", kianaVersion, representation}`。
  - 设计原因（源码注释）：已发布客户端不发 `kiana-version`；响应按 URL 缓存，所以不能按 User-Agent 变化响应，改为 **307 重定向**到带 `kiana-version` 的等价 URL。没有 Kiana User-Agent 的请求拿默认目录。
  - 注意：`URL.toString()` 会保留原有查询串及编码（测试断言 `types=chat%2Cimage` 原样保留、只追加 `&kiana-version=0.84.4`）。
- `selectModelCatalog(index, kianaVersion)`：
  - `kianaVersion === undefined` → `index.catalogs.find(c => c.revision === index.defaultRevision)`（可能 `undefined`）。
  - 非法版本 → `undefined`；否则选「`minimumKianaVersion <= requested` 且 `minimumKianaVersion` 最大」的条目；无匹配 → `undefined`。
- `parseModelCatalogIndex(value)`：校验 `object` 非 null 非数组、`schemaVersion === 1`、`defaultRevision` 是字符串且匹配 `MODEL_CATALOG_REVISION_RE`、`catalogs` 是非空数组、每项 `minimumKianaVersion` 可解析且 `revision` 匹配正则、且**至少一项**的 `revision === defaultRevision`；任一不满足 → 抛 `` `Model catalog index is invalid: ${MODEL_CATALOG_INDEX_KEY}` `` **【逐字】**。返回值**只保留协议字段**（`catalogs` 每项只取 `minimumKianaVersion` 与 `revision`，发布方的额外元数据如 `sourceCommit`/`modelCount` 被丢弃）。
- 私有 `parseKianaVersion(version)`：对 `version.trim()` 匹配 `KIANA_VERSION_RE`；三段数字须全部 `Number.isSafeInteger`，否则 `undefined`；**build 元数据被丢弃**，prerelease 缺省为 `""`。返回 `[major, minor, patch, prerelease]`。
- 私有 `compareKianaVersions(left, right)`：先按 major/minor/patch 数值差；再处理 prerelease：相等 → 0；left 为空（正式版）→ 1；right 为空 → -1；否则按 `.` 分段：逐段比较，left 段缺失 → -1、right 段缺失 → 1；两段都是纯数字 → 数值差；仅 left 数字 → -1；仅 right 数字 → 1；否则 `leftPart.localeCompare(rightPart)`（**JS localeCompare 语义**，Rust 需 `js_locale_compare`）。

#### A.1.5 307 重定向契约（客户端视角，与 §2.24.2 对应）

- `ModelRuntime` 的目录请求只发原 URL（`/api/models/providers/<id>?types=chat%2Cimage%2Cclassifier`，无 `kiana-version`）。
- 服务端（`parseModelCatalogRequest`）看到 UA `kiana/<semver> (...)` 且无 `kiana-version` 时返回 **307**，`location` 为追加 `&kiana-version=<UA 中的版本>` 的同一 URL；测试服务器还带 `cache-control: no-store`。
- 客户端**必须跟随** 307/308 且保留方法与 body（GET 无 body）；Rust `reqwest` 默认最多 10 次重定向。
- 服务端对非法 `types` 返回 400 + `Invalid model types.`，对非法 `kiana-version` 返回 400 + `Invalid Kiana version.`（见 `packages/coding-agent/test/model-catalog-protocol.test.ts` 里的 minimal server；该文件用 `x-kiana-model-catalog-revision` 头仅在**测试服务器**内传递选中 revision，不是协议的一部分）。

#### A.1.6 `scripts/model-catalog-protocol.test.ts` 的 7 条（node:test，`npm run test:scripts`）

1. `builds revision scoped storage keys`：三个 key 拼接逐字。
2. `orders Kiana versions with semver precedence`：`0.85.0 > 0.84.4`；`0.85.0-rc.1 < 0.85.0`；`0.85.0-rc.2 < 0.85.0-rc.10`（**数字段按数值**）；`v1.0.0 == 1.0.0+build`（忽略 `v` 与 build）；`0.85` 抛 `/Invalid Kiana version/`。
3. `selects the newest catalog a Kiana version supports`（回归 #9099）：`0.84.4 → legacy`、`0.85.0-rc.1 → legacy`、`0.85.0 → mixedApi`、`1.0.0 → mixedApi`、`0.80.6 → undefined`、`undefined → defaultRevision`。
4. `redirects Kiana user agents to an explicit catalog version`：UA `kiana/0.84.4 (linux; node/v22.0.0; x64)` + `?types=chat%2Cimage` → `location` 恰为 `https://kiana.dev/api/models/providers/openrouter?types=chat%2Cimage&kiana-version=0.84.4`；无查询串时 `https://kiana.dev/api/models?kiana-version=0.84.4`。
5. `serves explicit and unversioned catalog requests without redirecting`：已有合法 `kiana-version` 时不再重定向（即使 UA 有更高版本）；`types=chat` + `curl/8.0.0` → `typed` 且 `kianaVersion: undefined`；UA `kiana/latest`（版本非法）→ 不重定向，`legacy`。
6. `rejects invalid catalog request parameters`：`kiana-version=latest` → `Invalid Kiana version.`；`types=` → `Invalid model types.`；`parseModelCatalogRepresentation("chat,-image")` → `undefined`（`-image` 不匹配 `MODEL_TYPE_RE`）；`parseModelCatalogRepresentation(null)` → `"legacy"`。
7. `validates stored indexes and drops publication metadata`：带 `sourceCommit`/`modelCount` 的 index 解析后恰等于纯协议对象；`schemaVersion: 2` 抛 `/index is invalid/`；`defaultRevision` 不在 catalogs 中抛错；`minimumKianaVersion: "latest"` 抛错。

#### A.1.7 Rust 映射要点

- `KIANA_VERSION_RE` / `MODEL_CATALOG_REVISION_RE` / `MODEL_TYPE_RE` 用 Rust `regex` crate 可直接表达（不需要反向引用）。注意 **`\d` 语义差异**：JS 的 `\d` 只匹配 ASCII `0-9`，Rust `regex` 的 `\d` 默认是 Unicode 十进制数字类；逐字节对齐时请写成 `[0-9]` 或 `(?-u:\d)`。`KIANA_USER_AGENT_RE` 的 `i` 标志 → Rust `(?i)` 前缀。
- `Number.isSafeInteger` 不能只靠 `i64` 解析：JS 的 `Number("9007199254740993")` 已不 safe → 判非法，而 Rust `parse::<i64>()` 会成功。需显式校验 `<= 9_007_199_254_740_991`（且 `Number("007") == 7`，Rust `parse` 同样接受前导零，这一点一致）。
- prerelease 比较里的 `localeCompare` 不能用 `str::cmp`（ICU root collation 与码点序不同，虽然本题数据多为 ASCII）；建议 `js_locale_compare`。
- `parseModelCatalogIndex` 的「只保留协议字段」语义：Rust 用 `#[serde(deny_unknown_fields)]` 会**误拒**发布方元数据，应改为显式只取两个字段（serde 默认忽略未知字段即可）。

### A.2 测试清单缺口（§7.1 / §7.5 之外，均已逐一核对用例数）

以下文件直接 import 本单元模块并锁定其行为，此前完全未在 §7 出现。用例数按 `it(`/`test(` 语句计数（`it.each` 记 1）。

| 测试文件 | 用例数 | 锁定行为 |
|---|---|---|
| `test/runtime-credentials.test.ts` | 5 | `RuntimeCredentials`：override 遮蔽存储凭证且不持久化；`list` 合并 override 且不泄露 key；操作 signal 透传底层 store；持久删除取消时保留 override；`delete` 同时清 override 与持久凭证 |
| `test/suite/virtual-models.test.ts`（AgentSession 级） | 13 | 每轮（含重试）路由且选择保持虚拟；turn 的首请求按该 turn 路由；截断后 compact-and-retry 作为重试路由；扩展消息后为 continuation；扩展消息跟在 prompt 后时首请求按 user turn；路由失败以 error 响应结束并保留最后物理限额；用产生响应的物理模型检查 compaction；路由到更小窗口前先压缩；运行中下一请求路由到更小窗口时先行压缩；每个请求只投影一次会话；router state 存在分支上并传给后续请求；扩展提供的 compaction 不路由；compaction summary 路由到解析出的模型 |
| `test/suite/regressions/2753-reload-stale-resource-settings.test.ts` | 1 | 启动后 reload 要应用更新后的顶层 prompt 设置（`SettingsManager` + `ModelRuntime` 热重载链路） |
| `test/suite/regressions/2835-tools-allowlist-filters-extension-tools.test.ts` | 2 | tools allowlist 过滤扩展工具（`getDefaultTools`/loadout 语义） |
| `test/suite/regressions/2860-replaced-session-context.test.ts` | 3 | 会话替换后重绑 `kiana/ctx`；`withSession` 支持 fork / switchSession |
| `test/suite/regressions/3217-scoped-model-order.test.ts` | 2 | 重排后的 scoped models 回写到 session state |
| `test/suite/regressions/3592-no-builtin-tools-keeps-extension-tools.test.ts` | 3 | 关闭内置工具时保留扩展工具 |
| `test/suite/regressions/3616-settings-inmemory-reload.test.ts` | 3 | `SettingsManager.inMemory(...)` 的初始设置在 `reload()`（直接与经 `DefaultResourceLoader`）后**保留**——这是 §2.9.9「reload 失败不清空」之外的另一条约束：`InMemorySettingsStorage` 的 `withLock` 必须把初始值作为 current 返回 |
| `test/suite/regressions/5433-extension-oauth-prompt-input.test.ts` | 5 | 后一个 prompt 活跃时保持前一个输入稳定；显示 prompt 时保留 auth 指引/中性信息与链接/setup 细节；手动输入稳定 |
| `test/suite/regressions/5596-missing-theme-export.test.ts` | 1 | 配置主题缺失时用活跃 fallback 主题导出（`getThemeSetting`/`getTheme` 语义） |
| `test/suite/regressions/5661-uppercase-header-values.test.ts` | 1 | 启动迁移时把 models.json 中大写 header 值保持为字面量（`config-value-migration` 相关，见 §2.11/§2.18） |
| `test/suite/regressions/6949-unavailable-scoped-model.test.ts` | 4 | 显示并移除无目录条目的 enabled model；未匹配 settings pattern 合并为一次解析交给 selector；仅 session-scoped 不可用时仍打开；enabled model 不可用不清空部分 scope |
| `test/suite/regressions/6999-models-json-hot-reload.test.ts` | 1 | models.json 热重载后刷新并渲染（`ModelRuntime.refresh` + `ModelConfig.load`） |
| `test/suite/regressions/7027-credential-refresh-hang.test.ts` | 6（含 `it.each` 两个参数组） | 登录不被更早卡住的网络目录刷新阻塞；交互登录在有界后台刷新前完成（15 s 超时后 showWarning 的文案为 `${actionLabel}, but its model catalog refresh timed out; using cached models.`，其中 `actionLabel` 对 api_key 是 `Saved API key for ${providerName}`、对 oauth 是 `Logged in to ${providerName}`；刷新出错时改为 `... could not be refreshed; using cached models.`；测试断言 `Saved API key for Stalled Login, but its model catalog refresh timed out; using cached models.`）；登录后按刷新出的目录从 `defaultModelPerProvider[provider]` 选模型；空目录只在刷新后报 `no models are available`；刷新期间已选模型不被覆盖；刷新限时 15 s |
| `test/suite/regressions/7153-scoped-models-refresh.test.ts` | 2 | selector 先渲染缓存模型、后台刷新后更新；关闭 selector 时取消后台刷新 |
| `test/suite/regressions/7187-malformed-package-manifest.test.ts` | 1 | 畸形 package manifest 的处理（`setPackages` 读路径） |
| `test/suite/regressions/7209-model-selector-filter-resets-selection.test.ts` | 2 | 模型选择器过滤时重置选中项 |
| `test/suite/regressions/7572-provider-retry-settings-merge.test.ts` | 1 | 嵌套 `retry.provider` 的项目设置只覆盖自己的字段，global 的其余字段保留（`deepMergeSettings` 深合并语义） |
| `test/suite/regressions/8337-utf8-bom-parsing.test.ts` | 1 | settings.json / frontmatter 带前导 BOM 时仍能解析（`stripBom` 在 settings 读路径必须生效） |
| 根 `scripts/model-catalog-protocol.test.ts` | 7 | 见 §A.1.6（node:test，`npm run test:scripts`） |

另有 `test/suite/virtual-models.test.ts`（13）与 `test/virtual-models.test.ts`（17，已在 §7.4）是**不同**文件：前者是 AgentSession 集成级，后者是模块级。Rust 侧建议两组都复刻（模块级验证 `getBranchSelection` 的 `lookups` 精确断言，集成级验证路由状态机）。

### A.3 环境变量与依赖细节补充

- **§4.9 的 `KIANA_ATTRIBUTION` 行已删除**：它是 `ai-core` 的常量 `ClientAttribution` 对象（`packages/ai/src/utils/identity.ts:24`，`{ name:"kiana", url:"https://kiana.dev", categories:["cli-agent"] }`），不是环境变量。§5.1.1 的类型也已从 `string` 修正为 `ClientAttribution`。
- `KIANA_CACHE_RETENTION` 的查找链是 `getProviderEnvValue(name, env)` 内的 `env?.[name] || process.env[name] || Bun 沙箱兜底`（注意是 `||` 不是 `??`：空串会继续下探）：Bun 编译产物在 Linux sandbox 中 `process.env` 可能为空，此时 `getProviderEnvValue` 会读 `/proc/self/environ`（按 `\0` 分割、`key=value` 取第一个 `=`）。Rust 侧不需要该兜底，但**不得**改成只读 `process.env` 而跳过 `options.env`。
- `getShellConfig(customShellPath?)`（供 `!command` 用）在 win32 上还读 `ProgramFiles`、`ProgramFiles(x86)`、`SystemRoot` 等；自定义路径不存在时抛 `` `Custom shell path not found: ${customShellPath}` ``。接口已补进 §5.1.5（含 `commandTransport?: "argv" | "stdin"`）。
- 以下变量出现在 `src/core/` 但**不属于本单元**，不要误加：`KIANA_TIMING`（`timings.ts`）、`CI` / `TMUX` / `SSH_CLIENT` / `SSH_CONNECTION` / `SSH_TTY` / `SHELL`（`bug-report.ts`）。
- `fetchWithRetry` 的完整选项（`maxRetries` 默认 2、`retryOnStatus` 默认 true、`timeoutMs` 总预算、`attemptTimeoutMs` 单次）已补进 §5.1.5；此前只写了 `attemptTimeoutMs`。

### A.4 Rust 映射补充

- `SettingsManager`：71 个 getter / 57 个 setter；**没有**泛型 `get<K>(key)`；`save()` / `saveProjectSettings()` / `persistScopedSettings()` 是 private。Rust 侧若用 trait/宏批量生成 getter，必须保证 71 个名字逐一存在（§2.9.11 的表格按名字覆盖 getter，`getGlobalSettings` / `getProjectSettings` / `getSettings` 三个在 §2.9.8 单独描述）。注意 §2.9.11 小节末尾（表格之外）详述的错误文案属于 **private** 辅助方法 `getCompactionTokenSetting(field, model?)`（源码 `settings-manager.ts:833`），它不是公共 getter；公共入口只有 `getCompactionReserveTokens(model?)` 与 `getCompactionKeepRecentTokens(model?)`。
- §2.9.12 的 setter 表只列「有特殊行为」的 setter；其余 26 个（`setSteeringMode`、`setFollowUpMode`、`setTheme`、`setDefaultThinkingLevel`、`setTransport`、`setSessionShareDestination`、`setHideThinkingBlock`、`setShowCacheMissNotices`、`setQuietStartup`、`setDefaultProjectTrust`、`setCollapseChangelog`、`setEnableInstallTelemetry`、`setSkillPaths`、`setPromptTemplatePaths`、`setThemePaths`、`setEnableSkillCommands`、`setTuiMode`、`setFullscreenExitOutput`、`setFullscreenScrollbar`、`setFullscreenCopyOnSelect`、`setEnabledModels`、`setDoubleEscapeAction`、`setTreeFilterMode`、`setShowHardwareCursor`、`setLastChangelogVersion`、`setProjectTrusted`）都等价于「写 `globalSettings.<field>` + `markModified("<field>")` + `save()`」的机械模式。字段映射：`setSkillPaths→skills`、`setPromptTemplatePaths→prompts`、`setThemePaths→themes`（**setter 名与字段名不同**，别按名字推导）。`setProjectTrusted` 例外：它不走写盘，见 §2.9.8。
- `APP_KEYBINDINGS` 在 TS 中**未导出**（`keybindings.ts:25` 的模块级 `const`）；§5.2 把它列在导出面里会使 Rust 多暴露一个符号。若严格对齐导出面，应只导出 `KEYBINDINGS`、`KeybindingsManager`、`migrateKeybindingsConfig`、`useWindowsKeybindings` 以及类型 `AppKeybinding`/`AppKeybindings`/`Keybinding`/`KeyId`/`KeybindingsConfig`。
- `RuntimeCredentials.list` 的 override 覆盖同 provider 底层条目时**保持底层位置**（`Map.set` 对已存在键不改位置），新 provider 追加到末尾；Rust 用 `IndexMap` 的 `insert` 语义一致，但若用 `HashMap` 会丢顺序（`model-runtime-auth-options` 测试对顺序敏感）。

### A.5 本次审计原地修正的错误（供复核）

| 位置 | 原（错） | 现（对） |
|---|---|---|
| §2.1 | `ThinkingLevel` 不含 `off`，`ModelThinkingLevel = "off" \| ThinkingLevel` | `ThinkingLevel` **含** `off`（agent-core `packages/agent/src/types.ts:349`，7 值）；kiana-ai 的 `THINKING_LEVELS` 才是 6 值且不含 `off`，`MODEL_THINKING_LEVELS = ["off", ...THINKING_LEVELS]` |
| §2.9.9 | reload 失败会把该 scope 清空 | reload 失败**保留**旧 settings，只记录错误（`settings-manager.ts:500-526`；`settings-manager.test.ts:205-216` 断言 `getTheme()==="dark"`） |
| §2.25 | 生成正则里负向前瞻写成 `ctrl\+.*ctrl\+\+`（`\+` 重复） | 与 `schemas/keybindings.schema.json` 逐字一致的 343 字符版本（每处 `\+` 仅一次） |
| §4.9 | `KIANA_ATTRIBUTION` 作为环境变量行 | 删除；补上真实读取的 `HOME`（`trust-manager.ts:187`） |
| §5.1.1 | `export const KIANA_ATTRIBUTION: string;` | `export const KIANA_ATTRIBUTION: ClientAttribution;`（对象常量） |
| §5.1.5 | `fetchWithRetry(..., options: {attemptTimeoutMs?: number})` | 完整 `FetchRetryOptions`（4 个选项 + 重试语义） |
| §5.1.5 | `getShellConfig(): {shell, args}` | `getShellConfig(customShellPath?: string): ShellConfig`（含 `commandTransport`） |
| §5.1.6 | `emitProjectTrustEvent(ctx, event): Promise<ProjectTrustEventResult>`；`ProjectTrustEventResult { trusted?: boolean; save?: boolean }` | `emitProjectTrustEvent(extensionsResult, event, ctx): Promise<{result?, errors: ExtensionError[]}>`；`ProjectTrustEventResult { trusted: "yes"\|"no"\|"undecided"; remember?: boolean }` |
| §5.2 | `SettingsManager.create(cwd?, agentDir?)`、`fromStorage(storage, cwd?)`、`get<K>(key)` 强类型 getter 60 个、`save()` 公开 | `create(cwd, agentDir = getAgentDir(), options = {})`、`fromStorage(storage, options = {})`、**71** 个具名 getter（无泛型 `get`）、**57** 个 setter、`save()` 为 private |
| §5.2 | `SettingsStorage.withLock<T>(scope, fn => {result: T; next?: string}): T` | `withLock(scope, fn: (current: string \| undefined) => string \| undefined): void` |
| §5.2 | `readStoredCredential(authPath, providerId)` | `readStoredCredential(providerId, authPath = join(getAgentDir(), "auth.json"))` |
| §0.1 / §0.2 | 只列 `generate-schemas.ts` / `generate-references.ts` 两个脚本，且路径写成 `scripts/...` | 补 `scripts/model-catalog-protocol.ts`（260 行）+ `.test.ts`（120 行）并标明在**仓库根**；两个生成脚本路径改为 `packages/coding-agent/scripts/...` |
| §6.2 第 9 条 | `resolve-config-value.test.ts` 的 **7** 条用例 | **8** 个 `test(...)` 语句（其中一条 `test.each` 参数化 3 例，运行时 10 例），与 §7.1 的「8」一致 |
| §2.27.2 | `APP_KEYBINDINGS` 未标注导出状态 | 明确标注为**未导出**的模块级常量，并说明 §5.2 的导出面影响 |
| §5.1.5 | `getFileRevision(p: string): string` | `getFileRevision(p: string): string \| undefined`（`statSync(..., {bigint:true})` 失败 → `undefined`；`auth-storage.ts` 与 `models-store.ts` 的 revision 比较都依赖这个 `undefined`） |
| §4.10 | 只说 `fetchWithRetry` 的默认值 | 明确 `REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS = 4_000` 以 `attemptTimeoutMs` 传入、其余三个选项保持默认，并指向 §5.1.5 |
