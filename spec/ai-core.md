# ai-core 单元移植规格（TypeScript -> Rust）

本文件是 `@earendil-works/kiana-ai` 中 **ai-core** 单元的完整移植规格。源仓库 `packages/ai` 为只读参考；
Rust 实现位于 `/home/ctyun/code/kiana_rev/kiana`。

- 源包：`/home/ctyun/code/kiana_rev/kiana/packages/ai`，名称 `@earendil-works/kiana-ai`，版本 `1.4.0`（`src/utils/version.ts`
  的 `KIANA_AI_VERSION` 与 `package.json` 必须一致）。
- 本单元文件范围：`src/*.ts`、`src/utils/**`、`src/providers/**`、`src/compat/**`、`scripts/**`、
  `packages/ai/README.md`、`nix/model-catalog.json`、`scripts/model-catalog-protocol.ts`（仓库根 `scripts/`）。
- **不在本单元范围但被本单元直接调用**：`src/api/**`（各 wire protocol 实现）、`src/auth/**`（含 `auth/oauth/**`）、
  `packages/telemetry`。它们的**接口契约**在本文件第 5 节逐字记录，实现由对应单元负责。
  对应单元 id：`ai-auth`（`src/auth/**` + `src/oauth.ts` + `src/bun-oauth.ts`，规格 `spec/ai-auth.md`）、
  `ai-api-anthropic-openai` 与 `ai-api-other`（`src/api/**`，规格 `spec/ai-api-*.md`）、
  `telemetry-mcp`（`packages/telemetry`，规格 `spec/telemetry-mcp.md`；本单元只消费其 `TelemetrySpan` /
  `TelemetryMetrics` / `defineTelemetrySchema` 等公开 API）。
  本单元**不依赖** `ai-api-*` 的具体实现，只依赖其导出签名（`lazyApi()` 等，见 5.2）。
- 本文件同时是 (a) Rust 移植的验收清单（逐条可测），(b) Rust 侧设计说明。

## 目录

1. 概览
2. 逐文件规格（导出符号 + 行为）
3. 用户可见表面（逐字文本）
4. 外部格式（磁盘 / 环境变量 / HTTP / SSE）
5. 跨单元接口
6. npm 依赖与 Rust crate 映射
7. 测试清单
8. Rust 模块布局与最难复现的部分
9. Audit additions（对抗性审计补充：遗漏导出、常量、生成器内部函数、README/测试/配置与错误陈述汇总）

---

## 1. 概览

### 1.1 目的

`kiana-ai` 是 kiana 的 LLM 接入层：统一的 `AssistantMessage` 流式协议、provider 运行时（鉴权 / 模型目录 / 请求分发）、
生成式模型目录（models.dev + OpenRouter + Vercel AI Gateway + Radius 聚合）、工具参数校验、上下文估算与溢出检测、
OAuth 登录/刷新编排、遥测、以及一个兼容旧全局 API 的 `/compat` 入口。

本单元（ai-core）负责**除 wire protocol 实现与 auth 流程实现之外的全部内容**：类型系统、`Models` 集合、
`createProvider`、全部内置 provider 工厂、模型目录数据与生成脚本、目录分发协议、工具函数、CLI、遥测 schema。

### 1.2 在 monorepo 中的位置

- 被 `packages/coding-agent`、`packages/agent`、`packages/tui` 等通过 `@earendil-works/kiana-ai`（根入口）、
  `@earendil-works/kiana-ai/providers/*`、`/api/*`、`/compat` 导入。
- `packages/ai` 依赖 `packages/telemetry`（`@earendil-works/kiana-telemetry`，同版本 1.4.0）。
- 模型目录数据文件 `src/providers/data/*.json` 在 git 中被忽略，由 `npm run hydrate-model-data`
  （`generate-models.ts --strict --data-only`）从已发布的 `models.all.json` 生成；`src/providers/*.models.ts`
  与 `src/models.generated.ts` 是提交产物。

### 1.3 入口点（package exports，逐字）

`package.json` 的 `exports`：

| specifier | 目标 | 说明 |
|---|---|---|
| `.` | `dist/index.js` / `dist/index.d.ts` | 核心（无副作用） |
| `./models` | `dist/models.js` | 仅 `models.ts` 的导出（轻量入口） |
| `./compat` | `dist/compat.js` | 旧全局 API（副作用：注册 builtin api providers） |
| `./providers/*` | `dist/providers/*.js` | 每个 provider 工厂 |
| `./api/*` | `dist/api/*.js` | wire protocol 实现（非本单元） |
| `./utils/*` | `dist/utils/*.js` | 工具函数 |
| `./oauth` | `dist/oauth.js` | 仅类型（扩展 OAuth 兼容） |
| `./bedrock-provider` | `dist/bedrock-provider.js` | 只导出 `bedrockProviderModule = { stream, streamSimple }` |
| `./bun-oauth` | `dist/bun-oauth.js` | `registerBunOAuthFlows()`，为 Bun 单文件二进制静态注册 OAuth flow |

`bin`：`kiana-ai` -> `dist/cli.js`。`sideEffects`：`./dist/compat.js`、`./dist/images.js`、
`./dist/providers/images/register-builtins.js`。`engines.node >= 22.19.0`，`"type": "module"`。

### 1.4 关键不变量（必须逐条在 Rust 中成立）

1. `Models` 的**读取是同步的**且返回"最后一次已知列表"；网络只在 `refresh()` 中发生。
2. provider 的 `getModels()` 抛错等价于"没有模型"（`Models` 吞掉异常），不得向上传播。
3. `stream()` / `streamSimple()` **同步返回** `AssistantMessageEventStream`；鉴权与模块加载在流内部异步完成，
   失败以 `error` 事件 + 终止消息呈现；直接调用 API 实现的 `streamSimple()` 在缺少鉴权时可以**同步抛出**。
4. 一次生成失败后不得 throw：终态 `stopReason` 为 `"error"` 或 `"aborted"`，`errorMessage` 携带细节。
5. `error` 事件之前可以有 `start`；但 `done`/更新事件不得出现在 `start` 之前。
6. 模型 `type` 缺省为 `"chat"`；`stream*` 拒绝非 chat 模型，`generateImages`/`classify` 拒绝类型不匹配的模型，
   且这两个入口**永不 reject**（返回 `stopReason: "error"|"aborted"` 的结果对象）。
7. 存储的凭证"拥有"该 provider：只有没有存储凭证时才看环境变量；刷新失败不得回退到 env key。
8. 每个 provider 必须至少有一个 `apiKey` 或 `oauth` 鉴权方法（无 key 的本地服务也要有 `resolve()` 返回 `{auth:{}}`）。
9. 请求头合并顺序固定（`applyAuth`，后者覆盖前者、大小写不敏感、`null` 表示抑制/删除）：
   `getOpenCodeSessionHeaders(model, sessionId)` -> `getAuth()` 返回的 `auth.headers`（其中已先与 `model.headers`
   按 `mergeHeaders(auth.headers, model.headers)` 合并） -> `options.headers` -> `options.transformHeaders(...)` -> dispatch。
   注意 `model.headers` 只在传 **model** 给 `getAuth` 时合并（即请求路径），`getAuth(providerId)` 不含它；
   另外 provider 自身的 `headers` 字段由 API 实现在更底层合并（identity 头在 model/provider/request 之下）。
10. 标识头（identity headers）位于 model/provider/request headers **之下**：配置的头覆盖它，请求里 `null` 删除它。
11. OAuth 刷新在 credential store 的 `modify()` 锁内完成（双重检查），带 15 s 超时；刷新一旦开始，
    不因调用方取消而放弃（否则可能丢弃已轮换的 refresh token）。
12. 所有时间戳为 Unix 毫秒；`durationMs` 使用单调时钟且只由 `AssistantMessageEventStream` 在终态设置。

### 1.5 作用域与符号计数

> 计数方法（可复现）：对每个文件用正则 `^export\s+(?:declare\s+)?(?:const|let|var|function|class|interface|type|enum)\s+(\w+)`
> 抓取声明名，再对**不带 `from`** 的 `export { ... }` / `export type { ... }` 块按 `as` 后的名字补充
> （本单元只有两处：`src/providers/all.ts` 的 `radiusProvider`、`src/compat/extension-oauth-types.ts` 的
> `OAuthCredentials`）；`export *` 与带 `from` 的重导出**一律不计**（含 `index.ts` 的 `export { Type } from "typebox"`）。
> 按名字去重。

本单元范围内（`src/*.ts`、`src/utils/*.ts`、`src/providers/*.ts`、`src/providers/images/*.ts`、`src/compat/*.ts`，
排除 `src/api/*`、`src/auth/*`）共 **142 个 TS 文件**；**唯一导出声明名共 558 个**。其中：
- **432 个**非分片名字（100 个非分片文件）；
- `providers/*.models.ts` 共 42 个分片，各导出 `_MODELS`/`_IMAGE_MODELS`/`_CLASSIFIER_MODELS` 3 个常量，
  合计 **126** 个名字。
- 若只统计 `src/*.ts`、`src/utils/*.ts`、`src/providers/*.ts`、`src/compat/*.ts`（不含 `providers/images/`）
  的 141 个文件，则为 **555** 个（非分片 429）。
- `packages/ai/scripts/*.ts`（9 个文件）贡献 **24** 个导出名（`model-data.ts` 13、`openrouter-catalog.ts` 3、
  `ai-gateway-pricing.ts` 3、`models-dev-reasoning-options.ts` 2、`openrouter-reasoning-options.ts` 2、
  `hydrate-model-catalog.ts` 1；`check-model-data.ts`、`generate-models.ts`、`generate-test-image.ts` 为 0——
  `generate-models.ts` 里的 `export const ${catalogConstName(...)}` 只出现在模板字符串中，不是真实导出），
  仓库根 `scripts/model-catalog-protocol.ts` 贡献 **17** 个，合计 **41** 个。
  与 src 的 558 取并集：**无任何重名**（`scripts/*` 的 24 个名字与 `model-catalog-protocol.ts` 的 17 个名字都不在
  src 的 558 个名字中），故并集为 558 + 41 = **599**。
- 若把 `src/api/*`、`src/auth/*` 一并计入（仅作参考，不属于本单元），共 **203 个文件、730 个唯一导出名**；
  全包（src + scripts）口径为 **213 个文件、771 个唯一导出名**（730 + 41，无重合）。

> **前稿勘误**：原 §1.5 写的 564 / 438 / 744 / 785 四个数均无法按上述方法复现（实际为 558 / 432 / 730 / 771，
> 或按 141 文件口径 555 / 429）。最可能的原因是原稿统计口径混用了"文件内声明"与"跨文件去重"
> （例如把 `providers/images/register-builtins.ts` 的 2 个导出漏计，或把部分 `export { ... } from` 块也计入）。
> 以上为逐文件重算的可复现结果，Rust 侧应以本节为准。

---

## 2. 逐文件规格

约定：签名用 TypeScript 原文；"行为"按实现顺序描述；所有字符串字面量逐字给出（Rust 需完全一致）。

### 2.1 根文件

#### 2.1.1 `src/index.ts`（50 行）

无副作用 barrel。导出：

- `export type { Static, TSchema } from "typebox"`；`export { Type } from "typebox"`。
- `export * from "./api/lazy.ts"`（`lazyStream`、`lazyApi`、`LazyStreamObserver`、`LazyApiCapabilities`）。
- 类型重导出：`AnthropicEffort`、`AnthropicOptions`、`AnthropicThinkingDisplay`、`AzureOpenAIResponsesOptions`、
  `BedrockOptions`、`BedrockThinkingDisplay`、`GoogleOptions`、`GoogleApiThinkingLevel`、`ResolvedGoogleThinkingLevel`、
  `GoogleVertexOptions`、`MistralOptions`、`OpenAICodexResponsesOptions`、`OpenAICodexWebSocketDebugStats`、
  `OpenAICompletionsOptions`、`OpenAIResponsesOptions`、`KianaMessagesEvent`、`KianaMessagesOptions`、`KianaMessagesRewriteImpact`。
- `export *`：`auth/context.ts`、`auth/credential-store.ts`、`auth/helpers.ts`、`auth/types.ts`、`models.ts`、
  `models-store.ts`、`providers/faux.ts`、`session-resources.ts`、`types.ts`、`utils/diagnostics.ts`、
  `utils/event-stream.ts`、`utils/identity.ts`、`utils/json-parse.ts`、`utils/overflow.ts`、`utils/retry.ts`、
  `utils/streaming-json.ts`、`utils/transcript.ts`、`utils/typebox-helpers.ts`、`utils/validation.ts`。
- 选出的具名导出：`contentText`、`getSystemMessageText`、`renderSystemMessageUpdate`（`utils/text.ts`）、
  `uuidv7`（`utils/uuid.ts`）、`KIANA_AI_VERSION`（`utils/version.ts`）。
- 旧扩展 OAuth 类型（type-only）：`OAuthAuthInfo`、`OAuthDeviceCodeInfo`、`OAuthLoginCallbacks`、`OAuthPrompt`、
  `OAuthSelectOption`、`OAuthSelectPrompt`，来自 `./compat/extension-oauth-types.ts`（**不含** `OAuthCredentials`，
  它经 `auth/types.ts` 的 `export *` 提供）。

顺序即上面列出的顺序（源文件 1–50 行）；`index.ts` 本身**没有任何运行时副作用**（注释明确："Core only,
side-effect free: no generated catalogs, no provider factories, no api-registry, no OAuth implementations, no compat"）。

Rust 对应：`kiana_ai` crate 的 `lib.rs`（`pub use`）；不引入网络/SDK 初始化副作用。

#### 2.1.2 `src/types.ts`（915 行）——核心类型系统

来源类型（type-only import，擦除后无运行时依赖）：`TelemetrySpan`、各 API 的 `*Options`、
`*Compat`（compat-schema）、`CacheRetention/ModelCost/ModelInputLimits/ModelInputModality/ModelPromptCache/
ModelThinkingLevel/ThinkingLevel/ThinkingLevelMap`（model-schema）、`AssistantMessageDiagnostic`、
`AssistantMessageEventStream`、`ClientIdentity`。

导出（逐项，Rust 需要有等价 enum/struct；`Api`、`ProviderId`、`ClassifierApi`、`ImageApi` 是"开放字符串"）：

```ts
export type KnownApi = "openai-completions" | "mistral-conversations" | "openai-responses" |
  "azure-openai-responses" | "openai-codex-responses" | "anthropic-messages" |
  "bedrock-converse-stream" | "google-generative-ai" | "google-vertex" | "kiana-messages";
export type Api = KnownApi | (string & {});
export type KnownImageApi = "openrouter-images";
export type ImageApi = KnownImageApi | (string & {});
export type KnownClassifierApi = "typesafe-system-one" | "cloudflare-workers-ai-system-one" |
  "llama-cpp-classify" | "openai-decisions";
export type ClassifierApi = KnownClassifierApi | (string & {});
export type KnownProvider =
  | "amazon-bedrock" | "ant-ling" | "anthropic" | "google" | "google-vertex" | "openai" | "azure"
  | "openai-codex" | "radius" | "typesafe" | "nvidia" | "deepseek" | "github-copilot" | "xai" | "groq"
  | "cerebras" | "openrouter" | "vercel-ai-gateway" | "zai" | "zai-coding-cn" | "mistral" | "minimax"
  | "minimax-cn" | "moonshotai" | "moonshotai-cn" | "huggingface" | "fireworks" | "together" | "baseten"
  | "opencode" | "opencode-go" | "kimi-coding" | "meta" | "cloudflare-workers-ai"
  | "cloudflare-ai-gateway" | "qwen-token-plan" | "qwen-token-plan-cn" | "qwen-token-plan-individual"
  | "xiaomi" | "xiaomi-token-plan-cn" | "xiaomi-token-plan-ams" | "xiaomi-token-plan-sgp";   // 42 项，顺序即上面
export type ProviderId = KnownProvider | string;
export type ToolChoice = "auto" | "none";
export type SamplingParams = Record<string, unknown>;
export type SamplingParamsByThinkingLevel = Partial<Record<ModelThinkingLevel, SamplingParams>>;
export interface ThinkingBudgets { minimal?: number; low?: number; medium?: number; high?: number }
export type Transport = "sse" | "websocket" | "websocket-cached" | "auto";
export type ProviderEnv = Record<string, string>;
export type ProviderHeaders = Record<string, string | null>;
export type FetchFunction = typeof globalThis.fetch;
export interface ProviderResponse { status: number; headers: Record<string, string> }
```

`ProviderRequestOptions<TModel>`（所有请求共享）：

| 字段 | 类型 | 语义 |
|---|---|---|
| `signal` | `AbortSignal?` | 取消 |
| `telemetry` | `TelemetrySpan?` | 显式父 span（注意：是 span 不是 adapter） |
| `apiKey` | `string?` | 显式 key，覆盖 provider 解析结果 |
| `fetch` | `FetchFunction?` | 自定义 fetch；不支持注入的 adapter 可拒绝 |
| `env` | `ProviderEnv?` | provider 作用域环境，优先于 process.env |
| `onPayload` | `(payload, model) => unknown / undefined / Promise<...>` | 发送前检查/替换 payload；返回 undefined 表示不改 |
| `onResponse` | `(ProviderResponse, model) => void / Promise<void>` | 收到 HTTP 响应后 |
| `headers` | `ProviderHeaders?` | 自定义头；`null` 抑制同名默认头；Bedrock 走 Smithy build middleware 并忽略 `x-amz-*`/`authorization`/`host` |
| `identity` | `ClientIdentity?` | UA 与归因头；缺省字段用 kiana-ai 默认（detailed UA、无归因） |
| `timeoutMs` | `number?` | HTTP 超时（SDK 默认 10 分钟） |
| `maxRetries` | `number?` | SDK 客户端重试次数（默认 2） |
| `maxRetryDelayMs` | `number?` | 服务端要求的长重试延迟上限，默认 60000；0 表示关闭上限 |

`StreamOptions extends ProviderRequestOptions<Model<Api>>`：`onResponse` 窄化；`onProviderStreamEvent?: (data: unknown, model) => void|Promise<void>`
（在每个已解析的 provider 事件被归一化之前调用，按流顺序 await，抛错会让请求失败）；`temperature?: number`；
`samplingParams?: SamplingParams`（在具名请求字段之后合并，键覆盖之；仅 OpenAI 兼容 adapters 使用）；
`maxTokens?`；`transport?: Transport`；`cacheRetention?: CacheRetention`（默认 `"short"`）；`sessionId?: string`；
`websocketConnectTimeoutMs?: number`；`metadata?: Record<string, unknown>`。

`ProviderStreamOptions = StreamOptions & Record<string, unknown>`。
`DeferredFetchOptions extends ProviderRequestOptions` 增加 `wait?: number`（默认 0，做一次状态检查）。
`DeferredCancelOptions = ProviderRequestOptions<Model<Api>>`。

`ApiOptionsMap`：`anthropic-messages` -> `AnthropicOptions`；`openai-completions` -> `OpenAICompletionsOptions`；
`openai-responses` -> `OpenAIResponsesOptions`；`openai-codex-responses` -> `OpenAICodexResponsesOptions`；
`azure-openai-responses` -> `AzureOpenAIResponsesOptions`；`google-generative-ai` -> `GoogleOptions`；
`google-vertex` -> `GoogleVertexOptions`；`mistral-conversations` -> `MistralOptions`；
`bedrock-converse-stream` -> `BedrockOptions`；`kiana-messages` -> `KianaMessagesOptions`。
`ApiStreamOptions<TApi>` = 已知 api 的具体选项类型，否则 `StreamOptions & Record<string, unknown>`。

契约接口：

```ts
interface ProviderStreams {
  stream(model, context: TranscriptContext, options?: StreamOptions): AssistantMessageEventStream;
  streamSimple(model, context: TranscriptContext, options?: SimpleStreamOptions): AssistantMessageEventStream;
  fetchDeferred?(model, handle: DeferredHandle, options?: DeferredFetchOptions): AssistantMessageEventStream;
  cancelDeferred?(model, handle: DeferredHandle, options?: DeferredCancelOptions): Promise<void>;
}
interface ProviderImages { generateImages(model, context: ImagesContext, options?: ImagesOptions): Promise<AssistantImages> }
interface ProviderClassifier { classify(model, context: ClassifierContext, options?: ClassifierOptions): Promise<ClassifierResult> }
interface ClassifierOptions extends ProviderRequestOptions<ClassifierModel<ClassifierApi>> { temperature?: number }
interface ImagesOptions extends ProviderRequestOptions<ImageModel<ImageApi>> { metadata?: Record<string, unknown> }
type ProviderImagesOptions = ImagesOptions & Record<string, unknown>;
interface SimpleStreamOptions extends StreamOptions {
  toolChoice?: ToolChoice;                                  // provider 中立工具选择
  reasoning?: ThinkingLevel;
  deferred?: boolean | { window?: "15m" | "1h" | "24h" };
  thinkingBudgets?: ThinkingBudgets;
}
type StreamFunction<TApi, TOptions> = (model, context: TranscriptContext, options?: TOptions) => AssistantMessageEventStream;
type ImagesFunction<TOptions> = (model, context: ImagesContext, options?: TOptions) => Promise<AssistantImages>;
type ClassifierFunction<TOptions> = (model, context: ClassifierContext, options?: TOptions) => Promise<ClassifierResult>;
```

内容块（`ToolCall.arguments` 是 `JsonObject`）：

```ts
interface TextSignatureV1 { v: 1; id: string; phase?: "commentary" | "final_answer" }
interface TextContent { type: "text"; text: string; textSignature?: string }
interface ThinkingContent { type: "thinking"; thinking: string; thinkingSignature?: string; redacted?: boolean }
interface ImageContent { type: "image"; data: string; mimeType: string }   // data 为 base64
interface ToolCall {
  type: "toolCall"; id: string; name: string; arguments: JsonObject;
  thoughtSignature?: string; namespace?: string;
  argumentsError?: string;   // 完成时参数不是合法 JSON 对象时设置，arguments 此时为 {}
}
interface Usage {
  input: number; output: number; cacheRead: number; cacheWrite: number;
  cacheWrite1h?: number;     // cacheWrite 中按 1h 保留写入的子集（仅 Anthropic 报告）
  reasoning?: number;        // output 的子集
  totalTokens: number;
  cost: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number; totalFromProvider?: boolean };
}
type StopReason = "pending" | "stop" | "length" | "toolUse" | "error" | "aborted" | "deferred";
type JsonValue = null | boolean | number | string | readonly JsonValue[] | JsonObject;
type JsonObject = { [key: string]: JsonValue };
```

`JsonRepresentation<T>` / `IsJsonCompatible<T>` 是**编译期**工具类型（见源码 492-537 行）：对象键必须是 string|number，
值必须 JSON 兼容，函数/undefined/symbol 不兼容；`ToolResultMessage<TDetails>` 在 `TDetails` 不兼容时成为 `never`。
Rust 侧不需要该类型级机制，但需要在 serde 上保证同样的可序列化集合（拒绝 NaN/Infinity，见 6.3）。

`DeferredHandle`：`{ provider: string; modelId: string; api: string; id: string; expiresAt?: number; pollAfterMs?: number; data?: JsonValue }`。

`SystemMessage`：

```ts
interface SystemMessage {
  role: "system";
  content: string | TextContent[];
  sections?: Record<string, string | null>;   // 命名有序段落；后续消息按名替换，null 删除
  toolsAdded?: Tool[];
  toolsRemoved?: ToolReference[];
  timestamp: number;
}
interface UserMessage { role: "user"; content: string | (TextContent | ImageContent)[]; timestamp: number }
interface AssistantMessage {
  role: "assistant";
  content: (TextContent | ThinkingContent | ToolCall)[];
  api: Api; provider: ProviderId; model: string;
  responseModel?: string; responseId?: string;
  providerThinkingLevel?: string; thinkingLevel?: ModelThinkingLevel;
  diagnostics?: AssistantMessageDiagnostic[];
  usage: Usage; stopReason: StopReason;
  deferred?: DeferredHandle; errorMessage?: string; rawStopReason?: string; endTurn?: boolean;
  timestamp: number;
  durationMs?: number;   // 单调时钟；仅由事件流在终态设置
}
interface NestedToolCallRecord { id; name; arguments?; argumentsBytes?; status: "ok"|"error"|"unfinished"; durationMs?; error? }
interface NestedToolCalls { calls: NestedToolCallRecord[]; complete: boolean }
interface ToolResultMessage<TDetails = JsonValue> {
  role: "toolResult"; toolCallId: string; toolName: string;
  content: (TextContent | ImageContent)[]; details?: JsonRepresentation<TDetails>; usage?: Usage;
  nestedCalls?: NestedToolCalls; isError: boolean; timestamp: number; durationMs?: number;
}
type Message = SystemMessage | UserMessage | AssistantMessage | ToolResultMessage;
```

图像与分类：

```ts
type ImagesInputContent = TextContent | ImageContent;
type ImagesOutputContent = TextContent | ImageContent;
interface ImagesContext { input: ImagesInputContent[] }
type ImagesStopReason = "stop" | "error" | "aborted";
interface AssistantImages { api: ImageApi; provider: ProviderId; model: string; output: ImagesOutputContent[];
  responseId?: string; usage?: Usage; stopReason: ImagesStopReason; errorMessage?: string; timestamp: number }
type ClassifierQuestion = ClassifierChoiceQuestion | ClassifierScoreQuestion | ClassifierBoolQuestion;
  // choice: { type:"choice"; instructions; criteria: Record<string,string> }
  // score:  { type:"score"; instructions; criteria: string[] }
  // bool:   { type:"bool"; instructions; criteria: { true: string; false: string } }
interface ClassifierContext { state: JsonObject; images?: ImageContent[]; questions: Record<string, ClassifierQuestion> }
type ClassifierAnswer = { type:"choice"; choice: string; probabilities: Record<string, number>; confidence: number }
  | { type:"score"; score: number; confidence: number } | { type:"bool"; probability: number };
type ClassifierStopReason = "stop" | "error" | "aborted";
interface ClassifierResult { api; provider; model; answers: Record<string, ClassifierAnswer>; usage?: Usage;
  stopReason: ClassifierStopReason; errorMessage?: string; timestamp: number }
```

工具与上下文：

```ts
type GrammarFormat = "openai_lark" | "openai_regex";
type GrammarVariants = Partial<Record<GrammarFormat, string>>;
type ConstrainedSamplingConfig =
  | { type: "json_schema"; strict: "prefer" | "require" }
  | { type: "grammar"; variants: GrammarVariants };
interface Tool<TParameters extends TSchema = TSchema> {
  name: string; description: string; parameters: TParameters;
  constrainedSampling?: false | ConstrainedSamplingConfig;
}
interface ToolReference { name: string }
interface Context { systemPrompt?: string; messages: Message[]; tools?: Tool[] }
type TranscriptContext = { messages: Message[]; readonly [brand]: true };   // 仅 normalizeContext() 可产生
```

`AssistantMessageEvent`（协议，逐字）：

```ts
type AssistantMessageEvent =
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
  | { type: "done"; reason: "stop"|"length"|"toolUse"|"deferred"; message: AssistantMessage }
  | { type: "error"; reason: "aborted"|"error"; error: AssistantMessage };
```

`partial` 是**共享的可变"截至目前响应"对象**，不是事件时刻快照；text/thinking 块在 `*_start` 时为空字符串，
只通过 `*_delta` 增长，直到权威的 `*_end`；redacted thinking 可以在 start 时就是完整的且不发 delta；
`toolcall_start` 时 arguments 是 provider 相关（可以是 `{}` 或已解析的前缀），`toolcall_delta` 携带后续 JSON 更新。
不同块的流事件**不保证连续**（可交错），消费方必须用 `contentIndex` 关联。

模型：

```ts
interface BaseModel<TApi extends string> {
  id: string; name: string; api: TApi; provider: ProviderId; baseUrl: string;
  input: ModelInputModality[]; inputLimits?: ModelInputLimits; cost: ModelCost; headers?: Record<string,string>;
}
interface Model<TApi extends Api> extends BaseModel<TApi> {
  type?: "chat"; reasoning: boolean; thinkingLevelMap?: ThinkingLevelMap; promptCache?: ModelPromptCache;
  contextWindow: number; maxTokens: number;
  samplingParams?: SamplingParams; samplingParamsByThinkingLevel?: SamplingParamsByThinkingLevel;
  compat?: <按 api 选择 OpenAICompletionsCompat | OpenAIResponsesCompat | AnthropicMessagesCompat |
                  BedrockCompat | MistralConversationsCompat | never>;
}
interface ImageModel<TApi extends ImageApi> extends BaseModel<TApi> { type: "image"; output: ("text"|"image")[] }
interface ClassifierModel<TApi extends ClassifierApi> extends BaseModel<TApi> {
  type: "classifier"; contextWindow: number; compat?: SystemOneCompat（仅 System One api）;
}
interface ModelTypeMap { chat: Model<Api>; image: ImageModel<ImageApi>; classifier: ClassifierModel<ClassifierApi> }
type ModelType = keyof ModelTypeMap;
type AnyModel = ModelTypeMap[ModelType];
```

#**关键运行时记录（逐字字段）**：
```ts
interface ToolCall { type: "toolCall"; id: string; name: string; arguments: JsonObject;
  thoughtSignature?: string; namespace?: string; argumentsError?: string }
interface AssistantMessage {
  role: "assistant"; content: (TextContent | ThinkingContent | ToolCall)[];
  api: Api; provider: ProviderId; model: string;
  responseModel?: string; responseId?: string; providerThinkingLevel?: string; thinkingLevel?: ModelThinkingLevel;
  diagnostics?: AssistantMessageDiagnostic[]; usage: Usage; stopReason: StopReason;
  deferred?: DeferredHandle; errorMessage?: string; rawStopReason?: string; endTurn?: boolean;
  timestamp: number; durationMs?: number;
}
```
`ContentBlock` 家族：`TextContent {type:"text"; text; textSignature?}`、`ThinkingContent {type:"thinking"; thinking;
thinkingSignature?; redacted?}`（**字段名是 `thinkingSignature`，不是 `signature`**；`thoughtSignature` 属于 `ToolCall`）、`ImageContent {type:"image"; data; mimeType}`（内容块中只有 text/thinking/toolCall 出现在
assistant 消息里；image 出现在 user/toolResult 消息）。`ToolResultMessage { role:"toolResult"; toolCallId; toolName;
content: (TextContent|ImageContent)[]; isError?: boolean; timestamp; ... }`。
`UserMessage { role:"user"; content: string | (TextContent|ImageContent)[]; timestamp }`；
`SystemMessage { role:"system"; content: string | TextContent[]; sections?: Record<string, string|null>;
toolsAdded?: Tool[]; toolsRemoved?: ToolReference[]; timestamp }`（**content 不含 ImageContent**）。
`Message = UserMessage | AssistantMessage | ToolResultMessage | SystemMessage`。
`Usage { input; output; cacheRead; cacheWrite; cacheWrite1h?; reasoning?; totalTokens; cost: { input; output;
cacheRead; cacheWrite; total; totalFromProvider? } }`（除 `cacheWrite1h`/`reasoning`/`totalFromProvider` 外全部必填）。
`DeferredHandle { provider: string; modelId: string; api: string; id: string; expiresAt?: number;
pollAfterMs?: number; data?: JsonValue }`（`data` 为 provider 重建最终 assistant 消息所需的转换数据；
`expiresAt` 为毫秒时间戳）。
`NestedToolCallRecord { id; name; arguments?: JsonObject; argumentsBytes?: number;
  status: "ok"|"error"|"unfinished"; durationMs?: number; error?: string }`；
`NestedToolCalls { calls: NestedToolCallRecord[]; complete: boolean }`（`arguments` 超限时省略，改给
`argumentsBytes`（UTF-8 JSON 字节数）；`complete` 为 false 表示有调用被丢弃、参数被省略或未完成）。
**重要**：`SystemMessage.content` 是 `string | TextContent[]`（**不含 image**）；`sections` 的注释明确要求
"Avoid integer-like names; JSON objects reorder those"——即 JS 对象把整数键前置，Rust 侧用 `IndexMap` 无法复现，
需在序列化层处理（见 §6 末尾"JS 语义差异清单"第 3 条）。

#### 2.1.3 `src/models.ts`（1300 行）——`Models` 集合与 `createProvider`

**导出**

```ts
export { ModelsError, type ModelsErrorCode } from "./auth/resolve.ts";   // 实际定义在 utils/models-error.ts
export { AI_TELEMETRY_SCHEMA } from "./telemetry.ts";
export { getModelType, isModelType } from "./utils/model-operations.ts";
export interface ModelsPublication { persist?: ModelsStoreEntry | null; update?: () => void }
export interface RefreshModelsContext {
  credential?: Credential; stored?: Readonly<ModelsStoreEntry>;
  publish(publication: ModelsPublication): Promise<boolean>;
  allowNetwork: boolean; force?: boolean; signal: AbortSignal; baseUrl?: string;
}
export interface ModelsRefreshOptions { allowNetwork?: boolean; providers?: readonly string[]; force?: boolean; signal?: AbortSignal }
export interface ModelsRefreshResult { aborted: boolean; errors: ReadonlyMap<string, Error> }
export interface ModelsRequestTransforms { transformHeaders?: (headers: ProviderHeaders) => ProviderHeaders | Promise<ProviderHeaders> }
export type ModelsApiStreamOptions<TApi> = ApiStreamOptions<TApi> & ModelsRequestTransforms;
export type ModelsSimpleStreamOptions = SimpleStreamOptions & ModelsRequestTransforms;
export type ModelsDeferredFetchOptions = DeferredFetchOptions & ModelsRequestTransforms;
export type ModelsDeferredCancelOptions = DeferredCancelOptions & ModelsRequestTransforms;
export type ModelsImagesOptions = ImagesOptions & ModelsRequestTransforms;
export type ModelsClassifierOptions = ClassifierOptions & ModelsRequestTransforms;
export interface Provider<TApi extends Api = Api> { ... }     // 见下
export interface Models { ... }                              // 见下
export interface MutableModels extends Models { setProvider; deleteProvider; clearProviders }
export interface CreateModelsOptions { credentials?: CredentialStore; modelsStore?: ModelsStore; authContext?: AuthContext; identity?: ClientIdentity }
export function createModels(options?: CreateModelsOptions): MutableModels
export interface CreateProviderOptions<TApi extends Api = Api> { ... }
export function createProvider<TApi extends Api = Api>(input: CreateProviderOptions<TApi>): Provider<TApi>
export function hasApi<TApi extends Api>(model: AnyModel, api: TApi): model is Model<TApi>
export function calculateCost(model: AnyModel, usage: Usage): Usage["cost"]
export function getSupportedThinkingLevels<TApi extends Api>(model: Model<TApi>): ModelThinkingLevel[]
export function clampThinkingLevel<TApi extends Api>(model: Model<TApi>, level: ModelThinkingLevel): ModelThinkingLevel
export function modelsAreEqual(a: AnyModel | null | undefined, b: AnyModel | null | undefined): boolean
```

**`Provider` 接口字段**：`id`、`name`、`baseUrl?`、`headers?`、`auth: ProviderAuth`、`getModels(): readonly Model<TApi>[]`、
`getAllModels?()`、`refreshModels?(context: RefreshModelsContext)`、`filterModels?(models, credential)`、
`filterAllModels?(models, credential)`、`stream`、`streamSimple`、`fetchDeferred?`、`cancelDeferred?`、
`generateImages?`、`classify?`。

**`Models` 接口方法**（顺序即实现顺序）：`getProviders()`、`getProvider(id)`、`getModels(provider?)`、
`getModel(provider, id)`、`getModelsOfType(type, provider?)`、`getModelOfType(type, provider, id)`、
`getAllModels(provider?)`、`refresh(options?)`、`checkAuth(providerId, options?)`、`getAvailable(providerId?, options?)`、
`getAvailableOfType(type, providerId?, options?)`、`getAllAvailable(providerId?, options?)`、
`getAuth(providerId | model, overrides?)`、`login(providerId, type, interaction, options?)`、
`logout(providerId, options?)`、`stream`、`complete`、`streamSimple`、`completeSimple`、`streamDeferred`、
`fetchDeferred`、`cancelDeferred`、`generateImages`、`classify`。

**内部常量与辅助**

- `KNOWN_MODEL_TYPES: Record<ModelType, true> = { chat: true, image: true, classifier: true }`。
- `hasKnownModelType(model)`：`Object.hasOwn(KNOWN_MODEL_TYPES, getModelType(model))`。
- `withKnownModelTypes(entry)`：`{ ...entry, models: entry.models.filter(hasKnownModelType) }`（丢弃本版本不认识的 type）。
- `mergeHeaders(base, override)`：无两者返回 `undefined`；否则浅拷贝 base，对 override 的每个 `[name, value]`
  先删除 base 中**忽略大小写**同名的键，再写入 `merged[name] = value`（保留 override 的原始大小写；`null` 会被保留，
  由下游解释为"抑制"）。**注意**这不同于 `providerHeadersToRecord`（后者用全小写键、`null` 直接删除）。

**`ModelsImpl` 私有状态**：`providers: Map<string, Provider>`、`credentials`、`modelsStore`、`authContext`、
`identity`、`refreshGenerations: Map<string, number>`、`refreshControllers: Map<string, AbortController>`、
`publicationChains: Map<string, Promise<unknown>>`。

- `setProvider(provider)`：先 `supersedeProviderRefresh(provider.id)` 再 `providers.set`。
- `deleteProvider(id)`：先 supersede 再 delete。
- `clearProviders()`：对 `providers ∪ refreshControllers` 的 id 集合逐个 supersede，再清空 providers。
- `getProviders()`：`Array.from(providers.values())`（插入顺序）。
- `getModels(provider?)`：给定 provider 时未知返回 `[]`，`getModels()` 抛错捕获后返回 `[]`；否则按 provider 插入顺序拼接，逐个 try/catch。
- `getAllModels(provider?)`：同上但优先 `getAllModels?.() ?? getModels()`。
- `getModelsOfType(type, provider?)`：`getAllModels(provider).filter(isModelType(type))`。
- `getModel(provider, id)` / `getModelOfType(type, provider, id)`：线性 `find(id ===)`。
- `supersedeProviderRefresh(id)`：generation = 旧值+1（无则 1），写入 map；若存在旧 controller，从 map 删除并 `abort()`；返回 generation。
- `beginProviderRefresh(id)`：先 supersede，再新建 `AbortController` 存入 map。
- `publishProviderModels(providerId, generation, signal, publication)`：
  1. `previous = publicationChains.get(id) ?? Promise.resolve()`；
  2. 排队任务：`await previous.catch(()=>{})`；若 `signal.aborted` 或 generation 已变 -> `false`；
  3. `persist === null` -> `modelsStore.delete(id,{signal})`；`persist !== undefined` -> `modelsStore.write(id, structuredClone(persist), {signal})`；
  4. 再次检查 aborted/generation，返回 false；
  5. 同步执行 `publication.update?.()`，返回 true；
  6. 链尾（`.catch(()=>{})`）存入 map，并在结束时若仍是当前链则删除；
  7. 返回 `raceWithAbortSignal(queued, signal)`。
  含义：同一 provider 的发布串行化；取消或更新一代会丢弃过期发布（不写盘、不更新内存）。
- `runProviderRefreshPhase(provider, credential, allowNetwork, force, generation, signal)`：
  `stored = await modelsStore.read(provider.id,{signal})`，然后调用 `provider.refreshModels({ credential,
  stored: stored ? withKnownModelTypes(structuredClone(stored)) : undefined, publish: (pub)=>this.publishProviderModels(...),
  allowNetwork, force: allowNetwork ? force : undefined, signal, baseUrl: provider.baseUrl })`。

**`refresh(options = {})`（核心算法，必须逐字复刻顺序）**

1. `allowNetwork = options.allowNetwork ?? true`；`callerSignal = operationSignal(options.signal)`；
   `errors = new Map<string, Error>()`；若 `callerSignal.aborted` 立即返回 `{ aborted: true, errors }`。
2. `selected = options.providers ? new Set(options.providers) : undefined`。
3. `refreshable = providers.values()` 中 `refreshModels !== undefined` 且（未选择或 `selected.has(id)`）的 provider。
4. 对每个 refreshable 并发：
   - `{generation, controller} = beginProviderRefresh(id)`；`signal = AbortSignal.any([callerSignal, controller.signal])`。
   - 操作体：先 `readCredential`（失败存下 `credentialError`，不立即抛）；然后
     **先恢复缓存**：`runProviderRefreshPhase(provider, storedCredential, false, undefined, generation, signal)`；
     若 `credentialError !== undefined` 抛出它；若 `!allowNetwork || signal.aborted` 返回；
     否则 `credential = await resolveRefreshCredential(provider, storedCredential, signal)`，为 undefined 则返回；
     再 `runProviderRefreshPhase(provider, credential, true, options.force, generation, signal)`。
   - `raceWithAbortSignal(operation, signal)`；catch 时若 `!signal.aborted` 记录错误：
     `error instanceof Error ? error : new ModelsError("model_source", \`Model refresh failed for ${provider.id}\`, {cause: error})`。
   - finally：若当前 controller 仍是该 provider 的，则删除。
5. `await raceWithAbortSignal(Promise.all(refresh), callerSignal)`；catch 时若 callerSignal 未 abort 则重新抛出。
6. 返回 `{ aborted: callerSignal.aborted, errors: new Map(errors) }`。

注意：**缓存恢复阶段先于鉴权**，即使无凭证或离线也执行；`force` 只在 `allowNetwork` 为真时传递。

**`resolveRefreshCredential(provider, stored, signal)`**

- `stored?.type === "oauth"`：无 `provider.auth.oauth` 返回 undefined；`Date.now() < stored.expires` 直接返回 stored；
  `signal.aborted` 返回 undefined；否则 `refreshStoredOAuthCredential(credentials, id, oauth, (cur)=>Date.now()>=cur.expires, signal)`
  （注释：已开始的刷新会存活于取消或更新的模型刷新，保证轮换后的 refresh token 一定被持久化）。
- 否则取 `provider.auth.apiKey`；无则 undefined；`credential = stored?.type === "api_key" ? stored : undefined`；
  `result = await apiKey.resolve({ctx, credential, signal})`；undefined 返回 undefined；
  否则返回 `{ type: "api_key", key: result.auth.apiKey, env: result.env }`。

**`readCredential(providerId, signal)`**：`credentials.read` 抛错包装为
`new ModelsError("auth", \`Credential store read failed for ${providerId}\`, {cause})`。

**`checkProviderAuth(provider, credential, signal)`**

- stored oauth：有 `auth.oauth` 返回 `{ source: "OAuth", type: "oauth" }`，否则 undefined。
- 无 apiKey 返回 undefined。
- 有 `apiKey.check`：调用它（credential 仅当 `type==="api_key"`），抛错包装
  `ModelsError("auth", \`API key auth check failed for provider ${provider.id}\`)`。
- 否则 `resolveProviderAuth(provider, credentials, authContext, {signal})`，有结果返回 `{source: resolution.source, type:"api_key"}`。

**`checkAuth(providerId, options?)`**：`signal = operationSignal`；内部 async 先 `signal.throwIfAborted()`，
未知 provider 返回 undefined，否则 `checkProviderAuth(provider, await readCredential(...), signal)`；结果与 signal race。

**`getAuthenticatedProviders(providerId | undefined, signal)`**：throwIfAborted；按 id 取单个（过滤 undefined）或全量；
并发 `readCredential` + `checkProviderAuth`；只保留 `auth !== undefined` 的项。

**`getAvailable(providerId?, options?)`**：对已鉴权 provider，`models = provider.getModels()`，
`provider.filterModels?.(models, credential) ?? models`，flatMap。

**`getAllAvailable(providerId?, options?)`**：`models = provider.getAllModels?.() ?? provider.getModels()`；
若 `filterAllModels` 用它；否则若无 `filterModels` 原样返回；否则用 `filterModels(getModels(), credential)` 得到
可用 chat id 集合，非 chat 模型保留、chat 模型按 id 过滤。

**`getAvailableOfType`**：`(await getAllAvailable(...)).filter(isModelType(type))`。

**`getAuth(providerId | model, overrides?)`**：`signal = operationSignal(overrides?.signal)`；providerId 从字符串或
`model.provider` 取；未知 provider 返回 undefined；`resolveProviderAuth(provider, credentials, authContext, {...overrides, signal})`；
若传入的是 model 且 `model.headers` 存在，返回 `{...result, auth: {...result.auth, headers: mergeHeaders(result.auth.headers, model.headers)}}`。

**`login(providerId, type, interaction, options?)`**（完整并发语义，必须复刻）

1. `signal = operationSignal(interaction.signal)`；`throwIfAborted`；未知 provider ->
   `ModelsError("provider", \`Unknown provider: ${providerId}\`)`。
2. `method = type === "oauth" ? auth.oauth : auth.apiKey`；无 `method?.login` ->
   `ModelsError("auth", \`${provider.name} does not support ${type} login\`)`。
3. `loginOperation = method.login({...interaction, signal}, options)`；`credential = await raceWithAbortSignal(loginOperation, signal)`。
4. `mutationStarted = false`；`started` promise 由 `markMutationStarted` 解析；
   `mutation = credentials.modify(providerId, async () => { mutationStarted = true; markMutationStarted?.(); return credential; }, {signal})`；
   `void mutation.catch(()=>{})`。
5. 等待 `Promise.race([started, mutation])`，但外部注册 `abort` 监听：若 abort 发生时 `!mutationStarted` 则 reject
   `signal.reason`；settle 后移除监听。若 signal 已 abort，立即调用 onAbort。
6. `await mutation`；catch -> `signal.throwIfAborted()` 后抛 `ModelsError("auth", \`Credential store modify failed for ${providerId}\`, {cause})`。
7. 返回 credential。
语义：登录成功后**写盘（modify）一旦开始就不可取消**，未开始时可因取消而放弃，避免半写。

**`logout(providerId, options?)`**：throwIfAborted；`credentials.delete`；catch -> throwIfAborted 后
`ModelsError("auth", \`Credential store delete failed for ${providerId}\`)`。

**`requireProvider(model)`**：未知 -> `ModelsError("provider", \`Unknown provider: ${model.provider}\`)`。
**`requireChatProvider(model)`**：先 `assertChatModel(model)`。

**`applyAuth(model, options)`（请求路径的鉴权与头合并）**

1. `requireProvider(model)`。
2. `resolution = getAuth(model, { apiKey: options?.apiKey, env: options?.env, signal: options?.signal })`；
   为 undefined -> `ModelsError("auth", \`Provider is not configured: ${model.provider}\`)`。
3. `apiKey = options?.apiKey ?? auth.apiKey`；`requestModel = auth.baseUrl ? {...model, baseUrl: auth.baseUrl} : model`。
4. `sessionId` 仅当 options 有该字段且为 string。
5. `headers = mergeHeaders(mergeHeaders(getOpenCodeSessionHeaders(requestModel, sessionId), auth.headers), options?.headers)`；
   若有 `options.transformHeaders`：`headers = await options.transformHeaders(headers ?? {})`。
6. `env = resolution.env || options?.env ? {...(resolution.env ?? {}), ...(options?.env ?? {})} : undefined`（请求 env 覆盖 provider env）。
7. `identity = mergeClientIdentity(this.identity, options?.identity)`。
8. 从 options 中剥离 `transformHeaders`，其余原样透传，并附加 `apiKey`、`headers`、`env`、以及（identity 非 undefined 时）`identity`。

**流式入口**

- `stream(model, context, options?)`：`transcript = normalizeContext(context)`（同步，发生在返回流之前，见 2.2.27）；
  返回 `lazyStream(model, async () => { const provider = requireChatProvider(model); const {requestModel, requestOptions} =
  await applyAuth(model, options); return provider.stream(requestModel, transcript, requestOptions); },
  observeModelRequest(model, options, false))`。
- `complete(...)`：`stream(...).result()`。
- `streamSimple` / `completeSimple`：同上，`observeModelRequest(model, options, false)`。
- `streamDeferred`：在 lazy setup 内先 requireChatProvider；无 `provider.fetchDeferred` ->
  `ModelsError("provider", \`Provider ${model.provider} does not support deferred responses\`)`；
  然后 applyAuth 并调用；observer 的 `poll` 参数为 `true`。
- `fetchDeferred`：`streamDeferred(...).result()`。
- `cancelDeferred`：requireChatProvider；无 `cancelDeferred` -> 同一条 deferred 错误消息；applyAuth 后 await 调用。
- 注意 `stream()`/`streamSimple()` 的 lazy setup 中 `requireChatProvider` 先于 `applyAuth`，因此非 chat 模型的错误是
  `Model {provider}/{id} is not a chat model`，未注册 provider 的错误是 `Unknown provider: {id}`。

**`generateImages(model, context, options?)`**：整体 try/catch，catch 返回
`imageErrorResult(model, error, options?.signal?.aborted)`。try 内顺序：`assertImageModel` -> `requireProvider` ->
无 `generateImages` -> `ModelsError("provider", \`Provider ${model.provider} does not support image generation\`)` ->
`applyAuth` -> `await provider.generateImages(...)`。

**`classify(model, context, options?)`**：try/catch 返回 `classifierErrorResult`。顺序：`assertClassifierModel` ->
`assertClassifierInputSupported(model, context)` -> `requireProvider` -> 无 `classify` ->
`ModelsError("provider", \`Provider ${model.provider} does not support classification\`)` -> `applyAuth` -> await。

**`createProvider(input)`（构建 Provider，含动态模型合并与 API 分发）**

1. `single`：若 `input.api` 且有 `stream` 函数则视为单一实现，否则 undefined。
   `byApi`：无 single 且有 input.api 时视为 map。`images`/`classifiers` 按 api 映射。
   `streams` = `[single]` 或 `Object.values(byApi ?? {}).filter(defined)`；
   `imageImplementations`、`classifierImplementations` 同理。
2. 三者皆空 -> `throw new Error(\`Provider ${input.id}: at least one of "api", "images", or "classifiers" is required.\`)`
   （注意：普通 `Error`，不是 `ModelsError`）。
3. `baselineModels = input.models`；`dynamicModels = []`；`currentModels()`：
   从 baseline 的浅拷贝开始，对每个 dynamic 模型：以 `(getModelType(entry) === getModelType(model) && entry.id === model.id)`
   找第一个位置，命中则**原位替换**，否则 push（即动态覆盖基线，顺序沿用基线位置）。
4. `apiFor(model) = single ?? byApi?.[model.api]`。
5. `dispatch(model, run)`：无实现时返回 `lazyStream(model, async () => { throw new ModelsError("stream",
   \`Provider ${input.id} has no API implementation for "${model.api}"\`) })`。
6. provider 对象：`name = input.name ?? input.id`；`getModels` = chat 过滤；`getAllModels` = currentModels。
   `refreshModels`（仅当有 `fetchModels`）：
   - 若 `context.stored`：取 `stored.models` 中 `provider === input.id` 的项转为动态模型；`publish({update})` 返回 false 则 return。
   - 若 `!context.allowNetwork || context.signal.aborted` return。
   - `fetched = await fetchModels(context)`；若 aborted return；`refreshed = fetched.filter(hasKnownModelType)`；
     `publish({ persist: { models: refreshed, checkedAt: Date.now() }, update: () => dynamicModels = refreshed })`。
7. 若任一 stream 实现带 `fetchDeferred`：provider 增加 `fetchDeferred`，内部 `lazyStream` 中若 `apiFor(model)?.fetchDeferred`
   缺失 -> `ModelsError("provider", \`Provider ${input.id} does not support deferred responses for "${model.api}"\`)`。
8. 若任一实现带 `cancelDeferred`：provider 增加 `cancelDeferred`，缺失时抛
   `ModelsError("provider", \`Provider ${input.id} cannot cancel deferred responses for "${model.api}"\`)`。
9. 有 images 实现：`generateImages` 在 `images[model.api]` 缺失时返回
   `imageErrorResult(model, new ModelsError("provider", \`Provider ${input.id} has no image generation implementation for "${model.api}"\`))`。
10. 有 classifiers 实现：`classify` 同理返回 `classifierErrorResult(... \`Provider ${input.id} has no classifier implementation for "${model.api}"\`)`。

**`hasApi(model, api)`**：`isModelType(model,"chat") && model.api === api`（非 chat 模型即使 api 相同也不匹配）。

**`calculateCost(model, usage)`**

```
inputTokens = usage.input + usage.cacheRead + usage.cacheWrite
rates = model.cost；matchedThreshold = -1
for tier of model.cost.tiers ?? []:
    if inputTokens > tier.inputTokensAbove && tier.inputTokensAbove > matchedThreshold: rates = tier; matchedThreshold = tier.inputTokensAbove
longWrite = usage.cacheWrite1h ?? 0; shortWrite = usage.cacheWrite - longWrite
usage.cost.input      = rates.input      / 1e6 * usage.input
usage.cost.output     = rates.output     / 1e6 * usage.output
usage.cost.cacheRead  = rates.cacheRead  / 1e6 * usage.cacheRead
usage.cost.cacheWrite = (rates.cacheWrite * shortWrite + rates.input * 2 * longWrite) / 1e6
usage.cost.total      = input + output + cacheRead + cacheWrite
usage.cost.totalFromProvider = false
return usage.cost
```
（原地修改 `usage.cost`。1h cache write 按 2x input 计价。阈值比较是"严格大于"，tier 取"满足条件的最高阈值"。）

**`getSupportedThinkingLevels(model)`**：`!model.reasoning` 返回 `["off"]`；否则按
`["off","minimal","low","medium","high","xhigh","max"]` 过滤：`thinkingLevelMap[level] === null` 排除；
`xhigh`/`max` 额外要求 `mapped !== undefined`（必须显式映射）。

**`clampThinkingLevel(model, level)`**：可用列表包含则原样返回；未知 level（不在扩展顺序里）返回 `available[0] ?? "off"`；
否则先向后（更高级）找第一个可用，再向前（更低级）找，最后 `available[0] ?? "off"`。

**`modelsAreEqual(a,b)`**：任一 falsy 返回 false；否则 `type`、`id`、`provider` 三者相等。

#### 2.1.4 `src/models-store.ts`（46 行）

```ts
export interface ModelsStoreEntry { models: readonly AnyModel[]; lastModified?: number; checkedAt?: number; etag?: string }
export interface ModelsStoreOperationOptions { signal?: AbortSignal }
export interface ModelsStore {
  read(providerId, options?): Promise<ModelsStoreEntry | undefined>;
  write(providerId, entry, options?): Promise<void>;
  delete(providerId, options?): Promise<void>;
}
export class InMemoryModelsStore implements ModelsStore
```
`etag` 注释：远端目录的 ETag，**逐字保存（含引号）**，作为 `If-None-Match` 回传。
`InMemoryModelsStore` 每个方法先 `options?.signal?.throwIfAborted()`；读/写都做 `structuredClone`（写时克隆入、读时克隆出）。

#### 2.1.5 `src/session-resources.ts`（24 行）

```ts
export type SessionResourceCleanup = (sessionId?: string) => void;
export function registerSessionResourceCleanup(cleanup): () => void   // 返回注销函数
export function cleanupSessionResources(sessionId?: string): void
```
`cleanupSessionResources` 遍历 Set 中所有 cleanup，逐个 try/catch 收集错误；若有错误抛
`new AggregateError(errors, "Failed to cleanup session resources")`。用途：Codex WebSocket 连接池等按 session 的资源。

#### 2.1.6 `src/image-models.ts`（50 行，@deprecated 兼容读取）

由 `IMAGE_MODELS` 构建 `imageModelsByProvider: Map<string, Map<string, ImageModel<ImageApi>>>`（模块加载时，仅保留非空 provider）。
`getImageModel(provider, modelId)`（找不到返回 undefined）、`getImageProviders()`（provider id 数组）、
`getImageModels(provider)`（模型数组，缺失返回 `[]`）。三个函数均标注 `@deprecated`，新代码用 `getBuiltinImageModel` /
`Models.getModelOfType("image", ...)`。

#### 2.1.7 `src/images-api-registry.ts`（53 行）

```ts
export type ImagesApiFunction = (model, context: ImagesContext, options?: ImagesOptions) => Promise<AssistantImages>;
export interface ImagesApiProvider<TApi, TOptions> { api: TApi; generateImages: ImagesFunction<TOptions> }
export function registerImagesApiProvider(provider, sourceId?): void
export function getImagesApiProvider(api): ImagesApiProviderInternal | undefined
```
注册时用 `wrapGenerateImages` 包装：若 `model.api !== api` 抛 `Error(\`Mismatched api: ${model.api} expected ${api}\`)`。
模块级 `Map<string, {provider, sourceId?}>`；后注册覆盖前者。

#### 2.1.8 `src/images.ts`（26 行，旧全局入口，副作用）

导入即执行 `import "./providers/images/register-builtins.ts"`（注册 `openrouter-images`）。
`generateImages(model, context, options?)`：从注册表取 api 实现，缺失抛
`Error(\`No API provider registered for api: ${api}\`)`；否则委派。文档强调：必须显式传 `options.apiKey`，
新代码用 `Models.generateImages()`。

#### 2.1.9 `src/legacy-api-aliases.ts`（108 行）

模块加载时即构造 8 个 `*Api()`（来自 `api/*.lazy.ts`），并导出其 `stream`/`streamSimple` 作为旧别名：
`streamAnthropic`、`streamSimpleAnthropic`、`streamAzureOpenAIResponses`、`streamSimpleAzureOpenAIResponses`、
`streamGoogle`、`streamSimpleGoogle`、`streamGoogleVertex`、`streamSimpleGoogleVertex`、`streamMistral`、
`streamSimpleMistral`、`streamOpenAICodexResponses`、`streamSimpleOpenAICodexResponses`、`streamOpenAICompletions`、
`streamSimpleOpenAICompletions`、`streamOpenAIResponses`、`streamSimpleOpenAIResponses`（共 16 个常量，均 `@deprecated`）。

#### 2.1.10 `src/model-catalog.ts`（81 行）——生成数据的分组与扁平化

```ts
export type ModelGroups = Record<string, Record<string, object>>;    // api -> key(type:id) -> model
export type ChatModelCatalog<TGroups, TProvider> = ...   // 类型级
export type ImageModelCatalog<...>; export type ClassifierModelCatalog<...>;
export function flattenChatModelCatalog(provider, groups)       // 运行时：Object.fromEntries(values(groups).flatMap(values).filter(type===x).map([id, model]))
export function flattenImageModelCatalog(provider, groups)
export function flattenClassifierModelCatalog(provider, groups)
```
运行时只用 `model.type === type` 与 `model.id` 作键；`provider` 参数仅用于类型推断（运行时未使用）。
同一 id 在多个 api 组出现时，后者覆盖前者（`Object.fromEntries` 语义）——但生成器保证每 provider 内 `type:id` 唯一。

#### 2.1.11 `src/compat.ts`（302 行）——旧全局 API（有副作用）

`export *` 自：10 个 `api/*.lazy.ts`、`env-api-keys.ts`、`image-models.ts`、`images.ts`、`images-api-registry.ts`、
`index.ts`、`legacy-api-aliases.ts`、`providers/images/register-builtins.ts`；`export type { BuiltinProvider }`。

- `getModel = getBuiltinModel`、`getModels = getBuiltinModels`、`getProviders = getBuiltinProviders`（均 @deprecated）。
- api 注册表：`Map<string, {provider: {api, stream, streamSimple}, sourceId?}>`；
  `registerApiProvider(provider, sourceId?)` 用 `wrapStream`/`wrapStreamSimple` 包装（api 不匹配抛
  `Error(\`Mismatched api: ${model.api} expected ${api}\`)`）；`getApiProvider(api)`、`getApiProviders()`、
  `unregisterApiProviders(sourceId)`（按 sourceId 删除）、内部 `clearApiProviders()`。
- `registerFauxProvider(options = {})`：`createFauxCore(options)`；sourceId = `` `faux-provider-${Math.random().toString(36).slice(2, 10)}` ``；
  注册后返回 `{ api, models, getModel, state, setResponses, appendResponses, getPendingResponseCount, unregister }`。
- `BUILTIN_APIS`（顺序固定）：anthropic-messages、openai-completions、openai-responses、openai-codex-responses、
  azure-openai-responses、google-generative-ai、google-vertex、mistral-conversations、bedrock-converse-stream、kiana-messages。
- `registerBuiltInApiProviders()`：**不覆盖已存在条目**（若 `getApiProvider(api)` 已存在则跳过注册），
  但把当前实例记入 `builtinApiProviderInstances`。
- `resetApiProviders()`：清空注册表与实例表，再调用 `registerBuiltInApiProviders()`。
- 模块加载时立即调用一次 `registerBuiltInApiProviders()`（这是 `./compat` 的 side effect）。
- `compatModels = builtinModels()`（模块级）；`AMBIENT_AUTH_MARKER = "<authenticated>"`。
- `hasExplicitApiKey(k)`：`typeof k === "string" && k.trim().length > 0`。
- `withEnvApiKey(model, options)`：已有显式 key 则原样；否则 `getEnvApiKey(model.provider, options?.env)`，
  无值或等于 `<authenticated>` 则原样；否则 `{...options, apiKey}`。
- `hasResolvedCloudflareAuth(options)`：有显式 apiKey 或 `options.headers["cf-aig-authorization"]` 是 string。
- `getBuiltinProviderForModel(model)`：仅当 `getApiProvider(model.api)` 与记录的内置实例**同一引用**，
  且 `compatModels.getProvider(model.provider)?.getModels()` 中存在同 api 的模型时返回该 provider。
- `resolveApiProvider(api)`：缺失抛 `Error(\`No API provider registered for api: ${api}\`)`。
- `stream(model, context, options?)`：先 `normalizeContext`；若有内置 provider：
  若 `model.provider.startsWith("cloudflare-")` 且 `!hasResolvedCloudflareAuth(options)` -> 走 `compatModels.stream(...)`；
  否则走 `builtinProvider.stream(model, transcript, withEnvApiKey(model, options))`。否则走注册表 + withEnvApiKey。
- `complete` = `stream().result()`；`streamSimple`/`completeSimple` 同理（cloudflare 分支用 `compatModels.streamSimple`）。

#### 2.1.12 `src/env-api-keys.ts`（195 行）

**关键**：顶部注释 `NEVER convert to top-level imports - breaks browser/Vite builds`。用 `node:" + "fs"` 之类的
**拼接 specifier** 做动态 import，避免打包器解析；仅在 `process.versions.node || process.versions.bun` 时预加载
`existsSync` / `homedir` / `join`。

导出常量（逐字）：`ANTHROPIC_AUTH_TOKEN_ENV = "ANTHROPIC_AUTH_TOKEN"`、`ANTHROPIC_OAUTH_TOKEN_ENV`、
`ANTHROPIC_API_KEY_ENV`、`ANTHROPIC_FEDERATION_RULE_ID_ENV = "ANTHROPIC_FEDERATION_RULE_ID"`、
`ANTHROPIC_ORGANIZATION_ID_ENV`、`ANTHROPIC_SERVICE_ACCOUNT_ID_ENV`、`ANTHROPIC_IDENTITY_TOKEN_FILE_ENV`、
`ANTHROPIC_WORKSPACE_ID_ENV`。

- `cachedVertexAdcCredentialsExists: boolean | null`。
- `hasVertexAdcCredentials(env?)`：
  1. `env?.GOOGLE_APPLICATION_CREDENTIALS` 存在 -> 返回 `_existsSync ? _existsSync(path) : false`（**不缓存**）。
  2. 缓存为 null 时：若 fs 模块尚未加载（启动竞态），且**不是** Node/Bun（浏览器）才永久缓存 false；
     非浏览器则返回 false 且**不缓存**（下次重试）。
  3. 否则先看 `getProviderEnvValue("GOOGLE_APPLICATION_CREDENTIALS", env)`，有则 `existsSync(path)`；
     否则 `existsSync(join(homedir(), ".config", "gcloud", "application_default_credentials.json"))`。写入缓存。
  4. 返回缓存值。
- `getApiKeyEnvVars(provider)`：`github-copilot` -> `["COPILOT_GITHUB_TOKEN"]`；`anthropic` ->
  `[ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_API_KEY_ENV]`（注意顺序，AUTH_TOKEN 参与发现/状态，
  但 `getEnvApiKey` 跳过它）；其余见下表（未知 provider 返回 undefined）。

| provider | env 变量 |
|---|---|
| ant-ling | `ANT_LING_API_KEY` |
| qwen-token-plan | `QWEN_TOKEN_PLAN_API_KEY` |
| qwen-token-plan-cn | `QWEN_TOKEN_PLAN_CN_API_KEY` |
| qwen-token-plan-individual | `QWEN_TOKEN_PLAN_API_KEY` |
| openai | `OPENAI_API_KEY` |
| azure | `AZURE_OPENAI_API_KEY` |
| nvidia | `NVIDIA_API_KEY` |
| deepseek | `DEEPSEEK_API_KEY` |
| google | `GEMINI_API_KEY` |
| google-vertex | `GOOGLE_CLOUD_API_KEY` |
| groq | `GROQ_API_KEY` |
| cerebras | `CEREBRAS_API_KEY` |
| xai | `XAI_API_KEY` |
| typesafe | `TYPESAFE_API_KEY` |
| radius | `RADIUS_API_KEY` |
| openrouter | `OPENROUTER_API_KEY` |
| vercel-ai-gateway | `AI_GATEWAY_API_KEY` |
| zai | `ZAI_API_KEY` |
| zai-coding-cn | `ZAI_CODING_CN_API_KEY` |
| mistral | `MISTRAL_API_KEY` |
| minimax | `MINIMAX_API_KEY` |
| minimax-cn | `MINIMAX_CN_API_KEY` |
| moonshotai / moonshotai-cn | `MOONSHOT_API_KEY` |
| huggingface | `HF_TOKEN` |
| fireworks | `FIREWORKS_API_KEY` |
| together | `TOGETHER_API_KEY` |
| baseten | `BASETEN_API_KEY` |
| opencode / opencode-go | `OPENCODE_API_KEY` |
| kimi-coding | `KIMI_API_KEY` |
| meta | `META_API_KEY` |
| cloudflare-workers-ai / cloudflare-ai-gateway | `CLOUDFLARE_API_KEY` |
| xiaomi | `XIAOMI_API_KEY` |
| xiaomi-token-plan-cn | `XIAOMI_TOKEN_PLAN_CN_API_KEY` |
| xiaomi-token-plan-ams | `XIAOMI_TOKEN_PLAN_AMS_API_KEY` |
| xiaomi-token-plan-sgp | `XIAOMI_TOKEN_PLAN_SGP_API_KEY` |

（`amazon-bedrock` 无 env key 变量，仅有 ambient 凭证检测。）

- `findEnvKeys(provider, env?)`：取变量表；不存在返回 undefined；过滤 `!!getProviderEnvValue(var, env)`；
  空数组返回 undefined。
- `getEnvApiKey(provider, env?)`：
  1. `findEnvKeys` 有结果时：anthropic 选**第一个不是** `ANTHROPIC_AUTH_TOKEN` 的键，其余选第一个；有则返回值。
  2. `google-vertex`：`hasVertexAdcCredentials(env)` 且（`GOOGLE_CLOUD_PROJECT` 或 `GCLOUD_PROJECT`）且
     `GOOGLE_CLOUD_LOCATION` 三者齐备 -> 返回 `"<authenticated>"`。
  3. `amazon-bedrock`：`AWS_PROFILE` 或（`AWS_ACCESS_KEY_ID` 且 `AWS_SECRET_ACCESS_KEY`）或 `AWS_BEARER_TOKEN_BEDROCK`
     或 `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` 或 `AWS_CONTAINER_CREDENTIALS_FULL_URI` 或 `AWS_WEB_IDENTITY_TOKEN_FILE`
     任一存在 -> `"<authenticated>"`。
  4. 否则 undefined。

#### 2.1.13 `src/cli.ts`（124 行，bin `kiana-ai`）

- `AUTH_FILE = "auth.json"`（**当前工作目录**，非 home）。
- `PROVIDERS`：`builtinProviders().filter(p => p.auth.oauth !== undefined)`（即 9 个：anthropic、openai、openai-codex、
  github-copilot、openrouter、kimi-coding、meta、xai、radius（radius 通过 `radiusProvider()`，其 oauth 由 `lazyOAuth` 提供））。
- `loadAuth()`：文件不存在返回 `{}`；JSON 解析失败返回 `{}`；类型视为 `Record<string, OAuthCredential>`。
- `saveAuth(auth)`：`writeFileSync(AUTH_FILE, JSON.stringify(auth, null, 2), "utf-8")`（2 空格缩进，无尾随换行）。
- `answerPrompt(rl, authPrompt)`：
  - `select`：打印 `\n${message}`，然后每个选项 `  ${index+1}. ${label}`；读 `Enter number (1-${n}): `，
    `Number.parseInt(...,10) - 1`；无效（含 NaN）抛 `Error("Invalid selection")`；返回 `selected.id`。
  - 其他：读 `` `${message}${placeholder ? ` (${placeholder})` : ""}: ` ``。
- `login(providerId)`：未知 provider -> `Error(\`Unknown provider: ${providerId}\`)`；创建 readline；
  调 `provider.auth.oauth.login({signal: new AbortController().signal, prompt, notify}, { getDeviceId: randomUUID })`。
  notify 分支逐字：
  - `auth_url`：`\nOpen this URL in your browser:\n${event.url}`，若 `instructions` 则再打印；
  - `device_code`：`\nOpen this URL in your browser:\n${event.verificationUri}` 与 `Enter code: ${event.userCode}`；
  - `info` / `progress`：`event.message`。
  成功后 `loadAuth()`，`auth[providerId] = credential`，`saveAuth`，打印 `\nCredentials saved to ${AUTH_FILE}`，最后 `rl.close()`。
- `main()`：
  - 无命令或 `help`/`--help`/`-h`：打印
    ```
    Usage: npx @earendil-works/kiana-ai <command> [provider]

    Commands:
      login [provider]  Login to an OAuth provider
      list              List available providers

    Providers:
    ```
    后接每个 `` `  ${provider.id.padEnd(20)} ${provider.name}` ``（按内置顺序），整体单次 `console.log`。
  - `list`：逐行 `` `${provider.id.padEnd(20)} ${provider.name}` ``。
  - `login [provider]`：无 provider 时交互选择：打印 `  ${index+1}. ${name}`，读 `Enter number (1-${n}): `；
    再校验存在，否则 `Error(\`Unknown provider: ${providerId ?? ""}\`)`；然后 `login`。
  - 其他：`Error(\`Unknown command: ${command}\`)`。
- 顶层 `main().catch(e => { console.error("Error:", e instanceof Error ? e.message : String(e)); process.exit(1); })`。
  → 退出码 1，输出前缀 `Error: `。

#### 2.1.14 `src/telemetry.ts`（210 行）

依赖 `@earendil-works/kiana-telemetry`：`createTypedMetrics`、`createTypedSpanStarter`、`defineTelemetrySchema`、
`NOOP_TELEMETRY_CONTEXT`、`NOOP_TELEMETRY_METRICS`、类型 `SchemaTelemetrySpan`、`TelemetryMetrics`、`TelemetrySpan`。

`AI_TELEMETRY_SCHEMA`（version 1）逐字结构：

- spans：`kiana.ai.request` 与 `kiana.ai.poll`（后者 `description: "One deferred-response poll, not the deferred request's lifetime."`，其余同 request）。
  - `description: "One logical model request, including authentication and provider setup."`，`parents: { kind: "any" }`。
  - startAttributes：`provider`（string，required，desc "Provider identifier."）、`api`（string，required，"API identifier."）、
    `model`（string，required，"Requested model identifier."）、`reasoning`（string，optional，"Scalar requested reasoning level."）。
  - endAttributes：`stop`（string，"Normalized stop reason."，values 全部 7 个 StopReason）、`usage.input`（"Uncached input tokens."）、
    `usage.output`、`usage.cache_read`、`usage.cache_write`、`cost.total`（"Total request cost in USD."）、
    `response.id`（string，"Provider response identifier."，`cardinality: "high"`）。
  - events：`kiana.ai.first_chunk`（"First normalized stream event delivered."，attributes {}）；
    `kiana.ai.block`（"A completed content block; content is never recorded."，attributes：`type`（string，values `text|thinking|tool_call`，required，"Block kind."）、
    `index`（number，required，"Content block index."）、`chars`（number，required，"UTF-16 character count."）、
    `tool`（string，optional，"Tool name, for tool-call blocks only."））。
  - status：`default: "ok"`，`errorWhen: "Stop reason is error: provider_error with at most 500 error-message characters. Aborted stays ok."`。
- metrics：
  - `kiana.ai.request.duration`：histogram，unit `ms`，"Logical request or poll duration."，attributes provider/model/`stop`(required)。
  - `kiana.ai.first_chunk.duration`：histogram，`ms`，"Time to the first normalized stream event."，provider/model。
  - `kiana.ai.tokens`：counter，unit `token`，"Reported model token usage; pending and deferred polls are excluded."，
    provider/model/`kind`（values `input|output|cache_read|cache_write`，required）。
  - `kiana.ai.cost`：counter，unit `USD`，"Reported model cost; pending and deferred polls are excluded."，provider/model。

`createInstruments(adapter)` 创建 4 个 instrument；`instrumentCache: WeakMap<TelemetryMetrics, ...>`（每个 metrics 对象只创建一次，
root 与 child 请求共享）。

`observeModelRequest(requestedModel, options, poll): LazyStreamObserver`：

- `started = performance.now()`；`attributes = { provider, model: id }`；`adapter = options?.telemetry?.metrics ?? NOOP_TELEMETRY_METRICS`；
  取/建 instruments。
- `span`、`finish`、`first = true`、`toolChars: Map<number, number>`；`finished` promise 由 `finish` 解析；
  `run(activeSpan)` 记录 span 并返回 `finished`。
- `startAttributes = { ...attributes, api, ...(typeof options?.reasoning === "string" ? { reasoning } : {}) }`
  （provider 原生 reasoning 对象被忽略）。
- `start = createTypedSpanStarter(options?.telemetry ?? NOOP_TELEMETRY_CONTEXT, [AI_TELEMETRY_SCHEMA])`；
  `void start(poll ? "kiana.ai.poll" : "kiana.ai.request", startAttributes, run)`。
- 返回 observer：
  - `onEvent(event)`：首个事件 -> `span.addEvent("kiana.ai.first_chunk")` 并 `firstChunkDuration.record(performance.now()-started, attributes, span)`；
    `toolcall_delta` 累加 `event.delta.length` 到 `toolChars[contentIndex]`（不解析参数）；
    `text_end`/`thinking_end` -> `kiana.ai.block`，`type` 为 `text`/`thinking`，`chars = event.content.length`；
    `toolcall_end` -> 取并删除 `toolChars`，`kiana.ai.block` 带 `type:"tool_call"`、`chars`、`tool: event.toolCall.name`。
  - `onResult(message)`：若 message 存在，`span.setAttributes({ stop, usage.input, usage.output, usage.cache_read,
    usage.cache_write, cost.total, response.id })`；`stopReason === "error"` 时 `span.setStatus({status:"error",
    error:{ name:"provider_error", message: ownedSlice(errorMessage ?? "", 0, 500) }})`；
    `requestDuration.record(now-started, {...attributes, stop}, span)`；
    若 `!poll || (stopReason !== "pending" && stopReason !== "deferred")` 记录 4 个 token counter 与 cost counter。
    最后 `finish()`。

#### 2.1.15 `src/oauth.ts` / `src/bun-oauth.ts` / `src/bedrock-provider.ts` / `src/compat/extension-oauth-types.ts`

- `oauth.ts`：仅 `export type { OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt,
  OAuthSelectOption, OAuthSelectPrompt } from "./compat/extension-oauth-types.ts"`（`OAuthCredentials` 最终来自 `auth/types.ts`）。
- `extension-oauth-types.ts`：旧扩展回调类型：`OAuthPrompt { message; placeholder?; allowEmpty? }`、
  `OAuthAuthInfo { url; instructions? }`、`OAuthDeviceCodeInfo { userCode; verificationUri; openBrowserOnConfirm?;
  copyCodeOnConfirm?; intervalSeconds?; expiresInSeconds? }`、`OAuthSelectOption { id; label }`、
  `OAuthSelectPrompt { message; options }`、`OAuthLoginCallbacks { onAuth; onDeviceCode; onPrompt; onProgress?;
  onManualCodeInput?; onSelect; signal? }`。
- `bun-oauth.ts`：`registerBunOAuthFlows()` 调 `registerBundledOAuthFlowLoaders({anthropic, openaiCodex, openaiChatGPT,
  githubCopilot, openrouter, kimiCoding, meta, xai, radius: createRadiusOAuth})`（静态内嵌到 Bun 单文件二进制）。
- `bedrock-provider.ts`：`export const bedrockProviderModule = { stream, streamSimple }`（来自 `api/bedrock-converse-stream.ts`）。

#### 2.1.15b `src/providers/*.ts` 之外的其余根文件补充说明

`src/compat.ts` 的模块级副作用与顺序（易被忽略；**顺序已按源码 213–218 行核对**）：
1. `export *` 自 10 个 `api/*.lazy.ts`、`env-api-keys.ts`、`image-models.ts`、`images.ts`、`images-api-registry.ts`、
   `index.ts`、`legacy-api-aliases.ts`、`providers/images/register-builtins.ts`（**导入 `compat.ts` 即注册
   `openrouter-images`**）。
2. `registerBuiltInApiProviders();`（源码 **215 行**，模块级调用，注册 10 个内置 API）——**这一步先发生**。
3. 之后才是 `const compatModels = builtinModels()`（源码 **217 行**，注册全部 42 个 provider）与
   `const AMBIENT_AUTH_MARKER = "<authenticated>"`（218 行）。
即：`registerBuiltInApiProviders()` **早于** `compatModels` 的构造，而不是"文件末尾"。Rust 若按模块初始化顺序移植，
必须保持"先注册 api providers、再建 42 provider 集合"的次序（`builtinModels()` 内部会在 `setProvider` 时不依赖注册表，
但 `resetApiProviders()` 依赖 `builtinApiProviderInstances` 已被填充）。
`registerFauxProvider` 的 `sourceId` 为 `` `faux-provider-${Math.random().toString(36).slice(2, 10)}` ``
（8 个 base36 字符）；`unregister()` 按 sourceId 删除该 api 条目。

#### 2.1.16 `src/providers/model-schema.ts`（99 行）

```ts
export const THINKING_LEVELS = ["minimal","low","medium","high","xhigh","max"] as const;
export const MODEL_THINKING_LEVELS = ["off", ...THINKING_LEVELS] as const;   // 7 项
export const MODEL_INPUT_MODALITIES = ["text","image"] as const;
export const CACHE_RETENTIONS = ["none","short","long"] as const;
export const MODEL_PROMPT_CACHE_RETENTIONS = ["short","long"] as const;
export const ThinkingLevelSchema / ModelThinkingLevelSchema / ModelInputModalitySchema / CacheRetentionSchema  // Type.Enum
export const ThinkingLevelMapSchema = Type.Partial(Type.Record(ModelThinkingLevelSchema, Union([String, Null])), {description: "Provider value sent for each thinking level. null marks a level unsupported. Levels the model does not map use the provider's default value through high; xhigh and max are unsupported."});
export const ModelPromptCacheSchema = Type.Partial(Type.Record(Enum(["short","long"]), Number({exclusiveMinimum:0})), {description: "Best-effort prompt cache lifetime in seconds for the short and long retention tiers. Kiana warms only tiers with a known lifetime."});
export const ModelCostRatesSchema = Object({ input, output, cacheRead, cacheWrite })  // 每项 Number, desc "<X> cost in USD per million tokens."
export const ModelCostTierSchema = Object({ inputTokensAbove: Number(desc "Use this tier when total request input exceeds this token count."), ...rates })
export const ModelCostSchema = Object({ ...rates, tiers?: Array(ModelCostTierSchema, desc "Request-wide pricing tiers. The highest matching input threshold applies to the full request.") })
export const ModelImageResizeOptionsSchema = Object({ maxWidth?: Integer(min 1), maxHeight?: Integer(min 1),
  maxBytes?: Integer(min 1, desc "Maximum base64-encoded payload size in bytes."), jpegQuality?: Integer(min 1, max 100) })
export const ModelImageInputLimitsSchema = Object({ resize?: (desc "Cache-safe resize profile applied before a new image enters conversation history."),
  maxPerMessage?: Integer(min 1, "Maximum images accepted in one provider message."),
  maxPerRequest?: Integer(min 1, "Maximum images accepted across one provider request.") })
export const ModelInputLimitsSchema = Object({ maxRequestBytes?: Integer(min 1, "Maximum serialized provider request size in bytes."), images?: ... })
```
类型：`ThinkingLevel`、`ModelThinkingLevel`、`ThinkingLevelMap`（`Partial<Record<level, string|null>>`）、`ModelInputModality`、
`CacheRetention`、`ModelPromptCache`、`ModelCostRates`、`ModelCostTier`、`ModelCost`、`ModelImageResizeOptions`、
`ModelImageInputLimits`、`ModelInputLimits`。

#### 2.1.17 `src/providers/compat-schema.ts`（664 行）

TypeBox schema（运行时也用于 `models.json` 自定义 provider 校验）。导出 **14 个 const**（12 个 schema +
`CompatSchemasByApi` + `ProviderCompatSchema`）、**3 个 type 别名**（`ChatTemplateKwargValue`、
`ThinkingTokenBudgetField`、`SessionAffinityFormat`，均为 `Static<...>`）与 **9 个 interface**
（`OpenRouterRouting`、`VercelGatewayRouting`、`AnthropicAllowedFallbackModel`、`OpenAICompletionsCompat`、
`OpenAIResponsesCompat`、`AnthropicMessagesCompat`、`BedrockCompat`、`MistralConversationsCompat`、
`SystemOneCompat`）。原稿写作“13 个 schema + 6 个 interface”，漏掉了 3 个 type 与 3 个 interface，且 schema 实为 14 个：

`SessionAffinityFormatSchema`（Union: `"openai" | "openai-nosession" | "openrouter"`，desc "Session-affinity header format used to route related requests consistently."）、
`ThinkingTokenBudgetFieldSchema`（Union `"thinking_token_budget" | "thinking_budget" | "thinking_budget_tokens"`）、
`ChatTemplateKwargValueSchema`（Union：String | Number | Boolean | Null | Object{ `$var`: Union["thinking.enabled","thinking.effort","thinking.budget"], omitWhenOff?: Boolean }）、
`OpenRouterRoutingSchema`、`VercelGatewayRoutingSchema`、`AnthropicAllowedFallbackModelSchema`、
`OpenAICompletionsCompatSchema`、`OpenAIResponsesCompatSchema`、`AnthropicMessagesCompatSchema`、`BedrockCompatSchema`、
`MistralConversationsCompatSchema`、`SystemOneCompatSchema`、`CompatSchemasByApi`、`ProviderCompatSchema`。

由于 `models.json` 会持久化 compat，字段名/默认值必须逐字保留。**逐字段清单（字段名、类型、默认值、描述短语）**：

`OpenRouterRouting`（`additionalProperties` 未显式设置；desc 结尾："Sent as the provider field in the OpenRouter API request body. See https://openrouter.ai/docs/guides/routing/provider-selection."）：
`allow_fallbacks?` bool default true；`require_parameters?` bool default false；`data_collection?` `"deny"|"allow"` default `"allow"`；
`zdr?` bool；`enforce_distillable_text?` bool；`order?` string[]；`only?` string[]；`ignore?` string[]；`quantizations?` string[]；
`sort?` string | { by?: string; partition?: string|null default "model" }；`max_price?` { prompt?/completion?/image?/audio?/request? 均为 number|string }；
`preferred_min_throughput?` number | percentileCutoffs（p50/p75/p90/p99?: number）；`preferred_max_latency?` 同上。

`VercelGatewayRouting`：`only?` string[]；`order?` string[]。

`AnthropicAllowedFallbackModel`：`provider` string(minLength 1)、`model` string(minLength 1)、`cost` ModelCostSchema；
`allowedFallbackModels` 数组 `maxItems: 3`。

`OpenAICompletionsCompat`（`additionalProperties: true`）字段：
`supportsStore?`、`supportsDeveloperRole?`、`supportsReasoningEffort?`、`supportsUsageInStreaming?`(default true)、
`supportsFinishReason?`(default true)、`maxTokensField?`(`"max_completion_tokens"|"max_tokens"`)、`requiresToolResultName?`、
`requiresAssistantAfterToolResult?`、`requiresThinkingAsText?`、`requiresReasoningContentOnAssistantMessages?`、
`thinkingFormat?`(11 值，见下)、`chatTemplateKwargs?`(Record<string, ChatTemplateKwargValue>)、`chatTemplateArgs?`(同)、
`openRouterRouting?`、`vercelGatewayRouting?`、`zaiToolStream?`(default false)、`thinkingTokenBudgetField?`、
`supportsThinkingTokenBudget?`(default false)、`supportsOpenAIGrammarTools?`(default false)、
`supportsMidConvoSystemMessages?`(default false)、`supportsMidConvoToolAdditions?`(default false)、
`supportsStrictMode?`(default false)、`cacheControlFormat?`(`"anthropic"`)、`sendSessionAffinityHeaders?`（描述：默认 OpenRouter 端点 true）、
`sessionAffinityFormat?`、`supportsLongCacheRetention?`、`vllmPriority?`(number，desc "vLLM scheduler priority sent as the top-level priority request field. Lower values are handled earlier and the server default is 0. Only meaningful with --scheduling-policy priority. Off by default and not set on the generated catalog.")。

`thinkingFormat` 值：`openai`、`openrouter`、`deepseek`、`together`、`baseten`、`zai`、`qwen`、`chat-template`、
`qwen-chat-template`、`string-thinking`、`ant-ling`。描述逐字（含各值语义）：
`"openai" uses reasoning_effort, "openrouter" uses reasoning.effort, "deepseek" uses thinking.type plus reasoning_effort when supported, "together" uses reasoning.enabled plus reasoning_effort when supported, "baseten" uses configurable chat_template_args plus reasoning_effort when supported, "zai" uses thinking.type, "qwen" uses top-level enable_thinking, "qwen-chat-template" uses chat_template_kwargs.enable_thinking and preserve_thinking, "chat-template" uses configurable chat_template_kwargs, "string-thinking" uses top-level thinking, and "ant-ling" uses reasoning.effort only when the mapped effort is non-null.`

`OpenAIResponsesCompat`（additionalProperties true）：`supportsDeveloperRole?`(default true)、`supportsMidConvoSystemMessages?`(default false)、
`sessionAffinityFormat?`（openai 发送 `session_id` 与 `x-client-request-id`；openai-nosession 发送 `x-client-request-id`；openrouter 发送 `x-session-id`；
不影响 `prompt_cache_key`）、`supportsLongCacheRetention?`(default true；GPT-5.6+ 用 `prompt_cache_options.ttl:"30m"`，更早用 `prompt_cache_retention:"24h"`)、
`supportsStrictMode?`、`supportsOpenAIGrammarTools?`(default false)、`supportsAdditionalTools?`(default false)、
`supportsToolSearch?`(default false)、`supportsExplicitPromptCacheMode?`(default false)、`supportsMidConvoEffort?`(default false)、
`supportsMaxOutputTokens?`(default true)。

`AnthropicMessagesCompat`（additionalProperties true）：`supportsEagerToolInputStreaming?`(default true；false 时省略
`tools[].eager_input_streaming` 并为带工具的请求发送 legacy beta `fine-grained-tool-streaming-2025-05-14`)、
`supportsLongCacheRetention?`(default true)、`sendSessionAffinityHeaders?`（默认 OpenRouter 端点 true）、
`sessionAffinityFormat?`(仅 `"openrouter"`)、`supportsCacheControlOnTools?`(default true)、`supportsTemperature?`(default true；
Claude Opus 4.7+ 拒绝非默认值)、`forceAdaptiveThinking?`(default false)、`allowEmptySignature?`(default false)、
`supportsStrictTools?`(default false)、`supportsMidConvoEffort?`(default false)、`supportsMidConvoSystemMessages?`(default false)、
`supportsMidConvoToolChanges?`(default false，需前者)、`allowedFallbackModels?`(maxItems 3)。

`BedrockCompat`：`supportsStrictMode?`(default false)。
`MistralConversationsCompat`：`supportsMidConvoSystemMessages?`(default false)。
`SystemOneCompat`：`imageInput?`(`"assets"|"images"|"state-image-urls"`)、`sendImageOptInHeader?`（默认 typesafe-system-one 为 true，
除非模型被检测为 OpenRouter）。

`CompatSchemasByApi` 映射：openai-completions / openai-responses / azure-openai-responses / openai-codex-responses /
anthropic-messages / bedrock-converse-stream / mistral-conversations / typesafe-system-one / cloudflare-workers-ai-system-one。

`ProviderCompatSchema`：由 `mergeCompatProperties` 合并所有 API schema 的属性（重复属性必须出现在
`ProviderCompatPropertyOverrides` 中，否则抛 `Error(\`Duplicate compatibility schema property requires an override: ${name}\`)`；
override 未对应重复属性则抛 `Error("Compatibility schema override does not resolve a duplicate property: " + name)`）。
override 覆盖的属性：`supportsDeveloperRole`、`supportsMidConvoSystemMessages`(default false)、`sessionAffinityFormat`、
`supportsMidConvoEffort`(default false)、`supportsLongCacheRetention`、`supportsStrictMode`、`supportsOpenAIGrammarTools`(default false)、
`sendSessionAffinityHeaders`。最终 desc：`"Provider and model compatibility overrides."`，`additionalProperties: true`。

### 2.2 `src/utils/**` 逐文件规格

> 所有 utils 都被浏览器打包目标直接引用：不得出现顶层 `node:` 导入（`provider-env.ts`、`env-api-keys.ts` 使用拼接
> specifier 的动态 import 或延迟 require 正是为此）。Rust 侧没有这个限制，但**行为**必须一致。

#### 2.2.1 `utils/abort.ts`（50 行）

```ts
export function operationSignal(signal?: AbortSignal): AbortSignal
export function raceWithAbortSignal<T>(operation: Promise<T>, signal: AbortSignal): Promise<T>
```
内部 `abortReason(signal)`：`signal.reason !== undefined` 时返回它，否则新建 `Error("The operation was aborted")`
并把 `name` 设为 `"AbortError"`。
`operationSignal`：`signal ?? new AbortController().signal`（**永不返回 undefined**，且新 controller 的 signal 永不 abort）。
`raceWithAbortSignal` 语义要点：
1. signal 已 abort：`void operation.catch(()=>{})`（**吞掉**被放弃 promise 的后续 reject），返回 rejected promise（reason 为 abortReason）。
2. 否则返回新 Promise；`settled` 单次标志；`cleanup` 移除 abort 监听；abort 回调与 operation 的 then/catch 谁先到谁决定结果；
   注册监听后**再检查一次** `signal.aborted`（弥补 addEventListener 与检查之间的竞态）。
3. 放弃等待后仍保留对 operation 的 then/catch，因此不会有 unhandled rejection。

#### 2.2.2 `utils/abort-signals.ts`（41 行）

```ts
export interface CombinedAbortSignal { signal?: AbortSignal; cleanup: () => void }
export function combineAbortSignals(signals: readonly (AbortSignal | undefined)[]): CombinedAbortSignal
```
0 个有效 signal -> `{cleanup: noop}`（无 signal 键）。1 个 -> 直接返回该 signal。≥2 个 -> 新 controller；
按输入顺序注册 `once` 监听，遇到已 abort 的 signal 立即以其 reason abort 并 `break`（**后续 signal 不再注册**）；
cleanup 移除所有已注册监听。abort 只在 controller 未 abort 时执行。

#### 2.2.3 `utils/sleep.ts`（14 行）

```ts
export function sleep(ms: number, signal: AbortSignal): Promise<void>
```
先 `signal.throwIfAborted()`（同步抛出）；`setTimeout` 后注册 `once` abort 监听 -> `clearTimeout` 并 `reject(signal.reason)`；
正常到期时移除监听并 resolve。注意：这里的 reject 用的是 `signal.reason` 原值，不是 AbortError 包装。

#### 2.2.4 `utils/text.ts`（50 行）

```ts
export function contentText(content: string | readonly (TextContent|ImageContent|ThinkingContent|ToolCall)[], separator = "\n"): string
export function getSystemMessageText(message: SystemMessage): string
export function renderSystemMessageUpdate(message: SystemMessage): string
export function ownedSlice(text: string, start: number, end = text.length): string
```
- `contentText`：字符串原样返回；否则只取 `type === "text"` 的块并 `join(separator)`（**忽略** thinking/toolCall/image）。
- `getSystemMessageText`：`[contentText(content), ...Object.values(sections ?? {}) 中非 null 项]`，
  过滤长度 0 后以 `"\n\n"` 连接。sections 顺序为对象插入顺序。
- `renderSystemMessageUpdate`：正文非空则先 push；每个 section 生成
  `Removed system prompt section "${name}".`（value 为 null）或 `Updated system prompt section "${name}":\n\n${value}`；
  以 `"\n\n"` 连接。注释明确这是"请求期渲染，可跨版本变化"。
- `ownedSlice`：`\`${text.slice(start, end)} \`.slice(0, -1)`——语义等价于 `slice`，但强制 V8 复制（避免保留原大字符串）。
  Rust 中 `String` 天然拥有所有权，直接 `&text[start..end]` 即可；**但索引语义是 UTF-16 码元**（见 §6）。

#### 2.2.5 `utils/hash.ts`（13 行）

`shortHash(str)`：两个 32 位种子 `0xdeadbeef`、`0x41c6ce57`；逐 **UTF-16 码元** `charCodeAt(i)` 更新：
`h1 = imul(h1 ^ ch, 2654435761)`, `h2 = imul(h2 ^ ch, 1597334677)`；随后混合
`h1 = imul(h1 ^ (h1>>>16), 2246822507) ^ imul(h2 ^ (h2>>>13), 3266489909)`,
`h2 = imul(h2 ^ (h2>>>16), 2246822507) ^ imul(h1 ^ (h1>>>13), 3266489909)`；
返回 `(h2>>>0).toString(36) + (h1>>>0).toString(36)`。必须用 `u32` 包裹 + 算术右移用逻辑右移（`>>>`）复刻；
`toString(36)` 为小写无前缀。用途：日志/调试短标识，不是加密哈希。

#### 2.2.6 `utils/uuid.ts`（45 行）

`uuidv7(timestampMs?)`：UUIDv7（时间有序）。
- 常量 `MAX_UUID_V7_TIMESTAMP = 0xffffffffffff`、`MAX_SEQUENCE = (1n<<41n)-1n`。
- 校验：非整数/负数/超上限 -> `RangeError(\`UUIDv7 timestamp must be an integer between 0 and ${MAX_UUID_V7_TIMESTAMP}\`)`；
  实际渲染为 `... between 0 and 281474976710655`（`0xffffffffffff` 的十进制，**不是**十六进制）。
- `effectiveTimestamp`：无参时取 `max(now, lastOrdinaryTimestamp)` 并更新 `lastOrdinaryTimestamp`；带参时**按原值**且不更新全局。
- 16 随机字节；首次调用用 bytes[1..5] 组成 41 位 `sequence`；后续 `sequence++`，耗尽抛
  `RangeError("UUIDv7 generator sequence exhausted")`。
- 字节布局：bytes[0..5] = 48 位大端时间戳；`bytes[6] = 0x70 | (seq>>37 & 0x0f)`；
  `bytes[7] = (seq>>29) & 0xff`；`bytes[8] = 0x80 | ((seq>>23) & 0x3f)`；bytes[9]、bytes[10] 顺延；
  `bytes[11] = ((seq & 0x7f) << 1) | (bytes[11] & 0x01)`。
- 输出小写十六进制 8-4-4-4-12（版本位 7，variant 位 10）。
全局可变状态（`lastOrdinaryTimestamp`、`sequence`）意味着**并发调用需要串行化**；Rust 用 `Mutex` 包裹。

#### 2.2.7 `utils/sanitize-unicode.ts`（25 行）

`sanitizeSurrogates(text)`：删除**未配对**代理项。正则逐字：
`/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g`（含 lookbehind）。
Rust 中必须工作在 `Vec<u16>` 或 `char` 上（Rust `String` 不允许孤立代理项，因此此函数在 Rust 中实际是恒等变换——
但**输入从 JSON 解析时**若 JS 侧产生孤立代理项，Rust 的 serde 会失败，需要在解码处按 WTF-8/lossy 处理，见 §6）。

#### 2.2.8 `utils/models-error.ts`（21 行）

```ts
export type ModelsErrorCode = "model_source" | "model_validation" | "provider" | "stream" | "auth" | "oauth";
export class ModelsError extends Error {
  readonly code: ModelsErrorCode;
  constructor(code, message, options?: { cause?: unknown })
}
```
`name = "ModelsError"`（Rust 侧需要一个 `kind()` 方法映射到该字符串）。
`withCauseDetail(message, cause)`：cause 为 undefined/null 返回 message；否则 `detail = formatThrownValue(cause).trim()`；
detail 为空或 `message.includes(detail)` 返回 message；否则 `` `${message}: ${detail}` ``。
**重要**：因为调用方只展示 `error.message`，cause 的文本被拼进 message。

#### 2.2.9 `utils/diagnostics.ts`（47 行）

```ts
export interface DiagnosticErrorInfo { name?: string; message: string; stack?: string; code?: string | number }
export interface AssistantMessageDiagnostic { type: string; timestamp: number; error?: DiagnosticErrorInfo; details?: JsonObject }
export function formatThrownValue(value: unknown): string
export function extractDiagnosticError(error: unknown): DiagnosticErrorInfo
export function createAssistantMessageDiagnostic(type, error, details?): AssistantMessageDiagnostic
export function appendAssistantMessageDiagnostic<T extends { diagnostics?: ... }>(message, diagnostic): void
```
- `formatThrownValue`：Error -> `message || name`；string -> 原值；否则 `String(value)`
  （Rust：`String(v)` 对对象给 `"[object Object]"`，数组给逗号连接，null 给 `"null"`，undefined 给 `"undefined"`——需复刻）。
- `extractDiagnosticError`：非 Error -> `{name:"ThrownValue", message: formatThrownValue(error)}`；
  Error -> `{ name: error.name || undefined, message: error.message || error.name, stack: error.stack,
  code: 仅当 string|number }`。
- `appendAssistantMessageDiagnostic` 不可变追加：`message.diagnostics = [...(message.diagnostics ?? []), d]`。

#### 2.2.10 `utils/typebox-helpers.ts`（24 行）

`StringEnum(values, options?)`：返回 `Type.Unsafe({type:"string", enum: values, ...(description && {description}),
...(default && {default})})`。用于 Google 等不接受 `anyOf`/`const` 的 provider。Rust 侧对应"生成本文 enum 的 JSON Schema"辅助函数。

#### 2.2.11 `utils/model-operations.ts`（81 行）

```ts
export function getModelType(model: AnyModel): ModelType            // model.type ?? "chat"
export function isModelType<T>(model, type): model is ModelTypeMap[T]
export function assertChatModel(model): asserts model is Model<Api>
export function assertImageModel(model): asserts model is ImageModel<ImageApi>
export function assertClassifierModel(model): asserts model is ClassifierModel<ClassifierApi>
export function assertClassifierInputSupported(model, context): void
export function imageErrorResult(model, error, aborted = false): AssistantImages
export function classifierErrorResult(model, error, aborted = false): ClassifierResult
```
错误消息逐字：`Model ${provider}/${id} is not a chat model`、`... is not an image model`、
`... is not a classifier model`、`Model ${provider}/${id} does not accept image input`（均为 `ModelsError("provider", ...)`）。
`imageErrorResult` 返回 `{api, provider, model: id, output: [], stopReason: aborted ? "aborted" : "error",
errorMessage: error instanceof Error ? error.message : String(error), timestamp: Date.now()}`；
`classifierErrorResult` 同形但字段为 `answers: {}`。

#### 2.2.12 `utils/headers.ts`（23 行）

```ts
export function headersToRecord(headers: Headers): Record<string, string>
export function providerHeadersToRecord(...sources: (ProviderHeaders | undefined)[]): Record<string, string> | undefined
```
`headersToRecord`：按 `headers.entries()` 顺序，**保留原始（小写化的）名字**写入普通对象（后面同名覆盖前面）。
`providerHeadersToRecord`：用 `Map<string低写名, [原始名, 值]>`；每个 source 逐项：先 `delete(低写名)`，`value !== null` 时
`set(低写名, [原始名, value])`。**null 表示删除该头**。结果为空 Map -> undefined。返回 `Object.fromEntries(values)`，
即键为**原始大小写**，顺序为最后一次设置的顺序（Map 的插入顺序：删除再插入会移到末尾）。
与 `models.ts` 的 `mergeHeaders` 差异：后者保留 null（表示抑制），前者丢弃 null。

#### 2.2.13 `utils/provider-env.ts`（52 行）

```ts
export function getProviderEnvValue(name: string, env?: ProviderEnv): string | undefined
```
顺序：`env?.[name] || process.env[name] || getBunSandboxEnvValue(name) || undefined`（**用 `||`，空字符串视为缺失**）。
Bun 沙箱回退：仅当 `process` 存在、`process.versions.bun` 为真且 `Object.keys(process.env).length === 0` 时生效；
首次读取 `/proc/self/environ`（`require("node:fs").readFileSync(path,"utf-8")`）并按 `\0` 拆分，
对每项取第一个 `=` 之前为键（`idx > 0`）、其后为值，缓存到模块级 Map；读取失败静默（缓存空 Map）。
Rust 侧应实现为：scoped env -> 进程 env -> `/proc/self/environ`（仅 Linux，且仅在"进程 env 为空"时）。

#### 2.2.14 `utils/error-body.ts`（151 行）

常量 `MAX_PROVIDER_ERROR_BODY_CHARS = 4000`。
```ts
export interface NormalizedProviderError { status?: number; body?: string; message: string; messageCarriesBody: boolean }
export function normalizeProviderError(error: unknown): NormalizedProviderError
export function formatProviderError(norm, prefix?): string
export function truncateErrorText(text, maxChars): string
export function safeJsonStringify(value): string
```
- 非 Error -> `{ message: safeJsonStringify(error), messageCarriesBody: false }`。
- status 探测顺序（第一个数值命中）：`statusCode`（Mistral）-> `status`（openai / @google/genai）-> `$metadata.httpStatusCode`（Bedrock）
  -> `$response.statusCode`（Bedrock）。
- body 探测顺序：`body` 为 string -> `error` 为**非空普通对象**（原型为 `Object.prototype` 或 null）时 `safeJsonStringify` ->
  `$response.body` 为 string -> 是可读流（有 `pipe` 函数）则视为无 body -> 非空普通对象则序列化。
  body 经 `trim()`，空则 undefined，否则 `truncateErrorText(trimmed, 4000)`。
- `messageCarriesBody = body === undefined || error.message.includes(body)`。
- `truncateErrorText`：超长时 `` `${ownedSlice(text,0,maxChars)}... [truncated ${text.length - maxChars} chars]` ``。
- `formatProviderError`：
  - messageCarriesBody 或 status/body 任一缺失：有 prefix 且 status 存在 -> `` `${prefix} (${status}): ${message}` ``，否则 message；
  - 否则有 prefix -> `` `${prefix} (${status}): ${body}` ``，无 prefix -> `` `${status}: ${body}` ``。
- `safeJsonStringify`：`JSON.stringify`；结果为 undefined -> `String(value)`；抛错 -> `String(value)`。

#### 2.2.15 `utils/openrouter-cost.ts`（55 行）

```ts
export interface OpenRouterUsageAccounting { cost: number|null; is_byok: boolean;
  cost_details: { upstream_inference_cost: number|null; upstream_inference_prompt_cost: number;
                  upstream_inference_completions_cost: number; server_tool_cost?: number|null } | null }
export function isOpenRouterModel(model: {provider: string; baseUrl: string}): boolean
export function applyOpenRouterReportedCost(usage: Usage, reported: OpenRouterUsageAccounting): void
```
`isOpenRouterModel`：`provider === "openrouter" || baseUrl.includes("openrouter.ai")`。
`isValidCost(v)`：number 且 `Number.isFinite` 且 `>= 0`。
`applyOpenRouterReportedCost`：
1. `cost` 无效 -> 返回（不改 usage）。
2. `is_byok === false`：`usage.cost.total = cost`，`totalFromProvider = true`，返回。
3. `is_byok === true`：无 `cost_details` -> 返回；`upstream = details.upstream_inference_cost`；
   若为 `null`：两侧分量都有效则相加，否则返回；最终 upstream 无效 -> 返回；
   `usage.cost.total = reported.cost + upstream`，`totalFromProvider = true`。
4. `is_byok` 既非 true 也非 false（如 undefined）-> 什么都不做。
注意 `server_tool_cost` 仅注释说明"已包含在 cost 内"，代码不使用。

#### 2.2.16 `utils/estimate.ts`（117 行）

常量 `CHARS_PER_TOKEN = 3.5`、`ESTIMATED_IMAGE_CHARS = 4800`。
```ts
export interface ContextUsageEstimate { tokens: number; usageTokens: number; trailingTokens: number; lastUsageIndex: number | null }
export function calculateContextTokens(usage: Usage): number       // usage.totalTokens || input+output+cacheRead+cacheWrite
export function estimateTextTokens(text): number                    // Math.ceil(len / 3.5)
export function estimateTextAndImageContentTokens(content): number  // ceil(chars/3.5)，图片块记为 4800
export function estimateMessageTokens(message: Message): number
export function estimateContextTokens(context: TranscriptContext | readonly Message[]): ContextUsageEstimate
```
`estimateMessageTokens`：
- system：`estimateTextTokens(getSystemMessageText(m)) + estimateToolsTokens(toolsAdded) + estimateToolsTokens(toolsRemoved)`；
- user / toolResult：`estimateTextAndImageContentTokens(content)`；
- assistant：text 块计 `text.length`，thinking 计 `thinking.length`，toolCall 计 `name.length + safeJsonStringify(arguments).length`，
  最后 `ceil(chars/3.5)`。`safeJsonStringify`：`JSON.stringify(value) ?? "undefined"`，抛错 -> `"[unserializable]"`。
- `estimateToolsTokens`：空/缺失 0，否则 `estimateTextTokens(safeJsonStringify(tools))`。
`getLastAssistantUsageInfo`：遍历所有消息，维护 `latestPrefixTimestamp`（到目前为止所有消息 timestamp 的**最大值**，
含当前消息之前的所有角色）；对 assistant：`usageAppliesToPrefix = timestamp >= latestPrefixTimestamp`，
且 stopReason 非 aborted/error 且 `calculateContextTokens(usage) > 0` 时记为候选（**后面的覆盖前面的**）；
每轮末 `latestPrefixTimestamp = max(latestPrefixTimestamp, message.timestamp)`。
`estimateContextTokens`：有候选 -> `usageTokens + 其后所有消息的 estimateMessageTokens 之和`；否则全量累加，
`usageTokens: 0, trailingTokens: tokens, lastUsageIndex: null`。

#### 2.2.17 `utils/overflow.ts`（193 行，其中大量注释）

```ts
export function isContextOverflow(message: AssistantMessage, contextWindow?: number): boolean
export function isRecoverableLength(message: AssistantMessage, desiredMaxOutput: number): boolean
export function getOverflowPatterns(): RegExp[]   // 返回副本
```
`OVERFLOW_PATTERNS`（顺序敏感，逐字，全部 `i` 标志）：
```
/prompt (?:is )?too long/i
/prompt exceeds max length/i
/request_too_large/i
/input is too long for requested model/i
/exceeds the context window/i
/exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))/i
/input token count.*exceeds the maximum/i
/maximum prompt length is \d+/i
/reduce the length of the messages/i
/maximum context length is \d+ tokens/i
/exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?/i
/input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)/i
/exceeds the limit of \d+/i
/exceeds the available context size/i
/greater than the context length/i
/context window exceeds limit/i
/exceeded model token limit/i
/too large for model with \d+ maximum context length/i
/prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?/i
/model_context_window_exceeded/i
/prompt too long; exceeded (?:max )?context length/i
/range of input length should be/i
/context[_ ]length[_ ]exceeded/i
/too many tokens/i
/token limit exceeded/i
/^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))/i
```
`CEREBRAS_BODYLESS_OVERFLOW_PATTERN = /^4(?:00|13)\s*(?:status code)?\s*\(no body\)/i`；
`NON_OVERFLOW_PATTERNS = [/^(Throttling error|Service unavailable):/i, /rate limit/i, /too many requests/i]`。

算法：
1. `stopReason === "error" && errorMessage`：先测 NON_OVERFLOW（任一命中即跳过）：
   再测 OVERFLOW_PATTERNS；命中即 true；否则 provider 为 `cerebras` 且 bodyless 模式命中 -> true。
2. `contextWindow` 为真且 `stopReason === "stop"`：`input + cacheRead > contextWindow` -> true（静默溢出）。
3. `contextWindow` 为真且 `stopReason === "length"` 且 `usage.output === 0`：
   `input + cacheRead >= contextWindow * 0.99` -> true（Xiaomi 静默截断）。
4. 否则 false。
`isRecoverableLength`：`stopReason === "length" && desiredMaxOutput > 0 && usage.output < desiredMaxOutput`。
（注意第 3 条用的是 `>=` 与浮点 `* 0.99`；第 2 条用严格 `>`。）

#### 2.2.18 `utils/retry.ts`（271 行）

**分类器正则**（由 `patterns.join("|")` 构造，`i` 标志，**整体**包裹，注意其中的内联分组与 lookbehind）：

`NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN` 的片段顺序：
```
GoUsageLimitError
FreeUsageLimitError
Monthly usage limit reached
available balance
insufficient_quota
out of budget
quota exceeded
billing
subscription_sharing_usage_limit_exceeded
subscription_sharing_workplace_individual_usage_limit_exceeded
```

`RETRYABLE_PROVIDER_ERROR_PATTERN` 的片段顺序：
```
overloaded
server_busy
servers are currently busy
currently experiencing high demand
model is at capacity
rate.?limit
too many requests
(?<![a-z0-9])(?:429|500|502|503|504|520|524)(?![a-z0-9])
service.?(?:is.?)?(?:temporarily.?)?unavailable
server.?error
internal.?error
provider.?returned.?(?:an.?)?error
exceeded request buffer limit while retrying upstream
unable to forward upstream
upstream.?request.?failed
upstream_stream_read_error
stream was interrupted
http/?2 stream failed
network.?error
connection.?error
connection.?refused
connection.?lost
other side closed
fetch failed
request to \S+ failed, reason:
getaddrinfo
ENOTFOUND
EAI_AGAIN
upstream.?connect
reset before headers
socket hang up
socket connection was closed
timed? out
timeout
terminated
websocket.?closed
websocket.?error
ended without
stream ended before message_stop
stream ended before a terminal response event
http2 request did not get a response
stream disconnected before completion
pending stream has been canceled
retry delay
you can retry your request
try your request again
please retry your request
ResourceExhausted
subscription_sharing_usage_unavailable
subscription_sharing_user_unavailable
```
注意 `"request to \\S+ failed, reason:"` 是字符串字面量 `request to \S+ failed, reason:`；`http/?2` 匹配 `http2`/`http/2`；
`server.?error` 中的 `.` 匹配任意字符（含空格）。大小写不敏感。

```ts
export interface RetryPolicy { enabled: boolean; maxRetries: number; baseDelayMs: number; maxAgentDelayMs?: number }
export const DEFAULT_MAX_AGENT_RETRY_DELAY_MS = 60_000;
export function retryDelayMs(policy, attempt): number
export interface RetryCallbacks { onRetryScheduled?; onRetryAttemptStart?; onRetryFinished? }
export async function retryAssistantCall(produce, policy, signal, callbacks?): Promise<AssistantMessage>
export function isRetryableAssistantError(message: AssistantMessage): boolean
```
`retryDelayMs`：`delay = baseDelayMs * 2 ** max(0, attempt-1)`；`Number.isSafeInteger(delay) ? delay : Number.MAX_SAFE_INTEGER`；
返回 `min(safeDelay, policy.maxAgentDelayMs ?? 60000)`。

`retryAssistantCall` 循环（必须逐字复刻回调次数与顺序）：
- `maxAttempts = policy?.enabled ? policy.maxRetries : 0`；`attempt = 0`；`lastRetry` 未定义。
- 每次循环先 `await produce()`：
  - `stopReason === "aborted"`：若 `lastRetry` 存在 -> `onRetryFinished(false, lastRetry.attempt)`（**不传 finalError**）；返回 response。
  - `stopReason !== "error"`：若 lastRetry -> `onRetryFinished(true, lastRetry.attempt)`；返回。
  - 否则若 `attempt >= maxAttempts || !isRetryableAssistantError(response)`：
    若 lastRetry -> `onRetryFinished(false, lastRetry.attempt, response.errorMessage)`；返回。
  - 否则 `attempt++`；`lastRetry = {attempt, errorMessage: response.errorMessage || "Unknown error"}`；
    `delayMs = retryDelayMs(policy!, attempt)`；`onRetryScheduled(attempt, maxAttempts, delayMs, lastRetry.errorMessage)`；
    `sleep(delayMs, signal)`：若因 abort 抛 `RetrySleepAbortError` -> `onRetryFinished(false, attempt, lastRetry.errorMessage)`，
    返回 `{...response 去掉 errorMessage 字段, stopReason:"aborted"}`（**删除 errorMessage 键**）；其他错误重新抛出。
    休眠正常结束后 `onRetryAttemptStart()`。
- 内部 `sleep` 用 `RetrySleepAbortError`（message `"Aborted"`），与 `utils/sleep.ts` 不同（不包装 reason）。
`isRetryableAssistantError`：`stopReason !== "error" || !errorMessage` -> false；NON_RETRYABLE 命中 -> false；
否则 RETRYABLE 命中。

#### 2.2.19 `utils/provider-retry.ts`（128 行）

```ts
export async function retryProviderRequest<T>(request: () => Promise<T>, options?: ProviderRetryOptions): Promise<T>
interface ProviderRetryOptions { maxRetries?: number; maxRetryDelayMs?: number; signal?: AbortSignal; noRetryStatuses?: readonly number[] }
```
`DEFAULT_MAX_RETRY_DELAY_MS = 60_000`；`maxRetries` 默认 **0**。
`isProviderError(e)`：Error 且含 `status` 与 `headers` 自有/继承属性，且 status 为 undefined 或 number，
headers 为 undefined 或 `Headers` 实例。（Rust 侧定义为 trait/struct `ProviderError { status: Option<u16>, headers: Option<HeaderMap> }`，
由 SDK 错误适配层构造。）
`isRetryableProviderError`：先看 `x-should-retry` 头（`"true"` -> true，`"false"` -> false）；
status undefined -> true；否则 `408|409|429|>=500`。
`getRetryDelayMs`：`retry-after-ms`（`parseFloat` 有限即用）-> `retry-after`（`parseFloat`；NaN 时按 HTTP 日期
`Date.parse(v) - Date.now()`；结果有限即用）-> 指数退避 `min(0.5 * 2 ** retryIndex, 8) * 1000 * (1 - Math.random()*0.25)`。
`validateServerRetryDelayMs`：`maxDelayMs = maxRetryDelayMs ?? 60000`；仅当 `maxDelayMs > 0 && delayMs > maxDelayMs` 抛
`` `Server requested ${Math.ceil(delayMs/1000)}s retry delay (max: ${Math.ceil(maxDelayMs/1000)}s). ${providerErrorMessage}` ``。
`abortableSleep`：已 abort -> reject `createAbortError()`（`Error("Request aborted")`，name `"AbortError"`）；
否则 `setTimeout(max(0,ms))` + once abort 监听。
循环：try `await request()`；catch：signal aborted -> 抛 AbortError；`retriesRemaining <= 0` 或非 provider error 或不可重试 -> 原样抛出；
`noRetryStatuses` 含该 status -> 抛出；否则 `retryIndex = maxRetries - retriesRemaining`，`retriesRemaining--`，
sleep 计算出的延迟后重试。**每次重试都是全新 SDK 请求**（注释：X-Stainless-Retry-Count 保持 0）。

#### 2.2.20 `utils/node-http-proxy.ts`（161 行）

```ts
export const UNSUPPORTED_PROXY_PROTOCOL_MESSAGE =
  "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL.";
export function resolveHttpProxyUrlForTarget(targetUrl: string | URL, env?: ProviderEnv): URL | undefined
```
内部：`DEFAULT_PROXY_PORTS = { ftp:21, gopher:70, http:80, https:443, ws:80, wss:443 }`。
`getProxyEnv(key)`：`env[低写] || env[大写] || process env 低写 || 大写 || ""`（空串视为缺失）。
`parseProxyTargetUrl`：URL 实例原样；字符串 `new URL` 失败返回 undefined。
`stripBrackets`：`[..]` 去方括号（IPv6）。
`parseNoProxyEntry(entry)`：trim + 小写；空 -> undefined；
以 `[` 开头且找到 `]`：host 为括号内，剩余以 `:` 开头则 `parseInt` 端口（NaN -> 0），否则 0；
**冒号多于 1 个**（裸 IPv6）-> `{host: 整串, port: 0}`；
恰好 1 个冒号且端口可解析 -> `{host, port}`；否则 `{host: 整串, port: 0}`。
`shouldProxyHostname(hostname, port)`：no_proxy 为空 -> true；`"*"` -> false；
否则 `no_proxy.split(/[,\s]/)` **每一项都必须**返回 true（即不在 no_proxy 中）；
每项：解析失败 -> true（跳过）；`parsed.port && parsed.port !== port` -> true（端口不匹配则不排除）；
domain 去 `*.` / 前导 `.` 或 `*`；空 domain -> true；`host === domain` 或 `host.endsWith("." + domain)` -> **false**（排除，不代理）；
否则 true。注意 IPv6 目标与域名比较时都做了去括号/小写。
`getProxyForUrl`：解析失败或无 host -> `""`；protocol 取 `:` 前段；port 为 `parseInt(port) || 默认端口 || 0`；
不代理 -> `""`；否则 `${protocol}_proxy` 或 `all_proxy`；无 `://` 时前缀 `${protocol}://`。
`resolveHttpProxyUrlForTarget`：无代理 -> undefined；`new URL(proxy)` 失败 ->
`Error(\`Invalid proxy URL ${JSON.stringify(proxy)}: ${message}\`)`；协议非 http/https ->
`Error(\`${UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got ${proxyUrl.protocol}\`)`（注意 `protocol` 含尾冒号，如 `socks5:`）。

#### 2.2.21 `utils/oauth-page.ts`（109 行）

`LOGO_SVG` 逐字：
```html
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#F09082" d="M165.29 165.29H517.36V400H400V282.65H165.29Z"/><path fill="#4D9ABF" d="M165.29 282.65H282.65V400H400V517.36H282.65V634.72H165.29Z"/><path fill="#F1BE58" d="M517.36 400H634.72V634.72H517.36Z"/></svg>
```
`escapeHtml` 顺序（**顺序重要**）：`&` -> `&amp;`，`<` -> `&lt;`，`>` -> `&gt;`，`"` -> `&quot;`，`'` -> `&#39;`（用 `replaceAll`）。
`renderPage({title, heading, message, details?})`：返回完整 HTML（`<!doctype html>`、lang="en"、内联 CSS 见源码），
其中 `${details ? \`<div class="details">${details}</div>\` : ""}`。
- `oauthSuccessHtml(message)`：title/heading 均为 `"Authentication successful"`。
- `oauthErrorHtml(message, details?)`：title/heading 均为 `"Authentication failed"`。
Rust 需**逐字节**复刻该 HTML（含缩进与空行），因为它是浏览器可见界面。

#### 2.2.22 `utils/identity.ts`（193 行）

```ts
export type UserAgentMode = "short" | "detailed" | { custom: string };
export interface ClientAttribution { name: string; url: string; categories?: string[] }
export const KIANA_ATTRIBUTION: ClientAttribution = { name: "kiana", url: "https://kiana.dev", categories: ["cli-agent"] };
export interface ClientIdentity { userAgent?: UserAgentMode; attribution?: ClientAttribution | null; version?: string }
export interface ClientIdentityTarget { readonly provider: string; readonly baseUrl: string }
export function mergeClientIdentity(...sources): ClientIdentity | undefined
export function getClientVersion(identity?): string                     // identity?.version ?? KIANA_AI_VERSION
export function formatUserAgent(identity?): string
export function getAttributionHeaders(target, identity?): Record<string,string>
export function getOpenCodeSessionHeaders(target, sessionId): Record<string,string> | undefined
export function getIdentityHeaders(target, identity?): Record<string,string>
export function withClientIdentity(models, identity): Models | MutableModels   // 两个重载
```
`formatUserAgent`：mode 默认 `"detailed"`；对象 -> `mode.custom`；`"short"` -> `"kiana"`；
否则 `kiana/${version}`，有 runtime（`bun/<ver>` 优先于 `node/${process.version}`，`process.versions.node` 存在才算 Node）
则 `` `${product} (${process.platform}; ${runtime}; ${process.arch})` ``，否则 `"${product} (browser)"`。
**注意** `node/${process.version}` 的 version 形如 `v22.19.0`（含 `v`），而 `bun/${versions.bun}` 不含 `v`。
Rust 侧：`process.platform` -> `std::env::consts::OS` 映射（darwin/linux/win32 等），`process.arch` -> `ARCH`
（x64/arm64/ia32 等），runtime 为 `node` 或 `bun`——需要在编译期/运行期探测，见 §6。
`isOpenRouter`：`provider === "openrouter" || baseUrl.includes("openrouter.ai")`。
`isNvidiaNim`：`provider === "nvidia" || new URL(baseUrl).hostname === "integrate.api.nvidia.com"`（URL 解析失败 -> false）。
`isOpenCode`：`provider === "opencode" || provider === "opencode-go" || hostname === "opencode.ai"`。
`getAttributionHeaders`：无 attribution -> `{}`；
openrouter -> `{"HTTP-Referer": url, "X-OpenRouter-Title": name, ...(categories 非空 ? {"X-OpenRouter-Categories": categories.join(",")} : {})}`；
nvidia -> `{"X-BILLING-INVOKE-ORIGIN": name 首字母大写 + 其余原样}`（只大写第一个 UTF-16 码元）；
opencode -> `{"x-opencode-client": name}`；其他 -> `{}`。**优先级：openrouter > nvidia > opencode**。
`getOpenCodeSessionHeaders`：`sessionId && isOpenCode(target) ? {"x-opencode-session": sessionId} : undefined`。
`getIdentityHeaders`：`{"User-Agent": formatUserAgent(identity), ...getAttributionHeaders(...)}`（UA 在前）。
`mergeClientIdentity`：逐 source 逐字段覆盖，`undefined` 跳过（`null` attribution **会**覆盖）。
`withClientIdentity`：包装全部 24 个 Models 方法；请求类方法（stream/complete/streamSimple/completeSimple/streamDeferred/
fetchDeferred/cancelDeferred/generateImages/classify）把 `identity` 合并进 options（`mergeClientIdentity(identity, options?.identity)`）；
`getAuth` 用 `models.getAuth.bind(models)`（原样绑定，不注入 identity）；若原对象是 MutableModels 则返回含
`setProvider`/`deleteProvider`/`clearProviders` 的 MutableModels。

#### 2.2.23 `utils/version.ts`（2 行）

`export const KIANA_AI_VERSION = "1.4.0";`（由 `scripts/sync-versions.js` 与 package.json 同步）。

#### 2.2.24 `utils/event-stream.ts`（135 行）——流与计时

`index.ts` 通过 `export *` 重导出本文件（`EventStream`、`AssistantMessageEventStream`、
`createAssistantMessageEventStream`）。

```ts
class FifoQueue<T> {                      // 内部类，不导出
  private incoming: T[]; private outgoing: T[];
  get length(): number                    // incoming.length + outgoing.length
  enqueue(value: T): void                 // incoming.push
  dequeue(): T | undefined                // outgoing 空时把 incoming 逆序搬入 outgoing，再 outgoing.pop()
}
export class EventStream<T, R = T> implements AsyncIterable<T> {
  constructor(isComplete: (event: T) => boolean, extractResult: (event: T) => R)
  push(event: T): void
  end(result?: R): void
  [Symbol.asyncIterator](): AsyncIterator<T>
  result(): Promise<R>
}
export class AssistantMessageEventStream extends EventStream<AssistantMessageEvent, AssistantMessage> {}
export function createAssistantMessageEventStream(): AssistantMessageEventStream
```

`EventStream` 状态：`queue: FifoQueue<T>`、`waiting: FifoQueue<(r: IteratorResult<T>) => void>`、
`protected done = false`、`finalResultPromise` + `resolveFinalResult`、`isComplete`、`extractResult`。

- `push(event)`：`done` 为真时**直接 return（丢弃）**；否则若 `isComplete(event)` 则先置 `done = true` 并
  `resolveFinalResult(extractResult(event))`（**在投递之前**）；然后若 `waiting` 非空则交给第一个等待者
  （`{value:event, done:false}`），否则入队。
- `end(result?)`：`done = true`；`result !== undefined` 时 resolve（`undefined` 不覆盖既有 promise 结果——
  注意这意味着 `end()` 无参时若此前从未 resolve，`result()` 永远 pending）；然后把**所有**等待者以
  `{value: undefined, done: true}` 唤醒。
- `asyncIterator`：循环——队列非空则 `yield dequeue()`；否则 `done` 则 return；否则 `await` 一个新 Promise
  （把 resolve 压入 `waiting`），若 `result.done` 则 return，否则 `yield result.value`。
  即**先排空队列再等待**，且等待是单消费者语义（多个消费者会各自拿到不同事件）。
- `result()`：返回 `finalResultPromise`（同一 promise，不重新创建）。

`AssistantMessageEventStream`：
- 构造：`isComplete = (e) => e.type === "done" || e.type === "error"`；
  `extractResult = (e) => e.type === "done" ? e.message : e.type === "error" ? e.error
  : throw new Error("Unexpected event type for final result")`。
- `#startedAt = Date.now()`、`#startedAtMonotonic = performance.now()`（均在**构造时**取）。
- 覆写 `push`：`done` 事件先 `#time(event.message)`，`error` 事件先 `#time(event.error)`，再 `super.push`。
- 覆写 `end(result?)`：`result !== undefined` 时先 `#time(result)`，再 `super.end(result)`。
- `#time(message)`：若 `this.done`（注意：`super.push` 尚未执行，但 `end` 已可能置位）、或
  `message.durationMs !== undefined`、或 `message.timestamp < this.#startedAt`（转发别处已开始的响应，如 deferred
  结果）则**不设置**；否则 `message.durationMs = Math.max(0, Math.round(performance.now() - startedAtMonotonic))`。
- 工厂 `createAssistantMessageEventStream()` 只是 `new AssistantMessageEventStream()`。

Rust 对应：`EventStream<T, R>` → 带 `tokio::sync::mpsc` + `oneshot`/`Notify` 的结构；`durationMs` 用
`Instant`（单调）。注意 `push` 在 `done` 后静默丢弃、`end()` 不传值不 resolve 这两条**必须**保留。

#### 2.2.25 `utils/json-parse.ts`（149 行）

```ts
export function repairJson(json: string): string
export function parseJsonWithRepair<T>(json: string): T
export function parseToolCallArguments(json: string | undefined): Pick<ToolCall, "arguments" | "argumentsError">
export function parseStreamingJson<T = Record<string, unknown>>(partialJson: string | undefined): T
```
`VALID_JSON_ESCAPES = new Set(['"', "\\", "/", "b", "f", "n", "r", "t'])`。
`repairJson` 逐字符状态机（`inString`）：
- 字符串外：原样输出，遇 `"` 进入字符串。
- 字符串内：`"` -> 原样并退出；`\`：
  - 末尾（无后继）-> 输出 `\\`（两个字符）；
  - 后继为 `u` 且随后 4 位都匹配 `/^[0-9a-fA-F]{4}$/` -> 原样复制 `\uXXXX` 并 `index += 5`；
  - 后继在 VALID_JSON_ESCAPES -> 原样复制两者并 `index += 1`；
  - 否则输出 `\\`（**不消费**后继，下一轮再处理它）；
- 其他字符：控制字符（码点 0x00-0x1F）转义：`\b \f \n \r \t` 用短形式，其余 `` `\\u${hex.padStart(4,"0")}` ``（小写十六进制）；
  非控制字符原样。
`parseJsonWithRepair`：先 `JSON.parse`；失败则 `repairJson`，若结果与原文**不同**则 `JSON.parse(repaired)`
（若这次仍失败，抛的是**修复后**那次调用的错误，而不是最初那个）；若结果与原文相同，抛最初错误。
`parseToolCallArguments`：`!json?.trim()` -> `{arguments:{}}`；parse 后要求是**非 null、非数组的 object**，
否则抛 `Error("Tool arguments must be a JSON object")`（被 catch）；成功 `{arguments}`；
失败 `{arguments:{}, argumentsError: \`${message}\n\nReceived arguments:\n${json}\`}`。
`parseStreamingJson`：空/纯空白 -> `{}`；否则 `parseJsonWithRepair` -> 失败 -> `partialParse(partialJson)`（partial-json 包）
-> 失败 -> `partialParse(repairJson(partialJson))` -> 失败 -> `{}`；`result ?? {}`。

#### 2.2.26 `utils/streaming-json.ts`（559 行）——最难的移植点

**模块契约（源码头注释，逐条必须满足）**：
1. `append()` 只读每个片段一次；总代价与文本长度线性相关。
2. `snapshot()` 返回"迄今读到的文本"的值；快照是普通 JSON 值且**返回后永不改变**：每次新快照复制仍"打开"的容器
   （open path），共享已完成的容器与字符串。
3. 不完整文本的规则（对齐 `parseStreamingJson()`）：
   - 未终止字符串显示已读到的字符；未终止转义被**扣留**直到完整。
   - 未终止容器显示已读到的成员/元素。
   - 键或值尚未开始的成员被省略。
   - 未终止字面量一旦成为 `true`/`false`/`null` 的前缀就显示其值；未终止数字在已读数字构成数字时显示
     （**指数部分无数字时忽略**，回退到尾数）。
4. malformed 文本宽容读取（类似 `repairJson()`）：字符串内原始控制字符保留；非法转义如 `\d` 保留为字面文本；
   缺失逗号/冒号被容忍；非法位置的值与未知单词被丢弃；根值之后的文本被忽略；**永不抛错**。
   （需要精确值调用方在结束时用 `parseToolCallArguments()` 一次完整解析。）
5. 与 `parseStreamingJson()` 在**不完整文本**上的差异：未终止字符串内的尾随空白**保留**（后者会 trim）；
   含原始控制字符或非法转义的字符串在流式期间**可见**（后者在完成前丢弃）；`NaN`/`Infinity` 单词被丢弃。
6. `append()` 返回本次片段造成的变更，供自行维护副本的写入方使用：
   - `{kind:"set", path, value}`：键、数组元素或**已完成的值**出现时（新字符串以首段文本 set 一次；容器以空容器 set）。
   - `{kind:"append", path, text}`：片段**扩展了**一个打开的字符串。
   - 未终止的数字/字面量在结束前**不报告**。
   - 嵌套超过 32 层的容器在关闭时作为**整体一次 `set`**报告，使路径保持短。
7. 代价：复制 open path 的成本为其成员+元素数。每次 `append()` 赚取与长度成正比的复制 credit 加一个常量；
   `snapshot()` 花费它；当 open path 的成本超过 credit 时返回**上一个快照**（预览略微滞后）。
   因此没有任何输入使总工作量增长快于文本长度。

类型（逐字）：
```ts
type JsonContainer = { [key: string]: JsonValue } | JsonValue[];
type Slot = { kind: "root" } | { kind: "member"; key: string } | { kind: "element"; index: number };
export type JsonPath = readonly (string | number)[];
export type JsonChange =
  | { kind: "set"; path: JsonPath; value: JsonValue }
  | { kind: "append"; path: JsonPath; text: string };
type OpenChange = { kind: "set"; path: JsonPath; value: JsonValue } | { kind: "append"; path: JsonPath; text: string };
type Frame = { container: JsonContainer; slot: Slot | undefined; path: JsonPath | undefined;
               reportsInside: boolean; size: number; key: string | undefined; phase: "key"|"colon"|"value"|"comma" };
type OpenString = { target: "key"|"value"; slot: Slot | undefined; text: string; escape: string;
                    path: JsonPath | undefined; change: OpenChange | undefined };
```
常量：`MAX_REPORTED_DEPTH = 32`、`ROOT_SLOT = {kind:"root"}`、`CREDIT_PER_APPEND = 256`、
`CREDIT_PER_CHARACTER = 8`、`MAX_PENDING_SCALAR_LENGTH = 64`、
`NUMBER = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/`、
`NUMBER_WITHOUT_EXPONENT_DIGITS = /^(-?(?:0|[1-9]\d*)(?:\.\d+)?)[eE][+-]?$/`、
`LITERALS = [["true",true],["false",false],["null",null]]`、`QUOTE = 0x22`、`BACKSLASH = 0x5c`。

**`StreamingJsonParser` 状态**：`#root: JsonValue|undefined`、`#done`、`#stack: Frame[]`、`#string: OpenString|undefined`、
`#scalar: string`、`#scalarSlot: Slot|undefined`、`#pathCost`（open path 上 `size+1` 之和）、`#credit`、`#dirty`、
`#snapshot`、`#changes: OpenChange[]`。

**`append(text): readonly JsonChange[]`**
1. `#credit += 256 + 8 * text.length`；`#changes = []`；若存在打开字符串则把其 `change` 置 undefined
   （片段边界处不再续写上一个段的变更对象）。
2. `index = 0`；while `index < len && !done`：若有打开字符串 -> `#readString`；否则若 `#scalar !== ""` -> `#readScalar`；
   否则 `#structural(text[index++])`。
3. 返回 `#changes`（注意返回的是**内部数组**，类型为 readonly）。

**`snapshot(): JsonValue | undefined`**
- `!dirty` -> 返回 `#snapshot`（含初次调用返回 undefined）。
- `pending = #pendingScalar()`；`cost = #pathCost + (pending ? 1 : 0)`；`cost > #credit` -> 返回旧 `#snapshot`（不更新）。
- 否则 `#credit -= cost`、`dirty = false`、`#snapshot = #copyOpenPath(pending)`，返回之。

**`#readString(open, text, start): number`**（返回下一个索引）
- `runStart = start`；逐字符（`charCodeAt`）：
  - 若 `open.escape !== ""`：`#continueEscape(open, ch)` 返回 true -> `runStart = index+1`，continue；
    返回 false -> `runStart = index`（该字符按普通字符在下面重读）。
  - `code === QUOTE`：`index > runStart` 时先 `#appendString(open, slice)`；`#endString(open)`；返回 `index+1`。
  - `code === BACKSLASH`：`index > runStart` 时先 append；`open.escape = "\\"`；`runStart = index+1`。
- 循环结束：`text.length > runStart` 时 append 剩余；返回 `text.length`。

**`#readScalar(text, start): number`**：`end` 前进到分隔符（`isDelimiter`）；`end > start` 时
`#scalar += slice`，若 `#scalarSlot !== undefined` 则 `dirty = true`；`end < len` 时 `#endScalar()`；返回 `end`。
（注意：scalar 已存在才进入此函数，结构性分隔符不会被消费——`#endScalar` 后返回 `end`，外层循环再处理该分隔符。）

**`#structural(char)`**：
- `" "`/`"\t"`/`"\n"`/`"\r"` -> 返回。
- `'"'` -> `#startString()`。
- `'{'` / `'['` -> `#openContainer({} 或 [])`。
- `'}'` / `']'` -> `#closeContainer()`（**不校验类型匹配**：`}` 也会关闭数组）。
- `':'` -> 栈顶为非数组对象且 `phase === "colon"` 时置 `"value"`。
- `','` -> 栈顶存在时：数组置 `"value"`，对象置 `"key"`。
- 默认：`#scalar = char`；`#scalarSlot = #valueSlot()`；slot 存在则 `dirty = true`。

**`#valueSlot()`**：无 frame -> `#root === undefined ? ROOT_SLOT : undefined`（根值之后的位置无效）；
数组 -> `{element, index: container.length}`；对象且 `(phase === "value" || phase === "colon") && key !== undefined`
-> `{member, key}`（容忍缺失冒号）；否则 undefined。

**`#startString()`**：栈顶是对象且 `phase` 为 `"key"` 或 `"comma"` -> 新建 `{target:"key", slot:undefined, path:undefined}`（容忍缺失逗号）；
否则 `slot = #valueSlot()`、`path = #reportedPath(slot)`，新建 `{target:"value", slot, path}`；
`#place(slot, "")`（**值字符串立即可见为空串**）；path 存在则 `change = #report({kind:"set", path, value:""})`。

**`#reportedPath(slot)`**：slot 为 undefined -> undefined；root -> `[]`；栈顶 `!reportsInside` -> undefined；
否则 `[...frame.path!, slot.kind === "member" ? key : index]`。

**`#report(change)`**：push 进 `#changes` 并返回同一对象（后续会被原地扩展）。

**`#appendString` 的"就地扩展"判定**（容易写错）：`change = open.change`；仅当 `change !== undefined` **且**
`#changes.at(-1) === change`（引用相等，即它仍是本片段最后一条变更）时，才原地扩展——
`set` 扩展 `change.value = (change.value as string) + text`，`append` 扩展 `change.text += text`；
否则 `open.change = #report({kind:"append", path: open.path, text})`（**新** append 变更）。
这意味着"值字符串的 set 会被后续追加原地改成更长的字符串"，而中间插入了其它变更后就会另起 append。

**`#continueEscape(open, char): boolean`**：
- `escape === "\\"`：`SIMPLE_ESCAPES[char]` 命中 -> 清空 escape、`#appendString(decoded)`、true；
  `char === "u"` -> `escape = "\\u"`、true；否则**非法转义**：`escape = ""`、`#appendString("\\")`、返回 **false**
  （该字符稍后按普通字符读入）。
- 否则（`escape` 形如 `\u` + 已读十六进制）：是 hex -> 追加；长度 < 6 时 true；达到 6 ->
  `String.fromCharCode(parseInt(escape.slice(2), 16))`、清空、append、true。
- 非 hex：`literal = escape`（如 `"\u12"`）、清空、`#appendString(literal)`、返回 false（该字符按普通字符重读）。

**`#appendString(open, text)`**：`open.text += text`；若 `target !== "value"` 直接返回；
`#place(open.slot, open.text)`；无 path 返回；若 `open.change` 存在且是 `#changes` 的**最后一个**元素 ->
原地扩展（`set` 扩展 `value` 字符串，`append` 扩展 `text`）；否则 `open.change = #report({kind:"append", path, text})`。

**`#endString(open)`**：`#string = undefined`；`target === "key"`：无 frame 则返回（根上的键丢弃），否则
`frame.key = open.text`、`frame.phase = "colon"`；值字符串 -> `#valueEnded(栈顶)`。

**`#endScalar()`**：保存 token 与 slot 后清空；`scalarValue(token)` 非 undefined -> `#place(slot, value)`；
`#reportedPath(slot)` 非 undefined -> `#report({kind:"set", path, value})`；然后 `#valueEnded(栈顶)`。
（非法 token 不改变值，但仍推进状态。）

**`#pendingScalar()`**：slot 为 undefined、token 为空、或 `token.length > 64` -> undefined；
否则 `pendingScalarValue(token)`，undefined -> undefined；否则 `{slot, value}`。

**`#openContainer(container)`**：`slot = #valueSlot()`、`path = #reportedPath(slot)`；`#place(slot, container)`；
push frame：`reportsInside = path !== undefined && path.length < 32`、`size = 0`、`key = undefined`、
`phase = 数组 ? "value" : "key"`；`#pathCost += 1`；`dirty = true`；
path 存在 -> `#report({kind:"set", path, value: [] 或 {}})`（**工作容器持续变化，变更对象永不变化**，见注释）。

**`#closeContainer()`**：`frame = stack.pop()`；无 frame -> 返回（游离闭合符忽略）；`#pathCost -= frame.size + 1`；
`dirty = true`；若 `frame.path !== undefined && !frame.reportsInside` ->
`#report({kind:"set", path: frame.path, value: frame.container})`；然后 `#valueEnded(栈顶)`。

**`#valueEnded(frame)`**：无 frame -> 若 `#root !== undefined` 则 `#done = true`（根值之后的文本被忽略，
但**只在根值存在之后**）；否则 `frame.phase = "comma"`。

**`#place(slot, value)`**：slot undefined -> 返回；`dirty = true`；root -> `#root = value`；否则取栈顶 frame：
数组：若 `slot.index === elements.length` 则 `frame.size++`、`#pathCost++`；再 `elements[index] = value`；
对象：`!Object.hasOwn(members, slot.key)` 则 `frame.size++`、`#pathCost++`；`setMember(...)`。

**`#copyOpenPath(pending)`**：栈空 -> `pending?.slot.kind === "root" ? pending.value : #root`；
否则从**最深**frame 开始向上：拷贝容器（数组 `slice()`，对象 `{...}`）；最内层且有 pending -> `setSlot(copy, pending.slot, pending.value)`；
若有 child -> `setSlot(copy, childSlot, child)`；`child = copy`、`childSlot = frame.slot`；
最后返回 `childSlot?.kind === "root" ? child : #root`。

**`ToolCallArgumentsStream`**（字段 `#text`、`#parser`、`#snapshot`、`#arguments`）：
- `get text()`：原始参数文本。
- `append(delta): ToolCall["arguments"]`：`#text += delta`；`parser.append(delta)`；返回 `#partial()`。
- `replace(text): { delta: string | undefined; arguments }`：若 `text.startsWith(#text)` ->
  `delta = text.slice(#text.length)`，返回 `{delta, arguments: delta === "" ? #partial() : append(delta)}`；
  否则重置 `#text = ""`、新 parser，返回 `{delta: undefined, arguments: append(text)}`。
- `finish(): Pick<ToolCall,"arguments"|"argumentsError">` = `parseToolCallArguments(#text)`。
- `#partial()`：`snapshot = parser.snapshot()`；与上次**引用相等** -> 返回缓存的 `#arguments`；
  否则缓存 snapshot；若 snapshot 是**非 null 非数组 object** -> 作为 arguments，否则 `{}`。

辅助函数：
- `SIMPLE_ESCAPES = {'"':'"', '\\':'\\', '/':'/', b:"\b", f:"\f", n:"\n", r:"\r", t:"\t"}`。
- `isDelimiter(ch)`：空格、`\t`、`\n`、`\r`、`,`、`:`、`{`、`}`、`[`、`]`、`"`（**含双引号**）。
- `isHexDigit(ch)`：0-9 / a-f / A-F。
- `scalarValue(token)`：先精确匹配三个字面量；否则 `NUMBER.test(token) ? Number(token) : undefined`。
- `pendingScalarValue(token)`：任一 `literal.startsWith(token)` -> 该字面量值；`NUMBER.test` -> `Number(token)`；
  否则 `NUMBER_WITHOUT_EXPONENT_DIGITS.exec(token)`，null -> undefined，否则 `Number(m[1])`。
- `setSlot(container, slot, value)`：element -> 赋值；member -> `setMember`。
- `setMember(members, key, value)`：`key === "__proto__"` -> `Object.defineProperty(members, key,
  {value, writable:true, enumerable:true, configurable:true})`（**必须是自有成员**，不能改原型）；
  否则普通赋值。
  重要：普通赋值对已存在键**保持原插入位置**（JS 对象键序），这正是"重复键保留第一次位置"的实现机制。
  Rust 需用 `indexmap::IndexMap`（或等价"插入序 + 覆盖保位"语义），不能按字母排序。

**Rust 建议**：`StreamingJsonParser` 用 `serde_json::Value` + `IndexMap` 代替 JS 对象/数组。
`#copyOpenPath` 可用 `Rc`/`Arc` 共享已完成子树 + 仅深拷贝 open path（与 JS 的写时复制等价）；
`#pathCost`/`#credit` 必须原样实现，因为 `snapshot()` **返回旧快照**的行为是外部可观察的
（测试 `streaming-json.test.ts` 断言"大容器打开时预览滞后"）。

#### 2.2.27 `utils/transcript.ts`（263 行）

```ts
export type TranscriptToolSupport = "none" | "additions" | "changes";
export interface ToolStateChanges { toolsAdded: Tool[]; toolsRemoved: ToolReference[] }
export interface TranscriptTools { requestTools: Tool[]; foldedSystemMessages: number }
export type TranscriptMessages = readonly { role: string }[];

export function createInitialSystemMessage(systemPrompt: string|undefined, tools: Tool[]|undefined): SystemMessage | undefined
export function normalizeContext(context: Context): TranscriptContext
export function getInitialSystemMessage(messages): SystemMessage | undefined
export function withoutInitialSystemMessage(messages: Message[]): Message[]
export function getCurrentTools(messages): Tool[]
export function getCurrentSystemMessage(messages): SystemMessage | undefined
export function getCurrentSystemPrompt(messages): string
export function collapseSystemMessages(context: TranscriptContext): TranscriptContext
export function resolveTranscript(context, supportsMidConvoSystemMessages: boolean|undefined): TranscriptContext
export function toToolDeclaration(tool: Tool): Tool
export function declarationsEqual(left, right): boolean
export function getToolStateChanges(previous, current): ToolStateChanges
export function getDeclaredTools(messages): Tool[]
export function hasToolRedefinitions(messages): boolean          // @deprecated
export function hasNonAdditiveToolChanges(messages): boolean
export function resolveTranscriptTools(messages, support): TranscriptTools
```
- `createInitialSystemMessage`：`hasSystemPrompt = systemPrompt !== undefined && length > 0`；
  `hasTools = tools !== undefined && length > 0`；两者皆假返回 undefined；否则
  `{role:"system", content: systemPrompt ?? "", ...(hasTools ? {toolsAdded: tools} : {}), timestamp: 0}`。
- `normalizeContext`：initial 存在则 `[initial, ...context.messages]` 否则 `context.messages`；返回
  `{messages} as TranscriptContext`（品牌类型；**同步**、无拷贝以外的副作用）。
- `getInitialSystemMessage`：`messages[0]` 存在且 role 为 `"system"` 才返回。
- `withoutInitialSystemMessage`：有首条 system 则 `slice(1)`（**返回新数组**），否则原数组。
- `getCurrentTools`：对每个 system 消息：先 `toolsRemoved ?? []` 逐个 `delete(name)`，再 `toolsAdded ?? []` 逐个 `set(name, tool)`；
  返回 `[...map.values()]`（**首次插入顺序**，删除后重新添加会移到末尾）。
- `getCurrentSystemMessage`：遍历 system 消息：`timestamp ??= message.timestamp`（**第一条**的时间戳）；
  `contentText` 非空则 push；sections：null -> delete，否则 set；最后 `tools = getCurrentTools`。
  `timestamp === undefined && tools.length === 0` -> undefined；否则
  `{role:"system", content: content.join("\n\n"), ...(sections.size>0 ? {sections} : {}),
  ...(tools.length>0 ? {toolsAdded: tools} : {}), timestamp: timestamp ?? 0}`。
- `getCurrentSystemPrompt`：无消息返回 `""`，否则 `getSystemMessageText`。
- `collapseSystemMessages`：`head = getCurrentSystemMessage`；过滤掉所有 role 为 system 的消息；head 存在则前置。
- `resolveTranscript`：`supportsMidConvoSystemMessages` 为真原样返回，否则 collapse。
- `toToolDeclaration`：`{name, description, parameters: JSON.parse(JSON.stringify(parameters)), ...(constrainedSampling === undefined ? {} : {constrainedSampling})}`。
  **丢弃**工具对象上其它字段（如 `execute`、`display`、typebox 符号键）；JSON 往返也把 `undefined` 字段去掉。
- `declarationsEqual`：两侧先 `toToolDeclaration`，再比较 `JSON.stringify` 字符串（键序由构造顺序固定：name、description、
  parameters、constrainedSampling）。
- `getToolStateChanges(previous, current)`：`toolsAdded` = current 中"previous 无同名"或"声明不等"的项，映射为
  `toToolDeclaration`；`toolsRemoved` = previous 中"current 无同名"或"声明不等"的项，映射为 `{name}`。
  顺序沿用各自输入数组顺序。**改名 = 一删一增**。
- `getDeclaredTools`：所有 `toolsAdded` 中**首次声明顺序**的定义（`set` 覆盖值但保留首次位置）。
- `hasToolRedefinitions`：同名且声明不等出现过 -> true（@deprecated，无内置传输需要）。
- `hasNonAdditiveToolChanges`：`findLastNonAdditiveToolChange(仅 system 消息) >= 0`。
- `findLastNonAdditiveToolChange`：维护 `declared: Set<string>`；有 `toolsRemoved.length > 0` 则 `last = index`；
  对每个 `toolsAdded`：若 name **已声明过**（即使此前被移除，见注释 Kimi K3）则 `last = index`，然后 add。
  返回最后一个此类索引或 -1。
- `resolveTranscriptTools(messages, support)`：
  - `"none"` -> `{requestTools: getCurrentTools(systemMessages), foldedSystemMessages: systemMessages.length}`。
  - `"changes"` -> `{requestTools: [], foldedSystemMessages: 0}`。
  - `"additions"` -> `folded = Math.max(首个消息是 system ? 1 : 0, findLastNonAdditiveToolChange + 1)`；
    `requestTools = getCurrentTools(systemMessages.slice(0, folded))`；`foldedSystemMessages = folded`。
  语义（源注释）：`"none"` 顶层列表永远是当前工具；`"additions"` 顶层列表取"最后一次非增量变更之后"的状态，
  之后的纯新增保持原位锚定，因此每次移除才失效缓存一次；`"changes"` 顶层为空，每条 system 消息（含首条）就地声明，
  就地声明可替换更早的就地声明但**不能**替换顶层声明；移除无法上线，只能以文本通告且被移除工具保持已声明。

#### 2.2.28 `utils/validation.ts`（353 行）

```ts
export function validateToolCall(tools: Tool[], toolCall: ToolCall): any
export function validateToolArguments(tool: Tool, toolCall: ToolCall): any
```
内部：`validatorCache = WeakMap<object, CompiledValidator>`（按 schema 对象身份缓存，Rust 侧按 schema 指针/`Arc` 地址或
`TypeId` 缓存）；`TYPEBOX_KIND = Symbol.for("TypeBox.Kind")`（用于判断 schema 是否为真正的 TypeBox schema）。
`JsonSchemaObject` 是宽松的 JSON Schema 视图：`type?: string|string[]`、`properties`、`required`、`items`（单/元组）、
`additionalProperties`、`allOf`、`anyOf`、`oneOf`。

**`getSchemaTypes(schema)`**：string -> `[type]`；数组 -> 过滤 string；否则 `[]`。

**`matchesJsonType(value, type)`**：`number` -> `typeof === "number"`（**含 NaN/Infinity**）；
`integer` -> number 且 `Number.isInteger`；`boolean`；`string`；`null` -> `=== null`；
`array` -> `Array.isArray`；`object` -> `typeof === "object" && !== null && !Array.isArray`；其他 -> false。

**`coercePrimitiveByType(value, type)`**（**逐分支精确**）：
- `number`：null -> `0`；string 且 trim 非空且 `Number(v)` 有限 -> 该数；boolean -> `1`/`0`；否则原值。
- `integer`：null -> `0`；string 且 trim 非空且 `Number(v)` 为整数 -> 该数；boolean -> `1`/`0`；否则原值。
- `boolean`：null -> `false`；`"true"` -> true；`"false"` -> false；数字 `1` -> true、`0` -> false；否则原值。
- `string`：null -> `""`；number/boolean -> `String(v)`；否则原值。
- `null`：`""`、`0`、`false` -> null（**注意用 `===`，因此 `0` 与 `false` 也命中**）；否则原值。
- 其他 -> 原值。
`String(v)` 的 JS 数字格式化（`1e21` -> `"1e+21"`、`0.000001` -> `"0.000001"`、`1e-7` -> `"1e-7"`）必须复刻，见 §6。

**`applySchemaObjectCoercion(value, schema)`**：先对有 `properties` 的每个键**存在于 value**（`key in value`，含原型链）
的项递归 `coerceWithJsonSchema`；再若 `additionalProperties` 是对象，对不在 `properties` 键集合中的每个键递归。

**`applySchemaArrayCoercion(value, schema)`**：`items` 为数组（元组）时按索引且该位置 schema 存在才递归；否则 `items` 为对象时对所有元素递归。

**`coerceWithUnionSchema(value, schemas)`**：
1. 依次用各子 schema 的 validator `Check(value)`，任一通过 -> 原值返回（**优先保留原值**）。
2. 再依次 `structuredClone(value)` 后 coerce，若 coerce 结果通过该子 schema 校验 -> 返回该副本。
3. 都不行 -> 原值。

**`coerceWithJsonSchema(value, schema)`**（顺序固定）：
1. 依次应用 `allOf` 每个子 schema。
2. 有 `anyOf` -> `coerceWithUnionSchema`；有 `oneOf` -> 同理。
3. `schemaTypes`；`matchesUnionMember = types.length > 1 && 任一类型已匹配`；
   `types.length > 0 && !matchesUnionMember` -> 按 types 顺序尝试 `coercePrimitiveByType`，**第一个产生不同值**的采用并 break。
4. `types` 含 `object` 且值是非 null 非数组对象 -> `applySchemaObjectCoercion`。
5. `types` 含 `array` 且是数组 -> `applySchemaArrayCoercion`。

**`normalizeOptionalNulls(value, schema)`**（原地删除可选 null）：
- 数组：`items` 为数组（元组）时按索引且该位置 schema 存在才递归（**只遍历 `value.length`**）；否则 `items` 为对象时
  对所有元素递归；返回。
- 非对象/null/无 `properties` -> 返回。
- `required = new Set(schema.required ?? [])`；对每个 `properties` 键且 `key in object`：
  若 `object[key] === null && !required.has(key) && propertySchema.$ref 不是 string && 子 schema 校验 null 失败`
  -> `delete object[key]`；否则递归该值。
  意义：把"可选字段显式 null"变成"缺省"，交给 schema 默认值处理；`$ref` 与"接受 null 的 schema"不改。

**`getValidator(schema)`**：WeakMap 缓存；未命中 `Compile(schema)`（typebox 编译）。

**`formatValidationPath(error: TLocalizedValidationError)`**：
- `keyword === "required"`：取 `params.requiredProperties[0]`，`basePath = instancePath 去掉前导 "/" 并把 "/" 换 "."`；
  返回 `basePath ? \`${basePath}.${prop}\` : prop`（无属性名则走下面的通用路径）。
- 通用：`instancePath` 去前导 `/`、`/` 换 `.`；空 -> `"root"`。
  （数组索引因此变成 `a.0.b` 形态，而非 `a[0].b`。）

**`validateToolCall(tools, toolCall)`**：按 name 线性查找；未找到 ->
`Error(\`Tool "${toolCall.name}" not found\`)`；否则 `validateToolArguments`。

**`validateToolArguments(tool, toolCall)`**：
1. 若 `toolCall.argumentsError` -> `Error(\`Invalid arguments for tool "${name}": ${argumentsError}\`)`。
2. `args = structuredClone(toolCall.arguments)`（**不改调用方对象**）。
3. `normalizeOptionalNulls(args, tool.parameters)`；`Value.Convert(tool.parameters, args)`（typebox 的类型强制转换，原地）。
4. validator = 编译并缓存。
5. 若 `tool.parameters` **没有** `TypeBox.Kind` 自有符号（即普通 JSON Schema）：
   `coerced = coerceWithJsonSchema(args, parameters)`；若 `coerced !== args`：
   - 两者都是非 null object -> 清空 `args` 的所有自有键再 `Object.assign(args, coerced)`（保持 args 身份）；
   - 否则 `return validator.Check(coerced) ? coerced : args`。
6. `validator.Check(args)` 通过 -> 返回 `args`。
7. 否则收集 `validator.Errors(args)`，每项格式化为 `  - ${path}: ${message}`（两个空格 + 短横）；无错误 ->
   `"Unknown validation error"`。
8. 抛出
```
Validation failed for tool "<name>":
  - <path>: <message>
  - ...

Received arguments:
<JSON.stringify(arguments, null, 2)>
```
（注意 `Received arguments` 用的是**原始** `toolCall.arguments`，不是 coerced 后的 args；缩进 2 空格。）

**Rust 可行性说明**：`Value.Convert` 与 `Compile`/`Check`/`Errors` 来自 typebox。Rust 侧建议：
- 以 `jsonschema` crate 做 `Check`/`Errors`（**错误文本与路径格式需自建**，因为要求逐字一致）；
- `Value.Convert` 的逐字段转换规则需手写（见 typebox 文档：string->number/boolean、number->string 等），
  或直接复用第 5 步的 `coerceWithJsonSchema` 作为唯一强制转换通道，并在测试里对齐差异。
- `$ref` 支持：typebox `Compile` 支持 `$ref`/`$defs`；`normalizeOptionalNulls` 对 `$ref` 字段特殊对待。

### 2.3 `src/providers/**`

#### 2.3.1 `providers/all.ts`（190 行）

```ts
export { radiusProvider };
export type BuiltinProvider = keyof typeof MODELS;                      // 42 个 id
export function getBuiltinModel(provider, modelId): Model<Api>          // 类型级 CatalogApi 推导；运行时 MODELS[provider]?.[modelId]，缺失 undefined
export function getBuiltinImageModel(provider, modelId): ImageModel<ImageApi>
export function getBuiltinClassifierModel(provider, modelId): ClassifierModel<ClassifierApi>
export function getBuiltinProviders(): BuiltinProvider[]                // Object.keys(MODELS)
export function getBuiltinModelDataGeneratedAt(): number | undefined    // Date.parse(data/.manifest.json 的 generatedAt)，NaN -> undefined
export function getBuiltinModels(provider): Model<Api>[]                // Object.values(MODELS[provider] ?? {})
export function getBuiltinImageModels(provider): ImageModel<ImageApi>[]
export function getBuiltinClassifierModels(provider): ClassifierModel<ClassifierApi>[]
export function getAllBuiltinModels(provider): AnyModel[]               // chat + image + classifier
export function builtinProviders(): Provider[]                          // 42 个工厂，顺序见下
export function builtinModels(options?: CreateModelsOptions): MutableModels
```
`builtinProviders()` 顺序（**固定**，CLI `list` 与 help 输出依赖它）：
amazon-bedrock, ant-ling, anthropic, azure, baseten, cerebras, cloudflare-ai-gateway, cloudflare-workers-ai,
deepseek, fireworks, github-copilot, google, google-vertex, groq, huggingface, kimi-coding, meta, minimax, minimax-cn,
mistral, moonshotai, moonshotai-cn, nvidia, openai, openai-codex, opencode, opencode-go, openrouter, qwen-token-plan,
qwen-token-plan-cn, qwen-token-plan-individual, radius, together, typesafe, vercel-ai-gateway, xai, xiaomi,
xiaomi-token-plan-ams, xiaomi-token-plan-cn, xiaomi-token-plan-sgp, zai, zai-coding-cn。
`MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS` 的键顺序（来自 `models.generated.ts`，与上面一致，且 `radius` 在其中）。
`builtinModels`：`createModels(options)` 后按顺序 `setProvider`。

#### 2.3.2 `src/models.generated.ts`（309 行，生成物）

三个只读对象 `MODELS`、`IMAGE_MODELS`、`CLASSIFIER_MODELS`，每个键为 provider id、值为对应
`<PROVIDER>_MODELS`/`_IMAGE_MODELS`/`_CLASSIFIER_MODELS` 的引用。仅类型导出用于 `BuiltinProvider`。
Rust 侧：**不要**在源码里重复 42 个常量；用构建脚本（或 `include_str!` 的生成 JSON）在编译期生成静态表，
但**键顺序必须保持上面顺序**（`IndexMap` 或有序 vec）。

#### 2.3.3 每个 `<provider>.models.ts` 分片（42 个文件，各 **14** 行，模板完全一致）

模板逐字：
```ts
// This file is auto-generated by scripts/generate-models.ts
// Do not edit manually - run 'npm run generate-models' to update

import values from "./data/<provider>.json" with { type: "json" };
import { flattenChatModelCatalog, flattenClassifierModelCatalog, flattenImageModelCatalog, type ChatModelCatalog, type ClassifierModelCatalog, type ImageModelCatalog } from "../model-catalog.ts";

export const <UPPER>_MODELS: ChatModelCatalog<typeof values, "<provider>"> =
	flattenChatModelCatalog("<provider>", values);

export const <UPPER>_IMAGE_MODELS: ImageModelCatalog<typeof values, "<provider>"> =
	flattenImageModelCatalog("<provider>", values);

export const <UPPER>_CLASSIFIER_MODELS: ClassifierModelCatalog<typeof values, "<provider>"> =
	flattenClassifierModelCatalog("<provider>", values);
```
`<UPPER>` 为 provider id 大写、`-` 换 `_`（如 `CLOUDFLARE_AI_GATEWAY`）。
`providers/data-json.d.ts`：`declare module "*.json" { const value: unknown; export default value; }`。

#### 2.3.4 42 个 provider 工厂逐字清单

| 文件 | 工厂 | id | name | baseUrl | api | models | 备注 |
|---|---|---|---|---|---|---|---|
| amazon-bedrock.ts | `amazonBedrockProvider` | `amazon-bedrock` | Amazon Bedrock | — | `bedrock-converse-stream` | AMAZON_BEDROCK_MODELS | 自定义 `bedrockAuth`（见下） |
| ant-ling.ts | `antLingProvider` | `ant-ling` | Ant Ling | `https://api.ant-ling.com/v1` | `openai-completions` | ANT_LING_MODELS | |
| anthropic.ts | `anthropicProvider` | `anthropic` | Anthropic | `https://api.anthropic.com` | `anthropic-messages` | ANTHROPIC_MODELS | 自定义 apiKey auth（name `Anthropic API key`，resolve 链见 2.3.15）+ oauth `Anthropic (Claude Pro/Max)`（isSubscription） |
| azure.ts | `azureProvider` | `azure` | Azure | — | `azure-openai-responses` + `openai-completions` | AZURE_MODELS | 见下 azure 包装（**只有 `openai-completions` 一项被 `azureStreams(...)` 包裹**；`azure-openai-responses` 直接 `azureOpenAIResponsesApi()`） |
| baseten.ts | `basetenProvider` | `baseten` | Baseten | `https://inference.baseten.co/v1` | `openai-completions` | BASETEN_MODELS | |
| cerebras.ts | `cerebrasProvider` | `cerebras` | Cerebras | `https://api.cerebras.ai/v1` | `openai-completions` | CEREBRAS_MODELS | |
| cloudflare-ai-gateway.ts | `cloudflareAIGatewayProvider` | `cloudflare-ai-gateway` | Cloudflare AI Gateway | — | 三个 API 均 `cloudflareStreams(...)` | CLOUDFLARE_AI_GATEWAY_MODELS | api map 固定三项（注释说明 models.dev 会增删 `workers-ai/*` 条目） |
| cloudflare-workers-ai.ts | `cloudflareWorkersAIProvider` | `cloudflare-workers-ai` | Cloudflare Workers AI | — | `openai-completions` = `cloudflareStreams(...)`；classifier `cloudflare-workers-ai-system-one` = `cloudflareClassifier(...)` | **`[...Object.values(CLOUDFLARE_WORKERS_AI_MODELS), ...Object.values(CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS)]`**（classifier 模型被并入 chat models 列表） | 常量名前缀是 `CLOUDFLARE_WORKERS_AI_`，不是 `WORKERS_AI_` |
| deepseek.ts | `deepseekProvider` | `deepseek` | DeepSeek | `https://api.deepseek.com` | `openai-completions` | DEEPSEEK_MODELS | |
| fireworks.ts | `fireworksProvider` | `fireworks` | Fireworks | `https://api.fireworks.ai/inference` | `anthropic-messages` + `openai-completions` | FIREWORKS_MODELS | |
| github-copilot.ts | `githubCopilotProvider` | `github-copilot` | GitHub Copilot | `https://api.individual.githubcopilot.com` | 三个 API | GITHUB_COPILOT_MODELS | `filterModels`：oauth 且 `availableModelIds` 为 string[] 时按集合过滤，否则原样 |
| google.ts | `googleProvider` | `google` | Google | `https://generativelanguage.googleapis.com/v1beta` | `google-generative-ai` | GOOGLE_MODELS | |
| google-vertex.ts | `googleVertexProvider` | `google-vertex` | Google Vertex AI | — | `google-vertex` | GOOGLE_VERTEX_MODELS | 自定义 `vertexAuth`（见下） |
| groq.ts | `groqProvider` | `groq` | Groq | `https://api.groq.com/openai/v1` | `openai-completions` | GROQ_MODELS | |
| huggingface.ts | `huggingfaceProvider` | `huggingface` | Hugging Face | `https://router.huggingface.co/v1` | `openai-completions` | HUGGINGFACE_MODELS | |
| kimi-coding.ts | `kimiCodingProvider` | `kimi-coding` | Kimi For Coding | `https://api.kimi.com/coding/v1` | `openai-completions` | KIMI_CODING_MODELS | oauth `name: "Kimi Code (subscription)"` |
| meta.ts | `metaProvider` | `meta` | Meta | `https://api.meta.ai/v1` | `openai-responses` | META_MODELS | oauth `name: "Meta (Muse subscription)"` |
| minimax.ts | `minimaxProvider` | `minimax` | MiniMax | `https://api.minimax.io/anthropic` | `anthropic-messages` | MINIMAX_MODELS | |
| minimax-cn.ts | `minimaxCnProvider` | `minimax-cn` | MiniMax CN | `https://api.minimaxi.com/anthropic` | `anthropic-messages` | MINIMAX_CN_MODELS | |
| mistral.ts | `mistralProvider` | `mistral` | Mistral | `https://api.mistral.ai` | `mistral-conversations` | MISTRAL_MODELS | |
| moonshotai.ts | `moonshotaiProvider` | `moonshotai` | Moonshot AI | `https://api.moonshot.ai/v1` | `openai-completions` | MOONSHOTAI_MODELS | |
| moonshotai-cn.ts | `moonshotaiCnProvider` | `moonshotai-cn` | Moonshot AI CN | `https://api.moonshot.cn/v1` | `openai-completions` | MOONSHOTAI_CN_MODELS | |
| nvidia.ts | `nvidiaProvider` | `nvidia` | NVIDIA | `https://integrate.api.nvidia.com/v1` | `openai-completions` | NVIDIA_MODELS | |
| openai.ts | `openaiProvider` | `openai` | OpenAI | `https://api.openai.com/v1` | `openai-responses` | OPENAI_MODELS + OPENAI_CLASSIFIER_MODELS | `filterAllModels`：oauth 时过滤掉 classifier；classifiers `openai-decisions` |
| openai-codex.ts | `openaiCodexProvider` | `openai-codex` | OpenAI Codex (legacy) | `https://chatgpt.com/backend-api` | `openai-codex-responses` | OPENAI_CODEX_MODELS | 仅 oauth（`OpenAI (ChatGPT Plus/Pro)`），**无 apiKey** |
| opencode.ts | `opencodeProvider` | `opencode` | OpenCode Zen | — | 四个 API，均 `withOpenCodeSessionHeader` | OPENCODE_MODELS + CLASSIFIER | classifiers `typesafe-system-one` |
| opencode-go.ts | `opencodeGoProvider` | `opencode-go` | OpenCode Go | — | 同上四个 | OPENCODE_GO_MODELS | |
| openrouter.ts | `openrouterProvider` | `openrouter` | OpenRouter | `https://openrouter.ai/api/v1` | `anthropic-messages` + `openai-completions` | OPENROUTER_MODELS + IMAGE + CLASSIFIER | images `openrouter-images`；classifiers `typesafe-system-one`；oauth `OpenRouter OAuth` / `Sign in with OpenRouter` |
| qwen-token-plan.ts | `qwenTokenPlanProvider` | `qwen-token-plan` | Qwen Token Plan | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | `openai-completions` | QWEN_TOKEN_PLAN_MODELS | |
| qwen-token-plan-cn.ts | `qwenTokenPlanCnProvider` | `qwen-token-plan-cn` | Qwen Token Plan CN | `https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1` | `openai-completions` | ... | |
| qwen-token-plan-individual.ts | `qwenTokenPlanIndividualProvider` | `qwen-token-plan-individual` | Qwen Token Plan Individual | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | `openai-completions` | ... | |
| radius.ts | `radiusProvider(options?)` | `radius`（可覆盖） | `Radius`（可覆盖） | 动态 | `kiana-messages` | RADIUS_MODELS（仅默认 gateway） | 见 2.3.7 |
| together.ts | `togetherProvider` | `together` | Together | `https://api.together.ai/v1` | `openai-completions` | TOGETHER_MODELS | |
| typesafe.ts | `typesafeProvider` | `typesafe` | TypeSafe | — | 无 chat api（只有 classifiers） | `TYPESAFE_CLASSIFIER_MODELS`（**注意：`TYPESAFE_MODELS`/`TYPESAFE_IMAGE_MODELS` 也由分片导出，但本 provider 只使用 CLASSIFIER 分片**） | 返回**未参数化**的 `Provider`（非 `Provider<...>`）；classifiers `typesafe-system-one` |
| vercel-ai-gateway.ts | `vercelAIGatewayProvider` | `vercel-ai-gateway` | Vercel AI Gateway | `https://ai-gateway.vercel.sh` | `anthropic-messages` | VERCEL_AI_GATEWAY_MODELS + CLASSIFIER | classifiers `typesafe-system-one` |
| xai.ts | `xaiProvider` | `xai` | xAI | `https://api.x.ai/v1` | `openai-responses` | XAI_MODELS | oauth `xAI (Grok/X subscription)`，loginLabel `Sign in with SuperGrok or X Premium` |
| xiaomi.ts / xiaomi-token-plan-ams.ts / xiaomi-token-plan-cn.ts / xiaomi-token-plan-sgp.ts | `xiaomiProvider` / `xiaomiTokenPlanAmsProvider` / `xiaomiTokenPlanCnProvider` / `xiaomiTokenPlanSgpProvider` | `xiaomi` / `xiaomi-token-plan-ams` / `-cn` / `-sgp` | Xiaomi / Xiaomi Token Plan AMS/CN/SGP | `https://api.xiaomimimo.com/v1`（xiaomi）/ `https://token-plan-ams.xiaomimimo.com/v1` / `https://token-plan-cn.xiaomimimo.com/v1` / `https://token-plan-sgp.xiaomimimo.com/v1` | `openai-completions` | 对应 *_MODELS | |
| zai.ts | `zaiProvider` | `zai` | Z.AI | `https://api.z.ai/api/coding/paas/v4` | `openai-completions` | ZAI_MODELS | |
| zai-coding-cn.ts | `zaiCodingCnProvider` | `zai-coding-cn` | Z.AI Coding CN | `https://open.bigmodel.cn/api/coding/paas/v4` | `openai-completions` | ZAI_CODING_CN_MODELS | |

`envApiKeyAuth(name, envVars)`（定义在 `auth/helpers.ts`，**不在本单元**但工厂依赖其契约）：
```ts
{ name,
  login: async (interaction) => {
    interaction.signal.throwIfAborted();
    const key = await interaction.prompt({ type: "secret", message: `Enter ${name}` });
    interaction.signal.throwIfAborted();
    return { type: "api_key", key };
  },
  resolve: async ({ ctx, credential, signal }) => {
    signal.throwIfAborted();
    if (credential?.key) return { auth: { apiKey: credential.key }, env: credential.env, source: "stored credential" };
    for (const envVar of envVars) {
      const value = await ctx.env(envVar);
      signal.throwIfAborted();
      if (value) return { auth: { apiKey: value }, source: envVar };
    }
    return undefined;
  } }
```
**登录提示逐字**为 `Enter ${name}`——例如 openai -> `Enter OpenAI API key`、openrouter -> `Enter OpenRouter API key`、
groq -> `Enter Groq API key`、huggingface -> `Enter Hugging Face token`、github-copilot -> `Enter GitHub Copilot token`。
`lazyOAuth({name, isSubscription?, loginLabel?, load})`：返回完整 `OAuthAuth`，其中 `login`/`refresh`/`toAuth` 都是
`async (...) => (await loaded()).x(...)`；`loaded()` 用 `promise ??= input.load()` 缓存首次加载。

#### 2.3.5 `providers/amazon-bedrock.ts` 的 `bedrockAuth`（逐字）

`name: "AWS credentials or bearer token"`。`login`：
1. `throwIfAborted`；`select` 提示 `"Select Amazon Bedrock authentication method:"`，选项
   `bearer-token`/`Bearer token`、`aws-profile`/`AWS profile`、`credential-chain`/`Existing AWS credential chain`。
2. `throwIfAborted`。`bearer-token` -> `{type:"api_key", key: prompt({type:"secret", message:"Enter Amazon Bedrock bearer token"})}`。
3. 否则先 `notify({type:"info", message:"Amazon Bedrock supports AWS profiles, IAM credentials, and role-based credentials.",
   links:[{label:"AWS credential provider chain", url:"https://docs.aws.amazon.com/sdkref/latest/guide/standardized-credentials.html"}]})`。
4. `aws-profile` -> `{type:"api_key", env:{AWS_PROFILE: prompt({type:"text", message:"Enter AWS profile name"})}}`。
5. 非 `credential-chain` -> `Error(\`Unknown Amazon Bedrock auth method: ${method}\`)`。
6. `credential-chain` -> `prompt({type:"text", message:"Configure AWS credentials, then press Enter to continue"})`，返回 `{type:"api_key"}`。

`resolve` 顺序（每步前后 `throwIfAborted`）：
`credential.key` -> `{auth:{apiKey:key}, env: credential.env, source:"stored credential"}`；
`AWS_BEARER_TOKEN_BEDROCK` -> `{auth:{}, source:"AWS_BEARER_TOKEN_BEDROCK"}`；
`credential.env.AWS_PROFILE ?? env("AWS_PROFILE")` -> `{auth:{}, env: credential.env, source: credential.env?.AWS_PROFILE ? "stored credential" : "AWS_PROFILE"}`；
`AWS_ACCESS_KEY_ID && AWS_SECRET_ACCESS_KEY` -> `{auth:{}, source:"AWS access keys"}`；
`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` -> `"ECS task role"`；
`AWS_CONTAINER_CREDENTIALS_FULL_URI` -> `"ECS task role"`；
`AWS_WEB_IDENTITY_TOKEN_FILE` -> `"web identity token"`；否则 undefined。

#### 2.3.6 `providers/google-vertex.ts` 的 `vertexAuth`（逐字）

`name: "Google Cloud credentials"`。`login`：select `"Select Google Vertex AI authentication method:"`，选项
`api-key`/`Google Cloud API key`、`adc`/`Application Default Credentials`、`service-account`/`Service account credentials file`。
`api-key` -> secret 提示 `"Enter Google Cloud API key"`；非 adc/service-account ->
`Error(\`Unknown Google Vertex AI auth method: ${method}\`)`；
notify（info）消息：adc -> `"Run \`gcloud auth application-default login\`, then provide the project and location."`，
否则 `"Provide a service account credentials file, project, and location."`；links
`{label:"Application Default Credentials", url:"https://cloud.google.com/docs/authentication/provide-credentials-adc"}`；
再依次提示 `"Enter Google Cloud project ID"`、`"Enter Google Cloud location"`，service-account 额外
`"Enter service account credentials file path"`；返回
`{type:"api_key", env:{GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, ...(credentialsPath ? {GOOGLE_APPLICATION_CREDENTIALS} : {})}}`。

`resolve`：`key = credential?.key ?? env("GOOGLE_CLOUD_API_KEY")`，有则
`{auth:{apiKey:key}, source: credential?.key ? "stored credential" : "GOOGLE_CLOUD_API_KEY"}`；
否则 `adcPath = credential?.env?.GOOGLE_APPLICATION_CREDENTIALS ?? env("GOOGLE_APPLICATION_CREDENTIALS")`；
`hasCredentials = ctx.fileExists(adcPath ?? "~/.config/gcloud/application_default_credentials.json")`；
`project = credential?.env?.GOOGLE_CLOUD_PROJECT ?? env("GOOGLE_CLOUD_PROJECT") ?? env("GCLOUD_PROJECT")`；
`location = credential?.env?.GOOGLE_CLOUD_LOCATION ?? env("GOOGLE_CLOUD_LOCATION")`；
三者齐备 -> `{auth:{}, env: credential?.env, source: credential ? "stored credential" : "gcloud application default credentials"}`；
否则 undefined。常量 `VERTEX_ADC_PATH = "~/.config/gcloud/application_default_credentials.json"`
（`ctx.fileExists` 负责展开 `~`）。

#### 2.3.7 `providers/radius.ts` + `radius-config.ts`

`radiusProvider(options: {id?, name?, gateway?} = {})`：**不使用** `createProvider`，手写 Provider 对象。
- `id = options.id ?? "radius"`；`name = options.name ?? "Radius"`；
  `gateway = normalizeRadiusGatewayUrl(options.gateway ?? "https://radius.kiana.dev")`。
- `baselineModels`：仅当 gateway 规范化后等于默认 gateway 时取 `RADIUS_MODELS` 并覆盖 `provider: id`；否则 `[]`。
- 闭包变量 `dynamicModels: Model<"kiana-messages">[] | undefined`；注释：org owner 可禁用模型，**一旦已知目录就整体替换 baseline**。
- auth：`{ apiKey: envApiKeyAuth("Radius API key", ["RADIUS_API_KEY"]), oauth: lazyOAuth({name, load: () => loadRadiusOAuth({name, gateway})}) }`。
- `getModels: () => dynamicModels ?? baselineModels`。
- `refreshModels(context)`：
  1. 有 `context.stored` -> `restored = stored.models.filter(m => m.provider === id)`；
     `publish({update: () => dynamicModels = restored})` 返回 false 则 return。
  2. 无 stored 且 `credential?.type === "oauth"` -> `legacy = getRadiusModels(id, credential)`，非空则
     `publish({persist: {models: legacy, checkedAt: Date.now()}, update: () => dynamicModels = legacy})`，false 则 return。
     （注释：导入旧版 Radius 实现缓存到 ModelsStore。）
  3. `!allowNetwork || signal.aborted` -> return。
  4. `apiKey = credential?.type === "oauth" ? credential.access : credential?.key`；
     `config = await loadRadiusGatewayConfig(gateway, apiKey, signal)`；aborted -> return；
     `refreshed = getRadiusModelsFromConfig(id, config)`；
     `publish({persist:{models: refreshed, checkedAt: Date.now()}, update: () => dynamicModels = refreshed})`。
- `stream` / `streamSimple` 委派 `kianaMessagesApi()`。

`radius-config.ts`：
- `DEFAULT_RADIUS_GATEWAY = "https://radius.kiana.dev"`。
- 类型 `RadiusGatewayModel {id, name, reasoning, thinkingLevelMap?, input, cost, contextWindow, maxTokens}`、
  `RadiusGatewayConfig {baseUrl, models}`、`RadiusOAuthCredential = OAuthCredential & {gatewayConfig?}`。
- `isRadiusGatewayModel(value)`：对象、非数组，且 `id`/`name` 为 string、`reasoning` 为 boolean、`input` 为数组、
  `cost` 为非 null 非数组对象、`contextWindow`/`maxTokens` 为 number。
- `sanitizeRadiusGatewayConfig(config)`：非对象/数组 -> undefined；`baseUrl` 非 string 或 `models` 非数组 -> undefined；
  否则 `{baseUrl, models: models.filter(isRadiusGatewayModel).map(m => ({...m}))}`。
- `normalizeRadiusGatewayUrl(value)`：`/^https?:\/\//iu` 不匹配则前缀 `https://`；再去掉尾部所有 `/`。
- `getRadiusCredentialConfig(credential)`、`getRadiusModelsFromConfig(providerId, config)`（每个模型加
  `api:"kiana-messages"`, `provider: providerId`, `baseUrl: config.baseUrl`）、`getRadiusModels(providerId, credential)`。
- `loadRadiusGatewayConfig(gateway, apiKey?, signal?)`：headers `{accept:"application/json"}`，有 apiKey 加
  `authorization: Bearer <key>`；`fetch(new URL("/v1/config", gateway), {headers, signal})`；
  非 2xx -> `Error(\`Could not load Radius config from ${gateway}: ${status}: ${truncateHttpBody(await response.text())}\`)`；
  body 经 sanitize 失败 -> `Error(\`Invalid Radius config from ${gateway}\`)`。
  `truncateHttpBody`：trim；长度 > 512 时 `` `${ownedSlice(trimmed,0,512)}…` ``（**U+2026 省略号**）。

#### 2.3.8 `providers/cloudflare-auth.ts` / `cloudflare-stream.ts`

`cloudflare-auth.ts`：常量 `CLOUDFLARE_API_KEY="CLOUDFLARE_API_KEY"`、`CLOUDFLARE_ACCOUNT_ID`、`CLOUDFLARE_GATEWAY_ID`。
`resolveValue(name, ctx, credential, signal)`：**逐字段合并**——credential 存在时，API key 取 `credential.key`，
其他取 `credential.env?.[name]`；有值（`!== undefined`，**空串也算有值**）即返回；否则 `throwIfAborted` 后 `ctx.env(name)`，
再 `throwIfAborted`。
`resolveCloudflareEnv(kind, ...)`：读 apiKey、accountId、（ai-gateway 才读）gatewayId；任一缺失（`!value`）返回 undefined；
返回 `{apiKey, env: {CLOUDFLARE_ACCOUNT_ID: accountId, ...(gatewayId ? {CLOUDFLARE_GATEWAY_ID: gatewayId} : {})},
source: credential ? "stored credential" : CLOUDFLARE_API_KEY}`。
- `cloudflareWorkersAIAuth()`：name `"Cloudflare API key"`；login 提示 `"Enter Cloudflare API key"`（secret）与
  `"Enter Cloudflare account ID"`（text），返回 `{type:"api_key", key, env:{CLOUDFLARE_ACCOUNT_ID: accountId}}`；
  resolve -> `{auth:{apiKey}, env, source}`。
- `cloudflareAIGatewayAuth()`：login 额外提示 `"Enter Cloudflare AI Gateway ID"`，env 含两者；resolve 的 auth 为
  **header**：`{"cf-aig-authorization": "Bearer <apiKey>", "Authorization": null, "x-api-key": null}`
  （后两者为"抑制"标记，由头合并逻辑解释），`env` 同 workers。

`cloudflare-stream.ts`：
- `resolveCloudflareModel(model, env)`：无 env 原样；否则把 `baseUrl` 中的 `{CLOUDFLARE_ACCOUNT_ID}`、`{CLOUDFLARE_GATEWAY_ID}`
  `replaceAll` 为 env 值（缺失则保留占位符）；未变则同一对象，否则 `{...model, baseUrl}`。
- `cloudflareStreams(streams)`：包装 stream/streamSimple，dispatch 前 `resolveCloudflareModel(model, options?.env)`。
- `cloudflareClassifier(classifier)`：同上对 classify。

#### 2.3.9 `providers/opencode-headers.ts`

`OPENCODE_SESSION_HEADER = "x-opencode-session"`。`hasHeader(headers, name)`：键小写比较，任一匹配即 true。
`withSessionHeader(options)`：无 `sessionId` 或已存在该头（忽略大小写）-> 原样；否则
`{...options, headers: {...options.headers, "x-opencode-session": options.sessionId}}`。
`withOpenCodeSessionHeader(streams)`：`{...streams, stream, streamSimple}` 均先注入头（**展开保留其它键**，如 fetchDeferred）。

#### 2.3.10 `providers/openrouter-api.ts`

类型 `OpenRouterPricing {prompt?, completion?, input_cache_read?, input_cache_write?, overrides?}`、
`OpenRouterPricingOverride {min_prompt_tokens?, utc_start?, utc_end?, utc_days?, prompt?, completion?,
input_cache_read?, input_cache_write?}`、`OpenRouterModel {id, context_length?, top_provider?: {context_length?,
max_completion_tokens?}, pricing?}`。
`perMillion(value, fallback)`：`Number.parseFloat(value ?? "")`；`perToken >= 0` ->
`Number((perToken * 1_000_000).toFixed(6))`（**NaN >= 0 为 false**，故缺失/非法回落 fallback）；否则 fallback。
`openRouterCost(pricing)`：base 四费率 fallback 0；`overrides` 中**仅**保留 `min_prompt_tokens` 存在且
`utc_start`/`utc_end`/`utc_days` 全为 undefined 的项，转成 tier（各费率 fallback 为 base 对应值）；
tiers 非空 -> `{...base, tiers}`，否则 base。

#### 2.3.11 `providers/openrouter.ts` 的 `personalizeOpenRouterCatalog`

```ts
export async function personalizeOpenRouterCatalog(catalog: readonly AnyModel[], context: RefreshModelsContext): Promise<readonly AnyModel[]>
```
- `key = credential?.type === "oauth" ? credential.access : credential?.key`；无 key 原样返回。
- `fetch(\`${(context.baseUrl ?? "https://openrouter.ai/api/v1").replace(/\/+$/,"")}/models/user\`, {headers:{accept:"application/json",
  authorization:\`Bearer ${key}\`}, signal: AbortSignal.any([context.signal, AbortSignal.timeout(15_000)])})`。
- 401/403/404 -> 原样返回目录（注释：代理配置在 models.json 时端点可能不存在）。
- 非 2xx -> `Error(\`OpenRouter model list request failed: ${status}\`)`。
- body 非对象、null、无 `data` 或 `data` 非数组 -> `Error("Invalid OpenRouter model list response")`。
- 收集 `id` 为 string 的条目到 Map。
- 对每个 chat 模型：无对应条目 -> **丢弃**；否则 `{...model, contextWindow: top_provider.context_length || context_length || model.contextWindow,
  maxTokens: top_provider.max_completion_tokens || model.maxTokens, cost: pricing ? openRouterCost(pricing) : model.cost}`。
  非 chat 模型原样保留。
（注意：该函数**已导出但当前 provider 未在 refreshModels 中调用**——`openrouterProvider` 未设置 `fetchModels`。
保留导出以维持 API 兼容。）

#### 2.3.12 `providers/azure.ts`

`resolveAzureModel(model, options)`：`{...model, baseUrl: resolveAzureBaseUrl(model, options)}`（来自 `api/azure-openai-config.ts`，
**不在本单元**）。`withDeploymentName(model, options)`：`deploymentName = resolveDeploymentName(model, options)`；
等于 `model.id` 则返回原 options；否则返回 `{...options, onPayload: async (payload, payloadModel) => {
  const params = {...payload, model: deploymentName}; return (await options?.onPayload?.(params, payloadModel)) ?? params; }}`。
`azureStreams(streams)`：stream/streamSimple 都在 `lazyStream` 内先解析 baseUrl 与 deployment
（注释：未配置端点的错误应出现在流上而不是从 `stream()` 抛出）。

#### 2.3.13 `providers/faux.ts`（732 行）

常量：`DEFAULT_API = "faux"`、`DEFAULT_PROVIDER = "faux"`、`DEFAULT_MODEL_ID = "faux-1"`、
`DEFAULT_MODEL_NAME = "Faux Model"`、`DEFAULT_BASE_URL = "http://localhost:0"`、`DEFAULT_MIN_TOKEN_SIZE = 3`、
`DEFAULT_MAX_TOKEN_SIZE = 5`；`DEFAULT_USAGE`（全 0，`cost` 无 `totalFromProvider`）。

类型：`FauxModelDefinition {id, name?, reasoning?, input?, inputLimits?, cost?, contextWindow?, maxTokens?}`、
`FauxContentBlock = TextContent | ThinkingContent | ToolCall`、`FauxProviderState {callCount, deferredFetchCount,
cancelledDeferred: DeferredHandle[]}`、`FauxResponseFactory`、`FauxResponseStep = AssistantMessage | FauxResponseFactory`、
`RegisterFauxProviderOptions {api?, provider?, models?, deferred?: {pendingFetches?, pollAfterMs?}, tokensPerSecond?,
tokenSize?: {min?, max?}}`、`FauxProviderRegistration` / `FauxProviderHandle`（字段见源文件：api、models 元组、getModel、
state、setResponses、appendResponses、getPendingResponseCount、unregister（仅 registration））。

辅助：
- `fauxText(text)` -> `{type:"text", text}`；`fauxThinking` -> `{type:"thinking", thinking}`；
  `fauxToolCall(name, arguments, {id?})` -> id 默认 `randomId("tool")`。
- `fauxAssistantMessage(content, options?)`：内容规范化（string -> 单 text 块）；字段
  `{role:"assistant", content, api: "faux", provider: "faux", model: "faux-1", usage: DEFAULT_USAGE,
  stopReason: options.stopReason ?? "stop", ...(deferred/errorMessage/responseId 有则带), timestamp: options.timestamp ?? Date.now()}`。
- `estimateTokens(text) = Math.ceil(text.length / 4)`（**UTF-16 长度**）。
- `randomId(prefix) = \`${prefix}:${Date.now()}:${Math.random().toString(36).slice(2)}\``。
- `contentToText`：string 原样；块 -> text 用 `text`，其他 -> `` `[image:${mimeType}:${data.length}]` ``，`join("\n")`。
- `assistantContentToText`：text -> text；thinking -> thinking；toolCall -> `` `${name}:${JSON.stringify(arguments)}` ``，`join("\n")`。
- `toolResultToText(message)`：`[toolName, ...content 各块 contentToText([block])].join("\n")`。
- `messageToText(message)`：system -> `[getSystemMessageText, ...toolsRemoved 的 \`tool-:${JSON.stringify(tool)}\`,
  ...toolsAdded 的 \`tool+:${JSON.stringify(tool)}\`]` 过滤空串后 `join("\n")`；user -> contentToText；assistant ->
  assistantContentToText；其余 -> toolResultToText。
- `joinedLength(messages, count)`：`(count-1)*2 + 各条长度`（即用 `\n\n` 连接后的长度）。
- `commonPromptPrefixLength(previous, current)`：先按整条消息相同跳过；`rest(messages) = index === messages.length ?
  "" : (index > 0 ? "\n\n" : "") + messages.slice(index).join("\n\n")`；返回
  `joinedLength(previous, index) + commonPrefixLength(rest(previous), rest(current))`（**按 UTF-16 码元比较**）。
- `withUsageEstimate(message, context, options, promptCache)`：
  `prompt = context.messages.map(m => \`${m.role}:${messageToText(m)}\`)`；
  `promptLength = joinedLength(prompt)`；`promptTokens = ceil(promptLength/4)`；`outputTokens = estimateTokens(assistantContentToText(...))`；
  `input = promptTokens`，cacheRead/cacheWrite 0；
  若 `sessionId && cacheRetention !== "none"`：有上次 prompt -> `cachedChars = commonPromptPrefixLength(...)`，
  `cacheRead = ceil(cachedChars/4)`，`cacheWrite = max(0, promptTokens - cacheRead)`；否则 `cacheWrite = promptTokens`；
  然后 **`input = 0`**，并更新缓存。返回新对象（usage 替换，cost 全 0）。
- `splitStringByTokenSize(text, min, max)`：每块 `charSize = max(1, tokenSize*4)`，`tokenSize = min + floor(random()*(max-min+1))`；
  至少返回 `[""]`。
- `cloneMessage(message, api, provider, modelId)`：`structuredClone` 后覆盖 api/provider/model，`timestamp ?? Date.now()`，
  `usage ?? DEFAULT_USAGE`。
- `createDeferredMessage(model, handle)`：`{role:"assistant", content: [], api, provider, model: id, usage: DEFAULT_USAGE,
  stopReason: "deferred", deferred: handle, timestamp: Date.now()}`。
- `createErrorMessage(error, api, provider, modelId)`：content `[]`、`stopReason:"error"`、`errorMessage`。
- `createAbortedMessage(partial)`：`{...partial, stopReason:"aborted", errorMessage:"Request was aborted", timestamp: Date.now()}`。
- `scheduleChunk(chunk, tokensPerSecond)`：无/≤0 -> `queueMicrotask`；否则 `setTimeout((estimateTokens(chunk)/tps)*1000)`。
- `streamWithDeltas(stream, message, min, max, tps, signal)`：`partial = {...message, content: [], stopReason: "pending"}`；
  已 abort -> push `{type:"error", reason:"aborted", error: aborted}` 并 `end(aborted)`；否则 push `start`；
  逐块（每块前检查 abort）：
  - thinking：append `{type:"thinking", thinking:""}`，`thinking_start`，按 token 分块 await + 追加 + `thinking_delta`，
    再 `thinking_end`（`content: block.thinking`）。
  - text：同上 `text_start`/`text_delta`/`text_end`。
  - toolCall：append `{type:"toolCall", id, name, arguments:{}}`，`toolcall_start`；用 `ToolCallArgumentsStream`
    对 `JSON.stringify(block.arguments)` 的分块 `append` 得到 partial arguments；`toolcall_delta`；
    结束后把 arguments 设为**原始对象**并 push `toolcall_end`（带 `toolCall: block`）。
  每个事件都带 `partial: {...partial}`（**浅拷贝**）。
  最后：`stopReason === "pending"` -> `Error("Faux response ended without a stop reason")`；
  error/aborted -> `{type:"error", reason, error: message}` + `end(message)`；否则 `{type:"done", reason: message.stopReason, message}` + `end(message)`。

`createFauxCore(options)`：
- `api = options.api ?? randomId("faux")`（**注意默认不是 `"faux"`，而是随机串**）；
  `provider = options.provider ?? "faux"`；
  `minTokenSize = max(1, min(tokenSize.min ?? 3, tokenSize.max ?? 5))`；`maxTokenSize = max(minTokenSize, tokenSize.max ?? 5)`。
- `pendingResponses`、`tokensPerSecond`、`state`、`promptCache = Map<string, readonly string[]>`、`deferredResponses = Map<id, entry>`。
- 默认模型定义（无 models 时）：`{id:"faux-1", name:"Faux Model", reasoning:false, input:["text","image"],
  cost:{input:0,output:0,cacheRead:0,cacheWrite:0}, contextWindow:128000, maxTokens:16384}`；
  模型对象：`{id, name: name ?? id, api, provider, baseUrl:"http://localhost:0", reasoning: ?? false,
  input: ?? ["text","image"], inputLimits, cost: ?? 全 0, contextWindow: ?? 128000, maxTokens: ?? 16384}`。
- `resolveResponse(step, context, streamOptions, requestModel)`：函数则 `await step(context, streamOptions, state, requestModel)`；
  再 `withUsageEstimate(cloneMessage(resolved, api, provider, requestModel.id), context, streamOptions, promptCache)`。
- `stream`：新建 outer stream；`step = pendingResponses.shift()`；`state.callCount++`；
  `queueMicrotask` 内：先 `await streamOptions?.onResponse?.({status:200, headers:{}}, requestModel)`；
  无 step -> 错误消息 `new Error("No more faux responses queued")`（经 withUsageEstimate）push error + end；
  若 `streamOptions?.deferred`：构造 handle `{provider, modelId, api, id: randomId("deferred"),
  ...(pollAfterMs !== undefined ? {pollAfterMs} : {})}`，存入 map（`pendingFetches = max(0, floor(pendingFetches ?? 0))`，
  `cancelled: false`），然后 `streamWithDeltas(outer, createDeferredMessage(...), ...)`；
  否则 `message = await resolveResponse(...)` 后 `streamWithDeltas`；catch -> error 消息 push + end。
  **同步返回 outer**。
- `streamSimple` = `stream`。
- `fetchDeferred`：`state.deferredFetchCount++`；microtask 内 `onResponse`；查找 entry，缺失或 handle 的
  provider/modelId/api 不匹配 -> `Error(\`Unknown faux deferred response: ${handle.id}\`)`；
  `entry.cancelled` -> `Error(\`Faux deferred response was cancelled: ${handle.id}\`)`；
  `pendingFetches > 0` 则自减并再发一次 deferred 消息；否则若 `!entry.final`：
  解构去掉 `deferred`/`signal`/`onResponse` 得 submissionOptions，`resolveResponse(entry.step, entry.context, submissionOptions, entry.model)`
  （失败则 `createErrorMessage`），再 `streamWithDeltas(entry.final)`。
- `cancelDeferred`：`state.cancelledDeferred.push(structuredClone(handle))`；entry 存在则 `cancelled = true`；
  `await cancelOptions?.onResponse?.({status:200, headers:{}}, requestModel)`。
- `getModel(id?)`：无参返回 `models[0]`；有参线性 `find`。
- 返回 `{api, provider, models, stream, streamSimple, fetchDeferred, cancelDeferred, getModel, state, setResponses,
  appendResponses, getPendingResponseCount}`。

`fauxProvider(options = {})`：`core = createFauxCore(options)`；`provider = createProvider({id: core.provider,
auth: {apiKey: {name: "Faux", resolve: async () => ({auth: {}})}}, models: core.models,
api: {stream, streamSimple, fetchDeferred, cancelDeferred}})`；返回 `{provider, api, models, getModel, state,
setResponses, appendResponses, getPendingResponseCount}`。

#### 2.3.14 `providers/images/register-builtins.ts`（副作用模块）

`generateImagesOpenRouter: ImagesFunction<ImagesOptions>`：懒加载 `../../api/openrouter-images.ts`（promise 缓存，
`||=`），异常 -> `createLazyLoadErrorImages(model, error)`（`{api, provider, model: id, output: [],
stopReason:"error", errorMessage, timestamp}`）。
`registerBuiltInImagesApiProviders()`：注册 `{api: "openrouter-images", generateImages: generateImagesOpenRouter}`。
**模块加载时立即调用一次**（导入即有副作用）。

#### 2.3.15 `providers/anthropic.ts` 的 `anthropicApiKeyAuth()`（原稿遗漏）

`anthropicApiKeyAuth(): ApiKeyAuth`，`name: "Anthropic API key"`。

`login(interaction)`：`interaction.signal.throwIfAborted()`；`prompt({type:"secret", message:"Enter Anthropic API key"})`；
再 `throwIfAborted()`；返回 `{type:"api_key", key}`。

`resolve({ctx, credential, signal})`（**每一步之间都 `signal.throwIfAborted()`**，顺序不可调换）：

1. `credential?.key` 存在 -> `{auth:{apiKey: credential.key}, env: credential.env, source: "stored credential"}`。
2. `ctx.env("ANTHROPIC_AUTH_TOKEN")` 存在 -> `{auth:{headers:{Authorization: \`Bearer ${authToken}\`}}, source: "ANTHROPIC_AUTH_TOKEN"}`
   （**注意：不设 apiKey，只设 Authorization 头；也没有 `env` 字段**）。
3. 依次尝试 `ANTHROPIC_OAUTH_TOKEN`、`ANTHROPIC_API_KEY`：有值 -> `{auth:{apiKey}, source: envVar}`。
4. **Workload identity federation**（最后兜底，注释明确"keys and `ANTHROPIC_AUTH_TOKEN` keep winning, as in the SDK"）：
   - 三个**必需**变量全部存在才继续，任一为空立即 `return undefined`：
     `ANTHROPIC_FEDERATION_RULE_ID`、`ANTHROPIC_ORGANIZATION_ID`、`ANTHROPIC_IDENTITY_TOKEN_FILE`；
     这三项写入 `federation`（`ProviderEnv`）。
   - 两个**可选**变量：`ANTHROPIC_SERVICE_ACCOUNT_ID`、`ANTHROPIC_WORKSPACE_ID`，有值才写入 `federation`。
   - 返回 `{ auth: {}, env: federation, source: "workload identity federation" }`（注释：这些 id 是 provider 配置而非
     auth，因此放在 `env` 里；SDK 自己用身份令牌换取短期 access token 并自行刷新）。
5. 全部不满足 -> `undefined`。

对应的测试：`test/anthropic-auth-token.test.ts`、`test/anthropic-federation.test.ts`、
`test/anthropic-federation-sdk.test.ts`（原稿 §7.1 未列出，见 Audit additions）。

### 2.4 `packages/ai/scripts/**`

**只有 4 个脚本**以 `#!/usr/bin/env node` 开头（`check-model-data.ts`、`generate-models.ts`、`generate-test-image.ts`、
`hydrate-model-catalog.ts`）；其余 5 个是可被导入的模块（`model-data.ts`、`openrouter-catalog.ts`、
`ai-gateway-pricing.ts`、`models-dev-reasoning-options.ts`、`openrouter-reasoning-options.ts`）**没有 shebang**。
package.json 里全部通过 `node scripts/<name>.ts` 直接运行（Node ≥ 22.19 原生类型剥离，无需 `--experimental-strip-types` 标志）。
Rust 侧这些脚本**不是运行时依赖**：Rust 移植只需 (a) 生成器可在需要时重跑以更新数据，(b) `check-model-data`
等价校验可作为 CI 门禁。**但生成的 JSON 数据（`src/providers/data/*.json`）必须与 TS 生成器逐字节一致**，
因为 Rust 会读取同一份数据（或由它编译期嵌入）。

#### 2.4.1 `scripts/model-data.ts`（336 行）——数据目录的 schema、哈希与校验

```ts
export const MODEL_DATA_SCHEMA_VERSION = 6;
export const MODEL_DATA_MANIFEST_FILE = ".manifest.json";
export type ModelDataStructure = Record<string, Record<string, string>>;   // provider -> "type:id" -> api
export interface ModelDataManifest { schemaVersion: number; generatedAt: string; structureHash: string;
                                     files: Record<string, string> }
export function assertExactModelIds(label, expected: Iterable<string>, actual: Iterable<string>): void
export function readModelDataProviderIds(packageRoot): string[]
export function readModelDataStructure(packageRoot): ModelDataStructure
export function modelDataStructureHash(structure): string
export function createModelDataManifest(structure, fileContents, generatedAt): ModelDataManifest
export function validateModelDataDirectory(structure, dataDir): void
export function validateGeneratedModelData(packageRoot): void
export interface ModelCatalogEntry { type: string; id: string; api: string }
export function groupProviderModelData<T extends ModelCatalogEntry>(providerId, models):
  { groups: Record<string, Record<string, T>>; structure: Record<string, string> }
```
- `MODEL_DATA_IMPORT_PATTERN`（`gm`）：`/^import \{ [A-Z][A-Z0-9_]*_CLASSIFIER_MODELS, [A-Z][A-Z0-9_]*_IMAGE_MODELS, [A-Z][A-Z0-9_]*_MODELS \} from "\.\/providers\/([^"/]+)\.models\.ts";$/gm`
  —— 用于从 `src/models.generated.ts` 提取 provider id 列表。
- `sha256(value)`：`createHash("sha256").update(value).digest("hex")`（UTF-8 输入）。
- `sortedRecord(entries)`：按**码元序**升序（`a < b ? -1 : a > b ? 1 : 0`；**不是** `localeCompare`）排序后 `Object.fromEntries`。
- `sameStrings(a,b)`、`describeSetDifference(expected, actual)` ->
  `` [missing ? `missing: ${...join(", ")}` : "", extra ? `extra: ${...}` : ""].filter(Boolean).join("; ") ``。
- `assertExactModelIds`：去重+排序后比较，不等抛
  `` `${label} model IDs do not match (${describeSetDifference(...)})` ``。
- `readProviderStructure(path, providerId)`：读 JSON 对象；每个 api 组必须是对象，组内键跨组不得重复
  （`contains ${modelKey} in more than one API group`）；空 -> `contains no generated model data`；返回排序后的 map。
- `readModelDataProviderIds(packageRoot)`：读 `src/models.generated.ts` 文本，正则提取、排序；空 -> `No generated provider imports found in ${path}`；
  重复 -> `Generated model aggregator contains duplicate provider imports: ${path}`。
- `readModelDataStructure`：provider id 与 `providers/*.models.ts` 文件名集合必须一致（否则
  `Generated model aggregator and provider shards do not match (${diff})`）；再逐个读 `data/<id>.json`。
- `modelDataStructureHash`：对 provider 与模型键**双层排序**后 `JSON.stringify`（无缩进），取 sha256。
- `createModelDataManifest`：`{schemaVersion: 6, generatedAt, structureHash, files: sortedRecord(每个文件 -> sha256(内容))}`。
- `validateModelValue(value, providerId, modelId, expectedApi, errors)`：错误文本逐字：
  `${label} must be an object`；`${label} has id ${JSON.stringify(value.id)}, expected ${JSON.stringify(modelId)}`；
  `has provider ... expected ...`；`has api ... expected ...`；`has no model name`；`has no baseUrl string`；
  `has invalid input modalities`；image 类型要求 output 为模态列表且含 image，否则 `has invalid output modalities`；
  非 image 但存在 `output` -> `has unsupported output modalities`；
  chat 要求 `reasoning` boolean、`contextWindow` 有限且 > 0、`maxTokens` 有限且 > 0（`has no reasoning boolean` /
  `has invalid contextWindow` / `has invalid maxTokens`）；classifier 只要求 contextWindow；
  其他 type -> `has type ${JSON.stringify(value.type)}, expected "chat", "image", or "classifier"`；
  cost 必须为对象且四个费率都是有限 number（`has invalid cost metadata` / `has invalid cost.<field>`）。
  `isModalityList`：非空数组且每项为 `"text"` 或 `"image"`。
- `throwValidationErrors(errors)`：只显示**前 30 条**，多余追加 `\n  ... and ${n} more`；最终
  `Error(\`Invalid generated model data:\n${每行 "  - ..."}\`)`。
- `validateModelDataDirectory(structure, dataDir)`：目录不存在 -> `Generated model data directory does not exist: ${dataDir}`；
  校验：文件集合（排除 `.manifest.json`）、manifest 的 schemaVersion/generatedAt(可 `Date.parse`)/structureHash、
  manifest `files` 键集合、每个文件的 sha256、每个文件的 JSON 结构（api 组对象、`type:id` 唯一、模型值合法、
  键必须等于 `${type}:${id}`，否则 `${provider}/${key} has mismatched type/id identity`）、
  模型 id 集合与 api 归属。
- `validateGeneratedModelData(packageRoot)`：两者串联。
- `groupProviderModelData(providerId, models)`：按 `Array.from(new Set(apis)).sort()` 的 api 顺序建组；
  组内按**输入数组顺序**插入 `type:id` 键；重复 -> `Error(\`${providerId}/${identity} has duplicate ${api} catalog entries\`)`。
  `structure[identity] = api`。

#### 2.4.2 `scripts/check-model-data.ts`（16 行）

`validateGeneratedModelData(packageRoot)`；成功打印 `Generated model data is valid.`；
失败打印 `error.message`（stderr），再打印空行 +
`Model data is missing or stale. Run \`npm run hydrate:model-data\` from the repository root.`，`process.exitCode = 1`。

#### 2.4.3 `scripts/hydrate-model-catalog.ts`（86 行）

```ts
export function hydrateModelCatalog(packageRoot, catalogPath, options?: {validateOnly?: boolean}): void
```
- CLI：`node hydrate-model-catalog.ts <models.all.json>`；参数个数不是 3 ->
  `Error("Usage: node hydrate-model-catalog.ts <models.all.json>")`。
- 读 catalog（必须是对象，否则 `Model catalog must be an object`）。
- 对 `readModelDataProviderIds(packageRoot)` 的每个 provider：catalog 中缺失 ->
  `Model catalog is missing provider: ${provider}`；非数组或空 ->
  `Model catalog has no typed model list for provider: ${provider}`；条目非法 ->
  `Model catalog has an invalid entry for provider ${provider}`（`isCatalogEntry`：对象且 type/id/api 为 string，api 非空）。
  `groupProviderModelData` 后，文件内容为 `` `${JSON.stringify(groups)}\n` ``（**紧凑、无缩进、尾随换行**）。
- manifest 的 `generatedAt` 固定为 `"1970-01-01T00:00:00.000Z"`（注释：公开目录无生成时间，固定戳保证同输入同字节）。
- 在 `src/providers/.model-hydration-XXXXXX/` 暂存，写文件、写 manifest
  （`` `${JSON.stringify(manifest)}\n` ``），`validateModelDataDirectory(structure, stagedData)`；
  `validateOnly` 则直接返回（finally 清理暂存）；否则删除现有 `data/` 并 `rename` 暂存目录过去。

#### 2.4.4 `scripts/openrouter-catalog.ts`（107 行）

```ts
export interface OpenRouterModelListItem extends OpenRouterModel { name: string; supported_parameters?: string[];
  architecture?: { modality?: string; input_modalities?: string[]; output_modalities?: string[] };
  reasoning?: OpenRouterReasoningMetadata }
export interface OpenRouterCatalog { chat: Model<"anthropic-messages"|"openai-completions">[];
  images: ImageModel<"openrouter-images">[]; classifiers: ClassifierModel<"typesafe-system-one">[] }
export function buildOpenRouterCatalog(listed, imageListed, decisionListed): OpenRouterCatalog
```
- `modalities(values)`：过滤只保留 `"text"`/`"image"` 后去重（保留首次出现顺序）。
- chat：仅 `supported_parameters` 含 `"tools"`；`input` 初始 `["text"]`，`architecture.modality` 含 `"image"` 时加 `"image"`；
  `useAnthropicMessages = /^anthropic\//.test(id) && !id.endsWith(":batch")`；
  `api` 为 `anthropic-messages` 或 `openai-completions`；`baseUrl` 分别为 `https://openrouter.ai/api` 或 `.../api/v1`；
  `reasoning = supported_parameters.includes("reasoning") || false`；`thinkingLevelMap` 由 `getOpenRouterThinkingLevelMap` 得；
  `cost = openRouterCost(pricing)`；`contextWindow = top_provider.context_length || context_length || 4096`；
  `maxTokens = top_provider.max_completion_tokens || 4096`。
- images：按 id 去重（先到先得）；`output` 必须含 `"image"`；input 为空则 `["text"]`；
  `api: "openrouter-images"`、`baseUrl: "https://openrouter.ai/api/v1"`。
- classifiers：按 id 去重；`output_modalities` 必须含 `"decisions"`；`api: "typesafe-system-one"`、
  `baseUrl: "https://openrouter.ai/api/v1"`、`contextWindow: top_provider.context_length || context_length || 4096`、
  `compat: { imageInput: "state-image-urls", sendImageOptInHeader: false }`（注释：OpenRouter 对 TypeSafe `assets`
  返回 200 但丢弃图片）。

#### 2.4.5 `scripts/ai-gateway-pricing.ts`（67 行）

```ts
export interface AiGatewayPriceTier { cost?: string|number; min?: number; max?: number }
export interface AiGatewayPricing { input?; output?; input_cache_read?; input_cache_write?;
  input_tiers?: AiGatewayPriceTier[]; output_tiers?; input_cache_read_tiers?; input_cache_write_tiers? }
export function getAiGatewayCost(pricing): ModelCost
```
`perMillion(v)`：number 直接用，否则 `parseFloat(v ?? "0")`；有限 -> `Number((v * 1e6).toFixed(6))`，否则 0。
`RATE_FIELDS`：`input<->input_tiers`、`output<->output_tiers`、`cacheRead<->input_cache_read_tiers`、
`cacheWrite<->input_cache_write_tiers`。
`starts` = 所有 tiers 中 `min !== undefined && min > 0` 的取值集合；按数值升序；
每个 start 生成 tier `{inputTokensAbove: start - 1, ...base}`，并对每个 rate 找**第一个**满足
`(min ?? 0) <= start && (max === undefined || start < max)` 的 bracket，`cost !== undefined` 时覆盖该 rate。
tiers 非空 -> `{...base, tiers}`，否则 base。

#### 2.4.6 `scripts/models-dev-reasoning-options.ts`（30 行）

```ts
export type ModelsDevReasoningOption = {type:"toggle"} | {type:"effort"; values: Array<"none"|"minimal"|"low"|"medium"|"high"|"xhigh"|"max"|"default"|null>} | {type:"budget_tokens"; min?; max?}
export function getEffortThinkingLevelMap(options): ThinkingLevelMap | undefined
```
`THINKING_LEVELS = ["minimal","low","medium","high","xhigh","max"]`。收集所有 effort 值；空 -> undefined；
`supported` 集合既不含任何 THINKING_LEVELS 也不含 `"none"` -> undefined；
`map = {off: supported.has("none") ? "none" : null}`；每个 level -> `supported.has(level) ? level : null`。
（`default` 与 JSON `null` 被有意忽略。）

#### 2.4.7 `scripts/openrouter-reasoning-options.ts`（23 行）

```ts
export interface OpenRouterReasoningMetadata { mandatory?: boolean; default_enabled?: boolean;
  supported_efforts?: Array<ThinkingLevel | "none">; default_effort?: ThinkingLevel | "none" }
export function getOpenRouterThinkingLevelMap(reasoning): ThinkingLevelMap | undefined
```
无 reasoning -> undefined；`supported_efforts` 为空/缺失 -> `mandatory === true ? {off: null} : undefined`；
否则用 `getEffortThinkingLevelMap([{type:"effort", values: supported_efforts}])`，为 undefined 时同样回退
`{off: null}`（mandatory）或 undefined；成功时返回 `{...map, off: reasoning.mandatory === true ? null : "none"}`。

#### 2.4.8 `scripts/generate-test-image.ts`（32 行，**文件末尾无换行**，故 `wc -l` 报 32 但实际 33 行内容）

用 `canvas` 生成 200x200 PNG：白底、居中红圆（圆心 100,100，半径 50，`0..2π`），写入
`test/data/red-circle.png`，打印 `Generated test image at: ${outputPath}`。
（Rust 侧不需要该脚本；测试资产 `test/data/red-circle.png` 直接使用仓库内文件。）

#### 2.4.9 `scripts/generate-models.ts`（3642 行）——目录生成器

**CLI（`readGeneratorOptions`）**：`--strict`、`--data-only`、`--json-only`、`--pretty`、`--json-output <dir>`；
未知参数 -> `Error(\`Unknown argument: ${arg}\`)`；`--json-output` 缺值 -> `Error("--json-output requires a directory")`；
`--json-only` 无 `--json-output` -> `Error("--json-only requires --json-output")`；
`--data-only` 与任一 JSON 输出选项同用 -> `Error("--data-only cannot be combined with JSON catalog output")`。
`jsonOutputDir` 经 `resolve()` 绝对化。

**数据源（网络）**：
- models.dev：`https://models.dev/api.json`（工具能力模型与 classifier 模型两次读取）。
- OpenRouter：`https://openrouter.ai/api/v1/models`、`?output_modalities=image`、`?output_modalities=decisions`。
- Vercel AI Gateway：`https://ai-gateway.vercel.sh/v1/models`。
- Radius：`loadRadiusGatewayConfig("https://radius.kiana.dev")`（`/v1/config`）。
- NVIDIA NIM：`https://integrate.api.nvidia.com/v1/models`（仅用于模型 id 归一化，`normalizeNvidiaModelId`：
  小写 + `_` -> `.`）。
所有 fetch 失败：`console.error("Failed to fetch ...:", error)` 后**非 strict 时降级**（空目录/空数组）；
strict 时重新抛出。Radius 得到 0 个模型也抛 `Error("Radius API returned no models")`；
`--strict` 且 OpenRouter 图像模型为 0 -> `Error("OpenRouter API returned no usable image models")`。

**合并顺序**：`[...modelsDevModels, ...openRouterCatalog.chat, ...aiGatewayCatalog.chat, ...radiusModels]`
（注释：重叠以 models.dev 优先），再过滤掉 `xai` 的 `XAI_BUILTIN_EXCLUDED_MODEL_IDS`、
`opencode`/`opencode-go` 的 `gpt-5.3-codex-spark`。

**重要常量与特例（逐字）**：
- `KIMI_K3_MAX_TOKENS = 131072`（原规格中被写成 `MAX_...` 并被标注为“遮盖”，实际源码标识符就是 `KIMI_K3_MAX_TOKENS`，见 `scripts/generate-models.ts:329`，在 2922 行用于 `candidate.maxTokens = KIMI_K3_MAX_TOKENS`）。
- `KIMI_K3_COST = {input:3, output:15, cacheRead:0.3, cacheWrite:0}`。
- `KIMI_OPENAI_COMPAT`：`supportsStore:false, supportsDeveloperRole:false, supportsReasoningEffort:false,
  maxTokensField:"max_tokens", supportsStrictMode:false, thinkingFormat:"deepseek"`。
- `KIMI_K3_OPENAI_COMPAT = {...KIMI_OPENAI_COMPAT, requiresReasoningContentOnAssistantMessages:true,
  thinkingFormat:"openai", supportsReasoningEffort:true}`。
- `isKimiK3FamilyId(id) = /^kimi-k3(?:\.\d+)?(?:-|$)/`。
- `ANT_LING_RING_THINKING_LEVEL_MAP = {off:null, minimal:null, low:null, medium:null, high:"high", xhigh:"xhigh"}`。
- `BEDROCK_INFERENCE_PROFILE_ONLY_MODEL_IDS = {"anthropic.claude-opus-5"}`。
- `MODELS_DEV_OPENAI_UNSUPPORTED_MODEL_IDS = {"gpt-5.6"}`。
- `OPENAI_TOOL_SEARCH_MODEL_IDS = {gpt-5.4, gpt-5.4-mini, gpt-5.4-pro, gpt-5.5, gpt-5.6-sol, gpt-5.6-terra,
  gpt-5.6-luna, gpt-6-astra, gpt-6-sol, gpt-6-luna, gpt-6.1-sol}`；`OPENAI_ADDITIONAL_TOOLS_MODEL_IDS` 与
  `OPENAI_MID_CONVO_SYSTEM_MESSAGE_MODEL_IDS` 同集合。
- `OPENAI_CODEX_ADDITIONAL_TOOLS_MODEL_IDS = {gpt-5.6-sol, gpt-5.6-terra, gpt-5.6-luna, gpt-6-astra, gpt-6-sol,
  gpt-6-luna, gpt-6.1-sol}`；`OPENAI_MID_CONVO_EFFORT_MODEL_IDS = {gpt-6-astra, gpt-6-sol, gpt-6-luna, gpt-6.1-sol}`。
- `OPENAI_LONG_CONTEXT_INPUT_THRESHOLD = 272000`；`OPENAI_SHORT_CONTEXT_CAPPED_MODEL_IDS`、
  `OPENAI_LONG_CONTEXT_PRICING_MODEL_IDS`（见源文件集合）。
- `DEFAULT_IMAGE_RESIZE = {maxWidth: 2000, maxHeight: 2000, maxBytes: 4.5*1024*1024, jpegQuality: 80}`。
- `OPENAI_STANDARD_COSTS`（逐字，$/1M）：`gpt-5.6-luna {0.2,1.2,0.02,0.25}`、`gpt-5.6-sol {4,20,0.4,5}`、
  `gpt-5.6-terra {2,12,0.2,2.5}`、`gpt-6-astra {10,50,1,12.5}`、`gpt-6-luna {0.1,0.5,0.01,0.125}`、
  `gpt-6-sol {2,10,0.2,2.5}`、`gpt-6.1-sol {2,10,0.1,2.5}`。
- `withOpenAiLongContextPricing(cost)`：`tiers: [{inputTokensAbove: 272000, input: roundCost(input*2),
  output: roundCost(output*1.5), cacheRead: roundCost(cacheRead*2), cacheWrite: roundCost(cacheWrite*2)}]`。
- `roundCost(v) = Number(v.toFixed(6))`。
- `getModelsDevCost(cost)`：base 四费率（`|| 0`）；tiers 仅保留 `tier.tier?.type === "context" && size !== undefined`
  的项，`inputTokensAbove: size`，缺失费率回落 base。
- CLI 常量：`TOGETHER_BASE_URL = "https://api.together.ai/v1"`、`AI_GATEWAY_MODELS_URL = "https://ai-gateway.vercel.sh/v1"`、
  `AI_GATEWAY_BASE_URL = "https://ai-gateway.vercel.sh"`、`AI_GATEWAY_TYPESAFE_BASE_URL =
  "https://ai-gateway.vercel.sh/typesafe/v1"`、`VERTEX_BASE_URL = "https://{location}-aiplatform.googleapis.com"`、
  `NVIDIA_BASE_URL = "https://integrate.api.nvidia.com/v1"`；Bedrock 区域 baseUrl
  `eu-central-1` 或 `us-east-1`（`https://bedrock-runtime.<region>.amazonaws.com`）。
- Z.AI 变体：`zai-coding-plan -> provider "zai", baseUrl "https://api.z.ai/api/coding/paas/v4"`；
  `zhipuai-coding-plan -> provider "zai-coding-cn", baseUrl "https://open.bigmodel.cn/api/coding/paas/v4"`；
  `tool_call !== true` 跳过；`glm-5.2`/`glm-5.2-highspeed` 的 thinkingLevelMap `off = "none"`；
  参考成本取 `data.zai?.models[modelId]?.cost ?? m.cost`。
- AI Gateway evaluation 模型 -> classifier（`api: "typesafe-system-one"`, `provider: "vercel-ai-gateway"`,
  `baseUrl: "https://ai-gateway.vercel.sh/typesafe/v1"`, `input: ["text"]`，cost 由 `pricing * 1e6` 四舍五入 6 位，
  cache 0，`contextWindow: context_window || 4096`）；其余要求 `tags` 含 `"tool-use"`；`"vision"` 加 image；
  chat 条目 `api: "anthropic-messages"`、`baseUrl: "https://ai-gateway.vercel.sh"`、
  `compat: {allowEmptySignature: true}`、`cost: getAiGatewayCost(pricing)`、`contextWindow: context_window || 4096`、
  `maxTokens: max_tokens || 4096`。
- Qwen Token Plan：`QWEN_TOKEN_PLAN_REASONING_EFFORT_FALLBACK_MODEL_IDS = {glm-5, glm-5.1}`；
  `QWEN_TOKEN_PLAN_EXCLUDED_MODEL_IDS = {qwen3.8-max-preview}`；
  `QWEN_TOKEN_PLAN_PROVIDER_IDS = {qwen-token-plan, qwen-token-plan-cn, qwen-token-plan-individual}`；
  `QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS = {deepseek-v4-flash-0731, deepseek-v4-pro, deepseek-v4-pro-0813,
  glm-5.2, qwen3.6-flash, qwen3.7-max, qwen3.7-plus, qwen3.8-flash, qwen3.8-max}`。
- 缺失的 Copilot 模型（models.dev 尚未收录）手工补充：`claude-opus-5.5`（anthropic-messages，reasoning，
  thinkingLevelMap `{off:null,minimal:null,low:"low",medium:"medium",high:"high",xhigh:"xhigh",max:"max"}`，
  cost `{4,20,0.2,5}`，contextWindow 1000000，maxTokens 128000，带 `COPILOT_STATIC_HEADERS`）与
  `gpt-6-sol`/`gpt-6-luna`（openai-responses，名称 `GPT-6 Sol`/`GPT-6 Luna`）。

**compat 元数据应用器**（按 provider/api 计算 `Model.compat`）：`detectOpenAICompletionsCompat`、
`mergeOpenAICompletionsCompat`、`applyOpenAICompletionsCompatMetadata`、`mergeAnthropicMessagesCompat`、
`applyAnthropicMessagesCompatMetadata`、`applyModelsDevReasoningOptionMetadata`、`recordModelsDevReasoningOptions`、
`supportsDirectReasoningEffort`、`mergeThinkingLevelMap`、`getTogetherCompat`、`getTogetherThinkingLevelMap`、
`supportsOpenAiXhigh`、`supportsOpenAiMax`、`supportsAnthropicMidConvoEffort`、
`supportsAnthropicDisabledThinkingWithMidConvoEffort`、`supportsAnthropicMidConvoSystemMessages`、
`isAnthropicAdaptiveThinkingModel`、`isAnthropicTemperatureUnsupportedModel`、`isAnthropicFallbackMetadataModel`、
`isPlainEmptyObject`、`openAICompletionsCompatDelta`、`getModelKey`（`` `${provider}:${id}` ``）。
规则要点（详见源码 518–1100 行）：
- `supportsOpenAiXhigh`/`supportsOpenAiMax` 按模型 id 白名单/前缀判断。
- Anthropic：`forceAdaptiveThinking`（Opus 4.7+ 等）、`supportsTemperature`（`isAnthropicTemperatureUnsupportedModel`
  为假时才 true）、`supportsMidConvoEffort`、`supportsMidConvoSystemMessages`、`allowedFallbackModels`。
- Together：`thinkingFormat: "together"` + thinking level map 仅 `high`/`max`。
- `MODELS_DEV_OPENAI_UNSUPPORTED_MODEL_IDS` 中的模型不取自 models.dev 的 OpenAI 段。

**手工硬编码的 classifier 目录（不来自网络，必须逐字生成）**：
- `OPENCODE_CLASSIFIER_MODELS`（provider `opencode`，api `typesafe-system-one`，baseUrl `https://opencode.ai/inference/v1`，
  input `["text"]`，contextWindow 32000）：`jev-1.13`/`Jev 1.13`（cost input 0.042，其余 0）、
  `jev-1.13-free`/`Jev 1.13 Free`（cost 全 0）。
- `CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS`（provider `cloudflare-workers-ai`，api `cloudflare-workers-ai-system-one`，
  baseUrl `CLOUDFLARE_WORKERS_AI_REST_BASE_URL`）：`@cf/cloudflare/clef`/`Clef`（input text+image，cost 0.24，
  ctx 65536，`compat.imageInput:"images"`）、`@cf/cloudflare/clef-flash`/`Clef Flash`（0.038，24576，`images`）、
  `@cf/cloudflare/clef-omni`/`Clef Omni`（0.15，64000，`images`）、`typesafe/jev`/`Jev`（0.042，32000，`assets`）、
  `typesafe/jev-1.14.0`/`Jev 1.14`（0.042，32000，`assets`）。
- `OPENAI_CLASSIFIER_MODELS`（provider `openai`，api `openai-decisions`，baseUrl `https://api.openai.com/v1`）：
  `gpt-6-luna`/`GPT-6 Luna`（input text+image，cost `withOpenAiLongContextPricing({input:0.1, output:0, cacheRead:0,
  cacheWrite:0})`，contextWindow **922000**）。
- `COPILOT_STATIC_HEADERS = {"User-Agent":"GitHubCopilotChat/0.35.0", "Editor-Version":"vscode/1.107.0",
  "Editor-Plugin-Version":"copilot-chat/0.35.0", "Copilot-Integration-Id":"vscode-chat"}`。
- `XAI_BUILTIN_EXCLUDED_MODEL_IDS = {grok-3, grok-3-fast, grok-4.20-0309-non-reasoning, grok-4.20-0309-reasoning,
  grok-build-0.1, grok-code-fast-1}`。
- `OPENCODE_OPENAI_COMPLETIONS_LONG_CACHE_RETENTION_UNSUPPORTED_MODELS = {opencode:deepseek-v4-flash,
  opencode:deepseek-v4-pro, opencode:kimi-k2.5, opencode:kimi-k2.6, opencode:minimax-m2.7, opencode-go:kimi-k2.6}`。
- `GITHUB_COPILOT_EXTENDED_CONTEXT_MODELS`（100 万上下文白名单，含 `claude-fable-5`、`claude-opus-4.8`、
  `claude-opus-5` 等，完整列表见源码 495–510 行）。
- `TOGETHER_BASE_COMPAT: OpenAICompletionsCompat = {supportsStore:false, supportsDeveloperRole:false,
  supportsReasoningEffort:false, maxTokensField:"max_tokens", ...}`（完整字段见源码 174 行起）。
- `XAI_RESPONSES_COMPAT: OpenAIResponsesCompat = {supportsLongCacheRetention:false}`。

**chat 模型写入规则（逐字）**：
`providers[model.provider] ??= {chat:{}, image:{}, classifier:{}}; providers[model.provider].chat[model.id] ??= {...model, type: "chat"};`
即 **models.dev 优先**（`??=` 不覆盖已有 id）；image 与 classifier 同理分别来自各自的源（`openRouterCatalog.images`、
`[...modelsDevClassifierModels, ...openRouterCatalog.classifiers, ...aiGatewayCatalog.classifiers,
...OPENCODE_CLASSIFIER_MODELS, ...CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS, ...OPENAI_CLASSIFIER_MODELS]`）。
`sortedProviderIds = Object.keys(providers).sort()`（**码元序**）。

**输出阶段（顺序）**：
1. 计算 `providers[providerId].chat/image/classifier` 三个 keyed 对象（键顺序按 **`localeCompare`** 排序——
   注意与 `model-data.ts` 的码元序排序**不同**，JSON 输出的键序依赖 locale）。
2. `jsonAllProviders[providerId] = [chat..., image..., classifier...]`（数组，允许同一 id 每 type 一次）。
3. `serializeJson = JSON.stringify(value, null, pretty ? 2 : undefined) + "\n"`（`--pretty` 控制缩进）。
4. `--data-only` 时只对 `readModelDataProviderIds()` 的 provider 生成；缺失 ->
   `Error(\`Cannot hydrate missing providers: ${ids}\`)`。
5. 非 `jsonOnly`：暂存 `src/providers/.model-generation-XXXXXX/data`，写每个 `<provider>.json`（按 api 分组、`type:id` 键）
   与 `.manifest.json`，`validateModelDataDirectory` 通过后再原子替换 `data/`；失败时恢复（含回滚 `.models.ts` 与
   `models.generated.ts`）。
6. 非 `dataOnly` 且非 `jsonOnly`：重写 42 个 `providers/<provider>.models.ts`（模板见 2.3.3）与
   `src/models.generated.ts`（模板见下），并删除多余分片。
7. `--json-output`：删除目标目录后写
   `models.json`（chat，keyed）、`models.all.json`（全类型，数组）、`providers.json`（provider id 数组，排序后）、
   `providers/<id>.json`、`providers/<id>.all.json`。
8. 统计输出（stdout）：
```
Generated provider catalogs and src/models.generated.ts
Generated JSON model values under src/providers/data/          (或 data-only 时的 "Hydrated JSON model values under src/providers/data/")
Generated JSON model catalog under <dir>

Model Statistics:
  Total tool-capable models: <n>
  Reasoning-capable models: <n>
  <provider>: <c> chat models, <i> image models, <k> classifier models
```
（每个 provider 一行，按 providers 对象顺序。）
9. `generateModels().catch(e => { console.error(e); process.exitCode = 1; })`。

**`models.generated.ts` 生成模板（逐字）**：
```
// This file is auto-generated by scripts/generate-models.ts
// Do not edit manually - run 'npm run generate-models' to update

```
随后每个 sorted provider 一行 `import { <P>_CLASSIFIER_MODELS, <P>_IMAGE_MODELS, <P>_MODELS } from "./providers/<id>.models.ts";`，
空行，然后三段 `export const MODELS|IMAGE_MODELS|CLASSIFIER_MODELS: { readonly "<id>": typeof ...; ... } = { "<id>": ..., ... };`。
常量名：`` `${providerId.toUpperCase().replace(/[^A-Z0-9]+/g, "_")}_MODELS` ``（`_IMAGE_MODELS`/`_CLASSIFIER_MODELS` 同）。
separator 均为 `\t`，每行以 `\n` 结尾，文件以 `};\n` 结束。

#### 2.4.10 仓库根 `scripts/model-catalog-protocol.ts`（260 行）——目录分发协议

> 该文件在仓库根 `scripts/`，**逐字复制**到 `earendil-works/kiana` 与 `earendil-works/kiana.dev`
> (`src/shared/models/protocol.ts`)，两份必须字节一致。它被 kiana 用于发布修订与测试选择算法，也被 kiana.dev 用于服务请求。
> 必须**无 import、无运行时特定 API**（Node 与 Cloudflare Workers 均可运行）。

**存储布局（`MODEL_CATALOG_PREFIX` 之下）**：
```
index.json                                    revision 索引（ModelCatalogIndex）
revisions/<revision>/models.json              chat 模型，按 provider 与 id
revisions/<revision>/models.all.json          全类型模型，按 provider
revisions/<revision>/providers.json           排序后的 provider id
revisions/<revision>/providers/<id>.json      单个 provider 的 chat 模型
revisions/<revision>/providers/<id>.all.json  单个 provider 的全类型模型
```
带 `?types=` 的客户端得到 typed 的 `.all.json` 变体；旧客户端得到 legacy 的 chat-only 变体。

**常量与正则（逐字）**：
```ts
export const MODEL_CATALOG_SCHEMA_VERSION = 1;
export const MODEL_CATALOG_PREFIX = `models/v${MODEL_CATALOG_SCHEMA_VERSION}`;   // "models/v1"
export const MODEL_CATALOG_INDEX_KEY = `${MODEL_CATALOG_PREFIX}/index.json`;
export const MODEL_CATALOG_REVISION_RE = /^sha256-[0-9a-f]{64}$/;
const KIANA_VERSION_RE = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/;
const KIANA_USER_AGENT_RE = /^kiana\/([^\s()]+)(?: \([^;()]+(?:;\s*[^;()]+(?:;\s*[^()]+)?)?\))?$/i;
const MODEL_TYPE_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
type ParsedKianaVersion = readonly [major: number, minor: number, patch: number, prerelease: string];
```

**类型**：
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

**函数**：
- `getModelCatalogArtifactKey(revision, artifact)` -> `` `${MODEL_CATALOG_PREFIX}/revisions/${revision}/${artifact}` ``。
- `getModelCatalogProviderKey(revision, provider, representation)`：suffix 为 `typed ? ".all.json" : ".json"` ->
  `` `${MODEL_CATALOG_PREFIX}/revisions/${revision}/providers/${provider}${suffix}` ``。
- `isValidModelCatalogKianaVersion(version)`：`parseKianaVersion(version) !== undefined`。
- `compareModelCatalogKianaVersions(left, right)`：`compareKianaVersions(requireKianaVersion(left), requireKianaVersion(right))`；
  非法版本抛 `Error(\`Invalid Kiana version: ${version}\`)`。
- `parseModelCatalogRepresentation(types)`：`undefined`/`null` -> `"legacy"`；空串或按 `,` 拆分后任一段不匹配
  `MODEL_TYPE_RE` -> `undefined`（**非法**）；否则 `"typed"`。（注释：不发送该参数的客户端早于模型类型。）
- `parseModelCatalogRequest(url, userAgent): ModelCatalogRequest`：
  1. `new URL(url)`；`representation` 非法 -> `{kind:"invalid", error:"Invalid model types."}`。
  2. 无 `kiana-version` 参数：从 UA 用 `KIANA_USER_AGENT_RE` 提取版本；有效则**写回** `kiana-version` 参数并返回
     `{kind:"redirect", location: requestUrl.toString()}`；否则 `{kind:"catalog", kianaVersion: undefined, representation}`。
     （注释：已发布客户端不发 `kiana-version`；响应按 URL 缓存，所以用重定向而不是按 UA 变化响应。）
  3. 有但非法 -> `{kind:"invalid", error:"Invalid Kiana version."}`；否则 catalog。
- `selectModelCatalog(index, kianaVersion): ModelCatalogIndexEntry | undefined`：
  无版本 -> `index.catalogs.find(c => c.revision === index.defaultRevision)`；
  解析失败 -> undefined；否则遍历，取满足 `minimumVersion <= requested` 且**最小版本最高**的条目
  （`compareKianaVersions(minimumVersion, selectedVersion) > 0` 时替换）。
- `parseModelCatalogIndex(value): ModelCatalogIndex`：校验 `schemaVersion === 1`、`defaultRevision` 匹配
  `MODEL_CATALOG_REVISION_RE`、`catalogs` 非空数组、每项 `minimumKianaVersion` 可解析且 `revision` 匹配正则、
  且至少一项 revision 等于 defaultRevision；否则抛
  `Error(\`Model catalog index is invalid: ${MODEL_CATALOG_INDEX_KEY}\`)`。返回时**只保留协议字段**
  （丢弃发布方可能附加的元数据）。
- `parseKianaVersion(version)`：`version.trim()` 匹配 `KIANA_VERSION_RE`；三段 `Number(...)` 均为
  `Number.isSafeInteger` 才有效；prerelease 缺失时为 `""`。
- `compareKianaVersions(left, right)`：先比 major/minor/patch；prerelease 相等 -> 0；
  左为空（正式版）-> **1**（正式版更高）；右为空 -> -1；否则按 `.` 拆分逐段：长度不足的一方 -1/1；
  相等继续；两段都是数字（`/^\d+$/`）-> 数值差；仅左数字 -> -1；仅右数字 -> 1；
  否则 `leftPart.localeCompare(rightPart)`。

**Rust 移植注意**：`localeCompare` 在 prerelease 文本比较中使用**运行时区域设置**（Node 默认 ICU）。
Rust 需固定为与 V8 默认 ICU 一致的排序（`str::cmp` 字节序在 ASCII 字母数字上通常等价，但大小写/标点顺序不同），
建议在测试中覆盖混合大小写与 `-`/`.` 的 prerelease，并对齐 Node 默认 `en-US` 的 ICU 顺序。

---

### 2.5 交叉引用：本单元内文件之间的依赖顺序（移植顺序建议）

Rust 移植应按下述层次自底向上，因为上层文件的语义依赖下层：

1. **纯叶子**：`utils/version.ts`、`utils/models-error.ts`、`utils/text.ts`、`utils/hash.ts`、`utils/uuid.ts`、
   `utils/sanitize-unicode.ts`、`utils/diagnostics.ts`、`utils/sleep.ts`、`utils/abort.ts`、`utils/abort-signals.ts`。
2. **类型骨架**：`types.ts`（需要 `model-schema.ts`/`compat-schema.ts` 的 schema 类型）、`providers/model-schema.ts`、
   `providers/compat-schema.ts`（后者依赖 model-schema 的 `ModelCostSchema`）。
3. **工具层**：`utils/headers.ts`、`utils/provider-env.ts`、`utils/error-body.ts`、`utils/openrouter-cost.ts`、
   `utils/typebox-helpers.ts`、`utils/model-operations.ts`、`utils/estimate.ts`、`utils/overflow.ts`、
   `utils/retry.ts`、`utils/provider-retry.ts`、`utils/node-http-proxy.ts`、`utils/oauth-page.ts`、
   `utils/identity.ts`（依赖 `version.ts` 与 `models.ts` 的类型）。
4. **文本/解析**：`utils/json-parse.ts`（依赖 partial-json）、`utils/streaming-json.ts`（依赖 json-parse）、
   `utils/transcript.ts`（依赖 text）、`utils/validation.ts`（依赖 typebox）。
5. **流**：`utils/event-stream.ts`（无内部依赖）。
6. **集合**：`models-store.ts`、`session-resources.ts`、`model-catalog.ts`、`models.ts`
   （依赖 1–5 全部 + `telemetry.ts` + `api/lazy.ts` 的 `lazyStream` + `auth/**`）。
7. **provider 工厂**：`providers/*.ts`（依赖 `models.ts` 与各自的 `*_MODELS`）、`providers/all.ts`、
   `providers/faux.ts`、`providers/radius-config.ts` → `radius.ts`。
8. **注册表/兼容**：`images-api-registry.ts`、`providers/images/register-builtins.ts`、`images.ts`、
   `image-models.ts`、`legacy-api-aliases.ts`、`compat.ts`。
9. **入口**：`index.ts`、`oauth.ts`、`bun-oauth.ts`、`bedrock-provider.ts`、`cli.ts`。
10. **脚本与协议**：`scripts/**`、`scripts/model-catalog-protocol.ts`（与运行时代码无依赖，但共享数据格式）。

`models.ts` 的导入清单（源文件 1–71 行）也印证该层次：`api/lazy.ts`、`auth/context.ts`、`auth/credential-store.ts`、
`auth/resolve.ts`、`auth/types.ts`、`models-store.ts`、`telemetry.ts`、`types.ts`、`utils/abort.ts`、`utils/identity.ts`、
`utils/model-operations.ts`、`utils/models-error.ts`、`utils/text.ts`、`utils/transcript.ts`。

---

## 3. 用户可见表面（逐字）

本节列出所有用户可观察的文本/交互；Rust 实现必须**逐字节**一致（含空格、换行、标点、大小写）。

### 3.1 CLI `kiana-ai`（`packages/ai/src/cli.ts`，bin 名 `kiana-ai`）

无参数或 `help`/`--help`/`-h` 时输出（单次 `console.log`，末尾无额外空行）：
```
Usage: npx @earendil-works/kiana-ai <command> [provider]

Commands:
  login [provider]  Login to an OAuth provider
  list              List available providers

Providers:
  <id 以 padEnd(20) 对齐><name>
  ...
```
（`Providers:` 之后每行两个空格缩进，`id` 左对齐补空格到 20 列，然后一个空格，再 name。）

`list` 命令：每行 `` `${id.padEnd(20)} ${name}` ``（**无前导空格**），无标题行。

`login` 无 provider 参数时的交互：
```
  <n>. <provider name>
```
（两个空格、序号从 1、`. `），然后提示 `Enter number (1-<n>): `。

`answerPrompt` 的 `select` 提示：
```
\n<message>
  <n>. <label>
...
Enter number (1-<n>): 
```
其他 prompt：`` `<message>` `` 或 `` `<message> (<placeholder>): ` ``。

登录过程 notify 文本：
- `auth_url`：先 `console.log("\nOpen this URL in your browser:\n" + url)`；若有 `instructions` 则**另起一次**
  `console.log(instructions)`（无前缀、无额外换行；为空串/undefined 时不再输出）。
- `device_code`：`\nOpen this URL in your browser:\n<verificationUri>` 与 `Enter code: <userCode>`（两次输出）。
- `info` / `progress`：`<message>`。

成功：`\nCredentials saved to auth.json`（`AUTH_FILE = "auth.json"`，当前工作目录）。

错误（全部经顶层 catch）：
```
Error: <message>
```
（`console.error("Error:", e instanceof Error ? e.message : String(e))`，退出码 **1**。）
错误消息集合：`Unknown provider: <id>`、`Unknown command: <command>`、`Invalid selection`、
`Unknown provider: `（provider 参数为空串时也会出现）。

### 3.2 OAuth 浏览器页面（`src/utils/oauth-page.ts`）

成功页：`<title>Authentication successful</title>`、`<h1>Authentication successful</h1>`、`<p>{message}</p>`。
失败页：`Authentication failed`（title 与 h1）、`<p>{message}</p>`，可选 `<div class="details">{details}</div>`。
HTML 结构、CSS（含 `--text:#fafafa`、`--text-dim:#a1a1aa`、`--page-bg:#09090b`、字体栈、`.logo{width:72px;height:72px;
display:block;margin-bottom:24px}`、`h1{font-size:28px;line-height:1.15;font-weight:650}`、`p{line-height:1.7;
color:var(--text-dim);font-size:15px}`、`.details{margin-top:16px;font-family:var(--font-mono);font-size:13px;
color:var(--text-dim);white-space:pre-wrap;word-break:break-word}`）与 `LOGO_SVG` 必须逐字复制（见 2.2.21）。
转义顺序：`&`→`&amp;`、`<`→`&lt;`、`>`→`&gt;`、`"`→`&quot;`、`'`→`&#39;`。

### 3.3 错误消息总表（面向调用方/最终用户）

`ModelsError`（`name = "ModelsError"`，`code` 为下列之一）：

| code | 消息模板 |
|---|---|
| **auth** | `Provider is not configured: ${model.provider}` |
| provider | `Unknown provider: ${providerId}` |
| provider | `Provider ${model.provider} does not support deferred responses` |
| provider | `Provider ${model.provider} does not support image generation` |
| provider | `Provider ${model.provider} does not support classification` |
| provider | `Provider ${input.id} has no API implementation for "${model.api}"` |
| provider | `Provider ${input.id} has no image generation implementation for "${model.api}"` |
| provider | `Provider ${input.id} has no classifier implementation for "${model.api}"` |
| provider | `Provider ${input.id} does not support deferred responses for "${model.api}"` |
| provider | `Provider ${input.id} cannot cancel deferred responses for "${model.api}"` |
| provider | `Model ${provider}/${id} is not a chat model` / `... is not an image model` / `... is not a classifier model` |
| provider | `Model ${provider}/${id} does not accept image input` |
| auth | `${provider.name} does not support ${type} login` |
| auth | `Credential store modify failed for ${providerId}` |
| auth | `Credential store delete failed for ${providerId}` |
| auth | `Credential store read failed for ${providerId}` |
| auth | `API key auth check failed for provider ${provider.id}` |
| model_source | `Model refresh failed for ${provider.id}`（包装非 Error 的抛出） |

普通 `Error`：
`Provider ${input.id}: at least one of "api", "images", or "classifiers" is required.`、
`Mismatched api: ${model.api} expected ${api}`、`No API provider registered for api: ${api}`、
`Invalid selection`、`Unknown command: ${command}`、`Unknown provider: ${providerId}`（CLI）、
`Failed to cleanup session resources`（AggregateError）、
`Server requested ${n}s retry delay (max: ${m}s). ${message}`、`Request aborted`（AbortError）、
`The operation was aborted`（AbortError）、`Aborted`（内部 RetrySleepAbortError）、
`Tool "${name}" not found`、`Invalid arguments for tool "${name}": ${argumentsError}`、
`Validation failed for tool "${name}":\n  - ...\n\nReceived arguments:\n<json>`、
`Tool arguments must be a JSON object`、
`UUIDv7 timestamp must be an integer between 0 and ${MAX}` / `UUIDv7 generator sequence exhausted`、
`Unsupported proxy protocol... Got ${protocol}` / `Invalid proxy URL ${json}: ${message}`、
`OpenRouter model list request failed: ${status}` / `Invalid OpenRouter model list response`、
`Could not load Radius config from ${gateway}: ${status}: ${body}` / `Invalid Radius config from ${gateway}`、
`Unknown faux deferred response: ${id}` / `Faux deferred response was cancelled: ${id}` /
`No more faux responses queued` / `Faux response ended without a stop reason`、
`Unknown Amazon Bedrock auth method: ${method}` / `Unknown Google Vertex AI auth method: ${method}`、
`Unexpected event type for final result`（EventStream 内部）、
`Cannot hydrate missing providers: ${ids}`、`Model catalog is missing provider: ${provider}`（脚本）等。

### 3.4 交互提示（provider 登录流程）

- Anthropic API key：`Enter Anthropic API key`（secret）。
- Bedrock：`Select Amazon Bedrock authentication method:`、`Enter Amazon Bedrock bearer token`、
  `Enter AWS profile name`、`Configure AWS credentials, then press Enter to continue`；
  信息：`Amazon Bedrock supports AWS profiles, IAM credentials, and role-based credentials.`，
  链接 `AWS credential provider chain` -> `https://docs.aws.amazon.com/sdkref/latest/guide/standardized-credentials.html`。
- Google Vertex：`Select Google Vertex AI authentication method:`、`Enter Google Cloud API key`、
  `Enter Google Cloud project ID`、`Enter Google Cloud location`、`Enter service account credentials file path`；
  信息（adc）：`Run \`gcloud auth application-default login\`, then provide the project and location.`；
  信息（service-account）：`Provide a service account credentials file, project, and location.`；
  链接 `Application Default Credentials` -> `https://cloud.google.com/docs/authentication/provide-credentials-adc`。
- Cloudflare：`Enter Cloudflare API key`、`Enter Cloudflare account ID`、`Enter Cloudflare AI Gateway ID`。
- OAuth 登录名（`ProviderAuth.oauth.name`）：`Anthropic (Claude Pro/Max)`、`OpenAI (ChatGPT subscription)`、
  `OpenAI (ChatGPT Plus/Pro)`（openai-codex）、`GitHub Copilot`、`OpenRouter OAuth`、`Kimi Code (subscription)`、
  `Meta (Muse subscription)`、`xAI (Grok/X subscription)`、`Radius`（可配置）。
  `loginLabel`（共 5 个，其余 provider 不设置）：openai `Sign in with ChatGPT`、openrouter `Sign in with OpenRouter`、
  xai `Sign in with SuperGrok or X Premium`、**kimi-coding `Sign in with Kimi Code`**、**meta `Sign in with Meta`**。
- `isSubscription`：anthropic、openai、openai-codex、github-copilot、kimi-coding、meta、xai 为 true；
  openrouter、radius 未设置。

### 3.5 工具/控制台输出（脚本）

`check-model-data.ts`：成功 `Generated model data is valid.`；失败 stderr 两行（见 2.4.2）。
`generate-test-image.ts`：`Generated test image at: <path>`。
`generate-models.ts`：见 2.4.9 的阶段输出与统计块（`Model Statistics:` 前有空行）。
`hydrate-model-catalog.ts`：无成功输出（仅错误抛出）。

---

## 4. 外部格式

### 4.1 磁盘文件

#### 4.1.1 `auth.json`（CLI 登录凭据）

路径：**当前工作目录**下的 `auth.json`（`cli.ts` 的 `AUTH_FILE`）。格式：
```json
{
  "<providerId>": { "type": "oauth", "access": "...", "refresh": "...", "expires": 1234567890123, ... }
}
```
写入：`JSON.stringify(auth, null, 2)`，UTF-8，**无尾随换行**。读取：文件不存在或 JSON 解析失败都当作 `{}`。
字段由 `OAuthCredential`（`auth/types.ts`）定义（`type`、`access`、`refresh`、`expires`，以及 provider 特定的
`availableModelIds`、`gatewayConfig` 等）；不认识的多余字段应原样保留（`loadAuth` 不校验）。

#### 4.1.2 `src/providers/data/.manifest.json`（模型数据清单，schemaVersion 6）

```json
{
  "schemaVersion": 6,
  "generatedAt": "2026-...-...T...:...:...Z",
  "structureHash": "<sha256 hex 64>",
  "files": { "<provider>.json": "<sha256 hex 64>", ... }
}
```
- `structureHash` = sha256(`JSON.stringify(normalized)`)，`normalized` 是 `{provider: {"type:id": api}}`，
  provider 与内层键都按**码元序**升序（`Array.prototype.sort` 默认）。
- `files` 键按码元序排序；每个值是**文件完整内容**（含尾随换行）的 sha256。
- `generatedAt` 由生成器用 `new Date().toISOString()`；hydrate 时固定为 `"1970-01-01T00:00:00.000Z"`。
- 校验要求 manifest 的 `files` 键集合与数据文件集合完全一致。

#### 4.1.3 `src/providers/data/<provider>.json`（按 API 分组的模型目录）

结构：**两层对象**：外层键为 `api`（如 `"anthropic-messages"`），内层键为 `"<type>:<id>"`
（`"chat:claude-sonnet-4-5"`、`"image:..."`、`"classifier:..."`），值为完整模型对象（含 `id`、`name`、`api`、
`provider`、`baseUrl`、`reasoning`、`input`、`cost`、`contextWindow`、`maxTokens`、可选 `thinkingLevelMap`、
`compat`、`headers`、`inputLimits`、`type`、`output` 等）。
序列化：`JSON.stringify(groups)`（紧凑）或 `--pretty` 时 2 空格缩进，**尾随一个 `\n`**。
键序：provider 内 api 组按 `Array.from(new Set(apis)).sort()`；组内按生成顺序（`groupProviderModelData` 不再排序）。
模型对象字段顺序由构造时的对象字面量顺序决定（生成器构造顺序），JSON 序列化保持插入序。

#### 4.1.4 JSON catalog 输出（`--json-output <dir>`）

```
<dir>/models.json                 { provider: { "<type>:<id>"?: ... } }  // 仅 chat，keyed by id
<dir>/models.all.json             { provider: [ ...所有类型模型 ] }
<dir>/providers.json              [ "<providerId>", ... ]（排序）
<dir>/providers/<id>.json         单个 provider 的 chat，keyed by id
<dir>/providers/<id>.all.json     单个 provider 的全类型数组
```
注意 `models.json` 与 `providers/<id>.json` 的键是**模型 id**（不是 `type:id`），因为只含 chat；
键序为 `localeCompare` 排序（与内部 data 的码元序不同）。数组顺序为 chat、image、classifier 三段拼接。

#### 4.1.5 `models.generated.ts` 与 42 个 `providers/<id>.models.ts`

见 2.3.3 与 2.4.9 的模板（逐字）。这两个是**源码生成物**，Rust 侧可选择用构建脚本生成等价 Rust 常量或
在构建时读取 JSON；但若 Rust 也产出这些文件，必须字节一致。

#### 4.1.6 `nix/model-catalog.json`

**逐字内容**（一个 `revision` 字段，**文末有尾随换行**；`od -c` 末尾为 `"\n}\n`，共 92 字节）：

```json
{
  "revision": "sha256-7fe1b41feaf43a7aad4bc832e6b306c7ced56e6d41bbc724b74e91eeacff6edf"
}
```

这是 Nix 固定输出派生（FOD）的 sha256。Rust 侧只需保留文件（不解析）；`src/providers/data/` 被 `.gitignore` 忽略，
必须由该 revision 对应的目录版本 hydrate（package.json 脚本名 `hydrate-model-data`，内容
`generate-models.ts --strict --data-only`；`check-model-data.ts` 的报错文案里写的是 `hydrate:model-data`，
两者是同一个动作，**文案必须逐字保留**），否则 `check:model-data` 失败。

### 4.2 环境变量

| 变量 | 用途 |
|---|---|
| `ANTHROPIC_API_KEY` / `ANTHROPIC_OAUTH_TOKEN` / `ANTHROPIC_AUTH_TOKEN` | Anthropic 鉴权；`AUTH_TOKEN` 以 `Authorization: Bearer` 头形式使用，且**不**被 `getEnvApiKey` 选为 apiKey |
| `ANTHROPIC_FEDERATION_RULE_ID` / `ANTHROPIC_ORGANIZATION_ID` / `ANTHROPIC_SERVICE_ACCOUNT_ID` / `ANTHROPIC_IDENTITY_TOKEN_FILE` / `ANTHROPIC_WORKSPACE_ID` | Anthropic workload identity federation（前三个必需，后两个可选） |
| 各 provider 的 key 变量 | 见 2.1.12 表 |
| `GOOGLE_APPLICATION_CREDENTIALS` / `GOOGLE_CLOUD_PROJECT` / `GCLOUD_PROJECT` / `GOOGLE_CLOUD_LOCATION` / `GOOGLE_CLOUD_API_KEY` | Vertex ADC 检测与配置 |
| `AWS_PROFILE` / `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_BEARER_TOKEN_BEDROCK` / `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` / `AWS_CONTAINER_CREDENTIALS_FULL_URI` / `AWS_WEB_IDENTITY_TOKEN_FILE` | Bedrock ambient 凭证 |
| `AZURE_OPENAI_API_KEY` / `AZURE_OPENAI_BASE_URL` / `AZURE_OPENAI_RESOURCE_NAME` / `AZURE_OPENAI_API_VERSION`（默认 `v1`） / `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` | Azure provider（base URL/部署名解析在 `src/api/azure-openai-config.ts`，非本单元；本单元的 `env-api-keys` 只消费 `AZURE_OPENAI_API_KEY`） |
| `CLOUDFLARE_API_KEY` / `CLOUDFLARE_ACCOUNT_ID` / `CLOUDFLARE_GATEWAY_ID` | Cloudflare |
| `http_proxy` / `https_proxy` / `all_proxy` / `no_proxy`（各大小写） | 代理解析（`resolveHttpProxyUrlForTarget`） |
| 进程环境（`process.env`）与 `/proc/self/environ` | `getProviderEnvValue` 的回退链 |

环境值哨兵 `"<authenticated>"`（`AMBIENT_AUTH_MARKER`）表示"由环境中的 ambient 凭证完成鉴权，无需 apiKey"。
`/proc/self/environ` 只在 `process.versions.bun` 存在且 `process.env` 为空时读取（Bun 沙箱 bug 绕过）。

### 4.3 HTTP 端点与请求头

| 端点 | 方法 | 请求头 / 说明 | 用途 |
|---|---|---|---|
| `{baseUrl}/models/user` | `GET` | `accept: application/json`、`authorization: Bearer <key>` | OpenRouter 个性化目录；`AbortSignal.timeout(15000)`；401/403/404 时原样返回 catalog；非 2xx 抛 `OpenRouter model list request failed: <status>`；body 非 `{data: []}` 抛 `Invalid OpenRouter model list response` |
| `{gateway}/v1/config` | `GET` | `accept: application/json`，有 apiKey 时 `authorization: Bearer <key>`；URL 为 `new URL("/v1/config", gateway)`（绝对路径，会丢弃 gateway 的路径前缀） | Radius 目录；非 2xx 抛 `Could not load Radius config from <gateway>: <status>: <body 前 512 字符 + …>`；形状非法抛 `Invalid Radius config from <gateway>` |
| `https://openrouter.ai/api/v1/models[?output_modalities=image\|decisions]` | `GET` | — | 生成器数据源（三次：空 query、`?output_modalities=image`、`?output_modalities=decisions`） |
| `https://ai-gateway.vercel.sh/v1/models` | `GET` | — | 生成器数据源 |
| `https://models.dev/api.json` | `GET` | — | 生成器数据源 |
| `https://integrate.api.nvidia.com/v1/models` | `GET` | — | 生成器（模型 id 归一化） |

**身份/归属头**（`getIdentityHeaders`）：
- 所有请求：`User-Agent: kiana/<version> (<platform>; <runtime>/<runtime version>; <arch>)`（或 `kiana/<ver> (browser)`，
  `short` 模式为 `kiana`，自定义为给定值）。
- OpenRouter（provider 为 `openrouter` 或 baseUrl 含 `openrouter.ai`）：`HTTP-Referer: <url>`、
  `X-OpenRouter-Title: <name>`、`X-OpenRouter-Categories: <categories.join(",")>`（仅当非空）。
- NVIDIA（provider `nvidia` 或 host 为 `integrate.api.nvidia.com`）：`X-BILLING-INVOKE-ORIGIN: <Name>`（首字母大写）。
- OpenCode（provider `opencode`/`opencode-go` 或 host `opencode.ai`）：`x-opencode-client: <name>`、
  `x-opencode-session: <sessionId>`（有会话时）。
默认 `KIANA_ATTRIBUTION = {name:"kiana", url:"https://kiana.dev", categories:["cli-agent"]}`。

**头合并顺序**（`applyAuth`，后者覆盖前者，`null` 表示删除/抑制）：
1. `getOpenCodeSessionHeaders(model, sessionId)`；
2. provider 的 `auth.headers`（如 Cloudflare 的 `cf-aig-authorization` 与两个 null 抑制）；
3. 请求 `options.headers`；
4. `options.transformHeaders(headers ?? {})`（可选，最终变换；返回的即最终头）。

### 4.4 SSE / 流式事件

本单元不解析具体 SSE；它定义 `AssistantMessageEvent` 联合（见 2.1.2）与 `AssistantMessageEventStream` 语义：
- 事件顺序：`start` -> 每个块的 `*_start` -> 若干 `*_delta` -> `*_end` -> `done` 或 `error`；
  块之间按 `contentIndex` 递增，`partial` 是截至该事件的累积消息。
- `done` 事件携带最终 `message`；`error` 事件携带 `reason`（`"error"` 或 `"aborted"`）与 `error` 消息。
- `AssistantMessageEventStream` 在 `done`/`error`/`end(result)` 时写入 `durationMs`（单调时钟，`Math.round`，
  下限 0，仅当消息尚无 `durationMs` 且 `timestamp >= 流的 startedAt`）。
- `result()` 返回 `finalResultPromise`：`done` -> `message`，`error` -> `error`，`end(result)` -> result。
- 队列语义：`push` 在 `done` 之后被忽略；`end` 不传 result 时保留既有 promise 结果；`asyncIterator` 先排空队列再等待。

---

## 5. 跨单元接口（精确类型）

### 5.1 提供（本单元导出、其它单元消费）

| 类型/函数 | 消费方 | 契约 |
|---|---|---|
| `Provider` / `Models` / `MutableModels` / `createModels` / `createProvider` | coding-agent、kiana-durable | 见 2.1.3；`getModels()` 同步返回数组，`stream()` 同步返回流 |
| `AssistantMessageEvent` / `AssistantMessageEventStream` / `createAssistantMessageEventStream` | coding-agent TUI | 见 4.4 |
| `normalizeContext` / `TranscriptContext` / `resolveTranscript` / `resolveTranscriptTools` | 各 API 实现 | `normalizeContext` 是**唯一**产生 `TranscriptContext` 的入口；API 实现必须接收它 |
| `ModelsStore` / `InMemoryModelsStore` / `ModelsStoreEntry` | coding-agent 持久化 | 见 2.1.4；`etag` 原样保存（含引号） |
| `CredentialStore`（来自 `auth/`，本单元仅消费）/ `AuthContext` | — | `read`/`list`/`modify`/`delete`，`modify` 串行化 |
| `retryAssistantCall` / `isRetryableAssistantError` / `RetryPolicy` | coding-agent 重试循环 | 见 2.2.18 |
| `isContextOverflow` / `isRecoverableLength` / `getOverflowPatterns` | coding-agent 压缩逻辑 | 见 2.2.17 |
| `estimateContextTokens` / `ContextUsageEstimate` | coding-agent 上下文计量 | 见 2.2.16 |
| `calculateCost` / `getAiGatewayCost` / `openRouterCost` / `applyOpenRouterReportedCost` | 计费展示 | 见 2.1.3 / 2.2.15 |
| `validateToolCall` / `validateToolArguments` | 工具执行 | 见 2.2.28 |
| `StreamingJsonParser` / `ToolCallArgumentsStream` | API 流式实现 | 见 2.2.26 |
| `ClientIdentity` / `KIANA_ATTRIBUTION` / `withClientIdentity` / `getIdentityHeaders` | coding-agent | 见 2.2.22 |
| `registerSessionResourceCleanup` / `cleanupSessionResources` | Codex WebSocket 池 | 见 2.1.5 |
| `AI_TELEMETRY_SCHEMA` / `observeModelRequest` | telemetry 适配层 | 见 2.1.14 |
| `KIANA_AI_VERSION` | 各处 | `"1.4.0"` |

### 5.2 消费（本单元调用、其它单元实现）

| 依赖 | 需要的精确形状 |
|---|---|
| `src/api/*.lazy.ts` 的 `lazyApi()` | 返回 `{stream, streamSimple, fetchDeferred?, cancelDeferred?}`（`ApiStreamOptions`/`SimpleStreamOptions`） |
| `lazyStream(model, setup, observer?)` | 同步返回 `AssistantMessageEventStream`；`setup` 异步获得内层流后转发；observer 收到 `onEvent`/`onResult` |
| `auth/types.ts` | `ApiKeyAuth {name, login?, check?, resolve}`, `OAuthAuth {name, isSubscription?, loginLabel?, login, refresh, toAuth}`, `ProviderAuth {apiKey?, oauth?}`, `Credential`/`OAuthCredential`/`ApiKeyCredential`, `AuthContext {env(name), fileExists(path)}`, `CredentialStore`, `AuthInteraction`(`prompt`/`notify`/`select`/`signal`), `AuthResolution {auth:{apiKey?, headers?, baseUrl?}, env?, source}` |
| `auth/resolve.ts` | `resolveProviderAuth(provider, store, ctx, options)`, `refreshStoredOAuthCredential(store, providerId, oauth, shouldRefresh, signal)`, `ModelsError`/`ModelsErrorCode` |
| `auth/oauth/load.ts` | `loadAnthropicOAuth`, `loadOpenAIChatGPTOAuth`, `loadOpenAICodexOAuth`, `loadGitHubCopilotOAuth`, `loadOpenRouterOAuth`, `loadKimiCodingOAuth`, `loadMetaOAuth`, `loadXaiOAuth`, `loadRadiusOAuth({name, gateway})` |
| `api/azure-openai-config.ts` | `resolveAzureBaseUrl(model, options)`, `resolveDeploymentName(model, options)` |
| `api/cloudflare.ts` | 常量 `CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL`、`CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL`、`CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL`、`CLOUDFLARE_WORKERS_AI_BASE_URL`、`CLOUDFLARE_WORKERS_AI_REST_BASE_URL` |
| `@earendil-works/kiana-telemetry` | `TelemetrySpan`（`setAttributes`、`setStatus`、`addEvent`、`end`）、`defineTelemetrySchema`、`createTypedSpanStarter`、`createTypedMetrics`、`NOOP_TELEMETRY_CONTEXT`、`NOOP_TELEMETRY_METRICS`、`SchemaTelemetrySpan`、`TelemetryMetrics` |
| `typebox` | `Type`（schema 构造）、`TSchema`、`Static`、`TUnsafe`、`Compile`（`Check`/`Errors`）、`Value.Convert`、`TLocalizedValidationError`（`keyword`/`instancePath`/`params`/`message`） |
| `partial-json` | `parse(text)`：宽容解析不完整 JSON，返回 `unknown`（失败抛错） |

---

## 6. npm 依赖 → Rust crate 映射

| npm | 建议 Rust crate | 行为差异与移植注意 |
|---|---|---|
| `typebox` 1.3.27 | `serde_json` + `jsonschema`（或 `schemars` 生成） | `Value.Convert` 的隐式转换（string↔number/boolean、`null`→0/""/false）需自写；`Compile` 的 `Errors()` 消息文本与 `instancePath` 格式（`/a/b` → `a.b`）需复刻；`Type.Unsafe`/`Symbol.for("TypeBox.Kind")` 用自定义标记替代 |
| `partial-json` 0.1.7 | 自写（可参考 `json5`/`jsonrepair`） | 语义是"宽容解析不完整 JSON"，与 `repairJson` 叠加三层回退；建议直接移植 `parseStreamingJson` 的三级回退并加单元测试 |
| `@anthropic-ai/sdk` 0.129.0 | `reqwest` + 手写 SSE | 仅被 `src/api/anthropic-messages.ts`（非本单元）使用；本单元只依赖错误形状（`status`、`error`、`message`） |
| `openai` 7.19.0 | `reqwest` + `eventsource-stream` | 同上；错误形状 `status`、`error`（解析后的 body） |
| `@google/genai` 2.21.0 | `reqwest` | 错误形状 `status`、`message`（已含 body） |
| `@aws-sdk/client-bedrock-runtime` 3.1127.0 | `aws-sdk-bedrockruntime` + `aws-config` | 错误形状 `$metadata.httpStatusCode`、`$response.statusCode`、`$response.body`（class 实例，非普通对象） |
| `http-proxy-agent` 9.1.0 / `https-proxy-agent` 9.1.0 | `reqwest::Proxy` + 自写 `no_proxy` 解析 | 仅在 `src/api/bedrock-converse-stream.ts`（非本单元）用于 Bedrock 的 `NodeHttpHandler`；本单元只提供 `resolveHttpProxyUrlForTarget` 的 URL 解析语义（通配、IPv6 括号、端口、SOCKS/PAC 拒绝消息），代理连接本身由 API 单元负责 |
| `@smithy/node-http-handler` 4.12.1 | 无（`reqwest`） | 仅在 `src/api/bedrock-converse-stream.ts` 使用（`NodeHttpHandler` 与 `BuildMiddleware`/`DeserializeMiddleware`）；不属于本单元 |
| 全局 `fetch`（Node 内置，代码中**无** `import ... from "undici"`） | `reqwest` | `AbortSignal.any`/`AbortSignal.timeout` 用 `tokio_util::sync::CancellationToken` + `tokio::time::timeout` |
| `node:crypto`（sha256、randomUUID、getRandomValues） | `sha2`、`uuid`（v7）、`getrandom` | UUIDv7 的 41 位序列与单调时间戳需自写（`uuid` crate 的 v7 语义不同） |
| `node:fs` / `node:os` / `node:path` | `std::fs`、`dirs`、`std::path` | `~` 展开需显式实现；`/proc/self/environ` 直接读 |
| `node:readline` | `rustyline` 或自写 stdin 行读取 | CLI 提示文本逐字；实际提示为 `Enter number (1-${n}): `（**带范围，n 随选项数变化**，见 §2.1.x cli.ts），读一行后 `Number.parseInt(..., 10) - 1` |
| `canvas` | 无 | 仅 `generate-test-image.ts` 使用，Rust 不需要 |
| `vitest` 4.1.12 | `cargo test` + `insta`（快照） | 见 §7 |
| `@earendil-works/kiana-telemetry` | 自写 trait（`TelemetrySpan`/`TelemetryMetrics`） | 见 5.2；`NOOP` 实现必须零开销 |

**JS 语义差异清单（必须显式处理）**：
1. **字符串索引是 UTF-16 码元**：`text.length`、`slice`、`charCodeAt`、`shortHash`、`ownedSlice`、`estimateTokens`、
   faux 的 `splitStringByTokenSize` 全部按码元。Rust 的 `String::len()` 是字节、`chars()` 是标量。建议在
   "逐字对齐"处用 `Vec<u16>` 或 `encode_utf16()` 遍历，输出时再解码。
2. **JSON 数字格式化**：`String(number)`/`JSON.stringify` 使用 ECMAScript 算法（最短往返表示、`1e+21`、`1e-7`、
   `-0` -> `"0"`、`NaN`/`Infinity` -> `null`（JSON）或 `"NaN"`（String））。`serde_json` 的默认行为不同
   （大整数、浮点指数格式）。需要 `ryu` + 自定义 ECMAScript 格式化，或直接用 `serde_json::Number` 并逐例测试。
3. **对象键序**：JS 对象保持插入序，但**整数形态的键**（如 `"0"`、`"42"`）会被排到最前并按数值排序。
   模型数据里键是 `"chat:..."` 这类字符串，不受影响；但 `sections`、`headers`、`compat` 等用户数据可能含数字键。
   Rust 用 `IndexMap` 无法复现"整数键前置"规则，需在 JSON 序列化层处理。
4. **正则方言**：JS 支持 lookbehind（`(?<!...)`）、`\uXXXX`、`i` 标志下的 Unicode 大小写；
   `regex` crate 不支持 lookaround，需改用 `fancy-regex` 或手工实现（`sanitizeSurrogates`、retry 模式）。
5. **`localeCompare`**：`generate-models.ts` 的 JSON 输出键序与 `model-catalog-protocol.ts` 的 prerelease 比较
   使用 ICU 排序；Rust 需固定区域（`en-US`）并与 Node 默认 ICU 对齐。
6. **`Date.toISOString()`**：毫秒精度、`Z` 后缀、年份 4 位（`chrono` 的 `to_rfc3339_opts(SecondsFormat::Millis, true)` 一致）。
7. **`Number.isSafeInteger` / `MAX_SAFE_INTEGER`**：`retryDelayMs` 的溢出保护需 `i64`/`f64` 检查。
8. **`Math.imul`**：`shortHash` 的 32 位乘法需 `u32::wrapping_mul`。
9. **`structuredClone`**：`serde_json::Value` 的 `clone()` 等价（无循环引用、无函数、无 Map/Set 时）。
10. **`AbortSignal`**：`reason` 可为任意值；`throwIfAborted()` 抛 `reason`（非 Error 时也原样抛）。
    Rust 需 `CancellationToken` + `AbortReason`（`Any` 或枚举）。
11. **`Headers`**：忽略大小写、多值合并（`get` 返回逗号连接）、`null` 表示删除的约定由本单元维护。
12. **`performance.now()`**：单调毫秒（浮点）；`durationMs = Math.round(delta)`。

---

## 7. 测试清单

`packages/ai/test/` 顶层共 **190** 个条目 = 182 个 `*.test.ts` + 7 个辅助 `.ts` + 1 个 `data/` 目录。
辅助文件：`azure-utils.ts`、`bedrock-utils.ts`、`cloudflare-utils.ts`、`codex-websocket-cached-probe.ts`、
`memory-helpers.ts`（`collectGarbage`/`allocatedBytes`/`isCollected`）、`oauth.ts`、`scratch.ts`；
`data/` 内含 `json-test-suite.json`、`red-circle.png`。
`vitest.config.ts`：`globals: true`、`environment: "node"`、`testTimeout: 30000`、`silent: "passed-only"`、
`reporters`（CI 下 `["dot","github-actions"]`，否则 `["dot"]`）、把 `@earendil-works/kiana-telemetry` 别名到
`../telemetry/src/index.ts`（因此测试跑的是 telemetry 源码而非 dist）。

### 7.1 与本单元直接相关的测试文件（真实文件名 + `it/test` 计数）

| 测试文件 | 用例数 | 断言的行为 |
|---|---|---|
| `models-runtime.test.ts` | 43 | Provider 注册/覆盖、`refresh` 阶段顺序（先缓存后网络）、发布串行化与取消、`getAvailable*` 过滤、`checkAuth`、`login`/`logout` 的 modify 语义、错误消息 |
| `supports-xhigh.test.ts` | 28 | `getSupportedThinkingLevels` 对 `thinkingLevelMap` 的过滤（`xhigh`/`max` 需显式映射）、`clampThinkingLevel` 升降级 |
| `streaming-json.test.ts` | 31 | 增量解析全部规则：未终止字符串/转义/容器、缺失逗号冒号、非法转义、`__proto__` 自有成员、重复键保位、深度 >32 整体上报、`snapshot()` 不可变与 credit 滞后、`append` 变更序列 |
| `stream.test.ts` | 233 | e2e：各 provider 的 wire 请求 / SSE 解析 / 事件序列（`start`→`*_start`→`*_delta`→`*_end`→`done`）、`partial` 累积、`durationMs` |
| `empty.test.ts` | 120 | 空/空白/`null` 内容、空工具列表、空系统提示、空响应体等边界 |
| `unicode-surrogate.test.ts` | 90 | 未配对代理项在 JSON/SSE/工具参数中的处理、`ownedSlice` 按 UTF-16 码元切片 |
| `openai-completions-tool-choice.test.ts` | 49 | 工具选择（`auto`/`none`/`required`/指定）、并行工具调用、`tool_choice` 序列化 |
| `overflow.test.ts` | 23 | `isContextOverflow` 判定与 NON_OVERFLOW 排除 |
| `context-overflow.test.ts` | 35 | 溢出检测在真实 provider 错误消息上的端到端样例（regex 表） |
| `retry.test.ts` | 27 | `retryAssistantCall` 回调次数/顺序、`RetrySleepAbortError` -> `stopReason:"aborted"` 且删除 `errorMessage`、`isRetryableAssistantError` 正负样例 |
| `provider-retry.test.ts` | 6 | `retryProviderRequest`：`x-should-retry` 优先、408/409/429/5xx、`retry-after-ms`/`retry-after`（含 HTTP 日期）、上限错误文本、`noRetryStatuses`、`maxRetryDelayMs: 0` 禁用 |
| `transcript-tool-changes.test.ts` | 22 | `getToolStateChanges`、`hasNonAdditiveToolChanges`、`resolveTranscriptTools` 三种 support 的 `foldedSystemMessages` |
| `faux-provider.test.ts` | 24 | 脚本化响应、token 分块、prompt cache 模拟（首轮 `cacheWrite = promptTokens` 且 `input = 0`）、deferred（`pendingFetches`/`pollAfterMs`/cancel）、`No more faux responses queued` |
| `model-data-validation.test.ts` | 21 | `validateModelDataDirectory` 的每类错误、sha256/manifest 校验、`type:id` 唯一性 |
| `context-estimate.test.ts` | 3 | `estimateContextTokens` 的 trailing 计算与 `latestPrefixTimestamp` 规则 |
| `tokens.test.ts` / `total-tokens.test.ts` | 31 / 35 | `Usage` 与 `totalTokens` 的归一化、`calculateContextTokens` |
| `model-cost-tiers.test.ts` | 5 | `calculateCost` 的 tier 选择（严格大于、最高阈值）、1h cache write 2x |
| `providers.test.ts` | 30 | 42 个 provider 工厂的 id/name/baseUrl/api/auth 完整性 |
| `model-types.test.ts` | 4 | `getModelType`/`isModelType`/`normalizeContext` 类型行为 |
| `models-entry.test.ts` | 1 | `models.ts` 入口导出完整性 |
| `message-types.test.ts` | 2 | `Message` 联合的判别 |
| `image-model-data.test.ts` / `images-models.test.ts` / `images.test.ts` | 4 / 13 / 2 | 图像模型目录、`getImageModel*`、`generateImages` 全局入口 |
| `classifier-models.test.ts` | 8 | classifier 目录与 `classify` 入口 |
| `openrouter-user-models.test.ts` | 2 | `personalizeOpenRouterCatalog`（401/403/404 原样、limit 合并） |
| `openrouter-reasoning-options.test.ts` / `reasoning-options.test.ts` | 7 / 3 | thinking level map 转换（`mandatory`、`none`、无效值） |
| `radius-provider.test.ts` | 5 | Radius 动态目录、legacy 导入、持久化 |
| `generate-models-strict.test.ts` | 1 | `--strict` 下数据源失败即抛 |
| `cache-retention.test.ts` | 19 | `cacheRetention` 的 `none`/`short`/`long` 与 compat 交互 |
| `system-message-replay.test.ts` | 9 | `getCurrentSystemMessage`/`collapseSystemMessages`/`resolveTranscript` 的 sections 与工具重放 |
| `streaming-tool-arguments.test.ts` | 4 | `ToolCallArgumentsStream` 的 `append`/`replace`/`finish` |
| `lazy-module-load.test.ts` | 5 | `lazyApi`/`lazyStream` 的同步返回与延迟初始化、错误包装 |
| `pre-generation-error.test.ts` | 1 | 生成器在数据源失败时的行为 |
| `telemetry.test.ts` / `telemetry-options.test.ts` | 15 / 3 | span 名/属性/事件、指标记录条件（pending/deferred 不计 token/cost） |
| `compat-env.test.ts` | 1 | `withEnvApiKey` 与 `AMBIENT_AUTH_MARKER` |
| `provider-error-body-regression.test.ts` / `provider-error-body-passthrough.test.ts` | 5 / 1 | `normalizeProviderError`/`formatProviderError` 的 SDK 字段探测与 `messageCarriesBody` |
| `model-catalog-types.test.ts` | 3 | `flatten*Catalog` 的分组与覆盖 |
| `opencode-provider-headers.test.ts` | 3 | `x-opencode-session` 注入与已有头不覆盖 |
| `identity.test.ts` | 11 | UA 格式、归属头优先级（openrouter > nvidia > opencode）、`withClientIdentity` 字段合并与 Mutable 保持 |
| `env-api-keys.test.ts` | 7 | 变量表、anthropic 选择顺序（跳过 `AUTH_TOKEN`）、Vertex ADC、Bedrock ambient、`"<authenticated>"` |
| `node-http-proxy.test.ts` | 5 | `no_proxy` 通配/IPv6/端口、SOCKS/PAC 拒绝消息、`all_proxy` 回退 |
| `openrouter-cost.test.ts` | 5 | `openRouterCost` 的 fallback/overrides、`applyOpenRouterReportedCost` 的 BYOK 合并 |
| `uuid.test.ts` / `abort.test.ts` / `text.test.ts` / `error-body.test.ts` / `json-parse.test.ts` | 4 / 41 / 4 / 17 / 8 | UUIDv7、`raceWithAbortSignal`/`operationSignal`、`contentText`/`ownedSlice`、错误体归一化、`repairJson`/`parseToolCallArguments` |
| `event-stream.test.ts` | 9 | `FifoQueue` 顺序、`done` 后 `push` 忽略、`end()` 唤醒等待者、`durationMs` 规则 |
| `validation.test.ts` | 10 | `validateToolArguments` 的强制转换、可选 null 删除、错误消息与 `Received arguments` 逐字 |
| `compat-schema.test.ts` | 3 | `CompatSchemasByApi`/`ProviderCompatSchema` 的默认值与合并 |
| `xhigh.test.ts` / `max-thinking.test.ts` / `sampling-options.test.ts` / `fetch-option.test.ts` / `constrained-sampling.test.ts` | 3 / 4 / 11 / 6 / 9 | 思考级别、采样参数、`fetch` 选项透传 |
| `zen.test.ts` | 1 | OpenCode Zen provider 组装 |

### 7.2 其余测试

其余 **121** 个测试文件覆盖 `src/api/**` 的 provider wire 协议（anthropic-*、bedrock-*、google-*、openai-*、
mistral-*、azure-*、cloudflare-*、classifier-images-live、kiana-messages、typesafe-system-one、responseid、
tool-call-without-result、image-tool-result、interleaved-thinking、lax-message-content、transform-messages-*、
tool-call-id-normalization、cross-provider-handoff 等）、`src/auth/**` 的 OAuth 流程
（anthropic-oauth、openai-chatgpt-oauth、openai-codex-oauth、github-copilot-oauth、kimi-coding-oauth、meta-oauth、
openrouter-oauth、radius-oauth、xai-oauth、oauth-auth、oauth-callback-server、oauth-device-code）以及模型目录数据
（fireworks-models、bedrock-models、baseten-models、together-models、xiaomi-models、zai-coding-plan-models、
qwen-token-plan-models 等）。移植时它们属于对应单元，但**本单元的改动不得破坏其契约**（尤其 `normalizeContext`、
`applyAuth` 头合并、`lazyStream` 同步返回、`durationMs`、`AssistantMessageEvent` 事件顺序）。

### 7.3 Rust 侧测试建议

- `streaming_json`：把 TS 的输入/期望变更序列做成表驱动测试；加随机分块 fuzz——同一文本按不同切分必须得到
  相同的 `snapshot()` 序列（TS 的测试也隐含这一点）。
- `repair_json` / `parse_tool_call_arguments`：黄金样例；构建时用 TS 生成 fixture 供 Rust 对比。
- `overflow` / `retry`：正则表驱动，每条模式至少一个正例与一个反例（含 NON_OVERFLOW 排除）。
- `models`：用 faux provider 做端到端（注册、refresh 两阶段、publish 串行化、取消、deferred）。
- `identity` / `headers` / `provider_env` / `node_http_proxy`：纯函数黄金样例。
- 生成数据：校验 sha256 与结构（等价 `check-model-data`），防止数据漂移。

## 8. 建议的 Rust 模块布局与最难复现的部分

### 8.1 Crate 布局（`kiana/crates/kiana-ai`）

```
kiana-ai/
  Cargo.toml                 # crate 名 kiana-ai，lib 名 kiana_ai
  src/
    lib.rs                   # re-export 顶层 API（对齐 src/index.ts）
    types.rs                 # 2.1.2：Content/Usage/StopReason/Model/Context/TranscriptContext/AssistantMessageEvent
    models/
      mod.rs                 # 2.1.3：Models trait、ModelsImpl、create_models、create_provider、calculate_cost
      provider.rs            # Provider trait + CreateProviderOptions + 动态模型合并
      store.rs               # 2.1.4：ModelsStore trait、InMemoryModelsStore
      catalog.rs             # 2.1.10：flatten_*_catalog、ModelGroups
    auth/                    # 仅 trait 与类型（实现由 auth 单元提供）
      types.rs               # ProviderAuth/ApiKeyAuth/OAuthAuth/Credential/AuthContext/CredentialStore
      resolve.rs             # resolve_provider_auth/refresh_stored_oauth_credential 的 trait 依赖
    utils/
      event_stream.rs        # 2.2.24：EventStream/AssistantMessageEventStream
      streaming_json.rs      # 2.2.26（最难）
      json_parse.rs          # repair_json/parse_json_with_repair/parse_tool_call_arguments/parse_streaming_json
      validation.rs          # 2.2.28
      transcript.rs          # 2.2.27
      text.rs                # content_text/get_system_message_text/render_system_message_update/owned_slice
      identity.rs            # 2.2.22
      headers.rs             # headers_to_record/provider_headers_to_record/merge_headers
      provider_env.rs        # 2.2.13
      error_body.rs          # 2.2.14
      openrouter_cost.rs     # 2.2.15
      estimate.rs            # 2.2.16
      overflow.rs            # 2.2.17
      retry.rs               # 2.2.18
      provider_retry.rs      # 2.2.19
      node_http_proxy.rs     # 2.2.20
      oauth_page.rs          # 2.2.21
      abort.rs / abort_signals.rs / sleep.rs
      hash.rs / uuid.rs / sanitize_unicode.rs / version.rs / diagnostics.rs
      model_operations.rs / models_error.rs / typebox_helpers.rs
    providers/
      mod.rs                 # 2.3.1：all.rs 的等价物 + builtin_providers()
      builtin/               # 每个 provider 一个文件（factory 表）
      compat_schema.rs       # 2.1.17
      model_schema.rs        # 2.1.16
      faux.rs                # 2.3.13
      cloudflare_auth.rs / cloudflare_stream.rs / opencode_headers.rs
      openrouter_api.rs / radius_config.rs
      data/                  # 生成的 JSON（include_str! 或 build.rs 读取）
      images.rs              # register-builtins 等价物
    catalog_protocol.rs      # 2.4.10（若本单元实现）
    cli.rs                   # 2.1.13（bin `kiana-ai`，用 clap 但输出逐字手写）
    telemetry.rs             # 2.1.14
    compat.rs                # 2.1.11（legacy 全局 API，可 cfg 掉）
    legacy_aliases.rs
  tests/                     # 见 7.3
  build.rs                   # 可选：把 data/*.json 编译成静态表并校验 sha256
```

**关键 trait/struct 草图**：

```rust
pub enum StopReason { Stop, Length, ToolUse, Error, Aborted, Pending, Deferred }

pub struct Model {
    pub id: String, pub name: String, pub api: String, pub provider: String, pub base_url: String,
    pub reasoning: bool,
    pub thinking_level_map: Option<BTreeMap<ModelThinkingLevel, Option<String>>>,
    pub input: Vec<Modality>, pub cost: ModelCost, pub context_window: u64, pub max_tokens: u64,
    pub headers: Option<ProviderHeaders>, pub compat: Option<serde_json::Value>,
    pub input_limits: Option<ModelInputLimits>,
    pub model_type: ModelType,           // 序列化为 "type"，缺省 Chat
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn base_url(&self) -> Option<&str>;
    fn headers(&self) -> Option<&ProviderHeaders>;
    fn auth(&self) -> &ProviderAuth;
    fn get_models(&self) -> Vec<Model>;
    fn get_all_models(&self) -> Vec<AnyModel> { self.get_models().into_iter().map(AnyModel::Chat).collect() }
    fn refresh_models(&self, ctx: RefreshModelsContext) -> BoxFuture<'_, ()> { ... 默认空 }
    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> { models }
    fn filter_all_models(&self, models: Vec<AnyModel>, credential: Option<&Credential>) -> Vec<AnyModel> { ... }
    fn stream(&self, model: Model, context: TranscriptContext, options: StreamOptions) -> AssistantMessageEventStream;
    fn stream_simple(...) -> AssistantMessageEventStream;
    fn fetch_deferred(&self, ...) -> Option<AssistantMessageEventStream> { None }
    fn cancel_deferred(&self, ...) -> Option<BoxFuture<'_, Result<(), ModelsError>>> { None }
    fn generate_images(&self, ...) -> Option<BoxFuture<'_, AssistantImages>> { None }
    fn classify(&self, ...) -> Option<BoxFuture<'_, ClassifierResult>> { None }
}

pub struct ModelsImpl { providers: IndexMap<String, Arc<dyn Provider>>, credentials: Arc<dyn CredentialStore>,
  models_store: Arc<dyn ModelsStore>, auth_context: Arc<dyn AuthContext>, identity: Option<ClientIdentity>,
  refresh_generations: Mutex<HashMap<String, u64>>, refresh_controllers: Mutex<HashMap<String, CancellationToken>>,
  publication_chains: Mutex<HashMap<String, Pin<Box<dyn Future<Output = ()> + Send>>>> }
```

**关键设计选择**：
- `AssistantMessageEventStream` 用 `tokio::sync::mpsc::UnboundedSender/Receiver` + `FifoQueue` 语义；
  `result()` 用 `tokio::sync::oneshot` 或 `futures::future::Shared`。必须支持**同步 push**（provider 在同步上下文里
  构造流）——因此内部队列用 `std::sync::Mutex` 的 `VecDeque`，唤醒用 `Notify`。
- `lazyStream` 的"同步返回、异步初始化"用 `tokio::spawn` + 立即返回流；`stream()` 不能是 `async fn`。
- `AbortSignal.any([a, b])` 用 `CancellationToken` 的组合。
- `durationMs` 用 `std::time::Instant`（单调）而不是 `SystemTime`。
- `structuredClone` -> `serde_json::Value::clone()`；`WeakMap` 缓存 -> `HashMap<usize, ...>`（按 schema `Arc` 地址）
  或 `OnceCell` 存于 schema 包装类型。
- 生成模型数据的 provider 键顺序与 api 组顺序用 `IndexMap` 保序；JSON 序列化器需保持插入序（`serde_json` 默认
  `Map` 是 BTreeMap，需开 `preserve_order` feature）。

### 8.2 最难复现的部分（按难度排序）

1. **`StreamingJsonParser` 的精确语义**：变更序列（`set`/`append` 的顺序与合并）、`snapshot()` 因 credit 不足返回
   **旧快照**的可观察行为、深度 >32 的整体 `set`、`__proto__` 自有成员、重复键保位、非法转义/`\u` 不完整的扣留规则。
   建议照搬状态机而不是用通用 JSON 库。
2. **typebox 的 `Value.Convert` + `Compile().Errors()` + 自定义强制转换**：错误消息、路径格式、`$ref`、
   `normalizeOptionalNulls` 的"可选 null 删除"组合。需要为每条错误格式写黄金测试。
3. **JS UTF-16 索引语义**：`text.length`、`slice`、`charCodeAt`、`shortHash`、`estimateTokens`、
   faux 分块、`ownedSlice` 的截断长度（`truncateErrorText` 的 `[truncated N chars]` 中的 N 是码元差）。
4. **JS 数字格式化**：`String(number)`（`validation.ts` 的 `String(value)`、`safeJsonStringify`、错误消息里的
   `${status}`）、`toFixed(6)`（`perMillion`/`roundCost`）、`Number.isSafeInteger` 溢出保护。
5. **JSON 键序**：`indexmap` + 整数键前置规则 + `localeCompare` 排序（生成器 JSON 输出）与码元序排序
   （manifest/structureHash）的**混用**。
6. **OAuth 流程与跨进程凭据锁**（`auth/` 单元，但本单元依赖其 `modify()` 串行化与"刷新在取消后继续"语义）。
7. **`AbortSignal` 语义**：`reason` 任意值、`throwIfAborted` 原样抛出、`raceWithAbortSignal` 吞掉被放弃的 rejection、
   `combineAbortSignals` 的 `once` + break 规则。
8. **`Headers` 的 `null` 抑制约定与大小写不敏感合并**（`mergeHeaders` vs `providerHeadersToRecord` 两种不同语义）。
9. **`fetch`/代理环境解析**（`no_proxy` 的 IPv6/通配/端口细节）。
10. **`localeCompare` 与 semver prerelease 比较**（catalog protocol）。
11. **`Date.parse` 的可接受格式**（manifest `generatedAt` 校验、`retry-after` HTTP 日期）。
12. **Bun 沙箱 `/proc/self/environ` 回退**（Rust 侧仅在特定条件下需要）。

---

## Audit additions

本节由对抗性完整性审计追加，用于补齐前文遗漏的导出符号、常量、行为、测试与文档覆盖。前文如有冲突，以本节为准。

### A.1 前文完全未提及的导出符号

用脚本对 `src/*.ts`、`src/utils/*.ts`、`src/providers/*.ts`（含 `providers/images/`）、`src/compat/*.ts` 共
**142 个文件**做 `export (const|let|var|function|class|interface|type|enum) X` 声明抽取，得到 **557 个唯一名**
（分片 126 + 非分片 431），再在规格正文（不含本节）做逐词边界匹配。**正文完全未出现的有 93 个**：非分片 8 个 + 分片 85 个。
（把 `scripts/*.ts` 与 `../../scripts/model-catalog-protocol.ts` 一并计入时为 152 文件 / 598 名，缺失仍为 93——
脚本的 41 个导出名全部已在正文出现。）

补充说明：
- 若只把 `src/*.ts`、`src/utils/*.ts`、`src/providers/*.ts`、`src/compat/*.ts` 的 141 个文件计入（不含
  `providers/images/register-builtins.ts` 的 2 个导出），唯一名为 555、非分片 429。
- 分片 85 个缺失名的成因是：正文只用 `<UPPER>_MODELS`/`_IMAGE_MODELS`/`_CLASSIFIER_MODELS` 的模板写法，
  没有逐字列出 42 个 provider 的三元组；`AMAZON_BEDROCK_MODELS`、`ANT_LING_MODELS` 等 41 个**已出现**，
  因为 provider 表格里引用过它们。
- `SessionAffinityFormat`、`ThinkingTokenBudgetField`（仅在 2.1.17 的"3 个 type 别名"括号里被点名，**无定义**）与
  `xiaomiTokenPlanCnProvider`、`xiaomiTokenPlanSgpProvider`（仅在 2.3.4 的 provider 表格里出现，**无签名**）
  四个虽被正文提及，但缺定义/签名，也在此处补齐。

**非分片（正文完全缺失 8 个 + 仅提及无定义 4 个）**——必须在 Rust 中提供等价类型/函数/常量：

| 符号 | 所在文件 | 说明 |
|---|---|---|
| `ApiProvider<TApi, TOptions>` | `src/compat.ts:85` | `{ api: TApi; stream: StreamFunction<TApi, TOptions>; streamSimple: StreamFunction<TApi, SimpleStreamOptions> }`；`registerApiProvider` 的参数类型 |
| `ApiStreamFunction` | `src/compat.ts:73` | `(model, context: TranscriptContext, options?: StreamOptions) => AssistantMessageEventStream` |
| `ApiStreamSimpleFunction` | `src/compat.ts:79` | 同上但 `options?: SimpleStreamOptions` |
| `BuiltinImageProvider` | `src/image-models.ts:13` | `type BuiltinImageProvider = { [TProvider in keyof Catalog]: [ImageModelIds<TProvider>] extends [never] ? never : TProvider }[keyof Catalog]`；其中 `Catalog = typeof IMAGE_MODELS`，`ImageModelIds<TProvider extends keyof Catalog> = keyof Catalog[TProvider]`（`ImageModelIds` **未导出**）。作用是筛出"至少有一个 image model"的 provider |
| `ClassifierChoiceAnswer` | `src/types.ts:712` | `{ type:"choice"; choice: string; probabilities: Record<string, number>; confidence: number }` |
| `ClassifierScoreAnswer` | `src/types.ts:719` | `{ type:"score"; score: number; confidence: number }` |
| `ClassifierBoolAnswer` | `src/types.ts:725` | `{ type:"bool"; probability: number }`（**无 confidence**） |
| `RadiusProviderOptions` | `src/providers/radius.ts:15` | `{ id?: string; name?: string; gateway?: string }` |
| `SessionAffinityFormat` | `src/providers/compat-schema.ts:645` | `Static<typeof SessionAffinityFormatSchema>` = `"openai" \| "openai-nosession" \| "openrouter"` |
| `ThinkingTokenBudgetField` | `src/providers/compat-schema.ts:644` | `Static<typeof ThinkingTokenBudgetFieldSchema>` = `"thinking_token_budget" \| "thinking_budget" \| "thinking_budget_tokens"` |
| `xiaomiTokenPlanCnProvider` | `src/providers/xiaomi-token-plan-cn.ts:6` | `(): Provider<"openai-completions">`；**早先版本**的规格表格用 `...CnProvider` 省略写法，现 §2.3.4 已逐字修正，此处保留以免 Rust 生成器误用截断名 |
| `xiaomiTokenPlanSgpProvider` | `src/providers/xiaomi-token-plan-sgp.ts:6` | 同上，`(): Provider<"openai-completions">` |

**分片（126）**——每个 `providers/<id>.models.ts` 都导出三个常量（`_MODELS`/`_IMAGE_MODELS`/`_CLASSIFIER_MODELS`），共 42 个文件 × 3 = 126 个导出名。
前文只在泛型模板里写了 `<UPPER>_MODELS` 等，未给出具体名字。以下为**逐字完整清单**（按文件，每行三个）：

- `amazon-bedrock.models.ts`: `AMAZON_BEDROCK_MODELS`, `AMAZON_BEDROCK_IMAGE_MODELS`, `AMAZON_BEDROCK_CLASSIFIER_MODELS`
- `ant-ling.models.ts`: `ANT_LING_MODELS`, `ANT_LING_IMAGE_MODELS`, `ANT_LING_CLASSIFIER_MODELS`
- `anthropic.models.ts`: `ANTHROPIC_MODELS`, `ANTHROPIC_IMAGE_MODELS`, `ANTHROPIC_CLASSIFIER_MODELS`
- `azure.models.ts`: `AZURE_MODELS`, `AZURE_IMAGE_MODELS`, `AZURE_CLASSIFIER_MODELS`
- `baseten.models.ts`: `BASETEN_MODELS`, `BASETEN_IMAGE_MODELS`, `BASETEN_CLASSIFIER_MODELS`
- `cerebras.models.ts`: `CEREBRAS_MODELS`, `CEREBRAS_IMAGE_MODELS`, `CEREBRAS_CLASSIFIER_MODELS`
- `cloudflare-ai-gateway.models.ts`: `CLOUDFLARE_AI_GATEWAY_MODELS`, `CLOUDFLARE_AI_GATEWAY_IMAGE_MODELS`, `CLOUDFLARE_AI_GATEWAY_CLASSIFIER_MODELS`
- `cloudflare-workers-ai.models.ts`: `CLOUDFLARE_WORKERS_AI_MODELS`, `CLOUDFLARE_WORKERS_AI_IMAGE_MODELS`, `CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS`
- `deepseek.models.ts`: `DEEPSEEK_MODELS`, `DEEPSEEK_IMAGE_MODELS`, `DEEPSEEK_CLASSIFIER_MODELS`
- `fireworks.models.ts`: `FIREWORKS_MODELS`, `FIREWORKS_IMAGE_MODELS`, `FIREWORKS_CLASSIFIER_MODELS`
- `github-copilot.models.ts`: `GITHUB_COPILOT_MODELS`, `GITHUB_COPILOT_IMAGE_MODELS`, `GITHUB_COPILOT_CLASSIFIER_MODELS`
- `google-vertex.models.ts`: `GOOGLE_VERTEX_MODELS`, `GOOGLE_VERTEX_IMAGE_MODELS`, `GOOGLE_VERTEX_CLASSIFIER_MODELS`
- `google.models.ts`: `GOOGLE_MODELS`, `GOOGLE_IMAGE_MODELS`, `GOOGLE_CLASSIFIER_MODELS`
- `groq.models.ts`: `GROQ_MODELS`, `GROQ_IMAGE_MODELS`, `GROQ_CLASSIFIER_MODELS`
- `huggingface.models.ts`: `HUGGINGFACE_MODELS`, `HUGGINGFACE_IMAGE_MODELS`, `HUGGINGFACE_CLASSIFIER_MODELS`
- `kimi-coding.models.ts`: `KIMI_CODING_MODELS`, `KIMI_CODING_IMAGE_MODELS`, `KIMI_CODING_CLASSIFIER_MODELS`
- `meta.models.ts`: `META_MODELS`, `META_IMAGE_MODELS`, `META_CLASSIFIER_MODELS`
- `minimax-cn.models.ts`: `MINIMAX_CN_MODELS`, `MINIMAX_CN_IMAGE_MODELS`, `MINIMAX_CN_CLASSIFIER_MODELS`
- `minimax.models.ts`: `MINIMAX_MODELS`, `MINIMAX_IMAGE_MODELS`, `MINIMAX_CLASSIFIER_MODELS`
- `mistral.models.ts`: `MISTRAL_MODELS`, `MISTRAL_IMAGE_MODELS`, `MISTRAL_CLASSIFIER_MODELS`
- `moonshotai-cn.models.ts`: `MOONSHOTAI_CN_MODELS`, `MOONSHOTAI_CN_IMAGE_MODELS`, `MOONSHOTAI_CN_CLASSIFIER_MODELS`
- `moonshotai.models.ts`: `MOONSHOTAI_MODELS`, `MOONSHOTAI_IMAGE_MODELS`, `MOONSHOTAI_CLASSIFIER_MODELS`
- `nvidia.models.ts`: `NVIDIA_MODELS`, `NVIDIA_IMAGE_MODELS`, `NVIDIA_CLASSIFIER_MODELS`
- `openai-codex.models.ts`: `OPENAI_CODEX_MODELS`, `OPENAI_CODEX_IMAGE_MODELS`, `OPENAI_CODEX_CLASSIFIER_MODELS`
- `openai.models.ts`: `OPENAI_MODELS`, `OPENAI_IMAGE_MODELS`, `OPENAI_CLASSIFIER_MODELS`
- `opencode-go.models.ts`: `OPENCODE_GO_MODELS`, `OPENCODE_GO_IMAGE_MODELS`, `OPENCODE_GO_CLASSIFIER_MODELS`
- `opencode.models.ts`: `OPENCODE_MODELS`, `OPENCODE_IMAGE_MODELS`, `OPENCODE_CLASSIFIER_MODELS`
- `openrouter.models.ts`: `OPENROUTER_MODELS`, `OPENROUTER_IMAGE_MODELS`, `OPENROUTER_CLASSIFIER_MODELS`
- `qwen-token-plan-cn.models.ts`: `QWEN_TOKEN_PLAN_CN_MODELS`, `QWEN_TOKEN_PLAN_CN_IMAGE_MODELS`, `QWEN_TOKEN_PLAN_CN_CLASSIFIER_MODELS`
- `qwen-token-plan-individual.models.ts`: `QWEN_TOKEN_PLAN_INDIVIDUAL_MODELS`, `QWEN_TOKEN_PLAN_INDIVIDUAL_IMAGE_MODELS`, `QWEN_TOKEN_PLAN_INDIVIDUAL_CLASSIFIER_MODELS`
- `qwen-token-plan.models.ts`: `QWEN_TOKEN_PLAN_MODELS`, `QWEN_TOKEN_PLAN_IMAGE_MODELS`, `QWEN_TOKEN_PLAN_CLASSIFIER_MODELS`
- `radius.models.ts`: `RADIUS_MODELS`, `RADIUS_IMAGE_MODELS`, `RADIUS_CLASSIFIER_MODELS`
- `together.models.ts`: `TOGETHER_MODELS`, `TOGETHER_IMAGE_MODELS`, `TOGETHER_CLASSIFIER_MODELS`
- `typesafe.models.ts`: `TYPESAFE_MODELS`, `TYPESAFE_IMAGE_MODELS`, `TYPESAFE_CLASSIFIER_MODELS`
- `vercel-ai-gateway.models.ts`: `VERCEL_AI_GATEWAY_MODELS`, `VERCEL_AI_GATEWAY_IMAGE_MODELS`, `VERCEL_AI_GATEWAY_CLASSIFIER_MODELS`
- `xai.models.ts`: `XAI_MODELS`, `XAI_IMAGE_MODELS`, `XAI_CLASSIFIER_MODELS`
- `xiaomi-token-plan-ams.models.ts`: `XIAOMI_TOKEN_PLAN_AMS_MODELS`, `XIAOMI_TOKEN_PLAN_AMS_IMAGE_MODELS`, `XIAOMI_TOKEN_PLAN_AMS_CLASSIFIER_MODELS`
- `xiaomi-token-plan-cn.models.ts`: `XIAOMI_TOKEN_PLAN_CN_MODELS`, `XIAOMI_TOKEN_PLAN_CN_IMAGE_MODELS`, `XIAOMI_TOKEN_PLAN_CN_CLASSIFIER_MODELS`
- `xiaomi-token-plan-sgp.models.ts`: `XIAOMI_TOKEN_PLAN_SGP_MODELS`, `XIAOMI_TOKEN_PLAN_SGP_IMAGE_MODELS`, `XIAOMI_TOKEN_PLAN_SGP_CLASSIFIER_MODELS`
- `xiaomi.models.ts`: `XIAOMI_MODELS`, `XIAOMI_IMAGE_MODELS`, `XIAOMI_CLASSIFIER_MODELS`
- `zai-coding-cn.models.ts`: `ZAI_CODING_CN_MODELS`, `ZAI_CODING_CN_IMAGE_MODELS`, `ZAI_CODING_CN_CLASSIFIER_MODELS`
- `zai.models.ts`: `ZAI_MODELS`, `ZAI_IMAGE_MODELS`, `ZAI_CLASSIFIER_MODELS`

说明（重要）：所有 42 个分片都**导出全部三个常量**（即使内容为空对象），但**并非每个 provider 都使用全部三个**。
例如 `typesafe.ts` 只用 `TYPESAFE_CLASSIFIER_MODELS`、`amazon-bedrock.ts` 只用 `AMAZON_BEDROCK_MODELS`。
Rust 侧若只生成实际使用的常量，则 126 个分片名中只需 42+若干；但若要逐字对齐源码，必须生成 126 个 `static`。

### A.2 `scripts/generate-models.ts` 中前文未列出的常量（31 个）

前文 §2.4.9 已列出一批常量，但以下 31 个顶层 `const` 完全未出现，且它们直接决定生成的模型元数据
（`compat`、`thinkingLevelMap`、`cost`、`headers`），**必须逐字移植**：

| 常量（行号） | 逐字值 |
|---|---|
| `TOGETHER_TOGGLE_REASONING_COMPAT` (179) | `{...TOGETHER_BASE_COMPAT, thinkingFormat: "together"}` |
| `TOGETHER_REASONING_EFFORT_COMPAT` (183) | `{...TOGETHER_BASE_COMPAT, supportsReasoningEffort: true, thinkingFormat: "openai"}` |
| `TOGETHER_TOGGLE_REASONING_EFFORT_COMPAT` (188) | `{...TOGETHER_TOGGLE_REASONING_COMPAT, supportsReasoningEffort: true}` |
| `TOGETHER_REASONING_EFFORT_MODELS` (192) | `Set(["openai/gpt-oss-20b", "openai/gpt-oss-120b"])` |
| `TOGETHER_TOGGLE_REASONING_EFFORT_MODELS` (193) | `Set(["deepseek-ai/DeepSeek-V4-Pro-0813"])` |
| `TOGETHER_REASONING_EFFORT_LEVEL_MAP` (194) | `{off:null, minimal:null}` |
| `TOGETHER_DEEPSEEK_V4_THINKING_LEVEL_MAP` (198) | `{minimal:null, low:null, medium:null, high:"high", xhigh:null}` |
| `TOGETHER_TOGGLE_REASONING_LEVEL_MAP` (205) | `{minimal:null, low:null, medium:null}` |
| `NVIDIA_HEADERS` (218) | `{"NVCF-POLL-SECONDS": "3600"}` |
| `NVIDIA_OPENAI_COMPAT` (221) | `{supportsStore:false, supportsDeveloperRole:false, supportsReasoningEffort:false, maxTokensField:"max_tokens", supportsStrictMode:false, supportsLongCacheRetention:false}` |
| `NVIDIA_NIM_UNSUPPORTED_MODELS` (229) | 18 个 id：`abacusai/dracarys-llama-3.1-70b-instruct`、`bytedance/seed-oss-36b-instruct`、`deepseek-ai/deepseek-v4-flash`、`deepseek-ai/deepseek-v4-pro`、`google/gemma-2-2b-it`、`google/gemma-3n-e2b-it`、`google/gemma-3n-e4b-it`、`google/gemma-4-31b-it`、`meta/llama-3.2-1b-instruct`、`meta/llama-4-maverick-17b-128e-instruct`、`microsoft/phi-4-mini-instruct`、`minimaxai/minimax-m2.7`、`mistralai/mistral-nemotron`、`nvidia/nemotron-mini-4b-instruct`、`qwen/qwen3-next-80b-a3b-instruct`、`qwen/qwen3.5-397b-a17b`、`sarvamai/sarvam-m`、`upstage/solar-10.7b-instruct` |
| `ZAI_TOOL_STREAM_UNSUPPORTED_MODELS` (249) | `Set(["glm-4.5", "glm-4.5-air", "glm-4.5-flash", "glm-4.5v"])` |
| `OPENCODE_GO_GLM52_THINKING_LEVEL_MAP` (250) | `{off:null, minimal:null, low:null, medium:null, high:"high", max:"max"}` |
| `EAGER_TOOL_INPUT_STREAMING_UNSUPPORTED_ANTHROPIC_MODELS` (258) | `Set(["github-copilot:claude-haiku-4.5", "github-copilot:claude-sonnet-4", "github-copilot:claude-sonnet-4.5"])` |
| `ANTHROPIC_ALLOWED_FALLBACK_MODELS` (263) | `{"claude-fable-5": ["claude-opus-4-8", "claude-opus-5"], "claude-opus-5": ["claude-opus-4-8"]}` |
| `DEEPSEEK_V4_THINKING_LEVEL_MAP` (268) | `{minimal:null, low:null, medium:null, high:"high", max:"max"}` |
| `DEEPSEEK_V4_FLASH_THINKING_LEVEL_MAP` (275) | `{...DEEPSEEK_V4_THINKING_LEVEL_MAP, low:"low"}` |
| `AZURE_DEEPSEEK_V4_THINKING_LEVEL_MAP` (280) | `{minimal:null, low:"low", medium:"medium", high:"high", xhigh:null, max:null}` |
| `FIREWORKS_ADAPTIVE_THINKING_FALLBACK_MODELS` (291) | `Set(["accounts/fireworks/models/deepseek-v4-flash-0731", "accounts/fireworks/models/deepseek-v4-flash-vision-exp", "accounts/fireworks/models/deepseek-v4-pro-0813", "accounts/fireworks/models/qwen3p8-max", "accounts/fireworks/models/qwen3p8-2p4t-a95b"])` |
| `QWEN_TOKEN_PLAN_FALLBACK_THINKING_LEVEL_MAP` (298) | `{minimal:null, low:null, medium:null, high:"high", xhigh:null, max:"max"}` |
| `OPENAI_RESPONSES_NONE_REASONING_MODELS` (460) | `Set(["gpt-5.1","gpt-5.2","gpt-5.3-codex","gpt-5.4","gpt-5.4-mini","gpt-5.4-nano","gpt-5.5","gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna","gpt-6-sol","gpt-6-luna"])` |
| `GITHUB_COPILOT_THINKING_LEVEL_OVERRIDES` (512) | `{"claude-opus-4.8": {minimal:"low"}, "claude-opus-5": {minimal:"low"}, "claude-sonnet-4.6": {minimal:"low", max:"max"}}` |
| `VERIFIED_ANTHROPIC_MID_CONVO_EFFORT_PROVIDERS` (597) | `Set(["anthropic", "openrouter"])` |
| `MID_CONVO_EFFORT_UNSUPPORTED_ANTHROPIC_MODELS` (601) | `Set(["openrouter:anthropic/claude-opus-5"])` |
| `ANTHROPIC_DISABLED_THINKING_MODELS` (615) | `Set(["claude-opus-5", "claude-haiku-5-5", "claude-haiku-5.5"])` |
| `OPENAI_COMPLETIONS_DEFAULT_COMPAT` (671) | `{supportsStore:true, supportsDeveloperRole:true, supportsReasoningEffort:true, supportsUsageInStreaming:true, supportsFinishReason:true, maxTokensField:"max_completion_tokens", requiresToolResultName:false, requiresAssistantAfterToolResult:false, requiresThinkingAsText:false, requiresReasoningContentOnAssistantMessages:false, thinkingFormat:"openai", openRouterRouting:{}, vercelGatewayRouting:{}, chatTemplateKwargs:{}, chatTemplateArgs:{}, zaiToolStream:false, supportsStrictMode:false, supportsOpenAIGrammarTools:false, supportsMidConvoSystemMessages:false, supportsMidConvoToolAdditions:false, sendSessionAffinityHeaders:false, supportsLongCacheRetention:true}`（`satisfies Required<Omit<OpenAICompletionsCompat, "cacheControlFormat" \| "supportsThinkingTokenBudget" \| "thinkingTokenBudgetField">> & { cacheControlFormat?: OpenAICompletionsCompat["cacheControlFormat"] }`，紧随其后还有 `type OpenAICompletionsResolvedCompat = typeof OPENAI_COMPLETIONS_DEFAULT_COMPAT & { cacheControlFormat?: ... }`） |
| `OPENAI_GRAMMAR_TOOL_PROVIDERS` (892) | `Set(["openai","openai-codex","azure","github-copilot","opencode","cloudflare-ai-gateway"])` |
| `OPENAI_GRAMMAR_TOOL_APIS` (900) | `Set<Api>(["openai-responses","azure-openai-responses","openai-codex-responses"])` |
| `OPENAI_RESPONSES_PROXY_PROVIDERS` (960) | `Set(["opencode","opencode-go","github-copilot"])` |
| `ANTHROPIC_PROMPT_CACHE` (1008) | `{ short: 300, long: 3600 }`（`ModelPromptCache`） |
| `KIMI_CODING_IMPLIED_COSTS` (1768) | `{ k3: KIMI_K3_COST, "k3-256k": KIMI_K3_COST, "k3.1-pro": {input:2,output:10,cacheRead:0.2,cacheWrite:0}, "k3.1-pro-highspeed": {input:4,output:20,cacheRead:0.2,cacheWrite:0}, "kimi-for-coding": {input:0.95,output:4,cacheRead:0.19,cacheWrite:0}, "kimi-for-coding-highspeed": {input:1.9,output:8,cacheRead:0.38,cacheWrite:0} }` |

另外前文用"见源文件集合"带过的几个集合，此处补全逐字值：
`OPENAI_SHORT_CONTEXT_CAPPED_MODEL_IDS` = `Set(["gpt-5.4","gpt-5.5","gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna","gpt-6-astra","gpt-6-sol","gpt-6-luna","gpt-6.1-sol"])`；
`OPENAI_LONG_CONTEXT_PRICING_MODEL_IDS` = `Set(["gpt-5.4","gpt-5.4-pro","gpt-5.5","gpt-5.5-pro","gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna","gpt-6-astra","gpt-6-sol","gpt-6-luna","gpt-6.1-sol"])`；
`GITHUB_COPILOT_EXTENDED_CONTEXT_MODELS` = `Set(["claude-fable-5","claude-opus-4.8","claude-opus-5","claude-opus-5.5","claude-sonnet-4.6","claude-sonnet-5","gpt-5.3-codex","gpt-6-astra","gpt-6-luna","gpt-6-sol"])`；
`TOGETHER_BASE_COMPAT` = `{supportsStore:false, supportsDeveloperRole:false, supportsReasoningEffort:false, maxTokensField:"max_tokens", supportsStrictMode:false, supportsLongCacheRetention:false}`；
`XAI_RESPONSES_COMPAT` = `{supportsLongCacheRetention:false}`；
`OPENCODE_OPENAI_COMPLETIONS_LONG_CACHE_RETENTION_UNSUPPORTED_MODELS` = `Set(["opencode:deepseek-v4-flash","opencode:deepseek-v4-pro","opencode:kimi-k2.5","opencode:kimi-k2.6","opencode:minimax-m2.7","opencode-go:kimi-k2.6"])`。

### A.2b `generate-models.ts` 的内部函数与类型（前文几乎未提）

前文 §2.4.9 只描述了生成流程的"做什么"，**没有给出 27 个内部函数与 7 个内部类型的名字与签名**。
这些函数决定了每个模型的 `compat`/`thinkingLevelMap`/`cost`/`headers`，Rust 移植必须逐函数对应：

| 函数（行号） | 签名 |
|---|---|
| `applyAnthropicAllowedFallbackModelMetadata` (856) | `(models: readonly Model<"anthropic-messages">[]): void` |
| `applyStrictToolCompatMetadata` (877) | `(model: Model<Api>): void` |
| `applyOpenAIGrammarToolCompatMetadata` (906) | `(model: Model<Api>): void` |
| `applyOpenAIToolSearchMetadata` (913) | `(model: Model<Api>): void` |
| `applyOpenAICompletionsTranscriptMetadata` (932) | `(model: Model<Api>): void` |
| `applyOpenAIResponsesTranscriptMetadata` (962) | `(model: Model<Api>): void` |
| `applyOpenAIExplicitPromptCacheMetadata` (983) | `(model: Model<Api>): void` |
| `applyOpenAIMidConvoEffortMetadata` (995) | `(model: Model<Api>): void` |
| `applyPromptCacheMetadata` (1010) | `(model: Model<Api>): void` |
| `applyImageInputMetadata` (1019) | `(model: AnyModel): void` |
| `isGemma4Model` (1047) | `(modelId: string): boolean` |
| `getGoogleThinkingLevelMap` (1051) | `(modelId: string, reasoningOptions: readonly ModelsDevReasoningOption[]): NonNullable<Model<Api>["thinkingLevelMap"]> \| undefined` |
| `applyThinkingLevelMetadata` (1063) | `(model: Model<any>): void` |
| `getAnthropicMessagesCompat` (1247) | `(provider: string, modelId: string): AnthropicMessagesCompat \| undefined` |
| `getBedrockBaseUrl` (1278) | `(modelId: string): string` |
| `fetchNvidiaNimModelIds` (1317) | `async (): Promise<Map<string, string>>`（id → 原 id，同时写入 normalize 后的键） |
| `fetchOpenRouterList` (1339) | `async (query: string): Promise<OpenRouterModelListItem[]>` |
| `fetchOpenRouterModels` (1346) | `async (): Promise<OpenRouterCatalog>` |
| `fetchRadiusModels` (1369) | `async (): Promise<Model<"kiana-messages">[]>` |
| `fetchAiGatewayModels` (1384) | `async (): Promise<{ chat: Model<any>[]; classifiers: ClassifierModel<"typesafe-system-one">[] }>` |
| `processZaiModels` (1462) | `(data: ModelsDevCatalog): Model<Api>[]` |
| `processBasetenModels` (1517) | `(provider: ModelsDevProvider \| undefined): Model<Api>[]` |
| `processGoogleModels` (1612) | `(data: ModelsDevCatalog): Model<Api>[]` |
| `processFireworksModels` (1685) | `(provider: ModelsDevProvider \| undefined): Model<Api>[]` |
| `createKimiCodingModels` (1778) | `(kimiModels: Record<string, ModelsDevModel>): Model<"openai-completions">[]` |
| `loadModelsDevData` (1821) | `async (): Promise<Model<any>[]>` |
| `loadModelsDevClassifierModels` (2624) | `async (): Promise<ClassifierModel<"typesafe-system-one">[]>` |

内部类型（全部**未导出**）：
- `ModelsDevModel`：`{ id; name; tool_call?; structured_output?; reasoning?; reasoning_options?: ModelsDevReasoningOption[];
  status?; limit?: { context?; output? }; cost?: { input?; output?; cache_read?; cache_write?; tiers?: { input?; output?;
  cache_read?; cache_write?; tier?: { type?; size? } }[] }; modalities?: { input?: string[]; output?: string[] };
  provider?: { npm?: string } }`。
- `ModelsDevProvider`：`{ models?: Record<string, ModelsDevModel> }`。
- `ModelsDevMetadata`：`{ id; type?; name; limit?: { context? }; modalities?: { input?: string[] } }`
  （注意这是**另一套**更窄的元数据形状，与 `ModelsDevModel` 不同）。
- `ModelsDevCatalog`：`Record<string, ModelsDevProvider>`。
- `NvidiaNimModelListItem`：`{ id: string }`。
- `AiGatewayModel`：`{ id; name?; type?; context_window?; max_tokens?; tags?: string[]; pricing?: AiGatewayPricing }`。
- `OpenAICompletionsResolvedCompat`：`typeof OPENAI_COMPLETIONS_DEFAULT_COMPAT & { cacheControlFormat?: OpenAICompletionsCompat["cacheControlFormat"] }`。

另：`OpenRouterModelListItem extends OpenRouterModel`、`OpenRouterCatalog`（`{ chat; images; classifiers }`）、
`buildOpenRouterCatalog(listed, imageListed, decisionListed)` 在 `scripts/openrouter-catalog.ts`（前文已提）。

### A.2c src 中未描述的模块级常量（8 个）

前文没有给出这些**非导出**常量的名字与逐字值，而它们直接影响行为或用户可见输出：

| 常量 | 位置 | 逐字值 / 语义 |
|---|---|---|
| `NODE_FS_SPECIFIER` | `src/env-api-keys.ts:9` | `"node:" + "fs"`（**故意拼接**，避免打包器静态解析） |
| `NODE_OS_SPECIFIER` | `src/env-api-keys.ts:10` | `"node:" + "os"` |
| `NODE_PATH_SPECIFIER` | `src/env-api-keys.ts:11` | `"node:" + "path"` |
| `EXTENDED_THINKING_LEVELS` | `src/models.ts:1259` | `["off", "minimal", "low", "medium", "high", "xhigh", "max"]` 顺序固定；`getSupportedThinkingLevels` 先判 `!model.reasoning -> ["off"]`，再 filter：`thinkingLevelMap[level] === null` 排除；`xhigh`/`max` 仅在显式映射存在时保留；其余默认保留 |
| `OPENROUTER_BASE_URL` | `src/providers/openrouter.ts:13` | `"https://openrouter.ai/api/v1"` |
| `OPENROUTER_USER_MODELS_TIMEOUT_MS` | `src/providers/openrouter.ts:14` | `15_000`（即 15000 ms；用于 `GET /models/user`，超时后回退到公共目录） |
| `NVIDIA_NIM_HOST` | `src/utils/identity.ts:45` | `"integrate.api.nvidia.com"`（用于判断是否附加 `X-BILLING-INVOKE-ORIGIN`） |
| `OPENCODE_HOST` | `src/utils/identity.ts:46` | `"opencode.ai"`（用于判断是否附加 `x-opencode-client`） |

另有 3 个模块级可变缓存（`src/env-api-keys.ts:2-4`）：`_existsSync`、`_homedir`、`_join`，初值 `null`，
在 Node/Bun 环境下**急切** `dynamicImport` 后赋值（`typeof process !== "undefined" && (process.versions?.node || process.versions?.bun)`）。
`type DynamicImport = (specifier: string) => Promise<unknown>` 与 `const dynamicImport: DynamicImport = (specifier) => import(specifier)`
也是内部声明——Rust 侧无需对应，但要知道这是**为浏览器打包**做的惰性 Node 模块加载。

### A.3 生成器控制台输出与降级（前文只列了统计块，逐字补全）

`generate-models.ts` 的 `console.log`（stdout）逐字如下（`<...>` 为插值，不是字面）：

```
Fetching models from NVIDIA NIM API...
Fetched ${data.data?.length ?? 0} model IDs from NVIDIA NIM
Fetching models from OpenRouter API...
Fetched ${catalog.chat.length} tool-capable, ${catalog.images.length} image, and ${catalog.classifiers.length} classifier models from OpenRouter
Fetching models from Radius API...
Fetched ${models.length} models from Radius
Fetching models from Vercel AI Gateway API...
Fetched ${models.length} tool-capable and ${classifiers.length} classifier models from Vercel AI Gateway
Fetching models from models.dev API...
Loaded ${models.length} tool-capable models from models.dev
Fetching classifier models from models.dev API...
Generated provider catalogs and src/models.generated.ts
Generated JSON model values under src/providers/data/       (非 --data-only)
Hydrated JSON model values under src/providers/data/        (--data-only)
Generated JSON model catalog under ${generatorOptions.jsonOutputDir}
```
注意 `fetchOpenRouterModels` / `fetchAiGatewayModels` 的 `console.log` 是**跨行模板字符串**（源码 1355/1451 行），
渲染后为单行；Rust 侧日志文本须与渲染结果一致。

stderr（失败时；非 strict 降级，strict 则重新 throw）：
`Failed to fetch NVIDIA NIM models:`、`Failed to fetch OpenRouter models:`、`Failed to fetch Radius models:`、
`Failed to fetch Vercel AI Gateway models:`、`Failed to load models.dev data:`、`Failed to load models.dev classifier data:`
（均为 `console.error(前缀, error)` **两个参数**，Rust 侧需复现"前缀 + 空格 + 错误 Display"）。

非 strict 的降级值（逐字）：
- NVIDIA：`new Map()`。
- OpenRouter：`{ chat: [], images: [], classifiers: [] }`。
- Radius：`[]`。
- AI Gateway：`{ chat: [], classifiers: [] }`。
- models.dev 与 models.dev classifier：空数组。

另有两处**只在特定条件触发**的 throw（非 strict 也会抛）：
- `throw new Error("OpenRouter API returned no usable image models")` —— 仅在 `strict && catalog.images.length === 0` 时
  （注意 `strict` 前提；非 strict 不抛）。
- `throw new Error("Radius API returned no models")` —— `getRadiusModelsFromConfig` 返回空数组时**无条件抛**
  （随后被 catch 捕获；strict 重新抛出，非 strict 降级为 `[]`）。
- NVIDIA：`throw new Error(\`NVIDIA NIM API returned ${response.status}\`)`；AI Gateway 类似
  `\`Vercel AI Gateway API returned ${response.status}\``；OpenRouter：`\`OpenRouter API returned ${response.status}\``。

生成器顶层 `catch` 为 `console.error(error)`（单参数）+ `process.exitCode = 1`（不是 `process.exit(1)`）。

### A.4 `package.json` 脚本与构建（前文只提了 exports/bin/sideEffects）

脚本（逐字）：
```
generate-models        node scripts/generate-models.ts --strict
hydrate-model-data     node scripts/generate-models.ts --strict --data-only
generate-model-catalog node scripts/generate-models.ts --strict --json-only --json-output ../../.artifacts/model-catalog
check:model-data       node scripts/check-model-data.ts
build                  npm run generate-models && npm run build:offline
build:offline          npm run check:model-data && tsc -p tsconfig.build.json && shx rm -rf dist/providers/data && shx cp -r src/providers/data dist/providers/data
test                   vitest --run
clean                  shx rm -rf dist
prepublishOnly         npm run clean && npm run build
```
包元数据：`name "@earendil-works/kiana-ai"`、`version "1.4.0"`、`type "module"`、`main "./dist/index.js"`、
`types "./dist/index.d.ts"`、`files ["dist","README.md"]`、`engines.node ">=22.19.0"`、`bin { "kiana-ai": "dist/cli.js" }`。
`sideEffects` 只有三项：`./dist/compat.js`、`./dist/images.js`、`./dist/providers/images/register-builtins.js`
——这三个模块的顶层注册副作用**不可被 tree-shake 掉**，Rust 侧对应"模块初始化时注册内置实现"的语义。
`exports` 子路径：`.`、`./models`、`./compat`、`./providers/*`、`./api/*`、`./utils/*`、`./oauth`（还有更多，见前文 §1）。

依赖（版本逐字）：`@anthropic-ai/sdk 0.129.0`、`@aws-sdk/client-bedrock-runtime 3.1127.0`、
`@earendil-works/kiana-telemetry 1.4.0`、`@google/genai 2.21.0`、`@smithy/node-http-handler 4.12.1`、
`http-proxy-agent 9.1.0`、`https-proxy-agent 9.1.0`、`openai 7.19.0`、`partial-json 0.1.7`、`typebox 1.3.27`。
devDependencies：`@types/node 22.19.19`、`canvas 3.2.3`、`vitest 4.1.12`（另有 `shx` 由根 workspace 提供）。

`tsconfig.build.json`：`extends ../../tsconfig.base.json`，`outDir: ./dist`、`rootDir: ./src`、
`module`/`moduleResolution: NodeNext`、`include: ["src/**/*.ts"]`、
`exclude: ["node_modules","dist","**/*.d.ts","src/**/*.d.ts"]`。
`vitest.config.ts`：`globals: true`、`environment: "node"`、`testTimeout: 30000`（注释 `// 30 seconds for API calls`）、
`reporters: process.env.GITHUB_ACTIONS ? ["dot", "github-actions"] : ["dot"]`、`silent: "passed-only"`、
`resolve.alias` 用正则 `/^@earendil-works\/kiana-telemetry$/` 指向 `../telemetry/src/index.ts`（测试用源码而非 dist）。
`src/providers/data/` 被仓库根 `.gitignore` 第 11 行忽略（`packages/ai/src/providers/data/`），
但 `build:offline` 会把它复制进 `dist/`——因此发布包里有数据、仓库里没有。

### A.5 `README.md`（1998 行，前文几乎未覆盖）

`packages/ai/README.md` 在任务范围内，但前文只在 §1 提到一次。它**不是重复规格**，而是对外契约的补充来源；
其中若干细节是规格其他位置没有的。实测结构：25 个 H2、37 个 H3、1998 行。Rust 文档与错误提示宜对齐的要点：

- 25 个 H2 依次为：`Table of Contents`、`Supported Providers`、`Installation`、`Quick Start`、
  `Providers and Models`、`Auth`、`Tools`、`Image Input`、`Image Generation`、`Classification`、
  `Thinking/Reasoning`、`Stop Reasons`、`Telemetry`、`Error Handling`、`Custom Providers`、
  `Faux Provider for Tests`、`Cross-Provider Handoffs`、`System Messages`、`Context Serialization`、
  `Browser Usage`、`Bundling and Tree Shaking`、`OAuth Providers`、`Migrating from the Old Global API`、
  `Development`、`License`。
- `## Supported Providers` 共 **35 个**条目（不是 39）。其中包含规格正文未单独强调的：
  **Radius**（API key 或 OAuth，动态刷新网关目录）、**TypeSafe**（System One classifier API）、
  **OpenAI**（含 Decisions classifier API）、**OpenAI Codex (legacy)**（需 OAuth）、
  **Kimi For Coding**（Moonshot 订阅端点，Anthropic 兼容 API）、**Meta**（Model API，OpenAI Responses 兼容）、
  **Xiaomi MiMo**（默认 API 计费端点，另有 `cn`/`ams`/`sgp` 三个 Token Plan provider）、
  **Any OpenAI-compatible API**（Ollama、vLLM、LM Studio 等）。
- `### Environment Variables`（第 487 行起）是 35 行表格，与 `env-api-keys.ts` 的对应关系：
  - Azure 行：`AZURE_OPENAI_API_KEY` + `AZURE_OPENAI_BASE_URL`（如 `https://{resource}.ai.azure.com`）或
    `AZURE_OPENAI_RESOURCE_NAME`；支持 `*.openai.azure.com`、`*.cognitiveservices.azure.com`、`*.ai.azure.com`；
    根端点自动规范化为 `/openai/v1`；可选 `AZURE_OPENAI_API_VERSION`（默认 `v1`）、`AZURE_OPENAI_DEPLOYMENT_NAME_MAP`。
  - Anthropic 行**只写** `ANTHROPIC_API_KEY` 或 `ANTHROPIC_OAUTH_TOKEN`——**不含** `ANTHROPIC_AUTH_TOKEN`、
    `ANTHROPIC_FEDERATION_RULE_ID`、`ANTHROPIC_ORGANIZATION_ID`、`ANTHROPIC_IDENTITY_TOKEN_FILE`、
    `ANTHROPIC_SERVICE_ACCOUNT_ID`、`ANTHROPIC_WORKSPACE_ID`。这是**文档缺口**，不是实现缺口（见 §2.3.15）。
  - Vertex AI 行：`GOOGLE_CLOUD_API_KEY` 或 `GOOGLE_CLOUD_PROJECT`（或 `GCLOUD_PROJECT`）+
    `GOOGLE_CLOUD_LOCATION` + ADC。
  - Cloudflare 两行都用 `CLOUDFLARE_API_KEY`，AI Gateway 额外需要 `CLOUDFLARE_ACCOUNT_ID` + `CLOUDFLARE_GATEWAY_ID`。
  - `qwen-token-plan-individual` 与 `qwen-token-plan` 共用国际端点与 `QWEN_TOKEN_PLAN_API_KEY`；Individual 只暴露
    其订阅文档中的模型，existing 保留更宽目录以向后兼容。凭证按 provider 作用域存储。
  - 表后两段：Bedrock 解析环境 AWS 凭证（`AWS_PROFILE`、access key 对、`AWS_BEARER_TOKEN_BEDROCK`、ECS task role、
    web identity token）；provider-owned login 支持 bearer token / AWS profile / existing credential chain。
    Vertex AI 支持显式 key 或 gcloud ADC + project/location，provider-owned login 支持 API key / ADC / service account 文件。
- `### Client Identity`（`## Auth` 下）：`identity` 可设在 `createModels()` 或按请求覆盖，字段级合并。
  `userAgent` 三态：`"detailed"`（默认，`kiana/<version> (<platform>; <runtime>; <arch>)`）、`"short"`（`kiana`）、
  `{ custom: "..." }`；Bedrock 上该值追加到 AWS SDK user agent。attribution 映射：
  OpenRouter `HTTP-Referer`/`X-OpenRouter-Title`/`X-OpenRouter-Categories`（逗号分隔）、
  NVIDIA NIM `X-BILLING-INVOKE-ORIGIN`（首字母大写）、OpenCode `x-opencode-client`。
  `KIANA_ATTRIBUTION = { name: "kiana", url: "https://kiana.dev", categories: ["cli-agent"] }`；
  `withClientIdentity(models, identity)` 返回包装器。这些与 `utils/identity.ts` 实现互为契约。
- `### Transforming Request Headers`：`Models.stream/complete/streamSimple/completeSimple` 接受 Models-only 的
  `transformHeaders`，在 provider auth → `model.headers` → `options.headers` **之后**、dispatch **之前**运行一次；
  返回 `null` 可抑制下层默认头。`transformHeaders` 属于 `Models` 而非 `Provider`，传给 provider 前必须剥离。
- `## Browser Usage`：Bedrock 在浏览器不可用（可出现在模型列表、调用时运行期失败）；OAuth 登录仅 Node，
  经 bundler 不透明导入懒加载，注册 OAuth provider 不会把 Node 代码拉进浏览器包。
- `## Context Serialization`：`Context` 可 `JSON.stringify`/`JSON.parse` 往返；模型是纯可序列化数据；
  含 base64 图片时也会被序列化。
- `## Faux Provider for Tests`：明确 "Responses are consumed from a queue in request start order"、
  "Usage is estimated at roughly 1 token per 4 characters"、"By default, each streamed chunk is emitted on its own
  microtask"、"The intended use is one deterministic scripted flow per handle"。
- `## Development` → `### Adding a New Provider` 给出 8 步流程（`src/types.ts` → `src/api/<api-id>.ts` →
  `scripts/generate-models.ts` → `src/providers/<id>.ts` → `test/` → `../coding-agent/` → 文档 → changelog）。
  这是**跨单元契约**：新增 provider 必须同时改 `types.ts` 的 `KnownProvider`、生成器与 coding-agent。
- `## Telemetry`（1118 行）、`## Error Handling`（1152 行）、`## Cross-Provider Handoffs`（1578 行）、
  `## Stop Reasons`（1105 行）、`## System Messages`（1620 行）、`## Migrating from the Old Global API`（1881 行）、
  `## Bundling and Tree Shaking`（1715 行）、`## OAuth Providers`（1789 行，含 `### Vertex AI` 的本地/CI 示例、
  `### CLI Login`、`### Programmatic OAuth`）在前文均无对应段落。
- `## License`：MIT。

### A.6 交互字符串补遗（§3.4 之外）

- `cloudflareWorkersAIAuth` / `cloudflareAIGatewayAuth` 的 `name` 都是 `"Cloudflare API key"`；后者多一个提示
  `Enter Cloudflare AI Gateway ID`（前文 §2.3.8 已有，但 §3.4 未列）。
- `amazonBedrockProvider` 的 apiKey auth `name` 为 `"AWS credentials or bearer token"`；`googleVertexProvider` 为
  `"Google Cloud credentials"`；`anthropicProvider` 为 `"Anthropic API key"`；`fauxProvider` 为 `"Faux"`。
- 全部 `envApiKeyAuth` 的 `name` 与环境变量（36 处调用，逐字；`envApiKeyAuth(name, envVars)` 的 `name` 即登录提示 `Enter ${name}` 的后半段，**前文只举了 5 例**）：

| provider 文件 | 行 | name（逐字） | envVars |
|---|---|---|---|
| `ant-ling.ts` | 11 | `Ant Ling API key` | `"ANT_LING_API_KEY"` |
| `azure.ts` | 48 | `Azure OpenAI API key` | `"AZURE_OPENAI_API_KEY"` |
| `baseten.ts` | 11 | `Baseten API key` | `"BASETEN_API_KEY"` |
| `cerebras.ts` | 11 | `Cerebras API key` | `"CEREBRAS_API_KEY"` |
| `deepseek.ts` | 11 | `DeepSeek API key` | `"DEEPSEEK_API_KEY"` |
| `fireworks.ts` | 12 | `Fireworks API key` | `"FIREWORKS_API_KEY"` |
| `github-copilot.ts` | 15 | `GitHub Copilot token` | `"COPILOT_GITHUB_TOKEN"` |
| `google.ts` | 11 | `Gemini API key` | `"GEMINI_API_KEY"` |
| `groq.ts` | 11 | `Groq API key` | `"GROQ_API_KEY"` |
| `huggingface.ts` | 11 | `Hugging Face token` | `"HF_TOKEN"` |
| `kimi-coding.ts` | 13 | `Kimi API key` | `"KIMI_API_KEY"` |
| `meta.ts` | 13 | `Meta Model API key` | `"META_API_KEY"` |
| `minimax-cn.ts` | 11 | `MiniMax CN API key` | `"MINIMAX_CN_API_KEY"` |
| `minimax.ts` | 11 | `MiniMax API key` | `"MINIMAX_API_KEY"` |
| `mistral.ts` | 11 | `Mistral API key` | `"MISTRAL_API_KEY"` |
| `moonshotai-cn.ts` | 11 | `Moonshot AI API key` | `"MOONSHOT_API_KEY"` |
| `moonshotai.ts` | 11 | `Moonshot AI API key` | `"MOONSHOT_API_KEY"` |
| `nvidia.ts` | 11 | `NVIDIA API key` | `"NVIDIA_API_KEY"` |
| `openai.ts` | 15 | `OpenAI API key` | `"OPENAI_API_KEY"` |
| `opencode-go.ts` | 16 | `OpenCode API key` | `"OPENCODE_API_KEY"` |
| `opencode.ts` | 17 | `OpenCode API key` | `"OPENCODE_API_KEY"` |
| `openrouter.ts` | 68 | `OpenRouter API key` | `"OPENROUTER_API_KEY"` |
| `qwen-token-plan-cn.ts` | 11 | `Qwen Token Plan CN API key` | `"QWEN_TOKEN_PLAN_CN_API_KEY"` |
| `qwen-token-plan-individual.ts` | 11 | `Qwen Token Plan Individual API key` | `"QWEN_TOKEN_PLAN_API_KEY"` |
| `qwen-token-plan.ts` | 11 | `Qwen Token Plan API key` | `"QWEN_TOKEN_PLAN_API_KEY"` |
| `radius.ts` | 39 | `Radius API key` | `"RADIUS_API_KEY"` |
| `together.ts` | 11 | `Together API key` | `"TOGETHER_API_KEY"` |
| `typesafe.ts` | 11 | `TypeSafe API key` | `"TYPESAFE_API_KEY"` |
| `vercel-ai-gateway.ts` | 12 | `Vercel AI Gateway API key` | `"AI_GATEWAY_API_KEY"` |
| `xai.ts` | 13 | `xAI API key` | `"XAI_API_KEY"` |
| `xiaomi-token-plan-ams.ts` | 11 | `Xiaomi Token Plan AMS API key` | `"XIAOMI_TOKEN_PLAN_AMS_API_KEY"` |
| `xiaomi-token-plan-cn.ts` | 11 | `Xiaomi Token Plan CN API key` | `"XIAOMI_TOKEN_PLAN_CN_API_KEY"` |
| `xiaomi-token-plan-sgp.ts` | 11 | `Xiaomi Token Plan SGP API key` | `"XIAOMI_TOKEN_PLAN_SGP_API_KEY"` |
| `xiaomi.ts` | 11 | `Xiaomi API key` | `"XIAOMI_API_KEY"` |
| `zai-coding-cn.ts` | 11 | `Z.AI Coding CN API key` | `"ZAI_CODING_CN_API_KEY"` |
| `zai.ts` | 11 | `Z.AI API key` | `"ZAI_API_KEY"` |

- OAuth `name` 补（逐字）：`Anthropic (Claude Pro/Max)`（anthropic）、`OpenAI (ChatGPT subscription)`（openai）、
  `OpenAI (ChatGPT Plus/Pro)`（openai-codex）、`GitHub Copilot`（github-copilot，oauth 与 provider 同名）、
  `Kimi Code (subscription)`（kimi-coding）、`Meta (Muse subscription)`（meta）、`xAI (Grok/X subscription)`（xai）、
  `OpenRouter OAuth`（openrouter）、`Radius`（radius，可被 `RadiusProviderOptions.name` 覆盖）。
- `loginLabel` 仅 5 处（逐字）：`Sign in with ChatGPT`（openai）、`Sign in with OpenRouter`（openrouter）、
  `Sign in with SuperGrok or X Premium`（xai）、`Sign in with Kimi Code`（kimi-coding）、`Sign in with Meta`（meta）；
  其余 OAuth provider 无 `loginLabel`，CLI 回退显示 `name`。
- `isSubscription: true` 仅 7 处：anthropic、openai、openai-codex、github-copilot、kimi-coding、meta、xai。
- 自定义 `ApiKeyAuth`（非 `envApiKeyAuth`）共 6 处，`name` 分别为：
  `Anthropic API key`（`src/providers/anthropic.ts:21`，login 提示 `Enter Anthropic API key`）、
  `AWS credentials or bearer token`（`src/providers/amazon-bedrock.ts:12`，login 先 `select` 三种方式，提示
  `Select Amazon Bedrock authentication method:`，选项 label `Bearer token`/`AWS profile`/`Existing AWS credential chain`，
  选 bearer 后提示 `Enter Amazon Bedrock bearer token`）、
  `Google Cloud credentials`（`src/providers/google-vertex.ts:14`，login 先 `select`，提示
  `Select Google Vertex AI authentication method:`，选项 `Google Cloud API key`/`Application Default Credentials`/
  `Service account credentials file`，选 api-key 后提示 `Enter Google Cloud API key`）、
  `Cloudflare API key`（`src/providers/cloudflare-auth.ts:56` 与 `:76` 两处，后者多一个 `Enter Cloudflare AI Gateway ID`）、
  `Faux`（`src/providers/faux.ts:713`，`resolve` 直接返回 `{ auth: {} }`）。
- `isSubscription` 仅 7 个：anthropic、openai、openai-codex、github-copilot、kimi-coding、meta、xai
  （与 §3.4 一致；`typesafe`、`opencode` 等无 oauth）。

### A.6b AWS / Google Cloud 自定义凭证链（前文仅有概述）

`amazonBedrockProvider().auth.apiKey`（`name: "AWS credentials or bearer token"`）与
`googleVertexProvider().auth.apiKey`（`name: "Google Cloud credentials"`）都是**手写 `ApiKeyAuth`**，
其 `login` 与 `resolve` 都是多分支状态机，Rust 必须逐支复现。

**Amazon Bedrock `login`**（`src/providers/amazon-bedrock.ts:13`）：
1. `select` 提示 `Select Amazon Bedrock authentication method:`，选项 `{id:"bearer-token",label:"Bearer token"}`、
   `{id:"aws-profile",label:"AWS profile"}`、`{id:"credential-chain",label:"Existing AWS credential chain"}`。
2. 若选 `bearer-token`：`secret` 提示 `Enter Amazon Bedrock bearer token` → `{type:"api_key", key}`。
3. 否则先 `notify({type:"info", message:"Amazon Bedrock supports AWS profiles, IAM credentials, and role-based credentials.",
   links:[{label:"AWS credential provider chain", url:"https://docs.aws.amazon.com/sdkref/latest/guide/standardized-credentials.html"}]})`。
4. 若选 `aws-profile`：`text` 提示 `Enter AWS profile name` → `{type:"api_key", env:{AWS_PROFILE: value}}`。
5. 若剩 `credential-chain`：`text` 提示 `Configure AWS credentials, then press Enter to continue`（值被丢弃）→ `{type:"api_key"}`。
6. 其它值 → `throw new Error(\`Unknown Amazon Bedrock auth method: ${method}\`)`。

**Amazon Bedrock `resolve`**（依次，命中即返回）：
1. `credential?.key` → `{ auth:{apiKey}, env: credential.env, source:"stored credential" }`。
2. `AWS_BEARER_TOKEN_BEDROCK` → `{ auth:{}, source:"AWS_BEARER_TOKEN_BEDROCK" }`（**注意 auth 为空对象，token 由 SDK 自己读环境**）。
3. `credential?.env?.AWS_PROFILE ?? AWS_PROFILE` → `{ auth:{}, env: credential?.env, source: credential?.env?.AWS_PROFILE ? "stored credential" : "AWS_PROFILE" }`。
4. `AWS_ACCESS_KEY_ID` **且** `AWS_SECRET_ACCESS_KEY` → `{ auth:{}, source:"AWS access keys" }`。
5. `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` → `{ auth:{}, source:"ECS task role" }`。
6. `AWS_CONTAINER_CREDENTIALS_FULL_URI` → `{ auth:{}, source:"ECS task role" }`（同一 source 字符串）。
7. `AWS_WEB_IDENTITY_TOKEN_FILE` → `{ auth:{}, source:"web identity token" }`。
8. 否则 `undefined`。
每次 `ctx.env()` 前后都调用 `signal.throwIfAborted()`（Rust 需在每次 await 边界检查取消）。

**Google Vertex `login`**（`src/providers/google-vertex.ts:15`）：
1. `select` 提示 `Select Google Vertex AI authentication method:`，选项 `{id:"api-key",label:"Google Cloud API key"}`、
   `{id:"adc",label:"Application Default Credentials"}`、`{id:"service-account",label:"Service account credentials file"}`。
2. 若 `api-key`：`secret` 提示 `Enter Google Cloud API key` → `{type:"api_key", key}`。
3. 若既非 `adc` 也非 `service-account` → `throw new Error(\`Unknown Google Vertex AI auth method: ${method}\`)`。
4. `notify({type:"info", message: method === "adc" ? "Run \`gcloud auth application-default login\`, then provide the project and location." : "Provide a service account credentials file, project, and location.", links:[{label:"Application Default Credentials", url:"https://cloud.google.com/docs/authentication/provide-credentials-adc"}]})`。
5. `text` 提示 `Enter Google Cloud project ID`、`Enter Google Cloud location`；
   若 `service-account` 再提示 `Enter service account credentials file path`。
6. 返回 `{type:"api_key", env:{ GOOGLE_CLOUD_PROJECT: project, GOOGLE_CLOUD_LOCATION: location,
   ...(credentialsPath ? { GOOGLE_APPLICATION_CREDENTIALS: credentialsPath } : {}) }}`。

**Google Vertex `resolve`**：
1. `key = credential?.key ?? GOOGLE_CLOUD_API_KEY`；有则
   `{ auth:{apiKey}, source: credential?.key ? "stored credential" : "GOOGLE_CLOUD_API_KEY" }`。
2. `adcPath = credential?.env?.GOOGLE_APPLICATION_CREDENTIALS ?? GOOGLE_APPLICATION_CREDENTIALS`；
   `hasCredentials = ctx.fileExists(adcPath ?? "~/.config/gcloud/application_default_credentials.json")`
   （常量 `VERTEX_ADC_PATH = "~/.config/gcloud/application_default_credentials.json"`）。
3. `project = credential?.env?.GOOGLE_CLOUD_PROJECT ?? GOOGLE_CLOUD_PROJECT ?? GCLOUD_PROJECT`；
   `location = credential?.env?.GOOGLE_CLOUD_LOCATION ?? GOOGLE_CLOUD_LOCATION`。
4. 若 `hasCredentials && project && location` → `{ auth:{}, env: credential?.env,
   source: credential ? "stored credential" : "gcloud application default credentials" }`。
5. 否则 `undefined`。
同样每次 `ctx.env()`/`ctx.fileExists()` 前后 `signal.throwIfAborted()`。

> Rust 提示：这两条链的 `source` 字符串是**用户可见的诊断信息**（会出现在错误与日志中），必须逐字一致；
> 且 `AWS_BEARER_TOKEN_BEDROCK` 分支返回**空 auth**——不是把 token 注入 `apiKey`，Rust 侧不要"顺手"填进去。

### A.7 未列出的测试文件（121 个，其中 111 个直接导入本单元模块）

§7.1 的 47 行覆盖了 61 个测试文件；`test/` 下共 **182 个 `.test.ts`**，因此 **121 个未在 §7.1 逐行列出**。
§7.2 把它们整体归为 "`src/api/**`/`src/auth/**`" 的测试，这不完全准确：其中 **111 个直接导入了本单元文件**
（`src/types.ts`、`src/utils/*`、`src/providers/*`、`src/env-api-keys.ts`、`src/images.ts`、`src/models.ts`）。
这些测试是 Rust 移植的**免费黄金样例**。按导入的本单元模块分组（文件名去掉 `.test.ts` 后缀；完整文件名加回即可）：

- 导入 `types.ts` 的：`anthropic-adaptive-thinking-models.test.ts`、`anthropic-empty-thinking-signature-compat.test.ts`、`anthropic-error-tool-result-images.test.ts`、`anthropic-force-adaptive-thinking.test.ts`、`anthropic-mid-conversation-effort.test.ts`、`anthropic-oauth.test.ts`、`anthropic-opus-4-8-smoke.test.ts`、`anthropic-sse-parsing.test.ts`、`anthropic-temperature-compat.test.ts`、`anthropic-thinking-binding-e2e.test.ts`、`anthropic-thinking-disable.test.ts`、`anthropic-tool-name-normalization.test.ts`、`bedrock-custom-headers.test.ts`、`bedrock-endpoint-resolution.test.ts`、`bedrock-models.test.ts`、`bedrock-raw-stop-reason.test.ts`、`bedrock-response-headers.test.ts`、`bedrock-thinking-payload.test.ts`、`cross-provider-handoff.test.ts`、`google-shared-convert-tools.test.ts`、`google-thinking-disable.test.ts`、`image-tool-result.test.ts`、`kimi-coding-oauth.test.ts`、`lax-message-content.test.ts`、`llama-cpp-classify.test.ts`、`meta-oauth.test.ts`、`mistral-tool-schema.test.ts`、`oauth-callback-server.test.ts`、`openai-chatgpt-oauth.test.ts`、`openai-codex-cache-affinity-e2e.test.ts`、`openai-completions-cache-control-format.test.ts`、`openai-completions-prompt-cache.test.ts`、`openai-completions-provider-stream-event.test.ts`、`openai-completions-response-model.test.ts`、`openai-completions-thinking-token-budget.test.ts`、`openai-completions-tool-result-images.test.ts`、`openai-completions-vllm-priority.test.ts`、`openai-decisions.test.ts`、`openai-responses-cache-affinity-e2e.test.ts`、`openai-responses-compat.test.ts`、`openai-responses-empty-tool-result.test.ts`、`openai-responses-message-id.test.ts`、`openai-responses-reasoning-replay-e2e.test.ts`、`radius-oauth.test.ts`、`responseid.test.ts`、`tool-call-id-normalization.test.ts`、`tool-call-without-result.test.ts`、`transform-messages-copilot-openai-to-anthropic.test.ts`、`transform-messages-cross-model-thinking.test.ts`、`typesafe-system-one.test.ts`、`xai-oauth.test.ts`、`xiaomi-token-plan-ams-anthropic-empty-signature-smoke.test.ts`
- 导入 `transcript.ts,types.ts` 的：`anthropic-eager-tool-input-compat.test.ts`、`anthropic-federation-sdk.test.ts`、`anthropic-strict-tool-fallback.test.ts`、`anthropic-strict-tool-schema.test.ts`、`azure-openai-tool-choice.test.ts`、`bedrock-convert-messages.test.ts`、`bedrock-redacted-reasoning.test.ts`、`bedrock-stream-state.test.ts`、`cloudflare-ai-binding.test.ts`、`google-shared-gemini3-unsigned-tool-call.test.ts`、`google-shared-image-tool-result-routing.test.ts`、`google-shared-signed-empty-blocks.test.ts`、`google-thinking-level-map.test.ts`、`mistral-reasoning-mode.test.ts`、`openai-completions-raw-stop-reason.test.ts`、`openai-completions-reasoning-details.test.ts`、`openai-completions-retry.test.ts`、`openai-completions-thinking-as-text.test.ts`、`openai-responses-chatgpt-sign-in.test.ts`、`openai-responses-mid-convo-effort.test.ts`、`openai-responses-usage-limit.test.ts`、`kiana-messages.test.ts`
- 导入 `identity.ts,types.ts` 的：`azure-openai-base-url.test.ts`、`google-raw-stop-reason.test.ts`、`google-vertex-api-key-resolution.test.ts`、`mistral-http-transport.test.ts`
- 导入 `env-api-keys.ts` 的：`baseten-models.test.ts`、`qwen-token-plan-models.test.ts`、`together-models.test.ts`
- 导入 `env-api-keys.ts,types.ts` 的：`anthropic-eager-tool-input-e2e.test.ts`、`anthropic-long-cache-retention-e2e.test.ts`、`radius-thinking-replay-e2e.test.ts`
- 导入 `event-stream.ts,transcript.ts,types.ts` 的：`azure-openai-responses-reasoning-replay.test.ts`、`openai-responses-namespace.test.ts`、`openai-responses-terminal-event.test.ts`
- 导入 `all.ts` 的：`zai-coding-plan-models.test.ts`
- 导入 `all.ts,cloudflare-workers-ai.ts,models.ts,types.ts` 的：`cloudflare-workers-ai-system-one.test.ts`
- 导入 `all.ts,types.ts` 的：`bedrock-credentials.test.ts`
- 导入 `anthropic.ts,env-api-keys.ts,identity.ts,models.ts,transcript.ts,types.ts` 的：`anthropic-auth-token.test.ts`
- 导入 `anthropic.ts,github-copilot.ts,models.ts,openai.ts` 的：`oauth-auth.test.ts`
- 导入 `anthropic.ts,models.ts,transcript.ts,types.ts` 的：`anthropic-federation.test.ts`
- 导入 `azure.ts,types.ts` 的：`azure-openai-completions.test.ts`
- 导入 `cloudflare-stream.ts,event-stream.ts,transcript.ts,types.ts` 的：`cloudflare-stream.test.ts`
- 导入 `cloudflare-workers-ai.ts,models.ts,openrouter.ts,types.ts,typesafe.ts` 的：`classifier-images-live.test.ts`
- 导入 `diagnostics.ts,types.ts` 的：`bedrock-error-metadata.test.ts`
- 导入 `env-api-keys.ts,models.ts,types.ts` 的：`fireworks-models.test.ts`
- 导入 `env-api-keys.ts,typebox-helpers.ts,types.ts` 的：`interleaved-thinking.test.ts`
- 导入 `event-stream.ts,types.ts` 的：`openai-responses-partial-json-cleanup.test.ts`
- 导入 `github-copilot.ts,models.ts` 的：`github-copilot-oauth.test.ts`
- 导入 `hash.ts,types.ts` 的：`openai-responses-foreign-toolcall-id.test.ts`
- 导入 `identity.ts,models.ts,transcript.ts,types.ts,xai.models.ts,xai.ts` 的：`xai-responses.test.ts`
- 导入 `identity.ts,transcript.ts,types.ts` 的：`openai-codex-stream.test.ts`
- 导入 `images.ts,types.ts` 的：`openrouter-images.test.ts`
- 导入 `models.ts` 的：`github-copilot-anthropic.test.ts`
- 导入 `models.ts,openrouter.ts` 的：`openrouter-oauth.test.ts`
- 导入 `models.ts,transcript.ts,types.ts` 的：`fireworks-model-generation.test.ts`
- 导入 `models.ts,types.ts` 的：`xiaomi-models.test.ts`
- 导入 `overflow.ts,transcript.ts,types.ts` 的：`github-copilot-request-size-overflow.test.ts`
- 导入 `retry.ts,types.ts` 的：`mistral-raw-stop-reason.test.ts`

其余 **10 个未列入 §7.1 的测试**不导入任何本单元模块（只测 `src/api/**` 与 `src/auth/**`），逐字为：
`anthropic-cache-write-1h-cost.test.ts`、`bedrock-cache-write-1h-cost.test.ts`、`google-shared-retry.test.ts`、
`google-thinking-signature.test.ts`、`oauth-device-code.test.ts`、`openai-codex-oauth.test.ts`、
`openai-completions-empty-tools.test.ts`、`openai-responses-tool-result-images.test.ts`、
`openrouter-cache-control-models.test.ts`、`openrouter-cache-write-repro.test.ts`。
其中两个 cache-write-1h 与两个 openrouter-cache 测试**间接**验证本单元的 `calculateCost` / `ModelCost` 语义
（1h 缓存写入按 2x 计价），因此仍与 §2.1 的成本模型相关。

测试辅助文件（`test/` 根下 7 个非 `.test.ts`）：
- `azure-utils.ts`（28 行）：`hasAzureOpenAICredentials(): boolean`、`resolveAzureDeploymentName(modelId: string): string | undefined`。
- `bedrock-utils.ts`（18 行）：`hasBedrockCredentials(): boolean`。
- `cloudflare-utils.ts`（9 行）：`hasCloudflareWorkersAICredentials(): boolean`、`hasCloudflareAiGatewayCredentials(): boolean`。
- `codex-websocket-cached-probe.ts`（299 行）：Codex WebSocket 缓存探测，配合 `getOpenAICodexWebSocketDebugStats`。
- `memory-helpers.ts`（39 行）：`collectGarbage(): Promise<void>`、`allocatedBytes(run): Promise<number>`、
  `isCollected(reference: WeakRef<object>): Promise<boolean>`（GC 相关断言用）。
- `oauth.ts`（85 行）：`resolveApiKey(provider: string): Promise<string | undefined>` 等 OAuth 测试辅助。
- `scratch.ts`（57 行）：临时实验脚本，非测试。

数据夹具：`test/data/json-test-suite.json`（与 `utils/json-parse.ts` 的 JSON 修复用例对应）、
`test/data/red-circle.png`（图片输入测试用）。

§7.1 的 47 行（覆盖 61 个文件）**用例数全部与源码一致**（已逐文件核对 `it(`/`test(` 计数，0 处不符）。

### A.8 其它行为细节补遗

1. **`ModelsImpl.generateImages`/`classify`（模型级方法，`src/models.ts:990/1008`）整体包在 `try/catch` 里**：
   `assertImageModel`/`assertClassifierModel`/`assertClassifierInputSupported`、`requireProvider`、
   "不支持"检查、`applyAuth` 与实现调用的**任何**异常都会被吞掉并转成 `imageErrorResult(model, error,
   options?.signal?.aborted)` / `classifierErrorResult(...)`（返回错误结果，**不 reject**）。
   对应错误消息（逐字）：`Provider ${model.provider} does not support image generation`、
   `Provider ${model.provider} does not support classification`。
2. **`createProvider` 只在有实现时才挂 `generateImages`/`classify`，且缺失 api 时返回错误结果**：
   `images && imageImplementations.length > 0` 才定义 `provider.generateImages`，内部 `images[model.api]` 缺失时返回
   `imageErrorResult(model, new ModelsError("provider", \`Provider ${input.id} has no image generation implementation for "${model.api}"\`))`；
   classifier 同理，消息为 `Provider ${input.id} has no classifier implementation for "${model.api}"`。
   **注意两层错误消息不同**：`Models` 层用 `model.provider` + `does not support ...`，`createProvider` 层用
   `input.id` + `has no ... implementation for "<api>"`。
3. **`createProvider` 的 deferred 包装**：仅当 `streams.some(e => e.fetchDeferred !== undefined)` 时才挂
   `provider.fetchDeferred = (model, handle, options) => lazyStream(model, async () => ...)`（因此同步返回流，
   错误从流上以 error 事件出现，消息 `Provider ${input.id} does not support deferred responses for "${model.api}"`）；
   `cancelDeferred` 也在"至少一个条目定义"时才挂，但是**普通 async 函数**（直接 reject），消息为
   `Provider ${input.id} cannot cancel deferred responses for "${model.api}"`（注意是 `cannot ... for`，不是
   `does not support ...`）。
4. **`createProvider` 的 `stream`/`streamSimple` dispatch 用 `lazyStream` 兜底**：`apiFor(model)` 为 undefined 时返回
   `lazyStream(model, async () => { throw new ModelsError("stream", \`Provider ${input.id} has no API implementation for
   "${model.api}"\`) })`——错误码是 **`"stream"`**（不是 `"provider"`），且从流上出现而非同步抛出。
5. **`Models.streamDeferred`（`src/models.ts:952`）也走 `lazyStream`**，内部先 `requireChatProvider`（错误码
   `"provider"`），再检查 `provider.fetchDeferred`，消息 `Provider ${model.provider} does not support deferred responses`；
   `Models.cancelDeferred` 是普通 async，`requireChatProvider` + 同一消息后直接 `await`。
6. **`tools/typesafe.ts` 返回未参数化的 `Provider`**（不是 `Provider<...>`），`models: Object.values(TYPESAFE_CLASSIFIER_MODELS)`。
7. **`opencode-headers.ts` 的 `withOpenCodeSessionHeader` 必须展开 `...streams`**，以保留 `fetchDeferred`/`cancelDeferred`
   等其它键；仅覆盖 `stream`/`streamSimple`。
8. **`cloudflare-stream.ts` 的三个函数**：`resolveCloudflareModel<TModel extends { baseUrl: string }>`、
   `cloudflareStreams(streams: ProviderStreams): ProviderStreams`、
   `cloudflareClassifier(classifier: ProviderClassifier): ProviderClassifier` 均导出；前文只描述行为未给导出名。
   另外 `providers/cloudflare-workers-ai.ts` 的 `api` 字段是**裸的 `cloudflareStreams(openAICompletionsApi())`**
   （不是 `{ "openai-completions": ... }` 对象），而 `classifiers` 是 `{ "cloudflare-workers-ai-system-one":
   cloudflareClassifier(cloudflareWorkersAISystemOneApi()) }`——两者形状不同，Rust 结构体字段须分别建模。
9. **`providers/all.ts` 还 `export { radiusProvider }`**（源码第 48 行，单独一行 re-export，位于 import 块之后），
   因此 `@earendil-works/kiana-ai/providers/all` 暴露 `radiusProvider`；Rust 的 `providers` 模块应同样 re-export。
   注意 `BuiltinProvider = keyof typeof MODELS`，其注释明确：`KnownProvider` 额外包含无静态目录的动态 provider（如
   `"radius"`），而 `BuiltinProvider` 不包含它。
10. **`src/model-catalog.ts` 的 `flattenModelCatalog` 是**私有**实现**（未导出），三个 `flatten*ModelCatalog` 包装它；
   `_provider` 参数运行时未使用（仅类型推断）。
11. **`src/images-api-registry.ts` 的 `ImagesApiProvider` 是 interface（非 type）**，默认泛型参数
   `TApi extends ImageApi = ImageApi`、`TOptions extends ImagesOptions = ImagesOptions`。
12. **`src/image-models.ts` 的 `getImageProviders()` 返回 `BuiltinImageProvider[]`**（不是 `string[]`），
    实现为 `Array.from(imageModelsByProvider.keys()) as BuiltinImageProvider[]`；`getImageModels(provider)` 从
    `imageModelsByProvider.get(provider)` 取值，缺失时返回 `[]`（不是 `undefined`）。三个函数均带 `@deprecated`
    JSDoc，指引改用 `Models.getProviders()` / `getBuiltinImageModels` / `Models.getModelsOfType("image")`。
13. **`index.ts` 的 `export * from "./api/lazy.ts"` 导出 4 个名字**：`LazyStreamObserver`（interface）、
    `lazyStream`（function）、`LazyApiCapabilities`（interface）、`lazyApi`（function），声明顺序即此。
14. **`utils/version.ts` 只有 2 行**：第 1 行注释
    `// The @earendil-works/kiana-ai package version. scripts/sync-versions.js keeps it in sync with package.json.`，
    第 2 行 `export const KIANA_AI_VERSION = "1.4.0";`。Rust 侧应提供同名常量并在构建时与 Cargo 版本同步。
15. **`utils/model-operations.ts` 的 8 个导出**（`getModelType`、`isModelType`、`assertChatModel`、`assertImageModel`、
    `assertClassifierModel`、`assertClassifierInputSupported`、`imageErrorResult`、`classifierErrorResult`）前文已列，
    但**`assertClassifierInputSupported` 只在 `context.images?.length` 为真值时才检查**（`[]`/`undefined` 均放行），
    错误为 `ModelsError("provider", \`Model ${model.provider}/${model.id} does not accept image input\`)`。
    另注意三个 `assert*Model` 的错误码都是 `"provider"`，文本为
    `Model ${model.provider}/${model.id} is not a chat model` / `... is not an image model` / `... is not a classifier model`。
16. **`utils/diagnostics.ts` 的 5 个导出**（`DiagnosticErrorInfo`、`AssistantMessageDiagnostic`、`formatThrownValue`、
    `extractDiagnosticError`、`createAssistantMessageDiagnostic`、`appendAssistantMessageDiagnostic`）——共 6 个，前文
    §2.2.9 已给字段，但 `appendAssistantMessageDiagnostic<T extends { diagnostics?: ... }>` 的泛型约束需保留。
17. **`utils/estimate.ts` 6 个导出**：`ContextUsageEstimate`、`calculateContextTokens`、`estimateTextTokens`、
    `estimateTextAndImageContentTokens`、`estimateMessageTokens`、`estimateContextTokens`。
18. **`ai-core.md` 之外但同目录的 `README.md` 中 `## Telemetry` / `## Error Handling` / `## Cross-Provider Handoffs`
    等章节**给出的行为描述（如 `errorMessage` 截断、handoff 时 thinking 签名处理）与 §2/§3 并不重复，应作为补充阅读。

### A.8b 文件归属更正：`openRouterCost` 与 `utils/openrouter-cost.ts`

前文（§2.2.x 与 §2.3.x）容易混淆两个名字相近的模块：
- **`src/providers/openrouter-api.ts`** 导出 `OpenRouterModel`（type）、`OpenRouterPricing`（type）、
  `openRouterCost(pricing: OpenRouterPricing | undefined): ModelCost` —— 这是把 OpenRouter 的
  `pricing`（字符串/数字混合）转换为 `ModelCost` 的函数，供 `openrouterProvider()` 的 `refreshModels` 使用
  （`src/providers/openrouter.ts:56`：`cost: entry.pricing ? openRouterCost(entry.pricing) : model.cost`）。
- **`src/utils/openrouter-cost.ts`** 导出 `OpenRouterUsageAccounting`（interface）、
  `isOpenRouterModel(model: { provider: string; baseUrl: string }): boolean`、
  `applyOpenRouterReportedCost(usage: Usage, reported: OpenRouterUsageAccounting): void` —— 这是把 OpenRouter
  **响应里回报的实际费用**写回 `usage.cost` 的运行时逻辑。
两者不是同一个文件、不是同一个概念；Rust 侧应分属 `providers::openrouter_api` 与 `utils::openrouter_cost`。

`openRouterCost` 的三条易错规则（Rust 必须逐字复现）：
1. `perMillion(value, fallback)`：`Number.parseFloat(value ?? "")`；只有 `>= 0` 才采用，且为
   `Number((perToken * 1_000_000).toFixed(6))` —— **先乘 1e6 再保留 6 位小数**（不是先舍入再乘）。
   `NaN`（`Number.parseFloat("")`）与负数都走 fallback。OpenRouter 用 `-1` 表示 router 模型（如 `openrouter/auto`）
   的"价格取决于被选中的模型"，因此负数必须被视为缺失。
2. 基础价 `input/output/cacheRead/cacheWrite` 的 fallback 都是 **0**（不是模型原有 cost）。
3. `overrides` 只保留同时满足：`min_prompt_tokens !== undefined` 且 `utc_start/utc_end/utc_days` **全部** undefined
   的条目；转为 `ModelCostTier { inputTokensAbove: min_prompt_tokens, input, output, cacheRead, cacheWrite }`，
   每条 rate 的 fallback 是**对应基础价**（因此未列出的 rate 继承基础价）。带时间维度的 override 被**静默跳过**
   （`ModelCost` 无法表达）。`tiers.length > 0` 才附加 `tiers` 字段。

### A.9 前文错误陈述汇总（本次已在原处修正）

1. `src/compat.ts` 模块级顺序：**`registerBuiltInApiProviders()` 在 `compatModels` 之前**（源码 215 < 217 行），
   不是"文件末尾"。已在 §2.1.15b 修正。
2. §3.3 错误表把 `Provider is not configured: ${model.provider}` 的 code 写成 `provider`，实际是
   **`ModelsError("auth", ...)`**（`src/models.ts:873`）。已修正。
3. §2.1.17 写"13 个 schema + 6 个 interface"，实际 **14 个 const、3 个 type、9 个 interface**。已修正。
4. §3.4 缺 `Sign in with Kimi Code`（kimi-coding）与 `Sign in with Meta`（meta）两个 `loginLabel`。已修正。
5. §2.3.13 `DeferredHandle` 重复块缺 `expiresAt` 与 `data` 字段。已修正。
6. §2.1.2 ContentBlock 家族把 `ThinkingContent` 的字段写成 `signature?`，实际是 **`thinkingSignature?`**；
   同时 `TextContent` 漏了 `textSignature?`，`SystemMessage.content` 被写成可含 `ImageContent`（实际不含）。已修正。
7. §4.1.6 说 `nix/model-catalog.json`"无尾随换行"，实际**末尾有 `\n`**（92 字节，`od -c` 末尾 `"\\n}\\n`）。已修正。
8. §2.4.8 说 `generate-test-image.ts` 32 行：`wc -l` 报 32 是因为**文件末尾没有换行**，实际 33 行内容。已标注。
9. §2.4 开头说"所有脚本以 `#!/usr/bin/env node` 开头"，实际**只有 4 个**有 shebang；其余 5 个是导入模块。已修正。
10. §1.5 的计数：**564 / 438 / 744 / 785 四个数都无法复现**。逐文件重算（不含 `export *` 与带 `from` 的重导出，
    含两处不带 `from` 的 `export {}` 块）得：142 文件 **558** 名（非分片 **432** + 分片 126）；若不含
    `providers/images/` 则 141 文件 **555** 名（非分片 429）；全 `src/**` 203 文件 **730** 名；scripts 合计 **41**
    （24 + 17，无重名）。并集：src + scripts = **771**；本单元 142 文件 + scripts = **599**。已在 §1.5 重写。
11. 多处 utils 的"约 N 行"不准：overflow 193（原写 240）、retry 271（330）、provider-retry 128（170）、
    node-http-proxy 161（190）、oauth-page 109（110 尚可）、identity 193（240）、json-parse 149（130）、
    transcript 263（330）。已改为实测值。
12. `azure.ts` 的行为描述未说明**只有 `openai-completions` 被 `azureStreams` 包裹**，`azure-openai-responses`
    直接用未包裹的实现。已修正。
13. `cloudflare-workers-ai.ts` 的模型列写成 `WORKERS_AI_MODELS + CLASSIFIER`，实际常量是
    `CLOUDFLARE_WORKERS_AI_MODELS` / `CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS`，且 classifier 模型被**展开并入
    chat models 数组**。已修正。
14. `xiaomi*.ts` 表格行的工厂名用 `...CnProvider`/`...SgpProvider` 省略，且 baseUrl 用 `-cn`/`-sgp` 简写；
    现已逐字给出四个文件名、工厂名与完整 baseUrl。
15. §2.3.15（新增）补全 anthropic 的 apiKey resolve 链（含 `ANTHROPIC_AUTH_TOKEN` 走 Bearer 头、以及
    workload identity federation 的 3+2 个环境变量与 `source: "workload identity federation"`）。
16. §6 的 `node:readline` 行把 CLI 提示写成 `Enter number (n): `，实际是 `Enter number (1-${n}): `（**带范围**，
    `src/cli.ts:38/106`）；`undici` 行暗示代码导入 undici，实际**没有**该 import（只是 Node 内置 fetch 实现）。
17. §7 开头把测试辅助函数写成 `gc`，实际导出名是 `collectGarbage`（`test/memory-helpers.ts:9`）。
18. A.2b/A.2c 新增：`generate-models.ts` 的 27 个内部函数 + 7 个内部类型；`src` 的 8 个模块级常量
    （`NODE_FS/OS/PATH_SPECIFIER`、`EXTENDED_THINKING_LEVELS`、`OPENROUTER_BASE_URL`、
    `OPENROUTER_USER_MODELS_TIMEOUT_MS`、`NVIDIA_NIM_HOST`、`OPENCODE_HOST`）与 3 个惰性 Node 模块缓存。
19. A.6b 新增：AWS Bedrock 与 Google Vertex 的完整 login/resolve 状态机（含 `source` 字符串、notify 文案、
    `AWS_BEARER_TOKEN_BEDROCK` 返回**空 auth** 的反直觉行为）。
20. §4.3 的 HTTP 端点表把"方法"列填成了请求头，且缺 OpenRouter/Radius 的错误消息。已改为四列并补全逐字错误文本
    （`OpenRouter model list request failed: <status>`、`Invalid OpenRouter model list response`、
    `Could not load Radius config from <gateway>: <status>: <body>`、`Invalid Radius config from <gateway>`）。
