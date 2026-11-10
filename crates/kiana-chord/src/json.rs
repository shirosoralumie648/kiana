//! `json.ts`：严格 JSON 的深拷贝与判定。
//!
//! TS 侧的 `copyJson`/`isJsonValue` 针对**任意** JS 值做校验（拒绝 `undefined`、函数、
//! symbol、非有限数字、环、稀疏数组、带访问器/不可枚举/符号键的属性、非普通原型）。
//! Rust 侧的 [`JsonValue`] 是 `serde_json::Value`：这些非法值在类型层就不存在
//! （`Value` 只能表示 JSON 的六种值，`Number` 只能是有限数），因此：
//!
//! - [`is_json_value`] 对任何 `Value` 恒为 `true`（保留函数是为了 API 与 TS 对齐）；
//! - [`copy_json`] 做深拷贝（`Value::clone`），天然无别名、保留 `__proto__` 这类普通键
//!   （`serde_json` 的对象键就是普通字符串键，不存在原型链）；
//! - `omitUndefinedProperties` 在 Rust 侧没有对应物（对象里不可能有 `undefined` 值），
//!   保留在 [`CopyJsonOptions`] 里只为逐项对齐 TS 的选项形状。
//!
//! 错误类型 [`JsonError`] 只在 [`validate_json_value`] 里可能产生，且只保留 TS 第一条
//! `TypeError` 文案（[`NON_FINITE_MESSAGE`]）；其余几条（函数、环、稀疏数组、访问器、符号键、
//! 非普通原型）在 `serde_json::Value` 上不可能出现，因此没有对应的 Rust 常量。保留 `Result`
//! 返回形状是让上游按与 TS 相同的错误路径书写。
//!
//! # 调用方
//!
//! 本 crate 之外，`kiana-durable` 的任务调度器在把迁移结果交给任务时（input 与 checkpoint
//! 各一次），以及 `owned_tasks` 从内存状态 `state.live` 取出子任务记录时调用 [`copy_json`]，
//! 确保调度器手里是独立副本（见 `crates/kiana-durable/src/harness/scheduler.rs`：共 3 次调用、
//! 两个位置）。
//!
//! # 键的顺序
//!
//! 需要逐字节保真的地方是**键的顺序**：工作区启用了 `serde_json` 的 `preserve_order`，
//! `Value::Object` 内部是 `IndexMap`，克隆按插入序复制（见 [`JsonValue`]）。

use serde_json::Value;

/// TS `JsonValue`：严格 JSON。工作区 `serde_json` 开启 `preserve_order`，
/// 对象迭代序 = 插入序（与 JS 的「整数键优先」差异由 `kiana_json::js_object::sort_js_keys`
/// 在序列化边界处理，与 TS 的 `Object.keys` 一致）。
///
/// 这是一个**类型别名**而不是 newtype，因此 `Value` 的任何值都能传进来；`undefined`、函数、
/// 符号、非有限数字在 Rust 的类型系统里根本无法表示。
pub type JsonValue = Value;

/// `copyJson` 的选项（TS: `CopyJsonOptions`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyJsonOptions {
    /// 丢弃值为 `undefined` 的对象属性。Rust 的 `Value` 没有 `undefined`，恒无影响。
    pub omit_undefined_properties: bool,
}

impl CopyJsonOptions {
    /// 空选项。
    pub fn new() -> Self {
        CopyJsonOptions::default()
    }

    /// 打开 `omitUndefinedProperties`。
    pub fn omit_undefined_properties(mut self) -> Self {
        self.omit_undefined_properties = true;
        self
    }
}

/// `copyJson` 的错误。当前唯一会用的文案是 [`NON_FINITE_MESSAGE`]（与 TS 的 `TypeError` 逐字
/// 一致）；其它 TS 文案没有对应常量（见模块文档）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// 完整的 `TypeError` 消息。
    pub message: String,
}

impl JsonError {
    fn new(message: &'static str) -> Self {
        JsonError {
            message: message.to_owned(),
        }
    }
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsonError {}

/// 非有限数字（`NaN`/`±Infinity`）——`serde_json::Number` 无法表示，保留给调用方构造诊断。
pub const NON_FINITE_MESSAGE: &str = "Value contains a non-finite number and is not strict JSON";

/// `copyJson(value)`：拷贝为无别名的严格 JSON 树。
///
/// 对应的 TS 错误（`JsonError`）在当前类型边界下不可达；返回 `Result` 以保持调用方形状。
pub fn copy_json(value: &JsonValue) -> Result<JsonValue, JsonError> {
    copy_json_with(value, CopyJsonOptions::default())
}

/// `copyJson(value, options)`。
///
/// `serde_json::Value::clone` 是深拷贝，因此结果与输入不共享任何节点，`__proto__` 作为
/// 普通键原样保留（见模块文档）。
///
/// `options` 当前被忽略（签名里的 `_options`）：Rust 侧既没有 `undefined` 属性，也没有其它
/// 可由选项改变的输入。保留参数只为与 TS 的调用形状一致。
pub fn copy_json_with(
    value: &JsonValue,
    _options: CopyJsonOptions,
) -> Result<JsonValue, JsonError> {
    Ok(value.clone())
}

/// `isJsonValue(value)`：任何 `serde_json::Value` 都是严格 JSON（恒 `true`，参数被忽略）。
///
/// 保留这个函数是为了让从 TS 移植的调用方不必改写判断分支；真正逐项检查的版本是
/// [`validate_json_value`]。
pub fn is_json_value(_value: &JsonValue) -> bool {
    true
}

/// 显式校验一个 `serde_json::Value` 是否为严格 JSON。
///
/// 唯一可能的违反是数字非有限；`serde_json::Number` 构造时已排除 NaN/±Infinity
/// （`Number::from_f64` 对它们返回 `None`），所以本函数恒返回 `Ok`。它存在是为了让
/// 上游在「外部输入已解析成 `Value`」的边界上有与 TS 相同的检查点。
///
/// 遍历方式：数组逐元素、对象逐值递归（对象键在 `serde_json` 里永远是 `String`，不需要检查）；
/// `null`/布尔/字符串直接通过。遇到第一个非有限数字就以 [`NON_FINITE_MESSAGE`] 返回
/// [`JsonError`]——由于 `Number` 的构造约束，这条路径实际不可达。没有环检测：`Value` 是树，
/// 不可能自引用。
pub fn validate_json_value(value: &JsonValue) -> Result<(), JsonError> {
    match value {
        Value::Number(number) => match number.as_f64() {
            Some(value) if value.is_finite() => Ok(()),
            _ => Err(JsonError::new(NON_FINITE_MESSAGE)),
        },
        Value::Array(items) => {
            for item in items {
                validate_json_value(item)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (_key, item) in map {
                validate_json_value(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn copy_is_deep_and_alias_free() {
        let input = json!({"left": {"value": 1}, "right": {"value": 1}, "list": [1, 2]});
        let copied = copy_json(&input).expect("copy");
        assert_eq!(copied, input);
        // 深拷贝：修改副本不影响原值（Rust 所有权保证，但显式断言语义）。
        let mut copied = copied;
        copied["left"]["value"] = json!(2);
        assert_eq!(input["left"]["value"], json!(1));
    }

    #[test]
    fn preserves_null_prototype_style_keys_like_proto() {
        // TS 用 `Object.create(null)` + 自有 `__proto__` 数据属性；Rust 侧就是普通键。
        let input = json!({"__proto__": {"safe": true}, "constructor": 1, "prototype": 2});
        let copied = copy_json(&input).expect("copy");
        assert_eq!(copied, input);
        assert_eq!(copied["__proto__"]["safe"], json!(true));
    }

    #[test]
    fn options_are_inert_for_values() {
        let input = json!({"kept": 1, "nested": {"kept": true}});
        let copied = copy_json_with(&input, CopyJsonOptions::new().omit_undefined_properties())
            .expect("copy");
        assert_eq!(copied, input);
    }

    #[test]
    fn all_values_are_strict_json() {
        for value in [
            json!(null),
            json!(true),
            json!(1.5),
            json!("text"),
            json!([1, [2]]),
            json!({"a": {"b": []}}),
        ] {
            assert!(is_json_value(&value));
            assert!(validate_json_value(&value).is_ok());
        }
    }
}
