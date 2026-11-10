# 单元规范：ai-api-anthropic-openai

> 本文件是 `kiana`（TypeScript）→ `kiana`（Rust）全量重写中，单元 **`ai-api-anthropic-openai`** 的移植规范。
>
> 作用有二：
> 1. **验证清单**：Rust 实现必须逐条满足本文件列出的行为、字符串、字段、顺序与错误条件。
> 2. **Rust 设计**：给出模块划分、关键类型、以及 TS 语义到 Rust 语义的桥接方案。
>
> 源码范围（只读参考，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/ai/src/api/` 下的
> `anthropic-messages`、`openai-completions`、`openai-responses`、`openai-responses-shared`、
> `openai-codex-responses`、`azure-openai-responses`、`azure-openai-config`、`openai-decisions`、
> `openai-prompt-cache`、`github-copilot-headers`、`transform-messages`、`simple-options`、`lazy`、
> `constrained-sampling`、`classifier-shared` 及全部 `*.lazy.ts`；相关测试位于
> `/home/ctyun/code/kiana_rev/kiana/packages/ai/test/`。
>
> 规范中所有 **英文引号内的字符串、HTTP 头名、JSON 字段名、端点路径、beta 标识** 都是**逐字（verbatim）**的，
> Rust 侧不得改写、不得增删空格或标点。中文仅用于解释。
>
> 版本基准：`@earendil-works/kiana-ai` v1.4.0，`@anthropic-ai/sdk` 0.129.0，`openai` 7.19.0，
> `partial-json` 0.1.7，Node >= 22.19.0。

---

## 目录

1. [概览](#1-概览)
2. [逐文件规范](#2-逐文件规范)
3. [用户可见面（逐字）](#3-用户可见面逐字)
4. [外部格式（逐字）](#4-外部格式逐字)
5. [跨单元接口](#5-跨单元接口)
6. [npm 依赖 → Rust crate 映射](#6-npm-依赖--rust-crate-映射)
7. [测试清单](#7-测试清单)
8. [Rust 模块布局与最难部分](#8-rust-模块布局与最难部分)
9. [审计补充（Audit additions）](#9-审计补充audit-additions)

---

## 1. 概览

### 1.1 本单元的职责

本单元实现 kiana-ai 的**六个 LLM 传输适配器**与**一个分类器适配器**，全部属于 `src/api/`：

| 模块 | api 标识 | 传输 | 主要 SDK |
|---|---|---|---|
| `anthropic-messages.ts` | `anthropic-messages` | HTTP + SSE（Anthropic Messages beta 接口） | `@anthropic-ai/sdk` |
| `openai-completions.ts` | `openai-completions` | HTTP + SSE（Chat Completions） | `openai` |
| `openai-responses.ts` | `openai-responses` | HTTP + SSE（Responses API） | `openai` |
| `openai-codex-responses.ts` | `openai-codex-responses` | 自建 HTTP+SSE **与** WebSocket（Codex 后端协议） | 原生 `fetch` / 全局 `WebSocket` |
| `azure-openai-responses.ts` | `azure-openai-responses` | HTTP + SSE（Azure 上的 Responses API） | `openai` 的 `AzureOpenAI` |
| `openai-decisions.ts` | `openai-decisions` | HTTP POST JSON（一次性，非流式） | 原生 `fetch` |

另有若干**纯函数共享模块**（无 I/O）：`lazy.ts`、`simple-options.ts`、`transform-messages.ts`、
`openai-prompt-cache.ts`、`github-copilot-headers.ts`、`azure-openai-config.ts`、
`constrained-sampling.ts`、`classifier-shared.ts`，以及 **6 个**属于本单元的 `*.lazy.ts` 包装器
（`anthropic-messages` / `openai-completions` / `openai-responses` / `openai-codex-responses` /
`azure-openai-responses` / `openai-decisions`）。目录里另有 9 个 `*.lazy.ts` 属于**其他单元**
（bedrock / google-generative-ai / google-vertex / mistral / kiana-messages / cloudflare-workers-ai-system-one /
llama-cpp-classify / typesafe-system-one / openrouter-images），本单元**不实现**它们。

### 1.2 与其他包/单元的关系

- **上游依赖（本单元消费）**：
  - `types.ts`：`Model`、`Api`、`Message`、`AssistantMessage`、`AssistantMessageEvent`、`Tool`、
    `Usage`、`StreamOptions`、`SimpleStreamOptions`、`ProviderStreams`、`ProviderClassifier`、
    `TranscriptContext`、各类 compat 类型（`AnthropicMessagesCompat`/`OpenAICompletionsCompat`/`OpenAIResponsesCompat`）。
  - `utils/event-stream.ts`：`AssistantMessageEventStream`（事件协议 + `result()` + `durationMs` 计时）。
  - `utils/transcript.ts`：`normalizeContext`、`resolveTranscript`、`resolveTranscriptTools`、
    `getCurrentTools`、`getInitialSystemMessage`、`getDeclaredTools`（**不含** `collapseSystemMessages`——
    本单元零引用）。
  - `utils/text.ts`：`getSystemMessageText`、`renderSystemMessageUpdate`（**不含** `contentText`；
    后者在 `openai-responses-shared.ts:753` 是局部变量）。
  - `utils/streaming-json.ts`：`ToolCallArgumentsStream`（**不含** `StreamingJsonParser`）。
  - `utils/json-parse.ts`：`parseJsonWithRepair`（**仅此一个**；`parseToolCallArguments`/`repairJson`
    是 `parseJsonWithRepair` 的内部细节，本单元不直接 import）。
  - `utils/provider-retry.ts`：`retryProviderRequest`（本单元只调用它；`noRetryStatuses` 作为**入参**
    由 `openai-decisions` 传入 `[504]`）。
  - `utils/error-body.ts`：`normalizeProviderError`、`formatProviderError`（`MAX_PROVIDER_ERROR_BODY_CHARS`
    本单元不引用）。
  - `utils/identity.ts`：`getIdentityHeaders`、`ClientIdentity`（`formatUserAgent`/`getAttributionHeaders`
    是 `getIdentityHeaders` 的内部实现，本单元不直接 import）。
  - `utils/openrouter-cost.ts`：`isOpenRouterModel`、`applyOpenRouterReportedCost`。
  - `utils/hash.ts`：`shortHash`；`utils/uuid.ts`：`uuidv7`。
  - `utils/sanitize-unicode.ts`：`sanitizeSurrogates`。
  - `utils/headers.ts`：`headersToRecord`、`providerHeadersToRecord`。
  - `utils/diagnostics.ts`：`appendAssistantMessageDiagnostic`、`createAssistantMessageDiagnostic`、
    `formatThrownValue`（Codex 记 `lastWebSocketError` 用）。
  - `utils/abort-signals.ts`：`combineAbortSignals`（Anthropic 的 SSE/请求超时组合、Codex 的 header 超时组合）。
  - `utils/estimate.ts`：`estimateContextTokens`（**被 `simple-options.ts` 的 `clampMaxTokensToContext` 调用**）。
  - `utils/node-http-proxy.ts`：`resolveHttpProxyUrlForTarget`（Codex WebSocket 的 Bun 代理路径）。
  - `utils/provider-env.ts`：`getProviderEnvValue`（含 Bun sandbox `/proc/self/environ` 回退）。
  - `models.ts`：`calculateCost`、`clampThinkingLevel`（`getSupportedThinkingLevels` 只在测试里用，不在本单元运行时路径）。
  - `session-resources.ts`：`registerSessionResourceCleanup`。
  - `env-api-keys.ts`：`ANTHROPIC_FEDERATION_RULE_ID_ENV` 等常量。
- **下游消费者（依赖本单元）**：`compat.ts`（旧式全局入口）、`providers/*.ts`（provider 工厂，
  例如 `providers/azure.ts` 直接使用 `azureOpenAIResponsesApi()` 与 `resolveAzureBaseUrl`）、
  `index.ts` 的 API 注册表、以及 `kiana` 的 coding-agent 层（通过 `Models.stream()` 间接调用）。

### 1.3 统一入口契约（`ProviderStreams`）

每个流式模块必须导出两个同名函数，签名形状一致：

```ts
export const stream: StreamFunction<"<api>", XxxOptions> = (
  model: Model<"<api>">, context: TranscriptContext, options?: XxxOptions
) => AssistantMessageEventStream;

export const streamSimple: StreamFunction<"<api>", SimpleStreamOptions> = (
  model: Model<"<api>">, context: TranscriptContext, options?: SimpleStreamOptions
) => AssistantMessageEventStream;
```

**契约要点（所有适配器一致，Rust 必须照做）**：

1. `stream`/`streamSimple` **同步返回** `AssistantMessageEventStream`；所有网络与解析工作在一个
   内部 async 任务里进行，失败以 `error` 事件终结流，**不从函数中抛出**。
2. 例外：**缺少鉴权**时 `streamSimple` 允许**同步抛出** `No API key for provider: <provider>`。
   `anthropic-messages`、`openai-completions`、`openai-responses`、`openai-codex-responses`、
   `azure-openai-responses` 的 `streamSimple` 都会先做鉴权检查并同步抛出。
   （`openai-decisions.classify` 是 `async` 函数，因此是返回 rejected Promise，而不是同步抛出。）
3. 事件顺序：`start` 必须最先（`partial` 为初始 `AssistantMessage`），然后按内容块的
   开/增量/关顺序，最后 `done` 或 `error`。
4. `output.stopReason` 初值 `"pending"`；`done` 前必须已被映射成 `stop`/`length`/`toolUse` 之一，
   否则抛错（见各模块）。
5. `timestamp: Date.now()` 在流创建时写入；`AssistantMessageEventStream` 负责补 `durationMs`。
6. 任何 `catch` 分支都要：`output.stopReason = options?.signal?.aborted ? "aborted" : "error"`，
   写 `output.errorMessage`，push `{ type: "error", reason: output.stopReason, error: output }`，再 `end()`。

### 1.4 `streamSimple` 的通用形状

所有适配器的 `streamSimple` 都遵循同一骨架：

```ts
// 1) 同步鉴权检查（抛 `No API key for provider: ${model.provider}`）
// 2) base = { ...buildBaseOptions(model, context, options, options?.apiKey), toolChoice: options?.toolChoice }
// 3) 推理级别夹取（Anthropic 例外，见 2.9.4）
// 4) return stream(model, context, { ...base, reasoningEffort / thinkingEnabled ... })
```

- `openai-completions` / `openai-responses` / `azure-openai-responses` / `openai-codex-responses`：
  `clampedReasoning = options?.reasoning ? clampThinkingLevel(model, options.reasoning) : undefined;`
  `reasoningEffort = clampedReasoning === "off" ? undefined : clampedReasoning;`
- `anthropic-messages`：**不调用** `clampThinkingLevel`，改用 `mapThinkingLevelToEffort`（见 2.9.4）。
- `openai-codex-responses` 的 `stream` 还允许 `reasoningEffort === "none"`（其选项联合类型含 `"none"`）。

### 1.5 `lazy` 入口

每个模块都有 `xxx.lazy.ts` 包装器，导出 `xxxApi(): ProviderStreams`，内部 `lazyApi(() => import(...))`，
**首次调用 `stream`/`streamSimple` 时才加载实现模块**。`openai-decisions.lazy.ts` 是分类器版，
直接返回 `{ classify: async (...) => (await import(...)).classify(...) }`。

---

## 2. 逐文件规范

### 2.1 `lazy.ts` 与全部 `*.lazy.ts`

#### 2.1.1 `createSetupErrorMessage(model, error, timestamp)`（内部）

返回一个 `AssistantMessage`：

```json
{
  "role": "assistant",
  "content": [],
  "api": model.api,
  "provider": model.provider,
  "model": model.id,
  "usage": { "input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
             "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0} },
  "stopReason": "error",
  "errorMessage": <error instanceof Error ? error.message : String(error)>,
  "timestamp": <timestamp>
}
```

字段顺序即上表顺序（影响 `JSON.stringify`，见 6.3 的 JSON 键序注意项）。

#### 2.1.2 `LazyStreamObserver`（导出接口）

```ts
export interface LazyStreamObserver {
  onEvent?(event: AssistantMessageEvent): void;
  onResult?(result: AssistantMessage | undefined): void;
}
```

纯观察者，**只读**；`onResult` 恰好调用一次（由 `observedResult` 守卫）。

#### 2.1.3 `lazyStream(model, setup, observer?) -> AssistantMessageEventStream`（导出）

- 记录 `startedAt = Date.now()`。
- 立即返回 `outer = new AssistantMessageEventStream()`。
- 异步执行 `setup()`：
  - 成功：`forwardStream(outer, inner, observer, observeResult)`。
  - 失败：构造 `createSetupErrorMessage(model, error, startedAt)`，push
    `{ type: "error", reason: "error", error: message }`，调用 `observer?.onEvent?.(event)`，
    `outer.end(message)`，`observeResult(message)`。
- `forwardStream` 逻辑：
  ```
  let result = undefined
  for await (event of source):
      target.push(event); observer?.onEvent?.(event)
      if event.type === "done"  -> result = event.message
      else if "error"           -> result = event.error
  if hasResult(source): result = await source.result()
  target.end(result)
  observeResult(result)
  ```
  `hasResult` 判定：`typeof source.result === "function"`。
- **注意**：`target.push` 之后再调 `observer.onEvent`（顺序固定）。

#### 2.1.4 `LazyApiCapabilities` / `lazyApi(load, capabilities?) -> ProviderStreams`（导出）

```ts
export interface LazyApiCapabilities { fetchDeferred?: boolean; cancelDeferred?: boolean; }
```

- `api.stream` / `api.streamSimple` = `lazyStream(model, async () => (await load()).stream(...))`。
- `capabilities.fetchDeferred` 时挂 `api.fetchDeferred`：同样用 `lazyStream` 包裹；
  若实现模块没有 `fetchDeferred`，抛 `Error("API does not support deferred responses")`。
- `capabilities.cancelDeferred` 时挂 `api.cancelDeferred`（`async`）：实现缺失时抛
  `Error("API cannot cancel deferred responses")`。
- 本单元六个 `*.lazy.ts` 都**不传** capabilities：
  - `anthropicMessagesApi`、`openAICompletionsApi`、`openAIResponsesApi`、
    `openAICodexResponsesApi`、`azureOpenAIResponsesApi`、`openAIDecisionsApi`。
  - 精确实现：`export const anthropicMessagesApi = (): ProviderStreams => lazyApi(() => import("./anthropic-messages.ts"));`
- 同目录下**不属于本单元**但同构的懒加载器（供参考，不要实现）：`bedrockConverseStreamApi`、
  `googleGenerativeAIApi`、`googleVertexApi`、`mistralConversationsApi`、`kianaMessagesApi`、
  `cloudflareWorkersAISystemOneApi`、`llamaCppClassifyApi`、`typesafeSystemOneApi`、`openrouterImagesApi`。

#### 2.1.5 测试断言的懒加载语义（`lazy-module-load.test.ts`）

- 导入根 barrel、构建全部内置 provider、导入 `compat` 入口时：**不得**加载任何 provider SDK。
- 通过 lazy API 流式请求 Anthropic 时：**只**加载 `@anthropic-ai/sdk`。
- 该"按需加载"在 Rust 中是编译期静态的；**验证方式改为**：`kiana` 不得在模块初始化时构造
  HTTP 客户端 / 读取 env / 建连接池。等价约束：所有 SDK 客户端构造必须发生在 `stream` 的异步任务内部。

### 2.2 `openai-prompt-cache.ts`

```ts
export const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH = 64;

export function clampOpenAIPromptCacheKey(key: string | undefined): string | undefined {
  if (key === undefined) return undefined;
  const chars = Array.from(key);
  if (chars.length <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH) return key;
  return chars.slice(0, OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH).join("");
}
```

**关键语义**：`Array.from` 按 **Unicode 码点**切分（非 UTF-16 code unit）。即截断长度是"码点数 ≤ 64"，
不是 `String.length ≤ 64`，也不是字节数。Rust 侧用 `chars().take(64)` 精确等价；**不要**用
`&s[..64]`（会 panic 或按字节截断），也不要用 `floor_char_boundary`。

边界：
- `undefined` → `undefined`（调用方需区分"字段不出现"与"字段为 null"；TS 端 `prompt_cache_key: undefined`
  在 JSON 序列化时**字段消失**）。
- 空串 → 返回 `""`（字段**出现**且为空串）。
- 恰好 64 码点 → 原样返回（同一字符串引用语义无关）。
- 65 码点 → 前 64 码点。
- 含 emoji（如 4 字节 UTF-8 / 2 个 UTF-16 code unit）→ 按 1 个码点计。

被三处调用：`openai-completions.buildParams`（`prompt_cache_key`）、
`openai-responses.buildParams`（`prompt_cache_key`）、`azure-openai-responses.buildParams`、
`openai-codex-responses.stream`（`codexSessionId`，同时用于 `prompt_cache_key` 与
`session-id`/`x-client-request-id` 头、WebSocket 请求 id）。

### 2.3 `github-copilot-headers.ts`

```ts
export function inferCopilotInitiator(messages: Message[]): "user" | "agent" {
  const last = messages[messages.length - 1];
  return last && last.role !== "user" ? "agent" : "user";
}
```
- 空数组 → `"user"`。
- 最后一条是 `user` → `"user"`；否则（assistant / toolResult / system）→ `"agent"`。

```ts
export function hasCopilotVisionInput(messages: Message[]): boolean
```
- 任一 `user` 或 `toolResult` 消息（`content` 为数组时）含 `type === "image"` 的块 → `true`。
- `system` / `assistant` 消息里的图片**不计**。

```ts
export function buildCopilotDynamicHeaders(params: { messages: Message[]; hasImages: boolean }): Record<string,string>
```
固定返回（顺序固定）：
```json
{ "X-Initiator": "user"|"agent", "Openai-Intent": "conversation-edits" }
```
当 `hasImages` 为真时追加 `"Copilot-Vision-Request": "true"`。
注意大小写与键名逐字：`X-Initiator`、`Openai-Intent`（`Intent` 首字母大写、`Openai` 全小写 o）、
`Copilot-Vision-Request`。

调用点：
- `anthropic-messages.stream`：仅当 `model.provider === "github-copilot"`，用 `normalizedContext.messages`
  （即 `resolveTranscript` 之后）计算 `hasImages`。
- `openai-completions.createClient`、`openai-responses.createClient`：同样仅 Copilot，用传入的 `context.messages`。

### 2.4 `simple-options.ts`

常量：
```ts
const CONTEXT_SAFETY_TOKENS = 4096;
const MIN_MAX_TOKENS = 1;
export const MIN_ANSWER_TOKENS = 1024;
export const DEFAULT_THINKING_BUDGETS: ThinkingBudgets = { minimal: 1024, low: 2048, medium: 8192, high: 16384 };
```

#### 2.4.1 `clampMaxTokensToContext(model, context, maxTokens) -> number`

```
if model.contextWindow <= 0: return max(MIN_MAX_TOKENS, maxTokens)
available = model.contextWindow - estimateContextTokens(context).tokens - CONTEXT_SAFETY_TOKENS
return min(maxTokens, max(MIN_MAX_TOKENS, available))
```
- `estimateContextTokens` 来自 `utils/estimate.ts`：若有"最近一次可用 assistant usage"，用
  `totalTokens || input+output+cacheRead+cacheWrite` 加其后消息的估算；否则对全部消息估算。
  字符/令牌比 `CHARS_PER_TOKEN = 3.5`，图片按 `ESTIMATED_IMAGE_CHARS = 4800`。
  文本令牌 `Math.ceil(chars / 3.5)`；`system` 消息还计入 `toolsAdded`/`toolsRemoved` 的
  `JSON.stringify` 长度。
- 该函数可能把 `maxTokens` 压到 **1**（永不为 0 或负）。

#### 2.4.2 `resolveSamplingParams(model, thinkingLevel, requestParams?) -> SamplingParams | undefined`

```
effective = clampThinkingLevel(model, thinkingLevel)
levelParams = model.samplingParamsByThinkingLevel?.[effective]
return (model.samplingParams || levelParams || requestParams)
  ? { ...model.samplingParams, ...levelParams, ...requestParams }
  : undefined
```
- 键级覆盖顺序：model 默认 < 思考级别 < 请求。
- 三者皆空 → `undefined`（调用方不得写字段）。

#### 2.4.3 `buildBaseOptions(model, context, options?, apiKey?) -> StreamOptions`

逐字段复制（**顺序即对象字面量顺序**）：
```
temperature: options?.temperature
samplingParams: resolveSamplingParams(model, options?.reasoning ?? "off", options?.samplingParams)
maxTokens: clampMaxTokensToContext(model, context, options?.maxTokens ?? model.maxTokens)
signal, telemetry, apiKey: apiKey || options?.apiKey, fetch, transport,
cacheRetention, sessionId, headers, identity, onPayload, onResponse,
onProviderStreamEvent, timeoutMs, websocketConnectTimeoutMs, maxRetries,
maxRetryDelayMs, metadata, env
```
- `apiKey` 参数优先；空串回落到 `options.apiKey`（JS `||` 语义）。
- 注意 `maxTokens` 用的是 `options?.maxTokens ?? model.maxTokens`（`undefined` 才回落，`0` 保留）。

#### 2.4.4 思考预算族

```ts
export function clampReasoning(effort: ThinkingLevel | undefined) {
  return effort === "xhigh" || effort === "max" ? "high" : effort;
}

export function thinkingBudgetForLevel(reasoningLevel: ThinkingLevel, customBudgets?: ThinkingBudgets): number {
  const budgets = { ...DEFAULT_THINKING_BUDGETS, ...customBudgets };
  const level = clampReasoning(reasoningLevel)!;
  return budgets[level]!;
}
```
- `"off"` 也合法输入（不是 `undefined`），此时 `budgets["off"]` 为 `undefined`（类型断言掩盖）。
  实际调用点都先保证 `reasoningEffort` 非空（见 `resolveClampedThinkingBudget`），Rust 侧对此应
  返回 `Option<u32>` 或用 `expect` 明确。

```ts
export function clampThinkingBudgetToAnswerRoom(thinkingBudget: number, ceiling: number): number {
  return Math.min(thinkingBudget, Math.max(0, ceiling - MIN_ANSWER_TOKENS));
}

export function adjustMaxTokensForThinking(
  baseMaxTokens: number | undefined, modelMaxTokens: number, reasoningLevel: ThinkingLevel, customBudgets?: ThinkingBudgets
): { maxTokens: number; thinkingBudget: number }
```
```
thinkingBudget = thinkingBudgetForLevel(reasoningLevel, customBudgets)
maxTokens = baseMaxTokens === undefined ? modelMaxTokens : min(baseMaxTokens + thinkingBudget, modelMaxTokens)
if maxTokens <= thinkingBudget:
    thinkingBudget = clampThinkingBudgetToAnswerRoom(thinkingBudget, maxTokens)
return { maxTokens, thinkingBudget }
```
- `baseMaxTokens === undefined` 表示"调用方未设上限"，此时**不**加预算（防止预算吞掉全部 max_tokens）。

#### 2.4.5 使用点

- `anthropic-messages.streamSimple`：`adjustMaxTokensForThinking(base.maxTokens, model.maxTokens, options.reasoning, options.thinkingBudgets)`，
  再 `clampMaxTokensToContext`，再 `thinkingBudgetTokens = Math.min(adjusted.thinkingBudget, Math.max(0, maxTokens - 1024))`。
  注意：这里**硬编码 1024**，不是 `MIN_ANSWER_TOKENS` 常量引用（值相同）。
- `openai-completions.buildParams`：`resolveClampedThinkingBudget`（见 2.10.4）。
- `openai-completions.buildParams` 末尾与 `openai-responses.buildParams`、`azure-openai-responses.buildParams`
  末尾：`resolveSamplingParams(model, reasoningEffort ?? "off", options.samplingParams)`，
  非空则 `Object.assign(params, samplingParams)`（**最后**合并，可覆盖具名字段）。

### 2.5 `transform-messages.ts`

常量（逐字）：
```ts
const NON_VISION_USER_IMAGE_PLACEHOLDER = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER = "(tool image omitted: model does not support images)";
const ERROR_TOOL_IMAGE_PLACEHOLDER = "(image attached after the tool results)";
```

#### 2.5.1 `splitErrorToolResultImages(msg) -> SplitErrorToolResult | undefined`（导出）

```ts
export interface SplitErrorToolResult {
  content: TextContent[];
  caption: string;
  images: ImageContent[];
}
```
- `!msg.isError` → `undefined`。
- 无 image 块 → `undefined`。
- 否则：`content = replaceImagesWithPlaceholder(msg.content, ERROR_TOOL_IMAGE_PLACEHOLDER)`，
  `caption = \`Images from the error result of tool call ${msg.toolCallId} (${msg.toolName}):\``，
  `images` = 原 image 块（保持顺序）。

#### 2.5.2 `replaceImagesWithPlaceholder(content, placeholder)`（内部）

```
result = []; previousWasPlaceholder = false
for block of content:
    if block.type === "image":
        if !previousWasPlaceholder: result.push({type:"text", text: placeholder})
        previousWasPlaceholder = true
        continue
    result.push(block)
    previousWasPlaceholder = (block.text === placeholder)
```
**要点**：
- **连续**多张图片只插入**一个**占位文本。
- 若原文本块内容**恰好等于**占位文本，也视为"上一个就是占位"，从而抑制重复占位。
- 非连续（中间夹文本）则各自插入。

#### 2.5.3 `downgradeUnsupportedImages(messages, model)`（内部）

- `model.input.includes("image")` → 原样返回（同一引用）。
- 否则逐条：
  - `user` 且 `content` 为数组 → `{ ...msg, content: replaceImagesWithPlaceholder(content, NON_VISION_USER_IMAGE_PLACEHOLDER) }`
  - `toolResult` → `{ ...msg, content: replaceImagesWithPlaceholder(content, NON_VISION_TOOL_IMAGE_PLACEHOLDER) }`
  - 其他原样（同一引用）。

#### 2.5.4 `transformMessages(messages, model, normalizeToolCallId?) -> Message[]`（导出）

签名中 `normalizeToolCallId?: (id, model, source: AssistantMessage) => string`。

**准备**：
1. `toolCallIdMap = new Map<string,string>()`
2. 规范化 null/undefined content：`messages.map(msg => msg.content == null ? { ...msg, content: [] } : msg)`
   （注意：只补 `content`，其余字段与引用保持）。
3. `imageAwareMessages = downgradeUnsupportedImages(normalizedMessages, model)`

**第一遍 map（保持顺序）**：
- `system` / `user` → 原样返回。
- `toolResult` → 若 `toolCallIdMap` 有映射且新 id 不同，`{ ...msg, toolCallId: normalizedId }`；否则原样。
- `assistant`：
  - `isSameModel = provider 相同 && api 相同 && model.id 相同`。
  - `content.flatMap`：
    - `thinking`：
      - `!isSameModel` → 丢弃（返回 `[]`）。**理由**：签名/加密推理只对产出它的模型有效，且把可读
        推理当纯文本发会让新模型把它当可见回复。
      - `block.redacted || block.thinkingSignature` → 保留（即使 thinking 文本为空）。
      - `!block.thinking || block.thinking.trim() === ""` → 丢弃。
      - 否则保留。
    - `text`：`isSameModel` → 原块；否则 `{ type:"text", text: block.text }`（丢弃 `textSignature`）。
    - `toolCall`：
      - `!isSameModel && toolCall.thoughtSignature` → 浅拷贝后 `delete thoughtSignature`。
      - `!isSameModel && normalizeToolCallId` → 归一化 id；若变化则记入 `toolCallIdMap` 并替换。
      - 返回（可能修改过的）toolCall。
    - 其他块 → 原块。
  - 返回 `{ ...assistantMsg, content: transformedContent }`。
- 其他 role → 原样。

  注意 `flatMap` 的顺序：`thinking` 被丢弃后，块位置不影响相对顺序。

**第二遍（合成为孤儿 toolCall 的 toolResult）**：
状态：
```
result: Message[] = []
pendingToolCalls: ToolCall[] = []
existingToolResultIds = new Set<string>()
heldSystemMessages: Message[] = []
```
`closePendingToolCalls()`：
```
if pendingToolCalls.length > 0:
    for tc of pendingToolCalls:
        if !existingToolResultIds.has(tc.id):
            result.push({
              role: "toolResult", toolCallId: tc.id, toolName: tc.name,
              content: [{ type: "text", text: "No result provided" }],
              isError: true, timestamp: Date.now(),
            })
    pendingToolCalls = []; existingToolResultIds = new Set()
result.push(...heldSystemMessages); heldSystemMessages.length = 0
```
主循环：
- `assistant`：先 `closePendingToolCalls()`；若 `stopReason === "error" || "aborted"` → **整条跳过**
  （`continue`，不 push，也不清除它自己的 toolCalls——因为已经跳过）；否则若含 toolCall，
  设 `pendingToolCalls` 与该消息的 toolCalls、`existingToolResultIds = new Set()`；然后 push。
- `toolResult`：`existingToolResultIds.add(msg.toolCallId)`；push。
- `system`：若 `pendingToolCalls.length > 0` → 暂存到 `heldSystemMessages`；否则直接 push。
  **系统消息对 toolCall 记账透明**：夹在 toolCall 与其结果之间的系统消息被推迟到结果之后发出，
  这样不会对"稍后才回答的调用"产生重复结果。
- `user`：先 `closePendingToolCalls()`；push。
- 其他：push。
循环结束后再 `closePendingToolCalls()`（末尾未解析的调用）。

**验证要点**：
- 合成结果的文本固定为 `"No result provided"`，`isError: true`，`timestamp: Date.now()`。
- 被跳过的 `error`/`aborted` assistant 消息**不参与** `pendingToolCalls` 追踪。
- `toolCallIdMap` 只对**跨模型**（`!isSameModel`）的 toolCall 生效。

### 2.6 `azure-openai-config.ts`

```ts
const DEFAULT_AZURE_API_VERSION = "v1";

export interface AzureEndpointOptions extends StreamOptions {
  azureApiVersion?: string;
  azureResourceName?: string;
  azureBaseUrl?: string;
  azureDeploymentName?: string;
}
```

#### 2.6.1 `parseDeploymentNameMap(value?) -> Map<string,string>`（内部）

- 空/未定义 → 空 Map。
- 按 `","` 分割；每项 `trim()`；空项跳过。
- 按**第一个** `"="` 分成 `[modelId, deploymentName]`（`split("=", 2)` 语义：只切一次，
  值内可再含 `=` —— 但 JS 的 `split("=", 2)` 会**丢弃**第 3 段之后的内容；Rust 用 `splitn(2,'=')`）。
- 任一为空 → 跳过该项。
- 否则 `map.set(modelId.trim(), deploymentName.trim())`（后者覆盖前者）。

#### 2.6.2 `resolveDeploymentName(model, options?) -> string`（导出）

```
if options?.azureDeploymentName: return it
mapped = parseDeploymentNameMap(getProviderEnvValue("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", options?.env)).get(model.id)
return mapped || model.id
```
- `mapped` 为空串时回落 `model.id`（`||` 语义）。

#### 2.6.3 `normalizeAzureBaseUrl(baseUrl) -> string`（内部，未导出）

```
trimmed = baseUrl.trim().replace(/\/+$/, "")
url = new URL(trimmed)           // 失败 -> throw Error(`Invalid Azure OpenAI base URL: ${baseUrl}`)
isAzureHost = hostname.endsWith(".openai.azure.com")
           || hostname.endsWith(".cognitiveservices.azure.com")
           || hostname.endsWith(".ai.azure.com")
normalizedPath = url.pathname.replace(/\/+$/, "")
if isAzureHost && (normalizedPath === "" || "/" || "/openai" || "/openai/v1/responses"):
    url.pathname = "/openai/v1"; url.search = ""
return url.toString().replace(/\/+$/, "")
```
**要点**：
- 错误串里用的是**原始**（未 trim）的 `baseUrl`。
- 只有 `isAzureHost` 且路径属于那 4 种之一时才改写；其他路径（含 `/openai/v1`、`/openai/v2`、
  代理自定义路径）**保持**。
- 改写时**清空 query**；非 Azure 主机**保留** query。
- 末尾斜杠全去掉；`url.toString()` 会规范化（例如补 `/`、小写 host、保留端口）。
- 判定用 `endsWith`，故 `evil-openai.azure.com` 也算（`*.openai.azure.com` 后缀）。

#### 2.6.4 `buildDefaultBaseUrl(resourceName)`（内部）

```ts
return `https://${resourceName}.openai.azure.com/openai/v1`;
```

#### 2.6.5 `resolveAzureBaseUrl(model, options?) -> string`（导出）

优先级（**逐级回落**）：
1. `options?.azureBaseUrl?.trim()`（空串视为无）
2. `getProviderEnvValue("AZURE_OPENAI_BASE_URL", options?.env)?.trim()`
3. `options?.azureResourceName || getProviderEnvValue("AZURE_OPENAI_RESOURCE_NAME", options?.env)`
   → `buildDefaultBaseUrl(resourceName)`
4. `model.baseUrl`
5. 全空 → 抛错（逐字）：
   ```
   Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.
   ```
最终都过 `normalizeAzureBaseUrl`。

#### 2.6.6 `resolveAzureConfig(model, options?) -> { baseUrl, apiVersion }`（导出）

```
baseUrl = resolveAzureBaseUrl(model, options)
apiVersion = options?.azureApiVersion
          || getProviderEnvValue("AZURE_OPENAI_API_VERSION", options?.env)
          || "v1"
```

### 2.7 `constrained-sampling.ts`

#### 2.7.1 类型与错误

```ts
class UnsupportedStrictJsonSchemaError extends Error {}   // 内部，仅按 instanceof 判定
export type UnsupportedStrictSchemaKeywordCheck = (key: string, value: unknown) => boolean;
export interface GrammarConstrainedSampling {
  format: "lark" | "regex";
  definition: string;
  inputProperty: string;
}
export interface GrammarToolInputJsonBuffer {
  input: string;
  started: boolean;
  closed: boolean;
}
```

`UNSUPPORTED_STRICT_SCHEMA_KEYS`（顺序固定，逐字）：
```
$ref, $defs, definitions, allOf, oneOf, patternProperties, dependentSchemas,
dependencies, unevaluatedProperties, propertyNames, contains, prefixItems,
not, if, then, else
```

#### 2.7.2 `isStructuredSchema(schema)`（内部）

`isJsonSchemaObject(schema)`（object 且非 null 且非数组）且：
`type` 为字符串数组时取 `[type]`，数组时取自身，否则 `[]`；
返回 `types 含 "object" || types 含 "array" || schema.properties !== undefined || schema.items !== undefined`。

#### 2.7.3 `schemaAllowsNull(schema)`（内部）

```
if !isJsonSchemaObject: false
if schema.type === "null" or (array && includes "null"): true
if schema.const === null or (array enum && includes null): true
if anyOf 是数组且任一 variant allowsNull: true
else false
```

#### 2.7.4 `makeJsonSchemaNodeStrict(schema, isUnsupportedKeyword?)`（内部，原地修改）

错误消息**逐字**（全部以 `UnsupportedStrictJsonSchemaError` 抛出）：

| 条件 | message |
|---|---|
| 非对象 schema | `boolean schemas are unsupported` |
| 命中 `UNSUPPORTED_STRICT_SCHEMA_KEYS` 中某键（值 !== undefined） | `` `${key} schemas are unsupported` `` |
| 自定义检查命中 | `` `${key}: ${JSON.stringify(value)} is unsupported` `` |
| `anyOf` 非数组或空 | `anyOf must contain at least one schema` |
| `anyOf` 某分支是结构化 schema | `object and array unions are unsupported` |
| `items` 是数组 | `tuple schemas are unsupported` |
| `properties !== undefined && type !== "object"` | `properties require type object` |
| `additionalProperties !== undefined && !== false` | `schema-valued or true additionalProperties is unsupported` |
| `properties !== undefined && !isJsonSchemaObject(properties)` | `object properties must be a schema map` |
| `required` 非数组或含非字符串 | `object required must be a string array` |
| `required` 含未知属性名 | `required contains an unknown property` |

执行顺序（**严格按此顺序**，因为先抛哪个决定错误文本）：
1. 对象检查
2. `UNSUPPORTED_STRICT_SCHEMA_KEYS` 循环（数组顺序）
3. 自定义 `isUnsupportedKeyword` 遍历 `Object.entries(schema)`（**插入顺序**）
4. `anyOf` 处理：先数组非空检查，再逐分支"结构化"检查 + 递归
5. `items` 处理：元组检查，再递归
6. `properties` 与 `type` 一致性
7. `!isObjectSchema` → **提前 return**（不再处理 additionalProperties/required）
8. `additionalProperties`
9. `properties` 是 map
10. `required` 是 string array
11. `required` 未知属性检查
12. 对每个属性递归；若该属性**不在 required 中**且 **不允许 null**，替换为
    `{ anyOf: [property, { type: "null" }] }`
13. `schema.required = 所有 propertyNames`（**全部**属性都必填，顺序为 `Object.keys(properties)`）
14. `schema.additionalProperties = false`

注意第 12 步**原地替换** `properties[key]`，且新增的 `anyOf` 节点**不再**递归校验
（null 分支是可信的）。
注意第 13 步把 `required` 改写为属性全名数组——**这会改变原有 required 集合**。

#### 2.7.5 `makeStrictJsonSchema(schema, isUnsupportedKeyword?) -> Record<string, unknown>`（导出）

```
cloned = structuredClone(schema)                 // 深拷贝，保留原型语义（此处为纯 JSON）
if !isJsonSchemaObject(cloned): throw UnsupportedStrictJsonSchemaError("root schema must have type object")
makeJsonSchemaNodeStrict(cloned, isUnsupportedKeyword)
if cloned.type !== "object": throw UnsupportedStrictJsonSchemaError("root schema must have type object")
return cloned
```
- 深拷贝语义：`structuredClone` 不支持函数/符号/循环；JSON schema 应为纯数据。
  Rust 侧用 `serde_json::Value::clone()` 即可（注意 TS 的 `undefined` 字段会消失）。

#### 2.7.6 `getJsonSchemaToolParameters(tool, strict) -> Tool["parameters"]`（导出）

```
return strict === true ? makeStrictJsonSchema(tool.parameters) : tool.parameters
```
- **不缓存**：每次调用都重新转换（Anthropic 的 `convertTools` 每请求调用）。

#### 2.7.7 `getGrammarToolInput(toolName, arguments_, inputProperty) -> string`（导出）

```
input = arguments_[inputProperty]
if typeof input !== "string":
  throw new Error(`Grammar tool call "${toolName}" requires argument "${inputProperty}" to be a string.`)
return input
```

#### 2.7.8 `appendGrammarToolInputJsonDelta(buffer, inputProperty, inputDelta) -> string | undefined`（导出）

```
if buffer.closed:
  throw new Error(`grammar tool input for property "${inputProperty}" changed after it was closed`)
if inputDelta.length === 0: return undefined
buffer.input += inputDelta
return grammarToolInputJsonStart(buffer, inputProperty) + JSON.stringify(inputDelta).slice(1, -1)
```
- 返回的是"JSON 字符串字面量内部片段"：`JSON.stringify` 去掉首尾引号。
- `grammarToolInputJsonStart`（内部）：
  ```
  if buffer.started: return ""
  buffer.started = true
  return `{${JSON.stringify(inputProperty)}:"`
  ```
  即首次产出 `{"<prop>":"`。
- 复杂度 O(delta)，与已累积长度无关（不重新序列化整体）。

#### 2.7.9 `closeGrammarToolInputJson(buffer, inputProperty, finalInput) -> string | undefined`（导出）

```
if buffer.closed:
    if finalInput === undefined || finalInput === buffer.input: return undefined
    throw new Error(`grammar tool input for property "${inputProperty}" changed after it was closed`)
if finalInput !== undefined && !finalInput.startsWith(buffer.input):
    throw new Error(`grammar tool input for property "${inputProperty}" changed non-monotonically`)
rest = finalInput === undefined ? "" : finalInput.slice(buffer.input.length)
if finalInput !== undefined: buffer.input = finalInput
buffer.closed = true
return `${grammarToolInputJsonStart(buffer, inputProperty)}${JSON.stringify(rest).slice(1, -1)}"`
```
- 收尾产出 `"}`（若 start 尚未产出则先产 `{"<prop>":"`）。
- 重复 close 且 finalInput 与已累积相同（或 undefined）→ `undefined`（幂等）。

#### 2.7.10 `inferGrammarInputProperty(tool) -> string`（内部）

错误消息（普通 `Error`）：
- `grammar constrained sampling requires an object parameter schema`
- `grammar constrained sampling requires exactly one required string property`
- `` `grammar constrained sampling requires a properties entry for ${inputProperty}` ``
- `` `grammar constrained sampling property ${inputProperty} must have type string` ``

判定：
1. `parameters.type !== "object"` → 第 1 条
2. `required` 非数组、长度 !== 1、或元素非 string → 第 2 条
3. `inputProperty = required[0]`；`!schema.properties?.[inputProperty]` → 第 3 条
4. 该属性 `type !== "string"` → 第 4 条

#### 2.7.11 `resolveJsonSchemaStrictSampling(tool, supportsStrictMode, isUnsupportedKeyword?) -> boolean | undefined`（导出）

```
config = tool.constrainedSampling
if !config || config.type !== "json_schema": return undefined
if supportsStrictMode:
    try { makeStrictJsonSchema(tool.parameters, isUnsupportedKeyword); return true }
    catch e:
        if !(e instanceof UnsupportedStrictJsonSchemaError): throw e
        if config.strict !== "require": return undefined
        throw new Error(`Tool "${tool.name}" requires JSON-schema constrained sampling, but ${e.message}.`)
if config.strict === "require":
    throw new Error(`Tool "${tool.name}" requires JSON-schema constrained sampling, but strict tools are unsupported.`)
return undefined
```
- `config.strict` 取值：`"prefer" | "require"`（类型层）。`"prefer"` 失败回落 `undefined`（非 strict）。

#### 2.7.12 `resolveGrammarConstrainedSampling(tool, supportsOpenAIGrammarTools) -> GrammarConstrainedSampling | undefined`（导出）

```
config = tool.constrainedSampling
if !config || config.type !== "grammar": return undefined
if !supportsOpenAIGrammarTools: return undefined
lark = config.variants.openai_lark; regex = config.variants.openai_regex
hasLark = typeof lark === "string" && lark.trim().length > 0
hasRegex = typeof regex === "string" && regex.trim().length > 0
if !hasLark && !hasRegex:
    throw new Error(`Tool "${tool.name}" cannot use grammar constrained sampling: no supported grammar variant was provided.`)
try:
    return { format: hasLark ? "lark" : "regex", definition: hasLark ? lark : regex, inputProperty: inferGrammarInputProperty(tool) }
catch e:
    throw new Error(`Tool "${tool.name}" cannot use grammar constrained sampling: ${message(e)}.`)
```
- lark 优先于 regex。
- 注意 catch 包装会追加句点，因此嵌套消息形如
  `Tool "x" cannot use grammar constrained sampling: grammar constrained sampling requires an object parameter schema.`

#### 2.7.13 `createGrammarToolInputProperties(tools, supportsOpenAIGrammarTools) -> ReadonlyMap<string,string>`（导出）

对 `tools ?? []` 逐个 `resolveGrammarConstrainedSampling`，命中则 `map.set(tool.name, grammar.inputProperty)`。
- 同名工具后者覆盖。
- 会**抛错**（require/prefer 语义不适用：凡缺失变体即抛）。调用点都在请求构建早期，
  因而错误会进入 `error` 事件。

### 2.8 `classifier-shared.ts` 与 `openai-decisions.ts`

#### 2.8.1 `classifier-shared.ts`

```ts
export interface ClassifierHttpError extends Error {
  status: number | undefined;
  headers: Headers | undefined;
  body: string;
}
```
- `httpError(label, response, body)`：`new Error(\`${label} returned ${response.status}\`)`，
  附加 `status`、`headers`、`body`。
- `timeoutError(timeoutMs)`：`new Error(\`Request timed out after ${timeoutMs}ms\`)`，`name = "TimeoutError"`，
  `status/headers` 为 `undefined`，`body = ""`。
- `isRecord(value)`：object、非 null、非数组。
- `requiredNumber(label, value, field)`：非有限数字 → `` `${label} returned an invalid ${field}` ``。
- `requestHeaders(model, apiKey, optionsHeaders?, identity?)`：
  ```
  providerHeadersToRecord(
    getIdentityHeaders(model, identity),
    { authorization: `Bearer ${apiKey}`, "content-type": "application/json" },
    model.headers,
    optionsHeaders
  ) ?? {}
  ```
  `providerHeadersToRecord` 语义：按来源顺序合并，键名小写归一；`value === null` **删除**该键；
  最终返回 `Object.fromEntries`（键为**最后一次**写入时的原始大小写名）。空 → `undefined`。
- `postClassifierRequest(label, url, model, body, options, noRetryStatuses?) -> Promise<unknown>`：
  1. `!options?.apiKey` → 抛 `No API key for provider: ${model.provider}`。
  2. `payload = body`；`transformed = await options.onPayload?.(payload, model)`；非 undefined 则替换。
  3. `requestFetch = options.fetch ?? globalThis.fetch`。
  4. `retryProviderRequest(async () => {...}, { maxRetries: options.maxRetries ?? 2, maxRetryDelayMs, signal, noRetryStatuses })`
     每次尝试：
     - `timeoutSignal = options.timeoutMs !== undefined ? AbortSignal.timeout(options.timeoutMs) : undefined`
     - `signal = options.signal && timeoutSignal ? AbortSignal.any([...]) : (options.signal ?? timeoutSignal)`
     - `fetch(url, { method: "POST", headers: requestHeaders(...), body: JSON.stringify(payload), signal })`
     - `!next.ok` → `throw httpError(label, next, await next.text())`
     - 返回 `{ response: next, json: await next.json() }`
     - catch：若 `timeoutSignal?.aborted && !options.signal?.aborted` → `throw timeoutError(options.timeoutMs!)`
  5. `await options.onResponse?.({ status, headers: headersToRecord(response.headers) }, model)`
  6. 返回 `json`。
- `parseClassifierUsage(value, model) -> Usage | undefined`：
  - 非 record 或 `input_tokens`/`output_tokens` 都缺失 → `undefined`。
  - `tokenCount(v) = 有限数字且 > 0 ? v : 0`（注意**保留小数**，不做取整）。
  - `usage = { input, output, cacheRead: 0, cacheWrite: 0, totalTokens: input+output, cost: {...0} }`；
    `calculateCost(model, usage)`；返回。

#### 2.8.2 `openai-decisions.ts`

常量：
```ts
const LABEL = "OpenAI Decisions";
const MAX_IMAGES = 128;
const NO_RETRY_STATUSES = [504];
```

`DecisionQuestion`（线上形状）：
```ts
interface DecisionQuestion {
  type: "predicate" | "choice" | "score";
  name: string;
  instructions: string;
  choices?: Array<{ value: string; description?: string }>;
  levels?: Array<{ label: string }>;
}
```

- `predicateInstructions(question)`：
  ```
  meanings = [criteria.true ? `True means: ${criteria.true}` : "", criteria.false ? `False means: ${criteria.false}` : ""].filter(Boolean)
  return meanings.length > 0 ? `${question.instructions}\n\n${meanings.join("\n")}` : question.instructions
  ```
- `wireQuestion(name, question)`：
  - `choice`：`{ type:"choice", name, instructions, choices: Object.entries(criteria).map(([value, description]) => description ? { value, description } : { value }) }`
    （`criteria` 是 `Record<string, string|undefined>`；空 description 时**省掉**该键。）
  - `score`：`{ type:"score", name, instructions, levels: criteria.map(label => ({ label })) }`
  - `bool`：`{ type:"predicate", name, instructions: predicateInstructions(question) }`
- `wireInput(context)`：
  ```
  state = JSON.stringify(context.state)
  images = context.images ?? []
  if images.length === 0: return state
  if images.length > MAX_IMAGES:
      throw new Error(`${LABEL} accepts at most ${MAX_IMAGES} images, got ${images.length}`)
  return [{ role: "user", content: [
      { type: "input_text", text: state },
      ...images.map(image => ({ type: "input_image", image_url: `data:${image.mimeType};base64,${image.data}` }))
  ]}]
  ```
- `choiceProbabilities(value, id)`：
  - 非数组 → `` `${LABEL} returned invalid probabilities for ${id}` ``
  - 每项非 record 或 `value` 非 string → 同上
  - `probability` 必须有限数字，否则 `` `${LABEL} returned an invalid probability for ${id}.${entry.value}` ``
  - 返回 `Object.fromEntries`（同名后者覆盖；注意原型敏感键，见 6.3）。
- `parseAnswer(id, question, answer)`：
  - `answer.type === "refusal"` → `` `${LABEL} refused to answer ${id}` ``
  - `choice` 问题：`answer.type !== "choice" || typeof answer.choice !== "string"` →
    `` `${LABEL} did not return a choice answer for ${id}` ``；返回
    `{ type:"choice", choice, probabilities, confidence: requiredNumber(LABEL, answer.confidence, `confidence for ${id}`) }`
  - `score`：`answer.type !== "score"` → `` `${LABEL} did not return a score answer for ${id}` ``；
    返回 `{ type:"score", score: requiredNumber(..., `score for ${id}`), confidence: requiredNumber(..., `confidence for ${id}`) }`
  - `bool`：`answer.type !== "predicate"` → `` `${LABEL} did not return a predicate answer for ${id}` ``；
    返回 `{ type:"bool", probability: requiredNumber(..., `probability for ${id}`) }`
- `parseAnswers(value, context)`：
  - 非数组 → `` `${LABEL} returned an unexpected response` ``
  - `byName`：仅保存 `isRecord(answer) && typeof answer.name === "string"` 的项（后者覆盖）
  - 按 `Object.entries(context.questions)` 顺序；缺答案 → `` `${LABEL} did not return an answer for ${id}` ``
  - 返回 `Object.fromEntries`（**问题 ID 顺序 = questions 的键序**）
- `errorMessage(error)`：
  ```
  if error.status === 504:
      return `${LABEL} error (504): the request timed out at the gateway. Very large inputs (above roughly 600K tokens) currently exceed its time limit.`
  return formatProviderError(normalizeProviderError(error), `${LABEL} error`)
  ```
- `classify: ClassifierFunction<ClassifierOptions>`：
  ```ts
  output = { api, provider, model: model.id, answers: {}, stopReason: "stop", timestamp: Date.now() }
  try:
      if model.api !== "openai-decisions": throw new Error(`Unsupported classifier API: ${model.api}`)
      body = await postClassifierRequest(
          LABEL,
          new URL("decisions", `${model.baseUrl.replace(/\/+$/u, "")}/`),
          model,
          { model: model.id, input: wireInput(context),
            questions: Object.entries(context.questions).map(([id, q]) => wireQuestion(id, q)) },
          options, NO_RETRY_STATUSES)
      if !isRecord(body): throw new Error(`${LABEL} returned an unexpected response`)
      usage = parseClassifierUsage(body.usage, model)   // 先记用量：拒绝/畸形回答也已计费
      if usage: output.usage = usage
      output.answers = parseAnswers(body.answers, context)
      return output
  catch error:
      output.answers = {}
      output.stopReason = options?.signal?.aborted ? "aborted" : "error"
      output.errorMessage = errorMessage(error)
      return output
  ```
- 端点 URL 构造：`new URL("decisions", `${baseUrl 去掉尾部斜杠}/`)`。即 `https://api.openai.com/v1` →
  `https://api.openai.com/v1/decisions`。若 baseUrl 带路径，按 URL 相对解析规则拼接。
- 请求体：`{ "model", "input", "questions" }`；`input` 为 **JSON 字符串**（无图片）或数组（有图片）。
- 头：`authorization: Bearer <apiKey>`、`content-type: application/json`、
  以及 `getIdentityHeaders`（`User-Agent` + attribution）、`model.headers`、`options.headers`。

### 2.9 `anthropic-messages.ts`

文件 1690 行，是最复杂的模块之一。

#### 2.9.1 常量与工具函数

```ts
const claudeCodeVersion = "2.1.280";

const claudeCodeTools = [
  "Read","Write","Edit","Bash","Grep","Glob","AskUserQuestion","EnterPlanMode","ExitPlanMode",
  "KillShell","NotebookEdit","Skill","Task","TaskOutput","TodoWrite","WebFetch","WebSearch"
];   // 17 项，顺序固定

const ccToolLookup = new Map(claudeCodeTools.map(t => [t.toLowerCase(), t]));

const toClaudeCodeName = (name: string) => ccToolLookup.get(name.toLowerCase()) ?? name;

const fromClaudeCodeName = (name: string, tools?: Tool[]) => {
  if (tools && tools.length > 0) {
    const lowerName = name.toLowerCase();
    const matchedTool = tools.find(tool => tool.name.toLowerCase() === lowerName);
    if (matchedTool) return matchedTool.name;
  }
  return name;
};
```

Beta 常量（逐字）：
```ts
const FINE_GRAINED_TOOL_STREAMING_BETA = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA = "interleaved-thinking-2025-05-14";
const SERVER_SIDE_FALLBACK_BETA = "server-side-fallback-2026-07-01";
const MID_CONVERSATION_OUTPUT_CONFIG_BETA = "mid-conversation-output-config-2026-07-01";
const THINKING_BINDING_CONTROLS_BETA = "thinking-binding-controls-2026-08-01";
const INLINE_TOOLS_BETA = "inline-tools-2026-09-15";
```

OAuth 情形额外 beta（在 `getBetaFeatures` 中**最先**加入）：
```
"claude-code-20250219", "oauth-2025-04-20"
```

占位工具（逐字）：
```ts
const DEFERRED_TOOL_PLACEHOLDER: BetaTool = {
  name: "__kiana_deferred_placeholder__",
  description: "Reserved placeholder. Never available. Never call this.",
  input_schema: { type: "object", properties: {}, required: [] },
  defer_loading: true,
};
```

Claude Code 身份 system 文本（逐字）：
```
You are Claude Code, Anthropic's official CLI for Claude.
```

#### 2.9.2 `resolveCacheRetention` / `getCacheControl`（内部）

```ts
function resolveCacheRetention(cacheRetention?: CacheRetention, env?: ProviderEnv): CacheRetention {
  if (cacheRetention) return cacheRetention;
  if (getProviderEnvValue("KIANA_CACHE_RETENTION", env) === "long") return "long";
  return "short";
}

function getCacheControl(model, cacheRetention?, env?) {
  const retention = resolveCacheRetention(cacheRetention, env);
  if (retention === "none") return { retention };
  const ttl = retention === "long" && getAnthropicCompat(model).supportsLongCacheRetention ? "1h" : undefined;
  return { retention, cacheControl: { type: "ephemeral", ...(ttl && { ttl }) } };
}
```
- `cacheControl` 对象键序：`type` 然后（可选）`ttl`。
- `CacheRetention` 取值：`"none" | "short" | "long"`。

#### 2.9.3 `getAnthropicCompat(model)`（内部）

```ts
function getAnthropicCompat(model) {
  const isOpenRouter = model.provider === "openrouter" || model.baseUrl.includes("openrouter.ai");
  return {
    supportsEagerToolInputStreaming: model.compat?.supportsEagerToolInputStreaming ?? true,
    supportsLongCacheRetention: model.compat?.supportsLongCacheRetention ?? true,
    sendSessionAffinityHeaders: model.compat?.sendSessionAffinityHeaders ?? isOpenRouter,
    sessionAffinityFormat: model.compat?.sessionAffinityFormat ?? (isOpenRouter ? "openrouter" : undefined),
    supportsCacheControlOnTools: model.compat?.supportsCacheControlOnTools ?? true,
    supportsTemperature: model.compat?.supportsTemperature ?? true,
    allowEmptySignature: model.compat?.allowEmptySignature ?? false,
    supportsStrictTools: model.compat?.supportsStrictTools ?? false,
    supportsMidConvoSystemMessages: model.compat?.supportsMidConvoSystemMessages ?? false,
    supportsMidConvoToolChanges: model.compat?.supportsMidConvoToolChanges ?? false,
  };
}
```
- `supportsMidConvoEffort` 与 `forceAdaptiveThinking` **不在此处**默认化，直接读 `model.compat?.*`（undefined 视为 false）。

#### 2.9.4 `AnthropicOptions`（导出接口）

```ts
export type AnthropicEffort = "low" | "medium" | "high" | "xhigh" | "max";
export type AnthropicThinkingDisplay = "summarized" | "omitted";

export interface AnthropicOptions extends StreamOptions {
  thinkingEnabled?: boolean;
  thinkingBudgetTokens?: number;
  effort?: AnthropicEffort;
  thinkingDisplay?: AnthropicThinkingDisplay;   // 默认 "summarized"
  interleavedThinking?: boolean;               // 默认 true
  toolChoice?: "auto" | "any" | "none" | { type: "tool"; name: string };
  client?: Anthropic;                          // 注入 SDK 客户端，跳过内部构造
}
```

#### 2.9.5 `mergeHeaders` / `hasHeader` / `hasRequestAuth` / `assertRequestAuth`（内部）

- `mergeHeaders(...sources)`：`Object.assign` 顺序合并（后者覆盖，含 `null`）。
- `hasHeader(headers, name)`：大小写不敏感；值为 `null` 或 `trim()` 后为空 → 不算。
- `hasRequestAuth(apiKey, headers)`：`!!apiKey || hasHeader("authorization") || hasHeader("x-api-key") || hasHeader("cf-aig-authorization")`。
- `assertRequestAuth(provider, apiKey, headers)`：无鉴权 → `throw new Error(\`No API key for provider: ${provider}\`)`。

#### 2.9.6 `KianaAnthropic`（内部类）

```ts
class KianaAnthropic extends Anthropic {
  protected override _shouldResolveDefaultCredentials(): boolean { return false; }
}
```
作用：禁止 SDK 走自己的凭据链（`ANTHROPIC_PROFILE` 配置文件、federation env），
从而让 `apiKey: null, authToken: null` 的"头自带鉴权"客户端不会偷偷交换凭据。
Rust 侧无此问题；等价约束：只有当 `apiKey`/`authToken`/federation config 显式存在时才添加鉴权头。

#### 2.9.7 `getAnthropicFederation(model, apiKey, headers, env)`（内部）

```ts
if (model.provider !== "anthropic" || hasRequestAuth(apiKey, headers)) return undefined;
federationRuleId = getProviderEnvValue(ANTHROPIC_FEDERATION_RULE_ID_ENV, env);
organizationId   = getProviderEnvValue(ANTHROPIC_ORGANIZATION_ID_ENV, env);
identityTokenFile= getProviderEnvValue(ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, env);
if (!federationRuleId || !organizationId || !identityTokenFile) return undefined;
return {
  organization_id: organizationId,
  workspace_id: getProviderEnvValue(ANTHROPIC_WORKSPACE_ID_ENV, env),      // 可 undefined
  authentication: {
    type: "oidc_federation",
    federation_rule_id: federationRuleId,
    service_account_id: getProviderEnvValue(ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, env),   // 可选
    identity_token: { source: "file", path: identityTokenFile },
  },
};
```
环境变量名（来自 `env-api-keys.ts`，逐字）：`ANTHROPIC_FEDERATION_RULE_ID`、`ANTHROPIC_ORGANIZATION_ID`、
`ANTHROPIC_IDENTITY_TOKEN_FILE`、`ANTHROPIC_WORKSPACE_ID`、`ANTHROPIC_SERVICE_ACCOUNT_ID`。

`federationClient` 模块级缓存：
```ts
let federationClient: { key: string; fetch: typeof fetch | undefined; client: Anthropic } | undefined;
// key = JSON.stringify([model.baseUrl, federation])
// 若 key 或 fetch 变化则新建客户端；否则 client.withOptions({ defaultHeaders }) 复用（共享 token 缓存）
```

#### 2.9.8 SSE 解码（自定义，**不使用 SDK 的解析器**）

类型：
```ts
interface ServerSentEvent { event: string | null; data: string; raw: string[]; }
interface SseDecoderState { event: string | null; data: string[]; raw: string[]; }
const ANTHROPIC_MESSAGE_EVENTS: ReadonlySet<string> = new Set([
  "message_start","message_delta","message_stop","content_block_start","content_block_delta","content_block_stop",
]);
```

`flushSseEvent(state)`：
- `!state.event && state.data.length === 0` → `null`（不发空事件）。
- 否则生成 `{ event, data: state.data.join("\n"), raw: [...state.raw] }`，清空 state。

`decodeSseLine(line, state)`：
- `line === ""` → `flushSseEvent(state)`（空行 = 事件边界）。
- `state.raw.push(line)`；以 `":"` 开头 → 注释，返回 null。
- `delimiterIndex = line.indexOf(":")`；`fieldName = -1 ? line : slice(0, idx)`；
  `value = -1 ? "" : slice(idx+1)`；若 `value` 以 **一个空格** 开头则去掉该空格。
- `fieldName === "event"` → `state.event = value`；`"data"` → `state.data.push(value)`；其他忽略。

`nextLineBreakIndex(text)`：返回 `\r` 与 `\n` 中最小的索引；只有一个时返回它；都没有返回 -1。
`consumeLine(text)`：按上者切一行；若行尾是 `\r` 且下一字符是 `\n`，额外跳 1（CRLF）。
返回 `{ line, rest }`；无换行返回 `null`。

`iterateSseMessages(body, signal)`（async generator）：
- `reader = body.getReader()`；`decoder = new TextDecoder()`（UTF-8，流式）。
- 循环：
  1. `signal?.aborted` → `throw new Error("Request was aborted")`
  2. `reader.read()`；`done` → break
  3. `buffer += decoder.decode(value, { stream: true })`
  4. 反复 `consumeLine(buffer)` 并逐行 `decodeSseLine`，产出非 null 事件
- 循环后：`buffer += decoder.decode()`（冲刷），再按行处理；**残留 buffer**（无换行结尾）也交给
  `decodeSseLine`；最后 `flushSseEvent` 产出尾事件。
- `finally`：`reader.releaseLock()`。

`iterateAnthropicEvents(response, signal)`：
- `!response.body` → `throw new Error("Attempted to iterate over an Anthropic response with no body")`
- 对每个 SSE：
  - `sse.event === "error"` → `throw new Error(sse.data)`（逐字使用 data 作为消息）
  - 事件名不在 `ANTHROPIC_MESSAGE_EVENTS` → **跳过**（含 `null`、`ping` 等）
  - `parseJsonWithRepair<RawMessageStreamEvent>(sse.data)`：
    - 记录 `sawMessageStart`（`type === "message_start"`）/`sawMessageEnd`（`type === "message_stop"`）
    - yield
    - 解析失败 → `throw new Error(\`Could not parse Anthropic SSE event ${sse.event}: ${message}; data=${sse.data}; raw=${sse.raw.join("\\n")}\`)`
      （注意 `raw` 以**两个字面字符** `\n` 连接，即 `join("\\n")`）
- 结束后：`sawMessageStart && !sawMessageEnd` → `throw new Error("Anthropic stream ended before message_stop")`

#### 2.9.9 `stream`（导出）

签名：`stream(model, context: TranscriptContext, options?: AnthropicOptions) -> AssistantMessageEventStream`

同步阶段：
```ts
const stream = new AssistantMessageEventStream();
const normalizedContext = resolveTranscript(context, getAnthropicCompat(model).supportsMidConvoSystemMessages);
const currentTools = getCurrentTools(normalizedContext.messages);
```

异步阶段（IIFE）：

1. **构造 `output`**（字段顺序如下，影响 partial 的 JSON 形状）：
   ```
   { role:"assistant", content:[], api: model.api, provider: model.provider, model: model.id,
     usage:{input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,
            cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}},
     stopReason:"pending", timestamp: Date.now() }
   ```
2. **客户端**：
   - `options.client` 存在 → 直接使用，`isOAuth = false`。
   - 否则：
     - `federation = getAnthropicFederation(model, apiKey, options?.headers, options?.env)`
     - `!federation` 时 `assertRequestAuth(...)`（此时抛错 → 进 catch → error 事件；注意
       `streamSimple` 已提前同步抛过）。
     - Copilot：`hasImages = hasCopilotVisionInput(normalizedContext.messages)`，
       `copilotDynamicHeaders = buildCopilotDynamicHeaders({messages, hasImages})`。
     - `cacheRetention = resolveCacheRetention(options?.cacheRetention, options?.env)`
     - `cacheSessionId = cacheRetention === "none" ? undefined : options?.sessionId`
     - `createClient(...)` → `{ client, isOAuthToken }`
3. **requestOptions**：`{ ...(signal && {signal}), ...(timeoutMs !== undefined && {timeout: timeoutMs}), maxRetries: 0 }`
4. **strict 工具降级重试循环**：
   ```
   disablePreferredStrictTools = false
   for (;;):
       options?.signal?.throwIfAborted()
       built = buildParams(model, normalizedContext, isOAuth, options, disablePreferredStrictTools)
       if built.providerThinkingLevel: output.providerThinkingLevel = it
       params = built.params
       nextParams = await options?.onPayload?.(params, model)
       if nextParams !== undefined: params = { ...nextParams, stream: true }
       try:
           response = await retryProviderRequest(
               () => client.beta.messages.create(params, requestOptions).asResponse(),
               { maxRetries: options?.maxRetries, maxRetryDelayMs: options?.maxRetryDelayMs, signal: options?.signal })
           break
       catch error:
           // 只降级"被拒绝的请求"，绝不降级已开始的流
           if options?.signal?.aborted || disablePreferredStrictTools || !built.hasPreferredStrictTools
              || !(error instanceof Anthropic.APIError) || error.status !== 400
              || error.type !== "invalid_request_error"
              || ![ "The compiled grammar is too large",
                    "Too many strict tools (",
                    "Schemas contains too many parameters with union types (" ]
                   .some(m => error.message.includes(m)):
               throw error
           disablePreferredStrictTools = true
           appendAssistantMessageDiagnostic(output, {
               type: "anthropic_strict_tools_fallback",
               timestamp: Date.now(),
               error: { message: error.message },
               details: { reason: "strict_tool_limits_exceeded" },
           })
   ```
   - 三个触发子串**逐字**，注意第 3 个是 `Schemas contains`（原文如此，语法错误也照抄）。
   - `hasPreferredStrictTools` 由 `buildParams` 返回（见 2.9.12）。
5. `await options?.onResponse?.({ status: response.status, headers: headersToRecord(response.headers) }, model)`
6. `stream.push({ type: "start", partial: output })`
7. **流状态**：`openBlocks: Map<number, { contentIndex: number; arguments?: ToolCallArgumentsStream }>`
   - `openBlock(i)` 返回 `{ ...open, block: output.content[open.contentIndex] }` 或 `undefined`。
   - 每个 Anthropic block index 对应一个内容块；`arguments` 只对 toolCall 存在。
8. **事件处理**（`for await (event of iterateAnthropicEvents(response, options?.signal))`）：
   先 `await options?.onProviderStreamEvent?.(event, model)`（**每个事件**，包括被后续逻辑忽略的）。

   **`message_start`**：
   - `output.responseId = event.message.id`
   - `event.message.input_transformations` 为数组 → 记入 `inputTransformations`
   - `responseModel = event.message.model`；若 `!== model.id` → `output.responseModel = responseModel`
   - fallback 成本：
     ```
     fallbackCost = responseModel === model.id ? undefined
       : model.compat?.allowedFallbackModels?.find(f => f.provider === model.provider && f.model === responseModel)?.cost
     usageModel = fallbackCost ? { ...model, id: responseModel, cost: fallbackCost } : model
     ```
   - usage 初始化（`|| 0`）：
     ```
     input   = usage.input_tokens
     output  = usage.output_tokens
     cacheRead  = usage.cache_read_input_tokens
     cacheWrite = usage.cache_creation_input_tokens
     cacheWrite1h = usage.cache_creation?.ephemeral_1h_input_tokens
     totalTokens = input + output + cacheRead + cacheWrite
     ```
   - `calculateCost(usageModel, output.usage)`

   **`content_block_start`**：
   - `type === "fallback"`：若 `output.content.length > 0` → `throw new Error("Anthropic performed an unsupported mid-output model fallback")`；
     否则 `continue`（忽略）。
   - `text`：push `{ type:"text", text: content_block.text ?? "" }`，登记 openBlock，
     push `text_start`（`contentIndex` 为新块下标）。
   - `thinking`：push `{ type:"thinking", thinking: content_block.thinking ?? "", thinkingSignature: content_block.signature ?? "" }`，
     push `thinking_start`。
   - `redacted_thinking`：push `{ type:"thinking", thinking:"[Reasoning redacted]", thinkingSignature: content_block.data, redacted: true }`，
     push `thinking_start`。
   - `tool_use`：push
     `{ type:"toolCall", id: content_block.id, name: isOAuth ? fromClaudeCodeName(name, currentTools) : name,
        arguments: content_block.input ?? {} }`，
     openBlock 带 `new ToolCallArgumentsStream()`，push `toolcall_start`。

   **`content_block_delta`**（按 `open.block.type` 与 `delta.type` 双重匹配）：
   - `text_delta` + text 块：`block.text += delta.text`，push `text_delta`（`delta` 原文）。
   - `thinking_delta` + thinking 块：`block.thinking += delta.thinking`，push `thinking_delta`。
   - `input_json_delta` + toolCall：`block.arguments = arguments.append(delta.partial_json)`，
     push `toolcall_delta`（`delta: partial_json`）。
   - `signature_delta` + thinking：`block.thinkingSignature = (block.thinkingSignature || "") + delta.signature`
     （**不发事件**）。
   - 其他组合静默忽略。

   **`content_block_stop`**：取 openBlock，`openBlocks.delete(index)`；若有：
   - text → push `text_end`（`content: block.text`）
   - thinking → push `thinking_end`（`content: block.thinking`）
   - toolCall 且有 `arguments` → `Object.assign(block, arguments.finish())`（写入 `arguments` 与可能的
     `argumentsError`），push `toolcall_end`（`toolCall: block`）

   **`message_delta`**：
   - `event.input_transformations` 为数组 → 覆盖 `inputTransformations`
   - `event.delta.stop_reason` 存在 → `output.rawStopReason = it`；`mapStopReason(...)` 写入
     `output.stopReason` 与（可能的）`output.errorMessage`
   - `event.usage` 存在时，**仅当字段非 null** 才覆盖：
     `input_tokens`、`output_tokens`、`cache_read_input_tokens`、`cache_creation_input_tokens`、
     `usage.cache_creation?.ephemeral_1h_input_tokens`（Vercel AI Gateway 在 delta 里也给 TTL 拆分）、
     `usage.output_tokens_details?.thinking_tokens` → `output.usage.reasoning`
   - 重算 `totalTokens = input+output+cacheRead+cacheWrite`；`calculateCost(usageModel, usage)`
   - `isOpenRouterModel(model) && event.usage` → `applyOpenRouterReportedCost(output.usage, event.usage)`
9. **收尾检查**（顺序固定）：
   ```
   if options?.signal?.aborted: throw new Error("Request was aborted")
   if output.stopReason === "pending": throw new Error("Anthropic stream ended without a stop reason")
   if output.stopReason === "aborted" || "error": throw new Error(output.errorMessage || "An unknown error occurred")
   if inputTransformations && length > 0:
       appendAssistantMessageDiagnostic(output, {
         type: "anthropic_input_transformations", timestamp: Date.now(),
         details: { transformations: inputTransformations.map(t => ({
             type: t.type ?? undefined, path: t.path ?? undefined, reason: t.reason ?? undefined })) } })
   stream.push({ type: "done", reason: output.stopReason, message: output })
   stream.end()
   ```
10. **catch**：
    ```
    output.stopReason = options?.signal?.aborted ? "aborted" : "error"
    output.errorMessage = error instanceof Error ? error.message : JSON.stringify(error)
    stream.push({ type: "error", reason: output.stopReason, error: output })
    stream.end()
    ```
    注意：Anthropic **不做** `normalizeProviderError`/`formatProviderError`（与 OpenAI 系列不同）。

#### 2.9.10 `mapThinkingLevelToEffort(model, level)`（内部）

```ts
const mapped = level ? model.thinkingLevelMap?.[level] : undefined;
if (typeof mapped === "string") return mapped as AnthropicEffort;
switch (level) {
  case "minimal": case "low": return "low";
  case "medium": return "medium";
  case "high": return "high";
  default: return "high";      // 含 undefined / "off" / "xhigh" / "max"
}
```
- **注意**：`"xhigh"`/`"max"` 若无映射会**落到 `"high"`**（不会原样保留）。

#### 2.9.11 `streamSimple`（导出）

```ts
if (!getAnthropicFederation(model, options?.apiKey, options?.headers, options?.env)) {
  assertRequestAuth(model.provider, options?.apiKey, options?.headers);
}
const base = { ...buildBaseOptions(model, context, options, options?.apiKey), toolChoice: options?.toolChoice };
if (!options?.reasoning) return stream(model, context, { ...base, thinkingEnabled: false });

if (model.compat?.forceAdaptiveThinking === true) {
  const effort = mapThinkingLevelToEffort(model, options.reasoning);
  return stream(model, context, { ...base, thinkingEnabled: true, effort });
}

const adjusted = adjustMaxTokensForThinking(base.maxTokens, model.maxTokens, options.reasoning, options.thinkingBudgets);
const maxTokens = clampMaxTokensToContext(model, context, adjusted.maxTokens);
return stream(model, context, {
  ...base, maxTokens, thinkingEnabled: true,
  thinkingBudgetTokens: Math.min(adjusted.thinkingBudget, Math.max(0, maxTokens - 1024)),
});
```
- 无 `reasoning` → `thinkingEnabled: false`（会走 `thinking.type = "disabled"` 分支，见 2.9.12）。
- federation 存在时**跳过** `assertRequestAuth`。

#### 2.9.12 `createClient(...)`（内部）

参数：`(model, apiKey, optionsHeaders?, fetch?, dynamicHeaders?, sessionId?, federation?, identity?)`
→ `{ client, isOAuthToken }`。

三种分支：

**(a) `model.provider === "github-copilot"`**（Bearer）：
```ts
new KianaAnthropic({
  apiKey: null, authToken: apiKey ?? null, baseURL: model.baseUrl, dangerouslyAllowBrowser: true, fetch,
  defaultHeaders: mergeHeaders(
    getIdentityHeaders(model, identity),
    { accept: "application/json", "anthropic-dangerous-direct-browser-access": "true" },
    model.headers, dynamicHeaders, optionsHeaders),
})
// isOAuthToken: false   ← 即使 key 带 sk-ant-oat 也不当 OAuth
```

**(b) `apiKey && apiKey.includes("sk-ant-oat")`**（OAuth Bearer）：
```ts
new KianaAnthropic({
  apiKey: null, authToken: apiKey, baseURL: model.baseUrl, dangerouslyAllowBrowser: true, fetch,
  defaultHeaders: mergeHeaders(
    { accept: "application/json",
      "anthropic-dangerous-direct-browser-access": "true",
      "user-agent": `claude-cli/${claudeCodeVersion}`,   // 即 "claude-cli/2.1.280"
      "x-app": "cli" },
    model.headers, optionsHeaders),
})
// isOAuthToken: true   ← 不做 getIdentityHeaders！
```

**(c) API key / 头自带鉴权 / federation**：
```ts
sessionAffinityHeaders = {}
if (sessionId && compat.sendSessionAffinityHeaders):
    header = compat.sessionAffinityFormat === "openrouter" ? "x-session-id" : "x-session-affinity"
    sessionAffinityHeaders[header] = sessionId
defaultHeaders = mergeHeaders(
  getIdentityHeaders(model, identity),
  { accept: "application/json", "anthropic-dangerous-direct-browser-access": "true" },
  sessionAffinityHeaders, model.headers, optionsHeaders)
if federation:
    key = JSON.stringify([model.baseUrl, federation])
    if federationClient?.key !== key || federationClient.fetch !== fetch:
        federationClient = { key, fetch, client: new KianaAnthropic({ apiKey: null, authToken: null, config: federation,
            baseURL: model.baseUrl, dangerouslyAllowBrowser: true, fetch }) }
    return { client: federationClient.client.withOptions({ defaultHeaders }), isOAuthToken: false }
new KianaAnthropic({ apiKey: apiKey ?? null, authToken: null, baseURL: model.baseUrl,
                  dangerouslyAllowBrowser: true, fetch, defaultHeaders })
```
- **头合并顺序**：identity → accept/直连浏览器 → session affinity → `model.headers` → `options.headers`。
  后者覆盖前者（含大小写不同的同名头，因为 SDK/`Headers` 大小写不敏感）。
- Copilot 分支把 `dynamicHeaders` 插在 `model.headers` 与 `optionsHeaders` 之间。

#### 2.9.13 `getBetaFeatures(model, context, isOAuthToken, nativeToolChanges, options?)`（内部）

```
configuredFeatures: 遍历 [model.headers, options?.headers] 的 Object.entries，
                     若 name.toLowerCase() === "anthropic-beta" 则取 value（后者覆盖前者）
if configuredFeatures === null: return []                  // 显式 null = 关闭所有 beta
if configuredFeatures !== undefined:
    return [...new Set(configuredFeatures.split(",").map(f => f.trim()).filter(f => f.length > 0))]
features = []
if isOAuthToken: push "claude-code-20250219", "oauth-2025-04-20"
if shouldUseFineGrainedToolStreamingBeta(model, context): push FINE_GRAINED_TOOL_STREAMING_BETA
if model.reasoning && options?.thinkingEnabled === true && (options.interleavedThinking ?? true)
   && model.compat?.forceAdaptiveThinking !== true: push INTERLEAVED_THINKING_BETA
if shouldUseServerSideFallbackBeta(model): push SERVER_SIDE_FALLBACK_BETA
if model.compat?.supportsMidConvoEffort === true: push MID_CONVERSATION_OUTPUT_CONFIG_BETA, THINKING_BINDING_CONTROLS_BETA
if nativeToolChanges: push INLINE_TOOLS_BETA
return [...new Set(features)]
```
- `shouldUseServerSideFallbackBeta(model)`：`(model.compat?.allowedFallbackModels?.length ?? 0) > 0`。
- `shouldUseFineGrainedToolStreamingBeta(model, context)`：
  `getCurrentTools(context.messages).length > 0 && !supportsEagerToolInputStreaming`。
- 显式配置的 beta 列表**完全替换**自动列表（不合并）。
- `anthropic-beta` 头值为 `null` → 空数组（**不发** beta 字段）。

#### 2.9.14 `buildParams(model, context, isOAuthToken, options?, disablePreferredStrictTools=false)`（内部）

返回 `{ params, hasPreferredStrictTools, providerThinkingLevel }`。

准备：
```
{ cacheControl } = getCacheControl(model, options?.cacheRetention, options?.env)
compat = getAnthropicCompat(model)
initialSystemMessage = getInitialSystemMessage(context.messages)
initialSystemText = initialSystemMessage ? getSystemMessageText(initialSystemMessage) : ""
transformedMessages = transformMessages(context.messages, model, normalizeToolCallId)
conversationMessages = initialSystemMessage ? transformedMessages.slice(1) : transformedMessages
```
`normalizeToolCallId(id)`（**本模块内部**）：
```ts
id.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64)
```
（先替换非法字符为 `_`，再按 UTF-16 code unit 截断到 64 —— 对 ASCII 输入等价于 64 字节。）

工具转换闭包 `convertToolDefinitions(tools, toolCacheControl?)`：
```
definitions = convertTools(tools, isOAuthToken, compat.supportsEagerToolInputStreaming,
                           compat.supportsStrictTools, toolCacheControl, disablePreferredStrictTools)
if 存在 index 使 definitions[index].strict === true
   && tools[index].constrainedSampling?.type === "json_schema"
   && tools[index].constrainedSampling.strict === "prefer":
       hasPreferredStrictTools = true
return definitions
```

原生工具变更判定：
```
initialTools = initialSystemMessage?.toolsAdded ?? []
nativeToolChanges = compat.supportsMidConvoSystemMessages && compat.supportsMidConvoToolChanges && initialTools.length > 0
thinkingDisabled = model.reasoning && options?.thinkingEnabled === false && model.thinkingLevelMap?.off !== null
effortMessages = model.compat?.supportsMidConvoEffort === true && !thinkingDisabled
```

消息转换：
```
converted = convertMessages(conversationMessages, isOAuthToken, cacheControl, compat.allowEmptySignature,
                            effortMessages ? model.provider : undefined,
                            nativeToolChanges ? convertToolDefinitions : undefined)
activeEffort = options?.effort ?? "high"
betaFeatures = getBetaFeatures(model, context, isOAuthToken, nativeToolChanges, options)
params = {
  model: model.id,
  messages: effortMessages ? insertThinkingLevelMessages(converted, activeEffort) : converted.messages,
  max_tokens: options?.maxTokens ?? model.maxTokens,
  stream: true,
  ...(betaFeatures.length > 0 ? { betas: betaFeatures } : {}),
}
```

**system**：
- OAuth：
  ```ts
  params.system = [{ type:"text", text: "You are Claude Code, Anthropic's official CLI for Claude.",
                    ...(cacheControl ? { cache_control: cacheControl } : {}) }];
  if (initialSystemText) params.system.push({ type:"text", text: sanitizeSurrogates(initialSystemText),
                    ...(cacheControl ? { cache_control: cacheControl } : {}) });
  ```
- 非 OAuth 且有 initialSystemText：
  ```ts
  params.system = [{ type:"text", text: sanitizeSurrogates(initialSystemText),
                    ...(cacheControl ? { cache_control: cacheControl } : {}) }];
  ```
  注意：**开头的 system 前缀不计入 `messages`**（`conversationMessages` 已 `slice(1)`）；
  但 `transformMessages` 之后的开头 system 若被丢弃（例如 `content` 为空）仍会 slice 掉，需按实现。

**temperature**：
```ts
if (options?.temperature !== undefined && !options?.thinkingEnabled
    && model.compat?.supportsMidConvoEffort !== true && compat.supportsTemperature) {
  params.temperature = options.temperature;
}
```
（4 个条件全部满足才发。）

**tools**：
```ts
toolCacheControl = compat.supportsCacheControlOnTools ? cacheControl : undefined
if (nativeToolChanges):
    params.tools = [...convertToolDefinitions(initialTools, toolCacheControl), DEFERRED_TOOL_PLACEHOLDER]
else:
    tools = getCurrentTools(context.messages)
    if (tools.length > 0) params.tools = convertToolDefinitions(tools, toolCacheControl)
```

**thinking / output_config**（互斥顺序）：
1. `thinkingDisabled` → `params.thinking = { type: "disabled" }`（无 effort）
2. `effortMessages` →
   ```ts
   params.thinking = { type: "adaptive", display: options?.thinkingDisplay ?? "summarized",
                       block_binding: { prefix_mismatch_behavior: "drop_block" } };
   params.output_config = { effort: "low" };   // 顶层固定 low
   ```
3. `model.reasoning && options?.thinkingEnabled`：
   - `display = options.thinkingDisplay ?? "summarized"`
   - `forceAdaptiveThinking === true`：`params.thinking = { type:"adaptive", display }`；
     `if (options.effort) params.output_config = { effort: options.effort }`
   - 否则：`params.thinking = { type:"enabled", budget_tokens: (options.thinkingBudgetTokens || 1024), display }`
4. 其他 → 不设 `thinking`。

**metadata**：`options.metadata.user_id` 为 string 时 → `params.metadata = { user_id }`。

**tool_choice**：
- 字符串 → `params.tool_choice = { type: <str> }`（`"auto"|"any"|"none"`）
- 对象 → 原样 `params.tool_choice = options.toolChoice`

**fallbacks**：
```ts
if (allowedFallbackModels && length > 0) params.fallbacks = allowedFallbackModels.map(f => ({ model: f.model }));
```

返回值 `providerThinkingLevel = effortMessages ? activeEffort : undefined`。

#### 2.9.15 `convertMessages(...)`（内部）

签名：
```ts
function convertMessages(
  transformedMessages: Message[],
  isOAuthToken: boolean,
  cacheControl?: CacheControlEphemeral,
  allowEmptySignature = false,
  managedProvider?: string,
  convertToolDefinitions?: (tools: Tool[]) => BetaTool[],
): ConvertedAnthropicMessages   // { messages: MessageParam[]; assistantLevels: Map<number, AnthropicEffort> }
```

**system 消息（非首条）处理**：
```
text = renderSystemMessageUpdate(msg)
blocks = []
if text.length > 0: blocks.push({ type:"text", text: sanitizeSurrogates(text) })
if convertToolDefinitions:
    added = msg.toolsAdded ?? []
    redefined = new Set(added.map(t => t.name))
    for tool of msg.toolsRemoved ?? []:
        if redefined.has(tool.name): continue        // 同名重定义替代删除
        blocks.push({ type:"tool_removal", tool: { type:"tool_reference",
                     name: isOAuthToken ? toClaudeCodeName(tool.name) : tool.name } })
    for definition of convertToolDefinitions(added):
        blocks.push({ type:"tool_addition", tool: { type:"tool_definition", definition } })
if blocks.length > 0: pendingSystemMessages.push({ role:"system", content: blocks })
```
**待发系统消息的时序**：`pendingSystemMessages` 被"持有"，在下一个**被发送的** assistant 消息之前
`flush`（`flushPendingSystemMessages` 把队列内容 push 到 `params` 并清空）；循环结束再 flush 一次。
理由（源码注释）：转换后为空而被丢弃的 assistant 消息不应让更新跑到 user 消息之前；
Anthropic 要求 `tool_result` 紧跟 `tool_use`；效果是"放在 user 消息前的更新在线上会落到它之后"。

**user**：
- 字符串：`content.trim().length > 0` → push `{ role:"user", content: sanitizeSurrogates(content) }`
- 数组：逐块 → text 块 `{ type:"text", text: sanitizeSurrogates(text) }`；image 块 `convertImageBlock`
  （`{ type:"image", source:{ type:"base64", media_type: mimeType, data } }`）；
  过滤 `text.trim().length > 0` 的文本块（图片全保留）；过滤后为空 → `continue`。

**assistant**：
- 遍历 content：
  - `text`：`text.trim().length === 0` → skip；否则 `{ type:"text", text: sanitizeSurrogates(text) }`
  - `thinking`：
    - `block.redacted` → `{ type:"redacted_thinking", data: block.thinkingSignature! }`，continue
    - `hasThinkingSignature = !!sig && sig.trim().length > 0`
    - `thinking.trim().length === 0 && !hasThinkingSignature` → skip
    - `!hasThinkingSignature` →
      `allowEmptySignature ? { type:"thinking", thinking: sanitizeSurrogates(thinking), signature: "" }
                           : { type:"text", text: sanitizeSurrogates(thinking) }`
    - 否则 `{ type:"thinking", thinking: sanitizeSurrogates(thinking), signature }`
  - `toolCall`：`{ type:"tool_use", id, name: isOAuth ? toClaudeCodeName(name) : name, input: arguments ?? {} }`
- `blocks.length === 0` → `continue`（整条消息丢弃）
- `flushPendingSystemMessages()`；`messageIndex = params.length`；push `{ role:"assistant", content: blocks }`
- 若 `managedProvider !== undefined && msg.api === "anthropic-messages" && msg.provider === managedProvider
   && isAnthropicEffort(msg.providerThinkingLevel)` → `assistantLevels.set(messageIndex, level)`

**toolResult（合并连续）**：
```
toolResults = []; errorImages = []
j = i
while j < len && transformedMessages[j].role === "toolResult":
    toolMsg = ...
    split = splitErrorToolResultImages(toolMsg)
    toolResults.push(convertToolResult(toolMsg, split?.content))
    if split: errorImages.push({ type:"text", text: split.caption }, ...split.images.map(convertImageBlock))
    j++
i = j - 1
params.push({ role:"user", content: [...toolResults, ...errorImages] })   // 即使为空也 push？见下
```
`convertToolResult(msg, content = msg.content)` →
```ts
{ type:"tool_result", tool_use_id: msg.toolCallId, content: convertContentBlocks(content), is_error: msg.isError }
```
`convertContentBlocks(content)`（内部）：
- 无 image → 返回 `sanitizeSurrogates(所有 text 以 "\n" join)`
- 有 image → 块数组：text → `{type:"text", text: sanitizeSurrogates(text)}`；image → `convertImageBlock`
- 若结果无 text 块 → **unshift** `{ type:"text", text: "(see attached image)" }`（逐字）

**cache_control 收尾**：
```ts
if (cacheControl && params.length > 0) {
  lastMessage = params[params.length - 1]
  if (lastMessage.role === "user" || lastMessage.role === "system") {
    if (Array.isArray(lastMessage.content)) {
      lastBlock = lastMessage.content[lastMessage.content.length - 1]
      if (lastBlock && [ "text","image","tool_result","tool_addition","tool_removal" ].includes(lastBlock.type))
        lastBlock.cache_control = cacheControl
    } else if (typeof lastMessage.content === "string") {
      lastMessage.content = [{ type:"text", text: lastMessage.content, cache_control: cacheControl }]
    }
  }
}
```
- 只挂**最后一条**消息的**最后一个**块；assistant 结尾时不挂。
- `tool_use` / `thinking` / `redacted_thinking` 不在允许列表内（不会挂上）。

#### 2.9.16 `insertThinkingLevelMessages(converted, activeEffort)`（内部）

```
messages = []
for index of converted.messages:
    historical = converted.assistantLevels.get(index)
    if historical !== undefined:
        messages.push({ role:"system", content: [], output_config: { effort: historical } })
    messages.push(converted.messages[index])
messages.push({ role:"system", content: [], output_config: { effort: activeEffort } })
```
- 末尾**总是**追加一条当前 effort 的 marker。
- marker 是 `role: "system"` 且 `content: []`。

`isAnthropicEffort(v)`：`v ∈ {"low","medium","high","xhigh","max"}`。

#### 2.9.17 `convertTools(tools, isOAuthToken, supportsEagerToolInputStreaming, supportsStrictTools, cacheControl?, disablePreferredStrictTools=false)`（内部）

```
tools.map((tool, index) => {
  config = tool.constrainedSampling
  strict = (disablePreferredStrictTools && config?.type === "json_schema" && config.strict === "prefer")
             ? undefined
             : resolveJsonSchemaStrictSampling(tool, supportsStrictTools, isAnthropicStrictUnsupportedKeyword)
  parameters = getJsonSchemaToolParameters(tool, strict)
  schema = parameters as { properties?, required? }
  legacyInputSchema = { type: "object", properties: schema.properties ?? {}, required: schema.required ?? [] }
  inputSchema = strict === true ? { ...parameters, ...legacyInputSchema } : legacyInputSchema
  return {
    name: isOAuthToken ? toClaudeCodeName(tool.name) : tool.name,
    description: tool.description,
    ...(supportsEagerToolInputStreaming ? { eager_input_streaming: true } : {}),
    ...(strict === true ? { strict: true } : {}),
    input_schema: inputSchema,
    ...(cacheControl && index === tools.length - 1 ? { cache_control: cacheControl } : {}),
  }
})
```
- `tools` 为假值 → `[]`。
- `strict === true` 时 `input_schema` 是"完整 strict 参数 + 强制 `type/properties/required` 覆盖"。
  注意展开顺序：`...parameters` 在前，`...legacyInputSchema` 在后，所以 `type/properties/required`
  以 legacy 为准（`properties` 会是 `parameters.properties ?? {}`）。
- `cache_control` **只挂在最后一个工具**上。

**Anthropic strict 不支持的关键字**（`ANTHROPIC_STRICT_UNSUPPORTED_KEYWORDS`）：
```
minimum, maximum, exclusiveMinimum, exclusiveMaximum, multipleOf,
maxItems, uniqueItems, minContains, maxContains, minProperties, maxProperties
```
**允许的字符串 format 白名单**（`ANTHROPIC_STRICT_STRING_FORMATS`）：
```
date-time, time, date, duration, email, hostname, uri, ipv4, ipv6, uuid
```
`isAnthropicStrictUnsupportedKeyword(key, value)`：
```ts
if (UNSUPPORTED_KEYWORDS.has(key)) return true;
if (key === "minItems") return value !== 0 && value !== 1;
if (key === "format") return typeof value !== "string" || !ALLOWED_FORMATS.has(value);
return false;
```

#### 2.9.18 `mapStopReason(reason, stopDetails?) -> { stopReason, errorMessage? }`（内部）

| `reason` | 结果 |
|---|---|
| `end_turn` | `{ stopReason: "stop" }` |
| `max_tokens` | `{ stopReason: "length" }` |
| `tool_use` | `{ stopReason: "toolUse" }` |
| `refusal` | `{ stopReason: "error", errorMessage: stopDetails?.explanation || "The model refused to complete the request" }` |
| `pause_turn` | `{ stopReason: "stop" }`（可重发） |
| `stop_sequence` | `{ stopReason: "stop" }` |
| `sensitive` | `{ stopReason: "error", errorMessage: "Provider stopped with: sensitive" }` |
| 其他 | **抛** `Unhandled stop reason: ${reason}` |

（`"The model refused to complete the request"` 为**模板字符串**，无变量。）

### 2.10 `openai-completions.ts`

文件 1728 行。

#### 2.10.1 常量与类型

```ts
const OPENAI_COMPLETIONS_REASONING_FIELDS = ["reasoning", "reasoning_content", "reasoning_text"] as const;
type OpenAICompletionsReasoningField = (typeof OPENAI_COMPLETIONS_REASONING_FIELDS)[number];
function isOpenAICompletionsReasoningField(field: string): field is OpenAICompletionsReasoningField {
  return OPENAI_COMPLETIONS_REASONING_FIELDS.includes(field as OpenAICompletionsReasoningField);
}
```
（源码中该常量**未导出**，仅模块内使用；Rust 侧可为 `pub(crate)`。）

`OpenAICompletionsOptions`（导出）：
```ts
export interface OpenAICompletionsOptions extends StreamOptions {
  toolChoice?: OpenAI.Chat.Completions.ChatCompletionToolChoiceOption;
  reasoningEffort?: "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
  thinkingBudgets?: ThinkingBudgets;
}
export interface ConvertCompletionsMessagesOptions {
  grammarToolInputProperties?: ReadonlyMap<string, string>;
}
```

`OpenAICompatCacheControl = { type: "ephemeral"; ttl?: string }`（内部）。

`ResolvedOpenAICompletionsCompat`：`Required<OpenAICompletionsCompat>` 但以下几项保留可选：
`cacheControlFormat`、`supportsThinkingTokenBudget`、`thinkingTokenBudgetField`、
`supportsMidConvoSystemMessages`、`supportsMidConvoToolAdditions`、`vllmPriority`。

`reasoning_details` 相关类型（内部）：
```ts
type OpenAIReasoningDetailBase = Record<string, JsonValue> & { id?: string | null; format?: string; index?: number };
type OpenAIReasoningSummaryDetail   = ... & { type: "reasoning.summary"; summary: string };
type OpenAIEncryptedReasoningDetail = ... & { type: "reasoning.encrypted"; data: string };
type OpenAIReasoningTextDetail      = ... & { type: "reasoning.text"; text: string; signature?: string | null };
type OpenAIReasoningDetail = Summary | Encrypted | Text;
```

#### 2.10.2 reasoning_details 解析/合并（内部函数）

`isReasoningDetailObject(detail)`：object、非 null、非数组。
`hasValidCommonReasoningDetailFields(c)`：
```
(c.id === undefined || c.id === null || typeof c.id === "string")
&& (c.format === undefined || typeof c.format === "string")
&& (c.index === undefined || typeof c.index === "number")
```
`isOpenAIReasoningDetail(detail)`：
- 需 `isReasoningDetailObject` 且 `hasValidCommonReasoningDetailFields`
- `"reasoning.summary"` → `typeof detail.summary === "string"`
- `"reasoning.encrypted"` → `typeof detail.data === "string"`
- `"reasoning.text"` → `typeof detail.text === "string"` 且 `signature` 为 undefined/null/string
- 其他 type → false

`parseOpenAIReasoningDetails(signature)`：
- 空 → `undefined`
- `JSON.parse(signature)`；`Array.isArray && length > 0 && every(isOpenAIReasoningDetail)` → 返回；否则 `undefined`
- 解析异常 → `undefined`

`parseLegacyEncryptedReasoningDetail(signature)`：
- 空 → `undefined`
- 解析为单个对象，需 `isOpenAIReasoningDetail` 且 `type === "reasoning.encrypted"`
  且 `typeof id === "string" && id.length > 0 && data.length > 0`
- 否则 `undefined`

`fillMissingCommonReasoningDetailFields(target, source)`：
```
target.id ??= source.id
target.format ||= source.format
target.index ??= source.index
```
（`||=` 会对空串 `""` 也赋值；`??=` 只对 null/undefined。）

`appendOpenAIReasoningDetail(details, detail)`：
```
last = details[details.length - 1]
if detail.type === "reasoning.text" && last?.type === "reasoning.text":
    last.text += detail.text; last.signature ||= detail.signature;
    fillMissingCommonReasoningDetailFields(last, detail); return
if detail.type === "reasoning.summary" && last?.type === "reasoning.summary":
    last.summary += detail.summary;
    fillMissingCommonReasoningDetailFields(last, detail); return
details.push({ ...detail })       // 浅拷贝；encrypted 条目保持离散
```

#### 2.10.3 `resolveCacheRetention` / `hasHeader` / `getClientApiKey` / `hasToolHistory`（内部）

- `resolveCacheRetention`：与 Anthropic 版完全一致。
- `hasHeader`：与 Anthropic 版一致（大小写不敏感、非空）。
- `getClientApiKey(provider, apiKey, headers)`：
  ```
  if (apiKey) return apiKey
  if (hasHeader("authorization") || hasHeader("cf-aig-authorization")) return "unused"
  throw new Error(`No API key for provider: ${provider}`)
  ```
  **注意**：`openai-completions` / `openai-responses` **不**接受 `x-api-key` 作为鉴权（与 Anthropic 不同）。
- `hasToolHistory(messages)`：任一 `toolResult`，或任一 assistant `content` 含 `toolCall`。

#### 2.10.4 `stream`（导出）

同步阶段：`stream = new AssistantMessageEventStream()`；
`normalizedContext = resolveTranscript(context, getCompat(model).supportsMidConvoSystemMessages)`。

异步阶段：

1. `output` 同上（`api: model.api`，注意此处**不是**字面量断言）。
2. `streamedReasoningDetails: OpenAIReasoningDetail[] | undefined`；
   `applyStreamedReasoningDetails(block)`：若非 undefined → `block.thinkingSignature = JSON.stringify(streamedReasoningDetails)`。
3. `apiKey = getClientApiKey(...)`（抛错 → error 事件）。
4. `compat = getCompat(model)`。
5. `grammarToolInputProperties = createGrammarToolInputProperties(getDeclaredTools(normalizedContext.messages), compat.supportsOpenAIGrammarTools)`。
6. `cacheRetention = resolveCacheRetention(options?.cacheRetention, options?.env)`；
   `cacheSessionId = cacheRetention === "none" ? undefined : options?.sessionId`。
7. `client = createClient(model, normalizedContext, apiKey, options?.headers, options?.identity, options?.fetch, cacheSessionId, compat)`。
8. `params = buildParams(model, normalizedContext, options, compat, cacheRetention, grammarToolInputProperties)`；
   `nextParams = await options?.onPayload?.(params, model)`；非 undefined 则**整体替换**（不回填 `stream: true`，
   与 Anthropic 不同）。
9. `requestOptions = { ...(signal && {signal}), ...(timeoutMs !== undefined && {timeout: timeoutMs}), maxRetries: 0 }`。
10. `{ data: openaiStream, response } = await retryProviderRequest(
        () => client.chat.completions.create(params, requestOptions).withResponse(),
        { maxRetries, maxRetryDelayMs, signal })`。
11. `await options?.onResponse?.({status, headers: headersToRecord(response.headers)}, model)`。
12. `stream.push({ type: "start", partial: output })`。
13. **流状态**：
    ```ts
    type ToolCallInput =
      | { kind: "json"; arguments: ToolCallArgumentsStream }
      | { kind: "custom"; property: string; buffer: GrammarToolInputJsonBuffer };
    type ToolCallState = { streamIndex: number | undefined; input: ToolCallInput };
    let textBlock: TextContent | null = null;
    let thinkingBlock: ThinkingContent | null = null;
    let hasFinishReason = false;
    const toolCallBlocksByIndex = new Map<number, ToolCall>();
    const toolCallBlocksById = new Map<string, ToolCall>();
    const toolCallStates = new Map<ToolCall, ToolCallState>();
    const blocks = output.content as StreamingBlock[];
    ```
    `getContentIndex(block) = blocks.indexOf(block)`。
    `finishBlock(block)`：
    ```
    contentIndex = getContentIndex(block); if -1 return
    text     -> push text_end { contentIndex, content: block.text, partial }
    thinking -> applyStreamedReasoningDetails(block); push thinking_end { contentIndex, content: block.thinking, partial }
    toolCall:
        input = toolCallStates.get(block)!.input; toolCallStates.delete(block)
        if custom:
            delta = closeGrammarToolInputJson(input.buffer, input.property, undefined)
            if delta !== undefined: push toolcall_delta { contentIndex, delta, partial }
        else: Object.assign(block, input.arguments.finish())
        push toolcall_end { contentIndex, toolCall: block, partial }
    ```
    `ensureTextBlock()`：无则 `{ type:"text", text:"" }` push 到 blocks 并 push `text_start`。
    `ensureThinkingBlock(thinkingSignature)`：无则 `{ type:"thinking", thinking:"", thinkingSignature }` push + `thinking_start`。
    `ensureToolCallBlock(toolCall: StreamingToolCallDelta)`：
    ```
    streamIndex = typeof toolCall.index === "number" ? toolCall.index : undefined
    name = toolCall.function?.name ?? toolCall.custom?.name ?? ""
    block = streamIndex !== undefined ? byIndex.get(streamIndex) : undefined
    if (!block && toolCall.id) block = byId.get(toolCall.id)
    if (!block):
        customInputProperty = (toolCall.custom && !toolCall.function)
            ? (grammarToolInputProperties.get(name) ?? "input") : undefined
        block = { type:"toolCall", id: toolCall.id || "", name,
                  arguments: customInputProperty !== undefined ? { [customInputProperty]: "" } : {} }
        toolCallStates.set(block, { streamIndex,
            input: customInputProperty !== undefined ? customInput(customInputProperty)
                                                     : { kind:"json", arguments: new ToolCallArgumentsStream() } })
        if streamIndex !== undefined: byIndex.set(streamIndex, block)
        if toolCall.id: byId.set(toolCall.id, block)
        blocks.push(block); push toolcall_start
    state = toolCallStates.get(block)!
    if streamIndex !== undefined && state.streamIndex === undefined:
        state.streamIndex = streamIndex; byIndex.set(streamIndex, block)
    if toolCall.id: byId.set(toolCall.id, block)
    if (!block.name && name) block.name = name
    if (toolCall.custom && !toolCall.function && state.input.kind !== "custom"):
        customInputProperty = grammarToolInputProperties.get(block.name) ?? "input"
        block.arguments = { [customInputProperty]: "" }
        state.input = customInput(customInputProperty)
    return { block, state }
    ```
    `customInput(property) = { kind:"custom", property, buffer:{ input:"", started:false, closed:false } }`。
14. **chunk 循环**（`for await (chunk of openaiStream)`，先 `await options?.onProviderStreamEvent?.(chunk, model)`）：
    - `!chunk || typeof chunk !== "object"` → continue（**忽略 null chunk**）
    - `output.responseId ||= chunk.id`
    - `typeof chunk.model === "string" && chunk.model.length > 0 && chunk.model !== model.id` →
      `output.responseModel ||= chunk.model`
    - `chunk.usage` → `output.usage = parseChunkUsage(chunk.usage, model)`
    - `choice = Array.isArray(chunk.choices) ? chunk.choices[0] : undefined`；无则 continue
    - `!chunk.usage && choice.usage` → `output.usage = parseChunkUsage(choice.usage, model)`（Moonshot 回退）
    - `choice.finish_reason` 存在 →
      `output.rawStopReason = it`；`mapStopReason(...)`；`hasFinishReason = true`
    - `choice.delta` 存在时：
      1. **推理字段**（按 `["reasoning_content","reasoning","reasoning_text"]` 顺序找第一个非空字符串）：
         ```
         thinkingSignature = (model.provider === "opencode-go" && field === "reasoning") ? "reasoning_content" : field
         block = ensureThinkingBlock(thinkingSignature); block.thinking += delta; push thinking_delta
         ```
      2. **reasoning_details**：`Array.isArray(choice.delta.reasoning_details)` 时逐项：
         `isOpenAIReasoningDetail` 通过 → `ensureThinkingBlock("")`；`streamedReasoningDetails ??= []`；
         `appendOpenAIReasoningDetail(...)`（**不发事件**）
      3. **content**：`!== null && !== undefined && length > 0` → `ensureTextBlock()`，`block.text += content`，
         push `text_delta`
      4. **tool_calls**：逐个
         ```
         { block, state } = ensureToolCallBlock(toolCall)
         if (!block.id && toolCall.id) { block.id = toolCall.id; byId.set(id, block) }
         name = toolCall.function?.name ?? toolCall.custom?.name
         if (!block.name && name) block.name = name
         delta = ""
         if (toolCall.function?.arguments && state.input.kind === "json"):
             delta = toolCall.function.arguments
             block.arguments = state.input.arguments.append(delta)
         else if (toolCall.custom?.input && state.input.kind === "custom"):
             delta = appendGrammarToolInputJsonDelta(state.input.buffer, state.input.property, toolCall.custom.input) ?? ""
             block.arguments = { [state.input.property]: state.input.buffer.input }
         push toolcall_delta { contentIndex, delta, partial }
         ```
         注意：即使 `delta === ""` **也** push 事件。
15. **收尾**：`for (block of blocks) finishBlock(block)`（按内容顺序；**所有**块都关，即使流里没有对应结束事件）。
16. **状态检查（顺序固定）**：
    ```
    if options?.signal?.aborted: throw new Error("Request was aborted")
    if output.stopReason === "aborted": throw new Error("Request was aborted")
    if (!hasFinishReason && !compat.supportsFinishReason):
        output.stopReason = output.content.some(b => b.type === "toolCall") ? "toolUse" : "stop"
    if output.stopReason === "error": throw new Error(output.errorMessage || "Provider returned an error stop reason")
    if ((compat.supportsFinishReason && !hasFinishReason) || output.stopReason === "pending"):
        throw new Error("Stream ended without finish_reason")
    ```
17. `stream.push({ type: "done", reason: output.stopReason, message: output }); stream.end()`。
18. **catch**：
    ```
    for (block of output.content) if (block.type === "thinking") applyStreamedReasoningDetails(block)
    output.stopReason = options?.signal?.aborted ? "aborted" : "error"
    output.errorMessage = formatProviderError(normalizeProviderError(error))
    rawMetadata = (error as any)?.error?.metadata?.raw
    if (rawMetadata && !output.errorMessage.includes(String(rawMetadata))):
        output.errorMessage += `\n${rawMetadata}`
    stream.push({ type: "error", reason: output.stopReason, error: output }); stream.end()
    ```

#### 2.10.5 `streamSimple`（导出）

```ts
getClientApiKey(model.provider, options?.apiKey, options?.headers);   // 同步抛
const base = { ...buildBaseOptions(model, context, options, options?.apiKey), toolChoice: options?.toolChoice };
const clampedReasoning = options?.reasoning ? clampThinkingLevel(model, options.reasoning) : undefined;
const reasoningEffort = clampedReasoning === "off" ? undefined : clampedReasoning;
return stream(model, context, { ...base, reasoningEffort, thinkingBudgets: options?.thinkingBudgets });
```
- `thinkingBudgets` **透传**（影响 `thinking_token_budget` 字段）。

#### 2.10.6 `createClient(...)`（内部）

```ts
headers = { ...getIdentityHeaders(model, identity), ...model.headers }
if (model.provider === "github-copilot"):
    hasImages = hasCopilotVisionInput(context.messages)
    Object.assign(headers, buildCopilotDynamicHeaders({ messages: context.messages, hasImages }))
if (sessionId && compat.sendSessionAffinityHeaders):
    if compat.sessionAffinityFormat === "openrouter":
        headers["x-session-id"] = sessionId
    else:
        if compat.sessionAffinityFormat === "openai": headers.session_id = sessionId
        headers["x-client-request-id"] = sessionId
        headers["x-session-affinity"] = sessionId
if (optionsHeaders) Object.assign(headers, optionsHeaders)
return new OpenAI({ apiKey, baseURL: model.baseUrl, dangerouslyAllowBrowser: true, fetch, defaultHeaders: headers })
```
- 注意：`openai-nosession` 格式**只**发 `x-client-request-id` + `x-session-affinity`（跳过 `session_id`）。

#### 2.10.7 `buildParams(...)`（内部）

签名：`(model, context, options?, compat = getCompat(model), cacheRetention = resolveCacheRetention(...), grammarToolInputProperties = createGrammarToolInputProperties(...))`

```
transcriptTools = resolveTranscriptTools(context.messages,
    (compat.supportsMidConvoSystemMessages === true && compat.supportsMidConvoToolAdditions === true)
      ? "additions" : "none")
messages = convertMessages(model, context, compat, { grammarToolInputProperties })
cacheControl = getCompatCacheControl(compat, cacheRetention)

params = {
  model: model.id,
  messages,
  stream: true,
  prompt_cache_key:
      (model.baseUrl.includes("api.openai.com") && cacheRetention !== "none")
      || (cacheRetention === "long" && compat.supportsLongCacheRetention)
        ? clampOpenAIPromptCacheKey(options?.sessionId) : undefined,
  prompt_cache_retention: (cacheRetention === "long" && compat.supportsLongCacheRetention) ? "24h" : undefined,
}
if (compat.supportsUsageInStreaming !== false) params.stream_options = { include_usage: true }
if (compat.supportsStore) params.store = false
if (options?.maxTokens):
    if compat.maxTokensField === "max_tokens": params.max_tokens = options.maxTokens
    else: params.max_completion_tokens = options.maxTokens
if (options?.temperature !== undefined) params.temperature = options.temperature
if (transcriptTools.requestTools.length > 0):
    params.tools = convertTools(transcriptTools.requestTools, compat)
    if (compat.zaiToolStream) params.tool_stream = true
else if (hasToolHistory(context.messages)):
    params.tools = []                     // Anthropic-via-proxy 需要
if (cacheControl) applyAnthropicCacheControl(messages, params.tools, cacheControl)
if (options?.toolChoice) params.tool_choice = options.toolChoice
if (compat.vllmPriority !== undefined) params.priority = compat.vllmPriority
```

**思考字段**：
```
thinkingTokenBudgetField = resolveThinkingTokenBudgetField(compat)
thinkingBudget = resolveClampedThinkingBudget(model, options, params)
```
然后按 `compat.thinkingFormat`（且 `model.reasoning`）分支；**顺序与条件完全照抄**：

| thinkingFormat | 条件 | 写入 |
|---|---|---|
| `"zai"` | `model.reasoning` | `thinking = reasoningEffort ? { type:"enabled", clear_thinking:false } : { type:"disabled" }`；若 `reasoningEffort && supportsReasoningEffort`：`reasoning_effort = thinkingLevelMap[level] ?? level`（仅 string） |
| `"qwen"` | `model.reasoning` | `enable_thinking = !!reasoningEffort`；若 `reasoningEffort && supportsReasoningEffort`：`reasoning_effort = thinkingLevelMap[level] ?? level`（仅 string） |
| `"qwen-chat-template"` | `model.reasoning` | `chat_template_kwargs = { enable_thinking: !!reasoningEffort, preserve_thinking: true }` |
| `"chat-template"` | `model.reasoning` | `chat_template_kwargs = buildChatTemplateValues(model, options, compat.chatTemplateKwargs, thinkingBudget)`（非 undefined 才写） |
| `"baseten"` | `model.reasoning` | `chat_template_args = buildChatTemplateValues(...compat.chatTemplateArgs...)`（非 undefined 才写）；若 `supportsReasoningEffort`：`effort = (reasoningEffort ? map[level] : map.off) ?? 原值`，string 才写 `reasoning_effort` |
| `"deepseek"` | `model.reasoning` | `reasoningEffort ? thinking={type:"enabled"} : (map.off !== null ? thinking={type:"disabled"} : 不写)`；若 `reasoningEffort && supportsReasoningEffort`：`reasoning_effort = map[level] ?? level`（**不检查 string**，直接赋） |
| `"openrouter"` | `model.reasoning` | `reasoningEffort ? reasoning={effort: map[level] ?? level} : (map.off !== null ? reasoning={effort: map.off ?? "none"} : 不写)` |
| `"ant-ling"` | `model.reasoning && reasoningEffort` | `effort = map[level]`；若 typeof string → `reasoning = { effort }` |
| `"together"` | `model.reasoning` | `reasoning = { enabled: !!reasoningEffort }`；若 `reasoningEffort && supportsReasoningEffort`：`reasoning_effort = map[level] ?? level` |
| `"string-thinking"` | `model.reasoning` | `reasoningEffort ? thinking = map[level] ?? level : (map.off !== null ? thinking = map.off ?? "none" : 不写)` |
| 其他（含 `"openai"`） | — | 若 `reasoningEffort && model.reasoning && supportsReasoningEffort`：`reasoning_effort = map[level] ?? level`；否则若 `!reasoningEffort && model.reasoning && supportsReasoningEffort` 且 `map.off` 为 string：`reasoning_effort = map.off` |

**预算字段**（独立于 thinkingFormat）：
```ts
if (thinkingTokenBudgetField && thinkingBudget !== undefined)
    params[thinkingTokenBudgetField] = thinkingBudget
```

**路由**：
- `model.compat?.openRouterRouting` → `params.provider = <routing 原对象>`
- `model.compat?.vercelGatewayRouting`：若 `routing.only || routing.order` →
  `gatewayOptions = {}`；`only` → `gatewayOptions.only = routing.only`；`order` → `gatewayOptions.order = routing.order`；
  `params.providerOptions = { gateway: gatewayOptions }`

**最后**：
```ts
samplingParams = resolveSamplingParams(model, options?.reasoningEffort ?? "off", options?.samplingParams)
if (samplingParams) Object.assign(params, samplingParams)
```

`resolveThinkingTokenBudgetField(compat)`：
```ts
if (compat.thinkingTokenBudgetField) return it
if (compat.supportsThinkingTokenBudget) return "thinking_token_budget"
return undefined
```

`resolveClampedThinkingBudget(model, options, params)`：
```ts
if (!options?.reasoningEffort || !model.reasoning) return undefined
ceiling = params.max_tokens ?? params.max_completion_tokens ?? model.maxTokens
budget = clampThinkingBudgetToAnswerRoom(thinkingBudgetForLevel(options.reasoningEffort, options.thinkingBudgets), ceiling)
return budget > 0 ? budget : undefined
```
- `ceiling` 用 `??`：`max_tokens === 0` 时**不会**回退。

#### 2.10.8 `buildChatTemplateValues` / `resolveChatTemplateKwargValue`（内部）

`buildChatTemplateValues(model, options, values, thinkingBudget?)`：
```
resolvedValues = {}
for [key, value] of Object.entries(values):
    r = resolveChatTemplateKwargValue(model, options, value, thinkingBudget)
    if r !== undefined: resolvedValues[key] = r
return Object.keys(resolvedValues).length > 0 ? resolvedValues : undefined
```
`resolveChatTemplateKwargValue(model, options, value, thinkingBudget?)`：
```
if (typeof value !== "object" || value === null) return value
reasoningEffort = options?.reasoningEffort
if (!reasoningEffort && value.omitWhenOff) return undefined
if (value.$var === "thinking.enabled") return !!reasoningEffort
if (value.$var === "thinking.budget") return thinkingBudget       // 可能 undefined
mapped = reasoningEffort ? model.thinkingLevelMap?.[reasoningEffort] : model.thinkingLevelMap?.off
return mapped === undefined ? reasoningEffort
     : typeof mapped === "string" ? mapped : undefined
```
- `$var: "thinking.budget"` 为 undefined 时该键被**丢弃**。
- 注意 schema 里还允许 `$var: "thinking.effort"`，但此函数**未处理**它——会落到最后的 mapped 分支。

#### 2.10.9 Anthropic 风格 cache_control（内部函数）

`getCompatCacheControl(compat, cacheRetention)`：
```
if (compat.cacheControlFormat !== "anthropic" || cacheRetention === "none") return undefined
ttl = (cacheRetention === "long" && compat.supportsLongCacheRetention) ? "1h" : undefined
return { type: "ephemeral", ...(ttl ? { ttl } : {}) }
```

`applyAnthropicCacheControl(messages, tools, cacheControl)`：
1. `addCacheControlToSystemPrompt`：**第一条** `system`/`developer` 消息 → `addCacheControlToInstructionMessage`
2. `addCacheControlToLastTool`：`tools` 非空 → 最后一个 tool 的 `cache_control = cacheControl`
3. `addCacheControlToLastConversationMessage`：**从后往前**第一条 `user`/`assistant`/`tool` 消息，
   若 `addCacheControlToMessage` 返回 true 则停止

`addCacheControlToTextContent(message, cacheControl) -> boolean`：
```
content = message.content
if typeof content === "string":
    if content.length === 0: return false
    message.content = [{ type:"text", text: content, cache_control }]; return true
if !Array.isArray(content): return false
for i from end to 0:
    part = content[i]
    if part?.type === "text": part.cache_control = cacheControl; return true
return false
```
- `addCacheControlToMessage` 只处理 `user`/`assistant`/`tool`。
- `addCacheControlToInstructionMessage` 直接调 `addCacheControlToTextContent`。

#### 2.10.10 `convertMessages(model, context, compat, options?)`（**导出**）

**注意：这是本模块导出的函数（`export function convertMessages`）**，被 `test/openai-completions-tool-result-images.test.ts` 直接调用。

```
normalizedContext = resolveTranscript(context, compat.supportsMidConvoSystemMessages)
params = []
```
`normalizeToolCallId(id)`（内部）：
```
if id.includes("|"):
    separatorIndex = id.indexOf("|")
    callId = id.slice(0, separatorIndex).replace(/[^a-zA-Z0-9_-]/g, "_")
    itemId = id.slice(separatorIndex + 1).replace(/[^a-zA-Z0-9_-]/g, "_")
    combinedId = itemId.length > 0 ? `${callId}_${itemId}` : callId
    if (combinedId.length <= 40) return combinedId
    hash = shortHash(id).slice(0, 8)
    prefix = callId.slice(0, Math.max(1, 40 - hash.length - 1))
    return `${prefix}_${hash}`
if (model.provider === "openai") return id.length > 40 ? id.slice(0, 40) : id
return id
```
（`hash` 是 `shortHash(id)` 的**前 8 个字符**，故 `hash.length` **恒为 8** → prefix 长度 `max(1, 40-8-1)=31`，
最终 id 恒为 31+1+8 = **40** 字符。注意 `shortHash` 本体返回 `(h2>>>0).toString(36) + (h1>>>0).toString(36)`，
两段各 1–7 位 base36，全长 **13–14** 字符（实测：`"call_abc|item_1"` → 13，`"resp_123|fc_456"` → 14，
20 万随机样本最大 14）；但此处先 `.slice(0, 8)`，所以规范里**不要**用 13/14 去算 prefix。
Rust 侧必须逐位复刻 `Math.imul` 的 32 位有符号语义，否则 hash 值不同 → 跨版本 tool call id 不匹配。）

`shortHash` 本体（**此前规范只引用、未给出算法；跨版本 tool call id 依赖它逐位一致，故在此补全**）：
```ts
export function shortHash(str: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < str.length; i++) {          // 按 **UTF-16 码元**遍历（不是码点！）
    const ch = str.charCodeAt(i);
    h1 = Math.imul(h1 ^ ch, 2654435761);
    h2 = Math.imul(h2 ^ ch, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (h2 >>> 0).toString(36) + (h1 >>> 0).toString(36);
}
```
要点：
- `Math.imul(a, b)` 的结果是 **32 位有符号**整数（`(a*b) mod 2^32` 再按二补数解释）；随后 `^` 也是 32 位有符号。
  第二条 `h2 = ... ^ Math.imul(h1 ^ (h1 >>> 13), ...)` 用的是**已被更新过的** `h1`（顺序敏感）。
- `>>>` 是**逻辑右移**（高位补 0）；Rust 用 `((x as u32) >> n) as i32`，不要用算术右移。
- `>>> 0` 把结果转成 u32 再 `toString(36)`（小写字母 + 数字，无前导零）。
- 返回值长度：两段各 1–7 位 base36，总长 13–14 字符（见上）。
- 输入是 UTF-16 码元：Rust 侧不能直接遍历 `chars()`，必须遍历 `encode_utf16()`。
- **不要**引入 `DefaultHasher`/`fxhash` 等替代实现；已有会话文件里的 tool call id 与新生成 id 必须一致。

```
transformedMessages = transformMessages(normalizedContext.messages, model, (id) => normalizeToolCallId(id))
transcriptTools = resolveTranscriptTools(normalizedContext.messages,
    (compat.supportsMidConvoSystemMessages === true && compat.supportsMidConvoToolAdditions === true) ? "additions" : "none")
instructionRole = (model.reasoning && compat.supportsDeveloperRole) ? "developer" : "system"
lastRole = null; systemIndex = 0
```

主循环（`for i`）：
1. `compat.requiresAssistantAfterToolResult && lastRole === "toolResult" && msg.role === "user"` →
   push `{ role:"assistant", content: "I have processed the tool results." }`（**逐字**）
2. **system**：
   - `addedTools = systemIndex++ >= transcriptTools.foldedSystemMessages ? (msg.toolsAdded ?? []) : []`
   - 若 `addedTools.length > 0` → push `{ role:"system", tools: convertTools(addedTools, compat) }`
     （Kimi 风格的 `system.tools` 字段；**不是**标准 role/content）
   - `text = i === 0 ? getSystemMessageText(msg) : renderSystemMessageUpdate(msg)`
   - `text.length > 0` → push `{ role: instructionRole, content: sanitizeSurrogates(text) }`
3. **user**：
   - 字符串 → push `{ role:"user", content: sanitizeSurrogates(content) }`（空串也 push）
   - 数组 → 过滤掉 `type === "text" && text.length === 0` 的项；映射：
     text → `{ type:"text", text: sanitizeSurrogates(text) }`；
     image → `{ type:"image_url", image_url: { url: \`data:${mimeType};base64,${data}\` } }`；
     结果为空 → continue；否则 push。
4. **assistant**：
   ```
   assistantMsg = { role:"assistant", content: compat.requiresAssistantAfterToolResult ? "" : null }
   assistantTextParts = 非空 trim 的 text 块 → { type:"text", text: sanitizeSurrogates(text) }
   assistantText = parts.map(p => p.text).join("")
   thinkingBlocks, toolCalls
   signedReasoningDetails  = thinkingBlocks.map(b => parseOpenAIReasoningDetails(b.thinkingSignature)).find(d => d !== undefined)
   legacyReasoningDetails  = toolCalls.map(tc => parseLegacyEncryptedReasoningDetail(tc.thoughtSignature)).filter(ok)
   preservedReasoningDetails = signedReasoningDetails ?? (legacy.length > 0 ? legacy : undefined)
   nonEmptyThinkingBlocks = thinkingBlocks.filter(b => b.thinking.trim().length > 0)
   if (nonEmptyThinkingBlocks.length > 0):
       if (compat.requiresThinkingAsText):
           thinkingText = nonEmpty.map(b => sanitizeSurrogates(b.thinking)).join("\n\n")
           assistantMsg.content = [{ type:"text", text: thinkingText }, ...assistantTextParts]
       else:
           if (assistantText.length > 0) assistantMsg.content = assistantText
           if (!preservedReasoningDetails):
               signature = nonEmpty[0].thinkingSignature
               if (model.provider === "opencode-go" && signature === "reasoning") signature = "reasoning_content"
               if (signature && isOpenAICompletionsReasoningField(signature)):
                   assistantMsg[signature] = nonEmpty.map(b => b.thinking).join("\n")
   else if (assistantText.length > 0):
       assistantMsg.content = assistantText
   ```
   tool_calls：
   ```
   for tc of toolCalls:
       customInputProperty = options?.grammarToolInputProperties?.get(tc.name)
       if (customInputProperty !== undefined):
           { id: tc.id, type:"custom", custom: { name: tc.name,
             input: sanitizeSurrogates(getGrammarToolInput(tc.name, tc.arguments, customInputProperty)) } }
       else:
           { id: tc.id, type:"function", function: { name: tc.name, arguments: JSON.stringify(tc.arguments) } }
   ```
   然后：
   - `preservedReasoningDetails` → `assistantMsg.reasoning_details = it`
   - `compat.requiresReasoningContentOnAssistantMessages && model.reasoning && assistantMsg.reasoning_content === undefined`
     → `assistantMsg.reasoning_content = ""`
   - 跳过条件：`content` 为 null/undefined/空字符串/空数组 **且** 无 `tool_calls` → `continue`
5. **toolResult（合并连续）**：
   ```
   imageBlocks = []; j = i
   for (; j < len && role === "toolResult"; j++):
       textResult = content.filter(text).map(b => b.text).join("\n")
       hasImages = content.some(image)
       hasText = textResult.trim().length > 0
       toolResultText = hasText ? textResult : hasImages ? "(see attached image)" : "(no tool output)"
       toolResultMsg = { role:"tool", content: sanitizeSurrogates(toolResultText), tool_call_id: msg.toolCallId }
       if (compat.requiresToolResultName && msg.toolName) toolResultMsg.name = msg.toolName
       push it
       if (hasImages && model.input.includes("image")): 收集 image_url 块
   i = j - 1
   if (imageBlocks.length > 0):
       if (compat.requiresAssistantAfterToolResult):
           push { role:"assistant", content: "I have processed the tool results." }
       push { role:"user", content: [
           { type:"text", text: "Attached image(s) from tool result:" }, ...imageBlocks ] }
       lastRole = "user"
   else: lastRole = "toolResult"
   continue
   ```
6. 末尾 `lastRole = msg.role`。

- 占位文本逐字：`"(see attached image)"`、`"(no tool output)"`、`"Attached image(s) from tool result:"`。

#### 2.10.11 `convertTools(tools, compat)`（内部）

```
tools.map(tool => {
  grammar = resolveGrammarConstrainedSampling(tool, compat.supportsOpenAIGrammarTools)
  if (grammar):
      return { type:"custom", custom: { name, description,
               format: { type:"grammar", grammar: { syntax: grammar.format, definition: grammar.definition } } } }
  strict = resolveJsonSchemaStrictSampling(tool, compat.supportsStrictMode !== false)
  return { type:"function", function: { name, description,
           parameters: getJsonSchemaToolParameters(tool, strict),
           ...(compat.supportsStrictMode !== false && { strict: strict ?? false }) } }
})
```

#### 2.10.12 `parseChunkUsage(rawUsage, model)`（内部）

```
promptTokens = rawUsage.prompt_tokens || 0
cacheReadTokens = rawUsage.prompt_tokens_details?.cached_tokens
               ?? rawUsage.prompt_cache_hit_tokens
               ?? rawUsage.cached_tokens ?? 0
cacheWriteTokens = rawUsage.prompt_tokens_details?.cache_write_tokens || 0
input = max(0, promptTokens - cacheReadTokens - cacheWriteTokens)
outputTokens = rawUsage.completion_tokens || 0
usage = { input, output: outputTokens, cacheRead, cacheWrite,
          reasoning: rawUsage.completion_tokens_details?.reasoning_tokens || 0,
          totalTokens: input + output + cacheRead + cacheWrite,
          cost: {0...} }
calculateCost(model, usage)
if isOpenRouterModel(model): applyOpenRouterReportedCost(usage, rawUsage)
return usage
```
- **不减**扣 cache write —— 与某些实现不同，此处 `input` 同时扣 `cacheRead` 与 `cacheWrite`。

#### 2.10.13 `mapStopReason(reason)`（内部）

```
if reason === null return { stopReason: "stop" }
"stop" | "end"          -> stop
"length"                -> length
"function_call"         -> toolUse
"tool_calls"            -> toolUse
"content_filter"        -> error,  errorMessage: "Provider finish_reason: content_filter"
"network_error"         -> error,  errorMessage: "Provider finish_reason: network_error"
default                 -> error,  errorMessage: `Provider finish_reason: ${reason}`
```

#### 2.10.14 `detectCompat(model)`（内部）

布尔判定（provider 或 baseUrl 子串，**大小写敏感**，除 `isDeepSeek` 用 `toLowerCase()`）：
```
isZai            = provider ∈ {"zai","zai-coding-cn"} || baseUrl.includes("api.z.ai") || baseUrl.includes("open.bigmodel.cn")
isTogether       = provider === "together" || includes("api.together.ai") || includes("api.together.xyz")
isMoonshot       = provider ∈ {"moonshotai","moonshotai-cn"} || includes("api.moonshot.")
isOpenRouter     = provider === "openrouter" || includes("openrouter.ai")
isCloudflareWorkersAI = provider === "cloudflare-workers-ai" || includes("api.cloudflare.com")
isCloudflareAiGateway = provider === "cloudflare-ai-gateway" || includes("gateway.ai.cloudflare.com")
isNvidia         = provider === "nvidia" || includes("integrate.api.nvidia.com")
isAntLing        = provider === "ant-ling" || includes("api.ant-ling.com")
isCerebras       = provider === "cerebras" || includes("cerebras.ai")
isDeepSeek       = provider === "deepseek" || baseUrl.toLowerCase().includes("deepseek.com")
isNonStandard    = isNvidia || isCerebras || provider === "xai" || includes("api.x.ai") || isTogether
                || includes("chutes.ai") || isDeepSeek || isZai || isMoonshot
                || provider === "opencode" || includes("opencode.ai")
                || isCloudflareWorkersAI || isCloudflareAiGateway || isAntLing
useMaxTokens     = includes("chutes.ai") || isDeepSeek || isMoonshot || isCloudflareAiGateway
                || isTogether || isNvidia || isAntLing || isZai
isGrok           = provider === "xai" || includes("api.x.ai")
isOpenRouterDeveloperRoleModel = isOpenRouter && (id.startsWith("anthropic/") || id.startsWith("openai/"))
cacheControlFormat = (provider === "openrouter" && id.startsWith("anthropic/")) ? "anthropic" : undefined
```
返回对象（**字段顺序即下列顺序**）：
```ts
{
  supportsStore: !isNonStandard,
  supportsDeveloperRole: isOpenRouterDeveloperRoleModel || (!isNonStandard && !isOpenRouter),
  supportsReasoningEffort: !isGrok && !isZai && !isMoonshot && !isTogether && !isCloudflareAiGateway && !isNvidia && !isAntLing,
  supportsUsageInStreaming: true,
  supportsFinishReason: true,
  maxTokensField: useMaxTokens ? "max_tokens" : "max_completion_tokens",
  requiresToolResultName: false,
  requiresAssistantAfterToolResult: false,
  requiresThinkingAsText: false,
  requiresReasoningContentOnAssistantMessages: isDeepSeek,
  thinkingFormat: isDeepSeek ? "deepseek" : isZai ? "zai" : isTogether ? "together"
                  : isAntLing ? "ant-ling" : isOpenRouter ? "openrouter" : "openai",
  openRouterRouting: {},
  vercelGatewayRouting: {},
  chatTemplateKwargs: {},
  chatTemplateArgs: {},
  zaiToolStream: false,
  supportsThinkingTokenBudget: false,
  thinkingTokenBudgetField: undefined,
  supportsStrictMode: false,
  supportsOpenAIGrammarTools: false,
  supportsMidConvoSystemMessages: false,
  supportsMidConvoToolAdditions: false,
  cacheControlFormat,
  sendSessionAffinityHeaders: isOpenRouter,
  sessionAffinityFormat: isOpenRouter ? "openrouter" : "openai",
  supportsLongCacheRetention: !(isTogether || isCloudflareWorkersAI || isCloudflareAiGateway || isNvidia || isAntLing),
}
```

#### 2.10.15 `getCompat(model)`（内部）

逐字段 `model.compat.X ?? detected.X`，但例外：
- `openRouterRouting: model.compat.openRouterRouting ?? {}`（**不**取 detected，detected 恒为 `{}`）
- `vllmPriority: model.compat.vllmPriority`（**无**默认）
- 其余与 `detectCompat` 返回值一一对应。

确切字段列表见源码 1646–1700 行；顺序为：
`supportsStore, supportsDeveloperRole, supportsReasoningEffort, supportsUsageInStreaming, supportsFinishReason,
maxTokensField, requiresToolResultName, requiresAssistantAfterToolResult, requiresThinkingAsText,
requiresReasoningContentOnAssistantMessages, thinkingFormat, openRouterRouting, vercelGatewayRouting,
chatTemplateKwargs, chatTemplateArgs, zaiToolStream, supportsThinkingTokenBudget, thinkingTokenBudgetField,
supportsStrictMode, supportsOpenAIGrammarTools, supportsMidConvoSystemMessages, supportsMidConvoToolAdditions,
cacheControlFormat, sendSessionAffinityHeaders, sessionAffinityFormat, supportsLongCacheRetention, vllmPriority`。

### 2.11 `openai-responses.ts`

文件 453 行。

#### 2.11.1 常量

```ts
const OPENAI_TOOL_CALL_PROVIDERS = new Set(["openai", "openai-codex", "opencode"]);
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS = 16;   // OpenAI Responses 拒绝 < 16 的 max_output_tokens
const CHATGPT_USAGE_URL = "https://chatgpt.com/settings/usage";
const CHATGPT_WORKSPACE_LIMIT_HINT =
  "You've reached your ChatGPT workspace's usage limit. Ask your workspace admin to increase it at admin.openai.com → Workspace → Usage limits.";
const CONFIGURATION_UPDATE_REQUEST_EFFORT = "medium";
```
注意 `CHATGPT_WORKSPACE_LIMIT_HINT` 中的 `→` 是 **U+2192 右箭头**，前后各一个半角空格，句末有句点。

#### 2.11.2 `isChatGPTSignIn(model, apiKey)`（内部）

```ts
model.provider === "openai" && model.baseUrl === "https://api.openai.com/v1"
&& apiKey !== undefined && !apiKey.startsWith("sk-")
```

#### 2.11.3 `getCompat(model) -> Required<OpenAIResponsesCompat>`（内部）

```ts
{
  supportsDeveloperRole: model.compat?.supportsDeveloperRole ?? true,
  supportsMidConvoSystemMessages: ... ?? false,
  sessionAffinityFormat: ... ?? detectSessionAffinityFormat(model),
  supportsLongCacheRetention: ... ?? true,
  supportsStrictMode: ... ?? false,
  supportsOpenAIGrammarTools: ... ?? false,
  supportsAdditionalTools: ... ?? false,
  supportsToolSearch: ... ?? false,
  supportsExplicitPromptCacheMode: ... ?? false,
  supportsMaxOutputTokens: ... ?? true,
  supportsMidConvoEffort: ... ?? false,
}
```
`detectSessionAffinityFormat(model)`：`provider === "openrouter" || baseUrl.includes("openrouter.ai")`
→ `"openrouter"`，否则 `"openai"`。

#### 2.11.4 `getConfigurationUpdateEffort(model, options, compat)`（内部）

```ts
if (!compat.supportsMidConvoEffort || !options?.reasoningEffort || isChatGPTSignIn(model, options.apiKey))
  return undefined;
return model.thinkingLevelMap?.[options.reasoningEffort] ?? options.reasoningEffort;
```

#### 2.11.5 `getPromptCacheRetention` / `getPromptCacheOptions`（内部）

```ts
getPromptCacheRetention(compat, cacheRetention):
  return (cacheRetention === "long" && compat.supportsLongCacheRetention && !compat.supportsExplicitPromptCacheMode)
    ? "24h" : undefined;

getPromptCacheOptions(compat, cacheRetention):
  if (!compat.supportsExplicitPromptCacheMode) return undefined;
  if (cacheRetention === "none") return { mode: "explicit" };
  if (cacheRetention === "long" && compat.supportsLongCacheRetention) return { ttl: "30m" };
  return undefined;
```

#### 2.11.6 `OpenAIResponsesOptions`（导出）

```ts
export interface OpenAIResponsesOptions extends StreamOptions {
  reasoningEffort?: "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
  reasoningSummary?: "auto" | "detailed" | "concise" | null;
  serviceTier?: ResponseCreateParamsStreaming["service_tier"];    // "auto" | "default" | "flex" | "priority" | "fast"
  toolChoice?: ResponseCreateParamsStreaming["tool_choice"];
}
```

#### 2.11.7 `stream`（导出）

1. 同步：`normalizedContext = resolveTranscript(context, getCompat(model).supportsMidConvoSystemMessages)`。
2. `compat = getCompat(model)`；`configurationUpdateEffort = getConfigurationUpdateEffort(...)`。
3. `output` 构造，当 `configurationUpdateEffort !== undefined` 时**额外**带
   `providerThinkingLevel: configurationUpdateEffort`（键位置在 `model` 之后、`usage` 之前）。
4. `apiKey = getClientApiKey(...)`；`cacheRetention`/`cacheSessionId`；`grammarToolInputProperties`；
   `client = createClient(...)`。
5. `params = buildParams(model, normalizedContext, options, compat, grammarToolInputProperties, configurationUpdateEffort)`；
   `onPayload` 可整体替换。
6. `requestOptions`（同 completions）。
7. `retryProviderRequest(() => client.responses.create(params, requestOptions).withResponse(), {...})`。
8. `onResponse` → `start` → `processResponsesStream(openaiStream, output, stream, model, {
     onProviderStreamEvent, serviceTier: options?.serviceTier, grammarToolInputProperties,
     applyServiceTierPricing: (usage, tier) => applyServiceTierPricing(usage, tier, model) })`。
9. 收尾（顺序）：
   ```
   if signal.aborted: throw "Request was aborted"
   if output.stopReason === "pending": throw "OpenAI Responses stream ended without a stop reason"
   if output.stopReason ∈ {aborted, error}: throw (output.errorMessage || "An unknown error occurred")
   push done; end
   ```
10. catch：
    ```
    output.stopReason = signal?.aborted ? "aborted" : "error"
    errorMessage = formatProviderError(normalizeProviderError(error),
                     `${model.provider === "openai" ? "OpenAI" : model.provider} API error`)
    if errorMessage.includes("subscription_sharing_usage_limit_exceeded"):
        output.errorMessage = `${errorMessage}\nCheck your ChatGPT usage: ${CHATGPT_USAGE_URL}`
    else if errorMessage.includes("subscription_sharing_workplace_individual_usage_limit_exceeded"):
        output.errorMessage = `${errorMessage}\n${CHATGPT_WORKSPACE_LIMIT_HINT}`
    else: output.errorMessage = errorMessage
    push error; end
    ```
    注意两个判定都是**子串包含**。

#### 2.11.8 `streamSimple`（导出）

与 completions 一致（`getClientApiKey` 同步抛；`clampThinkingLevel`；`reasoningEffort`），
但**不**透传 `thinkingBudgets`。

#### 2.11.9 `createClient(...)`（内部）

```ts
headers = { ...getIdentityHeaders(model, identity), ...model.headers }
if (model.provider === "github-copilot"): 合并 Copilot 动态头（用 context.messages）
if (sessionId):
    if compat.sessionAffinityFormat === "openrouter": headers["x-session-id"] = sessionId
    else:
        if compat.sessionAffinityFormat === "openai": headers.session_id = sessionId
        headers["x-client-request-id"] = sessionId
        // 注意：Responses 版**没有** x-session-affinity
if (optionsHeaders) Object.assign(headers, optionsHeaders)
return new OpenAI({ apiKey, baseURL: model.baseUrl, dangerouslyAllowBrowser: true, fetch, defaultHeaders: headers })
```
- 与 completions 的差别（**三处，不要只记一处**）：
  1. Responses **不设** `x-session-affinity`；
  2. Responses 的 `if (sessionId)` **没有** `compat.sendSessionAffinityHeaders` 开关——
     completions 是 `if (sessionId && compat.sendSessionAffinityHeaders)`。也就是说 Responses 侧
     `sendSessionAffinityHeaders: false` **不起作用**，只要 `sessionId` 非空且 `cacheRetention !== "none"`
     就会发 `x-client-request-id`（以及 `session_id`，当格式为 `openai`）；
  3. Responses 的 `sessionAffinityFormat` 缺省由 `detectSessionAffinityFormat(model)` 得到
     （`openrouter` / `openai` 二选一，**没有** `openai-nosession`），见 2.11.3。

#### 2.11.10 `buildParams(...)`（内部）

```
transcriptTools = resolveTranscriptTools(context.messages, getResponsesToolSupport(compat))
messages = convertResponsesMessages(model, context, OPENAI_TOOL_CALL_PROVIDERS, {
  grammarToolInputProperties,
  supportsMidConvoSystemMessages: compat.supportsMidConvoSystemMessages,
  supportsAdditionalTools: compat.supportsAdditionalTools,
  supportsToolSearch: compat.supportsToolSearch,
  toolOptions: { supportsStrictMode: compat.supportsStrictMode,
                 supportsOpenAIGrammarTools: compat.supportsOpenAIGrammarTools },
  configurationUpdateEffort,
})
cacheRetention = resolveCacheRetention(...)
omitUnsupportedFields = isChatGPTSignIn(model, options?.apiKey)
params = {
  model: model.id,
  input: messages,
  stream: true,
  prompt_cache_key: cacheRetention === "none" ? undefined : clampOpenAIPromptCacheKey(options?.sessionId),
  prompt_cache_retention: omitUnsupportedFields ? undefined : getPromptCacheRetention(compat, cacheRetention),
  prompt_cache_options: omitUnsupportedFields ? undefined : getPromptCacheOptions(compat, cacheRetention),
  store: false,
}
if (options?.maxTokens && compat.supportsMaxOutputTokens && !omitUnsupportedFields):
    params.max_output_tokens = Math.max(options.maxTokens, 16)
if (options?.temperature !== undefined && !omitUnsupportedFields): params.temperature = options.temperature
if (options?.serviceTier !== undefined): params.service_tier = options.serviceTier
if (transcriptTools.requestTools.length > 0):
    params.tools = convertResponsesTools(transcriptTools.requestTools, {
      supportsStrictMode: compat.supportsStrictMode,
      supportsOpenAIGrammarTools: compat.supportsOpenAIGrammarTools })
if (options?.toolChoice !== undefined): params.tool_choice = options.toolChoice

reasoningEffort = options?.reasoningEffort ?? (options?.reasoningSummary ? "medium" : undefined)
if (model.reasoning):
    if (reasoningEffort):
        requestedEffort = options?.reasoningEffort
            ? (model.thinkingLevelMap?.[options.reasoningEffort] ?? options.reasoningEffort)
            : reasoningEffort
        effort = configurationUpdateEffort === undefined ? requestedEffort : "medium"
        params.reasoning = { effort, summary: options?.reasoningSummary || "auto" }
        params.include = ["reasoning.encrypted_content"]
    else if (model.provider !== "github-copilot" && model.thinkingLevelMap?.off !== null):
        params.reasoning = { effort: model.thinkingLevelMap?.off ?? "none" }
    if (model.provider === "xai"): params.include = ["reasoning.encrypted_content"]

samplingParams = resolveSamplingParams(model, reasoningEffort ?? "off", options?.samplingParams)
if (samplingParams) Object.assign(params, samplingParams)
```
**要点**：
- ChatGPT sign-in 时 `prompt_cache_retention`、`prompt_cache_options`、`max_output_tokens`、`temperature`
  全部**不出现**。
- `prompt_cache_key` 在 ChatGPT sign-in 时**仍会**出现（只受 `cacheRetention === "none"` 控制）。
- `reasoningSummary` 单独存在（无 `reasoningEffort`）时，effort 回落 `"medium"`。
- `reasoning.summary` 为 `options.reasoningSummary || "auto"`（`null` → `"auto"`）。
- `xai` 即使无 reasoning 也设 `include`。
- `github-copilot` 在无 reasoning 时**不**设 `reasoning`。

#### 2.11.11 服务层定价（内部）

```ts
function getServiceTierCostMultiplier(model, serviceTier) {
  switch (serviceTier) {
    case "flex": return 0.5;
    case "priority": case "fast": return model.id === "gpt-5.5" ? 2.5 : 2;
    default: return 1;
  }
}
function applyServiceTierPricing(usage, serviceTier, model) {
  multiplier = ...; if (multiplier === 1) return;
  usage.cost.input *= multiplier; output; cacheRead; cacheWrite 同样
  usage.cost.total = input + output + cacheRead + cacheWrite
}
```

### 2.12 `openai-responses-shared.ts`

文件 891 行。导出：`getResponsesToolSupport`、`convertResponsesMessages`、`convertResponsesTools`、
`processResponsesStream`；类型 `OpenAIResponsesStreamOptions`、`ConvertResponsesMessagesOptions`、
`ConvertResponsesToolsOptions`。

#### 2.12.1 文本签名编码（内部）

```ts
function encodeTextSignatureV1(id: string, phase?: TextSignatureV1["phase"]): string {
  const payload: TextSignatureV1 = { v: 1, id };
  if (phase) payload.phase = phase;
  return JSON.stringify(payload);      // {"v":1,"id":"..."} 或 {"v":1,"id":"...","phase":"..."}
}

function parseTextSignature(signature: string | undefined) {
  if (!signature) return undefined;
  if (signature.startsWith("{")) {
    try {
      parsed = JSON.parse(signature);
      if (parsed.v === 1 && typeof parsed.id === "string") {
        if (parsed.phase === "commentary" || parsed.phase === "final_answer")
          return { id: parsed.id, phase: parsed.phase };
        return { id: parsed.id };
      }
    } catch { /* 落入 legacy 处理 */ }
  }
  return { id: signature };     // 旧格式：整个签名就是 message id
}
```

#### 2.12.2 `convertToolResultOutput(model, content)`（内部）

```
textResult = content.filter(text).map(c => c.text).join("\n")
images = content.filter(image)
hasText = textResult.trim().length > 0
if (images.length === 0 || !model.input.includes("image")):
    return sanitizeSurrogates(hasText ? textResult : images.length > 0 ? "(see attached image)" : "(no tool output)")
output = []
if (hasText) output.push({ type:"input_text", text: sanitizeSurrogates(textResult) })
for image of images:
    output.push({ type:"input_image", detail:"auto", image_url: `data:${image.mimeType};base64,${image.data}` })
return output
```

#### 2.12.3 `OpenAIResponsesStreamOptions` / `ConvertResponsesMessagesOptions` / `ConvertResponsesToolsOptions`（导出接口）

```ts
export interface OpenAIResponsesStreamOptions {
  onProviderStreamEvent?: StreamOptions["onProviderStreamEvent"];
  serviceTier?: ResponseCreateParamsStreaming["service_tier"];
  grammarToolInputProperties?: ReadonlyMap<string, string>;
  resolveServiceTier?: (responseServiceTier, requestServiceTier) => ... | undefined;
  applyServiceTierPricing?: (usage: Usage, serviceTier) => void;
}

export interface ConvertResponsesMessagesOptions {
  includeSystemPrompt?: boolean;              // 默认 true
  grammarToolInputProperties?: ReadonlyMap<string, string>;
  supportsMidConvoSystemMessages?: boolean;
  supportsAdditionalTools?: boolean;
  supportsToolSearch?: boolean;
  toolOptions?: ConvertResponsesToolsOptions;
  configurationUpdateEffort?: string;
}

export interface ConvertResponsesToolsOptions {
  strict?: boolean | null;                    // 默认 false（undefined 时）
  supportsStrictMode?: boolean;               // 默认 true
  supportsOpenAIGrammarTools?: boolean;       // 默认 false
  toolSearchResult?: boolean;
}
```

#### 2.12.4 `getResponsesToolSupport(options) -> "none" | "additions" | "changes"`（导出）

```ts
if (options.supportsMidConvoSystemMessages && options.supportsAdditionalTools) return "changes";
return options.supportsToolSearch ? "additions" : "none";
```

#### 2.12.5 工具变更文案（内部）

`renderIncrementalToolsNotice(unavailable: ReadonlySet<string>)`：
```
notice = "This is an incremental tools update. Previously declared tools remain available unless explicitly marked unavailable. A tool declared again here is available again, and its latest definition replaces the earlier one."
return unavailable.size > 0 ? `${notice}\nStill unavailable: ${[...unavailable].join(", ")}.` : notice
```

`renderToolRemovalNotice(names)`：
```
`The following tools are no longer available. Do not call them:\n${names.map(n => `- ${n}`).join("\n")}`
```

#### 2.12.6 `convertResponsesMessages(model, context, allowedToolCallProviders, options?) -> ResponseInput`（导出）

```
normalizedContext = resolveTranscript(context, options?.supportsMidConvoSystemMessages)
messages = []
```

ID 归一化：
```ts
normalizeIdPart(part):
  sanitized = part.replace(/[^a-zA-Z0-9_-]/g, "_")
  normalized = sanitized.length > 64 ? sanitized.slice(0, 64) : sanitized
  return normalized.replace(/_+$/, "")          // 去尾部下划线

buildForeignResponsesItemId(itemId):
  normalized = `fc_${shortHash(itemId)}`
  return normalized.length > 64 ? normalized.slice(0, 64) : normalized

normalizeToolCallId(id, _targetModel, source):
  if (!allowedToolCallProviders.has(model.provider)) return normalizeIdPart(id)
  if (!id.includes("|")) return normalizeIdPart(id)
  [callId, itemId] = id.split("|")               // 只取前两段
  normalizedCallId = normalizeIdPart(callId)
  isForeignToolCall = source.provider !== model.provider || source.api !== model.api
  normalizedItemId = isForeignToolCall ? buildForeignResponsesItemId(itemId) : normalizeIdPart(itemId)
  if (!normalizedItemId.startsWith("fc_")) normalizedItemId = normalizeIdPart(`fc_${normalizedItemId}`)
  return `${normalizedCallId}|${normalizedItemId}`
```
- `id.split("|")` 在 JS 中会切出全部段；解构只取前两个，多余段被丢弃。
- `normalizeIdPart` 去尾下划线可能把 `fc_` 变 `fc`，此时再补 `fc_` → `fc_fc`（如实复刻）。
- 当 `_targetModel` 参数未使用时不要困惑：签名固定为三参。

```
transformedMessages = transformMessages(normalizedContext.messages, model, normalizeToolCallId)
toolSupport = getResponsesToolSupport(options ?? {})
transcriptTools = resolveTranscriptTools(normalizedContext.messages, toolSupport)
declaredAdditionalTools = false
unavailableTools = new Set<string>()
includeInitialSystemMessage = options?.includeSystemPrompt ?? true
compat = model.compat as { supportsDeveloperRole?: boolean } | undefined
instructionRole = (model.reasoning && compat?.supportsDeveloperRole !== false) ? "developer" : "system"
msgIndex = 0; sourceIndex = 0; systemIndex = 0
```

`appendSystemToolAdditions(message, seed)`：
```
tools = message.toolsAdded ?? []
for tool of tools: unavailableTools.delete(tool.name)
if tools.length === 0: return
if (toolSupport === "changes"):
    if (declaredAdditionalTools):
        messages.push({ role: instructionRole, content: renderIncrementalToolsNotice(unavailableTools) })
    declaredAdditionalTools = true
    messages.push({ type:"additional_tools", role:"developer", tools: convertResponsesTools(tools, options?.toolOptions) })
    return
if (toolSupport !== "additions"): return
names = tools.map(t => t.name)
callId = `kiana_tool_load_${shortHash(`${seed}:${names.join(",")}`)}`
messages.push({ type:"tool_search_call", call_id: callId, execution:"client", status:"completed",
                arguments: { query: names.join(" "), limit: names.length } })
messages.push({ type:"tool_search_output", call_id: callId, execution:"client", status:"completed",
                tools: convertResponsesTools(tools, { ...options?.toolOptions, toolSearchResult: true }) })
```
注意 `additional_tools` 的 `role` **恒为** `"developer"`（不随 `instructionRole`）。

`pushConfigurationUpdate(effort)`：
```
messages.push({ type:"configuration_update", reasoning: { effort } })
```

主循环：
```
for (msg of transformedMessages):
    isLeadingSystemMessage = (sourceIndex++ === 0 && msg.role === "system")
    if (msg.role === "system"):
        anchorsToolChanges = systemIndex++ >= transcriptTools.foldedSystemMessages
        if (anchorsToolChanges) appendSystemToolAdditions(msg, `system:${msgIndex}`)
        if (!isLeadingSystemMessage || includeInitialSystemMessage):
            parts = [isLeadingSystemMessage ? getSystemMessageText(msg) : renderSystemMessageUpdate(msg)]
            if (anchorsToolChanges):
                added = new Set((msg.toolsAdded ?? []).map(t => t.name))
                removed = (msg.toolsRemoved ?? []).map(t => t.name).filter(n => !added.has(n))
                if (removed.length > 0) parts.push(renderToolRemovalNotice(removed))
                for (name of removed) unavailableTools.add(name)
            text = parts.filter(p => p.length > 0).join("\n\n")
            if (text.length > 0) messages.push({ role: instructionRole, content: sanitizeSurrogates(text) })
    else if (user):
        string -> messages.push({ role:"user", content: [{ type:"input_text", text: sanitizeSurrogates(content) }] })
        array  -> 逐块 input_text / { type:"input_image", detail:"auto", image_url: data-url }
                  空数组 -> continue；否则 push { role:"user", content }
    else if (assistant):
        output = []
        isSameProviderAndApi = msg.provider === model.provider && msg.api === model.api
        isSameModel = isSameProviderAndApi && msg.model === model.id
        isDifferentModel = isSameProviderAndApi && msg.model !== model.id
        textBlockIndex = 0
        for block of msg.content:
            thinking: if (block.thinkingSignature): output.push(JSON.parse(block.thinkingSignature))   // 原样回放 reasoning item
            text:
                parsedSignature = parseTextSignature(block.textSignature)
                fallbackMessageId = textBlockIndex === 0 ? `msg_kiana_${msgIndex}` : `msg_kiana_${msgIndex}_${textBlockIndex}`
                textBlockIndex++
                msgId = parsedSignature?.id
                if (!msgId) msgId = fallbackMessageId
                else if (msgId.length > 64) msgId = `msg_${shortHash(msgId)}`
                output.push({ type:"message", role:"assistant",
                              content: [{ type:"output_text", text: sanitizeSurrogates(block.text), annotations: [] }],
                              status:"completed", id: msgId, phase: parsedSignature?.phase })
            toolCall:
                [callId, itemIdRaw] = block.id.split("|")
                customInputProperty = options?.grammarToolInputProperties?.get(block.name)
                itemId = itemIdRaw
                itemIdPrefix = customInputProperty === undefined ? "fc_" : "ctc_"
                if (isDifferentModel || !itemId?.startsWith(itemIdPrefix)) itemId = undefined
                if (customInputProperty !== undefined):
                    output.push({ type:"custom_tool_call", id: itemId, call_id: callId, name: block.name,
                                  input: sanitizeSurrogates(getGrammarToolInput(block.name, block.arguments, customInputProperty)),
                                  ...(isSameModel && block.namespace !== undefined ? { namespace: block.namespace } : {}) })
                else:
                    output.push({ type:"function_call", id: itemId, call_id: callId, name: block.name,
                                  arguments: JSON.stringify(block.arguments),
                                  ...(isSameModel && block.namespace !== undefined ? { namespace: block.namespace } : {}) })
        if (output.length === 0) continue
        if (options?.configurationUpdateEffort !== undefined && isSameModel && msg.providerThinkingLevel !== undefined):
            pushConfigurationUpdate(msg.providerThinkingLevel)
        messages.push(...output)
    else if (toolResult):
        [callId] = msg.toolCallId.split("|")
        output = convertToolResultOutput(model, msg.content)
        if (options?.grammarToolInputProperties?.has(msg.toolName)):
            messages.push({ type:"custom_tool_call_output", call_id: callId, output })
        else:
            messages.push({ type:"function_call_output", call_id: callId, output })
    if (!isLeadingSystemMessage) msgIndex++
if (options?.configurationUpdateEffort !== undefined): pushConfigurationUpdate(options.configurationUpdateEffort)
```
**关键细节**：
- `msgIndex` **不**为首条 system 递增（作为 `msg_kiana_<msgIndex>` 的稳定编号）。
- `thinking` 块的 `thinkingSignature` 直接 `JSON.parse` 后**原样** push（不校验）；
  若签名非法会抛错（进入调用方 error 事件）。
- 文本块 `phase` 可为 `undefined`（字段出现但为 undefined → JSON 中消失）。
- `function_call.arguments` 用 `JSON.stringify(block.arguments)`；`arguments` 为 `undefined` 时得到
  `undefined`（字段消失）。实际 toolCall 总有 `arguments`。

#### 2.12.7 `convertResponsesTools(tools, options?) -> OpenAITool[]`（导出）

```
defaultStrict = options?.strict === undefined ? false : options.strict
supportsStrictMode = options?.supportsStrictMode ?? true
supportsOpenAIGrammarTools = options?.supportsOpenAIGrammarTools ?? false

tools.map(tool => {
  grammar = resolveGrammarConstrainedSampling(tool, supportsOpenAIGrammarTools)
  if (grammar):
      return { type:"custom", name, description,
               format: { type:"grammar", syntax: grammar.format, definition: grammar.definition },
               ...(options?.toolSearchResult ? { defer_loading: true } : {}) }
  constrainedStrict = resolveJsonSchemaStrictSampling(tool, supportsStrictMode)
  strict = constrainedStrict ?? defaultStrict
  functionTool = { type:"function", name, description,
                   parameters: getJsonSchemaToolParameters(tool, strict === true),
                   ...(options?.toolSearchResult ? { defer_loading: true } : {}) }
  if (supportsStrictMode) functionTool.strict = strict
  return functionTool
})
```
- 与 completions 版差异：`strict` 允许 `null`（`strict: null` 时 `constrainedStrict ?? null` 可能为 null）。
- `strict` 字段仅当 `supportsStrictMode` 时出现（值可为 `true`/`false`/`null`）。

#### 2.12.8 `processResponsesStream(openaiStream, output, stream, model, options?) -> Promise<void>`（导出）

状态：
```
sawTerminalResponseEvent = false
outputSlots = Map<number, ResponsesOutputSlot>    // 键为 output_index
unfinishedToolCalls = Set<ToolCall>
reasoningBlocksById = Map<string, ThinkingContent>
```

槽位类型：
```ts
type ToolCallInput =
  | { kind:"json"; arguments: ToolCallArgumentsStream }
  | { kind:"custom"; property: string; buffer: GrammarToolInputJsonBuffer; unsent: string };
type ResponsesOutputSlot =
  | { type:"thinking"; block: ThinkingContent; contentIndex: number }
  | { type:"text"; block: TextContent; contentIndex: number }
  | { type:"toolCall"; block: ToolCall; contentIndex: number; input: ToolCallInput };
```

辅助：
- `applyMessagePhaseStopReason(item)`：`item.type === "message" && item.phase === "final_answer"` →
  `output.stopReason = "stop"`。
- `getSlot(outputIndex, type)`：取槽且类型匹配，否则 `undefined`。
- `pushToolCallDelta(slot, delta)`：`delta === undefined` 时不发；否则 push `toolcall_delta`。
- `createSlot(outputIndex, item)`：
  - `reasoning` → 新建 `{ type:"thinking", thinking:"" }` push 到 content，登记槽，push `thinking_start`
  - `message` → `applyMessagePhaseStopReason(item)`；新建 `{ type:"text", text:"" }`，push `text_start`
  - `function_call` → `args = new ToolCallArgumentsStream()`；块为
    `{ type:"toolCall", id: `${item.call_id}|${item.id}`, name: item.name,
       arguments: item.arguments ? args.append(item.arguments) : {},
       ...(item.namespace !== undefined ? { namespace: item.namespace } : {}) }`；
    加入 `unfinishedToolCalls`；槽 input `{ kind:"json", arguments: args }`；push `toolcall_start`
  - `custom_tool_call` → `inputProperty = options?.grammarToolInputProperties?.get(item.name) ?? "input"`；
    `input = item.input || ""`；块 `{ type:"toolCall", id: `${call_id}|${id}`, name,
            arguments: { [inputProperty]: input }, ...(namespace) }`；加入 unfinished；
    槽 input 为 `customInput(inputProperty, input)`；push `toolcall_start`
  - 其他 → `undefined`
- `getOrCreateSlot(outputIndex, item)`：先查槽，无则 `createSlot`。
- `backfillReasoningSignatures(responseOutput)`：对每个 `item.type === "reasoning" && item.encrypted_content`：
  找 `reasoningBlocksById.get(item.id)`；若块无 `thinkingSignature` → skip；
  `stored = JSON.parse(block.thinkingSignature)`；若 `stored.encrypted_content` 已存在 → skip；
  否则 `block.thinkingSignature = JSON.stringify({ ...stored, encrypted_content: item.encrypted_content })`。
- `finalizeResponse(response)`（`response.completed` 或 `response.incomplete`）：
  ```
  sawTerminalResponseEvent = true
  backfillReasoningSignatures(response.output ?? [])
  if (response?.id) output.responseId = response.id
  if (response?.usage):
      inputDetails = response.usage.input_tokens_details
      cachedTokens = inputDetails?.cached_tokens || 0
      cacheWriteTokens = inputDetails?.cache_write_tokens || 0
      output.usage = {
        input: Math.max(0, (response.usage.input_tokens || 0) - cachedTokens - cacheWriteTokens),
        output: response.usage.output_tokens || 0,
        cacheRead: cachedTokens,
        cacheWrite: cacheWriteTokens,
        reasoning: response.usage.output_tokens_details?.reasoning_tokens || 0,
        totalTokens: response.usage.total_tokens || 0,
        cost: {0...},
      }
  calculateCost(model, output.usage)
  if (options?.applyServiceTierPricing):
      serviceTier = options.resolveServiceTier
          ? options.resolveServiceTier(response?.service_tier, options.serviceTier)
          : (response?.service_tier ?? options.serviceTier)
      options.applyServiceTierPricing(output.usage, serviceTier)
  status = response?.status
  incompleteDetails = response?.incomplete_details
  incompleteReason = typeof incompleteDetails?.reason === "string" ? it : undefined
  output.rawStopReason = incompleteReason ? `${status}.${incompleteReason}` : status
  mapped = mapStopReason(status, incompleteReason)
  output.stopReason = mapped.stopReason
  if (mapped.errorMessage === undefined) delete output.errorMessage
  else output.errorMessage = mapped.errorMessage
  if (output.content.some(b => b.type === "toolCall") && output.stopReason === "stop")
      output.stopReason = "toolUse"
  ```
  - `input` 与 `totalTokens` 口径不同：`input` 扣缓存，`totalTokens` 直接用 provider 的。

事件循环（先 `await options?.onProviderStreamEvent?.(event, model)`）：
- `response.created` → `output.responseId = event.response.id`
- `response.output_item.added` → `createSlot(event.output_index, event.item)`
- `response.reasoning_summary_text.delta` → thinking 槽 `block.thinking += delta`，push `thinking_delta`
- `response.reasoning_summary_part.done` → thinking 槽 `+= "\n\n"`，push `thinking_delta`（delta 为 `"\n\n"`）
- `response.reasoning_text.delta` → 同上（`thinking_delta`）
- `response.output_text.delta` → text 槽 `block.text += delta`，push `text_delta`
- `response.refusal.delta` → text 槽 `block.text += delta`，push `text_delta`
- `response.function_call_arguments.delta` → toolCall 槽且 `input.kind === "json"`：
  `block.arguments = input.arguments.append(delta)`；`pushToolCallDelta(slot, delta)`
- `response.function_call_arguments.done` → `replaced = input.arguments.replace(event.arguments)`；
  `block.arguments = replaced.arguments`；`if (replaced.delta) pushToolCallDelta(slot, replaced.delta)`
- `response.custom_tool_call_input.delta` → `pushToolCallDelta(slot, appendCustomInput(block, input, event.delta))`
- `response.custom_tool_call_input.done` → `pushToolCallDelta(slot, closeCustomInput(block, input, event.input))`
- `response.output_item.done`：
  ```
  item = event.item; applyMessagePhaseStopReason(item); slot = getOrCreateSlot(event.output_index, item)
  reasoning && slot.type === "thinking":
      summaryText = item.summary?.map(s => s.text).join("\n\n") || ""
      contentText = item.content?.map(c => c.text).join("\n\n") || ""
      slot.block.thinking = summaryText || contentText || slot.block.thinking
      slot.block.thinkingSignature = JSON.stringify(item)
      reasoningBlocksById.set(item.id, slot.block)
      push thinking_end { contentIndex, content: slot.block.thinking, partial }
      outputSlots.delete(output_index)
  message && slot.type === "text":
      slot.block.text = item.content?.map(c => c.type === "output_text" ? c.text : c.refusal).join("") || ""
      slot.block.textSignature = encodeTextSignatureV1(item.id, item.phase ?? undefined)
      push text_end { contentIndex, content: slot.block.text, partial }
      outputSlots.delete(output_index)
  function_call && slot.type === "toolCall" && input.kind === "json":
      if (item.arguments) input.arguments.replace(item.arguments)
      Object.assign(slot.block, input.arguments.finish())
      if (item.namespace !== undefined) slot.block.namespace = item.namespace
      unfinishedToolCalls.delete(slot.block)
      push toolcall_end { contentIndex, toolCall: slot.block, partial }
      outputSlots.delete(output_index)
  custom_tool_call && slot.type === "toolCall" && input.kind === "custom":
      pushToolCallDelta(slot, closeCustomInput(slot.block, slot.input, item.input))
      if (item.namespace !== undefined) slot.block.namespace = item.namespace
      unfinishedToolCalls.delete(slot.block)
      push toolcall_end { contentIndex, toolCall: slot.block, partial }
      outputSlots.delete(output_index)
  ```
- `response.completed` / `response.incomplete` → `finalizeResponse(event.response)`
- `error` → `throw new Error(\`Error Code ${event.code}: ${event.message}\` || "Unknown error")`
  （`||` 永不生效，因为模板串至少是 `"Error Code undefined: undefined"`；**照抄**）
- `response.failed`：
  ```
  sawTerminalResponseEvent = true
  output.rawStopReason = event.response?.status
  error = event.response?.error; details = event.response?.incomplete_details
  msg = error ? `${error.code || "unknown"}: ${error.message || "no message"}`
      : details?.reason ? `incomplete: ${details.reason}`
      : "Unknown error (no error details in response)"
  throw new Error(msg)
  ```
循环结束：
```
if (!sawTerminalResponseEvent) throw new Error("OpenAI Responses stream ended before a terminal response event")
if (output.stopReason === "toolUse"):
    for (toolCall of unfinishedToolCalls):
        throw new Error(`OpenAI Responses stream completed with an unfinished tool call: ${toolCall.name} (${toolCall.id})`)
```

自定义输入辅助：
```ts
function customInput(property, initialInput) {
  const buffer = { input: "", started: false, closed: false };
  return { kind:"custom", property, buffer,
           unsent: appendGrammarToolInputJsonDelta(buffer, property, initialInput) ?? "" };
}
function sendCustomInput(block, input, jsonDelta) {
  block.arguments = { [input.property]: input.buffer.input };
  if (input.unsent === "") return jsonDelta;
  const sent = input.unsent + (jsonDelta ?? "");
  input.unsent = "";
  return sent;
}
```

`mapStopReason(status, incompleteReason?)`：
| status | 结果 |
|---|---|
| `undefined` | `{ stopReason: "stop" }` |
| `completed` | `{ stopReason: "stop" }` |
| `incomplete` + reason `max_output_tokens` | `{ stopReason: "length" }` |
| `incomplete` 其他 | `{ stopReason: "error", errorMessage: reason ? \`Response incomplete: ${reason}\` : "Response incomplete without a provider reason" }` |
| `failed` / `cancelled` | `{ stopReason: "error" }`（无 errorMessage） |
| `in_progress` / `queued` | `{ stopReason: "stop" }` |
| 其他 | 抛 `Unhandled stop reason: ${status}` |

### 2.13 `azure-openai-responses.ts`

文件 255 行。

常量：
```ts
const AZURE_TOOL_CALL_PROVIDERS = new Set(["openai", "openai-codex", "opencode", "azure"]);
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS = 16;
```

`formatAzureOpenAIError(error)`：`formatProviderError(normalizeProviderError(error), "Azure OpenAI API error")`。

`AzureOpenAIResponsesOptions`（导出）：
```ts
export interface AzureOpenAIResponsesOptions extends AzureEndpointOptions {
  reasoningEffort?: "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
  toolChoice?: ResponseCreateParamsStreaming["tool_choice"];
  reasoningSummary?: "auto" | "detailed" | "concise" | null;
}
```

#### 2.13.1 `stream`（导出）

1. 同步：`normalizedContext = resolveTranscript(context, model.compat?.supportsMidConvoSystemMessages)`。
2. `deploymentName = resolveDeploymentName(model, options)`：位于 async IIFE 内、`output` 构造**之前**、
   `try` **之外**。`resolveDeploymentName` 本身不会抛错（只读 `options.azureDeploymentName` 与 env），
   故无实际风险；Rust 侧保持同样位置即可（不得为它新增错误分支）。
   真正的端点解析错误来自 `createClient → resolveAzureConfig → resolveAzureBaseUrl`，它在 `try` 内，
   因此以 **error 事件**结束流。
   → 另见 `providers/azure.ts` 的 `azureStreams()`：它把 `resolveAzureModel`（baseUrl 归一化）与
   `withDeploymentName`（注入覆盖 `model` 的 `onPayload`）包在 `lazyStream` 里，因此 **provider 路径**下
   "端点未配置" 也变成流上的 error 事件，而不是从 `stream()` 抛出。验证用例：
   `azure-openai-completions.test.ts` 的
   "surfaces an unconfigured endpoint as an error event rather than throwing out of stream()"。
3. `output`：`api: "azure-openai-responses" as Api`（**字面量**，非 `model.api`）。
4. `try`：
   - `apiKey = options?.apiKey`；`if (!apiKey) throw new Error(\`No API key for provider: ${model.provider}\`)`
   - `client = createClient(model, apiKey, options)`
   - `grammarToolInputProperties = createGrammarToolInputProperties(getDeclaredTools(normalizedContext.messages),
       model.compat?.supportsOpenAIGrammarTools ?? false)`
   - `params = buildParams(model, normalizedContext, options, deploymentName, grammarToolInputProperties)`
   - `onPayload` 替换；`requestOptions`；`retryProviderRequest(() => client.responses.create(params, requestOptions).withResponse(), ...)`
   - `onResponse` → `start` → `processResponsesStream(..., { onProviderStreamEvent, grammarToolInputProperties })`
     （**无** serviceTier / applyServiceTierPricing）
   - 收尾：`signal.aborted` → `"Request was aborted"`；`pending` → `"Azure OpenAI Responses stream ended without a stop reason"`；
     `aborted`/`error` → `output.errorMessage || "An unknown error occurred"`
5. catch：`stopReason = aborted ? "aborted" : "error"`；`errorMessage = formatAzureOpenAIError(error)`。

#### 2.13.2 `streamSimple`（导出）

```ts
apiKey = options?.apiKey; if (!apiKey) throw new Error(`No API key for provider: ${model.provider}`);
base = { ...buildBaseOptions(model, context, options, apiKey), toolChoice: options?.toolChoice };
clampedReasoning = options?.reasoning ? clampThinkingLevel(model, options.reasoning) : undefined;
reasoningEffort = clampedReasoning === "off" ? undefined : clampedReasoning;
return stream(model, context, { ...base, reasoningEffort });
```

#### 2.13.3 `createClient` / `buildParams`（内部）

`createClient`：
```ts
headers = { ...getIdentityHeaders(model, options?.identity), ...model.headers }
if (options?.headers) Object.assign(headers, options.headers)
{ baseUrl, apiVersion } = resolveAzureConfig(model, options)
return new AzureOpenAI({ apiKey, apiVersion, dangerouslyAllowBrowser: true, fetch: options?.fetch,
                         defaultHeaders: headers, baseURL: baseUrl })
```

`buildParams`：
```
supportsAdditionalTools = model.compat?.supportsAdditionalTools ?? false
supportsToolSearch = model.compat?.supportsToolSearch ?? false
transcriptTools = resolveTranscriptTools(context.messages, getResponsesToolSupport({
    supportsMidConvoSystemMessages: model.compat?.supportsMidConvoSystemMessages ?? false,
    supportsAdditionalTools, supportsToolSearch }))
messages = convertResponsesMessages(model, context, AZURE_TOOL_CALL_PROVIDERS, {
    grammarToolInputProperties,
    supportsMidConvoSystemMessages: model.compat?.supportsMidConvoSystemMessages ?? false,
    supportsAdditionalTools, supportsToolSearch,
    toolOptions: { supportsStrictMode: model.compat?.supportsStrictMode ?? true,
                   supportsOpenAIGrammarTools: model.compat?.supportsOpenAIGrammarTools ?? false },
})
params = { model: deploymentName, input: messages, stream: true,
           prompt_cache_key: clampOpenAIPromptCacheKey(options?.sessionId), store: false }
if (options?.maxTokens) params.max_output_tokens = Math.max(options.maxTokens, 16)
if (options?.temperature !== undefined) params.temperature = options.temperature
if (transcriptTools.requestTools.length > 0)
    params.tools = convertResponsesTools(transcriptTools.requestTools, { supportsStrictMode: ..., supportsOpenAIGrammarTools: ... })
if (options?.toolChoice !== undefined) params.tool_choice = options.toolChoice
reasoningEffort = options?.reasoningEffort ?? (options?.reasoningSummary ? "medium" : undefined)
if (model.reasoning):
    if reasoningEffort:
        effort = options?.reasoningEffort ? (model.thinkingLevelMap?.[options.reasoningEffort] ?? options.reasoningEffort) : reasoningEffort
        params.reasoning = { effort, summary: options?.reasoningSummary || "auto" }
        params.include = ["reasoning.encrypted_content"]
    else if (model.thinkingLevelMap?.off !== null):
        params.reasoning = { effort: model.thinkingLevelMap?.off ?? "none" }
samplingParams = resolveSamplingParams(model, reasoningEffort ?? "off", options?.samplingParams)
if (samplingParams) Object.assign(params, samplingParams)
```
**差异 vs openai-responses**：
- `prompt_cache_key` **总是** `clampOpenAIPromptCacheKey(options?.sessionId)`（**无** `cacheRetention` 判断，
  也**无** `prompt_cache_retention` / `prompt_cache_options`）。
- `model` 字段用 **deploymentName**，但 `output.model` 记 `model.id`。
- `supportsStrictMode` 默认 **true**（responses 为 false）。

#### 2.13.4 Azure 上走 Chat Completions 的模型

`providers/azure.ts` 把 `api === "openai-completions"` 的 Azure 模型路由到 `openAICompletionsApi()`
（即 2.10 的模块），并在 provider 层把 `baseUrl` 归一化为 `resolveAzureBaseUrl(model, options)`。
因此 `azure-openai-completions.test.ts` 的断言对象是 **completions**，不是本文件：
- `reasoning_effort: "high"`（`reasoning: "max"` 被 clamp 到 high）
- 不写 `thinking`（deepseek 的 compat 覆盖）
- `KIANA_CACHE_RETENTION=long` 时**不写** `prompt_cache_key` / `prompt_cache_retention`
- 系统提示以 `role: "system"` 发送（deepseek 的 `supportsDeveloperRole=false`）
- 中途 system 消息**保留原位**（`supportsMidConvoSystemMessages=true`）
- assistant 重放时 `reasoning_content` 保留
- 客户端 `baseURL` = `https://my-resource.services.ai.azure.com/openai/v1`

### 2.14 `openai-codex-responses.ts`

文件 1719 行。除了 Responses 协议转换，还自建 **SSE + WebSocket 双传输**、zstd 请求压缩、
JWT 账号提取、会话级 WebSocket 连接缓存与 debug 统计。

#### 2.14.1 常量

```ts
const DEFAULT_CODEX_BASE_URL = "https://chatgpt.com/backend-api";
const JWT_CLAIM_PATH = "https://api.openai.com/auth";
const DEFAULT_MAX_RETRIES = 0;
const BASE_DELAY_MS = 1000;
const DEFAULT_MAX_RETRY_DELAY_MS = 60_000;
const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS = 15_000;
const REQUEST_COMPRESSION_ZSTD_LEVEL = 3;
const CODEX_TOOL_CALL_PROVIDERS = new Set(["openai", "openai-codex", "opencode"]);
const WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE = 1009;
const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE = "websocket_connection_limit_reached";
const PREVIOUS_RESPONSE_NOT_FOUND_CODE = "previous_response_not_found";
const OPENAI_BETA_RESPONSES_WEBSOCKETS = "responses_websockets=2026-02-06";
const SESSION_WEBSOCKET_CACHE_TTL_MS = 5 * 60 * 1000;     // 300000
const SESSION_WEBSOCKET_MAX_AGE_MS = 55 * 60 * 1000;      // 3300000
const CODEX_RESPONSE_STATUSES = new Set(["completed","incomplete","failed","cancelled","queued","in_progress"]);
```

`OpenAICodexResponsesOptions`（导出）：
```ts
export interface OpenAICodexResponsesOptions extends StreamOptions {
  reasoningEffort?: "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
  reasoningSummary?: "auto" | "concise" | "detailed" | "off" | "on" | null;
  serviceTier?: ResponseCreateParamsStreaming["service_tier"];
  textVerbosity?: "low" | "medium" | "high";          // 默认 "low"
  toolChoice?: "auto" | "none" | "required";          // 默认 "auto"
}
```

#### 2.14.2 `assertSuccessfulOutput(output)`（内部）

```
if (output.stopReason === "pending") throw new Error("Codex stream ended without a stop reason")
if (output.stopReason === "error" || "aborted") throw new Error(output.errorMessage || "An unknown error occurred")
```

#### 2.14.3 重试判定（内部）

`isTerminalRateLimitError(text)`（**不区分大小写**的正则）：
```
/GoUsageLimitError|FreeUsageLimitError|Monthly usage limit reached|available balance|insufficient_quota|out of budget|quota exceeded|billing/i
```
`isRetryableError(status, errorText)`：
```
if (status === 429 && isTerminalRateLimitError(errorText)) return false
if (status ∈ {429,500,502,503,504}) return true
return /rate.?limit|overloaded|service.?unavailable|upstream.?connect|connection.?refused/i.test(errorText)
```
`getRetryAfterDelayMs(headers)`：
```
retry-after-ms 存在 → Number(...) 有限则 max(0, v)
retry-after 存在 → Number(...) 有限则 max(0, v*1000)；否则 Date.parse(...) 有效则 max(0, t - Date.now())
否则 undefined
```
`RetryDelayExceededError`（内部类）。`validateRetryDelayMs(delayMs, options)`：
```
max = options?.maxRetryDelayMs ?? 60000
if (max > 0 && delayMs > max) throw new RetryDelayExceededError(
    `Server requested ${Math.ceil(delayMs/1000)}s retry delay (max: ${Math.ceil(max/1000)}s)`)
return delayMs
```
`sleep(ms, signal)`：`signal.aborted` → reject `Error("Request was aborted")`；否则 `setTimeout`，
abort 时 `clearTimeout` 并 reject 同一条消息。
`normalizeTimeoutMs(value)`：`undefined` → `undefined`；非有限或 `< 0` →
`throw new Error(\`Invalid timeoutMs: ${String(value)}\`)`；否则 `Math.floor`。

#### 2.14.4 zstd 压缩（内部）

`loadNodeZlib()`：仅当 `process` 存在且 `process.versions.node || process.versions.bun` 时，
尝试 `process.getBuiltinModule?.("node:zlib")`，否则 `null`。

`compressRequestBodyZstd(bodyJson)`：无 zlib 或无 `zstdCompressSync` → `null`；
否则 `zstdCompressSync(bodyJson, { params: { [ZSTD_c_compressionLevel]: 3 } })`，
包成 `Uint8Array`（复制到普通 ArrayBuffer）；异常 → `null`（**静默回退未压缩**）。

Rust 侧：`zstd::stream::encode_all(body.as_bytes(), 3)`；失败也回退未压缩（日志即可）。

#### 2.14.5 `stream`（导出）

同步：`normalizedContext = resolveTranscript(context, model.compat?.supportsMidConvoSystemMessages)`。

异步：
1. `output`：`api: "openai-codex-responses" as Api`、`provider: model.provider`、`model: model.id`。
2. `apiKey = options?.apiKey`；缺失 → `throw new Error(\`No API key for provider: ${model.provider}\`)`。
3. `accountId = extractAccountId(apiKey)`。
4. `grammarToolInputProperties = createGrammarToolInputProperties(getDeclaredTools(...),
    model.compat?.supportsOpenAIGrammarTools ?? false)`。
5. `cacheSessionId = options?.cacheRetention === "none" ? undefined : options?.sessionId`；
   `codexSessionId = clampOpenAIPromptCacheKey(cacheSessionId)`。
6. `body = buildRequestBody(model, normalizedContext, options, codexSessionId, grammarToolInputProperties)`；
   `onPayload` 可整体替换。
7. `websocketRequestId = codexSessionId || uuidv7()`。
8. `identityHeaders = { originator: "kiana", ...getIdentityHeaders(model, options?.identity) }`
   （`originator` 是 Codex 协议要求，**不是** attribution。`getIdentityHeaders` 只返回
   `User-Agent` + attribution 头（`HTTP-Referer`/`X-OpenRouter-Title`/`X-OpenRouter-Categories`/
   `X-BILLING-INVOKE-ORIGIN`/`x-opencode-client`），**不含** `originator`，所以展开后
   `originator: "kiana"` **保留**——除非 `model.headers` 或 `options.headers` 显式覆盖它
   （`openai-codex-stream.test.ts` 的 "lets model and caller headers override originator and User-Agent"
   用 `model.headers: { originator: "my-app" }` 断言最终头为 `my-app`）。
   Rust 侧：先放 `originator: "kiana"`，再按 identity → model.headers → options.headers 顺序覆盖。）
9. `sseHeaders = buildSSEHeaders(identityHeaders, model.headers, options?.headers, accountId, apiKey, codexSessionId)`；
   `websocketHeaders = buildWebSocketHeaders(..., websocketRequestId)`。
10. `bodyJson = JSON.stringify(body)`；`httpTimeoutMs = normalizeTimeoutMs(options?.timeoutMs)`；
    `websocketConnectTimeoutMs = normalizeTimeoutMs(options?.websocketConnectTimeoutMs)`；
    `transport = options?.transport || "auto"`。
11. `startEmitted = false`。
12. `websocketDisabledForSession = transport !== "sse" && isWebSocketSseFallbackActive(cacheSessionId)`；
    为真则 `recordWebSocketSseFallback(cacheSessionId)`。
13. **WebSocket 分支**（`transport !== "sse" && !websocketDisabledForSession`）：
    ```
    websocketStarted = false
    retriedWebSocketConnectionLimit = false
    retriedMissingWebSocketContinuation = false
    while (true):
        websocketStarted = false
        try:
            await processWebSocketStream(resolveCodexWebSocketUrl(model.baseUrl), body, websocketHeaders,
                output, stream, model,
                () => { websocketStarted = true; if (!startEmitted) { startEmitted = true; push start } },
                httpTimeoutMs, websocketConnectTimeoutMs, cacheSessionId, accountId,
                grammarToolInputProperties, options)
            if signal.aborted: throw new Error("Request was aborted")
            assertSuccessfulOutput(output)
            push done { reason: output.stopReason, message: output }; end(); return
        catch error:
            aborted = signal?.aborted
            connectionLimitBeforeStart = !websocketStarted && isWebSocketConnectionLimitReachedError(error)
            previousResponseNotFound = isPreviousResponseNotFoundError(error)
            if (!aborted && previousResponseNotFound && !retriedMissingWebSocketContinuation):
                retriedMissingWebSocketContinuation = true; continue
            if (!aborted && connectionLimitBeforeStart && !retriedWebSocketConnectionLimit):
                retriedWebSocketConnectionLimit = true; continue
            if (aborted || (isCodexNonTransportError(error) && !connectionLimitBeforeStart)): throw error
            appendAssistantMessageDiagnostic(output, createAssistantMessageDiagnostic(
                "provider_transport_failure", error, {
                  configuredTransport: transport,
                  ...(websocketStarted ? {} : { fallbackTransport: "sse" }),
                  eventsEmitted: websocketStarted,
                  phase: websocketStarted ? "after_message_stream_start" : "before_message_stream_start",
                  requestBytes: new TextEncoder().encode(bodyJson).byteLength,
                }))
            recordWebSocketFailure(cacheSessionId, error)
            if (websocketStarted): throw error
            recordWebSocketSseFallback(cacheSessionId)
            break
    ```
14. **SSE 分支**：
    ```
    compressedBody = compressRequestBodyZstd(bodyJson)
    if (compressedBody) sseHeaders.set("content-encoding", "zstd")
    sseBody = compressedBody ?? bodyJson
    maxRetries = options?.maxRetries ?? 0
    for (attempt = 0; attempt <= maxRetries; attempt++):
        if signal.aborted: throw new Error("Request was aborted")
        try:
            headerTimeoutSignal = (httpTimeoutMs !== undefined && httpTimeoutMs > 0) ? AbortSignal.timeout(httpTimeoutMs) : undefined
            combinedSignal = combineAbortSignals([options?.signal, headerTimeoutSignal])
            try:
                response = await (options?.fetch ?? globalThis.fetch)(resolveCodexUrl(model.baseUrl),
                    { method:"POST", headers: sseHeaders, body: sseBody, signal: combinedSignal.signal })
            catch error:
                if (headerTimeoutSignal?.aborted && !options?.signal?.aborted):
                    throw new Error(`Codex SSE response headers timed out after ${httpTimeoutMs}ms`)
                throw error
            finally: combinedSignal.cleanup()
            await options?.onResponse?.({ status: response.status, headers: headersToRecord(response.headers) }, model)
            if (response.ok) break
            errorText = await response.text()
            if (attempt < maxRetries && isRetryableError(response.status, errorText)):
                retryAfterDelayMs = getRetryAfterDelayMs(response.headers)
                delayMs = retryAfterDelayMs === undefined ? BASE_DELAY_MS * 2 ** attempt
                                                          : validateRetryDelayMs(retryAfterDelayMs, options)
                await sleep(delayMs, options?.signal); continue
            fakeResponse = new Response(errorText, { status, statusText })
            info = await parseErrorResponse(fakeResponse)
            throw new Error(info.friendlyMessage || info.message)
        catch error:
            if (error instanceof Error && (error.name === "AbortError" || error.message === "Request was aborted")):
                throw new Error("Request was aborted")
            lastError = error instanceof Error ? error : new Error(String(error))
            if (attempt < maxRetries && !(lastError instanceof RetryDelayExceededError)
                && !lastError.message.includes("usage limit")):
                await sleep(BASE_DELAY_MS * 2 ** attempt, options?.signal); continue
            throw lastError
    if (!response?.ok) throw lastError ?? new Error("Failed after retries")
    if (!response.body) throw new Error("No response body")
    if (!startEmitted) { startEmitted = true; push start }
    await processStream(response, output, stream, model, grammarToolInputProperties, options)
    if signal.aborted: throw new Error("Request was aborted")
    assertSuccessfulOutput(output)
    push done; end
    ```
15. catch（外层）：`stopReason = aborted ? "aborted" : "error"`；
    `errorMessage = formatProviderError(normalizeProviderError(error))`（**无前缀**）。

#### 2.14.6 `streamSimple`（导出）

```ts
if (!options?.apiKey) throw new Error(`No API key for provider: ${model.provider}`);
base = { ...buildBaseOptions(model, context, options, apiKey), toolChoice: options?.toolChoice };
clampedReasoning = options?.reasoning ? clampThinkingLevel(model, options.reasoning) : undefined;
reasoningEffort = clampedReasoning === "off" ? undefined : clampedReasoning;
return stream(model, context, { ...base, reasoningEffort });
```
- 测试断言：`gpt-5.3-codex` / `gpt-5.4` / `gpt-5.5` 的 `minimal` 被 clamp 到 `low`（由模型目录的
  `thinkingLevelMap` 决定，非本模块逻辑）；`gpt-5.5` 的 `xhigh` 保留。

#### 2.14.7 `buildRequestBody(...)`（内部）

```
supportsStrictMode = model.compat?.supportsStrictMode ?? true
supportsOpenAIGrammarTools = model.compat?.supportsOpenAIGrammarTools ?? false
supportsAdditionalTools = model.compat?.supportsAdditionalTools ?? false
supportsToolSearch = model.compat?.supportsToolSearch ?? false
transcriptTools = resolveTranscriptTools(context.messages, getResponsesToolSupport({
    supportsMidConvoSystemMessages: model.compat?.supportsMidConvoSystemMessages ?? false,
    supportsAdditionalTools, supportsToolSearch }))
messages = convertResponsesMessages(model, context, CODEX_TOOL_CALL_PROVIDERS, {
    includeSystemPrompt: false, grammarToolInputProperties,
    supportsMidConvoSystemMessages: model.compat?.supportsMidConvoSystemMessages ?? false,
    supportsAdditionalTools, supportsToolSearch,
    toolOptions: { strict: null, supportsStrictMode, supportsOpenAIGrammarTools },
})
initialSystemMessage = getInitialSystemMessage(context.messages)
instructions = initialSystemMessage ? getSystemMessageText(initialSystemMessage) : ""
body = {
  model: model.id,
  store: false,
  stream: true,
  instructions: instructions || "You are a helpful assistant.",
  input: messages,
  text: { verbosity: options?.textVerbosity || "low" },
  include: ["reasoning.encrypted_content"],
  prompt_cache_key: cacheSessionId,
  tool_choice: options?.toolChoice ?? "auto",
  parallel_tool_calls: true,
}
if (options?.temperature !== undefined) body.temperature = options.temperature
if (options?.serviceTier !== undefined) body.service_tier = options.serviceTier
if (transcriptTools.requestTools.length > 0):
    body.tools = convertResponsesTools(transcriptTools.requestTools,
        { strict: null, supportsStrictMode, supportsOpenAIGrammarTools })
if (options?.reasoningEffort !== undefined):
    effort = options.reasoningEffort === "none"
        ? (model.thinkingLevelMap?.off === undefined ? "none" : model.thinkingLevelMap.off)
        : (model.thinkingLevelMap?.[options.reasoningEffort] ?? options.reasoningEffort)
    if (effort !== null) body.reasoning = { effort, summary: options.reasoningSummary ?? "auto" }
else if (model.reasoning && model.thinkingLevelMap?.off !== null):
    body.reasoning = { effort: model.thinkingLevelMap?.off ?? "none" }
```
**要点**：
- `instructions` **不**来自 `convertResponsesMessages`（`includeSystemPrompt: false`），而是单独取首条
  system 消息；为空则用逐字 `"You are a helpful assistant."`。
- `prompt_cache_key: cacheSessionId` 即使为 `undefined` 字段也会在 `JSON.stringify` 时消失。
- `tool_choice` 恒有值（默认 `"auto"`）；`parallel_tool_calls: true` 恒发。
- `strict: null` 使 `convertResponsesTools` 的 `defaultStrict` 为 `null`（即非 strict，且 `supportsStrictMode`
  时 `strict` 字段值为 `null`）。

#### 2.14.8 URL 解析（内部）

```ts
function resolveCodexUrl(baseUrl?) {
  const raw = baseUrl && baseUrl.trim().length > 0 ? baseUrl : "https://chatgpt.com/backend-api";
  const normalized = raw.replace(/\/+$/, "");
  if (normalized.endsWith("/codex/responses")) return normalized;
  if (normalized.endsWith("/codex")) return `${normalized}/responses`;
  return `${normalized}/codex/responses`;
}
function resolveCodexWebSocketUrl(baseUrl?) {
  const url = new URL(resolveCodexUrl(baseUrl));
  if (url.protocol === "https:") url.protocol = "wss:";
  if (url.protocol === "http:") url.protocol = "ws:";
  return url.toString();
}
```

#### 2.14.9 SSE 与 WS 解析（内部）

`parseSSE(response, signal)`（async generator）：
- 无 `response.body` → 直接 `return`（**不抛**）。
- 读流，按 `"\n\n"` 切帧；**EOF 时若残留 buffer 非空则补 `"\n\n"`**（"EOF 终止残帧"）。
- 每帧：`chunk.split("\n").filter(l => l.startsWith("data:")).map(l => l.slice(5).trim())`；
  非空则 `data = lines.join("\n").trim()`；`data && data !== "[DONE]"` → `JSON.parse`；
  失败 → `throw new CodexProtocolError(\`Invalid Codex SSE JSON: ${formatThrownValue(cause)}\`, { cause, payload: data })`
- abort 监听 `reader.cancel()`；`finally` 里 `removeEventListener`、`reader.cancel()`、`releaseLock()`（都吞异常）。

`parseWebSocket(socket, signal, idleTimeoutMs?)`（async generator）：
- 队列 + `pending`/`done`/`failed`/`sawCompletion`。
- `onMessage`：解码数据（string / ArrayBuffer / TypedArray / Blob-like），`JSON.parse`；
  `type ∈ {response.completed, response.done, response.incomplete}` → `sawCompletion = true; done = true`；
  push 队列并唤醒。解析失败 → `failed = CodexProtocolError(\`Invalid Codex WebSocket JSON: ...\`, { cause, payload: text })`。
- `onError` → `failed = extractWebSocketError(event)`。
- `onClose` → `sawCompletion` 时正常结束；否则 `failed ??= extractWebSocketCloseError(event)`。
- `onAbort` → `failed = Error("Request was aborted")`。
- 消费循环：`signal.aborted` → throw；队列非空 → yield；`done` → break；否则等待，
  有 `idleTimeoutMs > 0` 时超时 → `Error(\`WebSocket idle timeout after ${idleTimeoutMs}ms\`)`，
  关闭 socket（code 1000, reason `"idle_timeout"`）并 reject。
- 结束后：`failed` → throw；`!sawCompletion` → `throw new Error("WebSocket stream closed before response.completed")`。

`decodeWebSocketData(data)`：string 原样；`ArrayBuffer` → 解码；`ArrayBuffer.isView` → 按视图解码；
带 `arrayBuffer()` 的 Blob-like → await 后解码；其他 → `null`。

`extractWebSocketError(event)`：
- 事件对象有非空字符串 `message` → `Error(message)`
- `error` 是 Error 且消息非空 → 该 Error
- `error` 是对象且有非空字符串 `message` → `Error(message)`
- 否则 `Error("WebSocket error")`

`extractWebSocketCloseError(event)`：
```
codeText = typeof code === "number" ? ` ${code}` : ""
reasonText = 非空字符串 reason ? ` ${reason}` : ""
if (!reasonText && code === 1009) reasonText = " message too big"
return new WebSocketCloseError(`WebSocket closed${codeText}${reasonText}`.trim(),
  { code: number|undefined, reason: string|undefined, wasClean: boolean|undefined })
// 非对象事件 -> Error("WebSocket closed")
```

`mapCodexEvents(events, output, model, onProviderStreamEvent)`（async generator）：
```
for await (event of events):
    try { await onProviderStreamEvent?.(event, model) }
    catch (error) { throw new ProviderStreamEventCallbackError(error) }   // 不触发 WS 重试/SSE 回退
    type = typeof event.type === "string" ? event.type : undefined
    if (!type) continue
    if (type === "error"):
        { code, message } = extractCodexEventError(event)
        throw new CodexApiError(`Codex error: ${message || code || JSON.stringify(event)}`, { code, payload: event })
    if (type === "response.failed"):
        response = event.response
        throw new CodexApiError(response?.error?.message || "Codex response failed",
                                { code: response?.error?.code, payload: event })
    if (type ∈ {"response.done","response.completed","response.incomplete"}):
        response = event.response
        if (typeof response?.end_turn === "boolean") output.endTurn = response.end_turn
        normalizedResponse = response ? { ...response, status: normalizeCodexStatus(response.status) } : response
        yield { ...event, type: "response.completed", response: normalizedResponse }
        return
    yield event
```
`extractCodexEventError(event)`：`code` 优先取 `event.code`（string），否则 `event.error.code`；
`message` 取 `event.message` 或 `event.error.message`（string）。
`normalizeCodexStatus(status)`：string 且在 `CODEX_RESPONSE_STATUSES` 中 → 自身，否则 `undefined`。

#### 2.14.10 WebSocket 连接缓存（内部 + 导出 API）

类型：
```ts
interface CachedWebSocketContinuationState {
  lastRequestBody: RequestBody;
  lastResponseId: string;
  lastResponseItems: ResponseInput;
}
interface CachedWebSocketConnection {
  socket: WebSocketLike;
  busy: boolean;
  createdAt: number;
  idleTimer?: ReturnType<typeof setTimeout>;
  continuation?: CachedWebSocketContinuationState;
}
export interface OpenAICodexWebSocketDebugStats {
  requests: number;
  connectionsCreated: number;
  connectionsReused: number;
  cachedContextRequests: number;
  storeTrueRequests: number;
  fullContextRequests: number;
  deltaRequests: number;
  lastInputItems: number;
  lastDeltaInputItems?: number;
  lastPreviousResponseId?: string;
  websocketFailures: number;
  sseFallbacks: number;
  websocketFallbackActive?: boolean;
  lastWebSocketError?: string;
}
```
模块级状态：
```
websocketSessionCache: Map<sessionId, Map<accountId, CachedWebSocketConnection>>
websocketDebugStats: Map<sessionId, OpenAICodexWebSocketDebugStats>
websocketSseFallbackSessions: Set<sessionId>
```

导出函数：
- `getOpenAICodexWebSocketDebugStats(sessionId)`：返回 `{ ...stats }` 或 `undefined`（浅拷贝，调用方可改不影响内部）。
- `resetOpenAICodexWebSocketDebugStats(sessionId?)`：有参 → 删该 session 的 stats 与 fallback 标记；
  无参 → 清空两者（**不清连接缓存**）。

内部统计写入点（**导出 API 的语义依赖它们，Rust 侧必须同名同义**）：
```
getOrCreateWebSocketDebugStats(sessionId):
    取或创建并注册一个全零 stats（requests/connectionsCreated/connectionsReused/cachedContextRequests/
    storeTrueRequests/fullContextRequests/deltaRequests/lastInputItems/websocketFailures/sseFallbacks 全 0，
    lastDeltaInputItems/lastPreviousResponseId/websocketFallbackActive/lastWebSocketError 为 undefined）

isWebSocketSseFallbackActive(sessionId) -> boolean:
    sessionId ? websocketSseFallbackSessions.has(sessionId) : false

recordWebSocketSseFallback(sessionId):
    if (!sessionId) return                       // 无 session 不记账
    stats = getOrCreateWebSocketDebugStats(sessionId)
    stats.sseFallbacks++
    stats.websocketFallbackActive = isWebSocketSseFallbackActive(sessionId)

recordWebSocketFailure(sessionId, error):
    if (!sessionId) return
    websocketSseFallbackSessions.add(sessionId)  // ← 注意：失败即把 session 标记为"粘滞 SSE"，
    stats = getOrCreateWebSocketDebugStats(sessionId)   //    即使该次仍在流中途抛错（websocketStarted）也一样
    stats.websocketFailures++
    stats.lastWebSocketError = formatThrownValue(error)
    stats.websocketFallbackActive = true
```
**粘滞语义（易漏）**：`websocketSseFallbackSessions` 一旦被 `recordWebSocketFailure` 加入，
该 session 上的**后续所有请求**都会在第 12 步走 `websocketDisabledForSession = true` 分支——
直接跳过 WebSocket、只走 SSE，并且每次都会再 `recordWebSocketSseFallback` 让 `sseFallbacks++`
（`websocketFallbackActive` 保持 true）。只有 `resetOpenAICodexWebSocketDebugStats(sessionId)`
或进程重启才会清除。`websocketStarted` 后抛错的那条路径虽然**不**回退到 SSE，但它**已经**把 session
标成粘滞——下一次请求将不再尝试 WS。Rust 侧不要把它实现成"仅本次请求有效"的局部标志。
- `closeOpenAICodexWebSocketSessions(sessionId?)`：关闭并清理连接（清 idleTimer，`close(socket, 1000, "debug_close")`）；
  无参清全部。
- 模块加载时 `registerSessionResourceCleanup(closeOpenAICodexWebSocketSessions)`。

内部判定辅助（逐字语义，Rust 侧应各成一个 `pub(crate) fn`）：
```ts
getWebSocketReadyState(socket):                 // (socket as {readyState?}).readyState
    typeof readyState === "number" ? readyState : undefined
isWebSocketReusable(socket):
    readyState === undefined || readyState === 1   // 运行时不给 readyState 时**假定可复用**
isWebSocketSessionExpired(entry):
    Date.now() - entry.createdAt >= 55 * 60 * 1000
closeWebSocketSilently(socket, code = 1000, reason = "done"):
    try { socket.close(code, reason) } catch {}    // 默认 reason 是 "done"（非空串）
```
`closeWebSocketSilently` 的 reason 分场景取值（Rust 侧若做快照/日志要对齐）：
`"debug_close"`（`closeOpenAICodexWebSocketSessions`）、`"idle_timeout"`（`scheduleSessionWebSocketExpiry`
与 `parseWebSocket` 的 idle 超时）、`"connection_age_limit"`（`acquireWebSocket` 过期条目）、
`"aborted"` / `"connect_timeout"`（`connectWebSocket` 的 abort/超时），以及**省略**（默认 `"done"`）：
一次性连接的 `release()`、不可复用条目的关闭、`release({keep:false})`。

`acquireWebSocket(url, headers, sessionId, accountId, signal?, connectTimeoutMs?, env?)`：
- 无 `sessionId` → 新建连接，`reused: false`，`release` 直接关闭。
- 有 `sessionId`：查 `accountEntries.get(accountId)`：
  - 清 idleTimer
  - `!busy && 超过 SESSION_WEBSOCKET_MAX_AGE_MS` → 关闭（reason `"connection_age_limit"`）并移除条目
  - `!busy && 可复用（readyState === 1 或不可知）` → 标记 busy，返回 `reused: true`；
    `release({keep})`：不 keep 或不可复用 → 关闭并清缓存；否则 `busy = false` 并
    `scheduleSessionWebSocketExpiry`（300000ms 后若仍不 busy 则关闭，reason `"idle_timeout"`）
  - `busy` → 新建**临时**连接（不进缓存），release 直接关闭
  - 不可复用 → 关闭并移除条目
- 新建连接后写入缓存，`busy: true`，`createdAt: Date.now()`。

`connectWebSocket(url, headers, signal?, connectTimeoutMs = 15000, env?)`：
- `getWebSocketConstructor(env)`；无 → `throw new Error("WebSocket transport is not available in this runtime")`。
  该函数有**模块级缓存** `_cachedWebsocket`：`env` 为空且已缓存 → 直接返回缓存；
  Bun 分支构造 `WebSocketWithProxy` 子类时**仅在 `env` 为空时**写入缓存（带自定义 env 的调用不缓存）。
  非 Bun 分支直接返回 `globalThis.WebSocket`（可能为 `undefined` → 返回 `null`），不写缓存。
- 头转 record，**删除 `"OpenAI-Beta"`**（大小写敏感删除 —— `headersToRecord` 保留服务端原样大小写，
  Node 的 `Headers` 归一化为小写 `openai-beta`，故此处可能删不到，**照抄**）
- `new WebSocketCtor(url, { headers: wsHeaders })`
- 事件：`open` resolve；`error` → `extractWebSocketError`；`close` → `extractWebSocketCloseError`；
  abort → `Error("Request was aborted")` + close reason `"aborted"`；
  超时 → `Error(\`WebSocket connect timeout after ${connectTimeoutMs}ms\`)` + close reason `"connect_timeout"`
- 若 `signal.aborted` 已为真 → 立刻走 onAbort。

Bun 特殊处理：`process.versions.bun` 时用子类，把 ws 地址换成 http(s) 后查
`resolveHttpProxyUrlForTarget`，透传 `proxy` 选项。

#### 2.14.11 缓存继续（delta）逻辑（内部）

```ts
function requestBodyWithoutInput(body) { const { input, previous_response_id, ...rest } = body; return rest; }
function responseInputsEqual(a, b) { return JSON.stringify(a ?? []) === JSON.stringify(b ?? []); }
function requestBodiesMatchExceptInput(a, b) { return JSON.stringify(requestBodyWithoutInput(a)) === JSON.stringify(requestBodyWithoutInput(b)); }

function getCachedWebSocketInputDelta(body, continuation) {
  if (!requestBodiesMatchExceptInput(body, continuation.lastRequestBody)) return undefined;
  currentInput = body.input ?? [];
  baseline = [...(continuation.lastRequestBody.input ?? []), ...continuation.lastResponseItems];
  if (currentInput.length < baseline.length) return undefined;
  prefix = currentInput.slice(0, baseline.length);
  if (!responseInputsEqual(prefix, baseline)) return undefined;
  return currentInput.slice(baseline.length);
}

function buildCachedWebSocketRequestBody(entry, body) {
  continuation = entry.continuation;
  if (!continuation) return body;
  delta = getCachedWebSocketInputDelta(body, continuation);
  if (!delta || !continuation.lastResponseId) { entry.continuation = undefined; return body; }
  return { ...body, previous_response_id: continuation.lastResponseId, input: delta };
}
```
**要点**：比较依赖 **JSON 序列化相等**（键序敏感）。Rust 侧必须保证同样的键序（见 6.3）。

#### 2.14.12 `processWebSocketStream(...)`（内部）

```
{ socket, entry, reused, release } = await acquireWebSocket(url, headers, cacheSessionId, accountId,
    options?.signal, websocketConnectTimeoutMs, options?.env)
keepConnection = true
useCachedContext = options?.transport === "websocket-cached" || options?.transport === "auto"
fullBody = body
requestBody = (useCachedContext && entry) ? buildCachedWebSocketRequestBody(entry, fullBody) : fullBody
stats = cacheSessionId ? getOrCreateWebSocketDebugStats(cacheSessionId) : undefined
if (stats):
    stats.requests++
    reused ? stats.connectionsReused++ : stats.connectionsCreated++
    if (useCachedContext) stats.cachedContextRequests++
    if (requestBody.store === true) stats.storeTrueRequests++
    stats.lastInputItems = requestBody.input?.length ?? 0
    if (requestBody.previous_response_id):
        stats.deltaRequests++; stats.lastDeltaInputItems = requestBody.input?.length ?? 0
        stats.lastPreviousResponseId = requestBody.previous_response_id
    else:
        stats.fullContextRequests++; stats.lastDeltaInputItems = undefined; stats.lastPreviousResponseId = undefined
try:
    socket.send(JSON.stringify({ type: "response.create", ...requestBody }))
    await processResponsesStream(
        startWebSocketOutputOnFirstEvent(
            mapCodexEvents(parseWebSocket(socket, options?.signal, idleTimeoutMs), output, model, options?.onProviderStreamEvent),
            onStart),
        output, stream, model,
        { serviceTier: options?.serviceTier, grammarToolInputProperties,
          resolveServiceTier: resolveCodexServiceTier,
          applyServiceTierPricing: (usage, tier) => applyServiceTierPricing(usage, tier, model) })
    if (options?.signal?.aborted): keepConnection = false
    else if (useCachedContext && entry && output.responseId):
        responseItems = convertResponsesMessages(model, normalizeContext({ messages: [output] }),
            CODEX_TOOL_CALL_PROVIDERS, { includeSystemPrompt: false, grammarToolInputProperties })
            .filter(item => item.type !== "function_call_output" && item.type !== "custom_tool_call_output")
        entry.continuation = { lastRequestBody: fullBody, lastResponseId: output.responseId, lastResponseItems: responseItems }
catch error:
    if (entry) entry.continuation = undefined
    keepConnection = false
    throw error
finally: release({ keep: keepConnection })
```
- `startWebSocketOutputOnFirstEvent(events, onStart)`：收到**第一个**事件时才 `onStart()`。
- 发送帧为 `{"type":"response.create", ...requestBody}`（`type` 在最前，可被 body 覆盖？body 无 `type`）。

服务层：
```ts
function resolveCodexServiceTier(responseTier, requestTier) {
  if (responseTier === "default" && (requestTier === "flex" || requestTier === "priority")) return requestTier;
  return responseTier ?? requestTier;
}
function getServiceTierCostMultiplier(model, tier) {
  switch (tier) { case "flex": return 0.5; case "priority": return model.id === "gpt-5.5" ? 2.5 : 2; default: return 1; }
}
```
（Codex **没有** `"fast"` 分支，与 `openai-responses` 不同。）

#### 2.14.13 `parseErrorResponse(response)`（内部）

```
raw = await response.text()
message = raw || response.statusText || "Request failed"
friendlyMessage = undefined
try:
    parsed = JSON.parse(raw)
    err = parsed?.error
    if err:
        code = err.code || err.type || ""
        if (/usage_limit_reached|usage_not_included|rate_limit_exceeded/i.test(code) || response.status === 429):
            plan = err.plan_type ? ` (${err.plan_type.toLowerCase()} plan)` : ""
            mins = err.resets_at ? Math.max(0, Math.round((err.resets_at * 1000 - Date.now()) / 60000)) : undefined
            when = mins !== undefined ? ` Try again in ~${mins} min.` : ""
            friendlyMessage = `You have hit your ChatGPT usage limit${plan}.${when}`.trim()
        message = err.message || friendlyMessage || message
catch {}
return { message, friendlyMessage }
```
- 注意 `plan_type` 会被 **小写化**。

#### 2.14.14 `extractAccountId(token)`（内部）

```
parts = token.split(".")
if (parts.length !== 3) throw new Error("Invalid token")
payload = JSON.parse(atob(parts[1]))
accountId = payload?.["https://api.openai.com/auth"]?.chatgpt_account_id
if (!accountId) throw new Error("No account ID in token")
return accountId
// 任何异常（含上面的 throw）都被外层 catch 吞掉，统一抛：
// throw new Error("Failed to extract accountId from token")
```
- `atob` 是 **base64**（非 base64url）；JWT 段若含 `-`/`_` 会失败。Rust 用标准 base64 解码，
  并允许无 padding（Node 的 `atob` 对缺 padding 宽容）。**不要**自动替换 `-_`。
- 最终错误消息固定为 `Failed to extract accountId from token`。

#### 2.14.15 头部构造（内部）

`buildBaseCodexHeaders(defaultHeaders, initHeaders, additionalHeaders, accountId, token)`：
```
headers = new Headers(defaultHeaders)                     // 大小写不敏感
for [key, value] of Object.entries(initHeaders || {}): headers.set(key, value)
for [key, value] of Object.entries(additionalHeaders || {}):
    if (value === null) headers.delete(key) else headers.set(key, value)
headers.set("Authorization", `Bearer ${token}`)
headers.set("chatgpt-account-id", accountId)
return headers
```
`buildSSEHeaders(...)` 追加：
```
headers.set("OpenAI-Beta", "responses=experimental")
headers.set("accept", "text/event-stream")
headers.set("content-type", "application/json")
if (sessionId): headers.set("session-id", sessionId); headers.set("x-client-request-id", sessionId)
```
`buildWebSocketHeaders(...)`：
```
headers.delete("accept"); headers.delete("content-type");
headers.delete("OpenAI-Beta"); headers.delete("openai-beta")
headers.set("OpenAI-Beta", "responses_websockets=2026-02-06")
headers.set("x-client-request-id", requestId)
headers.set("session-id", requestId)
```
- 因为 `Headers` 大小写不敏感，两次 delete 是冗余但无害；Rust 侧用 `HeaderMap`（同样不敏感）。
- `Authorization`/`chatgpt-account-id` 在 model/caller 头之后设置，**不可被覆盖**：`openai-codex-stream.test.ts`
  的 "lets model and caller headers override originator and User-Agent" 显式传入
  `headers: { Authorization: "Bearer ignored" }`，断言最终 `Authorization` 为 `Bearer ${token}`。
  同理 `chatgpt-account-id` 也恒为 token 里的 account id。

#### 2.14.16 与其他模块的行为差异汇总（Codex）

| 维度 | Codex | openai-responses |
|---|---|---|
| 系统提示 | `instructions` 字段（单独取首条 system） | `input` 里的 developer/system 消息 |
| `store` | `false` | `false` |
| `text.verbosity` | 有（默认 `"low"`） | 无 |
| `parallel_tool_calls` | `true` | 无 |
| `tool_choice` | 默认 `"auto"` | 仅显式传入时 |
| `prompt_cache_key` | `cacheSessionId`（受 `cacheRetention === "none"`） | 同 |
| `prompt_cache_retention` | **无** | 有 |
| 传输 | WS（默认 auto）+ SSE 回退 | 仅 SSE |
| 服务层 `"fast"` | 无 | 有 |
| 错误前缀 | 无 | `<provider> API error` |

---

## 3. 用户可见面（逐字）

本单元是**库层**，不直接拥有 CLI 参数、斜杠命令、键绑定或 TUI 渲染。它的"用户可见面"是：

1. **写入 `AssistantMessage.errorMessage` 的错误文本**——这些会原样显示在 TUI/CLI 中。
2. **写入 `AssistantMessage` 内容块的文本**（思考占位、工具结果占位、注入的 assistant 桥接文本）。
3. **写入 `diagnostics` 的结构**（可能被 `/debug` 类命令展示）。
4. **发往 provider 的 system 提示包装文本**（会影响模型行为，从而间接影响用户可见回答）。

### 3.1 错误消息全表（逐字，按来源模块）

#### 3.1.1 `lazy.ts`
| 条件 | 文本 |
|---|---|
| setup 抛错 | `error.message` 原样（非 Error 则 `String(error)`） |
| 缺 `fetchDeferred` | `API does not support deferred responses` |
| 缺 `cancelDeferred` | `API cannot cancel deferred responses` |

#### 3.1.2 `transform-messages.ts`（内容占位文本，非错误）
| 常量 | 文本 |
|---|---|
| 用户图片降级 | `(image omitted: model does not support images)` |
| 工具结果图片降级 | `(tool image omitted: model does not support images)` |
| 错误工具结果图片 | `(image attached after the tool results)` |
| 错误工具结果图片说明 | `Images from the error result of tool call <toolCallId> (<toolName>):` |
| 孤儿 toolCall 合成结果 | `No result provided` |

#### 3.1.3 `openai-prompt-cache.ts`
无错误；`64` 上限。

#### 3.1.4 `simple-options.ts`
无错误消息。

#### 3.1.5 `azure-openai-config.ts`
| 条件 | 文本 |
|---|---|
| URL 解析失败 | `Invalid Azure OpenAI base URL: <原始 baseUrl>` |
| 无法确定 base URL | `Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.` |

#### 3.1.6 `constrained-sampling.ts`
| 条件 | 文本 |
|---|---|
| 布尔 schema | `boolean schemas are unsupported` |
| 禁用关键字 | `<key> schemas are unsupported` |
| 自定义关键字 | `<key>: <JSON.stringify(value)> is unsupported` |
| anyOf 空 | `anyOf must contain at least one schema` |
| 结构化 anyOf 分支 | `object and array unions are unsupported` |
| 元组 items | `tuple schemas are unsupported` |
| properties 无 object | `properties require type object` |
| additionalProperties | `schema-valued or true additionalProperties is unsupported` |
| properties 非 map | `object properties must be a schema map` |
| required 非字符串数组 | `object required must be a string array` |
| required 含未知属性 | `required contains an unknown property` |
| 根不是对象 | `root schema must have type object` |
| strict require 但失败 | `Tool "<name>" requires JSON-schema constrained sampling, but <inner>.` |
| strict require 但 provider 不支持 | `Tool "<name>" requires JSON-schema constrained sampling, but strict tools are unsupported.` |
| grammar 调用参数非字符串 | `Grammar tool call "<toolName>" requires argument "<inputProperty>" to be a string.` |
| grammar 关闭后修改 | `grammar tool input for property "<p>" changed after it was closed` |
| grammar 非单调 | `grammar tool input for property "<p>" changed non-monotonically` |
| 无可用 grammar 变体 | `Tool "<name>" cannot use grammar constrained sampling: no supported grammar variant was provided.` |
| grammar 推断失败 | `Tool "<name>" cannot use grammar constrained sampling: <inner>.` |
| grammar schema 非 object | `grammar constrained sampling requires an object parameter schema` |
| grammar 非单一 required string | `grammar constrained sampling requires exactly one required string property` |
| grammar 缺属性定义 | `grammar constrained sampling requires a properties entry for <prop>` |
| grammar 属性非 string | `grammar constrained sampling property <prop> must have type string` |

#### 3.1.7 `classifier-shared.ts` / `openai-decisions.ts`
| 条件 | 文本 |
|---|---|
| 缺 key | `No API key for provider: <provider>` |
| HTTP 失败 | `OpenAI Decisions returned <status>` |
| 超时 | `Request timed out after <ms>ms` |
| 非法数字 | `OpenAI Decisions returned an invalid <field>` |
| 504 | `OpenAI Decisions error (504): the request timed out at the gateway. Very large inputs (above roughly 600K tokens) currently exceed its time limit.` |
| 其他错误 | `OpenAI Decisions error` 前缀 + `formatProviderError` 输出 |
| 图片过多 | `OpenAI Decisions accepts at most 128 images, got <n>` |
| 概率非法 | `OpenAI Decisions returned invalid probabilities for <id>` |
| 拒绝回答 | `OpenAI Decisions refused to answer <id>` |
| 非 choice | `OpenAI Decisions did not return a choice answer for <id>` |
| 非 score | `OpenAI Decisions did not return a score answer for <id>` |
| 非 predicate | `OpenAI Decisions did not return a predicate answer for <id>` |
| 非数组 answers | `OpenAI Decisions returned an unexpected response` |
| 缺答案 | `OpenAI Decisions did not return an answer for <id>` |
| api 不匹配 | `Unsupported classifier API: <api>` |

#### 3.1.8 `anthropic-messages.ts`
| 条件 | 文本 |
|---|---|
| 缺鉴权 | `No API key for provider: <provider>` |
| 中止 | `Request was aborted` |
| 无 body | `Attempted to iterate over an Anthropic response with no body` |
| SSE error 事件 | SSE 的 `data` 原文 |
| SSE 解析失败 | `Could not parse Anthropic SSE event <event>: <message>; data=<data>; raw=<raw 以字面 \n 连接>` |
| 提前结束 | `Anthropic stream ended before message_stop` |
| 中途 fallback | `Anthropic performed an unsupported mid-output model fallback` |
| 无 stop reason | `Anthropic stream ended without a stop reason` |
| aborted/error | `output.errorMessage` 或 `An unknown error occurred` |
| 拒答 | `stop_details.explanation` 或 `The model refused to complete the request` |
| sensitive | `Provider stopped with: sensitive` |
| 未知 stop reason | `Unhandled stop reason: <reason>` |
| 无 stop 的兜底 | `An unknown error occurred` |

#### 3.1.9 `openai-completions.ts`
| 条件 | 文本 |
|---|---|
| 缺鉴权 | `No API key for provider: <provider>` |
| 中止 | `Request was aborted` |
| 无 finish_reason（要求时） | `Stream ended without finish_reason` |
| provider error stop | `output.errorMessage` 或 `Provider returned an error stop reason` |
| content_filter | `Provider finish_reason: content_filter` |
| network_error | `Provider finish_reason: network_error` |
| 其他 finish_reason | `Provider finish_reason: <reason>` |
| 其他错误 | `formatProviderError(normalizeProviderError(error))`（无前缀），必要时追加 `\n<error.error.metadata.raw>` |

#### 3.1.10 `openai-responses.ts`
| 条件 | 文本 |
|---|---|
| 缺鉴权 | `No API key for provider: <provider>` |
| 中止 | `Request was aborted` |
| 无 stop reason | `OpenAI Responses stream ended without a stop reason` |
| 其他错误 | `<OpenAI|provider> API error (<status>): <body|message>` |
| 用量上限 | 上述文本 + `\nCheck your ChatGPT usage: https://chatgpt.com/settings/usage` |
| 工作区上限 | 上述文本 + `\nYou've reached your ChatGPT workspace's usage limit. Ask your workspace admin to increase it at admin.openai.com → Workspace → Usage limits.` |

#### 3.1.11 `openai-responses-shared.ts`
| 条件 | 文本 |
|---|---|
| 错误事件 | `Error Code <code>: <message>` |
| failed 且有 error | `<code|unknown>: <message|no message>` |
| failed 有 incomplete reason | `incomplete: <reason>` |
| failed 无信息 | `Unknown error (no error details in response)` |
| 无终止事件 | `OpenAI Responses stream ended before a terminal response event` |
| 未完成的工具调用 | `OpenAI Responses stream completed with an unfinished tool call: <name> (<id>)` |
| incomplete 无 reason | `Response incomplete without a provider reason` |
| incomplete 有 reason | `Response incomplete: <reason>` |
| 未知 status | `Unhandled stop reason: <status>` |
| 增量工具提示（正文） | `This is an incremental tools update. Previously declared tools remain available unless explicitly marked unavailable. A tool declared again here is available again, and its latest definition replaces the earlier one.` |
| 增量工具提示（追加） | `\nStill unavailable: <name1>, <name2>.` |
| 工具移除通知 | `The following tools are no longer available. Do not call them:\n- <name>...` |
| 工具结果占位 | `(see attached image)` / `(no tool output)` |

#### 3.1.12 `azure-openai-responses.ts`
| 条件 | 文本 |
|---|---|
| 缺鉴权 | `No API key for provider: <provider>` |
| 中止 | `Request was aborted` |
| 无 stop reason | `Azure OpenAI Responses stream ended without a stop reason` |
| 其他错误 | `Azure OpenAI API error (<status>): <body|message>` |

#### 3.1.13 `openai-codex-responses.ts`
| 条件 | 文本 |
|---|---|
| 缺鉴权 | `No API key for provider: <provider>` |
| 中止 | `Request was aborted` |
| token 无效 | `Failed to extract accountId from token` |
| 无 stop reason | `Codex stream ended without a stop reason` |
| 其他 | `output.errorMessage` 或 `An unknown error occurred` |
| 无效 timeout | `Invalid timeoutMs: <value>` |
| 重试延迟超限 | `Server requested <n>s retry delay (max: <m>s)` |
| SSE 头超时 | `Codex SSE response headers timed out after <ms>ms` |
| 无 body | `No response body` |
| 重试耗尽 | `Failed after retries` |
| SSE JSON 无效 | `Invalid Codex SSE JSON: <msg>` |
| WS JSON 无效 | `Invalid Codex WebSocket JSON: <msg>` |
| WS 错误 | `WebSocket error` / 事件里的 message |
| WS 关闭 | `WebSocket closed <code> <reason>`（无 code/reason 则 `WebSocket closed`；1009 且无 reason → `WebSocket closed 1009 message too big`） |
| WS 空闲超时 | `WebSocket idle timeout after <ms>ms` |
| WS 提前关闭 | `WebSocket stream closed before response.completed` |
| WS 连接超时 | `WebSocket connect timeout after <ms>ms` |
| WS 不可用 | `WebSocket transport is not available in this runtime` |
| Codex error 事件 | `Codex error: <message|code|JSON>` |
| response.failed | `Codex response failed` 或 provider message |
| 用量上限 | `You have hit your ChatGPT usage limit<plan>.< Try again in ~N min.>`（`.trim()` 可能吃掉尾空格） |

### 3.2 注入到对话中的文本（会进入后续请求，模型可见）

| 位置 | 文本 |
|---|---|
| Anthropic OAuth 身份 | `You are Claude Code, Anthropic's official CLI for Claude.` |
| 纯图片内容占位 | `(see attached image)` |
| 工具结果只有图片 | `(see attached image)` |
| 工具结果无内容 | `(no tool output)` |
| 工具结果图片批注（completions） | `Attached image(s) from tool result:` |
| 桥接 assistant（completions/responses 前） | `I have processed the tool results.` |
| Codex 默认 instructions | `You are a helpful assistant.` |
| 增量工具提示 | 见 3.1.11 |
| 工具移除通知 | 见 3.1.11 |
| 系统消息更新（`renderSystemMessageUpdate`） | `Updated system prompt section "<name>":\n\n<value>` 或 `Removed system prompt section "<name>".` |
| Anthropic 占位工具名 | `__kiana_deferred_placeholder__` |
| Anthropic 占位工具描述 | `Reserved placeholder. Never available. Never call this.` |
| Anthropic 红acted 思考文本 | `[Reasoning redacted]` |

### 3.3 `diagnostics` 条目（结构化）

| type | 触发 | details |
|---|---|---|
| `anthropic_strict_tools_fallback` | strict 工具被 400 拒绝后降级 | `{ reason: "strict_tool_limits_exceeded" }`，`error: { message }` |
| `anthropic_input_transformations` | 收到 `input_transformations` | `{ transformations: [{ type?, path?, reason? }] }` |
| `provider_transport_failure` | Codex WS 传输失败 | `{ configuredTransport, fallbackTransport?, eventsEmitted, phase, requestBytes }` |

`AssistantMessageDiagnostic` 形状：`{ type: string; timestamp: number; error?: { name?, message, stack?, code? }; details?: JsonObject }`。

### 3.4 无 ANSI / 无 TUI 渲染

本单元**不产生任何 ANSI 转义、颜色、边框、spinner 帧或布局宽度计算**。渲染完全在 coding-agent/TUI 层。
因此移植时**不需要** crossterm / unicode-width 依赖。

### 3.5 spinner / 进度

无。长耗时仅通过网络 I/O 体现。

---

## 4. 外部格式（逐字）

### 4.1 环境变量

| 变量 | 读取点 | 语义 |
|---|---|---|
| `KIANA_CACHE_RETENTION` | anthropic/completions/responses | 值 `"long"` → 默认 retention `"long"`；否则 `"short"`。显式 `options.cacheRetention` 优先 |
| `AZURE_OPENAI_BASE_URL` | `resolveAzureBaseUrl` | 端点（trim 后非空才用） |
| `AZURE_OPENAI_RESOURCE_NAME` | `resolveAzureBaseUrl` | 生成 `https://<name>.openai.azure.com/openai/v1` |
| `AZURE_OPENAI_API_VERSION` | `resolveAzureConfig` | 默认 `"v1"` |
| `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` | `resolveDeploymentName` | `modelId=deployment` 逗号分隔 |
| `AZURE_OPENAI_API_KEY` | `providers/azure.ts`（不在本单元） | 鉴权 |
| `ANTHROPIC_FEDERATION_RULE_ID` | `getAnthropicFederation` | 必需（三者之一） |
| `ANTHROPIC_ORGANIZATION_ID` | `getAnthropicFederation` | 必需 |
| `ANTHROPIC_IDENTITY_TOKEN_FILE` | `getAnthropicFederation` | 必需 |
| `ANTHROPIC_WORKSPACE_ID` | `getAnthropicFederation` | 可选 |
| `ANTHROPIC_SERVICE_ACCOUNT_ID` | `getAnthropicFederation` | 可选 |
| `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_OAUTH_TOKEN` | `auth/` 层（不在本单元） | 生成 Authorization 头 |
| `KIANA_CODING_AGENT_DIR` | 仅测试使用 | 隔离配置目录 |

`getProviderEnvValue(name, env)` 解析顺序：
1. `env?.[name]`（**真值**，空串视为无）
2. `process.env[name]`（`typeof process !== "undefined"` 时）
3. Bun sandbox 回退：`process.versions.bun && Object.keys(process.env).length === 0` 时读
   `/proc/self/environ`（NUL 分隔，首个 `=` 前为键），结果缓存。
4. `undefined`

Rust 侧：`env_override: Option<&HashMap<String,String>>` → `std::env::var` → 无 Bun 回退
（如需在 Linux sandbox 下等价，可选实现 `/proc/self/environ` 回退；建议保留一个 `bun_sandbox` 特性开关）。

### 4.2 HTTP 端点

| 模块 | 方法 | URL |
|---|---|---|
| Anthropic | POST | SDK 生成：`<baseURL>/v1/messages?beta=true`（`baseURL` = `model.baseUrl`，通常 `https://api.anthropic.com`）。**移植时须对照 `@anthropic-ai/sdk@0.129.0` 的 `resources/beta/messages` 确认查询参数与版本头** |
| Chat Completions | POST | `<model.baseUrl>/chat/completions` |
| Responses | POST | `<model.baseUrl>/responses` |
| Azure Responses | POST | `<resolvedBaseUrl>/responses?api-version=<apiVersion>`（SDK 生成；`baseUrl` 已含 `/openai/v1`） |
| Codex SSE | POST | `resolveCodexUrl(baseUrl)`：`.../codex/responses` |
| Codex WS | WS | `resolveCodexWebSocketUrl(baseUrl)`：把 scheme 换成 `wss`/`ws` |
| Decisions | POST | `<baseUrl 去尾斜杠>/decisions` |

**SDK 路径细节（Rust 需自行拼）**：
- Anthropic SDK `client.beta.messages.create(...)` 对 `baseURL = https://api.anthropic.com` 发出
  `POST https://api.anthropic.com/v1/messages?beta=true`（beta 资源路径带 `?beta=true`）。
  若 `baseURL` 已含 `/v1`，SDK 会再拼 `/v1/messages`（即 `https://host/v1/v1/messages`）
  ——**kiana 的 baseUrl 约定不含 `/v1`**。
  **验证方法**：在装有 `@anthropic-ai/sdk@0.129.0` 的环境里跑一次 mock fetch，捕获 `input[0]` 的
  URL 与 `headers`，作为 Rust 侧黄金样本；本规范不对该 URL 做"逐字节"承诺。
- OpenAI SDK `chat.completions.create` → `POST <baseURL>/chat/completions`
  （`baseURL` 默认 `https://api.openai.com/v1`，故完整为 `/v1/chat/completions`）。
- OpenAI SDK `responses.create` → `POST <baseURL>/responses`。
- `AzureOpenAI` 的 `baseURL` 是 `https://<res>.openai.azure.com/openai/v1`，SDK 拼
  `.../openai/v1/responses?api-version=v1`。

### 4.3 请求头

#### Anthropic（非 OAuth、非 Copilot）
```
accept: application/json
anthropic-dangerous-direct-browser-access: true
anthropic-beta: <逗号分隔，仅当非空>
x-session-affinity: <sessionId>     （或 x-session-id，openrouter 格式；仅当 caching 开启且 compat 允许）
x-api-key: <apiKey>                 （SDK 设置）
anthropic-version: 2023-06-01       （SDK 设置）
user-agent: <getIdentityHeaders 的 User-Agent>
<attribution 头>
<model.headers>
<options.headers>
```
OAuth 时改为 `authorization: Bearer <token>` + `user-agent: claude-cli/2.1.280` + `x-app: cli`，
**不**带 kiana 的 User-Agent/attribution。

Copilot 时 `authorization: Bearer <token>`（SDK 的 `authToken`）+ `X-Initiator`、`Openai-Intent`、
可选 `Copilot-Vision-Request`、`anthropic-dangerous-direct-browser-access: true`。

#### Chat Completions / Responses（OpenAI SDK）
```
Authorization: Bearer <apiKey>
User-Agent: <identity>
<attribution 头>
<Copilot 动态头（provider 为 github-copilot 时）>
session_id / x-client-request-id / x-session-affinity（按 compat 组合；Responses 无 x-session-affinity）
<model.headers>
<options.headers>
```
`apiKey === "unused"` 用于调用方已提供 `authorization` 头的场景。

#### Codex
SSE：
```
Authorization: Bearer <token>
chatgpt-account-id: <accountId>
originator: kiana
User-Agent: <identity>
OpenAI-Beta: responses=experimental
accept: text/event-stream
content-type: application/json
content-encoding: zstd        （压缩成功时）
session-id / x-client-request-id （有 sessionId 时；值为 clamp 到 64 码点的 sessionId）
<model.headers> <options.headers>
```
WebSocket：
```
Authorization: Bearer <token>
chatgpt-account-id: <accountId>
originator: kiana
User-Agent: <identity>
OpenAI-Beta: responses_websockets=2026-02-06
x-client-request-id: <requestId>
session-id: <requestId>
（无 accept / content-type）
<model.headers> <options.headers>
```
`requestId = codexSessionId || uuidv7()`。

#### Decisions
```
authorization: Bearer <apiKey>
content-type: application/json
User-Agent: <identity>
<attribution>
<model.headers> <options.headers>
```
（注意这里键名全小写 `authorization`，由 `providerHeadersToRecord` 保序输出。）

### 4.4 请求体（JSON）

#### 4.4.1 Anthropic Messages

```jsonc
{
  "model": "<model.id>",
  "messages": [ /* MessageParam[]，见 2.9.15 */ ],
  "max_tokens": <number>,
  "stream": true,
  "betas": ["..."],                       // 可选
  "system": [{ "type": "text", "text": "...", "cache_control": {"type":"ephemeral","ttl":"1h"} }],
  "temperature": <number>,                // 条件见 2.9.14
  "tools": [ /* BetaTool[] */ ],
  "thinking": { "type": "adaptive"|"enabled"|"disabled", "display"?, "budget_tokens"?, "block_binding"? },
  "output_config": { "effort": "low"|"medium"|"high"|"xhigh"|"max" },
  "metadata": { "user_id": "..." },
  "tool_choice": { "type": "auto"|"any"|"none" } | { "type": "tool", "name": "..." },
  "fallbacks": [{ "model": "..." }]
}
```
**消息块类型**（线上字段名逐字）：
```jsonc
{ "type": "text", "text": "..." }
{ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "..." } }
{ "type": "thinking", "thinking": "...", "signature": "..." }
{ "type": "redacted_thinking", "data": "..." }
{ "type": "tool_use", "id": "...", "name": "...", "input": { } }
{ "type": "tool_result", "tool_use_id": "...", "content": "..."|[...], "is_error": true }
{ "type": "tool_removal", "tool": { "type": "tool_reference", "name": "..." } }        // 仅 system
{ "type": "tool_addition", "tool": { "type": "tool_definition", "definition": {...} } } // 仅 system
```
**mid-conversation effort marker**（system 消息）：
```jsonc
{ "role": "system", "content": [], "output_config": { "effort": "low" } }
```
**工具定义**：
```jsonc
{
  "name": "...",
  "description": "...",
  "eager_input_streaming": true,
  "strict": true,
  "input_schema": { "type": "object", "properties": {}, "required": [] },
  "cache_control": { "type": "ephemeral", "ttl": "1h" },
  "defer_loading": true
}
```

#### 4.4.2 Chat Completions

```jsonc
{
  "model": "<model.id>",
  "messages": [...],
  "stream": true,
  "prompt_cache_key": "<≤64 码点>",
  "prompt_cache_retention": "24h",
  "stream_options": { "include_usage": true },
  "store": false,
  "max_tokens": <n>,              // 或 max_completion_tokens
  "temperature": <n>,
  "tools": [...],                 // 或 []
  "tool_stream": true,            // zai compat
  "tool_choice": ...,
  "priority": <n>,                // vllmPriority
  "thinking": {...},              // 视 thinkingFormat
  "reasoning_effort": "...",
  "enable_thinking": true,
  "chat_template_kwargs": {...},
  "chat_template_args": {...},
  "reasoning": {...},
  "thinking_token_budget"/"thinking_budget"/"thinking_budget_tokens": <n>,
  "provider": {...},              // OpenRouter routing
  "providerOptions": { "gateway": { "only": [...], "order": [...] } },
  ...samplingParams
}
```
**消息形状**：
```jsonc
{ "role": "developer"|"system", "content": "..." }
{ "role": "system", "tools": [...] }                                  // Kimi 工具注入
{ "role": "user", "content": "..." | [{ "type":"text","text":"..." }, { "type":"image_url","image_url":{"url":"data:..."} }] }
{ "role": "assistant", "content": null|""|"..."|[...], "tool_calls": [...], "reasoning_content": "...", "reasoning": "...", "reasoning_text": "...", "reasoning_details": [...] }
{ "role": "tool", "content": "...", "tool_call_id": "...", "name": "..." }
```
tool_calls 元素：
```jsonc
{ "id": "...", "type": "function", "function": { "name": "...", "arguments": "<JSON 文本>" } }
{ "id": "...", "type": "custom",   "custom":   { "name": "...", "input": "<纯文本>" } }
```
**cache_control** 注入点：首条 system/developer 的最后一个 text part、最后一个 tool、最后一条
user/assistant/tool 的最后一个 text part。

#### 4.4.3 Responses

```jsonc
{
  "model": "<model.id 或 deploymentName>",
  "input": [...],
  "stream": true,
  "prompt_cache_key": "...",
  "prompt_cache_retention": "24h",
  "prompt_cache_options": { "mode": "explicit" } | { "ttl": "30m" },
  "store": false,
  "max_output_tokens": <n>,
  "temperature": <n>,
  "service_tier": "auto"|"default"|"flex"|"priority"|"fast",
  "tools": [...],
  "tool_choice": ...,
  "reasoning": { "effort": "...", "summary": "auto"|"detailed"|"concise" },
  "include": ["reasoning.encrypted_content"],
  ...samplingParams
}
```
**input item 类型**：
```jsonc
{ "role": "developer"|"system"|"user", "content": "..." }                       // 字符串或 part 数组
{ "type": "input_text", "text": "..." }
{ "type": "input_image", "detail": "auto", "image_url": "data:..." }
{ "type": "message", "role": "assistant", "content": [{ "type":"output_text","text":"...","annotations":[] }], "status": "completed", "id": "...", "phase": "commentary"|"final_answer" }
{ "type": "reasoning", ... }                                                     // JSON.parse(thinkingSignature) 原样
{ "type": "function_call", "id": "fc_...", "call_id": "...", "name": "...", "arguments": "<JSON>", "namespace": "..." }
{ "type": "custom_tool_call", "id": "ctc_...", "call_id": "...", "name": "...", "input": "...", "namespace": "..." }
{ "type": "function_call_output", "call_id": "...", "output": "..."|[{input_text|input_image}] }
{ "type": "custom_tool_call_output", "call_id": "...", "output": ... }
{ "type": "additional_tools", "role": "developer", "tools": [...] }
{ "type": "tool_search_call", "call_id": "kiana_tool_load_<hash>", "execution": "client", "status": "completed", "arguments": { "query": "...", "limit": <n> } }
{ "type": "tool_search_output", "call_id": "kiana_tool_load_<hash>", "execution": "client", "status": "completed", "tools": [...] }
{ "type": "configuration_update", "reasoning": { "effort": "..." } }
```
**工具定义**：
```jsonc
{ "type": "function", "name": "...", "description": "...", "parameters": {...}, "strict": true|false|null, "defer_loading": true }
{ "type": "custom", "name": "...", "description": "...", "format": { "type":"grammar", "syntax":"lark"|"regex", "definition": "..." }, "defer_loading": true }
```
（注意 Responses 的工具是 **扁平** 结构，没有 `function` 包装；Chat Completions 有。）

#### 4.4.4 Codex

```jsonc
{
  "model": "<model.id>",
  "store": false,
  "stream": true,
  "instructions": "<首条 system 文本 或 'You are a helpful assistant.'>",
  "input": [ /* 同 Responses，但 includeSystemPrompt=false */ ],
  "text": { "verbosity": "low"|"medium"|"high" },
  "include": ["reasoning.encrypted_content"],
  "prompt_cache_key": "<sessionId 或省略>",
  "tool_choice": "auto"|"none"|"required",
  "parallel_tool_calls": true,
  "temperature": <n>,
  "service_tier": "...",
  "tools": [...],
  "reasoning": { "effort": "...", "summary": "auto"|"concise"|"detailed"|"off"|"on" },
  "previous_response_id": "<仅 WS 缓存继续>"
}
```
WebSocket 帧：`{"type":"response.create", ...body}`。

#### 4.4.5 Decisions

```jsonc
{
  "model": "<model.id>",
  "input": "<state 的 JSON 字符串>" | [ { "role":"user", "content":[ {"type":"input_text","text":"..."}, {"type":"input_image","image_url":"data:..."} ] } ],
  "questions": [
    { "type":"choice", "name":"...", "instructions":"...", "choices":[{"value":"...","description":"..."}] },
    { "type":"score", "name":"...", "instructions":"...", "levels":[{"label":"..."}] },
    { "type":"predicate", "name":"...", "instructions":"...\n\nTrue means: ...\nFalse means: ..." }
  ]
}
```

#### 4.4.6 响应（Decisions）

```jsonc
{
  "usage": { "input_tokens": <n>, "output_tokens": <n> },     // 可选
  "answers": [
    { "name": "...", "type": "choice", "choice": "...", "probabilities": [{"value":"...","probability":0.9}], "confidence": 0.8 },
    { "name": "...", "type": "score", "score": 3, "confidence": 0.7 },
    { "name": "...", "type": "predicate", "probability": 0.6 },
    { "name": "...", "type": "refusal" }
  ]
}
```

### 4.5 SSE 事件（上游 → 本单元）

#### 4.5.1 Anthropic SSE 帧
```
event: message_start
data: {"type":"message_start","message":{"id":"msg_...","model":"...","usage":{"input_tokens":10,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"cache_creation":{"ephemeral_1h_input_tokens":0}},"input_transformations":[...]}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text"|"thinking"|"redacted_thinking"|"tool_use"|"fallback", ...}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta"|"thinking_delta"|"input_json_delta"|"signature_delta", ...}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_details":{...}},"usage":{...},"input_transformations":[...]}

event: message_stop
data: {"type":"message_stop"}

event: error
data: <任意文本，直接作为错误消息>
```
- 只处理 `ANTHROPIC_MESSAGE_EVENTS` 中的 6 种；`ping`、`message_stop` 后的未知事件等忽略。
- 事件边界：空行；`:` 开头为注释；`data:` 多行以 `\n` 连接；去掉 `data:` 后的**一个**前导空格。
- 支持 `\r\n`、`\r`、`\n` 三种行尾。

#### 4.5.2 Chat Completions SSE
```
data: {"id":"chatcmpl-...","object":"chat.completion.chunk","created":...,"model":"...",
       "choices":[{"index":0,"delta":{"role":"assistant","content":"...","reasoning_content":"...",
                  "reasoning_details":[...],"tool_calls":[{"index":0,"id":"...","type":"function",
                  "function":{"name":"...","arguments":"{...partial"}}]},"finish_reason":null}],
       "usage":{...}}
data: [DONE]
```
- 解析交给 OpenAI SDK（Rust 需自实现 SSE 解析：按 `\n\n` 或 `\r\n\r\n` 分帧，取所有 `data:` 行，
  `[DONE]` 结束）。
- `usage` 可能出现在最后一块或 `choice.usage`。

#### 4.5.3 Responses SSE
事件类型（本单元处理）：
```
response.created
response.output_item.added
response.output_item.done
response.reasoning_summary_text.delta
response.reasoning_summary_part.done
response.reasoning_text.delta
response.output_text.delta
response.refusal.delta
response.function_call_arguments.delta
response.function_call_arguments.done
response.custom_tool_call_input.delta
response.custom_tool_call_input.done
response.completed
response.incomplete
response.failed
error
```
未列出的事件（`response.content_part.added`、`response.output_text.done` 等）被 `processResponsesStream`
的 if/else 链**静默跳过**（但仍会先调用 `onProviderStreamEvent`）。

#### 4.5.4 Codex SSE
- 帧以 `"\n\n"` 分隔；EOF 时残帧补 `"\n\n"` 处理。
- 每帧取所有以 `data:` 开头的行，`slice(5).trim()`，再 `join("\n").trim()`。
- `[DONE]` 跳过；空 data 跳过。
- 事件 JSON 的 `type` 经 `mapCodexEvents` 归一化：`response.done`/`response.completed`/`response.incomplete`
  统一变成 `response.completed`（并把 `status` 归一化到已知集合）。
- `end_turn`（boolean）写入 `output.endTurn`。

#### 4.5.5 Codex WebSocket
- 每个 message 一个 JSON 对象；`type ∈ {response.completed, response.done, response.incomplete}` 结束。
- 结束前必须见到其中之一，否则 `WebSocket stream closed before response.completed`。

### 4.6 磁盘文件

本单元**不读写任何磁盘文件**（除 `getProviderEnvValue` 的 `/proc/self/environ` 只读回退，
以及测试用 `KIANA_CODING_AGENT_DIR`）。
`ANTHROPIC_IDENTITY_TOKEN_FILE` 指向的文件由 **Anthropic SDK** 读取（Rust 侧需自行读取该文件内容，
用于 federation 的 `identity_token.source = file`）。

### 4.7 退出码 / 进程行为

无。本单元是库，不 `process.exit`、不写 stdout/stderr。

### 4.8 UUIDv7（`utils/uuid.ts`，Codex 使用）

- 时间戳毫秒（48 bit）+ 41 bit 序列 + 随机。
- 普通调用保证时间戳单调不减（`max(requested, lastOrdinaryTimestamp)`）；带显式时间戳时不影响全局状态。
- 序列：首次从随机字节派生 41 bit；之后每次 `+1`；耗尽抛 `RangeError("UUIDv7 generator sequence exhausted")`。
- 布局：`bytes[6] = 0x70 | (seq >> 37) & 0x0f`；`bytes[8] = 0x80 | (seq >> 23) & 0x3f`；
  `bytes[11] = ((seq & 0x7f) << 1) | (bytes[11] & 0x01)`。
- 输出小写十六进制，`8-4-4-4-12`。
- 非法时间戳 → `RangeError(\`UUIDv7 timestamp must be an integer between 0 and ${MAX}\`)`，`MAX = 0xffffffffffff`。

---

## 5. 跨单元接口

### 5.1 本单元消费的类型（必须由上游单元提供，逐字一致）

来自 `src/types.ts`（归属单元：`ai-core-types`）：

```ts
type Api = "anthropic-messages" | "openai-completions" | "openai-responses" | "openai-codex-responses"
         | "azure-openai-responses" | "google-generative-ai" | "google-vertex" | "mistral-conversations"
         | "bedrock-converse-stream" | "kiana-messages";

type StopReason = "pending" | "stop" | "length" | "toolUse" | "error" | "aborted";
type CacheRetention = "none" | "short" | "long";
type ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
type ModelThinkingLevel = ThinkingLevel;
type Transport = "auto" | "sse" | "websocket" | "websocket-cached";
type ToolChoice = { type: "auto" } | { type: "none" } | { type: "required" } | { type: "tool"; name: string };

interface Usage {
  input: number; output: number; cacheRead: number; cacheWrite: number;
  cacheWrite1h?: number; reasoning?: number; totalTokens: number;
  cost: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number;
          totalFromProvider?: boolean };
}

interface TextContent { type: "text"; text: string; textSignature?: string }
interface ThinkingContent { type: "thinking"; thinking: string; thinkingSignature?: string; redacted?: boolean }
interface ImageContent { type: "image"; mimeType: string; data: string }
interface ToolCall { type: "toolCall"; id: string; name: string; arguments: Record<string, unknown>;
                     argumentsError?: string; thoughtSignature?: string; namespace?: string }
interface ToolResultMessage { role: "toolResult"; toolCallId: string; toolName: string;
                              content: (TextContent|ImageContent)[]; isError: boolean; timestamp: number }
interface SystemMessage { role: "system"; content: string | (TextContent|ImageContent)[];
                          sections?: Record<string, string|null>; toolsAdded?: Tool[];
                          toolsRemoved?: ToolReference[]; timestamp: number }
interface AssistantMessage {
  role: "assistant"; content: (TextContent|ThinkingContent|ToolCall)[]; api: Api; provider: string;
  model: string; usage: Usage; stopReason: StopReason; errorMessage?: string; timestamp: number;
  durationMs?: number; responseModel?: string; responseId?: string; providerThinkingLevel?: string;
  diagnostics?: AssistantMessageDiagnostic[]; rawStopReason?: string; endTurn?: boolean;
}
interface Model<TApi extends Api = Api> {
  id: string; name: string; api: TApi; provider: string; baseUrl: string; reasoning: boolean;
  input: ("text"|"image")[]; cost: ModelCost; contextWindow: number; maxTokens: number;
  headers?: ProviderHeaders; compat?: ...; thinkingLevelMap?: Partial<Record<ModelThinkingLevel, string|null>>;
  samplingParams?: SamplingParams; samplingParamsByThinkingLevel?: Partial<Record<ModelThinkingLevel, SamplingParams>>;
}
type ProviderHeaders = Record<string, string | null>;
interface StreamOptions { ... }            // 见 5.2
interface SimpleStreamOptions extends StreamOptions { toolChoice?: ToolChoice; reasoning?: ThinkingLevel;
                                                      deferred?: ...; thinkingBudgets?: ThinkingBudgets }
type TranscriptContext = { readonly messages: readonly Message[] }  // branded
interface ProviderStreams { stream; streamSimple; fetchDeferred?; cancelDeferred? }
interface ProviderClassifier { classify(...) }
```

`AssistantMessageEvent` 联合（归属 `ai-core-event-stream`）：
```ts
{ type:"start"; partial: AssistantMessage }
{ type:"text_start"; contentIndex: number; partial: AssistantMessage }
{ type:"text_delta"; contentIndex: number; delta: string; partial: AssistantMessage }
{ type:"text_end"; contentIndex: number; content: string; partial: AssistantMessage }
{ type:"thinking_start"; contentIndex: number; partial: AssistantMessage }
{ type:"thinking_delta"; contentIndex: number; delta: string; partial: AssistantMessage }
{ type:"thinking_end"; contentIndex: number; content: string; partial: AssistantMessage }
{ type:"toolcall_start"; contentIndex: number; partial: AssistantMessage }
{ type:"toolcall_delta"; contentIndex: number; delta: string; partial: AssistantMessage }
{ type:"toolcall_end"; contentIndex: number; toolCall: ToolCall; partial: AssistantMessage }
{ type:"done"; reason: StopReason; message: AssistantMessage }
{ type:"error"; reason: StopReason; error: AssistantMessage }
```

### 5.2 `StreamOptions` 完整字段（本单元读取的部分）

```ts
interface StreamOptions {
  onPayload?: (payload: unknown, model: Model<Api>) => unknown | undefined | Promise<unknown | undefined>;
  onResponse?: (response: { status: number; headers: Record<string,string> }, model) => void | Promise<void>;
  onProviderStreamEvent?: (data: unknown, model) => void | Promise<void>;
  temperature?: number;
  samplingParams?: SamplingParams;         // Record<string, JsonValue>
  maxTokens?: number;
  transport?: Transport;
  cacheRetention?: CacheRetention;
  sessionId?: string;
  websocketConnectTimeoutMs?: number;
  metadata?: Record<string, unknown>;
  headers?: ProviderHeaders;
  identity?: ClientIdentity;
  timeoutMs?: number;
  maxRetries?: number;
  maxRetryDelayMs?: number;
  apiKey?: string;
  fetch?: typeof globalThis.fetch;
  signal?: AbortSignal;
  telemetry?: unknown;
  env?: ProviderEnv;                        // Record<string, string | undefined>
}
```

### 5.3 本单元向外部暴露的符号

| 模块 | 导出 |
|---|---|
| `lazy.ts` | `lazyStream`, `lazyApi`, `LazyStreamObserver`, `LazyApiCapabilities` |
| `anthropic-messages.ts` | `stream`, `streamSimple`, `AnthropicOptions`, `AnthropicEffort`, `AnthropicThinkingDisplay` |
| `openai-completions.ts` | `stream`, `streamSimple`, `convertMessages`, `OpenAICompletionsOptions`, `ConvertCompletionsMessagesOptions` |
| `openai-responses.ts` | `stream`, `streamSimple`, `OpenAIResponsesOptions` |
| `openai-responses-shared.ts` | `getResponsesToolSupport`, `convertResponsesMessages`, `convertResponsesTools`, `processResponsesStream`, `OpenAIResponsesStreamOptions`, `ConvertResponsesMessagesOptions`, `ConvertResponsesToolsOptions` |
| `openai-codex-responses.ts` | `stream`, `streamSimple`, `OpenAICodexResponsesOptions`, `OpenAICodexWebSocketDebugStats`, `getOpenAICodexWebSocketDebugStats`, `resetOpenAICodexWebSocketDebugStats`, `closeOpenAICodexWebSocketSessions` |
| `azure-openai-responses.ts` | `stream`, `streamSimple`, `AzureOpenAIResponsesOptions` |
| `azure-openai-config.ts` | `AzureEndpointOptions`, `resolveDeploymentName`, `resolveAzureBaseUrl`, `resolveAzureConfig` |
| `openai-decisions.ts` | `classify` |
| `openai-prompt-cache.ts` | `OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH`, `clampOpenAIPromptCacheKey` |
| `github-copilot-headers.ts` | `inferCopilotInitiator`, `hasCopilotVisionInput`, `buildCopilotDynamicHeaders` |
| `simple-options.ts` | `MIN_ANSWER_TOKENS`, `DEFAULT_THINKING_BUDGETS`, `clampMaxTokensToContext`, `resolveSamplingParams`, `buildBaseOptions`, `clampReasoning`, `thinkingBudgetForLevel`, `clampThinkingBudgetToAnswerRoom`, `adjustMaxTokensForThinking` |
| `transform-messages.ts` | `splitErrorToolResultImages`, `transformMessages`, `SplitErrorToolResult`（类型） |
| `constrained-sampling.ts` | `makeStrictJsonSchema`, `getJsonSchemaToolParameters`, `getGrammarToolInput`, `appendGrammarToolInputJsonDelta`, `closeGrammarToolInputJson`, `resolveJsonSchemaStrictSampling`, `resolveGrammarConstrainedSampling`, `createGrammarToolInputProperties`；类型 `UnsupportedStrictSchemaKeywordCheck`, `GrammarConstrainedSampling`, `GrammarToolInputJsonBuffer` |
| `classifier-shared.ts` | `ClassifierHttpError`, `isRecord`, `requiredNumber`, `postClassifierRequest`, `parseClassifierUsage` |
| `*.lazy.ts`（6 个本单元） | `anthropicMessagesApi`, `openAICompletionsApi`, `openAIResponsesApi`, `openAICodexResponsesApi`, `azureOpenAIResponsesApi`, `openAIDecisionsApi` |

注意 `makeJsonSchemaNodeStrict`、`UnsupportedStrictJsonSchemaError`、`isStructuredSchema`、
`schemaAllowsNull` **未导出**（Rust 侧可为 `pub(crate)`）。

### 5.4 上游单元必须在 Rust 侧提供的等价 API（供本单元 import）

**本表已按源码 import 逐条核对**（15 个 in-scope 模块 + 其 `.lazy.ts` 的全部 `import ... from "../..."`）。
只列**真正被 import 的符号**；不在表内的上游符号本单元不需要、Rust 侧也不应为本单元强加。

| 上游 TS 模块 | 需要的符号 | 本单元使用处 |
|---|---|---|
| `types.ts` | `Api`、`AssistantMessage`、`AssistantMessageEvent`、`CacheRetention`、`ChatTemplateKwargValue`、`ClassifierAnswer`、`ClassifierApi`、`ClassifierContext`、`ClassifierFunction`、`ClassifierModel`、`ClassifierOptions`、`ClassifierQuestion`、`ClassifierResult`、`ImageContent`、`JsonValue`、`Message`、`Model`、`ModelThinkingLevel`、`OpenAICompletionsCompat`、`OpenAIResponsesCompat`、`ProviderClassifier`、`ProviderEnv`、`ProviderHeaders`、`ProviderStreams`、`SamplingParams`、`SimpleStreamOptions`、`StopReason`、`StreamFunction`、`StreamOptions`、`SystemMessage`、`TextContent`、`TextSignatureV1`、`ThinkingBudgets`、`ThinkingContent`、`ThinkingLevel`、`ThinkingTokenBudgetField`、`Tool`、`ToolCall`、`ToolResultMessage`、`TranscriptContext`、`Usage` | 全部模块 |
| `utils/event-stream.ts` | `AssistantMessageEventStream`（`push`/`end`/`result`/`AsyncIterator` + `durationMs` 计时） | 全部 stream 模块 + `lazy.ts` |
| `utils/transcript.ts` | `normalizeContext`、`resolveTranscript`、`resolveTranscriptTools`、`getCurrentTools`、`getDeclaredTools`、`getInitialSystemMessage`、`TranscriptContext`、`TranscriptToolSupport` | completions / responses / responses-shared / codex / azure-responses |
| `utils/text.ts` | `getSystemMessageText`、`renderSystemMessageUpdate` | anthropic-messages / completions |
| `utils/streaming-json.ts` | `ToolCallArgumentsStream`（`append`/`replace`/`finish`/`text`） | anthropic-messages / codex |
| `utils/json-parse.ts` | `parseJsonWithRepair` | anthropic-messages |
| `utils/provider-retry.ts` | `retryProviderRequest`（本单元只调用它，不直接引用 `noRetryStatuses`/`abortableSleep`/`isRetryableProviderError`） | anthropic / completions / responses / codex / azure / classifier-shared |
| `utils/error-body.ts` | `normalizeProviderError`、`formatProviderError` | 除 lazy/simple-options/transform-messages 外几乎全部 |
| `utils/identity.ts` | `getIdentityHeaders`、`ClientIdentity` | anthropic / completions / responses / codex / azure-responses / classifier-shared |
| `utils/hash.ts` | `shortHash` | openai-completions（tool-call id 归一化） |
| `utils/uuid.ts` | `uuidv7` | completions / responses / codex 等 |
| `utils/sanitize-unicode.ts` | `sanitizeSurrogates` | anthropic / completions / responses / codex |
| `utils/headers.ts` | `headersToRecord`、`providerHeadersToRecord` | anthropic / completions / responses / codex / azure-responses / classifier-shared / openai-decisions |
| `utils/provider-env.ts` | `getProviderEnvValue` | anthropic-messages / azure-openai-config / 各 provider 头 |
| `utils/openrouter-cost.ts` | `isOpenRouterModel`、`applyOpenRouterReportedCost`、`OpenRouterUsageAccounting`（type） | anthropic / completions |
| `utils/diagnostics.ts` | `appendAssistantMessageDiagnostic`、`createAssistantMessageDiagnostic`、`formatThrownValue` | anthropic / completions / codex |
| `utils/abort-signals.ts` | `combineAbortSignals` | anthropic / codex |
| `utils/estimate.ts` | `estimateContextTokens` | **simple-options.ts**（`clampMaxTokensToContext`） |
| `env-api-keys.ts` | `ANTHROPIC_FEDERATION_RULE_ID_ENV`、`ANTHROPIC_IDENTITY_TOKEN_FILE_ENV`、`ANTHROPIC_ORGANIZATION_ID_ENV`、`ANTHROPIC_SERVICE_ACCOUNT_ID_ENV`、`ANTHROPIC_WORKSPACE_ID_ENV` | anthropic-messages（federation） |
| `session-resources.ts` | `registerSessionResourceCleanup` | **openai-codex-responses**（模块加载时注册 `closeOpenAICodexWebSocketSessions`） |
| `utils/node-http-proxy.ts` | `resolveHttpProxyUrlForTarget` | openai-codex-responses（WebSocket 代理） |
| `models.ts` | `calculateCost`、`clampThinkingLevel` | anthropic/completions/responses/codex/azure/classifier-shared |
| `ai-providers` | `providers/azure.ts` 的 `azureProvider()`（依赖本单元的 `resolveAzureBaseUrl`/`resolveDeploymentName`） | 下游装配层 |
| 第三方 | `@anthropic-ai/sdk`（`Anthropic`、`ClientOptions`、`Beta*` 类型）、`openai`（`AzureOpenAI`、`OpenAI`、`ChatCompletion*`、`Response*` 类型） | 见各模块；Rust 侧需自建等价 wire 类型 |

> **审计修正**：原表曾把 `collapseSystemMessages`、`getCurrentSystemMessage`、`getCurrentSystemPrompt`、
> `withoutInitialSystemMessage`、`getToolStateChanges`、`declarationsEqual`、`toToolDeclaration`、
> `repairJson`、`parseToolCallArguments`、`parseStreamingJson`、`StreamingJsonParser`、`formatUserAgent`、
> `getAttributionHeaders`、`getOpenCodeSessionHeaders`、`mergeClientIdentity`、`KIANA_ATTRIBUTION`、
> `truncateErrorText`、`safeJsonStringify`、`MAX_PROVIDER_ERROR_BODY_CHARS`、`isRetryableProviderError`、
> `abortableSleep`、`contentText`、`ownedSlice` 列为本单元依赖——**实测这些符号在 15 个 in-scope 模块中一次都没出现**，
> 属上游（`ai-utils-*`）内部实现细节，不应作为本单元的跨单元契约。另注：`openai-responses-shared.ts:753`
> 的 `contentText` 是**局部变量**（`item.content?.map(c => c.text).join("\n\n") || ""`），不是 `utils/text.ts` 的函数。

### 5.5 关键不变量（跨单元契约）

1. `stream()` 得到的 `AssistantMessageEventStream` 的 `partial` 字段是**同一个** `AssistantMessage`
   可变引用（TS 中可观察）；Rust 侧必须用 `Arc<RwLock<AssistantMessage>>` 或等价物，
   保证 `partial` 反映最新状态（TUI 会读它）。
2. `done` 事件的 `message` 与 `result()` 返回的值必须相等（同一对象/深相等）。
3. `error` 事件的 `error` 即 `result()` 的值。
4. `contentIndex` 是 `output.content` 的索引，且在流中**单调递增**（每开新块 +1）。
5. `usage.totalTokens` 在 OpenAI 系列里可能与分项之和不一致（Responses 直接用 provider 的
   `total_tokens`）；Anthropic/Completions 用分项之和。
6. `output.stopReason` 在 `done` 前必须非 `"pending"`。

---

## 6. npm 依赖 → Rust crate 映射

### 6.1 直接依赖（`packages/ai/package.json` 中与本单元相关的部分）

| npm 包 | 版本 | 用到的能力 | Rust crate 建议 | 行为差异与桥接 |
|---|---|---|---|---|
| `@anthropic-ai/sdk` | 0.129.0 | `Anthropic` 客户端、`client.beta.messages.create(...).asResponse()`、错误类型 `Anthropic.APIError`（`status`/`type`/`message`）、`withOptions`、`config`（federation） | **自建**：`reqwest` + 手工请求；无需 SDK | 必须复刻：`?beta=true` 查询参数、`anthropic-version: 2023-06-01`、`x-api-key` vs `Authorization: Bearer`、错误体解析（`type: "invalid_request_error"`、`status`）。federation 的 OIDC token 交换需自行实现（POST 到 Anthropic 的 token 端点；文件读取 + 刷新缓存） |
| `openai` | 7.19.0 | `OpenAI`/`AzureOpenAI`、`chat.completions.create().withResponse()`、`responses.create().withResponse()`、流式 SSE 解析 | **自建**：`reqwest` + SSE 解析；`AzureOpenAI` 仅需 URL/查询参数差异 | 必须复刻：默认头（`Authorization: Bearer`）、`OpenAI-Beta`（Responses 不需要）、错误体 `{error:{message,type,code}}`、`x-should-retry` 头语义（见 `retryProviderRequest`） |
| `partial-json` | 0.1.7 | `parse()` 容忍不完整 JSON | **自建**：本仓库已有 `StreamingJsonParser`（`utils/streaming-json.ts`），属上游单元；本单元只需 `ToolCallArgumentsStream` | `partial-json` 仅在 `parseStreamingJson` 的回退路径使用，本单元不直接用 |
| `typebox` | 1.3.27 | 仅用于 `compat-schema.ts` 的运行时 schema（不属于本单元的请求路径） | `serde` + `serde_json` + 手写校验，或 `schemars` | 本单元只需读 `model.compat` 的字段，不需要 schema 校验 |
| `@earendil-works/kiana-telemetry` | 1.4.0 | `options.telemetry` 透传 | 透传不透明值（`serde_json::Value` 或 `Arc<dyn Any>`） | 本单元不使用 |
| `http-proxy-agent` / `https-proxy-agent` | 9.1.0 / 9.1.0 | Codex WS 的 Bun 代理 | `reqwest` 内置代理；WS 用 `tokio-tungstenite` + `Proxy` | 仅 Bun 特殊路径；Rust 侧统一交给 `reqwest`/`tungstenite` |

### 6.2 建议的通用 Rust 依赖

| 用途 | crate | 备注 |
|---|---|---|
| 异步运行时 | `tokio`（`rt-multi-thread`, `macros`, `time`, `sync`, `signal`） | `AbortSignal` → `tokio_util::sync::CancellationToken` 或 `tokio::sync::watch` |
| HTTP | `reqwest`（`rustls-tls`, `stream`, `gzip`, `zstd`, `json`） | 关闭自动重试（`reqwest` 无内建重试，天然满足 `maxRetries: 0`） |
| SSE | 自建（约 120 行）或 `eventsource-stream` | 自建更可控：Anthropic 需要 `event:`/`data:`/`raw`；Codex 只要 `data:` |
| WebSocket | `tokio-tungstenite`（`rustls-tls-webpki-roots`） | 需支持自定义头、`close(code, reason)`、二进制帧 |
| zstd | `zstd` | 等级 3 |
| JSON | `serde`, `serde_json`（`preserve_order` 特性！） | **必须** `preserve_order` 以保留 `Map` 插入顺序（等价 JS 对象键序） |
| Base64 | `base64` | JWT 解码用 `STANDARD`（容忍无 padding：`STANDARD_NO_PAD` 也试一次） |
| URL | `url` | `Url::parse`、`path_segments`、`set_query(None)` |
| 时间 | `std::time::{Instant, SystemTime}` | `Date.now()` → `SystemTime::now().duration_since(UNIX_EPOCH)`；`performance.now()` → `Instant` |
| 随机 | `rand` | 重试抖动 `1 - rand*0.25`；UUID 随机字节 |
| UUID | `uuid`（`v7` 特性）**或**自建 | 自建更容易精确复刻 `uuidv7()` 的序列/单调语义 |
| 错误 | `thiserror`, `anyhow` | 库层用 `thiserror`，边界处转 `String` 进 `errorMessage` |
| 取消 | `tokio-util`（`CancellationToken`） | 组合多个取消源 → `CancellationToken::child_token` 或 `tokio::select!` |
| 结构化并发 | `futures`（`StreamExt`, `TryStreamExt`） | 事件流用 `async_stream::try_stream!` |
| 日志 | `tracing` | 不要输出到 stdout（库层） |
| 正则 | `regex`（不支持 lookbehind/backref） | `sanitizeSurrogates` 的 lookbehind 需手写；其余（`isTerminalRateLimitError` 等）`regex` 足够 |

**明确不需要**：`clap`、`crossterm`、`unicode-width`、`pulldown-cmark`、`similar`、`globset`、`rquickjs`、
`oxc`、`rusqlite`（本单元无 CLI/TUI/文件/JS 执行）。

### 6.3 必须桥接的 JS 语义差异

#### 6.3.1 字符串索引与长度（**高危**）

| 位置 | JS 语义 | Rust 风险 | 方案 |
|---|---|---|---|
| `clampOpenAIPromptCacheKey` | `Array.from` = **码点** | `&s[..64]` 会按字节切并 panic | `s.chars().take(64).collect::<String>()` |
| `normalizeToolCallId`（Anthropic） | `.replace(regex,"_").slice(0,64)` = UTF-16 code unit | 替换后全为 ASCII，等价于字节 | `s.chars()` 或字节均可（替换后 ASCII）；用 `chars()` 最安全 |
| `normalizeIdPart`（Responses） | `sanitized.length > 64` = UTF-16 | 同上（替换后 ASCII） | 同上 |
| `msgId.length > 64`（Responses） | UTF-16 | message id 通常 ASCII | 用 `chars().count()` 保守 |
| `normalizeToolCallId`（completions） | `id.length > 40`、`callId.slice(0, 40-hash-1)` | 同上 | 同上 |
| `truncateErrorText` | `text.length` = UTF-16 | 错误体可能含非 BMP 字符 | 用 `chars()` 计数与切片 |
| `estimateTextTokens` | `text.length / 3.5` = UTF-16 单元 | 影响 `maxTokens` 夹取，进而影响请求 | 必须用 **UTF-16 长度**：`s.encode_utf16().count()` |
| `JSON.stringify` 的 `slice(1,-1)`（grammar 输入） | 去掉首尾引号；转义非 ASCII 为 `\uXXXX`？——**不**，`JSON.stringify` 保留非 ASCII 原样，只转义控制字符与 `"`/`\` | Rust `serde_json::to_string` 也会转义控制字符，但**默认不转义非 ASCII**（一致）；注意 `/` **不**会被转义（JS 也不转义） | 用 `serde_json::to_string(&s)` 然后 `strip_prefix('"')`/`strip_suffix('"')` |

**实施建议**：在 `kiana` 的 `ai-utils-text` 中提供
`fn js_len(s: &str) -> usize { s.encode_utf16().count() }` 与
`fn js_slice(s: &str, start: usize, end: usize) -> Cow<str>`（按 UTF-16 索引，代理对边界处按 JS 行为
可能切开——`slice` 会切出孤立代理，`&str` 无法表示；实际调用点都是 ASCII，建议在 `js_slice` 中对
非 BMP 边界使用 U+FFFD 或断言，并在规范测试中标注）。

#### 6.3.2 JSON 键序

JS 对象键序规则：
1. **整数索引键**（`"0"`、`"42"`）按数值升序**排在前面**；
2. 其余字符串键按**插入顺序**。

影响：
- `JSON.stringify(params)` 的键序 → 影响 HTTP 请求体（对 provider 无影响，但**影响 Codex 的
  `requestBodiesMatchExceptInput` 相等判定**与 `responseInputsEqual`）。
- `JSON.stringify(tool.parameters)` 用于 `declarationsEqual`（上游单元）与 `estimateToolsTokens`。
- `JSON.stringify(reasoning_details)` 存入 `thinkingSignature`，之后要 `JSON.parse` 回来。

Rust 方案：
- `serde_json` 开启 `preserve_order`（内部用 `IndexMap`），键序 = 插入顺序；
- **但** JS 的整数索引键重排规则需要额外处理。本单元涉及的对象里，可能有纯数字键的是：
  - `tool.arguments`（模型的 JSON 参数，键名任意，含 `"0"`,`"1"`）；
  - `question.criteria` / `context.questions`（Decisions）、`sections`。
  若这些需要通过 `JSON.stringify` 比较（`responseInputsEqual`），必须模拟该排序。
  建议实现 `fn js_json_stringify(v: &Value) -> String`，对 object 排序：
  先输出 `array-index` 键（按 `u32` 升序，且长度/数值规范符合 JS 的 "array index" 定义：
  非负整数 < 2^32-1 且 `ToString(n) === key`），再输出其余键（插入序）。
- `serde_json::Value::Object` 用 `IndexMap` 可保插入序，但**插入时**就要按 JS 规则重排一次即可。

#### 6.3.3 数字格式化

- JS `JSON.stringify(1.0)` → `"1"`；`serde_json` 同样输出 `1.0` 若值是 `f64`。
  → 所有**整数值**必须用 `i64`/`u64` 表示（`max_tokens`、usage 计数、`index`、`limit` 等），
  避免 `1.0`。
- 大整数：JS `number` 是 f64，超过 2^53 会丢精度；`total_tokens` 等实际不会超。
- `probability`、`confidence`、`score` 可能是小数 → `f64`；JS `JSON.stringify(0.1)` → `"0.1"`，
  `serde_json` 的 Ryu 输出相同（都是最短往返表示）。**例外**：`1e21` 以上 JS 用指数、
  `1e-7` 以下 JS 用指数（`1e-7`），serde_json 也用指数但阈值可能不同 → 成本数值不会到这里。
- `usage.cost.*` 是 f64，只用于展示，不序列化进请求。

#### 6.3.4 `structuredClone`

`makeStrictJsonSchema` 用 `structuredClone(schema)`。对纯 JSON 值，`serde_json::Value::clone()` 等价。
差异：`undefined` 字段在 `structuredClone` 后**保留**（作为 `undefined`），而 JSON 值里不会有 `undefined`。
→ 若 Tool.parameters 来自 serde_json，天然无 undefined。

#### 6.3.5 `Object.hasOwn` / 原型链

- `Object.fromEntries` 对 `"__proto__"` 键：现代 JS 中 `Object.fromEntries([["__proto__", 1]])` 产生
  **自有属性**（不触发 setter）。`parseAnswers`/`choiceProbabilities` 使用它，因此
  `__proto__` 作为问题 ID 是安全的（`openai-decisions.test.ts` 有
  "preserves prototype-sensitive question IDs in answers"）。
- Rust `serde_json::Map` 无原型概念，天然安全。**验证点**：`__proto__`、`constructor`、
  `toString` 作为 problem ID 必须能往返。

#### 6.3.6 `Headers` 大小写不敏感

TS 用 `Headers`（Codex）与普通对象（Copilot/其他）。Rust 用 `http::HeaderMap`（不敏感）或
`IndexMap<String, String>` + 手动小写归一。注意：
- `providerHeadersToRecord` 输出保留**最后一次写入的原始大小写**；
- Codex 的 `buildBaseCodexHeaders` 用 `Headers.set/delete`，最终序列化时 Node 会**小写化**所有头名。
  → Rust 侧若直接用 `HeaderMap` 发送，`reqwest` 会按 `HeaderName` 规范化（小写），一致。

#### 6.3.7 正则方言

| 用法 | JS 正则 | Rust `regex` 是否支持 |
|---|---|---|
| `sanitizeSurrogates` | `/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g`（lookbehind + lookahead） | **不支持** lookaround → 手写 UTF-16 遍历 |
| `normalizeToolCallId` | `/[^a-zA-Z0-9_-]/g` | 支持 |
| `isTerminalRateLimitError` | `/.../i` | 支持（`(?i)`） |
| `rate.?limit|...` | `.` 不匹配换行 | 一致 |
| `usage_limit_reached|.../i` | 支持 | 支持 |
| `AZURE_OPENAI...` 无正则 | — | — |
| `baseUrl.replace(/\/+$/u, "")` | 支持 | `trim_end_matches('/')` |
| `AZURE baseUrl.replace(/\/+$/, "")` | 同上 | 同上 |

`sanitizeSurrogates` 的 Rust 实现建议：
```rust
pub fn sanitize_surrogates(input: &str) -> String {
    // Rust &str 永远是合法 UTF-8，不可能包含孤立代理。
    // 但输入可能来自 lossy 转换后的 U+FFFD，或来自含 \uD800 的 JSON 字符串（serde 会拒绝非法 UTF-8，
    // 但 JSON 允许 "\ud800" 转义 -> serde_json 会替换为 U+FFFD 或报错）。
    // 结论：对合法 &str，本函数是恒等映射；若上游保留了 WTF-8/代理信息，需在解码层处理。
    input.to_string()
}
```
**关键决策**：TS 的 `String` 可含孤立代理；Rust `String` 不能。因此必须在**JSON 解码边界**
（SSE data → `serde_json::Value`）处理：
- `serde_json` 对 `"\ud800"`（孤立代理转义）默认会**报错**（`lone leading surrogate in hex escape`）。
  需要在解析前把孤立代理转义替换为 `�` 或删除，以匹配 JS 的 `JSON.parse` 行为
  （JS 会生成含孤立代理的字符串，随后 `sanitizeSurrogates` 删除它）。
  → 建议：自建一个 `parse_json_js_compatible(&str)`，预处理 `\uD800-\uDBFF` 未配对的转义为 `""`。
  这与 `sanitizeSurrogates` 的净效果一致（因为所有出站文本都会过 `sanitizeSurrogates`）。

#### 6.3.8 `Date.now()` / `Date.parse`

- `Date.now()` → `SystemTime::now().duration_since(UNIX_EPOCH).as_millis() as i64`。
- `Date.parse(retryAfter)`（HTTP-date，如 `Wed, 21 Oct 2015 07:28:00 GMT`）→ `httpdate` crate。
- `Math.round((resets_at*1000 - Date.now())/60000)` → 注意 JS 的 `Math.round` 对 `.5` **向上**取整
  （向 +∞），Rust `f64::round` 也是远离零（对正数相同，负数不同：JS `-0.5` → `-0`，Rust `-1`）。
  此处为分钟数且 `max(0, ...)`，正数，安全。

#### 6.3.9 `Math.random` 抖动

`retryProviderRequest`：`exponentialDelay * (1 - Math.random() * 0.25)`。Rust 用 `rand::random::<f64>()`。
**验证不可逐位复现**——测试需允许抖动区间（`[0.75, 1.0] * base`）。

#### 6.3.10 `AbortSignal` / 超时

| JS | Rust |
|---|---|
| `options.signal` | `CancellationToken`（或 `tokio::sync::watch::Receiver<bool>`） |
| `AbortSignal.timeout(ms)` | `tokio::time::sleep` + `select!`，或 `CancellationToken` + `spawn` 定时取消 |
| `AbortSignal.any([a,b])` | `CancellationToken` 派生：`let child = parent.child_token(); tokio::spawn(async { a.cancelled().await; child.cancel(); })` |
| `signal.throwIfAborted()` | `if token.is_cancelled() { return Err(AbortError) }` |
| `signal.aborted` 检查 | `token.is_cancelled()` |

**语义细节**：`retryProviderRequest` 在 catch 里若 `signal.aborted`，抛 `AbortError`（`name = "AbortError"`，
`message = "Request aborted"`）——注意与各模块的 `"Request was aborted"`（带 `was`）不同！

#### 6.3.11 `TextDecoder` 流式解码

Rust 用 `String::from_utf8_lossy` 分段解码会破坏跨块的多字节字符。必须用
`encoding_rs::UTF_8.new_decoder()` 或自建"保留不完整尾字节"的缓冲。
**推荐**：累积 `Vec<u8>`，用 `std::str::from_utf8` 尝试解码，失败时保留末尾最多 3 字节
（`Utf8Error::valid_up_to`）。

#### 6.3.12 `structuredClone` / 引用相等

`OpenAIResponses` 的 `outputSlots` 用 `contentIndex` 而非对象引用，无问题。
`openai-completions` 用 `blocks.indexOf(block)` 与 `Map<ToolCall, State>`（**对象引用**作为键）。
Rust 侧必须改用显式 id/索引：
- `getContentIndex` → 记录 `contentIndex` 于槽位中；
- `Map<ToolCall, ToolCallState>` → `Map<usize /*contentIndex*/, ToolCallState>`。

这是本单元**最容易出错**的重构点之一。

#### 6.3.13 `Promise` 与 `async` 顺序

- `await options?.onPayload?.(...)` 在**每个**请求前调用；返回值 `undefined` 表示不替换（**包括**
  显式返回 `undefined` 与未定义该钩子）。
- `await options?.onProviderStreamEvent?.(event, model)` 在**每个**上游事件后、本模块处理前调用；
  若钩子抛错：
  - Anthropic/Completions/Responses/Azure：错误直接进入 catch（`error` 事件）。
  - Codex：包成 `ProviderStreamEventCallbackError`，**不触发** WS 重试/SSE 回退（但仍是 error 事件）。
- `lazyStream` 的 `observer.onEvent` 在 `target.push` **之后**同步调用。

#### 6.3.14 `Number.parseFloat` / `Number(...)` 差异

- `getRetryAfterDelayMs`（Codex）用 `Number(...)`：`Number("")` → `0`，`Number(" ")` → `0`，
  `Number("abc")` → `NaN`。Rust 用 `s.trim().parse::<f64>()`，空串会 `Err` → 与 JS 的 `0` 不同。
  **必须特判**：`trim()` 为空 → `0.0`。
- `retryProviderRequest.getRetryDelayMs` 用 `Number.parseFloat`：`parseFloat("abc")` → `NaN`，
  `parseFloat("12abc")` → `12`（**前缀解析**！）。Rust 需实现 `parse_f64_prefix`。
  且 `if (retryAfterMs)` 对 `"0"` 为真（非空字符串），对 `""` 为假。

#### 6.3.15 `atob`

Node 的 `atob` 接受标准 base64，**拒绝** `-`/`_`，容忍缺失 padding。Rust：
```rust
use base64::{engine::general_purpose::STANDARD, Engine};
let bytes = STANDARD.decode(s).or_else(|_| STANDARD_NO_PAD.decode(s))?;
```
**不要**用 `URL_SAFE`（与 Node `atob` 不同）。

#### 6.3.16 `Array.from(Set)` / `Set` 迭代序

JS `Set` 保持**插入顺序**。`unavailableTools` 的 `[...unavailableTools].join(", ")` 依赖此序。
Rust 用 `IndexSet`（`indexmap` crate）或 `Vec` + `HashSet` 双结构。

#### 6.3.17 `Object.entries` 顺序

同 6.3.2（整数键优先）。受影响的：`compat.chatTemplateKwargs`/`chatTemplateArgs`、
`question.criteria`、`context.questions`、`sections`、`headers`（`Object.entries(headers)` 用于
`anthropic-beta` 查找与 `mergeHeaders`）。

### 6.4 HTTP 层必须复刻的细节

1. **不做自动重定向**？——SDK 默认跟随重定向（`fetch` 默认 `follow`）。`reqwest` 默认 10 次重定向，
   一致。但重定向会丢弃 `Authorization`（跨域）——`reqwest` 行为一致。
2. **连接复用**：`reqwest::Client` 应**每个请求新建还是复用**？TS SDK 每次 `new OpenAI(...)`
   但底层用全局 `fetch`（Node undici 有连接池）。建议：`OnceLock<reqwest::Client>` 全局单例，
   以复用连接；但必须尊重 `options.fetch` 注入（Rust 侧用 `Arc<dyn HttpTransport>`）。
3. **`options.fetch` 注入点**：TS 允许调用方替换 `fetch`。Rust 提供
   `trait HttpTransport { async fn execute(&self, req: HttpRequest) -> Result<HttpResponse>; }`，
   测试用 mock 实现。
4. **压缩请求体**：`content-encoding: zstd` 只在 Codex SSE 路径；WS 发未压缩 JSON。
5. **`content-length`**：由 HTTP 客户端自动设置（`reqwest` 对 `Vec<u8>`/`String` body 自动）。

### 6.5 并发与资源

- **WebSocket 会话缓存**是模块级全局状态（`Map<session, Map<account, Conn>>`）。
  Rust 用 `static WEBSOCKET_SESSION_CACHE: LazyLock<Mutex<HashMap<...>>>`（或 `tokio::sync::Mutex`）。
  注意：`acquireWebSocket` 是 `async`，锁不能跨 `await` 持有 → 用 `std::sync::Mutex` 短临界区，
  或 `tokio::sync::Mutex` 并小心释放。
- **idle timer**：JS `setTimeout`。Rust 用 `tokio::spawn` + `sleep`，或保存 `JoinHandle` 并 `abort()`。
- **cleanup 注册**：`registerSessionResourceCleanup` → 全局 `Vec<Box<dyn Fn(Option<&str>) + Send + Sync>>`。
- **federation client 缓存**：单槽缓存（`key` + `fetch` 指针 + client）。Rust 用
  `Mutex<Option<FederationCache>>`；`fetch` 指针比较改为 `Arc::ptr_eq` 或 client id。

---

## 7. 测试清单

以下每个测试文件都必须有对应的 Rust 测试（`#[tokio::test]` / 纯函数 `#[test]`）。
"可离线"指无需真实 API key。

**用例计数口径**：`声明` = `it(...)` / `it.skipIf(...)` / `it.each(...)` 的个数；`用例` = 展开 `it.each` 矩阵后
实际运行的条数。下表是本单元相关测试文件的**实测**计数（`packages/ai/test/`，逐文件核对过源码），
Rust 侧测试数量应 **≥ 用例数**：

| 文件 | 声明 | 用例 | 备注 |
|---|---|---|---|
| `anthropic-sse-parsing` | 15 | 15 | 全离线 |
| `anthropic-mid-conversation-effort` | 10 | 14 | 含 `it.each(["low","medium","high","xhigh","max"])("preserves native effort %s")` |
| `anthropic-strict-tool-fallback` | 10 | 16 | 3 个 `it.each`：3 条 grammar 错误文案 / 3 种不重试工具集 / 3 种"不该降级"的 400 |
| `anthropic-strict-tool-schema` | 2 | 2 | |
| `anthropic-temperature-compat` | 6 | 6 | 4 个内置模型 + 2 个 custom |
| `anthropic-thinking-disable` | 8 | 8 | 第 8 条是**真实 API**（claude-sonnet-4-5，`retry:2`），需 `#[ignore]` |
| `anthropic-force-adaptive-thinking` | 5 | 5 | |
| `anthropic-empty-thinking-signature-compat` | 5 | 8 | 含 `it.each` 4 个 Fireworks 模型 |
| `anthropic-eager-tool-input-compat` | 3 | 3 | |
| `anthropic-cache-write-1h-cost` | 3 | 3 | |
| `anthropic-auth-token` | 10 | 10 | |
| `anthropic-federation` | 9 | 9 | |
| `anthropic-federation-sdk` | 2 | 2 | 需 mock token 端点 |
| `anthropic-tool-name-normalization` | 4 | 4 | `describe.skipIf(!oauthToken)` |
| `anthropic-error-tool-result-images` | 2 | 2 | |
| `anthropic-adaptive-thinking-models` | 1 | 1 | |
| `openai-completions-cache-control-format` | 4 | 4 | |
| `openai-completions-empty-tools` | 11 | 11 | |
| `openai-completions-prompt-cache` | 15 | 16 | 含 `it.each` 2 个 Fireworks 模型 |
| `openai-completions-provider-stream-event` | 3 | 3 | |
| `openai-completions-raw-stop-reason` | 2 | 2 | |
| `openai-completions-reasoning-details` | 6 | 6 | |
| `openai-completions-response-model` | 3 | 3 | |
| `openai-completions-retry` | 3 | 3 | |
| `openai-completions-thinking-as-text` | 3 | 3 | |
| `openai-completions-thinking-token-budget` | 10 | 11 | 含 `it.each(["thinking_budget","thinking_budget_tokens"])` |
| `openai-completions-tool-choice` | 49 | 49 | 无 `it.each` |
| `openai-completions-tool-result-images` | 3 | 5 | 含 `it.each(["","\r\n"," \n\t"])` 空白占位 |
| `openai-completions-vllm-priority` | 2 | 2 | |
| `openai-responses-compat` | 18 | 39 | 3 个 `it.each`：12/7/5 |
| `openai-responses-terminal-event` | 12 | 14 | 含 `it.each` 3 个 message phase 组合 |
| `openai-responses-namespace` | 5 | 5 | |
| `openai-responses-foreign-toolcall-id` | 1 | 1 | |
| `openai-responses-partial-json-cleanup` | 2 | 2 | |
| `openai-responses-empty-tool-result` | 1 | 3 | `it.each(["","\r\n"," \n\t"])` |
| `openai-responses-message-id` | 1 | 1 | |
| `openai-responses-mid-convo-effort` | 4 | 6 | 含 `it.each` 3 种"回落到 reasoning.effort"的组合 |
| `openai-responses-chatgpt-sign-in` | 3 | 4 | 含 `it.each` 2 种保留字段的调用方 |
| `openai-responses-usage-limit` | 2 | 2 | |
| `openai-responses-tool-result-images` | 4 | 4 | 2 个 describe 级 `skipIf` + 2 个 `it.skipIf` |
| `openai-codex-stream` | 31 | 40 | 5 个 `it.each`：3/4/2/3/2 |
| `azure-openai-base-url` | 17 | 17 | |
| `azure-openai-completions` | 15 | 15 | 8 + 5 + 2 |
| `azure-openai-responses-reasoning-replay` | 2 | 2 | |
| `azure-openai-tool-choice` | 2 | 2 | |
| `constrained-sampling` | 9 | 9 | |
| `openai-decisions` | 11 | 11 | |
| `lazy-module-load` | 5 | 5 | |
| `transform-messages-cross-model-thinking` | 2 | 2 | |
| `transform-messages-copilot-openai-to-anthropic` | 3 | 3 | |
| `sampling-options` | 11 | 16 | 3 个 `it.each`：3/3/2 |
| `max-thinking` | 4 | 12 | 2 个 `it.each`：5/5 |
| `xhigh` | 3 | 3 | |
| `supports-xhigh` | 28 | 41 | 4 个 `it.each`：7/5/3/2 |
| `cache-retention` | 19 | 32 | 4 个 `it.each`：6/4/5/2；另有 4 个 `it.skipIf` 需 key |
| `responseid` | 11 | 11 | 5 个 `it.skipIf`，全 E2E |
| `pre-generation-error` | 1 | 1 | 覆盖 7 个 API |
| `github-copilot-anthropic` | 3 | 3 | |
| `classifier-models` | 8 | 10 | 1 个 `it.each`（3 组 Jev 路由） |
| `abort` | 41 | 41 | 2 个 `it.skipIf`，全 E2E |
| `tool-call-without-result` | 30 | 30 | 4 个 `it.skipIf` |
| `tool-call-id-normalization` | 4 | 4 | 全 `it.skipIf`（OAuth/Copilot 凭据） |
| `fetch-option` | 6 | 6 | |
| `identity` | 11 | 11 | |
| `lax-message-content` | 1 | 1 | |
| `streaming-tool-arguments` | 4 | 4 | 含内存/GC 断言 |
| `xai-responses` | 11 | 11 | |
| `openrouter-reasoning-options` | 7 | 7 | |
| `provider-error-body-regression` | 5 | 5 | |
| `context-estimate` | 3 | 3 | |
| `telemetry-options` | 3 | 3 | |
| `telemetry` | 15 | 22 | 4 个 `it.each`：3/2/2/4 |
| `providers` | 30 | 30 | |
| `interleaved-thinking` | 4 | 4 | 真实 API |
| `radius-thinking-replay-e2e` | 3 | 3 | 全 `it.skipIf`，真实 API |
| `fireworks-models` | 19 | 25 | 3 个 `it.each`：2/3/4 |
| `fireworks-model-generation` | 4 | 4 | |
| `cloudflare-ai-binding` | 3 | 3 | |
| `openai-codex-oauth` | 10 | 10 | 跨单元（`auth/oauth`） |
| `openai-chatgpt-oauth` | 7 | 7 | 跨单元（`auth/oauth`） |
| `github-copilot-request-size-overflow` | 1 | 3 | `it.each` 413 nested / 413 flat / 400 |
| `classifier-images-live` | 3 | 8 | 2 个 `it.each`（Cloudflare 5 个模型 / OpenRouter 2 个模型），3 个 `describe.skipIf` |
| `openai-responses-reasoning-replay-e2e` | 3 | 3 | `describe.skipIf`，真实 API |
| `openai-codex-cache-affinity-e2e` | 1 | 1 | `it.skipIf(!codexToken)` |
| `openai-responses-cache-affinity-e2e` | 1 | 1 | `describe.skipIf` |
| `anthropic-opus-4-8-smoke` | 1 | 1 | `describe.skipIf`，`retry:2`、`timeout:30000` |
| `anthropic-thinking-binding-e2e` | 1 | 1 | `describe.skipIf`，`timeout:120000` |
| `xiaomi-token-plan-ams-anthropic-empty-signature-smoke` | 1 | 1 | `describe.skipIf`，`timeout:60000`、`retry:1` |
| `anthropic-eager-tool-input-e2e` / `anthropic-long-cache-retention-e2e` | 3 / 2 | **动态** | 文件内用 `for (const testCase of probeCases)` 在运行时生成 `it.skipIf(...)`，条数 = 当时 catalog 中该 provider 的 anthropic-messages 模型数；Rust 侧对应 `#[ignore]` 的参数化测试，至少保留"每个 provider 一个探针"的选取逻辑（`selectOneCasePerProvider` + `getProbePriority`） |
| `transcript-tool-changes` | 22 | 24 | 用 `test()`/`test.each` 而非 `it`；`test.each(["k3","k3-256k","kimi-for-coding"])` 展开 3 条（21 plain + 3 = 24）。**本单元相关**：Anthropic 原生 system 消息 + `tool_addition`/`tool_removal` 块、OpenAI `additional_tools`/tool-search 前缀、Kimi 顶层 tools 边界 |
| `system-message-replay` | 9 | 9 | `test()` 风格。**本单元相关**：`renderSystemMessageUpdate` 的"完整提示 vs 带框更新"文本、`declarationsEqual`、`getToolStateChanges`（上游 `ai-utils-transcript`，本单元只消费结果） |
| `anthropic-oauth` / `github-copilot-oauth` | — | — | **不属于本单元**（`ai-oauth` / provider 层），仅为对照 |

后续各小节按模块列出用例名与断言要点；若与上表计数冲突，以上表为准。
（`stream.test.ts` 233 条、`empty.test.ts` 120 条、`tool-call-without-result.test.ts` 30 条等"全 provider 矩阵"文件
只在 7.6 的 `fetch-option` / `pre-generation-error` 行之外做整体覆盖，**不逐条列出**；它们的断言全部是
"对该 provider 走真实 API 的端到端形状"，Rust 侧无需逐条复刻，只需保证相同断言在 `#[ignore]` E2E 中可跑。）

### 7.1 Anthropic 测试（`packages/ai/test/anthropic-*.test.ts`）

| 文件 | 用例 | 断言要点 | 离线 |
|---|---|---|---|
| `anthropic-sse-parsing.test.ts` | forwards parsed provider stream events in order | `onProviderStreamEvent` 收到的事件序列与原事件一致 | 是 |
| | uses the total reported by OpenRouter's Anthropic Messages endpoint | `applyOpenRouterReportedCost` 覆盖 `cost.total` | 是 |
| | keeps signed thinking replayable when a proxy relabels the model | `responseModel` 变更不影响 thinking 重放 | 是 |
| | uses a returned fallback model for cost attribution | `allowedFallbackModels` 命中 → 该 fallback 的 `cost` 参与计价 | 是 |
| | fails safely when Anthropic falls back after output begins | `content.length > 0` 后收到 `fallback` 块 → 错误文本 `Anthropic performed an unsupported mid-output model fallback` | 是 |
| | forces streaming after an onPayload replacement | `onPayload` 返回的 params 被强制加 `stream: true` | 是 |
| | omits the interleaved-thinking beta when thinking is disabled | `betas` 不含 `interleaved-thinking-2025-05-14` | 是 |
| | passes managed beta features to injected clients | 注入 `options.client` 时 `betas` 仍正确 | 是 |
| | uses the serving model input transformations from the final stream event | `message_delta.input_transformations` 覆盖 `message_start` 的 | 是 |
| | repairs malformed SSE JSON and malformed streamed tool JSON | `parseJsonWithRepair` + `argumentsError` | 是 |
| | preserves content from content_block_start events | `text`/`thinking` 初始内容保留 | 是 |
| | preserves refusal stop details from message_delta | `stop_details.explanation` 进 `errorMessage` | 是 |
| | preserves sensitive stop reasons | `Provider stopped with: sensitive` | 是 |
| | treats message_delta without usage as a no-op | usage 保留 message_start 值 | 是 |
| | ignores unknown SSE events after message_stop | 后续事件被跳过，不报错 | 是 |
| `anthropic-mid-conversation-effort.test.ts` | reconstructs an exact historical marker prefix and appends the current marker | `insertThinkingLevelMessages` 的 marker 位置与 effort 值 | 是 |
| | defaults omitted effort to high and still enables drop_block | `output_config.effort = "low"` + `thinking.block_binding.prefix_mismatch_behavior = "drop_block"` | 是 |
| | does not invent markers for legacy or other-provider assistants | 无 `providerThinkingLevel` 的 assistant 不产生 marker | 是 |
| | sends plain disabled thinking without effort when off is supported | `thinking = {type:"disabled"}` 且无 `output_config` | 是 |
| | keeps adaptive thinking when off is unsupported | `thinkingLevelMap.off === null` → 不 disabled | 是 |
| | leaves unsupported models on top-level effort | 无 `supportsMidConvoEffort` → 走 `output_config.effort = options.effort` | 是 |
| | sends the effort and binding beta headers | `betas` 含 `mid-conversation-output-config-2026-07-01` + `thinking-binding-controls-2026-08-01` | 是 |
| | generates exact model and transport gates | 见模型目录 | 是 |
| | supports off only on managed models that accept disabled thinking | — | 是 |
| `anthropic-strict-tool-fallback.test.ts` | downgrades inline additions and same-name redefinitions without downgrading require | 降级后 `strict` 消失，但 `require` 工具仍抛错 | 是 |
| | surfaces a second rejection rather than retrying again | 第二次 400 直接抛 | 是 |
| | does not downgrade a hook exception | `onPayload` 抛出的 400 形状错误不触发降级 | 是 |
| | honors cancellation before the fallback attempt | abort 后不再重试 | 是 |
| | does not retry an SSE error after response headers | 流已开始后不降级 | 是 |
| | does not disable strict sampling on later requests | `disablePreferredStrictTools` 是**单次请求**状态 | 是 |
| | preserves prefer through kiana-messages | 不适用（跨单元） | — |
| `anthropic-strict-tool-schema.test.ts` | only sends the full input schema for strict JSON-schema tools | strict 时 `input_schema` 含完整 schema + 强制字段 | 是 |
| | sends prefer tools non-strict when they use keywords Anthropic strict mode rejects | `minimum` 等 → 非 strict | 是 |
| `anthropic-temperature-compat.test.ts` | omits temperature for Claude Opus 4.7 / 4.8 / default 4.7 | `supportsTemperature=false` 时不发 | 是 |
| | keeps temperature for Claude Opus 4.6 / Sonnet 4.6 | 发 | 是 |
| | omits temperature for custom models with supportsTemperature disabled | 发 | 是 |
| `anthropic-thinking-disable.test.ts` | sends thinking.type=disabled for budget-based / adaptive / Opus 4.8 | `thinking = {type:"disabled"}` | 是 |
| | omits thinking.type=disabled for Claude Fable 5 | `thinkingLevelMap.off === null` → 不发 `thinking` | 是 |
| | uses adaptive thinking for Opus 4.8 / Sonnet 5 when reasoning enabled | `thinking.type = "adaptive"` | 是 |
| | maps xhigh reasoning to effort=xhigh for Opus 4.8 | `output_config.effort = "xhigh"` | 是 |
| `anthropic-force-adaptive-thinking.test.ts` | sends legacy thinking payload for custom model ids by default | `thinking.type = "enabled"` + `budget_tokens` | 是 |
| | sends adaptive thinking payload when compat.forceAdaptiveThinking is true | `adaptive` | 是 |
| | uses adaptive thinking with native xhigh effort for Fable 5 | `effort = "xhigh"` | 是 |
| | allows built-in adaptive models to opt out with forceAdaptiveThinking false | `enabled` | 是 |
| | preserves thinking.type=disabled when reasoning is off regardless of override | `disabled` | 是 |
| `anthropic-empty-thinking-signature-compat.test.ts` | converts empty-signature thinking to text by default | 转 `{type:"text"}` | 是 |
| | preserves empty thinking text when the signature is present | `{type:"thinking", thinking:"", signature}` | 是 |
| | preserves empty-signature thinking when allowEmptySignature is enabled | `{type:"thinking", signature:""}` | 是 |
| | allows empty thinking signatures for every Vercel AI Gateway model | 模型目录 `allowEmptySignature` | 是 |
| `anthropic-eager-tool-input-compat.test.ts` | sends per-tool eager_input_streaming by default | `eager_input_streaming: true` | 是 |
| | uses the legacy fine-grained tool streaming beta when disabled | `supportsEagerToolInputStreaming=false` → beta + 无字段 | 是 |
| | does not send the legacy beta when there are no tools | `betas` 无 | 是 |
| `anthropic-cache-write-1h-cost.test.ts` | prices the 1h portion at 2x input and the rest at the 5m rate | `cost.cacheWrite = (rate*short + rate*2*long)/1e6` | 是 |
| | prices 1h cache writes reported only in message_delta | delta 的 `cache_creation.ephemeral_1h_input_tokens` | 是 |
| | falls back to the 5m rate when no breakdown is reported | `cacheWrite1h = 0` | 是 |
| `anthropic-auth-token.test.ts` | resolves ANTHROPIC_AUTH_TOKEN as bearer / preserves ANTHROPIC_OAUTH_TOKEN / overrides | 见 `auth/` 层（跨单元）+ 本单元头构造 | 部分 |
| | Anthropic-compatible user agents: default kiana UA, explicit override, beta replacement/suppression | `User-Agent`、`anthropic-beta` | 是 |
| `anthropic-federation.test.ts` | resolves federation vars as provider env with no request auth | `getAnthropicFederation` 返回对象 | 是 |
| | passes ANTHROPIC_WORKSPACE_ID through | `workspace_id` | 是 |
| | not configured when a variable is missing | `undefined` | 是 |
| | treats ANTHROPIC_SERVICE_ACCOUNT_ID as optional | 缺省仍返回 | 是 |
| | keeps API key and auth token precedence over federation | 有 key → `undefined` | 是 |
| | hands the SDK a federation config instead of a key | client 构造参数 | 是 |
| | threads authContext federation variables through Models | 跨单元 | 部分 |
| | lets an explicit API key win over federation env | 是 |
| | does not federate other anthropic-messages providers | `provider !== "anthropic"` → `undefined` | 是 |
| `anthropic-federation-sdk.test.ts` | exchanges the identity token once across requests | 客户端缓存复用 | 是（需 mock token 端点） |
| | does not run the SDK credential chain for header-owned auth | 无 key 时不发凭据 | 是 |
| `anthropic-tool-name-normalization.test.ts` | todowrite ↔ TodoWrite 往返；内置工具名；find 不映射；自定义工具不变 | `toClaudeCodeName`/`fromClaudeCodeName` | 需 OAuth token（可用 `sk-ant-oat` 假 key 离线） |
| `anthropic-error-tool-result-images.test.ts` | moves the images after the tool results of the turn | `splitErrorToolResultImages` + `convertMessages` 顺序 | 是 |
| | keeps error tool results without images unchanged | `undefined` | 是 |
| `anthropic-adaptive-thinking-models.test.ts` | 见模型目录断言 | 目录 | 是 |
| `anthropic-oauth.test.ts` | OAuth 登录流（**不属于本单元**，属 `ai-oauth`） | — | — |
| `anthropic-eager-tool-input-e2e.test.ts` / `anthropic-long-cache-retention-e2e.test.ts` / `anthropic-opus-4-8-smoke.test.ts` / `anthropic-thinking-binding-e2e.test.ts` | 真实 API E2E | 需 `ANTHROPIC_API_KEY`，Rust 测试标 `#[ignore]` | 否 |
| `xiaomi-token-plan-ams-anthropic-empty-signature-smoke.test.ts` | 空签名 smoke | 需 key | 否 |

### 7.2 OpenAI Completions 测试

| 文件 | 用例 | 断言要点 | 离线 |
|---|---|---|---|
| `openai-completions-cache-control-format.test.ts` | applies Anthropic-style cache markers | system 首条 + 最后 tool + 最后对话消息 各挂 `cache_control` | 是 |
| | preserves for OpenRouter Anthropic batch aliases | `cacheControlFormat` 自动检测 | 是 |
| | moves the conversation cache marker to a tool result | 最后一条是 `tool` 时挂在其 text part | 是 |
| | omits when cacheRetention is none | 无标记 | 是 |
| `openai-completions-empty-tools.test.ts` | omits tools field when empty/undefined | `params.tools` 不存在 | 是 |
| | sends default / explicit maxTokens | `max_completion_tokens` 值 | 是 |
| | clamps default / explicit maxTokens to remaining context | `clampMaxTokensToContext` | 是 |
| | Cloudflare AI Gateway /compat 字段 | `max_tokens`、无 `store`、`system` 角色等 | 是 |
| | resolves Cloudflare base URL through provider auth | 跨单元（provider） | 是 |
| | preserves inline upstream Authorization（BYOK） | `defaultHeaders.Authorization` 与 `cf-aig-authorization` 并存 | 是 |
| | session affinity for Workers AI | `session_id`/`x-client-request-id`/`x-session-affinity` | 是 |
| | still emits tools: [] for Anthropic/LiteLLM proxy with tool history | `params.tools === []` | 是 |
| `openai-completions-prompt-cache.test.ts` | prompt_cache_key for api.openai.com | 有值 | 是 |
| | prompt_cache_retention 24h when long | 有值 | 是 |
| | clamps prompt_cache_key to 64 | 码点截断 | 是 |
| | omits when none | 两字段皆无 | 是 |
| | omits for non-OpenAI base URLs without long retention | 无 | 是 |
| | uses KIANA_CACHE_RETENTION | env | 是 |
| | session-affinity 各格式（openai / openai-nosession / openrouter / Baseten / 默认 OpenRouter / 禁用 / 显式头覆盖） | 头组合 | 是 |
| `openai-completions-provider-stream-event.test.ts` | exposes provider chunks incl. OpenRouter metadata | `onProviderStreamEvent` 收到原始 chunk | 是 |
| | OpenRouter total 不双计 | `applyOpenRouterReportedCost` | 是 |
| | BYOK 上游成本 | `is_byok === true` 时 `cost.total = reported.cost + upstream` | 是 |
| `openai-completions-raw-stop-reason.test.ts` | preserves raw finish reasons（成功与 error） | `output.rawStopReason` | 是 |
| `openai-completions-reasoning-details.test.ts` | preserves reasoning_details in thinking signature | `thinkingSignature = JSON.stringify(details)` | 是 |
| | falls back to encrypted tool-call signatures | `parseLegacyEncryptedReasoningDetail` | 是 |
| | preserves signed text/summary sequence | 合并规则 | 是 |
| | merges consecutive text/summary deltas | `appendOpenAIReasoningDetail` | 是 |
| | thinking before text in one delta（reasoning_content / reasoning_details） | 块顺序 | 是 |
| `openai-completions-response-model.test.ts` | surfaces routed chunk.model | `responseModel` | 是 |
| | leaves undefined when echo | 否 | 是 |
| | ignores empty/missing chunk.model | — | 是 |
| `openai-completions-retry.test.ts` | disables SDK retries by default | `maxRetries: 0` 传给 SDK | 是 |
| | honors provider retries | `retryProviderRequest` 次数 | 是 |
| | fails immediately when requested delay exceeds limit | 错误文本 `Server requested Ns retry delay (max: Ms). <msg>` | 是 |
| `openai-completions-thinking-as-text.test.ts` | serializes thinking-plus-text / thinking-only as assistant text parts | `requiresThinkingAsText` | 是 |
| `openai-completions-thinking-token-budget.test.ts` | 各用例 | `thinking_token_budget` 值 = `min(budget, ceiling-1024)`；`thinkingTokenBudgetField` 优先；`$var: "thinking.budget"` | 是 |
| `openai-completions-tool-choice.test.ts` | 见下方逐条 | 是 |

`openai-completions-tool-choice.test.ts` 的逐条断言（该文件覆盖了本模块大半的 compat 矩阵）。
该文件实际有 **49 个 `it` 声明**（无 `it.each`）；下面第 26、31、34 条在源码中是**多个独立用例**，已在下文用 `26a/26b/26c` 等标出，总数 44+5=49：

1. `forwards toolChoice from simple options to payload` — `tool_choice` 透传。
2. `includes toolChoice when no tools are provided` — 无 tools 也写 `tool_choice`。
3. `omits strict when compat disables strict mode` — `supportsStrictMode: false` 时工具无 `strict` 键。
4. `defaults unknown OpenAI-compatible endpoints to non-strict tools` — `strict` 为 `undefined` 时不发（或发 false，视 `supportsStrictMode`）。
5. `preserves strict tools for capable built-in Chat Completions models` — `strict: true`。
6. `maps Groq Qwen reasoning levels to default reasoning_effort` — `thinkingLevelMap` 映射。
7. `keeps normal reasoning_effort for groq models without compat mapping` — 原值。
8. `enables tool_stream for supported z.ai models with tools` — `compat.zaiToolStream` + 有 tools → `tool_stream: true`。
9. `stores z.ai tool_stream support in model compat metadata` — 目录（上游）。
10. `stores z.ai effort metadata` — 目录。
11. `maps z.ai GLM-5.2 thinking levels to reasoning_effort` — `thinking` + `reasoning_effort`。
12. `preserves z.ai thinking when replaying reasoning_content` — 重放时 `reasoning_content` 保留。
13. `omits z.ai GLM-5.2 reasoning_effort when thinking is off` — `thinking = {type:"disabled"}` 且无 effort。
14. `respects explicit z.ai tool_stream compat override` — `false` 时不发。
15. `omits tool_stream when no tools are provided` — 不发。
16. `maps non-standard provider finish_reason values to stopReason error` — `Provider finish_reason: <reason>`。
17. `ignores null stream chunks from openai-compatible providers` — null chunk 跳过。
18. `errors when a stream ends after only null finish_reason chunks` — `Stream ended without finish_reason`。
19. `accepts streams without finish_reason when compat disables it` — `supportsFinishReason: false` → 推断 `stop`/`toolUse`。
20. `ignores empty custom objects on function tool call deltas` — `custom: {}` 不当作 custom 工具。
21. `coalesces tool call deltas by stable index when provider mutates ids mid-stream` — `blocks_by_index` 优先。
22. `accumulates mixed content, reasoning, and parallel tool call deltas independently` — 多块并行。
23. `uses system messages for non-OpenAI/Anthropic OpenRouter reasoning model instructions` — `instructionRole = "system"`。
24. `keeps developer messages for OpenAI and Anthropic OpenRouter batch instructions` — `developer`。
25. `keeps developer messages for OpenAI reasoning model instructions` — `developer`。
26. `stores OpenRouter Kimi K2.6 reasoning replay compat in built-in metadata` — 目录。
26a. `stores Xiaomi MiMo reasoning replay compat in built-in metadata` — 目录。
26b. `stores Qwen Token Plan reasoning replay compat in built-in metadata` — 目录。
27. `replays Xiaomi MiMo assistant tool calls with empty reasoning_content when thinking is missing` — `requiresReasoningContentOnAssistantMessages` → `reasoning_content: ""`。
28. `normalizes OpenCode Go reasoning deltas to reasoning_content for replay` — `thinkingSignature = "reasoning_content"`。
29. `keeps non-OpenCode Go reasoning deltas on the original reasoning field` — `thinkingSignature = "reasoning"`。
30. `replays OpenCode Go reasoning thinking blocks as reasoning_content` — 重放时签名改写。
31. `sends thinking disabled for OpenCode Kimi K2.6 when thinking is off` — `thinking.type`。
31a. `sends thinking enabled for OpenCode Kimi K2.6 when thinking is enabled` — `thinking.type`。
32. `omits disabled thinking for Moonshot Kimi K2.7 Code models` — `thinkingLevelMap.off === null`。
33. `keeps disabled thinking for Moonshot Kimi K2.6 when thinking is off` — `disabled`。
34. `sends max_tokens for OpenCode completions models` — `maxTokensField = "max_tokens"`。
34a. `sends max_tokens for built-in and custom DeepSeek API models` — 同上。
34b. `sends max_tokens for Z.AI completions models` — 同上。
35. `omits reasoning effort for OpenCode Grok Build` — `supportsReasoningEffort: false`。
36. `does not double-count reasoning tokens in completion usage` — `output` 不减去 `reasoning`。
37. `preserves prompt_tokens_details cache read/write fields from chunk usage` — `parseChunkUsage`。
38. `preserves ... from choice usage fallback` — `choice.usage` 分支。
39. `uses OpenRouter reasoning object instead of reasoning_effort` — `reasoning.effort`。
40. `uses configurable chat template boolean thinking kwargs` — `chat_template_kwargs`。
41. `uses qwen chat template thinking kwargs` — `enable_thinking` + `preserve_thinking`。
42. `uses configurable chat template effort kwargs with static kwargs` — `$var`/静态值混合。
43. `uses Ant Ling compatibility metadata` — 目录 + `reasoning.effort`。
44. `omits Ant Ling reasoning for unmapped direct reasoning efforts and non-reasoning models` — 非 string 映射 → 不写。
| `openai-completions-tool-result-images.test.ts` | omits empty text parts from user messages with images | 过滤空 text | 是 |
| | batches tool-result images after consecutive tool results | `"Attached image(s) from tool result:"` | 是 |
| `openai-completions-vllm-priority.test.ts` | sends / omits `priority` | `compat.vllmPriority` | 是 |

### 7.3 OpenAI Responses 测试

| 文件 | 用例 | 断言要点 |
|---|---|---|
| `openai-responses-compat.test.ts` | 15 plain + `it.each`（12 模型 / 7 模型 / 5 服务层组合）→ **18 个声明 / 39 个用例** | 无 reasoning 时不发；`tool_choice`；Cloudflare strict；session affinity 各格式；prompt_cache_key 截断；cacheRetention none；`max_output_tokens` 开关 |
| `openai-responses-terminal-event.test.ts` | rejects streams ending before terminal event | 错误文本 |
| | rejects completed streams with unfinished tool call | 错误文本含 name + id |
| | rejects parallel tool calls without output_index | 混合调用被拒 |
| | forwards parsed provider stream events in order | `onProviderStreamEvent` |
| | emits error final result when wrapper ends early | `result()` 的 error 消息 |
| | replaces provisional final-answer stop with incomplete reason | `stopReason` 被 `incomplete` 覆盖 |
| | finalizes completed → stop / incomplete → length | `mapStopReason` |
| | content-filtered incomplete → non-retryable error | `Response incomplete: content_filter` |
| | unknown incomplete reasons preserved | 同上 |
| | rejects failed terminal events with provider error | `<code>: <message>` |
| `openai-responses-namespace.test.ts` | round-trips function/custom namespace（仅在 `output_item.done`） | `namespace` 保留 |
| | drops namespaces when target cannot replay load items | 跨模型 |
| | does not add namespace to ordinary calls | — |
| | omits an absent error message | `errorMessage` 字段不存在 |
| `openai-responses-foreign-toolcall-id.test.ts` | hashes foreign Copilot item IDs into `fc_<hash>` | `buildForeignResponsesItemId` |
| `openai-responses-partial-json-cleanup.test.ts` | removes partialJson at `output_item.done` | 最终块无中间态 |
| | marks malformed final arguments | `argumentsError` |
| `openai-responses-empty-tool-result.test.ts` | 空 tool result | `"(no tool output)"` |
| `openai-responses-message-id.test.ts` | unique fallback message IDs for multiple text blocks | `msg_kiana_<n>` / `msg_kiana_<n>_<k>` |
| `openai-responses-mid-convo-effort.test.ts` | keeps reasoning.effort constant and appends input on level change | `configuration_update` 位置 |
| | applies level change during tool loop | 下一轮 |
| | adds items only before replies recorded by this model | `isSameModel` |
| `openai-responses-chatgpt-sign-in.test.ts` | omits fields token sharing rejects | 无 `max_output_tokens`/`temperature`/`prompt_cache_retention`/`prompt_cache_options` |
| | omits prompt_cache_options on explicit-cache models | — |
| `openai-responses-usage-limit.test.ts` | links to ChatGPT usage（同步拒绝 / 流内失败） | `\nCheck your ChatGPT usage: https://chatgpt.com/settings/usage` |
| `openai-responses-tool-result-images.test.ts` | function_call_output 里的图片 | `input_image` + `detail: "auto"` |
| `openai-responses-cache-affinity-e2e.test.ts` / `openai-responses-reasoning-replay-e2e.test.ts` | 真实 API | `#[ignore]` |

### 7.4 Azure 测试

| 文件 | 用例 | 断言要点 |
|---|---|---|
| `azure-openai-base-url.test.ts` | 17 条：`normalizeAzureBaseUrl` **10 条** + `buildParams`/stream 7 条 | `normalizeAzureBaseUrl` 全部分支（根路径、`/openai`、`/openai/v1`、`/openai/v1/responses`、非 Azure 代理、query 处理、非法 URL） |
| | clamps prompt_cache_key / disables store / honors supportsStrictMode false / default URL from resource name | `buildParams` |
| | azure provider stream events | `onProviderStreamEvent` 顺序 |
| | azure user agent | `User-Agent` 可覆盖 |
| `azure-openai-completions.test.ts` | deepseek-v4-pro 8 条 | `reasoning_effort`、无 `thinking`、无 cache 参数、`system` 角色、保留中途 system、`reasoning_content` 重放 |
| | endpoint resolution 5 条 | baseURL、error 事件、model id、deployment 映射、`onPayload` 透传 |
| | api map 2 条 | 路由 |
| `azure-openai-responses-reasoning-replay.test.ts` | preserves / fills encrypted_content | `backfillReasoningSignatures` |
| `azure-openai-tool-choice.test.ts` | forwards provider-specific / neutral tool choice | `tool_choice` 与 `tools` 并存 |

### 7.5 Codex 测试（`openai-codex-stream.test.ts`）

该文件共 **26 个 `it` + 5 个 `it.each` 声明 = 31 个声明 / 40 个用例**（`it.each` 展开：3 模型 / 4 服务层组合 /
2 传输恢复 / 3 retry-after 变体 / 2 状态码）。下表按源码顺序列出全部 26 条 plain 用例，另在末尾补 5 个
`it.each` 矩阵与其展开值。

| 用例 | 断言要点 |
|---|---|
| streams SSE responses and forwards raw provider events | 事件顺序 + `onProviderStreamEvent` |
| processes a terminal SSE event without a trailing blank line | EOF 残帧处理 |
| completes after response.completed even when SSE body stays open | 收到终止事件即结束 |
| maps response.incomplete to stopReason length even when SSE body stays open | `stopReason` |
| aborts SSE fetch after the configured HTTP timeout when response headers do not arrive | `Codex SSE response headers timed out after <ms>ms` |
| aborts SSE body reads after response headers arrive | abort 中途 |
| sets session-id/x-client-request-id headers and prompt_cache_key when sessionId is provided | 头与 body |
| omits SSE cache affinity when cacheRetention is none | 无头无 key |
| clamps prompt_cache_key to OpenAI's 64-character limit | 码点（`clampOpenAIPromptCacheKey`） |
| clamps Codex session-id header to 64 characters | 码点（`session-id` 头单独一测） |
| lets model and caller headers override originator and User-Agent | 合并顺序（`originator` 可被 `model.headers` 覆盖；`Authorization` 不可） |
| preserves gpt-5.5 xhigh reasoning effort from simple options | `reasoning.effort = "xhigh"` |
| forwards required tool choice | `tool_choice = "required"` |
| sets Codex strict mode explicitly and honors constrained sampling | `strict: true/false/null` |
| does not set session-id/x-client-request-id headers when sessionId is not provided | 无 |
| forwards auto transport and raw provider events from streamSimple | — |
| scopes cached websockets to the authenticated account | `Map<account>` 键 |
| closes one-shot websockets when cacheRetention is none | 连接关闭 |
| falls back to SSE when websocket connect does not open before the connect timeout | `provider_transport_failure` 诊断 + SSE |
| reconnects once when the websocket connection limit is reached before output starts | 重试一次 |
| falls back to SSE when a websocket is idle before the first event | 回退 |
| errors when a websocket is idle after the stream started | 不回退，抛错 |
| opens a fresh cached websocket before the backend connection age limit | `connection_age_limit` |
| sends only response input deltas in websocket-cached mode | `previous_response_id` + 小 input |
| zstd-compresses SSE request bodies | `content-encoding: zstd` + 解压后 JSON 相等 |
| uses exponential backoff across repeated SSE retries without retry headers | 1000/2000/4000ms |

`it.each` 矩阵（源码中位于上表若干用例之间，Rust 侧应展开为参数化测试）：

| 矩阵 | 展开 | 断言要点 |
|---|---|---|
| `it.each(["gpt-5.3-codex","gpt-5.4","gpt-5.5"])("clamps %s minimal reasoning effort to low")` | 3 | 目录驱动：`minimal` → `low` |
| `it.each([[model, tier, multiplier]...])`（`gpt-5.1-codex flex 0.5` / `gpt-5.1-codex priority 2` / `gpt-5.5 flex 0.5` / `gpt-5.5 priority 2.5`）"uses the client-sent %s service tier for %s when Codex echoes default" | 4 | `resolveCodexServiceTier`：响应回 `"default"` 时采用请求 tier，并按 multiplier 计价 |
| `it.each(["websocket","sse"])("recovers a missing cached websocket continuation via %s")` | 2 | `previous_response_not_found` 后清 continuation 重试，WS/SSE 两条恢复路径都要覆盖 |
| `it.each([["retry-after-ms",…1500],["retry-after seconds","60"…60000],["retry-after HTTP date",…45000]])("uses %s for SSE retries")` | 3 | `getRetryAfterDelayMs` 三种头部形式 + 假时钟 |
| `it.each([429,503])("fails immediately when a %i retry delay exceeds the limit")` | 2 | `RetryDelayExceededError` 对 429 与 503 都生效 |

### 7.6 共享/纯函数测试

| 文件 | 用例 | 断言要点 |
|---|---|---|
| `constrained-sampling.test.ts` | converts supported constraints and falls back when unsupported | strict 转换 + prefer 回落 |
| | derives strict provider schemas without changing tool definitions | 不改原 `tool.parameters` |
| | falls back or rejects schemas that cannot be safely converted | 各错误文本 |
| | replays grammar calls as custom Responses items | `type: "custom_tool_call"` |
| | drops foreign item ids when replaying grammar calls | id 处理 |
| | keeps grammar input JSON deltas append-only | `appendGrammarToolInputJsonDelta` |
| | rejects a final grammar input that does not continue the streamed input | 非单调错误 |
| | streams custom tool input in time and memory linear in its length | 复杂度（大输入不超时） |
| | starts custom Responses tool calls with their initial input | `unsent` 机制 |
| `openai-decisions.test.ts` | maps questions to Decisions types and answers back by name | wire 形状 |
| | prices long-context requests at the long-context input rate | `cost.tiers` |
| | sends images after the state in one user message | input 数组形状 |
| | rejects more than 128 images before sending | 错误文本 |
| | fails the result when a question is refused and keeps the billed usage | `answers = {}` + `usage` 保留 |
| | returns missing and mistyped answers as classifier errors | 错误文本 |
| | preserves prototype-sensitive question IDs | `__proto__` 等 |
| | does not retry gateway timeouts | 504 不重试 + 说明文本 |
| | still retries other server errors | 500 重试 |
| | includes the API error body for other HTTP failures | `OpenAI Decisions error (<status>): <body>` |
| | rejects models for other classifier APIs and missing API keys | 错误文本 |
| `lazy-module-load.test.ts` | 5 条 | 按需加载（Rust：无初始化副作用） |
| `transform-messages-cross-model-thinking.test.ts` | does not send another model's thinking as text | 丢弃 |
| | keeps thinking only from target model when alternating | 保留 |
| `transform-messages-copilot-openai-to-anthropic.test.ts` | removes thoughtSignature on migration | `delete thoughtSignature` |
| | adds synthetic tool results for trailing orphaned calls | `"No result provided"` |
| | adds synthetic results only for still-missing results | 已有结果不重复 |
| `tool-call-without-result.test.ts` | 各 provider | 合成结果（多为 E2E，标 `#[ignore]`） |
| `tool-call-id-normalization.test.ts` | Live Handoff / Prefilled Context | id 归一化（多为 E2E） |
| `sampling-options.test.ts` | merges request sampling params；omits when none；through streamSimple；effective thinking level；off；stream-option 覆盖 thinking-level；override named fields；ignored by non-OpenAI APIs | `resolveSamplingParams` + `buildBaseOptions` |
| `max-thinking.test.ts` | is opt-in for ordinary reasoning models；supports a hole between high and max | `getSupportedThinkingLevels` / `clampThinkingLevel`（上游单元） |
| `xhigh.test.ts` | gpt-5.5 支持；gpt-5-mini 不支持（responses/completions 报错） | 目录 + `clampThinkingLevel`（E2E，标 `#[ignore]`） |
| `supports-xhigh.test.ts` | 目录断言 | 上游 |
| `cache-retention.test.ts` | Anthropic **7**（5 plain + 2 `it.skipIf` 需 key）/ Responses **16**（4 plain + 2 skipIf + `it.each` 6 模型 + `it.each` 4 模型）/ Completions **9**（2 plain + `it.each` 5 opencode + `it.each` 2 cerebras）→ 共 **19 个声明 / 32 个用例** | `KIANA_CACHE_RETENTION`、`ttl`、`prompt_cache_retention`、`prompt_cache_options`、`cache_control` 有无 |
| `responseid.test.ts` | 各 provider 暴露 `responseId` | E2E，`#[ignore]` |
| `pre-generation-error.test.ts` | 7 个 API 缺鉴权同步抛 `No API key for provider: test-provider` | **重要**：`streamSimple` 同步抛错 |
| `github-copilot-anthropic.test.ts` | Copilot Claude 的 adaptive effort 覆盖 + Bearer 头 + payload | `X-Initiator`/`Openai-Intent`/`Authorization` |
| `classifier-models.test.ts` | 7 plain + `it.each`（3 组 Jev 路由）→ 10 用例 | classifier 路由（跨单元） |
| `openai-codex-cache-affinity-e2e.test.ts` / `classifier-images-live.test.ts` | 真实 API | `#[ignore]` |
| `abort.test.ts` | 各 provider 的中止/立即中止/中止后新消息 | E2E，`#[ignore]` |
| `github-copilot-request-size-overflow.test.ts` | 413（嵌套/扁平 error 体）→ 上下文溢出，400 → 不溢出 | `isContextOverflow` 判定 + `errorMessage` 含 `(413)` 与 `failed to parse request`；`maxRetries: 0` 时不重试；`usage.totalTokens === 0` |
| `github-copilot-oauth.test.ts` | OAuth 登录 / 模型目录 / `proxy-ep` 派生 baseUrl | 跨单元（`auth/oauth`、`models`、`providers/github-copilot`）；本单元只需 `github-copilot-headers` 的头组合 |
| `fetch-option.test.ts` | 6 条 | `options.fetch` 注入是否贯穿：Anthropic SDK（1）、OpenAI SDK 三适配器 completions/responses/azure（1，循环 3 次）、Mistral+Codex SSE+kiana-messages（1）、Google 适配器拒绝自定义 fetch（1）、Google 接受 `globalThis.fetch`（1）、图片生成（1）。**本单元相关**：前 3 条中所有 in-scope 适配器都必须只用 `options.fetch`，绝不调用 ambient `globalThis.fetch`；Codex 的 `fetch` 注入点在 SSE 分支 |
| `identity.test.ts` | 11 条 | `User-Agent` 形态（detailed/short/custom/browser）、`mergeClientIdentity` 逐字段、无 attribution 时不发头、OpenRouter/NVIDIA（首字母大写）/OpenCode attribution、gateway 上的 NVIDIA 模型不归属、identity 位于 model/request 头之下、模型 "impersonating" 的 `User-Agent` 压过 identity、request identity 合并到 Models/wrapper、`x-opencode-session` 发给指向 OpenCode 的自定义 provider |
| `streaming-tool-arguments.test.ts` | 4 条 | 增量 `toolcall_delta` 的参数快照不得泄漏到 `partial` 之外的流状态；参数文本线性内存；调用方丢弃后释放 stream/partial/快照；中途截断的 error 消息上不留流状态。**本单元相关**：`ToolCallArgumentsStream`（anthropic/completions）与 `processResponsesStream` 的 `customInput` 缓冲 |
| `xai-responses.test.ts` | 11 条 | xAI 目录裁剪、全部走 Responses、Grok 4.7 能力与长上下文定价、`/responses` + bearer + xAI 字段、无 effort 也请求 encrypted reasoning、xhigh、Grok 4.3、Responses/Completions 默认 `User-Agent`（`kiana`）与显式头覆盖 |
| `openrouter-reasoning-options.test.ts` | 7 条 | OpenRouter reasoning 元数据生成（mandatory/不支持 effort 标记不可用、off 是否可选、无 effort 元数据时仍判 off 不可用、可选模型只有受支持 effort）；运行时：后台调用不带 reasoning、显式受支持 effort 仍发、可选模型仍显式 `reasoning: {effort:"none"}`。**本单元相关**：completions 的 `reasoning` 对象分支 |
| `provider-error-body-regression.test.ts` | 5 条 | 真实 SDK 错误路径下 `errorMessage` 必须同时含 status 与 body：openai-completions（body-blind，含 OpenRouter `metadata.raw` 不重复追加）、openai-responses（status-only）、Bedrock 两条（与本单元无关） |
| `context-estimate.test.ts` | 3 条 | `estimateContextTokens` 的 3.5 字符/token 预留、插入更早消息后旧 assistant usage 失效、新回合后重新采用。**本单元相关**：`clampMaxTokensToContext` |
| `telemetry-options.test.ts` | 3 条 | `telemetry` 选项在 request option 各面与 simple-stream 转换中继承、经 Models 的 stream/deferred 分发存活、经直连与 Models 的图片分发存活。**本单元相关**：`buildBaseOptions` 必须透传 `telemetry` |
| `providers.test.ts` | 30 条（4 个 describe） | provider 注册/模型目录/`lazyApi` 能力声明（`lazily exposes only declared deferred capabilities`）、混合 API provider 的 `model.api` 分发、env 合并、deferred 的 fetch/cancel 应用、无实现 API 产生 stream error、动态刷新竞态等。**本单元相关**：`lazyApi`/`lazyStream` 语义与 5.3 导出 |
| `telemetry.test.ts` | 11 plain + 4 `it.each`（3/2/2/4）→ 22 用例 | telemetry span/metrics（`lazyStream` 的 push/result 观测、有界错误消息、deferred 轮询用量只在终态记录）。**本单元相关**：`lazyStream` 的 observation 顺序 |
| `interleaved-thinking.test.ts` | 4 条（Opus 4.5/4.6 × 2 段 describe） | 真实 API 的 interleaved thinking 多轮工具循环；需 `ANTHROPIC_API_KEY` / Bedrock 凭据（`#[ignore]`） |
| `radius-thinking-replay-e2e.test.ts` | 3 条（全部 `it.skipIf`） | Radius 代理会丢弃重放的 Anthropic thinking 块：anthropic 直连作对照，radius 在"保留 thinking"与"客户端提供 level"两种情形下都要能续跑；验证 `providerThinkingLevel` + `drop_block`。真实 API，`#[ignore]` |
| `lax-message-content.test.ts` | 1 条 | 非类型化调用方传入 `content: null/undefined` 时 `transformMessages` 规范化为 `[]` 而不是崩溃 |
| `fireworks-models.test.ts` | 16 plain + 3 `it.each`（2/3/4）→ 25 用例 | Fireworks 的 Anthropic 兼容 compat：`x-session-affinity` 开关（cacheRetention none 时关闭）、OpenRouter 只发 `x-session-id`、`supportsCacheControlOnTools=false` 与 `supportsEagerToolInputStreaming=false`、`forceAdaptiveThinking` + 原生 effort 列表、Kimi K3 走 completions、GLM 5.3 Fast 对齐、`FIREWORKS_API_KEY` |
| `fireworks-model-generation.test.ts` | 4 条 | 模型目录生成器：上游 effort/toggle 元数据合并、新条目自动获得原生 effort、**不**从 toggle/budget/缺失元数据推断 adaptive thinking、上游 effort 覆盖固定 map |
| `cloudflare-ai-binding.test.ts` | 3 条 | `createAiBindingFetch` 的 binding 直通、构造期拒绝无 `fetch()` 的 binding、`null` 鉴权头与 SDK 占位 token 组合时占位鉴权不上线。**本单元相关**：completions 在 `Authorization`/`cf-aig-authorization` 头存在时用 `apiKey: "unused"`，不得把 `unused` 发上线 |
| `openai-codex-oauth.test.ts` | 10 条 | **跨单元**（`auth/oauth/openai-codex`）：设备码流、浏览器优先、取消、15 分钟超时、403/404 视为 pending、错误体透出、刷新失败不写 stderr、固定回调端口被占时回退到粘贴 redirect URL |
| `openai-chatgpt-oauth.test.ts` | 7 条 | **跨单元**（`auth/oauth/openai-chatgpt`）：user-owned client 注册、issued client ID/scope 校验、拒绝未授予 direct token use、device ID 必需、refresh 必须轮换 refresh token、带凭据刷新 |

### 7.7 移植时的测试基础设施需求

Rust 测试需要：
1. **Mock HTTP transport**（替代 `options.fetch`）：可编程返回 SSE 字节流（分块、含 CRLF、含残帧）。
2. **Mock WebSocket**：可编程 `open`/`message`/`error`/`close`，可控 `readyState` 与 `close(code, reason)`。
3. **假时钟**：`tokio::time::pause()` + `advance()` 用于重试/超时/idle 测试。
4. **夹具**：把 TS 测试里的 SSE 字符串原样搬过来（保持一致的分块边界）。
5. **快照**：对 `params` JSON 做 `assert_json_eq`（考虑键序的用 `assert_eq!(serde_json::to_string(...))`）。
6. **`it.skipIf` / `describe.skipIf` / 真实 API 用例的映射**：TS 里凡以 `skipIf(!process.env.X)`、
   `skipIf(!oauthToken)`、`skipIf(!hasAzureOpenAICredentials())` 守卫，或直接打真实端点的用例
   （`E2E`），在 Rust 中一律写成 **`#[ignore = "<需要哪个环境变量>"]`**，默认 `cargo test` 不跑、CI 也不跑。
   计数上仍应保留这些测试函数（数量见第 7 节上表），否则"离线用例数"与"总用例数"会混淆。
   具体需要 `#[ignore]` 的文件（共 19 个）：`anthropic-thinking-disable`（仅第 8 条）、
   `anthropic-tool-name-normalization`、`anthropic-eager-tool-input-e2e`、`anthropic-long-cache-retention-e2e`、
   `anthropic-opus-4-8-smoke`、`anthropic-thinking-binding-e2e`、
   `xiaomi-token-plan-ams-anthropic-empty-signature-smoke`、`responseid`、`abort`、`tool-call-without-result`、
   `tool-call-id-normalization`、`cache-retention`（4 个 `it.skipIf`）、
   `openai-responses-tool-result-images`（4 条全部跳过）、`openai-codex-cache-affinity-e2e`、
   `openai-responses-cache-affinity-e2e`、`openai-responses-reasoning-replay-e2e`、
   `radius-thinking-replay-e2e`、`interleaved-thinking`、`classifier-images-live`。
   其余"名义上离线"但依赖 OAuth 凭据的（`anthropic-tool-name-normalization`）可在 Rust 里用假
   `sk-ant-oat` key 离线化——见第 7 节 7.1 的说明。

---

## 8. Rust 模块布局与最难部分

### 8.1 workspace / crate 布局建议

```
kiana/
  crates/
    ai/                                  # 对应 packages/ai，crate 名 `kiana-ai`
      src/
        lib.rs
        types.rs                         # 上游单元 ai-core-types
        event_stream.rs                  # 上游单元
        models/
          mod.rs
          cost.rs                        # calculateCost
          thinking.rs                    # clampThinkingLevel / getSupportedThinkingLevels
          estimate.rs                    # estimateContextTokens
          catalog/                       # 生成的模型目录
        utils/
          mod.rs
          text.rs  transcript.rs  streaming_json.rs  json_parse.rs
          retry.rs errors.rs identity.rs headers.rs hash.rs uuid.rs
          sanitize.rs env.rs openrouter.rs diagnostics.rs abort.rs
        api/
          mod.rs
          lazy.rs                        # ← 本单元
          anthropic_messages.rs          # ← 本单元（约 1700 行 TS → ~2200 行 Rust）
          # 文件规模实测（TS 原始行数 → 建议 Rust 文件拆分见右）：
          #   文件                         TS 实到行数   Rust 建议拆分
          #   anthropic-messages.ts          1690       anthropic/{sse,convert,stop,client}.rs
          #   openai-completions.ts          1728       completions/{compat,convert,params,usage,reasoning_details}.rs
          #   openai-codex-responses.ts      1719       codex/{body,sse,ws,session_cache,headers,errors,compress}.rs
          #   openai-responses-shared.ts      891       responses_shared/{convert_messages,convert_tools,stream,text_signature}.rs
          #   openai-responses.ts             453       openai_responses.rs
          #   constrained-sampling.ts         313       constrained_sampling.rs
          #   azure-openai-responses.ts       255       azure_openai_responses.rs
          #   transform-messages.ts           256       transform_messages.rs
          #   openai-decisions.ts             197       openai_decisions.rs
          #   classifier-shared.ts            131       classifier_shared.rs
          #   lazy.ts                         123       lazy.rs
          #   simple-options.ts               109       simple_options.rs
          #   azure-openai-config.ts          107       azure_config.rs
          #   github-copilot-headers.ts        37       copilot_headers.rs
          #   openai-prompt-cache.ts            8       prompt_cache.rs
          #   合计                           8017
          anthropic/
            sse.rs
            convert.rs                   # convertMessages / convertTools / buildParams
            stop.rs
            client.rs                    # 客户端/头/federation
          openai_completions.rs          # ← 本单元
          completions/
            compat.rs                    # detectCompat / getCompat
            convert.rs                   # convertMessages / convertTools / cache_control
            params.rs                    # buildParams + thinkingFormat 分支
            usage.rs                     # parseChunkUsage / mapStopReason
            reasoning_details.rs
          openai_responses.rs            # ← 本单元
          openai_responses_shared.rs     # ← 本单元
          responses_shared/
            convert_messages.rs          # convertResponsesMessages
            convert_tools.rs
            stream.rs                    # processResponsesStream
            text_signature.rs
          openai_codex_responses.rs      # ← 本单元
          codex/
            body.rs                      # buildRequestBody / URL 解析
            sse.rs  ws.rs                # parseSSE / parseWebSocket
            session_cache.rs             # WS 缓存 + debug stats
            headers.rs  errors.rs        # parseErrorResponse / extractAccountId
            compress.rs                  # zstd
          azure_openai_responses.rs      # ← 本单元
          azure_config.rs                # ← 本单元
          openai_decisions.rs            # ← 本单元
          classifier_shared.rs           # ← 本单元
          simple_options.rs              # ← 本单元
          transform_messages.rs          # ← 本单元
          constrained_sampling.rs        # ← 本单元
          prompt_cache.rs                # ← 本单元
          copilot_headers.rs             # ← 本单元
      tests/
        anthropic_sse.rs  anthropic_convert.rs  anthropic_strict.rs
        anthropic_effort.rs  anthropic_thinking.rs  anthropic_cache.rs
        completions_compat.rs  completions_convert.rs  completions_thinking.rs
        completions_usage.rs  completions_cache_control.rs
        responses_convert.rs  responses_stream.rs  responses_effort.rs
        codex_sse.rs  codex_ws.rs  codex_retry.rs  codex_cache.rs
        azure_config.rs  azure_params.rs
        decisions.rs  constrained_sampling.rs  transform_messages.rs
        lazy.rs  prompt_cache.rs  copilot_headers.rs  simple_options.rs
```

### 8.2 关键类型设计

#### 8.2.1 事件流

```rust
pub struct AssistantMessageEventStream {
    inner: EventStream<AssistantMessageEvent>,
    started_at: SystemTime,           // Date.now()
    started_at_mono: Instant,         // performance.now()
}

impl AssistantMessageEventStream {
    pub fn push(&self, event: AssistantMessageEvent);   // done/error 时补 durationMs
    pub fn end(&self, result: Option<Arc<AssistantMessage>>);
    pub fn result(&self) -> impl Future<Output = Arc<AssistantMessage>>;
}
impl Stream for AssistantMessageEventStream { type Item = AssistantMessageEvent; }
```

`partial` 字段：`Arc<AssistantMessage>`，但内容会变化 → 建议 `Arc<RwLock<AssistantMessage>>`，
事件里存 `Arc<RwLock<AssistantMessage>>`；`done.message` 与 `error.error` 也存同一类型（或快照）。
**注意**：TS 中 `partial` 与最终 `message` 是同一对象；Rust 若要逐字节等价的外部观察，
必须保证 `done.message` 是同一 `Arc` 的最新快照。

#### 8.2.2 传输抽象

```rust
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn post(&self, req: HttpRequest) -> Result<HttpResponse, TransportError>;
}
pub struct HttpResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: BoxStream<'static, Result<Bytes, TransportError>>,
}
```

#### 8.2.3 取消

```rust
#[derive(Clone)]
pub struct CancelToken(tokio_util::sync::CancellationToken);
impl CancelToken {
    pub fn is_aborted(&self) -> bool;
    pub fn throw_if_aborted(&self) -> Result<(), AbortedError>;
    pub fn timeout(ms: u64) -> Self;                 // AbortSignal.timeout
    pub fn any(tokens: Vec<Self>) -> Self;           // AbortSignal.any
}
```

#### 8.2.4 参数构建（Anthropic）

```rust
pub struct AnthropicParams {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub max_tokens: u32,
    pub stream: bool,
    pub betas: Option<Vec<String>>,
    pub system: Option<Vec<serde_json::Value>>,
    pub temperature: Option<f64>,
    pub tools: Option<Vec<serde_json::Value>>,
    pub thinking: Option<serde_json::Value>,
    pub output_config: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    pub tool_choice: Option<serde_json::Value>,
    pub fallbacks: Option<Vec<serde_json::Value>>,
}
```
序列化时必须**省略 `None`** 且键序与 TS 一致（`model, messages, max_tokens, stream, betas, system,
temperature, tools, thinking, output_config, metadata, tool_choice, fallbacks` —— 实际 TS 顺序见源码
`params` 字面量顺序）。

#### 8.2.5 流式解析器复用

本单元依赖上游的 `ToolCallArgumentsStream`：
```rust
pub struct ToolCallArgumentsStream { text: String, parser: StreamingJsonParser, snapshot: Option<Value>, arguments: Map<String, Value> }
impl ToolCallArgumentsStream {
    pub fn append(&mut self, delta: &str) -> Map<String, Value>;
    pub fn replace(&mut self, text: &str) -> (Option<String>, Map<String, Value>);
    pub fn finish(self) -> (Map<String, Value>, Option<String>);  // (arguments, argumentsError)
}
```

#### 8.2.6 OpenAI completions 的块状态（替代 Map<对象引用>）

```rust
enum ToolCallInput {
    Json(ToolCallArgumentsStream),
    Custom { property: String, buffer: GrammarToolInputJsonBuffer },
}
struct ToolCallState { stream_index: Option<u32>, input: ToolCallInput }

struct CompletionsStreamState {
    text_block: Option<usize>,          // contentIndex
    thinking_block: Option<usize>,
    blocks_by_index: HashMap<u32, usize>,   // stream index -> contentIndex
    blocks_by_id: HashMap<String, usize>,
    states: HashMap<usize, ToolCallState>,  // contentIndex -> state
    has_finish_reason: bool,
    streamed_reasoning_details: Option<Vec<Value>>,
}
```

### 8.3 实现顺序建议（依赖驱动）

1. `prompt_cache.rs`、`copilot_headers.rs`、`simple_options.rs`、`transform_messages.rs`（纯函数，先测）。
2. `constrained_sampling.rs`（纯函数，测错误文本）。
3. `azure_config.rs`（纯函数 + URL）。
4. `classifier_shared.rs` + `openai_decisions.rs`（第一个端到端 HTTP 模块，最简单）。
5. `openai_completions.rs`（compat 矩阵 + 块状态）。
6. `openai_responses_shared.rs` → `openai_responses.rs`。
7. `azure_openai_responses.rs`（复用 6）。
8. `anthropic_messages.rs`（自建 SSE、federation、strict 降级、effort marker）。
9. `openai_codex_responses.rs`（WS + zstd + 会话缓存）。
10. `lazy.rs`（最后接线；Rust 中主要是 API 形状对齐）。

### 8.4 最难复刻的点（按风险排序）

1. **`processResponsesStream` 的槽位/未完成工具调用状态机**：`outputSlots: Map<output_index, slot>`、
   `unfinishedToolCalls: Set<ToolCall>`（对象引用！）、`output_item.done` 可能替换/新建槽位、
   `finalizeResponse` 与 `output_item.done` 的顺序耦合。Rust 必须把"对象引用"改为显式
   `contentIndex` 并保证"同一 index 多次 added/done"的幂等。
2. **`openai-completions` 的 `ensureToolCallBlock`**：按 `index` 或 `id` 双键合并、中途 id 变更、
   `custom` 覆盖 `json` 输入的动态切换、`Object.assign(block, finish())` 的字段覆盖。
3. **Anthropic 的 `convertMessages` 时序**：`pendingSystemMessages` 的持有/flush 时机、连续 toolResult
   合并、错误工具结果图片后置、`cache_control` 只挂最后一条非 assistant 消息的最后一个允许类型块。
4. **Anthropic 的 mid-conversation effort marker**：`assistantLevels` 记录"转换后消息索引"，且
   `insertThinkingLevelMessages` 在**每个**有历史 effort 的 assistant 前插入 marker，末尾再插当前 marker；
   顶层 `output_config.effort` 固定 `"low"`。
5. **Anthropic strict 工具降级**：仅 `strict === "prefer"` 且 `strict: true` 生效的工具、仅 400 +
   `invalid_request_error` + 3 个特定子串、仅**请求未开始流**、且是**单次请求**状态；
   诊断条目字段逐字。
6. **Codex WebSocket 会话缓存**：busy/复用/年龄上限/idle 定时器/账号隔离/`release({keep})` 的所有分支，
   以及 `provider_transport_failure` 的 `phase`/`eventsEmitted`/`fallbackTransport` 组合。
7. **Codex WebSocket 增量继续（delta）**：依赖 `JSON.stringify` 的**键序**相等（
   `requestBodiesMatchExceptInput`、`responseInputsEqual`）与 `lastResponseItems` 的构造
   （`convertResponsesMessages(normalizeContext({messages:[output]}), includeSystemPrompt:false)`
   再过滤掉 `function_call_output`/`custom_tool_call_output`）。
8. **`transformMessages` 的两遍算法**：第一遍 `flatMap` 丢弃跨模型 thinking、清除 `thoughtSignature`、
   归一化 id 并记录映射；第二遍孤儿 toolCall 合成 + 系统消息持有 + `error`/`aborted` assistant 跳过。
   与 `convertMessages`（Anthropic/Completions/Responses 各自）的交互必须精确。
9. **`makeJsonSchemaNodeStrict` 的原地变换顺序与错误优先级**：决定哪一个错误文本先出现；
   以及"非 required 且不允许 null 的属性包 `anyOf: [prop, {type:"null"}]`"和"`required` 变为全部属性"。
10. **grammar 工具输入的增量 JSON 拼接**：`{"prop":"` + `JSON.stringify(delta)` 去引号，收尾 `"}`；
    `unsent` 机制把初始 input 的 JSON 片段推迟到第一次 delta；`closed` 后的幂等/非单调错误。
11. **`sanitizeSurrogates` 与 Rust `String` 的不可能性**：需要在 JSON 解码层就处理孤立代理转义，
    否则行为分叉（TS 会保留再删除，Rust 会解析失败）。
12. **JS 对象键序（整数键优先）**：影响 `JSON.stringify` 相等的所有判定（Codex delta、`declarationsEqual`）。
13. **`estimateContextTokens` 的 UTF-16 长度**：影响 `maxTokens` 夹取 → 影响请求体与测试期望。
14. **`retryProviderRequest` 的 `parseFloat` 前缀解析与 `if (retryAfterMs)` 真值**、
    `Math.random` 抖动、`AbortError` 与 `"Request was aborted"` 的区分。
15. **`openai-completions` 的 `thinkingFormat` 分支矩阵（11 种）**：每个分支的"何时写字段、写什么、
    `thinkingLevelMap` 为 `null`/缺省/非 string 时如何"都要逐条对齐；`resolveChatTemplateKwargValue`
    对 `$var: "thinking.effort"` 的**未实现**行为也要照抄。
16. **`azure_openai_config.normalizeAzureBaseUrl` 的 URL 规范化**：`url.toString()` 的具体输出
    （末尾 `/`、默认端口、百分号编码）必须与 `URL` 一致；Rust `url::Url` 基本一致，但
    `Url::to_string()` 对空 path 会输出末尾 `/`，需 `trim_end_matches('/')`。
17. **流式 UTF-8 解码**：跨块多字节字符。
18. **`AssistantMessageEventStream` 的 `durationMs`**：`performance.now()` 单调时钟、`Math.round`、
    `message.timestamp < startedAt` 时跳过。

### 8.5 验收标准（Definition of Done）

- [ ] 第 7 节列出的**全部离线用例**在 `kiana` 有对应 Rust 测试并通过。
- [ ] 每个错误消息、占位文本、HTTP 头名、JSON 字段名与第 3、4 节逐字一致（可用快照测试）。
- [ ] 请求体的 JSON 键序与 TS 版一致（对每个模块各 1 个快照测试，用 `serde_json::to_string` 比较）。
- [ ] `streamSimple` 缺鉴权时**同步**返回错误（Rust 中用 `Result<AssistantMessageEventStream, Error>`
      或 panic-free 的 `Err`；调用方 `Models` 层需镜像"同步抛出"语义）。
- [ ] 所有 `*.lazy.ts` 的等价物存在且命名一致（`anthropic_messages_api()` 等）。
- [ ] `openai-codex-responses` 的 4 个 debug/cleanup 函数可被外部调用。
- [ ] 无 `unsafe`（若必须，需注释说明）。
- [ ] 每个模块有 `#[cfg(test)] mod tests` 或 `tests/` 文件覆盖边界条件。

---

## 9. 审计补充（Audit additions）

本节收罗对抗式复核中"原文缺失或表述不足、但不宜插进上文结构"的条目。与上文冲突时，**本节更精确**；
凡本节明确写出源码语义的，Rust 侧按本节实现。

### 9.1 `shortHash` 的算法（跨版本 tool call id 的硬约束）

已在前文 2.10（`normalizeToolCallId` 之后）补全逐行算法；此处只重申三条最容易写错的：
1. 迭代单位是 **UTF-16 码元**（`charCodeAt`），Rust 用 `s.encode_utf16()`。
2. `Math.imul` 是 32 位**有符号**乘法；`^` 也是 32 位有符号。`h2` 的第二条赋值用的是**更新后的** `h1`。
3. `>>>` 是逻辑右移；输出是 `(h2>>>0).toString(36) + (h1>>>0).toString(36)`，小写、无前导零、长 13–14。
测试建议：对 `"call_abc|item_1"`、`"resp_123|fc_456"`、含非 ASCII（emoji / 中文）的 id 各做一次快照，
与 TS 输出逐字节相等。

### 9.2 Codex WebSocket 的内部辅助函数（名字与默认值）

前文 2.14.10 已给出 `acquireWebSocket` / `connectWebSocket` 的分支；本节补齐 Rust 侧要照名字实现的小函数：
```rust
// 全部 pub(crate)，除标注外不导出
fn get_websocket_ready_state(socket: &WebSocketLike) -> Option<i32>;      // 非 number → None
fn is_websocket_reusable(socket: &WebSocketLike) -> bool;                  // None => true; Some(1) => true; 否则 false
fn is_websocket_session_expired(entry: &CachedWebSocketConnection) -> bool; // now - createdAt >= 55*60*1000
fn close_websocket_silently(socket: &WebSocketLike, code: u16, reason: &str); // 默认 (1000, "done")；吞异常
fn schedule_session_websocket_expiry(session_id: &str, account_id: &str, entry: &CachedWebSocketConnection);
fn get_or_create_websocket_debug_stats(session_id: &str) -> &mut OpenAICodexWebSocketDebugStats;
fn is_websocket_sse_fallback_active(session_id: Option<&str>) -> bool;
fn record_websocket_sse_fallback(session_id: Option<&str>);
fn record_websocket_failure(session_id: Option<&str>, error: &dyn std::error::Error);
```
- `close_websocket_silently` 的 reason 取值集合：`"debug_close"` / `"idle_timeout"` / `"connection_age_limit"` /
  `"aborted"` / `"connect_timeout"`，其余调用点用默认 `"done"`。
- `get_websocket_constructor` 有模块级缓存 `_cachedWebsocket`；**仅当 `env` 为空**时读写缓存。
- `parse_web_socket` 的 idle 超时、`acquire_websocket` 的 age 上限、`schedule_session_websocket_expiry`
  三者都必须用 `close_websocket_silently`（不能直接 `socket.close()`），否则 reason 会丢。

### 9.3 WebSocket → SSE 的**粘滞**回退（最容易实现成"仅本次生效"）

见前文 2.14.10 的 `recordWebSocketFailure` / `recordWebSocketSseFallback` 代码块。要点复述：
- 一旦某 `sessionId` 在任一次请求里发生过 WebSocket 传输失败（**包括已开始流之后**抛错的路径），
  该 session 就被写入 `websocketSseFallbackSessions`，**此后所有请求**直接走 SSE。
- 每次因粘滞标记而跳过 WS，都会再 `sseFallbacks++`（`websocketFallbackActive` 恒 true）。
- 只有 `resetOpenAICodexWebSocketDebugStats(sessionId)` 能清除；`closeOpenAICodexWebSocketSessions`
  **不清**（它只清连接缓存）。
- 无 `sessionId` 时三者（`isWebSocketSseFallbackActive` / `recordWebSocketFailure` /
  `recordWebSocketSseFallback`）全部是 no-op——不要把 `None` 当成一个共享的全局 bucket。

### 9.4 `openai-responses.createClient` **没有** `sendSessionAffinityHeaders` 开关

见前文 2.11.9 的"三处差别"。补充：
- 单测覆盖：`openai-responses-compat.test.ts` 的 "uses OpenAI no-session format when configured"
  与 "uses OpenAI no-session format for OpenCode Responses models" 直接给 `compat.sessionAffinityFormat`
  赋值（`openai-nosession`），**不设** `sendSessionAffinityHeaders`，期望仍发出
  `x-client-request-id` 且 `session_id` 缺省。
- Rust 侧若把 `sendSessionAffinityHeaders` 也套到 Responses 上，这两个用例会以"少发一个头"失败。

### 9.5 `§5.4` 上游依赖表已按 import 实测重写

原表把一批上游内部实现细节（`collapseSystemMessages`、`getToolStateChanges`、`declarationsEqual`、
`StreamingJsonParser`、`formatUserAgent`、`truncateErrorText`、`ownedSlice` 等）列为本单元依赖，实测在
15 个 in-scope 模块中**零引用**。现表只列真实 import，并新增了此前漏掉的三项：
`utils/estimate.ts::estimateContextTokens`（被 `simple-options.ts` 用）、
`session-resources.ts::registerSessionResourceCleanup`（被 `openai-codex-responses.ts` 模块级调用）、
`utils/node-http-proxy.ts::resolveHttpProxyUrlForTarget`（Codex WS 的 Bun 代理）。
另注：`openai-responses-shared.ts:753` 的 `contentText` 是**局部变量**（`join("

")`），不是 `utils/text.ts` 的函数。

### 9.6 测试计数的口径与已修正的错数

- 第 7 节开头新增了 **92 行 / 94 个测试文件**的实测表（`声明` / `用例` 两列；其中 `anthropic-oauth`、
  `github-copilot-oauth` 两个文件不属于本单元，只为对照）。此前文中散落的计数有 4 处错误，已就地修正：
  - `openai-responses-compat`：旧"15 plain / 39"→ 实际 **18 声明 / 39 用例**（12/7/5 三个 `it.each`）。
  - `openai-completions-tool-choice`：旧"44"→ 实际 **49** 个 `it`（无 `it.each`）。
  - `azure-openai-base-url`：旧"11 条"→ 实际 **17**（`normalizeAzureBaseUrl` 10 + 其余 7）。
  - `cache-retention`：旧"6/4/2"→ 实际 Anthropic 7 / Responses 16 / Completions 9，合计 **19 声明 / 32 用例**。
- 另有两处**用例名**与源码不符（旧文用概括名，已改成逐字名）：`anthropic-thinking-disable` 的第 8 条
  （`disables thinking for Claude reasoning models`，真实 API）、`openai-codex-stream` 的全部 26 条 plain 名。
- `it.each` 的展开值此前多处缺项，已补：`openai-responses-compat` 的第三个矩阵是 **5 组**
  （`gpt-5.4 priority→priority 2`、`gpt-5.5 priority→priority 2.5`、`gpt-5.5 flex→flex 0.5`、
  `gpt-6-luna priority→fast 2`、`gpt-6-luna fast→fast 2`），第 4/5 组验证"GPT-6 回 `fast` 而请求是
  `priority` 时仍按 2 倍计价"这一 #10034 回归。
- `transcript-tool-changes.test.ts`（22 声明 / 24 用例）与 `system-message-replay.test.ts`（9 条）此前
  完全不在清单里——它们用 `test()` 而非 `it()`，且直接断言 Anthropic 原生 system 消息 / `tool_addition`
  与 `tool_removal` 块、OpenAI `additional_tools` 前缀、Kimi 顶层 tools 边界，属于本单元**核心**行为。
- `classifier-models`：旧"7 plain"→ 实际 **8 声明 / 10 用例**（多出 `it.each` 3 组 Jev 路由）。
- 另有 5 个测试文件的 `it.each` 展开数已按源码逐个核对并写进上表：`supports-xhigh`（7/5/3/2 → 41）、
  `fireworks-models`（2/3/4 → 25）、`telemetry`（3/2/2/4 → 22）、`max-thinking`（5/5 → 12）、
  `sampling-options`（3/3/2 → 16）。这些数字此前要么缺失、要么按"数组元素"误算（把嵌套数组的
  内层元素也计进去，得到 13/7/6 之类）。Rust 侧做参数化测试时请以上表"用例"列为准。

### 9.7 本单元**不**包含的相邻文件（避免误实现）

`src/api/` 目录共 43 个 `.ts` 文件，其中 **22 个不属于本单元**（下面逐个列出），
规范只为对照而提及，Rust 侧不要实现：
`bedrock-converse-stream.ts` / `.lazy.ts`、`google-generative-ai.ts` / `.lazy.ts`、`google-shared.ts`、
`google-vertex.ts` / `.lazy.ts`、`mistral-conversations.ts` / `.lazy.ts`、`kiana-messages.ts` / `.lazy.ts`、
`cloudflare-ai-binding.ts`、`cloudflare-workers-ai-system-one.ts` / `.lazy.ts`、`cloudflare.ts`、
`openrouter-images.ts` / `.lazy.ts`、`llama-cpp-classify.ts` / `.lazy.ts`、`system-one-shared.ts`、
`typesafe-system-one.ts` / `.lazy.ts`。
（第 7 节里有若干测试文件（`cloudflare-ai-binding`、`fireworks-*`、`xai-responses` 等）虽然引用这些
相邻实现，但它们对**本单元**的断言点已在对应行注明。）

### 9.8 其它零散修正

- **`originator` 的合并顺序**：Codex 的 `identityHeaders = { originator: "kiana", ...getIdentityHeaders(...) }`；
  `getIdentityHeaders` 只返回 `User-Agent` + attribution（`HTTP-Referer`/`X-OpenRouter-Title`/
  `X-OpenRouter-Categories`/`X-BILLING-INVOKE-ORIGIN`/`x-opencode-client`），**不含** `originator`，
  故 `originator: "kiana"` 会保留，直到 `model.headers` / `options.headers` 覆盖（测试用
  `model.headers: { originator: "my-app" }` 验证）。`Authorization` / `chatgpt-account-id` 在最后无条件
  `set`，**不可被覆盖**（测试显式传 `Authorization: "Bearer ignored"` 仍断言 token 值）。
- **`normalizeToolCallId` 的 prefix 长度**：`hash = shortHash(id).slice(0, 8)` → `hash.length` 恒为 8 →
  prefix = `max(1, 40-8-1)` = **31**，最终 id 恒为 **40** 字符。不要用 `shortHash` 的 13–14 长度去算。
- **`normalizeCodexStatus` / `resolveCodexServiceTier`**：前者只接受 `CODEX_RESPONSE_STATUSES` 集合内的
  string；后者仅在**响应为 `"default"`** 且请求为 `flex`/`priority` 时采用请求值，否则
  `response ?? request`。Codex 的 `getServiceTierCostMultiplier` **没有** `"fast"` 分支（只有
  `flex`/`priority`），与 `openai-responses` 不同（后者 `priority | fast` 走同一分支）。
- **`openai-responses` 的 `serviceTier` 字段**：`buildParams` 里 `if (options?.serviceTier !== undefined)
  params.service_tier = options.serviceTier` 出现在 `reasoning` 之前；`samplingParams` 最后
  `Object.assign` 可以覆盖它。
- **`convertResponsesMessages` 的 ID 归一化**：`normalizeIdPart` 先 `replace(/[^a-zA-Z0-9_-]/g,"_")`
  再截 64 再 `replace(/_+$/,"")`（**先截断后去尾下划线**，顺序不能反）；`buildForeignResponsesItemId`
  用**全长** `shortHash`（不去 slice），再截 64。
- **`getResponsesToolSupport` 的优先级**：`changes` > `additions` > `none`；`changes` 需要
  `supportsMidConvoSystemMessages && supportsAdditionalTools` 同时为真。
- **`responseInputsEqual` / `requestBodiesMatchExceptInput`** 都是 `JSON.stringify` 比较，
  对 `undefined` 字段（序列化后消失）与键序敏感——Rust 侧必须用 `serde_json::to_string`（保持插入序，
  需启用 `preserve_order`）而不是 `Value == Value`。
- **`getWebSocketConstructor` 的 Bun 分支**：`wss:`→`https:`、`ws:`→`http:` 后再查代理；构造出的子类
  只加 `proxy` 选项，不改 URL。
- **`azure-openai-config`**：`normalizeAzureBaseUrl` 的错误串用**未 trim** 的原始 `baseUrl`；
  Azure 主机且路径属于 `""`/`"/"`/`"/openai"`/`"/openai/v1/responses"` 时改写为 `/openai/v1` 并清 query；
  判定用 `endsWith`（`evil-openai.azure.com` 也算）。
- **`simple-options`**：`CONTEXT_SAFETY_TOKENS = 4096`、`MIN_MAX_TOKENS = 1`、`MIN_ANSWER_TOKENS = 1024`、
  `DEFAULT_THINKING_BUDGETS = {minimal:1024, low:2048, medium:8192, high:16384}`；`buildBaseOptions`
  的 `apiKey: apiKey || options?.apiKey` 是 JS `||`（空串回落），而 `maxTokens` 用 `??`（`0` 保留）。
- **`transform-messages`**：`heldSystemMessages` **只**在 `closePendingToolCalls()` 里 flush，而该函数在
  **每个 assistant 分支的最开头**（`continue` 之前）、每个 `user` 分支、以及循环结束后都会调用——
  因此被跳过的 `error`/`aborted` assistant 消息**仍会**先触发一次 flush（已持有的 system 消息会落在它
  之前）。另外 `closePendingToolCalls()` 里的 `result.push(...heldSystemMessages)` 是**无条件**执行的
  （与 `pendingToolCalls` 是否为空无关）。`existingToolResultIds` 只在遇到新 assistant 的 toolCalls 时
  **重置**，所以**前面** assistant 已收到结果的 id 不会影响后面的合成判定。
- **Fireworks 的 Anthropic compat 组合**（来自模型目录，属上游 `models` 单元；本单元只需正确响应）：
  `fireworks-models.test.ts` 断言的完整集合是
  `{ allowEmptySignature: true, sendSessionAffinityHeaders: true, supportsEagerToolInputStreaming: false,
     supportsCacheControlOnTools: false, supportsLongCacheRetention: false }`。
  对应的本单元行为：不发 `tools[].eager_input_streaming`、改发 legacy fine-grained beta、
  工具上不挂 `cache_control`、`cacheRetention:"long"` 时不加 `ttl:"1h"`、
  且 `sessionId` 非空且 caching 开启时发 `x-session-affinity`（Fireworks 是 `sendSessionAffinityHeaders:true`
  的非 OpenRouter provider——这验证了 2.9.3 里"默认只有 OpenRouter 为 true、但可被 compat 打开"的分支）。
  另：Fireworks 的 `nemotron-3-ultra-nvfp4` **没有** `forceAdaptiveThinking`，走 budget 路径
  （`thinking = { type:"enabled", budget_tokens:16384, display:"summarized" }`，无 `output_config`）；
  `deepseek-v4p1-flash` / `qwen3p8-max` / `qwen3p8-2p4t-a95b` 有 `forceAdaptiveThinking: true` 且带原生
  effort 列表（`["off","low","high","max"]` / `["off","low","medium","xhigh"]`）。这些差异**全部来自目录数据**，
  本单元代码不得对 provider 名做特判。
- **`openai-completions` 的四个内容块类型守卫**（`isTextContentBlock`/`isThinkingContentBlock`/
  `isToolCallBlock`/`isImageContentBlock`）是 `block.type === "<literal>"` 的简单判定，Rust 侧用
  `matches!` 即可，无需 `pub`。

