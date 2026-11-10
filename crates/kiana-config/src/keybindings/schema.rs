//! `core/keybindings-schema.ts`：`KeybindingsSchema` 的 JSON 值构建。
//!
//! TS 源：`packages/coding-agent/src/core/keybindings-schema.ts`（21 行）。
//! 规范：`spec/ca-config-models.md` §2.26、§4.8。
//!
//! ```ts
//! const properties: Record<string, TSchema> = { $schema: Type.Optional(Type.String()) };
//! for (const [id, definition] of Object.entries(KEYBINDINGS))
//!     properties[id] = Type.Optional(keybindingValueSchema({ description: definition.description }));
//! export const KeybindingsSchema = Type.Object(properties, { additionalProperties: KeybindingValueSchema });
//! ```
//!
//! 这里生成的是 **TypeBox 对象本身的 JSON 值**（尚未经过 `scripts/generate-schemas.ts` 的
//! `addDefinitions` 去重/`$ref` 替换、也还没有 `$schema`/`$id`/`$comment` 外壳）。
//! 键序与 `JSON.stringify(KeybindingsSchema)` 一致：
//! `type` → `properties` → `additionalProperties`；属性顺序为 `$schema` + 90 个键位 id。
//!
//! 后续阶段若序列化该 schema，必须走 `kiana_json::js_object::stringify_pretty(value, 2)`
//! （§4.8 的 `JSON.stringify(..., null, 2)`），不要用 `serde_json::to_string_pretty`。
//!
//! `KeybindingValueSchema` 按 §2.26 从 [`super::key_id`] 再导出。
//!
//! ## 在 crate 中的位置
//!
//! 键位集合、顺序与描述全部来自父模块的 [`keybindings`]（90 项：47 个 `tui.*` +
//! 43 个 `app.*`）；本文件不定义任何键位，只把这张表渲染成 TypeBox 形态的 JSON。
//! 当前仓库中的调用方只有本文件的单元测试与 `tests/keybindings_schema.rs`，后者用
//! `tests/fixtures/keybindings_schema.json`（TS 生成）做逐字节比对——所以属性顺序、
//! 描述文本与嵌套结构都是冻结契约，不能排序、去重或改写。
//!
//! ## 构建流程
//!
//! 1. `properties` 先插入 `$schema`（`{"type": "string"}`），它恒为第一个属性；
//! 2. 按 [`keybindings`] 的顺序为每个 id 插入
//!    `{anyOf: [KeyIdSchema, {type: "array", items: KeyIdSchema}], description}`；
//! 3. 组装顶层 `{"type": "object", "properties": …, "additionalProperties": …}`，
//!    其中 `additionalProperties` 是联合的**完整副本**而非 `$ref`。
//!
//! TypeBox 的 `Type.Optional` 标记本身不序列化到 JSON；生成物中也没有 `required` 数组，
//! 按 JSON Schema 语义这 90 个键位属性都是可选的。
//!
//! ## 注意事项
//!
//! - 输出与平台无关：当前平台分支只改 [`keybindings`] 中各项的 `default_keys`，本文件
//!   只读 id 与 `description`。
//! - 描述取自 `KeybindingDefinition::description`；为 `None` 时调用点用
//!   `unwrap_or_default()` 写成空串（当前 90 个定义都是 `Some`，该分支不会触发）。
//! - [`KEY_ID_SCHEMA`] / [`KEYBINDING_VALUE_SCHEMA`] 由本文件 `pub use` 转发，
//!   `keybindings::schema::*` 与 `keybindings::key_id::*` 指向同一份静态值。

use std::sync::LazyLock;

use serde_json::{Map, Value};

pub use super::key_id::{KEY_ID_SCHEMA, KEYBINDING_VALUE_SCHEMA};
use super::key_id::{keybinding_value_schema, keybinding_value_schema_with_description};
use super::keybindings;

/// `KeybindingsSchema` 的 JSON 值（TypeBox `Type.Object` 形态）。
///
/// `properties` = `$schema` + [`keybindings`] 的 90 个 id；每个键位是
/// `{anyOf: [KeyIdSchema, {type: "array", items: KeyIdSchema}], description}`
/// （`Type.Optional` 在 JSON 里没有标记）；`additionalProperties` 是同一个联合（完整副本）。
///
/// 每次调用都重建整棵结构（91 个属性，另加 `additionalProperties` 的一份联合）；反复
/// 使用请取 [`KEYBINDINGS_SCHEMA`]。返回值是 TypeBox 对象的原始形态：
/// `$schema`/`$id`/`$comment` 外壳和 `$defs` 去重属于生成脚本阶段（见模块文档）。
pub fn keybindings_schema() -> Value {
    let mut properties = Map::new();

    // `$schema` 是 TS properties 字面量的第一个键；`Map` 的插入序就是 JSON 输出序。
    let mut schema_property = Map::new();
    schema_property.insert("type".to_string(), Value::String("string".to_string()));
    properties.insert("$schema".to_string(), Value::Object(schema_property));

    for (id, definition) in keybindings() {
        // `Type.Optional` 不写进 JSON；描述缺失时退化为空串。
        let description = definition.description.as_deref().unwrap_or_default();
        properties.insert(
            id.clone(),
            keybinding_value_schema_with_description(description),
        );
    }

    // 顶层键序 type → properties → additionalProperties 对应 JSON.stringify；
    // 三个 insert 的顺序不能调整，测试按字节比较。
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    schema.insert(
        "additionalProperties".to_string(),
        keybinding_value_schema(),
    );
    Value::Object(schema)
}

/// `KeybindingsSchema` 的静态副本（与 TS 的模块级常量一致，只构建一次）。
///
/// 首次解引用时构造一次；随后内容固定（父模块 [`keybindings`] 本身也是 `LazyLock`）。
pub static KEYBINDINGS_SCHEMA: LazyLock<Value> = LazyLock::new(keybindings_schema);

/// [`KEYBINDINGS_SCHEMA`] 的借用。
///
/// 返回共享的 `&'static Value`，避免克隆；当前仓库内没有调用点，测试直接解引用
/// [`KEYBINDINGS_SCHEMA`]。
pub fn keybindings_schema_static() -> &'static Value {
    &KEYBINDINGS_SCHEMA
}

// 锁定属性个数、顺序与描述来源；与 tests/keybindings_schema.rs 的整树 fixture 比对互补。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_ninety_one_properties_in_keybindings_order() {
        let schema = keybindings_schema();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("properties");
        assert_eq!(properties.len(), 91);
        let keys: Vec<&str> = properties.keys().map(String::as_str).collect();
        assert_eq!(keys[0], "$schema");
        let expected: Vec<&str> = keybindings().keys().map(String::as_str).collect();
        assert_eq!(&keys[1..], expected.as_slice());
    }

    #[test]
    fn property_descriptions_come_from_keybindings() {
        let schema = keybindings_schema();
        let properties = schema.get("properties").and_then(Value::as_object).unwrap();
        for (id, definition) in keybindings() {
            let property = &properties[id];
            assert_eq!(
                property.get("description").and_then(Value::as_str),
                definition.description.as_deref()
            );
            let any_of = property.get("anyOf").and_then(Value::as_array).unwrap();
            assert_eq!(any_of.len(), 2);
            assert_eq!(
                any_of[0].get("pattern").and_then(Value::as_str),
                Some(super::super::key_id::KEY_ID_PATTERN)
            );
            assert_eq!(
                any_of[1]
                    .get("items")
                    .and_then(|items| items.get("pattern")),
                any_of[0].get("pattern")
            );
        }
    }
}
