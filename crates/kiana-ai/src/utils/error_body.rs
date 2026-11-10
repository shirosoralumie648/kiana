//! `utils/error-body.ts`——provider HTTP 错误对象的统一归一化。
//!
//! TS: `packages/ai/src/utils/error-body.ts`。
//!
//! 代理/网关后面的端点可能返回非 2xx 响应，provider SDK 无法把 body 折进 `error.message`；
//! SDK 错误对象仍带着 HTTP 状态与原始/已解析 body，但字段名各不相同。只读 `error.message`
//! 的 provider catch 块因此会丢掉 body，露出 `"403 status code (no body)"` 这种空消息。
//!
//! [`normalize_provider_error`] 探测已知 SDK 字段形状（Mistral、`openai`、`@google/genai`、
//! AWS Bedrock），返回一个各 provider 自行拼进展示文本的结构体。`message_carries_body`
//! 记录 Anthropic / `@google/genai` 的顺风路径（SDK 已把 body 折进 message），provider 可以
//! 保留它而不重复打印。
//!
//! Rust 侧没有 JS 的 `Error` 原型链，因此错误用 [`ProviderErrorValue`] 表达：调用方把 SDK 的
//! 字段填进对应槽位（`status_code` / `status` / `body` / `error_body` / `metadata_status` /
//! `response_status` / `response_body`），本模块只做 TS 里那套探测与拼装。
//!
//! # 处理流程
//!
//! 1. 调用方把 SDK 错误对象映射成 [`ProviderErrorValue`]：`Error` 形状填
//!    [`ProviderErrorFields`]，其它抛出值填 [`ProviderErrorValue::Value`]。
//! 2. [`normalize_provider_error`] 按固定优先级探测 status（4 个槽位）与 body（3 类来源），
//!    对 body 做 JS `trim` 与 UTF-16 截断，并计算 `message_carries_body`。
//! 3. [`format_provider_error`] 决定最终展示文本：message 已含 body、或 status/body 任一
//!    缺失时返回 message（仅当 prefix 与 status 都存在时才在 message 前加
//!    `"<prefix> (<status>): "`）；否则拼 `"<status>: <body>"`（有 prefix 时为
//!    `"<prefix> (<status>): <body>"`）。
//!
//! # 约束与失败模式
//!
//! - 本模块不做 I/O、不重试、不分类错误，只决定「展示什么文本」。网络与重试归
//!   `utils::provider_retry`。
//! - Rust 无法区分 JS 的「普通对象 vs 类实例」：所有 `serde_json::Value::Object` 都按普通
//!   对象处理。SDK 包装类（AWS `$response` 流、类实例 `error` 字段）必须由调用方映射成
//!   [`ResponseBody::Stream`] 或干脆不填，否则会把内部字段序列化成噪声 body
//!   （参见 `tests/error_body.rs` 的模块说明）。
//! - status 是任意 `f64`（含 `NaN`/`±Infinity`）：TS 只检查 `typeof status === "number"`，
//!   本模块保持同一口径，展示时用 JS 的 `String(number)`
//!   （[`kiana_json::js_num::to_radix_string`]）。
//! - `message_carries_body` 的 `includes` 判断使用「trim + 截断后的 body」。body 被截断时，
//!   长 message 可能不再包含它，于是又会拼一次 `status: body`；这是当前稳定契约，改动需同步
//!   测试，不要"顺手优化"。
//!
//! # 调用方
//!
//! `api::{system_one_shared, openrouter_images, llama_cpp_classify, classifier_shared,
//! openai_responses_shared, bedrock_converse_stream}`；`kiana-compaction` 与
//! `kiana-resources` 复用 [`safe_json_stringify`]。

use crate::utils::text::owned_slice;

/// `MAX_PROVIDER_ERROR_BODY_CHARS`。
///
/// 单位是 **UTF-16 code unit**（JS 的 `string.length`），不是字节也不是 Unicode 标量数。
/// 4000 是 TS 侧冻结的上限，改变它会改变错误文本与相关测试期望。
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// TS 的 `unknown` 抛出值：`Error` 形状或任意 JSON。
///
/// 对应 TS 的 `error instanceof Error` 分支：`Error` 走字段探测，`Value` 直接
/// [`safe_json_stringify`] 作为 message（没有 status/body）。
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderErrorValue {
    /// `Error` 子类（带 SDK 可能附加的字段）。
    Error(ProviderErrorFields),
    /// 非 `Error` 的抛出值：`safeJsonStringify(error)`。
    Value(serde_json::Value),
}

/// SDK 错误对象上被探测的字段（TS 的 `SdkErrorShape`）。
///
/// 这些字段名来自不同 SDK（Mistral / openai / `@google/genai` / AWS Bedrock），同一个错误
/// 通常只有其中一两个有值。`None` 表示字段缺失**或类型不符**（例如 status 是字符串）；
/// `Some(NaN)`/`Some(inf)` 仍是合法值，因为 TS 只检查 `typeof === "number"`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderErrorFields {
    /// `error.message`。
    pub message: String,
    /// `statusCode`（Mistral）。
    pub status_code: Option<f64>,
    /// `status`（`openai`、`@google/genai`）。
    pub status: Option<f64>,
    /// `body` 字符串（Mistral）。
    pub body: Option<String>,
    /// `error`：`openai` SDK 的已解析 JSON body。
    pub error_body: Option<serde_json::Value>,
    /// `$metadata.httpStatusCode`（Bedrock）。
    pub metadata_status_code: Option<f64>,
    /// `$response.statusCode`（Bedrock）。
    pub response_status_code: Option<f64>,
    /// `$response.body`：字符串、可读流（`pipe`）或普通对象。
    pub response_body: Option<ResponseBody>,
}

/// `$response.body` 的三种可观察形态。
///
/// TS 用「是否有 `pipe` 方法」「原型是否为 `Object.prototype`」来区分流/类实例与普通对象；
/// Rust 侧没有原型链，调用方负责把不可识别的形态归到 [`ResponseBody::Stream`]
/// （语义等同「没有可用 body 文本」）。
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseBody {
    /// 字符串 body。
    Text(String),
    /// 可读流（有 `pipe` 方法）：视为没有 body。
    Stream,
    /// 已解析的 JSON body（只有**普通**非空对象才算）。
    Json(serde_json::Value),
}

/// `NormalizedProviderError`。
///
/// 由 [`normalize_provider_error`] 产出；各 provider 只负责把结果拼进展示文本（或加自己的
/// 前缀），不要再手工判断字段。`status`/`body` 只是从错误对象上提取到的信息，缺少它们并不
/// 意味着请求没有失败。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NormalizedProviderError {
    /// 能从 SDK 错误对象里提取到的 HTTP 状态码。
    ///
    /// TS 侧只要 `typeof status === "number"` 就保留原值，因此这里同样是任意 `Number`：
    /// 非整数（`403.5`）、越界（`70000`）、负数乃至 `NaN`/`±Infinity` 都原样保留，
    /// 展示时按 JS 的 `String(number)` 格式化（见 [`format_provider_error`]）。
    pub status: Option<f64>,
    /// 原始 HTTP body，已 trim 并截断到上限。
    pub body: Option<String>,
    /// `error.message`；非 `Error` 抛出时是 `safeJsonStringify(error)`。
    pub message: String,
    /// `message` 已包含 body（无需再单独附加）。
    pub message_carries_body: bool,
}

/// `normalizeProviderError(error)`。
///
/// # 执行顺序
///
/// 1. 非 `Error`（[`ProviderErrorValue::Value`]）：只产出 `message`（JSON 文本），
///    `status`/`body` 为 `None`、`message_carries_body` 为 `false`，随后直接返回。
/// 2. `Error`：按 [`ProviderErrorFields`] 的字段探测 status 与 body（各自的优先级见
///    `extract_status` / `pick_body_text`）。
/// 3. `message_carries_body = body.is_none() || message.contains(body)`：body 缺失时置
///    `true`，避免 [`format_provider_error`] 无谓地拼接。
///
/// 本函数不会失败，也不会修改输入。注意它对 `ProviderErrorFields` 的探测是「第一个可用
/// 命中」，不会把多个字段的 body 拼接起来。
pub fn normalize_provider_error(error: &ProviderErrorValue) -> NormalizedProviderError {
    let fields = match error {
        ProviderErrorValue::Value(value) => {
            return NormalizedProviderError {
                message: safe_json_stringify(value),
                message_carries_body: false,
                ..Default::default()
            };
        }
        ProviderErrorValue::Error(fields) => fields,
    };
    let status = extract_status(fields);
    let body = extract_body(fields);
    // body 缺失时置 true：否则 format_provider_error 会试图拼一个不存在的 body。
    let message_carries_body = match &body {
        None => true,
        Some(body) => fields.message.contains(body.as_str()),
    };
    NormalizedProviderError {
        status,
        body,
        message: fields.message.clone(),
        message_carries_body,
    }
}

/// JS 的 `typeof value === "number"`：Rust 侧 `Option<f64>` 的 `Some` 已经表示「存在且是
/// `Number`」，任何取值（非整数、越界、`NaN`、`±Infinity`）都不做收窄。
fn status_from(value: Option<f64>) -> Option<f64> {
    value
}

/// `extractStatus(error)`：`statusCode` → `status` → `$metadata.httpStatusCode` →
/// `$response.statusCode`，首个数值命中。
fn extract_status(fields: &ProviderErrorFields) -> Option<f64> {
    status_from(fields.status_code)
        .or_else(|| status_from(fields.status))
        .or_else(|| status_from(fields.metadata_status_code))
        .or_else(|| status_from(fields.response_status_code))
}

/// `extractBody(error)`：选中文本后 trim，空则没有，最后截断到上限。
///
/// 选中与 trim 是两步：`pick_body_text` 一旦返回某个来源的文本（哪怕全是空白），trim 后
/// 为空就直接当作「没有 body」，**不会**回头尝试下一个来源。这是 TS 的原始行为。
fn extract_body(fields: &ProviderErrorFields) -> Option<String> {
    let body_text = pick_body_text(fields)?;
    let trimmed = kiana_json::js_string::js_trim(&body_text);
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS))
}

/// `pickBodyText(error)`：`body` 字符串 → `error` 普通非空对象 → `$response.body`。
///
/// 只有 `body` 字符串是「存在即选中」：哪怕空串也直接返回，后面的 `error`/`$response.body`
/// 不再看（随后 `extract_body` 会把它 trim 成空并当作没有 body，且不再回退）。
/// `error`/`$response.body` 的 JSON 形态还要求是普通非空对象：空对象与流会被跳过，并继续
/// 尝试下一个来源。
fn pick_body_text(fields: &ProviderErrorFields) -> Option<String> {
    if let Some(body) = &fields.body {
        return Some(body.clone());
    }
    if let Some(error_body) = &fields.error_body
        && is_plain_non_empty_object(error_body)
    {
        return Some(safe_json_stringify(error_body));
    }
    match &fields.response_body {
        Some(ResponseBody::Text(text)) => Some(text.clone()),
        Some(ResponseBody::Stream) => None,
        Some(ResponseBody::Json(value)) if is_plain_non_empty_object(value) => {
            Some(safe_json_stringify(value))
        }
        Some(ResponseBody::Json(_)) | None => None,
    }
}

/// `isPlainNonEmptyObject(value)`：只有普通且非空的对象才算 HTTP body。
///
/// Rust 侧所有 `serde_json::Value::Object` 都对应 JS 的普通对象；SDK 的类实例由调用方映射成
/// [`ResponseBody::Stream`] 或其它形态，不会走到这里。空对象返回 `false`：TS 明确要求空对象
/// 不算 body，否则展示文本会出现无意义的 `"{}"`。
fn is_plain_non_empty_object(value: &serde_json::Value) -> bool {
    matches!(value, serde_json::Value::Object(map) if !map.is_empty())
}

/// `formatProviderError(norm, prefix?)`。
///
/// - 无前缀：`"<status>: <body>"`
/// - 有前缀：`"<prefix> (<status>): <body>"`
///
/// # 分支规则
///
/// - 若 `message_carries_body`、或 status/body 任一缺失：返回 message（不走 body 拼接）；
///   此时**只有**「有 prefix 且 status 存在」才会在 message 前加 `"<prefix> (<status>): "`。
///   换句话说，status 缺失时 prefix 会被丢弃——这是 TS 的行为，测试
///   `serializes_non_error_throws` 锁定了「非 Error 抛出即使给前缀也只得到 message」。
/// - 否则按是否给 prefix 选择 `"<prefix> (<status>): <body>"` 或 `"<status>: <body>"`。
///
/// status 用 [`kiana_json::js_num::to_radix_string`] 格式化（等价 JS 模板字符串里的
/// `String(number)`，`NaN`/`Infinity`/小数/大整数都按 JS 文本输出），不能用 Rust 的
/// `f64: Display` 代替。
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    // TS 的模板字符串内插 `${status}` 等价于 `String(number)`（含 `NaN`/`Infinity`/指数形式），
    // 不是 Rust 的 `f64: Display`。
    let status = |status: f64| kiana_json::js_num::to_radix_string(status, 10);
    if norm.message_carries_body || norm.status.is_none() || norm.body.is_none() {
        return match (prefix, norm.status) {
            (Some(prefix), Some(status_value)) => {
                format!("{prefix} ({}): {}", status(status_value), norm.message)
            }
            _ => norm.message.clone(),
        };
    }
    let status_value = norm.status.expect("checked");
    let body = norm.body.as_deref().expect("checked");
    match prefix {
        Some(prefix) => format!("{prefix} ({}): {body}", status(status_value)),
        None => format!("{}: {body}", status(status_value)),
    }
}

/// `truncateErrorText(text, maxChars)`：按 UTF-16 码元截断并附截断说明。
///
/// `maxChars` 与 `text` 的长度都按 JS 的 `string.length`（UTF-16 码元）计。超长时返回
/// `"<前 maxChars 个码元>... [truncated <超出数> chars]"`；未超长时原样返回，不追加说明。
///
/// # 边界
///
/// 前缀由 [`owned_slice`] 生成，它不会把代理对切成两半：若第 `maxChars` 个码元正好是代理对
/// 的前半，切片会多包含一个码元。因此返回的可见前缀可能比 `maxChars` 长 1，`truncated`
/// 的计数仍按 JS 的 `length - maxChars` 计算（本文件的单元测试
/// `truncates_bodies_by_utf16_units` 用 `"😀😀"` 锁定了代理对不被切开这一点）。
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let length = text.encode_utf16().count();
    if length <= max_chars {
        return text.to_string();
    }
    format!(
        "{}... [truncated {} chars]",
        owned_slice(text, 0, Some(max_chars)),
        length - max_chars
    )
}

/// `safeJsonStringify(value)`：`JSON.stringify`；`undefined` 时 `String(value)`。
///
/// Rust 的 `serde_json::Value` 不含 `undefined`，也不会因循环引用抛错，因此这里没有 TS 的
/// 两条回退分支（结果为 `undefined` → `String(value)`、抛错 → `String(value)`）。
/// `Null` 单独处理成 `"null"`；其余值走 [`kiana_json::js_object::stringify`]，保证数字格式
/// 与对象键序和 JS 一致。
pub fn safe_json_stringify(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        other => kiana_json::js_object::stringify(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error(fields: ProviderErrorFields) -> ProviderErrorValue {
        ProviderErrorValue::Error(fields)
    }

    #[test]
    fn keeps_the_message_when_it_carries_the_body() {
        let norm = normalize_provider_error(&error(ProviderErrorFields {
            message: "400 {\"error\":{\"message\":\"bad\"}}".into(),
            status: Some(400.0),
            body: Some("{\"error\":{\"message\":\"bad\"}}".into()),
            ..Default::default()
        }));
        assert!(norm.message_carries_body);
        assert_eq!(format_provider_error(&norm, None), norm.message);
        assert_eq!(
            format_provider_error(&norm, Some("Anthropic")),
            format!("Anthropic (400): {}", norm.message)
        );
    }

    #[test]
    fn surfaces_the_body_when_the_message_drops_it() {
        let norm = normalize_provider_error(&error(ProviderErrorFields {
            message: "403 status code (no body)".into(),
            status_code: Some(403.0),
            body: Some("  forbidden by policy  ".into()),
            ..Default::default()
        }));
        assert_eq!(norm.status, Some(403.0));
        assert_eq!(norm.body.as_deref(), Some("forbidden by policy"));
        assert_eq!(
            format_provider_error(&norm, None),
            "403: forbidden by policy"
        );
        assert_eq!(
            format_provider_error(&norm, Some("Mistral")),
            "Mistral (403): forbidden by policy"
        );
    }

    #[test]
    fn probes_sdk_status_fields_in_order() {
        let fields = ProviderErrorFields {
            message: "x".into(),
            status_code: Some(401.0),
            status: Some(402.0),
            metadata_status_code: Some(403.0),
            response_status_code: Some(404.0),
            ..Default::default()
        };
        assert_eq!(extract_status(&fields), Some(401.0));
        let fields = ProviderErrorFields {
            message: "x".into(),
            status_code: None,
            status: Some(402.0),
            metadata_status_code: Some(403.0),
            response_status_code: Some(404.0),
            ..Default::default()
        };
        assert_eq!(extract_status(&fields), Some(402.0));
    }

    #[test]
    fn keeps_any_js_number_status_and_formats_it_like_string_number() {
        // TS 只检查 `typeof status === "number"`，非整数与越界值原样保留并内插。
        let norm = normalize_provider_error(&error(ProviderErrorFields {
            message: "403 status code (no body)".into(),
            status: Some(403.5),
            body: Some("blocked".into()),
            ..Default::default()
        }));
        assert_eq!(norm.status, Some(403.5));
        assert_eq!(format_provider_error(&norm, None), "403.5: blocked");
        assert_eq!(
            format_provider_error(&norm, Some("OpenAI API error")),
            "OpenAI API error (403.5): blocked"
        );

        let norm = normalize_provider_error(&error(ProviderErrorFields {
            message: "Unknown: UnknownError".into(),
            status_code: Some(70_000.0),
            body: Some("gateway".into()),
            ..Default::default()
        }));
        assert_eq!(norm.status, Some(70_000.0));
        assert_eq!(format_provider_error(&norm, None), "70000: gateway");

        // `NaN`/`Infinity` 也是 `typeof "number"`：模板字符串写作 "NaN"/"Infinity"。
        let nan = NormalizedProviderError {
            status: Some(f64::NAN),
            body: Some("b".into()),
            message: "m".into(),
            message_carries_body: false,
        };
        assert_eq!(format_provider_error(&nan, None), "NaN: b");
        let infinity = NormalizedProviderError {
            status: Some(f64::INFINITY),
            body: Some("b".into()),
            message: "m".into(),
            message_carries_body: false,
        };
        assert_eq!(
            format_provider_error(&infinity, Some("P")),
            "P (Infinity): b"
        );
    }

    #[test]
    fn ignores_empty_objects_and_streams_as_bodies() {
        let norm = normalize_provider_error(&error(ProviderErrorFields {
            message: "Input is too long".into(),
            status: Some(400.0),
            error_body: Some(json!({})),
            response_body: Some(ResponseBody::Stream),
            ..Default::default()
        }));
        assert_eq!(norm.body, None);
        assert!(norm.message_carries_body);
        assert_eq!(format_provider_error(&norm, None), "Input is too long");
    }

    #[test]
    fn uses_parsed_json_bodies() {
        let norm = normalize_provider_error(&error(ProviderErrorFields {
            message: "openai error".into(),
            status: Some(400.0),
            error_body: Some(json!({"error": "boom"})),
            ..Default::default()
        }));
        assert_eq!(norm.body.as_deref(), Some("{\"error\":\"boom\"}"));
    }

    #[test]
    fn serializes_non_error_throws() {
        let norm = normalize_provider_error(&ProviderErrorValue::Value(json!({"code": "x"})));
        assert_eq!(norm.message, "{\"code\":\"x\"}");
        assert!(!norm.message_carries_body);
        assert_eq!(format_provider_error(&norm, Some("P")), "{\"code\":\"x\"}");
    }

    #[test]
    fn truncates_bodies_by_utf16_units() {
        assert_eq!(truncate_error_text("abc", 5), "abc");
        assert_eq!(
            truncate_error_text("abcdef", 3),
            "abc... [truncated 3 chars]"
        );
        // 不切开代理对：`😀` 占两个 UTF-16 码元。JS 的 `slice(0, 3)` 会留下一个孤立高代理项；
        // Rust 的 `String` 表示不了，`owned_slice` 因此多取一个码元（`ARCHITECTURE.md` §8 第 31 条），
        // 于是这里得到完整的第二个 emoji。
        let text = "😀😀";
        assert_eq!(truncate_error_text(text, 3), "😀😀... [truncated 1 chars]");
        let text = "😀😀😀";
        assert_eq!(truncate_error_text(text, 3), "😀😀... [truncated 3 chars]");
    }
}
