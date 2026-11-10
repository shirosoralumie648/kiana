# `ca-cli` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-coding-agent` 中 **ca-cli** 单元（CLI 入口、参数解析、配置路径、迁移、package/config/auth/mcp 子命令、启动编排、`src/utils/**`）的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`ca-cli`。
参考实现（只读，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。
> Rust 实现必须输出完全一致的字节序列（除另有说明）。中文用于解释语义。
> 凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B；`\x07` 表示 U+0007（BEL）；`\x1b\\` 表示 ESC + 反斜杠（ST）。
> 文中 `APP_NAME` / `APP_TITLE` / `CONFIG_DIR_NAME` / `VERSION` / `PACKAGE_NAME` 是运行期常量（见 §2.6），
> 默认值分别为 `kiana` / `π` / `.kiana` / `1.4.0`（取自 `packages/coding-agent/package.json`）/ `@earendil-works/kiana-coding-agent`。
> `ENV_AGENT_DIR` = `KIANA_CODING_AGENT_DIR`，`ENV_SESSION_DIR` = `KIANA_CODING_AGENT_SESSION_DIR`（由 `APP_NAME.toUpperCase()` 派生）。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
packages/coding-agent/src/cli.ts                              (6 行)    node 入口
packages/coding-agent/src/rpc-entry.ts                        (13 行)   RPC 入口
packages/coding-agent/src/cli/setup.ts                        (13 行)   setupCli
packages/coding-agent/src/main.ts                             (1001 行) 启动编排 / 会话选择 / 模式分发
packages/coding-agent/src/cli/args.ts                         (479 行)  parseArgs / printHelp / Args
packages/coding-agent/src/config.ts                           (656 行)  安装方式检测、自更新命令、包路径、agent 路径
packages/coding-agent/src/migrations.ts                       (315 行)  启动迁移
packages/coding-agent/src/index.ts                            (498 行)  SDK 公共导出面（仅“经由本单元导出”，不实现内部逻辑）
packages/coding-agent/src/package-manager-cli.ts               (1151 行) install/remove/uninstall/update/list/config
packages/coding-agent/src/cli/auth-command.ts                 (126 行)  auth 子命令解析与文案
packages/coding-agent/src/cli/auth-check.ts                   (73 行)   auth check 结果
packages/coding-agent/src/cli/credential-print.ts             (90 行)   print-api-key / print-bearer-token
packages/coding-agent/src/cli/list-models.ts                  (115 行)  --list-models 表格
packages/coding-agent/src/cli/initial-message.ts              (43 行)   初始消息拼接
packages/coding-agent/src/cli/file-processor.ts               (88 行)   @file 参数处理
packages/coding-agent/src/cli/project-trust.ts                (62 行)   ProjectTrustContext（启动选择器版）
packages/coding-agent/src/cli/session-picker.ts               (55 行)   --resume 选择器
packages/coding-agent/src/cli/config-selector.ts              (61 行)   kiana config 选择器
packages/coding-agent/src/cli/startup-ui.ts                   (253 行)  启动期 TUI、首次设置
packages/coding-agent/src/bun/cli.ts                          (4 行)    Bun 编译入口
packages/coding-agent/src/bun/sandbox-env-setup.ts            (4 行)    side-effect import
packages/coding-agent/src/bun/runtime-setup.ts                (12 行)   Bun 运行期注册
packages/coding-agent/src/bun/restore-sandbox-env.ts          (36 行)   /proc/self/environ 恢复
packages/coding-agent/src/bun/quickjs-wasm.d.ts               (5 行)    类型声明（无运行期行为）
packages/coding-agent/src/experimental/source-resolver.ts     (86 行)   源码运行期 tsconfig paths 解析
```

`src/utils/**`（全部属于本单元的**规格与验收**，共 37 个文件；`.rs` 实现在 `kiana-core`，见 §8.1 注）：

```
abort.ts(48) ansi.ts(82) changelog.ts(196) child-process.ts(137) clipboard-command.ts(47)
clipboard-image.ts(240) clipboard.ts(150) deprecation.ts(14) exif-orientation.ts(182)
frontmatter.ts(40) fs-watch.ts(30) git.ts(226) highlight-js.d.ts(36) html.ts(51)
image-convert.ts(89) image-process.ts(119) image-resize-core.ts(174) image-resize-worker.ts(43)
image-resize.ts(122) json.ts(6) management-http.ts(78) mime.ts(116) open-browser.ts(24)
output-files.ts(34) paths.ts(140) photon.ts(139) kiana-user-agent.ts(6) shell.ts(264)
sleep.ts(18) syntax-highlight.ts(219) text.ts(9) tool-result-images.ts(67) version-check.ts(109)
windows-self-update.ts(84) wsl.ts(15) zip.ts(72) tools-manager.ts(397)
```

相关文档/脚本（只读参考）：

```
packages/coding-agent/docs/cli.md                 CLI 参考
packages/coding-agent/docs/usage.md               交互入门
packages/coding-agent/docs/environment-variables.md
packages/coding-agent/docs/configuration.md
packages/coding-agent/docs/settings.md              设置项参考（类型/默认值；§2.22 已归纳）
packages/coding-agent/docs/cli-integration.md
packages/coding-agent/package.json                bin: {"kiana": "dist/bundle/cli.js"}，kianaConfig.configDir=".kiana"
kiana-test.sh                                         用 source-resolver 直接跑 src/cli.ts
```

### 0.2 本单元**不**实现（由其它单元拥有，ca-cli 只调用）

`core/agent-session*.ts`、`core/session-manager.ts`、`core/settings-manager.ts`、`core/package-manager.ts`、
`core/resource-loader.ts`、`core/model-runtime.ts`、`core/model-resolver.ts`、`core/auth-storage.ts`、
`core/export-html/**`、`core/extensions/**`、`modes/interactive/**`、`modes/rpc/**`、`modes/print-mode.ts`、
`modes/json-event.ts`、`extensions/mcp/cli*.ts`、`core/keybindings.ts`、`core/trust-manager.ts`、
`core/project-trust.ts`、`core/session-cwd.ts`、`core/auth-guidance.ts`、`core/output-guard.ts`、
`core/timings.ts`、`core/settings-diagnostics.ts`、`core/http-dispatcher.ts`、`core/experimental.ts`。
这些单元的规范尚未写出时必须按源码原样复刻其被 ca-cli 使用的接口（见 §5）。

---

## 1. 概览

### 1.1 职责

ca-cli 是 `kiana` 可执行文件的**最外层**：

1. 进程标记与标题（`process.title`、`KIANA_CODING_AGENT=true`、`AI_AGENT=kiana`）、禁用 `process.emitWarning`、配置 HTTP dispatcher。
2. 在 `main(args)` 中按**固定顺序**识别并执行：`auth` → Windows 隔离区清理 → managed install staging 清理 → `install/remove/uninstall/update/list` → `config` → `mcp` → 普通 CLI 解析。
3. 解析 CLI 参数（`parseArgs`），做诊断（warning/error），输出帮助（`printHelp`）、版本、`--export`。
4. 解析运行模式：`interactive | print | json | rpc`，并在非交互模式把 stdout 重定向到 stderr（`takeOverStdout`）以保护协议输出。
5. 执行启动迁移（auth.json、sessions、tools→bin、commands→prompts、keybindings），收集弃用警告。
6. 解析会话（`--session` / `--session-id` / `--fork` / `--resume` / `--continue` / `--no-session`），必要时提示用户。
7. 解析模型/工具/资源路径，创建运行时（由其它单元的 `createAgentSessionRuntime` 完成重活），报告诊断。
8. 读管道 stdin，构造初始消息，初始化主题，显示弃用警告。
9. 分发到 `runRpcMode` / `InteractiveMode` / `runPrintMode`，并处理退出码与 `KIANA_TIMING`、`KIANA_STARTUP_BENCHMARK`。

另外 ca-cli 拥有全部“平台/工具”实用函数：ANSI 处理、路径规范化、图片嗅探/缩放/EXIF、剪贴板（含 WSL/PowerShell）、
shell 探测、子进程等待、ZIP 写、HTTP 重试、版本检查、工具（fd/rg）下载、语法高亮、changelog 链接规范化、git URL 解析、前端 matter 解析。

### 1.2 用户可见入口

| 入口 | 说明 |
|---|---|
| `kiana`（`dist/bundle/cli.js`，`bin.kiana`） | `setupCli(); main(process.argv.slice(2))` |
| `kiana-rpc`（`./rpc-entry` 导出） | `main(["--mode","rpc",...argv.slice(2)])` |
| SDK `main(args, { extensionFactories })` | 从 `src/index.ts` 导出 |

---

## 2. 逐文件规范

### 2.1 `src/cli.ts`（6 行）

```ts
#!/usr/bin/env node
import { setupCli } from "./cli/setup.ts";
import { main } from "./main.ts";

setupCli();
main(process.argv.slice(2));
```

- 不 `await`，不捕获 rejection（未处理拒绝交由 Node 默认行为）。
- Rust 侧：`fn main()` 中先执行 `setup_cli()` 再 `block_on(main(std::env::args().skip(1).collect()))`；必须保持“错误信息走 stderr、退出码”语义。

### 2.2 `src/rpc-entry.ts`（13 行）

- `process.title = `${APP_NAME}-rpc``（默认 `kiana-rpc`）。
- `process.env.KIANA_CODING_AGENT = "true"`；`process.env.AI_AGENT = "kiana"`。
- `process.emitWarning = (() => {}) as typeof process.emitWarning;`（静默所有警告）。
- `configureHttpDispatcher()`（无参，默认配置）。
- `main(["--mode", "rpc", ...process.argv.slice(2)])`。

### 2.3 `src/cli/setup.ts`（13 行）

```ts
export function setupCli(): void
```

顺序（与 RPC 入口相同但 title 不带后缀）：
1. `process.title = APP_NAME`；
2. `process.env.KIANA_CODING_AGENT = "true"`；
3. `process.env.AI_AGENT = "kiana"`；
4. `process.emitWarning = noop`；
5. `configureHttpDispatcher()`（注释：在 provider SDK 发请求前配置 undici；全局/项目配置稍后由 SettingsManager 应用）。

### 2.4 `src/main.ts`（1001 行）

#### 2.4.1 模块常量

```ts
const EXTENSION_LOAD_FAILURE_HINT = `Hint: Start without extensions using "${APP_NAME} -ne".`;
```

#### 2.4.2 内部函数（逐字签名与行为）

| 函数 | 行为 |
|---|---|
| `async function readPipedStdin(): Promise<string \| undefined>` | `process.stdin.isTTY` 为真 → `undefined`。否则 `setEncoding("utf8")` 累积 `data`，`end` 时 `resolve(data.trim() \|\| undefined)`，`resume()`。 |
| `function reportDiagnostics(diagnostics: readonly AgentSessionRuntimeDiagnostic[]): void` | 每条：`error` → `chalk.red` + 前缀 `"Error: "`；`warning` → `chalk.yellow` + `"Warning: "`；其它 → `chalk.dim` 且无前缀。全部写 `console.error`。 |
| `function isTruthyEnvFlag(value: string \| undefined): boolean` | 空/未定义 → false；`value === "1"` 或小写等于 `"true"`/`"yes"` → true；否则 false。 |
| `function resolveAppMode(parsed: Args, stdinIsTTY: boolean, stdoutIsTTY: boolean): AppMode` | `mode==="rpc"`→`rpc`；`mode==="json"`→`json`；`print \|\| !stdinIsTTY \|\| !stdoutIsTTY`→`print`；否则 `interactive`。 |
| `function toPrintOutputMode(appMode: AppMode): Exclude<Mode,"rpc">` | `json` → `"json"`，否则 `"text"`。 |
| `function isPlainRuntimeMetadataCommand(parsed: Args): boolean` | `!parsed.print && parsed.mode === undefined && (parsed.help === true \|\| parsed.listModels !== undefined)`。用于**不**接管 stdout。 |
| `async function runAuthCommand(args: string[]): Promise<boolean>` | 见 2.4.3。 |
| `async function prepareInitialMessage(parsed, stdinContent?): Promise<{initialMessage?: string; initialImages?: ImageContent[]}>` | 无 `fileArgs` → `buildInitialMessage({parsed, stdinContent})`；否则 `processFileArguments(fileArgs, {autoResizeImages:false})` 后 `buildInitialMessage({parsed, fileText, fileImages, stdinContent})`。注释：图片由 AgentSession 在扩展钩子选出请求模型后再缩放。 |
| `type ResolvedSession = {type:"path";path} \| {type:"local";path} \| {type:"global";path;cwd} \| {type:"not_found";arg}` | 会话参数解析结果。 |
| `function findLocalSessionByExactId(sessionId, cwd, sessionDir?): {type:"local";path} \| undefined` | `SessionManager.findById(cwd, sessionId, sessionDir)`。 |
| `async function resolveSessionPath(sessionArg, cwd, sessionDir?): Promise<ResolvedSession>` | 见 2.4.4。 |
| `async function promptConfirm(message: string): Promise<boolean>` | `readline.createInterface({input: stdin, output: stdout})`，`rl.question(`${message} [y/N] `)`，`answer.toLowerCase()==="y" \|\| "yes"`。 |
| `function validateForkFlags(parsed)` | `--fork` 与非空冲突项 `--session`/`--continue`/`--resume`/`--no-session` 同时出现 → `chalk.red("Error: --fork cannot be combined with " + flags.join(", "))` 到 stderr，`process.exit(1)`。 |
| `function validateSessionIdFlags(parsed)` | `--session-id` 与 `--session`/`--continue`/`--resume` 冲突 → `Error: --session-id cannot be combined with <flags>` exit 1；随后 `assertValidSessionId(parsed.sessionId)`，失败打印 `Error: ${message}` exit 1。 |
| `function openSessionOrExit(path, sessionDir?)` | `SessionManager.open` 失败 → `Error: ${message}` exit 1。 |
| `function forkSessionOrExit(sourcePath, cwd, sessionDir?, sessionId?)` | `SessionManager.forkFrom(src, cwd, sessionDir, {id})` 失败 → 同上。 |
| `export async function createSessionManager(parsed, cwd, sessionDir, settingsManager): Promise<SessionManager>` | 见 2.4.5。 |
| `function buildSessionOptions(parsed, scopedModels, hasExistingSession, modelRuntime, settingsManager): {options; cliThinkingFromModel; diagnostics}` | 见 2.4.6。 |
| `function resolveCliPaths(cwd, paths?)` | 对每项：`isLocalPath(v)` 则 `resolvePath(v, cwd)`，否则原样。 |
| `async function promptForMissingSessionCwd(issue, settingsManager)` | `showStartupSelector(settingsManager, formatMissingSessionCwdPrompt(issue), [{label:"Continue",value:issue.fallbackCwd},{label:"Cancel",value:undefined}])`。 |
| `export interface MainOptions { extensionFactories?: InlineExtension[] }` | 外部注入扩展工厂。 |
| `export async function main(args: string[], options?: MainOptions)` | 见 2.4.7。 |

#### 2.4.3 `runAuthCommand`

1. `isAuthCommandHelp(args)`（`args[0]==="auth"` 且 `args[1]` 为 `undefined`/`"help"` 或 `args` 含 `--help`/`-h`）→ `printAuthCommandHelp()`，返回 `true`。
2. `parseAuthCommand(args)`；抛 `AuthCommandError` → `console.error(chalk.red(`Error: ${message}`))`，`process.exitCode = 1`，返回 `true`；非 `AuthCommandError` 用 `"Failed to parse auth command"`。返回 `undefined` → `false`。
3. 对 `command.args` 再跑 `parseArgs`：
   - `unknownFlags.size > 0` → stderr `chalk.red(`Unknown option --${option} for "${getAuthCommandName(kind)}".`)` 和 `chalk.dim(`Use "${APP_NAME} --help" or "${getAuthCommandUsage(kind)}".`)`，exitCode 1，返回 true。
4. try：
   - `parsed.diagnostics.length > 0` → 抛 `AuthCommandError(messages.join("\n"))`。
   - 非 `check`：`AbortSignal.timeout(15_000)`；`ModelRuntime.create({allowModelNetwork:false, signal})`；`resolveCredentialForPrint(parsed, rt, kind, minExpiryMs, signal)`；`process.stdout.write(`${credential}\n`)`。
   - `check`：`validateAuthCommandArgs(parsed,"check")`；credentials 为 `command.noRefresh ? new ReadOnlyAuthStorage() : AuthStorage.create()`；`createAuthCheckModelRuntime`；`checkProviderAuth(parsed, rt, {refresh: !command.noRefresh})`；若 `command.credentials && status==="ready"` 取 `getProviderCredential(...)`，取不到则把 result 改为 `{status:"not_ready", provider, reason:"credential_not_available"}`；任何异常 → `{status:"invalid", provider: requestedAuth.provider ?? requestedAuth.model!, reason:"invalid_state"}`。
   - 输出：`json ? JSON.stringify({...result, ...(credential?{credentials:credential}:{})}) : (credential ?? result.status)` 加 `"\n"`。
   - `process.exitCode = status==="ready"?0: status==="not_ready"?1:2`。
5. catch：`Error: ${message}`（非 `AuthCommandError` 用 `"Failed to resolve credential"`），`process.exitCode = kind==="check" ? 2 : 1`。
6. 始终返回 `true`。

#### 2.4.4 `resolveSessionPath`

1. `sessionArg` 含 `/` 或 `\`，或以 `.jsonl` 结尾 → `{type:"path", path: resolvePath(sessionArg, cwd)}`。
2. 精确本地 ID 命中 → `{type:"local", path}`。
3. `SessionManager.list(cwd, sessionDir)`，首个 `id.startsWith(sessionArg)` → `{type:"local"}`。
4. `SessionManager.listAll(sessionDir)`：先精确 `id===sessionArg`，再前缀 → `{type:"global", path, cwd}`。
5. 否则 `{type:"not_found", arg}`。

#### 2.4.5 `createSessionManager`

- `parsed.noSession || parsed.help || parsed.listModels !== undefined` → `SessionManager.inMemory(cwd, sessionId!==undefined ? {id} : undefined)`。
- `parsed.fork`：
  - 若同时给 `sessionId` 且本地已存在同 ID → `chalk.red(`Session already exists with id '${sessionId}'`)` + exit 1。
  - `resolveSessionPath`；`path|local|global` → `forkSessionOrExit(resolved.path, cwd, sessionDir, sessionId)`；`not_found` → `chalk.red(`No session found matching '${arg}'`)` + exit 1。
- `parsed.session`：`path|local` → `openSessionOrExit`；`global` → `chalk.yellow(`Session found in different project: ${cwd}`)`，`promptConfirm("Fork this session into current directory?")`；否则 `chalk.dim("Aborted.")` + exit 0；确认则 `forkSessionOrExit`；`not_found` → 同上报错 exit 1。
- `parsed.resume`：`selectSession((onProgress,signal)=>SessionManager.list(cwd,sessionDir,onProgress,signal), (onProgress,signal)=>SessionManager.listAll(sessionDir,onProgress,signal), settingsManager)`；`null` → `chalk.dim("No session selected")` + exit 0；否则 `SessionManager.open(selectedPath, sessionDir)`；`finally { stopThemeWatcher(); }`。
- `parsed.continue` → `SessionManager.continueRecent(cwd, sessionDir)`。
- `parsed.sessionId`：本地存在 → `SessionManager.open`；否则 stderr `chalk.yellow(`Warning: No project session found with id '${sessionId}'; creating a new session with that id.`)`。
- 最终 `SessionManager.create(cwd, sessionDir, {id: parsed.sessionId})`。

#### 2.4.6 `buildSessionOptions`

- `parsed.provider && !parsed.model` → diagnostics error：`--provider requires --model (for example: --provider ${provider} --model <pattern>)`。
- `parsed.model` → `resolveCliModel({cliProvider, cliModel, cliThinking, inlineApiKey: Boolean(parsed.apiKey), modelRuntime})`；`warning`/`error` 加入 diagnostics；`resolved.model` 写入 `options.model`；若未显式 `--thinking` 且 `resolved.thinkingLevel` 存在 → `options.thinkingLevel` 且 `cliThinkingFromModel = true`。
- 若仍无 model、`scopedModels.length>0` 且 `!hasExistingSession`：读 `getDefaultProvider()`/`getDefaultModel()`，`modelRuntime.getModel(...)`，`modelsAreEqual` 在 scope 内 → 用保存的模型（其 `thinkingLevel` 在未显式 `--thinking` 时生效），否则用 `scopedModels[0].model`（同理）。
- `parsed.thinking` 最终覆盖 `options.thinkingLevel`。
- `scopedModels.length>0` → `options.scopedModels = scopedModels.map(sm=>({model: sm.model, thinkingLevel: sm.thinkingLevel}))`（保留 undefined，表示循环时继承当前级别）。
- 工具：`noTools` → `"all"`；否则 `noBuiltinTools` → `"builtin"`；`tools`/`excludeTools` 拷贝数组。
- 返回 `{options, cliThinkingFromModel, diagnostics}`。

#### 2.4.7 `main` 的完整执行顺序（严格）

1. `resetTimings()`；`setThemeJsonValidator(validateThemeJson)`。
2. `extensionFactories = [...builtInExtensions, ...(options?.extensionFactories ?? [])]`。
3. `offlineMode = args.includes("--offline") || isTruthyEnvFlag(process.env.KIANA_OFFLINE)`；为真时设置 `process.env.KIANA_OFFLINE="1"`、`process.env.KIANA_SKIP_VERSION_CHECK="1"`。
4. `if (await runAuthCommand(args)) return;`（auth 命令短路）。
5. `process.platform === "win32"` → `cleanupWindowsSelfUpdateQuarantine(getPackageDir())`。
6. `cleanupManagedInstall()`。
7. `cwd = process.cwd()`；`agentDir = getAgentDir()`；`bootstrapSettingsManager = SettingsManager.create(cwd, agentDir, {projectTrusted:false})`；`applyHttpProxySettings(bootstrapSettingsManager.getGlobalSettings().httpProxy)`；`configureHttpDispatcher()`。
8. `await handlePackageCommand(args, {extensionFactories})` 为真：
   - `exitCode = process.exitCode ?? 0`；
   - win32 且 `exitCode===0` 且 `args[0]==="update"` → 直接 `return`（让 Node 自然 drain，规避 nodejs/node#56645）；
   - 否则 `process.exit(exitCode)`，return。
9. `await handleConfigCommand(args, {extensionFactories})` 为真 → `return`。成功路径在其内部调用 `process.exit(0)`；参数错误或未信任路径只设置 `process.exitCode = 1` 并返回 `true`，由 main 自然返回后以 1 退出。
10. `args[0]==="mcp"` → 懒加载 `loadMcpCommand()`，`process.exitCode = await runMcpCommand(args.slice(1), {cwd, agentDir})`，return。
11. `parsed = parseArgs(args)`；`parsed.diagnostics` 非空：每条 stderr（error 用 `chalk.red("Error: "+msg)`，warning 用 `chalk.yellow("Warning: "+msg)`）；**存在 error 则 `process.exit(1)`**。`time("parseArgs")`。
12. `parsed.version` → `console.log(VERSION)`，`process.exit(0)`。
13. `parsed.export` → `outputPath = parsed.messages[0]`（可空）；`exportFromFile(parsed.export, outputPath)`；异常 `Error: ${message}`（非 Error → `"Failed to export session"`）exit 1；成功 `console.log(`Exported to: ${result}`)`，exit 0。
14. `appMode = resolveAppMode(parsed, process.stdin.isTTY, process.stdout.isTTY)`；`shouldTakeOverStdout = appMode!=="interactive" && !isPlainRuntimeMetadataCommand(parsed)`；为真 `takeOverStdout()`。
15. `parsed.mode==="rpc" && parsed.fileArgs.length>0` → `chalk.red("Error: @file arguments are not supported in RPC mode")` exit 1。
16. `validateForkFlags(parsed)`；`validateSessionIdFlags(parsed)`。
17. `{migratedAuthProviders: migratedProviders, deprecationWarnings} = runMigrations(cwd)`；`time("runMigrations")`。
18. `startupSettingsManager = SettingsManager.create(cwd, agentDir)`；`startupSettingsDiagnostics = collectSettingsDiagnostics(startupSettingsManager)`。
19. `appMode==="interactive" && !parsed.help && parsed.listModels===undefined && shouldRunFirstTimeSetup()` → `await showFirstTimeSetup(startupSettingsManager)`；`time("firstTimeSetup")`。
20. `appMode==="interactive" && parsed.useTheme!==undefined` → `startupSettingsManager.applyOverrides({theme: parsed.useTheme})`。
21. `envSessionDir = process.env[ENV_SESSION_DIR]`；`sessionDir = (parsed.sessionDir ? normalizePath(parsed.sessionDir) : undefined) ?? (envSessionDir ? expandTildePath(envSessionDir) : undefined) ?? startupSettingsManager.getSessionDir()`（**优先级：`--session-dir` > 环境变量 > 设置**）。
22. `sessionManager = await createSessionManager(parsed, cwd, sessionDir, startupSettingsManager)`。
23. `missingSessionCwdIssue = getMissingSessionCwdIssue(sessionManager, cwd)`：interactive → `promptForMissingSessionCwd`；取消 → exit 0；选择 → `SessionManager.open(issue.sessionFile!, sessionDir, selectedCwd)`；非 interactive → `chalk.red(new MissingSessionCwdError(issue).message)` exit 1。
24. `parsed.name !== undefined`：`normalizeSessionName` 为 undefined → `chalk.red("Error: --name requires a non-empty value")` exit 1；否则 `sessionManager.appendSessionInfo(name)`。
25. `time("createSessionManager")`。
26. `trustStore = new ProjectTrustStore(agentDir)`；`sessionCwd = sessionManager.getCwd()`；`autoTrustOnReloadCwd = parsed.projectTrustOverride===undefined && !hasTrustRequiringProjectResources(sessionCwd) ? sessionCwd : undefined`；`trustPromptMode: AppMode = parsed.help || parsed.listModels!==undefined ? "print" : appMode`；`projectTrustByCwd = new Map()`。
27. 解析 4 组资源路径：`resolveCliPaths(cwd, parsed.extensions|skills|promptTemplates|themes)`。
28. 定义 `createRuntime: CreateAgentSessionRuntimeFactory`（见 2.4.8），`time("createRuntime")`；`runtime = await createAgentSessionRuntime(createRuntime, {cwd: sessionManager.getCwd(), agentDir, sessionManager})`；`time("createAgentSessionRuntime")`。
29. 解构 `{services, session, modelFallbackMessage} = runtime`；`{settingsManager, modelRuntime, resourceLoader} = services`；`setCapabilityOverrides(settingsManager.getTerminalCapabilityOverrides())`；`applyHttpProxySettings(settingsManager.getGlobalSettings().httpProxy)`；`configureHttpDispatcher(settingsManager.getHttpIdleTimeoutMs())`。
30. `startupDiagnostics = deduplicateDiagnostics([...startupSettingsDiagnostics, ...runtime.diagnostics])`；`hasRuntimeErrors = runtime.diagnostics.some(d=>d.type==="error")`。
31. `parsed.help` → `reportDiagnostics(startupDiagnostics)`；收集扩展 flags（`resourceLoader.getExtensions().extensions.flatMap(e=>Array.from(e.flags.values()))`）；`printHelp(flags)`；exit 0。
32. `parsed.listModels !== undefined` → `reportDiagnostics(startupDiagnostics)`；`listModels(modelRuntime, typeof parsed.listModels==="string"?parsed.listModels:undefined, AbortSignal.timeout(15_000))`；exit 0。
33. 非 rpc 模式读 stdin：`stdinContent = await readPipedStdin()`；若 interactive 且拿到内容 → `appMode = "print"`；`time("readPipedStdin")`。
34. `{initialMessage, initialImages} = await prepareInitialMessage(parsed, stdinContent)`；`time("prepareInitialMessage")`。
35. `initTheme(settingsManager.getTheme(), appMode==="interactive")`；`time("initTheme")`。
36. interactive 且 `deprecationWarnings.length>0` → `await showDeprecationWarnings(warnings)`。
37. `time("resolveModelScope")`。
38. `appMode!=="interactive" || hasRuntimeErrors` → `reportDiagnostics(startupDiagnostics)`。
39. `hasRuntimeErrors` → **检查 `runtime.diagnostics`（不是合并后的 `startupDiagnostics`）**中是否有 message 含 `"Failed to load extension"`；有则 stderr `chalk.yellow(EXTENSION_LOAD_FAILURE_HINT)`；随后 `process.exit(1)`。`time("createAgentSession")`。
40. `appMode!=="interactive" && !session.model` → `chalk.red(formatNoModelsAvailableMessage())` exit 1。
41. `startupBenchmark = isTruthyEnvFlag(process.env.KIANA_STARTUP_BENCHMARK)`；为真且非 interactive → `chalk.red("Error: KIANA_STARTUP_BENCHMARK only supports interactive mode")` exit 1。
42. `!offlineMode && appMode==="rpc"` → 后台 catalog 刷新：`AbortController` + `setTimeout(abort, 15_000)`；`modelRuntime.refresh({signal}).catch(()=>{}).finally(clearTimeout)`（不 await）。
43. 分发：
    - rpc：`printTimings()`；`await runRpcMode(runtime)`（不返回）。
    - interactive：构造 `new InteractiveMode(runtime, {migratedProviders, startupDiagnostics, modelFallbackMessage, autoTrustOnReloadCwd, initialMessage, initialImages, initialMessages: parsed.messages, verbose: parsed.verbose, tuiMode: parsed.tuiMode, initialThemeSetting: parsed.useTheme})`；
      - 若 benchmark：`await interactiveMode.init()`；`time("interactiveMode.init")`；`await sleep(150)`；`interactiveMode.stop()`；`stopThemeWatcher()`；`printTimings()`；若 `process.stdout.writableLength>0` 等待 `drain`；stderr 同理；return。
      - 否则 `printTimings()`；`await interactiveMode.run()`。
    - print/json：`printTimings()`；`exitCode = await runPrintMode(runtime, {mode: toPrintOutputMode(appMode), messages: parsed.messages, initialMessage, initialImages})`；`stopThemeWatcher()`；`restoreStdout()`；`exitCode!==0` → `process.exitCode = exitCode`；return。

#### 2.4.8 `createRuntime`（`CreateAgentSessionRuntimeFactory`）

入参 `{cwd, agentDir, sessionManager, sessionStartEvent, projectTrustContext}`。步骤：

1. `isInitialRuntime = sessionStartEvent === undefined`。
2. `projectTrustDiagnostics: []`。
3. `cachedProjectTrust = projectTrustByCwd.get(cwd)`；`hasTrustRequiringResources = hasTrustRequiringProjectResources(cwd)`；`shouldResolveProjectTrust = parsed.projectTrustOverride===undefined && cachedProjectTrust===undefined && hasTrustRequiringResources`。
4. `projectTrusted = shouldResolveProjectTrust ? false : (cachedProjectTrust ?? parsed.projectTrustOverride ?? (!hasTrustRequiringResources || trustStore.get(cwd)===true))`。
5. `runtimeSettingsManager = SettingsManager.create(cwd, agentDir, {projectTrusted})`。
6. `services = await createAgentSessionServices({...})`，字段严格如下：
   - `cwd`, `agentDir`, `settingsManager: runtimeSettingsManager`,
   - `modelRuntimeSignal: AbortSignal.timeout(15_000)`,
   - `extensionFlagValues: parsed.unknownFlags`,
   - `resourceLoaderReloadOptions: shouldResolveProjectTrust ? { resolveProjectTrust: async ({extensionsResult}) => {...} } : undefined`；其中调用 `resolveProjectTrusted({cwd, trustStore, trustOverride: parsed.projectTrustOverride, defaultProjectTrust: startupSettingsManager.getDefaultProjectTrust(), extensionsResult, projectTrustContext: projectTrustContext ?? createProjectTrustContext({cwd, mode: isInitialRuntime ? trustPromptMode : appMode, settingsManager: startupSettingsManager, hasUI: isInitialRuntime && trustPromptMode==="interactive"}), onExtensionError: (message)=>projectTrustDiagnostics.push({type:"warning",message})})`；结果写入 `projectTrustByCwd.set(cwd, trusted)`。
   - `resourceLoaderOptions`: `{additionalExtensionPaths, additionalSkillPaths, additionalPromptTemplatePaths, additionalThemePaths, noExtensions: parsed.noExtensions, disabledBuiltinExtensions: parsed.noMcp ? ["mcp"] : undefined, noSkills, noPromptTemplates, noThemes, noContextFiles, systemPrompt: parsed.systemPrompt, appendSystemPrompt, extensionFactories}`。
7. `diagnostics = [...projectTrustDiagnostics, ...services.diagnostics, ...collectSettingsDiagnostics(settingsManager), ...extensions.errors.map(({path,error})=>({type:"error", message:`Failed to load extension "${path}": ${error}`})), ...(warnings??[]).map(({path,warning})=>({type:"warning", message:`Extension package "${path}": ${warning}`}))]`。
8. `modelPatterns = parsed.models ?? settingsManager.getEnabledModels()`；若非空 → `scopedModels = await resolveModelScope(modelPatterns, modelRuntime, {signal: AbortSignal.timeout(15_000)})`，否则 `[]`。
9. `buildSessionOptions(parsed, scopedModels, sessionManager.buildSessionContext().messages.length>0, modelRuntime, settingsManager)`；诊断合并。
10. `parsed.apiKey`：无 `sessionOptions.model` → error `--api-key requires a model to be specified via --model, --provider/--model, or --models`；否则 `await modelRuntime.setRuntimeApiKey(model, parsed.apiKey)`。
11. `created = await createAgentSessionFromServices({services, sessionManager, sessionStartEvent, model, thinkingLevel, scopedModels, tools, excludeTools, noTools, customTools})`。
12. `cliThinkingOverride = parsed.thinking!==undefined || cliThinkingFromModel`；若 `created.session.model && cliThinkingOverride` → `created.session.setThinkingLevel(created.session.thinkingLevel)`。
13. 返回 `{...created, services, diagnostics}`。

### 2.5 `src/cli/args.ts`（479 行）

#### 2.5.1 类型

```ts
export type Mode = "text" | "json" | "rpc";

export interface Args {
  provider?: string; model?: string; apiKey?: string;
  systemPrompt?: string; appendSystemPrompt?: string[];
  thinking?: ThinkingLevel;
  continue?: boolean; resume?: boolean; help?: boolean; version?: boolean;
  mode?: Mode; name?: string; noSession?: boolean;
  session?: string; sessionId?: string; fork?: string; sessionDir?: string;
  models?: string[]; tools?: string[]; excludeTools?: string[];
  noTools?: boolean; noBuiltinTools?: boolean;
  extensions?: string[]; noExtensions?: boolean; noMcp?: boolean;
  print?: boolean; export?: string;
  noSkills?: boolean; skills?: string[]; promptTemplates?: string[]; noPromptTemplates?: boolean;
  themes?: string[]; useTheme?: string; noThemes?: boolean;
  noContextFiles?: boolean; listModels?: string | true;
  offline?: boolean; tuiMode?: TuiMode; verbose?: boolean;
  projectTrustOverride?: boolean;
  messages: string[]; fileArgs: string[];
  unknownFlags: Map<string, boolean | string>;
  diagnostics: Array<{type:"warning"|"error"; message:string}>;
}
```

- `const VALID_THINKING_LEVELS = ["off","minimal","low","medium","high","xhigh","max"] as const;`
- `export function isValidThinkingLevel(level: string): level is ThinkingLevel`
- `export function normalizeSessionName(value: string): string | undefined` — `trim()` 后非空则返回，否则 undefined。

#### 2.5.2 `parseArgs(args: string[]): Args` 精确规则

初始化 `messages:[]`, `fileArgs:[]`, `unknownFlags:new Map()`, `diagnostics:[]`。逐项 `i` 循环：

| 匹配 | 行为 |
|---|---|
| `"--"` | 其后全部参数：以 `@` 开头 → `fileArgs.push(arg.slice(1))`，否则 `messages.push`；`break`。 |
| `--help` / `-h` | `help = true` |
| `--version` / `-v` | `version = true` |
| `--mode` | 缺值或以 `-` 开头 → error `--mode requires text, json, or rpc`（**不消费**）；否则消费；非 `text/json/rpc` → error `Invalid mode "${mode}". Valid values: text, json, rpc`（已消费）。 |
| `--continue` / `-c` | `continue = true` |
| `--resume` / `-r` | `resume = true` |
| `--provider`（仅当 `i+1<len`） | `provider = args[++i]`；缺值**静默忽略** |
| `--model`（同上） | `model` |
| `--api-key`（同上） | `apiKey` |
| `--system-prompt`（同上） | `systemPrompt` |
| `--append-system-prompt`（同上） | 追加数组 |
| `--name` / `-n` | 有值 → 消费；无值 → error `--name requires a value` |
| `--no-session` | true |
| `--session` / `--session-id` / `--fork` / `--session-dir`（`i+1<len`） | 赋值；缺值静默忽略 |
| `--models` | 逗号分割、`trim()`、过滤空串 |
| `--no-tools` / `-nt` | `noTools = true` |
| `--no-builtin-tools` / `-nbt` | `noBuiltinTools = true` |
| `--tools` / `-t`（有值） | 逗号分割/trim/过滤；`getToolListError(tools)` 有错误 → error `${arg}: ${error}`（不赋值），否则赋值。 |
| `--exclude-tools` / `-xt`（有值） | 逗号分割/trim/过滤 |
| `--thinking`（有值） | 合法 → 赋值；否则 warning `Invalid thinking level "${level}". Valid values: ${VALID_THINKING_LEVELS.join(", ")}` |
| `--print` / `-p` | `print = true`；若下一项存在且不以 `@` 开头且（不以 `-` 开头 **或** 以 `---` 开头）→ 作为 message 消费。 |
| `--export`（有值） | 赋值 |
| `--extension` / `-e`（有值） | 追加 |
| `--no-extensions` / `-ne` | true |
| `--no-mcp` | true |
| `--skill`（有值） | 追加 |
| `--prompt-template`（有值） | 追加 |
| `--theme`（有值） | 追加 |
| `--use-theme` | 缺值或以 `-` 开头 → error `--use-theme requires a theme name`（不消费）；否则消费赋值 |
| `--no-skills` / `-ns` / `--no-prompt-templates` / `-np` / `--no-themes` / `--no-context-files` / `-nc` | 对应布尔 |
| `--list-models` | 下一项存在且不以 `-`、`@` 开头 → `listModels = 该值`；否则 `true` |
| `--tui-mode` | `"regular"`/`"fullscreen"` → 赋值并消费；缺值或以 `-` 开头 → error `--tui-mode requires regular or fullscreen`（不消费）；其它 → 消费并 error `Invalid TUI mode "${mode}". Valid values: regular, fullscreen` |
| `--verbose` | true |
| `--approve` / `-a` | `projectTrustOverride = true` |
| `--no-approve` / `-na` | `projectTrustOverride = false` |
| `--offline` | true |
| `@...` | `fileArgs.push(arg.slice(1))` |
| `--xxx`（未知长选项） | 含 `=` → `unknownFlags.set(name, value)`；否则若下一项存在且不以 `-`/`@` 开头 → 消费为字符串值；否则 `true`。 |
| `-x`（未知短选项） | error `Unknown option: ${arg}` |
| 其它非 `-` 开头 | `messages.push(arg)` |

注意（**必须逐条复刻**，测试会观察 `unknownFlags` 与 `diagnostics`）：
- 带值的长选项若**恰好位于末尾**则**不报错、不赋值、不消费**，而是落入后面的分支：
  - 以 `--` 开头 → 未知长选项逻辑，`next===undefined` → `unknownFlags.set(name, true)`；
  - 以单个 `-` 开头 → error `Unknown option: ${arg}`。
  受影响选项（条件为 `i+1<args.length`）：`--provider`、`--model`、`--api-key`、`--system-prompt`、`--append-system-prompt`、`--session`、`--session-id`、`--fork`、`--session-dir`、`--models`、`--tools`/`-t`、`--exclude-tools`/`-xt`、`--thinking`、`--export`、`--extension`/`-e`、`--skill`、`--prompt-template`、`--theme`。
  因此 `kiana --model` 末尾会得到 `unknownFlags={"model": true}`；`kiana -t` 末尾会得到 error `Unknown option: -t`；`kiana --thinking` 末尾同样变成 unknown flag。
- 显式处理缺值的选项不同：`--mode` → error（不消费下一项）、`--name`/`-n` → error、`--use-theme` → error（不消费）、`--tui-mode` → error（不消费）、`--list-models` → `true`。
- `--print` 后面的 `---foo` 会被当作 message（`startsWith("-")` 但 `startsWith("---")`）。
- `--` 之后不再识别任何 flag。

#### 2.5.3 `printHelp(extensionFlags?: ExtensionFlag[]): void`

先构造扩展段（仅 `extensionFlags` 非空）：

```
`\n${chalk.bold("Extension CLI Flags:")}\n` + 每个 flag 一行 `  --${flag.name}${flag.type==="string" ? " <value>" : ""}` 用 `padEnd(30)` 补空格 + description（`flag.description ?? `Registered by ${flag.extensionPath}`），行间 `\n`，末尾 `\n`
```

主文本 **【逐字】**（`${APP_NAME}` 展开为 `kiana`；chalk.bold 包裹标题；行首缩进与空行必须一致）：

```
kiana - AI coding assistant with read, bash, edit, write tools

Usage:
  kiana [options] [--] [@files...] [messages...]

Commands:
  kiana install <source> [-l]     Install extension source and add to settings
  kiana remove <source> [-l]      Remove extension source from settings
  kiana uninstall <source> [-l]   Alias for remove
  kiana update [source|self|kiana]   Update kiana, extensions, or model catalogs
  kiana list                      List installed extensions from settings
  kiana config [-l]               Open TUI to enable/disable package resources (Tab switches scope)
  kiana auth <command>            Print credentials or check provider readiness
  kiana mcp <command>             Manage MCP servers
  kiana <command> --help          Show help for install/remove/uninstall/update/list/config/auth/mcp

Options:
  --provider <name>              Provider to search for --model (requires --model)
  --model <pattern>              Model pattern or ID (supports "provider/id" and optional ":<thinking>")
  --api-key <key>                API key (defaults to env vars)
  --system-prompt <text>         System prompt (default: coding assistant prompt)
  --append-system-prompt <text>  Append text or file contents to the system prompt (can be used multiple times)
  --mode <mode>                  Output mode: text (default), json, or rpc
  --print, -p                    Non-interactive mode: process prompt and exit
  --continue, -c                 Continue previous session
  --resume, -r                   Select a session to resume
  --session <path|id>            Use specific session file or partial UUID
  --session-id <id>              Use exact project session ID, creating it if missing
  --fork <path|id>               Fork specific session file or partial UUID into a new session
  --session-dir <dir>            Directory for session storage and lookup
  --no-session                   Don't save session (ephemeral)
  --name, -n <name>              Set session display name
  --models <patterns>            Comma-separated model patterns for Ctrl+P cycling
                                 Supports globs (anthropic/*, *sonnet*) and fuzzy matching
  --no-tools, -nt                Disable all tools by default (built-in and extension)
  --no-builtin-tools, -nbt       Disable built-in tools by default but keep extension/custom tools enabled
  --tools, -t <tools>            Comma-separated allowlist of tool names or patterns (*) to enable
                                 Keeps MCP tools unless an entry starts with mcp__
                                 Only +name/-name entries add to or remove from the defaults
  --exclude-tools, -xt <tools>   Comma-separated denylist of tool names or patterns (*) to disable
                                 Applies to all tools, MCP tools included
  --thinking <level>             Set thinking level: off, minimal, low, medium, high, xhigh, max
  --extension, -e <path>         Load an extension file or builtin:<name> (can be used multiple times)
  --no-extensions, -ne           Disable extension discovery and built-in extensions (explicit -e paths still work)
  --no-mcp                       Disable built-in MCP support: no servers connect and no MCP tools
  --skill <path>                 Load a skill file or directory (can be used multiple times)
  --no-skills, -ns               Disable skills discovery and loading
  --prompt-template <path>       Load a prompt template file or directory (can be used multiple times)
  --no-prompt-templates, -np     Disable prompt template discovery and loading
  --theme <path>                 Load a theme file or directory (can be used multiple times)
  --use-theme <name[/name]>      Set the initial interactive theme for this run
  --no-themes                    Disable theme discovery and loading
  --no-context-files, -nc        Disable AGENTS.md and CLAUDE.md discovery and loading
  --export <file> [output]       Export session file to HTML and exit
  --list-models [search]         List available models (with optional fuzzy search)
  --verbose                      Force verbose startup (overrides quietStartup setting)
  --tui-mode <mode>              TUI mode: fullscreen (default) or regular
  --approve, -a                  Trust project-local files for this run
  --no-approve, -na              Ignore project-local files for this run
  --offline                      Disable startup network operations (same as KIANA_OFFLINE=1)
  --                             End option parsing; treat remaining arguments as messages/files
  --help, -h                     Show this help
  --version, -v                  Show version number

Extensions can register additional flags (e.g., --plan from plan-mode extension).<扩展段>

Examples:
  # Print a provider API key for an external client
  kiana auth print-api-key --provider openai

  # Print an OAuth bearer token for an external client (refreshes if expired)
  kiana auth print-bearer-token --provider openai-codex

  # Interactive mode
  kiana

  # Interactive mode with initial prompt
  kiana "List all .ts files in src/"

  # Include files in initial message
  kiana @prompt.md @image.png "What color is the sky?"

  # Non-interactive mode (process and exit)
  kiana -p "List all .ts files in src/"

  # Prompt beginning with a dash
  kiana -p -- "- Summarize these points"

  # Multiple messages (interactive)
  kiana "Read package.json" "What dependencies do we have?"

  # Continue previous session
  kiana --continue "What did we discuss?"

  # Start a named session
  kiana --name "Refactor auth module"

  # Use different model
  kiana --provider openai --model gpt-4o-mini "Help me refactor this code"

  # Use model with provider prefix (no --provider needed)
  kiana --model openai/gpt-4o "Help me refactor this code"

  # Use model with thinking level shorthand
  kiana --model sonnet:high "Solve this complex problem"

  # Limit model cycling to specific models
  kiana --models claude-sonnet,claude-haiku,gpt-4o

  # Limit to a specific provider with glob pattern
  kiana --models "github-copilot/*"

  # Cycle models with fixed thinking levels
  kiana --models sonnet:high,haiku:low

  # Start with a specific thinking level
  kiana --thinking high "Solve this complex problem"

  # Read-only mode (no file modifications possible)
  kiana --tools read,grep,find,ls -p "Review the code in src/"

  # Add codemode to the default tools
  kiana --tools +codemode

  # Codemode with only the tools of one MCP server
  kiana --tools read,bash,codemode,'mcp__radius__*'

  # Disable one tool while keeping the rest available
  kiana --exclude-tools ask_question

  # Export a session file to HTML
  kiana --export ~/.kiana/agent/sessions/--path--/session.jsonl
  kiana --export session.jsonl output.html

Environment Variables:
  ANTHROPIC_AUTH_TOKEN             - Anthropic bearer auth token
  ANTHROPIC_API_KEY                - Anthropic Claude API key
  ANTHROPIC_OAUTH_TOKEN            - Anthropic OAuth token (alternative to API key)
  ANT_LING_API_KEY                 - Ant Ling API key
  OPENAI_API_KEY                   - OpenAI GPT API key
  AZURE_OPENAI_API_KEY             - Azure OpenAI API key
  AZURE_OPENAI_BASE_URL            - Azure OpenAI/Cognitive Services base URL (e.g. https://{resource}.openai.azure.com)
  AZURE_OPENAI_RESOURCE_NAME       - Azure OpenAI resource name (alternative to base URL)
  AZURE_OPENAI_API_VERSION         - Azure OpenAI API version (default: v1)
  AZURE_OPENAI_DEPLOYMENT_NAME_MAP - Azure OpenAI model=deployment map (comma-separated)
  DEEPSEEK_API_KEY                 - DeepSeek API key
  NVIDIA_API_KEY                   - NVIDIA NIM API key
  GEMINI_API_KEY                   - Google Gemini API key
  GROQ_API_KEY                     - Groq API key
  CEREBRAS_API_KEY                 - Cerebras API key
  XAI_API_KEY                      - xAI Grok API key
  FIREWORKS_API_KEY                - Fireworks API key
  TOGETHER_API_KEY                 - Together AI API key
  BASETEN_API_KEY                  - Baseten API key
  OPENROUTER_API_KEY               - OpenRouter API key
  AI_GATEWAY_API_KEY               - Vercel AI Gateway API key
  ZAI_API_KEY                      - ZAI Coding Plan API key (Global)
  ZAI_CODING_CN_API_KEY            - ZAI Coding Plan API key (China)
  MISTRAL_API_KEY                  - Mistral API key
  MINIMAX_API_KEY                  - MiniMax API key
  MOONSHOT_API_KEY                 - Moonshot AI API key
  OPENCODE_API_KEY                 - OpenCode Zen/OpenCode Go API key
  KIMI_API_KEY                     - Kimi For Coding API key
  META_API_KEY                     - Meta Model API key
  CLOUDFLARE_API_KEY               - Cloudflare API token (Workers AI and AI Gateway)
  CLOUDFLARE_ACCOUNT_ID            - Cloudflare account id (required for both)
  CLOUDFLARE_GATEWAY_ID            - Cloudflare AI Gateway slug (required for AI Gateway)
  QWEN_TOKEN_PLAN_API_KEY          - Qwen Token Plan API key (international region)
  QWEN_TOKEN_PLAN_CN_API_KEY       - Qwen Token Plan API key (China region)
  XIAOMI_API_KEY                   - Xiaomi MiMo API key (api.xiaomimimo.com billing)
  XIAOMI_TOKEN_PLAN_CN_API_KEY     - Xiaomi MiMo Token Plan API key (China region)
  XIAOMI_TOKEN_PLAN_AMS_API_KEY    - Xiaomi MiMo Token Plan API key (Amsterdam region)
  XIAOMI_TOKEN_PLAN_SGP_API_KEY    - Xiaomi MiMo Token Plan API key (Singapore region)
  AWS_PROFILE                      - AWS profile for Amazon Bedrock
  AWS_ACCESS_KEY_ID                - AWS access key for Amazon Bedrock
  AWS_SECRET_ACCESS_KEY            - AWS secret key for Amazon Bedrock
  AWS_BEARER_TOKEN_BEDROCK         - Bedrock API key (bearer token)
  AWS_REGION                       - AWS region for Amazon Bedrock (e.g., us-east-1)
  KIANA_CODING_AGENT_DIR              - Config directory (default: ~/.kiana/agent)
  KIANA_CODING_AGENT_SESSION_DIR      - Session storage directory (overridden by --session-dir)
  KIANA_PACKAGE_DIR                   - Override package directory (for Nix/Guix store paths)
  KIANA_OFFLINE                       - Disable startup network operations when set to 1/true/yes
  KIANA_TELEMETRY                     - Override install telemetry when set to 1/true/yes or 0/false/no
  KIANA_SHARE_VIEWER_URL              - Base URL for /share command (default: https://kiana.dev/session/)

Built-in Tool Names:
  read       - Read file contents
  bash       - Execute bash commands
  powershell - Execute PowerShell commands on Windows
  edit       - Edit files with find/replace
  write      - Write files (creates/overwrites)
  grep       - Search file contents (read-only, off by default)
  find       - Find files by glob pattern (read-only, off by default)
  ls         - List directory contents (read-only, off by default)
```

实现细节：环境变量两行使用 `${ENV_AGENT_DIR.padEnd(32)}` / `${ENV_SESSION_DIR.padEnd(32)}` 后再接 ` - ...`；
其余每行是硬编码字符串。整体是**单个** `console.log`（结尾换行由 console.log 追加，屏幕最后一行为空行）。
`chalk.bold` 仅包裹 `APP_NAME`、`"Usage:"`、`"Commands:"`、`"Options:"`、`"Examples:"`、`"Environment Variables:"`、`"Built-in Tool Names:"`、`"Extension CLI Flags:"`。

### 2.6 `src/config.ts`（656 行）

#### 2.6.1 运行期检测常量

| 名称 | 定义 |
|---|---|
| `isBunBinary` | `import.meta.url` 含 `"$bunfs"`、`"~BUN"`、`"%7EBUN"` 之一。Rust 侧恒为 false（原生二进制），但 `KIANA_PACKAGE_DIR` 语义需保留。 |
| `isBunRuntime` | `!!process.versions.bun`。Rust：false。 |
| `isBundledNode` | 编译期 `KIANA_BUNDLED_NODE`；Rust：false。 |
| `__filename`/`__dirname` | 本模块文件路径与其目录。Rust 用可执行文件路径或 `KIANA_PACKAGE_DIR`。 |

#### 2.6.2 安装方式与自更新

```ts
export type InstallMethod = "bun-binary" | "npm" | "pnpm" | "yarn" | "bun" | "unknown";
interface SelfUpdateCommandStep { command: string; args: string[]; display: string }
export interface SelfUpdateCommand extends SelfUpdateCommandStep { steps?: SelfUpdateCommandStep[] }
export type SelfUpdatePackageTarget = string | { packageName: string; installSpec?: string };
```

- `normalizeSelfUpdatePackageTarget(target)`：字符串 → `{packageName: t, installSpec: t}`；对象 → `installSpec ?? packageName`。
- `makeSelfUpdateCommand(installStep, uninstallStep?)`：无 uninstall 直接返回 install；否则 `{...installStep, display: `${uninstall.display} && ${install.display}`, steps:[uninstall, install]}`。
- `makeSelfUpdateCommandStep(command, args)`：`display = [command, ...args].map(a => /\s/.test(a) ? `"${a}"` : a).join(" ")`（含空白才加双引号）。
- `export function detectInstallMethod(): InstallMethod`：
  1. `isBunBinary` → `bun-binary`。
  2. `resolvedPath = `${__dirname}\0${process.execPath || ""}`.toLowerCase().replace(/\\/g,"/")`。
  3. 含 `/pnpm/` 或 `/.pnpm/` → `pnpm`；含 `/yarn/` 或 `/.yarn/` → `yarn`；`isBunRuntime` 或含 `/install/global/node_modules/` → `bun`；含 `/npm/` 或 `/node_modules/` → `npm`；否则 `unknown`。
- `getInferredNpmInstall()`：基于 `getPackageDir()`：`parent = dirname(packageDir)`；若 `basename(parent)` 以 `@` 开头且 `basename(dirname(parent))==="node_modules"` → root=`dirname(parent)`；否则 `basename(parent)==="node_modules"` → root=parent；否则 undefined。若 `basename(dirname(root))==="lib"` → `{root, prefix: dirname(dirname(root))}`，否则 undefined（Windows 自定义前缀不推断）。
- `getSelfUpdateCommandForMethod(method, installedPackageName, updatePackageTarget=installedPackageName, npmCommand?)`：
  - `bun-binary`/`unknown` → undefined。
  - `pnpm`：`const match = readCommandOutput("pnpm",["root","-g"]) ? undefined : /^(.*[\\/]global[\\/][^\\/]+)[\\/]\.pnpm[\\/]/.exec(getPackageDir())`；`binDirArgs = match ? [`--config.global-bin-dir=${process.env.PNPM_HOME || dirname(dirname(match[1]))}`] : []`；install = `makeSelfUpdateCommandStep("pnpm", ["install","-g","--ignore-scripts","--config.minimumReleaseAge=0", ...binDirArgs, target.installSpec])`；卸载步骤仅在 `target.packageName !== installedPackageName` 时：`["remove","-g",...binDirArgs, installedPackageName]`。
  - `yarn`：install `["global","add","--ignore-scripts", installSpec]`；uninstall（条件同上）`["global","remove", installedPackageName]`。
  - `bun`：install `["install","-g","--ignore-scripts","--minimum-release-age=0", installSpec]`；uninstall `["uninstall","-g", installedPackageName]`。
  - `npm`：`const [command="npm", ...npmArgs] = npmCommand ?? []`；`inferred = npmCommand?.length ? undefined : getInferredNpmInstall()`；`prefixArgs=[...npmArgs, ...(inferred?["--prefix", inferred.prefix]:[])]`；install `[...prefixArgs,"install","-g","--ignore-scripts","--min-release-age=0", installSpec]`；uninstall `[...prefixArgs,"uninstall","-g", installedPackageName]`（条件同上）。
- `readCommandOutput(command, args, {requireSuccess?})`：`spawnProcessSync(..., {encoding:"utf-8", stdio:["ignore","pipe","pipe"]})`；`status===0` → `stdout.trim() || undefined`；失败且 `requireSuccess` → `throw new Error(`Failed to run ${[command,...args].join(" ")}: ${reason}`)`，`reason = error?.message || stderr.trim() || `exit code ${status ?? "unknown"}``；否则 undefined。
- `getGlobalPackageRoots(method, _packageName, npmCommand?)`：
  - npm：configured = `!!npmCommand?.length`；`[command="npm", ...npmArgs]`；若 configured 且 `command==="bun"`：`bun pm bin -g`（requireSuccess）→ roots=`[~/.bun/install/global/node_modules]` 加 `dirname(bunBin)/install/global/node_modules`；否则 `command ... "root","-g"`（requireSuccess=configured）+ 未配置时的 inferred.root。
  - pnpm：`pnpm root -g` → `[root, dirname(root)]`；否则正则抓 `(.*[\\/]global[\\/][^\\/]+)[\\/]\.pnpm[\\/]` → `[match[1]]`；否则 `[]`。
  - yarn：`yarn global dir` → `[dir, join(dir,"node_modules")]`；否则 `[]`。
  - bun：`bun pm bin -g` + `~/.bun/install/global/node_modules` 等。
  - bun-binary/unknown → `[]`。
- `normalizeExistingPathForComparison(path, resolveSymlinks)`：`resolve(path)`；不存在 → undefined；symlink 解析失败 → undefined；win32 转小写。
- `getPathComparisonCandidates(path)`：`[realpath=false 版本, realpath=true 版本]` 去重。
- `getEntrypointPackageDir()`：从 `process.argv[1]` 的目录逐级向上找含 `package.json` 的目录；到根仍无 → undefined。
- `isSelfUpdatePathWritable()`：`accessSync(packageDir, W_OK)` 且 `accessSync(dirname(packageDir), W_OK)` 都成功。
- `isManagedByGlobalPackageManager(method, packageName, npmCommand?)`：包目录候选（`getPackageDir()`、`getEntrypointPackageDir()`）与全局根候选逐对比较：根前缀（保证以 `sep` 结尾）是包目录的前缀。
- `export function getSelfUpdateCommand(packageName, npmCommand?, updatePackageTarget=packageName): SelfUpdateCommand | undefined`：`command && isManagedByGlobalPackageManager && isSelfUpdatePathWritable` 才返回。
- `export function getSelfUpdateUnavailableInstruction(packageName, npmCommand?, updatePackageTarget=packageName): string`：
  - `bun-binary` → **【逐字】** `Download from: https://github.com/earendil-works/kiana/releases/latest`
  - 有 command 且被全局包管理器管理但不可写 → `This installation is managed by a global ${method} install, but the install path is not writable. Update it yourself with: ${command.display}`
  - 有 command 但未被管理 → `This installation is not managed by a global ${method} install. Update it with the package manager, wrapper, or source checkout that provides it.`
  - 无 command → `Update ${target.installSpec} using the package manager, wrapper, or source checkout that provides this installation.`
- `export function getUpdateInstruction(packageName): string`：有 command → `Run: ${command.display}`；否则 unavailable instruction。

#### 2.6.3 包资源路径

| 函数 | 行为 |
|---|---|
| `export function findNodePackageDir(startDir)` | 逐级向上找 `package.json`；若目录名为 `dist` 且父目录也有 `package.json` → 返回父目录；找不到返回 `startDir`。 |
| `export function getPackageDir()` | `KIANA_PACKAGE_DIR` 非空 → `normalizePath(env)`；`isBunBinary` → `dirname(process.execPath)`；否则 `findNodePackageDir(__dirname)`。 |
| `getThemesDir()` | bun-binary → `join(pkgDir,"theme")`；否则 `join(pkgDir, exists(join(pkgDir,"src"))?"src":"dist", "modes","interactive","theme")`。 |
| `getExportTemplateDir()` | bun-binary → `join(pkgDir,"export-html")`（**没有** `core/` 段）；否则 `join(pkgDir, srcOrDist, "core","export-html")`。注意与 `getThemesDir` 的差异：bun-binary 分支的目录名不同（`export-html` vs `theme`）。 |
| `getPackageJsonPath()` | `join(pkgDir,"package.json")`。 |
| `getReadmePath()` / `getDocsPath()` / `getExamplesPath()` / `getChangelogPath()` | `resolve(join(pkgDir, "README.md" / "docs" / "examples" / "CHANGELOG.md"))`。 |
| `getInteractiveAssetsDir()` | bun-binary → `join(pkgDir,"assets")`；否则 `join(pkgDir, srcOrDist, "modes","interactive","assets")`。 |
| `getBundledInteractiveAssetPath(name)` | `join(getInteractiveAssetsDir(), name)`。 |
| `setEmbeddedQuickJSWasmPath(path)` / `getQuickJSWasmPath()` | 后者惰性 `createRequire(import.meta.url).resolve("quickjs-wasi/quickjs.wasm")`，缓存。Rust：返回随包资源路径或 `KIANA_QUICKJS_WASM`；必须“解析一次后缓存”，即使升级删除了旧文件。 |
| `resolveCodemodeWorkerSpecifier(runtime, moduleUrl)` | `bun-binary` → `"./src/extensions/codemode/worker.ts"`；`bundled-node` → `new URL("./codemode-worker.js", moduleUrl)`；`unbundled` → undefined。 |
| `getCodemodeWorkerSpecifier()` | runtime 选择同 `isBunBinary/isBundledNode`；bundled-node 时把 worker 文件读为 `data:text/javascript;base64,...` URL 并缓存。 |
| `export type InstallChange = {kind:"updated";version:string} \| {kind:"removed"}` / `detectInstallChange(packageJsonPath = startupPackageJsonPath)` | bun-binary 或无路径 → undefined；读 package.json（`stripBom`）失败且 errno `ENOENT` → `{kind:"removed"}`，其它错误 → undefined；`installed.version && installed.version !== VERSION` → `{kind:"updated",version}`。 |

#### 2.6.4 `kianaConfig` 与常量

模块加载时读一次 `getPackageJsonPath()`（ENOENT 静默；其它错误抛出），保存 `pkg` 与 `startupPackageJsonPath`。

```ts
const kianaConfigName = pkg.kianaConfig?.name;
export const PACKAGE_NAME = pkg.name || "@earendil-works/kiana-coding-agent";
export const APP_NAME = kianaConfigName || "kiana";
export const APP_TITLE = kianaConfigName ? APP_NAME : "π";
export const CONFIG_DIR_NAME = pkg.kianaConfig?.configDir || ".kiana";
export const VERSION = pkg.version || "0.0.0";
export const ENV_AGENT_DIR = `${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`;
export const ENV_SESSION_DIR = `${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR`;
export function expandTildePath(path: string): string { return normalizePath(path); }
export function getShareViewerUrl(gistId: string): string
```

- `getShareViewerUrl`：`base = process.env.KIANA_SHARE_VIEWER_URL || "https://kiana.dev/session/"`；返回 `${base}#${gistId}`。

#### 2.6.5 agent 目录与子路径

| 函数 | 返回 |
|---|---|
| `getAgentDir()` | `process.env[ENV_AGENT_DIR]` 存在 → `expandTildePath(env)`；否则 `join(homedir(), CONFIG_DIR_NAME, "agent")`。 |
| `getCustomThemesDir()` | `join(agentDir,"themes")` |
| `getModelsPath()` | `join(agentDir,"models.json")` |
| `getAuthPath()` | `join(agentDir,"auth.json")` |
| `getSettingsPath()` | `join(agentDir,"settings.json")` |
| `getToolsDir()` | `join(agentDir,"tools")` |
| `getBinDir()` | `join(agentDir,"bin")` |
| `getPromptsDir()` | `join(agentDir,"prompts")` |
| `getSessionsDir()` | `join(agentDir,"sessions")` |
| `getDebugLogPath()` | `join(agentDir, `${APP_NAME}-debug.log`)`（默认 `kiana-debug.log`） |

### 2.7 `src/migrations.ts`（315 行）

常量：

```ts
const MIGRATION_GUIDE_URL = "https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration";
const EXTENSIONS_DOC_URL = "https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/docs/extensions.md";
```

| 函数 | 行为 |
|---|---|
| `export function migrateAuthToAuthJson(): string[]` | 若 `auth.json` 已存在 → `[]`。若 `oauth.json` 存在：`JSON.parse(stripBom(readFileSync(utf-8)))`，每个 `[provider, cred]` → `migrated[provider] = {type:"oauth", ...cred}`，记录 provider；成功 rename 为 `${oauthPath}.migrated`；异常忽略。若 `settings.json` 存在：读并解析；`settings.apiKeys` 为对象时，逐项仅在 `!migrated[provider] && typeof key==="string"` 时 `{type:"api_key", key}`；随后 `delete settings.apiKeys` 并 `writeFileSync(settingsPath, JSON.stringify(settings, null, 2))`；异常忽略。若 `migrated` 非空：`mkdirSync(dirname(authPath),{recursive:true})`，`writeFileSync(authPath, JSON.stringify(migrated,null,2), {mode:0o600})`。返回 providers（顺序：oauth 先，settings 后）。 |
| `export function migrateSessionsFromAgentRoot(): void` | 读 `agentDir` 顶层 `*.jsonl`（非递归）；读取每文件首行（按 `"\n"` 分割的第一段），`JSON.parse`；要求 `header.type === "session"` 且 `header.cwd` 存在；`safePath = `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--``；目标目录 `agentDir/sessions/safePath`（不存在则 mkdir recursive）；文件名取 `/` 或 `\` 分割后的最后一段；目标已存在则跳过；否则 rename。全部异常忽略。 |
| `function migrateCommandsToPrompts(baseDir, label): boolean` | `commands` 存在且 `prompts` 不存在 → rename；成功 `console.log(chalk.green(`Migrated ${label} commands/ → prompts/`))` 返回 true；失败 `console.log(chalk.yellow(`Warning: Could not migrate ${label} commands/ to prompts/: ${err.message ?? err}`))`。 |
| `function migrateKeybindingsConfigFile(): void` | 读 `agentDir/keybindings.json`（若无直接返回）；`stripBom` 后 JSON.parse；非普通对象（null/数组/非 object）返回；`migrateKeybindingsConfig(parsed)` → `{config, migrated}`；`migrated` 为真才 `writeFileSync(configPath, `${JSON.stringify(config,null,2)}\n`, "utf-8")`；异常忽略。 |
| `function migrateToolsToBin(): void` | `tools/` 不存在直接返回；对 `["fd","rg","fd.exe","rg.exe"]`：旧文件存在时确保 `bin/` 存在；新路径不存在 → rename 并标记 moved；新路径已存在 → `rmSync?.(oldPath,{force:true})`。moved 为真 → `console.log(chalk.green("Migrated managed binaries tools/ → bin/"))`。 |
| `function checkDeprecatedExtensionDirs(baseDir, label): string[]` | `hooks/` 存在 → `${label} hooks/ directory found. Hooks have been renamed to extensions.`；`tools/` 存在且其中条目不是 `fd`/`rg`/`fd.exe`/`rg.exe`（大小写不敏感）且不以 `.` 开头 → `${label} tools/ directory contains custom tools. Custom tools have been merged into extensions.` |
| `function migrateExtensionSystem(cwd): string[]` | 对 `agentDir`（label `"Global"`）与 `join(cwd, CONFIG_DIR_NAME)`（label `"Project"`）依次 `migrateCommandsToPrompts`；再收集两处弃用目录警告（Global 在前，Project 在后）。 |
| `export async function showDeprecationWarnings(warnings: string[]): Promise<void>` | 空则返回。每条 `console.log(chalk.yellow(`Warning: ${warning}`))`；然后依次 `console.log(chalk.yellow(`\nMove your extensions to the extensions/ directory.`))`、`console.log(chalk.yellow(`Migration guide: ${MIGRATION_GUIDE_URL}`))`、`console.log(chalk.yellow(`Documentation: ${EXTENSIONS_DOC_URL}`))`、`console.log(chalk.dim(`\nPress any key to continue...`))`；`stdin.setRawMode?.(true)`、`resume()`、`once("data", ...)` → `setRawMode?.(false)`、`pause()`、resolve；最后 `console.log()`（空行）。 |
| `export function runMigrations(cwd): {migratedAuthProviders: string[]; deprecationWarnings: string[]}` | 顺序：`migrateAuthToAuthJson()` → `migrateSessionsFromAgentRoot()` → `migrateToolsToBin()` → `migrateKeybindingsConfigFile()` → `deprecationWarnings = migrateExtensionSystem(cwd)`。 |

### 2.8 `src/index.ts`（498 行）

仅**重导出**（不新增逻辑）。Rust 侧对应 `lib.rs` 的 `pub use`：

- `cli/args`: `Args`, `parseArgs`。
- `config`: `CONFIG_DIR_NAME`, `getAgentDir`, `getDocsPath`, `getExamplesPath`, `getPackageDir`, `getReadmePath`, `VERSION`。
- `core/agent-session`: `AgentSession`, 类型 `AgentSessionConfig`/`AgentSessionEvent`/`AgentSessionEventListener`/`ModelCycleResult`/`ParsedSkillBlock`/`PromptOptions`/`SessionStats`，`parseSkillBlock`。
- `core/auth-storage`: `readStoredCredential`。
- `core/cache-warmer`: 类型。
- `core/compaction/index`: 大量函数/类型（`calculateContextTokens`, `collectEntriesForBranchSummary`, `compact`, `DEFAULT_COMPACTION_SETTINGS`, `estimateTokens`, `findCutPoint`, `findTurnStartIndex`, `generateBranchSummary`, `generateSummary`, `generateSummaryWithUsage`, `getLastAssistantUsage`, `prepareBranchEntries`, `serializeConversation`, `shouldCompact` 等）。
- `core/event-bus`: `createEventBus`, 类型。
- `core/extensions/index`: 全部扩展事件类型与 `createExtensionRuntime`, `defineTool`, `discoverAndLoadExtensions`, `ExtensionRunner`, `isBashToolResult` 等谓词, `wrapRegisteredTool(s)`。
- `core/footer-data-provider`, `core/mcp-servers`（类型）。
- `core/messages`: `convertToLlm`。
- `core/model-registry`: `ModelRegistry`。
- `core/model-resolver`: `ModelScopeDiagnostic`, `ResolveCliModelResult`, `ResolveModelScopeResult`, `resolveCliModel`, `resolveModelScopeWithDiagnostics`, `ScopedModel`。
- `core/model-runtime`: `CreateModelRuntimeOptions`, `CredentialSynchronizationError`, `CredentialSynchronizationOperation`, `ModelRuntime`, `ModelRuntimeAuthOverrides`。
- `core/package-manager`: 类型 + `DefaultPackageManager`。
- `core/resource-loader`: 类型 + `DefaultResourceLoader`, `loadProjectContextFiles`。
- `core/sdk`: `AgentSessionRuntime` 及 `createAgentSession`, `createAgentSessionFromServices`, `createAgentSessionRuntime`, `createAgentSessionServices`, 工具工厂 `createBashTool`, `createCodingTools`, `createEditTool`, `createFindTool`, `createGrepTool`, `createLsTool`, `createPowerShellTool`, `createReadOnlyTools`, `createReadTool`, `createWriteTool`, `PromptTemplate`。
- `core/session-manager`: `buildContextEntries`, `buildSessionContext`, `buildSessionProjection`, `CURRENT_SESSION_VERSION`, `getLatestCompactionEntry`, `migrateSessionEntries`, `parseSessionEntries`, `SessionManager`, `sessionEntryToContextMessages` 及类型。
- `core/settings-manager`: 类型 + `SettingsManager`。
- `core/skills`: `formatSkillsForPrompt`, `loadSkills`, `loadSkillsFromDir` 及类型。
- `core/source-info`: `createSyntheticSourceInfo`。
- `core/tools/edit-diff`: `generateDiffString`, `generateUnifiedPatch`。
- `core/tools/index`: 全部工具类型与工厂、`formatSize`、`truncateHead/Line/Tail`、`withFileMutationQueue` 等。
- `core/tools/render-utils`: `formatReadLineRange`。
- `core/trust-manager`: `hasTrustRequiringProjectResources`, `ProjectTrustStore` 及类型。
- `core/virtual-models`: 类型与 `VIRTUAL_MODEL_STATE_ENTRY`。
- `extensions/codemode`: `createCodemodeExtension` 及类型。
- `extensions/mcp`: `createMcpExtension` 及类型。
- `extensions/tool-search`: `createToolSearchExtension`。
- `main`: `MainOptions`, `main`。
- `modes/index`: `InteractiveMode`, `InteractiveModeOptions`, `JsonAgentSessionEvent`, `ModelInfo`, `PrintModeOptions`, `RpcClient`, `RpcClientOptions`, `RpcCommand`, `RpcEventListener`, `RpcExtensionUIRequest`, `RpcExtensionUIResponse`, `RpcResponse`, `RpcSessionState`, `runPrintMode`, `runRpcMode`。
- `modes/interactive/components/index`: 全部 UI 组件与 `keyHint`, `keyText`, `renderDiff`, `truncateToVisualLines` 等。
- `modes/interactive/theme/theme`: `getLanguageFromPath`, `getMarkdownTheme`, `getSelectListTheme`, `getSettingsListTheme`, `highlightCode`, `initTheme`, `Theme` 及类型。
- `utils/clipboard`: `copyToClipboard`；`utils/frontmatter`: `parseFrontmatter`, `stripFrontmatter`；`utils/image-convert`: `convertToPng`；`utils/image-resize`: `formatDimensionNote`, `ResizedImage`, `resizeImage`；`utils/mime`: `detectSupportedImageMimeTypeFromFile`；`utils/shell`: `getPowerShellConfig`, `getShellConfig`。

### 2.9 `src/package-manager-cli.ts`（1151 行）

#### 2.9.1 类型与常量

```ts
export type PackageCommand = "install" | "remove" | "update" | "list";
type UpdateTarget = {type:"all"} | {type:"self"} | {type:"extensions"; source?: string} | {type:"models"};
const DEFAULT_INSTALLER_API_BASE = "https://kiana.dev/api/installer/releases";
const MANAGED_INSTALL_MARKER = "managed-install.json";
const MANAGED_RELEASE_VERSION_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
```

`PackageCommandOptions`（内部）：

```ts
{ command; source?; updateTarget?; showExtensionsSkippedNote; local; force;
  projectTrustOverride?; help; invalidOption?; invalidArgument?; missingOptionValue?; conflictingOptions? }
```

#### 2.9.2 managed install

- `getActiveManagedInstallRoot(): string | undefined`：
  - `configuredRoot = process.env.KIANA_MANAGED_INSTALL_ROOT?.trim()`；空 → undefined。
  - `managedRoot = resolve(configuredRoot)`；`releasesDir = canonicalizePath(join(managedRoot,"releases"))`；`getCwdRelativePath(canonicalizePath(getPackageDir()), releasesDir) === undefined` → undefined（防止源码 checkout 被误判）。
  - 读 `join(managedRoot,"managed-install.json")`（`stripBom` + JSON）：要求 `kind==="kiana-managed-install"`、`schemaVersion===1`、`layout==="releases-v1"`；否则抛 **【逐字】** `Managed install marker is missing or invalid: ${markerPath}`。
  - 返回 managedRoot。
- `async fetchInstallerArtifact(url, label)`：`fetch(url,{headers:{"User-Agent": getKianaUserAgent(VERSION)}})`；`!ok` → `Could not download managed installer ${label} from ${url}: HTTP ${response.status}`；返回 `response.text()`。
- `async runManagedNpmCi(stageDir)`：args **【逐字】** `["ci","--ignore-scripts","--omit=dev","--include=optional","--no-fund","--no-audit","--loglevel=error","--progress=false"]`；`spawnProcess("npm", args, {cwd: stageDir, stdio:"inherit"})` + `waitForChildProcess`；非 0 → `npm ${args.join(" ")} exited with code ${code ?? "unknown"}`。
- `verifyManagedRelease(releaseDir, expectedVersion)`：bin 路径 `join(releaseDir,"node_modules",".bin", win32 ? `${APP_NAME}.cmd` : APP_NAME)`；`spawnProcessSync(binPath, ["--version"], {encoding:"utf8", stdio:["ignore","pipe","pipe"]})`；`error || status!==0` → `Could not verify managed Kiana ${expectedVersion}: ${reason}`（reason = error.message / stderr.trim() / `exit code ${status ?? "unknown"}`）；`installedVersion !== expectedVersion` → `Managed Kiana smoke test returned version ${installedVersion}; expected ${expectedVersion}.`。
- `activateManagedRelease(managedRoot, version)`：写 `current-version.tmp.${pid}-${Date.now()}` 内容 `${version}\n`，rename 为 `current-version`，finally 删除临时文件。
- `pruneManagedReleases(managedRoot, activeVersion)`：遍历 `releases/`；跳过 `entry===activeVersion`、`entry===VERSION`、或不符合 `MANAGED_RELEASE_VERSION_RE`；其余 `rmSync(...,{force:true,recursive:true})`，异常忽略。
- `cleanupManagedStaging(managedRoot)`：遍历 `staging/`，删除 `update-` 前缀目录，异常忽略。
- `export function cleanupManagedInstall(): void`：取 root（异常/undefined 直接 return）；`lockfile.lockSync(join(managedRoot,"update"),{realpath:false})`；try `cleanupManagedStaging`；finally 释放；任何异常忽略。
- `async runManagedSelfUpdate(managedRoot, version)`：
  1. 版本不匹配正则 → `Invalid managed release version: ${version}`。
  2. `lockfile.lock(join(root,"update"),{realpath:false})`；`error.code==="ELOCKED"` → **【逐字】** `Another managed Kiana update is already running.`；其它错误原样抛。
  3. try：`cleanupManagedStaging`；`installerApiBase = (process.env.KIANA_INSTALLER_API_BASE?.trim() || DEFAULT_INSTALLER_API_BASE).replace(/\/+$/,"")`；`releaseUrl = `${base}/${encodeURIComponent(version)}``；`mkdirSync(releasesRoot,{recursive:true})`；若 `releases/<version>` 已存在 → `verifyManagedRelease` → `activateManagedRelease` → `pruneManagedReleases` → return。
  4. `mkdirSync(stagingRoot)`；`stageDir = mkdtempSync(join(stagingRoot,"update-"))`；并行拉取 `package.json` 与 `package-lock.json`（label 分别为同名字符串）；写入 stageDir。
  5. `runManagedNpmCi(stageDir)`；`verifyManagedRelease(stageDir, version)`；`renameSync(stageDir, releaseDir)`；`activateManagedRelease`；`pruneManagedReleases`。
  6. finally：stageDir 存在则递归删除；`await releaseLock()`。

#### 2.9.3 帮助文本（逐字）

`SELF_UPDATE_NOTE_MARKDOWN_THEME`：heading=`bold(yellow(text))`；link=`cyan`；linkUrl=`dim`；code=`yellow`；codeBlock/codeBlockBorder/quote/quoteBorder/hr=`dim`；listBullet=`yellow`；bold=`bold`；italic=`italic`；strikethrough=`strikethrough`；underline=`underline`。

`reportSettingsErrors(settingsManager, context)`：`drainErrors()` 每条 `console.error(chalk.yellow(`Warning (${context}, ${scope} settings): ${error.message}`))`；有 stack 再 `console.error(chalk.dim(error.stack))`。

`getPackageCommandUsage(command)` **【逐字】**：

```
install: `kiana install <source> [-l] [--approve|--no-approve]`
remove:  `kiana remove <source> [-l] [--approve|--no-approve]`
update:  `kiana update [source|self|kiana] [--self|--extensions|--models|--all] [--extension <source>] [--approve|--no-approve] [--force]`
list:    `kiana list [--approve|--no-approve]`
```

`CONFIG_COMMAND_USAGE` = `kiana config [-l] [--approve|--no-approve]`。

`printConfigCommandHelp()` **【逐字】**：

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

（`Usage:` 为 `chalk.bold`；`~/${CONFIG_DIR_NAME}/agent/settings.json`、`${CONFIG_DIR_NAME}/settings.json` 按常量展开。）

`printPackageCommandHelp(command)` **【逐字】**：

install：
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

remove：
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

update：
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

list：
```
Usage:
  kiana list [--approve|--no-approve]

List installed packages from user and project settings.

Options:
  -a, --approve      Trust project-local files for this command
  -na, --no-approve  Ignore project-local files for this command
```

#### 2.9.4 `parsePackageCommand(args): PackageCommandOptions | undefined`

- `rawCommand`：`uninstall` → `remove`；`install`/`remove`/`update`/`list` 原样；其它 → undefined（`handlePackageCommand` 返回 false，继续主流程）。
- 逐项解析 `rest`：
  - `-h`/`--help` → `help = true`。
  - `-l`/`--local`：仅 install/remove 接受，否则记 `invalidOption ?? arg`。
  - `--self`/`--extensions`/`--models`/`--all`：仅 update 接受，否则 invalidOption。
  - `-a`/`--approve` → override true；`-na`/`--no-approve` → false。
  - `--force`：仅 update，否则 invalidOption。
  - `--extension`：非 update → invalidOption 并继续；无值或以 `-` 开头 → `missingOptionValue ??= "--extension"`；已有值 → `conflictingOptions ??= "--extension can only be provided once"` 且 `index++`；否则取值并 `index++`。
  - 其它 `-...` → invalidOption。
  - 位置参数：首个 → `source`；其后 → `invalidArgument ?? arg`。
- update 目标推导（顺序敏感）：
  1. `allFlag && (selfFlag||extensionsFlag||modelsFlag||extensionFlagSource)` → conflict `--all cannot be combined with --self, --extensions, --models, or --extension`。
  2. `allFlag && source` → `--all cannot be combined with a positional source`。
  3. `modelsFlag`：与 self/extensions/all/extension 冲突 → `--models cannot be combined with --self, --extensions, --all, or --extension`；与位置 source 冲突 → `--models cannot be combined with a positional source`；目标 `{type:"models"}`。
  4. `extensionFlagSource`：与 self/extensions/all 冲突 → `--extension cannot be combined with --self, --extensions, or --all`；与位置 source 冲突 → `--extension cannot be combined with a positional source`；目标 `{type:"extensions", source}`。
  5. `source`：`"self"`/`"kiana"` → `extensionsFlag ? {type:"all"} : {type:"self"}`；否则与 self/extensions/all 冲突 → `positional update targets cannot be combined with --self, --extensions, or --all`，目标 `{type:"extensions", source}`。
  6. `allFlag` → `all`；`selfFlag && extensionsFlag` → `all`；`selfFlag` → `self`；`extensionsFlag` → `extensions`；否则 `self` + `showExtensionsSkippedNote = true`。

#### 2.9.5 其余函数

- `updateTargetIncludesSelf(t)`：`all|self`。`updateTargetIncludesExtensions(t)`：`all|extensions`。
- `async refreshModelCatalogs(agentDir)`：`AbortController` + 15s `setTimeout`；`ModelRuntime.create({authPath: join(agentDir,"auth.json"), modelsPath: join(agentDir,"models.json"), allowModelNetwork:false, signal})`；`refresh({allowNetwork:true, force:true, signal})`；`result.aborted` → `Model catalog refresh timed out.`；`result.errors.size>0` → `Could not refresh model catalogs: ${provider}: ${msg}; ...`（`; ` 连接）；finally `clearTimeout`；成功 `console.log(chalk.green("Model catalogs refreshed"))`。
- `printSelfUpdateUnavailable(npmCommand?, updatePackageTarget = PACKAGE_NAME)`：
  ```
  error: kiana cannot self-update this installation.
  <getSelfUpdateUnavailableInstruction(PACKAGE_NAME, npmCommand, updatePackageTarget)>
  ```
  若 `process.argv[1]` 存在再输出空行 + `Location of kiana executable: ${entrypoint}`。（前两行无 chalk；第三/四行为 stderr。）
- `printSelfUpdateFallback(command)`：`chalk.dim(`If this keeps failing, run this command yourself: ${command.display}`)`。
- `printPnpmSelfUpdateMetadataHint()`：`chalk.yellow("If pnpm reports missing package versions, its cached registry metadata may be stale.")`；`chalk.yellow(`Run \`pnpm store prune\` and retry \`${APP_NAME} update --self\`.`)`。
- `printSelfUpdateNote(note)`：`trim()` 空则返回；输出空行、`chalk.bold(chalk.yellow("Update note"))`；`width = Math.max(20, process.stdout.columns ?? 80)`；用 `new Markdown(trimmedNote, 0, 0, SELF_UPDATE_NOTE_MARKDOWN_THEME).render(width)`，每行 `trimEnd()`，`console.log(lines.join("\n"))`；异常则直接 `console.log(trimmedNote)`；最后空行。
- `interface SelfUpdatePlan { packageName; installSpec; version; shouldRun; note? }`
- `async getSelfUpdatePlan(force)`：`getLatestKianaRelease(VERSION, {retry:true})` 抛错 → `Could not determine latest kiana version: ${formatVersionCheckError(error)}`（带 `cause`）；返回 undefined → `Could not determine latest kiana version.`；`packageName = release.packageName ?? PACKAGE_NAME`；`installSpec = `${packageName}@${version}``；`force || packageName !== PACKAGE_NAME || isNewerPackageVersion(version, VERSION)` → `{..., shouldRun:true, ...(note?{note}:{})}`；否则 `console.log(chalk.green(`${APP_NAME} is already up to date (v${VERSION})`))`，返回 `shouldRun:false`。
- `async runSelfUpdate(command)`：`console.log(chalk.dim(`Updating ${APP_NAME} with ${command.display}...`))`；对 `command.steps ?? [command]` 逐步 `spawnProcess(step.command, step.args, {stdio:"inherit"})`；`error` → reject；`close`：code 0 → resolve；有 signal → `${step.display} terminated by signal ${signal}`；否则 `${step.display} exited with code ${code ?? "unknown"}`。
- `prepareWindowsNpmSelfUpdate()`：仅 win32；`cleanupWindowsSelfUpdateQuarantine(pkgDir)` + `quarantineWindowsNativeDependencies(pkgDir)`。
- `interface PackageCommandRuntimeOptions { extensionFactories?: InlineExtension[] }`
- `getCommandAppMode(): AppMode`：`stdin.isTTY && stdout.isTTY ? "interactive" : "print"`。
- `reportProjectTrustWarnings(warnings)`：每条 `console.error(chalk.yellow(`Warning: ${warning}`))`。
- `async createCommandSettingsManager({cwd, agentDir, projectTrustOverride?, useSavedProjectTrustOnly?, extensionFactories?})`：
  - `settingsManager = SettingsManager.create(cwd, agentDir, {projectTrusted:false})`。
  - `useSavedProjectTrustOnly` → `setProjectTrusted(override ?? trustStore.get(cwd)===true)`，返回。
  - 否则：若 override 未定义且有需信任资源 → `new DefaultResourceLoader({cwd,agentDir,settingsManager,extensionFactories}).loadProjectTrustExtensions()`；错误转 `Failed to load extension "${error.path}": ${error.error}` 警告。
  - `resolveProjectTrusted({cwd, trustStore, trustOverride, defaultProjectTrust, extensionsResult, projectTrustContext: createProjectTrustContext({cwd, mode: appMode, settingsManager, hasUI: appMode==="interactive"}), onExtensionError})`；`setProjectTrusted(result)`。
  - 返回 `{settingsManager, projectTrustWarnings}`。

#### 2.9.6 `handleConfigCommand(args, runtimeOptions = {}): Promise<boolean>`

1. `args[0] !== "config"` → false。
2. `rest` 含 `-h`/`--help` → `printConfigCommandHelp()`，true。
3. 逐项：`-l`/`--local` → local；`-a`/`--approve` → override true；`-na`/`--no-approve` → false；其它 `-` 开头 → stderr `chalk.red(`Unknown option ${arg} for "config".`)` + `chalk.dim(`Use "${APP_NAME} --help" or "${CONFIG_COMMAND_USAGE}".`)`，exitCode 1，true；位置参数 → `chalk.red(`Unexpected argument ${arg}.`)` + `chalk.dim(`Usage: ${CONFIG_COMMAND_USAGE}`)`，exitCode 1，true。
4. `createCommandSettingsManager({cwd, agentDir, projectTrustOverride, extensionFactories})`；`reportProjectTrustWarnings`。
5. `local && !isProjectTrusted()` → `chalk.red("Project is not trusted. Use --approve to modify local resource config.")`，exitCode 1，true。
6. `reportSettingsErrors(settingsManager, "config command")`。
7. `builtinExtensions = (extensionFactories ?? []).filter(isBuiltinExtension).map(i=>i.name)`；`globalSettingsManager = SettingsManager.create(cwd, agentDir, {projectTrusted:false})`；`globalResolvedPaths = await new DefaultPackageManager({cwd,agentDir,settingsManager: globalSettingsManager,builtinExtensions}).resolve()`；`projectResolvedPaths = isProjectTrusted() ? await new DefaultPackageManager({cwd,agentDir,settingsManager,builtinExtensions}).resolve() : globalResolvedPaths`。
8. 合并 `installFailures` 经 `formatPackageInstallWarning` 去重（Set），每条 `Warning: ${warning}`。
9. `await selectConfig({resolvedPaths:{global, project}, settingsManager, cwd, agentDir, writeScope: local?"project":"global", projectModeAvailable: settingsManager.isProjectTrusted()})`。
10. `process.exit(0)`。

#### 2.9.7 `handlePackageCommand(args, runtimeOptions = {}): Promise<boolean>`

1. `parsePackageCommand` 返回 undefined → false。
2. `help` → 打印对应帮助，true。
3. 依次检查 `invalidOption` / `missingOptionValue` / `invalidArgument` / `conflictingOptions`，错误文案：
   - `Unknown option ${invalidOption} for "${command}".` + `Use "${APP_NAME} --help" or "${usage}".`
   - `Missing value for ${missingOptionValue}.` + `Usage: ${usage}`
   - `Unexpected argument ${invalidArgument}.` + `Usage: ${usage}`
   - `${conflictingOptions}` + `Usage: ${usage}`
   均 `chalk.red` 第一行、`chalk.dim` 第二行，exitCode 1，true。
4. install/remove 缺 source → `Missing ${command} source.` + `Usage: ${usage}`，exitCode 1。
5. `update --models`：`refreshModelCatalogs(getAgentDir())`；异常 `Error: ${message ?? "Unknown model catalog refresh error"}`，exitCode 1；true。
6. `cwd`、`agentDir`；`writesProjectPackageConfig = (install|remove) && local`；`createCommandSettingsManager({..., useSavedProjectTrustOnly: command==="update", ...})`；`reportProjectTrustWarnings`。
7. `!isProjectTrusted() && writesProjectPackageConfig` → `chalk.red("Project is not trusted. Use --approve to modify local package config.")`，exitCode 1。
8. `reportSettingsErrors(settingsManager, "package command")`；`selfUpdateNpmCommand = getGlobalSettings().npmCommand`。
9. `packageManager = new DefaultPackageManager({cwd, agentDir, settingsManager})`；`setProgressCallback(event => event.type==="start" && process.stdout.write(chalk.dim(`${event.message}\n`)))`。
10. try switch：
    - install：`await installAndPersist(source, {local})`；`console.log(chalk.green(`Installed ${source}`))`。
    - remove：`removed = await removeAndPersist(source,{local})`；false → `chalk.red(`No matching package found for ${source}`)` exitCode 1；否则 `chalk.green(`Removed ${source}`)`。
    - list：`listConfiguredPackages()`；空 → `chalk.dim("No packages installed.")`；否则按 `scope==="user"` / `"project"` 分组：`User packages:` / `Project packages:`（bold；两组之间空行），每条 `  ${source}${filtered?" (filtered)":""}`，有 `installedPath` 再加 `chalk.dim(`    ${path}`)`。
    - update：`target = updateTarget ?? {type:"self"}`；`showExtensionsSkippedNote` → `chalk.dim(`Extensions are skipped. Run ${APP_NAME} update --extensions to update extensions.`)`；扩展部分：`update(source?)`，有 source → `Updated ${source}`，否则 `Updated packages`；self 部分：
      - `managedInstallRoot = getActiveManagedInstallRoot()`；有 root 且 `force` → `chalk.red(`Managed ${APP_NAME} installations do not support --force; rerun the installer to repair this installation.`)`，exitCode 1。
      - `plan = await getSelfUpdatePlan(force)`；`!shouldRun` → true。
      - 有 root：有 note 打印；`chalk.dim(`Updating managed ${APP_NAME} installation...`)`；`runManagedSelfUpdate(root, plan.version)`；异常 `Error: ${message ?? "Unknown managed update error"}` exitCode 1；成功 `chalk.green(`Updated ${APP_NAME} from ${VERSION} to ${plan.version}`)`。
      - `installMethod = detectInstallMethod()`；win32 且非 npm/pnpm → `chalk.red(`${APP_NAME} self-update on Windows is only supported for npm and pnpm installs.`)` + `chalk.dim(`Detected install method: ${installMethod}. Update ${APP_NAME} manually.`)`，exitCode 1。
      - `selfUpdateTarget = {packageName, installSpec}`；`getSelfUpdateCommand(PACKAGE_NAME, selfUpdateNpmCommand, target)`；无 → `printSelfUpdateUnavailable`，exitCode 1。
      - note 打印；try：npm 时 `prepareWindowsNpmSelfUpdate()`；`runSelfUpdate(command)`；catch：`Error: ${message ?? "Unknown package command error"}`；pnpm 额外 `printPnpmSelfUpdateMetadataHint()`；再 `printSelfUpdateFallback(command)`；exitCode 1。
      - 成功 `chalk.green(`Updated ${APP_NAME} from ${VERSION} to ${plan.version}`)`；npm 时再打印空行、`chalk.yellow(`This npm installation of ${APP_NAME} does not pin its dependencies.`)`、`chalk.yellow("Run the installer to migrate to a managed installation that does:")`、空行、`  ${chalk.bold(installerCommand)}`，其中 win32 `powershell -c "irm https://kiana.dev/install.ps1 | iex"`，否则 `curl -fsSL https://kiana.dev/install.sh | sh`。
11. 最外层 catch：`Error: ${message ?? "Unknown package command error"}`，exitCode 1，true。

### 2.10 `src/cli/auth-command.ts`（126 行）

```ts
export type AuthCommandKind = "check" | "api_key" | "bearer_token";
export interface AuthCommand { kind; args: string[]; json: boolean; credentials: boolean; noRefresh: boolean; minExpiryMs?: number }
export class AuthCommandError extends Error {}
const AUTH_COMMAND_USAGE: Record<AuthCommandKind,string>;
export function getAuthCommandName(kind): string
export function getAuthCommandUsage(kind): string
export function isAuthCommandHelp(args: string[]): boolean
export function printAuthCommandHelp(): void
export function parseAuthCommand(args: string[]): AuthCommand | undefined
export function validateAuthCommandArgs(args: Args, kind: AuthCommandKind): {provider?: string; model?: string}
export function getAuthCredential(auth: AuthResult | undefined): string | undefined
```

- usage **【逐字】**：
  - check: `kiana auth check --provider <provider> [--json] [--credentials] [--no-refresh]`
  - api_key: `kiana auth print-api-key --provider <provider> [--model <model>]`
  - bearer_token: `kiana auth print-bearer-token --provider <provider> [--model <model>] [--min-expiry <duration>]`
- 名称：`auth check` / `auth print-api-key` / `auth print-bearer-token`。
- `isAuthCommandHelp`：`args[0]==="auth" && (args[1]===undefined || args[1]==="help" || args.includes("--help") || args.includes("-h"))`。
- `printAuthCommandHelp()` **【逐字】**（`console.log` 单次，内部换行）：
```
Usage:
  kiana auth print-api-key [--provider <provider>] [--model <model>]
  kiana auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]
  kiana auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]

Auth commands require at least one of --provider or --model. Checks refresh expired OAuth credentials by default; --no-refresh prevents this. --credentials emits the credential, or includes it in JSON output.
```
- `parseAuthCommand`：`args[0]!=="auth"` → undefined；kind 由 `args[1]` 决定；未知 → 抛 `Unknown auth command "${args[1] ?? ""}". Use "kiana auth print-api-key", "kiana auth print-bearer-token", or "kiana auth check".`。
  - `--min-expiry`：仅 bearer_token；值须匹配 `/^(\d+)(ms|s|m|h)$/iu`，否则 `--min-expiry must use a duration such as 30m or 1h`；换算 ms/s/m/h = 1 / 1_000 / 60_000 / 3_600_000。
  - `--json`/`--credentials`/`--no-refresh`：仅 check，否则 `${arg} is only supported by auth check`。
  - 其余参数进入 `commandArgs`（随后交给 `parseArgs`）。
- `validateAuthCommandArgs`：trim 后取 provider/model；`unknownFlags.size>0` → `Unknown option --${option} for "${name}".`；`apiKey !== undefined || messages.length>0 || fileArgs.length>0` → `Auth commands only accept --provider and --model`；check 缺两者 → `Auth checks require --provider <provider> or --model <model>`；打印类缺两者 → `Credential printing requires --provider <provider> or --model <model>`。
- `getAuthCredential(auth)`：`auth.auth.apiKey` 优先；否则找 headers 中 `authorization`（大小写不敏感），若是字符串则 `/^Bearer\s+(.+)$/iu` 提取。

### 2.11 `src/cli/auth-check.ts`（73 行）

```ts
export type AuthCheckStatus = "ready" | "not_ready" | "invalid";
export type AuthCheckReason = "provider_not_found" | "credentials_not_configured" | "credential_not_available" | "invalid_state";
export interface AuthCheckResult { status; provider; reason?; authType?: "api_key" | "oauth" }
export async function checkProviderAuth(args, modelRuntime, options: {refresh:boolean} = {refresh:false}): Promise<AuthCheckResult>
export async function getProviderCredential(providerId, modelRuntime, credentials: CredentialStore, options: {refresh:boolean}): Promise<string | undefined>
export async function createAuthCheckModelRuntime(credentials: CredentialStore): Promise<ModelRuntime>
```

- `checkProviderAuth`：`validateAuthCommandArgs(args,"check")`；有 model → `resolveCliModel({cliProvider, cliModel, modelRuntime})`，错误/无模型 → 抛 `AuthCommandError(error ?? `Unable to resolve model "${cliModel}"`)`，provider 取 `resolved.model.provider`；无 provider → `Unable to resolve an auth provider`。
  - `modelRuntime.getError()` → `{status:"invalid", provider, reason:"invalid_state"}`。
  - 无 provider 注册 → `{status:"not_ready", provider, reason:"provider_not_found"}`。
  - `checkAuth(provider)` 空 → `credentials_not_configured`；`options.refresh && !(await getAuth(provider))` → `credentials_not_configured`；成功 → `{status:"ready", provider, authType: auth.type}`；异常 → `invalid_state`。
- `getProviderCredential`：`credentials.read(providerId)`；`!refresh && credential.type==="oauth"` → `credential.access`；否则 `getAuthCredential(await modelRuntime.getAuth(providerId))`。
- `createAuthCheckModelRuntime`：`ModelRuntime.create({credentials, modelsStore: new InMemoryCodingAgentModelsStore(), allowModelNetwork:false, refreshOnCreate:false})`。

### 2.12 `src/cli/credential-print.ts`（90 行）

```ts
const DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS = 30 * 60_000;
export async function resolveCredentialForPrint(args, modelRuntime, kind: Exclude<AuthCommandKind,"check">, minExpiryMs?, signal?): Promise<string>
```
- 先 `validateAuthCommandArgs`；`credentialTypes: Map<providerId, type>` 来自 `await modelRuntime.listCredentials({signal})`。
- `cliProvider` 分支：`getProvider(cliProvider)` 不存在 → `Unknown provider "${cliProvider}". Use --list-models to see available providers.`；有 cliModel → `resolveCliModel`，失败 → `resolved.error ?? "Unable to resolve the requested provider/model"`。
- 仅 `cliModel`（无 provider）：遍历 `getProviders()`，只看 `credentialTypes` 含有的 provider，用 `resolveModelReference({text: cliModel, provider: provider.id, thinkingSuffix:"strict", fuzzy:true}, models, ()=>true)`，`status==="resolved"` 收集；空 → `Model "${cliModel}" not found. Use --list-models to see available models.`。
- 过滤：`api_key` 跳过 oauth；`bearer_token` 跳过非 oauth；auth options：bearer_token 带 `minOAuthValidityMs: minExpiryMs ?? 30min`，并带 `signal`；`getAuth(model|providerId, options)`；`getAuthCredential` 有值则收集。
- 结果：1 个 → 返回；0 个 → 若 `cliProvider && api_key && type==="oauth"` → `Provider "${id}" is configured with OAuth, not an API key`；若 `cliProvider && bearer_token && type!=="oauth"` → `Provider "${id}" is not configured with an OAuth bearer token`；否则 `No usable ${kind==="api_key"?"API key":"OAuth bearer token"} is configured`。多个 → `Multiple configured providers matched (${ids.join(", ")}). Specify --provider.`。

### 2.13 `src/cli/list-models.ts`（115 行）

- 内部 `formatTokenCount(count)`：≥1e6 → `X M`（整数则 `${n}M`，否则 `${n.toFixed(1)}M`）；≥1e3 → `K` 同理；否则 `count.toString()`。
- `export async function listModels(modelRuntime, searchPattern?, signal?)`：
  1. `getError()` 有值 → `console.error(chalk.yellow(`Warning: errors loading models.json:\n${loadError}`))`。
  2. `models = [...await modelRuntime.getAvailable(undefined,{signal})]`；空 → `console.log(formatNoModelsAvailableMessage())` 并返回。
  3. `searchPattern` → `fuzzyFilter(models, pattern, m=>`${m.provider} ${m.id}`)`；过滤后空 → `No models matching "${searchPattern}"`。
  4. 排序：先 `provider.localeCompare`，再 `id.localeCompare`。
  5. 列：`provider`, `model`, `context`, `max-out`, `thinking`(`reasoning?"yes":"no"`), `images`(`input.includes("image")?"yes":"no"`)；宽度取表头与所有单元格最大长度；六列 `padEnd(width)` 后以 **两个空格** `"  "` 连接；先表头再每行 `console.log`。

### 2.14 `src/cli/initial-message.ts`（43 行）

```ts
export interface InitialMessageInput { parsed: Args; fileText?: string; fileImages?: ImageContent[]; stdinContent?: string }
export interface InitialMessageResult { initialMessage?: string; initialImages?: ImageContent[] }
export function buildInitialMessage(input): InitialMessageResult
```
- parts 顺序：`stdinContent`（若定义）→ `fileText`（非空）→ `parsed.messages[0]`（**shift 掉**，原地修改 `parsed.messages`）。
- 用 **空字符串** join（不是换行）。
- `initialImages`：`fileImages && fileImages.length>0` 时返回，否则 undefined。

### 2.15 `src/cli/file-processor.ts`（88 行）

```ts
export interface ProcessedFiles { text: string; images: ImageContent[] }
export interface ProcessFileOptions { autoResizeImages?: boolean } // 默认 true
export async function processFileArguments(fileArgs: string[], options?): Promise<ProcessedFiles>
```
逐文件：
1. `absolutePath = resolve(resolveReadPath(fileArg, process.cwd()))`（`resolveReadPath` 属于 tools/path-utils 单元，含 `~` 展开与 macOS 截图 Unicode 空格处理）。
2. `access` 失败 → stderr `chalk.red(`Error: File not found: ${absolutePath}`)`，`process.exit(1)`。
3. `stat().size === 0` → 跳过。
4. `detectSupportedImageMimeTypeFromFile` 判定图片：
   - 图片：`processImage(content, mime, {autoResizeImages})`；失败 → `text += `<file name="${absolutePath}">${processed.message}</file>\n``；成功 → push `{type:"image", mimeType, data}`，若有 hints → `<file name="${path}">${hints.join("\n")}</file>`，否则 `<file name="${path}"></file>`，均以 `\n` 结尾。
   - 文本：`stripBom(readFile(...utf-8))` → `<file name="${path}">\n${content}\n</file>\n`；读失败 → stderr `chalk.red(`Error: Could not read file ${absolutePath}: ${message}`)`，exit 1。

### 2.16 `src/cli/project-trust.ts`（62 行）

```ts
export function createProjectTrustContext(options: {cwd; mode: AppMode; settingsManager; hasUI: boolean}): ProjectTrustContext
```
- 返回对象：`cwd`；`mode: options.mode==="interactive" ? "tui" : options.mode`；`hasUI`；`ui`：
  - `select(title, options)`：`!hasUI` 或 `mode!=="interactive"` → undefined；否则 `showStartupSelector(settingsManager, title, options.map(o=>({label:o, value:o})))`。
  - `confirm(title, message)`：同上短路返回 false；否则 `showStartupSelector(settingsManager, `${title}\n${message}`, [{label:"Yes",value:true},{label:"No",value:false}]) ?? false`。
  - `input(title, placeholder)`：短路 undefined；否则 `showStartupInput`。
  - `notify(message, type="info")`：`mode!=="interactive"` 时 `console.error`，颜色 error=red/warning=yellow/其它=cyan；interactive 时无操作（由 TUI 处理）。

### 2.17 `src/cli/session-picker.ts`（55 行）

```ts
type SessionsLoader = (onProgress?: SessionListProgress, signal?: AbortSignal) => Promise<SessionInfo[]>;
export async function selectSession(currentSessionsLoader, allSessionsLoader, settingsManager): Promise<string | null>
```
- `ui = await createStartupTui(settingsManager)`；`KeybindingsManager.create()` → `setKeybindings`。
- 构造 `SessionSelectorComponent(currentLoader, allLoader, onSelect(path){...ui.stop(); resolve(path)}, onCancel(){...resolve(null)}, onExit(){ui.stop(); process.exit(0)}, onRender=>ui.requestRender(), {showRenameHint:false, keybindings})`。
- `ui.addChild(selector)`；`ui.setFocus(selector.getSessionList())`；`startStartupTui(ui, settingsManager)`；`resolved` 标志保证只 settle 一次。

### 2.18 `src/cli/config-selector.ts`（61 行）

```ts
export interface ConfigSelectorOptions { resolvedPaths: ScopedResolvedPaths; settingsManager; cwd; agentDir; writeScope: "global"|"project"; projectModeAvailable: boolean }
export async function selectConfig(options): Promise<void>
```
- `initTheme(settingsManager.getTheme(), true)`。
- `ui = new TuiMainScreen(new ProcessTerminal(), settingsManager.getShowHardwareCursor(), options.agentDir)`；`ui.setClearOnShrink(settingsManager.getClearOnShrink())`。
- `ConfigSelectorComponent(resolvedPaths, settingsManager, cwd, agentDir, onClose{ui.stop(); stopThemeWatcher(); resolve()}, onExit{ui.stop(); stopThemeWatcher(); process.exit(0)}, requestRender, ui.terminal.rows, writeScope, projectModeAvailable)`。
- `ui.addChild(selector)`；`ui.setFocus(selector.getResourceList())`；`ui.start()`。

### 2.19 `src/cli/startup-ui.ts`（253 行）

常量：`OFFICIAL_PACKAGE_NAME = "@earendil-works/kiana-coding-agent"`、`OFFICIAL_APP_NAME = "kiana"`、`OFFICIAL_CONFIG_DIR_NAME = ".kiana"`。

| 函数 | 行为 |
|---|---|
| `isOfficialDistribution({packageName, appName, configDirName})` | 三者全等才 true。 |
| `loadThemes(resources: ResolvedResource[]): Theme[]` | 仅 `enabled`；`loadThemeFromPath`；有 name 时去重（先到先得）；异常忽略（启动提示不应因坏主题失败）。 |
| `async loadStartupThemes(settingsManager)` | `globalSettingsManager = SettingsManager.inMemory(settingsManager.getGlobalSettings(), {projectTrusted:false})`；`new DefaultPackageManager({cwd: process.cwd(), agentDir: getAgentDir(), settingsManager: globalSettingsManager})`；`resolve(async () => "skip")`；返回 `loadThemes(resolvedPaths.themes)`。**只用全局设置、跳过安装**（传入 `"skip"` 回调）。 |
| `export async function createStartupTui(settingsManager): Promise<TUI>` | `setCapabilityOverrides(getTerminalCapabilityOverrides())`；`setRegisteredThemes(await loadStartupThemes(...))`；`markTerminalColorsPending()`；`initTheme(resolveThemeSetting(settingsManager.getThemeSetting(), getTerminalTheme()) ?? SYSTEM_THEME_NAME)`；`setKeybindings(KeybindingsManager.create())`；`ui = new TuiMainScreen(new ProcessTerminal(), getShowHardwareCursor(), getAgentDir())`；`ui.setClearOnShrink(getClearOnShrink())`。 |
| `export function startStartupTui(ui, settingsManager)` | `ui.start()`；`queryStartupTerminalColors(ui, ()=>setTheme(resolveThemeSetting(themeSetting, getTerminalTheme()) ?? SYSTEM_THEME_NAME))`。 |
| `queryStartupTerminalColors(ui, onColors)` | 不等待：`requestTerminalColors(ui, colors => { setTerminalColors(colors); onColors(); ui.invalidate(); ui.requestRender(); })`。 |
| `clearStartupTui(ui)` | `ui.clear(); ui.requestRender(); await sleep(25)`。 |
| `export function shouldRunFirstTimeSetup(settingsPath = getSettingsPath()): boolean` | 非官方分发 → false；`!areExperimentalFeaturesEnabled()`（即 `KIANA_EXPERIMENTAL !== "1"`）→ false；`process.env[ENV_AGENT_DIR]` 存在 → false；否则 `!existsSync(settingsPath)`。 |
| `export async function showStartupSelector<T>(settingsManager, title, options: Array<{label;value}>): Promise<T \| undefined>` | 创建 TUI；`ExtensionSelectorComponent(title, labels, onSelect→按 label 找 value, onCancel→undefined, {tui})`；settle 一次；`finish` 中 `await clearStartupTui(ui); ui.stop(); resolve(result)`。 |
| `export async function showFirstTimeSetup(settingsManager)` | TUI；先 `setTheme(SYSTEM_THEME_NAME)` 作为预览；`FirstTimeSetupComponent({onThemePreview: name=>{previewTheme=name; setTheme(name); ui.requestRender()}, onSubmit: result=>finish(result), onCancel: ()=>finish(undefined)})`；`finish` 中若有 result：`settingsManager.setTheme(result.theme)`、`setEnableAnalytics(result.shareAnalytics)`、`await flush()`；然后 clear/stop/resolve；`ui.start()` 后 `queryStartupTerminalColors(ui, ()=>setTheme(previewTheme))`。 |
| `export async function showStartupInput(settingsManager, title, placeholder?)` | `ExtensionInputComponent(title, placeholder, onInput, onCancel, {tui})`；`finish` 中 `input.dispose()` → clear → stop → resolve。 |

### 2.20 `src/bun/**` 与 `src/experimental/source-resolver.ts`

- `bun/sandbox-env-setup.ts`：仅 `import "./restore-sandbox-env.ts";` —— **该 side-effect import 在 `cli.ts` 之前执行**（`bun/cli.ts` 的导入顺序）。
- `bun/cli.ts`：`#!/usr/bin/env node`；`import "./sandbox-env-setup.ts"; import "./runtime-setup.ts"; import "../cli.ts";` 然后 `import { restoreSandboxEnv } from "./restore-sandbox-env.ts"; restoreSandboxEnv();`。
  注释：在评估读取环境变量的模块前恢复环境。
- `bun/runtime-setup.ts`：`process.title = APP_NAME`；`process.emitWarning = noop`；`registerBunOAuthFlows()`；`setBedrockProviderModule(bedrockProviderModule)`；`setEmbeddedQuickJSWasmPath(quickjsWasmPath)`（来自 `import quickjsWasmPath from "quickjs-wasi/quickjs.wasm"`）。
- `bun/restore-sandbox-env.ts`：`export function restoreSandboxEnv(): void` —— 非 Bun 直接返回；`Object.keys(process.env).length>0` 返回；读 `/proc/self/environ`，按 `\0` 分割，每项找第一个 `=`，要求 `idx > 0` 才设置。异常忽略。Rust 侧：原生二进制无此问题，但若编译为 Bun 不可用版本，需保持 no-op。
- `experimental/source-resolver.ts`：Node 模块解析钩子。读仓库根 `tsconfig.json`（相对本文件 `../../../..`），取 `compilerOptions.paths`；缺失 → `Source runtime requires compilerOptions.paths in ${tsconfigPath}`。仅保留 `@earendil-works/` 前缀的 pattern；含多个 `*` → `Source runtime does not support multiple wildcards in ${pattern}`；按 pattern 长度降序。`registerHooks({resolve})`：对每个 alias 匹配（无 `*` 则全等，返回 `""`）；替换每个 replacement 的 `*`，`resolveSourcePath`，命中则返回 `{url: pathToFileURL(resolved).href, shortCircuit: true}`；若匹配了 pattern 但无 replacement 命中 → 抛 `Source runtime could not resolve ${specifier} through tsconfig path ${matchedPattern}`；否则 `nextResolve`。`resolveSourcePath`：`resolve(repositoryRoot, replacement)` 必须在根内（前缀含 `sep`）；扩展名 `.js`→`.ts`、`.mjs`→`.mts`、`.cjs`→`.cts`，候选 `[base, 换扩展]`；`.ts/.mts/.cts/.json` 只候选自身；其它 `[base, base+".ts", base/index.ts]`；要求 existsSync 且 isFile。Rust 侧：不需要运行时解析器；保留仅作文档（src 直跑脚本可用 `node --import`）。

### 2.21 `src/utils/**` 逐文件

#### 2.21.1 `ansi.ts`（82 行）

常量（正则构造）：

```ts
const ST = "(?:\\u0007|\\u001B\\u005C|\\u009C)";
const OSC_START = "\\u001B\\]";
const CSI_START = "[\\u001B\\u009B][[\\]()#;?]*(?:\\d{1,4}(?:[;:]\\d{0,4})*)?";
const CSI_FINAL = "[\\dA-PR-TZcf-nq-uy=><~]";
const ansiRegex = new RegExp(`(?:${OSC_START}[\\s\\S]*?${ST})|${CSI_START}${CSI_FINAL}`, "g");
const unfinishedAnsiAtEndRegex = new RegExp(`(?:${OSC_START}(?:[^\\u0007\\u009C\\u001B]|\\u001B(?!\\\\))*|${CSI_START})$`);
const MAX_PENDING_ANSI_LENGTH = 256;
```

| 函数 | 行为 |
|---|---|
| `export function splitIncompleteAnsiSuffix(value: string): {complete; pending}` | 不含 `\x1b` 也不含 `\x9b` → `{complete:value,pending:""}`。`windowStart = max(0, value.length-256)`；在 `value.slice(windowStart)` 上匹配 `unfinishedAnsiAtEndRegex`；无匹配 → 原样；有 → `splitAt = windowStart + match.index`，`{complete: value.slice(0,splitAt), pending: value.slice(splitAt)}`。 |
| `export function stripAnsi(value: string): string` | 非 string → `throw new TypeError(`Expected a \`string\`, got \`${typeof value}\``)`；不含 ESC/CSI → 原样；否则 `value.replace(ansiRegex,"")`。 |

Rust：正则方言差异需注意（JS 的 `\u009B` 是字面字符；Rust `regex` 支持 `\x{009B}`）。`[\s\S]` 可用 `(?s:.)` 替代。

#### 2.21.2 `text.ts`（9 行）

- `splitBom(content)`：以 `\uFEFF` 开头 → `{bom:"\uFEFF", text: content.slice(1)}`；否则 `{bom:"", text:content}`。
- `stripBom(content)`：返回 `splitBom(content).text`。

#### 2.21.3 `json.ts`（6 行）

- `stripJsonComments(input)`：两遍替换：
  1. `"(?:\\.|[^"\\])*"|\/\/[^\n]*` → 字符串字面量原样保留（`m[0]==='"'`），否则删空。**只处理 `//` 行注释，不处理 `/* */`**。
  2. `"(?:\\.|[^"\\])*"|,(\s*[}\]])` → 字符串原样，否则保留捕获的 `tail`（即删掉尾随逗号）。

#### 2.21.4 `kiana-user-agent.ts`（6 行）

- `getKianaUserAgent(version)`：`formatUserAgent({userAgent:"detailed", version})`（来自 kiana-ai 单元）。默认格式 **【逐字】** `kiana/<version> (<platform>; <runtime>; <arch>)`，`runtime` 为 `bun/<ver>` 或 `node/<process.version>`；见 ai-core 规范。

#### 2.21.5 `deprecation.ts`（14 行）

- `const emittedDeprecationWarnings = new Set<string>()`；
- `warnDeprecation(message)`：已发过则忽略；否则 `console.warn(chalk.yellow(`Deprecation warning: ${message}`))`。
- `clearDeprecationWarningsForTests()`。

#### 2.21.6 `wsl.ts`（15 行）

- `isWSL(env = process.env)`：`env.WSL_DISTRO_NAME || env.WSLENV` → true；否则读 `/proc/version` 并 `/microsoft|wsl/i.test`，读失败 false。

#### 2.21.7 `sleep.ts`（18 行）

- `sleep(ms, signal?)`：`signal.aborted` → `reject(new Error("Aborted"))`；否则 `setTimeout(resolve, ms)`，abort 监听清 timer 并 `reject(new Error("Aborted"))`。

#### 2.21.8 `open-browser.ts`（24 行）

- `openBrowser(target)`：darwin `["open",[target]]`；win32 `["rundll32",["url.dll,FileProtocolHandler",target]]`；其它 `["xdg-open",[target]]`。`spawn(cmd,args,{stdio:"ignore",detached:true}).on("error",()=>{}).unref()`。**绝不使用 shell**（避免 cmd.exe 元字符注入）。

#### 2.21.9 `fs-watch.ts`（30 行）

- `export const FS_WATCH_RETRY_DELAY_MS = 5000;`
- `closeWatcher(watcher)`：空则返回；`close()` 异常忽略。
- `watchWithErrorHandler(path, listener, onError): FSWatcher | null`：`watch(path, listener)` 成功则挂 `error` 回调并返回；同步异常 → `onError()` 返回 null。

#### 2.21.10 `output-files.ts`（34 行）

- `OUTPUT_FILE_MODE = 0o600`。
- `createOutputFilePath(prefix, extension)`：`join(tmpdir(), `${prefix}-${randomBytes(8).toString("hex")}${extension}`)`（16 位十六进制）。
- `export async function writeOutputFile(prefix, extension, data): Promise<string>`：`writeFile(path, data, {mode:0o600, flag:"wx"})`（`wx` 防跟随他人放置的链接）。
- `export function createOutputFileStream(prefix, extension): {path; stream}`：`createWriteStream(path,{mode:0o600, flags:"wx"})`。

#### 2.21.11 `frontmatter.ts`（40 行）

- `normalizeNewlines`：`\r\n`→`\n`，再 `\r`→`\n`。
- `extractFrontmatter(content)`：`normalized = normalizeNewlines(stripBom(content))`；不以 `---` 开头 → `{yamlString:null, body:normalized}`；`endIndex = normalized.indexOf("\n---", 3)`，无 → 同上；否则 `{yamlString: normalized.slice(4,endIndex), body: normalized.slice(endIndex+4).trim()}`。
- `export const parseFrontmatter<T>(content)`：无 yaml → `{frontmatter:{} as T, body}`；有 → `parse(yamlString)`（`yaml` 包），`frontmatter: (parsed ?? {}) as T`。
- `export const stripFrontmatter(content)`：`parseFrontmatter(content).body`。

Rust：需用 YAML 解析库（`serde_yaml` 或 `yaml-rust2`），并注意 `yaml` 包对空串返回 null、错误消息不同（ca-cli 不暴露错误消息，只会在坏 YAML 时抛出——调用方是否捕获由扩展/技能单元决定）。

#### 2.21.12 `abort.ts`（48 行）

- `abortReason(signal)`：`signal.reason` 若定义则返回；否则创建 `Error("The operation was aborted")` 且 `name="AbortError"`。
- `operationSignal(signal?)`：`signal ?? new AbortController().signal`。
- `raceWithAbortSignal(operation, signal)`：无 signal → 原 promise；已 abort → `void operation.catch(()=>{}); reject(abortReason)`；否则挂 once 监听，`settled` 保护，cleanup 移除监听；注册后再检查一次 `signal.aborted`。

#### 2.21.13 `html.ts`（51 行）

- `decodeHtmlEntity(entity)`：`amp`→`&`、`lt`→`<`、`gt`→`>`、`quot`→`"`、`apos`→`'`；`#x`/`#X` 前缀 → `parseInt(slice(2),16)`；`#` → `parseInt(slice(1),10)`；其余 undefined。码点须整数且 `0..0x10FFFF`。
- `decodeHtmlEntityAt(html, index)`：从 `index+1` 找 `;`；无或 `semicolonIndex - index > 16` → undefined；`entity = html.slice(index+1, semicolonIndex)`；解码失败 undefined；成功 `{text, length: semicolonIndex - index + 1}`。返回类型是导出的 `interface DecodedHtmlEntity { text: string; length: number }`（§5.1 需一并声明；`length` 按 **UTF-16 码元**，见 §6.3）。

#### 2.21.14 `tool-result-images.ts`（67 行）

- `export type ToolResultContent = TextContent | ImageContent`。
- `export interface NormalizeToolResultImagesOptions { autoResizeImages?: boolean; resizeOptions?: ModelImageResizeOptions }`。
- `export async function normalizeToolResultImages(content, options?): Promise<ToolResultContent[]>`：
  - 无 image 块 → 原数组（同一引用）。
  - 默认 `autoResizeImages = true`；逐块：非 image 原样；image → `processImage(Buffer.from(data,"base64"), mimeType, {autoResizeImages, resizeOptions})`；失败 → **保留原块**（不删除）；成功且 `data/mimeType` 未变且 hints 为空 → 原块；否则替换为新块，hints 非空时追加 `{type:"text", text: hints.join("\n")}`；`changed=true`。返回 `changed ? normalized : content`。

#### 2.21.15 `zip.ts`（72 行）

- `dosDateTime(date)`：`time = (h<<11)|(m<<5)|(s>>1)`；`day = ((max(1980,year)-1980)<<9)|((month+1)<<5)|date`。
- `createZipArchive(entries)`：经典 ZIP。local header 30 字节：签名 `0x04034b50`、version 20、flags `0x0800`（UTF-8）、method 8（deflate）、time/day、crc32、压缩长度、原始长度、name 长度。central header 46 字节：`0x02014b50`、version made by 20、version needed 20、flags、method、time/day、crc32、长度、name 长度、offset（写在 42）。EOCD 22 字节：`0x06054b50`、条目数（8、10 两处）、central 目录长度（12）、offset（16）。
- `export function writeZipArchive(filePath, entries): Promise<void>`：`writeFile(filePath, createZipArchive(entries))`。
- 压缩用 `deflateRawSync`，CRC 用 `zlib.crc32`。Rust：`flate2`（raw deflate）+ `crc32fast`；`new Date()` 的本地时间字段需一致（`chrono::Local`）。

#### 2.21.16 `management-http.ts`（78 行）

```ts
const RETRYABLE_STATUS_CODES = new Set([408, 425, 429, 500, 502, 503, 504]);
export interface FetchRetryOptions { maxRetries?: number; retryOnStatus?: boolean; timeoutMs?: number; attemptTimeoutMs?: number }
export async function fetchWithRetry(input, init = undefined, options = {}): Promise<Response>
```
- `maxRetries`：undefined 或非有限 → 2；否则 `max(0, floor(x))`。
- `retryOnStatus` 默认 true。
- `parentSignal = init?.signal ?? undefined`；`timeoutSignal = timeoutMs!==undefined && timeoutMs>0 ? AbortSignal.timeout(timeoutMs) : undefined`；`attemptTimeoutMs` 同理。
- 每次尝试：`parentSignal?.throwIfAborted(); timeoutSignal?.throwIfAborted()`；`attemptTimeoutSignal = attemptTimeoutMs ? AbortSignal.timeout(...) : undefined`；`signal = 多个时 AbortSignal.any(...)`；`fetch(input, signal ? {...init, signal} : init)`。
  - 状态码可重试且 `attempt < maxRetries` → `response.body?.cancel()`（异常忽略）后重试。
  - 异常：`attemptTimedOut = attemptTimeoutSignal.aborted && !parent.aborted && !timeout.aborted`；若 `parent.aborted || timeout.aborted || (AbortError && !attemptTimedOut && timeoutSignal===undefined) || attempt>=maxRetries` → 抛出；否则重试。
- 语义要点：`timeoutMs` 是**全部尝试共享**的总预算；`attemptTimeoutMs` 是**每次尝试**独立计时（超时只中止当前尝试以便重试）。

#### 2.21.17 `paths.ts`（140 行）

- `const UNICODE_SPACES = /[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g;`
- `export interface PathInputOptions { trim?; expandTilde?; homeDir?; stripAtPrefix?; normalizeUnicodeSpaces? }`
- `canonicalizePath(path)`：`realpathSync` 失败返回原 path。
- `getFileRevision(path)`：`statSync({bigint:true})` → `${dev}:${ino}:${size}:${mtimeNs}:${ctimeNs}`；失败 undefined。
- `isLocalPath(value)`：`trim()` 后以 `npm:`/`git:`/`github:`/`http:`/`https:`/`ssh:`/`builtin:` 开头 → false；其余（含 `file:`、裸名、相对路径）→ true。
- `normalizeWindowsShellPath(filePath)`：非 `/` 开头、`//` 开头或含 `\` → 原样；匹配 `/^\/(?:mnt\/|cygdrive\/)?([a-z])(?:\/(.*))?$/i` → `<DRIVE>:\\<suffix 中 / 换 \>`。
- `normalizePath(input, options={})`：**仅当 `options.trim === true` 时才 `input.trim()`，否则原样使用输入**（源码 `let normalized = options.trim ? input.trim() : input;`——`trim` 是 opt-in，默认 **不** trim）；`normalizeUnicodeSpaces` 替换；`stripAtPrefix` 去 `@`；win32 走 `normalizeWindowsShellPath`；`expandTilde ?? true`：`~` → home；`~/`（win32 也 `~\`）→ `join(home, rest)`；`file://` → `fileURLToPath`；返回。
- `resolvePath(input, baseDir = cwd, options={})`：`normalizePath` 两者；绝对 → `nodeResolvePath(normalized)`；否则 `nodeResolvePath(base, normalized)`。
- `getCwdRelativePath(filePath, cwd): string | undefined`：解析两者；`relative(cwd, path)`；若是 `""` 或非 `..`/`../` 前缀且非绝对 → 返回 `relative || "."`，否则 undefined。
- `formatPathRelativeToCwdOrAbsolute(filePath, cwd)`：有相对路径用之，否则绝对路径；然后把 `sep` 全换成 `/`。
- `markPathIgnoredByCloudSync(path)`：darwin 属性 `["com.dropbox.ignored","com.apple.fileprovider.ignore#P"]`，linux `["user.com.dropbox.ignored"]`，其它 `[]`；darwin 用 `xattr -w <attr> 1 <path>`，其它用 `setfattr -n <attr> -v 1 <path>`，`stdio:"ignore"`。

#### 2.21.18 `child-process.ts`（137 行）

- `export const` 内部 `EXIT_STDIO_GRACE_MS = 100`。
- `spawnProcess(command, args, options)`：win32 用 `cross-spawn`，否则 `node:child_process.spawn`（两处重载仅类型差异）。
- `spawnProcessSync(command, args, options)`：win32 用 `cross-spawn.sync`，否则 `spawnSync`。
- `export function waitForChildProcess(child): Promise<number | null>`：
  - 状态：`settled`、`exited`、`exitCode`、`postExitTimer`、`stdoutEnded = child.stdout===null`、`stderrEnded = child.stderr===null`。
  - `finalize(code)` 幂等：cleanup（清 timer、移除所有监听）、`stdout?.destroy()`、`stderr?.destroy()`、resolve。
  - `maybeFinalizeAfterExit`：已 exit 且两个流都 ended → finalize。
  - `armIdleTimer`：重置 100ms 定时器 → finalize（防止继任后代持有管道时永久挂起）。
  - `onData`：exit 后仍有数据 → 重新武装 idle timer（避免截断尾部，earendil-works/kiana#5303）。
  - `onError` reject；`onExit` 记录并 maybeFinalize + armIdleTimer；`onClose` finalize(code)。
  - 监听：`stdout.once("end")`、`stderr.once("end")`、`stdout.on("data")`、`stderr.on("data")`、`child.once("error"/"exit"/"close")`。

#### 2.21.19 `mime.ts`（116 行）

- `IMAGE_TYPE_SNIFF_BYTES = 4100`；`PNG_SIGNATURE = [0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a]`。
- `detectSupportedImageMimeType(buffer)`：
  - `FF D8 FF` 开头：`buffer[3]===0xF7`（JPEG-LS？）→ null，否则 `image/jpeg`。
  - PNG 签名：`isPng` 且非动画 → `image/png`，否则 null。
  - `GIF87a`/`GIF89a` → `image/gif`。
  - `RIFF` + offset 8 处 `WEBP` → `image/webp`。
  - `BM` + `isBmp` → `image/bmp`。
  - 其它 null。
- `detectSupportedImageMimeTypeFromFile(filePath)`：`open(path,"r")`，读最多 4100 字节，`detectSupportedImageMimeType(subarray(0,bytesRead))`，finally close。
- `isPng`：长度 ≥16，且 `readUint32BE(8)===13`（IHDR 长度）且 offset 12 为 `IHDR`。
- `isAnimatedPng`：从 offset 8 遍历 chunk：chunk type 为 `acTL` → true；`IDAT` → false；`nextOffset = offset+8+chunkLength+4`，若 `nextOffset<=offset` 或 `> buffer.length` → false。无结论 false。
- `isBmp`：长度 <26 → false；`declaredFileSize`（offset 2，LE）、`pixelDataOffset`（10）、`dibHeaderSize`（14）；`declaredFileSize!==0 && <26` → false；`pixelDataOffset < 14+dibHeaderSize` → false；`declaredFileSize!==0 && pixelDataOffset>=declaredFileSize` → false；`dibHeaderSize===12` 时 `colorPlanes=readU16(22)`、`bpp=readU16(24)`；`40..124` 时要求长度 ≥30，`colorPlanes=readU16(26)`、`bpp=readU16(28)`；其它 false；最终 `colorPlanes===1 && bpp ∈ {1,4,8,16,24,32}`。

#### 2.21.20 `image-process.ts`（119 行）

```ts
export interface ProcessImageOptions { autoResizeImages?: boolean; resizeOptions?: ImageResizeOptions }
export type ProcessImageResult = {ok:true; data:string; mimeType:string; hints:string[]} | {ok:false; message:string}
export async function processImage(bytes: Uint8Array, mimeType: string, options?): Promise<ProcessImageResult>
```
- `baseMimeType` 去掉 `;` 后参数并小写/trim。
- `normalizeSupportedImageMimeType`：`image/png`、`image/jpeg`、`image/jpg`→`image/jpeg`、`image/gif`、`image/webp`；其它 null。
- `normalizeImage`：受支持直接返回；否则 `convertImageBytesToPng(bytes)`，失败 null，成功 `{bytes, mimeType:"image/png", convertedFrom: baseMimeType(原)}`。
- 失败 **【逐字】** `[Image omitted: could not be converted to a supported inline image format.]`。
- `conversionHint(from,to)`：无 from 或相等 → undefined；否则 **【逐字】** `[Image converted from ${from} to ${to}.]`。
- `autoResizeImages`（默认 true）：`resizeImage(bytes, mime, resizeOptions)`；null → `{ok:false, message:"[Image omitted: could not be resized below the inline image size limit.]"}`；成功 hints = [转换提示（若转换后格式 ≠ 缩放后格式的 base 名）] + [`formatDimensionNote`]。
- 不缩放：hints 仅转换提示；`data = Buffer.from(bytes).toString("base64")`。

#### 2.21.21 `image-resize.ts`（122 行）

- 重新导出 `ImageResizeOptions` / `ResizedImage`（定义在 core）。
- `toTransferableBytes(input)`：`new Uint8Array(input)`（worker 拥有副本，调用方缓冲不被 detach）。
- `isResizeImageWorkerResponse(value)`：对象且 `type === IMAGE_RESIZE_WORKER_RESPONSE_TYPE`。
- `resizeImageInWorker(workerSpecifier, inputBytes, mimeType, options?)`：`new Worker(specifier)`；post `{inputBytes, mimeType, options}` 并 transfer buffer；`message` 中非响应消息忽略（Node `--watch` 会发 `{"watch:require":[...]}`）；`error` 字段 → reject；`exit` 未 settle → `Image resize worker exited with code ${code}`；finally `worker.terminate().catch(()=>undefined)`。
- `export async function resizeImage(inputBytes, mimeType, options?)`：
  - `isTypeScriptRuntime = import.meta.url.endsWith(".ts")`；workerUrl = `new URL(isTS ? "./image-resize-worker.ts" : "./image-resize-worker.js", import.meta.url)`。
  - Bun（`typeof process.versions.bun === "string"`）先试字符串路径 `"./src/utils/image-resize-worker.ts"`，失败忽略。
  - 再试 workerUrl，失败 → `resizeImageInProcess(...)` 兜底。
  - Rust：无需 worker；直接调用 `image_resize_core`（可放在 `spawn_blocking`）。
- `export function formatDimensionNote(result): string | undefined`：`!wasResized` → undefined；`scale = originalWidth / width`；**【逐字】** `[Image: original ${ow}x${oh}, displayed at ${w}x${h}. Multiply coordinates by ${scale.toFixed(2)} to map to original image.]`。

#### 2.21.22 `image-resize-core.ts`（174 行）

```ts
export interface ImageResizeOptions { maxWidth?: number; maxHeight?: number; maxBytes?: number; jpegQuality?: number }
export interface ResizedImage { data: string; mimeType: string; originalWidth: number; originalHeight: number; width: number; height: number; wasResized: boolean }
export const IMAGE_RESIZE_WORKER_RESPONSE_TYPE = "kiana:image-resize-response";
export type ResizeImageWorkerResponse = {type; result: ResizedImage|null} | {type; error: string};
const DEFAULT_MAX_BYTES = 4.5 * 1024 * 1024;
const DEFAULT_OPTIONS = { maxWidth:2000, maxHeight:2000, maxBytes:DEFAULT_MAX_BYTES, jpegQuality:80 };
export async function resizeImageInProcess(inputBytes, mimeType, options?): Promise<ResizedImage | null>
```
算法（必须逐步骤一致，输出字节才可能一致）：
1. `opts = {...DEFAULT_OPTIONS, ...options}`；`inputBase64Size = ceil(byteLength/3)*4`。
2. `loadPhoton()` 为 null → null。
3. `rawImage = PhotonImage.new_from_byteslice(inputBytes)`；`image = applyExifOrientation(photon, rawImage, inputBytes)`；若返回新对象则 `rawImage.free()`。
4. `originalWidth/Height`；`format = mimeType.split("/")[1] ?? "png"`。
5. 若 `w<=maxWidth && h<=maxHeight && inputBase64Size < maxBytes` → 直接返回原字节 base64，`mimeType: mimeType || `image/${format}``，`wasResized:false`。
6. 计算目标尺寸：宽超限时先按宽缩放（高四舍五入），再检查高超限按高缩放。
7. `tryEncodings(w,h,qualities)`：`photon.resize(image,w,h,SamplingFilter.Lanczos3)`；候选 = PNG 字节 + 每个质量步的 JPEG 字节；resized.free()。
8. `qualitySteps = Array.from(new Set([opts.jpegQuality, 85, 70, 55, 40]))`。
9. 循环：生成候选，按顺序找第一个 `encodedSize < maxBytes` 的，返回 `{wasResized:true, width: currentWidth, height: currentHeight, ...}`；否则若 1x1 则 break；否则 `next = max(1, floor(current*0.75))`（每维独立；已是 1 保持 1），若两维都没变小则 break；更新。
10. 达 1x1 仍超限 → null；任何异常 → null；finally `image?.free()`。
- `encodeCandidate`：`data = Buffer.from(buffer).toString("base64")`；`encodedSize = Buffer.byteLength(data,"utf-8")`（即 base64 字符数）。

#### 2.21.23 `image-resize-worker.ts`（43 行）

- 无 `parentPort` → `throw new Error("image resize worker requires parentPort")`。
- `isResizeImageWorkerRequest`：对象、`inputBytes instanceof Uint8Array`、`mimeType` 为 string。
- `port.once("message")`：非法请求 → `Invalid image resize worker request`；成功 `{type, result}`；异常 `{type, error: message}`。
- Rust：不需要（无 worker 线程协议）。

#### 2.21.24 `exif-orientation.ts`（182 行）

- `readOrientationFromTiff(bytes, tiffStart)`：需要 `tiffStart+8 <= len`；byte order `0x4949` 为 little-endian；`read16/read32`；`ifdOffset = read32(tiffStart+4)`；`ifdStart = tiffStart+ifdOffset`；`ifdStart+2 > len` → 1；`entryCount = read16(ifdStart)`；每 12 字节条目；tag `0x0112` → 返回 `read16(entryPos+8)`（1..8 有效，否则 1）；遍历完 1。
- `findJpegTiffOffset(bytes)`：从 offset 2 扫描 marker：非 `0xFF` → -1；`0xFF` 填充跳过；`0xE1`（APP1）时检查 segmentStart=`offset+4` 处 `Exif\0\0` 头，命中返回 `segmentStart+6`（即 TIFF 起点）；否则 `length = (bytes[offset+2]<<8)|bytes[offset+3]`，`offset += 2 + length`；边界不足 → -1。
- `findWebpTiffOffset(bytes)`：从 offset 12 遍历 RIFF chunk（id 4 字节 ASCII，size 小端 32 位，数据从 +8）；`"EXIF"` chunk：`dataStart+chunkSize > len` → -1；若 `chunkSize>=6` 且 `dataStart` 处是 `Exif\0\0` → `dataStart+6`，否则 `dataStart`；其它 chunk `offset = dataStart + chunkSize + (chunkSize % 2)`。
- `getExifOrientation(bytes)`：JPEG（`FF D8`）或 WebP（`RIFF....WEBP`）→ 对应查找；-1 → 1；否则读 TIFF。
- `rotate90(photon, image, dstIndex)`：`src = image.get_raw_pixels()`；对每像素复制 RGBA 到 `dstIndex(x,y,w,h)*4`；返回 `new photon.PhotonImage(dst, h, w)`。
- `export function applyExifOrientation(photon, image, originalBytes)`：
  - orientation 1 → 原图；2 → `fliph`；3 → `fliph`+`flipv`；4 → `flipv`；
  - 5 → rotate90 用 `x*h + (h-1-y)` 再 `fliph`；
  - 6 → rotate90 用 `x*h + (h-1-y)`；
  - 7 → rotate90 用 `(w-1-x)*h + y` 再 `fliph`；
  - 8 → rotate90 用 `(w-1-x)*h + y`；
  - 其它 → 原图。翻转原地修改，旋转返回新图（调用方负责释放旧图）。
- Rust：`image` crate 的 `DynamicImage` 无“orientation”设；需手工按上述索引做像素旋转，且用无损 RGB/RGBA 内存布局。若要字节级一致，必须先复刻 Photon 的 `get_raw_pixels` 布局（RGBA8）与 `PhotonImage` 构造（宽高互换）。

#### 2.21.25 `photon.ts`（139 行）

- 惰性加载 `@silvia-odwyer/photon-node`；首次加载前 patch `fs.readFileSync`：若目标路径以 `photon_rs_bg.wasm` 结尾且读取抛 ENOENT（`err.code` 存在且不等于 ENOENT 则原样抛），依次尝试 `dirname(process.execPath)/photon_rs_bg.wasm`、`.../photon/photon_rs_bg.wasm`、`cwd/photon_rs_bg.wasm`，命中则用它重试（保持 options）；都不行则抛原错误。恢复原函数（赋值失败用 `Object.defineProperty`）。
- `loadPhoton()` 缓存 promise 与模块；失败返回 null（并缓存 null）。
- Rust：`image` crate 或 `photon-rs` 的 wasm 不适用，应替换为原生解码（`image` crate）+ 自实现 resize 滤镜（Lanczos3），并注意 JPEG 编码器差异导致输出字节与 TS 不一致——见 §8 最难复刻项。

#### 2.21.26 `image-convert.ts`（89 行）

- `encodePng(photon, bytes)`：`new_from_byteslice` → `applyExifOrientation` → 若新对象则释放原图 → `new Uint8Array(image.get_bytes())` → finally `image.free()`；异常 null。
- `export async function convertImageBytesToPng(bytes): Promise<Uint8Array | null>`：`loadPhoton` 为 null → null。
- `export async function convertToPng(base64Data, mimeType): Promise<{data;mimeType}|null>`：已是 `image/png` → 原样返回；否则解码 base64 → 转换；失败 null；返回 base64 + `image/png`。
- `export async function loadPngTranscoder(): Promise<ImageTranscoder | undefined>`：返回同步函数 `(base64Data) => pngBase64 | null`。
- `export function ensurePngTranscoder(onRegistered: () => void): void`：
  - 已注册或 `getCapabilities().images !== "kitty"` → 返回。
  - `pngTranscoderLoad ??= loadPngTranscoder().then(t => t ? (setImageTranscoder(t), registered=true, true) : false)`。
  - 注册成功才调用 `onRegistered()`。

#### 2.21.27 `shell.ts`（264 行）

```ts
export interface ShellConfig { shell: string; args: string[]; commandTransport?: "argv" | "stdin" }
```
- `isLegacyWslBashPath(path)`：`win32.normalize(path).toLowerCase()`；有 `SystemRoot` 时要求其形如 `^[a-z]:[\\/]`，比较 `join(SystemRoot,"System32"|"Sysnative","bash.exe")`；无 SystemRoot 时回退正则 `^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$`。
- `getBashShellConfig(shell)`：legacy WSL bash → `{shell, args:["-s"], commandTransport:"stdin"}`；否则 `{shell, args:["-c"]}`。
- `shellPathExists(path)`：`accessSync(path, F_OK)`（**不用 existsSync**，Windows Store 别名）。
- `findExecutableOnPath(executable)`：win32 用 `where <exe>`，`timeout:5000, windowsHide:true`，取 stdout 第一行（`/\r?\n/` 分割）且路径存在才返回；Unix 用 `which <exe>`，`timeout:5000`，取第一行非空；异常/null。
- `export function getShellConfig(customShellPath?)`：
  1. 自定义路径存在 → `getBashShellConfig`；不存在 → `Custom shell path not found: ${customShellPath}`。
  2. win32：`ProgramFiles\Git\bin\bash.exe`、`ProgramFiles(x86)\Git\bin\bash.exe`，存在则用之；否则 `findExecutableOnPath("bash.exe")`；都无 → `No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n${paths.map(p=>`  ${p}`).join("\n")}`。
  3. Unix：`/bin/bash` → 用之；否则 PATH 上 `bash`；否则 `{shell:"sh", args:["-c"]}`。
- `export const POWERSHELL_ARGS = ["-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-Command"] as const;`
- `export function getPowerShellConfig()`：非 win32 → `The powershell tool is only available on Windows.`；`findExecutableOnPath("pwsh.exe")` → `ProgramFiles\PowerShell\7\pwsh.exe` → `findExecutableOnPath("powershell.exe")` → `SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe`；都无 → `No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.`；返回 `{shell, args:[...POWERSHELL_ARGS]}`。
- `export function getShellEnv(): NodeJS.ProcessEnv`：找 PATH 键（大小写不敏感，缺省 `"PATH"`）；按 `delimiter` 分割过滤空；已包含 binDir 则原样；否则 `[binDir, currentPath].filter(Boolean).join(delimiter)`；返回 `{...process.env, [pathKey]: updated}`。
- `export function sanitizeBinaryOutput(str: string): string`：删除 `/[\x00-\x08\x0B\x0C\x0E-\x1F\uFFF9-\uFFFB]/g`（保留 `\t`(09)、`\n`(0A)、`\r`(0D)）。
- 分离子进程追踪：`trackDetachedChildPid(pid)`、`untrackDetachedChildPid(pid)`、`killTrackedDetachedChildren()`（逐个 `killProcessTree` 后清空）。
- `export function killProcessTree(pid: number): void`：
  - win32：`spawn(join(SystemRoot||"C:\\Windows","System32","taskkill.exe"), ["/F","/T","/PID",String(pid)], {stdio:"ignore", detached:true, windowsHide:true})`，`once("error",()=>{})`。
  - 其它：darwin 先 `process.kill(-pid,"SIGSTOP")`（失败忽略，减少 `a; b` 竞态，#11252）；再 `process.kill(-pid,"SIGKILL")`；失败回退 `process.kill(pid,"SIGKILL")`；再失败忽略。Linux 注释解释 tasklist_lock 语义。

#### 2.21.28 `clipboard-command.ts`（47 行）

```ts
export function runClipboardCommand(command: string, args: readonly string[], options?: {input?: string; timeoutMs?: number; maxBufferBytes?: number}): Promise<Buffer | undefined>
```
- spawn 配置：`stdio: ["pipe", options?.input===undefined ? "pipe" : "ignore", "ignore"]`；`detached: process.platform !== "win32"`；`windowsHide:true`（注释：writer 可能守护化；POSIX detach 使其脱离进程组，Ctrl+Z 不会停掉剪贴板所有者，#11179）。
- 默认 `timeoutMs` 3000；默认 `maxBufferBytes` 50MB。
- 超时 → `child.kill("SIGKILL")`、销毁 stdin/stdout、`finish(undefined)`。
- `error` → undefined；`close`：code 0 → `Buffer.concat(chunks,length)`，否则 undefined。
- stdout `data`：累计长度超限 → abort（undefined）。
- stdin `error` 忽略；`stdin.end(options?.input)`。
- Rust：注意“timeout 即失败（undefined）”与“空输出是成功（空 Buffer）”的区分。

#### 2.21.29 `clipboard.ts`（150 行）

- `MAX_OSC52_ENCODED_LENGTH = 100_000`。
- `isRemoteSession(env)`：`env.SSH_CONNECTION || env.SSH_CLIENT || env.MOSH_CONNECTION || env.KIANA_CLIPBOARD_OSC52 === "1"`。
- `emitOsc52(text)`：`encoded = Buffer.from(text).toString("base64")`；长度超 100000 → false；否则 `process.stdout.write(`\x1b]52;c;${encoded}\x07`)` 返回 true。
- `copyViaWindowsClipboard(text)`：写临时文件 `join(tmpdir(), `kiana-wsl-clip-${randomUUID()}.txt`)`（utf8，0600）；`wslpath -w <tmp>`（timeout 1000）→ winPath（trim）；无则 false；脚本 **【逐字】** `Set-Clipboard -Value ([System.IO.File]::ReadAllText('<winPath 单引号转义为 ''>', [System.Text.Encoding]::UTF8))`；`powershell.exe -NoProfile -Command <script>`（timeout 5000）；返回结果是否 undefined；finally 删临时文件。
- `export async function readClipboardText(): Promise<string | null>`：命令链——`TERMUX_VERSION` → `termux-clipboard-get`；linux 且 `WAYLAND_DISPLAY` → `wl-paste --no-newline --type text`；linux 且 `DISPLAY` → `xclip -selection clipboard -out`，`xsel --clipboard --output`；每个 timeout 5000；成功则 `bytes.toString("utf8") || null`；最后回退 `getNativeClipboard()?.getText() || null`（异常 null）。
- `export async function readClipboardFilePaths(): Promise<string[] | null>`：`getNativeClipboard()?.getFilePaths?.()`；空则 null。
- `export async function copyToClipboard(text): Promise<void>`：
  1. 非 linux 先试原生剪贴板 `setText`（异常忽略）。
  2. 未成功则命令链：darwin `pbcopy`；win32 `clip`；其它 `TERMUX_VERSION` → `termux-clipboard-set`；`WAYLAND_DISPLAY` → `wl-copy`；`DISPLAY` → `xclip -selection clipboard` 或 `xsel --clipboard --input`；带 `input: text`，timeout 5000；首个成功即 break。
  3. 未成功且 linux 且 `isWSL(env)`：有 `WT_SESSION` 先 `emitOsc52`；否则 `copyViaWindowsClipboard`。
  4. `headless = linux && !DISPLAY && !WAYLAND_DISPLAY && !TERMUX_VERSION`；未发过 OSC52 且（远程会话或（未成功且 headless））→ `emitOsc52`，成功置 copied，超限置 oversized。
  5. 成功 return。否则错误（**【逐字】**）：
     - oversized → `Clipboard unavailable: text exceeds the OSC 52 size limit`
     - TERMUX → `Clipboard unavailable: install the Termux:API app and \`termux-api\` package`
     - linux+Wayland → `Clipboard unavailable: install \`wl-clipboard\` (\`wl-copy\`) or check Wayland access`
     - linux+X11 → `Clipboard unavailable: install \`xclip\` or \`xsel\`, or check X11 access`
     - 其它 → `Clipboard unavailable`

#### 2.21.30 `clipboard-image.ts`（240 行）

- `export type ClipboardImage = {bytes: Uint8Array; mimeType: string}`。
- `SUPPORTED_IMAGE_MIME_TYPES = ["image/png","image/jpeg","image/webp","image/gif"]`；`DEFAULT_LIST_TIMEOUT_MS = 1000`；`DEFAULT_POWERSHELL_TIMEOUT_MS = 5000`。
- `export function isWaylandSession(env = process.env)`：`WAYLAND_DISPLAY` 或 `XDG_SESSION_TYPE==="wayland"`。
- `export function extensionForImageMimeType(mimeType): string | null`：png/jpeg→jpg/webp/gif，其它 null。
- `selectPreferredImageMimeType(types)`：按 `SUPPORTED_IMAGE_MIME_TYPES` 顺序找第一个 base 匹配，返回**原始字符串**；否则第一个 `image/` 前缀的原始字符串；否则 null。
- `readClipboardImageViaWlPaste()`：`wl-paste --list-types`（1000ms）；undefined → undefined（后端失败）；解析类型；无首选 → null（无图）；`wl-paste --type <t> --no-newline`；undefined → undefined；空 → null；否则 `{bytes, mimeType: base}`。
- `readClipboardImageViaPowerShell()`：临时文件 `kiana-wsl-clip-<uuid>.png`；`wslpath -w`（1000ms）；脚本（逐行 join `"; "`）：
  ```
  Add-Type -AssemblyName System.Windows.Forms
  Add-Type -AssemblyName System.Drawing
  $path = '<winPath 单引号转义>'
  $img = [System.Windows.Forms.Clipboard]::GetImage()
  if ($img) { $img.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' } else { Write-Output 'empty' }
  ```
  `powershell.exe -NoProfile -Command <script>`（5000ms）；输出须为 `ok`；读文件；空 → null；返回 png。异常 null；finally 删临时文件。
- `readClipboardImageViaXclip()`：`xclip -selection clipboard -t TARGETS -o`（1000ms）；解析类型；无首选 null；`xclip -selection clipboard -t <t> -o`；空 → null。
- `readClipboardImageViaNativeClipboard()`：`getNativeClipboard()?.getImage()`；undefined → undefined；空 → null；mime 用 `detectSupportedImageMimeType(bytes) ?? "application/octet-stream"`。
- `export async function readClipboardImage(options?: {env?; platform?})`：`TERMUX_VERSION` → null。linux 时：`isWaylandSession || isWSL` → wl-paste；`undefined` 则 xclip；`!image && isWSL` 则 PowerShell（保留 Linux 的“空/不可用”区分）；仍 undefined 则原生。非 linux 直接原生。无图 → null。非受支持 mime → Photon 转 PNG（失败 null），返回 `{pngBytes, "image/png"}`。

#### 2.21.31 `tools-manager.ts`（397 行）

- `TOOLS_DIR = getBinDir()`；`NETWORK_TIMEOUT_MS = 10_000`；`DOWNLOAD_TIMEOUT_MS = 120_000`。
- `isOfflineModeEnabled()`：`KIANA_OFFLINE` 为 `1`/`true`/`yes`（大小写不敏感）。
- `ToolConfig {name; repo; binaryName; systemBinaryNames?; tagPrefix; getAssetName(version,target)}`。
  - fd：repo `sharkdp/fd`，binary `fd`，system names `["fd","fdfind"]`，tagPrefix `v`，asset `fd-v${version}-${target}`。
  - rg：repo `BurntSushi/ripgrep`，binary `rg`，tagPrefix `""`，asset `ripgrep-${version}-${target}`。
- `getReleaseTarget(plat, arch)`：arch `arm64`→`aarch64`，`x64`→`x86_64`，其它 undefined；darwin `${arch}-apple-darwin.tar.gz`；linux `${arch}-unknown-linux-musl.tar.gz`；win32 `${arch}-pc-windows-msvc.zip`；其它 undefined。
- `commandExists(cmd)`：`spawnSync(cmd,["--version"],{stdio:"pipe"})`；`result.error` 为 undefined/null → true。
- `export function getToolPath(tool): string | null`：先看 `bin/<binary><win .exe>` 存在；再按 system 名 `commandExists`，命中返回命令名。
- `export async function getLatestVersion(repo): Promise<string>`：`fetchWithRetry(https://github.com/${repo}/releases/latest, {headers:{"User-Agent":getKianaUserAgent(VERSION)}, redirect:"manual"}, {timeoutMs:10000})`；丢弃 body；`status` 在 300..399 且 `location` 存在；否则 `Failed to resolve latest ${repo} release: HTTP ${status} without redirect`；`tag = new URL(location,"https://github.com").pathname.split("/").pop()`；无 tag 或 location 不含 `/releases/tag/` → `Failed to resolve latest ${repo} release: unexpected redirect to ${location}`；返回 `decodeURIComponent(tag).replace(/^v/,"")`。
- `downloadFile(url, dest)`：`fetchWithRetry(url, undefined, {timeoutMs:120000})`；`!ok` → `Download failed with HTTP ${status}: ${url}`；无 body → `No response body`；`pipeline(Readable.fromWeb(body), createWriteStream(dest))`。
- `findBinaryRecursively(rootDir, name)`：显式栈 DFS；返回首个匹配文件。
- `formatSpawnFailure(result)`：`error.message` → stderr.trim() → stdout.trim() → `exit status ${status ?? "unknown"}`。
- `runExtractionCommand(cmd,args)`：成功（无 error 且 status 0）→ null；否则 `${command}: ${formatSpawnFailure(result)}`。
- `extractTarGzArchive`：`tar xzf <archive> -C <dir>` 失败 → `Failed to extract ${assetName}: ${failure}`。
- `getWindowsTarCommand()`：`SystemRoot/System32/tar.exe` 存在则用，否则 `"tar.exe"`。
- `extractZipArchive`：win32 先 bsdtar `tar xf`，失败再 PowerShell `Expand-Archive`（脚本 **【逐字】** `& { param($archive, $destination) $ErrorActionPreference = 'Stop'; Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force }`，参数 `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command <script> <archive> <dir>`）；非 win32 先 `unzip -q`，再 `tar xf`；全失败 → `Failed to extract ${assetName}: ${failures.join("; ")}`。
- `downloadTool(tool)`：
  - target 无 → `Unsupported platform: ${plat}/${arch}`。
  - 版本：fd 且 darwin/x64 → 固定 `"10.3.0"`；否则 `getLatestVersion(repo)`。
  - `mkdirSync(TOOLS_DIR,{recursive:true})`；URL `https://github.com/${repo}/releases/download/${tagPrefix}${version}/${assetName}`；archive 落在 TOOLS_DIR。
  - 解压目录 `extract_tmp_${binaryName}_${pid}_${Date.now()}_${Math.random().toString(36).slice(2,10)}`（唯一，避免 fd/rg 并发竞争）。
  - 按后缀 `.tar.gz`/`.zip`；否则 `Unsupported archive format: ${assetName}`。
  - 找 binary：先 `extractDir/<asset 去后缀>/<binaryName>`，再 `extractDir/<binaryName>`；否则递归搜索；都没有 → `Binary not found in archive: expected ${binaryFileName} under ${extractDir}`。
  - rename 到 bin；非 win32 `chmod 0o755`；finally 删除 archive 与解压目录。
- `TERMUX_PACKAGES = {fd:"fd", rg:"ripgrep"}`。
- `export interface ToolStatus {type: "info"|"warning"; message: string}`。
- `export async function ensureTool(tool, onStatus?)`：
  1. 已有 → 返回。
  2. 离线 → warning `${config.name} not found. Offline mode enabled, skipping download.`，undefined。
  3. `platform()==="android"` → `${config.name} not found. Install with: pkg install ${pkgName}`。
  4. 无 release target → `${config.name} not found. Automatic download is not supported on ${platform()}/${arch()}. Install ${config.name} with your system package manager.`。
  5. info `${config.name} not found. Downloading...`；成功 → info `${config.name} installed to ${path}`；失败 → warning `Failed to download ${config.name}: ${messages.join(": ") || String(e)}`（沿 `cause` 链最多 5 层、去重消息）。

#### 2.21.32 `version-check.ts`（109 行）

- `LATEST_VERSION_URL = "https://kiana.dev/api/latest-version"`；`DEFAULT_VERSION_CHECK_TIMEOUT_MS = 10000`。
- `export interface LatestKianaRelease {version; packageName?; note?}`。
- `export function formatVersionCheckError(error)`：`rootMessage = error.message || String(error)`；`cause` 为 AggregateError → `cause.errors`，否则单元素或空；收集 `code` 字符串去重 → `${rootMessage} (${codes.join(", ")})`；否则首个有 message 的 Error → `${rootMessage} (cause: ${msg})`；否则 rootMessage。
- `comparePackageVersions(l,r)`：`semver.valid(trim)` 均有效 → `compare`，否则 undefined。
- `isNewerPackageVersion(candidate, current)`：可比较 → `>0`；否则 `candidate.trim() !== current.trim()`。
- `export async function getLatestKianaRelease(currentVersion, options: {timeoutMs?; retry?} = {})`：`process.env.KIANA_OFFLINE` → undefined；`fetchWithRetry(url, {headers:{"User-Agent":getKianaUserAgent(currentVersion), accept:"application/json"}}, {maxRetries: options.retry?2:0, timeoutMs: options.timeoutMs ?? 10000})`；`!ok` → undefined；JSON 中 `version` 必须是非空 string（trim 后有值）；`packageName`、`note` 若为非空 string 则 trim；返回 `{version, packageName, ...(note?{note}:{})}`。
- `getLatestKianaVersion`：返回 `.version`。
- `export async function checkForNewKianaVersion(currentVersion)`：`KIANA_SKIP_VERSION_CHECK` → undefined；`getLatestKianaRelease(currentVersion)` 且 `isNewerPackageVersion` → 返回；否则 undefined；异常吞掉。

#### 2.21.33 `windows-self-update.ts`（84 行）

- `QUARANTINE_DIR_NAME = ".kiana-native-quarantine"`。
- `normalizePath(path)`：`toNamespacedPath(resolve(path))`。
- `getQuarantineRoot(packageDir)`：向上找 `node_modules` 目录（大小写不敏感）→ `join(that, ".kiana-native-quarantine")`；到根 → undefined。
- `getLoadedSharedObjectsInPackageDir(packageDir)`：`process.report.getReport().sharedObjects`（数组）；根路径规范化小写；`getCwdRelativePath(filePath.toLowerCase(), root)` 非 undefined 且未见过 → 收集原 path。
- `export function cleanupWindowsSelfUpdateQuarantine(packageDir)`：无 root 返回；`rmSync(root,{recursive:true,force:true})`，异常忽略。
- `export function quarantineWindowsNativeDependencies(packageDir)`：无 root 或无已加载文件返回；`quarantineRunDir = join(root, `${Date.now()}-${pid}-${randomUUID()}`)`；对每个存在文件：`mkdirSync(dirname(quarantinePath),{recursive:true})`；`renameSync(loaded, quarantinePath)`；`copyFileSync(quarantinePath, loaded)`（把文件搬进隔离区再复制回来，避免升级时占用）。
- Rust：`process.report` 无对应；需用平台 API（`/proc/self/maps`、`dyld` 镜像列表、`EnumProcessModules`）枚举已加载 .so/.dll/.dylib。非 Windows 平台可为 no-op，但 `cleanupWindowsSelfUpdateQuarantine` 在 `main` 中仅在 win32 调用。

#### 2.21.34 `changelog.ts`（196 行）

- `GITHUB_REPO = "earendil-works/kiana"`；`CHANGELOG_LINK_BASE_PATH = "packages/coding-agent"`；
  `LEGACY_REPO_RE = /^https:\/\/github\.com\/(?:badlogic|earendil-works)\/kiana-mono(?=\/|$)/`；
  `URL_SCHEME_RE = /^[a-z][a-z0-9+.-]*:/i`；
  `INLINE_MARKDOWN_LINK_RE = /(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))/g`。
- `export interface ChangelogEntry {major;minor;patch;content}`。
- `entryVersion(entry)` → `x.y.z`；`normalizeTag(v)` → 无 `v` 前缀则加。
- `splitLocalTarget(target)`：按 `#` 拆 fragment；在 beforeHash 中按 `?` 拆 query。
- `normalizePathPart`：`\` → `/`。
- `resolveRepositoryPath(targetPath)`：POSIX normalize；绝对路径去前导 `/`，否则 join 基路径；结果 `"."`/`".."`/`"../"` 前缀 → undefined。
- `isDirectoryTarget(originalPath, repoPath)`：原始以 `/` 结尾 → true；否则 repoPath 的 basename 不含 `.` → true（视为目录）。
- `normalizeChangelogLinkTarget(target, tag)`：
  1. 旧域名重写为 `https://github.com/earendil-works/kiana`。
  2. 对 `blob`/`tree` × `main`/`master` 的浮动引用前缀替换为 `.../<route>/<tag>/...`。
  3. `#`、`//`、或带 scheme → 原样返回。
  4. 拆 fragment/query；无 path → 原样。
  5. `resolveRepositoryPath` 失败 → 原样。
  6. 返回 `https://github.com/earendil-works/kiana/${route}/${tag}/${encodeURI(repoPath)}${query}${fragment}`。
- `export function normalizeChangelogLinks(markdown, version|entry): string`：对每个行内链接应用上述。
- `export function parseChangelog(changelogPath): ChangelogEntry[]`：不存在 → `[]`；按 `\n` 分行；`## ` 开头的行结束上一段（若当前版本存在且内容非空 push）；用 `/##\s+\[?(\d+)\.(\d+)\.(\d+)\]?/` 解析版本（失败则重置）；其它行在解析到版本后收集；末段 push。成功返回；异常 `console.error(`Warning: Could not parse changelog: ${error}`)` 返回 `[]`。
- `export function compareVersions(v1,v2)`：major/minor/patch 依次相减。
- `export function getNewEntries(entries, lastVersion)`：`lastVersion.split(".").map(Number)`，缺省 0；返回 `compareVersions(entry,last) > 0`。
- 末尾重导出 `getChangelogPath`。

#### 2.21.35 `syntax-highlight.ts`（219 行）

- 急切注册 21 种语言（顺序）：`python, java, go, javascript, json, cpp, typescript, php, ruby, c, csharp, nix, bash, rust, scala, kotlin, swift, dart, groovy, perl, lua`（测试描述写作 “twenty most common languages”，实际导入 21 个模块）。
- `export function loadAllHighlightLanguages(): Promise<void>`：`setImmediate` 里 `import("highlight.js/lib/index.js")`；失败也 resolve（急切语言与 plaintext 回退仍可用）。
- 类型：`HighlightFormatter = (text)=>string`；`HighlightTheme = Partial<Record<string, HighlightFormatter>>`；`HighlightOptions {language?; ignoreIllegals?; languageSubset?; theme?}`。
- `SPAN_CLOSE = "</span>"`；`HIGHLIGHT_CLASS_PREFIX = "hljs-"`。
- `getScopeFromSpanTag(tag)`：正则 `/\sclass\s*=\s*(?:"([^"]*)"|'([^']*)')/`；class 按空白拆分；首个以 `hljs-` 开头的类去掉前缀作为 scope；否则 undefined。
- `getScopeFormatter(scope, theme)`：精确匹配优先；否则 `scope.indexOf(".")` 前的前缀；再否则 `scope.indexOf("-")` 前的前缀；否则 undefined。
- `getActiveFormatter(scopes, theme)`：从栈顶往下第一个有 formatter 的 scope；都没有 → `theme.default`。
- `isSpanOpenTagStart(html, index)`：`<span` 且下一字符为 `>`/空格/`\t`/`\n`/`\r`。
- `export function renderHighlightedHtml(html, theme = {})`：
  - 逐字符扫描；遇到 span 开标签 → flush 文本、解析 scope 压栈、跳过标签；遇到 `</span>` → flush、弹栈；遇到 `&` → `decodeHtmlEntityAt` 命中则追加解码文本并跳过；否则追加字符。
  - `flushText`：按当前 formatter 处理，**按 `\n` 分段，非空行才调用 formatter**（避免装饰空行）。
- `export function highlight(code, options = {})`：有 `language` → `hljs.highlight(code,{language,ignoreIllegals}).value`；否则 `hljs.highlightAuto(code, languageSubset).value`；再 `renderHighlightedHtml`。
- `export function supportsLanguage(name)`：`hljs.getLanguage(name) !== undefined`。
- Rust：`syntect` 的 scope 命名与 highlight.js **不同**，不能直接替换；应移植 highlight.js 语法（或用 `tree-sitter` 自建映射），并复刻此 HTML→ANSI 的转换器。见 §8。

#### 2.21.36 `git.ts`（226 行）

- `export type GitSource = {type:"git"; repo; host; path; ref?; pinned}`。
- `splitRef(url)`：scp-like `^git@([^:]+):(.+)$`：path 中首个 `@` 分割；两侧都非空才拆，repo 保留 `git@host:repoPath`。含 `://`：`new URL(url)`；pathname 去前导 `/`；首个 `@` 拆分；`parsed.pathname = "/"+repoPath`；repo 为 `parsed.toString()` 去尾部 `/`。无 scheme：`host/path...`；path 中首个 `@` 拆分；repo=`host/repoPath`。
- `decodeForValidation(value)`：`decodeURIComponent` 失败 → null。
- `hasUnsafeGitInstallPart(value, allowSlash)`：decode 失败 true；对 `[value, decoded]`：含 `\0`、含 `\\`、以 `/` 开头 → true；`!allowSlash` 且含 `/` → true；按 `/` 分割含 `..` → true。
- `buildGitSource({repo,host,path,ref})`：path 以 `/` 开头 → null；`normalizedPath = path.replace(/\.git$/,"").replace(/^\/+/,"")`；host 空或路径段数 <2 → null；host 或 path 不安全 → null；返回 `{type:"git", repo, host, path, ref, pinned: Boolean(ref)}`。
- `parseGenericGitUrl(url)`：`splitRef`；scp-like → host/path；`https|http|ssh|git://` → URL 解析 hostname/pathname；否则 `host/path` 且 host 必须含 `.` 或等于 `localhost`（否则 null），repo 改为 `https://<orig>`；最后 `buildGitSource`。
- `export function parseGitUrl(source): GitSource | null`：
  - `trim()`；有 `git:` 前缀 → 去掉并再 trim，接受所有历史简写；无前缀则必须匹配 `/^(https?|ssh|git):\/\//i`，否则 null。
  - `split = splitRef(url)`；hosted 候选：`split.ref ? `${split.repo}#${split.ref}` : undefined` 与 `url`；用 `hosted-git-info` 的 `fromUrl`；命中时若 `split.ref && info.project?.includes("@")` 跳过；`useHttpsPrefix` 判定：repo 不以 `http://`、`https://`、`ssh://`、`git://`、`git@` 开头；返回 `buildGitSource({repo: useHttpsPrefix ? `https://${split.repo}` : split.repo, host: info.domain||"", path: `${info.user}/${info.project}`, ref: info.committable||split.ref||undefined})`。
  - 再试 `https://` 前缀的候选。
  - 最后 `parseGenericGitUrl(url)`。
- Rust：`hosted-git-info` 需复刻其支持的 host 与 URL 归一化规则（github/gitlab/bitbucket/gist 等）；若只服务于包管理，建议以显式 host 表实现，并用测试用例（`test/git-ssh-url.test.ts`）锁定。

#### 2.21.37 `highlight-js.d.ts`（36 行）

`declare module "highlight.js/lib/core.js"`、`"highlight.js/lib/index.js"` 与每个 `highlight.js/lib/languages/<name>.js` 的最小类型声明（`registerLanguage`、`highlight`、`highlightAuto`、`getLanguage` 的签名）。**无运行期行为**，Rust 侧对应 `syntax_highlight` 模块的类型定义。


### 2.22 范围内文档与构建脚本（只读参考，用于校验行为）

- `packages/coding-agent/package.json`：
  - `name = "@earendil-works/kiana-coding-agent"`、`version = "1.4.0"`、`type = "module"`、`kianaConfig = {configDir: ".kiana"}`（**无 `kianaConfig.name`**，因此 `APP_NAME = "kiana"`、`APP_TITLE = "π"`）。
  - `bin = {"kiana": "dist/bundle/cli.js"}`；`exports["."]` 与 `exports["./rpc-entry"]`。
  - `dependencies` 见 §6.1；`engines.node >= 22.19.0`。
- `docs/cli.md`：命令行参考（与 §3 一致）。额外要点：`--mode text` 不会强制一次性执行；`@path` 相对 cwd 解析；`--tools` 仅含 `+name`/`-name` 时改变默认选择而非 allowlist；`--list-models [search]` 可用模糊搜索；`auth check` 退出码 0/1/2；`kiana mcp` 子命令（`add`/`remove`/`list`/`login`/`logout`，`--env`、`--cwd`、`--url`、`--header`、`--bearer-token-env-var`、`--oauth-*`、`--exposure`、`--description`、`--local`、`--json`、`--timeout`）。
- `docs/usage.md`：交互入门（首条提示、`@file`、`-p`、`--continue`、`--resume`、`--name`、`--model`、`--thinking`、只读工具集示例）。
- `docs/environment-variables.md`：进程标记（`AI_AGENT=kiana`、`KIANA_CODING_AGENT=true`）、shell 工具会话变量（`KIANA_SESSION_ID`、`KIANA_SESSION_FILE`、`KIANA_PROVIDER`、`KIANA_MODEL`、`KIANA_REASONING_LEVEL`，**不由 ca-cli 注入**，属 bash/powershell 工具）、全部 `KIANA_*` 配置变量。
- `docs/configuration.md`：agent 目录与项目 `.kiana` 布局（§4.1 已归纳）；`sessionDir` 是唯一在信任解析前读取的项目设置（对应 `main` 第 21 步）。
- `docs/settings.md`：设置项参考（类型/默认值）。与 ca-cli 直接相关的行：`defaultProvider`/`defaultModel`/`defaultThinkingLevel`（默认 `"medium"`，7 值联合）、`modelThinkingLevels`、`enabledModels`（与 `--models` 同格式）、`defaultProjectTrust`（`"ask"|"always"|"never"`，默认 `"ask"`，**只能在 agent 目录设置里生效**）、`defaultTools`（默认 `read,bash,edit,write`；纯名替换、`+name`/`-name` 增删；空数组禁用全部内置工具）、`sessionDir`（相对路径按 cwd 解析，被 `KIANA_CODING_AGENT_SESSION_DIR` 与 `--session-dir` 覆盖）、`packages`。文档明确 `--tools` 纯名会覆盖 `defaultTools`，而只含 `+name`/`-name` 时改的是解析后的选择——与 §2.5.2 的 `--tools` 语义一致，Rust 实现不得简化成"始终覆盖"。
- `docs/cli-integration.md`：Interactive/Print/JSON/RPC 四模式与 stdout/stderr/退出码约定（§4.8）。
- `kiana-test.sh`（85 行）：`--no-env` 时 unset **61 个** provider/云凭据变量（脚本第 19–79 行，覆盖 Anthropic/OpenAI/Gemini/Groq/Cerebras/xAI/OpenRouter/Z.ai/Mistral/MiniMax/OpenCode/Copilot/HF/GCP/AWS/Azure/DeepSeek/Fireworks/Kimi/Meta/Moonshot/NVIDIA/Qwen/Radius/Together/TypeSafe/Xiaomi/Zai 等），`--no-env` 本身从 `ARGS` 中剔除；随后 `RESOLVER_URL="$(node -p 'require("node:url").pathToFileURL(process.argv[1]).href' .../source-resolver.ts)"`（注释说明必须传 file URL，裸路径在含 `#`/`?`/`%` 时会失效），最后 `node --import "$RESOLVER_URL" packages/coding-agent/src/cli.ts "${ARGS[@]}"`。Rust 侧不需要复刻脚本，但它定义了"源码直跑"的入口等价物（且 `--no-env` 是脚本自身参数，**不会**传给 CLI）。

---

## 3. 用户可见面（逐字）

### 3.1 进程级可见行为

| 项目 | 值 |
|---|---|
| 进程标题 `kiana` | `kiana`（`setupCli`）；RPC 入口为 `kiana-rpc` |
| 环境标记 | `KIANA_CODING_AGENT=true`、`AI_AGENT=kiana` |
| 警告 | CLI/RPC 入口静默所有 `process.emitWarning` |
| 版本输出 | `console.log(VERSION)` → 单行 `1.4.0` + 换行，exit 0 |
| 帮助 | §2.5.3 全文，exit 0（带扩展 flags 时附加 `Extension CLI Flags:` 段） |

### 3.2 退出码总表

| 情形 | 退出码 |
|---|---|
| 正常交互退出 / `--help` / `--version` / `--list-models` / `--export` 成功 | 0 |
| `parseArgs` 诊断含 error | 1 |
| `--fork` 与 `--session/--continue/--resume/--no-session` 冲突 | 1 |
| `--session-id` 冲突或非法 ID | 1 |
| `--name` 空值 | 1 |
| `--mode rpc` 且带 `@file` | 1 |
| `MissingSessionCwdError`（非交互） | 1 |
| 运行时诊断含 error | 1 |
| 非交互模式无可用模型 | 1 |
| `KIANA_STARTUP_BENCHMARK` 用于非交互模式 | 1 |
| `auth check`：ready / not_ready / invalid | 0 / 1 / 2 |
| `auth` 参数解析错误 | check → 2；打印类 → 1 |
| package/config 命令参数错误 | 1 |
| `runPrintMode` 返回非 0 | 同为该值 |
| 无会话选择（`--resume` 取消） | 0 |
| `--session` 跨项目且用户拒绝 fork | 0（打印 `Aborted.`） |
| `@file` 不存在/不可读 | 1（`process.exit(1)` 于 file-processor） |

### 3.3 关键用户可见字符串（全部【逐字】）

**参数解析诊断**

```
--mode requires text, json, or rpc
Invalid mode "${mode}". Valid values: text, json, rpc
--name requires a value
Invalid thinking level "${level}". Valid values: off, minimal, low, medium, high, xhigh, max
--use-theme requires a theme name
--tui-mode requires regular or fullscreen
Invalid TUI mode "${mode}". Valid values: regular, fullscreen
Unknown option: ${arg}
${arg}: ${toolListError}         // --tools 校验失败（arg 为 "--tools" 或 "-t"）
                                   toolListError 可能值：
                                   "tool names cannot be mixed with +name or -name entries"
                                   "+name and -name entries take exact tool names, not patterns: ${pattern}"
```

诊断输出格式（main）：
```
Error: ${message}      // chalk.red
Warning: ${message}    // chalk.yellow
```

**会话**

```
Error: --fork cannot be combined with ${flags.join(", ")}
Error: --session-id cannot be combined with ${flags.join(", ")}
Session already exists with id '${sessionId}'
No session found matching '${arg}'
Session found in different project: ${cwd}
Fork this session into current directory? [y/N] 
Aborted.
No session selected
Warning: No project session found with id '${sessionId}'; creating a new session with that id.
Error: --name requires a non-empty value
cwd from session file does not exist\n${sessionCwd}\n\ncontinue in current cwd\n${fallbackCwd}
Stored session working directory does not exist: ${sessionCwd}\nSession file: ${file}\nCurrent working directory: ${fallbackCwd}
```

**运行时**

```
Error: @file arguments are not supported in RPC mode
Error: KIANA_STARTUP_BENCHMARK only supports interactive mode
Hint: Start without extensions using "kiana -ne".
No models available. Use /login to log into a provider via OAuth or API key. See:
  <docs>/providers.md
  <docs>/models.md
```
（最后一条由 `formatNoModelsAvailableMessage()` 生成，`getDocsPath()` 为 `<packageDir>/docs`。）

**迁移与弃用**

```
Migrated Global commands/ → prompts/         // 绿
Migrated Project commands/ → prompts/        // 绿
Warning: Could not migrate ${label} commands/ to prompts/: ${message}   // 黄
Migrated managed binaries tools/ → bin/      // 绿
Warning: ${warning}                          // 黄，逐条
\nMove your extensions to the extensions/ directory.     // 黄
Migration guide: https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration
Documentation: https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/docs/extensions.md
\nPress any key to continue...                // dim
（等待任意键后输出一个空行）
```
弃用警告原文：
```
Global hooks/ directory found. Hooks have been renamed to extensions.
Project hooks/ directory found. Hooks have been renamed to extensions.
Global tools/ directory contains custom tools. Custom tools have been merged into extensions.
Project tools/ directory contains custom tools. Custom tools have been merged into extensions.
```

**包命令**

```
Unknown option ${opt} for "${command}".
Use "kiana --help" or "${usage}".
Missing value for ${opt}.
Usage: ${usage}
Unexpected argument ${arg}.
Usage: ${usage}
${conflictMessage}
Usage: ${usage}
Missing ${command} source.
No matching package found for ${source}
Installed ${source}
Removed ${source}
No packages installed.
User packages:
Project packages:
  ${source}${filtered ? " (filtered)" : ""}
    ${installedPath}
Extensions are skipped. Run kiana update --extensions to update extensions.
Updated ${source}
Updated packages
Managed kiana installations do not support --force; rerun the installer to repair this installation.
kiana is already up to date (v${VERSION})
Updating kiana with ${display}...
Updating managed kiana installation...
Updated kiana from ${VERSION} to ${version}
kiana self-update on Windows is only supported for npm and pnpm installs.
Detected install method: ${method}. Update kiana manually.
Model catalogs refreshed
Could not refresh model catalogs: ${details}
Model catalog refresh timed out.
error: kiana cannot self-update this installation.
${unavailableInstruction}
Location of kiana executable: ${process.argv[1]}
If this keeps failing, run this command yourself: ${display}
If pnpm reports missing package versions, its cached registry metadata may be stale.
Run `pnpm store prune` and retry `kiana update --self`.
Update note
This npm installation of kiana does not pin its dependencies.
Run the installer to migrate to a managed installation that does:
  curl -fsSL https://kiana.dev/install.sh | sh
  powershell -c "irm https://kiana.dev/install.ps1 | iex"
Warning (${context}, ${scope} settings): ${error.message}
Project is not trusted. Use --approve to modify local resource config.
Project is not trusted. Use --approve to modify local package config.
Unknown option ${arg} for "config".
Use "kiana --help" or "kiana config [-l] [--approve|--no-approve]".
Unexpected argument ${arg}.
Usage: kiana config [-l] [--approve|--no-approve]
Error: ${message}
```

冲突消息全集：
```
--extension can only be provided once
--all cannot be combined with --self, --extensions, --models, or --extension
--all cannot be combined with a positional source
--models cannot be combined with --self, --extensions, --all, or --extension
--models cannot be combined with a positional source
--extension cannot be combined with --self, --extensions, or --all
--extension cannot be combined with a positional source
positional update targets cannot be combined with --self, --extensions, or --all
```

**auth**

```
Error: ${message}
Unknown option --${option} for "${name}".
Use "kiana --help" or "${usage}".
Unknown auth command "${value}". Use "kiana auth print-api-key", "kiana auth print-bearer-token", or "kiana auth check".
--min-expiry is only supported by print-bearer-token
--min-expiry must use a duration such as 30m or 1h
${arg} is only supported by auth check
Auth commands only accept --provider and --model
Auth checks require --provider <provider> or --model <model>
Credential printing requires --provider <provider> or --model <model>
Unable to resolve model "${cliModel}"
Unable to resolve an auth provider
Unknown provider "${provider}". Use --list-models to see available providers.
Unable to resolve the requested provider/model
Model "${model}" not found. Use --list-models to see available models.
Provider "${id}" is configured with OAuth, not an API key
Provider "${id}" is not configured with an OAuth bearer token
No usable API key is configured
No usable OAuth bearer token is configured
Multiple configured providers matched (${ids.join(", ")}). Specify --provider.
Failed to parse auth command
Failed to resolve credential
```
`auth check` 的纯文本输出就是 `ready` / `not_ready` / `invalid`（或 `--credentials` 时的凭据）加换行；JSON 为 `{"status":...,"provider":...,"reason"?:...,"authType"?:...,"credentials"?:"..."}`（键顺序与插入顺序一致）。

**`--list-models` 输出**

```
Warning: errors loading models.json:
${loadError}
No models matching "${searchPattern}"
```
表格：表头 `provider`, `model`, `context`, `max-out`, `thinking`, `images`；列宽 = max(表头长, 所有单元格长)；列间两个空格；无尾随空格（最后一列 padEnd 到其宽度，等价于原样）。`formatTokenCount`：`1000000`→`1M`、`1500000`→`1.5M`、`200000`→`200K`、`1500`→`1.5K`、`999`→`999`。

**`@file` 与图片**

```
Error: File not found: ${absolutePath}
Error: Could not read file ${absolutePath}: ${message}
<file name="${absolutePath}">
${content}
</file>
<file name="${absolutePath}"></file>
<file name="${absolutePath}">${hints.join("\n")}</file>
[Image omitted: could not be converted to a supported inline image format.]
[Image omitted: could not be resized below the inline image size limit.]
[Image converted from ${from} to ${to}.]
[Image: original ${ow}x${oh}, displayed at ${w}x${h}. Multiply coordinates by ${scale.toFixed(2)} to map to original image.]
```

**剪贴板**

```
Clipboard unavailable
Clipboard unavailable: text exceeds the OSC 52 size limit
Clipboard unavailable: install the Termux:API app and `termux-api` package
Clipboard unavailable: install `wl-clipboard` (`wl-copy`) or check Wayland access
Clipboard unavailable: install `xclip` or `xsel`, or check X11 access
```

**工具下载**

```
${name} not found. Offline mode enabled, skipping download.
${name} not found. Install with: pkg install ${pkg}
${name} not found. Automatic download is not supported on ${platform}/${arch}. Install ${name} with your system package manager.
${name} not found. Downloading...
${name} installed to ${path}
Failed to download ${name}: ${messages}
Failed to resolve latest ${repo} release: HTTP ${status} without redirect
Failed to resolve latest ${repo} release: unexpected redirect to ${location}
Download failed with HTTP ${status}: ${url}
No response body
Failed to extract ${assetName}: ${failure}
Unsupported archive format: ${assetName}
Binary not found in archive: expected ${binaryFileName} under ${extractDir}
Unsupported platform: ${platform}/${arch}
Unknown tool: ${tool}
```

**版本检查 / 自更新**

```
Could not determine latest kiana version: ${formatVersionCheckError(error)}
Could not determine latest kiana version.
Invalid managed release version: ${version}
Another managed Kiana update is already running.
Managed install marker is missing or invalid: ${markerPath}
Could not download managed installer ${label} from ${url}: HTTP ${status}
npm ${args.join(" ")} exited with code ${code}
Could not verify managed Kiana ${version}: ${reason}
Managed Kiana smoke test returned version ${installed}: expected ${expected}.
${step.display} terminated by signal ${signal}
${step.display} exited with code ${code}
```

**shell**

```
Custom shell path not found: ${customShellPath}
No bash shell found. Options:
  1. Install Git for Windows: https://git-scm.com/download/win
  2. Add your bash to PATH (Cygwin, MSYS2, etc.)
  3. Set shellPath in settings.json

Searched Git Bash in:
  ${path1}
  ${path2}
The powershell tool is only available on Windows.
No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.
```

### 3.4 ANSI / 渲染细节

- 全部颜色来自 chalk v6：`chalk.red`/`yellow`/`green`/`cyan`/`dim`/`bold`/`italic`/`strikethrough`/`underline`。
- chalk 的启用条件：非 TTY 或被 `NO_COLOR`（非空）/`FORCE_COLOR` 控制时按 chalk 规则降级。**Rust 实现必须复刻 chalk v6 的 level 判定**（`FORCE_COLOR` 0/1/2/3、`NO_COLOR` 存在即禁用、`TERM=dumb`、`CI`、TTY 检测），否则 `--help` 在管道下的字节不同（测试 `stdout-cleanliness.test.ts` 关注 stdout 内容而不含颜色，但其它场景会比对）。
- `reportDiagnostics` 的 `dim` 不带前缀。
- `SelfUpdateNote` 用 `Markdown(..., width = max(20, columns ?? 80))` 渲染，行尾 `trimEnd()`；Markdown 渲染器归 tui-components 单元，ca-cli 只需调用。
- OSC 52 序列：`\x1b]52;c;<base64>\x07`（BEL 结尾，不是 ST）。
- `printTimings()` 输出（仅 `KIANA_TIMING=1`，走 stderr）：
```

--- Startup Timings: main ---
  parseArgs: ${ms}ms
  ...
  TOTAL: ${sum}ms
-------------------------
```
分隔线长度 = `title.length + 8` 个 `-`，`title` 为 `Startup Timings: <namespace>`（`main` 时为 20 + 8 = 28 个连字符；上面示例里的连字符数量仅为示意，实现须按公式生成）。实际 label 序列（按 main 中的 `time()` 调用）：`parseArgs`, `runMigrations`, `firstTimeSetup`, `createSessionManager`, `createRuntime`, `createAgentSessionRuntime`, `readPipedStdin`, `prepareInitialMessage`, `initTheme`, `resolveModelScope`, `createAgentSession`, `interactiveMode.init`（仅 benchmark）。若某段未执行则不出现。

### 3.5 启动基准（`KIANA_STARTUP_BENCHMARK=1`）

仅交互模式：`interactiveMode.init()` → `time("interactiveMode.init")` → 等待 150ms（让 TUI 的 stdin handler 消费 Kitty 键盘协议、DA、cell size 应答）→ `stop()` → `stopThemeWatcher()` → `printTimings()` → stdout/stderr 若有待写数据则等待 `drain` → 返回（不进入事件循环）。

### 3.6 帮助中的扩展段格式

```
\nExtension CLI Flags:
  --${name}${string ? " <value>" : ""}     ${description}
```
（`  --${name}${value}` 用 `padEnd(30)` 补空格；`description` 缺失时 `Registered by ${extensionPath}`；整段前后各有一个换行。）

---

## 4. 外部格式（逐字）

### 4.1 磁盘路径

| 路径 | 用途 |
|---|---|
| `~/.kiana/agent/`（或 `KIANA_CODING_AGENT_DIR`） | agent 目录 |
| `<agent>/settings.json` | 用户设置 |
| `<agent>/auth.json` | 凭据（mode 0600） |
| `<agent>/oauth.json` | 旧凭据；迁移后 `<agent>/oauth.json.migrated` |
| `<agent>/models.json` | 自定义模型/覆盖 |
| `<agent>/keybindings.json` | 键位（迁移会重写，末尾补 `\n`，2 空格缩进） |
| `<agent>/mcp.json` | MCP 服务器 |
| `<agent>/themes/` | 用户主题 |
| `<agent>/extensions/`, `skills/`, `prompts/` | 资源 |
| `<agent>/tools/` | 旧二进制目录（迁移到 `bin/`） |
| `<agent>/bin/` | fd/rg 安装目录 |
| `<agent>/sessions/--<encoded-cwd>--/<id>.jsonl` | 会话 |
| `<agent>/kiana-debug.log` | 调试日志 |
| `<cwd>/.kiana/settings.json`、`.kiana/mcp.json`、`.kiana/extensions|skills|prompts|themes/` | 项目资源（需信任） |
| `<tmp>/<prefix>-<16 hex><ext>` | 输出文件（0600，`wx`） |
| `<tmp>/kiana-wsl-clip-<uuid>.txt|.png` | WSL 剪贴板临时文件 |
| `<managedRoot>/{current-version, managed-install.json, releases/<ver>/, staging/update-*/}` | managed 安装布局 |

会话目录编码：`--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`。

### 4.2 `managed-install.json`

```json
{ "kind": "kiana-managed-install", "schemaVersion": 1, "layout": "releases-v1" }
```
三个字段必须全部匹配（`kind` 字符串相等、`schemaVersion` 数字 1、`layout` 字符串 `releases-v1`）；文件可带 UTF-8 BOM（`stripBom`）。不合法 → 抛错（见 §3.3）。

### 4.3 `auth.json` 迁移格式

oauth 来源：
```json
{ "<provider>": { "type": "oauth", ...原 oauth.json 字段 } }
```
settings.apiKeys 来源：
```json
{ "<provider>": { "type": "api_key", "key": "<string>" } }
```
同 provider 时 oauth 优先（`!migrated[provider]` 才写 api_key）。整个对象 `JSON.stringify(obj, null, 2)`，写文件 mode `0o600`。迁移后 `settings.json` 删除 `apiKeys` 字段并用 `JSON.stringify(settings,null,2)` 重写（**注意：无尾随换行**）。

### 4.4 会话迁移

读 `<agent>/*.jsonl` 第一行；要求 `{"type":"session","cwd":"<abs>"...}`；移动到 `<agent>/sessions/--<encoded>--/<原文件名>`；目标存在则跳过。不修改文件内容。

### 4.5 环境变量

| 变量 | 读取处 | 语义 |
|---|---|---|
| `KIANA_CODING_AGENT_DIR` | `config.getAgentDir` | agent 目录；`expandTildePath`（= `normalizePath`） |
| `KIANA_CODING_AGENT_SESSION_DIR` | `main` sessionDir | 会话目录（次优先于 `--session-dir`） |
| `KIANA_PACKAGE_DIR` | `config.getPackageDir` | 包资源根 |
| `KIANA_OFFLINE` | `main`, `version-check`, `tools-manager` | **读取处语义不同，必须分别实现**：`main`（第 3 步）与 `tools-manager.isOfflineModeEnabled` 用真值判定 `1`/`true`/`yes`（大小写不敏感）；但 `version-check.getLatestKianaRelease` 是 `if (process.env.KIANA_OFFLINE) return undefined;`——**任意非空值**（含 `"0"`、`"false"`、`"no"`）都禁用版本检查。为真时 main 会设置 `KIANA_OFFLINE=1` 与 `KIANA_SKIP_VERSION_CHECK=1` |
| `KIANA_SKIP_VERSION_CHECK` | `version-check.checkForNewKianaVersion` | 任意非空即跳过 |
| `KIANA_TIMING` | `core/timings`（跨单元） | 必须严格 `=== "1"` 才启用 |
| `KIANA_STARTUP_BENCHMARK` | `main` | 真值判定；仅交互模式 |
| `KIANA_SHARE_VIEWER_URL` | `config.getShareViewerUrl` | 默认 `https://kiana.dev/session/` |
| `KIANA_MANAGED_INSTALL_ROOT` | `package-manager-cli` | trim 后作为 managed 根 |
| `KIANA_INSTALLER_API_BASE` | `package-manager-cli` | trim 后去尾部 `/`；默认 `https://kiana.dev/api/installer/releases` |
| `KIANA_EXPERIMENTAL` | `core/experimental`（跨单元） | 必须 `=== "1"` |
| `KIANA_CLIPBOARD_OSC52` | `clipboard.isRemoteSession` | 必须 `=== "1"` |
| `SSH_CONNECTION`/`SSH_CLIENT`/`MOSH_CONNECTION` | `clipboard` | 任一存在即远程 |
| `WT_SESSION` | `clipboard` | Windows Terminal 标记 |
| `WSL_DISTRO_NAME`/`WSLENV` | `wsl.isWSL` | WSL 标记 |
| `TERMUX_VERSION` | `clipboard`, `clipboard-image`, `tools-manager` 间接 | Termux 标记 |
| `WAYLAND_DISPLAY` / `DISPLAY` / `XDG_SESSION_TYPE` | 剪贴板 | 后端选择 |
| `PNPM_HOME` | `config` pnpm 自更新 | `--config.global-bin-dir` |
| `SystemRoot` / `ProgramFiles` / `ProgramFiles(x86)` | `shell`, `tools-manager`, `windows-self-update` | Windows 路径 |
| `VISUAL`/`EDITOR`、`HTTP_PROXY`/`HTTPS_PROXY` | 其它单元 | 仅文档 |
| `KIANA_TELEMETRY`, `KIANA_CODEMODE_MAX_WORKERS`, `KIANA_CACHE_RETENTION`, `KIANA_RADIUS_GATEWAY`, `KIANA_CLEAR_ON_SHRINK`, `KIANA_HARDWARE_CURSOR`, `KIANA_HYPERLINKS`, `KIANA_PROGRAM_STATUS`, `KIANA_IMAGE_PROTOCOL`, `KIANA_TRUE_COLOR`, `KIANA_TUI_ESC_TIMEOUT` | tui/ai/sdk 单元 | ca-cli 仅帮助文本列出部分 |

> **Rust 实现差异（品牌隔离）**：Kiana 自建服务没有默认上游。`KIANA_API_BASE_URL` 是模型目录、
> 版本检查、安装遥测与 MCP CIMD 的统一根地址；未配置时这些功能跳过或明确报不可用。
> `KIANA_INSTALLER_API_BASE` 缺省时由 `KIANA_API_BASE_URL/api/installer/releases` 派生；
> `KIANA_SHARE_VIEWER_URL` 缺省时由 `KIANA_API_BASE_URL/session/` 派生。下表保留 TS 源行为描述。

### 4.6 HTTP 端点

| 用途 | 方法与 URL | 请求头 | 响应处理 |
|---|---|---|---|
| 最新版本 | `GET https://kiana.dev/api/latest-version` | `User-Agent: kiana/<ver> (<platform>; node/<process.version>; <arch>)`，`accept: application/json` | `!ok` → undefined；JSON `{version, packageName?, note?}`；`version` 必须非空字符串 |
| GitHub 最新 release | `GET https://github.com/<repo>/releases/latest` | 同上；`redirect: "manual"` | 必须 3xx 且 `location` 含 `/releases/tag/`；取最后一段，`decodeURIComponent`，去 `v` 前缀 |
| 工具下载 | `GET https://github.com/<repo>/releases/download/<tag>/<asset>` | 同上 | 流式写文件 |
| managed 安装包 | `GET <installerApiBase>/<encodeURIComponent(version)>/package.json` 与 `/package-lock.json` | 同上 | `text()` |
| 安装器（提示用） | `https://kiana.dev/install.sh`、`https://kiana.dev/install.ps1` | — | 仅打印命令 |

`fetchWithRetry` 默认 2 次重试，状态码集合 `{408,425,429,500,502,503,504}`；网络错误按 §2.21.16 规则。

### 4.7 ZIP 输出格式

经典 ZIP（无 Zip64）：local header 30B + name + raw-deflate 数据；central header 46B + name；EOCD 22B。flags `0x0800`（UTF-8 名，不设 data descriptor），method 8，时间来自本地当前时间，`version needed = 20`，`version made by = 20`。CRC32 覆盖原始数据。

### 4.8 RPC / JSON 模式

ca-cli 不定义协议，只负责：
- `--mode rpc` 时拒绝 `@file`；不读 stdin 作为提示；`printTimings()` 后调用 `runRpcMode(runtime)`（永不返回，直到 shutdown）。
- 在 `KIANA_OFFLINE` 未启用时后台调用 `modelRuntime.refresh({signal})`，15s 超时，错误吞掉。
- `--mode json` 时 `toPrintOutputMode` 返回 `"json"`，交给 `runPrintMode`；stdout 已被 `takeOverStdout()` 接管，正常 `console.log` 全部转到 stderr，只有 `writeRawStdout` 写协议。
- `isPlainRuntimeMetadataCommand` 为真（`--help` 或 `--list-models` 且无 `--print`、无 `--mode`）时**不**接管 stdout，因此帮助/模型表格直接进 stdout。

---

> 说明：本目录的其它单元规范（`ai-*`、`tui-*`、`agent-codemode`、`telemetry-mcp`、`chord-*`、`durable-*`、`env-evals-durable-testing`）
> 已先行写出；`core/session-manager`、`core/settings-manager`、`core/package-manager`、`core/resource-loader`、`core/model-runtime`、
> `core/export-html`、`core/extensions`、`modes/*`、`extensions/mcp` 属于 coding-agent 内部其它单元，其规范若尚未写出，
> 下游实现必须以下面的签名为准（这些签名是从源码逐字提取的**契约面**，不是建议）。

## 5. 跨单元接口（ca-cli 消费 / 暴露）

### 5.1 ca-cli 暴露给其它单元

```rust
// 常量
pub const PACKAGE_NAME: &str;      // "@earendil-works/kiana-coding-agent"
pub const APP_NAME: &str;          // "kiana"
pub const APP_TITLE: &str;         // "π"
pub const CONFIG_DIR_NAME: &str;   // ".kiana"
pub const VERSION: &str;           // "1.4.0"
pub const ENV_AGENT_DIR: &str;     // "KIANA_CODING_AGENT_DIR"
pub const ENV_SESSION_DIR: &str;   // "KIANA_CODING_AGENT_SESSION_DIR"

// 路径
pub fn get_agent_dir() -> PathBuf;
pub fn get_package_dir() -> PathBuf;
pub fn get_themes_dir() -> PathBuf;
pub fn get_export_template_dir() -> PathBuf;
pub fn get_package_json_path() -> PathBuf;
pub fn get_readme_path() -> PathBuf;
pub fn get_docs_path() -> PathBuf;
pub fn get_examples_path() -> PathBuf;
pub fn get_changelog_path() -> PathBuf;
pub fn get_interactive_assets_dir() -> PathBuf;
pub fn get_bundled_interactive_asset_path(name: &str) -> PathBuf;
pub fn get_custom_themes_dir() -> PathBuf;
pub fn get_models_path() -> PathBuf;
pub fn get_auth_path() -> PathBuf;
pub fn get_settings_path() -> PathBuf;
pub fn get_tools_dir() -> PathBuf;
pub fn get_bin_dir() -> PathBuf;
pub fn get_prompts_dir() -> PathBuf;
pub fn get_sessions_dir() -> PathBuf;
pub fn get_debug_log_path() -> PathBuf;
pub fn get_share_viewer_url(gist_id: &str) -> String;
pub fn expand_tilde_path(p: &str) -> String;

// 安装/自更新
pub enum InstallMethod { BunBinary, Npm, Pnpm, Yarn, Bun, Unknown }
pub struct SelfUpdateCommandStep { pub command: String, pub args: Vec<String>, pub display: String }
pub struct SelfUpdateCommand { pub command: String, pub args: Vec<String>, pub display: String, pub steps: Option<Vec<SelfUpdateCommandStep>> }
pub enum SelfUpdatePackageTarget { Name(String), Target { package_name: String, install_spec: Option<String> } }
pub fn detect_install_method() -> InstallMethod;
pub fn get_self_update_command(package_name: &str, npm_command: Option<&[String]>, target: SelfUpdatePackageTarget) -> Option<SelfUpdateCommand>;
pub fn get_self_update_unavailable_instruction(package_name: &str, npm_command: Option<&[String]>, target: SelfUpdatePackageTarget) -> String;
pub fn get_update_instruction(package_name: &str) -> String;
pub enum InstallChange { Updated { version: String }, Removed }
pub fn detect_install_change(package_json_path: Option<&Path>) -> Option<InstallChange>;

// 参数
pub enum Mode { Text, Json, Rpc }
pub struct Args { ... }            // §2.5.1 全字段
pub struct Diagnostic { pub kind: DiagnosticKind /* Warning|Error */, pub message: String }
pub fn parse_args(args: &[String]) -> Args;
pub fn print_help(extension_flags: &[ExtensionFlag]);
pub fn is_valid_thinking_level(level: &str) -> bool;
pub fn normalize_session_name(v: &str) -> Option<String>;

// 迁移
pub struct MigrationResult { pub migrated_auth_providers: Vec<String>, pub deprecation_warnings: Vec<String> }
pub fn run_migrations(cwd: &Path) -> MigrationResult;
pub fn show_deprecation_warnings(warnings: &[String]); // async，读一次 stdin

// 入口
pub struct MainOptions { pub extension_factories: Vec<InlineExtension> }
pub async fn main(args: Vec<String>, options: Option<MainOptions>);
pub fn setup_cli();
```

### 5.2 ca-cli 依赖的其它单元（必须存在的签名）

```rust
// core/extensions/types.rs — kiana 单元 ca-extensions
pub struct ExtensionFlag { pub name: String, pub description: Option<String>, pub type_: FlagType /* "string" | "boolean" */, pub extension_path: String }
pub type InlineExtension = ...; // 具名内置扩展或工厂

// core/settings-manager.rs
impl SettingsManager {
  pub fn create(cwd: &Path, agent_dir: &Path, opts: SettingsCreateOptions) -> Self; // opts.project_trusted
  pub fn in_memory(settings: Settings, opts: SettingsCreateOptions) -> Self;
  pub fn get_global_settings(&self) -> &Settings;
  pub fn is_project_trusted(&self) -> bool;
  pub fn set_project_trusted(&mut self, v: bool);
  pub fn apply_overrides(&mut self, overrides: PartialSettings);
  pub async fn flush(&mut self);
  pub fn drain_errors(&mut self) -> Vec<SettingsError>; // {scope, error: Error}
  pub fn get_session_dir(&self) -> Option<PathBuf>;
  pub fn get_default_provider(&self) -> Option<String>;
  pub fn get_default_model(&self) -> Option<String>;
  pub fn get_enabled_models(&self) -> Option<Vec<String>>;
  pub fn get_theme_setting(&self) -> Option<String>;
  pub fn set_theme(&mut self, theme: &str);
  pub fn get_theme(&self) -> String;
  pub fn set_enable_analytics(&mut self, enabled: bool);
  pub fn get_show_hardware_cursor(&self) -> bool;
  pub fn get_clear_on_shrink(&self) -> bool;
  pub fn get_terminal_capability_overrides(&self) -> CapabilityOverrides;
  pub fn get_http_idle_timeout_ms(&self) -> u64;
  pub fn get_default_project_trust(&self) -> DefaultProjectTrust;
  pub fn get_tui_mode(&self) -> TuiMode;
}
pub fn get_tool_list_error(entries: &[String]) -> Option<String>;
pub const DEFAULT_TOOL_NAMES: [&str; 4] = ["read","bash","edit","write"];

// core/session-manager.rs
impl SessionManager {
  pub fn in_memory(cwd: &Path, opts: Option<InMemoryOptions>) -> Self;
  pub fn create(cwd: &Path, session_dir: Option<&Path>, opts: CreateOptions) -> Self;
  pub fn open(path: &Path, session_dir: Option<&Path>) -> Result<Self>;
  pub fn open_with_cwd(path: &Path, session_dir: Option<&Path>, cwd: &Path) -> Result<Self>;
  pub fn continue_recent(cwd: &Path, session_dir: Option<&Path>) -> Self;
  pub fn fork_from(src: &Path, cwd: &Path, session_dir: Option<&Path>, opts: ForkOptions) -> Result<Self>;
  pub fn find_by_id(cwd: &Path, id: &str, session_dir: Option<&Path>) -> Option<PathBuf>;
  pub async fn list(cwd: &Path, session_dir: Option<&Path>, on_progress: Option<ProgressFn>, signal: Option<AbortSignal>) -> Vec<SessionInfo>;
  pub async fn list_all(session_dir: Option<&Path>, on_progress: Option<ProgressFn>, signal: Option<AbortSignal>) -> Vec<SessionInfo>;
  pub fn get_cwd(&self) -> PathBuf;
  pub fn get_session_file(&self) -> Option<PathBuf>;
  pub fn append_session_info(&mut self, name: &str);
  pub fn build_session_context(&self) -> SessionContext; // .messages.len() 被使用
}
pub fn assert_valid_session_id(id: &str) -> Result<()>; // 失败 Error message 被逐字输出

// core/model-resolver.rs
pub struct ScopedModel { pub model: Model, pub thinking_level: Option<ThinkingLevel> }
pub struct ResolveCliModelResult { pub model: Option<Model>, pub thinking_level: Option<ThinkingLevel>, pub warning: Option<String>, pub error: Option<String> }
pub fn resolve_cli_model(opts: ResolveCliModelOptions) -> ResolveCliModelResult;
pub async fn resolve_model_scope(patterns: &[String], rt: &ModelRuntime, opts: AuthOperationOptions) -> Vec<ScopedModel>;
pub fn resolve_model_reference(req: ModelReferenceRequest, models: &[Model], is_usable: impl Fn(&str)->bool) -> ModelReferenceResult;

// core/model-runtime.rs
impl ModelRuntime {
  pub async fn create(opts: CreateModelRuntimeOptions) -> Result<Self>;
  pub fn get_model(&self, provider: &str, id: &str) -> Option<Model>;
  pub fn get_models(&self) -> Vec<Model>;
  pub fn get_providers(&self) -> Vec<Provider>;
  pub fn get_provider(&self, id: &str) -> Option<Provider>;
  pub fn get_error(&self) -> Option<String>;
  pub async fn get_auth(&self, target: AuthTarget, opts: AuthOptions) -> Option<AuthResult>;
  pub async fn check_auth(&self, provider: &str) -> Option<AuthResult>;
  pub async fn list_credentials(&self, opts: {signal}) -> Vec<CredentialInfo>;
  pub async fn set_runtime_api_key(&mut self, provider: &str, key: &str);
  pub async fn refresh(&self, opts: RefreshOptions) -> RefreshResult; // {aborted, errors: Map}
  pub fn has_configured_auth(&self, provider: &str) -> bool;
}

// core/auth-storage.rs
pub struct AuthStorage; impl AuthStorage { pub fn create() -> Self }
pub struct ReadOnlyAuthStorage; impl ReadOnlyAuthStorage { pub fn new() -> Self }

// core/output-guard.rs
pub fn take_over_stdout();
pub fn restore_stdout();
pub fn is_stdout_taken_over() -> bool;
pub fn write_raw_stdout(text: &str);
pub async fn wait_for_raw_stdout_backpressure();
pub async fn flush_raw_stdout();

// core/timings.rs
pub fn reset_timings(namespace: TimingLabel);
pub fn time(label: &str, namespace: TimingLabel);
pub fn print_timings();

// core/settings-diagnostics.rs
pub fn collect_settings_diagnostics(sm: &SettingsManager) -> Vec<AgentSessionRuntimeDiagnostic>;
pub fn deduplicate_diagnostics(d: Vec<AgentSessionRuntimeDiagnostic>) -> Vec<AgentSessionRuntimeDiagnostic>;

// core/project-trust.rs
pub enum AppMode { Interactive, Print, Json, Rpc }
pub async fn resolve_project_trusted(opts: ResolveProjectTrustedOptions) -> bool;

// core/trust-manager.rs
pub struct ProjectTrustStore; impl ProjectTrustStore { pub fn new(agent_dir: &Path) -> Self; pub fn get(&self, cwd: &Path) -> Option<bool> }
pub fn has_trust_requiring_project_resources(cwd: &Path) -> bool;

// core/session-cwd.rs
pub struct SessionCwdIssue { pub session_file: Option<PathBuf>, pub session_cwd: PathBuf, pub fallback_cwd: PathBuf }
pub fn get_missing_session_cwd_issue(sm: &SessionManager, fallback: &Path) -> Option<SessionCwdIssue>;
pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String;
pub struct MissingSessionCwdError; impl MissingSessionCwdError { pub fn new(issue) -> Self } // .message 即 format_missing_session_cwd_error

// core/auth-guidance.rs
pub fn format_no_models_available_message() -> String;

// core/export-html/index.rs
pub async fn export_from_file(input: &Path, output: Option<&Path>) -> Result<String>;

// core/agent-session-services.rs / agent-session-runtime.rs / sdk.rs
pub struct AgentSessionRuntimeDiagnostic { pub kind: DiagnosticKind, pub message: String }
pub struct CreateAgentSessionServicesOptions { cwd, agent_dir, settings_manager, model_runtime_signal, extension_flag_values: Map<String, FlagValue>, resource_loader_reload_options: Option<...>, resource_loader_options: ResourceLoaderOptions }
pub async fn create_agent_session_services(opts) -> AgentSessionServices; // {settings_manager, model_runtime, resource_loader, diagnostics}
pub async fn create_agent_session_from_services(opts) -> Created; // {session, ...}
pub async fn create_agent_session_runtime(factory, opts: {cwd, agent_dir, session_manager, session_start_event?}) -> AgentSessionRuntime; // 先 assert_session_cwd_exists

// modes
pub struct InteractiveModeOptions { ... }  // §5.3
pub struct PrintModeOptions { pub mode: PrintOutputMode /* Text|Json */, pub messages: Option<Vec<String>>, pub initial_message: Option<String>, pub initial_images: Option<Vec<ImageContent>> }
pub async fn run_print_mode(host: &AgentSessionRuntime, opts: PrintModeOptions) -> i32;
pub async fn run_rpc_mode(host: AgentSessionRuntime) -> !; // 不返回

// extensions
pub fn built_in_extensions() -> Vec<InlineExtension>;
pub async fn run_mcp_command(args: &[String], opts: {cwd, agent_dir}) -> i32;

// theme
pub fn init_theme(name: &str, interactive: bool);
pub fn set_theme(name: &str);
pub fn set_theme_json_validator(f: fn(&str) -> Result<()>);
pub fn stop_theme_watcher();
pub fn mark_terminal_colors_pending();
pub fn set_registered_themes(themes: Vec<Theme>);
pub fn load_theme_from_path(path: &Path) -> Result<Theme>;
pub fn resolve_theme_setting(setting: Option<&str>, terminal: Option<&TerminalTheme>) -> Option<String>;
pub fn get_terminal_theme() -> Option<TerminalTheme>;
pub fn set_terminal_colors(colors: TerminalColors);
pub const SYSTEM_THEME_NAME: &str;
pub async fn request_terminal_colors(tui: &mut TUI, cb: impl FnOnce(TerminalColors));

// kiana-tui
pub fn set_capability_overrides(o: CapabilityOverrides);
pub fn set_keybindings(kb: KeybindingsManager);
pub fn get_capabilities() -> Capabilities;
pub fn set_image_transcoder(f: ImageTranscoder);
pub fn get_native_clipboard() -> Option<NativeClipboard>;
pub struct TuiMainScreen; impl TuiMainScreen { pub fn new(terminal: ProcessTerminal, hardware_cursor: bool, agent_dir: &Path) -> Self; ... }
pub struct Markdown; impl Markdown { pub fn new(text, x: usize, y: usize, theme: MarkdownTheme) -> Self; pub fn render(&self, width: usize) -> Vec<String> }
pub fn fuzzy_filter<T>(items: &[T], query: &str, text: impl Fn(&T) -> String) -> Vec<T>;
```

### 5.3 `InteractiveModeOptions`（逐字段）

```
migratedProviders?: string[]
startupDiagnostics?: AgentSessionRuntimeDiagnostic[]
modelFallbackMessage?: string
autoTrustOnReloadCwd?: string
initialMessage?: string
initialImages?: ImageContent[]
initialMessages?: string[]
verbose?: boolean
tuiMode?: TuiMode          // "regular" | "fullscreen"
initialThemeSetting?: string
terminal?: Terminal
```

### 5.4 `ProjectTrustContext`

```rust
pub struct ProjectTrustContext {
  pub cwd: PathBuf,
  pub mode: TrustMode,   // "tui" | "print" | "json" | "rpc"
  pub has_ui: bool,
  pub ui: TrustUi,
}
pub struct TrustUi {
  pub select: Box<dyn Fn(&str, Vec<String>) -> BoxFuture<Option<String>>>,
  pub confirm: Box<dyn Fn(&str, &str) -> BoxFuture<bool>>,
  pub input: Box<dyn Fn(&str, Option<&str>) -> BoxFuture<Option<String>>>,
  pub notify: Box<dyn Fn(&str, NotifyType)>,
}
```
`mode` 映射：`interactive → "tui"`，其它原样（`"print"`/`"json"`/`"rpc"`）。

---

## 6. 依赖与 Rust crate 映射

### 6.1 本单元直接使用的 npm 包

| npm 包 | 用在哪 | 说明 |
|---|---|---|
| `chalk` (6.0.0) | 全部 CLI 输出 | ANSI 样式 + 颜色级别检测 |
| `yaml` (2.9.0) | `utils/frontmatter.ts` | YAML 解析 |
| `semver` (7.8.5) | `utils/version-check.ts` | `valid`/`compare` |
| `proper-lockfile` (4.1.2) | `package-manager-cli.ts` | managed 更新的跨进程锁（ELOCKED） |
| `cross-spawn` (7.0.6) | `utils/child-process.ts` | Windows 下执行 .cmd/.bat |
| `hosted-git-info` (9.0.3) | `utils/git.ts` | 识别 GitHub/GitLab/Bitbucket 等 |
| `highlight.js` (10.7.3) | `utils/syntax-highlight.ts` | 语法高亮（21 种急切语言） |
| `@silvia-odwyer/photon-node` (0.3.4) | `utils/photon.ts` 等 | WASM 图像解码/缩放/编码 |
| `quickjs-wasi` (3.6.2) | `config.getQuickJSWasmPath` | codemode worker 的 wasm 路径（跨单元使用） |
| `@earendil-works/kiana-tui` | 启动 UI、模糊过滤、Markdown | tui-core/tui-components 单元 |
| `@earendil-works/kiana-agent-core` | `src/cli/args.ts` 的 `import type { ThinkingLevel }`；`settings-manager`/`defaults`/`model-resolver` 的 `ThinkingLevel` | **1.4.0**。ca-cli 只用到类型，但 `Args.thinking`、`isValidThinkingLevel` 的类型守卫与默认值依赖其 7 值联合 `off\|minimal\|low\|medium\|high\|xhigh\|max`。Rust 侧需有一个共享 `ThinkingLevel` 枚举（args 单元与 runtime/agent 单元共用）。 |
| `@earendil-works/kiana-ai` | UA 格式、模型/凭据类型；`src/bun/runtime-setup.ts` 直连子路径 `@earendil-works/kiana-ai/bedrock-provider`、`/bun-oauth`、`/compat` | ai-core 单元；ca-cli 只依赖子路径的符号存在性（打包时须保留这三个导出，否则 bundle 阶段解析失败）。 |
| `undici`（间接） | `core/http-dispatcher` | 代理/超时（其它单元） |

### 6.2 建议的 crate 映射

| npm | Rust crate | 说明 |
|---|---|---|
| `chalk` | `anstream` + `owo-colors`（或自写 ANSI） | **必须**自定义 level 解析以复刻 chalk v6：`FORCE_COLOR`（0/1/2/3、空值视为 1）、`NO_COLOR`（非空即禁用）、`TERM=dumb`、`CI`、TTY。`anstream` 的自动检测规则与 chalk **不同**，不能直接依赖。 |
| `yaml` | `serde_yaml`（或 `yaml-rust2`） | `yaml` 包支持多文档/锚点；`parse` 错误消息不同（ca-cli 不比对消息），但空文档返回 null → `{}` 必须保持。 |
| `semver` | `semver` | 行为一致；注意 `valid()` 接受前导 `v` 与空白（`trim` 后传入）。 |
| `proper-lockfile` | `fs2` 或 `fd-lock` | **差异**：proper-lockfile 用 `<dir>.lock` 目录 + `realpath:false`，返回 ELOCKED 错误码；需移植其目录锁语义（mkdir 原子性 + stale 检测 + 更新 mtime），不能仅用 flock。 |
| `cross-spawn` | `std::process::Command` + `which` | Windows 下需要 PATHEXT/.cmd 解析；Linux 直接用 `Command`。 |
| `hosted-git-info` | 自写 | 维护 host 表（github.com/gitlab.com/bitbucket.org/gist.*）与 URL 形式（`git@`、`https://`、`#committish`）。 |
| `highlight.js` | **自写或移植** | `syntect` 的 scope 命名（`keyword.control`）与 highlight.js（`keyword`、`title.function_`、`string`）不同，替换会改变主题映射结果；建议移植 highlight.js 的 21 种 grammar 到 Rust（或内嵌其正则集），并复刻 `renderHighlightedHtml`。 |
| `photon-node`（WASM） | `image` + `jpeg-encoder`/`png` + 自写 Lanczos3 | **最难**：输出字节需与 Photon 的 resize/encode 完全一致，否则 base64 不同。见 §8。 |
| `zlib` (deflateRawSync / crc32) | `flate2`（`Compression::default()` 即 level 6？需核对）+ `crc32fast` | ZIP 内容对 zlib 版本敏感；`deflateRawSync` 默认 level 6，`flate2` 默认也是 6，但实现（miniz_oxide vs zlib）可能产生不同字节。测试若只校验可解压性则无碍。 |
| `node:readline`（promptConfirm） | `tokio::io::stdin` + 手写读取行 | 提示串必须逐字 `[y/N] ` 且不带换行。 |
| `node:worker_threads` | `tokio::task::spawn_blocking` | 图片缩放放阻塞池。 |
| `node:fs.watch` | `notify` | 5000ms 重试语义需上层实现。 |
| `node:os.tmpdir` | `tempfile`/`std::env::temp_dir` | 注意 macOS 的 `/var/folders/...` 差异由系统决定，不算移植差异。 |
| `node:crypto randomUUID/randomBytes` | `uuid` + `rand` | 输出格式一致（36 字符带连字符 / 16 位小写 hex）。 |
| `undici`/`fetch` | `reqwest`（rustls-tls） | 需要手动实现 `redirect:"manual"`、`AbortSignal` 语义（总预算 vs 单次尝试）、响应体取消（drop）。 |
| `node:zlib.crc32` | `crc32fast` | 一致。 |
| Node `Date` 格式化（zip DOS 时间） | `chrono::Local` | 必须用**本地时间**，字段公式见 §2.21.15。 |
| JS `String.prototype.toFixed(1)` / 数字格式化 | 手写 | `formatTokenCount` 的 `.toFixed(1)` 与 `formatDimensionNote` 的 `.toFixed(2)` 需要银行家舍入？JS 用“四舍五入到最近、正好一半时向上”的 IEEE 表示行为；Rust `format!("{:.1}")` 亦为最近偶数/一半向上需逐例核对（对 0.05 类边界可能不同）。 |

### 6.3 需要桥接的 JS/Rust 行为差异

| 差异 | 影响 | 桥接方式 |
|---|---|---|
| **UTF-16 索引** | `stripAnsi`/`splitIncompleteAnsiSuffix`/`decodeHtmlEntityAt`/`padEnd` 都按 UTF-16 码元计长；Rust `String` 按字节、`chars()` 按码点 | 在对齐处改用 `Vec<u16>` 或按码点显式实现；`padEnd(30)` 对 ASCII 安全，但对扩展名/描述含中文/emoji 时必须按 UTF-16 计长（chalk 的宽度不影响 `String(length)`）。 |
| **`String.prototype.padEnd`** | 用 UTF-16 长度补空格 | 同上；帮助文本中 `--name <value>` 后补到 30，均 ASCII，安全；环境变量行同理。 |
| **`localeCompare`** | `list-models` 排序与 `resolveCliModel` 的候选排序 | Node 默认 ICU locale（`en-US`，collation 对 ASCII 等同于码点序，但对重音/大小写不同）；Rust 用 `unicase`+显式规则，或直接比较小写后的字节序（模型 id 基本 ASCII）。 |
| **正则方言** | `ansiRegex`、`stripJsonComments`、changelog/git 正则 | Rust `regex` 不支持回溯/反向引用；这些正则均无回溯，可转写。`\u009B` 写 `\x{009B}`；`[\s\S]` → `(?s:.)`。 |
| **JSON 键顺序** | `JSON.stringify` 保持插入顺序 | `serde_json` 需启用 `preserve_order`（`indexmap`）。 |
| **JSON 数字/转义** | `JSON.stringify` 对字符串转义、对浮点用最短往返 | `serde_json` 行为基本一致；`JSON.stringify(x,null,2)` 的缩进为 2 空格、无尾随换行。 |
| **`Number.parseInt` + `String.fromCodePoint`** | HTML 实体解码 | Rust `char::from_u32` 拒绝代理区（`0xD800..0xDFFF`），JS `String.fromCodePoint` 会**接受**并产生孤立代理码元；必须在 `decodeCodePoint` 复刻 JS 行为（用 `char::decode_utf16` 或保留 `Vec<u16>`）。 |
| **`process.stdout.columns`** | SelfUpdateNote 宽度 | Rust 无全局；需由终端/`COLUMNS` 提供，缺省 80。 |
| **进程退出语义** | `process.exit()` 立即终止、不等待异步清理 | Rust `std::process::exit()` 语义类似；但 `main` 中 print 模式是设置 `exit_code` 后自然返回。 |
| **stdout 接管** | `takeOverStdout` 把 `process.stdout.write` 重定向到 stderr | Rust 需自定义输出层：`Output::stdout()` 在接管时写 stderr，`write_raw_stdout` 写真实 stdout（带 ENOBUFS/EAGAIN 重试）。 |

---

## 7. 测试清单

> 全部位于 `packages/coding-agent/test/`。下列测试直接覆盖本单元行为，Rust 侧应逐条转写为集成/单元测试。

| 测试文件 | 断言的行为 |
|---|---|
| `args.test.ts`（88 个调用点 / 92 个运行时用例） | `parseArgs` 全部分支：`--version`/`-v` 及其优先级、`--help`/`-h`、`--print`/`-p`（含 `-p "--- x"` 的 YAML frontmatter 提示、`-p` 后不吞选项）、`-c`/`-r`、带值选项、`--name`、`--models`、`--tools`/`-t`、`--exclude-tools`/`-xt`、`--thinking` 非法警告、`--mode` 校验、`--list-models` 可选搜索、`--tui-mode`、`--`/`@file`、未知短选项报错、未知长选项 `=`/空格取值、`-nt`/`-nbt`、`-e`/`-ne`、`-ns`/`-np`/`-nc` 等。 |
| `suite/regressions/7269-cli-end-of-options.test.ts` | `--` 之后停止解析但保留 `@file` 处理。 |
| `auth-check.test.ts` | `checkProviderAuth` 的 ready/not_ready/invalid、`--model` 解析 provider、`--no-refresh` 不刷新、默认刷新、未知 provider、环境变量未解析的凭据不算已配置、损坏 auth 状态 invalid、**不创建 auth.json 或父目录**、JSON/凭据/`--no-refresh` 参数接受、`createAuthCheckModelRuntime` 不落盘 catalog（`InMemoryCodingAgentModelsStore`）。 |
| `credential-print.test.ts` | 打印 API key、仅 `--model` 且只在已配置 provider 的 catalog 模型中解析、从 `Authorization` 头解析 bearer、过期 OAuth 先刷新、未知选项报错文案与 package 命令一致、参数/凭据类型校验。 |
| `initial-message.test.ts` | stdin 与首个 CLI message 合并、仅 stdin、stdin+fileText+message 顺序。 |
| `ansi-utils.test.ts` | 与 chalk `strip-ansi` 对生成输入一致、非字符串抛 `Expected a \`string\`, got \`<type>\``、RIS/单字节 ESC 不泄漏 final byte、常见 ANSI 序列。 |
| `config.test.ts` | `findNodePackageDir` 跳过 dist 内元数据；`detectInstallMethod` 从 Windows `.pnpm` 路径识别 pnpm；unknown wrapper 不自更新；自定义 npm 前缀自更新；精确版本不卸载当前包；重命名包从当前前缀；`npmCommand` 配置；空 `npmCommand` 视为未设置；display 路径引号；不从 Windows 包路径推断前缀；bun 全局从 `bun pm bin`；pnpm 重命名先卸载；pnpm v11 从 store 解析；yarn/bun 重命名；不可写路径不自更新；`detectInstallChange` 报告 removed。 |
| `paths.test.ts` | `canonicalizePath`（文件/符号链接/目录链接/不存在/悬挂链接）、`getCwdRelativePath` 点名前缀与父目录、`resolvePath` 只展开 home `~`、相对解析、`file:` URL、非法 file URL 抛错、字面 `%` 保留、Windows file URL 不当成本地路径、`normalizeWindowsShellPath`、`isLocalPath`。 |
| `image-process.test.ts` / `image-processing.test.ts` | BMP 魔数识别、BMP→PNG（禁用自动缩放时）、BMP 先转换再缩放；`convertToPng` 对 PNG 原样、JPEG→PNG、XMP APP1 段后的 EXIF 方向、`loadPngTranscoder` 同步输出方向修正后的 PNG、`resizeImage` 不破坏调用方字节、尺寸内原样、超尺寸缩放、超字节限制、无法压到 maxBytes 返回 null、JPEG 输入。 |
| `image-resize-callers.test.ts` | read 工具在无法产出安全图片时退化为纯文本；file processor 在自动缩放失败时省略图片；read 工具收到当前模型的 resize 配置；文件附件可延迟到提示派发时再缩放。 |
| `tool-result-images.test.ts` | 无 image 块返回原数组、已在限制内返回原数组、超限缩放并报告原始尺寸、禁用自动缩放时保留超大图、禁用时仍转换不支持格式、不可解码图保留、周围文本块顺序保持。 |
| `block-images.test.ts` | `blockImages` 设置默认 false、可设置、可持久化、与 autoResize 共存；read 工具始终读图（过滤在 convertToLlm 层）；`processFileArguments` 始终处理图片、BMP→PNG 附件、文本文件正常。 |
| `mime`（含于 image 测试） | JPEG/PNG/APNG/GIF87a/GIF89a/WebP/BMP 嗅探。 |
| `clipboard.test.ts`（20+ 例） | `readClipboardText` 等待原生并捕获拒绝、wl-paste 不可用时回退 X11；`copyToClipboard`：本地原生成功不发 OSC52、Linux 跳过原生、远程先等待原生再 OSC52、原生拒绝回退 pbcopy、只读原生使用命令写入、wl-copy 失败后试 xclip/xsel、本地 Linux 失败不把未验证 OSC52 当成功、无 display 回退 OSC52、WSL 无 display 用 PowerShell、WSL 互操作不可用回退 OSC52、WSL+WT 优先 OSC52、远程会话只发一次 OSC52、超大 payload 用 PowerShell、WSL 有 display 优先 Linux 工具、Wayland 报告 wl-clipboard、Termux 用 termux-clipboard-set、Android 报 Termux:API 需求、远程会话最终用 OSC52。 |
| `clipboard-command.test.ts` | 保留二进制、区分“空成功”与失败、Unicode 输入、超时不阻塞事件循环、超 buffer 上限拒绝。 |
| `clipboard-image.test.ts` / `clipboard-image-bmp-conversion.test.ts` / `clipboard-image-native-errors.test.ts` | X11 TARGETS 失败不探测图片类型、未声明类型不探测、WSL 先 PowerShell 再坏掉的 X11 桥、无原生 helper 返回 null、Termux 不读图片；BMP 转换路径；原生错误处理。 |
| `clipboard-paste-file-paths.test.ts` | `readClipboardFilePaths` 行为（经 tui 原生剪贴板）。 |
| `tools-manager.test.ts` | `getLatestVersion` 从 release 页重定向解析、无 `v` 前缀保留、相对重定向、丢弃 body、不重定向时报错、指向非 release tag 报错；`ensureTool` 通过回调报告状态且**不写 console**、下载失败暴露 cause 链。 |
| `version-check.test.ts` | 版本比较、只返回更新版本、kiana.dev API 与 UA、`retry:true` 时重试一次、自动检查只发一次请求、嵌套网络错误格式化、返回 packageName、返回 note、`KIANA_SKIP_VERSION_CHECK` 时跳过自动调用但直接调用仍可用。 |
| `management-http.test.ts` | 传输失败重试一次、`timeoutMs` 跨尝试共享、单次尝试超时可重试、可重试状态码后返回成功响应、不重试调用方取消。 |
| `frontmatter.test.ts` | 解析键/去引号/返回 body、CRLF 归一、非法 YAML 抛错、`|` 多行、缺失/未闭合返回原内容、空或仅注释返回 `{}`；`stripFrontmatter` 去前导并 trim、无 frontmatter 返回 body。 |
| `changelog.test.ts` | 包相对链接重写为 tag 固定的 GitHub 源码链接、旧仓库 URL 规范化且不改外部链接。 |
| `syntax-highlight.test.ts` | 启动时加载 20 种常用语言并延迟其余、按主题渲染 span、解码 highlight.js 的 HTML 实体、未映射嵌套 scope 继承父格式、跨无 scope span 保持父格式、通过 highlight.js 高亮、Python docstring 每行独立着色、diff 增删着色、cli-highlight 默认 scopes 映射。 |
| `git-ssh-url.test.ts` | 协议 URL（无 `git:` 前缀）解析 HTTPS/ssh/带 ref；简写 URL（必须 `git:` 前缀）解析 `git@host:path`、`host/path`、带 ref；拒绝不安全路径输入；无前缀时拒绝 `git@`、`host/path`、`user/repo`。 |
| `keybindings-migration.test.ts` | 旧键名重写为命名空间 id、新旧同名时保留新值、文件重写前内存中可加载旧键名。 |
| `kiana-user-agent.test.ts` | `getKianaUserAgent` 格式（kiana.dev 期望）。 |
| `sdk-client-identity.test.ts` | 详细 UA 与 CLI agent 归属、`KIANA_TELEMETRY` 关闭归属、`KIANA_TELEMETRY=0`、OpenCode 自定义 provider 的 session 头、配置头覆盖 session 头（跨单元，但经 ca-cli 常量）。 |
| `codemode-worker-config.test.ts` | Bun 二进制用相对源码入口、bundled Node 用旁边 worker、unbundled 用 kiana-codemode worker。 |
| `shell-path-exists.test.ts` | Windows Store 别名接受显式配置、PATH 结果失败仍拒绝；Unix 接受 `/bin/bash`、access 失败时回退 PATH、显式路径失败拒绝。 |
| `windows-shell-config.test.ts` | legacy WSL bash 检测（`System32`/`Sysnative`/无 SystemRoot 回退）→ `-s`/stdin。 |
| `powershell-config.test.ts` | 优先 PATH 上 pwsh、其次 ProgramFiles 下 PS7、其次 PATH 上 powershell、其次 SystemRoot 下 v1.0、都无时报错。 |
| `shell-kill-process-tree.test.ts` | macOS 先 SIGSTOP 组再 SIGKILL 且不恢复、SIGSTOP 失败仍杀组、组杀失败回退单进程、两者失败都容忍、Linux 组终止不变。 |
| `suite/regressions/6596-taskkill-enoent.test.ts` | taskkill spawn 失败（ENOENT）不崩溃。 |
| `suite/regressions/5303-bash-output-truncation.test.ts` | 子进程 exit 后仍输出（后代持有 stdout）能完整捕获；安静的后代在 grace 后释放。 |
| `suite/regressions/8337-utf8-bom-parsing.test.ts` | 带 BOM 的 frontmatter 与 settings 可加载。 |
| `first-time-setup.test.ts` | `shouldRunFirstTimeSetup`：实验特性 + 默认 agent 目录 + 无 settings.json → true；实验关闭/自定义 agent 目录/已有 settings → false；analytics 默认关闭且无 tracking id、opt-in 生成 id、opt-out 不生成、切换保留 id。 |
| `first-time-setup-fork.test.ts` | 分叉发行版返回 false。 |
| `session-id-readonly.test.ts` | `--session-id`：元数据命令不持久化自定义 ID、创建缺失 ID 并在同进程重开、精确 ID 查找不构建完整列表、重命名后精确 ID 可重开、自定义 session 目录中按 cwd 过滤、fork 目标已存在时进程内拒绝。 |
| `startup-session-name.test.ts` | `--name` 在运行时模型校验前设置到所选会话。 |
| `session-file-invalid.test.ts` | `--session` 指向非法文件时友好报错并保留文件内容。 |
| `stdout-cleanliness.test.ts` | 非交互模式下 `--help` 纯文本进 stdout；`--mode json --help` 时 stdout 为空、可信启动杂讯走 stderr。 |
| `max-thinking.test.ts` | CLI 与设置接受 `max`；旧主题回退 `thinkingXhigh`。 |
| `config-value-migration.test.ts` | 配置值 env var 语法迁移不影响大写 auth.json API key / models.json key 与 header。 |
| `format-resume-command.test.ts` | resume 命令格式（默认/非默认 session 目录、引号规则、非 TTY/内存/缺失文件返回 undefined）——属交互模式单元但依赖 config 路径。 |
| `package-command-paths.test.ts`（40 例） | 全局相对本地包路径持久化、尾斜杠删除、未信任项目设置跳过、remembered trust 用于 list、`--no-approve` 覆盖、`--approve` 批准、默认项目信任、`project_trust` 扩展、update 不提示不询问扩展、update 用已保存信任、`trust.json` 覆盖默认、未信任阻止本地包修改、本地安装可初始化新项目设置、install 子命令帮助、`update --models` 只刷新 catalog、拒绝 `--models` 与其它目标组合、config local 模式循环项目覆盖、config global 模式切换内置扩展、config local 模式循环内置扩展覆盖；另含 managed install（UTF-8 BOM marker、prune releases、ELOCKED、`--force` 拒绝、npmCommand 变体、pnpm 提示文案）。 |
| `restore-sandbox-env.test.ts`（3 例） | `src/bun/restore-sandbox-env.ts`：非 Bun 时不动 `process.env`；`process.env` 已有条目时不动；Bun 且 env 为空时从 `/proc/self/environ` 按 `\0` 与首个 `=` 恢复（`node:fs` 被 mock）。 |
| `package-distribution.test.ts` | `package.json` 入口布局：`bin.kiana === "dist/bundle/cli.js"`、`main === "./dist/index.js"`、`exports["."].import === "./dist/index.js"`、`exports["./rpc-entry"].import === "./dist/bundle/rpc-entry.js"`。Rust 侧对应两个 bin（`kiana`/`kiana-rpc`）。 |
| `session-cwd.test.ts`（3 例） | `getMissingSessionCwdIssue` 的三字段；`SessionManager.open(path,dir,cwd)` 覆盖有效 cwd 后不再报缺失；`createAgentSessionRuntime` 在 stored cwd 缺失时**先于** factory 调用抛 `MissingSessionCwdError`。 |
| `suite/regressions/5661-uppercase-header-values.test.ts` | 启动迁移（`runMigrations`）**不得**把 `models.json` 里的大写 env-var 字面量改写成小写或展开：`apiKey: "CUSTOM_API_KEY"`、`headers.Authorization: "BEARER"` 在迁移后仍为字面量（即使同名 env 已设置）。这约束 §2.7/§2.14 的"auth env 语法迁移"只作用于真正的 env 引用语法。 |
| `suite/regressions/7497-session-discovery-symlink.test.ts`（3 例） | `SessionManager.listAll()` 在 `sessions/` 下有目录符号链接时：① 通过目录链接发现会话并**保留别名路径**（`path === join(aliasDir,"linked.jsonl")`，即不 realpath）；② 断链目录被忽略且不影响其它会话；③ 指向**普通文件**的链接被忽略（`it.skipIf(win32)`）。Rust 侧 `read_dir` + `symlink_metadata` 的 `is_dir` 判断要能跟随目录链接、又能跳过断链与文件链接。 |
| `suite/regressions/5080-signal-shutdown-extension-cleanup.test.ts` | **仅作交叉引用，不属 ca-cli 必转写范围**：测 `InteractiveMode.shutdown`（SIGTERM/SIGHUP 时 `session_shutdown`/`runtimeHost.dispose` 先于终端恢复；Ctrl+D、`/quit` 相反）与 dead-terminal stdin 错误。只用到 `config.APP_NAME`，主体归 interactive-mode 单元。 |
| `documentation.test.ts` | 单一用例：`docs/docs.json`（顶层字段白名单 `navigation`/`redirects`）与 `docs/**/*.md` 的一致性——导航组必须含非空 `items`、导航项字段白名单（`title`/`path`/`items`）、重复导航路径、导航缺失页、本地 `.md` 链接断链、公开 slug 冲突、孤立页。当前 `redirects` 4 条（`development.md`→`index.md`、`session.md`→`session-format.md`、`tree.md`→`sessions.md`、`process-integration.md`→`cli-integration.md`），导航 3 个顶层组。覆盖 §2.22 引用的 `cli.md`/`environment-variables.md`/`settings.md` 等；不验证 ca-cli 运行期行为。 |

---

## 8. Rust 模块布局与最难复刻项

### 8.1 crate 与模块

```
kiana/
  Cargo.toml                     # workspace
  crates/
    kiana-coding-agent/             # 本单元 crate（ARCHITECTURE.md §3.7：spec 里的 `kiana-cli` 改名为
                                 # `kiana-coding-agent`，与 TS 包名一致；bin 名仍是 `kiana`）
      src/
        main.rs                  # cli.ts 等价：setup_cli(); block_on(app::run(args))
        lib.rs                   # ca-cli 公共面（对应 src/index.ts 的 ca-cli 部分）
        setup.rs                 # setup_cli / RPC 入口初始化
        migrations.rs            # 全部启动迁移
        args.rs                  # Args / Diagnostic / parse_args / print_help / thinking 校验
        app.rs                   # main() 编排、createSessionManager、buildSessionOptions、createRuntime
        auth/
          mod.rs
          command.rs             # parseAuthCommand / 文案 / getAuthCredential
          check.rs               # checkProviderAuth / createAuthCheckModelRuntime
          credential_print.rs    # resolveCredentialForPrint
        startup_ui.rs            # createStartupTui / startStartupTui / 首次设置 / 选择器
        session_picker.rs
        config_selector.rs
        project_trust_ctx.rs
        initial_message.rs
        file_processor.rs
        list_models.rs
        package_cmd/
          mod.rs                 # handlePackageCommand / handleConfigCommand
          parse.rs               # parsePackageCommand 状态机
          help.rs                # 全部帮助文本
          managed.rs             # managed install / 自更新
        utils/                   # 本单元拥有 `src/utils/**` 的**规格与验收**（§0.1 的 37 个文件、§2.21），
                                 # 但 `.rs` 文件按 ARCHITECTURE.md §5 落在 **`kiana-core`** 的模块树
                                 # （`kiana_core::{ansi,text,clipboard,image_process,syntax_highlight,…}`）。
                                 # 本 crate 不重复实现，只 `pub use kiana_core::…`（`config.rs` 同理）。
                                 # 因此下面的目录树是**职责映射**，不是 crate 边界。
          ansi.rs                # strip_ansi / split_incomplete_ansi_suffix
          text.rs                # BOM
          json.rs                # strip_json_comments
          paths.rs
          frontmatter.rs
          abort.rs
          sleep.rs
          open_browser.rs
          fs_watch.rs
          output_files.rs
          html.rs
          zip.rs
          management_http.rs
          mime.rs
          image/
            mod.rs               # process_image / tool_result_images / convert
            resize.rs            # resize_image / format_dimension_note
            resize_core.rs       # 算法（Lanczos3 + 质量步 + 0.75 循环）
            exif.rs              # orientation 解析与像素变换
            photon.rs            # 原生解码/编码适配（替代 WASM）
          shell.rs
          clipboard.rs
          clipboard_command.rs
          clipboard_image.rs
          child_process.rs
          tools_manager.rs
          version_check.rs
          windows_self_update.rs
          changelog.rs
          syntax_highlight/
            mod.rs               # render_html + 主题映射
            grammars/            # 21 种语言 grammar（移植 highlight.js）
          git.rs
          kiana_user_agent.rs
          deprecation.rs
          wsl.rs
      tests/                     # 对应 §7 的集成测试
```

关键类型：

```rust
pub struct Output;               // 封装 stdout 接管：write_stdout（可重定向到 stderr）、write_raw_stdout、flush
pub struct CliContext { pub output: Output, pub env: EnvSnapshot, pub cwd: PathBuf, pub agent_dir: PathBuf }
pub struct ExitCode(i32);
pub enum AppMode { Interactive, Print, Json, Rpc }
pub trait TrustUi { fn select(...); fn confirm(...); fn input(...); fn notify(...); }
pub struct SessionResolution;     // ResolvedSession 的 Rust 版
```

设计要点：
- **输出接管**：Rust 中 `println!` 直接写 fd 1，无法像 JS 一样替换 `process.stdout.write`。所有 CLI 输出必须走 `ctx.output` 抽象（`write_stdout` 在接管时写 fd 2；`write_raw_stdout` 写 fd 1 并处理 `ENOBUFS`/`EAGAIN`/`EWOULDBLOCK` 重试，重试间隔 10ms，串行化）。
- **chalk 级别**：在进程启动时计算一次 `ColorLevel`（0/1/2/3），之后 `red()` 等按级别加/不加 SGR。**必须**在 `--help` 与错误输出路径上使用同一级别，否则 stdout/stderr 颜色不一致。
- **异步**：`main` 为 `async fn`，统一在 `tokio` 上 `block_on`。子进程用 `tokio::process`，阻塞操作（图片、ZIP、部分同步 spawn）用 `spawn_blocking`。`wait_for_child_process` 的“exit 后 100ms 空闲计时、数据到达重置”逻辑用 `tokio::select!` + `tokio::time::sleep` 实现（不能用固定 deadline）。
- **信号/退出**：`std::process::exit` 用于与 `process.exit` 等价的路径；print 模式通过返回 `ExitCode` 设置。

### 8.2 最难复刻的部分（按风险排序）

1. **Photon WASM 的图片字节级一致**。`resizeImageInProcess` 的输出直接进入模型请求与会话历史；测试 `image-processing.test.ts` 断言尺寸/格式/是否重缩放，但不比对字节。若未来要求字节一致，需复刻 Photon 的：`new_from_byteslice` 解码（`image` crate 的解码像素可能不同，尤其 JPEG IDCT/CMYK）、`Lanczos3` 重采样（Photon 用 `resize` crate 的 Lanczos3）、`get_bytes()`（PNG 编码器设置）、`get_bytes_jpeg(quality)`（jpeg-encoder 的量化表/色度抽样）、`get_raw_pixels` 的 RGBA 布局与 EXIF 旋转索引。**建议**：先保证“尺寸/格式/是否 wasResized/base64 长度在限制内”一致，字节差异记录为已知偏差，并新增对比测试用固定输入比较 SHA-256（若可接受差异则放宽）。
2. **highlight.js 语法与 scope 命名**。`syntect` 无法直接产出 `hljs-*` 类名；主题映射（`getScopeFormatter` 的精确→`.`前缀→`-`前缀→`default`）依赖 highlight.js 的 scope 字符串（如 `title.function_`、`string.regexp`、`meta.string`）。必须移植 21 种 grammar（或至少在 Rust 中重实现并对拍）。测试 `syntax-highlight.test.ts` 断言了 Python docstring 分行、diff 着色、cli-highlight 默认映射，说明 scope 细节被观察。
3. **chalk v6 颜色级别判定**。`--help` 在 TTY 下必须带颜色、在管道下必须不带；`FORCE_COLOR`/`NO_COLOR`/`TERM=dumb`/`CI` 的组合规则需与 chalk 完全一致。任何偏差都会改变字节输出。
4. **proper-lockfile 的 ELOCKED 语义**。managed 更新要求“另一个更新正在运行”时精确报 `Another managed Kiana update is already running.`，且 `cleanupManagedInstall` 在锁被占用时静默跳过。`proper-lockfile` 用 `<path>.lock` **目录**、`realpath:false`、默认 stale 10s、更新间隔 5s、重试 0 次（`lockSync` 立即失败）；Rust `fs2`/`flock` 的跨进程语义不同（NFS、容器、同进程多线程）。需移植目录锁（`mkdir` 原子 + mtime 心跳），否则 ELOCKED 不会出现。
5. **`fetchWithRetry` 的 AbortSignal 组合语义**。`timeoutMs` 共享总预算（每次尝试前 `throwIfAborted`）、`attemptTimeoutMs` 每次独立、调用方 signal 终止、`AbortError` 且无 timeout 时直接抛、`response.body` 取消后再重试。`reqwest` 的 `timeout` 是单请求总超时，`connect_timeout` 不同；需用 `tokio::time::timeout` 包裹每次 attempt 并在循环间检查共享 deadline。还要复刻“`redirect:"manual"`”以读取 `location`（`reqwest` 需 `Policy::none()`）。
6. **`process.stdout.columns` 与 Markdown 渲染宽度**。SelfUpdateNote 宽度 `max(20, columns ?? 80)`；Rust 需从 `ioctl(TIOCGWINSZ)` 或 `COLUMNS` 取，且在非 TTY 时回退 80。Markdown 渲染本身归 tui-components 单元，但宽度传递与 `trimEnd` 必须在 ca-cli 侧一致。
7. **Node `String.prototype.toFixed`**。`formatDimensionNote` 的 `scale.toFixed(2)` 与 `formatTokenCount` 的 `.toFixed(1)` 在边界值（如 1.005、2.675）上 JS 与 Rust 都可能因二进制表示产生差异；需用测试锁定典型值，必要时实现 JS 兼容的“十进制最短表示 + 定点”算法。
8. **UTF-16 长度语义**。`padEnd`、`html.decodeHtmlEntityAt`（索引按 UTF-16）、`stripAnsi`（`slice` 按 UTF-16）在含代理对文本（emoji、部分 CJK 扩展）时与 Rust 字节/码点索引不同。建议在解析/切片处统一用 `Vec<u16>`，或明确限定这些函数只处理 BMP 字符并在测试中覆盖代理对。
9. **chalk 的 ANSI 嵌套/重置顺序**。`chalk.bold(chalk.yellow(text))` 产生 `\x1b[1m\x1b[33m...\x1b[39m\x1b[22m`（嵌套按内到外重置）。自写 ANSI 必须复刻 chalk 的 opener/closer 顺序，包括 `dim`(`2`/`22`)、`italic`(`3`/`23`)、`underline`(`4`/`24`)、`strikethrough`(`9`/`29`)、`red`(`31`/`39`)、`green`(`32`/`39`)、`yellow`(`33`/`39`)、`cyan`(`36`/`39`)、`bold`(`1`/`22`)。
10. **会话与信任交互的时序**。`--session` 跨项目 fork 提示、`--resume` 选择器与 `stopThemeWatcher()` 的 `finally`、`missing cwd` 的选择器、首次设置在 runtime 创建之前运行——这些顺序会影响输出与退出码，必须按 §2.4.7 的编号严格执行。
11. **Windows 特有路径**：`taskkill.exe` 绝对路径、`where`/`which`、Git Bash 探测、legacy WSL bash 的 `-s` stdin 传输、`cross-spawn` 的 `.cmd` 解析、`quarantineWindowsNativeDependencies` 的已加载模块枚举（`process.report` 无对应）。Linux CI 上无法测 Windows 分支，需用 feature gate 与单元测试桩。
12. **`hosted-git-info` 行为**：`fromUrl` 对 `github:user/repo`、`gitlab:`、`bitbucket:`、`gist:` 前缀、`#committish`、`@` 后缀、`git+https://`、`ssh://git@` 的识别与 `domain`/`user`/`project`/`committish` 字段；`parseGitUrl` 还依赖“`split.ref && info.project?.includes("@")` 跳过”的细节。建议把 `hosted-git-info` 的 host 表与解析规则逐条移植，并用 `git-ssh-url.test.ts` + 额外对拍用例验证。
13. **`yaml` 包的宽松性**：`parse("")` 返回 null → `{}`；`parse("# comment")` → null；`|` 多行；非法 YAML 抛错（消息不比对）。`serde_yaml` 对空文档返回 `Value::Null`（需映射为 `{}`），对重复键、tab 缩进、flow 集合的接受度可能不同。
14. **`zlib.crc32`/`deflateRawSync` 字节**：ZIP 若被外部工具校验，deflate 实现（zlib vs miniz_oxide）可能产生不同压缩字节，但解压结果一致；CRC 必须一致（`crc32fast` 一致）。测试若比对整包哈希需锁定压缩级别与实现。

### 8.3 建议的实施顺序

1. `utils/ansi`、`utils/text`、`utils/json`、`utils/paths`、`utils/kiana_user_agent`、`utils/wsl`、`utils/sleep`、`utils/open_browser`、`utils/deprecation`、`utils/html`（无外部依赖，易测试）。
2. `config` + `args` + `print_help`（可在无运行时的情况下单测所有文案）。
3. `migrations` + `utils/frontmatter` + `utils/output_files` + `utils/zip`。
4. `utils/child_process` + `utils/shell` + `utils/management_http` + `utils/version_check` + `utils/tools_manager` + `utils/changelog` + `utils/git`。
5. `utils/mime` + `utils/image/*`（先保证尺寸/格式，再做 EXIF，最后评估 Photon 字节一致性）。
6. `utils/clipboard*`（含 WSL/PowerShell 分支）。
7. `cli/auth*` + `list_models` + `initial_message` + `file_processor`。
8. `package_cmd`（含 managed install 与自更新）。
9. `startup_ui` + `session_picker` + `config_selector` + `project_trust_ctx`。
10. `app.rs` 编排（依赖前 9 项与其它单元的运行时接口）。
11. `syntax_highlight` 最后（需要 grammar 移植与主题映射对拍）。

---

## Audit additions

> 本节由独立审计追加。上面各节已就地修正错误陈述；以下补充遗漏项。凡与上文冲突，以本节为准。

### A.1 `parseArgs` 边角行为（补充 §2.5.2）

以下分支源码存在但上表未覆盖，Rust 必须逐条复刻（`test/args.test.ts` 会观察 `unknownFlags`/`diagnostics`/`messages`）：

1. `--unknown-flag=value` → `unknownFlags.set("unknown-flag","value")`；**`--unknown-flag=`（等号后为空）合法**，得到空字符串 `""`（不是 `true`、不是错误）。`eqIndex !== -1` 的判断不要求等号后非空。
2. 未知长选项的 `=` 分割对**任意** `--` 开头字符串生效，包括 `--=x`（flag 名为 `""`）。
3. 单个 `-`（长度为 1）与 `-abc`（多字符短选项）都走 `arg.startsWith("-") && !arg.startsWith("--")` 分支 → error `Unknown option: ${arg}`，**不**拆分为多个单字符 flag。
4. 未知长选项的下一项取值条件为 `next !== undefined && !next.startsWith("-") && !next.startsWith("@")`——因此 `--foo @bar` **不会**消费 `@bar`（`@bar` 随后进入 `fileArgs`），而 `--foo -1` 也不消费。
5. `--name`/`-n` 只要 `i+1 < len` 就消费下一项，**即使它以 `-` 开头**：`kiana -n --foo` → `name = "--foo"`（不会产生 unknown flag）。这是它与所有 `i+1<len` 值选项的关键差异。
6. `--print` 的下一项条件为 `!next.startsWith("@") && (!next.startsWith("-") || next.startsWith("---"))`：`---x` 被当作 message；`--x`、`-x`、`@x` 都**不**被消费。
7. `--mode` / `--use-theme` / `--tui-mode` 在"缺值"时检查 `undefined || startsWith("-")` 并 **不消费**；而 `--tui-mode` 在"值存在但不合法"时 **消费**（`i++`）再报错。三者语义不同，不能共用同一辅助函数。
8. 已知值选项与未知长选项的**优先级**：已知名一律走已知分支（即使缺值也不落入 unknown 处理），例如末尾的 `--model` 变成 `unknownFlags["model"]=true`，而末尾的 `--name` 报 `--name requires a value`。
9. `-t`/`-xt` 等短选项的报错消息里 `${arg}` 保留原样（`-t` 或 `--tools`），不会规范化成长名。
10. `--tools` 校验失败时 `tools` 字段保持 `undefined`（**不**赋空数组），且诊断只加一条 error。

### A.2 `Args.offline` 是死字段（补充 §2.5.1 / §2.4.7 第 3 步）

`parseArgs` 会设置 `Args.offline = true`，但 `main` **从不读取** `parsed.offline`；离线判定是 `args.includes("--offline") || isTruthyEnvFlag(process.env.KIANA_OFFLINE)`（对原始 argv，不是解析结果）。Rust 侧必须保留该字段（SDK/测试可能观察），但不得用它驱动行为，否则 `--offline` 会不会被 `--` 截断等边界会与 TS 不一致。

### A.3 `printHelp` 的签名（补充 §2.5.3 / §5.1）

TS 为 `printHelp(extensionFlags?: ExtensionFlag[])`（**可选**）。§5.1 的 `pub fn print_help(extension_flags: &[ExtensionFlag])` 应改为 `pub fn print_help(extension_flags: Option<&[ExtensionFlag]>)` 或允许空切片；扩展段条件为 `flags && flags.length > 0`，空数组与 undefined 输出完全相同。

### A.4 帮助文本逐字校验结果（确认 §2.5.3）

对 `args.ts` 模板字面量与 §2.5.3 代码块做机械 diff（把 `${APP_NAME}`→`kiana`、`${CONFIG_DIR_NAME}`→`.kiana`、两个 `padEnd(32)` 的环境变量行展开、去掉 chalk 包装、去掉 `${extensionFlagsText}` 占位），**其余 195 行逐字节一致**。唯一差异是 §2.5.3 用 `<扩展段>` 标注插入点。chalk.bold 仅包裹：`APP_NAME`、`Usage:`、`Commands:`、`Options:`、`Examples:`、`Environment Variables:`、`Built-in Tool Names:`、`Extension CLI Flags:`（§2.5.3 末尾已述，此处确认为完整集合——`Models`/`Tools` 等其余标题均不加粗）。

### A.5 `changelog.parseChangelog` 的 content 含版本标题行（修正 §2.21.34）

源码在遇到 `## ` 行且能解析出版本时执行 `currentLines = [line]`——**版本标题行本身是 content 的第一行**。因此 `entry.content` 形如 `## 1.4.0\n\n- 变更说明...`（`join("\n").trim()` 之后）。§2.21.34 只说"其它行在解析到版本后收集"，易被误读为不含标题行。Rust 必须包含该行，否则 `normalizeChangelogLinks` 对标题内链接的处理结果与快照不同。

另：`## ` 行解析失败时不仅 `currentVersion = null`，`currentLines` 也清空——非版本 `## ` 小节会**丢弃**此前已收集但未 push 的内容？不会：push 在 `line.startsWith("## ")` 分支开头已经发生（`currentVersion && currentLines.length > 0`），所以只是终止当前段。

### A.6 `migrateSessionsFromAgentRoot` 跳过空首行（补充 §2.7 / §4.4）

源码首行取出后有 `if (!firstLine?.trim()) continue;`——首行为空/纯空白的文件被**静默跳过**（不是解析失败，也不会被移动到 `sessions/`）。§4.4 只描述"读第一行，要求 `{"type":"session",...}`"，未点明空行早退，Rust 需按 `trim()` 判空。

同时确认：`readFileSync(file)` 读整文件后 `split("\n")[0]`，因此**没有换行符的单行文件**也会被正常处理（`content.split("\n")[0] === content`）。

### A.7 `getLatestKianaRelease` 与其它读取处的 `KIANA_OFFLINE` 差异（已在 §4.5 修正）

再强调三点实现细节：
- `version-check` 的判定是 `if (process.env.KIANA_OFFLINE) return undefined;`（任意非空，含 `"0"`/`"false"`）。
- `main` 用 `isTruthyEnvFlag`（`1`/`true`/`yes`，大小写不敏感）。
- `tools-manager.isOfflineModeEnabled` 用 `value === "1" || lower === "true" || lower === "yes"`（**注意 `"TRUE"`、`"Yes"` 通过，`" on "` 不通过，且不做 trim**）。
三处不可合并为一个"is_offline()"。

### A.8 环境变量补充（补充 §4.5）

本单元范围内**没有**直接读取 `NO_COLOR`/`FORCE_COLOR`/`TERM`/`CI`/`COLUMNS`（颜色级别判定在 chalk 内部、宽度来自 `process.stdout.columns`）。但 Rust 侧必须实现这些行为，且来源不是本单元的 `process.env` 读取：
- `NO_COLOR`（非空即禁用）、`FORCE_COLOR`（0/1/2/3，空值视为 1）、`TERM=dumb`、`CI` → chalk v6 的 level 判定（§3.4/§8.2-3）。
- `COLUMNS` → 仅在 `process.stdout.columns` 不可用时由终端层提供；TS 代码本身**不读** `COLUMNS`（§8.2-6 的"或 `COLUMNS`"是 Rust 建议，不是 TS 行为）。
- `KIANA_QUICKJS_WASM` 在 TS 中**不存在**（§2.6.3 提到的该名是 Rust 侧建议，不是 TS 环境变量）；TS 只有 `setEmbeddedQuickJSWasmPath`（Bun 入口调用）+ `createRequire().resolve`。
- `KIANA_MODEL`/`KIANA_PROVIDER`/`KIANA_REASONING_LEVEL`/`KIANA_SESSION_ID`/`KIANA_SESSION_FILE` 由 bash/powershell 工具注入，ca-cli 不读不写（§2.22 已述，确认）。

### A.9 `getExportTemplateDir` 与 `getThemesDir` 的 bun-binary 分支不对称（已在 §2.6.3 修正）

- `getThemesDir` bun-binary → `<pkgDir>/theme`（单数，无 `src`/`dist`）。
- `getInteractiveAssetsDir` bun-binary → `<pkgDir>/assets`。
- `getExportTemplateDir` bun-binary → `<pkgDir>/export-html`（**不是** `core/export-html`）。
- 三个非 bun 分支都是 `<pkgDir>/{src|dist}/...`，其中 `src` 存在性用 `existsSync(join(pkgDir,"src"))` 判定且**每次调用都重新判定**（可缓存但要注意 `KIANA_PACKAGE_DIR` 在进程内可变，源码未缓存）。

### A.10 依赖清单补充（补充 §6.1）

`src/cli/args.ts` 导入 `import type { ThinkingLevel } from "@earendil-works/kiana-agent-core"`（`@earendil-works/kiana-agent-core` 1.4.0，见 `package.json`）。该包原在 §6.1 缺失，**已补入 §6.1 表格**；`Args.thinking`、`isValidThinkingLevel` 的类型守卫依赖它。Rust 侧需有对应 `ThinkingLevel` 枚举（`off|minimal|low|medium|high|xhigh|max`），并由 args 单元与 runtime/agent 单元共享。

另：`src/bun/runtime-setup.ts` 导入 `@earendil-works/kiana-ai/bedrock-provider`、`@earendil-works/kiana-ai/bun-oauth`、`@earendil-works/kiana-ai/compat` 三个子路径（`bedrockProviderModule`、`registerBunOAuthFlows`、`setBedrockProviderModule`），以及 `quickjs-wasi/quickjs.wasm`（作为路径字符串，传给 `setEmbeddedQuickJSWasmPath`）。§6.1 只列了 `@earendil-works/kiana-ai` 整体，**已在 §6.1 行内标注这三个子路径**；Rust 侧没有 Bun，`runtime-setup` 的等价物应为 no-op（保留 Bedrock provider 注册的等价初始化，若 ai 单元需要）。

### A.11 用户可见字符串补充（补充 §3.3）

以下字符串在源码中存在但 §3.3 未收录，均为【逐字】：

```
Exported to: ${result}
Failed to export session                      // 非 Error 的 export 异常兜底
--provider requires --model (for example: --provider ${provider} --model <pattern>)
--api-key requires a model to be specified via --model, --provider/--model, or --models
Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character
Failed to run ${[command,...args].join(" ")}: ${reason}
Expected a `string`, got `${typeof value}`    // stripAnsi 的 TypeError
Failed to load extension "${path}": ${error}  // runtime 诊断
Extension package "${path}": ${warning}        // runtime 诊断
Warning: Could not parse changelog: ${error}    // 走 console.error，无 chalk
Deprecation warning: ${message}
Source runtime requires compilerOptions.paths in ${tsconfigPath}
Source runtime does not support multiple wildcards in ${pattern}
Source runtime could not resolve ${specifier} through tsconfig path ${matchedPattern}
Extension package "${failure.source}": ${describePackageInstallFailure(failure)}
image resize worker requires parentPort
Invalid image resize worker request
Image resize worker exited with code ${code}
Unknown tool: ${tool}
${command}: ${formatSpawnFailure(result)}       // runExtractionCommand 前缀
exit status ${status ?? "unknown"}             // formatSpawnFailure 兜底
No response body
```

另外 `printSelfUpdateUnavailable` 的两行是 `console.error` 直出**无 chalk**（§2.9.5 已述），但注意首行小写 `error:`，与其它 `Error:` 前缀不同。

### A.12 测试清单补充（补充 §7）

§7 原列 45 行 / 45 个测试文件（一行一文件）；审计确认遗漏以下 **7 个直接覆盖本单元**的测试，已补入 §7（现 52 行 / 52 个文件）：

| 测试文件 | 断言的行为 |
|---|---|
| `restore-sandbox-env.test.ts`（3 例） | `src/bun/restore-sandbox-env.ts`：非 Bun 时不动 `process.env`；`process.env` 已有条目时不动；Bun 且 env 为空时从 `/proc/self/environ` 按 `\0`/首个 `=` 恢复（`readFileSync` 被 mock）。 |
| `package-distribution.test.ts` | `package.json` 的 `bin.kiana === "dist/bundle/cli.js"`、`main === "./dist/index.js"`、`exports["."].import === "./dist/index.js"`、`exports["./rpc-entry"].import === "./dist/bundle/rpc-entry.js"`。Rust 侧对应 `Cargo.toml` 的 `[[bin]] kiana` 与 `kiana-rpc` 入口。 |
| `session-cwd.test.ts`（3 例） | `getMissingSessionCwdIssue` 的字段（`sessionFile`/`sessionCwd`/`fallbackCwd`）、`SessionManager.open(path, dir, cwd)` 覆盖有效 cwd 后不再报缺失、`createAgentSessionRuntime` 在 stored cwd 缺失时**先于** factory 调用抛 `MissingSessionCwdError`。 |
| `suite/regressions/5661-uppercase-header-values.test.ts` | `runMigrations` 不改写 `models.json` 里的大写 env-var 引用（`apiKey: "CUSTOM_API_KEY"`、`headers.Authorization: "BEARER"` 保持字面量）。 |
| `suite/regressions/7497-session-discovery-symlink.test.ts` | `ENV_AGENT_DIR` 指向的 agent 目录中 `sessions/` 经符号链接时仍能发现会话（依赖 `config.getAgentDir` + session-manager）。 |
| `suite/regressions/5080-signal-shutdown-extension-cleanup.test.ts` | SIGTERM/SIGHUP 时 `session_shutdown` 先于终端恢复（依赖 `config.APP_NAME` 与 interactive-mode；跨单元但与 ca-cli 常量相关）。 |
| `documentation.test.ts` | 文档导航/链接结构合法（覆盖 `docs/**`，含 §2.22 引用的 `cli.md`/`environment-variables.md` 等）。不验证 ca-cli 行为，但会在 docs 改动时失败。 |

同时修正 §7 的数量描述（**均已在 §7 表格中原位改正**）：`args.test.ts` 实际有 **88 个 `it`/`test` 调用点**（其中 3 个 `test.each` 展开为 3+2+2=7 个运行时用例，故运行时共 **92 例**），原 §7 写的"~113 例"偏高，已改为"88 个调用点 / 92 个运行时用例"；`package-command-paths.test.ts` 实际 40 个调用点，原 §7 写的"30+ 例"已改为"40 例"。其余实际用例数供 Rust 测试规模参考：`auth-check.test.ts` 10 个，`credential-print.test.ts` 6 个，`initial-message.test.ts` 3 个，`version-check.test.ts` 10 个，`clipboard.test.ts` 21 个。

### A.13 §5.2 跨单元接口补充

§5.2 未列出但 ca-cli 在 `package-manager-cli.ts` 中使用的两个 `core/resource-loader.ts` 导出：

```rust
pub fn is_builtin_extension(input: &InlineExtension) -> bool;              // typeof input !== "function" && input.builtin === true
pub fn format_package_install_warning(failure: &PackageInstallFailure) -> String; // `Extension package "${source}": ${describePackageInstallFailure(failure)}`
impl DefaultResourceLoader { pub async fn load_project_trust_extensions(&mut self) -> LoadExtensionsResult; }
```

`formatPackageInstallWarning` 的完整展开（`describePackageInstallFailure`，私有）：
`failed to install (${failure.message}). ` 后接：
- `failure.scope === "temporary"` → `Check the source and try again.`
- 否则 → `Run "${APP_NAME} remove ${failure.source}${failure.scope === "project" ? " --local" : ""}" to remove it from settings.json.`

即 `kiana config` 的安装失败警告形如
`Warning: Extension package "npm:foo": failed to install (HTTP 404). Run "kiana remove npm:foo" to remove it from settings.json.`
（project scope 时是 `Run "kiana remove npm:foo --local" ...`）。

`isBuiltinExtension` 的语义是"非函数且 `builtin === true`"（用于 `kiana config` 的内置扩展开关列表）；`formatPackageInstallWarning` 的输出会经 `chalk.yellow` 加 `Warning: ` 前缀后进 stderr（§2.9.6 第 8 步）。

另需 `DefaultPackageManager` 的 `resolve(skipInstall?)`、`installAndPersist`、`removeAndPersist`、`listConfiguredPackages`（`{source, scope, filtered, installedPath}`）、`update(source?)`、`setProgressCallback`（`{type:"start", message}`），以及 `SettingsManager.drainErrors()` 返回 `{scope, error: Error}`（`error.stack` 被读取）。

### A.14 Rust 映射补充

1. **`process.stdout.columns`**：TS 的 `printSelfUpdateNote` 用 `Math.max(20, process.stdout.columns ?? 80)`。Rust 若在非 TTY 下拿不到宽度，**必须**回退 80（不能用 0 或 1），否则 `max(20,...)` 会把宽度压到 20，Markdown 换行位置不同。`COLUMNS` 不是 TS 行为，但可作为 Rust 的补充来源（优先 ioctl，其次 `COLUMNS`，最后 80）。
2. **`Buffer.byteLength(data,"utf-8")` vs `data.length`**：`image-resize-core.encodeCandidate` 的 `encodedSize` 是 base64 字符串的 **UTF-8 字节长度**；base64 全 ASCII，故等于字符数/字节数。Rust 用 `data.len()` 即可，但不要用 `chars().count()` 之外再乘系数。
3. **`crc32`**：Node 22.19 的 `node:zlib.crc32` 与 `crc32fast` 输出一致（IEEE 802.3）。**`deflateRawSync` 默认 level 是 `Z_DEFAULT_COMPRESSION`（-1 → 6）**，§6.2 的"需核对"可确定：`flate2::Compression::default()` 也是 6，但 zlib 与 miniz_oxide 的匹配算法不同，压缩字节可能不同；若测试只校验可解压性则无碍，若要字节一致需用 `flate2` 的 `zlib` feature（链接系统 zlib）而非默认 `miniz_oxide`。
4. **`Math.random().toString(36).slice(2,10)`**（`downloadTool` 的解压目录名）：Rust 无等价物；目录名只需唯一，可用 `rand` 生成 8 位 base36 或直接用 `Uuid`，但**长度不必一致**（该名不出现在任何用户可见输出或断言中）。
5. **`String.fromCodePoint` 接受孤立代理**：§6.3 已述；补充 `Number.parseInt` 对 `"0x10FFFF"`/`" 12 "`/`"12abc"` 的宽松解析（前缀数字即可），Rust `u32::from_str_radix` 不接受尾随垃圾，需自行实现"取前缀数字"。
6. **`process.report.getReport().sharedObjects`**（windows-self-update）：Linux 可用 `/proc/self/maps` 过滤 `.so`，macOS 用 `_dyld_image_count`/`_dyld_get_image_name`；但 `cleanupWindowsSelfUpdateQuarantine` 只在 win32 被 `main` 调用，非 Windows 可整体 no-op（§2.21.33 已述）。

### A.15 `main` 顺序的补充边界

- 第 8 步 `handlePackageCommand` 为真时，**win32 + `args[0]==="update"` + `exitCode===0`** 才自然返回；`args[0]` 是 `"uninstall"` 时**不**走该分支（即便它被规范化为 remove）。Rust 需用原始 `args[0]`。
- 第 10 步 `mcp` 分支用 `args[0]==="mcp"`，不看 `args[1]`；`runMcpCommand` 的返回值写入 `process.exitCode` 后**自然返回**（不 `process.exit`），因此后续清理仍会跑。
- 第 14 步 `takeOverStdout` 在 `--mode rpc` 下也会生效（`appMode!=="interactive"`），但随后的 RPC 错误输出走 `console.error`（stderr），不受影响。
- 第 19 步首次设置的条件里 `parsed.listModels === undefined` 是**严格 undefined** 判断（`--list-models` 无值时为 `true`，也跳过首次设置）。
- 第 33 步读 stdin 只在 `appMode !== "rpc"` 时执行；interactive 拿到非空 stdin 后**改成 print**，但 `takeOverStdout` 已经按原 `appMode` 决定（interactive 时未接管），所以此时 stdout 仍是真实 stdout——这是 TS 的既有行为，Rust 不要"修正"。
- 第 38/39 步的诊断输出在 interactive 且有 runtime error 时也会打印（`appMode!=="interactive" || hasRuntimeErrors`）。

### A.16 `createCommandSettingsManager` 的 `useSavedProjectTrustOnly` 细节

`handlePackageCommand` 对所有 `update` 都传 `useSavedProjectTrustOnly: true`，该分支**不加载**项目信任扩展、不调用 `resolveProjectTrusted`，只 `setProjectTrusted(override ?? trustStore.get(cwd)===true)`，并立即返回（`projectTrustWarnings` 为空）。因此 `kiana update` 从不弹信任提示（§2.9.5 已隐含，此处明确）。

### A.17 `buildSessionOptions` 的 `--model` 与 `scopedModels` 交互（补充 §2.4.6）

§2.4.6 已列出主流程，但以下三个"静默"细节会影响最终模型/思考级别，Rust 易错：

1. **`--provider` 无 `--model`** 只产生一条 error 诊断（`--provider requires --model (for example: --provider ${provider} --model <pattern>)`），**不提前返回**；若同时也没有 `--models`，后续 `buildSessionOptions` 继续走空 options，最终由"无可用模型"路径退出 1。
2. **`--model pattern:thinking` 的 thinking 优先级**：仅当 `!parsed.thinking && resolved.thinkingLevel` 时写入 `options.thinkingLevel` 并置 `cliThinkingFromModel = true`；显式 `--thinking` 在函数末尾覆盖（`if (parsed.thinking) options.thinkingLevel = parsed.thinking`）。`cliThinkingFromModel` 又用于 `main` 第 12 步的 `cliThinkingOverride`（`parsed.thinking!==undefined || cliThinkingFromModel`），进而决定是否调用 `setThinkingLevel`。
3. **scope 内默认模型**：`!options.model && scopedModels.length>0 && !hasExistingSession` 时才算；已存在会话（`sessionManager.buildSessionContext().messages.length>0`）时**不**用 scoped 默认覆盖会话模型。`modelsAreEqual` 命中保存的默认模型则用它，否则用 `scopedModels[0]`；两者的 `thinkingLevel` 都只在未显式 `--thinking` 时生效。`options.scopedModels` **始终**复制完整列表（含 undefined thinkingLevel，表示循环时继承当前级别）。

### A.18 `docs/docs.json` 的重定向与 §2.22 文档清单（补充 §0.1 / §2.22）

- §0.1 的"相关文档"清单原缺 `docs/settings.md`，**已补**（`documentation.test.ts` 会校验文档可达性，`settings.md` 是 ca-cli 相关设置项的唯一参考）。
- `docs/docs.json` 顶层只有 `navigation`/`redirects` 两个字段（白名单由 `documentation.test.ts` 强制）；当前 `redirects` 4 条：`development.md`→`index.md`、`session.md`→`session-format.md`、`tree.md`→`sessions.md`、`process-integration.md`→`cli-integration.md`；`navigation` 3 个顶层组。Rust 侧不需要 docs 体系，但若要保留仓库文档结构，重定向表须一并保留，否则 `documentation.test.ts` 的等价检查会失败。

### A.19 `kiana-test.sh` 的精确事实（修正 §2.22）

脚本共 **85 行**；`--no-env` 分支在第 19–79 行 unset **61 个**凭据变量（不是"40-100 行"这一粗略区间），并从 `ARGS` 中剔除 `--no-env` 本身（该参数不会传给 CLI）。解析器以 file URL 传入（注释明确：裸路径在含 `#`/`?`/`%` 时会失效），最后一行是 `node --import "$RESOLVER_URL" .../src/cli.ts "${ARGS[@]}"`。§2.22 已就地更新为精确描述。
