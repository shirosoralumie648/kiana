//! api/openai-completions.ts。
//!
//! OpenAI Chat Completions 兼容适配器。逐条移植 `packages/ai/src/api/openai-completions.ts`：
//! compat 自动检测与覆盖、`reasoning_details` 流式合并、Anthropic 风格 `cache_control`、
//! `chat_template_kwargs` / thinking 字段族、工具调用分片与 grammar 工具。
//!
//! Rust 侧入口（`spec/ai-api-anthropic-openai.md` §1.3/§8.5）：
//! - [`stream`]：完整选项，**同步**返回事件流（请求期失败进入流上的 `error` 事件）；
//! - [`stream_simple`]：简单选项，缺鉴权时返回 `Err`（TS 的同步 throw）；
//! - [`openai_completions_api`]：`lazyApi(() => import(...))` 的 `ProviderStreams` 包装。
//!
//! # 为什么需要这个模块
//!
//! Chat Completions 已是事实上的「兼容协议」：OpenAI 官方、Azure（复用本适配器）、
//! OpenRouter、DeepSeek、Moonshot、z.ai、vLLM 等都暴露同一个 `/chat/completions` 形状，
//! 但思考参数、工具结果格式、缓存标记、token 上限字段名等细节各不相同。本模块用
//! `ResolvedOpenAICompletionsCompat` 把这些差异参数化：[`detect_compat`] 按
//! provider/baseUrl 给出默认值，[`get_compat`] 再用 `model.compat` 的显式字段逐项覆盖。
//!
//! 调用方：`providers/*` 中 api 为 `openai-completions` 的服务商工厂（deepseek、groq、
//! moonshotai、zai、qwen、xiaomi、opencode 等）都通过 [`openai_completions_api`] 取得实现；
//! `compat.rs` 也把它登记进兼容入口表。
//!
//! # 一次请求的执行流程
//!
//! 1. [`stream`] / [`stream_simple`] 在同步阶段返回：前者直接给出事件流，后者给出
//!    `Result`（缺鉴权时是 `Err`）；实际请求在 `spawn_forward` 的后台任务里执行。
//! 2. `run_completions_inner` 依次：取鉴权（`get_client_api_key`）→ 解析 compat → 收集
//!    grammar 工具属性 → 组装 header（`build_headers`）→ 组装请求体（`build_params`，
//!    可被 `on_payload` 整体替换）→ `request_with_retry` 发送。
//! 3. 收到响应先推 `start` 事件，再按 SSE 逐块处理：usage、finish_reason、推理字段、
//!    `reasoning_details`、正文增量、`tool_calls` 分片。
//! 4. 流结束后按内容顺序关闭所有块，校验 stop reason，推 `done` 并结束流。
//! 5. 后台执行阶段任一步出错都由 `run_completions` 统一转成 `error` 事件，并把
//!    `stop_reason` 置为 `Error` 或 `Aborted`（`stream_simple` 的同步鉴权错误除外）。
//!
//! # 关键不变量
//!
//! - 流式状态用**内容块下标**（`usize`）而不是 TS 的对象引用做键
//!   （`spec/ai-api-anthropic-openai.md` §6.3.12）；`by_index` / `by_id` 把后续只带
//!   `index` 或只带 `id` 的分片归并到同一个工具调用块。
//! - `reasoning_details` 的运输方式是 JSON 字符串：流式期间累积成数组，收尾时
//!   `JSON.stringify` 写进 `ThinkingContent::thinking_signature`；下一轮请求再从签名解析回来。
//! - 工具参数在流式期间由 `ToolCallArgumentsStream` / `GrammarToolInputJsonBuffer` 增量解析，
//!   只有 `finish_block` 才写入最终 `arguments` 与 `arguments_error`。
//!
//! # 注意事项
//!
//! - 大量逻辑是「逐字节兼容 TS」：字段写入顺序、`??` 与 `||` 的区别、带工具历史的会话在
//!   没有当前工具声明时仍写 `tools: []`，都会影响快照；改动前先看 `tests/openai_completions_*.rs`
//!   与 `tests/fixtures/api-openai/` 的 golden fixture。
//! - 请求体用 `serde_json::Map`（workspace 启用了 `preserve_order`，保持插入顺序）逐项
//!   写入，不要换成无序容器：快照测试按 JSON 文本逐字节比较。
//! - [`stream`] 会先对 context 调一次 `resolve_transcript`（按 compat 折叠中途 system 消息），
//!   [`convert_messages`] 内部还会对收到的 context 再调用一次；该操作幂等，重复调用无副作用。

// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定（`Display` 必须等于 TS 的
// `error.message`），因此结果类型只按文档要求携带它。
#![allow(clippy::result_large_err)]
use std::collections::HashMap;

use futures::StreamExt;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta,
    close_grammar_tool_input_json, create_grammar_tool_input_properties, get_grammar_tool_input,
    get_json_schema_tool_parameters, resolve_grammar_constrained_sampling,
    resolve_json_schema_strict_sampling,
};
use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::api::lazy::{lazy_api, lazy_stream_error, spawn_forward};
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    RequestFailure, build_url, ensure_response_body, format_thrown_error_with_value, js_stringify,
    json_from_object, json_object, provider_event_stream, request_with_retry, truthy,
};
use crate::api::simple_options::{
    build_base_options, clamp_thinking_budget_to_answer_room, resolve_sampling_params,
    thinking_budget_for_level,
};
use crate::api::transform_messages::{NormalizeToolCallIdFn, transform_messages_with};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::{HttpRequest, SharedHttpClient, json_body};
use crate::models::{ProviderStreams, calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, DoneReason,
    Model, ModelInputModality, ModelThinkingLevel, ProviderResponse, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent, ThinkingLevel, Tool,
    ToolCall, TranscriptContext, Usage, UsageCost, UserContent, UserContentBlock,
};
use crate::utils::error_body::ProviderErrorValue;
use crate::utils::hash::short_hash;
use crate::utils::headers::headers_to_record;
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::openrouter_cost::{
    OpenRouterCostDetails, OpenRouterUsageAccounting, apply_open_router_reported_cost,
    is_open_router_model,
};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{
    TranscriptToolSupport, get_declared_tools, resolve_transcript, resolve_transcript_tools,
};

// ---------------------------------------------------------------------------
// 选项
// ---------------------------------------------------------------------------

/// `OpenAICompletionsOptions`（TS：`StreamOptions` 的扩展）。
///
/// 比 [`StreamOptions`] 多出三个 API 专属字段（`tool_choice`、`reasoning_effort`、
/// `thinking_budgets`），其余通用选项（鉴权、fetch、取消、重试、回调等）都在 `base` 里。
/// `Models` 层通过 `StreamOptions::extra` 传 JSON 时由
/// [`OpenAICompletionsOptions::from_stream_options`] 解出这三个字段；直接调用本模块的
/// 代码则显式构造。
#[derive(Default)]
pub struct OpenAICompletionsOptions {
    /// 全部 [`StreamOptions`] 字段。
    pub base: StreamOptions,
    /// `toolChoice`（任意 OpenAI tool choice 形状）。
    ///
    /// 写入请求体前按 JS 真值过滤：`None`、`null`、`false`、`0`、空串都不发
    /// （TS 是 `if (options?.toolChoice)`；注意与 Responses/Azure 适配器的
    /// `!== undefined` 语义不同）。
    pub tool_choice: Option<Value>,
    /// `reasoningEffort`。
    ///
    /// `None` 表示「不启用思考」。部分方言（zai/deepseek/openrouter 等）在没有该值时
    /// 仍会显式写 `disabled` 或 `effort: "none"`，具体见 `build_params` 的分支。
    pub reasoning_effort: Option<ThinkingLevel>,
    /// `thinkingBudgets`。
    ///
    /// 各档位的思考 token 预算：既用于 `chat-template`/`baseten` 方言的
    /// `$var: "thinking.budget"` 模板值，也在 compat 声明预算字段时写进请求。
    /// 整条预算链都要求 `reasoning_effort` 存在且模型 `reasoning == true`。
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl From<StreamOptions> for OpenAICompletionsOptions {
    fn from(base: StreamOptions) -> Self {
        OpenAICompletionsOptions::from_stream_options(base)
    }
}

impl OpenAICompletionsOptions {
    /// 从 [`StreamOptions`] 的 `extra` 读取 API 专属字段（`Models` 的透传通道）。
    ///
    /// `extra` 是通用 JSON 袋：`toolChoice` 原样克隆；`reasoningEffort` 与
    /// `thinkingBudgets` 用 serde 反序列化，键缺失或类型不符时静默视为 `None`
    /// （`extra` 由上层拼装，未知/异构键必须被忽略，不能因此让请求失败）。
    pub fn from_stream_options(base: StreamOptions) -> Self {
        let tool_choice = base.extra.get("toolChoice").cloned();
        let reasoning_effort = base
            .extra
            .get("reasoningEffort")
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        let thinking_budgets = base
            .extra
            .get("thinkingBudgets")
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        OpenAICompletionsOptions {
            base,
            tool_choice,
            reasoning_effort,
            thinking_budgets,
        }
    }
}

/// `ConvertCompletionsMessagesOptions`。
///
/// 仅供 [`convert_messages`] 的调用方使用：把「grammar 工具名 → 其输入参数名」的映射
/// 带进转换过程。映射由 `create_grammar_tool_input_properties` 依据模型声明的工具生成；
/// `None` 表示不识别 grammar 工具，所有工具调用都按普通 function 序列化。
#[derive(Clone, Default)]
pub struct ConvertCompletionsMessagesOptions {
    /// grammar 工具名 → 输入参数名。
    ///
    /// 值是该工具在 `custom.input` 里承载 JSON 字符串的字段名（默认 `"input"`）。
    /// 命中时工具调用写成 `{"type":"custom","custom":{"name","input"}}`，而不是 function。
    pub grammar_tool_input_properties: Option<IndexMap<String, String>>,
}

// ---------------------------------------------------------------------------
// 鉴权 / header 小工具
// ---------------------------------------------------------------------------

/// `hasHeader(headers, name)`：大小写不敏感、值非空。
fn has_header(headers: Option<&crate::types::ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    let expected = name.to_ascii_lowercase();
    headers.iter().any(|(key, value)| {
        key.to_ascii_lowercase() == expected
            && value
                .as_deref()
                .is_some_and(|value| !kiana_json::js_string::js_trim(value).is_empty())
    })
}

/// `getClientApiKey(provider, apiKey, headers)`。
///
/// 空字符串等同于未提供；显式 `Authorization` 或 `cf-aig-authorization` 头存在时用
/// 占位 key `"unused"`——`build_headers` 仍会先写 `Bearer unused`，真实鉴权头需由
/// `model.headers` 或调用方 headers 覆盖。
/// 注意**不**接受 `x-api-key`（与 Anthropic 适配器不同，见 spec §2.10.3）。
/// 失败返回 [`AiErrorKind::Auth`]，消息逐字为 `No API key for provider: <provider>`。
fn get_client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&crate::types::ProviderHeaders>,
) -> Result<String, AiError> {
    if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
        return Ok(api_key.to_string());
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_string());
    }
    Err(AiError::new(
        AiErrorKind::Auth,
        format!("No API key for provider: {provider}"),
    ))
}

/// `resolveCacheRetention(cacheRetention, env)`。
///
/// 显式值优先；未设置时读环境变量 `KIANA_CACHE_RETENTION`，等于 `"long"` 才用长缓存，
/// 其余（含未设置、空串、其他值）一律 `Short`。`None`（完全关闭）只能由调用方显式传入。
fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&crate::types::ProviderEnv>,
) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention;
    }
    if get_provider_env_value("KIANA_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// `hasToolHistory(messages)`。
fn has_tool_history(messages: &[crate::types::Message]) -> bool {
    messages.iter().any(|message| match message {
        crate::types::Message::ToolResult(_) => true,
        crate::types::Message::Assistant(assistant) => assistant
            .content
            .iter()
            .any(|block| matches!(block, AssistantContentBlock::ToolCall(_))),
        _ => false,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

fn to_model_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

fn from_model_thinking_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    Some(match level {
        ModelThinkingLevel::Off => return None,
        ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ModelThinkingLevel::Low => ThinkingLevel::Low,
        ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ModelThinkingLevel::High => ThinkingLevel::High,
        ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        ModelThinkingLevel::Max => ThinkingLevel::Max,
    })
}

/// thinking level 的字面量（`ThinkingLevel` 的 serde 表示）。
fn thinking_level_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// `model.thinkingLevelMap?.[level]`：`None` = 键不存在；`Some(None)` = `null`。
fn mapped_thinking_level(model: &Model, level: ModelThinkingLevel) -> Option<Option<String>> {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .cloned()
}

/// `model.thinkingLevelMap?.off`。
fn mapped_off(model: &Model) -> Option<Option<String>> {
    mapped_thinking_level(model, ModelThinkingLevel::Off)
}

// ---------------------------------------------------------------------------
// compat
// ---------------------------------------------------------------------------

/// `ResolvedOpenAICompletionsCompat`（TS 的 `Required<...>` 例外项保持可选）。
///
/// 「探测 + 覆盖」后的最终兼容表，字段名沿用 provider 的 camelCase 形态（serde 用 camelCase，缺键取默认值）。
/// 取值优先级：`model.compat` 里的显式字段 > [`detect_compat`] 的启发式默认值。
/// 每个字段只控制请求的一个独立开关，互不推导。
///
/// `Default` 给出「全部关闭」的安全值，但正常路径总是经由 [`get_compat`] 构造；
/// 直接 `Default::default()` 会得到与真实 provider 不符的行为，只适合测试。
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResolvedOpenAICompletionsCompat {
    /// 为 true 时写 `store: false`（本模块从不发 `store: true`）。
    pub supports_store: bool,
    /// system 提示是否可用 `developer` 角色（还需 `model.reasoning == true`）。
    pub supports_developer_role: bool,
    /// 是否允许写 `reasoning_effort`；为 false 时仍可能用 `thinking`/`reasoning` 对象表达档位。
    pub supports_reasoning_effort: bool,
    /// 为 true 时写 `stream_options.include_usage = true`，请求 provider 在流中返回 usage。
    pub supports_usage_in_streaming: bool,
    /// provider 是否保证给 `finish_reason`；为 false 且缺失时按有无工具调用推断
    /// `toolUse`/`stop`，而不是报错。
    pub supports_finish_reason: bool,
    /// `"max_tokens"` 或 `"max_completion_tokens"`。
    pub max_tokens_field: String,
    /// tool 消息是否必须带 `name`（仅当 tool_name 非空时写）。
    pub requires_tool_result_name: bool,
    /// 工具结果后是否必须插一条 assistant 占位消息（文本逐字
    /// `I have processed the tool results.`），用于要求严格交替角色的代理。
    pub requires_assistant_after_tool_result: bool,
    /// 思考内容是否以 text 块回放（拼在正文之前），而不是写成 reasoning 字段。
    pub requires_thinking_as_text: bool,
    /// 仅在模型 `reasoning == true` 且 compat 开启时，为缺少 `reasoning_content` 的
    /// assistant 消息补空串（DeepSeek 等 provider 要求）。
    pub requires_reasoning_content_on_assistant_messages: bool,
    /// 思考参数方言：`zai`/`qwen`/`qwen-chat-template`/`chat-template`/`baseten`/
    /// `deepseek`/`openrouter`/`ant-ling`/`together`/`string-thinking`；
    /// 其余值（含 `openai`）走默认分支。
    pub thinking_format: String,
    /// OpenRouter 路由偏好。`build_params` 实际读的是 `model.compat` 的原始对象
    /// （与 TS 的 `model.compat?.openRouterRouting` 一致），此字段仅作解析结果保留。
    pub open_router_routing: Value,
    /// Vercel AI Gateway 路由偏好；与 `open_router_routing` 一样，实际写请求体时读原始对象。
    pub vercel_gateway_routing: Value,
    /// `chat-template` 方言的键值模板（支持 `$var` 占位与 `omitWhenOff`）。
    pub chat_template_kwargs: Value,
    /// `baseten` 方言的模板，写入请求的 `chat_template_args`。
    pub chat_template_args: Value,
    /// 有工具声明时是否写 `tool_stream: true`（z.ai 的流式工具参数）。
    pub zai_tool_stream: bool,
    /// 三态：`None` 表示未声明（视为不支持）；`Some(true)` 时预算字段名默认
    /// `thinking_token_budget`。
    pub supports_thinking_token_budget: Option<bool>,
    /// 自定义预算字段名，优先于 `supports_thinking_token_budget` 推出的默认名。
    pub thinking_token_budget_field: Option<String>,
    /// 是否支持 JSON Schema strict 模式；为 true 时 function 定义写 `strict`，
    /// 并尝试把 schema 收紧。
    pub supports_strict_mode: bool,
    /// 是否支持 grammar/custom 工具；还需工具本身带 grammar 约束配置才走 custom 分支。
    pub supports_openai_grammar_tools: bool,
    /// 是否接受对话中途的 system 消息；false 时 `resolve_transcript` 把它们折叠到首条。
    pub supports_mid_convo_system_messages: bool,
    /// 是否接受中途追加工具；与上一项同时为 true 才启用
    /// `TranscriptToolSupport::Additions`。
    pub supports_mid_convo_tool_additions: bool,
    /// 仅 `"anthropic"` 时注入 Anthropic 风格 `cache_control` 断点。
    pub cache_control_format: Option<String>,
    /// 是否发送会话亲和 header（还要求 session_id 非空）。
    pub send_session_affinity_headers: bool,
    /// `"openrouter"`：只发 `x-session-id`；`"openai"`：额外发 `session_id`；
    /// 其余值或 `None`：发 `x-client-request-id` + `x-session-affinity`。
    pub session_affinity_format: Option<String>,
    /// 是否支持长缓存：Anthropic 断点带 `ttl: "1h"`，OpenAI 请求带
    /// `prompt_cache_retention: "24h"`。
    pub supports_long_cache_retention: bool,
    /// 非空时写 `priority`（vLLM 调度优先级）。
    pub vllm_priority: Option<f64>,
}

/// 读 `model.compat` 原始对象里的一个键（不做类型转换）。
fn compat_field<'a>(model: &'a Model, key: &str) -> Option<&'a Value> {
    model.compat.as_ref().and_then(|compat| compat.get(key))
}

/// 只接受 JSON 布尔；字符串 `"true"` 等不算覆盖，回落到探测值。
fn compat_bool(model: &Model, key: &str) -> Option<bool> {
    compat_field(model, key).and_then(Value::as_bool)
}

/// 只接受 JSON 字符串；数字/布尔等不算覆盖。
fn compat_str(model: &Model, key: &str) -> Option<String> {
    compat_field(model, key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 键缺失或为 `null` 时返回空对象；空对象是合法的「覆盖为无路由」。
fn compat_value_or_empty_object(model: &Model, key: &str) -> Value {
    match compat_field(model, key) {
        Some(value) if !value.is_null() => value.clone(),
        _ => json_empty_object(),
    }
}

fn json_empty_object() -> Value {
    Value::Object(Map::new())
}

/// `detectCompat(model)`：按 provider 名与 baseUrl 自动检测。
///
/// 这是**启发式默认值**，不是权威声明：判定由「provider 精确匹配 + baseUrl 子串匹配」
/// 组成，且除 DeepSeek 外都按原始大小写匹配。自建反代/私有域名可能探测错，
/// 此时应通过 `model.compat` 显式覆盖（[`get_compat`] 会逐字段合并）。
///
/// 本函数完全不读 `model.compat`；返回的每个字段都只由下面的固定规则决定。
pub fn detect_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();

    // 第一层：按服务商名或域名归组。变量名沿用 TS 的 isXxx，便于与
    // `spec/ai-api-anthropic-openai.md` §2.10.14 逐条对照。
    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_open_router = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_cerebras = provider == "cerebras" || base_url.contains("cerebras.ai");
    let is_deep_seek = provider == "deepseek" || base_url.to_lowercase().contains("deepseek.com");

    // 第二层：由归组推导开关。注意两个方向不同的集合：
    // - `is_non_standard` 是「不符合 OpenAI 官方习惯」的服务商，用来关掉 store/developer role；
    // - `use_max_tokens` 是「只认旧字段名」的服务商，决定写 max_tokens 还是
    //   max_completion_tokens。二者成员不同，不能合并。
    let is_non_standard = is_nvidia
        || is_cerebras
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || is_deep_seek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;

    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deep_seek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;

    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_open_router_developer_role_model =
        is_open_router && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    // cache_control 只在 provider 字段精确为 "openrouter" 且模型是 Anthropic 系时启用；
    // 仅靠 baseUrl 命中 openrouter.ai 不算（需 provider 字段精确匹配）。
    let cache_control_format = (provider == "openrouter" && model.id.starts_with("anthropic/"))
        .then(|| "anthropic".to_string());

    ResolvedOpenAICompletionsCompat {
        // 非标准服务商不发 `store`；developer role 只在 OpenRouter 的 Anthropic/OpenAI
        // 模型上启用，或「标准且非 OpenRouter」的官方形态上启用。
        supports_store: !is_non_standard,
        supports_developer_role: is_open_router_developer_role_model
            || (!is_non_standard && !is_open_router),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            "max_tokens"
        } else {
            "max_completion_tokens"
        }
        .to_string(),
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deep_seek,
        thinking_format: if is_deep_seek {
            "deepseek"
        } else if is_zai {
            "zai"
        } else if is_together {
            "together"
        } else if is_ant_ling {
            "ant-ling"
        } else if is_open_router {
            "openrouter"
        } else {
            "openai"
        }
        .to_string(),
        open_router_routing: json_empty_object(),
        vercel_gateway_routing: json_empty_object(),
        chat_template_kwargs: json_empty_object(),
        chat_template_args: json_empty_object(),
        zai_tool_stream: false,
        supports_thinking_token_budget: Some(false),
        thinking_token_budget_field: None,
        supports_strict_mode: false,
        supports_openai_grammar_tools: false,
        supports_mid_convo_system_messages: false,
        supports_mid_convo_tool_additions: false,
        cache_control_format,
        send_session_affinity_headers: is_open_router,
        session_affinity_format: Some(
            if is_open_router {
                "openrouter"
            } else {
                "openai"
            }
            .to_string(),
        ),
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
        vllm_priority: None,
    }
}

/// `getCompat(model)`：自动检测后用 `model.compat` 逐字段覆盖。
///
/// 覆盖规则（与 TS 的 `??` 链一致）：
/// - 布尔/字符串项：键缺失或类型不符时保持 [`detect_compat`] 的探测值；
/// - 对象项（routing、`chatTemplateKwargs/Args`）：键存在且非 `null` 才整体替换，
///   空对象也是合法覆盖；
/// - `openRouterRouting` 是例外：缺键或 `null` 时回落到空对象（探测值本来也是空对象）。
///
/// 这里不复用 `Default`：每个字段都必须从 `detected` 出发，否则会把「未覆盖」
/// 误当成 `false`/空串。
pub fn get_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let detected = detect_compat(model);
    if model.compat.is_none() {
        return detected;
    }
    // `compat_bool(...).unwrap_or(fallback)`：非 bool 的 JSON 值（如字符串 "true"）也算未覆盖，
    // 不做隐式类型转换。
    let bool_or = |key: &str, fallback: bool| compat_bool(model, key).unwrap_or(fallback);
    ResolvedOpenAICompletionsCompat {
        supports_store: bool_or("supportsStore", detected.supports_store),
        supports_developer_role: bool_or("supportsDeveloperRole", detected.supports_developer_role),
        supports_reasoning_effort: bool_or(
            "supportsReasoningEffort",
            detected.supports_reasoning_effort,
        ),
        supports_usage_in_streaming: bool_or(
            "supportsUsageInStreaming",
            detected.supports_usage_in_streaming,
        ),
        supports_finish_reason: bool_or("supportsFinishReason", detected.supports_finish_reason),
        max_tokens_field: compat_str(model, "maxTokensField").unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: bool_or(
            "requiresToolResultName",
            detected.requires_tool_result_name,
        ),
        requires_assistant_after_tool_result: bool_or(
            "requiresAssistantAfterToolResult",
            detected.requires_assistant_after_tool_result,
        ),
        requires_thinking_as_text: bool_or(
            "requiresThinkingAsText",
            detected.requires_thinking_as_text,
        ),
        requires_reasoning_content_on_assistant_messages: bool_or(
            "requiresReasoningContentOnAssistantMessages",
            detected.requires_reasoning_content_on_assistant_messages,
        ),
        thinking_format: compat_str(model, "thinkingFormat").unwrap_or(detected.thinking_format),
        open_router_routing: compat_value_or_empty_object(model, "openRouterRouting"),
        vercel_gateway_routing: compat_field(model, "vercelGatewayRouting")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or(detected.vercel_gateway_routing),
        chat_template_kwargs: compat_field(model, "chatTemplateKwargs")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or(detected.chat_template_kwargs),
        chat_template_args: compat_field(model, "chatTemplateArgs")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or(detected.chat_template_args),
        zai_tool_stream: bool_or("zaiToolStream", detected.zai_tool_stream),
        supports_thinking_token_budget: compat_bool(model, "supportsThinkingTokenBudget")
            .or(detected.supports_thinking_token_budget),
        thinking_token_budget_field: compat_str(model, "thinkingTokenBudgetField")
            .or(detected.thinking_token_budget_field),
        supports_strict_mode: bool_or("supportsStrictMode", detected.supports_strict_mode),
        supports_openai_grammar_tools: bool_or(
            "supportsOpenAIGrammarTools",
            detected.supports_openai_grammar_tools,
        ),
        supports_mid_convo_system_messages: bool_or(
            "supportsMidConvoSystemMessages",
            detected.supports_mid_convo_system_messages,
        ),
        supports_mid_convo_tool_additions: bool_or(
            "supportsMidConvoToolAdditions",
            detected.supports_mid_convo_tool_additions,
        ),
        cache_control_format: compat_str(model, "cacheControlFormat")
            .or(detected.cache_control_format),
        send_session_affinity_headers: bool_or(
            "sendSessionAffinityHeaders",
            detected.send_session_affinity_headers,
        ),
        session_affinity_format: compat_str(model, "sessionAffinityFormat")
            .or(detected.session_affinity_format),
        supports_long_cache_retention: bool_or(
            "supportsLongCacheRetention",
            detected.supports_long_cache_retention,
        ),
        vllm_priority: compat_field(model, "vllmPriority").and_then(Value::as_f64),
    }
}

// ---------------------------------------------------------------------------
// reasoning_details（解析 / 合并）
// ---------------------------------------------------------------------------

// TS 模块级成员集合（`openai-completions.ts:279`），顺序只影响成员判断。
const OPENAI_COMPLETIONS_REASONING_FIELDS: [&str; 3] =
    ["reasoning", "reasoning_content", "reasoning_text"];

// TS 流式 delta 的本地搜索顺序（`openai-completions.ts:582` 的 `reasoningFields`）：
// 先 `reasoning_content`（llama.cpp），再 `reasoning`。与成员集合顺序刻意不同。
const OPENAI_COMPLETIONS_STREAM_REASONING_FIELDS: [&str; 3] =
    ["reasoning_content", "reasoning", "reasoning_text"];

fn is_openai_completions_reasoning_field(field: &str) -> bool {
    OPENAI_COMPLETIONS_REASONING_FIELDS.contains(&field)
}

/// 公共字段 `id` / `format` / `index` 的类型校验（TS 的
/// `hasValidCommonReasoningDetailFields`）。
///
/// 三个字段都可以缺失；`id` 允许缺失/null/字符串，`format` 只允许缺失/字符串，
/// `index` 只允许缺失/数字（spec §2.10.2）。
/// 校验在按 `type` 分派之前执行，保证后续分支可以安全读取。
fn has_valid_common_reasoning_detail_fields(candidate: &Map<String, Value>) -> bool {
    candidate
        .get("id")
        .is_none_or(|value| value.is_null() || value.is_string())
        && candidate
            .get("format")
            .is_none_or(|value| value.is_string())
        && candidate.get("index").is_none_or(Value::is_number)
}

/// `isOpenAIReasoningDetail(detail)`。
///
/// 先验证公共字段类型，再按 `type` 分派：
/// - `reasoning.summary`：必须带字符串 `summary`；
/// - `reasoning.encrypted`：必须带字符串 `data`；
/// - `reasoning.text`：必须带字符串 `text`，`signature` 可为缺失/null/字符串。
/// 未知 `type` 与非对象一律返回 `false`；校验失败的元素会被流式循环直接跳过。
fn is_openai_reasoning_detail(detail: &Value) -> bool {
    let Some(candidate) = detail.as_object() else {
        return false;
    };
    if !has_valid_common_reasoning_detail_fields(candidate) {
        return false;
    }
    match candidate.get("type").and_then(Value::as_str) {
        Some("reasoning.summary") => candidate.get("summary").is_some_and(Value::is_string),
        Some("reasoning.encrypted") => candidate.get("data").is_some_and(Value::is_string),
        Some("reasoning.text") => {
            candidate.get("text").is_some_and(Value::is_string)
                && candidate
                    .get("signature")
                    .is_none_or(|value| value.is_null() || value.is_string())
        }
        _ => false,
    }
}

/// `parseOpenAIReasoningDetails(signature)`。
///
/// OpenAI 兼容协议把「推理明细数组」塞进 `thinking_signature` 字符串里。本函数是回放方向：
/// 空签名、JSON 解析失败、不是数组、空数组、或任一元素不满足 [`is_openai_reasoning_detail`]
/// 都返回 `None`（整份签名作废，不部分回放）。成功时返回**原始** `Value` 数组，
/// 由 [`convert_messages`] 写回下一轮 assistant 消息的 `reasoning_details`。
fn parse_openai_reasoning_details(signature: Option<&str>) -> Option<Vec<Value>> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    let parsed = kiana_json::js_parse::parse(signature).ok()?;
    let items = parsed.as_array()?;
    if items.is_empty() || !items.iter().all(is_openai_reasoning_detail) {
        return None;
    }
    Some(items.clone())
}

/// `parseLegacyEncryptedReasoningDetail(signature)`。
///
/// 旧格式把单条 `reasoning.encrypted` 明细挂在**工具调用**的 `thought_signature` 上
/// （而不是思考块签名）。这里要求它通过完整校验、`type` 正确，且 `id` 与 `data`
/// 都是非空字符串；否则返回 `None`，该工具的签名被丢弃。
fn parse_legacy_encrypted_reasoning_detail(signature: Option<&str>) -> Option<Value> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    let parsed = kiana_json::js_parse::parse(signature).ok()?;
    if !is_openai_reasoning_detail(&parsed)
        || parsed.get("type").and_then(Value::as_str) != Some("reasoning.encrypted")
    {
        return None;
    }
    let id = parsed.get("id").and_then(Value::as_str)?;
    let data = parsed.get("data").and_then(Value::as_str)?;
    if id.is_empty() || data.is_empty() {
        return None;
    }
    Some(parsed)
}

/// `fillMissingCommonReasoningDetailFields(target, source)`。
///
/// 只填「缺失」的公共字段：`id`/`index` 用 `??=`（仅 `null`/缺失才写），
/// `format` 用 `||=`（空串也会被覆盖）。目标字段已有值时保持不动。
fn fill_missing_common_reasoning_detail_fields(target: &mut Map<String, Value>, source: &Value) {
    let source = source.as_object();
    // `target.id ??= source.id`
    if target.get("id").is_none_or(Value::is_null)
        && let Some(id) = source.and_then(|source| source.get("id"))
    {
        target.insert("id".to_string(), id.clone());
    }
    // `target.format ||= source.format`
    if !truthy(target.get("format"))
        && let Some(format) = source.and_then(|source| source.get("format"))
    {
        target.insert("format".to_string(), format.clone());
    }
    // `target.index ??= source.index`
    if target.get("index").is_none_or(Value::is_null)
        && let Some(index) = source.and_then(|source| source.get("index"))
    {
        target.insert("index".to_string(), index.clone());
    }
}

/// `appendOpenAIReasoningDetail(details, detail)`。
///
/// 相邻的 `reasoning.text` / `reasoning.summary` 会**就地合并**：文本拼接、目标签名为
/// 空时复制源签名（可能是 null/空串，后续分片可再次覆盖）、公共字段只补缺失；
/// `reasoning.encrypted` 永不合并（spec §2.10.2 要求每个
/// 加密条目保持离散）。provider 会把同一段推理切成多个 detail 连续下发，合并后回放
/// 才能还原成一条。
fn append_openai_reasoning_detail(details: &mut Vec<Value>, detail: &Value) {
    let detail_type = detail.get("type").and_then(Value::as_str);
    if detail_type == Some("reasoning.text")
        && let Some(last) = details.last_mut()
        && last.get("type").and_then(Value::as_str) == Some("reasoning.text")
        && let (Some(last), Some(detail)) = (last.as_object_mut(), detail.as_object())
    {
        let appended = format!(
            "{}{}",
            last.get("text").and_then(Value::as_str).unwrap_or(""),
            detail.get("text").and_then(Value::as_str).unwrap_or("")
        );
        last.insert("text".to_string(), Value::String(appended));
        if !truthy(last.get("signature"))
            && let Some(signature) = detail.get("signature")
        {
            last.insert("signature".to_string(), signature.clone());
        }
        fill_missing_common_reasoning_detail_fields(last, &Value::Object(detail.clone()));
        return;
    }
    if detail_type == Some("reasoning.summary")
        && let Some(last) = details.last_mut()
        && last.get("type").and_then(Value::as_str) == Some("reasoning.summary")
        && let (Some(last), Some(detail)) = (last.as_object_mut(), detail.as_object())
    {
        let appended = format!(
            "{}{}",
            last.get("summary").and_then(Value::as_str).unwrap_or(""),
            detail.get("summary").and_then(Value::as_str).unwrap_or("")
        );
        last.insert("summary".to_string(), Value::String(appended));
        fill_missing_common_reasoning_detail_fields(last, &Value::Object(detail.clone()));
        return;
    }
    details.push(detail.clone());
}

// ---------------------------------------------------------------------------
// chat_template kwargs
// ---------------------------------------------------------------------------

/// `resolveChatTemplateKwargValue(model, options, value, thinkingBudget?)`。
///
/// 模板值有三种形态：
/// - 非对象或 `null`：原样返回（`null` 会作为 JSON null 保留，与 JS 的 `undefined` 不同）；
/// - 对象带 `$var`：`thinking.enabled` → 布尔；`thinking.budget` → 预算（没有预算则丢弃该键）；
///   未知 `$var` 不报错，落到下面的档位映射分支（TS 同样未处理 `thinking.effort`）；
/// - 其他对象：按 `thinkingLevelMap` 映射档位（键缺失 → 用原档位字符串；显式 `null` → 丢弃）。
///
/// `omitWhenOff` 为真且未启用思考时整个键被丢弃。
/// 返回 `None` 统一表示「这个键不要写进请求体」。
fn resolve_chat_template_kwarg_value(
    model: &Model,
    reasoning_effort: Option<ThinkingLevel>,
    value: &Value,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    if !value.is_object() || value.is_null() {
        return Some(value.clone());
    }
    if reasoning_effort.is_none() && truthy(value.get("omitWhenOff")) {
        return None;
    }
    match value.get("$var").and_then(Value::as_str) {
        Some("thinking.enabled") => return Some(Value::Bool(reasoning_effort.is_some())),
        Some("thinking.budget") => {
            return thinking_budget.map(Value::from);
        }
        _ => {}
    }
    let mapped = match reasoning_effort {
        Some(level) => mapped_thinking_level(model, to_model_thinking_level(level)),
        None => mapped_off(model),
    };
    match mapped {
        None => reasoning_effort.map(|level| Value::String(thinking_level_str(level).to_string())),
        Some(Some(value)) => Some(Value::String(value)),
        Some(None) => None,
    }
}

/// `buildChatTemplateValues(model, options, values, thinkingBudget?)`。
///
/// 逐键解析模板，被丢弃的键直接省略；一个键都不剩时返回 `None`，
/// 调用方据此**不写** `chat_template_kwargs`/`chat_template_args`（而不是写空对象）。
/// `values` 不是对象时同样返回 `None`；键序沿用源对象的插入顺序。
fn build_chat_template_values(
    model: &Model,
    reasoning_effort: Option<ThinkingLevel>,
    values: &Value,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    let values = values.as_object()?;
    let mut resolved: Map<String, Value> = Map::new();
    for (key, value) in values {
        if let Some(value) =
            resolve_chat_template_kwarg_value(model, reasoning_effort, value, thinking_budget)
        {
            resolved.insert(key.clone(), value);
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(Value::Object(resolved))
    }
}

// ---------------------------------------------------------------------------
// thinking token budget
// ---------------------------------------------------------------------------

/// `resolveThinkingTokenBudgetField(compat)`。
///
/// 显式字段名优先；否则仅在 `supports_thinking_token_budget == Some(true)` 时使用默认名
/// `thinking_token_budget`。仅在没有显式 `thinking_token_budget_field` 时，
/// `supports_thinking_token_budget` 为 `None`/`Some(false)` 才表示不写预算字段。
fn resolve_thinking_token_budget_field(compat: &ResolvedOpenAICompletionsCompat) -> Option<String> {
    if let Some(field) = &compat.thinking_token_budget_field {
        return Some(field.clone());
    }
    if compat.supports_thinking_token_budget == Some(true) {
        return Some("thinking_token_budget".to_string());
    }
    None
}

/// `resolveClampedThinkingBudget(model, options, params)`。
///
/// 前置条件：传了 `reasoning_effort` 且 `model.reasoning == Some(true)`，否则不启用预算。
/// 上限取「已写入 `params` 的 token 字段」（`max_tokens` 或 `max_completion_tokens`），
/// 再回落到 `model.max_tokens`；预算由 `thinking_budget_for_level` 按档位取默认/自定义值
/// （`xhigh`/`max` 在它内部被归一为 `high`），最后用 `clamp_thinking_budget_to_answer_room`
/// 给回答预留空间。夹取后为 0 时返回 `None`，避免写一个无意义的 0 预算。
fn resolve_clamped_thinking_budget(
    model: &Model,
    options: &OpenAICompletionsOptions,
    params: &Map<String, Value>,
) -> Option<u64> {
    let reasoning_effort = options.reasoning_effort?;
    if model.reasoning != Some(true) {
        return None;
    }
    let ceiling = params
        .get("max_tokens")
        .and_then(Value::as_u64)
        .or_else(|| params.get("max_completion_tokens").and_then(Value::as_u64))
        .or(model.max_tokens)?;
    let budget = clamp_thinking_budget_to_answer_room(
        thinking_budget_for_level(reasoning_effort, options.thinking_budgets.as_ref()),
        ceiling,
    );
    (budget > 0).then_some(budget)
}

// ---------------------------------------------------------------------------
// Anthropic 风格 cache_control
// ---------------------------------------------------------------------------

/// `OpenAICompatCacheControl`。
///
/// 只有 compat 明确声明 `cacheControlFormat == "anthropic"` 且本次请求启用缓存
/// （`cacheRetention != None`）才生成断点对象；长缓存且 provider 支持时额外带
/// `ttl: "1h"`，否则只有 `{"type":"ephemeral"}`。
fn compat_cache_control(
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: CacheRetention,
) -> Option<Value> {
    if compat.cache_control_format.as_deref() != Some("anthropic")
        || cache_retention == CacheRetention::None
    {
        return None;
    }
    let ttl = (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention)
        .then_some("1h");
    let mut cache_control = Map::new();
    cache_control.insert("type".into(), Value::String("ephemeral".into()));
    if let Some(ttl) = ttl {
        cache_control.insert("ttl".into(), Value::String(ttl.into()));
    }
    Some(Value::Object(cache_control))
}

/// `addCacheControlToTextContent(message, cacheControl)`。
///
/// 把断点挂到消息的**最后一个 text 片段**上：
/// - 字符串 `content` 先包装成单个 text 片段（否则无处挂载），空串返回 `false`；
/// - 数组 `content` 从后往前找 text 片段，找到即挂并返回 `true`；
/// - 没有可用文本片段时返回 `false`（调用方据此决定是否继续向前找消息）。
///
/// 只改变内容承载形状，不新增或删除文本；返回值表示「断点是否放下」。
fn add_cache_control_to_text_content(
    message: &mut Map<String, Value>,
    cache_control: &Value,
) -> bool {
    match message.get("content") {
        Some(Value::String(content)) => {
            if content.is_empty() {
                return false;
            }
            let mut part = Map::new();
            part.insert("type".into(), Value::String("text".into()));
            part.insert("text".into(), Value::String(content.clone()));
            part.insert("cache_control".into(), cache_control.clone());
            message.insert("content".into(), Value::Array(vec![Value::Object(part)]));
            true
        }
        Some(Value::Array(_)) => {
            let Some(Value::Array(parts)) = message.get_mut("content") else {
                return false;
            };
            for part in parts.iter_mut().rev() {
                let Some(part) = part.as_object_mut() else {
                    continue;
                };
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.insert("cache_control".into(), cache_control.clone());
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// `addCacheControlToMessage(message, cacheControl)`：只处理
/// `user`/`assistant`/`tool` 三种角色，其余角色直接返回 `false`。
fn add_cache_control_to_message(message: &mut Map<String, Value>, cache_control: &Value) -> bool {
    match message.get("role").and_then(Value::as_str) {
        Some("user") | Some("assistant") | Some("tool") => {
            add_cache_control_to_text_content(message, cache_control)
        }
        _ => false,
    }
}

/// `applyAnthropicCacheControl(messages, tools, cacheControl)`。
///
/// 固定放置三个断点，分别对应 prompt 的稳定前缀与会话尾部：
/// 1. 第一条 system/developer 消息的最后一个 text 片段；
/// 2. 最后一个工具定义（在工具对象上直接加 `cache_control` 字段）；
/// 3. 从后往前第一条能挂上断点的 user/assistant/tool 消息。
///
/// 任一位置找不到可挂载的文本块就跳过，**不插入空块**——否则会改变消息语义。
/// 入参已是序列化后的 `Value`，本函数只做就地修改；`tools` 为 `None` 或空时第 2 步跳过。
fn apply_anthropic_cache_control(
    messages: &mut [Value],
    tools: Option<&mut Vec<Value>>,
    cache_control: &Value,
) {
    // 1. 第一条 system/developer 消息。
    for message in messages.iter_mut() {
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        if matches!(
            message.get("role").and_then(Value::as_str),
            Some("system") | Some("developer")
        ) {
            add_cache_control_to_text_content(message, cache_control);
            break;
        }
    }
    // 2. 最后一个工具定义。
    if let Some(tools) = tools
        && let Some(last) = tools.last_mut()
        && let Some(last) = last.as_object_mut()
    {
        last.insert("cache_control".into(), cache_control.clone());
    }
    // 3. 最后一条 user/assistant/tool 消息。
    for message in messages.iter_mut().rev() {
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        if add_cache_control_to_message(message, cache_control) {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// 工具转换
// ---------------------------------------------------------------------------

/// `convertTools(tools, compat)`。
///
/// 两种输出形态：
/// - grammar 约束且 provider 支持 grammar → `{"type":"custom","custom":{name,description,format}}`
///   （`format.grammar.syntax` 为 `lark`/`regex`，`definition` 为 grammar 文本）；
/// - 其余 → `{"type":"function","function":{name,description,parameters[,strict]}}`，
///   `strict` 只在 `supports_strict_mode` 为真时写出（缺省 false）。
///
/// 错误来自约束解析，但并非「不满足就失败」：
/// - grammar 配置在 provider 不支持 grammar 工具时**静默**按普通 function 发送
///   （`resolve_grammar_constrained_sampling` 直接返回 `None`）；只有 grammar 变体缺失
///   或参数形状非法时才报错；
/// - strict 只有 `"require"` 在 provider 不支持或 schema 无法收紧时报错；`"prefer"`
///   静默回落为非 strict。
fn convert_tools(
    tools: &[Tool],
    compat: &ResolvedOpenAICompletionsCompat,
) -> Result<Vec<Value>, AiError> {
    let mut converted = Vec::with_capacity(tools.len());
    for tool in tools {
        if let Some(grammar) =
            resolve_grammar_constrained_sampling(tool, compat.supports_openai_grammar_tools)?
        {
            let mut grammar_definition = Map::new();
            grammar_definition.insert(
                "syntax".into(),
                Value::String(grammar.format.as_str().into()),
            );
            grammar_definition.insert(
                "definition".into(),
                Value::String(grammar.definition.clone()),
            );
            let mut grammar_format = Map::new();
            grammar_format.insert("type".into(), Value::String("grammar".into()));
            grammar_format.insert("grammar".into(), Value::Object(grammar_definition));
            let mut custom = Map::new();
            custom.insert("name".into(), Value::String(tool.name.clone()));
            custom.insert(
                "description".into(),
                Value::String(tool.description.clone()),
            );
            custom.insert("format".into(), Value::Object(grammar_format));
            let mut item = Map::new();
            item.insert("type".into(), Value::String("custom".into()));
            item.insert("custom".into(), Value::Object(custom));
            converted.push(Value::Object(item));
            continue;
        }
        let strict = resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode, None)?;
        let mut function = Map::new();
        function.insert("name".into(), Value::String(tool.name.clone()));
        function.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        function.insert(
            "parameters".into(),
            get_json_schema_tool_parameters(tool, strict)?,
        );
        if compat.supports_strict_mode {
            function.insert("strict".into(), Value::Bool(strict.unwrap_or(false)));
        }
        let mut item = Map::new();
        item.insert("type".into(), Value::String("function".into()));
        item.insert("function".into(), Value::Object(function));
        converted.push(Value::Object(item));
    }
    Ok(converted)
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// `buildParams(...)`。
///
/// 组装 `/chat/completions` 请求体。字段按「基础字段 → 工具 → 缓存断点 → tool_choice →
/// 思考方言 → token 预算 → 路由 → 采样参数」的顺序写入；后写的会覆盖先写的同名键，
/// 所以 `sampling_params` 放在最后，允许调用方覆盖任何具名字段。
///
/// 关键约束：
/// - `messages` 由 [`convert_messages`] 生成，角色映射由 compat 决定；
/// - 缓存断点必须在 `tools` 写入之后施加，否则最后一个工具拿不到 `cache_control`；
/// - `resolve_clamped_thinking_budget` 读取的是**已写入 params** 的 token 上限，
///   因此必须排在 `max_tokens`/`max_completion_tokens` 写入之后；
/// - 错误来自消息/工具转换（约束配置不可满足时整请求失败），不是网络层。
fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: CacheRetention,
    grammar_tool_input_properties: &IndexMap<String, String>,
) -> Result<Value, AiError> {
    // 只有「中途 system 消息」与「中途追加工具」两个能力都具备时才按 Additions 处理；
    // 否则工具声明会折叠到首条 system 消息里（TranscriptToolSupport::None）。
    let tool_support =
        if compat.supports_mid_convo_system_messages && compat.supports_mid_convo_tool_additions {
            TranscriptToolSupport::Additions
        } else {
            TranscriptToolSupport::None
        };
    let transcript_tools = resolve_transcript_tools(&context.as_context().messages, tool_support);
    let messages = convert_messages(
        model,
        context,
        compat,
        Some(&ConvertCompletionsMessagesOptions {
            grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
        }),
    )?;
    let cache_control = compat_cache_control(compat, cache_retention);

    let mut params: Map<String, Value> = Map::new();
    params.insert("model".into(), Value::String(model.id.clone()));
    params.insert("messages".into(), Value::Array(messages));
    params.insert("stream".into(), Value::Bool(true));

    // prompt cache 的两条独立条件：OpenAI 官方域名（任意缓存保留级别），
    // 或长缓存且 provider 支持（`supports_long_cache_retention`）。key 由 session_id 派生并夹取长度。
    let prompt_cache_enabled = (model.base_url.contains("api.openai.com")
        && cache_retention != CacheRetention::None)
        || (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention);
    if prompt_cache_enabled
        && let Some(key) = clamp_openai_prompt_cache_key(options.base.session_id.as_deref())
    {
        params.insert("prompt_cache_key".into(), Value::String(key));
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert("prompt_cache_retention".into(), Value::String("24h".into()));
    }
    // 流式 usage 依赖 include_usage；是否写这个字段由 compat 决定。
    if compat.supports_usage_in_streaming {
        let mut stream_options = Map::new();
        stream_options.insert("include_usage".into(), Value::Bool(true));
        params.insert("stream_options".into(), Value::Object(stream_options));
    }
    if compat.supports_store {
        params.insert("store".into(), Value::Bool(false));
    }
    // max_tokens == 0 视为「未设置」，不写字段（对应 TS 的 `if (options?.maxTokens)`，
    // 0 是假值）。
    if let Some(max_tokens) = options.base.max_tokens.filter(|value| *value != 0) {
        params.insert(compat.max_tokens_field.clone(), Value::from(max_tokens));
    }
    // temperature 用「有值就写」：0.0 是合法温度，不能像 max_tokens 那样按真值过滤
    // （TS 是 `!== undefined`）。
    if let Some(temperature) = options.base.temperature {
        params.insert("temperature".into(), Value::from(temperature));
    }
    // 工具声明优先用「当前有效工具」；若一个都没有但历史里出现过工具调用，则写空数组
    // （Anthropic 经代理时要求带 tool_calls/tool_results 的会话必须传 tools）。
    // `zai_tool_stream` 为真时额外写 `tool_stream: true`（该方言的流式工具参数开关）。
    if !transcript_tools.request_tools.is_empty() {
        params.insert(
            "tools".into(),
            Value::Array(convert_tools(&transcript_tools.request_tools, compat)?),
        );
        if compat.zai_tool_stream {
            params.insert("tool_stream".into(), Value::Bool(true));
        }
    } else if has_tool_history(&context.as_context().messages) {
        // Anthropic（经 LiteLLM/代理）要求带 tool_calls/tool_results 的会话必须传 tools。
        params.insert("tools".into(), Value::Array(Vec::new()));
    }
    // 缓存断点直接改已序列化的 messages/tools。先把 tools 取出来独立修改，改完写回，
    // 这样「最后一个工具」与消息断点都能生效（见 apply_anthropic_cache_control）。
    if let Some(cache_control) = &cache_control {
        let mut tools = match params.get("tools") {
            Some(Value::Array(tools)) => Some(tools.clone()),
            _ => None,
        };
        if let Some(Value::Array(messages)) = params.get_mut("messages") {
            apply_anthropic_cache_control(messages, tools.as_mut(), cache_control);
        }
        if let Some(tools) = tools {
            if let Some(slot) = params.get_mut("tools") {
                *slot = Value::Array(tools);
            } else {
                params.insert("tools".into(), Value::Array(tools));
            }
        }
    }
    if let Some(tool_choice) = options
        .tool_choice
        .as_ref()
        .filter(|value| truthy(Some(value)))
    {
        // TS 是 `if (options?.toolChoice)`：null/false/"" 都不写进 payload
        // （Responses/Azure 适配器用的是 `!== undefined`，语义不同）。
        params.insert("tool_choice".into(), tool_choice.clone());
    }
    if let Some(priority) = compat.vllm_priority {
        params.insert("priority".into(), Value::from(priority));
    }

    // 预算字段名与预算值是两个独立来源：名字由 compat 决定，值由上一步的夹取结果决定。
    let thinking_token_budget_field = resolve_thinking_token_budget_field(compat);
    let thinking_budget = resolve_clamped_thinking_budget(model, options, &params);
    let reasoning_effort = options.reasoning_effort;
    let supports_reasoning_effort = compat.supports_reasoning_effort;
    let model_reasoning = model.reasoning == Some(true);

    // 思考参数按 provider 方言分派。所有分支都先要求 `model.reasoning == Some(true)`：
    // 非推理模型即使调用方传了 reasoning_effort，也不能凭空写思考字段。
    // 各方言写出的字段与回退规则见 `spec/ai-api-anthropic-openai.md` §2.10.7 的表格；
    // 未匹配到任何方言（含 `"openai"`）时走最后的默认分支。
    match compat.thinking_format.as_str() {
        "zai" if model_reasoning => {
            let mut thinking = Map::new();
            if reasoning_effort.is_some() {
                thinking.insert("type".into(), Value::String("enabled".into()));
                thinking.insert("clear_thinking".into(), Value::Bool(false));
            } else {
                thinking.insert("type".into(), Value::String("disabled".into()));
            }
            params.insert("thinking".into(), Value::Object(thinking));
            if let Some(level) = reasoning_effort
                && supports_reasoning_effort
            {
                let mapped = mapped_thinking_level(model, to_model_thinking_level(level));
                let effort = match mapped {
                    None => Some(thinking_level_str(level).to_string()),
                    Some(value) => value,
                };
                if let Some(effort) = effort {
                    params.insert("reasoning_effort".into(), Value::String(effort));
                }
            }
        }
        "qwen" if model_reasoning => {
            params.insert(
                "enable_thinking".into(),
                Value::Bool(reasoning_effort.is_some()),
            );
            if let Some(level) = reasoning_effort
                && supports_reasoning_effort
            {
                let mapped = mapped_thinking_level(model, to_model_thinking_level(level));
                let effort = match mapped {
                    None => Some(thinking_level_str(level).to_string()),
                    Some(value) => value,
                };
                if let Some(effort) = effort {
                    params.insert("reasoning_effort".into(), Value::String(effort));
                }
            }
        }
        "qwen-chat-template" if model_reasoning => {
            let mut kwargs = Map::new();
            kwargs.insert(
                "enable_thinking".into(),
                Value::Bool(reasoning_effort.is_some()),
            );
            kwargs.insert("preserve_thinking".into(), Value::Bool(true));
            params.insert("chat_template_kwargs".into(), Value::Object(kwargs));
        }
        "chat-template" if model_reasoning => {
            if let Some(values) = build_chat_template_values(
                model,
                reasoning_effort,
                &compat.chat_template_kwargs,
                thinking_budget,
            ) {
                params.insert("chat_template_kwargs".into(), values);
            }
        }
        "baseten" if model_reasoning => {
            if let Some(values) = build_chat_template_values(
                model,
                reasoning_effort,
                &compat.chat_template_args,
                thinking_budget,
            ) {
                params.insert("chat_template_args".into(), values);
            }
            if supports_reasoning_effort {
                let mapped = match reasoning_effort {
                    Some(level) => mapped_thinking_level(model, to_model_thinking_level(level)),
                    None => mapped_off(model),
                };
                let effort = match mapped {
                    None => reasoning_effort.map(|level| thinking_level_str(level).to_string()),
                    Some(value) => value,
                };
                if let Some(effort) = effort {
                    params.insert("reasoning_effort".into(), Value::String(effort));
                }
            }
        }
        "deepseek" if model_reasoning => {
            // `thinkingLevelMap.off` 显式为 null 表示「关闭时什么都不写」；键缺失或为字符串
            // 时写 `disabled`（对应 TS 的 `map.off !== null`）。
            if reasoning_effort.is_some() {
                let mut thinking = Map::new();
                thinking.insert("type".into(), Value::String("enabled".into()));
                params.insert("thinking".into(), Value::Object(thinking));
            } else if mapped_off(model) != Some(None) {
                let mut thinking = Map::new();
                thinking.insert("type".into(), Value::String("disabled".into()));
                params.insert("thinking".into(), Value::Object(thinking));
            }
            if let Some(level) = reasoning_effort
                && supports_reasoning_effort
            {
                let mapped = mapped_thinking_level(model, to_model_thinking_level(level));
                let effort = mapped
                    .flatten()
                    .unwrap_or_else(|| thinking_level_str(level).to_string());
                params.insert("reasoning_effort".into(), Value::String(effort));
            }
        }
        "openrouter" if model_reasoning => {
            let mapped_effort = match reasoning_effort {
                Some(level) => {
                    let mapped = mapped_thinking_level(model, to_model_thinking_level(level));
                    let effort = mapped
                        .flatten()
                        .unwrap_or_else(|| thinking_level_str(level).to_string());
                    Some(effort)
                }
                None if mapped_off(model) != Some(None) => Some(
                    mapped_off(model)
                        .flatten()
                        .unwrap_or_else(|| "none".to_string()),
                ),
                None => None,
            };
            if let Some(effort) = mapped_effort {
                let mut reasoning = Map::new();
                reasoning.insert("effort".into(), Value::String(effort));
                params.insert("reasoning".into(), Value::Object(reasoning));
            }
        }
        "ant-ling" if model_reasoning && reasoning_effort.is_some() => {
            let level = reasoning_effort.expect("checked");
            if let Some(Some(effort)) = mapped_thinking_level(model, to_model_thinking_level(level))
            {
                let mut reasoning = Map::new();
                reasoning.insert("effort".into(), Value::String(effort));
                params.insert("reasoning".into(), Value::Object(reasoning));
            }
        }
        "together" if model_reasoning => {
            let mut reasoning = Map::new();
            reasoning.insert("enabled".into(), Value::Bool(reasoning_effort.is_some()));
            params.insert("reasoning".into(), Value::Object(reasoning));
            if let Some(level) = reasoning_effort
                && supports_reasoning_effort
            {
                let mapped = mapped_thinking_level(model, to_model_thinking_level(level));
                let effort = mapped
                    .flatten()
                    .unwrap_or_else(|| thinking_level_str(level).to_string());
                params.insert("reasoning_effort".into(), Value::String(effort));
            }
        }
        "string-thinking" if model_reasoning => {
            let thinking = match reasoning_effort {
                Some(level) => {
                    let mapped = mapped_thinking_level(model, to_model_thinking_level(level));
                    Some(
                        mapped
                            .flatten()
                            .unwrap_or_else(|| thinking_level_str(level).to_string()),
                    )
                }
                None if mapped_off(model) != Some(None) => Some(
                    mapped_off(model)
                        .flatten()
                        .unwrap_or_else(|| "none".to_string()),
                ),
                None => None,
            };
            if let Some(thinking) = thinking {
                params.insert("thinking".into(), Value::String(thinking));
            }
        }
        _ => {
            if let Some(level) = reasoning_effort
                && model_reasoning
                && supports_reasoning_effort
            {
                let mapped = mapped_thinking_level(model, to_model_thinking_level(level));
                let effort = mapped
                    .flatten()
                    .unwrap_or_else(|| thinking_level_str(level).to_string());
                params.insert("reasoning_effort".into(), Value::String(effort));
            } else if reasoning_effort.is_none()
                && model_reasoning
                && supports_reasoning_effort
                && let Some(Some(off_value)) = mapped_off(model)
            {
                params.insert("reasoning_effort".into(), Value::String(off_value));
            }
        }
    }

    // 预算字段独立于方言：方言只决定「开关/档位」怎么表达，预算写不写、写哪个字段
    // 由 compat 决定（见 resolve_thinking_token_budget_field）。
    if let Some(field) = thinking_token_budget_field
        && let Some(budget) = thinking_budget
    {
        params.insert(field, Value::from(budget));
    }

    // OpenRouter 路由偏好：这里读 `model.compat` 的原始对象，而不是上面解析出的
    // `compat.open_router_routing`（resolved 结构里的同名字段在本模块未被使用）。
    if let Some(routing) = model
        .compat
        .as_ref()
        .and_then(|compat| compat.get("openRouterRouting"))
        .filter(|value| truthy(Some(value)))
    {
        params.insert("provider".into(), routing.clone());
    }
    // Vercel AI Gateway 路由偏好。
    if let Some(routing) = model
        .compat
        .as_ref()
        .and_then(|compat| compat.get("vercelGatewayRouting"))
        .filter(|value| truthy(Some(value)))
    {
        let only = routing.get("only").filter(|value| truthy(Some(value)));
        let order = routing.get("order").filter(|value| truthy(Some(value)));
        if only.is_some() || order.is_some() {
            let mut gateway = Map::new();
            if let Some(only) = only {
                gateway.insert("only".into(), only.clone());
            }
            if let Some(order) = order {
                gateway.insert("order".into(), order.clone());
            }
            let mut provider_options = Map::new();
            provider_options.insert("gateway".into(), Value::Object(gateway));
            params.insert("providerOptions".into(), Value::Object(provider_options));
        }
    }

    // 最后合并采样参数：允许调用方用 `sampling_params` 覆盖前面算出的任何具名字段
    // （例如 temperature / top_p / priority），顺序与 TS 的 `Object.assign` 一致。
    let sampling_params = resolve_sampling_params(
        model,
        reasoning_effort
            .map(to_model_thinking_level)
            .unwrap_or(ModelThinkingLevel::Off),
        options.base.sampling_params.as_ref(),
    );
    if let Some(sampling_params) = sampling_params {
        for (key, value) in sampling_params {
            params.insert(key, value);
        }
    }

    Ok(Value::Object(params))
}

// ---------------------------------------------------------------------------
// 消息转换
// ---------------------------------------------------------------------------

/// `/[^a-zA-Z0-9_-]/g` → `"_"`（按 UTF-16 码元）。
///
/// 逐码元判断并替换：一个非 BMP 字符（代理对）会被替换成**两个**下划线，与 JS 正则
/// 在 UTF-16 字符串上的行为一致；ASCII 字母/数字/`_`/`-` 原样保留。
fn replace_disallowed(part: &str) -> String {
    part.encode_utf16()
        .map(|unit| {
            let ch = unit as u32;
            let allowed = (u32::from(b'a')..=u32::from(b'z')).contains(&ch)
                || (u32::from(b'A')..=u32::from(b'Z')).contains(&ch)
                || (u32::from(b'0')..=u32::from(b'9')).contains(&ch)
                || ch == u32::from(b'_')
                || ch == u32::from(b'-');
            if allowed {
                char::from_u32(ch).unwrap_or('_')
            } else {
                '_'
            }
        })
        .collect()
}

/// `String.prototype.slice(start, end)`（按 UTF-16 码元）。
///
/// 越界会夹取；`start >= end` 返回空串。注意 `from_utf16_lossy` 会把被切断的代理项
/// 替换成 U+FFFD，而 JS 会保留孤立代理项——tool call id 通常是 ASCII，正常不会触发；
/// 若输入可能含非 BMP 字符，这里是已知的保真差异。
fn utf16_slice(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = end.min(units.len());
    if start >= end {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..end])
}

/// `String.prototype.length` 的等价物：UTF-16 码元数（不是字节数，也不是码点数）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `convertMessages(model, context, compat, options?)`（导出）。
///
/// 把内部消息模型翻译成 Chat Completions 的 `messages[]`。流程：
/// 1. `resolve_transcript` 归一化（provider 不支持中途 system 消息时折到首条）；
/// 2. `transform_messages_with` 做跨 provider 的消息变换，其中 tool call id 经
///    `normalize_tool_call_id` 清洗（含 `|` 的 id 重组并截断到 40 个 UTF-16 码元）；
/// 3. 逐条按角色转换；连续的 toolResult 在同一个内层循环里处理（每条输出一条
///    `role:"tool"`），其中的图片汇总后另起一条 user 消息；
/// 4. 返回的 `Vec<Value>` 保持键的插入顺序（`serde_json` 的 `preserve_order` 已启用，
///    快照测试按 JSON 文本逐字节比较）。
///
/// 错误：仅来自工具转换（约束配置不可满足）与 grammar 工具参数提取；普通消息永远能转换。
pub fn convert_messages(
    model: &Model,
    context: &TranscriptContext,
    compat: &ResolvedOpenAICompletionsCompat,
    options: Option<&ConvertCompletionsMessagesOptions>,
) -> Result<Vec<Value>, AiError> {
    let normalized = resolve_transcript(context, Some(compat.supports_mid_convo_system_messages));
    let source_messages = &normalized.as_context().messages;

    // 工具调用 id 归一化（TS 的同名内部函数）：
    // - 形如 `<callId>|<itemId>` 的 id（Responses 风格）先分别清洗再拼接；超过 40 个
    //   UTF-16 码元时改用 `short_hash(id)` 的前 8 位作后缀，前缀按剩余长度截断；
    // - provider 为 openai 时，普通 id 也硬截断到 40；
    // - 其他 provider 原样保留（它们接受长 id）。
    // 结果必须确定性：回放历史消息时同一个 id 要得到同一个值，tool_call 与
    // tool_result 才能配对。
    let target_provider = model.provider.clone();
    let normalize_tool_call_id = move |id: &str| -> String {
        if let Some(separator_index) = id.find('|') {
            let call_id = replace_disallowed(&id[..separator_index]);
            let item_id = replace_disallowed(&id[separator_index + 1..]);
            let combined_id = if !item_id.is_empty() {
                format!("{call_id}_{item_id}")
            } else {
                call_id.clone()
            };
            if utf16_len(&combined_id) <= 40 {
                return combined_id;
            }
            let hash = utf16_slice(&short_hash(id), 0, 8);
            let prefix = utf16_slice(&call_id, 0, std::cmp::max(1, 40 - hash.len() - 1));
            return format!("{prefix}_{hash}");
        }
        if target_provider == "openai" && utf16_len(id) > 40 {
            return utf16_slice(id, 0, 40);
        }
        id.to_string()
    };
    let callback: &NormalizeToolCallIdFn =
        &move |id: &str, _model: &Model, _source: &AssistantMessage| normalize_tool_call_id(id);
    let transformed = transform_messages_with(source_messages, model, Some(callback));

    let tool_support =
        if compat.supports_mid_convo_system_messages && compat.supports_mid_convo_tool_additions {
            TranscriptToolSupport::Additions
        } else {
            TranscriptToolSupport::None
        };
    let transcript_tools = resolve_transcript_tools(source_messages, tool_support);
    // 推理模型且 compat 支持 developer role 时，所有 system 提示都改用 `developer`；
    // 否则用 `system`。该选择对整条会话一致，不逐条切换。
    let instruction_role = if model.reasoning == Some(true) && compat.supports_developer_role {
        "developer"
    } else {
        "system"
    };
    let grammar_properties =
        options.and_then(|options| options.grammar_tool_input_properties.as_ref());

    // 主循环用 `last_role` 记录上一条**输出**消息的类别，只服务
    // requires_assistant_after_tool_result 的占位插入；`system_index` 用于判断某条
    // system 消息的工具声明是否已折叠进请求级 tools（见 resolve_transcript_tools）。
    let mut params: Vec<Value> = Vec::new();
    let mut last_role: Option<String> = None;
    let mut system_index: usize = 0;
    let mut index = 0usize;
    while index < transformed.len() {
        let message = &transformed[index];
        if compat.requires_assistant_after_tool_result
            && last_role.as_deref() == Some("toolResult")
            && matches!(message, crate::types::Message::User(_))
        {
            params.push(json_role_content(
                "assistant",
                Value::String("I have processed the tool results.".into()),
            ));
        }
        match message {
            crate::types::Message::System(system) => {
                // 前 `folded_system_messages` 条 system 消息的工具声明已经折叠进请求级
                // tools（`requestTools`），只有其后的 system 消息才需要用
                // `{role:"system", tools:[...]}` 单独声明新增工具（Kimi 风格）。
                let added_tools: &[Tool] =
                    if system_index >= transcript_tools.folded_system_messages {
                        system.tools_added.as_deref().unwrap_or(&[])
                    } else {
                        &[]
                    };
                system_index += 1;
                if !added_tools.is_empty() {
                    params.push(json_object(vec![
                        ("role", Value::String("system".into())),
                        ("tools", Value::Array(convert_tools(added_tools, compat)?)),
                    ]));
                }
                // 第一条消息渲染完整系统提示；后续 system 消息渲染成「更新」文本
                //（新增/删除的 section），避免每轮重复整段提示。
                let text = if index == 0 {
                    get_system_message_text(system)
                } else {
                    render_system_message_update(system)
                };
                if !text.is_empty() {
                    params.push(json_role_content(
                        instruction_role,
                        Value::String(sanitize_surrogates(&text)),
                    ));
                }
            }
            crate::types::Message::User(user) => match &user.content {
                UserContent::Text(content) => {
                    params.push(json_role_content(
                        "user",
                        Value::String(sanitize_surrogates(content)),
                    ));
                }
                UserContent::Blocks(blocks) => {
                    // 空文本块被跳过；图片统一转成 data URI 的 `image_url`。
                    // 这里不按 `model.input` 过滤图片（toolResult 分支会过滤）。
                    let mut content: Vec<Value> = Vec::with_capacity(blocks.len());
                    for block in blocks {
                        match block {
                            UserContentBlock::Text(text) => {
                                if text.text.is_empty() {
                                    continue;
                                }
                                content.push(json_object(vec![
                                    ("type", Value::String("text".into())),
                                    ("text", Value::String(sanitize_surrogates(&text.text))),
                                ]));
                            }
                            UserContentBlock::Image(image) => {
                                content.push(json_object(vec![
                                    ("type", Value::String("image_url".into())),
                                    (
                                        "image_url",
                                        json_object(vec![(
                                            "url",
                                            Value::String(format!(
                                                "data:{};base64,{}",
                                                image.mime_type, image.data
                                            )),
                                        )]),
                                    ),
                                ]));
                            }
                        }
                    }
                    if content.is_empty() {
                        index += 1;
                        continue;
                    }
                    params.push(json_role_content("user", Value::Array(content)));
                }
            },
            crate::types::Message::Assistant(assistant) => {
                // assistant 消息按 compat 组合 content / reasoning 字段 / tool_calls。
                // content 初值有讲究：requires_assistant_after_tool_result 用空串，
                // 否则用 null（不同 provider 对两种空值的接受度不同）。
                let mut assistant_msg = Map::new();
                assistant_msg.insert("role".into(), Value::String("assistant".into()));
                assistant_msg.insert(
                    "content".into(),
                    if compat.requires_assistant_after_tool_result {
                        Value::String(String::new())
                    } else {
                        Value::Null
                    },
                );

                let mut assistant_text_parts: Vec<Value> = Vec::new();
                let mut assistant_text = String::new();
                for block in &assistant.content {
                    if let AssistantContentBlock::Text(text) = block {
                        if kiana_json::js_string::js_trim(&text.text).is_empty() {
                            continue;
                        }
                        let part = json_object(vec![
                            ("type", Value::String("text".into())),
                            ("text", Value::String(sanitize_surrogates(&text.text))),
                        ]);
                        assistant_text
                            .push_str(part.get("text").and_then(Value::as_str).unwrap_or(""));
                        assistant_text_parts.push(part);
                    }
                }
                let thinking_blocks: Vec<&ThinkingContent> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Thinking(thinking) => Some(thinking),
                        _ => None,
                    })
                    .collect();
                let tool_calls: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::ToolCall(tool_call) => Some(tool_call),
                        _ => None,
                    })
                    .collect();

                // 推理明细有两条回放通道，优先级：思考块签名（新格式）> 工具调用上的
                // thought_signature（旧格式）；两者都没有时才退回把思考文本写进 reasoning
                // 字段（见下）。
                let signed_reasoning_details = thinking_blocks
                    .iter()
                    .filter_map(|block| {
                        parse_openai_reasoning_details(block.thinking_signature.as_deref())
                    })
                    .next();
                let legacy_reasoning_details: Vec<Value> = tool_calls
                    .iter()
                    .filter_map(|tool_call| {
                        parse_legacy_encrypted_reasoning_detail(
                            tool_call.thought_signature.as_deref(),
                        )
                    })
                    .collect();
                let preserved_reasoning_details = signed_reasoning_details.or_else(|| {
                    (!legacy_reasoning_details.is_empty()).then_some(legacy_reasoning_details)
                });

                // 只有 trim 后非空的思考块参与输出：纯空白思考不写进请求，避免污染上下文。
                let non_empty_thinking_blocks: Vec<&ThinkingContent> = thinking_blocks
                    .iter()
                    .copied()
                    .filter(|block| !kiana_json::js_string::js_trim(&block.thinking).is_empty())
                    .collect();
                if !non_empty_thinking_blocks.is_empty() {
                    if compat.requires_thinking_as_text {
                        let thinking_text = non_empty_thinking_blocks
                            .iter()
                            .map(|block| sanitize_surrogates(&block.thinking))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let mut content = vec![json_object(vec![
                            ("type", Value::String("text".into())),
                            ("text", Value::String(thinking_text)),
                        ])];
                        content.extend(assistant_text_parts.iter().cloned());
                        assistant_msg.insert("content".into(), Value::Array(content));
                    } else {
                        // 正文作为字符串写回。思考文本的字段名取自「首个非空思考块的签名」，
                        // 仅接受 reasoning / reasoning_content / reasoning_text 三个白名单
                        // 值；多块用换行拼接。opencode-go 的 "reasoning" 需要改写为
                        // "reasoning_content"（该 provider 的回放字段不同）。
                        if !assistant_text.is_empty() {
                            assistant_msg
                                .insert("content".into(), Value::String(assistant_text.clone()));
                        }
                        if preserved_reasoning_details.is_none() {
                            let mut signature =
                                non_empty_thinking_blocks[0].thinking_signature.clone();
                            if model.provider == "opencode-go"
                                && signature.as_deref() == Some("reasoning")
                            {
                                signature = Some("reasoning_content".to_string());
                            }
                            if let Some(signature) = signature.filter(|signature| {
                                !signature.is_empty()
                                    && is_openai_completions_reasoning_field(signature)
                            }) {
                                let thinking = non_empty_thinking_blocks
                                    .iter()
                                    .map(|block| block.thinking.clone())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                assistant_msg.insert(signature, Value::String(thinking));
                            }
                        }
                    }
                } else if !assistant_text.is_empty() {
                    assistant_msg.insert("content".into(), Value::String(assistant_text));
                }

                // 工具调用按 grammar 映射选择 custom（字符串 input）或 function（JSON 参数）；
                // grammar 映射缺失时一律回到标准 function 形态。
                if !tool_calls.is_empty() {
                    let mut converted_calls = Vec::with_capacity(tool_calls.len());
                    for tool_call in &tool_calls {
                        let custom_input_property = grammar_properties
                            .and_then(|properties| properties.get(&tool_call.name));
                        match custom_input_property {
                            Some(property) => {
                                let arguments: Map<String, Value> = tool_call
                                    .arguments
                                    .iter()
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect();
                                let input =
                                    get_grammar_tool_input(&tool_call.name, &arguments, property)?;
                                converted_calls.push(json_object(vec![
                                    ("id", Value::String(tool_call.id.clone())),
                                    ("type", Value::String("custom".into())),
                                    (
                                        "custom",
                                        json_object(vec![
                                            ("name", Value::String(tool_call.name.clone())),
                                            ("input", Value::String(sanitize_surrogates(&input))),
                                        ]),
                                    ),
                                ]));
                            }
                            None => {
                                converted_calls.push(json_object(vec![
                                    ("id", Value::String(tool_call.id.clone())),
                                    ("type", Value::String("function".into())),
                                    (
                                        "function",
                                        json_object(vec![
                                            ("name", Value::String(tool_call.name.clone())),
                                            (
                                                "arguments",
                                                Value::String(js_stringify(&json_from_object(
                                                    &tool_call.arguments,
                                                ))),
                                            ),
                                        ]),
                                    ),
                                ]));
                            }
                        }
                    }
                    assistant_msg.insert("tool_calls".into(), Value::Array(converted_calls));
                }
                if let Some(details) = preserved_reasoning_details {
                    assistant_msg.insert("reasoning_details".into(), Value::Array(details));
                }
                if compat.requires_reasoning_content_on_assistant_messages
                    && model.reasoning == Some(true)
                    && !assistant_msg.contains_key("reasoning_content")
                {
                    assistant_msg.insert("reasoning_content".into(), Value::String(String::new()));
                }
                // 既无内容又无工具调用的 assistant 消息会被丢弃：部分 provider 不接受空消息。
                // `content` 为 null、空串或空数组都算「无内容」。
                let has_content = match assistant_msg.get("content") {
                    None | Some(Value::Null) => false,
                    Some(Value::String(content)) => !content.is_empty(),
                    Some(Value::Array(content)) => !content.is_empty(),
                    Some(_) => true,
                };
                if !has_content && !assistant_msg.contains_key("tool_calls") {
                    index += 1;
                    continue;
                }
                params.push(Value::Object(assistant_msg));
            }
            crate::types::Message::ToolResult(_) => {
                // 连续的 toolResult 合并处理：每条写一条 role:"tool"，但图片统一收集，
                // 最后只追加一条 user 消息（协议不允许把 image_url 放进 tool 消息）。
                // 内层循环前进 `cursor`，外层 `index` 直接跳到合并段之后。
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut cursor = index;
                while cursor < transformed.len() {
                    let crate::types::Message::ToolResult(tool_result) = &transformed[cursor]
                    else {
                        break;
                    };
                    let text_result = tool_result
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            UserContentBlock::Text(text) => Some(text.text.as_str()),
                            UserContentBlock::Image(_) => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tool_result
                        .content
                        .iter()
                        .any(|block| matches!(block, UserContentBlock::Image(_)));
                    let has_text = !kiana_json::js_string::js_trim(&text_result).is_empty();
                    // 文本为空时给占位符：有图用 "(see attached image)"，否则
                    // "(no tool output)"。两个字符串是当前稳定契约，改动需同步测试（spec §2.10.10）。
                    let tool_result_text = if has_text {
                        text_result
                    } else if has_images {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };
                    let mut tool_result_msg = Map::new();
                    tool_result_msg.insert("role".into(), Value::String("tool".into()));
                    tool_result_msg.insert(
                        "content".into(),
                        Value::String(sanitize_surrogates(&tool_result_text)),
                    );
                    tool_result_msg.insert(
                        "tool_call_id".into(),
                        Value::String(tool_result.tool_call_id.clone()),
                    );
                    if compat.requires_tool_result_name && !tool_result.tool_name.is_empty() {
                        tool_result_msg
                            .insert("name".into(), Value::String(tool_result.tool_name.clone()));
                    }
                    params.push(Value::Object(tool_result_msg));
                    // 图片只在模型声明支持 image 输入时才搬运；不支持时直接丢弃图片数据
                    // 而不报错。注意占位符只在**纯图片**（无文本）时才说明「有图」；
                    // 同时含文本与图片时走文本分支，图片无提示丢弃。
                    if has_images && model.input.contains(&ModelInputModality::Image) {
                        for block in &tool_result.content {
                            if let UserContentBlock::Image(image) = block {
                                image_blocks.push(json_object(vec![
                                    ("type", Value::String("image_url".into())),
                                    (
                                        "image_url",
                                        json_object(vec![(
                                            "url",
                                            Value::String(format!(
                                                "data:{};base64,{}",
                                                image.mime_type, image.data
                                            )),
                                        )]),
                                    ),
                                ]));
                            }
                        }
                    }
                    cursor += 1;
                }
                index = cursor;
                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json_role_content(
                            "assistant",
                            Value::String("I have processed the tool results.".into()),
                        ));
                    }
                    let mut content = vec![json_object(vec![
                        ("type", Value::String("text".into())),
                        (
                            "text",
                            Value::String("Attached image(s) from tool result:".into()),
                        ),
                    ])];
                    content.extend(image_blocks);
                    params.push(json_role_content("user", Value::Array(content)));
                    last_role = Some("user".to_string());
                } else {
                    // 没有图片时，下一条 user 消息前面可能要补 assistant 占位；
                    // 有图片时上面已把 last_role 改成 "user"，占位判断不会再触发。
                    last_role = Some("toolResult".to_string());
                }
                continue;
            }
        }
        last_role = Some(message.role().to_string());
        index += 1;
    }
    Ok(params)
}

/// `{ role, content }`（保持键序）。
fn json_role_content(role: &str, content: Value) -> Value {
    json_object(vec![
        ("role", Value::String(role.to_string())),
        ("content", content),
    ])
}

// ---------------------------------------------------------------------------
// usage / stop reason
// ---------------------------------------------------------------------------

/// JS 的 `value || 0`（`NaN`、0、非数字都归零）。
fn js_number_or_zero(value: Option<&Value>) -> f64 {
    match value.and_then(Value::as_f64) {
        Some(number) if number != 0.0 && !number.is_nan() => number,
        _ => 0.0,
    }
}

/// `value ?? fallback` 的单步：缺失或 `null` 时 `None`。
fn nullish_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Null) | None => None,
        Some(value) => value.as_f64(),
    }
}

/// 浮点计数转非负 `u64`：`NaN`、无穷、负数与 0 都归零，其余向零截断。
/// provider 的 usage 是 JSON number，TS 侧保留浮点；Rust 的 [`Usage`] 字段是整数。
fn non_negative_u64(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        value as u64
    }
}

/// `parseChunkUsage(rawUsage, model)`。
///
/// 兼容三种 usage 形状：OpenAI 的 `prompt_tokens_details.cached_tokens`、DeepSeek 的
/// `prompt_cache_hit_tokens`、顶层 `cached_tokens`（按此顺序取第一个非 `null` 值）。
/// `input` 是 `prompt_tokens` 扣除缓存命中与缓存写入后的净值（下限 0），`total_tokens`
/// 再把三者加回来；这与「只扣 cache read」的实现不同，不能随手改。
/// 最后调用 `calculate_cost` 计算费用；OpenRouter 模型还会用 provider 上报的 cost 覆盖
/// （见 `apply_open_router_accounting`）。
fn parse_chunk_usage(raw_usage: &Value, model: &Model) -> Usage {
    let prompt_tokens = js_number_or_zero(raw_usage.get("prompt_tokens"));
    let prompt_details = raw_usage.get("prompt_tokens_details");
    let cache_read_tokens =
        nullish_number(prompt_details.and_then(|details| details.get("cached_tokens")))
            .or_else(|| nullish_number(raw_usage.get("prompt_cache_hit_tokens")))
            .or_else(|| nullish_number(raw_usage.get("cached_tokens")))
            .unwrap_or(0.0);
    let cache_write_tokens =
        js_number_or_zero(prompt_details.and_then(|details| details.get("cache_write_tokens")));
    let input = (prompt_tokens - cache_read_tokens - cache_write_tokens).max(0.0);
    let output_tokens = js_number_or_zero(raw_usage.get("completion_tokens"));
    let reasoning_tokens = js_number_or_zero(
        raw_usage
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens")),
    );
    let mut usage = Usage {
        input: non_negative_u64(input),
        output: non_negative_u64(output_tokens),
        cache_read: non_negative_u64(cache_read_tokens),
        cache_write: non_negative_u64(cache_write_tokens),
        cache_write1h: None,
        reasoning: Some(non_negative_u64(reasoning_tokens)),
        total_tokens: non_negative_u64(
            input + output_tokens + cache_read_tokens + cache_write_tokens,
        ),
        cost: UsageCost::default(),
    };
    calculate_cost(model, &mut usage);
    if is_open_router_model(&model.provider, &model.base_url) {
        apply_open_router_accounting(&mut usage, raw_usage);
    }
    usage
}

/// `applyOpenRouterReportedCost` 的宽松 JSON 入口。
///
/// 与共享实现的关系：共享函数用 `Option<f64>` 表达 `null`，因此这里先按 TS 的
/// `=== null` / `undefined` 区别把「键缺失」提前返回（TS 对缺失值不会走 fallback 分支）。
///
/// 分支：
/// - 非 BYOK：`cost` 必须存在、有限且 >= 0，直接作为总费用并标记来自 provider；
/// - BYOK：`cost` 与 `cost_details.upstream_inference_cost` 缺一不可；后者显式为 `null`
///   时用 prompt + completion 两个上游成本求和（两者都必须存在且合法）。
/// 任一步不满足就保持本地 `calculate_cost` 的结果，不报错。
fn apply_open_router_accounting(usage: &mut Usage, raw: &Value) {
    let Some(is_byok) = raw.get("is_byok").and_then(Value::as_bool) else {
        return;
    };
    let cost = match raw.get("cost") {
        Some(Value::Number(number)) => Some(number.as_f64().unwrap_or(f64::NAN)),
        _ => None,
    };
    if !is_byok {
        if let Some(cost) = cost.filter(|cost| cost.is_finite() && *cost >= 0.0) {
            usage.cost.total = cost;
            usage.cost.total_from_provider = Some(true);
        }
        return;
    }
    let Some(details) = raw.get("cost_details").and_then(Value::as_object) else {
        return;
    };
    // `details.upstream_inference_cost` 为 undefined（键缺失）时 isValidCost 失败 → 什么都不做；
    // 为显式 null 才回落到 prompt + completion 之和。
    let upstream = match details.get("upstream_inference_cost") {
        None => return,
        Some(Value::Null) => {
            let prompt = nullish_number(details.get("upstream_inference_prompt_cost"));
            let completion = nullish_number(details.get("upstream_inference_completions_cost"));
            match (prompt, completion) {
                (Some(prompt), Some(completion))
                    if prompt.is_finite()
                        && prompt >= 0.0
                        && completion.is_finite()
                        && completion >= 0.0 =>
                {
                    Some(prompt + completion)
                }
                _ => return,
            }
        }
        Some(Value::Number(number)) => Some(number.as_f64().unwrap_or(f64::NAN)),
        Some(_) => return,
    };
    let Some(cost) = cost.filter(|cost| cost.is_finite() && *cost >= 0.0) else {
        return;
    };
    let Some(upstream) = upstream.filter(|upstream| upstream.is_finite() && *upstream >= 0.0)
    else {
        return;
    };
    let accounting = OpenRouterUsageAccounting {
        cost: Some(cost),
        is_byok: true,
        cost_details: Some(OpenRouterCostDetails {
            upstream_inference_cost: Some(upstream),
            upstream_inference_prompt_cost: 0.0,
            upstream_inference_completions_cost: 0.0,
            server_tool_cost: None,
        }),
    };
    apply_open_router_reported_cost(usage, &accounting);
}

/// `mapStopReason(reason)`。
///
/// 返回 `(StopReason, 错误消息)`：只有 `stop`/`end`/`length`/`function_call`/`tool_calls`
/// 是正常结束；`content_filter`、`network_error` 与任何未知值都映射为
/// [`StopReason::Error`] 并给出逐字错误文本（调用方会因此把整次请求当失败处理）。
/// 未知值不静默当 `stop`：新出现的 provider 终止原因会表现为请求失败，而不是静默成功。
fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_string()),
        ),
        "network_error" => (
            StopReason::Error,
            Some("Provider finish_reason: network_error".to_string()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// header / 客户端
// ---------------------------------------------------------------------------

/// 大小写不敏感的 header 合并表：键是小写形式，值保留调用方给的原始大小写与内容。
///
/// `null` 表示删除（见 `merge_provider_headers`）；重设已存在的键会把它移动到末尾，
/// 因此最终顺序反映「最后一次设置」的先后。
type HeaderTable = IndexMap<String, (String, String)>;

/// 大小写不敏感地写入一个 header：先删旧键，再插入到末尾。
fn set_header(table: &mut HeaderTable, name: &str, value: &str) {
    let lower = name.to_ascii_lowercase();
    table.shift_remove(&lower);
    table.insert(lower, (name.to_string(), value.to_string()));
}

/// 合并普通 header（值恒为字符串，不能删除已有键）。
fn merge_plain_headers(table: &mut HeaderTable, headers: &IndexMap<String, String>) {
    for (name, value) in headers {
        set_header(table, name, value);
    }
}

/// 合并 [`crate::types::ProviderHeaders`]：值为 `None`（JSON null）时删除同名键
/// （TS 类型为 `Record<string, string | null>`，`null` 表示抑制同名默认头；
/// `undefined` 只是键缺失，不表示删除）。
fn merge_provider_headers(table: &mut HeaderTable, headers: &crate::types::ProviderHeaders) {
    for (name, value) in headers {
        match value {
            Some(value) => set_header(table, name, value),
            None => {
                table.shift_remove(&name.to_ascii_lowercase());
            }
        }
    }
}

/// `createClient(...)` 的 header 部分（SDK 的默认头顺序：Accept → 鉴权 → defaultHeaders →
/// Content-Type）。
///
/// 合并顺序：默认 `Accept`/`Authorization` → identity headers（User-Agent/归因）→
/// `model.headers` → Copilot 动态头 → 会话亲和头 → 调用方 headers → `Content-Type`。
/// 后写的同名头覆盖先写的（大小写不敏感），所以 `model.headers` 与调用方 headers 都能
/// 覆盖默认的 `Authorization`；[`crate::types::ProviderHeaders`] 中值为 `None` 表示删除。
///
/// 会话亲和头只在 `sendSessionAffinityHeaders` 为真且 `session_id` 非空时发送；
/// `cache_retention == none` 时调用方传的是 `None`（见 `run_completions_inner`）。
fn build_headers(
    model: &Model,
    context: &TranscriptContext,
    api_key: &str,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedOpenAICompletionsCompat,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut table: HeaderTable = IndexMap::new();
    set_header(&mut table, "Accept", "application/json");
    set_header(&mut table, "Authorization", &format!("Bearer {api_key}"));

    let target = ClientIdentityTarget::new(model.provider.clone(), model.base_url.clone());
    let identity_headers = get_identity_headers(&target, options.base.identity.as_ref());
    merge_provider_headers(&mut table, &identity_headers);
    if let Some(model_headers) = &model.headers {
        merge_plain_headers(&mut table, model_headers);
    }
    if model.provider == "github-copilot" {
        let messages = &context.as_context().messages;
        let has_images = has_copilot_vision_input(messages);
        for (name, value) in build_copilot_dynamic_headers(messages, has_images) {
            set_header(&mut table, &name, &value);
        }
    }
    if let Some(session_id) = session_id.filter(|session_id| !session_id.is_empty())
        && compat.send_session_affinity_headers
    {
        if compat.session_affinity_format.as_deref() == Some("openrouter") {
            set_header(&mut table, "x-session-id", session_id);
        } else {
            if compat.session_affinity_format.as_deref() == Some("openai") {
                set_header(&mut table, "session_id", session_id);
            }
            set_header(&mut table, "x-client-request-id", session_id);
            set_header(&mut table, "x-session-affinity", session_id);
        }
    }
    if let Some(options_headers) = &options.base.headers {
        merge_provider_headers(&mut table, options_headers);
    }
    set_header(&mut table, "Content-Type", "application/json");
    table.into_values().collect()
}

/// 默认 `HttpClient`（`options.fetch` 缺省时）。
///
/// 传入 `fetch` 时原样使用（测试与上层注入点）。否则进程内复用同一个
/// `reqwest::Client`（它内部持有连接池与 TLS 配置），用函数级 `OnceLock` 缓存。
/// `ReqwestHttpClient::new()` 失败（如 TLS 后端初始化失败）时退回 `default()`；
/// 但 `default()` 由 `#[derive(Default)]` 展开为 `reqwest::Client::default()`（即
/// `Client::new()`），同一 TLS 初始化失败场景同样会 panic，而且它不关闭内建解压。
/// 因此本函数不能承诺「总能拿到一个可用的 fetch」。
fn http_client(options: &StreamOptions) -> SharedHttpClient {
    if let Some(fetch) = &options.fetch {
        return fetch.clone();
    }
    static DEFAULT: OnceLock<SharedHttpClient> = OnceLock::new();
    DEFAULT
        .get_or_init(
            || match crate::http::reqwest_client::ReqwestHttpClient::new() {
                Ok(client) => client.shared(),
                Err(_) => Arc::new(crate::http::reqwest_client::ReqwestHttpClient::default()),
            },
        )
        .clone()
}

// ---------------------------------------------------------------------------
// 流状态
// ---------------------------------------------------------------------------

// 工具调用的两种参数载体：
// - `Json`：普通 function 调用，参数是增量 JSON 文本，由 `ToolCallArgumentsStream` 解析；
// - `Custom`：grammar 工具，参数是 `custom.input` 里的 JSON 字符串，按一个属性名缓冲，
//   收尾时由 `close_grammar_tool_input_json` 补上收尾引号/花括号。
#[allow(clippy::large_enum_variant)] // 与 TS 状态对象一一对应；大变体是流式 JSON 解析器。
enum ToolCallInput {
    Json(ToolCallArgumentsStream),
    Custom {
        property: String,
        buffer: GrammarToolInputJsonBuffer,
    },
}

/// 单个工具调用块的流式状态。
///
/// TS 用工具调用对象本身做 `Map` 的键；Rust 没有稳定对象身份，因此外层
/// `CompletionsStreamState::states` 用**内容块下标**索引本结构（spec §6.3.12）。
struct ToolCallState {
    /// 首个非空分片携带的 `index`；首片可能没有、后续补上后不再更新
    /// （`ensure_tool_call_block` 只在 `is_none()` 时写入）。
    stream_index: Option<i64>,
    /// 参数解析器（普通 JSON 或 grammar 缓冲）。
    input: ToolCallInput,
}

/// 一次 Completions 流式响应的可变状态。
///
/// 不变量：
/// - `output.content` 只追加、不删除；整个响应最多一个 text 块和一个 thinking 块
///   （首个增量创建，之后复用），工具调用块按分片出现的顺序追加。
/// - `by_index` / `by_id` 是同一批工具调用块的两种索引；分片可能只带其中一种，
///   二者共同保证后续增量归并到同一块。
/// - `states` 只保存**尚未 finish** 的工具调用解析器；`finish_block` 会把它移除。
/// - `streamed_reasoning_details` 是流式累积的推理明细，收尾时写进思考块签名。
struct CompletionsStreamState<'a> {
    output: &'a mut AssistantMessage,
    stream: &'a AssistantMessageEventStream,
    grammar: &'a IndexMap<String, String>,
    text_block: Option<usize>,
    thinking_block: Option<usize>,
    by_index: HashMap<i64, usize>,
    by_id: HashMap<String, usize>,
    states: HashMap<usize, ToolCallState>,
    streamed_reasoning_details: &'a mut Option<Vec<Value>>,
}

impl<'a> CompletionsStreamState<'a> {
    fn push(&self, event: AssistantMessageEvent) {
        let _ = self.stream.push(event);
    }

    fn partial(&self) -> Box<AssistantMessage> {
        Box::new(self.output.clone())
    }

    /// 首次出现正文增量时创建唯一的 text 块并推 `text_start`；之后一律复用该下标。
    fn ensure_text_block(&mut self) -> usize {
        if let Some(index) = self.text_block {
            return index;
        }
        self.output
            .content
            .push(AssistantContentBlock::Text(TextContent::default()));
        let index = self.output.content.len() - 1;
        self.text_block = Some(index);
        self.push(AssistantMessageEvent::TextStart {
            content_index: index,
            partial: self.partial(),
        });
        index
    }

    /// 首次出现推理增量时创建唯一的 thinking 块并推 `thinking_start`。
    ///
    /// `thinking_signature` 由首个命中的推理字段名决定（`reasoning`/`reasoning_content`/
    /// `reasoning_text`），回放时据此选择写回哪个字段。
    fn ensure_thinking_block(&mut self, thinking_signature: &str) -> usize {
        if let Some(index) = self.thinking_block {
            return index;
        }
        self.output
            .content
            .push(AssistantContentBlock::Thinking(ThinkingContent {
                thinking: String::new(),
                thinking_signature: Some(thinking_signature.to_string()),
                redacted: None,
                ..ThinkingContent::default()
            }));
        let index = self.output.content.len() - 1;
        self.thinking_block = Some(index);
        self.push(AssistantMessageEvent::ThinkingStart {
            content_index: index,
            partial: self.partial(),
        });
        index
    }

    /// 把流式累积的 `reasoning_details` JSON 序列化后写进思考块签名。
    ///
    /// 这是「明细 → 签名」的转换点；`None` 表示本次流没有推理明细，保持原签名不动。
    fn apply_streamed_reasoning_details(&mut self, index: usize) {
        let Some(details) = self.streamed_reasoning_details.as_ref() else {
            return;
        };
        let signature = js_stringify(&Value::Array(details.clone()));
        if let Some(AssistantContentBlock::Thinking(block)) = self.output.content.get_mut(index) {
            block.thinking_signature = Some(signature);
        }
    }

    /// `ensureToolCallBlock(toolCall)`。
    ///
    /// 归并顺序：先按分片 `index` 找块，再按 `id`，都没有命中才新建（新建时推
    /// `toolcall_start`）。新建的块若分片是 `custom`（而非 `function`），参数初始化为
    /// 「一个空字符串属性」，属性名来自 grammar 映射（缺省 `"input"`）。
    ///
    /// 兼容分支：同一个调用可能先以 function 形态出现、后续分片才变成 custom；
    /// 此时把已累积的参数整体替换为 custom 缓冲（`needs_switch`），避免两种解析器混用。
    /// 返回内容块下标；不会改动已关闭的块。
    fn ensure_tool_call_block(&mut self, tool_call: &Value) -> Result<usize, AiError> {
        let stream_index = tool_call.get("index").and_then(Value::as_i64);
        let function = tool_call
            .get("function")
            .filter(|value| truthy(Some(value)));
        let custom = tool_call.get("custom").filter(|value| truthy(Some(value)));
        let name_value = function
            .and_then(|function| function.get("name"))
            .filter(|value| !value.is_null())
            .or_else(|| {
                custom
                    .and_then(|custom| custom.get("name"))
                    .filter(|value| !value.is_null())
            });
        let name = name_value.and_then(Value::as_str).unwrap_or("");
        let id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());

        let mut block_index = stream_index.and_then(|index| self.by_index.get(&index).copied());
        if block_index.is_none()
            && let Some(id) = id
        {
            block_index = self.by_id.get(id).copied();
        }
        if block_index.is_none() {
            let custom_input_property = if custom.is_some() && function.is_none() {
                Some(
                    self.grammar
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| "input".to_string()),
                )
            } else {
                None
            };
            let mut arguments = crate::types::JsonObject::new();
            if let Some(property) = &custom_input_property {
                arguments.insert(property.clone(), Value::String(String::new()));
            }
            self.output
                .content
                .push(AssistantContentBlock::ToolCall(ToolCall {
                    id: id.unwrap_or("").to_string(),
                    name: name.to_string(),
                    arguments,
                    ..ToolCall::default()
                }));
            let index = self.output.content.len() - 1;
            let input = match custom_input_property {
                Some(property) => ToolCallInput::Custom {
                    property,
                    buffer: GrammarToolInputJsonBuffer::default(),
                },
                None => ToolCallInput::Json(ToolCallArgumentsStream::new()),
            };
            self.states.insert(
                index,
                ToolCallState {
                    stream_index,
                    input,
                },
            );
            if let Some(stream_index) = stream_index {
                self.by_index.insert(stream_index, index);
            }
            if let Some(id) = id {
                self.by_id.insert(id.to_string(), index);
            }
            self.push(AssistantMessageEvent::ToolCallStart {
                content_index: index,
                partial: self.partial(),
            });
            block_index = Some(index);
        }
        let index = block_index.expect("block was just created or found");
        if let Some(state) = self.states.get_mut(&index)
            && let Some(stream_index) = stream_index
            && state.stream_index.is_none()
        {
            state.stream_index = Some(stream_index);
            self.by_index.insert(stream_index, index);
        }
        if let Some(id) = id {
            self.by_id.insert(id.to_string(), index);
        }
        let block_name = match self.output.content.get(index) {
            Some(AssistantContentBlock::ToolCall(block)) => block.name.clone(),
            _ => String::new(),
        };
        if block_name.is_empty()
            && !name.is_empty()
            && let Some(AssistantContentBlock::ToolCall(block)) = self.output.content.get_mut(index)
        {
            block.name = name.to_string();
        }
        if custom.is_some() && function.is_none() {
            let needs_switch = matches!(
                self.states.get(&index).map(|state| &state.input),
                Some(ToolCallInput::Json(_))
            );
            if needs_switch {
                let current_name = match self.output.content.get(index) {
                    Some(AssistantContentBlock::ToolCall(block)) => block.name.clone(),
                    _ => String::new(),
                };
                let property = self
                    .grammar
                    .get(&current_name)
                    .cloned()
                    .unwrap_or_else(|| "input".to_string());
                if let Some(AssistantContentBlock::ToolCall(block)) =
                    self.output.content.get_mut(index)
                {
                    let mut arguments = crate::types::JsonObject::new();
                    arguments.insert(property.clone(), Value::String(String::new()));
                    block.arguments = arguments;
                }
                if let Some(state) = self.states.get_mut(&index) {
                    state.input = ToolCallInput::Custom {
                        property,
                        buffer: GrammarToolInputJsonBuffer::default(),
                    };
                }
            }
        }
        Ok(index)
    }

    /// 处理一个 `tool_calls[]` 分片，返回要发给 `toolcall_delta` 的文本。
    ///
    /// - `Json` 形态：分片里的 `function.arguments` 是 JSON 文本的一段，直接追加给
    ///   `ToolCallArgumentsStream`；解析过程中的问题不会在这里报错，最终由 `finish()`
    ///   产出 `arguments_error`。返回的 delta 就是原文。
    /// - `Custom` 形态：分片里的 `custom.input` 是转义后的字符串片段，经
    ///   `append_grammar_tool_input_json_delta` 解码并生成给客户端看的 JSON 文本增量；
    ///   buffer 已关闭时返回 `Err`（provider 违约）。非单调检查属于
    ///   `close_grammar_tool_input_json` 且要求传入最终文本，不在本路径触发。
    ///
    /// 状态先 `remove` 再写回，避免同时持有 `self.states` 与 `self.output` 的可变借用；
    /// 只有块类型不匹配的提前返回会把 state 放回；解析错误经 `?` 直接退出，state 被丢弃
    /// （流随即以 error 结束）。
    fn apply_tool_call_delta(
        &mut self,
        index: usize,
        tool_call: &Value,
    ) -> Result<String, AiError> {
        let function = tool_call
            .get("function")
            .filter(|value| truthy(Some(value)));
        let custom = tool_call.get("custom").filter(|value| truthy(Some(value)));
        let id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let name_value = function
            .and_then(|function| function.get("name"))
            .filter(|value| !value.is_null())
            .or_else(|| {
                custom
                    .and_then(|custom| custom.get("name"))
                    .filter(|value| !value.is_null())
            });
        let name = name_value.and_then(Value::as_str);
        let arguments_text = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
        let custom_input = custom
            .and_then(|custom| custom.get("input"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());

        let Some(mut state) = self.states.remove(&index) else {
            return Ok(String::new());
        };
        let Some(AssistantContentBlock::ToolCall(block)) = self.output.content.get_mut(index)
        else {
            self.states.insert(index, state);
            return Ok(String::new());
        };
        if block.id.is_empty()
            && let Some(id) = id
        {
            block.id = id.to_string();
            self.by_id.insert(id.to_string(), index);
        }
        if block.name.is_empty()
            && let Some(name) = name.filter(|name| !name.is_empty())
        {
            block.name = name.to_string();
        }
        let mut delta = String::new();
        match &mut state.input {
            ToolCallInput::Json(arguments) => {
                if let Some(text) = arguments_text {
                    delta = text.to_string();
                    block.arguments = arguments.append(text);
                }
            }
            ToolCallInput::Custom { property, buffer } => {
                if let Some(text) = custom_input {
                    delta = append_grammar_tool_input_json_delta(buffer, property, text)?
                        .unwrap_or_default();
                    let mut arguments = crate::types::JsonObject::new();
                    arguments.insert(property.clone(), Value::String(buffer.input.clone()));
                    block.arguments = arguments;
                }
            }
        }
        self.states.insert(index, state);
        Ok(delta)
    }

    /// `finishBlock(block)`：按块的类型收尾并推对应的 `*_end` 事件。
    ///
    /// - text：推 `text_end`（内容取当前累积文本）；
    /// - thinking：先把流式推理明细写进签名，再推 `thinking_end`；
    /// - toolCall：移除解析器；Custom 先补发最后一个 `toolcall_delta`，Json 调用
    ///   `finish()` 写入最终 `arguments` 与 `arguments_error`，最后推 `toolcall_end`。
    ///
    /// 非上述类型（正常流程不会出现）静默返回；调用方按内容顺序遍历，保证每个块都收到
    /// 结束事件，即使上游没有显式发送结束标记。
    fn finish_block(&mut self, index: usize) -> Result<(), AiError> {
        let is_text = matches!(
            self.output.content.get(index),
            Some(AssistantContentBlock::Text(_))
        );
        if is_text {
            let content = match self.output.content.get(index) {
                Some(AssistantContentBlock::Text(block)) => block.text.clone(),
                _ => String::new(),
            };
            self.push(AssistantMessageEvent::TextEnd {
                content_index: index,
                content,
                partial: self.partial(),
            });
            return Ok(());
        }
        let is_thinking = matches!(
            self.output.content.get(index),
            Some(AssistantContentBlock::Thinking(_))
        );
        if is_thinking {
            self.apply_streamed_reasoning_details(index);
            let content = match self.output.content.get(index) {
                Some(AssistantContentBlock::Thinking(block)) => block.thinking.clone(),
                _ => String::new(),
            };
            self.push(AssistantMessageEvent::ThinkingEnd {
                content_index: index,
                content,
                partial: self.partial(),
            });
            return Ok(());
        }
        if !matches!(
            self.output.content.get(index),
            Some(AssistantContentBlock::ToolCall(_))
        ) {
            return Ok(());
        }
        let state = self.states.remove(&index);
        match state {
            Some(ToolCallState {
                input:
                    ToolCallInput::Custom {
                        property,
                        mut buffer,
                    },
                ..
            }) => {
                let delta = close_grammar_tool_input_json(&mut buffer, &property, None)?;
                if let Some(delta) = delta {
                    self.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta,
                        partial: self.partial(),
                    });
                }
            }
            Some(ToolCallState {
                input: ToolCallInput::Json(arguments),
                ..
            }) => {
                let finished = arguments.finish();
                if let Some(AssistantContentBlock::ToolCall(block)) =
                    self.output.content.get_mut(index)
                {
                    block.arguments = finished.arguments;
                    block.arguments_error = finished.arguments_error;
                }
            }
            None => {}
        }
        let tool_call = match self.output.content.get(index) {
            Some(AssistantContentBlock::ToolCall(block)) => block.clone(),
            _ => ToolCall::default(),
        };
        self.push(AssistantMessageEvent::ToolCallEnd {
            content_index: index,
            tool_call,
            partial: self.partial(),
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 内层错误 + 最后一次请求失败信息。
///
/// 分开保存是因为错误格式化（`format_thrown_error_with_value`）需要 `RequestFailure`
/// 里的 status/body；普通 `AiError` 没有这部分，`From` 实现把它置为 `None`。
struct CompletionsRunError {
    error: AiError,
    last_failure: Option<RequestFailure>,
}

impl From<AiError> for CompletionsRunError {
    fn from(error: AiError) -> Self {
        CompletionsRunError {
            error,
            last_failure: None,
        }
    }
}

/// 执行一次完整的 Completions 请求；输出通过 `stream` 推送，返回 `Err` 由
/// [`run_completions`] 统一转成 `error` 事件。
///
/// 阶段：
/// 1. 鉴权与 compat（缺 key 在发出任何 HTTP 请求前失败）；
/// 2. grammar 工具映射、缓存保留策略、header、请求体；
/// 3. `on_payload` 可整体替换请求体（返回 `None` 表示不替换）；
/// 4. `request_with_retry` 发送，带 `signal` 与重试参数；失败同时带回 `last_failure`
///    供错误格式化；
/// 5. 推 `start`（在 `ensure_response_body` 之前，复刻 SDK 的迭代期报错时序）；
/// 6. SSE 循环与收尾校验（顺序见函数末尾注释）。
///
/// 错误消息逐字固定（`spec/ai-api-anthropic-openai.md` §3.1.9）：缺鉴权、
/// `Request was aborted`、`Stream ended without finish_reason`、
/// `Provider returned an error stop reason`。
async fn run_completions_inner(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICompletionsOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    streamed_reasoning_details: &mut Option<Vec<Value>>,
) -> Result<(), CompletionsRunError> {
    // 阶段 1：鉴权。`Authorization`/`cf-aig-authorization` 已由调用方给出时用占位 key
    // "unused"；两者都没有才返回 `No API key for provider: <provider>`。
    let api_key = get_client_api_key(
        &model.provider,
        options.base.api_key.as_deref(),
        options.base.headers.as_ref(),
    )?;
    let compat = get_compat(model);
    // 阶段 2：从「已声明工具」推导 grammar 映射；provider 不支持 grammar 时映射为空，
    // 所有工具都按 function 形态发送。
    let declared_tools = get_declared_tools(&context.as_context().messages);
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        Some(&declared_tools),
        compat.supports_openai_grammar_tools,
    )?;
    let cache_retention =
        resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    // 关闭缓存时连 session_id 都不传：会话亲和 header 与 prompt_cache_key 都不该出现。
    let cache_session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.base.session_id.clone()
    };
    // 阶段 3：header 与请求体。两者都依赖同一个 resolved compat。
    let headers = build_headers(
        model,
        context,
        &api_key,
        options,
        &compat,
        cache_session_id.as_deref(),
    );
    let mut params = build_params(
        model,
        context,
        options,
        &compat,
        cache_retention,
        &grammar_tool_input_properties,
    )?;
    // `on_payload` 返回 `Some` 时**整体替换**请求体（不会自动回填 `stream: true` 等字段）；
    // 返回 `None` 或未注册回调时保持原样。
    if let Some(next_params) = options.base.apply_on_payload(params.clone(), model).await {
        params = next_params;
    }
    // URL 由 `base_url` + `/chat/completions` 交给 `build_url` 拼接、解析并规范化；
    // 解析失败会以 `AiError` 返回（在发请求之前）。
    let request = HttpRequest {
        method: "POST".to_string(),
        url: build_url(&model.base_url, "/chat/completions", &[])?,
        headers,
        body: Some(json_body(&params)),
        timeout_ms: options.base.timeout_ms,
    };
    let client = http_client(&options.base);
    // 阶段 4：带重试发送。即使最终失败也返回 `last_failure`，让错误文本能带上
    // provider 的 status 与响应体。
    let (response, last_failure) = request_with_retry(
        client,
        request,
        options.base.signal.clone(),
        options.base.max_retries,
        options.base.max_retry_delay_ms,
    )
    .await;
    let response = response.map_err(|error| CompletionsRunError {
        error,
        last_failure,
    })?;
    // 响应回调在推 start 之前触发（TS 顺序）；状态码与 header 都已归一化为内部形状。
    if let Some(on_response) = &options.base.on_response {
        on_response(
            ProviderResponse {
                status: response.response.status,
                headers: headers_to_record(&response.response.headers),
            },
            model,
        );
    }
    // start 事件在检查 204/205 之前推送：复刻 SDK「先建流、迭代时才抛错」的时序。
    let _ = stream.push(AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });
    // 204/205 的响应没有 body：SDK 在迭代流时抛 OpenAIError（start 已推送之后）。
    ensure_response_body(response.response.status)?;

    // 阶段 5：SSE 循环。`provider_event_stream` 在后台解码事件，取消令牌同时终止读取。
    let mut events = provider_event_stream(response.body, options.base.signal.clone());
    let mut state = CompletionsStreamState {
        output,
        stream,
        grammar: &grammar_tool_input_properties,
        text_block: None,
        thinking_block: None,
        by_index: HashMap::new(),
        by_id: HashMap::new(),
        states: HashMap::new(),
        streamed_reasoning_details,
    };
    let mut has_finish_reason = false;

    // 阶段 6：逐事件处理。每个上游事件先交给观察者（`on_provider_stream_event`），
    // 再进入本模块逻辑；观察者回调没有返回值，无法在此中断请求。
    while let Some(event) = events.next().await {
        let chunk = event.map_err(CompletionsRunError::from)?;
        if let Some(on_event) = &options.base.on_provider_stream_event {
            on_event(chunk.clone(), model);
        }
        // 非对象 chunk（null、数组、字符串等）直接忽略，不让单个畸形事件终止整条流。
        if !chunk.is_object() {
            continue;
        }
        if state.output.response_id.is_none()
            && let Some(id) = chunk.get("id").and_then(Value::as_str)
        {
            state.output.response_id = Some(id.to_string());
        }
        if let Some(chunk_model) = chunk
            .get("model")
            .and_then(Value::as_str)
            .filter(|chunk_model| !chunk_model.is_empty() && *chunk_model != model.id)
            && state.output.response_model.is_none()
        {
            state.output.response_model = Some(chunk_model.to_string());
        }
        // usage 既可能在顶层，也可能在 choice 里（Moonshot）；顶层优先，见下。
        let chunk_usage = chunk.get("usage").filter(|value| truthy(Some(value)));
        if let Some(usage) = chunk_usage {
            state.output.usage = parse_chunk_usage(usage, model);
        }
        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .filter(|choice| choice.is_object());
        let Some(choice) = choice else {
            continue;
        };
        // Moonshot 等 provider 把 usage 放在 choice.usage 里。
        if chunk_usage.is_none()
            && let Some(usage) = choice.get("usage").filter(|value| truthy(Some(value)))
        {
            state.output.usage = parse_chunk_usage(usage, model);
        }
        // finish_reason 出现即记录原始值与映射结果；error 类映射会写 error_message，
        // 收尾时据此让整个请求失败。`has_finish_reason` 记录是否收到**非空字符串**
        // finish_reason；null、空串、非字符串都按缺失处理（之后走推断或报错分支）。
        if let Some(reason) = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
        {
            state.output.raw_stop_reason = Some(reason.to_string());
            let (stop_reason, error_message) = map_stop_reason(reason);
            state.output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                state.output.error_message = Some(error_message);
            }
            has_finish_reason = true;
        }
        let Some(delta) = choice
            .get("delta")
            .filter(|value| truthy(Some(value)))
            .and_then(Value::as_object)
        else {
            continue;
        };

        // 推理字段先于 content 处理（LiteLLM 会把 reasoning 与答案塞进同一个 delta）。
        // 按 TS 的本地顺序取第一个非空字段，避免 chutes.ai 同时返回两个字段时重复。
        let mut found_reasoning: Option<(&str, &str)> = None;
        for field_name in OPENAI_COMPLETIONS_STREAM_REASONING_FIELDS {
            if let Some(value) = delta
                .get(field_name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                found_reasoning = Some((field_name, value));
                break;
            }
        }
        if let Some((field_name, value)) = found_reasoning {
            let thinking_signature = if model.provider == "opencode-go" && field_name == "reasoning"
            {
                "reasoning_content"
            } else {
                field_name
            };
            let index = state.ensure_thinking_block(thinking_signature);
            if let Some(AssistantContentBlock::Thinking(block)) =
                state.output.content.get_mut(index)
            {
                block.thinking.push_str(value);
            }
            state.push(AssistantMessageEvent::ThinkingDelta {
                content_index: index,
                delta: value.to_string(),
                partial: state.partial(),
            });
        }

        // `reasoning_details` 不发独立事件，只在流式期间累积；收尾时整体序列化进
        // 思考块签名（见 `apply_streamed_reasoning_details`）。空签名创建空思考块以承载它。
        if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
            for detail in details {
                if !is_openai_reasoning_detail(detail) {
                    continue;
                }
                state.ensure_thinking_block("");
                let details = state
                    .streamed_reasoning_details
                    .get_or_insert_with(Vec::new);
                append_openai_reasoning_detail(details, detail);
            }
        }

        // 正文增量：首个非空 content 创建 text 块，之后追加到同一块。
        if let Some(content) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.is_empty())
        {
            let index = state.ensure_text_block();
            if let Some(AssistantContentBlock::Text(block)) = state.output.content.get_mut(index) {
                block.text.push_str(content);
            }
            state.push(AssistantMessageEvent::TextDelta {
                content_index: index,
                delta: content.to_string(),
                partial: state.partial(),
            });
        }

        // 工具调用分片：ensure 负责归并/新建块，apply 负责解析参数；即使解析出的 delta
        // 为空串也照常推 `toolcall_delta`（TS 行为，客户端可能依赖事件计数）。
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                let index = state.ensure_tool_call_block(tool_call)?;
                let delta_text = state.apply_tool_call_delta(index, tool_call)?;
                state.push(AssistantMessageEvent::ToolCallDelta {
                    content_index: index,
                    delta: delta_text,
                    partial: state.partial(),
                });
            }
        }
    }

    // 收尾 1：按内容顺序关闭所有块。上游可能没有显式结束标记，因此这里必须遍历全部块，
    // 保证每个 `start` 都有对应的 `end`。
    for index in 0..state.output.content.len() {
        state.finish_block(index)?;
    }
    // 收尾 2：状态校验。顺序按 spec §2.10.4 第 16 步，不能调整：
    // 中止 → 缺失 finish_reason 的推断（仅当 provider 不保证）→ error stop →
    // 仍无终止原因则报错。调序会改变并发取消与异常 finish_reason 时的错误文本。
    let signal_aborted = options
        .base
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled());
    if signal_aborted {
        return Err(AiError::new(AiErrorKind::Other, "Request was aborted").into());
    }
    if state.output.stop_reason == StopReason::Aborted {
        return Err(AiError::new(AiErrorKind::Other, "Request was aborted").into());
    }
    let has_tool_call = state
        .output
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)));
    if !has_finish_reason && !compat.supports_finish_reason {
        state.output.stop_reason = if has_tool_call {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        };
    }
    if state.output.stop_reason == StopReason::Error {
        let message = state
            .output
            .error_message
            .clone()
            .unwrap_or_else(|| "Provider returned an error stop reason".to_string());
        return Err(AiError::new(AiErrorKind::Other, message).into());
    }
    if (compat.supports_finish_reason && !has_finish_reason)
        || state.output.stop_reason == StopReason::Pending
    {
        return Err(AiError::new(AiErrorKind::Other, "Stream ended without finish_reason").into());
    }

    // 成功路径：先推 done（携带最终消息），再 end 关闭事件流；两者都用克隆，
    // 保证流消费者拿到的 `AssistantMessage` 与 output 一致。
    let stop_reason = state.output.stop_reason;
    let message = state.output.clone();
    drop(state);
    let _ = stream.push(AssistantMessageEvent::Done {
        reason: done_reason(stop_reason),
        message: Box::new(message.clone()),
    });
    stream.end(message);
    Ok(())
}

/// `applyStreamedReasoningDetails` 的 catch 侧收尾。
///
/// 与成功路径的区别：成功路径在 `finish_block` 里只写当前思考块；失败路径没有
/// `finish_block` 阶段，因此遍历 `output.content` 中**所有**思考块写入同一签名
/// （与 spec §2.10.4 第 18 步的 catch 行为一致）。
fn apply_streamed_reasoning_details_to_output(
    output: &mut AssistantMessage,
    streamed_reasoning_details: &Option<Vec<Value>>,
) {
    let Some(details) = streamed_reasoning_details else {
        return;
    };
    let signature = js_stringify(&Value::Array(details.clone()));
    for block in &mut output.content {
        if let AssistantContentBlock::Thinking(thinking) = block {
            thinking.thinking_signature = Some(signature.clone());
        }
    }
}

/// 跑一次请求，并把失败场景转成 `error` 事件。
///
/// 与 [`run_completions_inner`] 的分工：内层只返回 `Result`，不做收尾；这里负责：
/// 1. 根据取消令牌区分 `aborted` 与 `error`（令牌已取消时优先记为 `aborted`）；
/// 2. 把流式累积的 reasoning_details 写入思考块（失败前收到的部分也保留）；
/// 3. 用 `format_thrown_error_with_value` 生成与 TS 相同的错误文本与结构化值；
/// 4. OpenRouter 的 `error.error.metadata.raw` 追加到 message 末尾（已包含则不重复）；
/// 5. 推 `error` 事件并结束流。
///
/// 前置条件：`output` 与 `stream` 由调用方持有，本函数保证无论成功失败都会 `end` 流；
/// 成功路径由 `run_completions_inner` 自己推 `done` 并 `end`，不再经过这里。
async fn run_completions(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICompletionsOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    signal: Option<tokio_util::sync::CancellationToken>,
) {
    let mut streamed_reasoning_details: Option<Vec<Value>> = None;
    let result = run_completions_inner(
        model,
        context,
        options,
        output,
        stream,
        &mut streamed_reasoning_details,
    )
    .await;
    let Err(failure) = result else {
        return;
    };
    // 取消优先：即使内层报的是网络/解析错误，只要令牌已取消就记为 aborted。
    let aborted = signal.as_ref().is_some_and(|signal| signal.is_cancelled());
    output.stop_reason = if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    apply_streamed_reasoning_details_to_output(output, &streamed_reasoning_details);
    // 统一格式化：`formatProviderError(normalizeProviderError(error))` 的等价物，
    // 同时产出结构化值供后续探测 `error.error.metadata.raw`。
    let (mut message, value) =
        format_thrown_error_with_value(failure.error, failure.last_failure, None);
    // OpenRouter 等 provider 在 error.error.metadata.raw 里给出补充信息。
    if let ProviderErrorValue::Error(fields) = &value
        && let Some(raw) = fields
            .error_body
            .as_ref()
            .and_then(|body| body.get("metadata"))
            .and_then(|metadata| metadata.get("raw"))
            .filter(|value| truthy(Some(value)))
    {
        let raw = crate::api::openai_responses_shared::js_string(Some(raw));
        if !message.contains(&raw) {
            message.push('\n');
            message.push_str(&raw);
        }
    }
    // 错误消息写回 output，随 error 事件一起交给调用方；`stop_reason` 已在上面改写。
    output.error_message = Some(message);
    let reason = if aborted {
        crate::types::ErrorReason::Aborted
    } else {
        crate::types::ErrorReason::Error
    };
    let _ = stream.push(AssistantMessageEvent::Error {
        reason,
        error: Box::new(output.clone()),
    });
    stream.end(output.clone());
}

/// `stream`（导出）：完整选项。
///
/// 同步部分只做输入转换/克隆与 context 归一化：把入参转成
/// [`OpenAICompletionsOptions`]、构造事件流、克隆 model/context、按 compat 折叠
/// 中途 system 消息；随后 `spawn_forward` 把实际请求放到后台任务。因此本函数
/// **永不返回错误**：鉴权、网络、解析失败都变成流上的 `error` 事件。
///
/// 初始 `output` 的 `stop_reason` 是 `Pending`，`timestamp` 取当前毫秒；成功收尾时
/// `stop_reason` 改写为 provider 的映射结果，失败时由后台的 `run_completions`
/// 改写为 `Error`/`Aborted`。
///
/// 取消：`options.base.signal` 同时用于 HTTP 与 SSE；是否记为 `Aborted` 由
/// `run_completions` 在收尾时统一判断。
///
/// 参数用 `impl Into<...>`：调用方可以直接传 [`StreamOptions`]（`From` 已实现），
/// 也可以传完整选项。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: impl Into<OpenAICompletionsOptions>,
) -> AssistantMessageEventStream {
    let options = options.into();
    let stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let context = context.clone();
    // 在同步阶段按 compat 折叠中途 system 消息，后台任务拿到的是已归一化的 context。
    let normalized_context = resolve_transcript(
        &context,
        Some(get_compat(&model).supports_mid_convo_system_messages),
    );
    let signal = options.base.signal.clone();
    let stream_for_task = stream.clone();
    // 后台任务拥有 model/context/options 的克隆；事件流句柄通过 clone 共享，
    // 调用方立即拿到可读的流，无需等待请求完成。
    spawn_forward(async move {
        // 初始输出只填请求侧标识；usage/content 为空，stop_reason 为 Pending。
        let mut output = AssistantMessage {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            stop_reason: StopReason::Pending,
            timestamp: now_ms(),
            ..AssistantMessage::default()
        };
        run_completions(
            &model,
            &normalized_context,
            &options,
            &mut output,
            &stream_for_task,
            signal,
        )
        .await;
    });
    stream
}

/// `streamSimple`（导出）：简单选项；缺鉴权时同步返回 `Err`。
///
/// 与 [`stream`] 的关键差异：`get_client_api_key` 在**同步阶段**执行，因此缺 API key
/// 时直接返回 `Err`（同步校验鉴权失败，见 `spec/ai-api-anthropic-openai.md` §1.3）；
/// 其余错误仍然进入流上的 `error` 事件。
///
/// `options.reasoning` 先经 `clamp_thinking_level` 按模型能力夹取，`off` 映射为 `None`
/// （不启用思考）；`thinking_budgets` 原样透传。`tool_choice` 用 serde 转 JSON，
/// 理论上不会失败，失败时静默视为未设置。
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    get_client_api_key(
        &model.provider,
        options.base.api_key.as_deref(),
        options.base.headers.as_ref(),
    )?;
    let base = build_base_options(
        model,
        context,
        Some(&options),
        options.base.api_key.as_deref(),
    );
    let clamped_reasoning = options
        .reasoning
        .map(|level| clamp_thinking_level(model, to_model_thinking_level(level)));
    let reasoning_effort = clamped_reasoning.and_then(from_model_thinking_level);
    let tool_choice = options
        .tool_choice
        .and_then(|choice| serde_json::to_value(choice).ok());
    Ok(stream(
        model,
        context,
        OpenAICompletionsOptions {
            base,
            tool_choice,
            reasoning_effort,
            thinking_budgets: options.thinking_budgets,
        },
    ))
}

/// 冻结的 [`ProviderStreams`] 实现（零大小类型）。
///
/// `stream` 直接转发完整选项；`stream_simple` 把同步 `Err` 映射成流上的错误事件
/// （`lazy_stream_error`），这是 `api/mod.rs` 约定的「适配器入口可 `Err`、冻结接口
/// 不可 `Err`」的桥接点。
struct OpenAICompletionsStreams;

#[async_trait]
impl ProviderStreams for OpenAICompletionsStreams {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            OpenAICompletionsOptions::from_stream_options(options),
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        match stream_simple(model, context, options) {
            Ok(stream) => stream,
            Err(error) => lazy_stream_error(model, error),
        }
    }
}

/// `openAICompletionsApi()`：`lazyApi(() => import("./openai-completions.ts"))` 的等价物。
///
/// `lazy_api` 的 load 闭包在每次 `stream`/`stream_simple` 时经由 `lazy_stream` 的
/// 异步 setup 执行；模块本身在编译期已链接，所以闭包总是立即成功（不会像 TS 的动态
/// `import` 那样失败），保留的只是「工厂返回 `ProviderStreams` + 能力查询」的契约。
/// 第二个参数是 capabilities，本适配器不声明额外能力（deferred 请求等），传 `None`
/// 使用默认值。
///
/// 返回的 `Arc<dyn ProviderStreams>` 可被多个 provider 工厂共享。
pub fn openai_completions_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| {
            Box::pin(async { Ok(Arc::new(OpenAICompletionsStreams) as Arc<dyn ProviderStreams>) })
        }),
        None,
    )
}

#[cfg(test)]
mod golden {
    //! 与 TS 参考实现（`/home/ctyun/kiana-work/upstream-reference`）逐字节对照的转换黄金测试。
    //!
    //! 两个 fixture 是 TS 输出快照：`conversions.json` 锁定 `convert_messages`，
    //! `request_bodies.json` 锁定 `build_params`（含 thinking、缓存、采样字段）。
    //! 比较用 `js_stringify` 而非 `Value` 相等，因此**键序**也是被锁定的行为。
    //! 更新 fixture 等于改变基线，必须同时核对 TS 侧输出，不能只让 Rust 测试变绿。

    use super::*;
    use crate::types::TranscriptContext;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../../tests/fixtures/api-openai/conversions.json");
    const REQUEST_BODIES: &str =
        include_str!("../../tests/fixtures/api-openai/request_bodies.json");

    #[test]
    fn convert_messages_matches_typescript_reference() {
        let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        let model: Model =
            serde_json::from_value(fixture["completionsModel"].clone()).expect("completions model");
        let context: TranscriptContext =
            serde_json::from_value(fixture["context"].clone()).expect("context");
        let compat: ResolvedOpenAICompletionsCompat =
            serde_json::from_value(fixture["completions"]["compat"].clone()).expect("compat");
        let grammar: IndexMap<String, String> =
            serde_json::from_value(fixture["completions"]["grammarToolInputProperties"].clone())
                .expect("grammar map");
        let actual = convert_messages(
            &model,
            &context,
            &compat,
            Some(&ConvertCompletionsMessagesOptions {
                grammar_tool_input_properties: Some(grammar),
            }),
        )
        .expect("convert");
        assert_eq!(
            js_stringify(&Value::Array(actual)),
            js_stringify(&fixture["completions"]["messages"])
        );
    }

    #[test]
    fn build_params_matches_typescript_reference() {
        let fixture: Value = serde_json::from_str(REQUEST_BODIES).expect("fixture parses");
        let model: Model =
            serde_json::from_value(fixture["completions"]["model"].clone()).expect("model");
        let context: TranscriptContext =
            serde_json::from_value(fixture["completions"]["context"].clone()).expect("context");
        let options = OpenAICompletionsOptions {
            base: StreamOptions {
                max_tokens: Some(1234),
                temperature: Some(0.3),
                session_id: Some("session-xyz".into()),
                sampling_params: Some(
                    serde_json::from_value(json!({ "top_p": 0.9, "priority": 7 })).expect("params"),
                ),
                ..StreamOptions::default()
            },
            tool_choice: Some(json!({ "type": "function", "function": { "name": "read" } })),
            reasoning_effort: Some(ThinkingLevel::High),
            thinking_budgets: Some(ThinkingBudgets {
                high: Some(8192),
                ..ThinkingBudgets::default()
            }),
        };
        let grammar: IndexMap<String, String> = [("query".to_string(), "input".to_string())]
            .into_iter()
            .collect();
        let compat = get_compat(&model);
        let actual = build_params(
            &model,
            &context,
            &options,
            &compat,
            CacheRetention::Long,
            &grammar,
        )
        .expect("build params");
        assert_eq!(
            js_stringify(&actual),
            js_stringify(&fixture["completions"]["params"])
        );
    }
}
