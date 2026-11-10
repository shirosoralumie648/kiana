//! Provider 错误分类与可重试判定。
//!
//! # 分类
//!
//! [`ProviderErrorClass`] 把一次 provider 失败归到七类之一：`Auth`、`RateLimit`、
//! `ContextWindow`、`Quota`、`InvalidRequest`、`Transient`、`Unknown`。Agent 循环用分类
//! 决定「重试 / 压缩上下文 / 换模型 / 直接失败」，因此分类必须是保守的：
//!
//! - typed 字段（调用方已有的 status/code/分类/isRetryable）优先；
//! - 没有 typed 信息时对错误 JSON 做**深度 8** 的结构遍历，收集 status/code/message；
//! - `429` 或 `rate limit` 文案**绝不**被归为 `ContextWindow`（限流常提到 token）；
//! - quota/billing 文案优先于 transient：`insufficient_quota` + HTTP 429 是 `Quota`，
//!   不是可重试的 `RateLimit`。
//!
//! # 可重试
//!
//! 1. typed `is_retryable` 有值时直接采用（它覆盖状态码集合）；
//! 2. 否则只按状态码集合 `{408,409,425,429,500,502,503,504,529}` 判定；
//! 3. 完全没有状态码时才允许**一次** message fallback：OpenRouter 的
//!    `"Provider returned error"`。其它错误不会因为消息里出现 "timeout" 等词就被重试，
//!    避免把用户的请求错误无限重试。
//!
//! 设计参考 cline `providers/error-classification.ts`（typed pre-pass → 深度 8 signal
//! walk → 定额判定；429 否决 context、quota 优先）与 `providers/model-operations.ts`
//! 的 fail-closed 风格；实现按 Kiana 的 `serde_json::Value` 重写。

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorClass {
    /// 缺少/无效凭据（401/403、invalid_api_key）。
    Auth,
    /// 限流（429、rate limit）。可重试。
    RateLimit,
    /// 输入超过上下文窗口。应压缩或换模型，不重试。
    ContextWindow,
    /// 配额/账单耗尽。换模型或充值，不重试。
    Quota,
    /// 请求本身非法（400/422、schema 校验失败）。不重试。
    InvalidRequest,
    /// 瞬时故障（超时、5xx、连接中断）。可重试。
    Transient,
    /// 证据不足。
    Unknown,
}

impl ProviderErrorClass {
    /// 稳定字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderErrorClass::Auth => "auth",
            ProviderErrorClass::RateLimit => "rate_limit",
            ProviderErrorClass::ContextWindow => "context_window",
            ProviderErrorClass::Quota => "quota",
            ProviderErrorClass::InvalidRequest => "invalid_request",
            ProviderErrorClass::Transient => "transient",
            ProviderErrorClass::Unknown => "unknown",
        }
    }

    /// 该类错误是否值得原样重试（未考虑 typed `isRetryable`）。
    ///
    /// 只有限流与瞬时故障为真；上下文溢出应压缩，配额/鉴权/非法请求重试也不会成功。
    pub fn default_retryable(self) -> bool {
        matches!(
            self,
            ProviderErrorClass::RateLimit | ProviderErrorClass::Transient
        )
    }
}

/// 调用方已有的 typed 错误字段。所有字段都可缺省。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypedProviderError<'a> {
    /// 已经是结构化分类时直接采用（最高优先级）。
    pub classification: Option<ProviderErrorClass>,
    /// HTTP 状态码。
    pub status: Option<u16>,
    /// provider 的错误码（如 `context_length_exceeded`、`invalid_api_key`）。
    pub code: Option<&'a str>,
    /// 错误消息。
    pub message: Option<&'a str>,
    /// provider/SDK 给出的可重试标志；有值时覆盖状态码集合。
    pub is_retryable: Option<bool>,
}

impl<'a> TypedProviderError<'a> {
    /// 只带分类的构造。
    pub fn classified(classification: ProviderErrorClass) -> Self {
        TypedProviderError {
            classification: Some(classification),
            ..Self::default()
        }
    }

    /// 只带 HTTP 状态的构造。
    pub fn status(status: u16) -> Self {
        TypedProviderError {
            status: Some(status),
            ..Self::default()
        }
    }
}

/// 默认可重试状态码集合（typed `isRetryable` 缺席时使用）。
pub const RETRYABLE_STATUS_CODES: [u16; 9] = [408, 409, 425, 429, 500, 502, 503, 504, 529];

/// 状态码是否在默认可重试集合内。
pub fn is_retryable_status(status: u16) -> bool {
    RETRYABLE_STATUS_CODES.contains(&status)
}

/// 结构遍历的最大深度（根为 0，depth > 8 停止）。
pub const MAX_WALK_DEPTH: u8 = 8;

const CONTEXT_STATUSSES: [u16; 3] = [400, 413, 422];
const AUTH_STATUSSES: [u16; 2] = [401, 403];

fn regex_set(patterns: &[&'static str]) -> RegexSet {
    RegexSet::new(patterns).expect("valid provider error regex set")
}

/// 上下文窗口文案。
fn context_patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        regex_set(&[
            r"(?i)\bcontext\s*(?:length|window|limit)\b",
            r"(?i)\bmaximum\s*context\b",
            r"(?i)\b(?:input\s*)?tokens?\s+exceeds?\b",
            r"(?i)\btoo\s*many\s*tokens?\b",
            r"(?i)\binput\s+is\s+too\s+long\b",
            r"(?i)\bprompt\s+is\s+too\s+long\b",
            r"(?i)reduce\s+the\s+length\s+of\s+the\s+messages",
            r"(?i)\brequested\s+input\s+length\b.*\bexceed",
            r"(?i)\bexceeds?\s+(?:the\s+)?(?:model'?s\s+)?maximum\s+context\s+length\b",
            r"(?i)\bcontext_length_exceeded\b",
            r"(?i)\bmodel_context_window_exceeded\b",
            r"(?i)\bprompt\s+exceeds\s+max\s+length\b",
            r"(?i)\binput\s+is\s+too\s+long\s+for\s+requested\s+model\b",
            r"(?i)\bexceeds\s+(?:the\s+)?available\s+context\s+size\b",
            r"(?i)\bgreater\s+than\s+the\s+context\s+length\b",
            r"(?i)\btoken\s+limit\s+exceeded\b",
        ])
    })
}

/// 限流文案（否决上下文判断）。
fn rate_limit_patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        regex_set(&[
            r"(?i)\brate[\s_-]?limit\b",
            r"(?i)\btoo\s+many\s+requests\b",
            r"(?i)\bper[\s_-]?minute\b",
            r"(?i)\brequests?\s+per\s+(?:second|minute)\b",
            r"(?i)\bthrottl",
        ])
    })
}

/// 配额/账单文案（优先于 transient，也优先于 rate limit）。
fn quota_patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        regex_set(&[
            r"(?i)\binsufficient[_ ]quota\b",
            r"(?i)\bquota\s+(?:exceeded|exhausted|reached|depleted)\b",
            r"(?i)\bbilling\b",
            r"(?i)\bout\s+of\s+budget\b",
            r"(?i)\bavailable\s+balance\b",
            r"(?i)\bcredit\s+balance\b",
            r"(?i)\bpayment\s+required\b",
            r"(?i)\bsubscription\b.*\b(?:limit|exceeded|unavailable)\b",
            r"(?i)\binsufficient\s+balance\b",
        ])
    })
}

/// 鉴权文案。
fn auth_patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        regex_set(&[
            r"(?i)\binvalid[_ ]api[_ ]key\b",
            r"(?i)\bincorrect\s+api\s+key\b",
            r"(?i)\bunauthori[sz]ed\b",
            r"(?i)\bforbidden\b",
            r"(?i)\bauthentication\b",
            r"(?i)\binvalid\s+bearer\b",
            r"(?i)\bmissing\s+(?:api\s+key|credentials)\b",
        ])
    })
}

/// 非法请求文案。
fn invalid_request_patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        regex_set(&[
            r"(?i)\bmalformed\b",
            r"(?i)\binvalid\s+request\b",
            r"(?i)\binvalid[_ ](?:parameter|argument|value|tool)\b",
            r"(?i)\b(?:tool|function)\b.*\bschema\b.*\b(?:invalid|malformed|missing)\b",
            r"(?i)\bschema\b.*\b(?:invalid|malformed)\b",
            r"(?i)\bvalidation\b.*\b(?:failed|error)\b",
            r"(?i)\bunsupported\s+(?:parameter|field|value)\b",
        ])
    })
}

/// 瞬时故障文案（只在完全没有状态码时用于分类；不参与重试判定）。
fn transient_patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        regex_set(&[
            r"(?i)\boverloaded\b",
            r"(?i)\bserver[_ ]busy\b",
            r"(?i)\btemporarily\s+unavailable\b",
            r"(?i)\bservice\s+unavailable\b",
            r"(?i)\btime[d]?\s*out\b",
            r"(?i)\bconnection\s+(?:reset|refused|lost|closed)\b",
            r"(?i)\binternal\s+(?:server\s+)?error\b",
            r"(?i)\bprovider\s+returned\s+error\b",
            r"(?i)\bupstream\s+(?:request\s+)?failed\b",
        ])
    })
}

fn any_match(patterns: &RegexSet, text: &str) -> bool {
    patterns.is_match(text)
}

fn matches_any_message(patterns: &RegexSet, messages: &[String]) -> bool {
    messages.iter().any(|message| any_match(patterns, message))
}

fn is_context_code(code: &str) -> bool {
    matches!(
        code,
        "context_length_exceeded" | "context_window_exceeded" | "model_context_window_exceeded"
    )
}

fn is_quota_code(code: &str) -> bool {
    matches!(
        code,
        "insufficient_quota"
            | "billing_hard_limit_reached"
            | "insufficient_balance"
            | "quota_exceeded"
            | "payment_required"
    )
}

fn is_auth_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_api_key" | "authentication_error" | "unauthorized" | "invalid_credentials"
    )
}

fn is_invalid_request_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_request_error"
            | "invalid_request"
            | "validation_error"
            | "invalid_parameter"
            | "invalid_tool_schema"
    )
}

/// 分类一个已经有 typed 字段的错误。
///
/// 顺序（先命中先返回）：
/// 1. 显式 `classification`；
/// 2. Auth 状态（401/403）；
/// 3. quota/billing（状态或文案）；
/// 4. 429 / rate-limit 文案 → `RateLimit`（**否决**上下文判断）；
/// 5. 上下文 code，或上下文文案 + 允许的状态（无状态或 400/413/422）→ `ContextWindow`；
/// 6. 可重试状态或任意 5xx → `Transient`；
/// 7. 其它 4xx / invalid-request code → `InvalidRequest`；
/// 8. 无状态时的鉴权/瞬时文案；
/// 9. `Unknown`。
pub fn classify_typed(error: &TypedProviderError<'_>) -> ProviderErrorClass {
    if let Some(classification) = error.classification {
        return classification;
    }

    let status = error.status;
    let code = error.code.unwrap_or_default();
    let message = error.message.unwrap_or_default();
    let messages = if message.is_empty() {
        Vec::new()
    } else {
        vec![message.to_string()]
    };

    if status.is_some_and(|status| AUTH_STATUSSES.contains(&status)) {
        return ProviderErrorClass::Auth;
    }
    if is_quota_code(code) || any_match(quota_patterns(), message) {
        return ProviderErrorClass::Quota;
    }
    if status == Some(429) || any_match(rate_limit_patterns(), message) {
        return ProviderErrorClass::RateLimit;
    }
    if is_context_code(code) {
        return ProviderErrorClass::ContextWindow;
    }
    let context_status_allowed = status.is_none_or(|status| CONTEXT_STATUSSES.contains(&status));
    if context_status_allowed
        && (any_match(context_patterns(), message)
            || matches_any_message(context_patterns(), &messages))
    {
        return ProviderErrorClass::ContextWindow;
    }
    if let Some(status) = status {
        if is_retryable_status(status) || (500..=599).contains(&status) {
            return ProviderErrorClass::Transient;
        }
        if (400..=499).contains(&status) {
            return ProviderErrorClass::InvalidRequest;
        }
    }
    if is_auth_code(code) || any_match(auth_patterns(), message) {
        return ProviderErrorClass::Auth;
    }
    if is_invalid_request_code(code) || any_match(invalid_request_patterns(), message) {
        return ProviderErrorClass::InvalidRequest;
    }
    if any_match(transient_patterns(), message) {
        return ProviderErrorClass::Transient;
    }
    ProviderErrorClass::Unknown
}

/// 结构遍历收集到的信号。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderErrorSignals {
    /// 所有字符串（含从 JSON 字符串里解出的嵌套对象）。
    pub messages: Vec<String>,
    /// HTTP 状态码（数值或三位数字符串）。
    pub statuses: BTreeSet<u16>,
    /// `code`/`type`/`name` 字段。
    pub codes: BTreeSet<String>,
}

impl ProviderErrorSignals {
    fn push_status(&mut self, value: &Value) {
        let status = match value {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse::<u64>().ok(),
            _ => None,
        };
        if let Some(status) = status.filter(|status| (100..=599).contains(status)) {
            self.statuses.insert(status as u16);
        }
    }

    fn push_code(&mut self, value: &Value) {
        if let Value::String(text) = value {
            let text = text.trim();
            if !text.is_empty() {
                self.codes.insert(text.to_string());
            }
        }
    }

    fn push_message(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.messages.push(text.to_string());
    }
}

/// 从字符串里挖出嵌入的 `"code": 400` / `"status": 429`。
fn embedded_status(text: &str) -> Option<u16> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r#""(?:code|status)"\s*:\s*"?(\d{3})"?"#).expect("valid embedded status regex")
    });
    pattern
        .captures(text)
        .and_then(|captures| captures.get(1))
        .and_then(|matched| matched.as_str().parse::<u16>().ok())
        .filter(|status| (100..=599).contains(status))
}

const STATUS_KEYS: [&str; 3] = ["status", "statusCode", "code"];
const CODE_KEYS: [&str; 3] = ["code", "type", "name"];
const DETAIL_KEYS: [&str; 10] = [
    "message",
    "detail",
    "error_message",
    "error",
    "errors",
    "cause",
    "responseBody",
    "response_body",
    "data",
    "value",
];

fn collect_signals(value: &Value, signals: &mut ProviderErrorSignals, depth: u8) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
        Value::String(text) => {
            signals.push_message(text);
            // provider/gateway 常把上游错误 JSON 塞进 message 字符串。
            if let Ok(parsed) = serde_json::from_str::<Value>(text)
                && parsed.is_object()
            {
                collect_signals(&parsed, signals, depth + 1);
            } else if let Some(status) = embedded_status(text) {
                signals.statuses.insert(status);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_signals(item, signals, depth + 1);
            }
        }
        Value::Object(object) => {
            for key in STATUS_KEYS {
                if let Some(status) = object.get(key) {
                    signals.push_status(status);
                }
            }
            for key in CODE_KEYS {
                if let Some(code) = object.get(key) {
                    signals.push_code(code);
                }
            }
            for key in DETAIL_KEYS {
                if let Some(nested) = object.get(key) {
                    collect_signals(nested, signals, depth + 1);
                }
            }
        }
    }
}

/// 对任意 JSON 错误值做结构遍历并返回信号（供诊断/测试）。
pub fn collect_provider_error_signals(value: &Value) -> ProviderErrorSignals {
    let mut signals = ProviderErrorSignals::default();
    collect_signals(value, &mut signals, 0);
    signals
}

fn verdict_from_signals(signals: &ProviderErrorSignals) -> ProviderErrorClass {
    if signals
        .statuses
        .iter()
        .any(|status| AUTH_STATUSSES.contains(status))
    {
        return ProviderErrorClass::Auth;
    }
    if signals.codes.iter().any(|code| is_quota_code(code))
        || matches_any_message(quota_patterns(), &signals.messages)
    {
        return ProviderErrorClass::Quota;
    }
    if signals.statuses.contains(&429)
        || matches_any_message(rate_limit_patterns(), &signals.messages)
    {
        return ProviderErrorClass::RateLimit;
    }
    if signals.codes.iter().any(|code| is_context_code(code)) {
        return ProviderErrorClass::ContextWindow;
    }
    let context_status_allowed = signals.statuses.is_empty()
        || signals
            .statuses
            .iter()
            .any(|status| CONTEXT_STATUSSES.contains(status));
    if context_status_allowed && matches_any_message(context_patterns(), &signals.messages) {
        return ProviderErrorClass::ContextWindow;
    }
    if signals
        .statuses
        .iter()
        .any(|status| is_retryable_status(*status) || (500..=599).contains(status))
    {
        return ProviderErrorClass::Transient;
    }
    if signals
        .statuses
        .iter()
        .any(|status| (400..=499).contains(status))
    {
        return ProviderErrorClass::InvalidRequest;
    }
    if signals.codes.iter().any(|code| is_auth_code(code))
        || matches_any_message(auth_patterns(), &signals.messages)
    {
        return ProviderErrorClass::Auth;
    }
    if signals
        .codes
        .iter()
        .any(|code| is_invalid_request_code(code))
        || matches_any_message(invalid_request_patterns(), &signals.messages)
    {
        return ProviderErrorClass::InvalidRequest;
    }
    if matches_any_message(transient_patterns(), &signals.messages) {
        return ProviderErrorClass::Transient;
    }
    ProviderErrorClass::Unknown
}

/// 分类任意 JSON 错误值（先看 typed 字段位置，再做深度 8 结构遍历）。
pub fn classify_provider_error(value: &Value) -> ProviderErrorClass {
    verdict_from_signals(&collect_provider_error_signals(value))
}

/// typed 字段优先、否则回落到 JSON 结构遍历。
pub fn classify_provider_error_with_typed(
    typed: Option<&TypedProviderError<'_>>,
    value: &Value,
) -> ProviderErrorClass {
    if let Some(typed) = typed
        && (typed.classification.is_some()
            || typed.status.is_some()
            || typed.code.is_some()
            || typed.message.is_some())
    {
        let classified = classify_typed(typed);
        if classified != ProviderErrorClass::Unknown {
            return classified;
        }
    }
    classify_provider_error(value)
}

/// 可重试判定的唯一 message fallback（OpenRouter 的无状态 "Provider returned error"）。
fn provider_returned_error(message: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r"(?i)provider\s+returned\s+error").expect("valid provider returned regex")
    });
    pattern.is_match(message)
}

fn retryable_from_signals(signals: &ProviderErrorSignals) -> bool {
    if signals
        .statuses
        .iter()
        .any(|status| AUTH_STATUSSES.contains(status))
    {
        return false;
    }
    if signals.codes.iter().any(|code| is_quota_code(code)) {
        return false;
    }
    if signals.statuses.contains(&429) {
        return true;
    }
    if signals.codes.iter().any(|code| is_context_code(code)) {
        return false;
    }
    if signals
        .statuses
        .iter()
        .any(|status| is_retryable_status(*status))
    {
        return true;
    }
    if signals
        .statuses
        .iter()
        .any(|status| (400..=499).contains(status))
    {
        return false;
    }
    if !signals.statuses.is_empty() {
        // 有状态码但不在集合内（例如 501）：保守地不重试。
        return false;
    }
    // 没有状态码时只允许一次 message fallback（OpenRouter 的无状态错误）。分类结果里
    // 的 RateLimit/Transient 文案不参与重试判定，避免把「timeout」等词无限重试。
    signals
        .messages
        .iter()
        .any(|message| provider_returned_error(message))
}

/// 是否应重试。typed `is_retryable` 有值时直接采用；否则按状态码集合，最后只做一次
/// message fallback。
pub fn is_retryable_provider_error(typed: Option<&TypedProviderError<'_>>, value: &Value) -> bool {
    if let Some(typed) = typed
        && let Some(is_retryable) = typed.is_retryable
    {
        return is_retryable;
    }

    let mut signals = collect_provider_error_signals(value);
    if let Some(typed) = typed {
        if let Some(status) = typed.status {
            signals.statuses.insert(status);
        }
        if let Some(code) = typed.code {
            signals.codes.insert(code.to_string());
        }
        if let Some(message) = typed.message {
            signals.push_message(message);
        }
    }
    retryable_from_signals(&signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn http_429_with_context_text_is_rate_limit_not_context_window() {
        let value = json!({
            "status": 429,
            "message": "Rate limit reached: request exceeds the context length"
        });
        assert_eq!(
            classify_provider_error(&value),
            ProviderErrorClass::RateLimit
        );
        // 分类驱动压缩：RateLimit 不触发压缩；重试仍允许。
        assert!(is_retryable_provider_error(None, &value));
    }

    #[test]
    fn typed_401_and_invalid_api_key_are_auth() {
        let typed = TypedProviderError {
            status: Some(401),
            code: Some("invalid_api_key"),
            message: Some("Incorrect API key provided"),
            ..TypedProviderError::default()
        };
        assert_eq!(classify_typed(&typed), ProviderErrorClass::Auth);
        assert!(!is_retryable_provider_error(
            Some(&typed),
            &json!({"message": "Incorrect API key provided"})
        ));
    }

    #[test]
    fn malformed_tool_schema_is_invalid_request() {
        let value = json!({
            "status": 400,
            "message": "malformed tool schema: tools[0].input_schema.type is required"
        });
        assert_eq!(
            classify_provider_error(&value),
            ProviderErrorClass::InvalidRequest
        );
        assert!(!is_retryable_provider_error(None, &value));
    }

    #[test]
    fn quota_wins_over_rate_limit_and_is_not_retryable() {
        let value = json!({
            "status": 429,
            "code": "insufficient_quota",
            "message": "You exceeded your current quota, please check your plan and billing details"
        });
        assert_eq!(classify_provider_error(&value), ProviderErrorClass::Quota);
        assert!(!is_retryable_provider_error(None, &value));
    }

    #[test]
    fn retryable_status_codes_follow_the_fixed_set() {
        for status in RETRYABLE_STATUS_CODES {
            let value = json!({ "status": status });
            assert!(
                is_retryable_provider_error(None, &value),
                "status {status} should retry"
            );
            let expected = if status == 429 {
                ProviderErrorClass::RateLimit
            } else {
                ProviderErrorClass::Transient
            };
            assert_eq!(classify_provider_error(&value), expected);
        }
        for status in [400u16, 401, 403, 404, 422, 501] {
            let value = json!({ "status": status });
            assert!(
                !is_retryable_provider_error(None, &value),
                "status {status} should not retry"
            );
        }
    }

    #[test]
    fn typed_retryable_flag_overrides_the_status_set() {
        let value = json!({ "status": 503, "message": "service unavailable" });
        let typed = TypedProviderError {
            status: Some(503),
            is_retryable: Some(false),
            ..TypedProviderError::default()
        };
        assert!(!is_retryable_provider_error(Some(&typed), &value));

        let typed = TypedProviderError {
            status: Some(400),
            is_retryable: Some(true),
            ..TypedProviderError::default()
        };
        assert!(is_retryable_provider_error(Some(&typed), &value));
    }

    #[test]
    fn status_529_and_503_are_retryable() {
        assert!(is_retryable_provider_error(
            None,
            &json!({"status": 529, "message": "overloaded"})
        ));
        assert!(is_retryable_provider_error(
            None,
            &json!({"status": 503, "message": "service unavailable"})
        ));
    }

    #[test]
    fn structural_walk_finds_nested_status_and_code_at_depth_eight() {
        let mut value = json!({ "message": "container" });
        for _ in 0..MAX_WALK_DEPTH {
            value = json!({ "error": value });
        }
        // 深度 8 的嵌套里放一个 429 与 rate limit 文案（message 字符串在 depth 8）。
        let mut exact = json!({"status": 429, "message": "rate limit reached"});
        for _ in 0..(MAX_WALK_DEPTH - 1) {
            exact = json!({"error": exact});
        }
        let _ = value;
        assert_eq!(
            classify_provider_error(&exact),
            ProviderErrorClass::RateLimit
        );
        assert!(is_retryable_provider_error(None, &exact));

        // 超过深度 8 的 429 不会被看到。
        let mut too_deep = json!({"status": 429});
        for _ in 0..(MAX_WALK_DEPTH + 1) {
            too_deep = json!({"error": too_deep});
        }
        let signals = collect_provider_error_signals(&too_deep);
        assert!(signals.statuses.is_empty());
        assert!(!is_retryable_provider_error(None, &too_deep));
    }

    #[test]
    fn embedded_json_messages_are_parsed_for_signals() {
        let inner = json!({
            "error": { "code": "context_length_exceeded", "message": "too many tokens" }
        })
        .to_string();
        let value = json!({ "message": inner });
        assert_eq!(
            classify_provider_error(&value),
            ProviderErrorClass::ContextWindow
        );
        assert!(!is_retryable_provider_error(None, &value));
    }

    #[test]
    fn rate_limit_text_vetoes_a_context_code() {
        // typed code 说 context，但同一结构里有 429：429 胜出。
        let value = json!({
            "status": 429,
            "code": "context_length_exceeded",
            "message": "rate limit: too many tokens per minute"
        });
        assert_eq!(
            classify_provider_error(&value),
            ProviderErrorClass::RateLimit
        );
    }

    #[test]
    fn unknown_message_falls_back_once_to_provider_returned_error() {
        let value = json!({ "message": "Provider returned error" });
        assert!(is_retryable_provider_error(None, &value));
        // 其它无状态消息不因关键词被重试。
        let value = json!({ "message": "timeout while validating your request" });
        assert!(!is_retryable_provider_error(None, &value));
    }

    #[test]
    fn typed_classification_wins_over_structural_signals() {
        let typed = TypedProviderError::classified(ProviderErrorClass::ContextWindow);
        let value = json!({ "status": 503, "message": "service unavailable" });
        assert_eq!(
            classify_provider_error_with_typed(Some(&typed), &value),
            ProviderErrorClass::ContextWindow
        );
    }
}
