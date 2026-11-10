# `agent-codemode` 移植规范（TypeScript → Rust）

本文件是单元 **`agent-codemode`** 的完整移植规范，同时充当验收清单与 Rust 侧设计说明。
目标：Rust 重写后**功能与用户交互完全一致**——相同的公开 API 语义、相同的错误消息、相同的消息/事件顺序、相同的线格式、相同的默认值与超时、相同的确定性指纹与随机数。

- 单元 id：`agent-codemode`
- 源文件范围：`/home/ctyun/code/kiana_rev/kiana/packages/agent/**`（`src/`、`test/`、`examples/`、`README.md`）
  与 `/home/ctyun/code/kiana_rev/kiana/packages/codemode/**`（`src/`、`test/`、`examples/`、`README.md`、`CHANGELOG.md`）。
- 源仓库为只读参考，不得修改。
- 两个包彼此独立发布：`@earendil-works/kiana-agent-core` 1.4.0 与 `@earendil-works/kiana-codemode` 1.4.0；本单元在 Rust 侧也应保持为**两个 crate**，依赖方向 `agent → kiana-ai`、`codemode → (无 kiana 依赖)`，二者之间**没有**代码依赖，只有 examples/文档层面的组合。
- 本单元没有任何 CLI、TUI、slash command、ANSI 颜色或按键处理；"用户可见表面"在此等于：**库 API、逐字错误消息、被模型看到的工具描述/声明文本、日志（console.warn）、以及被持久化/传输的 JSON**。

## 目录

1. 概览（§1.1 目的 / §1.2 位置 / §1.3 入口点 / §1.4 关键不变量 / §1.5 符号计数）
2. 逐文件规格（§2.1 agent 的 8 个源/文档文件；§2.2 codemode 的 17 个源/文档文件）
3. 用户可见表面（§3.1 公开 API、§3.2/§3.4 逐字错误、§3.5 声明渲染、§3.6 日志、§3.7 工具描述）
4. 外部格式（§4.1 HTTP+SSE、§4.2 worker 消息、§4.3 RPC、§4.4 源码格式、§4.5 无环境变量）
5. 跨单元接口（§5.1 kiana-ai、§5.2 typebox、§5.3 mcp、§5.4 外部依赖、§5.5 消费者）
6. npm 依赖与 Rust crate 映射（§6.1 JS 语义桥接清单）
7. 测试清单（§7.1-§7.4 agent、§7.5-§7.12 codemode）
8. Rust 模块布局（§8.1）、最难的 10 个点（§8.2）、次难点（§8.3）、移植顺序（§8.4）

附录 A：解析后的 prelude 全文（逐字节，17233 字符，供 `include_str!` 直接使用）

---

## 1. 概览

### 1.1 目的

**`kiana-agent-core`（agent）** 是一个与 transport 无关的 Agent 运行时：

- 定义 `AgentMessage`（LLM 消息 + 应用自定义消息的联合）与到 `kiana-ai` `Message` 的边界转换（`convertToLlm`）。
- 实现 agent loop：一次 `prompt()`/`continue()` 从「准备上下文」开始，经过 provider 流式响应、工具批处理（顺序/并行）、
  steering / follow-up 队列、`finishTurn` / `prepareRequest` / `prepareNextTurn` 钩子，直到 `agent_end`。
- `Agent` 类是有状态包装：维护 transcript（`state.messages`）、可执行工具集（`state.tools`）、流状态
  （`isStreaming` / `streamingMessage` / `pendingToolCalls` / `errorMessage`），按订阅顺序 await 监听器。
- `streamProxy()`：把 LLM 调用转给一个后端 proxy 的 stream function（POST `/api/stream`，SSE 风格逐行 `data: `）。
- 工具调用在 provider 边界的参数验证由 `kiana-ai` 的 `validateToolArguments()` 完成（TypeBox schema + 强制转换）。

**`kiana-codemode`（codemode）** 在 QuickJS（编译成 WebAssembly，`quickjs-wasi@3.6.2`）VM 中运行模型编写的 JavaScript，
脚本唯一的能力是调用宿主注入的 `tools.*` / globals：

- 三种后端：`CodemodeSandbox`（Node/Bun worker thread，每次执行一个 worker + 一个 wasm 实例）、
  `InlineCodemodeSandbox`（调用线程 + 中断预算）、`RemoteCodemodeSandbox`（VM 在远端 isolate，经 Workers RPC 交换消息）。
- 统一宿主侧 `CodemodeSandboxBase`：工具表、调用记录、输出、deadline、abort、以及消息解码与校验。
- VM 内先执行一段 **prelude**（17,233 个 UTF-16 单元、ASCII-only 的 JS 源码），冻结全部内建、构造 `tools`/`ALL_TOOLS`/
  `text`/`image`/`exit`/`console`/`store`/`load`/globals，并通过闭包中的单一 `bridge()` 与宿主通信。
- `start()` 是**确定性执行**：虚拟时钟（`startedAt` + 每个结果的 `now`，单调不回退）、seed 派生的随机数、UTC、
  无 `WeakRef`/`FinalizationRegistry`、按 `seq` 严格顺序应用结果、启动时报告指纹（fingerprint）。
- 声明渲染器 `renderDeclarations()/schemaToType()` 把 JSON Schema 渲染成给模型看的 TypeScript 声明。
- `parseCodemodeSource()` 解析首行 `// @options:`；`CODEMODE_SOURCE_GRAMMAR` 是给 grammar-constrained provider 的 Lark 语法。

### 1.2 在 monorepo 中的位置

```
kiana-ai ──────────────► kiana-agent-core ──────► coding-agent (extensions, 工具/子代理)
   ▲                        │                 durable (harness 侧)
   │                        └─ examples/mcp-codemode ◄── kiana-mcp
   └──────── kiana-codemode ◄── coding-agent/src/extensions/codemode
                 ▲
                 └─ durable/src/codemode（只用 /portable 与 /source）
```

- **依赖（本单元消费）**：
  - agent：`@earendil-works/kiana-ai@1.4.0`（类型、transcript 重放、identity、校验、EventStream、streaming-json）、
    `typebox@1.3.27`（`TSchema`/`Static` 类型；运行时校验在 kiana-ai 内）。
  - codemode：**零 kiana 依赖**；仅 `quickjs-wasi@3.6.2`（+ Cloudflare 入口的 `cloudflare:workers` 与
    `quickjs-wasi/quickjs.wasm` 编译导入，属 Wrangler 特性）。
- **被依赖（本单元暴露）**：
  - `coding-agent/src/extensions/codemode/{tool.ts,execute.ts,execute.lazy.ts,worker.ts,renderer.ts}`：
    用 `CodemodeSandbox`、`loadQuickJSWasm`、`parseCodemodeSource`、`renderToolSample`、`renderToolOutputType`、
    `toCodemodeIdentifier`、`CODEMODE_SOURCE_GRAMMAR`、`CodemodeResult`/`CodemodeOutputItem`/`CodemodeTool` 等。
  - `durable/src/codemode/codemode.ts`：用 `/portable` 的 `CodemodeSandboxBase`、`CodemodeHostCall`、`CodemodeOutcome`、
    `CodemodeResult`、`CodemodeRun`、`toCodemodeIdentifier` 与 `/source` 的 `parseCodemodeSource`、
    `CODEMODE_SOURCE_GRAMMAR`，实现确定性重放（journal/run/store 文档）。
  - `agent/examples/mcp-codemode`：`runToolCall()` + `renderDeclarations()` + `CodemodeSandbox`。

### 1.3 入口点（package exports，逐字）

`packages/agent/package.json`（`@earendil-works/kiana-agent-core@1.4.0`，`"type": "module"`，`engines.node >= 22.19.0`，
依赖 `@earendil-works/kiana-ai: 1.4.0`、`typebox: 1.3.27`）：

| specifier | 目标 |
|---|---|
| `.` | `./dist/index.js` / `./dist/index.d.ts` |
| `./package.json` | `./package.json` |

`packages/codemode/package.json`（`@earendil-works/kiana-codemode@1.4.0`，依赖 `quickjs-wasi: 3.6.2`）：

| specifier | source | 说明 |
|---|---|---|
| `.` | `src/index.ts` | 完整入口（含 worker 后端；Node/Bun） |
| `./declarations` | `src/declarations.ts` | 仅声明渲染器 |
| `./source` | `src/source.ts` | 仅 `@options` 解析 + Lark 语法（无 Node 依赖） |
| `./portable` | `src/portable.ts` | 无 Node 内建的入口（Inline/Remote/Base/protocol 类型） |
| `./cloudflare` | `src/cloudflare.ts` | Durable Object 端点 + Wrangler 编译的 wasm |
| `./worker` | `src/runtime/worker.ts` | worker 线程入口（导入即启动） |

Rust 侧对应：`kiana_agent_core` crate 的 `lib.rs`；`kiana_codemode` crate 的 `lib.rs` + `declarations`/`source`/`portable`/
`cloudflare`/`worker` feature 或子模块（见 §8）。

### 1.4 关键不变量（逐条在 Rust 中成立）

**agent**

1. `agentLoop()/agentLoopContinue()` **同步返回** `EventStream`；`agentLoopContinue()` 在创建流之前同步抛出
   `Cannot continue: no messages in context` / `Cannot continue from message role: assistant`（`runAgentLoopContinue()`
   亦在 await 前抛同样错误）。
2. 一次 run 的事件顺序：`agent_start` → `turn_start` → 初始消息的 `message_start`/`message_end` → 每次 assistant 响应
   （`message_start`，若干 `message_update`，`message_end`）→ 工具事件 → 工具结果消息 → `finishTurn` → `turn_end`
   →（若继续）`turn_start` … → 最后一个事件必为 `agent_end`。
3. `message_update` 只对 assistant 消息发出，且携带 `assistantMessageEvent` 原始事件与 `{...partialMessage}` 的浅拷贝。
4. `agent_end` 是最后一个事件，但 run 的**结算**（`prompt()`/`waitForIdle()` resolve、`isStreaming=false`）在所有
   订阅者对该事件（以及此前所有事件）的 await 完成后才发生。
5. 监听器按订阅顺序**串行 await**；非 run 期间的 `processEvents` 抛
   `Agent listener invoked outside active run`。
6. 工具批：`tool_execution_start` 在准备（校验/`beforeToolCall`）之前发出；`tool_execution_end` 在 finalize 之后发出；
   并行模式下 `tool_execution_end` 按**完成顺序**，工具结果消息与 `turn_end.toolResults` 按**源顺序**。
7. 提前终止仅当该批**每一个**最终结果 `result.terminate === true`（且批次非空）。
8. `stopReason === "length"` 的 assistant 消息中所有 toolCall 一律不执行，改为错误结果（文本见 §3）。
9. `stopReason === "error" | "aborted"` 是硬退出：仍然调用 `finishTurn`、仍发 `turn_end`/`agent_end`，但忽略其
   `continue` 决策，且**此后**不再轮询任何队列（steering 与 follow-up 都不再轮询）。注意 run 开始时的
   **初始 steering 轮询仍会发生一次**（`runLoop` 第一行），所以一次 `error`/`aborted` 的 run 的计数是
   `steeringPolls === 1`、`followUpPolls === 0`（§7.1 第 17 条对此有逐字断言）。
10. `durationMs` 只测 `execute()` 本身（单调时钟，`Math.round` 到整数毫秒），不含 `beforeToolCall`/`afterToolCall`；
    未实际执行的调用（未知工具、被 block、参数校验失败、aborted、`stopReason==="length"` 的整批）**没有**
    `durationMs` 字段；工具 `execute()` 抛错**有** `durationMs`（catch 分支也取时）。
11. transcript 拥有 system prompt 与工具声明：`context.tools` 是"可执行集合"，每次请求前用 system 消息的
    `toolsAdded`/`toolsRemoved` 把差异告知模型；差异按 `kiana-ai` 的 `getToolStateChanges()` 计算（定义变化 = 先删后加）。
12. `Agent.prompt()` 在 run 中抛错；`reset()` 在 run 中抛错；`continue()` 在 run 中抛错；错误文本逐字见 §3。
13. `state.tools = [...]` / `state.messages = [...]` 复制**顶层数组**；`state.messages` 之后由事件处理 push。
14. `streamAssistantResponse` 传给 streamFn 的选项是 `{...config, apiKey, identity: mergeClientIdentity(DEFAULT_AGENT_IDENTITY, config.identity), signal}`，
    并在最终消息上强制 `thinkingLevel = config.reasoning ?? "off"`。

**codemode**

15. `execute()` 对脚本失败**从不 reject**（返回 `{ok:false}`）；仅 `Sandbox is closed` 时 reject（`start()` 则是同步 throw）。
16. 首个终态事件获胜（`done`、`crash`、launcher failure、deadline、abort）；之后的事件全部忽略；`finish()` 幂等。
17. `finish()` 会 abort 所有未完成工具调用的 `signal`，把其 `CodemodeCall.status` 保持为 `"cancelled"`，并在
    `channel.stop()` 完成后才 resolve 结果（没有 channel 则立即 resolve）。
18. 所有工具参数/结果/return 值都经过 **JSON 文本**跨边界（worker/remote 只传字符串，不构造结构化值）。
19. prelude 与宿主之间的任何畸形消息都变成 `kind: "sandbox"` 错误，而非宿主异常。
20. `store`/`load` 同步；`load` 返回副本；`store(k, undefined)` 删除；写集合只在成功结果中返回。
21. 输出（`text`/`console.*`/`image`）累计超限即失败，即使脚本 catch 也失败，且 `done()` 已报告错误。
22. 确定性 run：结果按 `seq` 从 1 连续应用；时钟 `now` 不回退；`WeakRef`/`FinalizationRegistry` 不存在；时区 UTC。
23. 指纹字符串格式固定，任何影响确定性观察的改动都要同步改 `DETERMINISTIC_VERSION` 与（或）prelude 使其哈希变化。

### 1.5 作用域与符号计数

- `packages/agent/src/*.ts` 中的 `export` 声明名 **44 个**（含类型/接口/常量/函数/类）。
- `packages/codemode/src/**/*.ts` 中的 `export` 声明名 **75 个**（含仅子路径入口可见的 `startVm`、`inlineLauncher`、
  `PRELUDE_SOURCE`、协议类型守卫等）。
- 合计 **119** 个导出声明名（`export *` 与纯重导出不计）。

---

## 2. 逐文件规格

约定：签名用 TypeScript 原文；行为按实现顺序；所有字符串字面量逐字给出（Rust 必须逐字节一致）；
`?` 表示可选字段；"→" 表示返回。

### 2.1 agent 包

#### 2.1.1 `packages/agent/src/index.ts`

纯 barrel，无副作用：

```ts
export * from "./agent.ts";
export * from "./agent-loop.ts";
export * from "./proxy.ts";
export { setDefaultStreamFn } from "./stream-fn.ts";
export * from "./types.ts";
```

注意 `getDefaultStreamFn` 只被 `agent-loop.ts` 与 `agent.ts` 内部 import（两处），两个文件的 `export *` 都不含它；
`stream-fn.ts` 只显式导出 `setDefaultStreamFn`。`AgentEventSink`、`ToolCallHooks`、`RunToolCallOptions`、
`DEFAULT_AGENT_IDENTITY` 由 `agent-loop.ts` 导出；`ProxyAssistantMessageEvent`、`ProxyStreamOptions`、`streamProxy`
由 `proxy.ts` 导出；`AgentInitialState` 由 `agent.ts` 导出。

#### 2.1.2 `packages/agent/src/types.ts`

全部为类型/接口，无运行时代码。逐项（字段语义按注释原文要点）：

- `StreamFn = (model: Model<Api>, context: TranscriptContext, options?: SimpleStreamOptions) => AssistantMessageEventStream | Promise<AssistantMessageEventStream>`
  - 契约：**不得**为请求/模型/运行时失败 throw 或返回 rejected promise；必须返回事件流；失败编码为终态事件 +
    `stopReason: "error" | "aborted"` + `errorMessage`。
  - loop 传入的是"归一化 transcript"：system prompt 与工具声明在 transcript 的 system 消息里，**不在**
    `context.systemPrompt` / `context.tools`。
- `ToolExecutionMode = "sequential" | "parallel"`；`QueueMode = "all" | "one-at-a-time"`。
- `AgentToolCall = Extract<AssistantMessage["content"][number], { type: "toolCall" }>`
  （字段：`type:"toolCall"`, `id`, `name`, `arguments: JsonObject`, 可选 `thoughtSignature`/`namespace`/`argumentsError`）。
- `BeforeToolCallResult { block?: boolean; reason?: string; terminate?: boolean }`
  - `block:true` 阻止执行，loop 发错误结果；`reason` 成为错误文本；省略时用默认 blocked 文本。
- `AfterToolCallResult { content?: (TextContent|ImageContent)[]; details?: unknown; structuredContent?: JsonValue; isError?: boolean; usage?: Usage; terminate?: boolean }`
  - 合并语义：逐字段替换；`content` 若给出而 `structuredContent` 未给出，则**丢弃**原有 structuredContent。
- `BeforeToolCallContext { assistantMessage; toolCall; args; context }`（`args` 是校验后的参数）。
- `AfterToolCallContext { assistantMessage; toolCall; args; result; isError; context }`。
- `AgentTurnContext { message: AssistantMessage; toolResults: ToolResultMessage[]; context: AgentContext; newMessages: AgentMessage[] }`
  - `newMessages`：本次 loop 调用将返回的消息；prompt 运行含初始 prompt 消息，continuation 不含既有上下文。
- `AgentTurnDecision = { action: "continue" } | { action: "end" }`；`FinishTurn = (turn, signal?) => AgentTurnDecision | void | Promise<...>`。
- `AgentLoopTurnUpdate { context?; messages?; model?; thinkingLevel? }`。
- `PrepareRequestContext { context; model; thinkingLevel }`；
  `AgentRequestUpdate = Omit<AgentLoopTurnUpdate, "messages">`；
  `PrepareRequest = (request, signal?) => AgentRequestUpdate | void | Promise<...>`。
- `PrepareNextTurnContext extends AgentTurnContext {}`。
- `AgentLoopConfig extends SimpleStreamOptions`：`model`；`convertToLlm`（**必填**；契约：不得抛/拒绝，
  应返回安全回退值；若抛，"会中断低层 agent loop 而不产生正常事件序列"——即上层流不会 end，见 §2.1.4 的
  `agentLoop` 说明）；`transformContext?`（`(messages, signal?) => Promise<AgentMessage[]>`，同样"不得抛"契约）；
  `getApiKey?`；`finishTurn?`；`prepareRequest?`；`prepareNextTurn?((ctx) => AgentLoopTurnUpdate|undefined|Promise)`；
  `getSteeringMessages?`；`getFollowUpMessages?`；`toolExecution?`（默认 `"parallel"`）；`beforeToolCall?`；
  `afterToolCall?`。其余字段来自 `SimpleStreamOptions`（`temperature`、`samplingParams`、`maxTokens`、`reasoning`、
  `cacheRetention`、`sessionId`、`headers`、`metadata`、`transport`、`thinkingBudgets`、`maxRetryDelayMs`、
  `toolChoice`、`deferred`、`onPayload`、`onResponse`、`onProviderStreamEvent`、`apiKey`、`signal`、`identity`）。
- `ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"`。
- `CustomAgentMessages {}`（declaration merging 扩展点）。
- `AgentMessage = Message | CustomAgentMessages[keyof CustomAgentMessages]`。
- `AgentState`：
  - `readonly systemPrompt: string`（重放 transcript 得到；只读，改 prompt 需追加 system 消息）；
  - `model: Model<any>`、`thinkingLevel: ThinkingLevel`；
  - `tools`（getter/setter，setter 复制顶层数组）、`messages`（同）；
  - `readonly isStreaming: boolean`、`readonly streamingMessage?: AgentMessage`、
    `readonly pendingToolCalls: ReadonlySet<string>`、`readonly errorMessage?: string`。
- `AgentToolResult<T = JsonValue | undefined> { content: (TextContent|ImageContent)[]; details: T; structuredContent?: JsonValue; usage?: Usage; isError?: boolean; terminate?: boolean }`。
- `AgentToolCallOutcome { toolCall; result; isError; durationMs? }`。
- `AgentToolUpdateCallback<T = any> = (partialResult: AgentToolResult<T>) => void`。
- `AgentTool<TParameters extends TSchema = TSchema, TDetails = any> extends Tool<TParameters>`：
  `label: string`；`prepareArguments?: (args: unknown) => Static<TParameters>`；`outputSchema?: TSchema`；
  `execute: (toolCallId, params, signal?, onUpdate?) => Promise<AgentToolResult<TDetails>>`；
  `replay?: "never" | "safe"`；`executionMode?: ToolExecutionMode`。
- `AgentContext { messages: AgentMessage[]; tools?: AgentTool<any>[] }`。
- `AgentEvent` 联合（字段逐字）：
  - `{type:"agent_start"}`
  - `{type:"agent_end"; messages: AgentMessage[]}`
  - `{type:"turn_start"}`
  - `{type:"turn_end"; message: AgentMessage; toolResults: ToolResultMessage[]}`
  - `{type:"message_start"; message: AgentMessage}`
  - `{type:"message_update"; message: AgentMessage; assistantMessageEvent: AssistantMessageEvent}`
  - `{type:"message_end"; message: AgentMessage}`
  - `{type:"tool_execution_start"; toolCallId; toolName; args}`
  - `{type:"tool_execution_update"; toolCallId; toolName; args; partialResult}`
  - `{type:"tool_execution_end"; toolCallId; toolName; result; isError; durationMs?}`

#### 2.1.3 `packages/agent/src/stream-fn.ts`

```ts
let defaultStreamFn: StreamFn | undefined;
export function setDefaultStreamFn(streamFn: StreamFn | undefined): void  // 直接赋值，可传 undefined 清除
export function getDefaultStreamFn(): StreamFn
```
`getDefaultStreamFn` 在未配置时抛：
`No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().`
（**求值时机**：`agent-loop.ts` 的 `agentLoop`/`agentLoopContinue` 里 `streamFn ?? getDefaultStreamFn()` 是惰性的，
只在真正要发请求时求值；但 `Agent` 构造函数是**急切**的——`this.streamFunction = runtimeOptions.streamFn ?? getDefaultStreamFn()`
（`agent.ts:244`），所以未配置默认流函数时 `new Agent({...})` 会立即抛上述错误，而不是等到第一次 prompt。
同一进程内先 `setDefaultStreamFn(...)` 再 `new Agent(...)` 即可；已有 `Agent` 实例不会重新求值。）

#### 2.1.4 `packages/agent/src/agent-loop.ts`

导出：`AgentEventSink`（类型）、`DEFAULT_AGENT_IDENTITY`、`agentLoop`、`agentLoopContinue`、`runAgentLoop`、
`runAgentLoopContinue`、`runToolCall`、`RunToolCallOptions`、`ToolCallHooks`。

`DEFAULT_AGENT_IDENTITY: ClientIdentity = { attribution: KIANA_ATTRIBUTION }`，其中
`KIANA_ATTRIBUTION = { name: "kiana", url: "https://kiana.dev", categories: ["cli-agent"] }`（来自 kiana-ai）。

##### `agentLoop(prompts, context, config, signal, streamFn) → EventStream<AgentEvent, AgentMessage[]>`

- `stream = new EventStream(ev => ev.type === "agent_end", ev => ev.type === "agent_end" ? ev.messages : [])`。
- `void runAgentLoop(prompts, context, config, ev => stream.push(ev), signal, streamFn).then(msgs => stream.end(msgs))`。
- `runAgentLoop` 若 reject（例如 `convertToLlm` 抛错），流**不会** end（TS 实现无 catch）；Rust 应复刻为
  "流保持未完成"还是记录错误需保持一致——按 TS 行为：不 end、不 resolve，调用方会挂起（见 §8 open question）。

##### `agentLoopContinue(context, config, signal, streamFn)`

同步校验（在创建流之前）：
- `context.messages.length === 0` → `throw new Error("Cannot continue: no messages in context")`
- 最后一条 `role === "assistant"` → `throw new Error("Cannot continue from message role: assistant")`
随后同 `agentLoop`，调用 `runAgentLoopContinue`。

##### `runAgentLoop(prompts, context, config, emit, signal, streamFn): Promise<AgentMessage[]>`

1. `initialMessages = declareToolChanges(context, prompts)`。
2. `newMessages = [...initialMessages]`；`currentContext = { ...context, messages: [...context.messages, ...initialMessages] }`。
3. `await emit({type:"agent_start"})`；`await emit({type:"turn_start"})`。
4. 对每条 `initialMessages`：`message_start`、`message_end`。
5. `await runLoop(currentContext, newMessages, config, signal, emit, streamFn ?? getDefaultStreamFn())`。
6. 返回 `newMessages`（**同一数组引用**，后续继续 push）。

##### `runAgentLoopContinue(context, config, emit, signal, streamFn)`

- 同上的两条同步前置校验（同样错误文本）。
- `newMessages = []`；`currentContext = { ...context }`；`agent_start`、`turn_start`，然后 `runLoop`。

##### `runLoop` 精确算法（伪代码，Rust 必须等价）

```
currentContext = initialContext; config = initialConfig
lastCompletedTurn = undefined; explicitContinuation = false
pendingMessages = (await config.getSteeringMessages?.()) || []
while (true) {                       // 外层：follow-up
  hasMoreToolCalls = true
  while (hasMoreToolCalls || pendingMessages.length > 0) {   // 内层
    preparedMessages = []
    if (lastCompletedTurn) {
      snapshot = await config.prepareNextTurn?.(lastCompletedTurn)
      if (snapshot) {
        currentContext = snapshot.context ?? currentContext
        preparedMessages = snapshot.messages ?? []
        config = { ...config,
          model: snapshot.model ?? config.model,
          reasoning: snapshot.thinkingLevel === undefined ? config.reasoning
                   : snapshot.thinkingLevel === "off" ? undefined
                   : snapshot.thinkingLevel }
      }
      if (pendingMessages.length === 0)
        pendingMessages = (await config.getSteeringMessages?.()) || []
      await emit({type:"turn_start"})
    }
    for (msg of declareToolChanges(currentContext, [...preparedMessages, ...pendingMessages])) {
      await emit({type:"message_start", message: msg})
      await emit({type:"message_end", message: msg})
      currentContext.messages.push(msg); newMessages.push(msg)
    }
    pendingMessages = []
    update = await config.prepareRequest?.({context: currentContext, model: config.model,
                                            thinkingLevel: config.reasoning ?? "off"}, signal)
    if (update) { currentContext = update.context ?? currentContext
                  config = { ...config, model: update.model ?? config.model,
                    reasoning: update.thinkingLevel === undefined ? config.reasoning
                             : update.thinkingLevel === "off" ? undefined : update.thinkingLevel } }
    message = await streamAssistantResponse(currentContext, config, signal, emit, streamFunction)
    newMessages.push(message)
    if (message.stopReason === "error" || message.stopReason === "aborted") {
      lastCompletedTurn = { message, toolResults: [], context: currentContext, newMessages }
      await config.finishTurn?.(lastCompletedTurn, signal)
      await emit({type:"turn_end", message, toolResults: []})
      await emit({type:"agent_end", messages: newMessages}); return
    }
    toolCalls = message.content.filter(c => c.type === "toolCall")
    toolResults = []; hasMoreToolCalls = false
    if (toolCalls.length > 0) {
      batch = message.stopReason === "length"
        ? await failToolCallsFromTruncatedMessage(toolCalls, emit)
        : await executeToolCalls(currentContext, message, config, signal, emit)
      toolResults.push(...batch.messages)
      hasMoreToolCalls = !batch.terminate
      for (r of toolResults) { currentContext.messages.push(r); newMessages.push(r) }
    }
    lastCompletedTurn = { message, toolResults, context: currentContext, newMessages }
    decision = await config.finishTurn?.(lastCompletedTurn, signal)
    await emit({type:"turn_end", message, toolResults})
    if (decision?.action === "end") { await emit({type:"agent_end", messages: newMessages}); return }
    explicitContinuation = decision?.action === "continue"
    pendingMessages = (await config.getSteeringMessages?.()) || []
    if (hasMoreToolCalls || pendingMessages.length > 0) explicitContinuation = false
  }
  followUp = (await config.getFollowUpMessages?.()) || []
  if (followUp.length > 0) { explicitContinuation = false; pendingMessages = followUp; continue }
  if (explicitContinuation) { explicitContinuation = false; continue }
  break
}
await emit({type:"agent_end", messages: newMessages})
```

关键点：
- 初始 steering 轮询发生在进入外层循环**之前**（`getSteeringMessages` 每次 run 至少调用一次；`Agent` 的
  `skipInitialSteeringPoll` 只跳过这一次）。
- `prepareNextTurn` 只在 `lastCompletedTurn` 存在时、且每次内层迭代顶部调用；其 `messages` 与 `pendingMessages`
  一起经 `declareToolChanges` 处理（可合并工具变更到 pending system 消息）。
- `prepareNextTurn` 之后、`turn_start` 之前会再补一次 steering 轮询（仅当之前没有 pending）。
- `prepareRequest` 之后**不再**轮询 steering。
- 只有正常结束的 turn 才可能设置 `explicitContinuation`；`hasMoreToolCalls` 在无 toolCall 时为 `false`。

##### `declareToolChanges(context, pendingMessages): AgentMessage[]`

1. 从后向前找最后一条 `role === "system"` 的 pending 消息索引 `systemIndex`（找不到为 -1）。
2. `pending = pendingMessages[systemIndex]`；`baseline` = 若 pending 存在，则把该条替换为
   `withToolChanges(pending, NO_CHANGES)`（即去掉 `toolsAdded`/`toolsRemoved`）后的数组，否则就是 `pendingMessages`。
3. `changes = getToolStateChanges(getCurrentTools([...context.messages, ...baseline]),
   (context.tools ?? []).map(toToolDeclaration))`（kiana-ai）。
4. `unchanged = changes.toolsAdded.length === 0 && changes.toolsRemoved.length === 0`。
5. 若 `pending` 存在：
   - `unchanged && !pending.toolsAdded?.length && !pending.toolsRemoved?.length` → 原样返回 `pendingMessages`
     （保留调用方对象）。
   - 否则返回 `baseline` 且把 `systemIndex` 那条替换为 `withToolChanges(pending, changes)`。
6. 若 `unchanged` → 原样返回。
7. 否则创建 `withToolChanges({role:"system", content:"", timestamp: Date.now()}, changes)`，插入位置为
   第一条 `role !== "system"` 之前（若全是 system 则末尾），返回新数组。
- `withToolChanges` 复制消息并**删除**原 `toolsAdded`/`toolsRemoved`，仅当列表非空时写回。
- `NO_CHANGES = { toolsAdded: [], toolsRemoved: [] }`。

##### `streamAssistantResponse(context, config, signal, emit, streamFunction): Promise<AssistantMessage>`

1. `messages = context.messages`；若 `config.transformContext` 存在 → `await transformContext(messages, signal)`。
2. `llmMessages = await config.convertToLlm(messages)`。
3. `llmContext = normalizeContext({ messages: llmMessages })`（kiana-ai：把 `systemPrompt`/`tools` 折进首条 system 消息；
   此处二者皆无，等价于原样包装为 `TranscriptContext`）。
4. `resolvedApiKey = (config.getApiKey ? await config.getApiKey(config.model.provider) : undefined) || config.apiKey`。
5. `response = await streamFunction(config.model, llmContext, { ...config, apiKey: resolvedApiKey,
   identity: mergeClientIdentity(DEFAULT_AGENT_IDENTITY, config.identity), signal })`。
6. `result = async () => Object.assign(await response.result(), { thinkingLevel: config.reasoning ?? "off" })`。
7. `partialMessage = null; addedPartial = false`；
   `for await (event of response)`：
   - `"start"`：`partialMessage = event.partial`；`context.messages.push(partialMessage)`；`addedPartial = true`；
     `emit message_start { ...partialMessage }`。
   - `"text_start" | "text_delta" | "text_end" | "thinking_start" | "thinking_delta" | "thinking_end" |
      "toolcall_start" | "toolcall_delta" | "toolcall_end"`：仅当 `partialMessage` 非 null 时，
     `partialMessage = event.partial`；替换 `context.messages[last]`；`emit message_update
      { assistantMessageEvent: event, message: {...partialMessage} }`。
   - `"done" | "error"`：`finalMessage = await result()`；若 `addedPartial` 替换最后一条，否则 push；若
     `!addedPartial` 先 `emit message_start {...finalMessage}`；`emit message_end finalMessage`；返回。
8. 流结束但没有 done/error：`finalMessage = await result()`；若 `addedPartial` 替换最后一条，否则 push 并先
   `emit message_start`；然后 `emit message_end`、返回。
   （对正常 kiana-ai 流 done/error 一定发生；若流被 `end()` 而没有终态事件，`result()` 可能永远 pending ——
   保持与 TS 相同。）
9. 三条路径的共同点：`context.messages` 里最终留下的是**最终消息**（若曾 push 过 partial），且
   `newMessages` 由调用方在 runLoop 里 push 一次（`streamAssistantResponse` 自己**不** push 到 `newMessages`）。

注意：`event.partial` 被直接放进 `context.messages`（引用同一对象），随后的 delta 事件替换为新对象；
`message_update` 的 `message` 是浅拷贝。

##### `failToolCallsFromTruncatedMessage(toolCalls, emit): Promise<{messages, terminate:false}>`

对每个 toolCall：
- `emit tool_execution_start { toolCallId: id, toolName: name, args: arguments }`；
- 结果 = `createErrorToolResult(`Tool call "${name}" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.`)`；
- `emit tool_execution_end`（无 `durationMs`）；
- 构造 toolResult 消息（`createToolResultMessage`）并 `emit message_start`/`message_end`；
- 收集消息。
返回 `{ messages, terminate: false }`（因此 loop 会继续，让模型重发）。

##### `executeToolCalls(...)`

- `toolCalls = assistantMessage.content.filter(c => c.type === "toolCall")`。
- `hasSequentialToolCall = toolCalls.some(tc => currentContext.tools?.find(t => t.name === tc.name)?.executionMode === "sequential")`。
- `config.toolExecution === "sequential" || hasSequentialToolCall` → 顺序；否则并行。

##### `executeToolCallsSequential(...)`

对每个 toolCall：
1. `emit tool_execution_start {toolCallId, toolName, args: toolCall.arguments}`。
2. `preparation = await prepareToolCall(...)`。
3. 若 `kind === "immediate"`：`finalized = { toolCall, result: preparation.result, isError: preparation.isError }`
   （**无** `durationMs`）。
4. 否则 `executed = await executePreparedToolCall(preparation, signal, emitToolExecutionUpdate(toolCall, emit))`，
   再 `finalized = await finalizeExecutedToolCall(currentContext, assistantMessage, preparation, executed, config, signal)`。
5. `emitToolExecutionEnd(finalized)`；`createToolResultMessage(finalized)`；`emitToolResultMessage`；收集。
6. `if (signal?.aborted) break`（剩余 toolCall 不再执行，也不产生结果消息）。
返回 `{ messages, terminate: shouldTerminateToolBatch(finalizedCalls) }`。

`shouldTerminateToolBatch(finalizedCalls) = finalizedCalls.length > 0 && finalizedCalls.every(f => f.result.terminate === true)`。

##### `executeToolCallsParallel(...)`

1. `finalizedCalls: (FinalizedToolCallOutcome | (() => Promise<FinalizedToolCallOutcome>))[] = []`。
2. 预处理循环（顺序）：
   - `emit tool_execution_start`。
   - `preparation = await prepareToolCall(...)`。
   - immediate：`finalized`（无 durationMs）；**立即** `await emitToolExecutionEnd(finalized)`；push；若
     `signal?.aborted` → break；否则 continue。
   - prepared：push 一个闭包：
     - 若 `signal?.aborted` → 结果 `createErrorToolResult("Operation aborted")`, `isError: true`，emit end，返回；
     - 否则 execute → finalize → emit end → 返回 finalized。
   - push 后若 `signal?.aborted` → break。
3. `orderedFinalizedCalls = await Promise.all(finalizedCalls.map(e => typeof e === "function" ? e() : Promise.resolve(e)))`
   （**按源顺序启动**，闭包并行跑）。
4. 按 `orderedFinalizedCalls` 顺序构造 toolResult 消息并 emit（源顺序）。
5. 返回 `{ messages, terminate: shouldTerminateToolBatch(orderedFinalizedCalls) }`。

要点：并行模式下 immediate 结果的 `tool_execution_end` 在预处理阶段就发出（可能与后续工具的 start 交错）；
prepared 工具的 end 按实际完成顺序；`tool_execution_update` 由工具在运行中触发。

##### `prepareToolCall(currentContext, assistantMessage, toolCall, config, signal, tools = currentContext.tools ?? [])`

1. `tool = tools.find(t => t.name === toolCall.name)`；未找到 →
   `{kind:"immediate", result: createErrorToolResult(`Tool ${toolCall.name} not found`), isError: true}`。
2. try：
   - `preparedToolCall = prepareToolCallArguments(tool, toolCall)`：
     若 `tool.prepareArguments` 存在则调用；若返回值 `=== toolCall.arguments` 原样返回；否则返回
     `{...toolCall, arguments: prepared as Record<string, any>}`。
   - `validatedArgs = validateToolArguments(tool, preparedToolCall)`（kiana-ai；抛错进入 catch）。
   - 若 `config.beforeToolCall`：`beforeResult = await config.beforeToolCall({assistantMessage, toolCall, args: validatedArgs, context: currentContext}, signal)`。
     - `signal?.aborted` → immediate `Operation aborted`。
     - `beforeResult?.block` → `result = createErrorToolResult(beforeResult.reason || "Tool execution was blocked")`；
       若 `beforeResult.terminate === true` 则 `result.terminate = true`；immediate, isError true。
   - `signal?.aborted` → immediate `Operation aborted`。
   - 返回 `{kind:"prepared", toolCall, tool, args: validatedArgs}`。
3. catch (error) → immediate `createErrorToolResult(error instanceof Error ? error.message : String(error))`, isError true。
   注意：`beforeToolCall` 内抛错也走这里。

`beforeToolCall` 可以修改 `args`（校验后的同一对象），修改不重新校验，直接传给 execute（测试 #528）。

##### `runToolCall(toolCall, options): Promise<AgentToolCallOutcome>`

- `RunToolCallOptions extends ToolCallHooks { tools; assistantMessage; context; signal?; onUpdate? }`。
- 与模型发起的调用走同一流程：`prepareToolCall(..., options.tools)` → immediate 则返回（无 durationMs）；
  否则 `executePreparedToolCall(preparation, signal, options.onUpdate ?? (() => {}))` → `finalizeExecutedToolCall`。
- 不 emit 事件、不加消息；对工具失败**永不 reject**。

##### `executePreparedToolCall(prepared, signal, onUpdate): Promise<{result, isError, durationMs}>`

```
updateEvents = []; acceptingUpdates = true
startedAt = performance.now()
try {
  result = await prepared.tool.execute(prepared.toolCall.id, prepared.args, signal, partial => {
    if (!acceptingUpdates) return
    updateEvents.push(Promise.resolve(onUpdate(partial)))
  })
  durationMs = Math.round(performance.now() - startedAt)
  acceptingUpdates = false
  await Promise.all(updateEvents)
  return { result, isError: result.isError === true, durationMs }
} catch (error) {
  durationMs = Math.round(performance.now() - startedAt)
  acceptingUpdates = false
  await Promise.all(updateEvents)
  return { result: createErrorToolResult(msg(error)), isError: true, durationMs }
} finally { acceptingUpdates = false }
```
- 工具 settle 之后的 `onUpdate` 调用被忽略（不产生未处理 rejection）。
- 抛错时也保留 `durationMs`（因此 `tool_execution_end` 带 durationMs）。
- `durationMs` 为整数毫秒（`Math.round`）。

##### `finalizeExecutedToolCall(...)`

```
result = executed.result; isError = executed.isError
if (config.afterToolCall) {
  try {
    after = await config.afterToolCall({ assistantMessage, toolCall: prepared.toolCall, args: prepared.args,
                                         result, isError, context: currentContext }, signal)
    if (after) {
      structuredContent = after.structuredContent ?? (after.content ? undefined : result.structuredContent)
      result = { ...result,
        content: after.content ?? result.content,
        details: after.details ?? result.details,
        usage: after.usage ?? result.usage,
        terminate: after.terminate ?? result.terminate }
      if (structuredContent === undefined) delete result.structuredContent
      else result.structuredContent = structuredContent
      isError = after.isError ?? isError
    }
  } catch (error) {
    result = createErrorToolResult(msg(error)); isError = true
  }
}
return { toolCall: prepared.toolCall, result, isError, durationMs: executed.durationMs }
```

##### 辅助

- `createErrorToolResult(message) = { content: [{type:"text", text: message}], details: {} }`。
- `emitToolExecutionEnd(finalized)`：emit `{type:"tool_execution_end", toolCallId, toolName, result, isError,
  ...(durationMs === undefined ? {} : {durationMs})}`。
- `createToolResultMessage(finalized)`：
  `{ role:"toolResult", toolCallId, toolName, content: finalized.result.content ?? [], details, usage,
     isError, ...(durationMs===undefined?{}:{durationMs}), timestamp: Date.now() }`。
- `emitToolResultMessage(msg)`：`message_start` + `message_end`。
- `emitToolExecutionUpdate(toolCall, emit)` 返回 `partialResult => emit({type:"tool_execution_update",
  toolCallId, toolName, args: toolCall.arguments, partialResult})`。

#### 2.1.5 `packages/agent/src/agent.ts`

导出：`QueueMode`（重导出）、`AgentInitialState`、`AgentOptions`、`Agent`。
内部：`defaultConvertToLlm`、`EMPTY_USAGE`、`DEFAULT_MODEL`、`MutableAgentState`、`createMutableAgentState`、
`PendingMessageQueue`、`ActiveRun`。

- `defaultConvertToLlm(messages)`：过滤 `role` ∈ {system,user,assistant,toolResult}，其余（自定义消息）丢弃。
- `EMPTY_USAGE = { input:0, output:0, cacheRead:0, cacheWrite:0, totalTokens:0,
  cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0} }`。
- `DEFAULT_MODEL = { id:"unknown", name:"unknown", api:"unknown", provider:"unknown", baseUrl:"",
  reasoning:false, input:[], cost:{input:0,output:0,cacheRead:0,cacheWrite:0}, contextWindow:0, maxTokens:0 }`。
- `AgentInitialState = Partial<Omit<AgentState, "pendingToolCalls"|"isStreaming"|"streamingMessage"|"errorMessage">>`。
- `createMutableAgentState(initialState?)`：
  - `tools = initialState?.tools?.slice() ?? []`；`messages = initialState?.messages?.slice() ?? []`；
  - `initialMessage = createInitialSystemMessage(initialState?.systemPrompt, tools.map(toToolDeclaration))`；
    若 `messages[0]?.role !== "system"` 且 `initialMessage` 存在 → `messages.unshift(initialMessage)`；
  - `systemPrompt` getter → `getCurrentSystemPrompt(messages)`；
  - `model` 默认 `DEFAULT_MODEL`；`thinkingLevel` 默认 `"off"`；
  - `tools`/`messages` getter 返回当前数组引用；setter 存 `next.slice()`；
  - `isStreaming=false`、`streamingMessage=undefined`、`pendingToolCalls=new Set()`、`errorMessage=undefined`。
- `AgentOptions`：`initialState?`、`convertToLlm?`、`transformContext?`、`streamFn`（类型上必填但运行时容错）、
  `getApiKey?`、`onPayload?`、`onResponse?`、`onProviderStreamEvent?`、`beforeToolCall?`、`afterToolCall?`、
  `finishTurn?`、`prepareRequest?`、`prepareNextTurn?((signal?) => ...)`、`prepareNextTurnWithContext?((ctx, signal?) => ...)`、
  `steeringMode?`、`followUpMode?`、`sessionId?`、`thinkingBudgets?`、`transport?`、`maxRetryDelayMs?`、
  `toolExecution?`、`identity?`。
- `PendingMessageQueue`：
  - `enqueue(msg)` push；`hasItems()`；`peek()`：mode `"all"` → 全部（slice）；`"one-at-a-time"` → 第一条（无则 `[]`）；
  - `drain()`：`peek()` 后把队列设为剩余；`clear()` 清空。
- `Agent` 字段：`convertToLlm`、`transformContext`、`streamFunction`、`getApiKey`、`onPayload`、`onResponse`、
  `onProviderStreamEvent`、`beforeToolCall`、`afterToolCall`、`finishTurn`、`prepareRequest`、`prepareNextTurn`、
  `prepareNextTurnWithContext`、`sessionId`、`thinkingBudgets`、`transport`、`maxRetryDelayMs`、`toolExecution`、
  `identity`；私有 `_state`、`listeners: Set`、`steeringQueue`、`followUpQueue`、`activeRun?`。
- 构造函数：`runtimeOptions = options ?? {}`（兼容旧调用）；`streamFunction = runtimeOptions.streamFn ?? getDefaultStreamFn()`；
  `steeringMode` 默认 `"one-at-a-time"`、`followUpMode` 默认 `"one-at-a-time"`；`transport` 默认 `"auto"`；
  `toolExecution` 默认 `"parallel"`。
- `subscribe(listener) → unsubscribe`：加入 Set，返回删除函数；不立即发事件。
- `get state()` 返回内部可变状态对象。
- `steeringMode`/`followUpMode` get/set（set 只改队列的 mode）。
- `steer(msg)` / `followUp(msg)` 入队；`clearSteeringQueue()` / `clearFollowUpQueue()` / `clearAllQueues()`。
- `hasQueuedMessages()`：任一队列非空。
- `peekQueuedMessages()`：steering 非空则返回其 `peek()`，否则返回 follow-up 的 `peek()`；不消费。
- `get signal()`：`activeRun?.abortController.signal`。
- `abort()`：`activeRun?.abortController.abort()`（无运行时不抛）。
- `waitForIdle()`：`activeRun?.promise ?? Promise.resolve()`。
- `reset()`：有 activeRun → 抛 `Agent is already processing. Wait for completion before resetting.`；
  否则 `baseline = getCurrentSystemMessage(state.messages)`；`messages = baseline ? [baseline] : []`；
  `isStreaming=false`、`streamingMessage=undefined`、`pendingToolCalls=new Set()`、`errorMessage=undefined`；
  清空两个队列。
- `prompt(input, images?)`：有 activeRun → 抛
  `Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.`；
  `normalizePromptInput`：
  - 数组 → 原样返回（不复制、不加时间戳）；
  - 非字符串非数组（AgentMessage）→ `[input]`；
  - 字符串 → `[{role:"user", content:[{type:"text",text:input}, ...images], timestamp: Date.now()}]`
    （仅当 `images?.length > 0` 时追加图片）。
  然后 `runPromptMessages(messages)`。
- `continue()`：有 activeRun → 抛 `Agent is already processing. Wait for completion before continuing.`；
  - `lastMessage = messages[last]`；若 `!lastMessage || messages.every(m => m.role === "system")` →
    抛 `No messages to continue from`；
  - 若 `lastMessage.role === "assistant"`：
    - `queuedSteering = steeringQueue.drain()`；非空 → `runPromptMessages(queuedSteering, {skipInitialSteeringPoll:true})`；
    - 否则 `queuedFollowUps = followUpQueue.drain()`；非空 → `runPromptMessages(queuedFollowUps)`；
    - 否则抛 `Cannot continue from message role: assistant`；
  - 否则 `runContinuation()`。
- `runPromptMessages(messages, {skipInitialSteeringPoll?})` → `runWithLifecycle(signal => runAgentLoop(messages,
  createContextSnapshot(), createLoopConfig(opts), ev => processEvents(ev), signal, streamFunction))`。
- `runContinuation()` → 同上但 `runAgentLoopContinue`。
- `createContextSnapshot()` → `{ messages: state.messages.slice(), tools: state.tools.slice() }`。
- `createLoopConfig(options)` 返回的对象逐字段：
  `model: state.model`；`reasoning: thinkingLevel === "off" ? undefined : thinkingLevel`；`sessionId`；`onPayload`；
  `onResponse`；`onProviderStreamEvent`；`transport`；`thinkingBudgets`；`maxRetryDelayMs`；`identity`；
  `toolExecution`；`beforeToolCall`；`afterToolCall`；`finishTurn`；`prepareRequest`；
  `prepareNextTurn`：仅当 `prepareNextTurnWithContext || prepareNextTurn` 存在时提供闭包
  `async context => prepareNextTurnWithContext ? await prepareNextTurnWithContext(context, this.signal)
                                              : await prepareNextTurn?.(this.signal)`；
  `convertToLlm`；`transformContext`；`getApiKey`；
  `getSteeringMessages: async () => { if (skipInitialSteeringPoll) { skipInitialSteeringPoll = false; return [] }
                                      return steeringQueue.drain() }`；
  `getFollowUpMessages: async () => followUpQueue.drain()`。
- `runWithLifecycle(executor)`：
  - 若已有 activeRun → 抛 `Agent is already processing.`（防御性，正常路径不会到达）；
  - 新建 `AbortController`、`promise`（手动 resolve），`activeRun = {promise, resolve, abortController}`；
  - `isStreaming=true`、`streamingMessage=undefined`、`errorMessage=undefined`；
  - `try { await executor(signal) } catch (e) { await handleRunFailure(e, signal.aborted) } finally { finishRun() }`。
- `handleRunFailure(error, aborted)`：构造 assistant 消息
  `{role:"assistant", content:[{type:"text",text:""}], api: state.model.api, provider: state.model.provider,
    model: state.model.id, usage: EMPTY_USAGE, stopReason: aborted ? "aborted" : "error",
    errorMessage: msg(error), timestamp: Date.now()}`；
  依次 `processEvents`：`message_start`、`message_end`、`turn_end {message, toolResults: []}`、
  `agent_end {messages: [failureMessage]}`。
- `finishRun()`：`isStreaming=false`、`streamingMessage=undefined`、`pendingToolCalls=new Set()`；
  `activeRun?.resolve()`；`activeRun=undefined`。
- `processEvents(event)`（先改状态，再按订阅顺序 await 监听器）：
  - `message_start` → `streamingMessage = event.message`
  - `message_update` → `streamingMessage = event.message`
  - `message_end` → `streamingMessage = undefined`；`messages.push(event.message)`
  - `tool_execution_start` → 复制 Set 并 add `toolCallId`（新 Set 实例）
  - `tool_execution_end` → 复制 Set 并 delete
  - `turn_end` → 若 `message.role === "assistant" && message.errorMessage` → `errorMessage = message.errorMessage`
  - `agent_end` → `streamingMessage = undefined`
  - 然后取 `activeRun?.abortController.signal`，无则抛 `Agent listener invoked outside active run`；
    对每个监听器 `await listener(event, signal)`。

#### 2.1.6 `packages/agent/src/proxy.ts`

导出：`ProxyAssistantMessageEvent`、`ProxyStreamOptions`、`streamProxy`。
内部：`buildProxyRequestOptions`、`processProxyEvent`。

`ProxyAssistantMessageEvent`（服务端事件，partial 字段被剥离以省带宽）：

```ts
| { type: "start" }
| { type: "text_start"; contentIndex: number }
| { type: "text_delta"; contentIndex: number; delta: string }
| { type: "text_end"; contentIndex: number; contentSignature?: string }
| { type: "thinking_start"; contentIndex: number }
| { type: "thinking_delta"; contentIndex: number; delta: string }
| { type: "thinking_end"; contentIndex: number; contentSignature?: string }
| { type: "toolcall_start"; contentIndex: number; id: string; toolName: string }
| { type: "toolcall_delta"; contentIndex: number; delta: string }
| { type: "toolcall_end"; contentIndex: number; toolCall: ToolCall }
| { type: "done"; reason: "stop"|"length"|"toolUse"; usage: AssistantMessage["usage"]; providerThinkingLevel?: string }
| { type: "error"; reason: "aborted"|"error"; errorMessage?: string; usage: AssistantMessage["usage"]; providerThinkingLevel?: string }
```

`ProxyStreamOptions` = `Pick<SimpleStreamOptions, "temperature"|"samplingParams"|"maxTokens"|"reasoning"|
"cacheRetention"|"sessionId"|"headers"|"metadata"|"transport"|"thinkingBudgets"|"maxRetryDelayMs">`
\+ `{ signal?: AbortSignal; authToken: string; proxyUrl: string }`。

`buildProxyRequestOptions(options)` 返回上述 11 个字段的**全字段对象**（缺失值为 `undefined`；JSON 序列化时被丢弃）。

`streamProxy(model, context, options) → AssistantMessageEventStream`：

1. 创建流；异步 IIFE 中：
   - 初始 `partial: AssistantMessage = { role:"assistant", stopReason:"pending", content:[], api: model.api,
     provider: model.provider, model: model.id, usage: EMPTY_USAGE(全 0), timestamp: Date.now() }`。
   - `abortHandler = () => { if (reader) reader.cancel("Request aborted by user").catch(() => {}) }`；
     若 `options.signal` 存在则 `addEventListener("abort", abortHandler)`。
   - `fetch(`${options.proxyUrl}/api/stream`, { method:"POST", headers: { Authorization: `Bearer ${options.authToken}`,
     "Content-Type": "application/json" }, body: JSON.stringify({ model, context, options: buildProxyRequestOptions(options) }),
     signal: options.signal })`。
   - `!response.ok`：先 `errorMessage = `Proxy error: ${response.status} ${response.statusText}``；尝试
     `response.json()` 且 `data.error` 真值 → `errorMessage = `Proxy error: ${data.error}``；然后 `throw new Error(errorMessage)`。
   - `reader = response.body!.getReader()`；`decoder = new TextDecoder()`；`buffer = ""`；`sawTerminalEvent = false`；
     `toolArguments = Map<contentIndex, ToolCallArgumentsStream>`。
   - `processLine(line)`：不以 `"data: "` 开头 → 忽略；`data = line.slice(6).trim()`；空 → 忽略；
     `JSON.parse(data)` → `event = processProxyEvent(...)`；若 event 非空：`done`/`error` 置 `sawTerminalEvent`；
     `stream.push(event)`。
   - 循环：`{done, value} = await reader.read()`；done → break；`options.signal?.aborted` → throw
     `Request aborted by user`；`buffer += decoder.decode(value, {stream:true})`；`lines = buffer.split("\n")`；
     `buffer = lines.pop() || ""`；逐行 `processLine`。
   - 循环后 `signal.aborted` → throw `Request aborted by user`；`buffer += decoder.decode()`；`buffer` 非空 →
     `processLine(buffer)`（无换行结尾的终态事件，回归 #8996）。
   - `!sawTerminalEvent` → `partial.stopReason = "error"`；`partial.errorMessage =
     "Connection closed by proxy server before the response completed"`；push `{type:"error", reason:"error", error: partial}`。
   - `stream.end()`。
   - catch：`errorMessage = msg(error)`；`reason = options.signal?.aborted ? "aborted" : "error"`；
     `partial.stopReason = reason`；`partial.errorMessage = errorMessage`；push `{type:"error", reason, error: partial}`；
     `stream.end()`。
   - finally：移除 abort 监听。

`processProxyEvent(proxyEvent, partial, toolArguments)`（返回 `AssistantMessageEvent | undefined`）：

| 事件 | 行为 |
|---|---|
| `start` | `{type:"start", partial}` |
| `text_start` | `partial.content[i] = {type:"text", text:""}` → `{type:"text_start", contentIndex, partial}` |
| `text_delta` | content 必须是 text，否则 throw `Received text_delta for non-text content`；`content.text += delta`；`{type:"text_delta", contentIndex, delta, partial}` |
| `text_end` | 同上校验（`Received text_end for non-text content`）；`content.textSignature = contentSignature`；`{type:"text_end", contentIndex, content: content.text, partial}` |
| `thinking_start` | `partial.content[i] = {type:"thinking", thinking:""}` → `{type:"thinking_start", ...}` |
| `thinking_delta` | 非 thinking → `Received thinking_delta for non-thinking content`；追加；`{type:"thinking_delta", ...}` |
| `thinking_end` | 非 thinking → `Received thinking_end for non-thinking content`；设 `thinkingSignature`；`{type:"thinking_end", contentIndex, content: content.thinking, partial}` |
| `toolcall_start` | 设置 `{type:"toolCall", id, name: toolName, arguments:{}}`；`toolArguments.set(i, new ToolCallArgumentsStream())`；`{type:"toolcall_start", contentIndex, partial}` |
| `toolcall_delta` | 需 content 为 toolCall 且 args 流存在，否则 `Received toolcall_delta for non-toolCall content`；`partial.content[i] = {...content, arguments: args.append(delta)}`（**新对象**，便于响应式消费者）；`{type:"toolcall_delta", contentIndex, delta, partial}` |
| `toolcall_end` | content 非 toolCall → 返回 `undefined`；否则 `Object.assign(content, toolCall)`（补齐只在 end 出现的字段，如 `namespace`）、删除参数流、`{type:"toolcall_end", contentIndex, toolCall: content, partial}` |
| `done` | `partial.stopReason = reason`；`partial.usage = usage`；若有 `providerThinkingLevel` 则设置；`{type:"done", reason, message: partial}` |
| `error` | `partial.stopReason = reason`；`partial.errorMessage = errorMessage`；`partial.usage = usage`；可选的 `providerThinkingLevel`；`{type:"error", reason, error: partial}` |
| default | `console.warn(`Unhandled proxy event type: ${(proxyEvent as any).type}`)`；返回 `undefined` |

`ToolCallArgumentsStream` 语义（kiana-ai 提供，必须线性时间）：`append(delta)` 返回最新 partial arguments 对象
（变更时才换新对象）；参数文本永不挂在 content block 上（回归 #9265：520KB 参数、16,000 个 delta 必须 < 10s）。

#### 2.1.7 agent 测试工具与示例

这些不是公开 API，但测试与文档依赖其**逐字**行为；Rust 侧测试应提供等价夹具。

**`test/utils/calculate.ts`**

```ts
export interface CalculateResult extends AgentToolResult<undefined> {
  content: Array<{ type: "text"; text: string }>; details: undefined;
}
export function calculate(expression: string): CalculateResult {
  try {
    const result = new Function(`return ${expression}`)();
    return { content: [{ type: "text", text: `${expression} = ${result}` }], details: undefined };
  } catch (e: any) { throw new Error(e.message || String(e)); }
}
const calculateSchema = Type.Object({ expression: Type.String({ description: "The mathematical expression to evaluate" }) });
export const calculateTool: AgentTool<typeof calculateSchema, undefined> = {
  label: "Calculator", name: "calculate", description: "Evaluate mathematical expressions",
  parameters: calculateSchema,
  execute: async (_toolCallId, args) => calculate(args.expression),
};
export function createCalculateToolWithUsage(usage: Usage): AgentTool<...> {
  return { ...calculateTool, execute: async (_id, args) => ({ ...calculate(args.expression), usage }) };
}
```
要点：表达式由 JS 引擎求值（Rust 侧若不能求值，可让测试只用四则运算/常量，或用等价的小型求值器）；
文本格式为 `` `${expression} = ${result}` ``（`result` 用 JS `String()` 规则）；错误消息取 `e.message || String(e)`。

**`test/utils/get-current-time.ts`**

```ts
export async function getCurrentTime(timezone?: string): Promise<GetCurrentTimeResult> {
  const date = new Date();
  if (timezone) {
    try {
      const timeStr = date.toLocaleString("en-US", { timeZone: timezone, dateStyle: "full", timeStyle: "long" });
      return { content: [{ type: "text", text: timeStr }], details: { utcTimestamp: date.getTime() } };
    } catch (_e) {
      throw new Error(`Invalid timezone: ${timezone}. Current UTC time: ${date.toISOString()}`);
    }
  }
  const timeStr = date.toLocaleString("en-US", { dateStyle: "full", timeStyle: "long" });
  return { content: [{ type: "text", text: timeStr }], details: { utcTimestamp: date.getTime() } };
}
export const getCurrentTimeTool = {
  label: "Current Time", name: "get_current_time", description: "Get the current date and time",
  parameters: Type.Object({ timezone: Type.Optional(Type.String({
    description: "Optional timezone (e.g., 'America/New_York', 'Europe/London')" })) }),
  execute: async (_id, args) => getCurrentTime(args.timezone),
};
```
`toLocaleString("en-US", {dateStyle:"full", timeStyle:"long"})` 的输出形如
`Sunday, October 26, 2026 at 10:00:00 AM GMT+8`（**逐字格式由 ICU 决定**；Rust 侧需用 ICU4X/chrono 复刻，
见 §6）。非法时区错误文本：`Invalid timezone: ${timezone}. Current UTC time: ${date.toISOString()}`
（`toISOString()` 形如 `2026-10-26T02:00:00.000Z`）。

**`test/utils/wait-for-tick.ts`**：`waitForTick() = new Promise(resolve => setTimeout(resolve, 0))`。

**`examples/mcp-codemode/tools.ts`**（被示例与文档引用，含逐字工具描述）：

- `toToolName(prefix, name)` = `` `${prefix}_${name}` `` 把 `[^A-Za-z0-9_-]` 换成 `_` 并 `slice(0, 64)`
  （注释：provider 工具名限 64 个 `[A-Za-z0-9_-]` 字符）。
- `textOf(result)`：`result.content.filter(block => block.type === "text").map(block => block.text).join("\n")`。
- `createMcpTools(client, prefix)`：`await client.listTools()` 后逐个映射成 `AgentTool`：
  - `name = toToolName(prefix, tool.name)`；`label = tool.title ?? tool.name`；
    `description = tool.description ?? tool.title ?? `MCP tool ${tool.name}``；
  - `parameters = Type.Unsafe({...tool.inputSchema, type: "object", properties: tool.inputSchema.properties ?? {}})`
    （注释：provider 要求 object schema，有些拒绝没有 `properties` 的）；
  - `outputSchema = tool.outputSchema ? Type.Unsafe(tool.outputSchema) : undefined`；
  - `execute(_toolCallId, params, signal, onUpdate)`：`client.callTool(tool.name, params, {signal,
    onProgress: progress => { const text = progress.message ?? `Progress ${progress.progress}`;
    onUpdate?.({content:[{type:"text", text}], details: undefined}) }})`；
    `content = toLlmContent(result)`；`result.isError && content.length === 0` → push
    `{type:"text", text:`MCP tool ${tool.name} failed`}`；返回
    `{content, details: undefined, structuredContent: result.structuredContent as JsonValue | undefined,
    isError: result.isError === true}`（注释：MCP 把失败放在 result 里而非协议错误）。
- `createNestedToolRunner(agent)`：`nextId` 从 1 起；每次调用：
  `assistantMessage = agent.state.messages.findLast(m => m.role === "assistant")`；若
  `assistantMessage?.role !== "assistant"` → `throw new Error("Nested tool calls need an assistant message")`；
  `tools = agent.state.tools`；`runToolCall({type:"toolCall", id:`nested-${nextId++}`, name,
  arguments: args ?? {}}, {tools, assistantMessage, context:{messages: agent.state.messages, tools},
  beforeToolCall: agent.beforeToolCall, afterToolCall: agent.afterToolCall, signal})`。
- `CODEMODE_TOOL_NAME = "codemode"`；`codemodeParameters = Type.Object({code: Type.String({
  description: "JavaScript source. Top-level await and return work." })})`。
- `createCodemodeTool(tools, runTool)`：过滤掉名为 `codemode` 的工具；`sandboxTools` 的 `inputSchema` =
  `tool.parameters`，`outputSchema` = `tool.outputSchema ?? {type: "string"}`；`execute`：`runTool` 结果若有
  `outputSchema` 且 `structuredContent !== undefined` 则返回它；否则取文本，`isError` 时
  `throw new Error(text || `Tool ${tool.name} failed`)`。工具 `description` 见 §3.7。
  `label: "Codemode"`；execute 新建 `new CodemodeSandbox({tools: sandboxTools, timeoutMs: 120_000})`，
  `content = [...result.output]`；成功且 `value !== undefined` → push `{type:"text", text: typeof value === "string" ?
  value : JSON.stringify(value)}`；失败 → push `{type:"text", text: `Script failed: ${result.error.stack ??
  result.error.message}`}`；返回 `{content, details:{calls: result.calls}, isError: !result.ok}`；
  `finally { await sandbox.close() }`。
- **`examples/mcp-codemode/main.ts`**（本单元唯一的 CLI 表面，属于示例而非库）：
  - 文件头注释给出用法示例：
    `ANTHROPIC_API_KEY=... node packages/agent/examples/mcp-codemode/main.ts npx -y @modelcontextprotocol/server-filesystem .`。
  - `const [command, ...args] = process.argv.slice(2)`；无 `command` →
    `console.error("Usage: main.ts <mcp server command> [args...]")` 并 `process.exit(1)`。
  - `createModels()` + `models.setProvider(anthropicProvider())`；
    `model = models.getModel("anthropic", "claude-sonnet-4-6")`；缺失 → `throw new Error("Model not found")`。
  - `new McpClient({name: "kiana-agent-example", version: "1.0.0"})` + `new StdioTransport({command, args})`。
  - `Agent({initialState:{systemPrompt:"You are a helpful assistant.", model}, streamFn: models.streamSimple.bind(models)})`。
  - `mcpTools = await createMcpTools(client, "mcp")`；
    `agent.state.tools = [...mcpTools, createCodemodeTool(mcpTools, createNestedToolRunner(agent))]`。
  - `subscribe`：`message_update` 且 `assistantMessageEvent.type === "text_delta"` →
    `process.stdout.write(delta)`；`tool_execution_start` → `console.log(`\n[${event.toolName}]`)`。
  - `await agent.prompt("Use codemode to list what the MCP tools can do, then summarize it in two sentences.")`；
    `console.log()`；`finally { await client.close() }`。
  - 请求头/环境变量来自 kiana-ai 的 provider（本单元不定义）。

#### 2.1.8 `packages/agent/README.md`（569 行）与 metadata

README 是公开文档表面的一部分，Rust 侧文档必须保留同样的章节与声明。章节：

`Installation`、`Quick Start`、`Core Concepts`（`AgentMessage vs LLM Message`、`Message Flow`）、
`Event Flow`（`prompt() Event Sequence`、`With Tool Calls`、`Request preparation and turn finalization`、
`continue() and queued input`、`Event Types`）、`Agent Options`、`Agent State`、`Methods`（`Prompting`、
`State Management`、`Session and Thinking Budgets`、`Control`、`Events`）、`Steering and Follow-up`、
`Custom Message Types`、`Tools`（`Error Handling`、`MCP and Codemode`）、`Proxy Usage`、`Low-Level API`、`License`。

其中逐字断言的不变量（已并入 §1.4/§2.1.x，不重复实现细节）：

- `AgentMessage` 允许自定义 role；`convertToLlm` 是唯一到 LLM 消息的桥。
- 事件表（与 §2.1.2 的 `AgentEvent` 一一对应）：`agent_start`、`agent_end`、`turn_start`、`turn_end`、
  `message_start`、`message_update`（**仅 assistant**，带 `assistantMessageEvent` 与 `message` 部分副本）、
  `message_end`、`tool_execution_start`、`tool_execution_update`、`tool_execution_end`。
- 并行模式：preflight 顺序、执行并发、`tool_execution_end` 按完成顺序、toolResult 与 `turn_end.toolResults` 按源序。
- `beforeToolCall` 在 `tool_execution_start` 与参数校验之后；`afterToolCall` 在 `tool_execution_end` 与最终
  toolResult 消息之前。两者都可 `terminate`。
- `prepareRequest` 在每个 provider 请求前运行（含第一个），且**不**轮询队列；`finishTurn` 在 assistant + 全部
  toolResult 之后、`turn_end` 之前运行，错误/中止也运行但不接受其决策。
- `agent.state.systemPrompt` 只读并重放 transcript；`state.tools` 是可执行集合，每次请求前与 transcript 声明差异
  通过 system 消息告知；`state.messages`/`state.tools` 赋值复制顶层数组。
- `Agent.subscribe()` 按注册顺序 await；`agent_end` 之后 `waitForIdle()`/`prompt()` 仍等 listener。
- 工具失败应 **throw**（而不是返回错误内容），agent 捕获后以 `isError: true` 报告给模型。
- `agentLoop()`/`agentLoopContinue()` 的流是观察性的：保持事件顺序，但不会等待消费者处理完再进入下一阶段；
  需要屏障时用 `Agent` 类。
- Proxy 用法：`streamFn: (model, context, options) => streamProxy(model, context, {...options, authToken, proxyUrl})`。

`packages/agent/package.json`：`name "@earendil-works/kiana-agent-core"`、`version 1.4.0`、`type module`、
`main dist/index.js`、`types dist/index.d.ts`、`exports "."` 与 `"./package.json"`、`files ["dist","README.md"]`、
`engines.node ">=22.19.0"`、依赖 `@earendil-works/kiana-ai 1.4.0` + `typebox 1.3.27`。
`agent/vitest.config.ts`：源码别名（`kiana-telemetry`/`kiana-agent-core`/`kiana-ai`/`kiana-ai/compat` 指向源码）+
`globals: true`、`environment: "node"`、`testTimeout: 30000`、`reporters` 在 `GITHUB_ACTIONS` 下用
`["dot","github-actions"]`、`silent: "passed-only"`、`resolve.conditions: ["source"]`。
`agent/tsconfig.build.json`：extends 根 `tsconfig.base.json`，`outDir ./dist`、`rootDir ./src`，
`paths` 指向 `../ai/dist/index.d.ts` 与 `../ai/dist/*.d.ts`/`../ai/dist/providers/*.d.ts`，
`include ["src/**/*.ts"]`，`exclude ["node_modules","dist","**/*.d.ts","src/**/*.d.ts"]`。
`agent/CHANGELOG.md`：1.4.0 加入 `identity`；1.1.0 加入 `durationMs`；Unreleased 修复 `streamProxy()` 每个 delta
重复解析累积参数并保留原始文本（#9265）。
`codemode/tsconfig.build.json` 另把 `src/cloudflare-modules.d.ts` 放入 `files`（`exclude` 对 `files` 不生效）；
`codemode/vitest.config.ts` 仅 `globals/environment/reporters/conditions`（无 testTimeout）。

### 2.2 codemode 包

#### 2.2.1 `packages/codemode/src/types.ts`

纯类型（254 行），无运行时代码。逐项：

- `CodemodeToolContext { signal: AbortSignal }`：在脚本结束（含未 await 的调用）、执行超时、调用方 abort、
  sandbox 关闭时被 abort。
- `CodemodeJsonSchema = { [key: string]: unknown } | boolean`：仅用于渲染声明，**不校验值**。
- `CodemodeTool`：
  - `name: string`：脚本以 `tools.<id>(args)`（`id` = 非法标识符字符换成 `_`，见 `toCodemodeIdentifier`）以及
    `tools["<name>"](args)` 调用。globals 以 `<name>(args)` 调用，名字必须是标识符或 `<namespace>.<member>`（分组进冻结的命名空间对象）。
  - `description?: string`：作为 `renderDeclarations` 的 doc comment，并出现在 `ALL_TOOLS`（tools）。
  - `inputSchema?`：单参数 schema，渲染为参数类型；缺省 `unknown`。
  - `outputSchema?`：解析值的 schema，渲染为 promise 类型；缺省 `unknown`。
  - `spread?: boolean`：仅 globals：`execute` 收到全部调用参数的数组而非第一个。
  - `signature?: string`：仅 globals：`renderDeclarations` 用的 TS 参数表与返回类型，例如
    `"(type: string, id?: string): Promise<Model[]>"`；替代由 schema 渲染。
  - `execute(args: unknown, context: CodemodeToolContext): Promise<unknown> | unknown`：`args` 是脚本传入值经过
    JSON 往返后的结果；返回值必须 JSON 可序列化；抛错在脚本中表现为同 message 的 `Error`。
- `CodemodeOutputItem = { type:"text"; text: string; console?: true } | { type:"image"; data: string; mimeType: string }`
  （`data` 为 base64；顺序即脚本产生顺序）。
- `CodemodeCallStatus = "ok" | "error" | "cancelled"`。
- `CodemodeCall { name: string; status: CodemodeCallStatus; durationMs: number }`。
- `CodemodeErrorKind = "script" | "timeout" | "aborted" | "sandbox"`（注释语义见 §1.4 / §3）。
- `CodemodeError { kind: CodemodeErrorKind; name?: string; message: string; stack?: string }`。
- `CodemodeStoreWrites { set: Record<string, unknown>; delete: string[] }`（只有成功执行才报告写集合；
  `delete` 是"以 `undefined` 存储"的 key）。
- `CodemodeResult`：
  - 成功：`{ ok: true; value: unknown; output: CodemodeOutputItem[]; calls: CodemodeCall[];
    storeWrites: CodemodeStoreWrites; during?: CodemodeDuring }`
  - 失败：`{ ok: false; error: CodemodeError; output: CodemodeOutputItem[]; calls: CodemodeCall[];
    during?: CodemodeDuring }`
  - `output` 对失败执行也保留（到失败点为止）；`exit()` 完成时 `value: undefined`。
- `CodemodeSandboxBaseOptions { tools?: CodemodeTool[]; globals?: CodemodeTool[]; timeoutMs?: number;
  memoryLimitBytes?: number }`：
  - `timeoutMs` 默认 **300000**，包含工具耗时；`Infinity` 关闭 deadline。
  - `memoryLimitBytes`：超限在脚本内表现为 `InternalError: out of memory`；默认仅受 wasm32 4 GiB 地址空间限制。
  - globals 名字不得遮蔽内建：`tools`、`ALL_TOOLS`、`console`、`text`、`image`、`exit`、`store`、`load`
    （注意 `RESERVED_GLOBALS` 里还有 `globalThis`）。
- `CodemodeSandboxOptions extends CodemodeSandboxBaseOptions { wasm?; workerUrl?; acquireWorker?; onWorkerStarted? }`：
  - `wasm?: CodemodeWasmModule | Promise<...>`，默认 `loadQuickJSWasm()`。
  - `workerUrl?: string | URL`；默认是本包模块旁的 `worker.ts`（源码运行）或 `worker.js`（dist）。
  - `acquireWorker?(): () => void`：加载 wasm 之后、创建每个 worker 之前**同步**调用；抛错 = 拒绝准入、不启动 worker；
    返回 release 回调，在 worker 构造失败或 worker 真正退出时调用一次（不在结果结算时）；release 不得抛错。
  - `onWorkerStarted?(exited: Promise<void>)`：每个由 `execute()`/`start()` 启动的 worker 调用一次；`exited` 在
    worker 真正退出时 resolve（可能晚于结果与 `close()`；Bun 上不可中断的内建会延迟 terminate）。
- `CodemodeHostCall { id: number; target: "tool" | "global"; name: string; args: unknown }`：
  `id` 是 VM 自己的调用编号，按脚本发出顺序 1,2,…；`args` 为 JSON 往返后的值，spread globals 是参数数组。
- `CodemodeDeclaredTool { name; jsName?; description? }`：`jsName` 默认 `toCodemodeIdentifier(name)`；重放时必须沿用
  run 启动时的映射，避免改名。
- `CodemodeDeclaredGlobal { name; spread? }`。
- `CodemodeOutcome = { ok: true; value: unknown } | { ok: false; message: string; properties?: Record<string, unknown> }`：
  失败时在脚本中变成 `Error`，并带上 `properties` 的自有属性（JSON 值；`name`/`message`/`stack` 被忽略）。
- `CodemodeDuring { seq: number; issued: number }`：脚本结束时 VM 正在处理的结果号（首次运行前为 0）与到那时为止发出的调用数。
- `CodemodeRunOptions { startedAt; seed; tools?; globals?; store?; signal?; timeoutMs?; onStarted?; onCall; onIdle? }`：
  - `startedAt`：VM 时钟起点（ms since epoch），在第一个结果到达前等于 `Date.now()`。
  - `seed`：32 位无符号整数，决定 `Math.random()` 与其它随机。
  - `timeoutMs` 默认 **无**（不是 sandbox 的默认）。
  - `onStarted(fingerprint)`：在脚本运行前、任何调用之前。
  - `onCall(call)`：脚本发起调用；用 `CodemodeRun.deliver()` 立即或稍后回答。
  - `onIdle(seq, issued)`：VM 处理完结果 `seq`（0 = 脚本首次运行）后再次等待，累计发出 `issued` 次调用。
- `CodemodeRun { readonly result: Promise<CodemodeResult>; readonly finished: boolean;
  deliver(seq, now, id, outcome): void }`：
  - `finished` 在结果决定时（脚本结束/超时/失败/abort）置位，早于 `result` settle；此后不能再回答调用；不表示 VM 已退出。
  - `deliver` 的 `seq` 从 1 连续无缺口；VM 严格按序应用；`now` 是该结果处理期间 VM 时钟读数（ms since epoch），
    更早则沿用前值。对不在等待的调用、已用过的 `seq`、非有限 `now` 抛错。
- `CodemodeExecuteOptions { signal?; timeoutMs?; store? }`：`store` 是 `load()` 的初值快照，必须 JSON 可序列化；
  `store()` 的写回由调用方持久化。

#### 2.2.2 `packages/codemode/src/source.ts`

```ts
export const CODEMODE_OPTIONS_PREFIX = "// @options:";
const SUPPORTED_FIELDS = ["max_output_tokens", "timeout_ms"] as const;   // 文本：`max_output_tokens` and `timeout_ms`
const MAX_TIMEOUT_MS = 2_147_483_647;
export const CODEMODE_SOURCE_GRAMMAR = String.raw`
start: options_source | plain_source
options_source: OPTIONS_LINE NEWLINE SOURCE
plain_source: SOURCE

OPTIONS_LINE: /[ \t]*\/\/ @options:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
`;
export interface CodemodeSourceOptions { maxOutputTokens?: number; timeoutMs?: number }
export interface ParsedCodemodeSource { code: string; options: CodemodeSourceOptions }
export class CodemodeSourceError extends Error { constructor(message) { super(message); this.name = "CodemodeSourceError" } }
export function parseCodemodeSource(input: string): ParsedCodemodeSource
```

`parseCodemodeSource` 精确算法：

1. `input.trim() === ""` → 抛
   `Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line \`// @options: {"max_output_tokens": 1000}\`.`
2. `lines = input.split("\n")`；`misplaced = lines.findIndex((line, index) => index > 0 && line.trimStart().startsWith(CODEMODE_OPTIONS_PREFIX))`；
   找到 → 抛
   ``The ${CODEMODE_OPTIONS_PREFIX} line must be the first line of the input; found one on line ${misplaced + 1}``。
   注意：这是**纯文本**检查，不做词法分析——从第 2 行起只要 `trimStart()` 后以前缀开头就报错，
   即使该行位于模板字面量或多行字符串内部（例如 `` const s = `\n// @options: x` `` 的第二行）也会报错。
3. `newline = input.indexOf("\n")`；`firstLine = lines[0].replace(/\r$/, "")`（只去掉行尾一个 `\r`）；
   `trimmed = firstLine.trimStart()`。
4. 不以 `CODEMODE_OPTIONS_PREFIX` 开头 → `{ code: input, options: {} }`（**原样**，含 BOM/空格/CRLF）。
5. `code = newline === -1 ? "" : input.slice(newline)`（从换行符开始，保留该换行 → 行号不变）。
6. `code.trim() === ""` → 抛 `The @options line must be followed by JavaScript source on subsequent lines`。
7. `parseOptions(trimmed.slice(CODEMODE_OPTIONS_PREFIX.length).trim())`：
   - 指令为空串 → `@options must be a JSON object with supported fields \`max_output_tokens\` and \`timeout_ms\``。
   - `JSON.parse` 失败 → ``@options must be valid JSON with supported fields `max_output_tokens` and `timeout_ms`: ${message}``
     （`message` 为 JS `JSON.parse` 的原文，如 `Unexpected token 't', ...`；Rust 需给出等价消息，见 §6）。
   - 非对象 / null / 数组 → 与空指令相同的消息。
   - `Object.keys` 逐 key 检查，未知键 → ``@options only supports `max_output_tokens` and `timeout_ms`; got `${key}``。
   - `max_output_tokens`：必须 `typeof number && Number.isSafeInteger(value) && value >= 0`，否则
     ``@options field `max_output_tokens` must be a non-negative safe integer``；通过则 `options.maxOutputTokens = value`。
   - `timeout_ms`：必须安全整数、`!== 0`、`<= MAX_TIMEOUT_MS`，否则
     ``@options field `timeout_ms` must be a positive integer up to ${MAX_TIMEOUT_MS}``；通过则 `options.timeoutMs = value`。
   - 键的枚举顺序 = JSON 文本中的插入顺序；错误只报**第一个**未知键（遇到即抛，后续键不再检查）。
   - 未知键检查在字段值检查之前；`max_output_tokens` 在 `timeout_ms` 之前检查（顺序由解构后的 if 顺序决定，
     与 JSON 键序无关）。
   - `JSON.parse("null")` → 非对象 → 报对象错误；`JSON.parse("[]")` → 数组 → 同样。
   - `max_output_tokens: 0` 合法；`timeout_ms: 0` 非法；`timeout_ms` 上限为 `2_147_483_647`（`setTimeout` 上限）。

#### 2.2.3 `packages/codemode/src/identifier.ts`

```ts
export function toCodemodeIdentifier(name: string): string {
  let identifier = "";
  for (const char of name) {            // 按 Unicode 码点迭代（for..of）
    const valid = identifier === "" ? /^[A-Za-z_$]$/.test(char) : /^[A-Za-z0-9_$]$/.test(char);
    identifier += valid ? char : "_";
  }
  return identifier === "" ? "_" : identifier;
}
```
- 只接受 ASCII `A-Za-z0-9_$`；首字符不能是数字；其他（含非 ASCII、代理对）逐码点变成 `_`。
- 空字符串 → `"_"`。
- `"mcp__docs__search"` 原样；`"my-tool"` → `"my_tool"`。

#### 2.2.4 `packages/codemode/src/declarations.ts`（340 行）

导出：`toCodemodeIdentifier`（重导出）、`DEFAULT_INPUT_SCHEMA_MAX_CHARS = 16_000`、`RenderDeclarationsOptions`、
`renderDeclarations`、`renderToolSignature`、`renderToolSample`、`renderToolOutputType`、`schemaToType`。

常量：
- `IDENTIFIER = /^[A-Za-z_$][A-Za-z0-9_$]*$/`；`INDENT = "  "`。
- `MAX_REF_EXPANSIONS = 32`。
- `TAG_KEYWORDS = ["minimum","exclusiveMinimum","maximum","exclusiveMaximum","multipleOf","minLength","maxLength",
  "pattern","format","minItems","maxItems"]`。
- `LOWER_BOUNDS = {minimum, exclusiveMinimum, minLength, minItems}`；`UPPER_BOUNDS = {maximum, exclusiveMaximum,
  maxLength, maxItems}`。
- `MAX_TAG_CHARS = 80`；`LINE_TERMINATORS = /[\r\n\u2028\u2029]/g`（源码里是 `\u2028`/`\u2029` 转义，即真正的 U+2028/U+2029）。

`renderDeclarations({tools?, globals?}) → string`：
1. `sections: string[]`。
2. tools 非空 → 每个成员 = `docComment(description, "  ") + "  " + renderToolSignature(tool)`，用 `"\n"` 连接，包成
   `declare const tools: {\n<members>\n};`，push。
3. globals 按顺序：
   - 无 `.` → push `` `declare function ${name}` + renderGlobal 后缀 ``（见下）。
   - 有 `.`（取**第一个** `.`）：namespace = 前缀，member = 其后部分；累积到 `namespaces` Map（保持首次出现顺序）。
4. 每个 namespace push `declare const ${namespace}: {\n<members>\n};`（members 用 `"\n"` 连接，每个成员已含缩进）。
5. 返回 `sections.join("\n\n")`。

`renderGlobal(head, global, indent)`：
- `global.signature !== undefined` → `docComment(description, indent) + indent + head + signature + ";"`
- 否则 → `docComment(...) + indent + head + "(args: " + (inputSchema ? schemaToType(inputSchema) : "unknown")
  + "): Promise<" + (outputSchema ? schemaToType(outputSchema) : "unknown") + ">;"`

`renderToolSignature(tool, {inputMaxChars?})`：
```
const input = tool.inputSchema === undefined ? "unknown"
            : schemaToType(tool.inputSchema, { maxChars: inputMaxChars ?? 16000 });
`${toCodemodeIdentifier(tool.name)}(args: ${input}): Promise<${renderToolOutputType(tool.outputSchema)}>;`
```

`renderToolSample(tool, {inputMaxChars?})`：
```
const declaration = `declare const tools: { ${renderToolSignature(tool, options)} };`;
return `${tool.description?.trim() ?? ""}\n\ncodemode tool declaration:\n\`\`\`ts\n${declaration}\n\`\`\``;
```

`renderToolOutputType(schema)`：`schema === undefined ? "unknown" : schemaToType(schema)`。

`docComment(description, indent)`：
- `text = description?.trim()`；空 → `""`。
- `lines = text.replaceAll("*/", "*\\/").split(/\r?\n/)`。
- 单行 → `` `${indent}/** ${lines[0]} */\n` ``。
- 多行 → `` `${indent}/**\n` + lines.map(line => `${indent} *${line ? ` ${line}` : ""}`).join("\n") + `\n${indent} */\n` ``。

`propertyKey(name)`：`IDENTIFIER.test(name) ? name : JSON.stringify(name)`。

`union(types)`：去重（保持首次出现顺序）；含 `"unknown"` → `"unknown"`；空 → `"never"`；否则 `" | "` 连接。

`schemaToType(schema, {maxChars?})`：
```
const render = tags => toType(schema, { root: schema, resolving: new Set(), expansions: 0, tags });
const type = render(true);
if (maxChars === undefined || type.length <= maxChars) return type;
const untagged = render(false);
return untagged.length <= maxChars ? untagged : "unknown";
```
（长度按 UTF-16 单元计。）

`resolveRef(ref, root)`：仅 `"#"` 或 `"#/"` 开头；`root` 开始按 `ref.slice(2).split("/").filter(Boolean)` 逐段下钻，
每段 `decodeURIComponent(segment).replaceAll("~1","/").replaceAll("~0","~")`；中途非普通对象或键不存在 → `undefined`；
终点为 boolean → 原样返回；为普通对象 → 返回；否则 `undefined`。

`toType(schema, ctx)` 顺序（**分支顺序即行为**）：
1. `true` → `"unknown"`；`false` → `"never"`。
2. 非对象 / null / 数组 → `"unknown"`。
3. `typeof schema.$ref === "string"`：循环（`resolving` 含该 ref）或 `expansions >= 32` → `"unknown"`；
   `resolveRef` 未命中 → `"unknown"`；否则 `expansions++`、`resolving.add(ref)`、递归、`finally` 移除。
4. `"const" in schema` → `JSON.stringify(schema.const) ?? "unknown"`。
5. `Array.isArray(schema.enum)` → `union(enum.map(v => JSON.stringify(v) ?? "unknown"))`。
6. `anyOf`（数组）优先，其次 `oneOf`（数组）→ `union(变体 toType)`。
7. `allOf`（数组）→ 各分支 toType，过滤掉 `"unknown"`；空 → `"unknown"`；否则用 `" & "` 连接，含 `" | "` 的分支加括号
   `(x)`。
8. `type` 为数组 → `union(type.map(t => toType({...schema, type: t}, ctx)))`。
9. `switch (type)`：`string`→`"string"`；`number`/`integer`→`"number"`；`boolean`→`"boolean"`；`null`→`"null"`；
   `array`→`arrayType`；`object`→`objectType`；`undefined`：若含 `properties`/`additionalProperties`/`required`
   之一 → `objectType`，否则若含 `items`/`prefixItems` → `arrayType`，否则 `"unknown"`；其他（含 `$ref` 已处理、
   `const`/`enum`/`anyOf` 等已处理）→ `"unknown"`。
10. 其他未知 type 值 → `"unknown"`。

`arrayType(schema, ctx)`：
- `items !== undefined && !Array.isArray(items)` → `Array<${toType(items)}>`。
- `tuple = Array.isArray(prefixItems) ? prefixItems : Array.isArray(items) ? items : []`；非空 →
  `[${tuple.map(toType).join(", ")}]`；否则 `"unknown[]"`。

`objectType(schema, ctx)`：
1. `properties`（普通对象，否则 `{}`）；`required = new Set(Array.isArray(schema.required) ? schema.required : [])`。
2. `names = Object.keys(properties).sort()`（**字典序，按 UTF-16 码元**）。
3. 成员 `members[i] = propertyKey(name) + (required.has(name) ? "" : "?") + ": " + toType(properties[name], ctx) + ";"`。
4. `additional = schema.additionalProperties`：
   - `!== undefined && !== false`：`type = additional === true ? "unknown" : toType(additional)`；追加
     `[key: string]: ${type};`。
   - `=== undefined && names.length === 0`：追加 `[key: string]: unknown;`。
5. `members.length === 0` → `"{}"`。
6. `comments[i]`：`descriptionOf(properties[name]).split(LINE_TERMINATORS).map(trim).filter(Boolean)`；
   若 `ctx.tags` 且 `constraintTags(...)` 非空则追加该 tag 行。
7. 没有任何注释 → `{ ${members.join(" ")} }`（单行，注意 `"{ "` 与 `" }"` 的空格）。
8. 否则多行：
   - `"{"`；
   - 逐属性：每个 comment 行 → `"  // " + line`；然后 `"  " + members[i].replaceAll("\n", "\n  ")`；
   - 额外的（additional）成员 → `"  " + member`；
   - `"}"`；用 `"\n"` 连接。

`descriptionOf(property)`：`property?.description` 为 string → `trim()`，否则 `""`。

`constraintTags(schema)`（**输出必须逐字**）：
1. 非普通对象 → `""`。
2. `variants = Array.isArray(property.anyOf) ? property.anyOf : property.oneOf`；`nonNull` = 变体里
   `variant?.type !== "null"` 的那些；`branch = nonNull.length === 1 ? nonNull[0] : undefined`；
   `branches = (branch 是普通对象) ? [property, branch] : [property]`。
3. 每个 branch 复制并转换 draft-04/OpenAPI3.0 布尔排他边界：
   `exclusiveMinimum === true` → `{minimum: undefined, exclusiveMinimum: schema.minimum}`；
   `exclusiveMaximum === true` → `{maximum: undefined, exclusiveMaximum: schema.maximum}`（并入原 schema，注意键顺序
   对输出无影响，因为按 TAG_KEYWORDS 顺序输出）。
4. `@integer`：任一 branch 的 `type === "integer"` 或 `type` 数组含 `"integer"`。
5. 按 `TAG_KEYWORDS` 顺序：
   - 下界类（`minimum`,`exclusiveMinimum`,`minLength`,`minItems`）：取各 branch 的数值，非空 → `@keyword max(values)`。
   - 上界类：`@keyword min(values)`。
   - 其他：对 `new Set(values)` 逐个，若为 number 或 string → `@keyword value`（`String(value)`；number 用 JS 数字
     格式化规则）。
6. `uniqueItems === true`（任一 branch）→ `@uniqueItems`。
7. 注解只看 property 本身：`default !== undefined && default !== null` → `@default ${JSON.stringify(default)}`；
   `deprecated === true` → `@deprecated`。
8. `tags.filter(t => t.length <= 80).map(t => t.replace(LINE_TERMINATORS, c => "\\u" + hex4(c.charCodeAt(0))))`
   （小写 4 位十六进制）。`join(" ")`。

已知逐字样例（测试锁定）：
- `{type:"object",properties:{timeoutMs:{type:"integer",minimum:0,maximum:30000,default:0,description:"Wait in milliseconds."},id:{type:"string",pattern:"^[a-z]+$",maxLength:8},tags:{type:"array",items:{type:"string"},maxItems:5,uniqueItems:true,deprecated:true},name:{type:"string"}},required:["id"]}`
  →
  ```
  {
    // @maxLength 8 @pattern ^[a-z]+$
    id: string;
    name?: string;
    // @maxItems 5 @uniqueItems @deprecated
    tags?: Array<string>;
    // Wait in milliseconds.
    // @integer @minimum 0 @maximum 30000 @default 0
    timeoutMs?: number;
  }
  ```
- `{type:"object",properties:{limit:{anyOf:[{type:"integer",minimum:1,maximum:100},{type:"null"}],default:null,title:"Limit"}}}`
  → `{\n  // @integer @minimum 1 @maximum 100\n  limit?: number | null;\n}`
- `{type:"object",properties:{n:{anyOf:[{type:"integer",minimum:10,maximum:50,format:"int32"},{type:"null"}],minimum:0,maximum:20,format:"port"}}}`
  → `{\n  // @integer @minimum 10 @maximum 20 @format port @format int32\n  n?: number | null;\n}`
  （`branches = [property, branch]`：先属性本身、再非 null 分支；`@format` 两个值都输出，`Set` 去重后按该顺序）
- draft-04：`n: {type:"integer", minimum:0, exclusiveMinimum:true, maximum:10, exclusiveMaximum:false}` →
  `// @integer @exclusiveMinimum 0 @maximum 10`；`m: {anyOf:[{type:"number",maximum:5,exclusiveMaximum:true},{type:"null"}],maximum:8}`
  → `// @maximum 8 @exclusiveMaximum 5`。
- `sep: {type:"string", pattern:"^[\\r\\n\\u2028\\u2029]$", default:"\\u2028", description:"a\\u2028b"}` →
  ```
  {
    // a
    // b
    // @pattern ^[\\u000d\\u000a\\u2028\\u2029]$ @default "\\u2028"
    sep?: string;
  }
  ```
  注意：`pattern` 值里的真实换行（U+000D/U+000A/U+2028/U+2029）被 `LINE_TERMINATORS` 逐个转成 `\u000d` 等 6 字符转义；
  `@default` 的 `JSON.stringify("\u2028")` 在 JS 里仍输出 U+2028 字符本身，随后 tag 级转义把它变成 `\u2028` 6 个字符。
- 长度预算：`schemaToType({type:"object",properties:{n:{type:"integer",minimum:1}}}, {maxChars:20})`
  → `"{ n?: number; }"`（**先去掉 tag**）；`maxChars:10` → `"unknown"`。

#### 2.2.5 `packages/codemode/src/wasm.ts`

```ts
export type CodemodeWasmModule = object;      // 不透明（WebAssembly.Module）
const modules = new Map<string, Promise<CodemodeWasmModule>>();
export function loadQuickJSWasm(path?: string): Promise<CodemodeWasmModule>
```
- `resolved = path ?? createRequire(import.meta.url).resolve("quickjs-wasi/quickjs.wasm")`。
- 每个路径只读+编译一次并缓存 Promise；失败时 `modules.delete(resolved)` 后重新抛出（下次重试）。
- 编译用 `WebAssembly.compile(bytes)`（`readFile` 读字节）。

#### 2.2.6 `packages/codemode/src/runtime/protocol.ts`

```ts
export interface VmStartData {
  code: string;
  tools: { name: string; jsName: string; description: string }[];
  globals: { name: string; spread: boolean }[];
  memoryLimitBytes: number | undefined;
  store: Record<string, string>;                 // key -> JSON 文本
  deterministic?: { startedAt: number; seed: number };
}
export interface WorkerData extends VmStartData { wasm: CodemodeWasmModule; interrupt: SharedArrayBuffer }
export type ScriptErrorJson = string;            // JSON: { name?, message, stack? }
export type WorkerToHostMessage =
  | { type: "call"; id: number; target: "tool" | "global"; name: string; args: string | undefined }
  | { type: "output"; item: CodemodeOutputItem }
  | { type: "done"; ok: true; value: string | undefined; writes: string; during?: CodemodeDuring }
  | { type: "done"; ok: false; error: ScriptErrorJson; during?: CodemodeDuring }
  | { type: "crash"; message: string }
  | { type: "started"; fingerprint: string }
  | { type: "idle"; seq: number; issued: number };
export type HostToWorkerMessage = {
  type: "result"; id: number; ok: boolean; payload: string | undefined;
  seq?: number; now?: number; properties?: string;
};
export function isWorkerToHostMessage(value: unknown): value is WorkerToHostMessage
export function isHostToWorkerMessage(value: unknown): value is HostToWorkerMessage
```
- 守卫只检查 `typeof value === "object" && value !== null` 与 `type` 是否属于对应集合
  （worker→host：`call|output|done|crash|started|idle`；host→worker：`"result"`），**不校验其余字段**。
- `done.writes` 是 JSON 数组：`[key, json]`（store）与 `[key]`（删除）。
- 所有 args/结果/值/error 都是 JSON **文本**。

#### 2.2.7 `packages/codemode/src/runtime/prelude-source.ts`（522 行）

导出：
```ts
export const MAX_STORE_VALUE_CHARS = 256 * 1024;      // 262144
export const MAX_STORE_TOTAL_CHARS = 1024 * 1024;     // 1048576
export const MAX_OUTPUT_CHARS = 16 * 1024 * 1024;     // 16777216
export const MAX_OUTPUT_ITEMS = 100_000;              // 100000
export const PRELUDE_SOURCE: string = `(function (bridge, toolsJson, globalsJson, storeJson) { ... })`;
```
`IMAGE_HELPER_EXPECTS`（未导出）：
`'image expects a non-empty image URL string, an object with image_url, or an image block { type: "image", data, mimeType }'`

**模板字面量的解析规则（Rust 必须精确复刻）**：
- 源码里 `PRELUDE_SOURCE` 是模板字面量，`\`` 与 `\\` 是转义；`${...}` 是插值。
- 5 个插值表达式（共 12 处）：`MAX_STORE_VALUE_CHARS`（2 次）、`MAX_STORE_TOTAL_CHARS`（2 次）、
  `MAX_OUTPUT_CHARS`（2 次）、`MAX_OUTPUT_ITEMS`（2 次）、`JSON.stringify(IMAGE_HELPER_EXPECTS)`（4 次）。
- 解析后（即 VM 实际 `eval` 的字符串）：
  - **UTF-16 长度 17233**，**全部 ASCII**（无多字节字符）；
  - 以 `(function (bridge, toolsJson, globalsJson, storeJson) {\n\t"use strict";\n\n\t// Freeze every object reachable` 开头，
    以 `\t\t\treturn true;\n\t\t},\n\t};\n})` 结尾（**最后 26 字符**即该串，**最后 10 字符**为 `\t},\n\t};\n})`）；
  - 缩进全部为 **TAB**；换行 `\n`；无 BOM、无 CR；
  - 模板源码里的 12 处 `${…}` 按上表替换（5 个不同表达式），替换后长度恰为 17233；
  - **FNV-1a（逐 UTF-16 码元、`Math.imul` 32 位）哈希 = `8c6c9242`**（`0x8c6c9242`）。这是
    `PRELUDE_HASH`，会进入确定性指纹；因此 Rust 侧 prelude 必须逐字节相同，否则指纹与
    `prelude [0-9a-f]{8}` 测试都会失败。
  - 参考校验值（用于 Rust 侧断言）：**SHA-256 = `57c8dc8c0e812117d82167fa751bc368f249d5aaee56d321066fc5280c785281`**
    （UTF-8 编码，因全 ASCII 与 UTF-16 等价）。
  - 已用 Node `vm.Script` 验证可解析（`test/sandbox.test.ts` 的 "parse as JavaScript" 亦验证）。
- Rust 侧建议：把解析后的文本作为 `include_str!("prelude.js")` 常量（或在构建期由同一模板生成），
  并在**测试**里断言长度 17233 与 FNV 值 `8c6c9242`，从而锁定等价性。**解析后的全文见附录 A**
  （可直接复制为 `prelude.js`）。

**prelude 行为规格（按执行顺序）**

1. `lockdown()`：冻结全部内建。
   - `OVERRIDABLE = new Set(["constructor","name","message","toString","toLocaleString","valueOf","toJSON"])`。
   - 从 `globalThis` 开始**后进先出 DFS**（`const queue = [];` 用 `queue.pop()` 取，`add()` 用 `push`；
     `seen` 去重，`globalThis` 预先入 seen）：收集每个 own property 的 value/get/set，收集 `Object.getPrototypeOf`；
     （遍历顺序影响 getter 的创建顺序，但不影响最终冻结结果；Rust 侧按 LIFO 复刻即可。）
   - `globalThis` 上 configurable 的 value 属性改为 `{writable:false, configurable:false}`；
   - 额外加入：generator/async/async-generator 函数的原型、`Object.getPrototypeOf(Int8Array)`（%TypedArray%）、
     array/map/set/string iterator 原型、`RegExp String Iterator` 原型；若存在 `Iterator`，加入 `Iterator.prototype.map`
     结果与 `Iterator.from({next(){}})` 的原型；
   - 对队列中每个对象：`Object.freeze`；对 `Object.prototype` 或 `OVERRIDABLE` 中 key 的**可写且可配置**数据属性，
     改成访问器：getter 返回原值；setter：若 `this === object` → 抛
     `TypeError("Cannot assign to read only property '" + String(key) + "' of a built-in")`；若 `this` 既非对象也非函数
     则静默返回；否则 `Object.defineProperty(this, key, {value: next, writable:true, enumerable:true, configurable:true})`。
     访问器 `enumerable` 继承原属性，`configurable: false`。
2. 捕获快照：`stringify = JSON.stringify`、`parse = JSON.parse`、`promiseThen = Promise.prototype.then`、
   `ErrorCtor/TypeErrorCtor/RangeErrorCtor`。脚本之后改写内建不再影响 prelude。
3. `pending: Map<id, {resolve,reject}>`、`nextId = 1`、`finished = false`、`EXIT = Object.freeze({})`。
4. `done(ok, payload, writes)`：`finished` 只发一次 `bridge("done", ok, payload, writes)`。
5. `serialize(value)` = `value === undefined ? undefined : stringify(value)`。
6. `errorText(error)`：`head = error.message ? error.name + ": " + error.message : String(error.name)`；
   `frames = typeof error.stack === "string" ? error.stack.split("\n").filter(line => line.trim() &&
   !line.includes("codemode-prelude.js")) : []`；`[head, ...frames].join("\n")`（V8 风格头 + QuickJS 帧）。
7. `format(value)`：string → 原样；`instanceof ErrorCtor` → `errorText`；否则 `stringify(value)`，
   `undefined` 时 `String(value)`；stringify 抛错 → `String(value)`。
8. `describeError(error)`：`instanceof ErrorCtor` →
   `stringify({name: String(error.name), message: String(error.message), stack: errorText(error)})`；
   否则 `stringify({message: String(format(error))})`；整体 try/catch →
   `stringify({message: "The script threw a value that cannot be described"})`。
9. `caller(kind, name, spread)` 返回：
   ```js
   (...args) => new Promise((resolve, reject) => {
     let json;
     try { json = serialize(spread ? args : args[0]); } catch (error) { reject(error); return; }
     const id = nextId++;
     pending.set(id, { resolve, reject });
     bridge(kind, id, name, json);
   })
   ```
   即：**先序列化**（失败直接 reject，不占 id、不发 bridge）；`nextId` 从 1 起。
10. `tools = Object.create(null)`；遍历 `parse(toolsJson)`：
    - `fn = caller("call", name)`；
    - `jsName` 不在 tools 中（避免原型链）→ `tools[jsName] = fn`；`allTools.push(Object.freeze({name: jsName, description}))`
      （**同名归一化时第一个工具赢**）；
    - 若 `name` 不在 tools 中 → `tools[name] = fn`（原样名字别名）。
    - `Object.freeze(tools)`、`Object.freeze(allTools)`。
11. `guard(target, label, names, hint)`：Proxy，`get` 拦截：
    - 条件 `typeof property !== "string" || property in object || property in Object.prototype ||
      property === "then" || property === "toJSON"` → `Reflect.get`；
    - `comparable(name) = name.toLowerCase().replace(/[^a-z0-9]/g, "")`；`wanted = comparable(property)`；
      `exact = names.filter(n => comparable(n) === wanted)`；`close = exact.length ? exact :
       names.filter(n => wanted && (comparable(n).includes(wanted) || wanted.includes(comparable(n))))`；
    - `message = label + "." + property + " does not exist."`；
      若 `close.length` → `" Did you mean " + close.slice(0,5).map(n => label + "." + n).join(", ") + "?"`；
      否则若 `names.length <= 20` → `" Available: " + names.join(", ") + "."`；
    - 若有 hint → `" " + hint`；
    - 末尾追加 `' Check for a member with "' + property + '" in ' + label + "."`；
    - `throw new TypeErrorCtor(message)`。
    `toolsProxy = guard(tools, "tools", allTools.map(t => t.name),
      "ALL_TOOLS lists every tool; searchTools(query) finds tools by topic.")`。
12. globals：遍历 `parse(globalsJson)`，`fn = caller("global", name, spread)`；
    - 无 `.` → `Object.defineProperty(globalThis, name, {value: fn, enumerable: true})`（默认 non-writable/non-configurable）；
    - 有 `.`（第一个）→ 成员累积到 `namespaces` 的 `Object.create(null)` 对象；
    - 每个 namespace：`Object.freeze(members)`；`value = guard(members, namespace, Object.keys(members))`（无 hint）；
      `Object.defineProperty(globalThis, namespace, {value, enumerable: true})`。
13. store：
    - `stored = new Map(Object.entries(parse(storeJson)))`（key → JSON 文本）；`writes = new Map()`；
      `storedChars = Σ(key.length + json.length)`。
    - `STORE_HINT = "store() is for small state such as IDs or summaries. Show images with image(), keep large data
      in variables, or write it to a file with a tool."`
    - `checkKey(name, key)`：非 string → `TypeError(name + "() key must be a string")`。
    - `store(key, value)`：
      - `previous = stored.has(key) ? key.length + stored.get(key).length : 0`；
      - `value === undefined` → 删除、`storedChars -= previous`、`writes.set(key, undefined)`、return；
      - `json = stringify(value)`，抛错 → `TypeError("store(" + stringify(key) + ") value is not JSON-serializable: " + format(error))`；
      - `json === undefined` → `TypeError("store(" + stringify(key) + ") value is not JSON-serializable")`；
      - `json.length > 262144` → `RangeError("store(" + stringify(key) + ") value has " + json.length +
        " characters of JSON, more than the limit of 262144. " + STORE_HINT)`；
      - `next = storedChars - previous + key.length + json.length`；`next > 1048576` →
        `RangeError("store is full: stored values would exceed 1048576 characters of JSON. Delete keys with store(key, undefined). " + STORE_HINT)`；
      - 写入 `stored`/`storedChars`/`writes.set(key, json)`。
    - `load(key)`：`checkKey("load", key)`；无则 `undefined`；否则 `parse(json)`（**每次返回新副本**）。
    - `serializeWrites()`：`[...writes]` → `json === undefined ? [key] : [key, json]` → `stringify(entries)`。
    - 以 `Object.defineProperty(globalThis, ...)` 安装 `store`、`load`。
14. 输出：`outputChars = 0; outputItems = 0`。
    - `output(kind, data, mimeType)`：`finished` 时直接 return；`outputChars += data.length`；`outputItems++`；
      若 `outputChars > 16777216 || outputItems > 100000` → 构造
      `RangeError("script output exceeded the limit of 16777216 characters or 100000 text(), image(), and console calls.
      Print a summary instead, or write large data to a file with a tool.")`；`done(false, describeError(error))`；`throw error`；
      否则 `bridge("output", kind, data, mimeType)`。
    - `outputText(value)`：`value === undefined || value === null || (typeof value !== "object" && typeof value !== "function")`
      → `String(value)`；否则 `json = stringify(value)`，`json === undefined ? String(value) : json`。
      注意运算符优先级：`typeof value !== "object" && typeof value !== "function"` 是整体与两侧的 `||` 结合，
      因此 `undefined`/`null` 显式短路，symbol/bigint 走 `String(value)`。
    - `text(value)`：`try { rendered = outputText(value) } catch (error) { throw new TypeErrorCtor(
      error instanceof ErrorCtor ? error.message : String(error)) }`；然后 `output("text", rendered)`
      （循环引用 → `TypeError: <引擎的 circular 消息>`）。
    - `imageUrl(value)`：string → 原值；非普通对象（含数组/null）→ IMAGE_HELPER_EXPECTS TypeError；
      `value.image_url !== undefined`：非 string → 同样 TypeError，否则返回它；
      `typeof value.type !== "string"` → 同样 TypeError；`value.type !== "image"` →
      `TypeError('image only accepts image blocks, got "' + value.type + '"')`；
      `typeof value.data !== "string" || value.data === ""` → `TypeError("image expected image block data")`；
      `value.data.toLowerCase().startsWith("data:")` → 原样；否则 `"data:;base64," + value.data`。
    - `image(value)`：
      - `url = imageUrl(value)`；`url === ""` → IMAGE_HELPER_EXPECTS TypeError；
      - `colon = url.indexOf(":")`；`scheme = colon === -1 ? "" : url.slice(0, colon).toLowerCase()`；
      - `scheme === "http" || "https"` → `TypeError("remote image URLs are not supported in tool outputs. Pass a base64
        data URI instead")`；
      - `comma = url.indexOf(",")`；`header = comma === -1 ? [] : url.slice(colon + 1, comma).split(";")`；
      - 若 `scheme !== "data" || comma === -1 || header.slice(1).every(p => p.toLowerCase() !== "base64")` →
        `TypeError("invalid image output. Pass a base64 data URI instead")`；
      - `data = url.slice(comma + 1).replace(/\s+/g, "")`（丢掉换行/空白）；
      - `data.length % 4 !== 0 || !/^[A-Za-z0-9+/]+={0,2}$/.test(data)` →
        `TypeError("invalid image output. The image data is not valid base64 (truncated or corrupted?)")`；
      - `head = data.slice(0, 16)`；签名表（顺序敏感）：
        `["image/png", /^iVBORw0KGg/]`、`["image/jpeg", /^[/]9j[/](?!9)/]`、`["image/gif", /^R0lGOD[dl]h/]`、
        `["image/webp", /^UklG.{8}RUJQ/]`；命中 → `output("image", data, mime)`，否则
        `TypeError("invalid image output. The image data is not a PNG, JPEG, GIF, or WebP image")`。
        （声明 MIME 被忽略，按数据判定。）
    - `exit()`：`serializeWrites()` 抛错 → `done(false, describeError(error))` + `throw EXIT`；
      否则 `done(true, undefined, writesJson)` + `throw EXIT`。
    - `console = {}`，对 `["log","info","warn","error","debug"]` 设 `(...args) => output("console",
      args.map(format).join(" "))`；`Object.freeze(console)`。
15. 安装 `tools`（Proxy）、`ALL_TOOLS`、`console`、`text`、`image`、`exit` 到 `globalThis`（enumerable，non-writable）。
16. 返回 `{ settle, run, stalled }`：
    - `settle(id, ok, payload, propertiesJson)`：无 pending 项 → return；删除；`!ok` →
      `error = new ErrorCtor(payload)`；若 `propertiesJson !== undefined` 则 `parse` 后逐 own key（跳过
      `name`/`message`/`stack`）`Object.defineProperty(error, key, {value, writable:true, enumerable:true, configurable:true})`；
      `entry.reject(error)`；否则 `payload === undefined ? undefined : parse(payload)`，解析失败 → `entry.reject(error)`，
      成功 → `entry.resolve(value)`。
    - `run(fn)`：`fn(toolsProxy, console)` 同步抛错 → `done(false, describeError(error))`；
      否则 `promiseThen.call(promise, value => { json = serialize(value) — 抛错 → done(false,...)；
      done(true, json, serializeWrites()) }, error => done(false, describeError(error)))`。
    - `stalled()`：`finished || pending.size > 0` → `false`；否则
      `done(false, stringify({name:"Error", message:"The script is waiting on a promise that can never settle:
      no tool call is pending, and timers do not exist here."}))`；返回 `true`。

**脚本可见 API 总结（文档级，逐字）**：`tools`、`ALL_TOOLS`、`console.*`、`text(value)`、`image(urlOrItem)`、
`exit()`、`store(key, value)`、`load(key)`；无 `setTimeout`/`fetch`/`process`/`require`/模块/`WebAssembly`；
`eval`/`Function` 可用但只在同一 VM 内；动态 `import()` 不可用。

#### 2.2.8 `packages/codemode/src/runtime/vm.ts`（341 行）

导出：`VmSession`、`StartVmOptions`、`startVm`。内部：`DETERMINISTIC_VERSION = 1`、`fnv1a`、`PRELUDE_HASH`、`prng`、
`VirtualClock`、`deterministicWasi`、`discardOutput`、`describeException`、`JOBS_PER_POLL = 50`、`crashMessage`。

```ts
export interface VmSession { deliver(message: HostToWorkerMessage): void; dispose(): void }
export interface StartVmOptions {
  data: VmStartData;
  wasm: CodemodeWasmModule;
  maxStackSize?: number;              // 默认 MAX_STACK_SIZE (512 KiB)
  interruptBudget?: number;           // 仅进入指纹
  post(message: WorkerToHostMessage): void;
  shouldInterrupt(): boolean;
}
```

- `fnv1a(text)`：`hash = 0x811c9dc5`；对每个 UTF-16 码元 `hash ^= charCodeAt(i); hash = Math.imul(hash, 0x01000193)`；
  `(hash >>> 0).toString(16).padStart(8, "0")`。`PRELUDE_HASH = fnv1a(PRELUDE_SOURCE)` = **`8c6c9242`**。
- `prng(seed)`（mulberry32）：`state = seed >>> 0`；每次：`state = (state + 0x6d2b79f5) >>> 0`；
  `value = Math.imul(state ^ (state >>> 15), 1 | state)`；`value = (value + Math.imul(value ^ (value >>> 7), 61 | value)) ^ value`；
  `return (value ^ (value >>> 14)) >>> 0`。用于 WASI `random_get`，种子 `seed ^ 0x9e3779b9`。
- 确定性 WASI（`deterministicWasi(clock)`）：
  - `clock_time_get(clockId, precision, resultPtr)`：
    - `clock.creating` 时：`clockId === 0` → `BigInt(seed + 1) * 1000n` ns；否则 `0n`。
    - 之后：`clockId === 0` → `BigInt(Math.round(now)) * 1_000_000n`；否则
      `BigInt(Math.round(now - startedAt)) * 1_000_000n`。
    - 以小端 u64 写入 wasm 内存，返回 0。
  - `random_get(ptr, len)`：按 4 字节一组 `value = random()`，小端写入（最后一组截断）。
  - 继承 `discardOutput(memory)`。
- `discardOutput(memory)`：`fd_write` 遍历 iovec（`nwritten += getUint32(iovsPtr + i*8 + 4, true)`），
  把总数写回 `nwrittenPtr`，返回 0（引擎诊断输出被丢弃，不污染宿主 stdout/stderr）。
- `describeException(error: JSException)`：`head = error.message ? name + ": " + message : name`；
  `stack = error.stack?.trimEnd()`；`JSON.stringify({name, message, stack: stack ? head + "\n" + stack : head})`。
- `crashMessage(error)`：`{type:"crash", message: error instanceof Error ? name + ": " + message : String(error)}`。
- `startVm(options)` 流程：
  1. `maxStackSize = options.maxStackSize ?? MAX_STACK_SIZE`；`clock` 仅在 `data.deterministic` 时构造
     （`creating: true, startedAt, now: startedAt, seed: seed >>> 0`）。
  2. `QuickJS.create({ wasm, memoryLimit: data.memoryLimitBytes, maxStackSize, interruptHandler: shouldInterrupt,
     wasi: clock ? deterministicWasi(clock) : discardOutput,
     ...(clock ? { timezoneOffset: 0, intrinsics: Intrinsics.ALL & ~Intrinsics.WEAK_REF } : {}) })`；
     `clock.creating = false`。
  3. 状态：`disposed`、`issued = 0`、`nextSeq = 1`、`current = 0`、`early: Map<seq, message>`。
  4. `apply(message)`：`withScope` 调 `settle(api, id, ok, payload ?? undefined, properties ?? undefined)`；然后 `drain()`。
  5. `session.deliver(message)`：
     - 非确定性：`apply(message)`。
     - 确定性：`seq` 缺失/非整数/`< nextSeq`/`early.has(seq)` →
       `throw new Error(`Result ${message.id} has sequence number ${seq}, expected ${nextSeq} or later, once`)`
       （被 catch 转 `crash`）；
       `early.set(seq, message)`；从 `nextSeq` 连续取出：删除、若 `now` 有限则 `clock.now = max(clock.now, now)`、
       `current = nextSeq`、`apply`、`post({type:"idle", seq: nextSeq, issued})`、`nextSeq++`。
     - 任何抛错 → `post(crashMessage(error))`。
  6. `session.dispose()`：只执行一次 `vm.dispose()`。
  7. 通过 `vm._getExports()` 直接调用内部导出（quickjs-wasi 版本被精确锁定）：
     `drain()`：`while (exports.qjs_is_job_pending()) { if (exports.qjs_execute_pending_job() < 0) throw new Error("Promise job failed");
       if (++jobs % 50 === 0 && options.shouldInterrupt()) throw new Error("Execution interrupted") }`；
     然后 `callFunction(stalled, api).dispose()`（检测"永远无法 settle 的等待"）。
  8. `bridge = vm.newFunction("bridge", (kind, a, b, c) => ...)`：
     - `"call"` / `"global"`：`issued++`；`post({type:"call", id: a.toNumber(), target: kind==="call" ? "tool" : "global",
       name: b.toString(), args: (c===undefined||c.isUndefined) ? undefined : c.toString()})`。
     - `"output"`：`a === "image"` → `{type:"image", data: b, mimeType: c}`；`a === "console"` →
       `{type:"text", text: b, console: true}`；否则 `{type:"text", text: b}`。
     - `"done"`：`during = clock ? {during:{seq: current, issued}} : {}`；`a.toBoolean()` →
       `{type:"done", ok:true, value: b undefined ? undefined : b.toString(), writes: c.toString(), ...during}`；
       否则 `{type:"done", ok:false, error: b.toString(), ...during}`（`during` 只在确定性模式出现）。
  9. 求值 prelude：`vm.evalCode(PRELUDE_SOURCE, "codemode-prelude.js")` 得到函数，调用参数
     `(undefined, bridge, vm.newString(JSON.stringify(data.tools)), JSON.stringify(data.globals), JSON.stringify(data.store))`；
     用 `scope.escape` 保住句柄；取出 `settle`、`run`、`stalled`。
  10. 确定性模式：`versions = vm.versions`；
      `post({type:"started", fingerprint: `quickjs-wasi ${versions["quickjs-wasi"]}, quickjs ${versions.quickjs},
      prelude ${PRELUDE_HASH}, deterministic ${DETERMINISTIC_VERSION}, stack ${maxStackSize},
      heap ${data.memoryLimitBytes ?? "none"}, budget ${options.interruptBudget ?? "none"}`})`。
  11. 编译脚本：`vm.evalCode(`(async (tools, console) => {${data.code}\n})`, "codemode.js")`
      （前缀与脚本共处第 1 行 → 行号与源码一致；第 1 列号会偏移）。`JSException` →
      `post({type:"done", ok:false, error: describeException(error)})` 并返回 session（不运行）。
  12. `callFunction(run, api, fn)`；`fn.dispose()`；`drain()`；确定性模式再 `post({type:"idle", seq: 0, issued})`。
  13. 任何外层异常 → `post(crashMessage(error))`。返回 session。
  14. 约定：`post` 在 VM 回调内**同步**调用；调用方不得从 `post` 重入 VM（不 deliver、不 dispose）；
      `deliver` 只在 VM 空闲时调用；VM 在 `done`/`crash` 后**不会自行结束**，直到调用方 dispose 或终止 worker。

**确定性随机数（已用 pinned 值反推并精确验证）**

`deterministicWasi` 的时钟在上下文创建期把 realtime 时钟读成 `seed + 1` 微秒（`NaN`/0 会被 QuickJS 变成 1），
QuickJS 用它初始化内部随机状态。观测到的精确链条（对所有 seed 成立，测试仅 pin 了 seed 0 与 1）：

```
S₁ = (seed + 1) mod 2^64
S_{n+1} = xorshift64star(S_n):
    x = S_n
    x ^= x >> 12        (mod 2^64)
    x ^= x << 25        (mod 2^64)
    x ^= x >> 27        (mod 2^64)
    out_n = (x * 2685821657736338717) mod 2^64
Math.random() 的第 k 次调用（k = 0,1,2,…）= ((out_{k+1} >> 12) as f64) * 2^-52
```
即 **丢弃 `out_0`**，第一次 `Math.random()` 用 `out_1`。验证：
- seed 0：`out_1` → `0.6711372530266764`，`out_2` → `0.7258461452833667`（= `PINNED_SEED_ZERO`）。
- seed 1：`out_1` → `0.34227450605335275`（= `PINNED_FIRST_RANDOM`）。
- seed 与 `startedAt` 无关（换 `startedAt` 不改变随机值）；seed 2 给出不同值；seed 0 与 seed 1 值不同
  （因为 QuickJS 把 0 状态变成 1，若直接把 seed 当 state 会让 seed 0 与 1 相同——实现必须用 `seed + 1`）。
- 转换必须是 `>> 12` 再乘 `2^-52`：用 `>> 11`/`2^-53` 会得到差 1 ulp 的 `0.7258461452833668`，测试会失败。

Rust 建议：由于 Rust 侧自带 VM，**不要依赖引擎随机数**——在确定性模式下直接注入上面的 `Math.random`
（以及在 prelude 顶层把 `Math.random` 定义为该序列），同时保留虚拟时钟，从而保证与 pinned 值一致；此时
`Intrinsics`/`WeakRef` 的删除与 `timezoneOffset: 0` 也必须等价实现（见 §8）。

#### 2.2.9 `packages/codemode/src/runtime/execution.ts`（573 行）

导出：`VmEvents`、`VmChannel`、`VmLauncher`、`errorMessage`、`CodemodeSandboxBase`。
内部：`DEFAULT_TIMEOUT_MS = 300_000`、`IDENTIFIER`、`RESERVED_GLOBALS`、
`serializeStore`、`BridgeError`、`parseBridgeJson`、`parseStoreWrites`、`parseScriptError`、`PendingCall`、
`checkGlobals`、`Control`、`ExecutionOptions`、`Execution`。

```ts
export interface VmEvents { message(message: unknown): void; failure(error: CodemodeError): void }
export interface VmChannel { post(message: HostToWorkerMessage): void; stop(): Promise<void> }
export type VmLauncher = (data: VmStartData, events: VmEvents) => VmChannel | Promise<VmChannel>;
export function errorMessage(error: unknown): string   // Error ? error.message : String(error)
```

`RESERVED_GLOBALS = {tools, ALL_TOOLS, console, text, image, exit, globalThis, store, load}`。

`serializeStore(store)`：遍历 `Object.entries(store ?? {})`，`JSON.stringify(value)`，非 `undefined` 的写入
`serialized[key] = json`（JSON 无法表示的值被静默丢弃）。

`BridgeError` 与解析器（错误文本逐字）：
- `parseBridgeJson(json, what)`：`JSON.parse` 失败 → BridgeError `` `${what} is not valid JSON` ``。
- `parseStoreWrites(json)`：
  - 非数组 → `"store writes are not an array"`；
  - 每项必须是数组；`typeof entry[0] === "string"`；`entry.length === 2` 时 `typeof entry[1] === "string"`，
    否则 `entry.length === 1`；不满足 → `"store writes contain a malformed entry"`；
  - `[key, value]`：`value === undefined` → `delete.push(key)`；否则 `set[key] = parseBridgeJson(value,
    `store value for ${JSON.stringify(key)}`)`。
  - 结果 `{ set, delete }`。
- `parseScriptError(json)`：
  - 非对象/null → `"script error is not an object"`；
  - `message` 必须 string；`name`/`stack` 若存在必须 string，否则 `"script error is malformed"`；
  - 返回 `{kind:"script", name, message, stack}`。

`checkGlobals(globals)`：
- `parts = name.split(".")`；`parts.length > 2` 或任一部分不匹配 `IDENTIFIER` 或 `RESERVED_GLOBALS.has(parts[0])` →
  `` throw new Error(`Invalid global name "${name}"`) ``；
- 重名 → `` `Global "${name}" is already registered` ``；
- `parts.length === 2` → namespace 集合加入 `parts[0]`；names 加入全名；
- 最后：namespace 名字若同时是某个 global 全名 → `` `Global "${name}" conflicts with the namespace "${name}"` ``。
  （检查顺序：先全部个体校验，再冲突校验。）

`Execution`（一次脚本运行）：
- 字段：`promise`（手动 resolve）、`channel?`、`unsent: HostToWorkerMessage[]`、`executors`、`control?`、
  `delivered: Set<number>`、`signal?`、`timer?`、`output`、`calls`、`pending: Map<id, PendingCall>`、`finished`。
- 构造函数：
  1. 建 promise。
  2. `Number.isFinite(timeoutMs)` → `setTimeout(() => finish({kind:"timeout", message:`Execution timed out after ${timeoutMs} ms`}), timeoutMs)`。
  3. signal：已 aborted → 立即 `onAbort()`；否则 `addEventListener("abort", onAbort, {once:true})`。
  4. 若已 finished 则 return。
  5. `data: VmStartData = { code, tools: tools.map(t => ({name, jsName: t.jsName ?? toCodemodeIdentifier(name),
     description: t.description ?? ""})), globals: globals.map(g => ({name, spread: g.spread === true})),
     memoryLimitBytes, store, ...(deterministic ? {deterministic} : {}) }`。
  6. `new Promise(resolve => resolve(options.launch(data, events))).then(channel => { if (finished) { void channel.stop().catch(...); return }
     channel = ...; for (m of unsent.splice(0)) channel.post(m) }, error => finish({kind:"sandbox", message: errorMessage(error)}))`。
- `deliver(seq, now, id, outcome)`（`CodemodeRun.deliver`）：
  1. `finished` → return（不抛）。
  2. `!Number.isFinite(now)` → `` throw new Error(`The clock value of result ${seq} is not a finite number`) ``。
  3. `!Number.isInteger(seq) || seq < 1 || delivered.has(seq)` →
     `` throw new Error(`Result number ${seq} is not a positive integer used once`) ``。
  4. `pending.get(id)` 不存在 → `` `Call ${id} is not waiting for a result` ``。
  5. `!outcome.ok && outcome.properties !== undefined` 且不是普通对象/null/数组 →
     `` `The properties of result ${seq} are not an object` ``。
  6. 序列化（**先于记录任何状态**，以便 JSON 无法表示的值让调用继续等待下一次回答）：
     `payload = outcome.ok ? (value === undefined ? undefined : JSON.stringify(value)) : outcome.message`；
     `properties = (!outcome.ok && properties !== undefined) ? JSON.stringify(properties) : undefined`；
     抛错 → 直接向上传播（seq 未标记）。
  7. `delivered.add(seq)`；`pending.delete(id)`；记录 `status = ok ? "ok" : "error"`、`durationMs = performance.now() - startedAt`；
     `post({type:"result", id, ok, payload, seq, now, ...(properties===undefined?{}:{properties})})`。
- `abort(message)`：`finish({kind:"aborted", message})`；返回 promise。
- `onAbort`：`finish({kind:"aborted", message: signal.reason instanceof Error ? reason.message : "Execution aborted"})`。
- `post(message)`：有 channel 直接 post，否则入 `unsent`。
- `receive(message)`：`try handleMessage catch (error)` → `finish({kind:"sandbox", message:
  error instanceof BridgeError ? `Sandbox bridge broken: ${error.message}. The script may have modified built-ins
  such as a prototype's toJSON.` : `Sandbox host failed: ${errorMessage(error)}`})`。
- `handleMessage(message)`：
  - finished → return；
  - `!isWorkerToHostMessage(message)` → `throw new BridgeError("unknown message from the worker")`；
  - `output` → `output.push(item)`；
  - `call` → `pending.has(id)` → `throw new BridgeError(`duplicate call id ${id}`)`；
    有 control → `forwardCall(message, control)`；否则 `void handleCall(message)`；
  - `started` → `control?.onStarted?.(fingerprint)`；
  - `idle` → `control?.onIdle?.(seq, issued)`；
  - `done` → `handleDone`；
  - `crash` → `finish({kind:"sandbox", message})`。
- `handleDone(message)`（先解码后 finish）：
  - `!ok` → `finish(parseScriptError(message.error), undefined, undefined, message.during)`；
  - 否则 `value = message.value === undefined ? undefined : parseBridgeJson(message.value, "return value")`；
    `finish(undefined, value, parseStoreWrites(message.writes), message.during)`。
- `forwardCall(message, control)`（确定性 run）：解析 `args = message.args === undefined ? undefined :
  parseBridgeJson(message.args, "call arguments")`；`record = target === "tool" ? {name, status:"cancelled", durationMs:0} : undefined`；
  `record` 存在则 push 到 calls；`pending.set(id, {record, startedAt: performance.now(), controller: new AbortController()})`；
  `control.onCall({id, target, name, args})`。
- `handleCall(message)`（execute 路径）：
  - `isTool = target === "tool"`；record 同上；
  - try：`tool = executors[isTool ? "tools" : "globals"].get(name)`；不存在 →
    `` throw new Error(`Unknown ${isTool ? "tool" : "global"} "${name}"`) ``；`args = message.args === undefined ?
    undefined : JSON.parse(message.args)`（**直接 JSON.parse**，畸形会走 catch 变成工具错误）；
    `value = await tool.execute(args, {signal: pending.controller.signal})`；
    `reply = {type:"result", id, ok:true, payload: value === undefined ? undefined : JSON.stringify(value)}`；status `"ok"`。
  - catch：`reply = {type:"result", id, ok:false, payload: errorMessage(error)}`；status `"error"`。
  - `if (!this.pending.delete(id)) return`（已被 finish 取消 → record 保持 `"cancelled"`）；
    否则记录 `status`/`durationMs = performance.now() - startedAt` 并 `post(reply)`。
- `finish(error?, value?, writes?, during?)`：
  1. 已 finished → return；置 `finished = true`；`clearTimeout(timer)`；移除 abort 监听。
  2. `now = performance.now()`；对每个 pending：record 的 `durationMs = now - startedAt`；`controller.abort()`；
     清空 pending、`unsent = []`。
  3. `ended = during === undefined ? {} : {during: {seq, issued}}`；
  4. result：error → `{ok:false, error, output, calls, ...ended}`；否则 `{ok:true, value, output, calls,
     storeWrites: writes ?? {set:{}, delete:[]}, ...ended}`。
  5. 无 channel → 立即 resolve；否则 `channel.stop().catch(() => undefined).then(() => resolve(result))`。

`CodemodeSandboxBase`：
- 字段：`toolsByName: Map`、`globalsByName: Map`、`timeoutMs`、`memoryLimitBytes?`、`launch`、`running: Set<Execution>`、`closed`。
- 构造函数：`timeoutMs = options.timeoutMs ?? 300000`；注册每个 tool（`registerTool`）；`checkGlobals(globals)`；
  存入 globalsByName（**顺序：先 tools，再 checkGlobals，再填 globals**——因此非法 global 会让构造函数抛错）。
- `registerTool(tool)`：重名 → `` `Tool "${tool.name}" is already registered` ``；否则 set。
- `unregisterTool(name)` → boolean。
- `get tools()`：`[...toolsByName.values()]`；`get globals()`：`[...globalsByName.values()]`。
- `execute(code, options = {})`：
  - `closed` → `Promise.reject(new Error("Sandbox is closed"))`；
  - `tools = new Map(toolsByName)`（快照）；构造 Execution：`code`, `tools: [...tools.values()]`,
    `globals: [...globalsByName.values()]`, `executors: {tools, globals: globalsByName}`,
    `timeoutMs: options.timeoutMs ?? this.timeoutMs`, `signal`, `memoryLimitBytes`,
    `store: serializeStore(options.store)`, `launch`；
  - `running.add(execution)`；返回 `execution.promise.finally(() => running.delete(execution))`。
- `start(code, options)`：
  - `closed` → `throw new Error("Sandbox is closed")`（同步）；
  - `!Number.isFinite(startedAt) || startedAt < 0` → `throw new Error("startedAt must be a non-negative number of milliseconds since the epoch")`；
  - `!Number.isInteger(seed) || seed < 0 || seed > 0xffffffff` → `throw new Error("seed must be a 32-bit unsigned integer")`；
  - `tools = options.tools ?? []`；name 重复 → `throw new Error("Tool names must be unique")`；
    `scriptNames = tools.map(t => t.jsName ?? toCodemodeIdentifier(t.name))` 重复 →
    `throw new Error("Tool script names must be unique")`；
  - `globals = options.globals ?? []`；`checkGlobals(globals)`；
  - Execution：`executors: {tools:new Map(), globals:new Map()}`，`timeoutMs: options.timeoutMs ?? Infinity`，
    `memoryLimitBytes`（sandbox 级），`store: serializeStore(options.store)`，
    `deterministic: {startedAt, seed}`，`control: {onStarted, onCall, onIdle}`，`launch`；
  - 返回 `{ result, deliver: (...), get finished() { return execution.finished } }`。
- `close()`：`closed = true`；`await Promise.all([...running].map(e => e.abort("Sandbox closed")))`。

#### 2.2.10 `packages/codemode/src/runtime/inline.ts`（126 行）

导出：`DEFAULT_INTERRUPT_BUDGET = 100_000`、`INLINE_MAX_STACK_SIZE = 256 * 1024`、
`InlineCodemodeSandboxOptions`、`inlineLauncher`、`InlineCodemodeSandbox`。

`inlineLauncher(wasm, interruptBudget = 100_000): VmLauncher`：
- `await wasm` 失败 → `throw new Error(`Failed to load QuickJS: ${errorMessage(error)}`)`。
- 局部状态 `stopped`、`settled`、`polls = 0`、`exhausted`。
- `startVm({data, wasm, maxStackSize: 256*1024, interruptBudget, shouldInterrupt: () => { if (stopped || settled ||
  exhausted) return true; if (++polls <= interruptBudget) return false; exhausted = true; return true }, post})`。
- `post`：`terminal = type === "done" || "crash"`；`terminal && settled` → 忽略（中断已结算 VM 造成的 crash 是噪声）；
  `terminal` → `settled = true`；`queueMicrotask(() => { if (stopped) return;
  if (exhausted && (done || crash)) events.failure({kind:"timeout", message: `Execution exceeded its interrupt budget of ${interruptBudget}`});
  else events.message(message) })`。
- 返回 channel：`post` → `!stopped && session.deliver(message)`；`stop` → 只一次：`stopped = true; session.dispose()`。

`InlineCodemodeSandbox extends CodemodeSandboxBase`：构造用 `inlineLauncher(options.wasm, options.interruptBudget)`。
（在调用线程运行；脚本计算期间宿主事件循环不跑，deadline/abort 只在脚本 await 工具时生效；预算耗尽 → `kind:"timeout"`。）

#### 2.2.11 `packages/codemode/src/runtime/host.ts`（88 行）

导出：`CodemodeSandbox`。内部：`defaultWorkerUrl`、`workerLauncher`。

- `defaultWorkerUrl()`：`new URL(import.meta.url.endsWith(".ts") ? "./worker.ts" : "./worker.js", import.meta.url)`。
- `workerLauncher(options): VmLauncher`：
  1. `module = await (options.wasm ?? loadQuickJSWasm())`，失败 →
     `throw new Error(`Failed to load QuickJS: ${errorMessage(error)}`)`。
  2. `interrupt = new SharedArrayBuffer(4)`；`workerData = {...data, wasm: module, interrupt}`。
  3. `releaseWorker = options.acquireWorker?.()`（**在 worker 构造之前、wasm 加载之后**）。
  4. `new Worker(options.workerUrl ?? defaultWorkerUrl(), { workerData })` 抛错 → `releaseWorker?.()` 后
     `throw new Error(`Failed to start worker: ${errorMessage(error)}`)`。
  5. `releaseWorker` 存在 → `worker.once("exit", releaseWorker)`。
  6. `worker.on("message", m => ...)`：
     - Node 24/26 `--watch` 会泄露依赖报告（nodejs/node#65044）：若 `m` 是对象且含 `"watch:import"` 或
       `"watch:require"` key → 忽略；
     - 否则 `events.message(m)`。
  7. `worker.on("error", e => events.failure({kind:"sandbox", name: e instanceof Error ? e.name : undefined,
     message: errorMessage(e)}))`。
  8. `worker.on("exit", code => events.failure({kind:"sandbox", message: `Worker exited with code ${code} before the script settled`}))`。
  9. `stop()`：`Atomics.store(new Int32Array(interrupt), 0, 1)`；`void worker.terminate().catch(() => undefined)`；
     `worker.unref()`（terminate 内部会 ref；不等待退出）。
  10. `try { options.onWorkerStarted?.(new Promise(resolve => worker.once("exit", () => resolve()))) }
      catch (error) { void stop(); throw error }`（回调抛错 → 请求终止并把错误抛出 → 执行以 `kind:"sandbox"` 失败）。
  11. 返回 `{post: m => worker.postMessage(m), stop}`。
- `CodemodeSandbox extends CodemodeSandboxBase`：`constructor(options: CodemodeSandboxOptions = {})`。

#### 2.2.12 `packages/codemode/src/runtime/remote.ts`（277 行）

导出：`CodemodeRemoteRequest`、`CodemodeRemoteMessage`、`CodemodeRemoteReply`、`CodemodeRemoteExchange`、
`CodemodeRemote`、`RemoteCodemodeSandboxOptions`、`CodemodeServeOptions`、`serveCodemodeRemote`、
`RemoteCodemodeSandbox`。内部：`FLUSH_ITEMS = 1_000`、`FLUSH_CHARS = 1 << 20`、`remoteLauncher`。

```ts
export interface CodemodeRemoteRequest { data: VmStartData; interruptBudget?: number }
export type CodemodeRemoteMessage = WorkerToHostMessage | { type: "failure"; error: CodemodeError };
export type CodemodeRemoteReply = HostToWorkerMessage | { type: "stop" } | null;
export type CodemodeRemoteExchange = (messages: CodemodeRemoteMessage[]) => Promise<CodemodeRemoteReply>;
export interface CodemodeRemote { execute(request, exchange): Promise<void> }
export interface RemoteCodemodeSandboxOptions extends CodemodeSandboxBaseOptions {
  remote: () => CodemodeRemote; interruptBudget?: number;
}
export interface CodemodeServeOptions { wasm; maxInterruptBudget?; maxMemoryLimitBytes? }
```

`serveCodemodeRemote(request, exchange, options)`：
- `maxBudget = maxInterruptBudget ?? 100000`；`budget = min(request.interruptBudget ?? maxBudget, maxBudget)`。
- 内存：`requestedMemory = request.data.memoryLimitBytes`；`maxMemory = maxMemoryLimitBytes`；
  `memoryLimitBytes = requestedMemory === undefined ? maxMemory : maxMemory === undefined ? requestedMemory :
  min(requestedMemory, maxMemory)`；`data = {...request.data, memoryLimitBytes}`。
- 状态：`channel?`、`ended`、`buffered: CodemodeRemoteMessage[]`、`bufferedChars`、`unsent: HostToWorkerMessage[]`。
- `takeBuffer()`；`end(terminal?)`：幂等；`batch = takeBuffer()`；`delivered = terminal ? exchange([...batch, terminal]).catch(() => null)
  : Promise.resolve()`；然后 `delivered.then(() => channel?.stop()).catch(...).then(resolve)`。
- `reply(message)`：`ended || message === null` → return；`{type:"stop"}` → `end(undefined)`；
  否则 channel 存在则 post，否则 unsent.push。
- `onMessage(message)`：`ended` → return；`output` → 入 buffer，`bufferedChars += item.type === "text" ?
  item.text.length : item.data.length`；达到 `1000` 项或 `1 Mi` 字符 → `exchange(takeBuffer()).then(reply, () => end(undefined))`；
  `started` → 入 buffer；`call`/`idle` → `exchange([...takeBuffer(), message]).then(reply, () => end(undefined))`；
  默认（done/crash）→ `end(typed)`。
- `launch = inlineLauncher(options.wasm, budget)`；`Promise.resolve(launch(data, {message: onMessage,
  failure: error => end({type:"failure", error})})).then(started => { channel = started; if (ended) { void started.stop(); return }
  for (m of unsent.splice(0)) started.post(m) }, error => end({type:"failure", error: {kind:"sandbox", message: errorMessage(error)}}))`。

`remoteLauncher(remote, interruptBudget): VmLauncher`：
- `stopped`、`settled`、`waiting: Map<id, resolve>`。
- `exchange(messages)`：`new Promise(resolve => { if (stopped || !Array.isArray(messages)) { resolve({type:"stop"}); return }
  const call = messages.find(m => isWorkerToHostMessage(m) && m.type === "call")；
  if (call) waiting.set(call.id, resolve); else resolve(null)；
  for (m of messages) { if (m 是对象且 type === "failure") { settled = true; events.failure(m.error); continue }
  events.message(m)；if (isWorkerToHostMessage(m) && (done || crash)) settled = true } })`
  （**先注册 waiter**，因为宿主可能在工具同步抛错时立即回答）。
- `target = remote()` 抛错 → `throw new Error(`Failed to reach the remote sandbox: ${errorMessage(error)}`)`。
- `request = interruptBudget === undefined ? {data} : {data, interruptBudget}`；
  `Promise.resolve().then(() => target.execute(request, exchange)).then(() => { if (!settled && !stopped)
  events.failure({kind:"sandbox", message:"Remote sandbox returned before the script settled"}) },
  error => { if (!stopped) events.failure({kind:"sandbox", message: `Remote sandbox failed: ${errorMessage(error)}`}) })`。
- 返回 channel：`post(m)` → 取 `waiting.get(m.id)`，无则 return；删除并 resolve(m)；
  `stop()` → `stopped = true`；对每个 waiter resolve `{type:"stop"}`；清空。
- `RemoteCodemodeSandbox`：`super(options, remoteLauncher(options.remote, options.interruptBudget))`。

#### 2.2.13 `packages/codemode/src/runtime/worker.ts`（39 行）

- 模块顶层：`if (parentPort) main(workerData as WorkerData).catch(error => post({type:"crash",
  message: error instanceof Error ? `${error.name}: ${error.message}` : String(error)}))`——**导入即启动**；
  主线程导入该模块（`parentPort` 为 null）时什么也不做。
- `post(message)` = `parentPort?.postMessage(message)`。
- `main(data: WorkerData)`：`interrupt = new Int32Array(data.interrupt)`；`session = await startVm({data, wasm:
  data.wasm, post, shouldInterrupt: () => Atomics.load(interrupt, 0) !== 0})`（**永不 dispose**——VM 活到宿主
  terminate）；然后 `parentPort?.on("message", message => { if (isHostToWorkerMessage(message))
  session.deliver(message) })`（注意：监听器在 VM 建好**之后**才注册；启动期间到达的 result 会排队在端口上）。
- `startVm` 抛错（VM 建不起来）→ 上面的 `.catch` 发 `crash`。

#### 2.2.14 `packages/codemode/src/cloudflare.ts` 与 `cloudflare-modules.d.ts`

- `export * from "./portable.ts"`。
- `quickjsWasm = quickjsWasmImport as CodemodeWasmModule`（Wrangler `CompiledWasm` 规则）。
- `export const DEFAULT_REMOTE_MEMORY_LIMIT_BYTES = 32 * 1024 * 1024`。
- `export const cloudflareQuickJSWasm: CodemodeWasmModule = quickjsWasm`。
- `export function createInlineCodemodeSandbox(options: Omit<InlineCodemodeSandboxOptions, "wasm"> = {})` →
  `new InlineCodemodeSandbox({...options, wasm: quickjsWasm})`。
- `export class CodemodeSandboxDurableObject<Env = unknown> extends DurableObject<Env>`：
  `protected maxInterruptBudget = DEFAULT_INTERRUPT_BUDGET`；`protected maxMemoryLimitBytes = DEFAULT_REMOTE_MEMORY_LIMIT_BYTES`；
  `execute(request, exchange): Promise<void>` → `serveCodemodeRemote(request, exchange, {wasm: quickjsWasm,
  maxInterruptBudget: this.maxInterruptBudget, maxMemoryLimitBytes: this.maxMemoryLimitBytes})`。
- `cloudflare-modules.d.ts` 只为仓库自身类型检查声明 `cloudflare:workers` 的 `DurableObject` 抽象类与
  `*/quickjs.wasm` 默认导出（不发布）。

#### 2.2.15 `packages/codemode/src/index.ts` 与 `portable.ts`

**`index.ts`（主入口，含 worker 后端）逐字导出清单**：

- 值：`DEFAULT_INPUT_SCHEMA_MAX_CHARS`、`renderDeclarations`、`renderToolOutputType`、`renderToolSample`、
  `renderToolSignature`、`schemaToType`、`toCodemodeIdentifier`、`CodemodeSandboxBase`、`CodemodeSandbox`、
  `DEFAULT_INTERRUPT_BUDGET`、`InlineCodemodeSandbox`、`MAX_OUTPUT_CHARS`、`MAX_OUTPUT_ITEMS`、
  `MAX_STORE_TOTAL_CHARS`、`MAX_STORE_VALUE_CHARS`、`RemoteCodemodeSandbox`、`serveCodemodeRemote`、
  `CODEMODE_OPTIONS_PREFIX`、`CODEMODE_SOURCE_GRAMMAR`、`CodemodeSourceError`、`parseCodemodeSource`、
  `loadQuickJSWasm`。
- 类型：`RenderDeclarationsOptions`、`InlineCodemodeSandboxOptions`、`CodemodeRemote`、`CodemodeRemoteExchange`、
  `CodemodeRemoteMessage`、`CodemodeRemoteReply`、`CodemodeRemoteRequest`、`CodemodeServeOptions`、
  `RemoteCodemodeSandboxOptions`、`CodemodeSourceOptions`、`ParsedCodemodeSource`、`CodemodeCall`、
  `CodemodeCallStatus`、`CodemodeDeclaredGlobal`、`CodemodeDeclaredTool`、`CodemodeDuring`、`CodemodeError`、
  `CodemodeErrorKind`、`CodemodeExecuteOptions`、`CodemodeHostCall`、`CodemodeJsonSchema`、`CodemodeOutcome`、
  `CodemodeOutputItem`、`CodemodeResult`、`CodemodeRun`、`CodemodeRunOptions`、`CodemodeSandboxBaseOptions`、
  `CodemodeSandboxOptions`、`CodemodeStoreWrites`、`CodemodeTool`、`CodemodeToolContext`、`CodemodeWasmModule`。
- **不**从主入口导出：`PRELUDE_SOURCE`（任何公开入口都不导出；测试直接 `import` `../src/runtime/prelude-source.ts`）、
  `startVm`/`VmSession`/`StartVmOptions`
  （内部）、`inlineLauncher`/`INLINE_MAX_STACK_SIZE`（内部，测试直接引 `../src/runtime/inline.ts`）、
  协议守卫 `isWorkerToHostMessage`/`isHostToWorkerMessage`/`VmStartData`/`WorkerData`/`WorkerToHostMessage`/
  `HostToWorkerMessage`（`/portable` 只导出后 3 个类型）、`errorMessage`（内部，测试从 execution.ts 引）、
  `ScriptErrorJson`（仅协议文件）。

**`portable.ts`（无 Node 内建）**：`toCodemodeIdentifier`、`CodemodeSandboxBase`、`DEFAULT_INTERRUPT_BUDGET`、
`InlineCodemodeSandbox`、`InlineCodemodeSandboxOptions`、`MAX_OUTPUT_CHARS`、`MAX_OUTPUT_ITEMS`、
`MAX_STORE_TOTAL_CHARS`、`MAX_STORE_VALUE_CHARS`、`HostToWorkerMessage`/`VmStartData`/`WorkerToHostMessage`（类型）、
`CodemodeRemote`、`CodemodeRemoteExchange`、`CodemodeRemoteMessage`、`CodemodeRemoteReply`、`CodemodeRemoteRequest`、
`CodemodeServeOptions`、`RemoteCodemodeSandbox`、`RemoteCodemodeSandboxOptions`、`serveCodemodeRemote`、
`CodemodeWasmModule` 以及除 `CodemodeSandboxOptions` 外的全部 `Codemode*` 类型。
**不**导出 `CodemodeSandbox`（worker 后端）、`loadQuickJSWasm`（文件系统）、`render*`/`schemaToType`/`source` 解析器
（这些走各自的子路径入口）。

#### 2.2.16 `packages/codemode/examples/**`

**`examples/cloudflare/agent/src/index.ts`**（Session Durable Object，工具在会话内执行、脚本在独立 Worker）：

```ts
interface Env {
  SESSION: DurableObjectNamespace<Session>;
  CODEMODE_SANDBOX: DurableObjectNamespace<CodemodeSandboxDurableObject>;
}
export class Session extends DurableObject<Env> {
  private counter = 0;
  async runCodemode(code: string) {
    const tools: CodemodeTool[] = [{
      name: "count",
      description: "Adds to a counter kept in the session and returns the new value",
      execute: (args) => { this.counter += (args as { add: number }).add; return this.counter; },
    }];
    const sandbox = new RemoteCodemodeSandbox({
      tools, timeoutMs: 60_000,
      remote: () => this.env.CODEMODE_SANDBOX.getByName(this.ctx.id.toString()),
    });
    try { return await sandbox.execute(code); } finally { await sandbox.close(); }
  }
}
export default {
  async fetch(request, env) {
    if (request.method !== "POST") return new Response("POST a script\n", { status: 405 });
    const result = await env.SESSION.getByName("demo").runCodemode(await request.text());
    return Response.json(result);
  },
} satisfies ExportedHandler<Env>;
```

**`examples/cloudflare/sandbox/src/index.ts`**：`export { CodemodeSandboxDurableObject as CodemodeSandbox }
from "@earendil-works/kiana-codemode/cloudflare";` + `export default { fetch: () => new Response("Not found",
{ status: 404 }) }`（注释：只经 Durable Object 绑定可达，HTTP 一律 404）。

**`examples/cloudflare/agent/wrangler.jsonc`**：`name: "kiana-codemode-agent-example"`、`main: "src/index.ts"`、
`compatibility_date: "2026-04-22"`、`durable_objects.bindings: [{name:"SESSION", class_name:"Session"},
{name:"CODEMODE_SANDBOX", class_name:"CodemodeSandbox", script_name:"kiana-codemode-sandbox"}]`、
`migrations: [{tag:"v1", new_sqlite_classes:["Session"]}]`；`$schema: "node_modules/wrangler/config-schema.json"`。

**`examples/cloudflare/sandbox/wrangler.jsonc`**：`name: "kiana-codemode-sandbox"`、`main: "src/index.ts"`、
`compatibility_date: "2026-04-22"`、`workers_dev: false`、`preview_urls: false`、
`migrations: [{tag:"v1", new_sqlite_classes:["CodemodeSandbox"]}]`、`limits: {cpu_ms: 60000}`（可选，注释说明
默认 30 秒 CPU/请求，一次执行一个请求，应高于中断预算允许的时间以免对象被重置）。

**`examples/cloudflare/README.md`**：两个 Worker 的分工；本地试跑
`npx wrangler dev -c agent/wrangler.jsonc -c sandbox/wrangler.jsonc` 与
`curl -X POST http://localhost:8787 --data 'const n = await tools.count({ add: 2 }); text("n=" + n); return n'`；
部署顺序（先 sandbox，因 agent 的绑定按名字引用）：`(cd sandbox && npx wrangler deploy)`、
`(cd agent && npx wrangler deploy)`。

#### 2.2.17 `packages/codemode/README.md`（374 行）、`CHANGELOG.md` 与 metadata

README 章节：`Usage on Node and Bun`、`Store`、`Source format`、`Bundled Node and Bun hosts`、
`Usage on Cloudflare Workers`（`Why a separate Worker`、`Deploying the sandbox Worker`、
`Using it from the agent's Worker`、`Limits on Cloudflare`、`On the calling thread`）、`Declarations for the model`、
`Using with kiana-agent-core`、`Deterministic runs`、`Results`、`How it works`、`Runtime notes`。

其中逐字/语义断言（已并入 §2.2.x）：

- 环境对照表：Node/Bun → `CodemodeSandbox`（worker thread per execution）；Cloudflare/DO → `RemoteCodemodeSandbox`
  （`/portable`）；另一种 `InlineCodemodeSandbox` 在调用线程运行。
- 脚本内能力与限制的说明（`tools.<name>`、`ALL_TOOLS`、`text`、`image`、`exit`、`globals`、`store`/`load`、
  "no timers, fetch, process, require, modules, WebAssembly"；`eval`/`Function` 只在 VM 内）。
- `timeoutMs: Infinity` 关闭 deadline；`memoryLimitBytes` 超限为 `InternalError: out of memory`。
- abort/timeout/`close()` **请求** worker 终止但不等待退出；Bun 上不可中断内建会延迟；`terminate()` 内部 ref，
  所以之后 unref；Bun 1.4.0 的 pending `terminate()` 可能延迟宿主进程退出。
- `onWorkerStarted(exited)` 语义：每个 `execute()`/`start()` 各一次，resolve 于真实退出而非结果结算；
  `close()` 不等待这些 promise。
- `acquireWorker()`：wasm 加载之后、worker 创建之前**同步**调用；throw = 拒绝准入且不启动；返回的 release 在
  真实退出或构造失败时调用一次；不得在结果结算时释放。
- `store`：sandbox 不持久化；`options.store` 传入快照；成功结果 `result.storeWrites`（`{set, delete}`）；
  失败无写入。
- `Source format`：`max_output_tokens` / `timeout_ms` 由调用方解释；options 行被替换为空行以保持行号；
  非法输入抛 `CodemodeSourceError`；`CODEMODE_SOURCE_GRAMMAR` 供 grammar-constrained provider。
- `Bundled Node and Bun hosts`：打包时须显式传 `wasm`（`loadQuickJSWasm(path)`，每路径编译一次并缓存）与
  `workerUrl`（URL 或字符串；Bun 编译可执行文件用相对源码路径字符串）。
- `Declarations`：property 描述变 `//` 注释；约束以 `@integer @minimum 1 @default 20` 形式追加；
  超预算先丢 tag；schema 只塑形不校验；本地 `$ref` 展开，递归/远程为 `unknown`。
- `Deterministic runs`：`start()` 固定的五件事（时间、随机、时区、GC、结果顺序）；结果号必须 1,2,… 无缺口；
  同一 fingerprint 的 VM 上可重放；用 `signal` 以脚本无法 catch 的错误结束。
- `Results`：`kind` 表（script/timeout/aborted/sandbox）；输出与调用记录的顺序与 `MAX_OUTPUT_CHARS`/`MAX_OUTPUT_ITEMS`；
  未 await 的调用被 abort 并标为 `cancelled`。
- `How it works`：每次 `execute()` 一个 worker thread（约 20 ms 含 VM 创建），新 wasm 实例；
  唯一 import 是 WASI shim 与一个 host-call 入口；prelude 在 VM 内求值；脚本编译为 async 函数体；
  宿主持有 deadline 与 abort，触发时置共享中断标志再 `terminate()`。
- `Runtime notes`：Node/Bun 支持内存上限、中断、深递归的可 catch `RangeError`；Bun 可能延迟退出；
  栈帧为 QuickJS 格式（`at f (codemode.js:2:31)`），前缀与脚本共享第 1 行，故列号在第 1 行有偏移；
  QuickJS 是解释器，重计算比 V8 慢。

`CHANGELOG.md` 版本：`Unreleased`、`1.4.0`、`1.3.0`、`1.2.0`、`1.1.0`、`1.0.4`、`1.0.3`、`1.0.2`、`1.0.1`、`1.0.0`、
`0.99.2`、`0.99.1`、`0.99.0`（后几个多为空节）。有行为内容的条目（Rust 侧应在 crate CHANGELOG 里保留同等承诺）：

- 1.3.0（**breaking**）：worker 结果与 `close()` 现在在“请求关闭”后即结算，而不是等 worker 真正退出；
  需要真实退出的宿主用 `onWorkerStarted(exited)` 或 `acquireWorker()` 的 release 计数。新增这两个钩子；
  修复 Bun 上 aborts/timeout/`close()` 等待 terminate 的问题（#10748）。
- 1.2.0（**breaking**）：删除 `MCP_TYPESCRIPT_PREAMBLE` 与 `mcpStructuredContentSchema()`；
  `renderToolOutputType()` 不再把 MCP `CallToolResult` schema 变成 `CallToolResult<T>`。新增
  `RemoteCodemodeSandbox`/`CodemodeSandboxDurableObject`/`/cloudflare` 入口、`InlineCodemodeSandbox`、
  `/portable` 与 `CodemodeSandboxBase`、`sandbox.start()`（#10707、#10631、#10725 等）。
- 1.1.0：`console.*` 的输出项带 `console: true`。
- 1.0.4：内建冻结与内建全局只读（#10444）；桥接畸形载荷失败为 `sandbox` 错误而不是崩溃宿主。
- 1.0.1：`MAX_OUTPUT_CHARS`/`MAX_OUTPUT_ITEMS` 输出限额（#10283）。
- 1.0.0：新增 `renderToolOutputType()`；读取不存在的 `tools`/命名空间成员改为抛错。
- 0.99.2：`workerUrl` 可为字符串（#10204）；`image()` 校验 base64 与签名（#10215）。
- 0.99.0：初始 spike。

agent 包的 CHANGELOG 另有更早的 harness 历史（0.83.0-0.99.x：`shouldStopAfterTurn` 被 `finishTurn` 取代、
`prepareRequest`/`peekQueuedMessages` 新增、`prepareNextTurn` 时机变更 #6879、`streamProxy` 保留
`toolcall_end` 元数据 #7709、`BeforeToolCallResult.terminate` #7715、`Agent.reset()` 拒绝运行中重置 #7717、
1.0.0 移除 harness/`./node`/`./harness/*`/`./experimental/pico3` 子路径）；Rust 侧不需要移植这些历史，
但**不能**因为“已移除”而重新引入 harness 概念。

`packages/codemode/package.json`：`name "@earendil-works/kiana-codemode"`、`version 1.4.0`、`type module`、
`main dist/index.js`、`types dist/index.d.ts`、`files ["dist","README.md"]`、`engines.node ">=22.19.0"`、
依赖仅 `quickjs-wasi 3.6.2`；exports 见 §1.3（`.`, `./declarations`, `./source`, `./portable`, `./cloudflare`, `./worker`，
每项都带 `source`（源码运行）、`types`、`import`）。`codemode/vitest.config.ts`：源码别名。


---

## 3. 用户可见表面（逐字）

本单元没有 CLI、TUI、slash command、ANSI 颜色或按键处理。用户可见表面 = 库 API + 逐字错误消息 + 模型可见的
工具描述/声明文本 + 日志 + 被传输/持久化的 JSON。

### 3.1 agent 包：公开 API

```ts
// 值导出（即 packages/agent/src/index.ts 的实际公开面）
export class Agent { … }
export function agentLoop(prompts, context, config, signal: AbortSignal | undefined, streamFn: StreamFn): EventStream<AgentEvent, AgentMessage[]>
export function agentLoopContinue(context, config, signal: AbortSignal | undefined, streamFn: StreamFn): EventStream<AgentEvent, AgentMessage[]>
export function runAgentLoop(prompts, context, config, emit: AgentEventSink, signal: AbortSignal | undefined, streamFn: StreamFn): Promise<AgentMessage[]>
export function runAgentLoopContinue(context, config, emit: AgentEventSink, signal: AbortSignal | undefined, streamFn: StreamFn): Promise<AgentMessage[]>
// 注意：`signal` 与 `streamFn` 在 TS 签名里是**必填**位置参数（不是可选的），只是类型允许 `undefined`。
// 老调用方（编译后的 JS）可以少传最后两个实参：测试用 `Reflect.apply(agentLoop, undefined, [prompts, context, config, undefined])`
// 验证这一点，此时 `streamFn` 为 `undefined`，运行到发请求时走 `streamFn ?? getDefaultStreamFn()`。
export async function runToolCall(toolCall, options: RunToolCallOptions): Promise<AgentToolCallOutcome>
export function streamProxy(model, context, options: ProxyStreamOptions): AssistantMessageEventStream
export function setDefaultStreamFn(fn: StreamFn | undefined): void
export const DEFAULT_AGENT_IDENTITY: ClientIdentity
export type { AgentEventSink, ToolCallHooks, RunToolCallOptions, …所有类型 }
```

**注意**：`getDefaultStreamFn()` **不在**包的公开面（`index.ts` 只显式导出 `setDefaultStreamFn`；`agent-loop.ts` 的
`export *` 也不含它）。它是 `stream-fn.ts` 的模块级导出，仅供包内与测试使用。其错误消息（逐字）：
`No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().`

### 3.2 agent 包：逐字错误消息

| 位置 | 消息 |
|---|---|
| `Agent.reset()` 运行中 | `Agent is already processing. Wait for completion before resetting.` |
| `Agent.prompt()` 运行中 | `Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.` |
| `Agent.continue()` 运行中 | `Agent is already processing. Wait for completion before continuing.` |
| `Agent.continue()` 无消息 | `No messages to continue from` |
| `Agent.continue()` 尾巴是 assistant 且无队列 | `Cannot continue from message role: assistant` |
| `Agent` 内部（listener 逃逸） | `Agent listener invoked outside active run` |
| `Agent` 内部（run 冲突） | `Agent is already processing.` |
| `agentLoopContinue()` 无消息（同步抛） | `Cannot continue: no messages in context` |
| `agentLoopContinue()` 末条 assistant（同步抛） | `Cannot continue from message role: assistant` |
| 工具不存在 | `Tool ${toolCall.name} not found` |
| 执行中被 abort | `Operation aborted` |
| beforeToolCall 返回 `block` 但无 reason | `Tool execution was blocked` |
| 响应被输出 token 限制截断 | `Tool call "${name}" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.` |
| proxy 非 2xx（无 JSON error 字段） | `Proxy error: ${status} ${statusText}` |
| proxy 非 2xx（有 JSON error 字段） | `Proxy error: ${data.error}` |
| proxy 被 abort | `Request aborted by user` |
| proxy 流提前 EOF | `Connection closed by proxy server before the response completed` |
| proxy 事件类型不匹配 | `Received text_delta for non-text content` / `Received text_end for non-text content` / `Received thinking_delta for non-thinking content` / `Received thinking_end for non-thinking content` / `Received toolcall_delta for non-toolCall content` |
| proxy 未知事件（仅 warn，不抛） | `Unhandled proxy event type: ${type}` |

工具错误结果统一形状：`{ content: [{ type: "text", text: message }], details: {} }`。
`Agent` 的 run 失败（streamFn 抛错或 `prompt` 期间异常）会合成一条 assistant 消息：
`{ role: "assistant", content: [{ type: "text", text: "" }], api: state.model.api, provider: state.model.provider,
model: state.model.id, usage: EMPTY_USAGE, stopReason: aborted ? "aborted" : "error", errorMessage: <message>,
timestamp: Date.now() }`；随后依次发 `message_start`、`message_end`、`turn_end {message, toolResults: []}`、
`agent_end {messages: [failureMessage]}`。

### 3.3 codemode 包：公开 API（与 §1.3/§2.2.15 配合阅读）

**公开入口导出的符号**（Rust 侧 `pub use` 的等价集合；逐条语义见 §2.2）：

```ts
// 类
export class CodemodeSandbox extends CodemodeSandboxBase              // Node/Bun worker（主入口）
export class InlineCodemodeSandbox extends CodemodeSandboxBase        // 调用线程（主入口 + /portable）
export class RemoteCodemodeSandbox extends CodemodeSandboxBase        // 远端 transport（主入口 + /portable）
export class CodemodeSandboxBase                                      // 基类（主入口 + /portable）
export class CodemodeSandboxDurableObject<Env>                        // 仅 /cloudflare
export class CodemodeSourceError extends Error                        // 仅主入口 + /source

// 函数
export function createInlineCodemodeSandbox(options?)                 // 仅 /cloudflare
export function serveCodemodeRemote(request, exchange, options)       // 主入口 + /portable
export function loadQuickJSWasm(path?)                                // 仅主入口
export function renderDeclarations(options)                           // 主入口 + /declarations
export function renderToolSignature(tool, options?)                   // 主入口 + /declarations
export function renderToolSample(tool, options?)                      // 主入口 + /declarations
export function renderToolOutputType(schema)                          // 主入口 + /declarations
export function schemaToType(schema, options?)                        // 主入口 + /declarations
export function parseCodemodeSource(input)                            // 主入口 + /source
export function toCodemodeIdentifier(name)                            // 主入口 + /declarations + /portable

// 常量
export const MAX_STORE_VALUE_CHARS = 262144        // 主入口 + /portable
export const MAX_STORE_TOTAL_CHARS = 1048576       // 主入口 + /portable
export const MAX_OUTPUT_CHARS = 16777216           // 主入口 + /portable
export const MAX_OUTPUT_ITEMS = 100000             // 主入口 + /portable
export const DEFAULT_INTERRUPT_BUDGET = 100000     // 主入口 + /portable
export const DEFAULT_INPUT_SCHEMA_MAX_CHARS = 16000// 主入口 + /declarations
export const CODEMODE_OPTIONS_PREFIX = "// @options:"  // 主入口 + /source
export const CODEMODE_SOURCE_GRAMMAR: string       // 主入口 + /source
export const DEFAULT_REMOTE_MEMORY_LIMIT_BYTES = 33554432  // 仅 /cloudflare
export const cloudflareQuickJSWasm: CodemodeWasmModule     // 仅 /cloudflare
```

**仅源码可见（`src/` 内部导出，供测试与其他模块使用；Rust 侧可 `pub` 但不算公开 API）**：
`startVm`/`VmSession`/`StartVmOptions`（`runtime/vm.ts`）、`inlineLauncher`/`INLINE_MAX_STACK_SIZE`（`runtime/inline.ts`）、
`errorMessage`/`VmEvents`/`VmChannel`/`VmLauncher`（`runtime/execution.ts`）、`PRELUDE_SOURCE`（`runtime/prelude-source.ts`）、
`isWorkerToHostMessage`/`isHostToWorkerMessage`/`WorkerData`/`ScriptErrorJson`（`runtime/protocol.ts`）、
`CodemodeSandboxDurableObject` 的 `maxInterruptBudget`/`maxMemoryLimitBytes` 受保护字段。

### 3.4 codemode 包：逐字错误消息

**宿主侧（抛出的 Error / CodemodeError.message）**

| 位置 | 消息 |
|---|---|
| `CodemodeSandboxBase.execute` 已关闭 | `Sandbox is closed` |
| `start` 已关闭 | `Sandbox is closed`（同步抛） |
| `start` startedAt 非法 | `startedAt must be a non-negative number of milliseconds since the epoch` |
| `start` seed 非法 | `seed must be a 32-bit unsigned integer` |
| `start` 工具名重复 | `Tool names must be unique` |
| `start` 工具脚本名重复 | `Tool script names must be unique` |
| `registerTool` 重复 | `Tool "${tool.name}" is already registered` |
| `checkGlobals` 非法名 | `Invalid global name "${name}"` |
| `checkGlobals` 重名 | `Global "${name}" is already registered` |
| `checkGlobals` 命名空间冲突 | `Global "${name}" conflicts with the namespace "${name}"` |
| 工具未注册（execute 路径） | `Unknown tool "${name}"` |
| global 未注册 | `Unknown global "${name}"` |
| 超时 | `Execution timed out after ${timeoutMs} ms` |
| 中断预算耗尽（inline） | `Execution exceeded its interrupt budget of ${budget}` |
| abort 且 reason 非 Error | `Execution aborted` |
| close | `Sandbox closed` |
| 桥接损坏 | `Sandbox bridge broken: ${msg}. The script may have modified built-ins such as a prototype's toJSON.` |
| 宿主处理失败 | `Sandbox host failed: ${msg}` |
| 桥接 JSON 错误 | `return value is not valid JSON` / `call arguments is not valid JSON` / `store value for "k" is not valid JSON` / `store writes are not an array` / `store writes contain a malformed entry` / `script error is not an object` / `script error is malformed` / `unknown message from the worker` / `duplicate call id ${id}` |
| 确定性交付校验 | `The clock value of result ${seq} is not a finite number` / `Result number ${seq} is not a positive integer used once` / `Call ${id} is not waiting for a result` / `The properties of result ${seq} are not an object` |
| VM 序号校验 | `Result ${id} has sequence number ${seq}, expected ${nextSeq} or later, once` |
| 微任务失败 | `Promise job failed` |
| 执行被中断 | `Execution interrupted` |
| worker 加载 wasm 失败 | `Failed to load QuickJS: ${message}` |
| worker 构造失败 | `Failed to start worker: ${message}` |
| worker 提前退出 | `Worker exited with code ${code} before the script settled` |
| 远端不可达 | `Failed to reach the remote sandbox: ${message}` |
| 远端抛错 | `Remote sandbox failed: ${message}` |
| 远端提前返回 | `Remote sandbox returned before the script settled` |

**脚本侧（`CodemodeError{kind:"script"}` 或被脚本 catch 到的 Error）**

- 与 store 相关、与输出限制相关、与 image 相关的全部文本：见 §2.2.7；`store` 相关错误是 `TypeError`/`RangeError`，
  `image` 相关一律 `TypeError`，输出超限是 `RangeError`。
- 脚本语法/运行错误：`{kind:"script", name, message, stack}`，`stack` 形如
  `TypeError: boom 1\n    at <函数名> (codemode.js:2:31)`（QuickJS 帧），且**不含** `codemode-prelude.js` 帧。
- 非 Error 抛出：`message` 为 `format(value)`（如 `{"code":7}`），`name` 缺省。
- 非 JSON 可序列化返回值：`{kind:"script", name:"TypeError", message: …}`（V8 风格 `TypeError: Converting circular
  structure to JSON` 或 QuickJS 等价文本；见 §6 差异与 §8 最难点）。
- tools 上读取不存在的成员：`TypeError: tools.${property} does not exist. … Check for a member with "${property}" in tools.`
  含建议 `Did you mean tools.a, tools.b?`（最多 5 个）或 `Available: a, b, c.`（成员 ≤ 20 时），以及提示
  `ALL_TOOLS lists every tool; searchTools(query) finds tools by topic.`
- 命名空间上读取不存在的成员：`TypeError: ${namespace}.${property} does not exist. … Check for a member with
  "${property}" in ${namespace}.`
- `stalled`：`Error: The script is waiting on a promise that can never settle: no tool call is pending, and timers
  do not exist here.`

### 3.5 模型可见的渲染文本（declarations）

`renderDeclarations({tools, globals})` 输出示例（逐字，来自测试）：

```ts
declare const tools: {
  /** Read a file */
  read(args: {
    path: string;
  }): Promise<string>;
};
```

规范细节见 §2.2.4。关键不变量：

- 工具声明缩进 2 空格；全局函数/命名空间平铺（无缩进）。
- 工具名经 `toCodemodeIdentifier` 归一化。
- 属性名按字典序排序（`Object.keys(...).sort()`，即 UTF-16 码元序）。
- 有注释（description 或 constraint tag）的对象 → 多行；否则单行 `{ a: string; b?: number; }`。
- 空对象：有 `additionalProperties===false` 或无属性 → `{}`；无 additionalProperties 且无属性 → `{ [key: string]: unknown; }`。
- 类型超预算（默认 16000 UTF-16 单位，仅 `renderToolSignature`/`renderToolSample` 传）先去掉 tags，
  再不行 → `unknown`。
- tags 顺序固定：`@integer` → 按 `TAG_KEYWORDS` 顺序（minimum, exclusiveMinimum, maximum, exclusiveMaximum,
  multipleOf, minLength, maxLength, pattern, format, minItems, maxItems）→ `@uniqueItems` → `@default …` → `@deprecated`；
  每个 tag ≤ 80 字符，换行/段落分隔符转义为 `\uXXXX`。
- 本地 `$ref` 展开最多 32 次，递归 → `unknown`，远程 ref → `unknown`。
- `*/` 在 description 中写成 `*\/`。
- 节之间用空行（`"\n\n"`）分隔；无 tools 且无 globals → `""`。

`renderToolSample(tool)` 逐字模板：

```
${description?.trim() ?? ""}

codemode tool declaration:
```ts
declare const tools: { ${renderToolSignature(tool)} };
```
```

`renderToolSignature` 逐字模板：`${toCodemodeIdentifier(name)}(args: ${input}): Promise<${outputType}>;`

### 3.6 日志

- `agent/src/proxy.ts`：未知 proxy 事件 → `console.warn(`Unhandled proxy event type: ${type}`)`。
- 其余包内无 `console.*` 输出（codemode 的 VM stdout/stderr 被 `discardOutput` 丢弃；worker 不打印）。

### 3.7 模型/宿主可见的 codemode 工具描述（agent example，逐字）

`createCodemodeTool` 的 `description` 是一个 `.join("\n")` 的 7 元素数组；Rust 侧应逐字复刻为
同一个 7 行字符串（第 6 行是空行，第 7 行 `\`\`\`ts`，第 8 行是 `renderDeclarations(...)` 的输出，
第 9 行 `\`\`\``）。元素列表（逐字，顺序即数组顺序）：

| # | 元素 |
|---|---|
| 1 | `Run JavaScript that calls other tools, to chain calls, run them in parallel, or filter large results.` |
| 2 | `The code is the body of an async function. There is no Node, file system, network, or timers.` |
| 3 | `` Call tools as `await tools.<name>(args)`. Output with `text(value)` or `console.log()`, or `return` a value. `` |
| 4 | `Only the output and the return value reach you, not the results of the tool calls.` |
| 5 | ``（空字符串）`` |
| 6 | `` ```ts `` |
| 7 | `renderDeclarations({ tools: sandboxTools })` 的返回值（多行） |
| 8 | `` ``` `` |

（上表编号即 8 项；数组字面量在源码中为 7 个字符串 + 1 个 `renderDeclarations(...)` 表达式。）

拼接结果（`\n` 换行；最后一行之后**没有**额外换行）：

<pre>
Run JavaScript that calls other tools, to chain calls, run them in parallel, or filter large results.
The code is the body of an async function. There is no Node, file system, network, or timers.
Call tools as `await tools.&lt;name&gt;(args)`. Output with `text(value)` or `console.log()`, or `return` a value.
Only the output and the return value reach you, not the results of the tool calls.

```ts
&lt;renderDeclarations({ tools: sandboxTools }) 的输出&gt;
```
</pre>

`name: "codemode"`、`label: "Codemode"`、参数 schema `{code: string}`（描述
`JavaScript source. Top-level await and return work.`）、`timeoutMs: 120_000`。

---

## 4. 外部格式

本单元**没有** CLI 参数、环境变量、磁盘文件格式、数据库 schema。外部格式有：HTTP/SSE（proxy）、
worker_threads 消息（worker 后端）、Workers RPC 消息（remote 后端）、以及 codemode 源码文本与声明文本。

### 4.1 HTTP：agent proxy

**请求**

```
POST ${proxyUrl}/api/stream
Authorization: Bearer ${authToken}
Content-Type: application/json
```

body（`JSON.stringify`，键顺序如下，`undefined` 字段被 JSON.stringify 省略）：

```json
{
  "model": { …kiana-ai Model 对象原样… },
  "context": { "messages": [ …TranscriptContext… ] },
  "options": {
    "temperature": …,
    "samplingParams": …,
    "maxTokens": …,
    "reasoning": …,
    "cacheRetention": …,
    "sessionId": …,
    "headers": …,
    "metadata": …,
    "transport": …,
    "thinkingBudgets": …,
    "maxRetryDelayMs": …
  }
}
```

- `context` 是已归一化的 `TranscriptContext`（system prompt 与 tools 在 transcript 的 system 消息里）。
- `signal` 传给 `fetch`；abort 时调用 `reader.cancel("Request aborted by user")`。
- 非 2xx：先尝试 `response.json()` 读 `{error}`；成功且 `error` 为真值 → `Proxy error: ${error}`，
  否则 `Proxy error: ${status} ${statusText}`。

**响应（SSE-ish 行流）**

- 按 UTF-8 `TextDecoder({stream:true})` 解码；按 `"\n"` 切行，最后一段留在 buffer；
  只有以 `"data: "` 开头的行被处理；`line.slice(6).trim()` 后空串跳过；`JSON.parse` 得 `ProxyAssistantMessageEvent`。
- 行内 `\r`（CRLF）由 `.trim()` 去掉。
- 流的最后一帧可能**不以换行结尾**：EOF 后 `decoder.decode()` 再处理残余 buffer。
- 无 `done`/`error` 终态事件就 EOF → 合成 `{type:"error", reason:"error", error:{…stopReason:"error",
  errorMessage:"Connection closed by proxy server before the response completed"}}`。

事件形状（服务端 → 客户端，`partial` 字段已被服务端剥离）：

```ts
| { type: "start" }
| { type: "text_start"; contentIndex: number }
| { type: "text_delta"; contentIndex: number; delta: string }
| { type: "text_end"; contentIndex: number; contentSignature?: string }
| { type: "thinking_start"; contentIndex: number }
| { type: "thinking_delta"; contentIndex: number; delta: string }
| { type: "thinking_end"; contentIndex: number; contentSignature?: string }
| { type: "toolcall_start"; contentIndex: number; id: string; toolName: string }
| { type: "toolcall_delta"; contentIndex: number; delta: string }
| { type: "toolcall_end"; contentIndex: number; toolCall: ToolCall }
| { type: "done"; reason: "stop"|"length"|"toolUse"; usage: Usage; providerThinkingLevel?: string }
| { type: "error"; reason: "aborted"|"error"; errorMessage?: string; usage: Usage; providerThinkingLevel?: string }
```

客户端重建规则：`text_start` 用**下标赋值**占位 `{type:"text",text:""}`；`*_delta` 追加到对应块（类型不符 → 错误）；
`text_end`/`thinking_end` 写回 `contentSignature`/`thinkingSignature`（可能为 `undefined`，键仍被赋值）；
`toolcall_start` 建 `{type:"toolCall", id, name, arguments:{}}` 并创建 `ToolCallArgumentsStream`；
`toolcall_delta` 用 `args.append(delta)` 生成**新块对象**写回下标；`toolcall_end` 用 `Object.assign(content, toolCall)`
覆盖（保留客户端已有字段），删除参数流，返回 `toolcall_end`；若下标处不是 toolCall → 返回 `undefined`（不推事件）。
`done`/`error` 更新 `stopReason`/`usage`/`errorMessage`/`providerThinkingLevel`（仅当非 undefined）。

初始 partial：

```ts
{ role: "assistant", stopReason: "pending", content: [], api: model.api, provider: model.provider,
  model: model.id, usage: {input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,
  cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}}, timestamp: Date.now() }
```

### 4.2 worker_threads 消息协议（Node/Bun 后端）

- worker 入口 `runtime/worker.ts`；`workerData` = `WorkerData`（可结构化克隆：`wasm` 为 `WebAssembly.Module`，
  `interrupt` 为 `SharedArrayBuffer(4)`）。
- 方向 host → worker：`HostToWorkerMessage`（`{type:"result", id, ok, payload?, seq?, now?, properties?}`）。
- 方向 worker → host：`WorkerToHostMessage`（`call`/`output`/`done`/`crash`/`started`/`idle`，见 §2.2.6）。
- 中断：`Atomics.store(new Int32Array(interrupt), 0, 1)`；worker 侧 `Atomics.load(...) !== 0` → 中断。
- `stop()`：置中断 → `worker.terminate()` → `worker.unref()`；不等待 exit。
- Node `--watch` 消息过滤：若消息是对象且 `"watch:import" in message || "watch:require" in message`
  （**`in`，即包括原型链上的键**，不是 `hasOwnProperty`）→ 静默丢弃，不交给 `events.message`。
- 测试用的 raw worker fixture 直接 `parentPort.postMessage(JSON.parse(code))`。

### 4.3 Workers RPC 消息协议（Cloudflare/remote 后端）

```ts
interface CodemodeRemoteRequest { data: VmStartData; interruptBudget?: number }
type CodemodeRemoteMessage = WorkerToHostMessage | { type: "failure"; error: CodemodeError };
type CodemodeRemoteReply = HostToWorkerMessage | { type: "stop" } | null;
type CodemodeRemoteExchange = (messages: CodemodeRemoteMessage[]) => Promise<CodemodeRemoteReply>;
interface CodemodeRemote { execute(request: CodemodeRemoteRequest, exchange: CodemodeRemoteExchange): Promise<void> }
```

- 批处理：`output` 入 buffer，`bufferedChars += item.type === "text" ? item.text.length : item.data.length`，
  达到 `>= 1000` 项或 `>= 1 Mi` 字符时 `exchange(takeBuffer())`；**`started` 只入 buffer**（不立即 flush）；
  `call`/`idle` 立即 `exchange([...takeBuffer(), message])`（因此它们之前的输出总是先送达）；
  其余类型（`done`/`crash`）→ `end(typed)`，作为终态随最后一次 flush。
- 每次 exchange 的 reply：`result` → 投递给 VM；`stop` → 结束；`null` → 仅确认。
- 结束语义：`end(terminal?)` 幂等；先发最后一批（含 terminal），等 exchange resolve 后 `channel.stop()`，再 resolve。
- 所有消息必须是结构化可克隆的 JSON 值（`CodemodeError` 是普通对象）。
- Durable Object 类名/入口：`CodemodeSandboxDurableObject`（可在 sandbox Worker 里用
  `export { CodemodeSandboxDurableObject as CodemodeSandbox }` 导出）；绑定 `script_name` + `class_name`。
- 服务端限制：`maxInterruptBudget`（默认 100000）、`maxMemoryLimitBytes`（默认 32 MiB），请求只能被降到上限。

### 4.4 codemode 源码文本格式

```
[<空白>// @options: <JSON 对象>]\n
<JavaScript 源码，作为 async 函数体>
```

- 第一行**可以**是 options 行（前导空白/制表符允许）；否则整段都是代码。
- options 支持的键只有 `max_output_tokens`（≥0 安全整数）和 `timeout_ms`（1..2147483647）；JSON 对象，不接受数组/null。
- 除第一行外任何一行以 `// @options:` 开头 → 错误（防止被忽略的超时）。
- options 行被替换为一个空行（保留行号）。

### 4.5 无环境变量

两个包的 `src/` 都不读 `process.env`（`examples/` 也不读；示例只要求 shell 里存在 `ANTHROPIC_API_KEY`，
由 kiana-ai 的 anthropic provider 自己读取）。`process.env` 的实际使用点全部在**测试与构建配置**里：

- `packages/agent/vitest.config.ts:14` 与 `packages/codemode/vitest.config.ts:7`：
  `reporters: process.env.GITHUB_ACTIONS ? ["dot", "github-actions"] : ["dot"]`（CI 追加 reporter，不影响行为）。
- `packages/codemode/test/deterministic.test.ts:100-103`：`const savedTimezone = process.env.TZ;`（模块级取值），
  `afterEach` 里 `savedTimezone === undefined ? delete process.env.TZ : process.env.TZ = savedTimezone`；
  `:149` 的 UTC 用例在**测试体内**把 `process.env.TZ = "Asia/Kolkata"`。这是唯一影响被测行为的变量。
- 其他环境变量（如 `KIANA_CODEMODE_MAX_WORKERS`）由**消费方** coding-agent 读取，不在本单元范围内（见 §5.5）。

---

## 5. 跨单元接口

### 5.1 agent → kiana-ai（`@earendil-works/kiana-ai`，版本 1.4.0）

**值导入**（行为必须完全一致；Rust 侧应直接调用同一 `kiana_ai` crate 的对应函数）：

| 符号 | 语义（agent 依赖的部分） |
|---|---|
| `EventStream<T,R>` | 异步可迭代事件流；`push`/`end`/`result(): Promise<R>`；消费者 `for await`。精确语义：`push(event)` 在 `done` 后**静默丢弃**（不排队、不报错）；若 `isComplete(event)` 为真则先置 `done` 并 `resolveFinalResult(extractResult(event))`，事件本身**仍会被投递**（消费者收得到终止事件）；`end(result?)` 置 `done` 并唤醒所有等待者，但**只有 `result !== undefined` 时才 resolve `result()`**——因此“流正常结束但没人 push 终止事件、且 `end()` 无参”时 `result()` 永久 pending（agent 依赖这一点保证 `streamProxy` 的 EOF 错误合成路径）。内部是 FIFO（两栈实现），`result()` 是单次 Promise，多次调用返回同一 Promise。 |
| `createAssistantMessageEventStream()` | `AssistantMessageEventStream extends EventStream<AssistantMessageEvent, AssistantMessage>`：`isComplete = (e) => e.type === "done" || e.type === "error"`；`extractResult` 对 `done` 取 `message`、对 `error` 取 `error`，**其他类型抛 `Unexpected event type for final result`**。`durationMs` 规则：构造时记 `#startedAt = Date.now()` 与 `#startedAtMonotonic = performance.now()`；在 `push(done)`/`push(error)` 以及 `end(result)`（仅当 `result !== undefined`）时对最终消息调用 `#time`，条件是 `!this.done && message.durationMs === undefined && message.timestamp >= #startedAt`，值为 `Math.max(0, Math.round(performance.now() - #startedAtMonotonic))`。三条后果必须复刻：(a) 已带 `durationMs` 的消息不覆盖；(b) `timestamp` 早于流创建时间的消息（延迟取回的历史响应、replay）不打时间戳；(c) `#time` 在 `super.push`/`super.end` 之前执行，所以流已被 `done` 事件结束后再 `end(result)` 传进来的 result 不会被打点。 |
| `normalizeContext(context)` | 返回 `{messages}`：若 `context.systemPrompt` 有内容或 `context.tools` 非空，则前置一条 `SystemMessage`。 |
| `createInitialSystemMessage(systemPrompt, tools)` | `hasSystemPrompt = systemPrompt !== undefined && systemPrompt.length > 0`；`hasTools = tools !== undefined && tools.length > 0`；两者皆假 → `undefined`；否则返回 `{role:"system", content: systemPrompt ?? "", ...(hasTools ? {toolsAdded: tools} : {}), timestamp: 0}`。**字段是 `toolsAdded` 不是 `tools`**；`timestamp` 恒为 `0`；空字符串 prompt 视作无 prompt（但仍会因 tools 存在而生成消息，此时 `content: ""`）。 |
| `getCurrentSystemMessage(messages)` | 重放 system 消息（content 追加、sections 命名替换、toolsAdded/toolsRemoved 合并）。 |
| `getCurrentSystemPrompt(messages)` | `const m = getCurrentSystemMessage(messages); return m ? getSystemMessageText(m) : ""`；`getSystemMessageText` = `[contentText(content), ...Object.values(sections ?? {}).filter(t => t !== null)].filter(part => part.length > 0).join("\n\n")`——**不只是 `contentText`：sections 也会按插入序追加，用双换行连接，空片段被丢弃**。无 system 消息时返回 `""`（不是 `undefined`）。 |
| `getCurrentTools(messages)` | 当前声明工具。 |
| `getToolStateChanges(previous, current)` | 返回 `ToolStateChanges { toolsAdded: Tool[]; toolsRemoved: ToolReference[] }`（**只有这两个字段，没有 `added`/`removed`/`changed`**）。同名工具按 `declarationsEqual` 比较：`toolsAdded` = `current` 中"previous 无同名 或 声明不等"的工具（经 `toToolDeclaration` 归一）；`toolsRemoved` = `previous` 中"current 无同名 或 声明不等"的工具，但只保留 `{name}`（`ToolReference`）。**"定义变化" 同时出现在两个列表里（先删后加），这是刻意设计**。`declarationsEqual(a,b)` = `JSON.stringify(toToolDeclaration(a)) === JSON.stringify(toToolDeclaration(b))`（键序敏感，见 §6.1）。 |
| `toToolDeclaration(tool)` | 只复制 `{name, description, parameters: JSON.parse(JSON.stringify(tool.parameters)), ...(tool.constrainedSampling === undefined ? {} : {constrainedSampling: tool.constrainedSampling})}`。**不包含 `outputSchema`/`execute`/`label`**：JSON 往返同时剥掉 schema 上的 symbol/函数与 `undefined`。`parameters` 用 JSON 往返深拷贝（不是 `structuredClone`），因此 `undefined` 值字段被丢弃、`NaN`/`Infinity` 变 `null`、函数/`symbol` 键被丢弃。 |
| `mergeClientIdentity(...sources)` | 逐字段合并；`attribution: null` 表示禁用。 |
| `KIANA_ATTRIBUTION` | `{name:"kiana", url:"https://kiana.dev", categories:["cli-agent"]}`。 |
| `validateToolArguments(tool, toolCall)` | 见下。 |
| `ToolCallArgumentsStream` | 增量 JSON 参数流：`append(delta): Record<string, unknown>`、`replace?`、`finish`；线性时间。 |

**逐文件的实际 import 清单（逐字，`type` 前缀表示仅类型）**：

| 文件 | 导入 |
|---|---|
| `agent-loop.ts` | `type AssistantMessage`、`type ClientIdentity`、`EventStream`、`getCurrentTools`、`getToolStateChanges`、`mergeClientIdentity`、`normalizeContext`、`KIANA_ATTRIBUTION`、`type SystemMessage`、`type ToolResultMessage`、`type ToolStateChanges`、`toToolDeclaration`、`validateToolArguments` |
| `agent.ts` | `type ClientIdentity`、`createInitialSystemMessage`、`getCurrentSystemMessage`、`getCurrentSystemPrompt`、`type ImageContent`、`type Message`、`type Model`、`type SimpleStreamOptions`、`type TextContent`、`type ThinkingBudgets`、`type Transport`、`toToolDeclaration` |
| `proxy.ts` | `type AssistantMessage`、`type AssistantMessageEvent`、`type AssistantMessageEventStream`、`createAssistantMessageEventStream`、`type Model`、`type SimpleStreamOptions`、`type StopReason`、`type ToolCall`、`ToolCallArgumentsStream`、`type TranscriptContext` |
| `types.ts` | 仅类型：`Api`、`AssistantMessage`、`AssistantMessageEvent`、`AssistantMessageEventStream`、`ImageContent`、`JsonValue`、`Message`、`Model`、`SimpleStreamOptions`、`TextContent`、`Tool`、`ToolResultMessage`、`TranscriptContext`、`Usage` |

即 agent 只依赖 kiana-ai 的 **13 个值符号**（下表第一张表列出的全部符号，逐个：
`EventStream`、`createAssistantMessageEventStream`、`normalizeContext`、`createInitialSystemMessage`、
`getCurrentSystemMessage`、`getCurrentSystemPrompt`、`getCurrentTools`、`getToolStateChanges`、
`toToolDeclaration`、`mergeClientIdentity`、`KIANA_ATTRIBUTION`、`validateToolArguments`、`ToolCallArgumentsStream`）
与第二张表列出的全部类型。
**注意**：agent **不**直接使用 `contentText`/`getSystemMessageText`/`renderSystemMessageUpdate`/
`resolveTranscript`/`collapseSystemMessages`/`withClientIdentity`/`getIdentityHeaders` —— 那些由 kiana-ai 内部
（`getCurrentSystemMessage`/`getCurrentSystemPrompt`/`normalizeContext` 的实现）或 coding-agent 使用。

`validateToolArguments` 的准确契约（kiana-ai 提供，agent 只调用）：
1. `toolCall.argumentsError` 存在 → 抛 `Invalid arguments for tool "${name}": ${argumentsError}`。
2. `structuredClone(arguments)`；`normalizeOptionalNulls`（把可选字段的 `null` 归一为删除/`undefined`）；
   `Value.Convert(schema, args)`（TypeBox 强转，如 `"5"`→5）。
3. 非 TypeBox schema（无 symbol kind）时先 `coerceWithJsonSchema`；`validator.Check`。
4. 失败 → 抛带路径列表的错误：
   `Validation failed for tool "<name>":\n  - path: message\n\nReceived arguments:\n<JSON>`。

**`StreamFn` 契约**（Rust 侧必须保留）：`(model, context: TranscriptContext, options?: SimpleStreamOptions) =>
AssistantMessageEventStream`；**不得抛错**；返回后所有失败都编码为 `stopReason: "error" | "aborted"` 的 assistant 消息
（通过 `error` 事件）。`SimpleStreamOptions` 关键字段：`temperature`、`samplingParams`、`maxTokens`、`reasoning`
（`ThinkingLevel`）、`cacheRetention`、`sessionId`、`headers`、`metadata`、`transport`、`thinkingBudgets`、
`maxRetryDelayMs`、`toolChoice`、`deferred`、`onPayload`、`onResponse`、`onProviderStreamEvent`、`apiKey`、`signal`、
`identity`。

### 5.2 agent → typebox（`typebox` 1.3.27）

- 仅类型层面：`Static<TSchema>`、`TSchema`（`AgentTool.parameters: TSchema`）。
- 运行时由 kiana-ai 的 `validateToolArguments` 使用 TypeBox 的 `Value.Convert`/validator；agent 自身不直接调用。
- 测试与 examples 用 `Type.Object/Type.String/Type.Optional` 构造 schema。

### 5.3 agent → mcp（仅 examples）

`examples/mcp-codemode` 依赖 `@earendil-works/kiana-mcp`（`Client`、`callTool`、`listTools`、`onProgress`）与
`@earendil-works/kiana-ai`（models）。该示例不是库 API 的一部分。

### 5.4 codemode → 外部

- 运行时依赖仅 `quickjs-wasi@3.6.2`（`quickjs.wasm` 与 QuickJS API 绑定）。
- Node 内建：`node:worker_threads`、`node:fs/promises`、`node:module`（`createRequire`）、`node:url`。
- 全局：`fetch` 无；`WebAssembly`、`SharedArrayBuffer`、`Atomics`、`AbortController`、`performance`、
  `queueMicrotask`、`structuredClone`（仅测试/transport）。
- Cloudflare：`cloudflare:workers`（`DurableObject`）与 Wrangler 的 `CompiledWasm` 导入。
- **无 kiana 依赖**（README 明确）。

### 5.5 消费者（其他单元对本单元的依赖）

- `coding-agent/src/extensions/codemode/tool.ts` / `execute.ts` / `worker.ts`：从主入口取
  `CodemodeSandbox`、`loadQuickJSWasm`、`parseCodemodeSource`、`renderToolSample`、`CodemodeOutputItem`/
  `CodemodeResult`/`CodemodeTool`/`CodemodeJsonSchema`；从 `/declarations` 取 `renderToolOutputType`、
  `renderToolSample`、`toCodemodeIdentifier`；从 `/source` 取 `CODEMODE_SOURCE_GRAMMAR`。
  它自己管理 worker 槽位（`KIANA_CODEMODE_MAX_WORKERS`，非法值忽略、默认值见该单元）与内存上限（256 MiB），
  并**不**直接用 `renderDeclarations`（只用其更窄的三个导出）。`worker.ts` 仅 `import "@earendil-works/kiana-codemode/worker"`
  （副作用导入，用于打包时把 worker 入口做成独立 entrypoint——Rust 侧对应"worker 启动二进制/线程函数"的独立产物）。
- `durable/src/codemode/{codemode,describe,output}.ts`：从 `/portable` 取 `CodemodeSandboxBase`、`CodemodeRun`、
  `CodemodeHostCall`、`CodemodeOutcome`、`CodemodeResult`、`toCodemodeIdentifier`；从 `/source` 取
  `CODEMODE_SOURCE_GRAMMAR`、`parseCodemodeSource`；从 `/declarations` 取 `renderToolSample`、
  `toCodemodeIdentifier`。`start()`/`deliver`/`onCall`/`onIdle` 用于确定性重放与 journal。
- 其它包消费 `Agent`、`agentLoop`、`runToolCall`、`AgentTool`、`AgentEvent`。

---

## 6. npm 依赖与 Rust crate 映射

| npm 包 | 用途 | 建议 Rust | 需桥接的行为差异 |
|---|---|---|---|
| `@earendil-works/kiana-ai` 1.4.0 | 消息/类型/流/校验/身份 | 内部 crate `kiana_ai`（本仓库其他单元） | 类型一一对应；`AssistantMessageEvent` 的 `partial` 语义（Rust 需 `Arc`/克隆而非同引用）；`EventStream` 的 `result` 与 `durationMs`；`structuredClone` → 深拷贝。 |
| `typebox` 1.3.27 | JSON Schema + 编译期类型 + `Value.Convert` | `serde_json::Value` + `jsonschema` crate（或自写校验器）+ `schemars`（生成 schema） | TypeBox 的 `Value.Convert` 强转规则（字符串→数字/布尔、数字→字符串等）需逐条复刻；`JSON.stringify` 数值格式；错误路径文本 `  - path: msg`。 |
| `quickjs-wasi` 3.6.2 | QuickJS 引擎（wasm） | `rquickjs` + `quickjs-ng`/`quickjs`（或 `quickjs-wasi` 的 wasm 通过 `wasmtime`/`wasmer` 嵌入） | 见 §8：错误文本、栈帧格式、`versions` 字符串、`_getExports()`（`qjs_is_job_pending`/`qjs_execute_pending_job`）、`Intrinsics` 掩码、WASI shim、内存上限、interrupt handler、`maxStackSize`。 |
| `node:worker_threads` | 每执行一个 worker | `std::thread` + `crossbeam-channel`，或 `tokio::task`（CPU 密集用 `spawn_blocking`） | `terminate()` 不保证停止 wasm 内循环 → 需要协作中断标志（AtomicBool）与"不等待退出"的语义；`unref()` 无直接等价（detach）。 |
| `SharedArrayBuffer` + `Atomics` | 中断标志 | `Arc<AtomicI32>` | 语义直接对应。 |
| `node:fs/promises` / `node:module` | 定位并读取 `quickjs.wasm` | `std::fs::read` + `include_bytes!` 或运行时路径 | 每路径缓存、失败不缓存。 |
| `WebAssembly.compile` / `CompiledWasm` | 编译 wasm | `wasmtime::Module`（不可序列化 → 用 `Arc<Module>`） | `CodemodeWasmModule` 是"不透明对象"，Rust 侧用 `Arc<dyn Any + Send + Sync>` 或泛型。 |
| `fetch` + `ReadableStream`（proxy） | SSE 客户端 | `reqwest`（rustls）+ `futures::StreamExt` + `bytes` | 行分割/`data: ` 解析/无换行尾帧/EOF 语义逐条复刻；`TextDecoder` 的流式 UTF-8 需允许跨 chunk 的多字节字符。 |
| `AbortSignal` | 取消 | `tokio_util::sync::CancellationToken` + 自定义 `AbortSignal` trait | `signal.reason`（Error 或任意值）、`{once:true}`、`addEventListener` 语义、已 abort 时立即回调。 |
| `performance.now()` | 时长与 VM 时钟 | `std::time::Instant` | 单调；`Math.round` 取整；Rust f64 与 JS number 均为 IEEE-754 double。 |
| `Date` / `toLocaleString`（测试工具） | 时间工具 | `chrono` / `time` + `icu`（或 `chrono-tz`） | `en-US` 的 `dateStyle:"full"`, `timeStyle:"long"` 格式需逐字（如 `Sunday, October 26, 2026 at 10:00:00 AM GMT+8`）；非法时区错误文本。 |
| `structuredClone` | 消息克隆（remote/测试） | 直接克隆 `serde_json::Value` | JS 值域（undefined、BigInt、Symbol、函数）→ JSON 值域；`NaN`/`Infinity` → `null`。 |
| `vitest` | 测试 | `cargo test` + `tokio::test` | 每个 `it` 对应一个 `#[test]`；`it.each` 展开。 |
| Lark grammar（`CODEMODE_SOURCE_GRAMMAR`） | 提供给 provider 的约束语法 | 字符串常量 | 只需**逐字保留文本**（Rust 侧不解析它）；`parseCodemodeSource` 手写实现。 |

### 6.1 必须显式桥接的 JS 语义

1. **UTF-16 字符串索引**：`String.length`、`slice`、`charCodeAt`、`.sort()` 顺序、`outputChars += data.length`
   全部按 UTF-16 码元。Rust `String` 是 UTF-8 → 长度/切片/排序需用 `encode_utf16()` 或 `Vec<u16>`。
   受影响：`fnv1a`、`store` 字符计数、`output` 限额、`MAX_TAG_CHARS`、`maxChars` 预算、声明排序。
2. **JS 数字格式化**：`String(number)`（`@minimum 1` 等 tag）、`JSON.stringify(number)`（`1e+21`、`-0` → `0`、
   `1.5`）、`JSON.stringify` 对 `NaN`/`Infinity` → `null`。Rust 需实现 ECMAScript Number::toString（最短往返表示）。
3. **JSON 键顺序**：`JSON.stringify` 保持插入顺序；`storeWrites.set`、`toolCall.arguments`、错误 `properties` 的
   键序会影响逐字输出与快照测试。Rust 用 `serde_json::Map` 默认 `BTreeMap`（排序）→ 必须启用
   `preserve_order` 特性（`IndexMap`）。
4. **`JSON.stringify` 的 `undefined` 语义**：对象中 `undefined` 值被省略、数组中变 `null`、顶层 → `undefined`。
   `serde_json::Value` 无法表示 `undefined` → 需要自定义 `JsValue` 枚举或"Option + 省略"约定。
5. **`undefined` vs `null`**：工具参数/结果/返回值/`text(undefined)`（→ `"undefined"`）都要区分。
6. **正则方言**：`^[a-z]+$`、`[/]9j[/]`、`\s+`、`[^A-Za-z0-9_$]` 等在 JS 与 Rust `regex` 大体一致，
   但 JS 的 `\s` 包含 `﻿`（Rust `regex` 的 `\s` 默认按 Unicode White_Space，也含 FEFF？——
   需在测试里锁定），`String.prototype.replace` 的 `$` 替换语义、`replaceAll` 的字符串模式都要注意。
   建议：prelude 内的正则随 prelude 原样运行在 QuickJS 里，**不要**改写成 Rust 正则；只有宿主侧的
   `IDENTIFIER`、`/\r?\n/`、`LINE_TERMINATORS`、`image` 签名检查需要 Rust 等价。
7. **`Math.imul`** 与 32 位无符号回绕：`fnv1a` 用 `u32::wrapping_mul`。
8. **`Math.round`**：`Math.round(-0.5)` = `-0`，Rust `f64::round` 是"远离零"（-0.5 → -1）。`durationMs` 用
   `(x + 0.5).floor()` 并处理 `-0`。
9. **`Array.prototype.sort()`**：默认按 UTF-16 码元排序（等价于按 code unit 的字典序），Rust `str::cmp` 是字节序
   ——对 ASCII 相同，对非 ASCII 不同。属性名排序必须用 `encode_utf16` 比较。
10. **`Object.keys` 顺序**：整数索引键按数值升序、其余按插入顺序。声明渲染与 store 序列化都受影响。
11. **`structuredClone` / JSON 往返**：`Date`、`Map`、`Set`、`BigInt`、`Error` 在 JSON 往返中丢失/变形；
    `JSON.stringify(Error)` = `{}`；`10n` → `TypeError`。
12. **`performance.now()`/`Date.now()`**：毫秒；`Date` 在确定性模式下 UTC 且从 `startedAt` 起。
13. **`String(undefined)`/`String(null)`**：`"undefined"`/`"null"`。
14. **`Set`/`Map` 的 `SameValueZero`**：`new Set(values)` 在 `constraintTags` 里按 `===` 去重（数字/字符串）。

---

## 7. 测试清单（逐文件）

所有测试位于 `/home/ctyun/code/kiana_rev/kiana/packages/{agent,codemode}/test/`。Rust 侧应一一对应
（`cargo test`），并保留同样的断言文本。

### 7.1 `agent/test/agent-loop.test.ts`（36 个 `it` 声明，其中 2 个 `it.each`（各 2 例）→ 38 次运行；2234 行）

结构：`describe("default stream function compatibility")`（1）、`describe("agentLoop with AgentMessage")`（30）、
`describe("agentLoopContinue with AgentMessage")`（3）、`describe("runToolCall")`（2）。

辅助：脚本化 fake `streamFn`（按事件序列返回 `createAssistantMessageEventStream()`）、记录事件的 `emit`、
`waitForTick()`、`calculateTool`/`createCalculateToolWithUsage`、`beforeToolCall`/`afterToolCall` 记录器、
`Type.Object` 工具、`setDefaultStreamFn`。逐条（标题逐字）：

1. `uses the configured default when a legacy caller omits streamFn`
2. `should emit events with AgentMessage types` —— 断言完整事件序列与消息形状。
3. `should build provider context exclusively from transcript messages` —— provider 收到的 `context.messages` 只来自
   transcript（system 在前），不来自 `context.systemPrompt`/`context.tools`。
4. `should handle custom message types via convertToLlm`
5. `should apply transformContext before convertToLlm` —— 顺序与 `signal` 透传。
6. `should handle tool calls and results` —— `tool_execution_start/update/end`、toolResult 消息、`turn_end.toolResults`。
7. `records how long execute() took on the tool result, excluding hooks` —— `durationMs` 为整数毫秒，hooks 时间不计入；
   结果消息与 `tool_execution_end` 都带。
8. `should not execute tool calls from a length-truncated assistant message` —— 全部变为 `isError`，文本为截断提示。
9. `should execute mutated beforeToolCall args without revalidation`（#528）。
10. `should prepare tool arguments for validation` —— `prepareArguments` 的返回值进入校验与执行。
11. `should emit tool_execution_end in completion order but persist tool results in source order`。
12. `should inject queued messages after all tool calls complete`。
13. `should force sequential execution when a tool has executionMode=sequential even with default parallel config`。
14. `should force sequential execution when one of multiple tools has executionMode=sequential`。
15. `should allow parallel execution when all tools have executionMode=parallel`。
16. `runs finishTurn after tool-result messages and before turn_end`。
17. `it.each(["error","aborted"])` + `runs finishTurn for a %s assistant before turn_end without changing the hard
    exit`（错误/中止是硬退出；`finishTurn` 仍运行但其 `continue` 决策被忽略；仍发 `turn_end`/`agent_end`）。
    精确断言（`agent-loop.test.ts:1162-1220`）：`ordering === ["finishTurn","turn_end"]`、`providerCalls === 1`、
    `steeringPolls === 1`、`followUpPolls === 0`。**是 1 和 0，不是“按实现”**：硬退出前仍轮询一次 steering
    （并丢弃结果），follow-up 一次都不轮询。Rust 侧请直接按这 4 个数值写断言。
18. `action:end skips queue polling and next-turn preparation`。
19. `makes exactly one context-only request when no natural request satisfies continuation`。
20. `lets a natural tool-result request satisfy continuation`。
21. `it.each(["steering","follow-up"])` + `lets a natural %s request satisfy continuation`（自然队列满足
    `finishTurn` 的 `continue` 决策，不再额外发 context-only 请求）。
22. `prepares the initial request after pending messages and can replace request state`。
23. `does not poll steering after prepareRequest`。
24. `should use prepareNextTurn snapshot before continuing`。
25. `picks up steering queued during prepareNextTurn before the next request`。
26. `action:end receives finalized turn context and stops before queue polling`。
27. `should stop after a tool batch when every tool result sets terminate=true`。
28. `should stop after a blocked tool call when beforeToolCall sets terminate=true`。
29. `should continue after a mixed batch with one terminating blocked call`。
30. `should continue after parallel tool calls when not all tool results terminate`。
31. `should allow afterToolCall to mark a tool batch as terminating`。
32. `should throw when context has no messages` —— `Cannot continue: no messages in context`。
33. `should continue from existing context without emitting user message events`。
34. `should allow custom message types as last message (caller responsibility)`。
35. `validates, runs the hooks, and reports failures as error outcomes`（`runToolCall`）。
36. `lets afterToolCall replace structured content and drops it when only content is replaced`。

### 7.2 `agent/test/agent.test.ts`（36 个 `it` 声明，其中 5 个 `it.each`（各 2 例）→ 41 次运行；1229 行）

`describe("Agent")`。逐条（标题逐字）：

1. `uses the configured default when a legacy caller omits streamFn`
2. `should create an agent instance with default state`
3. `should create an agent instance with custom initial state`
4. `converts initial prompt and tools into transcript state`
5. `declares tool loadout changes to the model before the next request`
6. `merges tool changes into a pending system message`
7. `rewrites pending tool declarations to match the executable set`
8. `restores the transcript baseline when reset`
9. `should subscribe to events`
10. `emits full lifecycle events for thrown run failures` —— `message_start`/`message_end`/`turn_end`/`agent_end` 与
    合成 assistant 消息（`content:[{type:"text",text:""}]`）。
11. `should await async subscribers before prompt resolves`
12. `waitForIdle should wait for async subscribers`
13. `should pass the active abort signal to subscribers`
14. `should ignore tool updates after the tool execution settles`
15. `should ignore a settled parallel tool update while another tool is still running`
16. `should update state with mutators`
17. `should support steering message queue`
18. `should support follow-up message queue`
19. `should handle abort controller`
20. `should reject reset while processing without corrupting the transcript`
21. `should throw when prompt() called while streaming`
22. `should throw when continue() called while streaming`
23. `continue() should process queued follow-up messages after an assistant turn`
24. `it.each([{mode:"one-at-a-time",expectedRequests:2},{mode:"all",expectedRequests:1}])` +
    `continue() keeps $mode steering semantics for assistant-tail fallback`
25. `it.each([{name:"empty",messages:[]},{name:"system-only",messages:[...]}])` +
    `rejects a queued continuation from $name context without draining queues`
26. `it.each([{name:"user",...},{name:"toolResult",...}])` +
    `defers follow-up input on the first continuation request from a $name tail`
27. `it.each([{mode:"one-at-a-time",expectedRequests:2},{mode:"all",expectedRequests:1}])` +
    `polls $mode steering at continuation startup`
28. `it.each(["error","aborted"])` + `keeps queues on a %s response even when finishTurn requests continuation`
29. `keeps legacy prepareNextTurn signal callback behavior`
30. `forwards finishTurn through AgentOptions with the active abort signal`
31. `keeps steering ahead of follow-up from a non-assistant continuation tail`
32. `keeps queues when finishTurn ends the run`
33. `previews the next selected queued messages without consuming them`
34. `forwards provider stream event observers through AgentOptions`
35. `forwards sessionId to streamFunction options`
36. `attributes requests to kiana by default and merges a configured identity over it` —— `DEFAULT_AGENT_IDENTITY` 默认、
    `identity` 逐字段覆盖、`attribution: null` 关闭。

### 7.3 `agent/test/e2e.test.ts`（10 个 `it`，416 行）

用 `@earendil-works/kiana-ai/compat` 的 faux provider（`registerFauxProvider`/`fauxAssistantMessage`/`fauxText`/
`fauxThinking`/`fauxToolCall`/`streamSimple`），`afterEach` unregister。
`describe("Agent integration with faux provider")`：`handles a basic text prompt`（3 条消息、system/user/assistant、
`isStreaming === false`）、`executes tools and tracks pending tool calls`、`handles abort during streaming`、
`emits lifecycle updates while streaming`、`maintains context across multiple turns`、`preserves thinking content blocks`。
`describe("Agent.continue() with faux provider")`：`validation`（`throws when no messages in context`、
`throws when last message is assistant`）、`continue from user message`（`continues and gets a response when last
message is user`）、`continue from tool result`（`continues and processes tool results`）。

### 7.4 `agent/test/proxy.test.ts`（4 个 `it`，171 行）

`model: Model<"openai-responses">`（`gpt-5.4`/`openai`/`https://api.openai.com/v1`/`reasoning: true`/`input:
["text"]`/`contextWindow: 400000`/`maxTokens: 128000`）；`usage` 全 0；`afterEach` `vi.unstubAllGlobals()`。

1. `preserves tool-call metadata received only on toolcall_end` —— `toolcall_start` 的 id 为 `call_test|fc_test`，
   `toolcall_delta` 后 `toolcall_end` 携带 `{id, name, arguments}`，最终 partial 的 `arguments` 为合并结果。
2. `processes terminal metadata when the event is not newline-terminated` —— 最后一帧**不以换行结尾**也能被处理：
   `body` 的最后一帧没有 `\n\n`，EOF 后靠 `decoder.decode()` + 残余 buffer 处理（`processLine(buffer)`）。
3. `emits an error instead of hanging when the stream ends without a terminal event` —— `reason:"error"`，
   `errorMessage` 为 EOF 文本。
4. `shows growing partial arguments without keeping the raw text on the block, in linear time`（#9265）——
   20k 行 `toolcall_delta` 的 `partial` 参数增长为线性，块上不留原始文本。

### 7.5 `codemode/test/declarations.test.ts`（17 个 it，328 行）

见 §2.2.4 的逐字样例：原语/字面量/联合、单行对象+排序、属性注释、约束 tag、可选字段非 null 分支、
draft-04 布尔排他边界、长 tag 丢弃与行分隔符转义、超预算先丢 tag、`$ref` 展开与递归、数组/元组、
超预算 → unknown、工具签名归一化、MCP CallToolResult 输出形状、`renderToolSample`、`renderDeclarations`
（tools+globals、命名空间+显式 signature、描述中的 `*/` 转义）。

### 7.6 `codemode/test/source.test.ts`（5 个 it，78 行）

逐字见 §2.2.2；另含"只有第一行是 options 行"（`// @optionsx {}`、字符串中的 `// @options: {}` 不算）。

### 7.7 `codemode/test/sandbox.test.ts`（53 个 `it` 声明；`describe.each(Object.keys(BACKENDS))` 内 47 个 ×
3 个后端 = 141 次运行，加上顶层的 1 个与 `worker host` 的 4 个 `it` + 1 个 `it.each`（7 个用例）→ 共 153 次运行；869 行）

后端（`BACKENDS`）：
- `worker`：`new CodemodeSandbox(options)`。
- `inline`：`new InlineCodemodeSandbox({...options, wasm: loadQuickJSWasm(), interruptBudget: 20_000})`。
- `remote`：`new RemoteCodemodeSandbox({...options, remote: () => ({execute: (request, exchange) =>
  serveCodemodeRemote(structuredClone(request), async messages => structuredClone(await exchange(structuredClone(
  messages))), {wasm: loadQuickJSWasm(), maxInterruptBudget: 20_000})})})`。

常量：`INTERRUPT_BUDGET = 20_000`；`echo = {name:"echo", execute: args => args}`；`createSandbox(tools = [],
timeoutMs = 10_000)`；`afterEach` 关闭全部 sandbox。PNG `iVBORw0KGgo=`、JPEG `/9j/4A==`、GIF `R0lGODlh`、
WEBP `UklGRgAAAABXRUJQ`。大图为 `iVBORw0KGgoA + "QUJD".repeat(256 * 1024)`。

**顶层 `describe("embedded sources")`（1）**

1. `parse as JavaScript`：`new vm.Script(PRELUDE_SOURCE, {filename: "prelude.js"})` 不抛。

**`script execution`（12）**：返回值 JSON 往返（对象 / `'plain'` / `""` → `{ok:true, value: undefined}`）；
顶层 await（41+1）；`text`/`image`/`console` 顺序与形状（9 个 output 项逐字断言，含
`{type:"text",text:'hello 1 {"a":1}',console:true}`、`text(undefined)` → `"undefined"`、`text(7)` → `"7"`、
MIME 由数据判定、末项 `Error: bad` 前缀且 `console:true`）；非法 `text`/`image` 参数（**16 个用例**的逐字错误列表：
第 0 项 `text(circular)` 匹配 `/^TypeError: .*circular/`，其余 15 项为 1 个 IMAGE_HELPER_EXPECTS + 1 个 remote URL +
1 个 `invalid image output. Pass a base64 data URI instead` + 1 个 `image only accepts image blocks, got "text"` +
1 个 `image expected image block data` + 1 个 IMAGE_HELPER_EXPECTS（`image(42)`）+ 6 个 `not valid base64` +
3 个 `not a PNG, JPEG, GIF, or WebP image`）；换行与超大 base64（`iVBORw0K\r\nGgo=\n` 归一化为 PNG、
`"QUJD".repeat(256*1024)` 原样）；`exit()` 保留输出与 store 写入且 `value` 为 `undefined`；
失败前输出保留；语法错误（`name:"SyntaxError"`，`stack` 匹配 `/codemode\.js:2/`）；运行错误
（`{kind:"script", name:"TypeError", message:"boom 1"}` + 行号）；V8 风格栈且无 prelude 帧
（`/^RangeError: outer\n {4}at .*codemode\.js:2/`，输出里的 `Error: inner` 匹配 `/codemode\.js:1/`，
`JSON.stringify(result.output)` 不含 `codemode-prelude.js`）；非 Error 抛出
（`{kind:"script", message:'{"code":7}'}`）；不可序列化返回值 `10n`（`{kind:"script", name:"TypeError"}`）。

**`tools`（9）**：异步函数 + 记录调用；并发调用 + `ALL_TOOLS`；归一化标识符（`ALL_TOOLS` 用 jsName）；
`undefined` 参数/结果穿过；工具错误 → 可 catch 的 Error；未知工具（`Unknown tool "..."` 经 rejection）；
未 await 的调用在脚本返回时被 abort（`status:"cancelled"`，signal 已 abort）；读取不存在工具的近似名建议；
register/unregister 跨执行生效。

**`store and load`（5）**：读快照 + 写报告；`load` 返回副本（改副本不改 store）；非法 key/值/超限（脚本内 catch）；
超限解释文本（含 `STORE_HINT`）；`store`/`load` 作为 global 名被拒绝（`Invalid global name`）。

**`globals`（4）**：顶层函数且不计入 `calls`；命名空间 + `spread: true`（execute 收到参数数组）；
命名空间成员不存在时的建议文本；非法/保留名被 `start` 拒绝。

**`limits and lifetime`（9）**：同步死循环 `while (true) {}` → `timeout`（10 s 期限或中断预算）；
`timeoutMs: Infinity` 无 deadline；等待无法 settle 的 promise → 立即失败（`The script is waiting on a promise
that can never settle...`）；微任务自旋 `while (true) await null` → `timeout`；`signal` abort 并取消在途调用；
输出超限即使 catch 也失败（`RangeError`）；`close()` 后 `execute()` reject `Sandbox is closed`；
并行执行不共享状态；深递归 → 可 catch 的 `RangeError`。

**`escape hatches`（8）**：无宿主全局（无 `process`/`require`/`fetch` 等）；`eval`/`Function` 局限在 VM；
动态 `import` 被拒绝；改写内建与内建全局无效；实例可达的内建被冻结；实例仍可覆盖冻结原型上的
`OVERRIDABLE` 属性（`this.name = ...` 可用）；`name`/`message` 非字符串的错误；`tools` 与 `console` 冻结
（`tools.echo = ...` 无效、`"extra" in tools === false`、`globalThis.tools = null` 无效）。

**`worker host`（4 个 `it` + 1 个 `it.each` 7 用例）**

- `accepts a worker path string`：`workerUrl` 为 `src/runtime/worker.ts` 的路径字符串。
- `reports a missing worker file as a sandbox error`：`workerUrl: ./does-not-exist.js` → `kind:"sandbox"`。
- `reports a failing wasm module as a sandbox error`：`wasm: Promise.reject(new Error("no wasm"))` →
  `"Failed to load QuickJS: no wasm"`。
- `ignores Node --watch notifications from the worker`（#10725）：fixture `raw-worker.ts` 先发
  `{"watch:require":["/worker.js"]}`、`{"watch:import":["file:///worker.js"]}`，再发
  `{type:"done", ok:true, value:"42", writes:"[]"}` → `{ok:true, value: 42}`。
- `it.each`（#10444）`reports a broken bridge as a sandbox error: %j`，7 个用例逐字：
  | payload | `Sandbox bridge broken:` 后的原因 |
  |---|---|
  | `{type:"done",ok:true,value:"1",writes:"null"}` | `store writes are not an array` |
  | `{type:"done",ok:true,value:"1",writes:"[1]"}` | `store writes contain a malformed entry` |
  | `{type:"done",ok:true,value:"1",writes:'[["k", "{"]'}` | `store value for "k" is not valid JSON` |
  | `{type:"done",ok:true,value:"{",writes:"[]"}` | `return value is not valid JSON` |
  | `{type:"done",ok:false,error:"5"}` | `script error is not an object` |
  | `{type:"done",ok:false,error:"{}"}` | `script error is malformed` |
  | `{type:"nonsense"}` | `unknown message from the worker` |

### 7.8 `codemode/test/deterministic.test.ts`（14 个 it 在 2 个后端循环内 → 28 次运行；
1 个 remote transport 专项；1 个 `execute()` 非确定性 = 共 30 次运行；410 行）

常量：`BUDGET = 50_000`；`STARTED_AT = Date.UTC(2026, 0, 15, 10, 0, 0)`；默认 `seed: 1`、`timeoutMs: 20_000`；
工具 `echo`/`wait`；`start()` 对除 `wait` 外的调用立即以 `seq` 递增、`now = STARTED_AT + 1000 * seq` 回答；
`answer` 默认回显 `call.args`。`afterEach` 恢复 `process.env.TZ`。

后端：`worker`（`CodemodeSandbox`）与 `remote`（`RemoteCodemodeSandbox` + `serveCodemodeRemote`，
`structuredClone` 每个消息；`delayFirst` 在 `reply.seq === 1` 时延迟 30 ms 以打乱到达顺序）。

1. **相同 start/seed/结果 → 相同调用与输出**。脚本记录
   `[Date.now(), performance.now(), Math.random(), Math.random(), new Date().toISOString()]` 并三次 echo/输出。
   两个 run 的结果除 `calls[].durationMs`（墙钟）外全等；第一次调用的参数
   `[0] = STARTED_AT`、`[1] = 0`、`[4] = "2026-01-15T10:00:00.000Z"`；第二次输出为
   `[STARTED_AT + 1000, 1000]`；`[2] === PINNED_FIRST_RANDOM`。`startedAt + 5000` 不改变随机值；
   `seed: 2` 改变；`seed: 0` 得到 `PINNED_SEED_ZERO` 且不含 seed 1 的值。
2. **UTC 且无 WeakRef**。`TZ=Asia/Kolkata` 下 `[new Date(0).getHours(), getTimezoneOffset(), typeof WeakRef,
   typeof FinalizationRegistry]` = `[0, 0, "undefined", "undefined"]`；同一 code 在 `execute()` 中得到
   `[5, -330, "function", "function"]`。
3. **按 seq 应用**，即使乱序 deliver：两个 `tools.wait` 的 promise，`deliver(2,...)` 先于 `deliver(1,...)`，
   结果为 `["b", "a"]`；`idles === [[0, 2], [1, 2]]`（最后一个结果后没有 idle）。
4. **指纹**：`events` 前三项为 `["started", "call 1", "idle 0"]`；`idles === [[0,1],[1,3],[2,3]]`；
   `calls` 为 `[[1,"tool","echo",1],[2,"tool","echo",2],[3,"tool","echo",3]]`；指纹匹配
   `/^quickjs-wasi \S+, quickjs \S+, prelude [0-9a-f]{8}, deterministic \d+, stack \d+, heap 67108864, budget \S+$/`
   （`memoryLimitBytes: 64 MiB`）；remote 为 `budget 50000`，worker 为 `budget none`；
   相同配置指纹相同，`memoryLimitBytes: 32 MiB` 的沙箱指纹不同。
5. **globals 与 tools 一起编号；错误以 rejection 返回**：`search("x", 2)`（`spread: true`）为
   `{id:1, target:"global", name:"search", args:["x",2]}`；`tools.echo("fail")` 的回答
   `{ok:false, message:"no"}` 被脚本 catch 后 `text(error.message)`；`idles` 前两项 `[[0,1],[1,2]]`；
   `result.calls === [{name:"echo", status:"error"}]`。
6. **时钟单调 + 非有限 now 抛错**：`deliver(1, NaN, ...)` 抛 `not a finite number`；
   `deliver(1, STARTED_AT - 2000, ...)` 后结果为 `[STARTED_AT, STARTED_AT, 0]`。
7. **结束时报告正在处理的结果号**：`await echo(1); return await echo(2)` → `during:{seq:2, issued:2}`；
   并行后抛错 → `{seq:3, issued:3}`；无调用 → `{seq:0, issued:0}`；`execute()` 结果**没有** `during` 键。
8. **答案不可序列化时调用保持等待**：`value: 1n` 与 `properties: {big: 1n}` 的 deliver 都抛错（且不消耗 seq），
   随后 `deliver(1, ..., {ok:true, value:"fine"})` 成功。
9. **rejection 携带 properties**：`{structuredOutput:{code:7}, name:"Ignored", message:"ignored"}` →
   脚本内 `[true, "denied", "Error", {code:7}]`（`name`/`message`/`stack` 被忽略）。
10. **properties 非对象 + 工具脚本名重复**：`null` 与 `[1]` 都抛 `not an object`；`tools: [{name:"a-b"},{name:"a_b"}]`
    → `start()` 抛 `script names must be unique`。
11. **使用 run 传入的脚本名**：`{name:"my-tool", jsName:"renamed"}` →
    `["function", ["renamed", "my-tool"], ["renamed"]]`（`jsName` 与原名都是 tools 成员，`ALL_TOOLS` 只有 `jsName`）。
12. **调用方 abort → 脚本无法 catch 的错误**：`controller.abort(new Error("diverged"))` →
    `{ok:false, error:{kind:"aborted", message:"diverged"}, output: []}`（catch 与后续 `text` 都不生效）；
    之后 `deliver` 不抛（静默忽略）。
13. **有结果即 `finished`**：等待时 `false`，结果后可 `true`。
14. **未等待的调用 / 重复 seq**：`deliver(1, ..., 99, ...)` 抛 `Call 99 is not waiting`；
    已用过的 seq 1 再 deliver 抛 `used once`；`seq = 0` 也抛 `used once`；`seq = 2` 正常。
15. **remote transport 重排**：按序 `deliver(1,...)`、`deliver(2,...)`，但 reply 1 被延迟 30 ms →
    结果仍为 `["a", "b"]`。
16. **`execute()` 非确定性**：`new CodemodeSandbox()` 的两次 `[Date.now(), Math.random()]`
    第一次时间落在 `before..now`，第二次随机值不同。

### 7.9 `codemode/test/portable.test.ts`（7 个 it，159 行）

`const wasm = loadQuickJSWasm()`（模块级，只加载一次）；辅助 `recordingRemote(options)` 记录每次 exchange 的消息数，
并用 `structuredClone` 模拟 Workers RPC。

**`InlineCodemodeSandbox`**

1. `interruptBudget: 1_000`，`timeoutMs: Infinity`，对 `while (true) {}` 与 `while (true) await null` 都返回
   `{ok:false, error:{kind:"timeout", message:"Execution exceeded its interrupt budget of 1000"}}`。
2. 预算跨多次等待工具累计：`for (;;) { for (let i = 0; i < 1e5; i++); await tools.tick(); }` → `kind:"timeout"`，
   且 `result.calls.length > 1`。

**`RemoteCodemodeSandbox`**

3. 工具在调用方执行、脚本在远端：`text('hi'); return await tools.add({a:1})`，`store:{k:1}` →
   `{ok:true, value:2, output:[{type:"text",text:"hi"}]}`；调用方 `seen = [{a:1}]`；`calls = [{name:"add",status:"ok"}]`。
4. 输出批量：`for (let i = 0; i < 5000; i++) console.log(i)` → 输出 5000 项、最后一项
   `{type:"text", text:"4999", console:true}`、`exchanges.length <= 6`（即不是每行一次 RPC）。
5. 上限裁剪：远端 `maxInterruptBudget: 1_000`、`maxMemoryLimitBytes: 8 MiB`，客户端请求 `interruptBudget: 1e9` →
   死循环得到 `"Execution exceeded its interrupt budget of 1000"`；再跑分配循环 → `ok:false` 且序列化结果包含
   `"out of memory"`（`InternalError: out of memory`）。
6. 调用方 abort → 结果 `kind:"aborted"`；远端未决 exchange 收到 `{type:"stop"}`，其 VM 被 dispose 且
   `execute()` 返回；`await Promise.all(served)` 不挂起。
7. 远端失败/消失/不可达三种：
   - `remote.execute` reject `Error("object reset")` → `{kind:"sandbox", message:"Remote sandbox failed: object reset"}`；
   - `remote.execute` 直接 resolve（无终态） → `"Remote sandbox returned before the script settled"`；
   - `remote()` 抛 `Error("no binding")` → `"Failed to reach the remote sandbox: no binding"`。

### 7.10 `codemode/test/worker-lifetime.test.ts`（第一个 describe 里 6 个 `it` 声明——其中 1 个
`it.each(["abort","timeout","close"])` 3 例；`describe.each(runtimes)`（Node 恒有、Bun 用
`spawnSync("bun", ["--version"]).status === 0` 探测）里 1 个 `it.skipIf(!available).each(["abort","timeout","close"])`
3 例 × 2 运行时。共 **14 次运行**（第一个 describe 5 个普通 `it` + 3 例 = 8，runtimes 3 例 × 2 = 6；
无 Bun 时 11 次）；229 行；`afterEach` 恢复 fake timers 与 spy）

`it.each(["abort","timeout","close"])`（3 例共用一条断言链）：准入拒绝不启动 worker；构造失败归还配额；
wasm 加载失败不占配额；结算而不等待退出（`terminate` 只调一次、内部先 `ref` 再 `unref`、`onWorkerStarted` 的
exited 未 resolve、`close()` 与迟到的 worker 消息都不改变已决结果）；execute/start 各报告一次真实退出并归还；
`onWorkerStarted` 抛错 → 请求终止并归还。
另有 Node/Bun 进程生命周期 fixture（`fixtures/worker-process-exit.ts`），断言两行 JSON（result、exit）与时间差。

### 7.11 `codemode/test/fixtures/`

- `raw-worker.ts`（448 B）：把 `workerData.code` 解析成消息（或消息数组）并原样 `postMessage`
  （`JSON.parse((workerData as { code: string }).code)`），用于注入真实 prelude 不会产生的宿主消息
  （如 `duplicate call id`、`unknown message from the worker`、非 JSON 的 `store writes`）。
- `worker-process-exit.ts`（1.4 KB）：进程生命周期夹具。用法 `node fixture.ts abort|timeout|close`（其他值抛
  `Unknown stop mode: ${how}`）；`process.on("exit")` 打印 `{type:"exit", elapsedMs}`；sandbox
  `memoryLimitBytes: 256 MiB`，工具 `ready` 在非 timeout 模式下 100 ms 后 `controller.abort()` 或 `sandbox.close()`；
  脚本为 `await tools.ready(); return 'a'.repeat(300000).indexOf('a'.repeat(10000) + 'b')`（Bun 上不可中断的内建），
  `timeoutMs` 为 300（timeout）或 `Infinity`；结果打印 `{type:"result", kind, elapsedMs, workerExited}`。
  测试断言两行 JSON、`kind` 为 `timeout`/`aborted`、`result.elapsedMs < 1000`，Node 上
  `exit.elapsedMs - result.elapsedMs < 1000`。

### 7.12 测试配置

- `agent/vitest.config.ts`：别名到源码，`testTimeout: 30000`。
- `codemode/vitest.config.ts`：同上。
- 根 `vitest.base.ts`：workspace 源码别名。
- `tsconfig.base.json`：target ES2024，module Node16，`erasableSyntaxOnly`。

---

## 8. Rust 模块布局与最难复现的部分

### 8.1 建议的 crate 与模块

```
kiana/
  crates/
    kiana-agent-core/                 # 对应 @earendil-works/kiana-agent-core
      src/
        lib.rs                     # 重导出（与 index.ts 对齐）
        types.rs                   # AgentMessage/AgentTool/AgentEvent/...（§2.1.2）
        stream_fn.rs               # set_default_stream_fn/get_default_stream_fn + OnceLock
        agent_loop.rs              # run_loop/stream_assistant_response/tool 执行（§2.1.4）
        agent.rs                   # Agent 状态机、队列、生命周期（§2.1.5）
        proxy.rs                   # stream_proxy（§2.1.6）
        identity.rs                # DEFAULT_AGENT_IDENTITY
      tests/                       # §7.1-7.4 对应
    kiana-codemode/
      src/
        lib.rs                     # 公开面（§1.3）
        types.rs
        source.rs                  # parse_codemode_source + CODEMODE_SOURCE_GRAMMAR
        identifier.rs
        declarations.rs            # schema → TS 声明
        wasm.rs                    # 模块加载/缓存
        prelude.js                 # include_str! 的解析后 prelude（17233 字符）
        runtime/
          mod.rs
          protocol.rs              # VmStartData/WorkerData/消息 + 守卫
          vm.rs                    # start_vm、确定性 WASI、指纹
          execution.rs             # Execution/CodemodeSandboxBase
          inline.rs
          host.rs                  # 线程/worker 后端
          remote.rs
          worker.rs                # worker 入口（二进制或线程启动函数）
        cloudflare.rs              # 可选（见 open questions）
      tests/
    kiana-codemode-cli?               # 无（本单元无 CLI）
```

Rust 设计要点：

- **异步**：用 `async-trait` 定义 `StreamFn`、`CodemodeTool::execute`、钩子；事件流用
  `futures::stream::Stream<Item = AgentEvent>` + `tokio::sync::mpsc`，并保留 `result()` 语义
  （`EventStream` 需同时支持推送与结果 future）。
- **类型等价**：`AgentMessage` 用 `serde_json::Value` + `#[serde(tag = "role")]` 枚举；`ToolCall.arguments` 是
  `serde_json::Value`；`usage`/`cost` 用结构体；所有可选字段 `Option<T>` 且 `skip_serializing_if`。
- **钩子**：`before_tool_call: Option<Arc<dyn Fn(BeforeToolCallContext, Option<CancellationToken>) -> BoxFuture<Option<BeforeToolCallResult>>>>`。
- **工具执行**：`join_all` 保序返回 + `FuturesUnordered` 乱序完成事件；`durationMs` 在 `execute` 前后取
  `Instant`，不含 hooks，`Math.round` 语义。
- **sandbox 后端**：`CodemodeSandboxBase` 为 trait + 泛型 channel；`VmLauncher = Arc<dyn Fn(VmStartData, VmEvents) -> BoxFuture<VmChannel>>`。
- **VM**：若选 `rquickjs`（原生 QuickJS），可去掉 WASI shim 与 wasm 内存上限，但需自行注入虚拟时钟/随机数；
  若选 wasm（`wasmtime` 嵌入 quickjs-wasi.wasm），可保留原 wasm 二进制与 WASI 行为，指纹字符串仍可精确复现。
  **推荐后者**，因为错误文本、`Intrinsics`、`versions`、interrupt 语义都由同一份 wasm 决定。

### 8.2 最难的 10 个点（按风险排序）

1. **prelude 逐字节复刻与哈希**。指 `PRELUDE_SOURCE`（解析后 17233 个 UTF-16 单元、全 ASCII、FNV-1a =
   `8c6c9242`）。Rust 侧必须把**同一文本**注入 VM（含 TAB 缩进与全部字符串），否则：
   - 确定性指纹变化（`prelude [0-9a-f]{8}` 测试失败）；
   - 所有脚本可见错误文本、`lockdown()` 行为、store/output/image 校验都可能偏移。
   做法：把解析后的文本作为 `prelude.js` 资源，构建期/测试用 `include_str!` + 断言长度与哈希。
2. **确定性 `Math.random()` 链**。QuickJS 用 WASI 时钟初始化内部 PRNG，实测链条为
   `state = seed + 1` → 丢一次 → xorshift64* → `(out >> 12) * 2^-52`（§2.2.8）。Rust 若不嵌入同一份
   quickjs-wasi.wasm，很难逐位复现；**推荐直接注入 `Math.random`**（确定性模式下 prelude 顶层把 `Math.random`
   重定义为该序列），这样与引擎无关。注意 `startedAt` 不影响随机、seed 0 与 1 不同（`+1` 的原因）。
3. **QuickJS 错误文本与栈帧**。`Name: message\n    at f (codemode.js:2:31)`、语法错误、`InternalError: out of
   memory`、`Converting circular structure to JSON`、`RangeError: Maximum call stack size exceeded`、
   `10n` 的 `TypeError`、`name`/`message` 非字符串时的 `String()` 行为。换引擎版本会改文本 →
   必须锁定 quickjs 版本（或嵌入同一 wasm）并把每个文本写进测试。
4. **确定性指纹中的版本字符串**。`quickjs-wasi ${versions["quickjs-wasi"]}, quickjs ${versions.quickjs}` ——
   Rust 侧要么从同一 wasm 读取（若绑定暴露 `versions`），要么固定为常量并在文档中标注。
5. **并行工具事件顺序**。`tool_execution_end` 按完成顺序、消息按源序、preflight 立即结果在 preflight 阶段即时
   emit end；`terminate` 只在**全部**最终结果都为 true 时生效；`durationMs` 缺失时的键省略。
   Rust 需精确建模"先全部 preflight 再并发执行"两阶段。
6. **`ToolCallArgumentsStream` 的线性时间增量解析**。20k 增量行必须 O(n)；每次 `append` 返回完整的新
   `arguments` 对象，且块对象被替换（Rust 侧需克隆 `Value`，但可用 `Arc` 共享未变部分或保持 O(n) 结构）。
   还需支持不完整 JSON（部分字符串/数字/嵌套）与 `replace` 语义。
7. **`EventStream` 结算语义**。`Agent` 的 `isStreaming`/`prompt()`/`waitForIdle()` 要等异步 listener（含
   `agent_end`）全部完成；listener 抛错 → 合成 assistant 消息并走 `agent_end`；低层 `agentLoop` 的流是"观察性"的，
   不等待消费者。Rust 需复刻"推事件 + 结果 future"双通道，且 backpressure 语义不改变事件顺序。
8. **worker 生命周期与中断**。`terminate()` 不等待；`unref()` 在 `terminate()` 之后；`acquireWorker` 在 wasm 加载后、
   worker 构造前同步调用；构造失败/真实退出时 release 一次；`onWorkerStarted` 抛错 → 终止并释放；
   Bun 上 `terminate()` 可能挂起。Rust `std::thread` 无法强杀 → 需要协作中断（AtomicBool 传给 VM 的 interrupt
   handler）+ detach，且 `close()`/结果结算不等待线程退出。测试用 `Terminate` 的 invocations 断言需以等价
   语义（只请求一次、不 join）实现。
9. **`structuredClone`/JSON 往返与 `undefined`**。工具 args/results、store 值、返回值的 JSON 往返丢 `undefined`、
   `NaN`/`Infinity` → `null`、`undefined` 属性被省略；`deliver()` 中序列化失败要让调用保持等待（顺序：先序列化
   再记录 seq）；`store` 序列化失败抛 `TypeError`。Rust 需 `JsValue` 或"Option 表示 undefined + 省略键"策略，
   并保持键插入顺序（`serde_json` 的 `preserve_order`）。
10. **`renderDeclarations` 的精确排版**。单行 vs 多行、`{ ` / ` }` 空格、缩进、排序（UTF-16）、
    `@keyword` 顺序与取值（下界取 max、上界取 min、`new Set` 去重）、draft-04 布尔边界、超预算先丢 tag、
    `\uXXXX` 转义、`*/` → `*\/`、`$ref` 展开 32 次。这是模型实际看到的文本，任何偏差都改变行为。

### 8.3 次难点

- `proxy` 的 UTF-8 流式解码 + 行分割 + 无换行尾帧 + EOF 错误合成。
- `parseCodemodeSource` 的 `lines.findIndex` 语义（`trimStart()` 前缀匹配、CRLF）与 `code` 保留首换行。
- `checkGlobals` 的两阶段校验与命名空间冲突。
- `image()` 的校验路径（测试逐字覆盖 16 个脚本侧用例，其中 15 条落到 image）与签名表顺序
  （`image/jpeg` 的 `(?!9)` 否定前瞻）。
- `lockdown()` 的"可覆盖访问器"技巧与 `Intrinsics.ALL & ~WEAK_REF`。
- `stalled()` 的判定（`finished || pending.size > 0` → false，否则以 `Error` 结束并返回 true；在每次 drain 末尾调用）。
- `remote` 的批处理阈值（1000 项 / 1 Mi 字符）与 `end()` 幂等、`waiting` 先注册后发送、`stop` 回复。
- `Agent.continue()` 的回退顺序：尾部非 assistant → 直接用既有 context；assistant 尾 → steering 队列（`skipInitialSteeringPoll:
  true`）→ follow-up 队列 → 抛错；以及空/仅 system transcript 的 `No messages to continue from`。
- `declareToolChanges` 的 pending system 消息合并、`withToolChanges` 省略空列表、插入位置（第一个非 system 之前）、
  以及"caller 的消息已无变更时保留原对象"。
- `identity` 的逐字段合并与 `attribution: null`。
- `Agent.reset()` 用 `getCurrentSystemMessage` 生成 baseline（只保留一条 system 消息）并清空两个队列。
- `Agent.processEvents` 的"先改状态再 await listener"顺序，以及 `pendingToolCalls` 每次新建 `Set` 实例
  （以便 `ReadonlySet` 消费者看到新引用）。

### 8.4 移植顺序建议（可验证的里程碑）

1. **纯函数层**（无 VM、无网络）：`identifier.rs` → `source.rs` → `declarations.rs`；用 §7.5/7.6 的逐字断言锁定。
2. **协议与执行骨架**：`types.rs` + `protocol.rs` + `execution.rs`（用一个假 launcher 跑 `execution` 层测试）
   + `prelude.js` 的哈希测试。
3. **VM**：`vm.rs` + 确定性 WASI；先让 `sandbox.test.ts` 的 script execution/tools/store/globals 组通过，
   再补 limits/escape hatches。
4. **后端**：`inline.rs`（预算）→ `host.rs`（线程）→ `remote.rs`（transport）→ 可选 `cloudflare`。
5. **agent 包**：`types.rs` + `stream_fn.rs` + `proxy.rs`（用 §7.4 的假 fetch/SSE）→ `agent_loop.rs`（§7.1 的 fake
   stream）→ `agent.rs`（§7.2/7.3）。
6. **跨 crate 集成**：`kiana_ai` 的 transcript/identity/validation 必须先就绪（本单元只消费）。

---

## 附录 A：解析后的 prelude 全文（逐字节，17233 字符）

用途：Rust 侧必须把**下面这段文本原样**存为 `crates/kiana-codemode/src/prelude.js`（`include_str!`），
并在测试中断言 `len() == 17233`、FNV-1a == `8c6c9242`、SHA-256 ==
`57c8dc8c0e812117d82167fa751bc368f249d5aaee56d321066fc5280c785281`。

注意事项：
- 下面 fenced block 的内容**就是**解析后（`${...}` 已替换）的字符串；prelude 本身**末尾没有换行**，
  最后一个字符是 `)`。从本文件复制时不要带入 fence 前的那一个换行（校验：首字符 `(`，
  末 10 字符 `\t},\n\t};\n})`）。
- 缩进是 **TAB**（U+0009）；共 475 行（474 个 `\n`，末尾无换行）；无 CR、无 BOM、无非 ASCII 字符。
- 这段文本由 `prelude-source.ts` 的模板字面量在 12 处插值（5 个不同表达式）后得到：
  `262144`（2）、`1048576`（2）、`16777216`（2）、`100000`（2）、以及 `JSON.stringify(IMAGE_HELPER_EXPECTS)`
  的结果（4）。若 Rust 侧改为在构建期由模板生成，必须产出完全相同的字节。
- 文本中含两处单反引号（注释里的 `` `in` ``），不含三反引号，故可用 ``` 围栏安全嵌入。

```js
(function (bridge, toolsJson, globalsJson, storeJson) {
	"use strict";

	// Freeze every object reachable from the built-in globals, plus the intrinsics that are only
	// reachable from instances (iterator and generator prototypes, %TypedArray%).
	//
	// Freezing alone breaks ordinary code through the "override mistake": a data property that is
	// read-only on a prototype cannot be assigned on an instance either, so
	// this.name = "MyError" in an Error subclass would throw. Commonly overridden properties
	// become accessors whose setter defines an own property on the instance instead.
	(function lockdown() {
		const OVERRIDABLE = new Set(["constructor", "name", "message", "toString", "toLocaleString", "valueOf", "toJSON"]);
		const seen = new Set([globalThis]);
		const queue = [];
		const add = (value) => {
			if ((typeof value === "object" && value !== null) || typeof value === "function") {
				if (!seen.has(value)) {
					seen.add(value);
					queue.push(value);
				}
			}
		};

		// Accessors from an object literal have no own prototype object, unlike function expressions,
		// whose prototype.constructor would be converted again without end.
		function allowOverride(object, key, value, enumerable) {
			const { get, set } = Object.getOwnPropertyDescriptor(
				{
					get accessor() {
						return value;
					},
					set accessor(next) {
						if (this === object) {
							throw new TypeError("Cannot assign to read only property '" + String(key) + "' of a built-in");
						}
						if ((typeof this !== "object" || this === null) && typeof this !== "function") return;
						Object.defineProperty(this, key, { value: next, writable: true, enumerable: true, configurable: true });
					},
				},
				"accessor",
			);
			Object.defineProperty(object, key, { get, set, enumerable, configurable: false });
			add(get);
			add(set);
		}

		for (const key of Reflect.ownKeys(globalThis)) {
			const descriptor = Object.getOwnPropertyDescriptor(globalThis, key);
			add(descriptor.value);
			add(descriptor.get);
			add(descriptor.set);
			if ("value" in descriptor && descriptor.configurable) {
				Object.defineProperty(globalThis, key, { writable: false, configurable: false });
			}
		}
		add(Object.getPrototypeOf(function* () {}));
		add(Object.getPrototypeOf(async function () {}));
		add(Object.getPrototypeOf(async function* () {}));
		add(Object.getPrototypeOf(Int8Array));
		add(Object.getPrototypeOf([][Symbol.iterator]()));
		add(Object.getPrototypeOf(new Map()[Symbol.iterator]()));
		add(Object.getPrototypeOf(new Set()[Symbol.iterator]()));
		add(Object.getPrototypeOf(""[Symbol.iterator]()));
		add(Object.getPrototypeOf(/a/[Symbol.matchAll]("")));
		if (typeof Iterator === "function") {
			if (typeof Iterator.prototype.map === "function") add(Object.getPrototypeOf([].values().map((x) => x)));
			if (typeof Iterator.from === "function") add(Object.getPrototypeOf(Iterator.from({ next() {} })));
		}

		while (queue.length > 0) {
			const object = queue.pop();
			add(Object.getPrototypeOf(object));
			const descriptors = Object.getOwnPropertyDescriptors(object);
			for (const key of Reflect.ownKeys(descriptors)) {
				const descriptor = descriptors[key];
				if ("value" in descriptor) {
					add(descriptor.value);
					if (
						descriptor.writable &&
						descriptor.configurable &&
						(object === Object.prototype || OVERRIDABLE.has(key))
					) {
						allowOverride(object, key, descriptor.value, descriptor.enumerable);
					}
				} else {
					add(descriptor.get);
					add(descriptor.set);
				}
			}
			Object.freeze(object);
		}
	})();
	const stringify = JSON.stringify;
	const parse = JSON.parse;
	const promiseThen = Promise.prototype.then;
	const ErrorCtor = Error;
	const TypeErrorCtor = TypeError;
	const RangeErrorCtor = RangeError;
	const pending = new Map();
	let nextId = 1;
	let finished = false;
	// Thrown by exit() to unwind the script after it already reported success.
	const EXIT = Object.freeze({});

	function done(ok, payload, writes) {
		if (finished) return;
		finished = true;
		bridge("done", ok, payload, writes);
	}

	function serialize(value) {
		return value === undefined ? undefined : stringify(value);
	}

	// QuickJS stacks list frames only. Prefix "Name: message" like V8 so the
	// text reads the same as a Node error, and drop this prelude's frames.
	function errorText(error) {
		const head = error.message ? error.name + ": " + error.message : String(error.name);
		const frames =
			typeof error.stack === "string"
				? error.stack.split("\n").filter((line) => line.trim() && !line.includes("codemode-prelude.js"))
				: [];
		return [head, ...frames].join("\n");
	}

	function format(value) {
		if (typeof value === "string") return value;
		if (value instanceof ErrorCtor) return errorText(value);
		try {
			const json = stringify(value);
			return json === undefined ? String(value) : json;
		} catch {
			return String(value);
		}
	}

	// The host requires string fields. A script can set an error's name or message to anything, so
	// they are coerced, and a value whose coercion throws gets a placeholder.
	function describeError(error) {
		try {
			if (error instanceof ErrorCtor) {
				return stringify({ name: String(error.name), message: String(error.message), stack: errorText(error) });
			}
			return stringify({ message: String(format(error)) });
		} catch {
			return stringify({ message: "The script threw a value that cannot be described" });
		}
	}

	function caller(kind, name, spread) {
		return (...args) =>
			new Promise((resolve, reject) => {
				let json;
				try {
					json = serialize(spread ? args : args[0]);
				} catch (error) {
					reject(error);
					return;
				}
				const id = nextId++;
				pending.set(id, { resolve, reject });
				bridge(kind, id, name, json);
			});
	}

	const tools = Object.create(null);
	const allTools = [];
	for (const { name, jsName, description } of parse(toolsJson)) {
		const fn = caller("call", name);
		// The first tool wins when two names normalize to the same identifier.
		if (!(jsName in tools)) {
			tools[jsName] = fn;
			allTools.push(Object.freeze({ name: jsName, description }));
		}
		if (!(name in tools)) tools[name] = fn;
	}
	Object.freeze(tools);
	Object.freeze(allTools);

	// Reading a member that does not exist throws an error that names the close matches, instead of
	// a later "not a function". `in` checks still work.
	const comparable = (name) => name.toLowerCase().replace(/[^a-z0-9]/g, "");
	function guard(target, label, names, hint) {
		return new Proxy(target, {
			get(object, property, receiver) {
				if (typeof property !== "string" || property in object || property in Object.prototype || property === "then" || property === "toJSON") {
					return Reflect.get(object, property, receiver);
				}
				const wanted = comparable(property);
				const exact = names.filter((name) => comparable(name) === wanted);
				const close = exact.length > 0 ? exact : names.filter((name) => wanted && (comparable(name).includes(wanted) || wanted.includes(comparable(name))));
				let message = label + "." + property + " does not exist.";
				if (close.length > 0) message += " Did you mean " + close.slice(0, 5).map((name) => label + "." + name).join(", ") + "?";
				else if (names.length <= 20) message += " Available: " + names.join(", ") + ".";
				if (hint) message += " " + hint;
				message += ' Check for a member with "' + property + '" in ' + label + ".";
				throw new TypeErrorCtor(message);
			},
		});
	}
	const toolsProxy = guard(
		tools,
		"tools",
		allTools.map((tool) => tool.name),
		"ALL_TOOLS lists every tool; searchTools(query) finds tools by topic.",
	);

	const namespaces = new Map();
	for (const { name, spread } of parse(globalsJson)) {
		const fn = caller("global", name, spread);
		const dot = name.indexOf(".");
		if (dot === -1) {
			Object.defineProperty(globalThis, name, { value: fn, enumerable: true });
			continue;
		}
		const namespace = name.slice(0, dot);
		if (!namespaces.has(namespace)) namespaces.set(namespace, Object.create(null));
		namespaces.get(namespace)[name.slice(dot + 1)] = fn;
	}
	for (const [namespace, members] of namespaces) {
		Object.freeze(members);
		const value = guard(members, namespace, Object.keys(members));
		Object.defineProperty(globalThis, namespace, { value, enumerable: true });
	}

	// key -> JSON text. Sizes count key and JSON characters.
	const stored = new Map(Object.entries(parse(storeJson)));
	const writes = new Map();
	let storedChars = 0;
	for (const [key, json] of stored) storedChars += key.length + json.length;

	const STORE_HINT =
		"store() is for small state such as IDs or summaries. Show images with image(), keep large data in variables, or write it to a file with a tool.";

	function checkKey(name, key) {
		if (typeof key !== "string") throw new TypeError(name + "() key must be a string");
	}

	function store(key, value) {
		checkKey("store", key);
		const previous = stored.has(key) ? key.length + stored.get(key).length : 0;
		if (value === undefined) {
			stored.delete(key);
			storedChars -= previous;
			writes.set(key, undefined);
			return;
		}
		let json;
		try {
			json = stringify(value);
		} catch (error) {
			throw new TypeError("store(" + stringify(key) + ") value is not JSON-serializable: " + format(error));
		}
		if (json === undefined) {
			throw new TypeError("store(" + stringify(key) + ") value is not JSON-serializable");
		}
		if (json.length > 262144) {
			throw new RangeError(
				"store(" + stringify(key) + ") value has " + json.length + " characters of JSON, more than the limit of 262144. " +
					STORE_HINT,
			);
		}
		const next = storedChars - previous + key.length + json.length;
		if (next > 1048576) {
			throw new RangeError(
				"store is full: stored values would exceed 1048576 characters of JSON. Delete keys with store(key, undefined). " +
					STORE_HINT,
			);
		}
		stored.set(key, json);
		storedChars = next;
		writes.set(key, json);
	}

	function load(key) {
		checkKey("load", key);
		const json = stored.get(key);
		return json === undefined ? undefined : parse(json);
	}

	function serializeWrites() {
		const entries = [];
		for (const [key, json] of writes) entries.push(json === undefined ? [key] : [key, json]);
		return stringify(entries);
	}

	Object.defineProperty(globalThis, "store", { value: store, enumerable: true });
	Object.defineProperty(globalThis, "load", { value: load, enumerable: true });

	let outputChars = 0;
	let outputItems = 0;

	// Past the output limits the script fails: done() reports the error, so catching it does not
	// resume output, and the host ends the script.
	function output(kind, data, mimeType) {
		if (finished) return;
		outputChars += data.length;
		outputItems++;
		if (outputChars > 16777216 || outputItems > 100000) {
			const error = new RangeErrorCtor(
				"script output exceeded the limit of 16777216 characters or 100000 text(), image(), and console calls. " +
					"Print a summary instead, or write large data to a file with a tool.",
			);
			done(false, describeError(error));
			throw error;
		}
		bridge("output", kind, data, mimeType);
	}

	// Primitives become their string form, everything else JSON.
	function outputText(value) {
		if (value === undefined || value === null || typeof value !== "object" && typeof value !== "function") {
			return String(value);
		}
		const json = stringify(value);
		return json === undefined ? String(value) : json;
	}

	function text(value) {
		let rendered;
		try {
			rendered = outputText(value);
		} catch (error) {
			throw new TypeErrorCtor(error instanceof ErrorCtor ? error.message : String(error));
		}
		output("text", rendered);
	}

	function imageUrl(value) {
		if (typeof value === "string") return value;
		if (typeof value !== "object" || value === null || Array.isArray(value)) {
			throw new TypeErrorCtor("image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }");
		}
		if (value.image_url !== undefined) {
			if (typeof value.image_url !== "string") throw new TypeErrorCtor("image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }");
			return value.image_url;
		}
		if (typeof value.type !== "string") throw new TypeErrorCtor("image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }");
		if (value.type !== "image") {
			throw new TypeErrorCtor('image only accepts image blocks, got "' + value.type + '"');
		}
		if (typeof value.data !== "string" || value.data === "") throw new TypeErrorCtor("image expected image block data");
		if (value.data.toLowerCase().startsWith("data:")) return value.data;
		return "data:;base64," + value.data;
	}

	// Base64 of the signatures of the formats providers accept inline (PNG, JPEG except
	// JPEG-LS, GIF, "RIFF....WEBP"). Signatures start at byte 0, so their encodings are prefixes.
	const IMAGE_SIGNATURES = [
		["image/png", /^iVBORw0KGg/],
		["image/jpeg", /^[/]9j[/](?!9)/],
		["image/gif", /^R0lGOD[dl]h/],
		["image/webp", /^UklG.{8}RUJQ/],
	];

	function image(value) {
		const url = imageUrl(value);
		if (url === "") throw new TypeErrorCtor("image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }");
		const colon = url.indexOf(":");
		const scheme = colon === -1 ? "" : url.slice(0, colon).toLowerCase();
		if (scheme === "http" || scheme === "https") {
			throw new TypeErrorCtor("remote image URLs are not supported in tool outputs. Pass a base64 data URI instead");
		}
		const comma = url.indexOf(",");
		const header = comma === -1 ? [] : url.slice(colon + 1, comma).split(";");
		if (scheme !== "data" || comma === -1 || header.slice(1).every((part) => part.toLowerCase() !== "base64")) {
			throw new TypeErrorCtor("invalid image output. Pass a base64 data URI instead");
		}
		// Providers reject the whole request on a bad image, and a persisted image block would be
		// resent on every later turn. Line breaks from wrapped base64 are dropped. The declared type
		// is ignored in favor of the detected one, as providers also reject mismatches.
		const data = url.slice(comma + 1).replace(/\s+/g, "");
		if (data.length % 4 !== 0 || !/^[A-Za-z0-9+/]+={0,2}$/.test(data)) {
			throw new TypeErrorCtor("invalid image output. The image data is not valid base64 (truncated or corrupted?)");
		}
		const head = data.slice(0, 16);
		const signature = IMAGE_SIGNATURES.find(([, pattern]) => pattern.test(head));
		if (!signature) {
			throw new TypeErrorCtor("invalid image output. The image data is not a PNG, JPEG, GIF, or WebP image");
		}
		output("image", data, signature[0]);
	}

	function exit() {
		let writesJson;
		try {
			writesJson = serializeWrites();
		} catch (error) {
			done(false, describeError(error));
			throw EXIT;
		}
		done(true, undefined, writesJson);
		throw EXIT;
	}

	const console = {};
	for (const level of ["log", "info", "warn", "error", "debug"]) {
		console[level] = (...args) => {
			output("console", args.map(format).join(" "));
		};
	}
	Object.freeze(console);

	Object.defineProperty(globalThis, "tools", { value: toolsProxy, enumerable: true });
	Object.defineProperty(globalThis, "ALL_TOOLS", { value: allTools, enumerable: true });
	Object.defineProperty(globalThis, "console", { value: console, enumerable: true });
	Object.defineProperty(globalThis, "text", { value: text, enumerable: true });
	Object.defineProperty(globalThis, "image", { value: image, enumerable: true });
	Object.defineProperty(globalThis, "exit", { value: exit, enumerable: true });

	return {
		settle(id, ok, payload, propertiesJson) {
			const entry = pending.get(id);
			if (!entry) return;
			pending.delete(id);
			if (!ok) {
				const error = new ErrorCtor(payload);
				if (propertiesJson !== undefined) {
					const properties = parse(propertiesJson);
					for (const key of Object.keys(properties)) {
						if (key === "name" || key === "message" || key === "stack") continue;
						Object.defineProperty(error, key, { value: properties[key], writable: true, enumerable: true, configurable: true });
					}
				}
				entry.reject(error);
				return;
			}
			let value;
			try {
				value = payload === undefined ? undefined : parse(payload);
			} catch (error) {
				entry.reject(error);
				return;
			}
			entry.resolve(value);
		},
		run(fn) {
			let promise;
			try {
				promise = fn(toolsProxy, console);
			} catch (error) {
				done(false, describeError(error));
				return;
			}
			promiseThen.call(
				promise,
				(value) => {
					let json;
					try {
						json = serialize(value);
					} catch (error) {
						done(false, describeError(error));
						return;
					}
					done(true, json, serializeWrites());
				},
				(error) => {
					done(false, describeError(error));
				},
			);
		},
		stalled() {
			if (finished || pending.size > 0) return false;
			done(
				false,
				stringify({
					name: "Error",
					message:
						"The script is waiting on a promise that can never settle: no tool call is pending, and timers do not exist here.",
				}),
			);
			return true;
		},
	};
})
```


## Audit additions

本节是对前面章节的**补盲**（审计时新发现、在原章节结构里插入会破坏阅读顺序的条目）。编号 `A.n`；每条都注明了
源码位置与 Rust 侧要求。原章节中**已修正**的 21 处错误不再重复。

### A.1 kiana-ai 侧被本单元间接依赖、但原规格未展开的内部实现

agent 只调用 kiana-ai 的 13 个值符号，但其中 4 个的**行为**等价于一整段算法；Rust 端口若只按 §5.1 的表格实现会
在测试里失败。以下为逐行核对 `packages/ai/src/utils/**` 后的补充。

**A.1.1 `ToolCallArgumentsStream` / `StreamingJsonParser`（`utils/streaming-json.ts`，559 行）**

`ToolCallArgumentsStream` 的四个成员与精确语义（§2.1.6/§5.1 只写了"线性时间"）：

| 成员 | 语义 |
|---|---|
| `get text(): string` | 已收到的**原始参数文本**。只用于 `finish()`；永不进入 partial 消息或 transcript（#9265 的根因就是旧实现把文本挂在 content block 上）。 |
| `append(delta): ToolCall["arguments"]` | `#text += delta`；`#parser.append(delta)`；返回 `#partial()`。**参数未变时返回同一个对象**（`#partial` 比较 `parser.snapshot()` 与 `#snapshot` 的引用），变了才换新对象。 |
| `replace(text): { delta: string \| undefined; arguments }` | 若 `text.startsWith(#text)` → `delta = text.slice(#text.length)`；`delta === ""` 时返回现有 partial（不重新 append），否则 `append(delta)`。**否则**重置 `#text = ""`、`#parser = new StreamingJsonParser()`，`delta: undefined`，再 `append(text)`。 |
| `finish(): Pick<ToolCall, "arguments" \| "argumentsError">` | 直接 `return parseToolCallArguments(this.#text)`——**完整文本只解析一次**，且畸形参数在这里才报错（流式期间 `#partial` 从不抛）。 |

`#partial()` 的关键规则：`snapshot` 是**任意 JSON 值**；只有当它是"非 null、非数组的 object"时才作为
`arguments` 返回，否则返回 `{}`。两次调用之间 snapshot 未变（引用相同）时返回**同一个** `#arguments` 对象；
snapshot 变了但新值不是对象时，`#arguments` 被换成一个**新的** `{}`。因此流式早期 `arguments` 恒为 `{}`，
不会出现 `null`/数组/字符串（但空对象的引用身份可能变，依赖 `===` 的消费者要注意）。

`StreamingJsonParser`（内部类，不导出）必须复刻的常量与语义：

- `MAX_REPORTED_DEPTH = 32`、`CREDIT_PER_APPEND = 256`、`CREDIT_PER_CHARACTER = 8`、
  `MAX_PENDING_SCALAR_LENGTH = 64`。
- 复制信用模型：`append(text)` 先 `#credit += 256 + 8 * text.length`；`snapshot()` 在 `#dirty` 为假时返回**上一次
  快照的同一引用**；脏时计算 `cost = #pathCost + (有 pending scalar ? 1 : 0)`，`cost > #credit` 则**返回旧快照**
  （这就是"超大开放容器时预览略微滞后"的行为，也是 Rust 侧 O(n) 保证的来源），否则扣信用并重建。**Rust 若改成
  "每次全量重建"会在 520KB/16000 delta 的 proxy 测试里超时（10 秒断言）。**
- `snapshot()` 返回的容器是**开放路径的副本**（从内向外逐层 `slice()`/`{...}`），已完成容器与字符串**共享**；
  因此调用方拿到的旧快照永远不变（Rust 侧用 `Arc` 共享冻结子树 + 写时复制开放路径即可）。
- 宽容解析规则（与 `parseStreamingJson` 一致的子集）：未终止字符串显示已读字符；未完成转义被挂起；未开始时
  的 key/value 省略；未终止字面量在仍是 `true`/`false`/`null` 前缀时显示；未终止数字当且仅当已读数字构成数字时
  显示（`1e`、`1e+` 这类"指数无数字"被忽略，`NUMBER_WITHOUT_EXPONENT_DIGITS` 取尾数部分）。缺逗号（`{"a": 1 "b": 2}`）、
  缺冒号（`{"a" 1}`）都被容忍；非法转义（`\d`）保留反斜杠为文本；字符串内的原始控制字符**保留**（`repairJson`
  会转义，两者不同）；根值之后的文本忽略并置 `#done`；游离的 `}`/`]` 被忽略。**解析器永不抛错。**
- 与 `parseStreamingJson` 的三处**有意差异**（会体现为 partial 快照的逐字差异）：未终止字符串里的尾随空白**保留**、
  含原始控制字符或非法转义的字符串在流式期间**可见**、`NaN`/`Infinity` 单词被丢弃。
- `setMember` 对 key `"__proto__"` 必须走 `Object.defineProperty(..., {value, writable:true, enumerable:true,
  configurable:true})` 变成**自有成员**（等价 `JSON.parse`），不能赋值（否则会被原型 setter 吞掉）。Rust 的
  `serde_json::Map` 天然没有这个问题，但若用 `HashMap` + 自建对象语义需注意。
- `append()` 还返回 `readonly JsonChange[]`（`{kind:"set", path, value}` / `{kind:"append", path, text}`），
  路径深度超过 32 时整值作为一个 `set` 上报；`ToolCallArgumentsStream` 忽略它，但 Rust 侧若省略该能力不影响本单元。

**A.1.2 `parseToolCallArguments` / `repairJson` / `parseStreamingJson`（`utils/json-parse.ts`，149 行）**

- `repairJson(json)`：字符串内原始控制字符（U+0000..U+001F）被转成 `\b`/`\f`/`\n`/`\r`/`\t` 或 `\u00XX`
  （4 位小写十六进制）；`\` 后跟非合法转义字符时**再加一个反斜杠**（`\d` → `\\d`）；`\u` 后不是 4 位十六进制时同样
  双写；字符串在 EOF 处直接以反斜杠结尾时写成两个反斜杠。
- `parseJsonWithRepair(json)`：先 `JSON.parse`；失败且 `repairJson(json) !== json` 时再 `JSON.parse` 修复版；
  否则**抛出原始错误**。
- `parseToolCallArguments(json | undefined)`：
  - `!json?.trim()` → `{ arguments: {} }`（**无 `argumentsError` 键**）。
  - `parseJsonWithRepair` 后：非 object / `null` / 数组 → 抛 `Error("Tool arguments must be a JSON object")`。
  - catch → `{ arguments: {}, argumentsError: `${msg}\n\nReceived arguments:\n${json}` }`。
  - **Rust 侧必须逐字保留 `"\n\nReceived arguments:\n"` 这两行的格式**：它会被 `validateToolArguments` 原样拼进
    `Invalid arguments for tool "..."`，并被模型看到。
- `parseStreamingJson`（本单元不直接用，但 `StreamingJsonParser` 的文档与旧行为以它为参照）：空/空白 → `{}`；
  否则 `parseJsonWithRepair` → 失败再用 `partial-json` 包的 `parse` → 再失败用 `parse(repairJson(...))` → 最后 `{}`。
  这引入了一个**未在 §6 列出的第三方依赖 `partial-json@0.1.7`**（见 A.4）。

**A.1.3 `validateToolArguments`（`utils/validation.ts`，353 行）——§5.1 第 1-4 条的展开**

原规格第 2 条只写"`normalizeOptionalNulls`（把可选字段的 null 归一为删除/undefined）"。精确规则：

- 递归下钻 `items`（元组用 `schema.items[i]`，数组用同一个 `items`）、`allOf`/`anyOf`/`oneOf` **不**下钻。
- 只处理 `typeof value === "object" && value !== null && schema.properties` 的对象。
- 删除条件**全部满足**：`key in object`、`object[key] === null`、`!required.has(key)`、
  `typeof propertySchema.$ref !== "string"`、`getSubSchemaValidator(propertySchema)?.Check(null) === false`。
  注意 `required` 取的是**当前对象** schema 的 `required` 数组；带 `$ref` 的属性**不删**。
- 非 TypeBox schema（`Object.getOwnPropertySymbols(tool.parameters)` 不含 `Symbol.for("TypeBox.Kind")`）时，在
  `Value.Convert` **之后**额外做 `coerceWithJsonSchema`（`allOf` 依次、`anyOf`/`oneOf` 先"原值能过就用原值"再逐个
  `structuredClone` + 强转后校验、联合类型只在不匹配时才强转、`object`/`array` 递归、`additionalProperties` 为对象时
  也递归）。强转结果若是对象，用"清空原对象再 `Object.assign`"**原地替换**（保持调用方 `structuredClone` 的那份
  引用被后续 `beforeToolCall` 修改时语义一致）。
- 错误格式：`validator.Errors(args).map(e => `  - ${formatValidationPath(e)}: ${e.message}`).join("\n")`；
  若为空串则 `"Unknown validation error"`。完整消息为
  `Validation failed for tool "${toolCall.name}":\n${errors}\n\nReceived arguments:\n${JSON.stringify(toolCall.arguments, null, 2)}`
  ——**末尾回显的是 `toolCall.arguments`（模型原始输入），不是 `structuredClone` + `normalizeOptionalNulls` +
  `Value.Convert` 之后的 `args`**；两者可能不同（例如原始里是 `"5"`，校验后是 `5`）。这里的 `JSON.stringify(..., null, 2)`
  是**两个空格缩进的多行**文本，与 `argumentsError` 的 `Received arguments:\n${json}`（原文单行）不同。
- `formatValidationPath(error)`：`keyword === "required"` 时取 `params.requiredProperties[0]`，`instancePath` 去掉前导
  `/`、把 `/` 换成 `.`，非空则 `${basePath}.${prop}`，空则 `prop`；其他情况去掉前导 `/` 再换 `/` → `.`，
  空则 **`"root"`**。`root` 是**模型可见文本**的一部分（§3.2 的 `- path: message` 里 path 可能是 `root`）；
  本单元的两个测试文件**没有**逐字断言这条消息（`agent-loop.test.ts:2167` 只断言 `Tool missing not found`），
  但 coding-agent/其它单元可能依赖，Rust 侧仍应逐字复刻。
- `getValidator` 用 `WeakMap<schema, Compile(schema)>` 缓存（同一 schema 对象复用编译器）；`Compile` 来自
  `typebox/compile`。Rust 侧可用 `OnceCell` + 按指针身份缓存，或直接每次编译（测试不检查缓存）。
- `validateToolCall(tools, toolCall)`（同文件导出，**agent 不使用**）：找不到工具 → `Tool "${name}" not found`。
  Rust 侧若实现 `kiana_ai` crate 需要它，但它不属于本单元的调用面。

**A.1.4 `mergeClientIdentity` / identity 其余辅助（`utils/identity.ts`，193 行）**

§5.1 只列了 `mergeClientIdentity` 与 `KIANA_ATTRIBUTION`。`agent-loop.ts` 的 `DEFAULT_AGENT_IDENTITY` 之上的行为取决于：

- `ClientIdentity { userAgent?: "short" | "detailed" | { custom: string }; attribution?: ClientAttribution | null;
  version?: string }`——`agent.ts` 的 `AgentOptions.identity` 就是这个类型；**§2.1.2 未列出 `ClientIdentity` 的字段**，
  而 `AgentOptions.identity` / `Agent.identity` 的可写字段（`userAgent`/`attribution`/`version`）是公开 API。
- `mergeClientIdentity(...sources)`：跳过 `undefined`；每个非空来源处 `merged = { ...merged }`（**每次都新建对象**，
  所以传入的源对象与其返回值永远不会互相影响；最终结果是独立对象），只覆盖 `!== undefined` 的字段；
  `attribution: null` 是**有效值**（用于关闭合并链上更早的来源）。全为 `undefined` 时返回 `undefined`（不是 `{}`）。
- 实际发给 provider 的**头部**由 kiana-ai 内部完成，不在本单元：`formatUserAgent`（`detailed` →
  `kiana/<version> (<platform>; node/<ver>|bun/<ver>; <arch>)`，无 `process` 时 `kiana/<version> (browser)`；`short` → `kiana`；
  自定义对象 → 原值）、`getAttributionHeaders`（OpenRouter：`HTTP-Referer` + `X-OpenRouter-Title` + 可选
  `X-OpenRouter-Categories`；NVIDIA：`X-BILLING-INVOKE-ORIGIN`，名字首字母大写；OpenCode：`x-opencode-client`）、
  `getOpenCodeSessionHeaders`、`getIdentityHeaders`。**agent 只负责把合并后的 identity 放进 stream options**；
  Rust 端口若把 kiana-ai 一起移植，这些函数的输出必须逐字一致，否则真机请求头会变。
- 契约（§7.2 第 36 条测试逐字依赖）：`DEFAULT_AGENT_IDENTITY` = `{attribution: KIANA_ATTRIBUTION}`；
  `agent.identity = {userAgent:"short", attribution: acme}` → `{attribution: acme, userAgent: "short"}`；
  `agent.identity = {userAgent:"short"}` → `{attribution: KIANA_ATTRIBUTION, userAgent: "short"}`（默认 attribution 仍在）。

**A.1.5 transcript 其余辅助（`utils/transcript.ts`）——agent 不直接用，但 Rust 的 `kiana_ai` crate 需要**

| 符号 | 语义（与 §5.1 的已列项互补） |
|---|---|
| `contentText(content, separator = "\n")` | string → 原样；数组 → 只取 `type === "text"` 的 `text`，用 `separator` 连接（**默认单换行**）。`getSystemMessageText` 的 sections 用 `"\n\n"` 连接是在**外层**做的，别混淆。 |
| `getInitialSystemMessage(messages)` | 仅当 `messages[0]` 是 system 时返回它。 |
| `withoutInitialSystemMessage(messages)` | 首条是 system 则 `slice(1)`，否则原数组。 |
| `getDeclaredTools(messages)` | 遍历所有 system 消息的 `toolsAdded`，按**首次声明顺序**去重（`Map` 保插入序）。 |
| `hasToolRedefinitions(messages)` | 同名不同定义 → true；**已 `@deprecated`**（Anthropic 用 inline `tool_definition`）。 |
| `hasNonAdditiveToolChanges(messages)` / `findLastNonAdditiveToolChange` | 最后一次"有 `toolsRemoved` 或同名重复声明"的 system 消息索引；被移除过的名字再声明也算重复（Kimi K3 行为，2026-10 验证）。 |
| `collapseSystemMessages(context)` | `head = getCurrentSystemMessage(...)`；过滤掉所有 system；`head ? [head, ...rest] : rest`。 |
| `resolveTranscript(context, supportsMidConvoSystemMessages)` | 支持中途 system 消息则原样，否则 `collapseSystemMessages`。 |
| `renderSystemMessageUpdate(message)` | 中途 system 消息的渲染：`contentText` 非空先入列；每个 section → `Removed system prompt section "<name>".` 或 `Updated system prompt section "<name>":\n\n<value>`；`"\n\n"` 连接。**仅在请求期使用，跨版本可变**。 |
| `ownedSlice(text, start, end)` | V8 切片逃逸优化（避免短摘录持有大字符串）；Rust 侧无等价问题，忽略即可。 |

`getCurrentSystemMessage` 的**精确算法**（§5.1 只写了"重放"）：遍历所有 system 消息，`timestamp ??= message.timestamp`
（**第一条** system 消息的时间戳）；`text = contentText(message.content)`，非空才 push 到 `content[]`；`sections` 用
`Map<string,string>`，`value === null` → `delete`，否则 `set`（**同名后写覆盖**，`Map` 保留首次插入位置）；
`tools = getCurrentTools(messages)`；`timestamp === undefined && tools.length === 0` → `undefined`；
否则返回 `{role:"system", content: content.join("\n\n"), ...(sections.size > 0 ? {sections: Object.fromEntries(sections)} : {}),
...(tools.length > 0 ? {toolsAdded: tools} : {}), timestamp: timestamp ?? 0}`。
**注意 `timestamp: timestamp ?? 0` 与 `content` 恒存在（可空串）**——`Agent.reset()` 的 baseline 依赖这一点。

`getCurrentTools(messages)`：`Map<name, Tool>`，对每条 system 消息**先**按 `toolsRemoved` 逐个 `delete`，**再**按
`toolsAdded` 逐个 `set`；返回 `[...map.values()]`（插入序 = 首次声明序，重声明不改位置）。

### A.2 `StopReason` 与状态机里的"未列出"取值

- kiana-ai 的 `StopReason = "pending" | "stop" | "length" | "toolUse" | "error" | "aborted" | "deferred"`
  （`packages/ai/src/types.ts:489`）。本单元实际出现的只有 `pending`（proxy 初始 partial）、`stop`/`length`/`toolUse`
  （proxy `done` 的 `Extract`）、`error`/`aborted`（硬退出与 proxy `error`）。**`"deferred"` 不出现**，但
  `SimpleStreamOptions.deferred` 字段会随 `{...config}` 透传给 streamFn（§2.1.4 第 5 步），Rust 结构体必须保留该字段
  并能序列化，即使 agent 不解释它。
- `Agent.continue()` 的"无消息"判定是 `!lastMessage || messages.every(m => m.role === "system")`——**整段只有 system
  消息**（不只是空数组）也抛 `No messages to continue from`；而低层 `agentLoopContinue` 的两条同步错误文本是
  `Cannot continue: no messages in context` / `Cannot continue from message role: assistant`（**与 `Agent` 的文本不同**，
  §3.2 已分别列出，不要合并）。
- `agentLoopContinue` 中这两条错误在**同步阶段**抛出（在 `createAgentStream()` 之前），因此调用方拿不到
  `AgentMessageEventStream`（异常直接冒出）；`Agent.continue()` 的同名前置校验也是同步抛出，但文本不同（见上）。
  `agentLoop`/`agentLoopContinue` 内部的其他异常（如 `convertToLlm` 抛错）发生在异步 IIFE 中，流**不会 end**、
  `result()` 永远 pending（§2.1.4 的 `agentLoop` 小节已写明这一点；此处只强调它同样适用于 `agentLoopContinue`，
  且 Rust 侧的 "clone 出来的事件流" 必须能被 drop 而不 panic）。

### A.3 测试清单的补充细节（原 §7 未覆盖）

**A.3.1 `sandbox.test.ts` 的 describe 分块与运行次数（便于逐块移植）**

`describe.each(Object.keys(BACKENDS))("%s backend")` 内部有 **6** 个 describe：`script execution` 12、`tools` 9、
`store and load` 5、`globals` 4、`limits and lifetime` 9、`escape hatches` 8 → 12+9+5+4+9+8 = **47** 个 `it`
（× 3 个后端 = 141 次运行）。`describe.each` **之外**还有顶层 `describe("embedded sources")` 1 个 `it`，以及
`describe("worker host")` 的 4 个 `it` + 1 个 `it.each`（7 例）。声明数 47+1+4+1 = **53 个 `it` 声明**（`it.each`
按 1 个声明计），运行次数 141+1+4+7 = **153**（§7.7 的"53 个 it 声明 / 153 次运行"正确）。`BACKENDS` 为 `worker`/`inline`/`remote` 三个键，
`describe.each` 的回调里 `beforeEach` 把模块级 `backend` 设成当前键；`worker host` 的 `beforeEach` 把它设回 `"worker"`。

**A.3.2 `limits and lifetime` 里几条容易漏的断言**

- 同步死循环 `while (true) {}`：`timeoutMs: 200`，结果 `{ok:false, error:{kind:"timeout"}}`，并断言
  `performance.now() - started < 5_000`（**不是** 200ms，只是上限）。
- `timeoutMs: Infinity` + 一个 50ms 的真实 `setTimeout` 工具 → `{ok:true, value:"late"}`（证明没有 deadline 时工具可以
  慢慢来）。
- "永远无法 settle 的 promise"：`await tools.echo(1); await new Promise(() => {}); return 'never'` → `kind:"script"`，
  message 含 `"can never settle"`；**但** `tools.echo(2); return 'early'`（返回时仍有未决调用）是**成功**的——
  `stalled()` 的判定是 `finished || pending.size > 0` 才返回 false，返回时 pending 非空所以不算 stall。
- 输出超限的三条脚本（`text(s)` / `console.log(s)` / `image("data:image/png;base64," + p)`，`s = "x".repeat(1<<20)`，
  `p = "iVBORw0KGgoA" + "A".repeat(1<<20)`，`for(;;){ try { print } catch {} }`）都失败且 `error.name === "RangeError"`，
  message 含 `"script output exceeded"`；测试还会检查 `result.output` 的**字符总数**
  `chars = Σ(item.type === "text" ? item.text.length : item.data.length)` 满足
  `MAX_OUTPUT_CHARS - 2 MiB <= chars <= MAX_OUTPUT_CHARS`（宿主保留全部输出，所以脚本不能靠 catch 无限打印）。
  紧随其后的 `for (;;) text("")` 用例：`error.name === "RangeError"` 且 `output.length === MAX_OUTPUT_ITEMS`
  （**恰好** 100000），即条目上限与字符上限分别生效。
- abort 用例一共**三次** `execute`，顺序不能"简化"：
  1. `sandbox.execute("await tools.hang(); return 'never'")`（**不传 signal**）→ 工具挂起，测试用 `nextCall()` 的
     promise 等到工具真的被调用。
  2. 同一个 `controller.abort(new Error("user cancelled"))`；此时 `expect(toolSignal?.aborted).toBe(false)`——
     **第一次执行没有 signal，所以它的工具 signal 不受影响**。
  3. 用**已 abort 的** signal 再 `sandbox.execute("await tools.hang()", {signal: controller.signal})` →
     `Execution` 构造时"signal 已 aborted → 立即 `onAbort()`"，`{kind:"aborted", message:"user cancelled"}`，
     **不会** 调用工具（launch 被跳过）。
  4. 换第二个 `AbortController`，`execute("await tools.hang(); return 'never'", {signal: second.signal})`，
     等工具被调用后 `second.abort()` → `{kind:"aborted"}` 且 `calls` 为 `[{name:"hang", status:"cancelled"}]`，
     `toolSignal?.aborted` 此时才为 **true**。
  5. 最后 `sandbox.close()`：**第一次**（无 signal 的）执行以 `{kind:"aborted", message:"Sandbox closed"}` 结束。
  Rust 侧需要能同时持有三个 in-flight execution，且 `close()` 只影响仍在跑的。

**A.3.3 `worker-lifetime.test.ts` 的 `runtimes` 探测与真实进程生命周期**

- `runtimes = [{name:"Node", command: process.execPath, available: true},
  {name:"Bun", command: "bun", available: spawnSync("bun",["--version"],{timeout:5000}).status === 0}]`。
- `describe.each(runtimes)("$name process lifetime")` 里只有 1 个 `it.skipIf(!available).each(["abort","timeout","close"])`
  （3 例 × 2 运行时）；标题按运行时不同：Node 是 `exits promptly after %s with a real worker`，Bun 是
  `settles %s promptly with a real worker`。
- 断言：stdout 恰好 **2 行** JSON；第 1 行 `{type:"result", kind: how === "timeout" ? "timeout" : "aborted", elapsedMs}`，
  `kind` 匹配；第 2 行 `type === "exit"`；`result.elapsedMs < 1000`；仅 Node 额外断言
  `exit.elapsedMs - result.elapsedMs < 1000`。**Bun 分支不断言进程退出时间**（Bun 1.4.0 的 pending `terminate()` 会
  拖住宿主进程，这是测试注释里写明的已知问题）。
- 该 `it.each` 的超时是 **25000 ms**（第三个参数），不是 vitest 默认值。

**A.3.4 `deterministic.test.ts` 的 `PINNED_*` 常量**

`PINNED_FIRST_RANDOM`、`PINNED_SEED_ZERO` 是测试文件顶部的**字面量数组/数字**，不是计算出来的：Rust 侧必须把
§2.2.8 的链条跑出**完全相同**的 f64 位模式才能对上（`0.6711372530266764`、`0.7258461452833667`、
`0.34227450605335275`）。另外 `start()` 辅助默认 `tools: [{name:"echo"},{name:"wait"}]`，`answer` 默认
`call => ({ok:true, value: call.args})`，`onCall` 里对非 `wait` 的调用 `queueMicrotask(() => { seq++; run.deliver(seq,
STARTED_AT + 1000*seq, call.id, answer(call)) })`——**用 microtask 而不是同步 deliver**，这会影响 `idles` 的观察顺序。

**A.3.5 `portable.test.ts` 的 `recordingRemote`**

远端每次 `execute` 都对 `request`、每个 `messages`、每个 `reply` 做 `structuredClone`（模拟 Workers RPC 的**深拷贝**）；
`exchanges.push(messages.length)` 记录每次 RPC 的消息数（第 4 条 test 断言 `<= 6`）。Rust 侧用 `serde_json::Value`
往返即可，但**必须真的往返**（不能把 `&mut` 传过去），否则"批处理"与"stop 回复"的引用语义会与 TS 不同。

### A.4 依赖清单缺项

§6 的表格没有列出本单元实际会**间接**触达的两个包：

| npm 包 | 用途 | 建议 Rust |
|---|---|---|
| `partial-json@0.1.7` | 仅 `parseStreamingJson()` 使用（`utils/json-parse.ts`）；`ToolCallArgumentsStream` **不**用它（自带 `StreamingJsonParser`）。 | 本单元不必移植；若移植 `parseStreamingJson`，需复刻 partial-json 的容错解析（或用 `json5`/自写前缀解析，并在 `kiana_ai` crate 的测试里锁定行为）。 |
| `cloudflare:workers` + `quickjs-wasi/quickjs.wasm`（编译期导入） | 仅 `/cloudflare` 入口；由 Wrangler 的 `CompiledWasm` 规则提供。 | 可选；Rust 端口若不目标 Cloudflare 可跳过整个 `cloudflare.rs`，但要保留 `serveCodemodeRemote`（`/portable` 已有）。 |

另外 §1.3 的 `packages/agent/package.json` 段与 §2.1.8/§2.2.17 的 metadata 段**缺少**以下字段（审计时逐字核对）：

- agent：`description: "General-purpose agent with transport abstraction, state management, and attachment support"`、
  `keywords: ["ai","agent","llm","transport","state-management"]`、`author: "Mario Zechner"`、`license: "MIT"`、
  `repository: {type:"git", url:"git+https://github.com/earendil-works/kiana.git", directory:"packages/agent"}`、
  `scripts: {clean: "shx rm -rf dist", build: "tsc -p tsconfig.build.json", test: "vitest --run",
  prepublishOnly: "npm run build"}`、`devDependencies: {"@types/node":"22.19.19", vitest:"4.1.12"}`。
- codemode：`description: "Sandboxed JavaScript execution where the only capability is calling injected tools"`、
  `keywords: ["kiana","codemode","sandbox","quickjs","wasm","cloudflare","durable-objects"]`、`author: "Earendil Works"`、
  `license: "MIT"`、`repository: {..., directory:"packages/codemode"}`、
  `scripts: {clean, build, test, prepublishOnly: "npm run clean && npm run build"}`（**与 agent 不同，先 clean**）、
  `devDependencies: {shx:"0.3.4", vitest:"4.1.12"}`。
- 两个包的 `files` 都是 `["dist","README.md"]`（§1.3 未写），`engines.node` 都是 `">=22.19.0"`。
- `codemode/tsconfig.build.json` 的 `files` 含 `src/cloudflare-modules.d.ts`（已写），但**未写**它同时把
  `src/**/*.ts` 作为 `include` 且 `exclude` 里有 `node_modules`/`dist`；agent 的 `tsconfig.build.json` 已在 §2.1.8 列出。
- 根 `tsconfig.base.json`：`target ES2024`、`module Node16`、`lib ["ES2024"]`、`strict`、`erasableSyntaxOnly`、
  `verbatimModuleSyntax`、`esModuleInterop`、`declaration`/`declarationMap`/`sourceMap`/`inlineSources`、
  `moduleResolution Node16`、`resolveJsonModule`、`allowImportingTsExtensions`、`rewriteRelativeImportExtensions`、
  `types: ["node"]`（§7.12 只写了前四项）。
- 根 `vitest.base.ts` 是一个**导出 `workspaceSourcePaths` 的模块**，不只是 `defineConfig`：它把
  `chord`/`telemetry`/`mcp`/`ai`/`agent`/`codemode`/`coding-agent`/`tui` 的 `src/index.ts` 都映射到源码
  （`agentIndex`、`codemodeIndex`、`codemodeDeclarations`、`codemodeSource`）。本单元的两个包在根配置里也有别名。

### A.5 codemode 侧被原规格略过的行为

**A.5.1 `remote` 的"远端 VM 会继续计算"语义（README「Limits on Cloudflare」与 `remote.ts` 头注释）**

§2.2.12 只写了"client 回答 `{type:"stop"}`，remote 在那一刻停下"。精确承诺（**Rust 侧必须写进文档，且不能等待
远端退出**）：

- 调用方侧（`timeoutMs`/`signal`/`close()`）立即以 `kind:"timeout"`/`"aborted"` 结束，**不等远端**。
- 远端在**下一次**要发 `exchange`（即下一次工具调用 / 缓冲区 flush / 终态）时才看到 `stop`；一个正在纯计算的远端
  VM **会继续跑**，直到它调用工具或**用完中断预算**。后者以 `failure` 消息上报，客户端的 `remoteLauncher` 把它转成
  与本地 VM 相同的 `kind:"timeout"`。
- 若远端 isolate 崩溃/被重置/被重新部署，或 `execute` 返回而没有终态消息 → `kind:"sandbox"`
  （`Remote sandbox failed: ...` / `Remote sandbox returned before the script settled`）。
- 同一个 Durable Object 实例上并发到达的 execution **共享线程**：等待工具时交错，纯计算时互相阻塞。测试与文档都
  依赖"按 session 命名实例"（`getByName(sessionId)`）的做法。
- **`exchange` 每次最多携带一个 `call`**（并发工具调用 = 并发 RPC）；同一 stub 上的调用按序投递（因此 call/result
  之前的输出总是先到）。

**A.5.2 中断预算只在"VM 内联运行"时是唯一上限**

`InlineCodemodeSandbox` 与 Remote 的远端 VM 都在**调用线程/远端 isolate 内联**运行：脚本纯计算时宿主事件循环不跑，
`timeoutMs` 与 `signal` 只在脚本 `await` 工具时生效；此时唯一的停止手段是 `interruptBudget`（每次 `shouldInterrupt`
轮询计 1，默认 100000）。README 给出量级：100000 次轮询在 Apple M 系列笔记本约 **2 秒**、在 Cloudflare 约
**10 秒**；预算应低于 Worker 的 `cpu_ms`（默认 30 秒），否则对象被重置而不是脚本失败。

VM 内共有**两条**取得控制权的路径，都调用同一个 `shouldInterrupt`：
1. QuickJS 自身的 `interruptHandler`——VM 执行字节码期间由引擎周期性调用（同步死循环靠它停）；
2. `drain()` 的 `while (qjs_is_job_pending())` 循环——每 **50** 个 job 检查一次（`JOBS_PER_POLL = 50`，§2.2.8 已写
   该常量与循环，此处只补"这是第二条路径"以及它与 `interruptHandler` 计的是**同一个预算**）。
命中后 `drain()` 抛 `Error("Execution interrupted")`，`qjs_execute_pending_job() < 0` 抛
`Error("Promise job failed")`；两者最终都以 `crash` 或失败的 `done` 上报，inline launcher 再按 `exhausted`
改写成 `kind:"timeout"`（因此 Rust 侧若只实现其中一条路径，`while (true) await null` 或 `while (true) {}` 会有一个
永远停不下来）。

**A.5.3 `handleCall` 的 JSON 解析与参数错误**

`handleCall` 用 **`JSON.parse(message.args)`**（不是 `parseBridgeJson`）：畸形 args 被 catch 成**工具错误**
（`{type:"result", id, ok:false, payload: <SyntaxError message>}`，`record.status = "error"`），而不是
`Sandbox bridge broken`。Rust 侧必须保留这个区分（`serde_json::from_str` 的错误文本会不同，建议映射成 V8/QuickJS 风格
的 `Unexpected token ... in JSON at position N`；若无法精确，至少保证 `kind` 与 `isError` 语义）。

**A.5.4 `finish()` 的 `storeWrites` 缺省对象**

成功结果但 `writes === undefined` 时返回**新的** `{ set: {}, delete: [] }`（不是共享常量）；失败结果**没有**
`storeWrites` 键。`calls`/`output` 始终是 `Execution` 内部的数组引用（Rust 侧返回克隆或 `Arc` 均可，但
`result.calls` 与 `result.output` 的顺序必须与事件顺序一致）。

**A.5.5 `serveCodemodeRemote` 的内存上限叠加**

`requestedMemory === undefined` → 用 `maxMemory`；`maxMemory === undefined` → 用请求值；两者皆有 → `min`。
**注意**：`maxMemoryLimitBytes` 为 `undefined` 时**不会**让客户端任意分配——`InlineCodemodeSandbox` 的 `startVm`
仍受 wasm32 4 GiB 限制；但 Cloudflare 入口默认 `32 MiB`（`DEFAULT_REMOTE_MEMORY_LIMIT_BYTES`），
`CodemodeSandboxDurableObject` 用受保护字段暴露它（子类可覆盖）。

### A.6 Rust 映射的补充（§8 的缺口）

1. **两套栈上限**：worker 后端用 `MAX_STACK_SIZE`（quickjs-wasi 的默认 **512 KiB**），inline 后端用
   `INLINE_MAX_STACK_SIZE = 256 * 1024`（注释：约 1000 层递归，且远早于 1 MB 宿主栈溢出）。§8 只提到
   `maxStackSize`，未强调**inline 必须更小**——Rust 若用 `std::thread` 默认栈（8 MB）或 tokio 线程栈，深递归测试的
   "可 catch 的 RangeError"会变成真实栈溢出（abort 进程）。
2. **`Intrinsics.ALL & ~Intrinsics.WEAK_REF`**：确定性模式把 `intrinsics` 换成"全部减去 WEAK_REF"这份掩码；实测效果是
   `typeof WeakRef === "undefined"` **且** `typeof FinalizationRegistry === "undefined"`（`deterministic.test.ts` 第二个 `it`
   "runs in UTC whatever the host's timezone, and without WeakRef or FinalizationRegistry" 的逐字断言）。`WEAK_REF` 具体覆盖哪些位需对照锁定的 quickjs-wasi 3.6.2 的 `Intrinsics` 定义确认（本地仓库未安装该包，
   无法离线核对）；Rust 若自建 VM，需在**创建上下文时**移除这两个全局，而不是在 prelude 里删除（prelude 的
   `lockdown` 只冻结，不删除）。
3. **`timezoneOffset: 0`**：确定性模式下 QuickJS 的本地时区为 UTC，`new Date(0).getHours() === 0`、
   `getTimezoneOffset() === 0`；同一脚本在 `execute()` 中（宿主 `TZ=Asia/Kolkata`）得到 `[5, -330]`。Rust 的
   `chrono`/`time` 默认 UTC，但若使用宿主本地时区就会失败——必须显式固定。
4. **`vm.versions`**：指纹里的 `versions["quickjs-wasi"]` 与 `versions.quickjs` 是 quickjs-wasi 暴露的字符串；Rust 若换
   引擎，只能固定成常量并写进文档/测试（§8.2 第 4 点已提，但未给出"固定常量"时的推荐值格式：仍须匹配
   `/^quickjs-wasi \S+, quickjs \S+, ...$/` 两个 `\S+`）。
5. **`_getExports()`**：`qjs_is_job_pending()` / `qjs_execute_pending_job()` 是**内部但公开**的导出，
   quickjs-wasi 被精确锁定到 `3.6.2`；`rquickjs` 没有等价的"导出表"访问，需要用该绑定提供的 job 队列驱动入口（以锁定的
   版本为准）或自己驱动，并且**必须**保留"每 50 个 job 检查一次 `shouldInterrupt`"的行为（否则 `while (true) await null`
   在 inline 后端不会被中断，§7.7 的微任务自旋测试会挂）。
6. **`queueMicrotask` 的重入保护**：inline launcher 的 `post` 把每条 VM 消息放进 `queueMicrotask`，保证宿主代码
   （工具调用、`finish()`、`dispose()`）**永远不在 VM 回调内运行、也永不重入 VM**。Rust 侧若同步调用宿主回调，
   `finish()` → `session.dispose()` 会在 QuickJS 执行中途 dispose，导致 use-after-free 或 panic。必须用
   `tokio::task::yield_now`/channel 把消息送到 VM 之外再处理（§2.2.8 第 14 条的"调用方不得从 `post` 重入 VM"是同一
   约束的另一种表述，但 Rust 侧需要具体的调度机制）。
7. **无 timer / 无 IO**：VM 内没有 `setTimeout`/`fetch`/`process`/`require`/模块/`WebAssembly`；WASI 的 `fd_write` 被
   `discardOutput` 吞掉（引擎诊断不污染 stdout）。Rust 若用 `wasmtime` + WASI preview1，必须覆盖 `fd_write` 为丢弃
   iovec 内容但仍写入 `nwritten`（否则 QuickJS 的错误输出会打到宿主 stderr，且 `nwritten` 错误会让引擎重试）。
8. **`structuredClone` 的每消息开销**：remote 测试对每条消息做真实深拷贝；Rust 用 `serde_json::to_value`/`from_value`
   时注意 `NaN`/`Infinity` → `null`、`undefined` 字段被省略、`-0` → `0`（§6.1 已列，但 remote 路径的**性能**也依赖
   "只克隆一次"——不要在每个 exchange 里多次克隆）。
9. **`unref()`/`detach`**：Node 的 `worker.unref()` 在 `terminate()` 之后调用，使宿主进程不被等待中的 worker 拖住；
   Rust `std::thread` 无法 unref，必须 `detach`（且**不** join）。`onWorkerStarted(exited)` 的 `exited` 在真实退出时
   resolve——Rust 侧用一个 `oneshot`/`Barrier` 在 detach 前注册退出通知。
10. **`InternalError: out of memory`**：`memoryLimitBytes` 超限在脚本内表现为 QuickJS 的 `InternalError: out of memory`
    （脚本能 catch 到的 `Error`，其 `name === "InternalError"`），在 remote 测试里通过 `JSON.stringify(result)`
    包含 `"out of memory"` 来断言。Rust 若用 `wasmtime` 的 `StoreLimits`，超限会变成 trap（`unreachable`），
    需要转换为同样的错误文本/类型。
11. **`write` 到 wasm 内存的字节序**：确定性 WASI 的 `clock_time_get` 用**小端 u64**、`random_get` 按 4 字节一组
    小端写（最后一组截断）；`fd_write` 的 iovec 为 `(ptr, len)` 每 8 字节，`nwritten` 是 u32 小端。Rust 侧若自己实现
    WASI shim，必须逐字节一致（否则 QuickJS 读到的时间/随机数会完全不同）。

### A.7 与"只读参考"约束相关的提醒

- 源仓库 `/home/ctyun/code/kiana_rev/kiana` 为只读参考，本次审计**未修改**其中任何文件；本文件的所有 `path:line`
  引用均指该仓库的当前工作树（`master` 分支，`kiana/` 与 `kiana/` 为未跟踪目录）。
- 本节的补充以 `packages/agent/**` 与 `packages/codemode/**` 的源码为准；`kiana-ai` 的 `utils/**` 只在与本单元
  行为相关处引用（`transcript.ts`/`text.ts`/`event-stream.ts`/`streaming-json.ts`/`json-parse.ts`/`validation.ts`/
  `identity.ts`）。
