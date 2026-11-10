//! TS `number` 字段的 serde 辅助（`ARCHITECTURE.md` §4.3）。
//!
//! JS 的 `number` 不区分整数与浮点，`JSON.stringify(2)` 是 `2`；而 serde 的 `f64`
//! 默认走 ryu，会把 `2.0` 写成 `2.0`、把 `1e-6` 写成 `1e-6`（JS 是 `0.000001`）、
//! 把 `9.3e18` 写成 `9.3e18`（JS 是 `9300000000000000000`）。因此本 crate 里所有
//! `f64` 字段都经 [`kiana_json::js_num::format_js_number`]（ECMAScript `Number::toString`）
//! 生成文本，再用 [`serde_json::value::RawValue`] 原样写出。
//!
//! `NaN`/`±Infinity` 按 `JSON.stringify` 写成 `null`；`-0` 写成 `0`。
//! 反序列化不需要辅助：serde_json 会把 JSON 数字读入 `f64`（与 JS 的双精度语义一致）。
//!
//! # 执行流程
//!
//! 1. 结构体字段上的 `#[serde(serialize_with = "js_number::serialize_f64")]`
//!    （或 `js_number::serialize_option`）在写出该字段时被调用；
//! 2. [`kiana_json::js_num::format_js_number`] 生成 ECMAScript `Number::toString` 文本
//!    （含 `-0` → `"0"`、非有限值 → `"null"`）；
//! 3. 文本被包成 [`serde_json::value::RawValue`]，再由 serde_json 原样写入。
//!    不能先把文本解析成 `serde_json::Number`：那会再次走 serde_json/ryu 的格式化，
//!    `0.000001234` 会被写成 `1.234e-6`（`kiana-ai` 的 auth 序列化注释记录了同一问题）。
//!
//! # 约束与失败模式
//!
//! - `js_number::serialize_f64`/`js_number::serialize_option` 是 `pub(crate)`，只服务本
//!   crate 的 `f64` 字段；反序列化不经过它们（serde_json 读 `f64` 的语义与 JS 双精度一致）。
//! - [`serde_json::value::RawValue::from_string`] 只接受**单个合法 JSON 值**；
//!   `format_js_number` 的输出保证满足这一点，所以正常路径不会失败。若上游实现违约，
//!   错误会经 `S::Error::custom` 变成序列化错误，而不是 panic。
//! - 这两个函数只决定数字文本；字段名与键序由各结构体的声明顺序和 serde 属性决定
//!   （本 crate 的 `tests/serde_contracts.rs` 用字符串比较钉住键序）。

use serde::ser::Error as _;
use serde::{Serialize, Serializer};

/// 序列化一个 `f64`。
pub(crate) fn serialize_f64<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    serialize(*value, serializer)
}

/// 序列化 `Option<f64>`：`None` 写出 `null`。
///
/// 可选字段通常同时带 `skip_serializing_if = "Option::is_none"`（省略键）；
/// 必填键（例如 [`crate::types::ContextUsage::percent`]）则用本函数把 `None` 写成 `null`。
pub(crate) fn serialize_option<S: Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        None => serializer.serialize_none(),
        Some(number) => serialize(*number, serializer),
    }
}

/// 把 `f64` 按 JS `JSON.stringify` 的数字文本原样写进 serde 输出。
///
/// 实现上交给 [`kiana_json::js_num::format_js_number`] 生成文本，再用 `RawValue` 原样写出。
/// 这里不做有限性/符号的特殊处理：`format_js_number` 已把 `NaN`/`±Infinity` 映射为
/// `"null"`、把 `-0` 映射为 `"0"`（`tests/js_number_contracts.rs` 覆盖这些特殊值）。
fn serialize<S: Serializer>(number: f64, serializer: S) -> Result<S::Ok, S::Error> {
    let raw =
        serde_json::value::RawValue::from_string(kiana_json::js_num::format_js_number(number))
            .map_err(S::Error::custom)?;
    raw.serialize(serializer)
}
