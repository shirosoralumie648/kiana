# `durable-harness` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-durable` 中 **durable-harness** 单元的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、错误、并发模型与最难复刻之处的说明。

- 单元 id：`durable-harness`
- 参考实现（只读，**不得修改**）：`/home/ctyun/code/kiana_rev/kiana/packages/durable/`
  - 代码：`/home/ctyun/code/kiana_rev/kiana/packages/durable/src/harness/**`（23 个文件，9,598 行，197 个 `export`）
  - 文档：`/home/ctyun/code/kiana_rev/kiana/packages/durable/docs/spec.md`（5,739 行，章节 §1–§13）
- 目标工作区：`/home/ctyun/code/kiana_rev/kiana`
- 上游依赖单元（Rust 侧需先就绪或并行开发）：
  - `chord-core` / `chord-rest`（`@earendil-works/chord`）：`JsonValue`、`Op`、`Tracker`/`Draft`、`Watch`/`NativeWatch`、`applyImmutable`、`OutputBuffer`/`Progress`、`Context`；
  - `durable-rest`（同包其余部分，**已有一份独立规范** `durable-rest.md`）：`SessionImpl`、`Transaction`、`Storage`、`defineDoc`/`defineDocFamily`/`defineEntry`/`defineTask`、`truncate`、`errors`、`timers`、`env`；
  - `kiana-ai`（`@earendil-works/kiana-ai`）：`Message`、`AssistantMessage`、`Models`、`streamSimple`/`completeSimple`、`StreamingJsonParser`、`validateToolArguments`、`estimateMessageTokens`、`calculateContextTokens`、`retryDelayMs`、`isRetryableAssistantError`、`uuidv7`、`getCurrentTools`/`toToolDeclaration`/`declarationsEqual`；
  - `kiana-telemetry`：`TelemetryContext`、`defineTelemetrySchema`、`createTypedSpanStarter`、`createTypedMetrics`、NOOP 上下文。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串均按 TypeScript 源码**逐字**抄录。Rust 实现必须输出完全一致的文本（除另有说明）。中文用于解释语义。凡标 **【逐字】** 的字符串/JSON/正则，测试会逐字节比对。
>
> 本单元**没有 CLI、没有 TUI、没有 slash 命令、没有按键绑定、没有运行时环境变量**（唯一与 `process.env` 相关的是测试夹具 `KIANA_DURABLE_DOCUMENT_RETENTION_MS`，见 §3.7，src 不读取它）。它是一个库（`@earendil-works/kiana-durable` 的根导出，外加 harness 内部模块）。所谓"用户可见面"是：公开 API 的错误类型/错误消息、`ReportInfo` 分类、`AgentEvent` 事件流、`kiana.*` 文档/条目/任务种类与持久化 JSON、遥测 schema、默认设置数值。见 §3 与 §4。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

行数以 `wc -l` 为准，全部位于 `packages/durable/src/harness/`：

```
types.ts              927 行   公共类型面（58 个导出）
harness.ts            564 行   Harness.open / HarnessImpl / ConversationImpl / safeReport / safeNow / boundConversation
scheduler.ts         2527 行   TaskScheduler（单元内最大的文件，必须逐段移植）
tool.ts               976 行   ToolTask v2 + NestedCallDoc + ToolExecutionApi 运行时
generation.ts         797 行   GenerationTask v1
events.ts             505 行   AgentEvent 翻译与 watchEvents
compaction.ts         483 行   CompactionTask v1 + 切点选择 + 序列化 + 摘要放置
agent.ts              355 行   kiana.agent 文档、resolveSettings、resolveAgent、cfg 变更
context.ts            333 行   上下文推导（head marker、edits、tool result 排序、baseline）
live.ts               284 行   kiana.live 文档与调度器结果清理
task-graph.ts         276 行   任务图视图
submissions.ts        277 行   Submissions + admitSubmission
view.ts               250 行   会话视图（entries + 内置文档挂载）
partial-changes.ts    184 行   流式 partial 的增量提交
inbox.ts              168 行   kiana.inbox + 边界放置
prompt.ts             158 行   系统提示重放/计划（kiana.system 条目）
telemetry.ts          139 行   DURABLE_TELEMETRY_SCHEMA v1
registry.ts           114 行   注册表与内置任务
usage.ts               72 行   kiana.usage 账本
util.ts                66 行   Waiters / scanAll / closedError / optionalExtension
json.ts                60 行   assignJson（Chord 增量写入计划）
provider.ts            39 行   kiana.provider 文档与 sessionId
define.ts              44 行   恒等构造函数（defineExtension/defineTool/section/hook/wrapTool/wrapSection）
```

### 0.2 同包内属于其他单元（本单元只消费，不移植）

```
packages/durable/src/index.ts                  导出面（本单元决定其中 harness 部分的名字）
packages/durable/src/types.ts                  内核类型（id 品牌、EntryRecord、TaskRecord、Storage、Tx、Session …）
packages/durable/src/documents.ts              defineDoc / defineDocFamily / 地址解析 / 版本校验
packages/durable/src/entries.ts                defineEntry + 6 个内置条目
packages/durable/src/tasks.ts                  defineTask（恒等）
packages/durable/src/errors.ts                 errorMessage / ReadAfterWrite / StorageRequestError / SessionFailed / ReadOnlySession / ConversationBusy
packages/durable/src/ids.ts                    idFromNumber / seqFromNumber
packages/durable/src/timers.ts                 nativeTimer / hostTimers / MAX_TIMER_DELAY
packages/durable/src/truncate.ts               DEFAULT_MAX_LINES / DEFAULT_MAX_BYTES / truncateHead / formatSize
packages/durable/src/session/**                SessionImpl、Transaction、forks（durable-rest 单元）
packages/durable/src/storage/**                4 个后端（durable-rest 单元）
packages/durable/src/env/**                    ExecutionEnv（durable-rest 单元）
packages/durable/src/tools/**                  编码工具（durable-rest 单元）
packages/durable/src/images/**、codemode/**、tool-search/**   （durable-rest 单元）
```

### 0.3 文档（必须逐节阅读）

`/home/ctyun/code/kiana_rev/kiana/packages/durable/docs/spec.md`：

| 章节 | 与本单元的关系 |
|---|---|
| §1 术语与不变量 | 全部 |
| §2 核心记录 / §2.1 条目与上下文 / §2.2 Harness 公开面 | 直接规范 |
| §3 文档（3.1–3.7） | 内置文档的定义语义 |
| §4 事务与存储所有权 / §4.1 延迟提交 | `PROGRESS_COMMIT`、`persistWithinMs` |
| §5 任务（5.1–5.6） | 任务定义、effect sandwich、调度器、结构化并发、睡眠/唤醒 |
| §6 提交与收件箱 | `Submit`/inbox/boundary 的权威规范 |
| §7 扩展、钩子、工具与系统提示（7.1–7.6） | registry/agent resolve/hooks/tools/prompt/extension reload/code mode |
| §8 内置任务（8.1–8.7） | 内置条目、live 文档、generation、tool、tool rounds、usage、compaction |
| §9 文档观察与 Chord（9.1–9.5） | watches、watchDoc、conversation view、agent events、task graph |
| §10 存储契约 / §10.1 失败 | 本单元调用 Storage 的方式 |
| §11 后端（11.1 Memory / 11.2 SQLite） | 存储语义前提：Memory 拷贝保留值与读结果（`context.ts` 深冻结断言的基础）、"一个 SQL 事务 = 一个提交"、schema v2 引入 `sleeping` |
| §12 API footguns、§13 非目标 | 语义陷阱清单 |

### 0.4 建议阅读顺序（对实现者）

1. `types.ts` → `harness.ts` → `agent.ts` → `registry.ts`（公开面与解析）；
2. `context.ts` → `prompt.ts` → `inbox.ts` → `submissions.ts`（上下文与提交流）；
3. `live.ts` → `generation.ts` → `tool.ts` → `compaction.ts`（三个内置任务）；
4. `scheduler.ts`（调度器；建议按 §2.15 的小节顺序读，2.15.4 的不变量先看）；
5. `events.ts` → `task-graph.ts` → `view.ts` → `partial-changes.ts` → `json.ts`；
6. `provider.ts` → `usage.ts` → `util.ts` → `telemetry.ts` → `define.ts`。

---

## 1. 概览

### 1.1 包的用途

`@earendil-works/kiana-durable`（v1.4.0，ESM，Node >= 22.19.0）是**持久化 agent harness 内核**：在一个 `Session`（单条串行 mutation 线 + 原子提交 + 不可变条目 + Chord 追踪的文档）之上，提供

- **会话（Conversation）**：用户输入提交、上下文推导、fork、重置、手动压缩；
- **可持久化任务调度器（TaskScheduler）**：状态机任务、所有权树、abort 级联、等待图、重启限制、定义迁移与交接；
- **三个内置任务**：`kiana.generation`（模型请求/重试/轮询/工具轮）、`kiana.tool`（工具执行与嵌套工具调用）、`kiana.compaction`（上下文压缩）；
- **五个内置会话文档**：`kiana.agent`、`kiana.live`、`kiana.inbox`、`kiana.provider`、`kiana.usage`，外加任务族文档 `kiana.tool.nested-call`；
- **扩展机制**：注册表（registry）、工具/提示 section/hook/wrap、按会话选择的 agent 配置；
- **观察面**：会话视图（`watch()`）、任务图（`watchTaskGraph()`）、实验性 agent 事件流（`watchEvents()`）、遥测。

本单元是 harness 层；它**不**实现 Session/Storage/文档内核（那是 `durable-rest`），也不实现模型协议（那是 `kiana-ai` 的 providers）。

### 1.2 与其他包/单元的关系

```
@earendil-works/kiana-ai        ← Message 类型、Models、流式/complete、重试分类、token 估算、StreamingJsonParser、uuidv7、identity
@earendil-works/chord        ← JsonValue/copyJson、Op/applyImmutable、Draft/Tracker、Watch/NativeWatch、Context、OutputBuffer/Progress、overlap/ownedSlice/appendText
@earendil-works/kiana-telemetry ← TelemetryContext/span/metrics/schema
@earendil-works/kiana-durable
 ├── src/index.ts             ← 导出面（包含 harness 的公共名字）
 ├── src/{types,documents,entries,tasks,errors,ids,timers,truncate}.ts   ← 内核（durable-rest）
 ├── src/session/**          ← Session/Transaction（durable-rest）
 ├── src/storage/**          ← Memory/SQLite/JSONL/IndexedDB（durable-rest）
 ├── src/env/**              ← ExecutionEnv（durable-rest）
 ├── src/tools/**、images/**、codemode/**、tool-search/**（durable-rest）
 └── src/harness/**          ← ★ 本单元
```

### 1.3 入口点

本单元通过包根导出以下 harness 名字（`src/index.ts`，共 216 行、217 个导出名；其中**源自 `./harness/*` 的 113 个**，含 **25 个值导出与 88 个类型导出**；另有 85 个来自 `./types.ts`、7 个 `./entries.ts`、5 个 `./errors.ts` 等兄弟单元）。以 `export { … } from "./harness/<file>"` 的 17 条语句计：`agent.ts` 6 值、`compaction.ts` 1 值 + 3 类型、`define.ts` 6 值、`events.ts` 1 值 + 5 类型、`generation.ts` 1 值 + 3 类型、`harness.ts` 2 值、`inbox.ts` 1 值 + 2 类型、`live.ts` 1 值 + 6 类型、`provider.ts` 1 值 + 1 类型、`registry.ts` 1 值、`scheduler.ts` 1 类型、`task-graph.ts` 4 类型、`telemetry.ts` 1 值、`tool.ts` 2 值 + 4 类型、`types.ts` 57 类型、`usage.ts` 1 值 + 1 类型、`view.ts` 1 类型。`Harness` 是对象字面量 `{ open }`（不是 class）。

```ts
// 值（runtime）
defineDoc, defineDocFamily                        (durable-rest)
AssistantEntry, CompactionEntry, defineEntry, ResetEntry, SystemEntry, ToolResultEntry, UserEntry  (durable-rest)
ConversationBusy, ReadAfterWrite, ReadOnlySession, SessionFailed, StorageRequestError             (durable-rest)
AgentDoc, configure, DEFAULT_COMPACTION_POLICY, DEFAULT_PROGRESS_POLICY, DEFAULT_RESTART_LIMIT, DEFAULT_RETRY_POLICY
CompactionTask
defineExtension, defineTool, hook, section, wrapSection, wrapTool
watchEvents
GenerationTask
DEFAULT_HARNESS_IDENTITY, Harness
InboxDoc
LiveDoc
ProviderDoc
createRegistry
DURABLE_TELEMETRY_SCHEMA
NestedCallDoc, ToolTask
UsageDoc
createSession, MemoryStorage, defineTask, ROOT_CONVERSATION_ID                                       (durable-rest)
// 类型
AgentEvent, AgentEventStream, MessageChange, SnapshotEvent, ToolEventCall
CompactionCheckpoint, CompactionInput, SummaryRequest
GenerationCheckpoint, GenerationInput, GenerationResult
CompactionFailure, CompactionStatus, LiveState, NestedToolSlot, NestedToolSummary, ToolSlot
InboxItem, InboxState, ProviderState, TaskGraph, TaskGraphNode, TaskGraphState, TaskGraphWatch
BlockedReason, NestedCallState, ToolTaskCheckpoint, ToolTaskInput, ToolTaskResult
ConversationView, UsageState, CompactionHooks …（以及 50 余个 types.ts 类型）
```

*注*：`createRegistry` 是唯一创建 Harness 可用注册表的公开途径；`Harness` 是一个对象字面量 `{ open }`（不是 class）。

### 1.4 运行面特征（决定 Rust 设计）

1. **单线串行**：所有任务状态转移都发生在 Session 的串行 commit 回调里（"on the line"）。回调中可以读 Storage、写 table、读写文档；回调外（"off the line"）运行 handler、join、等待。Rust 侧必须复刻这条串行线（`tokio::sync::Mutex` + 队列，或单消费者 actor）。
2. **同步 listener**：`session.observeCommits` / `subscribeClose` / `registry.subscribe` 都是同步回调，在提交被采纳后立刻运行，且**不能**在 listener 内同步发起新的 commit（调度器用 `queueMicrotask` 推迟）。
3. **两阶段延迟提交**：进度提交使用 `{ persistence: { kind: "deferred" } }`，先进入内存并被 watch 看到，之后由下一次读写同一文档的提交 / `persistWithinMs` 定时器 / 关闭写盘。
4. **所有 handler 可重入崩溃恢复**：每个 phase handler 必须先把"意图"（checkpoint）写盘，再做外部副作用，再写"结果"；崩溃后从 checkpoint 重放。Rust 移植必须保持这一契约（尤其是 `tool.ts` 的 `call`/`execute` 拆分与 `generation.ts` 的 `request`/`poll`）。
5. **时间来源**：所有时间来自 `HarnessOptions.now`（默认 `Date.now`），睡眠/退避时限存储在同一时钟上；`safeNow` 在宿主时钟抛错时回落到 `Date.now` 并只报告一次。
6. **随机来源**：`uuidv7()` 只在 `kiana.provider` 的 `initial()` 里调用。
7. **闭包/对象身份**：工具、section、hook、extension 都是 JS 对象，注册表按**对象身份**（`includes`/`===`）判断"哪个 extension 安装了某个 task"（`installerOf` 用 `installed.tasks?.includes(task)`）。Rust 中需要用稳定的 `ExtensionId`（数值或索引）来表达身份，并保留"同一个 Extension 值可被多次列举"的语义。
8. **`WeakMap`/`WeakSet` 语义**：`tool.ts` 的 `structuredValidators`（工具 schema → 编译器）是 `WeakMap<object, Compile>`，`context.ts` 的 `deeplyFrozen`（已深冻结的 JSON）是 `WeakSet<object>`，`view.ts` 的 `VIEWS`（session → `ConversationViews`）是 `WeakMap<object, ConversationViews>`；本单元**没有** `WeakRef`。Rust 侧分别用"按 schema 指针缓存 / 不缓存"、"不做冻结（所有权已保证不可变）"、`Harness` 上的字段或 `Weak` 指针替代。

---


## 2. 逐文件规范

> 记号：`f(x: T): R` 为 TS 签名；`?` 可选；`#name` 为 ES private 字段；`Context` 是 Chord 的取消/遥测上下文（携带 `abortSignal` 与 `telemetry`）；"线上"（on the line）指在 `Session.readOnLine` / `commitWith` 回调内；"线外"指普通异步执行。
> 所有字符串、字段名、数字常量按源码逐字。凡标 **【逐字】** 的必须字节级一致。

### 2.1 `src/harness/types.ts`（927 行，58 个类型导出，无 runtime 代码）

纯类型文件。Rust 侧 `kiana-durable-harness/src/types.rs`，全部 `serde` 结构，JSON 字段名与 TS 完全一致（camelCase，`serde(rename_all = "camelCase")` 仅用于 snake_case 字段；本文件里已全是 camelCase）。

#### 2.1.1 基础

```ts
export type ModelRef = { readonly provider: string; readonly modelId: string };
export type UserInput = UserMessage["content"];              // = string | ContentBlock[]（kiana-ai）
export type InputEntry = { readonly kind: string; readonly data?: JsonValue };
```

- `InputEntry.kind` 是**应用自定义**条目种类；`submissions.ts` 校验它非空且**不以 `"kiana."` 开头**。
- `data` 是给 transcript 展示的数据（作者等）；模型消息仍是输入的 user message。

#### 2.1.2 提交【字段逐字】

```ts
export type SubmissionDraft = { readonly requestId?: string } & (
  | { readonly type: "input";
      readonly content: UserInput;
      readonly whenBusy?: "steer" | "followUp" | "reject";
      readonly entry?: InputEntry }
  | { readonly type: "write";
      readonly entry: EntryDraft;
      readonly content?: never;
      readonly whenBusy?: never }
);
export type InputSubmissionDraft = Extract<SubmissionDraft, { readonly type: "input" }>;
export type SettledSubmissionRecord = SubmissionRecord & { readonly status: "done" | "unanswered" };

export interface Submission {
  readonly id: SubmissionId;
  status(context: Context): Promise<SubmissionRecord>;
  wait(context: Context): Promise<SettledSubmissionRecord>;
  abort(context: Context): Promise<"aborted" | "already_placed" | "settled">;
}
```

#### 2.1.3 任务结果

```ts
export type SettledTask<R> = TaskRecord<JsonValue, JsonValue, R> & {
  readonly state: Extract<TaskState<JsonValue, R>, { readonly status: "terminal" }>;
};
export type AnyTask = {
  readonly definition: {
    readonly name: string;
    readonly version: number;
    readonly initial: unknown;
    readonly phases: Readonly<Record<string, unknown>>;
    readonly abort: unknown;
    readonly migrate?: unknown;
    readonly hooks?: object;
  };
};
```

#### 2.1.4 会话句柄与控制

```ts
export type ConversationAbortOptions = { readonly background?: boolean };
export type HooksOf<K> = K extends Task<infer _I, infer _S, infer _R, infer H> ? H : never;

export interface ConversationHandle {
  readonly id: ConversationId;
  submit(submission: InputSubmissionDraft, context: Context): Promise<Submission>;
  abort(context: Context, options?: ConversationAbortOptions): Promise<void>;
  waitForIdle(context: Context): Promise<void>;
}
```

`ConversationHandle` 是**绑定到某次 task invocation** 的句柄：每个操作先 `binding.check()`，再在 `binding.signal` 下运行；invocation 结束后全部 reject，但已准入的工作继续持久化。实现见 `boundConversation()`（§2.2.9）。

#### 2.1.5 工具控制/诊断/结果

```ts
export type ToolControl = {
  readonly addTools?: readonly string[];
  readonly terminate?: boolean;
  readonly handoff?: string;
};
export type ToolDiagnostic = { readonly severity: "info" | "warn" | "error"; readonly message: string; readonly code?: string };
```

注意 `code` 是**可选**（`code?: string`），但本单元产生的诊断总会带 code；`severity` 的字面量是 **`"warn"`**（不是 `"warning"`）。本单元实际用到的 code 全表与逐字消息见 **§A.10**。

```ts
export type ToolExecutionResult<TDetails extends JsonValue = JsonValue> = {
  readonly output?: ToolResultMessage["content"];
  readonly structuredOutput?: JsonValue;
  readonly isError?: boolean;
  readonly details?: TDetails;
  readonly diagnostics?: readonly ToolDiagnostic[];
  readonly usage?: Usage;
  readonly control?: ToolControl;
};

export type NestedToolExecutionResult = {
  readonly taskId: TaskId;
  readonly structuredOutput?: JsonValue;
  readonly isError: boolean;                 // 必填
  readonly details?: JsonValue;
  readonly diagnostics: readonly ToolDiagnostic[];   // 必填
  readonly usage?: Usage;
  readonly durationMs?: number;
};
export type ToolExecutionMode = "parallel" | "sequential";
export type QueueMode = "all" | "one-at-a-time";
```

#### 2.1.6 `ToolExecutionApi`

```ts
export interface ToolExecutionApi<TDetails extends JsonValue = JsonValue> extends DocumentObserver, DocumentReader {
  readonly taskId: TaskId;
  readonly conversationId: ConversationId;
  readonly callId: string;
  readonly registry: RegistrySnapshot;
  agent(context: Context): Promise<Agent>;
  readonly models: Models;
  readonly env: ExecutionEnv | undefined;
  now(): number;
  output(chunk: string | Uint8Array, skipped?: ShellOutputSkip): void;
  readonly outputWindow: ShellOutputWindow | undefined;
  retainedOutput(): { readonly text: string; readonly truncated: boolean };
  diagnostic(diagnostic: ToolDiagnostic): void;
  details(value: TDetails, context: Context): Promise<void>;
  commit<T>(change: (tx: Tx) => T | Promise<T>, context: Context, options?: CommitOptions): Promise<T>;
  memo<T extends JsonValue>(name: string, context: Context): Promise<T | undefined>;
  memo<T extends JsonValue>(name: string, candidate: T, context: Context): Promise<T>;
  createTask<I, S extends { phase: string }, R, H extends object>(
    task: Task<I, S, R, H>, input: I, options: Omit<TaskOptions, "conversationId">, context: Context,
  ): Promise<TaskId<R>>;
  getTask<R>(id: TaskId<R>, context: Context): Promise<TaskRecord<JsonValue, JsonValue, R> | undefined>;
  waitForTask<R>(id: TaskId<R>, context: Context): Promise<SettledTask<R>>;
  conversation(id: ConversationId, context: Context): Promise<ConversationHandle | undefined>;
  executeTool(
    name: string, args: JsonObject, context: Context,
    options?: { readonly key?: string; readonly progress?: boolean },
  ): Promise<NestedToolExecutionResult>;
}
```

`registry` 是**该工具任务 phase 快照**（`RegistrySnapshot`），不是当前注册表。

#### 2.1.7 `ToolRegistration`【字段逐字】

```ts
export type ToolRegistration<TParameters extends TSchema = TSchema, TDetails extends JsonValue = JsonValue> =
  Tool<TParameters> & {
    readonly replay?: "safe" | "unsafe";                 // 默认 "unsafe"
    readonly executionMode?: ToolExecutionMode;          // 默认取 settings.toolExecution
    prepareArguments?(args: unknown): Static<TParameters>;
    readonly structuredOutputSchema?: TSchema;
    readonly callers?: readonly ("model" | "tools")[];   // 默认两者都可
    readonly deferred?: true;
    readonly outputLimits?: {
      readonly maxBytes?: number;
      readonly maxLines?: number;
      readonly retain?: "head" | "tail";
    };
    execute(args: Static<TParameters>, api: ToolExecutionApi<TDetails>, context: Context): Promise<ToolExecutionResult<TDetails>>;
  };
```

`prepareArguments` 契约：纯函数、不得修改 `args`；在"意图记录之前重试调用"时会**再次运行**；结果仍按 `parameters` 校验。

#### 2.1.8 提示/扩展/包装/注册表

```ts
export type PromptInput<Tool extends ToolRegistration = ToolRegistration> = {
  readonly conversationId: ConversationId;
  readonly agent: Agent<Tool>;
  readonly env: ExecutionEnv | undefined;
  readonly shown: Readonly<Record<string, string>>;
  readonly read: DocumentReader;
};
export type PromptSection<Tool extends ToolRegistration = ToolRegistration> = {
  readonly key: string;
  render(input: PromptInput<Tool>, context: Context): string | undefined | Promise<string | undefined>;
  readonly tag?: boolean;                            // 默认 true
};
export type HookRegistration = { readonly task: string; readonly handlers: object };
export type Wrap<Tool extends ToolRegistration = ToolRegistration> =
  | { readonly tool: string; wrap(tool: Tool): Tool }
  | { readonly section: string; wrap(section: PromptSection<Tool>): PromptSection<Tool> };
export interface Extension<Tool extends ToolRegistration = ToolRegistration> {
  readonly name: string;
  readonly tools?: readonly Tool[];
  readonly sections?: readonly PromptSection<Tool>[];
  readonly hooks?: readonly HookRegistration[];
  readonly wraps?: readonly Wrap<Tool>[];
  readonly tasks?: readonly AnyTask[];
}
export interface RegistrySnapshot<Tool extends ToolRegistration = ToolRegistration> {
  installed(): readonly Extension<Tool>[];
  extension(name: string): Extension<Tool> | undefined;
  tools(): readonly { readonly extension: Extension<Tool>; readonly tool: Tool }[];
  sections(): readonly { readonly extension: Extension<Tool>; readonly section: PromptSection<Tool> }[];
  tasks(): readonly AnyTask[];
  task(name: string): AnyTask | undefined;
}
export interface RegistryReader<Tool extends ToolRegistration = ToolRegistration> {
  snapshot(): RegistrySnapshot<Tool>;
  subscribe(listener: () => void): () => void;
}
export interface Registry<Tool extends ToolRegistration = ToolRegistration> extends RegistryReader<Tool> {
  install(extension: Extension<Tool>): void;
  uninstall(extension: Extension): void;
}
```

#### 2.1.9 `AgentState` / `AgentChange` / `Agent`【字段逐字】

```ts
export type AgentState = {
  model?: ModelRef;
  thinkingLevel?: ModelThinkingLevel;                 // "off" | "minimal" | "low" | "medium" | "high"
  extensions?: string[] | { add?: string[]; remove?: string[] };
  tools?: string[] | { remove: string[] };
  modelTools?: string[] | { remove?: string[]; add?: string[] };
  instructions?: string;
  cwd?: string;
};

export type AgentChange = {
  readonly model?: ModelRef | null;
  readonly thinkingLevel?: ModelThinkingLevel | null;
  readonly extensions?: readonly Extension[] | { readonly add?: readonly Extension[]; readonly remove?: readonly Extension[] } | null;
  readonly tools?: readonly ToolRegistration[] | { readonly remove: readonly ToolRegistration[] } | null;
  readonly modelTools?: readonly ToolRegistration[] | { readonly remove?: readonly ToolRegistration[]; readonly add?: readonly ToolRegistration[] } | null;
  readonly instructions?: string | null;
  readonly cwd?: string | null;
};

export type Agent<Tool extends ToolRegistration = ToolRegistration> = {
  readonly model?: ModelRef;
  readonly thinkingLevel: ModelThinkingLevel;
  readonly extensions: readonly Extension<Tool>[];
  readonly tools: readonly Tool[];        // offered：enabled + 模型可调用 + modelTools 选中
  readonly callable: readonly Tool[];     // nested 可解析：enabled + callers 含 "tools"
  readonly deferred: readonly Tool[];     // enabled、模型可调用、deferred、未被 offer
  readonly sections: readonly PromptSection<Tool>[];   // 扩展 sections，最后是 instructions
  readonly instructions?: string;
  readonly cwd?: string;
};
```

**重要**：`AgentChange` 用**对象**（`Extension[]`、`ToolRegistration[]`），写入 `AgentState` 时只取 `.name`（见 `agent.ts` 的 `names()`）。因此 configure 之后文档里只有名字。

#### 2.1.10 会话创建/流选项/策略/设置

```ts
export type ConversationInit = (tx: Tx, conversationId: ConversationId) => void | Promise<void>;
export type ConversationCreateOptions = {
  readonly ownership: ConversationOwnership;
  readonly agent?: AgentChange;
  readonly init?: ConversationInit;
};
export type ConversationStreamOptions = {
  transport?: Transport;
  timeoutMs?: number;
  maxRetries?: number;
  maxRetryDelayMs?: number;
  headers?: Record<string, string>;
  metadata?: JsonObject;
  cacheRetention?: CacheRetention;
  deferred?: boolean | { window?: "15m" | "1h" | "24h" };
};
export type ConversationRetryPolicy = { enabled: boolean; maxRetries: number; baseDelayMs: number; maxAgentDelayMs?: number };
export type CompactionPolicy = { enabled: boolean; reserveTokens: number; keepRecentTokens: number; backgroundTokens: number };
export type CompactionSettings = Partial<CompactionPolicy> & {
  readonly modelOverrides?: Readonly<Record<string, Partial<CompactionPolicy>>>;
};
export type ProgressPolicy = { partialIntervalMs: number; outputIntervalMs: number; persistWithinMs: number };
export type CompactionReason = "manual" | "threshold" | "overflow";
export type CompactionResult = { entryId?: EntryId; submissionId?: SubmissionId };
export type AfterFailedRun = "keep" | "continue" | "withdraw";
```

`CompactionSettings` 用 `modelOverrides`（**不是** `policies`），键为精确 `"provider/modelId"`。

`HarnessSettings` / `Settings` 字段见 §2.3.4；`Settings.afterFailedRun` 默认 `"keep"`。

#### 2.1.11 `HarnessOptions` / `EnvTarget` / `Wake` / `TaskInspection` / `HarnessInspection`

```ts
export type EnvTarget = {
  readonly conversationId: ConversationId;
  readonly cwd?: string;
  readonly read: DocumentReader;
};
export type HarnessOptions<Tool extends ToolRegistration = ToolRegistration> = {
  readonly telemetry?: TelemetryContext;
  readonly models: Models | (() => Models);
  readonly identity?: ClientIdentity;
  readonly registry: RegistryReader<Tool>;
  readonly settings?: HarnessSettings | (() => HarnessSettings | undefined);
  readonly env?: (target: EnvTarget, context: Context) => ExecutionEnv | undefined | Promise<ExecutionEnv | undefined>;
  readonly conversationCreated?: (tx: Tx, conversation: ConversationRecord) => void | Promise<void>;
  readonly now?: () => number;
  readonly setTimer?: SetTimer;
  readonly onWake?: (wake: Wake) => void;
  readonly onReport?: (error: unknown, info: ReportInfo) => void;
  readonly readOnly?: boolean;
};
export type Wake = { readonly active: boolean; readonly at?: number };
export type TaskInspection = {
  readonly record: TaskRecord<JsonValue, JsonValue, JsonValue>;
  readonly state:
    | { readonly kind: "running" }
    | { readonly kind: "ready"; readonly migrates: boolean }
    | { readonly kind: "waiting"; readonly on: readonly TaskId[] }
    | { readonly kind: "sleeping"; readonly until: number }
    | { readonly kind: "completing" }
    | { readonly kind: "blocked";
        readonly reason: "missing_task" | "task_too_old" | "migration_failed" | "restart_limit";
        readonly error?: unknown };
};
export type HarnessInspection = {
  readonly scheduling: "paused" | "running" | "closing";
  readonly tasks: readonly TaskInspection[];
  readonly submissions: readonly SubmissionRecord[];
};
```

#### 2.1.12 `ContextView` / `Conversation` / `Harness`

```ts
export type ContextView = {
  readonly head: EntryRecord | undefined;
  readonly entries: readonly EntryRecord[];
  readonly contributions: readonly (readonly Message[])[];
  readonly messages: readonly Message[];
};

export interface Conversation {
  readonly id: ConversationId;
  agent(context: Context): Promise<Agent>;
  configure(change: AgentChange, context: Context): Promise<void>;
  submit(submission: SubmissionDraft, context: Context): Promise<Submission>;
  reset(handoff: string | undefined, context: Context): Promise<void>;
  compact(instructions: string | undefined, context: Context): Promise<TaskId<CompactionResult>>;
  commit<T>(change: (tx: Tx) => T | Promise<T>, context: Context, options?: CommitOptions): Promise<T>;
  context(context: Context, options?: { readonly at?: EntryId }): Promise<ContextView>;
  entries(query: Omit<EntryQuery, "conversationId">, limit: number, cursor: Cursor | undefined, context: Context): Promise<Page<EntryRecord, Cursor>>;
  fork(at: EntryId, options: ConversationCreateOptions, context: Context): Promise<Conversation>;
  abort(context: Context, options?: ConversationAbortOptions): Promise<void>;
  waitForIdle(context: Context): Promise<void>;
  watch(context: Context): Promise<ConversationWatch>;
}
export type ConversationWatch = WatchHandle<ConversationView>;

export interface Harness extends Session {
  resume(): void;
  wake(): Wake;
  root(context: Context, options?: { readonly agent?: AgentChange; readonly init?: ConversationInit }): Promise<Conversation>;
  conversation(id: ConversationId, context: Context): Promise<Conversation | undefined>;
  createConversation(options: ConversationCreateOptions, context: Context): Promise<Conversation>;
  getTask<R>(id: TaskId<R>, context: Context): Promise<TaskRecord<JsonValue, JsonValue, R> | undefined>;
  inspect(context: Context): Promise<HarnessInspection>;
  submission(id: SubmissionId, context: Context): Promise<Submission | undefined>;
  abortSubmission(id: SubmissionId, context: Context, conversationId?: ConversationId):
    Promise<"aborted" | "already_placed" | "settled" | "not_found">;
  abortTask(id: TaskId, context: Context): Promise<"marked" | "terminal">;
  retryTask(id: TaskId, context: Context): Promise<void>;
  waitForTask<R>(id: TaskId<R>, context: Context): Promise<SettledTask<R>>;
  waitForIdle(context: Context): Promise<void>;
  usage(context: Context): Promise<UsageState>;
  watchTaskGraph(context: Context): Promise<TaskGraphWatch>;
}
```

#### 2.1.13 钩子

```ts
export interface HookApi extends DocumentReader {
  readonly taskId: TaskId;
  readonly conversationId: ConversationId;
  readonly models: Models;
  memo<T extends JsonValue>(name: string, context: Context): Promise<T | undefined>;
  memo<T extends JsonValue>(name: string, candidate: T, context: Context): Promise<T>;
}
export type HookResult<T> = T | undefined | Promise<T | undefined>;

export interface GenerationHooks {
  beforeRequest(request: { readonly messages: readonly Message[] }, api: HookApi, context: Context):
    HookResult<{ readonly messages: readonly Message[] }>;
  afterResponse(message: AssistantMessage, api: HookApi, context: Context): void | Promise<void>;
  onYield(answer: AssistantMessage, api: HookApi, context: Context):
    HookResult<{ readonly continue: UserInput }>;
  afterTools(assistant: EntryId, results: readonly EntryId[], api: HookApi, context: Context): void | Promise<void>;
}
export type ToolHookCall = ToolCall & { readonly parent?: { readonly taskId: TaskId; readonly callId: string } };
export interface ToolHooks {
  beforeTool(call: ToolHookCall, api: HookApi, context: Context):
    HookResult<{ readonly arguments?: JsonObject; readonly block?: string }>;
  afterTool(call: ToolHookCall, result: ToolExecutionResult, api: HookApi, context: Context):
    HookResult<ToolExecutionResult>;
}
export interface CompactionHooks {
  beforeCompact(
    compaction: {
      readonly reason: CompactionReason;
      readonly entries: readonly EntryRecord[];
      readonly messages: readonly Message[];
      readonly firstKept: EntryId;
      readonly instructions?: string;
    },
    api: HookApi, context: Context,
  ): HookResult<{ readonly decline: true } | { readonly summary: string }>;
}
```

**钩子语义要点**（跨文件约束）：
- `beforeRequest`：每次请求尝试前（含恢复）运行；返回值只用于本次请求；`messages` 被替换后 `view.messages` 不变。
- `afterResponse`：每个终态 provider 消息在**分类之前**运行。
- `onYield`：**只有第一个** `continue` 生效（后续忽略）；它追加一条 user 消息并继续 run。
- `afterTools`：整轮工具全部 terminal 后运行，`results` 是本轮工具结果条目（调用顺序）。
- `beforeTool`：在意图记录前；第一个 `block` 胜出，否则返回的 `arguments` 替换调用参数；**抛错等同于 block**（错误消息作为 block 文本）。
- `afterTool`：执行后、结果提交前；可替换结果。
- `beforeCompact`：切点选择之后、摘要之前；第一个决定胜出（`decline` 或 `summary`）。

### 2.2 `src/harness/harness.ts`（564 行）

导出：`SCHEDULER_INDEX_SIZES`、`DEFAULT_HARNESS_IDENTITY`、`safeNow`、`Harness`（对象 `{open}`）、类型 `Harness`；内部类 `ConversationImpl`、`HarnessImpl`；内部函数 `currentModels`、`currentSettings`、`safeReport`、`boundConversation`。

#### 2.2.1 常量

```ts
const SCAN_PAGE_SIZE = 256;
export const SCHEDULER_INDEX_SIZES = Symbol.for("kiana-durable.schedulerIndexSizes");
export const DEFAULT_HARNESS_IDENTITY: ClientIdentity = { attribution: KIANA_ATTRIBUTION };
```

`SCHEDULER_INDEX_SIZES` 是测试用的符号键，HarnessImpl 上以 getter 暴露 `scheduler.indexSizes`（Rust 侧提供同形访问器/测试钩子，不参与持久化）。

#### 2.2.2 `currentModels(models, identity)`

```ts
const merged = mergeClientIdentity(DEFAULT_HARNESS_IDENTITY, identity) ?? DEFAULT_HARNESS_IDENTITY;
if (typeof models === "function") return () => withClientIdentity(models(), merged);
const wrapped = withClientIdentity(models, merged);
return () => wrapped;
```

- `HarnessOptions.models` 可以是 `Models` 或 `() => Models`；**每次使用**才求值。
- `mergeClientIdentity` 逐字段合并（见 kiana-ai `utils/identity.ts`）；`identity.attribution = null` 显式清除归因头。

#### 2.2.3 `currentSettings(settings)`

`typeof settings === "function" ? settings : () => settings`；即设置**每次使用时**读取。

#### 2.2.4 `ConversationImpl`

字段：`id: ConversationId`、`#host: ConversationHost`（`harness`、`storage`、`tasks`、`submissions`、`views`、`now`、`create`）。

| 方法 | 精确行为 |
|---|---|
| `agent(context)` | `harness.resolveAgent(id, undefined, context)`（当前注册表快照 + 当前设置，每次重建） |
| `configure(change, context)` | `harness.commitWith(tx => configure(tx, id, change), context)` |
| `submit(submission, context)` | `submissions.submit(id, submission, context)` |
| `compact(instructions, context)` | **先 `tasks.resume()`**；`input = { reason: "manual", ...(instructions === undefined ? {} : { instructions }) }`；`harness.commitWith(tx => createCompaction(tx, id, input), context)`，返回 `TaskId<CompactionResult>` |
| `reset(handoff, context)` | `model = handoff === undefined ? {} : { model: [{ role: "user", content: handoff, timestamp: host.now() }] }`；`entry = { kind: ResetEntry.kind, head: "self", ...model }`；`await submissions.submit(id, { type: "write", entry }, context)`；返回 void |
| `commit(change, context, options?)` | `harness.commitWith(change, context, { conversationId: id }, options)` |
| `context(context, { at }?)` | `readContext(harness, storage, id, context, at)` |
| `entries(query, limit, cursor, context)` | 构造 `EntryQuery = { conversationId: id, ...(minEntryId?), ...(maxEntryId?), ...(order?) }`，`harness.readOnLine(() => storage.scanEntries(bounded, limit, cursor, context))` |
| `fork(at, options, context)` | `host.create({ kind: "fork", parentId: id, at, ownership: options.ownership }, options, context)` |
| `abort(context, options?)` | **先 `tasks.resume()`**；`tasks.abortConversation(id, options?.background === true, context)` |
| `waitForIdle(context)` | **先 `tasks.resume()`**；`tasks.waitForIdle(id, context)` |
| `watch(context)` | `views.watch(id, context)` |

#### 2.2.5 `HarnessImpl` 字段与构造

```ts
readonly #options: HarnessOptions;
readonly #hostSettings: () => HarnessSettings | undefined;
readonly #report: (error, info, context?) => void;
readonly #host: ConversationHost;
readonly #tasks: TaskScheduler;
readonly #submissions: Submissions;
readonly #taskGraph: TaskGraphView;
```

构造顺序（**有依赖顺序，不可调换**）：
1. `report = safeReport(options.onReport, options.telemetry ?? NOOP_TELEMETRY_CONTEXT)`；
2. `now = safeNow(options.now, report)`；
3. `hostSettings = currentSettings(options.settings)`；
4. `super(rawStorage, now, () => hostSettings()?.documentRetentionMs, options.setTimer, options.readOnly === true, () => hostSettings()?.progress?.persistWithinMs)`——SessionImpl 构造参数依次为 storage、now、documentRetention、setTimer、readOnly、persistWithin；
5. `storage = this.storage`（**Session 的失败保护包装后的 storage**；`rawStorage` 只供 `super()`）；
6. 构造 `TaskScheduler`，传入：`telemetry`、`session: this`、`storage`、`registry`、`models: currentModels(...)`、`agent: (id, snapshot, ctx) => this.resolveAgent(...)`、`settings: () => resolveSettings(hostSettings())`、`env: (id, ctx) => this.buildEnv(id, ctx)`、`now`、`onWake`、`timers: hostTimers(options.setTimer, error => report(error, {type:"host_error", callback:"setTimer"}))`、`writeAtClose: writes => this.writeAtClose(writes)`、`report`、`settleOutcome: settleSchedulerOutcome`、`withdrawInputs: withdrawQueuedInputs`、`conversation: async (id, binding, ctx) => { const record = await this.readOnLine(() => storage.conversation(id, ctx)); return record === undefined ? undefined : boundConversation(id, binding, this.#submissions, this.#tasks); }`、`context: withoutAbortSignal(context)`、`readOnly: options.readOnly === true`；
7. `#submissions = new Submissions(this, storage, now, settings, () => this.#tasks.resume(), options.telemetry)`；
8. `#taskGraph = new TaskGraphView(this, storage, { blocked: r => this.#tasks.blockedReason(r), subscribeBlocked: l => this.#tasks.subscribeBlocked(l) })`；
9. `#host = { harness: this, storage, tasks, submissions, views: new ConversationViews(this, storage), now, create: (t, o, c) => this.#create(t, o, c) }`。

#### 2.2.6 `HarnessImpl` 方法

| 方法 | 精确行为 |
|---|---|
| `resolveAgent(id, snapshot?, context)` | `registry = snapshot ?? options.registry.snapshot()`；`state = await this.snapshot(AgentDoc, id, context)`；`resolveAgent(state, registry, resolveSettings(hostSettings()), (error, extension) => report(error, { type: "extension_error", conversationId: id, extension }, context))` |
| `buildEnv(id, context)` | `build = options.env`；`undefined` 直接返回 `undefined`；`cwd = (await this.snapshot(AgentDoc, id, context))?.cwd`；`build({ conversationId: id, ...(cwd === undefined ? {} : { cwd }), read: this }, context)` |
| `openTasks(context)` | `tasks.open(context)`（open 的一部分：恢复 `running` → `pending`） |
| `resume()` | `assertUsable()` 后 `tasks.resume()` |
| `wake()` | `assertUsable()` 后 `tasks.wake()` |
| `getTask(id, context)` | `readOnLine(() => storage.task(id, context))` |
| `[SCHEDULER_INDEX_SIZES]` | `tasks.indexSizes`（测试用） |
| `inspect(context)` | `readOnLine`：`{ scheduling, tasks } = tasks.inspect(options.registry.snapshot())`；`submissions = [...scanAll(queued), ...scanAll(placed)].sort((a,b) => a.id - b.id)`；返回 `{ scheduling, tasks, submissions }` |
| `submission(id, context)` | `submissions.get(id, context)` |
| `abortSubmission(id, context, conversationId?)` | `submissions.abort(id, context, conversationId)` |
| `abortTask(id, context)` | `tasks.abort(id, context)` |
| `retryTask(id, context)` | `assertUsable()` 后 `tasks.retry(id, context)` |
| `waitForTask(id, context)` | **先 `tasks.resume()`**；`tasks.waitForTask(id, context)` |
| `waitForIdle(context)` | **先 `tasks.resume()`**；`tasks.waitForIdle(undefined, context)` |
| `usage(context)` | `readOnLine(scanAll(scanConversations({})))`；`total = UsageDoc.definition.initial()`；对每个会话 `snapshot(UsageDoc, id, context)` 若存在则 `addUsageState(total, state)`；返回 `total` |
| `watchTaskGraph(context)` | `taskGraph.watch(context)` |
| `root(context, options?)` | `#create({ kind: "root" }, options ?? {}, context)` |
| `conversation(id, context)` | `assertUsable()`；`readOnLine(storage.conversation(id))`；不存在返回 `undefined`，否则 `new ConversationImpl(record.id, host)` |
| `createConversation(options, context)` | `#create({ kind: "independent", ownership: options.ownership }, options, context)` |
| `report(error, info = { type: "listener_error" }, context?)` | `#report(error, info, context)`（override Session 的 report） |
| `beforeClose()`（protected override） | `tasks.join()`（在 close listeners 之后、Storage 关闭之前；**不写 task outcome**） |

#### 2.2.7 `#create(target, options, context)`

```ts
this.assertUsable();
const id = await this.commitWith(async (tx) => {
  if (target.kind === "root" && (await tx.conversation(ROOT_CONVERSATION_ID)) !== undefined) return ROOT_CONVERSATION_ID;
  const record =
    target.kind === "root" ? await tx.createRootConversation()
    : target.kind === "fork" ? await tx.forkConversation(target.parentId, target.at, { ownership: target.ownership })
    : await tx.createConversation({ ownership: target.ownership });
  if (options.agent !== undefined) await configure(tx, record.id, options.agent);
  if (options.init !== undefined) await options.init(tx, record.id);
  return record.id;
}, context);
return new ConversationImpl(id, this.#host);
```

**顺序约束**：创建会话（触发 `conversationCreated`）→ `agent` 变更 → `init`。根会话幂等：已存在直接复用，不再 configure/init。

#### 2.2.8 `conversationCreated(tx, record)`（protected override）

```ts
await tx.doc(LiveDoc, record.id);
await tx.doc(InboxDoc, record.id);
await tx.doc(UsageDoc, record.id);
await tx.doc(ProviderDoc, record.id);       // 每次运行 initial() → 新 uuidv7 sessionId
await createAgent(tx, record);
await this.#options.conversationCreated?.(tx, record);
```

即**任何**创建/派生会话的提交（含宿主直接 `tx.createConversation()`）都先建四个内置文档，再写 `kiana.agent`，最后调用宿主钩子。钩子抛错会失败整个创建提交。

#### 2.2.9 `boundConversation(id, binding, submissions, tasks)`

```ts
const bind = (context) => withAbortSignal(binding.signal, context);
const bound = (op) => async (context) => { binding.check(); return op(bind(context)); };
return {
  id,
  submit: async (draft, context) => {
    binding.check();
    const submission = await submissions.submit(id, draft, bind(context));
    return {
      id: submission.id,
      status: bound(c => submission.status(c)),
      wait: bound(c => submission.wait(c)),
      abort: bound(c => submission.abort(c)),
    };
  },
  abort: async (context, options) => { binding.check(); return tasks.abortConversation(id, options?.background === true, bind(context)); },
  waitForIdle: bound(c => tasks.waitForIdle(id, c)),
};
```

每次操作都 `binding.check()`（invocation 已结束则抛 `Task ${taskId} invocation has ended`，见 §2.11.23 的 `endedError`），并把 binding 的 signal 叠加到调用上下文。

#### 2.2.10 `safeReport(onReport, telemetry)`

```ts
const reports = createTypedMetrics(telemetry.metrics ?? NOOP_TELEMETRY_METRICS, [DURABLE_TELEMETRY_SCHEMA]).counter("kiana.durable.reports");
return (error, info, context) => {
  contained(() => onReport?.(error, info), () => {});     // onReport 抛错被吞
  const attributes = { type: info.type };
  switch (info.type) {
    case "task_faulted": case "task_outdated":
      attributes.reason = info.reason; attributes.extension = info.extension; attributes["task.id"] = info.task.id; break;
    case "task_parked": case "migration_failed": case "task_error": case "extension_error":
      attributes.extension = info.extension; attributes["task.id"] = info.task?.id; break;
    case "host_error":
      attributes["task.id"] = info.task?.id; break;
  }
  reports.add(1, { type: info.type });
  context?.telemetry.addEvent("kiana.durable.report", attributes);
};
```

`ReportInfo` 的 9 种 `type`：`session_failed`、`task_faulted`、`task_parked`、`migration_failed`、`task_outdated`、`task_error`、`extension_error`、`host_error`、`listener_error`。

#### 2.2.11 `safeNow(now, report)`

```ts
if (now === undefined) return Date.now;
let reported = false;
return () => {
  try { return now(); }
  catch (error) {
    if (!reported) { reported = true; report(error, { type: "host_error", callback: "now" }); }
    return Date.now();
  }
};
```

先置 `reported = true` 再报告（report handler 里再读时钟不会二次报告）。

#### 2.2.12 `Harness.open(storage, options, context)`

```ts
context.abortSignal?.throwIfAborted();
const snapshot = options.registry.snapshot();
const missing = BUILTIN_TASKS.filter(task => snapshot.task(task.definition.name) === undefined);
if (missing.length > 0) {
  const names = missing.map(task => task.definition.name).join(", ");
  throw new Error(`Registry lacks built-in tasks ${names}; create it with createRegistry()`);
}
const harness = new HarnessImpl(storage, options, context);
try { await harness.openTasks(context); }
catch (error) { await harness.close(withoutAbortSignal(context)).catch(() => {}); throw error; }
return harness;
```

【逐字】`Registry lacks built-in tasks ${names}; create it with createRegistry()`（`names` 按 `BUILTIN_TASKS` 顺序：`kiana.generation`、`kiana.tool`、`kiana.compaction`，逗号+空格分隔）。

### 2.3 `src/harness/agent.ts`（355 行，14 个导出）

导出：`DEFAULT_RETRY_POLICY`、`DEFAULT_COMPACTION_POLICY`、`DEFAULT_PROGRESS_POLICY`、`INSTRUCTIONS_KEY`、`AgentDoc`、`DEFAULT_RESTART_LIMIT`、`resolveSettings`、`configure`、`addTools`、`createAgent`、`agentHooks`、`sectionExtension`、`toolExtension`、`resolveAgent`（内部：`restartLimit`、`applyChange`、`isList`、`names`、`offeredTools`、`filterTools`、`selectExtensions`、`applyWrap`）。

#### 2.3.1 常量【逐字】

```ts
export const DEFAULT_RETRY_POLICY: ConversationRetryPolicy = { enabled: true, maxRetries: 3, baseDelayMs: 2000, maxAgentDelayMs: 60000 };
export const DEFAULT_COMPACTION_POLICY: CompactionPolicy = { enabled: true, reserveTokens: 16384, keepRecentTokens: 20000, backgroundTokens: 32768 };
export const DEFAULT_PROGRESS_POLICY: ProgressPolicy = { partialIntervalMs: 100, outputIntervalMs: 100, persistWithinMs: 1_000 };
export const INSTRUCTIONS_KEY = "instructions";
const DEFAULT_CONTEXT_RETENTION_MS = 600_000;    // 内部
export const DEFAULT_RESTART_LIMIT = 5;
```

#### 2.3.2 `AgentDoc`

```ts
export const AgentDoc = defineDoc<AgentState>({
  kind: "kiana.agent", version: 1, scope: "conversation",
  history: "rewindable", fork: "asOf",
  initial: () => ({}),
  checkpointWhen: () => true,
});
```

- `history: "rewindable"` + `fork: "asOf"`：fork 时取**该 fork 条目时刻**的 agent 状态。
- `initial()` 返回 `{}`（所有字段未设）。

#### 2.3.3 `restartLimit(limit)`【内部】

```ts
typeof limit === "number" && limit >= 1 ? Math.floor(limit) : DEFAULT_RESTART_LIMIT;
```

注意：**不检查有限性** — `Infinity >= 1` 为真，`Math.floor(Infinity) === Infinity`，故 `Infinity` 表示"永不 park"（与文档一致）。`NaN >= 1` 为假 → 默认。`0.5` → `Math.floor(0.5) = 0`（保留为 0，即**立刻** park；这是源码行为，需照抄）。

#### 2.3.4 `resolveSettings(settings?)` → `Settings`

```ts
{
  ...(extensions === undefined ? {} : { extensions }),
  stream: { ...settings?.stream },
  retry: { ...DEFAULT_RETRY_POLICY, ...settings?.retry },
  compaction: { ...DEFAULT_COMPACTION_POLICY, ...settings?.compaction },
  progress: {
    partialIntervalMs: settings?.progress?.partialIntervalMs ?? 100,
    outputIntervalMs: settings?.progress?.outputIntervalMs ?? 100,
    persistWithinMs: persistWithin(settings?.progress?.persistWithinMs),
  },
  toolExecution: settings?.toolExecution ?? "parallel",
  steeringMode: settings?.steeringMode ?? "one-at-a-time",
  followUpMode: settings?.followUpMode ?? "one-at-a-time",
  contextRetentionMs: settings?.contextRetentionMs ?? 600_000,
  documentRetentionMs: documentRetention(settings?.documentRetentionMs),
  afterFailedRun: settings?.afterFailedRun ?? "keep",
  restartLimit: restartLimit(settings?.restartLimit),
}
```

- `stream` 是**浅拷贝**（`{...undefined}` → `{}`）。
- `compaction` 是浅合并；`CompactionSettings.modelOverrides` 若存在则作为 `compaction.modelOverrides` 保留（源码 `{...DEFAULT_COMPACTION_POLICY, ...settings?.compaction}`）。
- `persistWithin(ms)`（`session/session.ts`）：`typeof ms === "number" && ms >= 0 ? ms : DEFAULT_PERSIST_WITHIN_MS`（1000）。**不做 `Math.floor`**；`Infinity` 合法并保留（表示只在下一次读写同一文档或关闭时存储）。`NaN >= 0` 为假 → 默认。
- `documentRetention(ms)`（`session/session.ts`）：`typeof ms === "number" && ms >= 0 ? ms : defaultRetention`；`defaultRetention` 初值 600_000，可被 `setDefaultDocumentRetention(ms)` 全局改写（测试 `setup-document-retention.ts` 用环境变量 `KIANA_DURABLE_DOCUMENT_RETENTION_MS` 覆盖，见 §4.5）。`Infinity` 合法（永不回收）。

#### 2.3.5 `configure(tx, conversationId, change)`

```ts
const state = await tx.doc(AgentDoc, conversationId);   // 草稿（可写）
applyChange(state, change);
```

`applyChange` 逐字段：
- `model`：`change.model === undefined || change.model === null ? change.model : { ...change.model }`（**null 删除，undefined 不改，对象做浅拷贝**）。
- `thinkingLevel`：直接 `set`（字符串或 null/undefined）。
- `extensions`：
  - `undefined`/`null` → 原样 `set`（即删除）；
  - 数组 → `names(extensions)`（取 `.name`）；
  - 对象 → `{...(add === undefined ? {} : { add: names(add) }), ...(remove === undefined ? {} : { remove: names(remove) })}`（**只保留 add/remove 中给出的**）。
- `tools`：数组 → `names`；对象 → `{ remove: names(remove) }`（**只保留 remove**，`tools` 没有 `add`）。
- `modelTools`：数组 → `names`；对象 → `{...(remove?), ...(add?)}`。
- `instructions`、`cwd`：直接 set。
- `set(key, value)`：`undefined` 直接 return（不写）；`null` → `delete state[key]`；否则赋新值。
- 注意：`AgentChange` 的对象字段是 `Extension`/`ToolRegistration` 数组，`names()` 只取 `.name`，因此**configure 会丢弃对象身份**（这正是文档中存名字的设计）。

#### 2.3.6 `addTools(tx, conversationId, added)`

工具轮 `control.addTools` 的落地逻辑：
- `tools` 是数组 → 对每个 `added` 名字，若不在则 `push`（保持顺序）。
- `tools` 是 `{ remove }` 且其中含任一 `added` 名字 → `state.tools = { remove: tools.remove.filter(n => !added.includes(n)) }`（**只有命中了才重写**）。
- `modelTools` 是数组 → 对每个 `added` 若不在则 push，**然后 return**（不再处理对象分支）。
- 否则（`undefined` 或对象）：`remove = (modelTools?.remove ?? []).filter(n => !added.includes(n))`；`add = [...(modelTools?.add ?? [])]`；对每个 `added` 若不在 `add` 则 push；`state.modelTools = { ...(remove.length === 0 ? {} : { remove }), add }`（**总会写 `add`，即使为空数组**）。
- 语义：`tools` 未设表示"全部启用"，故不写；`modelTools` 未设或对象都要把名字加入 `add`（加载 deferred 工具；对非 deferred 名字无副作用）。

#### 2.3.7 `createAgent(tx, conversation)`

```ts
if (conversation.parent !== undefined) return;                 // fork：已有 asOf 副本
const agent = await tx.doc(AgentDoc, conversation.id);
if (conversation.owner === undefined) return;                  // ownerless：留空
const owner = await tx.doc(AgentDoc, conversation.owner.conversationId);
Object.assign(agent, copyJson(owner));                          // task-owned：复制 owner 会话的 agent
```

- fork 直接返回（`forkConversation` 已按 `asOf` 复制）；
- 新建 ownerless → 空 `{}`；
- 新建成 task-owned → 复制 owner 会话的 agent 文档。

#### 2.3.8 `agentHooks(agent, taskName)`

按 `agent.extensions` 顺序，收集 `extension.hooks ?? []` 中 `hook.task === taskName` 的项，返回 `{ extension: extension.name, handlers }[]`。

#### 2.3.9 `sectionExtension(agent, key)` / `toolExtension(agent, name)`

遍历 `agent.extensions`，若该扩展 `sections` 里有同 key（或 `wraps` 里 `"section" in wrap && wrap.section === key`），则把 `found` 更新为当前扩展名（**最后一个胜出**）；返回 `found`。

#### 2.3.10 `resolveAgent(state, snapshot, settings, report)` → `Agent`

严格顺序：
1. `extensions = selectExtensions(state?.extensions, snapshot, settings)`；
2. 组合工具：遍历 extensions、每个 extension 的 `tools` 顺序 `composed.set(tool.name, tool)`（**后面的扩展覆盖同名工具；Map 插入位置保留首次出现**）；
3. 组合 sections：同样 `sections.set(section.key, section)`；
4. 应用 wraps（按扩展顺序、每个扩展的 `wraps` 顺序）：
   - `"tool" in wrap` → `applyWrap(composed, wrap.tool, t => wrap.wrap(t), t => t.name, report)`；
   - 否则 → `applyWrap(sections, wrap.section, s => wrap.wrap(s), s => s.key, report)`。
5. `enabled = filterTools([...composed.values()], state?.tools)`；
6. `callableBy(caller) = tool => tool.callers === undefined || tool.callers.includes(caller)`；
7. `modelCallable = enabled.filter(callableBy("model"))`；
8. `tools = offeredTools(modelCallable, state?.modelTools)`；
9. `offered = new Set(tools)`；
10. `deferred = modelCallable.filter(t => t.deferred === true && !offered.has(t))`（**按 registry 顺序**）；
11. `callable = enabled.filter(callableBy("tools"))`；
12. `instructions = state?.instructions`；`agentSections = [...sections.values()]`；若 `instructions !== undefined` 则 push `{ key: "instructions", render: () => instructions }`；
13. 返回 `{ ...(state?.model === undefined ? {} : { model }), thinkingLevel: state?.thinkingLevel ?? "off", extensions, tools, callable, deferred, sections: agentSections, ...(instructions === undefined ? {} : { instructions }), ...(cwd === undefined ? {} : { cwd }) }`。

`applyWrap(items, target, wrap, nameOf, report)`：

```ts
const item = items.get(target);
if (item === undefined) return;                      // 没有目标：什么都不做（不报告）
try {
  const wrapped = wrap(item);
  if (nameOf(wrapped) !== target) throw new Error(`Wrapper renamed ${target} to ${nameOf(wrapped)}`);
  items.set(target, wrapped);
} catch (error) {
  items.delete(target);                              // 抛错或改名：删除目标并报告
  report(error);
}
```

【逐字】`Wrapper renamed ${target} to ${nameOf(wrapped)}`。

#### 2.3.11 `offeredTools(tools, filter)`

```ts
if (Array.isArray(filter)) return filterTools(tools, filter);
const removed = new Set(filter?.remove ?? []);
const kept = tools.filter(tool => tool.deferred !== true && !removed.has(tool.name));
const byName = new Map(tools.map(tool => [tool.name, tool]));
const loaded = [...new Set(filter?.add ?? [])].flatMap(name => {
  const tool = byName.get(name);
  return tool?.deferred === true && !removed.has(name) ? [tool] : [];
});
return [...kept, ...loaded];
```

- `undefined` 过滤器：`kept` = 非 deferred 的全部（`removed` 为空）。
- 对象过滤器：非 deferred 且未被 remove（registry 顺序）+ `add` 中**是 deferred** 且未被 remove 的（按 `add` 顺序、去重）。
- **非 deferred 名字出现在 `add` 里无效**（它们已在 `kept` 或已被 remove）。

#### 2.3.12 `filterTools(tools, filter)`

```ts
if (filter === undefined) return tools;
if (Array.isArray(filter)) {
  const byName = new Map(tools.map(t => [t.name, t]));
  return [...new Set(filter)].flatMap(name => { const t = byName.get(name); return t === undefined ? [] : [t]; });
}
const removed = new Set(filter.remove);
return tools.filter(t => !removed.has(t.name));
```

- 数组：**按 filter 顺序**返回匹配工具（去重）；未注册名字忽略。
- 对象：保留除 `remove` 外的全部（registry 顺序）。

#### 2.3.13 `selectExtensions(stored, snapshot, settings)`

```ts
let selected: string[];
if (Array.isArray(stored)) selected = stored;
else {
  const base = settings.extensions?.map(e => e.name) ?? snapshot.installed().map(e => e.name);
  const edit = stored as { add?: string[]; remove?: string[] } | undefined;
  const removed = new Set(edit?.remove ?? []);
  selected = [...base, ...(edit?.add ?? [])].filter(name => !removed.has(name));
}
const extensions = [];
for (const name of new Set(selected)) {                 // 去重，保留首次位置
  const extension = snapshot.extension(name);
  if (extension !== undefined) extensions.push(extension);
}
return extensions;
```

- 数组：精确选择（顺序即数组顺序）；
- 对象/`undefined`：基础 = `settings.extensions`（Harness 级默认）或全部已安装；加 `add`、减 `remove`；
- 未安装名字被跳过。

### 2.4 `src/harness/registry.ts`（114 行）

导出：`BUILTIN_TASKS`、`createRegistry`（`RegistryImpl`/`RegistryState`/`validateExtension` 内部）。

#### 2.4.1 常量

```ts
const SECTION_KEY = /^[a-z][a-z0-9_-]*$/;
export const BUILTIN_TASKS: readonly AnyTask[] = [GenerationTask, ToolTask, CompactionTask];
```

顺序即 `kiana.generation`、`kiana.tool`、`kiana.compaction`（决定 `Harness.open` 报错里的名字顺序）。

#### 2.4.2 `RegistryState(extensions)`（不可变快照）

- `#extensions`：安装顺序数组；
- `#byName`：name → extension（**后安装同名覆盖**，但 `install` 本身按 name 替换原位，故 `#extensions` 位置不变）；
- `#tasks`：先放三个内置任务，再按扩展顺序放 `extension.tasks ?? []`；**重名抛错**：`Task ${name} of extension ${extension.name} is already installed`（因此扩展不能覆盖内置任务，也不能与先前扩展重名）。
- `tools()`：`#extensions.flatMap(e => (e.tools ?? []).map(tool => ({ extension: e, tool })))`——**按安装顺序展开，名字可重复**。
- `sections()`：同理。
- `tasks()`：`#tasks.values()`（内置在前，然后扩展顺序）。
- `task(name)`、`extension(name)`、`installed()`。

#### 2.4.3 `RegistryImpl`

- `#current = new RegistryState([])`；`#listeners = new Set()`。
- `snapshot()`：返回 `#current`（不可变对象）。
- `subscribe(listener)`：加入 Set，返回 `() => this.#listeners.delete(listener)`。
- `install(extension)`：
  1. `validateExtension(extension)`；
  2. `index = current.findIndex(installed => installed.name === extension.name)`；
  3. `next = index < 0 ? [...current, extension] : current.map((installed, at) => at === index ? extension : installed)`（**同名替换原位，不移动位置**）；
  4. `#publish(next)`。
- `uninstall(extension)`：若当前没有同名扩展则**直接返回**；否则 `#publish(current.filter(i => i.name !== extension.name))`（按 **name** 删除，不是对象身份）。
- `#publish(extensions)`：`this.#current = new RegistryState(extensions)`（**可能抛错且不发布**）；然后 `for (const listener of [...this.#listeners]) listener()`（**同步**，listener 抛错会向上传播给 install 调用者！源码无 try/catch）。
- `validateExtension(extension)`：
  - 工具重名：`Extension ${extension.name} has two tools named ${tool.name}`（`Error`）；
  - section key 不匹配：`Section key ${JSON.stringify(key)} must match ${SECTION_KEY}`（**`TypeError`**，且 `${SECTION_KEY}` 是正则的 `toString()` → 字面 `/^[a-z][a-z0-9_-]*$/`）；
  - 保留 key：`Section key ${key} is reserved for the agent's instructions`；
  - section 重名：`Extension ${extension.name} has two sections with key ${key}`。
- `createRegistry()`：`new RegistryImpl()`。

> **Rust 提示**：`RegistrySnapshot` 必须是"发布即冻结"的共享结构（`Arc<RegistrySnapshot>`）；`install` 走写锁构造新快照再替换，listener 同步通知。注意 §1.4 的第 7 条：`installerOf` 用对象身份判断 task 属于哪个扩展，而 `uninstall` 用名字 —— Rust 中 `Extension` 需要 `Arc` 身份 + `name` 字段两套语义。

### 2.5 `src/harness/context.ts`（333 行，8 个导出）

导出：`ContextBounds`、`ContextRange`、`captureContextBounds`、`readContextFrom`、`readContext`、`deriveContext`、`activeEntries`、`orderToolResults`（内部：`emptyView`、`deriveRange`、`extendRange`、`findBaseline`、`carriesSystemMessage`、`leadWithSystem`、`contribute`、`freezeJson`、`settle`、`rangeQuery`、`scanRange`、`selectActive`、`missingResult`）。

#### 2.5.1 常量【逐字】

```ts
const SCAN_PAGE_SIZE = 256;
const EXCLUDED_STOP_REASONS = new Set(["aborted", "error", "deferred"]);
const MISSING_RESULT_TEXT = "Tool result unavailable: history ends before this call completed.";
```

#### 2.5.2 `ContextBounds`【字段逐字】

```ts
export type ContextBounds = {
  readonly head: (EntryRecord & { readonly head: EntryId }) | undefined;
  readonly tail: EntryId;
};
```

注意 `head` 是**条目记录本身**（带 `head` 字段的 head marker 条目），`tail` 是**最大的可见条目 id**。

#### 2.5.3 `captureContextBounds(storage, conversationId, context, at?)`

- `at === undefined`：`scanEntries({ conversationId }, 1, undefined, context).items[0]?.id` 作为 tail（**返回的第一条**，即最新/最大 id；Storage 的默认顺序是 descending）；空会话返回 `undefined`。
- `at` 给出：`storage.entry(conversationId, at, context)` 不存在则抛【逐字】`Entry ${at} is not visible from conversation ${conversationId}`；否则 tail = at。
- 返回 `{ head: await storage.findLatestHeadMarker(conversationId, tail, context), tail }`。
- **必须在线上**运行（O(1) 两次读），之后可离线推导。

#### 2.5.4 `ContextRange`

```ts
export type ContextRange = {
  readonly bounds: ContextBounds;
  readonly entries: readonly EntryRecord[];       // 升序，从 head 的 head（或起点）到 tail
  readonly view: ContextView;
  readonly edited: ReadonlySet<EntryId>;          // 被 edits 命中的目标
  readonly settled: readonly Message[];           // 最后一条 assistant 之前的有序消息
  readonly open: readonly Message[];              // 最后一条 assistant 起（含）的贡献消息（未排序）
  readonly baseline: Baseline;                    // Message | null | undefined
};
type Baseline = Message | null | undefined;
```

#### 2.5.5 `readContextFrom(session, storage, conversationId, context, at, previous)`

返回 `{ view, range }`：
1. `bounds = await session.readOnLine(() => captureContextBounds(...))`；`undefined` → `{ view: emptyView(), range: undefined }`；
2. `freezeJson(bounds.head)`；
3. 分支：
   - `previous === undefined || previous.bounds.head?.id !== bounds.head?.id` → 全量读：`deriveRange(bounds, freezeJson(await scanRange(...rangeQuery(bounds))))`；
   - `bounds.tail === previous.bounds.tail` → **`range = previous`（同一对象，完全复用）**；
   - `bounds.tail < previous.bounds.tail` → `deriveRange(bounds, previous.entries.filter(e => e.id <= bounds.tail))`（截断）；
   - 否则 → `minEntryId = idFromNumber(previous.bounds.tail + 1)`；`added = scanRange(..., { minEntryId, maxEntryId: bounds.tail })`；`extendRange(previous, bounds, freezeJson(added))`。
4. 返回 `{ view: { ...view, entries: [...], contributions: [...], messages: [...] }, range }`——**浅拷贝三个数组**（避免调用者持有内部数组）。

#### 2.5.6 `readContext(session, storage, conversationId, context, at?)`

线上捕获 bounds → `deriveContext(storage, conversationId, bounds, context)`。

#### 2.5.7 `deriveContext(storage, conversationId, bounds, context)`

`bounds === undefined` → `emptyView()`（`{ head: undefined, entries: [], contributions: [], messages: [] }`）；否则 `deriveRange(bounds, await scanRange(...)).view`。

#### 2.5.8 `deriveRange(bounds, entries)`

```ts
const edits = new Map<EntryId, ContextEdit>();
for (const entry of entries) for (const edit of entry.edits ?? []) edits.set(edit.target, edit);   // 后写覆盖：最大 id 胜
const active = selectActive(bounds.head, entries);
const contributions = active.map(entry => contribute(entry, edits.get(entry.id)));
const { settled, open } = settle([], contributions.flat());
const baseline = findBaseline(bounds.head, active);
const messages = leadWithSystem([...settled, ...orderToolResults(open)], baseline);
return { bounds, entries, view: { head: bounds.head, entries: active, contributions, messages },
         edited: new Set(edits.keys()), settled, open, baseline };
```

- edits 遍历**整个 range**（含被 `selectActive` 丢掉的旧 head marker 的 edits）；
- `contributions` 与 `active` 一一对应。

#### 2.5.9 `extendRange(previous, bounds, added)`

```ts
const entries = [...previous.entries, ...added];
if (added.some(e => e.edits !== undefined || e.head !== undefined || previous.edited.has(e.id))) return deriveRange(bounds, entries);
const contributions = added.map(e => contribute(e, undefined));
const { settled, open } = settle(previous.settled, [...previous.open, ...contributions.flat()]);
const baseline = previous.baseline !== undefined ? previous.baseline : findBaseline(bounds.head, added);
const view = { head: bounds.head, entries: [...previous.view.entries, ...added],
               contributions: [...previous.view.contributions, ...contributions],
               messages: leadWithSystem([...settled, ...orderToolResults(open)], baseline) };
return { bounds, entries, view, edited: previous.edited, settled, open, baseline };
```

- 只有在"新增条目既无 edits/head、也不是旧 edit 目标"时才增量；否则整体重算。
- `previous.baseline !== undefined` 时沿用（`null` 也算已定）；否则在新增里找。

#### 2.5.10 `findBaseline(head, entries)`

```ts
for (const entry of entries) {
  if (head !== undefined && entry.id <= head.id) continue;        // head 之前的旧条目跳过
  if (SystemEntry.is(entry)) return entry.model?.find(m => m.role === "system") ?? null;
  if (head === undefined && carriesSystemMessage(entry)) return null;
}
return undefined;
```

- 找到第一个（head 之后）`kiana.system` 条目 → 取其中第一条 system 消息；没有则 `null`。
- 无 head marker 时，若在遇到 `kiana.system` 之前就有任何条目携带 system 消息（自身或 replace edit）→ `null`（说明基线不是"完整基线"）。
- 都没有 → `undefined`（后续条目仍可能成为基线）。

#### 2.5.11 `carriesSystemMessage(entry)`

`entry.model?.some(m => m.role === "system")` 或 `entry.edits?.some(e => e.action === "replace" && e.messages.some(m => m.role === "system"))`。

#### 2.5.12 `leadWithSystem(messages, baseline)`

```ts
const index = messages.findIndex(m => m.role === "system");
if (index <= 0) return messages;
const system = messages[index];
if (system !== baseline && messages.slice(0, index).some(m => m.role !== "user")) return messages;
return [system, ...messages.slice(0, index), ...messages.slice(index + 1)];
```

- 第一个 system 消息前面只有 user 消息，或它就是 baseline → 移到最前；
- 否则不动（避免改变语义）。

#### 2.5.13 `contribute(entry, edit)`

```ts
if (edit?.action === "omit") return Object.freeze([]);
const contributed = edit?.action === "replace" ? edit.messages : (entry.model ?? []);
return Object.freeze(contributed.filter(m => m.role !== "assistant" || !EXCLUDED_STOP_REASONS.has(m.stopReason)));
```

即：`aborted` / `error` / `deferred` 的 assistant 消息**不进入**模型上下文（但仍在 transcript 里）。

#### 2.5.14 `freezeJson(value)`

`WeakSet` `deeplyFrozen`；`value` 非对象或已冻结直接返回；否则递归 `Object.values` 后 `Object.freeze` 并记入 Set。Rust 用不可变 `Arc<JsonValue>`，不需要冻结。

#### 2.5.15 `settle(settled, open)`

```ts
const last = open.findLastIndex(m => m.role === "assistant");
if (last <= 0) return { settled, open };
return { settled: [...settled, ...orderToolResults(open.slice(0, last))], open: open.slice(last) };
```

#### 2.5.16 `activeEntries(storage, conversationId, bounds, context)`

`bounds === undefined` → `[]`；否则 `selectActive(bounds.head, await scanRange(...))`。

#### 2.5.17 `rangeQuery(bounds)`

`head === undefined ? { maxEntryId: bounds.tail } : { minEntryId: head.head, maxEntryId: bounds.tail }`。

#### 2.5.18 `scanRange(...)` 与 `selectActive(...)`

- `scanRange`：以 `SCAN_PAGE_SIZE=256` 分页取 `{ conversationId, ...range }` 的所有条目，拼接后 **`reverse()`**（Storage 返回降序，这里转升序）。
- `selectActive(head, range)`：`head === undefined ? range : [head, ...range.filter(e => e.head === undefined)]`——head marker 放最前，其余非 head 条目按升序。

#### 2.5.19 `orderToolResults(messages)`【关键算法】

```ts
const ordered: Message[] = [];
for (let index = 0; index < messages.length; index++) {
  const message = messages[index];
  if (message.role === "toolResult") continue;            // 结果统一延后插入
  ordered.push(message);
  if (message.role !== "assistant") continue;
  const calls = message.content.filter(c => c.type === "toolCall");
  if (calls.length === 0) continue;
  const results = new Map<string, number>();              // toolCallId → 位置
  for (let next = index + 1; next < messages.length && messages[next].role !== "assistant"; next++) {
    const candidate = messages[next];
    if (candidate.role === "toolResult" && !results.has(candidate.toolCallId)) results.set(candidate.toolCallId, next);
  }
  for (const call of calls) {
    const resultIndex = results.get(call.id);
    ordered.push(resultIndex === undefined ? missingResult(call, message.timestamp) : messages[resultIndex]);
  }
}
return ordered;
```

- 结果只从"下一条 assistant 之前"的窗口里找；**同一 toolCallId 的重复结果只取第一个**；
- 缺失结果合成 `missingResult`（timestamp 取该 assistant 的）；
- **未被任何调用引用的 toolResult 被丢弃**。

`missingResult(call, timestamp)`【逐字】：

```json
{ "role": "toolResult", "toolCallId": "<call.id>", "toolName": "<call.name>",
  "content": [{ "type": "text", "text": "Tool result unavailable: history ends before this call completed." }],
  "isError": true, "details": { "reason": "missing_result" }, "timestamp": <timestamp> }
```
（并 `freezeJson`）

### 2.6 `src/harness/prompt.ts`（158 行，3 个导出 + 1 个内部）

导出：`replaySections`、`renderSections`、`planSystemEntries`（内部：`planTools`、`planSections`、`systemEntry`）。

#### 2.6.1 `replaySections(messages)` → `Map<string, string>`

```ts
for (const message of messages) {
  if (message.role !== "system" || message.sections === undefined) continue;
  for (const [key, value] of Object.entries(message.sections)) {
    if (value === null) shown.delete(key);
    else shown.set(key, value);
  }
}
```

- "就地设置，`null` 删除，重新添加会**追加到末尾**"（Map 语义：delete 后 set 会排到最后）；
- 只处理 `role === "system"` 且有 `sections` 的消息；顺序按消息顺序、再按对象键顺序。

#### 2.6.2 `renderSections(sections, input, shown, report, context)` → `Map<string, string>`

```ts
for (const section of sections) {
  let text;
  try { text = await section.render(input, context); }
  catch (error) {
    if (context.abortSignal?.aborted) throw error;      // 已取消：传播
    report(error, section);                             // 否则报告
    const kept = shown.get(section.key);
    if (kept !== undefined) desired.set(section.key, kept);   // 保留上次显示文本
    continue;
  }
  if (text === undefined) continue;                     // 渲染成 undefined：省略
  desired.set(section.key, section.tag === false ? text : `<${section.key}>\n${text}\n</${section.key}>`);
}
```

【逐字】包裹格式 `` `<${key}>\n${text}\n</${key}>` ``（仅当 `section.tag !== false`）。

#### 2.6.3 `planSystemEntries(view, desired, tools, timestamp)` → `SystemDraft[]`

`type SystemDraft = TypedEntryDraft<never>`，即 `{ model: [SystemMessage] , ... }`（可带 `edits`）。

分支 A：**head marker 存在且 head 之后无 `kiana.system` 条目**
```ts
const edits = view.entries.filter(e => SystemEntry.is(e)).map(e => ({ target: e.id, action: "omit" }));
const baseline = systemEntry(Object.fromEntries(desired), { toolsRemoved: [], toolsAdded: tools.map(toToolDeclaration) }, timestamp);
return [edits.length === 0 ? baseline : { ...baseline, edits }];
```
- 写一条**完整基线**：所有 desired sections + 全部工具声明；
- 并对范围内（`view.entries` 中含 head marker 本身？`filter` 全部 system 条目，包括 head 之前被 head 排除的？——`view.entries` 是 `active`，即 `[head, ...range非head]`，所以只含 head 及其后的条目）每个 `kiana.system` 条目加 `omit` edit；
- **即使基线与重放值相同也会写**。

分支 B：否则
```ts
const sections = planSections(replaySections(view.messages), desired);
const changes = planTools(getCurrentTools(view.messages), tools);
if (changes.toolsRemoved.length === 0 && changes.toolsAdded.length === 0)
  return sections.map(patch => systemEntry(patch, undefined, timestamp));
if (sections.length === 0) return [systemEntry(undefined, changes, timestamp)];
return sections.map((patch, index) => systemEntry(patch, index === sections.length - 1 ? changes : undefined, timestamp));
```

#### 2.6.4 `planTools(offered, desired)` → `{ toolsRemoved: ToolReference[]; toolsAdded: Tool[] }`

```ts
const wanted = new Map(desired.map(t => [t.name, t]));
const kept = offered.filter(t => { const next = wanted.get(t.name); return next !== undefined && declarationsEqual(t, next); });
const keptNames = new Set(kept.map(t => t.name));
const added = desired.filter(t => !keptNames.has(t.name));
const replayed = [...kept, ...added];
if (replayed.some((t, i) => t.name !== desired[i].name))
  return { toolsRemoved: offered.map(t => ({ name: t.name })), toolsAdded: desired.map(toToolDeclaration) };
return {
  toolsRemoved: offered.filter(t => !keptNames.has(t.name)).map(t => ({ name: t.name })),
  toolsAdded: added.map(toToolDeclaration),
};
```

- 声明不变（`declarationsEqual`）的工具保留；改动过的"移除再添加"；
- 若重放顺序与目标顺序不一致 → 全部移除 + 全部按序重加。

#### 2.6.5 `planSections(shown, desired)` → `Record<string, string | null>[]`

```ts
const patchedOrder = [...[...shown.keys()].filter(k => desired.has(k)), ...[...desired.keys()].filter(k => !shown.has(k))];
const desiredOrder = [...desired.keys()];
if (patchedOrder.some((k, i) => k !== desiredOrder[i]))
  return [Object.fromEntries([...shown.keys()].map(k => [k, null])), Object.fromEntries(desired)];
const patch = {};
for (const [key, value] of shown) { const next = desired.get(key); if (next !== value) patch[key] = next ?? null; }
for (const [key, value] of desired) if (!shown.has(key)) patch[key] = value;
return Object.keys(patch).length === 0 ? [] : [patch];
```

- 顺序会不一致 → 两个 patch：全删（`null`）+ 全加；
- 否则最小 patch（改值、删缺、加新）；无变化返回 `[]`。

#### 2.6.6 `systemEntry(sections, tools, timestamp)`

```ts
const message: SystemMessage = {
  role: "system", content: "",
  ...(sections === undefined ? {} : { sections }),
  ...(tools === undefined || tools.toolsRemoved.length === 0 ? {} : { toolsRemoved: tools.toolsRemoved }),
  ...(tools === undefined || tools.toolsAdded.length === 0 ? {} : { toolsAdded: tools.toolsAdded }),
  timestamp,
};
return { model: [message] };
```

【逐字】`content: ""` 恒定；`sections`/`toolsRemoved`/`toolsAdded` 为空时不出现（不写 `undefined` 键）。

### 2.7 `src/harness/inbox.ts`（168 行，15 个导出）

导出：`InboxItem`、`InboxState`、`InboxDoc`、`QueueModes`、`Boundary`、`BoundaryResult`、`prepareBoundary`、`applyBoundary`、`appendInput`、`pickInputEntry`、`isDisplayOnly`、`hasQueuedReset`、`isStale`、`removeInboxItem`、`withdrawQueuedInputs`。

#### 2.7.1 `InboxItem` / `InboxState` / `InboxDoc`

```ts
export type InboxItem =
  | { id: SubmissionId; mode: "steer" | "followUp"; content: JsonRepresentation<UserInput>; entry?: JsonRepresentation<InputEntry> }
  | { id: SubmissionId; mode: "write"; entry: JsonObject };
export type InboxState = { items: InboxItem[] };

export const InboxDoc = defineDoc<InboxState>({
  kind: "kiana.inbox", version: 1, scope: "conversation",
  history: "latest", fork: "initial",
  initial: () => ({ items: [] }),
  checkpointWhen: value => value.items.length === 0,
});
```

- `checkpointWhen`：**只有在空队列时**才 checkpoint（非空时用 delta 链，压缩存储）。

#### 2.7.2 `Boundary` / `BoundaryResult`

```ts
export type QueueModes = Pick<Settings, "steeringMode" | "followUpMode">;
export type Boundary = {
  readonly conversationId: ConversationId;
  readonly inbox: Draft<InboxState>;
  readonly steeringMode: QueueMode;
  readonly followUpMode: QueueMode;
  head: EntryId | undefined;
};
export type BoundaryResult = { readonly users: SubmissionId[]; readonly reset: boolean };
```

#### 2.7.3 `prepareBoundary(tx, conversationId, modes)`

```ts
const head = (await tx.latestHeadMarker(conversationId))?.head;
const inbox = await tx.doc(InboxDoc, conversationId);
return { conversationId, inbox, steeringMode: modes.steeringMode, followUpMode: modes.followUpMode, head };
```

必须在**提交的第一次表写之前**调用（表读不能晚于表写，否则 `ReadAfterWrite`）。

#### 2.7.4 `applyBoundary(tx, boundary, at, now)`【关键顺序】

```ts
const items = inbox.items;
const reset = hasQueuedReset(items);
const final = at === "final" || reset;
const pick = (mode, queueMode) => {
  const indexes = items.flatMap((item, index) => (item.mode === mode ? [index] : []));
  return queueMode === "all" ? indexes : indexes.slice(0, 1);
};
const writes = items.flatMap((item, index) => (item.mode === "write" ? [index] : []));
const users = [...pick("steer", steeringMode), ...(final ? pick("followUp", followUpMode) : [])];
users.sort((a, b) => a - b);
```

1. **writes 先**（按 inbox 顺序）：stale 判断 → `tx.settleSubmission(item.id, { status: "unanswered", reason: "stale" })`；否则 `entry = await tx.appendEntry(conversationId, draft)`；若 `draft.head !== undefined` 则 `boundary.head = draft.head === "self" ? entry.id : draft.head`；`tx.placeSubmission(item.id, entry.id)`。
2. **users 后**（按索引升序）：`appendInput(...)` → `tx.placeSubmission(item.id, entry.id)` → 收集到 `placed`。
3. **位置删除**：`removed = [...writes, ...users].sort((a, b) => b - a)`，逐个 `items.splice(index, 1)`（降序删除避免位移）。
4. 返回 `{ users: placed, reset }`。

语义要点：
- `reset`（队列里有 `head: "self"` 写）把 `postTools` 边界**升级为 `final`**；
- `steer` 与 `followUp` 的队列模式分别由 `steeringMode`/`followUpMode` 决定（`one-at-a-time` 只取第一个）；
- **写先于用户输入**，因此 reset 之后排队的用户输入会落进新上下文；
- 目标 head 早于当前活动范围（含本次提交中已推进的范围）的写是 stale（settle `unanswered`/`stale`，不写条目）。

#### 2.7.5 `appendInput(tx, conversationId, content, entry, now)`

```ts
const message = { role: "user", content, timestamp: now };
if (entry === undefined) return tx.appendEntry(UserEntry, conversationId, { model: [message] });
return tx.appendEntry(conversationId, { ...pickInputEntry(entry), model: [message] });
```

#### 2.7.6 `pickInputEntry({kind, data})`

`data === undefined ? { kind } : { kind, data }`，再 `copyJson(..., { omitUndefinedProperties: true })`。**只保留 kind 与 data**（丢弃未类型化调用者可能传入的其他字段）。

#### 2.7.7 `isDisplayOnly(entry)`

`entry.model === undefined && entry.head === undefined && entry.edits === undefined`（**present 即计数，即使空数组**）。

#### 2.7.8 `hasQueuedReset(items)` / `isStale(boundary, entry)`

- `items.some(item => item.mode === "write" && item.entry.head === "self")`；
- `typeof entry.head === "number" && boundary.head !== undefined && entry.head < boundary.head`。

#### 2.7.9 `removeInboxItem(tx, conversationId, id)`

取 `InboxDoc` 的 `items`，`findIndex(item => item.id === id)`，`>= 0` 则 `splice(index, 1)`。

#### 2.7.10 `withdrawQueuedInputs(tx, conversationId, reason = "aborted")`

**倒序**遍历 `items`；跳过 `mode === "write"`；对其余 `tx.settleSubmission(item.id, { status: "unanswered", reason })` 并 `splice`。队列写保留（等待后续边界放置）。

### 2.8 `src/harness/submissions.ts`（277 行，2 个导出：`Submissions`、`admitSubmission`）

#### 2.8.1 `Submissions` 类

字段：`#telemetry`、`#session`、`#storage`、`#now`、`#queueModes`、`#resume`、`#waiters: Waiters<SubmissionId, SettledSubmissionRecord>`、`#closed`。

构造函数：`session.observeCommits(pub => this.#observe(pub))`；`session.subscribeClose(() => { this.#closed = true; this.#waiters.rejectAll(closedError(this.#session)); })`。

| 方法 | 精确行为 |
|---|---|
| `submit(conversationId, draft, context)` | `#resume()`；`parent = draft.type === "input" ? #telemetry : context.telemetry`；`createTypedSpanStarter(parent, [DURABLE_TELEMETRY_SCHEMA])("kiana.durable.submit", { "submission.type": draft.type, "conversation.id": conversationId }, async span => { if (draft.type === "input") span.addLink({ context: context.telemetry }); try { const { id, status } = await session.commitWith(tx => admitSubmission(tx, conversationId, draft, #now(), #queueModes(), span), withTelemetry(span, context)); span.setAttributes({ "submission.id": id, result: status }); return new SubmissionHandle(id, this); } catch (error) { span.setStatus({ status: "error" }); throw error; } })` |
| `get(id, context)` | `readOnLine(storage.submission(id))`；不存在 → `undefined`；否则 `new SubmissionHandle(record.id, this)` |
| `status(id, context)` | 同上读取；不存在 → 抛【逐字】`Submission ${id} does not exist` |
| `wait(id, context)` | `#resume()`；`readOnLine`：读记录；不存在 → 抛 `Submission ${id} does not exist`；已 settle → `{ promise: Promise.resolve(record) }`；`#closed` → 抛 `closedError`；否则 `{ promise: #waiters.add(id, context) }` |
| `abort(id, context, conversationId?)` | `commitWith`：`record = await tx.submission(id)`；不存在或（给了 conversationId 且不匹配）→ `"not_found"`；`status === "queued"` → `tx.settleSubmission(id, { status: "unanswered", reason: "aborted" })` + `removeInboxItem` → `"aborted"`；`status === "placed"` → `"already_placed"`；否则 `"settled"` |
| `#observe(publication)` | `publication.kind === "memory"` 直接 return；遍历 `publication.changes`，`change.type === "submission" && isSettled(change.value)` → `#waiters.resolve(change.value.id, change.value)` |

`SubmissionHandle`：`status`/`wait` 委托；`abort` 若 `Submissions.abort` 返回 `"not_found"` 则抛 `Submission ${id} does not exist`，否则返回结果。

`isSettled(record)`：`status === "done" || status === "unanswered"`。

#### 2.8.2 `checkInputEntry(entry)`

【逐字】
```ts
throw new TypeError(`An input entry's kind must be a non-empty string not starting with "kiana.", got ${String(kind)}`)
```
条件：`typeof kind !== "string" || kind.length === 0 || kind.startsWith("kiana.")`。`String(kind)` 对非字符串做 JS 字符串化（`undefined` → `"undefined"`，`null` → `"null"`，数字 → 十进制等）。

#### 2.8.3 `admitSubmission(tx, conversationId, draft, now, queueModes, span?)` → `{ id, status }`【核心顺序】

1. 若 `draft.type === "input" && draft.entry !== undefined` → `checkInputEntry`。
2. 若 `draft.requestId !== undefined`：
   - `existing = await tx.submissionByRequest(conversationId, draft.requestId)`；
   - 存在且 `existing.type !== draft.type` → 抛【逐字】`Request ${draft.requestId} already identifies a submission of type ${existing.type}`；
   - 存在且类型相同 → 返回 `{ id: existing.id, status: existing.status === "unanswered" ? "done" : existing.status }`（**不写任何东西**；`unanswered` 被归一为 `done`）。
3. `fields = { conversationId, requestId: draft.requestId, trace: span?.propagation?.inject(span) }`。
4. **display-only 写快速路径**：
   ```ts
   if (draft.type === "write" && isDisplayOnly(draft.entry) && !hasQueuedReset((await tx.doc(InboxDoc, conversationId)).items)) {
     const entry = await tx.appendEntry(conversationId, draft.entry);
     const write = { ...fields, type: "write", status: "done", entry: entry.id };
     return { id: (await tx.createSubmission(write)).id, status: "done" };
   }
   ```
   仅当**没有**排队的 reset 时才即时追加（否则要排到 reset 之后）。
5. `live = await tx.doc(LiveDoc, conversationId)`；`busy = live.run !== undefined`。
6. steer 追踪链接：`draft.type === "input" && draft.whenBusy === "steer" && live.run !== undefined && span?.propagation !== undefined` → `run = await tx.task(live.run.taskId)`；`run?.trace !== undefined` → `span.addLink({ context: span.propagation.extract(run.trace) })`。
7. `busy && draft.type === "input" && draft.whenBusy === "reject"` → 抛 `new ConversationBusy(conversationId)`（【逐字】`Conversation ${conversationId} is busy`，见 durable-rest）。
8. **边界准备（必须在首次表写前）**：`boundary = busy ? undefined : await prepareBoundary(tx, conversationId, queueModes)`。
9. 若 `boundary === undefined || boundary.inbox.items.length > 0`（忙，或空闲但已有排队项）：
   - `{ id } = await tx.createSubmission({ ...fields, type: draft.type, status: "queued" })`；
   - `value = copyJson(draft.type === "write" ? draft.entry : draft.content, { omitUndefinedProperties: true })`；
   - `items = boundary?.inbox ?? await tx.doc(InboxDoc, conversationId)`；
   - `write` → `items.push({ id, mode: "write", entry: value })`；
   - `input` → `mode = draft.whenBusy === "steer" ? "steer" : "followUp"`；push `{ id, mode, content: value, ...(draft.entry === undefined ? {} : { entry: pickInputEntry(draft.entry) }) }`；
   - `boundary === undefined`（忙）→ 返回 `{ id, status: "queued" }`；
   - 否则（空闲但已有排队）：`{ users } = await applyBoundary(tx, boundary, "final", now)`；`users.length > 0` → `await startRun(tx, conversationId, live, users)`；返回 `{ id, status: draft.type === "write" ? "done" : users.at(-1) === id ? "placed" : "queued" }`（**注意：输入的 status 依据是否排在最后**）。
10. 空闲、队列空：
    - `write`：`isStale(boundary, draft.entry)` → 创建 `{ ...fields, type: "write", status: "unanswered", reason: "stale" }` 提交并返回 `{ id, status: "done" }`；否则 `appendEntry` → 创建 `{ ...fields, type: "write", status: "done", entry: entry.id }` → `{ id, status: "done" }`。
    - `input`：`entry = await appendInput(tx, conversationId, draft.content, draft.entry, now)`；`input = { ...fields, type: "input", status: "placed", entry: entry.id }`；`{ id } = await tx.createSubmission(input)`；`await startRun(tx, conversationId, live, [id])`；返回 `{ id, status: "placed" }`。

> **Rust 提示**：`span?.propagation?.inject(span)` / `extract(run.trace)` 是 kiana-telemetry 的 W3C trace 传播，可空实现（NOOP 时返回 undefined）。

### 2.9 `src/harness/live.ts`（284 行，18 个导出）

导出：`ToolSlot`、`NestedToolSlot`、`NestedToolSummary`、`CompactionStatus`、`CompactionFailure`、`LiveState`、`LiveDoc`、`PROGRESS_COMMIT`、`endRun`、`addCompactionStatus`、`compactionStatus`、`removeCompactionStatus`、`failCompactionStatus`、`toolSlot`、`finishSlot`、`removeNestedSlots`、`clearProgress`、`settleSchedulerOutcome`（内部类型 `SlotProgress`）。

#### 2.9.1 `SlotProgress`（内部）/ `ToolSlot` / `NestedToolSlot`【字段逐字】

```ts
type SlotProgress = {
  callId: string;
  name: string;
  status: "pending" | "running" | "done";
  output?: string;            // 保留的运行输出
  droppedBytes?: number;
  droppedLines?: number;
  details?: JsonValue;        // 最后一次 details() 值
  diagnostics?: ToolDiagnostic[];   // api.diagnostic() 记录
};
export type ToolSlot = SlotProgress & {
  taskId?: TaskId;            // 尚未开始的顺序调用、以及请求未提供的调用（后者 done 且带 entry）没有
  entry?: EntryId;            // 完成后写；faulted/orphaned 时没有
};
export type NestedToolSlot = SlotProgress & {
  parentCallId: string;
  parentTaskId: TaskId;
  taskId: TaskId;
  arguments: JsonObject;
  summary?: NestedToolSummary;
};
export type NestedToolSummary = { isError: boolean; durationMs?: number; usage?: JsonRepresentation<Usage>; error?: string };
```

#### 2.9.2 `CompactionStatus` / `CompactionFailure` / `LiveState`

```ts
export type CompactionStatus = {
  taskId: TaskId<CompactionResult>;
  reason: CompactionReason;
  blocking: boolean;
  attempt: number;
  retry?: { at: number; error: string };
};
export type CompactionFailure = { taskId: TaskId<CompactionResult>; reason: CompactionReason; error: string };
export type LiveState = {
  run?: { taskId: TaskId; inputs: SubmissionId[] };
  generation?: {
    attempt: number;
    message?: JsonRepresentation<AssistantMessage>;
    retry?: { at: number; error: string };
    deferred?: { pollAt: number };
  };
  tools?: ToolSlot[];
  nestedTools?: NestedToolSlot[];
  compactions?: CompactionStatus[];
  compactionFailure?: CompactionFailure;
};
```

#### 2.9.3 `LiveDoc` 与 `PROGRESS_COMMIT`

```ts
export const LiveDoc = defineDoc<LiveState>({
  kind: "kiana.live", version: 1, scope: "conversation",
  history: "latest", fork: "initial",
  initial: () => ({}),
  checkpointWhen: value =>
    value.generation === undefined &&
    !(value.tools ?? []).some(slot => slot.status === "running") &&
    !(value.nestedTools ?? []).some(slot => slot.status === "running"),
});
export const PROGRESS_COMMIT: CommitOptions = { persistence: { kind: "deferred" } };
```

- `checkpointWhen` 的**不变量**（源码注释明确要求，性能基准会检查）：只要没有 generation 且没有 running 的工具/嵌套槽，就必须是完整 base。因此 base 最多跨一个 generation 或一轮工具的重叠执行；槽位只在 running 时持有 output，base 很小；嵌套结果存在调用者的任务文档里，**不放这里**。**不得**增加"delta 数上限"之类的规则。
- `PROGRESS_COMMIT` 是**所有**进度提交（partial 答案、工具输出、details、diagnostics）的持久化策略：延迟存储；`kiana.live` 的下一次读写提交会把它一并落盘，或在 `progress.persistWithinMs` 后由定时器落盘。

内部常量：`RUN_TASK_KINDS = new Set(["kiana.generation"])`、`TOOL_TASK_KIND = "kiana.tool"`、`COMPACTION_TASK_KIND = "kiana.compaction"`。

#### 2.9.4 `endRun(tx, live, taskId, settlement)`

```ts
if (live.run?.taskId === taskId) {
  for (const id of live.run.inputs) tx.settleSubmission(id, settlement);
  delete live.run;
}
delete live.generation;
delete live.tools;
delete live.nestedTools;
```

注意：即使 `run` 不属于该 task，也**总是**删除 generation/tools/nestedTools（它们的展示属于正在结束的 run）。

#### 2.9.5 compaction 状态辅助

- `addCompactionStatus(live, status)`：`live.compactions ??= []` 后 `push(status)`（任务 id 升序由创建顺序保证）。
- `compactionStatus(live, taskId)`：`live.compactions?.find(s => s.taskId === taskId)`。
- `removeCompactionStatus(live, taskId)`：找到则 `splice`；数组空则 `delete live.compactions`。
- `failCompactionStatus(live, taskId, error)`：若存在该状态，`live.compactionFailure = { taskId: status.taskId, reason: status.reason, error }`；然后移除状态。

#### 2.9.6 `toolSlot(live, taskId)`

```ts
const slot = live.tools?.find(each => each.taskId === taskId);
if (slot !== undefined || live.nestedTools === undefined) return slot;
// nestedTools 按 taskId 升序：二分查找
let low = 0, high = nested.length - 1;
while (low <= high) {
  const middle = (low + high) >>> 1;
  const candidate = nested[middle];
  if (candidate.taskId === taskId) return candidate;
  if (candidate.taskId < taskId) low = middle + 1; else high = middle - 1;
}
return undefined;
```

**必须用二分**（一个工具可能发起上千个嵌套调用，每次提交多次查槽位，而每个被读的 draft 元素都会在 Chord change tracker 里产生节点）。

#### 2.9.7 `finishSlot(slot)` / `clearProgress(slot)`

`finishSlot`：`slot.status = "done"; clearProgress(slot)`。
`clearProgress`：`delete slot.output/droppedBytes/droppedLines/details/diagnostics`。

#### 2.9.8 `removeNestedSlots(live, taskId)`

```ts
const removed = new Set<TaskId>([taskId]);
for (const slot of nested) if (removed.has(slot.parentTaskId)) removed.add(slot.taskId);   // 传递闭包（父先于子）
for (let index = nested.length - 1; index >= 0; index--) if (removed.has(nested[index].parentTaskId)) nested.splice(index, 1);
if (nested.length === 0) delete live.nestedTools;
```

#### 2.9.9 `settleSchedulerOutcome(tx, record, outcome)`

调度器自己写 `faulted`/`orphaned` 终态时的 harness 清理（调度器不知道 task kind，由 Harness 注入）：

- `record.kind === "kiana.tool"`：取 `LiveDoc`；`toolSlot` 存在则 `finishSlot`；`removeNestedSlots(live, record.id)`；返回。
- `record.kind === "kiana.compaction"`：`error = outcome.status === "faulted" ? outcome.error.message : outcome.reason`；`failCompactionStatus(await tx.doc(LiveDoc, ...), record.id, error)`；返回。
- 不是 `RUN_TASK_KINDS` 之一 → 返回（**绝不**在其他会话创建 `kiana.live`）。
- generation：`live.run?.taskId !== record.id` → 返回；否则 `await convertPartial(tx, live, record.conversationId)`；`endRun(tx, live, record.id, outcome.status === "faulted" ? { status: "unanswered", reason: "faulted", detail: outcome.error.message } : { status: "unanswered", reason: outcome.reason })`。

注释明确：此处提交的 aborted assistant 条目**没有** `byTaskId` 归属（调度器提交没有任务 scope）；fault 来自任务 bug 或畸形 provider 数据；不确定的存储失败会毒化 Session 而不写 outcome。

### 2.10 `src/harness/generation.ts`（797 行，7 个导出）

导出：`GenerationInput`、`GenerationCheckpoint`、`GenerationResult`、`GenerationTask`、`convertPartial`、`startRun`、`handOver`（内部：`readCalls`、`createToolTask`、`thresholdCompaction`、`compactionFailure`、`failModelError`、`failNoModel`、`prepareFailure`、`endFailedRun`、`streamResponse`、`classify`、`answer`、`startToolRound`、`finishToolRound`、`appendAssistant`、`createGeneration`、`DEFAULT_POLL_AFTER_MS`）。

#### 2.10.1 类型（同上源码结构）

```ts
export type GenerationInput = Record<string, never>;
export type GenerationCheckpoint =
  | { phase: "prepare"; attempt: number; compacted?: TaskId<CompactionResult>; overflow?: string; threshold?: true }
  | { phase: "request"; attempt: number; compacted?: TaskId<CompactionResult>;
      model: ModelRef; thinkingLevel: ModelThinkingLevel;
      streamOptions: ConversationStreamOptions; cutoff: EntryId }
  | { phase: "retry"; attempt: number; compacted?: TaskId<CompactionResult>; until: number }
  | { phase: "poll"; attempt: number; compacted?: TaskId<CompactionResult>;
      model: ModelRef; thinkingLevel?: ModelThinkingLevel;   // 旧版 checkpoint 无此字段
      cutoff: EntryId; handle: DeferredHandle; pollAt: number }
  | { phase: "tools"; assistant: EntryId; tools: TaskId<ToolTaskResult>[]; pending: string[] };
export type GenerationResult = { entryId: EntryId };
const DEFAULT_POLL_AFTER_MS = 5000;
```

#### 2.10.2 任务定义

```ts
export const GenerationTask = defineTask<...>({
  name: "kiana.generation",
  version: 1,
  initial: () => ({ phase: "prepare", attempt: 1 }),
  phases: { prepare, request, retry, poll, tools },
  abort,
});
```

**没有 `migrate`**（v1 是最早版本）。

#### 2.10.3 `prepare(task, runtime, context)`【严格顺序】

1. `agent = await runtime.agent(context)`；`settings = runtime.settings`；`{ model, thinkingLevel } = agent`；
   `resolved = model === undefined ? undefined : runtime.models.getModel(model.provider, model.modelId)`；
   `model === undefined || resolved === undefined` → `return failNoModel(runtime, model, context)`。
2. 读 checkpoint：`{ attempt, compacted, overflow, threshold: blocked }`。
3. **溢出压缩结果检查**：`compacted !== undefined && overflow !== undefined` → `runtime.outcomes([compacted])`；若 `outcome?.status !== "completed" || outcome.result.entryId === undefined` → `failModelError(runtime, overflow, context)`。
4. **阈值压缩结果检查**：`compacted !== undefined && blocked === true` → `outcomes([compacted])`；若 `outcome !== undefined && outcome.status !== "completed"` → `failModelError(runtime, compactionFailure(outcome), context)`。
5. `view = await runtime.context(conversationId, context)`；`shown = replaySections(view.messages)`。
6. `env`：`try { env = await runtime.env(context) } catch (error) { if (context.abortSignal?.aborted) throw error; runtime.report(error, { type: "host", callback: "env" }); }`。
7. `input: PromptInput = { conversationId, agent, env, shown: Object.fromEntries(shown), read: runtime }`。
8. `desired = await renderSections(agent.sections, input, shown, (error, section) => runtime.report(error, { type: "extension", ...optionalExtension(sectionExtension(agent, section.key)) }), context)`。
9. `entries = planSystemEntries(view, desired, agent.tools, runtime.now())`。
10. `threshold = compacted === undefined ? thresholdCompaction(view, entries, resolved.contextWindow, compactionPolicy(settings, model)) : undefined`。
11. **blocking 分支**：
    ```ts
    await runtime.commit(async (tx): Promise<Next> => {
      const child = await createCompaction(tx, conversationId, { reason: "threshold" }, runtime.taskId);
      const checkpoint = { phase: "prepare", attempt, compacted: child, threshold: true } as const;
      return { status: "waiting", checkpoint, on: [child], policy: "allSettled" };
    }, context);
    return;
    ```
    （转录本在压缩追加前不变，故重新 prepare。）
12. **正常提交**：
    ```ts
    await runtime.commit(async (tx) => {
      let cutoff = (await tx.scanEntries({ conversationId }, 1)).items[0]?.id;
      for (const entry of entries) cutoff = (await tx.appendEntry(SystemEntry, conversationId, entry)).id;
      if (cutoff === undefined) throw new Error(`Conversation ${conversationId} has no entries to send`);
      if (threshold === "background" && (await tx.doc(LiveDoc, conversationId)).compactions === undefined) {
        await createCompaction(tx, conversationId, { reason: "threshold" });   // 无 owner：会话所有
      }
      const request = {
        attempt,
        ...(compacted === undefined ? {} : { compacted }),
        model, thinkingLevel,
        streamOptions: settings.stream,
        cutoff,
      };
      return { status: "running", checkpoint: { phase: "request", ...request } };
    }, context);
    ```
    【逐字】`Conversation ${conversationId} has no entries to send`。
    注意 cutoff 取"计划写入前的最新条目 id"（`scanEntries(...).items[0]?.id`，降序第一条），再被每个新追加的 system 条目 id 覆盖；若会话完全无条目（且无 system 计划）则抛错。background 压缩在同一提交里"若无 compactions 才建"（准备期间已准入的压缩也算）。

#### 2.10.4 `request(task, runtime, context)`

1. 解构 checkpoint：`{ attempt, compacted, model: ref, thinkingLevel, streamOptions, cutoff }`。
2. 提交：`live = await tx.doc(LiveDoc)`；`await convertPartial(tx, live, conversationId)`；`live.generation = { attempt }`。
3. `model = runtime.models.getModel(ref.provider, ref.modelId)`；`undefined` → `failNoModel(runtime, ref, context)`。
4. `view = await runtime.context(conversationId, context, { at: cutoff })`；`messages = view.messages`。
5. `beforeRequest` 钩子链：`runtime.hooks.each("beforeRequest", async hook => { const replaced = await hook({ messages }, runtime, context); if (replaced !== undefined) messages = replaced.messages; })`。
6. `options: SimpleStreamOptions = { ...streamOptions, signal: runtime.signal, telemetry: context.telemetry, sessionId: await ensureProviderSessionId(runtime, context), ...(thinkingLevel === "off" ? {} : { reasoning: thinkingLevel }) }`。
7. `message = await streamResponse(runtime, model, messages, options, attempt, context)`。
8. `request = { attempt, compacted, model: ref, thinkingLevel, cutoff, messages: view.messages }`（**注意 `messages` 用的是未被钩子替换的 `view.messages`**，分类与工具轮据此判断 offered）。
9. `await classify(runtime, request, message, context)`。

#### 2.10.5 `retry` / `poll` / `tools` 阶段

**retry**：`until > runtime.now()` → `runtime.sleepUntil(until, task.state.checkpoint, context)`；否则提交：`live.generation = { attempt: attempt + 1 }`，checkpoint `{ phase: "prepare", attempt: attempt + 1, ...(compacted?) }`，`status: "running"`。

**poll**：`pollAt > runtime.now()` → `sleepUntil(pollAt, ...)`；`model = getModel(...)`，`undefined` → `failNoModel`；`message = await runtime.models.fetchDeferred(model, handle, { signal: runtime.signal, telemetry: context.telemetry })`；`request = { attempt, compacted, model: ref, thinkingLevel, cutoff, pollAt }`；`classify`。

**tools**：`{ assistant, tools, pending }`；`[next, ...rest] = pending`；`next === undefined` → `finishToolRound(runtime, assistant, tools, context)`；否则提交：`live.tools?.find(slot => slot.callId === next && slot.taskId === undefined)` 设置 `slot.taskId = taskId`；checkpoint `{ phase: "tools", assistant, tools: [...tools, taskId], pending: rest }`，`status: "waiting"`，`on: [taskId]`，`policy: "allSettled"`。

#### 2.10.6 `abort(task, runtime, context)`

1. 若 checkpoint 是 `poll`：取 model；存在则 `await runtime.models.cancelDeferred(model, checkpoint.handle, { signal: runtime.signal, telemetry: context.telemetry })`；抛错 → `runtime.report(error, { type: "host", callback: "models" })`（吞掉）。
2. `unstarted = checkpoint.phase === "tools" ? await readCalls(runtime, checkpoint.assistant, checkpoint.pending, context) : []`。
3. 提交：
   ```ts
   const live = await tx.doc(LiveDoc, conversationId);
   await convertPartial(tx, live, conversationId);
   for (const call of unstarted) {
     const result = harnessError("aborted", `Tool ${call.name} was aborted`);
     await appendToolResult(tx, conversationId, call, result, { timestamp: runtime.now() });
   }
   endRun(tx, live, runtime.taskId, { status: "unanswered", reason: "aborted" });
   return { status: "terminal", outcome: { status: "aborted" } };
   ```
   【逐字】`Tool ${call.name} was aborted`。
   语义：abort handler 在该轮工具任务全部终态之后运行；**从未开始**的调用得到 `aborted` 结果。

#### 2.10.7 `readCalls(runtime, assistant, callIds, context)`

读 `AssistantEntry` 的 `model[0]`；若 `role === "assistant"` 取其 `content` 中 `type === "toolCall"` 的项；按 `callIds` 顺序 `flatMap(id => calls.filter(c => c.id === id).slice(0, 1))`（每个 id 最多一次；找不到的跳过）。

#### 2.10.8 `createToolTask(tx, runtime, assistant, callId)`

`tx.createTask(ToolTask, { kind: "model", assistant, callId }, { ownership: { kind: "task", taskId: runtime.taskId } })`——**generation 拥有**。

#### 2.10.9 `thresholdCompaction(view, planned, contextWindow, policy)`

```ts
if (!policy.enabled || contextWindow <= 0) return undefined;
const tokens = estimateContext(view, planned.flatMap(entry => entry.model ?? []));
const blocking = contextWindow - policy.reserveTokens;
const background = blocking - policy.backgroundTokens;
const over = tokens > blocking ? "blocking" : policy.backgroundTokens > 0 && tokens > background ? "background" : undefined;
if (over === undefined || selectCut(view, policy.keepRecentTokens) === undefined) return undefined;
return over;
```

#### 2.10.10 `compactionFailure(outcome)`【逐字】

```ts
case "failed":
case "faulted":   return `Compaction failed: ${outcome.error.message}`;
case "orphaned":  return `Compaction failed: ${outcome.reason}`;
case "aborted":   return "Compaction was aborted";
```

#### 2.10.11 `failModelError(runtime, text, context)` / `failNoModel(runtime, ref, context)`

`failModelError`：
```ts
await runtime.commit(async (tx) => {
  const failure = await prepareFailure(tx, runtime);
  const live = await tx.doc(LiveDoc, runtime.conversationId);
  await endFailedRun(tx, runtime, live, failure, { status: "unanswered", reason: "model_error", detail: text });
  return { status: "terminal", outcome: { status: "failed", error: { message: text, detail: { reason: "model_error" } } } };
}, context);
```

`failNoModel`：`message = ref === undefined ? "No model is configured" : `Model ${ref.provider}/${ref.modelId} is not available``；settlement `{ status: "unanswered", reason: "no_model" }`（**无 detail**）；outcome `{ status: "failed", error: { message, detail: { reason: "no_model" } } }`。

#### 2.10.12 `prepareFailure(tx, runtime)` → `{ policy, boundary? }`

```ts
const policy = runtime.settings.afterFailedRun;
if (policy !== "continue") return { policy };
return { policy, boundary: await prepareBoundary(tx, runtime.conversationId, settings) };
```

（表读必须在首次表写前。）

#### 2.10.13 `endFailedRun(tx, runtime, live, failure, settlement)`

```ts
endRun(tx, live, runtime.taskId, settlement);
if (failure.policy === "withdraw") await withdrawQueuedInputs(tx, conversationId, "run_failed");
if (failure.boundary === undefined) return;
const { users } = await applyBoundary(tx, failure.boundary, "final", runtime.now());
if (users.length > 0) await startRun(tx, conversationId, live, users);
```

`afterFailedRun` 的三种行为：`keep`（默认，不动队列）、`continue`（立即 final 边界并启动下一 run）、`withdraw`（队列输入 settle `unanswered`/`run_failed`，写保留）。

#### 2.10.14 `convertPartial(tx, live, conversationId)`

```ts
const partial = live.generation?.message;
if (partial === undefined) return;
const message = copyJson(partial) as unknown as AssistantMessage;
await appendAssistant(tx, conversationId, { ...message, stopReason: "aborted" });
```

#### 2.10.15 `streamResponse(runtime, model, messages, options, attempt, context)`

变量：`interval = settings.progress.partialIntervalMs`、`pending: AssistantMessage | undefined`、`timer`、`inFlight`、`stopped = false`、`changes = new PartialChanges()`。

`flush()`：
```ts
timer = undefined;
const partial = pending; pending = undefined;
if (partial === undefined || stopped) return;
inFlight = (async () => {
  const message = copyJson(partial, { omitUndefinedProperties: true }) as PartialMessage;   // 同步拷贝：provider 仍在改 partial
  const taken = changes.take(message);
  try {
    await runtime.commit(async (tx) => {
      const live = await tx.doc(LiveDoc, runtime.conversationId);
      live.generation ??= { attempt };
      changes.commit(live.generation, message, taken);
      return undefined;
    }, context, PROGRESS_COMMIT);
  } catch (error) { changes.restore(taken); throw error; }
})()
  .catch(error => { if (!runtime.signal.aborted) runtime.report(error); })
  .finally(() => { inFlight = undefined; if (pending !== undefined && !stopped) timer = runtime.setTimer(flush, interval); });
```

主循环：
```ts
const events = runtime.models.streamSimple(model, { messages: [...messages] }, options);
for await (const event of events) {
  if (event.type === "done" || event.type === "error" || event.partial.content.length === 0) continue;
  changes.observe(event);
  pending = event.partial;
  if (timer === undefined && inFlight === undefined) timer = runtime.setTimer(flush, interval);
}
return await events.result();
finally { stopped = true; timer?.(); await inFlight; }
```

语义：最多一个 commit 在飞；`finally` 停表并等待最后一个 commit，确保没有陈旧 partial 落在结果之后；空内容 partial（如 kiana-ai 的 `start`）不产生提交（deferred 响应永远不会留下 partial）。

#### 2.10.16 `classify(runtime, request, message, context)`

1. `runtime.signal.throwIfAborted()`。
2. `if (thinkingLevel !== undefined) message.thinkingLevel = thinkingLevel;`（记录请求的等级）。
3. **deferred**：`message.stopReason === "deferred" && message.deferred !== undefined` →
   ```ts
   const pollAt = Math.max(runtime.now() + (handle.pollAfterMs ?? DEFAULT_POLL_AFTER_MS),
                          request.pollAt === undefined ? Number.NEGATIVE_INFINITY : request.pollAt + 1);
   commit: live.generation = { attempt, deferred: { pollAt } };
           checkpoint = { phase: "poll", attempt, ...(compacted?), model: ref, ...(thinkingLevel?), cutoff, handle, pollAt };
           return { status: "sleeping", checkpoint, until: pollAt };
   ```
4. `await runtime.hooks.each("afterResponse", hook => hook(message, runtime, context))`。
5. `calls = message.content.filter(c => c.type === "toolCall")`；
   `stopReason === "toolUse" && calls.length > 0` → `startToolRound(...)`。
6. `stopReason === "stop" | "length" | "toolUse"` → `answer(...)`。
7. 读设置：`settings = runtime.settings`；`overflow = message.stopReason === "error" && isContextOverflow(message)`；`compaction = compactionPolicy(settings, ref)`。
8. **溢出压缩**：`overflow && compacted === undefined && compaction.enabled` →
   `view = await runtime.context(conversationId, context, { at: cutoff })`；`selectCut(view, compaction.keepRecentTokens) !== undefined` →
   ```ts
   const text = message.errorMessage ?? "Context overflow";
   commit: live = doc(LiveDoc); await appendAssistant(tx, conversationId, message); delete live.generation;
           child = await createCompaction(tx, conversationId, { reason: "overflow" }, runtime.taskId);
           checkpoint = { phase: "prepare", attempt, compacted: child, overflow: text };
           return { status: "waiting", checkpoint, on: [child], policy: "allSettled" };
   ```
9. **重试判定**：
   ```ts
   const policy = settings.retry;
   const retry = message.stopReason === "error" && !overflow && isRetryableAssistantError(message)
                 && policy.enabled && attempt <= policy.maxRetries;
   const until = retry ? runtime.now() + retryDelayMs(policy, attempt) : 0;
   ```
10. **最终提交**：
    ```ts
    const failure = retry ? undefined : await prepareFailure(tx, runtime);     // 先表读
    const live = await tx.doc(LiveDoc, conversationId);
    await appendAssistant(tx, conversationId, message);
    if (failure === undefined) {
      live.generation = { attempt, retry: { at: until, error: message.errorMessage ?? "" } };
      checkpoint = { phase: "retry", attempt, ...(compacted?), until };
      return { status: "sleeping", checkpoint, until };
    }
    const text = message.errorMessage ?? `Model response ended with stop reason ${message.stopReason}`;
    await endFailedRun(tx, runtime, live, failure, { status: "unanswered", reason: "model_error", detail: text });
    return { status: "terminal", outcome: { status: "failed", error: { message: text, detail: { reason: "model_error" } } } };
    ```
    【逐字】`Model response ended with stop reason ${message.stopReason}`。

#### 2.10.17 `answer(runtime, message, context)`

1. `onYield` 钩子链：`runtime.hooks.each("onYield", async hook => { if (continuation !== undefined) return; continuation = (await hook(message, runtime, context))?.continue; })`（**第一个 continuation 胜出**）。
2. 提交：
   ```ts
   const boundary = await prepareBoundary(tx, conversationId, runtime.settings);   // 线上读队列模式
   const live = await tx.doc(LiveDoc, conversationId);
   const entry = await appendAssistant(tx, conversationId, message);
   const result: Next = { status: "terminal", outcome: { status: "completed", result: { entryId: entry.id } } };
   const { users, reset } = await applyBoundary(tx, boundary, "final", runtime.now());
   if (continuation !== undefined && users.length === 0 && !reset) {
     const user = { role: "user", content: continuation, timestamp: runtime.now() };
     await tx.appendEntry(UserEntry, conversationId, { model: [user] });
     handOver(live, runtime.taskId, await createGeneration(tx, conversationId));
     delete live.generation;
     return result;
   }
   endRun(tx, live, runtime.taskId, { status: "done", answer: entry.id });
   if (users.length > 0) await startRun(tx, conversationId, live, users);
   return result;
   ```
   语义：answer 的 `entryId` 是**本次 assistant 条目**（即使交接也如此）。

#### 2.10.18 `startToolRound(runtime, request, message, calls, context)`

1. `messages = request.messages ?? (await runtime.context(conversationId, context, { at: request.cutoff })).messages`；
   `offered = new Set(getCurrentTools(messages).map(t => t.name))`。
2. `tools = (await runtime.agent(context)).tools`（轮开始时读取）。
3. `sequential = settings.toolExecution === "sequential" || calls.some(call => offered.has(call.name) && tools.find(t => t.name === call.name)?.executionMode === "sequential")`。
4. 提交：
   - `entry = await appendAssistant(tx, conversationId, message)`；
   - 逐个 call：
     - **未提供**：`unavailable = harnessError("tool_unavailable", `Tool ${call.name} is not available`)`；`result = await appendToolResult(...)`；`slots.push({ callId, name, status: "done", entry: result.id })`；continue。
     - 顺序轮且已有工具任务：`pending.push(call.id)`；`slots.push({ callId, name, status: "pending" })`。
     - 否则：`taskId = await createToolTask(tx, runtime, entry.id, call.id)`；`tools.push(taskId)`；`slots.push({ callId, name, taskId, status: "pending" })`。
   - `delete live.generation; live.tools = slots;`
   - checkpoint `{ phase: "tools", assistant: entry.id, tools, pending }`；`status: "waiting"`，`on: tools`，`policy: "allSettled"`。

#### 2.10.19 `finishToolRound(runtime, assistant, tools, context)`

1. `outcomes = await runtime.outcomes(tools, context)`；
   `controls`：对每个 tool id，`outcome.status === "completed" && outcome.result.kind !== "nested" ? outcome.result.control : undefined`（v1 结果无 kind；generation 轮里读到的都是 model 调用）。
2. `slots = (await runtime.snapshot(LiveDoc, conversationId, context))?.tools ?? []`。
3. `results = slots.flatMap(slot => slot.entry === undefined ? [] : [slot.entry])`。
4. `afterTools` 钩子：`hook(assistant, results, runtime, context)`。
5. `terminate = slots.length > 0 && slots.every(slot => slot.taskId !== undefined && controls.get(slot.taskId)?.terminate === true)`（**包括**无任务的调用，它们没有 taskId 故不可能满足）。
6. `added = [...controls.values()].flatMap(control => control?.addTools ?? [])`。
7. `handoff = [...controls.values()].findLast(control => control?.handoff !== undefined)?.handoff`（**调用顺序中最后一个**）。
8. 提交：
   - `boundary = await prepareBoundary(tx, conversationId, runtime.settings)`（先表读）；
   - `await addTools(tx, conversationId, added)`（若有）；
   - `live = await tx.doc(LiveDoc)`；`now = runtime.now()`；
   - handoff 存在 → `entry = await tx.appendEntry(ResetEntry, conversationId, { head: "self", model: [{ role: "user", content: handoff, timestamp: now }] })`；`boundary.head = entry.id`（handoff 的 reset 先于所有排队项）；
   - `terminate` → `applyBoundary(tx, boundary, "final", now)`；`endRun(tx, live, runtime.taskId, { status: "done", answer: assistant })`；`users.length > 0` → `startRun`；
   - 否则 → `{ users, reset } = applyBoundary(tx, boundary, "postTools", now)`；
     - `reset` → `endRun(..., { status: "unanswered", reason: "reset" })`；`users.length > 0` → `startRun`；
     - 否则 → `delete live.tools; delete live.nestedTools; if (live.run?.taskId === runtime.taskId) live.run.inputs.push(...users); handOver(live, runtime.taskId, await createGeneration(tx, conversationId));`
   - 返回 `{ status: "terminal", outcome: { status: "completed", result: { entryId: assistant } } }`。

#### 2.10.20 `appendAssistant` / `startRun` / `createGeneration` / `handOver`

```ts
async function appendAssistant(tx, conversationId, message) {
  await recordUsage(tx, conversationId, "models", `${message.provider}/${message.model}`, message.usage);
  return tx.appendEntry(AssistantEntry, conversationId, { model: [message] });
}
export async function startRun(tx, conversationId, live, inputs: SubmissionId[]): Promise<void> {
  live.run = { taskId: await createGeneration(tx, conversationId, { submissionIds: inputs }), inputs };
}
function createGeneration(tx, conversationId, telemetry = {}) {
  return tx.createTask(GenerationTask, {}, { ownership: { kind: "conversation" }, conversationId, ...telemetry });
}
export function handOver(live, from, to) { if (live.run?.taskId === from) live.run.taskId = to; }
```

**不变式**：所有内置的 assistant 条目写入都经过 `appendAssistant()`（保证 usage 账本完整）。

### 2.11 `src/harness/tool.ts`（976 行，8 个导出）

导出：`ToolTaskInput`、`ToolTaskCheckpoint`、`ToolTaskResult`、`NestedCallState`、`NestedCallDoc`、`ToolTask`、`harnessError`、`appendToolResult`（内部：`MADE_NESTED_CALLS`、`Runtime`、`resolveTool`、`readCall`、`prepare`、`validate`、`invalid`、`run`、`progressClock`、`publishProgress`、`finalResult`、`structuredValidators`、`structuredOutputError`、`settle`、`nestedResult`、`startNestedCall`、`outputValue`、`fallbackResult`、`fromSlot`、`toolDiagnostic`、`truncated`、`summaryOf`、`errorTextOf`、`renderDiagnostics`、`boundContent`、`checkKey`、`jsonEqual`、`MAX_SUMMARY_ERROR_CHARS`、`COMPLETED`）。

#### 2.11.1 输入/检查点/结果

```ts
export type ToolTaskInput =
  | { kind: "model"; assistant: EntryId; callId: string }
  | { kind: "nested"; parent: TaskId; parentCallId: string; key: string; call: ToolCall; progress?: false };

export type ToolTaskCheckpoint =
  | { phase: "call" }
  | { phase: "execute"; arguments: JsonObject; replay: "safe" | "unsafe" };

export type ToolTaskResult = { kind: "model"; entryId: EntryId; control?: ToolControl } | { kind: "nested" };
```

注意：**v1 遗留**的 `completing` 结果可能没有 `kind`，读作 model-issued（`finishToolRound` 里 `outcome.result.kind !== "nested"`）。

#### 2.11.2 `MADE_NESTED_CALLS` / `NestedCallState` / `NestedCallDoc`

```ts
const MADE_NESTED_CALLS = "kiana.tool.madeNestedCalls";
export type NestedCallState = {
  taskId?: TaskId<ToolTaskResult>;    // 仅在创建调用那次提交内缺失；创建失败则不留成员
  result?: JsonRepresentation<NestedToolExecutionResult>;
};
export const NestedCallDoc = defineDocFamily<NestedCallState, null>({
  kind: "kiana.tool.nested-call", version: 1,
  scope: "task", family: true,
  initial: () => ({}),
  checkpointWhen: () => false,        // 随任务一起退休
});
```

- memo 名 `"kiana.tool.madeNestedCalls"`：在**每次尝试的第一次嵌套准入之前**提交（只有那时 `kiana.live` 才可能有它的子槽位，settle 时要删）。
- `NestedCallDoc` 是**任务作用域族文档**，key = 嵌套调用的 `key`；一次调用一份文档，任何提交都不重写其他文档。

#### 2.11.3 `ToolTask` 定义

```ts
export const ToolTask = defineTask<...>({
  name: "kiana.tool",
  version: 2,
  initial: () => ({ phase: "call" }),
  migrate: (input, checkpoint) => ({
    input: { kind: "model", ...(input as { assistant: EntryId; callId: string }) },
    checkpoint: checkpoint as ToolTaskCheckpoint,
  }),
  phases: { call, execute },
  abort,
});
```

**v1 → v2 迁移**：给输入加 `kind: "model"`，checkpoint 原样。v1 只有 model 调用。

#### 2.11.4 `call` 阶段【严格顺序】

1. `call = await readCall(runtime, task.input, context)`。
2. `tool = await resolveTool(runtime, task.input, call.name, context)`；`undefined` → `settle(runtime, input, call, COMPLETED, () => harnessError("tool_unavailable", `Tool ${call.name} is not available`), context)`。
3. `prepared = prepare(tool, call.arguments)`；`checked = "error" in prepared ? prepared : validate(tool, call, prepared.args)`；`"error" in checked` → `settle(..., invalid(checked.error), ...)`。
4. `args = checked.args`；`block: string | undefined`；
   `beforeTool` 钩子链：
   ```ts
   if (block !== undefined) return;                       // 第一个 block 之后不再调用后续钩子
   try {
     const decision = await hook({ ...call, arguments: args }, runtime, context);
     if (decision?.block !== undefined) block = decision.block;
     else if (decision?.arguments !== undefined) args = decision.arguments;
   } catch (error) {
     if (runtime.signal.aborted) throw error;
     block = errorMessage(error);                         // 抛错 == block
   }
   ```
5. `block !== undefined` → `settle(..., harnessError("blocked", `Tool call blocked: ${block}`), ...)`。
6. 再次 `validate(tool, call, args)`；错误 → invalid。
7. `final = copyJson(validated.args, { omitUndefinedProperties: true })`（严格 JSON：repair 可能把可选属性设为 undefined）。
8. 提交（意图）：
   ```ts
   const slot = toolSlot(await tx.doc(LiveDoc, runtime.conversationId), runtime.taskId);
   if (slot !== undefined) slot.status = "running";
   const changed = task.input.kind === "nested" && !jsonEqual(task.input.call.arguments, final);
   if (changed && slot !== undefined && "parentTaskId" in slot) slot.arguments = final;
   const intent = { phase: "execute", arguments: final, replay: tool.replay ?? "unsafe" };
   return { status: "running", checkpoint: intent };
   ```
   （与输入比较而非读 draft，避免为每个叶子建追踪节点。）
9. `await run(runtime, task.input, call, tool, final, context)`。

#### 2.11.5 `execute` 阶段（仅恢复会到达）

1. `{ arguments: args, replay } = checkpoint`；`call = readCall(...)`；`tool = resolveTool(...)`。
2. `replay === "safe" && tool?.replay === "safe"` → 提交清空槽位进度（`clearProgress(slot)`），`run(...)` 重跑。
3. 否则：`message = `Tool ${call.name} was interrupted and may have partially run``；`ending = { status: "failed", message }`（`failed` 记录取消意图，令该调用拥有的会话被 abort）；`settle(..., slot => fromSlot(slot, "interrupted", message), ...)`。

#### 2.11.6 `abort` 阶段

```ts
const call = await readCall(runtime, task.input, context);
const [code, message] =
  task.abortReason !== "restart"
    ? ["aborted", `Tool ${call.name} was aborted`]
    : task.state.checkpoint.phase === "execute"
      ? ["interrupted", `Tool ${call.name} was interrupted by a restart and may have partially run`]
      : ["abandoned", `Tool ${call.name} was not started: its caller ended with a restart`];
await settle(runtime, task.input, call, { status: "aborted" }, slot => fromSlot(slot, code, message), context);
```

#### 2.11.7 `resolveTool` / `readCall`

- `resolveTool`：`agent = await runtime.agent(context)`；`(input.kind === "model" ? agent.tools : agent.callable).find(t => t.name === name)`。
- `readCall`：
  - nested：`context.telemetry.setAttributes({ "tool.name": input.call.name, "tool.call_id": input.call.id })`；返回 `{ ...input.call, parent: { taskId: input.parent, callId: input.parentCallId } }`。
  - model：`entry = await runtime.entry(AssistantEntry, input.assistant, context)`；`message = entry?.model?.[0]`；从 `message.content` 找 `type === "toolCall" && id === input.callId`；找不到抛【逐字】`Entry ${input.assistant} has no tool call ${input.callId}`；设置同样的 telemetry 属性；返回 call（**无 parent**）。

#### 2.11.8 `prepare` / `validate` / `invalid`

```ts
function prepare(tool, args): Checked {
  if (tool.prepareArguments === undefined) return { args };
  try { return { args: tool.prepareArguments(args) as JsonObject }; }
  catch (error) { return { error: errorMessage(error) }; }
}
function validate(tool, call, args): Checked {
  try { return { args: validateToolArguments(tool, { ...call, arguments: args }) as JsonObject }; }
  catch (error) { return { error: errorMessage(error) }; }
}
function invalid(message) { return harnessError("invalid_arguments", message); }
```

#### 2.11.9 `run(runtime, input, call, tool, args, context)`【核心】

准备：
```ts
const limits: OutputLimits = {
  maxBytes: tool.outputLimits?.maxBytes ?? DEFAULT_MAX_BYTES,     // 50 * 1024
  maxLines: tool.outputLimits?.maxLines ?? DEFAULT_MAX_LINES,     // 2000
  retain: tool.outputLimits?.retain ?? "head",
};
const reported: Reported = { output: new OutputBuffer(limits), limits, diagnostics: [], details: undefined };
const streams = input.kind === "model" || input.progress !== false;
const progress = publishProgress(runtime, reported, streams, context);
const admissions: Promise<unknown>[] = [];
const resumes = tool.replay === "safe";
let sequence = 0;
let madeNestedMemo: Promise<unknown> | undefined;
let ended = false;
const assertLive = () => { if (ended) throw new Error(`Tool call ${call.id} has settled`); };
let env: ExecutionEnv | undefined;
```

`ToolExecutionApi` 实现要点：
| 成员 | 行为 |
|---|---|
| `taskId`/`conversationId`/`callId`/`registry`/`agent` | 直接取自 runtime |
| `models`、`env` | getter（`env` 在 `run` 内延迟构建） |
| `now()` | `runtime.now()` |
| `output(chunk, skipped)` | `assertLive()`；`reported.output.push(chunk, skipped)` 返回真则 `progress.mark()` |
| `outputWindow` | `retain === "tail"` 时为 `{ maxBytes, maxLines, minIntervalMs: settings.progress.outputIntervalMs, bytesPerSecond: PROGRESS_BYTES_PER_SECOND /* 100*1024 */ }`；否则 `undefined` |
| `retainedOutput()` | `assertLive()`；`snapshot()` → `{ text, truncated: droppedBytes > 0 }` |
| `diagnostic(d)` | `assertLive()`；`reported.diagnostics.push(copyJson(d, { omitUndefinedProperties: true }))`；`progress.mark()` |
| `details(value, ctx)` | `assertLive()`；`ctx.abortSignal?.throwIfAborted()`；`reported.details = copyJson(value, { omitUndefinedProperties: true })`；`const committed = progress.markAndWait(); committed.catch(() => {}); return awaitWithContext(committed, ctx)` |
| `commit(change, ctx, options)` | 包一层 `runtime.commit` 并返回 change 的结果 |
| `memo` | `runtime.memo`（直接复用） |
| `createTask(task, input, options, ctx)` | 若 `options.ownership.kind === "task" && !resumes` → `{ abandonOnRestart: true, ...options }`；提交内 `tx.createTask` |
| `getTask`/`waitForTask`/`conversation`/`snapshot`/`snapshotAsOf`/`watchDoc` | 直接转调 runtime |
| `executeTool(name, args, ctx, options = {})` | 见下 |

`executeTool` 实现：
```ts
assertLive();
const key = options.key ?? String(++sequence);
if (options.key !== undefined) checkKey(options.key);
madeNestedMemo ??= runtime.memo(MADE_NESTED_CALLS, true, callContext).catch(error => { madeNestedMemo = undefined; throw error; });
const admission = madeNestedMemo.then(() => startNestedCall(runtime, call.id, name, toolArgs,
  { key, progress: options.progress, abandonOnRestart: !resumes }, callContext));
admissions.push(admission);
const id = await admission;
const settled = await runtime.waitForTask(id, callContext);
const stored = (await runtime.snapshot(NestedCallDoc, runtime.taskId, key, callContext))?.result;
return stored === undefined
  ? fallbackResult(id, name, settled.state.outcome)
  : copyJson(stored);            // 工具可修改的副本
```

执行与收尾：
```ts
let result: ToolExecutionResult;
let ending = COMPLETED;
let durationMs: number | undefined;
try {
  env = await runtime.env(context);                     // 环境构建失败也走 catch
  const startedAt = performance.now();
  try { result = await tool.execute(args, api, context); }
  finally { durationMs = Math.round(performance.now() - startedAt); }
} catch (error) {
  if (runtime.signal.aborted) {
    ended = true;
    for (const waiter of await progress.stop()) waiter.reject(error);
    throw error;
  }
  result = { isError: true, diagnostics: [toolDiagnostic("tool_error", errorMessage(error))] };
  ending = { status: "failed", message: `Tool ${call.name} threw` };
}
ended = true;
await Promise.allSettled(admissions);                   // 让已开始的准入提交，settle 才能 abort/列出它们
reported.output.end();
const pending = await progress.stop();
try {
  const settled = await finalResult(runtime, input, call, tool, result, reported, context);
  await settle(runtime, input, call, ending, () => settled, context, durationMs);
} catch (error) {
  for (const waiter of pending) waiter.reject(error);
  throw error;
}
for (const waiter of pending) waiter.resolve();
```

#### 2.11.10 `progressClock(runtime)`

```ts
let last = 0;
return {
  now: () => { try { last = runtime.now(); } catch { /* invocation ended */ } return last; },
  setTimer: (cb, delayMs) => runtime.setTimer(cb, delayMs),
};
```

#### 2.11.11 `publishProgress(runtime, reported, enabled, context)` → `Progress`

```ts
let written = { text: "", details: undefined, diagnostics: 0 };
return new Progress(async () => {
  if (!enabled) return 0;
  const snapshot = reported.output.snapshot();
  const current = { text: snapshot.text, details: reported.details, diagnostics: reported.diagnostics.length };
  const added = reported.diagnostics.slice(written.diagnostics, current.diagnostics);
  const detailsChanged = current.details !== written.details;
  let bytes = 0;
  if (snapshot.text !== written.text) {
    const shared = snapshot.text.startsWith(written.text) ? written.text.length : overlap(written.text, snapshot.text, 65_536);
    bytes += utf8ByteLength(snapshot.text.slice(shared));
  }
  if (detailsChanged) bytes += utf8ByteLength(JSON.stringify(current.details ?? null));
  if (added.length > 0) bytes += utf8ByteLength(JSON.stringify(added));
  await runtime.commit(async (tx) => {
    const slot = toolSlot(await tx.doc(LiveDoc, runtime.conversationId), runtime.taskId);
    if (slot === undefined) return undefined;
    if ((slot.output ?? "") !== snapshot.text) slot.output = snapshot.text;      // 整串赋值，让 Chord 差分成 append / trim+append
    if (snapshot.droppedBytes > 0) slot.droppedBytes = snapshot.droppedBytes;
    if (snapshot.droppedLines > 0) slot.droppedLines = snapshot.droppedLines;
    if (detailsChanged && current.details !== undefined) assignJson(slot, "details", current.details);
    if (added.length > 0) {
      if (slot.diagnostics === undefined) slot.diagnostics = [];
      for (const diagnostic of added) slot.diagnostics.push(diagnostic);
    }
    return undefined;
  }, context, PROGRESS_COMMIT);
  written = current;
  return bytes;
}, error => { if (!runtime.signal.aborted) runtime.report(error); },
   runtime.settings.progress.outputIntervalMs,
   progressClock(runtime));
```

**必须**：`slot.output` 作为**单个字符串字段**整体赋值（Chord 会差分成 append 或 trim+append）；替换整个 slot 对象会在每次提交记录整个窗口。`details` 用 `assignJson` 逐叶差分，新诊断逐个 push。

#### 2.11.12 `finalResult(runtime, input, call, tool, result, reported, context)`

```ts
const harness: ToolDiagnostic[] = [];
const retained = result.output === undefined ? reported.output.snapshot() : undefined;
const output: Content = retained === undefined ? result.output!
  : retained.text === "" ? [] : [{ type: "text", text: retained.text }];
let final: ToolExecutionResult = {
  ...result, output,
  details: result.details === undefined ? reported.details : result.details,
  diagnostics: [...reported.diagnostics, ...(result.diagnostics ?? [])],
};
await runtime.hooks.each("afterTool", async hook => { final = (await hook(call, final, runtime, context)) ?? final; });
if (retained !== undefined && retained.droppedBytes > 0 && final.output === output) harness.push(truncated(retained, reported.limits.retain));
const bounded = boundContent(final.output ?? [], reported.limits);
if (bounded.droppedBytes > 0) harness.push(truncated(bounded, reported.limits.retain));
const broken = structuredOutputError(tool, final);
if (broken !== undefined) {
  const { structuredOutput: _dropped, ...rest } = final;
  if (input.kind === "nested") { final = { ...rest, isError: true }; harness.push(toolDiagnostic("invalid_structured_output", broken)); }
  else {
    final = rest;
    const extension = await runtime.agent(context).then(agent => toolExtension(agent, tool.name), () => undefined);
    runtime.report(new Error(broken), { type: "extension", ...optionalExtension(extension) });
  }
}
if (input.kind === "nested" && tool.structuredOutputSchema === undefined) {
  final = { ...final, structuredOutput: outputValue(bounded.content) };
}
if (input.kind === "nested" && tool.structuredOutputSchema !== undefined && final.isError === true
    && final.structuredOutput === undefined
    && ![...(final.diagnostics ?? []), ...harness].some(d => d.severity === "error")) {
  const text = bounded.content.flatMap(item => item.type === "text" ? [item.text] : []).join("\n");
  if (text !== "") harness.push(toolDiagnostic("tool_error", text));
}
return { ...final, output: bounded.content, diagnostics: [...(final.diagnostics ?? []), ...harness] };
```

顺序语义：
1. `output` 回退为保留输出（单条 text，空串则 `[]`）；
2. `details` 回退为最后一次 `details()`；
3. `diagnostics` = api 记录的（按记录顺序）+ 结果自带的；
4. `afterTool` 链（每个钩子返回非 undefined 即替换）；
5. 保留输出的截断诊断**仅在 afterTool 没换掉 output 时**加；
6. `boundContent` 再截断（可能又加一条诊断）；
7. structuredOutput 契约校验（见下）；
8. 无 schema 的 nested：`structuredOutput = outputValue(bounded.content)`；
9. 有 schema 的 nested 错误结果且无 structuredOutput 且没有 error 级诊断 → 把 output 文本变成 `tool_error` 诊断；
10. 最终 `diagnostics` 顺序：`final.diagnostics`（含 afterTool 替换后的）在前，`harness` 生成的在后。

#### 2.11.13 `structuredOutputError(tool, result)`【逐字】

```ts
if (schema === undefined) return value === undefined ? undefined
  : `Tool ${tool.name} returned structuredOutput but declares no structuredOutputSchema`;
if (value === undefined) return result.isError === true ? undefined : `Tool ${tool.name} returned no structuredOutput`;
const [first] = validator.Errors(value);
const path = first?.instancePath.replace(/^\//, "").replace(/\//g, ".") || "root";
return `Tool ${tool.name} returned structuredOutput that does not match its schema: ${path}: ${first?.message ?? "invalid"}`;
```

- `structuredValidators: WeakMap<schema, Compile(schema)>`（按 schema 对象缓存 TypeBox 编译器）。
- 校验：`validator.Check(value)`；失败取第一条错误。
- 路径转换：去掉开头 `/`，其余 `/` 换成 `.`；空则 `"root"`。

#### 2.11.14 `settle(runtime, input, call, ending, build, context, durationMs?)`

```ts
const nestedIds = (await runtime.ownedTasks(context))
  .filter(r => r.kind === ToolTask.definition.name && (r.input as ToolTaskInput).kind === "nested")
  .map(r => r.id);
const madeNested = (await runtime.memo(MADE_NESTED_CALLS, context)) === true;
await Promise.all(nestedIds.map(id => runtime.abortOwned(id, context)));     // 先全部标记，再等
await runtime.commit(async (tx) => {
  const live = await tx.doc(LiveDoc, conversationId);
  const slot = toolSlot(live, runtime.taskId);
  const result = build(slot);
  let settled: ToolTaskResult;
  if (input.kind === "nested") {
    const nested = nestedResult(runtime.taskId, result, durationMs);
    if (nested.usage !== undefined) await recordUsage(tx, conversationId, "tools", call.name, nested.usage);
    const state = await tx.doc(NestedCallDoc, input.parent, input.key, null);
    if (state.taskId !== runtime.taskId) throw new Error(`Nested call ${call.id} is not in its caller's index`);
    state.result = nested;
    if (slot !== undefined && "parentTaskId" in slot) { finishSlot(slot); slot.summary = summaryOf(nested, result.output ?? []); }
    settled = { kind: "nested" };
  } else {
    const timing = { timestamp: runtime.now(), durationMs };
    const entry = await appendToolResult(tx, conversationId, call, result, timing);
    if (slot !== undefined && !("parentTaskId" in slot)) { finishSlot(slot); slot.entry = entry.id; }
    const control = result.control === undefined || ending.status !== "completed" ? {}
      : { control: copyJson(result.control, { omitUndefinedProperties: true }) };
    settled = { kind: "model", entryId: entry.id, ...control };
  }
  if (madeNested) removeNestedSlots(live, runtime.taskId);
  if (ending.status === "aborted") return { status: "terminal", outcome: { status: "aborted", result: settled } };
  if (ending.status === "failed") return { status: "terminal", outcome: { status: "failed", error: { message: ending.message }, result: settled } };
  return { status: "terminal", outcome: { status: "completed", result: settled } };
}, context);
```

【逐字】`Nested call ${call.id} is not in its caller's index`。

#### 2.11.15 `nestedResult(taskId, result, durationMs)`

```ts
const nested = {
  taskId, structuredOutput: result.structuredOutput,
  isError: result.isError ?? false,
  details: result.details,
  diagnostics: result.diagnostics ?? [],
  usage: result.usage,
  durationMs,
};
return copyJson(nested, { omitUndefinedProperties: true });
```

即：**不含 `output`**（只给模型看）、**不含 `control`**（只对 model 调用生效）。

#### 2.11.16 `startNestedCall(runtime, parentCallId, name, args, options, context)`

```ts
const call: ToolCall = { type: "toolCall", id: `${parentCallId}/${key}`, name,
                         arguments: copyJson(args, { omitUndefinedProperties: true }) };
let id; let replayed = false;
await runtime.commit(async (tx) => {
  const state = await tx.doc(NestedCallDoc, runtime.taskId, key, null);    // 打开或创建
  const existing = state.taskId;
  if (existing !== undefined) {
    const input = (await tx.task(existing))?.input as ToolTaskInput | undefined;
    const same = input?.kind === "nested" && input.call.name === name
                 && jsonEqual(input.call.arguments, call.arguments);
    if (!same) throw new Error(`Nested call ${call.id} was already made with another tool or other arguments`);
    id = existing; replayed = state.result !== undefined; return undefined;
  }
  const input: ToolTaskInput = { kind: "nested", parent: runtime.taskId, parentCallId, key, call,
                                 ...(options.progress === false ? { progress: false } : {}) };
  const created = await tx.createTask(ToolTask, input,
    { ownership: { kind: "task", taskId: runtime.taskId }, abandonOnRestart: options.abandonOnRestart });
  state.taskId = created;
  const live = await tx.doc(LiveDoc, runtime.conversationId);
  live.nestedTools ??= [];
  live.nestedTools.push({ callId: call.id, parentCallId, parentTaskId: runtime.taskId, name,
                          taskId: created, arguments: call.arguments, status: "pending" });
  id = created;
  return undefined;
}, context);
if (replayed) context.telemetry.addEvent("kiana.durable.replay", { "tool.name": name });
return id!;
```

【逐字】`Nested call ${call.id} was already made with another tool or other arguments`。

#### 2.11.17 `outputValue(output)` / `fallbackResult(taskId, name, outcome)`

```ts
function outputValue(output: Content) {
  if (output.length === 0) return "";
  if (output.length > 1) return output;
  const [only] = output;
  return only.type === "text" ? only.text : only;
}
```

`fallbackResult`【逐字】：
```ts
outcome.status === "faulted"  ? `Tool ${name} failed: ${outcome.error.message}`
: outcome.status === "orphaned" ? `Tool ${name} could not resume: ${outcome.reason}`
: `Tool ${name} ended without a result`;
return { taskId, isError: true, diagnostics: harnessError(outcome.status, message).diagnostics ?? [] };
```

#### 2.11.18 `Ending` / `COMPLETED` / `fromSlot` / `harnessError` / `toolDiagnostic`

```ts
type Ending = { readonly status: "completed" | "aborted" } | { readonly status: "failed"; readonly message: string };
const COMPLETED: Ending = { status: "completed" };

function fromSlot(slot, code, message) {
  const diagnostics = [...(slot?.diagnostics ?? [])];
  const droppedBytes = slot?.droppedBytes ?? 0;
  if (droppedBytes > 0) diagnostics.push(truncated({ droppedBytes, droppedLines: slot?.droppedLines ?? 0 }));
  diagnostics.push(toolDiagnostic(code, message));
  return {
    output: slot?.output === undefined || slot.output === "" ? [] : [{ type: "text", text: slot.output }],
    isError: true,
    ...(slot?.details === undefined ? {} : { details: slot.details }),
    diagnostics,
  };
}
export function harnessError(code, message) { return { output: [], isError: true, diagnostics: [toolDiagnostic(code, message)] }; }
function toolDiagnostic(code, message) { return { severity: "error", code, message }; }
```

#### 2.11.19 `truncated(dropped, retain?)`

```ts
const kept = retain === undefined ? "" : ` to its ${retain === "head" ? "beginning" : "end"}`;
return { severity: "warn", code: "truncated",
  message: `Output truncated${kept}: ${dropped.droppedLines} lines, ${dropped.droppedBytes} bytes dropped` };
```

#### 2.11.20 `summaryOf(result, output)` / `errorTextOf(result, output)`

```ts
const MAX_SUMMARY_ERROR_CHARS = 500;
function summaryOf(result, output) {
  const error = result.isError ? ownedSlice(errorTextOf(result, output), 0, 500) : "";
  return { isError: result.isError,
           ...(result.durationMs === undefined ? {} : { durationMs: result.durationMs }),
           ...(result.usage === undefined ? {} : { usage: result.usage }),
           ...(error === "" ? {} : { error }) };
}
function errorTextOf(result, output) {
  const errors = result.diagnostics.filter(d => d.severity === "error");
  if (errors.length > 0) return errors.map(d => d.message).join("\n");
  return output.flatMap(item => item.type === "text" ? [item.text] : []).join("\n");
}
```

`ownedSlice` 会拷贝，避免槽位通过切片持有大输出。

#### 2.11.21 `appendToolResult(tx, conversationId, call, result, meta)`

```ts
const diagnostics = [...(result.diagnostics ?? [])];
const content: Content = [...(result.output ?? [])];
if (diagnostics.length > 0) content.push({ type: "text", text: renderDiagnostics(diagnostics) });
const message = {
  role: "toolResult", toolCallId: call.id, toolName: call.name, content,
  ...(result.details === undefined ? {} : { details: result.details }),
  ...(result.usage === undefined ? {} : { usage: result.usage }),
  isError: result.isError ?? false,
  ...(meta.durationMs === undefined ? {} : { durationMs: meta.durationMs }),
  timestamp: meta.timestamp,
};
if (result.usage !== undefined) await recordUsage(tx, conversationId, "tools", call.name, result.usage);
return tx.appendEntry(ToolResultEntry, conversationId, { model: [message], data: { diagnostics } });
```

`renderDiagnostics`【逐字】：
```
<harness>
[${severity}] ${message}
...
</harness>
```
即 `` `<harness>\n${diagnostics.map(d => `[${d.severity}] ${d.message}`).join("\n")}\n</harness>` ``。

#### 2.11.22 `boundContent(content, limits)`

```ts
const texts = content.filter(item => item.type === "text");
const bounded = boundOutput(texts.map(item => item.text).join(""), limits);
if (bounded.droppedBytes === 0) return { content, droppedBytes: 0, droppedLines: 0 };
const keep = limits.retain === "head" ? texts[0] : texts.at(-1);
const result: Content = [];
for (const item of content) {
  if (item.type !== "text") result.push(item);
  else if (item === keep) result.push({ ...item, text: bounded.text });
}
return { content: result, droppedBytes: bounded.droppedBytes, droppedLines: bounded.droppedLines };
```

#### 2.11.23 `checkKey(key)`【逐字】

```ts
if (key === "" || key.includes("/") || key === "__proto__" || /^[1-9][0-9]*$/.test(key)) {
  throw new Error(`Nested call key ${JSON.stringify(key)} must be non-empty, without "/", not "__proto__", and not a positive integer`);
}
```

`JSON.stringify(key)` 会加双引号并转义（如 `"a b"`、`"a\"b"`）。

#### 2.11.24 `jsonEqual(a, b)`

结构相等：`a === b` 真；非对象/空 → false；数组需长度相同且逐项相等；对象需键数相同且每个键在 b 中存在并相等（**键顺序无关**）。

### 2.12 `src/harness/compaction.ts`（483 行，10 个导出）

导出：`CompactionInput`、`SummaryRequest`、`CompactionCheckpoint`、`CompactionTask`、`createCompaction`、`compactionPolicy`、`selectCut`、`summarizedMessages`、`estimateContext`、`serializeConversation`（内部：`TOOL_RESULT_MAX_CHARS`、`SUMMARY_PREFIX`、`SUMMARY_SUFFIX`、`SUMMARIZATION_SYSTEM_PROMPT`、`SUMMARIZATION_PROMPT`、`isCandidate`、`summaryText`、`summaryFailure`、`summaryPrompt`、`contentText`、`truncate`、`place`、`placeSummary`、`complete`、`failNoModel`）。

#### 2.12.1 常量与类型【全部逐字】

```ts
export type CompactionInput = { reason: CompactionReason; instructions?: string };
export type SummaryRequest = {
  attempt: number;
  model: ModelRef;
  thinkingLevel: ModelThinkingLevel;
  streamOptions: ConversationStreamOptions;
  maxTokens: number;
  tail: EntryId;          // 选中范围的上下文的最新条目
  firstKept: EntryId;     // 逐字保留的第一条；摘要条目的 head
};
export type CompactionCheckpoint =
  | { phase: "select" }
  | ({ phase: "summarize" } & SummaryRequest)
  | ({ phase: "retry"; until: number } & SummaryRequest);

const TOOL_RESULT_MAX_CHARS = 2000;

const SUMMARY_PREFIX = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const SUMMARY_SUFFIX = "\n</summary>";
```

`SUMMARIZATION_SYSTEM_PROMPT`【逐字】（两段，中间一个空行）：
```
You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.
```

`SUMMARIZATION_PROMPT`【逐字】（Markdown 模板，必须逐字节一致；行间空行与 `- [x]` / `- [ ]` 都是内容）：
```
The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work. If the conversation starts with an earlier summary, preserve its information and fold the newer messages into it.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.
```

#### 2.12.2 `CompactionTask`

```ts
export const CompactionTask = defineTask<...>({
  name: "kiana.compaction",
  version: 1,
  initial: () => ({ phase: "select" }),
  phases: { select, summarize, retry },
  abort,
});
```

无 `migrate`。

#### 2.12.3 `select(task, runtime, context)`

1. `agent = await runtime.agent(context)`；`settings = runtime.settings`；`ref = agent.model`；
   `model = ref === undefined ? undefined : runtime.models.getModel(ref.provider, ref.modelId)`；任一 `undefined` → `failNoModel(runtime, ref, context)`。
2. `policy = compactionPolicy(settings, ref)`；`view = await runtime.context(conversationId, context)`；`cut = selectCut(view, policy.keepRecentTokens)`；`undefined` → `complete(runtime, context)`。
3. `firstKept = view.entries[cut].id`。
4. `{ reason, instructions } = task.input`；
   `compaction = { reason, entries: view.entries.slice(0, cut), messages: summarizedMessages(view, cut), firstKept, ...(instructions === undefined ? {} : { instructions }) }`。
5. `beforeCompact` 钩子链：`if (decision === undefined) decision = await hook(compaction, runtime, context)`（**第一个决定胜出**）。
6. `decision` 有 `decline` → `complete`；有 `summary` → `place(runtime, firstKept, decision.summary, context)`。
7. `request: SummaryRequest = { attempt: 1, model: ref, thinkingLevel: agent.thinkingLevel, streamOptions: settings.stream, maxTokens: Math.min(Math.floor(0.8 * policy.reserveTokens), model.maxTokens > 0 ? model.maxTokens : Number.POSITIVE_INFINITY), tail: view.entries.reduce((tail, entry) => entry.id > tail ? entry.id : tail, firstKept), firstKept }`。
8. `runtime.commit(() => ({ status: "running", checkpoint: { phase: "summarize", ...request } }), context)`。

#### 2.12.4 `summarize(task, runtime, context)`

1. 解构 `{ phase: _, ...request } = task.state.checkpoint`；取 `{ model: ref, thinkingLevel, streamOptions, maxTokens, tail, firstKept, attempt }`。
2. `model = getModel(...)`；`undefined` → `failNoModel`。
3. `view = await runtime.context(conversationId, context, { at: tail })`（tail 之前不可变，故范围与 select 时相同）；`cut = view.entries.findIndex(entry => entry.id === firstKept)`；`now = runtime.now()`。
4. 消息：
   ```ts
   const messages: Message[] = [
     { role: "system", content: SUMMARIZATION_SYSTEM_PROMPT, timestamp: now },
     { role: "user", content: [{ type: "text", text: summaryPrompt(summarizedMessages(view, cut), task.input.instructions) }], timestamp: now },
   ];
   ```
5. 选项：
   ```ts
   const { deferred: _deferred, ...forwarded } = streamOptions;      // 去掉 deferred
   const options: SimpleStreamOptions = {
     ...forwarded, cacheRetention: "none", maxTokens,
     signal: runtime.signal, telemetry: context.telemetry,
     sessionId: await ensureProviderSessionId(runtime, context),
     ...(thinkingLevel === "off" ? {} : { reasoning: thinkingLevel }),
   };
   ```
6. `message = await runtime.models.completeSimple(model, { messages }, options)`。
7. `runtime.signal.throwIfAborted()`。
8. `summary = summaryText(message)`；`policy = settings.retry`；
   `retry = message.stopReason === "error" && isRetryableAssistantError(message) && policy.enabled && attempt <= policy.maxRetries`；
   `until = retry ? runtime.now() + retryDelayMs(policy, attempt) : 0`。
9. 提交（`tx, current`）：
   - `recordUsage(tx, conversationId, "models", `${message.provider}/${message.model}`, message.usage)`；
   - `live = await tx.doc(LiveDoc, ...)`；
   - `summary !== undefined` → `placeSummary(tx, runtime, current, live, firstKept, summary)`；
   - 否则 `retry` → `status = compactionStatus(live, taskId)`；若存在 `status.retry = { at: until, error: message.errorMessage ?? "" }`；checkpoint `{ phase: "retry", ...request, until }`；返回 `{ status: "sleeping", checkpoint, until }`；
   - 否则 `text = summaryFailure(message)`；`failCompactionStatus(live, taskId, text)`；返回 `{ status: "terminal", outcome: { status: "failed", error: { message: text, detail: { reason: "model_error" } } } }`。

#### 2.12.5 `retry(task, runtime, context)`

```ts
const { phase: _, until, ...request } = task.state.checkpoint;
if (until > runtime.now()) return runtime.sleepUntil(until, task.state.checkpoint, context);
const attempt = request.attempt + 1;
await runtime.commit(async (tx) => {
  const status = compactionStatus(await tx.doc(LiveDoc, runtime.conversationId), runtime.taskId);
  if (status !== undefined) { status.attempt = attempt; delete status.retry; }
  return { status: "running", checkpoint: { phase: "summarize", ...request, attempt } };
}, context);
```

#### 2.12.6 `abort`

```ts
await runtime.commit(async (tx) => {
  removeCompactionStatus(await tx.doc(LiveDoc, runtime.conversationId), runtime.taskId);
  return { status: "terminal", outcome: { status: "aborted" } };
}, context);
```
（aborted 时**保留** `live.compactionFailure`——只有产出摘要才删除。）

#### 2.12.7 `createCompaction(tx, conversationId, input, owner?)`

```ts
const ownership = owner === undefined ? { kind: "conversation" } : { kind: "task", taskId: owner };
const background = owner === undefined && input.reason !== "manual";
const taskId = await tx.createTask(CompactionTask, input, { ownership, conversationId, background });
const status = { taskId, reason: input.reason, blocking: owner !== undefined, attempt: 1 };
addCompactionStatus(await tx.doc(LiveDoc, conversationId), status);
return taskId;
```

- owner 存在（generation 拥有）→ **blocking**；
- 无 owner → conversation-owned，且**手工压缩不是 background**，threshold/overflow 是 background。

#### 2.12.8 `compactionPolicy(settings, model)` → `CompactionPolicy`

```ts
const { modelOverrides, ...policy } = settings.compaction;
const override = modelOverrides?.[`${model.provider}/${model.modelId}`];
return {
  enabled: override?.enabled ?? policy.enabled,
  reserveTokens: override?.reserveTokens ?? policy.reserveTokens,
  keepRecentTokens: override?.keepRecentTokens ?? policy.keepRecentTokens,
  backgroundTokens: override?.backgroundTokens ?? policy.backgroundTokens,
};
```

#### 2.12.9 `selectCut(view, keepRecentTokens)`【核心算法】

```ts
const { contributions } = view;
const start = view.head === undefined ? 0 : 1;          // 有 head marker 时索引 0 是 marker，不能作为切点
const candidates: number[] = [];
for (let index = start; index < contributions.length; index++) if (isCandidate(contributions, index)) candidates.push(index);
let kept = 0;
let cut: number | undefined;
for (let index = contributions.length - 1; index >= start; index--) {
  for (const message of contributions[index]) kept += estimateMessageTokens(message);
  if (kept < keepRecentTokens) continue;
  cut = candidates.find(candidate => candidate >= index) ?? candidates.at(-1);
  break;
}
if (cut === undefined) return undefined;
for (let index = start; index < cut; index++) if (contributions[index].length > 0) return cut;
return undefined;
```

- 从尾部往前累计 token，达到 `keepRecentTokens` 后，切在**第一个 ≥ 该位置的候选条目**；没有则取最后一个候选；
- 最后再要求 `cut` 之前**至少有一个非空贡献**，否则没有可压缩内容 → `undefined`。

#### 2.12.10 `isCandidate(contributions, index)`

```ts
const first = contributions[index][0];
if (first?.role === "assistant") return true;
if (first?.role !== "user") return false;
let calls = new Set();
for (let before = index - 1; before >= 0; before--) {
  const assistant = contributions[before].findLast(message => message.role === "assistant");
  if (assistant === undefined) continue;
  calls = new Set(assistant.content.flatMap(c => c.type === "toolCall" ? [c.id] : []));
  break;
}
if (calls.size === 0) return true;
for (let after = index; after < contributions.length; after++) {
  for (const [position, message] of contributions[after].entries()) {
    if (message.role === "assistant" && (after > index || position > 0)) return true;
    if (message.role === "toolResult" && calls.has(message.toolCallId)) return false;
  }
}
return true;
```

规则：**首个贡献消息必须是 user 或 assistant**；**永远不能是 tool result**；user 条目若其后续（下一条 assistant 之前）跟着"前一条 assistant 调用的结果"，则不是候选（会把调用与结果切开）。注意 `entries()` 给出 `[position, message]`，`after > index || position > 0` 处理同一数组内"index 之后的 assistant"。

#### 2.12.11 `summarizedMessages(view, cut)`

`orderToolResults(view.contributions.slice(0, cut).flat())`（head marker 贡献在最前；按模型上下文顺序整理工具结果）。

#### 2.12.12 `estimateContext(view, extra)` → token 数

```ts
let measured: AssistantMessage | undefined;
const after = view.head?.id ?? Number.NEGATIVE_INFINITY;
for (let index = view.entries.length - 1; index >= 0 && measured === undefined; index--) {
  if (view.entries[index].id <= after) continue;
  measured = view.contributions[index].findLast(m => m.role === "assistant" && calculateContextTokens(m.usage) > 0);
}
const from = measured === undefined ? 0 : view.messages.lastIndexOf(measured) + 1;
let tokens = measured === undefined ? 0 : calculateContextTokens(measured.usage);
for (const message of view.messages.slice(from)) tokens += estimateMessageTokens(message);
for (const message of extra) tokens += estimateMessageTokens(message);
return tokens;
```

- 用"head marker 之后最新的、usage 计数 > 0 的 assistant"的 `calculateContextTokens` 作为已测基线；
- 加上 `view.messages` 中**该消息之后**的消息估算，再加 `extra`。

#### 2.12.13 `summaryText(message)` / `summaryFailure(message)`

```ts
function summaryText(message) {
  if (message.stopReason !== "stop" || message.content.some(c => c.type === "toolCall")) return undefined;
  const text = message.content.flatMap(c => c.type === "text" ? [c.text] : []).join("\n").trim();
  return text.length === 0 ? undefined : text;
}
function summaryFailure(message) {
  if (message.stopReason === "error" || message.stopReason === "aborted")
    return `Summarization failed: ${message.errorMessage ?? message.stopReason}`;
  if (message.stopReason === "length") return "Summarization hit the token limit; the summary is incomplete";
  if (message.content.some(c => c.type === "toolCall")) return "Summarization attempted to call a tool";
  return "Summarization produced no text";
}
```

#### 2.12.14 `summaryPrompt(messages, instructions)`

```ts
const focus = instructions === undefined ? "" : `\n\nAdditional focus: ${instructions}`;
return `<conversation>\n${serializeConversation(messages)}\n</conversation>\n\n${SUMMARIZATION_PROMPT}${focus}`;
```

#### 2.12.15 `serializeConversation(messages)`【逐字格式】

```ts
const parts: string[] = [];
for (const message of messages) {
  if (message.role === "user") {
    const text = contentText(message.content);
    if (text.length > 0) parts.push(`[User]: ${text}`);
  } else if (message.role === "assistant") {
    const text = message.content.flatMap(c => c.type === "text" ? [c.text] : []);
    const calls = message.content.flatMap(c => c.type === "toolCall"
      ? [`${c.name}(${Object.entries(c.arguments).map(([key, value]) => `${key}=${JSON.stringify(value)}`).join(", ")})`]
      : []);
    if (text.length > 0) parts.push(`[Assistant]: ${text.join("\n")}`);
    if (calls.length > 0) parts.push(`[Assistant tool calls]: ${calls.join("; ")}`);
  } else if (message.role === "toolResult") {
    const text = contentText(message.content);
    if (text.length > 0) parts.push(`[Tool result]: ${truncate(text, TOOL_RESULT_MAX_CHARS)}`);
  }
}
return parts.join("\n\n");
```

- 前缀【逐字】：`"[User]: "`、`"[Assistant]: "`、`"[Assistant tool calls]: "`、`"[Tool result]: "`；
- assistant 文本多块用 `"\n"` 连接；工具调用参数按 `key=JSON.stringify(value)` 逗号+空格连接，多个调用用 `"; "` 连接；
- **system 消息被省略**；thinking 也被省略（源码注释给出理由：provider 返回的是推理摘要，重放的 thinking 常不在活动上下文中，包含它会让请求大于被摘要的上下文）；
- 空文本的 user / tool result 不产生部分；
- 部分之间用 `"\n\n"` 连接。

`contentText(content)`：字符串原样返回；块数组取 `type === "text" && text !== undefined` 的文本，`"\n"` 连接。

`truncate(text, maxChars)`：
```ts
if (text.length <= maxChars) return text;
return `${text.slice(0, maxChars)}\n\n[... ${text.length - maxChars} more characters truncated]`;
```
【逐字】`"\n\n[... ${n} more characters truncated]"`。

#### 2.12.16 `place(runtime, firstKept, summary, context)`

```ts
await runtime.commit(async (tx, current) =>
  placeSummary(tx, runtime, current, await tx.doc(LiveDoc, runtime.conversationId), firstKept, summary), context);
```

#### 2.12.17 `placeSummary(tx, runtime, current, live, firstKept, summary)`

```ts
removeCompactionStatus(live, runtime.taskId);
delete live.compactionFailure;
const text = `${SUMMARY_PREFIX}${summary}${SUMMARY_SUFFIX}`;
const entry = {
  kind: CompactionEntry.kind,                       // "kiana.compaction"
  head: firstKept,
  model: [{ role: "user", content: [{ type: "text", text }], timestamp: runtime.now() }],
  data: { reason: current.input.reason },
} satisfies EntryDraft;
const result: CompactionResult =
  current.owner === undefined
    ? { submissionId: (await admitSubmission(tx, runtime.conversationId,
        { type: "write", requestId: `compaction:${runtime.taskId}`, entry }, runtime.now(), runtime.settings)).id }
    : { entryId: (await tx.appendEntry(runtime.conversationId, entry)).id };
return { status: "terminal", outcome: { status: "completed", result } };
```

- **blocking**（有 owner）→ 直接 `appendEntry`，结果 `{ entryId }`；
- **conversation-owned** → 通过 `admitSubmission` 以写提交放置，`requestId = `compaction:${taskId}``（幂等去重）；空闲时立即放置，忙时排队到下一个边界或 settle `stale`；结果 `{ submissionId }`；
- 摘要条目 `data.reason` 来自任务输入（`manual`/`threshold`/`overflow`）；用户消息文本是 `SUMMARY_PREFIX + summary + SUMMARY_SUFFIX`；
- **删除 `live.compactionFailure`**（成功产出摘要）。

> 源码 REMINDER：忙会话上除 Harness 外无人能追加条目，所以所有非 blocking 摘要都必须走准入。

#### 2.12.18 `complete(runtime, context)` / `failNoModel(runtime, ref, context)`

```ts
// complete
await runtime.commit(async (tx) => {
  removeCompactionStatus(await tx.doc(LiveDoc, runtime.conversationId), runtime.taskId);
  return { status: "terminal", outcome: { status: "completed", result: {} } };
}, context);

// failNoModel
const message = ref === undefined ? "No model is configured" : `Model ${ref.provider}/${ref.modelId} is not available`;
await runtime.commit(async (tx) => {
  failCompactionStatus(await tx.doc(LiveDoc, runtime.conversationId), runtime.taskId, message);
  return { status: "terminal", outcome: { status: "failed", error: { message, detail: { reason: "no_model" } } } };
}, context);
```

### 2.13 `src/harness/events.ts`（505 行，6 个导出）

导出：`MessageChange`、`SnapshotEvent`、`AgentEvent`、`ToolEventCall`、`AgentEventStream`、`watchEvents`（内部：`Block`、`QueuedItem`、`Parts`、`parts`、`snapshotOf`、`queued`、`resultOf`、`translate`、`PARTIAL_PATH`、`messageChanges`、`callOf`、`toolUpdate`、`startsWith`）。

#### 2.13.1 类型【字段逐字】

```ts
type Block = AssistantMessage["content"][number];
type QueuedItem = { id: SubmissionId; mode: InboxItem["mode"] };

export type MessageChange =
  | { type: "text_start" | "thinking_start" | "toolcall_start"; contentIndex: number; block: Block }
  | { type: "text_delta" | "thinking_delta"; contentIndex: number; delta: string }
  | { type: "toolcall_delta"; contentIndex: number; path: readonly (string | number)[]; delta: string }
  | { type: "block"; contentIndex: number; block: Block }
  | { type: "message"; message: AssistantMessage };
```

`SnapshotEvent`：
```ts
export type SnapshotEvent = {
  type: "snapshot";
  entries: readonly EntryRecord[];
  run?: { inputs: readonly SubmissionId[] };
  generation?: { attempt: number; message?: AssistantMessage; retry?: { at: number; error: string }; deferred?: { pollAt: number } };
  tools: readonly ToolSlot[];
  nestedTools: readonly NestedToolSlot[];
  compactions: readonly CompactionStatus[];
  compactionFailure?: CompactionFailure;
  inbox: readonly QueuedItem[];
  agent: AgentState;
  usage: UsageState;
};
```

`AgentEvent` 联合（**共 22 种 `type`**，顺序即文档中的列举顺序）：
`snapshot`、`run_start`、`run_end`、`turn_start`、`turn_end`、`message_start`、`message_update`、`message_end`、`tool_execution_start`、`tool_execution_update`、`tool_execution_end`、`inbox_update`、`submission`、`auto_retry_start`、`auto_retry_end`、`deferred_poll`、`entry_appended`、`agent_changed`、`usage_changed`、`task_failed`、`compaction_start`、`compaction_end`。

```ts
| { type: "run_start"; inputs: readonly SubmissionId[] }
| { type: "run_end"; inputs: readonly SubmissionId[] }
| { type: "turn_start" }
| { type: "turn_end" }
| { type: "message_start"; message: Message }
| { type: "message_update"; usage: Usage; changes: readonly MessageChange[] }
| { type: "message_end"; entry: EntryRecord }
| ({ type: "tool_execution_start"; args: JsonObject } & ToolEventCall)
| ({ type: "tool_execution_update";
     output?: { trimStart?: number; append?: string } | { set: string };
     details?: JsonValue; diagnostics?: readonly ToolDiagnostic[] } & ToolEventCall)
| ({ type: "tool_execution_end"; entry?: EntryRecord; result?: NestedToolExecutionResult } & ToolEventCall)
| { type: "inbox_update"; items: readonly QueuedItem[] }
| { type: "submission"; record: SubmissionRecord }
| { type: "auto_retry_start"; attempt: number; at: number; errorMessage: string }
| { type: "auto_retry_end"; attempt: number }
| { type: "deferred_poll"; pollAt: number }
| { type: "entry_appended"; entry: EntryRecord }
| { type: "agent_changed"; agent: AgentState }
| { type: "usage_changed"; usage: UsageState }
| { type: "task_failed"; taskId: TaskId; kind: string; message: string }
| { type: "compaction_start"; taskId: TaskId; reason: CompactionReason; blocking: boolean }
| { type: "compaction_end"; taskId: TaskId; reason: CompactionReason; error?: string };
```

```ts
export type ToolEventCall = {
  toolCallId: string;        // provider ID（model 调用）或 `<parent call ID>/<key>`（nested）；按它匹配事件，不要解析
  toolName: string;
  taskId?: TaskId;           // 该调用的工具任务；从未获得任务的调用（未提供、顺序轮未开始）没有
  parentToolCallId?: string;
  parentTaskId?: TaskId;
};
export interface AgentEventStream {
  readonly snapshot: SnapshotEvent;
  start(listener: (events: readonly AgentEvent[], context: Context) => Promise<void>): void;
  stop(): Promise<WatchEnd>;
  readonly closed: Promise<WatchEnd>;
}
```

#### 2.13.2 `parts(view)` / `snapshotOf(view)` / `queued(inbox)`

- `parts`：`live = view.docs["kiana.live"] ?? {}`、`inbox`、`agent`、`usage`（后三者可为 undefined）。
- `snapshotOf`：`{ type: "snapshot", entries: view.entries, ...(live.run ? { run: { inputs } } : {}), ...(live.generation ? { generation } : {}), tools: live.tools ?? [], nestedTools: live.nestedTools ?? [], compactions: live.compactions ?? [], ...(compactionFailure?), inbox: queued(inbox), agent: agent ?? AgentDoc.definition.initial(), usage: usage ?? UsageDoc.definition.initial() }`。
- `queued`：`(inbox?.items ?? []).map(item => ({ id: item.id, mode: item.mode }))`（**只暴露 id 与 mode**，不含内容）。

#### 2.13.3 `watchEvents(harness, conversationId, context)`

```ts
const views = conversationViews(harness);
let watch!: NativeWatch<readonly AgentEvent[]>;
let snapshot!: SnapshotEvent;
await views.attach(conversationId, async (initial, release, storage) => {
  const query = { conversationId, kind: "kiana.generation", status: "completing" } as const;
  const completing = await scanAll(cursor => storage.scanTasks(query, 100, cursor, context));   // 页大小 100
  const held = new Set<TaskId>(completing.map(record => record.id));
  let current = initial;
  snapshot = snapshotOf(initial);
  watch = new NativeWatch<readonly AgentEvent[]>([], release, error => views.report(error),
            () => [snapshotOf(current)]);                       // 溢出时以最新视图快照替换未投递批次
  return {
    publication: (before, after, ops, publication, commitContext) => {
      current = after;
      const events = translate(conversationId, before, after, ops, publication, held);
      if (events.length > 0) watch.advance(events, commitContext);
    },
    end: failure => watch.end(failure),
  };
}, context);
const signal = context.abortSignal;
if (signal?.aborted) { watch.cancel(); throw signal.reason; }
if (signal !== undefined) watch.observeCancellation(signal);
return { snapshot, start: listener => watch.start(listener), stop: () => watch.stop(), closed: watch.closed };
```

语义：
- 与普通 watch 一样，**获取时的 context 决定流的生命周期**；
- 每次提交产出一个"批次"（`readonly AgentEvent[]`）；`NativeWatch` 不做折叠（折叠会丢事件）；
- 溢出（Chord 的待投递批次超过上限，默认 100）时投递一次 `[snapshotOf(current)]`；
- `held` = 打开时已处于 `completing` 的 `kiana.generation` 任务 id（其 outcome 已经结束过 turn，不重复发 `turn_end`）。

#### 2.13.4 `resultOf(entries, callId)`

在条目中找 `entry.model?.[0]` 为 `role === "toolResult" && toolCallId === callId` 的第一条。

#### 2.13.5 `translate(conversationId, before, after, viewOps, publication, held)`【事件顺序权威实现】

**收集阶段**：
```ts
const entries: EntryRecord[] = [];
const tasks = new Map<TaskId, TaskChange["value"]>();
const submissions: SubmissionRecord[] = [];
const nestedResults = new Map<TaskId, NestedToolExecutionResult>();
for (const change of publication.changes) {
  if (change.type === "document" && change.record.kind === NestedCallDoc.definition.kind
      && change.value !== null && change.conversationId === conversationId) {
    const state = change.value as NestedCallState;
    if (state.taskId !== undefined && state.result !== undefined) nestedResults.set(state.taskId, state.result);
  }
  if (change.type === "entry" && change.value.conversationId === conversationId) entries.push(change.value);
  if (change.type === "task" && change.value.conversationId === conversationId) tasks.set(change.value.id, change.value);
  if (change.type === "submission" && change.value.conversationId === conversationId) submissions.push(change.value);
}
if (viewOps.length === 0 && entries.length === 0 && tasks.size === 0 && submissions.length === 0) return [];
submissions.sort((a, b) => a.id - b.id);
const was = parts(before), now = parts(after);
const events: AgentEvent[] = [];
```

**① 进度事件**（tool starts → message → tool updates → retry/deferred）：
1. **tool_execution_start**：对 `[...slots, ...nested]` 每个槽，`previous = "parentCallId" in slot ? nestedBefore.get(slot.taskId) : slotsBefore.get(slot.callId)`；仅当 `slot.status === "running" && previous?.status !== "running"`：
   - `checkpoint = slot.taskId === undefined ? undefined : tasks.get(slot.taskId)?.state.checkpoint`；
   - `args = "parentCallId" in slot ? slot.arguments : (checkpoint?.arguments ?? {})`；
   - `events.push({ type: "tool_execution_start", ...callOf(slot), args })`。
2. **message_start / message_update**：
   ```ts
   const partialBefore = was.live.generation?.message;
   const partial = now.live.generation?.message;
   if (partial !== undefined && partialBefore === undefined) events.push({ type: "message_start", message: partial });
   else if (partial !== undefined && partial !== partialBefore)
     events.push({ type: "message_update", usage: partial.usage, changes: messageChanges(viewOps, partial) });
   ```
3. **tool_execution_update（tools）**：对 `slots.entries()`，仅当 `slot.status === "running" && previous?.status === "running"`，`update = toolUpdate(viewOps, ["tools", index], slot, previous)` 非 undefined 时 push。
4. **tool_execution_update（nestedTools）**：同上，路径 `["nestedTools", index]`。
5. **auto_retry_start**：`generation?.retry !== undefined && generationBefore?.retry === undefined` → `{ type: "auto_retry_start", attempt: generation.attempt, at, errorMessage: error }`。
6. **auto_retry_end**：`generationBefore?.retry !== undefined && generation?.retry === undefined` → `{ type: "auto_retry_end", attempt: generationBefore.attempt }`。
7. **deferred_poll**：`generation?.deferred !== undefined && generation.deferred.pollAt !== generationBefore?.deferred?.pollAt` → `{ type: "deferred_poll", pollAt }`。

**② 工具结束（先收集，稍后按条目顺序插入）**：
- `endTool(slot, entryId)`：`entry = entries.find(e => e.id === entryId)`；push `{ type: "tool_execution_end", ...callOf(slot), ...(entry === undefined ? {} : { entry }) }`。
- 遍历 `slotsBefore.values()`：跳过 `status === "done"`；`slot = slots.find(s => s.callId === previous.callId)`；
  - `slot?.status === "done"` → `endTool(slot, slot.entry)`；
  - `slot === undefined` → `endTool(previous, resultOf(entries, previous.callId)?.id)`（run 在本提交结束，结果可能同提交追加）。
- 再遍历 `slots`：`status === "done" && !slotsBefore.has(callId)` → `endTool(slot, slot.entry)`（本提交创建即完成，例如未提供的调用）。
- `endNested(slot)`：`result = nestedResults.get(slot.taskId)`；push `{ type: "tool_execution_end", ...callOf(slot), ...(result === undefined ? {} : { result }) }`。
- 遍历 `[...nestedBefore.values()].reverse()`：跳过 done；`slot = nestedNow.get(taskId)`；`slot?.status === "done" || slot === undefined` → `endNested(slot ?? previous)`（**子先于父**：列表父在前，故倒序）。
- 再遍历 `[...nested].reverse()`：`status === "done" && !nestedBefore.has(taskId)` → `endNested(slot)`。

**③ 条目**：
```ts
let assistantAppended = false;
for (const entry of entries) {
  events.push(...toolEnds.filter(end => end.entry === entry));     // 工具结束紧邻其结果消息之前
  const message = entry.model?.[0];
  if (message === undefined) { events.push({ type: "entry_appended", entry }); continue; }
  const streamed = message.role === "assistant" && partialBefore !== undefined && !assistantAppended;
  if (message.role === "assistant") assistantAppended = true;
  if (!streamed) events.push({ type: "message_start", message });
  events.push({ type: "message_end", entry });
}
events.push(...toolEnds.filter(end => end.entry === undefined));    // 无结果条目的结束（faulted/orphaned/run 结束）
```

**④ 压缩结束 / 任务失败 / turn 与 run 结束**：
```ts
const compactionsBefore = was.live.compactions ?? [];
const compactions = now.live.compactions ?? [];
const failure = now.live.compactionFailure;
for (const { taskId, reason } of compactionsBefore) {
  if (!compactions.some(s => s.taskId === taskId)) {
    const error = failure?.taskId === taskId ? failure.error : undefined;
    events.push({ type: "compaction_end", taskId, reason, ...(error === undefined ? {} : { error }) });
  }
}
let turnEnded = false;
for (const task of tasks.values()) {
  const status = task.state.status;
  if (task.kind === "kiana.generation" && status === "completing" && !held.has(task.id)) { held.add(task.id); turnEnded = true; }
  if (status !== "terminal") continue;
  if (task.kind === "kiana.generation" && !held.delete(task.id)) turnEnded = true;
  const outcome = task.state.outcome;
  if (outcome.status === "faulted" || outcome.status === "orphaned") {
    const message = outcome.status === "faulted" ? outcome.error.message : outcome.reason;
    events.push({ type: "task_failed", taskId: task.id, kind: task.kind, message });
  }
}
if (turnEnded) events.push({ type: "turn_end" });
const run = now.live.run, runBefore = was.live.run;
const runChanged = run?.inputs[0] !== runBefore?.inputs[0];
if (runBefore !== undefined && runChanged) events.push({ type: "run_end", inputs: runBefore.inputs });
```

**⑤ 提交与文档，最后是"开始"的事件**：
```ts
for (const record of submissions) events.push({ type: "submission", record });
if (now.inbox !== was.inbox) events.push({ type: "inbox_update", items: queued(now.inbox) });
if (now.agent !== was.agent) events.push({ type: "agent_changed", agent: now.agent ?? AgentDoc.definition.initial() });
if (now.usage !== was.usage) events.push({ type: "usage_changed", usage: now.usage ?? UsageDoc.definition.initial() });
for (const { taskId, reason, blocking } of compactions) {
  if (!compactionsBefore.some(s => s.taskId === taskId)) events.push({ type: "compaction_start", taskId, reason, blocking });
}
if (run !== undefined && runChanged) events.push({ type: "run_start", inputs: run.inputs });
if (run !== undefined && run.taskId !== runBefore?.taskId && tasks.get(run.taskId)?.kind === "kiana.generation") {
  events.push({ type: "turn_start" });
}
return events;
```

**顺序不变量（必须逐条测试）**：
1. 进度事件在前（starts → message → updates → retry → poll）；
2. 每个工具结束**紧邻**其结果条目的 `message_start` 之前（`toolEnds` 在条目循环里按 `end.entry === entry` 插入）；
3. 流式答案的 `message_start` 已在第一个 partial 时发过，因此条目循环里**不再发**（`partialBefore !== undefined && !assistantAppended`），只发 `message_end`；
4. 无条目的工具结束在所有条目之后；
5. `compaction_end` 先于 `turn_end`；`turn_end` 先于 `run_end`；
6. `submission` → `inbox_update` → `agent_changed` → `usage_changed` → `compaction_start` → `run_start` → `turn_start`；
7. `run_start`/`run_end` 以 `run.inputs[0]` 是否变化判定"换 run"；`turn_start` 还要求 `run.taskId` 变化且新 task 是 `kiana.generation`。

#### 2.13.6 `PARTIAL_PATH` 与 `messageChanges(viewOps, message)`

```ts
const PARTIAL_PATH = ["docs", "kiana.live", "generation", "message"];
```

```ts
const changes: MessageChange[] = [];
const whole = new Set<number>();
for (const op of viewOps) {
  const path = op[1];
  if (!startsWith(path, PARTIAL_PATH)) {
    if (startsWith(PARTIAL_PATH, path)) return [{ type: "message", message }];   // 整条消息/整个 generation 被替换
    continue;
  }
  const rest = path.slice(PARTIAL_PATH.length);
  if (rest[0] === "usage") continue;
  if (rest[0] !== "content") return [{ type: "message", message }];
  if (rest.length === 1) {
    if (op[0] !== "p" || op[3] !== 0) return [{ type: "message", message }];
    for (const [offset, block] of op[4].entries()) {
      const type = block.type === "text" ? "text_start" : block.type === "thinking" ? "thinking_start" : "toolcall_start";
      changes.push({ type, contentIndex: op[2] + offset, block });
    }
    continue;
  }
  const contentIndex = rest[1];
  const field = rest[2];
  if (whole.has(contentIndex)) continue;
  if (op[0] === "a" && rest.length === 3 && (field === "text" || field === "thinking")) {
    changes.push({ type: field === "text" ? "text_delta" : "thinking_delta", contentIndex, delta: op[2] });
  } else if (op[0] === "a" && field === "arguments") {
    changes.push({ type: "toolcall_delta", contentIndex, path: rest.slice(3), delta: op[2] });
  } else {
    whole.add(contentIndex);
    changes.push({ type: "block", contentIndex, block: message.content[contentIndex] });
  }
}
return changes;
```

Chord `Op` 形状：`["r", path, value]`、`["p", path, index, deleteCount, items]`、`["m", path, from, to?]`、`["s", path, value]`、`["d", path]`、`["a", path, text]`、`["t", path, count]`。

#### 2.13.7 `callOf(slot)`

```ts
{
  toolCallId: slot.callId,
  toolName: slot.name,
  ...(slot.taskId === undefined ? {} : { taskId: slot.taskId }),
  ...("parentCallId" in slot ? { parentToolCallId: slot.parentCallId, parentTaskId: slot.parentTaskId } : {}),
}
```

#### 2.13.8 `toolUpdate(viewOps, at, slot, previous)`

```ts
const outputPath = ["docs", "kiana.live", ...at, "output"];
let trimStart = 0, append = "", set = false;
for (const op of viewOps) {
  if (!startsWith(op[1], outputPath)) continue;
  if (op[0] === "t") trimStart += op[2];
  else if (op[0] === "a") append += op[2];
  else set = true;
}
let output;
if (set || (slot.output !== previous.output && trimStart === 0 && append === "")) output = { set: slot.output ?? "" };
else if (trimStart > 0 || append !== "") output = { ...(trimStart > 0 ? { trimStart } : {}), ...(append === "" ? {} : { append }) };
const update = {
  ...(output === undefined ? {} : { output }),
  ...(slot.details === previous.details ? {} : { details: slot.details ?? null }),          // 删除 → null
  ...(slot.diagnostics === previous.diagnostics ? {} : { diagnostics: slot.diagnostics ?? [] }),  // 删除 → []
};
return Object.keys(update).length === 0 ? undefined : update;
```

语义：`t`（trim）累加、`a`（append）拼接、其余（`s`/`r`/`p`/`m`/`d`）视为整体 set；安全重放清空 running 槽时会以 `details: null` / `diagnostics: []` 表达删除。

#### 2.13.9 `startsWith(path, prefix)`

`prefix.length <= path.length && prefix.every((segment, index) => path[index] === segment)`。

### 2.14 `src/harness/view.ts`（250 行，4 个导出）

导出：`ConversationView`、`ViewObserver`、`conversationViews`、`ConversationViews`（内部：`MOUNTED`、`MOUNTED_KINDS`、`Mount`、`VIEWS`、`advance`、`prefixed`）。

#### 2.14.1 类型

```ts
export type ConversationView = {
  readonly conversation: ConversationRecord;
  readonly entries: readonly EntryRecord[];
  readonly docs: Readonly<Record<string, JsonObject>>;
};
export type ViewObserver = {
  advance?(value: ConversationView, ops: readonly Op[], context: Context): void;
  publication?(before: ConversationView, after: ConversationView, ops: readonly Op[], publication: ObservedCommit, context: Context): void;
  end(failure: { readonly error: unknown } | undefined): void;
};
```

#### 2.14.2 常量

```ts
const MOUNTED = [AgentDoc, LiveDoc, InboxDoc, ProviderDoc, UsageDoc];
const MOUNTED_KINDS = new Set(MOUNTED.map(token => token.definition.kind));
const VIEWS = new WeakMap<object, ConversationViews>();   // 按 session 索引
```

`conversationViews(harness)`：取不到抛【逐字】`Not a Harness`。

#### 2.14.3 `ConversationViews`

字段：`#session`、`#storage`、`#mounts: Map<ConversationId, Mount>`、`#closed`。
构造：`VIEWS.set(session, this)`；`session.observeCommits((publication, context) => { for (const [id, mount] of #mounts) advance(id, mount, publication, context, error => this.report(error)); })`；`session.subscribeClose(() => { #closed = true; 对每个 mount 的每个 observer 调 contained(() => observer.end(this.#session.failure), error => this.report(error)); #mounts.clear(); })`。

`report(error)`：`this.#session.report(error)`。

`watch(id, context)`：
```ts
const { observer } = await this.attach(id,
  (value, release) => new Watch<ConversationView>(value, release, error => this.#session.report(error)), context);
const signal = context.abortSignal;
if (signal?.aborted) { observer.cancel(); throw signal.reason; }
if (signal !== undefined) observer.observeCancellation(signal);
return observer;
```

`attach(id, create, context)`：
```ts
return this.#session.readOnLine(async () => {
  const mount = this.#mounts.get(id) ?? (await this.#build(id, context));
  const detach = () => { mount.observers.delete(observer); if (mount.observers.size === 0 && this.#mounts.get(id) === mount) this.#mounts.delete(id); };
  const observer = await create(mount.value, detach, this.#storage);
  if (this.#closed) throw closedError(this.#session);
  context.abortSignal?.throwIfAborted();
  this.#mounts.set(id, mount);
  mount.observers.add(observer);
  return { observer, detach };
});
```
（在线上原子：observer 只看到之后的发布，看不到之前的；`create` 可以仍在线上读已提交 Storage。）

`#build(id, context)`：
```ts
const conversation = await this.#storage.conversation(id, context);
if (conversation === undefined) throw new Error(`Conversation ${id} does not exist`);
const bounds = await captureContextBounds(this.#storage, id, context);
const entries = await activeEntries(this.#storage, id, bounds, context);
const docs = {}; const incarnations = new Map();
for (const token of MOUNTED) {
  const loaded = await this.#session.conversationDocumentOnLine(token, id, context);
  if (loaded === undefined) continue;
  docs[token.definition.kind] = loaded.value;
  incarnations.set(token.definition.kind, { id: loaded.record.id, version: loaded.version });
}
return { value: { conversation, entries, docs }, docs: incarnations, observers: new Set() };
```

#### 2.14.4 `advance(id, mount, publication, context, report)`

```ts
const docOps: Op[] = [];
const entryOps: Op[] = [];
let entries = mount.value.entries;
for (const change of publication.changes) {
  if (change.type === "entry" && change.value.conversationId === id) {
    const entry = change.value;
    if (entry.head === undefined) {
      entryOps.push(["p", ["entries"], entries.length, 0, [entry]]);
      entries = [...entries, entry];
      continue;
    }
    const target = entry.head;
    let kept = entries.findIndex(candidate => candidate.head === undefined && candidate.id >= target);
    if (kept < 0) kept = entries.length;
    entryOps.push(["p", ["entries"], 0, kept, [entry]]);       // head marker 放最前，保留从 head 起的非 head 后缀
    entries = [entry, ...entries.slice(kept)];
    continue;
  }
  if (change.type !== "document" || change.conversationId !== id) continue;
  const kind = change.record.kind;
  if (!MOUNTED_KINDS.has(kind) || change.record.key !== undefined) continue;
  const path = ["docs", kind];
  const mounted = mount.docs.get(kind);
  if (change.value === null) {
    if (mounted?.id !== change.record.id) continue;
    mount.docs.delete(kind);
    docOps.push(["d", path]);
  } else if (mounted?.id === change.record.id && mounted.version === change.version) {
    for (const op of change.ops) docOps.push(prefixed(op, path));
  } else {
    mount.docs.set(kind, { id: change.record.id, version: change.version });
    docOps.push(["s", path, change.value]);
  }
}
const before = mount.value;
const ops = [...docOps, ...entryOps];
const frameContext = withoutAbortSignal(context);
if (ops.length > 0) {
  const value = docOps.length === 0 ? mount.value : applyImmutable(mount.value, docOps);
  mount.value = entries === value.entries ? value : { ...value, entries };
  for (const observer of [...mount.observers]) contained(() => observer.advance?.(mount.value, ops, frameContext), report);
}
for (const observer of [...mount.observers]) contained(() => observer.publication?.(before, mount.value, ops, publication, frameContext), report);
```

- **文档 op 在前，entry op 在后**（因此 `ops` 顺序是 docOps 然后 entryOps）；
- 文档"新化身或新版本"→ 整体 `set`；同化身同版本 → 前缀重写后的 delta ops；`null` 且化身匹配 → `delete`；
- 取 `withoutAbortSignal(context)` 作为 observer 的帧上下文；
- 一个 observer 抛错不影响其他（`contained` + report）。

#### 2.14.5 `prefixed(op, prefix)`

```ts
const at = path => [...prefix, ...path];
switch (op[0]) {
  case "r": return ["s", prefix, op[1]];           // 根替换 → 前缀 set
  case "p": return ["p", at(op[1]), op[2], op[3], op[4]];
  case "m": return ["m", at(op[1]), op[2]];
  case "s": return ["s", at(op[1]), op[2]];
  case "d": return ["d", at(op[1])];
  case "a": return ["a", at(op[1]), op[2]];
  case "t": return ["t", at(op[1]), op[2]];
}
```

### 2.15 `src/harness/scheduler.ts`（2,527 行，7 个导出）

导出：`BlockedSubject`、`blockedSubject`、`BlockedReason`、`SchedulerOutcome`、`InvocationBinding`、`TaskSchedulerOptions`、`TaskScheduler`（内部类型 `AnyTaskRecord`、`RunnableTaskRecord`、`Checkpoint`、`ErasedDefinition`、`ErasedRuntime`、`ErasedRunningTask`、`Resolution`、`Fit`、`Invocation`、`Phase`、`Reservation`、`ReportedTask`、`PhaseResult`、`Decision`、`TaskNode`、`Up`、`Step`、`Overlay`、`Node`、`Scope`，内部函数 `conversationNode`/`conversationOf`/`parentNode`/`REGISTRY`/`SETTINGS`/`ON_WAKE`/`installerOf`/`reportTask`/`recordTask`/`withAbortMark`/`cancellationIntent`/`failedOutcome`/`parentOf`/`nodeOf`/`overlayOf`/`addTo`/`deleteFrom`/`memoOf`/`sessionMethod`/`missingMigration`/`erased`/`endedError`/`progressed`/`withState`/`canReserve`/`validateSleep`/`jsonEqual`）。

#### 2.15.1 模块常量【逐字】

```ts
const SCAN_PAGE_SIZE = 256;
const LIVE_STATUSES = ["pending", "running", "waiting", "sleeping", "completing"] as const;
const REGISTRY: ReportInfo = { type: "host_error", callback: "registry" };
const SETTINGS: ReportInfo = { type: "host_error", callback: "settings" };
const ON_WAKE: ReportInfo = { type: "host_error", callback: "onWake" };
```

#### 2.15.2 公开类型

`TaskSchedulerOptions` 完整形状（逐字，构造见 §2.2.5 第 6 步）：
```ts
export type TaskSchedulerOptions = {
  telemetry?: HarnessOptions["telemetry"];
  session: SessionImpl; storage: Storage; registry: RegistryReader;
  models: () => Models;                                  // 每次访问读取
  agent: (conversationId, snapshot: RegistrySnapshot, context: Context) => Promise<Agent>;  // 每 phase 至多一次
  settings: () => Settings; env: (conversationId, context) => Promise<ExecutionEnv | undefined>;
  now: () => number; onWake: ((wake: Wake) => void) | undefined; timers: Timers;
  writeAtClose: (writes: () => readonly StorageWrite[]) => Promise<void>;
  report: (error: unknown, info: ReportInfo, context?: Context) => void;
  settleOutcome: (tx: Transaction, record: AnyTaskRecord, outcome: SchedulerOutcome) => Promise<void>;
  withdrawInputs: (tx: Transaction, conversationId: ConversationId) => Promise<void>;
  conversation: (id, binding: InvocationBinding, context) => Promise<ConversationHandle | undefined>;
  context: Context;      // 调度器提交与 invocation 用；不携带调用者取消
  readOnly?: boolean;    // open 按已提交状态载入 live 任务（含 running）；不提交、不运行、不请求唤醒
};
```
`InvocationBinding`：`{ readonly signal: AbortSignal; check(): void }`（`HarnessImpl` 为 conversation handle 创建；`check()` 在每次跨线调用前验证 harness 未关闭、invocation 未结束）。


```ts
export type BlockedSubject = Pick<AnyTaskRecord,
  "id" | "kind" | "version" | "abortRequested" | "abortReason" | "restarts"> & {
  readonly status: TaskState<JsonValue, JsonValue>["status"];
};
export function blockedSubject(record): BlockedSubject {
  return { id, kind, version, abortRequested,
           ...(abortReason === undefined ? {} : { abortReason }),
           ...(restarts === undefined ? {} : { restarts }),
           status: record.state.status };
}
export type BlockedReason = "missing_task" | "task_too_old" | "migration_failed" | "restart_limit";
export type SchedulerOutcome = Extract<TaskOutcome<JsonValue>, { readonly status: "faulted" | "orphaned" }>;
export type InvocationBinding = { readonly signal: AbortSignal; check(): void };
export type TaskSchedulerOptions = { /* 完整形状见 §2.15.2 开头 */ };
```

#### 2.15.3 内部类型

```ts
type Checkpoint = { readonly phase: string; readonly attempt?: number };
type TaskNode = Pick<AnyTaskRecord, "conversationId" | "owner" | "background">;
type Up = { readonly task: TaskId } | { readonly conversation: ConversationId };
type Step = { readonly task: TaskId; readonly node: TaskNode } | { readonly conversation: ConversationId } | { readonly unknown: true };
type Overlay = { readonly tasks: ReadonlyMap<TaskId, AnyTaskRecord>; readonly edges: ReadonlyMap<ConversationId, TaskId | null>; links?: ReadonlyMap<Node, ReadonlySet<Node>> };
type Node = TaskId | `c${number}`;                  // 会话节点记作 "c<id>"
type Scope = { readonly conversation: ConversationId } | { readonly roots: true };
type Resolution = { readonly kind: "ready"; readonly task: AnyTask; readonly record: RunnableTaskRecord }
                 | { readonly kind: "blocked"; readonly reason: BlockedReason };
type Fit = { readonly task: AnyTask; readonly migrates: boolean }
         | { readonly reason: BlockedReason; readonly error?: unknown };
type Decision = boolean | { readonly fault: unknown };
type PhaseResult = { readonly checkpoint: Checkpoint; readonly failure?: { readonly error: unknown } };
```

`Invocation` 字段：`taskId`、`kind`、`extension: string | undefined`（预留时安装该定义的扩展；内置任务没有）、`conversationId`、`mode: "run" | "abort"`、`controller: AbortController`、`links?: TelemetryCarrier[]`、`context: Context`、`record?: TaskRecord`、`fault?`、`watches: Set<DocumentWatch>`、`timers: Set<() => void>`、`ended: boolean`、`done: Promise<void>`、`finish: () => void`。

`Phase`：`{ snapshot: () => RegistrySnapshot; task: () => AnyTask; agent: Promise<Agent> | undefined }`。

#### 2.15.4 字段与不变量（必须逐条保留）

`#live: Map<TaskId, AnyTaskRecord>` —— 每个已提交的非终态记录（pending/running/waiting/sleeping/completing）的镜像；**同步**的提交监听在 Session 线上更新它，因此线上的代码读到的正是已提交状态。

`#contexts: Map<ConversationId, { range: ContextRange; idleSince: number | undefined }>` —— 每个会话最近一次通过任务 runtime 读取的上下文范围；后续读取（任何任务）只扫描更新条目。不持久化；idle 超过 `settings.contextRetentionMs` 的第一次 idle 检查时丢弃，close 时清空。

其余字段：
`#expiry`（最早 idle context 到期的后台定时器）、`#invocations: Map<TaskId, Invocation>`、`#taskWaiters: Waiters<TaskId, SettledTask>`、`#idleWaiters: Waiters<ConversationId | undefined, void>`、`#failedMigrations: Map<TaskId, { task, error }>`、`#edges: Map<ConversationId, TaskId | null>`（已知会话的 owner；`null` 表示 ownerless）、`#settled: Map<TaskId, TaskNode>`（已知终态任务的归属字段）、`#dropQueue: Set<Node>`、`#below: Map<Node, Set<Node>>`（活工作周围的向下树）、`#roots: Set<ConversationId>`、`#unloaded: Set<TaskId>`（链可能未加载的已知任务）、`#intent: Set<TaskId>`（有取消意图的活任务）、`#runnable: Set<TaskId>`（非 completing 的活任务候选）、`#finalizeChecks: Set<TaskId>`、`#waiters: Map<TaskId, Set<TaskId>>`、`#failFastChecks: Set<TaskId>`、`#abandoned: Set<TaskId>`、`#sleeping: Set<TaskId>`、`#timer`、`#lastWake`、`#wakeScheduled`、`#blockedListeners: Set<() => void>`、`#snapshotReported`、`#parkedTasks: Set<TaskId>`、`#reconcileScheduled`、`#reconcilingWork: number`、`#sweepScheduled`、`#cascadePending`、`#unsubscribeRegistry`、`#enabled`、`#readOnly`、`#closing`、`#dirty`、`#draining`。

**全局不变量（源码注释，必须逐条实现并测试）**：
1. 每个任务状态转移都由**一个在 Session 线上串行化的回调**决定并写入：预留、标记、runtime 提交、finalize，以及每个 phase 之前的同步 step（应用优先级规则并写 fault 或交接）。Handler 与 join 在线外运行。invocation 在其结束决策的 step **内部**结束，因此它排队的 runtime 提交要么在该决策前落地，要么被拒绝。
2. 所有权是一棵树：任务的父 = 其 owner 任务或所属会话；会话的父 = 其 owner 任务（若有）。向上走决定什么能到达某任务（取消中的 owner、作用域）；`#below` 列出活工作周围的向下树。
3. 索引有界：owner 边与已结束任务的归属字段只在 `#below` 中其下方仍列出东西时保留；因此只完整知道活任务之上的链。之后出现在被丢弃节点下方的工作会发现它"未知"，从 Storage 重新加载其链。
4. `#reconcile` 的提交不含扩展代码；抛错视为存储失败/宿主回调/ bug，直接 `#failSession`（不再重试）。

#### 2.15.5 `open(context)`

```ts
this.#session.observeCommits(publication => this.#observe(publication));
this.#session.subscribeClose(() => this.#seal());
this.#unsubscribeRegistry = this.#registry.subscribe(() => { this.#notifyBlocked(); this.#kick(); });
const limit = this.#restartLimit();
const parked: AnyTaskRecord[] = [];
await this.#session.commitWith(async (tx) => {
  const scans = [];
  for (const status of LIVE_STATUSES) scans.push(await scanAll(cursor => tx.scanTasks({ status }, 256, cursor)));
  for (const records of scans) for (const record of records) {
    this.#track(undefined, record);
    this.#resumed.add(record.id);
    if (this.#readOnly) continue;
    if (record.abandonOnRestart === true) this.#abandoned.add(record.id);
    if (record.state.status === "running") {
      const restarts = (record.restarts ?? 0) + 1;
      const pending = withState(record, { status: "pending", checkpoint: record.state.checkpoint });
      tx.setTask({ ...pending, restarts });
      if (restarts >= limit && !record.abortRequested) parked.push(record);
    }
    if (record.state.status === "waiting" && record.state.policy === "failFast") this.#failFastChecks.add(record.id);
  }
}, context);
for (const record of parked) {
  const restarts = (record.restarts ?? 0) + 1;
  const error = new Error(`Task ${record.id} (${record.kind}) was cut off ${restarts} times without progress; it is parked until retryTask()`);
  this.#report(error, { type: "task_parked", ...this.#aboutTask(recordTask(record)) });
}
if (this.#readOnly) return;
this.#cascadePending = true;
this.#scheduleReconcile();
this.#scheduleWake();
```

【逐字】`Task ${record.id} (${record.kind}) was cut off ${restarts} times without progress; it is parked until retryTask()`

#### 2.15.6 `resume()` / `retry(id, context)`

- `resume`：`if (readOnly) throw new ReadOnlySession("running tasks")`；`#enabled = true`；`#wakeDue()`；`#kick()`。幂等。
- `retry`：提交：`current = await tx.task(id)`；不存在抛 `Task ${id} does not exist`；`status === "terminal" || "completing" || !#parked(current)` 直接返回；否则 `tx.setTask(progressed(current))`（清 `restarts`）。之后 `resume()`。

#### 2.15.7 `blockedReason(record: BlockedSubject)` / `subscribeBlocked(listener)`

```ts
const status = record.status;
if (status === "running" || status === "completing" || status === "terminal") return undefined;
if (record.abortRequested && record.abortReason === undefined) return undefined;   // 请求会跑 abort 或 orphan
if (this.#parked(record)) return "restart_limit";
let snapshot;
try { snapshot = this.#registry.snapshot(); }
catch (error) {
  if (!this.#snapshotReported) this.#report(error, REGISTRY);
  this.#snapshotReported = true;
  return undefined;
}
const fit = this.#fit(record, snapshot.task(record.kind));
if ("reason" in fit) return fit.reason;
return fit.migrates && fit.task.definition.migrate === undefined ? "migration_failed" : undefined;
```

`subscribeBlocked`：加入 Set，返回移除函数。

#### 2.15.8 `wake()` → `Wake`【逐项】

```ts
if (this.#readOnly) return { active: false };
const now = this.#now();
let active = this.#invocations.size > 0 || this.#reconcilingWork > 0 || this.#reconcileWork();
let at: number | undefined;
let snapshot: RegistrySnapshot | undefined;
let snapshotFailed = false;
for (const record of this.#live.values()) {
  const state = record.state;
  if (state.status === "completing") continue;
  if (state.status === "waiting" && !record.abortRequested && state.on.some(id => this.#live.has(id))) continue;
  if (state.status === "sleeping" && !record.abortRequested && state.until > now) {
    at = at === undefined ? state.until : Math.min(at, state.until);
    continue;
  }
  if (active) continue;
  if (this.#parked(record)) continue;
  if (record.abortRequested && this.#waitingOn(record, true).length > 0) continue;
  if (!record.abortRequested || record.abortReason !== undefined) {
    if (snapshot === undefined && !snapshotFailed) {
      try { snapshot = this.#registry.snapshot(); } catch (error) { snapshotFailed = true; this.#report(error, REGISTRY); }
    }
    if (snapshot !== undefined && "reason" in this.#fit(record, snapshot.task(record.kind))) continue;
  }
  active = true;
}
return at === undefined ? { active } : { active, at };
```

语义：`completing` 不计；等活任务的等待不计（它们等的活工作自己算）；未到期的 sleep 只贡献 `at`；parked、无定义可接的任务、abort 等待其下方活工作的任务都不算 active；**reconcile 有待办也算工作**。

#### 2.15.9 `join()`

```ts
const stopped = [...this.#invocations.values()];
for (const invocation of stopped) invocation.controller.abort();
await Promise.allSettled(stopped.map(invocation => invocation.done));
if (stopped.length === 0) return;
await this.#writeAtClose(() => stopped.flatMap(invocation => {
  const record = this.#live.get(invocation.taskId);
  if (record?.state.status !== "running") return [];
  const value = withState(record, { status: "pending", checkpoint: record.state.checkpoint });
  return [{ type: "task", value }];
})).catch(() => {});
```

语义：close 不是 crash——被停下的任务写回 `pending`（下次 open 不计 restart）。`writeAtClose` 在 seal 之前已准入的提交之后、线上读取，从而保留那些提交（如 abort 标记）。

#### 2.15.10 `abort(id, context, keepRestart = false)`

```ts
const marked = await this.#session.commitWith(async (tx) => {
  const current = await tx.task(id);
  if (current === undefined) throw new Error(`Task ${id} does not exist`);
  if (current.state.status === "terminal") return { result: "terminal" };
  const invocation = this.#invocations.get(id);
  if (keepRestart && current.abortReason === "restart") return { result: "marked" };
  if (invocation === undefined && current.state.status !== "completing") {
    await this.#loadScopes(false);
    if (!this.#hasOwnedLive(id)) {
      const resolution = this.#resolve(current as RunnableTaskRecord, this.#registry.snapshot());
      if (resolution.kind === "blocked") {
        await this.#terminate(tx, current, { status: "orphaned", reason: resolution.reason });
        return { result: "marked" };
      }
    }
  }
  if (!current.abortRequested || current.abortReason !== undefined) tx.setTask(withAbortMark(current));
  return { result: "marked", run: invocation?.mode === "run" ? invocation : undefined };
}, context);
if (marked.run !== undefined) {
  await awaitWithContext(Promise.race([marked.run.done, this.#session.failed]), context);
}
return marked.result;
```

#### 2.15.11 `waitForTask(id, context)`

```ts
const found = await this.#session.readOnLine(async () => {
  if (this.#closing) throw closedError(this.#session);
  if (this.#live.has(id)) return { promise: this.#taskWaiters.add(id, context) };
  const record = await this.#storage.task(id, context);
  if (record === undefined) throw new Error(`Task ${id} does not exist`);
  return { promise: Promise.resolve(record as SettledTask<JsonValue>) };
});
return found.promise;
```

#### 2.15.12 `waitForIdle(conversationId, context)`

```ts
if (this.#closing) return Promise.reject(closedError(this.#session));
if (this.#idle(conversationId)) return Promise.resolve();
this.#scheduleReconcile();
return this.#idleWaiters.add(conversationId, context);
```

#### 2.15.13 `abortConversation(conversationId, background, context)`

```ts
const reached = await this.#session.commitWith(async (tx) => {
  const queued = await this.#loadScopes(true);
  const scope = { conversation: conversationId };
  const reached: TaskId[] = [];
  const walk = this.#walkDown(conversationNode(conversationId), { crossBackground: background });
  for (const { node, live } of walk) {
    if (live === undefined || (node.background && !background)) continue;
    reached.push(live.id);
    if (!live.abortRequested || live.abortReason !== undefined) tx.setTask(withAbortMark(live));
  }
  for (const id of queued) {
    if (this.#inScope({ conversation: id }, scope, background) === true) await this.#withdrawInputs(tx, id);
  }
  return reached;
}, context);
if (background) for (const id of reached) await this.waitForTask(id, context);
await this.waitForIdle(conversationId, context);
```

#### 2.15.14 `#observe(publication)`【提交监听，顺序严格】

```ts
if (publication.kind === "memory") return;                 // 内存提交只改文档
const updated: AnyTaskRecord[] = [];
const failed: TaskId[] = [];
let changed = false;
// 1) 会话边先：同一提交里在新会话中创建的任务要能挂上去
for (const change of publication.changes)
  if (change.type === "conversation" && !this.#edges.has(change.value.id))
    this.#setEdge(change.value.id, change.value.owner?.taskId ?? null);
// 2) 任务
for (const change of publication.changes) {
  if (change.type !== "task") continue;
  changed = true;
  const record = change.value;
  const previous = this.#live.get(record.id);
  if (failedOutcome(record) && (previous === undefined || !failedOutcome(previous))) failed.push(record.id);
  this.#track(previous, record);
  if (record.state.status === "terminal") {
    this.#failedMigrations.delete(record.id);
    this.#failFastChecks.delete(record.id);
    this.#taskWaiters.resolve(record.id, record as SettledTask<JsonValue>);
    this.#scheduleReconcile();
    continue;
  }
  if (record.abortRequested && previous?.abortRequested !== true) {
    this.#cascadePending = true;
    const invocation = this.#invocations.get(record.id);
    if (invocation?.mode === "run") invocation.controller.abort();
  }
  if (previous?.abortReason !== undefined && record.abortReason === undefined) this.#cascadePending = true;
  const status = record.state.status;
  if (status === "completing" && previous?.state.status !== "completing") {
    if (cancellationIntent(record)) this.#cascadePending = true;
    this.#scheduleReconcile();
  }
  if (status === "waiting" && record.state.policy === "failFast" && previous?.state.status !== "waiting") {
    this.#failFastChecks.add(record.id);
    this.#scheduleReconcile();
  }
  if (previous === undefined) updated.push(record);
}
// 3) 失败任务的 failFast 等待者
for (const id of failed) {
  for (const waiter of this.#waiters.get(id) ?? []) {
    const state = this.#live.get(waiter)?.state;
    if (state?.status === "waiting" && state.policy === "failFast") {
      this.#failFastChecks.add(waiter);
      this.#scheduleReconcile();
    }
  }
}
// 4) 被取消 owner 之下的排队输入
for (const change of publication.changes) {
  if (change.type !== "submission" || change.value.status !== "queued" || change.value.type !== "input") continue;
  if (this.#intent.size === 0) continue;
  const conversation = change.value.conversationId;
  if (!this.#known(conversationNode(conversation)) || this.#belowCancelled({ conversation })) this.#cascadePending = true;
}
// 5) 新任务：链未加载，或其父已被取消
for (const record of updated) {
  if (this.#unloaded.has(record.id)) this.#scheduleReconcile();
  else if (!record.background && !record.abortRequested && this.#belowCancelled(parentOf(record))) this.#cascadePending = true;
}
if (this.#cascadePending) this.#scheduleReconcile();
this.#sweep();
if (!changed) return;
this.#settleIdle();
this.#kick();
```

#### 2.15.15 `#contextRetentionMs()` / `#settleIdle()` / `#scheduleExpiry()`

- `#contextRetentionMs`：`try { return settings().contextRetentionMs } catch (error) { #report(error, SETTINGS); return 0 }`（保持上下文只是缓存，丢弃安全；此处抛错会逃出提交监听）。
- `#settleIdle`：
  ```ts
  for (const conversationId of this.#idleWaiters.keys()) if (this.#idle(conversationId)) this.#idleWaiters.resolve(conversationId);
  const now = this.#now();
  const retention = this.#contextRetentionMs();
  for (const [conversationId, kept] of this.#contexts) {
    if (kept.idleSince !== undefined && now - kept.idleSince >= retention) this.#contexts.delete(conversationId);
    else if (!this.#idle(conversationId)) kept.idleSince = undefined;
    else if (retention > 0) kept.idleSince ??= now;
    else this.#contexts.delete(conversationId);
  }
  this.#scheduleExpiry();
  ```
- `#scheduleExpiry`：取所有 `idleSince + retention` 的最小值；与当前 `#expiry.at` 相同则返回；否则解除旧定时器；`at === undefined || closing` 则不设；`this.#timers.background(cb, at - now)`（**后台定时器，不阻止进程退出**）；返回的 disarm 存在则记录。Cloudflare Workers 上宿主拒绝（`disarm === undefined`）时**不保留**：待定后台定时器会阻止 Durable Object 被驱逐。

#### 2.15.16 `#scheduleReconcile()` / `#reconcile()`

```ts
#scheduleReconcile() {
  if (this.#reconcileScheduled || this.#closing || this.#readOnly) return;
  this.#reconcileScheduled = true;
  queueMicrotask(() => void this.#reconcile());
}
```

`#reconcile`：
```ts
this.#reconcileScheduled = false;
const work = this.#reconcileWork();
if (work) this.#reconcilingWork++;
const cascade = this.#cascadePending;
this.#cascadePending = false;
const checks = [...this.#failFastChecks];
this.#failFastChecks.clear();
try {
  await this.#session.commitWith(async (tx) => {
    if (this.#closing) return;
    const queued = await this.#loadScopes(cascade);
    const marks = new Map<TaskId, { record: AnyTaskRecord; reason: "restart" | undefined }>();
    const mark = (record, reason) => {
      const staged = marks.get(record.id);
      if (staged !== undefined && staged.reason === undefined) return;     // 请求优先于 restart
      if (record.abortRequested && (record.abortReason === undefined || reason !== undefined)) return;
      marks.set(record.id, { record, reason });
    };
    for (const id of this.#intent) {
      const owner = this.#live.get(id)!;
      const restart = owner.abortReason === "restart" && !failedOutcome(owner);
      for (const { node, live } of this.#walkDown(id, { enter: below => !this.#intent.has(below) })) {
        if (live !== undefined && !node.background) mark(live, restart ? "restart" : undefined);
      }
    }
    for (const id of checks) {
      const waiter = this.#live.get(id);
      if (waiter?.state.status !== "waiting" || !(await this.#anyFailed(waiter.state.on))) continue;
      for (const member of waiter.state.on) {
        const record = this.#live.get(member);
        if (record !== undefined && !failedOutcome(record)) mark(record, undefined);
      }
    }
    for (const { record, reason } of marks.values()) tx.setTask(withAbortMark(record, reason));
    for (const id of queued) if (this.#belowCancelled({ conversation: id })) await this.#withdrawInputs(tx, id);
    await this.#finalize(tx);
  }, this.#context);
} catch (error) {
  this.#failSession(error);
}
if (work) this.#reconcilingWork--;
this.#scheduleWake();
this.#settleIdle();
```

`#reconcileWork()`：`#cascadePending || #finalizeChecks.size > 0 || #failFastChecks.size > 0`。
`#anyFailed(ids)`：逐个取 `#live` 或 storage 记录，`failedOutcome` 为真即 true。

#### 2.15.17 `#finalize(tx)`

```ts
let round = [...this.#finalizeChecks];
this.#finalizeChecks.clear();
const done = new Set<TaskId>();
const overlay = overlayOf(tx);
while (round.length > 0) {
  const next = new Set<TaskId>();
  for (const id of round) {
    const record = overlay.tasks.get(id) ?? this.#live.get(id);
    if (record?.state.status !== "completing" || done.has(id)) continue;
    if (this.#hasOwnedLive(id, overlay, done)) continue;
    const outcome = record.state.outcome;
    const terminal = withState(record, { status: "terminal", outcome });
    tx.setTask(terminal);
    (overlay.tasks as Map<TaskId, AnyTaskRecord>).set(id, terminal);
    done.add(id);
    if (outcome.status === "faulted" || outcome.status === "orphaned") await this.#settleOutcome(tx, record, outcome);
    const above = this.#nearestLiveAbove(record, overlay);
    if (above?.state.status === "completing") next.add(above.id);
  }
  round = [...next];
}
```

**轮次**设计：一轮中释放的 holder 在下一轮检查一次，所以"许多任务同时结束的 holder"只走一次，而不是每任务一次。

#### 2.15.18 `#loadScopes(queued)` / `#loadChain(start, overlay?)` / `#setEdge` / `#settle` / `#dropIfUnneeded` / `#scheduleSweep` / `#sweep`

- `#loadScopes(queued)`：先对 `#unloaded` 的每个 id `await #loadChain({ task: id })`（循环中前一次加载可能已处理它，故每次重查）；`queued` 为真时 `scanAll(scanSubmissions({ status: "queued" }))`，过滤 `type === "input"`，取去重会话 id，逐个 `#loadChain({ conversation: id })`，返回会话 id 数组。
- `#loadChain(start, overlay?)`：
  ```ts
  const passed: TaskId[] = [];
  let at: Up | undefined = start;
  while (at !== undefined) {
    if ("task" in at) {
      const id = at.task;
      let node = this.#live.get(id) ?? this.#settled.get(id);
      if (node === undefined && overlay?.tasks.has(id) === true) break;        // 本提交创建：其自身记录覆盖
      if (node !== undefined && !this.#unloaded.has(id)) break;
      if (node === undefined) {
        const record = await this.#storage.task(id, this.#context);
        if (record === undefined) return;
        node = nodeOf(record);
        if (record.state.status === "terminal") this.#settle(id, node);
        this.#scheduleReconcile();
      }
      passed.push(id);
      at = parentOf(node);
    } else {
      let edge = this.#edge(at.conversation, overlay);
      if (edge === undefined) {
        const record = await this.#storage.conversation(at.conversation, this.#context);
        edge = record?.owner?.taskId ?? null;
        this.#setEdge(at.conversation, edge);
        this.#scheduleReconcile();
      }
      at = edge === null ? undefined : { task: edge };
    }
  }
  for (const id of passed) this.#unloaded.delete(id);
  ```
- `#setEdge(id, owner)`：`#edges.set`；`#relist(conversationNode(id))`；`#dropIfUnneeded(...)`。
- `#settle(id, node)`：`#settled.set`；`#unloaded.add`；`#relist(id)`；`#dropIfUnneeded(id)`。
- `#dropIfUnneeded(node)`：若 node 是任务且 `#live` 有它 → 返回；若 `#below.has(node)` → 返回；否则 `#dropQueue.add(node)` + `#scheduleSweep()`。
- `#scheduleSweep()`：`#sweepScheduled || closing` 返回；否则 `readOnLine` 中 `#sweepScheduled = false; #sweep()`，错误 `.catch(() => {})`（只在关闭/失败时会拒绝）。
- `#sweep()`：
  ```ts
  for (const node of this.#dropQueue) {
    if (this.#below.has(node)) continue;
    if (typeof node === "string") { this.#edges.delete(conversationOf(node)); this.#roots.delete(conversationOf(node)); }
    else if (!this.#live.has(node)) { this.#settled.delete(node); this.#unloaded.delete(node); }
  }
  this.#dropQueue.clear();
  ```

#### 2.15.19 `#track(previous, record)`

```ts
const id = record.id;
const invocation = this.#invocations.get(id);
if (invocation !== undefined) invocation.record = record;
if (record.state.status === "terminal") {
  if (previous !== undefined && !this.#closing) this.#activeMetric.add(-1, { task: record.kind });
  this.#resumed.delete(id);
} else if (previous === undefined && !this.#closing) this.#activeMetric.add(1, { task: record.kind });
if (previous?.state.status === "waiting") for (const on of previous.state.on) deleteFrom(this.#waiters, on, id);
if (record.state.status === "terminal") {
  const node = nodeOf(record);
  this.#live.delete(id); this.#sleeping.delete(id); this.#intent.delete(id);
  this.#runnable.delete(id); this.#finalizeChecks.delete(id);
  this.#settled.set(id, node);
  this.#release(node);
  for (const waiter of this.#waiters.get(id) ?? []) this.#runnable.add(waiter);
  this.#relist(id);
  this.#dropIfUnneeded(id);
  return;
}
this.#live.set(id, record);
if (previous === undefined) {
  if (!this.#known(parentNode(record))) this.#unloaded.add(id);
  this.#relist(id);
}
if (cancellationIntent(record)) this.#intent.add(id); else this.#intent.delete(id);
if (record.state.status === "completing") { this.#runnable.delete(id); this.#finalizeChecks.add(id); }
else this.#runnable.add(id);
if (record.state.status === "waiting") for (const on of record.state.on) addTo(this.#waiters, on, id);
if (record.state.status === "sleeping") this.#sleeping.add(id); else this.#sleeping.delete(id);
```

`#release(node, overlay?)`：`above = #nearestLiveAbove`；`completing` → `#finalizeChecks.add`；`abortRequested` → `#runnable.add`；返回 above。

#### 2.15.20 `#relist(start)`【向上传播】

```ts
let node: Node = start;
for (;;) {
  const needed = (typeof node !== "string" && this.#live.has(node)) || this.#below.has(node);
  let parent: Node | null | undefined;
  if (typeof node === "string") parent = this.#edges.get(conversationOf(node));
  else {
    const known = this.#live.get(node) ?? this.#settled.get(node);
    if (known === undefined) return;
    parent = parentNode(known);
  }
  if (parent === undefined) return;
  if (parent === null) {
    const root = conversationOf(node as `c${number}`);
    if (needed) this.#roots.add(root); else this.#roots.delete(root);
    return;
  }
  if ((this.#below.get(parent)?.has(node) === true) === needed) return;
  if (needed) {
    const first = !this.#below.has(parent);
    addTo(this.#below, parent, node);
    if (!first) return;
  } else {
    deleteFrom(this.#below, parent, node);
    if (this.#below.has(parent)) return;
    this.#dropIfUnneeded(parent);
  }
  node = parent;
}
```

#### 2.15.21 `#nearestLiveAbove` / `#known` / `#edge` / `#node` / `#above` / `#walkDown` / `#links`

- `#nearestLiveAbove(node, overlay?)`：
  ```ts
  for (let current = node; !current.background; ) {
    const owner = current.owner ?? this.#edge(current.conversationId, overlay);
    if (owner === undefined || owner === null) return undefined;
    const record = overlay?.tasks.get(owner) ?? this.#live.get(owner);
    if (record !== undefined && record.state.status !== "terminal") return record;
    const above = record ?? this.#settled.get(owner);
    if (above === undefined) return undefined;
    current = above;
  }
  return undefined;
  ```
- `#known(node)`：任务 → `(#live.has || #settled.has) && !#unloaded.has`；会话 → `owner = #edges.get`；`owner === null || (owner !== undefined && #known(owner))`。
- `#edge(id, overlay?)`：overlay 有则 overlay 值，否则 `#edges.get`（`null` ownerless，`undefined` 未加载）。
- `#node(id, overlay?)`：`overlay.tasks.get ?? #live.get ?? #settled.get`。
- `#above(start, overlay?)` 生成器：向上产出 `{ task, node }` / `{ conversation }` / `{ unknown: true }`；`edge === undefined` 时产出 unknown 并结束。
- `#walkDown(start, { overlay?, crossBackground?, enter? })` 生成器：
  ```ts
  const pending: Node[] = [start];
  for (let at = pending.pop(); at !== undefined; at = pending.pop()) {
    for (const children of [this.#below.get(at), links?.get(at)]) {
      for (const child of children ?? []) {
        if (typeof child === "string") { pending.push(child); continue; }
        const record = overlay?.tasks.get(child) ?? this.#live.get(child);
        const node = record ?? this.#settled.get(child);
        if (node === undefined) continue;
        const live = record !== undefined && record.state.status !== "terminal" ? record : undefined;
        yield { id: child, node, live };
        if ((crossBackground || !node.background) && (enter === undefined || enter(child))) pending.push(child);
      }
    }
  }
  ```
  产出顺序：DFS（栈）；background 任务**被到达但不进入**（除非 `crossBackground`）。
- `#links(overlay)`：为提交中的新任务构建"父节点 → 子节点"链接，直到 `#below` 已列出的节点、活任务或链端；结果缓存在 `overlay.links`。

#### 2.15.22 `#ownedLive` / `#hasOwnedLive` / `#inScope` / `#belowCancelled` / `#cancellingOwner`

```ts
#ownedLive(owner, { overlay?, first?, done? } = {}) {
  const found: TaskId[] = [];
  const walk = this.#walkDown(owner, { ...(overlay && { overlay }), ...(done && { enter: id => !done.has(id) }) });
  for (const { id, node, live } of walk) {
    if (live === undefined || node.background) continue;
    found.push(id);
    if (first) break;
  }
  return found;
}
#hasOwnedLive(owner, overlay?, done?) { return this.#ownedLive(owner, { first: true, overlay, done }).length > 0; }

#inScope(start, scope, crossBackground = false) {
  for (const step of this.#above(start)) {
    if ("unknown" in step) return undefined;
    if ("conversation" in step) { if ("conversation" in scope && step.conversation === scope.conversation) return true; }
    else if (step.node.background && !crossBackground) return false;
  }
  return "roots" in scope;
}
#belowCancelled(start) { return this.#cancellingOwner(start) !== undefined; }
#cancellingOwner(start) {
  if (this.#intent.size === 0) return undefined;      // 常见路径：不走路
  for (const step of this.#above(start)) {
    if ("unknown" in step) return undefined;
    if (!("task" in step)) continue;
    const live = this.#live.get(step.task);
    if (live !== undefined && cancellationIntent(live)) return live;
    if (step.node.background) return undefined;
  }
  return undefined;
}
```

#### 2.15.23 `#failSession` / `#seal` / `#scheduleWake` / `#armTimer` / `#wakeDue` / `#parked` / `#aboutTask` / `#unparkRaised` / `#restartLimit` / `#notifyBlocked` / `#notDue`

- `#failSession(error, context?)`：`if (!this.#closing) this.#session.fail(error, context)`。
- `#seal()`（close 监听，同步，在 `join()` 之前或 Session 失败时）：
  ```ts
  for (const record of this.#live.values()) this.#activeMetric.add(-1, { task: record.kind });
  this.#resumed.clear();
  this.#closing = true;
  try { this.#unsubscribeRegistry(); } catch (error) { this.#report(error, REGISTRY); }
  const error = closedError(this.#session);
  this.#taskWaiters.rejectAll(error);
  this.#idleWaiters.rejectAll(error);
  this.#contexts.clear();
  this.#expiry?.disarm(); this.#expiry = undefined;
  this.#timer?.disarm(); this.#timer = undefined;
  ```
- `#scheduleWake()`：`#wakeScheduled || closing || readOnly` 返回；置位后 `queueMicrotask`：
  ```ts
  this.#wakeScheduled = false;
  if (this.#closing) return;
  const wake = this.wake();
  if (this.#onWake === undefined) { this.#armTimer(this.#enabled ? wake.at : undefined); return; }
  const last = this.#lastWake;
  if (last !== undefined && last.active === wake.active && last.at === wake.at) return;
  try { this.#onWake(wake); this.#lastWake = wake; } catch (error) { this.#report(error, ON_WAKE); }
  ```
  （只有成功投递才记录 `lastWake`；抛错后下次变化或 kick 会再次提供。）
- `#armTimer(at)`：`#timer` 已存在且 `at` 相同返回；否则解除旧定时器；`at === undefined || closing` 返回；`this.#timers.foreground(cb, at - now)`，cb 里 `#timer = undefined; #wakeDue(); #kick();`；记录 `{ at, disarm }`（**前台定时器，保持进程存活**）。
- `#wakeDue()`：对 `#sleeping` 每个 id，记录存在且 `!#notDue(record)` → `#runnable.add(id)`。
- `#parked(record)`：`!record.abortRequested && record.restarts !== undefined && record.restarts >= this.#restartLimit()`。
- `#aboutTask(task)`：`{ task: reportTask(task), ...optionalExtension(extension) }`；`extension` 若 `task` 自带则用之，否则 `snapshot = registry.snapshot(); extension = installerOf(snapshot, snapshot.task(task.kind))`（抛错吞掉，报告无扩展名）。
- `#unparkRaised()`：对 `#parkedTasks` 每个 id，记录存在且仍 parked → 继续；删除；记录存在则 `#runnable.add(id)` 并标记 `unparked`；最后若有则 `queueMicrotask(() => this.#notifyBlocked())`。
- `#restartLimit()`：`try { settings().restartLimit } catch { #report(error, SETTINGS); return DEFAULT_RESTART_LIMIT }`。
- `#notifyBlocked()`：遍历 `#blockedListeners`，`try { listener() } catch (error) { #report(error, { type: "listener_error" }) }`。
- `#notDue(record)`：`state.status === "sleeping" && !record.abortRequested && state.until > this.#now()`。

#### 2.15.24 `#kick()` / `#drain()`

```ts
#kick() {
  this.#scheduleWake();
  this.#dirty = true;
  if (this.#draining || !this.#enabled || this.#closing) return;
  this.#draining = true;
  queueMicrotask(() => void this.#drain());      // 绝不在提交/注册监听里同步提交
}
async #drain() {
  try {
    while (this.#dirty && this.#enabled && !this.#closing) {
      this.#dirty = false;
      for (const reservation of await this.#reserve()) this.#start(reservation);
    }
  } catch (error) { this.#failSession(error); }
  finally { this.#draining = false; if (this.#dirty) this.#kick(); }
}
```

#### 2.15.25 `#reserve()`【一次提交里预留所有合格任务】

```ts
const reservations: Reservation[] = [];
const faulted: { record: AnyTaskRecord; message: string }[] = [];
let abandoned: TaskId[] = [];
try {
  await this.#session.commitWith(async (tx) => {
    if (!this.#enabled || this.#closing) return;
    for (const id of this.#abandoned) {
      const record = this.#live.get(id);
      if (record !== undefined && !record.abortRequested) tx.setTask(withAbortMark(record, "restart"));
    }
    if (this.#abandoned.size > 0) { abandoned = [...this.#abandoned]; return; }   // 第一趟只打标记
    await this.#loadScopes(false);
    this.#wakeDue();
    this.#unparkRaised();
    let snapshot: RegistrySnapshot | undefined;
    for (const id of [...this.#runnable]) {
      const record = this.#live.get(id);
      if (record === undefined || record.state.status === "completing" || this.#invocations.has(id)) { this.#runnable.delete(id); continue; }
      if (this.#waitingOn(record, true).length > 0) { this.#runnable.delete(id); continue; }
      if (this.#notDue(record)) { this.#runnable.delete(id); continue; }
      if (this.#parked(record)) {
        this.#runnable.delete(id);
        if (!this.#parkedTasks.has(id)) queueMicrotask(() => this.#notifyBlocked());
        this.#parkedTasks.add(id);
        continue;
      }
      const runnable = record as RunnableTaskRecord;
      const mode = record.abortRequested ? "abort" : "run";
      if (mode === "run" && !record.background && this.#belowCancelled(parentOf(record))) {
        this.#cascadePending = true;
        this.#scheduleReconcile();
        continue;
      }
      if (mode === "abort" && (record.restarts ?? 0) >= this.#restartLimit()) {
        const message = `Abort handler of task ${id} (${record.kind}) was cut off ${record.restarts} times`;
        await this.#terminate(tx, record, { status: "faulted", error: { message, detail: { reason: "restart_limit" } } });
        faulted.push({ record, message });
        continue;
      }
      snapshot ??= this.#registry.snapshot();
      const resolution = this.#resolve(runnable, snapshot);
      if (resolution.kind === "blocked") {
        if (mode === "abort" && record.abortReason === undefined) {
          await this.#terminate(tx, record, { status: "orphaned", reason: resolution.reason });
        }
        continue;
      }
      if (resolution.record !== runnable || record.state.status !== "running") {
        tx.setTask(withState(resolution.record, { status: "running", checkpoint: resolution.record.state.checkpoint }));
      }
      this.#runnable.delete(id);
      const invocation = this.#createInvocation(runnable, mode, installerOf(snapshot, resolution.task));
      reservations.push({ invocation, task: resolution.task, snapshot });
    }
  }, this.#context);
} catch (error) {
  for (const { invocation } of reservations) {
    this.#invocations.delete(invocation.taskId);
    this.#runnable.add(invocation.taskId);
    invocation.finish();
  }
  throw error;
}
for (const { record, message } of faulted) {
  this.#report(new Error(message), { type: "task_faulted", ...this.#aboutTask(recordTask(record)), reason: "restart_limit" });
}
for (const id of abandoned) this.#abandoned.delete(id);
if (abandoned.length > 0) this.#dirty = true;
return reservations;
```

【逐字】`Abort handler of task ${id} (${record.kind}) was cut off ${record.restarts} times`。

#### 2.15.26 `#waitingOn(record, first = false)`

```ts
if (record.abortRequested) return this.#ownedLive(record.id, { first });
if (record.state.status !== "waiting") return [];
if (!first) return record.state.on.filter(id => this.#live.has(id));
const live = record.state.on.find(id => this.#live.has(id));
return live === undefined ? [] : [live];
```

#### 2.15.27 `#resolve(record, snapshot)` / `#fit(record, task)`

```ts
#resolve(record, snapshot) {
  const fit = this.#fit(record, snapshot.task(record.kind));
  if ("reason" in fit) return { kind: "blocked", reason: fit.reason };
  if (!fit.migrates) return { kind: "ready", task: fit.task, record };
  const definition = erased(fit.task);
  try {
    if (definition.migrate === undefined) throw missingMigration(record, definition);
    const migrated = definition.migrate(record.input, record.state.checkpoint, record.version);
    const state = { ...record.state, checkpoint: copyJson(migrated.checkpoint) };
    const migratedRecord = { ...record, version: definition.version, input: copyJson(migrated.input), state };
    return { kind: "ready", task: fit.task, record: migratedRecord };
  } catch (error) {
    this.#failedMigrations.set(record.id, { task: fit.task, error });
    queueMicrotask(() => this.#notifyBlocked());
    this.#scheduleWake();
    const extension = installerOf(snapshot, fit.task);
    this.#report(error, { type: "migration_failed", ...this.#aboutTask({ ...recordTask(record), extension }) });
    return { kind: "blocked", reason: "migration_failed" };
  }
}
#fit(record, task) {
  if (task === undefined) return { reason: "missing_task" };
  const version = task.definition.version;
  if (version === record.version) return { task, migrates: false };
  if (version < record.version) return { reason: "task_too_old" };
  const failed = this.#failedMigrations.get(record.id);
  if (failed?.task === task) return { reason: "migration_failed", error: failed.error };
  return { task, migrates: true };
}
```

`missingMigration`【逐字】：`Task ${record.kind} version ${definition.version} has no migration from ${record.version}`。

#### 2.15.28 `indexSizes` getter / `inspect(snapshot)` / `#inspectTask(record, snapshot)`

```ts
get indexSizes() {
  let below = 0;
  for (const children of this.#below.values()) below += children.size;
  return { live: #live.size, settled: #settled.size, edges: #edges.size, below, roots: #roots.size,
           unloaded: #unloaded.size, dropQueue: #dropQueue.size };
}
async inspect(snapshot) {
  await this.#loadScopes(false);
  const tasks = [];
  for (const record of this.#live.values()) tasks.push({ record, state: this.#inspectTask(record, snapshot) });
  const scheduling = this.#closing ? "closing" : this.#enabled ? "running" : "paused";
  return { scheduling, tasks };
}
#inspectTask(record, snapshot) {
  if (this.#invocations.has(record.id)) return { kind: "running" };
  if (record.state.status === "completing") return { kind: "completing" };
  const on = this.#waitingOn(record);
  if (on.length > 0) return { kind: "waiting", on };
  if (this.#notDue(record)) return { kind: "sleeping", until: record.state.until };
  if (this.#parked(record)) return { kind: "blocked", reason: "restart_limit" };
  const fit = this.#fit(record, snapshot.task(record.kind));
  if ("reason" in fit) return { kind: "blocked", ...fit };
  if (fit.migrates && fit.task.definition.migrate === undefined) {
    return { kind: "blocked", reason: "migration_failed", error: missingMigration(record, erased(fit.task)) };
  }
  return { kind: "ready", migrates: fit.migrates };
}
```

#### 2.15.29 `#createInvocation` / `#start` / `#run` / `#phase`

```ts
#createInvocation(record, mode, extension) {
  const controller = new AbortController();
  const { promise: done, resolve: finish } = Promise.withResolvers<void>();
  const invocation = {
    taskId: record.id, kind: record.kind, extension, conversationId: record.conversationId, mode, controller,
    links: record.startedAt === undefined ? record.links : undefined,
    context: withAbortSignal(controller.signal, this.#context),
    record, watches: new Set(), timers: new Set(), ended: false, done, finish: () => finish(),
  };
  this.#invocations.set(record.id, invocation);
  return invocation;
}
#start(reservation) {
  const invocation = reservation.invocation;
  void (async () => {
    try {
      if (invocation.mode === "run") await this.#run(reservation);
      else await this.#runAbort(reservation);
    } catch (error) {
      this.#report(error, { type: "task_error", ...this.#aboutTask(invocation) }, invocation.context);
    } finally {
      this.#end(invocation);
      invocation.record = undefined; invocation.links = undefined; invocation.fault = undefined;
      invocation.finish();
      this.#kick();
    }
  })();
}
async #run(reservation) {
  const invocation = reservation.invocation;
  const state = { task: reservation.task, snapshot: reservation.snapshot, reported: undefined };
  const phase = { snapshot: () => state.snapshot, task: () => state.task, agent: undefined };
  const runtime = this.#runtime(invocation, phase);
  let current = await this.#step(invocation, (tx, next) =>
    this.#decide(tx, next, undefined, state, invocation.extension, invocation.context));
  while (current !== undefined && !this.#closing) {
    const task = current;
    current = await this.#phase(invocation, task, async () => {
      const checkpoint = task.state.checkpoint;
      phase.agent = undefined;                       // 每个 phase handler 第一次使用时重新解析 agent
      let previous: PhaseResult;
      try {
        await erased(state.task).phases[checkpoint.phase](task, runtime, invocation.context);
        previous = { checkpoint };
      } catch (error) { previous = { checkpoint, failure: { error } }; }
      return this.#step(invocation, (tx, next) =>
        this.#decide(tx, next, previous, state, invocation.extension, invocation.context));
    });
  }
}
```

`#phase(invocation, task, run)`：包一个 `kiana.durable.phase` span（父 span = 从 `task.trace` 提取，否则 Harness telemetry）：
- start attrs：`{ "task.id": task.id, "task.name": task.kind, phase: checkpoint.phase, kind: invocation.mode, resumed, ...(attempt === undefined ? {} : { attempt }) }`；
- `resumed = this.#resumed.delete(task.id)`；
- 若 span 有 propagation 且 `invocation.links` 存在，逐个 `span.addLink({ context: span.propagation.extract(carrier) })`；
- `invocation.context = withTelemetry(span, previous)`；`started = performance.now()`；
- `finally`：恢复 context；计算 `outcome`：
  ```ts
  const state = invocation.record!.state;
  const terminal = invocation.fault ?? (state.status === "terminal" || state.status === "completing" ? state.outcome : undefined);
  const outcome =
    terminal !== undefined
      ? terminal.status === "completed" ? "done"
      : terminal.status === "aborted" ? "aborted" : "faulted"
      : state.status === "waiting" || state.status === "sleeping" ? state.status
      : invocation.ended || this.#closing ? "aborted"
      : `next=${state.checkpoint.phase}`;
  span.setAttributes({ outcome });
  if (terminal?.status === "faulted" || terminal?.status === "failed" || terminal?.status === "orphaned") {
    const message = terminal.status === "orphaned" ? terminal.reason : terminal.error.message;
    span.setStatus({ status: "error", error: { name: `task_${terminal.status}`, message: ownedSlice(message, 0, 500) } });
  } else if (outcome === "aborted") { span.setStatus({ status: "ok" }); }
  this.#durationMetric.record(performance.now() - started, { task: task.kind, phase: checkpoint.phase, outcome }, span);
  ```

#### 2.15.30 `#decide(tx, current, previous, state, extension, context)`【优先级规则】

```ts
// 3. abort 标记：结束；一旦其普通拥有工作消失，新的 abort invocation 会启动
if (current.abortRequested) return false;
// owner 的取消意图也会结束它，早于其级联标记；预留随后会挡住它
if (!current.background && this.#belowCancelled(parentOf(current))) {
  this.#cascadePending = true; this.#scheduleReconcile(); return false;
}
if (previous === undefined) return true;
// 4. 未捕获错误
if (previous.failure !== undefined) return { fault: previous.failure.error };
// 6. 没有持久进展
if (jsonEqual(current.state.checkpoint, previous.checkpoint)) {
  return { fault: new Error(`Task ${current.kind} phase ${previous.checkpoint.phase} returned without durable progress`) };
}
// 5. 有进展：刷新快照；交给能接管的替换定义
state.snapshot = this.#registry.snapshot();
const next = state.snapshot.task(current.kind);
if (next !== state.task) {
  if (next !== undefined && canReserve(next, current)) {
    tx.setTask(withState(current, { status: "pending", checkpoint: current.state.checkpoint }));
    return false;
  }
  if (state.reported === undefined || state.reported.task !== next) {
    state.reported = { task: next };
    this.#report(new Error(`Task ${current.id} keeps running under its old ${current.kind} definition`), {
      type: "task_outdated", ...this.#aboutTask({ ...recordTask(current), extension }),
      reason: next === undefined ? "missing_task" : "incompatible_task",
    }, context);
  }
}
return true;
```

【逐字】
- `Task ${current.kind} phase ${previous.checkpoint.phase} returned without durable progress`
- `Task ${current.id} keeps running under its old ${current.kind} definition`

规则编号对应 spec §5.4：1（终态/completing/waiting/sleeping）、2（closing）由 `#step` 应用；3 abort 标记；4 未捕获错误；5 进展/交接；6 无持久进展。

#### 2.15.31 `#runAbort(reservation)` / `#step(invocation, decide)`

```ts
async #runAbort(reservation) {
  const invocation = reservation.invocation;
  const current = this.#live.get(invocation.taskId);
  if (current === undefined || this.#closing) return;
  await this.#phase(invocation, current, async () => {
    let failure;
    try {
      const phase = { snapshot: () => reservation.snapshot, task: () => reservation.task, agent: undefined };
      const runtime = this.#runtime(invocation, phase);
      await erased(reservation.task).abort(current, runtime, invocation.context);
    } catch (error) { failure = { error }; }
    const message = `Abort handler of task ${invocation.taskId} returned without a terminal outcome`;
    await this.#step(invocation, () => ({ fault: failure?.error ?? new Error(message) }));
  });
}
async #step(invocation, decide) {
  let fault;
  try {
    const continued = await this.#session.commitWith(async (tx) => {
      const found = this.#live.get(invocation.taskId);
      const current = found?.state.status === "running" ? found : undefined;
      const decision = current !== undefined && !this.#closing ? decide(tx, current) : false;
      if (decision === true) return current;
      this.#end(invocation);
      if (decision !== false) {
        fault = { error: decision.fault };
        const message = errorMessage(decision.fault);
        invocation.fault = { status: "faulted", error: { message } };
        await this.#terminate(tx, current, invocation.fault);
      }
      return undefined;
    }, withoutAbortSignal(invocation.context));
    if (fault !== undefined)
      this.#report(fault.error, { type: "task_faulted", ...this.#aboutTask(invocation), reason: "threw" }, invocation.context);
    return continued;
  } catch (error) {
    this.#end(invocation);
    this.#failSession(error, invocation.context);
    return undefined;
  }
}
```

【逐字】`Abort handler of task ${invocation.taskId} returned without a terminal outcome`。
语义：`#step` 在**一个**提交里读线上记录、应用规则 1/2、可能 `#end` 并写 fault（`#terminate`）；fault 在持久化后才报告（`reason: "threw"`）。

#### 2.15.32 `#terminate` / `#commitState`

```ts
async #terminate(tx, record, outcome) {
  await this.#loadScopes(false);
  if (this.#hasOwnedLive(record.id, overlayOf(tx))) {
    tx.setTask(withState(record, { status: "completing", outcome }));
    return;
  }
  tx.setTask(withState(record, { status: "terminal", outcome }));
  await this.#settleOutcome(tx, record, outcome);
}
async #commitState(tx, invocation, current, next) {
  if (next.status === "waiting") await this.#validateWait(tx, invocation, current, next.on, next.policy);
  if (next.status === "sleeping") validateSleep(invocation, current.id, next.until);
  if (next.status === "terminal") {
    const overlay = overlayOf(tx);
    await this.#loadScopes(false);
    for (const record of overlay.tasks.values()) await this.#loadChain(parentOf(record), overlay);
    if (this.#hasOwnedLive(current.id, overlay)) {
      tx.setTask(withState(progressed(current), { status: "completing", outcome: next.outcome }));
      return;
    }
  }
  const same = jsonEqual(current.state, next);
  tx.setTask(withState(same ? current : progressed(current), next));
}
```

**注意**：终态若仍有普通拥有工作，则持有为 `completing`（并把 overlay 里本提交新建的工作也算进去）；提交的 state 与当前不同才算进展（重启计数清零），**相同 checkpoint 的重复提交（如意图）不算**。

#### 2.15.33 `#validateWait` / `#waitCycle`

```ts
async #validateWait(tx, invocation, current, on, policy) {
  if (invocation.mode === "abort") throw new Error(`Abort handler of task ${current.id} cannot wait`);
  const overlay = overlayOf(tx);
  const targets: TaskId[] = [];
  for (const id of on) {
    const member = overlay.tasks.get(id) ?? this.#live.get(id) ?? (await this.#storage.task(id, this.#context));
    if (member === undefined) throw new Error(`Task ${id} does not exist`);
    if (policy === "failFast" && member.owner !== current.id) {
      throw new Error(`Task ${current.id} can wait failFast only on tasks it owns; ${id} is not one`);
    }
    if (member.state.status !== "terminal" && member.owner !== current.id) targets.push(id);
  }
  if (targets.length === 0) return;
  const cycle = await this.#waitCycle(current.id, targets, overlay);
  if (cycle !== undefined) {
    throw new Error(`Task ${current.id} cannot wait on ${cycle[0]}: ${[current.id, ...cycle].join(" -> ")} is a wait cycle`);
  }
}
```

【逐字】
- `Abort handler of task ${current.id} cannot wait`
- `Task ${id} does not exist`
- `Task ${current.id} can wait failFast only on tasks it owns; ${id} is not one`
- `Task ${current.id} cannot wait on ${cycle[0]}: ${[current.id, ...cycle].join(" -> ")} is a wait cycle`

`#waitCycle(task, on, overlay)`：BFS/DFS 在等待图上找从 `on` 的成员回到 `task` 的路径；边的定义：等待中的非 abort 任务的 `on` 成员，以及活任务的普通拥有工作（它们让任务持有为 `completing`）。`from` 记录父指针、`walked` 记录已向下走过的任务（其下方已覆盖），`reach(id, via)` 去重入队，`path(end)` 回溯。返回路径或 `undefined`。

#### 2.15.34 `#end(invocation)` / `#idle(conversationId)`

```ts
#end(invocation) {
  if (invocation.ended) return;
  invocation.ended = true;
  if (this.#invocations.get(invocation.taskId) === invocation) {
    this.#invocations.delete(invocation.taskId);
    if (this.#live.has(invocation.taskId)) this.#runnable.add(invocation.taskId);
  }
  for (const watch of invocation.watches) void watch.stop();
  for (const disarm of invocation.timers) disarm();
  invocation.timers.clear();
  invocation.controller.abort(endedError(invocation));      // 绑定它的等待（如工具的 waitForTask）一起 reject
}
#idle(conversationId) {
  const scope = conversationId === undefined ? { roots: true } : { conversation: conversationId };
  for (const id of this.#unloaded) {
    const record = this.#live.get(id);
    if (record !== undefined && !record.background && this.#inScope(parentOf(record), scope) !== false) return false;
  }
  const starts = conversationId === undefined ? this.#roots : [conversationId];
  for (const start of starts) {
    for (const { node, live } of this.#walkDown(conversationNode(start))) {
      if (live !== undefined && !node.background) return false;
    }
  }
  return true;
}
```

【逐字】`Task ${invocation.taskId} invocation has ended`（`endedError`）。

#### 2.15.35 `#runtime(invocation, phase)` —— 任务 runtime

`agent()`：invocation 已结束 → reject `endedError`；`phase.agent === undefined` 时 `phase.agent = this.#agent(conversationId, phase.snapshot(), invocation.context)` 并 `.catch(() => {})`（不留下未观察的失败）；返回同一 promise。

`hooks.each(name, invoke)`：对 `agentHooks(await agent(), phase.task().definition.name)` 每一项，`handler = handlers[name]`；非函数跳过；`await invoke(handler.bind(handlers))`；抛错时若 `invocation.controller.signal.aborted` 则 rethrow，否则 report `{ type: "extension_error", extension, task: reportTask(invocation) }`。

返回对象成员：
| 成员 | 行为 |
|---|---|
| `taskId` / `conversationId` | 来自 invocation |
| `signal` | `invocation.controller.signal` |
| `models` | getter → `models()` |
| `agent(context)` | 已结束 → reject；否则 `awaitWithContext(agent(), context)` |
| `settings` | getter → `settings()` |
| `env(context)` | `#read(invocation, () => #env(conversationId, context))` |
| `hooks` | 上述 runner |
| `registry` | getter → `phase.snapshot()` |
| `commit(change, context, options)` | `#gated(...)`：`next = await change(tx, current)`；`next !== undefined` → `#commitState`；若 `next.status === "running"` 且 phase 变了 → 记录 `checkpoint`；提交后若 `checkpoint !== undefined` → `invocation.context.telemetry.addEvent("kiana.durable.checkpoint", { phase: checkpoint })` |
| `memo(name, ...rest)` | 1 个额外参数（context）→ `#read` → `memoOf(#live.get(taskId), name)`；2 个（candidate, context）→ `#gated`：已有则返回之，否则 `tx.setTask({ ...current, memos: { ...current.memos, [name]: candidate } })` 并返回 candidate |
| `sleepUntil(until, checkpoint, context)` | `#gated(..., (tx, current) => #commitState(... { status: "sleeping", checkpoint, until }))` |
| `watchDoc(...)` | `#watchDoc(invocation, args)` |
| `snapshot(...)` / `snapshotAsOf(...)` | `#read(invocation, () => sessionMethod(session, "snapshot"\|"snapshotAsOf")(...args))` |
| `getTask(id, context)` | `#read(invocation, () => session.readOnLine(() => storage.task(id, context)))` |
| `waitForTask(id, context)` | `#read(invocation, () => this.waitForTask(id, withAbortSignal(invocation.controller.signal, context)))` |
| `ownedTasks(context)` | `#read` + `readOnLine`：已结束抛 `endedError`；`context.abortSignal?.throwIfAborted()`；遍历 `#below.get(invocation.taskId)` 的子节点，取 `#live.get(node)` 且 `record.owner === invocation.taskId`；按 id 升序，`copyJson` 拷贝 |
| `abortOwned(id, context)` | `#read`：`bound = withAbortSignal(...)`；读记录；`record?.owner !== invocation.taskId` → 抛【逐字】`Task ${id} is not owned by task ${invocation.taskId}`；终态返回；否则 `await this.abort(id, bound, true)` 然后 `await this.waitForTask(id, bound)` |
| `outcomes(ids, context)` | `#read` + `readOnLine`：逐个读 storage；`state?.status !== "terminal"` → 抛【逐字】`Task ${id} is not terminal`；收集 `state.outcome` |
| `conversation(id, context)` | `#read`：`#conversation(id, { signal: controller.signal, check: () => { if (ended) throw endedError } }, context)` |
| `entry(...)` | 两参 → `(id, context)`（无 token）；三参 → `(token, id, context)`；`#read` + `readOnLine(storage.entry(conversationId, id, context))`；`token === undefined \|\| entry?.kind === token.kind ? entry : undefined` |
| `context(conversationId, context, options)` | `#read`：`readContextFrom(session, storage, conversationId, context, options?.at, #contexts.get(conversationId)?.range)`；若 `range !== undefined && !closing && !invocation.ended && (kept === undefined \|\| kept.range.bounds.tail <= range.bounds.tail)`：非 idle → `#contexts.set(id, { range, idleSince: undefined })`；idle 且 retention > 0 → `#contexts.set(id, { range, idleSince: kept?.idleSince ?? now })` 并 `#scheduleExpiry()`；返回 `view` |
| `now()` | 已结束抛 `endedError`；否则 `#now()` |
| `setTimer(cb, delayMs)` | 已结束返回 `() => {}`；`disarm = timers.foreground(() => { timers.delete(disarm); if (!ended) cb(); }, delayMs)`；加入 `invocation.timers`；返回移除并 disarm 的函数 |
| `report(error, from)` | 已结束抛 `endedError`；`task = reportTask(invocation)`；`from === undefined` → `{ type: "task_error", ...#aboutTask(invocation) }`；`from.type === "extension"` → `{ type: "extension_error", task, ...optionalExtension(from.extension) }`；否则 `{ type: "host_error", callback: from.callback, task }` |

`#read(invocation, read)`：已结束抛 `endedError`；否则 `read()`。
`#gated(invocation, change, context, options?)`：
```ts
if (invocation.ended) return Promise.reject(endedError(invocation));
return this.#session.commitWith(async (tx) => {
  if (invocation.ended) throw endedError(invocation);
  if (this.#closing) throw closedError(this.#session);
  const found = this.#live.get(invocation.taskId);
  if (found === undefined) throw new Error(`Task ${invocation.taskId} is terminal`);
  if (found.state.status !== "running") throw new Error(`Task ${invocation.taskId} is ${found.state.status}`);
  const current = found;
  if (invocation.mode === "run" && current.abortRequested) throw new Error(`Task ${invocation.taskId} has a durable abort mark`);
  return change(tx, current);
}, context, { conversationId: invocation.conversationId, taskId: invocation.taskId }, options);
```

【逐字】`Task ${invocation.taskId} is terminal`、`Task ${invocation.taskId} is ${found.state.status}`、`Task ${invocation.taskId} has a durable abort mark`。

`#watchDoc(invocation, args)`：已结束抛 `endedError`；`session.watchDoc(...args)`；`undefined` 返回；若构建后已结束 → `void watch.stop()` 并抛 `endedError`；加入 `invocation.watches`；`watch.closed.then(() => invocation.watches.delete(watch))`。

#### 2.15.36 模块辅助函数

```ts
function installerOf(snapshot, task) {
  if (task === undefined) return undefined;
  return snapshot.installed().findLast(installed => installed.tasks?.includes(task) === true)?.name;
}
function reportTask(task) { return { id: task.taskId, kind: task.kind, conversationId: task.conversationId }; }
function recordTask(record) { return { taskId: record.id, kind: record.kind, conversationId: record.conversationId }; }
function withAbortMark(record, reason?) {
  const { abortReason: _reason, restarts: _restarts, ...rest } = record;
  return { ...rest, abortRequested: true, ...(reason === undefined ? {} : { abortReason: reason }) };
}
function cancellationIntent(record) { return record.state.status !== "terminal" && (record.abortRequested || failedOutcome(record)); }
function failedOutcome(record) {
  const state = record.state;
  return (state.status === "completing" || state.status === "terminal") && state.outcome.status !== "completed";
}
function parentOf(node) { return node.owner !== undefined ? { task: node.owner } : { conversation: node.conversationId }; }
function nodeOf(record) {
  return { conversationId: record.conversationId, ...(record.owner === undefined ? {} : { owner: record.owner }), background: record.background };
}
function overlayOf(tx) {
  const tasks = new Map(tx.stagedTasks().map(record => [record.id, record]));
  const edges = new Map(tx.stagedConversations().map(record => [record.id, record.owner?.taskId ?? null]));
  return { tasks, edges };
}
function memoOf(record, name) {
  const memos = record?.memos;
  return memos !== undefined && Object.hasOwn(memos, name) ? memos[name] : undefined;
}
function endedError(invocation) { return new Error(`Task ${invocation.taskId} invocation has ended`); }
function progressed(record) {
  if (record.restarts === undefined) return record;
  const { restarts: _restarts, ...rest } = record;
  return rest;
}
function withState(record, state) {
  if (state.status !== "terminal" && state.status !== "completing") return { ...record, state };
  const { memos: _memos, restarts: _restarts, ...rest } = record;
  return { ...rest, state };
}
function canReserve(task, record) {
  const definition = task.definition;
  return definition.version === record.version
      || (definition.version > record.version && definition.migrate !== undefined);
}
function validateSleep(invocation, task, until) {
  if (invocation.mode === "abort") throw new Error(`Abort handler of task ${task} cannot sleep`);
  if (typeof until !== "number" || !Number.isFinite(until) || until < 0)
    throw new Error(`Task ${task} cannot sleep until ${until}: not a time in milliseconds`);
}
function jsonEqual(left, right) { /* 结构相等，键序无关，同 tool.ts 的 jsonEqual */ }
```

【逐字】`Abort handler of task ${task} cannot sleep`、`Task ${task} cannot sleep until ${until}: not a time in milliseconds`。

> **实现顺序建议（Rust）**：先做 `#observe`/`#track`/`#relist`/`#below`/`#walkDown` 这套索引，再做 `#reserve`/`#step`/`#decide`/`#commitState`/`#terminate`，最后 `#reconcile`/`#finalize` 与 `wake`/`join`。测试钩子 `indexSizes` 与 `inspect()` 是验证索引有界性的关键（`harness-scheduler-indexes.test.ts`、`harness-scheduler-retention.test.ts`）。

### 2.16 `src/harness/task-graph.ts`（276 行，6 个导出）

导出：`TaskGraphState`、`TaskGraphNode`、`TaskGraph`、`TaskGraphWatch`、`TaskGraphBlocking`、`TaskGraphView`（内部：`LIVE_STATUSES`、`SCAN_PAGE_SIZE = 256`、`advance`、`publish`、`nodeOf`、`stateOf`、`phaseOf`、`Mount`、`Observer`、`AnyTaskRecord`）。

#### 2.16.1 类型（逐字）

```ts
export type TaskGraphState =
  | { status: "pending" | "running"; phase: string }
  | { status: "waiting"; phase: string; on: readonly TaskId[]; policy: JoinPolicy }
  | { status: "sleeping"; phase: string; until: number }
  | { status: "completing"; outcome: TaskOutcome<JsonValue>["status"] };  // completed|failed|aborted|orphaned|faulted

export type TaskGraphNode = {
  id: TaskId; kind: string; conversationId: ConversationId;
  owner?: TaskId;                 // 缺省 = 会话拥有的任务
  background: boolean; abortRequested: boolean;
  state: TaskGraphState;
  blocked?: BlockedReason;
  conversations: readonly ConversationId[];   // 该任务拥有的会话，按 ID 升序
};
export type TaskGraph = { tasks: Readonly<Record<string, TaskGraphNode>> };  // 键是十进制 ID 字符串
export type TaskGraphWatch = WatchHandle<TaskGraph>;
export type TaskGraphBlocking = {
  readonly blocked: (record: BlockedSubject) => BlockedReason | undefined;
  readonly subscribeBlocked: (listener: () => void) => () => void;
};
```

`stateOf(record)`：`pending`/`running` → `{ status, phase: phaseOf(checkpoint) }`；`waiting` → 加 `on: [...state.on]` 与 `policy`；`sleeping` → 加 `until`；`completing` 与 `terminal` 都映射为 `{ status: "completing", outcome: state.outcome.status }`（注释：终态记录不会到这里，它们离开图）。`phaseOf(checkpoint)`：`(checkpoint as { phase: string }).phase`。

`nodeOf(record, conversations, blocked)`：字段顺序为 `id, kind, conversationId, ...(owner ? {owner} : {}), background, abortRequested, state, conversations, ...(blocked ? {blocked} : {})`（**键序即序列化顺序**）。

#### 2.16.2 `TaskGraphView`

构造 `(session: SessionImpl, storage: Storage, blocking: TaskGraphBlocking)`：
1. `session.observeCommits((publication, context) => { if (mount === undefined || publication.kind === "memory") return; advance(mount, publication, context, blocking.blocked, error => session.report(error)); })`（**memory 提交只改文档，图不显示**）；
2. `blocking.subscribeBlocked(() => this.#refreshBlocked())`；
3. `session.subscribeClose(() => { closed = true; for (const observer of [...mount.observers]) contained(() => observer.end(session.failure), error => session.report(error)); mount = undefined; })`。

`watch(context)`：`#attach((value, release) => new Watch<TaskGraph>(value, release, error => session.report(error)), context)`；`signal = context.abortSignal`；若已 aborted → `observer.cancel()` 并 `throw signal.reason`；否则 `observer.observeCancellation(signal)`；返回 `observer`。

`#attach(create, context)`：在 Session 线上 `readOnLine`：
- `mount = this.#mount ?? { ...(await this.#build(context)), observers: new Set() }`；
- `detach`：从 `observers` 删除；若空且 `#mount === mount` → `#mount = undefined`；
- `observer = create(mount.value, detach)`；
- 若 `#closed` → 抛 `closedError(session)`；`context.abortSignal?.throwIfAborted()`；
- 登记 `#mount = mount`、`observers.add(observer)`；返回 `{ observer, detach }`。

`#refreshBlocked()`：`mount === undefined || closed` 返回；`readOnLine(async () => { for (const [key, record] of mount.records) { const blocked = blocking.blocked(record); if (blocked === mount.value.tasks[key]?.blocked) continue; ops.push(blocked === undefined ? ["d", ["tasks", key, "blocked"]] : ["s", ["tasks", key, "blocked"], blocked]); } publish(mount, ops, BACKGROUND_CONTEXT, error => session.report(error)); }).catch(() => {})`（只会在关闭/失败时拒绝）。

`#build(context)`：对 5 个 `LIVE_STATUSES` 逐个 `scanAll(scanTasks({status}, 256, cursor, context))` 收集记录；`records.sort((a,b) => a.id - b.id)`；每条记录 `scanAll(scanConversations({ ownerTaskId: record.id }, 256, cursor, context))` 得 `conversations = ids.sort()`；`subject = blockedSubject(record)`；`tasks[String(record.id)] = nodeOf(record, conversations, blocking.blocked(subject))`；`live.set(String(record.id), subject)`；返回 `{ value: { tasks }, records: live }`。

#### 2.16.3 `advance(mount, publication, context, blocked, report)`

```ts
const ops: Op[] = [];
const changed = new Map<string, TaskGraphNode | undefined>();
const node = (key) => changed.has(key) ? changed.get(key) : mount.value.tasks[key];
for (const change of publication.changes) {
  if (change.type !== "task") continue;
  const record = change.value; const key = String(record.id); const previous = node(key);
  if (record.state.status === "terminal") {
    mount.records.delete(key);
    if (previous === undefined) continue;
    ops.push(["d", ["tasks", key]]); changed.set(key, undefined); continue;
  }
  const subject = blockedSubject(record);
  mount.records.set(key, subject);
  const next = nodeOf(record, previous?.conversations ?? [], blocked(subject));
  if (previous !== undefined && JSON.stringify(next) === JSON.stringify(previous)) continue;   // 结构相等才跳过
  ops.push(["s", ["tasks", key], next]); changed.set(key, next);
}
// 任务之后处理会话：同一提交里创建会话与其 owner 任务时，owner 节点已存在；提交内 change 顺序未定义，故每个 owner 的列表重新排序
const created = new Map<string, ConversationId[]>();
for (const change of publication.changes) {
  if (change.type !== "conversation" || change.value.owner === undefined) continue;
  const key = String(change.value.owner.taskId);
  if (node(key) !== undefined) created.set(key, [...(created.get(key) ?? []), change.value.id]);
}
for (const [key, ids] of created) {
  const conversations = [...node(key)!.conversations, ...ids].sort((a, b) => a - b);
  ops.push(["s", ["tasks", key, "conversations"], conversations]);
}
publish(mount, ops, context, report);
```

`publish(mount, ops, context, report)`：`ops.length === 0` 返回；`mount.value = applyImmutable(mount.value, ops)`；`frameContext = withoutAbortSignal(context)`；对 `[...mount.observers]` 逐个 `contained(() => observer.advance(mount.value, ops, frameContext), report)`（一个观察者抛错被报告，其他仍收到）。

> **Rust 注意**：`JSON.stringify(next) === JSON.stringify(previous)` 依赖**键序**与数字/字符串格式；Rust 要按 §6.3 的 `stringify_js` 比较，或直接用结构相等（注意 `undefined` 字段已被省略，两者等价）。

### 2.17 `src/harness/partial-changes.ts`（184 行，3 个导出）

导出：`PartialMessage`、`PartialCommit`、`PartialChanges`（内部：`JsonContainer`、`Arguments`、`ToolCallState`、`applyChange`、`value`、`isRecord`）。

```ts
export type PartialMessage = JsonRepresentation<AssistantMessage>;
export type PartialCommit = {
  readonly plan: AssignPlan;   // 文本 append；tool call 参数交给 changes
  readonly toolCalls: ReadonlyMap<number, { readonly changes: readonly JsonChange[]; readonly snapshot: Arguments }>;
};
type ToolCallState = { readonly parser: StreamingJsonParser; changes: JsonChange[]; committed: boolean };
```

类字段：`#text: Map<number, { field: "text" | "thinking"; text: string }>`、`#toolCalls: Map<number, ToolCallState>`。

`observe(event)`：
- `text_delta` / `thinking_delta`：`field = "text" | "thinking"`；若该项存在且 `field` 相同 → `pending.text += event.delta`；否则 `set(index, { field, text: event.delta })`。
- `toolcall_start`：`set(index, { parser: new StreamingJsonParser(), changes: [], committed: false })`。
- `toolcall_delta`：取该项，缺失则返回；对 `parser.append(event.delta)` 的每个 change：若 `change.path.length === 0` 且（`kind === "set"` 时其值不是对象）→ **删除该项并返回**（参数根不是对象 → 放弃增量，最终提交快照）；否则 `changes.push(change)`。
- `text_end` / `thinking_end`：删除该项（provider 可能替换整块文本）。
- `toolcall_end`：删除该项。

`take(message)`：为每个文本项产出 `blocks.set(index, { kind: "fields", fields: new Map([[field, { kind: "append", text }]]) })`，清空 `#text`；对每个 toolCall 项：取 `message.content[index]`，若不是 `toolCall` 块跳过；否则 `toolCalls.set(index, { changes, snapshot: block.arguments })`、**`block.arguments = {}`**、`blocks.set(index, { kind: "fields", fields: new Map([["arguments", { kind: "keep" }]]) })`、`call.changes = []`。返回 `{ plan: { kind: "fields", fields: new Map([["content", { kind: "fields", fields: blocks }]]) }, toolCalls }`。

`commit(generation, message, commit)`：
```ts
const committedBlocks = generation.message?.content.length ?? 0;
const apply: [number, readonly JsonChange[]][] = [];
for (const [index, { changes, snapshot }] of commit.toolCalls) {
  const call = this.#toolCalls.get(index);
  if (call === undefined) continue;
  const block = message.content[index];
  if (index >= committedBlocks && call.committed && block?.type === "toolCall") {
    // 早先的 changes 不在已提交消息里：提交快照并停止跟踪
    this.#toolCalls.delete(index); block.arguments = snapshot; continue;
  }
  call.committed = true; apply.push([index, changes]);
}
assignJson(generation, "message", message, commit.plan);
const content = generation.message?.content ?? [];
for (const [index, changes] of apply) {
  const block = content[index];
  if (block?.type !== "toolCall") continue;
  for (const change of changes) applyChange(block, change);
}
```

`restore(commit)`：对每个 `[index, { changes }]`，若仍跟踪该项 → `call.changes = [...changes, ...call.changes]`。

`applyChange(block, change)`：`key = change.path.at(-1)`；`key === undefined` → 只处理 `kind === "set"` 且值为对象 → `block.arguments = root`；否则沿 `path.slice(0, -1)` 走容器（每段都已由 `observe` 见过 set）；`kind === "set"` → `slots[key] = value(change)`；`kind === "append"`（数组父）→ `slots[key] = `${slots[key]}${change.text}``；对象父 → `appendText(parent, String(key), change.text)`。

> 关键不变量（`harness-events` 的 "commits a growing text and tool call at a cost that follows the new text, not the string lengths" 与 "keeps a watcher's memory proportional to a long streamed text and tool call"）：提交成本只与新文本/新参数增量成正比；`take()` 必须在提交失败时能 `restore()`。

### 2.18 `src/harness/telemetry.ts`（139 行，1 个导出）

唯一导出 `DURABLE_TELEMETRY_SCHEMA`（`defineTelemetrySchema({ version: 1, spans, metrics })`），全文逐字见 §4.7。四个局部常量 `entry`（`kiana.durable.entry` 事件定义）、`reportAttributes`、`report`、`submission` 依次组装进两个 span 的 `events`。**没有运行时逻辑**；`defineTelemetrySchema` 是 `kiana-telemetry` 的类型辅助函数（编译期校验属性名/类型/枚举），运行时返回原对象。

### 2.19 `src/harness/provider.ts`（39 行，3 个导出）

```ts
export type ProviderState = { sessionId: string };
export const ProviderDoc = defineDoc<ProviderState>({
  kind: "kiana.provider", version: 1, scope: "conversation", history: "latest", fork: "initial",
  initial: () => ({ sessionId: uuidv7() }), checkpointWhen: () => true,
});
export async function ensureProviderSessionId(runtime, context): Promise<string> {
  const existing = await runtime.snapshot(ProviderDoc, runtime.conversationId, context);
  if (existing !== undefined) return existing.sessionId;
  let created: string | undefined;
  await runtime.commit(async (tx) => {
    created = (await tx.doc(ProviderDoc, runtime.conversationId)).sessionId;
    return undefined;
  }, context);
  if (created === undefined) throw new Error(`Conversation ${runtime.conversationId} has no provider session ID`);
  return created;
}
```

要点：**正常路径不写**（`snapshot` 命中即返回）；旧会话（无 `kiana.provider`）在 provider 请求前走一次迁移提交，`tx.doc()` 调 `initial()` 生成 uuidv7；`fork: "initial"` 使每个 fork 得到**全新身份**而非复制父会话。`runtime.commit` 返回 `undefined` → 不改变任务状态（无持久进展检查不触发，因为 `#gated` 的 change 返回 `undefined` 时 `#commitState` 不被调用）。

### 2.20 `src/harness/usage.ts`（72 行，5 个导出）

`UsageState` 与 `UsageDoc` 见 §4.5。`recordUsage(tx, conversationId, bucket, key, usage)`：
```ts
const totals = (await tx.doc(UsageDoc, conversationId))[bucket];
const total = Object.hasOwn(totals, key) ? totals[key] : undefined;   // 工具可能叫 toString
if (total === undefined) totals[key] = copyJson(usage, { omitUndefinedProperties: true });
else addUsage(total, usage);
```
`addUsage(total, usage)`：`input`、`output`、`cacheRead`、`cacheWrite`、`totalTokens` 直接 `+=`；`cacheWrite1h`、`reasoning` 仅在 `usage` 侧非 `undefined` 时 `(total.x ?? 0) + usage.x`；`cost.input/output/cacheRead/cacheWrite/total` 各 `+=`。
`addUsageState(sum, state)`：对 `models`、`tools` 两个桶，`Object.entries(state[bucket])`：已有则 `addUsage`；否则 `Object.defineProperty(sum[bucket], key, { value: structuredClone(usage), enumerable: true, writable: true })`（**用 defineProperty，因为名为 `__proto__` 的工具赋值会改原型**）。

> Rust：`serde_json::Map` 无原型问题，但 **`__proto__` 必须作为普通键**保留；数值累加为 f64（JS number），并按 chord 的数值集合写增量。

### 2.21 `src/harness/json.ts`（60 行，2 个导出）

```ts
export type AssignPlan =
  | { kind: "append"; text: string }    // 字符串自上次赋值后增长了 text：长度吻合时 append
  | { kind: "keep" }                    // 保持原值，调用者自己写
  | { kind: "fields"; fields: ReadonlyMap<string | number, AssignPlan> };

export function assignJson(target, key, value, plan?): void {
  if (plan?.kind === "keep") return;
  const slots = target as Record<string | number, JsonValue>;
  const current = slots[key];
  const fields = plan?.kind === "fields" ? plan.fields : undefined;
  if (isRecord(current) && isRecord(value)) {
    for (const name of Object.keys(current)) if (!Object.hasOwn(value, name)) delete current[name];
    for (const [name, child] of Object.entries(value)) assignJson(current, name, child, fields?.get(name));
    return;
  }
  if (Array.isArray(current) && Array.isArray(value) && current.length <= value.length) {
    for (let i = 0; i < value.length; i++) {
      if (i < current.length) assignJson(current, i, value[i], fields?.get(i));
      else current.push(value[i]);
    }
    return;
  }
  if (plan?.kind === "append" && typeof current === "string" && typeof value === "string"
      && !Array.isArray(target) && current.length + plan.text.length === value.length) {
    appendText(target as Draft<Record<string, string>>, String(key), plan.text);
    return;
  }
  if (current !== value) slots[key] = value;
}
```
`isRecord(v)`：`typeof v === "object" && v !== null && !Array.isArray(v)`。

**语义要点**（必须逐条移植）：
- 容器递归：对象只遍历 **value 的键**（current 中多余的键先删），数组在 `current.length <= value.length` 时逐项递归、超出部分 `push`（**数组缩短时整体赋值**）；
- 只在 `plan.kind === "append"` 且长度吻合（`current.length + text.length === value.length`）时用 `appendText`；否则退回比较（`current !== value` 才写）；
- `plan.kind === "keep"` 立即返回，**不删除/不写入**；
- 目标不是数组时才允许 append（数组字串必须重赋值，因为 `appendText` 只接受对象成员）。

### 2.22 `src/harness/util.ts`（66 行，4 个导出）

`Waiters<K, T>`（按 key 的挂起等待，每个只 settle 一次）：
```ts
add(key, context): Promise<T> {
  const signal = context.abortSignal;
  if (signal?.aborted) return Promise.reject(signal.reason);
  const waiter = Promise.withResolvers<T>();
  const set = this.#sets.get(key) ?? new Set();
  this.#sets.set(key, set.add(waiter));
  if (signal !== undefined) {
    const onAbort = () => { set.delete(waiter); if (set.size === 0 && this.#sets.get(key) === set) this.#sets.delete(key); waiter.reject(signal.reason); };
    signal.addEventListener("abort", onAbort, { once: true });
    const detach = () => signal.removeEventListener("abort", onAbort);
    waiter.promise.then(detach, detach);
  }
  return waiter.promise;
}
keys(): K[]                       // 插入序
resolve(key, value): void         // 先删 key，再 resolve 该集合的全部
rejectAll(error): void            // 清空全部并逐个 reject
```
`scanAll(scan)`：按页序收集所有 item，直到 `page.next === undefined`。
`closedError(session)`：`session.failure === undefined ? new Error("Harness is closed") : new SessionFailed(session.failure.error)`。
`optionalExtension(extension)`：`undefined ? {} : { extension }`。

### 2.23 `src/harness/define.ts`（44 行，6 个导出）

全部是**恒等/轻量构造**，无运行时校验（除 TS 类型）：
```ts
export function defineExtension(extension) { return extension; }
export function defineTool(tool) { return tool; }
export function section(key, render, options?) {
  return options?.tag === undefined ? { key, render } : { key, render, tag: options.tag };
}
export function hook(task, handlers) { return { task: task.definition.name, handlers }; }
export function wrapTool(tool, wrapper) { return { tool: tool.name, wrap: wrapper }; }
export function wrapSection(key, wrapper) { return { section: key, wrap: wrapper }; }
```
注意 `hook` 取 `task.definition.name`（**名字**，不是对象）；`wrapTool` 取 `tool.name`；`section` 在 `tag` 未给时**不写 `tag` 字段**（影响文档/线协议的键存在性）。

## 3. 用户可见面（逐字）

> **重要范围说明**：`packages/durable` 是**运行时库**，没有 CLI、没有 TUI、没有按键绑定、没有 ANSI 渲染、没有颜色、没有 spinner、没有布局宽度。本节覆盖该单元真正对外可见的东西：抛出的错误文本、报告（report）的结构与语义、错误消息目录、默认设置数值、文档/条目/任务种类名、telemetry 名、环境变量。CLI/TUI 的逐字表在 `kiana/spec/` 的其他单元（`coding-agent`、`tui` 等）中。

### 3.1 错误类（逐字，`src/errors.ts`）

| 类 | name | 消息模板 | 说明 |
|---|---|---|---|
| `ReadAfterWrite` | `"ReadAfterWrite"` | `Tx.${method}() cannot read tables after the first table write` | 事务在首个表写之后读表 |
| `StorageRequestError` | `"StorageRequestError"` | 由存储层给出 | 只失败该次读，不失败 Session；从 `commit()` 抛出则与其他错误一样致命 |
| `SessionFailed` | `"SessionFailed"` | `Session failed after a storage error; close and reopen it`（`cause` = 首个错误） | Session 死亡 |
| `ReadOnlySession` | `"ReadOnlySession"` | `Read-only Harness: ${what} refused` | 只读 Harness 拒绝了写或运行 |
| `ConversationBusy` | `"ConversationBusy"` | `Conversation ${conversationId} is busy`（另有属性 `conversationId`） | `whenBusy: "reject"` 的输入到达忙碌会话 |

`errorMessage(error)`：`Error` → `.message`；否则 `String(error)`；`String()` 自身抛错（如 `Object.create(null)`）时退回 `Object.prototype.toString.call(error)`。更细的三分支与 Rust 复刻要点见 **§A.11**。

### 3.2 全部抛出点消息目录（逐字）

harness 目录内（不含兄弟单元）：

| 文件 | 消息（模板） |
|---|---|
| `harness/registry.ts` | `Task ${name} of extension ${extension.name} is already installed`；`Extension ${extension.name} has two tools named ${tool.name}`；`Section key ${JSON.stringify(key)} must match ${SECTION_KEY}`（`TypeError`）；`Section key ${key} is reserved for the agent's instructions`；`Extension ${extension.name} has two sections with key ${key}`（同一扩展内两个任务重名不单独校验：由 `RegistryState` 的 `Task ${name} of extension ${extension.name} is already installed` 覆盖，含与内置任务冲突） |
| `harness/harness.ts`（`Harness.open`） | `Registry lacks built-in tasks ${names}; create it with createRegistry()`（`names` 为缺失的内置任务名以 `", "` 连接，顺序 `kiana.generation, kiana.tool, kiana.compaction`） |
| `harness/agent.ts` | `Wrapper renamed ${target} to ${nameOf(wrapped)}` |
| `harness/generation.ts` | `Conversation ${conversationId} has no entries to send`；`Model ${ref.provider}/${ref.modelId} is not available`；`Tool ${call.name} is not available` |
| `harness/compaction.ts` | `Model ${ref.provider}/${ref.modelId} is not available` |
| `harness/context.ts` | `Entry ${at} is not visible from conversation ${conversationId}` |
| `harness/provider.ts` | `Conversation ${runtime.conversationId} has no provider session ID` |
| `harness/tool.ts` | `Tool ${call.name} is not available`；`Entry ${input.assistant} has no tool call ${input.callId}`；`Tool call ${call.id} has settled`；`Nested call ${call.id} is not in its caller's index`；`Nested call ${call.id} was already made with another tool or other arguments`；`Nested call key ${JSON.stringify(key)} must be non-empty, without "/", not "__proto__", and not a positive integer` |
| `harness/submissions.ts` | `Submission ${id} does not exist`；`Submission ${this.id} does not exist`；`Request ${draft.requestId} already identifies a submission of type ${existing.type}`；`An input entry's kind must be a non-empty string not starting with "kiana.", got ${String(kind)}`（`TypeError`） |
| `harness/scheduler.ts` | 见 §2.15 各处的【逐字】标注。计数口径：源码共 **22 个** `new Error(...)` / `new ReadOnlySession(...)` 构造点，但只对应 **19 条去重后的消息模板**（`Task ${id} does not exist` 出现 **4** 次；`new Error(message)` 3 处由变量拼出；`ReadOnlySession("running tasks")` 的实际文本是基类拼出的 `Read-only Harness: running tasks refused`）。逐条模板见 §2.15，不要在 Rust 侧按"22 条独立消息"去重 |
| `harness/view.ts` | `Not a Harness`；`Conversation ${id} does not exist` |
| `harness/util.ts` | `Harness is closed`（无 Session 失败时）；否则 `new SessionFailed(failure.error)`（`SessionFailed` 的文本由 `errors.ts` 拼出：`Session failed after a storage error; close and reopen it`，`name = "SessionFailed"`） |
| `harness/tool.ts`（**非 throw** 的诊断文本，同样逐字） | `harnessError(code, message)` → `{ output: [], isError: true, diagnostics: [{ severity: "error", code, message }] }`。已知 code/消息对：`tool_unavailable` → `Tool ${call.name} is not available`；`blocked` → `Tool call blocked: ${block}`；`invalid_arguments` → 由 `validateToolArguments` 的错误文本；`aborted` → `Tool ${call.name} was aborted`；`fallbackResult` 用 `outcome.status` 当 code：`faulted` → `Tool ${name} failed: ${outcome.error.message}`、`orphaned` → `Tool ${name} could not resume: ${outcome.reason}`、其余 → `Tool ${name} ended without a result`。截断诊断：`truncated` → 消息 `Output truncated${kept}: ${dropped.droppedLines} lines, ${dropped.droppedBytes} bytes dropped`（`kept` 为 `""` 或 ` to its beginning`/` to its end`，`severity: "warn"`，`code: "truncated"`） |

### 3.3 `ReportInfo`（宿主可见的类型，逐字判别字段）

```ts
export type ReportTask = { id: TaskId; kind: string; conversationId: ConversationId };
export type HostCallback = "now" | "settings" | "registry" | "env" | "models" | "setTimer" | "onWake";
export type ReportInfo =
  | { type: "session_failed" }
  | { type: "task_faulted"; task: ReportTask; extension?: string; reason: "threw" | "restart_limit" }
  | { type: "task_parked"; task: ReportTask; extension?: string }
  | { type: "migration_failed"; task: ReportTask; extension?: string }
  | { type: "task_outdated"; task: ReportTask; extension?: string; reason: "missing_task" | "incompatible_task" }
  | { type: "task_error"; task: ReportTask; extension?: string }
  | { type: "extension_error"; extension?: string; task?: ReportTask; conversationId?: ConversationId }
  | { type: "host_error"; callback: HostCallback; task?: ReportTask }
  | { type: "listener_error" };
export type ReportFrom = { type: "extension"; extension?: string } | { type: "host"; callback: HostCallback };
```

使用位置（必须逐一实现）：
- `session_failed`：`Harness` 在 Session 失败时报告一次（`safeReport`）。
- `task_faulted`：`#step`（`reason: "threw"`）、`#reserve`（`reason: "restart_limit"`）。
- `task_parked`：`open()` 恢复时被 park 的任务。
- `migration_failed`：`#resolve` 迁移抛错。
- `task_outdated`：`#decide` 中定义被卸载（`missing_task`）或被不兼容定义替换（`incompatible_task`）。
- `task_error`：`#start` 捕获（运行期失败）、`runtime.report(err)` 无 `from`。
- `extension_error`：hooks 抛错、`runtime.report(err, {type:"extension"})`、结构化输出无效。
- `host_error`：`now`/`settings`/`registry`/`env`/`models`/`setTimer`/`onWake` 抛错（分别带 `callback`）；可带 `task`。
- `listener_error`：提交/关闭/watch/view/文档监听抛错；`#notifyBlocked` 的监听抛错。

### 3.4 默认设置数值（逐字，必须一致）

| 设置 | 默认 |
|---|---|
| `DEFAULT_RETRY_POLICY` | `{ enabled: true, maxRetries: 3, baseDelayMs: 2000, maxAgentDelayMs: 60000 }` |
| `DEFAULT_COMPACTION_POLICY` | `{ enabled: true, reserveTokens: 16384, keepRecentTokens: 20000, backgroundTokens: 32768 }` |
| `DEFAULT_PROGRESS_POLICY` | `{ partialIntervalMs: 100, outputIntervalMs: 100, persistWithinMs: 1_000 }` |
| `DEFAULT_CONTEXT_RETENTION_MS` | `600_000`（10 分钟，**非导出**） |
| `DEFAULT_RESTART_LIMIT` | `5`（导出） |
| `toolExecution` | `"parallel"` |
| `steeringMode` / `followUpMode` | `"one-at-a-time"` |
| `afterFailedRun` | `"keep"` |
| `documentRetentionMs` | `600_000`（Session 的 `defaultRetention`，`KIANA_DURABLE_DOCUMENT_RETENTION_MS` 可覆盖） |
| `DEFAULT_POLL_AFTER_MS` | `5000`（generation.ts，**非导出**） |
| `DEFAULT_MAX_BYTES` / `DEFAULT_MAX_LINES` | 见 `../truncate.ts`（兄弟单元） |
| `DEFAULT_HARNESS_IDENTITY` | `{ attribution: KIANA_ATTRIBUTION }` |
| `SCAN_PAGE_SIZE` | `256`（`context.ts`、`scheduler.ts`、`task-graph.ts`、`harness.ts` **四份**同名私有常量；`context.ts` 的用于 `scanRange`，另三处用于 `scanAll`。不要提取为共享常量，保持各文件独立） |

`restartLimit(limit)`：`typeof limit === "number" && limit >= 1 ? Math.floor(limit) : DEFAULT_RESTART_LIMIT`（`Infinity` 合法 → 永不 park；`0`、负数、`NaN`、非数字 → 默认 5）。
`documentRetention(ms)`：`typeof ms === "number" && ms >= 0 ? ms : 600_000`。
`persistWithin(ms)`：`typeof ms === "number" && ms >= 0 ? ms : 1_000`。

### 3.5 种类名清单（逐字，磁盘与线协议共享）

**文档 kind**：`kiana.agent`、`kiana.live`、`kiana.inbox`、`kiana.provider`、`kiana.usage`、`kiana.tool.nested-call`（family）。

**任务 name**：`kiana.generation`（v1）、`kiana.tool`（v2；v1 迁移）、`kiana.compaction`（v1）。

**条目 kind**：`kiana.user`、`kiana.assistant`、`kiana.system`、`kiana.tool-result`（`data: { diagnostics: ToolDiagnostic[] }`）、`kiana.reset`（总是 `head: "self"`）、`kiana.compaction`（`data: { reason: "manual" | "threshold" | "overflow" }`）。

**保留 section key**：`instructions`（`INSTRUCTIONS_KEY`）。

### 3.6 telemetry 名（逐字，见 §4.7）

span：`kiana.durable.submit`、`kiana.durable.phase`。
event：`kiana.durable.entry`、`kiana.durable.submission`、`kiana.durable.report`、`kiana.durable.checkpoint`、`kiana.durable.replay`。
metric：`kiana.durable.reports`（counter）、`kiana.durable.phase.duration`（histogram, unit `ms`）、`kiana.durable.tasks.active`（upDownCounter）。

### 3.7 环境变量

| 变量 | 读取处 | 语义 |
|---|---|---|
| `KIANA_DURABLE_DOCUMENT_RETENTION_MS` | **仅测试** `test/setup-document-retention.ts`（不自读于 src） | 若定义则 `setDefaultDocumentRetention(Number(forced))`，使整个套件按"用后立即驱逐"跑。**注意**：源码注释里提到的 `npm run test:evicting` 在 `package.json` 中**并不存在**（只有 `test`/`test:workers`/`bench:*`）；实际入口是 `test` 脚本里的 `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0 vitest --run` 第二遍。src 中**没有**从 `process.env` 读取任何 durable 配置。Rust 端应保留为集成测试的开关，而不是运行时配置。 |

其他与宿主进程相关的读取在 `src/env/node.ts`（`SystemRoot`、`ProgramFiles`、`ProgramFiles(x86)`、`PATH`），属兄弟单元 `env`，不在本单元。

## 4. 外部格式（逐字）

### 4.1 文档定义

| kind | version | scope | history | fork | initial() | checkpointWhen |
|---|---|---|---|---|---|---|
| `kiana.agent` | 1 | conversation | `rewindable` | `asOf` | `{}` | `true` |
| `kiana.live` | 1 | conversation | `latest` | `initial` | `{}` | 无 generation 且无 running 的 tools/nestedTools 槽 |
| `kiana.inbox` | 1 | conversation | `latest` | `initial` | `{ items: [] }` | `items.length === 0` |
| `kiana.provider` | 1 | conversation | `latest` | `initial` | `{ sessionId: uuidv7() }` | `true` |
| `kiana.usage` | 1 | conversation | `latest` | `initial` | `{ models: {}, tools: {} }` | `true` |
| `kiana.tool.nested-call` | 1 | **task** | family | — | `{}` | `false`（随任务退休） |

`kiana.live.checkpointWhen` 的源码注释（逐字语义）：完整 base 恰在"无运行"时：空闲、把 generation 交给 tool round 的提交、工具之间；因此 delta 链最多跨一个 generation 或一轮工具的重叠执行。槽只在 running 时持有 output，所以每个 base 都小；嵌套调用的结果在调用者的任务文档里，绝不在这里。**不要**加 delta 数上限；tool output benchmark 检查这条规则。

### 4.2 `kiana.agent`（`AgentState`）

```ts
{ model?: ModelRef; thinkingLevel?: ModelThinkingLevel;
  extensions?: string[] | { add?: string[]; remove?: string[] };
  tools?: string[] | { remove: string[] };
  modelTools?: string[] | { remove?: string[]; add?: string[] };
  instructions?: string; cwd?: string }
```

`AgentChange`：给定字段替换存储值，`null` 清除，`undefined` 不动。对象形式 `{add, remove}` 的语义：`extensions` 的 `add` 把未安装的扩展追加，`remove` 按名字移除；`tools` 为数组时 `addTools` 追加缺失名，为 `{remove}` 时把 `added` 从 remove 中滤掉；`modelTools` 为未设或对象时把 `added` 追加到其 `add`（加载 deferred 工具，对其他工具无效果）。

### 4.3 `kiana.live`（`LiveState`）

```ts
{ run?: { taskId: TaskId; inputs: SubmissionId[] };
  generation?: { attempt: number; message?: AssistantMessage; retry?: { at: number; error: string }; deferred?: { pollAt: number } };
  tools?: ToolSlot[];
  nestedTools?: NestedToolSlot[];
  compactions?: CompactionStatus[];
  compactionFailure?: CompactionFailure }

ToolSlot = { callId, name, status: "pending"|"running"|"done", output?, droppedBytes?, droppedLines?,
             details?, diagnostics?, taskId?, entry? }
NestedToolSlot = ToolSlot & { parentCallId, parentTaskId, taskId, arguments: JsonObject, summary? }
NestedToolSummary = { isError: boolean; durationMs?: number; usage?: Usage; error?: string }
CompactionStatus = { taskId, reason: CompactionReason, blocking: boolean, attempt: number, retry?: { at, error } }
CompactionFailure = { taskId, reason, error: string }
```

不变量：`run` 恰在忙碌时存在；`tools` 按调用顺序，从带 tool 调用的答案到 generation 的 `tools` phase 结束；`nestedTools` 按创建顺序（父先于子，task ID 递增），调用 settle 时移除其槽；`compactions` 按 task ID 升序；`compactionFailure` 是"自上次产出摘要以来最新的一次失败"，产出摘要的 compaction 清除它，被 abort 或找不到可压缩内容的保留它。

### 4.4 `kiana.inbox`（`InboxState`）

```ts
{ items: InboxItem[] }
InboxItem = { id: SubmissionId; mode: "steer" | "followUp"; content: UserInput; entry?: InputEntry }
          | { id: SubmissionId; mode: "write"; entry: JsonObject }
```

按 ID 升序（数字顺序，不是字典序）。

### 4.5 `kiana.usage`（`UsageState`）

```ts
{ models: Record<string /* `${provider}/${modelId}` */, Usage>;
  tools: Record<string /* tool name */, Usage> }
```

`Usage` 为 kiana-ai 类型：`{ input, output, cacheRead, cacheWrite, totalTokens, cost: { input, output, cacheRead, cacheWrite, total } }`。所有计数为**数字集合**（numeric sets）——`recordUsage` 用 `Object.hasOwn` 检查自有键，因此名为 `toString` 的工具不会污染原型；`totals[key]` 缺失时视为全 0。持久化为 chord 的数值集合（数值键 `{"#add": n}` 形式，见 §5.1）。

### 4.6 `kiana.tool.nested-call`（`NestedCallState`，family，按 key）

```ts
{ taskId?: TaskId<ToolTaskResult>;       // 仅在创建该调用的提交内缺失
  result?: NestedToolExecutionResult }   // 运行中缺失；被 fault/orphan 的调用也缺失
```

每个嵌套调用一个文档，只有该调用写它；随调用者任务退休。`key` 是调用者给的键（默认是自增正整数），也是调用 ID 的第二段：`<callId>/<key>`。

### 4.7 telemetry 契约（逐字，`DURABLE_TELEMETRY_SCHEMA` v1）

**span `kiana.durable.submit`**
- 描述：`Public submission admission and its commit. Inputs start root traces linked to their callers; writes are children. Internal admissions emit phase events only.`
- `parents: { kind: "any" }`
- startAttributes：`"submission.type"`（string, required, values `["input","write"]`）、`"conversation.id"`（number, required, cardinality high）
- endAttributes：`"submission.id"`（number, cardinality high）、`result`（string, values `["queued","placed","done"]`）
- events：`kiana.durable.entry`、`kiana.durable.submission`、`kiana.durable.report`
- status：`{ default: "ok", errorWhen: "Admission failed." }`

**span `kiana.durable.phase`**
- 描述：`One task phase invocation, including its decision commit, never a wait or restart.`
- startAttributes：`"task.id"`（number, required, high）、`"task.name"`（string, required）、`phase`（string, required）、`kind`（string, required, values `["run","abort"]`）、`attempt`（number, optional）、`resumed`（boolean, required）
- endAttributes：`outcome`（string；取值文档：`next=<phase>`, `waiting`, `sleeping`, `done`, `faulted`, `aborted`）、`"tool.name"`（string）、`"tool.call_id"`（string, high）
- events：`kiana.durable.entry`、`kiana.durable.submission`、`kiana.durable.report`、`kiana.durable.checkpoint`（`phase` string required）、`kiana.durable.replay`（`"tool.name"` string required）
- status：`{ default: "ok", errorWhen: "Faulted, failed, and orphaned outcomes use task_faulted, task_failed, and task_orphaned with a diagnostic capped at 500 characters. Aborted outcomes keep status ok." }`

**事件属性**
- `kiana.durable.entry`：`"entry.id"`（number, required, high）、`"entry.kind"`（string, required）
- `kiana.durable.submission`：`"submission.id"`（number, required, high）、`status`（string, required, values `["done","unanswered"]`）、`"answer.entry_id"`（number, optional, high）
- `kiana.durable.report`：`type`（string, required, values 九种）、`reason`（string, optional, values `["threw","restart_limit","missing_task","incompatible_task"]`）、`extension`（string, optional）、`"task.id"`（number, optional, high）

**全部 `description` 字符串（逐字；`defineTelemetrySchema` 只做编译期校验并在运行时原样返回，但这些字符串属于 schema 字面量，Rust 侧应作为 `&'static str` 常量保留）**
- `entry`（=`kiana.durable.entry` 事件）：`A transcript entry was committed; content remains in the transcript.`
  - `"entry.id"`：`Transcript entry ID.`；`"entry.kind"`：`Entry kind.`
- `reportAttributes.type`：`ReportInfo type.`；`reportAttributes.reason`：`Fixed report reason, when present.`；`reportAttributes.extension`：`Registered extension name, when present.`；`reportAttributes["task.id"]`：`Task ID, when present.`
- `report`（=`kiana.durable.report` 事件）：`A report was raised; no error payload is recorded.`
- `submission`（=`kiana.durable.submission` 事件）：`A submission was settled.`；`"submission.id"`：`Submission ID.`；`status`：`Settlement.`；`"answer.entry_id"`：`Answer entry ID.`
- `kiana.durable.submit` 的 `"submission.type"`：`Submission type.`；`"conversation.id"`：`Conversation ID.`；`"submission.id"`：`Admitted submission ID.`；`result`：`Admission result; stale writes report done.`
- `kiana.durable.phase` 的 `"task.id"`：`Task ID.`；`"task.name"`：`Registered task name.`；`phase`：`Invoked checkpoint phase.`；`kind`：`Invocation kind.`；`attempt`：`Checkpoint attempt, when present.`；`resumed`：`First invocation of a task loaded in this process.`；`outcome`：`next=<phase>, waiting, sleeping, done, faulted, or aborted.`；`"tool.name"`：`Called tool name.`；`"tool.call_id"`：`Transcript tool call ID.`
- `kiana.durable.checkpoint` 事件：`The running checkpoint's phase changed while the invocation continued.`；其 `phase`：`Committed checkpoint phase.`
- `kiana.durable.replay` 事件：`A nested tool call was answered from its stored result.`；其 `"tool.name"`：`Called tool name.`
- `kiana.durable.phase.duration` 的 `task`：`Registered task name.`；`phase`：`Invoked checkpoint phase.`；`outcome`：`Phase outcome.`
- `kiana.durable.tasks.active` 的 `task`：`Registered task name.`
- `DURABLE_TELEMETRY_SCHEMA` 的文档注释（`/** … */`）：`Durable's content-free span, event, and metric vocabulary. IDs address durable records, never content.`
- `kiana.durable.phase` 的 `status.errorWhen` 与 `kiana.durable.submit` 的 `status.errorWhen` 已在上文两条 span 的 `status` 行逐字给出（`Faulted, failed, and orphaned outcomes…` / `Admission failed.`）；`status.default` 都是 `"ok"`。

**metric**
- `kiana.durable.reports`：counter，`Reports raised, grouped by their fixed ReportInfo type.`，属性 `type`
- `kiana.durable.phase.duration`：histogram，unit `ms`，`Duration of a phase invocation and its decision commit.`，属性 `task`、`phase`、`outcome`
- `kiana.durable.tasks.active`：upDownCounter，`Live tasks held by this Harness; decremented at terminal settlement or close.`，属性 `task`

**span status 映射**：`status.status === "orphaned"` → message = `reason`；否则 `error.message`；name 为 `task_${status}`（即 `task_faulted`、`task_failed`、`task_orphaned`）；message 经 `ownedSlice(message, 0, 500)`；aborted → `{ status: "ok" }`。

### 4.8 存储契约（本单元消费的部分，逐字来自 `src/types.ts`）

`Session`/`Storage` 提供（`kiana` 的兄弟单元实现，本单元只依赖）：
- `scanTasks({ status }, limit, cursor)` / `scanSubmissions({ status })` / `scanConversations`：分页 `Page<T, Cursor>`，`SCAN_PAGE_SIZE = 256`。
- `task(id)`、`conversation(id)`、`entry(conversationId, id)`、`submission(id)`、`submissionByRequest(conversationId, requestId)`、`doc`、`docFamily*`。
- `commitWith(fn, context, options)`、`readOnLine`、`observeCommits`、`subscribeClose`、`failed`、`fail(error)`、`watchDoc`、`snapshot`、`snapshotAsOf`。
- `Tx`：`task`、`setTask`、`conversation`、`createConversation`、`createTask`、`createSubmission`、`submission`、`submissionByRequest`、`appendEntry`、`entry`、`doc`、`docFamily`、`stagedTasks()`、`stagedConversations()`、`scanTasks`、`scanSubmissions`、`scanEntries`、`scanConversations`。
- `CommitOptions`（逐字）：`{ readonly persistence?: CommitPersistence }`，`CommitPersistence = { kind: "immediate" } | { kind: "deferred"; withinMs?: number }`。
- `commitWith` 的完整签名（`session.ts:305`，逐字）：`commitWith<T>(change: (tx: Transaction) => T | Promise<T>, context: Context, scope?: TransactionScope, options?: CommitOptions): Promise<T>`，`TransactionScope = { conversationId?: ConversationId; taskId?: TaskId }`（`taskId` 会作为 `byTaskId` 打戳到追加的条目上）。
- `readOnLine<T>(job: () => Promise<T>): Promise<T>`：先 `assertUsable()`（同步抛 → rejected Promise），再 `#enqueue(...)`（`#assertHealthy()` 后执行 job）；在线上排队，**不会与提交交错**。
- `fail(error, context?)`：首个错误胜出；设置 `#failure`、reject `#failed`（`SessionFailed(error)`），**同步**运行 `close(BACKGROUND_CONTEXT)`（不等待），然后 `report(error, { type: "session_failed" }, context)`。
- `commit()` = `commitWith(change, context, undefined, options)`。

### 4.9 任务记录（磁盘上的持久形状，逐字）

```ts
TaskRecordBase = { trace?, links?, id, conversationId, kind, version, input, owner?, background: boolean,
                   abortRequested: boolean, abortReason?: "restart", abandonOnRestart?: true,
                   startedAt?, endedAt?, restarts? }
TaskRecord = TaskRecordBase & (
  { state: { status: "pending"|"running"|"waiting"|"sleeping", checkpoint, ... }; memos?: Record<string, JsonValue> }
  | { state: { status: "completing"|"terminal", outcome }; memos?: never })
TaskState = pending { checkpoint }
          | running { checkpoint }
          | waiting { checkpoint, on: TaskId[], policy: "failFast"|"allSettled" }
          | sleeping { checkpoint, until: number }
          | completing { outcome }
          | terminal { outcome }
TaskOutcome = completed { result }
            | failed { error: { message, detail? }, result? }
            | aborted { reason?, result? }
            | orphaned { reason }
            | faulted { error: { message, detail? } }
```

`startedAt` / `endedAt` 由 **Session 打戳**（不是调度器）：首次变为 `running` 时写 `startedAt`（墙钟毫秒），保持跨等待与恢复；变为 `terminal` 时写 `endedAt`。

### 4.10 提交记录（`SubmissionRecord`，逐字）

```ts
SubmissionRecordBase = { trace?, id, conversationId, requestId? }
input  × { queued } | { placed, entry } | { done, entry, answer } | { unanswered, entry?, reason, detail? }
write  × { queued } | { done, entry } | { unanswered, entry?, reason, detail? }
```

### 4.11 条目记录（`EntryRecord`，逐字）

```ts
{ id: EntryId; conversationId; kind: string; model?: Message[]; data?: JsonValue;
  head?: EntryId; edits?: ContextEdit[]; byTaskId?: TaskId }
EntryDraft = Omit<EntryRecord, "id"|"conversationId"|"byTaskId"|"head"> & { head?: EntryId | "self" }
ContextEdit = { target: EntryId; ... }   // 见 context.ts / types.ts；用于覆盖更早的可见条目
```

## 5. 跨单元接口

### 5.1 依赖的兄弟单元

本单元（`src/harness/**`）是 `packages/durable` 的上层；下列文件在**同一包内**但不在本单元范围，Rust 侧须先实现（或同 crate 的兄弟模块）：

| 文件 | 本单元消费的东西 |
|---|---|
| `src/types.ts` | `JsonObject`、`Id`/`ConversationId`/`EntryId`/`TaskId`/`SubmissionId`/`DocumentId`/`Seq`、文档语义类型（`DocDefinition`、`DocFamilyDefinition`、`DocToken`、`DocFamilyToken` 及其会话/任务特化）、`ReportTask`、`HostCallback`、`ReportInfo`、`ReportFrom`、`SetTimer`、`RunningTask`、`NextTaskState`、`PhaseHandler`、`HookRunner`、`JoinPolicy`、`TaskOptions`、`TaskOutcomeError`、`TaskOutcome`、`TaskState`、`TaskRecordBase`、`TaskRecord`、`ContextEdit`、`EntryRecord`、`EntryDraft`、`TypedEntry`、`TypedEntryDraft`、`Entry`、`SubmissionRecordBase`、`SubmissionRecord`、`SettledSubmissionRecord`、`Submission`、`SettledTask`、`AnyTask`、`DocumentRecord`、`DocumentCreate`、`TaskRuntime`、`Tx`、`Session`、`CommitOptions`、`CommitPersistence`、`CommitPublication`、`CommitChange`、`TableCommitChange`、`DocumentCommitChange`、`DocumentObserver`、`DocumentReader`、`DocumentWatch`、`Storage`、`Page`、`Cursor`、`ScanOrder`、`EntryQuery`、`ConversationQuery`、`DocumentQuery`、`SubmissionQuery`、`ConversationRecord`、`ConversationOwnership`、`ExecutionEnv` 相关 |
| `src/documents.ts` | `defineDoc`、`defineDocFamily` |
| `src/entries.ts` | `defineEntry`、`UserEntry`、`AssistantEntry`、`SystemEntry`、`ToolResultEntry`、`ResetEntry`、`CompactionEntry` |
| `src/tasks.ts` | `defineTask`（`Task` 定义构造器；`definition` 含 `name`、`version`、`initial()`、`phases`、`abort?`、`migrate?`） |
| `src/errors.ts` | `errorMessage`、`ReadAfterWrite`、`StorageRequestError`、`SessionFailed`、`ReadOnlySession`、`ConversationBusy` |
| `src/ids.ts` | `idFromNumber`、`seqFromNumber` |
| `src/timers.ts` | `nativeTimer`、`Timers`、`hostTimers`、`MAX_TIMER_DELAY = 2_147_483_647` |
| `src/truncate.ts` | `DEFAULT_MAX_BYTES`、`DEFAULT_MAX_LINES`、`utf8ByteLength`、`truncate` 相关 |
| `src/session/session.ts` | `SessionImpl`、`contained`、`documentRetention`、`persistWithin`、`setDefaultDocumentRetention`、`closedError` 的语义来源（`closedError` 实际在 `harness/util.ts`） |
| `src/session/transaction.ts` | `Transaction`、`TaskCreationOptions` |
| `src/env/index.ts` | `ExecutionEnv`、`ShellOutputSkip`、`ShellOutputWindow` |

### 5.2 来自 `@earendil-works/chord`（`kiana-chord`）

| 导入 | 使用处 | 语义要点 |
|---|---|---|
| `Context`（类型） | 几乎每个文件 | 取消/传播/telemetry 的载体；`withAbortSignal`、`withoutAbortSignal`、`awaitWithContext`、`withTelemetry` 来自 `@earendil-works/chord/context` |
| `JsonValue`、`JsonRepresentation` | 全单元 | 持久 JSON 值类型。`JsonRepresentation<T>` 是 JSON 安全表示（`undefined` 字段被省去） |
| `copyJson` | agent/generation/inbox/scheduler/submissions/tool/usage（7 个文件） | **深拷贝并丢弃 `undefined` 属性**；`omitUndefinedProperties` 语义。Rust：`serde_json::Value` 的自然行为即如此，但**对象键序**需保留插入序（见 §6.4） |
| `Draft<T>` | agent/compaction/generation/harness/inbox/json/live/partial-changes/prompt/submissions/tool/types/usage | 可变文档草稿类型（Chord 的 `tx.doc()` 返回 `Draft`，只有被重新赋值的叶子才产生操作） |
| `Watch`、`NativeWatch`、`WatchHandle`、`WatchEnd`、`Op`、`Path` | `Watch`：task-graph/view（`new Watch(value, release, onError)`）；`NativeWatch`：仅 events；`WatchHandle`：task-graph/view/types（`ConversationWatch`/`TaskGraphWatch` 是 `WatchHandle<T>` 别名）；`WatchEnd`：仅 events；`Op`/`Path`：events/json/task-graph/view（`json.ts` 只用 `appendText`/`Draft`，**不**用 Watch） | 文档 watch、视图序列化操作 `["s", path..., value]` / `["d", path...]` / `["a", path, text]` 等；`NativeWatch` 有 pending 批次上限（events 用 100） |
| `appendText`（`chord/delta`） | json.ts | 追加字符串时产生 append 操作而非整串 set |
| `overlap`（`chord/delta`） | 仅 tool.ts（`overlap(written.text, snapshot.text, 65_536)`，即 `overlap(a, b, scan, probe = 64, maxCandidates = 8)`） | 尾部窗口滑动时找共享前缀（返回 `a` 的后缀与 `b` 前缀的最长重叠长度） |
| `ownedSlice`（`chord/delta`） | scheduler.ts（`ownedSlice(message, 0, 500)`）/ tool.ts（`ownedSlice(errorTextOf(result, output), 0, MAX_SUMMARY_ERROR_CHARS)`） | **不是**"安全切片"：源码为 ``return `${text.slice(start, end)} `.slice(0, -1)``——即用 `String.prototype.slice` 按 UTF-16 code unit 切（**可以拆开代理对**），再用模板串拼接强制生成扁平拷贝（避免 V8 的 sliced string 让槽位继续引用大输出），最后去掉补的空格。用于 500 字符错误上限与 `NestedToolSummary.error` 上限；Rust 实现必须保留"按 code unit 切 + 强制拷贝"两点，**不要**加"不拆代理对"的额外保护（那会与 TS 行为不一致） |
| `boundOutput`、`OutputBuffer`、`OutputLimits`、`PROGRESS_BYTES_PER_SECOND`、`Progress`、`ProgressClock`（`chord/output`） | tool.ts | 工具输出截断与进度节流 |

### 5.3 来自 `@earendil-works/kiana-ai`（`kiana-ai`）

| 导入 | 使用处 | 说明 |
|---|---|---|
| `AssistantMessage`、`UserMessage`、`Message`、`SystemMessage`、`ToolCall`、`ToolResultMessage`、`TextContent`、`ImageContent`、`Usage`、`Tool`、`ToolReference`、`SimpleStreamOptions`、`StopReason`、`CacheRetention`、`ClientIdentity`、`Transport`、`TSchema`、`Static`（`StopReason` 只以 `AssistantMessage["stopReason"]` 的形式出现在 `context.ts` 的 `EXCLUDED_STOP_REASONS`） | 逐文件：`context.ts`（`AssistantMessage`/`Message`/`ToolCall`/`ToolResultMessage`）、`prompt.ts`（`Message`/`SystemMessage`/`Tool`/`ToolReference`）、`tool.ts`（`ImageContent`/`TextContent`/`ToolCall`/`ToolResultMessage`）、`generation.ts`（`Api`/`SimpleStreamOptions` 等）、`compaction.ts`（`SimpleStreamOptions`）、`events.ts`/`live.ts`/`types.ts`/`usage.ts`/`scheduler.ts`/`harness.ts`/`partial-changes.ts`/`define.ts`（各自子集）；`TSchema`/`Static` 在 `types.ts` 与 `define.ts`，`CacheRetention`/`Transport`/`ClientIdentity` 在 `types.ts` | 消息与用量模型；**`agent.ts` 不 import kiana-ai**；注意 `TSchema`/`Static` 是 kiana-ai 从 `typebox` 重导出的（`export type { Static, TSchema } from "typebox"`） |
| `ModelRef`（**本包 `harness/types.ts` 定义**：`{ provider, modelId }`，不是 kiana-ai 导出）、`ModelThinkingLevel`、`Api`、`Model` | `ModelThinkingLevel`：compaction/generation/types；`Api`：仅 generation（`Api` 是 kiana-ai 的 API 判别联合，用于 `streamSimple` 的选项）；`Model`：compaction/generation/types | `Model` 含 `contextWindow`、`maxOutputTokens` 等；`Agent`/`agent.ts` 本身只用 `ModelRef`，不直接用 `Model` |
| `Models` | harness/scheduler/types（类型）、compaction/generation（调用） | `getModel(provider, id)`、`streamSimple`、`fetchDeferred`、`cancelDeferred`、`completeSimple` 等；`Runtime.models` 是 `() => Models`（每次调用重新取，见 §2.2 的 `currentModels`） |
| `TSchema`、`Static` | `define.ts`（`defineTool<TParameters extends TSchema>`）、`types.ts`（`ToolRegistration`/`prepareArguments`/`structuredOutputSchema`）；**`tool.ts` 不 import 它们**，只用 `typebox/compile` 的 `Compile` | TypeBox 工具参数 schema；`tool.ts` 把 `Compile(schema)` 缓存到 `structuredValidators`（`WeakMap<object, ReturnType<typeof Compile>>`） |
| `utils/overflow` → `isContextOverflow` | 仅 generation.ts | 判断上下文溢出错误 |
| `utils/retry` → `isRetryableAssistantError`、`retryDelayMs` | generation.ts、compaction.ts | 可重试判定与退避时长 |
| `utils/transcript` → `getCurrentTools`、`toToolDeclaration`、`declarationsEqual` | 仅 prompt.ts | 系统提示的工具声明差分（`planTools` → `ToolChanges { toolsRemoved: ToolReference[]; toolsAdded: Tool[] }`）；generation 不直接 import |
| `utils/estimate` → `calculateContextTokens`、`estimateMessageTokens` | 仅 compaction.ts | 上下文 token 估算 |
| `utils/validation` → `validateToolArguments` | 仅 tool.ts | 工具参数校验与修复 |
| `utils/identity` → `ClientIdentity`、`mergeClientIdentity`、`KIANA_ATTRIBUTION`、`withClientIdentity` | 仅 harness.ts | 请求身份与归属头：`DEFAULT_HARNESS_IDENTITY = { attribution: KIANA_ATTRIBUTION }`；`currentModels(options.models, options.identity)` 对裸 `Models` 包一次 `withClientIdentity` |
| `utils/uuid` → `uuidv7` | 仅 provider.ts | `ProviderDoc.initial()` 的 `sessionId` |
| `utils/streaming-json` → `JsonChange`、`StreamingJsonParser` | 仅 partial-changes.ts | 流式工具参数增量 |
| `typebox/compile` → `Compile` | 仅 tool.ts | 编译参数 schema 做校验（`structuredValidators` WeakMap 缓存） |

### 5.4 来自 `@earendil-works/kiana-telemetry`（`kiana-telemetry`）

`defineTelemetrySchema`、`createTypedMetrics`、`createTypedSpanStarter`、`NOOP_TELEMETRY_CONTEXT`、`NOOP_TELEMETRY_METRICS`、`SpanAttributes`、`TelemetryContext`、`TelemetrySpan`、`TelemetryCarrier`。

（更正：`ownedSlice` 来自 `@earendil-works/chord/delta`，不是 telemetry；`scheduler.ts` 的 span status 消息上限 500 字符与 `tool.ts` 的 `MAX_SUMMARY_ERROR_CHARS` 都用它。语义是按 **UTF-16 code unit** 调 `String.prototype.slice(start, end)`（`end` 缺省为 `length`；越界自动截断，**不保证**不拆代理对），随后用模板串 `` `${…} ` `` 再 `` .slice(0, -1) `` 强制生成拷贝。见 §5.2。）

### 5.5 本单元**暴露**给其它单元的类型

`src/index.ts` 导出（harness 部分，按源码顺序）：

```ts
// agent.ts
AgentDoc, configure, DEFAULT_COMPACTION_POLICY, DEFAULT_PROGRESS_POLICY, DEFAULT_RESTART_LIMIT, DEFAULT_RETRY_POLICY
// compaction.ts
type CompactionCheckpoint, type CompactionInput, CompactionTask, type SummaryRequest
// define.ts
defineExtension, defineTool, hook, section, wrapSection, wrapTool
// events.ts
type AgentEvent, type AgentEventStream, type MessageChange, type SnapshotEvent, type ToolEventCall, watchEvents
// generation.ts
type GenerationCheckpoint, type GenerationInput, type GenerationResult, GenerationTask
// harness.ts
DEFAULT_HARNESS_IDENTITY, Harness
// inbox.ts
InboxDoc, type InboxItem, type InboxState
// live.ts
type CompactionFailure, type CompactionStatus, LiveDoc, type LiveState, type NestedToolSlot,
type NestedToolSummary, type ToolSlot
// provider.ts
ProviderDoc, type ProviderState
// registry.ts
createRegistry
// scheduler.ts
type BlockedReason
// task-graph.ts
type TaskGraph, type TaskGraphNode, type TaskGraphState, type TaskGraphWatch
// telemetry.ts
DURABLE_TELEMETRY_SCHEMA
// tool.ts
NestedCallDoc, type NestedCallState, ToolTask, type ToolTaskCheckpoint, type ToolTaskInput, type ToolTaskResult
// usage.ts
UsageDoc, type UsageState
// view.ts
type ConversationView
// harness/types.ts（值：无；类型 57 个）
AfterFailedRun, Agent, AgentChange, AgentState, AnyTask, CompactionHooks, CompactionPolicy, CompactionReason,
CompactionResult, CompactionSettings, ContextView, Conversation, ConversationAbortOptions, ConversationCreateOptions,
ConversationHandle, ConversationInit, ConversationRetryPolicy, ConversationStreamOptions, ConversationWatch, EnvTarget,
Extension, GenerationHooks, HarnessInspection, HarnessOptions, HarnessSettings, HookApi, HookRegistration, HookResult,
HooksOf, InputEntry, InputSubmissionDraft, ModelRef, NestedToolExecutionResult, ProgressPolicy, PromptInput,
PromptSection, QueueMode, Registry, RegistryReader, RegistrySnapshot, Settings, SettledSubmissionRecord, SettledTask,
Submission, SubmissionDraft, TaskInspection, ToolControl, ToolDiagnostic, ToolExecutionApi, ToolExecutionMode,
ToolExecutionResult, ToolHookCall, ToolHooks, ToolRegistration, UserInput, Wake, Wrap
```

未从 `index.ts` 导出、但被兄弟单元或测试直接引用的：`INSTRUCTIONS_KEY`（`agent.ts`，`registry.ts` 用）、`DEFAULT_CONTEXT_RETENTION_MS`（`agent.ts` 私有）、`harness/util.ts` 的 `closedError`/`optionalExtension`/`scanAll`/`Waiters`/`errorMessage` 重导出（`task-graph.ts`、`view.ts`、`events.ts`、`submissions.ts` 用）、`scheduler.ts` 的 `TaskScheduler`/`TaskSchedulerOptions`/`blockedSubject`/`SchedulerOutcome`/`InvocationBinding`/`BlockedSubject`/`installerOf`/`withAbortMark`（`index.ts` **只**导出 `BlockedReason`；`harness.ts`、`task-graph.ts`、`live.ts`、`tool.ts` 从模块直接 import）、`partial-changes.ts` 的 `PartialChanges`（`generation.ts` 用）、`json.ts` 的 `assignJson`/`AssignPlan`（`live.ts`、`tool.ts`、`partial-changes.ts` 用）、`usage.ts` 的 `addUsage`/`addUsageState`/`recordUsage`（`harness.ts` 的 `usage()` 用）、`provider.ts` 的 `ensureProviderSessionId`（`generation.ts`、`compaction.ts` 用）、`context.ts` 的 `readContextFrom`/`ContextRange`（`scheduler.ts` 用）、`task-graph.ts` 的 `TaskGraphView`/`TaskGraphBlocking`（`harness.ts` 用）、`agent.ts` 的 `agentHooks`/`addTools`/`createAgent`/`DEFAULT_*`（`scheduler.ts`/`generation.ts`/`tool.ts` 用）。

**Rust 侧可见性策略**：保持两层 `pub`（包根可见面 = `lib.rs` 的 `pub use`；模块内 `pub` 只给 crate 其余部分用）。TS 里"未导出但被兄弟文件 import"的项在 Rust 用 `pub(crate)`；测试要用的用 `#[doc(hidden)] pub` 或在 `#[cfg(test)]` 下 `pub use`。类型导出共 88 个，其中 `harness/types.ts` 的 57 个名字**全部**被 `index.ts` 以 `export type` 重导出（`harness/types.ts` 共声明 58 个导出类型，唯一未被重导出的是 `Harness`：`types.ts` 的 `export interface Harness` 与 `harness.ts` 的 `export type Harness = HarnessType` 同名，`index.ts` 只按值导出了 `Harness`）。其余 31 个类型来自 `compaction.ts`(3)、`events.ts`(5)、`generation.ts`(3)、`inbox.ts`(2)、`live.ts`(6)、`provider.ts`(1)、`scheduler.ts`(1)、`task-graph.ts`(4)、`tool.ts`(4)、`usage.ts`(1)、`view.ts`(1) = 31。值导出 25 个、顶层导出名合计 113 个（= 25 + 88）。

**Rust 侧建议**：全部从 `kiana_durable::harness` 公开；`index.ts` 的可见性用 `pub use` 在 `lib.rs` 复现（`pub(crate)` 与 `pub` 的边界要与 TS 的"未导出"一致，测试需要访问的用 `#[doc(hidden)] pub` 或 `#[cfg(any(test, feature = "testing"))]`）。

## 6. npm 依赖 → Rust crate 映射

### 6.1 直接依赖（`packages/durable/package.json`，**逐字**）

```json
"dependencies": {
  "@earendil-works/chord": "1.4.0",
  "@earendil-works/kiana-ai": "1.4.0",
  "@earendil-works/kiana-codemode": "1.4.0",
  "@earendil-works/kiana-telemetry": "1.4.0",
  "@silvia-odwyer/photon": "0.3.3",
  "diff": "8.0.4",
  "typebox": "1.3.27"
},
"devDependencies": {
  "@cloudflare/vitest-plugin": "1.4.4",
  "fake-indexeddb": "6.2.6",
  "shx": "0.3.4",
  "vitest": "4.1.12"
}
```
`name = "@earendil-works/kiana-durable"`、`version = "1.4.0"`、`engines.node = ">=22.19.0"`、`type: "module"`、`sideEffects: false`。`exports` 共 18 个键（`.`、`./env`、`./env/node`、`./codemode`、`./tool-search`、`./tools`、`./images`、`./images/node`、`./images/cloudflare`、`./storage/memory`、`./storage/indexeddb`、`./storage/jsonl`、`./storage/jsonl/node`、`./storage/sqlite`、`./storage/sqlite/cloudflare`、`./storage/sqlite/node`、`./testing`、`./package.json`），每项形如 `{ source, types, import, default }`（`"./package.json"` 除外）；Rust 侧只需复现 `.`（harness 公开面）与 `./testing`（`src/testing/index.ts`）。

**本单元（harness）实际用到的**：`chord`（全量语义）、`kiana-ai`（消息/模型/schema/工具函数）、`kiana-telemetry`（schema/span/metric/context）、`typebox`（仅 `tool.ts` 的 `Compile`；`TSchema`/`Static` 只出现在 `define.ts` 与 `types.ts` 的类型标注里，且它们是从 **kiana-ai** 重导出的类型）。**不使用**：`kiana-codemode`、`photon`、`diff`（这三者属于兄弟单元 `codemode`/`images`/`tools`）。

### 6.2 映射表

| npm | 用途 | Rust crate | 差异与桥接 |
|---|---|---|---|
| `@earendil-works/chord` | JsonValue、`copyJson`、`Draft`、delta ops（`appendText`/`overlap`）、Watch/`NativeWatch`、`Context`/`withAbortSignal`/`withoutAbortSignal`/`awaitWithContext`/`withTelemetry`、output 限流 | `kiana-chord`（自研，见 `spec/chord-core.md`、`spec/chord-rest.md`） | **必须**先按其规范实现：`Op` 编码、`copyJson` 丢 `undefined`、`Draft` 的"只有重赋值的叶子才产生操作"语义（Rust 用 `Draft<T>` 包装 + 脏路径追踪，或先 `clone` 后 diff）。`Context` → 自研 `Context`（携带 `CancellationToken` + telemetry span + abort 绑定的组合） |
| `@earendil-works/kiana-ai` | 消息/模型/工具函数 | `kiana-ai`（自研，见 `spec/ai-core.md` 等） | 先实现 `AssistantMessage`/`UserMessage`/`SystemMessage`/`ToolResultMessage`/`Usage`/`Model`/`Models` trait；`utils/*` 的四个函数（overflow/retry/transcript/estimate/validation/identity/uuid/streaming-json）按各自规范逐字复刻 |
| `@earendil-works/kiana-telemetry` | span/event/metric/schema | `kiana-telemetry`（自研，见 `spec/telemetry-mcp.md`） | schema 定义 `defineTelemetrySchema` → 静态 Rust 结构 + 运行时属性校验（可用 const 表 + `&'static str` 常量）；`ownedSlice(s, 0, 500)` 必须按 **UTF-16 code unit** 切并强制拷贝（`slice(start, end)` 可拆代理对；见 §5.2、§6.3） |
| `typebox` | 工具参数 schema | `schemars` / `jsonschema` 生成 + 自写 `Compile` 等价 | 只需要"给定 `serde_json::Value` 参数、按 `TSchema` 校验并修复（`Value.Clean`/`Value.Default`/`Value.Convert`）"。**`validateToolArguments` 的修复结果**（`prepareArguments`、可选字段 `undefined` → 删除、coercion）必须一致；TypeBox 错误文本会进入工具结果（诊断 code `invalid_arguments`，消息即 kiana-ai 抛出的文本，例如 `Invalid arguments for tool "${toolCall.name}": ${toolCall.argumentsError}`），见 §2.11 |
| `vitest` 4.1.12 | 测试运行器 | `cargo test` / `cargo nextest` | 无直接对应；测试语义见 §7 |
| `fake-indexeddb` | IndexedDB 后端测试 | 不需要（wasm 目标单独处理） | |
| `@cloudflare/vitest-plugin` | workerd 目标测试 | 不需要（Cloudflare 适配在 Rust 是 feature 门控） | |
| `node:crypto`（`randomUUID`）| 无（本单元用 `kiana-ai/utils/uuid` 的 `uuidv7`） | `uuid` crate（`v7` feature） | **必须是 UUIDv7**，不是 v4；`ProviderDoc.initial()` 每次调用生成一个 |
| `AbortController`/`AbortSignal` | 取消、绑定上下文、任务 invocation | `tokio_util::sync::CancellationToken` + `tokio::select!` | 需要复刻：`signal.aborted`、`abort(reason)`、监听器立即同步触发、`throwIfAborted()` 抛出 reason（`endedError` 的 Error 对象会被当作 reason）。**`controller.abort(endedError(...))` 让绑定该 signal 的等待以该 Error reject**——Rust 用 `CancellationToken` + `AbortReason` 槽位复刻 |
| `queueMicrotask` | 调度器/存储/唤醒的延迟提交 | `tokio::task::yield_now()` 或自建 microtask 队列 | **关键**：顺序必须是"当前同步任务结束后立即执行，且先于任何 I/O"。Node 的 microtask 在**每个** await 点后清空；Rust 的 `yield_now` 只让出一次。桥接：自建 `MicrotaskQueue`（`RefCell<VecDeque<Box<dyn FnOnce()>>>` + 单线程 executor 的 `drain()`），在每次 `await` 边界后 drain，以复现"绝不在提交/注册监听里同步提交"这一不变量 |
| `setTimeout`/`clearTimeout` | 前台/后台计时器 | `tokio::time::sleep` + `JoinHandle::abort` | 后台计时器必须"不阻止进程退出"：Rust 即"不 join、drop 即取消"；前台计时器需要保活（用 `tokio::spawn` 持有 handle）。`MAX_TIMER_DELAY = 2_147_483_647` 的分段等待要复刻 |
| `performance.now()` | 阶段耗时直方图 | `std::time::Instant` | 精度差异可接受（metric 值不同不算可见差异，除非测试断言） |
| `Date.now()` | `Harness.now` 默认 | `std::time::SystemTime` | 必须是 epoch 毫秒（number）。宿主注入 `now` 的路径优先 |
| `structuredClone` | `usage.ts`（`addUsageState`） | `serde_json::Value` / `Usage` 的普通 `clone()` | `Object.defineProperty(sum[bucket], key, { value: structuredClone(usage), enumerable: true, writable: true })`：仅当该桶（`models`/`tools`）里还没有该键（含工具名 `__proto__`/`toString` 时）才深拷贝一份 `Usage` 放进去。JS 的 `structuredClone` 是深拷贝且保留普通对象；Rust 无原型、无 `defineProperty` 语义，直接 clone 即可 |
| `Object.freeze` | 上下文视图/kept entries 冻结 | 不可变所有权 | `harness-context.test.ts` 断言"深冻结 kept entries，即使 storage 只浅冻结"——Rust 用 `Arc<Value>` 不可变即天然满足；测试改为断言不可变性（无 `&mut` 路径） |
| `WeakMap`/`WeakSet`/`WeakRef` | `context.ts` 的 `deeplyFrozen`（`WeakSet<object>`）、`view.ts` 的 `VIEWS`（`WeakMap<object, ConversationViews>`）、`tool.ts` 的 `structuredValidators`（`WeakMap<object, Compile>`）；**无 `WeakRef`** | `Arc` / 不缓存 | 见 §1.4 第 8 条与 §2.5.14。`VIEWS` 让 `conversationViews(harness)` 从一个 harness 对象取回它的挂载（不是 Harness 时报 `Not a Harness`）；Rust 用 `Harness` 上的字段或 `Weak` 指针 |
| `Map`/`Set` 插入序 | 调度器索引（`#live`、`#below`、`#runnable`、`#intent`、`#settled`、`#edges`、`#dropQueue`） | `IndexMap`/`IndexSet`（`indexmap` crate） | **必须**保持插入序：`wake()`、`#sweep()`、`#ownedLive()` 的遍历顺序影响 `wake.at`、报告顺序、`indexSizes`。用 `indexmap::IndexMap`/`IndexSet`，不要用 `HashMap` |
| `JSON.stringify`（错误消息里的 `JSON.stringify(key)`） | `Section key ${JSON.stringify(key)}`、`Nested call key ${JSON.stringify(key)}` | `serde_json::to_string` | 字符串转义的差异：JS 不转义 `/`，转义 `"`/`\`/控制字符（`\u0000`–`\u001f` 用 `\b\f\n\r\t` 或 `\uXXXX`），对非 ASCII **原样输出**（不转 `\uXXXX`）；`serde_json` 默认同样不转非 ASCII，但会转义 `/`?（不会）。需逐例对齐（`"__proto__"`、`"1"`、`""`、`"a/b"`） |

### 6.3 JS 语义桥接点（必须逐项复刻）

1. **UTF-16 索引**：`String.prototype.length`、`slice`、`charCodeAt`、`JSON.stringify` 的转义、`ownedSlice` 的 500 字符上限（注意它**可能切在代理对中间**，Rust 的 `utf16_slice` 须允许这种切法，返回的 `Vec<u16>` 可能半对）、`truncate` 的字节/行数、`boundOutput`、`messageText` 的长度判断——全部按 **UTF-16 code unit** 计数。Rust 的 `str` 是 UTF-8 字节 + `char` 边界。桥接方案：内部保留 `Vec<u16>` 或提供 `JsString` 包装（`Vec<u16>` 与 `String` 双视图），所有"长度/切片"API 走 UTF-16；渲染到磁盘/线协议再转回 UTF-8。**特别注意** `estimateMessageTokens`、`truncate` 与 tool output 的 `droppedBytes`（后者是 UTF-8 字节，不是 UTF-16）。
2. **正则方言**：`SECTION_KEY = /^[a-z][a-z0-9_-]*$/`（简单，可用 `regex` crate 等价）；`/^[1-9][0-9]*$/`（checkKey）；`abortReason` 等无正则。`String.prototype.startsWith`（`kind.startsWith("kiana.")`、`startsWith("kiana.durable")`）→ `str::starts_with`（UTF-8 安全）。**注意** `kiana.` 前缀检查按 code unit 等价于按字节（ASCII）。
3. **JSON 键序**：对象键按**插入序**序列化（chord 的文档值与 `serde_json::Value` 默认 `BTreeMap` 不同）。必须启用 `serde_json` 的 `preserve_order` feature（底层 `indexmap`）。`copyJson` 丢 `undefined` → Rust 用 `Option` + `skip_serializing_if = "Option::is_none"`，或显式删除 `Value::Null`（**注意**：JS 的 `copyJson` 删除 `undefined` 属性，但**保留** `null`；Rust 的 `Option::None` 与 `Value::Null` 必须区分——建议持久层统一用 `Value`，`None` 不出现）。
4. **数字格式化**：JS 的 `Number` → 字符串规则（`1e21`、`0.000001`、`-0`、整数无 `.0`）与 `serde_json` 的 `f64` 格式化不同。桥接：所有数字先判 `fract() == 0 && abs < 1e21` → 整数格式；否则用 `ryu` + JS 规定的指数阈值（`>=1e21` 或 `<1e-6` 用指数，且指数带符号）。**涉及**：`JSON.stringify` 写盘、telemetry 属性、错误消息里的数字。
5. **`Date` 格式化**：本单元**不**格式化日期（只存 epoch 毫秒数字）。无桥接需求。
6. **`String(value)`**：`errorMessage` 的第三分支、`String(kind)`、`String(key)`、模板字符串插值（`${id}` 对 number 即 JS 数字格式）。Rust 需要 `js_to_string(&Value) -> String`：`null`→`"null"`、`true`/`false`、number 按 §6.4、string 原样、array → 逐项 `js_to_string` 逗号连接（`null`/`undefined` 项为空串）、object → `"[object Object]"`。
7. **`Object.hasOwn` / 原型链**：`Object.hasOwn(memos, name)`、`Object.hasOwn(totals, key)`、`Object.keys`、`Object.entries`。Rust 的 `Map` 无原型，天然安全；但 `kiana.usage.tools` 里工具名可能是 `"toString"`、`"__proto__"`（测试 `keeps tools named like object prototype keys`），必须确保**任何**按名字索引都走 map 查找而非"属性存在即真"（Rust 无此问题，但 `__proto__` 键在 `serde_json` Map 里合法）。
8. **`Array.prototype.includes`/`findLast`/`filter`/`some`/`sort`**：`installerOf` 用 `findLast`（**最后一个**安装该任务的扩展）；`snapshot.installed()` 是安装序。Rust 用 `iter().rev().find(...)`。`ownedTasks` 按 id 升序排序（数字）。
9. **`Promise.withResolvers`**：`#createInvocation` 的 `done`/`finish`。Rust 用 `tokio::sync::oneshot` + 共享句柄，或 `futures::future::Shared` + `Notify`。
10. **`Promise.allSettled`**：`join()`。Rust 用 `futures::future::join_all`（不早退）。
11. **`Promise.race([run.done, session.failed])`**：`abort()` 等待第一个。Rust 用 `tokio::select!`。
12. **`structuredClone` 与对象身份**：扩展用对象身份（`registry.uninstall(extension)` 按名字，`installed.tasks?.includes(task)` 按**对象引用**）。Rust 用 `Arc` 指针相等（`Arc::ptr_eq`）或稳定的 `ExtensionId`/`TaskId` 标记；`installerOf` 的 "includes" 语义必须按**同一 Arc**判断，不能按 `name`。
13. **异常传播**：JS 的 `throw` 可以抛任意值（`unknown`），`#step` 的 `decision.fault` 可以是任何值；`errorMessage` 处理它。Rust 用 `Box<dyn Any + Send>` 或统一 `ErrorValue` 枚举（`Message(String)` / `Json(Value)` / `Object`）。**报告里 `fault.error.message`** 来自 `errorMessage`。
14. **`Error.cause`**：`SessionFailed(cause)`。Rust 用 `thiserror` + `source`。
15. **`Array.prototype.sort` 稳定性**：`ownedTasks` 显式按 id 比较（稳定）；`installed()` 不排序。Rust 用 `sort_by_key`（稳定）。

### 6.4 建议 Cargo 依赖

```toml
[dependencies]
kiana-chord = { path = "../chord" }
kiana-ai = { path = "../ai" }
kiana-telemetry = { path = "../telemetry" }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order", "raw_value"] }
indexmap = { version = "2", features = ["serde"] }
uuid = { version = "1", features = ["v7"] }
tokio = { version = "1", features = ["sync", "time", "macros", "rt", "rt-multi-thread"] }
tokio-util = { version = "0.7", features = ["rt"] }         # CancellationToken
futures = "0.3"
thiserror = "2"
regex = "1"
ryu = "1"
js-sys = "0.3"          # 仅 wasm 目标需要（String.length 等）
wasm-bindgen = "0.2"    # 仅 wasm
tracing = "0.1"         # 可选：本地调试；telemetry 走 kiana-telemetry
```

`typebox` → 无直接 crate；`schemars`/`jsonschema` 只作校验器底座，**修复**（Clean/Default/Convert）需自写。`diff`、`photon`、`kiana-codemode` 属于兄弟单元，本单元不依赖。

## 7. 测试清单

全部 harness 测试位于 `/home/ctyun/code/kiana_rev/kiana/packages/durable/test/`：**40** 个 `harness-*.test.ts`、**24,900** 行、**695** 个 `it` 调用点（694 个普通 `it(...)` + 1 个 `it.each(["faulted","failed","orphaned"])`；`it.each` 展开后实际跑 **697** 个用例）、**98** 个 `describe`（无 `it.skip`/`it.only`/`it.todo`，也没有 `test(...)` 调用点）。另有 `harness-support.ts`、`task-support.ts`、`session-support.ts`、`chat-support.ts` 四个共享夹具（`chat-support.ts` 被 25 个 harness 测试文件 import、`harness-support.ts` 被 23 个）、`test/examples/` 的 31 个可运行示例，以及 `test-workers/` 的 workerd 目标测试（4 个用例，见 §A.12）。

运行方式（`package.json` 的 `test` 脚本，逐字）：

```
vitest --run && KIANA_DURABLE_DOCUMENT_RETENTION_MS=0 vitest --run && npm run test:workers
```

`vitest.config.ts`（逐字要点）：`test.environment = "node"`；`test.exclude = [...configDefaults.exclude, "test-workers/**"]`；`test.setupFiles = ["./test/setup-document-retention.ts"]`；`resolve.conditions = ["source"]`；别名 `^@earendil-works/kiana-durable$` → `./src/index.ts`、`^@earendil-works/kiana-durable/testing$` → `./src/testing/index.ts`（两者都用 `fileURLToPath(new URL(..., import.meta.url))` 解析为绝对路径）；`ssr.resolve.conditions = ["source"]`。另有 `vitest.benchmark.config.ts`（`bench:storage` 用）：`test.environment = "node"`、`test.benchmark.include = ["test/**/*.bench.ts"]`、`test.benchmark.reporters = ["verbose"]`，**不**设 `setupFiles`/`exclude`/别名。

### 7.1 文件规模

| 文件 | 行数 | it 数 | describe 数 |
|---|---:|---:|---:|
| `harness-blocked.test.ts` | 515 | 12 | 2 |
| `harness-callers.test.ts` | 144 | 5 | 1 |
| `harness-cancellation-barrier.test.ts` | 235 | 4 | 1 |
| `harness-compaction.test.ts` | 2458 | 96 | 17 |
| `harness-context.test.ts` | 982 | 22 | 1 |
| `harness-conversations.test.ts` | 477 | 12 | 3 |
| `harness-deferred-progress.test.ts` | 444 | 11 | 4 |
| `harness-events.test.ts` | 779 | 18 | 2 |
| `harness-failed-run-inbox.test.ts` | 82 | 4 | 1 |
| `harness-generation-recovery.test.ts` | 390 | 9 | 1 |
| `harness-generation.test.ts` | 740 | 26 | 1 |
| `harness-host-inputs.test.ts` | 125 | 4 | 1 |
| `harness-inbox.test.ts` | 1092 | 37 | 2 |
| `harness-input-entries.test.ts` | 456 | 11 | 2 |
| `harness-inspect.test.ts` | 162 | 2 | 1 |
| `harness-lifecycle.test.ts` | 598 | 14 | 4 |
| `harness-live-deltas.test.ts` | 455 | 11 | 1 |
| `harness-nested-tools-restart.test.ts` | 729 | 11 | 1 |
| `harness-nested-tools.test.ts` | 1344 | 33 | 2 |
| `harness-ownership.test.ts` | 1378 | 37 | 2 |
| `harness-partial-changes.test.ts` | 215 | 7 | 1 |
| `harness-prompt.test.ts` | 274 | 9 | 2 |
| `harness-read-only.test.ts` | 144 | 4 | 1 |
| `harness-registry.test.ts` | 303 | 10 | 3 |
| `harness-reports.test.ts` | 225 | 4 | 1 |
| `harness-scheduler-indexes.test.ts` | 259 | 5 | 2 |
| `harness-scheduler-retention.test.ts` | 583 | 14 | 1 |
| `harness-sleep.test.ts` | 451 | 13 | 1 |
| `harness-storage-failure.test.ts` | 649 | 24 | 2 |
| `harness-structured-output.test.ts` | 407 | 10 | 1 |
| `harness-structured.test.ts` | 1920 | 56 | 11 |
| `harness-submissions.test.ts` | 278 | 10 | 1 |
| `harness-task-graph.test.ts` | 359 | 5 | 1 |
| `harness-tasks-recovery.test.ts` | 891 | 24 | 5 |
| `harness-tasks.test.ts` | 1366 | 36 | 5 |
| `harness-telemetry-options.test.ts` | 24 | 1 | 1 |
| `harness-telemetry.test.ts` | 988 | 22 | 1 |
| `harness-tools-recovery.test.ts` | 405 | 9 | 1 |
| `harness-tools.test.ts` | 1220 | 39 | 6 |
| `harness-view.test.ts` | 354 | 14 | 1 |
| 合计 | 24900 | 695 | 98 |

### 7.2 共享夹具

**`test/harness-support.ts`（158 行）** 导出（12 个，含类型 `Installed = { dispose(): void }`）：
- `tool(name, description = \`${name} tool\`)` → `defineTool({ name, description, parameters: Type.Object({}), execute: async () => ({ output: [] }) })`。
- `openHarness(storage, toolNames = [], options = {})`：`createRegistry()`；`toolNames` 非空时安装名为 `"tools"` 的扩展（每个名字一个工具）；`Harness.open(storage, { models: createModels(), registry, ...(onReport ? { onReport } : {}) }, context)`；返回 `{ harness, registry }`。
- `user(text)` → `{ role: "user", content: text, timestamp: 1 }`。
- `assistant(text, { calls = [], stopReason })`：`content` = 一个 text + 每个 call 的 `{ type: "toolCall", id, name: \`tool-${id}\`, arguments: {} }`；`api/provider/model` = `"faux"`；usage 全 0（`input/output/cacheRead/cacheWrite/totalTokens` 与 `cost.{input,output,cacheRead,cacheWrite,total}`）；`stopReason` 默认 `calls.length > 0 ? "toolUse" : "stop"`；`timestamp: 2`。
- `toolResult(id, text = \`result ${id}\`)` → `{ role: "toolResult", toolCallId: id, toolName: \`tool-${id}\`, content: [{ type: "text", text }], isError: false, timestamp: 3 }`。
- `system(sections)` → `{ role: "system", content: "", sections, timestamp: 4 }`。
- `describeMessage(message)`：`user:${content}` / `assistant:${首个 text}` / `result:${toolCallId}:${isError ? "error" : text}` / `system:${Object.keys(sections).join(",")}`（测试断言用的紧凑渲染）。
- `addTool(registry, tool, name = \`tool:${tool.name}\`)`、`addTask(registry, task, name = \`task:${task.definition.name}\`)`、`addHooks(registry, task, handlers, name = \`hooks:${++hookExtensions}\`)`、`addSection(registry, key, render, options?)`（`name = \`section:${key}\``）：均返回 `Installed`（`{ dispose(): void }`），dispose 调 `registry.uninstall(extension)`（按名字卸载，与对象无关）。

**`test/task-support.ts`（132 行）** 导出（9 个）：类型 `Deferred<T>`；`deferred<T>()`（`{ promise, resolve, reject }`）；`aborted(signal)`（signal 已 abort 则立即以 `signal.reason` reject，否则监听一次 abort；用于"阻塞到被取消"的 handler）；`eventually(check)`（最多 200 次 `flush()` 轮转，超时抛 `Condition was not reached`）；`settled(promise)`（flush 后是否已 settle）；`openTasks(...)`；`completed(result)` / `abortedWith(reason)`（任务结局构造）；`countingReader(registry)`（返回 `RegistryReader & { subscriptions(): number }`）。它从 `session-support.ts` 取 `context` 与 `flush`。

**`test/session-support.ts`（180 行）** 导出（9 个）：`context = BACKGROUND_CONTEXT`（本单元测试全部由此拿 `Context`）；类型 `Gate = { readonly entered: Promise<void>; release(): void }`；`ControlledStorage extends MemoryStorage`（可观察提交、可扣住调用、可注入提交失败）；`openTestSession()`；`documentChanges(publication)`、`documentCopyChanges(...)`、`observeCommits(...)`（提交变化的观测辅助）；`createConversation(session)`；`flush()`（`new Promise(resolve => setTimeout(resolve, 0))`，即等一轮**宏**任务，不是 microtask）；以及一个**非导出**的 `deferred()`（`{ promise, resolve }`，与 `task-support.ts` 的同名函数不同：**没有 reject**）。

**`test/setup-document-retention.ts`（7 行）**：见 §3.7。**`test/chat-support.ts`（121 行）**：见 §A.2（`ChatSetup`/`chatSetup`/`openChat`）。

**`test/fixtures/`**：`delete-buffer.ts`、`utf8-byte-length-without-buffer.ts`（兄弟单元用）。

**`test/examples/`（31 个可运行脚本，非 vitest 测试）**：`00-conversation.ts`、`01-documents.ts`、`02-forks.ts`、`03-owned-conversations.ts`、`04-watches.ts`、`05-harness.ts`、`06-configuration.ts`、`07-harness-conversations.ts`、`08-context.ts`、`09-registry-reload.ts`、`10-extension-state.ts`、`11-tasks.ts`、`12-recovery.ts`、`13-chat.ts`、`14-system-prompt.ts`、`15-real-model.ts`、`16-coding-tools.ts`、`17-print.ts`、`18-json.ts`、`19-inbox.ts`、`20-late-join.ts`、`21-subagent-foreground.ts`、`22-subagent-background.ts`、`23-child-tasks.ts`、`24-compaction.ts`、`25-coding-agent.ts`、`26-plan-mode.ts`、`27-reviewer.ts`、`28-sandbox-per-conversation.ts`、`29-tool-override.ts`、`30-reload-and-restart.ts`。Rust 侧建议做成 `examples/*.rs`，输出与 TS 逐字一致；其中 `01`、`02`、`08`、`11`、`12`、`18`、`19`、`24` 覆盖本单元核心语义。

**`test-workers/`**（6 个文件）：`wake.test.ts`（4.9K，`onWake`/`resume()` 在 Durable Object alarm 下的行为）、`storage.test.ts`（2.6K，Durable Object SQLite 存储契约）、`worker.ts`（5.9K，`TestSession extends DurableObject`：构造函数里 `Harness.open`，`Reminder` 任务（`runtime.sleepUntil` → `deliver`）、脚本化 faux 模型响应存 storage 以便实例重建后继续、`HEARTBEAT_MS = 60_000`、`onWake` 收到的 `wake` 列表、`hanging` 的 `Promise.withResolvers()`）、`vitest.config.ts`（`cloudflareTest({ wrangler: { configPath } })`，`test.include: ["*.test.ts"]`，`resolve.conditions: ["source"]`，别名同上并把 `@earendil-works/kiana-durable/storage/sqlite/cloudflare` 指到 `../src/storage/sqlite/cloudflare.ts`）、**`wrangler.jsonc`**（310B：`name: "kiana-durable-test-workers"`、`main: "./worker.ts"`、`compatibility_date: "2026-10-01"`、`compatibility_flags: ["nodejs_compat"]`、`durable_objects.bindings: [{ name: "SESSIONS", class_name: "TestSession" }]`、`migrations: [{ tag: "v1", new_sqlite_classes: ["TestSession"] }]`）、**`cloudflare.d.ts`**（2.0K：为 Node 程序声明 `cloudflare:workers` 的 `DurableObjectStorage`/`DurableObjectState`/`DurableObjectId`/`DurableObjectStub`/`DurableObjectNamespace`/`env` 与 `cloudflare:test` 的 `runInDurableObject`/`runDurableObjectAlarm`/`evictDurableObject`；`DurableObject` 类本身由 `packages/codemode/src/cloudflare-modules.d.ts` 声明）。运行方式：`npm run test:workers` = `vitest --run --config test-workers/vitest.config.ts`（`package.json` 的 `test` 脚本第三步）。

### 7.3 各文件断言（标题逐字）

> 标题为 TS 源码字面量（模板串以反引号包裹）。Rust 侧建议一一对应：`crates/kiana-durable/tests/harness_<file>.rs`，函数名蛇形转写，断言内容按标题语义。`(it.each)` 表示该标题在源码里由 `it.each` 对多个参数各跑一遍。

#### `harness-blocked.test.ts`

- describe "recovery at open"
  - it "keeps the host's wake active until open's recovery pass has landed"
- describe "tasks that cannot run"
  - it "parks a task cut off restartLimit times in a row, shows why, and runs it again on retryTask()"
  - it "counts no restart for a task a close stopped"
  - it "keeps an abort mark admitted before close in the record close writes back"
  - it "faults a task whose abort handler is cut off restartLimit times"
  - it "runs a parked task once a raised restartLimit no longer parks it"
  - it "still runs the abort handler of a parked task"
  - it "does not count the same checkpoint committed again as progress"
  - it "starts the count over when the task makes progress"
  - it "tells the graph and the host when a migration fails"
  - it "shows an abandoned task waiting for its definition as blocked"
  - it "shows a task without a definition as blocked, and wakes the host once a registry change unblocks it"

#### `harness-callers.test.ts`

- describe "who may call a tool"
  - it "offers the model only tools it may call, and lets tools call only tools they may call"
  - it "narrows what the model is offered with modelTools, leaving the rest callable by tools"
  - it "never offers a tool that is not enabled, nor lets tools call it"
  - it "answers a model-issued call of a tool it was not offered as unavailable"
  - it "applies addTools to modelTools too, so the model is offered the added tools"

#### `harness-cancellation-barrier.test.ts`

- describe "work below a cancelled owner"
  - it "does not start its next phase while the cascade that marks it has not committed"
  - it "lets a failFast request in the same pass override a restart mark"
  - it "upgrades restart marks down the tree when an abort is requested above them"
  - it "marks a flagged task that holds its outcome, aborting its owned work, and keeps the outcome"

#### `harness-compaction.test.ts`

- describe "range selection"
  - it "keeps about keepRecentTokens and cuts at the first candidate at or after the budget (spec §8.7 example)"
  - it "cuts at a user entry"
  - it "cuts at an assistant in the middle of one long run and never at a tool result"
  - it "never cuts at a display-only entry where the budget is reached (#10407)"
  - it "keeps a huge last tool result together with its assistant"
  - it "never cuts at a system entry or an excluded error or aborted answer"
  - it "follows edited contributions: an omitted entry adds nothing and is no candidate"
  - it "does not cut at a user entry that a result of the preceding call still follows"
  - it "finds nothing when the budget is never reached or only the marker precedes the cut"
  - it "summarizes an earlier summary marker first"
- describe "serialization"
  - it "writes a transcript, truncates tool results, and omits system messages and thinking"
- describe "manual compaction"
  - it "places the summary at once when idle and keeps raw history"
  - it "creates provider state before a legacy conversation's summarization request"
  - it "keeps working while busy and places the summary at the next final boundary"
  - it "places a queued summary at postTools and the run continues in the compacted context"
  - it "runs follow-ups left by a failed run after placing the summary"
  - it "settles stale when a reset lands while it summarizes"
  - it "does not make the conversation busy: a submission during summarization starts its run at once"
  - it "counts the spend of a summary that ends stale, and writes no entry for it"
  - it "lets the compaction that cuts furthest win, whatever finishes first"
  - it "places an older-selected summary that cuts later than the newer one"
  - it `places two queued summaries in one boundary when the second cuts ${order} the first`
  - it "is aborted by Conversation.abort(); an already queued summary survives it"
  - it "is ordinary work: idle waits include it"
  - it "enables scheduling right after open"
- describe "compaction outcomes"
  - it "completes without a summary when there is nothing to compact"
  - it "asks beforeCompact: the first decision wins, a throw is reported and skipped"
  - it "completes without a summary when a hook declines"
  - it "fails with no_model without a configured model"
  - it "retries a retryable error with the pinned request and counts every attempt once"
  - it "adds no usage when a hook declines or supplies the summary"
  - it "caps maxTokens at the model's output limit and sends no tools"
  - it `fails with model_error on ${name}`
- describe "background threshold compaction"
  - it "starts above the background threshold without blocking the run; idle waits and Esc ignore it"
  - it `does not start when ${name}`
  - it "does not start while another compaction is listed"
  - it "records a failure in kiana.live, the snapshot, and compaction_end until a summary is produced"
  - it "stops through abortTask() and Conversation.abort() with background"
- describe "blocking threshold compaction"
  - it "waits for its compaction, which appends the summary before the request"
  - it "sends the request once, without a second compaction, when the kept part is still above the threshold"
  - it "sends the request anyway when its compaction declines"
  - it "fails the run with the compaction's error, without a request, when its compaction fails"
  - it "fails the run when its compaction is aborted directly"
  - it "is aborted with its generation by Esc, before the generation's abort handler"
  - it "wins over a background compaction still in flight, which then settles stale"
- describe "overflow compaction"
  - it "compacts and retries with the same attempt, leaving the error out of the retry"
  - it "fails a second overflow with its error entry"
  - it "fails without compacting when compaction is disabled, even for a retryable-looking overflow"
  - it "fails after a blocking threshold compaction in the same generation"
  - it `fails with the overflow text when compaction ${name}`
- describe "compaction estimates and interactions"
  - it `ignores usage measured before a summary placed mid-run (${clock} clock)`
  - it "rebaselines the system prompt over kept system deltas"
  - it "summarizes a replaced entry's replacement and shows it to the hook"
  - it "compacts a fork whose cut falls on a parent entry"
  - it "settles stale in a fork reset while it summarizes"
  - it "places an older queued summary and the current one together when idle admission drains the inbox"
  - it "places a hook's summary and holds while work the hook created runs"
  - it "removes the status of a faulted compaction"
- describe "compaction events and live status"
  - it "reports start and end, and the retry backoff in a late joiner's snapshot"
  - it "lists concurrent compactions in task ID order"
- describe "compaction recovery"
  - it "repeats nothing after reopen once the summary is placed"
  - it "fails an overflow run with its text when its compaction fails after reopen"
  - it "reruns select and its hook after a crash in select"
  - it "resends an interrupted summarization once and counts only the answered attempt"
  - it "resumes a retry backoff after reopen"
  - it "keeps a generation waiting on its blocking compaction across reopen"
  - it "keeps a queued summary across reopen and places it at the next boundary"
- describe "compaction and the inbox"
  - it "places a reset queued after the summary last, and makes a summary queued after a reset stale"
  - it "places a summary left queued by a failed run at the next submission, before its input"
  - it "keeps the full retry budget after an overflow compaction"
  - it "loses an application edit placed while a compaction summarizes (spec §12)"
  - it "keeps the kept entries mounted in the conversation view"
- describe "compaction edge cases"
  - it "keeps the one-compaction limit through a retry backoff"
  - it "does not start a background compaction when a manual one was admitted during preparation"
  - it "starts no background compaction after a blocking one in the same generation"
  - it "fails the run when its blocking compaction faults, and records the fault"
  - it "fails an overflow run with its text when its compaction is aborted directly or itself overflows"
  - it "treats a length stop as an ordinary answer, not an overflow"
  - it "orders the events of a summary placed at once"
  - it "puts compaction_start last in the batch of the preparation commit"
  - it "keeps a background summary queued through a retry backoff while a blocking compaction wins"
  - it "summarizes the previous summary in a second compaction"
  - it "leaves no usage, submission, or summary when the classifying commit fails, which fails the Harness"
- describe "blocking and manual compaction together"
  - it "places a manual summary selected before the blocking one landed; its equal cut replaces it"
  - it "finds nothing to compact for a manual compaction selected after the blocking summary landed"
  - it "aborts the run and both compactions on Esc and appends nothing"
  - it "places a summary that survived Esc at the next submission, before its input"
- describe "compaction pinning, silent overflow, and late policy changes"
  - it "keeps the pinned model through a retry and advances the live attempt"
  - it "shows a late joiner a compaction that is summarizing"
  - it `treats silent overflow as an ordinary answer: ${name}`
  - it "sends the request when a blocking compaction finds nothing under a policy changed after preparation"
- describe "blocked compaction"
  - it "survives reopen blocked and is orphaned on abort with its status removed"
- describe "context contributions"
  - it "apply edits carried by an older head marker in the range"
- describe "per-model compaction overrides"
  - it "applies the current model's override to the blocking threshold"
  - it "applies the request model's override on a context overflow"
  - it "applies the model's override when a compaction selects and sizes its summary"

#### `harness-context.test.ts`

- describe "conversation context"
  - it "returns the whole transcript without a head and excludes model-less entries from messages"
  - it "excludes aborted, error, and deferred assistant messages but keeps their raw entries"
  - it "resolves self heads and uses the newest head marker"
  - it "applies the newest edit per target within the active range"
  - it "keeps positional system messages and orders tool results by call order"
  - it "leads with a system message that only user messages precede, and keeps later ones in place"
  - it "leads with the baseline after a compaction that keeps assistant messages"
  - it "leads with a baseline written after the first answers, when the agent had no section or tool before"
  - it "keeps system messages other than the baseline in place once an assistant message precedes them"
  - it "does not lead with a first system entry written after another system message, even once that is omitted"
  - it "does not lead with a retained, edited, or later system entry in place of the baseline"
  - it "synthesizes missing tool results after a fork and drops results cut from their call"
  - it "reads the context as of an earlier entry, as a fork at that entry starts"
  - it "extends a task's context read with only newer entries"
  - it "keeps a conversation's context read across its tasks and for the retention period once idle"
  - it "derives the same view from extended reads as from whole reads"
  - it "finds the baseline among the entries an extended read adds"
  - it "keeps returned views and kept ranges apart"
  - it "freezes kept entries deeply even when storage freezes them shallowly"
  - it "does not keep a read that finishes after its task ended"
  - it "drops an idle conversation's context read on a timer when nothing else runs"
  - it "drops a conversation's context read once idle with zero retention"

#### `harness-conversations.test.ts`

- describe "Harness root and conversations"
  - it "creates the root lazily with its agent change and init in one commit"
  - it "keeps root and conversation identity and state across reopen"
  - it "creates independent conversations atomically with init and rolls back failures"
  - it "forks at a concrete entry with the as-of agent and applies agent and init overrides"
  - it "paginates fork-aware history through deep ancestor caps and same-commit prefixes"
  - it "binds commits and task creation to the conversation"
  - it "runs conversationCreated in every creating commit, after the built-ins and before agent and init"
- describe "Harness agent"
  - it "replaces whole fields, clears them with null, and leaves undefined fields alone"
  - it "gives conversations created through Tx their documents: empty, an owner copy, or the fork's as-of copy"
  - it "reads an absent agent for conversations a plain Session created without writing"
- describe "Harness lifecycle"
  - it "returns stateless handles and rejects operations after close"
  - it "forwards generic Session document APIs"

#### `harness-deferred-progress.test.ts`

- describe "generation partials"
  - it "hold partials in memory and store them together, at most every persistWithinMs"
  - it "converts what was stored into the aborted entry after a crash, and resends"
  - it "leaves no aborted entry for a stream that crashed before its first store"
  - it "converts a stored partial once through two crashes, before and after the conversion"
  - it "stores everything held at a clean close"
  - it "never counts partial commits as progress of a task cut off again and again"
  - it "resumes polling a deferred response after a crash"
- describe "tool output"
  - it "answers a tool without replay that crashed with the output stored by then"
  - it "reruns a replay-safe tool after a crash with its stored progress cleared, also through a second crash"
- describe "compaction"
  - it "reruns a summarization cut off by a crash from its checkpoint"
- describe "reading kiana.live in a commit"
  - it "stores the held partial a third-party task's commit read, with what it derived"

#### `harness-events.test.ts`

- describe "agent events"
  - it "streams a run as lifecycle events and text deltas that rebuild the answer"
  - it "reports tool start, output appends, and the result entry"
  - it "reports queued submissions, inbox changes, and retries"
  - it "delivers each of 100 pending batches in its own call, in order, unlike state watches"
  - it "replaces undelivered batches with one snapshot after 100 pending batches"
  - it "rebuilds every committed partial of thinking, text, and tool-call arguments from message changes"
  - it "rebuilds a sliding tail window from output trims and appends"
  - it "emits one exact batch when a run ends and a queued follow-up starts the next"
  - it "ends a call that never runs and a tool aborted with its generation"
  - it "reports a steer without run events, a reset as an appended entry, and nothing for other conversations"
  - it "rejects an attachment cancelled while it waits for the Session line"
  - it "applies deltas after an overflow snapshot that holds an in-flight partial"
  - it "reports usage-only updates, cleared tool progress, tools ending without entries, and deferred polls"
  - it "ends the stream with the Harness"
  - it "starts a message at the first committed partial and ends it with the converted entry on abort"
- describe "streamed partials"
  - it "streams a tool-call partial as toolcall_delta appends at its argument paths"
  - it "commits a growing text and tool call at a cost that follows the new text, not the string lengths"
  - it "keeps a watcher's memory proportional to a long streamed text and tool call, not to their commits"

#### `harness-failed-run-inbox.test.ts`

- describe "inputs queued behind a failed run"
  - it "stay queued by default"
  - it "start the next run with continue"
  - it "place queued writes, and keep them with withdraw"
  - it "settle unanswered with run_failed with withdraw"

#### `harness-generation-recovery.test.ts`

- describe "generation recovery"
  - it "reruns preparation interrupted before its commit"
  - it "resends a request interrupted before any partial without repeating preparation"
  - it "converts a committed partial into an aborted entry and resends the same messages"
  - it "resumes a retry backoff after reopen"
  - it "sleeps again for a retry stored as a running phase by an earlier version"
  - it "resumes polling a deferred response after reopen"
  - it "polls a checkpoint stored without a thinking level and records none"
  - it "fails no_model when the pinned model is gone after reopen, in request and in poll"
  - it "runs a print-style turn and reads the durable answer after reopen"

#### `harness-generation.test.ts`

- describe "generation"
  - it "answers an input and settles its submission"
  - it "stores partials as deltas and a complete base once nothing is in flight"
  - it "still ends a run whose input something else already settled"
  - it "fails with no_model when no model is configured or the model is unknown"
  - it "retries a retryable error after a durable backoff and then answers"
  - it "fails with model_error once retries are exhausted"
  - it "fails a retryable error without retrying when the retry policy is disabled"
  - it "reports section wrapper failures while preparing"
  - it "fails a non-retryable error without retrying"
  - it "polls a deferred response until it is ready"
  - it "converts the committed partial when aborted during streaming"
  - it "cancels a deferred response when aborted during polling"
  - it "reports a failed deferred cancellation and still ends the run aborted"
  - it "forwards stream options and the thinking level"
  - it "records the requested thinking level on answers, streamed and polled"
  - it "attributes model requests to kiana by default and merges the Harness identity over it"
  - it "keeps provider session IDs request-local across concurrent conversations"
  - it "creates and persists provider state before a legacy conversation's request"
  - it "reads settings through getters at every decision"
  - it "resolves settings over the built-in defaults"
  - it "commits partials no more often than progress.partialIntervalMs"
  - it "renders sections that read conversation documents through input.read"
  - it "commits no partial for a response that turns deferred after an empty start event"
  - it "faults a run task, settling its inputs and converting the committed partial"
  - it "orphans a blocked run task with full run cleanup"
  - it "rejects a registry without the built-in tasks"

#### `harness-host-inputs.test.ts`

- describe "HarnessOptions.models and settings as functions"
  - it "generation uses the current return value of a models function at each request"
  - it "still accepts a plain Models object"
  - it "tools read the current models at each access of api.models"
  - it "generation reads the current return value of a settings function"

#### `harness-inbox.test.ts`

- describe "inbox"
  - it "queues busy submissions and places writes before user items at the final boundary, one follow-up per run"
  - it "places every follow-up in one successor run with followUpMode all"
  - it "reads queue modes when the final boundary's commit runs on the Session line"
  - it "adds steers to the run at the postTools boundary and holds follow-ups for the final boundary"
  - it "ends the run at a queued reset after tools and runs earlier follow-ups in the new context"
  - it "places a queued reset after the answer at the final boundary"
  - it "resets an idle conversation at once, with or without handoff text"
  - it "makes a queued head write stale when it targets an entry before the active range"
  - it "makes a head write stale behind a reset placed earlier in the same boundary"
  - it "continues the run in a new context when a tool requests a handoff"
  - it "places queued writes and steers after the handoff text and adds the steers to the continuing run"
  - it "holds a follow-up queued across a handoff until the continuing run's final boundary"
  - it "ends a handed-off run at a queued reset and runs earlier follow-ups after it"
  - it "hands off again in a later round of the same run"
  - it "ends the run after the handoff when the round also terminates"
  - it "drops an onYield continuation when the final boundary selects a follow-up"
  - it "withdraws a queued submission and removes its item"
  - it "leaves queued items after a failed run until the next submission places them in order"
  - it "keeps queued submissions across reopen and settles them afterwards"
  - it "commits inbox changes as positional Chord operations and a base when empty"
  - it "settles a stale write at once while idle, and a queued one behind waiting items"
  - it "keeps an onYield continuation across a queued plain write, with the run's original input"
  - it "drops an onYield continuation for a queued reset"
  - it "adds every steer to the run at the postTools boundary with steeringMode all"
  - it "queues an idle steer behind waiting items and places it with the first follow-up in ID order"
  - it "starts a queued follow-up after a terminating round"
  - it "writes only the last handoff in call order and runs queued follow-ups after the continuing run"
  - it "leaves the inbox alone when the run's task is aborted"
  - it "returns a queued submission for its repeated request ID without a second item"
  - it "withdraws a middle item with one positional removal"
  - it "stores the inbox as a base exactly when it becomes empty"
  - it "keeps a complete inbox base exactly while it is empty, and a usage base on every change"
- describe "usage"
  - it "totals assistant usage per model and tool usage per tool, and sums the Session"
  - it "counts failed attempts, converted partials, and tool usage replaced by afterTool"
  - it "keeps tools named like object prototype keys in the ledger and the Session total"
  - it "keeps usage totals exact across reopen, counting a partial converted after reopen once"
  - it "records usage as numeric sets on the ledger in the entry's commit"

#### `harness-input-entries.test.ts`

- describe "input entries"
  - it "places an idle input as its entry: the given kind and data, with the content as its user message"
  - it "rejects an input entry whose kind is not a non-empty string or starts with kiana., writing nothing"
  - it "takes only kind and data from an untyped caller's entry"
  - it "queues busy inputs with their entries in the inbox and places them as those entries at boundaries"
  - it "withdraws queued inputs with entries, one by one and by conversation abort"
- describe "data-only writes"
  - it "appends a data-only write mid-run in time order, leaving the model's messages unchanged"
  - it "queues a data-only write behind a queued reset, so it lands after the reset and stays visible"
  - it "leaves the request unchanged by a data-only write that lands while the generation prepares"
  - it "appends a data-only write while idle after a failed run without placing the queued items"
  - it "keeps a queued input's entry across reopen and places it as that entry"
  - it "appends a data-only write at once behind a non-empty inbox, and still queues writes with model or edits"

#### `harness-inspect.test.ts`

- describe "Harness.inspect()"
  - it "derives every live task's state without running task code"
  - it "lists unsettled submissions"

#### `harness-lifecycle.test.ts`

- describe "Harness open"
  - it "closes without a cancelled caller context and rethrows the original error when open fails"
  - it "rethrows the open error and reports it once, though closing then fails too"
- describe "Harness close"
  - it "joins a task handler that ignores its signal before closing Storage"
  - it "joins a tool execute and a hook that ignore their signal before closing Storage"
  - it "lets a new Harness open the same Storage once close resolved: no old invocation code runs"
  - it "keeps shutting down after a cancelled close, and a second close awaits the same shutdown"
  - it "settles durably a commit whose committer was cancelled while it was in Storage"
  - it "publishes no frame to watches from a commit that settles during close"
  - it "leaves no subscription behind a watch acquisition cancelled on the line"
  - it "rejects conversation and Harness operations once close begins; inspect queued before reports closing"
  - it "completes reads queued at the seal and rejects queued waits and acquisitions that would follow commits"
- describe "scheduling on a paused Harness"
  - it "never schedules from a read-only viewer"
  - it "schedules from every progress call"
- describe "registry changes before resume"
  - it "runs what the registry holds at resume: a definition installed or replaced after open"

#### `harness-live-deltas.test.ts`

- describe "kiana.live deltas"
  - it "hands a generation over to its tool round and starts a tool with one field write each"
  - it "appends head output and then only updates the dropped counts once the window is full"
  - it "slides a tail window as a front trim plus an append"
  - it "writes the whole window when Chord's overlap search cannot find the shared part"
  - it "diffs details leaf by leaf and appends diagnostics"
  - it "streams partial text as appends"
  - it "stores a complete base exactly in the commits where nothing runs"
  - it "starts calls the request did not offer as done and marks a faulted tool's slot done without an entry"
  - it "commits the tool-calling answer, its tool tasks, the generation's wait, and the tool round in one commit"
  - it "writes an aborted tool's slot with field-level ops"
  - it "keeps a complete base exactly while nothing runs"

#### `harness-nested-tools-restart.test.ts`

- describe "nested tool calls across a restart"
  - it "abandons a replay-unsafe caller's unfinished nested calls before they run again"
  - it "abandons the work an abandoned nested call owns, which never runs a phase again"
  - it "keeps an abandoned task whose definition is missing waiting until it is installed, then runs its abort"
  - it "orphans an abandoned task whose definition is missing once an abort is requested"
  - it "marks nothing when the reopened Harness never starts scheduling"
  - it "lets a replay-safe caller's unfinished nested calls recover and reattaches to them by key"
  - it "clears the slots of nested calls an earlier attempt finished when the rerun makes none"
  - it "aborts a nested call an earlier attempt of a replay-safe caller left running and the rerun never made again"
  - it "reattaches a replay-safe caller by default and explicit keys, and rejects a rerun that calls differently"
  - it "abandons the child tasks a replay-unsafe tool created and awaited in memory"
  - it "leaves background work under an abandoned nested call running, and settles without it"

#### `harness-nested-tools.test.ts`

- describe "nested tool calls"
  - it "stores one document per nested call, keyed by its key, written only by that call"
  - it "rejects a nested call of an aborted caller and leaves no document for it"
  - it "runs a nested call as its own tool task and returns its result to the caller, not the transcript"
  - it "gives each nested call its own task and call ID"
  - it "runs the tool hooks on nested calls, which see their parent"
  - it "returns an error result for an unavailable tool and invalid arguments"
  - it "returns the call a key already names, and rejects reusing the key for another call"
  - it "matches a reused key's arguments regardless of key order"
  - it "aborts the nested calls a caller left running when it returns, keeping their details"
  - it "records a nested call's usage under its tool"
  - it "reports nested calls in kiana.live.nestedTools and as tool events with their parent"
  - it "aborts nested calls with their caller"
  - it "aborts a running nested call when its caller throws"
  - it "reattaches a replay-safe caller to the nested calls it made before a restart"
  - it "stops only the wait when a caller cancels it; the call runs on and its key returns its result"
  - it "admits one nested call for a key used by concurrent calls"
  - it "aborts left-running nested calls of a caller interrupted while it cleaned them up"
  - it "aborts a nested call whose admission was still committing when its caller returned"
  - it "aborts every left-running nested call before waiting for any"
  - it "rejects explicit keys that are empty, have a slash, are __proto__, or are positive integers"
  - it "runs nested calls of nested calls, and ends them children first"
  - it "reports the output of several running nested calls, and shows them to a late subscriber"
  - it "commits only the status of a nested call made with progress: false, and keeps its output in the result"
  - it "loses the running output of a nested call made with progress: false when it is aborted"
  - it "shows a nested call's arguments in its slot: as made, then, unbounded, as it runs with them"
  - it "runs a nested call whose repaired arguments set an optional property to undefined"
  - it "finds each nested call's slot after slots in the middle were removed"
  - it "gives callers a result with its task ID and only diagnostics for a nested call the scheduler faulted"
  - it "gives callers what a schema-less tool streamed before it threw"
  - it "shows a finished nested call as a done slot with its summary until its caller settles"
  - it "migrates version 1 tool task input to a model-issued call"
- describe "tool task version 1 records"
  - it "migrates a pending version 1 task and runs it as a model-issued call"
  - it "migrates a version 1 task interrupted after intent and settles it interrupted"

#### `harness-ownership.test.ts`

- describe "ownership"
  - it "keeps a conversation busy while its owned foreground subtree has live work, holding the completed owner"
  - it "holds an owner for work in a conversation its ended child task owns"
  - it "cascades through ended tasks into an owned background subtree when aborting across background"
  - it "keeps ownership queries off finished subtrees"
  - it "tracks ownership chains deeper than the call stack"
  - it "holds an owner for work a commit creates below ended tasks loaded from storage"
  - it "holds an owner for work created later below ended tasks, without a reopen"
  - it `aborts work below ${depth} ended tasks with their live owner, which waits for it`
  - it `keeps the root busy and aborts with it work below ${depth} ended tasks`
  - it "reaches work below ended tasks across two owned conversations"
  - it `frees an owner that ${ending} once the last work below ended tasks ends after a background task there`
  - it "finalizes many held siblings that end together in one pass"
  - it "rejects a conversation created in the commit that ends its owner task"
  - it "stops idle traversal at a background owner"
  - it "cascades an abort mark to the owned foreground subtree and withdraws its queued inputs"
  - it "cascades a failed owner but not a completed one"
  - it "aborts a background task directly with its ordinary subtree"
  - it "aborts a conversation: queued inputs withdrawn, writes kept, foreground work aborted, background kept"
  - it "aborts work created below a held failed owner, but not below a terminal one"
  - it "withdraws queued inputs below the aborted task but keeps its own conversation's queue and queued writes"
  - it "keeps a nested background owner's subtree when its cancelled background owner is aborted"
  - it "decides idle after reopen from owner edges it has to load first"
  - it `derives marks a crash left unapplied below ${label} at open`
  - it "withdraws an input queued below a held failed owner after its cascade"
  - it "marks work admitted after reopen below a cancelled owner whose edge was not loaded"
  - it "cascades from an owner the scheduler orphans"
  - it "cancels only the caller's wait, never the shared work"
  - it "aborts a waiting child whose awaited task completes in the commit that marks its owner"
  - it "applies marks found through an edge loaded after reopen once a failed cascade commit is reopened"
  - it "fails the Harness on a failed cascade commit, and reopening applies the cascade"
- describe "owned conversations from tools and supervisors"
  - it "gives a tool an invocation-bound handle whose submissions stay durable after the call ends"
  - it "rejects a handle operation queued on the line when the invocation ends before it runs"
  - it "aborts a subagent run with its parent's conversation, rejecting the waiting tool"
  - it "lets a running tool abort its owned child and continue"
  - it "aborts the children of a tool call that throws or is interrupted, while the run continues"
  - it "reruns a replay-safe subagent tool after a restart with the same child and submission"
  - it "lets a background supervisor resubmit after a restart without submitting twice"

#### `harness-partial-changes.test.ts`

- describe "PartialChanges"
  - it "commits tool-call arguments equal to the final parse, at every piece size and commit rhythm"
  - it "handles a number ending and a string starting in one piece, and escapes split across pieces"
  - it "appends growing argument and text strings instead of reassigning them"
  - it "applies the changes of a failed commit in the next one"
  - it "re-applies a failed commit's changes even when it would have added the call"
  - it "falls back to the snapshot when the committed message lost a call whose changes were committed"
  - it "commits the final arguments after the call ends, and drops text appends when a block ends"

#### `harness-prompt.test.ts`

- describe "system prompt preparation"
  - it "renders sections in order with tags, omissions, wrappers, and failures"
  - it "propagates section errors after cancellation"
  - it "replays sections in place, deletes on null, and appends re-additions"
  - it "emits minimal value patches, removals, and additions"
  - it "rewrites order-only changes and re-additions as two entries"
  - it "rebaselines after a head marker, omitting retained deltas on both sides of it"
  - it "writes a complete post-head baseline even when replay already matches"
- describe "tool loadout preparation"
  - it "adds, removes, replaces changed declarations, and rewrites the order when needed"
  - it "puts tool changes on the last section entry and re-declares every tool after a head cut"

#### `harness-read-only.test.ts`

- describe "read-only Harness"
  - it "opens a session with work running elsewhere without writing"
  - it "refuses writes and scheduling, and stays usable"
  - it "refuses to create the root of an empty storage"
  - it "leaves recovery to the next writable open"

#### `harness-registry.test.ts`

- describe "registry"
  - it "installs, replaces in place, and uninstalls extensions by name"
  - it "validates the registry as it would be after an install and publishes nothing when invalid"
  - it "always holds the built-in tasks, which are not an extension"
- describe "settings"
  - it "keeps the default of a progress interval given as undefined"
- describe "agent resolution"
  - it "selects the default, an array, or the default edited by add and remove"
  - it "skips uninstalled names and resolves them again once they are installed"
  - it "replaces same-name tools in place, wraps the winner, then applies the filter"
  - it "drops a tool or section whose wrapper throws or renames it and reports the failure"
  - it "orders sections by extension, replaces same keys in place, and renders instructions last and unwrapped"
  - it "collects hooks of the selected extensions in extension order and applies field defaults"

#### `harness-reports.test.ts`

- describe "reports"
  - it "say which task reported a failure or faulted"
  - it "name the extension whose definition ran, also after another one replaced it"
  - it "name the extension of a failing section or hook, and tell them from a failing environment builder"
  - it "tell extension, listener, wake, and timer failures from a failed Session"

#### `harness-scheduler-indexes.test.ts`

- describe "scheduler indexes"
  - it "takes a cascade's reason from the nearest cancelling owner"
  - it "finalizes a chain of held owners in the commit that frees the last one"
  - it "keeps a scope busy after reopen for work below an ended task whose chain is not loaded yet"
- describe "TaskRuntime.ownedTasks"
  - it "lists the live tasks a task owns directly, in ID order, also after a reopen"
  - it "rejects once its invocation has ended"

#### `harness-scheduler-retention.test.ts`

- describe "scheduler retention"
  - it "forgets ended subagent chains once their work ends"
  - it "holds a live owner for work later created in a forgotten conversation, loading its chain again"
  - it "holds an owner for work its own terminal commit creates below a forgotten chain"
  - it "withdraws an input queued in a forgotten conversation below an abort-marked owner"
  - it "aborts work created in a forgotten conversation below an abort-marked owner before it runs"
  - it "keeps a scope busy while the chain of new work below a forgotten conversation loads"
  - it "keeps a chain when one commit ends its last task and creates new work below it"
  - it "drops what a rejected commit's chain load read, without a later publication"
  - it "drops what a read loaded after reopen, without a later publication"
  - it "withdraws an input queued two ended levels below an owner whose intent appears later"
  - it "forgets a 20,000-deep chain of ended tasks without recursion"
  - it "keeps no more after all work ended than a fresh reopen"
  - it "does not scan queued submissions for an input when nothing cancels"
  - it "lets a task wait on a forgotten ended owner"

#### `harness-sleep.test.ts`

- describe "sleeping tasks"
  - it "sleeps without an invocation and resumes at its checkpoint once due"
  - it "keeps a sleep across reopen"
  - it "resumes a due sleep with its timer without onWake, with background timers declined"
  - it "runs a due sleep at any scheduling pass, before its timer fires"
  - it "disarms the sleep timer when the sleep is aborted and when the Harness closes"
  - it "disarms an invocation's timers when it ends, and a late fire runs nothing"
  - it "arms no timer for sleeps with onWake"
  - it "arms a foreground timer the host declines with setTimeout, reporting it once"
  - it "counts neither a wait on a sleeping task nor an outcome held over one as active"
  - it "runs the abort handler of a sleeping task without waiting for its time"
  - it "rejects a sleep until something that is not a time, and a sleep in an abort handler"
  - it "does not count a task without a definition as active"
  - it "reports a throwing onWake and offers the same wake again at the next pass"

#### `harness-storage-failure.test.ts`

- describe "a failed Storage call"
  - it "ends everything with the first error: the call, later calls, waits, watches, streams, running tasks"
  - it "fails it from a read off the Session line, as a context read makes"
  - it "ends Storage calls underway with SessionFailed, and closes once they have settled"
  - it "closes the backend only after a paged read underway, which cannot start another page"
  - it "fails it when Storage cannot close; an earlier failure stays the cause"
  - it "rejects the Harness's own calls with SessionFailed after a failure"
  - it "ends what a task holds as failed, not as cancelled by its signal"
  - it "fails it from a failed ID mint"
  - it "does not commit what a callback did after it caught a failed read"
  - it "fails it for a StorageRequestError from a commit, whose effect is unknown"
  - it "fails only the call for an invalid request"
  - it "fails it for a throw in its own commit listeners, which would leave memory behind storage"
  - it "fails it for a storage error during a host close, before the backend closes"
  - it "ends an abort that joins a run ignoring its signal, and a reentrant report finds the Session failed"
  - it "does not fail it for a read its caller cancelled"
- describe "listener and report errors"
  - it "reports a listener or onReport whose promise rejects, without an unhandled rejection"
  - it "reports a throwing commit or close listener and runs the others; the commit stands"
  - it "keeps feeding the other observers of a view when one throws"
  - it "contains a fulfilled thenable that calls only its fulfillment handler"
  - it "contains a rejected promise from another realm"
  - it "reports a throwing watch listener, which ends that watch"
  - it "faults only the task, and ends only the watch, for a thrown value that cannot be formatted"
  - it "reports a throwing clock once, also when the report handler reads the clock again"
  - it "keeps working when onReport throws, and falls back to Date.now when the clock throws"

#### `harness-structured-output.test.ts`

- describe "structured output"
  - it "gives callers a schema-less tool's output: one text as a string, one image as itself, else the list"
  - it "gives callers the bounded output, as the model sees it, without the rendered diagnostics"
  - it "stores exactly what callers and events get, without the model's output, and keeps a stored null"
  - it "gives callers a schema tool's validated structured output, which the transcript does not store"
  - it "turns structured output that breaks the schema into an error result for callers, and a report for the model's calls"
  - it "gives callers the output of an error result of a tool without a schema"
  - it "gives callers no structured output for an error result the Harness wrote, only diagnostics"
  - it "gives callers a schema tool's error text as a diagnostic, never as structured output"
  - it "keeps structured output when afterTool replaces the output, and validates one afterTool replaces"
  - it "gives callers of bash its retained output and exit code, also for a nonzero exit"

#### `harness-structured.test.ts`

- describe "task ownership"
  - it "creates a child in its owner's conversation with its owner recorded"
  - it "rejects no ownership, a missing owner, a child in another conversation, and a background child"
  - it "rejects new owned work below an owner that is completing, terminal, or abort-marked"
  - it "cannot create a child in its finishing commit, but work it starts in an owned conversation holds it"
- describe "waiting"
  - it "resumes once every awaited task is terminal and reads their outcomes in order (allSettled)"
  - it `fails fast when a child ends ${ending === "throw" ? "faulted" : "failed"}: its live siblings are aborted, the parent is not`
  - it "fails fast on a held failure before the failing child drains"
  - it "waits with allSettled on tasks it does not own, including already terminal ones"
  - it "rejects waits on itself, its owner, a missing task, and failFast on a task it does not own"
  - it "rejects the wait that closes a cycle"
  - it "rejects the wait that closes a cycle through a held owner"
  - it "rejects a cycle through holds two levels deep"
  - it "lets a background task wait on the owner of its conversation"
  - it "rejects a cycle of a hold and two waits that each look reasonable"
  - it "allows a diamond of waits"
  - it "follows no wait of an abort-marked task, which waits only for its owned work"
  - it "rejects a longer cycle of waits"
  - it "resumes at the next pass when it waits on nothing"
  - it "keeps running phases after spawning and waits on subsets in sequence"
- describe "completing"
  - it "holds a finished task until its owned work drains; waiters and task documents wait for the final commit"
  - it "keeps holding for ordinary work created during the hold"
  - it `aborts the work below ${label}, then finishes with the held outcome`
  - it "only marks a completing task when aborted: the work below is aborted, the held outcome stays"
- describe "abort order"
  - it "runs abort handlers bottom-up across three levels, each after the level below is terminal"
  - it "does not wait for a task it waits on but does not own"
  - it "reports an abort-marked task as waiting for its live owned work"
  - it "faults an abort handler that tries to wait"
  - it "orphans a blocked task only after its owned work drained"
  - it "cascades through task and conversation edges, bottom-up"
- describe "boundaries"
  - it "keeps background work through Conversation.abort(); { background: true } aborts it and waits"
  - it "never cascades from a terminal owner: an aborted subagent's conversation runs new work normally"
- describe "recovery"
  - it "resumes a parent whose awaited children finished before the crash"
  - it "marks failFast siblings a crash left unmarked"
  - it "finalizes a held outcome whose work drained before the crash, and keeps one whose work lives"
  - it "resumes a bottom-up abort a crash interrupted before the cascade"
  - it "reopens a checkout waiting on live payments and finishes it"
- describe "tool rounds"
  - it "owns its tool tasks, waits for them, and hands the run to a conversation-owned generation"
  - it "aborts a parallel round with its generation: tools first, then the generation, with every result written"
  - it "answers the unstarted calls of an aborted sequential round with aborted results, in call order"
  - it "ends a turn at the generation's hold, before its successor's turn starts"
  - it "keeps run control with a faulted generation until its owned work drains, and fails the Harness on a failed final commit"
- describe "definitions and waits"
  - it "keeps a waiting task blocked without its definition and resumes it migrated under a newer one"
  - it "orphans an aborted waiting task without its definition at once, leaving the task it waits on running"
  - it "never migrates a held outcome: a newer definition leaves it and its version alone"
  - it "treats a held task as live: outcomes() rejects, and a task waiting on it resumes at its final commit"
- describe "conversation abort and boundaries"
  - it "marks a held completed task with Conversation.abort(): the work below is aborted, the outcome stays"
  - it "marks and awaits only the background work reached when { background: true } is admitted"
  - it "stops a cascade at an unmarked background task, but not at a marked one"
  - it "fails the Harness on a failed finalization, which reopening applies"
- describe "recovery through ownership edges"
  - it "shows an abort-marked owner waiting for work in its owned conversation before resume after reopen"
  - it "keeps the root busy after reopen for work below a child task's owned conversation"
  - it "keeps a finished background ancestor a boundary after reopen, which { background: true } crosses"
- describe "tool rounds and events"
  - it "ends an aborted sequential round's unstarted calls with their result entries, right before them"
  - it "emits one turn_end per generation, also for a stream attached while it holds"
  - it "lets a tool that owns live work finish its call at the hold while the generation waits for its final commit"
  - it "holds a faulted tool's slot and task_failed until the work it owns drained"

#### `harness-submissions.test.ts`

- describe "submissions"
  - it "appends an idle write and settles it done without a turn"
  - it "places idle input, and rejects busy input with whenBusy reject without writing"
  - it "deduplicates request IDs per conversation before any write"
  - it "reports abort results and looks submissions up by conversation"
  - it "cancels only a wait and rejects pending waits on close"
  - it "rejects a wait whose submission read spans the start of close"
  - it "reacquires a submission after reopen and settles it durably"
  - it "enables scheduling when a caller submits or waits"
  - it "settles submissions by their current record in the transaction"
  - it "appends and reads typed entries through tokens"

#### `harness-task-graph.test.ts`

- describe "task graph view"
  - it "follows every live task through its statuses, owner edges, and owned conversations"
  - it "builds from committed tasks, shows surviving tasks as pending after reopen, and marks aborts"
  - it "lists owned conversations in ID order whatever order one commit creates them in"
  - it "registers nothing for an acquisition cancelled while it waits for the line"
  - it "publishes no revision for a commit that changes no node, and shares one mount between observers"

#### `harness-tasks-recovery.test.ts`

- describe "task recovery"
  - it "resumes an intent/effect/outcome task interrupted after its intent across close and reopen"
  - it "keeps a task's startedAt across close and reopen and stamps endedAt when it settles"
  - it "resumes abort work after close at every direct-task abort stage"
- describe "task crash recovery"
  - it "crash while the mark commit is in storage: the run resumes"
  - it "crash after the mark before the run joins: only the abort handler runs"
  - it "crash while the abort handler runs: a fresh abort invocation settles the task"
  - it "crash while the abort outcome is in storage: the abort handler runs again"
  - it "crash after the terminal outcome: nothing runs again"
  - it "crash while the reservation commit is in storage: the task is still pending"
- describe "blocked tasks"
  - it "keeps a task with a missing definition pending, and live for idle waits, until registration"
  - it "keeps a task stored by a newer version pending until a fitting definition is registered"
  - it "migrates at reservation, leaves the record unchanged when migration fails, and retries only for a new definition"
  - it "blocks an older record whose newer definition has no migration"
  - it "settles an aborted blocked task as orphaned and retires its documents"
  - it "orphans a marked task whose definition disappeared while its run was active"
  - it "orphans a reopened abort-marked task without a definition once scheduling resumes"
- describe "definition handover"
  - it "hands over at the next phase boundary to a same-version replacement without overlap"
  - it "keeps the memos across a handover"
  - it "hands over to a newer version with a migration"
  - it "hands over to a newer version whose migration fails and leaves the task blocked"
  - it "keeps running under the old definition when the replacement is missing or cannot take the task"
  - it "rejects a runtime commit of the old invocation queued behind its handover commit"
  - it "preserves an abort mark that races the handover commit; the new definition aborts"
- describe "Harness open"
  - it "releases its registry subscription and closes the Session when open fails"

#### `harness-tasks.test.ts`

- describe "task phases"
  - it "continues one invocation through checkpoint progress and completes with a typed result"
  - it "faults a phase without durable progress and a throwing phase"
  - it "keeps a committed terminal outcome when the handler throws afterwards"
  - it "compares checkpoints by value, including arrays"
  - it "commits results with entries atomically, keeps memos until terminal, and retires task documents"
  - it "resumes a waiting task once every task in `on` is terminal, whatever the outcome"
  - it "stamps startedAt at the first run and endedAt when terminal, keeping startedAt through a wait"
  - it "keeps one registry snapshot per phase and refreshes it at the phase boundary"
  - it "resolves the agent at first use in a phase from its snapshot, keeps it for the phase, and anew at the next"
  - it "rejects an agent() wait whose caller context is cancelled, without affecting the phase's resolution"
  - it "observes a failed agent resolution whose only caller stopped waiting"
- describe "task runtime"
  - it "rejects runtime operations after the invocation ends and stops its watches"
  - it "reads committed documents and context through the runtime, and forwards the clock and reports"
  - it "orders runtime commits against the step: one queued before it lands, one after it rejects"
  - it "stops a watch whose acquisition finishes after the invocation ended"
- describe "task scheduling"
  - it "fails the Harness on a failed reservation commit; reopening runs the task"
  - it "fails the Harness instead of running a task again when its fault write fails"
  - it "waits for Harness and conversation idleness, counting blocked work and ignoring background tasks"
  - it "pages the newest tasks first without scanning older ones"
  - it "rejects unknown tasks and reports terminal tasks"
  - it "rejects task waits cancelled or closed while queued on the line, and pending waits on close"
- describe "task abort"
  - it "rejects the run's commits and memo writes after the mark, and settles through a fresh abort invocation"
  - it "starts no further phase after a mark that lands during a phase with progress"
  - it "signals and joins the run before returning, then runs the abort handler"
  - it "aborts waiting work before its wait ends and faults abort handlers that throw or settle nothing"
  - it "does not signal a running abort handler when aborted again, and a cancelled caller leaves the mark durable"
  - it "keeps a terminal outcome committed by an abort handler that throws afterwards"
  - it "never starts phase one when the abort lands while the reservation settles"
  - it "lets an abort mark win over a fault that races it"
- describe "task close"
  - it "stops without outcomes and starts no fresh phase or abort invocation while closing"
  - it "seals admission before signalling handlers and stops watches before joining them"
  - it "writes no fault when close seals while the step after a failed phase is queued"
  - it "starts no abort handler whose reservation settles while closing"
  - it "rejects a runtime commit that was queued on the line when close sealed it"
  - it "starts no next phase when close seals during a step that decided to continue"
  - it "joins a reservation that settles while closing without starting its handler"

#### `harness-telemetry-options.test.ts`

- describe "HarnessOptions.telemetry"
  - it "accepts one adapter with optional propagation and metrics capabilities"

#### `harness-telemetry.test.ts`

- describe "Durable telemetry"
  - it "records content-free submit and phase spans, nested parents, checkpoints, entry events, and metrics"
  - it "returns admission results without re-reading records or probing root link targets"
  - it "inherits omitted or undefined task traces and stores no carrier for an explicit root"
  - it "records checkpoints only when the running phase changes"
  - it "starts each queued input's run in its admission trace and links input roots to their caller"
  - it "links the other queued inputs to the first invocation of their shared run"
  - it "links a steer admission to the current run without moving its work into the steer's trace"
  - it "keeps a public write admission open through commit as a child of its caller"
  - it "links an input submitted inside a turn to the calling phase without parenting it"
  - it "parents background compaction to the prepare phase that creates it"
  - it "marks only the first invocation after reopen resumed, retaining its stored parent"
  - it "emits entry and settlement events only after successful storage commits"
  - it "records internal compaction admissions as phase events without a submit span"
  - it.each "records a fixed %s category and a bounded diagnostic"（`it.each(["faulted", "failed", "orphaned"] as const)`，展开为 3 个用例；每个用例：`span.status === { status: "error", error: { name: `task_${status}`, message: "diagnostic ".repeat(80).slice(0, 500) } }`（即 `ownedSlice(message, 0, 500)` 的结果；注意 `ownedSlice` 补空格再 `slice(0,-1)` 后的值与 `message.slice(0,500)` 相同）；`JSON.stringify(span)` 不含 `private-exception-name`、不含 `private-error-detail`；`JSON.stringify({ attributes: span.attributes, events: span.events })` 不含 `diagnostic`；另断言 `span.parentId === null` 且 `span.attributes.outcome === "faulted"`（三个用例都一样，因为 abort 处理器提交的是 `aborted` 终端，phase span 的 outcome 取的是 `faulted`））
  - it "keeps both interrupted runs and explicit abort handlers at status ok"
  - it "mirrors runtime reports as fixed-field phase events and type-only counters"
  - it "reports hook failures on the phase and clock failures without a phase context"
  - it "counts an outside-span session failure without starting a span"
  - it "counts reports raised with a NOOP span without creating a root"
  - it "attaches synchronous commit-listener reports to the active submission span"
  - it "preserves async listener error containment and report counting"
  - it "emits replay for a codemode journal result without rerunning its nested tool"

#### `harness-tools-recovery.test.ts`

- describe "tool recovery"
  - it "answers an unsafe tool interrupted after intent with its durable partial output"
  - it "reruns a tool only when both the stored and the current replay policy are safe"
  - it "reruns a safe tool with the environment of the conversation's cwd at rerun, and not once it is deselected"
  - it "reruns beforeTool when interrupted before intent, and executes once"
  - it "reruns the generation tools phase interrupted before its commit"
  - it "answers an aborted tool with its partial output and continues the run"
  - it "lets context derivation answer a faulted tool and continues the run"
  - it "answers a real bash command interrupted by close and reopen, then finishes the run"
  - it "clears the interrupted attempt's progress before a safe rerun"

#### `harness-tools.test.ts`

- describe "tool round"
  - it "runs input, tool call, tool result, and answer, and settles the input"
  - it "gives tools and hooks the Harness's models"
  - it "answers calls to tools the request did not offer without a task"
  - it "answers a call to a tool deactivated after preparation with tool_unavailable"
  - it "removes unregistered active tools from the offer and adds them back after re-registration"
  - it "produces tool_unavailable when the implementation is unregistered before its task runs"
  - it "reads the execution mode when a round starts and keeps it for the round"
  - it "runs a round in parallel by default and sequentially when configured or required by a tool"
- describe "tool results"
  - it "uses retained output and the last details when the result omits them, with diagnostics in order"
  - it "offers the tail window with the configured pace, not a head window, and accepts skipped output"
  - it "bounds explicit text content and keeps other content"
  - it "turns a throw into a tool_error result with the partial output"
  - it "records how long execute() took, excluding hooks, and nothing for calls that did not run"
  - it "validates arguments before and after beforeTool and applies blocks and replacements"
  - it "repairs arguments with prepareArguments before validation, and a throwing repair is invalid"
  - it "lets the first beforeTool block win and skips later handlers"
  - it "chains afterTool replacements and observes the round with afterTools"
  - it "runs the hooks of the selected extensions; a task-owned child copies its owner's selection"
  - it "applies addTools and terminates only when every result of the round asks to"
  - it "does not terminate on terminate: false"
- describe "generation hooks"
  - it "replaces request messages, observes responses, and continues on yield"
  - it "keeps the run's input open across an onYield continuation and answers it with the final answer"
  - it "observes responses that arrive by polling a deferred request"
  - it "lets the first onYield continuation win and reports throws without stopping later handlers"
  - it "keeps durable hook decisions in task memos"
- describe "tool execution api"
  - it "builds the environment per call from the conversation's cwd and runs commits, memos, and child tasks"
  - it "answers a throwing environment with a tool_error result and reports it once while preparing"
- describe "coding tools"
  - it "answers a failing command with its retained tail and diagnostics in order"
  - it "reads, edits, and runs a command in one run, then answers"
- describe "tool progress and lifetime"
  - it "applies the default output limits"
  - it "sanitizes running output but keeps explicit result content as the tool returned it"
  - it "drops control keys set to undefined instead of faulting"
  - it "uses explicit null details instead of the last reported value"
  - it "settles details() promises with coalesced progress commits and the terminal commit"
  - it "finishes a call under the implementation it resolved when the tool is replaced mid-call"
  - it "uses a section and hook extension reloaded mid-run from the run's next request"
  - it "rejects invocation-bound waits and stops watches when the tool's invocation ends"
  - it "rejects details() still waiting when the call is aborted during afterTool"
  - it "answers an aborted tool with only its durable output, discarding buffered output"

#### `harness-view.test.ts`

- describe "conversation view"
  - it "hydrates the active entries and the built-in documents"
  - it "publishes one frame per touching commit, whose operations rebuild every revision"
  - it "shares unchanged parts between revisions and skips commits that touch nothing mounted"
  - it "cuts the entries at a head marker, keeping the entries from its head"
  - it "keeps only mounted entries for a raw head write that targets before the active range"
  - it "cuts a fork's view into its inherited entries"
  - it "shows a fork's inherited entries and follows only the fork's own commits"
  - it "unmounts a retired document and mounts its recreation whole"
  - it "folds undelivered commits into one call whose ops reach the newest view"
  - it "keeps watches of one conversation independent and remounts after the last one detaches"
  - it "ends watches at close and rejects later acquisition"
  - it "rejects an acquisition cancelled or closed while it waits for the Session line"
  - it "shares one mount between concurrent observers and isolates a failing listener"
  - it "publishes one frame for a commit that appends several entries and edits a document"


## 8. 建议的 Rust 模块布局与最难复刻的部分

### 8.1 crate 与模块

本单元属于 `kiana-durable` crate（见 `spec/durable-rest.md` §8 的整体布局）。harness 部分建议单独一个模块树，**不引入任何 Node API**（可移植核心）：

```
crates/kiana-durable/src/
  harness/
    mod.rs               # pub use 面：与 src/harness/*.ts 的 23 个文件一一对应
    types.rs             # §2.1：全部公开类型（约 58 个 export）
    harness_impl.rs      # §2.2：HarnessImpl / ConversationImpl / Harness::open / safe_report / safe_now
    agent.rs             # §2.3：AgentDoc、configure、apply_change、add_tools、create_agent、agent_hooks、
                         #       section_extension / tool_extension、resolve_agent、apply_wrap、offered_tools、filter_tools
    registry.rs          # §2.4：RegistryImpl、RegistryState、BUILTIN_TASKS、SECTION_KEY、validate_extension
    context.rs           # §2.5：ContextBounds/ContextRange、capture_context_bounds、read_context_from、derive_*、
                         #       find_baseline、carries_system_message、lead_with_system、contribute、active_entries、
                         #       order_tool_results、select_active、scan_range
    prompt.rs            # §2.6：replay_sections、render_sections、plan_system_entries、plan_tools、plan_sections、system_entry
    inbox.rs             # §2.7：InboxDoc、Boundary、prepare_boundary、apply_boundary、append_input、pick_input_entry、
                         #       is_display_only、has_queued_reset、is_stale、remove_inbox_item、withdraw_queued_inputs
    submissions.rs       # §2.8：Submissions、SubmissionHandle、check_input_entry、admit_submission
    live.rs              # §2.9：LiveDoc、SlotProgress、ToolSlot、NestedToolSlot、CompactionStatus、checkpoint_when、
                         #       PROGRESS_COMMIT、end_run、tool_slot、finish_slot、clear_progress、settle_scheduler_outcome
    generation.rs        # §2.10：GenerationTask（prepare/request/retry/poll/tools）、classify、answer、start_tool_round、
                         #        finish_tool_round、convert_partial、stream_response、hand_over
    tool.rs              # §2.11：ToolTask（call/execute/abort，v2 + v1 迁移）、run()/execute_tool、nested calls、progress
    compaction.rs        # §2.12：CompactionTask（select/summarize/retry）、select_cut、serialize_conversation、place
    scheduler.rs         # §2.15：TaskScheduler 及全部私有方法、索引、模块辅助函数
    events.rs            # §2.13：AgentEvent 22 种、AgentEventStream、watch_events、translate
    view.rs              # §2.14：ConversationViews、ConversationView
    task_graph.rs        # TaskGraphView
    partial_changes.rs   # 流式 partial 的 JsonChange 折叠
    telemetry.rs         # DURABLE_TELEMETRY_SCHEMA（静态 const 表）
    provider.rs          # ProviderDoc、ensure_provider_session_id
    usage.rs             # UsageDoc、record_usage
    define.rs            # define_extension、define_tool、section、hook、wrap_tool、wrap_section
    util.rs              # scan_all、closed_error、optional_extension、error_message
    json.rs              # AssignPlan、assign_json
```

`lib.rs` 用 `pub use harness::...` 复现 `src/index.ts` 的导出面（见 §5.5）。不可见但测试需要的项：`#[doc(hidden)] pub` 或 `pub(crate)` + `tests/` 内的 `#[path]` 引入。

### 8.2 并发模型（最关键的结构决策）

TS 版本的**全部正确性**建立在一条串行化的 Session 线上：提交监听是同步的，`#observe` 在提交被采纳的**同一个**同步栈里更新 `#live` 与索引；`#kick`/`#scheduleReconcile`/`#scheduleWake` 只把工作排到 microtask；`#step`/`#reserve`/`#reconcile`/`#admit` 都在 `commitWith` 里同步读 `#live` 并决定写入。

Rust 侧的等价物是**单线程事件循环 + 显式 microtask 队列**：

```rust
pub struct Runtime {
    microtasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,
    // 计时器/IO 由宿主注入（trait Timers + trait Clock）
}

impl Runtime {
    /// 在当前同步栈结束后立即 drain，复现 Node 的 microtask 检查点。
    pub fn check_point(&self) { while let Some(task) = self.microtasks.borrow_mut().pop_front() { task(); } }
    pub fn queue_microtask(&self, f: impl FnOnce() + 'static) { self.microtasks.borrow_mut().push_back(Box::new(f)); }
}
```

- 所有"Session 线"上的回调（`commit_with` 的 change 闭包、`observe_commits` 监听、`subscribe_close`、registry 的 `subscribe`）**必须**在同一个 `&mut SessionState` 独占访问下同步执行，且**禁止**在其中 `.await` 任何会重新进入 Session 的东西。Rust 用 `RefCell`/`&mut` 借用检查天然强制这一点；跨 `await` 的部分（handler、hook、tool execute）通过 `SessionHandle`（`Rc<RefCell<...>>` 或 `Arc<Mutex<...>>` + 单线程 executor）在**线外**运行。
- `commit_with` 的签名建议：`async fn commit_with<T>(&self, change: impl FnOnce(&mut Tx) -> Result<T, Error> + 'static, ctx: Context, options: CommitOptions) -> Result<T>`；内部把 `change` 放在 Session 线的临界区内执行，之后才 `check_point()`。**顺序**：先在线上评估 change（同步）、写入、发布、`check_point()`（运行 microtask）、最后 resolve 返回的 future。
- **不要**用 `tokio::spawn` 跑 handler：那会引入真正的并行，破坏"提交监听同步更新索引"的假设。用 `LocalSet` + `spawn_local`，或 `futures::executor::LocalPool` + 显式 `poll`。
- 计时器：`foreground` 用会保活的 `tokio::time::sleep`（持有 `JoinHandle`）；`background` 用不 join 的 `spawn_local`（drop 即取消），宿主可返回 `None` 拒绝（Cloudflare 语义）。
- 取消：`CancellationToken` + `AbortReason`。`controller.abort(ended_error)` 的语义 = `token.cancel()` 且把 `ended_error` 存进 `reason` 槽；所有 `with_abort_signal(signal, ctx)` 派生的等待在取消时以该 reason 失败（`ended_error` 是普通 Error，不是 `SessionFailed`）。

### 8.3 关键类型草图

```rust
pub type TaskId = i64;            // 数字品牌（TS 的 Id<"task">）
pub type ConversationId = i64;
pub type EntryId = i64;
pub type SubmissionId = i64;
pub type JsonValue = serde_json::Value;

pub struct TaskRecord {
    pub trace: Option<TelemetryCarrier>,
    pub links: Option<Vec<TelemetryCarrier>>,
    pub id: TaskId,
    pub conversation_id: ConversationId,
    pub kind: String,
    pub version: i64,
    pub input: JsonValue,
    pub owner: Option<TaskId>,
    pub background: bool,
    pub abort_requested: bool,
    pub abort_reason: Option<AbortReason>,   // Some(Restart)
    pub abandon_on_restart: Option<bool>,    // 只有 true
    pub started_at: Option<f64>,
    pub ended_at: Option<f64>,
    pub restarts: Option<i64>,
    pub memos: Option<IndexMap<String, JsonValue>>,
    pub state: TaskState,
}

pub enum TaskState {
    Pending { checkpoint: JsonValue },
    Running { checkpoint: JsonValue },
    Waiting { checkpoint: JsonValue, on: Vec<TaskId>, policy: JoinPolicy },
    Sleeping { checkpoint: JsonValue, until: f64 },
    Completing { outcome: TaskOutcome },
    Terminal { outcome: TaskOutcome },
}

pub enum TaskOutcome {
    Completed { result: JsonValue },
    Failed { error: TaskOutcomeError, result: Option<JsonValue> },
    Aborted { reason: Option<String>, result: Option<JsonValue> },
    Orphaned { reason: String },
    Faulted { error: TaskOutcomeError },
}

pub struct TaskDefinition {
    pub name: String,
    pub version: i64,
    pub initial: fn() -> JsonValue,
    pub phases: IndexMap<String, Box<dyn Fn(&RunningTask, &mut TaskRuntime, &Context) -> BoxFuture<'static, Result<(), Error>>>>,
    pub abort: Option<Box<dyn Fn(...)>>,
    pub migrate: Option<Box<dyn Fn(&JsonValue, &JsonValue, i64) -> Result<Migrated, Error>>>,
}

pub trait AnyTask: Send + Sync { fn definition(&self) -> &TaskDefinition; }
```

调度器索引（§2.15.4）在 Rust 用 `IndexMap`/`IndexSet`（保插入序）：

```rust
pub struct TaskScheduler {
    live: IndexMap<TaskId, TaskRecord>,
    contexts: IndexMap<ConversationId, KeptContext>,
    invocations: IndexMap<TaskId, Invocation>,
    task_waiters: Waiters<TaskId, SettledTask>,
    idle_waiters: Waiters<Option<ConversationId>, ()>,
    failed_migrations: IndexMap<TaskId, FailedMigration>,
    edges: IndexMap<ConversationId, Option<TaskId>>,
    settled: IndexMap<TaskId, TaskNode>,
    drop_queue: IndexSet<Node>,
    below: IndexMap<Node, IndexSet<Node>>,
    roots: IndexSet<ConversationId>,
    unloaded: IndexSet<TaskId>,
    intent: IndexSet<TaskId>,
    runnable: IndexSet<TaskId>,
    finalize_checks: IndexSet<TaskId>,
    waiters: IndexMap<TaskId, IndexSet<TaskId>>,
    fail_fast_checks: IndexSet<TaskId>,
    abandoned: IndexSet<TaskId>,
    sleeping: IndexSet<TaskId>,
    resumed: IndexSet<TaskId>,
    parked_tasks: IndexSet<TaskId>,
    blocked_listeners: Vec<Box<dyn Fn()>>,
    // 标量：enabled, read_only, closing, dirty, draining, wake_scheduled, reconcile_scheduled,
    //       cascade_pending, reconciling_work, snapshot_reported, last_wake
    expiry: Option<TimerHandle>, timer: Option<TimerHandle>,
    unsubscribe_registry: Option<Box<dyn FnOnce()>>,
}
```

### 8.4 最难复刻的部分（按风险排序）

1. **Session 线的同步语义与 microtask 顺序**（§8.2）。几乎所有时序测试（`harness-deferred-progress`、`harness-events`、`harness-live-deltas`、`harness-cancellation-barrier`、`harness-lifecycle`）依赖："提交监听在提交的同一同步栈里运行"、"`#kick` 绝不同步提交"、"`queueMicrotask` 在当前宏任务后立即运行"、"`#reconcile` 的提交在下一轮 microtask"。Rust 若用 tokio 的自然 `.await` 调度，这些顺序会漂移。**必须先建 microtask 队列并用 `check_point()` 在每个 await 边界调用**，并把"提交 → 监听 → 索引 → 排 microtask"写成纯同步函数。
2. **索引的有界性与丢弃/重载**（`#relist`/`#release`/`#dropIfUnneeded`/`#sweep`/`#loadChain`/`#unloaded`）。`harness-scheduler-indexes.test.ts` 与 `harness-scheduler-retention.test.ts` 断言 `indexSizes` 的具体数值与"深层链（>调用栈深度）"的正确性。`#relist` 的向上传播规则（`needed`、`first`、`parent === null` → roots）必须逐行对照；`#loadChain` 的"overlay 里新建则 break"与"`#unloaded` 重查"也必须一致。这是最容易写出"功能对但 indexSizes 不匹配"的地方。
3. **`#reconcile` 的标记优先级**（restart vs request）、`#decide` 的六条规则顺序、`#commitState` 的"相同 checkpoint 不算进展"、`#terminate` 的 `completing` 持有。`harness-cancellation-barrier`、`harness-blocked`、`harness-ownership` 全部依赖。特别注意 `withAbortMark` 会**丢弃** `abortReason` 与 `restarts`；`withState` 在 terminal/completing 时丢弃 `memos` 与 `restarts`；`progressed` 只丢 `restarts`。
4. **Chord `Draft` 的"只有重赋值的叶子产生操作"**。`kiana.live` 的增量测试（`harness-live-deltas`）断言"一次提交只有一个字段写"、"tail 窗口滑动 = 一次 front trim + 一次 append"、"文本流是 append"、"完整 base 恰在无运行时"。Rust 侧必须实现同样的文档草稿：要么用 `Draft<T>` 包装 + 脏路径记录，要么用 `Arc` 结构共享 + 按字段 diff。`assignJson` 的 `AssignPlan::{append, keep, fields}` 是这一语义的核心（`src/harness/json.ts`），并且它依赖 `appendText()` 产生 append 而非 set。
5. **JSON 键序与数字/字符串格式化**（§6.3）。`serde_json` 的 `preserve_order` 是必要的但不充分：`copyJson` 丢 `undefined`、`JSON.stringify` 的转义（错误消息里 `JSON.stringify(key)` 出现在 `registry.ts` 与 `tool.ts`）、数字的 JS 格式（telemetry 属性、错误消息、`String(until)`）。建议写一个 `js_json` 模块，提供 `stringify_js(&Value) -> String` 与 `js_number_to_string(f64) -> String`，并配一组与 Node 对拍的单元测试。
6. **UTF-16 长度与切片**：`ownedSlice(s, 0, 500)`（span status 消息、`NestedToolSummary.error`；按 code unit 切、可拆代理对、并强制拷贝，见 §5.2）、`truncate` 的行/字节、`boundOutput` 的 `droppedBytes`（UTF-8 字节）与 `droppedLines`、`estimateMessageTokens` 的字符估算。`harness-nested-tools` 的 "shows a nested call's arguments ... as made, then, unbounded, as it runs with them" 与 `harness-structured-output` 的截断断言都对边界敏感。Rust 用 `Vec<u16>` 视图或自写 `utf16_len`/`utf16_slice`。
7. **结构化输出与 TypeBox `Compile` 的修复语义**（`tool.ts` 的 `structuredOutputError`、`validateToolArguments`、`prepareArguments`、可选属性设为 `undefined`、`__proto__` 键）。`harness-structured-output`（10 个测试）与 `harness-nested-tools` 的 "runs a nested call whose repaired arguments set an optional property to undefined" 直接断言修复结果。Rust 需要自写 JSON Schema 子集的 validate/clean/default/convert，并保证错误文本与 kiana-ai 一致（错误文本会进 `kiana.tool-result` 的诊断块）。
8. **工具输出流式与进度节流**：`chord/output` 的 `Progress`/`ProgressClock`/`PROGRESS_BYTES_PER_SECOND`/`boundOutput`，加上 `progressIntervalMs`/`outputIntervalMs`/`persistWithinMs` 三个节流与"大提交按大小成比例暂停"的规则。`harness-live-deltas` 与 `harness-deferred-progress` 断言具体的提交次数与内容。Rust 必须复刻 `Progress` 的"按字节/时间预算"算法，而不仅是"每 N ms 一次"。
9. **Compaction 的 cut 选择**（`selectCut`/`isCandidate`/`estimateContext`）与摘要放置的竞态（stale 判定、`compaction:<taskId>` 写提交、多摘要"cut 更远者胜"、reset 与摘要的顺序）。`harness-compaction.test.ts` 有 96 个测试，是最重的单个文件；其中 "keeps about keepRecentTokens and cuts at the first candidate at or after the budget (spec §8.7 example)" 直接把 spec 的示例数值化。Rust 需逐字复刻 `estimateMessageTokens`/`calculateContextTokens` 的估算（这是"约"的边界）。
10. **`kiana.live` 的完整 base 规则**（`checkpointWhen`）与 `kiana.tool.nested-call` 的"一个调用一个文档"（family，scope task）。`harness-live-deltas` 的 "keeps a complete base exactly while nothing runs" 与 `harness-nested-tools` 的 "stores one document per nested call, keyed by its key, written only by that call" 要求 family key 语义与 Chord 完全一致。
11. **嵌套工具调用的重放安全与键规则**（`startNestedCall`/`checkKey`/`MADE_NESTED_CALLS`/`reattach`/`abandonOnRestart`/`progress:false`）。`harness-nested-tools.test.ts`（33 个）与 `harness-nested-tools-restart.test.ts`（11 个）覆盖：键为空/含 `/`/`__proto__`/正整数被拒；replay-safe 调用者按 key 重挂；replay-unsafe 调用者的未完成调用被放弃并级联到其子任务；"admission 仍在提交时调用者返回"要 abort。Rust 的 `Arc` 生命周期与"调用结束后槽位清理"的顺序要精确。
12. **Agent events 的翻译**（§2.13）：22 种 `AgentEvent`（含 `snapshot`）+ 8 种 `MessageChange` 类型串（`text_start`/`thinking_start`/`toolcall_start`、`text_delta`/`thinking_delta`、`toolcall_delta`、`block`、`message`）、view op → event 的五阶段顺序、100 个 pending 批次后的 overflow snapshot、`NativeWatch` 的 folding、`messageChanges` 的 partial 重建、`toolcall_delta` 的路径追加。`harness-events.test.ts`（18 个）断言精确的批次内容（"emits one exact batch"），需要逐 op 复刻。
13. **失败/关闭路径**：`#failSession`、`#seal`、`closedError`、`join()` 的 `writeAtClose` 回写 `pending`（不增加 restart）、`ReadOnlySession`、`StorageRequestError` 只失败一次读。`harness-storage-failure.test.ts`（24 个）、`harness-lifecycle.test.ts`（14 个）、`harness-read-only.test.ts`（4 个）覆盖。Rust 中"关闭后拒绝所有操作"与"排队在 seal 的读仍完成"的区分要精确。
14. **调度器的 `wake()` 计算**（`active`/`at` 的来源：invocations、reconcilingWork、reconcileWork()、未到期 sleep 的 min、parked/不可接任务的排除）。`harness-sleep.test.ts`（13 个）与 `test-workers/wake.test.ts` 直接断言。`onWake` 的"仅在值变化时投递、抛错后下次变化再投递"也要一致（`#lastWake` 只在成功后记录）。
15. **报告与 telemetry 的精确性**：九个 `ReportInfo` 变体的字段、`task_faulted` 的 `reason` 二值、span 属性的名字/类型/枚举值、status 消息的 500 字符截断、`kiana.durable.checkpoint` 事件仅在 phase 变化时发。`harness-telemetry.test.ts`（22 个）用 `InMemoryTelemetryContext` 断言。
16. **`kiana.agent` 的 `AgentChange` 应用语义**（GIVEN 字段替换、`null` 清除、`undefined` 不动；`tools: {remove}` 与 `modelTools: {add}` 的合并规则；`addTools` 只追加缺失名）。`harness-conversations.test.ts` 的 "replaces whole fields, clears them with null, and leaves undefined fields alone" 是逐字断言。Rust 的 `Option<Option<T>>` 三态要在类型上区分"未给"与"清除"。
17. **`resolveSettings` 的逐字段合并**：`progress` 三个字段分别 `??` 默认（显式 `undefined` 保持默认）；`persistWithin`/`documentRetention` 的宽松校验；`restartLimit` 的 `Math.floor` 与 `>= 1`；`retry`/`compaction` 的浅合并。`harness-generation.test.ts` 的 "resolves settings over the built-in defaults" 与 "reads settings through getters at every decision" 断言。Rust 要避免 `#[serde(default)]` 把"缺失"和"显式 null"混为一谈。
18. **`kiana.usage` 的数值集合语义**：`recordUsage` 必须产生 Chord 的数值集合（`{"#add": n}` 形式的增量操作），而不是读改写整个对象；`harness-inbox.test.ts` 的 "records usage as numeric sets on the ledger in the entry's commit" 与 "keeps usage totals exact across reopen, counting a partial converted after reopen once" 断言。Rust 侧依赖 `kiana-chord` 的数值集合支持。
19. **`#runtime` 的每个成员在结束后抛 `endedError`**：`now()`、`report()`、`setTimer()`（结束后返回 noop 而非抛）、`memo`、`commit`、`context`、`watchDoc`、`abortOwned`、`waitForTask` 的具体行为不同（有的抛、有的 noop、有的 reject）。`harness-tasks.test.ts`（36 个）与 `harness-nested-tools` 覆盖。Rust 用 `RuntimeHandle { ended: Cell<bool> }` 逐成员照抄。
20. **测试自身的并发原语**：`deferred()`、`aborted(signal)`、`flush()` 的语义要在 Rust 测试工具里复刻（`futures::channel::oneshot` + `LocalPool::run_until_stalled`），否则测试会假通过。

### 8.5 验证清单（Rust 移植完成的判定）

- [ ] 40 个 `harness-*.test.ts` 的 695 个 `it` 全部有对应的 Rust 测试且通过（`cargo nextest run -p kiana-durable`）。
- [ ] `KIANA_DURABLE_DOCUMENT_RETENTION_MS=0` 的等价模式（cargo feature 或环境变量驱动 `set_default_document_retention(0)`）跑第二遍，结果一致。
- [ ] 31 个 `test/examples/*.ts` 的 Rust 等价物输出逐字一致。
- [ ] `indexSizes`、`inspect()`、`wake()`、`TaskGraph` 的数值/结构与 TS 逐一对比一致。
- [ ] 全部错误消息（§3.2 的 47 个构造点 / 去重后的模板，其中 §2.15 的 `scheduler.ts` 占 22 个构造点、19 条模板）逐字一致，包括数字/字符串插值格式。
- [ ] telemetry span/event/metric 的名字、属性名、枚举值、status 映射逐字一致。
- [ ] 磁盘上 `kiana.agent`/`kiana.live`/`kiana.inbox`/`kiana.provider`/`kiana.usage`/`kiana.tool.nested-call` 的 JSON 结构、键序、`version`、`history`/`fork`/`checkpointWhen` 语义一致；旧版本记录（`kiana.tool` v1）可迁移。
- [ ] 无 Node API 依赖（可移植核心可用 `wasm32-unknown-unknown` 编译）。
- [ ] 单线程语义可验证：所有状态变更都在 Session 线内；`#kick`/`#reconcile`/`#scheduleWake` 不同步提交。

---

## Audit additions（对抗性完整性审计的补充与更正）

> 本节由对 `packages/durable/src/harness/**`、`packages/durable/docs/spec.md` 与 `test/harness-*.test.ts` 的逐文件复核产生。上文已被就地更正的部分（不在此重复）：
- `ownedSlice` 不是"不拆代理对的安全切片"，而是按 UTF-16 code unit 的 `slice` + 强制拷贝（§5.2、§5.4 注、§6.3 第 1 条、§8.4 第 6 条）；
- §1.3 的值/类型导出计数反了（实为 **25 值 / 88 类型**，并补了 17 条 `export … from "./harness/*"` 的逐文件分解）；
- §5.5 的 `harness/types.ts` 计数（48 → **57**，且 57 个全部被 `export type` 重导出）；
- §7 的 `it` 计数（694 → **695 个调用点 / 697 个执行用例**），§7.1 合计行同步；
- §6.2 的 `structuredClone`（"未使用" → `usage.ts` 使用）与 `WeakMap`/`WeakRef`（"无" → 三处使用）；
- §5.2 `overlap` 的使用处（`tool.ts / live.ts` → **仅 tool.ts**），以及 `copyJson`/`Draft`/`Watch` 的逐文件清单；
- §5.3 的 kiana-ai 导入表（`agent.ts` 不 import kiana-ai；`ModelRef` 是本包定义；各 `utils/*` 的实际使用者）；
- §2.18 的局部常量个数（三个 → **四个**）；
- §8.1 的 `AgentEvent` 个数（26 → **22**）；
- §3.4 的 `SCAN_PAGE_SIZE` 文件数（三份 → **四份**，含 `context.ts`）；
- §3.2 的 scheduler 计数口径（22 个构造点 / 19 条模板）与新增的 tool 诊断文本行；
- §3.7 的 `npm run test:evicting`（**package.json 里并不存在**）；
- §7.2 的 `test-workers/` 文件清单（补 `wrangler.jsonc`、`cloudflare.d.ts`）与 `vitest.benchmark.config.ts`；
- §6.1 的 package.json 元数据（补 `exports` 18 个键）。

以下为**新增**或**此前完全缺失**的内容。

### A.1 本单元内**未从 `index.ts` 导出**的模块级符号（补充 §5.5 的清单）

分两类：
- **模块内 `export` 但未进 `index.ts`**（可被同包兄弟文件/测试 `import`）：Rust 侧 `pub(crate)`（测试要用的 `#[doc(hidden)] pub`），**不要**放进 `lib.rs` 的公开面。
- **模块私有**（没有 `export`，仅本文件用）：Rust 侧保持私有即可，但**必须**保留其结构与语义——它们是各文件内部算法的骨架。下表用 `(私有)` 标出：`CreateTarget`、`CreateOptions`、`ConversationHost`（`harness.ts`）、`SystemDraft`、`ToolChanges`（`prompt.ts`）、`AbortResult`（`submissions.ts`）都属于这一类。

| 文件 | 符号 |
|---|---|
| `harness.ts` | `SCHEDULER_INDEX_SIZES = Symbol.for("kiana-durable.schedulerIndexSizes")`（`test/harness-scheduler-retention.test.ts`、`test/scheduler-memory-bench.ts` 用）；`safeNow` 也是模块 `export` 但未进 `index.ts`（`HarnessImpl` 构造时用）；类型 `CreateTarget`（私有；`{ kind: "root" } \| { kind: "independent"; ownership } \| { kind: "fork"; parentId; at; ownership }`）、`CreateOptions`（私有；`{ agent?: AgentChange; init?: ConversationInit }`）、`ConversationHost`（私有） |
| `harness/prompt.ts` | 类型 `SystemDraft = TypedEntryDraft<never>`（私有）、`ToolChanges = { readonly toolsRemoved: ToolReference[]; readonly toolsAdded: Tool[] }`（私有）；内部 `planTools`、`planSections`、`systemEntry` |
| `harness/submissions.ts` | 类型 `AbortResult = "aborted" \| "already_placed" \| "settled"`（私有；`abort()` 公开签名另可返回 `"not_found"`） |
| `harness/inbox.ts` | `QueueModes = Pick<Settings, "steeringMode" \| "followUpMode">`、`Boundary`、`BoundaryResult = { readonly users: SubmissionId[]; readonly reset: boolean }`、`prepareBoundary`、`appendInput`、`pickInputEntry`、`isDisplayOnly`、`hasQueuedReset`、`isStale`、`withdrawQueuedInputs`、`removeInboxItem`、`applyBoundary` |
| `harness/registry.ts` | `BUILTIN_TASKS: readonly AnyTask[] = [GenerationTask, ToolTask, CompactionTask]`（顺序即 `Harness.open` 报缺失名时的顺序） |
| `harness/live.ts` | `PROGRESS_COMMIT`、`endRun`、`addCompactionStatus`、`compactionStatus`、`removeCompactionStatus`、`failCompactionStatus`、`toolSlot`、`finishSlot`、`removeNestedSlots`、`clearProgress` |
| `harness/partial-changes.ts` | 类型 `PartialMessage = JsonRepresentation<AssistantMessage>`、`PartialCommit`；类 `PartialChanges` |
| `harness/tool.ts` | 模块级 `export` 但未进 `index.ts` 的只有 `harnessError`、`appendToolResult`；其余（`startNestedCall`、`nestedResult`、`toolDiagnostic`、`renderDiagnostics`、`checkKey`、`boundContent`、`summaryOf`、`errorTextOf`、`outputValue`、`fallbackResult`、`fromSlot`、`truncated`、`invalid`、`validate`、`prepare`、`run`、`progressClock`、`publishProgress`、`finalResult`、`structuredOutputError`、`resolveTool`、`readCall`、`MADE_NESTED_CALLS`、`COMPLETED`、`Runtime`）是**模块私有**，`MAX_SUMMARY_ERROR_CHARS = 500` 亦为私有常量 |
| `harness/context.ts` | `ContextBounds`、`ContextRange`、`captureContextBounds`、`readContextFrom`、`deriveContext`、`activeEntries`、`orderToolResults` |

### A.2 `test/` 下与本单元相关的**非 `harness-*.test.ts`** 辅助文件（§7 未列）

- `test/chat-support.ts`（4.4K）：`ChatSetup`（`faux`/`models`/`registry`/`reports`/`settings`/`now`/`identity?`）、`chatSetup(options)`、`openChat(storage, setup, options)`（`Harness.open` + `harness.root(..., { agent: { model: { provider: "faux", modelId: "faux-1" } } })`）。**40 个 harness 测试文件中有 25 个 import 它**（`harness-support.ts` 是 23 个）——两者都常用，Rust 测试夹具应两个都复刻。
- 与本单元直接相关的 bench（`package.json` 的 `bench:*` 脚本，非 vitest 测试）：`test/scheduler-memory-bench.ts`（唯一直接 `import { SCHEDULER_INDEX_SIZES } from "../src/harness/harness.ts"`）、`test/scheduler-fanout-bench.ts`、`test/nested-tools-bench.ts`、`test/tool-output-bench.ts`、`test/document-append-bench.ts`。这些断言"索引有界 / 提交次数与字节成本成比例"的**性能不变量**，Rust 侧建议对应的 `benches/`。`test/heap-retained.ts`（`retainedBytes()`，基于 `node:inspector/promises` 的堆快照）被 `scheduler-memory-bench.ts` 与 `session-document-cache-bench.ts` 用于内存保持检查。

### A.3 `test/examples/` 与 `test-workers/` 的补充

- `test/examples/` 的 31 个脚本在 `package.json` 里**没有**对应脚本；通过 `node --conditions=source --experimental-strip-types` 直接运行（`bench:*` 脚本展示了这条命令形状）。Rust 侧 `examples/*.rs` 应以 `cargo run --example` 运行并逐字比对 stdout。
- `test-workers/` 的 `wrangler.jsonc` 与 `cloudflare.d.ts` 已在 §7.2 补全。

### A.4 `docs/spec.md` 的章节对照（§0.3 之外补正）

- `docs/spec.md` 的准确顶层结构是 §1–§13（5,739 行）：§11 Backends（11.1 Memory / 11.2 SQLite）、§12 API footguns、§13 Non-goals。§0.3 现已补列 §11（并已列 §12/§13）；**§11 也须读**：11.1 说明 `MemoryStorage` 会拷贝保留值与所有读结果（这正是 `context.ts` 深冻结与 `harness-context.test.ts` "深冻结 kept entries" 断言的前提），11.2 说明"一个 SQL 事务 = 一个 Session 提交"与 schema v2 引入 `sleeping`。
- §2 只有 2.1/2.2 两个子节；§6 无子节；§10 只有 10.1 Failures。§0.3 表格里的"§2 核心记录 / §2.1 / §2.2"写法正确，但"§10 存储契约 / §10.1 失败"之外无其它子节。

### A.5 需要逐字保留的**其它**字符串（上文未覆盖）

- `util.ts`：`"Harness is closed"`（无 Session 失败时）。
- `context.ts`：`MISSING_RESULT_TEXT = "Tool result unavailable: history ends before this call completed."`；合成结果的 `details: { reason: "missing_result" }`。
- `harness.ts`：`DEFAULT_HARNESS_IDENTITY = { attribution: KIANA_ATTRIBUTION }`；`safeNow` 报 `{ type: "host_error", callback: "now" }` 且**先置 `reported = true` 再报告**。
- `registry.ts`：`BUILTIN_TASKS` 的顺序（`kiana.generation`、`kiana.tool`、`kiana.compaction`）决定 `Registry lacks built-in tasks ${names}` 的 `${names}` 顺序（`", "` 连接）。
- `submissions.ts`：`AbortResult` 的四个字面量 `"aborted"`/`"already_placed"`/`"settled"`/`"not_found"`；`SubmissionHandle.abort` 对 `"not_found"` 抛 `Submission ${id} does not exist`。
- `scheduler.ts`：`resume()` 的 `ReadOnlySession("running tasks")` 实际消息（由 `errors.ts` 的 `ReadOnlySession` 基类拼出）是 `Read-only Harness: running tasks refused`，`name = "ReadOnlySession"`。
- `submissions.ts`：`settleSubmission(id, { status: "unanswered", reason: "aborted" })` 里的 `reason: "aborted"`（与 `AbortResult` 的 `"aborted"` 同名但是不同字段）。

### A.6 `kiana-ai` 导入表的更正要点（摘要）

- `agent.ts` **不** import `@earendil-works/kiana-ai`（上一版 §5.3 把 `agent` 列进了 `ModelRef`/`Model` 的使用处）。
- `ModelRef` 是**本包 `harness/types.ts` 定义的** `{ provider: string; modelId: string }`，不是 kiana-ai 导出；`Api` 只在 `generation.ts`；`ModelThinkingLevel` 在 `compaction.ts`/`generation.ts`/`types.ts`。
- `utils/transcript`（`getCurrentTools`/`toToolDeclaration`/`declarationsEqual`）**只**在 `prompt.ts`；`utils/estimate`、`utils/retry` 只在 `compaction.ts`（`utils/retry` 另有 `generation.ts`）；`utils/validation`、`typebox/compile` 只在 `tool.ts`；`utils/uuid` 只在 `provider.ts`；`utils/streaming-json` 只在 `partial-changes.ts`；`utils/identity` **只**在 `harness.ts`；`utils/overflow` 只在 `generation.ts`。

### A.7 Rust 映射的两处行为缺口

1. **`ownedSlice` 的拷贝语义**：TS 版本的 `` `${slice} `.slice(0, -1) `` 除了截断还**强制生成扁平字符串**，避免保留住原始大输出（`tool.ts` 的注释明说"`ownedSlice` 会拷贝，避免槽位通过切片持有大输出"）。Rust 若直接 `&s[..]` 或 `String::truncate` 只得到子串拷贝即可满足"拷贝"；但**不要**顺手改成按 `char` 边界切——那会改变 `500` 这个"字符数"的计数口径。
2. **`structuredClone` 与 `Object.defineProperty`（`usage.ts`）**：Rust 的 `Map`/`serde_json::Value` 天然没有原型链，因此不需要 `defineProperty` 的特殊路径；但**必须**保留"已存在键走 `addUsage` 累加、不存在键整块拷贝"的分支——`harness-usage` 的 "keeps tools named like object prototype keys" 依赖这一点（工具名可能是 `"__proto__"`/`"toString"`）。

### A.8 测试断言的计数口径（供 Rust 测试一一对应）

- §7.1 表格的 `it` 列合计 **695**（每行按 `^\s*it(\.each(...))?\(` 计数；`harness-telemetry.test.ts` 的 22 含 1 个 `it.each`）。
- 真正**执行**的用例数 = 694 个普通 `it` + 3 个 `it.each` 展开 = **697**。
- §8.5 验证清单里的 "695 个 `it`" 指**调用点**（文件里可见的 `it` 数量），不是执行用例数；两者都对，但不要混用。
- 全部 40 个文件共 **98** 个 `describe`；`it.each` 只有 1 处（`harness-telemetry.test.ts`）。

### A.9 余下的小瑕疵（已知但影响小，实现时留意）

- §8.5 验证清单原写 "§3.2 的 40 余条 + §2.15 的 22 条"，`§3.2` 的 "40 余条" 本身是**约数**（已就地改为 47 个构造点 / 去重模板）；精确口径：本单元 23 个源文件共 **47** 个 `new Error`/`new TypeError`/`new ReadOnlySession`/`new SessionFailed` 构造点，逐文件为 `scheduler.ts` 22、`tool.ts` 6、`registry.ts` 5、`submissions.ts` 5、`util.ts` 2、`view.ts` 2，以及 `agent.ts`/`context.ts`/`generation.ts`/`harness.ts`/`provider.ts` 各 1。其中 4 个的消息是运行期拼接的（`scheduler.ts` 的 `new Error(message)` 三处 + `tool.ts` 的 `new Error(broken)`），因此去重后的**模板**条数少于构造点（例如 `Task ${id} does not exist` 在 `scheduler.ts` 出现 4 次、`Submission ${id} does not exist` 在 `submissions.ts` 出现 2 次）。
- `§2.15` 的【逐字】块覆盖 **18 条** `Task …`/`Abort handler …` 消息模板（含 `Task ${id} does not exist`，它以 `【逐字】` 块的列表项形式出现），加上 `ReadOnlySession("running tasks")` 共 **19 条**去重消息；与 22 个构造点的差额来自 `Task ${id} does not exist` 的 4 个构造点与 3 处 `new Error(message)`（消息由上文变量拼出）。
- `test/harness-*.test.ts` 里没有 `it.skip`/`it.only`/`it.todo`，也没有 `test(...)` 调用点——Rust 侧不需要考虑"跳过"语义。

### A.10 诊断（`ToolDiagnostic`）的 code 与消息全表

`ToolDiagnostic = { severity: "info" | "warn" | "error"; message: string; code?: string }`（`code` 可选；`severity` 的字面量是 **`"warn"`**，不是 `"warning"`；`toolDiagnostic(code, message)` 固定 `severity: "error"`）。本单元产生的全部 code：

| code | severity | message | 产生处 |
|---|---|---|---|
| `tool_unavailable` | error | `Tool ${call.name} is not available` | `tool.ts` 的 `resolveTool` 失败、`generation.ts` 的 tool round |
| `blocked` | error | `Tool call blocked: ${block}` | `tool.ts` 的 hook 拦截 |
| `invalid_arguments` | error | `validateToolArguments` / `prepareArguments` 抛出的文本（kiana-ai 的 `Invalid arguments for tool "<name>": <detail>`，或修复函数自己的错误文本） | `tool.ts` 的 `invalid(...)` |
| `invalid_structured_output` | error | `structuredOutputError` 返回的文本 | `tool.ts` 结构化输出校验 |
| `tool_error` | error | `errorMessage(error)` 或结构化输出解析失败的文本 | `tool.ts` 执行抛错 / `text !== ""` |
| `aborted` | error | `Tool ${call.name} was aborted` | `tool.ts` 的 abort 处理器（非 restart）、`generation.ts` 的 tool round |
| `truncated` | warn | `Output truncated${kept}: ${dropped.droppedLines} lines, ${dropped.droppedBytes} bytes dropped`（`kept` ∈ `""` / ` to its beginning` / ` to its end`） | `tool.ts` 的 `truncated(...)`、`fromSlot` |
| `interrupted` | error | 两处：`Tool ${call.name} was interrupted and may have partially run`（`execute` 阶段的非 replay-safe 恢复）、`Tool ${call.name} was interrupted by a restart and may have partially run`（abort 处理器里 `abortReason === "restart"` 且 checkpoint 阶段是 `execute`） | `tool.ts` |
| `abandoned` | error | `Tool ${call.name} was not started: its caller ended with a restart` | `tool.ts` 的 abort 处理器（`abortReason === "restart"` 且阶段不是 `execute`） |
| `stale` | —（**不是** `ToolDiagnostic` code） | — | 仅作为 submission 的 `reason`：`{ status: "unanswered", reason: "stale" }`（`inbox.ts`、`submissions.ts`） |
| `missing_result` | —（不是 `ToolDiagnostic`，是合成 tool-result 的 `details.reason`） | — | `context.ts` 的 `missingResult` |

`fallbackResult` 与 `#terminate` 的结局把 `outcome.status`（`"faulted"`/`"failed"`/`"orphaned"`/`"aborted"`）直接当 `code` 用，因此这四种 code 也可能出现在诊断里。

### A.11 `errorMessage` 的三分支（Rust 复刻要点）

`errors.ts` 的 `errorMessage(error)`（本单元多处用于诊断文本）逐字为：
```ts
if (error instanceof Error) return error.message;
try { return String(error); } catch { return Object.prototype.toString.call(error); }
```
即：Error → `.message`；否则 `String(error)`（`null` → `"null"`、`undefined` → `"undefined"`、字符串原样、数字按 JS 数字格式、对象 → `"[object Object]"`、数组 → 逗号连接且 `null`/`undefined` 项为空串）；若 `String()` 自己抛错（例如 `toString` 被改成抛错的对象）→ `Object.prototype.toString.call(error)`（通常是 `"[object Object]"`）。

Rust 的 `error: Box<dyn Error>` / `unknown` 需要显式分支，且 `String()` 那一路必须实现 §6.3 第 6 条的 `js_to_string`。

### A.12 `test-workers/` 的两个测试文件的语义（Rust 侧可对照）

- `wake.test.ts`（4.9K，describe "a Harness woken by its Durable Object alarm"，3 个 it）：`"sleeps through an eviction and resumes the task from its alarm"`、`"recovers a turn an eviction cut off mid-stream through the heartbeat alarm"`、`"can evict an object whose Harness keeps idle documents and contexts in memory"`。`TestSession` 的 `onWake` 把 `wake` 收进 `wakes`（`Wake[]`），`arm(wake)` 用 `Date.now() + HEARTBEAT_MS`（`HEARTBEAT_MS = 60_000`）作为 `wake.active` 时的心跳、用 `wake.at - #offset` 作为睡眠到期时刻（`#offset` 持久化在 storage 里，测试可跨 eviction 移动 Harness 时钟）。这是 `harness.ts` 的 `onWake` 与 `scheduler.ts` 的 `#wakeDue`/`resume()` 在宿主层的契约；Rust 侧对应的"foreground/background timer + host wake"要在 `kiana-durable` 的宿主适配层复刻（可移植核心只需保证 `Harness.wake(): { active: boolean; at?: number }` 的语义——`readOnly` 恒 `{ active: false }`、`completing` 不计、未被 abort 的 `waiting`（其 `on` 仍有活任务）不计、未被 abort 的 `sleeping` 且 `until > now` 只贡献 `at = min(...)`——且 `resume()` 幂等）。
- `storage.test.ts`（2.6K，describe "Durable Object SQLite schema"，1 个 it）：`"migrates a version 1 schema to admit sleeping tasks, keeping tasks and indexes"`。属 `storage/sqlite/cloudflare` 兄弟单元，但 `Harness.open` 的恢复路径依赖它。

> 这两个文件（3 + 1 个 it）**不在** §7.1 的 40 个 `harness-*.test.ts` 与 `695` 的计数内；Rust 侧若要全量对拍，需另计 `test-workers` 的 4 个用例。

---

### A.13 测试夹具的完整导出清单（§7.2 的补充）

| 文件 | 行数 | 导出 |
|---|---:|---|
| `test/harness-support.ts` | 158 | `tool`、`openHarness`、`user`、`assistant`、`toolResult`、`system`、`describeMessage`、类型 `Installed`、`addTool`、`addTask`、`addHooks`、`addSection`（12） |
| `test/task-support.ts` | 132 | 类型 `Deferred`、`deferred`、`aborted`、`eventually`、`settled`、`openTasks`、`completed`、`abortedWith`、`countingReader`（9） |
| `test/session-support.ts` | 180 | `context`、类型 `Gate`、`ControlledStorage`、`openTestSession`、`documentChanges`、`observeCommits`、`documentCopyChanges`、`createConversation`、`flush`（9 个导出；`deferred()` 是文件私有） |
| `test/chat-support.ts` | 121 | 类型 `ChatSetup`、`chatSetup`、`openChat`（3） |
| `test/setup-document-retention.ts` | 7 | 无导出（副作用脚本） |

`task-support.ts` 的 `eventually(check)` 在 200 次 `flush()` 后仍未满足时抛 `Condition was not reached`——Rust 的等价测试辅助需要同样的"有限轮转 + 明确失败"，避免死等。
### 附：文档结束

本规范覆盖 `packages/durable/src/harness/**` 的 23 个源文件（9,598 行，197 个 `export` 语句）、`packages/durable/docs/spec.md` 的相关章节、以及 `packages/durable/test/harness-*.test.ts` 的 40 个测试文件（24,900 行）。逐字字符串以【逐字】标注；`【逐字】` 标记的文本必须字节级一致。兄弟单元（`src/types.ts`、`src/documents.ts`、`src/entries.ts`、`src/tasks.ts`、`src/errors.ts`、`src/ids.ts`、`src/timers.ts`、`src/truncate.ts`、`src/session/**`、`src/storage/**`、`src/env/**`、`src/tools/**`、`src/images/**`、`src/codemode/**`、`src/tool-search/**`）的规范见 `kiana/spec/durable-rest.md`；`chord` 见 `chord-core.md`/`chord-rest.md`；`kiana-ai` 见 `ai-core.md` 等；`kiana-telemetry` 见 `telemetry-mcp.md`。
