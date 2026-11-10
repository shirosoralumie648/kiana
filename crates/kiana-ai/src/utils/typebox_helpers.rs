//! `utils/typebox-helpers.ts`——`StringEnum` schema 助手。
//!
//! TS: `packages/ai/src/utils/typebox-helpers.ts`。
//!
//! TS 侧 `StringEnum(values, options?)` 返回 `Type.Unsafe<string>({ type: "string", enum: values,
//! description?, default? })`：一个**普通 JSON schema**（`type: "string"` + `enum`），刻意不用
//! `anyOf`/`const`，因为 Google 的 API 不支持后者。
//!
//! Rust 侧不需要 TypeBox 类型，只需要同一个 JSON 形状，因此本函数直接构造
//! [`serde_json::Value`]。`description` / `default` 为 `undefined`（`None`）时键被省略——连
//! 空字符串 `description` 也会省略，与 TS 的 `...(options?.description && {...})` 一致。
//!
//! # 键序与使用边界
//!
//! 键按 `type`、`enum`、`description`、`default` 的顺序插入；`JsonObject` 本身就是
//! `IndexMap`，且 workspace 打开了 serde_json 的 `preserve_order`，所以转成
//! `Value::Object` 后仍保留这个顺序。序列化文本因此与 TS 对象字面量的 `JSON.stringify`
//! 顺序一致，可用于逐字比较。
//!
//! 本函数只负责形状对齐，不做校验：空 `values` 数组、重复的枚举值、空串枚举值都会被原样
//! 写入。它也不添加 TypeBox 的符号元数据（`Symbol.for("TypeBox.Kind")`），产出的就是可
//! 直接发给 provider 的普通 JSON schema。

use crate::types::JsonObject;
use serde_json::Value;

/// `StringEnum` 的可选参数（TS 的 `{ description?, default? }`）。
///
/// `None` 与 `Some("")` 在输出上等价（都被省略），调用方无需先把空串归一化成 `None`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StringEnumOptions {
    /// `description`（空字符串会被省略，与 TS 的真值判断一致）。
    pub description: Option<String>,
    /// `default`（空字符串会被省略）。
    pub default: Option<String>,
}

/// `StringEnum(values, options?)`：`{type: "string", enum: [...], description?, default?}`。
///
/// `values` 按传入顺序写入；`options` 为 `None` 表示使用默认值。`description` /
/// `default` 只在非空时出现（空字符串省略，见 [`StringEnumOptions`]）。
///
/// 返回的是普通 [`serde_json::Value`]，调用方不能假设它携带 TypeBox 的类型信息（没有 kind
/// 符号，也没有 `anyOf`/`const`）；这是 JSON Schema 的协议约束，不是实现缺失。
pub fn string_enum(values: &[&str], options: Option<&StringEnumOptions>) -> Value {
    let mut schema = JsonObject::new();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    schema.insert(
        "enum".to_string(),
        Value::Array(
            values
                .iter()
                .map(|value| Value::String((*value).to_string()))
                .collect(),
        ),
    );
    if let Some(options) = options {
        if let Some(description) = options
            .description
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            schema.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        if let Some(default) = options.default.as_deref().filter(|text| !text.is_empty()) {
            schema.insert("default".to_string(), Value::String(default.to_string()));
        }
    }
    Value::Object(schema.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_a_plain_string_enum_schema() {
        assert_eq!(
            string_enum(&["add", "subtract"], None),
            json!({"type": "string", "enum": ["add", "subtract"]})
        );
    }

    #[test]
    fn adds_description_and_default_only_when_truthy() {
        let options = StringEnumOptions {
            description: Some("The operation to perform".into()),
            default: Some("add".into()),
        };
        assert_eq!(
            string_enum(&["add", "subtract"], Some(&options)),
            json!({
                "type": "string",
                "enum": ["add", "subtract"],
                "description": "The operation to perform",
                "default": "add",
            })
        );
        // 空字符串按 TS 的真值判断被省略。
        let options = StringEnumOptions {
            description: Some(String::new()),
            default: Some(String::new()),
        };
        assert_eq!(
            string_enum(&["add"], Some(&options)),
            json!({"type": "string", "enum": ["add"]})
        );
    }
}
