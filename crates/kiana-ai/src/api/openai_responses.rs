//! api/openai-responses.ts。
//!
//! OpenAI Responses 适配器：请求体构造（reasoning / prompt cache / service tier）、
//! `processResponsesStream` 驱动与 ChatGPT 登录态的特殊处理。
//!
//! Rust 侧入口见 `spec/ai-api-anthropic-openai.md` §1.3/§8.5：`stream` 同步返回事件流，
//! [`stream_simple`] 缺鉴权时返回 `Err`，[`openai_responses_api`] 是 lazy 包装。
//!
//! # 这个模块负责什么
//!
//! 它是 `model.api == "openai-responses"` 的实现：把内部的 [`TranscriptContext`] 翻译成
//! `POST {base_url}/responses` 的请求体，再把服务端的 SSE 事件流还原成 [`AssistantMessage`]
//! 与流事件。Provider 层（`providers/openai.rs`、`github_copilot.rs`、`opencode*.rs`、
//! `cloudflare_ai_gateway.rs` 等）通过 [`openai_responses_api`] 把它注册进 `Models`，
//! 之后由 `ProviderStreams` 契约被 agent / 会话层调用。
//!
//! 本文件只保留 Responses 协议特有的部分：选项解析、compat 默认值、请求体字段拼装、
//! header 与 session 亲和、service tier 计价、入口与错误收尾。以下工作委托给
//! [`crate::api::openai_responses_shared`]：消息/工具转换（`convert_responses_messages`、
//! `convert_responses_tools`）、SSE 解析（`provider_event_stream`）、事件状态机
//! （`process_responses_stream`）、重试与错误正文格式化（`request_with_retry`、
//! `format_thrown_error`）。
//!
//! # 一次请求的执行流程
//!
//! 1. [`stream`]（同步）读取 compat、用 `resolve_transcript` 归一化上下文（不支持中途
//!    system 消息的模型会把 system 消息折叠到开头），算出 `configuration_update_effort`，
//!    再用 `spawn_forward` 把后续工作放进后台任务，立即返回事件流。
//! 2. 后台任务 `run_responses` 调 `run_responses_inner`：解析 API key、构造 grammar 映射、
//!    header（`build_headers`）与请求体（`build_params`）；`on_payload` 可在序列化前整体
//!    替换 payload。
//! 3. `request_with_retry` 发 POST `/responses`；拿到响应后先触发 `on_response`，推 `start`
//!    事件，再检查 204/205 无 body 的情况。
//! 4. `provider_event_stream` 把响应体解析成 JSON 事件，`process_responses_stream` 驱动
//!    文本/思考/工具调用增量事件并填充 `output`。
//! 5. 收尾：取消 → 抛 "Request was aborted"；`stop_reason` 仍为 `pending`、或为
//!    `error`/`aborted` → 抛错；否则推 `done` 并 `end`。
//! 6. `run_responses` 捕获第 2-5 步的任何错误，把 `stop_reason` 置为 `aborted`/`error`，
//!    格式化 `error_message` 后推 `error` 事件并 `end`；`stream` 的调用方只从事件流观察失败。
//!
//! # 与 TS 逐字对齐时最容易出错的约束
//!
//! - **ChatGPT 登录态**（`is_chatgpt_sign_in`）：`provider == "openai"`、`base_url` 恰为
//!   `https://api.openai.com/v1`、且 key 存在但不是以 `sk-` 开头时，视为 OAuth 登录。
//!   请求里不出现 `max_output_tokens`、`temperature`、`prompt_cache_retention`、
//!   `prompt_cache_options`，但 `prompt_cache_key` 仍然会发。
//! - **中途换档**：`supportsMidConvoEffort` 为真、请求了 `reasoningEffort`、且不是
//!   ChatGPT 登录时，`reasoning.effort` 固定为 `"medium"`，真实档位改为 input 里的
//!   `configuration_update` 项，并记录到 `output.provider_thinking_level`，使多轮之间
//!   换档不改写历史请求前缀（见 `tests/openai_responses_mid_convo_effort.rs`）。
//! - **session 亲和**：Responses 版本没有 `x-session-affinity`。`openrouter` 格式只发
//!   `x-session-id`；`openai` 格式发 `session_id` + `x-client-request-id`；其它取值
//!   （如模型目录给 opencode 用的 `openai-nosession`）只发 `x-client-request-id`。
//!   `cacheRetention == none` 时连这些头一起不发。
//! - **请求体键序**：workspace 的 `serde_json` 启用了 `preserve_order`，`build_params`
//!   的插入顺序就是 JSON 键序；黄金测试用 `js_stringify` 与 TS 输出逐字节比较。
//!
//! # 注意事项
//!
//! - `stream_simple` 的鉴权检查与 `stream` 内部那次是重复的：后者只能变成 `error` 事件，
//!   而 TS 契约要求 `streamSimple` 在缺鉴权时**同步抛出**（`spec/ai-api-anthropic-openai.md`
//!   §1.3 第 2 条），所以必须在建流之前先查一次。
//! - 本文件不实现 `fetch_deferred` / `cancel_deferred`；[`openai_responses_api`] 的
//!   capabilities 为 `None`，`Models` 层对这些能力会得到“不支持”。
//! - 错误前缀（`OpenAI API error` / `<provider> API error`）与两条 ChatGPT 用量提示属于
//!   用户可见文案，改动需同步 spec §2.11.7。

// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定（`Display` 必须等于 TS 的
// `error.message`），因此结果类型只按文档要求携带它。
#![allow(clippy::result_large_err)]
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::api::lazy::{lazy_api, lazy_stream_error, spawn_forward};
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, HeaderTable,
    OpenAIResponsesStreamOptions, RequestFailure, build_url, convert_responses_messages,
    convert_responses_tools, default_http_client, ensure_response_body, format_thrown_error,
    get_responses_tool_support, header_list, js_string, merge_plain_headers,
    merge_provider_headers, now_ms, process_responses_stream, provider_event_stream,
    provider_response, request_with_retry, set_header, truthy,
};
use crate::api::simple_options::{build_base_options, resolve_sampling_params};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::HttpRequest;
use crate::models::{ProviderStreams, clamp_thinking_level};
use crate::types::{
    AssistantMessage, CacheRetention, DoneReason, ErrorReason, Model, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, TranscriptContext, Usage,
};
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::transcript::{get_declared_tools, resolve_transcript, resolve_transcript_tools};

/// `OPENAI_TOOL_CALL_PROVIDERS`。
///
/// 使用「`call_id|item_id`」复合工具调用 id 的 provider 名单，作为
/// `convert_responses_messages` 的 `allowed_tool_call_providers` 传入：命中时历史里的复合
/// id 会被拆开、分别归一化，通常会给 item id 补上 `fc_` 前缀；不命中时整个 id 只做一次
/// 普通归一化。边界：item 段为空或全为下划线时（如 `call_1|`、`call_1|_`），归一化结果是
/// 空串，补 `fc_` 后再去尾部下划线只剩 `fc`，并不满足 `fc_` 形状（`normalize_id_part`
/// 的文档也承认这一点）。Azure Responses 适配器另有一份含 `azure` 的名单，两者不要混用。
pub const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
/// `OPENAI_RESPONSES_MIN_OUTPUT_TOKENS`。
///
/// OpenAI 拒绝小于 16 的 `max_output_tokens`；`build_params` 用 `max(16)` 把它抬到下限，
/// 而不是丢弃这个字段——所以请求 1 也会发送 16。
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;
/// `CHATGPT_USAGE_URL`。
///
/// 命中 ChatGPT 订阅用量上限时追加到 `error_message` 的链接。
const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/settings/usage";
/// `CHATGPT_WORKSPACE_LIMIT_HINT`（`→` 是 U+2192）。
///
/// ChatGPT Business 工作区级上限的整句提示；`→` 前后各一个半角空格、句末有句点，
/// 是当前稳定契约，改动需同步测试。
const CHATGPT_WORKSPACE_LIMIT_HINT: &str = "You've reached your ChatGPT workspace's usage limit. Ask your workspace admin to increase it at admin.openai.com → Workspace → Usage limits.";
/// `CONFIGURATION_UPDATE_REQUEST_EFFORT`。
///
/// 启用中途换档时请求体里固定的 `reasoning.effort`；真实档位改由 input 里的
/// `configuration_update` 项表达（见 `get_configuration_update_effort`）。
const CONFIGURATION_UPDATE_REQUEST_EFFORT: &str = "medium";

// ---------------------------------------------------------------------------
// 选项
// ---------------------------------------------------------------------------

/// `OpenAIResponsesOptions`。
///
/// Responses 专属选项 = [`StreamOptions`] + 下面四个 API 字段。经 `StreamOptions.extra`
/// 传入时用 TS 的 camelCase 键名（`reasoningEffort` / `reasoningSummary` / `serviceTier` /
/// `toolChoice`），由 [`OpenAIResponsesOptions::from_stream_options`] 提取；直接构造本结构体
/// 时填写具名字段即可。
#[derive(Default)]
pub struct OpenAIResponsesOptions {
    /// 全部 [`StreamOptions`] 字段。
    ///
    /// 鉴权、重试、超时、`fetch`、hook、`session_id`、`cache_retention`、`env` 都从这里读；
    /// 其余字段只覆盖 Responses 专属选项。
    pub base: StreamOptions,
    /// `reasoningEffort`。
    ///
    /// Kiana 的思考档位；只有 `model.reasoning == Some(true)` 时才写进请求体的
    /// `reasoning` 字段，并按 `model.thinking_level_map` 映射成 provider 的取值。
    /// 在 `model.reasoning == Some(true)` 下，缺省 effort 且 `reasoningSummary` 也不成立时
    /// 不会走“有档位”分支；但只要 `off` 映射不为 `null` 且 provider 不是 `github-copilot`，
    /// 仍会发送 `reasoning.effort`（取 `off` 的映射值，缺省 `"none"`，例如 `gpt-5.5`）。
    /// 只有 `github-copilot` 或 `off` 显式为 `null` 的模型才在缺档位时完全不出现 `reasoning`；
    /// `xai` 另会在同一外层分支补 `include`（见 `build_params`）。
    pub reasoning_effort: Option<ThinkingLevel>,
    /// `reasoningSummary`（`null` 表示显式 null，与缺省一样回落到 `"auto"`）。
    ///
    /// 按 JS 真值判断：`null`、`""`、`false`、`0` 都视为未提供；为真时用 `js_string`
    /// 转成字符串写进 `reasoning.summary`（TS 是 `reasoningSummary || "auto"`）。
    /// 只提供它、没有 `reasoningEffort` 时，隐含档位是 `"medium"`。
    pub reasoning_summary: Option<Value>,
    /// `serviceTier`。
    ///
    /// 原样写进请求体 `service_tier`（不受 ChatGPT 登录态的字段过滤影响），同时作为计价的
    /// 回退档位（响应未报告 service_tier 时使用）：`flex` 0.5，`priority`/`fast` 2
    /// （请求模型是 `gpt-5.5` 时 2.5）。
    pub service_tier: Option<String>,
    /// `toolChoice`。
    ///
    /// 原样写进请求体 `tool_choice`，可以是字符串（如 `"required"`）或对象；
    /// `None` 时字段不出现。
    pub tool_choice: Option<Value>,
}

impl From<StreamOptions> for OpenAIResponsesOptions {
    fn from(base: StreamOptions) -> Self {
        OpenAIResponsesOptions::from_stream_options(base)
    }
}

impl OpenAIResponsesOptions {
    /// 从 [`StreamOptions`] 的 `extra` 读取 API 专属字段。
    ///
    /// `extra` 是 `Models` 层原样透传的 TS 字段名容器（见 `StreamOptions::extra`）。
    /// 读取是宽松的：`reasoningEffort` 反序列化失败会被静默忽略，`serviceTier` 不是字符串
    /// 时按未提供处理，`reasoningSummary` / `toolChoice` 原样克隆 JSON 值。`base` 的其余
    /// 字段保持不变，因此调用方无需区分“从 TS 形状转换”和“直接构造”。
    pub fn from_stream_options(base: StreamOptions) -> Self {
        let reasoning_effort = base
            .extra
            .get("reasoningEffort")
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        let reasoning_summary = base.extra.get("reasoningSummary").cloned();
        let service_tier = base
            .extra
            .get("serviceTier")
            .and_then(Value::as_str)
            .map(str::to_string);
        let tool_choice = base.extra.get("toolChoice").cloned();
        OpenAIResponsesOptions {
            base,
            reasoning_effort,
            reasoning_summary,
            service_tier,
            tool_choice,
        }
    }
}

/// `Required<OpenAIResponsesCompat>`。
///
/// 模型/网关的能力开关，决定请求里出现哪些字段、工具走顶层还是 `additional_tools`、
/// header 用哪种会话格式。由 [`get_compat`] 从 `Model.compat`（无类型的 JSON 对象，
/// schema 随 api 而变）逐键读取并补齐默认值；键缺失或值不是布尔时一律用默认值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAIResponsesCompat {
    /// 推理模型是否用 `developer` 角色代替 `system`，默认 `true`。
    ///
    /// 本文件不消费该字段：`convert_responses_messages` 内部直接从 `model.compat` 读同名键
    /// （它的入参没有这个布尔）。字段保留在结构里，与 TS `getCompat` 的返回形状对齐。
    pub supports_developer_role: bool,
    /// 是否允许在对话中途出现 system 消息；为假时 `resolve_transcript` 会把它们折叠到
    /// 开头。默认 `false`。
    pub supports_mid_convo_system_messages: bool,
    /// `"openrouter"` 或 `"openai"`。
    ///
    /// 运行时还接受 `"openai-nosession"`（模型目录给 opencode 等代理用）：除前两种取值外，
    /// 会话头只发 `x-client-request-id`，不发 `session_id` / `x-session-id`。缺省由
    /// `detect_session_affinity_format` 推断：provider 是 `openrouter` 或 base_url 含
    /// `openrouter.ai` 时用 `openrouter`，否则 `openai`。
    pub session_affinity_format: String,
    /// 是否支持长缓存保留，默认 `true`。`cacheRetention == long` 时，没有显式缓存模式则表现为
    /// `prompt_cache_retention: "24h"`；有显式模式且支持长保留则表现为
    /// `prompt_cache_options.ttl: "30m"`。
    pub supports_long_cache_retention: bool,
    /// 工具定义是否发送 `strict` 字段，默认 `false`。为假时不写该键。
    pub supports_strict_mode: bool,
    /// 是否支持 OpenAI 的 grammar 约束采样，默认 `false`。
    ///
    /// 为真时带 grammar 配置的工具会转成 `custom` 类型，并在 `run_responses_inner` 里生成
    /// 「工具名 → 输入参数名」映射；为假时这类工具按普通 function 工具发送（不报错）。
    /// 为真但配置里没有可用变体、或推断不出输入参数名时，整次请求以错误结束。
    pub supports_openai_grammar_tools: bool,
    /// 是否支持 input 里「按消息锚定」的 `additional_tools` 项，默认 `false`。
    pub supports_additional_tools: bool,
    /// 是否支持客户端执行的 tool search（transcript 锚定的增量工具），默认 `false`。
    pub supports_tool_search: bool,
    /// 是否接受 `prompt_cache_options`（显式缓存模式），默认 `false`；旧模型会拒绝该参数。
    /// 为真时它取代 `prompt_cache_retention`。
    pub supports_explicit_prompt_cache_mode: bool,
    /// 是否允许发送 `max_output_tokens`，默认 `true`（某些 Codex 协议网关会拒绝）。
    pub supports_max_output_tokens: bool,
    /// 是否支持对话中途换档（`configuration_update`），默认 `false`。
    /// 本文件用它决定 Responses 侧的中途换档方案，`anthropic_messages` 也读取同名键；
    /// 在 openai provider 的 ChatGPT 登录态下本适配器不启用该方案。
    pub supports_mid_convo_effort: bool,
}

fn compat_bool(model: &Model, key: &str) -> Option<bool> {
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.get(key))
        .and_then(Value::as_bool)
}

fn compat_str(model: &Model, key: &str) -> Option<String> {
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `detectSessionAffinityFormat(model)`。
fn detect_session_affinity_format(model: &Model) -> String {
    if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
        "openrouter".to_string()
    } else {
        "openai".to_string()
    }
}

/// `getCompat(model)`。
///
/// 返回**已补齐默认值**的 [`OpenAIResponsesCompat`]：调用方不必再处理 `Option`。
/// 每个键独立读取，缺失即默认；值必须是 JSON 布尔，字符串 `"true"` 之类会被当成未设置
/// （`compat_bool` 用 `Value::as_bool`）——schema 要求布尔，这里不对错误类型做纠正。
pub fn get_compat(model: &Model) -> OpenAIResponsesCompat {
    OpenAIResponsesCompat {
        supports_developer_role: compat_bool(model, "supportsDeveloperRole").unwrap_or(true),
        supports_mid_convo_system_messages: compat_bool(model, "supportsMidConvoSystemMessages")
            .unwrap_or(false),
        session_affinity_format: compat_str(model, "sessionAffinityFormat")
            .unwrap_or_else(|| detect_session_affinity_format(model)),
        supports_long_cache_retention: compat_bool(model, "supportsLongCacheRetention")
            .unwrap_or(true),
        supports_strict_mode: compat_bool(model, "supportsStrictMode").unwrap_or(false),
        supports_openai_grammar_tools: compat_bool(model, "supportsOpenAIGrammarTools")
            .unwrap_or(false),
        supports_additional_tools: compat_bool(model, "supportsAdditionalTools").unwrap_or(false),
        supports_tool_search: compat_bool(model, "supportsToolSearch").unwrap_or(false),
        supports_explicit_prompt_cache_mode: compat_bool(model, "supportsExplicitPromptCacheMode")
            .unwrap_or(false),
        supports_max_output_tokens: compat_bool(model, "supportsMaxOutputTokens").unwrap_or(true),
        supports_mid_convo_effort: compat_bool(model, "supportsMidConvoEffort").unwrap_or(false),
    }
}

/// `isChatGPTSignIn(model, apiKey)`。
///
/// 判定「Sign in with ChatGPT」OAuth 登录态，三个条件必须同时成立：provider 为 `openai`、
/// base_url 恰为 `https://api.openai.com/v1`（共享网关或自定义端点不算）、API key 存在且
/// 不以 `sk-` 开头。三个条件是当前稳定判定契约，改动需同步测试。
///
/// 命中后请求体会省略若干字段（见 `build_params`），错误信息也可能追加 ChatGPT 用量提示。
/// 副作用：如果调用方给官方端点传了一个不以 `sk-` 开头的自有凭据，也会被当成登录态。
fn is_chatgpt_sign_in(model: &Model, api_key: Option<&str>) -> bool {
    model.provider == "openai"
        && model.base_url == "https://api.openai.com/v1"
        && api_key.is_some_and(|api_key| !api_key.starts_with("sk-"))
}

/// `hasHeader(headers, name)`：header 名大小写不敏感，且值必须非空（按 `js_trim` 判空白）。
///
/// 用于判断调用方是否已经通过 `authorization` / `cf-aig-authorization` 头自带鉴权；
/// `None`（显式删除）与空值都不算提供。
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
/// 解析顺序：非空 `apiKey` 原样返回；否则若调用方自带 `authorization` 或
/// `cf-aig-authorization` 头，返回占位串 `"unused"`（TS 的 OpenAI 客户端构造要求 apiKey
/// 非空；若给的是 `authorization`，`build_headers` 后面合并的同名头会覆盖 `Bearer unused`）；
/// 两者都没有时返回 [`AiErrorKind::Auth`] 错误，消息逐字为
/// `No API key for provider: <provider>`。
///
/// 注意：`openai-completions` / `openai-responses` **不**接受 `x-api-key`（与 Anthropic 不同）。
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
/// 优先级：显式选项 > 环境变量 `KIANA_CACHE_RETENTION == "long"` > `Short`。
/// 环境变量经 `get_provider_env_value` 读取（provider scoped env 覆盖进程 env），且只识别
/// `"long"` 这一个值，其它字符串与缺失等价。
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
///
/// 三态在调用点有区别：`Some(None)` 表示模型显式声明不支持该档（`build_params` 的 `off`
/// 分支据此跳过 `reasoning`），`None` 表示没有配置、按默认值处理。
fn mapped_thinking_level(model: &Model, level: ModelThinkingLevel) -> Option<Option<String>> {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .cloned()
}

fn mapped_off(model: &Model) -> Option<Option<String>> {
    mapped_thinking_level(model, ModelThinkingLevel::Off)
}

/// `getConfigurationUpdateEffort(model, options, compat)`。
///
/// 返回本回复应记录到 `output.provider_thinking_level`、并以 `configuration_update` input
/// 项回放的 provider 原生档位。三个条件缺一不可：模型支持 `supports_mid_convo_effort`、
/// 请求带了 `reasoning_effort`、且不是 ChatGPT 登录态（该组合下不发送 `configuration_update`
/// 条目，见 `tests/openai_responses_mid_convo_effort.rs`）。
///
/// 取值先查 `model.thinking_level_map`，没有该档映射、或映射为 `null` 时回退到 Kiana 档位名
/// （`"high"` 等），与 TS 的 `?? options.reasoningEffort` 一致。
fn get_configuration_update_effort(
    model: &Model,
    options: &OpenAIResponsesOptions,
    compat: &OpenAIResponsesCompat,
) -> Option<String> {
    if !compat.supports_mid_convo_effort
        || options.reasoning_effort.is_none()
        || is_chatgpt_sign_in(model, options.base.api_key.as_deref())
    {
        return None;
    }
    let level = options.reasoning_effort?;
    Some(
        mapped_thinking_level(model, to_model_thinking_level(level))
            .flatten()
            .unwrap_or_else(|| thinking_level_str(level).to_string()),
    )
}

/// `getPromptCacheRetention(compat, cacheRetention)`。
///
/// 只在「长保留 + 模型支持 + 没有显式缓存模式」时返回 `"24h"`。与
/// `get_prompt_cache_options` 互斥：显式缓存模式开启时本函数返回 `None`。
fn get_prompt_cache_retention(
    compat: &OpenAIResponsesCompat,
    cache_retention: CacheRetention,
) -> Option<String> {
    (cache_retention == CacheRetention::Long
        && compat.supports_long_cache_retention
        && !compat.supports_explicit_prompt_cache_mode)
        .then(|| "24h".to_string())
}

/// `getPromptCacheOptions(compat, cacheRetention)`。
///
/// 仅在模型支持显式缓存模式时生效：`None` → `{"mode":"explicit"}`，
/// `Long` 且支持长保留 → `{"ttl":"30m"}`，其余（含 `Short`）→ 不发字段、用服务端默认。
fn get_prompt_cache_options(
    compat: &OpenAIResponsesCompat,
    cache_retention: CacheRetention,
) -> Option<Value> {
    if !compat.supports_explicit_prompt_cache_mode {
        return None;
    }
    if cache_retention == CacheRetention::None {
        return Some(serde_json::json!({ "mode": "explicit" }));
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        return Some(serde_json::json!({ "ttl": "30m" }));
    }
    None
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// `buildParams(...)`。
///
/// 构造 `/responses` 的请求体，并按 TS 对象字面量的顺序插入键（workspace 的 `serde_json`
/// 启用 `preserve_order`，插入顺序即 JSON 输出顺序）：先是 `model` / `input` / `stream`，
/// 然后是缓存字段与 `store`，之后按条件追加 `max_output_tokens`、`temperature`、
/// `service_tier`、`tools`、`tool_choice`、`reasoning` / `include`，最后并入采样参数。
///
/// - `context` 应已由 `stream` 归一化（`resolve_transcript`）；本函数不再折叠 system 消息。
/// - `grammar_tool_input_properties` 用于消息转换（历史 `custom_tool_call` 回放时查输入
///   参数名，并决定回放的 item id 前缀）与流式解析（`process_responses_stream` 判定
///   `custom_tool_call` 的输入参数名）。顶层工具转换不读取它：`convert_responses_tools`
///   只按 `supports_openai_grammar_tools` 对每个 `Tool` 重新解析 grammar。
/// - 错误：消息/工具转换可能因 grammar 配置或 `strict: "require"` 不被支持而返回
///   [`AiError`]，整次请求在发网之前就失败。
/// - ChatGPT 登录态下 `max_output_tokens`、`temperature`、`prompt_cache_retention`、
///   `prompt_cache_options` 不出现，其余字段照常。
/// - 返回值可被 `on_payload` 整体替换，本函数不感知。
#[allow(clippy::too_many_arguments)]
fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAIResponsesOptions,
    compat: &OpenAIResponsesCompat,
    grammar_tool_input_properties: &IndexMap<String, String>,
    configuration_update_effort: Option<&str>,
) -> Result<Value, AiError> {
    // 把工具相关的 compat 开关一次性放进 convert_options：tool_support 决定顶层是否发
    // tools，tool_options 决定 strict / grammar 的用法；消息转换与工具转换共用这一份配置。
    let convert_options = ConvertResponsesMessagesOptions {
        include_system_prompt: None,
        grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
        supports_mid_convo_system_messages: Some(compat.supports_mid_convo_system_messages),
        supports_additional_tools: Some(compat.supports_additional_tools),
        supports_tool_search: Some(compat.supports_tool_search),
        tool_options: Some(ConvertResponsesToolsOptions {
            strict: None,
            supports_strict_mode: Some(compat.supports_strict_mode),
            supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools),
            tool_search_result: false,
        }),
        configuration_update_effort: configuration_update_effort.map(str::to_string),
    };
    // 支持级别决定工具如何出现在请求里：None → 顶层 `tools` 发当前工具集；Additions →
    // 顶层 `tools` 只发折叠前缀对应的工具集，前缀之后的新增写进 input 的 `tool_search_call`
    // / `tool_search_output` 项；Changes → 顶层 `tools` 为空，全部工具写进 input 的
    // `additional_tools` 项。消息转换内部会按同一份选项再算一次（`get_responses_tool_support`）。
    let tool_support = get_responses_tool_support(&convert_options);
    let transcript_tools = resolve_transcript_tools(&context.as_context().messages, tool_support);
    let messages = convert_responses_messages(
        model,
        context,
        &OPENAI_TOOL_CALL_PROVIDERS,
        Some(&convert_options),
    )?;

    // 这里重新解析一次 cacheRetention（`run_responses_inner` 也算过一次）：两次结果相同，
    // 保持与 TS 在 `buildParams` 内部单独解析的结构一致。
    let cache_retention =
        resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    // Sign in with ChatGPT 拒绝这些请求字段。
    let omit_unsupported_fields = is_chatgpt_sign_in(model, options.base.api_key.as_deref());

    let mut params: Map<String, Value> = Map::new();
    params.insert("model".into(), Value::String(model.id.clone()));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), Value::Bool(true));
    // `prompt_cache_key` 只受 cacheRetention 控制（ChatGPT 登录态仍会发送）；session id
    // 超过 64 码点会被截断，`None` 时字段不出现，空串则原样保留。
    if cache_retention != CacheRetention::None
        && let Some(key) = clamp_openai_prompt_cache_key(options.base.session_id.as_deref())
    {
        params.insert("prompt_cache_key".into(), Value::String(key));
    }
    if !omit_unsupported_fields
        && let Some(retention) = get_prompt_cache_retention(compat, cache_retention)
    {
        params.insert("prompt_cache_retention".into(), Value::String(retention));
    }
    if !omit_unsupported_fields
        && let Some(cache_options) = get_prompt_cache_options(compat, cache_retention)
    {
        params.insert("prompt_cache_options".into(), cache_options);
    }
    params.insert("store".into(), Value::Bool(false));

    // `max_tokens == 0` 按 JS 真值视为未提供；支持该字段时抬到 16 的下限再发送。
    if let Some(max_tokens) = options.base.max_tokens.filter(|value| *value != 0)
        && compat.supports_max_output_tokens
        && !omit_unsupported_fields
    {
        params.insert(
            "max_output_tokens".into(),
            Value::from(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(temperature) = options.base.temperature
        && !omit_unsupported_fields
    {
        params.insert("temperature".into(), Value::from(temperature));
    }
    // `service_tier` 不参与 ChatGPT 登录态的字段过滤：只要调用方给了就发送。
    if let Some(service_tier) = &options.service_tier {
        params.insert("service_tier".into(), Value::String(service_tier.clone()));
    }
    // 只有顶层工具非空时才发 `tools`：Changes 级别下工具都在 input 的增量条目里，
    // 这里自然为空。
    if !transcript_tools.request_tools.is_empty() {
        params.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(
                &transcript_tools.request_tools,
                Some(&ConvertResponsesToolsOptions {
                    strict: None,
                    supports_strict_mode: Some(compat.supports_strict_mode),
                    supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools),
                    tool_search_result: false,
                }),
            )?),
        );
    }
    if let Some(tool_choice) = &options.tool_choice {
        params.insert("tool_choice".into(), tool_choice.clone());
    }

    // summary 按 JS 真值判断；单独的 summary 会把档位隐含为 "medium"（见下面
    // reasoning_effort 的 or_else），因此「只要 summary、不要 effort」也能得到 reasoning。
    let reasoning_summary = options
        .reasoning_summary
        .as_ref()
        .filter(|value| truthy(Some(value)));
    let reasoning_effort_raw = options.reasoning_effort;
    let reasoning_effort = reasoning_effort_raw
        .map(|level| thinking_level_str(level).to_string())
        .or_else(|| reasoning_summary.map(|_| "medium".to_string()));
    // 三个分支：
    // 1) 有档位：写 `reasoning.effort` + `reasoning.summary`，并请求加密推理内容。
    //    若启用中途换档，`effort` 固定为 medium（真实档位在 input 的
    //    `configuration_update` 项里），否则用映射后的请求档位。
    // 2) 无档位、provider 不是 github-copilot、且 `off` 未被映射为 null：只写
    //    `reasoning.effort`（映射值，缺省 "none"）。
    // 3) 其余情况不写 `reasoning`。
    // `xai` 在任何分支之后都要求 `include: ["reasoning.encrypted_content"]`。
    if model.reasoning == Some(true) {
        if let Some(effort) = &reasoning_effort {
            let requested_effort = match reasoning_effort_raw {
                Some(level) => mapped_thinking_level(model, to_model_thinking_level(level))
                    .flatten()
                    .unwrap_or_else(|| thinking_level_str(level).to_string()),
                None => effort.clone(),
            };
            let effort = if configuration_update_effort.is_some() {
                CONFIGURATION_UPDATE_REQUEST_EFFORT.to_string()
            } else {
                requested_effort
            };
            let summary = match reasoning_summary {
                Some(value) => js_string(Some(value)),
                None => "auto".to_string(),
            };
            let mut reasoning = Map::new();
            reasoning.insert("effort".into(), Value::String(effort));
            reasoning.insert("summary".into(), Value::String(summary));
            params.insert("reasoning".into(), Value::Object(reasoning));
            params.insert(
                "include".into(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".into())]),
            );
        } else if model.provider != "github-copilot" && mapped_off(model) != Some(None) {
            let effort = mapped_off(model)
                .flatten()
                .unwrap_or_else(|| "none".to_string());
            let mut reasoning = Map::new();
            reasoning.insert("effort".into(), Value::String(effort));
            params.insert("reasoning".into(), Value::Object(reasoning));
        }
        if model.provider == "xai" {
            params.insert(
                "include".into(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".into())]),
            );
        }
    }

    // 采样参数用**请求的档位**而不是最终 wire effort：只有 summary 时为 Medium，
    // 两者都没有时为 Off。`resolve_sampling_params` 内部会再 clamp 到模型支持的档位，
    // 并按「模型默认 < 该档位参数 < 请求参数」合并；返回 None 时不写任何字段。
    let thinking_level = reasoning_effort_raw
        .map(to_model_thinking_level)
        .or_else(|| reasoning_summary.map(|_| ModelThinkingLevel::Medium))
        .unwrap_or(ModelThinkingLevel::Off);
    let sampling_params =
        resolve_sampling_params(model, thinking_level, options.base.sampling_params.as_ref());
    if let Some(sampling_params) = sampling_params {
        for (key, value) in sampling_params {
            params.insert(key, value);
        }
    }
    Ok(Value::Object(params))
}

// ---------------------------------------------------------------------------
// header / service tier
// ---------------------------------------------------------------------------

/// `createClient(...)` 的 header 部分（Responses 版**没有** `x-session-affinity`）。
///
/// 合并顺序（同名 header 后写覆盖先写，并在最终列表里移到末尾）：
/// `Accept` / `Authorization: Bearer <apiKey>` → identity（User-Agent、attribution）→
/// `model.headers` → Copilot 动态头 → 会话亲和 → 调用方 `options.headers` → `Content-Type`。
/// 因此调用方显式给的 header 覆盖前面各层（`Content-Type` 除外：它最后统一设置；
/// `options.headers` 里的 `None` 表示删除同名 header）。
///
/// `session_id`：cache 关闭时调用方传 `None`，否则原样传入（可能是空串）；空串由本函数
/// 末尾的 `filter(|session_id| !session_id.is_empty())` 丢弃。两种情况下都不会产生会话头。
/// 不返回错误：header 合并是全量的。
fn build_headers(
    model: &Model,
    context: &TranscriptContext,
    api_key: &str,
    options: &OpenAIResponsesOptions,
    compat: &OpenAIResponsesCompat,
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
    // 会话亲和格式：openrouter 只发 x-session-id；openai 发 session_id + x-client-request-id；
    // 其它取值（模型目录里的 openai-nosession）只发 x-client-request-id。
    // Responses 没有 x-session-affinity（与 completions 的三处差别之一）。
    if let Some(session_id) = session_id.filter(|session_id| !session_id.is_empty()) {
        if compat.session_affinity_format == "openrouter" {
            set_header(&mut table, "x-session-id", session_id);
        } else {
            if compat.session_affinity_format == "openai" {
                set_header(&mut table, "session_id", session_id);
            }
            set_header(&mut table, "x-client-request-id", session_id);
        }
    }
    if let Some(options_headers) = &options.base.headers {
        merge_provider_headers(&mut table, options_headers);
    }
    set_header(&mut table, "Content-Type", "application/json");
    header_list(table)
}

/// `getServiceTierCostMultiplier(model, serviceTier)`。
///
/// 倍率表：`flex` → 0.5；`priority` / `fast` → 2.0（请求模型 id 为 `gpt-5.5` 时 2.5）；
/// 其它（含 `None`、`"auto"`、`"default"`）→ 1.0。
///
/// `gpt-5.5` 的 2.5 倍按**请求模型 id** 判定（不是按响应返回的 tier）：`gpt-6-luna`
/// 请求 `priority`、响应 `fast` 时仍按 2 倍计价（回归 #10034，见
/// `tests/openai_responses_compat.rs` 的 `applies_service_tier_cost_multipliers`）。
fn get_service_tier_cost_multiplier(model_id: &str, service_tier: Option<&str>) -> f64 {
    match service_tier {
        Some("flex") => 0.5,
        Some("priority") | Some("fast") => {
            if model_id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

/// `applyServiceTierPricing(usage, serviceTier, model)`。
///
/// 对 `input` / `output` / `cache_read` / `cache_write` 四项分别乘倍率，再用四项之和重算
/// `total`（原来的 total 被覆盖）。倍率恰为 1.0 时直接返回，原始数值保持不变。
/// 调用点在 `process_responses_stream` 的终态处理里：先 `calculate_cost`，再应用倍率。
fn apply_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model_id: &str) {
    let multiplier = get_service_tier_cost_multiplier(model_id, service_tier);
    if multiplier == 1.0 {
        return;
    }
    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// `run_responses_inner` 的错误载体：除 [`AiError`] 外还保留最后一次 HTTP 尝试的失败信息，
/// 让 catch 等价逻辑可以复刻 SDK 错误对象的 status 与已解析 body（交给 `format_thrown_error`）。
struct ResponsesRunError {
    /// 面向调用方的错误；其 `message` 是 TS 错误对象的 `message`。
    error: AiError,
    /// 最后一次请求失败的 status/body；流或解析阶段的错误为 `None`。
    last_failure: Option<RequestFailure>,
}

// 非 HTTP 来源的错误（凭据、流提前结束、中止等）没有对应的失败响应。
impl From<AiError> for ResponsesRunError {
    fn from(error: AiError) -> Self {
        ResponsesRunError {
            error,
            last_failure: None,
        }
    }
}

/// 执行一次 Responses 请求并把结果写进 `output`。
///
/// 成功路径由本函数推 `done` 并 `end`；失败路径只返回 `ResponsesRunError`，`error` 事件
/// 与 `end` 由 `run_responses` 负责。中途可能已经推送过 `start` 与增量事件，因此调用方
/// 必须容忍「先有部分内容、再收到 error」。
///
/// 副作用：发起 HTTP 请求、调用 `on_payload` / `on_response` / `on_provider_stream_event`
/// 回调、向 `stream` 推事件。`output.error_message` 可能被流处理覆盖（例如
/// `response.incomplete` 的原因）。
async fn run_responses_inner(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAIResponsesOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    compat: &OpenAIResponsesCompat,
    configuration_update_effort: Option<&str>,
) -> Result<(), ResponsesRunError> {
    let api_key = get_client_api_key(
        &model.provider,
        options.base.api_key.as_deref(),
        options.base.headers.as_ref(),
    )?;
    let cache_retention =
        resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    // cache 关闭时连 session 亲和头一起省略：session id 在这里只用于缓存路由。
    let cache_session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.base.session_id.clone()
    };
    // 工具声明来自 transcript 的 system 消息（`normalize_context` 把 `Context.tools` 折进
    // 首条 system 消息的 `tools_added`），不是重新读 `Context.tools`。
    let declared_tools = get_declared_tools(&context.as_context().messages);
    // 可能因 grammar 配置无效而失败：此时还没发请求，`run_responses` 会直接产出 error 事件。
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        Some(&declared_tools),
        compat.supports_openai_grammar_tools,
    )?;
    let headers = build_headers(
        model,
        context,
        &api_key,
        options,
        compat,
        cache_session_id.as_deref(),
    );
    let mut params = build_params(
        model,
        context,
        options,
        compat,
        &grammar_tool_input_properties,
        configuration_update_effort,
    )?;
    // `on_payload` 可返回替换后的整个 payload，`None` 表示不修改（TS 的
    // `onPayload?.(params, model) ?? params`）；先克隆是因为回调拿走所有权、原值仍需保留。
    if let Some(next_params) = options.base.apply_on_payload(params.clone(), model).await {
        params = next_params;
    }
    // URL 由 `build_url` 做 WHATWG 归一化（等价 SDK 的 `new URL(baseURL + path)`）；
    // 非法 base_url 会在这里返回 "Invalid URL"。
    let request = HttpRequest {
        method: "POST".to_string(),
        url: build_url(&model.base_url, "/responses", &[])?,
        headers,
        body: Some(crate::http::json_body(&params)),
        timeout_ms: options.base.timeout_ms,
    };
    let client = default_http_client(&options.base);
    // 除最终响应外还返回最后一次失败信息：HTTP 非 2xx 时 SDK 错误对象带 status 与解析后的
    // body，catch 需要用它格式化（见 `ResponsesRunError.last_failure`）。
    let (response, last_failure) = request_with_retry(
        client,
        request,
        options.base.signal.clone(),
        options.base.max_retries,
        options.base.max_retry_delay_ms,
    )
    .await;
    let response = response.map_err(|error| ResponsesRunError {
        error,
        last_failure,
    })?;
    // `on_response` 在读取 body 之前触发，只拿得到 status 与响应头（与 TS 的
    // `withResponse()` 一致）。
    if let Some(on_response) = &options.base.on_response {
        on_response(
            provider_response(response.response.status, &response.response.headers),
            model,
        );
    }
    // `push` 在当前实现里不会失败（见 `StreamPushError` 的文档）；`let _` 对应 TS 对已结束
    // 流的静默忽略。
    let _ = stream.push(crate::types::AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });
    // 204/205 的响应没有 body：SDK 在迭代流时抛 OpenAIError（start 已推送之后）。
    ensure_response_body(response.response.status)?;

    // 闭包只捕获被请求的模型 id：倍率按请求模型判定，clone 是为了满足 'static 约束。
    let pricing_model_id = model.id.clone();
    let stream_options = OpenAIResponsesStreamOptions {
        on_provider_stream_event: options.base.on_provider_stream_event.clone(),
        service_tier: options.service_tier.clone(),
        grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
        resolve_service_tier: None,
        apply_service_tier_pricing: Some(Arc::new(move |usage, service_tier| {
            apply_service_tier_pricing(usage, service_tier, &pricing_model_id);
        })),
    };
    // SSE 解析在独立任务里运行，这里逐事件消费；解析错误或协议错误（缺终态事件、工具调用
    // 未收尾、`response.failed` 等）都以 `?` 返回，转成 `ResponsesRunError`。
    let events = provider_event_stream(response.body, options.base.signal.clone());
    process_responses_stream(events, output, stream, model, Some(&stream_options)).await?;

    // 收尾顺序固定：取消优先（即便流已正常结束，只要 signal 已触发也报 aborted）；
    // 其次是「流结束但没有 stop reason」；最后把 aborted/error 当异常抛出，由
    // `run_responses` 统一转成 `error` 事件。
    if options
        .base
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled())
    {
        return Err(AiError::new(AiErrorKind::Other, "Request was aborted").into());
    }
    if output.stop_reason == StopReason::Pending {
        return Err(AiError::new(
            AiErrorKind::Other,
            "OpenAI Responses stream ended without a stop reason",
        )
        .into());
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        let message = output
            .error_message
            .clone()
            .unwrap_or_else(|| "An unknown error occurred".to_string());
        return Err(AiError::new(AiErrorKind::Other, message).into());
    }
    let message = output.clone();
    let _ = stream.push(crate::types::AssistantMessageEvent::Done {
        reason: done_reason(output.stop_reason),
        message: Box::new(message.clone()),
    });
    stream.end(message);
    Ok(())
}

/// 把内部 stop reason 映射成 `done` 事件允许的取值。
///
/// 调用前已排除 `Pending` / `Error` / `Aborted`，因此 `Stop` 与其它未列出的值都落到
/// `DoneReason::Stop`；`Deferred` 原样保留。
fn done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

/// `run_responses_inner` 的 catch 等价物：把错误变成 `error` 事件并结束流。
///
/// 顺序固定：先按 signal 决定 `stop_reason` 是 `aborted` 还是 `error`（TS 只看
/// `options?.signal?.aborted`，与错误本身来自哪里无关），再格式化 `error_message`
/// （HTTP 失败优先使用最后一次的 status/body），最后推 `error` 并 `end`。
/// `push` 在已结束的流上是 no-op、`end` 可重复调用且不覆盖已提交结果，所以重复执行安全。
async fn run_responses(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAIResponsesOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    compat: &OpenAIResponsesCompat,
    configuration_update_effort: Option<String>,
) {
    let result = run_responses_inner(
        model,
        context,
        options,
        output,
        stream,
        compat,
        configuration_update_effort.as_deref(),
    )
    .await;
    let Err(failure) = result else {
        return;
    };
    // 取消优先：即使错误本身是 HTTP/解析失败，只要 signal 已触发就按 aborted 上报。
    let aborted = options
        .base
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled());
    output.stop_reason = if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    // 前缀只区分大小写敏感的字面量 `OpenAI` 与其它 provider 名；`format_thrown_error`
    // 在没有 status/body 时返回纯 message，有则在前面拼 `<prefix> (<status>): `。
    let prefix = format!(
        "{} API error",
        if model.provider == "openai" {
            "OpenAI"
        } else {
            model.provider.as_str()
        }
    );
    let error_message = format_thrown_error(failure.error, failure.last_failure, Some(&prefix));
    // Sign in with ChatGPT 与其它应用共享订阅额度；ChatGPT Business 还有工作区级上限。
    // 两个判定都是**子串包含**，因此错误正文里带 code 也能命中；提示语是用户可见文案，改动需同步测试。
    output.error_message = Some(
        if error_message.contains("subscription_sharing_usage_limit_exceeded") {
            format!("{error_message}\nCheck your ChatGPT usage: {CHATGPT_USAGE_URL}")
        } else if error_message
            .contains("subscription_sharing_workplace_individual_usage_limit_exceeded")
        {
            format!("{error_message}\n{CHATGPT_WORKSPACE_LIMIT_HINT}")
        } else {
            error_message
        },
    );
    // 事件 reason 与 stop_reason 同源：消费方只看事件也能区分取消与失败。
    let reason = if aborted {
        ErrorReason::Aborted
    } else {
        ErrorReason::Error
    };
    // 先推 error 事件（会顺带写入 durationMs），再用同一份消息提交 `end` 的结果。
    let _ = stream.push(crate::types::AssistantMessageEvent::Error {
        reason,
        error: Box::new(output.clone()),
    });
    stream.end(output.clone());
}

/// `stream`（导出）：完整选项。
///
/// 同步完成三件不依赖网络的事——读取 compat、归一化 transcript、计算
/// `configuration_update_effort`——然后把请求交给后台任务并立即返回事件流。因此本函数
/// 不返回 `Result`：鉴权、网络、解析错误都以 `error` 事件出现在流里。
///
/// 事件顺序取决于请求是否成功建立：拿到响应并触发 `on_response` 之后是 `start`（初始
/// `AssistantMessage`）→ 若干内容增量 → `done` 或 `error`；建立请求前的失败（缺少 API
/// key、URL/请求体/grammar 映射构造失败、HTTP 重试耗尽）不推 `start`，只推一个 `error`
/// 事件并结束流，因此流的首个事件也可能是 `error`。
/// 取消通过 `options.base.signal`（`CancellationToken`）传递：它被交给 HTTP 重试与 SSE
/// 读取，并在收尾时决定 `aborted` 的判定。
///
/// 初始消息的 `api` / `provider` / `model` 取自模型，`stop_reason = Pending`，`timestamp`
/// 是创建时刻（`durationMs` 由事件流在终态事件时补写）；启用中途换档时额外带
/// `provider_thinking_level`。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: impl Into<OpenAIResponsesOptions>,
) -> AssistantMessageEventStream {
    let options = options.into();
    let stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let context = context.clone();
    let compat = get_compat(&model);
    // 与 TS 一样在同步前缀里归一化 context：不支持中途 system 消息时折叠到开头。
    let normalized_context =
        resolve_transcript(&context, Some(compat.supports_mid_convo_system_messages));
    let configuration_update_effort = get_configuration_update_effort(&model, &options, &compat);
    let stream_for_task = stream.clone();
    // `spawn_forward` 在已有 tokio runtime 时直接 spawn，否则用专用线程跑，调用方不必
    // 处于 async 上下文中（同步返回语义不变）。
    spawn_forward(async move {
        let mut output = AssistantMessage {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            provider_thinking_level: configuration_update_effort.clone(),
            stop_reason: StopReason::Pending,
            timestamp: now_ms(),
            ..AssistantMessage::default()
        };
        run_responses(
            &model,
            &normalized_context,
            &options,
            &mut output,
            &stream_for_task,
            &compat,
            configuration_update_effort,
        )
        .await;
    });
    stream
}

/// `streamSimple`（导出）：缺鉴权时同步返回 `Err`。
///
/// 这是 TS 契约里唯一允许同步抛出的场景（`spec/ai-api-anthropic-openai.md` §1.3 第 2 条）：
/// 先做一次与 `run_responses_inner` 相同的鉴权检查，把 `No API key for provider: <p>` 提前
/// 到调用点，而不是变成流里的 `error` 事件。
///
/// 其余步骤：`build_base_options` 复制基础选项（并按上下文窗口钳制 `max_tokens`、合并采样
/// 参数）；思考档位先 `clamp_thinking_level` 到模型支持的范围，`Off` 映射为“不传 effort”；
/// `tool_choice` 序列化成 JSON。`thinking_budgets` 不透传（Responses 没有对应字段）。
///
/// 返回的流与 [`stream`] 完全相同；鉴权以外的失败仍走事件流。
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
    // 先按模型支持的档位 clamp；`Off` 表示关闭思考，映射为“不设置 reasoningEffort”。
    let clamped_reasoning = options
        .reasoning
        .map(|level| clamp_thinking_level(model, to_model_thinking_level(level)));
    let reasoning_effort = clamped_reasoning.and_then(|level| match level {
        ModelThinkingLevel::Off => None,
        other => Some(model_level_to_thinking_level(other)),
    });
    // 序列化失败会被静默降级为 `None`（请求里不出现 `tool_choice`），不向上报错。
    let tool_choice = options
        .tool_choice
        .and_then(|choice| serde_json::to_value(choice).ok());
    Ok(stream(
        model,
        context,
        OpenAIResponsesOptions {
            base,
            reasoning_effort,
            reasoning_summary: None,
            service_tier: None,
            tool_choice,
        },
    ))
}

/// `ModelThinkingLevel` → `ThinkingLevel` 的全量映射。
///
/// `Off` 的唯一调用点（`stream_simple`）已先把它过滤掉，因此 `Off → Low` 分支在当前代码里
/// 不可达；保留该分支保证 match 穷尽，若以后有调用点直接传入 `Off`，会得到 `Low`。
fn model_level_to_thinking_level(level: ModelThinkingLevel) -> ThinkingLevel {
    match level {
        ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ModelThinkingLevel::Low => ThinkingLevel::Low,
        ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ModelThinkingLevel::High => ThinkingLevel::High,
        ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        ModelThinkingLevel::Max => ThinkingLevel::Max,
        ModelThinkingLevel::Off => ThinkingLevel::Low,
    }
}

/// 无状态的 Responses 适配器。
///
/// 只作为 `ProviderStreams` 的实现载体：没有任何字段，所有状态都在每次调用的局部变量里，
/// 因此同一个实例可以被多个并发请求共享。
struct OpenAIResponsesStreams;

#[async_trait]
impl ProviderStreams for OpenAIResponsesStreams {
    // 转发到自由函数；`StreamOptions.extra` 由 `from_stream_options` 还原成 Responses 字段。
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            OpenAIResponsesOptions::from_stream_options(options),
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // trait 方法必须同步返回流，无法把 `Err` 交给调用方：把错误包装成一个只产出该
        // error 事件的流（`lazy_stream_error`），保持“失败以事件结束”的统一语义。
        match stream_simple(model, context, options) {
            Ok(stream) => stream,
            Err(error) => lazy_stream_error(model, error),
        }
    }
}

/// `openAIResponsesApi()`。
///
/// 返回 lazy 包装的 [`ProviderStreams`]：每次 `stream` / `stream_simple` 先执行加载器，
/// 而本加载器只构造一个无状态的 `OpenAIResponsesStreams`（无 IO、不会失败）。
/// capabilities 传 `None`，因此 `fetch_deferred` / `cancel_deferred` 都按“不支持”处理。
pub fn openai_responses_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| {
            Box::pin(async { Ok(Arc::new(OpenAIResponsesStreams) as Arc<dyn ProviderStreams>) })
        }),
        None,
    )
}

#[cfg(test)]
mod golden {
    //! 与 TS 参考实现（`/home/ctyun/kiana-work/upstream-reference`）逐字节对照的请求体黄金测试。
    //!
    //! fixture 的 `responses` 段提供 model / context / params 三份数据；测试固定一组选项
    //! （maxTokens、temperature、sessionId、samplingParams、cacheRetention 以及
    //! reasoning / service tier / toolChoice），用 `js_stringify` 比较 `build_params` 的输出
    //! 与 TS `buildParams` 的 `params`。它锁定的是字段名、键序与默认值，不覆盖 HTTP 行为。

    use super::*;
    use crate::types::TranscriptContext;
    use serde_json::json;

    // fixture 随仓库提交，`responses.params` 是 TS 参考实现的期望输出，不要手改。
    const REQUEST_BODIES: &str =
        include_str!("../../tests/fixtures/api-openai/request_bodies.json");

    #[test]
    fn build_params_matches_typescript_reference() {
        let fixture: Value = serde_json::from_str(REQUEST_BODIES).expect("fixture parses");
        let model: Model =
            serde_json::from_value(fixture["responses"]["model"].clone()).expect("model");
        let context: TranscriptContext =
            serde_json::from_value(fixture["responses"]["context"].clone()).expect("context");
        let options = OpenAIResponsesOptions {
            base: StreamOptions {
                max_tokens: Some(1234),
                temperature: Some(0.3),
                session_id: Some("session-xyz".into()),
                sampling_params: Some(
                    serde_json::from_value(json!({ "top_p": 0.9 })).expect("params"),
                ),
                cache_retention: Some(CacheRetention::Long),
                ..StreamOptions::default()
            },
            reasoning_effort: Some(ThinkingLevel::High),
            reasoning_summary: Some(json!("detailed")),
            service_tier: Some("flex".into()),
            tool_choice: Some(json!({ "type": "function", "name": "read" })),
        };
        let grammar: IndexMap<String, String> = [("query".to_string(), "input".to_string())]
            .into_iter()
            .collect();
        let compat = get_compat(&model);
        let configuration_update_effort =
            get_configuration_update_effort(&model, &options, &compat);
        let actual = build_params(
            &model,
            &context,
            &options,
            &compat,
            &grammar,
            configuration_update_effort.as_deref(),
        )
        .expect("build params");
        assert_eq!(
            crate::api::openai_responses_shared::js_stringify(&actual),
            crate::api::openai_responses_shared::js_stringify(&fixture["responses"]["params"])
        );
    }
}
