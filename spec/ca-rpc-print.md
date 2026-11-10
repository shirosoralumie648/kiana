# ca-rpc-print 单元移植规格（TypeScript -> Rust）

本文件是 `packages/coding-agent/src/modes/` 中 **ca-rpc-print** 单元的完整移植规格：
print 模式（纯文本单次输出）、JSON 模式（JSONL 事件流）、RPC 模式（双向 JSONL 服务端）、
严格 JSONL 框架、事件 wire 投影，以及 TypeScript 侧 `RpcClient`（子进程客户端）。

源仓库 `/home/ctyun/code/kiana_rev/kiana` 是只读参考；Rust 实现位于 `/home/ctyun/code/kiana_rev/kiana`。

- 源包：`packages/coding-agent`，名称 `@earendil-works/kiana-coding-agent`，版本 `1.4.0`，
  ESM（`"type": "module"`），`engines.node >= 22.19.0`。
- 本单元文件范围（全部逐行读过，行数为源文件实际行数）：

| 文件 | 行数 | 职责 |
|---|---|---|
| `src/modes/index.ts` | 16 | modes 子树的公共再导出入口 |
| `src/modes/json-event.ts` | 61 | `AgentSessionEvent` -> wire 事件投影（去累积快照） |
| `src/modes/print-mode.ts` | 173 | print / json 单次运行模式 |
| `src/modes/rpc/jsonl.ts` | 58 | 严格 JSONL 序列化与行读取 |
| `src/modes/rpc/rpc-types.ts` | 303 | RPC 协议类型（命令 / 响应 / UI / 状态） |
| `src/modes/rpc/rpc-mode.ts` | 819 | RPC 服务端（stdin 命令、stdout 记录、UI 子协议、关停） |
| `src/modes/rpc/rpc-client.ts` | 617 | `RpcClient`：spawn 子进程 + 类型化命令 API |

- 相关文档（只读校验依据）：`docs/rpc.md`、`docs/rpc-commands.md`、`docs/json.md`、
  `docs/cli-integration.md`、`docs/rpc-extension-ui.md`、`docs/cli.md`、`docs/environment-variables.md`、
  `docs/session-format.md`、`docs/message-types.md`。
- **不在本单元范围、但本单元直接调用**（契约在第 5 节逐字记录，实现由对应单元负责）：
  `core/agent-session.ts`（`AgentSession`）、`core/agent-session-runtime.ts`（`AgentSessionRuntime`）、
  `core/output-guard.ts`（stdout 接管，归 ca-session）、`core/session-manager.ts`（归 ca-session）、
  `core/extensions/**`（`ExtensionRunner` / `ExtensionUIContext`，归 ca-extensions）、
  `core/model-runtime.ts`（归 ca-config-models）、`core/resource-loader.ts` / `core/skills.ts` /
  `core/prompt-templates.ts`（归 ca-resources）、`core/bash-executor.ts`（归 ca-session）、
  `core/compaction/**`（归 ca-tools-compaction-export）、`core/source-info.ts`（归 ca-resources）、
  `src/utils/shell.ts`（`killTrackedDetachedChildren`）、`src/cli/args.ts` 与 `src/main.ts`（归 ca-cli）、
  `packages/agent`（`@earendil-works/kiana-agent-core`，归 agent-codemode）、
  `packages/ai`（`@earendil-works/kiana-ai`，归 ai-core / ai-api-*）。
- 本单元同时是 (a) Rust 移植的验收清单（逐条可测），(b) Rust 侧设计说明。

## 目录

- 0. 单元边界与记号约定
- 1. 概览（目的、依赖图、四种模式的入口、关键不变量）
- 2. 逐文件规格
  - 2.1 `src/modes/index.ts`
  - 2.2 `src/modes/json-event.ts`
  - 2.3 `src/modes/print-mode.ts`
  - 2.4 `src/modes/rpc/jsonl.ts`
  - 2.5 `src/modes/rpc/rpc-types.ts`
  - 2.6 `src/modes/rpc/rpc-mode.ts`
  - 2.7 `src/modes/rpc/rpc-client.ts`
  - 2.8 直接决定本单元行为的邻居：`core/output-guard.ts` 契约
  - 2.9 导出符号计数
- 3. 用户可见表面（逐字）
- 4. 外部格式（逐字）
- 5. 跨单元接口
- 6. npm 依赖与 Rust crate 映射
- 7. 测试清单
- 8. Rust 模块布局与最难复刻部分
- 附录 A：源码索引（逐行锚点）
- 附录 B：`RpcCommand` 33 个类型清单
- 附录 C：`AssistantMessageEvent` -> wire 速查
- 附录 D：关键常量与逐字字面量

---

## 0. 单元边界与记号约定

### 0.1 记号

- **【逐字】** 标记的字符串必须逐字节一致（含空格、标点、大小写、换行）。
- `${expr}` 表示 JS 模板字符串插值；Rust 侧对应 `format!`。插值内容按 §4.2 的
  `JSON.stringify` 语义或 `String(...)` 语义产生。
- `\n` = LF (U+000A)，`\r` = CR (U+000D)，`\t` = TAB (U+0009)。
- “可选键”指 TypeScript 里 `key?: T`；在 JSON 输出中，**值为 `undefined` 的键会被
  `JSON.stringify` 整个丢弃**（见 §4.2）。因此“字段可选”与“字段值为 undefined”在 wire 上等价。
- “顺序”一律指**对象键的插入顺序**；`JSON.stringify` 保留插入顺序（整数型键例外，见 §4.2）。
- “同步 / 异步”按 JS 语义描述（microtask / task 队列）。Rust 侧用 tokio task 与 `select!` 近似，
  详见 §8.3。

### 0.2 本单元负责 vs 不负责

本单元**必须**在 Rust 中完整复刻：

1. print 模式的文本输出、错误输出、退出码、信号处理、资源释放顺序。
2. JSON 模式的会话头 + 事件流 + 退出码规则（注意：JSON 模式下 assistant 失败**不**导致非零退出码）。
3. RPC 服务端的全部 33 个命令、响应编码、错误消息、prompt 预检响应语义、`agent_settled` 驱动的关停。
4. RPC 扩展 UI 子协议（9 种 request、3 种 response、dialog 的 timeout/signal 语义）。
5. 严格 JSONL 框架（LF-only、单 CR 剥离、U+2028/U+2029 保留、UTF-8 增量解码）。
6. `toJsonEvent` 投影（去 `message`、去 `partial`、`toolcall_start` 追加 `id`/`toolName`、顶层 `usage`）。
7. `RpcClient`（Node/TS 侧 API）。Rust 侧应提供等价的 `rpc` 客户端模块（`ca-rpc-client` 或
   `kiana-rpc-client`），行为、错误消息、超时逐字一致。

本单元**不**负责：CLI 参数解析、模式选择（ca-cli）、`AgentSession` 内部（ca-session）、
扩展运行器语义（ca-extensions）、TUI（ca-interactive-mode / tui-*）、模型目录与认证（ca-config-models / ai-auth）。

### 0.3 阅读与验证建议

1. 先读 §1.4 的 12 条不变量——它们是最容易在 Rust 里“看起来对、实际不同”的地方。
2. §2.6.7 是 RPC 命令的逐条表，实现时建议直接按该表写 `match` 分支并逐条对拍。
3. §4.2 / §4.3 是 JS 运行时语义（`JSON.stringify` / `JSON.parse` 错误文本），必须桥接。
4. §8.4 按风险排序列出最难项；前 5 项建议先写黄金测试再写实现。

---

## 1. 概览

### 1.1 目的

`modes/` 把同一个 `AgentSession` 内核包装成三种非交互运行形态：

- **print**（`-p` / `--print` / 非 TTY）：跑完给定提示，把最后一条 assistant 文本写到 stdout，退出。
- **json**（`--mode json`）：先写会话头，再把每个 `AgentSessionEvent` 按 wire 投影写成 JSONL，退出。
- **rpc**（`--mode rpc`）：长生命周期进程；stdin 收 JSONL 命令，stdout 写 `response` + 事件 +
  `extension_ui_request`，支持扩展 UI 子协议与优雅关停。

`RpcClient` 是 TS 侧的客户端封装（spawn `node <cliPath> --mode rpc ...`），供示例、测试与 SDK 用户使用。

### 1.2 依赖图（本单元在系统中的位置）

```
main.ts (ca-cli)
  ├─ resolveAppMode / toPrintOutputMode 决定模式与 print 输出形态
  ├─ takeOverStdout()（非 interactive 且非“纯元数据命令”时；ca-session 提供）
  ├─ runRpcMode(runtime)    -> 永不返回（Promise<never>）
  └─ runPrintMode(runtime, {mode, messages, initialMessage, initialImages}) -> Promise<number>
                                                                    │
        ┌───────────────────────────────────────────────────────────┤
        ▼                                                           ▼
  json-event.toJsonEvent                                   output-guard
  rpc/jsonl (serializeJsonLine / attachJsonlLineReader)    ├ writeRawStdout
  rpc/rpc-types (协议类型)                                  ├ waitForRawStdoutBackpressure
                                                           ├ flushRawStdout
                                                           └ takeOverStdout
        │
        ▼
  AgentSessionRuntime  ->  AgentSession  ->  Agent（packages/agent）
        │                     ├ bindExtensions(ExtensionBindings)  -> ExtensionRunner
        │                     ├ subscribe(AgentSessionEventListener)
        │                     └ 各 getter / 命令方法（见 §5.1）
        ▼
  SessionManager（getHeader/getEntries/getTree/getLeafId/getCwd）
```

要点：

- **只有 RPC 模式调用 `takeOverStdout()`**（在自身入口再调一次，`takeOverStdout` 幂等）；
  print/json 模式依赖 `main.ts` 在进入前已接管 stdout。Rust 侧必须保持“谁接管”由 CLI 决定，
  但 runRpcMode 的入口调用可安全重复。
- print/json 模式**不提供** `uiContext`，因此扩展的 `ctx.hasUI === false`、`ctx.mode` 为
  `"print"` / `"json"`。RPC 模式提供完整的 `ExtensionUIContext`，`ctx.hasUI === true`、`ctx.mode === "rpc"`。
- print/json 模式**不提供** `shutdownHandler` 与 `abortHandler`；RPC 模式提供 `shutdownHandler`。

### 1.3 四种模式的入口与选择规则（ca-cli 计算，本单元消费）

`src/main.ts`：

```ts
function resolveAppMode(parsed: Args, stdinIsTTY: boolean, stdoutIsTTY: boolean): AppMode {
	if (parsed.mode === "rpc") return "rpc";
	if (parsed.mode === "json") return "json";
	if (parsed.print || !stdinIsTTY || !stdoutIsTTY) return "print";
	return "interactive";
}
function toPrintOutputMode(appMode: AppMode): Exclude<Mode, "rpc"> {
	return appMode === "json" ? "json" : "text";
}
```

补充规则（按代码顺序，影响本单元输入）：

1. `--mode` 只接受 `text` / `json` / `rpc`（`args.ts` 校验）：
   - 缺值或以 `-` 开头：【逐字】`--mode requires text, json, or rpc`（diagnostic，不消费下一个参数）；
   - 值非法：【逐字】`Invalid mode "${mode}". Valid values: text, json, rpc`。
   两者都以 `Error: ` 前缀输出到 stderr 并计入启动诊断（ca-cli 决定是否 `exit(1)`）。
2. `--mode rpc` 且存在 `@file` 参数时：`main.ts` 在进入 RPC 前打印
   【逐字】`Error: @file arguments are not supported in RPC mode`（chalk.red）并 `exit(1)`。
3. `readPipedStdin()` 仅在 `appMode !== "rpc"` 时执行；若读到了管道 stdin 且原为 interactive，
   则把 `appMode` 改成 `print`。
4. `prepareInitialMessage(parsed, stdinContent)` 产出 `{initialMessage, initialImages}`；
   `parsed.messages` 是其余位置参数。二者都传给 `runPrintMode`。
5. 非 interactive 且 `!session.model` 时（RPC 也适用）：`main.ts` 打印
   `chalk.red(formatNoModelsAvailableMessage())` 并 `exit(1)`；RPC 模式不会进入 `runRpcMode`。
6. RPC 模式下 `main.ts` 还会：`printTimings()`；未设 `KIANA_OFFLINE`（truthy 判定见 §4.5 的 `isTruthyEnvFlag`）时后台
   `modelRuntime.refresh({signal})`（15s 超时、错误吞掉）。
7. print 模式返回后：`stopThemeWatcher(); restoreStdout(); if (exitCode !== 0) process.exitCode = exitCode;`
   （`main.ts:988`–`996`；**不是** `process.exit`，让 Node 自然退出以冲刷 stderr）。
8. **`shouldTakeOverStdout`（旧版本未记录）**：`appMode !== "interactive" && !isPlainRuntimeMetadataCommand(parsed)`
   时 `main.ts` 调 `takeOverStdout()`（`main.ts:654`–`658`）。其中
   `isPlainRuntimeMetadataCommand(parsed) = !parsed.print && parsed.mode === undefined &&
   (parsed.help === true || parsed.listModels !== undefined)`（`main.ts:129`–`131`）。
   因此 `kiana --help` 与 `kiana --list-models`（未显式指定 mode）**不**接管 stdout；
   `kiana --mode json --help --approve` 仍**接管**（`mode !== undefined`）——这正是
   `stdout-cleanliness.test.ts` 的断言来源（stdout 必须为空、Help 文本进 stderr）。
9. **`--offline` / `KIANA_OFFLINE` 的回写（旧版本未记录）**：`offlineMode = args.includes("--offline") ||
   isTruthyEnvFlag(process.env.KIANA_OFFLINE)`；为真时立刻写
   `process.env.KIANA_OFFLINE = "1"` 与 `process.env.KIANA_SKIP_VERSION_CHECK = "1"`（`main.ts:579`–`583`）。
   该分支在任何子命令/模式判定**之前**执行，影响所有下游（含模型目录刷新与版本检查）。
10. **`--export`（旧版本未记录）**：`parsed.export` 存在时 `main.ts` 走 `exportFromFile(...)`，
    `console.log("Exported to: " + path)` 后 `process.exit(0)`（失败 `Error: <msg>` + exit 1），
   **完全不进入** `resolveAppMode`/本单元。`--export` 由 `args.ts:184` 解析（需要值，
   后一个位置参数作为输出路径）。
11. **启动诊断与退出码（与本单元交互）**：`hasRuntimeErrors`（runtime diagnostics 里有
   `type === "error"`，例如扩展加载失败）时，非 interactive 会 `reportDiagnostics` 后
   `process.exit(1)`；其中若消息含 `Failed to load extension` 还会先打 `EXTENSION_LOAD_FAILURE_HINT`。
   RPC/print 因此**可能根本到不了** `runRpcMode`/`runPrintMode`。
12. **`KIANA_STARTUP_BENCHMARK`**：`isTruthyEnvFlag` 为真且 `appMode !== "interactive"` ->
   `console.error(chalk.red("Error: KIANA_STARTUP_BENCHMARK only supports interactive mode"))` + `exit(1)`。
13. **`RpcClient` 测试的 CLI 参数**：`test/rpc.test.ts` 用 `cliPath = <pkg>/dist/cli.js`、`cwd = <pkg>`、
   `env: { KIANA_CODING_AGENT_DIR: <tmp> }`、`provider: "anthropic"`、`model: "claude-sonnet-4-5"`，
   没有 `args`；因此 `RpcClient` 会拼出 `node <pkg>/dist/cli.js --mode rpc --provider anthropic --model claude-sonnet-4-5`。
   真实跑该测试需要先构建 `dist/`（当前 checkout 未见 `dist/`）。

### 1.4 关键不变量（必须在 Rust 中逐条成立）

1. **print 模式的 stdout 只有“最后一条 assistant 的 text 块”**，每个 text 块后追加一个 `\n`；
   不使用 `console.log`（那会被拦截到 stderr），而是 `writeRawStdout`。没有 text 块则不写任何内容。
2. **print 模式文本分支的退出码**：`stopReason` 为 `"error"` 或 `"aborted"` 时把
   `errorMessage || \`Request ${stopReason}\`` 打到 stderr 并返回 1；`"length"` 时**先输出已有文本**，
   再打 stderr 警告【逐字】`Response was truncated: the model reached its output token limit.` 并返回 1；
   其它情况返回 0。JSON 模式**完全不看** stopReason，正常完成即返回 0。
3. **JSON 模式的会话头只写一次，且在 `rebindSession()` 之前**：`session.sessionManager.getHeader()`
   为 falsy（`null`/`undefined`）时跳过（内存会话仍返回 header 对象——见 §5.1.4）。
4. **JSONL 框架是 LF-only**：`serializeJsonLine(v) = JSON.stringify(v) + "\n"`；读入侧只按 `\n` 切分，
   每条记录剥离**一个**尾随 `\r`；U+2028 / U+2029 必须原样保留在字符串内（`JSON.stringify` 不转义它们）。
5. **`message_update` 投影是破坏性去累积**：wire 记录**没有** `message` 键，嵌套事件**没有** `partial` 键；
   但顶层保留 `usage`（累积值），`toolcall_start` 保留 `contentIndex` 并追加 `id`、`toolName`。
   其它事件对象**原样返回（同一引用、同一键序）**。
6. **RPC 的 prompt 响应是“预检后唯一响应”**：成功路径只有一次 `response`，由
   `prompt({preflightResult})` 回调触发；预检失败（如无 API key）才走 `.catch` 且仅在
   `preflightSucceeded === false` 时输出错误响应。接受后的失败只出现在事件流。
7. **`response` 记录形状固定**：`{id?, type:"response", command, success, data?}` 或
   `{id?, type:"response", command, success:false, error}`；`data === undefined` 时**不出现** `data` 键，
   `data === null` 时出现 `"data":null`（`cycle_model` / `cycle_thinking_level` 的“无结果”）。
8. **id 透传与缺省**：命令的 `id` 原样回填；解析失败与 `extension_ui_response` 路径不带 id
   （`id: undefined` 在 JSON 中被丢弃）；未知命令必须保留 id（回归 #5868）。
9. **RPC 关停由四处触发**：stdin `end`（exit 0）、收到 SIGTERM（exit 143，**不** flush raw stdout）、
   SIGHUP（非 win32，exit 129，flush）、扩展 `shutdownHandler`（延迟到 `agent_settled` 或当前命令处理完后 exit 0）。
   `shutdown` 幂等：重入直接 `process.exit(exitCode)`。
10. **stdout 写入串行化**：所有协议字节经 `writeRawStdout`（promise 链 + ENOBUFS/EAGAIN/EWOULDBLOCK 重试 10ms，
    其它写入错误 -> `process.exit(1)`）；`waitForRawStdoutBackpressure()` 自旋直到链尾稳定；
    `flushRawStdout()` = 等待 + 写空串（空串写入被 `writeRawStdout` 直接忽略，见 §2.8）。
11. **`RpcClient` 的三重失败面**：进程退出（`Agent process exited (code=${code} signal=${signal}). Stderr: ${stderr}`）、
    进程错误（`Agent process error: ${msg}. Stderr: ${stderr}`）、stdin 错误
    （`Agent process stdin error: ${msg}. Stderr: ${stderr}`）；单个请求 30s 超时
    （`Timeout waiting for response to ${type}. Stderr: ${stderr}`）；`waitForIdle` / `collectEvents` 默认 60s 超时。
12. **所有退出路径都必须 `await runtimeHost.dispose()`**（触发 `session_shutdown{reason:"quit"}`），
    再 flush / exit；print 模式用 `disposed` 标志保证只 dispose 一次（信号 + finally 竞争）。

---
## 2. 逐文件规格

### 2.1 `src/modes/index.ts`（16 行）

纯再导出，无运行时代码。导出清单（`exportedSymbolCount` 统计依据，共 **15** 个名字，
与 §2.9 的 15 一致；早先版本误写为 14）：

| 名字 | 类别 | 来源 |
|---|---|---|
| `InteractiveMode` | 值（class） | `./interactive/interactive-mode.ts`（ca-interactive-mode 单元） |
| `InteractiveModeOptions` | 类型 | 同上 |
| `JsonAgentSessionEvent` | 类型 | `./json-event.ts` |
| `PrintModeOptions` | 类型 | `./print-mode.ts` |
| `runPrintMode` | 值（函数） | `./print-mode.ts` |
| `ModelInfo` | 类型 | `./rpc/rpc-client.ts` |
| `RpcClient` | 值（class） | `./rpc/rpc-client.ts` |
| `RpcClientOptions` | 类型 | `./rpc/rpc-client.ts` |
| `RpcEventListener` | 类型 | `./rpc/rpc-client.ts` |
| `runRpcMode` | 值（函数） | `./rpc/rpc-mode.ts` |
| `RpcCommand` | 类型 | `./rpc/rpc-types.ts` |
| `RpcExtensionUIRequest` | 类型 | 同上 |
| `RpcExtensionUIResponse` | 类型 | 同上 |
| `RpcResponse` | 类型 | 同上 |
| `RpcSessionState` | 类型 | 同上 |

注意：`RpcSlashCommand`、`RpcCommandType` **不**从 `modes/index.ts` 再导出（只从
`rpc/rpc-types.ts` 导出）。`rpc-mode.ts` 自身再导出 5 个类型
（`RpcCommand`、`RpcExtensionUIRequest`、`RpcExtensionUIResponse`、`RpcResponse`、`RpcSessionState`）。
**更正**：旧版本这句话写成“只从 `rpc/rpc-types.ts` 与 `rpc/rpc-mode.ts` 导出”，容易被读成
`rpc-mode.ts` 也导出 `RpcSlashCommand`/`RpcCommandType`——它**不**导出（`rpc-mode.ts` 只有
`export type { RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse,
RpcSessionState }` 与 `export async function runRpcMode`）。

**依赖副作用（旧版本未记录）**：`modes/index.ts` 顶部有
`export { InteractiveMode, type InteractiveModeOptions } from "./interactive/interactive-mode.ts";`
——这是**值**再导出，因此任何 import `modes/index.ts` 的代码（包括只用 `RpcClient` 的客户端）
都会在模块图里传递依赖整个交互模式/TUI 子树。Rust 侧若按 crate 拆分，应把该再导出放到
feature gate（如 `interactive`）或独立 crate，避免 RPC 二进制链入 TUI。

Rust 侧对应：`ca_rpc_print`（或 `kiana_modes`）crate 的 `lib.rs` 顶层 `pub use`，
类型/函数名与之对齐（见 §8.1）。

### 2.2 `src/modes/json-event.ts`（61 行）

#### 2.2.1 类型（无运行时）

```ts
type WithoutPartial<T> = T extends { partial: unknown } ? Omit<T, "partial"> : T;

type ToJsonAssistantMessageEvent<T> = T extends { type: "toolcall_start"; partial: unknown }
	? WithoutPartial<T> & { id: string; toolName: string }
	: WithoutPartial<T>;

type MessageUpdateEvent = Extract<AgentSessionEvent, { type: "message_update" }>;

type JsonMessageUpdateEvent = {
	type: "message_update";
	usage: Usage;
	assistantMessageEvent: ToJsonAssistantMessageEvent<MessageUpdateEvent["assistantMessageEvent"]>;
};

export type JsonAgentSessionEvent =
	| Exclude<AgentSessionEvent, { type: "message_update" }>
	| JsonMessageUpdateEvent;
```

语义：`JsonAgentSessionEvent` = 除 `message_update` 外的所有 `AgentSessionEvent` 原样，
加上被投影过的 `message_update`。`AgentSessionEvent` 定义见 §5.1.6（含 `parentToolCallId`
对 `tool_execution_*` 的扩展）。

#### 2.2.2 `toJsonEvent(event): JsonAgentSessionEvent`（重载 2 个 + 实现）

签名：

```ts
export function toJsonEvent(event: MessageUpdateEvent): JsonMessageUpdateEvent;
export function toJsonEvent(event: AgentSessionEvent): JsonAgentSessionEvent;
```

实现步骤（严格顺序）：

1. `if (event.type !== "message_update") return event;` —— **返回同一个对象引用**，不复制、不重建。
   Rust 侧对应“零拷贝直通”：枚举 variant 直接返回。
2. `if (event.message.role !== "assistant") throw new Error("message_update message is not an assistant message");`
   【逐字】`message_update message is not an assistant message`
3. 返回**新对象**，键插入顺序固定为 `type`、`usage`、`assistantMessageEvent`：
   - `type: "message_update"`
   - `usage: event.message.usage`（引用同一 `Usage` 对象，含 `cacheWrite1h?`、`reasoning?`、`cost.totalFromProvider?`）
   - `assistantMessageEvent: toJsonAssistantMessageEvent(event.assistantMessageEvent)`
   注意：`event` 上其余键（如 `message`）**全部丢弃**。

#### 2.2.3 `toJsonAssistantMessageEvent(event)`（内部函数）

分三种情况：

**(a) `event.type === "toolcall_start"`**

```ts
const toolCall = event.partial.content[event.contentIndex];
if (toolCall?.type !== "toolCall") {
	throw new Error(`toolcall_start content at index ${event.contentIndex} is not a tool call`);
}
const { partial: _partial, ...deltaEvent } = event;
return { ...deltaEvent, id: toolCall.id, toolName: toolCall.name };
```

- 索引语义：`contentIndex` 是 **JS 数组下标**（number）；越界得到 `undefined` -> 抛错。
- 错误文本【逐字】：`toolcall_start content at index ${contentIndex} is not a tool call`
  （`contentIndex` 用 `String(number)` 格式化，整数不带小数点）。
- 输出键插入顺序：先 `deltaEvent` 的剩余键（原事件解构后的顺序，即 `type`、`contentIndex`，
  外加类型上可能存在的其它键），再追加 `id`、`toolName`。因此 wire 上典型形态是
  `{"type":"toolcall_start","contentIndex":0,"id":"call_x","toolName":"write"}`。
- `partial` 被移除；`partial.content` 只用于取 `id`/`name`。

**(b) 事件对象**没有** `partial` 键**（`!("partial" in event)`）：**原样返回同一引用**。
这覆盖 `text_start`、`text_delta`（无 partial 的变体）、`done`、`error` 等。

**(c) 其余（有 `partial`）**：解构去掉 `partial`，返回浅拷贝（键序 = 原键序去掉 `partial`）。

> 关键细节：JS 的 `in` 判定针对**自有或继承属性是否存在**，而不是值是否为 undefined。
> 在 Rust 实现中，应把 `partial` 建模为 `Option<Value>` 且在序列化时跳过；
> “不存在 `partial` 键”与“`partial: None`”在 wire 上等价，因此 Rust 可统一按 (c) 处理，
> 唯一需要对齐的是**键顺序**（`partial` 位于原位置，删除后其余键相对顺序不变）。

#### 2.2.4 与文档的一致性

`docs/json.md:205`–`218` 的 `JsonAssistantMessageEvent<T>` 与 `JsonAgentSessionEvent` 与此逐字一致
（文档导出的类型名是 `JsonAssistantMessageEvent`，源码内部名是 `ToJsonAssistantMessageEvent`；
两者结构相同）。文档强调的“wire `message_update` 是 delta-only、大小线性增长”由回归测试 #7290 断言（§7）。
另据 `docs/json.md:91`：正常的 agent loop 会把 provider 级的 `start`/`done`/`error` 转成
`message_start`/`message_end`，**不**作为 `message_update` 发出；但 `JsonAgentSessionEvent` 的类型变换
仍允许调用方构造这类事件（因此 §2.2.3 的 `done`/`error` 分支不是死代码）。

#### 2.2.5 补充边界（审计新增）

- `toJsonEvent` 只对 `message_update` 做角色校验；其余事件**从不**校验（例如 `tool_execution_*`
  的字段缺失会原样透传）。
- `message_update` 投影后的 `usage` 是 `event.message.usage` 的**引用**，不是拷贝；因此如果在
  同一 tick 内对同一 `AssistantMessage` 继续 mutate usage（不会发生——每次 update 都是新对象），
  序列化结果可能变。Rust 侧按值克隆更安全（wire 结果相同）。
- `assistantMessageEvent` 的 `partial` 判定用 `in`：若某变体**自带** `partial: undefined`
  属性（非本仓库产生），`("partial" in event)` 为真 -> 走 (c) 分支并被删除；wire 结果不变。
- `toolcall_start` 的 `id`/`toolName` 来自 `event.partial.content[contentIndex]`，**不是**来自
  `toolCall` 增量本身；`contentIndex` 越界或该位置不是 `toolCall` 都抛错（见上）。
- `text_start` / `thinking_start` / `done` / `error` 等变体在 wire 上保留其原有其它字段
  （`done` 保留 `reason`+`message`；`error` 保留 `reason`+`error`），只删 `partial`。
  这意味着 JSON/RPC 的 `message_update` 里**仍可能**携带完整 `message`（`done`/`error` 内层），
  累计体积主要靠 `partial` 的移除来控制（#7290 的线性断言即针对此）。

#### 2.2.6 错误传播

`toJsonEvent` 的两次 `throw` 会沿调用方向上抛：

- print/json 模式：在 `session.subscribe` 回调中抛出 -> 该回调是同步监听器，异常会冒泡到
  `AgentSession._emit` -> agent 事件处理链；实践中意味着该次运行失败，`runPrintMode` 的 `try`
  捕获后打印错误并返回 1。
- RPC 模式：同样在 `session.subscribe` 回调中抛出；`output(toJsonEvent(event))` 在 `output` 之前求值，
  因此**该事件不会写出**，异常冒泡至 `_emit` 的调用者（agent loop 或 session 内部方法）。

Rust 侧建议：把 `to_json_event` 设计为 `Result<..., JsonEventError>`，在监听器边界记录并
按 TS 语义上抛（或至少保证不 panic，且不写出半条记录）。

### 2.3 `src/modes/print-mode.ts`（173 行）

#### 2.3.1 `PrintModeOptions`

```ts
export interface PrintModeOptions {
	mode: "text" | "json";
	messages?: string[];
	initialMessage?: string;
	initialImages?: ImageContent[];
}
```

- `mode`：`"text"` 最终文本；`"json"` 事件流。**没有** `"rpc"`（由 `toPrintOutputMode` 保证）。
- `messages`：在 `initialMessage` 之后依次 `prompt` 的额外消息；默认 `[]`。
- `initialMessage`：第一条消息（可能含 `@file` 内容或管道 stdin 内容）；为空字符串时**跳过**
  （`if (initialMessage)` 判定，空串 falsy）。
- `initialImages`：只附加到 `initialMessage` 的 `prompt` 的 `{images}` 选项；
  `messages` 里的消息**不带** images。

#### 2.3.2 `runPrintMode(runtimeHost, options): Promise<number>`

局部状态（按声明顺序）：
`exitCode = 0`、`session = runtimeHost.session`、`unsubscribe?`、`unsubscribeBackpressure?`、
`disposed = false`、`signalCleanupHandlers: Array<() => void> = []`。

**(1) 定义 `disposeRuntime()`**

```ts
const disposeRuntime = async (): Promise<void> => {
	if (disposed) return;
	disposed = true;
	unsubscribe?.();
	unsubscribeBackpressure?.();
	await runtimeHost.dispose();
};
```

幂等；先退订（避免 dispose 期间继续写协议），再 `runtimeHost.dispose()`（会 emit
`session_shutdown{reason:"quit"}` 并 `session.dispose()`，见 §5.1.2）。

**(2) 定义并立即调用 `registerSignalHandlers()`**

```ts
const signals: NodeJS.Signals[] = ["SIGTERM"];
if (process.platform !== "win32") signals.push("SIGHUP");
for (const signal of signals) {
	const handler = () => {
		killTrackedDetachedChildren();
		void disposeRuntime().finally(() => {
			process.exit(signal === "SIGHUP" ? 129 : 143);
		});
	};
	process.on(signal, handler);
	signalCleanupHandlers.push(() => process.off(signal, handler));
}
```

- 顺序：SIGTERM 先注册，SIGHUP 后注册（win32 只有 SIGTERM）。
- 处理动作顺序固定：`killTrackedDetachedChildren()`（同步杀掉所有被跟踪的 detached 子进程 PID，
  清空集合）-> dispose（异步）-> `process.exit(129|143)`。
- **不写入任何协议或 stderr 内容**；不做 flush（`process.exit` 立即终止）。
- 注册发生在 `setRebindSession` / `rebindSession` 之前（即 `try` 块外）。

**(3) `runtimeHost.setRebindSession(async () => { await rebindSession(); })`**

回调忽略 `runtimeHost` 传入的 session 参数，改为重新读取 `runtimeHost.session`。

**(4) `rebindSession()`（内部，async）**

```ts
session = runtimeHost.session;
await session.bindExtensions({
	mode: mode === "json" ? "json" : "print",
	commandContextActions: {
		waitForIdle: () => session.waitForIdle(),
		newSession: async (newSessionOptions) => runtimeHost.newSession(newSessionOptions),
		fork: async (entryId, forkOptions) => {
			const result = await runtimeHost.fork(entryId, forkOptions);
			return { cancelled: result.cancelled };
		},
		navigateTree: async (targetId, navigateOptions) => {
			const result = await session.navigateTree(targetId, {
				summarize: navigateOptions?.summarize,
				customInstructions: navigateOptions?.customInstructions,
				replaceInstructions: navigateOptions?.replaceInstructions,
				label: navigateOptions?.label,
			});
			return { cancelled: result.cancelled };
		},
		switchSession: async (sessionPath, switchOptions) => runtimeHost.switchSession(sessionPath, switchOptions),
		reload: async () => { await session.reload(); },
	},
	onError: (err) => {
		console.error(`Extension error (${err.extensionPath}): ${err.error}`);
	},
});

unsubscribe?.();
unsubscribeBackpressure?.();
unsubscribe = session.subscribe((event) => {
	if (mode === "json") {
		writeRawStdout(`${JSON.stringify(toJsonEvent(event))}\n`);
	}
});
unsubscribeBackpressure =
	mode === "json"
		? session.agent.subscribe(async () => { await waitForRawStdoutBackpressure(); })
		: undefined;
```

细节：

- **不传 `uiContext`** -> `ExtensionRunner.setUIContext(undefined, mode)` 使用 `noOpUIContext`，
  `ctx.hasUI === false`。`ctx.mode` 为 `"json"` 或 `"print"`。
- **不传 `shutdownHandler` / `abortHandler`**。若扩展调用 `ctx.shutdown()`，`ExtensionRunner.shutdown()`
  调用的是默认 handler（打印警告——由 ca-extensions 定义），print/json 模式不会因此退出。
- `onError` 消息【逐字】：`Extension error (${err.extensionPath}): ${err.error}`（`console.error`，
  在 stdout 已接管时实际写 stderr；带一个尾随 LF）。
- `navigateTree` 把 `navigateOptions` 的 4 个字段**显式重打包**（`undefined` 也显式传入），
  Rust 侧应保持“字段级透传”语义（避免把 `undefined` 当成“未提供”而改变下游默认值）。
- 先 `await bindExtensions`（内含 emit `session_start`、扩展资源发现），**之后**才重订阅。
  旧订阅在新订阅建立前退订，因此重绑期间事件会丢失（与 TS 一致）。
- JSON 模式额外订阅 `session.agent`（`Agent.subscribe`）用于**背压**：每个 agent 事件后
  `await waitForRawStdoutBackpressure()`。该订阅与 `session.subscribe` 是两条独立链路。
- backpressure 订阅的 listener **忽略事件参数**，仅等待 stdout 链清空。

**(5) `try` 主体（严格顺序）**

```ts
if (mode === "json") {
	const header = session.sessionManager.getHeader();
	if (header) writeRawStdout(`${JSON.stringify(header)}\n`);
}
await rebindSession();
if (initialMessage) await session.prompt(initialMessage, { images: initialImages });
for (const message of messages) await session.prompt(message);
if (mode === "text") { ...见下... }
return exitCode;
```

- 会话头在 `rebindSession()` **之前**写；判定是 `if (header)`（truthiness），因此 `null` **或**
  `undefined` 都跳过。`SessionManager.inMemory()` 也有 header 对象（`getHeader()` 返回对象），
  返回 `null` 的情形见 §5.1.4；测试里 mock 成 `undefined` 也走跳过路径（`print-mode.test.ts`）。
- `initialImages` 作为 `{images: undefined}` 传入也可（`JSON.stringify` 丢弃 undefined，但这里是函数调用）。
- `session.prompt` 串行 `await`；任一 `prompt` 抛出 -> 进入 catch，**后续消息不再发送**。

**(6) 文本输出块（`if (mode === "text")`）**

```ts
const state = session.state;
const lastMessage = state.messages[state.messages.length - 1];
if (lastMessage?.role === "assistant") {
	const assistantMsg = lastMessage as AssistantMessage;
	if (assistantMsg.stopReason === "error" || assistantMsg.stopReason === "aborted") {
		console.error(assistantMsg.errorMessage || `Request ${assistantMsg.stopReason}`);
		exitCode = 1;
	} else {
		for (const content of assistantMsg.content) {
			if (content.type === "text") writeRawStdout(`${content.text}\n`);
		}
		if (assistantMsg.stopReason === "length") {
			console.error("Response was truncated: the model reached its output token limit.");
			exitCode = 1;
		}
	}
}
```

逐条：

1. 取的是 `session.state.messages`（`Agent.state.messages`，**agent 的权威消息数组**）。取最后一条。
   注意：`session.messages` 这个 getter **也不是**“session 投影”——源码是
   `get messages(): AgentMessage[] { return this.agent.state.messages; }`
   （`src/core/agent-session.ts:1652`–`1654`），与 `session.state.messages` 是**同一个数组引用**。
   本文件旧版把 `session.messages` 称作“session 投影”属错误描述（真正的投影是
   `sessionManager.buildSessionProjection()`，只在 `_refreshFinalizedContext` 内部用于改写
   `agent.state.messages`）。对移植无行为影响，但不要据此假设两条不同的数据源。
2. 最后一条**不是** assistant（例如 `bashExecution`、`toolResult`、`user`）或数组为空：什么都不输出，退出码 0。
3. `stopReason` 为 `"error"` / `"aborted"`：只写 stderr，**不写 stdout 文本**（即使 content 里有 text）。
   错误文本 = `errorMessage` 非空则用它，否则【逐字】`Request ${stopReason}`（如 `Request aborted`）。
4. 其它 stopReason（`stop` / `toolUse` / `length` / `pending` 等）：把**每个** `type === "text"` 的
   content 块按 `text + "\n"` 写出；非 text 块（thinking / toolCall / image）跳过。
5. `length`：文本照常输出后，再写 stderr【逐字】
   `Response was truncated: the model reached its output token limit.`，退出码 1。
6. 若最后一条 assistant 的 stopReason 是 `toolUse`（模型以工具调用结束），只输出文本块（通常为空），退出码 0。

**(7) `catch`**

```ts
catch (error: unknown) {
	console.error(error instanceof Error ? error.message : String(error));
	return 1;
}
```

只输出 `error.message`（**不是** stack）；非 Error 用 `String(error)`。JSON 模式下同样适用：
异常时可能已经写出了部分事件，随后 stderr 报错、返回 1。

**(8) `finally`**

```ts
for (const cleanup of signalCleanupHandlers) cleanup();
await disposeRuntime();
await flushRawStdout();
```

顺序固定：先移除信号处理器（避免 dispose 期间被信号打断），再 dispose（emit `session_shutdown`
并向 stderr 输出扩展错误），最后 flush raw stdout（等待写链清空再写空 chunk，确保字节落地）。

#### 2.3.3 观测点清单（写测试时用）

| 场景 | stdout | stderr | 返回值 |
|---|---|---|---|
| text，assistant `stop` 带 1 个 text 块 `done` | `done\n` | — | 0 |
| text，无消息 | — | — | 0 |
| text，最后一条非 assistant | — | — | 0 |
| text，`error` + `errorMessage="provider failure"` | — | `provider failure\n` | 1 |
| text，`aborted` 无 errorMessage | — | `Request aborted\n` | 1 |
| text，`length` 带文本 `partial` | `partial\n` | `Response was truncated: ...\n` | 1 |
| text，prompt 抛错 `boom` | — | `boom\n` | 1 |
| json，任意 | header + 每事件一行；无 header 则只有事件 | 扩展错误 | 0（除非抛错） |
| json，assistant `error` | 含 `message_end` 等事件 | — | 0 |

（`runPrintMode` 的 `console.error` 始终写 stderr（`process.stderr` 从未被接管）；
`console.log` / `process.stdout.write` 在接管后也被重定向到 stderr。Rust 侧固定写 stderr 即可。）

### 2.4 `src/modes/rpc/jsonl.ts`（58 行）

#### 2.4.1 `serializeJsonLine(value: unknown): string`

```ts
return `${JSON.stringify(value)}\n`;
```

- 使用全局 `JSON.stringify`（单参，无缩进），语义见 §4.2。
- 若 `JSON.stringify` 返回 `undefined`（顶层为 `undefined` / 函数 / symbol），模板字符串把它变成
  【逐字】`undefined` + `\n` = `"undefined\n"`。Rust 侧必须复刻（不 panic、不返回 `null`）。
- 输出**只有一个**尾随 `\n`，不追加 `\r`。

#### 2.4.2 `attachJsonlLineReader(stream: Readable, onLine: (line: string) => void): () => void`

```ts
const decoder = new StringDecoder("utf8");
let buffer = "";

const emitLine = (line: string) => {
	onLine(line.endsWith("\r") ? line.slice(0, -1) : line);
};

const onData = (chunk: string | Buffer) => {
	buffer += typeof chunk === "string" ? chunk : decoder.write(chunk);
	while (true) {
		const newlineIndex = buffer.indexOf("\n");
		if (newlineIndex === -1) return;
		emitLine(buffer.slice(0, newlineIndex));
		buffer = buffer.slice(newlineIndex + 1);
	}
};

const onEnd = () => {
	buffer += decoder.end();
	if (buffer.length > 0) {
		emitLine(buffer);
		buffer = "";
	}
};

stream.on("data", onData);
stream.on("end", onEnd);

return () => {
	stream.off("data", onData);
	stream.off("end", onEnd);
};
```

逐条语义：

1. **只在 `\n` 处切分**。`\r`、U+2028、U+2029、其它 Unicode 分隔符都**不是**边界。
2. **单 CR 剥离**：每行末尾若有且仅有一个 `\r`，去掉它（`a\r\r\n` -> `a\r`；`a\r\n` -> `a`）。
3. **不剥离其它空白**；空行会以 `""` 触发 `onLine("")`（RPC 侧会走 `JSON.parse("")` 失败路径）。
4. **UTF-8 增量解码**：`Buffer` 经 `StringDecoder("utf8")`；跨 chunk 截断的多字节序列被保留到下一 chunk。
   非法字节序列每字节产出一个 U+FFFD；`decoder.end()` 对不完整尾序列**再产出一个 U+FFFD**（一次）。
   字符串 chunk 直接拼接（不经过 decoder）。
5. **`end` 时冲刷**：`buffer` 非空才触发最后一行（无尾随 LF 也输出）。空 `buffer` 不触发。
6. **`onLine` 同步调用**（在 data/end 事件处理内）；异常会冒泡到 stream 的 emit（可能成为
   `uncaughtException`）。RPC 侧用 `void handleInputLine(line)` 包住，因此异常不会冒泡。
7. 返回的 detach 只移除这两个监听器；**不**停止流、不 flush decoder、不清 buffer。

#### 2.4.3 Rust 对应

`JsonlReader` 应持有 `Vec<u8>`/`String` 缓冲 + `StringDecoder` 等价物（`encoding_rs` 或手写
UTF-8 增量状态机），提供 `push(&[u8])` / `push_str(&str)` / `finish()`（产出剩余行，含 U+FFFD 语义）/
`detach()` 语义由调用方的 task 取消实现。

---
### 2.5 `src/modes/rpc/rpc-types.ts`（303 行）

纯类型文件，无运行时代码。但每一条都是 wire 契约，Rust 侧必须逐字段（含可选性、联合分支、键序）
建模为 `enum` + `serde` 结构。导出符号 7 个：`RpcCommand`、`RpcSlashCommand`、`RpcSessionState`、
`RpcResponse`、`RpcExtensionUIRequest`、`RpcExtensionUIResponse`、`RpcCommandType`。

#### 2.5.1 `RpcCommand`（stdin，33 个分支）

每个分支都有可选 `id?: string`（第一个键，见下方顺序）。按源码分组：

| 组 | `type` | 其它字段 |
|---|---|---|
| Prompting | `prompt` | `message: string`、`images?: ImageContent[]`、`streamingBehavior?: "steer" \| "followUp"` |
| | `steer` | `message: string`、`images?: ImageContent[]` |
| | `follow_up` | `message: string`、`images?: ImageContent[]` |
| | `abort` | — |
| | `clear_queue` | — |
| | `new_session` | `parentSession?: string` |
| State | `get_state` | — |
| Model | `set_model` | `provider: string`、`modelId: string` |
| | `cycle_model` | — |
| | `get_available_models` | — |
| Thinking | `set_thinking_level` | `level: ThinkingLevel` |
| | `cycle_thinking_level` | — |
| | `get_available_thinking_levels` | — |
| Queue modes | `set_steering_mode` | `mode: "all" \| "one-at-a-time"` |
| | `set_follow_up_mode` | `mode: "all" \| "one-at-a-time"` |
| Compaction | `compact` | `customInstructions?: string` |
| | `set_auto_compaction` | `enabled: boolean` |
| Retry | `set_auto_retry` | `enabled: boolean` |
| | `abort_retry` | — |
| Bash | `bash` | `command: string`、`excludeFromContext?: boolean` |
| | `abort_bash` | — |
| Session | `get_session_stats` | — |
| | `export_html` | `outputPath?: string` |
| | `switch_session` | `sessionPath: string` |
| | `fork` | `entryId: string` |
| | `clone` | — |
| | `get_fork_messages` | — |
| | `get_entries` | `since?: string` |
| | `get_tree` | — |
| | `get_last_assistant_text` | — |
| | `set_session_name` | `name: string` |
| Messages | `get_messages` | — |
| Commands | `get_commands` | — |

数量核对：6 (prompting) + 1 (state) + 3 (model) + 3 (thinking) + 2 (queue) + 2 (compaction)
+ 2 (retry) + 2 (bash) + 10 (session) + 1 (messages) + 1 (commands) = **33** 个命令类型。
（本表按源码逐条枚举，共 33 个 `type` 字面量；`docs/rpc-commands.md` 未给出总数，实现与测试一律以 33 为准。）

`ThinkingLevel` 取值（来自 `packages/agent`）：`"off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"`。
文档约束：`"xhigh"` / `"max"` 只在所选模型支持时暴露（由 `getAvailableThinkingLevels` 决定）；
`set_thinking_level` 不做取值校验（直接交给 `session.setThinkingLevel`，见 §5.1.1）。

`RpcCommandType = RpcCommand["type"]`：上述 33 个字面量的联合。

**反序列化契约**：

- 命令对象是 `JSON.parse` 的产物，**没有字段校验**。缺失必需字段时，TS 的字段访问得到 `undefined`，
  随后传给下层方法（如 `session.prompt(undefined)` -> `undefined.startsWith` 之类抛 TypeError），
  由 `handleInputLine` 的 catch 转成 `error(id, command.type, commandError.message)`。
- 未知 `type`：走 `default` 分支产出 `Unknown command: ${type}`（保留 id，见 §2.6.10）。
- 顶层不是对象（数字、字符串、数组）：`extension_ui_response` 检测失败后当作 `RpcCommand`；
  `command.id` 与 `command.type` 均为 `undefined`（原始值装箱后属性不存在，不抛错）->
  `handleCommand` 的 `switch (undefined)` 落到 `default` -> `error(undefined, undefined, "Unknown command: undefined")`，
  `id` 与 `command` 键被 `JSON.stringify` 丢弃，实际写出
  `{"type":"response","success":false,"error":"Unknown command: undefined"}`。
- 顶层为 `null`：`handleCommand` 第一行 `const id = command.id` 抛
  `TypeError: Cannot read properties of null (reading 'id')`；该异常被 `handleInputLine` 的 `catch` 捕获，
  但 catch 块里再次访问 `command.id` **又抛一次**，于是 `handleInputLine` 的 Promise reject；
  因为调用处是 `void handleInputLine(line)`，结果是 **unhandled rejection，无任何记录写出**
  （Node 默认行为：打印警告到 stderr 并继续/按版本退出）。实测确认（Node v26.12.1）：
  首抛消息为 `Cannot read properties of null (reading 'id')`。
  Rust 侧应复刻“无输出”的结果（不 panic；可选：写一条 stderr 诊断，但这会与 TS 的 stderr 文本不同，
  建议只在日志级别记录）。见 §8.4 第 7 项。

`ImageContent`（`packages/ai`）：`{ type: "image"; data: string; mimeType: string }`，
键序 `type`、`data`、`mimeType`；`data` 为 base64（无 `data:` 前缀），`mimeType` 如 `"image/png"`。
RPC 侧 `images` 原样透传给 `session.prompt` / `steer` / `followUp`，不校验、不缩放。

#### 2.5.2 `RpcSlashCommand`

```ts
export interface RpcSlashCommand {
	name: string;
	description?: string;
	source: "extension" | "prompt" | "skill";
	sourceInfo: SourceInfo;
}
```

`SourceInfo`（`core/source-info.ts`）：

```ts
export type SourceScope = "user" | "project" | "temporary";
export type SourceOrigin = "package" | "top-level";
export interface SourceInfo {
	path: string;
	source: string;
	scope: SourceScope;
	origin: SourceOrigin;
	baseDir?: string;
}
```

键序：`path`、`source`、`scope`、`origin`、`baseDir?`（`createSourceInfo` / `createSyntheticSourceInfo`
的返回顺序；`baseDir` 为 `undefined` 时被序列化丢弃）。Rust 侧必须保持该顺序。

#### 2.5.3 `RpcSessionState`

```ts
export interface RpcSessionState {
	model?: Model<any>;
	thinkingLevel: ThinkingLevel;
	isStreaming: boolean;
	isCompacting: boolean;
	steeringMode: "all" | "one-at-a-time";
	followUpMode: "all" | "one-at-a-time";
	sessionFile?: string;
	sessionId: string;
	sessionName?: string;
	autoCompactionEnabled: boolean;
	messageCount: number;
	pendingMessageCount: number;
}
```

键序 = 声明序（`rpc-mode.ts` 的构造字面量同序，见 §2.6.7 `get_state`）。
`model` / `sessionFile` / `sessionName` 为 `undefined` 时**键缺失**。

#### 2.5.4 `RpcResponse`（stdout，33 个成功分支 + 1 个通用错误分支）

通用形状：`{id?, type:"response", command, success, data?}`（成功）/
`{id?, type:"response", command, success:false, error}`（失败）。逐命令 `data` 形态：

| `command` | `data` |
|---|---|
| `prompt` | `{ disposition: PromptDisposition }`，`PromptDisposition = "handled" \| "queued" \| "started"` |
| `steer` | `{ disposition: QueuedInputDisposition }`（`"handled" \| "queued"`） |
| `follow_up` | 同上 |
| `abort` | 无（`data === undefined` -> 无 `data` 键） |
| `clear_queue` | `{ steering: string[]; followUp: string[] }` |
| `new_session` | `{ cancelled: boolean }` |
| `get_state` | `RpcSessionState` |
| `set_model` | `Model<any>`（完整模型对象） |
| `cycle_model` | `{ model: Model<any>; thinkingLevel: ThinkingLevel; isScoped: boolean } \| null` |
| `get_available_models` | `{ models: Model<any>[] }` |
| `set_thinking_level` | 无 |
| `cycle_thinking_level` | `{ level: ThinkingLevel } \| null` |
| `get_available_thinking_levels` | `{ levels: ThinkingLevel[] }` |
| `set_steering_mode` / `set_follow_up_mode` | 无 |
| `compact` | `CompactionResult` |
| `set_auto_compaction` / `set_auto_retry` / `abort_retry` | 无 |
| `bash` | `BashResult` |
| `abort_bash` | 无 |
| `get_session_stats` | `SessionStats` |
| `export_html` | `{ path: string }` |
| `switch_session` | `{ cancelled: boolean }` |
| `fork` | `{ text?: string; cancelled: boolean }`（`selectedText` 为 `undefined` 时 `text` 键缺失，见下） |
| `clone` | `{ cancelled: boolean }` |
| `get_fork_messages` | `{ messages: Array<{ entryId: string; text: string }> }` |
| `get_entries` | `{ entries: SessionEntry[]; leafId: string \| null }` |
| `get_tree` | `{ tree: SessionTreeNode[]; leafId: string \| null }` |
| `get_last_assistant_text` | `{ text?: string }`（实现为 `string \| undefined`；无文本时 `data` 为 `{}`，详见 §2.6.7） |
| `set_session_name` | 无 |
| `get_messages` | `{ messages: AgentMessage[] }` |
| `get_commands` | `{ commands: RpcSlashCommand[] }` |
| 任意 | `{ id?, type:"response", command: string, success: false, error: string }` |

`fork` 的 `text` 键在 TS 类型里是必需 `string`，但运行时若 `result.selectedText` 为 `undefined`
（被扩展取消时 `runtimeHost.fork` 返回 `{cancelled:true}` 且无 `selectedText`），
`JSON.stringify` 会**丢弃** `text` 键 -> 实际写出 `{"cancelled":true}`。文档示例正是如此。Rust 侧应
用 `Option<String>` 并在 `None` 时跳过键。

`BashResult`（`core/bash-executor.ts`）：键序 `output`、`exitCode`、`cancelled`、`truncated`、`fullOutputPath?`；
`exitCode` 为 `undefined`（被信号杀死/取消）时键缺失。

`CompactionResult`（`core/compaction/compaction.ts`）：键序 `summary`、`firstKeptEntryId`、`tokensBefore`、
`estimatedTokensAfter?`、`usage?`、`details?`。

`SessionStats`（`core/agent-session.ts`）：键序 `sessionFile`（`string|undefined`）、`sessionId`、
`userMessages`、`assistantMessages`、`toolCalls`、`toolResults`、`totalMessages`、`tokens`
（`{input,output,cacheRead,cacheWrite,total}`）、`cost`（number）、`contextUsage?`
（`{tokens: number|null, contextWindow: number, percent: number|null}`）。

#### 2.5.5 `RpcExtensionUIRequest`（stdout，9 种 method）

共同前缀：`{ type: "extension_ui_request", id: string, method }`，随后按 method：

| method | 附加字段（按插入顺序） |
|---|---|
| `select` | `title: string`、`options: string[]`、`timeout?: number` |
| `confirm` | `title: string`、`message: string`、`timeout?: number` |
| `input` | `title: string`、`placeholder?: string`、`timeout?: number` |
| `editor` | `title: string`、`prefill?: string`（**无** timeout） |
| `notify` | `message: string`、`notifyType?: "info" \| "warning" \| "error"` |
| `setStatus` | `statusKey: string`、`statusText: string \| undefined` |
| `setWidget` | `widgetKey: string`、`widgetLines: string[] \| undefined`、`widgetPlacement?: "aboveEditor" \| "belowEditor"` |
| `setTitle` | `title: string` |
| `set_editor_text` | `text: string` |

实际 `output` 调用是 `{ type, id, ...request }`（§2.6.2），因此键序为
`type`、`id`、`method`、其余。`statusText` / `widgetLines` 值为 `undefined` 时键缺失（但仍**发出**请求）。
`widgetPlacement` 为 `undefined` 时键缺失。

#### 2.5.6 `RpcExtensionUIResponse`（stdin，3 种）

```ts
| { type: "extension_ui_response"; id: string; value: string }
| { type: "extension_ui_response"; id: string; confirmed: boolean }
| { type: "extension_ui_response"; id: string; cancelled: true };
```

分发规则（§2.6.11）：`parsed.type === "extension_ui_response"` 时按 `response.id` 查
`pendingExtensionRequests`；找到则删除并 `resolve(response)`（**原样传整个对象**）；找不到则静默忽略
（无响应记录、无日志）。**不校验** `value` / `confirmed` / `cancelled` 是否存在——由 `parseResponse` 决定：
先看 `"cancelled" in r && r.cancelled`，再看 `"value" in r` / `"confirmed" in r`，否则默认值。

### 2.6 `src/modes/rpc/rpc-mode.ts`（819 行）

#### 2.6.1 导出

```ts
export type { RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse, RpcSessionState } from "./rpc-types.ts";
export async function runRpcMode(runtimeHost: AgentSessionRuntime): Promise<never>;
```

`runRpcMode` 声明返回 `Promise<never>`：正常路径返回 `new Promise(() => {})`（永不 resolve），
所有退出都经 `process.exit`。

导入依赖（Rust 侧需要的邻居能力）：
`node:crypto`（`randomUUID`）、`AgentSessionRuntime`、`ExtensionUIContext`/`ExtensionUIDialogOptions`/
`ExtensionWidgetOptions`/`WorkingIndicatorOptions`、`flushRawStdout`/`takeOverStdout`/
`waitForRawStdoutBackpressure`/`writeRawStdout`、`killTrackedDetachedChildren`、`theme`（全局主题 Proxy）、
`toJsonEvent`、`attachJsonlLineReader`/`serializeJsonLine`、协议类型。

`theme` 的特殊性：`export const theme: Theme = new Proxy({} as Theme, { get(...) { ... } })`，
未初始化时读取任意属性会抛【逐字】`Theme not initialized. Call initTheme() first.`。
RPC 的 `uiContext.theme` getter 返回该 Proxy（每次访问转发到当前全局主题）。

#### 2.6.2 顶层工具函数（runRpcMode 内部闭包）

```ts
const output = (obj: RpcResponse | RpcExtensionUIRequest | object) => {
	writeRawStdout(serializeJsonLine(obj));
};

const success = <T extends RpcCommand["type"]>(id: string | undefined, command: T, data?: object | null): RpcResponse => {
	if (data === undefined) return { id, type: "response", command, success: true } as RpcResponse;
	return { id, type: "response", command, success: true, data } as RpcResponse;
};

const error = (id: string | undefined, command: string, message: string): RpcResponse => {
	return { id, type: "response", command, success: false, error: message };
};
```

- `success` 的键序：`id`、`type`、`command`、`success`、`data?`。`id: undefined` 被序列化丢弃。
- `data === null` 会保留 `"data":null`；`data` 为 `false`/`0`/`""` 也会保留（只有 `undefined` 特殊）。
- `error` 键序：`id`、`type`、`command`、`success`、`error`。`command` 为 `undefined` 时键被丢弃。

#### 2.6.3 `pendingExtensionRequests` 与关停标志

```ts
const pendingExtensionRequests = new Map<string, { resolve: (value: any) => void; reject: (error: Error) => void }>();
let shutdownRequested = false;
let shuttingDown = false;
const signalCleanupHandlers: Array<() => void> = [];
```

Map 的键是 dialog 的 `crypto.randomUUID()`（v4，36 字符小写带连字符）。
`reject` 只在 `editor` 与 `createDialogPromise` 中登记，**从未被调用**（没有超时/退订的 reject 路径），
因此 RPC 模式下的 dialog Promise 只会 resolve。

#### 2.6.4 `createDialogPromise`（select / confirm / input 共用）

```ts
function createDialogPromise<T>(
	opts: ExtensionUIDialogOptions | undefined,
	defaultValue: T,
	request: Record<string, unknown>,
	parseResponse: (response: RpcExtensionUIResponse) => T,
): Promise<T> {
	if (opts?.signal?.aborted) return Promise.resolve(defaultValue);

	const id = crypto.randomUUID();
	return new Promise((resolve, reject) => {
		let timeoutId: ReturnType<typeof setTimeout> | undefined;

		const cleanup = () => {
			if (timeoutId) clearTimeout(timeoutId);
			opts?.signal?.removeEventListener("abort", onAbort);
			pendingExtensionRequests.delete(id);
		};

		const onAbort = () => { cleanup(); resolve(defaultValue); };
		opts?.signal?.addEventListener("abort", onAbort, { once: true });

		if (opts?.timeout) {
			timeoutId = setTimeout(() => { cleanup(); resolve(defaultValue); }, opts.timeout);
		}

		pendingExtensionRequests.set(id, {
			resolve: (response: RpcExtensionUIResponse) => { cleanup(); resolve(parseResponse(response)); },
			reject,
		});
		output({ type: "extension_ui_request", id, ...request } as RpcExtensionUIRequest);
	});
}
```

逐条：

1. **已 aborted 的 signal**：立即 resolve 默认值，**不发出请求**、不登记、不生成 id。
2. id 在**检查 signal 之后**生成（因此只有真正发出的请求才消耗一个 uuid）。
3. `cleanup` 的三个动作顺序：清 timeout -> 移除 abort 监听 -> 从 Map 删除。
4. `timeout` 判定用 `if (opts?.timeout)`：`0` 视为“无超时”；`undefined` 无超时。
5. `opts.timeout` 会被**原样放入 request**（`timeout: opts?.timeout`），因此 wire 上
   `timeout` 键仅在非 undefined 时出现（`JSON.stringify` 丢弃 undefined）。
6. 超时与 abort 都 resolve **默认值**（`undefined` / `false`），不是 reject。
7. 请求在登记**之后**发出，避免客户端同步回包时找不到 pending（Node 的 stdin 回包至少下一 tick，
   但顺序仍必须保持）。
8. `...request` 展开在 `{type,id}` 之后，因此 request 里的键不能覆盖 `type`/`id`（实际不会）。
9. 默认值：`select` -> `undefined`；`confirm` -> `false`；`input` -> `undefined`；`editor` 用单独实现。

`parseResponse`：

- `select`：`("cancelled" in r && r.cancelled) ? undefined : ("value" in r ? r.value : undefined)`
- `confirm`：`("cancelled" in r && r.cancelled) ? false : ("confirmed" in r ? r.confirmed : false)`
- `input`：同 select

注意 `r.value` 的类型未校验（可能是 number/undefined/对象），原样返回给扩展。

#### 2.6.5 `createExtensionUIContext()`（完整方法表）

返回一个**新对象**（每次 rebind 都新建）。逐方法（顺序与源码一致）：

| 方法 | 行为 |
|---|---|
| `select(title, options, opts)` | `createDialogPromise(opts, undefined, { method:"select", title, options, timeout: opts?.timeout }, parseSelect)` |
| `confirm(title, message, opts)` | `createDialogPromise(opts, false, { method:"confirm", title, message, timeout: opts?.timeout }, parseConfirm)` |
| `input(title, placeholder, opts)` | `createDialogPromise(opts, undefined, { method:"input", title, placeholder, timeout: opts?.timeout }, parseSelect)` |
| `notify(message, type?)` | fire-and-forget：`output({type:"extension_ui_request", id: randomUUID(), method:"notify", message, notifyType: type})` |
| `onTerminalInput(handler)` | 返回 `() => {}`（no-op）；**忽略** handler |
| `setStatus(key, text)` | fire-and-forget：`{type, id:uuid, method:"setStatus", statusKey:key, statusText:text}` |
| `setWorkingMessage(_msg?)` | no-op |
| `setWorkingVisible(_visible)` | no-op |
| `setWorkingIndicator(_opts?)` | no-op |
| `setHiddenThinkingLabel(_label?)` | no-op |
| `setWidget(key, content, options?)` | **仅当** `content === undefined \|\| Array.isArray(content)` 时发请求：`{type,id:uuid,method:"setWidget",widgetKey:key,widgetLines:content,widgetPlacement:options?.placement}`；组件工厂函数被静默忽略（不发任何记录） |
| `setFooter(_factory)` | no-op |
| `setHeader(_factory)` | no-op |
| `setTitle(title)` | fire-and-forget：`{type,id:uuid,method:"setTitle",title}` |
| `custom()` | `async () => undefined as never`（返回 `undefined`） |
| `pasteToEditor(text)` | `this.setEditorText(text)`（依赖方法调用时的 `this`，即返回对象自身） |
| `setEditorText(text)` | fire-and-forget：`{type,id:uuid,method:"set_editor_text",text}` |
| `getEditorText()` | 同步返回 `""` |
| `editor(title, prefill?)` | 手写 Promise：生成 uuid、登记 `{resolve, reject}`、`output({type,id,method:"editor",title,prefill})`；**无 timeout/signal 支持**。resolve 规则：`cancelled` -> `undefined`；`"value" in response` -> `response.value`；否则 `undefined` |
| `addAutocompleteProvider()` | no-op |
| `setEditorComponent()` | no-op |
| `getEditorComponent()` | 返回 `undefined` |
| `get theme()` | 返回全局 `theme` Proxy |
| `getAllThemes()` | 返回 `[]` |
| `getTheme(_name)` | 返回 `undefined` |
| `setTheme(_theme)` | 返回【逐字】`{ success: false, error: "Theme switching not supported in RPC mode" }` |
| `getToolsExpanded()` | 返回 `false` |
| `setToolsExpanded(_expanded)` | no-op |

要点：

- fire-and-forget 的 5 个 method（`notify`、`setStatus`、`setWidget`、`setTitle`、`set_editor_text`）
  **每次都生成新 uuid**，且**不登记** pending。
- `editor` 与 dialog 的 id 空间共享（同一 `randomUUID` + 同一 Map）。
- `setStatus(key, undefined)` 与 `setWidget(key, undefined)` 仍发出请求（用于清除），
  对应键缺失但请求存在。
- `ExtensionRunner.setUIContext` 会用 `wrapUIPromptContext` 包一层：`select`/`confirm`/`input`/`editor`/`custom`
  在调用前后发 `ui_prompt_start` / `ui_prompt_end` 微任务事件（见 §5.2.3）。RPC 模式**不消费**这些事件，
  但它们会进入 `RunnerEmitEvent` 通道，不写 stdout。
- **包装是对象展开**（`{...ui, select: ..., ...}`）：扩展拿到的不是本函数返回的原始对象，而是一份浅拷贝；
  其中 `theme` getter 在展开时被求值，得到全局 `theme` **Proxy 引用**（读取其属性时才可能抛
  `Theme not initialized...`）。因此扩展读 `ctx.ui.theme.x` 仍是“实时”的。
- `pasteToEditor` 内部用 `this.setEditorText(text)`：以 `ctx.ui.pasteToEditor(...)` 形式调用时 `this`
  指向包装后的对象，正常；若被解构后单独调用（`const {pasteToEditor} = ctx.ui`），`this` 为 `undefined`
  -> 抛 TypeError。Rust 侧等价实现可直接调用自身的 `set_editor_text`（不引入该脆弱性，属可接受差异）。
- `ctx.hasUI` 在 RPC 模式为 `true`（`uiContext !== noOpUIContext`）。

#### 2.6.6 `rebindSession()`（RPC 版）

```ts
runtimeHost.setRebindSession(async () => { await rebindSession(); });

const rebindSession = async (): Promise<void> => {
	session = runtimeHost.session;
	await session.bindExtensions({
		uiContext: createExtensionUIContext(),
		mode: "rpc",
		commandContextActions: { /* 与 print 模式相同的 6 个动作，见 §2.3.2(4) */ },
		shutdownHandler: () => { shutdownRequested = true; },
		onError: (err) => {
			output({ type: "extension_error", extensionPath: err.extensionPath, event: err.event, error: err.error });
		},
	});

	unsubscribe?.();
	unsubscribeBackpressure?.();
	unsubscribe = session.subscribe((event) => {
		output(toJsonEvent(event));
		if (event.type === "agent_settled") { void checkShutdownRequested(); }
	});
	unsubscribeBackpressure = session.agent.subscribe(async () => {
		await waitForRawStdoutBackpressure();
	});
};

await rebindSession();
registerSignalHandlers();
```

差异（相对 print/json）：

1. `uiContext` 存在 -> `hasUI === true`，`mode === "rpc"`。
2. `shutdownHandler` 只把 `shutdownRequested = true`，**不当场退出**；真正的退出由
   `checkShutdownRequested()` 在“当前命令处理完后”或“`agent_settled` 后”执行。
3. `onError` 不写 stderr，而是写一条 stdout 记录：
   键序 `type`、`extensionPath`、`event`、`error`。**不含** `stack`。
4. 事件订阅**始终**输出（无 mode 判断），`bash_execution_update` 等 RPC 专属事件也在此路径写出。
5. backpressure 订阅**无条件**（print 模式只在 json 模式）。
6. `registerSignalHandlers()` 在**首次 rebind 之后**调用（print 模式在 rebind 之前）。
7. 顺序：`takeOverStdout()` 在 `runRpcMode` 最开头（第 2 行）。

#### 2.6.7 命令处理 `handleCommand(command)` 逐条表

返回 `Promise<RpcResponse | undefined>`；`undefined` 表示“不立即响应”（只有 `prompt`）。

**prompt**

```ts
let preflightSucceeded = false;
void session.prompt(command.message, {
	images: command.images,
	streamingBehavior: command.streamingBehavior,
	source: "rpc",
	preflightResult: (disposition) => {
		preflightSucceeded = true;
		output(success(id, "prompt", { disposition }));
	},
}).catch((e) => {
	if (!preflightSucceeded) output(error(id, "prompt", e.message));
});
return undefined;
```

- **不 await**：命令处理立即返回 `undefined`，response 由预检回调异步写出。
- 因此 `prompt` 的 response 可能晚于后续命令的 response（异步交错，客户端必须按 id 关联）。
- `preflightResult` 只在三种结果时调用：`"handled"`（扩展命令/输入处理器消费）、`"queued"`、
  `"started"`（见 §5.1.1）。**被拒绝时不调用**。
- 预检失败的错误文本来自 `Error.message`；例如无 API key：
  【逐字】`No API key found for ${provider}.\n\nUse /login to log into a provider via OAuth or API key. See:\n  ${docsPath}/providers.md\n  ${docsPath}/models.md`
  （`formatNoApiKeyFoundMessage`，见 §5.1.7）。
- 预检成功后若运行中途抛错，`.catch` 不输出任何东西（`preflightSucceeded === true`）。
- 同一 id 只会有一个 response（回归 #9098 / #9803 断言）。
- `.catch` 里用的是 `e.message`（**不**做 `instanceof Error` 判定）。实测（Node v26.12.1）
  三种边界必须区分（旧版本只写了第一种，且把结论说得过宽）：
  1. 抛出**原始值**（`"s"`、`42`、`Symbol()`、`undefined`）或普通对象无 `message`
     属性：`e.message` 为 `undefined`（`undefined`/`null` 甚至会先抛
     `TypeError: Cannot read properties of undefined (reading 'message')`），
     `error(id,"prompt", undefined)` -> `error` 键被 `JSON.stringify` **丢弃**，实际写出
     `{"id":...,"type":"response","command":"prompt","success":false}`（无 `error` 键）。
  2. `new Error()`（`message === ""`）：`e.message` 是**空字符串**（不是 `undefined`），
     实际写出 `...,"success":false,"error":""`（`error` 键**存在**且为空串）。
  3. 普通对象 `{message:"m"}`：`e.message === "m"`，正常写出文本。
  Rust 侧应保持该边界（用 `Option<String>` + skip；同时区分 `Some("")` 与 `None`）。

**steer / follow_up**

```ts
const disposition = await session.steer(command.message, command.images, { source: "rpc" });
return success(id, "steer", { disposition });
```

`follow_up` 用 `session.followUp`，命令名 `"follow_up"`，其它相同。异常走外层 catch。

**旧版本遗漏的失败面**：`steer` / `follow_up` 走 `_queueUserInput`，其第一步是
`if (text.startsWith("/")) this._throwIfExtensionCommand(text);`——若文本是**已注册的扩展命令**，
抛【逐字】`Extension command "/${commandName}" cannot be queued. Use prompt() or execute the command when not streaming.`
（`commandName` 取第一个空格前的命令名，不含 `/`）。该异常 -> 外层 catch ->
`{id, type:"response", command:"steer"|"follow_up", success:false, error:"Extension command \"/name\" cannot be queued. ..."}`。
其它分支：
- 输入处理器消费该输入 -> `disposition:"handled"`；
- 否则扩展技能命令（`/skill:`）与提示模板后入队 -> `disposition:"queued"`；并 emit
  `queue_update`（`_queueSteer`/`_queueFollowUp` 内部 `_emitQueueUpdate()`，因此在 response 之前写出）。
- `source: "rpc"` 会传给 input handler（见 §5.1.1）。

**abort**

`await session.abort(); return success(id, "abort");`（`abort` 会等 agent 空闲）。

`session.abort()` 的内部顺序（`agent-session.ts:2498`–`2508`，Rust 侧若自行实现需对齐）：
`_isAgentRunActive` 时置 `_agentRunAbortRequested = true` -> `abortRetry()` ->
`abortCompaction()` -> `abortBranchSummary()` -> 若正在 `agent_before_settle` 则置
`_abortDuringBeforeSettle` -> `agent.abort()` -> `await waitForIdle()`。
因此 `abort` 的 response 会在 `agent_settled` **之后**（并已 flush 掉排队消息）才写出。

**clear_queue**

`return success(id, "clear_queue", session.clearQueue());` -> `{steering, followUp}`（文本数组，非消息）。
`clearQueue` 内部会 emit `queue_update`（因此 stdout 先出现事件再出现响应）。
细节（`agent-session.ts:2466`）：先**复制**两个数组、清空内部队列、`agent.clearAllQueues()`，
再 `_emitQueueUpdate()`；返回的是**清空前**的内容（因此 `queue_update` 里两个数组都是空的）。

**new_session**

```ts
const options = command.parentSession ? { parentSession: command.parentSession } : undefined;
const result = await runtimeHost.newSession(options);
if (!result.cancelled) await rebindSession();
return success(id, "new_session", result);
```

- `parentSession` 为空串时按“未提供”处理（falsy）。
- `runtimeHost.newSession` 可能抛（`emitBeforeSwitch`/`createRuntime` 失败），走外层 catch ->
  `success:false`；扩展可通过 `session_before_switch` 取消 -> `{cancelled:true}` 且**不** rebind。
- `runtimeHost.newSession` 自己也会调用 `rebindSession`（`finishSessionReplacement`），
  因此未取消时实际 rebind **两次**（第二次会重新 bindExtensions 并重新订阅）。这是 TS 现状，必须保留：
  它导致 `session_start` 扩展事件被 emit 两次、新旧订阅切换两次。
- `result` 直接作为 `data` -> `{cancelled}`。

**get_state**

按 §2.5.3 键序构造字面量，字段来源：
`session.model`、`session.thinkingLevel`、`session.isStreaming`、`session.isCompacting`、
`session.steeringMode`、`session.followUpMode`、`session.sessionFile`、`session.sessionId`、
`session.sessionName`、`session.autoCompactionEnabled`、`messageCount: session.messages.length`、
`pendingMessageCount: session.pendingMessageCount`。

**set_model**

```ts
const models = session.modelRuntime.getAvailableSnapshot();
const model = models.find((m) => m.provider === command.provider && m.id === command.modelId);
if (!model) return error(id, "set_model", `Model not found: ${command.provider}/${command.modelId}`);
await session.setModel(model);
return success(id, "set_model", model);
```

错误文本【逐字】`Model not found: ${provider}/${modelId}`（原样拼接，不转义）。

**第二个失败面（旧版本遗漏）**：模型存在但该 provider 无凭据时，`session.setModel` 内部
`if (!(await this._modelRuntime.checkAuth(model.provider)))` 抛
【逐字】`No API key for ${model.provider}/${model.id}`（`src/core/agent-session.ts:2543`）。
该异常由 `handleInputLine` 的外层 catch 转成
`{id, type:"response", command:"set_model", success:false, error:"No API key for <provider>/<id>"}`。
Rust 侧必须区分 `Model not found: ...`（命令层）与 `No API key for ...`（session 层）两条文本。

`set_model` 还有副作用（Rust 侧若由 ca-session 实现需保持一致）：成功时
`appendModelChange(provider, id)`、按目标模型重算并 `setThinkingLevel(...)`（可能触发
`thinking_level_changed` 事件与 `thinking_level_change` 条目）、`await _emitModelSelect`
（`model_select` 扩展事件，仅在模型变化时）；`options.persist` 为 false（RPC 不传）。

**cycle_model**

`const result = await session.cycleModel();` -> 无结果 `success(id,"cycle_model",null)`；
有结果直接返回 `{model, thinkingLevel, isScoped}`（`ModelCycleResult`）。

**旧版本遗漏的语义**（`src/core/agent-session.ts:2583`–`2666`）：
- 有 `scopedModels`（`--models` 传入）时走 `_cycleScopedModel`：先过滤掉当前不可用的 scoped 模型，
  数量 `<= 1` 返回 `undefined` -> `null`；结果 `isScoped: true`。
- 否则走 `_cycleAvailableModel`：`getAvailableSnapshot()` 数量 `<= 1` 返回 `undefined` -> `null`；
  结果 `isScoped: false`。
- 当前模型不在列表里时 `currentIndex = 0`（不是 -1 取模，因此从第 0 项开始）。
- 成功时同 `set_model`：`appendModelChange`、`setThinkingLevel`（可能 emit `thinking_level_changed`）、
  `model_select` 扩展事件（`source: "cycle"`，仅模型变化时）。
- `thinkingLevel` 字段是**切换后**的 `this.thinkingLevel`（可能已被 clamp）。

**get_available_models** -> `{ models }`（`getAvailableSnapshot()` 的数组，完整模型对象）。

**set_thinking_level** -> `session.setThinkingLevel(command.level)`，无 data。
**旧版本遗漏**：该 setter 会**钳制**取值——`getAvailableThinkingLevels()` 不含请求值时走
`clampThinkingLevel(model, level)`（`packages/ai/src/models.ts:1272`：向上找最近的受支持档，
找不到再向下，最后回落 `availableLevels[0] ?? "off"`；无模型时直接 `"off"`）。
只有**生效值**与旧值不同才 `appendThinkingLevelChange` 并 emit
`thinking_level_changed {level: <生效值>}`。因此 `set_thinking_level "max"` 在不支持 `max`
的模型上会静默变成 `"high"` 之类，且 response 仍是 `success:true`（无 data、不回传生效值）。
`command.level` 缺失/非法字符串时：`getAvailableThinkingLevels` 不含它 -> clamp 的
`indexOf === -1` 分支 -> `availableLevels[0] ?? "off"`（不抛错）。

**cycle_thinking_level** -> `undefined` 时 `success(..., null)`；否则 `{ level }`。
`cycleThinkingLevel` 在 `!supportsThinking()`（模型无 `reasoning` 或**无模型**）时返回
`undefined` -> `"data":null`。循环在 `getAvailableThinkingLevels()` 上取模加一。

**get_available_thinking_levels** -> `{ levels }`。无模型时返回**全部** 7 档
`["off","minimal","low","medium","high","xhigh","max"]`（`THINKING_LEVEL_OPTIONS`，
`src/core/defaults.ts`）；有模型时用 `getSupportedThinkingLevels(model)`
（`reasoning:false` -> 仅 `["off"]`；`xhigh`/`max` 只在 `thinkingLevelMap` 显式包含时出现）。

**set_steering_mode / set_follow_up_mode** -> 调用对应 setter，无 data。

**compact** -> `await session.compact(command.customInstructions)` -> `data = CompactionResult`。
（`customInstructions` 为 `undefined` 时按无指令压缩。）

**旧版本严重遗漏**：`session.compact()` 不是纯读取，其内部（`src/core/agent-session.ts:2828` 起）：
1. **先 `await this.abort()`**——会中止正在进行的 retry / compaction / branch summary / agent run
   并 `await waitForIdle()`。因此在流式中发 `compact` 会先打断当前 run。
2. 新建 `_compactionAbortController`，并 `_emit({type:"compaction_start", reason:"manual"})`
   （stdout 先出现该事件）。
3. 失败路径的【逐字】错误文本（都经外层 catch -> `success:false`）：
   - 无模型：`formatNoModelSelectedMessage()`（见 §5.1.7）
   - `Already compacted`（分支末尾已是 compaction 条目）
   - `Nothing to compact (session too small)`（`prepareCompaction` 返回 undefined 且末尾不是 compaction）
   - `Compaction cancelled`（`session_before_compact` 扩展返回 `{cancel:true}`，或 abort controller 被触发）
   - 默认压缩生成失败时：catch 块先算 `errorMessage = aborted ? undefined : \`Compaction failed: ${message}\``，
     把它放进 `compaction_end.errorMessage` 与 `session_compact_failed` 扩展事件，然后
     **rethrow 原始 error**——因此 `compact` 的 response `error` 是**原始** message（例如
     `No API key for ...` / provider 文本），**不是** `Compaction failed: ...`；后者只出现在事件里。
4. 失败时仍 emit `compaction_end {reason:"manual", result: undefined, aborted, willRetry:false,
   errorMessage?}`（`aborted` 为 true 时 `errorMessage` 为 undefined）。
5. 成功时 emit `compaction_end {reason:"manual", result: CompactionResult, aborted:false, willRetry:false}`，
   且在此之前 emit 扩展事件 `session_compact`。
Rust 侧必须复刻“先 abort + 两个事件 + 五类错误文本”的时序，否则 RPC 的 `compact` 响应与事件流都会不同。

**set_auto_compaction** -> `session.setAutoCompactionEnabled(command.enabled)`。
**set_auto_retry** -> `session.setAutoRetryEnabled(command.enabled)`。
**abort_retry** -> `session.abortRetry()`。

**bash**

```ts
const eventResult = await session.extensionRunner.emitUserBash({
	type: "user_bash",
	command: command.command,
	excludeFromContext: command.excludeFromContext ?? false,
	cwd: session.sessionManager.getCwd(),
});

if (eventResult?.result) {
	session.recordBashResult(command.command, eventResult.result, { excludeFromContext: command.excludeFromContext });
	return success(id, "bash", eventResult.result);
}

const result = await session.executeBash(command.command, undefined, {
	excludeFromContext: command.excludeFromContext,
	id,
	operations: eventResult?.operations,
});
return success(id, "bash", result);
```

- `excludeFromContext` 默认 `false`（事件里用 `?? false`；`recordBashResult` / `executeBash` 传原始值）。
- 扩展抛错时 `emitUserBash` 内部先 `emitError`（`event: "user_bash"`）再 rethrow；
  RPC 的 `onError` 把它写成 `extension_error` 记录，随后外层 catch 写失败响应（回归 #9068）。
- 扩展返回 `{}`（非法）-> `emitUserBash` 抛【逐字】
  `Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object`。
- `eventResult.result` 存在时**不再执行** bash，直接记录并返回该结果。
- `id` 传给 `executeBash` 用于 `bash_execution_update.id`（命令 id 与事件关联）。
- `emitUserBash` 的校验（`runner.ts:142`–`165`，`isUserBashEventResult`）细则：返回值必须是对象，
  且 `operations !== undefined` 与 `result !== undefined` **恰有一个**成立；`operations` 必须是带
  函数 `exec` 的对象；`result` 必须满足 `output: string`、**存在** `exitCode` 键且为
  `undefined|number`、`cancelled: boolean`、`truncated: boolean`、`fullOutputPath: undefined|string`。
  任一不满足 -> 抛上面那条 `Invalid user_bash handler result: ...`。
- 多个 `user_bash` 处理器按注册顺序串行 `await`；第一个返回非 `undefined` 的即生效（后续不再调用）。
- **旧版本遗漏**：`executeBash` 会应用 `settingsManager.getShellCommandPrefix()`（非空时
  `resolvedCommand = \`${prefix}\n${command}\``）与 `getShellPath()`；RPC `bash` 命令本身不注入
  `KIANA_SESSION_ID` / `KIANA_PROVIDER` 等 shell 工具环境变量（那属于 LLM 可调用的 `bash` 工具，
  `src/core/tools/bash.ts`，**不是** RPC 的 `bash` 命令）。
- `executeBash` 在流式期间不会立即把 `bashExecution` 消息写入历史：`recordBashResult` 见到
  `isStreaming` 为真时 push 到 `_pendingBashMessages`，在 `agent_end`/`turn_end` 后由
  `_flushPendingBashMessages()` 落盘（保持 tool_use/tool_result 顺序）。RPC 的 `bash` 响应本身
  仍立即返回；但 `get_messages` / `get_state.messageCount` 在 flush 之前看不到该消息。
- `executeBash` 每个输出块都 emit `{type:"bash_execution_update", id, delta}`（`id` 为
  `options.id`，即命令 id），因此 stdout 会先出现若干 update 事件、再出现 `response`。

**abort_bash** -> `session.abortBash()`（对 `_bashAbortControllers` 里**每个** controller 调
`abort()`，不等待；正在跑的 `executeBash` 会以 `cancelled:true`、`exitCode:undefined` 结束，
并照常 `recordBashResult`）。无 data。
**get_session_stats** -> `session.getSessionStats()`。语义（`agent-session.ts:4243`–`4295`）：
遍历**全部** `sessionManager.getEntries()`（含被压缩掉的历史）统计 `userMessages`、
`assistantMessages`、`toolResults`、`totalMessages`、`toolCalls`（assistant content 里的
`toolCall` 块数）；usage 合计来自 `usage` 条目、`branch_summary`/`compaction` 条目（带 `usage` 时）、
`toolResult` 消息的 `usage`、assistant 的 `usage`。`contextUsage` 来自 `getContextUsage()`：
无模型或 `contextWindow <= 0` 时**键缺失**；最近一次 compaction 之后没有任何非 aborted/error 且
usage 非零的 assistant 时返回 `{tokens:null, contextWindow, percent:null}`（不是整体缺键）。
`sessionFile` 为 `undefined` 时键缺失。
**export_html** -> `await session.exportToHtml(command.outputPath)` -> `{ path }`。
`exportToHtml(outputPath?, options?)` 的第二个参数 RPC **不传**（主题由
`settingsManager.getTheme()` 与已注册主题决定：取 `options.themeName` 与 settings 主题中第一个
存在的；都不存在则用默认）。未传 `outputPath` 时导出到会话目录。失败（fs/渲染）走外层 catch。
**switch_session** -> `runtimeHost.switchSession(command.sessionPath)`；`!cancelled` 时 `rebindSession()`（同样双绑）；`data = {cancelled}`。
失败面（走外层 catch）：`SessionManager.open` 的 fs 错误；`assertSessionCwdExists` 抛
`MissingSessionCwdError`，其 message 【逐字】为
`Stored session working directory does not exist: <sessionCwd>`，若有会话文件再追加
`\nSession file: <sessionFile>`，最后再追加 `\nCurrent working directory: <fallbackCwd>`
（`core/session-cwd.ts:35`–`38`）；扩展 `session_before_switch` 取消 -> `{cancelled:true}`。**注意**：`switchSession` 不接收 `command` 里的 cwd 覆盖（RPC 只传路径）。

**fork**

```ts
const result = await runtimeHost.fork(command.entryId);
if (!result.cancelled) await rebindSession();
return success(id, "fork", { text: result.selectedText, cancelled: result.cancelled });
```

`selectedText` 可能为 `undefined`（取消；或 `position:"at"`——`runtimeHost.fork` 只在
`position:"before"` 且目标是 user 消息时填 `selectedText = extractUserMessageText(...)`，
`position:"at"` 恒为 `undefined`；此外 user 消息内容全为空文本块时 `extractUserMessageText`
返回 `""`，此时 `text` 键**存在且为 `""`**，不要一律当作缺失）-> 键缺失。

`runtimeHost.fork` 的错误（`command` 走外层 catch -> `error(id, "fork", message)`）【逐字】：
- `Invalid entry ID for forking`（`entryId` 不存在；或 `position:"before"` 下目标不是 user 消息）
- `Persisted session is missing a session file`
- `This session has not been saved yet. Send a message before cloning or forking it.`
  （持久化会话文件尚未落盘）
- `Failed to create forked session`（`createBranchedSession` 返回空）

**clone**

```ts
const leafId = session.sessionManager.getLeafId();
if (!leafId) return error(id, "clone", "Cannot clone session: no current entry selected");
const result = await runtimeHost.fork(leafId, { position: "at" });
if (!result.cancelled) await rebindSession();
return success(id, "clone", { cancelled: result.cancelled });
```

错误文本【逐字】`Cannot clone session: no current entry selected`。

**get_fork_messages** -> `{ messages: session.getUserMessagesForForking() }`
（每项 `{entryId, text}`，只含文本非空的 user 消息，按 append 顺序）。
`text` 由 `contentText(content, "")` 得到（**不 trim**；多 text 块直接拼接；只有空白字符的
消息也算非空）。图片等非 text 块被忽略。

**get_entries**

```ts
let entries = sessionManager.getEntries();
if (command.since !== undefined) {
	const sinceIndex = entries.findIndex((e) => e.id === command.since);
	if (sinceIndex === -1) return error(id, "get_entries", `Entry not found: ${command.since}`);
	entries = entries.slice(sinceIndex + 1);
}
return success(id, "get_entries", { entries, leafId: sessionManager.getLeafId() });
```

`getEntries()` 是**全部**条目（含被压缩掉的历史），按 append 顺序、已排除 header（`type:"session"`）；
返回的是浅拷贝数组（`session-manager.ts:1520`）。`leafId` 是当前叶（空会话为 `null`）。

- `since` 为 `null`（JSON null）时 `!== undefined` 成立 -> `findIndex(e => e.id === null)` 全不匹配 ->
  `Entry not found: null`（`String(null)` 插值为 `null`）。
- `since` 为 `""` -> 同上，文本 `Entry not found: `（空）。
- 结果严格“之后”（不含 `since` 本身）。`leafId` 为 `null`（空会话）时保留 `"leafId":null`。

**get_tree** -> `{ tree: sessionManager.getTree(), leafId: sessionManager.getLeafId() }`。

**get_last_assistant_text**

```ts
const text = session.getLastAssistantText();
return success(id, "get_last_assistant_text", { text });
```

`getLastAssistantText()` 的实现返回 `string | undefined`（**不是** `null`）：从后往前找第一条
assistant 消息（跳过 `stopReason === "aborted"` 且 `content` 为空的），把全部 text 块拼接后
`trim()`（JS `String.prototype.trim`，见 §4.2.6），空则返回 `undefined`。因此：

- 有文本 -> `data: {"text":"..."}`；
- 无文本 -> `data` 是 `{text: undefined}`，`JSON.stringify` 得到 **`"data":{}`**（不是
  `"data":null`，也不是缺 `data` 键）。

`docs/rpc-commands.md` 与 TS 类型声明 `string | null`，与实现不符。**Rust 侧必须按实现**：
`data` 为 `{}` 或 `{"text": "..."}`。`RpcClient.getLastAssistantText()` 的 `getData(...).text`
在 `{}` 上得到 `undefined`，与 `rpc.test.ts` 的 `expect(text).toBeUndefined()` 一致。

**set_session_name**

```ts
const name = command.name.trim();
if (!name) return error(id, "set_session_name", "Session name cannot be empty");
session.setSessionName(name);
return success(id, "set_session_name");
```

错误文本【逐字】`Session name cannot be empty`。`trim` 用 JS `String.prototype.trim`
（Unicode `WhiteSpace` + `LineTerminator`：含 U+FEFF，见 §4.2.6）。
`command.name` 缺失（`undefined`）时 `undefined.trim()` 抛 `TypeError`，外层 catch 把它转成
`success:false`，`error` 文本为 `Cannot read properties of undefined (reading 'trim')`（V8 原文）。
`setSessionName` 会 `appendSessionInfo(name)` 并 emit `session_info_changed {name}`（由此
**先**于 response 写出该事件）。注：`getSessionName()` 返回的是 `session_info` 条目的
`name?.trim() || undefined`。

**get_messages** -> `{ messages: session.messages }`（`AgentMessage[]`，含自定义消息；见 §2.3.2(6)
对 `session.messages === agent.state.messages` 的说明——**不是**独立投影）。

**get_commands**

```ts
const commands: RpcSlashCommand[] = [];
for (const command of session.extensionRunner.getRegisteredCommands()) {
	commands.push({ name: command.invocationName, description: command.description, source: "extension", sourceInfo: command.sourceInfo });
}
for (const template of session.promptTemplates) {
	commands.push({ name: template.name, description: template.description, source: "prompt", sourceInfo: template.sourceInfo });
}
for (const skill of session.resourceLoader.getSkills().skills) {
	commands.push({ name: `skill:${skill.name}`, description: skill.description, source: "skill", sourceInfo: skill.sourceInfo });
}
return success(id, "get_commands", { commands });
```

- 顺序：扩展命令 -> 提示模板 -> 技能，各自内部按注册/加载顺序。
- 扩展命令名用 `invocationName`（重名时追加 `:1`、`:2` 或冲突时递增后缀，见 §5.2.2）。
  算法（`ExtensionRunner.resolveRegisteredCommands`，`runner.ts:814`–`848`）：先统计每个
  `command.name` 在所有扩展中的出现次数；出现次数 `> 1` 时 `invocationName = name:occurrence`
  （occurrence 从 1 起）；若该名字已被占用，则从 `occurrence` 起递增后缀直到未占用。
  出现次数为 1 的命令名**不加**后缀。
- `getRegisteredCommands()` 每次调用都会**重置** `commandDiagnostics` 并重新解析（有 O(n) 成本）。
- 技能名**加前缀** `skill:`；`description` 为 `undefined` 时键缺失。
- 每项键序：`name`、`description`、`source`、`sourceInfo`。

**default**

```ts
const unknownCommand = command as { type: string };
return error(id, unknownCommand.type, `Unknown command: ${unknownCommand.type}`);
```

错误文本【逐字】`Unknown command: ${type}`；`command` 字段为原 `type`（回归 #5868 要求保留 id）。

#### 2.6.8 信号处理

```ts
const registerSignalHandlers = (): void => {
	const signals: NodeJS.Signals[] = ["SIGTERM"];
	if (process.platform !== "win32") signals.push("SIGHUP");
	for (const signal of signals) {
		const handler = () => {
			killTrackedDetachedChildren();
			void shutdown(signal === "SIGHUP" ? 129 : 143, signal);
		};
		process.on(signal, handler);
		signalCleanupHandlers.push(() => process.off(signal, handler));
	}
};
```

与 print 模式的区别：**带 signal 参数**调 `shutdown`，因此 SIGTERM 走“不 flush”分支。

#### 2.6.9 `shutdown(exitCode = 0, signal?)`

```ts
async function shutdown(exitCode = 0, signal?: NodeJS.Signals): Promise<never> {
	if (shuttingDown) process.exit(exitCode);
	shuttingDown = true;
	for (const cleanup of signalCleanupHandlers) cleanup();
	unsubscribe?.();
	unsubscribeBackpressure?.();
	await runtimeHost.dispose();
	detachInput();
	process.stdin.pause();
	if (signal !== "SIGTERM") await flushRawStdout();
	process.exit(exitCode);
}
```

逐条：

1. **重入**：`shuttingDown` 为真时立即 `process.exit(exitCode)`（不等待、不 flush）。这让
   “第二个 SIGTERM”能强杀。
2. 先移除信号处理器（防止 dispose 期间重入信号处理）。
3. 退订会话事件与 agent 背压。
4. `await runtimeHost.dispose()`（emit `session_shutdown{reason:"quit"}`；扩展可在此写记录）。
5. `detachInput()`（解除 JSONL reader 与 stdin `end` 监听）、`process.stdin.pause()`。
6. **SIGTERM 不 flush**（退出码 143 的强制语义：不等待未完成写）；其它情况（stdin end -> 0、
   SIGHUP -> 129、扩展请求 -> 0）先 `flushRawStdout()`。
7. `process.exit(exitCode)`。

注意 `shutdown` 的 `exitCode` 参数与 `signal` 组合：
- stdin `end` -> `shutdown()` -> `(0, undefined)` -> flush -> exit 0。
- SIGTERM -> `shutdown(143, "SIGTERM")` -> 不 flush -> exit 143。
- SIGHUP -> `shutdown(129, "SIGHUP")` -> flush -> exit 129。
- 扩展请求 -> `shutdown()` -> flush -> exit 0。

#### 2.6.10 `checkShutdownRequested()` 与 `handleInputLine`

```ts
async function checkShutdownRequested(): Promise<void> {
	if (!shutdownRequested) return;
	await shutdown();
}
```

```ts
const handleInputLine = async (line: string) => {
	let parsed: unknown;
	try {
		parsed = JSON.parse(line);
	} catch (parseError: unknown) {
		output(error(undefined, "parse", `Failed to parse command: ${parseError instanceof Error ? parseError.message : String(parseError)}`));
		await waitForRawStdoutBackpressure();
		return;
	}

	if (typeof parsed === "object" && parsed !== null && "type" in parsed && parsed.type === "extension_ui_response") {
		const response = parsed as RpcExtensionUIResponse;
		const pending = pendingExtensionRequests.get(response.id);
		if (pending) { pendingExtensionRequests.delete(response.id); pending.resolve(response); }
		return;
	}

	const command = parsed as RpcCommand;
	try {
		const response = await handleCommand(command);
		if (response) { output(response); await waitForRawStdoutBackpressure(); }
		await checkShutdownRequested();
	} catch (commandError: unknown) {
		output(error(command.id, command.type, commandError instanceof Error ? commandError.message : String(commandError)));
		await waitForRawStdoutBackpressure();
	}
};
```

要点：

1. **解析失败**：`output(error(undefined, "parse", "Failed to parse command: ${msg}"))`，
   无 id（键缺失），command 为 `"parse"`；随后 `await waitForRawStdoutBackpressure()`，不检查关停。
2. **UI 响应分支**：不写任何记录、不调用 `checkShutdownRequested`、不经 try/catch。
   若 `response.id` 缺失 -> `get(undefined)` 无匹配 -> 静默忽略。
3. **命令分支**：`await handleCommand`；仅当返回非 `undefined` 时才 `output` + 等背压。
   `prompt` 返回 `undefined`，因此**不等待背压**、直接进入 `checkShutdownRequested()`。
4. **异常分支**：`error(command.id, command.type, message)`；`command` 为 `null` 时访问 `.id`/`.type`
   本身会再抛（未捕获，成为 rejected promise——因为 `handleInputLine` 被 `void` 调用，
   于是变成 unhandled rejection）。Rust 侧应记录此边界并与 TS 一致（至少不崩溃地静默）。
5. `handleInputLine` 的调用方式是 `void handleInputLine(line)`：**并发处理**，不排队。
   多个命令可同时 in-flight（因此响应顺序不保证）。
6. 解析错误文本依赖 V8 的 `JSON.parse` 消息（§4.3）。

#### 2.6.11 stdin 装配与永不 resolve

```ts
const onInputEnd = () => { void shutdown(); };
process.stdin.on("end", onInputEnd);

detachInput = (() => {
	const detachJsonl = attachJsonlLineReader(process.stdin, (line) => { void handleInputLine(line); });
	return () => { detachJsonl(); process.stdin.off("end", onInputEnd); };
})();

return new Promise(() => {});
```

- `detachInput` 初始为 `() => {}`（供 `shutdown` 在装配前调用时安全）。
- 监听顺序：先 `end`，后 `data`（`attachJsonlLineReader` 内部注册 `data` 与 `end`）。
  Node 中同一事件多个监听器按注册顺序执行，因此 `onInputEnd` 先于 JSONL reader 的 `onEnd` 运行。
  注意：`onInputEnd` 会立即开始 `shutdown()`（异步），其中 `detachInput()` 会移除 reader 的 `end`
  监听器——但 `emit` 已在进行中，reader 的 `onEnd` 仍会被调用（Node 的 listeners 快照），
  于是最后一段无换行的数据仍可能被处理。Rust 侧不必复刻这种事件细节，但**必须**保证：
  stdin EOF 时先处理完剩余完整行（含最后无 LF 的行），再关停（否则丢命令）。
- `runRpcMode` 返回一个永不 resolve 的 Promise，调用方 `await` 永远挂起（直到进程退出）。

#### 2.6.12 RPC 模式的记录写出顺序（同一命令内）

以成功命令为例：

1. 命令执行期间由 `session` 发出的事件（如 `queue_update`、`message_*`、`bash_execution_update`）
   经 `session.subscribe` -> `output(toJsonEvent(event))`。
2. 命令返回 `response` -> `output(response)`。
3. `await waitForRawStdoutBackpressure()`。
4. `await checkShutdownRequested()`。

因 `writeRawStdout` 串行链，顺序即调用顺序；跨命令交错由并发决定。

---
### 2.7 `src/modes/rpc/rpc-client.ts`（617 行）

#### 2.7.1 类型

```ts
type DistributiveOmit<T, K extends keyof T> = T extends unknown ? Omit<T, K> : never;
type RpcCommandBody = DistributiveOmit<RpcCommand, "id">;

export interface RpcClientOptions {
	cliPath?: string;     // 默认 "dist/cli.js"
	cwd?: string;
	env?: Record<string, string>;
	provider?: string;
	model?: string;
	args?: string[];
}

export interface ModelInfo {
	provider: string;
	id: string;
	contextWindow: number;
	reasoning: boolean;
}

export type RpcEventListener = (event: JsonAgentSessionEvent) => void;
```

#### 2.7.2 类字段

```ts
private process: ChildProcess | null = null;
private stopReadingStdout: (() => void) | null = null;
private eventListeners: RpcEventListener[] = [];
private pendingRequests: Map<string, { resolve: (response: RpcResponse) => void; reject: (error: Error) => void }> = new Map();
private requestId = 0;
private stderr = "";
private exitError: Error | null = null;
private options: RpcClientOptions;
constructor(options: RpcClientOptions = {}) { this.options = options; }
```

#### 2.7.3 `start(): Promise<void>`

1. `if (this.process) throw new Error("Client already started");` 【逐字】`Client already started`
2. `this.exitError = null;`
3. `const cliPath = this.options.cliPath ?? "dist/cli.js";`
4. `const args = ["--mode", "rpc"];`，随后按序追加：`--provider <p>`（若 provider）、
   `--model <m>`（若 model）、`...args`（若 options.args）。
5. `spawn("node", [cliPath, ...args], { cwd: this.options.cwd, env: { ...process.env, ...this.options.env }, stdio: ["pipe","pipe","pipe"] })`
   - `cwd` 为 `undefined` 时继承当前进程 cwd。
   - `env` 合并：`options.env` 的值覆盖同名环境变量；**值为 `undefined` 的键在 Node 中被删除**
     （`spawn` 的 env 处理：`undefined` 值不传给子进程）。
   - 命令字面量是 `"node"`（不是 `process.execPath`）——依赖 PATH。Rust 侧应可配置。
6. `this.process = childProcess;`
7. stderr 收集：`childProcess.stderr?.on("data", (data) => { this.stderr += data.toString(); process.stderr.write(data); })`
   - 用 `data.toString()`（UTF-8 整体解码，非增量；跨 chunk 的多字节字符可能被拆成 U+FFFD）。
   - **镜像**到父进程 stderr（原始 Buffer 写入）。
8. `childProcess.once("exit", (code, signal) => { if (this.process !== childProcess) return; const error = this.createProcessExitError(code, signal); this.exitError = error; this.rejectPendingRequests(error); })`
9. `childProcess.once("error", (error) => { if (this.process !== childProcess) return; const processError = new Error(\`Agent process error: ${error.message}. Stderr: ${this.stderr}\`); ... })`
10. `childProcess.stdin?.on("error", (error) => { if (this.process !== childProcess) return; const stdinError = this.exitError ?? new Error(\`Agent process stdin error: ${error.message}. Stderr: ${this.stderr}\`); ... })`
    - 若已有 `exitError` 则**复用它**（不新建）。
11. `this.stopReadingStdout = attachJsonlLineReader(childProcess.stdout!, (line) => { this.handleLine(line); });`
12. `await new Promise((resolve) => setTimeout(resolve, 100));` —— 固定等待 100ms 让子进程初始化。
13. `if (this.process.exitCode !== null) { const error = this.exitError ?? this.createProcessExitError(this.process.exitCode, this.process.signalCode); this.exitError = error; throw error; }`
    - 子进程在 100ms 内退出 -> `start()` 抛出（错误文本含 code/signal 与 stderr）。

错误文本【逐字】：
- `Agent process exited (code=${code} signal=${signal}). Stderr: ${this.stderr}`（`code`/`signal` 为
  `number|null` / `string|null`，`String(null)` 为 `null`）
- `Agent process error: ${error.message}. Stderr: ${this.stderr}`
- `Agent process stdin error: ${error.message}. Stderr: ${this.stderr}`

#### 2.7.4 `stop(): Promise<void>`

1. 无 process 直接返回。
2. `stopReadingStdout?.(); this.stopReadingStdout = null;`
3. `this.process.kill("SIGTERM");`
4. 等待退出或 1000ms 后 `SIGKILL`：
   ```ts
   await new Promise<void>((resolve) => {
       const timeout = setTimeout(() => { this.process?.kill("SIGKILL"); resolve(); }, 1000);
       this.process?.on("exit", () => { clearTimeout(timeout); resolve(); });
   });
   ```
   注意：`exit` 监听器被**新增**（不清除），每次 `stop` 都加一个；`clearTimeout` 只在 exit 时执行。
5. `this.process = null; this.pendingRequests.clear();`（**不 reject** 未完成请求；它们会因
   `exitError` 在 exit 监听中已被 reject 过；若无 exit 事件则永久挂起）。

#### 2.7.5 `onEvent(listener): () => void`

push 到数组；返回退订函数，用 `indexOf` + `splice(index, 1)`（移除**第一个**匹配项，多次注册同一函数
会有多个条目）。退订幂等（找不到就不动）。

#### 2.7.6 `getStderr(): string`

返回累积的 stderr 字符串（原样，可能含已被父进程 stderr 输出过的内容）。

#### 2.7.7 命令方法（33 个）+ waitForIdle / collectEvents / promptAndWait

所有方法都走 `send(body)` + `getData(response)`。逐条签名与返回：

| 方法 | 发给服务端的命令体 | 返回 |
|---|---|---|
| `prompt(message, images?, streamingBehavior?)` | `{type:"prompt", message, images, streamingBehavior}` | `data.disposition: PromptDisposition` |
| `steer(message, images?)` | `{type:"steer", message, images}` | `data.disposition: QueuedInputDisposition` |
| `followUp(message, images?)` | `{type:"follow_up", message, images}` | `data.disposition` |
| `abort()` | `{type:"abort"}` | `void` |
| `clearQueue()` | `{type:"clear_queue"}` | `data`（`{steering, followUp}`） |
| `newSession(parentSession?)` | `{type:"new_session", parentSession}` | `data`（`{cancelled}`） |
| `getState()` | `{type:"get_state"}` | `RpcSessionState` |
| `setModel(provider, modelId)` | `{type:"set_model", provider, modelId}` | `{provider, id}`（实际是完整 Model） |
| `cycleModel()` | `{type:"cycle_model"}` | `data` |
| `getAvailableModels()` | `{type:"get_available_models"}` | `data.models` |
| `setThinkingLevel(level)` | `{type:"set_thinking_level", level}` | `void` |
| `cycleThinkingLevel()` | `{type:"cycle_thinking_level"}` | `data` |
| `getAvailableThinkingLevels()` | `{type:"get_available_thinking_levels"}` | `data.levels` |
| `setSteeringMode(mode)` | `{type:"set_steering_mode", mode}` | `void` |
| `setFollowUpMode(mode)` | `{type:"set_follow_up_mode", mode}` | `void` |
| `compact(customInstructions?)` | `{type:"compact", customInstructions}` | `data`（CompactionResult） |
| `setAutoCompaction(enabled)` | `{type:"set_auto_compaction", enabled}` | `void` |
| `setAutoRetry(enabled)` | `{type:"set_auto_retry", enabled}` | `void` |
| `abortRetry()` | `{type:"abort_retry"}` | `void` |
| `bash(command)` | `{type:"bash", command}` | `data`（BashResult） |
| `abortBash()` | `{type:"abort_bash"}` | `void` |
| `getSessionStats()` | `{type:"get_session_stats"}` | `data`（SessionStats） |
| `exportHtml(outputPath?)` | `{type:"export_html", outputPath}` | `data`（`{path}`） |
| `switchSession(sessionPath)` | `{type:"switch_session", sessionPath}` | `data`（`{cancelled}`） |
| `fork(entryId)` | `{type:"fork", entryId}` | `data`（`{text, cancelled}`） |
| `clone()` | `{type:"clone"}` | `data`（`{cancelled}`） |
| `getForkMessages()` | `{type:"get_fork_messages"}` | `data.messages` |
| `getEntries(since?)` | `{type:"get_entries", since}` | `data`（`{entries, leafId}`） |
| `getTree()` | `{type:"get_tree"}` | `data`（`{tree, leafId}`） |
| `getLastAssistantText()` | `{type:"get_last_assistant_text"}` | `data.text` |
| `setSessionName(name)` | `{type:"set_session_name", name}` | `void` |
| `getMessages()` | `{type:"get_messages"}` | `data.messages` |
| `getCommands()` | `{type:"get_commands"}` | `data.commands` |

注意：`bash(command)` **不接受** `excludeFromContext`（客户端 API 限制；协议本身支持）。

#### 2.7.8 `waitForIdle(timeout = 60000): Promise<void>`

```ts
return new Promise((resolve, reject) => {
	const timer = setTimeout(() => { unsubscribe(); reject(new Error(`Timeout waiting for agent to become idle. Stderr: ${this.stderr}`)); }, timeout);
	const unsubscribe = this.onEvent((event) => {
		if (event.type === "agent_settled") { clearTimeout(timer); unsubscribe(); resolve(); }
	});
});
```

错误文本【逐字】`Timeout waiting for agent to become idle. Stderr: ${this.stderr}`。
注意 `unsubscribe` 在 timeout 回调中被引用（TDZ 风险：`setTimeout` 40 行后才赋值，但 timeout 至少
下一 tick 触发，故安全）。

#### 2.7.9 `collectEvents(timeout = 60000): Promise<JsonAgentSessionEvent[]>`

收集**所有**事件直到 `agent_settled`（含该事件），超时 reject
【逐字】`Timeout collecting events. Stderr: ${this.stderr}`。

#### 2.7.10 `promptAndWait(message, images?, timeout = 60000)`

```ts
const eventsPromise = this.collectEvents(timeout);
await this.prompt(message, images);
return eventsPromise;
```

先装监听器（避免快速完成丢事件），再发 prompt，最后 `await` 事件。

#### 2.7.11 `handleLine(line)`（私有）

```ts
try {
	const data = JSON.parse(line);
	if (data.type === "response" && data.id && this.pendingRequests.has(data.id)) {
		const pending = this.pendingRequests.get(data.id)!;
		this.pendingRequests.delete(data.id);
		pending.resolve(data as RpcResponse);
		return;
	}
	for (const listener of [...this.eventListeners]) listener(data as JsonAgentSessionEvent);
} catch {
	// Ignore non-JSON lines
}
```

- 只有 `type === "response"` **且** `id` truthy **且** 有 pending 时才算响应；否则一律当事件分发。
- **快照**遍历（`[...this.eventListeners]`），监听器在分发中退订不会导致后续监听器漏事件。
- 任何异常（JSON 解析失败、监听器抛错）都被吞掉——注意监听器抛错会中断**同一行**后续监听器。
- 响应分发后 `return`，不再当事件广播。

#### 2.7.12 `send(command)`（私有）

```ts
const childProcess = this.process;
const stdin = childProcess?.stdin;
if (!childProcess || !stdin) throw new Error("Client not started");
if (this.exitError) throw this.exitError;
if (childProcess.exitCode !== null) {
	const error = this.createProcessExitError(childProcess.exitCode, childProcess.signalCode);
	this.exitError = error;
	throw error;
}
if (stdin.destroyed || !stdin.writable) {
	const error = new Error(`Agent process stdin is not writable. Stderr: ${this.stderr}`);
	this.exitError = error;
	throw error;
}

const id = `req_${++this.requestId}`;
const fullCommand = { ...command, id } as RpcCommand;

return new Promise((resolve, reject) => {
	const timeout = setTimeout(() => {
		this.pendingRequests.delete(id);
		reject(new Error(`Timeout waiting for response to ${command.type}. Stderr: ${this.stderr}`));
	}, 30000);

	this.pendingRequests.set(id, {
		resolve: (response) => { clearTimeout(timeout); resolve(response); },
		reject: (error) => { clearTimeout(timeout); reject(error); },
	});

	try { stdin.write(serializeJsonLine(fullCommand)); }
	catch (error: unknown) {
		const writeError = error instanceof Error ? error : new Error(String(error));
		const pending = this.pendingRequests.get(id);
		this.pendingRequests.delete(id);
		pending?.reject(writeError);
	}
});
```

要点：

1. 检查顺序：未启动 -> `exitError` -> `exitCode !== null` -> stdin 不可写。
2. 请求 id 形如 `req_1`、`req_2`（**没有**补零，进程内单调递增）。
3. `fullCommand` 的键序：`{...command, id}` —— `id` 在**最后**（覆盖原有 id；`command` 是
   `RpcCommandBody`，类型上无 id，但运行时若传入含 id 的对象会被覆盖）。
4. 固定 **30000ms** 请求超时（不可配置）。超时文本【逐字】
   `Timeout waiting for response to ${command.type}. Stderr: ${this.stderr}`。
5. 超时后从 Map 删除；之后到达的响应会被当事件广播（因为 Map 中已无该 id）。
6. 写失败时立即 reject（不等待 30s）。注意 `stdin.write` 是异步失败（回调）时**不会**在这里抛出，
   而是在 `stdin` 的 `error` 事件中 -> `Agent process stdin error: ...` 并 reject 所有 pending。
7. 不检查 `write` 返回值（背压由 Node 内部缓冲）。

#### 2.7.13 `getData<T>(response): T`

```ts
if (!response.success) {
	const errorResponse = response as Extract<RpcResponse, { success: false }>;
	throw new Error(errorResponse.error);
}
const successResponse = response as Extract<RpcResponse, { success: true; data: unknown }>;
return successResponse.data as T;
```

成功但无 `data` 的响应（如 `abort`）返回 `undefined`。`setModel` 的返回类型 `{provider,id}` 是
`getData` 对完整 Model 的“结构性”断言（TS 类型断言，运行时返回整个模型对象）。

---
### 2.8 直接决定本单元行为的邻居：`core/output-guard.ts` 契约（108 行）

本单元所有 stdout 字节都经此模块。它归 ca-session 单元实现，但 **RPC/JSON 的字节正确性依赖它**，
因此在此逐条记录契约（Rust 侧必须提供等价 API）。

```ts
interface StdoutTakeoverState {
	rawStdoutWrite: (chunk: string, callback?: (error?: Error | null) => void) => boolean;
	rawStderrWrite: (chunk: string, callback?: (error?: Error | null) => void) => boolean;
	originalStdoutWrite: typeof process.stdout.write;
}
let stdoutTakeoverState: StdoutTakeoverState | undefined;
const RAW_STDOUT_RETRY_DELAY_MS = 10;
let rawStdoutWriteTail: Promise<void> = Promise.resolve();
```

#### 2.8.1 `takeOverStdout()`

- 幂等（已接管则 return）。
- 捕获真实 `process.stdout.write` / `process.stderr.write`。
- 把 `process.stdout.write` 替换为“写 stderr”的包装：
  - 若第二参数是函数（回调形式），转 `rawStderrWrite(String(chunk), encodingOrCallback)`；
  - 否则 `rawStderrWrite(String(chunk), callback)`。
  - `chunk` 为 `Uint8Array` 时先 `String(chunk)`。**注意**：这里与 `String(Buffer)` **不同**。
    实测（Node v26.12.1）：`String(new Uint8Array([104,105]))` -> `"104,105"`（隐式
    `Uint8Array.prototype.toString()` = 逗号连接的十进制字节），而 `String(Buffer.from("hi"))`
    -> `"hi"`（`Buffer.prototype.toString("utf8")`）。`process.stdout.write` 的 `chunk` 在 Node
    运行时几乎总是 `Buffer`（`Buffer` 是 `Uint8Array` 的子类，但 `String()` 走 `Buffer` 重写的
    `toString`），所以实际路径是 UTF-8 解码；但若有人传入裸 `Uint8Array`，接管后会写成
    `"104,105"` 形态。Rust 侧按“UTF-8 解码 `Buffer`”实现即可，并记录该边界差异。
- 因此接管后任何 `console.log` / `process.stdout.write` 都进 stderr。

#### 2.8.1.1 `console.*` 的具体落点（审计新增）

`takeOverStdout()` 只改写 `process.stdout.write`，因此：
- `console.log` / `console.info` / `console.debug` / `console.dir` 在 Node 里写到
  `process.stdout` 的**写函数**（运行时每次调用都读 `process.stdout.write`），接管后一律进 stderr。
- `console.error` / `console.warn` 写 `process.stderr`，不受影响。
- 直接 `process.stdout.write` 的库代码同样进 stderr（但可能带 Buffer -> `String(Buffer)` UTF-8 解码）。
- `process.stdout.fd`、`process.stdout.isTTY` 等属性**不**被改写；`process.stdout.write` 的返回值
  变成 `rawStderrWrite(...)` 的返回值（布尔背压信号来自 stderr 而非 stdout）。
- 未接管时 `writeRawStdout` 直接写 `process.stdout.write.bind(process.stdout)`（绑定一次，闭包内
  固定），因此**接管之后再调用 `writeRawStdout` 仍写真实 stdout**（`getRawStdoutWrite()` 走的是
  保存的 `rawStdoutWrite`）——这是协议字节不被重定向的关键。`restoreStdout()` 只是把
  `process.stdout.write` 还原，不影响已捕获的 `rawStdoutWrite`。
- `restoreStdout()` 是幂等的（未接管时 no-op）；`isStdoutTakenOver()` 返回布尔。
- **进程内只能有一份接管状态**：`stdoutTakeoverState` 是模块级单例；`restoreStdout` 后再
  `takeOverStdout` 会重新捕获（新的 `originalStdoutWrite`）。

#### 2.8.2 `restoreStdout()` / `isStdoutTakenOver()`

恢复原 `write` / 返回布尔。print 模式返回后由 `main.ts` 调 `restoreStdout()`；RPC 模式不恢复（直接退出）。

#### 2.8.3 `writeRawStdout(text)`

```ts
if (text.length === 0) return;
rawStdoutWriteTail = rawStdoutWriteTail.then(() => writeRawStdoutChunk(text));
void rawStdoutWriteTail.catch(() => { process.exit(1); });
```

- 空串直接丢弃（**不调用**底层 write；这就是 `flushRawStdout` 末尾 `writeRawStdoutChunk("")` 直接写、
  而 `writeRawStdout("")` 无作用的原因）。
- 串行链：每次调用把新的写操作追加到链尾，保证跨调用的字节顺序。
- 链上任何错误 -> `process.exit(1)`（异步、不可捕获）。
- `text.length` 是 UTC-16 长度；空串判定不受影响。

`writeRawStdoutChunk(text)`：

```ts
while (true) {
	try {
		await new Promise<void>((resolve, reject) => {
			try {
				getRawStdoutWrite()(text, (error) => { if (error) reject(error); else resolve(); });
			} catch (error) { reject(error instanceof Error ? error : new Error(String(error))); }
		});
		return;
	} catch (error) {
		const code = (error as {code?: unknown}).code;
		if (code !== "ENOBUFS" && code !== "EAGAIN" && code !== "EWOULDBLOCK") throw error;
		await new Promise<void>((resolve) => setTimeout(resolve, 10));
	}
}
```

- 未接管时 `getRawStdoutWrite()` 返回 `process.stdout.write.bind(process.stdout)`。
- 重试三类背压错误，间隔 10ms，无限重试。
- 其它错误上抛（由链上的 `.catch` 触发 `process.exit(1)`）。
- 写入回调保证“chunk 已交给内核缓冲区”（不保证已 flush 到管道对端）。

#### 2.8.4 `waitForRawStdoutBackpressure()`

```ts
while (true) {
	const tail = rawStdoutWriteTail;
	await tail;
	if (tail === rawStdoutWriteTail) return;
}
```

自旋直到链尾稳定（即等待期间没有新写入追加）。注意：单个 await 之后若又有新写入，会继续等待。

#### 2.8.5 `flushRawStdout()`

```ts
await waitForRawStdoutBackpressure();
await writeRawStdoutChunk("");
```

写空串的用途：确保底层流驱动一次写回调（Node 的 `write("")` 仍会排队一个回调），
从而把已在缓冲区中的字节推出去。

Rust 侧等价设计：

- 一个全局（或进程级）`RawStdout` 单例，内部为 `tokio::sync::mpsc` + 单写任务，或
  `Mutex<Pin<Box<dyn Future>>>` 串行链。
- `write_raw_stdout(&str)`：空串忽略；否则入队并等待完成（TS 侧不等待，因此 Rust 也应
  “发后不管”，但要保留 `wait_for_backpressure` 语义）。
- 重试用 `std::io::ErrorKind::WouldBlock` 或 `raw_os_error()` 匹配 `ENOBUFS(105)` / `EAGAIN(11)` / `EWOULDBLOCK`。
- 错误 -> 直接 `std::process::exit(1)`。

### 2.9 导出符号计数（逐文件）

> 更正：`modes/index.ts` 是 **15** 个名字（§2.1 旧版本误写 14，本表原本就是 15）。其余计数与源码一致。

| 文件 | 导出符号数 | 明细 |
|---|---|---|
| `modes/index.ts` | 15 | `InteractiveMode`*、`InteractiveModeOptions`*、`JsonAgentSessionEvent`、`PrintModeOptions`、`runPrintMode`、`ModelInfo`、`RpcClient`、`RpcClientOptions`、`RpcEventListener`、`runRpcMode`、`RpcCommand`、`RpcExtensionUIRequest`、`RpcExtensionUIResponse`、`RpcResponse`、`RpcSessionState` |
| `modes/json-event.ts` | 2 | `JsonAgentSessionEvent`、`toJsonEvent`（3 个重载声明算 1 个符号） |
| `modes/print-mode.ts` | 2 | `PrintModeOptions`、`runPrintMode` |
| `modes/rpc/jsonl.ts` | 2 | `serializeJsonLine`、`attachJsonlLineReader` |
| `modes/rpc/rpc-types.ts` | 7 | `RpcCommand`、`RpcSlashCommand`、`RpcSessionState`、`RpcResponse`、`RpcExtensionUIRequest`、`RpcExtensionUIResponse`、`RpcCommandType` |
| `modes/rpc/rpc-mode.ts` | 6 | `runRpcMode` + 再导出 5 个类型（`RpcCommand`、`RpcExtensionUIRequest`、`RpcExtensionUIResponse`、`RpcResponse`、`RpcSessionState`） |
| `modes/rpc/rpc-client.ts` | 4 | `RpcClient`、`RpcClientOptions`、`ModelInfo`、`RpcEventListener` |
| **合计（按文件导出声明累加）** | **38** | |

\* `InteractiveMode` / `InteractiveModeOptions` 属 ca-interactive-mode 单元，仅由 `modes/index.ts` 再导出。
本单元**自有**导出符号（去重、不含 `InteractiveMode*`）= **18**：
- 值：`runPrintMode`、`runRpcMode`、`toJsonEvent`、`serializeJsonLine`、`attachJsonlLineReader`、`RpcClient`（6）
- 类型：`PrintModeOptions`、`JsonAgentSessionEvent`、`ModelInfo`、`RpcClientOptions`、`RpcEventListener`、
  `RpcCommand`、`RpcSlashCommand`、`RpcSessionState`、`RpcResponse`、`RpcExtensionUIRequest`、
  `RpcExtensionUIResponse`、`RpcCommandType`（12）
- 去重后按“名字”计为 **18**（6 + 12）；按“文件导出声明累加”计为 **38**（15+2+2+2+7+6+4，含重复再导出）。
Rust 侧 `lib.rs` 应导出上述 18 个去重名（`pub use`），`modes/index.ts` 的 15 个名字作为
`kiana_coding_agent::modes` 模块的再导出面。

---

## 3. 用户可见表面（逐字）

本单元**没有 TUI 渲染、没有 ANSI 颜色、没有 spinner、没有键绑定、没有斜杠命令实现**。
`modes/interactive/**` 属于 ca-interactive-mode 单元。本单元的用户可见表面只有：

1. CLI 标志（由 ca-cli 解析，本单元消费其结果）。
2. print 模式的 stdout 文本与 stderr 错误。
3. 进程退出码。
4. RPC / JSON 协议记录（stdout / stdin 的 JSONL）。
5. `RpcClient` 的 Node API 与错误消息。

### 3.1 CLI 标志（相关子集）

| 标志 | 参数 | 作用 | 归属 |
|---|---|---|---|
| `-p` / `--print` | 无 | 强制 print 模式（含 TTY 时） | ca-cli |
| `--mode` | `text` \| `json` \| `rpc` | 选择输出接口 | ca-cli |
| `--provider` | `<name>` | 传给模型选择；RpcClient 会追加 | ca-cli |
| `--model` | `<pattern>` | 同上 | ca-cli |
| `--no-session` | 无 | 内存会话（RPC 常用） | ca-cli |
| `--session-dir` | `<dir>` | 覆盖会话目录 | ca-cli |
| `-n` / `--name` | `<name>` | 会话显示名 | ca-cli |
| `--offline` | 无 | 关闭启动网络操作（同 `KIANA_OFFLINE=1`），并回写 `KIANA_SKIP_VERSION_CHECK=1` | ca-cli |
| `--export <file> [output]` | 值 | 导出会话为 HTML 后退出，**不进入任何模式** | ca-cli |
| `--models` | `<list>` | scoped models（影响 `cycle_model` 的 `isScoped`） | ca-cli |
| `--approve` / `--no-approve` | 无 | 项目信任覆盖（影响 TUI 首次设置/诊断，间接影响启动） | ca-cli |
| `@path` | — | RPC 模式**拒绝**，JSON/print 允许 | ca-cli |

帮助文本原文（`args.ts:306`–`307`、`337`、`343`，旧版本只给了 `docs/cli.md` 的近似文本）：

```
  --mode <mode>                  Output mode: text (default), json, or rpc
  --print, -p                    Non-interactive mode: process prompt and exit
  --export <file> [output]       Export session file to HTML and exit
  --offline                      Disable startup network operations (same as KIANA_OFFLINE=1)
```

（`--mode` 的说明文字是 `Output mode: text (default), json, or rpc`，与 `docs/cli.md` 里的
`Select text output; still open the terminal UI when stdin and stdout are terminals` **不同**；
后者是文档措辞，不是 `--help` 原文。）

`kiana --help` 的模式段（`docs/cli.md`，非本单元文案，供对照）：

```
--print                  Run the supplied prompts, write the final assistant text to stdout, then exit
--mode text              Select text output; still open the terminal UI when stdin and stdout are terminals
--mode json              Run the supplied prompts, write JSONL events to stdout, then exit
--mode rpc               Read JSONL commands from stdin and write responses and events to stdout until shutdown
```

（实际帮助文本由 ca-cli 的 `printHelp` 生成，本单元不拥有。）

### 3.2 print 模式的 stdout / stderr（逐字）

stdout（每条 text 块）：`${content.text}\n`（内容原样，不 trim、不转义）。

stderr（按触发条件）：

| 条件 | 文本【逐字】 |
|---|---|
| assistant `stopReason === "error"` 且 `errorMessage` 为空 | `Request error` |
| assistant `stopReason === "aborted"` 且 `errorMessage` 为空 | `Request aborted` |
| assistant `stopReason === "error"` / `"aborted"` 且有 `errorMessage` | `${errorMessage}` |
| assistant `stopReason === "length"`（文本已输出后） | `Response was truncated: the model reached the output token limit.` |
| 抛出的 Error | `error.message`（无前缀、无 stack） |
| 抛出的非 Error | `String(error)` |
| 扩展错误（print/json） | `Extension error (${extensionPath}): ${error}` |

每条后跟一个 `\n`（`console.error` 语义）。Rust 侧写 `stderr` 并 flush。

### 3.3 RPC 模式的 stderr

RPC 模式**不**向 stderr 写协议内容。stderr 仅来自：
- `takeOverStdout` 之后被重定向的 `console.*`（扩展或底层库输出）；
- `KIANA_TIMING=1` 的启动计时（ca-cli）；
- 模型目录刷新等后台任务（ca-cli / ca-config-models）。

### 3.4 退出码总表

| 场景 | 退出码 |
|---|---|
| print 成功 | 0 |
| print assistant `error` / `aborted` | 1 |
| print assistant `length` | 1 |
| print 抛错 | 1 |
| JSON 正常完成（含 assistant error/aborted/length） | 0 |
| JSON 抛错 | 1 |
| RPC stdin `end` | 0 |
| RPC 扩展请求关停（`shutdownHandler` -> 等 `agent_settled`/当前命令） | 0 |
| RPC SIGTERM | 143 |
| RPC SIGHUP（非 win32） | 129 |
| print SIGTERM | 143 |
| print SIGHUP（非 win32） | 129 |
| `writeRawStdout` 链上非背压错误 | 1 |
| main.ts 层：无模型 | 1 |
| main.ts 层：RPC + `@file` | 1 |
| 扩展加载失败（`hasRuntimeErrors`） | 1 |
| `RpcClient.start()` 100ms 内子进程退出 | `start()` 抛出（无退出码） |

### 3.5 ANSI / 渲染细节

**本单元无 ANSI 输出**。print 模式写原始文本；JSON/RPC 写 JSONL。唯一相关的 ANSI 行为是
`console.error` 在 TTY 下不添加颜色（Node 的 `console.error` 本身无颜色）。Rust 侧必须**不**添加
任何颜色/加粗/清屏序列。

### 3.6 协议记录（用户可见的“界面”）

RPC 模式下客户端看到的是 §2.5 与 §4.4 描述的全部记录。JSON 模式下客户端看到的是
会话头 + §4.4.2 的事件流。print 模式下客户端只看到最终文本。

---
## 4. 外部格式（逐字）

### 4.1 JSONL 框架

| 项 | 规则 |
|---|---|
| 记录分隔符 | 仅 `\n` (LF, U+000A) |
| 输入兼容 | 每条记录剥离**一个**尾随 `\r`（CRLF 输入） |
| 输出 | 每个记录 = `JSON.stringify(value)` + 单个 `\n`；不写 `\r`、不写 BOM |
| U+2028 / U+2029 | **原样输出**（`JSON.stringify` 不转义），也不作为记录边界 |
| 其它 Unicode 分隔符（U+0085 NEL、U+000B、U+000C） | 同上，仅在字符串内出现，不切分 |
| 空行 | 读入侧以 `""` 回调；RPC 侧 `JSON.parse("")` 失败 -> parse 错误响应 |
| 最后无 LF 的行 | `end` 时若非空则作为一条记录处理 |
| 编码 | UTF-8；增量解码（跨 chunk 不完整序列保留；非法字节 -> U+FFFD） |
| 不支持 | Node `readline`（会额外按 U+2028/U+2029 切分） |

### 4.1.1 `StringDecoder("utf8")` 的替换语义（实测）

RPC 的 stdin 读取与 `RpcClient` 的 stdout 读取都用 `StringDecoder`。它的行为等价于 WHATWG
UTF-8 解码 + “每个 maximal subpart 一个 U+FFFD”。实测（Node v26.12.1）：

| 输入字节序列（十六进制） | `write` + `end` 输出（码位） |
|---|---|
| `e2`（3 字节序列只到 1 字节即 EOF） | `U+FFFD` |
| `e2 28` | `U+FFFD`, `(` |
| `e2 82` 然后 `41` | `U+FFFD`, `A` |
| `e2 82` 然后 `a9`（跨 chunk 补齐） | `U+20A9`（合法，无替换） |
| `ff ff ff` | `U+FFFD` ×3（每字节一个） |
| `c0 af`（overlong 2 字节） | `U+FFFD` ×2 |
| `e0 80 80`（overlong 3 字节） | `U+FFFD` ×3 |
| `ed a0 80`（UTF-16 代理区编码） | `U+FFFD` ×3 |
| `f0 80 80 80`（overlong 4 字节） | `U+FFFD` ×4 |
| `f4 90 80 80`（> U+10FFFF） | `U+FFFD` ×4 |
| `e2 82 41 42` | `U+FFFD`, `A`, `B` |
| `f0 9f 92 41` | `U+FFFD`, `A`（截断序列整体一个替换） |
| `f0 9f` 即 EOF | `U+FFFD`（单个） |
| `c2` 即 EOF | `U+FFFD`（单个） |
| `80`（孤立续字节） | `U+FFFD` |
| `e2 82` 未 `end()` | 输出为空（字节留在内部缓冲） |

要点：**不完整序列留在 decoder 内部**，直到下一 chunk 或 `end()`；`end()` 对被截断的序列只补
**一个** U+FFFD（不管缺几个字节），但对“无效起始/续字节”每个字节一个。Rust 用 `encoding_rs`
时必须逐行对拍本表（`encoding_rs` 的 `decode_to_string_without_replacement` 与
`last=true` 的替换计数与 Node 有差异）。

### 4.2 `JSON.stringify` 语义（必须桥接）

本单元所有 stdout 记录都由 `JSON.stringify` 产生（单参、无 replacer、无缩进）。

#### 4.2.1 键顺序

- 对象键按**插入顺序**输出。
- **整数型键**先输出，按数值升序，然后才是其余键（按插入顺序）。
  “整数型键”= 规范形式（`String(n)` 能还原，无前导零、无 `+`、无小数点）且
  `0 <= n <= 2^32 - 2`（即 `4294967294`）。探测结果：
  `{b:1, 2:2, a:undefined, 1:1, "01":3, "4294967294":4, "4294967295":5, "-1":6, "__proto__":7}`
  ->
  `{"1":1,"2":2,"4294967294":4,"b":1,"01":3,"4294967295":5,"-1":6}`
  （`a: undefined` 被丢弃；`__proto__` 作为普通自有键保留，但本例中未出现在输出里——
  因为对象字面量里的 `__proto__: 7` 设置的是原型而非自有属性。Rust 侧用 `Map`/`IndexMap`
  保存时 `__proto__` 会被当作普通键，**这是可接受的差异**，因为协议记录里不会出现该键。）
- 本单元的记录都是**扁平或浅层**结构，键序由构造字面量决定（§2.5 / §2.6 已逐条列出）。
  Rust 侧必须用保序映射（`serde_json` 的 `preserve_order`）或按固定顺序的 `Serialize` 结构体。

#### 4.2.2 值的序列化

| JS 值 | 输出 |
|---|---|
| `undefined`（对象属性） | 键被丢弃 |
| `undefined`（数组元素） | `null` |
| 函数 / symbol（对象属性） | 键被丢弃 |
| 函数 / symbol（数组元素） | `null` |
| 顶层 `undefined` / 函数 / symbol | `JSON.stringify` 返回 `undefined`（`serializeJsonLine` 得到 `"undefined\n"`） |
| `NaN` / `Infinity` / `-Infinity` | `null` |
| `-0` | `0` |
| `null` | `null` |
| `true` / `false` | `true` / `false` |
| 字符串 | 见 4.2.3 |
| 数字 | 见 4.2.4 |
| 数组 | `[a,b,...]`，无空格 |
| 普通对象 | `{"k":v,...}`，无空格；`toJSON()` 存在时先调用 |
| `Date` | `toJSON()` = `toISOString()` -> `"YYYY-MM-DDTHH:mm:ss.sssZ"` |
| `BigInt` | 抛 `TypeError: Do not know how to serialize a BigInt`（本单元不产生） |

#### 4.2.3 字符串转义

| 码位 | 输出 |
|---|---|
| `"` (U+0022) | `\"` |
| `\` (U+005C) | `\\` |
| U+0008 | `\b` |
| U+0009 | `\t` |
| U+000A | `\n` |
| U+000C | `\f` |
| U+000D | `\r` |
| 其它 U+0000–U+001F | `\u00XX`（小写十六进制，4 位，如 `\u0000`、`\u001f`） |
| 孤立代理项（U+D800–U+DFFF 未配对） | `\udXXX`（**小写**，如 `\ud800`） |
| U+2028 / U+2029 | **原样**（不转义） |
| U+007F、U+0080–U+009F、U+00A0、U+FEFF | **原样** |
| 其它 | 原样 UTF-8 |

探测确认：`JSON.stringify("a b c")` -> `"a b c"` 的原文（引号内为真实字符）；
`JSON.stringify("\u0000\u0001\u0007\u000b\u001f")` -> `"\u0000\u0001\u0007\u000b\u001f"`（`\b` 只用于 U+0008）；
`JSON.stringify("\ud800")` -> `"\ud800"`。

Rust 注意：`serde_json` 默认会把 `\u007f` 及以上原样输出（符合）；但会对孤立代理项报错
（Rust `String` 不能容纳），因此 Rust 侧内部字符串必须是合法 UTF-8——若需要复刻孤立代理项，
需在类型层用 `Vec<u16>`（见 §8.4）。

#### 4.2.4 数字格式化（ECMAScript `Number::toString`）

规则（最短往返表示，`Number.prototype.toString()` 语义）：

- 整数且 `|n| < 1e21`：十进制无小数点，如 `100`、`123456789012345680000`。
- `1e-6 <= |n| < 1e21`：十进制表示（必要时带小数点），如 `0.000001`、`1e-5` -> `0.00001`、
  `0.1`、`0.3333333333333333`。**下界是 `1e-6`（含）**：实测 `1e-6` -> `0.000001`、
  `1.5e-6` -> `0.0000015`、`1.0000001e-6` -> `0.0000010000001`。
- `|n| >= 1e21`：指数形式带 `+`，如 `1e+21`、`1.2345678901234569e+23`。
- `|n| < 1e-6`（**严格小于**，不是“即 `<= 1e-7`”；例如 `5e-7`、`5.5e-7`、`9.99e-7` 均为指数形式）：
  指数形式，如 `1e-7`、`5e-7`、`5e-324`、`-1e-7`、`2.5e-8`。
  旧版本此处写成“即 `<= 1e-7`”在区间 `1e-7 < |n| < 1e-6`（如 `5e-7`）上会判错。
- 最大/最小：`1.7976931348623157e+308`、`5e-324`。
- 指数部分无前导零（`1e-7` 不是 `1e-07`）；`+` 只用于非负指数。
- `-0` -> `0`；`NaN`/`±Infinity` -> `null`（见 4.2.2）。

Rust：`serde_json` 使用 `ryu`，输出格式与 JS 在多数值上一致，但有差异：
- `ryu` 对 `1e21` 输出 `1e21`（无 `+`）；JS 输出 `1e+21`。
- `ryu` 对 `1e-7` 输出 `1e-7`（一致）。
- `ryu` 可能输出 `1.0`（`f64` 整数值），JS 输出 `1`。
**必须**自行实现“JS 数字格式化”或对 `serde_json::Number` 做后处理（见 §8.4 第 2 项）。

#### 4.2.5 与 `String(value)` 的关系

`String(n)` 与 `JSON.stringify(n)` 对有限数一致（除 `-0`：`String(-0)` = `"0"`，一致）。
`String(undefined)` = `"undefined"`、`String(null)` = `"null"`、`String(true)` = `"true"`。
模板字符串插值用 `String`，因此错误消息里的数字/布尔是十进制表示。

#### 4.2.6 `String.prototype.trim()`

`set_session_name` 使用 `.trim()`。JS 的 `trim` 去除的字符集（`WhiteSpace` + `LineTerminator`）：
U+0009、U+000A、U+000B、U+000C、U+000D、U+0020、U+00A0、U+1680、U+2000–U+200A、U+2028、U+2029、
U+202F、U+205F、U+3000、**U+FEFF**。探测确认全部被去除。
Rust 的 `str::trim` 用 `char::is_whitespace`，**不含 U+FEFF**，且包含 U+0085（JS **不**包含）。
必须自行实现该字符集。

### 4.3 `JSON.parse` 错误消息（必须桥接）

RPC 的解析失败响应文本为【逐字】`Failed to parse command: ${message}`，其中 `message` 是 V8 的错误消息。
下表在 **Node v26.12.1 / V8 14.6** 上实测（本机唯一可用运行时；源包 `engines: node >= 22.19.0`）。
Node 22 的部分消息不同（例如 `Unexpected token } in JSON at position N` 旧式），
**必须以移植目标的实际运行时为准**，并把黄金测试锁定到同一版本。

| 输入 | 消息 |
|---|---|
| `""` / `"  "` / `"\t"` | `Unexpected end of JSON input` |
| `"{"` | `Expected property name or '}' in JSON at position 1 (line 1 column 2)` |
| `"}"` | `Unexpected token '}', "}" is not valid JSON` |
| `'{"a"}'` | `Expected ':' after property name in JSON at position 4 (line 1 column 5)` |
| `'{"a":}'` | `Unexpected token '}', "{"a":}" is not valid JSON` |
| `'{"a":1,}'` | `Expected double-quoted property name in JSON at position 7 (line 1 column 8)` |
| `"[1,]"` | `Unexpected token ']', "[1,]" is not valid JSON` |
| `"tru"` / `"nul"` | `Unexpected end of JSON input` |
| `"NaN"` | `"NaN" is not valid JSON` |
| `"undefined"` | `"undefined" is not valid JSON` |
| `"01"` | `Unexpected number in JSON at position 1 (line 1 column 2)` |
| `"1."` | `Unterminated fractional number in JSON at position 2 (line 1 column 3)` |
| `".5"` | `Unexpected token '.', ".5" is not valid JSON` |
| `"1e"` | `Exponent part is missing a number in JSON at position 2 (line 1 column 3)` |
| `'"a'` | `Unterminated string in JSON at position 2 (line 1 column 3)` |
| `'"a\\q"'` | `Bad escaped character in JSON at position 3 (line 1 column 4)` |
| `"{a:1}"` | `Expected property name or '}' in JSON at position 1 (line 1 column 2)` |
| `'{"a" 1}'` | `Expected ':' after property name in JSON at position 5 (line 1 column 6)` |
| `"{} x"` / `"{} {}"` / `"123abc"` | `Unexpected non-whitespace character after JSON at position 3 (line 1 column 4)` |
| `"-"` / `"-x"` | `No number after minus sign in JSON at position 1 (line 1 column 2)` |
| `"+1"` | `Unexpected token '+', "+1" is not valid JSON` |
| `"[1 2]"` | `Expected ',' or ']' after array element in JSON at position 3 (line 1 column 4)` |
| `"{,}"` | `Expected property name or '}' in JSON at position 1 (line 1 column 2)` |
| `'"\\u12"'` | `Bad Unicode escape in JSON at position 5 (line 1 column 6)` |
| `"1e400"` | 成功（`Infinity`） |
| `'{"a":1}\n'` | 成功（尾随空白允许） |

补充规则（V8 实现细节，**本审计在 Node v26.12.1 上逐条实测**）：

- 消息里的 `position` 是 **0 基** UTF-16 索引；`column` 是 `position` 在所在行的 **1 基**列号，
  `line` 是 1 基行号。行终止符集合 = `\n` 与 `\r`（**不是** U+2028/U+2029，也**不是** U+0085；
  `\r\n` 计一次换行）。例：`"[1,\n2 3]"` -> `position 6`、`line 2`、`column 3`；
  `"[1,\r\r2 3]"` -> `position 7`、`line 3`、`column 3`；`"[1,\r\n2 3]"` -> `position 7`、`line 2`。
  `position` 是 UTF-16 码元索引（含代理对时按 2 计），与 Rust 的 `char` 索引不同，必须先转 UTF-16。
- `is not valid JSON` 形式里引用的片段（`"..."`）规则（实测）：
  - 当整个输入长度 **`<= 20` UTF-16 码元**时：引用**整个**输入，无省略号。
  - 长度 **`>= 21`** 时：取错误位置附近 **20 码元**窗口，即 `[errIndex-10, errIndex+9]`（含两端），
    左边界截断时前缀 `...`，右边界截断时后缀 `...`（标记随截断一侧独立出现）。
    **不是**“10 码元窗口”；旧版本此处描述有误。验证：`"[" + "0,"*k + "x" + ",0"*m + "]"` 在
    `k=m=20` 时片段为 `0,0,0,0,0,x,0,0,0,0,`（左 10 码元 + `x` + 右 9 码元）。
    边界实例（错误在 index 5）：len 20（`[0,0,x0000000000000]`）整段引用、无省略号；
    len 21（`[0,0,x00000000000000]`）片段为 `[0,0,x000000000` 加后缀 `...`。
  - 窗口按 UTF-16 码元切，**可能把代理对切成孤立代理项**：实测
    `["😀😀😀😀😀","aaaa",x]` 的片段以孤立低代理 `\ude00` 开头（JSON 里写作 `"\ude00"`）。
  - 片段内部按 JSON 字符串转义展示：`"` -> `\"`、`\` -> `\\`，**但 `\t`、`\n`、`\r` 等控制字符
    以**原文**（真实 TAB/LF/CR 字节）出现在消息里，不转义；错误 token 本身的字符也原样嵌入
    `Unexpected token '<ch>'` 的单引号内（孤立代理项/情感符号的首个 UTF-16 码元同理，如
    `Unexpected token '\ud83d'`）。
  - 片段前后缀省略号的确切文本是 `, ..."` 与 `"... is not valid JSON`（即 `...` 紧贴引号）。
- 仅 **`Unexpected token`** 与 `"<input>" is not valid JSON` 这两类消息带片段；
  `Expected ...` / `Unexpected number ...` / `Unterminated ...` / `Bad ...` 等一律只带
  `position`/`line`/`column`，不带片段。
- **JSON 白名单空白只有 4 个**：U+0009 TAB、U+000A LF、U+000D CR、U+0020 SPACE。
  U+00A0、U+000B、U+000C、U+0085、U+2028、U+2029、U+FEFF（BOM）**都不是**空白：
  出现在结构位置会被当作 `Unexpected token` 报错（实测），出现在字符串内则合法。
- BOM 在开头不会被跳过：`JSON.parse("﻿{}")` -> `Unexpected token '﻿', "﻿{}" is not valid JSON`。
- `JSON.parse` 允许重复键（后者覆盖）、允许字符串内的孤立代理项转义（`"\ud800"` 解析为
  单码元字符串，再 `JSON.stringify` 回 `"\ud800"`）。
- 顶层 `null` / `true` / `false` / 数字 / 字符串 / 数组解析成功后，RPC 侧按 §2.5.1 的边界处理。
- `1e400` 解析为 `Infinity`（成功）；之后 `JSON.stringify` 会写成 `null`（§4.2.2）。

**结论**：Rust 无法直接复用 `serde_json` 的错误消息（文本完全不同）。必须实现一个
“错误分类 + 位置计算 + 消息拼装”的兼容层（见 §8.4 第 1 项）。最小可行方案：自己写 JSON 解析器，
在错误点产出与 V8 同构的 `(kind, position)`，再套模板。

### 4.4 wire 记录（逐字）

#### 4.4.1 `response`

```json
{"id":"req-1","type":"response","command":"get_state","success":true,"data":{...}}
{"id":"req-2","type":"response","command":"set_model","success":false,"error":"Model not found: invalid/model"}
{"type":"response","command":"parse","success":false,"error":"Failed to parse command: Unexpected end of JSON input"}
```

- 键序：`id`（可缺）、`type`、`command`、`success`、`data` 或 `error`。
- 解析错误记录：**无 id**，`command: "parse"`。
- 未知命令：`command` = 未知的 type 字符串，`error: "Unknown command: <type>"`，id 保留。

#### 4.4.2 会话事件（JSON 与 RPC 共用；RPC 无会话头）

JSON 模式首条记录（`SessionHeader`，仅当 `getHeader()` 非 null）：

```json
{"type":"session","version":3,"id":"<uuid-v7>","timestamp":"2024-12-03T14:00:00.000Z","cwd":"/path"}
```

键序：`type`、`version`、`id`、`timestamp`、`cwd`、`parentSession?`。
`version` 当前为 `3`（`CURRENT_SESSION_VERSION = 3`）；`id` 为 `uuidv7()`；
`timestamp` 为 `new Date().toISOString()`（`YYYY-MM-DDTHH:mm:ss.sssZ`，UTC，毫秒 3 位）。

事件记录（`toJsonEvent` 之后）：

| `type` | 键（按序） |
|---|---|
| `agent_start` | `type` |
| `agent_end` | `type`、`messages`、`willRetry` |
| `agent_settled` | `type`、`aborted` |
| `turn_start` | `type` |
| `turn_end` | `type`、`message`、`toolResults` |
| `message_start` | `type`、`message` |
| `message_update` | `type`、`usage`、`assistantMessageEvent` |
| `message_end` | `type`、`message` |
| `tool_execution_start` | `type`、`toolCallId`、`toolName`、`args`、`parentToolCallId?` |
| `tool_execution_update` | `type`、`toolCallId`、`toolName`、`args`、`partialResult`、`parentToolCallId?` |
| `tool_execution_end` | `type`、`toolCallId`、`toolName`、`result`、`isError`、`durationMs?`、`parentToolCallId?` |
| `queue_update` | `type`、`steering`、`followUp` |
| `entry_appended` | `type`、`entry` |
| `session_info_changed` | `type`、`name`（`undefined` -> 键缺失） |
| `thinking_level_changed` | `type`、`level` |
| `compaction_start` | `type`、`reason` |
| `compaction_end` | `type`、`reason`、`result`（`undefined` -> 键缺失）、`aborted`、`willRetry`、`errorMessage?` |
| `auto_retry_start` | `type`、`attempt`、`maxAttempts`、`delayMs`、`errorMessage` |
| `auto_retry_end` | `type`、`success`、`attempt`、`finalError?` |
| `summarization_retry_scheduled` | `type`、`attempt`、`maxAttempts`、`delayMs`、`errorMessage` |
| `summarization_retry_attempt_start` | `type`、`source`、`reason?`（`compaction` 时带 `reason`；`branchSummary` 时无） |
| `summarization_retry_finished` | `type` |
| `bash_execution_update`（RPC only） | `type`、`id?`、`delta` |

`assistantMessageEvent`（`message_update` 内层；`partial` 一律移除）：

| 内层 `type` | 键 |
|---|---|
| `start` | `type`（`partial` 移除后无其它键） |
| `text_start` / `thinking_start` | `type`、`contentIndex` |
| `text_delta` / `thinking_delta` | `type`、`contentIndex`、`delta` |
| `text_end` / `thinking_end` | `type`、`contentIndex`、`content` |
| `toolcall_start` | `type`、`contentIndex`、`id`、`toolName` |
| `toolcall_delta` | `type`、`contentIndex`、`delta` |
| `toolcall_end` | `type`、`contentIndex`、`toolCall` |
| `done` | `type`、`reason`、`message` |
| `error` | `type`、`reason`、`error` |

`usage` 对象键序：`input`、`output`、`cacheRead`、`cacheWrite`、`cacheWrite1h?`、`reasoning?`、
`totalTokens`、`cost`（`{input, output, cacheRead, cacheWrite, total, totalFromProvider?}`）。

`Model` 对象键序（`get_state` / `set_model` / `cycle_model` / `get_available_models`）：
由**运行时对象的实际构造顺序**决定（`JSON.stringify` 保留插入顺序，与 TS 类型声明顺序无关）。
`BaseModel` 的声明顺序为 `id`、`name`、`api`、`provider`、`baseUrl`、`input`、`inputLimits?`、
`cost`、`headers?`；`Model` 追加 `type?`、`reasoning`、`thinkingLevelMap?`、`promptCache?`、
`contextWindow`、`maxTokens`、`samplingParams?`、`samplingParamsByThinkingLevel?`、`compat?`。
实际 wire 上看到的顺序（`docs/rpc-commands.md` 的 Model 示例）为
`id`、`name`、`api`、`provider`、`baseUrl`、`reasoning`、`input`、`contextWindow`、`maxTokens`、`cost`。
`cost` 为 `{input, output, cacheRead, cacheWrite}`。
（完整定义归 ai-core；Rust 侧必须按**模型目录/模型运行时构造出的实际顺序**透传，不得重排。
建议做法：模型对象在 Rust 侧用 `IndexMap<String, Value>` 或保序结构体，
并在 ca-config-models 的黄金测试里锁定该顺序。）

#### 4.4.3 `extension_ui_request`（stdout）

见 §2.5.5；键序 `type`、`id`、`method`、其余。

示例（握手，来自 `docs/rpc-extension-ui.md`）：

```json
{"type":"extension_ui_request","id":"uuid-1","method":"select","title":"Allow dangerous command?","options":["Allow","Block"],"timeout":10000}
{"type":"extension_ui_request","id":"uuid-2","method":"confirm","title":"Clear session?","message":"All messages will be lost.","timeout":5000}
{"type":"extension_ui_request","id":"uuid-3","method":"input","title":"Enter a value","placeholder":"type something..."}
{"type":"extension_ui_request","id":"uuid-4","method":"editor","title":"Edit some text","prefill":"Line 1\nLine 2\nLine 3"}
{"type":"extension_ui_request","id":"uuid-5","method":"notify","message":"Command blocked by user","notifyType":"warning"}
{"type":"extension_ui_request","id":"uuid-6","method":"setStatus","statusKey":"my-ext","statusText":"Turn 3 running..."}
{"type":"extension_ui_request","id":"uuid-7","method":"setWidget","widgetKey":"my-ext","widgetLines":["--- My Widget ---","Line 1","Line 2"],"widgetPlacement":"aboveEditor"}
{"type":"extension_ui_request","id":"uuid-8","method":"setTitle","title":"kiana - my project"}
{"type":"extension_ui_request","id":"uuid-9","method":"set_editor_text","text":"prefilled text for the user"}
```

#### 4.4.4 `extension_ui_response`（stdin）

```json
{"type":"extension_ui_response","id":"uuid-1","value":"Allow"}
{"type":"extension_ui_response","id":"uuid-2","confirmed":true}
{"type":"extension_ui_response","id":"uuid-3","cancelled":true}
```

服务端只检查 `type` 与 `id`；其余字段由 `parseResponse` 解释。

#### 4.4.5 `extension_error`（RPC only，stdout）

```json
{"type":"extension_error","extensionPath":"/path/to/extension.ts","event":"tool_call","error":"Error message"}
```

键序：`type`、`extensionPath`、`event`、`error`。**无 `stack`**（`ExtensionError.stack` 被丢弃）。

#### 4.4.6 `bash_execution_update`（RPC only）

```json
{"type":"bash_execution_update","id":"req-1","delta":"total 48\n"}
```

`id` 来自 `bash` 命令的 `id`；命令无 id 时键缺失。

### 4.4.7 `extension_ui_request` 的 fire-and-forget 记录与 pending 记录的区别

- 9 种 method 里，`notify` / `setStatus` / `setWidget` / `setTitle` / `set_editor_text` 是
  **fire-and-forget**：每次都生成新 `id`，但**不**登记 `pendingExtensionRequests`。客户端若回
  `extension_ui_response` 给这些 id，服务端查不到 -> 静默忽略。
- `select` / `confirm` / `input` / `editor` 是 **pending**：登记后才发出，客户端回包按 id resolve。
- 两类 id 共用同一 `crypto.randomUUID()` 空间与同一 Map。
- `statusText` / `widgetLines` 为 `undefined` 时对应键缺失但记录仍发送（见 §2.5.5）；
  `notifyType` / `widgetPlacement` 同理。
- 所有 `extension_ui_request` 都经 `writeRawStdout` 串行链，因此与同期的 `response`/事件保持调用顺序。

### 4.4.8 会话事件在 RPC 与 JSON 的差异（澄清）

| 差异 | JSON 模式 | RPC 模式 |
|---|---|---|
| 会话头（`type:"session"`） | 有（`getHeader()` 非空时，且在 rebind 之前） | **无** |
| `bash_execution_update` | 会写出（若发生；print/json 的 prompt 不触发 bash 工具） | 有（RPC `bash` 命令时） |
| `extension_error` | **无**（`onError` 走 stderr） | 有（stdout 记录） |
| `message_update` 投影 | 相同 | 相同 |
| 订阅过滤 | 无（json 全量；text 不订阅） | 无（全量） |

### 4.5 环境变量

| 变量 | 作用 | 本单元关系 |
|---|---|---|
| `KIANA_CODING_AGENT` | `"true"` 标记（`rpc-entry.ts` 设置） | RPC 独立入口设置；供子进程检测 |
| `AI_AGENT` | `"kiana"` 通用标记（`rpc-entry.ts` 设置） | 同上 |
| `KIANA_CODING_AGENT_DIR` | 覆盖配置目录（默认 `~/.kiana/agent`） | `RpcClient` 测试用它隔离会话目录 |
| `KIANA_CODING_AGENT_SESSION_DIR` | 覆盖会话目录（`--session-dir` 优先） | 同上 |
| `KIANA_TIMING` | `"1"`（严格等于 `"1"`，`KIANA_TIMING=true` **不**生效）时输出启动计时到 stderr | RPC/print 均受影响（`printTimings()`）；格式见下方 |
| `KIANA_OFFLINE` | 真值时禁用网络活动 | RPC 模式下跳过模型目录刷新；同时**写回** `process.env.KIANA_OFFLINE = "1"` 与 `process.env.KIANA_SKIP_VERSION_CHECK = "1"`（子进程继承） |
| `KIANA_SKIP_VERSION_CHECK` | 跳过 `kiana.dev` 版本检查 | `main.ts` 在 offline 时自动设置；本单元只间接受影响 |
| `KIANA_PACKAGE_DIR` | 覆盖包目录（Nix/Guix 场景） | 影响 `getDocsPath()`（错误消息里的 `/providers.md` 路径）与主题/示例路径 |
| `KIANA_TELEMETRY` | `1`/`true`/`yes` 或 `0`/`false`/`no` | 与协议无关，但 `isTruthyEnvFlag` 的三值语义与本单元的 `KIANA_OFFLINE`/`KIANA_STARTUP_BENCHMARK` 共用 |
| `AI_AGENT` | `"kiana"`（`rpc-entry.ts` / `cli/setup.ts` 设置） | 与 `KIANA_CODING_AGENT` 成对出现；**非 RPC 专属**：普通 `cli.ts` 也设置 |
| `KIANA_SESSION_ID` / `KIANA_SESSION_FILE` / `KIANA_PROVIDER` / `KIANA_MODEL` / `KIANA_REASONING_LEVEL` | shell 工具环境 | **不是** RPC `bash` 命令注入的（见 §2.6.7 的 `bash`），只由 LLM 可调用的 `bash`/`powershell` 工具注入 |

`printTimings()` 的输出格式（`src/core/timings.ts`，全部写 stderr）：
先 `console.error("\n--- <title> ---")`（`title` 为 `Startup Timings: main` / `Startup Timings: extensions`），
每项 `console.error("  <label>: <ms>ms")`，再 `console.error("  TOTAL: <sum>ms")`，最后
`console.error("-".repeat(title.length + 8) + "\n")`。ms 为整数（`Date.now()` 差值）；
负值项被过滤；某组无条目则**整组不输出**。`KIANA_TIMING` 未设时全部 no-op。
`printTimings()` 在 RPC 分支里于 `runRpcMode` **之前**调用（`main.ts:950`–`952`），print 分支同样
（`main.ts:987`）。
| `KIANA_STARTUP_BENCHMARK` | 真值时仅支持 interactive；非 interactive 报错退出 | RPC/print 会报错 `Error: KIANA_STARTUP_BENCHMARK only supports interactive mode` |
| `FORCE_COLOR` / `NO_COLOR` / `TERM` / `CI` | chalk 颜色级别 | 本单元不产生颜色；仅影响 ca-cli 的 stderr 诊断 |

`isTruthyEnvFlag(value)`：`!value` -> false；否则 `value === "1" || value.toLowerCase() === "true" || value.toLowerCase() === "yes"`。

### 4.6 `rpc-entry.ts`（独立 RPC 入口）

```ts
#!/usr/bin/env node
import { APP_NAME } from "./config.ts";
import { configureHttpDispatcher } from "./core/http-dispatcher.ts";
import { main } from "./main.ts";

process.title = `${APP_NAME}-rpc`;
process.env.KIANA_CODING_AGENT = "true";
process.env.AI_AGENT = "kiana";
process.emitWarning = (() => {}) as typeof process.emitWarning;

configureHttpDispatcher();

main(["--mode", "rpc", ...process.argv.slice(2)]);
```

- `process.title` = `"kiana-rpc"`（`APP_NAME` 默认 `"kiana"`，可被 `package.json` 的 `kianaConfig.name` 重命名；
  当前仓库的 `kianaConfig` 只有 `configDir`，无 `name`）。
- **覆盖** `process.emitWarning` 为空函数（吞掉所有 Node 警告）。
- `main(["--mode","rpc", ...args])`：入口参数前置 `--mode rpc`，因此用户再传 `--mode` 会覆盖
  （`parseArgs` 后者覆盖前者）。
- `package.json` 的 `exports["./rpc-entry"]` 指向 `dist/bundle/rpc-entry.js`。

本单元不拥有该文件（ca-cli 单元），但 Rust 侧若提供独立 RPC 二进制/入口，必须复刻以上 4 项副作用。

**旧版本遗漏**：普通 CLI 入口 `src/cli.ts`（`bin: {"kiana": "dist/bundle/cli.js"}`）调用
`setupCli()`（`src/cli/setup.ts`，13 行），后者执行与 `rpc-entry.ts` **相同的 4 项副作用**：
`process.title = APP_NAME`（不带 `-rpc`）、`KIANA_CODING_AGENT="true"`、`AI_AGENT="kiana"`、
`process.emitWarning = () => {}`，外加 `configureHttpDispatcher()`。也就是说
“吞掉所有 Node 警告”与两个进程标记**不是 RPC 专属**，任何 CLI 模式都有。
`package.json` 的 `exports` 只有 `"."` 与 `"./rpc-entry"`；`bin` 只有 `kiana -> dist/bundle/cli.js`（bundle 路径，
与 `exports` 的 `dist/index.js` 不同）。`kianaConfig` 里只有 `configDir: ".kiana"`，**没有** `name`，
所以 `APP_NAME` 默认就是 `"kiana"`（改名需在 fork 的 `package.json` 加 `kianaConfig.name`）。
相关的环境变量名也是派生的：`ENV_AGENT_DIR = ${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`、
`ENV_SESSION_DIR = ${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR`（`config.ts:585`–`586`），
Rust 侧若要支持 rebrand 必须用同一派生规则（测试里硬编码了 `KIANA_CODING_AGENT_DIR`）。

### 4.7 信号与进程语义

| 信号 | print 模式 | RPC 模式 |
|---|---|---|
| SIGTERM | `killTrackedDetachedChildren()` -> dispose -> `exit(143)` | 同左 + `shutdown(143, "SIGTERM")`（不 flush） |
| SIGHUP（非 win32） | `killTrackedDetachedChildren()` -> dispose -> `exit(129)` | `shutdown(129, "SIGHUP")`（flush） |
| win32 | 只有 SIGTERM | 只有 SIGTERM |
| Ctrl+C (SIGINT) | **不处理**（Node 默认终止：退出码 130） | **不处理**（默认终止；注意 RPC 模式下 stdin 是管道，Ctrl+C 由终端驱动产生 SIGINT） |
| SIGPIPE | 不处理（Node 忽略 SIGPIPE；写管道失败以 EPIPE 错误事件出现，由 `writeRawStdout` 的 `.catch` -> `process.exit(1)` 兜底） | 同左 |
| 退出前 flush | `finally` 里 `await flushRawStdout()`；但 `process.exit` 路径（129/143）**不** flush | 见 §2.6.9（仅 SIGTERM 不 flush） |

补充：`print-mode.ts` 的信号处理器**只**注册在 `runPrintMode` 内（不在 `main.ts`），因此
`--export`/`--help` 等提前退出路径没有这些处理器。两处都只注册 SIGTERM（所有平台）
+ SIGHUP（非 win32），**没有** SIGINT/SIGQUIT。

`killTrackedDetachedChildren()`：遍历被跟踪的 detached 子进程 PID，`killProcessTree(pid)`
（win32 用 `System32/taskkill.exe /F /T /PID`；其它平台杀进程组），随后清空集合。

### 4.8 背压与缓冲

- 服务端：`writeRawStdout` 串行链 + 背压重试（ENOBUFS/EAGAIN/EWOULDBLOCK，10ms）；
  `waitForRawStdoutBackpressure()` 在每条响应后与每个 agent 事件后调用（后者只在订阅里）。
- 客户端：`stdin.write` 返回值被忽略（Node 内部缓冲）；stdout 由 `attachJsonlLineReader` 持续消费。
- 文档要求：客户端必须持续读 stdout，否则管道缓冲填满会阻塞服务端。
- **`RpcClient` 不实现扩展 UI 子协议**（旧版本未点明）：它没有发送 `extension_ui_response` 的方法
  （`rpc-client.ts` 里 `extension_ui_response` 出现 0 次），也**不对** `extension_ui_request` 做任何处理——
  这些记录会原样作为“事件”广播给 `onEvent` 监听器（无 `id` 匹配 pending，故走事件分支）。
  需要应答扩展对话框的客户端必须自己写 JSONL（参考 `examples/rpc-extension-ui.ts`，661 行）。
  Rust 侧 `RpcClient` 应**照抄该缺失**（或明确记为增强），否则事件流/行为与 TS 不一致。

---
## 5. 跨单元接口

本单元**消费**下列接口；Rust 侧对应 crate 必须提供等价签名（名称可调整，语义不可调整）。

### 5.1 `AgentSession`（ca-session）

#### 5.1.1 被 RPC 命令直接调用的方法

```ts
async prompt(text: string, options?: PromptOptions): Promise<void>;
async steer(text: string, images?: ImageContent[], options?: { source?: InputSource }): Promise<QueuedInputDisposition>;
async followUp(text: string, images?: ImageContent[], options?: { source?: InputSource }): Promise<QueuedInputDisposition>;
async abort(): Promise<void>;
async waitForIdle(): Promise<void>;
clearQueue(): { steering: string[]; followUp: string[] };
async setModel(model: Model<any>, options?: ModelMutationOptions): Promise<void>;
async cycleModel(options?: ModelMutationOptions): Promise<ModelCycleResult | undefined>;
setThinkingLevel(level: ThinkingLevel, options?: ModelMutationOptions): void;
cycleThinkingLevel(options?: ModelMutationOptions): ThinkingLevel | undefined;
getAvailableThinkingLevels(): ThinkingLevel[];
setSteeringMode(mode: "all" | "one-at-a-time"): void;
setFollowUpMode(mode: "all" | "one-at-a-time"): void;
async compact(customInstructions?: string): Promise<CompactionResult>;
setAutoCompactionEnabled(enabled: boolean): void;
setAutoRetryEnabled(enabled: boolean): void;
abortRetry(): void;
async executeBash(
	command: string,
	onChunk?: (chunk: string) => void,
	options?: { excludeFromContext?: boolean; id?: string; operations?: BashOperations },
): Promise<BashResult>;
recordBashResult(command: string, result: BashResult, options?: { excludeFromContext?: boolean }): void;
abortBash(): void;
getSessionStats(): SessionStats;
async exportToHtml(outputPath?: string, options?: { themeName?: string }): Promise<string>;
getUserMessagesForForking(): Array<{ entryId: string; text: string }>;
getLastAssistantText(): string | undefined;
setSessionName(name: string): void;
async navigateTree(targetId: string, options?: {
	summarize?: boolean; customInstructions?: string; replaceInstructions?: boolean; label?: string;
}): Promise<{ cancelled: boolean }>;
async reload(options?: { beforeSessionStart?: () => void | Promise<void> }): Promise<void>;
async bindExtensions(bindings: ExtensionBindings): Promise<void>;
subscribe(listener: AgentSessionEventListener): () => void;
dispose(): void;
```

`PromptOptions`：

```ts
export interface PromptOptions {
	expandPromptTemplates?: boolean;   // 默认 true
	images?: ImageContent[];
	streamingBehavior?: "steer" | "followUp";
	source?: InputSource;              // "interactive" | "rpc" | "extension"，默认 "interactive"
	preflightResult?: (disposition: PromptDisposition) => void;
}
```

**`preflightResult` 调用契约（对 RPC 至关重要）**：

| 路径 | 调用 | 说明 |
|---|---|---|
| `text.startsWith("/")` 且命中扩展命令 | `"handled"`，随后 return | 扩展命令立即执行（流式时也执行） |
| 输入处理器返回 falsy（消费了输入） | `"handled"`，随后 return | |
| 压缩进行中（`_compactionAbortController` 非空） | **不调用**，抛错 | 【逐字】`Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.` |
| 正在流式且给了 `streamingBehavior` | `"queued"`（followUp / steer 各自入队后） | |
| 正在流式但**没给** `streamingBehavior` | **不调用**，抛错 | 【逐字】`Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.` |
| 无模型 | **不调用**，抛错 | `formatNoModelSelectedMessage()` |
| 无凭据 | **不调用**，抛错 | `formatNoApiKeyFoundMessage(provider)` 或 OAuth 过期文本 |
| 正常运行 | `"started"`，在 `_startRun` 的 `onStarted` 回调里（`_prepareRun` 成功之后、`agent.prompt` 之前） | `_prepareRun` 抛错则**不调用** |

`QueuedInputDisposition = "handled" | "queued"`；`PromptDisposition = QueuedInputDisposition | "started"`。

`ModelCycleResult`：`{ model: Model<any>; thinkingLevel: ThinkingLevel; isScoped: boolean }`。

`InputSource = "interactive" | "rpc" | "extension"`。

#### 5.1.2 `AgentSessionRuntime`（ca-session）

```ts
class AgentSessionRuntime {
	get session(): AgentSession;
	get services(): AgentSessionServices;
	get cwd(): string;
	get diagnostics(): readonly AgentSessionRuntimeDiagnostic[];
	get modelFallbackMessage(): string | undefined;
	setRebindSession(rebindSession?: (session: AgentSession) => Promise<void>): void;
	setBeforeSessionInvalidate(cb?: () => void): void;
	async switchSession(sessionPath: string, options?: {
		cwdOverride?: string;
		withSession?: (ctx: ReplacedSessionContext) => Promise<void>;
		projectTrustContextFactory?: (cwd: string) => ProjectTrustContext;
	}): Promise<{ cancelled: boolean }>;
	async newSession(options?: {
		parentSession?: string;
		setup?: (sessionManager: SessionManager) => Promise<void>;
		withSession?: (ctx: ReplacedSessionContext) => Promise<void>;
	}): Promise<{ cancelled: boolean }>;
	async fork(entryId: string, options?: {
		position?: "before" | "at";
		withSession?: (ctx: ReplacedSessionContext) => Promise<void>;
	}): Promise<{ cancelled: boolean; selectedText?: string }>;
	async importFromJsonl(inputPath: string, cwdOverride?: string): Promise<{ cancelled: boolean }>;
	async dispose(): Promise<void>;
}
```

**本单元依赖的生命周期语义**（必须由 Rust 侧 ca-session 保证）：

1. `setRebindSession(cb)` 后，`newSession` / `switchSession` / `fork` / `importFromJsonl` 在**成功替换**
   后调用 `cb(newSession)`（`finishSessionReplacement`），而 `dispose()` **不**调用。
2. 替换前会：`await session.abort()` -> emit `session_shutdown{reason: "new"|"resume"|"fork"|"quit",
   targetSessionFile?}` -> `beforeSessionInvalidate?.()` -> `session.dispose()`。
3. 扩展可在 `session_before_switch` / `session_before_fork` 返回 `{cancel: true}` -> 返回
   `{cancelled: true}`，此时**不**替换、**不** rebind。
4. `fork(entryId)` 默认 `position: "before"`：只接受 user 消息条目，返回该消息文本（`selectedText`）；
   `position: "at"`：任意条目，`selectedText` 为 `undefined`。
5. `dispose()`：emit `session_shutdown{reason:"quit"}` -> `beforeSessionInvalidate?.()` -> `session.dispose()`。

> 因为本单元在 `new_session`/`switch_session`/`fork`/`clone` 里**再次**调用 `rebindSession()`，
> Rust 侧若优化掉重复 rebind，会改变 `session_start` 扩展事件的次数与订阅切换时序——**不要优化**。

#### 5.1.3 `AgentSession` 的 getter（`get_state` 等使用）

```ts
get model(): Model<any> | undefined;
get thinkingLevel(): ThinkingLevel;
get isStreaming(): boolean;
get isCompacting(): boolean;
get steeringMode(): "all" | "one-at-a-time";
get followUpMode(): "all" | "one-at-a-time";
get sessionFile(): string | undefined;
get sessionId(): string;
get sessionName(): string | undefined;
get autoCompactionEnabled(): boolean;
get messages(): AgentMessage[];              // = agent.state.messages（同一引用，非独立投影）
get pendingMessageCount(): number;           // steering + followUp 队列长度
get state(): AgentState;                     // agent.state（含 messages）
get modelRuntime(): ModelRuntime;
get sessionManager(): SessionManager;
get extensionRunner(): ExtensionRunner;
get promptTemplates(): ReadonlyArray<PromptTemplate>;
get resourceLoader(): ResourceLoader;
```

`AgentState`（`packages/agent`）至少含 `{ model?: Model; thinkingLevel: ThinkingLevel; messages: AgentMessage[]; tools: ...; systemPrompt: string }`。

#### 5.1.4 `SessionManager`（ca-session）

```ts
getHeader(): SessionHeader | null;   // 找 fileEntries 里 type === "session" 的第一条
getEntries(): SessionEntry[];        // 排除 header，浅拷贝，append 顺序
getTree(): SessionTreeNode[];        // 含 label/labelTimestamp，children 按 timestamp 升序
getLeafId(): string | null;
getCwd(): string;
getSessionFile(): string | undefined;
```

`getHeader()` 返回 `null` 的唯一情形：`fileEntries` 中没有 `type === "session"` 的条目。
**注意**：`SessionManager.inMemory()` 走 `newSession()`，因此**有** header（`id` 为 uuidv7、
`timestamp` 为当前时间、`cwd` 为 `resolvePath(cwd)`、无 `parentSession`）。所以 JSON 模式在
内存会话下**仍会**输出会话头。

#### 5.1.5 wire 相关类型（ca-session / ai-core）

```ts
export interface SessionHeader {
	type: "session";
	version?: number;      // v1 无此字段
	id: string;
	timestamp: string;
	cwd: string;
	parentSession?: string;
}

export type SessionEntry =
	| SessionMessageEntry      // { type:"message", id, parentId, timestamp, message }
	| ThinkingLevelChangeEntry // { type:"thinking_level_change", ... }
	| ModelChangeEntry         // { type:"model_change", ... }
	| UsageEntry               // { type:"usage", ... }
	| CompactionEntry          // { type:"compaction", ... , summary, firstKeptEntryId, tokensBefore, ... }
	| BranchSummaryEntry       // { type:"branch_summary", ... }
	| CustomEntry              // { type:"custom", ... }
	| LabelEntry               // { type:"label", ... }
	| SessionInfoEntry         // { type:"session_info", ... }
	| CustomMessageEntry       // { type:"custom_message", ... }
	| ContextEditEntry         // { type:"context_edit", ... }

export interface SessionEntryBase {
	type: string;
	id: string;
	parentId: string | null;
	timestamp: string;
}

export interface SessionTreeNode {
	entry: SessionEntry;
	children: SessionTreeNode[];
	label?: string;
	labelTimestamp?: string;
}
```

`AgentMessage`（`packages/agent`）：`user` / `assistant` / `toolResult` / `bashExecution` / `custom` /
`custom_message` 等（完整定义归 ai-core / ca-session）。RPC 只透传，不解释。

#### 5.1.6 `AgentSessionEvent`（完整联合，wire 的上游）

```ts
export type AgentSessionEvent =
	| WithParentToolCallId<Exclude<AgentEvent, { type: "agent_end" }>>
	| { type: "agent_end"; messages: AgentMessage[]; willRetry: boolean }
	| { type: "agent_settled"; aborted: boolean }
	| { type: "queue_update"; steering: readonly string[]; followUp: readonly string[] }
	| { type: "compaction_start"; reason: "manual" | "threshold" | "overflow" }
	| { type: "entry_appended"; entry: SessionEntry }
	| { type: "session_info_changed"; name: string | undefined }
	| { type: "thinking_level_changed"; level: ThinkingLevel }
	| {
			type: "compaction_end";
			reason: "manual" | "threshold" | "overflow";
			result: CompactionResult | undefined;
			aborted: boolean;
			willRetry: boolean;
			errorMessage?: string;
	  }
	| { type: "auto_retry_start"; attempt: number; maxAttempts: number; delayMs: number; errorMessage: string }
	| { type: "auto_retry_end"; success: boolean; attempt: number; finalError?: string }
	| { type: "summarization_retry_scheduled"; attempt: number; maxAttempts: number; delayMs: number; errorMessage: string }
	| { type: "summarization_retry_attempt_start"; source: "branchSummary" }
	| { type: "summarization_retry_attempt_start"; source: "compaction"; reason: "manual" | "threshold" | "overflow" }
	| { type: "summarization_retry_finished" }
	| { type: "bash_execution_update"; id?: string; delta: string };
```

`AgentEvent`（`packages/agent`）：

```ts
export type AgentEvent =
	| { type: "agent_start" }
	| { type: "agent_end"; messages: AgentMessage[] }
	| { type: "turn_start" }
	| { type: "turn_end"; message: AgentMessage; toolResults: ToolResultMessage[] }
	| { type: "message_start"; message: AgentMessage }
	| { type: "message_update"; message: AgentMessage; assistantMessageEvent: AssistantMessageEvent }
	| { type: "message_end"; message: AgentMessage }
	| { type: "tool_execution_start"; toolCallId: string; toolName: string; args: any }
	| { type: "tool_execution_update"; toolCallId: string; toolName: string; args: any; partialResult: any }
	| { type: "tool_execution_end"; toolCallId: string; toolName: string; result: any; isError: boolean; durationMs?: number };
```

`WithParentToolCallId<E>`：对三个 `tool_execution_*` 事件附加 `parentToolCallId?: string`。

`AssistantMessageEvent`（ai-core）：

```ts
| { type: "start"; partial: AssistantMessage }
| { type: "text_start"; contentIndex: number; partial: AssistantMessage }
| { type: "text_delta"; contentIndex: number; delta: string; partial: AssistantMessage }
| { type: "text_end"; contentIndex: number; content: string; partial: AssistantMessage }
| { type: "thinking_start"; contentIndex: number; partial: AssistantMessage }
| { type: "thinking_delta"; contentIndex: number; delta: string; partial: AssistantMessage }
| { type: "thinking_end"; contentIndex: number; content: string; partial: AssistantMessage }
| { type: "toolcall_start"; contentIndex: number; partial: AssistantMessage }
| { type: "toolcall_delta"; contentIndex: number; delta: string; partial: AssistantMessage }
| { type: "toolcall_end"; contentIndex: number; toolCall: ToolCall; partial: AssistantMessage }
| { type: "done"; reason: StopReason; message: AssistantMessage }
| { type: "error"; reason: StopReason; error: AssistantMessage }
```

`AssistantMessage`：`{ role:"assistant"; content: (TextContent|ThinkingContent|ToolCall)[]; api; provider; model;
usage: Usage; stopReason: "stop"|"length"|"toolUse"|"error"|"aborted"|"pending"; errorMessage?: string;
timestamp: number; ... }`。

`Usage`：

```ts
export interface Usage {
	input: number; output: number; cacheRead: number; cacheWrite: number;
	cacheWrite1h?: number;
	reasoning?: number;
	totalTokens: number;
	cost: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number; totalFromProvider?: boolean };
}
```

#### 5.1.6.1 `Agent.subscribe` 的语义（背压订阅依赖）

```ts
private readonly listeners = new Set<(event: AgentEvent, signal: AbortSignal) => Promise<void> | void>();

subscribe(listener: (event: AgentEvent, signal: AbortSignal) => Promise<void> | void): () => void {
	this.listeners.add(listener);
	return () => this.listeners.delete(listener);
}
```

- **Set 语义**：同一函数引用重复 subscribe 只保留一份；退订按引用删除。
- **emit 串行 await**：agent loop 的 `emit` 对每个监听器 `await listener(event, signal)`，
  因此一个 async 监听器（如本单元的 `waitForRawStdoutBackpressure()`）会**阻塞**该事件的后续监听器
  与后续事件，直到 resolve。这正是“backpressure 订阅”生效的机制。
- **顺序**：Set 的插入顺序 = subscribe 顺序。
- **退订函数幂等**。

Rust 侧必须在 `kiana-agent-core` 等价物里复刻“await 每个监听器”的语义，否则背压订阅形同虚设，
RPC/JSON 在管道满时会丢字节。

#### 5.1.7 认证/模型错误文案（`core/auth-guidance.ts`，由 ca-session 抛出）

```ts
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
	const providerDisplay = provider === "unknown" ? "the selected model" : provider;
	return `No API key found for ${providerDisplay}.\n\n${getProviderLoginHelp()}`;
}
```

这三条会作为 RPC `prompt` 的失败响应 `error` 完整出现（含换行）。Rust 侧必须逐字复刻，
`getDocsPath()` 的路径解析同 ca-cli。

OAuth 过期文本（`AgentSession.prompt` 内联）：

```
Authentication failed for "<provider>". Credentials may have expired or network is unavailable. Run '/login <provider>' to re-authenticate.
```

（注意：整串是一个 `Error` 的 message，拼接方式为三段字符串相加，无双空格问题。）

#### 5.1.8 `killTrackedDetachedChildren()`（`src/utils/shell.ts`）

```ts
export function killTrackedDetachedChildren(): void;  // 同步；遍历 trackedDetachedChildPids -> killProcessTree -> clear
```

Rust 侧需要等价物：全局 `Mutex<HashSet<u32>>` + `kill_process_tree`（unix 用 `kill(-pgid, SIGKILL)`）。

### 5.2 扩展系统（ca-extensions）

#### 5.2.1 `ExtensionBindings`

```ts
export interface ExtensionBindings {
	uiContext?: ExtensionUIContext;
	mode?: ExtensionMode;                                   // "tui" | "rpc" | "json" | "print"
	commandContextActions?: ExtensionCommandContextActions;
	abortHandler?: () => void;
	shutdownHandler?: ShutdownHandler;                      // () => void
	onError?: ExtensionErrorListener;                       // (error: ExtensionError) => void
}
```

`ExtensionCommandContextActions`：

```ts
export interface ExtensionCommandContextActions {
	waitForIdle: () => Promise<void>;
	newSession: (options?: { parentSession?: string; setup?: ...; withSession?: ... }) => Promise<{ cancelled: boolean }>;
	fork: (entryId: string, options?: { position?: "before" | "at"; withSession?: ... }) => Promise<{ cancelled: boolean }>;
	navigateTree: (targetId: string, options?: {
		summarize?: boolean; customInstructions?: string; replaceInstructions?: boolean; label?: string;
	}) => Promise<{ cancelled: boolean }>;
	switchSession: (sessionPath: string, options?: { withSession?: ... }) => Promise<{ cancelled: boolean }>;
	reload: () => Promise<void>;
}
```

`ExtensionError`：`{ extensionPath: string; event: string; error: string; stack?: string }`。

`bindExtensions` 语义（ca-session 实现，ca-extensions 消费）：

1. 逐个可选字段：`if (bindings.X !== undefined) this._X = bindings.X;` —— **`undefined` 不覆盖**已有值。
   因此 print/json 模式多次 rebind 不会清除上一次设置的值（但每次都是同一份值）。
2. `_applyExtensionBindings`：`runner.setUIContext(uiContext, mode)`（`uiContext === undefined` 时用
   `noOpUIContext`）、`runner.bindCommandContext(actions)`、重新订阅 `onError`。
3. `await runner.emit(session_start)`，再 `reportUnhandledMcpServers()`，再
   `extendResourcesFromExtensions(reason)`（`reload` 时为 `"reload"`，否则 `"startup"`）。

#### 5.2.2 `ExtensionRunner` 被本单元使用的成员

```ts
setUIContext(uiContext?: ExtensionUIContext, mode?: ExtensionMode): void;
hasUI(): boolean;
getRegisteredCommands(): ResolvedCommand[];
async emitUserBash(event: UserBashEvent): Promise<UserBashEventResult | undefined>;
onError(listener: ExtensionErrorListener): () => void;
emitError(error: ExtensionError): void;
shutdown(): void;                  // 调用 bindExtensions 传入的 shutdownHandler
async emit(event): Promise<...>;
```

`ResolvedCommand extends RegisteredCommand { invocationName: string }`，其中 `RegisteredCommand`：
`{ name: string; sourceInfo: SourceInfo; description?: string; getArgumentCompletions?; handler }`。

`invocationName` 生成规则（重名消歧）：

1. 统计每个 `name` 出现次数 `counts`。
2. 按扩展加载顺序遍历命令，出现序 `occurrence` 从 1 开始。
3. 若 `counts[name] > 1`：`invocationName = ${name}:${occurrence}`；否则 `invocationName = name`。
4. 若该 `invocationName` 已被占用：从 `suffix = occurrence` 开始 `suffix++` 直到
   `${name}:${suffix}` 未被占用。
5. 记录占用。

`UserBashEvent`：`{ type: "user_bash"; command: string; excludeFromContext: boolean; cwd: string }`。

`UserBashEventResult`：

```ts
| { operations: BashOperations; result?: never }
| { operations?: never; result: BashResult }
```

`emitUserBash` 语义：

- 按扩展顺序、每扩展的 handler 顺序遍历。
- handler 返回 `undefined` -> 继续下一个。
- 返回非 `undefined` 且不满足 `isUserBashEventResult` -> 抛
  【逐字】`Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object`。
- handler 抛错：先 `emitError({extensionPath: ext.path, event: "user_bash", error: message, stack})`，
  再 rethrow。
- 全部返回 `undefined` -> 返回 `undefined`。

`setUIContext` 的包装（`wrapUIPromptContext`）：对 `select`/`confirm`/`input`/`editor`/`custom` 包一层
`withUIPrompt(kind, title, run)`，在**最外层** dialog 开始/结束时（用 `uiPromptDepth` 计数）
`queueMicrotask` 发出 `ui_prompt_start` / `ui_prompt_end` 事件。RPC 模式不消费这些事件。

`noOpUIContext`（print/json 模式使用）：所有方法 no-op；`select -> undefined`、`confirm -> false`、
`input -> undefined`、`editor -> undefined`、`custom -> undefined`、`getEditorText -> ""`、
`getEditorComponent -> undefined`、`getAllThemes -> []`、`getTheme -> undefined`、
`setTheme -> { success: false, error: "UI not available" }`、`getToolsExpanded -> false`、
`theme` getter 返回全局 `theme`。

#### 5.2.3 `ExtensionUIContext`（完整方法集，RPC 实现见 §2.6.5）

见 §2.5.5 与 §2.6.5；关键类型：

```ts
export interface ExtensionUIDialogOptions { signal?: AbortSignal; timeout?: number; }
export type WidgetPlacement = "aboveEditor" | "belowEditor";
export interface ExtensionWidgetOptions { placement?: WidgetPlacement; }
export interface WorkingIndicatorOptions { frames?: string[]; intervalMs?: number; }
export type TerminalInputHandler = (data: string) => { consume?: boolean; data?: string } | undefined;
```

`ExtensionMode = "tui" | "rpc" | "json" | "print"`。

### 5.3 资源与模型（ca-resources / ca-config-models）

```ts
// PromptTemplate（core/prompt-templates.ts）
export interface PromptTemplate {
	name: string;
	description: string;
	argumentHint?: string;
	content: string;
	sourceInfo: SourceInfo;
	filePath: string;
}

// Skill（core/skills.ts）
export interface Skill {
	name: string;
	description: string;
	filePath: string;
	baseDir: string;
	sourceInfo: SourceInfo;
	disableModelInvocation: boolean;
}
export interface LoadSkillsResult { skills: Skill[]; diagnostics: ResourceDiagnostic[] }

// ResourceLoader（core/resource-loader.ts）
getSkills(): LoadSkillsResult;
```

```ts
// ModelRuntime（core/model-runtime.ts）
getAvailableSnapshot(): Model<any>[];
hasConfiguredAuth(provider: string): boolean;
checkAuth(provider: string): Promise<unknown>;
isUsingOAuth(provider: string): boolean;
async refresh(options?: { signal?: AbortSignal }): Promise<unknown>;
getPhysicalModel(provider: string, id: string): Model<any> | undefined;
```

`getAvailableSnapshot()` 返回当前可用模型的**快照数组**（顺序即模型目录顺序）；
`set_model` / `get_available_models` 直接使用该数组。

### 5.4 本单元暴露给其它单元

| 导出 | 使用者 |
|---|---|
| `runPrintMode` / `PrintModeOptions` | `main.ts`（ca-cli） |
| `runRpcMode` | `main.ts`（ca-cli） |
| `toJsonEvent` / `JsonAgentSessionEvent` | `RpcClient`、SDK 消费者、文档 |
| `serializeJsonLine` / `attachJsonlLineReader` | `RpcClient`、测试 |
| `RpcClient` / `RpcClientOptions` / `ModelInfo` / `RpcEventListener` | 示例、SDK 用户 |
| `RpcCommand` / `RpcResponse` / `RpcSessionState` / `RpcExtensionUIRequest` / `RpcExtensionUIResponse` / `RpcSlashCommand` / `RpcCommandType` | 客户端实现、文档 |

`src/index.ts` 的再导出（`src/index.ts:419`–`435`）：**全部 15 个名字**都从包根导出：

```ts
export {
	InteractiveMode,
	type InteractiveModeOptions,
	type JsonAgentSessionEvent,
	type ModelInfo,
	type PrintModeOptions,
	RpcClient,
	type RpcClientOptions,
	type RpcCommand,
	type RpcEventListener,
	type RpcExtensionUIRequest,
	type RpcExtensionUIResponse,
	type RpcResponse,
	type RpcSessionState,
	runPrintMode,
	runRpcMode,
} from "./modes/index.ts";
```

即 `ModelInfo`、`RpcEventListener`、`RpcCommand`、`RpcExtensionUIRequest`、`RpcExtensionUIResponse`、
`RpcResponse`、`RpcSessionState`、`InteractiveMode`、`InteractiveModeOptions` **都**从包根导出。
（旧版本此处只列 6 个名字并声称其余“不从包根导出”，与源码与行号均不符，已更正。
注意行号：再导出块起始于 `src/index.ts:419`，`} from "./modes/index.ts";` 在 435 行。）
`RpcSlashCommand` / `RpcCommandType` / `toJsonEvent` / `serializeJsonLine` / `attachJsonlLineReader`
确实**不**从包根导出（它们只从各自模块导出；`modes/index.ts` 也不再导出前两者）。

---
## 6. npm 依赖与 Rust crate 映射

### 6.1 本单元直接使用的 npm 包 / 内建模块

| 模块 | 用在哪 | 说明 |
|---|---|---|
| `node:crypto`（`randomUUID`） | `rpc-mode.ts` | v4 UUID，36 字符小写带连字符（dialog / notify / setStatus / setWidget / setTitle / set_editor_text） |
| `node:child_process`（`spawn` / `ChildProcess`） | `rpc-client.ts` | `spawn("node", [...], {cwd, env, stdio:["pipe","pipe","pipe"]})` |
| `node:stream`（`Readable`） | `jsonl.ts` | `on("data")` / `on("end")` / `off` |
| `node:string_decoder`（`StringDecoder`） | `jsonl.ts` | UTF-8 增量解码（跨 chunk 保留不完整序列，非法字节 -> U+FFFD） |
| 全局 `JSON.stringify` / `JSON.parse` | 本单元全部 | 见 §4.2 / §4.3 |
| 全局 `setTimeout` / `clearTimeout` | dialog timeout、RpcClient 超时、`start()` 100ms、`stop()` 1000ms | |
| 全局 `Promise` / `queueMicrotask` | 事件/订阅 | |
| `process`（stdin/stdout/stderr/平台/信号/exit） | 全部 | 见 §4.7 |
| `@earendil-works/kiana-agent-core`（类型） | `rpc-types.ts` / `rpc-client.ts` | `AgentMessage`、`ThinkingLevel`、`AgentEvent` |
| `@earendil-works/kiana-ai`（类型） | `json-event.ts` / `print-mode.ts` / `rpc-types.ts` / `rpc-client.ts` | `Usage`、`ImageContent`、`Model`、`AssistantMessage` |
| `core/output-guard.ts` | `print-mode.ts` / `rpc-mode.ts` | 见 §2.8 |
| `utils/shell.ts`（`killTrackedDetachedChildren`） | `print-mode.ts` / `rpc-mode.ts` | |
| `modes/interactive/theme/theme.ts`（`theme`） | `rpc-mode.ts` | 全局主题 Proxy（未初始化时抛错） |

本单元**不**使用：`chalk`、`yaml`、`undici`、`proper-lockfile`、`cross-spawn`、`diff`、`highlight.js` 等。

### 6.2 建议的 crate 映射

| npm / Node 能力 | Rust crate | 说明 / 差异 |
|---|---|---|
| `JSON.stringify` | `serde_json`（`preserve_order`）+ 自定义序列化 | **必须**：`preserve_order`（`indexmap`）保键序；数字格式需自定义（§4.2.4）；字符串转义需核对（`serde_json` 不转义 U+2028/U+2029，与 JS 一致；不转义 U+007F+，一致；孤立代理项 Rust 无法表示） |
| `JSON.parse` | `serde_json` + 自定义错误映射，或自写解析器 | **必须**桥接：错误消息文本与 position/line/column 计算（§4.3）。建议自写递归下降解析器（只支持标准 JSON），在错误点产出 `(kind, index)`；`serde_json` 仅用于成功路径的类型绑定 |
| `StringDecoder("utf8")` | `encoding_rs`（`Decoder`）或手写 | 需复刻：每个非法字节一个 U+FFFD；`end()` 对不完整尾序列再补一个 U+FFFD。`encoding_rs` 的 `decode_to_string` 在 `last=false` 时保留不完整序列；`last=true` 时的替换计数与 Node 略有差异，需测试锁定 |
| `node:crypto randomUUID` | `uuid`（`features=["v4","fast-rng"]`） | 格式一致（36 字符小写带连字符） |
| `node:child_process spawn` | `tokio::process::Command` | 差异：Node 的 `env` 合并语义（`undefined` 值删除键）、`stdio` 三管道、`exit` 事件携带 `(code, signal)`（signal 为字符串名，如 `"SIGTERM"`，无信号时为 `null`）。Rust 的 `ExitStatus` 用 `ExitStatusExt::signal()` 区分；需要把 signal 映射成 Node 的字符串名 |
| `node:stream` backpressure | `tokio::io::AsyncWrite` + `mpsc` 串行任务 | 见 §2.8.5 |
| `setTimeout` | `tokio::time::sleep` / `timeout` | 注意 `timeout: 0` 视为“无超时”（falsy 判定） |
| `process.exit(code)` | `std::process::exit(code)` | 立即终止、不跑析构；Rust 侧需要在 exit 前显式 flush（见 §2.6.9） |
| 信号 | `tokio::signal::unix::{signal, SignalKind}` | SIGTERM/SIGHUP；win32 只有 SIGTERM（`tokio::signal::ctrl_c` 不算） |
| `AbortSignal`（dialog 的 `opts.signal`） | `tokio_util::sync::CancellationToken` 或 `tokio::select!` + `Notify` | 需复刻“已 aborted 立即返回默认值、不发出请求” |
| `fs` / 路径 | `std::path` / `camino` | 本单元只透传路径字符串（`cwd`、`sessionPath`、`outputPath`） |
| `Date.toISOString()` | `chrono`（`DateTime<Utc>::to_rfc3339_opts(SecondsFormat::Millis, true)`） | 格式 `YYYY-MM-DDTHH:mm:ss.sssZ`；年份 < 1000 时 JS 补零到 4 位（如 `0001-01-01T00:00:00.000Z`），`chrono` 需检查 |
| `String.prototype.trim` | 自写 | JS 空白集含 U+FEFF、不含 U+0085（§4.2.6） |
| `String(...)` 数字格式化 | 自写 | 见 §4.2.4 |
| `theme` 全局 Proxy | 见 ca-interactive-mode | RPC 的 `uiContext.theme` getter 需返回当前全局主题；未初始化时抛出同一文本 |

### 6.3 需要桥接的 JS/Rust 行为差异（清单）

| 差异 | 影响面 | 桥接方式 |
|---|---|---|
| **UTF-16 索引/长度** | `text.length === 0` 判定、`buffer.indexOf("\n")`、`slice`、`line.endsWith("\r")`、`contentIndex` | 对 JSONL 读取，用**字节**或 `Vec<u16>`；“空串”判定在合法 UTF-8 下与 `is_empty()` 等价；`contentIndex` 是数组下标，与字符串无关。真正需要 UTF-16 的只有错误消息里的 position/column（§4.3） |
| **JSON 键顺序** | 所有 wire 记录 | `serde_json` 开 `preserve_order`；或全部用显式有序结构体（推荐：结构体字段顺序即输出顺序，`#[serde(skip_serializing_if = "Option::is_none")]` 处理可选） |
| **JSON 数字格式** | `usage`、`cost`、`timestamp`（number 类型）、`durationMs`、`tokensBefore` 等 | 自写 `format_js_number(f64)`；整数用 `i64/u64` 直出；浮点走 Ryu + 后处理（补 `+`、去 `.0`、指数阈值切换） |
| **JSON 字符串转义** | 所有字符串 | `serde_json` 默认接近；需确认不对 U+2028/U+2029 转义（默认不转义），不把 `/` 转义（默认不转义） |
| **孤立代理项** | 理论上可能出现在模型输出文本中 | Rust `String` 不能容纳；建议内部统一为合法 UTF-8（替换为 U+FFFD），并在文档中记为已知差异；如需字节级一致，用 `Vec<u16>` + 自定义序列化（成本高） |
| **正则方言** | 本单元**无正则**（唯一相关的是 `session-manager` 的 header 扫描） | 不适用 |
| **`JSON.parse` 错误文本** | RPC `Failed to parse command: ...` | 自写解析器 + 模板（§4.3） |
| **`trim` 字符集** | `set_session_name` | 自写（§4.2.6） |
| **stdout 接管** | print/json/rpc 全部输出 | 见 §2.8.5：Rust 需显式实现“协议写 vs 诊断写”两条通道 |
| **进程信号退出码** | 129 / 143 | 显式 `exit(129/143)`（不要用“重新抛信号”的默认语义） |
| **`spawn` 的 `env` 合并** | RpcClient | `options.env` 的值覆盖；`Option::None` 表示删除该键 |
| **子进程 signal 名称** | `Agent process exited (code=... signal=...)` | 把 `SIGTERM`/`SIGKILL` 等映射为 Node 的字符串名；无信号为 `null` |
| **`ChildProcess.exitCode` 初始为 `null`** | `start()` 的 100ms 检查、`send()` 的前置检查 | Rust 用 `Option<i32>` + `try_wait()` 近似 |
| **microtask 顺序** | 事件与响应交错 | 用单 task 串行 + `tokio::task::yield_now()` 近似；黄金测试只对“同一命令内”的顺序做断言 |

---
## 7. 测试清单

> 全部位于 `packages/coding-agent/test/`。Rust 侧应逐条转写为单元/集成测试。
> “Mock 依赖”列说明该测试对 `output-guard` / `jsonl` 的 `vi.mock` 方式——Rust 侧对应
> 需要可注入的 `RawStdout` / `JsonlReader` trait。

### 7.1 直接覆盖本单元的测试

| 测试文件 | 用例数 | 断言的行为 |
|---|---|---|
| `test/print-mode.test.ts` | 4 | ① text 模式 emit `session_shutdown{reason:"quit"}` 恰好一次；`prompt("Say done", {images})` 参数；退出码 0；② json 模式同样 emit 一次；`prompt("hello")`（无 images 选项）；③ assistant `stopReason:"error"` + `errorMessage:"provider failure"` -> 返回 1、`console.error("provider failure")`、仍 emit shutdown；④ `stopReason:"length"` -> 返回 1、`console.error` 被调用（#10799）。Mock：`sessionManager.getHeader()` 返回 `undefined`（因此 json 模式不写 header）、`bindExtensions`、`subscribe`、`agent.subscribe`、`prompt`、`reload`、`extensionRunner.emit`、`runtimeHost.setRebindSession` |
| `test/rpc-jsonl.test.ts` | 4（用 `test()` 定义，非 `it()`） | ① `serializeJsonLine({text:"a b c"})` 保留 U+2028/U+2029、以 `\n` 结尾、可被 `JSON.parse(line.trim())` 回读；② `Readable.from([序列化结果])` 读取得到 1 行且内容一致（只按 LF 切分）；③ `Buffer.from('{"a":1}\r\n{"b":2}\r\n')` -> `['{"a":1}','{"b":2}']`（CRLF 剥离）；④ 无尾随 LF 的 `{"a":1}` -> `['{"a":1}']` |
| `test/rpc.test.ts` | 18（`describe.skipIf` 无 `ANTHROPIC_API_KEY`/`ANTHROPIC_OAUTH_TOKEN` 时整组跳过） | 真实 CLI（`dist/cli.js`）+ 真实 API：`getState`（model provider/id、`isStreaming=false`、`messageCount=0`）；`promptAndWait` 后 `message_end` >= 2 且会话文件 `sessions/<cwd>/<ts>_<id>.jsonl` 首条 `type:"session"`、含 user+assistant；`compact` 后会话文件出现 1 条 `type:"compaction"`；`bash("echo hello")` -> `output.trim()==="hello"`、`exitCode===0`、`cancelled===false`；bash 结果落为 `bashExecution` 消息；bash 输出进入 LLM 上下文；`setThinkingLevel("high")` 后 `getState().thinkingLevel==="high"`；`cycleThinkingLevel` 改变 level；`getAvailableThinkingLevels` 非空、含当前 level、cycle 结果属于该集合；`getAvailableModels` 每项含 provider/id/contextWindow>0/reasoning:boolean；`getSessionStats` 含 sessionFile/sessionId/userMessages>=1/assistantMessages>=1；`newSession` 后 `messageCount===0`；`exportHtml` 返回 `.html` 路径且文件存在；`getLastAssistantText` 初始 `undefined`、之后含 `test123`；`getEntries` 的 `leafId` 等于最后一条 id、`since` 严格之后、未知 since reject `Entry not found`；`getTree` 单根链与 entries 一致、`leafId` 一致；compact 后 `get_entries` 仍保留压缩前条目且出现 `type:"compaction"`；`setSessionName` 后 `sessionName` 生效且会话文件出现 1 条 `session_info`（名字一致） |
| `test/rpc-client-clear-queue.test.ts` | 1 | 直接构造 `RpcClient`，替换私有 `send`/`getData`，断言 `clearQueue()` 调 `send({type:"clear_queue"})` 并返回 `{steering, followUp}` |
| `test/rpc-client-clone.test.ts` | 1 | 同上，`clone()` 调 `send({type:"clone"})` 并返回 `{cancelled:false}` |
| `test/rpc-client-process-exit.test.ts` | 1（`test()`） | 用临时 `child.mjs`（`stdin.once("data", () => process.exit(43))`）作为 `cliPath`；`start()` 成功；`getCommands()` reject，消息匹配 `/Agent process exited \(code=43 signal=null\)/` |
| `test/rpc-prompt-response-semantics.test.ts` | **9**（5 个普通 `it` + 2 个 `it.each` 各展开 2 次；旧版误写 8） | ① 预检失败：无 API key -> 单条 `{id,type:"response",command:"prompt",success:false,error:含 "No API key found for fake-provider.\n\nUse /login ..."}`；② 预检成功 -> 单条 `success:true, data:{disposition:"started"}`（#9098）；③ 扩展命令 `/handled` 与输入拦截 `"handled input"` -> `disposition:"handled"` 且无 `agent_start`；④ 流式中带 `streamingBehavior:"followUp"` -> `disposition:"queued"`；⑤ `steer`/`follow_up` 在扩展消费 A 但另queue B 时报告 `handled`，且 `queue_update` 反映 B（#9803）；⑥ `steer`/`follow_up` 经 input transform 后报 `queued` 且 `queue_update` 反映变换后的文本；⑦ `clear_queue` 返回 `{steering:["Change direction"], followUp:["Summarize when finished"]}` 且此后只有 1 个 `agent_start`。Mock：`output-guard`（写 `rpcIo.outputLines`）、`theme`、`jsonl`（捕获 `lineHandler`）。导入的辅助：`test/model-runtime-test-utils.ts`（`createInMemoryModelRegistry` / `getModelRuntime`）与 `test/utilities.ts`（`createTestExtensionsResult` / `createTestResourceLoader`） |
| `test/suite/regressions/5868-rpc-unknown-command-id.test.ts` | 1（`test()`） | `{id:"test", type:"foobar"}` -> `{id:"test", type:"response", command:"foobar", success:false, error:"Unknown command: foobar"}`（保留 id）。同时快照/恢复 `process.stdin` 的 `end` 监听与 SIGTERM/SIGHUP 监听 |
| `test/suite/regressions/7150-rpc-prompt-during-compaction.test.ts` | 1 | 手动压缩进行中（`session_before_compact` 挂起）时 `session.prompt(..., {source:"rpc", preflightResult})`：`preflightResult` 不被调用、抛错消息含 `compaction is in progress`、提示未进入用户消息与持久化、无 `agent_start`/`agent_settled` |
| `test/suite/regressions/7290-json-stream-linear.test.ts` | 1 | `toJsonEvent` 后的 `message_update` 不含 `message`、`assistantMessageEvent` 不含 `partial`；总字节数随文本长度线性（4000/2000 字符的字节比 < 2.2） |
| `test/suite/regressions/7911-json-stream-usage.test.ts` | 1 | `toJsonEvent(update).usage` 深等于 `update.message.usage`；无 `message`；内层无 `partial`（#7911 修复 #7290 丢失 usage） |
| `test/suite/regressions/7925-toolcall-start-metadata.test.ts` | 1 | `toJsonEvent` 对 `toolcall_start` 精确等于 `{type:"message_update", usage, assistantMessageEvent:{type:"toolcall_start", contentIndex:0, id:"call_7925", toolName:"write"}}`（键集合与值均断言） |
| `test/suite/regressions/9068-user-bash-fail-closed.test.ts` | 3（RPC 部分，`test.each(rpcCases)` 展开 3 次）+ 2（interactive 部分，`test.each` 展开，非本单元） | RPC `bash` 命令：① user_bash handler 抛错 -> 响应 `success:false` 且 `error` 含 `Routing failed`，并出现 `{type:"extension_error", event:"user_bash", error:含 "Routing failed"}`，`executeBash` 调用 0 次；② handler 返回 `{}` -> 失败，error 含 `Invalid user_bash handler result`，executeBash 0 次；③ handler 返回 `undefined` -> 成功，`data` 深等于本地结果，executeBash 1 次 |
| `test/stdout-cleanliness.test.ts` | 2 | 真实 CLI 子进程（`--import source-resolver`）：① `--help`（stdout 重定向）-> exit 0、stdout 含 `Usage:`、stderr 不含 `Usage:` 且不含 npm chatter（`changed 1 package in 471ms`、`found 0 vulnerabilities`）；② `--mode json --help --approve` -> exit 0、**stdout 为空**、stderr 含上述 npm chatter 与 `Usage:` |

### 7.2 示例与手工脚本（非断言，但定义行为契约）

| 文件 | 作用 |
|---|---|
| `test/rpc-example.ts` | 交互式 `RpcClient` 用法：`cliPath=dist/cli.js`、`provider:"anthropic"`、`model:"claude-sonnet-4-20250514"`、`args:["--no-session"]`；`onEvent` 里 `message_update` 的 `text_delta`/`thinking_delta` 直接 `process.stdout.write(delta)`；`tool_execution_start` 打印 `\n[Tool: ${toolName}]`；`tool_execution_end` 打印 `[Result: ${JSON.stringify(result).slice(0,200)}...]\n`；`readline` 输入 `exit` -> `stop()`+`exit 0`；`SIGINT` 时正在等待 -> `abort()`，否则 `stop()`+`exit 0` |
| `examples/rpc-client.ts` | 单次提示：`cliPath=../dist/cli.js`、`args:["--no-session"]`；`text_delta` -> stdout；`tool_execution_start` -> stderr `\n[tool: ${toolName}]\n`；`promptAndWait` 后写 `"\n"`；`finally` 退订 + `stop()` |
| `examples/rpc-extension-ui.ts`（661 行） | 完整 RPC UI 客户端：自定义 JSONL reader、9 种 request 的终端渲染、3 种 response 发送、Esc -> abort、Ctrl+D -> exit；子进程立即退出时打印 `Agent exited immediately. Stderr:\n${stderr}`；退出时打印 `Agent exited with code ${code}` |
| `examples/extensions/rpc-demo.ts` | 覆盖全部 RPC 可用 UI 方法的演示扩展（select/confirm/input/editor/notify/setStatus/setWidget/setTitle/setEditorText） |
| `test/fixtures/before-compaction.jsonl` | 压缩相关会话夹具（被其它单元测试使用，本单元不直接读） |
| `test/suite/harness.ts` | 测试夹具：`createHarness()` 返回 `session/sessionManager/settingsManager/authStorage/faux/models/getModel/setResponses/appendResponses/getPendingResponseCount/events/eventsOfType/tempDir/cleanup`；`createTestUiContext()` 提供 no-op `ExtensionUIContext` |
| `test/utilities.ts` | `createTestExtensionsResult()` / `createTestResourceLoader()`（`rpc-prompt-response-semantics.test.ts` 直接使用）；Rust 侧的对拍夹具需提供等价物 |
| `test/model-runtime-test-utils.ts` | `createInMemoryModelRegistry()` / `getModelRuntime()`（同上） |
| `test/test-network-env.ts` | `allowNetwork()`：`stdout-cleanliness.test.ts` 的 `beforeEach` 用它（该测试跑真实子进程且要避免测试网络封锁） |

### 7.3 相邻但会影响本单元行为的测试（建议回归）

| 测试文件 | 相关点 |
|---|---|
| `test/args.test.ts` | `--mode` 校验与错误文案、`--print`、`@file` 解析（RPC 拒绝逻辑的输入） |
| `test/agent-session-runtime*.test.ts` | `setRebindSession` 调用时机、`newSession`/`switchSession`/`fork` 的 `cancelled` 返回值 |
| `test/agent-session-queue.test.ts` | `clearQueue` 返回值、`queue_update` 事件 |
| `test/suite/agent-session-boundaries.test.ts` | `agent_settled` 与 `aborted` 语义 |
| `test/agent-session-stats.test.ts` | `SessionStats` 字段 |
| `test/session-manager/*` | `getHeader` / `getEntries` / `getTree` / `getLeafId` 语义 |

### 7.4 建议新增的 Rust 黄金测试

1. **JSONL 框架**：与 `test/rpc-jsonl.test.ts` 同构 + 跨 chunk 多字节 UTF-8 + 非法字节 + `end` 尾行 +
   `\r\r\n` + U+2028/U+2029。
2. **`toJsonEvent`**：以构造的 `AgentSessionEvent` 逐 variant 对比 `serde_json::to_string`
   （含键序、`toolcall_start` 追加字段、错误文本）。
3. **RPC 命令面**：用假 `AgentSession`（mock trait）逐命令断言请求/响应 JSON 字符串**逐字节**相等
   （含 `data` 缺省、`null`、`cancelled`、错误文本）。参考 `rpc-prompt-response-semantics.test.ts` 的 mock 方式。
4. **`JSON.parse` 错误文本**：§4.3 的表格逐行对拍（锁定运行时版本）。
5. **print 模式**：§2.3.3 的 8 个场景逐字节对比 stdout/stderr/退出码。
6. **信号**：真实子进程发 SIGTERM/SIGHUP，断言退出码 143/129 与 stdout 内容（SIGTERM 不 flush 的差异
   通过“写到一半”场景验证）。
7. **`RpcClient`**：与 `rpc-client-process-exit.test.ts` 同构（子进程 `exit(43)`），断言错误文本。
8. **端到端**：`kiana --mode json` 与 `kiana --mode rpc` 的真实二进制对拍（事件流逐字节，去掉时间戳/ID 等易变字段后比较）。

---
## 8. Rust 模块布局与最难复刻部分

### 8.1 crate 与模块

建议一个 crate `ca-rpc-print`（与 `ca-session` 同族命名；若 workspace 统一用 `kiana-*` 前缀则改名
`kiana-rpc-print`，模块划分不变）。依赖方向：`ca-rpc-print -> ca-session, ca-extensions, ca-resources,
ca-config-models, kiana-ai, kiana-agent-core`；**不**依赖 `kiana-interactive` / `kiana-tui`（RPC 只使用全局 `theme`
的只读快照，可通过 ca-session 或注入的 `ThemeHandle` 获取）。`ca-cli` 依赖本 crate。

```
crates/ca-rpc-print/
  Cargo.toml
  src/
    lib.rs                 // 顶层 pub use：run_print_mode / PrintModeOptions / run_rpc_mode /
                           // to_json_event / JsonAgentSessionEvent / serialize_json_line /
                           // attach_jsonl_line_reader / RpcClient / RpcClientOptions / ModelInfo /
                           // RpcEventListener / RpcCommand / RpcResponse / RpcSessionState /
                           // RpcExtensionUIRequest / RpcExtensionUIResponse / RpcSlashCommand /
                           // RpcCommandType
    json_event.rs          // to_json_event + JsonAgentSessionEvent + WireAssistantMessageEvent
    print_mode.rs          // run_print_mode + PrintModeOptions + 信号 + 文本/JSON 分支
    jsonl.rs               // serialize_json_line / JsonlLineReader（StringDecoder 等价物）
    js_json.rs             // JS 兼容层：stringify（数字格式/转义/键序）、parse（V8 错误消息）、
                           // trim、String(value)、to_iso_string
    rpc/
      mod.rs               // pub use types + server + client
      types.rs             // RpcCommand（33 变体）/ RpcResponse / RpcSessionState / RpcSlashCommand /
                           // RpcExtensionUIRequest / RpcExtensionUIResponse / RpcCommandType
      server.rs            // run_rpc_mode：装配、rebind、信号、shutdown、handle_input_line
      dispatch.rs          // handle_command 的 33 分支（建议独立文件，逐条对应 §2.6.7）
      ui_context.rs        // RpcExtensionUiContext（9 种 request）+ create_dialog_promise + editor
      pending.rs           // PendingExtensionRequests（id -> oneshot sender）
      client.rs            // RpcClient：spawn、JSONL reader、pending map、33 个命令方法、
                           // wait_for_idle / collect_events / prompt_and_wait
    raw_stdout.rs          // 本 crate 侧的 RawStdout trait 定义（实现由 ca-session 的 output-guard 提供）
  tests/
    jsonl_parity.rs
    json_event_parity.rs
    rpc_dispatch_parity.rs
    rpc_ui_parity.rs
    print_mode_parity.rs
    js_json_parity.rs      // V8 JSON.parse/stringify 对拍（含 §4.3 全表）
    client_process_exit.rs
    signal_exit_codes.rs
```

关键 trait / 类型：

```rust
/// 由 ca-session 的 output-guard 实现；本单元只依赖 trait，便于测试注入。
pub trait RawStdout: Send + Sync {
    fn write_raw(&self, text: &str);                  // 空串忽略；内部串行 + 背压重试
    fn wait_for_backpressure(&self) -> BoxFuture<'_, ()>;
    fn flush(&self) -> BoxFuture<'_, ()>;
    fn take_over(&self);                              // 幂等
}

/// 事件订阅的最小面（由 AgentSession 适配）。
pub trait SessionEvents: Send + Sync {
    fn subscribe(&self, cb: Box<dyn Fn(AgentSessionEvent) + Send + Sync>) -> Subscription;
    fn subscribe_agent(&self, cb: Box<dyn Fn(AgentEvent) + Send + Sync>) -> Subscription;
}

/// RPC 服务端的会话门面（把 §2.6.7 的 33 个分支与 AgentSession/Runtime 解耦，便于对拍）。
#[async_trait]
pub trait RpcSessionHost: Send + Sync {
    fn session(&self) -> &dyn AgentSessionFacade;
    fn runtime(&self) -> &dyn RuntimeFacade;
    fn rebind(&self) -> BoxFuture<'_, Result<(), Error>>;
    fn dispose(&self) -> BoxFuture<'_, Result<(), Error>>;
}
```

建议的并发模型：

- **单 actor 串行化**：`run_rpc_mode` 起一个主任务，持有 `mpsc::Receiver<RpcInput>`（来自 stdin 的
  每条 line + 信号 + 关停请求）。命令处理用 `tokio::spawn` 并发（与 TS 的“不排队”一致），
  但**所有 stdout 写入**经 `RawStdout` 的单链保证顺序。
- 关停用一个 `watch`/`Notify`：`shutdown_requested: AtomicBool` + `agent_settled` 时检查。
- `shutting_down: AtomicBool` + `compare_exchange` 实现 `shutdown` 的幂等重入。
- dialog 的 pending map：`Mutex<HashMap<String, oneshot::Sender<RpcExtensionUiResponse>>>`；
  dialog future 用 `tokio::select!` 合并 `oneshot` / `sleep(timeout)` / `cancellation`。
- `RpcClient`：一个 reader task（读子进程 stdout -> 行 -> 分发）+ 一个 pending map
  （`Mutex<HashMap<String, oneshot::Sender<RpcResponse>>>`）；每个请求 `tokio::time::timeout(30s)`。

### 8.2 与 ca-session 的接口清单（Rust 侧需要存在的类型）

```rust
pub enum PromptDisposition { Handled, Queued, Started }
pub enum QueuedInputDisposition { Handled, Queued }

pub struct PromptOptions {
    pub expand_prompt_templates: Option<bool>,   // 默认 true
    pub images: Option<Vec<ImageContent>>,
    pub streaming_behavior: Option<StreamingBehavior>, // Steer | FollowUp
    pub source: Option<InputSource>,
    pub preflight_result: Option<Box<dyn Fn(PromptDisposition) + Send + Sync>>,
}

pub struct ModelCycleResult { pub model: Model, pub thinking_level: ThinkingLevel, pub is_scoped: bool }

pub struct ExtensionBindings {
    pub ui_context: Option<Arc<dyn ExtensionUiContext>>,
    pub mode: Option<ExtensionMode>,
    pub command_context_actions: Option<CommandContextActions>,
    pub abort_handler: Option<Box<dyn Fn() + Send + Sync>>,
    pub shutdown_handler: Option<Box<dyn Fn() + Send + Sync>>,
    pub on_error: Option<Box<dyn Fn(ExtensionError) + Send + Sync>>,
}
```

`ExtensionUiContext` 在 Rust 侧建议做成**一个含 27 个方法的 trait**（与 TS 接口一一对应），
RPC 实现见 §2.6.5；print/json 模式传 `None`（由 ca-extensions 用 `NoOpUiContext`）。

### 8.3 建议的实施顺序

1. `js_json.rs`（数字格式 + 转义 + 键序 + `trim` + `to_iso_string`）——无依赖，先用 §4.2 的探测用例锁定。
2. `jsonl.rs`（`serialize_json_line` + `JsonlLineReader`，含 §4.1.1 的表）。
3. `json_event.rs`（`to_json_event`，逐 variant + 三个回归 #7290/#7911/#7925）。
4. `print_mode.rs`（§2.3.3 的 8 场景）。
5. `rpc/types.rs` + `rpc/ui_context.rs`（wire 记录与 dialog）。
6. `rpc/dispatch.rs`（33 分支，先做无副作用的查询类：`get_state`/`get_messages`/`get_entries`/`get_tree`/
   `get_commands`；再做会话变更类；最后 `prompt`/`bash`）。
7. `rpc/server.rs`（装配、信号、关停、`agent_settled` 门控）。
8. `rpc/client.rs`（spawn + 33 方法 + 超时 + 错误文本）。
9. `js_json.rs` 的 `JSON.parse` 错误消息（最后做，但必须在发布前完成——它是唯一无法“近似正确”的部分）。
10. 端到端黄金测试（真实二进制 + 事件流对拍）。

### 8.4 最难复刻的部分（按风险排序）

1. **V8 `JSON.parse` 错误消息逐字一致**（RPC 的 `Failed to parse command: ...`）。
   需要：错误分类（约 12 类）、UTF-16 位置计算、line/column 计算、`is not valid JSON` 的片段截断规则
   （长度 ≤ 20 全量引用，否则错误点附近 10 码元 + `...`）、单引号片段的转义规则。
   `serde_json` 的错误文本完全不可用。**必须自写解析器**，并用 §4.3 的全表 + Node 版本锁定做黄金测试。
   风险最高的原因是：任何输入都可能触发，且客户端可能依赖错误文本做判断。

2. **JS 数字格式化（`JSON.stringify` / `String(n)`）**。`ryu` 与 ECMAScript 的差异：
   指数阈值（`>= 1e21`、`< 1e-6`）、`e+21` 的 `+` 号、`.0` 后缀、`1e-7` 的写法。`usage`/`cost` 的浮点
   会大量出现（如 `0.0030000000000000005`）。需实现 `format_js_number(f64) -> String`：
   先取最短往返十进制（`ryu` 的 digits + exponent），再按 ECMAScript 规则排版。测试用
   `0.1`、`1/3`、`1e21`、`1e-7`、`5e-324`、`1.7976931348623157e308`、`-0`、`1e20`、`123456789012345680000`。

3. **stdout 接管 + 串行写 + 背压重试 + flush 语义**。TS 侧 `writeRawStdout` 是“发后不管 + 全局链”，
   `waitForRawStdoutBackpressure` 自旋到链尾稳定，`flushRawStdout` 写空串驱动一次回调。
   Rust 侧若用 `tokio::io::stdout()` + `write_all`，在管道满时会 await（等价于背压），
   但**重试语义**（ENOBUFS/EAGAIN/EWOULDBLOCK）与“空串 flush”需要显式实现。
   另外 `takeOverStdout` 的“把 console 输出重定向到 stderr”在 Rust 里没有对应物——需要把所有诊断
   输出统一走一个 `Diagnostics` 通道（固定 stderr），并在 RPC 模式下**确保**没有任何库代码直接写 stdout。

4. **RPC 关停时序**。三处触发 + `shuttingDown` 幂等重入 + SIGTERM 不 flush + `agent_settled` 门控 +
   “当前命令处理完后”检查 + 扩展 `shutdownHandler` 只置标志。任何一处顺序错误都会导致
   （a）退出码不对、（b）最后一条记录丢失、（c）扩展的 `session_shutdown` 未执行。
   特别地：`shutdown` 中 `await runtimeHost.dispose()` **之后**才 `detachInput` 与 `stdin.pause()`，
   而 `unsubscribe` 在 dispose **之前**——顺序必须逐条照抄。

5. **`prompt` 预检响应语义**。`preflightResult` 只在上游 `AgentSession.prompt` 的 8 条路径中的 4 条被调用；
   RPC 靠它产出唯一响应。Rust 侧若把 `prompt` 实现成“返回 `Result<Disposition>`”，必须保证
   语义等价：**拒绝时**不产出成功响应、由 catch 产出错误响应；**接受后**的异步失败不再产出响应。
   `_startRun` 的 `onStarted` 回调时机（`_prepareRun` 成功后、`agent.prompt` 前）也必须一致。

6. **`agent_settled` 恰好一次与 `aborted` 语义**。RPC 关停依赖它；JSON 事件流的最后一个记录通常是它。
   它由 ca-session 的 `_emitAgentSettled` 产出，需保证每个 run 恰好一次（含被 abort 的 run）。
   本单元的测试（`rpc-prompt-response-semantics.test.ts` 的 `sleep(150)`/`sleep(600)`）依赖该时序。

7. **`JSON.parse` 成功后的“非对象/畸形命令”边界**。`handleInputLine` 对顶层 `null`、数字、字符串、
   数组、缺字段对象的处理会产生**特定**的错误文本（含 V8 的 TypeError 文本，如
   `Cannot read properties of null (reading 'type')`）。这些文本在 TS 里是运行时产物，
   Rust 侧必须显式硬编码（或复刻 V8 的 TypeError 格式）。建议为每个边界写一条黄金用例。

8. **`StringDecoder` 的 U+FFFD 语义**。见 §4.1.1 的 15 行实测表。`encoding_rs` 的替换计数与 Node
   有差异（尤其 overlong / 代理区 / 截断 4 字节序列），需要手写状态机。

9. **`String.prototype.trim` 的字符集**（含 U+FEFF、不含 U+0085）。影响 `set_session_name` 的
   “空名字”判定与最终名字。测试：`"﻿"` -> 空 -> 错误；`"\u0085"` -> 非空 -> 接受。

10. **`RpcClient` 的子进程语义**。`spawn("node", ...)` 依赖 PATH；`exit` 事件携带 `(code, signal)`；
    `stdin.write` 的异步错误；`stop()` 的 1000ms SIGKILL 兜底；`start()` 的固定 100ms 等待；
    `exitCode !== null` 的前置检查。Rust 侧用 `tokio::process` 需要：
    （a）`try_wait()` 近似 `exitCode`；（b）`ExitStatusExt::signal()` -> `"SIGTERM"` 名称映射；
    （c）stderr 的“整体 `toString()` 累积”语义（不是增量解码）。

11. **重复 rebind**。`new_session` / `switch_session` / `fork` / `clone` 成功后，
    `runtimeHost` 内部已经 rebind 一次，本单元又调一次。这会让 `session_start` 扩展事件 emit 两次、
    订阅切换两次、`agent.subscribe` 增加又移除一次。Rust 侧若“优化”成一次，会改变扩展可见事件次数
    与背压订阅时序——**必须保留**。

12. **`theme` Proxy 的未初始化错误**。RPC 的 `uiContext.theme` getter 返回全局主题；若主题未初始化，
    读取任意属性会抛【逐字】`Theme not initialized. Call initTheme() first.`。Rust 侧若把主题做成
    `Option<Theme>`，必须在这条路径上复刻该错误（何时触发：扩展在 `session_start` 里读 `ctx.ui.theme`，
    而 `initTheme` 在 `runRpcMode` 之前由 `main.ts` 调用——正常不会触发）。

13. **`BashResult.exitCode` 的 `undefined` vs `null`**。被取消/信号杀死时 `exitCode` 为 `undefined`
    （键缺失），不是 `null`。Rust 的 `Option<i32>` 正好对应，但注意不要序列化成 `null`。

14. **`fork` 响应的 `text` 键缺失**。当 `selectedText` 为 `undefined` 时键缺失（文档示例即如此）。
    Rust 侧若用 `#[serde(default)]` + 非 Option 字段会序列化成 `""`，**必须**用 `Option<String>` +
    `skip_serializing_if`。

15. **时间戳**：`SessionHeader.timestamp` 用 `new Date().toISOString()`，固定 3 位毫秒 + `Z`。
    `chrono` 的 `to_rfc3339_opts(SecondsFormat::Millis, true)` 输出 `2024-12-03T14:00:00.000Z`
    （一致）；但年份 < 1000 时 `chrono` 输出 `0001-...`（一致），年份 > 9999 时 JS 输出 `+010000-...`
    （`chrono` 需检查）。会话 header 的时间戳来自 ca-session，本单元只透传。

### 8.5 已知可接受差异（记录在案）

| 差异 | 理由 |
|---|---|
| 孤立代理项（lone surrogate） | Rust `String` 不能容纳；替换为 U+FFFD。仅在模型输出含非法 UTF-16 时出现，概率极低 |
| `__proto__` 作为对象键 | JS 对象字面量里 `__proto__:` 设置原型；Rust map 视为普通键。协议记录不含该键 |
| `JSON.stringify` 的 `toJSON()` | Rust 无隐式 `toJSON`；本单元所有类型都由我们自己的结构体序列化，行为已固定 |
| 浮点 `-0` | `ryu` 输出 `-0.0`，JS 输出 `0`；自定义格式器必须处理 |
| 事件交错顺序 | 多命令并发时 TS 的 microtask 顺序无法逐字节复刻；只保证“同一命令内”的顺序 |
| `spawn` 的错误文本 | Node 的 `error.message`（如 `spawn node ENOENT`）与 Rust 的 `io::Error` 文本不同；`Agent process error: ...` 中的 `${error.message}` 会有差异。建议对 `NotFound` 映射为 `spawn node ENOENT` |

---

## 附录 A：本单元源码索引（逐行锚点）

| 内容 | 文件:行 |
|---|---|
| `resolveAppMode` / `toPrintOutputMode` | `src/main.ts:112` / `src/main.ts:125` |
| print/json 模式调用 | `src/main.ts:988` |
| RPC 模式调用（`await runRpcMode(runtime)`） | `src/main.ts:950` |
| `takeOverStdout` 调用点 | `src/main.ts:655` |
| `--mode` 校验与错误文案 | `src/cli/args.ts:96`–`110` |
| `--print` / `-p` | `src/cli/args.ts:177` |
| `rpc-entry.ts` 副作用 | `src/rpc-entry.ts:6`–`13` |
| `toJsonEvent` | `src/modes/json-event.ts:46`–`61` |
| `toJsonAssistantMessageEvent` | `src/modes/json-event.ts:20`–`38` |
| `runPrintMode` | `src/modes/print-mode.ts:33`–`173` |
| 信号处理（print） | `src/modes/print-mode.ts:50`–`68` |
| `rebindSession`（print） | `src/modes/print-mode.ts:74`–`119` |
| 文本输出块 | `src/modes/print-mode.ts:139`–`160` |
| `serializeJsonLine` | `src/modes/rpc/jsonl.ts:10`–`12` |
| `attachJsonlLineReader` | `src/modes/rpc/jsonl.ts:21`–`58` |
| `RpcCommand` | `src/modes/rpc/rpc-types.ts:20`–`74` |
| `RpcSlashCommand` | `src/modes/rpc/rpc-types.ts:81`–`90` |
| `RpcSessionState` | `src/modes/rpc/rpc-types.ts:96`–`109` |
| `RpcResponse` | `src/modes/rpc/rpc-types.ts:116`–`245` |
| `RpcExtensionUIRequest` | `src/modes/rpc/rpc-types.ts:252`–`287` |
| `RpcExtensionUIResponse` | `src/modes/rpc/rpc-types.ts:294`–`297` |
| `RpcCommandType` | `src/modes/rpc/rpc-types.ts:303` |
| `runRpcMode` 入口 + `takeOverStdout` | `src/modes/rpc/rpc-mode.ts:54`–`55` |
| `output` / `success` / `error` | `src/modes/rpc/rpc-mode.ts:60`–`77` |
| `createDialogPromise` | `src/modes/rpc/rpc-mode.ts:91`–`131` |
| `createExtensionUIContext` | `src/modes/rpc/rpc-mode.ts:136`–`311` |
| `rebindSession`（RPC） | `src/modes/rpc/rpc-mode.ts:317`–`364` |
| `registerSignalHandlers`（RPC） | `src/modes/rpc/rpc-mode.ts:366`–`380` |
| `handleCommand` | `src/modes/rpc/rpc-mode.ts:386`–`718` |
| `shutdown` / `checkShutdownRequested` | `src/modes/rpc/rpc-mode.ts:726`–`748` |
| `handleInputLine` | `src/modes/rpc/rpc-mode.ts:750`–`800` |
| stdin 装配 + 永不 resolve | `src/modes/rpc/rpc-mode.ts:802`–`819` |
| `RpcClient.start` | `src/modes/rpc/rpc-client.ts:74`–`140` |
| `RpcClient.stop` | `src/modes/rpc/rpc-client.ts:145`–`167` |
| `onEvent` / `getStderr` | `src/modes/rpc/rpc-client.ts:172`–`187` |
| 命令方法 | `src/modes/rpc/rpc-client.ts:198`–`461` |
| `waitForIdle` / `collectEvents` / `promptAndWait` | `src/modes/rpc/rpc-client.ts:471`–`517` |
| `handleLine` | `src/modes/rpc/rpc-client.ts:523`–`543` |
| `createProcessExitError` / `rejectPendingRequests` | `src/modes/rpc/rpc-client.ts:545`–`554` |
| `send` | `src/modes/rpc/rpc-client.ts:556`–`605` |
| `getData` | `src/modes/rpc/rpc-client.ts:607`–`616` |
| `output-guard` 全部 | `src/core/output-guard.ts:1`–`108` |
| `AgentSession.prompt` 预检路径 | `src/core/agent-session.ts:2092`–`2176`（`preflightResult?.("started")` 在 2173，函数闭合在 2176） |
| `_startRun` 的 `onStarted` | `src/core/agent-session.ts:1874`（函数声明）–`1890`（`onStarted?.()` 在 1890；doc 注释从 1873 开始） |
| `clearQueue` / `pendingMessageCount` | `src/core/agent-session.ts:2466`–`2480` |
| `getLastAssistantText` | `src/core/agent-session.ts:4407`–`4429` |
| `getUserMessagesForForking` | `src/core/agent-session.ts:4221`–`4237` |
| `executeBash`（`bash_execution_update`） | `src/core/agent-session.ts:3901`–`3933` |
| `bindExtensions` | `src/core/agent-session.ts:3318`–`3342` |
| `emitUserBash` | `src/core/extensions/runner.ts:1269`–`1298` |
| `noOpUIContext` | `src/core/extensions/runner.ts:331`–`362` |
| `wrapUIPromptContext` / `withUIPrompt` | `src/core/extensions/runner.ts:577`–`616` |
| `emitError` | `src/core/extensions/runner.ts:749`–`753` |
| `resolveRegisteredCommands` | `src/core/extensions/runner.ts:814`–`848`（`getRegisteredCommands` 在 854） |
| `AgentSessionRuntime` 全量 | `src/core/agent-session-runtime.ts:74`–`412`（类体到 412 行；文件共 447 行，末尾是再导出） |
| `getHeader` | `src/core/session-manager.ts:1505`–`1508` |
| `getEntries` / `getTree` / `getLeafId` | `src/core/session-manager.ts:1520` / `1529` / `1404` |
| `newSession`（header 构造） | `src/core/session-manager.ts:1057`–`1072` |
| `killTrackedDetachedChildren` | `src/utils/shell.ts:210`–`215` |
| `auth-guidance` 全部 | `src/core/auth-guidance.ts:1`–`30` |
| `ProviderLoginHelp` 文档路径 | `src/config.ts` 的 `getDocsPath()` |

## 附录 B：`RpcCommand` 33 个类型的完整清单（用于签名/枚举生成）

```
abort, abort_bash, abort_retry, bash, clear_queue, clone, compact, cycle_model,
cycle_thinking_level, export_html, follow_up, fork, get_available_models,
get_available_thinking_levels, get_commands, get_entries, get_fork_messages,
get_last_assistant_text, get_messages, get_session_stats, get_state, get_tree,
new_session, prompt, set_auto_compaction, set_auto_retry, set_follow_up_mode,
set_model, set_session_name, set_steering_mode, set_thinking_level, steer,
switch_session
```

（按字典序；与 §2.5.1 的分组清单一致。Rust 侧建议用 `#[serde(tag = "type", rename_all = "snake_case")]`
的内部标签枚举，但**注意** `follow_up`、`new_session`、`get_state` 等下划线命名与
`rename_all = "snake_case"` 一致；`followUp`（`streamingBehavior` 的值）不是命令名，别混淆。）

## 附录 C：`AssistantMessageEvent` -> wire 转换速查

| 输入事件 | `partial` | 输出额外字段 |
|---|---|---|
| `start` | 有 | 无（移除 `partial` 后只剩 `type`） |
| `text_start` | 有 | 无 |
| `text_delta` | 有 | 无 |
| `text_end` | 有 | 无 |
| `thinking_start` / `thinking_delta` / `thinking_end` | 有 | 无 |
| `toolcall_start` | 有 | `id`、`toolName`（从 `partial.content[contentIndex]` 取，必须 `type === "toolCall"`） |
| `toolcall_delta` | 有 | 无 |
| `toolcall_end` | 有 | 无（`toolCall` 字段保留） |
| `done` | 无 | 无 |
| `error` | 无 | 无 |

“无 partial”的行在 TS 里走 `!( "partial" in event )` 分支**原样返回**；Rust 侧统一按“移除 `partial`
后序列化”即可（结果相同）。

---

## 附录 D：关键常量和字面量汇总

```text
RAW_STDOUT_RETRY_DELAY_MS = 10
RpcClient start 等待                 = 100 ms
RpcClient stop SIGKILL 兜底          = 1000 ms
RpcClient 单请求超时                 = 30000 ms
RpcClient waitForIdle 默认超时        = 60000 ms
RpcClient collectEvents 默认超时      = 60000 ms
默认 cliPath                         = "dist/cli.js"
默认 spawn 命令                      = "node"
请求 id 前缀                          = "req_"
CURRENT_SESSION_VERSION              = 3
```

【逐字】错误/消息字面量清单：

```text
Failed to parse command: <message>
Unknown command: <type>
Model not found: <provider>/<modelId>
Entry not found: <since>
Session name cannot be empty
Cannot clone session: no current entry selected
Theme switching not supported in RPC mode
message_update message is not an assistant message
toolcall_start content at index <n> is not a tool call
Request <stopReason>
Response was truncated: the model reached its output token limit.
Extension error (<extensionPath>): <error>
Agent process exited (code=<code> signal=<signal>). Stderr: <stderr>
Agent process error: <message>. Stderr: <stderr>
Agent process stdin error: <message>. Stderr: <stderr>
Client already started
Client not started
Agent process stdin is not writable. Stderr: <stderr>
Timeout waiting for response to <type>. Stderr: <stderr>
Timeout waiting for agent to become idle. Stderr: <stderr>
Timeout collecting events. Stderr: <stderr>
Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object
Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.
Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
No API key found for <provider>.\n\nUse /login to log into a provider via OAuth or API key. See:\n  <docs>/providers.md\n  <docs>/models.md
No model selected.\n\nUse /login to log into a provider via OAuth or API key. See:\n  <docs>/providers.md\n  <docs>/models.md\n\nThen use /model to select a model.
No models available. Use /login to log into a provider via OAuth or API key. See:\n  <docs>/providers.md\n  <docs>/models.md
Authentication failed for "<provider>". Credentials may have expired or network is unavailable. Run '/login <provider>' to re-authenticate.
Theme not initialized. Call initTheme() first.
Error: @file arguments are not supported in RPC mode
Error: KIANA_STARTUP_BENCHMARK only supports interactive mode
Invalid mode "<mode>". Valid values: text, json, rpc
--mode requires text, json, or rpc
No API key for <provider>/<modelId>
Extension command "/<name>" cannot be queued. Use prompt() or execute the command when not streaming.
Already compacted
Nothing to compact (session too small)
Compaction cancelled
Compaction failed: <message>
Invalid entry ID for forking
Persisted session is missing a session file
This session has not been saved yet. Send a message before cloning or forking it.
Failed to create forked session
Stored session working directory does not exist: <sessionCwd>[\nSession file: <sessionFile>]\nCurrent working directory: <fallbackCwd>
```

（`Error:` 开头的三条与 `Invalid mode ...`、`--mode requires ...` 由 ca-cli / ca-extensions 产生，
列在此处便于端到端对拍。审计新增的 12 条来自 `agent-session.ts` / `agent-session-runtime.ts` /
`session-cwd.ts`，经 `handleInputLine` 的外层 catch 变成 `success:false` 的 `error` 文本
（例外：`Compaction failed: ...` 只出现在 `compaction_end.errorMessage` 与
`session_compact_failed` 扩展事件里，**不**作为 response 的 `error`——见 §2.6.7 `compact`）。）

（最后四条由 ca-cli / ca-extensions 产生，列在此处便于端到端对拍。）

---

## Audit additions

> 本节由对抗式完整性审计追加。上文已在原处更正若干错误陈述（同步修改处不再重复列举）；
> 本节按主题汇总**未在上文成段出现**或需要集中强调的内容。行号均针对源仓库
> `/home/ctyun/code/kiana_rev/kiana` 的当前 HEAD。

### A.1 上文已就地更正的错误（汇总）

1. §2.1：“共 14 个名字” -> **15**。
2. §2.5.1 标题：“39 个分支” -> **33**。
3. §2.3.2(6) 与 §5.1.3：把 `session.messages` 说成“session 投影” -> 实际是
   `agent.state.messages` 的**同一引用**（`agent-session.ts:1652`–`1654`）。
4. §2.8.1：`String(Uint8Array)` 的语义（`"104,105"`）与 `String(Buffer)`（UTF-8 解码）不同。
5. §5.4：包根 `src/index.ts` 实际导出**全部 15 个**名字（含 `ModelInfo`/`RpcEventListener`/`RpcCommand`/
   `RpcExtensionUIResponse` 等），旧文只列 6 个并声称其余不导出。
6. §2.6.7 `prompt` 的 `.catch` 边界：`new Error()` 的 `message === ""` 会写出 `"error":""`
   （键存在），与“非 Error -> 键缺失”是两种情形。
7. §4.3：`is not valid JSON` 片段窗口是 **20 码元**（长度 `>= 21` 时），不是 10 码元；
   长度 `<= 20` 时整段引用。
8. §4.2.4：小数阈值下界是 `1e-6`（含），旧文“即 `<= 1e-7`”在 `(1e-7, 1e-6)` 区间判错。
9. §7.1：`rpc-prompt-response-semantics.test.ts` 为 **9** 个用例（5 个 `it` + 2 个 `it.each` × 2）。
10. §2.1 脚注：`rpc-mode.ts` **不**导出 `RpcSlashCommand` / `RpcCommandType`。
11. §4.6：`package.json` 的 `kianaConfig` 只有 `configDir`，**没有** `name`（`APP_NAME` 默认 `"kiana"` 并非因
    `kianaConfig.name` 缺失以外的原因；描述本身可保留，但不要假设有 `name` 字段）。

### A.2 上文未覆盖的源码事实（按文件）

#### A.2.1 `src/modes/rpc/rpc-mode.ts`

- `runRpcMode` 的第一条语句是 `takeOverStdout()`；**没有** `restoreStdout`。函数返回
  `new Promise(() => {})`（`Promise<never>`）。
- `output` / `success` / `error` 是闭包，非导出（`rpc-mode.ts:60`–`77`）；
  `success(id, cmd, data?)` 用 `data === undefined` 判定是否带 `data`，`null` 会保留。
- `pendingExtensionRequests` 的 `reject` **从未被调用**；也没有超时/退订 reject 路径。
- `createDialogPromise` 的 `if (opts?.timeout)` 让 `timeout: 0` 等价于“无超时”；
  `timeout` 同时被放进 request（`undefined` 时键缺失）。
- `editor()` 与 dialog 共用 Map/id 空间，但**不支持** timeout/signal。
- `createExtensionUIContext()` 每次 rebind 新建对象；`pasteToEditor` 依赖 `this.setEditorText`
  （`ctx.ui` 被 `wrapUIPromptContext` 浅拷贝后 `this` 指向拷贝，仍可用；解构后调用会 `TypeError`）。
- `rebindSession()` 中 `unsubscribe?.(); unsubscribeBackpressure?.();` 在建立新订阅**之前**，
  因此 rebind 期间的事件丢失（与 print/json 一致）。
- `registerSignalHandlers()` 只在**首次** rebind 后调用一次；`newSession`/`switchSession`/`fork`/`clone`
  的后续 rebind **不**重新注册（不会重复注册）。
- `shutdown()` 的顺序：`shuttingDown` 幂等 -> 移除信号处理器 -> `unsubscribe` ->
  `unsubscribeBackpressure` -> `await runtimeHost.dispose()` -> `detachInput()` ->
  `process.stdin.pause()` -> 非 SIGTERM 时 `await flushRawStdout()` -> `process.exit(exitCode)`。
- `shutdown` 的 `exitCode` 对扩展请求路径是 `0`；`checkShutdownRequested()` 在每个成功命令后与
  每个 `agent_settled` 后调用（后者用 `void`，不阻塞事件写出）。
- `handleInputLine` 的 `extension_ui_response` 分支**不**等待 backpressure、**不**检查关停、
  不经 try/catch；未知 id 静默忽略。
- `handleCommand` 的 `switch` 没有 `default` 分支的显式枚举——`default` 用
  `(command as {type:string}).type` 拼 `Unknown command: ...`。
- 顶层 `null` 输入：`command.id` 先抛 `TypeError`，catch 里再次访问 `command.id` 又抛，
  最终 **unhandled rejection、无记录写出**（已在 §2.5.1 记录，此处强调 Rust 侧不要 panic）。

#### A.2.2 `src/modes/rpc/rpc-client.ts`

- `start()` 的 100ms 固定等待是 `setTimeout`，**无** unref；子进程若 100ms 后才退出，
  `start()` 仍成功返回，之后的请求通过 `exit` 监听 reject。
- `stop()` 每次都 `process.on("exit", ...)` **新增**监听器（不清除），多次 `stop()` 会累积；
  `pendingRequests.clear()` **不** reject（依赖 exit 监听已 reject 过）。
- `handleLine` 的判定是 `data.type === "response" && data.id && this.pendingRequests.has(data.id)`；
  响应若 `id` 为 `""`/`0`/缺失，会被当**事件**广播。
- 监听器遍历用 `[...this.eventListeners]` 快照；任一监听器抛错会中断**同一行**后续监听器
  （且被外层 `catch` 吞掉）。
- `send()` 的前置检查顺序：`!childProcess || !stdin`（`Client not started`）-> `exitError` ->
  `exitCode !== null`（新建并缓存 `createProcessExitError`）-> `stdin.destroyed || !stdin.writable`
  （`Agent process stdin is not writable. Stderr: ...`）。
- `send()` 的 `fullCommand = { ...command, id }`：`id` 在最后（覆盖调用方可能带的 id）。
- 请求超时 30000ms 固定；超时后从 Map 删除，**迟到响应会走事件广播**。
- `stdin.write` 同步抛错时立即 reject（不等 30s），并 `pending?.reject(writeError)`。
- `getData` 对 `success:false` 抛 `new Error(errorResponse.error)`；`error` 若为 `""` 会抛
  `Error("")`（message 空串）。成功但无 data -> `undefined`。
- `getLastAssistantText()` 的 TS 返回类型是 `Promise<string | null>`，实现是
  `getData<{text: string|null}>().text`——服务端实际给出 `{}` 时为 `undefined`（类型断言不改变运行时）。
- `RpcClient` **没有** `getAvailableModels()` 之外的模型查询方法；`setModel` 返回类型
  `{provider, id}` 是**类型断言**，运行时是完整 Model 对象。
- `RpcClient` **没有** 发送 `extension_ui_response` 的 API、也不解析 `extension_ui_request`
  （见 §4.8 补充）；这些记录作为事件传给 `onEvent`。

#### A.2.3 `src/modes/print-mode.ts`

- 局部状态声明顺序：`exitCode`、`session`、`unsubscribe`、`unsubscribeBackpressure`、`disposed`、
  `signalCleanupHandlers`。
- `registerSignalHandlers()` 在 `setRebindSession` / `rebindSession` 定义**之前**被立即调用
  （函数声明提升使 `rebindSession` 可用；信号处理器里只引用 `disposeRuntime`）。
- `disposeRuntime()`：`disposed` 幂等 -> 退订两条订阅 -> `await runtimeHost.dispose()`。
- `finally` 顺序：移除信号处理器 -> `await disposeRuntime()` -> `await flushRawStdout()`。
- `catch`：`console.error(error instanceof Error ? error.message : String(error))`，返回 1
  （**不**跑 `finally` 之外的额外清理；`finally` 仍执行）。
- json 分支的 header 写出用 `JSON.stringify(header)`（**不是** `serializeJsonLine`），
  但语义相同（单参 stringify + `\n`）。
- `session.agent.subscribe` 只在 `mode === "json"` 时建立背压订阅（text 模式无）；listener 忽略事件参数。
- `initialImages` 只用于 `initialMessage` 的 `{ images }`；`messages` 的每个 prompt 只传文本。
- `initialMessage` 为空串时**跳过**（falsy 判定），`messages` 为空数组时循环 0 次。

#### A.2.4 `src/modes/json-event.ts`

- `toJsonEvent` 对非 `message_update` 事件返回**同一引用**（无拷贝）；对 `message_update` 返回
  新对象，键序固定 `type`、`usage`、`assistantMessageEvent`。
- `usage` 引用 `event.message.usage`（非拷贝）。
- `toolcall_start` 的新增字段按 `{...deltaEvent, id, toolName}` 顺序追加（`id`/`toolName` 在最后）。
- 无 `partial` 键的变体（`done`/`error`）原样返回**同一引用**；有 `partial` 的返回浅拷贝。
- 两个 `throw` 的错误文本见 §8.4 清单；RPC 侧异常会导致该事件不写出（`output(toJsonEvent(...))`
  先求值）。

#### A.2.5 `src/modes/rpc/jsonl.ts`

- `attachJsonlLineReader` 的 detach 只 `off("data")` / `off("end")`；不清 decoder、不 flush buffer。
- 字符串 chunk 直接拼接（不经 decoder）；`Buffer`/`Uint8Array` 经 `decoder.write`。
- `onEnd` 时 `buffer += decoder.end()`，仅当 `buffer.length > 0` 才 emit 最后一行。
- 空行（连续 `\n\n`）会以 `""` 回调。

### A.3 需要补进 Rust 侧的类型/接口细节

| 项 | 细节 |
|---|---|
| `ExtensionUIContext` 成员数 | 28（27 个方法 + `get theme()` getter）。`custom` 在 RPC 返回 `undefined`；`getEditorComponent` 返回 `undefined`；`getAllThemes` 返回 `[]`；`getTheme` 返回 `undefined`；`getToolsExpanded` 返回 `false`；`getEditorText` 返回 `""`；`setTheme` 返回 `{success:false,error:"Theme switching not supported in RPC mode"}`；noOp 版本返回 `{success:false,error:"UI not available"}` |
| `hasUI` | `runner.uiContext !== noOpUIContext`；RPC 为 `true`，print/json 为 `false` |
| `ExtensionMode` | `"tui" \| "rpc" \| "json" \| "print"`，print 模式传 `mode === "json" ? "json" : "print"` |
| `ExtensionBindings` 的 `undefined` 语义 | 逐字段 `if (bindings.X !== undefined)` 才赋值——**不覆盖**旧值；因此 print/json 的二次 rebind 保留上次值 |
| `ContextUsage` | `{tokens: number\|null, contextWindow: number, percent: number\|null}`（`core/extensions/types.ts`），`get_session_stats` 的 `contextUsage` 用它 |
| `SessionStartEvent` | `{type:"session_start", reason:"startup"\|"reload"\|"new"\|"resume"\|"fork", previousSessionFile?}`；`bindExtensions` 每次都 `await emit(session_start)`、`reportUnhandledMcpServers()`、`extendResourcesFromExtensions(...)` |
| `ExtensionCommandContextActions` 的 6 个动作 | 见 §2.3.2(4)；RPC 版把 `parentSession` 等字段原样透传，**不**重打包（print 版显式重打包 `navigateTree` 的 4 个字段） |
| `SourceInfo` | `{path, source, scope:"user"\|"project"\|"temporary", origin:"package"\|"top-level", baseDir?}` |
| `SessionStats` | 见 §2.5.4 与 §2.6.7 的 `get_session_stats` 补充 |
| `BashResult` | `{output, exitCode?: number, cancelled, truncated, fullOutputPath?}`——`exitCode` 为 `undefined` 时键缺失（不是 `null`） |
| `CompactionResult` | `{summary, firstKeptEntryId, tokensBefore, estimatedTokensAfter?, usage?, details?}` |
| `ModelCycleResult` | `{model, thinkingLevel, isScoped}` |
| `PromptOptions` | `{expandPromptTemplates?, images?, streamingBehavior?, source?, preflightResult?}` |
| `PromptDisposition` / `QueuedInputDisposition` | `"handled"\|"queued"\|"started"` / `"handled"\|"queued"` |

### A.4 事件键序补充（易错点）

- `message_update` 的**内层**事件：`{...deltaEvent, id, toolName}`（`toolcall_start`）；
  其余变体删除 `partial` 后保持原键序。
- `turn_end` 是 `{type, message, toolResults}`；`agent_end` 是 `{type, messages, willRetry}`。
- `queue_update`：`{type, steering, followUp}`（数组为新拷贝）。
- `session_info_changed`：`{type, name}`（`undefined` -> 键缺失）。
- `compaction_start`：`{type, reason}`；`compaction_end`：`{type, reason, result, aborted, willRetry, errorMessage?}`。
- `auto_retry_start` / `auto_retry_end` / `summarization_retry_*`：键序见 §4.4.2；
  `summarization_retry_attempt_start` 在 `source === "compaction"` 时带 `reason`，`branchSummary` 时**
  无** `reason` 键。
- 嵌套工具事件（`ctx.executeTool`）：`tool_execution_start/update/end` 追加 **`parentToolCallId` 在最后**
  （`{type, toolCallId, toolName, args, parentToolCallId}` /
  `{..., args, partialResult, parentToolCallId}` / `{..., result, isError, durationMs?, parentToolCallId}`）。
  `durationMs` 仅在工具真正执行时有值；取消/未执行时缺键。
- `tool_execution_end` 的 `durationMs` 用单调时钟测量；键存在时位于 `isError` 之后、`parentToolCallId` 之前。
- `bash_execution_update`：`{type, id?, delta}`——`id` 来自命令 id（`undefined` 时缺键）。

### A.5 JSON / JS 运行时桥接补充（实测 Node v26.12.1）

- `JSON.stringify` 的整数键排序：数组索引键（`"0"`–`"4294967294"`）按数值升序先行，其余按插入序。
  本单元所有 wire 对象都由字面量构造、无整数键，因此**不受影响**，但 Rust 侧若用通用 `Value`
  透传模型对象里的 headers 等需要 preserve_order + 整数键规则。
- `JSON.stringify(-0)` -> `"0"`；`JSON.stringify(NaN/±Infinity)` -> `"null"`。
- `JSON.stringify("\u007f")` / `"\u0080"` / `" "` / `"﻿"` -> 原样（不转义）；
  `" "` / `" "` -> 原样；孤立代理项 -> `\udXXX`（小写）。
- `JSON.stringify` 顶层 `undefined`/函数/symbol -> `undefined`（`serializeJsonLine` 得到
  `"undefined\n"`）。
- `JSON.parse` 成功路径：重复键后者覆盖；`__proto__` 是普通自有键（`JSON.parse` 不触发 setter）。
- `JSON.parse` 失败消息的行终止符只认 `\n` 与 `\r`（`\r\n` 一次）；U+2028/U+2029 只当普通字符
  （出现在结构位置 -> `Unexpected token`，出现在字符串内合法）。
- `JSON.parse` 的空白集只有 TAB/LF/CR/SPACE 四个；NBSP/VT/FF/NEL/LS/PS/BOM 都不是空白。
- `JSON.parse("1e400")` -> `Infinity`（成功）。
- `Number::toString` 细节：`1e-6` -> `"0.000001"`（十进制）；`5e-7` -> `"5e-7"`（指数）；
  `1e21` -> `"1e+21"`；`1e20` -> `"100000000000000000000"`；`123456789012345680000` 为十进制。
- `String.prototype.trim` 集合已在 §4.2.6；补充实测：`"\u0085"` **不**被 trim，
  `"﻿"` **被** trim，`"᠎"` **不**被 trim。
- `String(Buffer)` 为 UTF-8 解码；`String(Uint8Array)` 为 `"a,b,c"`（见 §2.8.1）。

### A.6 环境变量与 CLI 的补充清单

- `KIANA_OFFLINE` / `--offline` -> 同时设置 `KIANA_SKIP_VERSION_CHECK=1`（写回进程环境）。
- `KIANA_SKIP_VERSION_CHECK`、`KIANA_PACKAGE_DIR`、`KIANA_TELEMETRY`、`KIANA_CODEMODE_MAX_WORKERS`、
  `KIANA_CACHE_RETENTION`、`KIANA_SHARE_VIEWER_URL`、`KIANA_RADIUS_GATEWAY`、`KIANA_CLEAR_ON_SHRINK`、
  `KIANA_HARDWARE_CURSOR`、`KIANA_CLIPBOARD_OSC52`、`KIANA_HYPERLINKS`、`KIANA_PROGRAM_STATUS`、
  `KIANA_IMAGE_PROTOCOL`、`KIANA_TRUE_COLOR`、`KIANA_TUI_ESC_TIMEOUT`、`VISUAL`/`EDITOR`、
  `HTTP_PROXY`/`HTTPS_PROXY` —— 均由 `docs/environment-variables.md` 列出；本单元只对
  `KIANA_CODING_AGENT`、`AI_AGENT`、`KIANA_CODING_AGENT_DIR`、`KIANA_CODING_AGENT_SESSION_DIR`、
  `KIANA_TIMING`、`KIANA_OFFLINE`、`KIANA_STARTUP_BENCHMARK` 有直接行为，其余为间接受影响
  （本单元不应读取它们；Rust 侧也不要自行解释）。
- 环境变量名是**派生**的：`ENV_AGENT_DIR = ${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`、
  `ENV_SESSION_DIR = ${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR`（`config.ts:585`–`586`）；
  rebrand 后变成 `MY_AGENT_CODING_AGENT_DIR` 等。
- `--export` / `--offline` / `--models` / `--approve` 见 §3.1；`--mode` 的诊断文本
  （`--mode requires text, json, or rpc`、`Invalid mode "<mode>". Valid values: text, json, rpc`）
  在 `args.ts:96`–`110`，两者都是 `diagnostics` 数组项，**不**消费下一个参数（缺值/以 `-` 开头时）。
- `--print`/`-p` 会**消费**下一个非 `@`、非 `-`（或以 `---` 开头）的参数进 `messages`（`args.ts:177`–`183`）。

### A.7 测试清单补充

- 本单元相关的**全部**测试文件（已在 §7.1/§7.2 列出的之外）：无其它直接测试。
  交叉检索 `modes/rpc|modes/print-mode|modes/json-event|rpc-client|RpcClient|runRpcMode|runPrintMode`
  在 `test/` 与 `examples/` 下只命中 §7 已列的文件加上 `examples/README.md`。
- `test/rpc.test.ts` 用 `test()` 而非 `it()`；共 18 个 `test` 调用（与 §7.1 的 18 一致）。
- `test/suite/regressions/9068` 的 RPC 部分为 `test.each(rpcCases)` 展开 3 例；interactive 部分
  `test.each([["!pwd", false], ["!!pwd", true]])` 展开 2 例（非本单元）。
- `test/stdout-cleanliness.test.ts` 运行的是 `src/cli.ts`（`--import <source-resolver>`），
  **不是** `dist/`；它通过临时 `settings.json` 的 `packages` + `npmCommand` 伪造
  `changed 1 package in 471ms` / `found 0 vulnerabilities` 的 npm chatter。
  断言 ① `--help`（stdout 重定向）：exit 0、stdout 含 `Usage:`、stderr 无 `Usage:` 无 chatter；
  ② `--mode json --help --approve`：exit 0、**stdout 为空**、stderr 含 chatter 与 `Usage:`。
- 缺少的辅助文件已在 §7.2 补充：`test/utilities.ts`、`test/model-runtime-test-utils.ts`、
  `test/test-network-env.ts`。

### A.8 Rust crate 映射的补充风险

1. **`serde_json` 的 `preserve_order` 只在 `Value` 上生效**；用派生 `Serialize` 的结构体键序由字段
   声明顺序决定——两者都要，且**不要**混用（`#[serde(flatten)]` 会破坏顺序）。
2. **`Option<T>` + `skip_serializing_if`** 只能表达“缺键”，不能表达“键存在且为 `null`”；
   `cycle_model`/`cycle_thinking_level` 的 `data: null` 需要显式的 `Option<Option<T>>` 或
   `enum Data { None, Null, Value(T) }`。
3. **错误文本里的空串 `error: ""`**（`new Error()` 路径）不能被 `skip_serializing_if` 误删。
4. **`prompt` 的 fire-and-forget 响应**：Rust 若把 `handle_command` 设计成返回 `Option<Response>`，
   必须允许“先返回 None、之后由回调写响应”；且回调执行在 `handle_input_line` 的 try/catch 之外
   （TS 里 `.catch` 在 Promise 上，不经过命令的 catch）。
5. **并发**：`void handleInputLine(line)` 意味着同一行后续命令可与前一行并发处理。Rust 侧若用单
   actor 串行处理命令，会改变 `prompt` 与后续命令的交错顺序（RPC 客户端按 id 关联，通常无碍，
   但 `bash_execution_update`/`queue_update` 的相对顺序会变）——至少在测试里要固定为“允许交错”。
6. **`writeRawStdout` 的“发后不管 + 全局链”**：Rust 若在 `write_raw` 里 `await` 完成，则
   `prompt`/事件路径的时序会变成同步背压，改变 `agent_settled` 与 flush 的相对时机。
   建议保持“入队即返回 + 单独 flush/backpressure 等待”。
7. **stdout 接管无对应物**：Rust 侧只能靠“所有诊断显式写 stderr”保证；需要审计第三方库是否有
   直接写 fd 1 的行为（例如 `println!`）。建议在二进制入口用 `gag` 类机制或代码审查保证。
8. **`process.exit` 不跑析构**：Rust 的 `std::process::exit` 同样跳过析构，但 **不** 保证 stdio
   buffer 冲刷；必须在 exit 前显式 flush（本单元已在 SIGTERM 路径刻意不 flush，要保留该差异）。
9. **`crypto.randomUUID()`**：v4、小写、带连字符；Rust `uuid::Uuid::new_v4().to_string()` 一致。
10. **`JSON.parse` 的 position 是 UTF-16 索引**：Rust 需把输入转成 `Vec<u16>`（或按 UTF-16 计数的
    包装），再做位置/行/列计算，否则含 astral 字符时 `position`/`column` 会偏。
11. **片段窗口可能切断代理对**：Rust `String` 无法表示孤立代理项；若要逐字节一致，必须用
    `Vec<u16>` 承载片段并在序列化时转义为 `\udXXX`。这是本单元**最难**的字节级对齐点之一。
12. **`fork` / `clone` / `new_session` / `switch_session` 的双 rebind**：见 §8.4 第 11 条；
    Rust 侧不要“优化”掉。
13. **`bindExtensions` 的 `undefined` 不覆盖**：Rust 的 `Option<T>` 字段若用 `unwrap_or` 会覆盖，
    必须区分“未提供”与“显式 `None`”（TS 的 `undefined` 只能来自未提供）。
14. **`theme` Proxy**：Rust 需要 `Option<Theme>` + 访问时报错；错误文本逐字
    `Theme not initialized. Call initTheme() first.`。
15. **`Server` 关停的 `process.stdin.pause()`**：Rust 侧对应“停止从 stdin 读取”，但**不要**关闭
    stdin（客户端可能仍在写；TS 只是 pause）。

### A.9 文档一致性（`docs/`）

- `docs/rpc.md`：协议四类记录、按 id 关联、`bash_execution_update` 是唯一带命令 id 的事件、
  `extension_ui_response` 不产生普通响应、LF-only 框架、不得用 `readline`、stdout 保留给协议、
  `prompt` 成功只代表接受/排队/处理、`agent_end` 后仍可能有自动工作、订阅先于 prompt。
  这些与实现一致（§1/§4.1/§4.4 已覆盖）。
- `docs/rpc-commands.md` 与实现的差异：`get_last_assistant_text` 的 `text` 文档为 `string | null`，
  实现为 `string | undefined`（无文本时 `data` 为 `{}`）；`fork` 的 `text` 文档未标可选。
  **以实现为准**（已在 §2.6.7 记录）。
- `docs/json.md`：会话头只在 JSON 模式；`message_update` 是 delta-only；`agent_settled` 语义；
  `start`/`done`/`error` 不作为 `message_update` 发出（类型仍允许）。与实现一致。
- `docs/cli-integration.md`：四种模式的选型表、print/json/rpc 的生命周期与 stdout 约定；
  未给出 `takeOverStdout` 的精确条件（`isPlainRuntimeMetadataCommand`），本规格已在 §1.3 补充。
- `docs/cli.md` 的模式段落是**文档措辞**，与 `--help` 原文不同（见 §3.1 补充）。
- `docs/session-format.md` 与 `docs/message-types.md` 是会话头/消息类型的权威参考；本单元只透传
  `SessionHeader` / `SessionEntry` / `AgentMessage`，不重新定义它们。

### A.10 交叉检索结论（本单元是否还有遗漏文件）

- `src/modes/` 下除 `index.ts`、`json-event.ts`、`print-mode.ts`、`rpc/**` 外的文件全部属于
  `interactive/**`（ca-interactive-mode 单元），不在本单元范围。
- 本单元直接依赖但未在 §2 逐行展开的邻居：`core/output-guard.ts`（已展开 §2.8）、
  `core/agent-session.ts` / `core/agent-session-runtime.ts` / `core/session-manager.ts` /
  `core/extensions/**` / `core/bash-executor.ts` / `core/compaction/**` / `core/timings.ts` /
  `cli/args.ts` / `main.ts` / `rpc-entry.ts` / `cli/setup.ts`（契约见 §5 与本附录）。
- `src/index.ts` 的再导出（15 个名字）与 `modes/index.ts` 的 15 个名字一致；`src/index.ts`
  还导出 `main`、`AgentSession` 等大量其它符号（非本单元）。
