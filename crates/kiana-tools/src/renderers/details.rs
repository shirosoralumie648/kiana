//! `result.details` 的宽松读取。
//!
//! TS 渲染器直接做属性访问（`result.details?.truncation?.truncated`）：缺字段、字段类型不对
//! 或 details 来自旧版本 kiana / 扩展时，属性只是 `undefined`，渲染照常进行；`??` 回落到默认
//! 上限。Rust 若把 details 整体反序列化成 `*ToolDetails`，任何一个必需字段缺失都会让整个
//! details 变成 `None`，警告行与 bash 的 `Full output:` 脚注处理全部消失。
//!
//! 这里按 JS 的逐字段访问实现同一语义：
//! - `undefined` 在模板字符串里是 `"undefined"`（[`text`]）；
//! - 真值判定用 [`kiana_json::js_string::is_truthy`]；
//! - `??` 只对 `null`/`undefined` 回落到默认值，其余值走 JS `ToNumber`。
//!
//! # 边界
//! 本模块是 `pub(crate)`，只服务 `renderers` 下的渲染器（bash/find/grep/ls/read）；它不
//! 构造 `*ToolDetails` 结构体、也不校验 schema，因此 details 新增字段时不需要改这里，
//! 只要渲染器按需读取即可。
//!
//! # 注意事项
//! - 所有 `truncation_*` 辅助函数把「字段缺失」「字段为 `null`」「`truncation` 不是对象」
//!   统一视为 `undefined`，由调用方决定是否渲染警告行。
//! - 数值/计数类字段先做 JS 真值判断：`0`、`""`、`NaN` 都是 falsy，例如
//!   `entryLimitReached: 0` 不会产生警告（`ls` 的测试锁定该行为）。
//! - `maxBytes` 缺失/null 时回落到 `DEFAULT_MAX_BYTES`（50 KiB），渲染为 `50.0KB`；
//!   其它值走 `to_number`，非数字（对象/数组等）得到 `NaN`，最终文案是 `NaNMB`。

use kiana_ai::types::JsonValue;

use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

/// `details?.[key]`：`details` 不是对象时为 `undefined`。
pub(crate) fn property<'a>(details: Option<&'a JsonValue>, key: &str) -> Option<&'a JsonValue> {
    details.and_then(JsonValue::as_object)?.get(key)
}

/// `details?.truncation?.[key]`：`truncation` 不是对象时为 `undefined`。
pub(crate) fn truncation_field<'a>(
    details: Option<&'a JsonValue>,
    key: &str,
) -> Option<&'a JsonValue> {
    property(details, "truncation")
        .and_then(JsonValue::as_object)?
        .get(key)
}

/// JS 真值（`if (value)`）。
pub(crate) fn truthy(value: Option<&JsonValue>) -> bool {
    kiana_json::js_string::is_truthy(value)
}

/// 模板字符串的 `${value}` / `String(value)`；`undefined` 写成 `"undefined"`。
pub(crate) fn text(value: Option<&JsonValue>) -> String {
    kiana_json::js_string::js_string(value)
}

/// `details?.truncation?.truncated`。
pub(crate) fn truncation_truncated(details: Option<&JsonValue>) -> bool {
    truthy(truncation_field(details, "truncated"))
}

/// `details?.truncation?.[key]` 的真值（`firstLineExceedsLimit` 等布尔字段）。
pub(crate) fn truncation_truthy(details: Option<&JsonValue>, key: &str) -> bool {
    truthy(truncation_field(details, key))
}

/// `${details?.truncation?.[key]}`。
pub(crate) fn truncation_text(details: Option<&JsonValue>, key: &str) -> String {
    text(truncation_field(details, key))
}

/// `details?.truncation?.truncatedBy === "lines"`。
pub(crate) fn truncated_by_lines(details: Option<&JsonValue>) -> bool {
    truncation_field(details, "truncatedBy").and_then(JsonValue::as_str) == Some("lines")
}

/// `${details?.truncation?.maxLines ?? DEFAULT_MAX_LINES}`。
///
/// 缺失或 `null` 时返回 `DEFAULT_MAX_LINES`（2000）的十进制文本；其它值（包括字符串/
/// 布尔）按 JS `String(value)` 输出，不做数值校验。
pub(crate) fn truncation_max_lines_text(details: Option<&JsonValue>) -> String {
    match truncation_field(details, "maxLines") {
        None | Some(JsonValue::Null) => DEFAULT_MAX_LINES.to_string(),
        Some(value) => text(Some(value)),
    }
}

/// `formatSize(details?.truncation?.maxBytes ?? DEFAULT_MAX_BYTES)`。
///
/// 缺失或 `null` 时用 `DEFAULT_MAX_BYTES`（50 KiB）并渲染为 `50.0KB`；其余值先按 JS
/// `ToNumber` 转换再格式化（`< 1024` 用 `B`，`< 1 MiB` 用 `KB`，否则 `MB`，后两者保留
/// 一位小数）。字符串 `"51200"` 这类数字文本会按数字处理。
pub(crate) fn truncation_max_bytes_size(details: Option<&JsonValue>) -> String {
    let bytes = match truncation_field(details, "maxBytes") {
        None | Some(JsonValue::Null) => DEFAULT_MAX_BYTES as f64,
        Some(value) => to_number(value),
    };
    format_size_number(bytes)
}

/// JS `ToNumber`（`formatSize` 的实参）。
fn to_number(value: &JsonValue) -> f64 {
    match value {
        JsonValue::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        JsonValue::Bool(boolean) => {
            if *boolean {
                1.0
            } else {
                0.0
            }
        }
        JsonValue::String(string) => kiana_json::js_num::parse_js_number(string),
        _ => f64::NAN,
    }
}

/// JS `formatSize(bytes: number)`（`bytes` 可以是任意 `ToNumber` 结果）。
fn format_size_number(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{}B", number_text(bytes))
    } else if bytes < 1024.0 * 1024.0 {
        format!(
            "{}KB",
            kiana_ai::model_catalog::js_to_fixed(bytes / 1024.0, 1)
        )
    } else {
        format!(
            "{}MB",
            kiana_ai::model_catalog::js_to_fixed(bytes / (1024.0 * 1024.0), 1)
        )
    }
}

/// JS `String(number)`：与 `JSON.stringify` 的区别只在非有限值。
fn number_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        kiana_json::js_num::format_js_number(value)
    }
}
