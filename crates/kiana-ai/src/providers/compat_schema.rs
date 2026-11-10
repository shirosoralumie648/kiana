//! providers/compat-schema.ts：各 API 的 compat TypeBox schema 与 provider 超集。
//!
//! TS: `packages/ai/src/providers/compat-schema.ts`
//!
//! schema 以 TS 侧 `JSON.stringify(schema)` 的逐字快照嵌入（键序、描述、`default` 都不重排）。
//! [`provider_compat_schema`] 用 [`merge_compat_properties`] 从 `CompatSchemasByApi` 去重后的
//! schema 合并，再套 [`provider_compat_property_overrides`]，与 TS 的模块初始化逻辑一致。
//!
//! # 为什么用 JSON 字面量快照
//!
//! 这些 schema 会被 `kiana-config` 直接拼进 `models.json` 的校验 schema（`compat` 字段），
//! 同时 `tests/compat_schema.rs` 会把它与 TS 导出的 fixture 逐字段比较。对象键序、描述
//! 文本、`default` 值都属于可观察结果：用 `serde` 结构体重写会丢失键序与未知字段，还会
//! 改变 TypeBox 的 `anyOf` 形状。因此这里原样嵌入 TS 的输出，交给 `serde_json`
//! （工作区启用了 `preserve_order`）解析成 [`Value`]。
//!
//! # 执行流程
//!
//! 1. 各 `*_schema()` 首次调用时由 `LazyLock` 解析字面量，之后返回同一个 `&'static Value`；
//! 2. [`compat_schemas_by_api`] 按 TS 模块初始化的顺序登记 API → schema：azure 与 codex
//!    复用 openai-responses 的 schema，cloudflare-workers-ai 复用 typesafe-system-one；
//! 3. [`provider_compat_schema`] 先按 `&Value` 的指针身份去重（同一 schema 只合并一次），
//!    再用 [`merge_compat_properties`] 合并各 schema 的 `properties`，最后套用
//!    [`provider_compat_property_overrides`] 里更通用的描述。
//!
//! # 约束与失败模式
//!
//! - 快照损坏（字面量不是合法 JSON）只可能是源码被改坏，`parse_schema` 直接 panic；
//! - 合并时每个重复属性都必须被 overrides 覆盖：缺少覆盖或多出未被重复的属性都返回
//!   `Err(String)`；[`provider_compat_schema`] 把这个 `Err` 当作不变量违例 panic；
//! - 重复属性保留**首次出现**的位置，覆盖值只替换内容、不移动键序——这与 JS 对象赋值
//!   语义一致，键序与 TS 对象赋值的结果相同（供下游拼装 `models.json` 的 schema）；
//!   现有 fixture 只比较值，不校验该键序。

use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::Value;

/// 解析嵌入的 schema 快照（失败即快照损坏，属于不变量）。
fn parse_schema(text: &str) -> Value {
    serde_json::from_str(text).expect("embedded schema snapshot is valid JSON")
}

// ---------------------------------------------------------------------------
// 以下 static 是 TS `JSON.stringify(schema)` 的逐字快照。不要手改字段顺序、描述或
// `default`：任何修改都必须同步重新生成 tests/fixtures/providers/schemas.json，
// 并跑 tests/compat_schema.rs 的逐字段比较。
// ---------------------------------------------------------------------------

static SESSION_AFFINITY_FORMAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"anyOf":[{"type":"string","const":"openai"},{"type":"string","const":"openai-nosession"},{"type":"string","const":"openrouter"}],"description":"Session-affinity header format used to route related requests consistently."}"#,
    )
});

/// TS: `providers/compat-schema.ts#SessionAffinityFormatSchema`
///
/// 会话亲和 header 的三种格式枚举（`openai` / `openai-nosession` / `openrouter`）。
pub fn session_affinity_format_schema() -> &'static Value {
    &SESSION_AFFINITY_FORMAT_SCHEMA_VALUE
}

static THINKING_TOKEN_BUDGET_FIELD_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"anyOf":[{"type":"string","const":"thinking_token_budget"},{"type":"string","const":"thinking_budget"},{"type":"string","const":"thinking_budget_tokens"}],"description":"Top-level request field name used by OpenAI-compatible endpoints to cap reasoning tokens. \"thinking_token_budget\" is used by vLLM, \"thinking_budget\" by Qwen, DashScope, or SGLang, and \"thinking_budget_tokens\" by llama.cpp."}"#,
    )
});

/// TS: `providers/compat-schema.ts#ThinkingTokenBudgetFieldSchema`
///
/// 推理 token 上限字段的三种命名：vLLM 用 `thinking_token_budget`，Qwen/DashScope/SGLang
/// 用 `thinking_budget`，llama.cpp 用 `thinking_budget_tokens`。
pub fn thinking_token_budget_field_schema() -> &'static Value {
    &THINKING_TOKEN_BUDGET_FIELD_SCHEMA_VALUE
}

static CHAT_TEMPLATE_KWARG_VALUE_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"anyOf":[{"type":"string"},{"type":"number"},{"type":"boolean"},{"type":"null"},{"type":"object","required":["$var"],"properties":{"$var":{"anyOf":[{"type":"string","const":"thinking.enabled"},{"type":"string","const":"thinking.effort"},{"type":"string","const":"thinking.budget"}]},"omitWhenOff":{"type":"boolean"}}}]}"#,
    )
});

/// TS: `providers/compat-schema.ts#ChatTemplateKwargValueSchema`
///
/// `chat_template_kwargs` 单个值的联合类型：标量、`null`，或带 `$var` 的占位对象
/// （`$var` 只能是 `thinking.enabled`/`thinking.effort`/`thinking.budget`，由 Kiana 代填）。
pub fn chat_template_kwarg_value_schema() -> &'static Value {
    &CHAT_TEMPLATE_KWARG_VALUE_SCHEMA_VALUE
}

static OPEN_ROUTER_ROUTING_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"allow_fallbacks":{"type":"boolean","description":"Whether to allow backup providers to serve requests.","default":true},"require_parameters":{"type":"boolean","description":"Whether to filter providers to only those that support all parameters in the request.","default":false},"data_collection":{"anyOf":[{"type":"string","const":"deny"},{"type":"string","const":"allow"}],"description":"Data collection setting. \"allow\": allow providers that may store or train on data. \"deny\": only use providers that do not collect user data.","default":"allow"},"zdr":{"type":"boolean","description":"Whether to restrict routing to only ZDR (Zero Data Retention) endpoints."},"enforce_distillable_text":{"type":"boolean","description":"Whether to restrict routing to only models that allow text distillation."},"order":{"type":"array","items":{"type":"string"},"description":"An ordered list of provider names or slugs to try in sequence, falling back to the next if unavailable."},"only":{"type":"array","items":{"type":"string"},"description":"List of provider names or slugs to exclusively allow for this request."},"ignore":{"type":"array","items":{"type":"string"},"description":"List of provider names or slugs to skip for this request."},"quantizations":{"type":"array","items":{"type":"string"},"description":"A list of quantization levels to filter providers by, for example [\"fp16\", \"bf16\", \"fp8\", \"fp6\", \"int8\", \"int4\", \"fp4\", \"fp32\"]."},"sort":{"anyOf":[{"type":"string"},{"type":"object","properties":{"by":{"type":"string","description":"The sorting metric, such as \"price\", \"throughput\", or \"latency\"."},"partition":{"anyOf":[{"type":"string"},{"type":"null"}],"description":"Partitioning strategy: \"model\" or \"none\".","default":"model"}}}],"description":"Sorting strategy. Can be a string such as \"price\", \"throughput\", or \"latency\", or an object."},"max_price":{"type":"object","properties":{"prompt":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per million prompt tokens."},"completion":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per million completion tokens."},"image":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per image."},"audio":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per audio unit."},"request":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per request."}},"description":"Maximum price per million tokens in USD."},"preferred_min_throughput":{"anyOf":[{"type":"number"},{"type":"object","properties":{"p50":{"type":"number","description":"Minimum tokens per second at the 50th percentile."},"p75":{"type":"number","description":"Minimum tokens per second at the 75th percentile."},"p90":{"type":"number","description":"Minimum tokens per second at the 90th percentile."},"p99":{"type":"number","description":"Minimum tokens per second at the 99th percentile."}}}],"description":"Preferred minimum throughput in tokens per second. A number applies to p50."},"preferred_max_latency":{"anyOf":[{"type":"number"},{"type":"object","properties":{"p50":{"type":"number","description":"Maximum latency in seconds at the 50th percentile."},"p75":{"type":"number","description":"Maximum latency in seconds at the 75th percentile."},"p90":{"type":"number","description":"Maximum latency in seconds at the 90th percentile."},"p99":{"type":"number","description":"Maximum latency in seconds at the 99th percentile."}}}],"description":"Preferred maximum latency in seconds. A number applies to p50."}},"description":"OpenRouter provider routing preferences. Controls which upstream providers OpenRouter routes requests to. Sent as the provider field in the OpenRouter API request body. See https://openrouter.ai/docs/guides/routing/provider-selection."}"#,
    )
});

/// TS: `providers/compat-schema.ts#OpenRouterRoutingSchema`
///
/// OpenRouter 请求体 `provider` 字段的路由偏好（回退、排序、价格/吞吐/延迟约束）。
pub fn open_router_routing_schema() -> &'static Value {
    &OPEN_ROUTER_ROUTING_SCHEMA_VALUE
}

static VERCEL_GATEWAY_ROUTING_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"only":{"type":"array","items":{"type":"string"},"description":"List of provider slugs to exclusively use for this request, for example [\"bedrock\", \"anthropic\"]."},"order":{"type":"array","items":{"type":"string"},"description":"List of provider slugs to try in order, for example [\"anthropic\", \"openai\"]."}},"description":"Vercel AI Gateway routing preferences. Controls which upstream providers the gateway routes requests to. See https://vercel.com/docs/ai-gateway/models-and-providers/provider-options."}"#,
    )
});

/// TS: `providers/compat-schema.ts#VercelGatewayRoutingSchema`
///
/// Vercel AI Gateway 的路由偏好，只有 `only`（独占）与 `order`（按序尝试）两个数组。
pub fn vercel_gateway_routing_schema() -> &'static Value {
    &VERCEL_GATEWAY_ROUTING_SCHEMA_VALUE
}

static ANTHROPIC_ALLOWED_FALLBACK_MODEL_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","required":["provider","model","cost"],"properties":{"provider":{"type":"string","minLength":1},"model":{"type":"string","minLength":1},"cost":{"type":"object","required":["input","output","cacheRead","cacheWrite"],"properties":{"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."},"tiers":{"type":"array","items":{"type":"object","required":["inputTokensAbove","input","output","cacheRead","cacheWrite"],"properties":{"inputTokensAbove":{"type":"number","description":"Use this tier when total request input exceeds this token count."},"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."}}},"description":"Request-wide pricing tiers. The highest matching input threshold applies to the full request."}}}},"description":"An Anthropic server-side refusal fallback model with local pricing metadata."}"#,
    )
});

/// TS: `providers/compat-schema.ts#AnthropicAllowedFallbackModelSchema`
///
/// Anthropic 服务端拒绝回退模型的一个条目：`provider`/`model` 非空，`cost` 是四项单价
/// （可带阶梯），用于本地计算回退响应的费用。
pub fn anthropic_allowed_fallback_model_schema() -> &'static Value {
    &ANTHROPIC_ALLOWED_FALLBACK_MODEL_SCHEMA_VALUE
}

static OPEN_AI_COMPLETIONS_COMPAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"supportsStore":{"type":"boolean","description":"Whether the provider supports the store field. Default: auto-detected from URL."},"supportsDeveloperRole":{"type":"boolean","description":"Whether the provider supports the developer role instead of system. Default: auto-detected from URL."},"supportsReasoningEffort":{"type":"boolean","description":"Whether the provider supports reasoning_effort. Default: auto-detected from URL."},"supportsUsageInStreaming":{"type":"boolean","description":"Whether the provider supports stream_options.include_usage for token usage in streaming responses.","default":true},"supportsFinishReason":{"type":"boolean","description":"Whether streamed responses include finish_reason. When false, kiana infers stop or toolUse when the stream ends.","default":true},"maxTokensField":{"anyOf":[{"type":"string","const":"max_completion_tokens"},{"type":"string","const":"max_tokens"}],"description":"Which field to use for max tokens. Default: auto-detected from URL."},"requiresToolResultName":{"type":"boolean","description":"Whether tool results require the name field. Default: auto-detected from URL."},"requiresAssistantAfterToolResult":{"type":"boolean","description":"Whether a user message after tool results requires an assistant message in between. Default: auto-detected from URL."},"requiresThinkingAsText":{"type":"boolean","description":"Whether thinking blocks must be converted to text blocks with <thinking> delimiters. Default: auto-detected from URL."},"requiresReasoningContentOnAssistantMessages":{"type":"boolean","description":"Whether all replayed assistant messages must include an empty reasoning_content field when reasoning is enabled. Default: auto-detected from URL."},"thinkingFormat":{"anyOf":[{"type":"string","const":"openai"},{"type":"string","const":"openrouter"},{"type":"string","const":"deepseek"},{"type":"string","const":"together"},{"type":"string","const":"baseten"},{"type":"string","const":"zai"},{"type":"string","const":"qwen"},{"type":"string","const":"chat-template"},{"type":"string","const":"qwen-chat-template"},{"type":"string","const":"string-thinking"},{"type":"string","const":"ant-ling"}],"description":"Format for reasoning or thinking parameters. When omitted, Kiana auto-detects the format from the provider URL. \"openai\" uses reasoning_effort, \"openrouter\" uses reasoning.effort, \"deepseek\" uses thinking.type plus reasoning_effort when supported, \"together\" uses reasoning.enabled plus reasoning_effort when supported, \"baseten\" uses configurable chat_template_args plus reasoning_effort when supported, \"zai\" uses thinking.type, \"qwen\" uses top-level enable_thinking, \"qwen-chat-template\" uses chat_template_kwargs.enable_thinking and preserve_thinking, \"chat-template\" uses configurable chat_template_kwargs, \"string-thinking\" uses top-level thinking, and \"ant-ling\" uses reasoning.effort only when the mapped effort is non-null."},"chatTemplateKwargs":{"type":"object","patternProperties":{"^.*$":{"anyOf":[{"type":"string"},{"type":"number"},{"type":"boolean"},{"type":"null"},{"type":"object","required":["$var"],"properties":{"$var":{"anyOf":[{"type":"string","const":"thinking.enabled"},{"type":"string","const":"thinking.effort"},{"type":"string","const":"thinking.budget"}]},"omitWhenOff":{"type":"boolean"}}}]}},"description":"Kwargs sent as chat_template_kwargs when thinkingFormat is \"chat-template\". Use $var with \"thinking.enabled\", \"thinking.effort\", or \"thinking.budget\" for kiana-controlled values."},"chatTemplateArgs":{"type":"object","patternProperties":{"^.*$":{"anyOf":[{"type":"string"},{"type":"number"},{"type":"boolean"},{"type":"null"},{"type":"object","required":["$var"],"properties":{"$var":{"anyOf":[{"type":"string","const":"thinking.enabled"},{"type":"string","const":"thinking.effort"},{"type":"string","const":"thinking.budget"}]},"omitWhenOff":{"type":"boolean"}}}]}},"description":"Arguments sent as chat_template_args when thinkingFormat is \"baseten\". Use $var with \"thinking.enabled\", \"thinking.effort\", or \"thinking.budget\" for kiana-controlled values."},"openRouterRouting":{"type":"object","properties":{"allow_fallbacks":{"type":"boolean","description":"Whether to allow backup providers to serve requests.","default":true},"require_parameters":{"type":"boolean","description":"Whether to filter providers to only those that support all parameters in the request.","default":false},"data_collection":{"anyOf":[{"type":"string","const":"deny"},{"type":"string","const":"allow"}],"description":"Data collection setting. \"allow\": allow providers that may store or train on data. \"deny\": only use providers that do not collect user data.","default":"allow"},"zdr":{"type":"boolean","description":"Whether to restrict routing to only ZDR (Zero Data Retention) endpoints."},"enforce_distillable_text":{"type":"boolean","description":"Whether to restrict routing to only models that allow text distillation."},"order":{"type":"array","items":{"type":"string"},"description":"An ordered list of provider names or slugs to try in sequence, falling back to the next if unavailable."},"only":{"type":"array","items":{"type":"string"},"description":"List of provider names or slugs to exclusively allow for this request."},"ignore":{"type":"array","items":{"type":"string"},"description":"List of provider names or slugs to skip for this request."},"quantizations":{"type":"array","items":{"type":"string"},"description":"A list of quantization levels to filter providers by, for example [\"fp16\", \"bf16\", \"fp8\", \"fp6\", \"int8\", \"int4\", \"fp4\", \"fp32\"]."},"sort":{"anyOf":[{"type":"string"},{"type":"object","properties":{"by":{"type":"string","description":"The sorting metric, such as \"price\", \"throughput\", or \"latency\"."},"partition":{"anyOf":[{"type":"string"},{"type":"null"}],"description":"Partitioning strategy: \"model\" or \"none\".","default":"model"}}}],"description":"Sorting strategy. Can be a string such as \"price\", \"throughput\", or \"latency\", or an object."},"max_price":{"type":"object","properties":{"prompt":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per million prompt tokens."},"completion":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per million completion tokens."},"image":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per image."},"audio":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per audio unit."},"request":{"anyOf":[{"type":"number"},{"type":"string"}],"description":"Price per request."}},"description":"Maximum price per million tokens in USD."},"preferred_min_throughput":{"anyOf":[{"type":"number"},{"type":"object","properties":{"p50":{"type":"number","description":"Minimum tokens per second at the 50th percentile."},"p75":{"type":"number","description":"Minimum tokens per second at the 75th percentile."},"p90":{"type":"number","description":"Minimum tokens per second at the 90th percentile."},"p99":{"type":"number","description":"Minimum tokens per second at the 99th percentile."}}}],"description":"Preferred minimum throughput in tokens per second. A number applies to p50."},"preferred_max_latency":{"anyOf":[{"type":"number"},{"type":"object","properties":{"p50":{"type":"number","description":"Maximum latency in seconds at the 50th percentile."},"p75":{"type":"number","description":"Maximum latency in seconds at the 75th percentile."},"p90":{"type":"number","description":"Maximum latency in seconds at the 90th percentile."},"p99":{"type":"number","description":"Maximum latency in seconds at the 99th percentile."}}}],"description":"Preferred maximum latency in seconds. A number applies to p50."}},"description":"OpenRouter provider routing preferences. Controls which upstream providers OpenRouter routes requests to. Sent as the provider field in the OpenRouter API request body. See https://openrouter.ai/docs/guides/routing/provider-selection."},"vercelGatewayRouting":{"type":"object","properties":{"only":{"type":"array","items":{"type":"string"},"description":"List of provider slugs to exclusively use for this request, for example [\"bedrock\", \"anthropic\"]."},"order":{"type":"array","items":{"type":"string"},"description":"List of provider slugs to try in order, for example [\"anthropic\", \"openai\"]."}},"description":"Vercel AI Gateway routing preferences. Controls which upstream providers the gateway routes requests to. See https://vercel.com/docs/ai-gateway/models-and-providers/provider-options."},"zaiToolStream":{"type":"boolean","description":"Whether z.ai supports top-level tool_stream for streaming tool call deltas.","default":false},"thinkingTokenBudgetField":{"anyOf":[{"type":"string","const":"thinking_token_budget"},{"type":"string","const":"thinking_budget"},{"type":"string","const":"thinking_budget_tokens"}],"description":"Top-level request field used to cap reasoning tokens from thinkingBudgets. Reasoning and the answer share max_tokens on these endpoints. \"thinking_token_budget\" is vLLM, \"thinking_budget\" is Qwen, DashScope, or SGLang, and \"thinking_budget_tokens\" is llama.cpp. Off by default and not set on the generated catalog."},"supportsThinkingTokenBudget":{"type":"boolean","description":"Alias for thinkingTokenBudgetField: \"thinking_token_budget\" (vLLM). Prefer thinkingTokenBudgetField.","default":false},"supportsOpenAIGrammarTools":{"type":"boolean","description":"Whether the provider supports OpenAI custom tools with Lark or regex grammar formats. When false, grammar-constrained tools fall back to normal function tools. The generated catalog enables this for capable models.","default":false},"supportsMidConvoSystemMessages":{"type":"boolean","description":"Whether the exact model accepts system or developer messages after the conversation has started. When false, later system messages are folded into the leading system message. The generated catalog enables this for verified models.","default":false},"supportsMidConvoToolAdditions":{"type":"boolean","description":"Whether system messages can introduce additional tools mid-conversation. Requires supportsMidConvoSystemMessages. The generated catalog enables this for capable models.","default":false},"supportsStrictMode":{"type":"boolean","description":"Whether the provider supports the strict field in tool definitions. Generated capable models enable it explicitly.","default":false},"cacheControlFormat":{"type":"string","const":"anthropic","description":"Cache control convention for prompt caching. Anthropic applies cache_control markers to the system prompt, last tool definition, and last user, assistant, or tool-result text content."},"sendSessionAffinityHeaders":{"type":"boolean","description":"Whether to send session-affinity data from options.sessionId. Default: true for OpenRouter endpoints, false otherwise."},"sessionAffinityFormat":{"anyOf":[{"type":"string","const":"openai"},{"type":"string","const":"openai-nosession"},{"type":"string","const":"openrouter"}],"description":"Session-affinity header format. openai sends session_id, x-client-request-id, and x-session-affinity; openai-nosession sends x-client-request-id and x-session-affinity; openrouter sends x-session-id. Does not affect prompt_cache_key. Default: auto-detected."},"supportsLongCacheRetention":{"type":"boolean","description":"Whether the provider supports long prompt cache retention (prompt_cache_retention: \"24h\" or Anthropic-style cache_control.ttl: \"1h\", depending on format). Default: auto-detected from provider and URL."},"vllmPriority":{"type":"number","description":"vLLM scheduler priority sent as the top-level priority request field. Lower values are handled earlier and the server default is 0. Only meaningful with --scheduling-policy priority. Off by default and not set on the generated catalog."}},"description":"Compatibility settings for OpenAI-compatible completions APIs. Use this to override URL-based auto-detection for custom providers.","additionalProperties":true}"#,
    )
});

/// TS: `providers/compat-schema.ts#OpenAICompletionsCompatSchema`
///
/// OpenAI-compatible completions 的 compat 超集，覆盖字段最多（thinking 格式、路由、
/// 缓存、strict tools 等）；`additionalProperties: true`，未知键在配置里保留。
pub fn open_ai_completions_compat_schema() -> &'static Value {
    &OPEN_AI_COMPLETIONS_COMPAT_SCHEMA_VALUE
}

static OPEN_AI_RESPONSES_COMPAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"supportsDeveloperRole":{"type":"boolean","description":"Whether the provider supports the developer role instead of system.","default":true},"supportsMidConvoSystemMessages":{"type":"boolean","description":"Whether the exact model accepts developer or system messages after the conversation has started. When false, later system messages are folded into the leading system message. The generated catalog enables this for verified models.","default":false},"sessionAffinityFormat":{"anyOf":[{"type":"string","const":"openai"},{"type":"string","const":"openai-nosession"},{"type":"string","const":"openrouter"}],"description":"Session-affinity header format. openai sends session_id and x-client-request-id; openai-nosession sends x-client-request-id; openrouter sends x-session-id. Does not affect prompt_cache_key. Default: auto-detected."},"supportsLongCacheRetention":{"type":"boolean","description":"Whether the provider supports long prompt cache retention. This uses prompt_cache_options.ttl: \"30m\" on GPT-5.6+ and prompt_cache_retention: \"24h\" on earlier models.","default":true},"supportsStrictMode":{"type":"boolean","description":"Whether the provider supports strict JSON-schema function tools. Defaults are API-specific; generated OpenAI models enable it explicitly."},"supportsOpenAIGrammarTools":{"type":"boolean","description":"Whether to emit OpenAI custom tools with Lark or regex grammar formats. When false, grammar-constrained tools fall back to normal function tools. The generated catalog enables this for capable models.","default":false},"supportsAdditionalTools":{"type":"boolean","description":"Whether the model supports message-anchored additional_tools input items.","default":false},"supportsToolSearch":{"type":"boolean","description":"Whether the model supports client-executed tool search for transcript-anchored additions.","default":false},"supportsExplicitPromptCacheMode":{"type":"boolean","description":"Whether the model accepts prompt_cache_options. Older OpenAI models reject the parameter.","default":false},"supportsMidConvoEffort":{"type":"boolean","description":"Whether thinking level changes are sent as configuration_update input items instead of changing reasoning.effort, so a change keeps the prompt cache. Only used by the openai-responses API, and ignored with Sign in with ChatGPT on the openai provider.","default":false},"supportsMaxOutputTokens":{"type":"boolean","description":"Whether the provider accepts max_output_tokens. Some Codex-protocol gateways reject it.","default":true}},"description":"Compatibility settings for OpenAI Responses APIs.","additionalProperties":true}"#,
    )
});

/// TS: `providers/compat-schema.ts#OpenAIResponsesCompatSchema`
///
/// OpenAI Responses 的 compat 字段；azure-openai-responses 与 openai-codex-responses
/// 在 [`compat_schemas_by_api`] 里复用同一份 schema。
pub fn open_ai_responses_compat_schema() -> &'static Value {
    &OPEN_AI_RESPONSES_COMPAT_SCHEMA_VALUE
}

static ANTHROPIC_MESSAGES_COMPAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"supportsEagerToolInputStreaming":{"type":"boolean","description":"Whether the provider accepts per-tool eager_input_streaming. When false, the Anthropic provider omits tools[].eager_input_streaming and sends the legacy fine-grained-tool-streaming-2025-05-14 beta header for tool-enabled requests.","default":true},"supportsLongCacheRetention":{"type":"boolean","description":"Whether the provider supports Anthropic long cache retention through cache_control.ttl.","default":true},"sendSessionAffinityHeaders":{"type":"boolean","description":"Whether to send x-session-affinity from options.sessionId when caching is enabled. Required for providers like Fireworks that use session affinity for prompt cache routing; requests to the same replica maximize cache hits. Default: true for OpenRouter endpoints, false otherwise."},"sessionAffinityFormat":{"type":"string","const":"openrouter","description":"Session-affinity format. openrouter sends x-session-id; when unset, sends x-session-affinity."},"supportsCacheControlOnTools":{"type":"boolean","description":"Whether the provider supports Anthropic-style cache_control markers on tool definitions. When false, cache_control is omitted from tool parameters. Some Anthropic-compatible providers, such as Fireworks, do not support this field on tools and may reject or ignore it.","default":true},"supportsTemperature":{"type":"boolean","description":"Whether the model accepts the Anthropic temperature request field. Claude Opus 4.7+ rejects non-default values.","default":true},"forceAdaptiveThinking":{"type":"boolean","description":"Whether to force adaptive thinking (thinking.type: adaptive plus output_config.effort) regardless of model ID. Built-in models that require adaptive thinking set this in generated metadata. Custom Anthropic-compatible providers can set this to true for any model whose upstream requires the adaptive format. Set false to opt out on overridden built-in models.","default":false},"allowEmptySignature":{"type":"boolean","description":"Whether to replay empty thinking signatures instead of converting thinking to text.","default":false},"supportsStrictTools":{"type":"boolean","description":"Whether the provider supports Anthropic strict tool schemas. Generated Anthropic models enable it explicitly.","default":false},"supportsMidConvoEffort":{"type":"boolean","description":"Whether the exact model transport supports effort-only system messages and thinking binding controls.","default":false},"supportsMidConvoSystemMessages":{"type":"boolean","description":"Whether the exact model accepts system-role messages inside the conversation. When false, later system messages are folded into the top-level system prompt.","default":false},"supportsMidConvoToolChanges":{"type":"boolean","description":"Whether the exact model accepts mid-conversation tool_addition and tool_removal blocks. Requires supportsMidConvoSystemMessages.","default":false},"allowedFallbackModels":{"type":"array","items":{"type":"object","required":["provider","model","cost"],"properties":{"provider":{"type":"string","minLength":1},"model":{"type":"string","minLength":1},"cost":{"type":"object","required":["input","output","cacheRead","cacheWrite"],"properties":{"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."},"tiers":{"type":"array","items":{"type":"object","required":["inputTokensAbove","input","output","cacheRead","cacheWrite"],"properties":{"inputTokensAbove":{"type":"number","description":"Use this tier when total request input exceeds this token count."},"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."}}},"description":"Request-wide pricing tiers. The highest matching input threshold applies to the full request."}}}},"description":"An Anthropic server-side refusal fallback model with local pricing metadata."},"maxItems":3,"description":"Models Anthropic accepts for server-side refusal fallback, with local pricing metadata for returned fallback responses. When absent or empty, callers must omit fallbacks; Anthropic rejects the field for models with no permitted fallback targets."}},"description":"Compatibility settings for Anthropic Messages-compatible APIs.","additionalProperties":true}"#,
    )
});

/// TS: `providers/compat-schema.ts#AnthropicMessagesCompatSchema`
///
/// Anthropic Messages 的 compat 字段，含 `allowedFallbackModels`（最多 3 个）与各 provider
/// 常见的兼容开关（eager tool streaming、缓存 TTL、strict tools 等）。
pub fn anthropic_messages_compat_schema() -> &'static Value {
    &ANTHROPIC_MESSAGES_COMPAT_SCHEMA_VALUE
}

static BEDROCK_COMPAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"supportsStrictMode":{"type":"boolean","description":"Whether the model supports Bedrock strict tool schemas.","default":false}},"description":"Compatibility settings for Amazon Bedrock models.","additionalProperties":true}"#,
    )
});

/// TS: `providers/compat-schema.ts#BedrockCompatSchema`
///
/// Bedrock 的 compat 只有一个开关：是否支持 strict tool schema。
pub fn bedrock_compat_schema() -> &'static Value {
    &BEDROCK_COMPAT_SCHEMA_VALUE
}

static MISTRAL_CONVERSATIONS_COMPAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"supportsMidConvoSystemMessages":{"type":"boolean","description":"Whether the exact model accepts system messages after the conversation has started. When false, later system messages are folded into the leading system message.","default":false}},"description":"Compatibility settings for the Mistral chat API.","additionalProperties":true}"#,
    )
});

/// TS: `providers/compat-schema.ts#MistralConversationsCompatSchema`
///
/// Mistral 的 compat 只有一个开关：是否接受对话中途的 system 消息。
pub fn mistral_conversations_compat_schema() -> &'static Value {
    &MISTRAL_CONVERSATIONS_COMPAT_SCHEMA_VALUE
}

static SYSTEM_ONE_COMPAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"imageInput":{"anyOf":[{"type":"string","const":"assets"},{"type":"string","const":"images"},{"type":"string","const":"state-image-urls"}],"description":"How the service takes classifier images. \"assets\" sends TypeSafe's assets map (PNG and JPEG) and references the images from the state, as TypeSafe and Jev on Workers AI expect. \"images\" sends Clef's images list of data URLs (PNG, JPEG, and WebP, at most four), as Clef on Workers AI expects. \"state-image-urls\" places image_url parts before the state, as OpenRouter expects; OpenRouter drops assets without an error. Auto-detected from the provider, baseUrl, and model ID. Classifier models read it from the provider-level compat; models and modelOverrides entries apply to chat models only."},"sendImageOptInHeader":{"type":"boolean","description":"Whether requests with images send the TypeSafe-Danger-Zone-Images: true header, which TypeSafe's own API requires for its experimental image input. Defaults to true for the typesafe-system-one API unless the model is detected as OpenRouter."}},"description":"Compatibility settings for System One classifier APIs.","additionalProperties":true}"#,
    )
});

/// TS: `providers/compat-schema.ts#SystemOneCompatSchema`
///
/// System One 分类器的 compat：图片如何随 state 发送（`assets`/`images`/
/// `state-image-urls`）以及是否附带 TypeSafe 图片 opt-in header。
pub fn system_one_compat_schema() -> &'static Value {
    &SYSTEM_ONE_COMPAT_SCHEMA_VALUE
}

static PROVIDER_COMPAT_PROPERTY_OVERRIDES_VALUE: LazyLock<serde_json::Map<String, Value>> =
    LazyLock::new(|| {
        let mut overrides = serde_json::Map::new();
        // 顺序与 TS 的 `ProviderCompatPropertyOverrides` 一致。
        overrides.insert("supportsDeveloperRole".to_string(), parse_schema(r#"{"type":"boolean","description":"Whether the provider supports the developer role instead of system. Defaults are API-specific."}"#));
        overrides.insert("supportsMidConvoSystemMessages".to_string(), parse_schema(r#"{"type":"boolean","description":"Whether the exact model accepts system or developer messages after the conversation has started. When false, later system messages are folded into the leading system message.","default":false}"#));
        overrides.insert("sessionAffinityFormat".to_string(), parse_schema(r#"{"anyOf":[{"type":"string","const":"openai"},{"type":"string","const":"openai-nosession"},{"type":"string","const":"openrouter"}],"description":"Session-affinity header format. Defaults are API-specific or auto-detected."}"#));
        overrides.insert("supportsMidConvoEffort".to_string(), parse_schema(r#"{"type":"boolean","description":"Whether thinking level changes are sent inside the conversation instead of changing the request's effort, so a change keeps the prompt cache. Used by the anthropic-messages and openai-responses APIs.","default":false}"#));
        overrides.insert("supportsLongCacheRetention".to_string(), parse_schema(r#"{"type":"boolean","description":"Whether the provider supports long prompt cache retention. Defaults are API-specific or auto-detected."}"#));
        overrides.insert("supportsStrictMode".to_string(), parse_schema(r#"{"type":"boolean","description":"Whether the provider supports strict tool schemas. Defaults are API-specific."}"#));
        overrides.insert("supportsOpenAIGrammarTools".to_string(), parse_schema(r#"{"type":"boolean","description":"Whether the provider supports OpenAI custom tools with Lark or regex grammar formats. When false, grammar-constrained tools fall back to normal function tools.","default":false}"#));
        overrides.insert("sendSessionAffinityHeaders".to_string(), parse_schema(r#"{"type":"boolean","description":"Whether to send session-affinity data from options.sessionId. Defaults are API-specific."}"#));
        overrides
    });

/// `ProviderCompatPropertyOverrides`：超集里每个重复属性的通用元数据。
///
/// 键必须与 `CompatSchemasByApi` 中真正重复的属性名一一对应：这些条目整体替换同名属性
/// 的值——不引入新属性，但可能同时改变 `default` 或 schema 形状（如
/// `sessionAffinityFormat` 的 `const` → 三值 `anyOf`）。键序沿用 TS 对象字面量顺序。
///
/// TS: `providers/compat-schema.ts#ProviderCompatPropertyOverrides`（未导出）
pub fn provider_compat_property_overrides() -> &'static serde_json::Map<String, Value> {
    &PROVIDER_COMPAT_PROPERTY_OVERRIDES_VALUE
}

/// `CompatSchemasByApi`：每个接受 compat 的 API 对应的 schema（顺序与 TS 相同）。
///
/// 键是 `model.api` 的逐字值；多个 API 可以指向同一个 `&Value`（指针身份相同），
/// [`provider_compat_schema`] 依赖这一点对 schema 去重。未知 API 不在表中。
///
/// TS: `providers/compat-schema.ts#CompatSchemasByApi`
pub fn compat_schemas_by_api() -> &'static IndexMap<&'static str, &'static Value> {
    static COMPAT_SCHEMAS_BY_API: LazyLock<IndexMap<&'static str, &'static Value>> =
        LazyLock::new(|| {
            let mut schemas = IndexMap::new();
            schemas.insert("openai-completions", open_ai_completions_compat_schema());
            schemas.insert("openai-responses", open_ai_responses_compat_schema());
            schemas.insert("azure-openai-responses", open_ai_responses_compat_schema());
            schemas.insert("openai-codex-responses", open_ai_responses_compat_schema());
            schemas.insert("anthropic-messages", anthropic_messages_compat_schema());
            schemas.insert("bedrock-converse-stream", bedrock_compat_schema());
            schemas.insert(
                "mistral-conversations",
                mistral_conversations_compat_schema(),
            );
            schemas.insert("typesafe-system-one", system_one_compat_schema());
            schemas.insert(
                "cloudflare-workers-ai-system-one",
                system_one_compat_schema(),
            );
            schemas
        });
    &COMPAT_SCHEMAS_BY_API
}

/// `mergeCompatProperties(propertyGroups, overrides)`。
///
/// 输入是若干 schema 的 `properties` map 与 overrides；输出是合并后的 map。
/// 重复属性必须出现在 overrides 里，否则返回
/// `Duplicate compatibility schema property requires an override: {name}`；
/// overrides 里的键必须对应一个重复属性，否则返回
/// `Compatibility schema override does not resolve a duplicate property: {name}`。
///
/// 返回的 map 保留每个属性**首次出现**的位置（`serde_json::Map` + `preserve_order`，
/// 对已存在的键 `insert` 不改变位置），因此结果键序与 TS 的对象赋值一致。
///
/// TS: `providers/compat-schema.ts#mergeCompatProperties`
pub fn merge_compat_properties(
    property_groups: &[&serde_json::Map<String, Value>],
    overrides: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    let mut merged: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut duplicate_names: Vec<String> = Vec::new();

    // 第一遍：按 schema 分组顺序合并；遇到已存在的名字就记为重复，并要求 overrides
    // 已经为它准备了通用元数据（否则说明两个 schema 的描述冲突且无人裁决）。
    for properties in property_groups {
        for (name, schema) in *properties {
            if merged.contains_key(name) {
                if !duplicate_names.contains(name) {
                    duplicate_names.push(name.clone());
                }
                if !overrides.contains_key(name) {
                    return Err(format!(
                        "Duplicate compatibility schema property requires an override: {name}"
                    ));
                }
            }
            // `Map::insert` 对已存在的键保留原位置（与 JS 对象赋值一致）。
            merged.insert(name.clone(), schema.clone());
        }
    }

    // 第二遍：overrides 只能覆盖真实存在的重复属性；多出的键会让合并结果与 TS 不一致，
    // 说明快照与 overrides 没有同步更新。
    for name in overrides.keys() {
        if !duplicate_names.contains(name) {
            return Err(format!(
                "Compatibility schema override does not resolve a duplicate property: {name}"
            ));
        }
    }

    // 第三遍：用通用元数据替换重复属性的内容；键位置保持不变。
    for (name, schema) in overrides {
        merged.insert(name.clone(), schema.clone());
    }
    Ok(merged)
}

/// `ProviderCompatSchema`：API 无关的 compat 超集。
///
/// 根形状固定为 `{type:"object", properties, description, additionalProperties:true}`，
/// `description` 是 `"Provider and model compatibility overrides."`。结果只初始化一次，
/// 之后返回同一 `&'static Value`；合并失败（快照与 overrides 不同步）被视为不变量 panic。
///
/// TS: `providers/compat-schema.ts#ProviderCompatSchema`
pub fn provider_compat_schema() -> &'static Value {
    static PROVIDER_COMPAT_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
        let mut used: Vec<&'static Value> = Vec::new();
        // 同一份 schema 会被多个 API 复用（见 `compat_schemas_by_api`）。按指针身份去重，
        // 保证其 properties 只参与一次合并，不会把属性误判成「重复」。
        for schema in compat_schemas_by_api().values() {
            if !used.iter().any(|existing| std::ptr::eq(*existing, *schema)) {
                used.push(schema);
            }
        }
        let groups: Vec<&serde_json::Map<String, Value>> = used
            .iter()
            .filter_map(|schema| schema.get("properties").and_then(Value::as_object))
            .collect();
        // `merge_compat_properties` 只在「快照与 overrides 不同步」时返回 Err；
        // 那属于源码不变量被破坏，不应在运行期吞掉。
        let properties = merge_compat_properties(&groups, provider_compat_property_overrides())
            .expect("compatibility schema overrides resolve every duplicate property");
        let mut schema = serde_json::Map::new();
        schema.insert("type".into(), Value::String("object".into()));
        schema.insert("properties".into(), Value::Object(properties));
        schema.insert(
            "description".into(),
            Value::String("Provider and model compatibility overrides.".into()),
        );
        schema.insert("additionalProperties".into(), Value::Bool(true));
        Value::Object(schema)
    });
    &PROVIDER_COMPAT_SCHEMA_VALUE
}
