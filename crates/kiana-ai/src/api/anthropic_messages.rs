//! `api/anthropic-messages.ts`：Anthropic Messages 的流式适配器。
//!
//! 覆盖：自定义 SSE 解码（不使用 SDK）、thinking / redacted thinking / signature、
//! cache_control（含 1h 保留）、strict 工具降级、eager tool input、OAuth（Claude Code 身份）、
//! GitHub Copilot 动态头、workload identity federation 与 mid-conversation effort。
//!
//! TS: `packages/ai/src/api/anthropic-messages.ts`。
//!
//! Rust 侧没有 `@anthropic-ai/sdk`，等价物是：
//! - 请求直接走 [`crate::http::HttpClient`]（`POST <baseUrl>/v1/messages?beta=true`，
//!   带 `anthropic-version`、`x-api-key` 或 `Authorization: Bearer`；`betas` 拼进
//!   `anthropic-beta` 头，`betas` 字段本身不进入 body）；
//! - 非 2xx 的错误消息按 SDK 的 `APIError.makeMessage` 公式拼装（`${status} ${JSON}`），
//!   并保留 `status` / `error.type` 供 strict 工具降级判定；
//! - federation 的 OIDC JWT-bearer 交换由本模块自行实现（`POST /v1/oauth/token`）。
//!
//! 与 TS 的可观察差异：SDK 的 `X-Stainless-*` 头不再发送（Rust 不是 JS SDK）；
//! 注入 SDK 客户端的 `options.client` 由 `options.fetch`（HttpClient）取代。
//!
//! # 设计原因
//!
//! TS 版依赖 `@anthropic-ai/sdk` 的 beta Messages 客户端，但流式解析、错误消息、超时与重试
//! 语义都由本文件自定义（SDK 的 `.asResponse()` 只负责发请求并返回原始响应）。Rust 侧没有
//! 同款 SDK，因此把 SDK 中**可观察**的部分逐条复刻：`buildHeaders` 的合并/删除顺序、
//! `APIError.makeMessage` 的文案、`TokenCache` 的刷新窗口、`timeout` 只覆盖响应头等。改动
//! 这些行为会改变下游可见的错误文本、头集合或计费结果，不能按“更合理”的方式重写。
//!
//! # 执行流程
//!
//! 1. [`stream`] 同步返回事件流，先按 `supports_mid_convo_system_messages` 归一化 transcript，
//!    并冻结当前工具列表（OAuth 下还原 Claude Code 工具名要用），随后把 `run_stream` 交给
//!    `spawn_forward` 在后台执行。
//! 2. `run_stream` 构造空的 `AssistantMessage`（`stop_reason = Pending`），调用
//!    `run_stream_inner`；后者依次完成：鉴权选择（federation / API key / 头自带鉴权）→
//!    `create_client` → strict 工具降级重试循环 → 发请求 → 逐事件消费 SSE 并增量更新同一个
//!    `output`。
//! 3. `run_stream_inner` 正常结束时推 `done` 事件并结束流；出错时由 `run_stream` 统一改写
//!    `stop_reason`/`error_message`，推 `error` 事件再结束流。因此公开的 [`stream`] 不返回
//!    `Result`：所有失败都经由事件流表达。
//!
//! # 关键约束
//!
//! - **超时只包住“到响应头为止”**：SDK 的 `timeout` 在收到响应头后清除，之后的 SSE 读取
//!   不再受该超时限制。`create_message_response` 用 `tokio::time::timeout` 包住
//!   `fetch.execute`，并把 `HttpRequest::timeout_ms` 设为 `None`。
//! - **重试只发生在建立响应阶段**：`retry_provider_request` 包住的是
//!   `create_message_response`；一旦 2xx 响应头到达，SSE 中途的读取/解析错误不会重试。
//! - **strict 工具降级只降级完整请求**：仅当最后一次失败是 400 + `invalid_request_error` +
//!   三个协议子串之一，且本次请求确实带了 `strict: prefer` 工具时才重建参数重试；已经开始的
//!   流绝不回退。
//! - **SSE 解码是自定义的**：空行分帧、`raw` 行拼进解析错误、`event: "error"` 直接作为错误
//!   文本、未知事件名被忽略；`AnthropicSseReader` 另外检查 `message_start`/`message_stop`
//!   是否配对。
//! - **字节级保真**：`build_params` 中字段的插入顺序是当前稳定契约（JSON 序列化顺序
//!   可观察）；`max_tokens: null`、空 `event:` 字段的保留、`Schemas contains ...` 的原文
//!   拼写等，都是既有协议行为，不要“顺手修正”，改动需同步测试。
//!
//! # 阅读顺序
//!
//! 先看 [`AnthropicOptions`]、[`AnthropicCompat`]、[`get_anthropic_compat`] 了解可配置面；
//! 再看 [`stream`]/[`stream_simple`] 与 `run_stream_inner` 的主干；最后按需读 `build_params`、
//! `convert_messages`、`convert_tools` 和 SSE 解码小节。

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::{
    UnsupportedStrictSchemaKeywordCheck, get_json_schema_tool_parameters,
    resolve_json_schema_strict_sampling,
};
use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::api::lazy::{lazy_api, lazy_stream_error};
use crate::api::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
};
use crate::api::transform_messages::{split_error_tool_result_images, transform_messages_with};
use crate::env_api_keys::{
    ANTHROPIC_FEDERATION_RULE_ID_ENV, ANTHROPIC_IDENTITY_TOKEN_FILE_ENV,
    ANTHROPIC_ORGANIZATION_ID_ENV, ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV,
};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::{HttpError, HttpRequest, HttpResponse, SharedHttpClient};
use crate::models::calculate_cost;
use crate::models::provider::ProviderStreams;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention,
    DoneReason, ErrorReason, ImageContent, JsonObject, Message, Model, ModelThinkingLevel,
    ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent,
    ThinkingLevel, Tool, ToolCall, ToolResultMessage, TranscriptContext, Usage, UsageCost,
    UserContent, UserContentBlock,
};
use crate::utils::diagnostics::{
    ThrownValue, append_assistant_message_diagnostic, create_assistant_message_diagnostic,
};
use crate::utils::headers::headers_to_record;
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::json_parse::parse_json_with_repair;
use crate::utils::openrouter_cost::{
    OpenRouterCostDetails, OpenRouterUsageAccounting, apply_open_router_reported_cost,
    is_open_router_model, is_valid_cost,
};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{
    HeaderLookup, ProviderError, ProviderRetryOptions, retry_provider_request,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{get_current_tools, get_initial_system_message, resolve_transcript};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// Stealth 模式的 Claude Code 版本（`user-agent: claude-cli/<version>`）。
const CLAUDE_CODE_VERSION: &str = "2.1.280";

/// Claude Code 2.x 工具名（规范大小写）。
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MID_CONVERSATION_OUTPUT_CONFIG_BETA: &str = "mid-conversation-output-config-2026-07-01";
const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";
const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";

/// SDK 的 `DEFAULT_TIMEOUT`（`client.mjs`：10 分钟）。TS 只在 `timeoutMs !== undefined` 时
/// 传 `timeout`，否则 SDK 用这个默认值；超时表现为 `APIConnectionTimeoutError`
/// （"Request timed out."）并留在 `retryProviderRequest` 的重试路径上。
const DEFAULT_ANTHROPIC_TIMEOUT_MS: u64 = 600_000;

/// OAuth 身份 system 文本（逐字）。
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's CLI for Claude.";

/// `AnthropicEffort`。
///
/// TS 的 `"low" | "medium" | "high" | "xhigh" | "max"`。Rust 用 [`String`] 承载，`effort`
/// 选项原样进入请求体；`is_anthropic_effort` 只在判断历史 assistant 的
/// `provider_thinking_level` 是否合法时使用。
pub type AnthropicEffort = String;
/// `AnthropicThinkingDisplay`。
///
/// TS 的 `"summarized" | "omitted"`；请求体缺省时 `build_params` 用 `"summarized"`。
pub type AnthropicThinkingDisplay = String;

const ANTHROPIC_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// SDK `requestOptions.timeout`：`timeoutMs !== undefined ? timeoutMs : DEFAULT_TIMEOUT`
/// （10 分钟），只用于等待响应头。
fn anthropic_request_timeout_ms(timeout_ms: Option<u64>) -> Option<u64> {
    Some(timeout_ms.unwrap_or(DEFAULT_ANTHROPIC_TIMEOUT_MS))
}

/// `AnthropicOptions`（TS 接口的 Rust 形态）。
///
/// 通用字段放在 [`StreamOptions`] 里；TS 接口上的 API 专属字段在此显式建模，其余未知选项
/// 仍留在 `base.extra` 中透传。
///
/// TS: `src/api/anthropic-messages.ts#AnthropicOptions`。
#[derive(Default)]
pub struct AnthropicOptions {
    /// 通用流选项：API key、headers、超时、重试、取消信号、回调与 `extra` 等。
    pub base: StreamOptions,
    /// `thinkingEnabled`。`None` 表示“未指定”，与显式 `Some(false)`（关闭思考）不同。
    pub thinking_enabled: Option<bool>,
    /// `thinkingBudgetTokens`：`thinking.type = "enabled"` 时的 `budget_tokens`；
    /// 值为 `0` 时请求体回落到 1024（复刻 TS 的 `|| 1024`）。
    pub thinking_budget_tokens: Option<u64>,
    /// `effort`：adaptive thinking 的 `output_config.effort` 与 mid-convo effort 标记使用。
    pub effort: Option<AnthropicEffort>,
    /// `thinkingDisplay`：请求体的 `display` 字段；缺省时用 `"summarized"`。
    pub thinking_display: Option<AnthropicThinkingDisplay>,
    /// `interleavedThinking`：`None` 视为默认开启，只有显式 `Some(false)` 才不发对应 beta。
    pub interleaved_thinking: Option<bool>,
    /// `"auto" | "any" | "none" | { type: "tool", name }`。
    ///
    /// 字符串值会被包成 `{ "type": <值> }`，对象值原样进入请求体。
    pub tool_choice: Option<Value>,
    /// 内部标记：TS 在 `maxTokens` 为 `undefined` 时经 `clampMaxTokensToContext` 得到
    /// `NaN`，JSON 序列化成 `max_tokens: null`。Rust 的 `Option<u64>` 表达不了 `NaN`，
    /// 用这个标记让请求体输出显式的 `null`。
    pub force_null_max_tokens: bool,
}

impl AnthropicOptions {
    /// 从冻结的 [`StreamOptions`] 解析 API 专属字段（`extra` 里的 TS 字段名）。
    ///
    /// `force_null_max_tokens` 是请求构建期的临时标记，不从 `extra` 恢复，始终为 `false`。
    pub fn from_stream_options(base: StreamOptions) -> Self {
        let extra = &base.extra;
        Self {
            thinking_enabled: extra.get("thinkingEnabled").and_then(Value::as_bool),
            thinking_budget_tokens: extra.get("thinkingBudgetTokens").and_then(Value::as_u64),
            effort: extra
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_string),
            thinking_display: extra
                .get("thinkingDisplay")
                .and_then(Value::as_str)
                .map(str::to_string),
            interleaved_thinking: extra.get("interleavedThinking").and_then(Value::as_bool),
            tool_choice: extra.get("toolChoice").cloned(),
            force_null_max_tokens: false,
            base,
        }
    }

    /// 写回冻结形态（ProviderStreams 的入口用）。
    ///
    /// 只写回六个 API 专属字段；`force_null_max_tokens` 是请求构建期的临时标记，不写入
    /// `extra`。`extra` 里已有的同名键会被覆盖，其余键原样保留。
    pub fn into_stream_options(mut self) -> StreamOptions {
        if let Some(value) = self.thinking_enabled {
            self.base
                .extra
                .insert("thinkingEnabled".into(), Value::Bool(value));
        }
        if let Some(value) = self.thinking_budget_tokens {
            self.base
                .extra
                .insert("thinkingBudgetTokens".into(), json!(value));
        }
        if let Some(value) = self.effort {
            self.base
                .extra
                .insert("effort".into(), Value::String(value));
        }
        if let Some(value) = self.thinking_display {
            self.base
                .extra
                .insert("thinkingDisplay".into(), Value::String(value));
        }
        if let Some(value) = self.interleaved_thinking {
            self.base
                .extra
                .insert("interleavedThinking".into(), Value::Bool(value));
        }
        if let Some(value) = self.tool_choice {
            self.base.extra.insert("toolChoice".into(), value);
        }
        self.base
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn obj<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(key))
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    obj(value, key).and_then(Value::as_str)
}

/// `value == null` 或缺失 → `None`。
fn non_null(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    obj(value, key).and_then(Value::as_u64)
}

/// JS 的 `value || 0`（这里只用于 token 计数：非数值/缺失 → 0）。
fn token_count(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite() && *number > 0.0)
        .map_or(0, |number| number as u64)
}

/// JS 的字符串真值：`!!value`。
fn js_truthy_str(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn js_trim_empty(value: &str) -> bool {
    kiana_json::js_string::js_trim(value).is_empty()
}

fn push(map: &mut Map<String, Value>, key: &str, value: Value) {
    map.insert(key.to_string(), value);
}

fn to_string_map(headers: &ProviderHeaders) -> Vec<(String, Option<String>)> {
    headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Claude Code 工具名
// ---------------------------------------------------------------------------

fn claude_code_name(name: &str) -> String {
    let lower = name.to_lowercase();
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|known| known.to_lowercase() == lower)
        .map_or_else(|| name.to_string(), |known| (*known).to_string())
}

fn from_claude_code_name(name: &str, tools: &[Tool]) -> String {
    let lower = name.to_lowercase();
    if let Some(tool) = tools.iter().find(|tool| tool.name.to_lowercase() == lower) {
        return tool.name.clone();
    }
    name.to_string()
}

// ---------------------------------------------------------------------------
// compat
// ---------------------------------------------------------------------------

/// `getAnthropicCompat(model)`。
///
/// 把 `model.compat` 里稀疏的 JSON 开关解析成完整能力表。缺省值见 [`get_anthropic_compat`]；
/// 这些开关只影响请求体/请求头，不改变流事件的形状。
#[derive(Debug, Clone)]
pub struct AnthropicCompat {
    /// 支持工具定义的 `eager_input_streaming`；为 `false` 时改用 fine-grained-tool-streaming beta。
    pub supports_eager_tool_input_streaming: bool,
    /// 支持长缓存保留（`cache_control.ttl = "1h"`）；为 `false` 时长保留退化为普通 ephemeral。
    pub supports_long_cache_retention: bool,
    /// 是否发送会话亲和头（OpenRouter 默认开启）。
    pub send_session_affinity_headers: bool,
    /// 会话亲和头的格式：`"openrouter"` → `x-session-id`，其他/缺省 → `x-session-affinity`。
    pub session_affinity_format: Option<String>,
    /// 是否允许把 `cache_control` 挂在工具定义上（只挂最后一个工具）。
    pub supports_cache_control_on_tools: bool,
    /// 是否允许发送 `temperature`（思考开启或 mid-convo effort 时本来就不发）。
    pub supports_temperature: bool,
    /// 非空的历史 thinking 块缺 signature 时是否保留为 `thinking`；为 `false` 时降级成普通
    /// `text`（thinking 文本与 signature 都为空则整块跳过）。
    pub allow_empty_signature: bool,
    /// 是否支持 strict JSON schema 工具（请求体里的 `strict: true`）。
    pub supports_strict_tools: bool,
    /// 是否支持会话中途插入 system 消息（原生 tool_addition/tool_removal 的前提）。
    pub supports_mid_convo_system_messages: bool,
    /// 是否支持会话中途增删工具（还需要 `supports_mid_convo_system_messages` 同时为真）。
    pub supports_mid_convo_tool_changes: bool,
}

fn compat_value(model: &Model, key: &str) -> Option<Value> {
    model
        .compat
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|compat| compat.get(key))
        .cloned()
}

fn compat_bool(model: &Model, key: &str) -> Option<bool> {
    compat_value(model, key).and_then(|value| value.as_bool())
}

/// `getAnthropicCompat(model)`。
///
/// 除 `allow_empty_signature`、`supports_strict_tools` 与两个 mid-convo 开关默认 `false` 外，
/// 其余布尔开关默认 `true`；`send_session_affinity_headers` 默认取 `is_open_router`，
/// `session_affinity_format` 仅在 OpenRouter 下默认 `"openrouter"`（其余为 `None`）。
/// OpenRouter 通过 `provider == "openrouter"` 或 `base_url` 含 `openrouter.ai` 识别。
pub fn get_anthropic_compat(model: &Model) -> AnthropicCompat {
    let is_open_router = model.provider == "openrouter" || model.base_url.contains("openrouter.ai");
    AnthropicCompat {
        supports_eager_tool_input_streaming: compat_bool(model, "supportsEagerToolInputStreaming")
            .unwrap_or(true),
        supports_long_cache_retention: compat_bool(model, "supportsLongCacheRetention")
            .unwrap_or(true),
        send_session_affinity_headers: compat_bool(model, "sendSessionAffinityHeaders")
            .unwrap_or(is_open_router),
        session_affinity_format: compat_value(model, "sessionAffinityFormat")
            .and_then(|value| value.as_str().map(str::to_string))
            .or_else(|| is_open_router.then(|| "openrouter".to_string())),
        supports_cache_control_on_tools: compat_bool(model, "supportsCacheControlOnTools")
            .unwrap_or(true),
        supports_temperature: compat_bool(model, "supportsTemperature").unwrap_or(true),
        allow_empty_signature: compat_bool(model, "allowEmptySignature").unwrap_or(false),
        supports_strict_tools: compat_bool(model, "supportsStrictTools").unwrap_or(false),
        supports_mid_convo_system_messages: compat_bool(model, "supportsMidConvoSystemMessages")
            .unwrap_or(false),
        supports_mid_convo_tool_changes: compat_bool(model, "supportsMidConvoToolChanges")
            .unwrap_or(false),
    }
}

fn force_adaptive_thinking(model: &Model) -> bool {
    compat_bool(model, "forceAdaptiveThinking") == Some(true)
}

fn supports_mid_convo_effort(model: &Model) -> bool {
    compat_bool(model, "supportsMidConvoEffort") == Some(true)
}

/// `model.compat.allowedFallbackModels`。
fn allowed_fallback_models(model: &Model) -> Vec<Value> {
    compat_value(model, "allowedFallbackModels")
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn should_use_server_side_fallback_beta(model: &Model) -> bool {
    !allowed_fallback_models(model).is_empty()
}

// ---------------------------------------------------------------------------
// cache retention
// ---------------------------------------------------------------------------

/// `resolveCacheRetention(cacheRetention?, env?)`。
///
/// 优先级：显式 `cache_retention` > 环境变量 `KIANA_CACHE_RETENTION == "long"` > `Short`。
/// 返回的 [`CacheRetention::None`] 表示显式关闭缓存标记，与“未指定（落到 Short）”不同。
pub fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&IndexMap<String, String>>,
) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if get_provider_env_value("KIANA_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

fn cache_control_value(ttl: Option<&str>) -> Value {
    let mut map = Map::new();
    map.insert("type".into(), Value::String("ephemeral".into()));
    if let Some(ttl) = ttl {
        map.insert("ttl".into(), Value::String(ttl.to_string()));
    }
    Value::Object(map)
}

/// `getCacheControl(model, cacheRetention?, env?)`。
///
/// 返回解析后的 retention 与要挂到消息/工具上的 `cache_control` 对象。`CacheRetention::None`
/// 时返回 `(CacheRetention::None, None)`；`Long` 只有在 `supportsLongCacheRetention` 为真时
/// 才带 `ttl: "1h"`，否则退化为普通 ephemeral（键序固定为 `type` 然后可选 `ttl`）。
fn get_cache_control(options: &AnthropicOptions, model: &Model) -> (CacheRetention, Option<Value>) {
    let retention =
        resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    if retention == CacheRetention::None {
        return (retention, None);
    }
    let ttl = if retention == CacheRetention::Long
        && get_anthropic_compat(model).supports_long_cache_retention
    {
        Some("1h")
    } else {
        None
    };
    (retention, Some(cache_control_value(ttl)))
}

// ---------------------------------------------------------------------------
// 鉴权与 federation
// ---------------------------------------------------------------------------

/// `hasHeader(headers, name)`：大小写不敏感；`null` / 空白值不算。
fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case(name)
            && value
                .as_ref()
                .is_some_and(|value| !kiana_json::js_string::js_trim(value).is_empty())
    })
}

/// `hasRequestAuth(apiKey, headers)`。
///
/// 判定是否存在可用的请求鉴权，两部分口径不同：`api_key` 只按非空判定（JS 真值，全空白
/// 字符串也算有值）；`authorization` / `x-api-key` / `cf-aig-authorization` 的头值 `trim`
/// 后为空才算没有鉴权（头名大小写不敏感）。`None` 与空串没有鉴权，此时 federation 才有
/// 机会接管。
pub fn has_request_auth(api_key: Option<&str>, headers: Option<&ProviderHeaders>) -> bool {
    js_truthy_str(api_key)
        || has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
}

// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定（`Display` == TS 的
// `error.message`，字段不可装箱），签名保持不变。
#[allow(clippy::result_large_err)]
fn assert_request_auth(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
) -> Result<(), AiError> {
    if has_request_auth(api_key, headers) {
        Ok(())
    } else {
        Err(AiError::new(
            AiErrorKind::Auth,
            format!("No API key for provider: {provider}"),
        ))
    }
}

/// `AnthropicFederationConfig`（`ClientOptions["config"]` 的 Rust 形态）。
///
/// 由 [`get_anthropic_federation`] 从环境变量组装；仅当 provider 为 `anthropic` 且没有其他
/// 鉴权时生效。发请求前用它获取（或复用模块级缓存的）Bearer token，见
/// `federation_access_token`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicFederationConfig {
    /// 必填，来自 `ANTHROPIC_ORGANIZATION_ID`。
    pub organization_id: String,
    /// 可选，来自 `ANTHROPIC_WORKSPACE_ID`。
    pub workspace_id: Option<String>,
    /// 必填，来自 `ANTHROPIC_FEDERATION_RULE_ID`。
    pub federation_rule_id: String,
    /// 可选，来自 `ANTHROPIC_SERVICE_ACCOUNT_ID`。
    pub service_account_id: Option<String>,
    /// 必填，identity token（JWT）文件的路径，来自 `ANTHROPIC_IDENTITY_TOKEN_FILE`。
    pub identity_token_path: String,
}

impl AnthropicFederationConfig {
    /// `JSON.stringify(config)` 的等价物（键序与 SDK 的 config 对象一致）。
    ///
    /// 该值直接参与 federation token 的缓存 key（`[baseUrl, config]`），所以键序与可选字段的
    /// 有无都是可观察的：改动结构会让旧缓存失效并触发一次新的 token 交换。
    pub fn to_json(&self) -> Value {
        let mut authentication = Map::new();
        authentication.insert("type".into(), Value::String("oidc_federation".into()));
        authentication.insert(
            "federation_rule_id".into(),
            Value::String(self.federation_rule_id.clone()),
        );
        if let Some(service_account_id) = &self.service_account_id {
            authentication.insert(
                "service_account_id".into(),
                Value::String(service_account_id.clone()),
            );
        }
        authentication.insert(
            "identity_token".into(),
            json!({ "source": "file", "path": self.identity_token_path }),
        );
        let mut config = Map::new();
        config.insert(
            "organization_id".into(),
            Value::String(self.organization_id.clone()),
        );
        if let Some(workspace_id) = &self.workspace_id {
            config.insert("workspace_id".into(), Value::String(workspace_id.clone()));
        }
        config.insert("authentication".into(), Value::Object(authentication));
        Value::Object(config)
    }
}

/// `getAnthropicFederation(model, apiKey, headers, env)`。
///
/// 返回 `Some` 需要同时满足：`model.provider == "anthropic"`、当前没有其他鉴权
/// （[`has_request_auth`] 为假）、且 federation rule / organization / identity token 三个
/// 必填环境变量都非空（JS 真值；仅空白字符串也算有值，不会被 trim 掉）。任一不满足返回
/// `None`，调用方据此回退到普通 API key 校验。
pub fn get_anthropic_federation(
    model: &Model,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
    env: Option<&IndexMap<String, String>>,
) -> Option<AnthropicFederationConfig> {
    if model.provider != "anthropic" || has_request_auth(api_key, headers) {
        return None;
    }
    let federation_rule_id = get_provider_env_value(ANTHROPIC_FEDERATION_RULE_ID_ENV, env);
    let organization_id = get_provider_env_value(ANTHROPIC_ORGANIZATION_ID_ENV, env);
    let identity_token_file = get_provider_env_value(ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, env);
    if !js_truthy_str(federation_rule_id.as_deref())
        || !js_truthy_str(organization_id.as_deref())
        || !js_truthy_str(identity_token_file.as_deref())
    {
        return None;
    }
    Some(AnthropicFederationConfig {
        organization_id: organization_id.expect("checked above"),
        workspace_id: get_provider_env_value(ANTHROPIC_WORKSPACE_ID_ENV, env),
        federation_rule_id: federation_rule_id.expect("checked above"),
        service_account_id: get_provider_env_value(ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, env),
        identity_token_path: identity_token_file.expect("checked above"),
    })
}

#[allow(clippy::result_large_err)]
fn default_http_client() -> Result<SharedHttpClient, AiError> {
    crate::http::reqwest_client::ReqwestHttpClient::new()
        .map(|client| client.shared())
        .map_err(AiError::from)
}

/// SDK `TokenCache` 的进程级单例。TS 的 `federationClient` 只有**一个**槽位：key
/// （`JSON.stringify([baseUrl, federation])`）变化时旧 client 连同 token cache 被丢弃，
/// 因此这里也用单槽 `Option`，而不是按 key 的 map（交替 config 会重新交换 token）。
///
/// `Mutex` 只在读取/写回的同步片段内持有，不跨 `.await`；并发首次请求可能各自交换一次
/// token，以最后写入者为准（单槽语义）。锁中毒时通过 `into_inner` 取回内部值继续使用。
struct FederationTokenCache {
    key: String,
    token: String,
    expires_at: f64,
}

fn federation_token_cache() -> &'static Mutex<Option<FederationTokenCache>> {
    static CACHE: OnceLock<Mutex<Option<FederationTokenCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// SDK `ADVISORY_REFRESH_THRESHOLD_IN_SECONDS`。
const ADVISORY_REFRESH_THRESHOLD_IN_SECONDS: f64 = 120.0;
/// SDK `MANDATORY_REFRESH_THRESHOLD_IN_SECONDS`。
const MANDATORY_REFRESH_THRESHOLD_IN_SECONDS: f64 = 30.0;
/// SDK `MAX_TOKEN_RESPONSE_BYTES`。
const MAX_TOKEN_RESPONSE_BYTES: usize = 1 << 20;
/// SDK `redactSensitive` 的截断阈值。
const MAX_ERROR_BODY_CHARS: usize = 2000;
/// SDK `OAUTH_API_BETA_HEADER`。
const OAUTH_API_BETA_HEADER: &str = "oauth-2025-04-20";

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

/// 从文件读 identity token（`File` source）。
#[allow(clippy::result_large_err)]
fn read_identity_token(path: &str) -> Result<String, AiError> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        // TS: `Failed to read identity token file at ${path}: ${err}`，`err` 是 Node 的
        // `Error`（`${err}` → "Error: ENOENT: no such file or directory, open '<path>'"）。
        let detail = if error.kind() == std::io::ErrorKind::NotFound {
            format!("Error: ENOENT: no such file or directory, open '{path}'")
        } else {
            format!("Error: {error}")
        };
        AiError::new(
            AiErrorKind::Auth,
            format!("Failed to read identity token file at {path}: {detail}"),
        )
    })?;
    let token = kiana_json::js_string::js_trim(&content);
    if token.is_empty() {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!("Identity token file at {path} is empty"),
        ));
    }
    Ok(token.to_string())
}

/// `requireSecureTokenEndpoint(baseURL)`：拒绝明文 HTTP（loopback 除外）。
#[allow(clippy::result_large_err)]
fn require_secure_token_endpoint(base_url: &str) -> Result<(), AiError> {
    let Ok(parsed) = url::Url::parse(base_url) else {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!("Invalid token endpoint base URL \"{base_url}\": TypeError: Invalid URL"),
        ));
    };
    if parsed.scheme() == "https" {
        return Ok(());
    }
    let host = parsed.host_str().unwrap_or_default().to_lowercase();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if parsed.scheme() == "http" && (host == "localhost" || host == "127.0.0.1" || host == "::1") {
        return Ok(());
    }
    Err(AiError::new(
        AiErrorKind::Auth,
        format!("Refusing to send credential over non-https token endpoint \"{base_url}\""),
    ))
}

/// `redactSensitive(body)` 的对象分支：只保留 RFC 6749 §5.2 的标准字段。
fn redact_sensitive_value(value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::String(text) => Value::String(redact_sensitive_text(text)),
        Value::Object(object) => {
            // TS 按 `Object.entries(body)` 的顺序遍历，只保留 RFC 6749 §5.2 的标准字段，
            // 因此输出键序跟随原对象，而不是固定字段表的顺序。
            let mut out = Map::new();
            for (key, value) in object {
                if matches!(key.as_str(), "error" | "error_description" | "error_uri") {
                    out.insert(key.clone(), value.clone());
                }
            }
            Value::Object(out)
        }
        // `typeof body === 'object' && !Array.isArray(body)` 之外的（数组、数字、布尔）→ null。
        _ => Value::Null,
    }
}

/// `redactSensitive(body)`：字符串先尝试 `JSON.parse`，失败则按 UTF-16 长度截断。
fn redact_sensitive_text(text: &str) -> String {
    if let Ok(parsed) = kiana_json::js_parse::parse(text) {
        return kiana_json::js_object::stringify(&redact_sensitive_value(&parsed));
    }
    let length = text.encode_utf16().count();
    if length <= MAX_ERROR_BODY_CHARS {
        return text.to_string();
    }
    let head: Vec<u16> = text.encode_utf16().take(MAX_ERROR_BODY_CHARS).collect();
    format!(
        "{}... <{} more chars>",
        String::from_utf16_lossy(&head),
        length - MAX_ERROR_BODY_CHARS
    )
}

/// JS `Number(value)`（`Number.isFinite` 判定用）。
fn js_number_of(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(value)) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(text)) => kiana_json::js_num::parse_js_number(text),
        Some(Value::Array(items)) => match items.len() {
            0 => 0.0,
            1 => js_number_of(Some(&items[0])),
            _ => f64::NAN,
        },
        Some(Value::Object(_)) => f64::NAN,
    }
}

/// SDK `readLimitedText(resp)`：最多读 1 MiB。
async fn read_limited_token_text(mut body: crate::http::ByteStream) -> Result<String, HttpError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > MAX_TOKEN_RESPONSE_BYTES {
            let remaining = MAX_TOKEN_RESPONSE_BYTES - bytes.len();
            bytes.extend_from_slice(&chunk[..remaining]);
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// 一次 JWT-bearer 交换（`oidcFederationProvider`），返回 `(token, expiresAt 秒)`。
///
/// 请求体字段与 SDK 一致：`grant_type`、`assertion`、`federation_rule_id`、
/// `organization_id`，以及可选的 `service_account_id`/`workspace_id`。identity token 从文件
/// 读取，空白视为错误，长度上限 16 KiB（按 UTF-16 计数）；终点的 URL 必须是 https（loopback
/// 的 http 除外），否则拒绝发送凭据。非 2xx 的错误体先经 `redactSensitive`：JSON 对象只保留
/// RFC 6749 §5.2 的三个标准字段，超长文本按 2000 个 UTF-16 单元截断。401 额外附带排查提示。
#[allow(clippy::result_large_err)]
async fn exchange_federation_token(
    effective_base_url: &str,
    config: &AnthropicFederationConfig,
    fetch: &SharedHttpClient,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(String, f64), AiError> {
    let jwt = read_identity_token(&config.identity_token_path)?;
    let jwt_len = jwt.encode_utf16().count();
    if jwt_len > 16 * 1024 {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!(
                "Identity token is {} KiB, exceeds the 16 KiB assertion limit",
                (jwt_len as f64 / 1024.0).ceil()
            ),
        ));
    }
    let mut body = Map::new();
    body.insert(
        "grant_type".into(),
        Value::String("urn:ietf:params:oauth:grant-type:jwt-bearer".into()),
    );
    body.insert("assertion".into(), Value::String(jwt));
    body.insert(
        "federation_rule_id".into(),
        Value::String(config.federation_rule_id.clone()),
    );
    body.insert(
        "organization_id".into(),
        Value::String(config.organization_id.clone()),
    );
    if let Some(service_account_id) = &config.service_account_id {
        body.insert(
            "service_account_id".into(),
            Value::String(service_account_id.clone()),
        );
    }
    if let Some(workspace_id) = &config.workspace_id {
        body.insert("workspace_id".into(), Value::String(workspace_id.clone()));
    }
    // `effectiveBaseURL = (config.base_url || options.baseURL).replace(/\/+$/, '')`。
    let url = format!("{effective_base_url}/v1/oauth/token");
    let request = HttpRequest {
        method: "POST".into(),
        url: url.clone(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            (
                "anthropic-beta".into(),
                format!("{OAUTH_API_BETA_HEADER},oidc-federation-2026-04-01"),
            ),
            // SDK 的 token 请求带 client 的 User-Agent（`config.userAgent || client UA`）。
            ("User-Agent".into(), "Anthropic/JS 0.129.0".into()),
        ],
        body: Some(kiana_json::js_object::stringify(&Value::Object(body)).into_bytes()),
        timeout_ms: None,
    };
    let response = fetch.execute(request, signal).await.map_err(|error| {
        AiError::new(
            AiErrorKind::Auth,
            format!(
                "Failed to reach token endpoint {url}: TypeError: {}",
                error.message
            ),
        )
    })?;
    let status = response.response.status;
    let request_id = response
        .response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("request-id"))
        .map(|(_, value)| value.clone());
    let request_id_suffix = request_id
        .map(|request_id| format!(" (request-id {request_id})"))
        .unwrap_or_default();
    if !(200..300).contains(&status) {
        let text = read_limited_token_text(response.body)
            .await
            .unwrap_or_default();
        let redacted = redact_sensitive_text(&text);
        // 401 时附带排查提示（RFC 6749 的其他错误不带）。
        let hint = if status == 401 {
            let hint_middle = if config.workspace_id.is_some() {
                ""
            } else {
                "If your federation rule is scoped to multiple workspaces, set the ANTHROPIC_WORKSPACE_ID environment variable, the 'workspace_id' config key, or the `workspaceId` option. "
            };
            format!(
                " Ensure your federation rule matches your identity token. {hint_middle}View your authentication events in the Workload identity page of Claude Console for more details."
            )
        } else {
            String::new()
        };
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!(
                "Token exchange failed with status {status}{request_id_suffix}: {redacted}{hint}"
            ),
        ));
    }

    // `parseTokenResponse`。
    let text = read_limited_token_text(response.body)
        .await
        .map_err(|error| AiError::new(AiErrorKind::Auth, error.message))?;
    let Ok(parsed) = kiana_json::js_parse::parse(&text) else {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!("Token endpoint returned non-JSON response (status {status})"),
        ));
    };
    if parsed.is_null() {
        // `data.access_token` 对 null 抛 V8 TypeError（未包装，直接冒泡）。
        return Err(AiError::new(
            AiErrorKind::Auth,
            "Cannot read properties of null (reading 'access_token')",
        ));
    }
    let access_token_value = obj(&parsed, "access_token");
    if !kiana_json::js_string::is_truthy(access_token_value) {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!(
                "Token endpoint response missing access_token: {}",
                kiana_json::js_object::stringify(&redact_sensitive_value(&parsed))
            ),
        ));
    }
    if let Some(token_type) = obj(&parsed, "token_type")
        && kiana_json::js_string::is_truthy(Some(token_type))
    {
        let lowered = token_type
            .as_str()
            .map(str::to_lowercase)
            .unwrap_or_default();
        if lowered != "bearer" {
            return Err(AiError::new(
                AiErrorKind::Auth,
                format!(
                    "Token endpoint response: unsupported token_type \"{}\" (want Bearer)",
                    kiana_json::js_string::js_string(Some(token_type))
                ),
            ));
        }
    }
    let expires_in = js_number_of(obj(&parsed, "expires_in"));
    if !expires_in.is_finite() {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!(
                "Token endpoint response missing required fields: {}",
                kiana_json::js_object::stringify(&redact_sensitive_value(&parsed))
            ),
        ));
    }
    let access_token = match access_token_value {
        Some(Value::String(token)) => token.clone(),
        Some(other) => kiana_json::js_string::js_string(Some(other)),
        None => String::new(),
    };
    Ok((access_token, now_seconds() + expires_in))
}

/// advisory 窗口的后台刷新：失败时静默保留旧 token。
///
/// 写回缓存前会再核对 key：只有槽位为空或仍是同一 key 时才覆盖；槽位已被其他配置占用时丢弃
/// 这次结果，避免把旧配置的 token 写进新槽位。
fn spawn_federation_background_refresh(
    effective_base_url: String,
    config: AnthropicFederationConfig,
    fetch: SharedHttpClient,
    cache_key: String,
) {
    crate::api::lazy::spawn_forward(async move {
        let Ok((token, expires_at)) =
            exchange_federation_token(&effective_base_url, &config, &fetch, None).await
        else {
            // advisory 刷新失败：继续使用旧 token（SDK 只通过 hook 记录）。
            return;
        };
        let mut cache = federation_token_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if cache.as_ref().is_none_or(|entry| entry.key == cache_key) {
            *cache = Some(FederationTokenCache {
                key: cache_key,
                token,
                expires_at,
            });
        }
    });
}

/// `oidcFederationProvider` + SDK `TokenCache`：JWT-bearer grant（`POST /v1/oauth/token`）。
///
/// 缓存策略与 SDK 一致：剩余 > 120s 直接复用；30–120s 返回旧 token 并后台刷新；
/// ≤ 30s（或已过期/无缓存）阻塞刷新。TS 的 `federationClient` 是单槽，key 变化会丢弃旧缓存。
#[allow(clippy::result_large_err)]
async fn federation_access_token(
    base_url: &str,
    config: &AnthropicFederationConfig,
    fetch: &SharedHttpClient,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<String, AiError> {
    let effective_base_url = base_url.trim_end_matches('/');
    require_secure_token_endpoint(effective_base_url)?;
    let cache_key = format!(
        "[{}]",
        [
            kiana_json::js_object::stringify(&Value::String(base_url.to_string())),
            kiana_json::js_object::stringify(&config.to_json()),
        ]
        .join(",")
    );
    let cached = federation_token_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .filter(|entry| entry.key == cache_key)
        .map(|entry| (entry.token.clone(), entry.expires_at));
    if let Some((token, expires_at)) = cached {
        let remaining = expires_at - now_seconds();
        if remaining > ADVISORY_REFRESH_THRESHOLD_IN_SECONDS {
            return Ok(token);
        }
        if remaining > MANDATORY_REFRESH_THRESHOLD_IN_SECONDS {
            // advisory 窗口：先返回旧 token，后台刷新（下一次请求再用新值）。
            spawn_federation_background_refresh(
                effective_base_url.to_string(),
                config.clone(),
                fetch.clone(),
                cache_key,
            );
            return Ok(token);
        }
    }
    let (token, expires_at) =
        exchange_federation_token(effective_base_url, config, fetch, signal).await?;
    *federation_token_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(FederationTokenCache {
        key: cache_key,
        token: token.clone(),
        expires_at,
    });
    Ok(token)
}

// ---------------------------------------------------------------------------
// HTTP 请求
// ---------------------------------------------------------------------------

async fn collect_body(mut body: crate::http::ByteStream) -> Result<Vec<u8>, HttpError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(bytes)
}

/// SDK `APIError` 在 Rust 侧的可观察形态。
///
/// 与 [`AiError`] 的区别：这里保留 HTTP `status` 与响应体里的 `error.type`，供 strict 工具
/// 降级判定（要求 400 + `invalid_request_error`）和重试策略（读取 `retry-after`）使用。
#[derive(Clone)]
pub struct AnthropicApiError {
    /// HTTP 状态码；`0` 表示没有拿到响应（连接失败、超时或被取消）。
    pub status: u16,
    /// 响应体 JSON 的 `error.type`；body 不是 JSON 或缺少该字段时为 `None`。
    pub error_type: Option<String>,
    /// 按 SDK `APIError.makeMessage` 公式拼出的可读消息。
    pub message: String,
    /// 响应头取值回调（重试策略用）；传输层错误时为 `None`。
    pub header: HeaderLookup,
}

impl std::fmt::Debug for AnthropicApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicApiError")
            .field("status", &self.status)
            .field("error_type", &self.error_type)
            .field("message", &self.message)
            .finish()
    }
}

fn header_lookup(headers: &IndexMap<String, String>) -> HeaderLookup {
    let headers = headers.clone();
    Some(Arc::new(move |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }))
}

/// 用 SDK 的 `APIError.makeMessage` 公式构造错误消息。
fn build_api_error(
    status: u16,
    body_text: &str,
    headers: &IndexMap<String, String>,
) -> AnthropicApiError {
    let parsed: Option<Value> = serde_json::from_str(body_text).ok();
    let error_type = parsed
        .as_ref()
        .and_then(|parsed| obj(parsed, "error"))
        .and_then(|error| str_of(error, "type"))
        .map(str::to_string);
    // SDK `APIError.makeMessage(status, error, message)`：
    //   msg = error?.message ? (string ? error.message : JSON.stringify(error.message))
    //                        : error ? JSON.stringify(error) : message
    // 其中 `error` 是解析后的 body（仅当它是**真值**），`message` 是原始文本
    // （body 不是真值 JSON 时才有值，空 body 时为空串）。
    let message = match parsed.as_ref() {
        Some(parsed) if kiana_json::js_string::is_truthy(Some(parsed)) => {
            let top_level_message = obj(parsed, "message");
            let msg = match top_level_message {
                Some(value) if kiana_json::js_string::is_truthy(Some(value)) => match value {
                    Value::String(message) => message.clone(),
                    value => kiana_json::js_object::stringify(value),
                },
                _ => kiana_json::js_object::stringify(parsed),
            };
            format!("{status} {msg}")
        }
        _ if body_text.is_empty() => format!("{status} status code (no body)"),
        _ => format!("{status} {body_text}"),
    };
    AnthropicApiError {
        status,
        error_type,
        message,
        header: header_lookup(headers),
    }
}

/// 客户端：base URL + 固定头 + 鉴权方式 + fetch。
///
/// 三种鉴权形态互斥：`api_key`（`x-api-key`）、`auth_token`（`authorization: Bearer`，
/// OAuth 与 Copilot 使用）、`federation`（发请求前交换 token）。`default_headers` 中的
/// `None` 表示“删除同名头”，这是 SDK `buildHeaders` 的语义。
struct AnthropicClient {
    base_url: String,
    /// `defaultHeaders`：`null` 值表示“删除该头”（SDK `buildHeaders` 的语义）。
    default_headers: Vec<(String, Option<String>)>,
    api_key: Option<String>,
    auth_token: Option<String>,
    federation: Option<AnthropicFederationConfig>,
    fetch: SharedHttpClient,
}

impl AnthropicClient {
    fn build_url(&self) -> String {
        let path = "/v1/messages?beta=true";
        if self.base_url.ends_with('/') {
            format!("{}{}", self.base_url, &path[1..])
        } else {
            format!("{}{}", self.base_url, path)
        }
    }

    fn headers(&self, betas: Option<&[String]>, token_auth: Option<&str>) -> Vec<(String, String)> {
        // SDK `buildHeaders` 的合并顺序：base 默认 → 鉴权 → defaultHeaders（identity/model/
        // caller）→ bodyHeaders（content-type/anthropic-beta）。因此调用方显式给出的
        // `x-api-key` / `authorization` 覆盖 SDK 的鉴权头；defaultHeaders 里的 `null` 则
        // **删除**已存在的同名头（包括 SDK 自己加上的鉴权头，见 SDK
        // `internal/headers.mjs#buildHeaders`）。
        let mut headers: Vec<(String, String)> = Vec::new();
        let set = |headers: &mut Vec<(String, String)>, name: &str, value: String| {
            if let Some(entry) = headers
                .iter_mut()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
            {
                entry.1 = value;
            } else {
                headers.push((name.to_string(), value));
            }
        };
        let delete = |headers: &mut Vec<(String, String)>, name: &str| {
            headers.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        };
        set(&mut headers, "accept", "application/json".into());
        set(
            &mut headers,
            "anthropic-dangerous-direct-browser-access",
            "true".into(),
        );
        set(&mut headers, "anthropic-version", "2023-06-01".into());
        if let Some(api_key) = &self.api_key {
            set(&mut headers, "x-api-key", api_key.clone());
        }
        if let Some(auth_token) = &self.auth_token {
            set(
                &mut headers,
                "authorization",
                format!("Bearer {auth_token}"),
            );
        }
        // SDK 的 `authHeaders()`：token cache（federation）鉴权注入 Bearer token。
        if let Some(token) = token_auth {
            set(&mut headers, "authorization", format!("Bearer {token}"));
        }
        for (name, value) in &self.default_headers {
            match value {
                Some(value) => set(&mut headers, name, value.clone()),
                None => delete(&mut headers, name),
            }
        }
        set(&mut headers, "content-type", "application/json".into());
        if let Some(betas) = betas.filter(|betas| !betas.is_empty()) {
            set(&mut headers, "anthropic-beta", betas.join(","));
        }
        // SDK `prepareRequest`：token cache 且 `apiKey == null` 时，在**所有**头合并之后把
        // `oauth-2025-04-20` 追加到 `anthropic-beta`（已在列表里则保持不变）。
        if token_auth.is_some() {
            const OAUTH_API_BETA_HEADER: &str = "oauth-2025-04-20";
            let existing: Option<Vec<String>> = headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("anthropic-beta"))
                .map(|(_, value)| {
                    value
                        .split(',')
                        .map(|feature| kiana_json::js_string::js_trim(feature).to_string())
                        .collect()
                });
            let mut features = existing.unwrap_or_default();
            if !features
                .iter()
                .any(|feature| feature == OAUTH_API_BETA_HEADER)
            {
                features.push(OAUTH_API_BETA_HEADER.to_string());
                set(&mut headers, "anthropic-beta", features.join(","));
            }
        }
        headers
    }

    /// SDK `validateHeaders`：没有任何可用的鉴权头时抛错（federation 的 token cache 除外）。
    #[allow(clippy::result_large_err)]
    fn validate_headers(&self, headers: &[(String, String)]) -> Result<(), AiError> {
        let value_of = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
                .unwrap_or_default()
        };
        if !value_of("x-api-key").is_empty() || !value_of("authorization").is_empty() {
            return Ok(());
        }
        if self.federation.is_some() {
            return Ok(());
        }
        let nulls: Vec<String> = self
            .default_headers
            .iter()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_lowercase())
            .collect();
        if nulls.iter().any(|name| name == "x-api-key") {
            return Ok(());
        }
        if nulls.iter().any(|name| name == "authorization") {
            return Ok(());
        }
        Err(AiError::new(
            AiErrorKind::Other,
            "Could not resolve authentication method. Expected one of apiKey, authToken, credentials, config, or profile to be set. Or for one of the \"X-Api-Key\" or \"Authorization\" headers to be explicitly omitted",
        ))
    }
}

/// `model.headers`（`Record<string, string>`）→ `ProviderHeaders`。
fn model_provider_headers(headers: Option<&IndexMap<String, String>>) -> Option<ProviderHeaders> {
    headers.map(|headers| {
        headers
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect()
    })
}

/// 合并多个头来源（SDK `buildHeaders` 的等价物）；`null` 值表示“抑制该头”。
fn merge_headers(sources: &[Option<&ProviderHeaders>]) -> IndexMap<String, Option<String>> {
    // SDK 的 `buildHeaders` 按小写名合并：后一个来源的任何大小写同名头都覆盖前一个，
    // 并移到末尾（与 WHATWG `Headers` 一致）。`null` 保留为“抑制”，在发请求时删除。
    let mut merged: IndexMap<String, Option<String>> = IndexMap::new();
    for source in sources.iter().flatten() {
        for (key, value) in source.iter() {
            let lower = key.to_lowercase();
            let existing = merged
                .keys()
                .find(|existing| existing.to_lowercase() == lower)
                .cloned();
            if let Some(existing) = existing {
                merged.shift_remove(&existing);
            }
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

/// `createClient(...)`：构造请求所需的鉴权与默认头。
///
/// 第二个返回值是 `is_oauth_token`：`api_key` 含 `sk-ant-oat` 时为 `true`，此后走 Bearer
/// 鉴权、Claude Code 身份头与 system 文本，并在请求/解析工具名时做 Claude Code 名称映射。
/// provider 为 `github-copilot` 时走独立分支（Bearer + 动态头，不做 OAuth 身份处理）。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)]
fn create_client(
    model: &Model,
    api_key: Option<&str>,
    options_headers: Option<&ProviderHeaders>,
    fetch: Option<&SharedHttpClient>,
    dynamic_headers: Option<&IndexMap<String, String>>,
    session_id: Option<&str>,
    federation: Option<&AnthropicFederationConfig>,
    identity: Option<&crate::types::ClientIdentity>,
) -> Result<(AnthropicClient, bool), AiError> {
    let fetch = match fetch {
        Some(fetch) => fetch.clone(),
        None => default_http_client()?,
    };
    let target = ClientIdentityTarget::new(model.provider.clone(), model.base_url.clone());
    let identity_headers = get_identity_headers(&target, identity);
    let base_headers = indexmap::IndexMap::from([
        ("accept".to_string(), Some("application/json".to_string())),
        (
            "anthropic-dangerous-direct-browser-access".to_string(),
            Some("true".to_string()),
        ),
    ]);

    let dynamic_owned: Option<ProviderHeaders> = dynamic_headers.map(|headers| {
        headers
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect()
    });

    let model_headers = model_provider_headers(model.headers.as_ref());

    // Copilot：Bearer auth。
    if model.provider == "github-copilot" {
        let merged = merge_headers(&[
            Some(&identity_headers),
            Some(&base_headers),
            model_headers.as_ref(),
            dynamic_owned.as_ref(),
            options_headers,
        ]);
        return Ok((
            AnthropicClient {
                base_url: model.base_url.clone(),
                default_headers: to_string_map(&merged),
                api_key: None,
                auth_token: api_key.map(str::to_string),
                federation: None,
                fetch,
            },
            false,
        ));
    }

    // OAuth：Bearer auth + Claude Code 身份头。
    if let Some(api_key) = api_key.filter(|key| key.contains("sk-ant-oat")) {
        let oauth_headers = indexmap::IndexMap::from([
            ("accept".to_string(), Some("application/json".to_string())),
            (
                "anthropic-dangerous-direct-browser-access".to_string(),
                Some("true".to_string()),
            ),
            (
                "user-agent".to_string(),
                Some(format!("claude-cli/{CLAUDE_CODE_VERSION}")),
            ),
            ("x-app".to_string(), Some("cli".to_string())),
        ]);
        let merged = merge_headers(&[
            Some(&oauth_headers),
            model_headers.as_ref(),
            options_headers,
        ]);
        return Ok((
            AnthropicClient {
                base_url: model.base_url.clone(),
                default_headers: to_string_map(&merged),
                api_key: None,
                auth_token: Some(api_key.to_string()),
                federation: None,
                fetch,
            },
            true,
        ));
    }

    // API key / 头自带鉴权 / federation。
    let compat = get_anthropic_compat(model);
    let mut session_affinity_headers: ProviderHeaders = IndexMap::new();
    if let Some(session_id) = session_id.filter(|session_id| !session_id.is_empty())
        && compat.send_session_affinity_headers
    {
        let header = if compat.session_affinity_format.as_deref() == Some("openrouter") {
            "x-session-id"
        } else {
            "x-session-affinity"
        };
        session_affinity_headers.insert(header.to_string(), Some(session_id.to_string()));
    }
    let merged = merge_headers(&[
        Some(&identity_headers),
        Some(&base_headers),
        Some(&session_affinity_headers),
        model_headers.as_ref(),
        options_headers,
    ]);
    Ok((
        AnthropicClient {
            base_url: model.base_url.clone(),
            default_headers: to_string_map(&merged),
            api_key: api_key.map(str::to_string),
            auth_token: None,
            federation: federation.cloned(),
            fetch,
        },
        false,
    ))
}

/// 一次 Messages 请求（SDK `client.beta.messages.create(params).asResponse()` 的等价物）。
///
/// `params.betas` 只用于 `anthropic-beta` 头，发送前会从 body 中移除。2xx 返回原始响应（body
/// 仍是流）；非 2xx 会先读完整 body，再按 `APIError.makeMessage` 转成
/// [`AnthropicApiError`]。`timeout_ms` 只覆盖等待响应头，不含后续 SSE 读取。
async fn create_message_response(
    client: &AnthropicClient,
    params: &Value,
    betas: Option<&[String]>,
    token_auth: Option<&str>,
    timeout_ms: Option<u64>,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<HttpResponse, AnthropicApiError> {
    let headers = client.headers(betas, token_auth);
    let body = {
        let mut wire_params = params.clone();
        if let Value::Object(map) = &mut wire_params {
            map.shift_remove("betas");
        }
        kiana_json::js_object::stringify(&wire_params).into_bytes()
    };
    let request = HttpRequest {
        method: "POST".into(),
        url: client.build_url(),
        headers,
        body: Some(body),
        // SDK 的 `timeout` 只包住底层 `fetch()`（`fetchWithTimeout` 在响应头到达后
        // `clearTimeout`），**不**限制随后读取 SSE body 的时间。因此超时由这里单独施加，
        // `HttpClient` 的超时（覆盖整个 body）保持关闭。
        timeout_ms: None,
    };
    let send = client.fetch.execute(request, signal);
    let timeout_ms = timeout_ms.filter(|timeout| *timeout > 0);
    let result = match (timeout_ms, signal) {
        (Some(timeout_ms), Some(token)) => {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(HttpError::new(
                    crate::http::reqwest_client::ABORTED_MESSAGE,
                )),
                result = tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    send,
                ) => match result {
                    Ok(result) => result,
                    Err(_) => Err(HttpError::new(
                        crate::http::reqwest_client::TIMEOUT_MESSAGE,
                    )),
                },
            }
        }
        (Some(timeout_ms), None) => {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), send).await {
                Ok(result) => result,
                Err(_) => Err(HttpError::new(crate::http::reqwest_client::TIMEOUT_MESSAGE)),
            }
        }
        (None, _) => send.await,
    };
    match result {
        Ok(response) => {
            let status = response.response.status;
            if (200..300).contains(&status) {
                Ok(response)
            } else {
                let headers = response.response.headers.clone();
                let bytes = match collect_body(response.body).await {
                    Ok(bytes) => bytes,
                    Err(error) => error.message.into_bytes(),
                };
                let text = String::from_utf8_lossy(&bytes).to_string();
                Err(build_api_error(status, &text, &headers))
            }
        }
        Err(error) => {
            // SDK 把底层 fetch 失败包装成 `APIConnectionError`（"Connection error."），
            // 超时是 `APIConnectionTimeoutError`（"Request timed out."）。
            let message = if error.message == crate::http::reqwest_client::TIMEOUT_MESSAGE {
                "Request timed out.".to_string()
            } else if error.message == crate::http::reqwest_client::ABORTED_MESSAGE {
                "Request was aborted".to_string()
            } else {
                "Connection error.".to_string()
            };
            Err(AnthropicApiError {
                status: 0,
                error_type: None,
                message,
                header: None,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// SSE 解码
// ---------------------------------------------------------------------------

/// `ServerSentEvent`。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerSentEvent {
    event: Option<String>,
    data: String,
    raw: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct SseDecoderState {
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

fn flush_sse_event(state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    // TS: `if (!state.event && state.data.length === 0) return null;`——空字符串的 `event:`
    // 是假值：不 flush，也不清空 state（`raw` 行继续累积，出现在后续解析错误信息里）。
    let has_event = state
        .event
        .as_deref()
        .is_some_and(|event| !event.is_empty());
    if !has_event && state.data.is_empty() {
        return None;
    }
    let event = ServerSentEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.event = None;
    state.data.clear();
    Some(event)
}

fn decode_sse_line(line: &str, state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if line.is_empty() {
        return flush_sse_event(state);
    }
    state.raw.push(line.to_string());
    if line.starts_with(':') {
        return None;
    }
    let delimiter_index = line.find(':');
    let (field_name, mut value) = match delimiter_index {
        Some(index) => (&line[..index], line[index + 1..].to_string()),
        None => (line, String::new()),
    };
    if let Some(stripped) = value.strip_prefix(' ') {
        value = stripped.to_string();
    }
    if field_name == "event" {
        state.event = Some(value);
    } else if field_name == "data" {
        state.data.push(value);
    }
    None
}

/// `nextLineBreakIndex`：`\r` 与 `\n` 中最小者（UTF-16 索引在 ASCII 换行下等价）。
fn next_line_break_index(text: &str) -> Option<usize> {
    match (text.find('\r'), text.find('\n')) {
        (None, newline) => newline,
        (carriage_return, None) => carriage_return,
        (Some(carriage_return), Some(newline)) => Some(carriage_return.min(newline)),
    }
}

fn consume_line(text: &str) -> Option<(String, String)> {
    let break_index = next_line_break_index(text)?;
    let bytes = text.as_bytes();
    let mut next_index = break_index + 1;
    if bytes[break_index] == b'\r' && bytes.get(next_index) == Some(&b'\n') {
        next_index += 1;
    }
    Some((
        text[..break_index].to_string(),
        text[next_index..].to_string(),
    ))
}

/// 增量 UTF-8 解码（`TextDecoder` 的 `{stream:true}` 语义）。
///
/// `stream = true` 时把不完整的尾序列留在 `pending` 等下一块；非法字节按 WHATWG 的替换字符
/// U+FFFD 输出。`stream = false` 表示流结束，剩余字节按 lossy 方式冲刷。
#[derive(Debug, Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn decode(&mut self, chunk: &[u8], stream: bool) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        out.push_str(
                            std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"),
                        );
                        self.pending.drain(..valid);
                    }
                    match error.error_len() {
                        None => break,
                        Some(len) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..len);
                        }
                    }
                }
            }
        }
        if !stream && !self.pending.is_empty() {
            out.push_str(&String::from_utf8_lossy(&self.pending));
            self.pending.clear();
        }
        out
    }
}

const ANTHROPIC_MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

/// `iterateSseMessages` + `iterateAnthropicEvents` 的合并实现。
///
/// 数据管线：原始 `body` 字节流 → `Utf8Decoder`（保留跨 chunk 的半截 UTF-8）→ `buffer` 文本
/// → `consume_line` 切出的完整行 → `SseDecoderState` 累积成事件 → 解析后的 JSON 事件。
/// `finished_body` 表示底层流已结束，此时按“解码器冲刷 → 残留行 → 残留 buffer → flush
/// 事件”的顺序收尾；`saw_message_start`/`saw_message_end` 用于检测提前结束。
struct AnthropicSseReader {
    body: crate::http::ByteStream,
    decoder: Utf8Decoder,
    buffer: String,
    state: SseDecoderState,
    signal: Option<tokio_util::sync::CancellationToken>,
    finished_body: bool,
    saw_message_start: bool,
    saw_message_end: bool,
    finished: bool,
}

impl AnthropicSseReader {
    fn new(
        body: crate::http::ByteStream,
        signal: Option<tokio_util::sync::CancellationToken>,
    ) -> Self {
        Self {
            body,
            decoder: Utf8Decoder::default(),
            buffer: String::new(),
            state: SseDecoderState::default(),
            signal,
            finished_body: false,
            saw_message_start: false,
            saw_message_end: false,
            finished: false,
        }
    }

    fn is_aborted(&self) -> bool {
        self.signal
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    }

    /// 处理缓冲区里所有完整的行；返回第一个完整事件。
    fn drain_lines(&mut self) -> Option<ServerSentEvent> {
        while let Some((line, rest)) = consume_line(&self.buffer) {
            self.buffer = rest;
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                return Some(event);
            }
        }
        None
    }

    /// 读取下一个 Anthropic 消息事件；流结束返回 `Ok(None)`。
    ///
    /// 非 Anthropic 事件（`ping`、空 event 名等）会被跳过；`event: error`、JSON 解析失败、
    /// 传输错误都返回 `Err`，由调用方终止流。取消在每个等待点检查，优先于继续读 body。
    #[allow(clippy::result_large_err)]
    async fn next_event(&mut self) -> Result<Option<Value>, AiError> {
        loop {
            if self.finished {
                return Ok(None);
            }
            if self.is_aborted() {
                return Err(AiError::aborted("Request was aborted"));
            }
            if let Some(sse) = self.drain_lines() {
                if let Some(value) = self.handle_event(sse)? {
                    return Ok(Some(value));
                }
                continue;
            }
            // 底层流结束：先冲刷 decoder 里可能残留的半截 UTF-8，再依次消化完整行、无换行
            // 的尾行与状态机里未 flush 的事件，最后才判定流是否异常结束。
            if self.finished_body {
                // 冲刷解码器，再处理残留行与残留 buffer。
                let tail = self.decoder.decode(&[], false);
                self.buffer.push_str(&tail);
                if let Some(sse) = self.drain_lines() {
                    if let Some(value) = self.handle_event(sse)? {
                        return Ok(Some(value));
                    }
                    continue;
                }
                if !self.buffer.is_empty() {
                    let line = std::mem::take(&mut self.buffer);
                    if let Some(sse) = decode_sse_line(&line, &mut self.state) {
                        if let Some(value) = self.handle_event(sse)? {
                            return Ok(Some(value));
                        }
                        continue;
                    }
                }
                if let Some(sse) = flush_sse_event(&mut self.state) {
                    if let Some(value) = self.handle_event(sse)? {
                        return Ok(Some(value));
                    }
                    continue;
                }
                self.finished = true;
                // TS 只要求“有 message_start 就必须有 message_stop”；从未收到 message_start
                // 的流按正常 EOF 处理。
                if self.saw_message_start && !self.saw_message_end {
                    return Err(AiError::new(
                        AiErrorKind::Parse,
                        "Anthropic stream ended before message_stop",
                    ));
                }
                return Ok(None);
            }

            // 没有可消费的完整行时才等待下一个 chunk；有 signal 时用 biased select，
            // 保证取消优先于继续读取。
            let next = match &self.signal {
                Some(token) => {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => return Err(AiError::aborted("Request was aborted")),
                        next = self.body.next() => next,
                    }
                }
                None => self.body.next().await,
            };
            match next {
                Some(Ok(chunk)) => {
                    let text = self.decoder.decode(&chunk, true);
                    self.buffer.push_str(&text);
                }
                Some(Err(error)) => {
                    if self.is_aborted() {
                        return Err(AiError::aborted("Request was aborted"));
                    }
                    return Err(AiError::new(AiErrorKind::Transport, error.message));
                }
                None => {
                    self.finished_body = true;
                }
            }
        }
    }

    #[allow(clippy::result_large_err)]
    fn handle_event(&mut self, sse: ServerSentEvent) -> Result<Option<Value>, AiError> {
        if sse.event.as_deref() == Some("error") {
            return Err(AiError::new(AiErrorKind::Other, sse.data));
        }
        let Some(event_name) = sse.event.clone() else {
            return Ok(None);
        };
        if !ANTHROPIC_MESSAGE_EVENTS.contains(&event_name.as_str()) {
            return Ok(None);
        }
        match parse_json_with_repair(&sse.data) {
            Ok(event) => {
                // TS 在 try 里访问 `event.type`；JSON null 会抛 TypeError，被 catch 包成
                // 解析错误并结束流（V8 的消息逐字）。
                if event.is_null() {
                    return Err(AiError::new(
                        AiErrorKind::Parse,
                        format!(
                            "Could not parse Anthropic SSE event {event_name}: Cannot read properties of null (reading 'type'); data={}; raw={}",
                            sse.data,
                            sse.raw.join("\\n")
                        ),
                    ));
                }
                match str_of(&event, "type") {
                    Some("message_start") => self.saw_message_start = true,
                    Some("message_stop") => self.saw_message_end = true,
                    _ => {}
                }
                Ok(Some(event))
            }
            Err(error) => Err(AiError::new(
                AiErrorKind::Parse,
                format!(
                    "Could not parse Anthropic SSE event {event_name}: {}; data={}; raw={}",
                    error.message,
                    sse.data,
                    sse.raw.join("\\n")
                ),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// `stream`：同步返回事件流，网络与解析在后台任务里进行。
///
/// 同步阶段做两件事：按 `supports_mid_convo_system_messages` 归一化 transcript，并冻结当前
/// 工具列表（OAuth 情形要用它把 Claude Code 工具名还原成调用方名字）。之后把 `run_stream`
/// 交给 `spawn_forward`，本函数立即返回。
///
/// 返回的流不用 `Result` 报错：鉴权、网络、解析、取消等失败都会在后台推
/// `AssistantMessageEvent::Error` 后结束。需要同步拿到鉴权错误时用 [`stream_simple`]。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: AnthropicOptions,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let context = context.clone();
    let compat = get_anthropic_compat(&model);
    let normalized_context =
        resolve_transcript(&context, Some(compat.supports_mid_convo_system_messages));
    let current_tools = get_current_tools(&normalized_context.0.messages);
    let stream_for_task = stream.clone();
    crate::api::lazy::spawn_forward(async move {
        run_stream(
            &model,
            &normalized_context,
            &current_tools,
            options,
            stream_for_task,
        )
        .await;
    });
    stream
}

/// `streamSimple`：缺少鉴权时同步返回 `Err`（等价 TS 的同步 throw）。
///
/// 同步阶段先做鉴权判定：存在 federation 配置或 [`has_request_auth`] 通过才继续，否则返回
/// `No API key for provider: <provider>`。随后把 `SimpleStreamOptions` 翻译成
/// [`AnthropicOptions`]：
///
/// - `reasoning` 为 `None` → 显式关闭思考；
/// - `tool_choice` 的 `Auto`/`None` 分别写成 `{ "type": "auto" }` / `{ "type": "none" }`；
/// - `forceAdaptiveThinking` → 设置 effort 并走 adaptive thinking；
/// - 模型没有 `max_tokens` → 保留 TS 的 NaN 语义：请求体写 `max_tokens: null`、budget 回落
///   到 1024；
/// - 其余情况按 [`adjust_max_tokens_for_thinking`] 计算预算，再按上下文上限裁剪。
#[allow(clippy::result_large_err)]
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    let api_key = options.base.api_key.clone();
    let federation = get_anthropic_federation(
        model,
        api_key.as_deref(),
        options.base.headers.as_ref(),
        options.base.env.as_ref(),
    );
    if federation.is_none() {
        assert_request_auth(
            &model.provider,
            api_key.as_deref(),
            options.base.headers.as_ref(),
        )?;
    }

    let mut base = build_base_options(model, context, Some(&options), api_key.as_deref());
    if let Some(tool_choice) = options.tool_choice {
        base.extra.insert(
            "toolChoice".into(),
            match tool_choice {
                crate::types::ToolChoice::Auto => json!({ "type": "auto" }),
                crate::types::ToolChoice::None => json!({ "type": "none" }),
            },
        );
    }
    let mut anthropic_options = AnthropicOptions::from_stream_options(base);
    let model_max_tokens_missing = model.max_tokens.is_none();
    let base_max_tokens_missing = anthropic_options.base.max_tokens.is_none();
    let Some(reasoning) = options.reasoning else {
        // TS: `buildBaseOptions` 用 `clampMaxTokensToContext(..., undefined)` 得到 NaN，
        // 序列化为 `max_tokens: null`。
        anthropic_options.force_null_max_tokens =
            model_max_tokens_missing && base_max_tokens_missing;
        anthropic_options.thinking_enabled = Some(false);
        return Ok(stream(model, context, anthropic_options));
    };

    if force_adaptive_thinking(model) {
        anthropic_options.force_null_max_tokens =
            model_max_tokens_missing && base_max_tokens_missing;
        let effort = map_thinking_level_to_effort(model, Some(reasoning));
        anthropic_options.thinking_enabled = Some(true);
        anthropic_options.effort = Some(effort);
        return Ok(stream(model, context, anthropic_options));
    }

    if model_max_tokens_missing {
        // TS: `Math.min(baseMaxTokens + thinkingBudget, undefined)` 是 NaN，随后
        // `clampMaxTokensToContext` 仍是 NaN → `max_tokens: null`；而
        // `Math.min(thinkingBudget, Math.max(0, NaN - 1024))` 也是 NaN，
        // 请求体里的 `thinkingBudgetTokens || 1024` 把 NaN 当作假值 → `budget_tokens: 1024`。
        anthropic_options.force_null_max_tokens = true;
        anthropic_options.base.max_tokens = None;
        anthropic_options.thinking_enabled = Some(true);
        anthropic_options.thinking_budget_tokens = Some(0);
        return Ok(stream(model, context, anthropic_options));
    }
    let adjusted = adjust_max_tokens_for_thinking(
        anthropic_options.base.max_tokens,
        model.max_tokens.unwrap_or(0),
        reasoning,
        options.thinking_budgets.as_ref(),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
    let thinking_budget = adjusted
        .thinking_budget
        .min(max_tokens.saturating_sub(1024));
    anthropic_options.base.max_tokens = Some(max_tokens);
    anthropic_options.thinking_enabled = Some(true);
    anthropic_options.thinking_budget_tokens = Some(thinking_budget);
    Ok(stream(model, context, anthropic_options))
}

/// `anthropicMessagesApi()`：`lazyApi(() => import("./anthropic-messages.ts"))`。
///
/// 返回的 [`ProviderStreams`] 延迟构造 `AnthropicMessagesApi`；其中的 `stream_simple` 会把
/// [`stream_simple`] 的同步 `Err` 经 [`lazy_stream_error`] 转成错误事件，满足冻结接口
/// “同步返回流”的签名。
pub fn anthropic_messages_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| {
            Box::pin(async { Ok(Arc::new(AnthropicMessagesApi) as Arc<dyn ProviderStreams>) })
        }),
        None,
    )
}

/// [`ProviderStreams`] 的适配器：把冻结形态的选项转成 [`AnthropicOptions`] 后转发到模块级
/// [`stream`]/[`stream_simple`]，并把同步错误转为错误事件。
struct AnthropicMessagesApi;

impl ProviderStreams for AnthropicMessagesApi {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            AnthropicOptions::from_stream_options(options),
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

/// 后台任务主体：调用 `run_stream_inner`，并把失败统一翻译成 `error` 事件。
///
/// 出错时按 `options.base.signal` 是否已取消区分 `aborted` / `error`，写回
/// `output.stop_reason` 与 `error_message`，推 `AssistantMessageEvent::Error` 后结束流；
/// 成功路径的 `done` 事件和 `stream.end` 由 `run_stream_inner` 自己完成。
async fn run_stream(
    model: &Model,
    normalized_context: &TranscriptContext,
    current_tools: &[Tool],
    options: AnthropicOptions,
    stream: AssistantMessageEventStream,
) {
    let mut output = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: zero_usage(),
        stop_reason: StopReason::Pending,
        timestamp: now_ms(),
        ..Default::default()
    };
    let mut usage_model = model.clone();
    let mut input_transformations: Option<Vec<Value>> = None;

    let result = run_stream_inner(
        model,
        normalized_context,
        current_tools,
        &options,
        &stream,
        &mut output,
        &mut usage_model,
        &mut input_transformations,
    )
    .await;

    match result {
        Ok(()) => {}
        Err(error) => {
            let aborted = options
                .base
                .signal
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
            output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            output.error_message = Some(error.message);
            let reason = if aborted {
                ErrorReason::Aborted
            } else {
                ErrorReason::Error
            };
            let _ = stream.push(AssistantMessageEvent::Error {
                reason,
                error: Box::new(output.clone()),
            });
            stream.end(output);
        }
    }
}

fn zero_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

#[allow(clippy::too_many_arguments, clippy::result_large_err)]
async fn run_stream_inner(
    model: &Model,
    normalized_context: &TranscriptContext,
    current_tools: &[Tool],
    options: &AnthropicOptions,
    stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
    usage_model: &mut Model,
    input_transformations: &mut Option<Vec<Value>>,
) -> Result<(), AiError> {
    // 阶段 1：鉴权。federation 仅在 provider=anthropic 且当前没有其他鉴权时返回配置；
    // federation 与 api key/头都没有则直接失败，该错误不进入重试。
    let api_key = options.base.api_key.clone();
    let federation = get_anthropic_federation(
        model,
        api_key.as_deref(),
        options.base.headers.as_ref(),
        options.base.env.as_ref(),
    );
    if federation.is_none() {
        assert_request_auth(
            &model.provider,
            api_key.as_deref(),
            options.base.headers.as_ref(),
        )?;
    }

    // 阶段 2：装配客户端。Copilot 需要按消息内容生成动态头；缓存关闭（None）时不发
    // 会话亲和头，因此 session_id 也一并丢弃。
    let copilot_dynamic_headers = if model.provider == "github-copilot" {
        Some(build_copilot_dynamic_headers(
            &normalized_context.0.messages,
            has_copilot_vision_input(&normalized_context.0.messages),
        ))
    } else {
        None
    };
    let cache_retention =
        resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    let cache_session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.base.session_id.clone()
    };
    let (client, is_oauth) = create_client(
        model,
        api_key.as_deref(),
        options.base.headers.as_ref(),
        options.base.fetch.as_ref(),
        copilot_dynamic_headers.as_ref(),
        cache_session_id.as_deref(),
        federation.as_ref(),
        options.base.identity.as_ref(),
    )?;
    // SDK `validateHeaders`：鉴权头在**请求之前**解析失败时抛普通 Error（不是 APIError），
    // 因此 `retryProviderRequest` 不会重试它。放在重试循环外，等价于“每次请求都同样失败”。
    client.validate_headers(&client.headers(None, None))?;
    // SDK `validatePositiveInteger('timeout', options.timeout)`：`timeoutMs: 0` 直接抛错。
    if options.base.timeout_ms == Some(0) {
        return Err(AiError::new(
            AiErrorKind::Other,
            "timeout must be a positive integer",
        ));
    }

    // 阶段 3：strict 工具降级循环。每轮重新 build_params（降级后 strict 被移除）；
    // `retry_provider_request` 只包住“建立响应”，拿到 2xx 之后的流错误不会触发重试。
    let mut disable_preferred_strict_tools = false;
    let response = loop {
        if options
            .base
            .signal
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(AiError::aborted("Request was aborted"));
        }
        let built = build_params(
            model,
            normalized_context,
            is_oauth,
            options,
            disable_preferred_strict_tools,
        )?;
        if let Some(level) = &built.provider_thinking_level {
            output.provider_thinking_level = Some(level.clone());
        }
        let mut params = built.params;
        if let Some(next) = options.base.apply_on_payload(params.clone(), model).await {
            let mut map = match next {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            map.insert("stream".into(), Value::Bool(true));
            params = Value::Object(map);
        }
        let betas = params
            .as_object()
            .and_then(|params| params.get("betas"))
            .and_then(Value::as_array)
            .map(|betas| {
                betas
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });
        let last_error_slot = Arc::new(Mutex::new(None::<AnthropicApiError>));
        // SDK 在每次请求的 `authHeaders()` 里取 token cache 的 token；federation 交换失败
        // 抛的是 `WorkloadIdentityError`（没有 `status`/`headers` 字段），`retryProviderRequest`
        // 视为非 provider 错误**立即抛出**，不进入重试。这里在任何 HTTP 尝试之前交换一次，
        // 失败直接冒泡（token 由模块级缓存复用）。
        let token_auth = match client.federation.as_ref() {
            Some(federation) => Some(
                federation_access_token(
                    &client.base_url,
                    federation,
                    &client.fetch,
                    options.base.signal.as_ref(),
                )
                .await?,
            ),
            None => None,
        };
        let timeout_ms = anthropic_request_timeout_ms(options.base.timeout_ms);
        let signal = options.base.signal.clone();
        let retry_options = ProviderRetryOptions {
            max_retries: options.base.max_retries.map(u64::from),
            max_retry_delay_ms: options.base.max_retry_delay_ms,
            signal: signal.clone().map(crate::utils::abort::AbortScope::from),
            no_retry_statuses: Vec::new(),
        };
        let request_result = retry_provider_request(
            || {
                let client = &client;
                let params = params.clone();
                let betas = betas.clone();
                let token_auth = token_auth.clone();
                let signal = signal.clone();
                let last_error_slot = last_error_slot.clone();
                async move {
                    match create_message_response(
                        client,
                        &params,
                        betas.as_deref(),
                        token_auth.as_deref(),
                        timeout_ms,
                        signal.as_ref(),
                    )
                    .await
                    {
                        Ok(response) => Ok(response),
                        Err(error) => {
                            let provider_error = ProviderError {
                                message: error.message.clone(),
                                status: (error.status != 0).then_some(error.status),
                                header: error.header.clone(),
                            };
                            *last_error_slot
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
                            Err(provider_error)
                        }
                    }
                }
            },
            retry_options,
        )
        .await;
        match request_result {
            Ok(response) => break response,
            Err(error) => {
                let last_error = last_error_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                // 只有“本次请求确实带了 strict:prefer 工具”，且失败是 400
                // invalid_request_error 并命中三个已知容量限制子串时才降级；已取消或已经
                // 降级过则原样返回。
                if options
                    .base
                    .signal
                    .as_ref()
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                    || disable_preferred_strict_tools
                    || !built.has_preferred_strict_tools
                {
                    return Err(error);
                }
                let Some(last_error) = last_error else {
                    return Err(error);
                };
                let Some(status) = (last_error.status != 0).then_some(last_error.status) else {
                    return Err(error);
                };
                if status != 400
                    || last_error.error_type.as_deref() != Some("invalid_request_error")
                    || ![
                        "The compiled grammar is too large",
                        "Too many strict tools (",
                        "Schemas contains too many parameters with union types (",
                    ]
                    .iter()
                    .any(|needle| last_error.message.contains(needle))
                {
                    return Err(error);
                }
                disable_preferred_strict_tools = true;
                append_assistant_message_diagnostic(
                    output,
                    create_assistant_message_diagnostic(
                        "anthropic_strict_tools_fallback",
                        &ThrownValue::Error(crate::types::DiagnosticErrorInfo {
                            name: Some("Error".into()),
                            message: last_error.message.clone(),
                            stack: None,
                            code: None,
                        }),
                        Some(indexmap::IndexMap::from([(
                            "reason".to_string(),
                            Value::String("strict_tool_limits_exceeded".into()),
                        )])),
                    ),
                );
            }
        }
    };
    if let Some(on_response) = &options.base.on_response {
        on_response(
            crate::types::ProviderResponse {
                status: response.response.status,
                headers: headers_to_record(&response.response.headers),
            },
            model,
        );
    }
    let _ = stream.push(AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });

    // 阶段 4：消费 SSE。Anthropic 的 block index 是服务端下标，`open_blocks` 把它映射到
    // `output.content` 的下标（两者可能因被忽略的块而不一致）；tool_use 块额外持有增量
    // JSON 解析器，`content_block_stop` 时才 finish 成参数对象。
    let mut open_blocks: HashMap<i64, OpenBlock> = HashMap::new();
    let mut reader = AnthropicSseReader::new(response.body, options.base.signal.clone());

    while let Some(event) = reader.next_event().await? {
        if let Some(callback) = &options.base.on_provider_stream_event {
            callback(event.clone(), model);
        }
        match str_of(&event, "type").unwrap_or_default() {
            // message_start：记录响应 id 与实际模型；实际模型与请求不同时，按
            // allowedFallbackModels 中该模型的 cost 计价。usage 经 token_count 归一化
            // （非数值/非正数/缺失 → 0），total_tokens 与 cost 都在本地重算。
            "message_start" => {
                let message = obj(&event, "message").cloned().unwrap_or(Value::Null);
                if let Some(id) = str_of(&message, "id") {
                    output.response_id = Some(id.to_string());
                }
                if let Some(transformations) = obj(&message, "input_transformations")
                    && let Value::Array(transformations) = transformations
                {
                    *input_transformations = Some(transformations.clone());
                }
                let response_model = str_of(&message, "model").map(str::to_string);
                if let Some(response_model) = &response_model
                    && response_model != &model.id
                {
                    output.response_model = Some(response_model.clone());
                }
                let fallback_cost = response_model.as_ref().and_then(|response_model| {
                    if response_model == &model.id {
                        return None;
                    }
                    allowed_fallback_models(model)
                        .into_iter()
                        .find(|fallback| {
                            str_of(fallback, "provider") == Some(model.provider.as_str())
                                && str_of(fallback, "model") == Some(response_model.as_str())
                        })
                        .and_then(|fallback| obj(&fallback, "cost").cloned())
                });
                *usage_model = match (&fallback_cost, &response_model) {
                    (Some(cost), Some(response_model)) => {
                        let mut usage_model = model.clone();
                        usage_model.id = response_model.clone();
                        usage_model.cost = model_cost_from_json(cost);
                        usage_model
                    }
                    _ => model.clone(),
                };
                let usage = obj(&message, "usage").cloned().unwrap_or(Value::Null);
                output.usage.input = token_count(obj(&usage, "input_tokens"));
                output.usage.output = token_count(obj(&usage, "output_tokens"));
                output.usage.cache_read = token_count(obj(&usage, "cache_read_input_tokens"));
                output.usage.cache_write = token_count(obj(&usage, "cache_creation_input_tokens"));
                output.usage.cache_write1h =
                    Some(token_count(obj(&usage, "cache_creation").and_then(
                        |cache_creation| obj(cache_creation, "ephemeral_1h_input_tokens"),
                    )));
                output.usage.total_tokens = output.usage.input
                    + output.usage.output
                    + output.usage.cache_read
                    + output.usage.cache_write;
                calculate_cost(usage_model, &mut output.usage);
            }
            "content_block_start" => {
                let index = u64_field(&event, "index").unwrap_or(0) as i64;
                let Some(content_block) = obj(&event, "content_block") else {
                    continue;
                };
                match str_of(content_block, "type").unwrap_or_default() {
                    // fallback 块出现在已有输出之后，说明服务端在输出中途换了模型，无法
                    // 安全拼接，直接报错；出现在开头则跳过（后续内容来自 fallback 模型）。
                    "fallback" => {
                        if !output.content.is_empty() {
                            return Err(AiError::new(
                                AiErrorKind::Other,
                                "Anthropic performed an unsupported mid-output model fallback",
                            ));
                        }
                        continue;
                    }
                    "text" => {
                        output
                            .content
                            .push(AssistantContentBlock::Text(TextContent {
                                text: str_of(content_block, "text")
                                    .unwrap_or_default()
                                    .to_string(),
                                ..Default::default()
                            }));
                        let content_index = output.content.len() - 1;
                        open_blocks.insert(
                            index,
                            OpenBlock {
                                content_index,
                                arguments: None,
                            },
                        );
                        let _ = stream.push(AssistantMessageEvent::TextStart {
                            content_index,
                            partial: Box::new(output.clone()),
                        });
                    }
                    "thinking" => {
                        output
                            .content
                            .push(AssistantContentBlock::Thinking(ThinkingContent {
                                thinking: str_of(content_block, "thinking")
                                    .unwrap_or_default()
                                    .to_string(),
                                thinking_signature: Some(
                                    str_of(content_block, "signature")
                                        .unwrap_or_default()
                                        .to_string(),
                                ),
                                ..Default::default()
                            }));
                        let content_index = output.content.len() - 1;
                        open_blocks.insert(
                            index,
                            OpenBlock {
                                content_index,
                                arguments: None,
                            },
                        );
                        let _ = stream.push(AssistantMessageEvent::ThinkingStart {
                            content_index,
                            partial: Box::new(output.clone()),
                        });
                    }
                    "redacted_thinking" => {
                        output
                            .content
                            .push(AssistantContentBlock::Thinking(ThinkingContent {
                                thinking: "[Reasoning redacted]".into(),
                                thinking_signature: str_of(content_block, "data")
                                    .map(str::to_string),
                                redacted: Some(true),
                                ..Default::default()
                            }));
                        let content_index = output.content.len() - 1;
                        open_blocks.insert(
                            index,
                            OpenBlock {
                                content_index,
                                arguments: None,
                            },
                        );
                        let _ = stream.push(AssistantMessageEvent::ThinkingStart {
                            content_index,
                            partial: Box::new(output.clone()),
                        });
                    }
                    // OAuth 下把 Claude Code 规范名还原为调用方的工具名；`input` 作为初始
                    // 参数，后续的 input_json_delta 由增量解析器继续累积。
                    "tool_use" => {
                        let name = str_of(content_block, "name").unwrap_or_default();
                        let name = if is_oauth {
                            from_claude_code_name(name, current_tools)
                        } else {
                            name.to_string()
                        };
                        let arguments = match obj(content_block, "input") {
                            Some(Value::Object(map)) => map
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect(),
                            _ => JsonObject::new(),
                        };
                        output
                            .content
                            .push(AssistantContentBlock::ToolCall(ToolCall {
                                id: str_of(content_block, "id").unwrap_or_default().to_string(),
                                name,
                                arguments,
                                ..Default::default()
                            }));
                        let content_index = output.content.len() - 1;
                        open_blocks.insert(
                            index,
                            OpenBlock {
                                content_index,
                                arguments: Some(ToolCallArgumentsStream::new()),
                            },
                        );
                        let _ = stream.push(AssistantMessageEvent::ToolCallStart {
                            content_index,
                            partial: Box::new(output.clone()),
                        });
                    }
                    _ => {}
                }
            }
            // 只有已打开的块才处理（未知下标按 TS 的 `openBlock` 返回 undefined 处理，整条
            // delta 被忽略）；按 block 类型追加内容并推对应 delta 事件。
            "content_block_delta" => {
                let index = u64_field(&event, "index").unwrap_or(0) as i64;
                let Some(delta) = obj(&event, "delta") else {
                    continue;
                };
                let Some(open) = open_blocks.get_mut(&index) else {
                    continue;
                };
                let content_index = open.content_index;
                match str_of(delta, "type").unwrap_or_default() {
                    "text_delta" => {
                        if let Some(AssistantContentBlock::Text(block)) =
                            output.content.get_mut(content_index)
                        {
                            let text = str_of(delta, "text").unwrap_or_default();
                            block.text.push_str(text);
                            let _ = stream.push(AssistantMessageEvent::TextDelta {
                                content_index,
                                delta: text.to_string(),
                                partial: Box::new(output.clone()),
                            });
                        }
                    }
                    "thinking_delta" => {
                        if let Some(AssistantContentBlock::Thinking(block)) =
                            output.content.get_mut(content_index)
                        {
                            let thinking = str_of(delta, "thinking").unwrap_or_default();
                            block.thinking.push_str(thinking);
                            let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                                content_index,
                                delta: thinking.to_string(),
                                partial: Box::new(output.clone()),
                            });
                        }
                    }
                    "input_json_delta" => {
                        let partial_json = str_of(delta, "partial_json").unwrap_or_default();
                        if let Some(AssistantContentBlock::ToolCall(block)) =
                            output.content.get_mut(content_index)
                            && let Some(arguments) = open.arguments.as_mut()
                        {
                            block.arguments = arguments.append(partial_json);
                            let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                                content_index,
                                delta: partial_json.to_string(),
                                partial: Box::new(output.clone()),
                            });
                        }
                    }
                    "signature_delta" => {
                        if let Some(AssistantContentBlock::Thinking(block)) =
                            output.content.get_mut(content_index)
                        {
                            let signature =
                                block.thinking_signature.get_or_insert_with(String::new);
                            signature.push_str(str_of(delta, "signature").unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            // 未登记的 index 直接忽略；tool_use 的参数在 stop 时才 finish（此时才最终确定
            // 参数 JSON 是否完整），并推 ToolCallEnd。
            "content_block_stop" => {
                let index = u64_field(&event, "index").unwrap_or(0) as i64;
                let Some(open) = open_blocks.remove(&index) else {
                    continue;
                };
                let content_index = open.content_index;
                match output.content.get(content_index) {
                    Some(AssistantContentBlock::Text(block)) => {
                        let content = block.text.clone();
                        let _ = stream.push(AssistantMessageEvent::TextEnd {
                            content_index,
                            content,
                            partial: Box::new(output.clone()),
                        });
                    }
                    Some(AssistantContentBlock::Thinking(block)) => {
                        let content = block.thinking.clone();
                        let _ = stream.push(AssistantMessageEvent::ThinkingEnd {
                            content_index,
                            content,
                            partial: Box::new(output.clone()),
                        });
                    }
                    Some(AssistantContentBlock::ToolCall(_)) => {
                        if let Some(arguments) = open.arguments {
                            let finish = arguments.finish();
                            if let Some(AssistantContentBlock::ToolCall(block)) =
                                output.content.get_mut(content_index)
                            {
                                block.arguments = finish.arguments;
                                block.arguments_error = finish.arguments_error;
                                let tool_call = block.clone();
                                let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
                                    content_index,
                                    tool_call,
                                    partial: Box::new(output.clone()),
                                });
                            }
                        }
                    }
                    None => {}
                }
            }
            // usage 只覆盖出现且非 null 的字段（复刻 TS 的字段非 null 才覆盖语义）；随后重算
            // total_tokens 与 cost。OpenRouter 还会用上报的 cost 覆盖本地计价。
            "message_delta" => {
                if let Some(transformations) = obj(&event, "input_transformations")
                    && let Value::Array(transformations) = transformations
                {
                    *input_transformations = Some(transformations.clone());
                }
                let delta = obj(&event, "delta").cloned().unwrap_or(Value::Null);
                if let Some(stop_reason) = str_of(&delta, "stop_reason") {
                    output.raw_stop_reason = Some(stop_reason.to_string());
                    let explanation = obj(&delta, "stop_details")
                        .and_then(|details| str_of(details, "explanation"));
                    let (mapped, error_message) = map_stop_reason(stop_reason, explanation)?;
                    output.stop_reason = mapped;
                    if let Some(error_message) = error_message {
                        output.error_message = Some(error_message);
                    }
                }
                let usage = obj(&event, "usage").cloned().unwrap_or(Value::Null);
                if obj(&event, "usage").is_some() {
                    if let Some(value) = non_null(obj(&usage, "input_tokens")) {
                        output.usage.input = token_count(Some(value));
                    }
                    if let Some(value) = non_null(obj(&usage, "output_tokens")) {
                        output.usage.output = token_count(Some(value));
                    }
                    if let Some(value) = non_null(obj(&usage, "cache_read_input_tokens")) {
                        output.usage.cache_read = token_count(Some(value));
                    }
                    if let Some(value) = non_null(obj(&usage, "cache_creation_input_tokens")) {
                        output.usage.cache_write = token_count(Some(value));
                    }
                    if let Some(value) = obj(&usage, "cache_creation").and_then(|cache_creation| {
                        non_null(obj(cache_creation, "ephemeral_1h_input_tokens"))
                    }) {
                        output.usage.cache_write1h = Some(token_count(Some(value)));
                    }
                    if let Some(value) = obj(&usage, "output_tokens_details")
                        .and_then(|details| non_null(obj(details, "thinking_tokens")))
                    {
                        output.usage.reasoning = Some(token_count(Some(value)));
                    }
                }
                output.usage.total_tokens = output.usage.input
                    + output.usage.output
                    + output.usage.cache_read
                    + output.usage.cache_write;
                calculate_cost(usage_model, &mut output.usage);
                if is_open_router_model(&model.provider, &model.base_url)
                    && let Some(usage) = obj(&event, "usage")
                    && let Some(accounting) = open_router_usage_accounting(usage)
                {
                    apply_open_router_reported_cost(&mut output.usage, &accounting);
                }
            }
            _ => {}
        }
    }

    if options
        .base
        .signal
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(AiError::aborted("Request was aborted"));
    }
    // 阶段 5：收尾校验。提前取消、缺少 stop_reason、或 stop_reason 本身是 error/aborted
    // 都视为失败，由 `run_stream` 统一转成 error 事件。
    if output.stop_reason == StopReason::Pending {
        return Err(AiError::new(
            AiErrorKind::Other,
            "Anthropic stream ended without a stop reason",
        ));
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        return Err(AiError::new(
            AiErrorKind::Other,
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_string()),
        ));
    }
    if let Some(transformations) = input_transformations.as_ref()
        && !transformations.is_empty()
    {
        let mapped: Vec<Value> = transformations
            .iter()
            .map(|transformation| {
                let mut map = Map::new();
                if let Some(kind) = str_of(transformation, "type") {
                    map.insert("type".into(), Value::String(kind.to_string()));
                }
                if let Some(path) = str_of(transformation, "path") {
                    map.insert("path".into(), Value::String(path.to_string()));
                }
                if let Some(reason) = str_of(transformation, "reason") {
                    map.insert("reason".into(), Value::String(reason.to_string()));
                }
                Value::Object(map)
            })
            .collect();
        append_assistant_message_diagnostic(
            output,
            crate::types::AssistantMessageDiagnostic {
                kind: "anthropic_input_transformations".into(),
                timestamp: now_ms(),
                error: None,
                details: Some(indexmap::IndexMap::from([(
                    "transformations".to_string(),
                    Value::Array(mapped),
                )])),
            },
        );
    }

    let reason = match output.stop_reason {
        StopReason::Stop => DoneReason::Stop,
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    };
    let _ = stream.push(AssistantMessageEvent::Done {
        reason,
        message: Box::new(output.clone()),
    });
    stream.end(output.clone());
    Ok(())
}

/// 一个尚未收到 `content_block_stop` 的内容块。
///
/// 记录它在 `output.content` 里的下标（与 Anthropic 的 block index 不同），以及仅 tool_use
/// 才有的增量 JSON 解析器。
struct OpenBlock {
    content_index: usize,
    arguments: Option<ToolCallArgumentsStream>,
}

/// 把 `compat.allowedFallbackModels[].cost` 的 JSON 还原成 [`crate::types::ModelCost`]。
///
/// 缺失的数值字段按 0 处理（与 TS 读取可选字段的默认行为一致）；`tiers` 只在存在时构建。
/// 仅在 `message_start` 报告了 fallback 模型时使用，用于按实际模型计价。
fn model_cost_from_json(cost: &Value) -> crate::types::ModelCost {
    let number = |key: &str| obj(cost, key).and_then(Value::as_f64).unwrap_or(0.0);
    let tiers = obj(cost, "tiers").and_then(Value::as_array).map(|tiers| {
        tiers
            .iter()
            .map(|tier| crate::types::ModelCostTier {
                input_tokens_above: number_of(tier, "inputTokensAbove"),
                input: number_of(tier, "input"),
                output: number_of(tier, "output"),
                cache_read: number_of(tier, "cacheRead"),
                cache_write: number_of(tier, "cacheWrite"),
            })
            .collect()
    });
    crate::types::ModelCost {
        input: number("input"),
        output: number("output"),
        cache_read: number("cacheRead"),
        cache_write: number("cacheWrite"),
        tiers,
    }
}

fn number_of(value: &Value, key: &str) -> f64 {
    obj(value, key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// 从流式 `message_delta.usage` 的原始 JSON 还原 OpenRouter accounting。
///
/// TS 把事件里的原始对象**直接强转**成 `OpenRouterUsageAccounting` 传给
/// `applyOpenRouterReportedCost`，因此运行期字段可能是 `undefined`；`undefined` 与
/// `null` 的语义不同（`is_byok` 用严格比较，`upstream_inference_cost` 用 `isValidCost`
/// 区分缺失与显式 `null`）。Rust 的 [`OpenRouterUsageAccounting`] 是类型化结构，无法表达
/// `undefined`，所以这里先按 TS 的判定顺序求出「可应用」的 accounting；返回 `None` 表示
/// TS 会在改写 usage 之前 return。
///
/// TS: `packages/ai/src/utils/openrouter-cost.ts#applyOpenRouterReportedCost`
fn open_router_usage_accounting(usage: &Value) -> Option<OpenRouterUsageAccounting> {
    let number = |value: Option<&Value>| value.and_then(Value::as_f64);
    // `if (!isValidCost(reported.cost)) return;`
    let cost = number(obj(usage, "cost")).filter(|value| is_valid_cost(Some(*value)))?;
    let is_byok = match obj(usage, "is_byok") {
        // TS 的 `=== false` / `=== true`：其它值（含缺失、`0`、`"true"`）不进入任何分支。
        Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        _ => return None,
    };
    if !is_byok {
        return Some(OpenRouterUsageAccounting {
            cost: Some(cost),
            is_byok: false,
            cost_details: None,
        });
    }
    // TS：`const details = reported.cost_details; if (!details) return;`。非对象的真值取不到
    // `upstream_inference_cost`（undefined）→ `isValidCost` 失败 → 同样不修改 usage。
    let details = obj(usage, "cost_details").filter(|value| value.is_object())?;
    let upstream_inference_cost = match obj(details, "upstream_inference_cost") {
        // `undefined`：`isValidCost` 失败 → 不修改 usage；只有显式 `null` 才回落到分量求和。
        None => return None,
        Some(Value::Null) => {
            let prompt = number(obj(details, "upstream_inference_prompt_cost"));
            let completions = number(obj(details, "upstream_inference_completions_cost"));
            match (prompt, completions) {
                (Some(prompt), Some(completions))
                    if is_valid_cost(Some(prompt)) && is_valid_cost(Some(completions)) =>
                {
                    prompt + completions
                }
                _ => return None,
            }
        }
        Some(value) => {
            let value = number(Some(value))?;
            if !is_valid_cost(Some(value)) {
                return None;
            }
            value
        }
    };
    Some(OpenRouterUsageAccounting {
        cost: Some(cost),
        is_byok: true,
        cost_details: Some(OpenRouterCostDetails {
            upstream_inference_cost: Some(upstream_inference_cost),
            upstream_inference_prompt_cost: 0.0,
            upstream_inference_completions_cost: 0.0,
            server_tool_cost: None,
        }),
    })
}

// ---------------------------------------------------------------------------
// streamSimple 辅助
// ---------------------------------------------------------------------------

/// `mapThinkingLevelToEffort(model, level)`。
///
/// 先查 `model.thinking_level_map`（键是模型级 thinking level）：映射为 `Some("...")` 时直接
/// 返回；映射为 `null` 时忽略并落到默认表。默认表：minimal/low → `"low"`，medium →
/// `"medium"`，high/xhigh/max 以及 `None` → `"high"`。
pub fn map_thinking_level_to_effort(
    model: &Model,
    level: Option<ThinkingLevel>,
) -> AnthropicEffort {
    let model_level = level.map(thinking_level_to_model);
    if let Some(model_level) = model_level
        && let Some(Some(mapped)) = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&model_level))
    {
        return mapped.clone();
    }
    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => "low".to_string(),
        Some(ThinkingLevel::Medium) => "medium".to_string(),
        Some(ThinkingLevel::High) => "high".to_string(),
        _ => "high".to_string(),
    }
}

fn thinking_level_to_model(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

// ---------------------------------------------------------------------------
// buildParams
// ---------------------------------------------------------------------------

struct BuiltParams {
    params: Value,
    has_preferred_strict_tools: bool,
    provider_thinking_level: Option<AnthropicEffort>,
}

fn normalize_tool_call_id(id: &str) -> String {
    // TS: `id.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64)`。正则按 **UTF-16 code unit**
    // 匹配，因此星平面字符（代理对）会变成两个下划线；`.slice(0, 64)` 同样按 code unit。
    let replaced: Vec<u16> = id
        .encode_utf16()
        .map(|unit| {
            let is_ascii_alphanumeric = u16::from(b'a') <= unit && unit <= u16::from(b'z')
                || u16::from(b'A') <= unit && unit <= u16::from(b'Z')
                || u16::from(b'0') <= unit && unit <= u16::from(b'9');
            if is_ascii_alphanumeric || unit == u16::from(b'_') || unit == u16::from(b'-') {
                unit
            } else {
                u16::from(b'_')
            }
        })
        .take(64)
        .collect();
    String::from_utf16_lossy(&replaced)
}

/// `isAnthropicStrictUnsupportedKeyword(key, value)`。
///
/// 判定 strict 模式下必须从 schema 剔除的关键字：固定的 11 个不支持关键字；`minItems` 只
/// 允许 0/1；`format` 只允许 10 个白名单值。返回 `true` 表示该键/值不被 Anthropic 接受。
fn is_anthropic_strict_unsupported_keyword(key: &str, value: &Value) -> bool {
    const UNSUPPORTED: [&str; 11] = [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "maxItems",
        "uniqueItems",
        "minContains",
        "maxContains",
        "minProperties",
        "maxProperties",
    ];
    const ALLOWED_FORMATS: [&str; 10] = [
        "date-time",
        "time",
        "date",
        "duration",
        "email",
        "hostname",
        "uri",
        "ipv4",
        "ipv6",
        "uuid",
    ];
    if UNSUPPORTED.contains(&key) {
        return true;
    }
    if key == "minItems" {
        return !matches!(value.as_f64(), Some(number) if number == 0.0 || number == 1.0);
    }
    if key == "format" {
        return !value
            .as_str()
            .is_some_and(|format| ALLOWED_FORMATS.contains(&format));
    }
    false
}

/// `constrainedSampling.type === "json_schema" && constrainedSampling.strict === "prefer"`
/// （TS 的**严格**相等；缺省 `strict` 不算 prefer）。
fn is_preferred_strict_sampling(tool: &Tool) -> bool {
    match &tool.constrained_sampling {
        Some(crate::types::ConstrainedSampling::Config(
            crate::types::ConstrainedSamplingConfig::JsonSchema {
                strict: crate::types::StrictMode::Prefer,
            },
        )) => true,
        Some(crate::types::ConstrainedSampling::Raw(value)) => {
            value.get("type").and_then(Value::as_str) == Some("json_schema")
                && value.get("strict").and_then(Value::as_str) == Some("prefer")
        }
        _ => false,
    }
}

fn is_anthropic_effort(value: Option<&str>) -> bool {
    value.is_some_and(|value| ANTHROPIC_EFFORTS.contains(&value))
}

fn convert_image_block(image: &ImageContent) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": image.mime_type,
            "data": image.data,
        },
    })
}

/// 用户内容 → Anthropic 的 `content` 字段。
///
/// 无图片时压成单个字符串（多段文本用 `\n` 连接）；有图片时生成块数组，并在一个文本块都
/// 没有时前置 `"(see attached image)"`（逐字，见 `convertContentBlocks`）。
fn convert_content_blocks(content: &[UserContentBlock]) -> Value {
    let has_images = content
        .iter()
        .any(|block| matches!(block, UserContentBlock::Image(_)));
    if !has_images {
        let text = content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Value::String(sanitize_surrogates(&text));
    }
    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => json!({
                "type": "text",
                "text": sanitize_surrogates(&text.text),
            }),
            UserContentBlock::Image(image) => convert_image_block(image),
        })
        .collect();
    let has_text = blocks
        .iter()
        .any(|block| str_of(block, "type") == Some("text"));
    if !has_text {
        blocks.insert(0, json!({ "type": "text", "text": "(see attached image)" }));
    }
    Value::Array(blocks)
}

fn convert_tool_result(message: &ToolResultMessage, content: Option<&[UserContentBlock]>) -> Value {
    let content = content.unwrap_or(&message.content);
    json!({
        "type": "tool_result",
        "tool_use_id": message.tool_call_id,
        "content": convert_content_blocks(content),
        "is_error": message.is_error,
    })
}

struct ConvertedAnthropicMessages {
    messages: Vec<Value>,
    assistant_levels: HashMap<usize, AnthropicEffort>,
}

type ConvertToolDefinitions<'a> =
    &'a dyn Fn(&[Tool], Option<&Value>) -> Result<Vec<Value>, AiError>;

#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)]
fn convert_messages(
    transformed_messages: &[Message],
    is_oauth_token: bool,
    cache_control: Option<&Value>,
    allow_empty_signature: bool,
    managed_provider: Option<&str>,
    convert_tool_definitions: Option<ConvertToolDefinitions<'_>>,
) -> Result<ConvertedAnthropicMessages, AiError> {
    let mut params: Vec<Value> = Vec::new();
    let mut assistant_levels: HashMap<usize, AnthropicEffort> = HashMap::new();
    // system 消息先暂存，在下一个**被发送的** assistant 消息之前 flush（循环结束再 flush
    // 一次）：中间被丢弃的空 assistant 消息不会让更新跑到 user 消息之前；同时 Anthropic
    // 要求 tool_result 紧跟 tool_use，放在 user 消息前的更新在线上会落到它之后。
    let mut pending_system_messages: Vec<Value> = Vec::new();

    let mut index = 0usize;
    while index < transformed_messages.len() {
        let message = &transformed_messages[index];
        match message {
            Message::System(system) => {
                let text = render_system_message_update(system);
                let mut blocks: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({
                        "type": "text",
                        "text": sanitize_surrogates(&text),
                    }));
                }
                if let Some(convert) = convert_tool_definitions {
                    let added = system.tools_added.clone().unwrap_or_default();
                    let redefined: HashSet<&str> =
                        added.iter().map(|tool| tool.name.as_str()).collect();
                    for tool in system.tools_removed.iter().flatten() {
                        if redefined.contains(tool.name.as_str()) {
                            continue;
                        }
                        blocks.push(json!({
                            "type": "tool_removal",
                            "tool": {
                                "type": "tool_reference",
                                "name": if is_oauth_token {
                                    claude_code_name(&tool.name)
                                } else {
                                    tool.name.clone()
                                },
                            },
                        }));
                    }
                    for definition in convert(&added, None)? {
                        blocks.push(json!({
                            "type": "tool_addition",
                            "tool": { "type": "tool_definition", "definition": definition },
                        }));
                    }
                }
                if !blocks.is_empty() {
                    pending_system_messages.push(json!({
                        "role": "system",
                        "content": blocks,
                    }));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    if !js_trim_empty(text) {
                        params.push(json!({
                            "role": "user",
                            "content": sanitize_surrogates(text),
                        }));
                    }
                }
                UserContent::Blocks(content) => {
                    let blocks: Vec<Value> = content
                        .iter()
                        .map(|block| match block {
                            UserContentBlock::Text(text) => json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }),
                            UserContentBlock::Image(image) => convert_image_block(image),
                        })
                        .collect();
                    let filtered: Vec<Value> = blocks
                        .into_iter()
                        .filter(|block| {
                            if str_of(block, "type") == Some("text") {
                                let text = str_of(block, "text").unwrap_or_default();
                                !js_trim_empty(text)
                            } else {
                                true
                            }
                        })
                        .collect();
                    if filtered.is_empty() {
                        index += 1;
                        continue;
                    }
                    params.push(json!({ "role": "user", "content": filtered }));
                }
            },
            Message::Assistant(assistant) => {
                let mut blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            if js_trim_empty(&text.text) {
                                continue;
                            }
                            blocks.push(json!({
                                "type": "text",
                                "text": sanitize_surrogates(&text.text),
                            }));
                        }
                        AssistantContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                let mut item = Map::new();
                                item.insert(
                                    "type".into(),
                                    Value::String("redacted_thinking".into()),
                                );
                                print_opt_string(
                                    &mut item,
                                    "data",
                                    thinking.thinking_signature.clone(),
                                );
                                blocks.push(Value::Object(item));
                                continue;
                            }
                            let signature = thinking.thinking_signature.as_deref();
                            let has_signature =
                                signature.is_some_and(|signature| !js_trim_empty(signature));
                            if js_trim_empty(&thinking.thinking) && !has_signature {
                                continue;
                            }
                            if !has_signature {
                                if allow_empty_signature {
                                    blocks.push(json!({
                                        "type": "thinking",
                                        "thinking": sanitize_surrogates(&thinking.thinking),
                                        "signature": "",
                                    }));
                                } else {
                                    blocks.push(json!({
                                        "type": "text",
                                        "text": sanitize_surrogates(&thinking.thinking),
                                    }));
                                }
                            } else {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": sanitize_surrogates(&thinking.thinking),
                                    "signature": signature.unwrap_or_default(),
                                }));
                            }
                        }
                        AssistantContentBlock::ToolCall(tool_call) => {
                            let name = if is_oauth_token {
                                claude_code_name(&tool_call.name)
                            } else {
                                tool_call.name.clone()
                            };
                            let mut item = Map::new();
                            item.insert("type".into(), Value::String("tool_use".into()));
                            item.insert("id".into(), Value::String(tool_call.id.clone()));
                            item.insert("name".into(), Value::String(name));
                            item.insert(
                                "input".into(),
                                Value::Object(
                                    tool_call
                                        .arguments
                                        .iter()
                                        .map(|(key, value)| (key.clone(), value.clone()))
                                        .collect(),
                                ),
                            );
                            blocks.push(Value::Object(item));
                        }
                    }
                }
                if blocks.is_empty() {
                    index += 1;
                    continue;
                }
                params.append(&mut pending_system_messages);
                let message_index = params.len();
                params.push(json!({ "role": "assistant", "content": blocks }));
                if let Some(managed_provider) = managed_provider
                    && assistant.api == "anthropic-messages"
                    && assistant.provider == managed_provider
                    && is_anthropic_effort(assistant.provider_thinking_level.as_deref())
                    && let Some(level) = assistant.provider_thinking_level.clone()
                {
                    assistant_levels.insert(message_index, level);
                }
            }
            // 连续的 tool_result 合并进同一条 user 消息（TS 用 while 循环打包）；带图片的
            // 错误结果拆成 text 说明 + 图片块，追加在该 user 消息末尾。
            Message::ToolResult(_) => {
                let mut tool_results: Vec<Value> = Vec::new();
                let mut error_images: Vec<Value> = Vec::new();
                let mut cursor = index;
                while cursor < transformed_messages.len()
                    && let Message::ToolResult(tool_message) = &transformed_messages[cursor]
                {
                    let split = split_error_tool_result_images(tool_message);
                    match &split {
                        Some(split) => {
                            let split_blocks: Vec<UserContentBlock> = split
                                .content
                                .iter()
                                .cloned()
                                .map(UserContentBlock::Text)
                                .collect();
                            tool_results
                                .push(convert_tool_result(tool_message, Some(&split_blocks)));
                            error_images.push(json!({ "type": "text", "text": split.caption }));
                            for image in &split.images {
                                error_images.push(convert_image_block(image));
                            }
                        }
                        None => tool_results.push(convert_tool_result(tool_message, None)),
                    }
                    cursor += 1;
                }
                index = cursor.saturating_sub(1);
                let mut content = tool_results;
                content.extend(error_images);
                params.push(json!({ "role": "user", "content": content }));
            }
        }
        index += 1;
    }
    params.append(&mut pending_system_messages);

    // 只给最后一条 user/system 消息的最后一个可缓存块挂 cache_control；assistant 结尾不挂，
    // 且 tool_use/thinking 等类型不在允许列表内。
    if let Some(cache_control) = cache_control
        && let Some(last_message) = params.last_mut()
    {
        let role = str_of(last_message, "role").unwrap_or_default();
        if role == "user" || role == "system" {
            match obj(last_message, "content").cloned() {
                Some(Value::Array(mut blocks)) => {
                    if let Some(last_block) = blocks.last_mut() {
                        let kind = str_of(last_block, "type").unwrap_or_default();
                        if [
                            "text",
                            "image",
                            "tool_result",
                            "tool_addition",
                            "tool_removal",
                        ]
                        .contains(&kind)
                            && let Value::Object(map) = last_block
                        {
                            map.insert("cache_control".into(), cache_control.clone());
                        }
                    }
                    if let Value::Object(map) = last_message {
                        map.insert("content".into(), Value::Array(blocks));
                    }
                }
                Some(Value::String(text)) => {
                    if let Value::Object(map) = last_message {
                        map.insert(
                            "content".into(),
                            Value::Array(vec![json!({
                                "type": "text",
                                "text": text,
                                "cache_control": cache_control,
                            })]),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    Ok(ConvertedAnthropicMessages {
        messages: params,
        assistant_levels,
    })
}

fn print_opt_string(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_string(), Value::String(value));
    }
}

/// 在历史 assistant 消息前插入 effort 标记，并在末尾追加当前 effort 标记。
///
/// 每条标记是 `{ role: "system", content: [], output_config: { effort } }`。`assistant_levels`
/// 的键是 `converted.messages` 的下标（不是插入后的下标），因此先查后插不会错位；末尾标记
/// 无条件追加，表示本次请求的生效 effort。
fn insert_thinking_level_messages(
    converted: &ConvertedAnthropicMessages,
    active_effort: &str,
) -> Vec<Value> {
    let mut messages = Vec::with_capacity(converted.messages.len() + 1);
    for (index, message) in converted.messages.iter().enumerate() {
        if let Some(level) = converted.assistant_levels.get(&index) {
            messages.push(json!({
                "role": "system",
                "content": [],
                "output_config": { "effort": level },
            }));
        }
        messages.push(message.clone());
    }
    messages.push(json!({
        "role": "system",
        "content": [],
        "output_config": { "effort": active_effort },
    }));
    messages
}

#[allow(clippy::result_large_err)]
fn convert_tools(
    tools: &[Tool],
    is_oauth_token: bool,
    supports_eager_tool_input_streaming: bool,
    supports_strict_tools: bool,
    cache_control: Option<&Value>,
    disable_preferred_strict_tools: bool,
) -> Result<Vec<Value>, AiError> {
    let mut result = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let preferred = is_preferred_strict_sampling(tool);
        let strict = if disable_preferred_strict_tools && preferred {
            None
        } else {
            let check: &UnsupportedStrictSchemaKeywordCheck =
                &|key, value| is_anthropic_strict_unsupported_keyword(key, value);
            resolve_json_schema_strict_sampling(tool, supports_strict_tools, Some(check))?
        };
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        let schema_properties = obj(&parameters, "properties")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let schema_required = obj(&parameters, "required")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let legacy_input_schema = json!({
            "type": "object",
            "properties": schema_properties,
            "required": schema_required,
        });
        // strict 模式下 input_schema 是“完整参数 + 强制的 type/properties/required”：
        // Anthropic 的 strict schema 要求这三个键存在，而 constrained sampling 产出的参数
        // 可能缺省它们。非 strict 模式只发最小的 legacy schema。
        let input_schema = if strict == Some(true) {
            let mut schema = match parameters {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            schema.insert("type".into(), Value::String("object".into()));
            schema.insert(
                "properties".into(),
                obj(&legacy_input_schema, "properties")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new())),
            );
            schema.insert(
                "required".into(),
                obj(&legacy_input_schema, "required")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            );
            Value::Object(schema)
        } else {
            legacy_input_schema
        };
        let mut item = Map::new();
        item.insert(
            "name".into(),
            Value::String(if is_oauth_token {
                claude_code_name(&tool.name)
            } else {
                tool.name.clone()
            }),
        );
        item.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        if supports_eager_tool_input_streaming {
            item.insert("eager_input_streaming".into(), Value::Bool(true));
        }
        if strict == Some(true) {
            item.insert("strict".into(), Value::Bool(true));
        }
        item.insert("input_schema".into(), input_schema);
        if let Some(cache_control) = cache_control
            && index == tools.len() - 1
        {
            item.insert("cache_control".into(), cache_control.clone());
        }
        result.push(Value::Object(item));
    }
    Ok(result)
}

/// 没有 eager input streaming 且当前有工具时，需要 fine-grained-tool-streaming beta。
fn should_use_fine_grained_tool_streaming_beta(model: &Model, context: &TranscriptContext) -> bool {
    !get_current_tools(&context.0.messages).is_empty()
        && !get_anthropic_compat(model).supports_eager_tool_input_streaming
}

fn get_beta_features(
    model: &Model,
    context: &TranscriptContext,
    is_oauth_token: bool,
    native_tool_changes: bool,
    options: &AnthropicOptions,
) -> Vec<String> {
    // 显式配置的 `anthropic-beta` 头会完全替换下面的自动列表（不是合并）；model.headers
    // 先读，options.headers 后读并覆盖。options.headers 中该头为 null 时返回空列表。
    // `configuredFeatures`: undefined / null / string。
    let mut configured: Option<Option<String>> = None;
    if let Some(headers) = &model.headers {
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("anthropic-beta") {
                configured = Some(Some(value.clone()));
            }
        }
    }
    if let Some(headers) = &options.base.headers {
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("anthropic-beta") {
                configured = Some(value.clone());
            }
        }
    }
    match configured {
        Some(None) => return Vec::new(),
        Some(Some(value)) => {
            let mut features: Vec<String> = Vec::new();
            for feature in value.split(',') {
                let feature = kiana_json::js_string::js_trim(feature);
                if !feature.is_empty() && !features.iter().any(|existing| existing == feature) {
                    features.push(feature.to_string());
                }
            }
            return features;
        }
        None => {}
    }

    let mut features: Vec<String> = Vec::new();
    if is_oauth_token {
        features.push("claude-code-20250219".into());
        features.push("oauth-2025-04-20".into());
    }
    if should_use_fine_grained_tool_streaming_beta(model, context) {
        features.push(FINE_GRAINED_TOOL_STREAMING_BETA.into());
    }
    if model.reasoning == Some(true)
        && options.thinking_enabled == Some(true)
        && options.interleaved_thinking != Some(false)
        && !force_adaptive_thinking(model)
    {
        features.push(INTERLEAVED_THINKING_BETA.into());
    }
    if should_use_server_side_fallback_beta(model) {
        features.push(SERVER_SIDE_FALLBACK_BETA.into());
    }
    if supports_mid_convo_effort(model) {
        features.push(MID_CONVERSATION_OUTPUT_CONFIG_BETA.into());
        features.push(THINKING_BINDING_CONTROLS_BETA.into());
    }
    if native_tool_changes {
        features.push(INLINE_TOOLS_BETA.into());
    }
    let mut deduped: Vec<String> = Vec::new();
    for feature in features {
        if !deduped.contains(&feature) {
            deduped.push(feature);
        }
    }
    deduped
}

#[allow(clippy::result_large_err)]
fn build_params(
    model: &Model,
    context: &TranscriptContext,
    is_oauth_token: bool,
    options: &AnthropicOptions,
    disable_preferred_strict_tools: bool,
) -> Result<BuiltParams, AiError> {
    // 阶段 1：解析缓存策略并拆出开头的 system 消息。若上下文以 system 开头，它单独走
    // `system` 字段（即 transformed 的第 0 条），其余消息进入 messages；否则全部进入 messages。
    let (_, cache_control) = get_cache_control(options, model);
    let cache_control = cache_control.as_ref();
    let compat = get_anthropic_compat(model);
    let initial_system_message = get_initial_system_message(&context.0.messages).cloned();
    let initial_system_text = initial_system_message
        .as_ref()
        .map(get_system_message_text)
        .unwrap_or_default();
    // 工具调用 ID 先按 Anthropic 的字符集与长度限制归一化（复刻 TS 正则的 UTF-16 语义）。
    let transformed_messages = transform_messages_with(
        &context.0.messages,
        model,
        Some(&|id, _model, _source| normalize_tool_call_id(id)),
    );
    let conversation_messages = if initial_system_message.is_some() {
        transformed_messages
            .get(1..)
            .map(<[Message]>::to_vec)
            .unwrap_or_default()
    } else {
        transformed_messages.clone()
    };

    // 阶段 2：工具转换闭包。除了生成 wire 定义，还记录“是否真的发出了 strict:prefer”，
    // 这是 strict 工具降级重试的唯一触发条件。
    let has_preferred_strict_tools = Cell::new(false);
    let convert_tool_definitions =
        |tools: &[Tool], tool_cache_control: Option<&Value>| -> Result<Vec<Value>, AiError> {
            let definitions = convert_tools(
                tools,
                is_oauth_token,
                compat.supports_eager_tool_input_streaming,
                compat.supports_strict_tools,
                tool_cache_control,
                disable_preferred_strict_tools,
            )?;
            for (index, definition) in definitions.iter().enumerate() {
                let Some(tool) = tools.get(index) else {
                    continue;
                };
                let preferred = is_preferred_strict_sampling(tool);
                if obj(definition, "strict") == Some(&Value::Bool(true)) && preferred {
                    has_preferred_strict_tools.set(true);
                }
            }
            Ok(definitions)
        };

    let initial_tools = initial_system_message
        .as_ref()
        .and_then(|message| message.tools_added.clone())
        .unwrap_or_default();
    let native_tool_changes = compat.supports_mid_convo_system_messages
        && compat.supports_mid_convo_tool_changes
        && !initial_tools.is_empty();
    let thinking_disabled = model.reasoning == Some(true)
        && options.thinking_enabled == Some(false)
        && !matches!(
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off)),
            Some(None)
        );
    let effort_messages = supports_mid_convo_effort(model) && !thinking_disabled;
    let converted = convert_messages(
        &conversation_messages,
        is_oauth_token,
        cache_control,
        compat.allow_empty_signature,
        effort_messages.then_some(model.provider.as_str()),
        native_tool_changes.then_some(&convert_tool_definitions as ConvertToolDefinitions<'_>),
    )?;
    let active_effort = options.effort.clone().unwrap_or_else(|| "high".to_string());
    let beta_features =
        get_beta_features(model, context, is_oauth_token, native_tool_changes, options);

    // 阶段 3：按 TS 对象字面量的顺序拼 params。Map 的插入顺序就是 JSON stringify 顺序，
    // 下游若有逐字节断言会观察到这里。
    let mut params = Map::new();
    push(&mut params, "model", Value::String(model.id.clone()));
    push(
        &mut params,
        "messages",
        Value::Array(if effort_messages {
            insert_thinking_level_messages(&converted, &active_effort)
        } else {
            converted.messages.clone()
        }),
    );
    if options.force_null_max_tokens {
        // TS 的数值路径产生 NaN，`JSON.stringify` 写作 null。
        push(&mut params, "max_tokens", Value::Null);
    } else if let Some(max_tokens) = options.base.max_tokens.or(model.max_tokens) {
        push(&mut params, "max_tokens", json!(max_tokens));
    }
    push(&mut params, "stream", Value::Bool(true));
    if !beta_features.is_empty() {
        push(
            &mut params,
            "betas",
            Value::Array(beta_features.iter().cloned().map(Value::String).collect()),
        );
    }

    if is_oauth_token {
        let mut identity_block = Map::new();
        identity_block.insert("type".into(), Value::String("text".into()));
        identity_block.insert("text".into(), Value::String(CLAUDE_CODE_IDENTITY.into()));
        if let Some(cache_control) = cache_control {
            identity_block.insert("cache_control".into(), cache_control.clone());
        }
        let mut system = vec![Value::Object(identity_block)];
        if !initial_system_text.is_empty() {
            let mut block = Map::new();
            block.insert("type".into(), Value::String("text".into()));
            block.insert(
                "text".into(),
                Value::String(sanitize_surrogates(&initial_system_text)),
            );
            if let Some(cache_control) = cache_control {
                block.insert("cache_control".into(), cache_control.clone());
            }
            system.push(Value::Object(block));
        }
        push(&mut params, "system", Value::Array(system));
    } else if !initial_system_text.is_empty() {
        let mut block = Map::new();
        block.insert("type".into(), Value::String("text".into()));
        block.insert(
            "text".into(),
            Value::String(sanitize_surrogates(&initial_system_text)),
        );
        if let Some(cache_control) = cache_control {
            block.insert("cache_control".into(), cache_control.clone());
        }
        push(
            &mut params,
            "system",
            Value::Array(vec![Value::Object(block)]),
        );
    }

    if let Some(temperature) = options.base.temperature
        && options.thinking_enabled != Some(true)
        && !supports_mid_convo_effort(model)
        && compat.supports_temperature
    {
        push(&mut params, "temperature", json!(temperature));
    }

    let tool_cache_control = compat
        .supports_cache_control_on_tools
        .then_some(cache_control)
        .flatten();
    // 原生工具变更模式下 tools 只发初始工具（后续增删走 system 消息里的 tool_addition/
    // tool_removal），并追加占位工具；否则发当前完整工具列表。
    if native_tool_changes {
        let mut tools = convert_tool_definitions(&initial_tools, tool_cache_control)?;
        tools.push(deferred_tool_placeholder());
        push(&mut params, "tools", Value::Array(tools));
    } else {
        let tools = get_current_tools(&context.0.messages);
        if !tools.is_empty() {
            push(
                &mut params,
                "tools",
                Value::Array(convert_tool_definitions(&tools, tool_cache_control)?),
            );
        }
    }

    // thinking 的配置分支互斥，按 TS 优先级判定：disabled > mid-convo adaptive >
    // 普通 enabled/adaptive；其中 mid-convo 与 force-adaptive 分支会同时发送 thinking 与
    // output_config。
    if thinking_disabled {
        push(&mut params, "thinking", json!({ "type": "disabled" }));
    } else if effort_messages {
        push(
            &mut params,
            "thinking",
            json!({
                "type": "adaptive",
                "display": options.thinking_display.clone().unwrap_or_else(|| "summarized".into()),
                "block_binding": { "prefix_mismatch_behavior": "drop_block" },
            }),
        );
        push(&mut params, "output_config", json!({ "effort": "low" }));
    } else if model.reasoning == Some(true) && options.thinking_enabled == Some(true) {
        let display = options
            .thinking_display
            .clone()
            .unwrap_or_else(|| "summarized".to_string());
        if force_adaptive_thinking(model) {
            push(
                &mut params,
                "thinking",
                json!({ "type": "adaptive", "display": display }),
            );
            if let Some(effort) = &options.effort {
                push(&mut params, "output_config", json!({ "effort": effort }));
            }
        } else {
            push(
                &mut params,
                "thinking",
                json!({
                    "type": "enabled",
                    "budget_tokens": options.thinking_budget_tokens.filter(|budget| *budget != 0).unwrap_or(1024),
                    "display": display,
                }),
            );
        }
    }

    if let Some(metadata) = &options.base.metadata
        && let Some(user_id) = metadata.get("user_id").and_then(Value::as_str)
    {
        push(&mut params, "metadata", json!({ "user_id": user_id }));
    }

    if let Some(tool_choice) = &options.tool_choice {
        match tool_choice {
            Value::String(kind) => {
                push(&mut params, "tool_choice", json!({ "type": kind }));
            }
            Value::Object(_) => {
                push(&mut params, "tool_choice", tool_choice.clone());
            }
            _ => {}
        }
    }

    let fallbacks = allowed_fallback_models(model);
    if !fallbacks.is_empty() {
        let mapped: Vec<Value> = fallbacks
            .iter()
            .map(|fallback| json!({ "model": str_of(fallback, "model").unwrap_or_default() }))
            .collect();
        push(&mut params, "fallbacks", Value::Array(mapped));
    }

    Ok(BuiltParams {
        params: Value::Object(params),
        has_preferred_strict_tools: has_preferred_strict_tools.get(),
        provider_thinking_level: effort_messages.then_some(active_effort),
    })
}

/// 原生工具变更协议保留的占位工具（占位名与 `input_schema` 是稳定契约，改动需同步测试）。
///
/// 名字是保留名，模型不应调用；`defer_loading: true` 把它标记为服务端延迟加载的定义。
/// 只在 `native_tool_changes` 分支随 `tools` 一起发送。
fn deferred_tool_placeholder() -> Value {
    json!({
        "name": "__kiana_deferred_placeholder__",
        "description": "Reserved placeholder. Never available. Never call this.",
        "input_schema": { "type": "object", "properties": {}, "required": [] },
        "defer_loading": true,
    })
}

/// `mapStopReason(reason, stopDetails?)`。
///
/// 未知 stop reason 返回错误而不是静默忽略；`refusal`/`sensitive` 映射为 `Error` 并附错误
/// 消息（`refusal` 的 explanation 为空字符串时回落到固定文案）；`pause_turn`/`stop_sequence`
/// 视为正常 `Stop`。
#[allow(clippy::result_large_err)]
fn map_stop_reason(
    reason: &str,
    stop_explanation: Option<&str>,
) -> Result<(StopReason, Option<String>), AiError> {
    Ok(match reason {
        "end_turn" => (StopReason::Stop, None),
        "max_tokens" => (StopReason::Length, None),
        "tool_use" => (StopReason::ToolUse, None),
        "refusal" => (
            StopReason::Error,
            Some(
                stop_explanation
                    .filter(|explanation| !explanation.is_empty())
                    .unwrap_or("The model refused to complete the request")
                    .to_string(),
            ),
        ),
        "pause_turn" => (StopReason::Stop, None),
        "stop_sequence" => (StopReason::Stop, None),
        "sensitive" => (
            StopReason::Error,
            Some("Provider stopped with: sensitive".to_string()),
        ),
        other => {
            return Err(AiError::new(
                AiErrorKind::Other,
                format!("Unhandled stop reason: {other}"),
            ));
        }
    })
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_code_tool_names_are_normalized() {
        assert_eq!(claude_code_name("read"), "Read");
        assert_eq!(claude_code_name("webfetch"), "WebFetch");
        assert_eq!(claude_code_name("myTool"), "myTool");
        let tools = vec![Tool {
            name: "Read".into(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
            constrained_sampling: None,
        }];
        assert_eq!(from_claude_code_name("read", &tools), "Read");
        assert_eq!(from_claude_code_name("other", &tools), "other");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn cache_control_respects_retention_and_compat() {
        let model = Model {
            provider: "anthropic".into(),
            ..Model::default()
        };
        let mut options = AnthropicOptions::default();
        let (retention, control) = get_cache_control(&options, &model);
        assert_eq!(retention, CacheRetention::Short);
        assert_eq!(control, Some(json!({ "type": "ephemeral" })));
        options.base.cache_retention = Some(CacheRetention::Long);
        let (retention, control) = get_cache_control(&options, &model);
        assert_eq!(retention, CacheRetention::Long);
        assert_eq!(control, Some(json!({ "type": "ephemeral", "ttl": "1h" })));
        let mut model = model;
        model.compat = Some(json!({ "supportsLongCacheRetention": false }));
        let (_, control) = get_cache_control(&options, &model);
        assert_eq!(control, Some(json!({ "type": "ephemeral" })));
        options.base.cache_retention = Some(CacheRetention::None);
        assert_eq!(
            get_cache_control(&options, &model),
            (CacheRetention::None, None)
        );
    }

    #[test]
    fn tool_call_ids_normalize_like_the_ts_regex() {
        assert_eq!(normalize_tool_call_id("a.b/c"), "a_b_c");
        assert_eq!(normalize_tool_call_id(&"x".repeat(80)), "x".repeat(64));
    }

    #[test]
    fn stop_reasons_match_the_table() {
        assert_eq!(
            map_stop_reason("end_turn", None).unwrap().0,
            StopReason::Stop
        );
        assert_eq!(
            map_stop_reason("max_tokens", None).unwrap().0,
            StopReason::Length
        );
        assert_eq!(
            map_stop_reason("tool_use", None).unwrap().0,
            StopReason::ToolUse
        );
        assert_eq!(
            map_stop_reason("pause_turn", None).unwrap().0,
            StopReason::Stop
        );
        assert_eq!(
            map_stop_reason("stop_sequence", None).unwrap().0,
            StopReason::Stop
        );
        assert_eq!(
            map_stop_reason("refusal", None).unwrap().1.as_deref(),
            Some("The model refused to complete the request")
        );
        assert_eq!(
            map_stop_reason("refusal", Some("explained"))
                .unwrap()
                .1
                .as_deref(),
            Some("explained")
        );
        assert_eq!(
            map_stop_reason("sensitive", None).unwrap().1.as_deref(),
            Some("Provider stopped with: sensitive")
        );
        assert_eq!(
            map_stop_reason("nope", None).unwrap_err().message,
            "Unhandled stop reason: nope"
        );
    }

    #[test]
    fn thinking_effort_mapping() {
        let mut model = Model::default();
        assert_eq!(
            map_thinking_level_to_effort(&model, Some(ThinkingLevel::Minimal)),
            "low"
        );
        assert_eq!(
            map_thinking_level_to_effort(&model, Some(ThinkingLevel::Low)),
            "low"
        );
        assert_eq!(
            map_thinking_level_to_effort(&model, Some(ThinkingLevel::Medium)),
            "medium"
        );
        assert_eq!(
            map_thinking_level_to_effort(&model, Some(ThinkingLevel::High)),
            "high"
        );
        assert_eq!(
            map_thinking_level_to_effort(&model, Some(ThinkingLevel::Xhigh)),
            "high"
        );
        assert_eq!(
            map_thinking_level_to_effort(&model, Some(ThinkingLevel::Max)),
            "high"
        );
        model.thinking_level_map = Some(indexmap::IndexMap::from([(
            ModelThinkingLevel::Xhigh,
            Some("xhigh".into()),
        )]));
        assert_eq!(
            map_thinking_level_to_effort(&model, Some(ThinkingLevel::Xhigh)),
            "xhigh"
        );
    }

    #[test]
    fn sse_decoder_matches_the_ts_state_machine() {
        let mut state = SseDecoderState::default();
        assert!(decode_sse_line(": comment", &mut state).is_none());
        assert!(decode_sse_line("event: message_start", &mut state).is_none());
        assert!(decode_sse_line("data: {\"a\":1}", &mut state).is_none());
        let event = decode_sse_line("", &mut state).expect("flush");
        assert_eq!(event.event.as_deref(), Some("message_start"));
        assert_eq!(event.data, "{\"a\":1}");
        assert_eq!(
            event.raw,
            vec![": comment", "event: message_start", "data: {\"a\":1}"]
        );
        // 多行 data 以 \n 连接；只去掉一个前导空格。
        let mut state = SseDecoderState::default();
        decode_sse_line("data:  x", &mut state);
        decode_sse_line("data:y", &mut state);
        let event = flush_sse_event(&mut state).expect("flush");
        assert_eq!(event.data, " x\ny");
        assert!(flush_sse_event(&mut state).is_none());
    }

    #[test]
    fn empty_event_field_keeps_state_like_typescript() {
        // TS `!state.event` 把空字符串当作“没有事件”：空行不 flush，state（含 raw）保留。
        let mut state = SseDecoderState::default();
        decode_sse_line("event: ", &mut state);
        assert_eq!(state.event.as_deref(), Some(""));
        assert!(decode_sse_line("", &mut state).is_none());
        assert_eq!(state.raw, vec!["event: "]);
        assert_eq!(state.event.as_deref(), Some(""));

        // 后续 data 行仍能 flush；空事件名原样保留在事件对象里，raw 包含之前空 event 行。
        decode_sse_line("data: x", &mut state);
        let event = decode_sse_line("", &mut state).expect("flush");
        assert_eq!(event.event.as_deref(), Some(""));
        assert_eq!(event.data, "x");
        assert_eq!(event.raw, vec!["event: ", "data: x"]);
        assert!(flush_sse_event(&mut state).is_none());
    }

    #[test]
    fn crlf_and_lone_cr_line_endings() {
        assert_eq!(
            consume_line("a\r\nb"),
            Some(("a".to_string(), "b".to_string()))
        );
        assert_eq!(
            consume_line("a\rb"),
            Some(("a".to_string(), "b".to_string()))
        );
        assert_eq!(consume_line("abc"), None);
    }

    #[test]
    fn utf8_decoder_keeps_incomplete_sequences_across_chunks() {
        let mut decoder = Utf8Decoder::default();
        let bytes = "é".as_bytes();
        assert_eq!(decoder.decode(&bytes[..1], true), "");
        assert_eq!(decoder.decode(&bytes[1..], true), "é");
        assert_eq!(decoder.decode(&[0xff], false), "\u{FFFD}");
    }

    #[test]
    fn strict_keyword_check_matches_anthropic_list() {
        assert!(is_anthropic_strict_unsupported_keyword(
            "minimum",
            &json!(1)
        ));
        assert!(is_anthropic_strict_unsupported_keyword(
            "maxItems",
            &json!(1)
        ));
        assert!(!is_anthropic_strict_unsupported_keyword(
            "minItems",
            &json!(0)
        ));
        assert!(!is_anthropic_strict_unsupported_keyword(
            "minItems",
            &json!(1)
        ));
        assert!(is_anthropic_strict_unsupported_keyword(
            "minItems",
            &json!(2)
        ));
        assert!(!is_anthropic_strict_unsupported_keyword(
            "format",
            &json!("date-time")
        ));
        assert!(is_anthropic_strict_unsupported_keyword(
            "format",
            &json!("regex")
        ));
        assert!(is_anthropic_strict_unsupported_keyword("format", &json!(1)));
        assert!(!is_anthropic_strict_unsupported_keyword(
            "type",
            &json!("object")
        ));
    }

    #[test]
    fn convert_tools_sets_strict_input_schema_and_cache_control() {
        let tools = vec![Tool {
            name: "edit".into(),
            description: "Edit".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
            constrained_sampling: Some(crate::types::ConstrainedSampling::Config(
                crate::types::ConstrainedSamplingConfig::JsonSchema {
                    strict: crate::types::StrictMode::Require,
                },
            )),
        }];
        let converted = convert_tools(
            &tools,
            false,
            true,
            true,
            Some(&json!({ "type": "ephemeral" })),
            false,
        )
        .expect("converts");
        assert_eq!(converted.len(), 1);
        let tool = &converted[0];
        assert_eq!(str_of(tool, "name"), Some("edit"));
        assert_eq!(obj(tool, "eager_input_streaming"), Some(&Value::Bool(true)));
        assert_eq!(obj(tool, "strict"), Some(&Value::Bool(true)));
        assert_eq!(
            obj(tool, "cache_control"),
            Some(&json!({ "type": "ephemeral" }))
        );
        assert_eq!(
            obj(tool, "input_schema").and_then(|schema| str_of(schema, "type")),
            Some("object")
        );
    }

    #[test]
    fn request_timeout_defaults_to_the_sdk_ten_minutes() {
        assert_eq!(anthropic_request_timeout_ms(None), Some(600_000));
        assert_eq!(anthropic_request_timeout_ms(Some(1234)), Some(1234));
    }

    /// `message_delta.usage` → OpenRouter accounting 的 JSON 映射必须保留 TS 的
    /// `undefined`/`null` 区分。期望值逐条对照 TS
    /// `applyOpenRouterReportedCost`（node --experimental-strip-types 跑出的结果）。
    #[test]
    fn open_router_accounting_preserves_undefined_versus_null() {
        fn apply(raw: Value) -> (f64, Option<bool>) {
            let mut usage = Usage {
                input: 1,
                output: 1,
                total_tokens: 2,
                cost: UsageCost {
                    total: 1.0,
                    total_from_provider: Some(false),
                    ..UsageCost::default()
                },
                ..Usage::default()
            };
            if let Some(accounting) = open_router_usage_accounting(&raw) {
                apply_open_router_reported_cost(&mut usage, &accounting);
            }
            (usage.cost.total, usage.cost.total_from_provider)
        }

        // 缺少 `is_byok`（undefined）：TS 的 `=== false` / `=== true` 都不成立 → usage 不变。
        assert_eq!(apply(json!({ "cost": 0.5 })), (1.0, Some(false)));
        assert_eq!(
            apply(json!({ "cost": 0.5, "is_byok": 0 })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({ "cost": 0.5, "is_byok": "true" })),
            (1.0, Some(false))
        );

        // BYOK 且 `upstream_inference_cost` 缺失（undefined）→ isValidCost 失败 → usage 不变。
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_prompt_cost": 0.6, "upstream_inference_completions_cost": 0.3 },
            })),
            (1.0, Some(false))
        );

        // BYOK 且显式 `null` 但分量缺失/无效 → usage 不变（分量不能按 0 处理）。
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": null },
            })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": null, "upstream_inference_prompt_cost": 0.6 },
            })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": null, "upstream_inference_prompt_cost": "0.6", "upstream_inference_completions_cost": 0.3 },
            })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": null, "upstream_inference_prompt_cost": -0.6, "upstream_inference_completions_cost": 0.3 },
            })),
            (1.0, Some(false))
        );

        // BYOK 且显式 `null` + 两个有效分量 → cost + 分量和。
        let (total, from_provider) = apply(json!({
            "cost": 0.5,
            "is_byok": true,
            "cost_details": { "upstream_inference_cost": null, "upstream_inference_prompt_cost": 0.6, "upstream_inference_completions_cost": 0.3 },
        }));
        assert!((total - 1.4).abs() < 1e-12, "total: {total}");
        assert_eq!(from_provider, Some(true));

        // BYOK 且总上游成本为数字 → cost + upstream。
        let (total, from_provider) = apply(json!({
            "cost": 0.5,
            "is_byok": true,
            "cost_details": { "upstream_inference_cost": 0.4 },
        }));
        assert!((total - 0.9).abs() < 1e-12, "total: {total}");
        assert_eq!(from_provider, Some(true));

        // 非 BYOK：直接用 OpenRouter 的 cost。
        assert_eq!(
            apply(json!({ "cost": 0.5, "is_byok": false })),
            (0.5, Some(true))
        );

        // cost 本身无效 / cost_details 为 null / upstream 为字符串 → usage 不变。
        assert_eq!(
            apply(json!({ "cost": null, "is_byok": false })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({ "cost": "0.5", "is_byok": false })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({ "cost": -1.0, "is_byok": false })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({ "cost": 0.5, "is_byok": true, "cost_details": null })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": "0.4", "upstream_inference_prompt_cost": 0.6, "upstream_inference_completions_cost": 0.3 },
            })),
            (1.0, Some(false))
        );

        // `is_byok` 为 null/对象、`cost_details` 为空对象/数组/数字/false、上级成本为负
        // → 全部保持 usage 不变。
        assert_eq!(
            apply(json!({ "cost": 0.5, "is_byok": null })),
            (1.0, Some(false))
        );
        assert_eq!(
            apply(json!({ "cost": 0.5, "is_byok": {} })),
            (1.0, Some(false))
        );
        for details in [json!({}), json!([]), json!(5), json!(false)] {
            assert_eq!(
                apply(json!({ "cost": 0.5, "is_byok": true, "cost_details": details })),
                (1.0, Some(false))
            );
        }
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": -0.4 },
            })),
            (1.0, Some(false))
        );

        // 数值 0 / 0 分量都是有效成本（`>= 0`）。
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": 0 },
            })),
            (0.5, Some(true))
        );
        assert_eq!(
            apply(json!({
                "cost": 0.5,
                "is_byok": true,
                "cost_details": { "upstream_inference_cost": null, "upstream_inference_prompt_cost": 0, "upstream_inference_completions_cost": 0 },
            })),
            (0.5, Some(true))
        );
        assert_eq!(
            apply(json!({ "cost": 0, "is_byok": false })),
            (0.0, Some(true))
        );
    }
}
