//! `utils/diagnostics.ts`——把抛出的任意值转成诊断信息。
//!
//! TS 里 `formatThrownValue` 是 `ModelsError` 消息拼接与 provider 错误包装的共用底层：
//! - `Error` → `message || name`；
//! - `string` → 原样；
//! - 其它 → `String(value)`（`"[object Object]"`、数组逗号连接、`"null"`、`"undefined"`）。
//!
//! Rust 侧 [`ThrownValue`] 覆盖同一组可观察形态（错误 / 字符串 / JSON 值），
//! [`format_thrown_value`] 对 JSON 值走 [`kiana_json::js_string::js_string`]（JS 的 `String(v)`）。
//!
//! TS: `utils/diagnostics.ts`
//!
//! # 为什么单独抽一层
//!
//! TS 的 `catch (error: unknown)` 可能抛出任意值；把 cause 拼进 `ModelsError` 的 message、
//! 以及在 assistant 消息上记录诊断，都需要同一套「任意值 → 文本 / 结构体」的规则。集中到
//! 本模块可以避免每个调用点各自 `format!` 造成文本不一致。
//!
//! # 调用方
//!
//! - [`format_thrown_value`]：`utils::models_error::with_cause_detail` 与 `auth::types` 的
//!   错误 cause 拼接（只取文本）。
//! - [`extract_diagnostic_error`] / [`create_assistant_message_diagnostic`] /
//!   [`append_assistant_message_diagnostic`]：`api::kiana_messages` 把 provider 响应失败
//!   记入 assistant 消息的 `diagnostics`（只用于会话记录与调试，不参与模型请求）。
//!
//! # 映射边界
//!
//! Rust 侧没有 JS 的 `undefined`：`ThrownValue::Json(Null)` 对应 `throw null`，但
//! `String(undefined)` 这一分支无法表达。数组/对象按 [`kiana_json::js_string::js_string`]
//! 渲染（对象一律 `"[object Object]"`，数组按逗号连接，不做深序列化）。结构化诊断使用
//! [`crate::types::DiagnosticErrorInfo`]；而从 [`crate::error::AiError`] 转换时只有
//! `name`/`message` 可用，`stack`/`code` 恒为 `None`。

use serde_json::Value;

use crate::types::{AssistantMessageDiagnostic, DiagnosticErrorInfo, JsonObject};

/// TS 的 `unknown` 抛出值在 Rust 侧的可观察形态。
///
/// 三个变体分别对应 TS 的三条分支：`instanceof Error`、`typeof value === "string"`、
/// 其余值（走 `String(value)`）。只有 [`ThrownValue::Error`] 能携带结构化的
/// `stack`/`code`；后两者在 `extractDiagnosticError` 里统一折成 `name = "ThrownValue"`。
#[derive(Debug, Clone, PartialEq)]
pub enum ThrownValue {
    /// `Error` 实例（含 `name` / `message` / `stack` / 可选 `code`）。
    Error(DiagnosticErrorInfo),
    /// 字符串。
    Text(String),
    /// 任意 JSON 值（非 `Error`、非字符串）。
    ///
    /// 覆盖 `null`、数字、布尔、数组与对象；渲染时按 JS 的 `String(v)`，对象不展开字段。
    Json(Value),
}

impl ThrownValue {
    /// 从 [`crate::error::AiError`] 构造。
    ///
    /// `name` 总是 `Some`（`AiError.name` 默认 `"Error"`；即使调用方用 `with_name("")`
    /// 把它置空，这里也保留空串而不会变成 `None`）；`AiError` 没有 `stack`/`code` 字段，
    /// 因此这两项恒为 `None`。
    pub fn from_ai_error(error: &crate::error::AiError) -> Self {
        ThrownValue::Error(DiagnosticErrorInfo {
            name: Some(error.name.clone()),
            message: error.message.clone(),
            stack: None,
            code: None,
        })
    }

    /// 从字符串构造（`throw "x"`）。
    ///
    /// 空串也原样保留：JS 里字符串分支直接返回原值，不做 truthiness 回退。
    pub fn from_text(text: impl Into<String>) -> Self {
        ThrownValue::Text(text.into())
    }

    /// 从 JSON 值构造（`throw {reason:"boom"}`）。
    ///
    /// 注意 `Json` 只是「非 Error、非字符串」的容器；渲染成文本时对象会变成
    /// `"[object Object]"`，结构化字段不会出现在 message 里。
    pub fn from_json(value: Value) -> Self {
        ThrownValue::Json(value)
    }
}

impl From<&crate::error::AiError> for ThrownValue {
    fn from(error: &crate::error::AiError) -> Self {
        ThrownValue::from_ai_error(error)
    }
}

impl From<crate::error::AiError> for ThrownValue {
    fn from(error: crate::error::AiError) -> Self {
        ThrownValue::from_ai_error(&error)
    }
}

/// `formatThrownValue(value)`：错误取 `message || name`，字符串原样，其余 `String(value)`。
///
/// # 规则与边界
///
/// - `Error` 分支用 JS 的 `||` 语义：`message` 为空串时回退到 `name`（`name` 缺失则得到
///   空串）。非空 message 优先，即使 name 更长。
/// - `Text` 原样返回，不 trim、不转义。
/// - `Json` 走 [`kiana_json::js_string::js_string`]：对象与嵌套对象一律 `"[object Object]"`，
///   数组按逗号连接，`null` → `"null"`，数字按 JS `String(number)` 格式化。
/// - 本函数只产出展示文本；需要保留 `stack`/`code` 时用 [`extract_diagnostic_error`]。
///
/// TS: `utils/diagnostics.ts#formatThrownValue`
pub fn format_thrown_value(value: &ThrownValue) -> String {
    match value {
        ThrownValue::Error(info) => {
            if info.message.is_empty() {
                info.name.clone().unwrap_or_default()
            } else {
                info.message.clone()
            }
        }
        ThrownValue::Text(text) => text.clone(),
        ThrownValue::Json(json) => kiana_json::js_string::js_string(Some(json)),
    }
}

/// `extractDiagnosticError(error)`。
///
/// 非 `Error` → `{ name: "ThrownValue", message: formatThrownValue(error) }`；
/// `Error` → `{ name: error.name || undefined, message: error.message || error.name,
/// stack: error.stack, code: 仅当 string|number }`。
///
/// # 与 TS 的逐项对应
///
/// - `name`：空串按 JS 的 `||` 过滤成 `None`（TS 的 `undefined`）。
/// - `message`：`Error.message` 为空时回退到 `error.name`；两者都空时是空串（TS 也一样，
///   `message` 字段必存在）。
/// - `code`：只有字符串/数字保留；布尔、`null`、对象、数组一律丢弃（测试锁定）。
/// - `stack`：原样透传，Rust 侧调用方通常没有。
/// - 非 `Error` 分支的 `name` 固定为 `"ThrownValue"`，message 由 [`format_thrown_value`] 产生。
///
/// TS: `utils/diagnostics.ts#extractDiagnosticError`
pub fn extract_diagnostic_error(error: &ThrownValue) -> DiagnosticErrorInfo {
    match error {
        ThrownValue::Error(info) => {
            let name = info.name.clone().filter(|name| !name.is_empty());
            let message = if info.message.is_empty() {
                info.name.clone().unwrap_or_default()
            } else {
                info.message.clone()
            };
            DiagnosticErrorInfo {
                name,
                message,
                stack: info.stack.clone(),
                code: info
                    .code
                    .clone()
                    .filter(|code| code.is_string() || code.is_number()),
            }
        }
        other => DiagnosticErrorInfo {
            name: Some("ThrownValue".into()),
            message: format_thrown_value(other),
            stack: None,
            code: None,
        },
    }
}

/// `createAssistantMessageDiagnostic(type, error, details?)`。
///
/// `timestamp` 用当前 Unix 毫秒（`Date.now()`）。`kind` 对应 TS 的 `type` 字段（Rust 的
/// [`AssistantMessageDiagnostic`] 用 `#[serde(rename = "type")]` 写出）。`error` 总是
/// `Some`：`extractDiagnosticError` 对任意抛出值都会产生结构体；`details` 原样透传。
///
/// 时间戳来自系统时钟：若时钟早于 UNIX_EPOCH（不应发生），内部回退为 0。
///
/// TS: `utils/diagnostics.ts#createAssistantMessageDiagnostic`
pub fn create_assistant_message_diagnostic(
    kind: &str,
    error: &ThrownValue,
    details: Option<JsonObject>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        kind: kind.to_string(),
        timestamp: now_unix_ms(),
        error: Some(extract_diagnostic_error(error)),
        details,
    }
}

/// `appendAssistantMessageDiagnostic(message, diagnostic)`：不可变追加。
///
/// TS 的泛型版本作用于 `{ diagnostics?: ... }`；Rust 侧直接作用于
/// [`AssistantMessage`](crate::types::AssistantMessage)。
///
/// TS 用 `[...(old ?? []), d]` 生成新数组再赋回；Rust 侧改为对 `&mut` 就地 push，
/// 可观察结果相同（`diagnostics` 为 `None` 时先创建空 `Vec`），只是不再保留旧数组快照。
///
/// TS: `utils/diagnostics.ts#appendAssistantMessageDiagnostic`
pub fn append_assistant_message_diagnostic(
    message: &mut crate::types::AssistantMessage,
    diagnostic: AssistantMessageDiagnostic,
) {
    let diagnostics = message.diagnostics.get_or_insert_with(Vec::new);
    diagnostics.push(diagnostic);
}

// 时钟早于 UNIX_EPOCH 时返回 0 而不是 panic：时间戳仅用于诊断展示，缺失精度可以接受。
fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_errors_strings_and_values() {
        assert_eq!(
            format_thrown_value(&ThrownValue::Error(DiagnosticErrorInfo {
                name: Some("TypeError".into()),
                message: "boom".into(),
                stack: None,
                code: None,
            })),
            "boom"
        );
        // message 为空时回退到 name。
        assert_eq!(
            format_thrown_value(&ThrownValue::Error(DiagnosticErrorInfo {
                name: Some("EmptyError".into()),
                message: String::new(),
                stack: None,
                code: None,
            })),
            "EmptyError"
        );
        assert_eq!(format_thrown_value(&ThrownValue::Text("raw".into())), "raw");
        assert_eq!(
            format_thrown_value(&ThrownValue::Json(serde_json::json!({"reason": "boom"}))),
            "[object Object]"
        );
        assert_eq!(format_thrown_value(&ThrownValue::Json(Value::Null)), "null");
        assert_eq!(
            format_thrown_value(&ThrownValue::Json(serde_json::json!([1, 2]))),
            "1,2"
        );
    }

    #[test]
    fn extracts_diagnostic_errors() {
        let info = extract_diagnostic_error(&ThrownValue::Text("raw".into()));
        assert_eq!(info.name.as_deref(), Some("ThrownValue"));
        assert_eq!(info.message, "raw");
        assert!(info.stack.is_none());

        let info = extract_diagnostic_error(&ThrownValue::Error(DiagnosticErrorInfo {
            name: Some("ApiError".into()),
            message: "failed".into(),
            stack: Some("stack".into()),
            code: Some(serde_json::json!(401)),
        }));
        assert_eq!(info.name.as_deref(), Some("ApiError"));
        assert_eq!(info.message, "failed");
        assert_eq!(info.stack.as_deref(), Some("stack"));
        assert_eq!(info.code, Some(serde_json::json!(401)));

        // code 非 string/number 时被丢弃。
        let info = extract_diagnostic_error(&ThrownValue::Error(DiagnosticErrorInfo {
            name: None,
            message: "x".into(),
            stack: None,
            code: Some(serde_json::json!({"a": 1})),
        }));
        assert!(info.code.is_none());
        assert!(info.name.is_none());
    }

    #[test]
    fn appends_diagnostics_immutably() {
        let mut message = crate::types::AssistantMessage::default();
        assert!(message.diagnostics.is_none());
        append_assistant_message_diagnostic(
            &mut message,
            create_assistant_message_diagnostic("stream", &ThrownValue::Text("x".into()), None),
        );
        append_assistant_message_diagnostic(
            &mut message,
            create_assistant_message_diagnostic("parse", &ThrownValue::Text("y".into()), None),
        );
        let diagnostics = message.diagnostics.expect("diagnostics");
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].kind, "stream");
        assert_eq!(diagnostics[1].kind, "parse");
    }
}
