# `ca-resources` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-coding-agent` 中 **ca-resources** 单元（资源发现与加载、Kiana 包管理、技能、
提示模板、系统提示、MCP 配置校验、SDK 会话工厂、Bug 报告、崩溃日志、页脚 git 数据、计时与诊断、
内置斜杠命令）的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`ca-resources`。
参考实现（只读，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。
> Rust 实现必须输出完全一致的字节序列（除另有说明）。中文用于解释语义。
> 凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B；`\x07` 表示 U+0007（BEL）。
> 文中 `APP_NAME` / `APP_TITLE` / `CONFIG_DIR_NAME` / `VERSION` / `PACKAGE_NAME` 是运行期常量（见 `ca-cli` §2.6），
> 默认值分别为 `kiana` / `π` / `.kiana` / `1.4.0`（取自 `packages/coding-agent/package.json`）/ `@earendil-works/kiana-coding-agent`。
> `ENV_AGENT_DIR` = `KIANA_CODING_AGENT_DIR`，`ENV_SESSION_DIR` = `KIANA_CODING_AGENT_SESSION_DIR`（由 `APP_NAME.toUpperCase()` 派生）。
> 本单元**不修改** TS 树中的任何文件；所有路径均以 `packages/coding-agent/` 为根。

---

## 目录

0. 单元边界与阅读顺序
1. 概览
2. 逐文件规范
3. 用户可见表面（逐字文本）
4. 外部格式（磁盘 / 环境变量 / 协议）
5. 跨单元接口
6. npm 依赖与 Rust crate 映射
7. 测试清单
8. Rust 模块布局与最难复现的部分

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
src/core/package-manager.ts           (2791 行)  包解析 / 安装 / 更新 / 资源收集
src/core/resource-loader.ts           (1322 行)  DefaultResourceLoader / ResourceLoader 接口
src/core/skills.ts                     (514 行)  SKILL.md 发现、校验、提示词格式化
src/core/prompt-templates.ts           (325 行)  模板加载、参数替换
src/core/system-prompt.ts              (229 行)  结构化 system prompt
src/core/mcp-servers.ts                (346 行)  mcp.json 校验、exposure、注册表
src/core/sdk.ts                        (479 行)  createAgentSession 工厂
src/core/bug-report.ts                 (375 行)  Bug 报告元数据 / 诊断 / ZIP / 摘要
src/core/bug-report-upload.ts           (37 行)  multipart 上传
src/core/crash-log.ts                  (170 行)  崩溃记录与栈归属
src/core/footer-data-provider.ts       (388 行)  git 分支 / 扩展状态 / 监听
src/core/source-info.ts                 (57 行)  SourceInfo 与合成路径
src/core/kiana-manifest.ts                 (35 行)  package.json 的 `kiana` 键
src/core/slash-commands.ts              (44 行)  内置斜杠命令表
src/core/timings.ts                     (50 行)  KIANA_TIMING 计时
src/core/diagnostics.ts                 (15 行)  ResourceDiagnostic / ResourceCollision
src/package-manager-cli.ts            (1151 行)  install/remove/update/list/config 子命令
install-lock/package.json                       安装器锁根（15 行）
install-lock/package-lock.json                  （1507 行，lockfileVersion 3）
```

相关文档（只读参考，语义权威）：

```
docs/packages.md              Kiana 包
docs/skills.md                技能
docs/prompt-templates.md      提示模板
docs/slash-commands.md        斜杠命令
docs/sdk.md                   SDK
docs/mcp.md                   MCP 服务器
```

相关测试（第 7 节逐条列出）：

```
test/package-manager.test.ts             (2941 行)
test/package-manager-ssh.test.ts           (97 行)
test/resource-loader.test.ts             (1476 行)
test/resource-loader-theme.test.ts        (146 行)
test/skills.test.ts                       (432 行)
test/prompt-templates.test.ts             (674 行)
test/system-prompt.test.ts                (238 行)
test/system-prompt-updates.test.ts        (303 行)
test/export-html-skill-block.test.ts       (40 行)
test/tool-system-prompt-contributions.test.ts (44 行)
test/bug-report.test.ts                   (113 行)
test/crash-log.test.ts                     (89 行)
test/footer-data-provider.test.ts         (279 行)
test/sdk-skills.test.ts                   (113 行)
test/sdk-session-manager.test.ts          (131 行)
test/sdk-stream-options.test.ts           (324 行)
test/sdk-client-identity.test.ts          (191 行)
test/sdk-codex-cache-probe-tool-loop.ts   (505 行，手动探针脚本，非 vitest)
test/interactive-mode-bug-report-hint.test.ts (71 行)
test/interactive-mode-assistant-diagnostics.test.ts (95 行)
test/suite/regressions/2753-reload-stale-resource-settings.test.ts (105 行)
test/suite/regressions/2781-skill-collision-precedence.test.ts     (120 行)
test/suite/regressions/2023-queued-slash-command-followup.test.ts  (80 行)
test/suite/regressions/7187-malformed-package-manifest.test.ts      (47 行)
test/suite/regressions/2791-fswatch-error-crash.test.ts            (106 行)
test/mcp-command.test.ts                  (197 行)
test/mcp-extension.test.ts                (868 行，校验部分属本单元)
test/mcp-manager-view.test.ts              (40 行)
test/package-command-paths.test.ts       (1076 行)
test/package-distribution.test.ts          (24 行)
test/settings-diagnostics.test.ts          (47 行)
```

### 0.2 本单元**不**实现（由其它单元拥有，ca-resources 只调用）

| 文件 / 包 | 归属单元 | 本单元如何使用 |
|---|---|---|
| `src/core/settings-manager.ts`、`settings-schema.ts`、`settings-defaults.ts`、`settings-diagnostics.ts` | `ca-config-models` | 读写 settings；`PackageSource` 类型 |
| `src/core/model-runtime.ts`、`model-resolver.ts`、`model-registry.ts`、`auth-storage.ts`、`client-identity.ts`、`telemetry.ts`、`defaults.ts`、`auth-guidance.ts`、`radius.ts` | `ca-config-models` | SDK 构造、Bug 报告 provider 描述 |
| `src/core/agent-session.ts`、`session-manager.ts`、`agent-session-runtime.ts`、`agent-session-services.ts`、`messages.ts`、`virtual-models.ts`、`cache-warmer.ts`、`output-guard.ts` | `ca-session` | SDK 组装 `AgentSession`；`getBranchSelection`；`CacheWarmer` |
| `src/core/extensions/**`（`loader.ts`、`types.ts`、`runner.ts` …） | **`kiana-ext-api`（类型）+ `kiana-extensions`（实现）**（spec 旧名 `durable-harness` 是 `packages/durable` 单元，与此无关） | `loadExtensionsCached`、`loadExtensionFromFactory`、`createExtensionRuntime`、`Extension` / `InlineExtension` / `LoadExtensionsResult` 类型 |
| `src/core/tools/**` | **`kiana-tools`**（spec 旧名 `durable-harness`） | SDK 的 `createCodingTools` 等；系统提示的 `promptSnippet` / `promptGuidelines` |
| `src/core/compaction/**` | `ca-session` | Bug 报告的 `estimateTokens` / `serializeConversation` / `completeSummarization` / `getSummarizationFailure` |
| `src/modes/interactive/theme/theme.ts`、`theme-schema.ts`、`theme-tokens.ts` | **`kiana-theme`**（spec 旧名 `tui-core`/`tui-components`；`highlightCode` 委托 `kiana-core::syntax_highlight`） | `loadThemeFromPath`、`Theme`、`getTerminalColorMode` |
| `@earendil-works/kiana-tui`（`detectCapabilities`、`getTerminalColorMode`、`Markdown`、`MarkdownTheme`、`Container`） | `kiana-tui`（`tui-core`/`tui-text`/`tui-components` 三个单元合成一个 crate） | 主题颜色模式、自更新说明 markdown 渲染 |
| `@earendil-works/kiana-ai`（`streamSimple`、`normalizeContext`、`getSystemMessageText`、`uuidv7`、`formatUserAgent`、`KIANA_ATTRIBUTION`、`completeSummarization` 依赖的流类型） | `ai-core` / `ai-api-*`（**spec 单元名**，crate 统一为 `kiana-ai`，见 ARCH §3.1） | SDK 默认 stream、系统提示文本拼接、Bug 报告 |
| `@earendil-works/kiana-agent-core`（`Agent`、`setDefaultStreamFn`、`AgentMessage`、`AgentTool`） | `agent-codemode`（`packages/agent` → `kiana-agent-core`） | SDK 构造 |
| `src/utils/stdout- takeover`（`output-guard.ts`） | `ca-session` | 包管理器 spawn 时决定 stdio |
| `src/utils/paths.ts`、`text.ts`、`child-process.ts`、`fs-watch.ts`、`git.ts`、`zip.ts`、`kiana-user-agent.ts`、`version-check.ts`、`windows-self-update.ts` | `ca-cli` | 被本单元直接调用（接口在第 5 节） |
| `src/cli/project-trust.ts`、`cli/config-selector.ts`、`core/project-trust.ts`、`core/trust-manager.ts` | `ca-cli` | `package-manager-cli` 的信任流程 |
| `src/core/export-html/**` | `ca-tools-compaction-export`（→ `kiana-export`） | `export-html-skill-block.test.ts` 断言其模板行为（第 7 节记录） |
| `packages/coding-agent/src/core/index.ts` | `ca-session` | 公共再导出 |

### 0.3 与 `ca-cli` 的重叠：`package-manager-cli.ts`

`ca-cli` §0.1 与 §2.9 也把 `src/package-manager-cli.ts` 列入其范围。**分工约定（本规范采用）**：

- **ca-cli 拥有**：命令行入口、`parsePackageCommand` 的参数文法、帮助文本渲染、`process.exitCode` 设置、
  子命令在 `main()` 中的调用顺序、`config` 子命令的 TUI 调用点、managed install 的文件系统动作序列。
- **ca-resources 拥有**：被 CLI 调用的**数据契约与语义**——`getActiveManagedInstallRoot()` 的 marker 校验规则、
  `MANAGED_RELEASE_VERSION_RE`、`KIANA_INSTALLER_API_BASE` 归一化、`cleanupManagedInstall()`、
  `runManagedSelfUpdate()` 的锁与目录布局、`DEFAULT_INSTALLER_API_BASE`，
  以及它们依赖的 `DefaultPackageManager` 行为（本单元 `package-manager.ts`）。
- **实现方式**：Rust 侧把 `package-manager-cli.ts` 整体放在 `kiana-coding-agent` crate 内（`ca-cli` 的模块；
  ARCHITECTURE.md §3.6/§3.7 已把 spec 里的 `kiana-cli` 改名为 `kiana-coding-agent`），
  但其中调用的 `PackageManager` trait 与本文 §2.10 的行为必须由 `kiana-resources` crate 提供。
  两单元若冲突，以**本文件 §2.10 的包管理器语义**与 **`ca-cli` §2.9 的 CLI 文案**为准。

`install-lock/**` 只被安装器读取，运行期 `kiana` 不读它；本单元负责保证其内容与 `package.json` 版本一致（§2.17）。

### 0.4 关键不变量（Rust 中必须逐条成立）

1. **首次写入胜出（first wins）**：`addResource` 对同一路径只记录首次出现的 `{metadata, enabled}`；
   后续同路径写入被静默忽略。资源优先级由 `resolve()` 的**添加顺序**决定，不由排序决定
   （排序只在 `toResolvedPaths` 中用于最终列表顺序与去重顺序）。
2. **累积器排序稳定**：`toResolvedPaths` 按 `resourcePrecedenceRank` **升序**排序（0 最高），
   同 rank 保持插入顺序（JS `Array.prototype.sort` 在 V8 中稳定）。随后按
   `canonicalizePath(path)` 去重，保留首个。
3. **项目作用域先于用户作用域**：`resolve()` 先处理 `projectSettings.packages` 再处理 `globalSettings.packages`；
   资源路径同理（project 在前）。因此项目资源在同名冲突中胜出。
4. **包去重按 identity**：npm 用包名；git 用 `host/path`（去掉 ref、协议归一）；local 用解析后的绝对路径。
   项目条目替换用户条目；例外：项目条目是对象且 `autoload === false` 时两者都保留（delta 在前）。
5. **trust 门禁**：项目作用域（`scope === "project"`）的所有包存储访问在未信任时抛
   `Project is not trusted; refusing to access project package storage`。
   `getBaseDirForScope("project")`、`getNpmInstallRoot("project")`、`getManagedNpmInstallPath(…,"project")`
   都经过该断言。
6. **离线模式跳过一切网络与安装**：`KIANA_OFFLINE` 为 `1` / `true` / `yes`（大小写不敏感）时，
   `resolvePackageSources` 不安装、`checkForAvailableUpdates` 返回 `[]`、`updateConfiguredSources` 直接返回、
   `npmHasAvailableUpdate` / `gitHasAvailableUpdate` 返回 false、临时 git 源不刷新。
7. **安装失败不阻断启动**：包安装抛错被捕获成 `PackageInstallFailure` 并作为启动警告上报，资源跳过。
8. **资源类型只有四种**：`extensions`（`.ts|.js`）、`skills`（`SKILL.md` 或 `.md`）、`prompts`（`.md`）、
   `themes`（`.json`）。未知类型抛 `Unknown resource type: ${resourceType}`。
9. **叠加而非替换的过滤**：包过滤（settings 对象形式）作用在**包 manifest 已声明的集合**上，
   永不扩大集合。`[]` 表示显式禁用该类型的全部资源。
10. **路径规范化与去重**：所有对外路径使用 `resolvePath(...)`（展开 `~`，必要时 trim），
    去重使用 `canonicalizePath`（`realpathSync`，失败回退原路径）。
11. **合成路径永不触碰文件系统**：`builtin:<name>` 与 `<inline:name>` 前缀路径跳过 `existsSync`。
12. **JSON 键顺序**：所有写入磁盘的 JSON（crash log、bug report、managed marker、`package.json` 模板）
    依赖对象字面量构造顺序。Rust 侧必须启用 `serde_json` 的 `preserve_order`（IndexMap），
    否则字节级输出不同。
13. **系统提示的 section 结构**：`preamble` 无标签，其余 section 包成 `<name>\n…\n</name>`，
    顺序为插入顺序（`preamble`、`tools`、`rules`、`docs`、`addendum`、`project_context`、`skills`、`cwd`、自定义）。
14. **计时开关在模块加载时求值一次**：`KIANA_TIMING === "1"` 在进程启动时确定，之后改环境变量无效。

---

## 1. 概览

### 1.1 目的

ca-resources 是 kiana 的**资源层 + 装配层**：

1. **资源发现**：从四个作用域（项目本地设置、项目自动发现、用户本地设置、用户自动发现）、
   Kiana 包（npm / git / 本地路径）、CLI 参数（`-e`、`--skill`、`--prompt-template`、`--theme`）、
   内置扩展中收集 extensions / skills / prompts / themes，带优先级、过滤、去重与冲突诊断。
2. **包生命周期**：解析包源（`npm:` / `git:` / 本地路径 / URL），安装到受管目录
   （`agentDir/npm`、`cwd/.kiana/npm`、`agentDir/git/<host>/<path>`、临时目录），更新、校验、
   迁移遗留全局安装、在 trust 未定时阻止项目包。
3. **系统提示构造**：把工具列表、规则、文档路径、追加指令、项目上下文文件、技能清单、cwd
   组合成结构化、可增量 diff 的 system prompt。
4. **SDK 装配**：`createAgentSession()` 把 `ModelRuntime`、`SettingsManager`、`SessionManager`、
   `DefaultResourceLoader`、工具集、扩展运行器、缓存预热器组装成一个 `AgentSession`。
5. **诊断与报告**：启动警告、资源冲突诊断、崩溃日志、Bug 报告打包 / 脱敏 / 上传、页脚 git 分支。
6. **MCP 配置**：`mcp.json` 的校验与 exposure 语义（协议实现归 `telemetry-mcp`）。

### 1.2 在 monorepo 中的位置

- 依赖 `@earendil-works/kiana-ai`（模型、消息、provider、`getSystemMessageText`、`formatUserAgent`、`uuidv7`）、
  `@earendil-works/kiana-agent-core`（`Agent`、`setDefaultStreamFn`）、
  `@earendil-works/kiana-tui`（终端能力、Markdown、主题类型）、
  `@earendil-works/kiana-mcp` / `@earendil-works/kiana-codemode`（由扩展加载，不在本单元的源文件里）。
- 被 `src/main.ts`（ca-cli）、`src/modes/**`（interactive / print / rpc）、`src/index.ts`（SDK 公共面）
  与 `src/package-manager-cli.ts` 大量使用。
- 第三方依赖中语义关键的有：`ignore`（gitignore 语义）、`minimatch`（glob）、`semver`、
  `hosted-git-info`、`yaml`、`proper-lockfile`、`chalk`、`cross-spawn`、`globSync`（Node 22 `fs.globSync`）。

### 1.3 入口点

| 入口 | 说明 |
|---|---|
| `new DefaultResourceLoader(opts).reload()` | 启动与 `/reload` 的唯一资源入口 |
| `createAgentSession(options)` | SDK 与内置模式共用的会话工厂 |
| `new DefaultPackageManager({cwd, agentDir, settingsManager, builtinExtensions})` | 包解析 / 安装 |
| `handlePackageCommand(args)` / `handleConfigCommand(args)` | `kiana install/remove/update/list/config` |
| `buildSystemPrompt(options)` / `buildSystemPromptSections` / `buildSystemPromptState` / `diffSystemPromptSections` | 系统提示 |
| `validateMcpServerConfig(name, raw)` / `McpServerRegistry` | MCP 配置 |
| `loadSkills(options)` / `loadSkillsFromDir` / `formatSkillsForPrompt` | 技能 |
| `loadPromptTemplates(options)` / `expandPromptTemplate` / `substituteArgs` / `parseCommandArgs` | 模板 |
| `collectBugReportMetadata` / `collectBugReportDiagnostics` / `bugReportFiles` / `writeBugReportArchive` / `generateBugReportSummary` / `uploadBugReport` | Bug 报告 |
| `recordCrash` / `readCrashLog` / `takeUnnotifiedCrash` / `findExtensionStackMatches` / `clearCrashLog` | 崩溃 |
| `new FooterDataProvider(cwd)` | 页脚 git 分支 |
| `findGitPaths` | 被 `resource-loader` 与 `footer-data-provider` 共用 |
| `getExtensionTempFolder` | 临时扩展目录（`agentDir/tmp/extensions`） |

### 1.4 建议移植顺序

1. `source-info.ts`、`diagnostics.ts`、`timings.ts`、`kiana-manifest.ts`、`slash-commands.ts`（无依赖）
2. `skills.ts` → `prompt-templates.ts` → `system-prompt.ts`
3. `package-manager.ts`（依赖前面的 manifest / source-info）
4. `resource-loader.ts`（依赖 package-manager / skills / prompts / themes）
5. `mcp-servers.ts`、`crash-log.ts`、`footer-data-provider.ts`、`bug-report.ts`、`bug-report-upload.ts`
6. `sdk.ts`（依赖几乎全部）
7. `package-manager-cli.ts`（与 ca-cli 协同）

---

## 2. 逐文件规范

### 2.1 `src/core/source-info.ts`（57 行）

```ts
export type SourceScope = "user" | "project" | "temporary";
export type SourceOrigin = "package" | "top-level";

export interface SourceInfo {
  path: string;
  source: string;        // "local" | "auto" | "builtin" | "cli" | "npm:foo" | "git:..." | 扩展自定义
  scope: SourceScope;
  origin: SourceOrigin;
  baseDir?: string;
}

export const BUILTIN_PATH_PREFIX = "builtin:";
export function getSyntheticPathSource(path: string): string | undefined;
export function isSyntheticPath(path: string): boolean;
export function createSourceInfo(path: string, metadata: PathMetadata): SourceInfo;
export function createSyntheticSourceInfo(path, options): SourceInfo;
```

**行为（逐条）**

- `getSyntheticPathSource(path)`：
  - `path` 以 `builtin:` 开头 → `"builtin"`。
  - `path` 以 `<` 开头且以 `>` 结尾 → `path.slice(1, -1).split(":")[0] || "temporary"`。
    例：`<inline:foo>` → `"inline"`；`<inline:1>` → `"inline"`；`<>` → `"temporary"`。
  - 否则 → `undefined`。
- `isSyntheticPath(path)`：`path.startsWith("builtin:") || path.startsWith("<")`。
- `createSourceInfo(path, metadata)`：字段一一对应；`baseDir` 原样透传（可能是 `undefined`）。
- `createSyntheticSourceInfo(path, options)`：`scope` 默认 `"temporary"`，`origin` 默认 `"top-level"`，
  `baseDir` 无默认。

**Rust 设计**：`enum SourceScope { User, Project, Temporary }`、`enum SourceOrigin { Package, TopLevel }`，
`struct SourceInfo { path: PathBuf/String, source: String, scope, origin, base_dir: Option<String> }`。
`path` 用 `String`（可能是合成路径，不能强制为 `PathBuf`）。

---

### 2.2 `src/core/diagnostics.ts`（15 行）

```ts
export interface ResourceCollision {
  resourceType: "extension" | "skill" | "prompt" | "theme";
  name: string;
  winnerPath: string;
  loserPath: string;
  winnerSource?: string;
  loserSource?: string;
}
export interface ResourceDiagnostic {
  type: "warning" | "error" | "collision";
  message: string;
  path?: string;
  collision?: ResourceCollision;
}
```

仅类型，无行为。`message` 是**逐字**用户可见文本（第 3 节汇总）。

---

### 2.3 `src/core/timings.ts`（50 行）

```ts
const ENABLED = process.env.KIANA_TIMING === "1";
type TimingLabel = "main" | "extensions";
export function resetTimings(namespace: TimingLabel = "main"): void;
export function time(label: string, namespace: TimingLabel = "main"): void;
export function printTimings(): void;
```

**行为**

- `ENABLED` 在模块加载时求值一次；为 false 时三个函数全是 no-op（`resetTimings` / `time` / `printTimings` 立即返回）。
- `resetTimings(ns)`：`timingNamespaces.set(ns, { timings: [], lastTime: Date.now() })`。
- `time(label, ns)`：若 ns 不存在则先 `resetTimings(ns)`；push `{ label, ms: now - lastTime }`；更新 `lastTime = now`。
  因此第一个 `time()` 的 `ms` 是「自 reset 以来」的耗时，不是 0。
- `printTimings()`：**按 Map 插入顺序**遍历 namespace，对每个调用 `printTimingGroup`。
- `printTimingGroup(title, timings)`：
  - 过滤 `ms >= 0`；为空则**不输出任何内容**（连标题都不输出）。
  - 全部写 **stderr**：
    ```
    \n--- ${title} ---
      ${label}: ${ms}ms
      …（逐条）
      TOTAL: ${sum}ms
    ${"-".repeat(title.length + 8)}\n
    ```
    注意首行前有 `\n`，最后一行后是换行（`console.error` 每次自带 `\n`，最后一条是 `"-".repeat(...)+"\n"`，
    因此结尾有**两个**换行）。
  - `title` 形如 `Startup Timings: main` / `Startup Timings: extensions`。
- `ca-cli` 的 `main()` 在退出前调用 `printTimings()`；`resource-loader.reload()` 开头调用 `resetTimings("extensions")`；
  `sdk.ts` 在资源加载后调用 `time("resourceLoader.reload")`。

**Rust 设计**：`static ENABLED: LazyLock<bool>`；`Mutex<IndexMap<TimingLabel, TimingNamespace>>`。
时间用 `std::time::Instant` 差值（JS 用 `Date.now()` 毫秒整数；Rust 必须**取整到毫秒**后打印整数）。

---

### 2.4 `src/core/kiana-manifest.ts`（35 行）

```ts
export interface KianaManifest {
  extensions?: string[]; skills?: string[]; prompts?: string[]; themes?: string[];
}
export function readKianaManifest(packageJsonPath: string): KianaManifest | null;
```

**行为**

1. `readFileSync(utf-8)` → `stripBom` → `JSON.parse`。任何异常（读失败、语法错误）→ `null`。
   **注意**：语法错误会向上传播吗？不会——整个函数体在 `try` 内，`catch` 返回 `null`。
   但在 `resource-loader.collectExtensionPackageWarnings` 中，同一文件被独立解析且**不 catch**，
   因此非法 JSON 会让 `reload()` 抛 `SyntaxError`（见 §7 的
   `should fail when an extension package manifest cannot be parsed`）。
2. `pkg` 不是对象，或 `pkg.kiana` 不是对象（含数组、`null`）→ `null`。
3. 对四个字段（**固定顺序** `extensions, skills, prompts, themes`）：
   值为数组且**每一项都是 string** 时写入 `manifest[field]`；否则该字段**不出现**（不是空数组）。
4. 返回的 manifest 可能为空对象 `{}`（`kiana` 存在但无有效字段）。调用方据此判断「有 manifest」。
   这与「没有 `kiana` 键」返回 `null` 语义不同：`collectPackageResources` 中 `if (manifest)` 分支
   会把「空 manifest」视为「已声明但全空」，不落到目录约定发现。

**Rust**：`#[derive(Deserialize)]` + `serde_json::Value` 手写检查，确保 `kiana: []` / `kiana: null` → `None`；
`serde` 的 `deny_unknown_fields` 不可用（要忽略未知字段）。

---

### 2.5 `src/core/slash-commands.ts`（44 行）

```ts
export type SlashCommandSource = "extension" | "prompt" | "skill";
export interface SlashCommandInfo { name: string; description?: string; source: SlashCommandSource; sourceInfo: SourceInfo; }
export interface BuiltinSlashCommand { name: string; description: string; argumentHint?: string; }
export const BUILTIN_SLASH_COMMANDS: ReadonlyArray<BuiltinSlashCommand>;
```

**【逐字】完整 24 项列表（顺序即 UI 顺序）**：

| # | name | description | argumentHint |
|---|---|---|---|
| 1 | `settings` | `Open settings menu` | — |
| 2 | `model` | `Select model (opens selector UI)` | `<provider/model>` |
| 3 | `tree` | `Navigate session tree (switch branches)` | — |
| 4 | `thinking` | `Set thinking level` | `<level>` |
| 5 | `scoped-models` | `Enable/disable models for Ctrl+P cycling` | — |
| 6 | `export` | `Export session (HTML default, or specify path: .html/.jsonl)` | — |
| 7 | `import` | `Import and resume a session from a JSONL file` | — |
| 8 | `share` | `Share session via a Radius artifact or a GitHub gist` | — |
| 9 | `bug` | `Report a bug to the Kiana developers` | `<description>` |
| 10 | `copy` | `Copy last agent message to clipboard` | — |
| 11 | `name` | `Set session display name` | — |
| 12 | `session` | `Show session info and stats` | — |
| 13 | `changelog` | `Show changelog entries` | — |
| 14 | `hotkeys` | `Show all keyboard shortcuts` | — |
| 15 | `fork` | `Create a new fork from a previous user message` | — |
| 16 | `clone` | `Duplicate the current session at the current position` | — |
| 17 | `trust` | `Save project trust decision for future sessions` | — |
| 18 | `login` | `Configure provider authentication` | `<provider>` |
| 19 | `logout` | `Remove provider authentication` | — |
| 20 | `new` | `Start a new session` | — |
| 21 | `compact` | `Manually compact the session context` | — |
| 22 | `resume` | `Resume a different session` | — |
| 23 | `reload` | `Reload keybindings, extensions, skills, prompts, themes, and context files` | — |
| 24 | `quit` | `Quit ${APP_NAME}`（运行期拼接，默认 `Quit kiana`） | — |

**文档差异（必须记录，不要「修正」实现）**：`docs/slash-commands.md` 列出的 `/llama` 与 `/mcp` **不在**
`BUILTIN_SLASH_COMMANDS` 中。它们由扩展（`mcp` 内置扩展 / llama 扩展）注册。移植时不得把它们加进本表。

**Rust**：`pub static BUILTIN_SLASH_COMMANDS: &[BuiltinSlashCommand]`，`quit` 的 description 用 `format!`
在首次访问时生成（`LazyLock<Vec<BuiltinSlashCommand>>`）或直接写死 `"Quit kiana"`（`APP_NAME` 可由
`package.json.kianaConfig.name` 覆盖，故必须运行期生成）。

---

### 2.6 `src/core/skills.ts`（514 行）

#### 2.6.1 类型

```ts
export interface SkillFrontmatter { name?: string; description?: string; "disable-model-invocation"?: boolean; [key: string]: unknown; }
export interface Skill {
  name: string; description: string; filePath: string; baseDir: string;
  sourceInfo: SourceInfo; disableModelInvocation: boolean;
}
export interface LoadSkillsResult { skills: Skill[]; diagnostics: ResourceDiagnostic[]; }
export interface LoadSkillsFromDirOptions { dir: string; source: string; }
export interface LoadSkillsOptions { cwd: string; agentDir: string; skillPaths: string[]; includeDefaults: boolean; }
```

常量：`MAX_NAME_LENGTH = 64`、`MAX_DESCRIPTION_LENGTH = 1024`、
`IGNORE_FILE_NAMES = [".gitignore", ".ignore", ".fdignore"]`。

#### 2.6.2 ignore 相关内部函数（与 `package-manager.ts` 中同名函数**逐字相同**）

- `toPosixPath(p)`：`p.split(sep).join("/")`（Windows 反斜杠 → `/`）。
- `prefixIgnorePattern(line, prefix): string | null`：
  1. `trimmed = line.trim()`；空 → `null`。
  2. `trimmed` 以 `#` 开头且不以 `\#` 开头 → `null`（注释）。
  3. `pattern = line`（**原始未 trim 行**）；`negated = false`。
  4. `pattern` 以 `!` 开头 → `negated = true; pattern = pattern.slice(1)`；
     否则以 `\!` 开头 → `pattern = pattern.slice(1)`（转义，保持非否定）。
  5. `pattern` 以 `/` 开头 → 去掉该 `/`（锚定为相对 root）。
  6. `prefixed = prefix ? prefix + pattern : pattern`；`negated ? "!" + prefixed : prefixed`。
- `addIgnoreRules(ig, dir, rootDir)`：
  - `relativeDir = relative(rootDir, dir)`；`prefix = relativeDir ? toPosixPath(relativeDir) + "/" : ""`。
  - 对三个文件名依次：文件存在则读 UTF-8，按 `/\r?\n/` 切分、映射 `prefixIgnorePattern`、过滤空值，
    非空才 `ig.add(patterns)`。读失败静默。

#### 2.6.3 `loadSkillsFromDir(options)` → `loadSkillsFromDirInternal(dir, source, true)`

`loadSkillsFromDirInternal(dir, source, includeRootFiles, ignoreMatcher?, rootDir?)`：

1. `dir` 不存在 → `{skills: [], diagnostics: []}`。
2. `root = rootDir ?? dir`；`ig = ignoreMatcher ?? ignore()`；`addIgnoreRules(ig, dir, root)`。
3. `readdirSync(dir, {withFileTypes:true})`（失败则 catch 后返回已积累结果）。
4. **第一遍**：找名为 `SKILL.md` 的目录项：
   - `isFile = entry.isFile()`；符号链接则 `statSync(fullPath).isFile()`（失败 `continue`）。
   - `relPath = toPosixPath(relative(root, fullPath))`；`!isFile || ig.ignores(relPath)` → `continue`。
   - 加载 `loadSkillFromFile(fullPath, source)`；push skill（若非 null）与 diagnostics；
     **立即 return**（不再看其它 `SKILL.md`，也不递归）。
     注意 `continue` 只跳过**这一个** `SKILL.md` 条目（例如它是目录或被 ignore），继续循环找下一个。
5. **第二遍**：遍历所有条目：
   - 名字以 `.` 开头 → skip；`node_modules` → skip。
   - 符号链接解析真实类型（`statSync`，失败 `continue`）。
   - `relPath`；`ignorePath = isDirectory ? relPath + "/" : relPath`；`ig.ignores(ignorePath)` → `continue`。
   - 目录 → 递归 `loadSkillsFromDirInternal(fullPath, source, false, ig, root)`，合并。
   - 文件：仅当 `isFile && includeRootFiles && entry.name.endsWith(".md")` 才加载（根目录的散装 `.md`）。
     子目录中的散装 `.md` **不**加载（只有 `SKILL.md` 才算技能根）。
6. 返回累计结果。

**顺序**：`SKILL.md` 根优先；根目录散装 `.md` 与子目录递归按 `readdirSync` 顺序（Node 返回目录序，
通常非字典序；Rust 若用 `read_dir` 需自行决定，**不得排序**，但需要与 Node 在测试夹具上的行为等价——
测试夹具均只有一个候选，故实践上稳定）。

#### 2.6.4 `loadSkillFromFile(filePath, source)`

返回 `{ skill: Skill | null; diagnostics }`。

1. `isDeclaredSkill = basename(filePath) === "SKILL.md"`。
2. 读取 UTF-8；失败 → 诊断 `{type:"warning", message: error.message, path}`（`error.message` 原文；
   非 Error 时用 `failed to read skill file`），返回 `{null}`。
3. `parseFrontmatter<SkillFrontmatter>(rawContent)`；抛错时：
   - `isDeclaredSkill` → 诊断 `{type:"warning", message: error.message, path}`（`yaml` 的 message，例如
     `line 1, column 14` 之类，见 §7 skills 测试）；非声明技能（散装 `.md`）→ **无诊断**。
   - 返回 `{null}`。
4. `description = frontmatter.description`；`hasDescription = typeof description === "string" && description.trim() !== ""`。
   `!isDeclaredSkill && !hasDescription` → `{null}`（散装 `.md` 无 description 视为普通文档，不报错）。
5. `skillDir = dirname(filePath)`；`parentDirName = basename(skillDir)`。
6. 校验 description：`typeof description !== "string" || description.trim() === ""` →
   `description is required`；否则 `description.length > 1024` → `description exceeds 1024 characters (${length})`。
   每条都 push `{type:"warning", message, path}`。
7. `frontmatterName = typeof frontmatter.name === "string" ? frontmatter.name : undefined`；
   `name = frontmatterName || parentDirName`（空字符串回退到目录名）。
8. 校验 name（四条独立检查，可同时命中，**顺序固定**）：
   - `name.length > 64` → `name exceeds 64 characters (${name.length})`
   - `!/^[a-z0-9-]+$/.test(name)` → `name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)`
   - `name.startsWith("-") || name.endsWith("-")` → `name must not start or end with a hyphen`
   - `name.includes("--")` → `name must not contain consecutive hyphens`
   每条 push warning。
   **注意**：长度按 JS UTF-16 code unit 计（`name.length`）。Rust 必须用 `encode_utf16().count()`。
9. `!hasDescription` → `{null}`（此时已产生过 `description is required` 警告）。
   **即**：有 description 但 name 非法 → 仍然加载技能（只警告）。
10. 返回 skill：
    ```ts
    { name, description, filePath, baseDir: skillDir,
      sourceInfo: createSkillSourceInfo(filePath, skillDir, source),
      disableModelInvocation: frontmatter["disable-model-invocation"] === true }
    ```
    `disable-model-invocation` 必须是 YAML 布尔 `true`；字符串 `"true"` 不算。

`createSkillSourceInfo(filePath, baseDir, source)`：

| source | 结果 |
|---|---|
| `"user"` | `createSyntheticSourceInfo(filePath, {source:"local", scope:"user", baseDir})` |
| `"project"` | `{source:"local", scope:"project", baseDir}` |
| `"path"` | `{source:"local", baseDir}`（scope 默认 `temporary`） |
| 其它 | `{source, baseDir}`（scope 默认 `temporary`） |

#### 2.6.5 `formatSkillsForPrompt(skills, fileReadTool = "read")`

```ts
export function formatSkillsForPrompt(skills: Skill[], fileReadTool: "read" | "bash" | "indirect" = "read"): string
```

1. `visibleSkills = skills.filter(s => !s.disableModelInvocation)`；为空 → `""`。
2. `lines` 初始为 **【逐字】**：
   ```
   ""                                            ← 第 0 个元素是空串，故最终以 "\n\n" 开头
   ""
   "The following skills provide specialized instructions for specific tasks."
   <第二行，取决于 fileReadTool>
   "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands."
   ""
   "<available_skills>"
   ```
   注意：数组首元素是 `"\n\nThe following skills..."` —— 实际源码是
   ```ts
   const lines = [
     "\n\nThe following skills provide specialized instructions for specific tasks.",
     fileReadTool === "read" ? "Use the read tool to load a skill's file when the task matches its description."
       : fileReadTool === "bash" ? "Use bash to load a skill's file when the task matches its description."
       : "Load a skill's file when the task matches its description.",
     "When a skill file references a relative path, …",
     "",
     "<available_skills>",
   ];
   ```
   即**只有一个**以 `\n\n` 开头的首元素，不是两个空串。最终 `join("\n")` 后返回值以 `\n\n` 起头。
3. 每个可见技能 push：
   ```
     <skill>
       <name>${escapeXml(name)}</name>
       <description>${escapeXml(description)}</description>
       <location>${escapeXml(filePath)}</location>
     </skill>
   ```
   缩进为 2、4、4、4、2 空格。
4. push `"</available_skills>"`，`join("\n")`。
5. `escapeXml` 顺序：`&` → `&amp;`，`<` → `&lt;`，`>` → `&gt;`，`"` → `&quot;`，`'` → `&apos;`
   （必须先 `&`，否则二次转义）。

#### 2.6.6 `loadSkills(options)`

1. `resolvedCwd = resolvePath(options.cwd)`；
   `resolvedAgentDir = resolvePath(agentDir ?? getAgentDir())`（`agentDir` 在类型上必填，`??` 是历史遗留）。
2. 状态：`skillMap: Map<name, Skill>`、`realPathSet: Set<string>`、`allDiagnostics: []`、`collisionDiagnostics: []`。
3. `addSkills(result)`：
   - 先把 `result.diagnostics` 全部 push 到 `allDiagnostics`。
   - 对每个 skill：`realPath = canonicalizePath(skill.filePath)`；
     - 已在 `realPathSet` → 静默跳过（同一文件经符号链接重复）。
     - `skillMap` 已有同名 → push collision 诊断：
       ```ts
       { type: "collision", message: `name "${skill.name}" collision`, path: skill.filePath,
         collision: { resourceType: "skill", name: skill.name,
                      winnerPath: existing.filePath, loserPath: skill.filePath } }
       ```
       **注意**：loser 的 realPath **不**加入 `realPathSet`，因此同一文件可对多个名字产生碰撞。
     - 否则 `skillMap.set(name, skill)` 且 `realPathSet.add(realPath)`。
4. `includeDefaults` 时先加载：
   - `join(resolvedAgentDir, "skills")`，source `"user"`，`includeRootFiles = true`
   - `resolve(resolvedCwd, CONFIG_DIR_NAME, "skills")`，source `"project"`
5. `isUnderPath(target, root)`：`target === resolve(root)` 或 `target.startsWith(resolve(root) + sep)`。
6. `getSource(resolvedPath)`：当 `!includeDefaults` 时，若在 userSkillsDir 下 → `"user"`；
   在 projectSkillsDir 下 → `"project"`；否则 `"path"`。`includeDefaults` 时显式路径一律 `"path"`。
7. 遍历 `skillPaths`：
   - `resolvedPath = resolvePath(rawPath, resolvedCwd, {trim:true})`。
   - 不存在 → 诊断 **【逐字】** `{type:"warning", message:"skill path does not exist", path: resolvedPath}`。
   - `statSync`：
     - 目录 → `addSkills(loadSkillsFromDirInternal(resolvedPath, source, true))`。
     - 文件且以 `.md` 结尾 → `loadSkillFromFile`；有 skill 则 `addSkills({skills:[skill], diagnostics})`，
       否则 diagnostics 直接进 `allDiagnostics`。
     - 其它（存在但不是 `.md` 文件）→ `{type:"warning", message:"skill path is not a markdown file", path}`。
   - `statSync` 抛错 → `{type:"warning", message: error.message ?? "failed to read skill path", path}`。
8. 返回 `{ skills: [...skillMap.values()], diagnostics: [...allDiagnostics, ...collisionDiagnostics] }`
   （**碰撞诊断永远排在最后**，顺序为发现顺序）。

**测试约定**：`~` 展开由 `resolvePath` 完成（`expandTilde` 默认 true）。

**Rust 设计**：

```rust
pub struct Skill { pub name: String, pub description: String, pub file_path: String,
                   pub base_dir: String, pub source_info: SourceInfo, pub disable_model_invocation: bool }
pub fn load_skills_from_dir(dir: &str, source: &str) -> LoadSkillsResult
pub fn load_skills(opts: LoadSkillsOptions) -> LoadSkillsResult
pub fn format_skills_for_prompt(skills: &[Skill], reader: SkillReader) -> String
pub enum SkillReader { Read, Bash, Indirect }
```

---

### 2.7 `src/core/prompt-templates.ts`（325 行）

#### 2.7.1 类型

```ts
export interface PromptTemplate {
  name: string; description: string; argumentHint?: string;
  content: string; sourceInfo: SourceInfo; filePath: string;
}
export interface LoadPromptTemplatesOptions { cwd: string; agentDir: string; promptPaths: string[]; includeDefaults: boolean; }
export interface LoadPromptTemplatesResult { templates: PromptTemplate[]; diagnostics: ResourceDiagnostic[]; }
```

#### 2.7.2 `parseCommandArgs(argsString): string[]`

bash 风格引号解析，**不支持反斜杠转义**。

```
args = []; current = ""; hasToken = false; inQuote = null
for char of argsString:                  // 按 UTF-16 code unit 迭代（JS 字符串索引）
  if inQuote:
    if char === inQuote: inQuote = null
    else: current += char
  else if char === '"' || char === "'":
    inQuote = char; hasToken = true
  else if /\s/.test(char):                // 所有 JS \s：空格、\t、\n、\r、\f、\v、NBSP、全角空格等
    if hasToken: args.push(current); current = ""; hasToken = false
  else: current += char; hasToken = true
if hasToken: args.push(current)
```

要点：
- 引号内的引号字符不进入 `current`；不配对的引号延续到结尾。
- `""` / `''` 产生**一个空字符串参数**（`hasToken` 机制）。
- 引号可以出现在 token 中间（`ab"c d"e` → `abc de`）。
- 未加引号的换行按空白分隔（测试 `should treat unquoted newlines as separators`）。
- 反斜杠是普通字符（测试 `should handle escaped quotes inside quoted strings` 断言
  `parseCommandArgs('"a \\"b\\""')` 的行为由不处理转义决定）。

#### 2.7.3 `substituteArgs(content, args): string`

正则（**逐字**，`g` 标志，单次 `replace`，不递归）：

```js
/\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)/g
```

回调参数：`(_match, defaultTarget, defaultValue, sliceStart, sliceLength, simple)`。

- 分支 1（`${X:-default}`）：
  - `X === "@" || X === "ARGUMENTS"` → `value = args.join(" ")`；
  - 否则 `value = args[parseInt(X,10) - 1]`（可能 `undefined`）。
  - 返回 `value ? value : defaultValue`（**falsy 即用默认值**，包括空串与 undefined；
    注意 `"0"` 是 truthy）。
- 分支 2（`${@:N}` / `${@:N:L}`）：
  - `start = parseInt(N,10) - 1`；`start < 0 → start = 0`（`${@:0}` 等价 `${@:1}`）。
  - 有 `L` → `args.slice(start, start + parseInt(L,10)).join(" ")`
  - 无 `L` → `args.slice(start).join(" ")`
- 分支 3（`$ARGUMENTS` / `$@`）→ `args.join(" ")`。
- 分支 4（`$N`）→ `args[parseInt(N,10) - 1] ?? ""`。
- **替换结果不再参与替换**（`String.replace` 单趟）。
- `$0` → `args[-1] ?? ""` → `""`。
- `$1.5` → 匹配 `$1`，`.5` 原样保留。
- `\$1` → 反斜杠保留 + `$1` 被替换（无转义语义）。
- 正则优先级：`${...:-...}` > `${@:...}` > `$ARGUMENTS|$@|$N`。`${@:1}` 不会命中分支 1
  （需要 `:-`），也不会命中 `$@`（前面有 `${`）。
- 大小写敏感（`$arguments` 不匹配）。

#### 2.7.4 `loadTemplateFromFile(filePath, sourceInfo)`

1. 读取失败 → warning（`error.message` 或 `failed to read prompt template file`），`{template:null}`。
2. `parseFrontmatter` 抛错 → warning，`{template:null}`。**注意诊断 message 的实际取值**：
   ```ts
   const message = error instanceof Error ? error.message : "failed to parse prompt template file";
   ```
   即**通常**是 `yaml` 包的原始错误消息（如 `Nested mappings are not allowed in compact mappings at line …`），
   只有在抛出非 `Error` 时才用字面量 `failed to parse prompt template file`。旧版本把它写成「总是该字面量」，
   会让 `[逐字]` 断言写错（同理见 §2.6.4 技能的 `failed to parse skill file`、§2.7.4 第 1 步的
   `failed to read prompt template file`、§2.6.4 第 2 步的 `failed to read skill file`——都是**非 Error 兜底**）。
   注意：`parseFrontmatter` 只在 YAML 语法错误时抛；无 frontmatter 返回 `{}` + 全文 body。
3. `name = basename(filePath).replace(/\.md$/, "")`（**只去尾部 `.md`**，大小写敏感）。
4. `description = typeof frontmatter.description === "string" ? frontmatter.description : ""`；
   若 falsy（空串也算）→ `firstLine = body.split("\n").find(line => line.trim())`；
   存在则 `description = firstLine.slice(0, 60)`，若 `firstLine.length > 60` 再追加 `"..."`。
   **注意**：`slice(0,60)` 是 UTF-16 code unit 切片，可能切开代理对；Rust 必须按 UTF-16 码元截断。
5. `argumentHint = typeof frontmatter["argument-hint"] === "string" ? … : undefined`；
   空串会被保留吗？——`argumentHint && {argumentHint}` 在对象字面量里展开，**空串不写入字段**
   （测试 `should ignore empty argument-hint` 断言 `argumentHint === undefined`）。
6. 返回 `{name, description, ...(argumentHint && {argumentHint}), content: body, sourceInfo, filePath}`。

`parseFrontmatter` 语义（`src/utils/frontmatter.ts`，属 ca-cli）：

- 先 `stripBom`，再把 `\r\n`、`\r` 归一为 `\n`。
- 不以 `---` 开头 → `{frontmatter:{}, body: normalized}`（**body 已去 BOM/归一换行**）。
- 找 `normalized.indexOf("\n---", 3)`；找不到 → 同上。
- `yamlString = normalized.slice(4, endIndex)`（跳过开头 `---\n`）；
  `body = normalized.slice(endIndex + 4).trim()`（`\n---` 之后可能是 `\n`，`slice(endIndex+4)` 跳过它；
  然后 **trim 两端空白**）。
- `parse(yamlString)`（`yaml` 包的 `parse`）；`null` → `{}`。

#### 2.7.5 `loadTemplatesFromDir(dir, getSourceInfo)`

- 目录不存在 → 空。
- **非递归**，遍历直接子项；符号链接解析 `statSync`（失败 `continue`）。
- `isFile && name.endsWith(".md")` 才加载。
- `readdirSync` 抛错 → 返回已积累结果（catch 整个循环）。

#### 2.7.6 `loadPromptTemplates(options)`

1. `resolvedCwd`、`resolvedAgentDir`、`promptPaths`、`includeDefaults`。
2. `globalPromptsDir = join(resolvedAgentDir, "prompts")`；
   `projectPromptsDir = resolve(resolvedCwd, CONFIG_DIR_NAME, "prompts")`。
3. `getSourceInfo(resolvedPath)`：
   - 在 `globalPromptsDir` 下 → `{source:"local", scope:"user", baseDir: globalPromptsDir}`；
   - 在 `projectPromptsDir` 下 → `{source:"local", scope:"project", baseDir: projectPromptsDir}`；
   - 否则 → `{source:"local", baseDir: statSync(p).isDirectory() ? p : dirname(p)}`（scope 默认 `temporary`）。
4. `includeDefaults` → 先 global 后 project 各 `loadTemplatesFromDir`。
5. `promptPaths`：不存在 → **静默跳过**（不产生诊断！与 skills 不同）；
   目录 → 递归？**不**，只一层；`.md` 文件 → 加载；其它忽略；
   `statSync` 抛错 → warning（`error.message` 或 `failed to read prompt template path`）。
6. 返回，顺序：defaults（global、project）→ 显式路径顺序。

#### 2.7.7 `expandPromptTemplate(text, templates)`

```ts
if (!text.startsWith("/")) return text;
const match = text.match(/^\/([^\s]+)(?:\s+([\s\S]*))?$/);
if (!match) return text;
const templateName = match[1]; const argsString = match[2] ?? "";
const template = templates.find(t => t.name === templateName);
if (!template) return text;
return substituteArgs(template.content, parseCommandArgs(argsString));
```

- 名字不能含空白；`/` 后必须至少一个非空白字符。
- `argsString` 可以是多行（`[\s\S]*`），首个空白分隔符被吃掉。
- 找不到模板 → 原样返回（调用方继续按普通斜杠命令处理）。
- 同名模板取**第一个**。

**测试要覆盖的边界**（`test/prompt-templates.test.ts`，**94 个用例**，按 describe 分布如下）：

| describe | 数量 | 覆盖点 |
|---|---|---|
| `substituteArgs` | 37 | 非递归、空参数、越界、`$0`、小数（`$1.5` 只替换 `$1`）、Unicode、换行/tab、连续 `$` 模式、`$ARGUMENTS` 作为词的一部分（`x$ARGUMENTSy`）、大小写敏感、超长参数列表 |
| `substituteArgs - positional defaults` | 10 | 缺失/存在/空串/越界、默认值含空格、默认值不递归 |
| `substituteArgs - array slicing` | 19 | `${@:N}`、`${@:N:L}`、越界、长度 0、长度超长、`${@:0}` 视为全部、与 `$@` 混用、引号/特殊字符/Unicode 参数、多个切片、无空格切片 |
| `parseCommandArgs` | 16 | 空串、tab、引号内换行、引号内转义引号、首尾空格、空白折叠、引号内空串保留 |
| `expandPromptTemplate` | 3 | 空引号参数、按未加引号换行切分、模板名与参数间换行 |
| `parseCommandArgs + substituteArgs` | 3 | 两个函数串联的集成用例 |
| `loadPromptTemplates - argument-hint` | 5 | `argument-hint` 取 frontmatter、空串丢弃、非字符串忽略 |
| `loadPromptTemplates - diagnostics` | 1 | 坏的 YAML frontmatter 报诊断且不影响兄弟文件 |

**Rust 设计**：

```rust
pub struct PromptTemplate { pub name: String, pub description: String, pub argument_hint: Option<String>,
                            pub content: String, pub source_info: SourceInfo, pub file_path: String }
pub fn parse_command_args(s: &str) -> Vec<String>
pub fn substitute_args(content: &str, args: &[String]) -> String
pub fn load_prompt_templates(o: LoadPromptTemplatesOptions) -> LoadPromptTemplatesResult
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String
```

`substitute_args` 不要用 `regex` crate 的 `$name` 组名（会被 `$` 语法干扰），应手写扫描器保持与
JS 正则完全一致的回溯行为；若用 `regex`，需用 `regex::escape` 无关的**命名捕获**并注意
`${d+}` 与 `${@:N:L}` 的优先级（regex crate 左优先选择与 JS 相同）。

---

### 2.8 `src/core/system-prompt.ts`（229 行）

#### 2.8.1 类型

```ts
export interface BuildSystemPromptOptions {
  customPrompt?: string;
  forceSystemPrompt?: string;
  selectedTools?: string[];       // 默认 ["read","bash","edit","write"]
  hiddenTools?: string[];
  toolSnippets?: Record<string, string>;
  toolGuidelines?: Record<string, string[]>;
  promptGuidelines?: string[];
  appendSystemPrompt?: string;
  sections?: Record<string, string>;   // 自定义 XML section
  cwd: string;
  contextFiles?: Array<{ path: string; content: string }>;
  skills?: Skill[];
}
export type NormalizedBuildSystemPromptOptions = BuildSystemPromptOptions & {
  selectedTools: string[]; hiddenTools: string[]; toolSnippets: Record<string,string>;
  toolGuidelines: Record<string,string[]>; promptGuidelines: string[]; appendSystemPrompt: string;
  sections: Record<string,string>; contextFiles: Array<{path:string;content:string}>; skills: Skill[];
};
export type SystemPromptSections = Record<string, string>;
const SYSTEM_PROMPT_SECTION_NAME = /^[a-z][a-z0-9_-]*$/;
```

#### 2.8.2 `normalizeBuildSystemPromptOptions(input)`

深拷贝可变集合：`selectedTools`（默认 `["read","bash","edit","write"]`）、`hiddenTools`（默认 `[]`）、
`toolSnippets` 浅拷贝、`toolGuidelines` 每项数组拷贝、`promptGuidelines`、`appendSystemPrompt`（默认 `""`）、
`sections` 浅拷贝、`contextFiles` 每项浅拷贝、`skills` 每项浅拷贝。其余字段原样透传。

#### 2.8.3 `renderProjectContext(contextFiles)`（内部）

```
"Project-specific instructions and guidelines:"
+ 每个文件：`\n\n<project_instructions path="${path}">\n${content}\n</project_instructions>`
```
（`join("\n\n")`，故首行与第一个标签之间是空行；path 不转义。）

#### 2.8.4 `buildRules(selectedTools, toolGuidelines, promptGuidelines)`（内部）

1. `rules: []`、`seen: Set<string>`；`addRule(rule)`：`normalized = rule.trim()`；
   空或已见则丢弃；否则 push。
2. `hasBash / hasPowerShell / hasGrep / hasFind / hasLs`。
3. 若 `(hasBash || hasPowerShell) && !hasGrep && !hasFind && !hasLs`：
   - 两者都有 → `Use bash or PowerShell for file operations like listing, searching, and finding files`
   - 只有 PowerShell → `Use PowerShell for file operations like listing, searching, and finding files`
   - 否则（只有 bash）→ `Use bash for file operations like ls, rg, find`
4. 按 `selectedTools` **顺序**，对每个工具把 `toolGuidelines[name]` 逐条 `addRule`。
5. `promptGuidelines` 逐条 `addRule`。
6. `addRule("Be concise in your responses")`、`addRule("Show file paths clearly when working with files")`。
7. 返回 `rules.map(r => "- " + r).join("\n")`。

**注意**：去重是全局的（同一文本只保留首次出现的位置）。

#### 2.8.5 `buildSystemPromptSections(input)` → `SystemPromptSections`

1. `options = normalize(...)`；解构。
2. **校验自定义 section 名**：对 `Object.keys(customSections)` 每个名字，
   若不匹配 `/^[a-z][a-z0-9_-]*$/` 或等于 `"preamble"` → 抛
   **【逐字】** `Invalid system prompt section name: ${name}`。
   （`Error` 类型，消息即此串。）
3. `declaredTools = selectedTools.filter(n => !hiddenTools.includes(n))`。
4. `promptSections: {}`。
5. 若 `customPrompt` → `promptSections.preamble = customPrompt`。
   否则：
   - `preamble` **【逐字】**：
     ```
     You are an expert coding assistant operating inside kiana, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.
     ```
   - `visibleTools = declaredTools.filter(n => !!toolSnippets[n])`；
     `tools = visibleTools.length ? visibleTools.map(n => "- " + n + ": " + toolSnippets[n]).join("\n") : "(none)"`。
   - `promptSections.tools` **【逐字】**：
     ```
     ${tools}

     In addition to the tools above, you may have access to other custom tools depending on the project.
     ```
     （即 `${tools}\n\nIn addition to …project.`）
   - `promptSections.rules = buildRules(declaredTools, toolGuidelines, promptGuidelines)`。
   - `promptSections.docs` **【逐字】**（注意 `${getReadmePath()}` 等为**运行期绝对路径**）：
     ```
     Kiana documentation (read only when the user asks about kiana itself, its SDK, extensions, themes, skills, or TUI):
     - Main documentation: <getReadmePath()>
     - Additional docs: <getDocsPath()>
     - Examples: <getExamplesPath()> (extensions, custom tools, SDK)
     - When reading kiana docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory
     - When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), kiana packages (docs/packages.md), environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md), codemode scripts and non-LLM models such as classifiers and image models (docs/codemode.md)
     - When working on kiana topics, read the docs and examples, and follow .md cross-references before implementing
     - Always read kiana .md files completely and follow links to related docs (e.g., tui.md for TUI API details)
     ```
6. `appendSystemPrompt` 非空 → `promptSections.addendum = appendSystemPrompt`。
7. `contextFiles.length > 0` → `promptSections.project_context = renderProjectContext(contextFiles)`。
8. 技能：
   ```
   readers = ["read", "bash"]
   skillFileReadTool = readers.find(t => declaredTools.includes(t))
                       ?? (readers.some(t => selectedTools.includes(t)) ? "indirect" : undefined)
   if (skillFileReadTool && skills.length > 0) {
     const skillsPrompt = formatSkillsForPrompt(skills, skillFileReadTool).trim();
     if (skillsPrompt) promptSections.skills = skillsPrompt;
   }
   ```
   即：可见的 reader 优先 `read` 再 `bash`；reader 被 hidden 但仍在 `selectedTools` 中 → `"indirect"`；
   两者都未选中 → 无 skills section。
9. `promptSections.cwd = cwd.replace(/\\/g, "/")`（Windows 反斜杠全部换成 `/`）。
10. 自定义 sections：仅当 `content` 为 **truthy**（非空串）才写入 `promptSections[name]`。
11. 组装返回值：
    ```ts
    const sections = { preamble: promptSections.preamble };
    for (const [name, content] of Object.entries(promptSections)) {
      if (name !== "preamble") sections[name] = `<${name}>\n${content}\n</${name}>`;
    }
    ```
    `preamble` **无标签**且**永远存在**（即使 customPrompt 是空串，preamble 也是 `""`，
    因为 `if (customPrompt)` 对空串走默认分支）。顺序：preamble → tools → rules → docs →
    addendum → project_context → skills → cwd → 自定义（插入顺序）。

#### 2.8.6 `buildSystemPromptState(input)`

```ts
if (input.forceSystemPrompt !== undefined) return { content: input.forceSystemPrompt };  // 无 sections
return { content: "", sections: buildSystemPromptSections(input) };
```
`undefined` 才回退；`null` 会返回 `{content: null}`（类型上不允许，但 JS 运行期如此）——Rust 用
`Option<String>` 判 `Some` 即强制。

#### 2.8.7 `buildSystemPrompt(input)`

```ts
return getSystemMessageText({ role: "system", ...buildSystemPromptState(input), timestamp: 0 });
```
`getSystemMessageText`（`packages/ai/src/utils/text.ts`，属 ai-core）：

```ts
const parts = [contentText(message.content)];
for (const text of Object.values(message.sections ?? {})) if (text !== null) parts.push(text);
return parts.filter(p => p.length > 0).join("\n\n");
```
即：`content`（通常 `""`）+ 各 section 值，**非空**的用 `\n\n` 连接。强制 prompt 时只有 content，
原样返回（测试 `preserves an exact forced prompt without sections` 断言 `=== "exact"`）。

#### 2.8.8 `diffSystemPromptSections(previous, current)`

```ts
const patch = {};
for (const [name, text] of Object.entries(current)) if (previous[name] !== text) patch[name] = text;
for (const name of Object.keys(previous)) if (current[name] === undefined) patch[name] = null;
return Object.keys(patch).length > 0 ? patch : undefined;
```
- 新增与修改 → 新文本；删除 → `null`；无变化 → `undefined`。
- patch 的键顺序：先新增/修改（按 `current` 顺序），再删除（按 `previous` 顺序）。

**Rust 设计**：

```rust
pub struct BuildSystemPromptOptions { custom_prompt: Option<String>, force_system_prompt: Option<String>,
  selected_tools: Option<Vec<String>>, hidden_tools: Option<Vec<String>>,
  tool_snippets: Option<IndexMap<String,String>>, tool_guidelines: Option<IndexMap<String,Vec<String>>>,
  prompt_guidelines: Option<Vec<String>>, append_system_prompt: Option<String>,
  sections: Option<IndexMap<String,String>>, cwd: String,
  context_files: Option<Vec<ContextFile>>, skills: Option<Vec<Skill>> }
pub struct SystemPromptState { pub content: String, pub sections: Option<IndexMap<String,String>> }
pub fn build_system_prompt_sections(o: &BuildSystemPromptOptions) -> Result<IndexMap<String,String>, String>
pub fn build_system_prompt_state(o: &BuildSystemPromptOptions) -> Result<SystemPromptState, String>
pub fn build_system_prompt(o: &BuildSystemPromptOptions) -> Result<String, String>
pub fn diff_system_prompt_sections(prev: &IndexMap<String,Option<String>>, cur: &IndexMap<String,String>)
  -> Option<IndexMap<String,Option<String>>>
```
`IndexMap` 保序；`serde_json` 需 `preserve_order`。`sections` 的值类型用 `Option<String>`（`null` 表示删除）。

---

### 2.9 `src/core/mcp-servers.ts`（346 行）

本文件只做**配置校验与注册表**；连接、OAuth、工具暴露协议由 `telemetry-mcp` 单元实现。

#### 2.9.1 工具名匹配

```ts
function toolPatternRegExp(pattern: string): RegExp {
  const source = pattern.split("*")
    .map(part => part.replace(/[.+?^${}()|[\]\\]/g, "\\$&"))
    .join(".*");
  return new RegExp(`^${source}$`);
}
export function createToolNameMatcher(entries: readonly string[]): (name: string) => boolean;
```
- 转义集合**逐字**：`[.+?^${}()|[\]\\]`（注意不含 `*`，`*` 是通配符；也不含 `-`、`/`、`,`）。
- 转义后以 `.*` 连接各段，锚定 `^…$`。
- `createToolNameMatcher`：精确名进 `HashSet`；含 `*` 的进模式列表；匹配 = 精确命中或任一模式命中。
  **无 `*` 的条目不会被当正则**，即 `.` 等字符按字面量。

#### 2.9.2 常量

```ts
export const LIST_MCP_RESOURCES_TOOL = "list_mcp_resources";
export const LIST_MCP_RESOURCE_TEMPLATES_TOOL = "list_mcp_resource_templates";
export const READ_MCP_RESOURCE_TOOL = "read_mcp_resource";
export function isMcpToolName(name: string): boolean;  // name.startsWith("mcp__") || 三个资源工具之一
```

#### 2.9.3 exposure

```ts
export type McpExposure = "codemode" | "deferred" | "direct" | "hidden";
const MCP_EXPOSURES = ["codemode","deferred","direct","hidden"];
const MCP_EXPOSURE_ALIASES = { "codemode-deferred": "codemode" };
export function getMcpToolExposure(config: McpServerConfig, toolName: string): McpExposure;
```
`getMcpToolExposure`：
1. `overrides = config.toolExposure ?? {}`；`overrides[toolName]` 存在（**精确名**）→ 返回它。
   注意用 `!== undefined` 判断，故值为空串也会返回（但校验会拒绝非法 exposure）。
2. 遍历 `Object.entries(overrides)`（插入顺序），第一个 `pattern.includes("*") && 模式匹配` 的返回。
3. 回退 `config.exposure ?? "codemode"`。

#### 2.9.4 配置类型

```ts
interface McpServerConfigBase {
  exposure?: McpExposure;        // 默认 codemode
  description?: string;
  toolExposure?: Record<string, McpExposure>;
  enabled?: boolean;             // 默认 true
  timeout?: number;              // 秒，默认 60
}
export interface McpStdioServerConfig extends McpServerConfigBase {
  type?: "stdio"; command: string; args?: string[]; env?: Record<string,string>; cwd?: string;
}
export interface McpHttpServerConfig extends McpServerConfigBase {
  type?: "http"; url: string; headers?: Record<string,string>;
  oauth?: McpOAuthConfig; auth?: { provider: string };
}
export interface McpOAuthConfig {
  clientId?: string; clientSecret?: string; callbackPort?: number; callbackUrl?: string;
  scope?: string; clientName?: string; clientRegistration?: "dcr" | "cimd"; authServerMetadataUrl?: string;
}
export type McpServerConfig = McpStdioServerConfig | McpHttpServerConfig;
```

#### 2.9.5 `isLoopbackRedirectUri(value)`

```ts
if (!URL.canParse(value)) return false;
const url = new URL(value);
return url.protocol === "http:" && ["localhost","127.0.0.1","[::1]"].includes(url.hostname)
    && url.search === "" && url.hash === "";
```
- 只接受 `http:`；https 拒绝。
- `url.hostname` 对 IPv6 保留方括号（WHATWG URL 的行为），故比较 `"[::1]"`。
- 查询串/片段必须为空（`?` 或 `#` 存在即失败，即使后面为空？`url.search === ""` 对 `"http://x/?"` 返回
  `""`（WHATWG 会规范化），因此 `?` 单独出现是**通过**的——Rust 需复刻 WHATWG 解析器，建议用 `url` crate）。

#### 2.9.6 `mcpNamespace(server)`

```ts
return `mcp__${server.replace(/-/g, "_")}`;
```
即 `dev-radius` → `mcp__dev_radius`。

#### 2.9.7 `validateMcpServerConfig(name, raw): McpServerConfig | string`

**校验顺序与错误文案（全部逐字）**：

| 步 | 条件 | 返回值（错误串） |
|---|---|---|
| 1 | `!/^[A-Za-z0-9_-]+$/.test(name)` | `invalid server name "${name}" (use letters, digits, "_" and "-")` |
| 2 | `!isRecord(raw)` | `server "${name}" must be an object` |
| 3 | （无错） | `value = resolveExposureAliases(raw)`：`exposure` 与 `toolExposure` 的**每个值**都把 `"codemode-deferred"` 换成 `"codemode"`；其余字段浅拷贝 |
| 4 | `exposure !== undefined && !isExposure(exposure)` | `server "${name}": exposure must be one of "codemode", "deferred", "direct", "hidden"` |
| 5 | `toolExposure !== undefined && !isRecord(toolExposure)` | `server "${name}": toolExposure must map tool names to exposures` |
| 6 | 某个 `toolExposure[tool]` 非法 | `server "${name}": toolExposure "${tool}" must be one of "codemode", "deferred", "direct", "hidden"` |
| 7 | `enabled !== undefined && typeof enabled !== "boolean"` | `server "${name}": enabled must be a boolean` |
| 8 | `description !== undefined && typeof description !== "string"` | `server "${name}": description must be a string` |
| 9 | `timeout !== undefined && (typeof timeout !== "number" || !(timeout > 0))` | `server "${name}": timeout must be a positive number of seconds` |
| 10 | `type === "sse"` | `server "${name}": legacy SSE transport is not supported; use the streamable HTTP URL` |
| 11 | **HTTP 分支**：`typeof value.url === "string"` 且 `type === undefined \|\| type === "http" \|\| type === "streamable-http"` | 见下 |
| 11a | URL 不可解析或协议不是 `http:`/`https:` | `server "${name}": url must be an http or https URL` |
| 11b | `headers !== undefined && !isStringRecord(headers)` | `server "${name}": headers must map names to strings` |
| 11c | `validateOAuth(value.oauth)` 返回错误 | `server "${name}": ${oauthError}` |
| 11d | `auth` 存在且（非对象 / `provider` 非 string / 空） | `server "${name}": auth.provider must be a provider name` |
| 11e | `auth` 存在且 URL 不是 https 且 host 不在 loopback 列表 | `server "${name}": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]` |
| 11f | 否则 | 返回 `value as McpHttpServerConfig`（**注意 `streamable-http` 会原样保留在返回值里**） |
| 12 | **stdio 分支**：`typeof value.command === "string"` 且 `type === undefined \|\| type === "stdio"` | 见下 |
| 12a | `args !== undefined && !(Array.isArray(args) && args.every(a => typeof a === "string"))` | `server "${name}": args must be an array of strings` |
| 12b | `env !== undefined && !isStringRecord(env)` | `server "${name}": env must map names to strings` |
| 12c | `cwd !== undefined && typeof cwd !== "string"` | `server "${name}": cwd must be a string` |
| 12d | 否则 | 返回 `value as McpStdioServerConfig` |
| 13 | 其它 | `server "${name}" needs either "command" (stdio) or "url" (streamable HTTP)` |

`isStringRecord(value)` = `isRecord && Object.values(value).every(v => typeof v === "string")`。

`validateOAuth(value)` 的错误串（不带 `server "${name}": ` 前缀，由调用方加）：

| 条件 | 文案 |
|---|---|
| `value === undefined` | （无错误） |
| 非对象 | `oauth must be an object` |
| `clientId` 非 string | `oauth.clientId must be a string` |
| `clientSecret` 非 string | `oauth.clientSecret must be a string` |
| `callbackPort` 非整数或 <1 或 >65535 | `oauth.callbackPort must be a port number` |
| `callbackUrl` 非 string 或非 loopback http URI | `oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment` |
| `callbackUrl` 带端口且与 `callbackPort` 不同 | `oauth.callbackUrl and oauth.callbackPort name different ports` |
| `scope` 非 string | `oauth.scope must be a string` |
| `clientName` 非非空 string | `oauth.clientName must be a non-empty string` |
| `clientRegistration` 既非 `"dcr"` 也非 `"cimd"` | `oauth.clientRegistration must be "dcr" or "cimd"` |
| `clientRegistration === "cimd"` 且 `clientId` 或 `clientName` 存在 | `oauth.clientRegistration "cimd" cannot be combined with oauth.clientId or oauth.clientName` |
| `cimd` 且 `callbackUrl` 的 host 是 `[::1]` 或 path 不是 `/callback` | `oauth.clientRegistration "cimd" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback` |
| `authServerMetadataUrl` 不可解析或（非 https 且非 loopback http） | `oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]` |

注意：`clientRegistration === "dcr"` 显式值**不做** cimd 检查；`callbackPort` 的整数判断用 `Number.isInteger`
（`1.0` 是整数，`"1"` 不是数字）。

#### 2.9.8 `RegisteredMcpServer` / `McpServerRegistry`

```ts
export interface RegisteredMcpServer { name: string; config: McpServerConfig; extensionPath: string; }
export class McpServerRegistry {
  register(server): void;                 // set(name) 后触发 changeListener
  unregister(name, extensionPath): void;  // 仅当现有项的 extensionPath 相同才删；删除后触发
  get(name): RegisteredMcpServer | undefined;
  list(): RegisteredMcpServer[];          // 注册顺序；每项 config 做 structuredClone（深拷贝）
  setChangeListener(listener | undefined): void;
}
```
- `list()` 返回的是**副本**，`config` 深拷贝（`structuredClone`；Rust 用 `serde_json` 往返或 `Clone`）。
- `register` 同名替换（保留 Map 原插入位置——JS `Map.set` 对已存在键**不改变顺序**）。
- `unregister` 名字不存在或 owner 不同 → 静默无操作（**不**触发 listener）。

**Rust 设计**：

```rust
pub enum McpExposure { Codemode, Deferred, Direct, Hidden }
pub enum McpServerConfig { Stdio(McpStdioServerConfig), Http(McpHttpServerConfig) }
pub struct McpServerRegistry { servers: Mutex<IndexMap<String, RegisteredMcpServer>>, change_listener: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> }
pub fn validate_mcp_server_config(name: &str, raw: &serde_json::Value) -> Result<McpServerConfig, String>
pub fn get_mcp_tool_exposure(config: &McpServerConfig, tool: &str) -> McpExposure
pub fn create_tool_name_matcher(entries: &[String]) -> impl Fn(&str) -> bool
```
`validate_*` 返回 `Result<_, String>`：**错误是值而非异常**，调用方决定是否上报。

---

### 2.10 `src/core/package-manager.ts`（2791 行）

本单元最大的文件。Rust 侧建议拆成 5 个模块：
`pkg/parse.rs`（源解析与 identity）、`pkg/discover.rs`（目录发现与 ignore）、`pkg/patterns.rs`（过滤模式）、
`pkg/install.rs`（npm/git 安装与更新）、`pkg/manager.rs`（`DefaultPackageManager` 编排）。

#### 2.10.1 常量与全局辅助

```ts
const NETWORK_TIMEOUT_MS = 10000;     // 所有 git/npm 网络命令的 timeout
const UPDATE_CHECK_CONCURRENCY = 4;
const GIT_UPDATE_CONCURRENCY = 4;

function isOfflineModeEnabled(): boolean;      // KIANA_OFFLINE: "1" | "true"(ci) | "yes"(ci)
function isExactNpmVersion(v?: string): boolean;  // semver.valid(v) !== null
function getNpmVersionRange(v?: string): string | undefined;  // v ? (semver.validRange(v) ?? undefined) : undefined
function getEnv(): NodeJS.ProcessEnv;
```

`getEnv()` 特殊行为：Linux 且 `Object.keys(process.env).length === 0` 时读 `/proc/self/environ`
（按 `\0` 切分、`=` 前为键），失败回退 `process.env`。**这是 Bun 编译二进制的兼容路径**。
Rust 侧：直接 `std::env::vars()`；若为空且 `/proc/self/environ` 可读则解析之。

`getHomeDir()`：`process.env.HOME || homedir()`。

#### 2.10.2 类型（全部导出）

```ts
export interface PathMetadata { source: string; scope: SourceScope; origin: "package" | "top-level";
                                baseDir?: string; packageRoot?: string; }
export interface ResolvedResource { path: string; enabled: boolean; metadata: PathMetadata; }
export interface ResolvedPaths { extensions: ResolvedResource[]; skills: ResolvedResource[];
                                 prompts: ResolvedResource[]; themes: ResolvedResource[];
                                 installFailures?: PackageInstallFailure[]; }
export interface PackageInstallFailure { source: string; scope: SourceScope; message: string; }
export type MissingSourceAction = "install" | "skip" | "error";
export interface ProgressEvent { type: "start"|"progress"|"complete"|"error";
  action: "install"|"remove"|"update"|"clone"|"pull"; source: string; message?: string; }
export type ProgressCallback = (event: ProgressEvent) => void;
export interface PackageUpdate { source: string; displayName: string; type: "npm"|"git";
                                 scope: Exclude<SourceScope,"temporary">; }
export interface ConfiguredPackage { source: string; scope: "user"|"project"; filtered: boolean; installedPath?: string; }
export interface PackageManager { /* 12 个方法，见下 */ }
export function getExtensionTempFolder(agentDir: string): string;
```

`PackageManager` 接口（**12 个方法**）：

```ts
resolve(onMissing?: (source: string) => Promise<MissingSourceAction>): Promise<ResolvedPaths>;
install(source: string, options?: { local?: boolean }): Promise<void>;
installAndPersist(source: string, options?: { local?: boolean }): Promise<void>;
remove(source: string, options?: { local?: boolean }): Promise<void>;
removeAndPersist(source: string, options?: { local?: boolean }): Promise<boolean>;
update(source?: string): Promise<void>;
listConfiguredPackages(): ConfiguredPackage[];
resolveExtensionSources(sources: string[], options?: { local?: boolean; temporary?: boolean }): Promise<ResolvedPaths>;
addSourceToSettings(source: string, options?: { local?: boolean }): boolean;
removeSourceFromSettings(source: string, options?: { local?: boolean }): boolean;
setProgressCallback(callback: ProgressCallback | undefined): void;
getInstalledPath(source: string, scope: "user" | "project"): string | undefined;
```
（接口声明了 **12** 个方法——`resolve`、`install`、`installAndPersist`、`remove`、`removeAndPersist`、`update`、
`listConfiguredPackages`、`resolveExtensionSources`、`addSourceToSettings`、`removeSourceFromSettings`、
`setProgressCallback`、`getInstalledPath`；`DefaultPackageManager` 还额外公开 `checkForAvailableUpdates()`，
但它**不在**接口里。旧版本此处误写为「11 个」，与 §5.1 的「12 方法」矛盾。）

内部类型：

```ts
type NpmSource = { type:"npm"; spec:string; name:string; version?:string; range?:string; pinned:boolean };
type LocalSource = { type:"local"; path:string };
type ParsedSource = NpmSource | GitSource | LocalSource;
type ResourceType = "extensions" | "skills" | "prompts" | "themes";
const RESOURCE_TYPES: ResourceType[] = ["extensions","skills","prompts","themes"];
const FILE_PATTERNS = { extensions: /\.(ts|js)$/, skills: /\.md$/, prompts: /\.md$/, themes: /\.json$/ };
const IGNORE_FILE_NAMES = [".gitignore", ".ignore", ".fdignore"];
```

#### 2.10.3 `getExtensionTempFolder(agentDir)`

```ts
const tempFolder = join(agentDir, "tmp", "extensions");
mkdirSync(tempFolder, { recursive: true, mode: 0o700 });
chmodSync(tempFolder, 0o700);
return tempFolder;
```
**每次调用都 chmod 0700**（即使已存在）。路径：`<agentDir>/tmp/extensions`。

#### 2.10.4 目录发现函数

`collectFiles(dir, filePattern, skipNodeModules = true, ignoreMatcher?, rootDir?)`

1. `dir` 不存在 → `[]`。
2. `root = rootDir ?? dir`；`ig = ignoreMatcher ?? ignore()`；`addIgnoreRules(ig, dir, root)`（逐字同 §2.6.2）。
3. `readdirSync(withFileTypes)`；对每项：
   - `name` 以 `.` 开头 → skip；`skipNodeModules && name === "node_modules"` → skip。
   - 符号链接 → `statSync` 判类型（失败 `continue`）。
   - `relPath = toPosixPath(relative(root, fullPath))`；`ignorePath = isDir ? relPath + "/" : relPath`；
     `ig.ignores(ignorePath)` → skip。
   - 目录 → 递归（传入同一个 `ig` 与 `root`；递归内部会再次 `addIgnoreRules(ig, subdir, root)`）。
   - 文件且 `filePattern.test(entry.name)` → push `fullPath`。
4. `readdirSync` 抛错 → 返回已积累（catch 整个循环）。

`collectSkillEntries(dir, mode: "kiana"|"agents", ignoreMatcher?, rootDir?)`

1. 不存在 → `[]`。
2. **第一遍**：找 `SKILL.md`：
   - `isFile`（符号链接解析）；`relPath`；`isFile && !ig.ignores(relPath)` → push 并 **立即 return**
     （停止递归，整个子树视为一个技能根）。
3. **第二遍**：遍历：
   - `.` 开头 / `node_modules` → skip。
   - 符号链接解析。
   - `shouldIncludeMarkdownFile = isFile && name.endsWith(".md") && !ig.ignores(relPath) &&
     ((mode === "kiana" && dir === root) || (mode === "agents" && dir !== root))`
     - `kiana` 模式：**只在根目录**收散装 `.md`（子目录里的散装 `.md` 不是技能）。
     - `agents` 模式：**只在非根目录**收散装 `.md`（`.agents/skills` 根下的散装 `.md` 不算）。
   - 目录：`ig.ignores(relPath + "/")` → skip；否则递归。
4. 顺序：先本目录文件，再子目录递归。

`collectAutoSkillEntries(dir, mode)` = `collectSkillEntries(dir, mode)`。
`findGitRepoRoot(startDir)`：从 `resolve(startDir)` 向上找 `join(dir, ".git")` 存在即返回（**`.git` 是文件也算**）；
到根返回 `null`。
`collectAncestorAgentsSkillDirs(startDir)`：
- `resolvedStartDir = resolve(startDir)`；`gitRepoRoot = findGitRepoRoot(resolvedStartDir)`。
- 从 start 向上 push `join(dir, ".agents", "skills")`；当 `gitRepoRoot && dir === gitRepoRoot` 时 **break**
  （包含 repo root 自身那一层）；否则到文件系统根 break。
- 返回顺序：cwd 最近 → 最远。

`collectAutoPromptEntries(dir)`：**非递归**，直接子项；跳过 `.`/`node_modules`；符号链接解析；
`ig.ignores(relPath)` 跳过；`isFile && endsWith(".md")` → push。

`collectAutoThemeEntries(dir)`：同上但 `endsWith(".json")`。

`resolveExtensionEntries(dir): string[] | null`
1. 有 `package.json` → `readKianaManifest`；若 `manifest?.extensions?.length`：
   对每个条目 `resolve(dir, extPath)`，存在则 push；若 push 了至少一个 → 返回（**不再看 index.\***）。
2. `index.ts` 存在 → `[index.ts]`；否则 `index.js` → `[index.js]`；否则 `null`。

`collectAutoExtensionEntries(dir)`：
1. 不存在 → `[]`。
2. 若 `resolveExtensionEntries(dir)` 非 null → 直接返回它（**目录自身的 manifest/index 优先**）。
3. 否则遍历（非递归）：
   - `.`/`node_modules` skip；符号链接解析；`ig.ignores(...)` skip。
   - 文件 `.ts`/`.js` → push。
   - 目录 → `resolveExtensionEntries(subdir)`，非 null 则 push 全部（**只下探一层**，无 index 的子目录被忽略）。

`collectResourceFiles(dir, resourceType)`
- `skills` → `collectSkillEntries(dir, "kiana")`
- `extensions` → `collectAutoExtensionEntries(dir)`
- 其它 → `collectFiles(dir, FILE_PATTERNS[type])`

#### 2.10.5 模式匹配（glob / 覆盖）

`isPattern(s)`：`s` 以 `!` / `+` / `-` 开头，或含 `*` / `?`。
`isOverridePattern(s)`：以 `!` / `+` / `-` 开头。
`hasGlobPattern(s)`：含 `*` 或 `?`。
`getOverridePatterns(entries)`：过滤 `isOverridePattern`。

`expandPackageGlob(pattern, root)`：
```ts
globSync(pattern, { cwd: root })
  .map(m => resolve(root, m))
  .filter(p => relative(root, p).split(sep).every(seg => seg === ".." || !seg.startsWith(".")))
  .sort((a,b) => a < b ? -1 : a > b ? 1 : 0);
```
- 用 Node 22 `fs.globSync`（**不是** `minimatch`）；`*` 默认不匹配 `.` 开头的段（dot 选项默认 false）。
- 过滤：相对路径的每一段要么是 `..`，要么不以 `.` 开头（允许 `..` 出现在结果中？——`globSync` 不产生 `..`，
  故实际作用是排除隐藏路径）。
- 按字符串序升序排序（`<` 比较，UTF-16 码元序）。

`matchesAnyPattern(filePath, patterns, baseDir)`：
```
rel = toPosix(relative(baseDir, filePath))
name = basename(filePath)
filePathPosix = toPosix(filePath)
isSkillFile = name === "SKILL.md"
parentDir = isSkillFile ? dirname(filePath) : undefined
parentRel = isSkillFile ? toPosix(relative(baseDir, parentDir)) : undefined
parentName = isSkillFile ? basename(parentDir) : undefined
parentDirPosix = isSkillFile ? toPosix(parentDir) : undefined

patterns.some(pattern => {
  p = toPosix(normalizePath(pattern))          // 展开 ~、Windows 路径归一
  if (minimatch(rel, p) || minimatch(name, p) || minimatch(filePathPosix, p)) return true
  if (!isSkillFile) return false
  return minimatch(parentRel, p) || minimatch(parentName, p) || minimatch(parentDirPosix, p)
})
```
`minimatch` 默认选项（`dot:false`、`nocase:false`、`nocomment:false`、`nonegate:false`）。
**注意**：`minimatch` 的 `!` 否定语义在这里也会生效（传给 `minimatch` 的 pattern 可能以 `!` 开头，
但调用方已把 `!`/`+`/`-` 剥离）。技能文件额外匹配「所在目录」的三种形式。

`normalizeExactPattern(pattern)`：去掉前缀 `./` 或 `.\`，再 `toPosix(normalizePath(...))`。

`matchesAnyExactPattern(filePath, patterns, baseDir)`：与上面同构，但用 `===` 比较
（`normalized === rel || normalized === filePathPosix`，技能文件再加 `parentRel` / `parentDirPosix`），
**不做 minimatch**。

`isEnabledByOverrides(filePath, patterns, baseDir)`：
```
overrides = getOverridePatterns(patterns)
excludes      = overrides 中以 "!" 开头的（去头）
forceIncludes = overrides 中以 "+" 开头的（去头）
forceExcludes = overrides 中以 "-" 开头的（去头）
enabled = true
if (excludes 非空 && matchesAnyPattern(filePath, excludes, baseDir)) enabled = false
if (forceIncludes 非空 && matchesAnyExactPattern(filePath, forceIncludes, baseDir)) enabled = true
if (forceExcludes 非空 && matchesAnyExactPattern(filePath, forceExcludes, baseDir)) enabled = false
return enabled
```

`applyPatterns(allPaths, patterns, baseDir): Set<string>`（**四步固定顺序**）：
1. 分类：`+` → forceIncludes（去 `+`）、`-` → forceExcludes（去 `-`）、`!` → excludes（去 `!`）、
   其它 → includes。
2. 第 1 步：`includes` 为空 → `result = [...allPaths]`；否则 `result = allPaths.filter(f => matchesAnyPattern(f, includes, baseDir))`。
3. 第 2 步：`excludes` 非空 → `result = result.filter(f => !matchesAnyPattern(f, excludes, baseDir))`。
4. 第 3 步 force-include：对 `allPaths` 中**不在** `result` 且 `matchesAnyExactPattern(f, forceIncludes, baseDir)`
   的 push（顺序为 `allPaths` 顺序）。
5. 第 4 步 force-exclude：`result = result.filter(f => !matchesAnyExactPattern(f, forceExcludes, baseDir))`。
6. `new Set(result)`（保留首次出现顺序）。

`applyAutoloadDisabledPatterns(allPaths, patterns, baseDir): Map<string, boolean>`：
```
result = new Map()
for pattern of patterns:
  target = pattern.slice(以 +|-|! 开头 ? 1 : 0)
  enabled = !pattern.startsWith("-") && !pattern.startsWith("!")
  exact = pattern.startsWith("+") || pattern.startsWith("-")
  for filePath of allPaths:
    if (exact ? matchesAnyExactPattern(filePath,[target],baseDir)
              : matchesAnyPattern(filePath,[target],baseDir)) result.set(filePath, enabled)
return result
```
（后写的模式覆盖先写的；`set` 不改变已有键的顺序。）

#### 2.10.6 优先级排序 `resourcePrecedenceRank(m)`

```ts
if (m.source === "builtin") return 5;
if (m.origin === "package") return 4;
const scopeBase = m.scope === "project" ? 0 : 2;
return scopeBase + (m.source === "local" ? 0 : 1);
```
| rank | 含义 |
|---|---|
| 0 | project + `source:"local"`（settings 显式条目） |
| 1 | project + `source:"auto"`（自动发现） |
| 2 | user + `local` |
| 3 | user + `auto`（含 `cli` 来源的临时项？见下） |
| 4 | package（任意 scope） |
| 5 | builtin |

注意 **`source:"cli"` 且 `scope:"temporary"`** 会落到 `scopeBase = 2`（`temporary` ≠ `project`），
且 `source !== "local"` → **rank 3**。`buildin` 恒为 5。

#### 2.10.7 `DefaultPackageManager` 构造函数与状态

```ts
constructor(options: {
  cwd: string; agentDir: string; settingsManager: SettingsManager;
  builtinExtensions?: string[];
})
```
- `this.cwd = resolvePath(options.cwd)`、`this.agentDir = resolvePath(options.agentDir)`。
- 私有缓存：`globalNpmRoot?: string`、`globalNpmRootCommandKey?: string`（按 npmCommand argv 的 `\0` 连接作 key）。
- `progressCallback?: ProgressCallback`。

`emitProgress(event)`：`this.progressCallback?.(event)`。
`withProgress(action, source, message, op)`：start → op() → complete；抛错时先发
`{type:"error", action, source, message: error.message ?? String(error)}` 再 rethrow。

#### 2.10.8 `resolve(onMissing?)` —— 资源解析主流程

```
accumulator = createAccumulator()
globalSettings = settingsManager.getGlobalSettings()
projectSettings = settingsManager.getProjectSettings()

// 1. 收集包（project 先，user 后）
allPackages = [...projectSettings.packages.map(p => ({pkg:p, scope:"project"})),
               ...globalSettings.packages.map(p => ({pkg:p, scope:"user"}))]
packageSources = dedupePackages(allPackages)
installFailures = await resolvePackageSources(packageSources, accumulator, onMissing)

// 2. 顶层 settings 资源（每种类型：project 先、user 后）
globalBaseDir = agentDir
projectBaseDir = join(cwd, CONFIG_DIR_NAME)
for (type of RESOURCE_TYPES) {
  target = getTargetMap(accumulator, type)
  resolveLocalEntries(projectSettings[type] ?? [], type, target,
                      {source:"local", scope:"project", origin:"top-level"}, projectBaseDir)
  resolveLocalEntries(globalSettings[type] ?? [], type, target,
                      {source:"local", scope:"user", origin:"top-level"}, globalBaseDir)
}

// 3. 自动发现
addAutoDiscoveredResources(accumulator, globalSettings, projectSettings, globalBaseDir, projectBaseDir)

// 4. 内置扩展
for (name of builtinExtensions) {
  path = "builtin:" + name
  projectEnabled = applyAutoloadDisabledPatterns([path], getOverridePatterns(projectSettings.extensions ?? []), projectBaseDir).get(path)
  addResource(accumulator.extensions, path,
              {source:"builtin", scope: projectEnabled === undefined ? "user" : "project", origin:"top-level"},
              projectEnabled ?? isEnabledByOverrides(path, globalSettings.extensions ?? [], globalBaseDir))
}
return {...toResolvedPaths(accumulator), installFailures}
```

内置扩展的 enabled 语义：
- 项目 `extensions` 中有匹配的 `+`/`-`/`!` → 用它，且 scope 记为 `"project"`（rank 5 不变）。
- 否则用全局 `extensions` 的 `isEnabledByOverrides`（默认 true），scope `"user"`。

`resolveLocalEntries(entries, type, target, metadata, baseDir)`：
```
if (entries.length === 0) return
{plain, patterns} = splitPatterns(entries)       // isPattern 分流
resolvedPlain = plain.map(p => resolvePathFromBase(p, baseDir))
allFiles = collectFilesFromPaths(resolvedPlain, type)
enabledPaths = applyPatterns(allFiles, patterns, baseDir)
for (f of allFiles) addResource(target, f, metadata, enabledPaths.has(f))
```
注意：无 pattern 时 `applyPatterns(allFiles, [], baseDir)` → includes 为空 → 全部 enabled。

`collectFilesFromPaths(paths, type)`：对每个存在路径 `statSync`；文件直接 push；目录
`collectResourceFiles(p, type)`；异常忽略。

`addAutoDiscoveredResources(...)`（**顺序即优先级**）：
1. `userMetadata = {source:"auto", scope:"user", origin:"top-level", baseDir: globalBaseDir}`
   `projectMetadata = {source:"auto", scope:"project", origin:"top-level", baseDir: projectBaseDir}`
2. `userOverrides` / `projectOverrides`：四种类型的 settings 数组。
3. `userDirs` = `agentDir/{extensions,skills,prompts,themes}`；
   `projectDirs` = `.kiana/{extensions,skills,prompts,themes}`。
4. `userAgentsSkillsDir = join(getHomeDir(), ".agents", "skills")`
   `projectTrusted = settingsManager.isProjectTrusted()`
   `projectAgentsSkillDirs = projectTrusted ? collectAncestorAgentsSkillDirs(cwd).filter(d => resolve(d) !== resolve(userAgentsSkillsDir)) : []`
5. `addResources(type, paths, metadata, overrides, baseDir)`：对每个 path
   `enabled = isEnabledByOverrides(path, overrides, baseDir)`，`addResource`。
6. **projectTrusted 时**：
   - 项目 extensions：`collectAutoExtensionEntries(projectDirs.extensions)`
   - 项目 skills：`collectAutoSkillEntries(projectDirs.skills, "kiana")`
7. **总是**（不受 trust 影响）：项目 `.agents/skills`（每个目录单独 baseDir = 其父级 `.agents`），
   用 `collectAutoSkillEntries(dir, "agents")` 与 `projectOverrides.skills`。
8. **projectTrusted 时**：项目 prompts（`collectAutoPromptEntries`）、项目 themes（`collectAutoThemeEntries`）。
9. **总是**：用户 extensions（`collectAutoExtensionEntries(userDirs.extensions)`）。
10. **总是**：用户 skills（`collectAutoSkillEntries(userDirs.skills, "kiana")`）。
11. **总是**：用户 `.agents/skills`（baseDir = `dirname(userAgentsSkillsDir)` = `~/.agents`，mode `"agents"`）。
12. **总是**：用户 prompts、用户 themes。

要点：**项目 `.agents/skills` 在 trust 未定时也不加载**（因为 `projectAgentsSkillDirs` 由 trust 门控）；
用户 `.agents/skills` 始终加载。

`createAccumulator()`：四个空 `Map`。
`getTargetMap(accumulator, type)`：switch；默认抛 `Unknown resource type: ${resourceType}`。
`addResource(map, path, metadata, enabled)`：`if (!path) return; if (!map.has(path)) map.set(path, {metadata, enabled})`。

`toResolvedPaths(accumulator)`：
```ts
mapToResolved(entries) {
  resolved = [...entries].map(([path,{metadata,enabled}]) => ({path, enabled, metadata}))
  resolved.sort((a,b) => resourcePrecedenceRank(a.metadata) - resourcePrecedenceRank(b.metadata))  // 稳定
  seen = new Set(); return resolved.filter(e => { const c = canonicalizePath(e.path);
    if (seen.has(c)) return false; seen.add(c); return true })
}
```
返回四个数组 + 调用方附加的 `installFailures`。

#### 2.10.9 `resolveExtensionSources(sources, options?)`

```
scope = options?.temporary ? "temporary" : options?.local ? "project" : "user"
for (source of sources.filter(s => s.startsWith("builtin:")))
  addResource(accumulator.extensions, source, {source:"builtin", scope, origin:"top-level"}, true)
packageSources = sources.filter(s => !s.startsWith("builtin:")).map(s => ({pkg: s, scope}))
installFailures = await resolvePackageSources(packageSources, accumulator)   // 无 onMissing
return {...toResolvedPaths(accumulator), installFailures}
```
即 `-e builtin:<name>` 直接作为 enabled 的内置扩展加入；未知名字由 resource-loader 报错。

#### 2.10.10 `resolvePackageSources(sources, accumulator, onMissing?)`

对每个 `{pkg, scope}`（**顺序遍历，串行**）：
1. `sourceStr = typeof pkg === "string" ? pkg : pkg.source`；`filter = typeof pkg === "object" ? pkg : undefined`。
2. `deltaBase = findAutoloadDeltaBase(pkg, scope, sources)`；
   `resolvedSource = deltaBase?.source ?? sourceStr`；`resolvedScope = deltaBase?.scope ?? scope`。
3. `parsed = parseSource(resolvedSource)`；
   `metadata = {source: sourceStr, scope, origin: "package"}`（**注意用原始 sourceStr/scope**）。
4. **local**：`baseDir = getBaseDirForScope(resolvedScope)`；`resolveLocalExtensionSource(parsed, accumulator, filter, metadata, baseDir)`；continue。
5. 定义 `installMissing()`：
   - 离线 → `false`。
   - `onMissing` 存在 → `action = await onMissing(resolvedSource)`；
     `"skip"` → `false`；`"error"` → **抛** `Missing source: ${resolvedSource}`。
   - `try { await installParsedSource(parsed, resolvedScope); return true } catch (e) {
       installFailures.push({source: resolvedSource, scope: resolvedScope, message: e.message}) ; return false }`。
6. **npm**：
   - `installedPath = getNpmInstallPath(parsed, resolvedScope)`。
   - `needsInstall = !existsSync(installedPath) || !(await installedNpmMatchesConfiguredVersion(parsed, installedPath))`。
   - 需要装 → `installMissing()`；失败 → continue（跳过该包资源）；成功 → 重新计算 `installedPath`。
   - `metadata.baseDir = installedPath`；`metadata.packageRoot = installedPath`；
     `collectPackageResources(installedPath, accumulator, filter, metadata)`。
7. **git**：
   - `installedPath = getGitInstallPath(parsed, resolvedScope)`。
   - 不存在 → `installMissing()`，失败 continue。
   - 存在且 `resolvedScope === "temporary"` 且 `!parsed.pinned` 且非离线 →
     `await refreshTemporaryGitSource(parsed, resolvedSource)`（失败静默，保留缓存）。
   - `metadata.baseDir = packageRoot = installedPath`；`collectPackageResources(...)`。

`findAutoloadDeltaBase(pkg, scope, sources)`：
```
if (scope !== "project" || typeof pkg !== "object" || pkg.autoload !== false) return undefined
identity = getPackageIdentity(pkg.source, scope)
userEntry = sources.find(e => e.scope === "user" && getPackageIdentity(sourceString(e.pkg), "user") === identity)
return userEntry ? {source: sourceString(userEntry.pkg), scope: "user"} : undefined
```

`resolveLocalExtensionSource(source: LocalSource, accumulator, filter, metadata, baseDir)`：
```
resolved = resolvePathFromBase(source.path, baseDir)
if (!existsSync(resolved)) return          // 静默
stats = statSync(resolved)                 // 抛错 → return
if (stats.isFile()) { metadata.baseDir = dirname(resolved);
                      addResource(accumulator.extensions, resolved, metadata, true); return }
if (stats.isDirectory()) { metadata.baseDir = resolved; metadata.packageRoot = resolved;
  resources = collectPackageResources(resolved, accumulator, filter, metadata)
  if (!resources) addResource(accumulator.extensions, resolved, metadata, true) }   // 目录本身作为扩展
```

`installParsedSource(parsed, scope)`：npm → `installNpm(parsed, scope, scope === "temporary")`；
git → `installGit(parsed, scope)`；local → 无操作。

#### 2.10.11 `collectPackageResources` 与 manifest

```ts
private collectPackageResources(packageRoot, accumulator, filter, metadata): boolean
```
1. **有 filter（settings 对象形式）**：对四种类型：
   - `filter.autoload === false` → `applyPackageDeltaFilter(root, filter[type] ?? [], type, target, metadata)`
   - `filter[type] !== undefined` → `applyPackageFilter(root, filter[type], type, target, metadata)`
   - 否则 → `collectDefaultResources(root, type, target, metadata)`
   返回 `true`。
2. **无 filter**：`manifest = readKianaManifest(join(root,"package.json"))`；
   有 manifest（含 `{}`）→ 四种类型 `addManifestEntries(manifest[type], root, type, target, metadata)`；返回 `true`。
3. 否则目录约定：对四种类型，若 `join(root, type)` 存在 → `collectResourceFiles(dir, type)`
   全部 `addResource(..., true)`，`hasAnyDir = true`。返回 `hasAnyDir`。

`collectDefaultResources(root, type, target, metadata)`：先看 manifest 的该字段（`if (entries)` ——
**空数组也是 truthy**，于是 `addManifestEntries([], ...)` 什么都不加，且不再看目录）；
否则看约定目录。

`applyPackageFilter(root, userPatterns, type, target, metadata)`：
```
{allFiles} = collectManifestFiles(root, type)
if (userPatterns.length === 0) { for (f of allFiles) addResource(target, f, metadata, false); return }
enabledByUser = applyPatterns(allFiles, userPatterns, root)
for (f of allFiles) addResource(target, f, metadata, enabledByUser.has(f))
```

`applyPackageDeltaFilter(root, userPatterns, type, target, metadata)`：
```
if (userPatterns.length === 0) return
{allFiles} = collectManifestFiles(root, type)
enabledByUser = applyAutoloadDisabledPatterns(allFiles, userPatterns, root)
for ([path, enabled] of enabledByUser) addResource(target, path, metadata, enabled)
```
（delta 模式只对**显式提到的**文件产生条目，未提到的由全局包条目提供。）

`collectManifestFiles(root, type)`：
```
manifest = readKianaManifest(join(root,"package.json"))
entries = manifest?.[type]
if (entries && entries.length > 0) {
  allFiles = collectFilesFromManifestEntries(entries, root, type)
  manifestPatterns = entries.filter(isOverridePattern)
  enabledByManifest = manifestPatterns.length > 0 ? applyPatterns(allFiles, manifestPatterns, root) : new Set(allFiles)
  return { allFiles: [...enabledByManifest], enabledByManifest }
}
conventionDir = join(root, type)
if (!existsSync(conventionDir)) return { allFiles: [], enabledByManifest: new Set() }
allFiles = collectResourceFiles(conventionDir, type)
return { allFiles, enabledByManifest: new Set(allFiles) }
```
注意 `allFiles` 在 manifest 分支被替换成 `enabledByManifest` 的数组（即已按 manifest 过滤）。

`addManifestEntries(entries, root, type, target, metadata)`：
```
if (!entries) return
allFiles = collectFilesFromManifestEntries(entries, root, type)
patterns = entries.filter(isOverridePattern)
enabledPaths = applyPatterns(allFiles, patterns, root)
for (f of allFiles) if (enabledPaths.has(f)) addResource(target, f, metadata, true)
```

`collectFilesFromManifestEntries(entries, root, type)`：
```
sourceEntries = entries.filter(e => !isOverridePattern(e))
resolved = sourceEntries.flatMap(e => hasGlobPattern(e) ? expandPackageGlob(e, root) : [resolve(root, e)])
return collectFilesFromPaths(resolved, type)
```
**注意**：`resolve(root, e)` 会展开 `~` 吗？——`path.resolve` **不**展开 `~`。测试
`should keep kiana manifest entries with leading tilde package-relative` 断言 `~foo` 按包内相对路径处理
（`resolve(root, "~foo")` = `<root>/~foo`）。

#### 2.10.12 `listConfiguredPackages()` / `getInstalledPath` / settings 读写

`listConfiguredPackages()`：**user 在前，project 在后**；每项
`{source: stringForm, scope, filtered: typeof pkg === "object", installedPath: getInstalledPath(source, scope)}`。

`getInstalledPath(source, scope)`：
- npm → `getNpmInstallPath(parsed, scope)` 存在则返回；
- git → `getGitInstallPath(parsed, scope)` 存在则返回；
- local → `resolvePathFromBase(parsed.path, getBaseDirForScope(scope))` 存在则返回；
- 否则 `undefined`。

`addSourceToSettings(source, options?)`：
```
scope = local ? "project" : "user"
current = scope==="project" ? getProjectSettings() : getGlobalSettings()
list = current.packages ?? []
normalized = normalizePackageSourceForSettings(source, scope)
index = list.findIndex(existing => packageSourcesMatch(existing, source, scope))
if (index !== -1) {
  existing = list[index]
  if (getPackageSourceString(existing) === normalized) return false
  next[index] = typeof existing === "string" ? normalized : {...existing, source: normalized}   // 保留 filters
  persist(next); return true
}
persist([...list, normalized]); return true
```

`removeSourceFromSettings(source, options?)`：过滤掉所有匹配项；数量变化才持久化并返回 `true`。

`normalizePackageSourceForSettings(source, scope)`：非 local 原样返回；local →
`rel = relative(baseDir, resolvePath(parsed.path))`；返回 `rel || "."`（相对 settings 文件基准目录）。
注意 local 的 `parsed.path` 用 `resolvePath`（基准 `cwd`）解析，而非 `baseDir`。

`getSourceMatchKeyForInput(source)`：
- npm → `npm:${name}`；git → `git:${host}/${path}`；local → `local:${resolvePath(path)}`（基准 cwd）。

`getSourceMatchKeyForSettings(source, scope)`：同上，但 local 用 `resolvePathFromBase(path, getBaseDirForScope(scope))`。

`packageSourcesMatch(existing, inputSource, scope)`：两个 key 相等。

#### 2.10.13 `parseSource` / npm spec / identity / dedupe

`parseSource(source)`：
```
if (source.startsWith("npm:")) {
  spec = source.slice(4).trim()
  {name, version} = parseNpmSpec(spec)
  return {type:"npm", spec, name, version, range: getNpmVersionRange(version), pinned: isExactNpmVersion(version)}
}
if (isLocalPath(source)) return {type:"local", path: source}
const gitParsed = parseGitUrl(source)     // 见 §2.10.20（utils/git.ts）
if (gitParsed) return gitParsed
return {type:"local", path: source}
```
`isLocalPath`：trim 后**不**以 `npm:` `git:` `github:` `http:` `https:` `ssh:` `builtin:` 开头即为 local
（`file:` 属于 local）。

`parseNpmSpec(spec)`：
```ts
const match = spec.match(/^(@?[^@]+(?:\/[^@]+)?)(?:@(.+))?$/);
if (!match) return { name: spec };
return { name: match[1] ?? spec, version: match[2] };
```
- `@scope/pkg@1.2.3` → name `@scope/pkg`, version `1.2.3`。
- `pkg@latest` → version `latest`（`valid("latest") === null` → `pinned = false`，`validRange("latest")` 为
  `"*"`? 实际 semver `validRange("latest")` 返回 `null` → `range = undefined`）。
- 无版本 → `version = undefined`，`range = undefined`，`pinned = false`。
- 名字里含 `@` 的 pathological 情况按正则回溯处理（`@a@b` → name `@a`, version `b`）。

`getPackageIdentity(source, scope?)`：
- npm → `npm:${name}`；git → `git:${host}/${path}`；
- local + scope → `local:${resolvePathFromBase(path, getBaseDirForScope(scope))}`；
- local 无 scope → `local:${resolvePath(path)}`。

`dedupePackages(packages)`：
```
result = []; seen = Map<identity, index>
for entry of packages:
  identity = getPackageIdentity(sourceString(entry.pkg), entry.scope)
  index = seen.get(identity)
  if (index === undefined) { seen.set(identity, result.length); result.push(entry); continue }
  existing = result[index]
  if (existing.scope === "project" && entry.scope === "user") {
    if (typeof existing.pkg === "object" && existing.pkg.autoload === false) result.push(entry)   // delta：两条都留
  } else if (entry.scope === "project") {
    result[index] = entry                                                                        // project 覆盖
  }
```

#### 2.10.14 安装 / 卸载 / 更新

`install(source, options?)`：
```
parsed = parseSource(source); scope = local ? "project" : "user"
assertProjectTrustedForScope(scope)
await withProgress("install", source, `Installing ${source}...`, async () => {
  npm → installNpm(parsed, scope, false)
  git → installGit(parsed, scope)
  local → resolved = resolvePath(parsed.path)；!existsSync → throw `Path does not exist: ${resolved}`；else return
  其它 → throw `Unsupported install source: ${source}`        // 类型上不可达
})
```

`remove`：`Removing ${source}...`；npm → `uninstallNpm`；git → `removeGit`；local → 无操作。

`update(source?)`：
1. 遍历 global 再 project 的 packages，用 `getPackageIdentity(sourceStr, scope)` 与
   `getPackageIdentity(source)`（**无 scope**，local 用 cwd 基准）比较；匹配的加入 `updateSources`。
2. 有 `source` 但没匹配 → 抛 `buildNoMatchingPackageMessage(source, [...global.packages, ...project.packages])`。
3. `await updateConfiguredSources(updateSources)`。

`buildNoMatchingPackageMessage(source, configured)`：
- 无建议 → `No matching package found for ${source}`
- 有建议 → `No matching package found for ${source}. Did you mean ${suggestion}?`

`findSuggestedConfiguredSource(source, configured)`：
- `trimmedSource = source.trim()`；收集 `Set<string>`。
- npm：若 `trimmedSource === parsed.name || trimmedSource === parsed.spec` → 加入该包的 **sourceStr**。
- git：`shorthand = host/path`；`shorthandWithRef = ref ? shorthand@ref : undefined`；
  若等于任一 → 加入。
- 返回 Set 的第一个值（插入顺序）。

`updateConfiguredSources(sources)`：
1. 离线或空 → return。
2. 分类：npm 且 `!pinned` → npmCandidates；git（**含 pinned**）→ gitCandidates。
3. npm 候选并发检查（limit 4）：`shouldUpdateNpmSource(parsed, scope)`；分 user/project 两组。
4. 任务：user 批量 `updateNpmBatch`、project 批量、git 逐个 `withProgress("update", src, `Updating ${src}...`)`
   并发（limit 4）；`await Promise.all(tasks)`（npm 批量与 git 组之间并行）。

`shouldUpdateNpmSource(source, scope)`：
```
installedPath = getManagedNpmInstallPath(source, scope)
installedVersion = exists ? getInstalledNpmVersion(path) : undefined
if (!installedVersion) return true
try { target = await getLatestNpmVersion(source.version ? source.spec : source.name, source.range)
      return gt(target, installedVersion) } catch { return true }
```

`updateNpmBatch(sources, scope)`：
```
sourceLabel = sources.length === 1 ? sources[0].source : `${scope} npm packages`
message     = sources.length === 1 ? `Updating ${sources[0].source}...` : `Updating ${scope} npm packages...`
specs = sources.map(e => e.parsed.version ? e.parsed.spec : `${e.parsed.name}@latest`)
await withProgress("update", sourceLabel, message, () => installNpmBatch(specs, scope))
```
`installNpmBatch(specs, scope)`：`installRoot = getNpmInstallRoot(scope, false)`；`ensureNpmProject`；
`runNpmCommand(getNpmInstallArgs(specs, installRoot))`。

`checkForAvailableUpdates()`：
1. 离线 → `[]`。
2. 收集 project + user 包，`dedupePackages`，过滤掉 `temporary`。
3. 每个包并发（limit 4）：
   - `parsed.type === "local" || parsed.pinned` → `undefined`。
   - npm：`installedPath = getNpmInstallPath(parsed, scope)`；不存在 → undefined；
     `npmHasAvailableUpdate` 为 false → undefined；否则
     `{source, displayName: parsed.name, type:"npm", scope}`。
   - git：`installedPath = getGitInstallPath`；不存在 → undefined；`gitHasAvailableUpdate` 为 false → undefined；
     否则 `{source, displayName: `${host}/${path}`, type:"git", scope}`。
4. 返回过滤掉 undefined 的结果（**保持输入顺序**）。

`installedNpmMatchesConfiguredVersion(source, installedPath)`：
```
installed = getInstalledNpmVersion(path); if (!installed) return false
return source.range ? satisfies(installed, source.range) : true
```

`npmHasAvailableUpdate(source, installedPath)`：离线 → false；无 installed → false；
try `gt(await getLatestNpmVersion(...), installed)` catch false。

`getInstalledNpmVersion(installedPath)`：读 `<path>/package.json`（stripBom + JSON），返回 `version`；
任何异常 → undefined。

`getLatestNpmVersion(packageSpec, range?)`：
```
npmCommand = getNpmCommand()
stdout = await runCommandCapture(npmCommand.command,
           [...npmCommand.args, "view", packageSpec, "version", "--json"],
           {cwd: this.cwd, timeoutMs: NETWORK_TIMEOUT_MS})
raw = stdout.trim(); if (!raw) throw new Error("Empty response from npm view")
parsed = JSON.parse(raw)
if (typeof parsed === "string") return parsed
if (Array.isArray(parsed)) {
  versions = parsed.filter(v => typeof v === "string" && v.length > 0)
  latest = range ? maxSatisfying(versions, range) : [...versions].sort(rcompare)[0]
  if (latest) return latest
}
throw new Error("Unexpected response from npm view")
```

`gitHasAvailableUpdate(installedPath)`：离线 → false；
```
localHead = await runCommandCapture("git", ["rev-parse","HEAD"], {cwd: installedPath, timeoutMs: 10000})
remoteHead = await getRemoteGitHead(installedPath)
return localHead.trim() !== remoteHead.trim()
catch → false
```

`getRemoteGitHead(installedPath)`：
1. `upstreamRef = getGitUpstreamRef(installedPath)`；若有：
   `ls-remote origin <upstreamRef>`，用 `/^([0-9a-f]{40})\s+/m` 提取，命中即返回。
2. 否则 `ls-remote origin HEAD`，`/^([0-9a-f]{40})\s+HEAD$/m`；不匹配 → throw `Failed to determine remote HEAD`。
（`runGitRemoteCommand` 带环境 `GIT_TERMINAL_PROMPT=0`，timeout 10000。）

`getLocalGitUpdateTarget(installedPath)`：
1. try：
   - `git rev-parse --abbrev-ref @{upstream}` → trim；不以 `origin/` 开头 → throw
     `Unsupported upstream remote: ${trimmed}`；`branch = slice(7)`；空 → throw `Missing upstream branch name`。
   - `git rev-parse @{upstream}` → head。
   - 返回 `{ref:"@{upstream}", head, fetchArgs:["fetch","--prune","--no-tags","origin",`+refs/heads/${branch}:refs/remotes/origin/${branch}`]}`。
2. catch：
   - `git remote set-head origin -a`（失败忽略）；
   - `git rev-parse origin/HEAD`；
   - `git symbolic-ref refs/remotes/origin/HEAD`（失败 → `""`）；
   - `branch = trim 后去掉 ^refs/remotes/origin/`；
   - 有 branch → `{ref:"origin/HEAD", head, fetchArgs:[..., `+refs/heads/${branch}:refs/remotes/origin/${branch}`]}`；
   - 否则 `{ref:"origin/HEAD", head, fetchArgs:["fetch","--prune","--no-tags","origin","+HEAD:refs/remotes/origin/HEAD"]}`。

`getGitUpstreamRef(installedPath)`：`rev-parse --abbrev-ref @{upstream}`；`origin/` 前缀 → `refs/heads/<branch>`；
否则 undefined；异常 → undefined。

`runWithConcurrency(tasks, limit)`：
```
if (tasks.length === 0) return []
results = new Array(tasks.length)
nextIndex = 0
workerCount = max(1, min(limit, tasks.length))
worker: while (true) { index = nextIndex; nextIndex += 1; if (index >= tasks.length) return;
                       results[index] = await tasks[index]() }
await Promise.all(workerCount 个 worker)
return results
```
**注意**：`nextIndex` 自增不是原子的，但 JS 单线程 await 点之间安全。Rust 用 `tokio::sync::Mutex`
或原子索引 + `JoinSet`；错误传播：JS 中任一 task 抛错 → `Promise.all` 立即 reject，其余 task 继续运行
（不会被取消）。Rust 必须**不取消**其余任务，但返回第一个错误。

#### 2.10.15 npm 命令构造

`getNpmCommand()`：
```
configured = settingsManager.getNpmCommand()
if (!configured || configured.length === 0) return {command:"npm", args:[]}
[command, ...args] = configured
if (!command) throw new Error("Invalid npmCommand: first array entry must be a non-empty command")
return {command, args}
```

`getPackageManagerName()`：
```
command = getNpmCommand(); normalize(name) = basename(name).replace(/\.(cmd|exe)$/i, "")
supported = {"npm","pnpm","bun"}
direct = normalize(command.command)
sep = command.args.lastIndexOf("--")
if (sep >= 0) return command.args[sep+1] ? normalize(command.args[sep+1]) : direct
if (supported.has(direct)) return direct
wrapped = unique(command.args.map(normalize).filter(x => supported.has(x)))
if (wrapped.length > 1) throw new Error(`Ambiguous npmCommand package managers: ${wrapped.join(", ")}`)
return wrapped[0] ?? direct
```

`getGitDependencyInstallArgs()`：
| manager | args |
|---|---|
| `bun` | `["install","--omit=dev","--omit=peer"]` |
| `pnpm` | `["install","--prod","--config.auto-install-peers=false","--config.strict-peer-dependencies=false","--config.strict-dep-builds=false"]` |
| `npm` | `["install","--omit=dev","--legacy-peer-deps"]` |
| 其它 | `["install"]` |

`getNpmInstallArgs(specs, root)`：
```
installRoot = existsSync(root) ? realpathSync(root) : root
manager = getPackageManagerName()
bun  → ["install", ...specs, "--cwd", installRoot, "--omit=peer"]
pnpm → ["install", ...specs, "--prefix", installRoot,
        "--config.auto-install-peers=false","--config.strict-peer-dependencies=false","--config.strict-dep-builds=false"]
其它 → ["install", ...specs, "--prefix", installRoot, "--legacy-peer-deps"]
```

`installNpm(source, scope, temporary)`：`root = getNpmInstallRoot(scope, temporary)`；`ensureNpmProject(root)`；
`runNpmCommand(getNpmInstallArgs([source.spec], root))`。

`uninstallNpm(source, scope)`：
```
root = getNpmInstallRoot(scope, false); if (!existsSync(root)) return
installRoot = realpathSync(root)
bun  → runNpmCommand(["uninstall", source.name, "--cwd", installRoot])
其它 → args = ["uninstall", source.name, "--prefix", installRoot]
       if (manager !== "pnpm") args.push("--legacy-peer-deps")
       runNpmCommand(args)
```

#### 2.10.16 git 安装 / 更新

`installGit(source, scope)`：
```
targetDir = getGitInstallPath(source, scope)
if (existsSync(targetDir)) {
  if (source.ref) { await ensureGitRef(targetDir, ["fetch","origin",source.ref], "FETCH_HEAD"); return }
  const target = await getLocalGitUpdateTarget(targetDir)
  await ensureGitRef(targetDir, target.fetchArgs, target.ref); return
}
gitRoot = getGitInstallRoot(scope)
if (gitRoot) ensureGitIgnore(gitRoot)
mkdirSync(dirname(targetDir), {recursive:true})
rmSync(getGitUpdateMarkerPath(targetDir), {force:true})
try {
  await runCommand("git", ["clone", source.repo, targetDir])
  if (source.ref) await runCommand("git", ["checkout", source.ref], {cwd: targetDir})
  if (existsSync(join(targetDir,"package.json")))
    await runNpmCommand(getGitDependencyInstallArgs(), {cwd: targetDir})
} catch (error) {
  rmSync(targetDir, {recursive:true, force:true})
  pruneEmptyGitParents(targetDir, gitRoot)
  throw error
}
```

`updateGit(source, scope)`：目标不存在 → `installGit`；有 ref → fetch + `ensureGitRef(..., "FETCH_HEAD")`；
否则 `getLocalGitUpdateTarget` + `ensureGitRef`。

`ensureGitRef(targetDir, fetchArgs, ref)`：
```
await runCommand("git", fetchArgs, {cwd: targetDir})
localHead = await runCommandCapture("git",["rev-parse","HEAD"],{cwd, timeout 10000})
targetHead = await runCommandCapture("git",["rev-parse", `${ref}^{commit}`],{cwd, timeout 10000})
markerPath = getGitUpdateMarkerPath(targetDir)
if (localHead.trim() === targetHead.trim()) {
  existsSync(markerPath) ? await cleanAndInstallGitDependencies(targetDir, markerPath)
                         : await repairMissingGitDependencies(targetDir)
  return
}
writeFileSync(markerPath, "", "utf-8")          // 先写 marker，再 reset
await runCommand("git", ["reset","--hard",`${ref}^{commit}`], {cwd: targetDir})
await cleanAndInstallGitDependencies(targetDir, markerPath)
```

`cleanAndInstallGitDependencies(targetDir, markerPath)`：
```
try { await runCommand("git", ["clean","-fdx"], {cwd: targetDir}) }
catch (e) { await repairMissingGitDependencies(targetDir).catch(()=>{}); throw e }
if (existsSync(join(targetDir,"package.json")))
  await runNpmCommand(getGitDependencyInstallArgs(), {cwd: targetDir})
rmSync(markerPath, {force:true})
```

`hasMissingGitDependencies(targetDir)`：
- 无 `package.json` → false。
- 解析 `dependencies`；非对象或数组 → false。
- `nodeModulesDir = resolve(targetDir, "node_modules")`；任一依赖：
  `dependencyPath = resolve(nodeModulesDir, name)`；若 `!dependencyPath.startsWith(nodeModulesDir + sep)` → false（跳过）；
  `!existsSync(dependencyPath)` → true。
- 异常 → false。

`repairMissingGitDependencies(targetDir)`：`if (!hasMissing) return; await runNpmCommand(getGitDependencyInstallArgs(), {cwd})`。

`getGitUpdateMarkerPath(targetDir)`：`join(dirname(targetDir), "." + basename(targetDir) + ".kiana-update-incomplete")`。

`getGitInstallPath(source, scope)`：
- `temporary` → `getTemporaryDir("git-" + source.host, source.path, source.ref)`
- 其它 → `resolveManagedPath(installRoot, source.host, source.path)`；`installRoot` 缺失 → throw `Missing git install root`。

`refreshTemporaryGitSource(source, sourceStr)`：离线 → return；
`withProgress("pull", sourceStr, `Refreshing ${sourceStr}...`, () => updateGit(source, "temporary"))`；异常吞掉。

`removeGit(source, scope)`：`rmSync(targetDir, {recursive:true, force:true})`；
`rmSync(getGitUpdateMarkerPath(targetDir), {force:true})`；`pruneEmptyGitParents`。

`pruneEmptyGitParents(targetDir, installRoot?)`：
```
if (!installRoot) return
resolvedRoot = resolve(installRoot); current = dirname(targetDir)
while (current.startsWith(resolvedRoot) && current !== resolvedRoot) {
  if (!existsSync(current)) { current = dirname(current); continue }
  if (readdirSync(current).length > 0) break
  try { rmSync(current, {recursive:true, force:true}) } catch { break }
  current = dirname(current)
}
```
注意 `startsWith` 是字符串前缀（不检查分隔符）——`/a/bc` 会匹配 `/a/b` 前缀。必须**逐字复刻**。

#### 2.10.17 安装路径

`ensureNpmProject(installRoot)`：
```
if (!existsSync(installRoot)) mkdirSync(installRoot, {recursive:true})
markPathIgnoredByCloudSync(installRoot)
ensureGitIgnore(installRoot)
pkgPath = join(installRoot, "package.json")
if (!existsSync(pkgPath))
  writeFileSync(pkgPath, JSON.stringify({name:"kiana-extensions", private:true}, null, 2), "utf-8")
```
即写入 **【逐字】**：
```json
{
  "name": "kiana-extensions",
  "private": true
}
```
（`JSON.stringify(..., null, 2)` 不追加结尾换行。）

`ensureGitIgnore(dir)`：
```
if (!existsSync(dir)) mkdirSync(dir, {recursive:true})
ignorePath = join(dir, ".gitignore")
if (!existsSync(ignorePath)) writeFileSync(ignorePath, "*\n!.gitignore\n", "utf-8")
```

`getNpmInstallRoot(scope, temporary)`：
- `temporary` → `getTemporaryDir("npm")`
- `project` → 断言 trust；`join(cwd, ".kiana", "npm")`
- `user` → `join(agentDir, "npm")`

`getGlobalNpmRoot()`（带缓存）：
```
key = [command, ...args].join("\0")
if (cached && cachedKey === key) return cached
if (manager === "bun") { binDir = runNpmCommandSync(["pm","bin","-g"]).trim();
                         root = join(dirname(binDir), "install", "global", "node_modules") }
else                   { root = runNpmCommandSync(["root","-g"]).trim() }
cache = root; cachedKey = key; return root
```

`getPnpmGlobalPackagePath(packageName)`：
- 非 pnpm → undefined。
- `output = runNpmCommandSync(["list","-g","--depth","0","--json"])`；`JSON.parse` 为数组；
  遍历找 `entry.dependencies?.[packageName]?.path`；返回首个非空；异常 → undefined（由调用方 catch）。

`getManagedNpmInstallPath(source, scope)`：
- `temporary` → `join(getTemporaryDir("npm"), "node_modules", source.name)`
- `project` → 断言 trust；`join(cwd, ".kiana", "npm", "node_modules", source.name)`
- `user` → `join(agentDir, "npm", "node_modules", source.name)`

`getLegacyGlobalNpmInstallPath(source)`：try `getPnpmGlobalPackagePath(name) ?? join(getGlobalNpmRoot(), name)` catch undefined。

`getNpmInstallPath(source, scope)`：
```
managed = getManagedNpmInstallPath(source, scope)
if (scope !== "user" || existsSync(managed)) return managed
legacy = getLegacyGlobalNpmInstallPath(source)
return legacy && existsSync(legacy) ? legacy : managed
```
即**只在 user 作用域**、managed 不存在时回退到遗留全局安装（`npm root -g` / pnpm）。

`getGitInstallRoot(scope)`：temporary → undefined；project → 断言 trust + `join(cwd,".kiana","git")`；
user → `join(agentDir, "git")`。

`getTemporaryDir(prefix, suffix?, ref?)`：
```
root = resolveManagedPath(getExtensionTempFolder(agentDir), prefix)
hash = sha256(`${prefix}-${suffix ?? ""}${ref ? `@${ref}` : ""}`).hex().slice(0, 8)
return resolveManagedPath(root, hash, suffix ?? "")
```
即 `<agentDir>/tmp/extensions/<prefix>/<hash8>/<suffix>`（suffix 为空时就是 `<prefix>/<hash8>`）。

`resolveManagedPath(root, ...parts)`：
```
resolvedRoot = resolve(root); resolvedPath = resolve(resolvedRoot, ...parts)
if (resolvedPath !== resolvedRoot && !resolvedPath.startsWith(resolvedRoot + sep))
  throw new Error(`Refusing to use path outside package install root: ${resolvedPath}`)
return resolvedPath
```
测试 `should reject paths outside git install roots` 用 git path 含 `..` 触发。

`getBaseDirForScope(scope)`：project → 断言 trust + `join(cwd, ".kiana")`；user → `agentDir`；temporary → `cwd`。

`resolvePath(input)`：`resolvePath(input, this.cwd, {homeDir: getHomeDir(), trim: true})`。
`resolvePathFromBase(input, baseDir)`：同但基准为 `baseDir`。

#### 2.10.18 进程执行（`spawnCommand` / `runCommand*`）

`spawnCommand(command, args, options?)`：
```ts
spawnProcess(command, args, {
  cwd: options?.cwd,
  stdio: isStdoutTakenOver() ? ["ignore", 2, 2] : "inherit",
  env: getEnv(),
})
```
**stdio 特殊值**：`isStdoutTakenOver()` 为 true 时是 `["ignore", 2, 2]`——stdout 忽略，stderr 与 stdin
都指向**文件描述符 2**（即 stderr）。这样交给子进程的输出不会污染协议流。
否则 `"inherit"`（三者都继承）。

`spawnCaptureCommand(command, args, options?)`：`env = {...baseEnv, ...options.env}`；
stdio `["ignore","pipe","pipe"]`。

`runCommandCapture(command, args, options?): Promise<string>`：
- 收集 stdout/stderr 字符串；`timeoutMs` 存在时 `setTimeout(() => { timedOut = true; child.kill() }, timeoutMs)`。
- `error` 事件 → clearTimeout + reject(error)。
- `close(code, signal)`：
  - clearTimeout；
  - `timedOut` → reject(`${command} ${args.join(" ")} timed out after ${timeoutMs}ms`)；
  - `code === 0` → resolve(**stdout.trim()**)（**不含 stderr**）；
  - 否则 `exitStatus = code === null ? `signal ${signal ?? "unknown"}` : `code ${code}``；
    reject(`${command} ${args.join(" ")} failed with ${exitStatus}: ${stderr || stdout}`)。
- **注意**：`runCommandCapture` 在 `close` 事件解析（不是 `exit`），确保管道读完。测试
  `should wait for close before resolving captured stdout` 固定此行为。

`runCommand(command, args, options?): Promise<void>`：
- `child.on("error", reject)`；`child.on("exit", code => code === 0 ? resolve() : reject(`${command} ${args.join(" ")} failed with code ${code}`))`。
- **用 `exit` 而不是 `close`**（继承 stdio 时 `close` 可能延迟）。

`runCommandSync(command, args): string`：
```ts
result = spawnProcessSync(command, args, {stdio:["ignore","pipe","pipe"], encoding:"utf-8", env:getEnv()})
if (result.error || result.status !== 0)
  throw new Error(`Failed to run ${command} ${args.join(" ")}: ${result.error?.message || result.stderr || result.stdout}`)
return (result.stdout || result.stderr || "").trim()
```
**argv 保真**：参数不被 shell 解析（测试 `should preserve argv entries containing spaces`）。

#### 2.10.19 测试固定下来的具体命令形态（必须逐字）

| 场景 | 实际 argv（含 npmCommand 前缀） |
|---|---|
| npm 安装（默认 npm） | `npm install <spec> --prefix <realpath(root)> --legacy-peer-deps` |
| npm 安装（bun） | `bun install <spec> --cwd <realpath(root)> --omit=peer` |
| npm 安装（pnpm） | `pnpm install <spec> --prefix <realpath(root)> --config.auto-install-peers=false --config.strict-peer-dependencies=false --config.strict-dep-builds=false` |
| npm 卸载（npm） | `npm uninstall <name> --prefix <realpath(root)> --legacy-peer-deps` |
| npm 卸载（pnpm） | `pnpm uninstall <name> --prefix <realpath(root)>` |
| npm 卸载（bun） | `bun uninstall <name> --cwd <realpath(root)>` |
| npm 版本查询 | `<npm> view <spec-or-name> version --json`（cwd = manager.cwd） |
| npm 全局根 | `<npm> root -g`（bun：`pm bin -g`） |
| pnpm 全局包路径 | `<npm> list -g --depth 0 --json` |
| git 依赖安装（npm） | `npm install --omit=dev --legacy-peer-deps`（cwd = checkout） |
| git 依赖安装（bun） | `bun install --omit=dev --omit=peer` |
| git 依赖安装（pnpm） | `pnpm install --prod --config.auto-install-peers=false --config.strict-peer-dependencies=false --config.strict-dep-builds=false` |
| git clone | `git clone <repo> <targetDir>` |
| git checkout | `git checkout <ref>`（cwd = targetDir） |
| git fetch（pinned） | `git fetch origin <ref>` |
| git reset | `git reset --hard <ref>^{commit}` |
| git clean | `git clean -fdx` |
| git remote head | `git ls-remote origin <ref>` / `HEAD`（env `GIT_TERMINAL_PROMPT=0`） |
| 解析 head | `git rev-parse HEAD` / `git rev-parse <ref>^{commit}` / `git rev-parse @{upstream}` / `git rev-parse origin/HEAD` |
| upstream 名 | `git rev-parse --abbrev-ref @{upstream}` |
| set-head | `git remote set-head origin -a` |
| symbolic-ref | `git symbolic-ref refs/remotes/origin/HEAD` |

**npmCommand 前缀语义**：settings 的 `npmCommand` 是 argv 数组。例如
`["mise","exec","node@20","--","npm"]` 时，`npm install` 变成
`mise exec node@20 -- npm install ...`；`mise` 是 command，其余是 args。
`getPackageManagerName()` 对 `["mise","exec","node@20","--","npm"]` 返回 `npm`（`--` 之后）。

#### 2.10.20 `utils/git.ts` 的解析规则（被 `parseSource` 调用）

`parseGitUrl(source)`：
1. `trimmed = source.trim()`；`hasGitPrefix = trimmed.startsWith("git:")`；
   `url = hasGitPrefix ? trimmed.slice(4).trim() : trimmed`。
2. **无 `git:` 前缀时只接受显式协议**：`!/^(https?|ssh|git):\/\//i.test(url)` → `null`。
   因此 `github.com/x/y`（shorthand）无前缀 → `null` → 落到 `local`（测试
   `should treat host/path shorthand as local without git: prefix`）。
3. `splitRef(url)`：拆出 `@ref`：
   - SCP 形式 `git@host:path@ref` → `{repo: "git@host:path", ref}`。
   - 含 `://` → `new URL`，`pathname` 去前导 `/`，第一个 `@` 之后为 ref；`parsed.pathname = "/" + repoPath`，
     `repo = parsed.toString().replace(/\/$/, "")`。
   - 其它（`host/path@ref`）→ `{repo: "host/path", ref}`。
   任一部分为空 → 返回 `{repo: url}`（不拆）。
4. `hostedCandidates = [split.ref ? `${split.repo}#${split.ref}` : undefined, url]`，
   对每个 `hostedGitInfo.fromUrl(candidate)`：
   - 命中且 `split.ref && info.project?.includes("@")` → continue（避免把 ref 吃进 project）。
   - `useHttpsPrefix = repo 不以 http:// / https:// / ssh:// / git:// / git@ 开头`；
     返回 `buildGitSource({repo: useHttpsPrefix ? "https://" + split.repo : split.repo,
                           host: info.domain || "", path: `${info.user}/${info.project}`,
                           ref: info.committish || split.ref || undefined})`。
5. 否则 `httpsCandidates = [split.ref ? `https://${split.repo}#${split.ref}` : undefined, `https://${url}`]`
   再试一遍 hostedGitInfo，命中同 4（repo 固定 `https://${split.repo}`）。
6. 最后 `parseGenericGitUrl(url)`：
   - `{repo: repoWithoutRef, ref} = splitRef(url)`。
   - SCP (`git@host:path`) → host/path。
   - `https://` / `http://` / `ssh://` / `git://` → `new URL`，host/path。
   - 其它：`slashIndex = repo.indexOf("/")`；无 → null；`host = repo.slice(0, slash)`；
     `path = repo.slice(slash+1)`；**`host` 必须含 `.` 或等于 `localhost`**，否则 null；
     `repo = "https://" + repoWithoutRef`。
   - `buildGitSource({repo, host, path, ref})`。

`buildGitSource({repo, host, path, ref})`：
```
if (path.startsWith("/")) return null
normalizedPath = path.replace(/\.git$/, "").replace(/^\/+/, "")
if (!host || !normalizedPath || normalizedPath.split("/").length < 2) return null
if (hasUnsafeGitInstallPart(host, false) || hasUnsafeGitInstallPart(normalizedPath, true)) return null
return { type:"git", repo, host, path: normalizedPath, ref, pinned: Boolean(ref) }
```
`hasUnsafeGitInstallPart(value, allowSlash)`：
```
decoded = decodeURIComponent(value)（失败返回 null → true）
对 [value, decoded] 每个 candidate：
  candidate 含 "\0" 或 "\\" 或以 "/" 开头 → true
  !allowSlash && candidate 含 "/" → true
  candidate.split("/") 含 ".." → true
return false
```

**identity 归一**：`host/path` 忽略协议与 `.git`，因此
`https://github.com/user/repo`、`ssh://git@github.com/user/repo`、`git@github.com:user/repo`、
`git:github.com/user/repo` 全部得到 `host="github.com"`、`path="user/repo"`，identity 一致。


---

### 2.11 `src/core/resource-loader.ts`（1322 行）

#### 2.11.1 导出概览

```ts
export type { ResourceCollision, ResourceDiagnostic };   // 再导出
export interface ResourceExtensionPaths {
  skillPaths?: Array<{ path: string; metadata: PathMetadata }>;
  promptPaths?: Array<{ path: string; metadata: PathMetadata }>;
  themePaths?: Array<{ path: string; metadata: PathMetadata }>;
}
export interface ResourceLoaderReloadOptions {
  resolveProjectTrust?: (input: { extensionsResult: LoadExtensionsResult }) => Promise<boolean>;
}
export function formatPackageInstallWarning(failure: PackageInstallFailure): string;
export function isBuiltinExtension(input: InlineExtension): input is BuiltinExtension;
export function loadProjectContextFiles(options: { cwd: string; agentDir: string }): Array<{ path: string; content: string }>;
export interface ResourceLoader { /* 11 方法 */ }
export interface DefaultResourceLoaderOptions { /* 见 2.11.3 */ }
export class DefaultResourceLoader implements ResourceLoader;
```

`ResourceLoader` 接口 **11 个方法**：

```ts
getExtensions(): LoadExtensionsResult;
getSkills(): { skills: Skill[]; diagnostics: ResourceDiagnostic[] };
getPrompts(): { prompts: PromptTemplate[]; diagnostics: ResourceDiagnostic[] };
getThemes(): { themes: Theme[]; diagnostics: ResourceDiagnostic[] };
getAgentsFiles(): { agentsFiles: Array<{ path: string; content: string }> };
getSystemPrompt(): string | undefined;
getSystemPromptSource(): { path: string } | undefined;
getAppendSystemPrompt(): string[];
getAppendSystemPromptSources(): Array<{ path: string }>;
extendResources(paths: ResourceExtensionPaths): void;
reload(options?: ResourceLoaderReloadOptions): Promise<void>;
```
全部为**同步 getter**（返回内部引用，不做克隆）。

#### 2.11.2 常量 `HOST_PROVIDED_EXTENSION_PACKAGES`（**逐字 10 项**）

```
@earendil-works/kiana-agent-core
@earendil-works/kiana-ai
@earendil-works/kiana-coding-agent
@earendil-works/kiana-tui
@mariozechner/kiana-agent-core
@mariozechner/kiana-ai
@mariozechner/kiana-coding-agent
@mariozechner/kiana-tui
@sinclair/typebox
typebox
```

`collectExtensionPackageWarnings(extensionPaths, metadataByPath)`：
1. `packageRoots` = 这些扩展路径的 `metadata.packageRoot` 去重集合（Set，插入顺序）。
2. 对每个 root：读 `<root>/package.json`（stripBom + JSON.parse，**解析失败会抛出**，未捕获）
   - `dependencies` 非对象 / null / 数组 → skip。
   - `hostDependencies = keys.filter(inSet).sort()`（字典序，`Array.prototype.sort` 默认 UTF-16 比较）。
   - 空 → skip。
3. push `{ path: packageJsonPath, warning: 【逐字】 }`：

```
Host-provided extension packages must be declared in peerDependencies with a "*" range, not dependencies: ${hostDependencies.join(", ")}. Installed copies can bypass the extension loader and create duplicate runtime modules.
```

#### 2.11.3 安装失败警告文案

```ts
function describePackageInstallFailure(failure): string {
  const hint = failure.scope === "temporary"
    ? "Check the source and try again."
    : `Run "${APP_NAME} remove ${failure.source}${failure.scope === "project" ? " --local" : ""}" to remove it from settings.json.`;
  return `failed to install (${failure.message}). ${hint}`;
}
export function formatPackageInstallWarning(failure) {
  return `Extension package "${failure.source}": ${describePackageInstallFailure(failure)}`;
}
```
即启动横幅总共三种形态（**逐字**）：
```
Extension package "<source>": failed to install (<message>). Check the source and try again.
Extension package "<source>": failed to install (<message>). Run "kiana remove <source> --local" to remove it from settings.json.   // project
Extension package "<source>": failed to install (<message>). Run "kiana remove <source>" to remove it from settings.json.          // user
```
`collectPackageInstallWarnings(...failures)` 返回 `{path: failure.source, warning: describePackageInstallFailure(failure)}`
（注意**没有** `Extension package "…":` 前缀，与 `formatPackageInstallWarning` 不同；两处都真实存在）。

`mergeExtensionWarnings(result, warnings)`：按 `path` 去重，**后者覆盖前者**（`new Map([...old, ...new])` 保序且值取新）；
结果写回 `result.warnings`。

#### 2.11.4 `isBuiltinExtension` 与 `omitReplacedExtensions`

```ts
type BuiltinExtension = Exclude<InlineExtension, ExtensionFactory>;
export function isBuiltinExtension(input: InlineExtension): input is BuiltinExtension {
  return typeof input !== "function" && input.builtin === true;
}
```

`omitReplacedExtensions(extensions, warnings?)`：
1. `names(ext)` = `tools` 键 → `tool:<name>`，`commands` 键 → `command:<name>`，`flags` 键 → `flag:<name>`（各保持 Map 顺序；先 tools 再 commands 再 flags）。
2. `taken` = 所有 **非 replaceable** 扩展的名字 → 扩展（**先到先得**：`new Map(pairs)` 中后写的同键覆盖？——这里用
   `flatMap` 生成 pairs 再 `new Map`，**后者覆盖前者**。因加载顺序 = 优先级顺序，实际等于「最后一个非 replaceable 者」拥有？
   不对：pairs 序列是 [ext1 的所有名字..., ext2 的所有名字...]，`new Map` 对重复键保留**后**出现的值。
   然而重复键意味着两个非 replaceable 扩展注册同名工具，这已由 `detectExtensionConflicts` 记错误；此处只影响谁被 reporting。）
3. 对每个扩展：非 replaceable → 保留。replaceable → 找第一个 `taken.get(name)` 存在的 `{name, extension}`：
   - 无 → 保留。
   - 有，且 `extension.path` 以 `builtin:` 开头 → push 警告，路径为**内置扩展的 path**，文案：
     ```
     Extension ${replacement.extension.path} registers ${kind} \`${registeredName}\`, so built-in extension \`${builtinName}\` was not loaded. To use \`${builtinName}\`, run \`kiana config\` and make sure it is enabled under Built-in extensions, then disable or remove the existing extension. We recommend only having one or the other loaded at a time.
     ```
     其中 `kind` 是 `tool` / `command` / `flag`（`replacement.name` 冒号前），
     `registeredName` = `command` → `/${rawName}`，`flag` → `--${rawName}`，`tool` → `rawName`；
     `builtinName` = `extension.path.slice("builtin:".length)`。
     注意文案里的 `` `kiana config` `` 是**硬编码 `kiana`**（不插值 `APP_NAME`），**逐字**。
   - 有 → **丢弃**该 replaceable 扩展（无论是否内置）。
4. 返回过滤后的数组（顺序不变）。

#### 2.11.5 `resolvePromptInput`

```ts
function resolvePromptInput(input: string | undefined, description: string): string | undefined
```
1. `!input` → `undefined`（空串也算）。
2. `existsSync(input)` → try `stripBom(readFileSync(input,"utf-8"))`；catch →
   `console.error(chalk.yellow(\`Warning: Could not read ${description} file ${input}: ${error}\`))` 并**返回 `input` 原样**
   （把路径当作提示词文本）。
3. 不存在 → 返回 `input` 原样。

`description` 只有两个值：`"system prompt"` 与 `"append system prompt"`。
`${error}` 是 `Error` 的 `toString()`（`Error: EACCES: …`）。

#### 2.11.6 `loadContextFileFromDir(dir)`

候选文件（**顺序固定，逐字含大小写变体**）：
```
AGENTS.override.md
AGENTS.md
AGENTS.MD
CLAUDE.md
CLAUDE.MD
```
对每个 `<dir>/<filename>`：`existsSync` → try `statSync().isFile()`；非文件 → `continue`；
返回 `{path: filePath, content: stripBom(readFileSync(...,"utf-8"))}`；异常 →
`console.error(chalk.yellow(\`Warning: Could not read ${filePath}: ${error}\`))` 后继续下一个候选。
全部失败 → `null`。

#### 2.11.7 `findShadowedContextFile(cwd)`（worktree 去重）

```
gitPaths = findGitPaths(cwd); if (!gitPaths) return undefined
commonGitDir = canonicalizePath(gitPaths.commonGitDir)
worktreeRoot = canonicalizePath(gitPaths.repoDir)
mainRepoRoot = dirname(commonGitDir)
if (!worktreeRoot.startsWith(mainRepoRoot + sep)) return undefined     // 普通仓库 / 兄弟 worktree
if (canonicalizePath(join(mainRepoRoot, ".git")) !== commonGitDir) return undefined   // bare / submodule
worktreeContextFile = loadContextFileFromDir(worktreeRoot)
return worktreeContextFile ? join(mainRepoRoot, basename(worktreeContextFile.path)) : undefined
```
**注意字符串前缀比较 `startsWith(mainRepoRoot + sep)`**，不做路径规范化。
`gitPaths` 来自 `footer-data-provider.findGitPaths`（见 §2.15）。

#### 2.11.8 `loadProjectContextFiles({cwd, agentDir})`

1. `resolvedCwd = resolvePath(cwd)`；`resolvedAgentDir = resolvePath(agentDir)`。
2. `contextFiles = []`；`seenPaths = Set`。
3. 全局：`loadContextFileFromDir(resolvedAgentDir)` 存在 → push + seen。
4. `shadowedContextFile = findShadowedContextFile(resolvedCwd)`。
5. `ancestorContextFiles = []`；`currentDir = resolvedCwd`；循环：
   - `contextFile = loadContextFileFromDir(currentDir)`
   - `isShadowed = shadowed !== undefined && contextFile !== null &&
       join(canonicalizePath(currentDir), basename(contextFile.path)) === shadowed`
   - 若 `contextFile && !isShadowed && !seenPaths.has(contextFile.path)` → `ancestorContextFiles.unshift(contextFile)` + seen。
   - 父目录等于自身 → break；否则 `currentDir = parentDir`。
6. `contextFiles.push(...ancestorContextFiles)`。

**顺序**：全局（agentDir）→ 根祖先 → … → cwd 自身（最近的最后）。
`seenPaths` 用**原始路径**（未 canonicalize），同一文件经不同路径（符号链接）可能重复加载。

#### 2.11.9 `DefaultResourceLoaderOptions`（全部字段）

```ts
{
  cwd: string; agentDir: string;
  settingsManager?: SettingsManager;
  eventBus?: EventBus;
  additionalExtensionPaths?: string[];
  additionalSkillPaths?: string[];
  additionalPromptTemplatePaths?: string[];
  additionalThemePaths?: string[];
  extensionFactories?: InlineExtension[];
  noExtensions?: boolean;
  disabledBuiltinExtensions?: string[];
  noSkills?: boolean;
  noPromptTemplates?: boolean;
  noThemes?: boolean;
  noContextFiles?: boolean;
  systemPrompt?: string;
  appendSystemPrompt?: string[];
  extensionsOverride?: (base: LoadExtensionsResult) => LoadExtensionsResult;
  skillsOverride?: (base: { skills; diagnostics }) => { skills; diagnostics };
  promptsOverride?: (base: { prompts; diagnostics }) => { prompts; diagnostics };
  themesOverride?: (base: { themes; diagnostics }) => { themes; diagnostics };
  agentsFilesOverride?: (base: { agentsFiles }) => { agentsFiles };
  systemPromptOverride?: (base: string | undefined) => string | undefined;
  appendSystemPromptOverride?: (base: string[]) => string[];
}
```

构造函数：
- `cwd = resolvePath(options.cwd)`、`agentDir = resolvePath(options.agentDir)`。
- `settingsManager = options.settingsManager ?? SettingsManager.create(cwd, agentDir)`。
- `eventBus = options.eventBus ?? createEventBus()`。
- `extensionFactories` = 工厂中**非** builtin 的；`builtinExtensions: Map<name, BuiltinExtension>` 只收 builtin
  （同名后者覆盖前者）。
- `new DefaultPackageManager({cwd, agentDir, settingsManager, builtinExtensions: [...map.keys()]})`。
- 各 `additional*Paths` 默认 `[]`；`no*` 默认 `false`；`systemPromptSource = options.systemPrompt`；
  `appendSystemPromptSource = options.appendSystemPrompt`。
- 初始状态：`extensionsResult = {extensions:[], errors:[], runtime: createExtensionRuntime()}`、
  `skills/prompts/themes/agentsFiles/appendSystemPrompt/appendSystemPromptSourcePaths/lastSkillPaths/lastPromptPaths/lastThemePaths = []`、
  `extension{Skill,Prompt,Theme}SourceInfos = new Map()`、`resourceMetadataByPath = new Map()`、`loaded = false`。

#### 2.11.10 `extendResources(paths)`

```
skillPaths = normalizeExtensionPaths(paths.skillPaths ?? [])
promptPaths = normalizeExtensionPaths(paths.promptPaths ?? [])
themePaths = normalizeExtensionPaths(paths.themePaths ?? [])
```
`normalizeExtensionPaths(entries)`：每项 `path = resolveResourcePath(entry.path)`；
`metadata = entry.metadata.baseDir ? {...entry.metadata, baseDir: resolveResourcePath(baseDir)} : entry.metadata`。

然后对每类：
- 把 `createSourceInfo(path, metadata)` 写入对应 `extension*SourceInfos` Map（**同名覆盖，保留首次插入位置**）。
- 若非空：`last*Paths = mergePaths(last*Paths, 新路径)`；调用对应 `update*FromPaths(last*Paths, resourceMetadataByPath)`。

**注意**：extendResources 在 `reload()` 之前调用也有效，但此时 `resourceMetadataByPath` 为空。

`mergePaths(primary, additional)`：
```
for p of [...primary, ...additional]:
  resolved = resolveResourcePath(p); canonical = canonicalizePath(resolved)
  if (seen.has(canonical)) continue; seen.add(canonical); merged.push(resolved)
```
（**保留首次出现，输出为解析后的绝对路径**；`builtin:` 等合成路径原样保留。）

`resolveResourcePath(p)` = `isSyntheticPath(p) ? p : resolvePath(p, this.cwd, { trim: true })`。
`resolveExtensionLoadPath(p)` = `isSyntheticPath(p) ? p : resolvePath(p, this.cwd, { normalizeUnicodeSpaces: true })`
（**注意 trim 与 normalizeUnicodeSpaces 的差别**——两处不同）。

#### 2.11.11 `reload(options?)` —— **完整顺序（必须逐字复刻）**

```
1.  resetTimings("extensions")
2.  if (this.loaded) clearExtensionCache()
3.  preTrustExtensions = undefined
    if (options?.resolveProjectTrust) {
      preTrustExtensions = await this.loadProjectTrustExtensions()
        //   = settingsManager.setProjectTrusted(false); await settingsManager.reload();
        //     return this.loadCurrentExtensionSet({includeInlineFactories:true})
      projectTrusted = await options.resolveProjectTrust({extensionsResult: preTrustExtensions})
      this.settingsManager.setProjectTrusted(projectTrusted)
    }
4.  await settingsManager.reload()          // 保留当前 projectTrusted
5.  resolvedPaths = await packageManager.resolve()                    // 无 onMissing
6.  cliExtensionPaths = await packageManager.resolveExtensionSources(additionalExtensionPaths, {temporary:true})
7.  installWarnings = collectPackageInstallWarnings(resolvedPaths.installFailures, cliExtensionPaths.installFailures)
8.  resourceMetadataByPath = new Map(); metadataByPath = this.resourceMetadataByPath
9.  extension{Skill,Prompt,Theme}SourceInfos = new Map()
10. getEnabledResources(resources): 先把**所有**（含 disabled）path→metadata 写入 metadataByPath（不覆盖已有），
    再 filter(enabled)
    getEnabledPaths(resources) = getEnabledResources(resources).map(r => r.path)
11. enabledExtensions = getEnabledPaths(resolvedPaths.extensions)
    enabledSkillResources = getEnabledResources(resolvedPaths.skills)
    enabledPrompts = getEnabledPaths(resolvedPaths.prompts)
    enabledThemes  = getEnabledPaths(resolvedPaths.themes)
    enabledSkills = enabledSkillResources.map(r => mapSkillPath(r, metadataByPath))
12. CLI 路径 metadata：对 cliExtensionPaths.extensions 与 .skills 中每个未记录路径，
    写入 {source:"cli", scope:"temporary", origin:"top-level"}
13. cliEnabledExtensions/Skills/Prompts/Themes = getEnabledPaths(各自)
14. extensionPaths = (noExtensions ? cliEnabledExtensions : mergePaths(cliEnabledExtensions, enabledExtensions))
      .filter(p => !p.startsWith(BUILTIN_PATH_PREFIX) ||
                   !disabledBuiltinExtensions.has(p.slice(BUILTIN_PATH_PREFIX.length)))
15. packageWarnings = collectExtensionPackageWarnings(extensionPaths, metadataByPath)
16. extensionsResult = await loadFinalExtensionSet(extensionPaths, preTrustExtensions)
17. mergeExtensionWarnings(extensionsResult, [...installWarnings, ...packageWarnings])
18. 对每个 additionalExtensionPaths 且 isLocalPath(p)：resolved = resolveResourcePath(p)；
    !existsSync → extensionsResult.errors.push({path:resolved, error:`Extension path does not exist: ${resolved}`})
19. this.extensionsResult = extensionsOverride ? extensionsOverride(extensionsResult) : extensionsResult
20. applyExtensionSourceInfo(this.extensionsResult.extensions, metadataByPath)
21. skillPaths = noSkills ? mergePaths(cliEnabledSkills, additionalSkillPaths)
                          : mergePaths([...cliEnabledSkills, ...enabledSkills], additionalSkillPaths)
    lastSkillPaths = skillPaths; updateSkillsFromPaths(skillPaths, metadataByPath)
    对每个 additionalSkillPaths 且 isLocalPath：!existsSync && 尚无同 path 诊断 →
      skillDiagnostics.push({type:"error", message:"Skill path does not exist", path:resolved})
22. promptPaths = noPromptTemplates ? mergePaths(cliEnabledPrompts, additionalPromptTemplatePaths)
                                    : mergePaths([...cliEnabledPrompts, ...enabledPrompts], additionalPromptTemplatePaths)
    lastPromptPaths = promptPaths; updatePromptsFromPaths(...)
    同上追加 {type:"error", message:"Prompt template path does not exist", path:resolved}
23. themePaths = noThemes ? mergePaths(cliEnabledThemes, additionalThemePaths)
                          : mergePaths([...cliEnabledThemes, ...enabledThemes], additionalThemePaths)
    lastThemePaths = themePaths; updateThemesFromPaths(...)
    同上追加 {type:"error", message:"Theme path does not exist", path:resolved}
    （**注意主题路径诊断不检查 isLocalPath**）
24. agentsFiles = { agentsFiles: noContextFiles ? [] : loadProjectContextFiles({cwd, agentDir}) }
    this.agentsFiles = (agentsFilesOverride?.(agentsFiles) ?? agentsFiles).agentsFiles
25. systemPromptSource = this.systemPromptSource ?? discoverSystemPromptFile()
    baseSystemPrompt = resolvePromptInput(systemPromptSource, "system prompt")
    this.systemPrompt = systemPromptOverride ? systemPromptOverride(baseSystemPrompt) : baseSystemPrompt
    this.systemPromptSourcePath = systemPromptSource && existsSync(systemPromptSource)
                                  ? resolvePath(systemPromptSource) : undefined
26. appendSources = this.appendSystemPromptSource ?? (discovered ? [discovered] : [])
    baseAppend = appendSources.map(s => resolvePromptInput(s, "append system prompt"))
                             .filter(s => s !== undefined)
    this.appendSystemPrompt = appendSystemPromptOverride ? ... : baseAppend
    this.appendSystemPromptSourcePaths = appendSources.filter(existsSync).map(resolvePath)
27. this.loaded = true
```

关键细节：
- **step 14 的 filter 只在最终集合上移除 disabled builtin**，`-e builtin:x` 的 CLI 路径也受影响。
- **step 21/22** 中 `enabledSkills` 已经是 `mapSkillPath` 后的（目录 → `SKILL.md`）。
- `additional*Paths` 的诊断是 **error** 级，而 package 缺失是 **warning**（installWarnings）。
- 主题诊断的 `type` 是 `"warning"`（见 `loadThemes`），而追加的 additionalThemePaths 诊断是 `"error"`。

#### 2.11.12 `loadProjectTrustExtensions()`（预信任引导）

```
settingsManager.setProjectTrusted(false)
await settingsManager.reload()
return loadCurrentExtensionSet({includeInlineFactories:true})
```
目的：预加载「用户/全局 + 临时 CLI」扩展，项目本地扩展/包被排除；项目信任结果确定后再做最终加载。

`loadCurrentExtensionSet({includeInlineFactories})`：
```
resolvedPaths = await packageManager.resolve()
cliExtensionPaths = await packageManager.resolveExtensionSources(additionalExtensionPaths, {temporary:true})
enabledExtensions = resolvedPaths.extensions.filter(enabled).map(path)
cliEnabledExtensions = cliExtensionPaths.extensions.filter(enabled).map(path)
extensionPaths = (noExtensions ? cli : mergePaths(cli, enabled)).filter(p => !p.startsWith("builtin:"))
metadataByPath = Map([...resolvedPaths.extensions, ...cliExtensionPaths.extensions].map(r => [r.path, r.metadata]))
   // 后者覆盖前者（cli 元数据覆盖同名路径）
packageWarnings / installWarnings 同上
extensionsResult = await loadExtensionsCached(extensionPaths, this.cwd, this.eventBus)
mergeExtensionWarnings(...)
if (!includeInlineFactories) return extensionsResult
inlineExtensions = await loadExtensionFactories(extensionsResult.runtime)
extensionsResult.extensions.push(...inline.extensions); extensionsResult.errors.push(...inline.errors)
replacementWarnings = []
extensionsResult.extensions = omitReplacedExtensions(extensionsResult.extensions, replacementWarnings)
mergeExtensionWarnings(extensionsResult, replacementWarnings)
return extensionsResult
```

#### 2.11.13 `loadExtensionPaths(paths, runtime?)`

```
result = await loadExtensionsCached(paths.filter(!builtin), this.cwd, this.eventBus, runtime)
for path of paths.filter(builtin):
  builtin = builtinExtensions.get(name)
  if (!builtin) { result.errors.push({path, error:`Unknown built-in extension: ${path}`}); continue }
  try {
    extension = await loadExtensionFromFactory(builtin.factory, cwd, eventBus, result.runtime, path)
    extension.hidden = true
    extension.replaceable = builtin.replaceable === true
    result.extensions.push(extension)
  } catch (e) {
    result.errors.push({path, error: e instanceof Error ? e.message : "failed to load extension"})
  }
```
`builtin:` 扩展 **hidden 恒为 true**；`replaceable` 取 `builtin.replaceable === true`。

#### 2.11.14 `loadFinalExtensionSet(extensionPaths, preTrustExtensions)`

1. `preloaded = preTrustExtensions?.extensions ?? []`
2. `preloadedByPath = Map(preloaded.filter(e => !e.path.startsWith("<inline:")).map(e => [e.resolvedPath, e]))`
3. `failedPreloadPaths = Set(preTrustExtensions?.errors.map(e => resolveExtensionLoadPath(e.path)) ?? [])`
4. `remainingPaths = extensionPaths.filter(p => !preloadedByPath.has(resolve(p)) && !failedPreloadPaths.has(resolve(p)))`
5. `remainingExtensions = await loadExtensionPaths(remainingPaths, preTrustExtensions?.runtime)`
6. `loadedByPath = Map(preloadedByPath)` + 加入 `remainingExtensions.extensions`（按 `resolvedPath`）
7. `inlineExtensions = preTrustExtensions
     ? { extensions: preloaded.filter(e => e.path.startsWith("<inline:")), errors: [] }
     : await loadExtensionFactories(remainingExtensions.runtime)`
8. `orderedExtensions = extensionPaths.map(p => loadedByPath.get(resolve(p))).filter(defined)`，
   再 `push(...inlineExtensions.extensions)`（**inline 永远排在最后**）
9. `extensionsResult = { extensions: omitReplacedExtensions(orderedExtensions, replacementWarnings),
                         errors: [...preTrust.errors ?? [], ...remaining.errors, ...inline.errors],
                         warnings: [...preTrust.warnings ?? [], ...remaining.warnings ?? []],
                         runtime: remainingExtensions.runtime }`
10. `mergeExtensionWarnings(extensionsResult, replacementWarnings)`
11. `addExtensionConflictDiagnostics(extensionsResult)`：把 `detectExtensionConflicts` 的每个冲突
    **push 到 `extensionsResult.errors`**（不是 warnings）。

`detectExtensionConflicts(extensions)`：
```
toolOwners = Map<name, extPath>; flagOwners = Map<name, extPath>
for ext of extensions:                       // 按有序数组顺序
  for toolName of ext.tools.keys():
    owner = toolOwners.get(toolName)
    if (owner && owner !== ext.path) conflicts.push({path:ext.path, message:`Tool "${toolName}" conflicts with ${owner}`})
    else toolOwners.set(toolName, ext.path)
  for flagName of ext.flags.keys():
    owner = flagOwners.get(flagName)
    if (owner && owner !== ext.path) conflicts.push({path:ext.path, message:`Flag "--${flagName}" conflicts with ${owner}`})
    else flagOwners.set(flagName, ext.path)
```
**命令冲突不检测**（只 tool 与 flag）。

#### 2.11.15 `mapSkillPath(resource, metadataByPath)`

```
if (metadata.source !== "auto" && metadata.origin !== "package") return resource.path
try { if (!statSync(resource.path).isDirectory()) return resource.path } catch { return resource.path }
skillFile = join(resource.path, "SKILL.md")
if (existsSync(skillFile)) { if (!metadataByPath.has(skillFile)) metadataByPath.set(skillFile, metadata);
                             return skillFile }
return resource.path
```
即**只有 auto 或 package 来源**的目录型技能条目会被展开成 `SKILL.md`。

#### 2.11.16 `updateSkillsFromPaths` / `updatePromptsFromPaths` / `updateThemesFromPaths`

`updateSkillsFromPaths(skillPaths, metadataByPath?)`：
```
if (noSkills && skillPaths.length === 0) skillsResult = {skills:[], diagnostics:[]}
else skillsResult = loadSkills({cwd, agentDir, skillPaths, includeDefaults:false})
resolved = skillsOverride ? skillsOverride(skillsResult) : skillsResult
this.skills = resolved.skills.map(skill => ({...skill, sourceInfo:
   findSourceInfoForPath(skill.filePath, extensionSkillSourceInfos, metadataByPath)
   ?? skill.sourceInfo
   ?? getDefaultSourceInfoForPath(skill.filePath)}))
this.skillDiagnostics = resolved.diagnostics
```

`updatePromptsFromPaths(promptPaths, metadataByPath?)`：
```
if (noPromptTemplates && promptPaths.length === 0) promptsResult = 空
else { loaded = loadPromptTemplates({cwd, agentDir, promptPaths, includeDefaults:false})
       deduped = dedupePrompts(loaded.templates)
       promptsResult = {prompts: deduped.prompts, diagnostics:[...loaded.diagnostics, ...deduped.diagnostics]} }
resolved = promptsOverride ? ... : promptsResult
this.prompts = resolved.prompts.map(p => ({...p, sourceInfo: findSourceInfoForPath(p.filePath, extensionPromptSourceInfos, metadataByPath) ?? p.sourceInfo ?? getDefaultSourceInfoForPath(p.filePath)}))
this.promptDiagnostics = resolved.diagnostics
```

`dedupePrompts(prompts)`：按 `name` 去重（首见保留）；重复时 push
```
{ type:"collision", message:`name "/${prompt.name}" collision`, path: prompt.filePath,
  collision: { resourceType:"prompt", name, winnerPath: existing.filePath, loserPath: prompt.filePath } }
```
**注意 message 里名字前有 `/`**（斜杠命令形态）。

`updateThemesFromPaths(themePaths, metadataByPath?)`：
```
if (noThemes && themePaths.length === 0) themesResult = 空
else {
  colorMode = getTerminalColorMode({...detectCapabilities(() => false),
                                     ...settingsManager.getTerminalCapabilityOverrides()})
  loaded = this.loadThemes(themePaths, /*includeDefaults*/ false, colorMode)
  deduped = dedupeThemes(loaded.themes)
  themesResult = {themes: deduped.themes, diagnostics:[...loaded.diagnostics, ...deduped.diagnostics]}
}
resolved = themesOverride ? ... : themesResult
this.themes = resolved.themes.map(theme => {
  sourcePath = theme.sourcePath
  theme.sourceInfo = sourcePath
    ? (findSourceInfoForPath(sourcePath, extensionThemeSourceInfos, metadataByPath) ?? theme.sourceInfo ?? getDefaultSourceInfoForPath(sourcePath))
    : theme.sourceInfo
  return theme                                   // 原地修改
})
this.themeDiagnostics = resolved.diagnostics
```
`getTerminalColorMode` 的第二参数 `detectCapabilities(() => false)`：**传入一个恒 false 的 hyperlink 探测**，
避免 tmux 探测（注释：「Theme construction only needs trueColor, so skip the unrelated tmux hyperlink probe.」）。

`dedupeThemes(themes)`：按 `name ?? "unnamed"` 去重；重复时
```
{ type:"collision", message:`name "${name}" collision`, path: t.sourcePath,
  collision: { resourceType:"theme", name,
               winnerPath: existing.sourcePath ?? "<builtin>",
               loserPath:  t.sourcePath ?? "<builtin>" } }
```

#### 2.11.17 主题加载

`loadThemes(paths, includeDefaults, colorMode)`：
```
if (includeDefaults) for dir of [agentDir/themes, cwd/.kiana/themes] → loadThemesFromDir
for p of paths:
  resolved = resolveResourcePath(p)
  if (!existsSync(resolved)) { diagnostics.push({type:"warning", message:"theme path does not exist", path:resolved}); continue }
  try {
    stats = statSync(resolved)
    if (stats.isDirectory()) loadThemesFromDir(...)
    else if (stats.isFile() && resolved.endsWith(".json")) loadThemeFromFile(...)
    else diagnostics.push({type:"warning", message:"theme path is not a json file", path:resolved})
  } catch (e) { diagnostics.push({type:"warning", message: e instanceof Error ? e.message : "failed to read theme path", path: resolved}) }
```
`loadThemesFromDir(dir, ...)`：不存在 → return；`readdirSync(withFileTypes)`：
符号链接 → `statSync` 判文件（失败 continue）；非文件 → skip；不以 `.json` 结尾 → skip；
否则 `loadThemeFromFile(join(dir,name))`。外层异常 →
`{type:"warning", message: e.message ?? "failed to read theme directory", path: dir}`。

`loadThemeFromFile(filePath, ...)`：
```
try { themes.push(loadThemeFromPath(filePath, colorMode)) }
catch (e) { diagnostics.push({type:"warning", message: e.message ?? "failed to load theme", path: filePath}) }
```
主题解析错误文案来自 `ca-tui` 的 `loadThemeFromPath`（`Failed to parse theme ${label}: ${error}` 等）。

#### 2.11.18 `loadExtensionFactories(runtime)`

```
for ([index, input] of extensionFactories.entries()):
  isNamed = typeof input !== "function"
  factory = isNamed ? input.factory : input
  extensionPath = `<inline:${isNamed ? input.name : index + 1}>`
  try { ext = await loadExtensionFromFactory(factory, cwd, eventBus, runtime, extensionPath)
        ext.hidden = isNamed && input.hidden
        ext.replaceable = isNamed && input.replaceable === true
        extensions.push(ext) }
  catch (e) { errors.push({path: extensionPath, error: e.message ?? "failed to load extension"}) }
```
**匿名工厂的编号是 1 基**（`index + 1`）。
`ext.hidden = isNamed && input.hidden` —— 具名时取 `input.hidden`（可能 undefined，赋值为 undefined）。

#### 2.11.19 `applyExtensionSourceInfo` 与 `findSourceInfoForPath`

`applyExtensionSourceInfo(extensions, metadataByPath)`：对每个扩展
```
ext.sourceInfo = findSourceInfoForPath(ext.path, undefined, metadataByPath) ?? getDefaultSourceInfoForPath(ext.path)
for (command of ext.commands.values()) command.sourceInfo = ext.sourceInfo
for (tool of ext.tools.values()) tool.sourceInfo = ext.sourceInfo
```

`findSourceInfoForPath(resourcePath, extraSourceInfos?, metadataByPath?)`：
1. `!resourcePath` → undefined。
2. `isSyntheticPath` → `getDefaultSourceInfoForPath(resourcePath)`。
3. `normalizedResourcePath = resolve(resourcePath)`。
4. `extraSourceInfos`：遍历（插入顺序）`resolve(sourcePath)`；若相等或以 `sourcePath + sep` 开头 →
   返回 `{...sourceInfo, path: resourcePath}`。
5. `metadataByPath`：先精确查 `get(normalizedResourcePath) ?? get(resourcePath)` → `createSourceInfo`；
   再按插入顺序遍历前缀匹配 → `createSourceInfo`。
6. 否则 undefined。

**顺序敏感**：先注册的 extra source（扩展 `extendResources` 提供的）优先于 package metadata；
package metadata 中先插入的路径优先（更宽泛的目录会吞掉后来更具体的路径）。

`getDefaultSourceInfoForPath(filePath)`：
1. `getSyntheticPathSource(filePath)` 有值 → `{path, source: 该值, scope:"temporary", origin:"top-level"}`。
2. `normalized = resolve(filePath)`。`agentRoots = [agentDir/{skills,prompts,themes,extensions}]`；
   `projectRoots = [cwd/.kiana/{skills,prompts,themes,extensions}]`。
3. 命中 agentRoot（`isUnderPath`）→ `{path, source:"local", scope:"user", origin:"top-level", baseDir: root}`。
4. 命中 projectRoot → 同上但 `scope:"project"`。
5. 否则 `{path, source:"local", scope:"temporary", origin:"top-level",
        baseDir: statSync(normalized).isDirectory() ? normalized : resolve(normalized, "..")}`
   （`statSync` 可能抛错——未捕获）。

`isUnderPath(target, root)`：`normalizedRoot = resolve(root)`；相等 → true；
否则 `target.startsWith(normalizedRoot.endsWith(sep) ? normalizedRoot : normalizedRoot + sep)`。

#### 2.11.20 `discoverSystemPromptFile` / `discoverAppendSystemPromptFile`

```
projectPath = join(cwd, CONFIG_DIR_NAME, "SYSTEM.md")
if (settingsManager.isProjectTrusted() && existsSync(projectPath)) return projectPath
globalPath = join(agentDir, "SYSTEM.md")
if (existsSync(globalPath)) return globalPath
return undefined
```
`APPEND_SYSTEM.md` 同构。

**要点**：项目 `.kiana/SYSTEM.md` 只在**项目被信任**时生效；未信任则静默回退全局。

#### 2.11.21 Rust 设计

```rust
pub struct DefaultResourceLoader {
    cwd: PathBuf, agent_dir: PathBuf,
    settings: Arc<SettingsManager>, event_bus: EventBus,
    package_manager: DefaultPackageManager,
    additional_extension_paths: Vec<String>, /* …4 组 */
    extension_factories: Vec<InlineExtension>,
    builtin_extensions: IndexMap<String, BuiltinExtension>,
    disabled_builtin_extensions: HashSet<String>,
    flags: LoaderFlags,                        // no_extensions/no_skills/…
    overrides: LoaderOverrides,                // Box<dyn Fn(...)> 7 个
    // 内部状态（reload 后填充）
    extensions_result: LoadExtensionsResult,
    skills: Vec<Skill>, skill_diagnostics: Vec<ResourceDiagnostic>,
    prompts: Vec<PromptTemplate>, prompt_diagnostics: Vec<ResourceDiagnostic>,
    themes: Vec<Theme>, theme_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<ContextFile>, system_prompt: Option<String>,
    system_prompt_source_path: Option<PathBuf>,
    append_system_prompt: Vec<String>, append_system_prompt_source_paths: Vec<PathBuf>,
    last_skill_paths: Vec<String>, last_prompt_paths: Vec<String>, last_theme_paths: Vec<String>,
    extension_source_infos: [IndexMap<String, SourceInfo>; 3],
    resource_metadata_by_path: IndexMap<String, PathMetadata>,
    loaded: bool,
}
impl DefaultResourceLoader {
    pub async fn reload(&mut self, options: Option<ResourceLoaderReloadOptions>) -> Result<(), Error>;
}
```
- Rust 中 `overrides` 用 `Option<Box<dyn Fn(...) + Send + Sync>>`；`resolveProjectTrust` 是
  `Box<dyn Fn(LoadExtensionsResult) -> BoxFuture<'static, bool>>`。
- **`reload` 必须是 `&mut self`**（TS 是原地修改）；并发调用不允许（TS 也不安全）。
- `mergePaths` 的 `canonicalizePath` 在 Rust 用 `std::fs::canonicalize().unwrap_or(path)`。
- `findSourceInfoForPath` 的「插入顺序遍历」需要 `IndexMap`，不能用 `HashMap`。
- `loadThemes` 依赖 `ca-tui` 的主题解析器；建议本单元只定义 trait `ThemeLoader` 并注入实现，
  以免把主题解析复制一遍。


---

### 2.12 `src/core/sdk.ts`（479 行）

#### 2.12.1 模块级副作用

```ts
import { Agent, setDefaultStreamFn, type ThinkingLevel } from "@earendil-works/kiana-agent-core";
setDefaultStreamFn(streamSimple);   // 模块加载即执行
```
注释原文：*Preserve the pre-0.81 fallback for extensions that construct Agent instances or invoke
low-level agent loops without supplying streamFn.* 移植时 Rust 需在 `kiana-agent-core` 提供等价的
「默认 stream 函数」全局（`OnceLock<Arc<dyn StreamFn>>`），并在本模块初始化时设置。
**副作用顺序**：本模块被 import 即生效，构造 `Agent` 时若未提供 `streamFn` 会用它。

#### 2.12.2 `CreateAgentSessionOptions`

| 字段 | 类型 | 默认 |
|---|---|---|
| `cwd` | `string?` | `options.sessionManager?.getCwd() ?? process.cwd()`，再 `resolvePath` |
| `agentDir` | `string?` | `getAgentDir()`（`~/.kiana/agent`，受 `KIANA_CODING_AGENT_DIR` 覆盖） |
| `modelRuntime` | `ModelRuntime?` | `await ModelRuntime.create({authPath, modelsPath})` |
| `model` | `Model<any>?` | 会话恢复值 → settings → 第一个可用 |
| `thinkingLevel` | `ThinkingLevel?` | 会话恢复 → per-model → 全局默认 → `"medium"`，再 clamp |
| `scopedModels` | `Array<{model, thinkingLevel?}>?` | — |
| `noTools` | `"all" \| "builtin"?` | — |
| `tools` | `string[]?` | 见 2.12.6 |
| `excludeTools` | `string[]?` | — |
| `customTools` | `ToolDefinition[]?` | — |
| `resourceLoader` | `ResourceLoader?` | 新建 `DefaultResourceLoader` 并 `await reload()` + `time("resourceLoader.reload")` |
| `sessionManager` | `SessionManager?` | `SessionManager.create(cwd, getDefaultSessionDir(cwd, agentDir))` |
| `settingsManager` | `SettingsManager?` | `SettingsManager.create(cwd, agentDir)` |
| `sessionStartEvent` | `SessionStartEvent?` | — |

`CreateAgentSessionResult`：
```ts
{ session: AgentSession; extensionsResult: LoadExtensionsResult; modelFallbackMessage?: string }
```

#### 2.12.3 再导出（模块公共表面）

```ts
export * from "./agent-session-runtime.ts";
export type { ExtensionAPI, ExtensionCommandContext, ExtensionContext, ExtensionFactory,
              InlineExtension, SlashCommandInfo, SlashCommandSource, ToolDefinition };
export type { PromptTemplate } from "./prompt-templates.ts";
export type { Skill } from "./skills.ts";
export type { Tool } from "./tools/index.ts";
export { withFileMutationQueue, createCodingTools, createReadOnlyTools, createReadTool,
         createBashTool, createEditTool, createWriteTool, createGrepTool, createFindTool,
         createLsTool, createPowerShellTool };
```

#### 2.12.4 `createAgentSession(options = {})` 完整算法

**阶段 A：基础对象**
1. `cwd = resolvePath(options.cwd ?? options.sessionManager?.getCwd() ?? process.cwd())`。
2. `agentDir = options.agentDir ? resolvePath(options.agentDir) : getAgentDir()`。
3. `authPath = options.agentDir ? join(agentDir,"auth.json") : undefined`；
   `modelsPath = options.agentDir ? join(agentDir,"models.json") : undefined`。
   **只有显式传入 `agentDir` 时才把 auth/models 路径交给 `ModelRuntime`**（否则用默认解析）。
4. `modelRuntime = options.modelRuntime ?? await ModelRuntime.create({authPath, modelsPath})`。
5. `settingsManager = options.settingsManager ?? SettingsManager.create(cwd, agentDir)`。
6. `sessionManager = options.sessionManager ?? SessionManager.create(cwd, getDefaultSessionDir(cwd, agentDir))`。
7. 若未提供 `resourceLoader`：`new DefaultResourceLoader({cwd, agentDir, settingsManager})`；
   `await reload()`；`time("resourceLoader.reload")`（`KIANA_TIMING` 打点标签 **【逐字】** `resourceLoader.reload`）。

**阶段 B：会话恢复**
8. `existingSession = sessionManager.buildSessionContext()`；
   `hasExistingSession = existingSession.messages.length > 0`；
   `hasThinkingEntry = sessionManager.getBranch().some(e => e.type === "thinking_level_change")`。
9. `sessionModel = getBranchSelection(sessionManager.getBranch(),
       (provider, modelId) => modelRuntime.getModel(provider, modelId))`。
10. 若 `!model && hasExistingSession && sessionModel`：
    `restored = modelRuntime.getModel(provider, modelId)`；
    `restored && modelRuntime.hasConfiguredAuth(restored.provider)` → `model = restored`；
    若仍无：`modelFallbackMessage = \`Could not restore model ${provider}/${modelId}\``。
11. 若仍无 `model`：`findInitialModel({scopedModels: [], isContinuing: hasExistingSession,
    defaultProvider: settingsManager.getDefaultProvider(), defaultModelId: settingsManager.getDefaultModel(),
    defaultThinkingLevel: settingsManager.getDefaultThinkingLevel(),
    modelThinkingLevels: settingsManager.getAllModelThinkingLevels(), modelRuntime })`；
    - 无模型 → `modelFallbackMessage = formatNoModelsAvailableMessage()`；
    - 有模型且已有 message → `modelFallbackMessage += \`. Using ${model.provider}/${model.id}\``。

**阶段 C：思考等级**
12. `thinkingLevel = options.thinkingLevel`。
13. `thinkingLevel === undefined && hasExistingSession` →
    `hasThinkingEntry ? existingSession.thinkingLevel : (settingsManager.getDefaultThinkingLevel() ?? "medium")`。
14. 仍 undefined 且 `model` → `settingsManager.getModelThinkingLevel(provider, id)`（真值才采用）。
15. 仍 undefined → `settingsManager.getDefaultThinkingLevel() ?? "medium"`。
16. clamp：`!model` → `"off"`；否则 `clampThinkingLevel(model, thinkingLevel)`。

**阶段 D：工具选择**
17. `toolListError = options.tools ? getToolListError(options.tools) : undefined`；
    有错 → `throw new Error(\`Invalid tools option: ${toolListError}\`)`，其中错误串来自：
    - `tool names cannot be mixed with +name or -name entries`
    - `+name and -name entries take exact tool names, not patterns: ${pattern}`
18. `defaultToolNames = options.noTools ? [] : (settingsManager.getDefaultTools() ?? ["read","bash","edit","write"])`。
19. `toolModifiers = options.tools?.some(isToolModifier) ? options.tools : undefined`。
20. `selectedToolNames = toolModifiers ? applyToolModifiers(defaultToolNames, toolModifiers) : options.tools`。
21. `allowedToolNames = toolModifiers
      ? (options.noTools === "all" ? selectedToolNames : undefined)
      : (options.tools ?? (options.noTools === "all" ? [] : undefined))`。
22. `isExcludedTool = options.excludeTools ? createToolNameMatcher(options.excludeTools) : undefined`。
23. `initialActiveToolNames = (selectedToolNames ?? defaultToolNames).filter(n => !isExcludedTool?.(n))`。

`applyToolModifiers(base, entries)`：按顺序，`+name` 仅在未存在且 name 非空时 push；`-name` 存在则 splice。

**阶段 E：convertToLlm 包装（图片屏蔽）**
24. `convertToLlmWithBlockImages(messages)`：
    - `converted = convertToLlm(messages)`；`!settingsManager.getBlockImages()` → 直接返回。
    - 对 `role === "user" | "toolResult"` 且 `content` 为数组、含 `type === "image"` 的消息：
      把每个 image 换成 `{type:"text", text:"Image reading is disabled."}`（**【逐字】**），
      并过滤**相邻重复**的该文本（仅当 `i > 0` 且前一项也是同文本 text）。
    - **动态读取设置**：每次调用都检查，故会话中修改立刻生效。

**阶段 F：CacheWarmer 与请求选项**
25. `extensionRunnerRef = {current?: ExtensionRunner}`。
26. `cacheWarmer = new CacheWarmer(modelRuntime, sessionManager,
      () => settingsManager.getCacheWarmingMode(),
      async (event) => extensionRunnerRef.current?.emitCacheWarmingDecision(event) ?? event.action)`。
27. `buildRequestOptions(options = {})`：
    ```
    providerRetrySettings = settingsManager.getProviderRetrySettings()
    httpIdleTimeoutMs = settingsManager.getHttpIdleTimeoutMs()
    effectiveTimeoutMs = httpIdleTimeoutMs === 0 ? 2147483647 : httpIdleTimeoutMs
    return {
      ...options,
      timeoutMs: options.timeoutMs ?? providerRetrySettings.timeoutMs ?? effectiveTimeoutMs,
      websocketConnectTimeoutMs: options.websocketConnectTimeoutMs ?? settingsManager.getWebSocketConnectTimeoutMs(),
      maxRetries: options.maxRetries ?? providerRetrySettings.maxRetries,
      maxRetryDelayMs: options.maxRetryDelayMs ?? providerRetrySettings.maxRetryDelayMs,
      identity: getClientIdentity(settingsManager),
      transformHeaders: async (headers) =>
        runner?.hasHandlers("before_provider_headers")
          ? runner.emitBeforeProviderHeaders(headers) : headers,
    }
    ```
    **`httpIdleTimeoutMs === 0` → `2147483647`**（约 24.8 天，等价于 JS `setTimeout` 上限）。

**阶段 G：payload / response / stream 钩子**
28. `cacheContextIsCurrent(requestModel)`：闭包捕获 `messages = agent.state.messages`（快照引用），
    返回判断函数：当前 `model.provider`/`id` 与 `requestModel` 相同、且 `messages.length <= currentMessages.length`
    且**逐项引用相等**（`messages.every((m,i) => currentMessages[i] === m)`）。
29. `transformProviderPayload(payload)`：runner 无 `before_provider_request` → 原样；否则 `emitBeforeProviderRequest`。
30. `handleProviderResponse(response)`：runner 无 `after_provider_response` → return；
    否则 `emit({type:"after_provider_response", status: response.status, headers: response.headers})`。
31. `handleProviderStreamEvent(data, model)`：runner 无 `provider_stream_event` → return；
    否则 `emit({data, type:"provider_stream_event", provider: model.provider, api: model.api, model: model.id})`。
    **形状逐字**：键顺序 `data, type, provider, api, model`。

**阶段 H：Agent 构造**
32. `new Agent({ initialState: {systemPrompt:"", model, thinkingLevel, tools: [], messages: existingSession.messages},
      convertToLlm: convertToLlmWithBlockImages,
      streamFn: async (model, context, options) => {
        requestOptions = buildRequestOptions(options);
        if (options?.sessionId === sessionManager.getSessionId())
          cacheWarmer.start({model, context, options: requestOptions}, cacheContextIsCurrent(model));
        return modelRuntime.streamSimple(model, context, requestOptions);
      },
      onPayload: transformProviderPayload, onResponse: handleProviderResponse,
      onProviderStreamEvent: handleProviderStreamEvent,
      sessionId: sessionManager.getSessionId(),
      transformContext: async (messages) => runner ? runner.emitContext(messages) : messages,
      steeringMode: settingsManager.getSteeringMode(),
      followUpMode: settingsManager.getFollowUpMode(),
      transport: settingsManager.getTransport(),
      thinkingBudgets: settingsManager.getThinkingBudgets(),
      maxRetryDelayMs: settingsManager.getProviderRetrySettings().maxRetryDelayMs })`
    注意 `initialState.tools` 恒为**空数组**（工具由 AgentSession 注入），`systemPrompt` 恒为 `""`。

**阶段 I：会话元数据落盘**
33. `hasExistingSession`：
    - 无 thinking 条目 → `sessionManager.appendThinkingLevelChange(thinkingLevel)`。
34. 否则（新会话）：有 model → `appendModelChange(provider, id)`；随后 `appendThinkingLevelChange(thinkingLevel)`。

**阶段 J：AgentSession**
35. `new AgentSession({agent, sessionManager, settingsManager, cwd, scopedModels,
      resourceLoader, customTools, modelRuntime, cacheWarmer, initialActiveToolNames,
      usesDefaultTools: (options.tools === undefined || toolModifiers !== undefined) && !options.noTools,
      defaultToolModifiers: toolModifiers, allowedToolNames, excludedToolNames,
      extensionRunnerRef, sessionStartEvent})`。
36. `extensionsResult = resourceLoader.getExtensions()`；返回 `{session, extensionsResult, modelFallbackMessage}`。

#### 2.12.5 边界与错误

- `modelFallbackMessage` 三种最终形态：
  - `Could not restore model <provider>/<id>`（随后可能被追加 `. Using <p>/<id2>`）
  - `Could not restore model <p>/<id>. Using <p2>/<id2>`
  - `formatNoModelsAvailableMessage()`（来自 `auth-guidance`，由 `ca-config-models` 定义）
- `Invalid tools option: …` 是唯一从 `createAgentSession` 同步抛出的参数校验错误。
- 若 `resourceLoader` 由外部提供，**不会**调用 `reload()`，也不会打点。

#### 2.12.6 Rust 设计

```rust
pub struct CreateAgentSessionOptions {
    pub cwd: Option<PathBuf>, pub agent_dir: Option<PathBuf>,
    pub model_runtime: Option<Arc<ModelRuntime>>,
    pub model: Option<Model>, pub thinking_level: Option<ThinkingLevel>,
    pub scoped_models: Vec<ScopedModel>, pub no_tools: Option<NoTools>,
    pub tools: Option<Vec<String>>, pub exclude_tools: Option<Vec<String>>,
    pub custom_tools: Vec<ToolDefinition>, pub resource_loader: Option<Arc<dyn ResourceLoader>>,
    pub session_manager: Option<Arc<SessionManager>>, pub settings_manager: Option<Arc<SettingsManager>>,
    pub session_start_event: Option<SessionStartEvent>,
}
pub struct CreateAgentSessionResult { pub session: AgentSession,
    pub extensions_result: LoadExtensionsResult, pub model_fallback_message: Option<String> }
pub async fn create_agent_session(options: CreateAgentSessionOptions) -> Result<CreateAgentSessionResult, Error>;
```
- `streamFn` 闭包需要 `Arc<Mutex<Option<Arc<ExtensionRunner>>>>`（对应 `extensionRunnerRef`），
  因为 runner 在 `AgentSession` 构造后才注入。
- `cacheContextIsCurrent` 用 `Arc<Vec<AgentMessage>>` 快照 + `Arc::ptr_eq` 逐项比较（Rust 无对象身份，
  需给消息加 `Arc` 包裹或用 `Arc::ptr_eq` 对 `Arc<dyn Message>`）。
- `2147483647` 必须原样（不要换成 `Duration::MAX`），因为会写进 provider 请求配置。
- `getClientIdentity(settingsManager)` **每次请求**读取（telemetry 立即生效），不能缓存到构造期。


---

### 2.13 `src/core/bug-report.ts`（375 行）

#### 2.13.1 常量

```ts
export const BUG_REPORT_CUSTOM_ENTRY_TYPE = "kiana.bug-report";   // 会话自定义条目类型
const BUG_REPORT_SCHEMA_VERSION = 1;
const REDACTED = "<redacted>";
const SENSITIVE_KEY = /(?:^|[-_])(api[-_]?key|secret|token|password|passwd|credential|authorization|cookie)(?:$|[-_])/i;
```

`isSensitiveKey(key)`：先做 camelCase → snake 转换
`key.replace(/([a-z0-9])([A-Z])/g, "$1_$2")`，再 `SENSITIVE_KEY.test(...)`。
- 因此 `apiKey` → `api_Key` → 匹配 `api[-_]?key`（不区分大小写）。
- 分隔符只认 `-` 与 `_`，**不认 `.` 或空格**：`foo.token` 不匹配。
- 要求敏感词前后是开头/结尾或 `-`/`_`：`mysecrets` 不匹配，`my_secret_x` 匹配。

#### 2.13.2 `redactUrl(value)`

```ts
const nested = /^([a-z][a-z0-9+.-]*:)([a-z][a-z0-9+.-]*:\/\/.*)$/i.exec(value);
if (nested) return `${nested[1]}${redactUrl(nested[2])}`;
try {
  const url = new URL(value);
  let changed = false;
  if (url.username || url.password) { url.username = ""; url.password = ""; changed = true; }
  for (const key of url.searchParams.keys()) {
    if (isSensitiveKey(key)) { url.searchParams.set(key, "<redacted>"); changed = true; }
  }
  return changed ? url.toString() : value;
} catch { return value; }
```
- 嵌套形式处理 `foo:https://user:pass@host`（外层非 URL 协议前缀）。
- 未变化时**返回原始字符串**（不重新序列化，保留原格式）。
- 变化时返回 WHATWG 序列化（`https://host/path?k=%3Credacted%3E`——`set` 会百分号编码 `<` `>`）。

#### 2.13.3 `redactJsonValue(value)`

```ts
if (value === undefined) return undefined;
return JSON.parse(JSON.stringify(value, (key, child) => {
  if (child !== null && child !== undefined && isSensitiveKey(key)) return "<redacted>";
  return typeof child === "string" ? redactUrl(child) : child;
}));
```
- 键名敏感 → 值变 `<redacted>`（**先于**字符串 URL 处理）。
- 所有字符串值经过 `redactUrl`。
- `undefined` 值会被 `JSON.stringify` 丢弃；`JSON.parse` 后为对象/数组/原始值。
- 循环引用会抛（未捕获）。

`redactSettings(settings)`：解构丢弃 `trackingId` 与 `deviceId`，其余 `redactJsonValue`。

#### 2.13.4 `collectEnvironment()` 形状（**逐字字段名**）

```json
{
  "version": "<VERSION>",
  "userAgent": "<getKianaUserAgent(VERSION)>",
  "runtime": "bun/<ver>" | "node/<ver>",
  "platform": "<process.platform>",
  "arch": "<process.arch>",
  "osRelease": "<os.release()>",
  "osVersion": "<os.version()>",
  "shell": "<$SHELL 的最后一段，按 / 或 \\ 切分>" | null,
  "terminal": {
    "term": "<$TERM>" | null,
    "program": "<$TERM_PROGRAM>" | null,
    "programVersion": "<$TERM_PROGRAM_VERSION>" | null,
    "colorterm": "<$COLORTERM>" | null,
    "tmux": <bool>,
    "ssh": <bool>,          // SSH_CONNECTION || SSH_CLIENT || SSH_TTY
    "ci": <bool>            // CI
  },
  "kianaEnvironmentVariables": ["KIANA_...", ...]   // 仅名字，排序
}
```
`env(name)` = `process.env[name] || null`（空串 → null）。
`kianaEnvironmentVariables`：`Object.keys(process.env).filter(startsWith("KIANA_")).sort()`（**不含值**）。

#### 2.13.5 `describeModel` / `describeProvider` / `describeExtension`

`describeModel(model)`：
```json
{ "provider", "id", "name", "api", "baseUrl": "<redactUrl>", "reasoning", "input",
  "contextWindow", "maxTokens",
  "samplingParams": <redactJsonValue> | null,
  "compat": <redactJsonValue> | null,
  "thinkingLevelMap": <对象> | null,
  "headerNames": ["<排序的 model.headers 键>"] }
```
`describeProvider(modelRuntime, provider)`：
```json
{ "id", "name", "baseUrl": "<redactUrl>" | null,
  "headerNames": [...排序],
  "authTypes": ["api_key","oauth"] 的子集（顺序：api_key 先，oauth 后）,
  "authStatus": modelRuntime.getProviderAuthStatus(id),
  "usingOAuth": modelRuntime.isUsingOAuth(id),
  "registeredByExtension": modelRuntime.getRegisteredProviderIds().includes(id) }
```
`describeExtension(extension)`：
```json
{ "path", "source": "<redactUrl(extension.sourceInfo.source)>",
  "scope": extension.sourceInfo.scope, "origin": extension.sourceInfo.origin,
  "hidden": extension.hidden === true }
```

#### 2.13.6 `collectBugReportMetadata(options)`

返回对象（**键顺序即 JSON 序列化顺序**）：
```json
{
  "schemaVersion": 1,
  "id": "<options.id ?? uuidv7()>",
  "createdAt": "<new Date().toISOString()>",
  "hint": "<trim 后的 hint 或 null>",          // options.hint?.trim() || null
  "environment": { … },
  "session": {
    "id": "<sessionId>",
    "included": <bool>, "summaryIncluded": <bool>, "messageCount": <number>,
    "cwd": "<cwd>"                              // 仅当 includeSession 为 true
  },
  "model": <describeModel> | null,
  "provider": <describeProvider> | null,        // 仅当 model 存在且能取到 provider
  "thinkingLevel": "<ThinkingLevel>",
  "extensions": [<describeExtension>...],
  "extensionErrors": [{"path","error"}...],
  "settings": { "global": <redactSettings>, "project": <redactSettings> }
}
```
`createdAt` 是 `Date.toISOString()`：`YYYY-MM-DDTHH:mm:ss.sssZ`（毫秒恒 3 位，UTC）。

#### 2.13.7 `collectBugReportDiagnostics(sessionManager, crashes = [])`

```
entries = sessionManager.getEntries()
assistant = []
assistantMessageCount = 0
for entry of entries:
  if (entry.type !== "message" || entry.message.role !== "assistant") continue
  assistantMessageCount++
  diagnostics = message.diagnostics ?? []
  if (diagnostics.length === 0 && stopReason !== "error" && stopReason !== "aborted" && !errorMessage) continue
  assistant.push({
    entryId: entry.id, timestamp: entry.timestamp,
    provider, model, api, stopReason,
    rawStopReason: …   // 仅当 !== undefined
    errorMessage: …    // 仅当 !== undefined
    diagnostics,
  })
return { schemaVersion: 1, sessionId, entryCount: entries.length, assistantMessageCount, assistant,
         crashes: crashes.map(({notified, ...record}) => record) }   // 丢弃 notified
```

#### 2.13.8 `bugReportFiles(bundle)`（**顺序固定**）

| # | name | contentType | data |
|---|---|---|---|
| 1 | `report.json` | `application/json` | `JSON.stringify(metadata, null, 2) + "\n"` |
| 2 | `diagnostics.json` | `application/json` | `JSON.stringify(diagnostics, null, 2) + "\n"` |
| 3 | `session.jsonl` | `application/x-ndjson` | `bundle.sessionJsonl`（**不加换行**；仅当 `!== undefined`） |
| 4 | `summary.md` | `text/markdown` | 无结尾 `\n` 则补一个（仅当 `!== undefined`） |

`writeBugReportArchive(bundle, filePath)` = `writeZipArchive(filePath, bugReportFiles(bundle))`
（ZIP 实现见 `src/utils/zip.ts`，属 `ca-cli` 的 utils，本单元只调用；**逐字节布局见 §4.3「ZIP 字节布局」**）。
`bugReportArchiveFileName(id)` = `kiana-bug-report-${id}.zip`。

#### 2.13.9 摘要生成

`BUG_SUMMARY_SYSTEM_PROMPT`（**逐字，含空行**）：
```
You are helping a user file a bug report about kiana, the coding agent they are talking to. You will be shown the conversation transcript. Write a report for the kiana developers describing what the user was doing and what went wrong.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the report.
```
`BUG_SUMMARY_INSTRUCTIONS`（**逐字**）：
```
Write the bug report in Markdown with these sections:

## What the user was doing
One short paragraph.

## What went wrong
Concrete description of the failure: wrong output, errors, hangs, tool failures, unexpected behavior. Quote error messages and tool output verbatim where they exist.

## Steps to reproduce
Numbered list, as specific as the transcript allows.

## Relevant details
Tool calls involved, files touched, model behavior, anything else that helps a developer reproduce or locate the problem.

Do not include file contents, secrets, or credentials from the transcript; refer to files by path only. Keep the report factual and concise.
```

`selectMessages(messages, tokenBudget)`：
```
selected = []; tokens = 0
for (index = messages.length - 1; index >= 0; index--):
  next = estimateTokens(messages[index])
  if (selected.length > 0 && tokens + next > tokenBudget) break
  selected.push(messages[index]); tokens += next
return selected.reverse()
```
**至少选 1 条**（即使第一条就超预算）。

`generateBugReportSummary(options)`：
1. `contextWindow = model.contextWindow > 0 ? model.contextWindow : 128_000`。
2. `budget = floor(contextWindow * 0.6)`；`messages = selectMessages(options.messages, budget)`。
3. `prompt` = 非 undefined 部分用 `"\n\n"` 连接：
   - 若截断：`Note: only the last ${messages.length} of ${options.messages.length} messages are shown.`
   - `` `<conversation>\n${serializeConversation(convertToLlm(messages))}\n</conversation>` ``
   - 若 `hint` 非空：`` `<user-report>\n${hint}\n</user-report>` ``
   - `BUG_SUMMARY_INSTRUCTIONS`
4. `requestOptions`：`maxTokens = min(4096, model.maxTokens > 0 ? model.maxTokens : Infinity)`；
   `signal`、`apiKey`、`headers`、`env`、`sessionId`；
   若 `model.reasoning && thinkingLevel && thinkingLevel !== "off"` → `{reasoning: thinkingLevel}`。
5. `completeSummarization(model, normalizeContext({systemPrompt: BUG_SUMMARY_SYSTEM_PROMPT,
     messages: [{role:"user", content:[{type:"text", text:prompt}], timestamp: Date.now()}]}),
     requestOptions, options.streamFn, options.retry)`。
6. 错误（**逐字**，按检查顺序）：
   - `stopReason === "aborted"` → `Bug report summary was cancelled`
   - `getSummarizationFailure(response, "Bug report summary")` 返回非空 → 抛该串
     （形如 `Bug report summary failed: <msg>` 或
      `Bug report summary failed: generation hit the token cap and the summary is incomplete`）
   - 内容含 `toolCall` 块 → `Bug report summary attempted to call a tool`
   - `contentText(content).trim()` 为空 → `Bug report summary was empty`
7. 返回 trim 后的文本。

#### 2.13.10 `src/core/bug-report-upload.ts`（37 行）

```ts
export async function uploadBugReport(bundle, options = {}): Promise<{ id: string }>
```
1. `body = new FormData()`；对 `bugReportFiles(bundle)` 每个文件
   `body.append(file.name, new Blob([file.data], {type: file.contentType}), file.name)`
   —— **字段名与文件名都是 `file.name`**；顺序同上表。
2. `fetch(new URL("/v1/bug-reports", options.gatewayUrl ?? getRadiusGatewayUrl()), {
     method: "POST",
     headers: options.token ? { Authorization: `Bearer ${options.token}` } : undefined,
     body, signal: options.signal })`。
   - 不手动设置 `Content-Type`（由 FormData 生成 multipart boundary）。
3. 解析 JSON（失败 → `null`）：
   ```ts
   { ok?: boolean; bug_report?: { id?: string }; error?: string; description?: string } | null
   ```
4. 成功条件：`response.ok && json?.ok === true && typeof json.bug_report?.id === "string"` → `{id}`。
5. 否则 `detail = json && !json.ok ? json.description || json.error : undefined`；
   抛 `` `Bug report upload failed: ${detail || response.statusText || response.status}` ``。
   注意运算符优先级与真值：`detail` 为空串时用 `statusText`，再空用数字 `status`（字符串化）。

**Rust 映射**：`reqwest::multipart::Form::new().part(name, Part::bytes(data).file_name(name).mime_str(ct))`；
`Authorization: Bearer <token>`；**不要**手动设置 Content-Type。

---

### 2.14 `src/core/crash-log.ts`（170 行）

#### 2.14.1 类型与常量

```ts
export interface CrashRecord {
  timestamp: string;   // ISO 8601（toISOString）
  version: string;     // VERSION
  kind: "uncaught_exception" | "fatal_error";
  message: string;
  stack: string | null;
  sessionFile: string | null;
  cwd: string;
  notified?: boolean;
}
const MAX_CRASH_RECORDS = 5;
const MAX_AGE = 7 * 24 * 60 * 60 * 1000;   // 604800000 ms
```
路径：`join(getAgentDir(), "crashes.json")`（`crashLogPath(agentDir = getAgentDir())`）。

#### 2.14.2 `readCrashLog(path = crashLogPath())`

```
try { records = JSON.parse(readFileSync(path, "utf8"))
      return Array.isArray(records)
        ? records.filter(r => typeof r === "object" && r !== null &&
                              typeof r.timestamp === "string" && typeof r.message === "string")
        : [] }
catch { return [] }
```
**仅校验 `timestamp` 与 `message` 是字符串**；其余字段不校验（可能缺失）。
BOM **不剥离**（`JSON.parse` 遇 BOM 会抛 → 返回 `[]`）。

#### 2.14.3 `writeCrashLog(records, path)`

```
mkdirSync(dirname(path), {recursive:true})
writeFileSync(path, `${JSON.stringify(records, null, 2)}\n`)
```
即 2 空格缩进 + **结尾换行**。

#### 2.14.4 `findExtensionStackMatches(stack, extensions)`

```
if (!stack) return []
normalizedStack = stack.split("\n").slice(1)                     // 丢掉第一行（"Error: msg"）
  .filter(line => /^\s+at\s/u.test(line))                        // 只保留 "    at ..." 帧
  .map(line => { try { return decodeURI(line) } catch { return line } })
  .join("\n").replace(/\\/g, "/")
matches = []; seen = Set
for extension of extensions:
  resolvedPath = normalizeStackPath(extension.resolvedPath)      // "\"→"/"，去尾部 "/"
  singleFilePackage = sourceInfo.origin === "package"
      && !/^(?:npm:|git:|https?:\/\/|ssh:\/\/)/u.test(sourceInfo.source)
      && /\.[cm]?[jt]s$/u.test(sourceInfo.source)
  packageRoot = sourceInfo.origin === "package" && !singleFilePackage && sourceInfo.baseDir
      ? sourceInfo.baseDir : undefined
  slashIndex = resolvedPath.lastIndexOf("/")
  directoryEntry = /\/index\.[cm]?[jt]s$/u.test(resolvedPath)
  matched = packageRoot ? stackContainsPath(normalizedStack, packageRoot, true)
          : directoryEntry && slashIndex !== -1
              ? stackContainsPath(normalizedStack, resolvedPath.slice(0, slashIndex), true)
              : stackContainsPath(normalizedStack, resolvedPath, false)
  if (!matched) continue
  label = sourceInfo.origin === "package" && sourceInfo.source ? sourceInfo.source : extension.path
  if (!seen.has(label)) { seen.add(label); matches.push(label) }
return matches
```

`normalizeStackPath(v)`：`v.replace(/\\/g,"/").replace(/\/+$/u,"")`（去**所有**尾部斜杠）。

`stackContainsPath(stack, targetPath, includeDescendants)`：
```
target = normalizeStackPath(targetPath)
if (!target || isSyntheticPath(target)) return false
caseInsensitive = /^[a-z]:\//iu.test(target)          // Windows 盘符
haystack = caseInsensitive ? stack.toLowerCase() : stack
needle   = caseInsensitive ? target.toLowerCase() : target
if (includeDescendants) return haystack.includes(`${needle}/`)
index = haystack.indexOf(needle)
while (index !== -1):
  next = haystack[index + needle.length]
  if (next === undefined || next === ":" || next === ")" || /\s/u.test(next)) return true
  index = haystack.indexOf(needle, index + needle.length)
return false
```
**边界字符**：结尾、`:`（Windows `C:\x:12:3`）、`)`、任意空白。

#### 2.14.5 `recordCrash(crash, path)`

```
try {
  record = { timestamp: new Date().toISOString(), version: VERSION, kind,
             message: error instanceof Error ? (error.message || error.name) : String(error),
             stack: error instanceof Error && error.stack ? error.stack : null,
             sessionFile: crash.sessionFile ?? null, cwd: crash.cwd }
  writeCrashLog([...readCrashLog(path), record].slice(-5), path)
  return record
} catch { return undefined }
```
- `message` 为空串时回退 `error.name`。
- **保留最后 5 条**（`slice(-5)`）。
- `notified` 不写入（新记录无该字段）。

#### 2.14.6 `takeUnnotifiedCrash(path, now = Date.now())`

```
records = readCrashLog(path)
crash = [...records].reverse()
  .find(r => !r.notified && now - Date.parse(r.timestamp) <= MAX_AGE)
if (!crash) return undefined
try { writeCrashLog(records.map(r => r.notified ? r : {...r, notified:true}), path) } catch { /* 忽略 */ }
return crash
```
- **把全部未通知记录都标记 notified**（不只是返回的那条）。
- `Date.parse` 失败 → `NaN`；`now - NaN <= MAX_AGE` 为 false → 该记录被忽略（**不会崩溃**）。
- 返回的记录是**标记前**的副本（不含 `notified` 字段，除非本来就有）。

`clearCrashLog(path)`：`rmSync(path, {force:true})`，异常忽略。

---

### 2.15 `src/core/footer-data-provider.ts`（388 行）

#### 2.15.1 `findGitPaths(cwd)`

```
dir = cwd
loop:
  gitPath = join(dir, ".git")
  if (existsSync(gitPath)):
    try {
      stat = statSync(gitPath)
      if (stat.isFile()):
        content = readFileSync(gitPath,"utf8").trim()
        if (content.startsWith("gitdir: ")):
          gitDir = resolve(dir, content.slice(8).trim())
          headPath = join(gitDir, "HEAD")
          if (!existsSync(headPath)) return null
          commonDirPath = join(gitDir, "commondir")
          commonGitDir = existsSync(commonDirPath) ? resolve(gitDir, readFileSync(commonDirPath,"utf8").trim()) : gitDir
          return {repoDir: dir, commonGitDir, headPath}
      else if (stat.isDirectory()):
        headPath = join(gitPath, "HEAD")
        if (!existsSync(headPath)) return null
        return {repoDir: dir, commonGitDir: gitPath, headPath}
    } catch { return null }
  parent = dirname(dir)
  if (parent === dir) return null
  dir = parent
```
- `.git` 文件必须**恰好**以 `gitdir: ` 开头（8 字符，含空格）。
- `.git` 文件但**不是** `gitdir:` 格式 → 继续向上找（**不**返回 null）。
- 找到 `.git` 但 HEAD 不存在 → `null`（不再向上）。

#### 2.15.2 分支解析

`resolveBranchWithGitSync(repoDir)`：
```
spawnSync("git", ["--no-optional-locks","symbolic-ref","--quiet","--short","HEAD"],
          {cwd: repoDir, encoding:"utf8", stdio:["ignore","pipe","ignore"]})
return result.status === 0 && result.stdout.trim() ? result.stdout.trim() : null
```
`resolveBranchWithGitAsync(repoDir)`：`execFile` 同 argv（`{cwd, encoding:"utf8"}`）；
`error` → null；否则 trim 后为空 → null。

`isWslEnvironment()`：`platform === "linux" && (WSL_DISTRO_NAME || WSL_INTEROP)`。
`isWindowsMountedRepoPath(repoDir)`：`/^\/mnt\/[a-z](?:\/|$)/i`。
`shouldPollGitHead(repoDir)` = 两者皆真。

#### 2.15.3 `FooterDataProvider`

**字段**：`cwd`；`WATCH_DEBOUNCE_MS = 500`（静态）；`extensionStatuses: Map<string,string>`；
`cachedBranch: string | null | undefined = undefined`；`gitPaths: GitPaths | null | undefined = undefined`；
`headWatcher`、`headWatchFilePath`、`headWatchFileListener`、`reftableWatcher`、`reftableTablesListWatcher`、
`reftableTablesListPath`、`branchChangeCallbacks: Set<() => void>`、`availableProviderCount = 0`、
`refreshTimer`、`gitWatcherRetryTimer`、`refreshInFlight = false`、`refreshPending = false`、`disposed = false`。

构造函数：`gitPaths = findGitPaths(cwd)`；`setupGitWatcher()`。

**方法**：
- `getGitBranch()`：`cachedBranch === undefined` → `resolveGitBranchSync()`（含缓存）。返回 `string|null`。
- `getExtensionStatuses()`：返回内部 Map（**引用**，非副本）。
- `onBranchChange(cb)`：加入集合并返回 `() => set.delete(cb)`。
- `setExtensionStatus(key, text)`：`undefined` → delete；否则 set。
- `clearExtensionStatuses()`、`getAvailableProviderCount()`、`setAvailableProviderCount(n)`。
- `setCwd(cwd)`：相同 → return；否则更新 cwd、清 refreshTimer、`clearGitWatchers()`、
  `cachedBranch = undefined`、`gitPaths = findGitPaths(cwd)`、`setupGitWatcher()`、`notifyBranchChange()`。
- `dispose()`：`disposed = true`、清定时器、`clearGitWatchers()`、清 callbacks。

**内部**：
- `scheduleRefresh()`：`disposed || refreshTimer` → return；`refreshInFlight` → `refreshPending = true`，return；
  否则 `setTimeout(..., 500)` → `refreshGitBranchAsync()`。
- `refreshGitBranchAsync()`：`disposed` → return；`refreshInFlight` → `refreshPending = true`，return；
  `refreshInFlight = true`；`next = await resolveGitBranchAsync()`；`disposed` → return（finally 仍复位）；
  若 `cachedBranch !== undefined && cachedBranch !== next` → 赋值 + `notifyBranchChange()` + return；
  否则仅赋值。finally：`refreshInFlight = false`；`refreshPending && !disposed` → 清标志并 `scheduleRefresh()`。
- `resolveGitBranchSync/Async()`：读 HEAD，`trim()`；
  - 以 `ref: refs/heads/` 开头 → `branch = content.slice(16)`；
    `branch === ".invalid"` → `(git symbolic-ref 结果) ?? "detached"`，，否则返回 branch。
  - 其它 → `"detached"`。
  - 异常 → `null`。
  **`slice(16)` 正好是 `"ref: refs/heads/"` 的长度**。
- `clearGitWatchers()`：关闭所有 watcher、unwatchFile 两处、清 retry timer。
- `scheduleGitWatcherRetry()`：`disposed || timer` → return；5000ms 后 `setupGitWatcher()`。
- `handleGitWatcherError()`：`clearGitWatchers()` + `scheduleGitWatcherRetry()`。
- `setupGitWatcher()`：
  1. `clearGitWatchers()`；无 gitPaths → return。
  2. `pollGitHead = shouldPollGitHead(repoDir)`。
  3. **监视 HEAD 所在目录**（不是文件）：回调 `(_event, filename)`；
     `!filename || filename === "HEAD"` → `scheduleRefresh()`。
     原因注释：git 用原子写（临时文件 + rename 覆盖 HEAD），inode 变化后对文件的 `fs.watch` 失效。
  4. `pollGitHead` → `watchFile(headPath, {interval:1000}, listener)`，比较 `mtimeMs`、`ctimeMs`、`size`。
  5. `!headWatcher && !pollGitHead` → return。
  6. reftable：`reftableDir = join(commonGitDir, "reftable")` 存在 →
     `watchWithErrorHandler(reftableDir, () => scheduleRefresh(), onError)`；若 watcher 为 null → return。
  7. `tables.list` 存在 → `watchWithErrorHandler(path, scheduleRefresh, onError)`；
     若 null → return；再 `watchFile(path, {interval: 250}, listener)`（比较同样三字段）。

`ReadonlyFooterDataProvider = Pick<FooterDataProvider, "getGitBranch"|"getExtensionStatuses"|"getAvailableProviderCount"|"onBranchChange">`。

#### 2.15.4 `src/utils/fs-watch.ts`（30 行）

```ts
export const FS_WATCH_RETRY_DELAY_MS = 5000;
export function closeWatcher(watcher): void          // try { watcher.close() } catch {}
export function watchWithErrorHandler(path, listener, onError): FSWatcher | null {
  try { const watcher = watch(path, listener); watcher.on("error", onError); return watcher }
  catch { onError(); return null }
}
```
**注意**：`onError` 在同步失败时也调用；`handleGitWatcherError` 会因此再排一次重试。

#### 2.15.5 Rust 设计

```rust
pub struct GitPaths { pub repo_dir: PathBuf, pub common_git_dir: PathBuf, pub head_path: PathBuf }
pub fn find_git_paths(cwd: &Path) -> Option<GitPaths>;
pub struct FooterDataProvider { /* cwd, git_paths, cached_branch: Option<Option<String>>,
    statuses: BTreeMap<String,String>, callbacks: Vec<Arc<dyn Fn()>>, handle: tokio::runtime::Handle, … */ }
```
- **`cached_branch` 的三态**用 `Option<Option<String>>`（`None` = 未解析）。
- watcher 用 `notify` crate（`RecommendedWatcher`）+ `notify::PollWatcher` 处理 WSL `/mnt`；
  去抖 500ms 用 `tokio::time::sleep` + 代际计数（TS 用 timer 句柄，语义等价）。
- 回调集合用 `Vec<Arc<dyn Fn() + Send + Sync>>`；`onBranchChange` 返回的取消器持 `Arc<Mutex<Vec<…>>>` + id。
- `get_extension_statuses()` TS 返回内部引用；Rust 返回 `&BTreeMap`（同语义，但需注意顺序——
  TS `Map` 是插入序，Rust 应用 `IndexMap` 才能保持顺序）。


---

### 2.16 `src/package-manager-cli.ts`（1151 行）

> 分工见 §0.3：CLI 文案与参数文法以 `ca-cli` §2.9 为准；本节重复列出本单元**必须保证的语义**
> （managed install、self-update、settings 写入、列出格式），以便单独测试。

#### 2.16.1 managed install 常量与判定

```ts
const DEFAULT_INSTALLER_API_BASE = "https://kiana.dev/api/installer/releases";
// Rust：KIANA_INSTALLER_API_BASE 优先，否则 KIANA_API_BASE_URL/api/installer/releases；
// 两者都未配置时托管更新明确报不可用。
const MANAGED_INSTALL_MARKER = "managed-install.json";
const MANAGED_RELEASE_VERSION_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
```

`getActiveManagedInstallRoot(): string | undefined`：
1. `configuredRoot = process.env.KIANA_MANAGED_INSTALL_ROOT?.trim()`；空 → `undefined`。
2. `managedRoot = resolve(configuredRoot)`；`releasesDir = canonicalizePath(join(managedRoot,"releases"))`。
3. **不把源码检出/其它安装误判为 managed**：
   `if (getCwdRelativePath(canonicalizePath(getPackageDir()), releasesDir) === undefined) return undefined`。
   即当前 `getPackageDir()` 必须**位于** `<managedRoot>/releases` 之下。
4. 读 `<managedRoot>/managed-install.json`（stripBom + JSON.parse），校验
   `kind === "kiana-managed-install" && schemaVersion === 1 && layout === "releases-v1"`；
   任何不符或读失败 → **抛** `Managed install marker is missing or invalid: ${markerPath}`。
5. 返回 `managedRoot`。

`fetchInstallerArtifact(url, label)`：
- `fetch(url, {headers: {"User-Agent": getKianaUserAgent(VERSION)}})`；
- `!response.ok` → 抛 `Could not download managed installer ${label} from ${url}: HTTP ${response.status}`；
- 返回 `await response.text()`。

`runManagedNpmCi(stageDir)`：argv **逐字**：
```
npm ci --ignore-scripts --omit=dev --include=optional --no-fund --no-audit --loglevel=error --progress=false
```
（`cwd: stageDir`，`stdio:"inherit"`）；退出码非 0 →
`npm ${args.join(" ")} exited with code ${code ?? "unknown"}`。
注释：release lockfile 钉死所有包，npm 不解析版本，故不受 `min-release-age` 影响。

`verifyManagedRelease(releaseDir, expectedVersion)`：
- `binPath = join(releaseDir, "node_modules", ".bin", process.platform === "win32" ? \`${APP_NAME}.cmd\` : APP_NAME)`；
  即文件名由**运行期常量 `APP_NAME`（"kiana"）**插值得到，不是字面量 `"kiana"`/`"kiana.cmd"`；
- `spawnProcessSync(binPath, ["--version"], {encoding:"utf8", stdio:["ignore","pipe","pipe"]})`；
- `error || status !== 0` → 抛
  `Could not verify managed Kiana ${expectedVersion}: ${reason}`，其中
  `reason = result.error?.message || result.stderr.trim() || \`exit code ${result.status ?? "unknown"}\``；
- `installedVersion !== expectedVersion` → 抛
  `Managed Kiana smoke test returned version ${installedVersion}; expected ${expectedVersion}.`

`activateManagedRelease(managedRoot, version)`：
```
tmp = join(managedRoot, `current-version.tmp.${process.pid}-${Date.now()}`)
try { writeFileSync(tmp, `${version}\n`); renameSync(tmp, currentPath) }
finally { rmSync(tmp, {force:true}) }
```
`currentPath = <managedRoot>/current-version`；内容为版本号 + `\n`。

`pruneManagedReleases(managedRoot, activeVersion)`：
- 读 `<managedRoot>/releases`（失败 → return）。
- 跳过 `entry === activeVersion`、`entry === VERSION`、不匹配 `MANAGED_RELEASE_VERSION_RE` 的条目。
- 其余 `rmSync(join(releasesRoot, entry), {force:true, recursive:true})`，异常忽略。
  注释：保留「活动版本」与「正在运行本次更新的版本」——其它会话可能仍在用，且可用于回滚。

`cleanupManagedStaging(managedRoot)`：遍历 `<managedRoot>/staging`，删除所有以 `update-` 开头的条目；异常忽略。

`cleanupManagedInstall()`（导出）：
```
try { managedRoot = getActiveManagedInstallRoot() } catch { return }
if (!managedRoot) return
try {
  releaseLock = lockfile.lockSync(join(managedRoot,"update"), {realpath:false})
  try { cleanupManagedStaging(managedRoot) } finally { releaseLock() }
} catch { /* 有活跃更新持有 staging，或不可清理 */ }
```

`runManagedSelfUpdate(managedRoot, version)`：
1. `!MANAGED_RELEASE_VERSION_RE.test(version)` → 抛 `Invalid managed release version: ${version}`。
2. `lockfile.lock(join(managedRoot,"update"), {realpath:false})`；`code === "ELOCKED"` →
   抛 `Another managed Kiana update is already running.`；其它错误原样抛。
3. `cleanupManagedStaging(managedRoot)`。
4. `installerApiBase = (process.env.KIANA_INSTALLER_API_BASE?.trim() || DEFAULT_INSTALLER_API_BASE).replace(/\/+$/, "")`；
   `releaseUrl = \`${installerApiBase}/${encodeURIComponent(version)}\``。
5. `mkdirSync(releasesRoot, {recursive:true})`；`releaseDir = <releasesRoot>/<version>`；
   若已存在 → `verifyManagedRelease` + `activateManagedRelease` + `pruneManagedReleases` + return。
6. `mkdirSync(stagingRoot)`；`stageDir = mkdtempSync(join(stagingRoot,"update-"))`。
7. **并行**抓取 `package.json` 与 `package-lock.json`（`Promise.all`），标签分别为 `"package.json"` / `"package-lock.json"`；
   写入 `stageDir`。
8. `runManagedNpmCi(stageDir)`；`verifyManagedRelease(stageDir, version)`；
   `renameSync(stageDir, releaseDir)`；`activateManagedRelease`；`pruneManagedReleases`。
9. finally：`stageDir` 存在则递归删除；`await releaseLock()`。

#### 2.16.2 参数文法 `parsePackageCommand(args)`

命令别名：`uninstall` → `remove`；`install`/`remove`/`update`/`list` 原样；其它 → `undefined`（非包命令）。

选项（`rest` 顺序解析，`for` 循环）：
| token | 命令约束 | 效果 |
|---|---|---|
| `-h` / `--help` | 所有 | `help = true` |
| `-l` / `--local` | 仅 install/remove | 否则 `invalidOption ??= arg` |
| `--self` | 仅 update | 否则 invalidOption |
| `--extensions` | 仅 update | 否则 invalidOption |
| `--models` | 仅 update | 否则 invalidOption |
| `--all` | 仅 update | 否则 invalidOption |
| `--approve` / `-a` | 所有 | `projectTrustOverride = true` |
| `--no-approve` / `-na` | 所有 | `projectTrustOverride = false` |
| `--force` | 仅 update | 否则 invalidOption |
| `--extension <value>` | 仅 update | 非 update → invalidOption；`!value \|\| value.startsWith("-")` → `missingOptionValue ??= "--extension"` 且**不消费下一个 token**（`index` 不前进，下一轮会再把它当普通 token 解析：`-` 开头 → invalidOption，否则可能成为 `source`）；已设置过 → `conflictingOptions ??= "--extension can only be provided once"` 且**仍 index++** |
| 其它 `-` 开头 | — | `invalidOption ??= arg` |
| 非 `-` 开头 | — | 首个作 `source`，其后 `invalidArgument ??= arg` |

**`invalidOption` / `invalidArgument` / `missingOptionValue` / `conflictingOptions` 都是「首次赋值胜出」**。

update 目标推导（顺序敏感）：
```
if (allFlag && (selfFlag || extensionsFlag || modelsFlag || extensionFlagSource))
  conflictingOptions ??= "--all cannot be combined with --self, --extensions, --models, or --extension"
if (allFlag && source)
  conflictingOptions ??= "--all cannot be combined with a positional source"
if (modelsFlag):
  if (selfFlag || extensionsFlag || allFlag || extensionFlagSource)
    conflictingOptions ??= "--models cannot be combined with --self, --extensions, --all, or --extension"
  if (source) conflictingOptions ??= "--models cannot be combined with a positional source"
  updateTarget = {type:"models"}
else if (extensionFlagSource):
  if (selfFlag || extensionsFlag || allFlag)
    conflictingOptions ??= "--extension cannot be combined with --self, --extensions, or --all"
  if (source) conflictingOptions ??= "--extension cannot be combined with a positional source"
  updateTarget = {type:"extensions", source: extensionFlagSource}
else if (source):
  sourceIsSelf = source === "self" || source === "kiana"
  if (sourceIsSelf) updateTarget = extensionsFlag ? {type:"all"} : {type:"self"}
  else { if (extensionsFlag || selfFlag || allFlag)
           conflictingOptions ??= "positional update targets cannot be combined with --self, --extensions, or --all"
         updateTarget = {type:"extensions", source} }
else if (allFlag)            updateTarget = {type:"all"}
else if (selfFlag && extensionsFlag) updateTarget = {type:"all"}
else if (selfFlag)           updateTarget = {type:"self"}
else if (extensionsFlag)     updateTarget = {type:"extensions"}
else { updateTarget = {type:"self"}; showExtensionsSkippedNote = true }
```

`updateTargetIncludesSelf(t)` = `t.type === "all" || t.type === "self"`。
`updateTargetIncludesExtensions(t)` = `t.type === "all" || t.type === "extensions"`。

#### 2.16.3 帮助文本（**逐字**，均以 `chalk.bold("Usage:")` 开头）

`getPackageCommandUsage`：
```
install:  kiana install <source> [-l] [--approve|--no-approve]
remove:   kiana remove <source> [-l] [--approve|--no-approve]
update:   kiana update [source|self|kiana] [--self|--extensions|--models|--all] [--extension <source>] [--approve|--no-approve] [--force]
list:     kiana list [--approve|--no-approve]
```
`CONFIG_COMMAND_USAGE = "kiana config [-l] [--approve|--no-approve]"`。

`printConfigCommandHelp()`：
```
Usage:
  kiana config [-l] [--approve|--no-approve]

Open the resource configuration TUI to enable or disable package resources.
Without -l, starts in global settings (~/.kiana/agent/settings.json).
Press Tab in the TUI to switch between global and project-local modes.

Options:
  -l, --local       Edit project overrides (.kiana/settings.json)
  -a, --approve     Trust project-local files for this command with -l
  -na, --no-approve Ignore project-local files for this command with -l
```
（末尾**有一个空行**，因为模板以 `\n` 结尾。）

`printPackageCommandHelp("install")`：
```
Usage:
  kiana install <source> [-l] [--approve|--no-approve]

Install a package and add it to settings.

Options:
  -l, --local       Install project-locally (.kiana/settings.json)
  -a, --approve     Trust project-local files for this command
  -na, --no-approve Ignore project-local files for this command

Examples:
  kiana install npm:@foo/bar
  kiana install git:github.com/user/repo
  kiana install git:git@github.com:user/repo
  kiana install https://github.com/user/repo
  kiana install ssh://git@github.com/user/repo
  kiana install ./local/path
```
`("remove")`：
```
Usage:
  kiana remove <source> [-l] [--approve|--no-approve]

Remove a package and its source from settings.
Alias: kiana uninstall <source> [-l]

Options:
  -l, --local       Remove from project settings (.kiana/settings.json)
  -a, --approve     Trust project-local files for this command
  -na, --no-approve Ignore project-local files for this command

Examples:
  kiana remove npm:@foo/bar
  kiana uninstall npm:@foo/bar
```
`("update")`：
```
Usage:
  kiana update [source|self|kiana] [--self|--extensions|--models|--all] [--extension <source>] [--approve|--no-approve] [--force]

Update kiana, installed packages, or model catalogs.

Options:
  --self                  Update kiana only (default when no target is given)
  --extensions            Update installed packages only
  --models                Refresh model catalogs only
  --all                   Update kiana and installed packages
  --extension <source>    Update one package only
  -a, --approve           Trust project-local files for this command
  -na, --no-approve       Ignore project-local files for this command
  --force                 Reinstall kiana even if the current version is latest

Short forms:
  kiana update                Update kiana only
  kiana update --all          Update kiana and all extensions
  kiana update --models       Refresh model catalogs only
  kiana update <source>       Update one package
  kiana update kiana             Update kiana only (self works as alias to kiana)
```
`("list")`：
```
Usage:
  kiana list [--approve|--no-approve]

List installed packages from user and project settings.

Options:
  -a, --approve      Trust project-local files for this command
  -na, --no-approve  Ignore project-local files for this command
```

#### 2.16.4 `handlePackageCommand(args, runtimeOptions)` 控制流

```
options = parsePackageCommand(args); if (!options) return false
if (help) { printPackageCommandHelp; return true }
if (invalidOption) {
  console.error(chalk.red(`Unknown option ${invalidOption} for "${command}".`))
  console.error(chalk.dim(`Use "${APP_NAME} --help" or "${usage}".`))
  process.exitCode = 1; return true }
if (missingOptionValue) {
  console.error(chalk.red(`Missing value for ${missingOptionValue}.`))
  console.error(chalk.dim(`Usage: ${usage}`)); exitCode = 1; return true }
if (invalidArgument) {
  console.error(chalk.red(`Unexpected argument ${invalidArgument}.`))
  console.error(chalk.dim(`Usage: ${usage}`)); exitCode = 1; return true }
if (conflictingOptions) {
  console.error(chalk.red(conflictingOptions))
  console.error(chalk.dim(`Usage: ${usage}`)); exitCode = 1; return true }
if ((install || remove) && !source) {
  console.error(chalk.red(`Missing ${command} source.`))
  console.error(chalk.dim(`Usage: ${usage}`)); exitCode = 1; return true }
if (update && updateTarget.type === "models") {
  try { await refreshModelCatalogs(getAgentDir()) }
  catch (e) { console.error(chalk.red(`Error: ${e.message ?? "Unknown model catalog refresh error"}`)); exitCode = 1 }
  return true }
```

随后：
```
cwd = process.cwd(); agentDir = getAgentDir()
writesProjectPackageConfig = (install || remove) && local
{settingsManager, projectTrustWarnings} = await createCommandSettingsManager({
   cwd, agentDir, projectTrustOverride, useSavedProjectTrustOnly: command === "update",
   extensionFactories: runtimeOptions.extensionFactories })
reportProjectTrustWarnings(projectTrustWarnings)
if (!settingsManager.isProjectTrusted() && writesProjectPackageConfig) {
  console.error(chalk.red("Project is not trusted. Use --approve to modify local package config."))
  exitCode = 1; return true }
reportSettingsErrors(settingsManager, "package command")
selfUpdateNpmCommand = settingsManager.getGlobalSettings().npmCommand
packageManager = new DefaultPackageManager({cwd, agentDir, settingsManager})
packageManager.setProgressCallback(event => { if (event.type === "start") process.stdout.write(chalk.dim(`${event.message}\n`)) })
```

**install**：`await installAndPersist(source, {local})` → `console.log(chalk.green(\`Installed ${source}\`))`。

**remove**：`removed = await removeAndPersist(...)`；`!removed` →
`console.error(chalk.red(\`No matching package found for ${source}\`))` + `exitCode = 1`；
否则 `console.log(chalk.green(\`Removed ${source}\`))`。

**list**：
```
configured = packageManager.listConfiguredPackages()
userPackages = filter(scope==="user"); projectPackages = filter(scope==="project")
if (configured.length === 0) { console.log(chalk.dim("No packages installed.")); return true }
formatPackage(pkg): display = pkg.filtered ? `${pkg.source} (filtered)` : pkg.source
                    console.log(`  ${display}`)
                    if (pkg.installedPath) console.log(chalk.dim(`    ${pkg.installedPath}`))
if (userPackages.length > 0) { console.log(chalk.bold("User packages:")); for … formatPackage }
if (projectPackages.length > 0) { if (userPackages.length > 0) console.log();
                                  console.log(chalk.bold("Project packages:")); for … }
```

**update**：
```
target = updateTarget ?? {type:"self"}
if (showExtensionsSkippedNote)
  console.log(chalk.dim(`Extensions are skipped. Run ${APP_NAME} update --extensions to update extensions.`))
if (updateTargetIncludesExtensions(target)) {
  updateSource = target.type === "extensions" ? target.source : undefined
  await packageManager.update(updateSource)
  console.log(chalk.green(updateSource ? `Updated ${updateSource}` : "Updated packages"))
}
if (updateTargetIncludesSelf(target)) {
  managedInstallRoot = getActiveManagedInstallRoot()
  if (managedInstallRoot && force) {
    console.error(chalk.red(`Managed ${APP_NAME} installations do not support --force; rerun the installer to repair this installation.`))
    exitCode = 1; return true }
  plan = await getSelfUpdatePlan(force); if (!plan.shouldRun) return true
  if (managedInstallRoot) {
    if (plan.note) printSelfUpdateNote(plan.note)
    try { console.log(chalk.dim(`Updating managed ${APP_NAME} installation...`))
          await runManagedSelfUpdate(managedInstallRoot, plan.version) }
    catch (e) { console.error(chalk.red(`Error: ${e.message ?? "Unknown managed update error"}`)); exitCode = 1; return true }
    console.log(chalk.green(`Updated ${APP_NAME} from ${VERSION} to ${plan.version}`)); return true }
  installMethod = detectInstallMethod()
  if (win32 && installMethod !== "npm" && installMethod !== "pnpm") {
    console.error(chalk.red(`${APP_NAME} self-update on Windows is only supported for npm and pnpm installs.`))
    console.error(chalk.dim(`Detected install method: ${installMethod}. Update ${APP_NAME} manually.`))
    exitCode = 1; return true }
  selfUpdateTarget = {packageName: plan.packageName, installSpec: plan.installSpec}
  selfUpdateCommand = getSelfUpdateCommand(PACKAGE_NAME, selfUpdateNpmCommand, selfUpdateTarget)
  if (!selfUpdateCommand) { printSelfUpdateUnavailable(selfUpdateNpmCommand, selfUpdateTarget); exitCode = 1; return true }
  if (plan.note) printSelfUpdateNote(plan.note)
  try { if (installMethod === "npm") prepareWindowsNpmSelfUpdate()
        await runSelfUpdate(selfUpdateCommand) }
  catch (e) { console.error(chalk.red(`Error: ${e.message ?? "Unknown package command error"}`))
              if (installMethod === "pnpm") printPnpmSelfUpdateMetadataHint()
              printSelfUpdateFallback(selfUpdateCommand); exitCode = 1; return true }
  console.log(chalk.green(`Updated ${APP_NAME} from ${VERSION} to ${plan.version}`))
  if (installMethod === "npm") {
    installerCommand = win32 ? 'powershell -c "irm https://kiana.dev/install.ps1 | iex"'
                             : "curl -fsSL https://kiana.dev/install.sh | sh"
    console.log()
    console.log(chalk.yellow(`This npm installation of ${APP_NAME} does not pin its dependencies.`))
    console.log(chalk.yellow("Run the installer to migrate to a managed installation that does:"))
    console.log()
    console.log(`  ${chalk.bold(installerCommand)}`)
  }
}
```

外层 `catch`：`console.error(chalk.red(\`Error: ${message}\`))`，`message = e.message ?? "Unknown package command error"`，
`exitCode = 1`，`return true`。

#### 2.16.5 self-update 辅助

`getSelfUpdatePlan(force)`：
```
try { latestRelease = await getLatestKianaRelease(VERSION, {retry:true}) }
catch (e) { throw new Error(`Could not determine latest ${APP_NAME} version: ${formatVersionCheckError(e)}`, {cause:e}) }
if (!latestRelease) throw new Error(`Could not determine latest ${APP_NAME} version.`)
packageName = latestRelease.packageName ?? PACKAGE_NAME
installSpec = `${packageName}@${latestRelease.version}`
if (force || packageName !== PACKAGE_NAME || isNewerPackageVersion(latestRelease.version, VERSION))
  return {packageName, installSpec, version, ...(note ? {note} : {}), shouldRun: true}
console.log(chalk.green(`${APP_NAME} is already up to date (v${VERSION})`))
return {packageName, installSpec, version: latestRelease.version, shouldRun: false}
```

`runSelfUpdate(command)`：
```
console.log(chalk.dim(`Updating ${APP_NAME} with ${command.display}...`))
for (step of command.steps ?? [command]):
  spawnProcess(step.command, step.args, {stdio:"inherit"})
  error → reject(error)
  close(code, signal):
    code === 0 → resolve
    signal 非空 → reject(`${step.display} terminated by signal ${signal}`)
    否则 reject(`${step.display} exited with code ${code ?? "unknown"}`)
```

`printSelfUpdateUnavailable(npmCommand?, updatePackageTarget = PACKAGE_NAME)`：
```
stderr: `error: ${APP_NAME} cannot self-update this installation.`
stderr: getSelfUpdateUnavailableInstruction(PACKAGE_NAME, npmCommand, updatePackageTarget)
若 process.argv[1] 存在：
  stderr: ""（空行）
  stderr: `Location of ${APP_NAME} executable: ${entrypoint}`
```
`printSelfUpdateFallback(command)`：stderr `chalk.dim(\`If this keeps failing, run this command yourself: ${command.display}\`)`。
`printPnpmSelfUpdateMetadataHint()`：
```
stderr chalk.yellow("If pnpm reports missing package versions, its cached registry metadata may be stale.")
stderr chalk.yellow(`Run \`pnpm store prune\` and retry \`${APP_NAME} update --self\`.`)
```
`printSelfUpdateNote(note)`：
```
trimmed = note.trim(); 空 → return
console.log()                                    // 空行
console.log(chalk.bold(chalk.yellow("Update note")))
try { width = max(20, process.stdout.columns ?? 80)
      lines = new Markdown(trimmed, 0, 0, SELF_UPDATE_NOTE_MARKDOWN_THEME).render(width).map(l => l.trimEnd())
      console.log(lines.join("\n")) }
catch { console.log(trimmed) }
console.log()                                    // 空行
```
`SELF_UPDATE_NOTE_MARKDOWN_THEME`（chalk 包装）：heading `bold(yellow)`、link `cyan`、linkUrl `dim`、
code `yellow`、codeBlock `dim`、codeBlockBorder `dim`、quote `dim`、quoteBorder `dim`、hr `dim`、
listBullet `yellow`、bold `bold`、italic `italic`、strikethrough `strikethrough`、underline `underline`。

`refreshModelCatalogs(agentDir)`：
```
controller = new AbortController(); timeout = setTimeout(() => controller.abort(), 15_000)
try {
  modelRuntime = await ModelRuntime.create({authPath: join(agentDir,"auth.json"),
      modelsPath: join(agentDir,"models.json"), allowModelNetwork:false, signal: controller.signal})
  result = await modelRuntime.refresh({allowNetwork:true, force:true, signal: controller.signal})
  if (result.aborted) throw new Error("Model catalog refresh timed out.")
  if (result.errors.size > 0) {
    details = Array.from(result.errors, ([provider, error]) => `${provider}: ${error.message}`).join("; ")
    throw new Error(`Could not refresh model catalogs: ${details}`) }
} finally { clearTimeout(timeout) }
console.log(chalk.green("Model catalogs refreshed"))
```

#### 2.16.6 settings 与 trust 辅助

`createCommandSettingsManager({cwd, agentDir, projectTrustOverride, useSavedProjectTrustOnly, extensionFactories})`：
```
settingsManager = SettingsManager.create(cwd, agentDir, {projectTrusted:false})
projectTrustWarnings = []
trustStore = new ProjectTrustStore(agentDir)
if (useSavedProjectTrustOnly) {
  saved = trustStore.get(cwd) === true
  settingsManager.setProjectTrusted(projectTrustOverride ?? saved)
  return {settingsManager, projectTrustWarnings} }
appMode = (stdin.isTTY && stdout.isTTY) ? "interactive" : "print"
extensionsResult = (projectTrustOverride === undefined && hasTrustRequiringProjectResources(cwd))
  ? await new DefaultResourceLoader({cwd, agentDir, settingsManager, extensionFactories}).loadProjectTrustExtensions()
  : undefined
for (error of extensionsResult?.errors ?? [])
  projectTrustWarnings.push(`Failed to load extension "${error.path}": ${error.error}`)
projectTrusted = await resolveProjectTrusted({cwd, trustStore, trustOverride: projectTrustOverride,
  defaultProjectTrust: settingsManager.getDefaultProjectTrust(), extensionsResult,
  projectTrustContext: createProjectTrustContext({cwd, mode: appMode, settingsManager,
                                                 hasUI: appMode === "interactive"}),
  onExtensionError: message => projectTrustWarnings.push(message)})
settingsManager.setProjectTrusted(projectTrusted)
return {settingsManager, projectTrustWarnings}
```

`reportProjectTrustWarnings(warnings)`：每条 `console.error(chalk.yellow(\`Warning: ${warning}\`))`。
`reportSettingsErrors(settingsManager, context)`：对 `drainErrors()` 每条
`console.error(chalk.yellow(\`Warning (${context}, ${scope} settings): ${error.message}\`))`；
有 `stack` 再 `console.error(chalk.dim(error.stack))`。
`context` 取值：`"config command"` / `"package command"`。

#### 2.16.7 `handleConfigCommand(args, runtimeOptions)`

```
[command, ...rest] = args; if (command !== "config") return false
if (rest 含 "-h" 或 "--help") { printConfigCommandHelp(); return true }
local = false; projectTrustOverride = undefined
for arg of rest:
  "-l"/"--local" → local = true
  "-a"/"--approve" → projectTrustOverride = true
  "-na"/"--no-approve" → projectTrustOverride = false
  arg.startsWith("-") → console.error(chalk.red(`Unknown option ${arg} for "config".`))
                        console.error(chalk.dim(`Use "${APP_NAME} --help" or "${CONFIG_COMMAND_USAGE}".`))
                        exitCode = 1; return true
  其它 → console.error(chalk.red(`Unexpected argument ${arg}.`))
         console.error(chalk.dim(`Usage: ${CONFIG_COMMAND_USAGE}`))
         exitCode = 1; return true
cwd = process.cwd(); agentDir = getAgentDir()
{settingsManager, projectTrustWarnings} = await createCommandSettingsManager({cwd, agentDir,
    projectTrustOverride, extensionFactories})
reportProjectTrustWarnings(...)
if (local && !settingsManager.isProjectTrusted()) {
  console.error(chalk.red("Project is not trusted. Use --approve to modify local resource config."))
  exitCode = 1; return true }
reportSettingsErrors(settingsManager, "config command")
builtinExtensions = (extensionFactories ?? []).filter(isBuiltinExtension).map(input => input.name)
globalSettingsManager = SettingsManager.create(cwd, agentDir, {projectTrusted:false})
globalResolvedPaths = await new DefaultPackageManager({cwd, agentDir,
    settingsManager: globalSettingsManager, builtinExtensions}).resolve()
projectResolvedPaths = settingsManager.isProjectTrusted()
    ? await new DefaultPackageManager({cwd, agentDir, settingsManager, builtinExtensions}).resolve()
    : globalResolvedPaths
installWarnings = new Set([...global.installFailures ?? [], ...project.installFailures ?? []]
                            .map(formatPackageInstallWarning))
for warning of installWarnings: console.error(chalk.yellow(`Warning: ${warning}`))
await selectConfig({resolvedPaths: {global: globalResolvedPaths, project: projectResolvedPaths},
                    settingsManager, cwd, agentDir, writeScope: local ? "project" : "global",
                    projectModeAvailable: settingsManager.isProjectTrusted()})
process.exit(0)
```
**注意**：`handleConfigCommand` 结束调用 `process.exit(0)`（不是 return）。

#### 2.16.8 Rust 设计

```rust
pub struct PackageCommandOptions { pub command: PackageCommand, pub source: Option<String>,
  pub update_target: Option<UpdateTarget>, pub show_extensions_skipped_note: bool,
  pub local: bool, pub force: bool, pub project_trust_override: Option<bool>,
  pub help: bool, pub invalid_option: Option<String>, pub invalid_argument: Option<String>,
  pub missing_option_value: Option<String>, pub conflicting_options: Option<String> }
pub fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions>;
pub async fn handle_package_command(args: &[String], rt: &PackageCommandRuntimeOptions) -> bool;
pub async fn handle_config_command(args: &[String], rt: &PackageCommandRuntimeOptions) -> bool;
pub fn cleanup_managed_install();
```
- 解析器写成**纯函数**并配单元测试（`invalidOption ??=` 的首次赋值语义用 `Option::get_or_insert_with` 无法直接表达
  「已有值不变」——用 `if opt.is_none() { opt = Some(...) }`）。
- `chalk` 的颜色决策由 `ca-cli` 的 `chalk` 兼容层处理（`NO_COLOR`/`FORCE_COLOR`/TTY 检测），
  本单元只输出「已着色」字符串。
- `printSelfUpdateNote` 依赖 `ca-tui` 的 Markdown 渲染（宽度 = `max(20, stdout.columns ?? 80)`）；
  Rust 侧应调用同一渲染器以保证换行与 trimming 一致。
- `proper-lockfile` → `fslock`/`fd-lock`：`lockSync(dir, {realpath:false})` 对应 `<dir>.lock` 目录；
  `ELOCKED` 对应 `try_lock` 失败。

---

### 2.17 `install-lock/**`

#### 2.17.1 `install-lock/package.json`（15 行）

```json
{
	"name": "@earendil-works/kiana-coding-agent-install",
	"version": "1.4.0",
	"private": true,
	"description": "Lockfile root used by the Kiana installer and updater.",
	"dependencies": {
		"@earendil-works/kiana-coding-agent": "1.4.0"
	},
	"overrides": {
		"protobufjs": "7.6.6"
	},
	"engines": {
		"node": ">=22.19.0"
	}
}
```
（**逐字**：Tab 缩进——该仓库的 `package.json` 用 tab，而不是 2 空格；`version` 与
`dependencies[...]` **必须**等于 `coding-agent/package.json` 的 `version`。）

#### 2.17.2 `install-lock/package-lock.json`（1507 行）

- `lockfileVersion: 3`，`requires: true`。Tab 缩进，2 空格层级同样由 Tab 表达（npm 生成）。
- 头部键顺序：`name`、`version`、`lockfileVersion`、`requires`、`packages`。
- 根 `packages[""]` 与上述 `package.json` 对应（**注意它不含 `description` 与 `overrides`**，
  只含 `name`/`version`/`dependencies`/`engines`）。
- 每个依赖条目钉死 `version` + `resolved` + `integrity`（`sha512-…`）。
- **运行期 `kiana` 不读此目录**；只有 `kiana.dev` 安装器（`scripts/install.sh` / `install.ps1`）用它做 `npm ci`。
- Rust 移植：不需要搬运 lock 内容，但**必须在 CI 中保留一致性检查**（版本号与 pin 对齐），
  因为 `runManagedNpmCi` 期望 release 目录里的 `package-lock.json` 与 `package.json` 匹配。


---

## 3. 用户可见表面（逐字）

> 本节汇总所有**用户能看到**的字符串。§2 中已给出上下文者仅列条目与所在小节，避免重复。
> 颜色标注：`chalk.dim` = SGR 2；`chalk.yellow` = SGR 33；`chalk.red` = SGR 31；`chalk.green` = SGR 32；
> `chalk.bold` = SGR 1；`chalk.cyan` = SGR 36。是否着色由 chalk 的全局检测（`NO_COLOR`/`FORCE_COLOR`/TTY/
> `TERM=dumb`）决定，Rust 侧必须复用同一套检测（见 `ca-cli` 的 chalk 兼容层），否则管道输出字节不同。

### 3.1 CLI：`kiana install` / `remove` / `uninstall` / `update` / `list` / `config`

**帮助文本**：见 §2.16.3（全部 5 段，逐字）。
**错误与提示**（每条前面的流：`stderr`/`stdout`）：

| 触发 | 流 | 文本（逐字） | 颜色 |
|---|---|---|---|
| 非包命令 | — | 返回 `false`，无输出 | — |
| 未知选项 | stderr | `Unknown option ${opt} for "${command}".` | red |
| 未知选项第二行 | stderr | `Use "kiana --help" or "${usage}".` | dim |
| 缺少选项值 | stderr | `Missing value for ${opt}.` | red |
| 缺少选项值第二行 | stderr | `Usage: ${usage}` | dim |
| 多余位置参数 | stderr | `Unexpected argument ${arg}.` | red |
| 冲突选项 | stderr | 见 §2.16.2 的 5 条 `conflictingOptions` | red |
| 冲突第二行 | stderr | `Usage: ${usage}` | dim |
| install/remove 无 source | stderr | `Missing ${command} source.` | red |
| 未信任项目写本地配置 | stderr | `Project is not trusted. Use --approve to modify local package config.` | red |
| install 成功 | stdout | `Installed ${source}` | green |
| remove 未找到 | stderr | `No matching package found for ${source}` | red |
| remove 成功 | stdout | `Removed ${source}` | green |
| list 空 | stdout | `No packages installed.` | dim |
| list 用户段标题 | stdout | `User packages:` | bold |
| list 项目段标题 | stdout | `Project packages:` | bold |
| list 每项 | stdout | `  ${source}` 或 `  ${source} (filtered)` | 无色 |
| list 安装路径 | stdout | `    ${installedPath}` | dim |
| update 跳过扩展提示 | stdout | `Extensions are skipped. Run kiana update --extensions to update extensions.` | dim |
| update 扩展完成（单个） | stdout | `Updated ${updateSource}` | green |
| update 扩展完成（全部） | stdout | `Updated packages` | green |
| 进度事件（start） | stdout | `${event.message}\n` | dim |
| managed + `--force` | stderr | `Managed kiana installations do not support --force; rerun the installer to repair this installation.` | red |
| 已是最新 | stdout | `kiana is already up to date (v${VERSION})` | green |
| managed 更新开始 | stdout | `Updating managed kiana installation...` | dim |
| 更新成功 | stdout | `Updated kiana from ${VERSION} to ${version}` | green |
| Windows 非 npm/pnpm | stderr | `kiana self-update on Windows is only supported for npm and pnpm installs.` | red |
| 同上第二行 | stderr | `Detected install method: ${installMethod}. Update kiana manually.` | dim |
| 无法自更新 | stderr | `error: kiana cannot self-update this installation.`（**注意 `error: ` 前缀且无颜色**） | 无 |
| 无法自更新第二行 | stderr | `getSelfUpdateUnavailableInstruction(...)`（`ca-cli` 拥有） | 无 |
| 可执行文件位置 | stderr | 空行 + `Location of kiana executable: ${entrypoint}` | 无 |
| 更新失败回退 | stderr | `If this keeps failing, run this command yourself: ${command.display}` | dim |
| pnpm 元数据提示 | stderr | `If pnpm reports missing package versions, its cached registry metadata may be stale.` | yellow |
| pnpm 元数据提示 2 | stderr | ``Run `pnpm store prune` and retry `kiana update --self`.`` | yellow |
| 更新说明标题 | stdout | 空行 + `Update note` + 渲染的 Markdown + 空行 | bold(yellow) |
| 模型目录成功 | stdout | `Model catalogs refreshed` | green |
| 模型目录失败 | stderr | `Error: ${message}` | red |
| 通用失败 | stderr | `Error: ${message}`（`message` 缺省 `Unknown package command error` / `Unknown managed update error` / `Unknown model catalog refresh error`） | red |
| npm 安装器迁移提示 | stdout | 空行 + `This npm installation of kiana does not pin its dependencies.` + `Run the installer to migrate to a managed installation that does:` + 空行 + `  ${installerCommand}` | yellow / yellow / – / bold |
| config 未知选项 | stderr | `Unknown option ${arg} for "config".` + `Use "kiana --help" or "kiana config [-l] [--approve|--no-approve]".` | red / dim |
| config 多余参数 | stderr | `Unexpected argument ${arg}.` + `Usage: kiana config [-l] [--approve|--no-approve]` | red / dim |
| config 未信任 + `-l` | stderr | `Project is not trusted. Use --approve to modify local resource config.` | red |

`installerCommand` 二选一（**逐字**）：
```
powershell -c "irm https://kiana.dev/install.ps1 | iex"      // win32
curl -fsSL https://kiana.dev/install.sh | sh                 // 其它
```

**trust / settings 警告**（stderr，均 `chalk.yellow`）：
- `Warning: ${projectTrustWarning}`
- `Warning (${context}, ${scope} settings): ${error.message}`，`context` ∈ {`package command`, `config command`}；
  随后若有 stack：`chalk.dim(error.stack)`。

### 3.2 启动期资源警告

| 来源 | 文本 | 级别 |
|---|---|---|
| 包安装失败（横幅） | `Extension package "${source}": failed to install (${message}). ${hint}` | 启动警告 |
| 同一失败（诊断项） | `failed to install (${message}). ${hint}` | warning |
| 宿主包依赖 | 见 §2.11.2 长串 | warning |
| 内置扩展被替换 | 见 §2.11.4 长串 | warning |
| 扩展路径缺失 | `Extension path does not exist: ${resolved}` | error |
| 技能路径缺失 | `Skill path does not exist` | error（path 单列） |
| 提示模板路径缺失 | `Prompt template path does not exist` | error |
| 主题路径缺失 | `Theme path does not exist` | error |
| 未知内置扩展 | `Unknown built-in extension: ${path}` | error |
| 工具冲突 | `Tool "${toolName}" conflicts with ${existingOwner}` | error |
| 标志冲突 | `Flag "--${flagName}" conflicts with ${existingOwner}` | error |
| 主题路径不存在（package 解析） | `theme path does not exist` | warning |
| 主题不是 JSON | `theme path is not a json file` | warning |
| 主题读取失败 | `failed to read theme path` / `failed to read theme directory` / `failed to load theme` | warning |
| 系统提示读取失败 | `Warning: Could not read system prompt file ${input}: ${error}` | stderr yellow |
| 追加提示读取失败 | `Warning: Could not read append system prompt file ${input}: ${error}` | stderr yellow |
| 上下文文件读取失败 | `Warning: Could not read ${filePath}: ${error}` | stderr yellow |
| 技能 | 见 §2.6 全部诊断 | — |
| 提示模板 | 见 §2.7 | — |

### 3.3 诊断渲染（`/loaded` 与启动详情）

本单元提供数据，`ca-tui`/interactive 渲染。格式（**逐字结构**，颜色来自主题）：

```
[Skill conflicts]
  "<name>" collision:
    ✓ <path> (<source 标注>)            // theme.fg("success","✓")，整行 dim
    ✗ <path> (<source 标注>) (skipped)  // theme.fg("warning","✗")，整行 dim
  <path>                                // type=error → "error" 色，否则 "warning"
    <message>
  <message>                             // 无 path 时只有一行
```
区块标题：`[Skill conflicts]`、`[Prompt conflicts]`、`[Extension issues]`、`[Theme conflicts]`，
均为 `theme.fg("warning", …)`，每个区块后跟一个 `Spacer(1)`。
无 `path` 的诊断只输出缩进 2 空格的 message。

### 3.4 崩溃提示与通知

**启动时未通知的崩溃**（interactive，`showWarning` 一行，**逐字**）：
```
kiana crashed on ${when} (${crash.message}). Run /bug to report it; the crash details are attached automatically.
```
`when = new Date(crash.timestamp).toLocaleString()`（**本地化格式，依赖 ICU/区域设置**——Rust 需复刻
`toLocaleString()` 的默认输出，建议用 `time` + `icu` 或手工按 `en-US` 风格；见 §8 难点）。

**fatal runtime error**（TUI 内）：
- `showError(\`${prefix}: ${message}\`)`；
- 若有扩展栈匹配 → `theme.fg("warning", crashExtensionHint)`；
- 若写入了崩溃记录 → `theme.fg("muted", crashReportInstructions())`，然后 `process.exit(1)`。

`crashReportInstructions()`（**逐字**）：
```
To report this crash: run `kiana -r` to resume the session, then run /bug. The crash details are attached automatically.
To report this crash: start kiana and run /bug. The crash details are attached automatically.
```
（有 `sessionFile` 用第一行；否则用第二行。）

**uncaughtException**（终端仍存活时）：
```
stderr: `kiana exiting due to uncaughtException:`
stderr: <Error 对象（Node 的 inspect 输出）>
stderr: `\n${extensionHint}`（若有）
stderr: `\n${crashReportInstructions()}`（若已记录）
exit(1)
```

`formatCrashExtensionHint(matches)`（**逐字**）：
```
A stack frame came from loaded ${noun} ${labels}, which may be involved. Try disabling ${pronoun} with `kiana config`, or run `kiana -ne` to confirm.
```
- `matches` 过滤掉空串/非字符串；空 → `undefined`。
- `labels`：1 个 → `` `a` ``；2 个 → `` `a` and `b` ``；≥3 → `` `a`, `b`, and `c` ``（最后一项前有 `, and `）。
- `noun`：1 → `extension`，否则 `extensions`；`pronoun`：1 → `it`，否则 `them`。

**Bug 报告提示**（`suggestBugReport`）：
```
If this looks like a kiana bug, /bug sends a report to the developers.
```
（`theme.fg("muted", …)`，**只显示一次**。）

### 3.5 斜杠命令

见 §2.5 的 24 行逐字表。补充一致性缺口（**移植时必须保持现状**）：

| 命令 | `docs/slash-commands.md` | `BUILTIN_SLASH_COMMANDS` | 结论 |
|---|---|---|---|
| `/llama` | 列出 | **不在表内** | 文档过期；以代码为准 |
| `/mcp` | 列出 | **不在表内** | 内置 MCP 扩展注册（见 `extensions/mcp`），不是 `BUILTIN_SLASH_COMMANDS` 条目 |

### 3.6 页脚 git 分支

- 非 git 仓库 → `null`（页脚不显示分支段）。
- detached HEAD → **逐字** `detached`。
- reftable 且 HEAD 为 `.invalid` 且 `git symbolic-ref` 失败 → `detached`。
- 分支名原样（`refs/heads/` 之后的部分，可能含 `/`）。

### 3.7 MCP 配置错误（用户可读，来自 `kiana mcp` 与校验）

全部 `server "${name}": …` 形式，见 §2.9.7 与 §2.9.5 的两张表。`kiana mcp` 子命令的其余文案由
`extensions/mcp/cli.ts`（不在本单元）拥有。

---

## 4. 外部格式（磁盘 / 环境变量 / 协议）

> 所有 JSON 的**键顺序**都来自对象字面量构造顺序。Rust 必须用 `serde_json` + `preserve_order`
> （`IndexMap` 后端）或手写序列化，否则字节输出不同。

### 4.1 目录布局总表

| 路径 | 归属 | 说明 |
|---|---|---|
| `<agentDir>` | user | 默认 `~/.kiana/agent`；`KIANA_CODING_AGENT_DIR` 覆盖（展开 `~`） |
| `<agentDir>/settings.json` | user settings | `SettingsManager` |
| `<agentDir>/auth.json` | user auth | `ModelRuntime` |
| `<agentDir>/models.json` | user models | `ModelRuntime` |
| `<agentDir>/mcp.json` | user MCP | 全局 MCP 服务器 |
| `<agentDir>/SYSTEM.md` | user | 全局系统提示文件 |
| `<agentDir>/APPEND_SYSTEM.md` | user | 全局追加提示 |
| `<agentDir>/crashes.json` | user | 崩溃日志（最多 5 条） |
| `<agentDir>/trust.json` | user | 项目信任记录（`ca-config-models`） |
| `<agentDir>/npm/` | user 包根 | 含 `package.json`、`package-lock.json`、`node_modules/`、`.gitignore` |
| `<agentDir>/git/<host>/<path>/` | user git 包 | 浅克隆（完整 clone）；父目录含 `.gitignore` |
| `<agentDir>/tmp/extensions/<prefix>/<hash8>[/<suffix>]` | 临时包 | `prefix` ∈ {`npm`,`git-<host>`}；目录权限 0700 |
| `<agentDir>/skills/`, `/prompts/`, `/themes/`, `/extensions/` | user 资源 | 自动发现 |
| `<cwd>/.kiana/settings.json` | project settings | 仅信任时读取 |
| `<cwd>/.kiana/mcp.json` | project MCP | 覆盖全局服务器的 `enabled`/`exposure`/`toolExposure` |
| `<cwd>/.kiana/SYSTEM.md`, `.kiana/APPEND_SYSTEM.md` | project | 仅信任时生效 |
| `<cwd>/.kiana/npm/`、`<cwd>/.kiana/git/` | project 包根 | 仅信任时访问 |
| `<cwd>/.kiana/skills|prompts|themes|extensions/` | project 资源 | 仅信任时自动发现 |
| `<cwd>/.agents/skills/` 及祖先到 git 根 | project 资源 | **仅信任时**；`mode="agents"` |
| `<home>/.agents/skills/` | user 资源 | 总是 |
| `AGENTS.override.md`/`AGENTS.md`/`AGENTS.MD`/`CLAUDE.md`/`CLAUDE.MD` | 上下文文件 | 每个目录取第一个命中的 |
| `SKILL.md` | 技能根 | 阻止递归 |
| `<managedRoot>/` | managed 安装 | `KIANA_MANAGED_INSTALL_ROOT` |
| `<managedRoot>/managed-install.json` | managed 标记 | 见 4.3 |
| `<managedRoot>/current-version` | managed | 文本，版本 + `\n` |
| `<managedRoot>/current-version.tmp.<pid>-<ms>` | managed | 原子替换临时文件 |
| `<managedRoot>/releases/<version>/` | managed | 每个版本独立目录 |
| `<managedRoot>/releases/<version>/node_modules/.bin/kiana` | managed | 冒烟测试可执行 |
| `<managedRoot>/staging/update-XXXXXX/` | managed | `mkdtempSync` |
| `<managedRoot>/update.lock/` | managed | `proper-lockfile` 锁目录 |

### 4.2 由本单元写入的文件（**逐字**）

**`<包根>/package.json`**（`ensureNpmProject`，仅当不存在时写；2 空格缩进，**无结尾换行**）：
```json
{
  "name": "kiana-extensions",
  "private": true
}
```

**`<包根>/.gitignore`**（`ensureGitIgnore`，仅当不存在时写；**逐字 2 行**）：
```
*
!.gitignore
```

**git 更新标记**：`<dirname(targetDir)>/.<basename(targetDir)>.kiana-update-incomplete`（空文件）。
语义：存在 = 「reset 后尚未完成依赖安装」。`git reset --hard` 前写入，依赖安装成功后删除；
下次 `ensureGitRef` 发现 head 已等于目标时，若标记存在则执行 `clean -fdx` + 依赖安装。

**`<agentDir>/crashes.json`**（2 空格缩进 + 结尾 `\n`）：
```json
[
  {
    "timestamp": "2026-10-26T12:34:56.789Z",
    "version": "1.4.0",
    "kind": "uncaught_exception",
    "message": "boom",
    "stack": "Error: boom\n    at …",
    "sessionFile": "/path/session.jsonl",
    "cwd": "/work",
    "notified": true
  }
]
```
- `notified` 仅在 `takeUnnotifiedCrash` 标记后出现（`true`）；新记录不带该字段。
- `stack` / `sessionFile` 可为 `null`。
- 数组最多 5 项（新的追加在末尾，超出从头截断）。

**`<managedRoot>/managed-install.json`**（读取校验，写入由安装器负责）：
```json
{ "kind": "kiana-managed-install", "schemaVersion": 1, "layout": "releases-v1" }
```
附加字段被忽略；`kind`/`schemaVersion`/`layout` 三者**都必须精确匹配**。
读文件时先 `stripBom`（测试 `accepts a managed install marker with a UTF-8 BOM`）。

**`<managedRoot>/current-version`**：`"<version>\n"`（版本号 + 单个换行）。

### 4.3 Bug 报告归档

**ZIP 布局**（顺序固定；由 `writeZipArchive` 写出）：
```
kiana-bug-report-<id>.zip
├── report.json          application/json    JSON.stringify(metadata, null, 2) + "\n"
├── diagnostics.json     application/json    JSON.stringify(diagnostics, null, 2) + "\n"
├── session.jsonl        application/x-ndjson 原样（可选）
└── summary.md           text/markdown      确保结尾 "\n"（可选）
```

**`report.json` 的完整形状**（键顺序）：
```json
{
  "schemaVersion": 1,
  "id": "0199…（uuidv7）",
  "createdAt": "2026-10-26T12:34:56.789Z",
  "hint": "用户输入的描述或 null",
  "environment": {
    "version": "1.4.0",
    "userAgent": "kiana/1.4.0 (linux; node/v22.19.0; x64)",
    "runtime": "node/v22.19.0",
    "platform": "linux",
    "arch": "x64",
    "osRelease": "6.8.0-40-generic",
    "osVersion": "#40-Ubuntu SMP …",
    "shell": "bash",
    "terminal": { "term": "xterm-256color", "program": "tmux", "programVersion": "3.4",
                  "colorterm": "truecolor", "tmux": true, "ssh": false, "ci": false },
    "kianaEnvironmentVariables": ["KIANA_CODING_AGENT_DIR", "KIANA_TIMING"]
  },
  "session": { "id": "…", "included": true, "summaryIncluded": false,
               "messageCount": 42, "cwd": "/work" },
  "model": { "provider": "anthropic", "id": "claude-…", "name": "…", "api": "anthropic-messages",
             "baseUrl": "https://…", "reasoning": true, "input": ["text","image"],
             "contextWindow": 200000, "maxTokens": 8192,
             "samplingParams": null, "compat": null, "thinkingLevelMap": null,
             "headerNames": ["anthropic-beta"] },
  "provider": { "id": "anthropic", "name": "Anthropic", "baseUrl": "https://api.anthropic.com",
                "headerNames": [], "authTypes": ["api_key"], "authStatus": "configured",
                "usingOAuth": false, "registeredByExtension": false },
  "thinkingLevel": "medium",
  "extensions": [ { "path": "/abs/ext.ts", "source": "npm:foo@1.0.0", "scope": "user",
                    "origin": "package", "hidden": false } ],
  "extensionErrors": [ { "path": "/abs/bad.ts", "error": "boom" } ],
  "settings": { "global": { …redacted… }, "project": { …redacted… } }
}
```
`runtime` 的两种前缀：`bun/<ver>` 或 `node/<ver>`。
`userAgent` 由 `formatUserAgent({userAgent:"detailed", version: VERSION})` 生成：
```
kiana/<version> (<platform>; <runtime>/<runtime version>; <arch>)     // Node/Bun
kiana/<version> (browser)                                             // 其它
```
其中 `<runtime version>` 对 Node 是 `process.version`（**含 `v` 前缀**，如 `v22.19.0`）。

**`diagnostics.json`**：
```json
{
  "schemaVersion": 1,
  "sessionId": "…",
  "entryCount": 128,
  "assistantMessageCount": 40,
  "assistant": [
    { "entryId": "…", "timestamp": "…", "provider": "anthropic", "model": "claude-…",
      "api": "anthropic-messages", "stopReason": "error",
      "rawStopReason": "…（可选）", "errorMessage": "…（可选）", "diagnostics": [ … ] }
  ],
  "crashes": [ { "timestamp": "…", "version": "1.4.0", "kind": "fatal_error",
                 "message": "…", "stack": "…", "sessionFile": null, "cwd": "/work" } ]
}
```
`crashes` 中的每项都**去掉了 `notified`**。

**上传协议**（`uploadBugReport`）：
```
POST <gatewayUrl>/v1/bug-reports
Authorization: Bearer <token>          // 仅当提供 token
Content-Type: multipart/form-data; boundary=----…    // 由 FormData 生成
User-Agent: （不设置；fetch 默认）
body: multipart/form-data，4 个（或 3/2 个）part
      每个 part 的 name 与 filename 都是 file.name（report.json / diagnostics.json / session.jsonl / summary.md）
      每个 part 的 Content-Type 是对应的 contentType
响应 200 + JSON {ok:true, bug_report:{id:"…"}} → 返回 {id}
否则读 JSON（失败按 null）：{ok?, bug_report?:{id?}, error?, description?}
detail = json && !json.ok ? (json.description || json.error) : undefined
throw `Bug report upload failed: ${detail || response.statusText || response.status}`
```
`gatewayUrl` 默认 `getRadiusGatewayUrl()` = `normalizeRadiusGatewayUrl($KIANA_RADIUS_GATEWAY ?? "https://radius.kiana.dev")`
（`KIANA_RADIUS_GATEWAY` 由 `ca-config-models`/`ai` 定义）。

#### 4.3.1 ZIP 字节布局（`src/utils/zip.ts`，逐字节复刻基准）

`writeZipArchive(filePath, entries)` = `writeFile(filePath, createZipArchive(entries))`：
**异步写文件、不创建父目录**（父目录不存在时 `ENOENT` 冒泡）。
`createZipArchive` 在函数开头取**一次** `new Date()`，所有条目共享同一个 DOS 时间戳；
每个条目用 `deflateRawSync(data)`（Node zlib 默认参数：level 6、`windowBits=15`、memLevel 8、strategy 0）
与 `crc32(data)`；`name` 与字符串 `data` 均按 **UTF-8** 编码。
通用位标志固定 `0x0800`（bit 11，UTF-8 文件名），因此文件名不做任何转义或规范化。

整体顺序：**全部本地记录 → 全部中央目录记录 → EOCD**，无数据描述符（sizes 已知）、无 extra field、无注释。

**本地文件头（30 字节）**

| 偏移 | 宽度 | 值 |
|---|---|---|
| 0 | u32 | `0x04034b50` |
| 4 | u16 | 20（version needed to extract） |
| 6 | u16 | `0x0800`（flags） |
| 8 | u16 | 8（deflate） |
| 10 | u16 | `time` |
| 12 | u16 | `day` |
| 14 | u32 | crc32（对**未压缩**数据） |
| 18 | u32 | 压缩后长度 |
| 22 | u32 | 原始长度 |
| 26 | u16 | 文件名字节长度 |
| 28 | u16 | 0（extra 长度） |

紧跟 `name` 字节，再紧跟压缩数据。本地记录总长 = `30 + name.len + compressed.len`，
`offset += 30 + name.len + compressed.len`（`offset` 初始为 0，用于中央目录项的最后 4 字节）。

**中央目录项（46 字节）**

| 偏移 | 宽度 | 值 |
|---|---|---|
| 0 | u32 | `0x02014b50` |
| 4 | u16 | 20（version made by；**高字节 0 = MS-DOS 宿主**） |
| 6 | u16 | 20（version needed） |
| 8 | u16 | `0x0800` |
| 10 | u16 | 8 |
| 12 | u16 | `time` |
| 14 | u16 | `day` |
| 16 | u32 | crc32 |
| 20 | u32 | 压缩后长度 |
| 24 | u32 | 原始长度 |
| 28 | u16 | 文件名长度 |
| 30–41 | — | **全 0**：extra 长度(30)、comment 长度(32)、起始磁盘(34)、internal attrs(36)、external attrs(38) |
| 42 | u32 | 该条目**本地头**的偏移 |

紧跟 `name` 字节（本实现**不写** extra 与 comment）。

**EOCD（22 字节）**

| 偏移 | 宽度 | 值 |
|---|---|---|
| 0 | u32 | `0x06054b50` |
| 4 | u16 | 0（本磁盘号） |
| 6 | u16 | 0（中央目录起始磁盘号） |
| 8 | u16 | 条目数（本磁盘） |
| 10 | u16 | 条目数（总数） |
| 12 | u32 | 中央目录字节长度 |
| 16 | u32 | 中央目录起始偏移（= 全部本地记录总长） |
| 20 | u16 | 0（comment 长度） |

`Buffer.alloc(...)` 会零填充，因此所有未显式写入的字节都是 0（含 external attributes，即 Unix 权限位为 0）。

**DOS 日期时间（`dosDateTime`，本地时区）**
```ts
function dosDateTime(date: Date): { time: number; day: number } {
	return {
		time: (date.getHours() << 11) | (date.getMinutes() << 5) | (date.getSeconds() >> 1),
		day: ((Math.max(1980, date.getFullYear()) - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate(),
	};
}
```
- `time`：**本地时间**的 `时<<11 | 分<<5 | (秒>>1)`，秒**右移 1 位**（2 秒精度，丢弃最低位）。
- `day`：**本地** `(年-1980)<<9 | (月+1)<<5 | 日`，年份下限钳到 1980（`Math.max(1980, year)`）。
- Rust 侧必须用 `chrono::Local::now()`，**不要**用 UTC；否则跨时区生成的归档字节不同。
  逐字节比对测试必须固定 `TZ`。
- 这是归档里唯一的「本机相关」字段：**内容相同且落在同一 2 秒窗口内**的两份归档才会字节相同。
  注意 `time` 的最低位恒为 0（秒被右移），所以 `time` 只可能取偶数值。

**测试现状（已核实）**：`test/bug-report.test.ts` 只有 3 个测试（见 §7.4），**没有一个**调用
`writeBugReportArchive` / `writeZipArchive`；全仓（`packages/**/*.ts`）也没有任何测试解压或比对
归档字节——`writeBugReportArchive` 的调用方只有 `src/modes/interactive/bug-report.ts` 的 `exportZip`。
因此 Rust 侧可以：(a) 用 `zip` crate 生成合法归档（内容一致即通过现有测试），或
(b) 若要与 TS 版本**逐字节一致**（例如 CI 快照或用户比对），按上表复刻，并注意 zlib 输出差异（见 §6.2）。

### 4.4 设置文件（本单元读取的部分）

**`<agentDir>/settings.json`（global）与 `<cwd>/.kiana/settings.json`（project）**：

| 键 | 类型 | 本单元用途 |
|---|---|---|
| `packages` | `Array<string \| {source, autoload?, extensions?, skills?, prompts?, themes?}>` | 包列表 |
| `extensions` | `string[]` | 本地扩展路径 + 覆盖模式（`+`/`-`/`!`/glob） |
| `skills` | `string[] \| {enableSkillCommands?, customDirectories?}`（**对象形式整体 deprecated**） | 本地技能路径。`SettingsManager.migrateSettings` 会把对象形式**改写成数组**：`enableSkillCommands` 提升到顶层（仅当顶层未设置），`skills` 变成 `customDirectories`（数组且非空时）否则**删除该键**。Rust 侧 `SettingsManager` 已在 `ca-config-models`，本单元只需知道读到的一定是数组或不存在 |
| `prompts` | `string[]` | 本地提示模板路径 |
| `themes` | `string[]` | 本地主题路径 |
| `npmCommand` | `string[]` | npm 调用的 argv 前缀 |
| `defaultTools` | `string[]` | 工具选择（`ca-config-models` 解析，本单元消费） |
| `defaultProjectTrust` | `"ask" \| "always" \| "never"`（推断） | trust 决策 |
| `enableInstallTelemetry` | `boolean` | `getClientIdentity` |
| `blockImages` | `boolean` | `convertToLlmWithBlockImages` |
| `httpIdleTimeoutMs` | `number` | 请求 timeout |
| `websocketConnectTimeoutMs` | `number` | WS 连接 timeout |
| `providerRetry`（`timeoutMs`/`maxRetries`/`maxRetryDelayMs`） | object | 重试 |
| `steeringMode`、`followUpMode`、`transport`、`thinkingBudgets`、`cacheWarmingMode` | — | Agent 构造 |
| `modelThinkingLevels` | `Record<"provider/modelId", level>` | 思考等级 |
| `terminalCapabilityOverrides` | object | 主题颜色模式 |

`PackageSourceSchema` 的对象形式：**只有** `source`（必填）、`autoload`、`extensions`、`skills`、`prompts`、`themes`；
未知键被 schema 拒绝（settings 校验由 `ca-config-models` 负责，本单元假定已校验）。

**settings 中的路径语义**：
- 相对路径基准：user → `<agentDir>`；project → `<cwd>/.kiana`（即 settings 文件所在目录）。
- `local` 源写入 settings 时转成相对基准的路径（`rel || "."`，`relative(baseDir, resolvePath(parsed.path))`），
  **不做 `toPosix` 转换**（Windows 上写回的是反斜杠形式；`toPosixPath` 只用于 ignore/模式匹配）。
  已存在项改成同一路径时返回 `false` 且**不写盘**（见 `addSourceToSettings`）。
- 支持 `~` 展开（`resolvePath` 默认 `expandTilde: true`）；`file://` URL 会被转成路径。

### 4.5 MCP 配置（`mcp.json`）

**读取位置**：全局 `<agentDir>/mcp.json`；项目 `<cwd>/.kiana/mcp.json`（仅信任时）。
文件形状（顶层对象）：
```json
{
  "mcpServers": {
    "<name>": {
      "type": "stdio" | "http" | "streamable-http",
      "command": "npx", "args": ["-y", "pkg"], "env": {"K":"V"}, "cwd": "/path",
      "url": "https://…", "headers": {"Authorization":"Bearer …"},
      "oauth": { "clientId": "…", "clientSecret": "…", "callbackPort": 1456,
                 "callbackUrl": "http://127.0.0.1:1456/callback", "scope": "…",
                 "clientName": "…", "clientRegistration": "dcr"|"cimd",
                 "authServerMetadataUrl": "https://…" },
      "auth": {"provider": "radius"},
      "exposure": "codemode"|"deferred"|"direct"|"hidden"|"codemode-deferred",
      "toolExposure": {"tool": "direct", "browser_*": "deferred"},
      "description": "…", "enabled": true, "timeout": 60
    }
  }
}
```
（顶层键名与合并规则由 `extensions/mcp` 拥有；本单元只提供 §2.9 的 `validateMcpServerConfig` 与
`McpServerRegistry`。`extensions/mcp/cli.ts` 的文案：`No MCP servers configured. Add them to ${join(agentDir,"mcp.json")} or .kiana/mcp.json.`）

**服务器命名**：`/^[A-Za-z0-9_-]+$/`；工具命名 `mcp__<server with - → _>__<tool>`，随后
**`replace(/[^A-Za-z0-9_]/g, "_")`**（`extensions/mcp/tools.ts` 的 `createMcpToolName`，**不属于本单元**，
此处只记录以便 `mcpNamespace`/`isMcpToolName` 的调用方对齐）。`MAX_TOOL_NAME_LENGTH = 64`；名字超长
或与其它 MCP 工具重名时，追加 `_<sha256(`${server}\0${tool}`) 前 8 位 hex>` 并把主干截到
`64 - 8 - 1 = 55` 个字符。本单元只提供 `mcpNamespace` 与 `isMcpToolName` 的 `mcp__` 前缀判断。

### 4.6 环境变量（本单元读取的）

| 变量 | 读取点 | 语义 |
|---|---|---|
| `KIANA_CODING_AGENT_DIR` | `getAgentDir()` | 覆盖 `<agentDir>`；展开 `~`；**空串视为未设置** |
| `KIANA_PACKAGE_DIR` | `getPackageDir()` | 覆盖包目录（Nix/Guix） |
| `KIANA_OFFLINE` | `isOfflineModeEnabled()` | `1` / `true` / `yes`（后两者大小写不敏感）；其它值（含空串）为 false |
| `KIANA_TIMING` | `timings.ts` **模块加载时** | 仅 `"1"` 开启 |
| `KIANA_MANAGED_INSTALL_ROOT` | `getActiveManagedInstallRoot()` | 去首尾空格；空 → 未启用 |
| `KIANA_INSTALLER_API_BASE` | `runManagedSelfUpdate()` | 去首尾空格，去尾部 `/`；空 → 默认值 |
| `KIANA_TELEMETRY` | `isInstallTelemetryEnabled()` | 存在则按真值解析；不存在回退 settings |
| `KIANA_RADIUS_GATEWAY` | `getRadiusGatewayUrl()` | 网关基址 |
| `KIANA_SHARE_VIEWER_URL` | `getShareViewerUrl()` | 分享查看器基址（`ca-cli`） |
| `KIANA_SKIP_VERSION_CHECK` | 版本检查 | 见 `ca-cli` |
| `HOME` | `getHomeDir()` | `HOME \|\| homedir()` |
| `SHELL` | bug report | 取最后一段 |
| `TERM`/`TERM_PROGRAM`/`TERM_PROGRAM_VERSION`/`COLORTERM`/`TMUX`/`SSH_*`/`CI` | bug report | 见 §2.13.4 |
| `WSL_DISTRO_NAME`/`WSL_INTEROP` | `isWslEnvironment()` | WSL 判定 |
| `NO_COLOR`/`FORCE_COLOR`/`CLICOLOR*` | chalk | 着色决策 |
| `GIT_TERMINAL_PROMPT` | git remote 命令 | 强制 `"0"` |
| `KIANA_*`（全部） | bug report | **只收集名字**，排序 |

`KIANA_OFFLINE` 的完整判定（**逐字**）：
```ts
const value = process.env.KIANA_OFFLINE;
if (!value) return false;                              // 未设置或空串
return value === "1" || value.toLowerCase() === "true" || value.toLowerCase() === "yes";
```

### 4.7 进度事件（`ProgressEvent`）

```ts
{ type: "start" | "progress" | "complete" | "error",
  action: "install" | "remove" | "update" | "clone" | "pull",
  source: string,
  message?: string }
```
CLI 只在 `type === "start"` 时把 `message` 写到 stdout（dim + `\n`）。
具体消息：
- install：`Installing ${source}...`
- remove：`Removing ${source}...`
- update 单包：`Updating ${source}...`；批量：`Updating ${scope} npm packages...`
- temporary git 刷新：`Refreshing ${sourceStr}...`
- 出错：`{type:"error", action, source, message: error.message ?? String(error)}`。

---

## 5. 跨单元接口（精确类型）

> 本单元**定义**的类型（由别人消费）与**消费**的类型（由别人定义）严格分开。
> Rust 侧：`kiana-resources` crate 不能反向依赖 `kiana-coding-agent`（spec 旧名 `kiana-cli`）/`kiana-tui`；
> 被调用的接口用 trait 注入。

### 5.1 本单元导出的公开 API（消费方：`ca-cli`、`ca-session`、`ca-config-models`、`ca-tui`）

```ts
// —— source-info ——
export type SourceScope = "user" | "project" | "temporary";
export type SourceOrigin = "package" | "top-level";
export interface SourceInfo { path: string; source: string; scope: SourceScope;
                              origin: SourceOrigin; baseDir?: string }
export const BUILTIN_PATH_PREFIX = "builtin:";
export function getSyntheticPathSource(path: string): string | undefined;
export function isSyntheticPath(path: string): boolean;
export function createSourceInfo(path: string, metadata: PathMetadata): SourceInfo;
export function createSyntheticSourceInfo(path: string, options: {
  source: string; scope?: SourceScope; origin?: SourceOrigin; baseDir?: string }): SourceInfo;

// —— diagnostics ——
export interface ResourceCollision { resourceType: "extension"|"skill"|"prompt"|"theme";
  name: string; winnerPath: string; loserPath: string; winnerSource?: string; loserSource?: string }
export interface ResourceDiagnostic { type: "warning"|"error"|"collision";
  message: string; path?: string; collision?: ResourceCollision }

// —— timings ——
// 注：`TimingLabel = "main" | "extensions"` 是**非导出**的内部类型（附录 B 的 3 个导出只含函数）。
export function resetTimings(namespace?: TimingLabel): void;   // 默认 "main"
export function time(label: string, namespace?: TimingLabel): void; // 默认 "main"
export function printTimings(): void;

// —— kiana-manifest ——
export interface KianaManifest { extensions?: string[]; skills?: string[]; prompts?: string[]; themes?: string[] }
export function readKianaManifest(packageJsonPath: string): KianaManifest | null;

// —— slash-commands ——
export type SlashCommandSource = "extension" | "prompt" | "skill";
export interface SlashCommandInfo { name: string; description?: string;
  source: SlashCommandSource; sourceInfo: SourceInfo }
export interface BuiltinSlashCommand { name: string; description: string; argumentHint?: string }
export const BUILTIN_SLASH_COMMANDS: readonly BuiltinSlashCommand[];

// —— skills ——
export interface SkillFrontmatter { name?: string; description?: string;
  "disable-model-invocation"?: boolean; [key: string]: unknown }
export interface Skill { name: string; description: string; filePath: string; baseDir: string;
  sourceInfo: SourceInfo; disableModelInvocation: boolean }
export interface LoadSkillsResult { skills: Skill[]; diagnostics: ResourceDiagnostic[] }
export interface LoadSkillsFromDirOptions { dir: string; source: string }
export function loadSkillsFromDir(options: LoadSkillsFromDirOptions): LoadSkillsResult;
export interface LoadSkillsOptions { cwd: string; agentDir: string; skillPaths: string[];
  includeDefaults: boolean }                       // 四个字段**全部必填**（无 `?`）
export function loadSkills(options: LoadSkillsOptions): LoadSkillsResult;
export function formatSkillsForPrompt(skills: Skill[],
  fileReadTool?: "read" | "bash" | "indirect"): string;   // 默认 "read"；注意是 `Skill[]` 而非 `readonly Skill[]`

// —— prompt-templates ——
export interface PromptTemplate { name: string; description: string; argumentHint?: string;
  content: string; sourceInfo: SourceInfo; filePath: string }
export interface LoadPromptTemplatesOptions { cwd: string; agentDir: string;
  promptPaths: string[]; includeDefaults: boolean } // 同样全部必填
export interface LoadPromptTemplatesResult { templates: PromptTemplate[];
  diagnostics: ResourceDiagnostic[] }
export function loadPromptTemplates(options: LoadPromptTemplatesOptions): LoadPromptTemplatesResult;
export function parseCommandArgs(argsString: string): string[];
export function substituteArgs(content: string, args: string[]): string;
export function expandPromptTemplate(text: string, templates: PromptTemplate[]): string;
// 注意：`expandPromptTemplate` 的**第一个参数是整段输入文本**（如 `/review foo`），不是模板名；
// 无匹配时**原样返回 text**（返回类型 `string`，永不为 `undefined`）。旧版本此处写成
// `(name, args, templates) => string | undefined` 是错的。

// —— system-prompt ——
export interface BuildSystemPromptOptions { /* 见 §2.8 */ }
export type NormalizedBuildSystemPromptOptions = BuildSystemPromptOptions & { /* 见 §2.8.1 */ }
export type SystemPromptSections = Record<string, string>;   // **不是** { name, content } 数组
export function normalizeBuildSystemPromptOptions(
  input: BuildSystemPromptOptions): NormalizedBuildSystemPromptOptions;
export function buildSystemPromptSections(options: BuildSystemPromptOptions): SystemPromptSections;
export function buildSystemPrompt(options: BuildSystemPromptOptions): string;
export function buildSystemPromptState(options: BuildSystemPromptOptions):
  { content: string; sections?: SystemPromptSections };
export function diffSystemPromptSections(previous: Record<string, string | null>,
  current: SystemPromptSections): Record<string, string | null> | undefined;
// 旧版本此处的 `SystemPromptSection {name, content}`、`SystemPromptState {sections: SystemPromptSection[]}`、
// `SystemPromptSectionPatch` 都是**虚构类型**；`buildSystemPromptSections` 返回 `Record<string,string>`
// （每个值已含 `<name>…</name>` 包装，`preamble` 除外），`diffSystemPromptSections` 的 `previous`
// **不接受 `undefined`**（调用方从 transcript 重放，永不为 null），无变化时返回 `undefined`。

// —— mcp-servers ——
export type McpExposure = "codemode" | "deferred" | "direct" | "hidden";
export interface McpStdioServerConfig { /* 见 §2.9.4 */ }
export interface McpOAuthConfig { /* 见 §2.9.4 */ }
export interface McpHttpServerConfig { /* 见 §2.9.4 */ }
export type McpServerConfig = McpStdioServerConfig | McpHttpServerConfig;
export const LIST_MCP_RESOURCES_TOOL: "list_mcp_resources";
export const LIST_MCP_RESOURCE_TEMPLATES_TOOL: "list_mcp_resource_templates";
export const READ_MCP_RESOURCE_TOOL: "read_mcp_resource";
export function isLoopbackRedirectUri(value: string): boolean;
export function validateMcpServerConfig(name: string, raw: unknown): McpServerConfig | string;
export function getMcpToolExposure(config: McpServerConfig, toolName: string): McpExposure;
export function createToolNameMatcher(entries: readonly string[]): (name: string) => boolean;
export function isMcpToolName(name: string): boolean;
export function mcpNamespace(server: string): string;
export interface RegisteredMcpServer { name: string; config: McpServerConfig; extensionPath: string }
export class McpServerRegistry { /* 见 §2.9.8 */ }

// —— package-manager ——
export interface PathMetadata { source: string; scope: SourceScope; origin: "package"|"top-level";
  baseDir?: string; packageRoot?: string }
export interface ResolvedResource { path: string; enabled: boolean; metadata: PathMetadata }
export interface ResolvedPaths { extensions: ResolvedResource[]; skills: ResolvedResource[];
  prompts: ResolvedResource[]; themes: ResolvedResource[]; installFailures?: PackageInstallFailure[] }
export interface PackageInstallFailure { source: string; scope: SourceScope; message: string }
export type MissingSourceAction = "install" | "skip" | "error";
export interface ProgressEvent { /* 见 §4.7 */ }
export type ProgressCallback = (event: ProgressEvent) => void;
export interface PackageUpdate { source: string; displayName: string; type: "npm" | "git";
  scope: Exclude<SourceScope, "temporary"> }
export interface ConfiguredPackage { source: string; scope: "user" | "project";
  filtered: boolean; installedPath?: string }
export function getExtensionTempFolder(agentDir: string): string;
export interface PackageManager { /* 12 方法，见 §2.10.2 */ }
export class DefaultPackageManager implements PackageManager { checkForAvailableUpdates(): Promise<PackageUpdate[]> }

// —— resource-loader ——
export interface ResourceExtensionPaths { skillPaths?: Array<{path: string; metadata: PathMetadata}>;
  promptPaths?: …; themePaths?: … }
export interface ResourceLoaderReloadOptions { resolveProjectTrust?: (
  input: { extensionsResult: LoadExtensionsResult }) => Promise<boolean> }
export function formatPackageInstallWarning(failure: PackageInstallFailure): string;
export function isBuiltinExtension(input: InlineExtension): input is BuiltinExtension;
export function loadProjectContextFiles(options: { cwd: string; agentDir: string }):
  Array<{ path: string; content: string }>;
export interface DefaultResourceLoaderOptions { /* 见 §2.11.9 */ }
export interface ResourceLoader { /* 11 方法，见 §2.11.1 */ }
export class DefaultResourceLoader implements ResourceLoader { }

// —— sdk ——
export interface CreateAgentSessionOptions { /* 见 §2.12.2 */ }
export interface CreateAgentSessionResult { session: AgentSession;
  extensionsResult: LoadExtensionsResult; modelFallbackMessage?: string }
export function createAgentSession(options?: CreateAgentSessionOptions): Promise<CreateAgentSessionResult>;

// —— bug-report ——
export const BUG_REPORT_CUSTOM_ENTRY_TYPE = "kiana.bug-report";
export function redactUrl(value: string): string;
export function redactJsonValue(value: unknown): unknown;
export function collectBugReportMetadata(options): BugReportMetadata;
export function collectBugReportDiagnostics(sessionManager, crashes?): BugReportDiagnostics;
export interface BugReportBundle { metadata; diagnostics; sessionJsonl?: string; summary?: string }
export interface BugReportSessionEntryData { id: string; createdAt: string; hint: string | null;
  sessionIncluded: boolean; summaryIncluded: boolean; delivery: "zip" | "upload"; path?: string }
export function bugReportFiles(bundle): Array<{name: string; contentType: string; data: string}>;
export function writeBugReportArchive(bundle, filePath: string): Promise<void>;
export function bugReportArchiveFileName(id: string): string;
export function generateBugReportSummary(options): Promise<string>;
export function uploadBugReport(bundle, options?): Promise<{id: string}>;

// —— crash-log ——
export interface CrashRecord { timestamp: string; version: string;
  kind: "uncaught_exception"|"fatal_error"; message: string; stack: string | null;
  sessionFile: string | null; cwd: string; notified?: boolean }
export function readCrashLog(path?: string): CrashRecord[];
export function findExtensionStackMatches(stack: string | undefined,
  extensions: readonly Pick<Extension,"path"|"resolvedPath"|"sourceInfo">[]): string[];
export function recordCrash(crash: {kind: CrashRecord["kind"]; error: unknown;
  sessionFile?: string; cwd: string}, path?: string): CrashRecord | undefined;
export function takeUnnotifiedCrash(path?: string, now?: number): CrashRecord | undefined;
export function clearCrashLog(path?: string): void;

// —— footer-data-provider ——
export type GitPaths = { repoDir: string; commonGitDir: string; headPath: string };
export function findGitPaths(cwd: string): GitPaths | null;
export class FooterDataProvider { /* 见 §2.15.3 */ }
export type ReadonlyFooterDataProvider = Pick<FooterDataProvider,
  "getGitBranch"|"getExtensionStatuses"|"getAvailableProviderCount"|"onBranchChange">;

// —— package-manager-cli ——
export type PackageCommand = "install" | "remove" | "update" | "list";
export interface PackageCommandRuntimeOptions { extensionFactories?: InlineExtension[] }
export function cleanupManagedInstall(): void;
export async function handleConfigCommand(args: string[],
  runtimeOptions?: PackageCommandRuntimeOptions): Promise<boolean>;
export async function handlePackageCommand(args: string[],
  runtimeOptions?: PackageCommandRuntimeOptions): Promise<boolean>;
export function formatCrashExtensionHint(extensionMatches: readonly string[] | undefined):
  string | undefined;                                    // 定义在 interactive-mode.ts，但崩溃文案属本单元语义
```

### 5.2 本单元消费的外部接口

**`SettingsManager`（`ca-config-models`）**
```ts
static create(cwd: string, agentDir: string, options?: { projectTrusted?: boolean }): SettingsManager;
reload(): Promise<void>;
setProjectTrusted(trusted: boolean): void;
isProjectTrusted(): boolean;
// `defaultProjectTrust` 有**默认值 "ask"**，故返回类型非可选（`DefaultProjectTrust = "ask"|"always"|"never"`）。
getDefaultProjectTrust(): "ask" | "always" | "never";
getSettings(): Settings;                 // `structuredClone` 后的合并结果（本单元的 sdk 不直接调用，扩展 API 会）
getGlobalSettings(): Settings;           // `structuredClone`
getProjectSettings(): Settings;          // `structuredClone`
drainErrors(): Array<{ scope: "global"|"project"; path?: string; error: Error }>;
// `warnings` 是**对象**而非字符串数组：`WarningSettings { anthropicExtraUsage?: boolean }`。
getWarnings(): WarningSettings; setWarnings(warnings: WarningSettings): void;
setPackages(packages: PackageSource[]): void;        // package-manager 的 add/removeSourceFromSettings 用
setProjectPackages(packages: PackageSource[]): void;
getNpmCommand(): string[] | undefined;
getDefaultProvider(): string | undefined; getDefaultModel(): string | undefined;
getDefaultThinkingLevel(): ThinkingLevel | undefined;
getAllModelThinkingLevels(): Record<string, ThinkingLevel>;
getModelThinkingLevel(provider: string, modelId: string): ThinkingLevel | undefined;
getDefaultTools(): string[] | undefined;
getBlockImages(): boolean;
getHttpIdleTimeoutMs(): number;
// `websockets.connectTimeoutMs` 未设置时返回 `undefined`（不是 number！）
getWebSocketConnectTimeoutMs(): number | undefined;
// `maxRetryDelayMs` **必填**（有 SETTINGS_DEFAULTS.retry.provider.maxRetryDelayMs 兜底），
// 只有 `timeoutMs` / `maxRetries` 可为 undefined。
getProviderRetrySettings(): { timeoutMs?: number; maxRetries?: number; maxRetryDelayMs: number };
getSteeringMode(): "all" | "one-at-a-time"; getFollowUpMode(): "all" | "one-at-a-time";
getTransport(): "auto" | "sse" | "websocket" | "websocket-cached";
getThinkingBudgets(): ThinkingBudgetsSettings | undefined;
getCacheWarmingMode(): "off" | "streaming" | "idle";
getEnableInstallTelemetry(): boolean;
getTerminalCapabilityOverrides(): Partial<TerminalCapabilities>;   // 不是 Record<string, unknown>
```

**旧版本此段的 4 处错误**（已按源码 `src/core/settings-manager.ts` + `settings-schema.ts` 校正）：
`getWarnings/setWarnings` 的参数是 `WarningSettings` 对象而非 `string[]`；`getProviderRetrySettings()` 的
`maxRetryDelayMs` 是必填；`getWebSocketConnectTimeoutMs()` 可为 `undefined`；
`getDefaultProjectTrust()` 不接受 `undefined`。`getHttpIdleTimeoutMs()` 恒为 `number`
（`httpIdleTimeoutMs = 0` 表示禁用，在 `sdk.ts` 里被映射成 `2147483647`，见 §2.12.4 阶段 E）。

**`ModelRuntime`（`ca-config-models`）**
```ts
static create(options: CreateModelRuntimeOptions = {}): Promise<ModelRuntime>;
// CreateModelRuntimeOptions 实际字段（本单元只用前 4 个）：
//   credentials?: CredentialStore; authPath?: string; modelsPath?: string | null;
//   modelsStore?: ModelsStore; modelsStorePath?: string; allowModelNetwork?: boolean;
//   modelRefreshTimeoutMs?: number; catalogBaseUrl?: string; signal?: AbortSignal;
//   refreshOnCreate?: boolean;
getModel(provider: string, modelId: string): Model<any> | undefined;
hasConfiguredAuth(provider: string): boolean;
getProvider(id: string): Provider | undefined;
getProviderAuthStatus(id: string): AuthStatus; isUsingOAuth(id: string): boolean;
getRegisteredProviderIds(): readonly string[];      // 只读数组，不是 `string[]`
streamSimple(model, context, options): …;
// `force` 与 `providers` 均为可选；`refresh` 在 `options.providers` 存在时只重算这些 provider。
refresh(options?: { allowNetwork?: boolean; force?: boolean; providers?: readonly string[];
  signal?: AbortSignal }): Promise<{ aborted: boolean; errors: ReadonlyMap<string, Error> }>;
```
**注意**：`sdk.ts` 调 `ModelRuntime.create({ authPath, modelsPath })` 时，两者都可能是 `undefined`
（只有显式传 `options.agentDir` 时才设置），此时 `modelsPath === undefined` 让运行时用默认
`join(getAgentDir(), "models.json")`；**`null` 才是「不要 models.json」**（`modelsPath === null` →
`InMemoryCodingAgentModelsStore`）。Rust 侧必须保留 `Option<Option<PathBuf>>` 的三态。

**`SessionManager`（`ca-session`）**
```ts
static create(cwd: string, sessionDir?: string, options?: NewSessionOptions): SessionManager;
static inMemory(cwd?: string, options?: NewSessionOptions, entries?: FileEntry[]): SessionManager;
getCwd(): string; getSessionId(): string; getSessionFile(): string | undefined;
// 实际形状 `SessionContext { messages: AgentMessage[]; thinkingLevel: string;
//                            model: { provider: string; modelId: string } | null }`。
// `thinkingLevel` 是 **`string`**（不是 `ThinkingLevel`）且**非可选**（无思考记录时为默认等级）；
// 还有一个 `model` 字段，本单元只用 `messages` / `thinkingLevel`（旧版本漏了 `model` 且把
// `thinkingLevel` 写成可选，会误导 Rust 侧用 `Option`）。
buildSessionContext(): { messages: AgentMessage[]; thinkingLevel: string;
  model: { provider: string; modelId: string } | null };
getBranch(fromId?: string): SessionEntry[];      // 有可选起点参数
getEntries(): SessionEntry[];
appendModelChange(provider: string, modelId: string): string;   // 返回新 entry id
appendThinkingLevelChange(thinkingLevel: string): string;       // 返回新 entry id
```
`getDefaultSessionDir(cwd, agentDir?)` 属于 `ca-session`：路径为
`<agentDir>/sessions/--<cwd 去掉开头 / 或 \，再把 / \ : 换成 - >--`，**并会 `mkdirSync(recursive: true)`**。
因此 `createAgentSession()` 在**未传 `sessionManager`** 时会创建该目录；只有调用方显式传入
`SessionManager.inMemory()` 才完全不落盘。Rust 移植若把 `getDefaultSessionDir` 做成纯函数（不建目录），
会与 TS 的「默认启动即建目录」不同——必须在 §8.6 记录或以副作用实现。

**`AgentSession` / `Agent`（`ca-session`、`kiana-agent-core`）**
```ts
new Agent({ initialState, convertToLlm, streamFn, onPayload, onResponse,
  onProviderStreamEvent, sessionId, transformContext, steeringMode, followUpMode,
  transport, thinkingBudgets, maxRetryDelayMs });
new AgentSession(config: AgentSessionConfig);
```
`AgentSessionConfig` 字段见 §2.12.4 阶段 J；本单元传 `extensionRunnerRef` 为可变引用。

**`ExtensionRunner`（`ca-session`）**
```ts
hasHandlers(event: string): boolean;
emit(event): Promise<void>;
emitContext(messages): Promise<AgentMessage[]>;
emitBeforeProviderHeaders(headers): Promise<ProviderHeaders>;
emitBeforeProviderRequest(payload: unknown): Promise<unknown>;
emitCacheWarmingDecision(event): Promise<CacheWarmingDecisionEventResult["action"]>;
```
`after_provider_response` 事件形状：`{type, status, headers}`。
`provider_stream_event` 事件形状：`{data, type, provider, api, model}`。

**扩展加载器（`ca-session`：`core/extensions/loader.ts`）**
```ts
createExtensionRuntime(): ExtensionRuntime;
loadExtensionsCached(paths: string[], cwd: string, eventBus: EventBus,
  runtime?: ExtensionRuntime): Promise<LoadExtensionsResult>;
loadExtensionFromFactory(factory: ExtensionFactory, cwd: string, eventBus: EventBus,
  runtime: ExtensionRuntime, extensionPath: string): Promise<Extension>;
clearExtensionCache(): void;
```

**扩展类型（`ca-session`）**
```ts
export interface Extension { path: string; resolvedPath: string; hidden?: boolean;
  replaceable?: boolean; sourceInfo: SourceInfo;
  handlers: Map<string, HandlerFn[]>; tools: Map<string, RegisteredTool>;
  messageRenderers: Map<string, MessageRenderer>; toolRenderers?: ToolRendererResolver[];
  markdownTransformer?: MarkdownTransformer; entryRenderers?: Map<string, EntryRenderer>;
  commands: Map<string, RegisteredCommand>; flags: Map<string, ExtensionFlag>;
  shortcuts: Map<KeyId, ExtensionShortcut> }
export type InlineExtension = ExtensionFactory | { name: string; factory: ExtensionFactory;
  hidden?: boolean; replaceable?: boolean; builtin?: boolean };
export interface LoadExtensionsResult { extensions: Extension[];
  errors: Array<{path: string; error: string}>;
  warnings?: Array<{path: string; warning: string}>; runtime: ExtensionRuntime }
```
`ExtensionFactory` = `(api: ExtensionAPI) => void | Promise<void>`。

**`kiana-ai` / `kiana-agent-core`**
```ts
convertToLlm(messages: AgentMessage[]): Message[];
estimateTokens(message: AgentMessage): number;
serializeConversation(messages: Message[]): string;
normalizeContext(ctx): Context;
contentText(content): string;
completeSummarization(model, context, options, streamFn?, retry?): Promise<AssistantMessage>;
getSummarizationFailure(response, label): string | undefined;
clampThinkingLevel(model, level): string;
getBranchSelection(branch, getModel): { provider: string; modelId: string } | undefined;
findInitialModel(options): Promise<{ model?: Model<any> }>;
formatNoModelsAvailableMessage(): string;
formatUserAgent(identity): string;
uuidv7(): string;
streamSimple / setDefaultStreamFn;
```

**`kiana-tui`**
```ts
detectCapabilities(hyperlinkProbe: () => boolean): TerminalCapabilities;
getTerminalColorMode(caps): TerminalColorMode;
loadThemeFromPath(path: string, colorMode: TerminalColorMode): Theme;
class Markdown { constructor(text, paddingX, paddingY, theme: MarkdownTheme); render(width): string[] }
class Theme { fg(color: ThemeColor, text: string): string; … }
```

**`ca-cli` 工具**
```ts
isStdoutTakenOver(): boolean;             // package-manager.ts 的 spawnCommand 使用
spawnProcess(cmd, args, options): ChildProcess;
spawnProcessSync(cmd, args, options): SpawnSyncResult;
waitForChildProcess(child): Promise<number | null>;
getPackageDir(): string; getAgentDir(): string; detectInstallMethod(): InstallMethod;
getSelfUpdateCommand(...): SelfUpdateCommand | undefined;
getSelfUpdateUnavailableInstruction(...): string;
resolveProjectTrusted(...): Promise<boolean>; hasTrustRequiringProjectResources(cwd): boolean;
class ProjectTrustStore { get(cwd): boolean | null | undefined }
selectConfig(options): Promise<void>;
writeZipArchive(path, files: readonly { name: string; data: string | Uint8Array }[]): Promise<void>;
                                           // 语义与字节布局见 §4.3.1；不创建父目录
```

**`utils/paths.ts`**
```ts
export interface PathInputOptions { trim?: boolean; expandTilde?: boolean; homeDir?: string;
  stripAtPrefix?: boolean; normalizeUnicodeSpaces?: boolean }
canonicalizePath(path: string): string;
isLocalPath(value: string): boolean;
normalizePath(input: string, options?: PathInputOptions): string;
resolvePath(input: string, baseDir?: string, options?: PathInputOptions): string;
getCwdRelativePath(filePath: string, cwd: string): string | undefined;
formatPathRelativeToCwdOrAbsolute(filePath: string, cwd: string): string;  // 本单元不用（read 渲染器用）
markPathIgnoredByCloudSync(path: string): void;
normalizeWindowsShellPath(filePath: string): string;
getFileRevision(path: string): string | undefined;
```
**默认值**：`resolvePath` 的 `baseDir` 默认 `process.cwd()`、`PathInputOptions` 默认
`{ expandTilde: true }`（其余 false）；`normalizePath` 的 options 默认 `{}`。
`normalizePath` 的**处理顺序**是 trim → unicode 空格归一 → 去 `@` 前缀 →（win32）`normalizeWindowsShellPath`
→ `~` 展开（`~` 单独 → home；`~/`（win32 还有 `~\`）→ `join(home, rest)`）→ **`/^file:\/\//` → `fileURLToPath`**。
因此「`file://` 转路径」发生在 `normalizePath` 里（`isLocalPath` 也把 `file:` 视为 local），
`package-manager.ts` 的 `normalizePackageSourceForSettings` 只是 `relative(baseDir, resolvePath(parsed.path))`，
**没有**自己的 `toPosix` 转换（旧版 §4.4 写「使用 `toPosix`（Windows 反斜杠转 `/`）」是**错的**——
`toPosixPath` 只用于 ignore 规则与模式匹配，不用于写回 settings 的 local 路径；
写回的是平台原生的 `relative()` 结果，`rel || "."`）。
`markPathIgnoredByCloudSync`：**macOS** 用 `xattr -w com.dropbox.ignored 1` + `com.apple.fileprovider.ignore#P`，
**Linux** 用 `setfattr -n user.com.dropbox.ignored -v 1`，其它平台空操作；`spawnProcessSync` 的失败被静默
（`stdio: "ignore"` 但**没有** try/catch——进程不存在时 `spawnSync` 返回 `error` 而不抛，故实际静默）。

**`utils/frontmatter.ts`**
```ts
// 实际签名（泛型 + 抛错）：
export const parseFrontmatter: <T extends Record<string, unknown> = Record<string, unknown>>(
  content: string) => { frontmatter: T; body: string };
export const stripFrontmatter: (content: string) => string;   // = parseFrontmatter(content).body
```
**旧版本写成 `{ attributes, body, error? }` 是错的**：字段名是 `frontmatter`（不是 `attributes`），
**没有** `error` 字段；YAML 语法错误时 `yaml.parse` **抛异常**（`skills.ts` / `prompt-templates.ts`
各自 `try/catch`，且对散装 `.md` 的技能**静默**）。另外：`body` 已 `trim()` 两端空白
（`slice(endIndex + 4).trim()`），无 frontmatter 时 `body` 也经过 `stripBom` + 换行归一化。
`parse` 返回 `null`（空 YAML）→ `{}`。

**`utils/text.ts`**
```ts
// 实际返回**对象**，不是元组；旧版本 `[string, string]` 是错的。
splitBom(content: string): { bom: string; text: string };
stripBom(content: string): string;   // = splitBom(content).text
```
`splitBom` 只看**单个**前导 `U+FEFF`；`{bom: "﻿", text: content.slice(1)}`，否则 `{bom: "", text: content}`。

**`utils/fs-watch.ts`**
```ts
closeWatcher(watcher: FSWatcher | null | undefined): void;   // null/undefined 与 close 抛错都静默
watchWithErrorHandler(path: string, listener: WatchListener<string>,
  onError: () => void): FSWatcher | null;
const FS_WATCH_RETRY_DELAY_MS = 5000;
```
`watchWithErrorHandler` 的**同步抛错**也会调用 `onError()` 并返回 `null`（`fs.watch` 对不存在的路径会抛）；
`watcher.on("error", onError)` 是**异步**错误路径。两者都要算作「watcher 失败」，
`footer-data-provider` 的 5s 重试闭环依赖这一点。

**`utils/child-process.ts`**（`package-manager.ts` 与 `package-manager-cli.ts` 都用）
```ts
spawnProcess(command, args, options): ChildProcess;      // win32 走 cross-spawn，其它 node spawn
spawnProcessSync(command, args, options): SpawnSyncReturns<string>;
waitForChildProcess(child: ChildProcess): Promise<number | null>;   // 见下
```
`waitForChildProcess` 的语义（`EXIT_STDIO_GRACE_MS = 100`）：等 `exit`（拿到 code）后**不立即 resolve**，
而是等 stdout/stderr 两个管道 `end`；每个 `data` chunk 都**重置** 100ms 的 grace 计时器，
计时器到期仍未 end 也 resolve（防止 Windows 上被继承句柄永不 `close` 卡死）。
`error` → reject；resolve 前 `destroy()` 两个管道。本单元用它的地方只有 `package-manager-cli.ts` 的
`runManagedNpmCi`（`stdio:"inherit"`，`stdout/stderr` 为 null → 立即 resolve）。
**不要**用 `child.wait()` 简化实现：`bash` 工具依赖「exit 后再把在写的管道读尽」的行为。

**`utils/git.ts`**（见 §2.10.20）导出的类型 `GitSource = { type: "git"; repo: string; host: string;
path: string; ref: string | undefined; pinned: boolean }` 与 `parseGitUrl(source: string): GitSource | null`
也属于 `ca-cli`，但 `package-manager.ts` 直接依赖其行为。

**`utils/zip.ts`** 的 `ZipEntry` 是**非导出**接口：`{ name: string; data: string | Uint8Array }`；
`writeZipArchive` 是唯一导出（`kiana-user-agent.ts` 的 `getKianaUserAgent(version)` 同理，
= `formatUserAgent({ userAgent: "detailed", version })`）。

### 5.3 依赖单元一览

| 单元 | 被本单元使用的内容 |
|---|---|
| `ca-config-models` | `SettingsManager`、`ModelRuntime`、`settings-schema`、`auth-guidance`、`client-identity`、`telemetry`、`radius`、`project-trust`、`trust-manager` |
| `ca-session` | `AgentSession`、`Agent`（经 kiana-agent-core）、`SessionManager`、扩展加载器与类型、`EventBus`、`messages.convertToLlm`、`compaction`、`cache-warmer`、`model-resolver`、`virtual-models`、`tools/*` |
| `ca-cli` | `config.ts` 常量、`utils/paths`、`utils/child-process`、`utils/zip`、`utils/version-check`、`cli/config-selector`、`cli/project-trust`、`output-guard.isStdoutTakenOver`、`package-manager-cli` 的 CLI 层 |
| `ca-tui` | `Theme`、`loadThemeFromPath`、`getTerminalColorMode`、`detectCapabilities`、`Markdown` |
| `ai-core` / `ai-providers`（spec 单元名 → crate `kiana-ai`） | `formatUserAgent`、`KIANA_ATTRIBUTION`、`uuidv7`、`clampThinkingLevel`、`streamSimple`、`RetryPolicy` |
| `tui-core`（interactive） | `formatCrashExtensionHint` 的调用点、诊断渲染、启动警告展示 |

---

## 6. npm 依赖 → Rust crate 映射

### 6.1 直接依赖（`packages/coding-agent/package.json` 中与本单元相关的）

| npm 包 | 版本 | 用途（本单元） | Rust 选择 | 行为差异与注意事项 |
|---|---|---|---|---|
| `chalk` | 6.0.0 | 终端着色 | `anstyle` / `owo-colors` + 自写 chalk 兼容层 | chalk 6 的级别检测：`--color` 标志、`FORCE_COLOR`/`NO_COLOR`、`TERM=dumb`、TTY。**必须逐字节一致**（含 `chalk.bold(chalk.yellow(x))` 的嵌套顺序：先内层 SGR 后外层，结束时统一 reset）。Rust 侧建议直接复用 `ca-cli` 的 chalk 模拟器 |
| `hosted-git-info` | 9.0.3 | git URL 解析 | `url` + 自写 | 该库支持 GitHub/GitLab/Bitbucket/gist/… 的 shorthand 与 `#committish`。**本单元只用 `fromUrl` 的 `domain`/`user`/`project`/`committish`**，可用正则覆盖常见 host；但 `fromUrl` 能识别 `github:user/repo`、`gitlab:`、`bitbucket:`、`gist:` 等 shorthand——本单元先要求 `git:` 前缀或显式协议，故 shorthand 分支很少触发 |
| `ignore` | 7.0.8 | `.gitignore`/`.ignore`/`.fdignore` 匹配 | `ignore` crate（BurntSushi，同源算法） | 语义高度一致；差异：JS 版的 `ignores(path)` 要求路径为 POSIX 相对路径、可选尾 `/` 表示目录；Rust `ignore::gitignore::Gitignore::matched_path_or_any_parents` 语义略有不同。**建议用 `ignore::gitignore::GitignoreBuilder` 并保留「先按目录加规则」的调用顺序**。另外 `addIgnoreRules` 会给每一行加前缀（§2.6.2），Rust 必须自己实现同样的前缀化，不能依赖 crate 的 `add_line` 默认行为 |
| `minimatch` | 10.2.6 | glob 模式（`applyPatterns`） | `globset` / `glob` | minimatch 支持 `**`、`{a,b}`、`[abc]`、`!(…)` 扩展 glob、`!` 否定、`#` 注释、`dot` 选项。`globset` 不支持 `!(…)` 与 `#`。**本单元传入的模式来自用户 settings 与 manifest，可能含 `*`/`?`/`**`**。建议自写 minimatch 子集并覆盖测试用例；`glob` crate 的 `Pattern::matches` 语义接近（`*` 不跨 `/`），但 `**` 处理不同。**必须保证 `matchesAnyPattern` 的六个比较点（rel/name/abs + SKILL.md 的 parent 三种）** |
| `semver` | 7.8.5 | 版本范围 | `semver` crate | `valid`/`validRange`/`satisfies`/`gt`/`maxSatisfying`/`rcompare`。差异：npm semver 允许 `latest`/tag 作为「非版本」（`valid` 返回 null），`semver` crate 的 `Version::parse` 同样失败——语义一致。**`maxSatisfying(versions, range)` 在无匹配时返回 null；`rcompare` 排序需完全一致（含预发布版本的排序规则）** |
| `proper-lockfile` | 4.1.2 | managed 更新锁 | `fslock` / `fd-lock` / 手写 | `lockSync(dir, {realpath:false})` 创建 `<dir>.lock` 目录；`lock` 返回 release 函数；`ELOCKED` 错误码。Rust：`create_dir` 原子失败 + 重试 + 陈旧锁检测（proper-lockfile 默认 `stale: 10000ms`、`update: 5000ms`，会比较 mtime）。**只用于 `<managedRoot>/update`** |
| `yaml` | 2.9.0 | frontmatter | `yaml-rust2` 或 `serde_yaml`（已废弃）/ `saphyr` | `yaml` 包是 YAML 1.2；`serde_yaml` 是 1.1 且已停维。`saphyr`/`yaml-rust2` 更接近 1.2。差异点：重复键、`yes/no`（1.1 是布尔，1.2 是字符串）、多行折叠标量、日期。**frontmatter 里 `disable-model-invocation: yes` 在 YAML 1.2 下是字符串 `"yes"`，`parseFrontmatter` 之后 `=== true` 为 false**——必须与 TS 一致（TS 用 `yaml` 包，即 1.2）。Rust 选 `saphyr` 并确认 `yes` 不被当布尔 |
| `glob`（Node 内置 `fs.globSync`） | Node 22 | `expandPackageGlob` | `glob` crate | Node 的 `globSync(pattern,{cwd})` 返回相对路径字符串数组；`*` 不匹配 `/`，`**` 匹配任意层；默认不含 dotfile。`glob` crate 的 `glob()` 返回 `PathBuf` 绝对/相对取决于 pattern；需要自行转相对并排序（UTF-16 码元序 vs Rust `str` 字节序——**对 ASCII 一致，对非 ASCII 不同，需按 UTF-16 码元比较**） |

### 6.2 平台 / 语言运行时差异（必须桥接）

| 主题 | TypeScript 行为 | Rust 桥接方案 |
|---|---|---|
| **字符串索引** | UTF-16 码元；`String.length`、`slice`、`charAt` 都按码元 | `text.length` 出现在技能名/描述长度校验（`MAX_NAME_LENGTH=64`、`MAX_DESCRIPTION_LENGTH=1024`）、`description` 截断到 60、`content.slice(16)`、`slice(-5)`。**必须用 `encode_utf16().count()` 计长**；截断 60 需按 UTF-16 码元切（可能切断代理对，但 JS 也会，因此要复刻「切在码元边界」的行为）。建议封装 `js_len(s)`、`js_slice(s, a, b)` |
| **正则方言** | JS RegExp（ECMAScript） | `regex` crate 不支持反向引用/前瞻/`\p{...}` 的某些形式。本单元用到的模式（见 §2 各条）都简单，但 `SENSITIVE_KEY` 用了 `(?:...)` 与 `[-_]`、`/^\s+at\s/u`、`/^\/mnt\/[a-z](?:\/|$)/i`、`/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/`——`regex` 可覆盖。**`String.prototype.replace` 的替换串 `$1`/`$&` 语义需手写** |
| **JSON 键顺序** | 对象字面量顺序 | `serde_json` 开 `preserve_order`（`IndexMap`）+ 用 `json!` 或 `Serialize` 派生保持字段顺序 |
| **数字格式化** | `JSON.stringify(1.0)` → `1`；`0.1+0.2` → `0.30000000000000004` | `serde_json` 输出 `1.0` 为 `1.0`（不同！）。**必须对浮点做 JS 兼容格式化**（`ryu` 的 `format_finite` 与 JS 的 shortest round-trip 基本一致，但整数值要输出无 `.0`）。本单元写盘 JSON 中浮点出现于 settings（多为整数）、`osRelease`（字符串）、token 统计（整数）——风险低但需统一封装 |
| **`Date.toISOString()`** | `YYYY-MM-DDTHH:mm:ss.sssZ`，毫秒恒 3 位 | `chrono`：`to_rfc3339_opts(SecondsFormat::Millis, true)` 恰好等价 |
| **`Date.toLocaleString()`** | 依赖 ICU 与 `LANG`；Node 默认使用完整 ICU | 崩溃通知里的 `${when}`。Rust 建议用 `icu` 的 `DateTimeFormat` 按 `LANG` 选择 locale；**若无法一致，至少在同 locale 下匹配 `en-US` 的 `M/D/YYYY, h:mm:ss AM` 风格**（需实测记录基准） |
| **`structuredClone`** | 深拷贝，支持 Map/Set/Date | `serde_json` 往返或 `Clone`（`McpServerRegistry.list()` 的 config） |
| **`child_process.spawn`** | `stdio:"inherit"` / `["ignore",2,2]`；`exit` vs `close` 区别 | `tokio::process::Command`；`Stdio::inherit()`；`["ignore",2,2]` 需用 `Stdio::from_raw_fd(2)`（Unix）或 `Stdio::inherit()` 近似（Windows 无 fd 2 概念）。**`runCommand` 用 `exit`（进程一退出就 resolve），`runCommandCapture` 用 `close`（等管道 EOF）**——Rust 中 `child.wait()` 对应 exit，读完 stdout/stderr 后对应 close |
| **跨平台 spawn（Windows）** | `cross-spawn` 处理 `.cmd`/`.bat` 与 PATH 解析 | Rust 需自写：`npm` 在 Windows 上是 `npm.cmd`，`Command::new("npm")` 会失败。**必须复刻 `cross-spawn` 的 `npm.cmd` shim**（`cmd /c npm ...`） |
| **`fs.watch`** | inotify/FSEvents/kqueue 封装；回调 `(eventType, filename)`，`filename` 可能为 null | `notify` crate。**`filename` 为 null 时也要 refresh**（见 §2.15.3）。`notify` 的 `EventKind` 与 `filename` 语义不同，需归约为「任意变更 → scheduleRefresh」 |
| **`fs.watchFile` 轮询** | `{interval: 1000}`，比较 `mtimeMs`/`ctimeMs`/`size` | 手写 `tokio::time::interval` + `metadata()`；`ctime` 在 Linux 是 inode 变更时间（`Metadata::ctime`），Windows 无 ctime → 需退化处理 |
| **`process.env`** | 大小写敏感（Unix）/不敏感（Windows） | `std::env`；Windows 上需用 `GetEnvironmentVariable` 的大小写不敏感查询 |
| **`os.version()` 与 `os.release()`** | Node 包装 `uname` | `uname` crate 或 `libc::uname`；Windows 用 `RtlGetVersion` |
| **`Array.prototype.sort`** | V8 稳定排序（TimSort）；默认字符串比较按 UTF-16 码元 | `sort_by` 是稳定排序；但比较函数必须用 UTF-16 码元序（`a.encode_utf16().cmp(b.encode_utf16())`），不能直接用 `str::cmp`（字节序） |
| **`Map` 迭代顺序** | 插入序；`set` 已存在键**不改变位置** | `IndexMap`：`insert` 已存在键同样不改变位置（语义一致）。**注意 `serde_json` 的 `preserve_order` 用 IndexMap** |
| **`Promise.all` 错误语义** | 任一 reject → 立即 reject，其余继续运行 | `futures::future::try_join_all` 会 drop 其余 future（= 取消）。**必须用 `JoinSet`，收集第一个错误但等其余完成**（或 `FuturesUnordered` 不 drop） |
| **`AbortController`/`AbortSignal`** | 15s 超时 abort | `tokio_util::sync::CancellationToken` |
| **`decodeURIComponent`** | 失败抛 URIError | `percent_encoding::percent_decode_str(...).decode_utf8()` 失败即等价 |
| **`decodeURI`** | 不解码保留字符（`/`、`:` 等） | 需手写：`decodeURI` 不解码 `;/?:@&=+$,#` |
| **`encodeURIComponent`** | 精确的字符集 | `percent_encoding::utf8_percent_encode` 用 `NON_ALPHANUMERIC` 会多编码 `-_.!~*'()`；需自定义 AsciiSet |
| **`URL`（WHATWG）** | 宽松解析、自动百分号编码、`hostname` 对 IPv6 带方括号 | `url` crate（rust-url）就是 WHATWG URL 标准实现，语义一致 |
| **`fileURLToPath`** | `file:///a/b` → `/a/b`（Windows 处理盘符） | `url::Url::to_file_path()` |
| **`uuidv7`** | 时间有序 UUID | `uuid` crate `Uuid::now_v7()`（features `v7`） |
| **ZIP** | `src/utils/zip.ts` 自写（`deflateRawSync` + `crc32` + DOS 本地时间戳） | `zip` crate（`ZipWriter` + `Deflated` + `FileOptions::default().compression_level(Some(6)).last_modified_time(DateTime::default())`）；字节布局见 §4.3.1。**已核实无测试比对归档字节**，纯内容兼容即可；仅在需要逐字节对拍时复刻 `createZipArchive`（注意 Node zlib 与 `flate2` 的输出不一定相同——即使同 level，`miniz_oxide` 与 zlib 的匹配搜索不同） |
| **`Blob`/`FormData`/`fetch`** | WHATWG fetch | `reqwest` + `reqwest::multipart`；boundary 格式不同，但服务端不应依赖具体 boundary。**若测试比对原始请求体，需复刻 Node undici 的 boundary 生成（随机）——建议测试只比对 part 名与内容** |
| **`Intl`/locale** | 见 `toLocaleString` | 同上 |

### 6.3 新增 Rust 依赖建议

```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "fs", "time", "sync"] }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
indexmap = { version = "2", features = ["serde"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "multipart", "json", "stream"] }
url = "2"
percent-encoding = "2"
semver = { version = "1", features = ["serde"] }
globset = "0.4"
glob = "0.3"
ignore = "0.4"
saphyr = "0.0"               # 或 yaml-rust2
sha2 = "0.10"
hex = "0.4"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v7"] }
notify = "6"
notify-debouncer-mini = "0.4" # 可选，替代手写 500ms 去抖
fslock = "0.2"
zip = { version = "2", default-features = false, features = ["deflate"] }
crc32fast = "1"
# 仅当需要与 TS 逐字节对拍归档时才需要（Node 的 deflateRawSync 是 zlib 实现，
# zip crate 默认走 flate2/miniz_oxide，压缩流可能不同；换成 zlib 后端可提高一致概率，
# 但不要把「一致」作为验收条件——见 §4.3.1）。
# flate2 = { version = "1", default-features = false, features = ["zlib"] }
anstyle = "1"
owo-colors = "4"
thiserror = "1"
async-trait = "0.1"
futures = "0.3"
libc = "0.2"
unicode-width = "0.1"        # 主题/Markdown 渲染（ca-tui 侧）
```
**dev-dependencies**：`tempfile`、`assert_cmd`、`predicates`、`tokio = { features = ["test-util"] }`、
`insta`（快照测试逐字文案）。

### 6.4 明确的「不要替换」清单

1. 不要用 `std::fs::canonicalize` 直接替换 `canonicalizePath`：TS 版**失败时回退原路径**；
   Rust 版必须 `unwrap_or(path)`。
2. 不要用 `Path::starts_with` 替换 TS 的 `String.startsWith`：
   `pruneEmptyGitParents`、`findShadowedContextFile`、`findSourceInfoForPath` 里都是**字符串前缀**比较
   （`/a/bc` 命中 `/a/b`），而 `Path::starts_with` 是**路径分量**比较。必须逐字用字符串。
3. 不要把 `resolveManagedPath` 的检查改成 `Path::starts_with`：TS 是
   `resolvedPath !== root && !resolvedPath.startsWith(root + sep)`，等价于路径分量，但**根自身是允许的**。
4. 不要把 `KIANA_TIMING` 改成运行期读取：模块加载时求值一次。
5. 不要合并 `runCommand`（exit）与 `runCommandCapture`（close）的语义。
6. 不要把 `getEnabledResources` 的「先记录全部（含 disabled）metadata」优化掉——disabled 资源的
   metadata 仍用于 sourceInfo 归属（`findSourceInfoForPath`）。
7. 不要在 `applyPatterns` 中改变四步顺序（includes → excludes → force-includes → force-excludes）。
8. 不要把 `mapSkillPath` 应用到 `local`/`cli` 来源（只对 `auto` 与 `origin==="package"`）。

---

## 7. 测试清单

> 所有测试位于 `packages/coding-agent/test/`。Rust 移植应逐条转写为集成测试；标注 **[逐字]** 的断言字符串
> 或文件内容必须字节级一致。
> **运行方式（TS）：`vitest`**——`packages/coding-agent/package.json` 的 `"test": "vitest --run"`，
> 根目录 `vitest.config.ts` 的 `test.env` 固定 `KIANA_OFFLINE: "1"`、`testTimeout: 30000`、`globals: true`、
> `unstubEnvs: true`、`silent: "passed-only"`。所有测试从 `vitest` 导入 `describe/it/expect/vi`（**不是** `bun test`，
> **也不是** `node --test`；旧版本此处写「仓库使用 `bun test`」是错的）。需要网络路径的测试调用
> `test/test-network-env.ts` 的 `allowNetwork()`（`vi.stubEnv("KIANA_OFFLINE", undefined)`）显式退出离线模式。

### 7.1 核心资源与包管理

#### `package-manager.test.ts`（2941 行，132 个测试）

**解析与覆盖（`resolve`）**
1. `should return no package-sourced paths when no sources configured`
2. `should resolve local extension paths from settings`
3. `should resolve built-in extensions with user exclusions and project overrides` — rank 5、scope 判定
4. `should resolve skill paths from settings`
5. `should auto-discover root markdown skills from .kiana skill dirs` — kiana 模式只在根收 `.md`
6. `should resolve project paths relative to .kiana`
7. `should auto-discover user prompts with overrides`
8. `should resolve symlinked user and project resources once` — canonicalize 去重
9. `should auto-discover project prompts with overrides`
10. `should resolve directory with package.json kiana.extensions in extensions setting`
11. `should use the agent dir as baseDir for user .kiana/agent skills`
12. `should use the project .kiana dir as baseDir for project .kiana skills`
13. `should use ~/.agents as baseDir for user .agents skills`
14. `should use each project .agents dir as baseDir for project .agents skills`
15. `should scan .agents/skills from cwd up to git repo root`
16. `should scan .agents/skills up to filesystem root when not in a git repo`
17. `should ignore root markdown files in .agents/skills but discover nested markdown skills` — agents 模式反向
18. `should keep ~/.agents/skills user-scoped when cwd is under home in a non-git directory`
19. `should dedupe user skill entries when ~/.kiana/agent/skills is a symlink to ~/.agents/skills`
20. `should respect .gitignore in skill directories`
21. `should not apply parent .gitignore to .kiana auto-discovery` — rootDir 语义
22. `should resolve local paths`
23. `should handle directories with kiana manifest`
24. `should keep kiana manifest entries with leading tilde package-relative` — `~foo` 不展开
25. `should handle directories with auto-discovery layout`
26. `should stop recursing when a package skill directory contains SKILL.md`
27. `should emit progress events`
28. `should preserve argv entries containing spaces`

**npm / 包管理器命令**
29. `should use npmCommand argv for npm installs`
30. `should pass legacy peer deps when uninstalling npm packages`
31. `should pass the real path as --prefix when the agent dir is a symlink`
32. `should use bun --cwd for npm package installs`
33. `should install git package dependencies without auto-installing peers`
34. `should remove a newly created checkout when git clone fails`
35. `should remove a newly cloned checkout when dependency installation fails`
36. `should reconcile an existing git checkout to a pinned ref during install`
37. `should reconcile an existing git checkout to its update target when installing without a ref`
38. `should prefer the package manager after a separator over the outer executable` — `--` 之后
39. `should detect pnpm through a corepack wrapper without a separator`
40. `should disable peer installation for git package dependencies with bun`
41. `should update git package dependencies without auto-installing peers`
42. `should repair missing git package dependencies when the checkout is already current`
43. `should repair deleted git package dependencies when cleaning fails`
44. `should disable peer installation through wrapped pnpm when updating git dependencies`
45. `should use npmCommand argv for npm root lookup and invalidate cached root when npmCommand changes`
46. `should install user npm packages into the kiana-managed npm root`
47. `should load legacy pnpm global package paths from pnpm list output`
48. `should resolve wrapped pnpm global package paths from pnpm list output`
49. `should ignore malformed legacy pnpm global package lists`
50. `should emit progress events on install attempt`

**URL / identity**
51. `should recognize github URLs without git: prefix`
52. `should parse package source types from docs examples` — 覆盖 `docs/packages.md` 全部示例
53. `should never parse dot-relative paths as git`
54. `should reject paths outside git install roots` — `Refusing to use path outside package install root`
55. `should place temporary npm packages under the agent temp extension folder`
56. `should store global local packages relative to agent settings base`
57. `should store project local packages relative to .kiana settings base`
58. `should remove local package entries using equivalent path forms`
59. `should return false when adding the same git source with the same ref`
60. `should update the ref when adding the same git source with a different ref`
61. `should preserve package filters when replacing a package source ref`
62. `should parse HTTPS GitHub URLs correctly`
63. `should parse HTTPS URLs with git: prefix`
64. `should parse HTTPS URLs with ref`
65. `should parse host/path shorthand only with git: prefix`
66. `should treat host/path shorthand as local without git: prefix`
67. `should parse HTTPS URLs with .git suffix`
68. `should parse GitLab HTTPS URLs`
69. `should parse Bitbucket HTTPS URLs`
70. `should parse Codeberg HTTPS URLs`
71. `should generate correct package identity for protocol and git:-prefixed URLs`
72. `should deduplicate git URLs with different supported formats`
73. `should handle HTTPS URLs with refs in resolve`

**过滤模式**
74. `should expand ~ in exclusion and exact patterns`
75. `should exclude extensions with ! pattern`
76. `should filter themes with glob patterns`
77. `should filter prompts with exclusion pattern`
78. `should filter skills with exclusion pattern`
79. `should work without patterns (backward compatible)`
80. `should support glob patterns in manifest extensions`

（其余 ~52 个测试为同类变体：settings 对象形式的 `autoload:false` 增量、`next`/`remove` 持久化、
update 检查、并发限制、错误消息等；转写时以本节 + §2.10 的行为为准。）

#### `package-manager-ssh.test.ts`（97 行，8 个测试）
1. `should parse https:// URL`
2. `should parse ssh:// URL`
3. `should parse git@host:path format`
4. `should parse host/path shorthand`
5. `should parse shorthand with ref`
6. `should treat git@host:path as local without git: prefix`
7. `should treat host/path shorthand as local without git: prefix`
8. `should normalize protocol and shorthand-prefixed URLs to same identity`

#### `resource-loader.test.ts`（1476 行，57 个测试）

**包警告**
1. `should initialize with empty results before reload`
2. `should not treat a project manifest as the owner of a project extension`
3. `should warn about host dependencies in an extension package manifest` — §2.11.2 长串
4. `should warn and keep loading when a configured package cannot be installed`
5. `should point temporary extension sources at the source instead of kiana remove` — `Check the source and try again.`
6. `should point project extension packages at kiana remove --local`
7. `should warn once about a failing install when project trust is resolved`
8. `should fail when an extension package manifest cannot be parsed` — `JSON.parse` 抛出

**发现**
9. `should discover skills from agentDir`
10. `should ignore extra markdown files in auto-discovered skill dirs`
11. `should discover prompts from agentDir`
12. `should report invalid prompt frontmatter while loading valid siblings`
13. `should prefer project resources over user on name collisions`
14. `should load symlinked user and project extensions once`
15. `should load user extensions before trust and reuse them after trust resolves`
16. `should keep both extensions loaded when command names collide`
17. `should honor overrides for auto-discovered resources`

**上下文文件**
18. `should discover AGENTS.md context files`
19. `should prefer AGENTS.override.md within each directory while preserving ancestor layering`
20. `should ignore context file candidates that are directories`
21. `should skip context file discovery when noContextFiles is true`
22. `should discover SYSTEM.md from cwd/.kiana`
23. `should skip project resources that require trust when project is not trusted`
24. `should discover APPEND_SYSTEM.md`
25. `exposes discovered project SYSTEM.md as the system prompt source`
26. `exposes discovered global SYSTEM.md as the system prompt source`
27. `does not expose literal system prompt text as a source`
28. `exposes file-backed system prompt options as a source`
29. `exposes discovered APPEND_SYSTEM.md as an append system prompt source`
30. `does not expose literal append system prompt text as a source`
31. `only exposes file-backed append system prompt options as sources`

**元数据 / 覆盖**
32. `should load skills and prompts with extension metadata`
33. `should load extension resources returned as file URLs`
34. `should keep package metadata for skills, prompts, and themes`
35. `should skip skill discovery when noSkills is true`
36. `should still load additional skill paths when noSkills is true`
37. `should apply skillsOverride`
38. `should apply systemPromptOverride`

**扩展冲突 / 内置**
39. `should detect tool conflicts between extensions`
40. `should prefer explicit CLI extensions over discovered extensions when commands and tools conflict`
41. `should leave out replaceable extensions whose names another extension registers`
42. `should skip built-in extensions disabled in settings`
43. `should skip disabledBuiltinExtensions even when settings or -e enable them`
44. `should load built-in extensions after file extensions with and without trust resolution`
45. `should disable built-in extensions with noExtensions unless loaded with -e builtin:<name>`
46. `should apply project built-in extension overrides after trust resolves`

**worktree 上下文去重（`findShadowedContextFile`）**
47. `should skip the main repo's duplicate when the worktree root has its own context`
48. `should load the worktree's context once when it symlinks to the main repo's file`
49. `should skip the main repo's duplicate when the main repo's file is a symlink`
50. `should still inherit the main repo's context when the worktree root has none`
51. `should only skip the same filename, not a differently named context file`
52. `should NOT skip the container's context in a bare layout (proj/.bare + proj/main)`
53. `should keep loading ancestors above the main repo`
54. `should NOT skip anything for a sibling worktree (main repo is not an ancestor)`
55. `should NOT skip the superproject's context from inside a submodule`
56. `should keep climbing past an ordinary repo root`
57. `should climb normally when the gitdir: target does not exist`

#### `resource-loader-theme.test.ts`（146 行，3 个测试）
1. `it.each([...])` — 主题颜色模式与 `terminalCapabilityOverrides` 的交互（表驱动）
2. `returns to automatic detection after an explicit setting is removed`
3. `applies strict validation on initial load and reload` — 主题 schema 严格校验

#### `skills.test.ts`（432 行，28 个测试）
1. `should load a valid skill`
2. `should allow names that don't match parent directory`
3. `should warn when name contains invalid characters`
4. `should warn when name exceeds 64 characters`
5. `should warn and skip skill when description is missing`
6. `should ignore unknown frontmatter fields`
7. `should load nested skills recursively`
8. `should prefer a directory's root SKILL.md over nested SKILL.md files`
9. `should skip files without frontmatter`
10. `should warn and skip skill when YAML frontmatter is invalid`
11. `should preserve multiline descriptions from YAML`
12. `should warn when name contains consecutive hyphens`
13. `should load all skills from fixture directory`
14. `should return empty for non-existent directory`
15. `should use parent directory name when name not in frontmatter`
16. `should parse disable-model-invocation frontmatter field`
17. `should default disableModelInvocation to false when not specified`
18. `should return empty string for no skills`
19. `should format skills as XML`
20. `should include intro text before XML`
21. `should escape XML special characters`
22. `should format multiple skills`
23. `should exclude skills with disableModelInvocation from prompt`
24. `should return empty string when all skills have disableModelInvocation`
25. `should load from explicit skillPaths`
26. `should warn when skill path does not exist`
27. `should expand ~ in skillPaths`
28. `should detect name collisions and keep first skill`

#### `prompt-templates.test.ts`（674 行，94 个测试）
四组：
- **替换（`substituteArgs`）**：`$ARGUMENTS`、`$@`、`$1`、`$0`、`$10`、十进制（`$1.5` 只匹配整数部分）、
  空参数、重复出现、特殊字符、Unicode、换行/制表、连续 `$` 模式、引号参数、词内出现、大小写敏感、
  非递归替换、`\` 保留。
- **默认值**：`${1:-default}`、`${1:-}`（空默认）、`${ARGUMENTS:-x}`、`${@:-x}`、含空格的默认值、
  非递归默认值、越界位置默认值。
- **切片**：`${@:N}`、`${@:N:L}`、越界、零长度、超长长度、`${@:0}` 视为全部、空数组、单元素、
  文本中间、多个切片、带引号/特殊字符/Unicode、与位置参数混用。
- **`parseCommandArgs`**：空格分隔、双引号、单引号、混合引号、空串、多余空格、制表符、
  引号内空串（保留为参数）、特殊字符、Unicode、引号内换行、未引号换行作分隔、空白折叠、
  引号内转义引号。
关键断言（**逐字**）：`${@:0}` 等于全部参数；`$1.5` 只替换 `$1`；引号内 `\"`/`\'` 的去转义行为见 §2.7。

#### `system-prompt.test.ts`（238 行，16 个测试）
1. `shows (none) for empty tools list`
2. `shows file paths guideline even with no tools`
3. `keeps the default and custom prompt prefixes exact`
4. `preserves an exact forced prompt without sections`
5. `maps appended instructions and project context to stable sections`
6. `includes all default tools when snippets are provided`
7. `test.each([...])` — 工具片段渲染表
8. `instructs models to resolve kiana docs and examples under absolute base paths`
9. `includes custom tools in available tools section when promptSnippet is provided`
10. `omits custom tools from available tools section when promptSnippet is not provided`
11. `appends promptGuidelines to default guidelines`
12. `deduplicates and trims promptGuidelines`
13. `test.each([...])` — 读者（reader）选择表
14. `omits skills without read or bash`
15. `leaves hidden tools out of the tool list and rules`
16. `keeps skills without naming a hidden reader`

#### `system-prompt-updates.test.ts`（303 行，8 个测试）
1. `declares the prompt and tools once and reuses them across resume`
2. `opens a transcript without a system message and declares the prompt on the first request`
3. `diffs sections into a patch`
4. `keeps the preamble untagged and replaces it like any section`
5. `a forced prompt is sent as the leading prompt for the run and never recorded`
6. `setActiveTools emits prompt sections and tool changes before the next request`
7. `setActiveTools in before_agent_start controls the same request`
8. `keeps tool declarations stable across a session JSON round-trip`

#### `tool-system-prompt-contributions.test.ts`（44 行，2 个 `test.each`）
工具 `systemPrompt` 片段与 `promptGuidelines` 的注入表。

### 7.2 MCP

#### `mcp-command.test.ts`（197 行，8 个测试）——`kiana mcp` CLI
1. `lists servers with their state, tools, and errors, and fails while anything is wrong`
2. `prints JSON for scripts`
3. `rejects unknown servers and servers without OAuth for login and logout`
4. `adds stdio servers and passes options after the command through`
5. `adds HTTP servers and keeps other content of the file`
6. `rejects invalid add invocations without writing`
7. `adds and removes project servers`
8. `removes global servers`

#### `mcp-extension.test.ts`（868 行，30 个测试）——MCP 扩展（大部分不属于本单元，但校验规则相关）
1. `merges global and trusted project servers and validates entries`
2. `lets project entries override enabled and exposure of global servers`
3. `reads and edits mcp.json files that start with a UTF-8 BOM`
4. `rejects server names that differ only in - and _` — `mcpNamespace` 冲突
5. `validates exposure and reads autoEnableCodemode with project precedence`
6. `validates the OAuth callback URL, scope, and client name`
7. `resolves per-tool exposure from exact names, then patterns in order`
8. `validates provider auth and accepts it only in the global mcp.json`
9. `creates provider-safe tool names`
10–30. 连接、重试、OAuth、资源读取、20KB 截断、多字节边界等（属 `telemetry-mcp` 单元）。

`mcp-manager-view.test.ts`（40 行，2 个测试）：取消键行为。

### 7.3 SDK

#### `sdk-skills.test.ts`（113 行，3 个测试）
1. `should discover skills by default and expose them on session.skills`
2. `should have empty skills when resource loader returns none (--no-skills)`
3. `should use provided skills when resource loader supplies them`

#### `sdk-session-manager.test.ts`（131 行，4 个测试）
1. `uses agentDir for the default persisted session path`
2. `keeps an explicit sessionManager override`
3. `derives cwd from an explicit sessionManager when cwd is omitted`
4. `exposes current session state to the built-in bash tool`

#### `sdk-stream-options.test.ts`（324 行，10 个测试）
1. `schedules cache warming after a completed session request`
2. `waits for the next request instead of restoring cache warming`
3. `forwards httpIdleTimeoutMs as timeoutMs for OpenAI Codex`
4. `defaults timeoutMs from httpIdleTimeoutMs for all providers`
5. `lets request timeoutMs override httpIdleTimeoutMs for OpenAI Codex`
6. `forwards websocketConnectTimeoutMs from settings`
7. `lets request websocketConnectTimeoutMs override settings`
8. `forwards provider retry settings`
9. `forwards provider stream events to extensions` — 事件形状断言
10. `runs before_provider_headers on assembled headers without forwarding the transform`

#### `sdk-client-identity.test.ts`（191 行，5 个测试）
1. `identifies kiana with a detailed User-Agent and attribution as a CLI agent`
2. `turns attribution off when telemetry is disabled`
3. `turns attribution off with KIANA_TELEMETRY=0`
4. `adds OpenCode session headers for custom OpenCode providers`
5. `lets configured OpenCode headers override the session header`

### 7.4 Bug 报告与崩溃

#### `bug-report.test.ts`（113 行，3 个测试）
1. `preserves line breaks in pasted descriptions`（在 `describe("bug report prompt")` 内）
2. `removes URL credentials and secret query parameters`
3. `redacts nested secret values without hiding token counts`

> 已核实：该文件**不触及** `writeBugReportArchive` / `writeZipArchive` / `bugReportFiles`，
> 全仓也没有任何测试解压或比对 ZIP 字节（`grep -rnE 'writeBugReportArchive|createZipArchive|bugReportFiles|writeZipArchive' --include='*.ts' packages/`
> 只命中 3 个源文件 + `bug-report-upload.ts`，无测试）。
> 因此 ZIP 只需要「内容正确」，字节级复刻是可选项（见 §4.3.1）。

#### `crash-log.test.ts`（89 行，5 个测试，`describe("extension crash attribution")`）
1. `matches stack frames beneath loaded package roots`
2. `normalizes Windows paths and deduplicates package extensions`
3. `does not attribute sibling single-file packages`
4. `ignores extension paths in the error message`
5. `decodes frame paths independently from malformed error text`

#### `interactive-mode-bug-report-hint.test.ts`（71 行，5 个测试）
1. `identifies extensions with frames in a crash stack` — `formatCrashExtensionHint`
2. `does not suggest reports for retryable provider failures`
3. `does not suggest reports for cancellations`
4. `suggests reports for unexpected errors`
5. `shows the install change warning instead of a bug report hint`

#### `interactive-mode-assistant-diagnostics.test.ts`（95 行，2 个测试）
1. `shows Anthropic thinking drops when cache miss notices are enabled`
2. `does not repeat unchanged Anthropic thinking drops`

### 7.5 页脚

#### `footer-data-provider.test.ts`（279 行，8 个测试）
1. `uses HEAD directly in a regular repo from a nested directory`
2. `resolves the branch via git when HEAD is .invalid in a reftable repo`
3. `resolves the branch via git in a reftable-backed worktree`
4. `treats an unresolved .invalid reftable HEAD as detached`
5. `does not notify listeners when reftable updates keep the same branch`
6. `debounces rapid reftable updates into a single async refresh`
7. `updates the cached branch when the reftable directory changes`
8. `retries git watchers 5 seconds after an async fs.watch error`

#### `suite/regressions/2791-fswatch-error-crash.test.ts`（106 行，1 个测试）
`process should survive an error event on the theme FSWatcher` — `watchWithErrorHandler` 的 `onError` 不得
让进程崩溃（主题文件监视器）。

### 7.6 包命令 CLI

#### `package-command-paths.test.ts`（1076 行，35 个测试）
1. `should persist global relative local package paths relative to settings.json`
2. `should remove local packages using a path with a trailing slash`
3. `skips untrusted project package settings`
4. `uses remembered project trust for list`
5. `overrides remembered trust for list with --no-approve`
6. `approves project trust for list with --approve`
7. `uses default project trust for list`
8. `uses project_trust extensions for package commands`
9. `does not prompt or ask extensions for project trust during update`
10. `uses saved project trust during update`
11. `lets trust.json override default project trust`
12. `blocks local package changes when project is untrusted`
13. `allows local package install to initialize fresh project settings`
14. `shows install subcommand help`
15. `refreshes only model catalogs with update --models`
16. `rejects update --models combined with another update target`
17. `cycles project package overrides in config local mode`
18. `toggles built-in extensions in config global mode`
19. `cycles project built-in extension overrides in config local mode`
20. `shows a friendly error for unknown install options`
21. `shows a friendly error for missing install source`
22. `allows explicit self-update checks when automatic version checks are disabled`
23. `retries a transient self-update version check`
24. `updates installer-managed Kiana through a staged immutable release`
25. `accepts a managed install marker with a UTF-8 BOM`
26. `removes older managed releases after an update`
27. `rejects a concurrent managed update` — `Another managed Kiana update is already running.`
28. `rejects forced managed reinstalls`
29. `keeps the managed release active when its update fails`
30. `keeps npm self-updates non-managed when the managed environment is inherited`
31. `uses the current package name when the update check omits packageName`
32. `installs the active package name from the update check during self-update`
33. `prints a pnpm metadata hint when self-update fails`
34. `fails self-update when renamed npm package installation fails`
35. `suggests the configured source when update input omits the npm prefix`

#### `package-distribution.test.ts`（24 行，1 个测试）
`uses the bundle for executables and modular output for libraries` — `package.json` 的 `bin`/`exports` 结构。

#### `settings-diagnostics.test.ts`（47 行，3 个测试）
1. `includes the settings file path for file-backed storage`
2. `falls back to the settings scope for storage without file paths`
3. `deduplicates diagnostics by type and message`

### 7.7 回归测试

- `suite/regressions/2753-reload-stale-resource-settings.test.ts`（105 行，1 个）：
  `applies updated top-level prompt settings on reload after startup` — reload 必须重新读取 settings。
- `suite/regressions/2781-skill-collision-precedence.test.ts`（120 行，4 个）：
  1. `user auto-discovered skill should override package skill with same name`
  2. `project auto-discovered skill should override package skill with same name`
  3. `project skill should override user skill which should override package skill`
  4. `collision diagnostics should report package skill as loser when user skill wins`
- `suite/regressions/7187-malformed-package-manifest.test.ts`（47 行，1 个）：
  `ignores invalid resource fields without dropping valid fields` — manifest 逐字段容错。
- `suite/regressions/2791-fswatch-error-crash.test.ts`：见 §7.5。

### 7.8 其它相关（跨单元，但会触及本单元）

- `export-html-skill-block.test.ts`（40 行，4 个）：技能 XML 包装在 HTML 导出中的剥离/分块/侧栏。
- `sdk-codex-cache-probe-tool-loop.ts`（非测试入口，辅助）。
- `block-images.test.ts`（**148 行，9 个测试**，`describe("blockImages setting")`，分 3 个 describe）：
  旧版本只说「`convertToLlmWithBlockImages` 的图片占位符」——**这正是该测试没测的东西**（全仓没有任何测试
  直接引用 `convertToLlmWithBlockImages`，也没有断言 `Image reading is disabled.`）。实际断言的是：
  1. `SettingsManager`（4 个）：`should default blockImages to false`、`should return true when blockImages is set
     to true`、`should persist blockImages setting via setBlockImages`、`should handle blockImages alongside autoResize`。
  2. `Read tool`（2 个）：`should always read images (filtering happens at convertToLlm layer)`
     （`createReadTool` 结果里**仍有** `type:"image"` 内容）、`should read text files normally`。
  3. `processFileArguments`（3 个）：`should always process images (filtering happens at convertToLlm layer)`、
     `should process BMP images from disk as PNG attachments`（BMP → PNG，`mimeType === "image/png"`，
     文本里含 **【逐字】** `[Image converted from image/bmp to image/png.]`）、`should process text files normally`。
  即：这些测试断言的是「过滤发生在 `convertToLlm` 层（由 `sdk.ts` 包装），读取/预处理层**不**过滤」。
  本单元必须移植的只有 `sdk.ts` 里的 `convertToLlmWithBlockImages`（§2.12.4 阶段 E）+ `getBlockImages()`。

### 7.9 测试转写优先级

| 优先级 | 范围 | 理由 |
|---|---|---|
| P0 | `skills`、`prompt-templates`、`system-prompt`、`mcp-servers` | 纯函数，易逐字比对 |
| P0 | `package-manager` 的解析/identity/模式匹配 | 大量边界，回归风险最高 |
| P1 | `resource-loader` 的发现、优先级、worktree 去重 | 文件系统密集，需 `tempfile` 夹具 |
| P1 | `package-command-paths`（managed install 除外） | CLI 文案与 trust |
| P2 | `sdk-*` | 依赖 `Agent`/`ModelRuntime`，需 mock |
| P2 | `footer-data-provider` | 需要真实 git 仓库夹具与 `tokio::time::pause` |
| P3 | `mcp-command` / `mcp-extension` | 大部分属 `telemetry-mcp` |

---

## 8. Rust 模块布局与最难复现的部分

### 8.1 crate 归属（先解决冲突）

三份规范对 crate 的划分不完全一致：

| 规范 | 说法 |
|---|---|
| `ca-config-models.md` §8.1 | crate `kiana-coding-agent`（lib `kiana_coding_agent`）同时承载 `ca-cli`/`ca-session`/`ca-resources`/`ca-tools-*`，本单元代码放 `src/config/` 之类子模块 |
| `ca-session.md` §8.1 | 独立 crate `crates/ca-session/` |
| `ca-cli.md` §8.1 | 独立 crate `crates/kiana-cli/`（bin `kiana`）；**架构定稿已改名 `kiana-coding-agent`** |

**本规范的立场**：`kiana-resources` 有清晰的依赖方向（只依赖 `kiana-ai` / `kiana-agent-core` / `kiana-session` /
`kiana-config` 与若干工具 crate，被 `kiana-coding-agent` 与 `kiana-session` 同时使用），因此**建议独立 crate**
`crates/kiana-resources/`（lib 名 `kiana_resources`），并在 `ca-config-models` 的 `kiana-coding-agent` crate 里
`pub use kiana_resources::*` 以保持 TS 的导出面。若团队最终选择单 crate，则本单元的模块路径改为
`kiana-coding-agent/src/resources/**`，**代码内容不变**。

理由：`ca-cli` 需要 `package-manager-cli` 与 `DefaultPackageManager`；`ca-session` 需要
`ResourceLoader`/`McpServerRegistry`/`FooterDataProvider`；`ca-config-models` 需要 `SourceInfo` 与
`PackageInstallFailure`。若都塞进一个 crate 会产生循环（`kiana-session` ↔ `kiana-coding-agent`）；
拆出 `kiana-resources` 后依赖图为 DAG。

### 8.2 模块布局

```
kiana/crates/kiana-resources/
  Cargo.toml
  src/
    lib.rs                     # pub use（对齐 §5.1 的导出面）
    # 以下 4 个模块在骨架里是**再导出面**（定义在依赖链更低的 crate，见 ARCHITECTURE.md §3.5）：
    source_info.rs             # re-export `kiana_core::source_info`：SourceInfo / SourceScope / SourceOrigin /
                               # BUILTIN_PATH_PREFIX / getSyntheticPathSource / isSyntheticPath / create*SourceInfo
    diagnostics.rs             # re-export `kiana_core::diagnostics`：ResourceCollision / ResourceDiagnostic
    timings.rs                 # re-export `kiana_core::timings`：reset_timings / time / print_timings（OnceLock<bool>）
    manifest.rs                # re-export `kiana_core::kiana_manifest`：KianaManifest / read_kiana_manifest
    # slash_commands.rs 在骨架里**不存在**：`BUILTIN_SLASH_COMMANDS` / `SlashCommandInfo` 由
    # `kiana_core::slash_commands` 定义（`kiana-ext-api` L7 也要用），本 crate 只通过 `kiana_core` 引用。
    skills/
      mod.rs                   # Skill / SkillFrontmatter / LoadSkillsResult / load_skills
      validate.rs              # validate_name / validate_description（JS UTF-16 长度）
      discover.rs              # load_skills_from_dir_internal / load_skill_from_file
      prompt.rs                # format_skills_for_prompt / escape_xml
    prompts/
      mod.rs                   # PromptTemplate / load_prompt_templates
      args.rs                  # parse_command_args
      substitute.rs            # substitute_args（手写状态机，不用 regex，见 8.4）
      expand.rs                # expand_prompt_template
      frontmatter.rs           # 复用 kiana-config 的 parse_frontmatter（YAML 1.2）
    system_prompt/
      mod.rs                   # build_system_prompt / build_system_prompt_state /
                               # diff_system_prompt_sections
      sections.rs              # build_system_prompt_sections（preamble/tools/rules/docs）
      rules.rs                 # build_rules / shell guidance
      project_context.rs       # render_project_context
    mcp/
      mod.rs                   # McpServerConfig / McpExposure / validate_mcp_server_config
      exposure.rs              # get_mcp_tool_exposure / create_tool_name_matcher
      registry.rs              # McpServerRegistry / RegisteredMcpServer
      oauth.rs                 # validate_oauth / is_loopback_redirect_uri
    package/
      mod.rs                   # DefaultPackageManager / PackageManager trait / 公开类型
      types.rs                 # PathMetadata / ResolvedResource / ResolvedPaths / ProgressEvent / …
      parse.rs                 # parse_source / parse_npm_spec / git URL 解析 / identity
      npm.rs                   # npmCommand / install args / get_latest_npm_version / 全局根
      git.rs                   # install_git / ensure_git_ref / marker / prune / remote head
      paths.rs                 # install roots / get_temporary_dir / resolve_managed_path / gitignore
      patterns.rs              # apply_patterns / matches_any_pattern / overrides / expand_package_glob
      discover.rs              # collect_files / collect_skill_entries / auto entries / ancestor agents
      resolve.rs               # resolve / resolve_package_sources / dedupe / to_resolved_paths
      settings_io.rs           # add/remove source in settings / normalize for settings
      exec.rs                  # run_command / run_command_capture / run_command_sync / spawn helpers
      concurrency.rs           # run_with_concurrency（不取消其余任务）
    loader/
      mod.rs                   # DefaultResourceLoader / ResourceLoader trait / reload 步骤机
      context_files.rs         # load_context_file_from_dir / load_project_context_files /
                               # find_shadowed_context_file
      warnings.rs              # HOST_PROVIDED_EXTENSION_PACKAGES / describe_package_install_failure /
                               # format_package_install_warning / omit_replaced_extensions
      themes.rs                # load_themes / load_themes_from_dir / load_theme_from_file（调 kiana-tui）
      source_info_map.rs       # find_source_info_for_path / get_default_source_info_for_path / merge_paths
      dedupe.rs                # dedupe_prompts / dedupe_themes
      conflicts.rs             # detect_extension_conflicts
    sdk.rs                     # create_agent_session（依赖 kiana-session / kiana-config 注入）
    bug_report/
      mod.rs                   # collect_bug_report_metadata / diagnostics / files / archive / summary
      redact.rs                # is_sensitive_key / redact_url / redact_json_value / redact_settings
      environment.rs           # collect_environment / describe_model / provider / extension
      upload.rs                # upload_bug_report（reqwest multipart）
    crash_log.rs               # CrashRecord / read / write / record / take_unnotified / clear /
                               # find_extension_stack_matches
    footer.rs                  # GitPaths / find_git_paths / FooterDataProvider / fs-watch 重试
    pkg_cli/
      mod.rs                   # handle_package_command / handle_config_command
      parse.rs                 # parse_package_command
      help.rs                  # 5 段帮助文本
      managed.rs               # get_active_managed_install_root / run_managed_self_update / prune / cleanup
      self_update.rs           # get_self_update_plan / run_self_update / print_* 辅助
  tests/
    skills_parity.rs           # 对应 skills.test.ts 的 28 条
    prompts_parity.rs          # 对应 prompt-templates.test.ts 的 94 条
    system_prompt_parity.rs    # 对应 system-prompt.test.ts 的 16 条
    mcp_validation_parity.rs   # 对应 mcp 校验 9 条
    package_parse_parity.rs    # 对应 package-manager-ssh.test.ts + URL 相关 30 条
    package_resolve_parity.rs  # 对应 resolve/patterns 的 ~50 条（tempfile 夹具）
    loader_parity.rs           # 对应 resource-loader.test.ts 的 57 条
    loader_worktree.rs         # git worktree 夹具的 11 条
    bug_report_parity.rs
    crash_log_parity.rs
    footer_parity.rs           # tokio time pause + 真实 git 仓库
    pkg_cli_parity.rs          # 帮助文本与错误文案（assert_cmd）
    managed_install_parity.rs
```

### 8.3 关键类型草图

```rust
// source_info.rs
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

// package/types.rs
#[derive(Clone, Debug)]
pub struct PathMetadata {
    pub source: String,          // "local" | "auto" | "package" | "builtin" | "cli"
    pub scope: SourceScope,
    pub origin: PathOrigin,      // Package | TopLevel
    pub base_dir: Option<String>,
    pub package_root: Option<String>,
}
pub struct ResolvedResource { pub path: String, pub enabled: bool, pub metadata: PathMetadata }

// package/mod.rs
#[async_trait]
pub trait PackageManager: Send + Sync {
    async fn resolve(&self, on_missing: Option<&(dyn Fn(&str) -> BoxFuture<'_, MissingSourceAction> + Send + Sync)>)
        -> ResolvedPaths;
    async fn install(&self, source: &str, local: bool) -> Result<(), Error>;
    async fn install_and_persist(&self, source: &str, local: bool) -> Result<(), Error>;
    async fn remove(&self, source: &str, local: bool) -> Result<(), Error>;
    async fn remove_and_persist(&self, source: &str, local: bool) -> Result<bool, Error>;
    async fn update(&self, source: Option<&str>) -> Result<(), Error>;
    fn list_configured_packages(&self) -> Vec<ConfiguredPackage>;
    async fn resolve_extension_sources(&self, sources: &[String], local: bool, temporary: bool) -> ResolvedPaths;
    fn add_source_to_settings(&self, source: &str, local: bool) -> bool;
    fn remove_source_from_settings(&self, source: &str, local: bool) -> bool;
    fn set_progress_callback(&self, cb: Option<ProgressCallback>);
    fn get_installed_path(&self, source: &str, scope: InstallScope) -> Option<String>;
}

// loader/mod.rs
#[async_trait]
pub trait ResourceLoader: Send + Sync {
    fn get_extensions(&self) -> LoadExtensionsResult;
    fn get_skills(&self) -> SkillsResult;
    fn get_prompts(&self) -> PromptsResult;
    fn get_themes(&self) -> ThemesResult;
    fn get_agents_files(&self) -> Vec<ContextFile>;
    fn get_system_prompt(&self) -> Option<&str>;
    fn get_system_prompt_source(&self) -> Option<&str>;
    fn get_append_system_prompt(&self) -> &[String];
    fn get_append_system_prompt_sources(&self) -> Vec<String>;
    fn extend_resources(&mut self, paths: ResourceExtensionPaths);
    async fn reload(&mut self, options: Option<ResourceLoaderReloadOptions>);
}
```
要点：
- `PackageManager` 用 `async_trait`（对象安全）；`ResourceLoader::reload` 需要 `&mut self`，
  但 `AgentSession` 里是 `Arc<dyn ResourceLoader>`，因此实际实现用
  `Arc<Mutex<DefaultResourceLoader>>` 或把 `reload` 的 `&mut self` 改为 `&self` + `RwLock` 内部状态。
  **推荐后者**：`DefaultResourceLoader` 内部 `RwLock<LoaderState>`，`reload(&self)` 拿写锁；
  这样与 TS 的 `this.xxx = ...` 语义最接近。
- `LoadExtensionsResult` 来自 `kiana-session`，本 crate 只 re-export。
- `PathMetadata` 不加 `Serialize`（不落盘）；若需要，键名必须与 TS 一致（`source`,`scope`,`origin`,`baseDir`,`packageRoot`）。

### 8.4 最难复现的部分（按风险排序）

**1. `package-manager.ts` 的「添加顺序即优先级」在 Rust 里极易被排序破坏**
`addResource` 首次写入胜出 + `toResolvedPaths` 稳定排序 + `canonicalizePath` 去重，三者顺序不能动。
Rust 用 `IndexMap<String, (PathMetadata, bool)>`（**不是** `HashMap`），排序用 `sort_by_key(rank)`
（`sort_by_key` 是稳定排序），去重用 `HashSet<String>` 保插入序。
回归测试 `2781-skill-collision-precedence` 会直接暴露任何「先排序后添加」的实现。

**2. 子进程语义的 4 个细节**
- `runCommand` 用 `exit`（进程退出即 resolve），`runCommandCapture` 用 `close`（管道 EOF 后）。
  Rust：`child.wait()` 对应 exit；`wait_with_output()` 对应 close 且**会先把 stdout/stderr 读尽**。
- `stdio: ["ignore", 2, 2]`（`isStdoutTakenOver()` 时）——需 `Stdio::from_raw_fd(2)`，
  Windows 无对应物（退化 `inherit`）。
- 超时用 `child.kill()`（SIGKILL），`timedOut` 标志决定错误文案；
  **超时后仍要等 close 才 reject**。
- `runCommandSync` 的 `status !== 0` 与 `result.error` 都算失败，错误文案用 `error.message || stderr || stdout`。
  Rust：`Output::status`；`Command::output()` 不会因非零退出返回 `Err`，需手动判断。

**3. minimatch 语义**
`applyPatterns` / `matchesAnyPattern` / `matchesAnyExactPattern` / `isEnabledByOverrides` /
`applyAutoloadDisabledPatterns` 全部依赖 minimatch，且要匹配 6 种候选形式（含 SKILL.md 的父目录三种）。
Rust 的 `globset` 没有 `!` 否定、没有 `{a,b}`（有 `{}` 但不完整）、没有 `#` 注释。
**建议**：写一个 `MiniMatch` 结构体，实现 `*`、`?`、`**`、`{a,b}`、`[abc]`、`[!abc]`、
`!(pattern)` 扩展 glob（可选）、前导 `!` 否定、`#` 注释、`dot` 选项；用 `miminmatch` 的 JS 测试做黄金对比。
若时间紧，可先支持 `*`/`?`/`**` 并让不支持的语法按字面量失败（会与 TS 不同，必须记录为已知差异）。

**4. `ignore` crate 与 `.gitignore` 前缀化**
`addIgnoreRules(ig, dir, root)` 会给每行加 `relDir + "/"` 前缀（§2.6.2），因为 JS 的 `ignore` 库
按「相对于 root」匹配。Rust 的 `GitignoreBuilder` 支持把规则绑定到某个目录（`add(path)` 读文件），
但**逐行加前缀的语义**（尤其是 `!` 否定与 `/` 前导）必须手写，不能直接用 `add_line`。
另外要复刻「每个递归层级都重新 `addIgnoreRules`」的行为（同一个 matcher 累积规则）。

**5. `parseCommandArgs` 与 `substituteArgs` 的状态机**
`parseCommandArgs` 只有两种引号且**没有反斜杠转义**（引号内的 `\"` 由测试覆盖，行为是把 `\` 保留还是
吃掉取决于源码实现——见 §2.7 的逐字状态机）。`substituteArgs` 是一个 5 分支正则 + 手写逻辑，
且**非递归**（替换结果不再被扫描）。Rust 用单次扫描实现，**禁止用 `regex::Regex::replace_all` 后再扫描一遍**。
黄金测试：`prompt-templates.test.ts` 的 94 条，其中 6 条专门验证非递归。

**6. `system-prompt.ts` 的 section 拼装与 diff**
- `preamble` 无标签，其余包 `<name>\n…\n</name>`；
- 空内容 section 被**丢弃**（不是输出空标签）；
- `buildSystemPrompt` 用 `getSystemMessageText` 以 `\n\n` 连接非空 part；
- `diffSystemPromptSections` 生成 patch：新增/删除/修改的顺序、`undefined` 与空数组的区别。
`system-prompt-updates.test.ts` 的 `diffs sections into a patch` 会逐字段断言。
注意 `SYSTEM_PROMPT_SECTION_NAME` 校验与 `Invalid system prompt section name: ${name}`。

**7. `resource-loader.reload()` 的 27 步顺序**
任何一步提前/延后都会改变优先级、metadata 归属或诊断顺序。特别是：
- 第 8 步 `resourceMetadataByPath = new Map()` 与 `metadataByPath` 的别名；
- 第 10 步「先记录**全部**资源（含 disabled）的 metadata」；
- 第 12 步 CLI metadata 的 `{source:"cli", scope:"temporary"}`；
- 第 14 步 `disabledBuiltinExtensions` 过滤只作用于最终路径列表；
- 第 21/22 步 `noSkills`/`noPromptTemplates` 时仍保留 CLI 路径与 additional 路径；
- 第 23 步主题路径诊断**不检查** `isLocalPath`。
建议把 `reload` 写成显式的 `enum ReloadStep` 序列并加日志（`KIANA_TIMING` 之外），以便对比。

**8. `findShadowedContextFile` 的字符串前缀比较与 canonicalize**
```
worktreeRoot.startsWith(`${mainRepoRoot}${sep}`)
canonicalizePath(join(mainRepoRoot, ".git")) !== commonGitDir
join(canonicalizePath(currentDir), basename(contextFile.path)) === shadowedContextFile
```
三处都是**字符串**比较。Rust 必须用 `str::starts_with` 而非 `Path::starts_with`；
`canonicalizePath` 失败回退原字符串。11 条 worktree 测试覆盖 bare layout、submodule、sibling worktree、
symlink 主仓库文件等边界，全部依赖这些字符串细节。

**9. `mapSkillPath` 与 `findSourceInfoForPath` 的前缀匹配顺序**
`findSourceInfoForPath` 先查 `extraSourceInfos`（`extendResources` 注入，插入序），再查
`metadataByPath`（精确优先，再插入序前缀匹配）。`IndexMap` 的插入序必须是 TS 的插入序
（`addResource` 的顺序 + CLI metadata 的顺序）。顺序错会导致扩展提供的技能拿到 package 的 sourceInfo。
`resource-loader.test.ts` 的 `should keep package metadata for skills, prompts, and themes` 会断言。

**10. `formatExtensionPackageWarnings` 的 `JSON.parse` 未捕获**
`collectExtensionPackageWarnings` 在 package.json 损坏时**抛出**（resource-loader.reload 不捕获），
而 `readKianaManifest` 对同一文件返回 `null`。两个行为并存：
- `package-manager` 的 `collectPackageResources` → `readKianaManifest`（容错，返回 null）；
- `resource-loader` 的 `collectExtensionPackageWarnings` → 直接 `JSON.parse`（抛错，向上冒泡）。
回归测试 `7187-malformed-package-manifest` 覆盖前者，`resource-loader.test.ts` 的
`should fail when an extension package manifest cannot be parsed` 覆盖后者。**不要统一成一种**。

**11. `McpServerRegistry` 的克隆与顺序**
`list()` 返回深拷贝（`structuredClone`），但**顺序是注册序**且同名替换不改变位置。
`unregister(name, extensionPath)` 只有在 owner 匹配时才删除。`setChangeListener` 支持 `undefined`。
`register` 触发 listener，`unregister` 仅在真的删除时触发。

**12. `crash-log.findExtensionStackMatches` 的帧过滤与边界字符**
- 跳过 stack 第一行（错误消息本身）；
- 只保留 `/^\s+at\s/` 的行（**至少一个空白**，`at` 后至少一个空白）；
- `decodeURI` 逐行独立，失败保留原行；
- `\\` → `/`；
- 边界字符集合：结尾、`:`、`)`、任意空白（`\s`，含 `\n`、Tab、全角空格？JS `\s` 含 `﻿` 等）。
  Rust 的 `char::is_whitespace` 与 JS `\s` **不完全相同**（JS `\s` = `[\f\n\r\t\v    -     　﻿]`）；
  必须手写 `js_is_whitespace`。
- Windows 盘符触发大小写不敏感（`/^[a-z]:\//iu` 对 `target`，然后两边 lowercase）。

**13. `footer-data-provider` 的 watcher 生命周期**
`setupGitWatcher` → 失败 → `handleGitWatcherError` → `clearGitWatchers` + `scheduleGitWatcherRetry(5000)`。
`watchWithErrorHandler` 在**同步失败**时也调用 `onError`，因此「创建失败 → 清空 → 5s 重试」闭环。
`refreshInFlight`/`refreshPending` 的合并语义、500ms 去抖、以及「watcher 为 null 但 pollGitHead 为 true 时继续」
的早退条件都要精确。8 条测试用 `tokio::time::pause` + `advance` 可复刻（TS 侧用 fake timers）。

**14. `Date.toLocaleString()` 与 `Date.toISOString()` 的混合**
- `crashes.json` 的 `timestamp` 用 `toISOString()`（可精确复刻）；
- 启动崩溃通知的 `when` 用 `toLocaleString()`（**不可移植**）。
建议：先在 Rust 侧确定 ICU 数据源（`icu_datetime` + `LANG`），并加一个「同 locale 下与 Node 输出一致的」
黄金测试；若无法一致，在 §8.6 记录为已知差异（这属于用户可见文本）。

**15. `proper-lockfile` 的 managed 更新锁**
`lockSync(dir, {realpath:false})` 创建 `<dir>.lock`；`lock(dir)` 返回 release；
`ELOCKED`（已被持有）要转换为 `Another managed Kiana update is already running.`。
Rust 用 `create_dir` 原子性 + 陈旧检测（mtime > 10s 视为陈旧）+ 5000ms 心跳 update。
测试 `rejects a concurrent managed update` 依赖该错误映射。

**16. `KIANA_TIMING` 的模块加载时求值**
`export const ENABLED = process.env.KIANA_TIMING === "1";` —— Rust 用
`static ENABLED: OnceLock<bool>` 并在 `crate::timings` 首次访问时初始化。
**注意**：`resetTimings(namespace = "main")` **只**把该 namespace 的条目重置为
`{timings: [], lastTime: Date.now()}`（没有「title」概念，也没有全局清空——`resetTimings("extensions")`
不会影响 `"main"`）；`printTimings()` 的输出顺序是
**维护插入顺序**的 map，不是按耗时排序。

**17. 环境变量与路径的 `~` 展开/Windows 归一**
`resolvePath(input, baseDir, {homeDir, trim, expandTilde, normalizeUnicodeSpaces, stripAtPrefix})`
的默认值（`expandTilde: true`）与本单元**三处**不同调用必须分别复刻：

| 调用点 | options | 备注 |
|---|---|---|
| `package-manager.ts` 的私有 `resolvePath`/`resolvePathFromBase` | `{homeDir: getHomeDir(), trim: true}` | 显式 `homeDir`（`HOME \|\| homedir()`） |
| `skills.ts` / `prompt-templates.ts` 的显式路径、`resource-loader.ts` 的 `resolveResourcePath` | `{trim: true}` 或 `{}` | 用默认 `homedir()` |
| `resource-loader.ts` 的 `resolveExtensionLoadPath` | `{normalizeUnicodeSpaces: true}` | **不 trim**；`extensions/loader.ts` 内部也用它 |

旧版说「两处」不准确：`{normalizeUnicodeSpaces: true}` 只在 `resource-loader.ts` 一处，但 `{trim: true}`
出现在 `package-manager`（带 `homeDir`）、`skills`、`prompt-templates`、`resource-loader`
（`resolveResourcePath`）共 4 个位置，且 `resource-loader.ts` 的 `resolveExtensionLoadPath`
**不带** `trim`。`UNICODE_SPACES` 的源码字面量（**逐字**）是：
```
const UNICODE_SPACES = /[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g;
```
即：`U+00A0`（NBSP）、`U+2000`–`U+200A` 区间的 11 个空格变体、`U+202F`（narrow no-break）、
`U+205F`（medium mathematical space）、`U+3000`（表意空格）。**不含** `U+200B`（zero width space）、
`U+FEFF`（BOM）与 `U+1680`。Rust 用显式 `char` 匹配，**不要**用 `char::is_whitespace`（涵盖范围不同）。

**18. ZIP 字节一致性**
`src/utils/zip.ts` 已读完，布局与时间戳公式见 **§4.3.1**。要点：本地头 30 字节
（sig `0x04034b50`、version 20、flags `0x0800`、method 8、无 extra）、中央目录 46 字节
（sig `0x02014b50`、version-made-by/needed 均为 20、偏移在字节 42、30–41 全 0）、
EOCD 22 字节（sig `0x06054b50`、条目数在 8/10、目录长度在 12、偏移在 16）；
`deflateRawSync` 默认压缩（level 6）、`crc32`、DOS 时间戳**取本地时区**且**整个归档只取一次
`new Date()`**（`time = 时<<11|分<<5|秒>>1`，`day = (max(1980,年)-1980)<<9|(月+1)<<5|日`）。
**已核实：全仓没有任何测试解压/比对归档字节**（`bug-report.test.ts` 只测文案与脱敏），
所以 Rust 侧用 `zip` crate 即可通过现有测试；只有要与 TS 版本逐字节对拍时才需复刻，
且必须固定 `TZ` 并注意 zlib 实现差异。

**19. `convertToLlmWithBlockImages` 的「相邻去重」**
过滤条件依赖**过滤后数组的索引**（`arr[i-1]`），而 `arr` 是 `map` 后的结果。Rust 必须用一个
可变 `Vec` 构建并检查 `last()`，而不是先 map 再 filter（顺序不同）。
且只在 `role ∈ {user, toolResult}` 且 `content` 是数组且含 image 时改写。

**20. `createAgentSession` 的闭包捕获顺序**
`cacheContextIsCurrent` 在 `agent` 构造**之前**定义但引用 `agent.state`（TDZ 之外的延迟求值），
`extensionRunnerRef` 是可变的。Rust 必须用 `Arc<Mutex<...>>`/`Arc<RwLock<...>>` 表达这种
「先定义、后注入」的引用，且保证 `streamFn` 里的读取发生在注入之后。

### 8.5 验收策略

1. **逐字黄金测试**：所有 §3 的字符串用 `insta` 快照；所有 §2 的错误串用 `assert_eq!`。
2. **差分测试**：对纯函数（skills/prompts/system-prompt/mcp/patterns/URL 解析）用同一批 JSON 输入
   分别跑 TS 与 Rust，比对输出字节。建议写一个 `xtask parity` 脚本：TS 侧用 `bun` 跑一个 dump 程序，
   Rust 侧用 `cargo run --bin dump`，`diff` 两边。
3. **文件系统夹具**：`tempfile::TempDir` + 显式创建 `.git`、`SKILL.md`、`package.json`；
   `resource-loader.test.ts` 的 worktree 用例需真实 `git worktree add`（可用 `git2` 或直接调用 `git`）。
4. **时间控制**：footer 的 500ms/1000ms/250ms/5000ms 用 `tokio::time::pause()`；
   不要把时长改成常量以便测试（会改变生产行为）。
5. **进程替身**：`package-manager.test.ts` 通过注入假的 `npm`/`git` 可执行文件（PATH 前置脚本）验证 argv。
   Rust 测试同样用临时目录下的 shell 脚本（Windows 用 `.cmd`），断言收到的 argv 逐项相等。
6. **CI 中的 npm 依赖检查**：`install-lock` 的版本与 `coding-agent/package.json` 一致（见 §2.17）。

### 8.6 已知差异与决策点（需团队确认）

| # | 问题 | 现状（TS） | 建议 |
|---|---|---|---|
| 1 | `Date.toLocaleString()` 的崩溃通知时间 | 依赖 ICU/locale | 用 `icu_datetime` + `LANG`；若不可行，固定 `en-US` 并记录差异 |
| 2 | `stdout.columns ?? 80` 的 `printSelfUpdateNote` | 读 TTY 宽度，`max(20, …)` | Rust 用 `terminal_size` crate；非 TTY 时 80 |
| 3 | `fs.watch` 在 Linux 的 inotify 与 `notify` 的差异 | 目录监视 + filename 过滤 | `notify` 的 `Event::paths` 需归约为「任意变更」；`filename` 过滤逻辑保留但基本恒真 |
| 4 | `fs.watchFile` 的 `ctimeMs` | Linux 有，Windows 无 | Windows 上退化为 `mtime`+`size` |
| 5 | `cross-spawn` 的 Windows shim | 自动 | 必须自写 `.cmd` 解析（含 `PATHEXT`、`cmd /c`、参数引号规则） |
| 6 | `proper-lockfile` 的 `compromised` 回调 | 本单元未使用（只有 `lock`/`lockSync`） | 不需要实现 |
| 7 | `structuredClone` 对 `Map`/`Set` | 支持 | `McpServerRegistry` 只克隆普通对象，够用 |
| 8 | `fetch` 的 `FormData` boundary | 随机 | 服务端不应依赖；测试只校验 part 名与内容 |
| 9 | `uuidv7` 的单调性 | `uuid` 包的实现 | `uuid` crate `now_v7()` 也保证单调（同进程内） |
| 10 | `readdirSync` 的返回顺序 | 未排序（文件系统顺序） | Rust `read_dir` 同样未排序；**必须保持「不排序」**（`glob` 结果才排序） |
| 11 | `JSON.stringify` 对 `undefined` 字段 | 丢弃 | `serde` 的 `skip_serializing_if = "Option::is_none"` |
| 12 | `Number` 与 `i64` | 大整数精度 | 本单元数字多为布尔/小整数；`timeoutMs` 用 `u64` |
| 13 | `signal`/`AbortSignal` 在 `fetch` 与 `spawn` | 支持 | `CancellationToken` + `tokio::select!` |
| 14 | `process.exit(0)` 在 `handleConfigCommand` | 立即退出 | Rust 返回 `ExitAction::Exit(0)` 由 `kiana-coding-agent` 执行；**不要在库层直接 `std::process::exit`** |
| 15 | 主题解析依赖 `@earendil-works/kiana-tui` | 外部 | 注入 `ThemeLoader` trait，实现放在 `kiana-tui` crate |
| 16 | `Markdown` 渲染（更新说明） | `kiana-tui` | 同 15，注入 `MarkdownRenderer` |
| 17 | `getKianaUserAgent` 的运行时描述 | `node/vX` 或 `bun/vX` | Rust 无 Node；**必须**输出 `node/<编译期目标版本>` 还是 `bun/…`？建议按「模拟 Node」输出 `node/v22.19.0`（与 `engines` 一致），并加配置项 |
| 18 | 崩溃日志的 `stack` 格式 | V8 格式 | Rust 的 `std::backtrace` 格式完全不同；`findExtensionStackMatches` 只用于**读取历史 V8 栈**（来自旧 TS 版本的 crashes.json），Rust 自身写入的栈不需要匹配。**保留解析逻辑以兼容旧文件** |
| 19 | 崩溃通知里的 `Date.toLocaleString()` 与 ZIP 的 DOS 本地时间戳 | 都依赖本机时区/locale | 两者都不是「跨机器确定」的值。**测试里必须固定 `TZ` 与 locale**；若要生成可复现的归档，允许注入时钟（`fn now() -> DateTime<Local>`），但默认实现必须用本地时间（见 §4.3.1） |
| 20 | `writeZipArchive` 不创建父目录 | `fs.writeFile` 直接失败 | Rust 用 `tokio::fs::write`，**不要**顺手加 `create_dir_all`（会掩盖调用方的路径错误）。`exportZip` 写到 `process.cwd()`，正常情况下父目录存在 |

### 8.7 移植顺序（最终建议）

1. `source_info` → `diagnostics` → `timings` → `manifest` → `slash_commands`（无依赖，建立测试骨架）。
2. `skills` → `prompts` → `system_prompt`（纯逻辑，黄金测试密集）。
3. `mcp`（校验 + 注册表，无 IO）。
4. `package/parse.rs` + `package/patterns.rs`（URL 与 glob，全部可用单元测试覆盖）。
5. `package/discover.rs` + `package/resolve.rs` + `package/settings_io.rs`（文件系统夹具）。
6. `package/npm.rs` + `package/git.rs` + `package/exec.rs`（假可执行文件夹具）。
7. `loader/*`（依赖 2–6；worktree 夹具）。
8. `footer.rs` + `crash_log.rs` + `bug_report/*`（相对独立）。
9. `sdk.rs`（依赖 `kiana-session`，最后接入）。
10. `pkg_cli/*`（依赖 6 与 `kiana-coding-agent` 的 chalk/trust/TUI）。
11. 全量差分测试 + 文档回填。

---

## 附录 A：本规范覆盖的源文件清单（绝对路径）

```
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/package-manager.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/resource-loader.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/skills.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/prompt-templates.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/system-prompt.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/mcp-servers.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/sdk.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/bug-report.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/bug-report-upload.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/crash-log.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/footer-data-provider.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/source-info.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/kiana-manifest.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/slash-commands.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/timings.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/diagnostics.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/package-manager-cli.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/install-lock/package.json
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/install-lock/package-lock.json
```

支撑文件（只读参考）：

```
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/paths.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/frontmatter.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/text.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/fs-watch.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/git.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/child-process.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/zip.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/utils/kiana-user-agent.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/config.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/settings-manager.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/settings-schema.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/client-identity.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/telemetry.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/radius.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/trust-manager.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/auth-guidance.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/defaults.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/cache-warmer.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/virtual-models.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/compaction/compaction.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/compaction/utils.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/extensions/types.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/modes/interactive/theme/theme.ts
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/modes/interactive/interactive-mode.ts
/home/ctyun/code/kiana_rev/kiana/packages/ai/src/utils/identity.ts
```

文档：

```
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/docs/packages.md
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/docs/skills.md
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/docs/prompt-templates.md
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/docs/slash-commands.md
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/docs/sdk.md
/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/docs/mcp.md
```

## 附录 B：本单元导出的符号计数

按源码中 `^export (async )?(function|const|let|class|interface|type|enum) ` 统计（不含 `export type { … }`
形式的再导出，也不含 `export * from`）：

| 类别 | 数量 |
|---|---|
| `function`（含 `async function`） | 49 |
| `const` | 6 |
| `class` | 4 |
| `interface` | 37 |
| `type` | 14 |
| `enum` | 0 |
| **合计** | **110** |

按文件分布：`package-manager.ts` 12、`resource-loader.ts` 8、`skills.ts` 8、`prompt-templates.ts` 7、
`system-prompt.ts` 8、`mcp-servers.ts` 16、`sdk.ts` 3、`bug-report.ts` 13、`bug-report-upload.ts` 1、
`crash-log.ts` 6、`footer-data-provider.ts` 4、`source-info.ts` 8、`kiana-manifest.ts` 2、
`slash-commands.ts` 4、`timings.ts` 3、`diagnostics.ts` 2、`package-manager-cli.ts` 5。
另有 `export type { … }` 形式的再导出 5 处（`resource-loader.ts` 的 `ResourceCollision`/`ResourceDiagnostic`、
`sdk.ts` 的 4 组类型再导出）与 `export * from "./agent-session-runtime.ts"`；这些不计入 110。

`exportedSymbolCount` 在结构化输出中填 **110**。


---

## Audit additions

> 本节由一次对抗式完整性审计追加（独立通读 `packages/coding-agent/src/core/` 全部在范围内的源文件、
> 支撑工具、`src/package-manager-cli.ts`、`install-lock/**`、6 份 docs 与 test/ 下全部相关测试后编写）。
> 上面正文里**已就地改正**的错误不再重复；这里只补**正文缺失或表述不足**的内容。
> 行号以审计时的 TS 源码为准（文件行数见 §0.1，已逐字核对）。

### A0. 结论摘要

- 正文 §2 的逐文件行为描述与 §4 的外部格式**保真度很高**（抽查的上百处常量、文案、分支顺序均一致）。
- 缺陷集中在三类：(a) §5.1/§5.2 的**接口签名**（多处与源码不符，已在正文修正）；
  (b) §7 的**测试运行方式**与个别测试的**断言内容**（已在正文修正 `bun test` → `vitest`、`block-images`）；
  (c) **测试清单遗漏**：有 **8** 个 import 本单元模块（7 个运行期 + 1 个仅类型）、但 §7 完全没提的
  测试文件；另有 1 个（`block-images.test.ts`）只间接依赖本单元（见 A5）。
- 另发现若干**语义缺口**（A1–A4），主要影响 Rust 侧默认值、错误串逐字性、以及「谁负责什么」的边界。
- 第 (a) 类共在原位改写 **20+ 处**签名/类型块（`§5.1` 全部接口段、`§5.2` 的 SettingsManager /
  ModelRuntime / SessionManager / frontmatter / paths 等）；第 (b)(c) 类与新增语义见 A1–A10。
  统计：原位改写 **28** 处（其中**错误陈述 22 处**、缺漏签名/字段 6 组）、新增缺失条目 **72** 条
  （A1:4 / A2:7 / A3:13 / A4:9 / A5:8+1间接 / A6:9 / A7:7 / A8:5 / A9:5 / A10:5）。

### A1. 默认值与三态（Rust 侧最容易写错）

1. **`agentDir` 与 `authPath`/`modelsPath` 的三态**（§2.12.4 阶段 A 已写「只有显式传 `agentDir` 才传路径」，
   但没写清后果）：
   - `options.agentDir === undefined` → `authPath = modelsPath = undefined` → `ModelRuntime.create({})`
     → 运行时用 `join(getAgentDir(), "models.json")` 与默认 `auth.json`（**仍会读磁盘**，只是路径由
     运行时自己算）。
   - `options.agentDir` 显式给出 → `authPath = join(agentDir, "auth.json")`、`modelsPath = join(agentDir, "models.json")`。
   - `modelsPath === null` 才表示「不读 models.json」（`InMemoryCodingAgentModelsStore`）。
   因此 Rust 的 `CreateModelRuntimeOptions.models_path` 必须是 `Option<Option<PathBuf>>`，
   不能压成 `Option<PathBuf>`。
2. **`SettingsManager.create` 的 `projectTrusted` 默认是 `true`**（`settings-manager.ts`
   `options.projectTrusted ?? true`），而 `package-manager-cli.ts` 的 `createCommandSettingsManager`
   显式传 `{projectTrusted:false}` 再按 trust 流程 `setProjectTrusted`。
   正文 §2.16.6 描述了 CLI 侧的流程，但没写 `SettingsManager.create` 的**默认信任**——
   Rust 侧若把默认设成 `false`，`kiana install`/`kiana list` 之外的所有路径行为都会变。
   未信任时**项目 settings 整体被跳过**：`loadFromStorage(storage, "project", !projectTrusted)` 直接
   返回 `{}`（不读文件、不报错），因此 `getProjectSettings()` 是空对象而**不是**读到的内容。
3. **`DefaultResourceLoader` 未传 `settingsManager` 时**会 `SettingsManager.create(cwd, agentDir)`（默认信任 true），
   随后 `reload()` 第 1 步就 `setProjectTrusted(false)` + `reload()` 再按 `resolveProjectTrust` 决定
   （§2.11 的 27 步已覆盖，此处强调：**构造时是 trusted，reload 一开始就翻成 false**，
   所以 `resolveProjectTrust` 回调看到的是「未信任」状态下的 extensions 结果）。
4. **`loadSkills` 的 `agentDir` 在类型上必填**，但实现里仍是 `agentDir ?? getAgentDir()`
   （§2.6.6 第 1 步已注明「历史遗留」）。Rust 侧用必填参数即可，但**不要**因此改动行为：
   传 `""` 会得到 `resolvePath("")` 而不是默认目录。

### A2. 逐字错误串的「兜底分支」规律（影响 `[逐字]` 断言）

本单元大量错误消息形如
```ts
const message = error instanceof Error ? error.message : "<字面量>";
```
其中**字面量只在非 `Error` 抛出时出现**。审计中确认的位置（全部应保持同一规律）：

| 位置 | 正常情况下的 message | 兜底字面量 |
|---|---|---|
| `skills.ts` 读文件失败 | `readFileSync` 的 `Error.message`（含 `ENOENT: …`） | `failed to read skill file` |
| `skills.ts` YAML 解析失败（**仅 `SKILL.md`**） | `yaml` 包的错误消息 | `failed to parse skill file` |
| `prompt-templates.ts` 读文件失败 | 同上 | `failed to read prompt template file` |
| `prompt-templates.ts` YAML 解析失败 | `yaml` 包的错误消息 | `failed to parse prompt template file` |
| `prompt-templates.ts` 显式路径 `statSync` 失败 | 同上 | `failed to read prompt template path` |
| `skills.ts` 显式路径 `statSync` 失败 | 同上 | `failed to read skill path` |
| `resource-loader.ts` `resolvePromptInput` 读文件失败 | `String(error)` 插值（`Could not read ${description} file ${input}: ${error}`） | —（不是兜底，直接打印） |

Rust 侧建议统一封装 `js_error_message(&dyn Error, fallback: &str) -> String`，并对
`io::Error` 复刻 Node 的 `ENOENT: no such file or directory, open '<path>'` 形态
（`resource-loader.test.ts`、`skills.test.ts` 有若干 `.toContain("ENOENT")` 类断言；
若 Rust 不做映射，这些测试只能改写——必须在 §8.6 记录为已知差异）。

### A3. `package-manager.ts` 正文未逐条列出的行为

以下均已核对源码，正文缺失或只有一句带过：

1. **`getEnv()`**：仅当 `platform === "linux"` **且** `Object.keys(process.env).length === 0` 时读
   `/proc/self/environ`（`\0` 切分、`=` 首个出现处分割、`idx > 0` 才收）。这是 Bun 编译二进制的路径。
   **`spawnCommand`/`spawnCaptureCommand`/`runCommandSync` 全部用它**，所以 Rust 侧要有一个
   `fn env_snapshot() -> IndexMap<String, String>` 单点，而不是每次 `std::env::vars()`。
2. **`getPackageManagerName()`**：从 `npmCommand` argv 里取 basename（`\.(cmd|exe)$/i` 去掉）：
   ① 若 `args.lastIndexOf("--") >= 0` → 用 `--` 之后那个 token（为空则退回 `command` 的 basename）；
   ② 否则 `command` 的 basename ∈ **`{npm, pnpm, bun}`**（**不含 `yarn`**）→ 直接返回；
   ③ 否则在 args 里找所有属于该集合的 basename，去重后**多于一个 → 抛
   `Ambiguous npmCommand package managers: ${wrapped.join(", ")}`**，否则返回唯一值，仍无则返回直接 basename。
   它决定 `getGitDependencyInstallArgs` / `installNpm` / `uninstallNpm` / `npmHasAvailableUpdate` 的参数拼装
   （`yarn` 只出现在 `ca-cli` 的 self-update 命令构造里，**不在**包管理器名判定中）。
3. **`getGlobalNpmRoot()` 有进程内缓存**：`this.globalNpmRoot` + `this.globalNpmRootCommandKey`
   （`[command, ...args].join("\0")`）。**npmCommand 变了才重算**。Rust 侧对应 `Mutex<Option<(String, PathBuf)>>`，
   不要每次跑 `npm root -g`（`package-manager.test.ts` 有计数式断言）。
4. **`pruneEmptyGitParents`** 的条件是 `current.startsWith(resolvedRoot)`（**字符串前缀**）**且**
   `current !== resolvedRoot`；`readdirSync` 非空 → `break`；`rmSync` 抛错 → `break`（保留残余目录）。
5. **`ensureNpmProject`** 顺序：`mkdirSync` → `markPathIgnoredByCloudSync(installRoot)` → `ensureGitIgnore`
   → 仅当 `package.json` 不存在才写 `{"name":"kiana-extensions","private":true}`（2 空格、**无结尾换行**）。
   `ensureGitIgnore` 的 `"*\n!.gitignore\n"` 只在文件不存在时写（不覆盖用户改动）。
6. **`cleanAndInstallGitDependencies` 的失败路径**：`git clean -fdx` 抛错 → 先
   `repairMissingGitDependencies(targetDir).catch(()=>{})`（尽力补回 node_modules）**再原样 rethrow**；
   marker 只在最后一步成功后才删。
7. **`ensureGitRef` 的 `commitRef`**：`fetchArgs` 之后 `rev-parse HEAD` 与 `` rev-parse `${ref}^{commit}` ``
   都用 `timeoutMs: NETWORK_TIMEOUT_MS`；相等且 marker 存在 → `cleanAndInstallGitDependencies`；
   相等且无 marker → `repairMissingGitDependencies`；不等 → **先写 marker 再 reset**。
   注意 reset 用的是 `` `${ref}^{commit}` ``（带 `^{commit}`），而相等性比较也用同一串。
8. **`refreshTemporaryGitSource`** 的 progress action 是 **`"pull"`**（不是 `"update"`），
   文案 `Refreshing ${sourceStr}...`，且异常**全部吞掉**（保留旧缓存）。
9. **`updateConfiguredSources` 的并发**：npm 按 scope 批量（`updateNpmBatch`），git 用
   `runWithConcurrency(tasks, GIT_UPDATE_CONCURRENCY=4)` 且 `.then(()=>{})` 把结果抹平；
   两组都 push 进同一个 `tasks` 后 `await Promise.all(tasks)`——npm 批与 git 批**并行**。
10. **`checkForAvailableUpdates()`** 离线返回 `[]`；顺序是**先 project 后 global**（与 `resolve()` 一致），
    `UPDATE_CHECK_CONCURRENCY = 4`；返回项 `scope` 排除 `"temporary"`。
11. **`addSourceToSettings`/`removeSourceFromSettings`**：改的是 settings 数组的**副本**再
    `setProjectPackages`/`setPackages`；`addSourceToSettings` 对已存在项只把字符串形式规范化为
    `{...existing, source}`（**不重复追加**）；两者都返回 `boolean`（未命中/已在 → `false`/`true` 的语义见 §2.10）。
12. **`spawnCaptureCommand` 的 env 合并**：`{...getEnv(), ...options.env}`（后者覆盖前者）；
    `spawnCommand` 不合并额外 env。`stdio` 决策 `isStdoutTakenOver() ? ["ignore",2,2] : "inherit"` 只影响 `spawnCommand`。
13. **`runCommand` resolve 用 `exit`，非零 → reject `${command} ${args.join(" ")} failed with code ${code}`**；
    `runCommandCapture` 用 `close`（先读尽管道），同样拼该错误串。**两者的 timeout 都走 `child.kill()`
    （SIGKILL）后仍等 close**（§8.4 第 2 条已写，此处补「错误串里 args 用空格 join，不转义」）。

### A4. `resource-loader.ts` / `sdk.ts` 正文未列出的点

1. **`resolvePromptInput(input, description)`**（§2.11.5 已给函数体）：输入若**是存在的文件路径**
   （`existsSync(input)`）则读文件并 `stripBom`；读失败 → `console.error(chalk.yellow(...))` 并**回退为
   把 `input` 当字面 prompt 文本**。因此 `systemPrompt:` 既可以是文本也可以是文件路径——
   Rust 侧不要只实现「文本」分支，否则 `--system-prompt file.md` 行为不同。
2. **`getSystemPromptSource()` / `getAppendSystemPromptSources()`**：
   - `systemPromptSourcePath = systemPromptSource && existsSync(systemPromptSource) ? resolvePath(systemPromptSource)
     : undefined`，`getSystemPromptSource()` 返回 `{path}` 或 `undefined`。
   - `appendSystemPromptSourcePaths = appendSources.filter(s => existsSync(s)).map(s => resolvePath(s))`，
     `getAppendSystemPromptSources()` 返回 `appendSystemPromptSourcePaths.map(path => ({path}))`。
   **关键**：这个数组是**过滤后**的（只含「确实存在于磁盘」的项），因此**下标与
   `getAppendSystemPrompt()` 的文本数组不对齐**——不要试图用索引把两者配对。
   未显式传 `appendSystemPrompt` 时，`appendSources` 来自 `discoverAppendSystemPromptFile()`（0 或 1 项）。
   `resolvePromptInput` 的返回值恒为 `string`（读失败时回退输入本身），因此 `baseAppend` 的
   `.filter(s => s !== undefined)` 在 TS 里不会真的过滤掉任何东西。
3. **`loadProjectContextFiles` 的去重**：`seenPaths` 存**原始路径**（未 canonicalize），
   只有 `isShadowed` 判定里用 `join(canonicalizePath(currentDir), basename(contextFile.path))`。
   即「同一文件经符号链接出现两次」**会**被加载两次（正文已提一句，此处明确**不要**改成 canonicalize 去重）。
4. **`DefaultResourceLoader` 的 `getExtensions()` 返回的是同一对象引用**（`this.extensionsResult`），
   不是拷贝；`getSkills()`/`getPrompts()`/`getThemes()` 同理返回 `{skills, diagnostics}` 引用。
   Rust 侧若要 `Clone` 才能在 API 上等价，需要明确记录「TS 是共享可变，Rust 是拷贝」这一差异
   （`extendResources` 之后调用方拿到的是旧快照 vs 新快照的差异）。
5. **`sdk.ts` 的模块级副作用**：`setDefaultStreamFn(streamSimple)` 在**模块加载时**执行（不是
   `createAgentSession` 内）。Rust 侧对应 `OnceLock` 初始化必须放在「首次使用 `kiana-resources::sdk`」时，
   且**不能**因为 panic/错误而跳过（TS 里没有失败路径）。
6. **`sdk.ts` 的超时映射**：`httpIdleTimeoutMs === 0` → `effectiveTimeoutMs = 2147483647`（INT32_MAX，秒级语义
   由下游解释）；否则原值。`0` 是「禁用超时」的哨兵，**不是**「立即超时」。
7. **`convertToLlmWithBlockImages` 的闭包在每次调用时读 `settingsManager.getBlockImages()`**
   （支持会话中途改设置），不是构造时捕获。Rust 侧必须持有 `Arc<dyn SettingsLike>` 而不是 `bool`。
8. **`buildSystemPromptState` 的 `sections` 字段**：强制 prompt 时**完全省略** `sections`（不是 `{}`）；
   非强制时 `content === ""` 且 `sections` 必有。`getSystemMessageText` 用 `Object.values(sections)`，
   因此 **section 顺序 = 插入顺序**（Rust `IndexMap`）。
9. **`diffSystemPromptSections` 的 `previous` 值类型是 `string | null`**：`null` 表示「上一轮该 section 被删过」，
   `previous[name] !== text` 对 `null` 恒真 → 会重新下发。正文已给代码，此处强调 `undefined` 与 `null`
   在 Rust 里要区分（`Option<Option<String>>` 或专门的 `PatchValue` 枚举）。

### A5. §7 测试清单的遗漏（已核实：这些文件直接 import 本单元模块）

以下测试文件在 `test/` 下、直接 import 本单元（`sdk.ts` / `resource-loader.ts` / `system-prompt.ts` /
`package-manager.ts` / `footer-data-provider.ts`）**或**间接依赖本单元语义，但 §7 **完全没提**。
按「是否必须为 Rust 移植写测试」分级（行数/测试数均已实测）：

| 文件 | 行/测试数 | 触及的本单元面 | 建议 |
|---|---|---|---|
| `test/default-tools-setting.test.ts` | 476 / 20 | `sdk.ts` 的 `tools` / `defaultTools` / `+name`/`-name` / `noTools`、`DefaultResourceLoader` | **P0**：`+name`/`-name` 与 `excludeTools` 的交互只有这里有系统覆盖 |
| `test/git-update.test.ts` | 282 / 5 | `DefaultPackageManager.update()` 的 git 路径（正常更新、force-push 整史重写、pinned ref、临时 git 源刷新） | **P0**：`ensureGitRef`/`cleanAndInstallGitDependencies`/marker 的唯一直接测试 |
| `test/footer-width.test.ts` | 282 / 11 | `ReadonlyFooterDataProvider` 的消费方（`FooterComponent`），含虚拟模型路由与订阅成本 | P2：`ca-tui` 侧，但 `getGitBranch` 的 mock 形状要与本单元一致 |
| `test/suite/regressions/2835-tools-allowlist-filters-extension-tools.test.ts` | 94 / 2 | `sdk.ts` 的 allowlist 是否过滤扩展工具 | P1 |
| `test/suite/regressions/3592-no-builtin-tools-keeps-extension-tools.test.ts` | 119 / 3 | `noTools: "builtin"` 保留扩展工具 | P1 |
| `test/suite/regressions/3616-settings-inmemory-reload.test.ts` | 86 / 3 | `DefaultResourceLoader` + `SettingsManager.inMemory` 的 reload | P1 |
| `test/suite/regressions/6260-inline-extension-naming.test.ts` | 119 / 4 | `<inline:N>` / `<inline:name>` 命名、`hidden` 保留 | P1（合成路径规则） |
| `test/agent-session-dynamic-tools.test.ts` | 256 / 4 | `createAgentSession` + `DefaultResourceLoader` 的动态工具集 | P2（主要属 `ca-session`） |
| `test/block-images.test.ts` | 148 / 9 | 见正文 §7.8 已改写：只间接覆盖 `getBlockImages()` | P2 |

另：`test/suite/agent-session-prompt.test.ts`（723 行 / 21 个）只 `import type { PromptTemplate }`，
不需要移植；`test/utilities.ts`（335 行）与 `test/test-network-env.ts`（6 行）是**测试基础设施**，
后者提供 `allowNetwork()`（`vi.stubEnv("KIANA_OFFLINE", undefined)`），Rust 侧对应「测试可显式放开离线」。

**§7.1 的 `package-manager.test.ts`（132 个）里没有 git-update 的 5 个用例**——它们单独在
`git-update.test.ts`，而该文件用 `git` CLI + `spawnSync` 建真实仓库，是 Rust 侧 `git2` 方案的主要参考。

### A6. 文档与代码的差异（docs 是「语义权威」，但多处只写了一半或与代码有出入，实现以代码为准）

1. `docs/skills.md` 写「Names … can contain at most 64 characters; descriptions can contain at most 1024」——
   与代码一致（`MAX_NAME_LENGTH=64`、`MAX_DESCRIPTION_LENGTH=1024`），但**文档没说**：
   超长/非法 name 只是 **warning**，技能**仍然加载**（只要 description 有效）；只有 description 缺失才跳过。
   正文 §2.6.4 已正确描述代码行为。
2. `docs/skills.md` 写「Kiana neither requires nor warns when the declared name differs from the parent directory」——
   与代码一致（无此类诊断）。**但**代码在 `name` 为空串时会回退到目录名（`frontmatterName || parentDirName`），
   文档未提。
3. `docs/prompt-templates.md` 写「Conventional prompt directories load direct `.md` children only」——
   与代码一致（`loadTemplatesFromDir` 非递归）。文档**未写** `description` 的兜底与截断规则
   （首个非空行，`slice(0,60)`，超长加 `"..."`），而这是用户可见的 `/` 命令补全文本。
4. `docs/prompt-templates.md` 的替换表与代码一致（含 `${@:N:L}`）；文档写「Arguments follow shell-like
   quoting」，但代码**不支持反斜杠转义**（正文 §2.7.2 已强调）——文档措辞比实现宽松，以代码为准。
5. `docs/mcp.md` 说「`env` and `headers` values can … run a command with `!command`」「Relative `cwd`
   values resolve against the session directory」「A leading `~/` in `command`, an argument, or `cwd` names
   the home directory」——这些都**不在** `validateMcpServerConfig` 里（校验只查类型），属于 `extensions/mcp`
   的运行期展开。Rust 侧若把它们写进本单元的校验函数，会与 TS 的错误串/顺序不一致。
6. `docs/mcp.md` 说「`type` must be `stdio`, `http`, or `streamable-http`」——代码对 `type: "streamable-http"`
   **原样返回**（§2.9.7 的 11f），且 `type` 与 `command`/`url` 不匹配时会落到最后一条
   `server "${name}" needs either "command" (stdio) or "url" (streamable HTTP)`。
7. `docs/mcp.md` 说「Server names that differ only in `-` and `_` count as the same server: a second one is
   rejected」——这条**不在** `mcp-servers.ts` 里实现（`McpServerRegistry` 只按精确 name 存 Map）。
   实现在 `extensions/mcp/config.ts`：`servers.keys().find(other => other !== name && mcpNamespace(other) === mcpNamespace(name))`
   命中 → 错误串 **【逐字】** `` `${path}: server "${name}" conflicts with "${clash}"` ``。
   同处还有一条文档未提的规则：**项目 `mcp.json` 里的 `auth` 只允许出现在全局文件**，
   否则 `` `${path}: server "${name}": auth is only allowed in the global mcp.json` ``。
   两条都**不要**写进本单元的 `validateMcpServerConfig`（它只做单条目的类型/取值校验）。
8. `docs/sdk.md` 说「`session.systemPrompt` is read-only」——`AgentSession` 的 getter 是
   `get systemPrompt(): string`（`agent-session.ts:1489`），本单元 `sdk.ts` 不写它；但 `sdk.ts` 传给
   `Agent` 的 `initialState` 是 `{systemPrompt: "", tools: []}`，随后由 `AgentSession` 用
   `buildSystemPromptState` 的结果覆盖。Rust 侧注意「空串 + 空 tools」是**占位**，不是最终值。
9. `docs/packages.md` 与代码一致（已逐条核对 identity、`--local`、trust、`autoload:false` delta、
   filters），无差异需修。

### A7. §6 crate 映射的行为缺口（补充建议）

1. **`fs.globSync`**（`expandPackageGlob`，`package-manager.ts:310`）在 Node 22 是实验 API：默认 `exclude`
   为空，`*` 不匹配 `/`、`**` 跨层、**默认不返回 dotfile**，返回**相对 `cwd` 的 POSIX 字符串**。
   **本函数自己会排序**：
   ```ts
   globSync(pattern, { cwd: root }).map(m => resolve(root, m))
     .filter(p => relative(root, p).split(sep).every(seg => seg === ".." || !seg.startsWith(".")))
     .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
   ```
   排序键是**绝对路径的 JS 字符串比较（UTF-16 码元序）**，不是遍历序；`globSync` 的输出顺序因此被抹平。
   Rust 侧应先在 `glob` crate 结果上做同样的 `resolve` + 隐藏段过滤，再按
   `a.encode_utf16().cmp(b.encode_utf16())` 排序（不能直接 `str::cmp`）。
   `..` 段被**允许**（过滤条件写的是 `seg === ".." || !seg.startsWith(".")`），尽管 `globSync` 通常不产生它。
2. **`decodeURI` vs `decodeURIComponent`**：`git.ts` 的 `hasUnsafeGitInstallPart` 用
   `decodeURIComponent`（失败 → 视为 unsafe → `true`）；`crash-log.ts` 的栈路径用 `decodeURI`（失败保留原行）。
   两者**不能互换**（`decodeURI` 不解码 `;/?:@&=+$,#`）。正文 §6.2 已列，此处强调是**不同函数**。
3. **`proper-lockfile` 的 `lockSync(dir, {realpath:false})`**：在 **`<dir>.lock`** 上创建目录，
   若 `<dir>` 本身不存在也会成功（lockfile 只关心 `.lock` 兄弟路径）。Rust 侧用 `create_dir` 前
   **不要**先 mkdir 目标目录，否则陈旧检测的 mtime 语义会变。
4. **`chalk` 的嵌套**：`chalk.bold(chalk.yellow(x))` 的字节序列是 `\x1b[1m\x1b[33m…\x1b[39m\x1b[22m`
   （内层先关、外层后关），**不是**单个 `\x1b[1;33m`。§3 的所有彩色文案断言都依赖这一点。
5. **`Intl`/`toLocaleString`**：崩溃通知的 `when` 用 `Date.toLocaleString()`（无参，跟随 `LANG`）。
   正文已列为不可移植项；Rust 侧最稳妥是**直接调用 `chrono` 的本地时间 + 手写 `M/D/YYYY, h:mm:ss AM`
   格式并只在 `en-US` 下对齐**，其余 locale 记录为已知差异（这是用户可见文本，应进 §8.6）。
6. **`semver.maxSatisfying`**：npm 的 `maxSatisfying(versions, range)` 在没有 `range` 时不应调用；
   代码是 `range ? maxSatisfying(versions, range) : [...versions].sort(rcompare)[0]`。
   Rust `semver` crate 的 `VersionReq` 不支持 `latest`/tag，需在进入 semver 前先 `validRange` 判定。
7. **`structuredClone` 对 `Map`/`Set`/`Date`** 的支持：本单元只有 `McpServerRegistry.list()` 用它深拷贝
   `config`（纯 JSON 对象），`serde_json` 往返即可；**不要**为了「通用」引入 `dyn Clone` 体系。

### A8. 环境变量补充

正文 §4.6 已覆盖绝大多数变量。补充 4 个**在本单元路径上确实被读到**的：

| 变量 | 读取点 | 语义 |
|---|---|---|
| `PNPM_HOME` | `config.ts` 的 pnpm self-update 命令构造（**属 `ca-cli`**） | 有值 → `--config.global-bin-dir=${PNPM_HOME}`；否则由包目录推断 |
| `SHELL` | `bug-report.ts` 的 `collectEnvironment` | `split(/[\\/]/).pop() || null`（只取 basename；空串 → `null`） |
| `TMUX` / `SSH_CONNECTION` / `SSH_CLIENT` / `SSH_TTY` / `CI` | 同上 | 都是**布尔化**（存在即 `true`，不解析值） |
| `WSL_DISTRO_NAME` / `WSL_INTEROP` | `footer-data-provider.ts` 的 `isWslEnvironment()` | `platform === "linux" &&` 任一存在 |

另注意：`KIANA_OFFLINE` 在 **`vitest.config.ts` 的 `test.env` 里被固定为 `"1"`**——所有测试默认离线，
这是正文 §7 没写但影响全部网络相关测试的前提（见 A5 的 `allowNetwork()`）。

### A9. 逐字文案的补充（正文 §3 未收录但用户可见）

1. `resource-loader.ts` 的 `resolvePromptInput` 失败：
   `` `Warning: Could not read ${description} file ${input}: ${error}` ``（`chalk.yellow`，写 **stderr**
   经 `console.error`），其中 `description` 为 `"system prompt"` 或 `"append system prompt"`。
2. `resource-loader.ts` 的 `omitReplacedExtensions` 警告正文含硬编码的 `` `kiana config` ``（不是 `APP_NAME`），
   正文已提；补充：同一警告的路径列表**按扩展注册顺序**拼接。
3. `footer-data-provider.ts` 的 git 分支回退链：`HEAD` 为 `ref: refs/heads/<x>` 且 `<x>` 正常 → `<x>`；
   `<x> === ".invalid"` → `git symbolic-ref` 的结果，失败则字符串 `"detached"`；`HEAD` 是裸 SHA → `"detached"`；
   读 HEAD 抛错 → `null`；**没有 git（`gitPaths === null`）→ `null`**。
   即 `getGitBranch()` 的类型是 `string | null`，**永不返回 `undefined`**（`cachedBranch` 内部才有 `undefined`
   的「未计算」三态）。
4. `crash-log.ts` 的 `takeUnnotifiedCrash` 会把**所有**未通知记录标记为 `notified: true`（正文已写），
   但**返回值**是其中**最后一条**（`[...records].reverse().find(...)`）——不是第一条。
5. `package-manager-cli.ts` 的 `runManagedNpmCi` 错误串：
   `` `npm ${args.join(" ")} exited with code ${code ?? "unknown"}` ``（`args` 是**完整**的
   `["ci","--ignore-scripts","--omit=dev","--include=optional","--no-fund","--no-audit","--loglevel=error","--progress=false"]`）。
   spawn 自身失败（`ENOENT`）时 `waitForChildProcess` **以原始 `Error` reject**（`error` 事件），
   不会被包成 `exited with code`——两种失败形态不同，Rust 侧不要统一。

### A10. 建议的 Rust 侧「对齐夹具」

审计时确认以下 TS 测试的夹具最容易在 Rust 移植中失真，建议优先建等价夹具：

1. `package-manager.test.ts` 的 `should resolve symlinked user and project resources once`（canonicalize 去重）；
2. `resource-loader.test.ts` 的 worktree 系列（bare layout / submodule / sibling worktree / symlink 主仓库文件），
   依赖 `worktreeRoot.startsWith(mainRepoRoot + sep)` 的**字符串**语义；
3. `2781-skill-collision-precedence`（rank 0–5 + first-wins，直接暴露任何「先排序后添加」）；
4. `git-update.test.ts` 的 force-push 整史重写（`ensureGitRef` + marker + `clean -fdx`）；
5. `default-tools-setting.test.ts` 的 `+name`/`-name` 与 `excludeTools` 组合（`applyToolModifiers` 顺序）。
