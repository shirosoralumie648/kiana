//! `api/openai-codex-responses.ts`：Codex Responses 适配器（SSE + WebSocket + zstd）。
//!
//! TS: `packages/ai/src/api/openai-codex-responses.ts`。
//!
//! Rust 侧的等价物：
//! - SSE：`POST resolveCodexUrl(baseUrl)`，请求体用 `zstd` level 3 压缩（失败回退未压缩），
//!   事件按 `"\n\n"` 分帧（EOF 时给残帧补 `"\n\n"`）；
//! - WebSocket：`tokio-tungstenite`，会话级连接缓存、`previous_response_id` 增量继续、
//!   idle/age 上限与 debug 统计；
//! - 账号 id 从 JWT payload 的 `https://api.openai.com/auth.chatgpt_account_id` 提取。
//!
//! 与 TS 的差异（均因 Rust 运行时不同）：
//! - Bun 的 `proxy` WebSocket 选项不适用（tokio-tungstenite 不读 HTTP 代理环境变量）；
//! - `WebSocketLike.readyState` 不可观察，连接一律视为可复用（与 TS 的“运行时不给
//!   readyState 时假定可复用”一致）；
//! - 关闭连接是“发 Close 帧后丢弃”，没有 `wasClean` 之类的浏览器字段。
//!
//! # 为什么存在
//!
//! Codex 端点与通用 Responses API 的请求体、传输方式都不同：首条系统提示放在顶层
//! `instructions`（`input` 里不再重复包含它）、额外支持 `text.verbosity` 和 WebSocket，
//! 响应侧还允许用 `previous_response_id` 续传。因此这里不是薄封装，而是同时承担
//! **协议适配**（请求体、事件映射、错误文本）与**传输层**（SSE / WebSocket）两部分职责；
//! 两者共用 `openai_responses_shared` 的事件消费逻辑。
//!
//! # 执行流程
//!
//! 1. 调用方经由 [`openai_codex_responses_api`] 返回的 `ProviderStreams`，或直接调用
//!    [`stream`] / [`stream_simple`] 进入；`stream_simple` 在缺 API key 时同步返回 `Err`。
//! 2. `stream` 先用 `resolve_transcript` 归一化上下文（是否保留中途 system 消息由模型
//!    `compat.supportsMidConvoSystemMessages` 决定），再用 `spawn_forward` 把 `run_stream`
//!    派发到后台任务并**立即**返回事件流。此后的一切失败都只能表现为流上的 `Error` 事件，
//!    而不是调用点的 `Err`。
//! 3. `run_stream_inner` 组装请求（请求体、请求头、从 JWT 取出的账号 id），按 `transport`
//!    选择 WebSocket（默认 `auto`）或 SSE；WebSocket 只有**本次尝试尚未产出映射事件**时
//!    遇到传输故障才会回退 SSE（每次重试都会重置该判断，见下文 `websocket_started`）。
//! 4. 两条传输都把原始事件交给同一个 `CodexEventMapper`，再由
//!    [`process_responses_stream`] 累积成 [`AssistantMessage`] 并推送增量事件；
//!    `run_stream_inner` 在成功路径自己发 Done + end，`run_stream` 负责把 `Err` 转成
//!    Error 事件并结束流。
//!
//! # 关键状态
//!
//! - `start_emitted`（[`std::sync::atomic::AtomicBool`]）：每条输出流的 Start 事件只发一次。
//!   WebSocket 的内部重试或回退 SSE 复用同一条输出流，必须靠它保证幂等。
//! - `websocket_started`：**本次** WebSocket 尝试是否已产出映射事件。它在
//!   `run_stream_inner` 的重试循环内每次尝试都重新创建（重试会重置），只决定**本次尝试**
//!   失败时能否回退 SSE；跨尝试的 Start 幂等由 `start_emitted` 保证。它不等价于“调用方
//!   已收到内容事件”：第一个非 `Skip` 的原始事件就会置位（例如 `codex.rate_limits`，
//!   最终被 `process_responses_stream` 的 `_ => {}` 忽略）；而重试后的新连接若在首个
//!   事件前关闭，仍会回退 SSE。
//! - `end_turn`：`CodexEventMapper` 从终态事件里取出，经 `Arc<Mutex<_>>` 回填到
//!   [`AssistantMessage::end_turn`]（`process_responses_stream` 不处理这个字段）。
//!
//! # 并发与全局状态
//!
//! 连接缓存、debug 统计、SSE 回退标记都是进程级 `static`（见“WebSocket 支持”小节），
//! 不随适配器实例创建；因此 `lazy_api` 每次请求新建的适配器实例不会丢失复用状态。
//! 三者都用 `std::sync::Mutex` 保护，临界区内**不做任何 await**；唯一跨 await 持有的锁是
//! `process_websocket_stream` 里包住事件源的 `tokio::sync::Mutex`。
//!
//! # 错误传播
//!
//! - 传输层错误：`AiErrorKind::Transport`；协议/JSON 解析错误：`CodexProtocolError`
//!   （`AiErrorKind::Parse`）；服务端明确返回的错误事件：`CodexApiError`
//!   （`AiErrorKind::Other`）。
//! - `is_codex_non_transport_error` 用 `AiError.name` 区分“不要回退 SSE”的错误；改这三个
//!   名字会直接改变回退策略。
//! - 最终错误文本写入 `AssistantMessage.error_message`，由 `format_provider_error` 格式化；
//!   Codex 传 `None` 前缀（与 `openai-responses` 的 `<provider> API error` 不同）。
//!
//! # 阅读顺序
//!
//! `stream` → `run_stream` → `run_stream_inner`（传输选择与重试）→ `build_request_body`
//! → `process_stream` / `process_websocket_stream` → `CodexEventMapper`；理解连接复用从
//! `acquire_websocket` / `release_websocket` / `get_cached_websocket_input_delta` 入手。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::lazy::{lazy_api, lazy_stream_error};
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, OpenAIResponsesStreamOptions,
    convert_responses_messages, convert_responses_tools, get_responses_tool_support,
    process_responses_stream,
};
use crate::api::simple_options::build_base_options;
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::{HttpError, HttpRequest, SharedHttpClient};
use crate::models::clamp_thinking_level;
use crate::models::provider::ProviderStreams;
use crate::session_resources::register_session_resource_cleanup;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, DoneReason, ErrorReason,
    JsonObject, Message, Model, ModelThinkingLevel, ProviderHeaders, SimpleStreamOptions,
    StopReason, StreamOptions, ToolChoice, TranscriptContext, Usage, UsageCost,
};
use crate::utils::diagnostics::{
    ThrownValue, append_assistant_message_diagnostic, create_assistant_message_diagnostic,
    format_thrown_value,
};
use crate::utils::error_body::{
    ProviderErrorValue, format_provider_error, normalize_provider_error,
};
use crate::utils::headers::headers_to_record;
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::text::get_system_message_text;
use crate::utils::transcript::{
    get_declared_tools, get_initial_system_message, normalize_context, resolve_transcript,
    resolve_transcript_tools,
};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

// 与 TS `openai-codex-responses.ts` 的顶层常量一一对应，数值直接参与协议与重试行为
// （例如 55 分钟的连接年龄上限、5 分钟的空闲回收、zstd level 3）。改动前对照
// `spec/ai-api-anthropic-openai.md` §2.14.1。
const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const DEFAULT_MAX_RETRIES: u32 = 0;
const BASE_DELAY_MS: f64 = 1000.0;
const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;
const REQUEST_COMPRESSION_ZSTD_LEVEL: i32 = 3;
const CODEX_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
const WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE: u16 = 1009;
const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str = "websocket_connection_limit_reached";
const PREVIOUS_RESPONSE_NOT_FOUND_CODE: &str = "previous_response_not_found";
const OPENAI_BETA_RESPONSES_WEBSOCKETS: &str = "responses_websockets=2026-02-06";
const SESSION_WEBSOCKET_CACHE_TTL_MS: u64 = 5 * 60 * 1000;
const SESSION_WEBSOCKET_MAX_AGE_MS: i64 = 55 * 60 * 1000;
const CODEX_RESPONSE_STATUSES: [&str; 6] = [
    "completed",
    "incomplete",
    "failed",
    "cancelled",
    "queued",
    "in_progress",
];

/// `OpenAICodexResponsesOptions`（TS 接口的 Rust 形态）。
///
/// `base` 之外的字段也都能由 [`OpenAICodexResponsesOptions::from_stream_options`] 从
/// [`StreamOptions::extra`] 的同名 TS 字段解出，并由
/// [`OpenAICodexResponsesOptions::into_stream_options`] 写回。
#[derive(Default)]
pub struct OpenAICodexResponsesOptions {
    /// 全部 [`StreamOptions`] 字段（API key、headers、取消令牌、`fetch` 注入等）。
    pub base: StreamOptions,
    /// `reasoningEffort`：保留 TS 的字符串原样（`"none" | "minimal" | ...`），
    /// `build_request_body` 再按模型的 `thinking_level_map` 映射；未知值原样发给服务端。
    pub reasoning_effort: Option<String>,
    /// `reasoningSummary`：仅在显式 `reasoningEffort` 生效时写进请求体；此时
    /// `reasoning.summary` 缺省或显式 null 都会回落到 `"auto"`。没有显式 effort 的推理
    /// 模型走 `build_request_body` 的兜底分支，只写 `reasoning.effort`，本字段被忽略。
    pub reasoning_summary: Option<String>,
    /// `serviceTier`：`"flex"` / `"priority"` 等；同时影响请求体与费用倍率
    /// （见 `get_service_tier_cost_multiplier`）。
    pub service_tier: Option<String>,
    /// `textVerbosity`：缺省或空串都回落到 `"low"`（TS 的真值判断）。
    pub text_verbosity: Option<String>,
    /// `"auto" | "none" | "required"` 或 SimpleStreamOptions 的 `{ type }` 对象。
    /// 缺省时请求体写入字符串 `"auto"`。
    pub tool_choice: Option<Value>,
}

impl OpenAICodexResponsesOptions {
    /// 从冻结的 [`StreamOptions`] 解析 API 专属字段（`extra` 里的 TS 字段名）。
    ///
    /// 只读取本适配器认识的键，其它键原样留在 `base.extra` 里；由于整个 `base` 被保留，
    /// `from_stream_options` → `into_stream_options` 对未知键也是无损往返。
    pub fn from_stream_options(base: StreamOptions) -> Self {
        let extra = &base.extra;
        Self {
            reasoning_effort: extra
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .map(str::to_string),
            reasoning_summary: extra
                .get("reasoningSummary")
                .and_then(Value::as_str)
                .map(str::to_string),
            service_tier: extra
                .get("serviceTier")
                .and_then(Value::as_str)
                .map(str::to_string),
            text_verbosity: extra
                .get("textVerbosity")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_choice: extra.get("toolChoice").cloned(),
            base,
        }
    }

    /// 写回冻结形态。
    ///
    /// 只覆盖 `Some` 字段：已有同名 `extra` 键会被替换，缺省字段不会删除旧键。
    pub fn into_stream_options(mut self) -> StreamOptions {
        if let Some(value) = self.reasoning_effort {
            self.base
                .extra
                .insert("reasoningEffort".into(), Value::String(value));
        }
        if let Some(value) = self.reasoning_summary {
            self.base
                .extra
                .insert("reasoningSummary".into(), Value::String(value));
        }
        if let Some(value) = self.service_tier {
            self.base
                .extra
                .insert("serviceTier".into(), Value::String(value));
        }
        if let Some(value) = self.text_verbosity {
            self.base
                .extra
                .insert("textVerbosity".into(), Value::String(value));
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

// 这里的取值/格式化都刻意走 `kiana_json` 的 JS 等价实现：`is_js_trim_empty` 与
// `js_number_string` 分别复刻 `String.prototype.trim` 与 JS 的数字文本，避免 Rust 标准库
// 的空白集合/浮点格式差异改变发往服务端的内容或错误消息。
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

fn is_js_trim_empty(text: &str) -> bool {
    kiana_json::js_string::js_trim(text).is_empty()
}

fn js_number_string(value: f64) -> String {
    kiana_json::js_num::format_js_number(value)
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

// ---------------------------------------------------------------------------
// 重试辅助
// ---------------------------------------------------------------------------

// 重试判定与 TS 逐条对应（spec §2.14.3）：
// - 429 且正文命中“终态用量限制”正则时判为不可重试（TS 的终态分类）；
// - 其余 429 / 500 / 502 / 503 / 504，或正文命中限流/网关连接正则，视为可重试；
// - 服务端给的 retry-after 优先于指数退避（ms → 秒 → HTTP 日期）；
// - 请求的延迟超过 max_retry_delay_ms（默认 60s）时直接报错，而不是截断等待。

/// 命中即视为“重试也不会恢复”的用量/账单错误（大小写不敏感）。
fn is_terminal_rate_limit_error(error_text: &str) -> bool {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                "(?i)GoUsageLimitError|FreeUsageLimitError|Monthly usage limit reached|available balance|insufficient_quota|out of budget|quota exceeded|billing",
            )
            .expect("valid regex")
        })
        .is_match(error_text)
}

/// 结合状态码与响应正文判断是否可重试（不考虑剩余尝试次数）。
fn is_retryable_error(status: u16, error_text: &str) -> bool {
    if status == 429 && is_terminal_rate_limit_error(error_text) {
        return false;
    }
    if matches!(status, 429 | 500 | 502 | 503 | 504) {
        return true;
    }
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                "(?i)rate.?limit|overloaded|service.?unavailable|upstream.?connect|connection.?refused",
            )
            .expect("valid regex")
        })
        .is_match(error_text)
}

/// 读取服务端建议的重试延迟：`retry-after-ms` 优先，其次 `retry-after`（秒数或 HTTP
/// 日期）；非法值返回 `None`，由调用方退回指数退避。负值被钳到 0。
fn get_retry_after_delay_ms(headers: &IndexMap<String, String>) -> Option<f64> {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    if let Some(retry_after_ms) = get("retry-after-ms") {
        let millis = kiana_json::js_num::parse_js_number(&retry_after_ms);
        if millis.is_finite() {
            return Some(millis.max(0.0));
        }
    }
    let retry_after = get("retry-after").filter(|value| !value.is_empty())?;
    let seconds = kiana_json::js_num::parse_js_number(&retry_after);
    if seconds.is_finite() {
        return Some((seconds * 1000.0).max(0.0));
    }
    let date = crate::utils::provider_retry::parse_retry_after_date(&retry_after);
    if !date.is_nan() {
        return Some((date - now_ms() as f64).max(0.0));
    }
    None
}

/// `RetryDelayExceededError`（消息逐字）。
#[derive(Debug, Clone)]
struct RetryDelayExceededError {
    message: String,
}

/// 超过上限时返回 `RetryDelayExceededError`，消息里的秒数向上取整并用 JS 数字格式化。
/// `max_retry_delay_ms == 0` 表示不设上限（TS 同）。
fn validate_retry_delay_ms(
    delay_ms: f64,
    max_retry_delay_ms: Option<u64>,
) -> Result<f64, RetryDelayExceededError> {
    let max_retry_delay_ms = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max_retry_delay_ms > 0 && delay_ms > max_retry_delay_ms as f64 {
        return Err(RetryDelayExceededError {
            message: format!(
                "Server requested {}s retry delay (max: {}s)",
                js_number_string((delay_ms / 1000.0).ceil()),
                js_number_string((max_retry_delay_ms as f64 / 1000.0).ceil())
            ),
        });
    }
    Ok(delay_ms)
}

/// 可取消的 sleep：有取消令牌时用 `biased` select，取消优先于计时器；错误文本是当前稳定契约，改动需同步测试。
#[allow(clippy::result_large_err)]
async fn sleep(
    ms: f64,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), AiError> {
    let duration = std::time::Duration::from_millis(ms.max(0.0) as u64);
    match signal {
        Some(token) => {
            if token.is_cancelled() {
                return Err(AiError::aborted("Request was aborted"));
            }
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(AiError::aborted("Request was aborted")),
                _ = tokio::time::sleep(duration) => Ok(()),
            }
        }
        None => {
            tokio::time::sleep(duration).await;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// 请求压缩
// ---------------------------------------------------------------------------

/// `compressRequestBodyZstd(bodyJson)`：失败时静默回退 `None`。
///
/// 用 zstd level 3 压缩整个请求体（spec §2.14.4）。与 TS 一样，压缩失败不值得让请求失败：
/// 返回 `None` 后调用方发送未压缩的 JSON，并且不设置 `content-encoding`。
fn compress_request_body_zstd(body_json: &str) -> Option<Vec<u8>> {
    zstd::stream::encode_all(body_json.as_bytes(), REQUEST_COMPRESSION_ZSTD_LEVEL).ok()
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// `stream`：同步返回事件流。
///
/// 上下文按模型能力（`compat.supportsMidConvoSystemMessages`）归一化并 clone 一份，因此
/// 调用方之后修改自己持有的 context 不会影响本次请求。真正的请求在后台任务里执行；
/// 本函数没有 `Result`，缺 API key、JWT 解析失败、网络错误等只能以流上的 `Error` 事件
/// 出现。同步返回 `Err` 的只有模块级 [`stream_simple`]；经
/// [`openai_codex_responses_api`] 调用时（返回 `Arc<dyn ProviderStreams>`，其 `stream` /
/// `stream_simple` 都没有 `Result` 通道），缺 key 会由适配器的 `stream_simple` 用
/// `lazy_stream_error` 转换成流上的 `Error` 事件。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: OpenAICodexResponsesOptions,
) -> AssistantMessageEventStream {
    ensure_cleanup_registered();
    let stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let context = context.clone();
    let normalized_context = resolve_transcript(
        &context,
        compat_bool(&model, "supportsMidConvoSystemMessages"),
    );
    let stream_for_task = stream.clone();
    crate::api::lazy::spawn_forward(async move {
        run_stream(&model, &normalized_context, options, stream_for_task).await;
    });
    stream
}

/// `streamSimple`：缺少 apiKey 时同步返回 `Err`。
///
/// 其余步骤：先用 [`build_base_options`] 把 [`SimpleStreamOptions`] 冻结成
/// [`StreamOptions`]（采样参数、max tokens 钳制等），把 [`ToolChoice`] 变成请求体里的
/// `{ "type": ... }` 对象，再用 `clamp_thinking_level` 把思考档位钳到模型支持的范围。
/// `off` 不会写入 `reasoningEffort`；但 `build_request_body` 的兜底分支对
/// `reasoning == true` 的模型仍可能按 `thinking_level_map.off` 写入 `reasoning`
/// （缺键 → `"none"`），只有 off 显式映射为 null 时才完全省略该字段。
///
/// 返回的 `Err` 只用于缺 key；后续失败仍走事件流的 `Error` 事件。
#[allow(clippy::result_large_err)]
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    let api_key = options.base.api_key.clone();
    let Some(api_key) = api_key.filter(|key| !key.is_empty()) else {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!("No API key for provider: {}", model.provider),
        ));
    };
    let mut base = build_base_options(model, context, Some(&options), Some(&api_key));
    if let Some(tool_choice) = options.tool_choice {
        let value = match tool_choice {
            ToolChoice::Auto => json!({ "type": "auto" }),
            ToolChoice::None => json!({ "type": "none" }),
        };
        base.extra.insert("toolChoice".into(), value);
    }
    let clamped_reasoning = options
        .reasoning
        .map(|level| clamp_thinking_level(model, to_model_thinking_level(level)));
    let reasoning_effort = clamped_reasoning
        .filter(|level| *level != ModelThinkingLevel::Off)
        .map(model_thinking_level_string);
    let mut options = OpenAICodexResponsesOptions::from_stream_options(base);
    options.reasoning_effort = reasoning_effort.map(str::to_string);
    Ok(stream(model, context, options))
}

/// `openAICodexResponsesApi()`：`lazyApi(() => import("./openai-codex-responses.ts"))`。
///
/// `lazy_api` 的加载闭包在每次 `stream` / `stream_simple` 调用时才执行，新建一个零尺寸的
/// `OpenAICodexResponsesApi`（对应 TS 的动态 `import()`）；WebSocket 复用状态保存在模块级
/// 静态表里，不在适配器实例中，所以换实例不会丢连接缓存。
pub fn openai_codex_responses_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| {
            Box::pin(async { Ok(Arc::new(OpenAICodexResponsesApi) as Arc<dyn ProviderStreams>) })
        }),
        None,
    )
}

struct OpenAICodexResponsesApi;

impl ProviderStreams for OpenAICodexResponsesApi {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            OpenAICodexResponsesOptions::from_stream_options(options),
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

fn to_model_thinking_level(level: crate::types::ThinkingLevel) -> ModelThinkingLevel {
    match level {
        crate::types::ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        crate::types::ThinkingLevel::Low => ModelThinkingLevel::Low,
        crate::types::ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        crate::types::ThinkingLevel::High => ModelThinkingLevel::High,
        crate::types::ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        crate::types::ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

fn model_thinking_level_string(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

// 错误边界：`run_stream_inner` 在成功路径里自己发 Done + end 并返回 Ok；任何 `Err` 到这里
// 才被转换成用户可见的 Error 事件。出错时先就地改写 `output` 的 stop_reason/error_message，
// 再 clone 进事件，因此 Error 事件与 `result()` 看到的是同一份最终消息。
// 取消（`signal` 已触发）映射为 `Aborted`/`ErrorReason::Aborted`，其余一律 `Error`。
async fn run_stream(
    model: &Model,
    normalized_context: &TranscriptContext,
    options: OpenAICodexResponsesOptions,
    stream: AssistantMessageEventStream,
) {
    let mut output = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: "openai-codex-responses".into(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: zero_usage(),
        stop_reason: StopReason::Pending,
        timestamp: now_ms(),
        ..Default::default()
    };
    let result = run_stream_inner(model, normalized_context, &options, &stream, &mut output).await;
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
            output.error_message = Some(format_provider_error(
                &normalize_provider_error(&ProviderErrorValue::Error(
                    crate::utils::error_body::ProviderErrorFields {
                        message: error.message.clone(),
                        ..Default::default()
                    },
                )),
                None,
            ));
            let _ = stream.push(AssistantMessageEvent::Error {
                reason: if aborted {
                    ErrorReason::Aborted
                } else {
                    ErrorReason::Error
                },
                error: Box::new(output.clone()),
            });
            stream.end(output);
        }
    }
}

/// 成功路径的收尾校验：流正常结束但输出仍不完整时，在这里转成 `Err`。
///
/// - `Pending`：传输结束却没有终态事件（`"Codex stream ended without a stop reason"`）；
/// - `Error` / `Aborted`：`finalize_response` 可能已把 `failed`/`cancelled` 映射成 Error
///   （此时 `error_message` 常为空），这里把 `error_message`（缺省
///   `"An unknown error occurred"`）重新抛给 `run_stream` 统一收尾。
#[allow(clippy::result_large_err)]
fn assert_successful_output(output: &AssistantMessage) -> Result<(), AiError> {
    if output.stop_reason == StopReason::Pending {
        return Err(AiError::new(
            AiErrorKind::Other,
            "Codex stream ended without a stop reason",
        ));
    }
    if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
        return Err(AiError::new(
            AiErrorKind::Other,
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_string()),
        ));
    }
    Ok(())
}

/// 把内部 `StopReason` 映射成对外 `DoneReason`；只有 Length/ToolUse/Deferred 有专门取值，
/// 其余归为 Stop（Error/Aborted 在此之前已被 `assert_successful_output` 拦下）。
fn done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

// 一次请求的完整流程（TS `stream` 的异步部分）：
//
// 1. 前置校验：API key → JWT 账号 id → grammar 工具信息 → 请求体（`on_payload` 只调一次）。
// 2. 传输选择：`transport != sse` 且该 session 未被标记为“回退 SSE”时先试 WebSocket；
//    WebSocket 循环内部对两类可恢复错误各重试一次（见循环里的注释）。
// 3. WebSocket 在“本次尝试未产出映射事件”的传输失败时记录诊断并落到 SSE；已产出则直接失败。
// 4. SSE 分支自己处理压缩、响应头超时、重试与错误体解析，最后复用同一套事件处理。
//
// 不变量：`body` 只构造一次，WS 重试与 SSE 回退共用同一份；`start_emitted` 在两条路径
// 之间共享，保证 Start 事件至多出现一次。
#[allow(clippy::result_large_err)]
async fn run_stream_inner(
    model: &Model,
    normalized_context: &TranscriptContext,
    options: &OpenAICodexResponsesOptions,
    stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
) -> Result<(), AiError> {
    let api_key = options
        .base
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            AiError::new(
                AiErrorKind::Auth,
                format!("No API key for provider: {}", model.provider),
            )
        })?;
    let account_id = extract_account_id(&api_key)?;
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        Some(&get_declared_tools(&normalized_context.0.messages)),
        compat_bool(model, "supportsOpenAIGrammarTools") == Some(true),
    )?;
    // `cacheRetention == none` 表示本次请求不使用缓存：不写 `prompt_cache_key`，WebSocket
    // 也不做会话亲和（`ws_cache_session_id` 随之为空，连接不缓存）。
    let cache_session_id =
        if options.base.cache_retention == Some(crate::types::CacheRetention::None) {
            None
        } else {
            options.base.session_id.clone()
        };
    let codex_session_id = clamp_openai_prompt_cache_key(cache_session_id.as_deref());
    // TS 的所有 `if (cacheSessionId)` 都是**真值**判断：空串等同 `undefined`。
    let ws_cache_session_id = cache_session_id
        .as_deref()
        .filter(|session_id| !session_id.is_empty());
    let mut body = build_request_body(
        model,
        normalized_context,
        options,
        codex_session_id.as_deref(),
        &grammar_tool_input_properties,
    )?;
    if let Some(next) = options.base.apply_on_payload(body.clone(), model).await {
        body = next;
    }
    let websocket_request_id = codex_session_id
        .clone()
        .filter(|session_id| !session_id.is_empty())
        .map_or_else(uuid_v7, Ok)?;
    let mut identity_headers: Vec<(String, String)> = vec![("originator".into(), "kiana".into())];
    let identity = get_identity_headers(
        &ClientIdentityTarget::new(model.provider.clone(), model.base_url.clone()),
        options.base.identity.as_ref(),
    );
    merge_into_headers(&mut identity_headers, &identity);
    let sse_headers = build_sse_headers(
        &identity_headers,
        model.headers.as_ref(),
        options.base.headers.as_ref(),
        &account_id,
        &api_key,
        codex_session_id.as_deref(),
    );
    let websocket_headers = build_websocket_headers(
        &identity_headers,
        model.headers.as_ref(),
        options.base.headers.as_ref(),
        &account_id,
        &api_key,
        &websocket_request_id,
    );
    let body_json = kiana_json::js_object::stringify(&body);
    let http_timeout_ms = options.base.timeout_ms;
    let websocket_connect_timeout_ms = options.base.websocket_connect_timeout_ms;
    let transport = options
        .base
        .transport
        .unwrap_or(crate::types::Transport::Auto);
    let start_emitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 粘滞回退：某 session 只要发生过一次 WebSocket 传输故障，之后所有请求都会在这里
    // 跳过 WebSocket、直接走 SSE（每次仍记一次 `sse_fallbacks`）。标记只能由
    // `reset_openai_codex_websocket_debug_stats(session)` 或进程重启清除。
    // 注意：即使失败发生在本次尝试已产出映射事件之后（本次不回退），
    // `record_websocket_failure` 仍会把 session 标成粘滞——下一次请求不再尝试
    // WebSocket（spec §2.14.10）。
    let websocket_disabled_for_session = transport != crate::types::Transport::Sse
        && is_websocket_sse_fallback_active(ws_cache_session_id);
    if websocket_disabled_for_session {
        record_websocket_sse_fallback(ws_cache_session_id);
    }

    if transport != crate::types::Transport::Sse && !websocket_disabled_for_session {
        let mut retried_websocket_connection_limit = false;
        let mut retried_missing_websocket_continuation = false;
        loop {
            let websocket_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let end_turn_slot: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
            let result = process_websocket_stream(
                &resolve_codex_websocket_url(&model.base_url),
                &body,
                &websocket_headers,
                output,
                stream,
                model,
                websocket_started.clone(),
                http_timeout_ms,
                websocket_connect_timeout_ms,
                ws_cache_session_id,
                &account_id,
                &grammar_tool_input_properties,
                options,
                end_turn_slot.clone(),
                start_emitted.clone(),
            )
            .await;
            if let Some(end_turn) = *end_turn_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            {
                output.end_turn = Some(end_turn);
            }
            // 三类结局：
            // - `previous_response_not_found` 与“本次尝试产出映射事件前的
            //   websocket_connection_limit_reached”各允许一次原地重试（上次已在
            //   release 时删掉缓存条目，重试会新建连接）；
            // - 其它非传输错误（CodexApiError/CodexProtocolError）直接上抛，不回退 SSE；
            // - 纯传输错误：本次尝试未产出映射事件 → 记诊断并回退 SSE；已产出 → 原样上抛。
            // 已取消时一律上抛，避免把取消当成可恢复故障。
            match result {
                Ok(()) => {
                    if options
                        .base
                        .signal
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                    {
                        return Err(AiError::aborted("Request was aborted"));
                    }
                    assert_successful_output(output)?;
                    let _ = stream.push(AssistantMessageEvent::Done {
                        reason: done_reason(output.stop_reason),
                        message: Box::new(output.clone()),
                    });
                    stream.end(output.clone());
                    return Ok(());
                }
                Err(error) => {
                    let websocket_started =
                        websocket_started.load(std::sync::atomic::Ordering::SeqCst);
                    let aborted = options
                        .base
                        .signal
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                    let connection_limit_before_start =
                        !websocket_started && is_websocket_connection_limit_reached_error(&error);
                    let previous_response_not_found = is_previous_response_not_found_error(&error);
                    if !aborted
                        && previous_response_not_found
                        && !retried_missing_websocket_continuation
                    {
                        retried_missing_websocket_continuation = true;
                        continue;
                    }
                    if !aborted
                        && connection_limit_before_start
                        && !retried_websocket_connection_limit
                    {
                        retried_websocket_connection_limit = true;
                        continue;
                    }
                    if aborted
                        || (is_codex_non_transport_error(&error) && !connection_limit_before_start)
                    {
                        return Err(error);
                    }
                    let mut details: JsonObject = IndexMap::new();
                    details.insert(
                        "configuredTransport".to_string(),
                        Value::String(transport_literal(transport).into()),
                    );
                    if !websocket_started {
                        details
                            .insert("fallbackTransport".to_string(), Value::String("sse".into()));
                    }
                    details.insert("eventsEmitted".to_string(), Value::Bool(websocket_started));
                    details.insert(
                        "phase".to_string(),
                        Value::String(
                            if websocket_started {
                                "after_message_stream_start"
                            } else {
                                "before_message_stream_start"
                            }
                            .into(),
                        ),
                    );
                    details.insert("requestBytes".to_string(), json!(body_json.len()));
                    append_assistant_message_diagnostic(
                        output,
                        create_assistant_message_diagnostic(
                            "provider_transport_failure",
                            &ThrownValue::from_ai_error(&error),
                            Some(details),
                        ),
                    );
                    record_websocket_failure(ws_cache_session_id, &error);
                    if websocket_started {
                        return Err(error);
                    }
                    record_websocket_sse_fallback(ws_cache_session_id);
                    break;
                }
            }
        }
    }

    // SSE 分支。
    //
    // 压缩失败时发送未压缩 JSON 且不设置 `content-encoding`；重试策略复刻 TS 的两层
    // try/catch（见下方各处注释）。`max_retries` 默认 0，即默认不重试。
    let mut sse_headers = sse_headers;
    let compressed_body = compress_request_body_zstd(&body_json);
    if compressed_body.is_some() {
        set_header(&mut sse_headers, "content-encoding", "zstd");
    }
    let sse_body = compressed_body.unwrap_or_else(|| body_json.clone().into_bytes());
    let max_retries = options.base.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
    let mut response: Option<crate::http::HttpResponse> = None;
    let mut last_error: Option<AiError> = None;
    for attempt in 0..=max_retries {
        if options
            .base
            .signal
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(AiError::aborted("Request was aborted"));
        }
        let fetch = fetch_client(options.base.fetch.as_ref())?;
        let request = HttpRequest {
            method: "POST".into(),
            url: resolve_codex_url(Some(&model.base_url)),
            headers: sse_headers.clone(),
            body: Some(sse_body.clone()),
            timeout_ms: None,
        };
        let header_timeout = http_timeout_ms.filter(|timeout| *timeout > 0);
        let signal = options.base.signal.clone();
        let send = fetch.execute(request, signal.as_ref());
        let sent = match (header_timeout, &signal) {
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
                match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), send).await
                {
                    Ok(result) => result,
                    Err(_) => Err(HttpError::new(crate::http::reqwest_client::TIMEOUT_MESSAGE)),
                }
            }
            (None, _) => send.await,
        };
        let sent = match sent {
            Ok(response) => response,
            Err(error) => {
                if header_timeout.is_some()
                    && error.message == crate::http::reqwest_client::TIMEOUT_MESSAGE
                    && !options
                        .base
                        .signal
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                {
                    // TS 在「内层 try」里抛这个错误，随后被外层的重试 catch 当作普通
                    // 网络错误处理：`attempt < maxRetries` 时按指数退避重试。
                    let error = AiError::new(
                        AiErrorKind::Transport,
                        format!(
                            "Codex SSE response headers timed out after {}ms",
                            header_timeout.unwrap_or_default()
                        ),
                    );
                    if attempt < max_retries {
                        let delay = BASE_DELAY_MS * 2f64.powi(attempt as i32);
                        sleep(delay, options.base.signal.as_ref()).await?;
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
                if error.message == crate::http::reqwest_client::ABORTED_MESSAGE {
                    return Err(AiError::aborted("Request was aborted"));
                }
                if attempt < max_retries && !error.message.contains("usage limit") {
                    let delay = BASE_DELAY_MS * 2f64.powi(attempt as i32);
                    sleep(delay, options.base.signal.as_ref()).await?;
                    last_error = Some(AiError::new(AiErrorKind::Transport, error.message));
                    continue;
                }
                return Err(AiError::new(AiErrorKind::Transport, error.message));
            }
        };
        if let Some(on_response) = &options.base.on_response {
            on_response(
                crate::types::ProviderResponse {
                    status: sent.response.status,
                    headers: headers_to_record(&sent.response.headers),
                },
                model,
            );
        }
        if (200..300).contains(&sent.response.status) {
            response = Some(sent);
            break;
        }
        let status = sent.response.status;
        // TS: `new Response(errorText, { status, statusText: response.statusText })`，
        // `parseErrorResponse` 在空体时回退到 statusText。冻结的 `HttpResponse` 不携带
        // 服务端 reason phrase，这里用状态码的标准 reason phrase（Node 服务器实际发出的
        // 正是它）作为等价物；未知状态码仍为空串。
        let status_text = reqwest::StatusCode::from_u16(status)
            .ok()
            .and_then(|status| status.canonical_reason())
            .unwrap_or_default()
            .to_string();
        let headers = sent.response.headers.clone();
        let error_text = match collect_body(sent.body).await {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(error) => {
                // TS：`await response.text()` 的 rejection 同样进入外层 catch（可重试）。
                let error = AiError::new(AiErrorKind::Transport, error.message);
                if attempt < max_retries && !error.message.contains("usage limit") {
                    sleep(
                        BASE_DELAY_MS * 2f64.powi(attempt as i32),
                        options.base.signal.as_ref(),
                    )
                    .await?;
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
            }
        };
        // 可重试状态码：服务端给了 retry-after 就用它，否则指数退避；延迟超过
        // `max_retry_delay_ms` 视为终态错误（`RetryDelayExceededError`），不再等待。
        if attempt < max_retries && is_retryable_error(status, &error_text) {
            let retry_after_delay = get_retry_after_delay_ms(&headers);
            let delay_ms = match retry_after_delay {
                None => BASE_DELAY_MS * 2f64.powi(attempt as i32),
                Some(delay) => {
                    match validate_retry_delay_ms(delay, options.base.max_retry_delay_ms) {
                        Ok(delay) => delay,
                        Err(error) => return Err(AiError::new(AiErrorKind::Other, error.message)),
                    }
                }
            };
            sleep(delay_ms, options.base.signal.as_ref()).await?;
            continue;
        }
        let info = parse_error_response(&error_text, status, &status_text);
        let message = info
            .friendly_message
            .filter(|message| !message.is_empty())
            .unwrap_or(info.message);
        let error = AiError::new(AiErrorKind::Other, message);
        // TS 在「内层 try」里抛出这个错误，外层 catch 会对**所有**非 usage-limit 错误重试
        // （`attempt < maxRetries` 时按指数退避），包括 `isRetryableError()` 为 false 的
        // 400/401/403/404/422。`RetryDelayExceededError` 在上面直接返回，与该 catch 的
        // `instanceof` 排除一致。
        if attempt < max_retries && !error.message.contains("usage limit") {
            sleep(
                BASE_DELAY_MS * 2f64.powi(attempt as i32),
                options.base.signal.as_ref(),
            )
            .await?;
            last_error = Some(error);
            continue;
        }
        return Err(error);
    }

    // 防御性兜底：当前控制流不会走到这里——非 2xx 的每条路径在最后一轮都会直接
    // `return Err`（`continue` 仅在 `attempt < max_retries` 时执行），只有 2xx 的 `break`
    // 能带着 `Some(response)` 离开循环。保留该分支只为防止未来改动引入“循环耗尽却无错误”
    // 的静默路径；若真的到达这里，`last_error` 是唯一可用的失败原因。
    let Some(response) = response else {
        return Err(
            last_error.unwrap_or_else(|| AiError::new(AiErrorKind::Other, "Failed after retries"))
        );
    };
    if !start_emitted.load(std::sync::atomic::Ordering::SeqCst) {
        start_emitted.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(output.clone()),
        });
    }
    process_stream(
        response.body,
        output,
        stream,
        model,
        &grammar_tool_input_properties,
        options,
        None,
        options.base.signal.clone(),
        options.base.on_provider_stream_event.clone(),
    )
    .await?;

    if options
        .base
        .signal
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(AiError::aborted("Request was aborted"));
    }
    assert_successful_output(output)?;
    let _ = stream.push(AssistantMessageEvent::Done {
        reason: done_reason(output.stop_reason),
        message: Box::new(output.clone()),
    });
    stream.end(output.clone());
    Ok(())
}

/// 诊断字段 `configuredTransport` 的取值，与 TS 的 `Transport` 字面量一致。
fn transport_literal(transport: crate::types::Transport) -> &'static str {
    match transport {
        crate::types::Transport::Sse => "sse",
        crate::types::Transport::Websocket => "websocket",
        crate::types::Transport::WebsocketCached => "websocket-cached",
        crate::types::Transport::Auto => "auto",
    }
}

/// 生成 UUIDv7（时间有序），供没有 `codexSessionId` 的 WebSocket 请求充当 request id。
#[allow(clippy::result_large_err)]
fn uuid_v7() -> Result<String, AiError> {
    crate::utils::uuid::uuidv7(None)
        .map_err(|error| AiError::new(AiErrorKind::Other, error.to_string()))
}

/// 取注入的 `fetch` 实现；没有注入时新建默认 reqwest 客户端（TS `globalThis.fetch` 的
/// 等价物）。注入点保持不变，便于测试替换整个 HTTP 栈。
#[allow(clippy::result_large_err)]
fn fetch_client(fetch: Option<&SharedHttpClient>) -> Result<SharedHttpClient, AiError> {
    match fetch {
        Some(fetch) => Ok(fetch.clone()),
        None => crate::http::reqwest_client::ReqwestHttpClient::new()
            .map(|client| client.shared())
            .map_err(AiError::from),
    }
}

/// 把流式响应体收干成字节数组；只在非 2xx 分支使用（错误文本需要完整正文）。
async fn collect_body(mut body: crate::http::ByteStream) -> Result<Vec<u8>, HttpError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

fn parse_reasoning_level(value: &str) -> Option<ModelThinkingLevel> {
    match value {
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

// 构造 Codex Responses 请求体（TS `buildRequestBody`）。
//
// 与 `openai-responses` 的差别集中在这里（spec §2.14.7 / §2.14.16）：
// - 系统提示单独走顶层 `instructions`，`input` 里不再包含它；为空时用逐字
//   `"You are a helpful assistant."`。
// - 恒发 `store: false`、`stream: true`、`parallel_tool_calls: true`；`tool_choice` 恒有值
//   （默认字符串 `"auto"`）。
// - `text.verbosity` 缺省/空串 → `"low"`；`include` 固定请求
//   `reasoning.encrypted_content`。
// - `prompt_cache_key` 只在 `cache_session_id` 为 `Some` 时写入；这里不额外过滤空串
//   （TS 的 `clampOpenAIPromptCacheKey` 对空串返回空串，序列化后字段仍然存在）。
//
// `reasoning` 的写入规则对应 TS 对 `undefined` / `null` / 字符串的区分，详见下方注释。
#[allow(clippy::result_large_err)]
fn build_request_body(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICodexResponsesOptions,
    cache_session_id: Option<&str>,
    grammar_tool_input_properties: &IndexMap<String, String>,
) -> Result<Value, AiError> {
    let supports_strict_mode = compat_bool(model, "supportsStrictMode").unwrap_or(true);
    let supports_openai_grammar_tools =
        compat_bool(model, "supportsOpenAIGrammarTools").unwrap_or(false);
    let supports_additional_tools = compat_bool(model, "supportsAdditionalTools").unwrap_or(false);
    let supports_tool_search = compat_bool(model, "supportsToolSearch").unwrap_or(false);
    let supports_mid_convo_system_messages =
        compat_bool(model, "supportsMidConvoSystemMessages").unwrap_or(false);
    let transcript_tools = resolve_transcript_tools(
        &context.0.messages,
        get_responses_tool_support(&ConvertResponsesMessagesOptions {
            supports_mid_convo_system_messages: Some(supports_mid_convo_system_messages),
            supports_additional_tools: Some(supports_additional_tools),
            supports_tool_search: Some(supports_tool_search),
            ..Default::default()
        }),
    );
    let tool_options = ConvertResponsesToolsOptions {
        strict: Some(None),
        supports_strict_mode: Some(supports_strict_mode),
        supports_openai_grammar_tools: Some(supports_openai_grammar_tools),
        tool_search_result: false,
    };
    let messages = convert_responses_messages(
        model,
        context,
        &CODEX_TOOL_CALL_PROVIDERS,
        Some(&ConvertResponsesMessagesOptions {
            include_system_prompt: Some(false),
            grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
            supports_mid_convo_system_messages: Some(supports_mid_convo_system_messages),
            supports_additional_tools: Some(supports_additional_tools),
            supports_tool_search: Some(supports_tool_search),
            tool_options: Some(tool_options.clone()),
            configuration_update_effort: None,
        }),
    )?;
    let initial_system_message = get_initial_system_message(&context.0.messages);
    let instructions = initial_system_message
        .map(get_system_message_text)
        .unwrap_or_default();
    let mut body = Map::new();
    body.insert("model".into(), Value::String(model.id.clone()));
    body.insert("store".into(), Value::Bool(false));
    body.insert("stream".into(), Value::Bool(true));
    body.insert(
        "instructions".into(),
        Value::String(if instructions.is_empty() {
            "You are a helpful assistant.".into()
        } else {
            instructions
        }),
    );
    body.insert("input".into(), Value::Array(messages));
    body.insert(
        "text".into(),
        // TS: `options?.textVerbosity || "low"` —— 空串是假值，同样回退 "low"。
        json!({ "verbosity": options.text_verbosity.clone().filter(|value| !value.is_empty()).unwrap_or_else(|| "low".into()) }),
    );
    body.insert(
        "include".into(),
        Value::Array(vec![Value::String("reasoning.encrypted_content".into())]),
    );
    if let Some(cache_session_id) = cache_session_id {
        body.insert(
            "prompt_cache_key".into(),
            Value::String(cache_session_id.to_string()),
        );
    }
    body.insert(
        "tool_choice".into(),
        options
            .tool_choice
            .clone()
            .unwrap_or_else(|| Value::String("auto".into())),
    );
    body.insert("parallel_tool_calls".into(), Value::Bool(true));
    if let Some(temperature) = options.base.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(service_tier) = &options.service_tier {
        body.insert("service_tier".into(), Value::String(service_tier.clone()));
    }
    if !transcript_tools.request_tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(
                &transcript_tools.request_tools,
                Some(&tool_options),
            )?),
        );
    }
    // TS 的 `undefined` / `null` / 字符串三态：`thinking_level_map` 的“缺键”（None）与
    // “显式 null”（Some(None)）只在 `"none"` 分支含义不同（缺键 → `"none"`，null →
    // 省略 reasoning）；其它档位两者都按 `??` 回退到请求值。
    if let Some(reasoning_effort) = &options.reasoning_effort {
        let effort: Option<String> = if reasoning_effort == "none" {
            match model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off))
            {
                None => Some("none".to_string()),
                Some(Some(value)) => Some(value.clone()),
                Some(None) => None,
            }
        } else {
            let mapped = parse_reasoning_level(reasoning_effort).and_then(|level| {
                model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| map.get(&level))
                    .cloned()
            });
            match mapped {
                Some(Some(value)) => Some(value),
                // TS: `model.thinkingLevelMap?.[effort] ?? effort` —— 显式 null 也回退到
                // 请求的 effort 字符串（只有 `"none"` 分支把 null 当作“省略 reasoning”）。
                Some(None) => Some(reasoning_effort.clone()),
                None => Some(reasoning_effort.clone()),
            }
        };
        if let Some(effort) = effort {
            body.insert(
                "reasoning".into(),
                json!({
                    "effort": effort,
                    "summary": options.reasoning_summary.clone().unwrap_or_else(|| "auto".into()),
                }),
            );
        }
    } else if model.reasoning == Some(true)
        && !matches!(
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off)),
            Some(None)
        )
    {
        // 没有显式 effort：推理模型用 `off` 档的映射值（缺键时 `"none"`）作为 effort；
        // `off` 显式映射为 null 的情况已在上面的条件里排除，整个 `reasoning` 字段省略。
        let effort = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ModelThinkingLevel::Off))
            .cloned()
            .flatten()
            .unwrap_or_else(|| "none".to_string());
        body.insert("reasoning".into(), json!({ "effort": effort }));
    }
    Ok(Value::Object(body))
}

// 把模型 `base_url` 归一化成 SSE 端点（TS `resolveCodexUrl`）：空白（JS `trim` 意义下）
// 回退官方默认地址；去掉尾部全部 `/`；已以 `/codex/responses` 结尾则不动、以 `/codex`
// 结尾补 `/responses`，否则追加 `/codex/responses`。
fn resolve_codex_url(base_url: Option<&str>) -> String {
    let raw = base_url
        .filter(|base_url| !is_js_trim_empty(base_url))
        .unwrap_or(DEFAULT_CODEX_BASE_URL);
    let normalized = raw.trim_end_matches('/').to_string();
    if normalized.ends_with("/codex/responses") {
        return normalized;
    }
    if normalized.ends_with("/codex") {
        return format!("{normalized}/responses");
    }
    format!("{normalized}/codex/responses")
}

// 在 `resolve_codex_url` 的结果上把 `https` → `wss`、`http` → `ws`（TS
// `resolveCodexWebSocketUrl`）。URL 解析失败时直接返回原字符串（TS 的 `new URL()` 会抛）；
// 随后 `into_client_request` 会在连接阶段报错。
fn resolve_codex_websocket_url(base_url: &str) -> String {
    let resolved = resolve_codex_url(Some(base_url));
    match url::Url::parse(&resolved) {
        Ok(mut url) => {
            let scheme = match url.scheme() {
                "https" => "wss".to_string(),
                "http" => "ws".to_string(),
                other => other.to_string(),
            };
            let _ = url.set_scheme(&scheme);
            url.to_string()
        }
        Err(_) => resolved,
    }
}

// ---------------------------------------------------------------------------
// 认证与请求头
// ---------------------------------------------------------------------------

// 头构造的优先级固定为：default（originator/identity）→ model.headers → options.headers
// → Authorization / chatgpt-account-id。最后两个**恒被覆盖**，调用方无法伪造账号；
// `set_header`/`delete_header` 按 ASCII 大小写不敏感匹配，等价于 WHATWG `Headers`。

/// 从 Codex 的 JWT access token 里取 `chatgpt_account_id`。
///
/// 只接受恰好 3 段（header.payload.signature）；payload 用**标准 base64**（不是
/// base64url，`-`/`_` 会失败）并宽容缺失 padding。任何失败（段数、解码、JSON、claim 缺失
/// 或空串）都折叠成同一条 `AiErrorKind::Auth` 文本
/// `"Failed to extract accountId from token"`——TS 外层 catch 的逐字行为。
#[allow(clippy::result_large_err)]
fn extract_account_id(token: &str) -> Result<String, AiError> {
    fn inner(token: &str) -> Option<String> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        let payload = decode_base64_atob(parts[1])?;
        let parsed: Value = serde_json::from_slice(&payload).ok()?;
        let account_id = obj(&parsed, JWT_CLAIM_PATH)
            .and_then(|claim| obj(claim, "chatgpt_account_id"))
            .and_then(Value::as_str)?;
        if account_id.is_empty() {
            return None;
        }
        Some(account_id.to_string())
    }
    inner(token)
        .ok_or_else(|| AiError::new(AiErrorKind::Auth, "Failed to extract accountId from token"))
}

/// Node 的 `atob`：标准 base64，缺 padding 宽容，非法字符报错。
///
/// 先剥掉尾部所有 `=` 再补到 4 的倍数；余数为 1 是非法长度，直接失败。不做 base64url 的
/// `-`/`_` 替换（spec §2.14.14：JWT 段含这些字符时会失败）。
fn decode_base64_atob(text: &str) -> Option<Vec<u8>> {
    let mut padded = text.trim_end_matches('=').to_string();
    let remainder = padded.len() % 4;
    if remainder == 1 {
        return None;
    }
    if remainder != 0 {
        padded.extend(std::iter::repeat_n('=', 4 - remainder));
    }
    base64::engine::general_purpose::STANDARD
        .decode(padded.as_bytes())
        .ok()
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some(entry) = headers
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
    {
        entry.1 = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

fn delete_header(headers: &mut Vec<(String, String)>, name: &str) {
    headers.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
}

#[cfg(test)]
fn get_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn merge_into_headers(target: &mut Vec<(String, String)>, source: &ProviderHeaders) {
    for (name, value) in source {
        if let Some(value) = value {
            set_header(target, name, value);
        } else {
            delete_header(target, name);
        }
    }
}

// 基础头：先铺 default_headers，再依次覆盖 model.headers 与 caller headers
// （`ProviderHeaders` 的 `None` 表示删除同名默认头），最后无条件写 Authorization 与
// chatgpt-account-id——顺序即优先级，认证头不可被前两层覆盖。
fn build_base_codex_headers(
    default_headers: &[(String, String)],
    init_headers: Option<&IndexMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
) -> Vec<(String, String)> {
    let mut headers = default_headers.to_vec();
    if let Some(init_headers) = init_headers {
        for (name, value) in init_headers {
            set_header(&mut headers, name, value);
        }
    }
    if let Some(additional_headers) = additional_headers {
        for (name, value) in additional_headers {
            match value {
                Some(value) => set_header(&mut headers, name, value),
                None => delete_header(&mut headers, name),
            }
        }
    }
    set_header(&mut headers, "Authorization", &format!("Bearer {token}"));
    set_header(&mut headers, "chatgpt-account-id", account_id);
    headers
}

// SSE 专属头：`OpenAI-Beta: responses=experimental`、`accept: text/event-stream`、
// `content-type: application/json`；session id 非空时同时写 `session-id` 与
// `x-client-request-id`（两者同值，服务端用它做缓存亲和）。
fn build_sse_headers(
    default_headers: &[(String, String)],
    init_headers: Option<&IndexMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut headers = build_base_codex_headers(
        default_headers,
        init_headers,
        additional_headers,
        account_id,
        token,
    );
    set_header(&mut headers, "OpenAI-Beta", "responses=experimental");
    set_header(&mut headers, "accept", "text/event-stream");
    set_header(&mut headers, "content-type", "application/json");
    if let Some(session_id) = session_id.filter(|session_id| !session_id.is_empty()) {
        set_header(&mut headers, "session-id", session_id);
        set_header(&mut headers, "x-client-request-id", session_id);
    }
    headers
}

// WebSocket 专属头：删掉 SSE 的 `accept`/`content-type`/`OpenAI-Beta`，改用
// `responses_websockets=2026-02-06`；`x-client-request-id` 与 `session-id` 都用请求 id
// （有 codexSessionId 时就是它，否则是每次请求新生成的 uuidv7）。
fn build_websocket_headers(
    default_headers: &[(String, String)],
    init_headers: Option<&IndexMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
    request_id: &str,
) -> Vec<(String, String)> {
    let mut headers = build_base_codex_headers(
        default_headers,
        init_headers,
        additional_headers,
        account_id,
        token,
    );
    delete_header(&mut headers, "accept");
    delete_header(&mut headers, "content-type");
    delete_header(&mut headers, "OpenAI-Beta");
    set_header(
        &mut headers,
        "OpenAI-Beta",
        OPENAI_BETA_RESPONSES_WEBSOCKETS,
    );
    set_header(&mut headers, "x-client-request-id", request_id);
    set_header(&mut headers, "session-id", request_id);
    headers
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

// 错误名是跨函数契约：`is_codex_non_transport_error` 用 `AiError.name` 判断“不应回退
// SSE”，因此 `CodexApiError`/`CodexProtocolError` 不能随意改名。`detail` 里的 `code` 与
// `payload` 只用于诊断，以及 `websocket_connection_limit_reached` /
// `previous_response_not_found` 两个可恢复错误的识别。

fn codex_api_error(message: impl Into<String>, code: Option<String>, payload: Value) -> AiError {
    AiError::new(AiErrorKind::Other, message)
        .with_name("CodexApiError")
        .with_detail(json!({ "code": code, "payload": payload }))
}

fn codex_protocol_error(message: impl Into<String>, payload: impl Into<Value>) -> AiError {
    AiError::new(AiErrorKind::Parse, message)
        .with_name("CodexProtocolError")
        .with_detail(payload.into())
}

fn is_codex_non_transport_error(error: &AiError) -> bool {
    matches!(
        error.name.as_str(),
        "CodexApiError" | "CodexProtocolError" | "ProviderStreamEventCallbackError"
    )
}

fn error_code(error: &AiError) -> Option<&str> {
    error
        .detail
        .as_ref()
        .and_then(|detail| obj(detail, "code"))
        .and_then(Value::as_str)
}

fn is_websocket_connection_limit_reached_error(error: &AiError) -> bool {
    error.name == "CodexApiError"
        && error_code(error) == Some(WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE)
}

fn is_previous_response_not_found_error(error: &AiError) -> bool {
    error.name == "CodexApiError" && error_code(error) == Some(PREVIOUS_RESPONSE_NOT_FOUND_CODE)
}

/// 从 `error` 事件里取 code/message：顶层字段优先，其次嵌套的 `error` 对象（TS 同序）。
fn extract_codex_event_error(event: &Value) -> (Option<String>, Option<String>) {
    let nested = obj(event, "error").filter(|error| error.is_object());
    let code = str_of(event, "code").map(str::to_string).or_else(|| {
        nested
            .and_then(|nested| str_of(nested, "code"))
            .map(str::to_string)
    });
    let message = str_of(event, "message").map(str::to_string).or_else(|| {
        nested
            .and_then(|nested| str_of(nested, "message"))
            .map(str::to_string)
    });
    (code, message)
}

/// 只保留 Codex 已知的 6 个响应状态；未知状态返回 `None`，调用方会把 `status` 字段整个
/// 删掉（而不是原样透传）。
fn normalize_codex_status(status: Option<&Value>) -> Option<Value> {
    let status = status?.as_str()?;
    CODEX_RESPONSE_STATUSES
        .contains(&status)
        .then(|| Value::String(status.to_string()))
}

// ---------------------------------------------------------------------------
// 流处理（SSE / WS 共用）
// ---------------------------------------------------------------------------

/// `next_mapped` 的三种去向：产出事件 / 跳过 / 结束（结束事件本身仍会产出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MappedEvent {
    Yield,
    Skip,
    End,
}

/// 把原始 SSE/WS 事件映射成 `process_responses_stream` 认识的形状。
///
/// - `on_provider_stream_event` 在这里、分类之前对每个原始事件调用一次；两条传输都把该
///   回调交给 mapper，并把 `OpenAIResponsesStreamOptions::on_provider_stream_event` 置
///   `None`，避免重复调用。
/// - `error` / `response.failed` → `CodexApiError`（`detail.code` 供上层识别可重试错误）。
/// - `response.done` / `response.completed` / `response.incomplete` → 统一改写成
///   `response.completed`，`response.status` 归一化（未知状态删除），返回
///   `MappedEvent::End`，因此后续不再有事件。
/// - `end_turn` 不是 `process_responses_stream` 的字段，先存在共享 `Mutex` 里，由调用方回填。
struct CodexEventMapper {
    on_provider_stream_event: Option<crate::types::OnProviderStreamEventFn>,
    model: Model,
    end_turn: Arc<Mutex<Option<bool>>>,
}

impl CodexEventMapper {
    #[allow(clippy::result_large_err)]
    fn map(&self, event: Value) -> Result<(MappedEvent, Option<Value>), AiError> {
        if let Some(callback) = &self.on_provider_stream_event {
            callback(event.clone(), &self.model);
        }
        let Some(event_type) = str_of(&event, "type").map(str::to_string) else {
            return Ok((MappedEvent::Skip, None));
        };
        match event_type.as_str() {
            "error" => {
                let (code, message) = extract_codex_event_error(&event);
                let message = message.filter(|message| !message.is_empty());
                let code = code.filter(|code| !code.is_empty());
                let fallback = if message.is_some() || code.is_some() {
                    message.or_else(|| code.clone()).unwrap_or_default()
                } else {
                    kiana_json::js_object::stringify(&event)
                };
                return Err(codex_api_error(
                    format!("Codex error: {fallback}"),
                    code,
                    event,
                ));
            }
            "response.failed" => {
                let response = obj(&event, "response");
                let code = response
                    .and_then(|response| obj(response, "error"))
                    .and_then(|error| str_of(error, "code"))
                    .map(str::to_string);
                let message = response
                    .and_then(|response| obj(response, "error"))
                    .and_then(|error| str_of(error, "message"))
                    .map(str::to_string);
                return Err(codex_api_error(
                    message
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "Codex response failed".into()),
                    code.filter(|code| !code.is_empty()),
                    event,
                ));
            }
            // 终态事件：字段名与 status 对齐 `process_responses_stream` 的期望后结束映射。
            "response.done" | "response.completed" | "response.incomplete" => {
                let response = obj(&event, "response");
                if let Some(end_turn) = response
                    .and_then(|response| obj(response, "end_turn"))
                    .and_then(Value::as_bool)
                {
                    *self
                        .end_turn
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(end_turn);
                }
                let normalized_response = response.map(|response| {
                    let mut map = match response {
                        Value::Object(map) => map.clone(),
                        _ => Map::new(),
                    };
                    match normalize_codex_status(obj(response, "status")) {
                        Some(status) => {
                            map.insert("status".into(), status);
                        }
                        None => {
                            map.shift_remove("status");
                        }
                    }
                    Value::Object(map)
                });
                let mut mapped = match event {
                    Value::Object(map) => map,
                    _ => Map::new(),
                };
                mapped.insert("type".into(), Value::String("response.completed".into()));
                match normalized_response {
                    Some(response) => {
                        mapped.insert("response".into(), response);
                    }
                    None => {
                        mapped.shift_remove("response");
                    }
                }
                return Ok((MappedEvent::End, Some(Value::Object(mapped))));
            }
            _ => {}
        }
        Ok((MappedEvent::Yield, Some(event)))
    }
}

// `WsEventReader` 持有整个 WebSocket 连接（约 1.4 KB）；装箱只会把所有分支都变成
// 堆分配，且此枚举在流生命周期内只创建一次。
#[allow(clippy::large_enum_variant)]
enum CodexEventSource {
    Sse(CodexSseReader),
    WebSocket(WsEventReader),
}

impl CodexEventSource {
    #[allow(clippy::result_large_err)]
    async fn next_raw(&mut self) -> Result<Option<Value>, AiError> {
        match self {
            CodexEventSource::Sse(reader) => reader.next_event().await,
            CodexEventSource::WebSocket(reader) => reader.next_event().await,
        }
    }
}

/// 原始事件源 + 映射器 + 首次事件回调的组合。
///
/// 状态机：`ended` 之后 `next_mapped` 恒返回 `None`；`started` 保证 `on_first_event` 只
/// 触发一次，且只在遇到**非 `Skip`** 的事件时触发。`End` 分支先把映射后的终态事件返回，
/// 下一次调用才结束。
struct MappedSource {
    source: Option<CodexEventSource>,
    mapper: CodexEventMapper,
    ended: bool,
    started: bool,
    on_first_event: Option<Box<dyn FnMut() + Send>>,
}

impl MappedSource {
    /// 返回 `None` 表示流结束。
    ///
    /// `Skip` 被吞掉；`End` 在本次调用返回终态事件、下一次调用返回 `None`；来源错误直接
    /// 上抛，流随之结束（不会继续读取）。
    #[allow(clippy::result_large_err)]
    async fn next_mapped(&mut self) -> Result<Option<Value>, AiError> {
        if self.ended {
            return Ok(None);
        }
        loop {
            let Some(source) = self.source.as_mut() else {
                return Ok(None);
            };
            let Some(event) = source.next_raw().await? else {
                self.source = None;
                return Ok(None);
            };
            let mapped = self.mapper.map(event)?;
            // TS 的 `startWebSocketOutputOnFirstEvent`：没有 `type` 的事件（Skip）不算开始。
            if !matches!(mapped.0, MappedEvent::Skip) && !self.started {
                self.started = true;
                if let Some(callback) = self.on_first_event.as_mut() {
                    callback();
                }
            }
            match mapped {
                (MappedEvent::Skip, _) => continue,
                (MappedEvent::Yield, Some(event)) => return Ok(Some(event)),
                (MappedEvent::Yield, None) => continue,
                (MappedEvent::End, event) => {
                    self.ended = true;
                    return Ok(event);
                }
            }
        }
    }
}

/// 把 `MappedSource` 适配成 `ProviderEventStream`：错误作为一个 `Err` 元素发出后流结束，
/// 正常结束返回 `None`。
///
/// 只有 SSE 路径用它；WebSocket 路径要在流结束后取回 socket 以便缓存复用，用的是
/// `process_websocket_stream` 里基于 `tokio::sync::Mutex` 的定制 unfold。
fn responses_event_stream(
    source: MappedSource,
) -> crate::api::openai_responses_shared::ProviderEventStream {
    Box::pin(futures::stream::unfold(Some(source), |state| async move {
        let mut source = state?;
        match source.next_mapped().await {
            Ok(Some(event)) => Some((Ok(event), Some(source))),
            Ok(None) => None,
            Err(error) => Some((Err(error), None)),
        }
    }))
}

/// SSE 事件消费入口。
///
/// Start 事件在上游拿到 2xx 响应头后已经发出，所以这里的 `MappedSource` 以
/// `started: true`、`on_first_event: None` 构造。`end_turn` 传 `None`：SSE 路径不需要把
/// 槽位共享出去，mapper 的临时槽在函数末尾直接回填到 `output`。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)]
async fn process_stream(
    body: crate::http::ByteStream,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    grammar_tool_input_properties: &IndexMap<String, String>,
    options: &OpenAICodexResponsesOptions,
    end_turn: Option<Arc<Mutex<Option<bool>>>>,
    signal: Option<tokio_util::sync::CancellationToken>,
    on_provider_stream_event: Option<crate::types::OnProviderStreamEventFn>,
) -> Result<(), AiError> {
    let mapper_end_turn = end_turn.unwrap_or_else(|| Arc::new(Mutex::new(None)));
    let mapper = CodexEventMapper {
        on_provider_stream_event,
        model: model.clone(),
        end_turn: mapper_end_turn.clone(),
    };
    let model_id = model.id.clone();
    let source = MappedSource {
        source: Some(CodexEventSource::Sse(CodexSseReader::new(body, signal))),
        mapper,
        ended: false,
        started: true,
        on_first_event: None,
    };
    let event_stream = responses_event_stream(source);
    // `on_provider_stream_event` 已由 mapper 对每个原始事件调用；这里再传会重复。
    process_responses_stream(
        event_stream,
        output,
        stream,
        model,
        Some(&OpenAIResponsesStreamOptions {
            on_provider_stream_event: None,
            service_tier: options.service_tier.clone(),
            grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
            resolve_service_tier: Some(Arc::new(resolve_codex_service_tier)),
            apply_service_tier_pricing: Some(Arc::new(
                move |usage: &mut Usage, service_tier: Option<&str>| {
                    apply_service_tier_pricing(usage, service_tier, &model_id);
                },
            )),
        }),
    )
    .await?;
    if let Some(end_turn) = *mapper_end_turn
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        output.end_turn = Some(end_turn);
    }
    Ok(())
}

/// `resolveCodexServiceTier`：服务端回显 `"default"` 时保留客户端请求的 flex/priority，
/// 其余情况优先服务端值。结果交给 `apply_service_tier_pricing` 计算费用倍率。
fn resolve_codex_service_tier(
    response_service_tier: Option<&str>,
    request_service_tier: Option<&str>,
) -> Option<String> {
    if response_service_tier == Some("default")
        && matches!(request_service_tier, Some("flex") | Some("priority"))
    {
        return request_service_tier.map(str::to_string);
    }
    response_service_tier
        .map(str::to_string)
        .or_else(|| request_service_tier.map(str::to_string))
}

/// 服务层的费用倍率：flex 半价；priority 对 `gpt-5.5` 2.5 倍、其它 2 倍；其余 1 倍。
fn get_service_tier_cost_multiplier(model_id: &str, service_tier: Option<&str>) -> f64 {
    match service_tier {
        Some("flex") => 0.5,
        Some("priority") => {
            if model_id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

/// 把已由 `calculate_cost` 算好的四项费用乘上倍率，并重算 `total`。
/// 倍率为 1 时直接返回，避免无意义地重写浮点数。
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
// SSE 解析
// ---------------------------------------------------------------------------

/// Codex SSE 的增量分帧器（TS `parseSSE`）。
///
/// 规则：按空行（`\n\n`）切帧；每帧只取 `data:` 开头的行，去掉前缀后按 JS `trim` 处理，
/// 多行用 `\n` 连接；空 data 与 `[DONE]` 跳过；JSON 解析失败抛 `CodexProtocolError`。
/// EOF 时若 buffer 仍有非空白内容，补一个 `\n\n` 让最后一帧能被切出——否则末尾没有空行的
/// 终态事件会丢失（测试 `processes_a_terminal_sse_event_without_a_trailing_blank_line`）。
struct CodexSseReader {
    body: crate::http::ByteStream,
    decoder: Utf8StreamDecoder,
    buffer: String,
    signal: Option<tokio_util::sync::CancellationToken>,
    body_finished: bool,
    finished: bool,
}

impl CodexSseReader {
    fn new(
        body: crate::http::ByteStream,
        signal: Option<tokio_util::sync::CancellationToken>,
    ) -> Self {
        Self {
            body,
            decoder: Utf8StreamDecoder::default(),
            buffer: String::new(),
            signal,
            body_finished: false,
            finished: false,
        }
    }

    fn is_aborted(&self) -> bool {
        self.signal
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    }

    #[allow(clippy::result_large_err)]
    async fn next_event(&mut self) -> Result<Option<Value>, AiError> {
        loop {
            if self.finished {
                return Ok(None);
            }
            if self.is_aborted() {
                return Err(AiError::aborted("Request was aborted"));
            }
            if let Some(index) = self.buffer.find("\n\n") {
                let chunk: String = self.buffer[..index].to_string();
                self.buffer = self.buffer[index + 2..].to_string();
                if let Some(event) = self.parse_chunk(&chunk)? {
                    return Ok(Some(event));
                }
                continue;
            }
            if self.body_finished {
                self.finished = true;
                continue;
            }
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
                    let tail = self.decoder.decode(&[], false);
                    self.buffer.push_str(&tail);
                    // EOF：先冲刷解码器可能残留的不完整序列，再给非空残帧补分隔符，
                    // 下一轮循环就能把它当作完整帧解析。
                    if !is_js_trim_empty(&self.buffer) {
                        self.buffer.push_str("\n\n");
                    }
                    self.body_finished = true;
                }
            }
        }
    }

    /// 解析一个已按空行切出的帧；`Ok(None)` 表示该帧不含可用事件（无 `data:`、空数据或
    /// `[DONE]`），调用方应继续读下一帧。
    #[allow(clippy::result_large_err)]
    fn parse_chunk(&self, chunk: &str) -> Result<Option<Value>, AiError> {
        let data_lines: Vec<String> = chunk
            .split('\n')
            .filter(|line| line.starts_with("data:"))
            .map(|line| kiana_json::js_string::js_trim(&line[5..]).to_string())
            .collect();
        if data_lines.is_empty() {
            return Ok(None);
        }
        let data = kiana_json::js_string::js_trim(&data_lines.join("\n")).to_string();
        if data.is_empty() || data == "[DONE]" {
            return Ok(None);
        }
        match kiana_json::js_parse::parse(&data) {
            Ok(value) => Ok(Some(value)),
            Err(error) => Err(codex_protocol_error(
                format!("Invalid Codex SSE JSON: {}", error.message),
                Value::String(data),
            )),
        }
    }
}

/// 增量 UTF-8 解码（与 `TextDecoder` 的流式语义一致）。
///
/// 网络 chunk 可能把一个多字节字符切成两半：不完整的尾部先留在 `pending`，等下一块再拼；
/// 确定非法的字节立即产出 U+FFFD 并前进（`error_len() == None` 表示“还可能是合法前缀”，
/// 只是数据不够）。`stream == false` 表示流结束，残余字节按损失替换输出。
#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
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

// ---------------------------------------------------------------------------
// WebSocket 支持
// ---------------------------------------------------------------------------
//
// 三张进程级表（都是 `OnceLock<Mutex<...>>`）：
// - `websocket_session_cache`：sessionId → accountId → 一条可复用连接（含 continuation）；
// - `websocket_debug_stats`：sessionId → 调试统计（只在 session id 非空时记账）；
// - `websocket_sse_fallback_sessions`：发生过传输故障的 session（粘滞回退标记）。
//
// 锁纪律：都用 `std::sync::Mutex`，临界区内不 await；同一函数最多持有一把锁（按顺序取用、
// 用完即放），因此不存在锁序问题。缓存按 `(sessionId, accountId)` 隔离，不同账号不会
// 复用到彼此的连接。

type WsSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// `OpenAICodexWebSocketDebugStats`。
///
/// 每个 session 一份；只在 `session_id` 非空时记账，没有 session id 的请求完全不统计。
/// [`get_openai_codex_websocket_debug_stats`] 返回的是浅拷贝，调用方修改副本不影响内部状态。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenAICodexWebSocketDebugStats {
    /// 成功借到连接（`acquire_websocket` 返回 `Ok`）后的 WebSocket 请求次数，含内部重试
    /// 与发送后失败的尝试。连接建立 / acquire 失败不计数，只计入 `websocket_failures`。
    pub requests: u64,
    /// 新建连接数（正常缓存连接与不缓存的临时连接都算）。
    pub connections_created: u64,
    /// 从缓存取出的连接数。
    pub connections_reused: u64,
    /// 处于 cached-context 模式（`websocket-cached` 或 `auto`）的请求数。统计的是“模式”，
    /// 不等于“真的命中了增量”。
    pub cached_context_requests: u64,
    /// 请求体 `store == true` 的请求数；Codex 正常恒为 false，用于发现异常调用方。
    pub store_true_requests: u64,
    /// 发送了完整上下文（没有 `previous_response_id`）的请求数。
    pub full_context_requests: u64,
    /// 发送了增量上下文（带 `previous_response_id`）的请求数。
    pub delta_requests: u64,
    /// 最近一次请求的 `input` 项数。
    pub last_input_items: u64,
    /// 最近一次增量请求的 `input` 项数；完整上下文请求会把它清空。
    pub last_delta_input_items: Option<u64>,
    /// 最近一次增量请求使用的 `previous_response_id`。
    pub last_previous_response_id: Option<String>,
    /// WebSocket 传输失败次数（无论之后是否回退、失败是否发生在发出事件之后）。
    pub websocket_failures: u64,
    /// 进入 SSE 回退的次数，包含“session 已被标记为粘滞”后每次直接走 SSE 的计数。
    pub sse_fallbacks: u64,
    /// 该 session 当前是否处于“跳过 WebSocket”状态；从未失败/未被回退记账时为 `None`。
    /// 现有写入点只会写 `Some(true)`（失败标记是粘滞的），因此 `Some(false)` 实际不会出现。
    pub websocket_fallback_active: Option<bool>,
    /// 最近一次 WebSocket 失败的文本（`format_thrown_value` 形式，便于日志检索）。
    pub last_websocket_error: Option<String>,
}

/// 一次成功响应留下的续传状态（TS `CachedWebSocketContinuationState`）。
///
/// `last_request_body` 是**发送前的完整请求体**（不是 delta 版本），下次用它做“除
/// input/previous_response_id 外完全一致”的比较；`last_response_items` 是本轮助手输出
/// 转换出的 response items，已过滤 `function_call_output` / `custom_tool_call_output`。
struct Continuation {
    last_request_body: Value,
    last_response_id: String,
    last_response_items: Vec<Value>,
}

/// 缓存里的一条 WebSocket 连接。
struct CacheEntry {
    /// 连接 id（进程内单调递增），用于识别“缓存项已被替换”的旧引用。
    id: u64,
    /// 空闲时存 socket；被借出（`busy == true`）时为 `None`。
    socket: Option<WsSocket>,
    /// 是否正被某个请求使用；忙碌时新请求只能建不缓存的临时连接。
    busy: bool,
    /// 建连时刻（Unix 毫秒），用于 55 分钟连接年龄上限。
    created_at: i64,
    /// 上一次成功请求留下的续传状态；没有成功过则为 `None`。
    continuation: Option<Continuation>,
    /// 空闲计时器世代号：TS 在取出连接时 `clearTimeout(entry.idleTimer)`，释放时重新计时。
    /// Rust 的计时器是游离任务，用世代号让旧计时器在被取出/重新释放后失效。
    expiry_generation: u64,
}

fn websocket_session_cache() -> &'static Mutex<HashMap<String, HashMap<String, CacheEntry>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, HashMap<String, CacheEntry>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn websocket_debug_stats() -> &'static Mutex<HashMap<String, OpenAICodexWebSocketDebugStats>> {
    static STATS: OnceLock<Mutex<HashMap<String, OpenAICodexWebSocketDebugStats>>> =
        OnceLock::new();
    STATS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn websocket_sse_fallback_sessions() -> &'static Mutex<HashSet<String>> {
    static SESSIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn next_connection_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn update_websocket_debug_stats(
    session_id: Option<&str>,
    update: impl FnOnce(&mut OpenAICodexWebSocketDebugStats),
) {
    let Some(session_id) = session_id else {
        return;
    };
    let mut stats = websocket_debug_stats()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = stats.entry(session_id.to_string()).or_default();
    update(entry);
}

/// `getOpenAICodexWebSocketDebugStats(sessionId)`：浅拷贝。
///
/// 返回 `None` 表示该 session 还没有产生任何统计（不是“全零统计”）。调用方修改返回的
/// 副本不会影响内部状态。
pub fn get_openai_codex_websocket_debug_stats(
    session_id: &str,
) -> Option<OpenAICodexWebSocketDebugStats> {
    websocket_debug_stats()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .cloned()
}

/// `resetOpenAICodexWebSocketDebugStats(sessionId?)`。
///
/// 有参：删除该 session 的统计与“粘滞回退”标记；无参：清空两者。
/// **不清空连接缓存**——已建立的 WebSocket 仍可继续复用，也不会被关闭；需要关闭连接请调用
/// [`close_openai_codex_websocket_sessions`]。
pub fn reset_openai_codex_websocket_debug_stats(session_id: Option<&str>) {
    match session_id {
        Some(session_id) => {
            websocket_debug_stats()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(session_id);
            websocket_sse_fallback_sessions()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(session_id);
        }
        None => {
            websocket_debug_stats()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
            websocket_sse_fallback_sessions()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        }
    }
}

fn is_websocket_sse_fallback_active(session_id: Option<&str>) -> bool {
    session_id.is_some_and(|session_id| {
        websocket_sse_fallback_sessions()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(session_id)
    })
}

fn record_websocket_sse_fallback(session_id: Option<&str>) {
    let Some(session_id) = session_id else {
        return;
    };
    let active = is_websocket_sse_fallback_active(Some(session_id));
    update_websocket_debug_stats(Some(session_id), |stats| {
        stats.sse_fallbacks += 1;
        stats.websocket_fallback_active = Some(active);
    });
}

fn record_websocket_failure(session_id: Option<&str>, error: &AiError) {
    let Some(session_id) = session_id else {
        return;
    };
    websocket_sse_fallback_sessions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(session_id.to_string());
    update_websocket_debug_stats(Some(session_id), |stats| {
        stats.websocket_failures += 1;
        stats.last_websocket_error = Some(format_thrown_value(&ThrownValue::from_ai_error(error)));
        stats.websocket_fallback_active = Some(true);
    });
}

/// 异步发 Close 帧后丢弃 socket；发送失败被忽略（TS 的 `try { close } catch {}`）。
fn close_websocket_silently(socket: WsSocket, code: u16, reason: &'static str) {
    crate::api::lazy::spawn_forward(async move {
        let mut socket = socket;
        let _ = socket
            .close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: reason.into(),
            }))
            .await;
    });
}

/// 进程内只注册一次会话清理回调（TS 在模块加载时直接注册）。
///
/// 这里故意丢弃注销句柄：`register_session_resource_cleanup` 的返回值被 drop **不会**注销
/// （见 `session_resources` 的文档），因此清理会一直有效到进程结束。
#[allow(clippy::result_large_err)]
fn ensure_cleanup_registered() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let cleanup: crate::session_resources::SessionResourceCleanup = Arc::new(|session_id| {
            close_openai_codex_websocket_sessions(session_id);
            Ok(())
        });
        // TS 的调用方丢弃返回的注销函数；这里同样丢弃（`Box` 立刻 drop 不会注销，
        // 注销只由显式调用返回的闭包触发——见 `session_resources` 文档）。
        let _ = register_session_resource_cleanup(cleanup);
    });
}

/// `closeOpenAICodexWebSocketSessions(sessionId?)`。
///
/// 有参：移除该 session 下所有账号的连接；无参：清空全部。每个 socket 都异步发 Close 帧
/// （code 1000，reason `"debug_close"`）后丢弃，函数本身不等待关闭完成，也不清 debug 统计。
/// 它同时是 `register_session_resource_cleanup` 注册的会话清理回调。
pub fn close_openai_codex_websocket_sessions(session_id: Option<&str>) {
    let sockets: Vec<WsSocket> = {
        let mut cache = websocket_session_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match session_id {
            Some(session_id) => cache
                .remove(session_id)
                .map(|entries| {
                    entries
                        .into_values()
                        .filter_map(|entry| entry.socket)
                        .collect()
                })
                .unwrap_or_default(),
            None => {
                let all: Vec<WsSocket> = cache
                    .drain()
                    .flat_map(|(_, entries)| entries.into_values().filter_map(|entry| entry.socket))
                    .collect();
                all
            }
        }
    };
    for socket in sockets {
        close_websocket_silently(socket, 1000, "debug_close");
    }
}

/// `isWebSocketSessionExpired(entry)`：`Date.now() - entry.createdAt >= 55min`。
///
/// 边界是 `>=`：恰好 55 分钟即视为过期。
fn is_websocket_session_expired_at(created_at: i64, now: i64) -> bool {
    now - created_at >= SESSION_WEBSOCKET_MAX_AGE_MS
}

/// 空闲计时器到期时的缓存操作：只有 id/世代号都匹配且连接仍空闲时才移除。
/// 返回被移除连接的 socket（若有）。
///
/// 空闲计时器是游离任务，触发前连接可能已被再次借出、释放（世代号改变）或替换（id 改变）。
/// 三个条件同时匹配才移除，防止旧计时器关掉正在使用或已经更新的连接。
fn expire_session_websocket_if_current(
    session_id: &str,
    account_id: &str,
    id: u64,
    generation: u64,
) -> Option<WsSocket> {
    let mut cache = websocket_session_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entries = cache.get_mut(session_id)?;
    let remove = match entries.get(account_id) {
        Some(entry) => entry.id == id && entry.expiry_generation == generation && !entry.busy,
        None => false,
    };
    if !remove {
        return None;
    }
    let entry = entries.remove(account_id);
    if entries.is_empty() {
        cache.remove(session_id);
    }
    entry.and_then(|entry| entry.socket)
}

/// 起一个游离任务，5 分钟后尝试回收指定连接；到期时再由
/// `expire_session_websocket_if_current` 校验 id/世代号/空闲状态。
fn schedule_session_websocket_expiry(session_id: &str, account_id: &str, id: u64, generation: u64) {
    let session_id = session_id.to_string();
    let account_id = account_id.to_string();
    crate::api::lazy::spawn_forward(async move {
        tokio::time::sleep(std::time::Duration::from_millis(
            SESSION_WEBSOCKET_CACHE_TTL_MS,
        ))
        .await;
        if let Some(socket) =
            expire_session_websocket_if_current(&session_id, &account_id, id, generation)
        {
            close_websocket_silently(socket, 1000, "idle_timeout");
        }
    });
}

/// `new Error(\`WebSocket connect timeout after ${connectTimeoutMs}ms\`)`。
fn connect_timeout_error(connect_timeout_ms: u64) -> AiError {
    AiError::new(
        AiErrorKind::Transport,
        format!("WebSocket connect timeout after {connect_timeout_ms}ms"),
    )
}

/// 建立 WebSocket 连接（TS `connectWebSocket`）。
///
/// `headers` 逐条尝试写入握手请求，非法的头名/头值被**静默跳过**（不报错）。超时与取消都
/// 作用在 `connect_async` 这个 future 上，因此覆盖 DNS/TCP/TLS/握手全过程；取消优先
/// （`biased` select），`connect_timeout_ms == 0` 表示不设超时。
#[allow(clippy::result_large_err)]
async fn connect_websocket(
    url: &str,
    headers: &[(String, String)],
    signal: Option<&tokio_util::sync::CancellationToken>,
    connect_timeout_ms: Option<u64>,
) -> Result<WsSocket, AiError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url
        .into_client_request()
        .map_err(|error| AiError::new(AiErrorKind::Transport, error.to_string()))?;
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes()),
            tokio_tungstenite::tungstenite::http::HeaderValue::from_str(value),
        ) {
            request.headers_mut().insert(name, value);
        }
    }
    let connect_timeout_ms = connect_timeout_ms.unwrap_or(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS);
    // TS 的 `connectWebSocket` 用 `setTimeout` 包住整个「构造 socket → open 事件」窗口；
    // 超时必须**覆盖握手本身**（`connect_async` 未完成时也要能触发），因此这里把 deadline
    // 施加在 future 上，而不是等它 resolve 之后再套一层（那样永远立即成功）。
    let deadline =
        (connect_timeout_ms > 0).then(|| std::time::Duration::from_millis(connect_timeout_ms));
    let connect = tokio_tungstenite::connect_async(request);
    let result = match signal {
        Some(token) => match deadline {
            Some(deadline) => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(AiError::aborted("Request was aborted")),
                    result = tokio::time::timeout(deadline, connect) => match result {
                        Ok(result) => result,
                        Err(_) => return Err(connect_timeout_error(connect_timeout_ms)),
                    },
                }
            }
            None => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(AiError::aborted("Request was aborted")),
                    result = connect => result,
                }
            }
        },
        None => match deadline {
            Some(deadline) => match tokio::time::timeout(deadline, connect).await {
                Ok(result) => result,
                Err(_) => return Err(connect_timeout_error(connect_timeout_ms)),
            },
            None => connect.await,
        },
    };
    match result {
        Ok((socket, _response)) => Ok(socket),
        Err(error) => Err(AiError::new(AiErrorKind::Transport, error.to_string())),
    }
}

/// 借出结果：`tracking == Some((session, account, id))` 表示 socket 属于缓存、需要归还；
/// `None` 表示一次性连接，归还时直接关闭。
struct AcquiredSocket {
    socket: Option<WsSocket>,
    tracking: Option<(String, String, u64)>,
    reused: bool,
}

/// 借一条可用 WebSocket（TS `acquireWebSocket`）。
///
/// 决策顺序（在缓存锁内一次完成）：
/// 1. 没有 session id → 新建一次性连接（`tracking: None`，不缓存）。
/// 2. 该 `(session, account)` 的连接正 busy → 新建**不缓存**的临时连接。
/// 3. 空闲但年龄 ≥ 55 分钟 → 关闭并删除旧条目，再新建并缓存。
/// 4. 空闲 → 取出 socket、标记 busy、世代号 +1（作废旧空闲计时器），`reused: true`。
///
/// Rust 侧不观察 `readyState`，因此“空闲”即视为可复用（与 TS 在运行时不给 readyState 时
/// 的假定一致，见文件头）。
#[allow(clippy::result_large_err)]
async fn acquire_websocket(
    url: &str,
    headers: &[(String, String)],
    session_id: Option<&str>,
    account_id: &str,
    signal: Option<&tokio_util::sync::CancellationToken>,
    connect_timeout_ms: Option<u64>,
) -> Result<AcquiredSocket, AiError> {
    let Some(session_id) = session_id else {
        let socket = connect_websocket(url, headers, signal, connect_timeout_ms).await?;
        return Ok(AcquiredSocket {
            socket: Some(socket),
            tracking: None,
            reused: false,
        });
    };
    let mut cached_socket: Option<(WsSocket, u64)> = None;
    // TS 的 `acquireWebSocket`：只有「同一账号已有忙碌连接」这一分支会新建一条**永不缓存**
    // 的临时连接。其余落到 connect 的路径（含删除过期条目之后）都会把新连接写入缓存。
    let mut can_cache = true;
    {
        let mut cache = websocket_session_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entries) = cache.get_mut(session_id) {
            if entries.get(account_id).is_some_and(|entry| entry.busy) {
                can_cache = false;
            } else if entries
                .get(account_id)
                .is_some_and(|entry| is_websocket_session_expired_at(entry.created_at, now_ms()))
            {
                // 过期：关闭并删除，然后落到 connect 分支（TS 的下一个 `isWebSocketReusable`
                // 分支会再删一次，这里等价于只删一次）。
                if let Some(removed) = entries.remove(account_id)
                    && let Some(socket) = removed.socket
                {
                    close_websocket_silently(socket, 1000, "connection_age_limit");
                }
                if entries.is_empty() {
                    cache.remove(session_id);
                }
            } else if let Some(entry) = entries.get_mut(account_id)
                && let Some(socket) = entry.socket.take()
            {
                entry.busy = true;
                // 作废上一次释放时安排的空闲计时器（TS 的 `clearTimeout`）。
                entry.expiry_generation += 1;
                let id = entry.id;
                cached_socket = Some((socket, id));
            }
        }
    }
    if let Some((socket, id)) = cached_socket {
        return Ok(AcquiredSocket {
            socket: Some(socket),
            tracking: Some((session_id.to_string(), account_id.to_string(), id)),
            reused: true,
        });
    }
    let socket = connect_websocket(url, headers, signal, connect_timeout_ms).await?;
    let tracking = if can_cache {
        let mut cache = websocket_session_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entries = cache.entry(session_id.to_string()).or_default();
        let id = next_connection_id();
        entries.insert(
            account_id.to_string(),
            CacheEntry {
                id,
                socket: None,
                busy: true,
                created_at: now_ms(),
                continuation: None,
                expiry_generation: 0,
            },
        );
        Some((session_id.to_string(), account_id.to_string(), id))
    } else {
        // 同一账号已有忙碌连接：临时连接不进入缓存（TS 的 `release` 直接关闭）。
        None
    };
    Ok(AcquiredSocket {
        socket: Some(socket),
        tracking,
        reused: false,
    })
}

/// 归还或关闭借出的连接（TS `release({ keep })`）。
///
/// - `tracking == None`（一次性连接）→ 直接关闭，reason `"done"`。
/// - `keep == false` → 删除 id 匹配的缓存条目并关闭 socket；不匹配时只关闭 socket。
/// - `keep == true` → 仅当 id 仍匹配时把 socket 放回并 `busy = false`，世代号 +1 后安排
///   5 分钟空闲回收；id 不匹配说明条目已被替换，socket 随作用域丢弃。
fn release_websocket(acquired: AcquiredSocket, socket: WsSocket, keep: bool) {
    let Some((session_id, account_id, id)) = acquired.tracking else {
        close_websocket_silently(socket, 1000, "done");
        return;
    };
    if !keep {
        let removed = {
            let mut cache = websocket_session_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let removed = match cache.get_mut(&session_id) {
                Some(entries) if entries.get(&account_id).is_some_and(|entry| entry.id == id) => {
                    entries.remove(&account_id)
                }
                _ => None,
            };
            if cache.get(&session_id).is_some_and(HashMap::is_empty) {
                cache.remove(&session_id);
            }
            removed
        };
        let _ = removed;
        close_websocket_silently(socket, 1000, "done");
        return;
    }
    let inserted_generation = {
        let mut cache = websocket_session_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match cache
            .get_mut(&session_id)
            .and_then(|entries| entries.get_mut(&account_id))
        {
            Some(entry) if entry.id == id => {
                entry.socket = Some(socket);
                entry.busy = false;
                // TS 的 `scheduleSessionWebSocketExpiry` 从**本次释放**起重新计时：
                // 先作废旧计时器，再安排新计时器。
                entry.expiry_generation += 1;
                Some(entry.expiry_generation)
            }
            _ => None,
        }
    };
    let Some(generation) = inserted_generation else {
        // socket 已被移入缓存或被丢弃；这里无需再处理。
        return;
    };
    schedule_session_websocket_expiry(&session_id, &account_id, id, generation);
}

/// 取出指定连接 id 的续传状态快照；id 不匹配返回 `None`，避免拿到被替换连接的旧状态。
fn websocket_continuation(session_id: &str, account_id: &str, id: u64) -> Option<Value> {
    let cache = websocket_session_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(session_id)
        .and_then(|entries| entries.get(account_id))
        .filter(|entry| entry.id == id)
        .and_then(|entry| entry.continuation.as_ref())
        .map(|continuation| {
            json!({
                "lastRequestBody": continuation.last_request_body,
                "lastResponseId": continuation.last_response_id,
                "lastResponseItems": continuation.last_response_items,
            })
        })
}

/// 仅当连接 id 仍匹配时写入续传状态（并发下旧连接的结果不会污染新连接）。
fn set_websocket_continuation(
    session_id: &str,
    account_id: &str,
    id: u64,
    continuation: Continuation,
) {
    let mut cache = websocket_session_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = cache
        .get_mut(session_id)
        .and_then(|entries| entries.get_mut(account_id))
        && entry.id == id
    {
        entry.continuation = Some(continuation);
    }
}

/// 仅当连接 id 仍匹配时清除续传状态。
fn clear_websocket_continuation(session_id: &str, account_id: &str, id: u64) {
    let mut cache = websocket_session_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = cache
        .get_mut(session_id)
        .and_then(|entries| entries.get_mut(account_id))
        && entry.id == id
    {
        entry.continuation = None;
    }
}

/// 去掉 `input` 与 `previous_response_id` 后的请求体，用于判断“除增量部分外完全一致”。
fn request_body_without_input(body: &Value) -> Value {
    let mut map = match body {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    map.shift_remove("input");
    map.shift_remove("previous_response_id");
    Value::Object(map)
}

/// 用 JS `JSON.stringify` 的相等性比较两个 input 数组（TS 就是这么写的）。
///
/// 比较因此是**键序敏感**的：`kiana_json::js_object::stringify` 复刻 JS 的键序规则
/// （整数索引键升序在前、其余按插入序），构造请求体时必须保持与上次相同的顺序，否则
/// 会退化为完整上下文而不是增量。
fn response_inputs_equal(left: Option<&Value>, right: Option<&Value>) -> bool {
    let left = left.cloned().unwrap_or_else(|| Value::Array(Vec::new()));
    let right = right.cloned().unwrap_or_else(|| Value::Array(Vec::new()));
    kiana_json::js_object::stringify(&left) == kiana_json::js_object::stringify(&right)
}

/// 计算增量 input（TS `getCachedWebSocketInputDelta`）。
///
/// 只有同时满足三个条件才返回尾部 delta，否则 `None`（调用方回退完整上下文）：
/// 1. 上次请求体除 `input`/`previous_response_id` 外与本次完全一致；
/// 2. 本次 input 不短于 baseline（上次 input + 上次响应 items）；
/// 3. 本次 input 的前缀与 baseline 序列化相等。
fn get_cached_websocket_input_delta(body: &Value, continuation: &Value) -> Option<Value> {
    let last_request_body = obj(continuation, "lastRequestBody")?;
    if kiana_json::js_object::stringify(&request_body_without_input(body))
        != kiana_json::js_object::stringify(&request_body_without_input(last_request_body))
    {
        return None;
    }
    let current_input = match obj(body, "input") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let mut baseline: Vec<Value> = match obj(last_request_body, "input") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    if let Some(Value::Array(items)) = obj(continuation, "lastResponseItems") {
        baseline.extend(items.clone());
    }
    if current_input.len() < baseline.len() {
        return None;
    }
    let prefix = &current_input[..baseline.len()];
    if !response_inputs_equal(
        Some(&Value::Array(prefix.to_vec())),
        Some(&Value::Array(baseline.clone())),
    ) {
        return None;
    }
    Some(Value::Array(current_input[baseline.len()..].to_vec()))
}

/// 把完整请求体改写成增量请求：`previous_response_id` = 上次响应 id，`input` = delta。
///
/// 返回 `None` 表示无法安全增量（delta 计算失败或没有 response id），调用方应发送完整
/// 请求体。`previous_response_id` 由服务端保存的上下文补齐，因此本次只发新增项。
fn build_cached_websocket_request_body(continuation: &Value, body: &Value) -> Option<Value> {
    let delta = get_cached_websocket_input_delta(body, continuation)?;
    let last_response_id = obj(continuation, "lastResponseId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?;
    let mut map = match body {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    map.insert(
        "previous_response_id".into(),
        Value::String(last_response_id.to_string()),
    );
    map.insert("input".into(), delta);
    Some(Value::Object(map))
}

// -- WebSocket 事件读取 ------------------------------------------------------

/// WebSocket 事件读取器（TS `parseWebSocket` 队列状态机的 Rust 版）。
///
/// 状态：
/// - `saw_completion`：已见到 `response.completed` / `response.done` /
///   `response.incomplete`；此后服务端关闭连接或读错误都视为正常结束。
/// - `finished`：不再产出事件。
///
/// 失败路径：未见到终态事件就 Close/EOF/读错误 → `AiErrorKind::Transport`；Close 的
/// code/reason 由 `websocket_close_message` 格式化成 TS 文本。空闲超时按每次读取重新计时，
/// `None` 或 0 表示不设超时。Ping 自动回 Pong；Binary 按 UTF-8 损失解码后与 Text 同样处理。
struct WsEventReader {
    socket: Option<WsSocket>,
    signal: Option<tokio_util::sync::CancellationToken>,
    idle_timeout_ms: Option<u64>,
    saw_completion: bool,
    finished: bool,
}

impl WsEventReader {
    fn new(
        socket: WsSocket,
        signal: Option<tokio_util::sync::CancellationToken>,
        idle_timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            socket: Some(socket),
            signal,
            idle_timeout_ms,
            saw_completion: false,
            finished: false,
        }
    }

    fn is_aborted(&self) -> bool {
        self.signal
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    }

    /// 取回底层 socket（调用后 reader 不再可用），供成功路径放回缓存复用。
    fn into_socket(mut self) -> Option<WsSocket> {
        self.socket.take()
    }

    /// 读下一个完整事件；`Ok(None)` 表示正常结束（已经见过终态事件）。
    #[allow(clippy::result_large_err)]
    async fn next_event(&mut self) -> Result<Option<Value>, AiError> {
        loop {
            if self.finished {
                return Ok(None);
            }
            if self.is_aborted() {
                return Err(AiError::aborted("Request was aborted"));
            }
            let Some(socket) = self.socket.as_mut() else {
                return Ok(None);
            };
            let next = {
                let idle_timeout_ms = self.idle_timeout_ms.filter(|timeout| *timeout > 0);
                let signal = self.signal.clone();
                let read = socket.next();
                match (idle_timeout_ms, signal) {
                    (Some(idle_timeout_ms), Some(token)) => {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => Err(AiError::aborted("Request was aborted")),
                            _ = tokio::time::sleep(std::time::Duration::from_millis(idle_timeout_ms)) => Err(AiError::new(
                                AiErrorKind::Transport,
                                format!("WebSocket idle timeout after {idle_timeout_ms}ms"),
                            )),
                            next = read => Ok(next),
                        }
                    }
                    (Some(idle_timeout_ms), None) => {
                        match tokio::time::timeout(
                            std::time::Duration::from_millis(idle_timeout_ms),
                            read,
                        )
                        .await
                        {
                            Ok(next) => Ok(next),
                            Err(_) => Err(AiError::new(
                                AiErrorKind::Transport,
                                format!("WebSocket idle timeout after {idle_timeout_ms}ms"),
                            )),
                        }
                    }
                    (None, Some(token)) => {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => Err(AiError::aborted("Request was aborted")),
                            next = read => Ok(next),
                        }
                    }
                    (None, None) => Ok(read.await),
                }
            };
            match next {
                Err(error) => {
                    if self.is_aborted() {
                        return Err(AiError::aborted("Request was aborted"));
                    }
                    return Err(error);
                }
                Ok(Some(Ok(message))) => match message {
                    WsMessage::Text(text) => {
                        if let Some(event) = self.handle_text(text.as_str())? {
                            return Ok(Some(event));
                        }
                    }
                    WsMessage::Binary(bytes) => {
                        if let Some(event) = self.handle_text(&String::from_utf8_lossy(&bytes))? {
                            return Ok(Some(event));
                        }
                    }
                    WsMessage::Ping(payload) => {
                        if let Some(socket) = self.socket.as_mut() {
                            let _ = socket.send(WsMessage::Pong(payload)).await;
                        }
                    }
                    WsMessage::Close(frame) => {
                        if self.saw_completion {
                            self.finished = true;
                            return Ok(None);
                        }
                        let code = frame.as_ref().map(|frame| u16::from(frame.code));
                        let reason = frame
                            .as_ref()
                            .map(|frame| frame.reason.to_string())
                            .unwrap_or_default();
                        return Err(AiError::new(
                            AiErrorKind::Transport,
                            websocket_close_message(code, &reason),
                        ));
                    }
                    WsMessage::Pong(_) | WsMessage::Frame(_) => {}
                },
                Ok(Some(Err(error))) => {
                    if self.saw_completion {
                        self.finished = true;
                        return Ok(None);
                    }
                    return Err(AiError::new(AiErrorKind::Transport, error.to_string()));
                }
                Ok(None) => {
                    if self.saw_completion {
                        self.finished = true;
                        return Ok(None);
                    }
                    return Err(AiError::new(
                        AiErrorKind::Transport,
                        "WebSocket stream closed before response.completed",
                    ));
                }
            }
        }
    }

    /// 解析一帧文本并更新完成状态；`Ok(None)` 表示空帧被忽略。
    #[allow(clippy::result_large_err)]
    fn handle_text(&mut self, text: &str) -> Result<Option<Value>, AiError> {
        // TS `parseWebSocket`：`if (!text) return;` —— 空帧静默忽略。
        if text.is_empty() {
            return Ok(None);
        }
        let parsed = match kiana_json::js_parse::parse(text) {
            Ok(value @ Value::Object(_)) => value,
            Ok(value) => value,
            Err(error) => {
                return Err(codex_protocol_error(
                    format!("Invalid Codex WebSocket JSON: {}", error.message),
                    Value::String(text.to_string()),
                ));
            }
        };
        // TS 在同一个 try 里访问 `parsed.type`：JSON null 会抛 TypeError，被包成
        // CodexProtocolError（V8 的消息逐字）。
        if parsed.is_null() {
            return Err(codex_protocol_error(
                "Invalid Codex WebSocket JSON: Cannot read properties of null (reading 'type')"
                    .to_string(),
                Value::String(text.to_string()),
            ));
        }
        if let Some(event_type) = str_of(&parsed, "type")
            && matches!(
                event_type,
                "response.completed" | "response.done" | "response.incomplete"
            )
        {
            self.saw_completion = true;
            self.finished = true;
        }
        Ok(Some(parsed))
    }
}

/// 复刻 TS `extractWebSocketCloseError` 的文本：`WebSocket closed [code] [reason]`。
/// code 缺失时省略；reason 为空且 code 为 1009 时补 `"message too big"`。
fn websocket_close_message(code: Option<u16>, reason: &str) -> String {
    let code_text = code.map(|code| format!(" {code}")).unwrap_or_default();
    let mut reason_text = if reason.is_empty() {
        String::new()
    } else {
        format!(" {reason}")
    };
    if reason_text.is_empty() && code == Some(WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE) {
        reason_text = " message too big".to_string();
    }
    format!("WebSocket closed{code_text}{reason_text}")
}

// -- processWebSocketStream --------------------------------------------------

/// 一次 WebSocket 请求的完整生命周期（TS `processWebSocketStream`）。
///
/// 步骤：借连接 → 选择完整/增量请求体 → 记 debug 统计 → 发送 `response.create` 帧 →
/// 读取并映射事件 → `process_responses_stream` 累积输出 → 归还连接（成功保留，失败/取消
/// 删除缓存条目）。
///
/// 连接归属：`acquired.tracking` 是缓存键；`shared`（`tokio::sync::Mutex<Option<MappedSource>>`）
/// 让事件流消费完之后还能把 socket 从 reader 里取回来，成功时连同 continuation 一起放回缓存。
///
/// 失败清理：发送失败、事件流错误、continuation 构造失败都会清 continuation 并
/// `release(keep=false)`；否则缓存条目会永远停在 `busy = true`。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)]
async fn process_websocket_stream(
    url: &str,
    body: &Value,
    headers: &[(String, String)],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    websocket_started: Arc<std::sync::atomic::AtomicBool>,
    idle_timeout_ms: Option<u64>,
    connect_timeout_ms: Option<u64>,
    cache_session_id: Option<&str>,
    account_id: &str,
    grammar_tool_input_properties: &IndexMap<String, String>,
    options: &OpenAICodexResponsesOptions,
    end_turn: Arc<Mutex<Option<bool>>>,
    start_emitted: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), AiError> {
    let mut acquired = acquire_websocket(
        url,
        headers,
        cache_session_id,
        account_id,
        options.base.signal.as_ref(),
        connect_timeout_ms,
    )
    .await?;
    // 只有 websocket-cached / auto 才做增量续传；显式 websocket 模式始终发完整上下文。
    let use_cached_context = matches!(
        options.base.transport,
        Some(crate::types::Transport::WebsocketCached) | Some(crate::types::Transport::Auto)
    );
    let full_body = body.clone();
    // 缓存的 continuation 只在能安全算出 delta 时使用，否则退回完整请求体。
    let request_body = match (&acquired.tracking, use_cached_context) {
        (Some((session_id, account_id_key, id)), true) => {
            websocket_continuation(session_id, account_id_key, *id)
                .and_then(|continuation| {
                    build_cached_websocket_request_body(&continuation, &full_body)
                })
                .unwrap_or_else(|| full_body.clone())
        }
        _ => full_body.clone(),
    };
    // 统计在发送**之前**记录：即使 send 失败，也要能观察到本次尝试的请求形态。
    let stats_session = cache_session_id;
    update_websocket_debug_stats(stats_session, |stats| {
        stats.requests += 1;
        if acquired.reused {
            stats.connections_reused += 1;
        } else {
            stats.connections_created += 1;
        }
        if use_cached_context {
            stats.cached_context_requests += 1;
        }
        if obj(&request_body, "store") == Some(&Value::Bool(true)) {
            stats.store_true_requests += 1;
        }
        stats.last_input_items = obj(&request_body, "input")
            .and_then(Value::as_array)
            .map_or(0, |items| items.len() as u64);
        if let Some(previous_response_id) =
            str_of(&request_body, "previous_response_id").filter(|id| !id.is_empty())
        {
            stats.delta_requests += 1;
            stats.last_delta_input_items = obj(&request_body, "input")
                .and_then(Value::as_array)
                .map(|items| items.len() as u64);
            stats.last_previous_response_id = Some(previous_response_id.to_string());
        } else {
            stats.full_context_requests += 1;
            stats.last_delta_input_items = None;
            stats.last_previous_response_id = None;
        }
    });

    // 帧格式：`{"type":"response.create", ...请求体}`；请求体字段后写入，若其中含 `type`
    // 会覆盖前者（`build_request_body` 不产出该键，但 `on_payload` 可以整体替换请求体）。
    let frame = {
        let mut map = Map::new();
        map.insert("type".into(), Value::String("response.create".into()));
        if let Value::Object(body) = &request_body {
            for (key, value) in body {
                map.insert(key.clone(), value.clone());
            }
        }
        kiana_json::js_object::stringify(&Value::Object(map))
    };
    let mut socket = acquired.socket.take().expect("acquire returns a socket");
    if let Err(error) = socket.send(WsMessage::Text(frame.into())).await {
        if let Some((session_id, account_id_key, id)) = &acquired.tracking {
            clear_websocket_continuation(session_id, account_id_key, *id);
        }
        // TS：抛出后 catch 清空 continuation、`keepConnection = false`，finally 里的 release
        // 关闭 socket 并**删除缓存条目**。只关闭 socket 会让条目永远停在 busy=true。
        release_websocket(acquired, socket, false);
        return Err(AiError::new(AiErrorKind::Transport, error.to_string()));
    }
    let reader = WsEventReader::new(socket, options.base.signal.clone(), idle_timeout_ms);
    let mapper = CodexEventMapper {
        on_provider_stream_event: options.base.on_provider_stream_event.clone(),
        model: model.clone(),
        end_turn: end_turn.clone(),
    };
    // 首个事件到达时才发出 start（`startWebSocketOutputOnFirstEvent`）。
    let output_snapshot = output.clone();
    let stream_for_events = stream.clone();
    let start_emitted_for_ws = start_emitted.clone();
    let websocket_started_for_ws = websocket_started.clone();
    let source = MappedSource {
        source: Some(CodexEventSource::WebSocket(reader)),
        mapper,
        ended: false,
        started: false,
        on_first_event: Some(Box::new(move || {
            websocket_started_for_ws.store(true, std::sync::atomic::Ordering::SeqCst);
            // TS: `if (!startEmitted) { startEmitted = true; stream.push(start) }` —— 重试
            // （connection-limit / previous-response-not-found）时首次尝试可能已经发过 start，
            // 这里必须检查共享标志，不能无条件 push。
            if !start_emitted_for_ws.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let _ = stream_for_events.push(AssistantMessageEvent::Start {
                    partial: Box::new(output_snapshot.clone()),
                });
            }
        })),
    };
    // 用 `tokio::sync::Mutex` 包住事件源：`process_responses_stream` 消费完（或出错）后，
    // 这里还要从 reader 取回 socket；std Mutex 不能跨 await 持有，所以必须用 async mutex。
    let shared: Arc<tokio::sync::Mutex<Option<MappedSource>>> =
        Arc::new(tokio::sync::Mutex::new(Some(source)));
    let event_stream: crate::api::openai_responses_shared::ProviderEventStream = {
        let shared = shared.clone();
        Box::pin(futures::stream::unfold(shared, |shared| async move {
            let next_state = shared.clone();
            let mut guard = shared.lock().await;
            let source = guard.as_mut()?;
            // 结束/出错时**保留** source：调用方还要从 reader 取回 socket 以便缓存复用
            // （TS 的 async generator 同样在结束后仍持有 socket）。
            match source.next_mapped().await {
                Ok(Some(event)) => Some((Ok(event), next_state)),
                Ok(None) => None,
                Err(error) => Some((Err(error), next_state)),
            }
        }))
    };
    let stream_model_id = model.id.clone();
    let result = process_responses_stream(
        event_stream,
        output,
        stream,
        model,
        Some(&OpenAIResponsesStreamOptions {
            on_provider_stream_event: None,
            service_tier: options.service_tier.clone(),
            grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
            resolve_service_tier: Some(Arc::new(resolve_codex_service_tier)),
            apply_service_tier_pricing: Some(Arc::new(
                move |usage: &mut Usage, service_tier: Option<&str>| {
                    apply_service_tier_pricing(usage, service_tier, &stream_model_id);
                },
            )),
        }),
    )
    .await;

    // 默认保留连接；只有被取消或 continuation 构造失败会改成 false（删除缓存条目）。
    let mut keep_connection = true;
    let socket = {
        let source = shared.lock().await.take();
        match source.and_then(|source| source.source) {
            Some(CodexEventSource::WebSocket(reader)) => reader.into_socket(),
            _ => None,
        }
    };
    match result {
        Ok(()) => {
            if options
                .base
                .signal
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                keep_connection = false;
            } else if use_cached_context
                && let Some((session_id, account_id_key, id)) = &acquired.tracking
                && let Some(response_id) = output
                    .response_id
                    .clone()
                    .filter(|response_id| !response_id.is_empty())
            {
                // 成功且未取消：把本轮 assistant 输出转成 response items 存为下一次的
                // continuation。`function_call_output` / `custom_tool_call_output` 是工具结果
                // 而不是助手产出，必须过滤掉（TS 逐字做法）。
                match convert_responses_messages(
                    model,
                    &normalize_context(&Context {
                        system_prompt: None,
                        messages: vec![Message::Assistant(output.clone())],
                        tools: None,
                    }),
                    &CODEX_TOOL_CALL_PROVIDERS,
                    Some(&ConvertResponsesMessagesOptions {
                        include_system_prompt: Some(false),
                        grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
                        ..Default::default()
                    }),
                ) {
                    Ok(items) => {
                        let filtered: Vec<Value> = items
                            .into_iter()
                            .filter(|item| {
                                !matches!(
                                    str_of(item, "type"),
                                    Some("function_call_output") | Some("custom_tool_call_output")
                                )
                            })
                            .collect();
                        set_websocket_continuation(
                            session_id,
                            account_id_key,
                            *id,
                            Continuation {
                                last_request_body: full_body.clone(),
                                last_response_id: response_id,
                                last_response_items: filtered,
                            },
                        );
                    }
                    Err(error) => {
                        // TS 把转换放在 try 内：抛错会执行 catch（清空 continuation、
                        // keepConnection=false）与 finally 的 release（删除缓存条目），
                        // 错误继续向上传播，而不是带着旧 continuation 继续复用连接。
                        clear_websocket_continuation(session_id, account_id_key, *id);
                        if let Some(socket) = socket {
                            release_websocket(acquired, socket, false);
                        }
                        return Err(error);
                    }
                }
            }
        }
        Err(error) => {
            // 事件流错误：清 continuation 并以 keep=false 归还（删除缓存条目），错误继续
            // 上抛，由上层决定是否回退 SSE 或重试。
            if let Some((session_id, account_id_key, id)) = &acquired.tracking {
                clear_websocket_continuation(session_id, account_id_key, *id);
            }
            if let Some(socket) = socket {
                release_websocket(acquired, socket, false);
            }
            return Err(error);
        }
    }
    if let Some(socket) = socket {
        release_websocket(acquired, socket, keep_connection);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 错误响应解析
// ---------------------------------------------------------------------------

/// `parse_error_response` 的结果：`message` 是错误事件的文本，`friendly_message` 是命中用量
/// 限制时生成的完整友好句（调用方优先用它）。
struct ParsedErrorResponse {
    message: String,
    friendly_message: Option<String>,
}

/// 把非 2xx 响应体解析成用户可见错误（TS `parseErrorResponse`）。
///
/// `status_text` 由调用方从状态码的 canonical reason phrase 得到（`HttpResponse` 不带服务端
/// reason phrase）；空体时用它兜底，再空则 `"Request failed"`。命中用量限制（code 正则或
/// status 429）时生成友好文本
/// `You have hit your ChatGPT usage limit (<plan> plan). Try again in ~N min.`，
/// 其中 plan 小写、N 由 `resets_at`（秒）换算并四舍五入且下限为 0。最终 `message` 优先
/// 服务端 `error.message`，其次友好文本。
fn parse_error_response(raw: &str, status: u16, status_text: &str) -> ParsedErrorResponse {
    let mut message = if !raw.is_empty() {
        raw.to_string()
    } else if !status_text.is_empty() {
        status_text.to_string()
    } else {
        "Request failed".to_string()
    };
    let mut friendly_message = None;
    if let Ok(parsed) = kiana_json::js_parse::parse(raw)
        && let Some(error) = obj(&parsed, "error")
    {
        let code = str_of(error, "code")
            .filter(|code| !code.is_empty())
            .or_else(|| str_of(error, "type"))
            .unwrap_or_default();
        static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
        let is_usage_limit = PATTERN
            .get_or_init(|| {
                regex::Regex::new("(?i)usage_limit_reached|usage_not_included|rate_limit_exceeded")
                    .expect("valid regex")
            })
            .is_match(code)
            || status == 429;
        if is_usage_limit {
            let plan = str_of(error, "plan_type")
                .map(|plan| format!(" ({} plan)", plan.to_lowercase()))
                .unwrap_or_default();
            let mins = obj(error, "resets_at")
                .and_then(Value::as_f64)
                .filter(|resets_at| *resets_at != 0.0)
                .map(|resets_at| {
                    ((resets_at * 1000.0 - now_ms() as f64) / 60000.0)
                        .round()
                        .max(0.0)
                });
            let when = mins
                .map(|mins| format!(" Try again in ~{} min.", js_number_string(mins)))
                .unwrap_or_default();
            let friendly = kiana_json::js_string::js_trim(&format!(
                "You have hit your ChatGPT usage limit{plan}.{when}"
            ))
            .to_string();
            if let Some(error_message) =
                str_of(error, "message").filter(|message| !message.is_empty())
            {
                message = error_message.to_string();
            } else if !friendly.is_empty() {
                message = friendly.clone();
            }
            friendly_message = Some(friendly);
        } else if let Some(error_message) =
            str_of(error, "message").filter(|message| !message.is_empty())
        {
            message = error_message.to_string();
        }
    }
    ParsedErrorResponse {
        message,
        friendly_message,
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

// 覆盖：JWT 账号提取、URL 归一化、头构造顺序与覆盖规则、重试分类与 retry-after、
// 缓存 delta 的前缀匹配、WebSocket 关闭文本、用量限制友好文本、service tier 倍率、
// 空闲计时器世代号、过期条目替换后的缓存、SSE 多行 data 解析。
#[cfg(test)]
mod tests {
    use super::*;

    fn token(account_id: &str) -> String {
        let payload = base64::engine::general_purpose::STANDARD
            .encode(json!({ JWT_CLAIM_PATH: { "chatgpt_account_id": account_id } }).to_string());
        format!("aaa.{payload}.bbb")
    }

    #[test]
    fn extracts_the_chatgpt_account_id() {
        assert_eq!(extract_account_id(&token("acc_test")).unwrap(), "acc_test");
        assert_eq!(
            extract_account_id("not-a-token").unwrap_err().message,
            "Failed to extract accountId from token"
        );
        let bad = base64::engine::general_purpose::STANDARD.encode("{}");
        assert_eq!(
            extract_account_id(&format!("aaa.{bad}.bbb"))
                .unwrap_err()
                .message,
            "Failed to extract accountId from token"
        );
    }

    #[test]
    fn codex_url_resolution() {
        assert_eq!(
            resolve_codex_url(None),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            resolve_codex_url(Some("https://x/backend-api///")),
            "https://x/backend-api/codex/responses"
        );
        assert_eq!(
            resolve_codex_url(Some("https://x/codex")),
            "https://x/codex/responses"
        );
        assert_eq!(
            resolve_codex_url(Some("https://x/codex/responses")),
            "https://x/codex/responses"
        );
        assert_eq!(
            resolve_codex_websocket_url("https://x/backend-api"),
            "wss://x/backend-api/codex/responses"
        );
        assert_eq!(
            resolve_codex_websocket_url("http://127.0.0.1:1"),
            "ws://127.0.0.1:1/codex/responses"
        );
    }

    #[test]
    fn header_builder_order_and_overrides() {
        let defaults = vec![
            ("originator".to_string(), "kiana".to_string()),
            ("User-Agent".to_string(), "ua".to_string()),
        ];
        let model_headers = IndexMap::from([("originator".to_string(), "my-app".to_string())]);
        let caller = ProviderHeaders::from([("user-agent".to_string(), Some("custom".into()))]);
        let headers = build_sse_headers(
            &defaults,
            Some(&model_headers),
            Some(&caller),
            "acc",
            "tok",
            Some("sess"),
        );
        assert_eq!(get_header(&headers, "originator"), Some("my-app"));
        assert_eq!(get_header(&headers, "user-agent"), Some("custom"));
        assert_eq!(get_header(&headers, "Authorization"), Some("Bearer tok"));
        assert_eq!(get_header(&headers, "chatgpt-account-id"), Some("acc"));
        assert_eq!(
            get_header(&headers, "OpenAI-Beta"),
            Some("responses=experimental")
        );
        assert_eq!(get_header(&headers, "session-id"), Some("sess"));
        assert_eq!(get_header(&headers, "x-client-request-id"), Some("sess"));
    }

    #[test]
    fn websocket_headers_drop_accept_and_use_websocket_beta() {
        let defaults = vec![("originator".to_string(), "kiana".to_string())];
        let headers = build_websocket_headers(&defaults, None, None, "acc", "tok", "req");
        assert_eq!(get_header(&headers, "accept"), None);
        assert_eq!(get_header(&headers, "content-type"), None);
        assert_eq!(
            get_header(&headers, "OpenAI-Beta"),
            Some(OPENAI_BETA_RESPONSES_WEBSOCKETS)
        );
        assert_eq!(get_header(&headers, "x-client-request-id"), Some("req"));
        assert_eq!(get_header(&headers, "session-id"), Some("req"));
    }

    #[test]
    fn retry_classification() {
        assert!(is_retryable_error(429, "ordinary rate limit"));
        assert!(!is_retryable_error(429, "insufficient_quota"));
        assert!(is_retryable_error(503, "anything"));
        assert!(is_retryable_error(400, "upstream connect error"));
        assert!(!is_retryable_error(400, "bad request"));
        assert!(is_terminal_rate_limit_error("Monthly usage limit reached"));
    }

    #[test]
    fn retry_delay_validation_message() {
        let error = validate_retry_delay_ms(2000.0, Some(1000)).unwrap_err();
        assert_eq!(error.message, "Server requested 2s retry delay (max: 1s)");
        assert_eq!(validate_retry_delay_ms(500.0, Some(1000)).unwrap(), 500.0);
    }

    #[test]
    fn cached_continuation_delta_matches_prefix() {
        let continuation = json!({
            "lastRequestBody": { "model": "m", "input": [ { "role": "user", "content": "a" } ] },
            "lastResponseId": "resp_1",
            "lastResponseItems": [ { "type": "message", "role": "assistant" } ],
        });
        let body = json!({
            "model": "m",
            "input": [
                { "role": "user", "content": "a" },
                { "type": "message", "role": "assistant" },
                { "role": "user", "content": "b" },
            ],
        });
        let rebuilt = build_cached_websocket_request_body(&continuation, &body).expect("delta");
        assert_eq!(str_of(&rebuilt, "previous_response_id"), Some("resp_1"));
        assert_eq!(
            obj(&rebuilt, "input"),
            Some(&json!([{ "role": "user", "content": "b" }]))
        );
        let different = json!({ "model": "other", "input": [] });
        assert!(build_cached_websocket_request_body(&continuation, &different).is_none());
    }

    #[test]
    fn websocket_close_messages() {
        assert_eq!(
            websocket_close_message(Some(1009), ""),
            "WebSocket closed 1009 message too big"
        );
        assert_eq!(
            websocket_close_message(Some(1000), "bye"),
            "WebSocket closed 1000 bye"
        );
        assert_eq!(websocket_close_message(None, ""), "WebSocket closed");
    }

    #[test]
    fn error_response_friendly_message() {
        let parsed = parse_error_response(
            r#"{"error":{"code":"usage_limit_reached","plan_type":"PLUS","resets_at":9999999999,"message":"limit"}}"#,
            429,
            "",
        );
        assert_eq!(parsed.message, "limit");
        assert!(parsed.friendly_message.as_deref().is_some_and(|message| {
            message.starts_with("You have hit your ChatGPT usage limit (plus plan).")
        }));
        let parsed = parse_error_response("plain failure", 500, "");
        assert_eq!(parsed.message, "plain failure");
        assert!(parsed.friendly_message.is_none());
    }

    #[test]
    fn service_tier_pricing_multipliers() {
        assert_eq!(
            get_service_tier_cost_multiplier("gpt-5.5", Some("flex")),
            0.5
        );
        assert_eq!(
            get_service_tier_cost_multiplier("gpt-5.5", Some("priority")),
            2.5
        );
        assert_eq!(
            get_service_tier_cost_multiplier("other", Some("priority")),
            2.0
        );
        assert_eq!(get_service_tier_cost_multiplier("other", None), 1.0);
        assert_eq!(
            resolve_codex_service_tier(Some("default"), Some("flex")).as_deref(),
            Some("flex")
        );
        assert_eq!(
            resolve_codex_service_tier(Some("priority"), Some("flex")).as_deref(),
            Some("priority")
        );
        assert_eq!(
            resolve_codex_service_tier(None, Some("flex")).as_deref(),
            Some("flex")
        );
    }

    fn cached_entry(session_id: &str, account_id: &str, id: u64, generation: u64, busy: bool) {
        websocket_session_cache()
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .insert(
                account_id.to_string(),
                CacheEntry {
                    id,
                    socket: None,
                    busy,
                    created_at: 0,
                    continuation: None,
                    expiry_generation: generation,
                },
            );
    }

    #[test]
    fn stale_idle_timers_do_not_close_released_connections() {
        let session = "unit-expiry-generation";
        let account = "acc";
        let exists = || {
            websocket_session_cache()
                .lock()
                .unwrap()
                .contains_key(session)
        };

        // 当前世代 + 空闲 → 移除。
        cached_entry(session, account, 7, 1, false);
        let _ = expire_session_websocket_if_current(session, account, 7, 1);
        assert!(!exists());

        // 旧计时器（generation 1）晚到：不得移除 generation 3 的连接。
        cached_entry(session, account, 7, 3, false);
        let _ = expire_session_websocket_if_current(session, account, 7, 1);
        assert!(exists());

        // 忙碌连接不会被空闲计时器移除。
        cached_entry(session, account, 7, 3, true);
        let _ = expire_session_websocket_if_current(session, account, 7, 3);
        assert!(exists());

        // 释放后的最新世代 + 空闲 → 移除。
        cached_entry(session, account, 7, 3, false);
        let _ = expire_session_websocket_if_current(session, account, 7, 3);
        assert!(!exists());
    }

    #[test]
    fn websocket_session_age_limit_is_55_minutes() {
        let created = 1_000_000;
        assert!(!is_websocket_session_expired_at(
            created,
            created + SESSION_WEBSOCKET_MAX_AGE_MS - 1
        ));
        assert!(is_websocket_session_expired_at(
            created,
            created + SESSION_WEBSOCKET_MAX_AGE_MS
        ));
    }

    #[tokio::test]
    async fn caches_the_replacement_connection_after_an_expired_entry() {
        // 本地 WebSocket 服务器：完成握手后保持连接，直到客户端关闭。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while socket.next().await.is_some() {}
                });
            }
        });

        let session = "unit-expired-entry-replacement";
        let account = "acc";
        // `created_at = 0` 必然超过 55 分钟age 上限；TS 删除该条目后仍会缓存替换连接。
        cached_entry(session, account, 7, 0, false);
        let url = format!("ws://{address}/");
        let mut acquired = acquire_websocket(&url, &[], Some(session), account, None, Some(2_000))
            .await
            .expect("acquire");
        assert!(
            acquired.tracking.is_some(),
            "a replacement connection created after deleting an expired entry must be cached"
        );
        assert!(
            websocket_session_cache()
                .lock()
                .unwrap()
                .get(session)
                .is_some_and(|entries| entries.contains_key(account)),
            "the replacement entry must be present while busy"
        );

        let socket = acquired.socket.take().expect("socket");
        release_websocket(acquired, socket, false);
        // keep=false 后条目被删除，且不留下空的 session map。
        assert!(
            !websocket_session_cache()
                .lock()
                .unwrap()
                .contains_key(session)
        );
        server.abort();
    }

    #[test]
    fn sse_reader_parses_multiline_data() {
        let reader = CodexSseReader::new(Box::pin(futures::stream::empty()), None);
        let event = reader
            .parse_chunk("data: {\"a\":\ndata: 1}")
            .expect("parses")
            .expect("event");
        assert_eq!(event, json!({ "a": 1 }));
        // `[DONE]` 与空 data 都跳过。
        assert!(reader.parse_chunk("data: [DONE]").unwrap().is_none());
        assert!(reader.parse_chunk("event: ping").unwrap().is_none());
    }
}
